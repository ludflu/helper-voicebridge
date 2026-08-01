//! Bluetooth edge: watches for the QuietComfort play/pause button.
//!
//! On iPhone the same physical button opens Siri: the headset sends HFP
//! `AT+BVRA=1` while held and the phone opens a SCO mic session. On Linux,
//! PipeWire often answers `+BRSF: 3680` **without** the voice-recognition bit,
//! so the headset falls back to **AVRCP play/pause (0x44)** — the hex you saw
//! in btmon (`48 7c 44 00`). We watch both:
//!
//! - `AT+BVRA=1/0` when the stack advertises voice recognition (ideal)
//! - `AT+CHUP` on release (Bose QC ends voice rec with hang-up, not BVRA=0)
//! - AVRCP play press/release (what the button sends to the Pi today)
//!
//! Backend priority (`HFP_BACKEND=auto`): oFono → btmon → pulse (mic RUNNING).

use anyhow::{Context, Result};
use futures_util::stream::StreamExt;
use std::pin::Pin;
use std::sync::Mutex;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::task::JoinHandle;
use zbus::{proxy, zvariant::OwnedObjectPath, Connection};

use crate::core::Event;
use crate::pipeline::env_ms;

#[derive(Clone, Debug)]
pub enum HfpBackend {
    Ofono {
        conn: Connection,
        modem_path: OwnedObjectPath,
    },
    /// Mic source goes RUNNING when HFP SCO is open (hold-to-talk ground truth).
    Pulse { source: String },
    /// Parse btmon for AT+BVRA and AVRCP play/pause (QC button on Linux).
    Btmon,
}

#[proxy(
    default_service = "org.ofono",
    default_path = "/",
    interface = "org.ofono.Manager"
)]
trait Manager {
    fn get_modems(
        &self,
    ) -> zbus::Result<
        Vec<(
            OwnedObjectPath,
            std::collections::HashMap<String, zbus::zvariant::OwnedValue>,
        )>,
    >;
}

#[proxy(default_service = "org.ofono", interface = "org.ofono.Handsfree")]
trait Handsfree {
    #[zbus(property)]
    fn voice_recognition(&self) -> zbus::Result<bool>;

    #[zbus(property)]
    fn set_voice_recognition(&self, value: bool) -> zbus::Result<()>;
}

/// Open the best available HFP event source for this host.
pub async fn open_backend(name_hint: &str, pulse_source: &str) -> Result<HfpBackend> {
    let prefer = std::env::var("HFP_BACKEND").unwrap_or_else(|_| "auto".into());

    let try_ofono = prefer == "auto" || prefer == "ofono";
    let try_pulse = prefer == "auto" || prefer == "pulse";
    let try_btmon = prefer == "auto" || prefer == "btmon";

    if try_ofono {
        if let Ok(conn) = Connection::system().await {
            if let Ok(path) = find_headset_modem(&conn, name_hint).await {
                tracing::info!(?path, "using oFono VoiceRecognition backend");
                return Ok(HfpBackend::Ofono {
                    conn,
                    modem_path: path,
                });
            }
        }
        if prefer == "ofono" {
            anyhow::bail!("HFP_BACKEND=ofono but no org.ofono.Handsfree modem found");
        }
        tracing::warn!("oFono Handsfree modem not found");
    }

    if try_btmon {
        if btmon_available().await {
            tracing::info!("using btmon backend (AT+BVRA + AVRCP play/pause 0x44 for QC button)");
            return Ok(HfpBackend::Btmon);
        }
        if prefer == "btmon" {
            anyhow::bail!("HFP_BACKEND=btmon but btmon is not installed");
        }
        tracing::warn!("btmon not available");
    }

    if try_pulse {
        if pulse_source_available(pulse_source).await {
            tracing::info!(
                source = pulse_source,
                "using Pulse mic-source backend (SCO RUNNING); needs AT+BVRA from headset"
            );
            return Ok(HfpBackend::Pulse {
                source: pulse_source.into(),
            });
        }
        if prefer == "pulse" {
            anyhow::bail!("HFP_BACKEND=pulse but source {pulse_source} not found");
        }
        tracing::warn!(source = pulse_source, "Pulse source not found");
    }

    anyhow::bail!("no HFP backend available (set HFP_BACKEND=ofono|btmon|pulse|auto)");
}

async fn pulse_source_available(source: &str) -> bool {
    let output = Command::new("pactl")
        .args(["list", "sources", "short"])
        .output()
        .await;
    match output {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout)
            .lines()
            .any(|l| l.contains(source)),
        _ => false,
    }
}

