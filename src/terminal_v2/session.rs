use super::model::{TerminalBuffer, TerminalCell};
use super::renderer::TerminalFrame;

/// Terminal v2 state holder. In phase 1 this adapts line text snapshots
/// into a cell buffer and tracks dirty rows for future incremental rendering.
pub struct TerminalSession {
    buffer: TerminalBuffer,
}

impl TerminalSession {
    pub fn new(viewport_rows: usize, viewport_cols: usize) -> Self {
        Self {
            buffer: TerminalBuffer::new(viewport_rows, viewport_cols),
        }
    }

    pub fn resize(&mut self, viewport_rows: usize, viewport_cols: usize) {
        self.buffer.resize(viewport_rows, viewport_cols);
    }

    pub fn apply_plain_lines(&mut self, lines: &[String]) {
        let (_, cols) = self.buffer.size();
        self.buffer.resize(lines.len().max(1), cols);
        for (row_idx, row) in self.buffer.rows_mut().iter_mut().enumerate() {
            let src = lines.get(row_idx).map(String::as_str).unwrap_or("");
            for cell in &mut row.cells {
                *cell = TerminalCell::default();
            }
            for (col_idx, ch) in src.chars().take(cols).enumerate() {
                row.cells[col_idx].ch = ch;
            }
        }
        self.buffer.mark_all_dirty();
    }

    pub fn build_frame(&mut self) -> TerminalFrame {
        let (viewport_rows, viewport_cols) = self.buffer.size();
        let dirty_rows = self.buffer.take_dirty_rows();
        let rows = self.buffer.rows().to_vec();
        self.buffer.clear_dirty_rows();
        TerminalFrame {
            rows,
            viewport_rows,
            viewport_cols,
            dirty_rows,
        }
    }
}

