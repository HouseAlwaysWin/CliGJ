use clap::{Parser, Subcommand};

#[derive(Parser, Debug)]
#[command(name = "CliGJ")]
#[command(about = "CliGJ core engine")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Commands>,
}

#[derive(Subcommand, Debug)]
pub enum Commands {
    /// Run a single shell command
    Run {
        /// Command text (use `--` before command tokens)
        #[arg(required = true, trailing_var_arg = true)]
        command: Vec<String>,
    },
    /// Run multiple commands in sequence
    Chain {
        /// Command entries to run in order
        #[arg(long = "cmd", required = true)]
        cmd: Vec<String>,
    },
    /// Manage CliGJ config values
    Config {
        #[command(subcommand)]
        command: ConfigCommand,
    },
}

#[derive(Subcommand, Debug)]
pub enum ConfigCommand {
    /// Create config file with defaults
    Init,
    /// Read a config value
    Get {
        /// Config key path (supports dot path, e.g. tools.ffmpeg_path)
        key: String,
    },
    /// Set a config value
    Set {
        /// Config key path (supports dot path, e.g. tools.ffmpeg_path)
        key: String,
        value: String,
    },
}

pub fn has_cli_args() -> bool {
    std::env::args_os().nth(1).is_some()
}
