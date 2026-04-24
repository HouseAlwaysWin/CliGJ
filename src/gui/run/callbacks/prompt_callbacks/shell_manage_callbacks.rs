use std::cell::RefCell;
use std::collections::HashSet;
use std::fs::{self, File};
use std::io::copy;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::rc::Rc;
use std::thread;
use std::time::Duration;

use serde::Deserialize;
use slint::{ComponentHandle, Model, ModelRc, SharedString, VecModel};

use crate::core::config::AppConfig;
use crate::gui::fonts::{
    normalize_terminal_cjk_fallback_font_family, normalize_terminal_font_family,
};
use crate::gui::i18n::apply_slint_language_from_shell_setting;
use crate::gui::shell_profiles::{
    default_shell_profile_name, normalize_shell_profile_command, sync_shell_manage_editor_to_ui,
    sync_shell_profile_choices_to_ui,
};
use crate::gui::slint_ui::{AppWindow, InteractiveCmdEditorRow};
use crate::gui::state::GuiState;

use super::super::{model_interactive_editor_rows, set_shell_manage_rows};

const APP_GITHUB_URL: &str = "https://github.com/HouseAlwaysWin/CliGJ";
const APP_AUTHOR: &str = "HouseAlwaysWin";
const RELEASES_API_URL: &str = "https://api.github.com/repos/HouseAlwaysWin/CliGJ/releases?per_page=20";
const MAX_UPDATE_VERSIONS: usize = 10;
const APP_VERSION: &str = match option_env!("CLIGJ_APP_VERSION") {
    Some(v) => v,
    None => env!("CARGO_PKG_VERSION"),
};

#[derive(Debug, Clone, Deserialize)]
struct GithubReleaseAsset {
    name: String,
    browser_download_url: String,
}

#[derive(Debug, Clone, Deserialize)]
struct GithubRelease {
    tag_name: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    assets: Vec<GithubReleaseAsset>,
}

fn open_url_in_browser(url: &str) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    {
        let status = Command::new("cmd")
            .args(["/C", "start", "", url])
            .status()
            .map_err(|e| format!("open browser failed: {e}"))?;
        if !status.success() {
            return Err(format!("open browser returned status: {status}"));
        }
        Ok(())
    }
    #[cfg(target_os = "macos")]
    {
        let status = Command::new("open")
            .arg(url)
            .status()
            .map_err(|e| format!("open browser failed: {e}"))?;
        if !status.success() {
            return Err(format!("open browser returned status: {status}"));
        }
        Ok(())
    }
    #[cfg(all(not(target_os = "windows"), not(target_os = "macos")))]
    {
        let status = Command::new("xdg-open")
            .arg(url)
            .status()
            .map_err(|e| format!("open browser failed: {e}"))?;
        if !status.success() {
            return Err(format!("open browser returned status: {status}"));
        }
        Ok(())
    }
}

#[cfg(target_os = "windows")]
fn normalize_version_tag(tag: &str) -> String {
    tag.trim().trim_start_matches('v').to_ascii_lowercase()
}

