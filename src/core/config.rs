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

    /// `[[ui.interactive_commands]]` — display `name` + shell `command` (all launcher rows, including former "presets").
    pub fn interactive_commands(&self) -> Vec<(String, String, usize)> {
        read_interactive_command_array(self, "interactive_commands")
    }

    /// Deprecated: `[[ui.interactive_custom_commands]]` — read only for migrating old files.
    pub fn interactive_custom_commands(&self) -> Vec<(String, String, usize)> {
        read_interactive_command_array(self, "interactive_custom_commands")
    }

    pub fn set_interactive_commands(&mut self, pairs: &[(String, String, usize)]) {
        let ui = self
            .data
            .entry("ui".to_string())
            .or_insert_with(|| toml::Value::Table(toml::Table::new()));
        if !ui.is_table() {
            *ui = toml::Value::Table(toml::Table::new());
        }
        let Some(ui_table) = ui.as_table_mut() else {
            return;
        };
        let arr: Vec<toml::Value> = pairs
            .iter()
            .filter(|(n, c, _)| !n.is_empty() && !c.is_empty())
            .map(|(n, c, pinned_footer_lines)| {
                let mut t = toml::Table::new();
                t.insert("name".to_string(), toml::Value::String(n.clone()));
                t.insert("command".to_string(), toml::Value::String(c.clone()));
                t.insert(
                    "pinned_footer_lines".to_string(),
                    toml::Value::Integer((*pinned_footer_lines).try_into().unwrap_or(i64::MAX)),
                );
                toml::Value::Table(t)
            })
            .collect();
        ui_table.insert("interactive_commands".to_string(), toml::Value::Array(arr));
        // Old key: presets lived in code; only customs were stored. Drop after migration to unified list.
        ui_table.remove("interactive_custom_commands");
    }

    /// `[[ui.shell_profiles]]` — `name`, `command`, optional `workspace` (root for `@` picker etc.).
    pub fn shell_profiles(&self) -> Vec<(String, String, String)> {
        read_shell_profiles_array(self)
    }

    pub fn set_shell_profiles(&mut self, entries: &[(String, String, String)]) {
        let ui = self
            .data
            .entry("ui".to_string())
            .or_insert_with(|| toml::Value::Table(toml::Table::new()));
        if !ui.is_table() {
            *ui = toml::Value::Table(toml::Table::new());
        }
        let Some(ui_table) = ui.as_table_mut() else {
            return;
        };
        let arr: Vec<toml::Value> = entries
            .iter()
            .filter(|(n, c, _)| !n.is_empty() && !c.is_empty())
            .map(|(n, c, w)| {
                let mut t = toml::Table::new();
                t.insert("name".to_string(), toml::Value::String(n.clone()));
                t.insert("command".to_string(), toml::Value::String(c.clone()));
                let wt = w.trim();
                if !wt.is_empty() {
                    t.insert("workspace".to_string(), toml::Value::String(wt.to_string()));
                }
                toml::Value::Table(t)
            })
            .collect();
        ui_table.insert("shell_profiles".to_string(), toml::Value::Array(arr));
    }

    pub fn ui_language(&self) -> Option<String> {
        self.data
            .get("ui")
            .and_then(|v| v.as_table())
            .and_then(|t| t.get("language"))
            .and_then(|v| v.as_str())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    }

    pub fn default_shell_profile(&self) -> Option<String> {
        self.data
            .get("ui")
            .and_then(|v| v.as_table())
            .and_then(|t| t.get("default_shell_profile"))
            .and_then(|v| v.as_str())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    }

    pub fn terminal_font_family(&self) -> Option<String> {
        self.data
            .get("ui")
            .and_then(|v| v.as_table())
            .and_then(|t| t.get("terminal_font_family"))
            .and_then(|v| v.as_str())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    }

    pub fn terminal_cjk_fallback_font_family(&self) -> Option<String> {
        self.data
            .get("ui")
            .and_then(|v| v.as_table())
            .and_then(|t| t.get("terminal_cjk_fallback_font_family"))
            .and_then(|v| v.as_str())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    }

    pub fn set_ui_language(&mut self, language: &str) {
        let ui = self
            .data
            .entry("ui".to_string())
            .or_insert_with(|| toml::Value::Table(toml::Table::new()));
        if !ui.is_table() {
            *ui = toml::Value::Table(toml::Table::new());
        }
        let Some(ui_table) = ui.as_table_mut() else {
            return;
        };
        ui_table.insert(
            "language".to_string(),
            toml::Value::String(language.trim().to_string()),
        );
    }

    pub fn set_default_shell_profile(&mut self, profile: &str) {
        let ui = self
            .data
            .entry("ui".to_string())
            .or_insert_with(|| toml::Value::Table(toml::Table::new()));
        if !ui.is_table() {
            *ui = toml::Value::Table(toml::Table::new());
        }
        let Some(ui_table) = ui.as_table_mut() else {
            return;
        };
        ui_table.insert(
            "default_shell_profile".to_string(),
            toml::Value::String(profile.trim().to_string()),
        );
    }

    pub fn set_terminal_font_family(&mut self, family: &str) {
        let ui = self
            .data
            .entry("ui".to_string())
            .or_insert_with(|| toml::Value::Table(toml::Table::new()));
        if !ui.is_table() {
            *ui = toml::Value::Table(toml::Table::new());
        }
        let Some(ui_table) = ui.as_table_mut() else {
            return;
        };
        ui_table.insert(
            "terminal_font_family".to_string(),
            toml::Value::String(family.trim().to_string()),
        );
    }

    pub fn set_terminal_cjk_fallback_font_family(&mut self, family: &str) {
        let ui = self
            .data
            .entry("ui".to_string())
            .or_insert_with(|| toml::Value::Table(toml::Table::new()));
        if !ui.is_table() {
            *ui = toml::Value::Table(toml::Table::new());
        }
        let Some(ui_table) = ui.as_table_mut() else {
            return;
        };
        ui_table.insert(
            "terminal_cjk_fallback_font_family".to_string(),
            toml::Value::String(family.trim().to_string()),
        );
    }
}

