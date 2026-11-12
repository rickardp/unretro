//! XZ and legacy `.lzma` compressed file support.

use crate::compat::{String, Vec, format};
use crate::error::{Error, Result};
use crate::format::ContainerFormat;
use crate::sanitize_path_component;
use crate::{Container, ContainerInfo, Entry, Metadata};

use super::lzma::{LzmaProps, lzma_alone_decode};
use super::xz_decode::xz_decode;

const MAGIC: [u8; 6] = [0xFD, b'7', b'z', b'X', b'Z', 0x00];

/// Check if data looks like an xz file.
#[must_use]
pub fn is_xz_file(data: &[u8]) -> bool {
    data.starts_with(&MAGIC)
}

/// Check if data could be a legacy `.lzma` file.
///
/// The format has no magic: this only checks the properties byte and the
/// first byte of the stream, so callers should also require the extension.
#[must_use]
pub fn is_lzma_file(data: &[u8]) -> bool {
    // 13 header bytes and the 5 bytes that initialise the range decoder.
    data.len() >= 18 && LzmaProps::from_byte(data[0]).is_ok() && data[13] == 0
}

/// Container for xz-compressed and legacy `.lzma` files.
///
/// Both wrap a single file, so this yields at most one entry.
pub struct XzContainer {
    /// Display path prefix.
    prefix: String,
    /// Inner filename (derived from outer name minus .xz).
    inner_name: String,
    /// Decompressed data.
    data: Vec<u8>,
    method: &'static str,
}

