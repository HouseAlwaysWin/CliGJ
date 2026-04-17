mod core;
mod gui;
mod terminal;
mod terminal_v2;
mod workspace_files;

use crate::core::cli::{Cli, Commands, ConfigCommand};
use crate::core::config::AppConfig;
use crate::core::error::Result;
use crate::core::runner;
use clap::Parser;

#[tokio::main]
async fn main() {
    if let Err(err) = run().await {
        eprintln!("Error: {err}");
        std::process::exit(1);
    }
}

async fn run() -> Result<()> {
    let cli = Cli::parse();
    dispatch(cli).await
}

async fn dispatch(cli: Cli) -> Result<()> {
    match cli.command {
        Some(Commands::Run { command }) => {
            let command_line = command.join(" ");
            let output = runner::run_shell_command(&command_line).await?;
            runner::print_outcome(&output);
        }
        Some(Commands::Chain { cmd }) => {
            let output = runner::run_command_stack(&cmd).await?;
            for item in output {
                runner::print_outcome(&item);
            }
        }
        Some(Commands::Config { command }) => {
            let mut config = AppConfig::load_or_default()?;
            match command {
                ConfigCommand::Init => {
                    config.ensure_file_exists()?;
                    let path = crate::core::config::config_file_path()?;
                    println!("Config initialized at {}", path.display());
                }
                ConfigCommand::Get { key } => {
                    if let Some(value) = config.get_value(&key)? {
                        println!("{value}");
                    } else {
                        println!("<not set>");
                    }
                }
                ConfigCommand::Set { key, value } => {
                    config.set_value(&key, value)?;
                    config.save()?;
                    println!("Saved {key}");
                }
            }
        }
        None => gui::run_gui(cli.inject_file),
    }

    Ok(())
}
