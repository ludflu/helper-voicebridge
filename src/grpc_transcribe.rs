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

fn playback_rate(sample_rate: u32) -> u32 {
    if sample_rate == 0 {
        DEFAULT_PLAYBACK_RATE
    } else {
        sample_rate
    }
}

/// Merge streamed reply chunks into transcript text, PCM, and sample rate.
fn collect_speech_replies(
    replies: &[SpeechReply],
) -> (String, Vec<u8>, u32) {
    let mut transcript = String::new();
    let mut pcm = Vec::new();
    let mut sample_rate = DEFAULT_PLAYBACK_RATE;

    for reply in replies {
        if !reply.text.is_empty() {
            transcript = reply.text.clone();
        }
        if !reply.pcm_s16le.is_empty() {
            pcm.extend_from_slice(&reply.pcm_s16le);
            sample_rate = playback_rate(reply.sample_rate);
        }
    }

    (transcript, pcm, sample_rate)
}

/// Split PCM bytes into gRPC upload chunks (last chunk marked end_of_stream).
fn split_audio_chunks(data: &[u8], chunk_size: usize) -> Vec<(Vec<u8>, bool)> {
    if data.is_empty() {
        return vec![(Vec::new(), true)];
    }
    let mut out = Vec::new();
    let mut offset = 0;
    while offset < data.len() {
        let end = (offset + chunk_size).min(data.len());
        let last = end >= data.len();
        out.push((data[offset..end].to_vec(), last));
        offset = end;
    }
    out
}

fn audio_chunk_stream(data: Vec<u8>) -> impl tokio_stream::Stream<Item = AudioChunk> {
    async_stream::stream! {
        for (pcm, last) in split_audio_chunks(&data, CHUNK_BYTES) {
            yield AudioChunk {
                pcm_s16le: pcm,
                end_of_stream: last,
            };
        }
    }
}

async fn play_response_stream(
    mut stream: Streaming<SpeechReply>,
    pulse_sink: Option<&str>,
) -> Result<GrpcSessionResult> {
    let mut replies = Vec::new();
    while let Some(reply) = stream.message().await.context("read response chunk")? {
        let done = reply.end_of_stream;
        replies.push(reply);
        if done {
            break;
        }
    }

    let (transcript, pcm, sample_rate) = collect_speech_replies(&replies);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_grpc_uri_adds_http_scheme() {
        assert_eq!(
            normalize_grpc_uri("192.168.1.238:50051"),
            "http://192.168.1.238:50051"
        );
        assert_eq!(
            normalize_grpc_uri("http://localhost:50051"),
            "http://localhost:50051"
        );
        assert_eq!(
            normalize_grpc_uri("https://localhost:50051"),
            "https://localhost:50051"
        );
    }

    #[test]
    fn playback_rate_defaults_when_zero() {
        assert_eq!(playback_rate(0), 16_000);
        assert_eq!(playback_rate(22_050), 22_050);
    }

    #[test]
    fn split_audio_chunks_empty() {
        assert_eq!(split_audio_chunks(b"", 4), vec![(Vec::new(), true)]);
    }

    #[test]
    fn split_audio_chunks_marks_last() {
        assert_eq!(
            split_audio_chunks(b"abcdef", 4),
            vec![(b"abcd".to_vec(), false), (b"ef".to_vec(), true)]
        );
    }

    #[test]
    fn collect_speech_replies_merges_transcript_and_pcm() {
        let replies = vec![
            SpeechReply {
                text: "hello".into(),
                pcm_s16le: vec![1, 2],
                sample_rate: 16_000,
                end_of_stream: false,
            },
            SpeechReply {
                text: String::new(),
                pcm_s16le: vec![3, 4],
                sample_rate: 0,
                end_of_stream: true,
            },
        ];
        let (text, pcm, rate) = collect_speech_replies(&replies);
        assert_eq!(text, "hello");
        assert_eq!(pcm, vec![1, 2, 3, 4]);
        assert_eq!(rate, 16_000);
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
