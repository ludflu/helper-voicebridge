use anyhow::{Context, Result};
use libpulse_binding as pulse;
use libpulse_binding::context::{Context, FlagSet as ContextFlagSet};
use libpulse_binding::mainloop::standard::Mainloop;
use libpulse_simple_binding as psimple;
use pulse::sample::{Format, Spec};
use pulse::stream::Direction;
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::time::Duration;
use tokio::sync::mpsc;

const SAMPLE_RATE: u32 = 16_000;
const CHANNELS: u8 = 1;
/// ~100 ms of mono s16le @ 16 kHz.
const READ_BYTES: usize = (SAMPLE_RATE as usize * 2 * CHANNELS as usize) / 10;

pub fn playback_warmup() -> Duration {
    Duration::from_millis(400)
}

pub fn unsuspend_source(source: &str) -> Result<()> {
    set_source_suspended(source, false).context("unsuspend pulse source")
}

fn set_source_suspended(source_name: &str, suspend: bool) -> Result<()> {
    let mainloop = Rc::new(RefCell::new(
        Mainloop::new().context("create pulse mainloop")?,
    ));
    let context = Rc::new(RefCell::new(
        Context::new(&mainloop, "qc-voice-bridge").context("create pulse context")?,
    ));
    let result = Rc::new(RefCell::new(None::<Result<()>>));

    {
        let ctx = context.clone();
        let result_cb = result.clone();
        let name = source_name.to_string();
        context
            .borrow_mut()
            .set_state_callback(Some(Box::new(move || {
                let state = ctx.borrow().get_state();
                if !state.1.is_empty() {
                    return;
                }
                match state.0 {
                    pulse::context::State::Ready => {
                        let result_op = result_cb.clone();
                        let mut context = ctx.borrow_mut();
                        if let Err(e) = context.suspend_source_by_name(&name, suspend, move |_| {
                            *result_op.borrow_mut() = Some(Ok(()))
                        }) {
                            *result_cb.borrow_mut() =
                                Some(Err(anyhow::anyhow!("suspend_source_by_name failed: {e}")));
                        }
                    }
                    pulse::context::State::Failed | pulse::context::State::Terminated => {
                        *result_cb.borrow_mut() =
                            Some(Err(anyhow::anyhow!("pulse context failed to connect")));
                    }
                    _ => {}
                }
            })));
    }

    context
        .borrow_mut()
        .connect(None, ContextFlagSet::NOFLAGS, None)
        .context("connect pulse context")?;

    while result.borrow().is_none() {
        mainloop
            .borrow_mut()
            .iterate(true)
            .context("pulse mainloop iterate")?;
    }

    result
        .borrow_mut()
        .take()
        .unwrap_or_else(|| Err(anyhow::anyhow!("pulse suspend never completed")))
}

pub fn spawn_recorder(
    source: &str,
    chunk_tx: mpsc::Sender<Vec<u8>>,
    stop: Arc<AtomicBool>,
) -> Result<tokio::task::JoinHandle<()>> {
    let source = source.to_string();
    Ok(tokio::task::spawn_blocking(move || {
        if let Err(e) = record_loop(&source, chunk_tx, stop) {
            tracing::warn!("pulse recorder exited: {e:#}");
        }
    }))
}

fn record_loop(source: &str, chunk_tx: mpsc::Sender<Vec<u8>>, stop: Arc<AtomicBool>) -> Result<()> {
    let spec = pcm_spec();
    let simple = psimple::Simple::new(
        None,
        "qc-voice-bridge",
        Direction::Record,
        Some(source),
        "voice capture",
        &spec,
        None,
        None,
    )
    .map_err(|e| anyhow::anyhow!("open pulse record stream on {source}: {e}"))?;

    tracing::info!(source, "pulse recorder open (live PCM, no file)");

    let mut buf = vec![0u8; READ_BYTES];
    while !stop.load(Ordering::Relaxed) {
        simple
            .read(&mut buf)
            .map_err(|e| anyhow::anyhow!("pulse read failed: {e}"))?;
        if stop.load(Ordering::Relaxed) {
            break;
        }
        if chunk_tx.blocking_send(buf.clone()).is_err() {
            tracing::debug!("pcm consumer dropped");
            break;
        }
    }
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
        format: Format::S16LE,
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

fn pcm_spec() -> Spec {
    Spec {
        format: Format::S16LE,
        channels: CHANNELS,
        rate: SAMPLE_RATE,
    }
}
