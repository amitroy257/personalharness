//! `harness-server` — run Personal Harness without the desktop app: the
//! OpenAI-compatible API plus (optionally) the web UI in your browser.
//!
//!   harness-server [--data-dir DIR] [--port 4141] [--ui ./dist] [--no-ui]

use std::path::PathBuf;

use harness_core::{server, Harness};
use tower_http::services::{ServeDir, ServeFile};

struct Args {
    data_dir: PathBuf,
    port: Option<u16>,
    ui: Option<PathBuf>,
}

fn usage() -> ! {
    eprintln!(
        "Usage: harness-server [--data-dir DIR] [--port PORT] [--ui DIST_DIR | --no-ui]\n\n\
         Env: HARNESS_DATA_DIR (default ~/.personal-harness), RUST_LOG"
    );
    std::process::exit(2)
}

fn parse() -> Args {
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));
    let mut a = Args {
        data_dir: std::env::var_os("HARNESS_DATA_DIR").map(PathBuf::from).unwrap_or_else(|| home.join(".personal-harness")),
        port: None,
        ui: Some(PathBuf::from("dist")),
    };
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--data-dir" => a.data_dir = it.next().map(PathBuf::from).unwrap_or_else(|| usage()),
            "--port" => a.port = Some(it.next().and_then(|p| p.parse().ok()).unwrap_or_else(|| usage())),
            "--ui" => a.ui = Some(it.next().map(PathBuf::from).unwrap_or_else(|| usage())),
            "--no-ui" => a.ui = None,
            "-h" | "--help" => usage(),
            other => {
                eprintln!("unknown argument: {other}");
                usage()
            }
        }
    }
    a
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "harness_core=info,harness_server=info".into()),
        )
        .init();
    let args = parse();
    let h = Harness::open(&args.data_dir)?;
    if let Some(port) = args.port {
        h.update_settings(|s| {
            s.server.port = port;
            s.server.enabled = true;
        })?;
    }
    let ui = args.ui.filter(|d| d.join("index.html").exists());
    if let Some(dir) = &ui {
        let files = ServeDir::new(dir).fallback(ServeFile::new(dir.join("index.html")));
        h.set_extra_routes(server::ui_router(h.clone()).fallback_service(files));
    }
    h.start_server().await?;
    h.spawn_background();

    let s = h.settings();
    println!("Personal Harness (headless)");
    println!("  data:   {}", args.data_dir.display());
    println!("  API:    http://127.0.0.1:{}/v1", s.server.port);
    println!("  token:  {}", s.server.token);
    if ui.is_some() {
        println!("  web UI: http://127.0.0.1:{}/#token={}", s.server.port, s.server.token);
    } else {
        println!("  web UI: disabled (build it with `npm run build`, then pass --ui dist)");
    }
    tokio::signal::ctrl_c().await?;
    Ok(())
}
