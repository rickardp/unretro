use crate::compat::{String, Vec, format};
use crate::error::{Error, Result};
use crate::format::ContainerFormat;
use crate::limits::{checked_output_size, prealloc_hint};
use crate::sanitize_path_component;
use crate::{Container, ContainerInfo, Entry, Metadata};

use super::encoding::decode_mac_roman;
use super::resource_fork::ResourceFork;

pub const BINHEX_HEADER: &[u8] = b"(This file must be converted with BinHex";

#[must_use]
pub fn is_binhex_file(data: &[u8]) -> bool {
    find_binhex_start(data).is_some()
}

// BinHex files were commonly distributed via email/Usenet and may have
// lengthy headers before the marker, so search the first 2KB.
fn find_binhex_start(data: &[u8]) -> Option<usize> {
    let search_limit = data.len().min(2048);
    data[..search_limit]
        .windows(BINHEX_HEADER.len())
        .position(|window| window == BINHEX_HEADER)
}

// The RFC prints a space inside the table; without it there are exactly 64
// characters, so the space is taken as a typesetting artifact.
const ALPHABET: &[u8; 64] = b"!\"#$%&'()*+,-012345689@ABCDEFGHIJKLMNPQRSTUVXYZ[`abcdefhijklmpqr";
const INVALID_SIXTET: u8 = 0xFF;
const SIXTETS: [u8; 256] = sixtet_table();
const CRC_TABLE: [u16; 256] = crc_table();
const RLE_MARKER: u8 = 0x90;

const fn sixtet_table() -> [u8; 256] {
    let mut table = [INVALID_SIXTET; 256];
    let mut i = 0;
    while i < ALPHABET.len() {
        table[ALPHABET[i] as usize] = i as u8;
        i += 1;
    }
    table
}

// Remainder of each byte value times x^16, divided by the CCITT polynomial
// x^16 + x^12 + x^5 + 1.
const fn crc_table() -> [u16; 256] {
    let mut table = [0u16; 256];
    let mut i = 0;
    while i < table.len() {
        let mut rem = (i as u16) << 8;
        let mut bit = 0;
        while bit < 8 {
            rem = if rem & 0x8000 != 0 {
                (rem << 1) ^ 0x1021
            } else {
                rem << 1
            };
            bit += 1;
        }
        table[i] = rem;
        i += 1;
    }
    table
}

fn invalid(what: &str) -> Error {
    Error::invalid_format(format!("Invalid BinHex file: {what}"))
}

// Pulls decoded bytes one at a time, so a run is only ever expanded as far
// as the caller asks for.
struct Decoder<'a> {
    input: &'a [u8],
    pos: usize,
    acc: u32,
    acc_bits: u32,
    ended: bool,
    last: Option<u8>,
    pending_run: u8,
    crc: u16,
}

impl<'a> Decoder<'a> {
    fn new(input: &'a [u8]) -> Self {
        Self {
            input,
            pos: 0,
            acc: 0,
            acc_bits: 0,
            ended: false,
            last: None,
            pending_run: 0,
            crc: 0,
        }
    }

    // Next byte of the 6-bit layer; `None` at the closing ':' or end of input.
    fn next_packed(&mut self) -> Result<Option<u8>> {
        while self.acc_bits < 8 {
            if self.ended {
                return Ok(None);
            }
            let Some(&c) = self.input.get(self.pos) else {
                self.ended = true;
                return Ok(None);
            };
            self.pos += 1;
            match c {
                b'\r' | b'\n' | b' ' | b'\t' => {}
                b':' => {
                    self.ended = true;
                    return Ok(None);
                }
                _ => {
                    let sixtet = SIXTETS[usize::from(c)];
                    if sixtet == INVALID_SIXTET {
                        return Err(invalid(&format!(
                            "byte 0x{c:02X} is outside the encoding alphabet"
                        )));
                    }
                    self.acc = ((self.acc << 6) | u32::from(sixtet)) & 0xFFFF;
                    self.acc_bits += 6;
                }
            }
        }
        self.acc_bits -= 8;
        Ok(Some((self.acc >> self.acc_bits) as u8))
    }

