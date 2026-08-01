//! In-process PipeWire capture (replaces `parecord` for the pre-roll ring).

use anyhow::{Context, Result};
use pipewire as pw;
use pw::{properties::properties, spa};
use spa::param::format::{MediaSubtype, MediaType};
use spa::param::format_utils;
use spa::pod::Pod;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

use super::capture_tap::CaptureTap;

const SAMPLE_RATE: u32 = 16_000;
const CHANNELS: u32 = 1;

struct CaptureState {
    format: spa::param::audio::AudioInfoRaw,
    tap: Arc<CaptureTap>,
    stop: Arc<AtomicBool>,
    mainloop: pw::main_loop::MainLoopRc,
}

/// Block the current thread running a PipeWire main loop until `stop` is set.
pub fn capture_to_tap(
    source: &str,
    tap: Arc<CaptureTap>,
    stop: Arc<AtomicBool>,
) -> Result<()> {
    pw::init();

    let mainloop = pw::main_loop::MainLoopRc::new(None).context("pipewire MainLoop")?;
    let context = pw::context::ContextRc::new(&mainloop, None).context("pipewire Context")?;
    let core = context.connect_rc(None).context("pipewire connect")?;

    let mut props = properties! {
        *pw::keys::MEDIA_TYPE => "Audio",
        *pw::keys::MEDIA_CATEGORY => "Capture",
        *pw::keys::MEDIA_ROLE => "Communication",
    };
    props.insert(*pw::keys::TARGET_OBJECT, source.to_string());

    let stream =
        pw::stream::StreamBox::new(&core, "qc-voice-preroll", props).context("pipewire Stream")?;

    let state = CaptureState {
        format: Default::default(),
        tap,
        stop: stop.clone(),
        mainloop: mainloop.clone(),
    };

    let _listener = stream
        .add_local_listener_with_user_data(state)
        .param_changed(|_, state, id, param| {
            let Some(param) = param else {
                return;
            };
            if id != pw::spa::param::ParamType::Format.as_raw() {
                return;
            }
            let (media_type, media_subtype) = match format_utils::parse_format(param) {
                Ok(v) => v,
                Err(_) => return,
            };
            if media_type != MediaType::Audio || media_subtype != MediaSubtype::Raw {
                return;
            }
            if state.format.parse(param).is_err() {
                return;
            }
            eprintln!(
                "pipewire capture format: {} Hz, {} ch",
                state.format.rate(),
                state.format.channels()
            );
        })
        .process(|stream, state| {
            if state.stop.load(Ordering::Relaxed) {
                state.mainloop.quit();
                return;
            }
            let Some(mut buffer) = stream.dequeue_buffer() else {
                return;
            };
            let datas = buffer.datas_mut();
            if datas.is_empty() {
                return;
            }
            let data = &mut datas[0];
            let size = data.chunk().size() as usize;
            let Some(samples) = data.data() else {
                return;
            };
            if size == 0 || size > samples.len() {
                return;
            }
            state.tap.push(&samples[..size]);
        })
        .register()
        .context("pipewire stream listener")?;

    let mut audio_info = spa::param::audio::AudioInfoRaw::new();
    audio_info.set_format(spa::param::audio::AudioFormat::S16LE);
    audio_info.set_rate(SAMPLE_RATE);
    audio_info.set_channels(CHANNELS);

    let obj = pw::spa::pod::Object {
        type_: pw::spa::utils::SpaTypes::ObjectParamFormat.as_raw(),
        id: pw::spa::param::ParamType::EnumFormat.as_raw(),
        properties: audio_info.into(),
    };
    let values: Vec<u8> = pw::spa::pod::serialize::PodSerializer::serialize(
        std::io::Cursor::new(Vec::new()),
        &pw::spa::pod::Value::Object(obj),
    )
    .context("serialize pipewire format pod")?
    .0
    .into_inner();

    let mut params = [Pod::from_bytes(&values).context("pipewire format Pod")?];

    stream
        .connect(
            spa::utils::Direction::Input,
            None,
            pw::stream::StreamFlags::AUTOCONNECT
                | pw::stream::StreamFlags::MAP_BUFFERS
                | pw::stream::StreamFlags::RT_PROCESS,
            &mut params,
        )
        .context("pipewire stream connect")?;

    eprintln!("pipewire capture running on {source}");
    mainloop.run();
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn bytes_per_ms_at_16k() {
        assert_eq!(16_000 * 1 * 2 / 1000, 32);
    }
}
