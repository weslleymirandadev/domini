use std::{
    thread::sleep,
    time::{Duration, Instant},
};

use reqwest::Client;
use tokio::time::timeout;
use tracing::{debug, error, info, warn};

use super::server_health::{check_server_health, send_heartbeat};
use crate::{
    agent::{
        exec::execute_command_and_get_cwd,
        registration::{deregister, register},
    },
    types::ResultPayload,
};

const SERVER_HTTP: &str = "http://localhost:8080";

pub async fn poll_commands(client: &Client, agent_id: &str) -> Result<(), String> {
    let retries = 3;
    let mut timeout_count = 0;
    let mut last_heartbeat = Instant::now();

    loop {
        if last_heartbeat.elapsed() > Duration::from_secs(30) {
            if let Err(e) = send_heartbeat(client, agent_id).await {
                warn!("Heartbeat failed: {}", e);
            } else {
                debug!("Heartbeat sent successfully");
            }
            last_heartbeat = Instant::now();
        }

        if let Err(e) = check_server_health(client).await {
            warn!("Server unavailable: {}. Retrying in 5s", e);
            sleep(Duration::from_secs(5));
            continue;
        }

        for attempt in 1..=retries {
            let start = Instant::now();
            info!(
                "Fetching commands for UUID: {} (attempt {}/{})",
                agent_id, attempt, retries
            );
            let res = match timeout(
                Duration::from_secs(15),
                client
                    .get(format!("{}/tasks?uuid={}", SERVER_HTTP, agent_id))
                    .send(),
            )
            .await
            {
                Ok(res) => res.map_err(|e| format!("Error fetching commands: {}", e))?,
                Err(_) => {
                    error!("Timeout while fetching commands");
                    timeout_count += 1;
                    if timeout_count >= 3 {
                        warn!("Persistent timeouts, forcing re-registration");
                        if let Err(e) = deregister(client, agent_id).await {
                            warn!("Failed to deregister: {}", e);
                        }
                        register(client, agent_id).await?;
                        timeout_count = 0;
                    }
                    if attempt < retries {
                        info!("Retrying in 5 seconds...");
                        sleep(Duration::from_secs(5));
                    }
                    continue;
                }
            };
            let duration = start.elapsed();
            debug!("Polling for UUID {} took {:?}", agent_id, duration);

            let status = res.status();
            let text = res.text().await.unwrap_or_default();
            debug!("Polling response: {} - {}", status, text);
            timeout_count = 0;
            match status {
                reqwest::StatusCode::OK => {
                    let json: serde_json::Value = serde_json::from_str(&text)
                        .map_err(|e| format!("Error parsing command: {}", e))?;
                    let command = match json["command"].as_str() {
                        Some(cmd) => cmd.to_string(),
                        None => {
                            info!("No command received in response");
                            sleep(Duration::from_secs(1));
                            continue;
                        }
                    };
                    let command_id = match json["command_id"].as_str() {
                        // Extrai command_id
                        Some(id) => id.to_string(),
                        None => {
                            warn!("No command_id received for command: {}", command);
                            continue; // Ignora o comando se o ID estiver faltando
                        }
                    };
                    info!("Command received: {} (ID: {})", command, command_id);
                    let (result_output, current_working_directory) =
                        execute_command_and_get_cwd(&command).await;
                    info!("Command output: {:?}", result_output);

                    let payload = ResultPayload {
                        uuid: Some(agent_id.to_string()),
                        result: Some(result_output),
                        current_working_directory: Some(current_working_directory),
                        command_id: Some(command_id), // Inclua o command_id no payload
                    };
                    let res = client
                        .post(format!("{}/results", SERVER_HTTP))
                        .json(&payload)
                        .send()
                        .await
                        .map_err(|e| format!("Error sending result: {}", e))?;
                    info!(
                        "Result sending response: {} - {}",
                        res.status(),
                        res.text().await.unwrap_or_default()
                    );
                }
                reqwest::StatusCode::NO_CONTENT => {
                    info!("No commands available");
                }
                reqwest::StatusCode::NOT_FOUND => {
                    warn!("Agent not registered on server, attempting re-registration");
                    if let Err(e) = deregister(client, agent_id).await {
                        warn!("Failed to deregister: {}", e);
                    }
                    register(client, agent_id).await?;
                }
                _ => error!("Unexpected polling response: {} - {}", status, text),
            }
            sleep(Duration::from_secs(1));
            break;
        }
        sleep(Duration::from_secs(1));
    }
}