    // Next byte after run-length expansion.
    fn next_byte(&mut self) -> Result<Option<u8>> {
        loop {
            if self.pending_run > 0 {
                self.pending_run -= 1;
                return Ok(self.last);
            }
            let Some(byte) = self.next_packed()? else {
                return Ok(None);
            };
            if byte != RLE_MARKER {
                self.last = Some(byte);
                return Ok(self.last);
            }
            let Some(count) = self.next_packed()? else {
                return Ok(None);
            };
            if count == 0 {
                self.last = Some(RLE_MARKER);
                return Ok(self.last);
            }
            // The count is the total run length including the byte already
            // emitted, so a count of 1 adds nothing. The RFC gives a marker
            // nothing to repeat at the very start of the stream: rejected.
            if self.last.is_none() {
                return Err(invalid("run-length marker with no preceding byte"));
            }
            self.pending_run = count - 1;
        }
    }

    fn read(&mut self, what: &str) -> Result<u8> {
        let byte = self
            .next_byte()?
            .ok_or_else(|| invalid(&format!("truncated {what}")))?;
        let index = usize::from((self.crc >> 8) as u8 ^ byte);
        self.crc = (self.crc << 8) ^ CRC_TABLE[index];
        Ok(byte)
    }

    fn read_array<const N: usize>(&mut self, what: &str) -> Result<[u8; N]> {
        let mut out = [0u8; N];
        for slot in &mut out {
            *slot = self.read(what)?;
        }
        Ok(out)
    }

    // Consumes the stored CRC of the section read since the previous call.
    fn verify_crc(&mut self, what: &str) -> Result<()> {
        let computed = self.crc;
        let stored = u16::from_be_bytes(self.read_array(what)?);
        self.crc = 0;
        if stored == computed {
            Ok(())
        } else {
            Err(invalid(&format!("{what} CRC mismatch")))
        }
    }

    fn read_fork(&mut self, len: u32, what: &str) -> Result<Vec<u8>> {
        let mut fork = Vec::with_capacity(prealloc_hint(u64::from(len)));
        for _ in 0..len {
            fork.push(self.read(what)?);
        }
        self.verify_crc(what)?;
        Ok(fork)
    }
}

pub struct BinHexContainer {
    prefix: String,
    filename: String,
    data_fork: Vec<u8>,
    resource_fork: Vec<u8>,
    file_type: [u8; 4],
    creator: [u8; 4],
}

impl BinHexContainer {
    pub fn from_bytes(data: &[u8], prefix: String, _depth: u32) -> Result<Self> {
        let payload = find_binhex_start(data)
            .and_then(|start| data.get(start.checked_add(BINHEX_HEADER.len())?..))
            .and_then(|rest| {
                let colon = rest.iter().position(|&b| b == b':')?;
                rest.get(colon.checked_add(1)?..)
            })
            .ok_or_else(|| invalid("no encoded data after the marker line"))?;
        let mut decoder = Decoder::new(payload);

        let name_len = decoder.read("header")?;
        if name_len == 0 {
            return Err(invalid("empty filename"));
        }
        let mut name = Vec::with_capacity(usize::from(name_len));
        for _ in 0..name_len {
            name.push(decoder.read("header")?);
        }
        let _version = decoder.read("header")?;
        let file_type = decoder.read_array::<4>("header")?;
        let creator = decoder.read_array::<4>("header")?;
        let _flags = decoder.read_array::<2>("header")?;
        let data_len = u32::from_be_bytes(decoder.read_array("header")?);
        let resource_len = u32::from_be_bytes(decoder.read_array("header")?);
        decoder.verify_crc("header")?;

        checked_output_size(
            u64::from(data_len) + u64::from(resource_len),
            "Invalid BinHex file",
        )?;

        let data_fork = decoder.read_fork(data_len, "data fork")?;
        let resource_fork = decoder.read_fork(resource_len, "resource fork")?;

        Ok(Self {
            prefix,
            filename: decode_mac_roman(&name),
            data_fork,
            resource_fork,
            file_type,
            creator,
        })
    }

    fn data_path(&self) -> String {
        format!(
            "{}/{}",
            self.prefix,
            sanitize_path_component(&self.filename)
        )
    }
}

impl Container for BinHexContainer {
    fn prefix(&self) -> &str {
        &self.prefix
    }