fn read_shell_profiles_array(cfg: &AppConfig) -> Vec<(String, String, String)> {
    let Some(ui) = cfg.data.get("ui").and_then(|v| v.as_table()) else {
        return Vec::new();
    };
    let Some(arr) = ui.get("shell_profiles").and_then(|v| v.as_array()) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for item in arr {
        let Some(t) = item.as_table() else {
            continue;
        };
        let name = t
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_string();
        let command = t
            .get("command")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_string();
        let workspace = t
            .get("workspace")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_string();
        if !name.is_empty() && !command.is_empty() {
            out.push((name, command, workspace));
        }
    }
    out
}

fn read_interactive_command_array(cfg: &AppConfig, key: &str) -> Vec<(String, String, usize)> {
    let Some(ui) = cfg.data.get("ui").and_then(|v| v.as_table()) else {
        return Vec::new();
    };
    let Some(arr) = ui.get(key).and_then(|v| v.as_array()) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for item in arr {
        let Some(t) = item.as_table() else {
            continue;
        };
        let name = t
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_string();
        let command = t
            .get("command")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_string();
        let pinned_footer_lines = t
            .get("pinned_footer_lines")
            .and_then(interactive_pinned_footer_lines_value)
            .unwrap_or_else(|| default_interactive_pinned_footer_lines(&name, &command));
        if !name.is_empty() && !command.is_empty() {
            out.push((name, command, pinned_footer_lines));
        }
    }
    out
}

fn interactive_pinned_footer_lines_value(value: &toml::Value) -> Option<usize> {
    if let Some(n) = value.as_integer() {
        return usize::try_from(n).ok();
    }
    value
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .and_then(|s| s.parse::<usize>().ok())
}

fn normalize_interactive_program_name(text: &str) -> String {
    let trimmed = text.trim().trim_matches(|c| c == '"' || c == '\'');
    let leaf = trimmed.rsplit(['\\', '/']).next().unwrap_or(trimmed);
    leaf.strip_suffix(".exe").unwrap_or(leaf).to_ascii_lowercase()
}

fn default_interactive_pinned_footer_lines(name: &str, command: &str) -> usize {
    let name = normalize_interactive_program_name(name);
    let command = command
        .split_whitespace()
        .next()
        .map(normalize_interactive_program_name)
        .unwrap_or_default();
    if name == "gemini" || command == "gemini" {
        8
    } else {
        0
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
