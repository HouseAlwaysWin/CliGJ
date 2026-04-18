//! Shared helpers for `run` (clipboard, terminal selection text, inject, CJK/raw heuristics).

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use arboard::{Clipboard, ImageData};
use image::RgbaImage;
use slint::{Image, Rgba8Pixel, SharedPixelBuffer};

use crate::terminal::key_encoding;
use crate::terminal::render::ColoredLine;
use crate::workspace_files;

use super::super::slint_ui::AppWindow;
use super::super::state::{GuiState, PromptImageAttach, TabState};

pub(crate) fn inject_path_into_current(
    ui: &AppWindow,
    s: &mut GuiState,
    path: &Path,
) -> Result<(), String> {
    inject_paths_into_current(ui, s, std::slice::from_ref(&path.to_path_buf()))
}

/// Add one or more absolute paths as composer file chips (same rules as drag-and-drop).
pub(crate) fn inject_paths_into_current(
    ui: &AppWindow,
    s: &mut GuiState,
    paths: &[PathBuf],
) -> Result<(), String> {
    if s.current >= s.tabs.len() {
        return Err("invalid tab index".into());
    }
    if paths.is_empty() {
        return Ok(());
    }
    let tab = &mut s.tabs[s.current];
    for path in paths {
        let abs_path = path
            .canonicalize()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|_| path.to_string_lossy().to_string());
        let abs_path = workspace_files::strip_windows_verbatim_prefix(&abs_path);
        if path_has_prompt_attachment(tab, &abs_path) {
            continue;
        }
        if !tab.prompt_picked_files_abs.contains(&abs_path) {
            tab.prompt_picked_files_abs.push(abs_path);
        }
    }
    crate::gui::ui_sync::load_tab_to_ui(ui, tab);
    Ok(())
}

fn path_has_prompt_attachment(tab: &TabState, abs_path: &str) -> bool {
    tab.prompt_picked_files_abs.iter().any(|p| p == abs_path)
        || tab
            .prompt_picked_images
            .iter()
            .any(|p| p.abs_path == abs_path)
}

/// Windows: paths from Explorer copy (`CF_HDROP`). `None` if clipboard has no file list or read failed.
#[cfg(target_os = "windows")]
pub(crate) fn clipboard_file_paths_hdrop() -> Option<Vec<PathBuf>> {
    use clipboard_win::{formats, get_clipboard};
    let paths: Vec<PathBuf> = get_clipboard(formats::FileList).ok()?;
    (!paths.is_empty()).then_some(paths)
}

#[cfg(not(target_os = "windows"))]
pub(crate) fn clipboard_file_paths_hdrop() -> Option<Vec<PathBuf>> {
    None
}

/// Common raster extensions we treat as "attach as image preview" (not only a file chip).
pub(crate) fn is_probably_image_file(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| {
            matches!(
                e.to_ascii_lowercase().as_str(),
                "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "ico" | "tif" | "tiff" | "svg"
            )
        })
        .unwrap_or(false)
}

pub(crate) fn load_slint_image_from_path(path: &Path) -> Option<Image> {
    Image::load_from_path(path).ok()
}

fn slint_image_from_arboard_rgba(img: &ImageData<'_>) -> Option<Image> {
    let w = u32::try_from(img.width).ok()?;
    let h = u32::try_from(img.height).ok()?;
    let need = img.width.checked_mul(img.height)?.checked_mul(4)?;
    if img.bytes.len() < need {
        return None;
    }
    let src = &img.bytes[..need];
    let buf = SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(src, w, h);
    Some(Image::from_rgba8(buf))
}

fn temp_clipboard_png_path() -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    std::env::temp_dir().join(format!("cligj_clip_{nanos}.png"))
}

fn write_rgba_png(path: &Path, width: u32, height: u32, rgba: &[u8]) -> Result<(), String> {
    let expected = (width as usize)
        .checked_mul(height as usize)
        .and_then(|n| n.checked_mul(4))
        .ok_or_else(|| "bad image dimensions".to_string())?;
    if rgba.len() < expected {
        return Err("clipboard image buffer too small".into());
    }
    let img: RgbaImage = image::ImageBuffer::from_raw(width, height, rgba[..expected].to_vec())
        .ok_or_else(|| "bad image buffer".to_string())?;
    img.save(path).map_err(|e| e.to_string())?;
    Ok(())
}

