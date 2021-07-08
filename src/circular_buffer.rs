use bytes::{Buf, BufMut};

const MINIMUM_NON_EMPTY_CAPACITY: usize = 8;

/// A simple circular buffer structure whose memory usage is strictly capped.
pub struct CircularBuffer {
    buffer: Box<[u8]>,
    position: usize,
    length: usize,
    max_capacity: usize,
    initial_capacity: usize,
}

impl CircularBuffer {
    /// Create a new `CircularBuffer` with initial `capacity`, and a limit
    /// capacity set to `max_capacity`.
    pub fn new(capacity: usize, max_capacity: usize) -> Self {
        let capacity = std::cmp::min(capacity, max_capacity);
        let mut buffer = Vec::with_capacity(capacity);
        buffer.resize(capacity, 0);

        Self {
            buffer: buffer.into_boxed_slice(),
            position: 0,
            length: 0,
            max_capacity,
            initial_capacity: capacity,
        }
    }

    /// Checks whenever there is anything to read in the buffer.
    pub fn is_empty(&self) -> bool {
        self.length == 0
    }

    /// Total current capacity of the buffer.
    pub fn current_capacity(&self) -> usize {
        self.buffer.len()
    }

    /// The maximum amount of bytes that can be written to this buffer right now, *without* reallocating.
    fn remaining_mut_without_realloc(&self) -> usize {
        self.current_capacity() - self.length
    }

    fn resize_buffer(&mut self, new_capacity: usize) {
        if new_capacity == self.current_capacity() {
            return;
        }

        assert!(new_capacity >= self.length);

        let mut new_buffer = Vec::with_capacity(new_capacity);
        new_buffer.resize(new_capacity, 0);

        if self.length > 0 {
            if self.position + self.length <= self.current_capacity() {
                new_buffer[..self.length]
                    .copy_from_slice(&self.buffer[self.position..self.position + self.length]);
            } else {
                let a = self.position..self.current_capacity();
                let b = 0..self.length - a.len();
                new_buffer[..a.len()].copy_from_slice(&self.buffer[a.clone()]);
                new_buffer[a.len()..a.len() + b.len()].copy_from_slice(&self.buffer[b]);
            }
        }

        self.buffer = new_buffer.into_boxed_slice();
        self.position = 0;
    }

    #[inline(never)]
    #[cold]
    fn grow_buffer(&mut self) -> bool {
        if self.remaining_mut() == 0 {
            return false;
        }

        let new_capacity = std::cmp::min(
            self.max_capacity,
            std::cmp::max(
                self.current_capacity() * 2,
                std::cmp::max(MINIMUM_NON_EMPTY_CAPACITY, self.initial_capacity)
            ),
        );

        self.resize_buffer(new_capacity);
        true
    }

    fn advance_mut_impl(&mut self, count: usize) {
        assert!(
            count <= self.remaining_mut(),
            "tried to advance the write cursor past maximum buffer capacity"
        );
        assert!(
            count <= self.remaining_mut_without_realloc(),
            "tried to advance the write cursor past what was written"
        );
        self.length += count;
    }

    fn bytes_mut_impl(&mut self) -> &mut [u8] {
        if self.remaining_mut_without_realloc() == 0 {
            if !self.grow_buffer() {
                return &mut self.buffer[..0];
            }
        }

        let position_mut = (self.position + self.length) % self.current_capacity();
        let range = position_mut
            ..std::cmp::min(
                position_mut + self.remaining_mut_without_realloc(),
                self.current_capacity(),
            );
        &mut self.buffer[range]
    }

    pub fn read_cursor(&self) -> (usize, usize) {
        (self.position, self.length)
    }

    pub fn set_read_cursor(&mut self, (position, length): (usize, usize)) {
        self.position = position;
        self.length = length;
    }

