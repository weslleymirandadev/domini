use axum::{
    extract::{Json, State},
    http::StatusCode,
    response::IntoResponse,
};
use std::sync::Arc;
use tracing::info;

use crate::state::{FullCommandResult, ServerState};
use types::ResultPayload;

#[path = "../types.rs"]
mod types;

// POST /results
pub async fn post_results(
    State(state): State<Arc<ServerState>>,
    Json(payload): Json<ResultPayload>,
) -> impl IntoResponse {
    let uuid = match payload.uuid {
        Some(u) => u,
        None => return (StatusCode::BAD_REQUEST, "Missing UUID").into_response(),
    };

    let command_id = match payload.command_id {
        Some(u) => u,
        None => return (StatusCode::BAD_REQUEST, "Missing command_id").into_response(),
    };

    let result_text = match payload.result {
        Some(r) => r,
        None => return (StatusCode::BAD_REQUEST, "Missing result").into_response(),
    };

    let current_working_directory = payload
        .current_working_directory
        .unwrap_or_else(|| "N/A".to_string());

    info!(
        "[RESULT][{}] (CWD: {}) {}",
        uuid, current_working_directory, result_text
    );

    let full_result_arc = Arc::new(FullCommandResult {
        uuid: uuid.clone(),
        result: result_text.clone(),
        current_working_directory: current_working_directory.clone(),
        command_id: command_id.clone(),
    });

    let mut pending_results = state.pending_results.write().await;
    pending_results
        .entry(uuid.clone())
        .or_default() // Garante que o HashMap interno para o UUID existe
        .insert(command_id.clone(), full_result_arc.clone()); // Insere o resultado

    info!(
        "Command result (ID: {}) for agent {} stored in pending_results.",
        command_id, uuid
    );
    StatusCode::OK.into_response()
}
