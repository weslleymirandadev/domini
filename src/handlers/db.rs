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
            online BOOLEAN NOT NULL DEFAULT false,
            last_seen TIMESTAMP WITH TIME ZONE NOT NULL DEFAULT CURRENT_TIMESTAMP
        )"
    )
    .execute(db)
    .await?;

    sqlx::query(
        "CREATE TABLE IF NOT EXISTS agent_version (
            id SERIAL PRIMARY KEY,
            version TEXT NOT NULL UNIQUE,
            \"binary\" BYTEA NOT NULL,
            updated_at TIMESTAMP WITH TIME ZONE NOT NULL DEFAULT CURRENT_TIMESTAMP
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
            created_at TIMESTAMP WITH TIME ZONE NOT NULL DEFAULT CURRENT_TIMESTAMP,
            expires_at TIMESTAMP WITH TIME ZONE,
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
            execute_at TIMESTAMP WITH TIME ZONE NOT NULL,
            executed BOOLEAN NOT NULL DEFAULT false,
            created_at TIMESTAMP WITH TIME ZONE NOT NULL DEFAULT CURRENT_TIMESTAMP
        )"
    )
    .execute(db)
    .await?;

    sqlx::query(
        "DO $$ 
        BEGIN 
            IF NOT EXISTS (
                SELECT 1 
                FROM information_schema.columns 
                WHERE table_name = 'scheduled_tasks' 
                AND column_name = 'executed'
            ) THEN
                ALTER TABLE scheduled_tasks ADD COLUMN executed BOOLEAN NOT NULL DEFAULT false;
            END IF;
        END $$;"
    )
    .execute(db)
    .await?;

    sqlx::query(
        "DO $$ 
        BEGIN 
            IF EXISTS (
                SELECT 1 
                FROM information_schema.columns 
                WHERE table_name = 'scheduled_tasks' 
                AND column_name = 'execute_at' 
                AND data_type = 'timestamp without time zone'
            ) THEN
                ALTER TABLE scheduled_tasks 
                ALTER COLUMN execute_at TYPE TIMESTAMP WITH TIME ZONE 
                USING execute_at AT TIME ZONE 'UTC';
            END IF;
        END $$;"
    )
    .execute(db)
    .await?;

    Ok(())
}