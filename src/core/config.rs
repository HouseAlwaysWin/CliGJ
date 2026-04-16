use std::fs;
use std::path::PathBuf;

use super::error::{AppError, Result};

#[derive(Debug, Clone)]
pub struct AppConfig {
    pub data: toml::Table,
}

pub fn config_dir_path() -> Result<PathBuf> {
    let base = dirs::config_dir().ok_or(AppError::MissingConfigDir)?;
    Ok(base.join("cligj"))
}

pub fn config_file_path() -> Result<PathBuf> {
    Ok(config_dir_path()?.join("config.toml"))
}

impl AppConfig {
    pub fn load_or_default() -> Result<Self> {
        let path = config_file_path()?;
        if !path.exists() {
            return Ok(Self::default());
        }

        let content = fs::read_to_string(path)?;
        let value: toml::Value = toml::from_str(&content)?;
        let table = value.as_table().ok_or(AppError::InvalidConfigRoot)?.clone();
        Ok(Self { data: table })
    }

    pub fn ensure_file_exists(&self) -> Result<()> {
        let path = config_file_path()?;
        if path.exists() {
            return Ok(());
        }

        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }

        let encoded = toml::to_string_pretty(&toml::Value::Table(self.data.clone()))?;
        fs::write(path, encoded)?;
        Ok(())
    }

    pub fn save(&self) -> Result<()> {
        let path = config_file_path()?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let encoded = toml::to_string_pretty(&toml::Value::Table(self.data.clone()))?;
        fs::write(path, encoded)?;
        Ok(())
    }

    pub fn get_value(&self, key: &str) -> Result<Option<String>> {
        let segments = parse_key_path(key)?;
        let mut current: Option<&toml::Value> = None;

        for (index, segment) in segments.iter().enumerate() {
            let value = if index == 0 {
                self.data.get(*segment)
            } else {
                current
                    .and_then(toml::Value::as_table)
                    .and_then(|t| t.get(*segment))
            };

            match value {
                Some(v) => current = Some(v),
                None => return Ok(None),
            }
        }

        let rendered = current.map(value_to_string);
        Ok(rendered)
    }

    pub fn set_value(&mut self, key: &str, value: String) -> Result<()> {
        let segments = parse_key_path(key)?;
        set_path_value(&mut self.data, &segments, value);
        Ok(())
    }
}

impl Default for AppConfig {
    fn default() -> Self {
        let mut table = toml::Table::new();
        table.insert("tools".to_string(), toml::Value::Table(toml::Table::new()));
        table.insert("gemini".to_string(), toml::Value::Table(toml::Table::new()));
        table.insert("ui".to_string(), toml::Value::Table(toml::Table::new()));
        Self { data: table }
    }
}

fn parse_key_path(key: &str) -> Result<Vec<&str>> {
    let parts: Vec<&str> = key.split('.').collect();
    if parts.is_empty() || parts.iter().any(|p| p.trim().is_empty()) {
        return Err(AppError::InvalidConfigKey(key.to_string()));
    }
    Ok(parts)
}

fn set_path_value(table: &mut toml::Table, path: &[&str], value: String) {
    if path.len() == 1 {
        table.insert(path[0].to_string(), toml::Value::String(value));
        return;
    }

    let key = path[0].to_string();
    let entry = table
        .entry(key)
        .or_insert_with(|| toml::Value::Table(toml::Table::new()));

    if !entry.is_table() {
        *entry = toml::Value::Table(toml::Table::new());
    }

    if let Some(next) = entry.as_table_mut() {
        set_path_value(next, &path[1..], value);
    }
}

fn value_to_string(value: &toml::Value) -> String {
    match value {
        toml::Value::String(s) => s.clone(),
        toml::Value::Integer(i) => i.to_string(),
        toml::Value::Float(f) => f.to_string(),
        toml::Value::Boolean(b) => b.to_string(),
        toml::Value::Datetime(dt) => dt.to_string(),
        toml::Value::Array(_) | toml::Value::Table(_) => value.to_string(),
    }
}
