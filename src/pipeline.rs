//! Imperative shell: runs the `Effect`s the core state machine emits.

use anyhow::{Context, Result};
use std::path::PathBuf;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::time::{Duration, Instant};
use tokio::process::Command;
use tokio::sync::{mpsc, oneshot};

use crate::core::Event;

const MIN_PCM_BYTES: usize = 1000;

#[derive(Clone)]
pub struct Config {
    pub whisper_binary: PathBuf,
    pub whisper_model: PathBuf,
    /// PulseAudio/PipeWire source name for the connected headset's mic,
    /// e.g. "bluez_input.XX_XX_XX_XX_XX_XX" -- find with `pactl list sources`.
    pub pulse_source: String,
    /// Headset speaker sink for TTS, e.g. "bluez_output.XX_XX_XX_XX_XX_XX.1"
    pub pulse_sink: Option<String>,
    /// Remote gRPC Whisper service, e.g. `http://192.168.1.10:50051` (Mac on LAN).
    pub whisper_grpc_addr: Option<String>,
    /// Delay before opening the pulse recorder; 0 when mic source is already RUNNING.
    pub capture_sco_delay_ms: u64,
}

enum CaptureBackend {
    /// PCM is streamed to whisper-grpc as it is captured.
    LiveGrpc { result_rx: oneshot::Receiver<Event> },
    /// PCM is buffered in memory for local whisper-cli (no gRPC).
    LocalBuffer {
        buffer: Arc<std::sync::Mutex<Vec<u8>>>,
    },
}

/// In-process pulse capture; forwards PCM live to gRPC when configured.
pub struct CaptureHandle {
    started: Instant,
    stop: Arc<AtomicBool>,
    pcm_tx: Option<mpsc::Sender<Vec<u8>>>,
    _recorder: tokio::task::JoinHandle<()>,
    backend: CaptureBackend,
}

