//! `SCUMM` speech bundle (`MONSTER.SOU` and friends).
//!
//! Forward-only reader for the talkie-version voice file used by SCUMM CD
//! releases (Monkey Island 1/2 talkie, Fate of Atlantis, etc.). The file
//! header is `b"SOU \x00\x00\x00\x00"`, followed by a sequence of
//! `(VCTL header, VOC blob)` pairs:
//!
//! - **`VCTL`** chunk: 4-byte tag, 4-byte big-endian total chunk size
//!   (including the 8-byte header), then `size - 8` bytes of lipsync timing.
//! - **VOC blob**: Creative Voice File starting with
//!   `b"Creative Voice File\x1a"`. Walked via the standard VOC block list to
//!   find the next pair's offset.
//!
//! Each voice clip is exposed as a sequentially-numbered `.voc` entry. The
//! original VCTL lipsync data is skipped — unretro only emits the audio
//! container and never decodes its samples.
//!
//! Reference: ScummVM `engines/scumm/sound.cpp` (the `MonsterSoundFile`
//! handling around `MKTAG('V','C','T','L')`).

use crate::compat::{FastMap, String, Vec, format};
use crate::error::{Error, Result};
use crate::format::ContainerFormat;
use crate::{Container, ContainerInfo, Entry};

const SOU_MAGIC: &[u8; 4] = b"SOU ";
const SOU_HEADER_SIZE: usize = 8;
const VCTL_TAG: &[u8; 4] = b"VCTL";
const VOC_MAGIC: &[u8; 20] = b"Creative Voice File\x1a";
const VOC_HEADER_SIZE: usize = 26;

/// `SOU ` alone is ambiguous: it is also the magic of an internal SCUMM v5
/// sound sub-resource (the MIDI/ADL/ROL/SBL multi-track wrapper inside a
/// `SOUN` chunk). The talkie speech bundle is distinguishable by the four
/// zero bytes that follow the magic and the leading `VCTL` chunk that comes
/// next — together those make the false-positive rate effectively zero.
#[must_use]
pub fn is_scumm_speech_file(data: &[u8]) -> bool {
    data.len() >= SOU_HEADER_SIZE + VCTL_TAG.len()
        && &data[0..4] == SOU_MAGIC
        && data[4..8] == [0u8; 4]
        && &data[8..12] == VCTL_TAG
}

struct SpeechEntry {
    path: String,
    offset: usize,
    size: usize,
}

pub struct ScummSpeechContainer {
    prefix: String,
    source: Vec<u8>,
    entries: Vec<SpeechEntry>,
    path_index: FastMap<String, usize>,
}

impl ScummSpeechContainer {
    pub fn from_bytes(data: &[u8], prefix: String, depth: u32) -> Result<Self> {
        if depth == 0 {
            return Err(Error::MaxDepthExceeded);
        }
        if !is_scumm_speech_file(data) {
            return Err(Error::invalid_format("Not a SCUMM speech (.SOU) file"));
        }

        let mut entries = Vec::new();
        let mut pos = SOU_HEADER_SIZE;
        let mut idx: usize = 0;

        while pos < data.len() {
            // VCTL header: tag + BE size (size includes the 8-byte header).
            if pos + 8 > data.len() {
                return Err(Error::invalid_format(format!(
                    "SCUMM speech: VCTL header truncated at offset {pos}"
                )));
            }
            if &data[pos..pos + 4] != VCTL_TAG {
                return Err(Error::invalid_format(format!(
                    "SCUMM speech: expected VCTL at offset {pos}"
                )));
            }
            let vctl_size = u32::from_be_bytes(data[pos + 4..pos + 8].try_into().unwrap()) as usize;
            if vctl_size < 8 {
                return Err(Error::invalid_format("SCUMM speech: VCTL size < 8"));
            }
            let voc_off = pos
                .checked_add(vctl_size)
                .ok_or_else(|| Error::invalid_format("SCUMM speech: VCTL size overflow"))?;
            if voc_off > data.len() {
                return Err(Error::invalid_format(
                    "SCUMM speech: VCTL extends past end of file",
                ));
            }

            let voc_len = parse_voc_length(&data[voc_off..])?;
            let path = format!("{prefix}/speech_{idx:04}.voc");
            entries.push(SpeechEntry {
                path,
                offset: voc_off,
                size: voc_len,
            });
            idx += 1;
            pos = voc_off + voc_len;
        }

        let path_index =
            crate::formats::build_path_index(entries.iter().enumerate().map(|(i, e)| (i, &e.path)));

        Ok(Self {
            prefix,
            source: data.to_vec(),
            entries,
            path_index,
        })
    }
}

