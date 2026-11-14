//! `Metadata::modified` of committed fixtures, against the times that
//! independent tools report for them (`tar tvf`, `7z l -slt` with `TZ=UTC`).
// The helpers have no caller in a build with a single format family.
#![allow(dead_code, unused_imports)]

use std::collections::BTreeMap;
use std::path::PathBuf;

use unretro::{EntryType, Loader, Timestamp, VisitAction};

// Modification times by path below the fixture, which is named from the
// `testdata` directory.
fn times(fixture: &str) -> BTreeMap<String, Option<Timestamp>> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("testdata")
        .join(fixture);
    let root = format!("{}/", path.display());
    let mut seen = BTreeMap::new();
    Loader::from_path(&path)
        .visit(EntryType::Leaves, |entry| {
            let inside = entry.path.strip_prefix(&root).unwrap_or(entry.path);
            let modified = entry.metadata.and_then(|metadata| metadata.modified);
            seen.insert(inside.to_string(), modified);
            Ok(VisitAction::Continue)
        })
        .unwrap();
    assert!(!seen.is_empty(), "{fixture} lists nothing");
    seen
}

fn at(year: i64, month: u32, day: u32, hour: u32, minute: u32, second: u32) -> i64 {
    Timestamp::civil_seconds(year, month, day, hour, minute, second).unwrap()
}

#[cfg(feature = "common")]
#[test]
fn tar_times_are_utc() {
    let seen = times("compress/files.tar.Z");
    let want = Some(Timestamp::utc(at(2000, 1, 1, 0, 0, 0)));
    assert_eq!(seen["files.tar/readme.txt"], want);
    assert_eq!(seen["files.tar/empty.dat"], want);
}

#[cfg(feature = "amiga")]
#[test]
fn lha_times_are_local_from_the_dos_field_and_utc_otherwise() {
    // Level 0 without extended area: the MS-DOS field, two-second units.
    let dos = times("lha/hdr-l0-dos.lha");
    assert_eq!(
        dos["DOCS/GUIDE.TXT"],
        Some(Timestamp::local(at(1994, 3, 14, 9, 26, 52)))
    );
    // Level 0 with a Unix area, and level 2: Unix time, one second past what
    // the two-second MS-DOS field of the same header can say.
    let unix = times("lha/hdr-l0-unix.lha");
    assert_eq!(
        unix["docs/guide.txt"],
        Some(Timestamp::utc(at(1994, 3, 14, 9, 26, 53)))
    );
    let level2 = times("lha/hdr-l2.lha");
    assert_eq!(
        level2["docs/guide.txt"],
        Some(Timestamp::utc(at(1994, 3, 14, 9, 26, 53)))
    );
}

#[cfg(all(feature = "dos", feature = "common"))]
#[test]
fn iso_9660_times_have_their_gmt_offset_applied() {
    let seen = times("iso/et-floppy.iso.gz");
    assert_eq!(
        seen["et-floppy.iso/Read Me.txt"],
        Some(Timestamp::utc(at(2026, 11, 13, 7, 14, 21)))
    );
    // Boot images have no directory record of their own.
    let boot = seen
        .iter()
        .find(|(path, _)| path.contains("[BOOT]") || path.contains("boot-"))
        .map(|(_, modified)| *modified);
    assert!(boot.is_none_or(|modified| modified.is_none()), "{seen:?}");
}

// Written on a Unix host, which stores Unix time where MS-DOS hosts store a
// packed date: read as the latter, as 7-Zip does, it says 2033-07-22 01:37:48.
#[cfg(feature = "dos")]
#[test]
fn arj_times_from_a_unix_host_are_utc() {
    let seen = times("arj/m1.arj");
    assert_eq!(
        seen["text.txt"],
        Some(Timestamp::utc(at(2026, 11, 12, 19, 0, 8)))
    );
}

#[cfg(all(feature = "next", feature = "common"))]
#[test]
fn every_ufs_and_bfs_file_has_a_utc_time() {
    let mut fixtures = vec!["ufs/makefs-be-4k.img.gz", "ufs/netbsd-44-le-8k.img.gz"];
    if cfg!(feature = "beos") {
        fixtures.push("bfs/tiny.img.gz");
    }
    for fixture in fixtures {
        for (path, modified) in times(fixture) {
            let modified = modified.unwrap_or_else(|| panic!("{fixture}: {path} has no time"));
            assert!(!modified.local, "{fixture}: {path}");
            // Built between 2000 and 2100.
            assert!(
                (at(2000, 1, 1, 0, 0, 0)..at(2100, 1, 1, 0, 0, 0)).contains(&modified.seconds),
                "{fixture}: {path} {modified:?}"
            );
        }
    }
}

#[cfg(feature = "macintosh")]
#[test]
fn hfs_plus_times_are_utc_and_hard_links_share_their_targets() {
    let seen = times("hfsplus/spread.dmg");
    let link = seen["disk image/After/hard link.txt"].expect("link time");
    assert!(!link.local);
    assert_eq!(seen["disk image/After/late.txt"], Some(link));
    assert!((at(2020, 1, 1, 0, 0, 0)..at(2100, 1, 1, 0, 0, 0)).contains(&link.seconds));
}