pub fn env_ms(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

pub fn load_config(pulse_source: &str, backend: &crate::dbus_hfp::HfpBackend) -> Config {
    let capture_sco_delay_ms = match backend {
        crate::dbus_hfp::HfpBackend::Pulse { .. } => 0,
        _ => env_ms("CAPTURE_SCO_DELAY_MS", 400),
    };
    Config {
        whisper_binary: std::env::var("WHISPER_BIN")
            .unwrap_or_else(|_| "/usr/local/bin/whisper-cli".into())
            .into(),
        whisper_model: std::env::var("WHISPER_MODEL")
            .unwrap_or_else(|_| "/opt/whisper/models/ggml-base.en.bin".into())
            .into(),
        pulse_source: pulse_source.to_string(),
        pulse_sink: std::env::var("PULSE_SINK")
            .ok()
            .or_else(|| pulse_sink_from_source(pulse_source)),
        whisper_grpc_addr: std::env::var("WHISPER_GRPC_ADDR").ok(),
        capture_sco_delay_ms,
    }
}

pub async fn start_capture(cfg: &Config) -> Result<CaptureHandle> {
    crate::audio::unsuspend_source(&cfg.pulse_source)?;

    let sco_delay = Duration::from_millis(cfg.capture_sco_delay_ms);
    if !sco_delay.is_zero() {
        tokio::time::sleep(sco_delay).await;
    }

    let (pcm_tx, pcm_rx) = mpsc::channel(64);
    let stop = Arc::new(AtomicBool::new(false));
    let recorder = crate::audio::spawn_recorder(&cfg.pulse_source, pcm_tx.clone(), stop.clone())?;

    let backend = if let Some(addr) = cfg.whisper_grpc_addr.clone() {
        let (result_tx, result_rx) = oneshot::channel();
        let sink = cfg.pulse_sink.clone();
        tokio::spawn(async move {
            let event = run_live_grpc(&addr, pcm_rx, sink.as_deref()).await;
            let _ = result_tx.send(event);
        });
        CaptureBackend::LiveGrpc { result_rx }
    } else {
        let buffer = Arc::new(std::sync::Mutex::new(Vec::new()));
        let buffer_task = buffer.clone();
        tokio::spawn(async move {
            drain_pcm_to_buffer(pcm_rx, buffer_task).await;
        });
        CaptureBackend::LocalBuffer { buffer }
    };

    tracing::info!(source = %cfg.pulse_source, live_grpc = cfg.whisper_grpc_addr.is_some(), "capture started");

    Ok(CaptureHandle {
        started: Instant::now(),
        stop,
        pcm_tx: Some(pcm_tx),
        _recorder: recorder,
        backend,
    })
}

async fn drain_pcm_to_buffer(
    mut pcm_rx: mpsc::Receiver<Vec<u8>>,
    buffer: Arc<std::sync::Mutex<Vec<u8>>>,
) {
    while let Some(chunk) = pcm_rx.recv().await {
        if let Ok(mut buf) = buffer.lock() {
            buf.extend_from_slice(&chunk);
        }
    }
}

async fn run_live_grpc(addr: &str, pcm_rx: mpsc::Receiver<Vec<u8>>, sink: Option<&str>) -> Event {
    tracing::info!(%addr, "streaming mic PCM to whisper gRPC");
    match crate::grpc_transcribe::transcribe_live(addr, pcm_rx, sink).await {
        Ok(result) => {
            let trimmed = result.text.trim();
            if trimmed.is_empty() {
                println!("(no speech detected)");
                tracing::info!("transcript: (empty)");
            } else {
                println!("{trimmed}");
                tracing::info!(transcript = %trimmed, "user said");
            }
            if !result.played_response {
                say_thank_you(sink).await;
            }
            Event::TranscriptReady(result.text)
        }
        Err(e) => {
            eprintln!("transcription failed: {e:#}");
            tracing::warn!("live gRPC transcription failed: {e:#}");
            Event::TranscriptFailed
        }
    }
}

pub async fn stop_capture_and_transcribe(cfg: &Config, mut handle: CaptureHandle) -> Event {
    tracing::info!("stopping capture");

    let min_capture = Duration::from_millis(env_ms("MIN_CAPTURE_MS", 1500));
    let recorded = handle.started.elapsed();
    if recorded < min_capture {
        tokio::time::sleep(min_capture - recorded).await;
    }

    handle.stop.store(true, Ordering::Relaxed);
    drop(handle.pcm_tx.take());

    release_sco_mic(cfg).await;

    if cfg.whisper_grpc_addr.is_some() {
        let delay = Duration::from_millis(env_ms("PLAYBACK_SINK_DELAY_MS", 800));
        tokio::time::sleep(delay).await;
    }

    match handle.backend {
        CaptureBackend::LiveGrpc { result_rx } => {
            result_rx.await.unwrap_or(Event::TranscriptFailed)
        }
        CaptureBackend::LocalBuffer { buffer } => {
            let _ = handle._recorder.await;
            run_local_whisper(cfg, buffer).await
        }
    }
}

async fn run_local_whisper(cfg: &Config, buffer: Arc<std::sync::Mutex<Vec<u8>>>) -> Event {
    let pcm = buffer.lock().ok().map(|b| b.clone()).unwrap_or_default();
    if pcm.len() < MIN_PCM_BYTES {
        tracing::warn!(
            bytes = pcm.len(),
            "capture too small — mic may not have been ready"
        );
        return Event::TranscriptFailed;
    }

    let path = write_temp_wav(&pcm).await;
    match path {
        Ok(path) => {
            let result = run_whisper_cli(cfg, &path).await;
            let _ = tokio::fs::remove_file(&path).await;
            match result {
                Ok(text) => {
                    let trimmed = text.trim();
                    if trimmed.is_empty() {
                        println!("(no speech detected)");
                    } else {
                        println!("{trimmed}");
                        tracing::info!(transcript = %trimmed, "user said");
                    }
                    say_thank_you(cfg.pulse_sink.as_deref()).await;
                    Event::TranscriptReady(text)
                }
                Err(e) => {
                    eprintln!("transcription failed: {e:#}");
                    tracing::warn!("transcription failed: {e:#}");
                    Event::TranscriptFailed
                }
            }
        }
        Err(e) => {
            tracing::warn!("failed to write temp wav: {e:#}");
            Event::TranscriptFailed
        }
    }
}

async fn write_temp_wav(pcm: &[u8]) -> Result<std::path::PathBuf> {
    let path = std::path::PathBuf::from("/tmp/qc-voice-bridge-capture.wav");
    let header = wav_header(pcm.len() as u32);
    tokio::fs::write(&path, [header.as_slice(), pcm].concat())
        .await
        .context("write temp capture wav")?;
    Ok(path)
}

fn wav_header(pcm_len: u32) -> Vec<u8> {
    let sample_rate = 16_000u32;
    let byte_rate = sample_rate * 2;
    let mut h = Vec::with_capacity(44);
    h.extend_from_slice(b"RIFF");
    h.extend_from_slice(&(36 + pcm_len).to_le_bytes());
    h.extend_from_slice(b"WAVEfmt ");
    h.extend_from_slice(&16u32.to_le_bytes());
    h.extend_from_slice(&1u16.to_le_bytes());
    h.extend_from_slice(&1u16.to_le_bytes());
    h.extend_from_slice(&sample_rate.to_le_bytes());
    h.extend_from_slice(&byte_rate.to_le_bytes());
    h.extend_from_slice(&2u16.to_le_bytes());
    h.extend_from_slice(&16u16.to_le_bytes());
    h.extend_from_slice(b"data");
    h.extend_from_slice(&pcm_len.to_le_bytes());
    h
}

async fn run_whisper_cli(cfg: &Config, path: &std::path::Path) -> Result<String> {
    let output = Command::new(&cfg.whisper_binary)
        .args([
            "-m",
            cfg.whisper_model.to_str().unwrap(),
            "-f",
            path.to_str().unwrap(),
            "--no-timestamps",
            "--output-txt",
        ])
        .output()
        .await
        .with_context(|| format!("failed to run {}", cfg.whisper_binary.display()))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("whisper exited with {}: {}", output.status, stderr.trim());
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Drop the HFP SCO session so the QC mic leaves RUNNING (Bose often keeps
/// SCO open after AT+BVRA=1 until the source is suspended).
async fn release_sco_mic(cfg: &Config) {
    match Command::new("pactl")
        .args(["suspend-source", &cfg.pulse_source, "1"])
        .status()
        .await
    {
        Ok(s) if s.success() => {
            tracing::info!("suspended {} to close SCO", cfg.pulse_source);
        }
        Ok(s) => tracing::warn!("pactl suspend-source exited with {s}"),
        Err(e) => tracing::warn!("pactl suspend-source failed: {e}"),
    }

    let card = pulse_card_for_source(&cfg.pulse_source).await;
    if let Some(card) = card {
        cycle_hfp_profile(&card).await;
    } else {
        tracing::warn!(
            source = %cfg.pulse_source,
            "could not resolve Pulse card for profile cycle"
        );
    }
}

async fn pulse_card_for_source(source: &str) -> Option<String> {
    let output = Command::new("pactl")
        .args(["list", "sources"])
        .output()
        .await
        .ok()?;
    if !output.status.success() {
        return pulse_card_from_source(source);
    }

    let text = String::from_utf8_lossy(&output.stdout);
    let mut name_matches = false;
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("\tName: ") {
            name_matches = rest == source;
            continue;
        }
        if name_matches {
            if let Some(card) = line.strip_prefix("\tCard: ") {
                return Some(card.to_string());
            }
            if line.starts_with("Source #") {
                name_matches = false;
            }
        }
    }

    pulse_card_from_source(source)
}

fn pulse_card_from_source(source: &str) -> Option<String> {
    source
        .strip_prefix("bluez_input.")
        .map(|mac| format!("bluez_card.{}", mac.replace(':', "_")))
}

pub fn pulse_sink_from_source(source: &str) -> Option<String> {
    source
        .strip_prefix("bluez_input.")
        .map(|mac| format!("bluez_output.{}.1", mac.replace(':', "_")))
}

async fn say_thank_you(sink: Option<&str>) {
    let phrase = std::env::var("THANK_YOU_PHRASE").unwrap_or_else(|_| "thank you".into());
    let mut cmd = Command::new("espeak-ng");
    cmd.args(["-s", "150", &phrase]);
    if let Some(s) = sink {
        cmd.env("PULSE_SINK", s);
    }
    match cmd.status().await {
        Ok(s) if s.success() => tracing::info!(phrase = %phrase, "spoken to headset"),
        Ok(s) => tracing::warn!("espeak-ng exited with {s}"),
        Err(e) => tracing::warn!("espeak-ng failed: {e}"),
    }
}

async fn cycle_hfp_profile(card: &str) {
    for profile in ["a2dp-sink", "headset-head-unit"] {
        match Command::new("pactl")
            .args(["set-card-profile", card, profile])
            .status()
            .await
        {
            Ok(s) if s.success() => {
                tracing::info!("set-card-profile {card} -> {profile}");
            }
            Ok(s) => tracing::warn!("set-card-profile {card} {profile} exited with {s}"),
            Err(e) => tracing::warn!("set-card-profile {card} {profile} failed: {e}"),
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::pulse_card_from_source;

    #[test]
    fn pulse_sink_from_colon_mac() {
        assert_eq!(
            super::pulse_sink_from_source("bluez_input.E4:58:BC:80:90:CD"),
            Some("bluez_output.E4_58_BC_80_90_CD.1".into())
        );
    }

    #[test]
    fn pulse_card_from_colon_mac() {
        assert_eq!(
            pulse_card_from_source("bluez_input.E4:58:BC:80:90:CD"),
            Some("bluez_card.E4_58_BC_80_90_CD".into())
        );
    }

    #[test]
    fn pulse_card_from_underscore_mac() {
        assert_eq!(
            pulse_card_from_source("bluez_input.E4_58_BC_80_90_CD"),
            Some("bluez_card.E4_58_BC_80_90_CD".into())
        );
    }

    #[test]
    fn wav_header_length() {
        let h = super::wav_header(3200);
        assert_eq!(h.len(), 44);
        assert_eq!(&h[0..4], b"RIFF");
    }
}
