use std::path::PathBuf;

use clap::{Parser, Subcommand};

#[derive(Parser, Debug)]
#[command(name = "CliGJ")]
#[command(about = "CliGJ core engine")]
pub struct Cli {
    /// After the window opens, read this UTF-8 file and write its bytes to the active terminal (ConPTY on Windows).
    #[arg(long, value_name = "PATH")]
    pub inject_file: Option<PathBuf>,

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
