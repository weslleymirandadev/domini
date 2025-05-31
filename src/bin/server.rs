use crate::handlers::{agents, commands, results};
use crate::state::ServerState;
use axum::{
    Router,
    routing::{get, post},
};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tracing::info;
use tracing_subscriber;

#[path = "../handlers/mod.rs"]
mod handlers;

#[path = "../state.rs"]
mod state;

#[tokio::main(flavor = "multi_thread", worker_threads = 8)]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();
    let state = ServerState::new();
    
    let state_clone = Arc::clone(&state);
    
    tokio::spawn(async move {
        loop {
            state_clone.cleanup_inactive_agents().await;
            tokio::time::sleep(Duration::from_secs(60)).await;
        }
    });

    let app = Router::new()
        .merge(agents::routes())
        .route("/tasks", get(commands::get_tasks))
        .route("/", post(commands::post_command))
        .route("/broadcast", post(commands::post_broadcast))
        .route("/results", post(results::post_results))
        .with_state(state);

    let addr = SocketAddr::from(([127, 0, 0, 1], 8080)); // O servidor escuta em localhost:8080
    info!("Starting server at http://{}", addr);

    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .map_err(|e| format!("Error linking to address: {}", e))?;

    axum::serve(listener, app).await?;

    Ok(())
}
