// Reflected form of the CRC-32 generator polynomial
// x^32+x^26+x^23+x^22+x^16+x^12+x^11+x^10+x^8+x^7+x^5+x^4+x^2+x+1.
const CRC_POLYNOMIAL: u32 = 0xEDB8_8320;

// CRC_TABLES[k][b] is the register after byte `b` followed by `k` zero bytes,
// so eight input bytes can be folded in per step (the CRC is linear over GF(2)).
const CRC_TABLES: [[u32; 256]; 8] = crc_tables();

const fn crc_tables() -> [[u32; 256]; 8] {
    let mut tables = [[0u32; 256]; 8];
    let mut byte = 0;
    while byte < 256 {
        let mut register = byte as u32;
        let mut bit = 0;
        while bit < 8 {
            register = if register & 1 == 1 {
                (register >> 1) ^ CRC_POLYNOMIAL
            } else {
                register >> 1
            };
            bit += 1;
        }
        tables[0][byte] = register;
        byte += 1;
    }
    let mut k = 1;
    while k < 8 {
        let mut byte = 0;
        while byte < 256 {
            let previous = tables[k - 1][byte];
            tables[k][byte] = (previous >> 8) ^ tables[0][(previous & 0xFF) as usize];
            byte += 1;
        }
        k += 1;
    }
    tables
}

pub(crate) fn crc32(data: &[u8]) -> u32 {
    !crc32_update(u32::MAX, data)
}

// Feeds `data` to a running CRC-32. The register starts as `u32::MAX` and
// its complement is the checksum.
pub(crate) fn crc32_update(mut register: u32, data: &[u8]) -> u32 {
    let mut chunks = data.chunks_exact(8);
    for chunk in &mut chunks {
        let [a, b, c, d, e, f, g, h] = *chunk else {
            continue;
        };
        let [a, b, c, d] = (register ^ u32::from_le_bytes([a, b, c, d])).to_le_bytes();
        register = CRC_TABLES[7][a as usize]
            ^ CRC_TABLES[6][b as usize]
            ^ CRC_TABLES[5][c as usize]
            ^ CRC_TABLES[4][d as usize]
            ^ CRC_TABLES[3][e as usize]
            ^ CRC_TABLES[2][f as usize]
            ^ CRC_TABLES[1][g as usize]
            ^ CRC_TABLES[0][h as usize];
    }
    for &byte in chunks.remainder() {
        register = (register >> 8) ^ CRC_TABLES[0][((register ^ u32::from(byte)) & 0xFF) as usize];
    }
    register
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc32_matches_check_values() {
        const TEXT: &[u8] = b"123456789";
        assert_eq!(crc32(b""), 0);
        assert_eq!(crc32(TEXT), 0xCBF4_3926);
        // A message followed by its CRC leaves the register at the fixed
        // residue 0xdebb20e3.
        let mut message = TEXT.to_vec();
        message.extend_from_slice(&crc32(TEXT).to_le_bytes());
        assert_eq!(!crc32(&message), 0xDEBB_20E3);
    }

    #[test]
    fn crc32_is_independent_of_chunking() {
        let data: Vec<u8> = (0..1000u32).map(|i| (i * 7 + 3) as u8).collect();
        for len in 0..40 {
            let mut register = u32::MAX;
            for &byte in &data[..len] {
                register ^= u32::from(byte);
                for _ in 0..8 {
                    register = if register & 1 == 1 {
                        (register >> 1) ^ CRC_POLYNOMIAL
                    } else {
                        register >> 1
                    };
                }
            }
            assert_eq!(crc32(&data[..len]), !register, "length {len}");
        }
    }
}