/// Walk a Creative Voice File starting at `buf[0]` and return its total
/// length in bytes (header + all blocks up to and including the type-0
/// terminator). If the stream runs to EOF without a terminator, the entire
/// remaining buffer is treated as the VOC payload.
fn parse_voc_length(buf: &[u8]) -> Result<usize> {
    if buf.len() < VOC_HEADER_SIZE || &buf[0..20] != VOC_MAGIC {
        return Err(Error::invalid_format(
            "SCUMM speech: expected Creative Voice File header",
        ));
    }
    let data_off = u16::from_le_bytes([buf[20], buf[21]]) as usize;
    if data_off < VOC_HEADER_SIZE || data_off > buf.len() {
        return Err(Error::invalid_format(
            "SCUMM speech: VOC data offset out of range",
        ));
    }
    let mut pos = data_off;
    while pos < buf.len() {
        let block_type = buf[pos];
        pos += 1;
        if block_type == 0 {
            // Type-0 terminator is a single byte and ends the VOC stream.
            return Ok(pos);
        }
        if pos + 3 > buf.len() {
            return Err(Error::invalid_format("SCUMM speech: VOC block truncated"));
        }
        let size =
            (buf[pos] as usize) | ((buf[pos + 1] as usize) << 8) | ((buf[pos + 2] as usize) << 16);
        pos += 3;
        let end = pos
            .checked_add(size)
            .ok_or_else(|| Error::invalid_format("SCUMM speech: VOC block size overflow"))?;
        if end > buf.len() {
            return Err(Error::invalid_format(
                "SCUMM speech: VOC block extends past end of file",
            ));
        }
        pos = end;
    }
    // No terminator — VOCs in some games omit it for the final clip.
    Ok(pos)
}

impl Container for ScummSpeechContainer {
    fn prefix(&self) -> &str {
        &self.prefix
    }

