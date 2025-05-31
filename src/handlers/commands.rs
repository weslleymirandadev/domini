use crate::state::{ServerState};
use axum::{
    extract::{Json, Query, State},
    http::StatusCode,
    response::IntoResponse,
};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::time::{Duration, Instant, sleep};
use tracing::{debug, info, warn};
use types::{CommandPayload, BroadcastPayload};

#[path = "../types.rs"]
mod types;

pub async fn get_tasks(
    State(state): State<Arc<ServerState>>,
    Query(params): Query<HashMap<String, String>>,
) -> impl IntoResponse {
    let uuid = match params.get("uuid") {
        Some(u) => u,
        None => return (StatusCode::BAD_REQUEST, "Missing UUID").into_response(),
    };

    let start = Instant::now();
    debug!("Starting get_tasks for UUID: {}", uuid);

    let agent_active = {
        let agents = state.agents.read().await;
        match agents.get(uuid) {
            Some(agent) => {
                debug!("Agent {} found, active: {}", uuid, agent.active);
                agent.active
            }
            None => {
                debug!("Agent {} not found", uuid);
                return (StatusCode::NOT_FOUND, "Agent not found").into_response();
            }
        }
    };

    if !agent_active {
        warn!("Agent {} is inactive, cannot receive commands.", uuid);
        return (StatusCode::NOT_FOUND, "Agent not active").into_response();
    }


    let channels = state.agent_channels.read().await;
    let agent_channels = match channels.get(uuid) {
        Some(ch) => ch,
        None => {
            warn!(
                "Channels of agent {} was not found, removing agent.",
                uuid
            );
            drop(channels);
            state.remove_agent(uuid).await;
            return (StatusCode::NOT_FOUND, "Agent channels not found").into_response();
        }
    };

    let mut command_queue = agent_channels.command_queue.write().await;
    match command_queue.pop_front() {
        Some(full_command_string) => {
            let parts: Vec<&str> = full_command_string.splitn(2, ':').collect();
            if parts.len() == 2 {
                let command_id = parts[0].to_string();
                let command_text = parts[1].to_string();
                debug!(
                    "Command found for {}: {} (ID: {})",
                    uuid, command_text, command_id
                );
                Json(serde_json::json!({
                    "command": command_text,
                    "command_id": command_id,
                }))
                .into_response()
            } else {
                warn!(
                    "Invalid command format in queue for {}: {}",
                    uuid, full_command_string
                );
                (StatusCode::INTERNAL_SERVER_ERROR, "Invalid command format").into_response()
            }
        }
        None => {
            debug!(
                "No commands in queue for {}. Wait time: {:?}",
                uuid,
                start.elapsed()
            );
            (StatusCode::NO_CONTENT, "".to_string()).into_response()
        }
    }
}

pub async fn post_command(
    State(state): State<Arc<ServerState>>,
    Json(payload): Json<CommandPayload>,
) -> impl IntoResponse {
    let uuid = match payload.uuid {
        Some(u) => u,
        None => {
            return (StatusCode::BAD_REQUEST, "Missing UUID in command payload").into_response();
        }
    };
    let command = match payload.command {
        Some(c) => c,
        None => {
            return (
                StatusCode::BAD_REQUEST,
                "Missing command in command payload",
            )
                .into_response();
        }
    };
    let wait_response = payload.wait_response.unwrap_or(true);
    let command_id = payload.command_id;

    info!(
        "Command '{}' received for agent {} (ID: {}). Wait response: {}",
        command, uuid, command_id, wait_response
    );

    let channels = state.agent_channels.read().await;
    let agent_channels = match channels.get(&uuid) {
        Some(ch) => ch,
        None => {
            warn!(
                "Channels for agent {} not found. Agent may be offline.",
                uuid
            );
            return (StatusCode::NOT_FOUND, "Agent channels not found").into_response();
        }
    };

    let command_to_send = format!("{}:{}", command_id.clone(), command.clone());

    let mut queue = agent_channels.command_queue.write().await;
    queue.push_back(command_to_send);
    drop(queue);

    if wait_response {
        let start_time = Instant::now();
        let timeout_duration = Duration::from_secs(30);

        loop {
            if start_time.elapsed() > timeout_duration {
                warn!(
                    "Agent {} did not respond to command '{}' (ID: {}) within 30 seconds.",
                    uuid, command, command_id
                );
                return (
                    StatusCode::REQUEST_TIMEOUT,
                    "Agent did not respond within 30 seconds",
                )
                    .into_response();
            }

            let mut pending_results_lock = state.pending_results.write().await;
            if let Some(agent_results) = pending_results_lock.get_mut(&uuid) {
                if let Some(full_result_arc) = agent_results.remove(&command_id) {
                    debug!(
                        "Result for command {} (ID: {}) received from agent.",
                        full_result_arc.uuid, full_result_arc.command_id
                    );
                    return Json(vec![full_result_arc.as_ref().clone()]).into_response();
                }
            }
            drop(pending_results_lock);
            sleep(Duration::from_millis(100)).await;
        }
    } else {
        info!(
            "Async command '{}' (ID: {}) queued for agent {}.",
            command, command_id, uuid
        );
        (StatusCode::ACCEPTED, "Command queued asynchronously").into_response()
    }
}

