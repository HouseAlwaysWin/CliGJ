pub(crate) fn csi_j_clears_screen(params: &[u8]) -> bool {
    params.split(|&b| b == b';').any(|part| {
        let normalized = part.strip_prefix(b"?").unwrap_or(part);
        normalized == b"2" || normalized == b"3"
    })
}

fn csi_homes_cursor(params: &[u8]) -> bool {
    let normalized = params.strip_prefix(b"?").unwrap_or(params);
    normalized.is_empty()
        || normalized == b"1"
        || normalized == b"1;1"
        || normalized == b";1"
        || normalized == b"1;"
}

fn bytes_include_home_and_many_clear_lines(bytes: &[u8], clear_threshold: usize) -> bool {
    let mut i = 0usize;
    let mut home_seen = false;
    let mut clear_count = 0usize;

    while i < bytes.len() {
        if bytes[i] == 0x1b && i + 1 < bytes.len() && bytes[i + 1] == b'[' {
            let mut j = i + 2;
            let mut handled = false;
            while j < bytes.len() {
                let b = bytes[j];
                if (0x40..=0x7e).contains(&b) {
                    let params = &bytes[i + 2..j];
                    if b == b'H' && csi_homes_cursor(params) {
                        home_seen = true;
                        clear_count = 0;
                    } else if home_seen && b == b'K' {
                        clear_count += 1;
                        if clear_count >= clear_threshold {
                            return true;
                        }
                    } else if home_seen && b != b'm' {
                        home_seen = false;
                        clear_count = 0;
                    }
                    i = j + 1;
                    handled = true;
                    break;
                }
                j += 1;
            }
            if handled {
                continue;
            }
        }

        if home_seen {
            let b = bytes[i];
            if b == b'\r' || b == b'\n' || b == b' ' || b == b'\t' {
                i += 1;
                continue;
            }
            if b >= 0x20 {
                home_seen = false;
                clear_count = 0;
            }
        }

        i += 1;
    }

    false
}

pub(crate) fn bytes_include_clear_screen_sequence_for_rows(bytes: &[u8], rows: usize) -> bool {
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] == 0x1b {
            if i + 1 < bytes.len() && bytes[i + 1] == b'c' {
                return true;
            }
            if i + 1 < bytes.len() && bytes[i + 1] == b'[' {
                let mut j = i + 2;
                while j < bytes.len() {
                    let b = bytes[j];
                    if (0x40..=0x7e).contains(&b) {
                        if b == b'J' && csi_j_clears_screen(&bytes[i + 2..j]) {
                            return true;
                        }
                        break;
                    }
                    j += 1;
                }
            }
        } else if bytes[i] == 0x9b {
            let mut j = i + 1;
            while j < bytes.len() {
                let b = bytes[j];
                if (0x40..=0x7e).contains(&b) {
                    if b == b'J' && csi_j_clears_screen(&bytes[i + 1..j]) {
                        return true;
                    }
                    break;
                }
                j += 1;
            }
        }
        i += 1;
    }
    bytes_include_home_and_many_clear_lines(bytes, rows.saturating_div(2).max(4))
}

#[cfg(test)]
pub(crate) fn bytes_include_clear_screen_sequence(bytes: &[u8]) -> bool {
    bytes_include_clear_screen_sequence_for_rows(bytes, 16)
}

#[cfg(test)]
mod tests {
    use super::bytes_include_clear_screen_sequence;

    #[test]
    fn detects_clear_sequences() {
        assert!(bytes_include_clear_screen_sequence(b"\x1b[H\x1b[2J"));
        assert!(bytes_include_clear_screen_sequence(b"\x1b[3J"));
        assert!(bytes_include_clear_screen_sequence(b"\x1bc"));
        assert!(bytes_include_clear_screen_sequence(&[0x9b, b'2', b'J']));
        assert!(bytes_include_clear_screen_sequence(
            b"\x1b[H\x1b[K\r\n\x1b[K\r\n\x1b[K\r\n\x1b[K\r\n\x1b[K\r\n\x1b[K\r\n\x1b[K\r\n\x1b[K\r\n"
        ));
        assert!(!bytes_include_clear_screen_sequence(b"\x1b[0J"));
    }
}
