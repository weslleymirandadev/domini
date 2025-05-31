use reqwest::Client;
use tracing::info;

use crate::types::Agent;

const SERVER_HTTP: &str = "http://localhost:8080";

pub async fn list_agents(client: &Client, show: bool) -> Result<Vec<Agent>, String> {
    let res = client
        .get(format!("{}/agents", SERVER_HTTP))
        .send()
        .await
        .map_err(|e| format!("Failed to list agents: {}", e))?;
    let agents: Vec<Agent> = res
        .json()
        .await
        .map_err(|e| format!("Error parsing agent list: {}", e))?;
    if show {
        if agents.is_empty() {
            info!("No agents connected.");
        } else {
            info!("\nConnected agents:");
            for (i, agent) in agents.iter().enumerate() {
                println!(
                    "[{}] ({})[{}]: {}, {}",
                    i, agent.uuid, agent.ip, agent.hostname, agent.system
                );
            }
        }
    }
    Ok(agents)
}
