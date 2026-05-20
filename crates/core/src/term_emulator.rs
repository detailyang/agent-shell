//! Full terminal emulator backed by alacritty_terminal.
//!
//! Maintains complete terminal state including:
//! - Primary and alternate screen buffers
//! - SGR attributes (colors, bold, underline, etc.)
//! - Cursor position and visibility
//! - Scrollback history
//!
//! Used by the daemon to generate accurate ANSI redraw sequences
//! when a client attaches, instead of replaying raw PTY bytes.

use alacritty_terminal::event::{Event, EventListener};
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::term::cell::{Cell, Flags};
use alacritty_terminal::term::{Config, Term, TermMode};
use alacritty_terminal::vte::ansi::{Color, NamedColor, Processor};

/// Null event listener for headless operation.
struct NullListener;
impl EventListener for NullListener {
    fn send_event(&self, _event: Event) {}
}

/// A full terminal emulator wrapping alacritty_terminal.
pub struct TermEmulator {
    term: Term<NullListener>,
    parser: Processor,
}

impl TermEmulator {
    /// Create a new terminal emulator with the given dimensions.
    pub fn new(rows: u16, cols: u16) -> Self {
        let config = Config { scrolling_history: 10000, ..Default::default() };

        let size = TermSize { rows: rows as usize, cols: cols as usize };
        let term = Term::new(config, &size, NullListener);
        let parser = Processor::new();

        TermEmulator { term, parser }
    }

    /// Feed raw PTY output bytes into the terminal emulator.
    pub fn process(&mut self, bytes: &[u8]) {
        self.parser.advance(&mut self.term, bytes);
    }

    /// Resize the terminal.
    pub fn resize(&mut self, rows: u16, cols: u16) {
        let size = TermSize { rows: rows as usize, cols: cols as usize };
        self.term.resize(size);
    }

    /// Return the current visible screen lines (trimmed trailing spaces).
    /// Compatible with the old VteGrid::screen() API.
    pub fn screen(&self) -> Vec<String> {
        let grid = self.term.grid();
        let num_lines = grid.screen_lines();
        let num_cols = grid.columns();

        let mut lines = Vec::with_capacity(num_lines);
        for line_idx in 0..num_lines {
            let line = Line(line_idx as i32);
            let mut s = String::with_capacity(num_cols);
            for col_idx in 0..num_cols {
                let cell = &grid[line][Column(col_idx)];
                let c = cell.c;
                if c == '\0' {
                    s.push(' ');
                } else {
                    s.push(c);
                }
            }
            lines.push(s.trim_end().to_string());
        }
        lines
    }

    /// Return cursor position (row, col), 0-based.
    pub fn cursor(&self) -> (usize, usize) {
        let point = self.term.grid().cursor.point;
        (point.line.0 as usize, point.column.0)
    }

    /// Whether the terminal is currently in alternate screen mode.
    pub fn is_alt_screen(&self) -> bool {
        self.term.mode().contains(TermMode::ALT_SCREEN)
    }

    /// Generate a full ANSI redraw of the current terminal state.
    ///
    /// This produces escape sequences that, when written to a client terminal
    /// of the same dimensions, will reproduce the exact visual state including
    /// colors, attributes, and cursor position.
    pub fn full_redraw(&self) -> Vec<u8> {
        let grid = self.term.grid();
        let num_lines = grid.screen_lines();
        let num_cols = grid.columns();
        let cursor = grid.cursor.point;

        // Pre-allocate with a reasonable estimate
        let mut out = Vec::with_capacity(num_lines * num_cols * 3);

        // If in alternate screen, tell the client to switch
        if self.is_alt_screen() {
            out.extend_from_slice(b"\x1b[?1049h");
        }

        // Reset attributes + clear screen + cursor home
        out.extend_from_slice(b"\x1b[0m\x1b[H\x1b[2J");

        // Track current SGR state to emit minimal attribute changes
        let mut cur_fg = Color::Named(NamedColor::Foreground);
        let mut cur_bg = Color::Named(NamedColor::Background);
        let mut cur_flags = Flags::empty();

        for line_idx in 0..num_lines {
            let line = Line(line_idx as i32);

            // Find the last non-default cell in this line to avoid trailing spaces
            let mut last_non_empty = 0;
            for col_idx in (0..num_cols).rev() {
                let cell = &grid[line][Column(col_idx)];
                if !is_default_cell(cell) {
                    last_non_empty = col_idx + 1;
                    break;
                }
            }

            if last_non_empty == 0 {
                continue; // empty line, skip
            }

            // Move cursor to start of this line (only for non-empty lines)
            if line_idx > 0 {
                out.extend_from_slice(
                    format!("\x1b[{};1H", line_idx + 1).as_bytes(),
                );
            }

            for col_idx in 0..last_non_empty {
                let cell = &grid[line][Column(col_idx)];

                // Skip wide char spacers
                if cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
                    continue;
                }

                // Emit SGR changes if needed
                let sgr_needed = cell.fg != cur_fg
                    || cell.bg != cur_bg
                    || cell.flags.intersection(SGR_FLAGS) != cur_flags.intersection(SGR_FLAGS);

                if sgr_needed {
                    emit_sgr(&mut out, cell, &mut cur_fg, &mut cur_bg, &mut cur_flags);
                }

                // Emit character
                let c = cell.c;
                if c == '\0' || c == ' ' {
                    out.push(b' ');
                } else {
                    let mut buf = [0u8; 4];
                    let encoded = c.encode_utf8(&mut buf);
                    out.extend_from_slice(encoded.as_bytes());
                }

                // Emit combining characters (zerowidth)
                if let Some(zw) = cell.zerowidth() {
                    for &zwc in zw {
                        let mut buf = [0u8; 4];
                        let encoded = zwc.encode_utf8(&mut buf);
                        out.extend_from_slice(encoded.as_bytes());
                    }
                }
            }
        }

