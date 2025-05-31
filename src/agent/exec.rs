use std::{env, path::PathBuf, process::Command};

use tracing::info;

pub async fn execute_command_and_get_cwd(command: &str) -> (String, String) {
    info!("Executing command: {}", command);

    let result_output;

    if command.trim().starts_with("cd ") {
        let path_str = command.trim_start_matches("cd ").trim();
        
        let target_path = if path_str == "~" {
            dirs::home_dir().unwrap_or_else(|| PathBuf::from("."))
        } else if path_str.starts_with("~/") {
            dirs::home_dir().map_or_else(
                || PathBuf::from(path_str),
                |p| p.join(path_str.trim_start_matches("~/"))
            )
        } else {
            PathBuf::from(path_str)
        };

        match env::set_current_dir(&target_path) {
            Ok(_) => {
                result_output = format!("");
            },
            Err(e) => {
                result_output = format!("Error changing directory to {}: {}", target_path.display(), e);
            }
        }
    } else {
        // Para outros comandos, continuamos a executar via shell
        let shell = if cfg!(target_os = "windows") {
            "cmd.exe"
        } else {
            "sh" // Ou "bash", mas 'sh' é mais universal
        };
        let shell_arg = if cfg!(target_os = "windows") {
            "/C"
        } else {
            "-c"
        };

        match Command::new(shell)
            .arg(shell_arg)
            .arg(command)
            .output()
        {
            Ok(output) => {
                let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
                let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();

                if output.status.success() {
                    result_output = if stdout.is_empty() {
                        "(No output)".to_string()
                    } else {
                        stdout
                    };
                } else {
                    result_output = format!(
                        "Error executing command (exit code {}): {}",
                        output.status.code().unwrap_or(-1),
                        if stderr.is_empty() { "(No stderr output)" } else { &stderr }
                    );
                }
            }
            Err(e) => {
                result_output = format!("Error executing command: {}", e);
            }
        }
    }

    let current_working_directory = env::current_dir()
        .map_or_else(|_| "(unknown directory)".to_string(), |p| p.display().to_string());

    (result_output, current_working_directory)
}