/// Bitmap from OS clipboard → temp PNG path + Slint image (for submit line).
pub(crate) fn clipboard_raster_image_file() -> Option<(PathBuf, Image)> {
    let mut cb = Clipboard::new().ok()?;
    let data = cb.get_image().ok()?;
    let preview = slint_image_from_arboard_rgba(&data)?;
    let w = u32::try_from(data.width).ok()?;
    let h = u32::try_from(data.height).ok()?;
    let need = data.width.checked_mul(data.height)?.checked_mul(4)?;
    if data.bytes.len() < need {
        return None;
    }
    let path = temp_clipboard_png_path();
    write_rgba_png(&path, w, h, &data.bytes[..need]).ok()?;
    Some((path, preview))
}

fn normalize_abs_path(path: &Path) -> String {
    let abs_path = path
        .canonicalize()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| path.to_string_lossy().to_string());
    workspace_files::strip_windows_verbatim_prefix(&abs_path)
}

/// Append one image attachment (dedupe by absolute path).
pub(crate) fn push_prompt_image(
    ui: &AppWindow,
    s: &mut GuiState,
    abs_path: String,
    preview: Image,
) -> Result<(), String> {
    if s.current >= s.tabs.len() {
        return Err("invalid tab index".into());
    }
    let tab = &mut s.tabs[s.current];
    if path_has_prompt_attachment(tab, &abs_path) {
        return Ok(());
    }
    tab.prompt_picked_images.push(PromptImageAttach {
        abs_path,
        preview,
    });
    crate::gui::ui_sync::load_tab_to_ui(ui, tab);
    Ok(())
}

pub(crate) fn push_prompt_image_from_path(
    ui: &AppWindow,
    s: &mut GuiState,
    path: &Path,
) -> Result<(), String> {
    let Some(preview) = load_slint_image_from_path(path) else {
        return Err("could not load image".into());
    };
    let abs_path = normalize_abs_path(path);
    push_prompt_image(ui, s, abs_path, preview)
}

/// Paste / drop: image paths become image chips; other paths become file chips.
pub(crate) fn inject_paths_and_images_from_paths(
    ui: &AppWindow,
    s: &mut GuiState,
    paths: &[PathBuf],
) -> Result<(), String> {
    if s.current >= s.tabs.len() {
        return Err("invalid tab index".into());
    }
    if paths.is_empty() {
        return Ok(());
    }
    let mut non_image_paths = Vec::new();
    {
        let tab = &mut s.tabs[s.current];
        for path in paths {
            if is_probably_image_file(path) {
                if let Some(preview) = load_slint_image_from_path(path) {
                    let abs_path = normalize_abs_path(path);
                    if path_has_prompt_attachment(tab, &abs_path) {
                        continue;
                    }
                    tab.prompt_picked_images.push(PromptImageAttach {
                        abs_path,
                        preview,
                    });
                    continue;
                }
            }
            non_image_paths.push(path.clone());
        }
    }
    if !non_image_paths.is_empty() {
        inject_paths_into_current(ui, s, &non_image_paths)?;
    } else {
        let tab = &mut s.tabs[s.current];
        crate::gui::ui_sync::load_tab_to_ui(ui, tab);
    }
    Ok(())
}

pub(crate) fn remove_prompt_image_at(ui: &AppWindow, s: &mut GuiState, index: usize) {
    if s.current >= s.tabs.len() {
        return;
    }
    let tab = &mut s.tabs[s.current];
    if index < tab.prompt_picked_images.len() {
        tab.prompt_picked_images.remove(index);
        crate::gui::ui_sync::load_tab_to_ui(ui, tab);
    }
}

pub(crate) fn clear_all_prompt_images(ui: &AppWindow, s: &mut GuiState) {
    if s.current >= s.tabs.len() {
        return;
    }
    let tab = &mut s.tabs[s.current];
    tab.prompt_picked_images.clear();
    crate::gui::ui_sync::load_tab_to_ui(ui, tab);
}

