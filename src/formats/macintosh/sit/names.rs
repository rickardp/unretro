use crate::compat::{String, ToString};

use super::super::encoding::mac_roman_to_char;

// Classic names are MacRoman.
pub(super) fn decode_classic(bytes: &[u8]) -> String {
    bytes.iter().map(|&byte| mac_roman(byte)).collect()
}

// StuffIt 5 names and comments: ASCII, else well-formed UTF-8, else MacRoman.
// `from_utf8` applies the Unicode definition of well-formed and covers ASCII.
pub(super) fn decode_sit5(bytes: &[u8]) -> String {
    match core::str::from_utf8(bytes) {
        Ok(text) => text.to_string(),
        Err(_) => decode_classic(bytes),
    }
}

// The shared table maps 0xD2/0xD3 to ASCII quotes; the specification's table
// has the curly quotes there.
fn mac_roman(byte: u8) -> char {
    match byte {
        0xD2 => '\u{201C}',
        0xD3 => '\u{201D}',
        _ => mac_roman_to_char(byte),
    }
}

// Makes one decoded name usable as a component of a `/`-joined path. Stored
// names are single components and must never be split on separators.
pub(crate) fn safe_component(name: &str) -> String {
    let replaced: String = name
        .chars()
        .map(|c| match c {
            '/' => ':',
            '\0' => '_',
            other => other,
        })
        .collect();
    match replaced.as_str() {
        "" | "." => "_".to_string(),
        ".." => "__".to_string(),
        _ => replaced,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // containers.md 2.6, bytes 0x80..=0xFF.
    const SPEC_HIGH: [u16; 128] = [
        0x00C4, 0x00C5, 0x00C7, 0x00C9, 0x00D1, 0x00D6, 0x00DC, 0x00E1, 0x00E0, 0x00E2, 0x00E4,
        0x00E3, 0x00E5, 0x00E7, 0x00E9, 0x00E8, 0x00EA, 0x00EB, 0x00ED, 0x00EC, 0x00EE, 0x00EF,
        0x00F1, 0x00F3, 0x00F2, 0x00F4, 0x00F6, 0x00F5, 0x00FA, 0x00F9, 0x00FB, 0x00FC, 0x2020,
        0x00B0, 0x00A2, 0x00A3, 0x00A7, 0x2022, 0x00B6, 0x00DF, 0x00AE, 0x00A9, 0x2122, 0x00B4,
        0x00A8, 0x2260, 0x00C6, 0x00D8, 0x221E, 0x00B1, 0x2264, 0x2265, 0x00A5, 0x00B5, 0x2202,
        0x2211, 0x220F, 0x03C0, 0x222B, 0x00AA, 0x00BA, 0x03A9, 0x00E6, 0x00F8, 0x00BF, 0x00A1,
        0x00AC, 0x221A, 0x0192, 0x2248, 0x2206, 0x00AB, 0x00BB, 0x2026, 0x00A0, 0x00C0, 0x00C3,
        0x00D5, 0x0152, 0x0153, 0x2013, 0x2014, 0x201C, 0x201D, 0x2018, 0x2019, 0x00F7, 0x25CA,
        0x00FF, 0x0178, 0x2044, 0x20AC, 0x2039, 0x203A, 0xFB01, 0xFB02, 0x2021, 0x00B7, 0x201A,
        0x201E, 0x2030, 0x00C2, 0x00CA, 0x00C1, 0x00CB, 0x00C8, 0x00CD, 0x00CE, 0x00CF, 0x00CC,
        0x00D3, 0x00D4, 0xF8FF, 0x00D2, 0x00DA, 0x00DB, 0x00D9, 0x0131, 0x02C6, 0x02DC, 0x00AF,
        0x02D8, 0x02D9, 0x02DA, 0x00B8, 0x02DD, 0x02DB, 0x02C7,
    ];

    #[test]
    fn mac_roman_matches_specification_table() {
        for byte in 0u8..0x80 {
            assert_eq!(mac_roman(byte), char::from(byte));
        }
        for (index, &code) in SPEC_HIGH.iter().enumerate() {
            let byte = 0x80 + index as u8;
            assert_eq!(
                u32::from(mac_roman(byte)),
                u32::from(code),
                "byte {byte:#04x}"
            );
        }
    }

    #[test]
    fn classic_names_are_mac_roman() {
        assert_eq!(decode_classic(b"Icon\r"), "Icon\r");
        assert_eq!(decode_classic(b"Order Form US/CAN"), "Order Form US/CAN");
        assert_eq!(decode_classic(&[0x8E, 0x74, 0x8E]), "été");
        // Valid UTF-8 is still MacRoman in a classic archive.
        assert_eq!(decode_classic(&[0xC2, 0xB5]), "¬µ");
        assert_eq!(decode_classic(b""), "");
    }

    #[test]
    fn sit5_names_follow_the_three_step_rule() {
        assert_eq!(decode_sit5(b"plain"), "plain");
        // Micro sign and ellipsis, as in the real corpus.
        assert_eq!(decode_sit5(&[0xC2, 0xB5]), "µ");
        assert_eq!(decode_sit5(&[0xE2, 0x80, 0xA6]), "…");
        // Not well-formed UTF-8: MacRoman.
        assert_eq!(decode_sit5(&[0x8E, 0x74, 0x8E]), "été");
        // Overlong, surrogate, above U+10FFFF and truncated forms fall back.
        assert_eq!(decode_sit5(&[0xC0, 0xAF]), "¿Ø");
        assert_eq!(decode_sit5(&[0xED, 0xA0, 0x80]), "Ì†Ä");
        assert_eq!(decode_sit5(&[0xF4, 0x90, 0x80, 0x80]), "ÙêÄÄ");
        assert_eq!(decode_sit5(&[0xE2, 0x80]), "‚Ä");
    }

    #[test]
    fn safe_component_steps() {
        assert_eq!(safe_component("Order Form US/CAN"), "Order Form US:CAN");
        assert_eq!(safe_component("a\0b"), "a_b");
        assert_eq!(safe_component(""), "_");
        assert_eq!(safe_component("."), "_");
        assert_eq!(safe_component(".."), "__");
        assert_eq!(safe_component("..."), "...");
        assert_eq!(safe_component("Icon\r"), "Icon\r");
        assert_eq!(safe_component("a\\b:c"), "a\\b:c");
        // The steps apply in order: `/` alone is not empty afterwards.
        assert_eq!(safe_component("/"), ":");
    }
}