    fn visit(&self, visitor: &mut dyn FnMut(&Entry<'_>) -> Result<bool>) -> Result<()> {
        for entry in &self.entries {
            let bytes = &self.source[entry.offset..entry.offset + entry.size];
            let e = Entry::new(&entry.path, &self.prefix, bytes);
            if !visitor(&e)? {
                break;
            }
        }
        Ok(())
    }

    fn info(&self) -> ContainerInfo {
        ContainerInfo {
            path: self.prefix.clone(),
            format: ContainerFormat::ScummSpeech,
            entry_count: Some(self.entries.len()),
        }
    }

    fn get_file(&self, path: &str) -> Option<&[u8]> {
        let lower = path.to_lowercase();
        self.path_index.get(&lower).map(|&idx| {
            let e = &self.entries[idx];
            &self.source[e.offset..e.offset + e.size]
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn voc_blob(payload: &[u8]) -> Vec<u8> {
        // Minimal valid VOC: 26-byte header + one type-1 sound block + type-0 terminator.
        let mut v = Vec::new();
        v.extend_from_slice(VOC_MAGIC);
        v.extend_from_slice(&(VOC_HEADER_SIZE as u16).to_le_bytes()); // data offset
        v.extend_from_slice(&0x010au16.to_le_bytes()); // version 1.10
        v.extend_from_slice(&0x1129u16.to_le_bytes()); // checksum (unverified here)
        // Block type 1, 24-bit LE size, then `payload.len()` bytes.
        v.push(1);
        let sz = payload.len() as u32;
        v.extend_from_slice(&sz.to_le_bytes()[..3]);
        v.extend_from_slice(payload);
        // Terminator.
        v.push(0);
        v
    }

    fn make_speech(clips: &[&[u8]]) -> Vec<u8> {
        let mut blob = Vec::new();
        blob.extend_from_slice(SOU_MAGIC);
        blob.extend_from_slice(&[0u8; 4]);
        for clip in clips {
            // VCTL header with two bytes of dummy lipsync timing.
            let lipsync = [0x0fu8, 0xff];
            let vctl_size = (8 + lipsync.len()) as u32;
            blob.extend_from_slice(VCTL_TAG);
            blob.extend_from_slice(&vctl_size.to_be_bytes());
            blob.extend_from_slice(&lipsync);
            // VOC follows immediately.
            blob.extend_from_slice(&voc_blob(clip));
        }
        blob
    }

    #[test]
    fn detects_sou_magic() {
        let blob = make_speech(&[b"hi"]);
        assert!(is_scumm_speech_file(&blob));
        assert!(!is_scumm_speech_file(b"SOU"));
        assert!(!is_scumm_speech_file(b"NOPE\x00\x00\x00\x00VCTL"));
        // SCUMM v5 SOUN sub-resources also start with "SOU " but have a
        // different sub-chunk tag in place of the four zero bytes (e.g.
        // "MIDI" or "ROL "). Make sure those do not get misidentified.
        assert!(!is_scumm_speech_file(b"SOU MIDI\x00\x00\x00\x00"));
        assert!(!is_scumm_speech_file(b"SOU ROL \x00\x00\x00\x00"));
    }

    #[test]
    fn enumerates_voice_clips() {
        let clip_a = b"audio-payload-a";
        let clip_b = b"audio-payload-b";
        let blob = make_speech(&[&clip_a[..], &clip_b[..]]);
        let c = ScummSpeechContainer::from_bytes(&blob, "monster.sou".to_string(), 32).unwrap();
        assert_eq!(c.entries.len(), 2);
        assert_eq!(c.entries[0].path, "monster.sou/speech_0000.voc");
        assert_eq!(c.entries[1].path, "monster.sou/speech_0001.voc");
        // Each entry's bytes must start with the VOC magic.
        let got_a = c.get_file("monster.sou/speech_0000.voc").unwrap();
        let got_b = c.get_file("monster.sou/speech_0001.voc").unwrap();
        assert_eq!(&got_a[0..20], VOC_MAGIC);
        assert_eq!(&got_b[0..20], VOC_MAGIC);
        // And they must be byte-distinct (different payloads).
        assert_ne!(got_a, got_b);
    }

    #[test]
    fn voc_without_terminator_runs_to_eof() {
        let mut blob = Vec::new();
        blob.extend_from_slice(SOU_MAGIC);
        blob.extend_from_slice(&[0u8; 4]);
        blob.extend_from_slice(VCTL_TAG);
        blob.extend_from_slice(&10u32.to_be_bytes());
        blob.extend_from_slice(&[0x0f, 0xff]);
        // VOC header + one sound block, no terminator.
        blob.extend_from_slice(VOC_MAGIC);
        blob.extend_from_slice(&(VOC_HEADER_SIZE as u16).to_le_bytes());
        blob.extend_from_slice(&0x010au16.to_le_bytes());
        blob.extend_from_slice(&0x1129u16.to_le_bytes());
        blob.push(1);
        blob.extend_from_slice(&[3u8, 0, 0]); // 3-byte payload follows
        blob.extend_from_slice(b"abc");

        let c = ScummSpeechContainer::from_bytes(&blob, "x.sou".to_string(), 32).unwrap();
        assert_eq!(c.entries.len(), 1);
        let got = c.get_file("x.sou/speech_0000.voc").unwrap();
        assert!(got.starts_with(VOC_MAGIC));
        assert!(got.ends_with(b"abc"));
    }
}