#[cfg(target_os = "windows")]
fn fetch_releases() -> Result<Vec<GithubRelease>, String> {
    let client = reqwest::blocking::Client::builder()
        .build()
        .map_err(|e| format!("http client error: {e}"))?;
    let resp = client
        .get(RELEASES_API_URL)
        .header("User-Agent", "CliGJ")
        .send()
        .map_err(|e| format!("request failed: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("request failed: HTTP {}", resp.status()));
    }
    let payload: Vec<GithubRelease> = resp
        .json()
        .map_err(|e| format!("parse releases failed: {e}"))?;
    Ok(payload)
}

fn fetch_release_versions() -> Result<Vec<String>, String> {
    let payload = fetch_releases()?;
    let mut out = Vec::<String>::new();
    for release in payload {
        if release.draft || release.prerelease {
            continue;
        }
        let tag = release.tag_name.trim();
        if tag.is_empty() {
            continue;
        }
        let tag = tag.to_string();
        if !out.iter().any(|v| v == &tag) {
            out.push(tag);
        }
    }
    if out.is_empty() {
        out.push(APP_VERSION.to_string());
    } else {
        out.truncate(MAX_UPDATE_VERSIONS);
    }
    Ok(out)
}

fn set_update_versions_to_ui(ui: &AppWindow, versions: &[String], preferred: Option<&str>) {
    ui.set_ws_update_versions(ModelRc::new(VecModel::from(
        versions
            .iter()
            .map(|v| SharedString::from(v.as_str()))
            .collect::<Vec<_>>(),
    )));
    let selected = preferred
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .filter(|s| versions.iter().any(|v| v == s))
        .map(ToOwned::to_owned)
        .or_else(|| versions.first().cloned())
        .unwrap_or_else(|| APP_VERSION.to_string());
    ui.set_ws_update_selected_version(SharedString::from(selected.as_str()));
}

fn refresh_available_versions(app_weak: slint::Weak<AppWindow>, preferred: Option<String>) {
    let preferred_outer = preferred.clone();
    let _ = app_weak.upgrade_in_event_loop(move |ui| {
        ui.set_ws_update_status_text(SharedString::from("Checking updates..."));
        if let Some(p) = preferred_outer.as_deref() {
            ui.set_ws_update_selected_version(SharedString::from(p));
        }
    });
    thread::spawn(move || {
        let fetched = fetch_release_versions();
        let _ = app_weak.upgrade_in_event_loop(move |ui| match fetched {
            Ok(versions) => {
                set_update_versions_to_ui(&ui, &versions, preferred.as_deref());
                ui.set_ws_update_status_text(SharedString::from(
                    format!("Found {} available versions", versions.len()).as_str(),
                ));
            }
            Err(e) => {
                set_update_versions_to_ui(&ui, &[APP_VERSION.to_string()], preferred.as_deref());
                ui.set_ws_update_status_text(SharedString::from(
                    format!("Update check failed: {e}").as_str(),
                ));
            }
        });
    });
}

#[cfg(target_os = "windows")]
fn choose_windows_release_asset(release: &GithubRelease) -> Option<&GithubReleaseAsset> {
    release
        .assets
        .iter()
        .find(|a| a.name.ends_with("-windows-x64.zip"))
        .or_else(|| release.assets.iter().find(|a| a.name.ends_with(".zip")))
}

#[cfg(target_os = "windows")]
fn resolve_selected_release_asset(version: &str) -> Result<GithubReleaseAsset, String> {
    let target = normalize_version_tag(version);
    let releases = fetch_releases()?;
    for release in releases {
        if release.draft || release.prerelease {
            continue;
        }
        let release_tag_norm = normalize_version_tag(release.tag_name.as_str());
        if release_tag_norm != target {
            continue;
        }
        if let Some(asset) = choose_windows_release_asset(&release) {
            return Ok(asset.clone());
        }
        return Err(format!(
            "Version {} has no Windows .zip asset in release",
            release.tag_name
        ));
    }
    Err(format!("Version {version} not found in GitHub releases"))
}

#[cfg(target_os = "windows")]
fn download_release_asset_to_temp(
    asset: &GithubReleaseAsset,
    version: &str,
) -> Result<PathBuf, String> {
    let tmp_root = std::env::temp_dir().join("cligj-updates");
    fs::create_dir_all(&tmp_root).map_err(|e| format!("create temp folder failed: {e}"))?;
    let file_name = if asset.name.trim().is_empty() {
        format!("CliGJ-{version}-windows-x64.zip")
    } else {
        asset.name.clone()
    };
    let target_path = tmp_root.join(file_name);
    let mut resp = reqwest::blocking::Client::builder()
        .build()
        .map_err(|e| format!("http client error: {e}"))?
        .get(asset.browser_download_url.as_str())
        .header("User-Agent", "CliGJ")
        .send()
        .map_err(|e| format!("download request failed: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("download failed: HTTP {}", resp.status()));
    }
    let mut out =
        File::create(&target_path).map_err(|e| format!("create file failed: {e}"))?;
    copy(&mut resp, &mut out).map_err(|e| format!("write file failed: {e}"))?;
    Ok(target_path)
}

#[cfg(target_os = "windows")]
fn find_first_exe(dir: &Path) -> Option<PathBuf> {
    let entries = fs::read_dir(dir).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if let Some(found) = find_first_exe(path.as_path()) {
                return Some(found);
            }
            continue;
        }
        if path
            .extension()
            .and_then(|s| s.to_str())
            .map(|s| s.eq_ignore_ascii_case("exe"))
            .unwrap_or(false)
        {
            return Some(path);
        }
    }
    None
}

