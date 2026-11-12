// Raw 2352-byte CD-ROM sectors (MODE1 and MODE2/XA), shared by the HFS and
// ISO 9660 readers.

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
