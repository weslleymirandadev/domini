use reqwest::Client;
use tracing::{error, info};
use uuid::Uuid;

use crate::types::{CommandPayload, FullCommandResult};

const SERVER_HTTP: &str = "http://localhost:8080";

pub async fn send_command(
    client: &Client,
    uuid: &str,
    cmd: &str,
    wait_response: bool,
) -> Result<Option<FullCommandResult>, String> {
    let command_id = Uuid::new_v4().to_string();
    let payload = CommandPayload {
        uuid: Some(uuid.to_string()),
        command: Some(cmd.to_string()),
        wait_response: Some(wait_response),
        command_id,
    };

    let res = client
        .post(format!("{}/", SERVER_HTTP))
        .json(&payload)
        .send()
        .await
        .map_err(|e| format!("Failed to send command: {}", e))?;

    match res.status() {
        reqwest::StatusCode::OK => {
            if wait_response {
                let json_value = res
                    .json::<serde_json::Value>()
                    .await
                    .map_err(|e| format!("Error parsing result: {}", e))?;

                let full_result: FullCommandResult = if json_value.is_array() {
                    let arr = json_value
                        .as_array()
                        .ok_or_else(|| "Expected an array".to_string())?;
                    if arr.is_empty() {
                        return Err("Empty results array".to_string());
                    }
                    serde_json::from_value(arr[0].clone()).map_err(|e| {
                        format!("Error deserializing FullCommandResult from array: {}", e)
                    })?
                } else {
                    serde_json::from_value(json_value)
                        .map_err(|e| format!("Error deserializing FullCommandResult: {}", e))?
                };
                println!("{}", full_result.result);
                return Ok(Some(full_result));
            } else {
                info!("Command sent successfully (async).");
            }
        }
        reqwest::StatusCode::ACCEPTED => {
            info!("Command sent successfully (async).");
        }
        reqwest::StatusCode::NOT_FOUND => {
            error!("Agent {} not found on server.", uuid);
        }
        reqwest::StatusCode::REQUEST_TIMEOUT => {
            error!("Command response timeout exceeded.");
        }
        _ => {
            res.text().await.unwrap_or_default();
        }
    }
    Ok(None)
}
