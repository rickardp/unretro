// Hybrid discs: a Macintosh volume or partition map and an ISO 9660 volume
// in one image (docs/toast/README.md, section 5.3). The ISO 9660 volume is
// one more partition entry, opened by content like the others.

use core::sync::atomic::{AtomicUsize, Ordering};

use crate::compat::{Box, Cow, String, Vec, format};
use crate::error::Result;
use crate::format::ContainerFormat;
use crate::formats::cd_sector::{CD_USER_DATA_SIZE, RawCd};
use crate::formats::dos::iso9660::{Iso9660Container, has_readable_root, is_iso9660_image};
use crate::{Container, ContainerInfo, Entry};

// Beside the partitions of a map.
pub(crate) const ISO_PARTITION: &str = "iso9660.img";
// At the root of a volume without a map, where file names are not ours.
pub(crate) const ISO_PARTITION_IN_VOLUME: &str = "..iso9660.img";

// Sectors 0 to 15, which ISO 9660 leaves to the system: the partition map
// and the Macintosh volume header are there.
const SYSTEM_AREA: usize = 16 * CD_USER_DATA_SIZE;

struct HybridContainer {
    primary: Box<dyn Container>,
    name: &'static str,
    // The image without its system area, so that it opens as ISO 9660.
    iso: Vec<u8>,
    prefix: String,
    // Entries of `primary` the last visit left out for their name.
    shadowed: AtomicUsize,
}

// `primary` as opened from `data`, with the ISO 9660 volume of `data` added
// as an entry when it has a readable root directory.
pub(crate) fn with_iso_side(
    primary: Box<dyn Container>,
    data: &[u8],
    depth: u32,
) -> Box<dyn Container> {
    if !(is_iso9660_image(data) && has_readable_root(data)) {
        return primary;
    }
    let raw = RawCd::detect(data);
    let cooked_len = raw.map_or(data.len(), |cd| cd.sector_count() * CD_USER_DATA_SIZE);
    let mut iso = Vec::new();
    if cooked_len <= SYSTEM_AREA || iso.try_reserve_exact(cooked_len).is_err() {
        return primary;
    }
    iso.resize(SYSTEM_AREA, 0);
    match raw {
        Some(cd) => {
            for sector in SYSTEM_AREA / CD_USER_DATA_SIZE..cd.sector_count() {
                iso.extend_from_slice(cd.user_data(sector));
            }
        }
        None => iso.extend_from_slice(&data[SYSTEM_AREA..]),
    }
    // Not presentable: a volume that needs its system area, or lists no file.
    let lists_files = Iso9660Container::from_bytes(&iso, String::new(), depth)
        .is_ok_and(|volume| volume.info().entry_count != Some(0));
    if !lists_files {
        return primary;
    }
    let name = if primary.info().format == ContainerFormat::Apm {
        ISO_PARTITION
    } else {
        ISO_PARTITION_IN_VOLUME
    };
    let prefix = String::from(primary.prefix());
    Box::new(HybridContainer {
        primary,
        name,
        iso,
        prefix,
        shadowed: AtomicUsize::new(0),
    })
}

impl HybridContainer {
    // `path` is the reserved name or lies below it.
    fn is_reserved(&self, path: &str) -> bool {
        let len = self.name.len();
        path.get(..len)
            .is_some_and(|head| head.eq_ignore_ascii_case(self.name))
            && path
                .get(len..)
                .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
    }

    fn is_iso_entry(&self, path: &str) -> bool {
        path.eq_ignore_ascii_case(self.name)
    }
}

impl Container for HybridContainer {
    fn prefix(&self) -> &str {
        &self.prefix
    }

    fn visit(&self, visitor: &mut dyn FnMut(&Entry<'_>) -> Result<bool>) -> Result<()> {
        let mut stopped = false;
        let mut shadowed = 0;
        let visited = self.primary.visit(&mut |entry| {
            let below = entry.path.strip_prefix(self.prefix.as_str());
            if below
                .and_then(|path| path.strip_prefix('/'))
                .is_some_and(|path| self.is_reserved(path))
            {
                shadowed += 1;
                return Ok(true);
            }
            let more = visitor(entry)?;
            stopped = !more;
            Ok(more)
        });
        self.shadowed.store(shadowed, Ordering::Relaxed);
        visited?;
        if stopped {
            return Ok(());
        }
        let path = format!("{}/{}", self.prefix, self.name);
        visitor(&Entry::new(&path, &self.prefix, &self.iso)).map(|_| ())
    }

    fn skipped_entries(&self) -> Option<String> {
        let shadowed = match self.shadowed.load(Ordering::Relaxed) {
            0 => None,
            count => Some(format!(
                "{count} Macintosh entries at the reserved name {} are not listed",
                self.name
            )),
        };
        [self.primary.skipped_entries(), shadowed]
            .into_iter()
            .flatten()
            .reduce(|all, next| format!("{all}; {next}"))
    }

    fn info(&self) -> ContainerInfo {
        let mut info = self.primary.info();
        info.entry_count = info.entry_count.map(|count| count + 1);
        info
    }

    fn get_file(&self, path: &str) -> Option<&[u8]> {
        if self.is_iso_entry(path) {
            Some(&self.iso)
        } else if self.is_reserved(path) {
            None
        } else {
            self.primary.get_file(path)
        }
    }

    fn read_file(&self, path: &str) -> Option<Cow<'_, [u8]>> {
        if self.is_reserved(path) {
            self.get_file(path).map(Cow::Borrowed)
        } else {
            self.primary.read_file(path)
        }
    }

