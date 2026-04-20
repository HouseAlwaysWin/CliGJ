//! Workspace-relative paths for `@` file picker (bounded depth / count).

use std::fs;
use std::path::Path;

const MAX_DEPTH: usize = 14;
const MAX_FILES_SCAN: usize = 8000;
pub const CHOICES_DISPLAY: usize = 800;

static IGNORE_DIR_NAMES: &[&str] = &[
    ".git",
    ".hg",
    ".svn",
    "target",
    "node_modules",
    "__pycache__",
    ".vs",
    ".idea",
];

/// Returns stable relative paths using `/` separators for display injection.
pub fn scan_workspace_files(root: &Path) -> Vec<String> {
    let mut out = Vec::new();
    if !root.is_dir() {
        return out;
    }
    scan_dir(root, root, 0, &mut out);
    out.sort();
    out
}

fn scan_dir(base: &Path, dir: &Path, depth: usize, out: &mut Vec<String>) {
    if out.len() >= MAX_FILES_SCAN || depth > MAX_DEPTH {
        return;
    }
    let Ok(rd) = fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<_> = rd.filter_map(|e| e.ok()).collect();
    entries.sort_by_key(|e| e.file_name());

    for e in entries {
        let path = e.path();
        let name = e.file_name();
        let name = name.to_string_lossy();
        if path.is_dir() && IGNORE_DIR_NAMES.contains(&name.as_ref()) {
            continue;
        }

        let Ok(rel) = path.strip_prefix(base) else {
            continue;
        };
        let rel_s = rel.to_string_lossy().replace('\\', "/");
        if rel_s.is_empty() {
            continue;
        }
        if path.is_file() {
            out.push(rel_s);
            if out.len() >= MAX_FILES_SCAN {
                return;
            }
        }

        if path.is_dir() {
            scan_dir(base, &path, depth + 1, out);
            if out.len() >= MAX_FILES_SCAN {
                return;
            }
        }
    }
}

#[must_use]
pub fn filter_paths(paths: &[String], query: &str, max: usize) -> Vec<String> {
    let q = query.trim().to_lowercase();
    if q.is_empty() {
        // Pure lex order puts `src/...` after hundreds of `.foo` / nested paths on large trees.
        // Prefer shallow paths first so root + `src/` show up when the user types only `@`.
        let mut v: Vec<String> = paths.iter().cloned().collect();
        v.sort_by(|a, b| {
            let da = path_depth(a);
            let db = path_depth(b);
            da.cmp(&db).then_with(|| a.cmp(b))
        });
        return v.into_iter().take(max).collect();
    }
    paths
        .iter()
        .filter(|p| p.to_lowercase().contains(&q))
        .take(max)
        .cloned()
        .collect()
}

fn path_depth(p: &str) -> usize {
    p.chars().filter(|&c| c == '/' || c == '\\').count()
}

fn resolve_under_workspace(workspace_root: &Path, chosen_rel: &str) -> std::path::PathBuf {
    chosen_rel
        .split(|c| c == '/' || c == '\\')
        .filter(|s| !s.is_empty() && *s != ".")
        .fold(workspace_root.to_path_buf(), |acc, seg| acc.join(seg))
}

#[must_use]
pub fn absolute_path_from_pick(chosen_rel: &str, workspace_root: &Path) -> String {
    resolve_under_workspace(workspace_root, chosen_rel)
        .display()
        .to_string()
}

/// Replace active `@...` with nothing in the visible prompt and return hidden absolute path.
#[must_use]
pub fn apply_at_file_pick_hidden(prompt: &str, chosen_rel: &str, workspace_root: &Path) -> (String, String) {
    let mut visible = strip_active_at_segment(prompt);
    let ends_with_ws = visible.chars().last().map(char::is_whitespace).unwrap_or(true);
    if !visible.is_empty() && !ends_with_ws {
        visible.push(' ');
    }
    let abs = absolute_path_from_pick(chosen_rel, workspace_root);
    (visible, abs)
}

/// Strip `@` and following query on the current line (Escape cancel).
#[must_use]
pub fn strip_active_at_segment(prompt: &str) -> String {
    let Some(at) = prompt.rfind('@') else {
        return prompt.to_string();
    };
    let line_end = prompt[at + 1..]
        .find(['\r', '\n'])
        .map(|i| at + 1 + i)
        .unwrap_or(prompt.len());
    let mut s = prompt[..at].to_string();
    s.push_str(&prompt[line_end..]);
    s
}

