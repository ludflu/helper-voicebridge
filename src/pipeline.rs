//! Imperative shell: runs the `Effect`s the core state machine emits.

use anyhow::{Context, Result};
use std::path::PathBuf;
use std::time::{Duration, Instant};
use tokio::process::{Child, Command};

use crate::core::Event;

const CAPTURE_FILE: &str = "/tmp/qc-voice-bridge-capture.wav";
const MIN_WAV_BYTES: u64 = 1000;

pub struct Config {
    pub whisper_binary: PathBuf,
    pub whisper_model: PathBuf,
    /// PulseAudio/PipeWire source name for the connected headset's mic,
    /// e.g. "bluez_input.XX_XX_XX_XX_XX_XX" -- find with `pactl list sources`.
    pub pulse_source: String,
    /// Delay before parecord; 0 when mic source is already RUNNING (pulse backend).
    pub capture_sco_delay_ms: u64,
}

/// Handle to a running `parecord` process, so StopCapture can kill it.
pub struct CaptureHandle {
    child: Child,
    recording_since: Instant,
}

pub fn env_ms(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

pub async fn start_capture(cfg: &Config) -> Result<CaptureHandle> {
    // Unsuspend in case the previous session left the source suspended.
    let _ = Command::new("pactl")
        .args(["suspend-source", &cfg.pulse_source, "0"])
        .status()
        .await;

    let sco_delay = Duration::from_millis(cfg.capture_sco_delay_ms);
    if !sco_delay.is_zero() {
        tokio::time::sleep(sco_delay).await;
    }

    let child = Command::new("parecord")
        .args([
            "--device",
            &cfg.pulse_source,
            "--file-format=wav",
            "--format=s16le",
            "--rate=16000",
            "--channels=1",
            CAPTURE_FILE,
        ])
        .kill_on_drop(true)
        .spawn()
        .with_context(|| format!("failed to spawn parecord on device {}", cfg.pulse_source))?;

    tracing::info!("recording from {} -> {}", cfg.pulse_source, CAPTURE_FILE);
    Ok(CaptureHandle {
        child,
        recording_since: Instant::now(),
    })
}

pub async fn stop_capture_and_transcribe(cfg: &Config, handle: CaptureHandle) -> Event {
    tracing::info!("stopping capture");

    let CaptureHandle {
        mut child,
        recording_since,
    } = handle;

    let min_capture = Duration::from_millis(env_ms("MIN_CAPTURE_MS", 1500));
    let recorded = recording_since.elapsed();
    if recorded < min_capture {
        tokio::time::sleep(min_capture - recorded).await;
    }

    // Stop parecord before releasing SCO — it holds an extra transport acquire.
    if let Some(pid) = child.id() {
        let _ = Command::new("kill")
            .args(["-INT", &pid.to_string()])
            .status()
            .await;
    }
    match tokio::time::timeout(Duration::from_secs(2), child.wait()).await {
        Ok(status) => {
            if let Ok(exit) = &status {
                if !exit.success() {
                    tracing::warn!("parecord exited with {exit}");
                }
            }
        }
        Err(_) => {
            tracing::warn!("parecord did not exit after SIGINT, sending SIGKILL");
            let _ = child.start_kill();
            let _ = child.wait().await;
        }
    }

    // Give parecord a moment to flush the WAV to disk.
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Drop the HFP SCO session (QC often omits AT+BVRA=0).
    release_sco_mic(cfg).await;

    match run_whisper(cfg).await {
        Ok(text) => {
            let trimmed = text.trim();
            if trimmed.is_empty() {
                println!("(no speech detected)");
                tracing::info!("transcript: (empty)");
            } else {
                println!("{trimmed}");
                tracing::info!(transcript = %trimmed, "user said");
            }
            Event::TranscriptReady(text)
        }
        Err(e) => {
            eprintln!("transcription failed: {e:#}");
            tracing::warn!("transcription failed: {e:#}");
            Event::TranscriptFailed
        }
    }
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

    // Suspend only drops the PipeWire node; cycle HFP profile to tear down SCO.
    if let Some(card) = pulse_card_from_source(&cfg.pulse_source) {
        cycle_hfp_profile(&card).await;
    }
}

/// `bluez_input.E4_58_BC_80_90_CD` → `bluez_card.E4_58_BC_80_90_CD`
fn pulse_card_from_source(source: &str) -> Option<String> {
    source
        .strip_prefix("bluez_input.")
        .map(|mac| format!("bluez_card.{mac}"))
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

async fn run_whisper(cfg: &Config) -> Result<String> {
    let meta = tokio::fs::metadata(CAPTURE_FILE).await;
    match &meta {
        Ok(m) if m.len() < MIN_WAV_BYTES => {
            anyhow::bail!(
                "capture file too small ({} bytes) — mic may not have been ready; try holding the button longer",
                m.len()
            );
        }
        Err(_) => {
            anyhow::bail!("capture file {CAPTURE_FILE} not found — parecord did not write audio")
        }
        _ => {}
    }

    let output = Command::new(&cfg.whisper_binary)
        .args([
            "-m",
            cfg.whisper_model.to_str().unwrap(),
            "-f",
            CAPTURE_FILE,
            "--no-timestamps",
            "--output-txt",
        ])
        .output()
        .await
        .with_context(|| format!("failed to run {}", cfg.whisper_binary.display()))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!(
            "whisper exited with {}: {}",
            output.status,
            stderr.trim()
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}
