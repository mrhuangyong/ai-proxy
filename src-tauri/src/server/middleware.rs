use axum::body::Body;
use axum::http::{HeaderValue, Method, Request};
use axum::response::Response;
#[cfg(feature = "server")]
use axum::{http::StatusCode, response::IntoResponse};
use tower_http::cors::CorsLayer;

pub fn create_cors_layer(host: &str) -> CorsLayer {
    let origin = if host == "0.0.0.0" || host == "127.0.0.1" || host == "localhost" {
        "*"
    } else {
        host
    };

    let allowed_origin = if origin == "*" {
        tower_http::cors::Any.into()
    } else {
        match HeaderValue::from_str(&format!("http://{}", origin)) {
            Ok(v) => tower_http::cors::AllowOrigin::exact(v),
            Err(_) => tower_http::cors::Any.into(),
        }
    };

    CorsLayer::new()
        .allow_origin(allowed_origin)
        .allow_methods([
            Method::GET,
            Method::POST,
            Method::PUT,
            Method::DELETE,
            Method::OPTIONS,
        ])
        .allow_headers(tower_http::cors::Any)
}

pub async fn auth_middleware(req: Request<Body>, next: axum::middleware::Next) -> Response {
    // Desktop mode: proxy routes live behind the local Tauri boundary,
    // so API-key auth is not required here.
    #[cfg(not(feature = "server"))]
    {
        return next.run(req).await;
    }

    #[cfg(feature = "server")]
    {
        // Model discovery must work without Authorization (Codex probes these).
        if is_public_models_request(req.method(), req.uri().path()) {
            return next.run(req).await;
        }

        let pool = crate::db::get_pool().await;

        let enabled: (String,) =
            match sqlx::query_as("SELECT value FROM settings WHERE key = 'proxy_auth_enabled'")
                .fetch_one(pool)
                .await
            {
                Ok(v) => v,
                Err(_) => return next.run(req).await,
            };

        if enabled.0 != "true" {
            return next.run(req).await;
        }

        let pool = crate::db::get_pool().await;
        let expected_key: (String,) =
            match sqlx::query_as("SELECT value FROM settings WHERE key = 'proxy_auth_key'")
                .fetch_one(pool)
                .await
            {
                Ok(v) => v,
                Err(_) => return next.run(req).await,
            };

        if expected_key.0.is_empty() {
            return next.run(req).await;
        }

        let provided = req
            .headers()
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
            .map(String::from)
            .or_else(|| {
                req.headers()
                    .get("x-api-key")
                    .and_then(|v| v.to_str().ok())
                    .map(String::from)
            })
            .unwrap_or_default();

        if provided == expected_key.0 {
            next.run(req).await
        } else {
            (StatusCode::UNAUTHORIZED, "Unauthorized").into_response()
        }
    }
}

/// GET model-list / model-get paths that never require proxy auth.
#[cfg(feature = "server")]
fn is_public_models_request(method: &Method, path: &str) -> bool {
    if *method != Method::GET {
        return false;
    }
    path == "/v1/models"
        || path.starts_with("/v1/models/")
        || path == "/v1beta/models"
        || path.starts_with("/v1beta/models/")
        || path == "/failover/v1/models"
        || path.starts_with("/failover/v1/models/")
        || path == "/failover/v1beta/models"
        || path.starts_with("/failover/v1beta/models/")
}
