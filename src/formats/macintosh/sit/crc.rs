// CRC-16 (poly 0x8005 reflected = 0xA001, init 0, no final XOR): every header
// and fork checksum. CRC-32 (zip/gzip): the method 15 stream checksum.

const fn crc16_table() -> [u16; 256] {
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

const fn crc32_table() -> [u32; 256] {
    let mut table = [0u32; 256];
    let mut i = 0;
    while i < 256 {
        let mut reg = i as u32;
        let mut bit = 0;
        while bit < 8 {
            reg = if reg & 1 != 0 {
                (reg >> 1) ^ 0xEDB8_8320
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

static CRC16_TABLE: [u16; 256] = crc16_table();
static CRC32_TABLE: [u32; 256] = crc32_table();

// Continues a CRC-16 from `crc` (0 to start); lets a header be summed in
// pieces with its checksum field replaced by zeros.
pub(super) fn crc16_update(crc: u16, data: &[u8]) -> u16 {
    data.iter().fold(crc, |reg, &byte| {
        (reg >> 8) ^ CRC16_TABLE[usize::from((reg as u8) ^ byte)]
    })
}

pub(super) fn crc16(data: &[u8]) -> u16 {
    crc16_update(0, data)
}

// Incremental CRC-32: start from `Crc32::new()`, `update` as output is
// produced, compare `finish()` with the stored value.
#[derive(Clone, Copy)]
pub(super) struct Crc32(u32);

impl Crc32 {
    pub(super) const fn new() -> Self {
        Self(0xFFFF_FFFF)
    }

    pub(super) fn update(&mut self, data: &[u8]) {
        self.0 = data.iter().fold(self.0, |reg, &byte| {
            (reg >> 8) ^ CRC32_TABLE[usize::from((reg as u8) ^ byte)]
        });
    }

    pub(super) const fn finish(self) -> u32 {
        self.0 ^ 0xFFFF_FFFF
    }
}

pub(super) fn crc32(data: &[u8]) -> u32 {
    let mut crc = Crc32::new();
    crc.update(data);
    crc.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc16_check_values() {
        assert_eq!(crc16(b""), 0x0000);
        assert_eq!(crc16(b"123456789"), 0xBB3D);
        assert_eq!(crc16(b"Hello World\n"), 0x48FE);
        assert_eq!(crc16(b"RSRC"), 0x389C);
        assert_eq!(crc16(b"Hi!\r"), 0x291E);
        assert_eq!(crc16(&[0u8; 112]), 0x0000);
    }

    #[test]
    fn crc16_update_matches_one_shot() {
        let data = b"123456789";
        let (head, tail) = data.split_at(4);
        assert_eq!(crc16_update(crc16(head), tail), crc16(data));
    }

    #[test]
    fn crc32_check_values() {
        assert_eq!(crc32(b""), 0x0000_0000);
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
    }

    #[test]
    fn crc32_incremental_matches_one_shot() {
        let mut crc = Crc32::new();
        crc.update(b"1234");
        crc.update(b"56789");
        assert_eq!(crc.finish(), 0xCBF4_3926);
    }
}
