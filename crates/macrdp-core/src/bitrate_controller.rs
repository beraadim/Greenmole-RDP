use std::net::IpAddr;
use std::time::{Duration, Instant};

/// Determine if an IP address belongs to a private/local network.
pub fn is_private_ip(addr: IpAddr) -> bool {
    match addr {
        IpAddr::V4(ip) => ip.is_private() || ip.is_loopback() || ip.is_link_local(),
        IpAddr::V6(ip) => ip.is_loopback() || (ip.segments()[0] & 0xffc0) == 0xfe80,
    }
}

/// Continuous network quality based on RTT measurement.
#[derive(Debug, Clone, Copy)]
pub struct NetworkQuality {
    pub rtt_ms: f64,
    pub is_private_ip: bool,
}

impl NetworkQuality {
    /// Score in [0.1, 1.0]: higher means better (lower latency) network.
    pub fn score(&self) -> f64 {
        match self.rtt_ms {
            r if r < 2.0 => 1.0,
            r if r < 5.0 => 0.9,
            r if r < 10.0 => 0.8,
            r if r < 20.0 => 0.6,
            r if r < 50.0 => 0.4,
            r if r < 100.0 => 0.2,
            _ => 0.1,
        }
    }

    /// Bootstrap from IP type before any RTT samples are available.
    pub fn from_ip(is_private: bool) -> Self {
        Self {
            rtt_ms: if is_private { 5.0 } else { 50.0 },
            is_private_ip: is_private,
        }
    }

    /// Update RTT with a new EWMA sample.
    pub fn update_rtt(&mut self, rtt_ewma_ms: f64) {
        self.rtt_ms = rtt_ewma_ms;
    }
}

/// Per-frame statistics for BitrateController evaluation.
#[derive(Debug, Clone, Copy)]
pub struct FrameStats {
    pub encode_ms: f64,
    pub frame_bytes: u32,
    pub is_keyframe: bool,
}

/// Decision output from BitrateController.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AdaptiveDecision {
    pub bitrate_bps: u32,
    pub fps: u32,
}

const FPS_TIERS: [f32; 3] = [1.0, 0.5, 0.25];

pub struct BitrateController {
    initial_bitrate: u32,
    current_bitrate: u32,
    target_fps: u32,
    current_fps_tier: usize,
    eval_window: Vec<FrameStats>,
    last_eval_time: Instant,
    eval_interval: Duration,
    network: NetworkQuality,
}

impl BitrateController {
    pub fn new(initial_bitrate: u32, target_fps: u32, network: NetworkQuality) -> Self {
        Self {
            initial_bitrate,
            current_bitrate: initial_bitrate,
            target_fps,
            current_fps_tier: 0,
            eval_window: Vec::with_capacity(128),
            last_eval_time: Instant::now(),
            // React quickly to overload so latency drops before the stream feels sticky.
            eval_interval: Duration::from_millis(500),
            network,
        }
    }

    pub fn record_frame(&mut self, stats: FrameStats) {
        self.eval_window.push(stats);
    }
    pub fn current_bitrate(&self) -> u32 {
        self.current_bitrate
    }
    pub fn current_fps(&self) -> u32 {
        (self.target_fps as f32 * FPS_TIERS[self.current_fps_tier]) as u32
    }
    pub fn target_fps(&self) -> u32 {
        self.target_fps
    }
    pub fn network_score(&self) -> f64 {
        self.network.score()
    }

    pub fn update_network_rtt(&mut self, rtt_ewma_ms: f64) {
        self.network.update_rtt(rtt_ewma_ms);
    }

    pub fn on_idle_recovery(&mut self) {
        self.eval_window.clear();
        self.current_bitrate = self.initial_bitrate;
        self.current_fps_tier = 0;
        self.last_eval_time = Instant::now();
    }

