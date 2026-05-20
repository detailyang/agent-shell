/// Terminal raw mode and size utilities shared by `recording` (replay) and CLI (attach).

/// RAII guard that restores the original terminal settings on drop.
pub struct RawModeGuard {
    original: nix::sys::termios::Termios,
}

impl Drop for RawModeGuard {
    fn drop(&mut self) {
        let stdin_fd = unsafe { std::os::unix::io::BorrowedFd::borrow_raw(0) };
        let _ = nix::sys::termios::tcsetattr(
            &stdin_fd,
            nix::sys::termios::SetArg::TCSADRAIN,
            &self.original,
        );
    }
}

/// Put stdin into raw mode. Returns a guard that restores the original
/// settings when dropped. Returns `None` if stdin is not a TTY.
pub fn enter_raw_mode() -> Option<RawModeGuard> {
    if !std::io::IsTerminal::is_terminal(&std::io::stdin()) {
        return None;
    }
    let stdin_fd = unsafe { std::os::unix::io::BorrowedFd::borrow_raw(0) };
    let original = nix::sys::termios::tcgetattr(&stdin_fd).ok()?;
    let mut raw = original.clone();
    nix::sys::termios::cfmakeraw(&mut raw);
    nix::sys::termios::tcsetattr(&stdin_fd, nix::sys::termios::SetArg::TCSANOW, &raw).ok()?;
    Some(RawModeGuard { original })
}

/// Return the current terminal size of stdout as `(rows, cols)`.
/// Returns `None` if stdout is not a TTY or the ioctl fails.
pub fn terminal_size() -> Option<(u16, u16)> {
    // SAFETY: stdout fd (1) is valid for the lifetime of this process.
    let mut ws: libc::winsize = unsafe { std::mem::zeroed() };
    let ret = unsafe { libc::ioctl(1, libc::TIOCGWINSZ, &mut ws) };
    if ret == 0 && (ws.ws_row > 0 || ws.ws_col > 0) {
        Some((ws.ws_row, ws.ws_col))
    } else {
        None
    }
}

/// Resize the stdout terminal to `rows` × `cols` via `TIOCSWINSZ`.
/// Returns `true` on success, `false` if the ioctl fails or stdout is not a TTY.
pub fn set_terminal_size(rows: u16, cols: u16) -> bool {
    let ws = libc::winsize {
        ws_row: rows,
        ws_col: cols,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    // SAFETY: stdout fd (1) is valid; winsize is fully initialised above.
    let ret = unsafe { libc::ioctl(1, libc::TIOCSWINSZ, &ws) };
    ret == 0
}
