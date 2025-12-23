use fnv::FnvHashMap;
use libp2p_identity::PeerId;
use rand::seq::SliceRandom;
use std::cmp::Ordering;

/// Peer performance metrics
#[derive(Debug, Clone)]
pub struct PeerMetrics {
    /// Number of blocks received
    pub blocks_received: u64,
    /// Number of failures
    pub failures: u64,
    /// Total bytes received
    pub total_bytes: u64,
    /// Total transfer time (ms)
    pub total_time_ms: u64,
    /// Recent transfer speeds (bytes/sec)
    pub recent_speeds: std::collections::VecDeque<f64>,
    /// Last update time
    pub last_updated: web_time::Instant,
    /// Consecutive successes
    pub consecutive_successes: u32,
    /// Consecutive failures
    pub consecutive_failures: u32,
}

impl Default for PeerMetrics {
    fn default() -> Self {
        Self {
            blocks_received: 0,
            failures: 0,
            total_bytes: 0,
            total_time_ms: 0,
            recent_speeds: std::collections::VecDeque::with_capacity(10),
            last_updated: web_time::Instant::now(),
            consecutive_successes: 0,
            consecutive_failures: 0,
        }
    }
}

impl PeerMetrics {
    /// Calculate success rate (0.0 - 1.0)
    pub fn success_rate(&self) -> f64 {
        let total = self.blocks_received + self.failures;
        if total == 0 {
            return 0.5; // New peer gets neutral score
        }
        self.blocks_received as f64 / total as f64
    }

    /// Calculate average speed (bytes/sec)
    pub fn avg_speed(&self) -> f64 {
        if self.recent_speeds.is_empty() {
            return 0.0;
        }
        self.recent_speeds.iter().sum::<f64>() / self.recent_speeds.len() as f64
    }

    /// Calculate composite score (0.0 - 100.0)
    pub fn calculate_score(&self) -> f64 {
        const SPEED_WEIGHT: f64 = 0.5;
        const SUCCESS_RATE_WEIGHT: f64 = 0.35;
        const STABILITY_WEIGHT: f64 = 0.15;

        // New peer gets exploration bonus
        if self.blocks_received == 0 && self.failures == 0 {
            return 55.0; // Slightly above average to encourage trying new peers
        }

        // Use log-based speed normalization (works for any speed range)
        // 1 KB/s -> ~30, 100 KB/s -> ~50, 1 MB/s -> ~67, 10 MB/s -> ~83, 100 MB/s -> 100
        let speed_score = if self.avg_speed() > 0.0 {
            ((self.avg_speed().log10() - 3.0) / 5.0 * 100.0).clamp(0.0, 100.0)
        } else {
            0.0
        };

        // Success rate score (0-100)
        let success_score = self.success_rate() * 100.0;

        // Stability score (based on consecutive successes)
        let stability_score = if self.consecutive_failures > 3 {
            0.0
        } else {
            (self.consecutive_successes.min(10) as f64 / 10.0) * 100.0
        };

        // Progressive time decay (smoother than binary 1.0/0.8)
        let elapsed = self.last_updated.elapsed().as_secs();
        let decay_factor = 1.0 - (elapsed as f64 / 600.0).min(0.3); // 0-600s: 1.0->0.7

        let score = (speed_score * SPEED_WEIGHT
            + success_score * SUCCESS_RATE_WEIGHT
            + stability_score * STABILITY_WEIGHT)
            * decay_factor;

        score.clamp(0.0, 100.0)
    }

    /// Record successful transfer
    pub fn record_success(&mut self, bytes: usize, duration_ms: u64) {
        self.blocks_received += 1;
        self.total_bytes += bytes as u64;
        self.total_time_ms += duration_ms;
        self.consecutive_successes += 1;
        self.consecutive_failures = 0;
        self.last_updated = web_time::Instant::now();

        if duration_ms > 0 {
            let speed = (bytes as f64 / duration_ms as f64) * 1000.0;
            if self.recent_speeds.len() >= 10 {
                self.recent_speeds.pop_front();
            }
            self.recent_speeds.push_back(speed);
        }
    }

    /// Record failure
    pub fn record_failure(&mut self) {
        self.failures += 1;
        self.consecutive_failures += 1;
        self.consecutive_successes = 0;
        self.last_updated = web_time::Instant::now();
    }
}