    pub fn evaluate(&mut self) -> AdaptiveDecision {
        let current_fps = self.current_fps();
        if self.eval_window.is_empty() {
            return AdaptiveDecision {
                bitrate_bps: self.current_bitrate,
                fps: current_fps,
            };
        }

        let score = self.network.score();
        let frame_interval_ms = 1000.0 / current_fps as f64;
        let total = self.eval_window.len() as f64;
        let avg_encode_ms = self.eval_window.iter().map(|s| s.encode_ms).sum::<f64>() / total;
        let non_kf: Vec<_> = self.eval_window.iter().filter(|s| !s.is_keyframe).collect();
        let avg_frame_bytes = if non_kf.is_empty() {
            0.0
        } else {
            non_kf.iter().map(|s| s.frame_bytes as f64).sum::<f64>() / non_kf.len() as f64
        };

        let floor = (self.initial_bitrate as f64 * 0.3) as u32;
        let ceiling = (self.initial_bitrate as f64 * (1.0 + score * 0.5)) as u32;
        let target_frame_bytes = self.current_bitrate as f64 / current_fps as f64 / 8.0;

        // Score-based decrease factor: worse network → more aggressive reduction
        let decrease_factor = if score <= 0.2 {
            0.80
        } else if score < 0.5 {
            0.85
        } else {
            0.90
        };

        let mut new_bitrate = self.current_bitrate;
        let mut new_fps_tier = self.current_fps_tier;

        // Degradation: encode overload takes priority; frame size check only when encode is healthy.
        // Even on a great LAN, slow encode must still trigger adaptation or latency balloons.
        let encode_overloaded = avg_encode_ms > frame_interval_ms * 0.6;
        let severe_encode_overload = avg_encode_ms > frame_interval_ms * 0.9;
        if encode_overloaded {
            new_bitrate = ((new_bitrate as f64) * decrease_factor) as u32;
        } else if avg_frame_bytes > 0.0
            && target_frame_bytes > 0.0
            && avg_frame_bytes > target_frame_bytes * 1.5
        {
            new_bitrate = ((new_bitrate as f64) * 0.90) as u32;
        }
        new_bitrate = new_bitrate.max(floor).min(ceiling);

        // FPS step down: do it aggressively on severe overload, or once bitrate bottoms out.
        if severe_encode_overload
            || (new_bitrate <= floor && avg_encode_ms > frame_interval_ms * 0.8)
        {
            if new_fps_tier < FPS_TIERS.len() - 1 {
                new_fps_tier += 1;
            }
        }

        // Recovery (PRIORITY: fps first, then bitrate), but only when encode headroom is clearly back.
        if avg_encode_ms < frame_interval_ms * 0.25 {
            if new_fps_tier > 0 {
                new_fps_tier -= 1; // fps first
            } else if new_bitrate < ceiling {
                new_bitrate = (((new_bitrate as f64) * 1.10) as u32).min(ceiling);
            }
        }

        self.current_bitrate = new_bitrate;
        self.current_fps_tier = new_fps_tier;
        self.eval_window.clear();
        self.last_eval_time = Instant::now();
        AdaptiveDecision {
            bitrate_bps: new_bitrate,
            fps: self.current_fps(),
        }
    }

    pub fn should_evaluate(&self) -> bool {
        self.last_eval_time.elapsed() >= self.eval_interval && !self.eval_window.is_empty()
    }