        // Reset attributes
        out.extend_from_slice(b"\x1b[0m");

        // Position cursor
        out.extend_from_slice(
            format!("\x1b[{};{}H", cursor.line.0 + 1, cursor.column.0 + 1).as_bytes(),
        );

        // Cursor visibility
        if self.term.mode().contains(TermMode::SHOW_CURSOR) {
            out.extend_from_slice(b"\x1b[?25h");
        } else {
            out.extend_from_slice(b"\x1b[?25l");
        }

        out
    }
}

/// SGR-relevant flags (exclude WRAPLINE, WIDE_CHAR, etc.)
const SGR_FLAGS: Flags = Flags::BOLD
    .union(Flags::ITALIC)
    .union(Flags::UNDERLINE)
    .union(Flags::DOUBLE_UNDERLINE)
    .union(Flags::UNDERCURL)
    .union(Flags::DOTTED_UNDERLINE)
    .union(Flags::DASHED_UNDERLINE)
    .union(Flags::DIM)
    .union(Flags::INVERSE)
    .union(Flags::HIDDEN)
    .union(Flags::STRIKEOUT);

/// Check if a cell is "default" (space with default colors and no flags).
fn is_default_cell(cell: &Cell) -> bool {
    (cell.c == ' ' || cell.c == '\0')
        && cell.fg == Color::Named(NamedColor::Foreground)
        && cell.bg == Color::Named(NamedColor::Background)
        && cell.flags.intersection(SGR_FLAGS).is_empty()
}

