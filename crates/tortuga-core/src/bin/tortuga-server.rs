//! Headless Tortuga: engine + HTTP API (+ optional web UI).
//!
//! Usage: tortuga-server [--data-dir DIR] [--port PORT] [--ui-dir ui/dist] [--token TOKEN]

use std::{net::SocketAddr, path::PathBuf};

use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("info,librqbit=warn")),
        )
        .init();

    let mut data_dir = tortuga_core::default_data_dir();
    let mut port: u16 = 7979;
    let mut ui_dir: Option<PathBuf> = None;
    let mut token: Option<String> = None;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--data-dir" => data_dir = args.next().expect("--data-dir needs a value").into(),
            "--port" => port = args.next().expect("--port needs a value").parse()?,
            "--ui-dir" => ui_dir = Some(args.next().expect("--ui-dir needs a value").into()),
            "--token" => token = args.next(),
            "-h" | "--help" => {
                println!(
                    "tortuga-server [--data-dir DIR] [--port PORT] [--ui-dir DIR] [--token TOKEN]"
                );
                return Ok(());
            }
            other => anyhow::bail!("unknown argument {other}"),
        }
    }

    let engine = tortuga_core::Engine::new_with_token(data_dir, token).await?;
    let (addr, task) = tortuga_core::server::serve(
        engine.clone(),
        SocketAddr::from(([127, 0, 0, 1], port)),
        ui_dir,
    )
    .await?;
    println!("Tortuga API: http://{addr}/#token={}", engine.token);
    task.await?;
    Ok(())
}
