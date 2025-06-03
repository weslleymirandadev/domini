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
            online BOOLEAN NOT NULL DEFAULT false
        )"
    )
    .execute(db)
    .await?;
    Ok(())
}