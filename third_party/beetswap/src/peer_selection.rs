use cid::CidGeneric;
use fnv::FnvHashMap;
use libp2p_identity::PeerId;
use rand::seq::SliceRandom;
use std::cmp::Ordering;

/// 节点性能统计指标
#[derive(Debug, Clone)]
pub struct PeerMetrics {
    /// 成功接收的块数
    pub blocks_received: u64,
    /// 失败次数
    pub failures: u64,
    /// 总接收字节数
    pub total_bytes: u64,
    /// 总传输时间（毫秒）
    pub total_time_ms: u64,
    /// 最近 N 次传输速度记录（字节/秒）
    pub recent_speeds: std::collections::VecDeque<f64>,
    /// 最近更新时间
    pub last_updated: web_time::Instant,
    /// 平均往返时间（RTT，毫秒）
    pub avg_rtt_ms: f64,
    /// 连续成功次数
    pub consecutive_successes: u32,
    /// 连续失败次数
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
    /// 计算成功率 (0.0 - 1.0)
    pub fn success_rate(&self) -> f64 {
        let total = self.blocks_received + self.failures;
        if total == 0 {
            return 0.5; // 新节点给予中等评分
        }
        self.blocks_received as f64 / total as f64
    }

    /// 计算平均传输速度（字节/秒）
    pub fn avg_speed(&self) -> f64 {
        if self.recent_speeds.is_empty() {
            return 0.0;
        }
        self.recent_speeds.iter().sum::<f64>() / self.recent_speeds.len() as f64
    }

    /// 计算综合评分（0.0 - 100.0）
    pub fn calculate_score(&self) -> f64 {
        // 权重配置
        const SPEED_WEIGHT: f64 = 0.4;        // 速度权重 40%
        const SUCCESS_RATE_WEIGHT: f64 = 0.3; // 成功率权重 30%
        const RTT_WEIGHT: f64 = 0.2;          // RTT权重 20%
        const STABILITY_WEIGHT: f64 = 0.1;    // 稳定性权重 10%

        // 1. 速度评分 (归一化到 0-100)
        let speed = self.avg_speed();
        let speed_mb_s = speed / (1024.0 * 1024.0);
        let speed_score = (speed_mb_s.min(100.0) / 100.0) * 100.0;

        // 2. 成功率评分 (0-100)
        let success_score = self.success_rate() * 100.0;

        // 3. RTT评分 (越低越好，归一化到 0-100)
        let rtt_score = if self.avg_rtt_ms > 0.0 {
            ((1000.0 - self.avg_rtt_ms.min(1000.0)) / 1000.0) * 100.0
        } else {
            50.0 // 未知RTT给予中等评分
        };

        // 4. 稳定性评分（基于连续成功次数）
        let stability_score = if self.consecutive_failures > 3 {
            0.0 // 连续失败过多，评分为0
        } else {
            (self.consecutive_successes.min(10) as f64 / 10.0) * 100.0
        };

        // 5. 时间衰减因子（长时间未使用的节点降低评分）
        let elapsed = self.last_updated.elapsed().as_secs();
        let decay_factor = if elapsed > 300 {
            0.8 // 超过5分钟未使用，评分衰减
        } else {
            1.0
        };

        // 综合评分
        let score = (speed_score * SPEED_WEIGHT
            + success_score * SUCCESS_RATE_WEIGHT
            + rtt_score * RTT_WEIGHT
            + stability_score * STABILITY_WEIGHT)
            * decay_factor;

        score.max(0.0).min(100.0)
    }

    /// 记录成功传输
    pub fn record_success(&mut self, bytes: usize, duration_ms: u64) {
        self.blocks_received += 1;
        self.total_bytes += bytes as u64;
        self.total_time_ms += duration_ms;
        self.consecutive_successes += 1;
        self.consecutive_failures = 0;
        self.last_updated = web_time::Instant::now();

        // 计算本次速度
        if duration_ms > 0 {
            let speed = (bytes as f64 / duration_ms as f64) * 1000.0;
            if self.recent_speeds.len() >= 10 {
                self.recent_speeds.pop_front();
            }
            self.recent_speeds.push_back(speed);
        }

        // 更新平均 RTT（使用指数移动平均）
        if self.avg_rtt_ms == 0.0 {
            self.avg_rtt_ms = duration_ms as f64;
        } else {
            self.avg_rtt_ms = self.avg_rtt_ms * 0.8 + duration_ms as f64 * 0.2;
        }
    }

