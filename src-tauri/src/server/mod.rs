pub mod api;
pub mod handlers;
pub mod middleware;
pub mod retry_invisible;
pub mod retry_session;
pub mod router;

use axum::Router;
use std::net::SocketAddr;
use tracing::info;

use crate::server::middleware::create_cors_layer;
use crate::server::router::create_router;

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
