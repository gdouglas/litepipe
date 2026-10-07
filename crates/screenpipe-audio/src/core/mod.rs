pub mod device;
pub mod device_detection;
pub mod engine;
#[cfg(target_os = "macos")]
pub mod process_tap;
#[cfg(all(target_os = "linux", feature = "pulseaudio"))]
pub mod pulse;
mod run_record_and_transcribe;
pub mod source_buffer;
pub mod stream;
use crate::AudioInput;
use anyhow::Result;
use dashmap::DashMap;
use lazy_static::lazy_static;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use stream::AudioStream;

lazy_static! {
    // Global fallback timestamp for backward compatibility
    pub static ref LAST_AUDIO_CAPTURE: AtomicU64 = AtomicU64::new(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
    );

    // Per-device tracking of last audio capture
    pub static ref DEVICE_AUDIO_CAPTURES: DashMap<String, AtomicU64> = DashMap::new();
}

/// Updates the last capture time for a specific device
pub fn update_device_capture_time(device_name: &str) {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    // Update the global timestamp for backward compatibility
    LAST_AUDIO_CAPTURE.store(now, Ordering::Relaxed);

    // Update or insert the device-specific timestamp
    DEVICE_AUDIO_CAPTURES
        .entry(device_name.to_string())
        .or_insert_with(|| AtomicU64::new(now))
        .store(now, Ordering::Relaxed);
}

/// The last time any device delivered real audio, or `None` if none has
/// since the process started. Unlike `get_device_capture_time`, this never
/// falls back to the process start time, so it moves only when audio is
/// actually being captured.
pub fn last_real_capture_time() -> Option<u64> {
    DEVICE_AUDIO_CAPTURES
        .iter()
        .map(|entry| entry.value().load(Ordering::Relaxed))
        .max()
}

/// Gets the last capture time for a specific device
pub fn get_device_capture_time(device_name: &str) -> u64 {
    DEVICE_AUDIO_CAPTURES
        .get(device_name)
        .map(|atomic| atomic.load(Ordering::Relaxed))
        .unwrap_or_else(|| LAST_AUDIO_CAPTURE.load(Ordering::Relaxed))
}

#[cfg(all(test, target_os = "macos"))]
mod e2e_ghost_word_silent_room;

/// Runs one recording session for the device. Returns `Ok(())` on normal
/// shutdown (is_running went false) and `Err` when the underlying stream is
/// dead (is_disconnected latched, timeout, CPAL error).
///
/// There is no in-function retry loop — on `Err`, the task handle finishes
/// and `device_monitor::check_stale_recording_handles` notices within ~2s.
/// That path calls `cleanup_stale_device` which removes the dead AudioStream
/// from device_manager, then `start_device` rebuilds it. Retrying inside this
/// function with the same `Arc<AudioStream>` cannot work — once `is_disconnected`
/// latches to true, every subsequent `run_record_and_transcribe` returns Err
/// in microseconds, so the previous exponential-backoff loop just produced
/// log spam + CPU churn without making progress.
pub async fn record_and_transcribe(
    audio_stream: Arc<AudioStream>,
    duration: std::time::Duration,
    whisper_sender: Arc<crossbeam::channel::Sender<AudioInput>>,
    is_running: Arc<AtomicBool>,
    metrics: Arc<crate::metrics::AudioPipelineMetrics>,
) -> Result<()> {
    record_and_transcribe_with_live_tap(
        audio_stream,
        duration,
        whisper_sender,
        is_running,
        metrics,
        None,
    )
    .await
}

pub async fn record_and_transcribe_with_live_tap(
    audio_stream: Arc<AudioStream>,
    duration: std::time::Duration,
    whisper_sender: Arc<crossbeam::channel::Sender<AudioInput>>,
    is_running: Arc<AtomicBool>,
    metrics: Arc<crate::metrics::AudioPipelineMetrics>,
    live_audio_tap: Option<crate::meeting_streaming::MeetingAudioTap>,
) -> Result<()> {
    run_record_and_transcribe::run_record_and_transcribe(
        audio_stream,
        duration,
        whisper_sender,
        is_running,
        metrics,
        live_audio_tap,
    )
    .await
}

#[cfg(test)]
mod capture_time_tests {
    use super::*;

    /// The app's mic gate asks whether audio is still arriving after it
    /// closed. The time of the last database write answered that wrongly,
    /// because background transcription writes too. Only a device that
    /// delivered real audio may move this time.
    #[test]
    fn real_audio_moves_the_last_capture_time() {
        let before = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        update_device_capture_time("capture-time test device");
        let last = last_real_capture_time().expect("a device delivered audio");
        assert!(last >= before);
    }
}
