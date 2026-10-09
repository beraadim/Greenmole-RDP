use std::sync::{Arc, Mutex};
use std::time::Instant;

const MAX_SAMPLES: usize = 8192;

fn record_sample<T>(samples: &mut Vec<T>, sequence: u64, value: T) {
    if samples.len() < MAX_SAMPLES {
        samples.push(value);
    } else {
        samples[sequence as usize % MAX_SAMPLES] = value;
    }
}

/// Frame-level performance statistics collector.
/// When enabled, records per-frame metrics and generates summary reports.
pub struct PerfStats {
    enabled: bool,
    encode_times: Vec<f64>,
    frame_sizes: Vec<u32>,
    keyframe_count: u32,
    total_frames: u64,
    start_time: Instant,
    network_rtts: Vec<f64>,
    rtt_samples: u64,
    total_bytes: u64,
    capture_queue_times: Vec<f64>,
    captures_delivered: u64,
    captures_coalesced: u64,
}

impl PerfStats {
    pub fn new(enabled: bool) -> Self {
        Self {
            enabled,
            encode_times: Vec::with_capacity(if enabled { 4096 } else { 0 }),
            frame_sizes: Vec::with_capacity(if enabled { 4096 } else { 0 }),
            keyframe_count: 0,
            total_frames: 0,
            start_time: Instant::now(),
            network_rtts: Vec::with_capacity(if enabled { 256 } else { 0 }),
            rtt_samples: 0,
            total_bytes: 0,
            capture_queue_times: Vec::new(),
            captures_delivered: 0,
            captures_coalesced: 0,
        }
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// Record metrics for one encoded frame.
    pub fn record_frame(&mut self, encode_ms: f64, frame_bytes: u32, is_keyframe: bool) {
        if !self.enabled {
            return;
        }
        record_sample(&mut self.encode_times, self.total_frames, encode_ms);
        record_sample(&mut self.frame_sizes, self.total_frames, frame_bytes);
        self.total_bytes = self.total_bytes.saturating_add(u64::from(frame_bytes));
        self.total_frames += 1;
        if is_keyframe {
            self.keyframe_count += 1;
        }
    }

    /// Record an RTT sample.
    pub fn record_rtt(&mut self, rtt_ms: f64) {
        if !self.enabled {
            return;
        }
        record_sample(&mut self.network_rtts, self.rtt_samples, rtt_ms);
        self.rtt_samples += 1;
    }

    /// Captures combined into this delivery retain their damage but not their old image.
    pub fn record_capture(&mut self, queue_ms: f64, coalesced_frames: u64) {
        if !self.enabled {
            return;
        }
        record_sample(
            &mut self.capture_queue_times,
            self.captures_delivered,
            queue_ms,
        );
        self.captures_delivered += 1;
        self.captures_coalesced = self.captures_coalesced.saturating_add(coalesced_frames);
    }

    /// Print formatted performance summary to stdout.
    pub fn print_summary(&self) {
        if !self.enabled || self.total_frames == 0 {
            println!("Performance stats not available (no frames recorded).");
            return;
        }

        let duration = self.start_time.elapsed().as_secs_f64();
        let avg_fps = self.total_frames as f64 / duration;
        let kf_pct = if self.total_frames > 0 {
            self.keyframe_count as f64 / self.total_frames as f64 * 100.0
        } else {
            0.0
        };

        // Encode latency stats
        let enc = LatencyStats::compute(&self.encode_times);

        // Frame size stats
        let sizes_f64: Vec<f64> = self.frame_sizes.iter().map(|&s| s as f64).collect();
        let size_stats = LatencyStats::compute(&sizes_f64);

        // Throughput
        let avg_mbps = self.total_bytes as f64 * 8.0 / duration / 1_000_000.0;

        // RTT stats
        let rtt = LatencyStats::compute(&self.network_rtts);

        println!();
        println!("═══════════════════════════════════════════");
        println!("  macrdp Performance Report");
        println!("═══════════════════════════════════════════");
        println!("  Duration:      {:.1}s", duration);
        println!("  Total frames:  {}", self.total_frames);
        println!("  Avg FPS:       {:.1}", avg_fps);
        println!("  Keyframes:     {} ({:.1}%)", self.keyframe_count, kf_pct);
        println!("───────────────────────────────────────────");
        println!(
            "  Encode Latency (ms, latest {} samples):",
            self.encode_times.len()
        );
        println!("    Mean:   {:<7.1} Stddev: {:.1}", enc.mean, enc.stddev);
        println!(
            "    P50:    {:<7.1} P95:    {:<7.1} P99:   {:.1}",
            enc.p50, enc.p95, enc.p99
        );
        println!("    Min:    {:<7.1} Max:    {:.1}", enc.min, enc.max);
        println!("───────────────────────────────────────────");
        println!("  Frame Size (KB):");
        println!(
            "    Mean:   {:<7.1} P95:   {:.1}",
            size_stats.mean / 1024.0,
            size_stats.p95 / 1024.0
        );
        println!("  Throughput:");
        println!("    Avg:    {:.1} Mbps", avg_mbps);
        if !self.network_rtts.is_empty() {
            println!("───────────────────────────────────────────");
            println!("  Graphics ACK latency (ms, includes client decoding):");
            println!("    Mean:   {:<7.1} P95:    {:.1}", rtt.mean, rtt.p95);
        }
        let capture = LatencyStats::compute(&self.capture_queue_times);
        println!(
            "  Captures: {} delivered, {} coalesced",
            self.captures_delivered, self.captures_coalesced
        );
        println!(
            "  Capture queue: mean {:.1}ms, p95 {:.1}ms",
            capture.mean, capture.p95
        );
        println!("═══════════════════════════════════════════");
        println!();
    }
}

struct LatencyStats {
    mean: f64,
    stddev: f64,
    p50: f64,
    p95: f64,
    p99: f64,
    min: f64,
    max: f64,
}

impl LatencyStats {
    fn compute(data: &[f64]) -> Self {
        if data.is_empty() {
            return Self {
                mean: 0.0,
                stddev: 0.0,
                p50: 0.0,
                p95: 0.0,
                p99: 0.0,
                min: 0.0,
                max: 0.0,
            };
        }
        let mut sorted: Vec<f64> = data.iter().copied().filter(|v| v.is_finite()).collect();
        if sorted.is_empty() {
            return Self::compute(&[]);
        }
        sorted.sort_by(f64::total_cmp);

        let n = sorted.len();
        let mean = sorted.iter().sum::<f64>() / n as f64;
        let variance = sorted.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / n as f64;
        let stddev = variance.sqrt();

        Self {
            mean,
            stddev,
            p50: sorted[n * 50 / 100],
            p95: sorted[(n * 95 / 100).min(n - 1)],
            p99: sorted[(n * 99 / 100).min(n - 1)],
            min: sorted[0],
            max: sorted[n - 1],
        }
    }
}

/// Shared, thread-safe wrapper around PerfStats for use across threads.
pub type SharedPerfStats = Arc<Mutex<PerfStats>>;

/// Create a new shared PerfStats.
pub fn new_shared(enabled: bool) -> SharedPerfStats {
    Arc::new(Mutex::new(PerfStats::new(enabled)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn long_sessions_bound_samples_without_losing_total_counters() {
        let mut stats = PerfStats::new(true);
        for _ in 0..MAX_SAMPLES + 100 {
            stats.record_frame(2.0, 1000, false);
            stats.record_rtt(8.0);
            stats.record_capture(1.0, 2);
        }
        assert_eq!(stats.encode_times.len(), MAX_SAMPLES);
        assert_eq!(stats.network_rtts.len(), MAX_SAMPLES);
        assert_eq!(stats.capture_queue_times.len(), MAX_SAMPLES);
        assert_eq!(stats.total_bytes, (MAX_SAMPLES as u64 + 100) * 1000);
        assert_eq!(stats.captures_coalesced, (MAX_SAMPLES as u64 + 100) * 2);
    }

    #[test]
    fn nonfinite_samples_do_not_panic_or_corrupt_percentiles() {
        let stats = LatencyStats::compute(&[f64::NAN, 2.0, f64::INFINITY]);
        assert_eq!(stats.p95, 2.0);
        assert_eq!(LatencyStats::compute(&[f64::NAN]).mean, 0.0);
    }
}
