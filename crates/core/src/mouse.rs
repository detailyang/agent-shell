/// SGR mouse event encoding for PTY input.
///
/// All coordinates are 1-based (col, row) matching terminal convention.
/// Encodes events in SGR format: `\x1b[<{button};{col};{row}{M|m}`

/// Mouse button identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseButton {
    Left,
    Middle,
    Right,
}

impl MouseButton {
    /// SGR button code for press events.
    fn press_code(self) -> u8 {
        match self {
            MouseButton::Left => 0,
            MouseButton::Middle => 1,
            MouseButton::Right => 2,
        }
    }

    /// SGR button code for motion (drag) events = press_code + 32.
    fn motion_code(self) -> u8 {
        self.press_code() + 32
    }
}

/// Parse a button name string into MouseButton.
pub fn parse_button(s: &str) -> Result<MouseButton, String> {
    match s {
        "left" => Ok(MouseButton::Left),
        "middle" => Ok(MouseButton::Middle),
        "right" => Ok(MouseButton::Right),
        _ => Err(format!("unknown button: '{}' (expected left|middle|right)", s)),
    }
}

/// Scroll direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScrollDirection {
    Up,
    Down,
}

impl ScrollDirection {
    /// SGR button code for scroll events.
    fn code(self) -> u8 {
        match self {
            ScrollDirection::Up => 64,
            ScrollDirection::Down => 65,
        }
    }
}

/// Parse a direction string into ScrollDirection.
pub fn parse_direction(s: &str) -> Result<ScrollDirection, String> {
    match s {
        "up" => Ok(ScrollDirection::Up),
        "down" => Ok(ScrollDirection::Down),
        _ => Err(format!("unknown direction: '{}' (expected up|down)", s)),
    }
}

/// Encode a single SGR mouse event.
/// `suffix`: `M` for press/motion, `m` for release.
fn encode_sgr(button_code: u8, x: u16, y: u16, suffix: char) -> Vec<u8> {
    format!("\x1b[<{};{};{}{}", button_code, x, y, suffix).into_bytes()
}

/// Encode a button press event.
pub fn encode_press(button: MouseButton, x: u16, y: u16) -> Vec<u8> {
    encode_sgr(button.press_code(), x, y, 'M')
}

/// Encode a button release event.
pub fn encode_release(button: MouseButton, x: u16, y: u16) -> Vec<u8> {
    encode_sgr(button.press_code(), x, y, 'm')
}

/// Encode a click (press + release), repeated `count` times.
pub fn encode_click(button: MouseButton, x: u16, y: u16, count: u16) -> Vec<Vec<u8>> {
    let mut seqs = Vec::with_capacity(count as usize * 2);
    for _ in 0..count {
        seqs.push(encode_press(button, x, y));
        seqs.push(encode_release(button, x, y));
    }
    seqs
}

/// Encode scroll events, repeated `count` times.
/// Each scroll event is a press + release pair.
pub fn encode_scroll(direction: ScrollDirection, x: u16, y: u16, count: u16) -> Vec<Vec<u8>> {
    let mut seqs = Vec::with_capacity(count as usize);
    for _ in 0..count {
        seqs.push(encode_sgr(direction.code(), x, y, 'M'));
    }
    seqs
}

/// Encode a motion (move) event with a button held.
pub fn encode_move(button: MouseButton, x: u16, y: u16) -> Vec<u8> {
    encode_sgr(button.motion_code(), x, y, 'M')
}

