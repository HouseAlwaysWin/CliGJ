use std::collections::BTreeSet;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalCell {
    pub ch: char,
    pub fg: [u8; 3],
    pub bg: [u8; 3],
}

impl Default for TerminalCell {
    fn default() -> Self {
        Self {
            ch: ' ',
            fg: [240, 240, 240],
            bg: [18, 18, 18],
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalRow {
    pub cells: Vec<TerminalCell>,
}

impl TerminalRow {
    pub fn new(cols: usize) -> Self {
        Self {
            cells: vec![TerminalCell::default(); cols],
        }
    }
}

#[derive(Debug, Clone)]
pub struct TerminalBuffer {
    rows: Vec<TerminalRow>,
    dirty_rows: BTreeSet<usize>,
    cols: usize,
}

impl TerminalBuffer {
    pub fn new(rows: usize, cols: usize) -> Self {
        let rows_vec = (0..rows).map(|_| TerminalRow::new(cols)).collect();
        Self {
            rows: rows_vec,
            dirty_rows: (0..rows).collect(),
            cols,
        }
    }

    pub fn rows(&self) -> &[TerminalRow] {
        &self.rows
    }

    pub fn rows_mut(&mut self) -> &mut [TerminalRow] {
        &mut self.rows
    }

    pub fn size(&self) -> (usize, usize) {
        (self.rows.len(), self.cols)
    }

    pub fn resize(&mut self, rows: usize, cols: usize) {
        if self.rows.len() < rows {
            self.rows
                .extend((self.rows.len()..rows).map(|_| TerminalRow::new(cols)));
        } else {
            self.rows.truncate(rows);
        }
        for row in &mut self.rows {
            row.cells.resize(cols, TerminalCell::default());
        }
        self.cols = cols;
        self.dirty_rows.extend(0..self.rows.len());
    }

    pub fn set_cell(&mut self, row: usize, col: usize, cell: TerminalCell) {
        if row >= self.rows.len() || col >= self.cols {
            return;
        }
        if self.rows[row].cells[col] != cell {
            self.rows[row].cells[col] = cell;
            self.dirty_rows.insert(row);
        }
    }

    pub fn set_row_cells(&mut self, row: usize, cells: Vec<TerminalCell>) {
        if row >= self.rows.len() || cells.len() != self.cols {
            return;
        }
        if self.rows[row].cells != cells {
            self.rows[row].cells = cells;
            self.dirty_rows.insert(row);
        }
    }

    pub fn mark_row_dirty(&mut self, row: usize) {
        if row < self.rows.len() {
            self.dirty_rows.insert(row);
        }
    }

    pub fn mark_all_dirty(&mut self) {
        self.dirty_rows.extend(0..self.rows.len());
    }

    pub fn take_dirty_rows(&mut self) -> Vec<usize> {
        self.dirty_rows.iter().copied().collect::<Vec<_>>()
    }

    pub fn clear_dirty_rows(&mut self) {
        self.dirty_rows.clear();
    }
}

