# Usage Guide

This guide documents current usage for the Rust library, CLI, and Python bindings.

See also:

- [Architecture](ARCHITECTURE.md)
- [Contributing](../CONTRIBUTING.md)
- [Root README](../README.md)
- [Python README](../crates/unretro-python/README.md)

## Rust Library Usage

### Create a Loader

#### Filesystem path

```rust,no_run
use unretro::{EntryType, Loader, VisitAction};

# fn main() -> Result<(), unretro::Error> {
Loader::from_path("archive.zip").visit(EntryType::Leaves, |entry| {
    println!("{}", entry.path);
    Ok(VisitAction::Continue)
})?;
# Ok(())
# }
```

#### Virtual path (path inside archive)

```rust,no_run
use unretro::{EntryType, Loader, VisitAction};

# fn main() -> Result<(), unretro::Error> {
Loader::from_virtual_path("archive.zip/folder/file.txt").visit(EntryType::Leaves, |entry| {
    println!("{}", entry.path);
    Ok(VisitAction::Continue)
})?;
# Ok(())
# }
```

`from_virtual_path` probes filesystem path prefixes to find the real archive file, then applies an internal prefix filter.

#### In-memory bytes

```rust
use unretro::{EntryType, Loader, VisitAction};

# fn main() -> Result<(), unretro::Error> {
let data = std::fs::read("archive.tar")?;
Loader::from_bytes(data, "archive.tar").visit(EntryType::Leaves, |entry| {
    println!("{}", entry.path);
    Ok(VisitAction::Continue)
})?;
# Ok(())
# }
```

### Configure Traversal

#### Max recursion depth

```rust,no_run
# use unretro::{EntryType, Loader, VisitAction};
# fn main() -> Result<(), unretro::Error> {
Loader::from_path("nested.sit.hqx")
    .with_max_depth(8)
    .visit(EntryType::Leaves, |_entry| Ok(VisitAction::Continue))?;
# Ok(())
# }
```

Depth is not the only bound on nesting. All entries that the containers of one input
yield, at every level together, are counted against a budget of 64 times the input size
(at least 8 GiB), and every nested container opened costs another 4 KiB. Once the budget
is spent no further nested container is opened: such entries are still visited as
container entries, their contents are not, and the report carries one
`NestedContainerOpenFailed` diagnostic naming the first one refused. This stops inputs
whose entries all point at the same bytes from multiplying at every level. Each file
below a directory source has a budget of its own.

#### Memory mapping strategy

```rust,no_run
# use unretro::{EntryType, Loader, MmapStrategy, VisitAction};
# fn main() -> Result<(), unretro::Error> {
Loader::from_path("large.img")
    .with_mmap(MmapStrategy::Always)
    .visit(EntryType::Leaves, |_entry| Ok(VisitAction::Continue))?;
# Ok(())
# }
```

### EntryType and VisitAction Semantics

`EntryType` controls which entries trigger your callback:

- `EntryType::Leaves`: only non-container entries.
- `EntryType::Containers`: only detected containers.
- `EntryType::All`: both containers and leaves.

`VisitAction` controls recursion:

- `VisitAction::Continue`: continue traversal and recurse into nested containers.
- `VisitAction::Handled`: treat this entry as handled and skip recursion for it.

Use `Handled` when you intentionally process container payload yourself and do not want nested descent.

### Inspect Container Info Without Visiting

```rust,no_run
# use unretro::Loader;
# fn main() -> Result<(), unretro::Error> {
let info = Loader::from_path("archive.zip").info()?;
println!("{} {:?}", info.path, info.format);
# Ok(())
# }
```

### Best-Effort Diagnostics (`visit_with_report`)

```rust,no_run
use unretro::{EntryType, Loader, VisitAction};

# fn main() -> Result<(), unretro::Error> {
let report = Loader::from_path("archive.zip").visit_with_report(EntryType::Leaves, |_entry| {
    Ok(VisitAction::Continue)
})?;

if report.has_root_failures() {
    eprintln!("Root traversal failed");
}

if report.has_recoverable_diagnostics() {
    for d in &report.diagnostics {
        eprintln!("{:?}: {} ({})", d.code, d.message, d.path);
    }
}
# Ok(())
# }
```

Root-level failures and recoverable nested issues are separated in the report.

### Path and Data Lifetime Caveats

- `Entry::data` is borrowed and only valid during callback execution.
- Copy entry bytes if needed after callback returns.
- `path` is full virtual path; `container_path` is owning container path; `relative_path()` is path inside that container.

## Classic Mac Resource Forks

Every resource is yielded as `<owner>/..namedfork/rsrc/<TYPE>/<name>` (or `<TYPE>/#<id>`
with `with_numeric_identifiers(true)` / `--numeric`). `<TYPE>` is the 4-character code,
or `0xXXXXXXXX` when it is not printable ASCII. `<owner>` depends on where the fork lives:

| Source | `<owner>` | Example resource path |
|---|---|---|
| MacBinary / AppleSingle / archive / HFS file `app` | the file | `app.sit/app/..namedfork/rsrc/CODE/#1` |
| AppleDouble sidecar (`._app`, `__MACOSX/<dir>/._app`) next to `app`, in an archive or a plain directory | the data file `app` | `dir/app/..namedfork/rsrc/CODE/#1` |
| Standalone AppleDouble with a valid resource fork (`app.rsrc` from `unar -k visible`, or a `._app` with no companion) | the AppleDouble file itself | `dir/app.rsrc/..namedfork/rsrc/CODE/#1` |
| Bare resource-fork file (`app.rsrc`) | the fork file itself | `dir/app.rsrc/..namedfork/rsrc/CODE/#1` |
| Native macOS fork of `app` | the file | `app/..namedfork/rsrc/CODE/#1` |

Notes:

- A paired sidecar is hidden; its companion gets the sidecar's type/creator in `metadata`
  and its resource fork. A sidecar is paired only when traversing the archive or
  directory that contains both files; opening the data file on its own does not pick up
  its sidecar.
- A sidecar fork with no resources (the 286-byte placeholder macOS writes on volumes
  without native forks) is not a fork: the companion gets type/creator only, and no
  `..namedfork/rsrc` entry.
- A sidecar's resources sit one nesting level below the `..namedfork/rsrc` entry, as for
  every other fork, so they need the same `max_depth` headroom.
- A standalone AppleDouble yields the container entry `<file>/..namedfork/rsrc`, which
  carries the Finder type/creator, then the resources. Python iteration yields leaves
  only, so that metadata is visible to Rust `EntryType::All`/`Containers` visitors.
- AppleDouble is detected by magic `00 05 16 07` with version `0x00010000` or
  `0x00020000`; such a file is never treated as MacBinary. A standalone AppleDouble
  is a container only if its resource fork passes the strict validation used for bare
  forks (below), which requires at least one resource. Otherwise it is a plain leaf, so
  its bytes are still listed and extracted. Such Finder-info-only sidecars are hidden
  instead when they belong to a directory in a plain directory tree, or sit under a
  Finder zip's `__MACOSX/`.
- Each fork is yielded once. When a file has both a native fork and a sidecar (macOS
  synthesizes the native fork from `._name` on FAT/exFAT/SMB volumes), the native fork
  wins and the sidecar still supplies type/creator.
- A bare resource fork has no magic. Without a `/..namedfork/rsrc` path it is detected
  only if it passes strict structural validation (header, map, type list, reference
  lists, name list and every resource's length prefix in bounds, data and map not
  overlapping, at least one resource), and it is probed after every other format.
  The file is a `ResourceFork` container that yields one container entry,
  `<file>/..namedfork/rsrc` (same bytes), which is the `container_path` of its resources.

## StuffIt Archives

Read-only, `macintosh` feature (`ContainerFormat::StuffIt`). Decoded natively, also in
`no_std` and `wasm32` builds; the reader was written from the format specification in
[docs/stuffit/](stuffit/README.md).

- Classic (`SIT!`, StuffIt 1.x to 4.x) and StuffIt 5 archives, detected at offset 0 by
  structural checks rather than the signature alone. StuffIt X (`.sitx`), encrypted
  forks and segmented archives are not supported.
- Compression methods 0 (stored), 1 (run-length), 2 (LZW), 3 (Huffman), 13 (LZ77 with
  Huffman codes), 14 (raw deflate stream) and 15 (Arsenic).
- StuffIt installer applications (signatures such as `ST46`) store every fork with
  method 14 in a block framing that is not decoded. A file in which every fork with
  contents is such a fork is not detected as an archive: it stays an ordinary file
  entry instead of a container without contents.