#[cfg(target_os = "windows")]
fn extract_release_zip_to_temp(zip_path: &Path, version: &str) -> Result<PathBuf, String> {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|e| format!("clock error: {e}"))?
        .as_millis();
    let output_dir = std::env::temp_dir()
        .join("cligj-updates")
        .join(format!("extract-{version}-{stamp}"));
    fs::create_dir_all(&output_dir).map_err(|e| format!("create extract folder failed: {e}"))?;
    let status = Command::new("tar")
        .arg("-xf")
        .arg(zip_path)
        .arg("-C")
        .arg(&output_dir)
        .status()
        .map_err(|e| format!("extract zip failed: {e}"))?;
    if !status.success() {
        return Err(format!("extract zip returned status: {status}"));
    }
    find_first_exe(output_dir.as_path())
        .ok_or_else(|| format!("no .exe found after extracting {}", zip_path.display()))
}

#[cfg(target_os = "windows")]
fn launch_windows_self_replace_updater(downloaded_exe: &Path) -> Result<(), String> {
    let current_exe =
        std::env::current_exe().map_err(|e| format!("resolve current executable failed: {e}"))?;
    let updates_root = std::env::temp_dir().join("cligj-updates");
    fs::create_dir_all(&updates_root).map_err(|e| format!("create temp folder failed: {e}"))?;
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|e| format!("clock error: {e}"))?
        .as_millis();
    let script_path = updates_root.join(format!("apply-update-{stamp}.cmd"));
    let src = downloaded_exe.display().to_string();
    let dst = current_exe.display().to_string();
    let script = format!(
        r#"@echo off
setlocal
set "SRC={src}"
set "DST={dst}"
for /l %%i in (1,1,120) do (
  copy /Y "%SRC%" "%DST%" >nul 2>&1 && goto copied
  timeout /t 1 /nobreak >nul
)
exit /b 1
:copied
start "" "%DST%"
endlocal
"#
    );
    fs::write(&script_path, script).map_err(|e| format!("write updater script failed: {e}"))?;
    let script_arg = script_path.to_string_lossy().to_string();
    Command::new("cmd")
        .args(["/C", script_arg.as_str()])
        .spawn()
        .map_err(|e| format!("launch updater script failed: {e}"))?;
    Ok(())
}

fn schedule_app_quit(app_weak: slint::Weak<AppWindow>) {
    let _ = app_weak.upgrade_in_event_loop(move |_ui| {
        slint::Timer::single_shot(Duration::from_millis(900), move || {
            let _ = slint::quit_event_loop();
        });
    });
}

