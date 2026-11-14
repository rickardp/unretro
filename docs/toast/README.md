# Toast disc images (`.toast`): specification and findings

This document specifies what a file with the extension `.toast` can hold,
what this repository is required to do with each kind, and which of those
requirements it did not meet when the document was written (section 10,
"Gaps found", with what was done about each).

A Toast image is not a container format of its own. It is a dump of the
sectors of one disc or one track, written by Toast (Astarte, later Adaptec,
Roxio, Sonic, Corel) or by another tool and given Toast's extension. All
of its structure is that of the file systems and partition map inside it,
which are specified elsewhere (`docs/USAGE.md`, sections "HFS Plus and HFSX
Volumes", "Apple Partition Map Disk Images", "ISO 9660 CD-ROM Images" and
"UDF Disc Images"). What this document adds is the list of layouts that
occur under the extension, how to tell them apart, and the sector framings
that can sit between the file and those structures.

The document contains no program code and no pseudo-code. Commands quoted
in section 10 are reproductions, not implementation.

## 1. Purpose and provenance

- This is a **prose specification** with measurements. It was derived from
  vendor documentation, from reading the source of two open tools for
  facts about their behaviour, from measurements on 126 real `.toast`
  files of 77 Internet Archive items, from running this repository's
  command-line tool on thirteen of them downloaded in full and on
  purpose-built images, and from comparing its output with reference
  tools.
- The work is a **clean-room process**. The writer of this document could
  read existing implementations; code written from it is written **from
  this document and the documents it names only**.
- Real disc images are never committed. They, the sampled byte ranges,
  the purpose-built images and the scripts that made them are kept
  outside the repository under `~/.cache/unretro-specimens/toast/`
  (`NOTES.md` there lists URLs, sizes and digests; `PROBES.tsv` has one
  line per sampled file).
- No image was mounted or attached at any point. Purpose-built images
  were written to files by `hdiutil makehybrid`, by hfsutils and
  genisoimage in a container, and by scripts.
- No Rust source was changed for this document.

### 1.1 Evidence labels

- **MEASURED (full)**: observed on a specimen downloaded in full
  (section 8.1).
- **MEASURED (sampled)**: observed on the first and last 655,360 bytes of
  a specimen, plus the volume header of each HFS partition, fetched with
  HTTP range requests (section 8.2).
- **CONFIRMED (crafted)**: observed by running this repository's
  command-line tool, and a reference tool where one can read the image, on
  a purpose-built image (section 8.3).
- **VENDOR**: stated by Roxio or Corel documentation.
- **REFERENCE-ONLY**: stated by the source of an existing tool, not
  observed on a specimen. The tool is named.
- **REPORTED**: stated by users in a public forum, not verified.
- **INFERRED**: deduced, unconfirmed.
- **DECISION**: chosen by this specification (listed in section 12).

### 1.2 Terms

- **Cooked sector**: the 2048 user bytes of a data sector, without sync
  pattern, header, sub-header or error correction.
- **Raw sector**: all 2352 bytes of a sector as read from a CD.
- **Block**: a 512-byte unit of an Apple Partition Map, unless the map is
  said to use 2048-byte units.
- **Hybrid**: one image with both an HFS or HFS Plus volume and an
  ISO 9660 volume.
- **Bridge**: one image with both an ISO 9660 and a UDF volume over the
  same files.

## 2. Overview

| # | Variant | Share of the 126 sampled files | Evidence | Opens correctly today |
|--:|:--|--:|:--|:--|
| 1 | Cooked sectors, Apple Partition Map written by Toast (`MRKS`), HFS | 42 files have a map and HFS without ISO; 66 of all 114 maps are Toast's | MEASURED (full), MEASURED (sampled) | yes |
| 2 | Cooked sectors, Apple Partition Map, HFS Plus | 28 | MEASURED (full), MEASURED (sampled) | yes |
| 3 | Cooked sectors, bare HFS or HFS Plus volume without a map | 7 (6 HFS, 1 HFS Plus) | MEASURED (full), MEASURED (sampled) | yes |
| 4 | Cooked sectors, ISO 9660 only (with Joliet in all cases seen) | 4 | MEASURED (full) | yes |
| 5 | Hybrid, shared: the ISO 9660 volume space covers the HFS partition | 35 | MEASURED (full), MEASURED (sampled) | yes, both sides, one tree of the ISO 9660 volume: a file that only the Joliet tree of a Rock Ridge volume names is not listed (gap G3, fixed) |
| 6 | Hybrid, concatenated: the ISO 9660 volume ends before the HFS partition starts | 9 | MEASURED (full), MEASURED (sampled) | yes, both sides, with the same limit (gap G3, fixed) |
| 7 | UDF bridge (ISO 9660 + Joliet + UDF) | 1 | MEASURED (full) | yes, as ISO 9660 |
| 8 | UDF only | 0 | CONFIRMED (crafted) | yes |
| 9 | Partition map in 2048-byte units (DVD-sized Mac OS X install discs) | 6 | MEASURED (sampled), CONFIRMED (crafted) | yes (memory: see gap G5) |
| 10 | Raw 2352-byte sectors, Mode 1 or Mode 2 | 0 | REFERENCE-ONLY (Aaru), CONFIRMED (crafted) | yes |
| 11 | 2056-byte sectors (CD-ROM XA: sub-header and user data) | 0 | REFERENCE-ONLY (Aaru), CONFIRMED (crafted) | yes (gap G1, fixed) |
| 12 | 2336-byte sectors (Mode 2 without sync and header) | 0 | REFERENCE-ONLY (Aaru, disabled there; libmirage for `.iso`) | yes (gap G2, fixed) |
| 13 | Several tracks, or audio, in one `.toast` file | 0 | none found; VENDOR and REPORTED say Toast writes these as other files | not applicable (section 3.10) |
| 14 | Header, trailer, compression or segmentation added by Toast | 0 | MEASURED: absent in all 126 | not applicable |
| 15 | The image inside an archive (`.toast.sit`, `.toast.zip`) | 31 archives seen beside the 126 files | MEASURED (full) for one of each | yes |

