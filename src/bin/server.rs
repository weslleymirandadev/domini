use crate::handlers::{db::init_db, server::start_server};
use dotenvy::dotenv;
use sqlx::postgres::PgPoolOptions;
use std::env;
use std::error::Error;

#[path = "../handlers/mod.rs"]
mod handlers;

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let _ = dotenv();

    let database_url = env::var("DATABASE_URL")
        .expect("DATABASE_URL não definida no arquivo .env ou nas variáveis de ambiente");

    let db = PgPoolOptions::new()
        .max_connections(100)
        .connect(&database_url)
        .await?;

    init_db(&db).await?;

    println!("Initializing server...");
    start_server(db).await;
    Ok(())
}
