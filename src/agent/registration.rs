use std::{thread::sleep, time::{Duration, Instant}};

use reqwest::Client;
use tracing::{debug, info, warn};

use crate::types::RegisterPayload;

const SERVER_HTTP: &str = "http://localhost:8080";

pub async fn deregister(client: &Client, agent_id: &str) -> Result<(), String> {
    let start = Instant::now();
    let res = client
        .delete(format!("{}/deregister?uuid={}", SERVER_HTTP, agent_id))
        .timeout(Duration::from_secs(10))
        .send()
        .await
        .map_err(|e| format!("Error deregistering: {}", e))?;
    let duration = start.elapsed();
    debug!("Deregister for UUID {} took {:?}", agent_id, duration);
    if res.status().is_success() {
        info!("Agent {} successfully deregistered", agent_id);
        Ok(())
    } else {
        Err(format!("Failed to deregister: {} - {}", res.status(), res.text().await.unwrap_or_default()))
    }
}


pub async fn register(client: &Client, agent_id: &str) -> Result<(), String> {
    let retries = 3;
    let mut backoff = 5;
    for attempt in 1..=retries {
        let start = Instant::now();
        let ip = match client
            .get("https://ipinfo.io/ip")
            .timeout(Duration::from_secs(10))
            .send()
            .await
        {
            Ok(res) => res
                .text()
                .await
                .map_err(|e| format!("Error parsing IP: {}", e))?
                .trim()
                .to_string(),
            Err(e) => {
                warn!("Failed to get IP (attempt {}/{}): {}", attempt, retries, e);
                if attempt < retries {
                    sleep(Duration::from_secs(backoff));
                    backoff = (backoff * 2).min(60);
                    continue;
                }
                return Err(format!("Error getting IP after {} attempts: {}", retries, e));
            }
        };

        let hostname = hostname::get()
            .map_err(|e| format!("Error getting hostname: {}", e))?
            .to_string_lossy()
            .into_owned();
        let system = std::env::consts::OS.to_string();

        let payload = RegisterPayload {
            uuid: agent_id.to_string(),
            ip,
            hostname,
            system,
        };
        let res = match client
            .post(format!("{}/register", SERVER_HTTP))
            .json(&payload)
            .send()
            .await
        {
            Ok(res) => res,
            Err(e) => {
                warn!("Failed to register (attempt {}/{}): {}", attempt, retries, e);
                if attempt < retries {
                    sleep(Duration::from_secs(backoff));
                    backoff = (backoff * 2).min(60);
                    continue;
                }
                return Err(format!("Error registering after {} attempts: {}", retries, e));
            }
        };
        let duration = start.elapsed();
        debug!("Registration for UUID {} took {:?}", agent_id, duration);

        match res.status() {
            reqwest::StatusCode::OK => {
                info!("Registration successful: {}", res.text().await.unwrap_or_default());
                return Ok(());
            }
            _ => {
                let err_msg = format!(
                    "Unexpected response during registration: {} - {}",
                    res.status(),
                    res.text().await.unwrap_or_default()
                );
                if attempt < retries {
                    warn!("Registration failed (attempt {}/{}): {}", attempt, retries, err_msg);
                    sleep(Duration::from_secs(backoff));
                    backoff = (backoff * 2).min(60);
                    continue;
                }
                return Err(err_msg);
            }
        }
    }
    Err("Maximum registration attempts reached".to_string())
}