pub async fn post_broadcast(
    State(state): State<Arc<ServerState>>,
    Json(payload): Json<BroadcastPayload>,
) -> impl IntoResponse {
    let command = match payload.command {
        Some(c) => c,
        None => {
            return (
                StatusCode::BAD_REQUEST,
                "Missing command in broadcast payload",
            )
                .into_response();
        }
    };
    let wait_response = payload.wait_response.unwrap_or(true);
    let command_id = payload.command_id;

    info!(
        "Broadcasting command '{}' (ID: {}). Wait response: {}",
        command, command_id, wait_response
    );

    let channels = state.agent_channels.read().await;
    let mut active_agents_uuids = Vec::new();

    for (uuid, agent_channels) in channels.iter() {
        let is_active = {
            let agents_lock = state.agents.read().await;
            agents_lock.get(uuid).map(|a| a.active).unwrap_or(false)
        };

        if !is_active {
            debug!("Agent {} inactive, skipping broadcast.", uuid);
            continue;
        }

        let command_to_send = format!("{}:{}", command_id.clone(), command.clone());
        let mut queue = agent_channels.command_queue.write().await;
        queue.push_back(command_to_send);
        
        drop(queue);

        active_agents_uuids.push(uuid.clone());
        debug!(
            "Broadcast queued for UUID {}: {} (ID: {})",
            uuid, command, command_id
        );
    }

    drop(channels);

    if wait_response {
        let mut tasks = Vec::new();
        let timeout_duration = Duration::from_secs(30);

        for agent_uuid in active_agents_uuids.iter() {
            let agent_uuid_clone = agent_uuid.clone();
            let state_clone = Arc::clone(&state);
            let expected_command_id = command_id.clone();

            let command_clone = command.clone();
            tasks.push(tokio::spawn(async move {
                let start_time = Instant::now();
                loop {
                    if start_time.elapsed() > timeout_duration {
                        warn!("Agente {} não respondeu ao broadcast do comando '{}' (ID: {}) dentro de 30 segundos.", agent_uuid_clone, command_clone, expected_command_id);
                        return None;
                    }

                    let mut pending_results_lock = state_clone.pending_results.write().await;
                    if let Some(agent_results_map) = pending_results_lock.get_mut(&agent_uuid_clone) {
                        if let Some(full_result_arc) = agent_results_map.remove(&expected_command_id) {
                            debug!("Broadcast result for agent {} (ID: {}) received.", agent_uuid_clone, full_result_arc.command_id);
                            return Some(full_result_arc.as_ref().clone());
                        }
                    }
                    drop(pending_results_lock);
                    sleep(Duration::from_millis(100)).await;
                }
            }));
        }

        let mut broadcast_results = Vec::new();
        for task in tasks {
            if let Ok(Some(full_result)) = task.await {
                broadcast_results.push(full_result);
            }
        }

        if broadcast_results.len() == active_agents_uuids.len() && !active_agents_uuids.is_empty() {
            Json(broadcast_results).into_response()
        } else if !broadcast_results.is_empty()
            && broadcast_results.len() < active_agents_uuids.len()
        {
            warn!(
                "Nem todos os agentes responderam ao broadcast. Respostas recebidas: {}/{}",
                broadcast_results.len(),
                active_agents_uuids.len()
            );
            (StatusCode::PARTIAL_CONTENT, Json(broadcast_results)).into_response()
        } else if broadcast_results.is_empty() && !active_agents_uuids.is_empty() {
            warn!(
                "Nenhum agente ativo respondeu ao broadcast do comando '{}' (ID: {}).",
                command, command_id
            );
            (StatusCode::REQUEST_TIMEOUT, Json(broadcast_results)).into_response()
        } else {
            info!(
                "Nenhum agente ativo para broadcast do comando '{}' (ID: {}).",
                command, command_id
            );
            (StatusCode::NO_CONTENT, "".to_string()).into_response()
        }
    } else {
        info!(
            "Async broadcast '{}' (ID: {}) queued for {} agents.",
            command,
            command_id,
            active_agents_uuids.len()
        );
        (
            StatusCode::ACCEPTED,
            format!(
                "Broadcast enqueued asynchronously for {} agents",
                active_agents_uuids.len()
            ),
        )
            .into_response()
    }
}
