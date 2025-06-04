use sqlx::{Pool, Postgres};

pub async fn init_db(db: &Pool<Postgres>) -> Result<(), sqlx::Error> {
    // Criação da tabela agents
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS agents (
            id SERIAL PRIMARY KEY,
            uuid TEXT NOT NULL UNIQUE,
            ip TEXT NOT NULL,
            username TEXT NOT NULL,
            hostname TEXT NOT NULL,
            country TEXT,
            city TEXT,
            region TEXT,
            latitude DOUBLE PRECISION,
            longitude DOUBLE PRECISION,
            version TEXT NOT NULL,
            online BOOLEAN NOT NULL DEFAULT false
        )"
    )
    .execute(db)
    .await?;

    // Criação da tabela agent_version
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS agent_version (
            id SERIAL PRIMARY KEY,
            version TEXT NOT NULL UNIQUE,
            \"binary\" BYTEA NOT NULL,
            updated_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP
        )"
    )
    .execute(db)
    .await?;

    Ok(())
}