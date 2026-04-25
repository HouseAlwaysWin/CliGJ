use crate::render::ColoredLine;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RawPtyMode {
    Shell,
    InteractiveAi,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RawPtyEvent {
    Bytes(Vec<u8>),
    Resize { cols: u16, rows: u16 },
    RenderMode { mode: RawPtyMode },
}

impl RawPtyEvent {
    pub fn byte_len(&self) -> usize {
        match self {
            Self::Bytes(bytes) => bytes.len(),
            Self::Resize { .. } | Self::RenderMode { .. } => 0,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReaderRenderMode {
    Shell,
    InteractiveAi,
}

impl From<ReaderRenderMode> for RawPtyMode {
    fn from(value: ReaderRenderMode) -> Self {
        match value {
            ReaderRenderMode::Shell => Self::Shell,
            ReaderRenderMode::InteractiveAi => Self::InteractiveAi,
        }
    }
}

#[derive(Debug, Clone)]
pub struct TerminalRender {
    pub render_mode: ReaderRenderMode,
    pub raw_pty_events: Vec<RawPtyEvent>,
    pub text: String,
    /// ONLY lines that changed (matches changed_indices length).
    pub lines: Vec<ColoredLine>,
    /// Number of physical rows covered by this snapshot window.
    pub snapshot_len: usize,
    /// Total physical rows known to wezterm-term for this screen.
    pub full_len: usize,
    pub first_line_idx: usize,
    pub cursor_row: Option<usize>,
    pub cursor_col: Option<usize>,
    pub filled: bool,
    /// Indices of lines that changed since last render (for downstream diff).
    pub changed_indices: Vec<usize>,
    /// Next snapshot should replace the GUI buffer entirely (PTY geometry / reflow reset).
    pub reset_terminal_buffer: bool,
}

#[derive(Debug)]
pub enum ControlCommand {
    Resize { cols: u16, rows: u16 },
    SetRenderMode(ReaderRenderMode),
}
