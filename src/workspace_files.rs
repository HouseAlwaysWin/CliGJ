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

/// `prompt` has `@` segment: replace `@{query}` on current line with `@{chosen}` (+ trailing space).
#[must_use]
pub fn apply_at_file_pick(prompt: &str, chosen_rel: &str) -> String {
    let Some(at) = prompt.rfind('@') else {
        return prompt.to_string();
    };
    let line_end = prompt[at + 1..]
        .find(['\r', '\n'])
        .map(|i| at + 1 + i)
        .unwrap_or(prompt.len());
    let mut s = String::with_capacity(prompt.len() + chosen_rel.len());
    s.push_str(&prompt[..at + 1]);
    s.push_str(chosen_rel);
    s.push(' ');
    s.push_str(&prompt[line_end..]);
    s
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apply_pick() {
        let out = apply_at_file_pick("hi @f", "src/x.rs");
        assert_eq!(out, "hi @src/x.rs ");
    }

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
}
