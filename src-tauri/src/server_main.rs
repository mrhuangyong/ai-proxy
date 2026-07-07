use clap::Parser;
use std::path::PathBuf;
use tracing::info;
use tracing_subscriber::prelude::*;

#[derive(Parser, Debug)]
#[command(name = "ai-proxy-server")]
#[command(about = "AI Proxy Server Mode")]
struct Args {
    #[arg(long, default_value = "0.0.0.0")]
    host: String,

    #[arg(short, long, default_value_t = 7860)]
    port: u16,

    #[arg(
        short,
        long,
        env = "AI_PROXY_DATA_DIR",
        default_value = "/var/lib/ai-proxy"
    )]
    data_dir: PathBuf,

    #[arg(long, env = "AI_PROXY_ADMIN_PASSWORD")]
    admin_password: Option<String>,

    #[arg(long, env = "AI_PROXY_STATIC_DIR")]
    static_dir: Option<String>,
}

#[tokio::main]
async fn main() {
    let args = Args::parse();

    let jwt_secret = std::env::var("AI_PROXY_JWT_SECRET").unwrap_or_default();
    if jwt_secret.is_empty() {
        eprintln!("FATAL: AI_PROXY_JWT_SECRET environment variable is not set.");
        eprintln!(
            "       Set it to a random, sufficiently long string (e.g. `openssl rand -hex 32`)."
        );
        std::process::exit(1);
    }

    tracing_subscriber::registry()
        .with(
            tracing_subscriber::fmt::Layer::default()
                .with_filter(tracing_subscriber::filter::LevelFilter::INFO),
        )
        .with(
            ai_proxy_lib::get_log_layer()
                .clone()
                .with_filter(tracing_subscriber::filter::LevelFilter::INFO),
        )
        .init();

    info!("Starting AI Proxy Server on {}:{}", args.host, args.port);

    std::fs::create_dir_all(&args.data_dir).expect("failed to create data directory");

    let db_path = args.data_dir.join("ai-proxy.db");
    ai_proxy_lib::init_database(db_path.to_str().unwrap()).await;

    ai_proxy_lib::ensure_default_admin(args.admin_password).await;

    ai_proxy_lib::sync::scheduler::start_scheduler(tokio::runtime::Handle::current());

    // Keep the sender alive so shutdown_rx.changed() blocks until process is killed
    let (_shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    ai_proxy_lib::server::start_server_with_static(
        &args.host,
        args.port,
        args.static_dir,
        shutdown_rx,
    )
    .await;
}
