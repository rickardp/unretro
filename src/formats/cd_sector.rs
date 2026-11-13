// Raw 2352-byte CD-ROM sectors (MODE1 and MODE2/XA), shared by the HFS and
// ISO 9660 readers, and the XA framings without sync and header.

use crate::compat::Vec;

pub(crate) const CD_RAW_SECTOR_SIZE: usize = 2352;
pub(crate) const CD_USER_DATA_SIZE: usize = 2048;

#[derive(Clone, Copy)]
pub(crate) struct RawCd<'a> {
    data: &'a [u8],
}

impl<'a> RawCd<'a> {
    pub(crate) fn detect(data: &'a [u8]) -> Option<Self> {
        if data.len() < CD_RAW_SECTOR_SIZE * 2 || data.len() % CD_RAW_SECTOR_SIZE != 0 {
            return None;
        }

        let sectors_to_probe = (data.len() / CD_RAW_SECTOR_SIZE).min(16);
        let valid_headers = (0..sectors_to_probe)
            .filter(|&sector| {
                let offset = sector * CD_RAW_SECTOR_SIZE;
                data[offset] == 0x00
                    && data[offset + 1..offset + 11].iter().all(|&b| b == 0xFF)
                    && data[offset + 11] == 0x00
                    && matches!(data[offset + 15], 0x01 | 0x02)
            })
            .count();

        if valid_headers < sectors_to_probe.saturating_sub(1).max(1) {
            return None;
        }

        Some(Self { data })
    }

    pub(crate) fn sector_count(&self) -> usize {
        self.data.len() / CD_RAW_SECTOR_SIZE
    }

    pub(crate) fn user_data(&self, sector: usize) -> &'a [u8] {
        let raw = &self.data[sector * CD_RAW_SECTOR_SIZE..][..CD_RAW_SECTOR_SIZE];
        // Mode 2 (XA) sectors have an 8-byte subheader after the 16-byte
        // sync and header; Form 1 then carries the same 2048 bytes as Mode 1.
        let offset = if raw[15] == 0x02 { 24 } else { 16 };
        &raw[offset..offset + CD_USER_DATA_SIZE]
    }
}

#[cfg(feature = "dos")]
pub(crate) const CD_SYNC: [u8; 12] = [
    0, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0,
];

// Where the user data of a raw sector starts: decided by the sector's own
// mode byte when it has a sync pattern, `fallback` otherwise.
#[cfg(feature = "dos")]
pub(crate) fn user_data_offset(raw: &[u8], fallback: usize) -> usize {
    match (raw.get(..12) == Some(&CD_SYNC[..]), raw.get(15)) {
        (true, Some(1)) => 16,
        (true, Some(2)) => 24,
        _ => fallback,
    }
}

// CD-ROM XA sectors stored without sync and header: an 8-byte sub-header,
// 2048 bytes of user data and, in the 2336-byte kind, 280 bytes more
// (docs/toast/README.md, section 6). The smaller size is tried first.
pub(crate) const XA_SECTOR_SIZES: [usize; 2] = [2056, 2336];

const XA_SUBHEADER_SIZE: usize = 8;
// Through the sector that holds an ISO 9660 volume descriptor.
const XA_PROBE_SECTORS: usize = 17;

// Whether `data` can be whole sectors of `sector_size` bytes: each of the
// leading sectors repeats its four sub-header bytes. Nothing else marks the
// framing, so the caller must also find a volume in the cooked view.
pub(crate) fn has_xa_subheaders(data: &[u8], sector_size: usize) -> bool {
    data.len() >= XA_PROBE_SECTORS * sector_size
        && data.len() % sector_size == 0
        && data
            .chunks_exact(sector_size)
            .take(XA_PROBE_SECTORS)
            .all(|sector| sector[..4] == sector[4..XA_SUBHEADER_SIZE])
}

// Enough leading sectors to tell whether a volume is there: through the UDF
// anchor at sector 256.
pub(crate) const XA_HEAD_SECTORS: usize = 320;

// The user data of every sector. A Form 2 sector gives the first 2048 of
// its 2324 bytes. `None` when the copy is over the output limit or cannot
// be allocated.
pub(crate) fn cook_xa(data: &[u8], sector_size: usize) -> Option<Vec<u8>> {
    let size = (data.len() / sector_size) as u64 * CD_USER_DATA_SIZE as u64;
    let mut cooked = Vec::new();
    cooked
        .try_reserve_exact(crate::limits::checked_output_size(size, "XA image").ok()?)
        .ok()?;
    for sector in data.chunks_exact(sector_size) {
        cooked.extend_from_slice(&sector[XA_SUBHEADER_SIZE..][..CD_USER_DATA_SIZE]);
    }
    Some(cooked)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compat::vec;

    fn framed(sectors: usize, sector_size: usize) -> Vec<u8> {
        let mut data = vec![0u8; sectors * sector_size];
        for (number, sector) in data.chunks_exact_mut(sector_size).enumerate() {
            sector[..8].copy_from_slice(&[0, 0, 8, 0, 0, 0, 8, 0]);
            sector[8..2056].fill((number as u8).wrapping_add(1));
            sector[2056..].fill(0xEE);
        }
        data
    }

    #[test]
    fn xa_sectors_are_cooked_from_offset_eight() {
        for size in XA_SECTOR_SIZES {
            let data = framed(20, size);
            assert!(has_xa_subheaders(&data, size));
            let cooked = cook_xa(&data, size).unwrap();
            assert_eq!(cooked.len(), 20 * CD_USER_DATA_SIZE);
            for (number, sector) in cooked.chunks_exact(CD_USER_DATA_SIZE).enumerate() {
                assert!(sector.iter().all(|&byte| byte == number as u8 + 1));
            }
        }
    }

    #[test]
    fn xa_framing_needs_seventeen_whole_sectors_with_repeated_sub_headers() {
        for size in XA_SECTOR_SIZES {
            assert!(!has_xa_subheaders(&framed(16, size), size));
            assert!(has_xa_subheaders(&framed(17, size), size));
            let mut data = framed(20, size);
            data.push(0);
            assert!(!has_xa_subheaders(&data, size));
            data.pop();
            // Sector 16 is checked, sector 17 is not.
            data[16 * size + 6] ^= 1;
            assert!(!has_xa_subheaders(&data, size));
            data[16 * size + 6] ^= 1;
            data[17 * size + 6] ^= 1;
            assert!(has_xa_subheaders(&data, size));
        }
        assert!(!has_xa_subheaders(&[], 2056));
    }
}
