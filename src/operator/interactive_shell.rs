use std::{collections::HashMap, io::{self, Write}};
use reqwest::Client;
use tracing::{error, info};

use crate::operator::send_command::send_command;

pub async fn interactive_shell(
    client: &Client,
    selected_agent_uuid: &str,
    current_agent_cwds: &mut HashMap<String, String>,
) -> Result<(), String> {
    info!("Entering interactive mode for agent {}", selected_agent_uuid);

    let mut current_cwd = if let Some(cached_cwd) = current_agent_cwds.get(selected_agent_uuid) {
        cached_cwd.clone()
    } else {
        match send_command(client, selected_agent_uuid, "pwd", true).await {
            Ok(Some(result)) => {
                let cwd = result.current_working_directory;
                current_agent_cwds.insert(selected_agent_uuid.to_string(), cwd.clone());
                cwd
            }
            _ => {
                error!("Could not get initial agent CWD.");
                "?".to_string()
            }
        }
    };

    loop {
        print!("({}@domini)[{}] $ ", selected_agent_uuid.split("-").next().unwrap_or("?"), current_cwd);
        io::stdout()
            .flush()
            .map_err(|e| format!("Error flushing stdout: {}", e))?;

        let mut input = String::new();
        io::stdin()
            .read_line(&mut input)
            .map_err(|e| format!("Error reading input: {}", e))?;
        let cmd = input.trim();

        if cmd.is_empty() {
            continue;
        }
        if cmd == "exit" {
            break Ok(());
        }

        match send_command(client, selected_agent_uuid, cmd, true).await {
            Ok(Some(result)) => {
                current_cwd = result.current_working_directory;
                current_agent_cwds.insert(selected_agent_uuid.to_string(), current_cwd.clone());
            }
            Ok(None) => {
                match send_command(client, selected_agent_uuid, "pwd", true).await {
                    Ok(Some(pwd_result)) => {
                        current_cwd = pwd_result.current_working_directory;
                        current_agent_cwds.insert(selected_agent_uuid.to_string(), current_cwd.clone());
                    }
                    _ => {
                        info!("Command executed, but could not update CWD.");
                    }
                }
            }
            Err(e) => {
                error!("Error executing interactive command: {}", e);
            }
        }
    }
}
