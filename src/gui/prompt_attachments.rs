//! Composer `@file` tokens ↔ picked image list (keep prompt text and chips in sync).

use std::collections::HashMap;

use crate::workspace_files;

use super::state::TabState;

/// Composer `@basename` / `@basename_N` token for the image at `index` (matches append order).
pub(crate) fn hint_token_for_image_index(tab: &TabState, index: usize) -> Option<String> {
    tab.prompt_picked_images.get(index)?;
    let mut img_dup: HashMap<String, usize> = HashMap::new();
    for (i, img) in tab.prompt_picked_images.iter().enumerate() {
        let name = workspace_files::file_name_label(img.abs_path.as_str());
        let cnt = img_dup.entry(name.clone()).or_insert(0);
        *cnt += 1;
        if i == index {
            let file_count = tab
                .prompt_picked_files_abs
                .iter()
                .filter(|p| workspace_files::file_name_label(p) == name)
                .count();
            let occ = file_count + *cnt;
            return Some(workspace_files::filepath_hint_token(name.as_str(), occ));
        }
    }
    None
}

/// Drop image attachments whose `@…` token no longer appears in `tab.prompt`.
pub(crate) fn prune_prompt_images_not_in_prompt(tab: &mut TabState) -> bool {
    let prompt = tab.prompt.to_string();
    let mut changed = false;
    let mut i = 0usize;
    while i < tab.prompt_picked_images.len() {
        let Some(tok) = hint_token_for_image_index(tab, i) else {
            i += 1;
            continue;
        };
        if workspace_files::prompt_has_whitespace_delimited_token(&prompt, tok.as_str()) {
            i += 1;
        } else {
            tab.prompt_picked_images.remove(i);
            changed = true;
        }
    }
    changed
}
