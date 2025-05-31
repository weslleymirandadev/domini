use serde::{Deserialize, Serialize};
use std::time::SystemTime;
use std::{
    collections::{HashMap, VecDeque},
    sync::Arc,
    time::Duration,
};
use tokio::sync::{RwLock, mpsc};
use tracing::{debug, info, warn};

pub type Uuid = String;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AgentInfo {
    pub ip: String,
    pub hostname: String,
    pub system: String,
    pub last_seen: SystemTime,
    pub active: bool,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct FullCommandResult {
    pub uuid: Uuid,
    pub result: String,
    pub current_working_directory: String,
    pub command_id: String,
}

#[derive(Clone)]
pub struct AgentChannels {
    pub command_sender: mpsc::Sender<String>,
    pub command_queue: Arc<RwLock<VecDeque<String>>>,
    pub result_sender: mpsc::Sender<Arc<FullCommandResult>>,
}

#[derive(Default)]
pub struct ServerState {
    pub agents: RwLock<HashMap<Uuid, AgentInfo>>,
    pub agent_channels: RwLock<HashMap<Uuid, AgentChannels>>,
    pub pending_results: RwLock<HashMap<Uuid, HashMap<String, Arc<FullCommandResult>>>>,
}

impl ServerState {
    pub fn new() -> Arc<Self> {
        Arc::new(ServerState {
            agents: RwLock::new(HashMap::new()),
            agent_channels: RwLock::new(HashMap::new()),
            pending_results: RwLock::new(HashMap::new()),
        })
    }

    pub async fn remove_agent(&self, uuid: &Uuid) {
        let mut agents = self.agents.write().await;
        let mut channels = self.agent_channels.write().await;

        if agents.remove(uuid).is_some() {
            info!("Agent {} removed from agents list", uuid);
        } else {
            warn!("Attempt to remove non-existent agent from agents list: {}", uuid);
        }

        if let Some(agent_channels) = channels.remove(uuid) {
            drop(agent_channels.result_sender);
            debug!("Agent {} channels removed and result sender dropped", uuid);
        } else {
            warn!("Attempt to remove channels for non-existent agent: {}", uuid);
        }
    }

    pub async fn cleanup_inactive_agents(&self) {
        let now = SystemTime::now();
        let mut to_remove_or_deactivate = Vec::new();

        {
            let agents= self.agents.read().await;
            for (uuid, agent) in agents.iter() {
                let time_since_last_seen = now.duration_since(agent.last_seen).unwrap_or_default();
                if time_since_last_seen > Duration::from_secs(300) {
                    if agent.active {
                        debug!(
                            "Agent {} marked for deactivation (inactive: {}s)",
                            uuid, time_since_last_seen.as_secs()
                        );
                        to_remove_or_deactivate.push((uuid.clone(), false));
                    } else if time_since_last_seen > Duration::from_secs(600) {
                        debug!(
                            "Agent {} marked for removal (inactive and inactive for too long: {}s)",
                            uuid, time_since_last_seen.as_secs()
                        );
                        to_remove_or_deactivate.push((uuid.clone(), true));
                    }
                }
            }
        }

        if !to_remove_or_deactivate.is_empty() {
            info!("Processing {} inactive/removal agents", to_remove_or_deactivate.len());
            for (uuid, should_remove) in to_remove_or_deactivate {
                if should_remove {
                    self.remove_agent(&uuid).await;
                    info!("Agent {} completely removed during cleanup", uuid);
                } else {
                    let mut agents = self.agents.write().await;
                    if let Some(agent) = agents.get_mut(&uuid) {
                        agent.active = false;
                        info!("Agent {} deactivated during cleanup", uuid);
                    }
                }
            }
        }
    }
}