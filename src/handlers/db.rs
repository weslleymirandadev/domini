use sqlx::{Pool, Postgres};

pub async fn init_db(db: &Pool<Postgres>) -> Result<(), sqlx::Error> {
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

    sqlx::query(
        "CREATE TABLE IF NOT EXISTS operator_tokens (
            id SERIAL PRIMARY KEY,
            token TEXT NOT NULL UNIQUE,
            operator_id TEXT NOT NULL UNIQUE,
            public_key BYTEA NOT NULL,
            created_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
            expires_at TIMESTAMP,
            active BOOLEAN NOT NULL DEFAULT true
        )"
    )
    .execute(db)
    .await?;

    sqlx::query(
        "CREATE TABLE IF NOT EXISTS scheduled_tasks (
            id SERIAL PRIMARY KEY,
            command_type TEXT NOT NULL,
            args JSONB NOT NULL,
            execute_at TIMESTAMP NOT NULL,
            executed BOOLEAN NOT NULL DEFAULT false,
            created_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP
        )"
    )
    .execute(db)
    .await?;

    Ok(())
}