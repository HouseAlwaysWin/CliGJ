//! Composer line → ConPTY sync when `@` is present (path shows on the shell line).

use super::slint_ui::AppWindow;
use super::state::GuiState;

pub(crate) fn diff_composer_to_conpty(prev: &str, cur: &str) -> Vec<u8> {
    if prev == cur {
        return Vec::new();
    }
    let pa: Vec<char> = prev.chars().collect();
    let ca: Vec<char> = cur.chars().collect();
    let mut i = 0usize;
    while i < pa.len() && i < ca.len() && pa[i] == ca[i] {
        i += 1;
    }
    let mut out = Vec::new();
    for _ in 0..pa.len().saturating_sub(i) {
        out.push(0x08);
    }
    for c in ca.iter().skip(i) {
        let mut buf = [0u8; 4];
        let t = c.encode_utf8(&mut buf);
        out.extend_from_slice(t.as_bytes());
    }
    out
}

pub(crate) fn sync_composer_line_to_conpty(ui: &AppWindow, s: &mut GuiState) {
    #[cfg(not(target_os = "windows"))]
    let _ = (ui, s);

    #[cfg(target_os = "windows")]
    {
        use std::io::Write;

        if s.current >= s.tabs.len() {
            return;
        }
        // Raw mode does direct key->PTY routing; skip expensive full UI->tab sync on timer tick.
        if ui.get_ws_raw_input() {
            s.tabs[s.current].raw_input_mode = true;
            return;
        }
        let tab = &mut s.tabs[s.current];
        tab.raw_input_mode = false;
        // Only mirror prompt — avoid `tab_update_from_ui` here (it syncs full tab state every tick).
        tab.prompt = ui.get_ws_prompt();
        let Some(session) = tab.conpty.as_mut() else {
            return;
        };

        let cur = tab.prompt.to_string();
        let prev = tab.composer_pty_mirror.as_str();

        let bytes = diff_composer_to_conpty(prev, &cur);
        if bytes.is_empty() {
            return;
        }
        let _ = session.writer.write_all(&bytes);
        let _ = session.writer.flush();
        tab.composer_pty_mirror = cur;
    }
}

#[cfg(test)]
mod tests {
    use super::diff_composer_to_conpty;

    #[test]
    fn append_from_empty() {
        assert_eq!(diff_composer_to_conpty("", "ab"), b"ab");
    }

    #[test]
    fn shrink_one_char() {
        assert_eq!(diff_composer_to_conpty("ab", "a"), vec![0x08]);
    }

    #[test]
    fn common_prefix_replace_tail() {
        let d = diff_composer_to_conpty("hello@x", "hello@yz");
        assert!(d.iter().any(|&b| b == b'y' || b == b'z'));
    }

    #[test]
    fn clear_all() {
        assert_eq!(
            diff_composer_to_conpty("abc", "").as_slice(),
            &[0x08, 0x08, 0x08]
        );
    }
}