async fn find_headset_modem(conn: &Connection, name_hint: &str) -> Result<OwnedObjectPath> {
    let mgr = ManagerProxy::new(conn).await?;
    let modems = mgr
        .get_modems()
        .await
        .context("org.ofono.Manager.GetModems failed")?;

    for (path, props) in modems {
        let looks_like_name_match = name_hint.is_empty()
            || props
                .get("Name")
                .and_then(|v| v.downcast_ref::<&str>().ok())
                .map(|n| n.to_lowercase().contains(&name_hint.to_lowercase()))
                .unwrap_or(false);

        if looks_like_name_match
            && HandsfreeProxy::builder(conn)
                .path(path.clone())?
                .build()
                .await
                .is_ok()
        {
            return Ok(path);
        }
    }

    anyhow::bail!(
        "no oFono modem exposing org.ofono.Handsfree found (is the headset connected via HFP, not just A2DP?)"
    )
}

pub async fn recognition_events(
    backend: &HfpBackend,
    pulse_source: &str,
) -> Result<Pin<Box<dyn futures_util::Stream<Item = Event> + Send>>> {
    match backend {
        HfpBackend::Ofono { conn, modem_path } => recognition_events_ofono(conn, modem_path).await,
        HfpBackend::Pulse { source } => recognition_events_pulse(source).await,
        HfpBackend::Btmon => recognition_events_btmon(pulse_source).await,
    }
}

async fn recognition_events_ofono(
    conn: &Connection,
    modem_path: &OwnedObjectPath,
) -> Result<Pin<Box<dyn futures_util::Stream<Item = Event> + Send>>> {
    let proxy = HandsfreeProxy::builder(conn)
        .path(modem_path.clone())?
        .build()
        .await
        .context("failed to bind org.ofono.Handsfree proxy")?;

    let changes = proxy.receive_voice_recognition_changed().await;

    Ok(changes
        .filter_map(|change| async move {
            match change.get().await {
                Ok(true) => Some(Event::RecognitionStarted),
                Ok(false) => Some(Event::RecognitionStopped),
                Err(e) => {
                    tracing::warn!("failed to read VoiceRecognition change: {e}");
                    None
                }
            }
        })
        .boxed())
}

async fn pulse_source_running(source: &str) -> bool {
    let output = Command::new("pactl")
        .args(["list", "sources", "short"])
        .output()
        .await;
    let Ok(o) = output else {
        return false;
    };
    if !o.status.success() {
        return false;
    }
    String::from_utf8_lossy(&o.stdout).lines().any(|line| {
        let mut parts = line.split_whitespace();
        parts.next(); // index
        match (parts.next(), parts.last()) {
            (Some(name), Some(state)) => name == source && state == "RUNNING",
            _ => false,
        }
    })
}

async fn recognition_events_pulse(
    source: &str,
) -> Result<Pin<Box<dyn futures_util::Stream<Item = Event> + Send>>> {
    let source = source.to_string();
    let mut child = Command::new("pactl")
        .arg("subscribe")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .context("failed to spawn pactl subscribe")?;

    let stdout = child
        .stdout
        .take()
        .context("pactl subscribe stdout unavailable")?;

    let (tx, rx) = tokio::sync::mpsc::channel::<Event>(32);
    let watch = source.clone();
    tokio::spawn(async move {
        let mut was_running = pulse_source_running(&watch).await;
        tracing::debug!(was_running, "pulse mic initial state");

        let mut lines = BufReader::new(stdout).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            if !line.contains("source") {
                continue;
            }
            let running = pulse_source_running(&watch).await;
            if running && !was_running {
                tracing::info!("pulse mic RUNNING — voice session started");
                let _ = emit_bt_event(&tx, Event::RecognitionStarted).await;
            } else if !running && was_running {
                tracing::info!("pulse mic SUSPENDED — voice session ended");
                let _ = emit_bt_event(&tx, Event::RecognitionStopped).await;
            }
            was_running = running;
        }
        let _ = child.wait().await;
    });

    Ok(futures_util::stream::unfold(
        rx,
        |mut rx| async move { rx.recv().await.map(|ev| (ev, rx)) },
    )
    .boxed())
}

