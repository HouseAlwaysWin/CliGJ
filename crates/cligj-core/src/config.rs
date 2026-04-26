use std::fs;
use std::path::PathBuf;

use super::error::{AppError, Result};

#[derive(Debug, Clone)]
pub struct AppConfig {
    pub data: toml::Table,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InteractiveCommandConfig {
    pub name: String,
    pub command: String,
    pub interactive_cli: bool,
    pub pinned_footer_lines: usize,
    pub markers: Vec<String>,
    pub archive_repainted_frames: bool,
}

impl InteractiveCommandConfig {
    pub fn with_defaults(name: String, command: String, pinned_footer_lines: usize) -> Self {
        Self {
            interactive_cli: true,
            markers: default_interactive_markers(&name, &command),
            archive_repainted_frames: default_interactive_archive_repainted_frames(&name, &command),
            name,
            command,
            pinned_footer_lines,
        }
    }
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

    /// `[[ui.interactive_commands]]` — display `name` + shell `command` plus runtime detection rules.
    pub fn interactive_commands(&self) -> Vec<InteractiveCommandConfig> {
        read_interactive_command_array(self, "interactive_commands")
    }

    /// Deprecated: `[[ui.interactive_custom_commands]]` — read only for migrating old files.
    pub fn interactive_custom_commands(&self) -> Vec<InteractiveCommandConfig> {
        read_interactive_command_array(self, "interactive_custom_commands")
    }

