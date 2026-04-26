//! Build colored span runs from wezterm_term `Line`s using `ColorPalette` (ANSI / truecolor).

use wezterm_term::Line;
use wezterm_term::color::ColorPalette;

/// One horizontal run with the same resolved fg/bg.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ColoredSpan {
    pub text: String,
    pub fg: [u8; 3],
    pub bg: [u8; 3],
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ColoredLine {
    /// Physically empty row (no glyphs). Keep out of layout so a cursor on the next row doesn't sit "one line down".
    pub blank: bool,
    pub spans: Vec<ColoredSpan>,
}

#[must_use]
pub fn line_to_colored_spans(
    line: &Line,
    palette: &ColorPalette,
    cursor_col: Option<usize>,
) -> ColoredLine {
    let mut spans = Vec::new();
    let mut cur = String::new();
    let mut cur_fg = [240u8, 240, 240];
    let mut cur_bg = [18u8, 18, 18];
    let mut have = false;
    let mut line_had_reverse = false;
    let has_cursor = cursor_col.is_some();

    let flush = |out: &mut Vec<ColoredSpan>, s: String, fg: [u8; 3], bg: [u8; 3]| {
        if s.is_empty() {
            return;
        }
        out.push(ColoredSpan { text: s, fg, bg });
    };

    for col in 0..line.len() {
        let Some(cell) = line.get_cell(col) else {
            continue;
        };
        let t = cell.str();
        if t.is_empty() && cell.width() == 0 {
            continue;
        }
        if cell.attrs().reverse() {
            line_had_reverse = true;
        }

        let mut fg = srgba_tuple_to_rgb(palette.resolve_fg(cell.attrs().foreground()));
        let mut bg = srgba_tuple_to_rgb(palette.resolve_bg(cell.attrs().background()));
        if cell.attrs().reverse() {
            std::mem::swap(&mut fg, &mut bg);
        }
        if cursor_col == Some(col) {
            std::mem::swap(&mut fg, &mut bg);
        }

        if !have {
            cur_fg = fg;
            cur_bg = bg;
            cur.push_str(t);
            have = true;
            continue;
        }

        if fg == cur_fg && bg == cur_bg {
            cur.push_str(t);
        } else {
            let taken = std::mem::take(&mut cur);
            flush(&mut spans, taken, cur_fg, cur_bg);
            cur.push_str(t);
            cur_fg = fg;
            cur_bg = bg;
        }
    }

    if have {
        flush(&mut spans, cur, cur_fg, cur_bg);
    }

    if spans.is_empty() {
        if has_cursor {
            return ColoredLine {
                blank: false,
                spans: vec![ColoredSpan {
                    text: " ".to_string(),
                    fg: [18u8, 18, 18],
                    bg: [240u8, 240, 240],
                }],
            };
        }
        return ColoredLine {
            blank: true,
            spans: Vec::new(),
        };
    }

    if is_padding_only_line(&spans, line_had_reverse, has_cursor) {
        return ColoredLine {
            blank: true,
            spans: Vec::new(),
        };
    }

    ColoredLine {
        blank: false,
        spans,
    }
}

/// Full-width space fill (no visible glyphs) still produces span text of spaces. That row
/// reserves a whole UI line and pushes the real cursor (often on the next phys line) down.
/// A real text/cursor line either has non-whitespace, reverse video, or non-uniform styling.
fn is_padding_only_line(spans: &[ColoredSpan], line_had_reverse: bool, has_cursor: bool) -> bool {
    if line_had_reverse {
        return false;
    }
    if has_cursor {
        return false;
    }
    if !spans
        .iter()
        .all(|s| s.text.chars().all(|c| c.is_whitespace()))
    {
        return false;
    }
    let Some(first) = spans.first() else {
        return true;
    };
    spans.iter().all(|s| s.fg == first.fg && s.bg == first.bg)
}

fn srgba_tuple_to_rgb(t: wezterm_term::color::SrgbaTuple) -> [u8; 3] {
    let (r, g, b, _a) = t.to_srgb_u8();
    [r, g, b]
}
