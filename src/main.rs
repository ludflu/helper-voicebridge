mod audio;
mod bridge;
mod core;
mod dbus_hfp;
mod grpc_transcribe;
mod pipeline;
mod session;

use anyhow::Result;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();

    let pulse_source = std::env::var("PULSE_SOURCE")
        .context_or("PULSE_SOURCE env var not set (see `pactl list sources`)")?;
    let name_hint = std::env::var("HEADSET_NAME_HINT").unwrap_or_default();
    let backend = dbus_hfp::open_backend(&name_hint, &pulse_source).await?;
    tracing::info!(?backend, "HFP backend ready");

    let cfg = pipeline::load_config(&pulse_source, &backend);
    let hfp_events = dbus_hfp::recognition_events(&backend, &cfg.pulse_source).await?;
    session::run(&backend, &cfg, hfp_events).await
}

trait ContextOr<T> {
    fn context_or(self, msg: &str) -> Result<T>;
}
impl<T> ContextOr<T> for std::result::Result<T, std::env::VarError> {
    fn context_or(self, msg: &str) -> Result<T> {
        self.map_err(|_| anyhow::anyhow!(msg.to_string()))
    }
}
