// CRC-16 of container.md 3.2 (polynomial `8005` reflected, initial value 0,
// no final XOR; "123456789" gives `BB3D`). Used for entry data and for the
// header CRC.

use crate::error::Result;

const fn table() -> [u16; 256] {
    let mut table = [0u16; 256];
    let mut i = 0;
    while i < 256 {
        let mut reg = i as u16;
        let mut bit = 0;
        while bit < 8 {
            reg = if reg & 1 != 0 {
                (reg >> 1) ^ 0xA001
            } else {
                reg >> 1
            };
            bit += 1;
        }
        table[i] = reg;
        i += 1;
    }
    table
}

static TABLE: [u16; 256] = table();

// Continues from `crc` (0 to start), so a header can be summed in pieces
// with its own CRC field taken as zeros.
pub(super) fn update(crc: u16, data: &[u8]) -> u16 {
    data.iter().fold(crc, |reg, &byte| {
        (reg >> 8) ^ TABLE[usize::from((reg as u8) ^ byte)]
    })
}

// Infallible; the `Result` is the signature the callers were written against.
pub(super) fn crc16(data: &[u8]) -> Result<u16> {
    Ok(update(0, data))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn check_values() {
        // container.md 3.2.
        assert_eq!(crc16(b"").unwrap(), 0x0000);
        assert_eq!(crc16(b"123456789").unwrap(), 0xBB3D);
        assert_eq!(crc16(b"a").unwrap(), 0xE8C1);
        assert_eq!(crc16(b"Hello, LHA!\n").unwrap(), 0x4953);
    }

    #[test]
    fn matches_the_bitwise_description() {
        let data: Vec<u8> = (0..=255u8).chain((0..=255).rev()).collect();
        let mut reg = 0u16;
        for &byte in &data {
            reg ^= u16::from(byte);
            for _ in 0..8 {
                reg = if reg & 1 != 0 {
                    (reg >> 1) ^ 0xA001
                } else {
                    reg >> 1
                };
            }
        }
        assert_eq!(crc16(&data).unwrap(), reg);
    }

    #[test]
    fn update_continues() {
        let (head, tail) = b"123456789".split_at(4);
        assert_eq!(update(update(0, head), tail), 0xBB3D);
    }
}
