use agent::{id_management::get_or_create_id, poll_command::poll_commands, registration::{deregister, register}};
use reqwest::Client;
use std::time::Duration;
use tracing::warn;

#[path = "../agent/mod.rs"]
mod agent;

#[path = "../types.rs"]
mod types;


#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();
    let client = Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
        .map_err(|e| format!("Error creating HTTP client: {}", e))?;

    let agent_id = get_or_create_id().await?;
    if let Err(e) = deregister(&client, &agent_id).await {
        warn!("Failed to deregister at startup: {}", e);
    }
    register(&client, &agent_id).await?;
    poll_commands(&client, &agent_id).await?;
    Ok(())
}