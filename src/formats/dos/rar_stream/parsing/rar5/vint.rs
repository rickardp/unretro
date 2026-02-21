//! Variable-length integer (vint) parsing for RAR5.
//!
//! RAR5 uses variable-length integers where each byte contributes 7 bits
//! of data, and the high bit indicates if more bytes follow.
//!
//! Format:
//! - Bits 0-6: Data bits
//! - Bit 7: Continuation flag (1 = more bytes follow)

/// Read a variable-length integer from a byte slice.
/// Returns the value and the number of bytes consumed.
#[inline]
pub fn read_vint(data: &[u8]) -> Option<(u64, usize)> {
    let mut result = 0u64;
    let mut shift = 0;

    for (i, &byte) in data.iter().enumerate() {
        // Limit to 9 bytes (63 bits of data) to prevent overflow
        if i >= 9 {
            return None;
        }

        result |= u64::from(byte & 0x7F) << shift;

        if byte & 0x80 == 0 {
            return Some((result, i + 1));
        }

        shift += 7;
    }

    // Ran out of bytes without finding end
    None
}

/// Helper for reading multiple vints from a buffer.
pub struct VintReader<'a> {
    data: &'a [u8],
    offset: usize,
}

impl<'a> VintReader<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Self { data, offset: 0 }
    }

    /// Read the next vint from the buffer.
    #[inline]
    pub fn read(&mut self) -> Option<u64> {
        let (value, consumed) = read_vint(&self.data[self.offset..])?;
        self.offset += consumed;
        Some(value)
    }

    /// Read a fixed number of bytes.
    #[inline]
    pub fn read_bytes(&mut self, count: usize) -> Option<&'a [u8]> {
        let end = self.offset.checked_add(count)?;
        if end > self.data.len() {
            return None;
        }
        let slice = &self.data[self.offset..end];
        self.offset = end;
        Some(slice)
    }

    /// Read a u32 in little-endian format.
    #[inline]
    pub fn read_u32_le(&mut self) -> Option<u32> {
        let bytes = self.read_bytes(4)?;
        Some(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    /// Read a u64 in little-endian format.
    #[inline]
    pub fn read_u64_le(&mut self) -> Option<u64> {
        let bytes = self.read_bytes(8)?;
        Some(u64::from_le_bytes([
            bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
        ]))
    }

    /// Current position in the buffer.
    pub fn position(&self) -> usize {
        self.offset
    }

    /// Remaining bytes in the buffer.
    pub fn remaining(&self) -> &'a [u8] {
        &self.data[self.offset..]
    }

    /// Skip ahead by a number of bytes.
    pub fn skip(&mut self, count: usize) -> bool {
        let end = match self.offset.checked_add(count) {
            Some(end) => end,
            None => return false,
        };
        if end > self.data.len() {
            return false;
        }
        self.offset = end;
        true
    }
}
