use crate::Metrics;

pub(super) fn enrich(metrics: &mut Metrics, stats: &macrdp_audio::AudioStats) {
    let audio = stats.snapshot();
    metrics.audio_dropped_frames = audio.dropped_frames;
    metrics.audio_stale_frames = audio.stale_frames;
    metrics.audio_discontinuities = audio.discontinuities;
    metrics.audio_invalid_frames = audio.invalid_frames;
    metrics.audio_queue_age_ms = audio.queue_age_ms;
    metrics.audio_queued_ms = audio.queued_ms;
}
