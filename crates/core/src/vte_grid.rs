use vte::{Params, Perform};

/// VTE terminal grid wrapper. Parses VT100 escape sequences and maintains
/// a screen character matrix for `read --screen`.
///
/// NOTE: This grid tracks character positions only — it does NOT preserve
/// SGR attributes (colors, bold, underline, etc.). It is NOT used for
/// `attach` screen redraw. Attach transmits raw PTY bytes directly so the
/// client's real terminal handles all rendering, including colors.
pub struct VteGrid {
    rows: usize,
    cols: usize,
    grid: Vec<Vec<Cell>>,
    cursor_row: usize,
    cursor_col: usize,
}

#[derive(Clone, Debug)]
pub struct Cell {
    pub ch: char,
}

impl Default for Cell {
    fn default() -> Self {
        Cell { ch: ' ' }
    }
}

impl VteGrid {
    pub fn new(rows: u16, cols: u16) -> Self {
        let rows = rows as usize;
        let cols = cols as usize;
        VteGrid {
            rows,
            cols,
            grid: new_grid(rows, cols),
            cursor_row: 0,
            cursor_col: 0,
        }
    }

    /// Feed raw PTY output bytes into the VTE parser.
    pub fn process(&mut self, bytes: &[u8]) {
        let mut parser = vte::Parser::new();
        let mut performer = GridPerformer {
            rows: self.rows,
            cols: self.cols,
            grid: &mut self.grid,
            cursor_row: self.cursor_row,
            cursor_col: self.cursor_col,
        };
        for &byte in bytes {
            parser.advance(&mut performer, byte);
        }
        self.cursor_row = performer.cursor_row;
        self.cursor_col = performer.cursor_col;
        self.rows = performer.rows;
        self.cols = performer.cols;
    }

    /// Return the current visible screen lines (trimmed trailing spaces).
    pub fn screen(&self) -> Vec<String> {
        self.grid
            .iter()
            .map(|row| {
                let s: String = row.iter().map(|c| c.ch).collect();
                s.trim_end().to_string()
            })
            .collect()
    }

    /// Return cursor position (row, col).
    pub fn cursor(&self) -> (usize, usize) {
        (self.cursor_row, self.cursor_col)
    }

    /// Resize the grid.
    pub fn resize(&mut self, new_rows: u16, new_cols: u16) {
        let new_rows = new_rows as usize;
        let new_cols = new_cols as usize;
        self.rows = new_rows;
        self.cols = new_cols;
        self.grid = new_grid(new_rows, new_cols);
        if self.cursor_row >= new_rows {
            self.cursor_row = new_rows.saturating_sub(1);
        }
        if self.cursor_col >= new_cols {
            self.cursor_col = new_cols.saturating_sub(1);
        }
    }

    /// Generate VT100 escape sequences for a full screen redraw.
    pub fn full_redraw_bytes(&self) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(b"\x1b[H\x1b[2J");
        for (i, row) in self.grid.iter().enumerate() {
            if i > 0 {
                out.push(b'\n');
            }
            let line: String = row.iter().map(|c| c.ch).collect();
            let trimmed = line.trim_end();
            out.extend_from_slice(trimmed.as_bytes());
        }
        out.extend_from_slice(
            format!(
                "\x1b[{};{}H",
                self.cursor_row + 1,
                self.cursor_col + 1
            )
            .as_bytes(),
        );
        out
    }
}

fn new_grid(rows: usize, cols: usize) -> Vec<Vec<Cell>> {
    (0..rows).map(|_| vec![Cell::default(); cols]).collect()
}

/// VTE performer that mutates the grid.
struct GridPerformer<'a> {
    rows: usize,
    cols: usize,
    grid: &'a mut Vec<Vec<Cell>>,
    cursor_row: usize,
    cursor_col: usize,
}

impl<'a> GridPerformer<'a> {
    fn put_char(&mut self, ch: char) {
        if self.cursor_row >= self.rows || self.cursor_col >= self.cols {
            return;
        }
        (*self.grid)[self.cursor_row][self.cursor_col].ch = ch;
        self.cursor_col += 1;
        if self.cursor_col >= self.cols {
            self.cursor_col = 0;
            self.line_feed();
        }
    }

    fn line_feed(&mut self) {
        if self.cursor_row + 1 >= self.rows {
            (*self.grid).remove(0);
            (*self.grid).push(vec![Cell::default(); self.cols]);
        } else {
            self.cursor_row += 1;
        }
    }

    fn carriage_return(&mut self) {
        self.cursor_col = 0;
    }
}

impl<'a> Perform for GridPerformer<'a> {
    fn print(&mut self, c: char) {
        self.put_char(c);
    }

    fn execute(&mut self, byte: u8) {
        match byte {
            0x0a => self.line_feed(),
            0x0d => self.carriage_return(),
            0x08 => {
                if self.cursor_col > 0 {
                    self.cursor_col -= 1;
                }
            }
            0x09 => {
                let next = ((self.cursor_col / 8) + 1) * 8;
                self.cursor_col = next.min(self.cols - 1);
            }
            _ => {}
        }
    }