Variants 1, 2, 3, 5 and 6 add up to the 121 sampled files that hold an HFS
or HFS Plus volume (variant 9 is counted within 2 and 6); variants 4 and 7
are the other five.

Every one of the 126 sampled files is a whole number of 2048-byte sectors
and starts with file-system data at byte 0. Variants 10 to 12 are
therefore rare at best: they are documented from tools that accept them,
not from a specimen.

## 3. The variants

### 3.1 Properties common to all cooked images

- The file is the user data of the sectors of one data track, in order,
  2048 bytes each, starting at the first sector of the track. There is no
  header before sector 0 and no trailer. MEASURED on 126 of 126 files:
  the size is a multiple of 2048 in every one, and none carries a UDIF
  (`koly`) trailer.
- Toast adds nothing of its own inside the image except what it writes as
  the author of the file systems: its name in the partition map
  (section 3.2) and in the ISO 9660 application identifier (section 3.5).
  MEASURED.
- Many images end in zero-filled sectors after the last volume. Of the
  126 files, 60 end in a run of 148 to 155 zero sectors and 21 in a run of
  about 300; 150 sectors are two seconds of disc time, the run-out that
  follows a track. 39 end in fewer than 13 zero sectors. MEASURED
  (sampled). In `xircom/PORTGEAR201_Mac.toast` the HFS partition ends at
  byte 15,412,224 and the file is 15,720,448 bytes long: 150.5 sectors of
  zeros follow the last partition. MEASURED (full). A reader must not
  require the file to end where the volumes end.
- On a Macintosh the file carries the creator `CDr3` and a type that names
  the content: `GImg` on an ISO 9660-only image and `HImg` on a
  partition-map image with HFS. MEASURED (full) on one file each, kept in
  a ZIP and a StuffIt archive that preserve Finder metadata. These codes
  are not in the data fork and are not used for detection. Neither
  archived copy has a resource fork.
- Vendor documentation describes the format only as "ISO image files with
  a .toast file extension" that can be renamed to `.iso`. VENDOR (Corel
  knowledge base 129238). That is true of variants 4, 7 and 8 and of the
  ISO side of a hybrid, and misleading for the Mac volume images that make
  up most of what exists.

### 3.2 Mac volume behind Toast's partition map

The commonest layout. MEASURED (full) on three specimens, MEASURED
(sampled) on 66 maps.

- Block 0 is a driver descriptor record: `ER`, block size 512, the block
  count of the image, and usually no drivers. The rest of the block is
  zero.
- The partition map starts at block 1 and has two entries, both with a
  map entry count of 2:
  - type `Apple_partition_map`, name `MRKS`, start 1, length 2 (length 35
    in maps written by Toast 9 and later);
  - type `Apple_HFS`, name `Toast <version> <component>` or
    `TOAST 2.5 Partition`. Names seen run from `TOAST 2.5 Partition` to
    `Toast 12 HFS+ Builder`; the components are `Partition`,
    `HFS Builder`, `HFS Optimizer`, `HFS+ Builder`, `HFS/Joliet Builder`
    and `HFS+/Joliet Builder`, with `PPC` before the component in some
    3.x and 4.0 versions. The name field is 32 bytes, so long names are
    cut (`Toast D-5.0.1 HFS/Joliet Builde`).
- The volume partition is the Mac volume: HFS when the component says
  `HFS`, `Partition` or `Optimizer`, HFS Plus when it says `HFS+`. The
  type string is `Apple_HFS` in both cases. No HFS wrapper around an
  embedded HFS Plus volume was seen: the HFS Plus header is at byte 1024
  of the partition. MEASURED (sampled) on 23 `HFS+` maps.
- The partition does not start on a 2048-byte boundary in general. Start
  blocks seen for non-hybrid images are 9, 36, 37 and 39; the blocks
  between the map and the partition belong to no entry. In hybrids the
  start block is wherever the Mac volume was placed (19,814 in the Xircom
  specimen, which is byte 10,144,768, half way into a sector). A reader
  must address the map in 512-byte blocks and must not round to sectors.
- Images of bootable discs made with Toast 5.2.3 keep Toast's `MRKS` map
  but declare 2048-byte device blocks in the driver descriptor record
  and add Apple's driver and patch partitions (`Apple_Driver43`,
  `Apple_Driver43_CD`, `Apple_Driver_ATAPI`, `Apple_Patches`,
  `Apple_Extra`); the map itself is still in 512-byte blocks. MEASURED
  (sampled) on two.

### 3.3 Other partition maps found under the extension

Images of pressed discs, and images made by other programs, carry the map
of their origin. MEASURED (sampled) unless noted.

| Map entry name of the map / of the volume | Origin | Files |
|:--|:--|--:|
| `Apple` / `disk image`, with `Apple_Free` at the end, map of 63 blocks | Apple DiskImages (`hdiutil`), HFS Plus | 16 |
| `Apple` / `Pinnacle RCD` | Pinnacle RCD | 9 |
| `Quick TOPiX by OMI` or `QuickTOPiX II by OMI` / the volume's name | Optical Media International mastering | 6 |
| `Apple` / `Mac_OS_X`, `MacOS`, `Macintosh HD`, most with driver partitions | Apple and other pressed discs | 8 |
| `CDEveryWhere 2.0 Part. Map` / `CDEveryWhere Hybrid Volume` | CDEveryWhere | 2 (MEASURED (full)) |
| empty names, with a partition of type `ISO` at blocks 4 to 78 | Apple hybrid mastering | 2 (one MEASURED (full)) |
| `Apple` / `DiscRecording 2.0.10f2`, `Toy Story 2` | Apple DiscRecording and others | 2 |
| `DiskImages-generated` / `DiskImages` | Apple DiskImages | 1 |
| `auto-generated map` / the name of a `.dmg` | conversion from a disk image | 1 |
| `Apple` and nine further entries, two `Apple_HFS` and one `Apple_Rhapsody_UFS` | Darwin 0.3 | 1 |

