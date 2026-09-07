pub mod api;
pub mod handlers;
pub mod handlers_failover;
pub mod middleware;
pub mod passthrough;
pub mod retry_invisible;
pub mod retry_session;
pub mod router;

use axum::Router;
use std::net::SocketAddr;
use tracing::info;

use crate::server::middleware::create_cors_layer;
use crate::server::router::create_router;

/// Default proxy port when `http_port` is not configured in settings.
///
/// Dev builds share downstream app configs (~/.codex etc.) with the production
/// install but run their own proxy on their own database. Defaulting dev to a
/// distinct port prevents a dev launch from silently rewriting the production
/// base_url to a port that dies when the dev process exits.
pub fn default_http_port() -> u16 {
    if cfg!(debug_assertions) {
        17861
    } else {
        7860
    }
}

pub async fn create_server(host: &str, port: u16) -> Router {
    let cors = create_cors_layer(host);
    let app = create_router().layer(cors);
    info!(
        "HTTP server configured on {}:{} with CORS for host '{}'",
        host, port, host
    );
    app
}

pub async fn start_server(
    host: &str,
    port: u16,
    mut shutdown_rx: tokio::sync::watch::Receiver<bool>,
) {
    let app = create_server(host, port).await;

    // Start the virtual-model health checker (no-op when no virtual models exist).
    crate::virtual_model::health::spawn_health_checker();

    let addr = SocketAddr::new(
        host.parse()
            .unwrap_or_else(|_| "127.0.0.1".parse().unwrap()),
        port,
    );

    info!("Starting HTTP server on {}", addr);

    let listener = match tokio::net::TcpListener::bind(addr).await {
        Ok(l) => l,
        Err(e) => {
            tracing::error!("Failed to bind to {}: {}", addr, e);
            return;
        }
    };

    if let Err(e) = axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            shutdown_rx.changed().await.ok();
        })
        .await
    {
        tracing::error!("HTTP server error: {}", e);
    }

    info!("HTTP server stopped");
}

/// Server mode: start with optional static file serving for the Vue frontend.
#[cfg(feature = "server")]
pub async fn start_server_with_static(
    host: &str,
    port: u16,
    static_dir: Option<String>,
    mut shutdown_rx: tokio::sync::watch::Receiver<bool>,
) {
    let mut app = create_server(host, port).await;

    // Start the virtual-model health checker (no-op when no virtual models exist).
    crate::virtual_model::health::spawn_health_checker();

    if let Some(dir) = static_dir.filter(|s| !s.is_empty()) {
        let serve_dir =
            tower_http::services::ServeDir::new(&dir).append_index_html_on_directories(true);
        app = app.fallback_service(serve_dir);
        info!("Static file service enabled: {}", dir);
    }

    let addr = SocketAddr::new(
        host.parse()
            .unwrap_or_else(|_| "127.0.0.1".parse().unwrap()),
        port,
    );

    info!("Starting HTTP server on {}", addr);

    let listener = match tokio::net::TcpListener::bind(addr).await {
        Ok(l) => l,
        Err(e) => {
            tracing::error!("Failed to bind to {}: {}", addr, e);
            return;
        }
    };

    if let Err(e) = axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            shutdown_rx.changed().await.ok();
        })
        .await
    {
        tracing::error!("HTTP server error: {}", e);
    }

    info!("HTTP server stopped");
}
