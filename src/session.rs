//! Async event loop: HFP events, transcription completions, and pipeline I/O.

use anyhow::Result;
use futures_util::StreamExt;
use std::pin::Pin;
use std::sync::Arc;
use tokio::sync::mpsc;

use crate::bridge::{
    apply_event, incoming_to_event, remap_headset_button, request_stop_before_capture, LoopAction,
    LoopState,
};
use crate::core::{Event, State};
use crate::dbus_hfp::{self, HfpBackend};
use crate::grpc_transcribe;
use crate::pipeline::{CaptureHandle, Config};
use crate::audio::PreRollRecorder;

struct TranscriptCompletion {
    generation: u64,
    event: Event,
}

enum Incoming {
    Hfp(Event),
    Transcript(TranscriptCompletion),
}

pub async fn run(
    backend: &HfpBackend,
    cfg: &Config,
    mut hfp_events: Pin<Box<dyn futures_util::Stream<Item = Event> + Send>>,
) -> Result<()> {
    let (transcript_tx, mut transcript_rx) = mpsc::channel::<TranscriptCompletion>(4);
    if let Some(addr) = &cfg.whisper_grpc_addr {
        let addr = addr.clone();
        tokio::spawn(async move {
            if let Err(e) = grpc_transcribe::warm_connect(&addr).await {
                tracing::warn!(%addr, "gRPC warm connect failed: {e:#}");
            } else {
                tracing::info!(%addr, "gRPC channel warm");
            }
        });
    }
    let ring_ms = crate::pipeline::env_ms("PREROLL_RING_MS", 4000);
    let preroll = crate::audio::PreRollRecorder::spawn(&cfg.pulse_source, ring_ms)?;
    eprintln!("pre-roll buffer ready ({ring_ms}ms ring)");
    let warm_cfg = cfg.clone();
    tokio::spawn(async move {
        if let Err(e) = crate::pipeline::warm_sco(&warm_cfg).await {
            eprintln!("SCO warm-up failed: {e:#}");
        } else {
            eprintln!("SCO warm-up complete (mic kept ready for pre-roll)");
        }
    });

    let mut session = Session {
        loop_state: LoopState {
            core: State::Idle,
            stop_pending: false,
            transcribe_gen: 0,
        },
        capture: None,
        cfg: cfg.clone(),
        transcript_tx,
        use_btmon: matches!(backend, HfpBackend::Btmon),
        preroll,
    };

    loop {
        let incoming = tokio::select! {
            ev = hfp_events.next() => match ev {
                Some(event) => Incoming::Hfp(event),
                None => {
                    tracing::warn!("HFP event stream ended");
                    break;
                }
            },
            done = transcript_rx.recv() => match done {
                Some(completion) => Incoming::Transcript(completion),
                None => break,
            },
        };

        if !session.handle_incoming(backend, incoming).await? {
            continue;
        }
    }

    Ok(())
}

struct Session {
    loop_state: LoopState,
    capture: Option<CaptureHandle>,
    cfg: Config,
    transcript_tx: mpsc::Sender<TranscriptCompletion>,
    use_btmon: bool,
    preroll: Arc<PreRollRecorder>,
}

impl Session {
    /// Returns false when the event was dropped (stale transcription).
    async fn handle_incoming(&mut self, backend: &HfpBackend, incoming: Incoming) -> Result<bool> {
        let event = match incoming {
            Incoming::Hfp(event) => event,
            Incoming::Transcript(completion) => {
                match incoming_to_event(
                    completion.generation,
                    completion.event,
                    self.loop_state.transcribe_gen,
                ) {
                    Some(event) => event,
                    None => {
                        tracing::info!("ignoring stale transcription result");
                        return Ok(false);
                    }
                }
            }
        };

        let event = remap_headset_button(self.use_btmon, &self.loop_state.core, event);
        if matches!((&self.loop_state.core, &event), (State::Idle, Event::RecognitionStarted)) {
            eprintln!("button press — starting capture");
        }
        if matches!(
            (&self.loop_state.core, &event),
            (State::Listening, Event::RecognitionStarted)
        ) {
            eprintln!("button press while recording — stopping capture");
        }
        if matches!(
            (&self.loop_state.core, &event),
            (State::Listening, Event::RecognitionStopped)
        ) {
            eprintln!("button release — stopping capture");
        }

        tracing::info!(state = ?self.loop_state.core, ?event, "event");
        let (next, actions) = apply_event(&self.loop_state, event);
        self.loop_state = next;
        self.run_actions(backend, actions).await?;
        Ok(true)
    }

    async fn run_actions(&mut self, backend: &HfpBackend, actions: Vec<LoopAction>) -> Result<()> {
        for action in actions {
            match action {
                LoopAction::AbortPlayback => {
                    grpc_transcribe::abort_playback().await;
                    tracing::info!("button pressed during transcription — barge-in");
                }
                LoopAction::StartCapture => {
                    self.capture =
                        Some(crate::pipeline::start_capture(&self.cfg, &self.preroll).await?);
                }
                LoopAction::StopCaptureAndTranscribe => {
                    if let Some(handle) = self.capture.take() {
                        spawn_transcribe(
                            &self.cfg,
                            handle,
                            self.loop_state.transcribe_gen,
                            self.transcript_tx.clone(),
                            self.preroll.clone(),
                        );
                    } else {
                        tracing::info!(
                            "stop requested before capture started — will stop once capture begins"
                        );
                        self.loop_state = request_stop_before_capture(&self.loop_state);
                    }
                }
                LoopAction::ClearRecognitionFlag => {
                    if let Err(e) = dbus_hfp::clear_voice_recognition(backend).await {
                        tracing::warn!("failed to clear VoiceRecognition flag: {e}");
                    }
                }
            }
        }
        Ok(())
    }
}

fn spawn_transcribe(
    cfg: &Config,
    handle: CaptureHandle,
    generation: u64,
    tx: mpsc::Sender<TranscriptCompletion>,
    preroll: Arc<PreRollRecorder>,
) {
    let cfg = cfg.clone();
    tokio::spawn(async move {
        let event = crate::pipeline::stop_capture_and_transcribe(&cfg, handle, &preroll).await;
        if tx
            .send(TranscriptCompletion { generation, event })
            .await
            .is_err()
        {
            tracing::warn!("transcription completed after bridge shut down");
        }
    });
}
