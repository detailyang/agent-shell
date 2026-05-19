/// Ring buffer for session output, with per-client cursor tracking.
///
/// Default capacity: 524288 bytes (512KB). Hard minimum: 4096 bytes (4KB).
/// When full, oldest bytes are overwritten. `write_cursor` is monotonically
/// increasing, so clients can detect gaps.
pub struct RingBuffer {
    buf: Vec<u8>,
    capacity: usize,
    write_cursor: u64,
    start: usize, // index in buf where the oldest valid byte lives
    overflowed: bool,
    total_lost: u64,
}

pub const DEFAULT_BUFFER_SIZE: usize = 524288;
pub const MIN_BUFFER_SIZE: usize = 4096;

impl RingBuffer {
    pub fn new(capacity: usize) -> Self {
        let capacity = capacity.max(MIN_BUFFER_SIZE);
        RingBuffer {
            buf: vec![0; capacity],
            capacity,
            write_cursor: 0,
            start: 0,
            overflowed: false,
            total_lost: 0,
        }
    }

    pub fn capacity(&self) -> usize {
        self.capacity
    }

    pub fn write_cursor(&self) -> u64 {
        self.write_cursor
    }

    /// Write bytes into the ring buffer, overwriting oldest data if full.
    /// Returns the number of bytes overwritten (lost).
    pub fn write(&mut self, data: &[u8]) -> u64 {
        if data.is_empty() {
            return 0;
        }

        let data_len = data.len();
        let cap = self.capacity;

        // Fast path: buffer hasn't wrapped yet and can absorb all data
        if self.write_cursor < cap as u64 && (self.write_cursor as usize + data_len) <= cap {
            let start = self.write_cursor as usize;
            self.buf[start..start + data_len].copy_from_slice(data);
            self.write_cursor += data_len as u64;
            return 0;
        }

        // Slow path: may wrap or overwrite — fall back to byte-by-byte for correctness
        // (the ring buffer's `start` tracking requires per-byte overwrite detection)
        let mut lost = 0u64;

        for &byte in data {
            if self.write_cursor >= cap as u64 {
                let write_idx = (self.write_cursor % cap as u64) as usize;
                if write_idx == self.start && self.write_cursor > 0 {
                    self.start = (self.start + 1) % cap;
                    lost += 1;
                }
                self.buf[write_idx] = byte;
            } else {
                let write_idx = self.write_cursor as usize;
                self.buf[write_idx] = byte;
            }
            self.write_cursor += 1;
        }

        if lost > 0 {
            self.overflowed = true;
            self.total_lost += lost;
        }

        lost
    }

    /// Read bytes from `from_cursor` to `write_cursor`.
    /// Returns (bytes, gap, lost_bytes).
    /// If `from_cursor` is behind the current start, gap is true and
    /// lost_bytes indicates how many bytes were irrecoverably lost.
    pub fn read(&self, from_cursor: u64) -> (Vec<u8>, bool, u64) {
        if from_cursor >= self.write_cursor {
            return (Vec::new(), false, 0);
        }

        let effective_start = if self.write_cursor > self.capacity as u64 {
            self.write_cursor - self.capacity as u64
        } else {
            0
        };

        if from_cursor < effective_start {
            let lost = effective_start - from_cursor;
            let bytes = self.read_range(effective_start, self.write_cursor);
            (bytes, true, lost)
        } else {
            let bytes = self.read_range(from_cursor, self.write_cursor);
            (bytes, false, 0)
        }
    }

    fn read_range(&self, start: u64, end: u64) -> Vec<u8> {
        if start >= end {
            return Vec::new();
        }
        let len = (end - start) as usize;
        let cap = self.capacity as u64;
        let mut result = Vec::with_capacity(len);

        // Check if the range is contiguous (no wrap)
        let start_idx = (start % cap) as usize;
        let _end_idx = (end % cap) as usize;

        if start_idx + len <= self.capacity {
            // Contiguous: single slice copy
            result.extend_from_slice(&self.buf[start_idx..start_idx + len]);
        } else {
            // Wraps around: two slice copies
            let first_chunk = self.capacity - start_idx;
            result.extend_from_slice(&self.buf[start_idx..]);
            result.extend_from_slice(&self.buf[..len - first_chunk]);
        }

        result
    }

