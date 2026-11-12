//! Bzip2 compressed file support.

use crate::compat::{String, Vec, format};
use crate::error::Result;
use crate::format::ContainerFormat;
use crate::sanitize_path_component;
use crate::{Container, ContainerInfo, Entry, Metadata};

use super::bzip2::bunzip2;

const BLOCK_MAGIC: [u8; 6] = [0x31, 0x41, 0x59, 0x26, 0x53, 0x59];
// Follows the stream header directly when the stream holds no data.
const FOOTER_MAGIC: [u8; 6] = [0x17, 0x72, 0x45, 0x38, 0x50, 0x90];

/// Check if data looks like a bzip2 file.
#[must_use]
pub fn is_bzip2_file(data: &[u8]) -> bool {
    match data {
        [b'B', b'Z', b'h', b'1'..=b'9', rest @ ..] => {
            rest.starts_with(&BLOCK_MAGIC) || rest.starts_with(&FOOTER_MAGIC)
        }
        _ => false,
    }
}

/// Container for bzip2-compressed files.
///
/// Bzip2 wraps a single file, so this yields at most one entry.
pub struct Bzip2Container {
    /// Display path prefix.
    prefix: String,
    /// Inner filename (derived from outer name minus .bz2).
    inner_name: String,
    /// Decompressed data.
    data: Vec<u8>,
}

impl Bzip2Container {
    /// Open a bzip2 file from bytes.
    ///
    /// Concatenated streams are decoded as one file; bytes after the last
    /// stream are ignored.
    pub fn from_bytes(
        data: &[u8],
        prefix: String,
        inner_name: String,
        _depth: u32,
    ) -> Result<Self> {
        let decompressed = bunzip2(data, 0, crate::MAX_DECOMPRESSED_SIZE)?.data;
        Ok(Self {
            prefix,
            inner_name,
            data: decompressed,
        })
    }

    fn entry_path(&self) -> String {
        format!(
            "{}/{}",
            self.prefix,
            sanitize_path_component(&self.inner_name)
        )
    }
}

impl Container for Bzip2Container {
    fn prefix(&self) -> &str {
        &self.prefix
    }