/// Windows [`std::fs::canonicalize`] returns *verbatim* paths: `\\?\C:\...` or `\\?\UNC\server\...`.
/// Those are correct internally but look like "extra backslashes" (and `?` may render poorly in some fonts).
/// Strip the prefix for display and for shell command lines.
#[must_use]
pub fn strip_windows_verbatim_prefix(path: &str) -> String {
    const PREFIX: &str = r"\\?\";
    if !path.starts_with(PREFIX) {
        return path.to_string();
    }
    let rest = &path[PREFIX.len()..];
    if rest.starts_with("UNC\\") {
        // \\?\UNC\server\share -> \\server\share
        format!(r"\\{}", &rest[4..])
    } else {
        rest.to_string()
    }
}

#[must_use]
pub fn file_name_label(path: &str) -> String {
    Path::new(path)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(path)
        .to_string()
}

#[must_use]
pub fn file_attachment_token(index_1_based: usize) -> String {
    format!("[[file{index_1_based}]]")
}

#[must_use]
pub fn image_attachment_token(index_1_based: usize) -> String {
    format!("[[img{index_1_based}]]")
}

#[must_use]
pub fn selection_attachment_token(index_1_based: usize) -> String {
    format!("[[sel{index_1_based}]]")
}

#[must_use]
pub fn append_attachment_token(prompt: &str, token: &str) -> String {
    let p = prompt.trim_end();
    if p.is_empty() {
        return token.to_string();
    }
    if p.ends_with(' ') || p.ends_with('\n') || p.ends_with('\t') {
        format!("{p}{token}")
    } else {
        format!("{p} {token}")
    }
}

#[must_use]
pub fn expand_attachment_tokens(
    prompt: &str,
    file_paths: &[String],
    image_paths: &[String],
    selection_payloads: &[String],
) -> String {
    let mut out = prompt.to_string();
    for (i, p) in file_paths.iter().enumerate() {
        let token = file_attachment_token(i + 1);
        out = out.replace(token.as_str(), p.as_str());
    }
    for (i, p) in image_paths.iter().enumerate() {
        let token = image_attachment_token(i + 1);
        out = out.replace(token.as_str(), p.as_str());
    }
    for (i, p) in selection_payloads.iter().enumerate() {
        let token = selection_attachment_token(i + 1);
        out = out.replace(token.as_str(), p.as_str());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_query_prefers_shallow_paths() {
        let paths = vec![
            "z_shallow.rs".into(),
            "src/main.rs".into(),
            "a_shallow.rs".into(),
        ];
        let out = filter_paths(&paths, "", 10);
        assert_eq!(out[0], "a_shallow.rs");
        assert_eq!(out[1], "z_shallow.rs");
        assert_eq!(out[2], "src/main.rs");
    }

    #[test]
    fn strip_verbatim_d_drive() {
        assert_eq!(
            strip_windows_verbatim_prefix(r"\\?\D:\Projects\CliGJ\Cargo.toml"),
            r"D:\Projects\CliGJ\Cargo.toml"
        );
    }

    #[test]
    fn strip_verbatim_unc() {
        assert_eq!(
            strip_windows_verbatim_prefix(r"\\?\UNC\server\share\file.txt"),
            r"\\server\share\file.txt"
        );
    }

    #[test]
    fn hidden_pick_keeps_prompt_clean_and_returns_abs() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let (visible, abs) = apply_at_file_pick_hidden("open @car", "Cargo.toml", root);
        assert_eq!(visible, "open ");
        assert!(abs.ends_with("Cargo.toml"));
    }

    #[test]
    fn token_expand_replaces_file_and_image_tokens() {
        let prompt = "ask [[file1]] with [[img1]] and [[sel1]]";
        let files = vec!["D:/a.txt".to_string()];
        let images = vec!["D:/p.png".to_string()];
        let selections = vec!["code payload".to_string()];
        let out = expand_attachment_tokens(prompt, &files, &images, &selections);
        assert_eq!(out, "ask D:/a.txt with D:/p.png and code payload");
    }
}