impl XzContainer {
    /// Open an xz file, or a legacy `.lzma` file when the xz magic is absent.
    ///
    /// Concatenated xz streams are decoded as one file. Bytes after the last
    /// stream other than xz stream padding are an error.
    pub fn from_bytes(
        data: &[u8],
        prefix: String,
        inner_name: String,
        _depth: u32,
    ) -> Result<Self> {
        let (decompressed, method) = if is_xz_file(data) || !is_lzma_file(data) {
            let decoded = xz_decode(data, 0, crate::MAX_DECOMPRESSED_SIZE)?;
            (decoded.data, "lzma2")
        } else {
            let decoded = lzma_alone_decode(data, crate::MAX_DECOMPRESSED_SIZE)?;
            if decoded.consumed != data.len() {
                return Err(Error::decompression(
                    "lzma: data after the end of the stream",
                ));
            }
            (decoded.data, "lzma")
        };
        Ok(Self {
            prefix,
            inner_name,
            data: decompressed,
            method,
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

impl Container for XzContainer {
    fn prefix(&self) -> &str {
        &self.prefix
    }

    fn visit(&self, visitor: &mut dyn FnMut(&Entry<'_>) -> Result<bool>) -> Result<()> {
        let full_path = self.entry_path();
        let metadata = Metadata::new().with_compression_method(self.method);
        let entry = Entry::new(&full_path, &self.prefix, &self.data).with_metadata(&metadata);
        visitor(&entry)?;
        Ok(())
    }

    fn info(&self) -> ContainerInfo {
        ContainerInfo {
            path: self.prefix.clone(),
            format: ContainerFormat::Xz,
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

    // TEXT from `xz` (CRC64), python3 lzma.compress (CRC32) and
    // `xz -C sha256 -0`; SECOND from python3 without a check; b"" from `xz`;
    // TEXT from `xz --x86 --lzma2=preset=0`.
    const XZ_TEXT: &[u8] =
        b"\xfd\x37\x7a\x58\x5a\x00\x00\x04\xe6\xd6\xb4\x46\x04\xc0\x45\x6c\x21\x01\x16\x00\x00\
        \x00\x00\x00\x00\x00\x00\x00\xd5\xcf\x0f\x85\xe0\x00\x6b\x00\x3d\x5d\x00\x24\x9d\x00\
        \x07\x73\x43\x85\x19\xc1\xbc\x61\x9e\x16\x43\x3f\x49\xa5\x27\x15\xb4\xf5\x10\x18\x54\
        \xbf\xcd\x91\x38\x3b\x33\x8b\x52\x7d\x4a\xe7\x72\xd2\xb1\xd8\x4a\x2a\x51\x00\x44\xb2\
        \xe1\x69\x9f\x12\x41\xf5\x50\xc0\x17\xd2\xcd\xfc\x4d\x3c\x61\x00\x00\x00\x00\x00\xd8\
        \x73\x7a\x46\x2a\x63\x74\xd8\x00\x01\x61\x6c\xbe\xc2\x64\x18\x1f\xb6\xf3\x7d\x01\x00\
        \x00\x00\x00\x04\x59\x5a";
    const XZ_TEXT_CRC32: &[u8] =
        b"\xfd\x37\x7a\x58\x5a\x00\x00\x01\x69\x22\xde\x36\x02\x00\x21\x01\x16\x00\x00\x00\x74\
        \x2f\xe5\xa3\xe0\x00\x6b\x00\x3d\x5d\x00\x24\x9d\x00\x07\x73\x43\x85\x19\xc1\xbc\x61\
        \x9e\x16\x43\x3f\x49\xa5\x27\x15\xb4\xf5\x10\x18\x54\xbf\xcd\x91\x38\x3b\x33\x8b\x52\
        \x7d\x4a\xe7\x72\xd2\xb1\xd8\x4a\x2a\x51\x00\x44\xb2\xe1\x69\x9f\x12\x41\xf5\x50\xc0\
        \x17\xd2\xcd\xfc\x4d\x3c\x61\x00\x00\x00\x00\x00\xbf\xd9\x5a\x7a\x00\x01\x55\x6c\x49\
        \x31\x4e\xa3\x90\x42\x99\x0d\x01\x00\x00\x00\x00\x01\x59\x5a";
    const XZ_TEXT_SHA256: &[u8] =
        b"\xfd\x37\x7a\x58\x5a\x00\x00\x0a\xe1\xfb\x0c\xa1\x03\xc0\x46\x6c\x21\x01\x0c\x00\x00\
        \x00\x00\x00\xbf\x94\x78\x60\xe0\x00\x6b\x00\x3e\x5d\x00\x24\x9d\x00\x07\x73\x43\x85\
        \x19\xc1\xbc\x61\x9e\x16\x43\x3f\x49\xa5\x27\x15\xb4\xf5\x10\x18\x54\xbf\xca\xe4\xa6\
        \xec\x1c\x23\xa5\x64\xdc\x70\x8f\xc4\x86\x2f\x3e\xdd\xe5\xec\xe1\xc4\x4a\xc3\xa9\xea\
        \xc4\x70\x56\x57\x03\x11\x2c\x95\x60\xa9\x71\x74\xff\x00\x00\x00\x1d\xbc\x5f\x82\xd7\
        \xc3\x59\xb8\xb1\x89\x66\x6d\xac\x7f\xd3\xa8\xde\xd2\xc8\xa3\xb3\x77\x44\xd5\xeb\x80\
        \xa9\xc0\xea\xf2\x19\xff\x00\x01\x76\x6c\x28\x46\xe7\x1d\x18\x9b\x4b\x9a\x01\x00\x00\
        \x00\x00\x0a\x59\x5a";
    const XZ_SECOND: &[u8] =
        b"\xfd\x37\x7a\x58\x5a\x00\x00\x00\xff\x12\xd9\x41\x02\x00\x21\x01\x16\x00\x00\x00\x74\
        \x2f\xe5\xa3\x01\x00\x0d\x73\x65\x63\x6f\x6e\x64\x20\x73\x74\x72\x65\x61\x6d\x0a\x00\
        \x00\x00\x00\x01\x1e\x0e\xf3\xa7\x7f\x13\x06\x72\x9e\x7a\x01\x00\x00\x00\x00\x00\x59\
        \x5a";
    const XZ_EMPTY: &[u8] =
        b"\xfd\x37\x7a\x58\x5a\x00\x00\x04\xe6\xd6\xb4\x46\x00\x00\x00\x00\x1c\xdf\x44\x21\x1f\
        \xb6\xf3\x7d\x01\x00\x00\x00\x00\x04\x59\x5a";
    const XZ_TEXT_X86: &[u8] =
        b"\xfd\x37\x7a\x58\x5a\x00\x00\x04\xe6\xd6\xb4\x46\x04\xc1\x46\x6c\x04\x00\x21\x01\x0c\
        \x00\x00\x00\x00\x00\x00\x00\x83\xda\x72\xcb\xe0\x00\x6b\x00\x3e\x5d\x00\x24\x9d\x00\
        \x07\x73\x43\x85\x19\xc1\xbc\x61\x9e\x16\x43\x3f\x49\xa5\x27\x15\xb4\xf5\x10\x18\x54\
        \xbf\xca\xe4\xa6\xec\x1c\x23\xa5\x64\xdc\x70\x8f\xc4\x86\x2f\x3e\xdd\xe5\xec\xe1\xc4\
        \x4a\xc3\xa9\xea\xc4\x70\x56\x57\x03\x11\x2c\x95\x60\xa9\x71\x74\xff\x00\x00\x00\xd8\
        \x73\x7a\x46\x2a\x63\x74\xd8\x00\x01\x62\x6c\x7d\x91\x49\x33\x1f\xb6\xf3\x7d\x01\x00\
        \x00\x00\x00\x04\x59\x5a";

    fn open(data: &[u8]) -> Result<XzContainer> {
        XzContainer::from_bytes(data, "t.xz".to_string(), "t".to_string(), 1)
    }

    fn decode(data: &[u8]) -> Vec<u8> {
        open(data).expect("valid xz file").data
    }

    fn error(data: &[u8]) -> String {
        match open(data) {
            Ok(_) => panic!("corrupt xz file was accepted"),
            Err(error) => error.to_string(),
        }
    }

    #[test]
    fn detects_the_stream_header_magic() {
        for file in [XZ_TEXT, XZ_TEXT_CRC32, XZ_TEXT_SHA256, XZ_SECOND, XZ_EMPTY] {
            assert!(is_xz_file(file));
        }
        assert!(is_xz_file(&XZ_TEXT[..6]));
        assert!(!is_xz_file(&XZ_TEXT[..5]));
        assert!(!is_xz_file(b"\xfd7zXZ\x01"));
        assert!(!is_xz_file(b"7zXZ\x00\x00"));
        assert!(!is_xz_file(b"\x1f\x8b\x08\x00"));
        assert!(!is_xz_file(b""));
    }

    #[test]
    fn yields_one_entry_named_after_the_file() {
        let container = open(XZ_TEXT).unwrap();
        assert_eq!(container.info().format, ContainerFormat::Xz);
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
                "t.xz/t".to_string(),
                TEXT.to_vec(),
                Some("lzma2".to_string())
            )]
        );
    }

    #[test]
    fn decodes_tool_vectors_with_every_check() {
        assert_eq!(decode(XZ_TEXT), TEXT);
        assert_eq!(decode(XZ_TEXT_CRC32), TEXT);
        assert_eq!(decode(XZ_TEXT_SHA256), TEXT);
        assert_eq!(decode(XZ_SECOND), SECOND);
        assert_eq!(decode(XZ_EMPTY), b"");
    }

    #[test]
    fn concatenates_streams_and_skips_stream_padding() {
        let two = [XZ_TEXT, XZ_SECOND].concat();
        assert_eq!(decode(&two), [TEXT, SECOND].concat());
        let many = [
            XZ_EMPTY,
            XZ_TEXT_CRC32,
            &[0u8; 8],
            XZ_EMPTY,
            XZ_SECOND,
            &[0u8; 4],
        ]
        .concat();
        assert_eq!(decode(&many), [TEXT, SECOND].concat());
        assert_eq!(decode(&[XZ_EMPTY, XZ_EMPTY].concat()), b"");
    }

    #[test]
    fn rejects_trailing_garbage() {
        for garbage in [&b"garbage"[..], &[0u8; 3], b"\xfd7zX", b"\n"] {
            assert!(error(&[XZ_TEXT, garbage].concat()).contains("xz"));
            assert!(open(&[XZ_TEXT, XZ_SECOND, garbage].concat()).is_err());
        }
    }

    #[test]
    fn rejects_corrupt_data() {
        // Stream header, block header, compressed data, check, index, footer.
        for index in 0..XZ_TEXT.len() {
            let mut data = XZ_TEXT.to_vec();
            data[index] ^= 0x10;
            assert!(error(&data).contains("xz"), "byte {index}");
        }
        // Stored SHA-256 and CRC32 of the data.
        for file in [XZ_TEXT_SHA256, XZ_TEXT_CRC32] {
            let check_end = file.len() - 12 - 12;
            let mut data = file.to_vec();
            data[check_end - 1] ^= 1;
            assert!(error(&data).contains("xz"));
        }

        assert!(error(b"\xfd7zXZ\x00 is not a stream").contains("xz"));
        assert!(error(b"not xz at all").contains("xz"));
        // A second stream that is damaged fails the whole file.
        let mut data = [XZ_TEXT, XZ_SECOND].concat();
        data[XZ_TEXT.len() + 8] ^= 1;
        assert!(error(&data).contains("xz"));
    }

    #[test]
    fn rejects_every_truncation() {
        for cut in 0..XZ_TEXT.len() {
            assert!(open(&XZ_TEXT[..cut]).is_err(), "cut at {cut}");
        }
        let two = [XZ_TEXT, XZ_SECOND].concat();
        for cut in XZ_TEXT.len() + 1..two.len() {
            assert!(open(&two[..cut]).is_err(), "cut at {cut}");
        }
    }

    #[test]
    fn reports_bcj_filters_as_unsupported() {
        let message = error(XZ_TEXT_X86);
        assert!(message.contains("BCJ filter is not supported"), "{message}");
    }

    // TEXT from `xz --format=lzma`.
    const LZMA_TEXT: &[u8] = b"\
        \x5d\x00\x00\x80\x00\xff\xff\xff\xff\xff\xff\xff\xff\x00\x24\x9d\x00\x07\x73\x43\
        \x85\x19\xc1\xbc\x61\x9e\x16\x43\x3f\x49\xa5\x27\x15\xb4\xf5\x10\x18\x54\xbf\xcd\
        \x91\x38\x3b\x33\x8b\x52\x7d\x4a\xe7\x72\xd2\xb1\xd8\x4a\x2a\x51\x00\x44\xb2\xe1\
        \x69\x9f\x12\x41\xf5\x50\xc0\x17\xd2\xcd\xfc\xcc\x1b\x73\xed\xff\xd3\x25\x70\x00";

    fn lzma_with_size(size: u64) -> Vec<u8> {
        let mut data = LZMA_TEXT.to_vec();
        data[5..13].copy_from_slice(&size.to_le_bytes());
        data
    }

    #[test]
    fn legacy_lzma_file_yields_one_entry() {
        assert!(is_lzma_file(LZMA_TEXT));
        assert!(!is_xz_file(LZMA_TEXT));
        let container = open(LZMA_TEXT).unwrap();
        assert_eq!(container.data, TEXT);
        assert_eq!(container.method, "lzma");
        assert_eq!(open(XZ_TEXT).unwrap().method, "lzma2");

        // A header that states the size instead of leaving it to the marker.
        assert_eq!(decode(&lzma_with_size(TEXT.len() as u64)), TEXT);
    }

    #[test]
    fn legacy_lzma_header_is_only_a_plausibility_check() {
        assert!(!is_lzma_file(&LZMA_TEXT[..17]));
        let mut data = LZMA_TEXT.to_vec();
        data[0] = 9 * 5 * 5;
        assert!(!is_lzma_file(&data));
        let mut data = LZMA_TEXT.to_vec();
        data[13] = 1;
        assert!(!is_lzma_file(&data));
        assert!(!is_lzma_file(XZ_TEXT));
    }

    #[test]
    fn legacy_lzma_rejects_trailing_bytes_truncation_and_wrong_sizes() {
        for extra in [&b"junk"[..], &[0u8; 4], LZMA_TEXT] {
            assert!(error(&[LZMA_TEXT, extra].concat()).contains("lzma"));
        }
        for len in 0..LZMA_TEXT.len() {
            assert!(open(&LZMA_TEXT[..len]).is_err(), "{len}");
        }
        for size in [0, TEXT.len() as u64 - 1, TEXT.len() as u64 + 1] {
            assert!(open(&lzma_with_size(size)).is_err(), "{size}");
        }
    }
}
