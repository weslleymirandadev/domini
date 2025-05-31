use serde::{Deserialize, Serialize};

type Uuid = String;

// handlers/results.rs
#[derive(Serialize, Deserialize)]
pub struct ResultPayload {
    pub uuid: Option<Uuid>,
    pub result: Option<String>,
    pub current_working_directory: Option<String>,
    pub command_id: Option<String>,
}

// handlers/commands.rs
#[derive(Serialize, Deserialize, Clone)]
pub struct CommandPayload {
    pub uuid: Option<Uuid>,
    pub command: Option<String>,
    pub wait_response: Option<bool>,
    pub command_id: String,
}

// handlers/commands.rs
#[derive(Serialize, Deserialize, Clone)]
pub struct BroadcastPayload {
    pub command: Option<String>,
    pub wait_response: Option<bool>,
    pub command_id: String,
}

// handlers/agents.rs
#[derive(Serialize, Deserialize)]
pub struct RegisterPayload {
    pub uuid: Uuid,
    pub ip: String,
    pub hostname: String,
    pub system: String,
}

// handlers/agents.rs
#[derive(Deserialize)]
pub struct DeregisterParams {
    pub uuid: Uuid,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct Agent {
    pub uuid: String,
    pub ip: String,
    pub hostname: String,
    pub system: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct FullCommandResult {
    pub uuid: String,
    pub result: String,
    pub current_working_directory: String,
    pub command_id: String,
}