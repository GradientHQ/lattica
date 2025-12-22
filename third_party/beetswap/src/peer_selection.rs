use cid::CidGeneric;
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
    /// Average RTT (ms)
    pub avg_rtt_ms: f64,
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
            avg_rtt_ms: 0.0,
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
        const SPEED_WEIGHT: f64 = 0.4;
        const SUCCESS_RATE_WEIGHT: f64 = 0.3;
        const RTT_WEIGHT: f64 = 0.2;
        const STABILITY_WEIGHT: f64 = 0.1;

        // Speed score (normalized to 0-100)
        let speed_mb_s = self.avg_speed() / (1024.0 * 1024.0);
        let speed_score = (speed_mb_s.min(100.0) / 100.0) * 100.0;

        // Success rate score (0-100)
        let success_score = self.success_rate() * 100.0;

        // RTT score (lower is better, normalized to 0-100)
        let rtt_score = if self.avg_rtt_ms > 0.0 {
            ((1000.0 - self.avg_rtt_ms.min(1000.0)) / 1000.0) * 100.0
        } else {
            50.0
        };

        // Stability score (based on consecutive successes)
        let stability_score = if self.consecutive_failures > 3 {
            0.0
        } else {
            (self.consecutive_successes.min(10) as f64 / 10.0) * 100.0
        };

        // Time decay factor
        let elapsed = self.last_updated.elapsed().as_secs();
        let decay_factor = if elapsed > 300 { 0.8 } else { 1.0 };

        let score = (speed_score * SPEED_WEIGHT
            + success_score * SUCCESS_RATE_WEIGHT
            + rtt_score * RTT_WEIGHT
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

        // Update average RTT using exponential moving average
        if self.avg_rtt_ms == 0.0 {
            self.avg_rtt_ms = duration_ms as f64;
        } else {
            self.avg_rtt_ms = self.avg_rtt_ms * 0.8 + duration_ms as f64 * 0.2;
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
    pub avg_rtt_ms: f64,
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
}

impl Default for PeerSelectionConfig {
    fn default() -> Self {
        Self {
            top_n: 3,
            enabled: true,
            min_peers: 2,
            enable_randomness: true,
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
                "#{} {} | score: {:.2} | rate: {:.1}% | speed: {:.2} MB/s | rtt: {:.0}ms",
                i + 1,
                peer_id,
                score,
                metrics.success_rate() * 100.0,
                metrics.avg_speed() / (1024.0 * 1024.0),
                metrics.avg_rtt_ms
            );
        }
    }
}

/// Request tracker
#[derive(Debug)]
pub struct RequestTracker<const S: usize> {
    /// Request CID
    pub cid: CidGeneric<S>,
    /// Request start time
    pub start_time: web_time::Instant,
    /// WantBlock sent time
    pub want_block_sent_time: Option<web_time::Instant>,
    /// Peers that received WantBlock
    pub peers_sent: fnv::FnvHashSet<PeerId>,
    /// Request phase
    pub request_phase: RequestPhase,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum RequestPhase {
    /// Phase 1: WantHave probing
    Probing,
    /// Phase 2: WantBlock fetching
    Fetching,
}

/// Global statistics
#[derive(Debug, Default, Clone)]
pub struct GlobalStats {
    pub total_requests: u64,
    pub successful_requests: u64,
    pub failed_requests: u64,
    pub total_bytes_received: u64,
}
