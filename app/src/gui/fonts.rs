pub(crate) const DEFAULT_TERMINAL_FONT_FAMILY: &str = "DejaVu Sans Mono";
pub(crate) const TERMINAL_CJK_FALLBACK_FONT_FAMILY: &str = "MingLiU";

const TERMINAL_FONT_CHOICES: &[&str] = &[
    "Cascadia Mono",
    "Cascadia Code",
    "MingLiU",
    "DejaVu Sans Mono",
    "Microsoft JhengHei",
    "Noto Sans TC",
];

const TERMINAL_CJK_FALLBACK_FONT_CHOICES: &[&str] = &[
    "MingLiU",
    "Microsoft JhengHei",
    "Noto Sans TC",
    "PMingLiU",
];

pub(crate) fn terminal_font_choices() -> &'static [&'static str] {
    TERMINAL_FONT_CHOICES
}

pub(crate) fn terminal_cjk_fallback_font_choices() -> &'static [&'static str] {
    TERMINAL_CJK_FALLBACK_FONT_CHOICES
}

pub(crate) fn normalize_terminal_font_family(value: &str) -> &'static str {
    let trimmed = value.trim();
    TERMINAL_FONT_CHOICES
        .iter()
        .copied()
        .find(|name| *name == trimmed)
        .unwrap_or(DEFAULT_TERMINAL_FONT_FAMILY)
}

pub(crate) fn normalize_terminal_cjk_fallback_font_family(value: &str) -> &'static str {
    let trimmed = value.trim();
    TERMINAL_CJK_FALLBACK_FONT_CHOICES
        .iter()
        .copied()
        .find(|name| *name == trimmed)
        .unwrap_or(TERMINAL_CJK_FALLBACK_FONT_FAMILY)
}

