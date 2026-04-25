use std::error::Error as StdError;
use std::fmt::{Display, Formatter};

pub type Result<T> = std::result::Result<T, AppError>;

#[derive(Debug)]
pub enum AppError {
    Io(std::io::Error),
    TomlDeserialize(toml::de::Error),
    TomlSerialize(toml::ser::Error),
    MissingConfigDir,
    InvalidConfigKey(String),
    InvalidConfigRoot,
    CommandFailed {
        command: String,
        code: Option<i32>,
        stderr: String,
    },
}

impl Display for AppError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            AppError::Io(err) => write!(f, "I/O error: {err}"),
            AppError::TomlDeserialize(err) => write!(f, "Invalid config TOML: {err}"),
            AppError::TomlSerialize(err) => write!(f, "Failed to encode config TOML: {err}"),
            AppError::MissingConfigDir => write!(f, "Unable to resolve config directory"),
            AppError::InvalidConfigKey(key) => write!(f, "Invalid config key path: `{key}`"),
            AppError::InvalidConfigRoot => {
                write!(f, "Config root must be a TOML table")
            }
            AppError::CommandFailed {
                command,
                code,
                stderr,
            } => write!(
                f,
                "Command failed: `{command}` (exit: {:?}) {}",
                code,
                stderr.trim()
            ),
        }
    }
}

impl StdError for AppError {}

impl From<std::io::Error> for AppError {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value)
    }
}

impl From<toml::de::Error> for AppError {
    fn from(value: toml::de::Error) -> Self {
        Self::TomlDeserialize(value)
    }
}

impl From<toml::ser::Error> for AppError {
    fn from(value: toml::ser::Error) -> Self {
        Self::TomlSerialize(value)
    }
}
