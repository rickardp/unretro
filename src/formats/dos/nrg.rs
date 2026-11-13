//! Nero NRG disc images: the tracks of a CD or DVD in one file, located by a
//! chain of chunks that a footer at the end of the file points to.
//!
//! Each readable track is one entry, as for CUE/BIN: data tracks as
//! `trackNN.img` holding the 2048 user bytes of every sector, audio tracks as
//! `trackNN.pcm` holding headerless 16-bit little-endian stereo samples. See
//! `docs/nrg/nrg.md`.

use crate::compat::{String, Vec, format};
use crate::error::{Error, Result};
use crate::format::ContainerFormat;
use crate::formats::cd_sector::{CD_RAW_SECTOR_SIZE, CD_USER_DATA_SIZE, user_data_offset};
use crate::limits::{OutputBudget, checked_output_size};
use crate::{Container, ContainerInfo, Entry};

const MAX_CHAIN_SIZE: u64 = 1024 * 1024;
const MAX_CHUNKS: usize = 256;
const MAX_TRACKS: usize = 99;

const CHUNK_HEADER: usize = 8;
const DAO_HEADER: usize = 22;

const HEADERLESS_MODE2_SECTOR: usize = 2336;
// A raw sector followed by 96 bytes of sub-channel data.
const SUBCHANNEL_SECTOR: usize = 2448;

// Entries never exceed the image they are cut from.
const OUTPUT_BUDGET_RATIO: u64 = 1;

// Big-endian number of `width` (at most 8) bytes at `at`; bytes beyond the
// slice read as absent.
fn field(bytes: &[u8], at: usize, width: usize) -> u64 {
    bytes
        .iter()
        .skip(at)
        .take(width)
        .fold(0, |value, &byte| (value << 8) | u64::from(byte))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Audio,
    // `fallback` is the user-data offset in a raw sector without a sync
    // pattern and mode byte of its own.
    Data { fallback: usize },
}

impl Kind {
    const fn output_per_sector(self) -> usize {
        match self {
            Self::Audio => CD_RAW_SECTOR_SIZE,
            Self::Data { .. } => CD_USER_DATA_SIZE,
        }
    }
}

// The kind of track behind a mode code and the stored sector size the code
// implies.
const fn mode(code: u32) -> Option<(Kind, usize)> {
    const MODE1: Kind = Kind::Data { fallback: 16 };
    const MODE2: Kind = Kind::Data { fallback: 24 };
    Some(match code {
        0x00 => (MODE1, CD_USER_DATA_SIZE),
        0x02 => (MODE2, CD_USER_DATA_SIZE),
        0x03 => (MODE2, HEADERLESS_MODE2_SECTOR),
        0x05 => (MODE1, CD_RAW_SECTOR_SIZE),
        0x06 => (MODE2, CD_RAW_SECTOR_SIZE),
        0x07 | 0x08 => (Kind::Audio, CD_RAW_SECTOR_SIZE),
        0x0F => (MODE1, SUBCHANNEL_SECTOR),
        0x10 => (Kind::Audio, SUBCHANNEL_SECTOR),
        0x11 => (MODE2, SUBCHANNEL_SECTOR),
        _ => return None,
    })
}

// A track as the chain declares it. Offsets are from the start of the file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Declared {
    mode: u32,
    // The sector size field of a disc-at-once entry; a track-at-once entry
    // has none and the mode code implies the size.
    sector_size: Option<u64>,
    // Start of the pregap.
    first: u64,
    start: u64,
    // `None` when a track-at-once offset + length overflows.
    end: Option<u64>,
}

// Whether `more` further tracks still fit; counted before any is collected.
fn fits(tracks: &[Declared], more: usize) -> bool {
    tracks
        .len()
        .checked_add(more)
        .is_some_and(|total| total <= MAX_TRACKS)
}

// Entries of a `DAOI` (`width` 4) or `DAOX` (`width` 8) chunk.
fn disc_at_once(content: &[u8], width: usize, tracks: &mut Vec<Declared>) -> Option<()> {
    let entries = content.get(DAO_HEADER..).unwrap_or(&[]);
    let entries = entries.chunks_exact(18 + 3 * width);
    if !fits(tracks, entries.len()) {
        return None;
    }
    tracks.extend(entries.map(|entry| Declared {
        mode: u32::from(entry.get(14).copied().unwrap_or(0)),
        sector_size: Some(field(entry, 12, 2)),
        first: field(entry, 18, width),
        start: field(entry, 18 + width, width),
        end: Some(field(entry, 18 + 2 * width, width)),
    }));
    Some(())
}

// Entries of a `TINF`, `ETNF` (`width` 4) or `ETN2` (`width` 8) chunk.
fn track_at_once(
    content: &[u8],
    entry_size: usize,
    width: usize,
    tracks: &mut Vec<Declared>,
) -> Option<()> {
    let entries = content.chunks_exact(entry_size);
    if !fits(tracks, entries.len()) {
        return None;
    }
    tracks.extend(entries.map(|entry| {
        let offset = field(entry, 0, width);
        Declared {
            mode: u32::try_from(field(entry, 2 * width, 4)).unwrap_or(u32::MAX),
            sector_size: None,
            first: offset,
            start: offset,
            end: offset.checked_add(field(entry, width, width)),
        }
    }));
    Some(())
}

// The tracks of a chain, which is everything between the chain offset and
// the footer. `None` unless the chain reaches `END!` and declares 1 to 99
// tracks.
fn walk(chain: &[u8]) -> Option<Vec<Declared>> {
    let mut tracks = Vec::new();
    let mut rest = chain;
    for _ in 0..MAX_CHUNKS {
        let (header, after) = rest.split_at_checked(CHUNK_HEADER)?;
        let (identifier, _) = header.split_at_checked(4)?;
        if !identifier.iter().all(|byte| (0x20..=0x7E).contains(byte)) {
            return None;
        }
        // Compared with what is left before the footer, never added first.
        let length = usize::try_from(field(header, 4, 4)).ok()?;
        let (content, next) = after.split_at_checked(length)?;
        // The identifier, not the footer version, decides the layout.
        match identifier {
            b"END!" => return (!tracks.is_empty()).then_some(tracks),
            b"DAOI" => disc_at_once(content, 4, &mut tracks)?,
            b"DAOX" => disc_at_once(content, 8, &mut tracks)?,
            b"TINF" => track_at_once(content, 12, 4, &mut tracks)?,
            b"ETNF" => track_at_once(content, 20, 4, &mut tracks)?,
            b"ETN2" => track_at_once(content, 32, 8, &mut tracks)?,
            _ => {}
        }
        rest = next;
    }
    None
}

struct Image {
    // The chain offset: where the data area ends.
    data_end: u64,
    tracks: Vec<Declared>,
}

// The image behind a footer of `footer_size` bytes starting with
// `signature`, the rest of which is the chain offset.
fn with_footer(data: &[u8], signature: &[u8; 4], footer_size: usize) -> Option<Image> {
    let footer_at = data.len().checked_sub(footer_size)?;
    let (body, footer) = data.split_at_checked(footer_at)?;
    let (found, offset) = footer.split_at_checked(4)?;
    if found != signature {
        return None;
    }
    let data_end = field(offset, 0, offset.len());
    // Room for one chunk header, and no more than the largest chain.
    let chain_size = (footer_at as u64).checked_sub(data_end)?;
    if !(CHUNK_HEADER as u64..=MAX_CHAIN_SIZE).contains(&chain_size) {
        return None;
    }
    let chain = body.get(usize::try_from(data_end).ok()?..)?;
    Some(Image {
        data_end,
        tracks: walk(chain)?,
    })
}

fn parse(data: &[u8]) -> Option<Image> {
    with_footer(data, b"NER5", 12).or_else(|| with_footer(data, b"NERO", 8))
}

/// Check if data is a Nero NRG disc image.
///
/// The signature is in the footer at the end of the file; the chain of chunks
/// it points to has to be valid and declare at least one track as well.
#[must_use]
pub fn is_nrg_image(data: &[u8]) -> bool {
    parse(data).is_some()
}

