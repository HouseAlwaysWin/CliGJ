//! Build colored span runs from wezterm_term `Line`s using `ColorPalette` (ANSI / truecolor).

use wezterm_term::Line;
use wezterm_term::color::ColorPalette;

/// One horizontal run with the same resolved fg/bg.
#[derive(Debug, Clone)]
pub struct ColoredSpan {
    pub text: String,
    pub fg: [u8; 3],
    pub bg: [u8; 3],
}

#[derive(Debug, Clone)]
pub struct ColoredLine {
    pub spans: Vec<ColoredSpan>,
}

#[must_use]
pub fn line_to_colored_spans(line: &Line, palette: &ColorPalette) -> ColoredLine {
    let mut spans = Vec::new();
    let mut cur = String::new();
    let mut cur_fg = [240u8, 240, 240];
    let mut cur_bg = [18u8, 18, 18];
    let mut have = false;

    let flush = |out: &mut Vec<ColoredSpan>, s: String, fg: [u8; 3], bg: [u8; 3]| {
        if s.is_empty() {
            return;
        }
        out.push(ColoredSpan {
            text: s,
            fg,
            bg,
        });
    };

    for col in 0..line.len() {
        let Some(cell) = line.get_cell(col) else {
            continue;
        };
        let t = cell.str();
        if t.is_empty() && cell.width() == 0 {
            continue;
        }

        let mut fg = srgba_tuple_to_rgb(palette.resolve_fg(cell.attrs().foreground()));
        let mut bg = srgba_tuple_to_rgb(palette.resolve_bg(cell.attrs().background()));
        if cell.attrs().reverse() {
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
        spans.push(ColoredSpan {
            text: " ".into(),
            fg: [240, 240, 240],
            bg: [18, 18, 18],
        });
    }

    ColoredLine { spans }
}

fn srgba_tuple_to_rgb(t: wezterm_term::color::SrgbaTuple) -> [u8; 3] {
    let (r, g, b, _a) = t.to_srgb_u8();
    [r, g, b]
}