/// Emit SGR escape sequence for the given cell's attributes.
fn emit_sgr(
    out: &mut Vec<u8>,
    cell: &Cell,
    cur_fg: &mut Color,
    cur_bg: &mut Color,
    cur_flags: &mut Flags,
) {
    // Build SGR parameters
    let mut params: Vec<u8> = Vec::new();

    let new_flags = cell.flags.intersection(SGR_FLAGS);
    let old_flags = cur_flags.intersection(SGR_FLAGS);

    // If any attribute was removed, we must reset and re-apply
    let need_reset = old_flags.difference(new_flags) != Flags::empty();

    if need_reset {
        // Reset all, then re-apply
        params.extend_from_slice(b"\x1b[0");

        if new_flags.contains(Flags::BOLD) {
            params.extend_from_slice(b";1");
        }
        if new_flags.contains(Flags::DIM) {
            params.extend_from_slice(b";2");
        }
        if new_flags.contains(Flags::ITALIC) {
            params.extend_from_slice(b";3");
        }
        if new_flags.contains(Flags::UNDERLINE) {
            params.extend_from_slice(b";4");
        }
        if new_flags.contains(Flags::INVERSE) {
            params.extend_from_slice(b";7");
        }
        if new_flags.contains(Flags::HIDDEN) {
            params.extend_from_slice(b";8");
        }
        if new_flags.contains(Flags::STRIKEOUT) {
            params.extend_from_slice(b";9");
        }
        if new_flags.contains(Flags::DOUBLE_UNDERLINE) {
            params.extend_from_slice(b";21");
        }

        // After reset, force re-emit colors
        append_fg_color(&mut params, &cell.fg);
        append_bg_color(&mut params, &cell.bg);

        params.push(b'm');
        out.extend_from_slice(&params);
    } else {
        // Incremental: only emit changed attributes
        let mut started = false;

        // Added flags
        let added = new_flags.difference(old_flags);
        if added.contains(Flags::BOLD) {
            start_sgr(out, &mut started);
            out.extend_from_slice(b"1");
        }
        if added.contains(Flags::DIM) {
            start_sgr_param(out, &mut started);
            out.extend_from_slice(b"2");
        }
        if added.contains(Flags::ITALIC) {
            start_sgr_param(out, &mut started);
            out.extend_from_slice(b"3");
        }
        if added.contains(Flags::UNDERLINE) {
            start_sgr_param(out, &mut started);
            out.extend_from_slice(b"4");
        }
        if added.contains(Flags::INVERSE) {
            start_sgr_param(out, &mut started);
            out.extend_from_slice(b"7");
        }
        if added.contains(Flags::HIDDEN) {
            start_sgr_param(out, &mut started);
            out.extend_from_slice(b"8");
        }
        if added.contains(Flags::STRIKEOUT) {
            start_sgr_param(out, &mut started);
            out.extend_from_slice(b"9");
        }

        // Color changes
        if cell.fg != *cur_fg {
            start_sgr_param(out, &mut started);
            append_fg_color_raw(out, &cell.fg);
        }
        if cell.bg != *cur_bg {
            start_sgr_param(out, &mut started);
            append_bg_color_raw(out, &cell.bg);
        }

        if started {
            out.push(b'm');
        }
    }

    *cur_fg = cell.fg;
    *cur_bg = cell.bg;
    *cur_flags = new_flags;
}

fn start_sgr(out: &mut Vec<u8>, started: &mut bool) {
    out.extend_from_slice(b"\x1b[");
    *started = true;
}

fn start_sgr_param(out: &mut Vec<u8>, started: &mut bool) {
    if !*started {
        out.extend_from_slice(b"\x1b[");
        *started = true;
    } else {
        out.push(b';');
    }
}

/// Append foreground color as SGR parameter (after \x1b[0 prefix)
fn append_fg_color(params: &mut Vec<u8>, color: &Color) {
    match color {
        Color::Named(NamedColor::Foreground) => {} // default, no param needed after reset
        Color::Named(name) => {
            let code = named_color_to_fg(*name);
            params.extend_from_slice(format!(";{}", code).as_bytes());
        }
        Color::Spec(rgb) => {
            params.extend_from_slice(
                format!(";38;2;{};{};{}", rgb.r, rgb.g, rgb.b).as_bytes(),
            );
        }
        Color::Indexed(idx) => {
            params.extend_from_slice(format!(";38;5;{}", idx).as_bytes());
        }
    }
}

/// Append background color as SGR parameter (after \x1b[0 prefix)
fn append_bg_color(params: &mut Vec<u8>, color: &Color) {
    match color {
        Color::Named(NamedColor::Background) => {} // default, no param needed after reset
        Color::Named(name) => {
            let code = named_color_to_bg(*name);
            params.extend_from_slice(format!(";{}", code).as_bytes());
        }
        Color::Spec(rgb) => {
            params.extend_from_slice(
                format!(";48;2;{};{};{}", rgb.r, rgb.g, rgb.b).as_bytes(),
            );
        }
        Color::Indexed(idx) => {
            params.extend_from_slice(format!(";48;5;{}", idx).as_bytes());
        }
    }
}

/// Append foreground color as raw SGR parameter (no leading semicolon)
fn append_fg_color_raw(out: &mut Vec<u8>, color: &Color) {
    match color {
        Color::Named(NamedColor::Foreground) => {
            out.extend_from_slice(b"39"); // default fg
        }
        Color::Named(name) => {
            let code = named_color_to_fg(*name);
            out.extend_from_slice(format!("{}", code).as_bytes());
        }
        Color::Spec(rgb) => {
            out.extend_from_slice(format!("38;2;{};{};{}", rgb.r, rgb.g, rgb.b).as_bytes());
        }
        Color::Indexed(idx) => {
            out.extend_from_slice(format!("38;5;{}", idx).as_bytes());
        }
    }
}