    fn visit(&self, visitor: &mut dyn FnMut(&Entry<'_>) -> Result<bool>) -> Result<()> {
        let full_path = self.data_path();
        let metadata = Metadata::new().with_type_creator(self.file_type, self.creator);

        let entry = Entry::new(&full_path, &self.prefix, &self.data_fork).with_metadata(&metadata);
        visitor(&entry)?;

        // Yield resource fork as a single entry (it's a container itself)
        if !self.resource_fork.is_empty() && ResourceFork::is_valid(&self.resource_fork) {
            let rsrc_path = format!("{full_path}/..namedfork/rsrc");
            let entry = Entry::new(&rsrc_path, &self.prefix, &self.resource_fork);
            visitor(&entry)?;
        }

        Ok(())
    }

    fn get_file(&self, path: &str) -> Option<&[u8]> {
        let data_path = self.data_path();
        let rsrc_path = format!("{data_path}/..namedfork/rsrc");

        if path == data_path {
            Some(&self.data_fork)
        } else if path == rsrc_path {
            Some(&self.resource_fork)
        } else {
            None
        }
    }

    fn info(&self) -> ContainerInfo {
        ContainerInfo {
            path: self.prefix.clone(),
            format: ContainerFormat::BinHex,
            entry_count: Some(if self.resource_fork.is_empty() { 1 } else { 2 }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Written by the encoder of Python 3.9's `binhex` module.
    const REFERENCE: &[u8] = b"\
        (This file must be converted with BinHex 4.0)\r\
        \r\
        :#&+1Fh9YML$%!&4&@&4dG(Kd!3!!!!*4!!!!%la[5'9XE'm!N!U3!%&#N!#3!*!\r\
        !N!#3!%13rd13,AKjN!#3!(T%4%9&43!\"!J-%\"3B(#!N+#``0$Jm3%4)6&\"8@&aJ\r\
        C'KXF(4iI)#%L)b3P*LFS+5SV,#dZ,c!a-M-d06Bh1$Nk1c`p2Mp!38*$4%9'4dK\r\
        *5NY-68j28&&58e499PGB@9TEA&eHAf\"KBQ0NC@CRD'PUDfaYEQp`FA*cG(9fGhK\r\
        jHRYmIAjrJ)'#Ji5&KSH)LBU,M)f1Mj!!NC+6P*@@PjLCQTZFRCkIS+'LSk5PTUH\r\
        SUDUVV+fZVl#aXV1dYEDhZ,QkZlbp[Vr!`F,$a-A'amM*bX[-cFl2d0(5dp69eYI\r\
        BfGVEh0hHhq$KiZ2NjHERk1RUkqcYlZr`mI,cp2AfprMjq[[mrIlrN!AaN!#3!(*\r\
        cFQ-4N!H3!!#3!(4KD@c8'!:\r";

    const MARKER_LINE: &[u8] = b"(This file must be converted with BinHex 4.0)\n";

    fn reference_data_fork() -> Vec<u8> {
        let mut data = b"Hello".to_vec();
        data.extend_from_slice(&[0; 10]);
        data.extend_from_slice(b"\x90AB");
        data.extend_from_slice(&[0x90; 5]);
        data.extend_from_slice(&[b'C'; 300]);
        data.extend_from_slice(b"xy\x90\x90zDDEEE");
        data.extend(0..=255u8);
        data.extend_from_slice(&[0xFF; 4]);
        data
    }

    fn decode(data: &[u8]) -> Result<BinHexContainer> {
        BinHexContainer::from_bytes(data, String::from("p"), 0)
    }

    fn error_of(data: &[u8]) -> String {
        match decode(data) {
            Ok(_) => panic!("decoding unexpectedly succeeded"),
            Err(err) => err.to_string(),
        }
    }

    fn crc16(bytes: &[u8]) -> u16 {
        let mut crc = 0u16;
        for &byte in bytes {
            crc ^= u16::from(byte) << 8;
            for _ in 0..8 {
                crc = if crc & 0x8000 != 0 {
                    (crc << 1) ^ 0x1021
                } else {
                    crc << 1
                };
            }
        }
        crc
    }

    fn escape(bytes: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        for &byte in bytes {
            out.push(byte);
            if byte == RLE_MARKER {
                out.push(0);
            }
        }
        out
    }

    fn header(name: &[u8], data_len: u32, resource_len: u32) -> Vec<u8> {
        let mut header = vec![name.len() as u8];
        header.extend_from_slice(name);
        header.push(0);
        header.extend_from_slice(b"TEXTttxt\0\0");
        header.extend_from_slice(&data_len.to_be_bytes());
        header.extend_from_slice(&resource_len.to_be_bytes());
        header
    }

    fn with_crc(section: &[u8]) -> Vec<u8> {
        let mut out = escape(section);
        out.extend(escape(&crc16(section).to_be_bytes()));
        out
    }

    // 6-bit layer only: `stream` is already run-length encoded.
    fn pack(stream: &[u8]) -> Vec<u8> {
        let mut out = b":".to_vec();
        for chunk in stream.chunks(3) {
            let mut group = [0u8; 3];
            group[..chunk.len()].copy_from_slice(chunk);
            let bits = u32::from_be_bytes([0, group[0], group[1], group[2]]);
            for i in 0..=chunk.len() {
                out.push(ALPHABET[(bits >> (18 - 6 * i)) as usize & 0x3F]);
            }
        }
        out
    }

    fn file(stream: &[u8]) -> Vec<u8> {
        let mut out = MARKER_LINE.to_vec();
        out.extend(pack(stream));
        out.push(b':');
        out
    }

    // File whose data fork is the run-length encoded `encoded`, with CRCs
    // claiming it expands to `expanded`.
    fn rle_file(encoded: &[u8], expanded: &[u8]) -> Vec<u8> {
        let mut stream = with_crc(&header(b"rle", expanded.len() as u32, 0));
        stream.extend_from_slice(encoded);
        stream.extend(escape(&crc16(expanded).to_be_bytes()));
        stream.extend_from_slice(&[0, 0]);
        file(&stream)
    }

    fn assert_rle(encoded: &[u8], expanded: &[u8]) {
        let container = match decode(&rle_file(encoded, expanded)) {
            Ok(container) => container,
            Err(err) => panic!("{encoded:02X?}: {err}"),
        };
        assert_eq!(container.data_fork, expanded, "{encoded:02X?}");
    }

    fn plain_file(name: &[u8], data: &[u8], resource: &[u8]) -> Vec<u8> {
        let mut stream = with_crc(&header(name, data.len() as u32, resource.len() as u32));
        stream.extend(with_crc(data));
        stream.extend(with_crc(resource));
        file(&stream)
    }

    #[test]
    fn decodes_python_reference_file() {
        let container = decode(REFERENCE).unwrap_or_else(|err| panic!("{err}"));
        assert_eq!(container.filename, "R\u{e9}sum\u{e9} \u{192}");
        assert_eq!(&container.file_type, b"TEXT");
        assert_eq!(&container.creator, b"ttxt");
        assert_eq!(container.data_fork, reference_data_fork());
        assert_eq!(
            container.resource_fork,
            b"\x90rsrc\x11\x11\x11\x11\x11\x11\x11\x90\x00\x90tail"
        );
        assert_eq!(container.prefix, "p");
    }

    #[test]
    fn expands_rfc_run_length_examples() {
        assert_rle(
            &[0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77],
            &[0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77],
        );
        assert_rle(
            &[0x11, 0x22, 0x90, 0x06, 0x33],
            &[0x11, 0x22, 0x22, 0x22, 0x22, 0x22, 0x22, 0x33],
        );
        assert_rle(
            &[0x11, 0x22, 0x90, 0x00, 0x33, 0x44],
            &[0x11, 0x22, 0x90, 0x33, 0x44],
        );
    }

    #[test]
    fn run_length_edge_cases() {
        assert_rle(&[0x41, 0x90, 0x01, 0x42], b"AB");
        assert_rle(&[0x41, 0x90, 0x02, 0x42], b"AAB");
        assert_rle(&[0x41, 0x90, 0x03], b"AAA");

        let mut longest = vec![0x41; 255];
        longest.push(0x42);
        assert_rle(&[0x41, 0x90, 0xFF, 0x42], &longest);

        // A run repeats a preceding literal marker byte.
        assert_rle(&[0x90, 0x00, 0x90, 0x04], &[0x90; 4]);
        assert_rle(&[0x41, 0x90, 0x02, 0x90, 0x02], b"AAA");
        assert_rle(&[0x90, 0x00, 0x90, 0x00], &[0x90, 0x90]);
    }

    #[test]
    fn run_crosses_section_boundaries() {
        // One run finishes the header (resource length 0); another spans
        // the end of the data fork and its CRC.
        let head = header(b"z", 2, 0);
        let mut stream = head[..head.len() - 3].to_vec();
        stream.extend_from_slice(&[0x90, 0x04]);
        stream.extend(escape(&crc16(&head).to_be_bytes()));
        stream.extend_from_slice(&[0x00, 0x90, 0x04, 0x00, 0x00]);
        assert_eq!(crc16(&[0, 0]), 0);
        let container = decode(&file(&stream)).unwrap_or_else(|err| panic!("{err}"));
        assert_eq!(container.data_fork, [0, 0]);
        assert!(container.resource_fork.is_empty());
    }

    #[test]
    fn run_never_expands_past_declared_length() {
        // The final run asks for 255 bytes but only the two CRC bytes are read.
        let mut stream = with_crc(&header(b"z", 0, 0));
        stream.extend_from_slice(&[0x00, 0x00, 0x00, 0x90, 0xFF]);
        let container = decode(&file(&stream)).unwrap_or_else(|err| panic!("{err}"));
        assert!(container.data_fork.is_empty());
        assert!(container.resource_fork.is_empty());
    }

    #[test]
    fn rejects_run_with_no_preceding_byte() {
        let err = error_of(&file(&[0x90, 0x05, 0x41]));
        assert!(
            err.contains("Invalid BinHex file: run-length marker"),
            "{err}"
        );
    }

    #[test]
    fn reads_both_forks_and_metadata() {
        let container = decode(&plain_file(
            b"Read Me \xA5",
            b"data\x90fork",
            b"\x90\x90res",
        ))
        .unwrap_or_else(|err| panic!("{err}"));
        assert_eq!(container.filename, "Read Me \u{2022}");
        assert_eq!(&container.file_type, b"TEXT");
        assert_eq!(&container.creator, b"ttxt");
        assert_eq!(container.data_fork, b"data\x90fork");
        assert_eq!(container.resource_fork, b"\x90\x90res");
        assert_eq!(
            container.get_file("p/Read Me \u{2022}"),
            Some(&b"data\x90fork"[..])
        );
    }

    #[test]
    fn ignores_preamble_trailing_text_and_whitespace() {
        let mut input = b"From: someone\r\nSubject: a file\r\n\r\nHere: it is.\r\n".to_vec();
        input.extend_from_slice(REFERENCE);
        input.extend_from_slice(b"\r\n-- \r\nsignature: ~~~ {not alphabet}\r\n");
        let container = decode(&input).unwrap_or_else(|err| panic!("{err}"));
        assert_eq!(container.data_fork, reference_data_fork());

        let spaced: Vec<u8> = plain_file(b"a", b"xyz", b"")
            .iter()
            .flat_map(|&b| [b, b' ', b'\t', b'\r', b'\n'])
            .collect();
        // Spreading the marker line apart would hide it; keep it intact.
        let mut input = MARKER_LINE.to_vec();
        input.extend_from_slice(&spaced[MARKER_LINE.len() * 5..]);
        let container = decode(&input).unwrap_or_else(|err| panic!("{err}"));
        assert_eq!(container.data_fork, b"xyz");
    }

    #[test]
    fn tolerates_missing_closing_colon() {
        let mut input = plain_file(b"a", b"xyz", b"r");
        assert_eq!(input.pop(), Some(b':'));
        let container = decode(&input).unwrap_or_else(|err| panic!("{err}"));
        assert_eq!(container.data_fork, b"xyz");
        assert_eq!(container.resource_fork, b"r");
    }

    #[test]
    fn rejects_missing_payload() {
        let err = error_of(MARKER_LINE);
        assert!(
            err.contains("Invalid BinHex file: no encoded data"),
            "{err}"
        );
        assert!(decode(b"no marker here").is_err());
        assert!(decode(b"").is_err());
    }

    #[test]
    fn rejects_header_crc_mismatch() {
        let mut stream = escape(&header(b"a", 3, 0));
        stream.extend_from_slice(&[0x12, 0x34]);
        stream.extend(with_crc(b"xyz"));
        stream.extend_from_slice(&[0, 0]);
        let err = error_of(&file(&stream));
        assert!(
            err.contains("Invalid BinHex file: header CRC mismatch"),
            "{err}"
        );
    }

    #[test]
    fn rejects_fork_crc_mismatch() {
        let mut stream = with_crc(&header(b"a", 3, 1));
        stream.extend_from_slice(b"xyz\x12\x34");
        stream.extend(with_crc(b"r"));
        let err = error_of(&file(&stream));
        assert!(
            err.contains("Invalid BinHex file: data fork CRC mismatch"),
            "{err}"
        );

        let mut stream = with_crc(&header(b"a", 3, 1));
        stream.extend(with_crc(b"xyz"));
        stream.extend_from_slice(b"r\x12\x34");
        let err = error_of(&file(&stream));
        assert!(
            err.contains("Invalid BinHex file: resource fork CRC mismatch"),
            "{err}"
        );
    }

    #[test]
    fn rejects_empty_filename() {
        let err = error_of(&plain_file(b"", b"xyz", b""));
        assert!(err.contains("Invalid BinHex file: empty filename"), "{err}");
    }

    #[test]
    fn rejects_every_truncation() {
        let end = REFERENCE
            .iter()
            .rposition(|&b| b == b':')
            .expect("closing colon");
        for cut in 0..end {
            assert!(decode(&REFERENCE[..cut]).is_err(), "cut at {cut}");
        }
        assert!(decode(&REFERENCE[..end]).is_ok());

        let input = plain_file(b"a", b"xyz", b"rsrc");
        let err = error_of(&input[..MARKER_LINE.len() + 10]);
        assert!(
            err.contains("Invalid BinHex file: truncated header"),
            "{err}"
        );
        let err = error_of(&input[..MARKER_LINE.len() + 34]);
        assert!(
            err.contains("Invalid BinHex file: truncated data fork"),
            "{err}"
        );
        let err = error_of(&input[..input.len() - 4]);
        assert!(
            err.contains("Invalid BinHex file: truncated resource fork"),
            "{err}"
        );
    }

    #[test]
    fn rejects_bytes_outside_alphabet() {
        // 'g', '7', 'O', 'W', 'n', 'o' and '.' are not in the alphabet.
        for bad in [
            b'g', b'7', b'O', b'W', b'n', b'o', b'.', b'~', 0x00, 0x80, 0xFF,
        ] {
            let mut input = plain_file(b"a", b"xyz", b"");
            input[MARKER_LINE.len() + 5] = bad;
            let err = error_of(&input);
            assert!(
                err.contains("outside the encoding alphabet"),
                "{bad:#04X}: {err}"
            );
        }
    }

    #[test]
    fn huge_declared_fork_fails_fast() {
        let mut stream = with_crc(&header(b"big", u32::MAX, 0));
        stream.extend_from_slice(b"tiny");
        let err = error_of(&file(&stream));
        assert!(
            err.contains("Invalid BinHex file: truncated data fork"),
            "{err}"
        );

        // A maximal run still stops at the end of the input.
        let mut stream = with_crc(&header(b"big", 0, u32::MAX));
        stream.extend_from_slice(&[0, 0, 0x41, 0x90, 0xFF]);
        let err = error_of(&file(&stream));
        assert!(
            err.contains("Invalid BinHex file: truncated resource fork"),
            "{err}"
        );
    }

    #[test]
    fn rejects_forks_over_size_limit() {
        let stream = with_crc(&header(b"big", u32::MAX, u32::MAX));
        let err = error_of(&file(&stream));
        assert!(err.contains("Invalid BinHex file"), "{err}");
        assert!(err.contains("exceeds"), "{err}");
    }

    #[test]
    fn never_panics_on_corrupted_input() {
        let base = plain_file(b"fuzz", b"some\x90\x90\x90 data bytes", b"rsrc\x90");
        let mut state = 0x2545_F491u32;
        for _ in 0..20_000 {
            let mut input = base.clone();
            for _ in 0..3 {
                state ^= state << 13;
                state ^= state >> 17;
                state ^= state << 5;
                let at =
                    MARKER_LINE.len() + (state as usize >> 8) % (input.len() - MARKER_LINE.len());
                input[at] = state as u8;
            }
            let _ = decode(&input);
        }
    }
}