Facts a reader needs from this table:

- **Driver descriptor block size and map unit are independent.** 15 files
  declare 2048-byte device blocks. In 9 of them the map entries are 512
  bytes apart starting at byte 512 and count in 512-byte blocks; in 6
  (all Mac OS X install DVD images of 4 GB and more) the entries are 2048
  bytes apart starting at byte 2048 and count in 2048-byte blocks. The
  rule in `docs/USAGE.md` ("Apple Partition Map Disk Images": 512-byte
  blocks or the declared size, whichever holds the first entry) covers
  both. CONFIRMED (crafted) for both; the six real maps open on sparse
  reconstructions.
- An image can hold more than one volume partition (Darwin 0.3: two HFS
  volumes and a UFS partition).
- Partition names are free text, can contain `/` and non-ASCII
  characters (`MacHack™94`), and can be empty.

### 3.4 Bare volume without a map

Seven files start with two boot blocks and the HFS master directory block
(six) or the HFS Plus volume header (one) at byte 1024, with no driver
descriptor record. MEASURED (full) on
`shanghai-great-moments/ShanghaiGreatMoments.toast`, MEASURED (sampled) on
the rest. These are what Toast's "Mac Volume" source and its copy of a
mounted volume produce when no map is written; they are indistinguishable
from a `.dsk` or `.hfs` volume image of the same size.

### 3.5 ISO 9660 only

Four files have a primary volume descriptor at sector 16, a Joliet
supplementary descriptor, and neither a partition map nor an HFS
signature. MEASURED (full) on two, with content checked. Toast names
itself in the application identifier of the ISO volumes it writes, in
hybrids too: `TOAST ISO 9660 BUILDER COPYRIGHT (C) <years> <owner>` with
the owners Miles Software (1993-1996), Adaptec (1997), Roxio (1997-2002,
1997-2003) and Sonic (1997-2005); 33 of the 49 ISO volumes sampled carry
it. The system identifier of 39 is `APPLE COMPUTER, INC., TYPE: 0002`.
Neither string is needed to read the volume.

### 3.6 Hybrids

44 of the 126 files have both a Mac volume in a partition map and an
ISO 9660 volume at sector 16. No hybrid without a partition map was found
among the real files; genisoimage writes such images (HFS master directory
block at byte 1024, no map) and they must be expected. CONFIRMED (crafted).

Two layouts exist, told apart by the volume space size in the primary
volume descriptor:

- **Shared** (35 files): the ISO volume space runs to the end of the Mac
  partition or of the file. The ISO directories point at extents inside
  the HFS partition, so a file present on both sides is stored once.
  MEASURED (full) on `arple-11-94-technical/Technical.toast`: every one of
  the 2053 files of the ISO tree has the same content as a data fork of
  the HFS Plus tree, which has two more (the desktop database files).
- **Concatenated** (9 files): the ISO volume space ends at or before the
  first block of the Mac partition. The two file systems share nothing.
  MEASURED (full) on `xircom/PORTGEAR201_Mac.toast`: the ISO volume is
  10,143,744 bytes, the HFS partition starts 1024 bytes later, the ISO
  tree has 533 files and the HFS tree 360, and 335 of the ISO files
  (Windows drivers) have content found nowhere on the HFS side. The two
  Mac OS X 10.6 install DVD images (8.4 GB) are of this kind too: their
  ISO 9660 volume (with Joliet and Rock Ridge) fills the first 850 to 935
  MB, which the partition map declares as one `Apple_Driver_ATAPI`
  partition named `Macintosh`. MEASURED (sampled).

Either layout can also hide files from one side: a file can be listed on
the ISO side only or on the Mac side only. CONFIRMED (crafted) with
`hdiutil makehybrid -hide-hfs` and `-hide-iso`. A shared hybrid therefore
does not guarantee that the Mac tree is a superset.

Apple's own hybrid mastering lists the ISO descriptors as a partition of
type `ISO` (blocks 4 to 78, that is sectors 1 to 19). The partition holds
descriptors only; it is not an ISO 9660 image by itself, because its
directories are addressed from the start of the disc. MEASURED (full).

### 3.7 UDF and bridge images

One real file is a bridge: ISO 9660 with Joliet, and a UDF volume
(`BEA01`, `NSR02`, `TEA01` after the ISO descriptors, anchor at sector
256). MEASURED (full) on `medialas-mamba-2/MedialasMamba2.toast`; its 40
files match the reference. Toast writes this layout for DVD projects
(VENDOR: "create disc images from any Toast disc type"; Corel's note on
video disc images). No UDF-only `.toast` was found; the layout is what
`hdiutil makehybrid -udf` writes and is specified for completeness.
CONFIRMED (crafted).

No real DVD-Video or Blu-ray `.toast` was located. INFERRED: they are
bridge or UDF-only images like any other DVD image.

### 3.8 Raw 2352-byte sectors

Aaru accepts a `.toast` file whose length is a multiple of 2352, is at
most 846,720,000 bytes and starts with the 12-byte sync pattern, and
reads it as raw sectors. REFERENCE-ONLY (Aaru, `ZZZRawImage`). No
specimen was found. The sector layouts are those of
[`docs/cue/cue.md`](../cue/cue.md), section 6.2; cooking follows its
section 6.4, with the sector's own mode byte choosing between user data
at byte 16 (Mode 1) and byte 24 (Mode 2).

### 3.9 CD-ROM XA framings: 2056 and 2336 bytes