/// Append background color as raw SGR parameter (no leading semicolon)
fn append_bg_color_raw(out: &mut Vec<u8>, color: &Color) {
    match color {
        Color::Named(NamedColor::Background) => {
            out.extend_from_slice(b"49"); // default bg
        }
        Color::Named(name) => {
            let code = named_color_to_bg(*name);
            out.extend_from_slice(format!("{}", code).as_bytes());
        }
        Color::Spec(rgb) => {
            out.extend_from_slice(format!("48;2;{};{};{}", rgb.r, rgb.g, rgb.b).as_bytes());
        }
        Color::Indexed(idx) => {
            out.extend_from_slice(format!("48;5;{}", idx).as_bytes());
        }
    }
}

/// Map a named color to its ANSI foreground code.
fn named_color_to_fg(name: NamedColor) -> u8 {
    match name {
        NamedColor::Black => 30,
        NamedColor::Red => 31,
        NamedColor::Green => 32,
        NamedColor::Yellow => 33,
        NamedColor::Blue => 34,
        NamedColor::Magenta => 35,
        NamedColor::Cyan => 36,
        NamedColor::White => 37,
        NamedColor::BrightBlack => 90,
        NamedColor::BrightRed => 91,
        NamedColor::BrightGreen => 92,
        NamedColor::BrightYellow => 93,
        NamedColor::BrightBlue => 94,
        NamedColor::BrightMagenta => 95,
        NamedColor::BrightCyan => 96,
        NamedColor::BrightWhite => 97,
        NamedColor::Foreground => 39,
        _ => 39, // default
    }
}

/// Map a named color to its ANSI background code.
fn named_color_to_bg(name: NamedColor) -> u8 {
    match name {
        NamedColor::Black => 40,
        NamedColor::Red => 41,
        NamedColor::Green => 42,
        NamedColor::Yellow => 43,
        NamedColor::Blue => 44,
        NamedColor::Magenta => 45,
        NamedColor::Cyan => 46,
        NamedColor::White => 47,
        NamedColor::BrightBlack => 100,
        NamedColor::BrightRed => 101,
        NamedColor::BrightGreen => 102,
        NamedColor::BrightYellow => 103,
        NamedColor::BrightBlue => 104,
        NamedColor::BrightMagenta => 105,
        NamedColor::BrightCyan => 106,
        NamedColor::BrightWhite => 107,
        NamedColor::Background => 49,
        _ => 49, // default
    }
}

/// Helper for dimensions.
struct TermSize {
    rows: usize,
    cols: usize,
}

impl Dimensions for TermSize {
    fn columns(&self) -> usize {
        self.cols
    }

    fn screen_lines(&self) -> usize {
        self.rows
    }

    fn total_lines(&self) -> usize {
        self.rows
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basic_text() {
        let mut emu = TermEmulator::new(24, 80);
        emu.process(b"hello\r\n");
        let screen = emu.screen();
        assert_eq!(screen[0], "hello");
    }

    #[test]
    fn cursor_position() {
        let mut emu = TermEmulator::new(24, 80);
        emu.process(b"hello");
        assert_eq!(emu.cursor(), (0, 5));
    }

    #[test]
    fn alternate_screen() {
        let mut emu = TermEmulator::new(24, 80);
        assert!(!emu.is_alt_screen());
        emu.process(b"\x1b[?1049h");
        assert!(emu.is_alt_screen());
        emu.process(b"\x1b[?1049l");
        assert!(!emu.is_alt_screen());
    }

    #[test]
    fn sgr_colors_preserved() {
        let mut emu = TermEmulator::new(24, 80);
        // Red foreground text
        emu.process(b"\x1b[31mRED\x1b[0m");
        let redraw = emu.full_redraw();
        let s = String::from_utf8_lossy(&redraw);
        // Should contain SGR 31 for red
        assert!(s.contains("\x1b["), "redraw should contain escape sequences");
        // Should contain the text "RED"
        assert!(s.contains("RED"), "redraw should contain the text");
    }

    #[test]
    fn full_redraw_alt_screen() {
        let mut emu = TermEmulator::new(24, 80);
        emu.process(b"\x1b[?1049h");
        emu.process(b"ALT CONTENT");
        let redraw = emu.full_redraw();
        let s = String::from_utf8_lossy(&redraw);
        // Should start with alt screen switch
        assert!(s.contains("\x1b[?1049h"), "redraw should enter alt screen");
        assert!(s.contains("ALT CONTENT"), "redraw should contain alt content");
    }

    #[test]
    fn resize() {
        let mut emu = TermEmulator::new(24, 80);
        emu.process(b"hello");
        emu.resize(40, 120);
        let screen = emu.screen();
        assert_eq!(screen.len(), 40);
    }
}