    pub fn set_interactive_commands(&mut self, pairs: &[InteractiveCommandConfig]) {
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
            .filter(|spec| !spec.name.is_empty() && !spec.command.is_empty())
            .map(|spec| {
                let mut t = toml::Table::new();
                t.insert("name".to_string(), toml::Value::String(spec.name.clone()));
                t.insert(
                    "command".to_string(),
                    toml::Value::String(spec.command.clone()),
                );
                t.insert(
                    "interactive_cli".to_string(),
                    toml::Value::Boolean(spec.interactive_cli),
                );
                t.insert(
                    "pinned_footer_lines".to_string(),
                    toml::Value::Integer(spec.pinned_footer_lines.try_into().unwrap_or(i64::MAX)),
                );
                t.insert(
                    "markers".to_string(),
                    toml::Value::Array(
                        spec.markers
                            .iter()
                            .filter(|marker| !marker.trim().is_empty())
                            .map(|marker| toml::Value::String(marker.trim().to_string()))
                            .collect(),
                    ),
                );
                t.insert(
                    "archive_repainted_frames".to_string(),
                    toml::Value::Boolean(spec.archive_repainted_frames),
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

fn read_interactive_command_array(cfg: &AppConfig, key: &str) -> Vec<InteractiveCommandConfig> {
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
            let interactive_cli = t
                .get("interactive_cli")
                .and_then(interactive_bool_value)
                .unwrap_or(true);
            let markers = t
                .get("markers")
                .map(interactive_marker_values)
                .unwrap_or_else(|| default_interactive_markers(&name, &command));
            let archive_repainted_frames = t
                .get("archive_repainted_frames")
                .and_then(interactive_bool_value)
                .unwrap_or_else(|| default_interactive_archive_repainted_frames(&name, &command));
            out.push(InteractiveCommandConfig {
                name,
                command,
                interactive_cli,
                pinned_footer_lines,
                markers,
                archive_repainted_frames,
            });
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

fn interactive_marker_values(value: &toml::Value) -> Vec<String> {
    if let Some(arr) = value.as_array() {
        return arr
            .iter()
            .filter_map(|item| item.as_str())
            .map(str::trim)
            .filter(|item| !item.is_empty())
            .map(ToString::to_string)
            .collect();
    }
    value
        .as_str()
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .map(|item| vec![item.to_string()])
        .unwrap_or_default()
}

fn interactive_bool_value(value: &toml::Value) -> Option<bool> {
    if let Some(value) = value.as_bool() {
        return Some(value);
    }
    match value.as_str()?.trim().to_ascii_lowercase().as_str() {
        "true" | "1" | "yes" | "on" => Some(true),
        "false" | "0" | "no" | "off" => Some(false),
        _ => None,
    }
}

fn normalize_interactive_program_name(text: &str) -> String {
    let trimmed = text.trim().trim_matches(|c| c == '"' || c == '\'');
    let leaf = trimmed.rsplit(['\\', '/']).next().unwrap_or(trimmed);
    leaf.strip_suffix(".exe")
        .unwrap_or(leaf)
        .to_ascii_lowercase()
}

fn interactive_program_name(name: &str, command: &str) -> String {
    command
        .split_whitespace()
        .next()
        .map(normalize_interactive_program_name)
        .filter(|program| !program.is_empty())
        .unwrap_or_else(|| normalize_interactive_program_name(name))
}

fn default_interactive_pinned_footer_lines(name: &str, command: &str) -> usize {
    if interactive_program_name(name, command) == "gemini" {
        8
    } else {
        0
    }
}

fn default_interactive_markers(name: &str, command: &str) -> Vec<String> {
    match interactive_program_name(name, command).as_str() {
        "gemini" => vec![
            "gemini cli",
            "waiting for authentication",
            "signed in with google",
            "about gemini",
            "gemini.md",
            "? for shortcuts",
            "type your message",
        ],
        "codex" => vec![
            "openai codex",
            ">_ openai codex",
            "implement {feature}",
            "/model to change",
        ],
        "claude" => vec!["claude"],
        "copilot" => vec!["copilot"],
        _ => Vec::new(),
    }
    .into_iter()
    .map(ToString::to_string)
    .collect()
}

fn default_interactive_archive_repainted_frames(name: &str, command: &str) -> bool {
    let _ = (name, command);
    false
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

#[cfg(test)]
mod tests {
    use super::*;

    fn config_from_toml(input: &str) -> AppConfig {
        let value: toml::Value = toml::from_str(input).unwrap();
        AppConfig {
            data: value.as_table().unwrap().clone(),
        }
    }

    #[test]
    fn interactive_command_reads_custom_markers_and_repaint_policy() {
        let cfg = config_from_toml(
            r#"
            [[ui.interactive_commands]]
            name = "Acme AI"
            command = "acme-ai --tui"
            interactive_cli = true
            pinned_footer_lines = 2
            markers = ["Acme AI", "Ready for input"]
            archive_repainted_frames = true
            "#,
        );

        let commands = cfg.interactive_commands();
        assert_eq!(commands.len(), 1);
        assert_eq!(commands[0].name, "Acme AI");
        assert_eq!(commands[0].command, "acme-ai --tui");
        assert!(commands[0].interactive_cli);
        assert_eq!(commands[0].pinned_footer_lines, 2);
        assert_eq!(
            commands[0].markers,
            vec!["Acme AI".to_string(), "Ready for input".to_string()]
        );
        assert!(commands[0].archive_repainted_frames);
    }

    #[test]
    fn interactive_command_applies_builtin_codex_defaults_when_missing() {
        let cfg = config_from_toml(
            r#"
            [[ui.interactive_commands]]
            name = "Codex"
            command = "codex"
            "#,
        );

        let commands = cfg.interactive_commands();
        assert_eq!(commands.len(), 1);
        assert!(commands[0].interactive_cli);
        assert!(
            commands[0]
                .markers
                .iter()
                .any(|marker| marker == "openai codex")
        );
        assert!(!commands[0].archive_repainted_frames);
    }

    #[test]
    fn interactive_command_can_disable_interactive_cli_route() {
        let cfg = config_from_toml(
            r#"
            [[ui.interactive_commands]]
            name = "Build"
            command = "cargo build"
            interactive_cli = false
            "#,
        );

        let commands = cfg.interactive_commands();
        assert_eq!(commands.len(), 1);
        assert!(!commands[0].interactive_cli);
    }
}
