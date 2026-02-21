use crate::compat::String;

const MAC_ROMAN_HIGH: [char; 128] = [
    // 0x80-0x8F
    'Ä', 'Å', 'Ç', 'É', 'Ñ', 'Ö', 'Ü', 'á', 'à', 'â', 'ä', 'ã', 'å', 'ç', 'é', 'è',
    // 0x90-0x9F
    'ê', 'ë', 'í', 'ì', 'î', 'ï', 'ñ', 'ó', 'ò', 'ô', 'ö', 'õ', 'ú', 'ù', 'û', 'ü',
    // 0xA0-0xAF
    '†', '°', '¢', '£', '§', '•', '¶', 'ß', '®', '©', '™', '´', '¨', '≠', 'Æ', 'Ø',
    // 0xB0-0xBF
    '∞', '±', '≤', '≥', '¥', 'µ', '∂', '∑', '∏', 'π', '∫', 'ª', 'º', 'Ω', 'æ', 'ø',
    // 0xC0-0xCF
    '¿', '¡', '¬', '√', 'ƒ', '≈', '∆', '«', '»', '…', '\u{A0}', 'À', 'Ã', 'Õ', 'Œ', 'œ',
    // 0xD0-0xDF
    '–', '—', '"', '"', '\u{2018}', '\u{2019}', '÷', '◊', 'ÿ', 'Ÿ', '⁄', '€', '‹', '›', 'ﬁ', 'ﬂ',
    // 0xE0-0xEF
    '‡', '·', '‚', '„', '‰', 'Â', 'Ê', 'Á', 'Ë', 'È', 'Í', 'Î', 'Ï', 'Ì', 'Ó', 'Ô',
    // 0xF0-0xFF
    '\u{F8FF}', 'Ò', 'Ú', 'Û', 'Ù', 'ı', 'ˆ', '˜', '¯', '˘', '˙', '˚', '¸', '˝', '˛', 'ˇ',
];

#[inline]
#[must_use]
pub fn mac_roman_to_char(byte: u8) -> char {
    if byte < 0x80 {
        byte as char
    } else {
        MAC_ROMAN_HIGH[(byte - 0x80) as usize]
    }
}

#[must_use]
pub fn decode_mac_roman(data: &[u8]) -> String {
    let mut result = String::with_capacity(data.len());
    for &b in data {
        result.push(mac_roman_to_char(b));
    }
    result
}

#[must_use]
pub fn decode_mac_roman_cstring(data: &[u8]) -> String {
    let mut result = String::with_capacity(data.len());
    for &b in data {
        if b == 0 {
            break;
        }
        result.push(mac_roman_to_char(b));
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ascii_passthrough() {
        let data = b"Hello, World!";
        assert_eq!(decode_mac_roman(data), "Hello, World!");
    }

    #[test]
    fn test_cstring_null_termination() {
        let data = b"Hello\x00World";
        assert_eq!(decode_mac_roman_cstring(data), "Hello");
    }

    #[test]
    fn test_accented_characters() {
        // 0x87 = á, 0x8E = é, 0x92 = í, 0x97 = ó, 0x9C = ú
        let data = [0x87, 0x8E, 0x92, 0x97, 0x9C];
        assert_eq!(decode_mac_roman(&data), "áéíóú");
    }

    #[test]
    fn test_german_umlauts() {
        // 0x80 = Ä, 0x85 = Ö, 0x86 = Ü, 0x8A = ä, 0x9A = ö, 0x9F = ü
        let data = [0x80, 0x85, 0x86, 0x8A, 0x9A, 0x9F];
        assert_eq!(decode_mac_roman(&data), "ÄÖÜäöü");
    }

    #[test]
    fn test_special_symbols() {
        // 0xA0 = †, 0xA5 = •, 0xAA = ™, 0xA9 = ©, 0xA8 = ®
        let data = [0xA0, 0xA5, 0xAA, 0xA9, 0xA8];
        assert_eq!(decode_mac_roman(&data), "†•™©®");
    }

    #[test]
    fn test_math_symbols() {
        // 0xB0 = ∞, 0xB1 = ±, 0xB9 = π
        let data = [0xB0, 0xB1, 0xB9];
        assert_eq!(decode_mac_roman(&data), "∞±π");
    }

    #[test]
    fn test_euro_sign() {
        // 0xDB = € (added in Mac OS 8.5)
        let data = [0xDB];
        assert_eq!(decode_mac_roman(&data), "€");
    }

    #[test]
    fn test_ligatures() {
        // 0xDE = ﬁ, 0xDF = ﬂ
        let data = [0xDE, 0xDF];
        assert_eq!(decode_mac_roman(&data), "ﬁﬂ");
    }

    #[test]
    fn test_empty_input() {
        assert_eq!(decode_mac_roman(&[]), "");
        assert_eq!(decode_mac_roman_cstring(&[]), "");
    }
}