    fn visit(&self, visitor: &mut dyn FnMut(&Entry<'_>) -> Result<bool>) -> Result<()> {
        let full_path = self.entry_path();
        let metadata = Metadata::new().with_compression_method("bzip2");
        let entry = Entry::new(&full_path, &self.prefix, &self.data).with_metadata(&metadata);
        visitor(&entry)?;
        Ok(())
    }

    fn info(&self) -> ContainerInfo {
        ContainerInfo {
            path: self.prefix.clone(),
            format: ContainerFormat::Bzip2,
            entry_count: Some(1),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compat::ToString;

    const TEXT: &[u8] = b"It was the best of times, it was the worst of times, it was the age of wisdom, it was the age of foolishness";
    const SECOND: &[u8] = b"second stream\n";

    // python3 bz2.compress output for TEXT (levels 9 and 1), SECOND and b"".
    const BZ_TEXT: &[u8] =
        b"\x42\x5a\x68\x39\x31\x41\x59\x26\x53\x59\x61\x04\xe2\x87\x00\x00\x0d\x15\x80\x40\
        \x04\x00\x20\x37\xe7\x9c\x80\x20\x00\x54\x25\x43\xd5\x1a\x69\xe5\x0f\x4c\xa0\x94\
        \xd4\xd2\x64\x33\x43\x51\x66\xbb\xb5\x24\x52\x15\x1c\x13\xea\xad\x63\xe0\x54\x86\
        \x81\x74\x72\x24\x2f\xa3\x21\xe2\x1a\x79\xb6\x49\x1d\x02\x98\xe9\x30\x40\xfd\x98\
        \xab\xa1\x15\x76\x0b\x7e\x2e\xe4\x8a\x70\xa1\x20\xc2\x09\xc5\x0e";
    const BZ_TEXT_LEVEL1: &[u8] =
        b"\x42\x5a\x68\x31\x31\x41\x59\x26\x53\x59\x61\x04\xe2\x87\x00\x00\x0d\x15\x80\x40\
        \x04\x00\x20\x37\xe7\x9c\x80\x20\x00\x54\x25\x43\xd5\x1a\x69\xe5\x0f\x4c\xa0\x94\
        \xd4\xd2\x64\x33\x43\x51\x66\xbb\xb5\x24\x52\x15\x1c\x13\xea\xad\x63\xe0\x54\x86\
        \x81\x74\x72\x24\x2f\xa3\x21\xe2\x1a\x79\xb6\x49\x1d\x02\x98\xe9\x30\x40\xfd\x98\
        \xab\xa1\x15\x76\x0b\x7e\x2e\xe4\x8a\x70\xa1\x20\xc2\x09\xc5\x0e";
    const BZ_SECOND: &[u8] =
        b"\x42\x5a\x68\x39\x31\x41\x59\x26\x53\x59\x84\x52\x90\x94\x00\x00\x05\xd1\x80\x00\
        \x10\x40\x00\x2e\x03\x9c\x00\x20\x00\x22\x00\xd0\xd0\x40\xd0\x34\x05\x04\xbd\x38\
        \x4f\x1e\x1e\x2e\xe4\x8a\x70\xa1\x21\x08\xa5\x21\x28";
    const BZ_EMPTY: &[u8] = b"\x42\x5a\x68\x39\x17\x72\x45\x38\x50\x90\x00\x00\x00\x00";

    fn open(data: &[u8]) -> Result<Bzip2Container> {
        Bzip2Container::from_bytes(data, "t.bz2".to_string(), "t".to_string(), 1)
    }

    fn decode(data: &[u8]) -> Vec<u8> {
        open(data).expect("valid bzip2 file").data
    }

    fn error(data: &[u8]) -> String {
        match open(data) {
            Ok(_) => panic!("corrupt bzip2 file was accepted"),
            Err(error) => error.to_string(),
        }
    }

    #[test]
    fn detects_stream_header_and_block_magic() {
        for file in [BZ_TEXT, BZ_TEXT_LEVEL1, BZ_SECOND, BZ_EMPTY] {
            assert!(is_bzip2_file(file));
        }
        assert!(is_bzip2_file(&BZ_TEXT[..10]));
        assert!(!is_bzip2_file(&BZ_TEXT[..9]));
        assert!(!is_bzip2_file(b"BZh9"));
        assert!(!is_bzip2_file(b"BZh0\x31\x41\x59\x26\x53\x59"));
        assert!(!is_bzip2_file(b"BZh9 is not a stream"));
        assert!(!is_bzip2_file(b"BZ09\x31\x41\x59\x26\x53\x59"));
        assert!(!is_bzip2_file(b"\x1f\x8b\x08\x00"));
        assert!(!is_bzip2_file(b""));
    }

    #[test]
    fn yields_one_entry_named_after_the_file() {
        let container = open(BZ_TEXT).unwrap();
        assert_eq!(container.info().format, ContainerFormat::Bzip2);
        assert_eq!(container.info().entry_count, Some(1));

        let mut seen = Vec::new();
        container
            .visit(&mut |entry| {
                let method = entry.metadata.and_then(|m| m.compression_method.clone());
                seen.push((entry.path.to_string(), entry.data.to_vec(), method));
                Ok(true)
            })
            .unwrap();
        assert_eq!(
            seen,
            [(
                "t.bz2/t".to_string(),
                TEXT.to_vec(),
                Some("bzip2".to_string())
            )]
        );
    }

    #[test]
    fn decodes_python_vectors() {
        assert_eq!(decode(BZ_TEXT), TEXT);
        assert_eq!(decode(BZ_TEXT_LEVEL1), TEXT);
        assert_eq!(decode(BZ_SECOND), SECOND);
        assert_eq!(decode(BZ_EMPTY), b"");
    }

    #[test]
    fn concatenates_streams() {
        let two = [BZ_TEXT, BZ_SECOND].concat();
        assert_eq!(decode(&two), [TEXT, SECOND].concat());
        let many = [BZ_EMPTY, BZ_TEXT_LEVEL1, BZ_EMPTY, BZ_SECOND].concat();
        assert_eq!(decode(&many), [TEXT, SECOND].concat());
        assert_eq!(decode(&[BZ_EMPTY, BZ_EMPTY].concat()), b"");
    }

    #[test]
    fn ignores_trailing_garbage() {
        for garbage in [&b"garbage"[..], &[0u8; 512], b"BZ", b"BZh0", b"\n"] {
            assert_eq!(decode(&[BZ_TEXT, garbage].concat()), TEXT);
            let data = [BZ_TEXT, BZ_SECOND, garbage].concat();
            assert_eq!(decode(&data), [TEXT, SECOND].concat());
        }
    }

    #[test]
    fn rejects_corrupt_data() {
        // Inside the compressed block: the block or stream CRC no longer fits,
        // or the block stops decoding.
        for index in [20, BZ_TEXT.len() / 2, BZ_TEXT.len() - 12] {
            let mut data = BZ_TEXT.to_vec();
            data[index] ^= 0x10;
            assert!(error(&data).contains("bzip2"), "byte {index}");
        }
        // Stored block CRC.
        let mut data = BZ_TEXT.to_vec();
        data[10] ^= 1;
        assert!(error(&data).contains("CRC mismatch"));

        assert!(error(b"BZh9 is not a stream").contains("block signature"));
        assert!(error(b"not bzip2 at all").contains("stream signature"));
        // A second stream that is damaged fails the whole file.
        let mut data = [BZ_TEXT, BZ_SECOND].concat();
        data[BZ_TEXT.len() + 10] ^= 1;
        assert!(error(&data).contains("CRC mismatch"));
    }

    #[test]
    fn rejects_every_truncation() {
        for cut in 0..BZ_TEXT.len() {
            assert!(open(&BZ_TEXT[..cut]).is_err(), "cut at {cut}");
        }
        let two = [BZ_TEXT, BZ_SECOND].concat();
        for cut in BZ_TEXT.len() + 4..two.len() {
            assert!(open(&two[..cut]).is_err(), "cut at {cut}");
        }
    }
}
