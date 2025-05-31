use std::time::{Duration, Instant};

use reqwest::Client;
use tracing::debug;

const SERVER_HTTP: &str = "http://localhost:8080";


pub async fn check_server_health(client: &Client) -> Result<(), String> {
    let start = Instant::now();
    let res = client
        .get(format!("{}/agents", SERVER_HTTP))
        .timeout(Duration::from_secs(10))
        .send()
        .await
        .map_err(|e| format!("Error checking server health: {}", e))?;
    let duration = start.elapsed();
    debug!("Check server health took {:?}", duration);
    if res.status().is_success() {
        debug!("Server is active: {}", res.status());
        Ok(())
    } else {
        Err(format!("Server returned status: {} - {}", res.status(), res.text().await.unwrap_or_default()))
    }
}
pub async fn send_heartbeat(client: &Client, agent_id: &str) -> Result<(), String> {
    let res = client
        .get(format!("{}/heartbeat?uuid={}", SERVER_HTTP, agent_id))
        .timeout(Duration::from_secs(10))
        .send()
        .await
        .map_err(|e| format!("Error in heartbeat: {}", e))?;
    
    if res.status().is_success() {
        debug!("Heartbeat sent successfully for {}", agent_id);
        Ok(())
    } else {
        Err(format!("Heartbeat failed: {}", res.status()))
    }
}