    pub fn read_exact_into_vec(&mut self, length: usize) -> Vec<u8> {
        assert!(length <= self.remaining());
        let mut output = Vec::with_capacity(length);
        let first_chunk_size = std::cmp::min(length, self.bytes().len());
        output.extend_from_slice(&self.bytes()[..first_chunk_size]);
        self.advance(first_chunk_size);

        let remaining = length - first_chunk_size;
        output.extend_from_slice(&self.bytes()[..remaining]);
        self.advance(remaining);

        output
    }

    pub fn apply_soft_limit(&mut self, limit: usize) {
        let limit = std::cmp::min(limit, self.max_capacity);
        if self.remaining() > limit || self.current_capacity() <= limit {
            return;
        }

        if self.remaining() == 0 {
            self.buffer = Vec::new().into_boxed_slice();
            self.position = 0;
            return;
        }

        self.resize_buffer(limit);
    }
}

impl Buf for CircularBuffer {
    /// The amount of bytes that can be read from this buffer.
    fn remaining(&self) -> usize {
        self.length
    }

    fn bytes(&self) -> &[u8] {
        &self.buffer[self.position..std::cmp::min(self.position + self.length, self.current_capacity())]
    }

    fn advance(&mut self, count: usize) {
        assert!(
            count <= self.remaining(),
            "tried to advance the read cursor past what is available to read"
        );
        self.position = (self.position + count) % self.current_capacity();
        self.length -= count;
        if self.length == 0 {
            self.position = 0;
        }
    }
}

impl BufMut for CircularBuffer {
    /// The maximum amount of bytes that can still be written to this buffer.
    ///
    /// This does *not* signify how much data can be written to it *right now* without any reallocation.
    fn remaining_mut(&self) -> usize {
        self.max_capacity - self.length
    }

    unsafe fn advance_mut(&mut self, count: usize) {
        self.advance_mut_impl(count)
    }

    unsafe fn bytes_mut(&mut self) -> &mut [u8] {
        self.bytes_mut_impl()
    }
}

impl std::io::Read for CircularBuffer {
    fn read(&mut self, output: &mut [u8]) -> std::io::Result<usize> {
        let bytes = Buf::bytes(self);
        let length = std::cmp::min(bytes.len(), output.len());
        output[..length].copy_from_slice(&bytes[..length]);
        self.advance(length);
        Ok(length)
    }
}

