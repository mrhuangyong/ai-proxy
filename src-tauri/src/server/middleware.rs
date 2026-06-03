use axum::body::Body;
use axum::http::{HeaderValue, Method, Request, StatusCode};
use axum::response::{IntoResponse, Response};
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

pub async fn auth_middleware(
    req: Request<Body>,
    next: axum::middleware::Next,
) -> Response {
    let pool = crate::db::get_pool().await;

    let enabled: (String,) = match sqlx::query_as(
        "SELECT value FROM settings WHERE key = 'proxy_auth_enabled'",
    )
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
    let expected_key: (String,) = match sqlx::query_as(
        "SELECT value FROM settings WHERE key = 'proxy_auth_key'",
    )
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
