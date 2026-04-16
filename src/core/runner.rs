use tokio::process::Command;

use super::error::{AppError, Result};

#[derive(Debug)]
pub struct CommandOutcome {
    pub command: String,
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

pub async fn run_shell_command(command_line: &str) -> Result<CommandOutcome> {
    let output = if cfg!(target_os = "windows") {
        Command::new("cmd")
            .args(["/C", command_line])
            .output()
            .await?
    } else {
        Command::new("sh")
            .args(["-c", command_line])
            .output()
            .await?
    };

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    let status = output.status.code();

    if !output.status.success() {
        return Err(AppError::CommandFailed {
            command: command_line.to_string(),
            code: status,
            stderr,
        });
    }

    Ok(CommandOutcome {
        command: command_line.to_string(),
        exit_code: status,
        stdout,
        stderr,
    })
}

pub async fn run_command_stack(commands: &[String]) -> Result<Vec<CommandOutcome>> {
    let mut results = Vec::with_capacity(commands.len());
    for command in commands {
        let output = run_shell_command(command).await?;
        results.push(output);
    }
    Ok(results)
}

pub fn print_outcome(outcome: &CommandOutcome) {
    println!("> {}", outcome.command);
    println!("exit: {:?}", outcome.exit_code);

    if !outcome.stdout.trim().is_empty() {
        println!("stdout:\n{}", outcome.stdout.trim_end());
    }
    if !outcome.stderr.trim().is_empty() {
        eprintln!("stderr:\n{}", outcome.stderr.trim_end());
    }
}