impl std::io::Write for CircularBuffer {
    fn write(&mut self, input: &[u8]) -> std::io::Result<usize> {
        let buffer = self.bytes_mut_impl();
        let length = std::cmp::min(buffer.len(), input.len());
        buffer[..length].copy_from_slice(&input[..length]);
        self.advance_mut_impl(length);
        Ok(length)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use std::io::Write;

    #[test]
    fn basic() {
        let mut b = CircularBuffer::new(0, 16);
        b.write_all(b"01234567").unwrap();
        assert_eq!(b.remaining(), 8);
        assert_eq!(b.remaining_mut(), 8);
        assert_eq!(b.remaining_mut_without_realloc(), 0);
        assert_eq!(b.bytes(), b"01234567");
        assert_eq!(b.current_capacity(), 8);

        b.advance(1);
        assert_eq!(b.remaining(), 7);
        assert_eq!(b.remaining_mut(), 9);
        assert_eq!(b.remaining_mut_without_realloc(), 1);
        assert_eq!(b.bytes(), b"1234567");
        assert_eq!(b.current_capacity(), 8);

        b.write_all(b"8").unwrap();
        assert_eq!(b.remaining(), 8);
        assert_eq!(b.remaining_mut(), 8);
        assert_eq!(b.remaining_mut_without_realloc(), 0);
        assert_eq!(b.bytes(), b"1234567");
        assert_eq!(b.current_capacity(), 8);

        b.advance(7);
        assert_eq!(b.remaining(), 1);
        assert_eq!(b.remaining_mut(), 15);
        assert_eq!(b.remaining_mut_without_realloc(), 7);
        assert_eq!(b.bytes(), b"8");
        assert_eq!(b.current_capacity(), 8);
    }

    #[test]
    fn grow_buffer_without_wraparound() {
        let mut b = CircularBuffer::new(0, 16);
        b.write_all(b"01234567").unwrap();
        assert_eq!(b.remaining(), 8);
        assert_eq!(b.remaining_mut(), 8);
        assert_eq!(b.remaining_mut_without_realloc(), 0);
        assert_eq!(b.current_capacity(), 8);

        b.write_all(b"89ABCDEF").unwrap();
        assert_eq!(b.remaining(), 16);
        assert_eq!(b.remaining_mut(), 0);
        assert_eq!(b.remaining_mut_without_realloc(), 0);
        assert_eq!(b.bytes(), b"0123456789ABCDEF");
        assert_eq!(b.current_capacity(), 16);
    }

    #[test]
    fn grow_buffer_with_wraparound() {
        let mut b = CircularBuffer::new(0, 16);
        b.write_all(b"01234567").unwrap();
        b.advance(4);
        b.write_all(b"89ABCDEF").unwrap();
        assert_eq!(b.remaining(), 12);
        assert_eq!(b.remaining_mut(), 4);
        assert_eq!(b.remaining_mut_without_realloc(), 4);
        assert_eq!(b.bytes(), b"456789ABCDEF");
        assert_eq!(b.current_capacity(), 16);
    }

    #[test]
    fn fill_whole_buffer() {
        let mut b = CircularBuffer::new(0, 8);
        b.write_all(b"01234567").unwrap();
        assert_eq!(b.write(b"8").unwrap(), 0);
        assert_eq!(
            b.write_all(b"8").unwrap_err().kind(),
            std::io::ErrorKind::WriteZero
        );
    }

    #[test]
    fn read_from_buffer() {
        let mut b = CircularBuffer::new(0, 8);
        b.write_all(b"01234567").unwrap();

        {
            let mut tmp = [0; 4];
            assert_eq!(std::io::Read::read(&mut b, &mut tmp).unwrap(), 4);
            assert_eq!(&tmp, b"0123");
        }

        {
            let mut tmp = [0; 2];
            assert_eq!(std::io::Read::read(&mut b, &mut tmp).unwrap(), 2);
            assert_eq!(&tmp, b"45");
        }

        {
            let mut tmp = [0; 4];
            assert_eq!(std::io::Read::read(&mut b, &mut tmp).unwrap(), 2);
            assert_eq!(&tmp, b"67\0\0");
        }

        {
            let mut tmp = [0; 4];
            assert_eq!(std::io::Read::read(&mut b, &mut tmp).unwrap(), 0);
            assert_eq!(&tmp, b"\0\0\0\0");
        }
    }

    #[test]
    fn read_exact_into_vec() {
        let mut b = CircularBuffer::new(0, 16);
        b.write_all(b"01234567").unwrap();
        b.advance(4);
        b.write_all(b"89ABCDEF").unwrap();
        assert_eq!(b.read_exact_into_vec(12), b"456789ABCDEF");
    }

    #[test]
    fn apply_soft_limit() {
        let mut b = CircularBuffer::new(0, 16);
        b.write_all(b"0123456789ABCDEF").unwrap();
        assert_eq!(b.current_capacity(), 16);

        b.apply_soft_limit(16);
        assert_eq!(b.current_capacity(), 16);
        assert_eq!(b.bytes(), b"0123456789ABCDEF");

        b.advance(1);
        b.apply_soft_limit(16);
        assert_eq!(b.current_capacity(), 16);
        assert_eq!(b.bytes(), b"123456789ABCDEF");

        b.apply_soft_limit(15);
        assert_eq!(b.current_capacity(), 15);
        assert_eq!(b.bytes(), b"123456789ABCDEF");

        b.apply_soft_limit(8);
        assert_eq!(b.current_capacity(), 15);
        assert_eq!(b.bytes(), b"123456789ABCDEF");

        b.advance(15);
        b.apply_soft_limit(15);
        assert_eq!(b.current_capacity(), 15);
        assert_eq!(b.bytes(), b"");

        b.apply_soft_limit(14);
        assert_eq!(b.current_capacity(), 0);
        assert_eq!(b.bytes(), b"");
    }
}