    fn has_resource_fork(&self, path: &str) -> bool {
        !self.is_reserved(path) && self.primary.has_resource_fork(path)
    }

    fn has_file(&self, path: &str) -> bool {
        if self.is_reserved(path) {
            self.is_iso_entry(path)
        } else {
            self.primary.has_file(path)
        }
    }

    fn has_dir(&self, path: &str) -> bool {
        !self.is_reserved(path) && self.primary.has_dir(path)
    }
}

#[cfg(all(test, feature = "std", feature = "common"))]
mod tests {
    use super::*;
    use crate::compat::{ToString, Vec};
    use crate::format::ContainerFormat;
    use crate::formats::macintosh::apm::ApmContainer;
    use std::io::Read;

    fn fixture(name: &str) -> Vec<u8> {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("testdata/toast")
            .join(name);
        let mut data = Vec::new();
        flate2::read::GzDecoder::new(std::fs::File::open(path).unwrap())
            .read_to_end(&mut data)
            .unwrap();
        data
    }

    fn open(data: &[u8]) -> Box<dyn Container> {
        let primary = ApmContainer::from_bytes(data, "disc".to_string(), 8, false).unwrap();
        with_iso_side(Box::new(primary), data, 8)
    }

    fn paths(container: &dyn Container) -> Vec<String> {
        let mut paths = Vec::new();
        container
            .visit(&mut |entry| {
                assert_eq!(entry.container_path, "disc");
                paths.push(entry.path.to_string());
                Ok(true)
            })
            .unwrap();
        paths
    }

    #[test]
    fn iso_volume_is_listed_after_the_partitions() {
        let data = fixture("hybrid-concat.toast.gz");
        let container = open(&data);
        assert_eq!(
            paths(container.as_ref()),
            ["disc/p1.img", "disc/iso9660.img"]
        );
        let info = container.info();
        assert_eq!(info.format, ContainerFormat::Apm);
        assert_eq!(info.entry_count, Some(2));
        assert_eq!(container.prefix(), "disc");

        let iso = container.get_file("ISO9660.IMG").unwrap();
        assert_eq!(iso.len(), data.len());
        assert!(iso[..SYSTEM_AREA].iter().all(|&b| b == 0));
        assert_eq!(iso[SYSTEM_AREA..], data[SYSTEM_AREA..]);
        assert_eq!(
            crate::detect_format("iso9660.img", Some(iso)),
            Some(ContainerFormat::Iso9660)
        );
        assert!(container.has_file("iso9660.img"));
        assert!(!container.has_file("iso9660.img/README.TXT"));
        assert_eq!(container.get_file("P1.IMG").unwrap().len(), 819_200);
    }

    // A Macintosh volume with entries of its own at the reserved name.
    struct Shadowing;

    impl Container for Shadowing {
        fn prefix(&self) -> &str {
            "disc"
        }

        fn visit(&self, visitor: &mut dyn FnMut(&Entry<'_>) -> Result<bool>) -> Result<()> {
            for path in [
                "disc/..iso9660.img/README.TXT",
                "disc/..ISO9660.IMG",
                "disc/..iso9660.imgx",
                "disc/iso9660.img",
                "disc/kept.txt",
            ] {
                if !visitor(&Entry::new(path, "disc", b"mac"))? {
                    break;
                }
            }
            Ok(())
        }

        fn info(&self) -> ContainerInfo {
            ContainerInfo {
                path: "disc".to_string(),
                format: ContainerFormat::Hfs,
                entry_count: None,
            }
        }

        fn get_file(&self, _path: &str) -> Option<&[u8]> {
            Some(b"mac")
        }
    }

    #[test]
    fn reserved_name_in_a_volume_belongs_to_the_iso_side_alone() {
        let data = fixture("hybrid-concat.toast.gz");
        let container = with_iso_side(Box::new(Shadowing), &data, 8);
        assert_eq!(container.skipped_entries(), None);
        assert_eq!(
            paths(container.as_ref()),
            [
                "disc/..iso9660.imgx",
                "disc/iso9660.img",
                "disc/kept.txt",
                "disc/..iso9660.img",
            ]
        );
        assert_eq!(
            container.skipped_entries().unwrap(),
            "2 Macintosh entries at the reserved name ..iso9660.img are not listed"
        );
        assert_eq!(
            container.get_file("..iso9660.img").unwrap().len(),
            data.len()
        );
        assert!(container.get_file("..iso9660.img/README.TXT").is_none());
        assert!(!container.has_file("..ISO9660.IMG/other"));
        assert_eq!(container.get_file("kept.txt").unwrap(), b"mac");
        assert_eq!(container.get_file("iso9660.img").unwrap(), b"mac");
    }

    #[test]
    fn visitor_can_stop_on_either_side() {
        let container = open(&fixture("hybrid-concat.toast.gz"));
        for stop_after in 1..=2 {
            let mut seen = 0;
            container
                .visit(&mut |_| {
                    seen += 1;
                    Ok(seen < stop_after)
                })
                .unwrap();
            assert_eq!(seen, stop_after);
        }
    }

    #[test]
    fn image_without_an_iso_volume_is_left_as_it_is() {
        for name in ["apm-hfs.toast.gz", "apm-map2048.toast.gz"] {
            let container = open(&fixture(name));
            let listed = paths(container.as_ref());
            assert_eq!(listed.len(), 1, "{name}");
            assert_ne!(listed[0], "disc/iso9660.img", "{name}");
            assert_eq!(container.info().entry_count, Some(1), "{name}");
        }
    }
}
