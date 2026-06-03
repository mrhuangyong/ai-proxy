use axum::Json;

use crate::server::api::{ok, err_json, ApiError, ApiResponse};

pub async fn list_servers() -> Json<ApiResponse<()>> {
    ok(())
}

pub async fn create_server(
    _body: Json<()>,
) -> Result<Json<ApiResponse<()>>, Json<ApiError>> {
    Ok(ok(()))
}

pub async fn update_server(
    _body: Json<()>,
) -> Result<Json<ApiResponse<()>>, Json<ApiError>> {
    Ok(ok(()))
}

pub async fn delete_server() -> Result<Json<ApiResponse<()>>, Json<ApiError>> {
    Ok(ok(()))
}

pub async fn update_bindings(
    _body: Json<()>,
) -> Result<Json<ApiResponse<()>>, Json<ApiError>> {
    Ok(ok(()))
}

pub async fn import_from_app() -> Result<Json<ApiResponse<()>>, Json<ApiError>> {
    Ok(ok(()))
}

pub async fn apply_to_app() -> Result<Json<ApiResponse<()>>, Json<ApiError>> {
    Ok(ok(()))
}
