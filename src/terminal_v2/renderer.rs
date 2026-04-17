use super::model::TerminalRow;

#[derive(Debug, Clone)]
pub struct TerminalFrame {
    pub rows: Vec<TerminalRow>,
    pub viewport_rows: usize,
    pub viewport_cols: usize,
    pub dirty_rows: Vec<usize>,
}

pub trait TerminalRenderer {
    fn render(&mut self, frame: &TerminalFrame);
}

/// Placeholder renderer for phase 1 wiring.
pub struct NoopRenderer;

impl TerminalRenderer for NoopRenderer {
    fn render(&mut self, _frame: &TerminalFrame) {}
}