pub(crate) fn auto_disable_raw_on_cjk_prompt(ui: &AppWindow, s: &mut GuiState) {
    if s.current >= s.tabs.len() {
        return;
    }
    if !s.tabs[s.current].raw_input_mode {
        return;
    }
    let prompt = ui.get_ws_prompt().to_string();
    if !contains_cjk_char(&prompt) {
        return;
    }
    if let Err(e) = s.toggle_raw_input_current(ui) {
        eprintln!("CliGJ: raw input auto-toggle (prompt CJK): {e}");
    }
}

pub(crate) fn contains_cjk_char(text: &str) -> bool {
    text.chars().any(|ch| {
        matches!(
            ch as u32,
            0x3400..=0x4DBF // CJK Unified Ideographs Extension A
                | 0x4E00..=0x9FFF // CJK Unified Ideographs
                | 0xF900..=0xFAFF // CJK Compatibility Ideographs
                | 0x20000..=0x2CEAF // CJK Unified Ideographs Extension B-E
                | 0x2EBF0..=0x2EE5F // CJK Unified Ideographs Extension I
                | 0x3000..=0x303F // CJK Symbols and Punctuation
                | 0xFF00..=0xFFEF // Halfwidth and Fullwidth Forms
        )
    })
}

pub(crate) fn is_local_prompt_edit_key(mod_mask: u32, key: &str) -> bool {
    if mod_mask & (key_encoding::MOD_CTRL | key_encoding::MOD_ALT | key_encoding::MOD_META) != 0 {
        return false;
    }
    matches!(
        key,
        "Backspace" | "Delete" | "LeftArrow" | "RightArrow" | "Home" | "End"
    )
}

fn colored_line_plain_text(line: &ColoredLine) -> String {
    line.spans.iter().fold(String::new(), |mut acc, s| {
        acc.push_str(s.text.as_str());
        acc
    })
}

/// Inclusive character slice (Unicode scalar indices, matching Slint `char-count`).
fn slice_line_chars_inclusive(line: &ColoredLine, start: usize, end_inclusive: usize) -> String {
    let s = colored_line_plain_text(line);
    let chars: Vec<char> = s.chars().collect();
    let n = chars.len();
    if n == 0 || start > end_inclusive {
        return String::new();
    }
    let start = start.min(n - 1);
    let end_inclusive = end_inclusive.min(n - 1);
    chars[start..=end_inclusive].iter().collect()
}

fn slice_line_from_char(line: &ColoredLine, start: usize) -> String {
    let s = colored_line_plain_text(line);
    let chars: Vec<char> = s.chars().collect();
    let n = chars.len();
    if start >= n {
        return String::new();
    }
    chars[start..].iter().collect()
}

fn slice_line_to_char_inclusive(line: &ColoredLine, end_inclusive: usize) -> String {
    slice_line_chars_inclusive(line, 0, end_inclusive)
}

pub(crate) fn selected_text_from_terminal_lines(
    tab: &TabState,
    sr: i32,
    sc: i32,
    er: i32,
    ec: i32,
) -> String {
    if tab.terminal_lines.is_empty() {
        return String::new();
    }
    let sr = sr.max(0) as usize;
    let sc = sc.max(0) as usize;
    let er = er.max(0) as usize;
    let ec = ec.max(0) as usize;
    let max_row = tab.terminal_lines.len() - 1;
    let sr = sr.min(max_row);
    let er = er.min(max_row);
    if sr > er {
        return String::new();
    }
    let mut out = String::new();
    if sr == er {
        let line = &tab.terminal_lines[sr];
        return slice_line_chars_inclusive(line, sc, ec);
    }
    for row_idx in sr..=er {
        if row_idx > sr {
            out.push('\n');
        }
        let line = &tab.terminal_lines[row_idx];
        if row_idx == sr {
            out.push_str(&slice_line_from_char(line, sc));
        } else if row_idx == er {
            out.push_str(&slice_line_to_char_inclusive(line, ec));
        } else {
            out.push_str(&colored_line_plain_text(line));
        }
    }
    out
}

pub(crate) fn copy_to_clipboard(text: &str) -> Result<(), String> {
    let mut clipboard = Clipboard::new().map_err(|e| e.to_string())?;
    clipboard.set_text(text.to_string()).map_err(|e| e.to_string())
}