/// Detailed peer information for external reporting
#[derive(Debug, Clone)]
pub struct PeerDetail {
    pub peer_id: String,
    pub score: f64,
    pub blocks_received: u64,
    pub failures: u64,
    pub success_rate: f64,
    pub avg_speed: f64,
}

/// Peer selection configuration
#[derive(Debug, Clone)]
pub struct PeerSelectionConfig {
    /// Number of top peers to select
    pub top_n: usize,
    /// Enable smart selection (false = broadcast mode)
    pub enabled: bool,
    /// Minimum peers threshold
    pub min_peers: usize,
    /// Enable randomness in selection
    pub enable_randomness: bool,
    /// Wait window in milliseconds after first Have response before selecting peers
    /// This allows more peers to respond with Have, ensuring better selection
    pub have_wait_window_ms: u64,
    /// Minimum candidate ratio before starting selection (0.0 - 1.0)
    /// Selection starts when: candidates >= total_peers * min_candidate_ratio
    pub min_candidate_ratio: f64,
}

impl Default for PeerSelectionConfig {
    fn default() -> Self {
        Self {
            top_n: 3,
            enabled: true,
            min_peers: 2,
            enable_randomness: true,
            have_wait_window_ms: 100,  // Wait 100ms for more Have responses
            min_candidate_ratio: 0.3,   // Or start when 30% of peers responded
        }
    }
}

#[derive(Debug, Clone)]
struct ScoredPeer {
    peer_id: PeerId,
    score: f64,
}

impl PartialEq for ScoredPeer {
    fn eq(&self, other: &Self) -> bool {
        self.score == other.score
    }
}

impl Eq for ScoredPeer {}

impl PartialOrd for ScoredPeer {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        self.score.partial_cmp(&other.score)
    }
}

impl Ord for ScoredPeer {
    fn cmp(&self, other: &Self) -> Ordering {
        self.partial_cmp(other).unwrap_or(Ordering::Equal)
    }
}

/// Peer selector
pub struct PeerSelector;

impl PeerSelector {
    /// Select top N peers from candidates
    pub fn select_top_peers(
        candidates: &FnvHashMap<PeerId, &PeerMetrics>,
        config: &PeerSelectionConfig,
    ) -> Vec<PeerId> {
        if !config.enabled || candidates.len() <= config.min_peers {
            return candidates.keys().copied().collect();
        }

        let mut scored_peers: Vec<ScoredPeer> = candidates
            .iter()
            .map(|(peer_id, metrics)| ScoredPeer {
                peer_id: *peer_id,
                score: metrics.calculate_score(),
            })
            .collect();

        scored_peers.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(Ordering::Equal));

        let selection_count = config.top_n.min(scored_peers.len());

        if config.enable_randomness && scored_peers.len() > config.top_n {
            let extended_count = ((config.top_n as f64 * 1.5) as usize).min(scored_peers.len());
            let mut extended_pool: Vec<PeerId> = scored_peers
                .iter()
                .take(extended_count)
                .map(|sp| sp.peer_id)
                .collect();

            let mut rng = rand::thread_rng();
            extended_pool.shuffle(&mut rng);
            extended_pool.into_iter().take(selection_count).collect()
        } else {
            scored_peers
                .into_iter()
                .take(selection_count)
                .map(|sp| sp.peer_id)
                .collect()
        }
    }

    /// Print peer rankings for debugging
    #[allow(dead_code)]
    pub fn print_peer_rankings(candidates: &FnvHashMap<PeerId, &PeerMetrics>) {
        let mut scored_peers: Vec<(PeerId, f64, &PeerMetrics)> = candidates
            .iter()
            .map(|(peer_id, metrics)| (*peer_id, metrics.calculate_score(), *metrics))
            .collect();

        scored_peers.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(Ordering::Equal));

        tracing::debug!("Peer rankings:");
        for (i, (peer_id, score, metrics)) in scored_peers.iter().enumerate() {
            tracing::debug!(
                "#{} {} | score: {:.2} | rate: {:.1}% | speed: {:.2} MB/s",
                i + 1,
                peer_id,
                score,
                metrics.success_rate() * 100.0,
                metrics.avg_speed() / (1024.0 * 1024.0)
            );
        }
    }
}

/// Global statistics
#[derive(Debug, Default, Clone)]
pub struct GlobalStats {
    pub total_requests: u64,
    pub successful_requests: u64,
    pub failed_requests: u64,
    pub total_bytes_received: u64,
}
