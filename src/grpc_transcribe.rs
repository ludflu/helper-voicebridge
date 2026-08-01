//! Stream captured audio to the Mac gRPC service and play streamed TTS back.

use anyhow::{Context, Result};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};
use std::time::Duration;
use tokio::sync::mpsc;
use tonic::transport::{Channel, Endpoint};
use tonic::Streaming;

pub mod pb {
    #![allow(clippy::all)] // tonic-generated service stubs
    tonic::include_proto!("whisper.v1");
}

use pb::{whisper_transcriber_client::WhisperTranscriberClient, AudioChunk, SpeechReply};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(180);
const DEFAULT_PLAYBACK_RATE: u32 = 16000;

static PLAYBACK_STOP: Mutex<Option<Arc<AtomicBool>>> = Mutex::new(None);
static GRPC_CHANNEL: Mutex<Option<(String, Channel)>> = Mutex::new(None);

pub struct GrpcSessionResult {
    pub text: String,
    pub played_response: bool,
}

/// Stop in-flight TTS playback (barge-in while a prior turn is still playing).
pub async fn abort_playback() {
    let stop = PLAYBACK_STOP.lock().ok().and_then(|mut g| g.take());
    if let Some(stop) = stop {
        stop.store(true, Ordering::Relaxed);
        tracing::info!("aborted in-flight pulse playback");
    }
}

/// Upload a complete PCM buffer (used after capture stops).
pub async fn transcribe_pcm(
    grpc_addr: &str,
    pcm: Vec<u8>,
    pulse_sink: Option<&str>,
) -> Result<GrpcSessionResult> {
    let channel = connect(grpc_addr).await?;
    let mut client = WhisperTranscriberClient::new(channel);
    let response = client
        .transcribe(tonic::Request::new(pcm_chunk_stream(pcm)))
        .await
        .context("gRPC Transcribe RPC failed")?
        .into_inner();
    play_response_stream(response, pulse_sink).await
}

fn pcm_chunk_stream(pcm: Vec<u8>) -> impl tokio_stream::Stream<Item = AudioChunk> {
    const CHUNK: usize = 3200;
    async_stream::stream! {
        for chunk in pcm.chunks(CHUNK) {
            if chunk.is_empty() {
                continue;
            }
            yield AudioChunk {
                pcm_s16le: chunk.to_vec(),
                end_of_stream: false,
            };
        }
        yield AudioChunk {
            pcm_s16le: Vec::new(),
            end_of_stream: true,
        };
    }
}

/// Open a live gRPC stream and forward PCM chunks as they arrive from the mic.
pub async fn transcribe_live(
    grpc_addr: &str,
    pcm_rx: mpsc::Receiver<Vec<u8>>,
    pulse_sink: Option<&str>,
) -> Result<GrpcSessionResult> {
    match transcribe_live_once(grpc_addr, pcm_rx, pulse_sink).await {
        Ok(result) => Ok(result),
        Err(e) => {
            invalidate_channel(grpc_addr);
            Err(e)
        }
    }
}

async fn transcribe_live_once(
    grpc_addr: &str,
    pcm_rx: mpsc::Receiver<Vec<u8>>,
    pulse_sink: Option<&str>,
) -> Result<GrpcSessionResult> {
    let channel = connect(grpc_addr).await?;
    let mut client = WhisperTranscriberClient::new(channel);
    let response = client
        .transcribe(tonic::Request::new(live_pcm_stream(pcm_rx)))
        .await
        .context("gRPC Transcribe RPC failed")?
        .into_inner();
    play_response_stream(response, pulse_sink).await
}

fn invalidate_channel(addr: &str) {
    let uri = normalize_grpc_uri(addr);
    if let Ok(mut guard) = GRPC_CHANNEL.lock() {
        if guard
            .as_ref()
            .is_some_and(|(cached_uri, _)| cached_uri == &uri)
        {
            guard.take();
        }
    }
}

async fn connect(addr: &str) -> Result<Channel> {
    let uri = normalize_grpc_uri(addr);
    if let Ok(guard) = GRPC_CHANNEL.lock() {
        if let Some((cached_uri, channel)) = guard.as_ref() {
            if cached_uri == &uri {
                return Ok(channel.clone());
            }
        }
    }

    let channel = Endpoint::from_shared(uri.clone())?
        .connect_timeout(CONNECT_TIMEOUT)
        .timeout(REQUEST_TIMEOUT)
        .connect()
        .await
        .context("connect to whisper gRPC service")?;

    if let Ok(mut guard) = GRPC_CHANNEL.lock() {
        *guard = Some((uri, channel.clone()));
    }
    Ok(channel)
}

/// Pre-connect so the first button press does not pay TCP/TLS setup latency.
pub async fn warm_connect(addr: &str) -> Result<()> {
    connect(addr).await.map(|_| ())
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
fn collect_speech_replies(replies: &[SpeechReply]) -> (String, Vec<u8>, u32) {
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

fn live_pcm_stream(
    mut pcm_rx: mpsc::Receiver<Vec<u8>>,
) -> impl tokio_stream::Stream<Item = AudioChunk> {
    async_stream::stream! {
        let mut total_bytes = 0usize;
        while let Some(chunk) = pcm_rx.recv().await {
            if chunk.is_empty() {
                continue;
            }
            total_bytes += chunk.len();
            yield AudioChunk {
                pcm_s16le: chunk,
                end_of_stream: false,
            };
        }
        eprintln!("gRPC stream finished: {total_bytes} bytes sent to whisper");
        yield AudioChunk {
            pcm_s16le: Vec::new(),
            end_of_stream: true,
        };
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

async fn play_pcm_buffer(pcm: &[u8], sample_rate: u32, pulse_sink: Option<&str>) -> Result<()> {
    tracing::info!(
        bytes = pcm.len(),
        sample_rate,
        ?pulse_sink,
        "playing response audio"
    );

    let stop = Arc::new(AtomicBool::new(false));
    if let Ok(mut guard) = PLAYBACK_STOP.lock() {
        *guard = Some(stop.clone());
    }

    let handle = crate::audio::spawn_playback(pcm.to_vec(), sample_rate, pulse_sink, stop)?;
    tokio::time::sleep(crate::audio::playback_warmup()).await;

    match tokio::time::timeout(Duration::from_secs(30), handle).await {
        Ok(Ok(())) => {}
        Ok(Err(e)) => tracing::warn!("playback task failed: {e}"),
        Err(_) => tracing::warn!("playback did not finish in time"),
    }

    if let Ok(mut guard) = PLAYBACK_STOP.lock() {
        guard.take();
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