    /// 记录失败
    pub fn record_failure(&mut self) {
        self.failures += 1;
        self.consecutive_failures += 1;
        self.consecutive_successes = 0;
        self.last_updated = web_time::Instant::now();
    }
}

/// 节点选择配置
#[derive(Debug, Clone)]
pub struct PeerSelectionConfig {
    /// 选择的最优节点数量
    pub top_n: usize,
    /// 是否启用智能选择（false 则使用广播模式）
    pub enabled: bool,
    /// 最小节点数量（如果可用节点少于此数，使用所有节点）
    pub min_peers: usize,
    /// 是否启用随机性（从 top_n * 1.5 中随机选择 top_n 个）
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

/// 节点评分项（用于排序）
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

/// 节点选择器
pub struct PeerSelector;

impl PeerSelector {
    /// 从候选节点中选择最优的 top N 个节点
    pub fn select_top_peers(
        candidates: &FnvHashMap<PeerId, &PeerMetrics>,
        config: &PeerSelectionConfig,
    ) -> Vec<PeerId> {
        if !config.enabled || candidates.len() <= config.min_peers {
            // 如果未启用智能选择，或节点数太少，返回所有节点
            return candidates.keys().copied().collect();
        }

        // 1. 计算每个节点的评分
        let mut scored_peers: Vec<ScoredPeer> = candidates
            .iter()
            .map(|(peer_id, metrics)| ScoredPeer {
                peer_id: *peer_id,
                score: metrics.calculate_score(),
            })
            .collect();

        // 2. 按评分排序（降序）
        scored_peers.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(Ordering::Equal));

        // 3. 选择 top N
        let selection_count = config.top_n.min(scored_peers.len());

        if config.enable_randomness && scored_peers.len() > config.top_n {
            // 启用随机性：从 top_n * 1.5 个节点中随机选择 top_n 个
            let extended_count = ((config.top_n as f64 * 1.5) as usize).min(scored_peers.len());
            let mut extended_pool: Vec<PeerId> = scored_peers
                .iter()
                .take(extended_count)
                .map(|sp| sp.peer_id)
                .collect();

            // 使用随机数生成器
            let mut rng = rand::thread_rng();
            extended_pool.shuffle(&mut rng);

            extended_pool.into_iter().take(selection_count).collect()
        } else {
            // 不启用随机性：直接选择 top N
            scored_peers
                .into_iter()
                .take(selection_count)
                .map(|sp| sp.peer_id)
                .collect()
        }
    }

    /// 打印节点评分排名（用于调试）
    pub fn print_peer_rankings(candidates: &FnvHashMap<PeerId, &PeerMetrics>) {
        let mut scored_peers: Vec<(PeerId, f64, &PeerMetrics)> = candidates
            .iter()
            .map(|(peer_id, metrics)| (*peer_id, metrics.calculate_score(), *metrics))
            .collect();

        scored_peers.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(Ordering::Equal));

        tracing::info!("=== 节点评分排名 ===");
        for (i, (peer_id, score, metrics)) in scored_peers.iter().enumerate() {
            tracing::info!(
                "#{} 节点: {} | 评分: {:.2} | 成功率: {:.1}% | 速度: {:.2} MB/s | RTT: {:.0}ms",
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

/// 请求跟踪信息
#[derive(Debug)]
pub struct RequestTracker<const S: usize> {
    /// 请求的 CID
    pub cid: CidGeneric<S>,
    /// 请求开始时间
    pub start_time: web_time::Instant,
    /// WantBlock 发送时间
    pub want_block_sent_time: Option<web_time::Instant>,
    /// 已发送 WantBlock 的节点列表
    pub peers_sent: fnv::FnvHashSet<PeerId>,
    /// 请求类型（探测/获取数据）
    pub request_phase: RequestPhase,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum RequestPhase {
    /// 第一阶段：发送 WantHave 探测
    Probing,
    /// 第二阶段：向选定节点发送 WantBlock
    Fetching,
}

/// 全局统计
#[derive(Debug, Default, Clone)]
pub struct GlobalStats {
    pub total_requests: u64,
    pub successful_requests: u64,
    pub failed_requests: u64,
    pub total_bytes_received: u64,
}
