use crate::state::{AgentChannels, AgentInfo, FullCommandResult, ServerState};
use axum::{
    extract::{Query, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{delete, get, post}, Json,
};
use tokio::sync::{mpsc, RwLock};
use std::sync::Arc;
use std::collections::VecDeque;
use tracing::{debug, info};
use std::time::SystemTime; 
use types::{RegisterPayload, DeregisterParams};

#[path = "../types.rs"]
mod types;

pub fn routes() -> axum::Router<Arc<ServerState>> {
    axum::Router::new()
        .route("/agents", get(get_agents))
        .route("/register", post(post_register))
        .route("/deregister", delete(deregister))
}

pub async fn get_agents(State(state): State<Arc<ServerState>>) -> impl IntoResponse {
    let agents = state.agents.read().await;
    let list: Vec<_> = agents
        .iter()
        .filter(|(_, info)| info.active)
        .map(|(uuid, info)| {
            serde_json::json!({
                "uuid": uuid,
                "ip": info.ip,
                "hostname": info.hostname,
                "system": info.system,
                "last_seen": info.last_seen.duration_since(SystemTime::UNIX_EPOCH).unwrap_or_default().as_secs(),
                "active": info.active,
            })
        })
        .collect();
    Json(list).into_response()
}

pub async fn post_register(
    State(state): State<Arc<ServerState>>,
    Json(payload): Json<RegisterPayload>,
) -> impl IntoResponse {
    info!("Registration request for agent: {}", payload.uuid);

    let mut agents = state.agents.write().await;

    if agents.contains_key(&payload.uuid) {
        info!("Agent {} already registered, updating information.", payload.uuid);
    }

    let agent_info = AgentInfo {
        ip: payload.ip.clone(),
        hostname: payload.hostname.clone(),
        system: payload.system.clone(),
        last_seen: SystemTime::now(),
        active: true,
    };
    agents.insert(payload.uuid.clone(), agent_info);
    
    drop(agents);

    let (command_tx, mut command_rx) = mpsc::channel::<String>(100);
    let (result_tx, mut result_rx) = mpsc::channel::<Arc<FullCommandResult>>(100);
    let command_queue = Arc::new(RwLock::new(VecDeque::new()));

    let agent_channels = AgentChannels {
        command_sender: command_tx.clone(),
        command_queue: Arc::clone(&command_queue),
        result_sender: result_tx,
    };

    let mut channels_lock = state.agent_channels.write().await;
    channels_lock.insert(payload.uuid.clone(), agent_channels.clone());
    drop(channels_lock);

    let uuid_clone_for_commands = payload.uuid.clone();
    let cmd_queue_clone = Arc::clone(&command_queue);

    tokio::spawn(async move {
        debug!("Starting command consumer for agent {}", uuid_clone_for_commands);
        while let Some(cmd) = command_rx.recv().await {
            debug!("Command received from mpsc for agent {}: {}", uuid_clone_for_commands, cmd);
            let mut queue = cmd_queue_clone.write().await;
            queue.push_back(cmd);
        }
        info!("Operator command channel closed for agent {}", uuid_clone_for_commands);
    });

    let uuid_clone_for_results = payload.uuid.clone();
    let state_clone_for_results = Arc::clone(&state);

    tokio::spawn(async move {
        debug!("Starting result consumer for agent {}", uuid_clone_for_results);
        while let Some(full_result) = result_rx.recv().await {
            debug!("Result received via mpsc for agent {}: {}", uuid_clone_for_results, full_result.command_id);
            let mut pending_results = state_clone_for_results.pending_results.write().await;
            pending_results.entry(uuid_clone_for_results.clone())
                           .or_default()
                           .insert(full_result.command_id.clone(), full_result);
            drop(pending_results);
        }
        info!("Result consumer mpsc for agent {} closed.", uuid_clone_for_results);
    });

    info!("Agent {} successfully registered.", payload.uuid);
    StatusCode::OK.into_response()
}

pub async fn deregister(
    State(state): State<Arc<ServerState>>,
    Query(params): Query<DeregisterParams>,
) -> impl IntoResponse {
    let uuid = params.uuid;
    info!("Deregistration request for agent: {}", uuid);

    state.remove_agent(&uuid).await;

    info!("Agent {} successfully deregistered.", uuid);
    StatusCode::OK.into_response()
}