async fn btmon_available() -> bool {
    Command::new("btmon")
        .arg("-h")
        .output()
        .await
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Strip spaces and lowercase for hex substring checks.
fn hex_compact(line: &str) -> String {
    line.chars()
        .filter(|c| c.is_ascii_hexdigit())
        .collect::<String>()
        .to_ascii_lowercase()
}

/// HFP voice recognition: headset sends `AT+BVRA=1/0` over RFCOMM; the AG
/// (PipeWire) may reply with `+BVRA:0` on release. btmon may show plain text
/// and/or hex ACL payloads — do not match `+BVRA:1` (AG echo / SCO noise).
fn parse_at_bvra(line: &str) -> Option<Event> {
    let upper = line.to_ascii_uppercase();
    if upper.contains("AT+BVRA=1") || upper.contains("AT+BVRA?1") {
        return Some(Event::RecognitionStarted);
    }
    if upper.contains("AT+BVRA=0") || upper.contains("AT+BVRA?0") || upper.contains("+BVRA:0") {
        return Some(Event::RecognitionStopped);
    }

    let compact = hex_compact(line);
    // HF -> AG: "AT+BVRA=1" / "AT+BVRA=0". AG -> HF stop: "+BVRA:0"
    if compact.contains("41542b425652413d31") {
        return Some(Event::RecognitionStarted);
    }
    if compact.contains("41542b425652413d30") || compact.contains("2b425652413a30") {
        return Some(Event::RecognitionStopped);
    }
    None
}

/// Bose QC ends voice recognition with `AT+CHUP` (hang-up) on button release,
/// not `AT+BVRA=0`. PipeWire stock handler tries to hang up a call and replies ERROR.
fn parse_at_chup(line: &str) -> Option<Event> {
    let upper = line.to_ascii_uppercase();
    if upper.contains("AT+CHUP") {
        return Some(Event::RecognitionStopped);
    }

    let compact = hex_compact(line);
    if compact.contains("41542b43485550") {
        return Some(Event::RecognitionStopped);
    }
    None
}

/// AVRCP pass-through **play/pause** (0x44) from the QC multi-function button.
/// On Linux this is what the button sends when the Pi does not advertise HFP
/// voice recognition in +BRSF (unlike iPhone).
fn parse_avrcp_play(line: &str) -> Option<Event> {
    if !line.to_ascii_lowercase().contains("0e 00 48 7c") {
        return None;
    }
    let hex = line.to_ascii_lowercase();
    if hex.contains("7c 44 00") {
        return Some(Event::RecognitionStarted);
    }
    if hex.contains("7c c4 00") {
        return Some(Event::RecognitionStopped);
    }
    None
}

#[derive(Debug, PartialEq, Eq)]
enum BtmonSignal {
    AtBvra,
    AtChup,
    AvrcpPlay,
}

fn avrcp_play_enabled() -> bool {
    std::env::var("BT_AVRCP_PLAY").ok().as_deref() != Some("0")
}

fn classify_btmon_line(line: &str) -> Option<(Event, BtmonSignal)> {
    if let Some(ev) = parse_at_bvra(line) {
        return Some((ev, BtmonSignal::AtBvra));
    }
    if let Some(ev) = parse_at_chup(line) {
        return Some((ev, BtmonSignal::AtChup));
    }
    if avrcp_play_enabled() {
        if let Some(ev) = parse_avrcp_play(line) {
            return Some((ev, BtmonSignal::AvrcpPlay));
        }
    }
    None
}

static CAPTURE_WATCHDOG: Mutex<Option<JoinHandle<()>>> = Mutex::new(None);

fn schedule_capture_watchdog(tx: tokio::sync::mpsc::Sender<Event>) {
    let max_ms = env_ms("MAX_CAPTURE_MS", 30_000);
    let mut guard = CAPTURE_WATCHDOG.lock().expect("watchdog lock");
    if let Some(handle) = guard.take() {
        handle.abort();
    }
    *guard = Some(tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(max_ms)).await;
        tracing::info!(max_ms, "capture watchdog — stopping session");
        let _ = emit_bt_event(&tx, Event::RecognitionStopped).await;
    }));
}

fn cancel_capture_watchdog() {
    let mut guard = CAPTURE_WATCHDOG.lock().expect("watchdog lock");
    if let Some(handle) = guard.take() {
        handle.abort();
    }
}

async fn emit_bt_event(tx: &tokio::sync::mpsc::Sender<Event>, ev: Event) -> Result<(), ()> {
    match &ev {
        Event::RecognitionStarted => schedule_capture_watchdog(tx.clone()),
        Event::RecognitionStopped => {
            cancel_capture_watchdog();
            eprintln!("headset signal: stop");
        }
        _ => {}
    }
    tx.send(ev).await.map_err(|_| ())
}

async fn recognition_events_btmon(
    pulse_source: &str,
) -> Result<Pin<Box<dyn futures_util::Stream<Item = Event> + Send>>> {
    // Merge btmon (start + stop) with Pulse mic idle (stop only). Avoid
    // stream::select on two boxed streams — it can stop delivering events.
    let (tx, rx) = tokio::sync::mpsc::channel::<Event>(64);

    spawn_btmon_event_source(tx.clone());
    spawn_pulse_mic_stop_source(pulse_source, tx);

    Ok(futures_util::stream::unfold(
        rx,
        |mut rx| async move { rx.recv().await.map(|ev| (ev, rx)) },
    )
    .boxed())
}