    #[cfg(test)]
    fn set_bitrate_to_floor_for_test(&mut self) {
        self.current_bitrate = (self.initial_bitrate as f64 * 0.3) as u32;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

    #[test]
    fn test_private_ipv4_ranges() {
        assert!(is_private_ip(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1))));
        assert!(is_private_ip(IpAddr::V4(Ipv4Addr::new(10, 255, 255, 255))));
        assert!(is_private_ip(IpAddr::V4(Ipv4Addr::new(172, 16, 0, 1))));
        assert!(is_private_ip(IpAddr::V4(Ipv4Addr::new(172, 31, 255, 255))));
        assert!(is_private_ip(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 1))));
        assert!(is_private_ip(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1))));
        assert!(is_private_ip(IpAddr::V4(Ipv4Addr::new(169, 254, 1, 1))));
    }

    #[test]
    fn test_public_ipv4() {
        assert!(!is_private_ip(IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8))));
        assert!(!is_private_ip(IpAddr::V4(Ipv4Addr::new(1, 1, 1, 1))));
        assert!(!is_private_ip(IpAddr::V4(Ipv4Addr::new(203, 0, 113, 1))));
    }

    #[test]
    fn test_ipv6() {
        assert!(is_private_ip(IpAddr::V6(Ipv6Addr::LOCALHOST)));
        assert!(is_private_ip(IpAddr::V6(Ipv6Addr::new(
            0xfe80, 0, 0, 0, 0, 0, 0, 1
        ))));
        assert!(!is_private_ip(IpAddr::V6(Ipv6Addr::new(
            0x2001, 0xdb8, 0, 0, 0, 0, 0, 1
        ))));
    }

    fn make_controller(initial_bitrate: u32, is_lan: bool, target_fps: u32) -> BitrateController {
        let nq = if is_lan {
            NetworkQuality::from_ip(true)
        } else {
            NetworkQuality {
                rtt_ms: 30.0,
                is_private_ip: false,
            }
        };
        BitrateController::new(initial_bitrate, target_fps, nq)
    }

    #[test]
    fn test_lan_encode_overload_still_adapts() {
        let mut ctrl = make_controller(10_000_000, true, 60);
        for _ in 0..40 {
            ctrl.record_frame(FrameStats {
                encode_ms: 20.0,
                frame_bytes: 500_000,
                is_keyframe: false,
            });
        }
        let decision = ctrl.evaluate();
        assert!(decision.bitrate_bps < 10_000_000);
        assert_eq!(decision.fps, 30);
    }

    #[test]
    fn test_bitrate_decrease_on_encode_overload() {
        let mut ctrl = make_controller(10_000_000, false, 60);
        for _ in 0..40 {
            ctrl.record_frame(FrameStats {
                encode_ms: 12.0,
                frame_bytes: 50_000,
                is_keyframe: false,
            });
        }
        let decision = ctrl.evaluate();
        // rtt=30ms → score=0.4 → decrease_factor=0.85
        assert_eq!(decision.bitrate_bps, (10_000_000.0 * 0.85) as u32);
    }

    #[test]
    fn test_bitrate_floor_enforced() {
        let mut ctrl = make_controller(10_000_000, false, 60);
        ctrl.set_bitrate_to_floor_for_test();
        let floor = ctrl.current_bitrate();
        for _ in 0..40 {
            ctrl.record_frame(FrameStats {
                encode_ms: 12.0,
                frame_bytes: 50_000,
                is_keyframe: false,
            });
        }
        let decision = ctrl.evaluate();
        assert!(decision.bitrate_bps >= floor);
    }

    #[test]
    fn test_bitrate_recovery_only_when_fps_at_target() {
        let mut ctrl = make_controller(10_000_000, false, 60);
        ctrl.set_bitrate_to_floor_for_test();
        for _ in 0..40 {
            ctrl.record_frame(FrameStats {
                encode_ms: 14.0,
                frame_bytes: 50_000,
                is_keyframe: false,
            });
        }
        ctrl.evaluate();
        assert_eq!(ctrl.current_fps(), 30);
        let floor_bitrate = ctrl.current_bitrate();
        for _ in 0..40 {
            ctrl.record_frame(FrameStats {
                encode_ms: 3.0,
                frame_bytes: 20_000,
                is_keyframe: false,
            });
        }
        let decision = ctrl.evaluate();
        assert_eq!(decision.fps, 60);
        assert_eq!(decision.bitrate_bps, floor_bitrate);
        for _ in 0..40 {
            ctrl.record_frame(FrameStats {
                encode_ms: 3.0,
                frame_bytes: 20_000,
                is_keyframe: false,
            });
        }
        let decision2 = ctrl.evaluate();
        assert!(decision2.bitrate_bps > floor_bitrate);
    }

    #[test]
    fn test_fps_decrease_after_bitrate_floor() {
        let mut ctrl = make_controller(10_000_000, false, 60);
        ctrl.set_bitrate_to_floor_for_test();
        for _ in 0..40 {
            ctrl.record_frame(FrameStats {
                encode_ms: 14.0,
                frame_bytes: 50_000,
                is_keyframe: false,
            });
        }
        let decision = ctrl.evaluate();
        assert_eq!(decision.fps, 30);
    }

    #[test]
    fn test_keyframes_excluded_from_size_evaluation() {
        let mut ctrl = make_controller(10_000_000, false, 60);
        for _ in 0..40 {
            ctrl.record_frame(FrameStats {
                encode_ms: 5.0,
                frame_bytes: 500_000,
                is_keyframe: true,
            });
        }
        let decision = ctrl.evaluate();
        assert_eq!(decision.bitrate_bps, 10_000_000);
    }

    #[test]
    fn test_idle_recovery_resets() {
        let mut ctrl = make_controller(10_000_000, false, 60);
        for _ in 0..40 {
            ctrl.record_frame(FrameStats {
                encode_ms: 12.0,
                frame_bytes: 50_000,
                is_keyframe: false,
            });
        }
        ctrl.evaluate();
        assert!(ctrl.current_bitrate() < 10_000_000);
        ctrl.on_idle_recovery();
        assert_eq!(ctrl.current_bitrate(), 10_000_000);
        assert_eq!(ctrl.current_fps(), 60);
    }

    #[test]
    fn test_network_quality_scores() {
        let nq = NetworkQuality {
            rtt_ms: 1.0,
            is_private_ip: true,
        };
        assert_eq!(nq.score(), 1.0);
        let nq = NetworkQuality {
            rtt_ms: 8.0,
            is_private_ip: true,
        };
        assert!((nq.score() - 0.8).abs() < 0.01);
        let nq = NetworkQuality {
            rtt_ms: 30.0,
            is_private_ip: false,
        };
        assert!((nq.score() - 0.4).abs() < 0.01);
        let nq = NetworkQuality {
            rtt_ms: 150.0,
            is_private_ip: false,
        };
        assert!((nq.score() - 0.1).abs() < 0.01);
    }

    #[test]
    fn test_network_quality_from_ip() {
        let nq = NetworkQuality::from_ip(true);
        assert!(nq.score() >= 0.8);
        let nq = NetworkQuality::from_ip(false);
        assert!(nq.score() <= 0.5);
    }

    #[test]
    fn test_high_quality_network_keeps_fps_high_when_encode_is_healthy() {
        let mut ctrl = BitrateController::new(10_000_000, 60, NetworkQuality::from_ip(true));
        for _ in 0..40 {
            ctrl.record_frame(FrameStats {
                encode_ms: 3.0,
                frame_bytes: 50_000,
                is_keyframe: false,
            });
        }
        let decision = ctrl.evaluate();
        assert_eq!(decision.fps, 60);
        assert!(decision.bitrate_bps <= 10_000_000);
    }

    #[test]
    fn test_score_based_poor_quality_aggressive() {
        let nq = NetworkQuality {
            rtt_ms: 80.0,
            is_private_ip: false,
        };
        let mut ctrl = BitrateController::new(10_000_000, 60, nq);
        for _ in 0..40 {
            ctrl.record_frame(FrameStats {
                encode_ms: 12.0,
                frame_bytes: 50_000,
                is_keyframe: false,
            });
        }
        let decision = ctrl.evaluate();
        assert!(decision.bitrate_bps <= (10_000_000.0 * 0.80) as u32);
    }

    #[test]
    fn test_severe_encode_overload_reduces_fps_before_bitrate_floor() {
        let mut ctrl = make_controller(10_000_000, false, 60);
        for _ in 0..40 {
            ctrl.record_frame(FrameStats {
                encode_ms: 18.0,
                frame_bytes: 50_000,
                is_keyframe: false,
            });
        }
        let decision = ctrl.evaluate();
        assert_eq!(decision.fps, 30);
        assert!(decision.bitrate_bps < 10_000_000);
    }
}