    /// Check and reset the overflowed flag. Returns (was_overflowed, total_lost_since_last_check).
    pub fn take_overflow(&mut self) -> (bool, u64) {
        let was = self.overflowed;
        let lost = self.total_lost;
        self.overflowed = false;
        self.total_lost = 0;
        (was, lost)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basic_write_read() {
        let mut rb = RingBuffer::new(4096);
        rb.write(b"hello");
        let (data, gap, lost) = rb.read(0);
        assert_eq!(data, b"hello");
        assert!(!gap);
        assert_eq!(lost, 0);
    }

    #[test]
    fn incremental_read() {
        let mut rb = RingBuffer::new(4096);
        rb.write(b"hello");
        let (data, _, _) = rb.read(0);
        assert_eq!(data, b"hello");

        rb.write(b" world");
        let (data, _, _) = rb.read(5);
        assert_eq!(data, b" world");
    }

    #[test]
    fn overflow_detects_gap() {
        // Use minimum buffer size for testing
        let mut rb = RingBuffer::new(4096);
        // Write 8KB to overflow 4KB buffer
        let data = vec![b'x'; 8192];
        rb.write(&data);
        // Reading from cursor 0 should show gap
        let (_, gap, lost) = rb.read(0);
        assert!(gap);
        assert_eq!(lost, 4096); // 8KB - 4KB = 4KB lost
    }

    #[test]
    fn recent_cursor_no_gap() {
        let mut rb = RingBuffer::new(4096);
        // Write 6KB, then read from 4KB cursor (within buffer)
        let data1 = vec![b'a'; 4096];
        rb.write(&data1);
        let data2 = vec![b'b'; 2048];
        rb.write(&data2);

        // cursor at 4096 is exactly at the start of recent data
        let (data, gap, _) = rb.read(4096);
        assert!(!gap);
        assert_eq!(data.len(), 2048);
        assert!(data.iter().all(|&b| b == b'b'));
    }

    #[test]
    fn overflow_flag() {
        let mut rb = RingBuffer::new(4096);
        rb.write(&vec![b'x'; 5000]);
        let (overflowed, _) = rb.take_overflow();
        assert!(overflowed);

        // Second check should be false
        let (overflowed, _) = rb.take_overflow();
        assert!(!overflowed);
    }

    #[test]
    fn empty_read() {
        let rb = RingBuffer::new(4096);
        let (data, gap, lost) = rb.read(0);
        assert!(data.is_empty());
        assert!(!gap);
        assert_eq!(lost, 0);
    }

    #[test]
    fn read_at_write_cursor() {
        let mut rb = RingBuffer::new(4096);
        rb.write(b"hello");
        let (data, _, _) = rb.read(5);
        assert!(data.is_empty());
    }

    /// Incremental reads from an advancing cursor should only return new data,
    /// not re-read previously seen bytes — validates the prompt-check cursor
    /// optimisation pattern used in handle_send.
    #[test]
    fn incremental_cursor_reads_only_new_data() {
        let mut rb = RingBuffer::new(4096);
        rb.write(b"first");
        let cursor_after_first = rb.write_cursor(); // 5
        rb.write(b"second");
        let cursor_after_second = rb.write_cursor(); // 11

        // Read only from 'second' onwards
        let (data, gap, _) = rb.read(cursor_after_first);
        assert_eq!(data, b"second");
        assert!(!gap);

        // Nothing new beyond cursor_after_second
        let (data, _, _) = rb.read(cursor_after_second);
        assert!(data.is_empty());
    }

    /// After a fast-path fill followed by a slow-path write, the ring buffer
    /// must report the correct bytes via read().
    /// We use MIN_BUFFER_SIZE as the capacity because new() clamps to that minimum.
    #[test]
    fn fast_path_then_slow_path() {
        let cap = MIN_BUFFER_SIZE; // 4096
        let mut rb = RingBuffer::new(cap);

        // Fast path: exactly fill the buffer
        let fill = vec![b'a'; cap];
        rb.write(&fill);
        assert_eq!(rb.write_cursor(), cap as u64);

        // Slow path: write 4 more bytes — should overwrite the oldest 4
        rb.write(b"IJKL");
        assert_eq!(rb.write_cursor(), (cap + 4) as u64);

        // Reading from cursor 0 must show a gap of 4 bytes
        let (data, gap, lost) = rb.read(0);
        assert!(gap, "expected gap after overflow");
        assert_eq!(lost, 4, "expected 4 lost bytes");
        assert_eq!(data.len(), cap, "should get capacity bytes back");
        // Last 4 bytes of the result must be 'IJKL'
        assert_eq!(&data[cap - 4..], b"IJKL");
    }

    /// write() with a chunk larger than capacity must keep only the last
    /// `capacity` bytes, and read() from cursor 0 must show a gap.
    #[test]
    fn write_larger_than_capacity() {
        let cap = MIN_BUFFER_SIZE; // 4096
        let mut rb = RingBuffer::new(cap);
        // Write 2x capacity; the first `cap` bytes should be lost.
        let data: Vec<u8> = (0..cap * 2).map(|i| (i % 256) as u8).collect();
        rb.write(&data);
        let (out, gap, lost) = rb.read(0);
        assert!(gap, "expected gap");
        assert_eq!(lost, cap as u64, "expected cap bytes lost");
        assert_eq!(out.len(), cap, "should get cap bytes back");
        // Should contain the last `cap` bytes of the input
        assert_eq!(out, &data[cap..]);
    }
}
