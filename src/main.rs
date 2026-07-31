mod bridge;
mod core;
mod dbus_hfp;
mod grpc_transcribe;
mod pipeline;

use anyhow::Result;
use futures_util::StreamExt;
use tokio::sync::mpsc;

use crate::bridge::{accept_transcript, should_barge_in};
use crate::core::{transition, Effect, Event, State};
use crate::pipeline::{CaptureHandle, Config};

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();

    let pulse_source = std::env::var("PULSE_SOURCE")
        .context_or("PULSE_SOURCE env var not set (see `pactl list sources`)")?;

    let name_hint = std::env::var("HEADSET_NAME_HINT").unwrap_or_default();
    let backend = dbus_hfp::open_backend(&name_hint, &pulse_source).await?;
    tracing::info!(?backend, "HFP backend ready");

    let capture_sco_delay_ms = match &backend {
        dbus_hfp::HfpBackend::Pulse { .. } => 0,
        _ => pipeline::env_ms("CAPTURE_SCO_DELAY_MS", 400),
    };

    let cfg = Config {
        whisper_binary: std::env::var("WHISPER_BIN")
            .unwrap_or_else(|_| "/usr/local/bin/whisper-cli".into())
            .into(),
        whisper_model: std::env::var("WHISPER_MODEL")
            .unwrap_or_else(|_| "/opt/whisper/models/ggml-base.en.bin".into())
            .into(),
        pulse_source: pulse_source.clone(),
        pulse_sink: std::env::var("PULSE_SINK")
            .ok()
            .or_else(|| pipeline::pulse_sink_from_source(&pulse_source)),
        whisper_grpc_addr: std::env::var("WHISPER_GRPC_ADDR").ok(),
        capture_sco_delay_ms,
    };

    let mut hfp_events = dbus_hfp::recognition_events(&backend, &cfg.pulse_source).await?;
    let (transcript_tx, mut transcript_rx) = mpsc::channel::<TranscriptCompletion>(4);

    let mut state = State::Idle;
    let mut capture: Option<CaptureHandle> = None;
    let mut stop_pending = false;
    let mut transcribe_gen: u64 = 0;

    loop {
        let incoming = tokio::select! {
            ev = hfp_events.next() => {
                match ev {
                    Some(event) => Incoming::Hfp(event),
                    None => {
                        tracing::warn!("HFP event stream ended");
                        break;
                    }
                }
            }
            done = transcript_rx.recv() => {
                match done {
                    Some(completion) => Incoming::Transcript(completion),
                    None => break,
                }
            }
        };

        if matches!(
            &incoming,
            Incoming::Hfp(Event::RecognitionStarted) if should_barge_in(&state, &Event::RecognitionStarted)
        ) {
            transcribe_gen = transcribe_gen.wrapping_add(1);
            grpc_transcribe::abort_playback().await;
            tracing::info!("button pressed during transcription — barge-in");
        }

        let event = match incoming {
            Incoming::Hfp(event) => event,
            Incoming::Transcript(completion) => {
                if !accept_transcript(completion.generation, transcribe_gen) {
                    tracing::info!("ignoring stale transcription result");
                    continue;
                }
                completion.event
            }
        };

        tracing::info!(?state, ?event, "event");
        let (next_state, effects) = transition(&state, event);
        state = next_state;

        let mut pending = effects;
        while let Some(eff) = pending.pop() {
            match eff {
                Effect::StartCapture => {
                    capture = Some(pipeline::start_capture(&cfg).await?);
                    if stop_pending {
                        stop_pending = false;
                        if let Some(handle) = capture.take() {
                            spawn_transcribe(&cfg, handle, transcribe_gen, transcript_tx.clone());
                        }
                    }
                }
                Effect::StopCaptureAndTranscribe => {
                    if let Some(handle) = capture.take() {
                        stop_pending = false;
                        spawn_transcribe(&cfg, handle, transcribe_gen, transcript_tx.clone());
                    } else {
                        tracing::info!(
                            "stop requested before parecord started — will stop once capture begins"
                        );
                        stop_pending = true;
                    }
                }
                Effect::ClearRecognitionFlag => {
                    if let Err(e) = dbus_hfp::clear_voice_recognition(&backend).await {
                        tracing::warn!("failed to clear VoiceRecognition flag: {e}");
                    }
                }
            }
        }
    }

    Ok(())
}

struct TranscriptCompletion {
    generation: u64,
    event: Event,
}

enum Incoming {
    Hfp(Event),
    Transcript(TranscriptCompletion),
}

fn spawn_transcribe(
    cfg: &Config,
    handle: CaptureHandle,
    generation: u64,
    tx: mpsc::Sender<TranscriptCompletion>,
) {
    let cfg = cfg.clone();
    tokio::spawn(async move {
        let event = pipeline::stop_capture_and_transcribe(&cfg, handle).await;
        if tx
            .send(TranscriptCompletion { generation, event })
            .await
            .is_err()
        {
            tracing::warn!("transcription completed after bridge shut down");
        }
    });
}

trait ContextOr<T> {
    fn context_or(self, msg: &str) -> Result<T>;
}
impl<T> ContextOr<T> for std::result::Result<T, std::env::VarError> {
    fn context_or(self, msg: &str) -> Result<T> {
        self.map_err(|_| anyhow::anyhow!(msg.to_string()))
    }
}
