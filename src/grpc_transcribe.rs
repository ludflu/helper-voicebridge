//! Stream captured audio to the Mac gRPC service and play streamed TTS back.

use anyhow::{Context, Result};
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use tonic::transport::{Channel, Endpoint};
use tonic::Streaming;

pub mod pb {
    tonic::include_proto!("whisper.v1");
}

use pb::{whisper_transcriber_client::WhisperTranscriberClient, AudioChunk, SpeechReply};

const CHUNK_BYTES: usize = 32 * 1024;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(180);
const DEFAULT_PLAYBACK_RATE: u32 = 16000;
const PAPLAY_WARMUP: Duration = Duration::from_millis(400);

/// Stop in-flight TTS playback (barge-in while a prior turn is still playing).
pub async fn abort_playback() {
    match Command::new("pkill")
        .args(["-f", "paplay --format=s16le"])
        .status()
        .await
    {
        Ok(status) if status.success() => {
            tracing::info!("aborted in-flight paplay");
        }
        Ok(_) => {}
        Err(e) => tracing::debug!("paplay abort skipped: {e}"),
    }
}

pub struct GrpcSessionResult {
    pub text: String,
    pub played_response: bool,
}

/// Stream capture to Mac, print transcript, play streamed response audio.
pub async fn transcribe_and_play(
    grpc_addr: &str,
    capture_path: &Path,
    pulse_sink: Option<&str>,
) -> Result<GrpcSessionResult> {
    let data = tokio::fs::read(capture_path)
        .await
        .with_context(|| format!("read capture file {}", capture_path.display()))?;
    let channel = connect(grpc_addr).await?;
    let mut client = WhisperTranscriberClient::new(channel);
    let response = client
        .transcribe(tonic::Request::new(audio_chunk_stream(data)))
        .await
        .context("gRPC Transcribe RPC failed")?
        .into_inner();
    play_response_stream(response, pulse_sink).await
}

async fn connect(addr: &str) -> Result<Channel> {
    let uri = normalize_grpc_uri(addr);
    Endpoint::from_shared(uri)?
        .connect_timeout(CONNECT_TIMEOUT)
        .timeout(REQUEST_TIMEOUT)
        .connect()
        .await
        .context("connect to whisper gRPC service")
}

fn normalize_grpc_uri(addr: &str) -> String {
    if addr.starts_with("http://") || addr.starts_with("https://") {
        addr.to_string()
    } else {
        format!("http://{addr}")
    }
}

fn audio_chunk_stream(data: Vec<u8>) -> impl tokio_stream::Stream<Item = AudioChunk> {
    async_stream::stream! {
        if data.is_empty() {
            yield AudioChunk {
                pcm_s16le: Vec::new(),
                end_of_stream: true,
            };
            return;
        }
        let mut offset = 0;
        while offset < data.len() {
            let end = (offset + CHUNK_BYTES).min(data.len());
            let last = end >= data.len();
            yield AudioChunk {
                pcm_s16le: data[offset..end].to_vec(),
                end_of_stream: last,
            };
            offset = end;
        }
    }
}

async fn play_response_stream(
    mut stream: Streaming<SpeechReply>,
    pulse_sink: Option<&str>,
) -> Result<GrpcSessionResult> {
    let mut transcript = String::new();
    let mut pcm = Vec::new();
    let mut sample_rate = DEFAULT_PLAYBACK_RATE;

    while let Some(reply) = stream.message().await.context("read response chunk")? {
        if !reply.text.is_empty() {
            transcript = reply.text;
        }
        if !reply.pcm_s16le.is_empty() {
            pcm.extend_from_slice(&reply.pcm_s16le);
            sample_rate = playback_rate(reply.sample_rate);
        }
        if reply.end_of_stream {
            break;
        }
    }

    let played = if pcm.is_empty() {
        false
    } else {
        play_pcm_buffer(&pcm, sample_rate, pulse_sink).await?;
        true
    };

    Ok(GrpcSessionResult {
        text: transcript,
        played_response: played,
    })
}

async fn play_pcm_buffer(
    pcm: &[u8],
    sample_rate: u32,
    pulse_sink: Option<&str>,
) -> Result<()> {
    tracing::info!(
        bytes = pcm.len(),
        sample_rate,
        ?pulse_sink,
        "playing response audio"
    );
    let mut child = spawn_paplay(pulse_sink, sample_rate).await?;
    tokio::time::sleep(PAPLAY_WARMUP).await;
    if let Some(stdin) = child.stdin.as_mut() {
        stdin.write_all(pcm).await?;
    }
    drop(child.stdin.take());
    match tokio::time::timeout(Duration::from_secs(30), child.wait()).await {
        Ok(status) => {
            if let Ok(exit) = &status {
                if !exit.success() {
                    tracing::warn!("paplay exited with {exit}");
                }
            }
        }
        Err(_) => {
            tracing::warn!("paplay did not finish, killing");
            let _ = child.start_kill();
        }
    }
    Ok(())
}

fn playback_rate(sample_rate: u32) -> u32 {
    if sample_rate == 0 {
        DEFAULT_PLAYBACK_RATE
    } else {
        sample_rate
    }
}

async fn spawn_paplay(sink: Option<&str>, sample_rate: u32) -> Result<tokio::process::Child> {
    let mut cmd = Command::new("paplay");
    cmd.args([
        "--format=s16le",
        &format!("--rate={sample_rate}"),
        "--channels=1",
        "--raw",
    ]);
    if let Some(s) = sink {
        cmd.args(["--device", s]);
    }
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .context("spawn paplay for streamed response")
}
