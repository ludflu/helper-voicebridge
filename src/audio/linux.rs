use anyhow::{Context, Result};
use libpulse_binding as pulse;
use libpulse_simple_binding as psimple;
use pulse::sample::{Format, Spec};
use pulse::stream::Direction;
use std::process::Stdio;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::time::Duration;
use tokio::io::AsyncReadExt;
use tokio::process::Command;
use tokio::sync::mpsc;

const SAMPLE_RATE: u32 = 16_000;
const CHANNELS: u8 = 1;
/// ~20 ms of mono s16le @ 16 kHz.
const READ_BYTES: usize = (SAMPLE_RATE as usize * 2 * CHANNELS as usize) / 50;

pub fn playback_warmup() -> Duration {
    Duration::from_millis(400)
}

pub fn unsuspend_source(source: &str) -> Result<()> {
    std::process::Command::new("pactl")
        .args(["suspend-source", source, "0"])
        .status()
        .context("pactl unsuspend source")?;
    Ok(())
}

/// Live PCM via `parecord --raw` (triggers SCO reliably on PipeWire/BlueZ).
pub fn spawn_recorder(
    source: &str,
    chunk_tx: mpsc::Sender<Vec<u8>>,
    stop: Arc<AtomicBool>,
) -> Result<tokio::task::JoinHandle<()>> {
    let source = source.to_string();
    Ok(tokio::spawn(async move {
        if let Err(e) = parecord_loop(&source, chunk_tx, stop).await {
            eprintln!("parecord capture failed: {e:#}");
        }
    }))
}

async fn parecord_loop(
    source: &str,
    chunk_tx: mpsc::Sender<Vec<u8>>,
    stop: Arc<AtomicBool>,
) -> Result<()> {
    let mut cmd = Command::new("parecord");
    cmd.args([
        "-d",
        source,
        "--format=s16le",
        "--rate",
        "16000",
        "--channels=1",
        "--raw",
    ])
    .stdout(Stdio::piped())
    .stderr(Stdio::piped())
    .kill_on_drop(true);

    if let Ok(ps) = std::env::var("PULSE_SERVER") {
        cmd.env("PULSE_SERVER", ps);
    }
    if let Ok(xdg) = std::env::var("XDG_RUNTIME_DIR") {
        cmd.env("XDG_RUNTIME_DIR", xdg);
    }

    let mut child = cmd
        .spawn()
        .with_context(|| format!("spawn parecord on {source}"))?;

    eprintln!("parecord started on {source}");
    let mut stdout = child
        .stdout
        .take()
        .context("parecord stdout unavailable")?;

    let mut buf = vec![0u8; READ_BYTES];
    let mut total = 0u64;
    while !stop.load(Ordering::Relaxed) {
        let n = match tokio::time::timeout(Duration::from_millis(250), stdout.read(&mut buf)).await
        {
            Ok(Ok(0)) => break,
            Ok(Ok(n)) => n,
            Ok(Err(e)) => anyhow::bail!("parecord read: {e}"),
            Err(_) => continue,
        };
        total += n as u64;
        if chunk_tx.send(buf[..n].to_vec()).await.is_err() {
            break;
        }
    }

    stop.store(true, Ordering::Relaxed);
    let _ = child.kill().await;
    let status = child.wait().await.context("parecord wait")?;
    if !status.success() {
        eprintln!("parecord exited with {status}");
    }
    eprintln!("parecord captured {total} bytes");
    Ok(())
}

pub fn spawn_playback(
    pcm: Vec<u8>,
    sample_rate: u32,
    sink: Option<&str>,
    stop: Arc<AtomicBool>,
) -> Result<tokio::task::JoinHandle<()>> {
    let sink = sink.map(str::to_string);
    Ok(tokio::task::spawn_blocking(move || {
        if let Err(e) = play_loop(&pcm, sample_rate, sink.as_deref(), stop) {
            tracing::warn!("pulse playback exited: {e:#}");
        }
    }))
}

fn play_loop(
    pcm: &[u8],
    sample_rate: u32,
    sink: Option<&str>,
    stop: Arc<AtomicBool>,
) -> Result<()> {
    let spec = Spec {
        format: Format::S16NE,
        channels: CHANNELS,
        rate: sample_rate,
    };
    if !spec.is_valid() {
        anyhow::bail!("invalid playback spec (rate={sample_rate})");
    }

    let simple = psimple::Simple::new(
        None,
        "qc-voice-bridge",
        Direction::Playback,
        sink,
        "tts playback",
        &spec,
        None,
        None,
    )
    .map_err(|e| anyhow::anyhow!("open pulse playback stream: {e}"))?;

    let mut offset = 0;
    while offset < pcm.len() && !stop.load(Ordering::Relaxed) {
        let end = (offset + READ_BYTES).min(pcm.len());
        simple
            .write(&pcm[offset..end])
            .map_err(|e| anyhow::anyhow!("pulse write failed: {e}"))?;
        offset = end;
    }

    if stop.load(Ordering::Relaxed) {
        let _ = simple.flush();
        tracing::info!("pulse playback aborted (barge-in)");
    } else {
        simple
            .drain()
            .map_err(|e| anyhow::anyhow!("pulse drain failed: {e}"))?;
    }
    Ok(())
}