- Files are yielded as `<archive>/<folder>/.../<name>` with the data fork as the entry
  data, followed by `<file>/..namedfork/rsrc` when the resource fork holds a resource
  map. Each stored name is one path component: `/`, `\`, `:` and control characters in
  it become `_`, an empty name or `.` becomes `_`, and `..` becomes `__`. Folders
  themselves are not yielded.
- Every fork must decode to exactly its declared length and match its checksum (CRC-16
  in the entry header; the CRC-32 of the stream for method 15). A file whose data fork
  is encrypted, corrupt, truncated, of an unsupported method or over the output limit
  is left out; with an unreadable resource fork only that fork is left out. All unpacked
  forks of one archive together may not exceed 1024 times the archive size (at least
  16 MiB). An entry list that ends early (damaged links, entries that overlap, a
  truncated archive) keeps the entries found so far. In all these cases every readable
  file is still yielded and the traversal reports one recoverable
  `ContainerEntriesSkipped` diagnostic.

## HFS Plus and HFSX Volumes

Read-only, `macintosh` feature. HFS Plus and HFSX open as `ContainerFormat::Hfs`, like
HFS and MFS; there is no separate format value.

- Detected by the volume header signature and version at byte 1024 of the volume: `H+`
  version 4 (HFS Plus) or `HX` version 5 (HFSX). The volume is found wherever an HFS
  volume is: a bare image, behind a DiskCopy 4.2 header, in a DART image, in a partition
  of an Apple Partition Map, in a UDIF or NDIF disk image, in a raw
  2352-byte CD sector dump, and embedded in an HFS wrapper volume. For a wrapper the
  embedded volume is presented, not the wrapper's own stub files.
- UDIF images (`.dmg`) may be unpartitioned or carry an Apple Partition Map or a GUID
  partition table; the first HFS/HFS Plus partition is presented. Stored, zero-fill, ADC,
  zlib, bzip2 and LZMA chunks are decoded. A GUID-partitioned image without the UDIF trailer
  opens as `ContainerFormat::Gpt` (`dos` feature) with the volume as a partition entry.
- DiskCopy 4.2 images (HFS, MFS and HFS Plus alike) are recognised by the whole header,
  not by its `01 00` magic alone: a name length of at most 63, a data size that is a
  non-zero multiple of 512, a tag size of 0, 12 or 20 bytes per block, and a file that
  holds header, data and tags and is less than 2048 bytes longer. The checksums are not
  verified. The volume is the data bytes only; tag bytes and padding are not part of it.
  A truncated image does not open. A Lisa disk in a DiskCopy 4.2 image opens as
  `ContainerFormat::LisaFs` when the `lisa` feature is enabled; see "Apple Lisa Disk
  Images".
- DART images (disk kinds 400K, 720K, 800K and 1440K; fast, best and uncompressed chunks)
  of a disk with an HFS, MFS or HFS Plus volume are decoded when the `lisa` feature is
  enabled as well, which holds the DART decoder; with `macintosh` alone they are not
  recognised. They are detected by content. A chunk that does not decode fails the open;
  the checksums, which DART keeps in the resource fork, are not verified.
- Hybrid ISO 9660 / HFS Plus discs open as HFS, as hybrid HFS discs do, or as
  `ContainerFormat::Apm` with the volume as a partition entry when they carry an Apple
  Partition Map. With the `dos` feature the ISO 9660 volume of such a disc is listed as
  one more partition, `iso9660.img`; see "Toast Images".
- Names are converted from the stored UTF-16 to UTF-8 as stored: decomposed, without
  normalisation. An unpaired surrogate becomes U+FFFD. `/` (a `:` typed in the Finder
  shows as `/` in POSIX and is stored that way), `:` and control characters in a name
  are replaced by `_`, as for HFS. On HFSX, names that differ only in case are separate
  entries; they collide when extracted onto a case-insensitive filesystem.
- Forks: the data fork is the file entry and a non-empty resource fork is
  `<file>/..namedfork/rsrc`, as for HFS. Finder type and creator are in the entry
  metadata. Forks with more than eight extents are read through the extents overflow
  file.
- Hard links: every link is an entry with the content and the Finder type and creator of
  its target (the indirect node file). The content is held once however many links name
  it. All links together may yield up to 1024 times the volume size (at least 16 MiB,
  at most 4 GiB); links beyond that are left out and reported.
- Not listed: the private metadata directories that hold link targets, `.journal` and
  `.journal_info_block` of a journaled volume, symbolic links, device nodes, FIFOs and
  sockets, and files whose forks are both empty. Folders are not entries.
- Not supported:
  - Journal replay. The volume is read as it is on disk, so an image of a volume that
    was not unmounted cleanly can show the state before its last changes.
  - Transparent compression (decmpfs, as written by `ditto --hfsCompression` and by
    macOS for system files). Such a file has an empty data fork on disk and keeps its
    content elsewhere. With the content in its resource fork it appears as an empty
    file with a resource fork that is not a resource map (reported as one
    `NestedContainerOpenFailed` diagnostic). With an empty resource fork as well it is
    not listed, like any file whose forks are both empty.
  - Extended attributes (the attributes file) and directory hard links.
  - LZFSE compressed UDIF images (`ULFO`) and encrypted images.
- Damage: a fork whose extents leave the image is cut short. Left out are files with a
  fork inside the volume header, the catalog file or the extents overflow file, hard
  links without a target, and files whose path exceeds 1024 UTF-8 bytes. Every other
  file is still yielded and the traversal reports one recoverable
  `ContainerEntriesSkipped` diagnostic. A volume whose forks add up to more than twice
  its size, or whose catalog file is unreadable, fails to open.

## Apple Partition Map Disk Images

Read-only, `macintosh` feature (`ContainerFormat::Apm`).

- Detected by the `ER` driver descriptor at byte 0 and a `PM` partition map with at
  least one listed partition that starts inside the image. The map is read with
  512-byte blocks or with the block size the driver descriptor declares, whichever
  holds its first entry (some CDs declare 2048-byte blocks around a 512-byte map). Raw
  2352-byte CD sector dumps are converted to user data first, and so are images of
  2056- or 2336-byte CD-ROM XA sectors (see "Toast Images").
- Probed before HFS: an image that used to open as its first HFS partition now opens
  as its partition map, and the volume's files are one level deeper, under the
  partition entry.
- Every partition is one entry holding the partition's bytes, and is then opened by
  its content (HFS, HFS Plus, BFS, ISO 9660, ...), not by its type string. Partitions
  of type `Apple_partition_map`, `Apple_Free`, `Apple_Void` and `Apple_Scratch` are not
  listed; driver and patch partitions are.
- An entry is named after the partition's name in the map when that name is printable
  ASCII without `/`, `\` or `:` and no other listed partition has it (compared without
  regard to case); otherwise, and always with `numeric_identifiers`, it is `p<n>.img`,
  where `n` is the zero-based position in the map.
- A partition that reaches past the end of the image is cut off there. A partition of
  zero length, one that starts at block 0 or past the end, and one that shares bytes
  with an earlier listed partition are left out, without a diagnostic. Maps declaring
  more than 128 entries are not recognised.
- Not covered: a partition map inside a UDIF or NDIF disk image or behind a DiskCopy
  4.2 header. Those still open as `ContainerFormat::Hfs`, presenting the first HFS
  partition.

## ISO 9660 CD-ROM Images

Read-only, `dos` feature (`ContainerFormat::Iso9660`).

- Detected by a `CD001` primary volume descriptor at sector 16, in cooked 2048-byte
  images and in raw 2352-byte sector dumps (MODE1 and MODE2/XA `.bin`/`.img`). Images
  of 2056- or 2336-byte CD-ROM XA sectors (no sync pattern or header) are read through
  their user data when nothing else claims the file; see "Toast Images".
- Probe order: HFS (hybrid Mac discs), FAT, GPT and MBR are tried first and keep
  winning, so isohybrid images open as their partition table. A hybrid Mac disc lists
  its ISO 9660 volume as the partition `iso9660.img` beside the Macintosh side (see
  "Toast Images").
  A Nero NRG image is tried
  before all of them (see "Nero NRG Images"). A UDF bridge disc (ISO
  9660 and UDF over the same files) opens as ISO 9660; see "UDF Disc Images".
- Names: Rock Ridge `NM` names are used when present (including `CL`/`RE` relocated
  directories; symlinks and device nodes are not listed); otherwise the Joliet tree;
  otherwise plain ISO names with the `;1` version and a trailing `.` removed. Rock
  Ridge entries without `NM` names (as `hdiutil makehybrid` writes them) do not keep
  the Joliet tree from being used. A disc whose Rock Ridge names are not valid UTF-8
  uses its Joliet tree instead, if it has one. Plain names are presented in lower
  case, like FAT short names. A `/` inside a name is replaced by `_`.
- Multi-extent files are joined. Associated files (flag `0x04`) are not listed.
- High Sierra: a volume with `CDROM` at byte 9 of sector 16 (the May 1986 working
  paper that preceded ISO 9660) is read with the High Sierra descriptor and directory
  record layouts. Names are presented like plain ISO names. Rock Ridge, Joliet, boot
  catalogs and later sessions are not looked for on such a volume.
- El Torito: the boot images named by the boot catalog are listed as extra entries
  under the reserved directory `..eltorito/` at the disc root, as `<n>.img` with `n`
  counting the catalog's entries from 0 (the default entry first, then the section
  entries; an entry that is not listed keeps its number). The boot catalog itself is
  not listed, unless the writer also recorded it as a file. Sizes:
  - floppy emulation: 1,228,800, 1,474,560 or 2,949,120 bytes (1.2 MB, 1.44 MB,
    2.88 MB);
  - hard disk emulation: the extent covered by the partition table in the image's
    first sector;
  - no emulation: the catalog's sector count times 512. An entry with sector count 0
    names no image and is not listed.

  An image that is a file already listed is not listed twice: same start sector, and
  the file's length, rounded up either to the 512-byte sectors the catalog counts in
  or to a whole logical block (as xorriso records a no-emulation boot file), equals
  the image's. A no-emulation image that starts where a longer listed file starts is
  not listed either: its sector count only says how much of that loader the BIOS reads
  (isolinux records four sectors). A boot image hidden from the presented tree but recorded in the other
  one (primary or Joliet) is listed under `..eltorito/`. The catalog's validation
  entry must have its key bytes and a zero checksum, otherwise no boot image is listed.
- Multi-session: when one image file holds several sessions, the tree of the last
  session is presented; its directories already include the files of earlier sessions
  that were kept. There is no table of contents in a plain image, so a later session
  is looked for only at sector-aligned positions past the previous session's volume
  space, and is accepted only if its root directory lies in that session and its `.`
  record points back at it under addressing from the start of the image. An ISO file
  stored inside the first session is therefore not taken for a session. At most 99
  sessions are followed.
  - A later session's data track on its own (as a CUE sheet with one file per track
    hands it out) opens too, but only the files recorded in that track can be read;
    files kept from earlier sessions are left out and reported.
  - An image from which the gaps between sessions were removed does not hold together
    under absolute addressing; its first session is presented.
- Not supported: CD audio tracks, interleaved files, High Sierra volumes with
  descriptors other than the standard file structure descriptor, boot catalog
  metadata (platform, load segment, selection criteria), and separate access to
  earlier sessions.
- Files that cannot be read (interleaved mode, extents outside the image, directory
  loops, nesting deeper than 32 levels, output above twice the image size) and boot
  images that do not fit in the disc image, have no partition table to take a size
  from or use an unknown emulation type are left out; every readable file is still
  yielded and the traversal reports one recoverable `ContainerEntriesSkipped`
  diagnostic.

## UDF Disc Images

Read-only, `dos` feature (`ContainerFormat::Udf`, extension `.udf`).

- Detected by an `NSR02` or `NSR03` descriptor in the volume recognition sequence at
  byte 32768, an anchor volume descriptor pointer at sector 256, N-1, N-257 or 512,
  and a root directory that can be reached through the main or the reserve volume
  descriptor sequence. Cooked images with logical blocks of 2048, 512, 1024 or 4096
  bytes, and raw 2352-byte sector dumps (2048-byte blocks only).
- Bridge discs: an image that also has an ISO 9660 volume keeps opening as
  `ContainerFormat::Iso9660`, whatever its extension. UDF is chosen only when there is
  no ISO 9660 volume or when that volume has no root directory to read (neither the
  primary nor the Joliet root is where its descriptor says). HFS, FAT, GPT and MBR are
  tried before both.
- Revisions 1.02 to 2.60: file entries and extended file entries, ICB strategy 4,
  indirect entries, short, long and extended allocation descriptors, data embedded in
  the ICB, continuation extents, and unrecorded or sparse extents, which read as
  zeros.
- Partitions: physical (type 1); virtual (UDF 1.50 write-once media, with the VAT in
  the 1.50 or the 2.00 layout; the newest table is found by searching back from the
  end of the image); sparable (UDF 1.50, with its sparing table); metadata (UDF 2.50,
  with the mirror file standing in for unreadable blocks of the main file).
- Names are converted from OSTA compressed Unicode (8-bit and 16-bit) to UTF-8 as
  stored; an unpaired surrogate becomes U+FFFD and a `/` inside a name is replaced by
  `_`. The volume label is the logical volume identifier, or the name in the VAT of a
  UDF 2.00 write-once volume.
- Hard links to files are entries with the same content. Empty files are listed.
- Not listed: directories as such, symbolic links, device nodes, FIFOs and sockets,
  deleted entries, the parent entry, and the `Non-Allocatable Space` list of a UDF
  1.50 sparable volume.
- Not supported:
  - Named streams and extended attributes; only the main data stream is read.
  - ICB strategy 4096 and pseudo-overwrite partitions (UDF 2.60).
  - Raw sector dumps with a logical block size other than 2048.
  - Earlier states of a write-once volume: only the newest VAT is used.
  - Timestamps and permissions.
- Damage: every descriptor must have a valid tag checksum, CRC and recorded location
  to be trusted, and so must every directory entry (its CRC may cover the entry only).
  Files with extents outside the image or the partition, with fewer extents than their
  length needs or with a looping descriptor chain, directories nested deeper than 64
  levels, larger than 16 MiB or reachable under a second name, and files beyond twice
  the image size of output (at least 16 MiB) are left out; a single file larger than
  that budget, such as a huge sparse file, is left out on its own. Reading descriptors
  and mapping file blocks is bounded by a work budget of twice the image size (at least
  16 MiB); a volume whose entries alias one another beyond it is cut off there. Every
  other file is still yielded and the traversal reports one recoverable `ContainerEntriesSkipped`
  diagnostic.

## CUE/BIN CD Images

Read-only, `dos` feature (`ContainerFormat::CueBin`). Read natively, also in `no_std`
and `wasm32` builds; the reader was written from the format specification in
[docs/cue/](cue/README.md).

- Detected only under the extension `.cue` (any case), and then by content: text of at
  most 1 MiB whose first non-empty line starts with a cue sheet command and which has a
  `FILE` command followed by a `TRACK` command. Encoding: a UTF-8 or UTF-16 byte order
  mark decides; otherwise UTF-8 when valid, else one character per byte (ISO 8859-1).
  LF, CR LF and lone CR end a line; a Ctrl-Z ends the sheet; a NUL disqualifies it.
  Command words, file types and modes are case-insensitive.
- Used: `FILE` (types `BINARY` and `MOTOROLA`; no type means `BINARY`), `TRACK` and the
  first `INDEX 00` and `INDEX 01` of each track. Everything else is ignored: `PREGAP`
  and `POSTGAP` (they describe sectors that are not in the file), higher indexes,
  `FLAGS`, `CATALOG`, `ISRC`, `CDTEXTFILE`, `TITLE`, `PERFORMER`, `SONGWRITER`, `REM`
  and unknown commands.
- One entry per readable track, in sheet order, named after the track number only:
  - data tracks as `trackNN.img`, cooked to the 2048 user bytes of every sector
    (`MODE1/2048`, `MODE2/2048`, `MODE1/2352`, `MODE2/2352`, `CDI/2352`, `MODE2/2336`,
    `CDI/2336`). In 2352-byte sectors the sector's own mode byte chooses between user
    data at byte 16 and byte 24; the declared mode is used for sectors without a sync
    pattern. The track is detected by content like any other file, so it opens as the
    file system or partition map it holds (ISO 9660, HFS, FAT, ...);
  - audio tracks (`AUDIO`, `CDG`) as `trackNN.pcm`: raw PCM, not decoded and without a
    header (16-bit little-endian stereo samples at 44.1 kHz, 2352 bytes per sector).
    Samples of a `MOTOROLA` file are byte-swapped; the 96 sub-channel bytes of a `CDG`
    sector are dropped. A `.pcm` entry is always a leaf: it is not detected by content
    and not descended into, whatever its samples look like.
- A track runs from its `INDEX 01` to the first index of the next track in the same
  file, or to the end of the file; a trailing partial sector is ignored. The gap between
  `INDEX 00` and `INDEX 01` is in no entry. Entries carry no metadata; titles and
  performers are not used.
- Sessions are not modelled: `REM SESSION` is ignored and each track is an entry of its
  own (for a later session's data track see "ISO 9660 CD-ROM Images").
- Track files are looked up next to the sheet — in the same directory for a file on
  disk, in the same parent container when nested — and never outside that directory:
  `\` is read as `/`, a relative path without `.`/`..` components is tried as written,
  then the base name as written, in lower case and in upper case. An absolute path, a
  drive or a `..` component is never followed; only the base name is looked up beside
  the sheet. A sheet with a single track file also tries its own name with the file's
  extension and with `.bin`/`.BIN`. On disk, a link that leads out of the sheet's
  directory is not read. Data passed as bytes has no siblings: the sheet opens, lists
  nothing and reports every track.
- When a directory or archive is traversed, a track file that an opened sheet reads a
  track from is not listed again on its own. A track file that comes before its sheet
  is held back for it when the sheet has the file's name (`game.bin` and `game.cue`,
  `game (Track 2).bin` and `game.cue`) or, in a directory on disk, whatever the sheet
  is called; if that sheet is then not opened, the file is listed after the other
  entries. Inside an archive, a track file stored before a sheet with an unrelated name
  is listed on its own as well as through the sheet. A listing that does not descend
  into containers shows sheet and track files as they are, and a track file addressed
  directly opens on its own.
- Limits: 99 `FILE` commands; track numbers 1 to 99, increasing; one track at most
  4 GiB; all tracks of a sheet together at most the size of the sheet and its track
  files (at least 16 MiB, at most 4 GiB).
- Not supported: track files of type `WAVE`, `MP3`, `AIFF` or any other audio container
  (never looked up), `MODE2/2324` and unknown track modes, separate `.sub` sub-channel
  files, CD-TEXT, and the audio in a gap before `INDEX 01`. Mode 2 Form 2 sectors
  contribute their first 2048 bytes only. EDC and ECC are not checked.
- Damage: a sheet with a `FILE` without a name, a `TRACK` without number or mode, a
  `TRACK` before the first `FILE`, a track number outside 1 to 99 or not above the one
  before it, or more than 99 `FILE` commands fails to open. Otherwise a track is left
  out when its file is missing or of an unsupported type, its mode is unsupported, it
  follows a track of unknown sector size, it has no `INDEX 01`, it starts beyond the end
  of its file, it is empty or over a size limit; index times that go backwards leave out
  every track of that file. Every other track is still yielded and the traversal reports
  one recoverable `ContainerEntriesSkipped` diagnostic naming the first problem and
  counting the rest.

## Nero NRG Images

Read-only, `dos` feature (`ContainerFormat::Nrg`). Read natively, also in `no_std` and
`wasm32` builds; the reader was written from the format specification in
[docs/nrg/](nrg/README.md).

- Detected by content under any file name; the extension `.nrg` is not required and
  proves nothing. The file must end with a version 2 footer (`NER5` and a 64-bit chain
  offset, the last 12 bytes) or a version 1 footer (`NERO` and a 32-bit offset, the last
  8 bytes), and the chain of chunks between that offset and the footer must be valid:
  at most 1 MiB and 256 chunks, printable four-character identifiers, every chunk
  inside the chain, an `END!` chunk, and 1 to 99 track entries. A file with bytes after
  the footer, or cut short, is not an NRG image.
- Probe order: this test runs before every other format, whatever the extension. An NRG
  image has no header: it starts with whatever its first track holds, so a track-at-once
  image of a data disc and every DVD image are also valid ISO 9660 images with a few
  bytes appended. They open as `Nrg` all the same, with the volume one level down in
  `track01.img`. A `.nrg` file without a valid footer and chain (a plain sector dump
  under that name, say) is left to the other detectors.
- Used: the track chunks `DAOX` and `DAOI` (disc-at-once; 64- and 32-bit offsets),
  `ETN2`, `ETNF` and `TINF` (track-at-once). The identifier decides a chunk's layout,
  not the footer version. Everything else is skipped: `CUEX`/`CUES`, `SINF`, `MTYP`,
  `CDTX`, `DINF`, `TOCT`, `RELO`, `AFNM`, `VOLM` and unknown chunks.
- One entry per readable track, numbered from 1 in chain order across all sessions and
  named after the track number only, exactly like the entries of a CUE/BIN image:
  - data tracks as `trackNN.img`, cooked to the 2048 user bytes of every sector (mode
    codes `00`, `02`, `03`, `05`, `06`, `0F`, `11`; stored sectors of 2048, 2336, 2352
    or 2448 bytes). In a disc-at-once entry the sector size field gives the size and the
    mode code only the kind. In 2352- and 2448-byte sectors the sector's own mode byte
    chooses between user data at byte 16 and byte 24; the mode code is used for sectors
    without a sync pattern. The track is detected by content like any other file, so it
    opens as the file system or partition map it holds (ISO 9660, UDF, HFS, ...);
  - audio tracks (mode codes `07`, `08`, `10`) as `trackNN.pcm`: raw PCM, not decoded
    and without a header (16-bit little-endian stereo samples at 44.1 kHz, 2352 bytes
    per sector). A `.pcm` entry is always a leaf: it is not detected by content and
    not descended into, whatever its samples look like.
  - The 96 sub-channel bytes of a 2448-byte sector are dropped.
- A track runs from its index 1 to its end. An end beyond the data area is cut at the
  chain offset; a trailing partial sector is ignored. Pregaps (index 0 to index 1,
  usually 150 sectors before the first track of a disc-at-once session) and bytes that
  no track covers are in no entry, so `track01.img` of a data CD starts with the
  volume's first sector. Entries carry no metadata; catalogue numbers, ISRC codes,
  CD-TEXT and source file names are not used.
- Sessions are not modelled: a multi-session image lists the tracks of all sessions in
  order. A data track of the first session is a complete volume. A later session's data
  track is handed out like the track file of a cue sheet (see "ISO 9660 CD-ROM
  Images"): files recorded in that session are read, files it kept from an earlier
  session lie outside the track and are reported.
- Limits: 99 tracks; one track at most 4 GiB (so the single track of a larger DVD image
  is reported, not listed); all tracks together at most the size of the image (at least
  16 MiB, at most 4 GiB). Tracks never overlap, so an image cannot yield more data than
  it holds.
- Not supported: the audio in a pregap, sub-channel data, CD-TEXT, and disc addresses
  for later sessions. Mode 2 Form 2 sectors contribute their first 2048 bytes only, so
  the video streams of a Video CD are not recoverable. EDC and ECC are not checked.
- Damage: a track is left out when its mode code is unknown, its sector size is not
  one of the four (or not 2352 or 2448 for audio), it starts beyond the data area, it is
  empty or over a size limit. Offsets that go backwards anywhere in the chain (tracks
  or pregaps that overlap, a track-at-once length that overflows) leave out every
  track. The image still opens, every readable track is yielded and the traversal
  reports one recoverable `ContainerEntriesSkipped` diagnostic naming the first problem
  and counting the rest.

## Toast Images

A `.toast` file (Roxio Toast and its predecessors) is not a format of its own but a
dump of the sectors of one data track, without header or trailer. It opens as what it
holds; the extension only puts the Macintosh probes first, and the same bytes open the
same way under any name and inside any archive. The layouts found in real files, and
the measurements behind this section, are in [docs/toast/](toast/README.md).

| The file holds | Opens as |
|---|---|
| An Apple Partition Map (Toast's own `MRKS` map, or any other; map in 512- or 2048-byte units) | `Apm`, one entry per partition; the Macintosh volume opens as `Hfs` |
| A bare HFS or HFS Plus volume | `Hfs` |
| ISO 9660 only (Toast's "ISO 9660" and "Joliet" discs) | `Iso9660` |
| ISO 9660 and UDF over the same files (DVD projects) | `Iso9660`, as any bridge disc |
| UDF only | `Udf` |
| A Macintosh volume and an ISO 9660 volume (hybrid disc) | `Apm` or `Hfs`, with the ISO 9660 volume as one more partition entry, `iso9660.img`, which opens as `Iso9660` |
| Any of these in an archive (`.toast.sit`, `.toast.zip`) | the archive, with the image as an entry that opens as above |

- Sector framings: cooked 2048-byte sectors (every real file seen), raw 2352-byte
  sectors (Mode 1, or Mode 2 Form 1; told by the sync pattern), and CD-ROM XA sectors
  without sync and header, 2056 bytes (sub-header and user data) or 2336 bytes
  (sub-header, user data, error detection and correction) each. The last two have no
  signature, so they are accepted only when no format matches the file as it is, the
  length is a whole number of at least 17 such sectors, each of the first 17 sectors
  repeats its four sub-header bytes, and the 2048 bytes at offset 8 of every sector
  together hold a partition map, a Macintosh volume, ISO 9660 or UDF. In an image of
  more than 320 sectors the first 320 must already show that volume, so that a file
  without one is never copied in full to find out, and the user data must fit the 4 GiB
  limit for a single output. 2056 is tried before 2336. A Form 2 sector contributes the first 2048 of its 2324 bytes, as for
  CUE/BIN. This applies to such images under any name, not only `.toast`.
- Hybrid discs: an image that opens as `Apm` or `Hfs` and also has an ISO 9660 volume
  with a readable root directory lists that volume as one more partition entry, after
  the partitions of the map (`dos` and `macintosh` features together). The entry is
  named `iso9660.img`, has the size of the (cooked) image and holds its bytes with the
  first 16 sectors blanked, so that it opens as `Iso9660` and not as the partition map
  again; ISO 9660 addresses count from the start of the disc, so the volume cannot be
  cut any shorter. Below it is what the same bytes list when opened as ISO 9660 (same
  Joliet and Rock Ridge rules, `..eltorito/` included; one tree per volume, so a file
  that only the Joliet tree of a volume with Rock Ridge names holds is not listed).
  The Macintosh side is unchanged. Where both file systems describe the same file it
  is listed on both sides; where the ISO 9660 volume precedes the Macintosh partition
  or hides files from it, `iso9660.img/` is the only way to them.
  A partition whose map name is `iso9660.img` is left out and reported as skipped.
  A hybrid without a partition map opens as `Hfs` and has no partition list, so the
  entry sits at the root of the volume under the reserved name `..iso9660.img`; a
  Macintosh file or folder of that name there is left out and reported as skipped.
  The entry is absent when the ISO 9660 root lists no file, and it is not derived
  from a partition of type `ISO` (which stays an opaque entry). The UDF tree of a
  disc that also has an ISO 9660 tree is not listed separately. The entry counts as
  one more copy of the image against memory.
- A partition whose map name cannot be a path component is listed as `p<n>.img`; that
  includes Toast's own `Toast … HFS/Joliet Builder` and `HFS+/Joliet Builder` names.
- Bytes after the last volume (Toast often appends about 150 empty sectors) are
  ignored.
- Not supported: Toast's audio CD images (`.Sd2f`: headerless audio whose track table
  is in a resource fork) are not containers. No `.toast` file with several tracks is
  known; Toast exports those as CUE/BIN.

## ARJ Archives

Read-only, `dos` feature (`ContainerFormat::Arj`), decoded natively (also in `no_std`
and `wasm32` builds).

- Detected by the `60 EA` header id at offset 0 followed by a main header of plausible
  size whose CRC-32 matches. Self-extracting archives (an `.EXE` stub before the main
  header) are not recognised.
- Methods 0 (stored), 1-3 (LZ77 with Huffman coding) and 4 (fastest). Every file is
  checked against its CRC-32. `Metadata::compression_method` is `stored` or
  `arj1`-`arj4`; archives written on Unix also carry the permission bits as `mode`.
- Names keep their stored case and order. Names from DOS and OS/2 hosts are converted
  from code page 437; names from other hosts are taken as UTF-8 when they are valid
  UTF-8 and as code page 437 otherwise. `\` separators become `/` unless the header
  says the writer already translated them.
- Binary and text-mode files are listed; text-mode files are yielded as stored, without
  line-ending conversion. Directory entries, volume labels and chapter labels are not
  listed. Archive and file comments, timestamps and extended headers are ignored.
- Multi-volume sets (`name.arj`, `name.a01`, `name.a02`, ... up to `.a99`) are read
  through their first volume: it lists the files of every volume and joins files split
  across them. Later volumes are looked up next to it — in the same directory for a
  file on disk, in the same parent container when nested — and are not listed again
  when the volume before them is present. A missing volume leaves out the files that
  need it and is reported; a volume without its predecessor opens on its own and
  carries the rest of the set. Data passed as bytes has no siblings, so only whole
  files are listed.
- Not supported: garbled (password-protected) files, ARJ-PROTECT recovery data.
- A file that is garbled, incomplete, corrupt (CRC mismatch, truncated or invalid compressed
  data), of an unknown method or over the output limit is left out. All unpacked files
  of one archive together may not exceed 1024 times the archive size (at least 16 MiB).
  A damaged header ends the listing there and keeps the files found so far. In all
  these cases every readable file is still yielded and the traversal reports one
  recoverable `ContainerEntriesSkipped` diagnostic.

## RAR Archives

Read-only, `dos` feature (`ContainerFormat::Rar`). Read and decoded natively, also in
`no_std` and `wasm32` builds; the reader was written from the format specifications in
[docs/rar/](rar/README.md). Directories are not listed.

- Supported: the RAR 1.3/1.4 container (signature `RE~^`), the RAR 1.50 to 4.x format
  (`Rar!\x1a\x07\x00`) and the RAR 5 format (`Rar!\x1a\x07\x01\x00`) with both of
  its algorithms (RAR 5 and RAR 7). In the RAR 1.50 to 4.x format the decoder is chosen
  per file by its unpack version: 13 and 15 (RAR 1.5), 20 and 26 (RAR 2.0, including
  multimedia compression), 29 and 36 (RAR 2.9 to 4.x: LZ, PPMd and filters, the six
  standard filters as well as other filter programs). Solid archives of every
  generation are read.
- Self-extracting archives are read: in a DOS, Windows or OS/2 executable (`MZ`), an
  ELF or a Mach-O file the archive is searched for in the first 1 MiB, and a signature
  counts only when a sound archive header follows it. Other files have to start with
  the archive.
- Not supported: encrypted files; archives with encrypted headers, which open with
  nothing listed and are reported; other unpack versions and methods. A RAR 5 file
  with a dictionary above 1 GiB is decoded as long as no match reaches back further
  than 1 GiB.
- A RAR 1.50 to 4.x archive whose archive header fails its checksum is rejected as
  corrupt. A file header that fails its checksum (RAR 1.50 to 4.x and RAR 5) is damage:
  the listing ends there.
- Every file is verified before it is yielded: its length against the declared size
  and its contents against the stored checksum (CRC-32; in RAR 5 also the BLAKE2sp
  hash when one is recorded; the 16-bit checksum of the RAR 1.3/1.4 container). For a
  file split across volumes the last part carries the checksum of the file, and the
  checksums the other parts carry of their own packed bytes are verified too. A RAR 5
  file that carries neither a CRC-32 nor a BLAKE2sp hash is yielded unchecked, and so
  is an empty file of the RAR 1.50 to 4.x format.
- A file that is encrypted, of an unknown unpack version or method, corrupt (decoding
  error, wrong length, checksum mismatch) or incomplete is left out; the other files
  are still yielded and the traversal reports one recoverable
  `ContainerEntriesSkipped` diagnostic that spells out the first three problems and
  counts the further ones. In a solid archive a file that cannot be decoded, or a split
  file whose pieces are not all there, also takes the later files that depend on it
  with it (in RAR 1.x every later compressed file of the archive); a file that merely
  fails its checksum does not. An empty file is yielded as an empty file whatever
  happened to the files before it. A damaged or truncated block header ends the listing
  there, keeps the files found so far, is reported, and in the RAR 1.50 to 4.x and RAR 5
  formats also ends the volume set.
- In the RAR 1.50 to 4.x format an entry is a directory by its header flags alone
  (unpack version 20 and above) or by its DOS or OS/2 directory attribute (below 20);
  an empty entry that merely has a directory attribute is yielded as an empty file.
- Links: a Unix symbolic link of the RAR 1.50 to 4.x format is yielded as a file whose
  contents are the link target, as stored. So is a RAR 5 symbolic link or junction (a
  target over 4,096 bytes is left out and reported); a RAR 5 hard link or file copy is
  yielded with the contents of the file it refers to, and one whose file could not be
  read is left out and reported. Links are never followed.
- An old version of a file kept in a RAR 5 archive is yielded under its name followed
  by `;` and the version number (`hello.txt;1`).
- The compression-ratio limit (1000:1, measured over at least 64 packed bytes) applies
  to RAR 2.9 to 7 files; for a solid archive it is applied to the files of one solid
  group together, since a file that repeats earlier ones packs to almost nothing. A
  RAR 5 file of undeclared size may decode to as much as that limit allows.
- File names of the RAR 1.3/1.4 container, and names of the RAR 1.50 to 4.x format that
  have no Unicode form and are not UTF-8, are read as code page 437.
- Multi-volume sets are read through their first volume, which lists the files of every
  volume and joins files split across them. Both naming schemes are followed:
  `name.part1.rar`, `name.part2.rar`, ... and `name.rar`, `name.r00`, `name.r01`, ....
  A self-extracting first volume (`name.part1.exe` or `name.part1.sfx`, `name.exe` or
  `name.sfx`) is followed by `name.part2.rar` or `name.r00` in the same way.
  Later volumes are looked up next to the first one — in the same directory for a file
  on disk, in the same parent container when nested — and are not listed again when
  a volume that reaches them (the one before, or an earlier one across up to eight
  missing volumes) is present.
- A missing volume leaves out the files that need it and is reported as one recoverable
  `ContainerEntriesSkipped` diagnostic; when one of the next eight volumes is present,
  reading continues there, so files that lie entirely in later volumes are still
  yielded (in a solid set only from the next file that starts a solid group). A volume
  without its predecessor opens on its own and carries the rest of the set. Data passed as bytes has no siblings, so a file
  that continues in another volume is left out and reported rather than truncated.
  The volumes of a set are held in memory together while it is read.

## LHA / LZH Archives

Read-only, `amiga` feature (`ContainerFormat::Lha`). Decoded natively, also in `no_std`
and `wasm32` builds; the reader was written from the format specification in
[docs/lha/](lha/README.md).

- Header levels 0, 1, 2 and 3, including extended headers and 64-bit sizes. Detected by
  a valid entry header at offset 0 (signature, header level and header checksum or
  CRC), not by the `-lh` / `-lz` signature alone. Self-extracting archives and files
  with other data before the first header are not detected, nor are archives whose
  first entry uses a `-pm` method.
- Compression methods `-lh0-` and `-lz4-` (stored), `-lh1-`, `-lh4-`, `-lh5-`, `-lh6-`,
  `-lh7-`, `-lzs-` and `-lz5-`. The method is reported as `compression_method`
  (`lh5`, `lzs`, ...); stored entries report none.
- Files are yielded as `<archive>/<dir>/.../<name>`, sorted by path. Directory entries
  (`-lhd-`) and symbolic links are not yielded. Paths are made relative: empty, `.` and
  `..` components are removed (a level 1 to 3 name header that is only `.` or `..`
  becomes `_`, so the file does not take the path of its directory), `\`, `:` and
  control characters in a component become `_`, and a file without a name is called
  `unnamed`. Names are read as UTF-8 when they are well-formed UTF-8 and as
  ISO 8859-1 otherwise; other code pages (CP437, Shift-JIS) are not converted. Their
  bytes `80` to `9F`, which ISO 8859-1 maps to control characters, are written as
  `%80` to `%9F` so that different stored names stay different.
- Every entry must decode to exactly its declared length and match its CRC-16. An
  entry that is corrupt, truncated, over the output limit or of an unsupported method
  (`-lh2-`, `-lh3-`, `-lhx-`, `-pm0-` to `-pm2-`, ...) is left out. Everything
  decoded from one archive together may not exceed 1024 times the bytes its entries
  occupy (headers and packed data; bytes after the end marker do not count). There is
  no minimum, entries that fail after decoding count like delivered ones, and an
  entry whose declared length is above what is left is not decoded and does not use
  any of it. An entry list that ends early (an invalid header, a truncated
  archive) keeps the entries found so far. In all these cases every readable file is
  still yielded and the traversal reports one recoverable `ContainerEntriesSkipped`
  diagnostic.

## Amiga Disk Images (ADF)

Read-only, `amiga` feature (`ContainerFormat::Adf`).

- Detected by a `DOS\0`–`DOS\7` boot block plus a checksum-valid root block; DD, HD and
  other sizes with 512-byte blocks. Gzip-wrapped images (`.adz`, `.adf.gz`) work through
  the gzip layer.
- OFS and FFS, including the international and directory-cache variants. Names are
  converted from Latin-1.
- Not supported: long-filename volumes (`DOS\6`/`DOS\7`), RDB-partitioned hardfiles,
  DMS, non-DOS (trackloader) disks. Hard and soft links are not listed.
- Damaged blocks (bad checksums, pointers outside the image, loops) are left out; every
  readable file is still yielded and the traversal reports one recoverable
  `ContainerEntriesSkipped` diagnostic.

## NeXT Disk Labels

Read-only, `next` feature (`ContainerFormat::NextDisklabel`). Read natively, also in
`no_std` and `wasm32` builds; the reader was written from the format specification in
[docs/ufs/](ufs/README.md). The partition table of NeXTSTEP, OPENSTEP and Rhapsody
floppies, hard disks and CD-ROMs.

- Detected by a `NeXT`, `dlV2` or `dlV3` label with a valid checksum at byte 0, 7680,
  8192, 15360, 16384, 23040 or 24576 of the image (the copy positions for 512-, 1024-
  and 2048-byte device sectors); the first valid copy is used, so a damaged or missing
  first copy does not matter. The label must also have a sector size of 512, 1024, 2048
  or 4096, a position field that agrees with where it was found, the uninitialised flag
  clear, and at least one partition that starts inside the image. No extension is
  needed or used.
- Probed before UFS, BFS, Lisa, HFS, FAT, GPT, MBR, ISO 9660 and UDF. A label behind a
  PC boot sector (Intel floppies and whole-disk Intel installs) is used only when the
  boot sector's partition table is empty; with a partition in use the image opens as
  MBR.
- Labels are big-endian on every platform, Intel media included.
- Each partition in use is one entry holding the partition's bytes, named `p0.img` to
  `p7.img` by its index in the label. When the label has a single partition it is named
  after the label's volume name instead; `with_numeric_identifiers` keeps `p0.img`. In
  that name `/`, `\`, `:` and bytes outside printable ASCII become `_`.
- The entry is detected by content like any other file: a UFS volume opens as
  `ContainerFormat::Ufs` below it. The label's file system type string is ignored, and
  a partition that holds something unrecognised stays a plain file entry. A labelled
  image always opens as the label, also with one partition; it never opens directly as
  UFS.
- Intel hard disks may keep the label inside a PC partition of type `0xA7`. Such a disk
  opens as `ContainerFormat::Mbr` (`dos` feature) and the label is found in the
  partition entry; the label's absolute sector numbers are resolved against the
  partition. This layout is implemented from documentation and checked against a
  synthetic image only; no real disk of this kind was available.
- Not listed: the front porch (boot blocks and label copies) and unused partition
  entries.
- Not supported: labels at any other position (the image is never scanned for one), BSD
  and Sun disk labels.
- Damage: a copy with a bad checksum is passed over for the next one; an image with no
  valid copy is not a NeXT disk label. A partition that ends beyond the image is cut
  at its end; one that starts beyond it, or that contains the label copy describing
  it, is left out. A partition that overlaps an earlier one is left out and the
  traversal reports one recoverable `ContainerEntriesSkipped` diagnostic.

## UFS Volumes

Read-only, `next` feature (`ContainerFormat::Ufs`, extension `.ufs`). Read natively,
also in `no_std` and `wasm32` builds; the reader was written from the format
specification in [docs/ufs/](ufs/README.md). UFS1, the Berkeley Fast File System, as
written by NeXTSTEP, OPENSTEP and Rhapsody and by 4.2BSD, 4.3BSD, 4.4BSD and NetBSD.

- Detected by the magic `0x00011954` of the superblock at byte 8192 of the volume, in
  either byte order, plus consistent superblock fields and a root directory (inode 2)
  that starts with its own `.` record. The magic alone is not enough. Found as a bare
  volume image under any name and as a partition of a NeXT disk label; probed right
  after the label and before BFS, Lisa, HFS, FAT, GPT, MBR, ISO 9660 and UDF.
- Big-endian and little-endian volumes; the byte order is the volume's, not the
  machine's (NeXTSTEP and OPENSTEP media for Intel are big-endian). Block sizes from
  4096 to 65536 bytes, fragment sizes from 512 bytes up to the block size, at most 8
  fragments per block.
- Both generations: the 4.2BSD/4.3BSD layout (NeXTSTEP, OPENSTEP) and the 4.4BSD layout
  (Rhapsody, NetBSD; directory records with a type byte).
- Regular files are entries, read through direct, single, double and triple indirect
  blocks; a last block stored as fragments is read to the file's length. Holes read as
  zeros. Empty files are listed. Entries are yielded sorted by path.
- Hard links: every name is an entry with the full content.
- Names are taken as UTF-8; bytes that are not valid UTF-8 become U+FFFD. `\`, `:` and
  control characters are replaced by `_`. Names that differ only in case are separate
  entries; they collide when extracted onto a case-insensitive filesystem.
- `Metadata::mode` holds the permission bits as an `ls`-style string (`-rwxr-xr-x`,
  with set-user-id, set-group-id and sticky as `s`/`S` and `t`/`T`).
- Not listed: directories as such (so empty directories do not appear), `.` and `..`,
  symbolic links (they are not followed either), device nodes, FIFOs, sockets and
  whiteouts.
- Not supported:
  - UFS2: not detected as UFS.
  - Volumes that do not start at the first byte of the data: BSD and Sun disk labels
    are not interpreted, and the image is never scanned for a superblock. Backup
    superblocks are not used.
  - Timestamps, owners and groups (there is no `Metadata` field for them).
- Limits: the contents and paths of all listed files together may not exceed twice the
  volume size (at least 16 MiB, at most 4 GiB); a file that does not fit is left out on
  its own and smaller files after it are still listed. At most 4,194,304 files and
  directories. Directories nested more than 63 levels below the root are not entered.
  A directory reachable under a second name is entered once.
- Damage: a volume whose superblock or root directory fails the checks is not detected
  at all. A truncated image is accepted. Left out are files with a block or indirect
  block outside the volume or the image, names with an inode outside the volume or of
  an unknown type, names containing `/` or a zero byte, and the rest of a directory
  from a malformed record, an unallocated block or a block outside the volume on. Every
  other file is still yielded and the traversal reports one recoverable
  `ContainerEntriesSkipped` diagnostic.

## BeOS Volumes (BFS)

Read-only, `beos` feature (`ContainerFormat::Bfs`, extension `.bfs`). Read natively,
also in `no_std` and `wasm32` builds; the reader was written from the format
specification in [docs/bfs/](bfs/README.md).

- Detected by the `BFS1` superblock at byte 512 of the volume (Intel BeOS and Haiku) or
  at byte 0 (PowerPC BeOS), tried in that order, in little-endian or big-endian byte
  order at either position. All three magic numbers and the byte-order mark must match,
  the block size must be 1024, 2048, 4096 or 8192 bytes and equal the i-node size, the
  flags word must say clean or dirty, the allocation group fields must agree with the
  block count, and the root directory run must lead to a live directory i-node. The
  unrelated SCO / UnixWare "Boot File System" (`mkfs.bfs`) is not detected.
- Probed after the NeXT disk label and UFS and before Lisa, HFS, FAT, GPT, MBR and
  ISO 9660, whose signatures the boot area of a volume can imitate.
- The volume is found as a bare image (a partition dump) and inside an MBR partition,
  where it is detected by its content, not by the partition type `0xEB`: the disk opens
  as `ContainerFormat::Mbr` (`dos` feature) and the volume is the partition entry.
- Apple Partition Map discs (BeOS for PowerPC CDs) open as `ContainerFormat::Apm`
  (`macintosh` feature) and the volume is the `Be_BFS` partition entry, next to the
  disc's HFS partition. Without `macintosh` the volume is read only when its partition
  is opened on its own.
- Regular files reachable from the root directory are listed as
  `<image>/<dir>/.../<name>`, sorted by path, with the data stream as content (direct,
  indirect and double-indirect block runs). Empty files are listed. The volume name is
  not part of the path. The Trash is an ordinary directory and is listed.
- Names are taken as UTF-8 as stored: no normalisation and no case folding, so names
  that differ only in case are separate entries. Bytes that are not valid UTF-8 are
  written as `%XX`, so that different stored names stay different. `\`, `:` and control
  characters in a name are replaced by `_`; when that would give two files the same
  path, the changed name keeps those characters as `%XX` instead (`a:b` beside `a_b` is
  listed as `a%3Ab`).
- `Metadata::mode` holds the permission bits as an `ls`-style string (`-rwxr-xr-x`).
  Owner, group and timestamps are not reported.
- Not listed: directories as such (an empty folder leaves no trace), symbolic links
  (never followed), special nodes, deleted files, attribute directories, the index
  directory and its indices.
- Not supported:
  - BeOS attributes, in the i-node's small-data area or in an attribute directory:
    `BEOS:TYPE` and every other attribute are ignored and are neither entries nor
    metadata.
  - Log replay. The volume is read as it is on disk, so an image of a volume that was
    not unmounted cleanly can show the state before its last changes; nothing is
    reported.
  - Directory trees with a node size other than 1024 bytes.
- Damage: a truncated image, or a volume smaller than the image, still opens. Left out
  are files whose block runs leave the volume or the image, cover less than the file
  size (nothing is zero-filled or cut short) or whose size exceeds the volume; entries
  with an empty name, a name over 255 bytes or one containing a zero byte or `/`;
  entries that name a free, deleted, attribute or index i-node; a second entry with a
  path already listed; directories reached a second time (loops and cross-links),
  nested deeper than 128 levels or with an unreadable tree; and the subtree of a damaged
  tree node. File data, paths and 64 bytes per file together may not exceed the image
  size (at least 16 MiB), and neither may the directory trees read; the listing ends
  where the first limit is reached, or when more i-nodes were read than the volume has
  blocks. Every other file is still yielded and the traversal reports one recoverable
  `ContainerEntriesSkipped` diagnostic that counts the skipped items and spells out the
  first. Only a volume that fails the detection checks fails to open.

## Apple Lisa Disk Images

Read-only, `lisa` feature (`ContainerFormat::LisaFs`, extension `.dc42`). Read natively,
also in `no_std` and `wasm32` builds; the reader was written from the format
specification in [docs/lisa/](lisa/README.md).

- Detected by content: one of the first 100 blocks must be tagged as the volume header
  (file ID 1, relative page 0) and pass the header checks (on-disk version 14, 15 or 17,
  512-byte blocks, consistent block counts and S-list geometry). The sector tags are
  required, so an image without tag data is never a Lisa volume.
- Probed before HFS and MFS. A DiskCopy 4.2 image is a Lisa disk only when it holds that
  tagged volume header; otherwise it falls through to the Macintosh reader, so a
  Macintosh disk named `.dc42` still opens as `ContainerFormat::Hfs`.
- Image forms:
  - DiskCopy 4.2 with 12 tag bytes per block (diskettes) or 20 (hard disks). The header
    is checked as described under "HFS Plus and HFSX Volumes"; the checksums are not
    verified.
  - Twiggy diskettes (DiskCopy 4.2 encoding byte `54`, 1702 blocks): the second side is
    put back into logical order, and the drive's bad-block table is applied.
  - DART images of the tagged disk kinds (400K and 800K), with fast (run-length), best
    (LZH) or no compression.
  - Raw ProFile dumps of 532-byte blocks, with or without a BLU header block. The tag
    may come before or after the data, and the blocks may be in logical order or in the
    ProFile's 5:1 interleaved device order; the layout is decided from the tags, not
    from the file name. A dump without a header block must be a multiple of 532 bytes
    and at least 64 blocks long. DiskCopy 4.2 images with 20-byte tags get the same
    block-order test.
- On-disk versions 14, 15 (flat catalog) and 17 (catalog tree).
- Every file the catalog names is an entry, including system files such as `system.os`
  and empty files. Entries are sorted by path. Directories of version 17 volumes become
  path components; a file whose directory chain is broken, loops or is deeper than 16
  levels is listed directly under the root.
- Names are converted from the Lisa character set to UTF-8 and keep their stored case:
  bytes 0x80 to 0xD8 as in Mac Roman, 0x7F as U+2588, bytes below 0x20 and above 0xD8
  as U+FFFD. A `/` inside a name becomes `-`; `\`, `:` and control characters become
  `_`, and a name of `..` becomes `_`. Office System disks show their internal names
  (`{T3}obj`, `{D109T3}`); the desktop names kept in the file labels are not read.
- Entries carry no metadata: file labels, timestamps, file types and passwords are not
  surfaced. Password and protection flags are ignored and the contents are delivered as
  stored.
- All files together may not exceed twice the image size (at least 16 MiB); a decoded
  DART image counts against the same limit.
- Not listed: the boot blocks, volume header, allocation bitmap, S-list and catalog,
  file leaders, deleted entries, directories as such, and anything reachable only
  through tags and not through the catalog.
- Not supported:
  - Images without tag data, 24-byte tags and 536-byte blocks, and truncated images.
  - Lisa Monitor (UCSD p-System) and Xenix disks.
  - MacWorks disks as Lisa volumes: in a DiskCopy 4.2 or DART image they open as
    `ContainerFormat::Hfs` (`macintosh` feature); a raw 532-byte dump of one is not
    opened.
  - The backup volume header: a volume whose primary header is damaged is not
    recognised.
- Damage: left out are files whose S-list entry is stale, whose data pages are tagged
  as another file's, or whose file map has an extent outside the volume; a file whose
  map is shorter than its length, or whose catalog and S-list sizes differ, is cut to
  the smaller. An invalid catalog node ends the listing and keeps the files found so
  far. Once the size limit is reached, or reading unreadable entries has cost more than
  four times the volume's blocks, the remaining entries are not read. Every other file
  is still yielded and the traversal reports one recoverable `ContainerEntriesSkipped`
  diagnostic. A DART image with a chunk that does not decode fails to open.

## ZIP Archives

`common` feature (`ContainerFormat::Zip`).

- Detected by a leading `PK` signature. Archives behind prepended data (self-extractor
  stubs) are found from the end-of-central-directory record, when the name ends in
  `.zip` or no other format matches.
- Stored, deflate, bzip2 (method 12), LZMA (method 14) and XZ (method 95) entries are
  decoded natively, also in `no_std` and `wasm32` builds. An LZMA entry ends at its
  end-of-stream marker when general-purpose flag bit 1 announces one and at its declared
  size otherwise. `Metadata::compression_method` is `deflate`, `bzip2`, `lzma` or `xz`.
  An entry that is encrypted, uses another method, fails its size or CRC-32 check,
  or shares its data with other entries beyond the archive's own length is left out;
  every readable entry is still yielded and the traversal reports one recoverable
  `ContainerEntriesSkipped` diagnostic.
- All central directory headers inside the declared directory size are listed, also
  when the 16-bit entry count is smaller (writers without ZIP64 wrap it at 65536).
- Names: Info-ZIP Unicode Path field, then UTF-8 when flagged or when the bytes are
  valid UTF-8 (macOS and Unix writers omit the flag), otherwise CP437. A backslash is a
  path separator only in entries made on FAT, HPFS or NTFS hosts.

## Bzip2 Files

`common` feature (`ContainerFormat::Bzip2`).

- Detected by the `BZh1`–`BZh9` stream header followed by a block or end-of-stream
  signature, with or without a `.bz2` extension.
- Yields one entry named after the file without its last extension; `.tbz2`, `.tbz` and
  `.tb2` yield `<name>.tar`, which then opens as TAR like the payload of `.tar.bz2`.
- Concatenated streams are decoded as one file and bytes after the last stream are
  ignored. A file with a failing block or stream CRC, or one that is truncated, is
  rejected as a whole.
- Randomised blocks, written by bzip2 0.9.0 and earlier, are decoded.
- bzip2-compressed blocks in UDIF disk images (`hdiutil` format `UDBZ`) are decoded with
  the `macintosh` feature alone.

## Unix compress Files

`common` feature (`ContainerFormat::Compress`). Decoded natively, also in `no_std` and
`wasm32` builds; the reader was written from the format specification in
[docs/compress/](compress/README.md).

- Detected by content, with or without a `.Z` extension: the `1F 9D` magic, a flags byte
  with both reserved bits (`0x20`, `0x40`) clear and a maximum code width of 9 to 16, a
  literal as first code and a second code the format allows. Files with a reserved bit
  set or another width are not recognised.
- Yields one entry named after the file without its last extension; `.taz` yields
  `<name>.tar`, which then opens as TAR like the payload of `.tar.Z`.
  `Metadata::compression_method` is `lzw`.
- Maximum code widths 9 to 16 are decoded, in block mode (with CLEAR codes) and in the
  older format without the block-mode bit. A file of only the three header bytes yields
  an empty entry; a zero-length file has no magic and is not recognised.
- The format carries no checksum, length or end marker, so damage that still decodes is
  not detected. A truncated file yields what the whole codes before the cut decode to,
  without an error. Bytes after the stream are decoded as further codes; left-over bits
  short of one code are ignored.
- A file with an invalid code (a CLEAR code first, a string code with no code before it,
  a code beyond the string table) or with more than 4 GiB of output is rejected as a
  whole, without partial output.
- Not supported: the 9-bit dialect of newer ncompress (`compress -b 9`), which keeps
  9-bit codes once its 512-entry table is full. Maximum width 9 is read the historical
  way (10-bit codes from then on) and there is no fallback to the other reading, so such
  a file decodes correctly only when it ends before the table fills. Files without the
  magic are not recognised.

## XZ and LZMA Files

`xz` feature (`ContainerFormat::Xz`). Decoded natively, also in `no_std` and `wasm32`
builds.

- Detected by the six-byte stream header magic, with or without a `.xz` extension.
- Yields one entry named after the file without its last extension; `.txz` yields
  `<name>.tar`, which then opens as TAR (`common` feature) like the payload of `.tar.xz`.
- Concatenated streams are decoded as one file and stream padding between or after them
  is skipped. Any other bytes after a stream make the file invalid, as they do for the
  `xz` tool.
- The LZMA2 and Delta filters are decoded. The CRC32, CRC64 and SHA-256 integrity checks,
  the header and index CRCs and the index contents are verified; a file failing any of
  them, or one that is truncated, is rejected as a whole.
- Not supported: the BCJ (branch/call/jump) filters for executables (`xz --x86`,
  `--arm64`, ...) and reserved integrity check types.
- Legacy `.lzma` files (`xz --format=lzma`, LZMA Utils) have no magic and are opened
  only under a `.lzma` or `.tlz` extension; `.tlz` yields `<name>.tar` like `.txz`. The
  stream ends at its end marker or at the size in its header, and any byte after it
  makes the file invalid, as it does for the `xz` tool. The format carries no checksum,
  so damage that still decodes is not detected. `Metadata::compression_method` is
  `lzma` for these files and `lzma2` for `.xz`.
- LZMA-compressed blocks in UDIF disk images (`hdiutil` format `ULMO`), which are `.xz`
  streams, are decoded with the `macintosh` feature alone.

## Virtual Path Behavior

Examples with `archive.zip` containing `inner.sit` containing `a` and `b`:

- `archive.zip` -> traverse full archive tree.
- `archive.zip/inner.sit` -> traversal scoped to `inner.sit` subtree.
- `archive.zip/inner.sit/a` -> scoped to one internal target path.
- `disc.toast/iso9660.img/SETUP.EXE` -> a file of the ISO 9660 volume of a hybrid disc.

## CLI Usage

### Primary Commands

```bash
unretro tvf archive.sit.hqx
unretro xvf archive.sit.hqx
unretro --list=json -f archive.sit.hqx
```

### Supported Flags (Current CLI)

- `-t`, `--list[=FORMAT]` where `FORMAT` is `tree` (default), `tsv`, or `json`.
- `-x`, `--extract`
- `-f`, `--file=ARCHIVE`
- `-v`, `--verbose`
- `-C`, `--directory=DIR`
- `--strip-components=N`
- `--numeric`
- `--preserve-permissions`
- `--preserve-resource-fork`
- `--preserve-attributes`
- `--preserve-all`
- `--exec=COMMAND`

`--list=json` nests every resource fork (`"name": "rsrc"`) in the `children` of the file
it belongs to, whichever format supplied the fork.

### Extraction Semantics

Output paths are relative to the virtual path target:

- `xvf archive.zip` extracts from archive root.
- `xvf archive.zip/inner.sit` extracts contents of nested `inner.sit` as root.
- `xvf archive.zip/inner.sit/a` extracts only that file target.

Resource forks are not exploded into per-resource files next to their owner:

- A fork attached to a file (`app/..namedfork/rsrc`) is written as the native fork of
  `app` with `--preserve-resource-fork`, and skipped otherwise.
- A bare resource-fork file or standalone AppleDouble file inside an archive or
  directory is written as the plain file it is.
- Naming such a fork-only file as the archive (`xvf app.rsrc`) extracts its resources as
  `<TYPE>/<name>` files, the same as `xvf app.rsrc/..namedfork/rsrc`;
  `--preserve-resource-fork` has no effect on it.
- A file with an empty data fork and a native macOS resource fork (a font suitcase, for
  example) is fork-only in the same way: `xvf suitcase` extracts its resources, and
  listing it shows them instead of failing with an unsupported-format error.
- `xvf archive.zip/app/..namedfork/rsrc` extracts that fork's resources as
  `<TYPE>/<name>` files; `--exec` receives every individual resource.

Path sanitization is applied for filesystem safety, and conflicting output names are disambiguated.

## Python Usage

Install:

```bash
pip install unretro
```

### Iterate entries

```python
import unretro

