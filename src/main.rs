mod core;
mod dbus_hfp;
mod grpc_transcribe;
mod pipeline;

use anyhow::Result;
use futures_util::StreamExt;

use crate::core::{transition, Effect, State};
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

    let mut events = dbus_hfp::recognition_events(&backend, &cfg.pulse_source).await?;

    let mut state = State::Idle;
    let mut capture: Option<CaptureHandle> = None;
    let mut stop_pending = false;

    while let Some(event) = events.next().await {
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
                            let ev = pipeline::stop_capture_and_transcribe(&cfg, handle).await;
                            let (s, fx) = transition(&state, ev);
                            state = s;
                            pending.extend(fx);
                        }
                    }
                }
                Effect::StopCaptureAndTranscribe => {
                    if let Some(handle) = capture.take() {
                        stop_pending = false;
                        let ev = pipeline::stop_capture_and_transcribe(&cfg, handle).await;
                        let (s, fx) = transition(&state, ev);
                        state = s;
                        pending.extend(fx);
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

trait ContextOr<T> {
    fn context_or(self, msg: &str) -> Result<T>;
}
impl<T> ContextOr<T> for std::result::Result<T, std::env::VarError> {
    fn context_or(self, msg: &str) -> Result<T> {
        self.map_err(|_| anyhow::anyhow!(msg.to_string()))
    }
}
