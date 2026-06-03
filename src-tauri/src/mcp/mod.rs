pub mod types;
pub mod handlers;
pub mod sync;

use axum::routing;
use axum::Router;

pub fn mcp_routes() -> Router {
    Router::new()
        .route("/servers", routing::get(handlers::list_servers).post(handlers::create_server))
        .route("/servers/:id", routing::put(handlers::update_server).delete(handlers::delete_server))
        .route("/servers/:id/bindings", routing::put(handlers::update_bindings))
        .route("/import/:app_type", routing::post(handlers::import_from_app))
        .route("/apply/:app_type", routing::post(handlers::apply_to_app))
}