for entry in unretro.Loader(path="archive.zip"):
    print(entry.path, entry.size)
    data = entry.read()
```

### Loader construction rules

- Provide exactly one source:
  - `Loader(path="...")`, or
  - `Loader(data=b"...", name="archive.zip")`
- Providing both `path` and `data` raises `ValueError`.
- Providing `data` without `name` raises `ValueError`.

### Loader filters

```python
loader = unretro.Loader(path="disk.img").filter_extension(["mod", "xm"])
loader = loader.filter_path("disk.img/music")
loader = loader.with_numeric_identifiers()  # resources as `<TYPE>/#<id>`
```

### Module-level helpers

```python
import unretro

entry = unretro.open("archive.zip/readme.txt")
print(entry.read())

for dirpath, dirnames, filenames in unretro.walk("archive.zip"):
    print(dirpath, dirnames, filenames)

names = unretro.listdir("archive.zip")
fmt = unretro.detect_format("archive.zip")
```

Behavior notes:

- `open(path)` returns the first matching leaf entry from traversal scope.
- `walk`/`listdir` emit Python warnings for recoverable traversal diagnostics.
- `Loader` iteration, `walk` and `listdir` raise `RuntimeError` for root-level failures (root missing, unreadable, unsupported, or corrupt).
- `open` raises `FileNotFoundError` when the root path or entry does not exist, `OSError` when the root exists but cannot be read, and `RuntimeError` when the root is unsupported or corrupt.

## Operational Caveats

- Resource fork preservation (`--preserve-resource-fork`) is macOS/filesystem dependent.
- Attribute preservation may warn if destination filesystem does not support requested metadata.
- Use `with_mmap(MmapStrategy::Never)` for network filesystems if page-fault I/O patterns are undesirable.

## Where to Go Next

- Architecture details: [ARCHITECTURE.md](ARCHITECTURE.md)
- Contribution workflow: [../CONTRIBUTING.md](../CONTRIBUTING.md)
- Python package details: [../crates/unretro-python/README.md](../crates/unretro-python/README.md)
