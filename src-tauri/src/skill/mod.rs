pub mod types;
pub mod scanner;
pub mod manager;
pub mod handlers;

use axum::routing;
use axum::Router;

pub fn skill_routes() -> Router {
    Router::new()
        .route("/sources", routing::get(handlers::list_sources).post(handlers::create_source))
        .route("/sources/:id", routing::put(handlers::update_source).delete(handlers::delete_source))
        .route("/discover", routing::post(handlers::discover))
        .route("/", routing::get(handlers::list_skills).post(handlers::create_skill_handler))
        .route("/:id", routing::get(handlers::get_skill).delete(handlers::delete_skill_handler))
        .route("/:id/linked", routing::get(handlers::get_linked_skills))
        .route("/:id/skill-md", routing::put(handlers::update_skill_md))
        .route("/install", routing::post(handlers::install_skill))
        .route("/uninstall", routing::post(handlers::uninstall_skill))
        .route("/install-from-url", routing::post(handlers::install_from_url))
        .route("/scan", routing::post(handlers::scan))
}