    fn csi_dispatch(&mut self, params: &Params, _intermediates: &[u8], _ignore: bool, action: char) {
        match action {
            'H' | 'f' => {
                let row = params.iter().next().and_then(|p| p.first()).copied().unwrap_or(1);
                let col = params.iter().nth(1).and_then(|p| p.first()).copied().unwrap_or(1);
                self.cursor_row = (row as usize).saturating_sub(1).min(self.rows - 1);
                self.cursor_col = (col as usize).saturating_sub(1).min(self.cols - 1);
            }
            'A' => {
                let n = params.iter().next().and_then(|p| p.first()).copied().unwrap_or(1);
                self.cursor_row = self.cursor_row.saturating_sub(n as usize);
            }
            'B' => {
                let n = params.iter().next().and_then(|p| p.first()).copied().unwrap_or(1);
                self.cursor_row = (self.cursor_row + n as usize).min(self.rows - 1);
            }
            'C' => {
                let n = params.iter().next().and_then(|p| p.first()).copied().unwrap_or(1);
                self.cursor_col = (self.cursor_col + n as usize).min(self.cols - 1);
            }
            'D' => {
                let n = params.iter().next().and_then(|p| p.first()).copied().unwrap_or(1);
                self.cursor_col = self.cursor_col.saturating_sub(n as usize);
            }
            'J' => {
                let mode = params.iter().next().and_then(|p| p.first()).copied().unwrap_or(0);
                match mode {
                    0 => {
                        for col in self.cursor_col..self.cols {
                            (*self.grid)[self.cursor_row][col] = Cell::default();
                        }
                        for row in (self.cursor_row + 1)..self.rows {
                            for col in 0..self.cols {
                                (*self.grid)[row][col] = Cell::default();
                            }
                        }
                    }
                    1 => {
                        for row in 0..self.cursor_row {
                            for col in 0..self.cols {
                                (*self.grid)[row][col] = Cell::default();
                            }
                        }
                        for col in 0..=self.cursor_col {
                            (*self.grid)[self.cursor_row][col] = Cell::default();
                        }
                    }
                    2 => {
                        *self.grid = new_grid(self.rows, self.cols);
                    }
                    _ => {}
                }
            }
            'K' => {
                let mode = params.iter().next().and_then(|p| p.first()).copied().unwrap_or(0);
                match mode {
                    0 => {
                        for col in self.cursor_col..self.cols {
                            (*self.grid)[self.cursor_row][col] = Cell::default();
                        }
                    }
                    1 => {
                        for col in 0..=self.cursor_col {
                            (*self.grid)[self.cursor_row][col] = Cell::default();
                        }
                    }
                    2 => {
                        for col in 0..self.cols {
                            (*self.grid)[self.cursor_row][col] = Cell::default();
                        }
                    }
                    _ => {}
                }
            }
            'm' => {} // SGR - ignore attributes
            _ => {}
        }
    }

    fn esc_dispatch(&mut self, _intermediates: &[u8], _ignore: bool, _byte: u8) {}

    fn osc_dispatch(&mut self, _params: &[&[u8]], _bell_terminated: bool) {}

    fn hook(&mut self, _params: &Params, _intermediates: &[u8], _ignore: bool, _action: char) {}
    fn unhook(&mut self) {}
    fn put(&mut self, _byte: u8) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basic_text() {
        let mut grid = VteGrid::new(24, 80);
        grid.process(b"hello\n");
        let screen = grid.screen();
        assert_eq!(screen[0], "hello");
    }

    #[test]
    fn cursor_movement() {
        let mut grid = VteGrid::new(24, 80);
        grid.process(b"hello");
        assert_eq!(grid.cursor(), (0, 5));
    }

    #[test]
    fn line_wrap() {
        let mut grid = VteGrid::new(24, 5);
        grid.process(b"123456");
        assert_eq!(grid.screen()[0], "12345");
        assert_eq!(grid.screen()[1], "6");
    }

    #[test]
    fn scroll() {
        let mut grid = VteGrid::new(3, 80);
        grid.process(b"line1\r\nline2\r\nline3\r\nline4\r\n");
        let screen = grid.screen();
        assert_eq!(screen[0], "line3");
        assert_eq!(screen[1], "line4");
        assert_eq!(screen[2], "");
    }

    #[test]
    fn full_redraw() {
        let mut grid = VteGrid::new(24, 80);
        grid.process(b"test");
        let bytes = grid.full_redraw_bytes();
        assert!(!bytes.is_empty());
        assert!(bytes.starts_with(b"\x1b[H\x1b[2J"));
    }

    #[test]
    fn resize() {
        let mut grid = VteGrid::new(24, 80);
        grid.process(b"hello");
        grid.resize(40, 120);
        assert_eq!(grid.rows, 40);
        assert_eq!(grid.cols, 120);
    }
}
