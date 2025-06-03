use std::error::Error;
use crate::handlers::agent::Agent;

#[path = "../handlers/mod.rs"]
mod handlers;

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let mut agent = Agent::new().await?;
    println!("Connecting...");
    agent.run().await?;
    Ok(())
}