fn spawn_btmon_event_source(tx: tokio::sync::mpsc::Sender<Event>) {
    tokio::spawn(async move {
        // Orphan btmon processes survive service restarts and duplicate button events.
        let _ = Command::new("pkill")
            .args(["-x", "btmon"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .await;
        tokio::time::sleep(Duration::from_millis(100)).await;

        let iface = std::env::var("BTMON_INTERFACE").unwrap_or_else(|_| "hci0".into());
        let mut child = match Command::new("btmon")
            .args(["-i", &iface])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true)
            .spawn()
        {
            Ok(c) => c,
            Err(e) => {
                tracing::error!("failed to spawn btmon: {e:#}");
                return;
            }
        };

        let stdout = match child.stdout.take() {
            Some(s) => s,
            None => {
                tracing::error!("btmon stdout unavailable");
                return;
            }
        };

        let mut lines = BufReader::new(stdout).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            if let Some((ev, signal)) = classify_btmon_line(&line) {
                match signal {
                    BtmonSignal::AtBvra => {
                        tracing::info!(?ev, "btmon HFP AT+BVRA");
                    }
                    BtmonSignal::AtChup => {
                        tracing::info!(?ev, "btmon HFP AT+CHUP (Bose QC release)");
                    }
                    BtmonSignal::AvrcpPlay => {
                        tracing::info!(
                            ?ev,
                            "btmon AVRCP play/pause (QC button on Linux; iPhone uses AT+BVRA)"
                        );
                    }
                }
                if emit_bt_event(&tx, ev).await.is_err() {
                    break;
                }
            }
        }
        let _ = child.wait().await;
        tracing::warn!("btmon event source exited");
    });
}

fn spawn_pulse_mic_stop_source(source: &str, tx: tokio::sync::mpsc::Sender<Event>) {
    let source = source.to_string();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_millis(150));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut was_running = pulse_source_running(&source).await;
        loop {
            interval.tick().await;
            let running = pulse_source_running(&source).await;
            if !running && was_running {
                tracing::info!("pulse mic SUSPENDED — voice session ended");
                if emit_bt_event(&tx, Event::RecognitionStopped).await.is_err() {
                    break;
                }
            }
            was_running = running;
        }
    });
}

pub async fn clear_voice_recognition(backend: &HfpBackend) -> Result<()> {
    match backend {
        HfpBackend::Ofono { conn, modem_path } => {
            let proxy = HandsfreeProxy::builder(conn)
                .path(modem_path.clone())?
                .build()
                .await?;
            proxy.set_voice_recognition(false).await?;
        }
        HfpBackend::Pulse { .. } | HfpBackend::Btmon => {}
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn at_bvra_plain_text() {
        assert_eq!(
            parse_at_bvra("      AT+BVRA=1"),
            Some(Event::RecognitionStarted)
        );
        assert_eq!(
            parse_at_bvra("      AT+BVRA=0"),
            Some(Event::RecognitionStopped)
        );
    }

    #[test]
    fn at_bvra_rfccomm_hex() {
        assert_eq!(
            parse_at_bvra("  41 54 2b 42 56 52 41 3d 31 0d"),
            Some(Event::RecognitionStarted)
        );
        assert_eq!(
            parse_at_bvra("  41542b425652413d30"),
            Some(Event::RecognitionStopped)
        );
        assert_eq!(
            parse_at_bvra("      +BVRA:0"),
            Some(Event::RecognitionStopped)
        );
        assert_eq!(
            parse_at_bvra("  2b 42 56 52 41 3a 30 0d"),
            Some(Event::RecognitionStopped)
        );
    }

    #[test]
    fn at_chup_is_release() {
        assert_eq!(
            parse_at_chup("        41 54 2b 43 48 55 50 0d e4                       AT+CHUP.."),
            Some(Event::RecognitionStopped)
        );
        assert_eq!(
            parse_at_chup("      AT+CHUP"),
            Some(Event::RecognitionStopped)
        );
    }

    #[test]
    fn avrcp_play_is_default_for_qc_button() {
        assert_eq!(
            parse_avrcp_play("        90 11 0e 00 48 7c 44 00"),
            Some(Event::RecognitionStarted)
        );
    }

    #[test]
    fn ignores_avrcp_tx_echo() {
        assert_eq!(parse_avrcp_play("        92 11 0e 09 48 7c 44 00"), None);
    }
}