- **2056 bytes per sector.** Aaru accepts a `.toast` file whose length is
  a multiple of 2056 and reads each sector as an 8-byte sub-header
  followed by 2048 user bytes; it calls the layout "Toast XA".
  REFERENCE-ONLY (Aaru). This is bytes 16 to 2071 of a raw Mode 2 Form 1
  sector: the sub-header (four bytes, then the same four repeated) and the
  user data, without sync, header, error detection or correction. It is
  the framing a CD recorder accepts for writing an XA track. INFERRED:
  Toast writes it when it saves a CD-ROM XA track (its "Multitrack CD-ROM
  XA" and CD-i/Video CD disc types); no specimen was found.
- **2336 bytes per sector.** Bytes 16 to 2351 of a raw Mode 2 sector:
  sub-header, 2048 user bytes, 280 bytes of error detection and
  correction (Form 1), or sub-header and 2324 user bytes (Form 2). This
  is the `MODE2/2336` layout of `docs/cue/cue.md`, section 6.3. Aaru has
  the rule for `.toast` written but disabled; libmirage accepts 2332- and
  2336-byte sectors with user data at offset 8 for bare images.
  REFERENCE-ONLY. No specimen was found.

### 3.10 Several tracks, audio, and what Toast writes instead

No `.toast` file with more than one track, with audio, or with a track
table was found, and nothing in the 126 files suggests a place where a
track table could be. What the sources say:

- Toast saves the image of an audio CD as a Sound Designer II file
  (`.Sd2f`), not as `.toast`. VENDOR (Corel knowledge base 128747),
  REPORTED. Two such files were found: both are a whole number of
  2352-byte audio sectors (277,007 and 283,079), and the one sampled has
  no header or trailer in its data fork and holds big-endian 16-bit
  samples. MEASURED (sampled). INFERRED: the track boundaries are in the
  resource fork, which the Internet Archive copies do not have.
- Toast 3.5.7 refuses to copy a mixed-mode disc to an image. REPORTED.
- From version 6 on, Toast reads and writes BIN/CUE pairs. VENDOR (Toast
  6 manual), REPORTED. Two items keep a `.bin`/`.cue` pair beside the
  `.toast` of the same disc. BIN/CUE is specified in
  [`docs/cue/`](../cue/README.md).
- The Toast 6 manual lists "Multitrack CD-ROM XA" (several XA tracks in
  one session) as a disc type. How an image of it is saved was not
  established.

A `.toast` file is therefore one data track. INFERRED from the above.

### 3.11 Compression, segmentation, archives

Toast does not compress or split its images: none of the 126 files has
anything but sector data. MEASURED. Compression is added from outside:
the items searched hold 31 archives named `.toast.sit`, `.toast.zip`,
`.toast_.sit` or `.toast_.zip` beside or instead of the bare image. One StuffIt 5 archive
(Arsenic method) and one ZIP were downloaded and hold ordinary images of
variants 5 and 4: `USB2Connect.toast` in the StuffIt archive is a Toast
shared hybrid (map entry `Toast 5.2.3 HFS/Joliet Builder`, an ISO 9660
volume space of 22,151 sectors that is the whole 45,365,248-byte image).
MEASURED (full).

## 4. Recognising the content

The extension says nothing about the content. **DECISION:** `.toast` is
an ordering hint only, and every decision is made from the bytes.

The required order of probes for a file named `.toast`, and for the same
bytes under any other name or inside another container:

1. A Lisa disk in a DiskCopy 4.2 wrapper, an Apple Partition Map, a bare
   HFS, MFS or HFS Plus volume (the Mac probes, as for `.dsk` and `.hfs`).
2. If none matches, the general probe sequence, which reaches ISO 9660
   and UDF after the Mac, FAT, GPT and MBR probes. An image with an ISO
   9660 volume and a UDF volume opens as ISO 9660 unless the ISO volume
   has no readable root directory (`docs/USAGE.md`, "UDF Disc Images").

Consequences that must hold, whatever the extension and at any nesting
depth:

- An ISO 9660-only image opens as ISO 9660, a UDF-only image as UDF, a
  bridge image as ISO 9660. The Mac hint must not make them fail or open
  as something else.
- An image with a partition map opens as the partition map. An image with
  a bare Mac volume and no map opens as that volume.
- A hybrid opens as its partition map or Mac volume, and its ISO 9660
  tree must be reachable as well (section 5.3).

Each probe runs on the cooked view of the image. The cooked view is
found as follows:

- The file itself, when a probe matches it as it is.
- Otherwise, when the length is a multiple of 2352 and the leading
  sectors carry the sync pattern and a mode byte of 1 or 2: the user data
  of each raw sector (section 3.8).
- Otherwise, when the length is a multiple of 2056 or of 2336: the 2048
  bytes at offset 8 of each sector, accepted only under the conditions of
  section 6. **DECISION.**

## 5. Required behaviour

### 5.1 Per variant

| Variant (section 2) | Required result | Specified in |
|--:|:--|:--|
| 1, 2, 9 | `Apm` at the top; one entry per listed partition; the Mac partition opens as `Hfs` with data forks, resource forks and Finder type and creator. Driver, patch and `ISO` partitions are opaque entries. | `docs/USAGE.md`, "Apple Partition Map Disk Images", "HFS Plus and HFSX Volumes" |
| 3 | `Hfs` at the top. | same |
| 4 | `Iso9660` at the top; Joliet names when present. | `docs/USAGE.md`, "ISO 9660 CD-ROM Images" |
| 5, 6 | As variants 1 to 3 for the Mac side, unchanged from today; in addition every file of the ISO 9660 tree is reachable (section 5.3). | this document |
| 7 | `Iso9660` at the top; `Udf` only when the ISO volume has no readable root. | `docs/USAGE.md`, "UDF Disc Images" |
| 8 | `Udf` at the top. | same |
| 10 | As the cooked image with the same content. | `docs/cue/cue.md`, section 6 |
| 11, 12 | As the cooked image with the same content (section 6). | this document |
| 13 | Nothing: `.Sd2f` files are not containers and are not detected as anything. A `.toast` with several tracks is not specified until a specimen exists (section 11). | this document |
| 15 | The archive lists the image as an entry, and the entry opens as above. | `docs/USAGE.md` |

### 5.2 Rules that follow from the measurements

- Bytes after the last volume are ignored. A file longer than its
  partition map's block count, or than its ISO volume space, is normal
  (section 3.1).
- Partition offsets and lengths are taken in the unit the map uses
  (512 or 2048 bytes), never rounded to sectors (sections 3.2, 3.3).
- A partition named with `/`, `:`, `\`, a non-ASCII character or nothing
  gets the positional name `p<n>.img`, as `docs/USAGE.md` already says.
  `Toast 7.0 HFS+/Joliet Builder` is such a name: all images written by
  Toast's `HFS/Joliet` and `HFS+/Joliet` builders list their volume as
  `p1.img`. That is accepted. **DECISION.**
- A file of a volume is a file. A data fork is opened as a nested
  container only when a probe identifies it with the strictness the probe
  would need on unrelated data; a fork that a loose probe happens to
  accept must stay a plain file (gap G4 shows 427 violations on two real
  images).
- Reading a 4 to 9 GB image must not need a multiple of its size in
  memory (gap G5). **DECISION.**

### 5.3 Hybrids: the ISO 9660 side

**DECISION:** both trees of a hybrid are part of the image and both must
be reachable. The Mac tree stays the primary presentation, with the paths
it has today, because it has the long names, the resource forks and the
Finder metadata, and because existing users depend on those paths.

The ISO 9660 volume is to be presented in addition, as one more
partition entry beside those of the map, named `iso9660.img`, which then
opens as ISO 9660 like any partition opens as what it holds. An image
without a map has no partition list; there the entry sits at the root of
the Mac volume under the reserved name `..iso9660.img`, in the manner of
the reserved `..eltorito/` directory of the ISO 9660 reader.
Requirements:

- The entry holds the whole cooked image with sectors 0 to 15 (the ISO
  9660 system area, where the partition map and the Mac volume header
  are) blanked, so that it is not taken for the map or the Mac volume
  again. It cannot be shorter: ISO 9660 addresses count from the start
  of the disc.
- The entry exists exactly when the image has an Apple Partition Map or a
  bare Mac volume and also passes the ISO 9660 probe with a readable root
  directory. It does not exist for an ISO 9660-only image (which opens as
  ISO 9660 directly) and it is not derived from a partition of type
  `ISO`, which may be absent and holds descriptors only.
- Its content is what the same bytes would list when opened as ISO 9660:
  same names, same Joliet and Rock Ridge rules, addressed from the start
  of the image.
- In a shared hybrid most files then appear twice, once on each side.
  That is accepted; the bytes are stored once in the image.
- The UDF tree of an image that has all three (`hdiutil makehybrid -hfs
  -iso -joliet -udf`) is not presented separately; it describes the same
  files as the ISO tree.

Implemented (section 10, G3): the loader wraps the partition-map or HFS
container of a hybrid (`src/formats/hybrid.rs`) and lists the entry
after its entries. The two readers are unchanged. The entry is also
absent when the ISO 9660 root lists no file. An earlier revision listed
the ISO 9660 tree under a directory `..iso9660/`; the partition entry
replaced it.

The name is reserved against the Mac side as well, compared without
regard to letter case: a file or folder of that name at the top of the
Mac volume, and everything below it, is not listed and is reported as
skipped, and paths that start with the name are looked up on the ISO
9660 side only. No path is listed twice with different content.

## 6. The 2056- and 2336-byte framings

For a file of *S*-byte sectors, *S* being 2056 or 2336, sector *n* is the
*S* bytes at offset *n* × *S*, and its parts are:

| Offset in the sector | Size | Field |
|--:|--:|:--|
| 0 | 4 | Sub-header: file number, channel number, sub-mode, coding information. |
| 4 | 4 | The same four bytes repeated. |
| 8 | 2048 | User data (Form 1). |
| 2056 | 280 | Only when *S* is 2336: error detection and correction of a Form 1 sector, or the rest of the 2324 user bytes of a Form 2 sector and its 4 trailing bytes. |

The cooked view is the 2048 bytes at offset 8 of every sector, in order.
A Form 2 sector (sub-mode bit `20` hexadecimal set; only possible when
*S* is 2336) contributes its first 2048 user bytes, as in
`docs/cue/cue.md`, section 6.4.

Such a file has no sync pattern to recognise it by. **DECISION:** it is
accepted as an *S*-byte image only when all of these hold:

- no probe matched the file as cooked data or as raw 2352-byte sectors;
- the length is a non-zero multiple of *S* and at least 17 sectors;
- in each of the sectors 0 to 16, bytes 0 to 3 equal bytes 4 to 7;
- the cooked view passes one of the Mac or optical probes of section 4;
- in an image of more than 320 sectors, the cooked view of sectors 0 to
  319 alone already shows a volume: a partition map with a partition
  inside the whole image, a bare Mac volume, ISO 9660 or UDF;
- the cooked view is within the repository's limit for a single output
  (4 GiB).

The last two conditions bound the cost of the test. Whether a file with
no signature is such an image is asked of every file nothing else
claims, and the answer must not need a copy of all of it: the leading
sectors are cooked first (640 KiB at most), and the whole image only
once they have shown a volume. 320 sectors reach past the UDF anchor at
sector 256. A volume that only a structure near the end of the image
gives away is therefore not found in these framings.

2056 is tried before 2336. A length can be a multiple of 2048 and of
2056 or 2336 at once; the first condition gives cooked data precedence.

The evidence for these framings under the `.toast` extension is one
tool's behaviour (section 3.9). The requirement stands, at lower priority
than the gaps that real files show.

## 7. What was verified to work

Run with the release build of the command-line tool of this repository's
working tree on 2026-11-13.

On real files downloaded in full (section 8.1):

- ISO 9660 only: two images, 3 files, all identical to `7zz` extraction.
- Bridge: 40 of 40 files identical to `7zz` (which reads the ISO side).
- Toast partition map with HFS: `Apple Business Server Kit (1996).toast`
  (`TOAST 2.5 Partition`), 613 of 613 files identical to the machfs
  reference in data fork, resource fork and type/creator;
  `PORTGEAR201_Mac.toast`, 358 of 358. A file whose type and creator
  are both four zero bytes (the `Icon` files with a carriage return in
  their name: 19 in this image, 4 in `DemoCD(Hypercard).toast`, 1 in
  `ShanghaiGreatMoments.toast`) is listed without type and creator
  instead of with zero codes; its forks are identical.
- Bare HFS: `ShanghaiGreatMoments.toast`, 1039 of 1039.
- Partition map with HFS Plus: `Age of Empires II NoCD.toast`, 9 of 9
  data forks identical to `7zz`; the journal and the private directories
  are left out as documented.
- CDEveryWhere hybrids: 2 of 2 and 3 of 3.
- In all of these the tool also lists the desktop database files, which
  machfs leaves out.
- Nested: the ISO-only image inside a ZIP and the shared hybrid inside
  a StuffIt 5 archive open as `Iso9660` and as `Apm` with `Hfs`; the
  hybrid lists its 10 Mac files and, under `iso9660.img/`, the 10 files of
  its Joliet tree.
- The counts that `tests/toast_fixtures.rs` asserts for the Mac side are
  the files this tool lists: the reference's files, those with two empty
  forks included, plus the desktop database files.

On sparse reconstructions of the sampled files (head, volume headers and
tail in place, zeros elsewhere): all 126 open with the expected top-level
format (114 `Apm`, 7 `Hfs`, 5 `Iso9660`), every Mac partition is
recognised as `Hfs`, and both map units are handled.

On purpose-built images (section 8.3):

- `iso-only.toast` opens as `Iso9660`, `udf-only.toast` as `Udf`,
  `bridge.toast` as `Iso9660`, `hybrid.toast`, `hybrid-udf.toast` and
  `hfs-only.toast` as `Apm` with `Hfs`, `hfs-bare.toast` as `Hfs`; every
  source file that the opened side lists is extracted intact.
- The same holds for the raw 2352-byte Mode 1 and Mode 2 versions of
  each, and for the partition-map images with a 2048-byte driver
  descriptor block size and maps in 512-byte and in 2048-byte units.
- The same images inside a ZIP, a TAR and a gzip-compressed TAR open as
  the same formats.

## 8. Specimens

### 8.1 Downloaded in full

Paths are under `~/.cache/unretro-specimens/toast/full/`. All are from
`https://archive.org/download/<item>/<file>`; item names and SHA-256
digests are in `NOTES.md` there.

| Bytes | Path | Layout |
|--:|:--|:--|
| 3,049,472 | `personal-ear-trainer/Personal Ear Trainer.toast` | ISO 9660 + Joliet |
| 2,601,662 | `personal-ear-trainer-zip/Personal Ear Trainer.toast.zip` | ZIP holding the image above |
| 3,627,008 | `writer-tools-2.0/WriterTools1-1.toast` | CDEveryWhere shared hybrid, HFS |
| 4,378,624 | `writer-tools-2.0b/WriterTools 2.0.toast` | CDEveryWhere shared hybrid, HFS |
| 8,480,768 | `medialas-mamba-2/MediaLas License.toast` | ISO 9660 + Joliet |
| 9,582,592 | `aoe2-nocd/Age of Empires II NoCD.toast` | DiskImages map, HFS Plus |
| 15,720,448 | `xircom/PORTGEAR201_Mac.toast` | Toast concatenated hybrid, HFS (`Toast 4.0 PPC Partition`) |
| 43,452,529 | `usb-2-connect/USB2Connect.toast_.sit` | StuffIt 5 archive holding a Toast shared hybrid, HFS (`Toast 5.2.3 HFS/Joliet Builder`) |
| 69,308,416 | `hypercard-demo/DemoCD(Hypercard).toast` | Apple shared hybrid with an `ISO` partition, HFS |
| 105,508,864 | `medialas-mamba-2/MedialasMamba2.toast` | UDF bridge |
| 137,728,000 | `apple-business-server-kit/Apple Business Server Kit (1996).toast` | Toast map, HFS (`TOAST 2.5 Partition`) |
| 163,452,928 | `arple-11-94-technical/Technical.toast` | Toast shared hybrid, HFS Plus (`Toast 7.0 HFS+/Joliet Builder`) |
| 173,015,040 | `shanghai-great-moments/ShanghaiGreatMoments.toast` | bare HFS |

### 8.2 Sampled by range request

126 files (the eleven bare images above included) from 77 items, found by
searching the Internet Archive's item metadata for Toast images of
Macintosh software and listing the files of each hit. For each file the
first and the last 655,360 bytes were fetched, and the 512 bytes of the
volume header of every `Apple_HFS` partition that starts beyond the
first range. Measured from them: size modulo 2048, 2352, 2336, 2056 and
512; sync pattern; driver descriptor record; every partition map entry;
the kind of volume in each partition and at byte 0; ISO 9660 descriptors
at sector 16 for the 2048-, 2352-, 2336- and 2056-byte framings; the
volume recognition sequence and the UDF anchor at sector 256; a UDIF
trailer; the length of the zero run at the end. The ranges, the
measurements and the per-file table are under
`~/.cache/unretro-specimens/toast/probes/` and in `PROBES.tsv`.

The sample is biased toward Macintosh software discs, where the search
terms point. It says little about how common DVD-Video images are.

### 8.3 Purpose-built

Under `~/.cache/unretro-specimens/toast/synthetic/`, with the recipes in
`NOTES.md`: `hdiutil makehybrid` images for ISO only, UDF only, bridge,
hybrid, hybrid with UDF, hybrid with files hidden from either side, and
HFS Plus only; a classic HFS volume made with hfsutils; genisoimage
hybrids without and with a partition map; partition maps with each
combination of declared block size and map unit; and every one of these
re-sectored as 2352-byte Mode 1, 2352-byte Mode 2, 2336-byte and
2056-byte sectors.

## 9. Sources consulted

| Source | Kind | Licence / status |
|---|---|---|
| Corel knowledge base 129238, "Roxio Toast 20 - Saving your video disc project as a disc image (.toast) file", and 128747, "Toast 19 and later no longer mounts SD2F files" (as summarised by a web search) | vendor documentation | proprietary, quoted for facts only |
| Toast 19 help, "Saving Disc Images" (`help.corel.com/toast/v19/en/official-help/toast/h2-saving-disc-images.html`) | vendor documentation | proprietary, quoted for facts only |
| Roxio Toast 6 user manual, pages on advanced copy features, as mirrored at `manualsdir.com/manuals/199966/roxio-toast-6.html` | vendor documentation | proprietary, quoted for facts only |
| Aaru Data Preservation Suite, `Aaru.Images/ZZZRawImage/Identify.cs`, `Open.cs`, `Read.cs` (development branch as fetched in November 2026) | tool source, read for facts | LGPL-2.1 |
| libmirage (CDemu), `images/image-iso/parser.c` (table of accepted sector sizes) | tool source, read for facts | GPL-2.0-or-later |
| TinkerDifferent forum thread "Archiving CD images using Toast 3 (1997)"; GearSpace, AppleInsider and Command-Tab posts on Toast audio and mixed-mode images (as summarised by web searches) | user reports | public forum posts |
| fileinfo.com and docs.fileformat.com entries for `.toast` (as summarised by a web search) | documentation | public |
| Internet Archive search and item metadata APIs; 126 `.toast` files and one `.Sd2f` file of 78 items, thirteen files in full and the rest by range request | measurements on real data | the images are not redistributed |
| 7-Zip 26.01 (`7zz`), xorriso, machfs (Python), hfsutils and genisoimage (Debian bookworm, in a container), `hdiutil makehybrid` | reference tools, run only | as distributed |
| `docs/USAGE.md` and `docs/cue/cue.md` of this repository | specification | this repository |

The XNU ISO 9660 driver source was fetched and searched for sector-size
handling; nothing was taken from it.

## 10. Gaps found

Each gap gives the reproduction that showed it with the release build
(`target/release/unretro`, called `unretro` below), and under **Status**
what was done. `$T` is `~/.cache/unretro-specimens/toast`. The fixtures of
`testdata/toast/` and `tests/toast_fixtures.rs` hold every fixed gap in
place; the same test checks `corpus/toast` of the samples repository
(`full/` and `synthetic/`, laid out as under `$T`) when `UNRETRO_SAMPLES`
names the samples and that directory exists.

### G1. 2056-byte sector images were not recognised

- Recipe: `$T/synthetic/iso-only.2056.toast` (also `hfs-bare`, `hfs-only`,
  `hybrid`, `udf-only`, `bridge` with the same suffix), made by
  `$T/tools/mkraw.py <cooked image> 2056 <output>`.
- Command: `unretro -tf $T/synthetic/iso-only.2056.toast`
- Expected: the listing of `iso-only.toast` (`Iso9660`, 8 files), per
  section 6.
- Was: `Error: Unsupported format: Could not detect container format
  for: iso-only.2056.toast`, exit status 1. The same for all six images.
- **Status: fixed.** `src/formats/cd_sector.rs` tests the sub-headers and
  cooks the sectors; `src/loader.rs` (`xa_framed_volume`) applies the four
  conditions of section 6 as the last step of `detect_format` and opens
  the container from the cooked copy. The readers are unchanged. The
  probes run on the cooked view are the partition map, HFS/MFS/HFS Plus,
  ISO 9660 and UDF; a Lisa disk in a DiskCopy 4.2 wrapper is not looked
  for in such sectors. All six images, and the two fixtures, list what
  their cooked images list.
- Weight: no real specimen; the layout is known from Aaru only.

### G2. 2336-byte sector images were not recognised

- Recipe: `$T/synthetic/iso-only.2336.toast` and the five siblings, made
  by `$T/tools/mkraw.py <cooked image> 2336 <output>`.
- Command: `unretro -tf $T/synthetic/iso-only.2336.toast`
- Expected: the listing of `iso-only.toast`.
- Was: `Error: Unsupported format: Could not detect container format
  for: iso-only.2336.toast`, exit status 1. The same for all six images.
- **Status: fixed**, by the same code as G1, 2056 being tried first.
- Weight: no real specimen.

### G3. The ISO 9660 tree of a hybrid could not be reached

- Specimen: `$T/full/xircom/PORTGEAR201_Mac.toast` (real, concatenated
  hybrid).
- Command: `unretro --list=tsv -f $T/full/xircom/PORTGEAR201_Mac.toast`
- Expected: the 360 files of the HFS partition and, in addition, the 533
  files of the ISO 9660 volume (`7zz l` on the same file lists them:
  `Ch/EUA.TXT`, `Win98/USBENET.sys`, ...), per section 5.3.
- Was: `Apm` with one entry `Toast 4.0 PPC Partition` (`Hfs`), 360
  files. No path led to the ISO tree; 335 of its files have content
  that is nowhere on the HFS side.
- Recipes that showed the same: `$T/synthetic/hybrid-split.toast`
  (`pconly/SETUP.EXE` and `pconly/autorun.inf` unreachable) and
  `$T/synthetic/mkhybrid-nomap-split.toast` (no partition map, opens as
  `Hfs`).
- **Status: fixed.** Section 5.3 is implemented:
  `src/formats/hybrid.rs` lists the ISO 9660 volume as `iso9660.img`
  after the entries of the `Apm` or `Hfs` container, and `src/loader.rs`
  applies it wherever it opens one of the two (`macintosh` and `dos`
  features together). Paths of the Macintosh side are unchanged.
  Results on the specimens:
  - `PORTGEAR201_Mac.toast`: 533 files under `iso9660.img/`, all 533
    identical to `7zz x -tiso`.
  - `Technical.toast` (shared hybrid): 2053 files under `iso9660.img/`;
    2051 identical to `7zz`, one differs in name only (a trailing `.`
    that `7zz` drops), one is a Compact Pro archive that is listed by
    its content.
  - `WriterTools1-1.toast` and `WriterTools 2.0.toast` (CDEveryWhere):
    1 and 2 files, the Rock Ridge tree, as `xorriso -find` lists it.
    Their Joliet tree holds 4 and 5 files (`7zz`): the Windows
    installer, `autorun.inf` and an icon are in that tree only, and the
    ISO 9660 reader presents one tree per volume (`docs/USAGE.md`, "ISO
    9660 CD-ROM Images", names). Those files stay unreachable; this is
    a property of the ISO 9660 reader, not of hybrids.
  - `DemoCD(Hypercard).toast`: no `iso9660.img`, its ISO 9660 root is
    empty.
  - Not addressed: the Mac OS X 10.6 install DVD images (sampled only)
    still list their ISO 9660 region as the opaque partition
    `Macintosh` as well; `iso9660.img` is expected on them but could not
    be checked without the images.
- Weight: 44 of 126 sampled files are hybrids; 9 are concatenated, where
  the ISO side is certain to hold different files.

### G4. Data forks were taken for MacBinary files

Not specific to Toast images; found on two of them.

- Specimen: `$T/full/arple-11-94-technical/Technical.toast`.
- Command: `unretro -tf "$T/full/arple-11-94-technical/Technical.toast" |
  grep -c "(MacBinary)"`
- Expected: 0. The volume holds no MacBinary files.
- Was: 425. Each such file was shown as a container holding one
  zero-length entry with a garbage name; extraction created a directory
  in its place and the file's data fork was not written.
- Second specimen: `$T/full/hypercard-demo/DemoCD(Hypercard).toast`,
  where `unretro -xf ... --preserve-resource-fork -C <dir>` stopped at the
  first such file with `I/O error: Operation not permitted`, leaving 79
  of 109 files written.
- **Status: fixed** in `src/formats/macintosh/macbinary.rs`
  (`is_macbinary_file`): a MacBinary I header, which no checksum guards,
  must have a name without control characters. The count is 0 on both
  images, and the extraction of the second finishes with all 109 files,
  the 1,494,981-byte movie included. The fork lengths are still not
  compared with the file length.
- Related, lesser, unchanged: four resources of `Apple Business Server
  Kit (1996).toast` (`STR#` 16992) are taken for BinHex and reported as
  `NestedContainerOpenFailed`; nothing is lost.

### G5. Memory use is a multiple of the image size

- Command: `/usr/bin/time -l unretro --list=tsv -f <image>`
- Expected: memory bounded well below the image size, so that DVD-sized
  images and 32-bit targets work (section 5.2).
- Was, peak resident memory: 512 MB for the 163 MB `Technical.toast`;
  406 MB for the 138 MB `Apple Business Server Kit (1996).toast`; 349 MB
  for the 173 MB bare-HFS `ShanghaiGreatMoments.toast`; 212 MB for the
  106 MB bridge. On sparse reconstructions of the DVD-sized images: 5.7
  GB for a 2.8 GB image, 10.4 GB for 5.2 GB, 17.0 GB for 8.5 GB.
- **Status: reduced, not closed; the requirement of section 5.2 is
  deliberately not met.** Every container of this crate owns a copy of
  what it lists, and a nested container is opened from its parent's
  copy; bounding memory below the image size needs containers that read
  from a shared image on demand, which is a change to the container
  interface and to every reader, not to the Toast path. What was done:
  `src/loader.rs` no longer keeps the root file's bytes (or mapping)
  while the tree is traversed. Peak now: 418 MB for `Technical.toast`
  (which now also lists its 2053 ISO 9660 files, one more copy of their
  content), 264 MB for `Apple Business Server Kit (1996).toast`, 331 MB
  for `ShanghaiGreatMoments.toast`, 201 MB for the bridge. A partition
  map image still needs about twice its size and a DVD-sized image is
  still out of reach on a 32-bit target.

### G6. Documentation did not mention Toast images

- **Status: fixed.** `docs/USAGE.md` has a section "Toast Images" (what
  each variant opens as, the sector framings, `iso9660.img`), linked from
  `README.md` and from the Python package's README.

Separate issue, not about Toast: `unretro --list=json` wrote names with
Rust escapes (`U\u{308}`), which is not valid JSON for any non-ASCII
name. Fixed in `src/bin/unretro.rs` (`json_string`): names are written
as they are, with only the escapes JSON requires. Unchanged: the JSON
listing gives each file's last path component only, so the directories
of an HFS volume are lost (`print_list_json`). Reproduce with `unretro
--list=json -f $T/synthetic/hybrid.toast`.

## 11. Open points

- **A 2056- or 2336-byte `.toast` from Toast itself.** None was found.
  A Video CD, CD-i or PlayStation image saved by Toast would settle
  section 3.9, including whether such a file starts at sector 0 of the
  track and whether Form 2 sectors are padded to the sector size.
- **Multi-track and mixed-mode images.** Whether any Toast version writes
  several tracks into one `.toast`, and where it records the boundaries,
  is not established (section 3.10).
- **`.Sd2f` audio images.** The data fork is headerless audio; the track
  table is presumably in the resource fork. Without a specimen that kept
  its resource fork, nothing can be specified.
- **DVD-Video and Blu-ray images.** No specimen; expected to be bridge or
  UDF-only images.
- **Hybrids without a partition map from real discs.** Only purpose-built
  ones were tested.
- **The UFS partition of the Darwin 0.3 image** was not sampled far
  enough to say whether it opens.

## 12. Decisions

1. `.toast` is an ordering hint; detection is by content (section 4).
2. Both trees of a hybrid must be reachable; the Mac tree keeps today's
   paths and the ISO 9660 volume is added as one more partition
   entry, `iso9660.img` (section 5.3).
3. The UDF tree of an image that also has an ISO 9660 tree is not
   presented separately (section 5.3).
4. 2056- and 2336-byte framings are accepted only under the four
   conditions of section 6, cooked data and raw 2352-byte sectors taking
   precedence.
5. Partition names that cannot be used as entry names fall back to
   `p<n>.img`, including Toast's own `HFS/Joliet` names (section 5.2).
6. Trailing bytes after the last volume are ignored (section 5.2).
7. Audio images (`.Sd2f`) are not containers (section 5.1).
