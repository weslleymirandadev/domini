use reqwest::Client;
use tracing::{error, info};
use uuid::Uuid;

use crate::types::{BroadcastPayload, FullCommandResult};

const SERVER_HTTP: &str = "http://localhost:8080";

pub async fn broadcast_command(client: &Client, cmd: &str, wait_response: bool) -> Result<(), String> {
    let command_id = Uuid::new_v4().to_string();
    let payload: BroadcastPayload = BroadcastPayload {
        command: Some(cmd.to_string()),
        wait_response: Some(wait_response),
        command_id,
    };

    info!("Sending broadcast '{}'", cmd);

    let res = client
        .post(format!("{}/broadcast", SERVER_HTTP))
        .json(&payload)
        .send()
        .await
        .map_err(|e| format!("Failed to send broadcast: {}", e))?;

    match res.status() {
        reqwest::StatusCode::OK | reqwest::StatusCode::PARTIAL_CONTENT => {
            if wait_response {
                let results: Vec<FullCommandResult> = res
                    .json()
                    .await
                    .map_err(|e| format!("Error parsing broadcast results: {}", e))?;
                if results.is_empty() {
                    println!("No results received for broadcast.");
                } else {
                    println!("Broadcast results (ID: {}):", payload.command_id);
                    for result in results {
                        println!("  Agent {}:", result.uuid);
                        println!("    CWD: {}", result.current_working_directory);
                        println!("    Result:\n{}", result.result);
                    }
                }
            } else {
                info!("Broadcast sent successfully (async).");
            }
        }
        reqwest::StatusCode::ACCEPTED => {
            info!("Broadcast sent successfully (async).");
        }
        reqwest::StatusCode::NO_CONTENT => {
            println!("No active agents for broadcast or no results returned.");
        }
        s => {
            let text = res.text().await.unwrap_or_default();
            error!("Error sending broadcast (status {}): {}", s, text);
        }
    }
    Ok(())
}