// Whether no offset is below one before it, pregaps included, over all
// sessions together. Tracks of such a layout cannot overlap.
fn consistent(tracks: &[Declared]) -> bool {
    let mut previous = 0;
    tracks.iter().all(|track| {
        let Some(end) = track.end else {
            return false;
        };
        let ordered = previous <= track.first && track.first <= track.start && track.start <= end;
        previous = end;
        ordered
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Extent {
    kind: Kind,
    sector_size: usize,
    start: u64,
    sectors: u64,
}

// The sectors of a track of a consistent layout whose data area ends at
// `data_end`, or the reason it cannot be read.
fn extent(declared: &Declared, data_end: u64) -> core::result::Result<Extent, String> {
    let Some((kind, implied)) = mode(declared.mode) else {
        return Err(format!("has unknown mode code {:#04x}", declared.mode));
    };
    let sector_size = match declared.sector_size {
        None => implied,
        // The field gives the size and the mode code only the kind.
        Some(size) => {
            let allowed: &[usize] = match kind {
                Kind::Audio => &[CD_RAW_SECTOR_SIZE, SUBCHANNEL_SECTOR],
                Kind::Data { .. } => &[
                    CD_USER_DATA_SIZE,
                    HEADERLESS_MODE2_SECTOR,
                    CD_RAW_SECTOR_SIZE,
                    SUBCHANNEL_SECTOR,
                ],
            };
            allowed
                .iter()
                .copied()
                .find(|&allowed| allowed as u64 == size)
                .ok_or_else(|| format!("has unsupported sector size {size}"))?
        }
    };
    if declared.start >= data_end {
        return Err("starts beyond the data area".into());
    }
    // Ends are clamped to the data area, starts are not.
    let end = declared.end.map_or(data_end, |end| end.min(data_end));
    let sectors = end.saturating_sub(declared.start) / sector_size as u64;
    if sectors == 0 {
        return Err("is empty".into());
    }
    Ok(Extent {
        kind,
        sector_size,
        start: declared.start,
        sectors,
    })
}

// The entry data of `extent`, once it is within the limits: the user data or
// samples of each of its sectors.
fn content(
    data: &[u8],
    extent: &Extent,
    budget: &mut OutputBudget,
) -> core::result::Result<Vec<u8>, String> {
    let over_limit = || String::from("exceeds the size limit");
    let per_sector = extent.kind.output_per_sector();
    let size = extent
        .sectors
        .checked_mul(per_sector as u64)
        .and_then(|size| checked_output_size(size, "track").ok())
        .ok_or_else(over_limit)?;
    budget.charge(size, "track").map_err(|_| over_limit())?;

    let source = usize::try_from(extent.start)
        .ok()
        .zip(usize::try_from(extent.sectors).ok())
        .and_then(|(start, sectors)| {
            let end = sectors
                .checked_mul(extent.sector_size)?
                .checked_add(start)?;
            data.get(start..end)
        })
        .ok_or_else(|| String::from("starts beyond the data area"))?;
    let mut out = Vec::new();
    out.try_reserve_exact(size)
        .map_err(|_| String::from("is too large to be held in memory"))?;
    if extent.sector_size == per_sector {
        out.extend_from_slice(source);
        return Ok(out);
    }
    for sector in source.chunks_exact(extent.sector_size) {
        let offset = match extent.kind {
            Kind::Audio => 0,
            Kind::Data { .. } if extent.sector_size == HEADERLESS_MODE2_SECTOR => 8,
            Kind::Data { fallback } => user_data_offset(sector, fallback),
        };
        // Every larger sector size has room behind the largest offset.
        out.extend_from_slice(sector.get(offset..offset + per_sector).unwrap_or_default());
    }
    Ok(out)
}

// Tracks left out of the listing.
#[derive(Clone, Default)]
struct Skipped {
    count: usize,
    first: Option<String>,
}

impl Skipped {
    fn note(&mut self, message: String) {
        self.count += 1;
        self.first.get_or_insert(message);
    }

    fn into_message(self) -> Option<String> {
        let first = self.first?;
        Some(if self.count > 1 {
            format!("NRG: {first} (and {} more problems)", self.count - 1)
        } else {
            format!("NRG: {first}")
        })
    }
}

struct Track {
    number: usize,
    kind: Kind,
    // The entry data: the track's sectors from index 1 on, cooked.
    data: Vec<u8>,
}

/// Container yielding the tracks of a Nero NRG disc image.
pub struct NrgContainer {
    prefix: String,
    tracks: Vec<Track>,
    skipped: Skipped,
}

impl NrgContainer {
    /// Open an NRG image.
    ///
    /// # Errors
    ///
    /// Fails if `data` is not an NRG image or `depth` is zero.
    pub fn from_bytes(data: &[u8], prefix: String, depth: u32) -> Result<Self> {
        if depth == 0 {
            return Err(Error::MaxDepthExceeded);
        }
        let image = parse(data).ok_or_else(|| Error::invalid_format("Not an NRG image"))?;

        let mut skipped = Skipped::default();
        let mut tracks = Vec::new();
        if consistent(&image.tracks) {
            let mut budget = OutputBudget::for_input(data.len(), OUTPUT_BUDGET_RATIO);
            for (number, declared) in (1..).zip(&image.tracks) {
                let read = extent(declared, image.data_end).and_then(|extent| {
                    Ok(Track {
                        number,
                        kind: extent.kind,
                        data: content(data, &extent, &mut budget)?,
                    })
                });
                match read {
                    Ok(track) => tracks.push(track),
                    Err(problem) => skipped.note(format!("track {number} {problem}")),
                }
            }
        } else {
            // Overlapping tracks cannot both be right; none is read.
            for number in 1..=image.tracks.len() {
                skipped.note(format!("track {number} is in an inconsistent layout"));
            }
        }

        Ok(Self {
            prefix,
            tracks,
            skipped,
        })
    }
}

impl Container for NrgContainer {
    fn prefix(&self) -> &str {
        &self.prefix
    }

    fn visit(&self, visitor: &mut dyn FnMut(&Entry<'_>) -> Result<bool>) -> Result<()> {
        for track in &self.tracks {
            let extension = match track.kind {
                Kind::Audio => "pcm",
                Kind::Data { .. } => "img",
            };
            let path = format!("{}/track{:02}.{extension}", self.prefix, track.number);
            if !visitor(&Entry::new(&path, &self.prefix, &track.data))? {
                break;
            }
        }
        Ok(())
    }

    fn skipped_entries(&self) -> Option<String> {
        self.skipped.clone().into_message()
    }

    fn info(&self) -> ContainerInfo {
        ContainerInfo {
            path: self.prefix.clone(),
            format: ContainerFormat::Nrg,
            entry_count: Some(self.tracks.len()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compat::ToString;
    use crate::formats::cd_sector::CD_SYNC;

    // FIPS 180-4, for the digests the specification gives.
    fn sha256(data: &[u8]) -> String {
        let mut k = [0u32; 64];
        let mut h = [0u32; 8];
        let mut primes =
            (2u32..).filter(|n| (2..*n).take_while(|d| d * d <= *n).all(|d| n % d != 0));
        for (i, prime) in primes.by_ref().take(64).enumerate() {
            let fraction = |root: f64| ((root - root.floor()) * 4_294_967_296.0) as u32;
            k[i] = fraction(f64::from(prime).cbrt());
            if i < 8 {
                h[i] = fraction(f64::from(prime).sqrt());
            }
        }
        let mut message = data.to_vec();
        message.push(0x80);
        while message.len() % 64 != 56 {
            message.push(0);
        }
        message.extend_from_slice(&(data.len() as u64 * 8).to_be_bytes());
        for block in message.chunks_exact(64) {
            let mut w = [0u32; 64];
            for (i, word) in block.chunks_exact(4).enumerate() {
                w[i] = u32::from_be_bytes([word[0], word[1], word[2], word[3]]);
            }
            for i in 16..64 {
                let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
                let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
                w[i] = w[i - 16]
                    .wrapping_add(s0)
                    .wrapping_add(w[i - 7])
                    .wrapping_add(s1);
            }
            let mut v = h;
            for i in 0..64 {
                let s1 = v[4].rotate_right(6) ^ v[4].rotate_right(11) ^ v[4].rotate_right(25);
                let ch = (v[4] & v[5]) ^ (!v[4] & v[6]);
                let t1 = v[7]
                    .wrapping_add(s1)
                    .wrapping_add(ch)
                    .wrapping_add(k[i])
                    .wrapping_add(w[i]);
                let s0 = v[0].rotate_right(2) ^ v[0].rotate_right(13) ^ v[0].rotate_right(22);
                let maj = (v[0] & v[1]) ^ (v[0] & v[2]) ^ (v[1] & v[2]);
                v = [
                    t1.wrapping_add(s0.wrapping_add(maj)),
                    v[0],
                    v[1],
                    v[2],
                    v[3].wrapping_add(t1),
                    v[4],
                    v[5],
                    v[6],
                ];
            }
            for (state, value) in h.iter_mut().zip(v) {
                *state = state.wrapping_add(value);
            }
        }
        h.iter().map(|word| format!("{word:08x}")).collect()
    }

    fn hex(text: &str) -> Vec<u8> {
        text.split_whitespace()
            .map(|byte| u8::from_str_radix(byte, 16).unwrap())
            .collect()
    }

    fn repeated(text: &str, len: usize) -> Vec<u8> {
        text.bytes().cycle().take(len).collect()
    }

    // The building blocks of section 12.1 of the specification.
    fn data_sector(i: usize) -> Vec<u8> {
        repeated(&format!("T1-{i:06}|"), 2048)
    }

    fn audio_sector(i: usize) -> Vec<u8> {
        let mut sector = Vec::new();
        for k in 0..588usize {
            let v = ((i * 588 + k) % 30000) as i16;
            sector.extend(v.to_le_bytes());
            sector.extend((-v).to_le_bytes());
        }
        sector
    }

    const FORM1: [u8; 8] = [0, 0, 8, 0, 0, 0, 8, 0];

    fn mode2_sector(i: usize) -> Vec<u8> {
        let mut sector = if i == 7 {
            let mut sector = vec![0, 0, 0x20, 0, 0, 0, 0x20, 0];
            sector.extend(repeated("F2-000007|", 2324));
            sector
        } else {
            let mut sector = FORM1.to_vec();
            sector.extend(repeated(&format!("T3-{i:06}|"), 2048));
            sector
        };
        sector.resize(2336, 0);
        sector
    }

    fn bcd(value: i64) -> u8 {
        (value / 10 * 16 + value % 10) as u8
    }

    fn raw_sector(lba: i64, mode: u8, body: &[u8]) -> Vec<u8> {
        let address = lba + 150;
        let mut sector = CD_SYNC.to_vec();
        sector.extend([
            bcd(address / 4500),
            bcd(address / 75 % 60),
            bcd(address % 75),
            mode,
        ]);
        sector.extend_from_slice(body);
        sector.resize(2352, 0);
        sector
    }

    const SUBCHANNEL: [u8; 96] = [b'S'; 96];

    const T1: &str = "29a24186333aed9ef03ffbfd4ab452d397d634c03d967a27b30895f8575aaf3f";
    const AU: &str = "e7461c0534743f4e9f7833e84c20327dd9959436e44bcf9acf10fa5d0dac83d5";
    const T3: &str = "25a6f1093fdf46c5acac4800b2b485dfe079845a8afdf784a37150805bdd7f98";

    struct Listing {
        // (name, size, SHA-256)
        entries: Vec<(String, usize, String)>,
        skipped: String,
    }

    fn open(image: &[u8]) -> Listing {
        assert!(is_nrg_image(image));
        let container = NrgContainer::from_bytes(image, "dir/disc.nrg".into(), 8).unwrap();
        let mut entries = Vec::new();
        container
            .visit(&mut |entry| {
                assert_eq!(entry.container_path, "dir/disc.nrg");
                let name = entry.path.strip_prefix("dir/disc.nrg/").unwrap();
                entries.push((name.to_string(), entry.data.len(), sha256(entry.data)));
                Ok(true)
            })
            .unwrap();
        let info = container.info();
        assert_eq!(info.entry_count, Some(entries.len()));
        assert_eq!(info.format, ContainerFormat::Nrg);
        Listing {
            entries,
            skipped: container.skipped_entries().unwrap_or_default(),
        }
    }

    fn listed(listing: &Listing) -> Vec<(&str, &str)> {
        listing
            .entries
            .iter()
            .map(|(name, _, digest)| (name.as_str(), digest.as_str()))
            .collect()
    }

    fn rejected(image: &[u8]) {
        assert!(!is_nrg_image(image));
        assert!(NrgContainer::from_bytes(image, "x.nrg".into(), 8).is_err());
    }

    fn patched(image: &[u8], at: usize, bytes: &[u8]) -> Vec<u8> {
        let mut image = image.to_vec();
        image[at..at + bytes.len()].copy_from_slice(bytes);
        image
    }

    // Image A of section 12.2.
    fn image_a() -> Vec<u8> {
        let mut image = vec![0; 150 * 2048];
        (0..20).for_each(|i| image.extend(data_sector(i)));
        image.resize(image.len() + 10 * 2352, 0);
        (0..30).for_each(|i| image.extend(audio_sector(i)));
        for _ in 0..5 {
            image.extend(FORM1);
            image.resize(image.len() + 2328, 0);
        }
        (0..12).for_each(|i| image.extend(mode2_sector(i)));
        assert_eq!(image.len(), 481_952);
        image.extend(hex("
            43 55 45 58 00 00 00 40 41 00 00 00 ff ff ff 6a
            41 01 00 00 ff ff ff 6a 41 01 01 00 00 00 00 00
            01 02 00 00 00 00 00 14 01 02 01 00 00 00 00 1e
            41 03 00 00 00 00 00 3c 41 03 01 00 00 00 00 41
            41 aa 01 00 00 00 00 4d 44 41 4f 58 00 00 00 94
            00 00 00 94 00 00 00 00 00 00 00 00 00 00 00 00
            00 00 20 01 01 03 00 00 00 00 00 00 00 00 00 00
            00 00 08 00 00 00 00 01 00 00 00 00 00 00 00 00
            00 00 00 00 00 04 b0 00 00 00 00 00 00 05 50 00
            00 00 00 00 00 00 00 00 00 00 00 00 09 30 07 00
            00 01 00 00 00 00 00 05 50 00 00 00 00 00 00 05
            ab e0 00 00 00 00 00 06 bf 80 00 00 00 00 00 00
            00 00 00 00 00 00 09 20 03 00 00 01 00 00 00 00
            00 06 bf 80 00 00 00 00 00 06 ed 20 00 00 00 00
            00 07 5a a0 53 49 4e 46 00 00 00 04 00 00 00 03
            4d 54 59 50 00 00 00 04 00 00 00 01 45 4e 44 21
            00 00 00 00 4e 45 52 35 00 00 00 00 00 07 5a a0
        "));
        image
    }

    // Field positions in image A.
    const A_DAOX: usize = 0x75AE8;
    const A_TRACK1_SIZE: usize = 0x75B12;
    const A_TRACK1_MODE: usize = 0x75B14;
    const A_TRACK1_INDEX1: usize = 0x75B20;
    const A_TRACK2_MODE: usize = 0x75B3E;
    const A_TRACK2_INDEX0: usize = 0x75B42;
    const A_TRACK3_SIZE: usize = 0x75B66;
    const A_TRACK3_MODE: usize = 0x75B68;
    const A_TRACK3_INDEX1: usize = 0x75B74;
    const A_TRACK3_END: usize = 0x75B7C;
    const A_SINF_LENGTH: usize = 0x75B88;
    const A_END: usize = 0x75B9C;
    const A_FOOTER_OFFSET: usize = 0x75BA8;

    const A_ENTRIES: [(&str, &str); 3] = [
        ("track01.img", T1),
        ("track02.pcm", AU),
        ("track03.img", T3),
    ];

    // Image B of section 12.3.
    fn image_b() -> Vec<u8> {
        let mut image = Vec::new();
        (0..20).for_each(|i| image.extend(data_sector(i)));
        (0..12).for_each(|i| image.extend(mode2_sector(i)));
        (0..30).for_each(|i| image.extend(audio_sector(i)));
        assert_eq!(image.len(), 139_552);
        image.extend(hex("
            45 54 4e 46 00 00 00 3c 00 00 00 00 00 00 a0 00
            00 00 00 00 00 00 00 00 00 00 00 00 00 00 a0 00
            00 00 6d 80 00 00 00 03 00 00 00 14 00 00 00 00
            00 01 0d 80 00 01 13 a0 00 00 00 07 00 00 00 20
            00 00 00 00 53 49 4e 46 00 00 00 04 00 00 00 03
            45 4e 44 21 00 00 00 00 4e 45 52 4f 00 02 21 20
        "));
        image
    }

    // Image C of section 12.4.
    fn image_c() -> Vec<u8> {
        let mut image = Vec::new();
        for lba in [-2, -1] {
            image.extend(raw_sector(lba, 1, &[]));
        }
        for i in 0..20 {
            image.extend(raw_sector(i as i64, 1, &data_sector(i)));
        }
        image.resize(image.len() + 1000, 0xEE);
        for i in 0..30 {
            image.extend(audio_sector(i));
            image.extend(SUBCHANNEL);
        }
        for i in 0..12 {
            image.extend(raw_sector(11_300 + i as i64, 2, &mode2_sector(i)));
            image.extend(SUBCHANNEL);
        }
        for i in 0..20 {
            image.extend(raw_sector(11_312 + i as i64, 1, &data_sector(i)));
            image.extend(SUBCHANNEL);
        }
        for i in 0..12 {
            image.extend(raw_sector(11_332 + i as i64, 2, &mode2_sector(i)));
        }
        assert_eq!(image.len(), 232_744);
        image.extend(hex("
            43 55 45 58 00 00 00 30 41 00 00 00 ff ff ff fe
            41 01 00 00 ff ff ff fe 41 01 01 00 00 00 00 00
            01 02 00 00 00 00 00 14 01 02 01 00 00 00 00 14
            01 aa 01 00 00 00 00 32 44 41 4f 58 00 00 00 6a
            00 00 00 6a 00 00 00 00 00 00 00 00 00 00 00 00
            00 00 00 00 01 02 00 00 00 00 00 00 00 00 00 00
            00 00 09 30 05 00 00 01 00 00 00 00 00 00 00 00
            00 00 00 00 00 00 12 60 00 00 00 00 00 00 ca 20
            00 00 00 00 00 00 00 00 00 00 00 00 09 90 10 00
            00 01 00 00 00 00 00 00 ce 08 00 00 00 00 00 00
            ce 08 00 00 00 00 00 01 ec e8 5a 5a 5a 5a 00 00
            00 0d 75 6e 6b 6e 6f 77 6e 20 63 68 75 6e 6b 43
            55 45 58 00 00 00 40 41 00 00 00 00 00 2c 24 41
            03 00 00 00 00 2c 24 41 03 01 00 00 00 2c 24 41
            04 00 00 00 00 2c 30 41 04 01 00 00 00 2c 30 41
            05 00 00 00 00 2c 44 41 05 01 00 00 00 2c 44 41
            aa 01 00 00 00 2c 50 44 41 4f 58 00 00 00 94 00
            00 00 94 00 00 00 00 00 00 00 00 00 00 00 00 00
            00 20 01 03 05 00 00 00 00 00 00 00 00 00 00 00
            00 09 90 11 00 00 01 00 00 00 00 00 01 ec e8 00
            00 00 00 00 01 ec e8 00 00 00 00 00 02 5f a8 00
            00 00 00 00 00 00 00 00 00 00 00 09 90 0f 00 00
            01 00 00 00 00 00 02 5f a8 00 00 00 00 00 02 5f
            a8 00 00 00 00 00 03 1e e8 00 00 00 00 00 00 00
            00 00 00 00 00 09 30 06 00 00 01 00 00 00 00 00
            03 1e e8 00 00 00 00 00 03 1e e8 00 00 00 00 00
            03 8d 28 53 49 4e 46 00 00 00 04 00 00 00 02 53
            49 4e 46 00 00 00 04 00 00 00 03 4d 54 59 50 00
            00 00 04 00 00 00 01 45 4e 44 21 00 00 00 00 4e
            45 52 35 00 00 00 00 00 03 8d 28
        "));
        image
    }

    // Image D of section 12.5.
    fn image_d() -> Vec<u8> {
        let mut image = vec![0; 150 * 2048];
        (0..20).for_each(|i| image.extend(data_sector(i)));
        (0..30).for_each(|i| image.extend(audio_sector(i)));
        assert_eq!(image.len(), 418_720);
        image.extend(hex("
            43 55 45 53 00 00 00 30 41 00 00 00 00 00 00 00
            41 01 00 00 00 00 00 00 41 01 01 00 00 00 02 00
            01 02 00 00 00 00 02 20 01 02 01 00 00 00 02 20
            01 aa 01 00 00 00 02 50 44 41 4f 49 00 00 00 52
            00 00 00 52 00 00 00 00 00 00 00 00 00 00 00 00
            00 00 00 01 01 02 00 00 00 00 00 00 00 00 00 00
            00 00 08 00 00 00 00 01 00 00 00 00 00 04 b0 00
            00 05 50 00 00 00 00 00 00 00 00 00 00 00 00 00
            09 30 07 00 00 01 00 05 50 00 00 05 50 00 00 06
            63 a0 53 49 4e 46 00 00 00 04 00 00 00 02 45 4e
            44 21 00 00 00 00 4e 45 52 4f 00 06 63 a0
        "));
        image
    }

    // Builders for chains made in the tests.
    fn chunk(identifier: &[u8; 4], content: &[u8]) -> Vec<u8> {
        let mut chunk = identifier.to_vec();
        chunk.extend((content.len() as u32).to_be_bytes());
        chunk.extend_from_slice(content);
        chunk
    }

    fn be(value: u64, width: usize) -> Vec<u8> {
        value.to_be_bytes()[8 - width..].to_vec()
    }

    // (sector size, mode, index 0, index 1, end)
    type DaoEntry = (u16, u8, u64, u64, u64);

    fn dao(width: usize, entries: &[DaoEntry]) -> Vec<u8> {
        // Header fields a reader must not rely on.
        let mut content = vec![0xFF; DAO_HEADER];
        for &(size, mode, index0, index1, end) in entries {
            content.extend([b'I'; 12]);
            content.extend(size.to_be_bytes());
            content.extend([mode, 0, 0, 1]);
            for offset in [index0, index1, end] {
                content.extend(be(offset, width));
            }
        }
        chunk(if width == 8 { b"DAOX" } else { b"DAOI" }, &content)
    }

    // (offset, length, mode)
    type TaoEntry = (u64, u64, u32);

    fn tao(identifier: &[u8; 4], entries: &[TaoEntry]) -> Vec<u8> {
        let (entry_size, width) = match identifier {
            b"ETN2" => (32, 8),
            b"ETNF" => (20, 4),
            _ => (12, 4),
        };
        let mut content = Vec::new();
        for &(offset, length, mode) in entries {
            let at = content.len();
            content.extend(be(offset, width));
            content.extend(be(length, width));
            content.extend(mode.to_be_bytes());
            // LBA, zero and sector count: not to be used.
            content.resize(at + entry_size, 0xFF);
        }
        chunk(identifier, &content)
    }

    fn end() -> Vec<u8> {
        chunk(b"END!", &[])
    }

    fn footer(version: u8, chain_offset: u64) -> Vec<u8> {
        if version == 2 {
            [b"NER5".to_vec(), be(chain_offset, 8)].concat()
        } else {
            [b"NERO".to_vec(), be(chain_offset, 4)].concat()
        }
    }

    fn assemble(data_area: &[u8], chunks: &[Vec<u8>], version: u8) -> Vec<u8> {
        let mut image = data_area.to_vec();
        chunks.iter().for_each(|chunk| image.extend(chunk));
        image.extend(footer(version, data_area.len() as u64));
        image
    }

    #[test]
    fn sha256_helper_matches_known_digest() {
        assert_eq!(
            sha256(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn image_a_version_2_disc_at_once() {
        let image = image_a();
        assert_eq!(image.len(), 482_224);
        assert_eq!(
            sha256(&image),
            "bb401e8fec56a9a4556a5bf165d7f75c70fe177ffb358ad71add20b73e12db5f"
        );
        let parsed = parse(&image).unwrap();
        assert_eq!(parsed.data_end, 481_952);
        let declared = |sector_size, mode, first, start, end| Declared {
            mode,
            sector_size: Some(sector_size),
            first,
            start,
            end: Some(end),
        };
        assert_eq!(
            parsed.tracks,
            [
                declared(2048, 0x00, 0, 307_200, 348_160),
                declared(2352, 0x07, 348_160, 371_680, 442_240),
                declared(2336, 0x03, 442_240, 453_920, 481_952),
            ]
        );
        let listing = open(&image);
        assert_eq!(listed(&listing), A_ENTRIES);
        assert_eq!(
            listing.entries.iter().map(|e| e.1).collect::<Vec<_>>(),
            [40_960, 70_560, 24_576]
        );
        assert_eq!(listing.skipped, "");
    }

    #[test]
    fn image_a_variants_that_are_not_images() {
        let image = image_a();
        // 1: chain offsets at the file size, at zero and beyond everything.
        for offset in [482_224, 0, u64::MAX] {
            rejected(&patched(&image, A_FOOTER_OFFSET, &offset.to_be_bytes()));
        }
        // 2: the first identifier would hold a zero byte.
        rejected(&patched(&image, A_FOOTER_OFFSET, &481_953u64.to_be_bytes()));
        // 3: no `END!` before the footer.
        rejected(&patched(&image, A_END, b"XXXX"));
        // 4: a chunk reaching beyond the chain.
        rejected(&patched(&image, A_SINF_LENGTH, &[0xFF; 4]));
        // 5: a byte less or more.
        rejected(&image[..image.len() - 1]);
        rejected(&[image.clone(), vec![0]].concat());
        // 6: the version 1 footer would be the last 8 bytes.
        rejected(&patched(&image, image.len() - 12, b"NERO"));
    }

    #[test]
    fn image_a_chunk_limit() {
        let image = image_a();
        let with_junk = |count: usize| {
            let mut image = image.clone();
            let junk = chunk(b"JUNK", &[]).repeat(count);
            image.splice(A_END..A_END, junk);
            image
        };
        // 7: 256 chunks with `END!`.
        assert_eq!(listed(&open(&with_junk(251))), A_ENTRIES);
        // 8: one more.
        rejected(&with_junk(252));
    }

    #[test]
    fn image_a_variants_with_unreadable_tracks() {
        let image = image_a();
        // 9: unknown mode.
        let listing = open(&patched(&image, A_TRACK2_MODE, &[0x09]));
        assert_eq!(listed(&listing), [A_ENTRIES[0], A_ENTRIES[2]]);
        assert_eq!(listing.skipped, "NRG: track 2 has unknown mode code 0x09");
        // 10: sector size zero.
        let listing = open(&patched(&image, A_TRACK3_SIZE, &[0, 0]));
        assert_eq!(listed(&listing), A_ENTRIES[..2]);
        assert_eq!(
            listing.skipped,
            "NRG: track 3 has unsupported sector size 0"
        );
        // 11: an audio track of 2048-byte sectors.
        let listing = open(&patched(&image, A_TRACK1_MODE, &[0x07]));
        assert_eq!(listed(&listing), A_ENTRIES[1..]);
        assert_eq!(
            listing.skipped,
            "NRG: track 1 has unsupported sector size 2048"
        );
        // 16: a start beyond the data area.
        let beyond = 524_288u64.to_be_bytes();
        let listing = open(&patched(
            &patched(&image, A_TRACK3_INDEX1, &beyond),
            A_TRACK3_END,
            &beyond,
        ));
        assert_eq!(listed(&listing), A_ENTRIES[..2]);
        assert_eq!(listing.skipped, "NRG: track 3 starts beyond the data area");
    }

    #[test]
    fn image_a_variants_with_other_sizes() {
        let image = image_a();
        // 12: the sector size field decides, not the mode code.
        let listing = open(&patched(&image, A_TRACK3_MODE, &[0x02]));
        assert_eq!(listed(&listing), A_ENTRIES);
        assert_eq!(listing.skipped, "");
        // 13: 2352-byte sectors without sync pattern under mode 00.
        let listing = open(&patched(&image, A_TRACK1_SIZE, &[0x09, 0x30]));
        assert_eq!(
            listing.entries[0],
            (
                "track01.img".to_string(),
                34_816,
                "3b176a3cd893cfd37db51dad5b4a32bdee848d44ab1c8127cf137f489e29a7f7".to_string()
            )
        );
        assert_eq!(listed(&listing)[1..], A_ENTRIES[1..]);
        // 14: the end is clamped to the chain offset.
        let listing = open(&patched(
            &image,
            A_TRACK3_END,
            &0x7FFF_FFFF_FFFF_FFFFu64.to_be_bytes(),
        ));
        assert_eq!(listed(&listing), A_ENTRIES);
        assert_eq!(listing.skipped, "");
        // 15: a trailing partial sector is ignored.
        let listing = open(&patched(&image, A_TRACK3_END, &481_951u64.to_be_bytes()));
        assert_eq!(listed(&listing)[..2], A_ENTRIES[..2]);
        assert_eq!(
            listing.entries[2],
            (
                "track03.img".to_string(),
                22_528,
                "ee7514fbca6ee143c047819498f85611b1a9c97258ee135d621f37c4846ef38a".to_string()
            )
        );
    }

    #[test]
    fn image_a_variants_with_inconsistent_layouts() {
        let image = image_a();
        let three = "NRG: track 1 is in an inconsistent layout (and 2 more problems)";
        // 17: a pregap starting inside the track before.
        let listing = open(&patched(&image, A_TRACK2_INDEX0, &348_159u64.to_be_bytes()));
        assert!(listing.entries.is_empty());
        assert_eq!(listing.skipped, three);
        // 18: index 1 beyond the track's end.
        let listing = open(&patched(&image, A_TRACK1_INDEX1, &348_161u64.to_be_bytes()));
        assert!(listing.entries.is_empty());
        assert_eq!(listing.skipped, three);
        // 19: the same tracks declared twice.
        let mut twice = image.clone();
        let daox = image[A_DAOX..A_DAOX + 156].to_vec();
        twice.splice(A_DAOX + 156..A_DAOX + 156, daox);
        let listing = open(&twice);
        assert!(listing.entries.is_empty());
        assert_eq!(
            listing.skipped,
            "NRG: track 1 is in an inconsistent layout (and 5 more problems)"
        );
    }

    #[test]
    fn image_b_version_1_track_at_once() {
        let image = image_b();
        assert_eq!(image.len(), 139_648);
        assert_eq!(
            sha256(&image),
            "d45ed70fb4bcd3d7c8cc5bc82cc7168fc9422e06d4a77e00922283cfde5fa7cc"
        );
        let parsed = parse(&image).unwrap();
        assert_eq!(parsed.data_end, 139_552);
        let declared = |mode, offset, length| Declared {
            mode,
            sector_size: None,
            first: offset,
            start: offset,
            end: Some(offset + length),
        };
        assert_eq!(
            parsed.tracks,
            [
                declared(0x00, 0, 40_960),
                declared(0x03, 40_960, 28_032),
                declared(0x07, 68_992, 70_560),
            ]
        );
        let listing = open(&image);
        assert_eq!(
            listed(&listing),
            [
                ("track01.img", T1),
                ("track02.img", T3),
                ("track03.pcm", AU)
            ]
        );
        assert_eq!(listing.skipped, "");
    }

    #[test]
    fn image_c_two_sessions_raw_and_subchannel() {
        let image = image_c();
        assert_eq!(image.len(), 233_219);
        assert_eq!(
            sha256(&image),
            "ac97914aca3f08f591301e5c34ead7a709f30029f340255984dc049a54765b94"
        );
        let parsed = parse(&image).unwrap();
        assert_eq!(parsed.data_end, 232_744);
        assert_eq!(
            parsed
                .tracks
                .iter()
                .map(|t| (
                    t.sector_size.unwrap(),
                    t.mode,
                    t.first,
                    t.start,
                    t.end.unwrap()
                ))
                .collect::<Vec<_>>(),
            [
                (2352, 0x05, 0, 4_704, 51_744),
                (2448, 0x10, 52_744, 52_744, 126_184),
                (2448, 0x11, 126_184, 126_184, 155_560),
                (2448, 0x0F, 155_560, 155_560, 204_520),
                (2352, 0x06, 204_520, 204_520, 232_744),
            ]
        );
        let expected = [
            ("track01.img", T1),
            ("track02.pcm", AU),
            ("track03.img", T3),
            ("track04.img", T1),
            ("track05.img", T3),
        ];
        let listing = open(&image);
        assert_eq!(listed(&listing), expected);
        assert_eq!(listing.skipped, "");

        // Wrong mode codes are harmless: each sector's own mode byte decides.
        let position = |needle: &[u8]| {
            let chain = &image[232_744..];
            232_744
                + chain
                    .windows(needle.len())
                    .position(|w| w == needle)
                    .unwrap()
        };
        let mut wrong = image.clone();
        wrong[position(&[0x09, 0x30, 0x05]) + 2] = 0x02;
        wrong[position(&[0x09, 0x90, 0x0F]) + 2] = 0x02;
        wrong[position(&[0x09, 0x30, 0x06]) + 2] = 0x00;
        assert_ne!(wrong, image);
        assert_eq!(listed(&open(&wrong)), expected);
    }

    #[test]
    fn image_d_version_1_disc_at_once() {
        let image = image_d();
        assert_eq!(image.len(), 418_894);
        assert_eq!(
            sha256(&image),
            "06fdc93f479547a17089f247c8144e11b67fee343fefe1fae6dc22369ecc46c7"
        );
        let parsed = parse(&image).unwrap();
        assert_eq!(parsed.data_end, 418_720);
        assert_eq!(
            parsed
                .tracks
                .iter()
                .map(|t| (
                    t.sector_size.unwrap(),
                    t.mode,
                    t.first,
                    t.start,
                    t.end.unwrap()
                ))
                .collect::<Vec<_>>(),
            [
                (2048, 0x00, 0, 307_200, 348_160),
                (2352, 0x07, 348_160, 348_160, 418_720),
            ]
        );
        let listing = open(&image);
        assert_eq!(listed(&listing), [("track01.img", T1), ("track02.pcm", AU)]);
    }

    #[test]
    fn worked_example_smallest_chain() {
        // Section 11.1, without the footer.
        let chain = hex("
            45 54 4e 32 00 00 00 20 00 00 00 00 00 00 00 00
            00 00 00 00 03 2f a8 00 00 00 00 00 00 00 00 00
            00 00 00 00 00 00 65 f5 45 4e 44 21 00 00 00 00
        ");
        let tracks = walk(&chain).unwrap();
        assert_eq!(tracks.len(), 1);
        assert_eq!(
            extent(&tracks[0], 53_454_848),
            Ok(Extent {
                kind: Kind::Data { fallback: 16 },
                sector_size: 2048,
                start: 0,
                sectors: 26_101,
            })
        );
    }

    #[test]
    fn worked_example_version_1_video_cd() {
        // Section 11.2: the chain of `videocd.nrg` behind a blank data area.
        let mut image = vec![0; 2_630_336];
        image.extend(hex("
            45 54 4e 46 00 00 00 64 00 00 00 00 00 18 18 80
            00 00 00 03 00 00 00 00 00 00 00 00 00 18 18 80
            00 02 ac 60 00 00 00 03 00 00 02 a4 00 00 00 00
            00 1a c4 e0 00 02 ac 60 00 00 00 03 00 00 02 ef
            00 00 00 00 00 1d 71 40 00 02 ac 60 00 00 00 03
            00 00 03 3a 00 00 00 00 00 20 1d a0 00 08 05 20
            00 00 00 03 00 00 03 85 00 00 00 00 53 49 4e 46
            00 00 00 04 00 00 00 05 45 4e 44 21 00 00 00 00
            4e 45 52 4f 00 28 22 c0
        "));
        assert_eq!(image.len(), 2_630_472);
        let listing = open(&image);
        assert_eq!(
            listing
                .entries
                .iter()
                .map(|(name, size, _)| (name.as_str(), *size))
                .collect::<Vec<_>>(),
            [
                ("track01.img", 1_384_448),
                ("track02.img", 153_600),
                ("track03.img", 153_600),
                ("track04.img", 153_600),
                ("track05.img", 460_800),
            ]
        );
        assert_eq!(listing.skipped, "");
    }

    #[test]
    fn worked_example_disc_at_once_data_track() {
        // Section 11.3, without the footer.
        let chain = hex("
            43 55 45 58 00 00 00 20 41 00 00 00 ff ff ff 6a
            41 01 00 00 ff ff ff 6a 41 01 01 00 00 00 00 00
            41 aa 01 00 00 00 23 6a 44 41 4f 58 00 00 00 40
            00 00 00 40 00 00 00 00 00 00 00 00 00 00 00 00
            00 00 00 01 01 01 00 00 00 00 00 00 00 00 00 00
            00 00 08 00 00 00 00 01 00 00 00 00 00 00 00 00
            00 00 00 00 00 04 b0 00 00 00 00 00 01 20 00 00
            45 4e 44 21 00 00 00 00
        ");
        let tracks = walk(&chain).unwrap();
        assert_eq!(
            tracks,
            [Declared {
                mode: 0,
                sector_size: Some(2048),
                first: 0,
                start: 307_200,
                end: Some(18_874_368),
            }]
        );
        let extent = extent(&tracks[0], 18_874_368).unwrap();
        assert_eq!((extent.start, extent.sectors), (307_200, 9_066));
    }

    #[test]
    fn worked_example_two_data_sessions() {
        // Section 11.4, without the footer.
        let chain = hex("
            43 55 45 58 00 00 00 20 41 00 00 00 ff ff ff 6a
            41 01 00 00 ff ff ff 6a 41 01 01 00 00 00 00 00
            41 aa 01 00 00 01 0b 66 44 41 4f 58 00 00 00 40
            00 00 00 40 00 00 00 00 00 00 00 00 00 00 00 00
            00 00 20 00 01 01 00 00 00 00 00 00 00 00 00 00
            00 00 08 00 00 00 00 01 00 00 00 00 00 00 00 00
            00 00 00 00 00 04 b0 00 00 00 00 00 08 5f e0 00
            43 55 45 58 00 00 00 20 41 00 00 00 00 01 37 58
            41 02 00 00 00 01 37 58 41 02 01 00 00 01 37 ee
            41 aa 01 00 00 05 2b 59 44 41 4f 58 00 00 00 40
            00 00 00 40 00 00 00 00 00 00 00 00 00 00 00 00
            00 00 20 01 02 02 00 00 00 00 00 00 00 00 00 00
            00 00 08 00 00 00 00 01 00 00 00 00 08 5f e0 00
            00 00 00 00 08 64 90 00 00 00 00 00 27 ff e8 00
            53 49 4e 46 00 00 00 04 00 00 00 01 53 49 4e 46
            00 00 00 04 00 00 00 01 4d 54 59 50 00 00 00 04
            00 00 00 01 45 4e 44 21 00 00 00 00
        ");
        let tracks = walk(&chain).unwrap();
        assert!(consistent(&tracks));
        let extents: Vec<_> = tracks
            .iter()
            .map(|track| extent(track, 671_082_496).unwrap())
            .map(|extent| (extent.start, extent.sectors))
            .collect();
        assert_eq!(extents, [(307_200, 68_454), (140_808_192, 258_923)]);
    }

    #[test]
    fn gaps_between_tracks_are_consistent() {
        // Section 11.5.
        let declared = |first, start, end| Declared {
            mode: 7,
            sector_size: Some(2352),
            first,
            start,
            end: Some(end),
        };
        let mut tracks = vec![
            declared(0, 307_200, 92_735_488),
            declared(93_395_488, 93_395_488, 152_550_640),
            declared(152_903_440, 152_903_440, 218_921_728),
        ];
        assert!(consistent(&tracks));
        tracks[2].first = 152_550_639;
        assert!(!consistent(&tracks));
        tracks[2].first = 152_903_440;
        tracks[1].end = None;
        assert!(!consistent(&tracks));
    }

    // A stored sector of `size` bytes whose entry content is `data_sector(i)`
    // or `audio_sector(i)`.
    fn stored_sector(code: u32, size: usize, i: usize, sync: bool) -> Vec<u8> {
        let (kind, _) = mode(code).unwrap();
        let mut sector = match (kind, size) {
            (Kind::Audio, _) => audio_sector(i),
            (_, 2048) => data_sector(i),
            (_, 2336) => [FORM1.to_vec(), data_sector(i)].concat(),
            (Kind::Data { fallback: 16 }, _) => raw_sector(i as i64, 1, &data_sector(i)),
            _ => raw_sector(i as i64, 2, &[FORM1.to_vec(), data_sector(i)].concat()),
        };
        if !sync && kind != Kind::Audio && size >= 2352 {
            sector[..16].fill(0);
        }
        sector.resize(size, b'S');
        sector
    }

    const MODES: [(u32, usize); 10] = [
        (0x00, 2048),
        (0x02, 2048),
        (0x03, 2336),
        (0x05, 2352),
        (0x06, 2352),
        (0x07, 2352),
        (0x08, 2352),
        (0x0F, 2448),
        (0x10, 2448),
        (0x11, 2448),
    ];

    fn expected(code: u32) -> (String, String) {
        let audio = matches!(code, 0x07 | 0x08 | 0x10);
        let content: Vec<u8> = (0..4)
            .flat_map(|i| {
                if audio {
                    audio_sector(i)
                } else {
                    data_sector(i)
                }
            })
            .collect();
        let name = if audio { "track01.pcm" } else { "track01.img" };
        (name.to_string(), sha256(&content))
    }

    #[test]
    fn every_mode_in_track_at_once_chunks() {
        for (round, (code, size)) in MODES.into_iter().enumerate() {
            for sync in [true, false] {
                let area: Vec<u8> = (0..4)
                    .flat_map(|i| stored_sector(code, size, i, sync))
                    .collect();
                let identifier = [b"ETN2", b"ETNF", b"TINF"][round % 3];
                let version = if round % 2 == 0 { 2 } else { 1 };
                let image = assemble(
                    &area,
                    &[tao(identifier, &[(0, area.len() as u64, code)]), end()],
                    version,
                );
                let listing = open(&image);
                let (name, digest) = expected(code);
                assert_eq!(listed(&listing), [(name.as_str(), digest.as_str())]);
                assert_eq!(listing.skipped, "");
            }
        }
    }

    #[test]
    fn every_mode_in_disc_at_once_chunks() {
        for (round, (code, size)) in MODES.into_iter().enumerate() {
            let pregap = 3 * size;
            let mut area = vec![0xAB; pregap];
            (0..4).for_each(|i| area.extend(stored_sector(code, size, i, true)));
            let width = if round % 2 == 0 { 8 } else { 4 };
            let entry = (size as u16, code as u8, 0, pregap as u64, area.len() as u64);
            // The identifier decides the layout, whatever the footer version.
            for version in [1, 2] {
                let image = assemble(&area, &[dao(width, &[entry]), end()], version);
                let listing = open(&image);
                let (name, digest) = expected(code);
                assert_eq!(listed(&listing), [(name.as_str(), digest.as_str())]);
                assert_eq!(listing.skipped, "");
            }
        }
    }

    #[test]
    fn sector_size_field_limits() {
        let area = vec![0; 8 * 2448];
        let len = area.len() as u64;
        let open_with = |size: u16, code: u8| {
            open(&assemble(
                &area,
                &[dao(8, &[(size, code, 0, 0, len)]), end()],
                2,
            ))
        };
        for size in [0, 1, 2047, 2049, 2324, 2353, 2449, u16::MAX] {
            let listing = open_with(size, 0x00);
            assert!(listing.entries.is_empty());
            assert_eq!(
                listing.skipped,
                format!("NRG: track 1 has unsupported sector size {size}")
            );
        }
        for code in [0x07, 0x08, 0x10] {
            for size in [2048, 2336] {
                assert!(open_with(size, code).entries.is_empty());
            }
            for size in [2352, 2448] {
                let listing = open_with(size, code);
                assert_eq!(listing.entries[0].0, "track01.pcm");
                assert_eq!(listing.entries[0].1, 8 * 2448 / usize::from(size) * 2352);
            }
        }
        // Raw sectors under a cooked mode code.
        for code in [0x00, 0x02, 0x03] {
            let listing = open_with(2352, code);
            assert_eq!(listing.entries[0].1, 8 * 2448 / 2352 * 2048);
        }
    }

    #[test]
    fn unknown_modes_are_reported_and_later_tracks_found() {
        let area: Vec<u8> = (0..6).flat_map(data_sector).collect();
        for code in [
            0x01,
            0x04,
            0x09,
            0x20,
            0x100,
            0x0100_0000,
            0x0700_0007,
            u32::MAX,
        ] {
            let image = assemble(
                &area,
                &[tao(b"ETN2", &[(0, 4096, code), (4096, 8192, 0)]), end()],
                2,
            );
            let listing = open(&image);
            assert_eq!(listing.entries.len(), 1);
            assert_eq!(listing.entries[0].0, "track02.img");
            assert_eq!(listing.entries[0].1, 8192);
            assert_eq!(
                listing.skipped,
                format!("NRG: track 1 has unknown mode code {code:#04x}")
            );
        }
    }

    #[test]
    fn empty_and_out_of_range_tracks() {
        let area: Vec<u8> = (0..4).flat_map(data_sector).collect();
        let len = area.len() as u64;
        let image = assemble(
            &area,
            &[
                dao(
                    8,
                    &[
                        // Less than one sector.
                        (2048, 0, 0, 0, 2047),
                        (2048, 0, 2048, 2048, 4096),
                        // No sectors at all.
                        (2352, 7, 4096, 4096, 4096),
                        // Starts where the data area ends.
                        (2048, 0, 4096, len, len + 4096),
                        (2048, 0, len + 4096, len + 8192, u64::MAX),
                    ],
                ),
                end(),
            ],
            2,
        );
        let listing = open(&image);
        assert_eq!(
            listing.entries,
            [("track02.img".to_string(), 2048, sha256(&data_sector(1)))]
        );
        assert_eq!(
            listing.skipped,
            "NRG: track 1 is empty (and 3 more problems)"
        );
    }

    #[test]
    fn overflowing_track_at_once_length_is_inconsistent() {
        let area = vec![0; 4096];
        let image = assemble(
            &area,
            &[
                tao(b"ETN2", &[(0, 2048, 0), (2048, u64::MAX - 100, 0)]),
                end(),
            ],
            2,
        );
        let listing = open(&image);
        assert!(listing.entries.is_empty());
        assert_eq!(
            listing.skipped,
            "NRG: track 1 is in an inconsistent layout (and 1 more problems)"
        );
        // Tracks that go backwards across two chunks.
        let image = assemble(
            &area,
            &[
                tao(b"ETNF", &[(2048, 2048, 0)]),
                dao(4, &[(2048, 0, 0, 0, 2048)]),
                end(),
            ],
            1,
        );
        assert!(open(&image).entries.is_empty());
    }

    #[test]
    fn tracks_are_numbered_across_chunks_of_any_kind() {
        let area: Vec<u8> = (0..5).flat_map(data_sector).collect();
        let image = assemble(
            &area,
            &[
                tao(b"TINF", &[(0, 2048, 0)]),
                chunk(b"SINF", &[0xFF; 4]),
                dao(8, &[(2048, 2, 2048, 2048, 4096)]),
                chunk(b"odd ", &[1, 2, 3]),
                tao(b"ETN2", &[(4096, 2048, 0), (6144, 2048, 0)]),
                dao(4, &[(2048, 0, 8192, 8192, 10_240)]),
                end(),
            ],
            2,
        );
        let listing = open(&image);
        let names: Vec<String> = (1..=5).map(|n| format!("track{n:02}.img")).collect();
        for (i, entry) in listing.entries.iter().enumerate() {
            assert_eq!(entry.0, names[i]);
            assert_eq!(entry.2, sha256(&data_sector(i)));
        }
        assert_eq!(listing.entries.len(), 5);
    }

    #[test]
    fn track_count_limits() {
        let area = vec![0; 100 * 2048];
        let entries =
            |count: u64| -> Vec<TaoEntry> { (0..count).map(|i| (i * 2048, 2048, 0)).collect() };
        let listing = open(&assemble(&area, &[tao(b"ETN2", &entries(99)), end()], 2));
        assert_eq!(listing.entries.len(), 99);
        assert_eq!(listing.entries[98].0, "track99.img");
        rejected(&assemble(&area, &[tao(b"ETN2", &entries(100)), end()], 2));
        // Over the limit only together.
        rejected(&assemble(
            &area,
            &[
                tao(b"ETN2", &entries(50)),
                tao(b"ETNF", &entries(50)),
                end(),
            ],
            2,
        ));

        // No track chunk, an empty one, and ones without a whole entry.
        rejected(&assemble(&area, &[end()], 2));
        rejected(&assemble(&area, &[chunk(b"MTYP", &[0, 0, 0, 1]), end()], 2));
        rejected(&assemble(&area, &[chunk(b"ETN2", &[]), end()], 2));
        rejected(&assemble(&area, &[chunk(b"ETN2", &[0; 31]), end()], 2));
        rejected(&assemble(&area, &[chunk(b"DAOX", &[0; 21]), end()], 2));
        rejected(&assemble(&area, &[chunk(b"DAOX", &[0; 22 + 41]), end()], 2));
        rejected(&assemble(&area, &[chunk(b"DAOI", &[0; 22 + 29]), end()], 1));

        // An incomplete trailing entry is ignored.
        let mut partial = tao(b"ETN2", &entries(2));
        partial.extend([0; 31]);
        partial[4..8].copy_from_slice(&(64u32 + 31).to_be_bytes());
        let listing = open(&assemble(&area, &[partial, end()], 2));
        assert_eq!(listing.entries.len(), 2);
    }

    #[test]
    fn footer_and_chain_bounds() {
        let area = data_sector(0);
        let track = tao(b"ETN2", &[(0, 2048, 0)]);
        let good = assemble(&area, &[track.clone(), end()], 2);
        assert_eq!(open(&good).entries.len(), 1);

        // Identifiers are printable ASCII.
        for byte in [0x00, 0x1F, 0x7F, 0xFF] {
            let odd = chunk(&[b'A', byte, b'C', b'D'], &[]);
            rejected(&assemble(&area, &[track.clone(), odd, end()], 2));
        }
        let odd = chunk(b" ~~ ", &[]);
        assert_eq!(
            open(&assemble(&area, &[track.clone(), odd, end()], 2))
                .entries
                .len(),
            1
        );

        // The length of `END!` is bounds-checked like any other.
        let mut long_end = good.clone();
        let at = good.len() - 12 - 4;
        long_end[at..at + 4].copy_from_slice(&1u32.to_be_bytes());
        rejected(&long_end);

        // A chain of exactly 1 MiB, and one byte more.
        let filler = |len: usize| chunk(b"FILL", &vec![0; len]);
        let rest = track.len() + 8 + 8;
        let largest = assemble(
            &area,
            &[track.clone(), filler(1024 * 1024 - rest), end()],
            2,
        );
        assert_eq!(open(&largest).entries.len(), 1);
        rejected(&assemble(
            &area,
            &[track.clone(), filler(1024 * 1024 - rest + 1), end()],
            2,
        ));

        // No room for a chunk header before the footer.
        for cut in 0..8 {
            let mut image = area.clone();
            image.extend(&end()[..cut]);
            image.extend(footer(2, area.len() as u64));
            rejected(&image);
        }

        // Files shorter than any footer, and bare footers.
        for len in 0..40 {
            rejected(&good[good.len() - len..]);
            rejected(&good[..len]);
        }
        rejected(&footer(2, 0));
        rejected(&footer(1, 0));
        rejected(&[end(), footer(2, 0)].concat());
        rejected(&[end(), footer(1, 0)].concat());

        // A version 1 chain offset is not sign-extended or wrapped.
        let v1 = assemble(&area, &[tao(b"ETNF", &[(0, 2048, 0)]), end()], 1);
        assert_eq!(open(&v1).entries.len(), 1);
        rejected(&patched(&v1, v1.len() - 4, &[0xFF; 4]));
    }

    #[test]
    fn version_1_is_tried_after_a_failed_version_2() {
        // The last 12 bytes start with `NER5`, the last 8 with `NERO`.
        let area = data_sector(3);
        let mut image = area.clone();
        image.extend(tao(b"ETNF", &[(0, 2048, 0)]));
        image.extend(end());
        image.extend(b"NER5");
        image.extend(footer(1, area.len() as u64));
        let listing = open(&image);
        assert_eq!(
            listing.entries,
            [("track01.img".to_string(), 2048, sha256(&data_sector(3)))]
        );
    }

    #[test]
    fn oversized_tracks_are_not_read() {
        let huge = Declared {
            mode: 0,
            sector_size: Some(2048),
            first: 0,
            start: 0,
            end: Some(1 << 40),
        };
        let extent = extent(&huge, 1 << 40).unwrap();
        assert_eq!(extent.sectors, 1 << 29);
        let mut budget = OutputBudget::for_input(usize::MAX, 1);
        assert_eq!(
            content(&[0; 4096], &extent, &mut budget),
            Err("exceeds the size limit".to_string())
        );

        // Within the limit for one entry but not within the budget.
        let data = vec![0; 4096];
        let small = Extent {
            kind: Kind::Data { fallback: 16 },
            sector_size: 2048,
            start: 0,
            sectors: 2,
        };
        let mut budget = OutputBudget::for_input(0, 1);
        let floor = budget.remaining();
        assert_eq!(content(&data, &small, &mut budget).unwrap().len(), 4096);
        assert_eq!(budget.remaining(), floor - 4096);
        budget
            .charge(usize::try_from(floor - 4096 - 4095).unwrap(), "test")
            .unwrap();
        assert_eq!(
            content(&data, &small, &mut budget),
            Err("exceeds the size limit".to_string())
        );
        // An extent that is not in the data is not read.
        let mut budget = OutputBudget::for_input(0, 1);
        let outside = Extent {
            start: 2049,
            ..small
        };
        assert!(content(&data, &outside, &mut budget).is_err());
    }

    // Only the entries are held: what a track keeps is what the budget was
    // charged for, whatever its stored sector size.
    #[test]
    fn tracks_are_held_cooked() {
        let data = vec![0; 2 * 2448];
        for (kind, sector_size, held) in [
            (Kind::Data { fallback: 16 }, 2448, 4096),
            (Kind::Data { fallback: 24 }, 2336, 4096),
            (Kind::Audio, 2448, 4704),
            (Kind::Audio, 2352, 4704),
        ] {
            let extent = Extent {
                kind,
                sector_size,
                start: 0,
                sectors: 2,
            };
            let mut budget = OutputBudget::for_input(0, 1);
            let floor = budget.remaining();
            let track = content(&data, &extent, &mut budget).unwrap();
            assert_eq!(track.len(), held);
            assert_eq!(track.capacity(), held);
            assert_eq!(floor - budget.remaining(), held as u64);
        }
    }

    #[test]
    fn visit_stops_early_and_depth_is_limited() {
        let image = image_b();
        assert!(matches!(
            NrgContainer::from_bytes(&image, "x".into(), 0),
            Err(Error::MaxDepthExceeded)
        ));
        let container = NrgContainer::from_bytes(&image, "x".into(), 1).unwrap();
        let mut seen = 0;
        container
            .visit(&mut |_| {
                seen += 1;
                Ok(seen < 2)
            })
            .unwrap();
        assert_eq!(seen, 2);
        let failed = container.visit(&mut |_| Err(Error::invalid_format("stop")));
        assert!(failed.is_err());
    }

    #[test]
    fn hostile_chains_do_not_panic() {
        let mut state = 0x9E37_79B9_7F4A_7C15u64;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        let interesting = [
            0u64,
            1,
            2047,
            2048,
            2336,
            2352,
            2448,
            0x7FFF_FFFF,
            0xFFFF_FFFF,
            0x1_0000_0000,
            u64::MAX / 2,
            u64::MAX - 1,
            u64::MAX,
        ];
        for (image, data_end) in [(image_c(), 232_744usize), (image_b(), 139_552)] {
            let mut accepted = 0;
            for round in 0..3000 {
                let mut bytes = image.clone();
                for _ in 0..=(next() % 4) {
                    let at = data_end + (next() % (bytes.len() - data_end) as u64) as usize;
                    if round % 3 == 0 {
                        let value = interesting[(next() % interesting.len() as u64) as usize]
                            .wrapping_add(next() % 3);
                        let width = [1, 2, 4, 8][(next() % 4) as usize].min(bytes.len() - at);
                        bytes[at..at + width].copy_from_slice(&be(value, width));
                    } else {
                        bytes[at] = next() as u8;
                    }
                }
                if round % 7 == 0 {
                    bytes.truncate(bytes.len() - (next() % 16) as usize);
                }
                let recognised = is_nrg_image(&bytes);
                let opened = NrgContainer::from_bytes(&bytes, "dir/x.nrg".into(), 4);
                assert_eq!(recognised, opened.is_ok());
                let Ok(container) = opened else {
                    continue;
                };
                accepted += 1;
                let mut total = 0;
                container
                    .visit(&mut |entry| {
                        let name = entry.path.strip_prefix("dir/x.nrg/track").unwrap();
                        assert!(name.len() == 6 && !name.contains('/'), "{name}");
                        total += entry.data.len();
                        Ok(true)
                    })
                    .unwrap();
                // No amplification: tracks never overlap, and nothing but
                // the entries is held.
                let held: usize = container.tracks.iter().map(|t| t.data.len()).sum();
                assert!(total == held && held <= bytes.len(), "{total} {held}");
                let _ = container.skipped_entries();
            }
            assert!(accepted > 100, "{accepted}");
        }
    }
}
