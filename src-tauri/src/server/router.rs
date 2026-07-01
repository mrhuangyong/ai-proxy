use crate::mcp;
use crate::server::api;
use crate::server::handlers;
use crate::server::handlers_failover;
use crate::server::middleware::auth_middleware;
use crate::skill;
use axum::middleware;
use axum::routing::{get, post};
use axum::Router;

pub fn create_router() -> Router {
    let proxy_routes = Router::new()
        .route("/v1/chat/completions", post(handlers::handle_completions))
        .route("/v1/responses", post(handlers::handle_responses))
        .route(
            "/v1/responses/compact",
            post(handlers::handle_responses_compact),
        )
        .route("/v1/messages", post(handlers::handle_anthropic))
        .route(
            "/v1/messages/count_tokens",
            post(handlers::handle_anthropic_count_tokens),
        )
        .route("/v1/models", get(handlers::handle_list_models))
        .route("/v1/models/:model", get(handlers::handle_get_model))
        .route("/v1beta/models", get(handlers::handle_gemini_list_models))
        .route(
            "/v1beta/models/:model",
            get(handlers::handle_gemini_get_model).post(handlers::handle_gemini),
        )
        .layer(middleware::from_fn(auth_middleware));

    // Failover group: virtual model routing with sticky-failover.
    let failover_routes = Router::new()
        .route(
            "/failover/v1/chat/completions",
            post(handlers_failover::handle_completions),
        )
        .route(
            "/failover/v1/responses",
            post(handlers_failover::handle_responses),
        )
        .route(
            "/failover/v1/responses/compact",
            post(handlers_failover::handle_responses_compact),
        )
        .route(
            "/failover/v1/messages",
            post(handlers_failover::handle_anthropic),
        )
        .route(
            "/failover/v1/models",
            get(handlers_failover::handle_list_models),
        )
        .route(
            "/failover/v1/models/:model",
            get(handlers_failover::handle_get_model),
        )
        .route(
            "/failover/v1beta/models",
            get(handlers_failover::handle_gemini_list_models),
        )
        .route(
            "/failover/v1beta/models/:model",
            get(handlers_failover::handle_gemini_get_model).post(handlers_failover::handle_gemini),
        )
        .layer(middleware::from_fn(auth_middleware));

    let router = Router::new()
        .merge(proxy_routes)
        .merge(failover_routes)
        .route("/health", get(health_check))
        .nest("/api", api::api_routes());

    // Server mode: mount auth routes (login/logout/me)
    #[cfg(feature = "server")]
    let router = {
        let router = router.nest("/api/auth", crate::auth::handlers::auth_routes());
        let router = router.nest(
            "/api/mcp",
            mcp::mcp_routes().layer(middleware::from_fn(
                crate::auth::middleware::jwt_auth_middleware,
            )),
        );
        router.nest(
            "/api/skills",
            skill::skill_routes().layer(middleware::from_fn(
                crate::auth::middleware::jwt_auth_middleware,
            )),
        )
    };

    #[cfg(not(feature = "server"))]
    let router = {
        let router = router.nest("/api/mcp", mcp::mcp_routes());
        router.nest("/api/skills", skill::skill_routes())
    };

    router
}

async fn health_check() -> &'static str {
    "OK"
}
