//! Imperative shell: runs the `Effect`s the core state machine emits.

use anyhow::{Context, Result};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::process::Command;

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
    LiveGrpc,
    LocalBuffer,
}

/// Timestamp-only handle; PCM comes from the always-on pre-roll recorder.
pub struct CaptureHandle {
    started: Instant,
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
        _ => env_ms("CAPTURE_SCO_DELAY_MS", 0),
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

pub async fn warm_sco(cfg: &Config) -> Result<()> {
    quick_prepare_sco(cfg).await
}

pub async fn start_capture(
    cfg: &Config,
    preroll: &Arc<crate::audio::PreRollRecorder>,
) -> Result<CaptureHandle> {
    let started = Instant::now();
    quick_prepare_sco(cfg).await?;
    let preroll_ms = env_ms("PREROLL_MS", 3000);
    preroll.begin_session(preroll_ms);
    let ready = wait_for_sco_running(&cfg.pulse_source).await;
    if ready {
        eprintln!("SCO mic RUNNING");
    } else {
        eprintln!("SCO mic not RUNNING yet — continuing capture");
    }
    let backend = if cfg.whisper_grpc_addr.is_some() {
        CaptureBackend::LiveGrpc
    } else {
        CaptureBackend::LocalBuffer
    };
    tracing::info!(source = %cfg.pulse_source, preroll_ms, "capture started");
    Ok(CaptureHandle { started, backend })
}

async fn run_live_grpc_buffer(
    cfg: &Config,
    pcm: Vec<u8>,
) -> Event {
    let addr = match cfg.whisper_grpc_addr.as_deref() {
        Some(a) => a,
        None => return Event::TranscriptFailed,
    };
    eprintln!("sending {} bytes to whisper gRPC", pcm.len());
    tracing::info!(%addr, bytes = pcm.len(), "uploading capture to whisper gRPC");
    match crate::grpc_transcribe::transcribe_pcm(addr, pcm, cfg.pulse_sink.as_deref()).await {
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
                say_thank_you(cfg.pulse_sink.as_deref()).await;
            }
            eprintln!("transcript ready");
            Event::TranscriptReady(result.text)
        }
        Err(e) => {
            eprintln!("transcription failed: {e:#}");
            tracing::warn!("live gRPC transcription failed: {e:#}");
            Event::TranscriptFailed
        }
    }
}

pub async fn stop_capture_and_transcribe(
    cfg: &Config,
    handle: CaptureHandle,
    preroll: &Arc<crate::audio::PreRollRecorder>,
) -> Event {
    tracing::info!("stopping capture");

    let min_capture = Duration::from_millis(env_ms("MIN_CAPTURE_MS", 1500));
    let recorded = handle.started.elapsed();
    if recorded < min_capture {
        tokio::time::sleep(min_capture - recorded).await;
    }

    let pcm = preroll.end_session();
    let event = if pcm.len() < MIN_PCM_BYTES {
        eprintln!("capture too small ({} bytes) — SCO may not have opened", pcm.len());
        tracing::warn!(bytes = pcm.len(), "capture too small");
        Event::TranscriptFailed
    } else {
        match handle.backend {
            CaptureBackend::LiveGrpc => run_live_grpc_buffer(cfg, pcm).await,
            CaptureBackend::LocalBuffer => run_local_whisper(cfg, pcm).await,
        }
    };

    release_sco_mic(cfg).await;
    event
}

async fn run_local_whisper(cfg: &Config, pcm: Vec<u8>) -> Event {
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
    if env_ms("SCO_SUSPEND_ON_RELEASE", 0) == 0 {
        return;
    }

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

    if env_ms("HFP_PROFILE_CYCLE", 1) == 0 {
        return;
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

/// Fast SCO prep — unsuspend and ensure HFP. No profile cycling (that takes ~1s).
async fn quick_prepare_sco(cfg: &Config) -> Result<()> {
    crate::audio::unsuspend_source(&cfg.pulse_source)?;
    if let Some(card) = pulse_card_for_source(&cfg.pulse_source).await {
        let _ = Command::new("pactl")
            .args(["set-card-profile", &card, "headset-head-unit"])
            .status()
            .await;
    }
    Ok(())
}

/// Full SCO reset (a2dp ↔ HFP). Used only when `SCO_RESET_ON_START=1` or after failed capture.
async fn reset_sco(cfg: &Config) -> Result<()> {
    if env_ms("SCO_RESET_ON_START", 0) == 0 {
        return quick_prepare_sco(cfg).await;
    }

    let _ = Command::new("pactl")
        .args(["suspend-source", &cfg.pulse_source, "1"])
        .status()
        .await;
    tokio::time::sleep(Duration::from_millis(200)).await;

    if let Some(card) = pulse_card_for_source(&cfg.pulse_source).await {
        for profile in ["a2dp-sink", "headset-head-unit"] {
            let _ = Command::new("pactl")
                .args(["set-card-profile", &card, profile])
                .status()
                .await;
            tokio::time::sleep(Duration::from_millis(400)).await;
        }
    }

    quick_prepare_sco(cfg).await
}

/// Wait until the bluez mic source is RUNNING (SCO open).
async fn wait_for_sco_running(source: &str) -> bool {
    let timeout = Duration::from_millis(env_ms("SCO_READY_TIMEOUT_MS", 3000));
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if pulse_source_running(source).await {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    false
}

async fn pulse_source_running(source: &str) -> bool {
    let output = Command::new("pactl")
        .args(["list", "sources", "short"])
        .output()
        .await;
    let Ok(o) = output else {
        return false;
    };
    if !o.status.success() {
        return false;
    }
    String::from_utf8_lossy(&o.stdout).lines().any(|line| {
        let mut parts = line.split_whitespace();
        parts.next();
        match (parts.next(), parts.last()) {
            (Some(name), Some(state)) => name == source && state == "RUNNING",
            _ => false,
        }
    })
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