pub(super) fn connect(app: &AppWindow, state: Rc<RefCell<GuiState>>) {
    let st_shell_manage = Rc::clone(&state);
    let app_weak = app.as_weak();
    app.on_manage_cmd_types_requested(move || {
        let Some(ui) = app_weak.upgrade() else {
            return;
        };
        let s = st_shell_manage.borrow();
        sync_shell_manage_editor_to_ui(&ui, &*s);
        ui.set_ws_shell_startup_language(SharedString::from(s.startup_language.as_str()));
        ui.set_ws_shell_startup_default_profile(SharedString::from(
            s.startup_default_shell_profile.as_str(),
        ));
        ui.set_ws_shell_startup_terminal_font_family(SharedString::from(
            s.startup_terminal_font_family.as_str(),
        ));
        ui.set_ws_shell_startup_terminal_cjk_fallback_font_family(SharedString::from(
            s.startup_terminal_cjk_fallback_font_family.as_str(),
        ));
        drop(s);
        ui.set_ws_shell_settings_nav(SharedString::from("startup"));
        ui.set_ws_shell_manage_saved_hint(false);
        ui.set_ws_shell_manage_open(true);
    });

    let app_weak = app.as_weak();
    app.on_check_updates_requested(move || {
        let Some(ui) = app_weak.upgrade() else {
            return;
        };
        let preferred = AppConfig::load_or_default()
            .ok()
            .and_then(|cfg| cfg.get_value("ui.preferred_app_version").ok().flatten());
        ui.set_ws_update_current_version(SharedString::from(APP_VERSION));
        ui.set_ws_update_confirm_open(false);
        ui.set_ws_update_open(true);
        refresh_available_versions(app_weak.clone(), preferred);
    });

    let app_weak = app.as_weak();
    app.on_refresh_available_versions_requested(move || {
        let Some(ui) = app_weak.upgrade() else {
            return;
        };
        let preferred = ui.get_ws_update_selected_version().to_string();
        refresh_available_versions(app_weak.clone(), Some(preferred));
    });

    let app_weak = app.as_weak();
    app.on_update_version_selected(move |ver| {
        let Some(ui) = app_weak.upgrade() else {
            return;
        };
        ui.set_ws_update_confirm_open(false);
        ui.set_ws_update_selected_version(ver.clone());
        ui.set_ws_update_status_text(SharedString::from(
            format!("Selected target version: {}", ver.as_str()).as_str(),
        ));
    });

    let app_weak = app.as_weak();
    app.on_apply_update_version_requested(move |ver| {
        let Some(ui) = app_weak.upgrade() else {
            return;
        };
        let selected = ver.trim().to_string();
        if selected.is_empty() {
            ui.set_ws_update_status_text(SharedString::from("Please select a version first"));
            return;
        }
        let rollback_path = std::env::current_exe()
            .ok()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "<current executable path unavailable>".to_string());
        ui.set_ws_update_selected_version(SharedString::from(selected.as_str()));
        ui.set_ws_update_status_text(SharedString::from(
            format!("Preparing update for {selected}...").as_str(),
        ));
        match AppConfig::load_or_default() {
            Ok(mut cfg) => {
                if let Err(e) = cfg.set_value("ui.preferred_app_version", selected.clone()) {
                    ui.set_ws_update_status_text(SharedString::from(
                        format!("Save target version failed: {e}").as_str(),
                    ));
                    return;
                }
                if let Err(e) = cfg.save() {
                    ui.set_ws_update_status_text(SharedString::from(
                        format!("Save config failed: {e}").as_str(),
                    ));
                    return;
                }
            }
            Err(e) => {
                ui.set_ws_update_status_text(SharedString::from(
                    format!("Load config failed: {e}").as_str(),
                ));
                return;
            }
        }

        let app_weak_download = app_weak.clone();
        thread::spawn(move || {
            #[cfg(target_os = "windows")]
            {
                let _ = app_weak_download.upgrade_in_event_loop(|ui| {
                    ui.set_ws_update_status_text(SharedString::from("Resolving release asset..."));
                });
                let resolved = resolve_selected_release_asset(selected.as_str())
                    .and_then(|asset| {
                        let downloading_msg = format!("Downloading {}...", asset.name);
                        let _ = app_weak_download.upgrade_in_event_loop(move |ui| {
                            ui.set_ws_update_status_text(SharedString::from(
                                downloading_msg.as_str(),
                            ));
                        });
                        download_release_asset_to_temp(&asset, selected.as_str())
                    })
                    .and_then(|downloaded_zip| {
                        let extract_msg =
                            format!("Extracting update package from {}", downloaded_zip.display());
                        let _ = app_weak_download.upgrade_in_event_loop(move |ui| {
                            ui.set_ws_update_status_text(SharedString::from(
                                extract_msg.as_str(),
                            ));
                        });
                        let extracted_exe =
                            extract_release_zip_to_temp(downloaded_zip.as_path(), selected.as_str())?;
                        let launch_msg = format!(
                            "Scheduling replacement from {}",
                            extracted_exe.display()
                        );
                        let _ = app_weak_download.upgrade_in_event_loop(move |ui| {
                            ui.set_ws_update_status_text(SharedString::from(
                                launch_msg.as_str(),
                            ));
                        });
                        launch_windows_self_replace_updater(extracted_exe.as_path())
                    });
                let app_weak_for_quit = app_weak_download.clone();
                let _ = app_weak_download.upgrade_in_event_loop(move |ui| match resolved {
                    Ok(()) => {
                        ui.set_ws_update_status_text(SharedString::from(
                            format!(
                                "Update replacement scheduled. App will close and restart on the selected version. If needed, rollback by running: {rollback_path}"
                            )
                            .as_str(),
                        ));
                        schedule_app_quit(app_weak_for_quit.clone());
                    }
                    Err(e) => {
                        ui.set_ws_update_status_text(SharedString::from(
                            format!("Update failed: {e}").as_str(),
                        ));
                    }
                });
            }
            #[cfg(not(target_os = "windows"))]
            {
                let _ = app_weak_download.upgrade_in_event_loop(|ui| {
                    ui.set_ws_update_status_text(SharedString::from(
                        "Auto update is currently implemented for Windows builds only.",
                    ));
                });
            }
        });
    });

    let app_weak = app.as_weak();
    app.on_close_update_dialog(move || {
        let Some(ui) = app_weak.upgrade() else {
            return;
        };
        ui.set_ws_update_confirm_open(false);
        ui.set_ws_update_open(false);
    });

    let app_weak = app.as_weak();
    app.on_about_requested(move || {
        let Some(ui) = app_weak.upgrade() else {
            return;
        };
        ui.set_ws_about_version(SharedString::from(APP_VERSION));
        ui.set_ws_about_author(SharedString::from(APP_AUTHOR));
        ui.set_ws_about_github_url(SharedString::from(APP_GITHUB_URL));
        ui.set_ws_about_open(true);
    });

    let app_weak = app.as_weak();
    app.on_close_about_dialog(move || {
        let Some(ui) = app_weak.upgrade() else {
            return;
        };
        ui.set_ws_about_open(false);
    });

    let app_weak = app.as_weak();
    app.on_open_about_github_requested(move |url| {
        let Some(ui) = app_weak.upgrade() else {
            return;
        };
        let target = url.trim();
        if target.is_empty() {
            return;
        }
        if let Err(e) = open_url_in_browser(target) {
            ui.set_ws_update_status_text(SharedString::from(
                format!("Open link failed: {e}").as_str(),
            ));
        }
    });

    let app_weak = app.as_weak();
    app.on_manage_add_shell_row(move || {
        let Some(ui) = app_weak.upgrade() else {
            return;
        };
        let mut rows = model_interactive_editor_rows(&ui.get_ws_shell_manage_rows());
        for r in &mut rows {
            r.expanded = false;
        }
        rows.push(InteractiveCmdEditorRow {
            name: SharedString::new(),
            line: SharedString::new(),
            interactive_cli: false,
            pinned_footer_lines: SharedString::new(),
            markers: SharedString::new(),
            archive_repainted_frames: false,
            key_locked: false,
            expanded: true,
            workspace_path: SharedString::new(),
        });
        set_shell_manage_rows(&ui, rows);
    });

    let app_weak = app.as_weak();
    app.on_toggle_shell_manage_row_expanded(move |idx| {
        let Some(ui) = app_weak.upgrade() else {
            return;
        };
        if idx < 0 {
            return;
        }
        let i = idx as usize;
        let m = ui.get_ws_shell_manage_rows();
        let n = m.row_count();
        let Some(cur) = m.row_data(i) else {
            return;
        };
        let opening = !cur.expanded;
        for j in 0..n {
            let Some(mut row) = m.row_data(j) else {
                continue;
            };
            row.expanded = opening && j == i;
            m.set_row_data(j, row);
        }
    });

    let app_weak = app.as_weak();
    app.on_remove_shell_manage_row(move |idx| {
        let Some(ui) = app_weak.upgrade() else {
            return;
        };
        if idx < 0 {
            return;
        }
        let i = idx as usize;
        let mut rows = model_interactive_editor_rows(&ui.get_ws_shell_manage_rows());
        if i >= rows.len() || rows[i].key_locked {
            return;
        }
        rows.remove(i);
        set_shell_manage_rows(&ui, rows);
    });

    let app_weak = app.as_weak();
    app.on_shell_manage_name_edited(move |idx, new_text| {
        let Some(ui) = app_weak.upgrade() else {
            return;
        };
        if idx < 0 {
            return;
        }
        let i = idx as usize;
        let m = ui.get_ws_shell_manage_rows();
        let Some(mut row) = m.row_data(i) else {
            return;
        };
        if row.key_locked {
            return;
        }
        row.name = new_text;
        m.set_row_data(i, row);
    });

    let app_weak = app.as_weak();
    app.on_shell_manage_line_edited(move |idx, new_text| {
        let Some(ui) = app_weak.upgrade() else {
            return;
        };
        if idx < 0 {
            return;
        }
        let i = idx as usize;
        let m = ui.get_ws_shell_manage_rows();
        let Some(mut row) = m.row_data(i) else {
            return;
        };
        row.line = new_text;
        m.set_row_data(i, row);
    });

    let app_weak = app.as_weak();
    app.on_shell_manage_workspace_edited(move |idx, new_text| {
        let Some(ui) = app_weak.upgrade() else {
            return;
        };
        if idx < 0 {
            return;
        }
        let i = idx as usize;
        let m = ui.get_ws_shell_manage_rows();
        let Some(mut row) = m.row_data(i) else {
            return;
        };
        row.workspace_path = new_text;
        m.set_row_data(i, row);
    });

    let app_weak = app.as_weak();
    app.on_shell_manage_workspace_pick_folder(move |idx| {
        let Some(ui) = app_weak.upgrade() else {
            return;
        };
        if idx < 0 {
            return;
        }
        let i = idx as usize;
        let Some(path) = rfd::FileDialog::new().pick_folder() else {
            return;
        };
        let path_str = path.to_string_lossy().to_string();
        let m = ui.get_ws_shell_manage_rows();
        let Some(mut row) = m.row_data(i) else {
            return;
        };
        row.workspace_path = SharedString::from(path_str.as_str());
        m.set_row_data(i, row);
    });

    let st_shell_save = Rc::clone(&state);
    let app_weak = app.as_weak();
    app.on_save_shell_manage(move || {
        let Some(ui) = app_weak.upgrade() else {
            return;
        };
        let rows_m = ui.get_ws_shell_manage_rows();
        let n = rows_m.row_count();
        let mut seen = HashSet::<String>::new();
        let mut out: Vec<(String, String, String)> = Vec::new();
        for i in 0..n {
            let row = rows_m.row_data(i).unwrap();
            let name = row.name.to_string();
            let line = row.line.to_string();
            let workspace = row.workspace_path.to_string();
            let nt = name.trim();
            let (norm_line, normalized) = normalize_shell_profile_command(line.as_str());
            let lt = norm_line.trim();
            if nt.is_empty() && lt.is_empty() {
                continue;
            }
            if nt.is_empty() {
                eprintln!("CliGJ: shell profile row needs a display name");
                return;
            }
            if lt.is_empty() {
                eprintln!("CliGJ: shell profile row needs a startup command");
                return;
            }
            let nt = nt.to_string();
            if !seen.insert(nt.clone()) {
                eprintln!("CliGJ: duplicate shell profile name: {nt}");
                return;
            }
            if normalized {
                eprintln!("CliGJ: shell profile '{nt}' normalized to an in-app compatible command");
            }
            out.push((nt, lt.to_string(), workspace.trim().to_string()));
        }

        if out.is_empty() {
            eprintln!("CliGJ: need at least one shell profile");
            return;
        }
        if !out.iter().any(|(n, _, _)| n == "Command Prompt")
            || !out.iter().any(|(n, _, _)| n == "PowerShell")
        {
            eprintln!("CliGJ: default Command Prompt / PowerShell profiles are required");
            return;
        }

        {
            let mut s = st_shell_save.borrow_mut();
            s.shell_profiles = out;
            let fallback = default_shell_profile_name(&*s);
            let allowed: HashSet<String> = s.shell_profiles.iter().map(|(n, _, _)| n.clone()).collect();
            for tab in &mut s.tabs {
                if !allowed.contains(&tab.cmd_type) {
                    tab.cmd_type = fallback.clone();
                }
            }
        }

        let snapshot = st_shell_save.borrow().shell_profiles.clone();
        match AppConfig::load_or_default() {
            Ok(mut cfg) => {
                cfg.set_shell_profiles(&snapshot);
                if let Err(e) = cfg.save() {
                    eprintln!("CliGJ: save config: {e}");
                }
            }
            Err(e) => eprintln!("CliGJ: load config: {e}"),
        }

        let s = st_shell_save.borrow();
        sync_shell_profile_choices_to_ui(&ui, &*s);
        if s.current < s.tabs.len() {
            ui.set_ws_cmd_type(SharedString::from(s.tabs[s.current].cmd_type.as_str()));
        }
        let allowed: HashSet<String> = s.shell_profiles.iter().map(|(n, _, _)| n.clone()).collect();
        drop(s);
        let configured_default = ui.get_ws_shell_startup_default_profile().to_string();
        if !allowed.contains(&configured_default) {
            let fallback = st_shell_save
                .borrow()
                .shell_profiles
                .first()
                .map(|(n, _, _)| n.clone())
                .unwrap_or_else(|| "Command Prompt".to_string());
            ui.set_ws_shell_startup_default_profile(SharedString::from(fallback.as_str()));
            if let Ok(mut cfg) = AppConfig::load_or_default() {
                cfg.set_default_shell_profile(fallback.as_str());
                let _ = cfg.save();
            }
        }
        ui.set_ws_shell_manage_saved_hint(true);
        let ui_weak = ui.as_weak();
        slint::Timer::single_shot(std::time::Duration::from_millis(1600), move || {
            let Some(ui) = ui_weak.upgrade() else {
                return;
            };
            ui.set_ws_shell_manage_saved_hint(false);
        });
    });

    let st_startup_save = Rc::clone(&state);
    let app_weak = app.as_weak();
    app.on_save_shell_startup_settings(move || {
        let Some(ui) = app_weak.upgrade() else {
            return;
        };
        let language = ui.get_ws_shell_startup_language().to_string();
        let mut profile = ui.get_ws_shell_startup_default_profile().to_string();
        let terminal_font_family =
            normalize_terminal_font_family(ui.get_ws_shell_startup_terminal_font_family().as_str())
                .to_string();
        let terminal_cjk_fallback_font_family = normalize_terminal_cjk_fallback_font_family(
            ui.get_ws_shell_startup_terminal_cjk_fallback_font_family()
                .as_str(),
        )
        .to_string();
        let choices = ui.get_ws_cmd_type_choices();
        if choices.row_count() == 0 {
            eprintln!("CliGJ: no shell profiles available");
            return;
        }
        if (0..choices.row_count())
            .all(|i| choices.row_data(i).unwrap_or_default().to_string() != profile)
        {
            profile = choices.row_data(0).unwrap_or_default().to_string();
            ui.set_ws_shell_startup_default_profile(SharedString::from(profile.as_str()));
        }
        match AppConfig::load_or_default() {
            Ok(mut cfg) => {
                cfg.set_ui_language(language.as_str());
                cfg.set_default_shell_profile(profile.as_str());
                cfg.set_terminal_font_family(terminal_font_family.as_str());
                cfg.set_terminal_cjk_fallback_font_family(terminal_cjk_fallback_font_family.as_str());
                if let Err(e) = cfg.save() {
                    eprintln!("CliGJ: save config: {e}");
                    return;
                }
            }
            Err(e) => {
                eprintln!("CliGJ: load config: {e}");
                return;
            }
        }
        {
            let mut s = st_startup_save.borrow_mut();
            s.startup_language = language.clone();
            s.startup_default_shell_profile = profile.clone();
            s.startup_terminal_font_family = terminal_font_family.clone();
            s.startup_terminal_cjk_fallback_font_family = terminal_cjk_fallback_font_family.clone();
        }
        apply_slint_language_from_shell_setting(&ui, language.as_str());
        ui.set_ws_terminal_font_family(SharedString::from(terminal_font_family.as_str()));
        ui.set_ws_terminal_cjk_fallback_font_family(SharedString::from(
            terminal_cjk_fallback_font_family.as_str(),
        ));
        ui.set_ws_shell_startup_terminal_font_family(SharedString::from(
            terminal_font_family.as_str(),
        ));
        ui.set_ws_shell_startup_terminal_cjk_fallback_font_family(SharedString::from(
            terminal_cjk_fallback_font_family.as_str(),
        ));
        ui.set_ws_shell_manage_saved_hint(true);
        let ui_weak = ui.as_weak();
        slint::Timer::single_shot(std::time::Duration::from_millis(1600), move || {
            let Some(ui) = ui_weak.upgrade() else {
                return;
            };
            ui.set_ws_shell_manage_saved_hint(false);
        });
    });

    let st_lang_sync = Rc::clone(&state);
    let app_weak_lang = app.as_weak();
    app.on_shell_ui_language_changed(move |lang| {
        let Some(ui) = app_weak_lang.upgrade() else {
            return;
        };
        st_lang_sync.borrow_mut().startup_language = lang.to_string();
        apply_slint_language_from_shell_setting(&ui, lang.as_str());
    });

    let app_weak = app.as_weak();
    app.on_close_shell_manage(move || {
        let Some(ui) = app_weak.upgrade() else {
            return;
        };
        ui.set_ws_shell_manage_saved_hint(false);
        ui.set_ws_shell_manage_open(false);
    });
}