/// Encode a drag sequence: press(from) → interpolated moves → release(to).
///
/// `steps` controls the number of intermediate move events (clamped >= 1).
/// The interpolation is linear between from and to coordinates.
pub fn encode_drag(
    button: MouseButton,
    from_x: u16,
    from_y: u16,
    to_x: u16,
    to_y: u16,
    steps: u16,
) -> Vec<Vec<u8>> {
    let steps = steps.max(1);
    // press + steps moves + release
    let mut seqs = Vec::with_capacity(2 + steps as usize);

    // Press at start
    seqs.push(encode_press(button, from_x, from_y));

    // Interpolated move events
    for i in 1..=steps {
        let t = i as f64 / steps as f64;
        let x = from_x as f64 + (to_x as f64 - from_x as f64) * t;
        let y = from_y as f64 + (to_y as f64 - from_y as f64) * t;
        let ix = (x.round() as u16).max(1);
        let iy = (y.round() as u16).max(1);
        seqs.push(encode_move(button, ix, iy));
    }

    // Release at end
    seqs.push(encode_release(button, to_x, to_y));

    seqs
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn press_left() {
        assert_eq!(encode_press(MouseButton::Left, 10, 5), b"\x1b[<0;10;5M");
    }

    #[test]
    fn press_middle() {
        assert_eq!(encode_press(MouseButton::Middle, 1, 1), b"\x1b[<1;1;1M");
    }

    #[test]
    fn press_right() {
        assert_eq!(encode_press(MouseButton::Right, 80, 24), b"\x1b[<2;80;24M");
    }

    #[test]
    fn release_left() {
        assert_eq!(encode_release(MouseButton::Left, 10, 5), b"\x1b[<0;10;5m");
    }

    #[test]
    fn release_middle() {
        assert_eq!(encode_release(MouseButton::Middle, 3, 7), b"\x1b[<1;3;7m");
    }

    #[test]
    fn release_right() {
        assert_eq!(encode_release(MouseButton::Right, 1, 1), b"\x1b[<2;1;1m");
    }

    #[test]
    fn click_single() {
        let seqs = encode_click(MouseButton::Left, 10, 5, 1);
        assert_eq!(seqs.len(), 2);
        assert_eq!(seqs[0], b"\x1b[<0;10;5M");
        assert_eq!(seqs[1], b"\x1b[<0;10;5m");
    }

    #[test]
    fn click_triple() {
        let seqs = encode_click(MouseButton::Left, 10, 5, 3);
        assert_eq!(seqs.len(), 6);
        // Each pair is press+release
        for i in 0..3 {
            assert_eq!(seqs[i * 2], b"\x1b[<0;10;5M");
            assert_eq!(seqs[i * 2 + 1], b"\x1b[<0;10;5m");
        }
    }

    #[test]
    fn scroll_up() {
        let seqs = encode_scroll(ScrollDirection::Up, 10, 5, 3);
        assert_eq!(seqs.len(), 3);
        for s in &seqs {
            assert_eq!(s, b"\x1b[<64;10;5M");
        }
    }

    #[test]
    fn scroll_down() {
        let seqs = encode_scroll(ScrollDirection::Down, 10, 5, 1);
        assert_eq!(seqs.len(), 1);
        assert_eq!(seqs[0], b"\x1b[<65;10;5M");
    }

    #[test]
    fn move_left_drag() {
        assert_eq!(encode_move(MouseButton::Left, 15, 10), b"\x1b[<32;15;10M");
    }

    #[test]
    fn move_right_drag() {
        assert_eq!(encode_move(MouseButton::Right, 5, 3), b"\x1b[<34;5;3M");
    }

    #[test]
    fn drag_basic() {
        let seqs = encode_drag(MouseButton::Left, 1, 1, 5, 5, 4);
        // press + 4 moves + release = 6
        assert_eq!(seqs.len(), 6);
        // First is press at (1,1)
        assert_eq!(seqs[0], b"\x1b[<0;1;1M");
        // Last is release at (5,5)
        assert_eq!(seqs[5], b"\x1b[<0;5;5m");
        // Middle events are motion with button+32
        for seq in &seqs[1..5] {
            assert!(seq.starts_with(b"\x1b[<32;"));
            assert!(seq.ends_with(b"M"));
        }
        // Final move should reach (5,5)
        assert_eq!(seqs[4], b"\x1b[<32;5;5M");
    }

    #[test]
    fn drag_steps_clamped_to_one() {
        let seqs = encode_drag(MouseButton::Left, 1, 1, 10, 10, 0);
        // steps clamped to 1: press + 1 move + release = 3
        assert_eq!(seqs.len(), 3);
        assert_eq!(seqs[0], b"\x1b[<0;1;1M");
        assert_eq!(seqs[1], b"\x1b[<32;10;10M");
        assert_eq!(seqs[2], b"\x1b[<0;10;10m");
    }

    #[test]
    fn large_coordinates() {
        // SGR has no 223 limit
        assert_eq!(
            encode_press(MouseButton::Left, 1000, 500),
            b"\x1b[<0;1000;500M"
        );
    }

    #[test]
    fn parse_button_valid() {
        assert_eq!(parse_button("left").unwrap(), MouseButton::Left);
        assert_eq!(parse_button("middle").unwrap(), MouseButton::Middle);
        assert_eq!(parse_button("right").unwrap(), MouseButton::Right);
    }

    #[test]
    fn parse_button_invalid() {
        assert!(parse_button("unknown").is_err());
    }

    #[test]
    fn parse_direction_valid() {
        assert_eq!(parse_direction("up").unwrap(), ScrollDirection::Up);
        assert_eq!(parse_direction("down").unwrap(), ScrollDirection::Down);
    }

    #[test]
    fn parse_direction_invalid() {
        assert!(parse_direction("left").is_err());
    }
}
