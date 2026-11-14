# The Apple Lisa file system

Format specification for clean-room implementation. This document describes
the file system written by the Lisa Office System 1.x, 2.x and 3.x and by
the Lisa Workshop (one file system, three on-disk versions) to the extent
needed to **list and extract named files** from a volume, read-only.

It does not describe the image wrappers that carry a Lisa disk (DiskCopy
4.2, DART, raw 532-byte-block dumps, block interleave, Twiggy side order):
those are the subject of [`images.md`](images.md). This document starts
from a sequence of 512-byte blocks in device order, each with its tag.

Contents: 1 how to read this document; 2 what the reader is given and
returns; 3 sector tags; 4 volume layout; 5 the MDDF; 6 the S-list; 7 the
file leader (hint entry, label, file map); 8 catalogs; 9 from catalog entry
to file contents; 10 file names and the character set; 11 file types and
objects that are not user files; 12 timestamps; 13 devices and sizes; 14
when structures disagree; 15 volumes that are not Lisa FS; 16 worked
examples; 17 safety requirements; 18 observed implementation gaps; 19
decisions; 20 open questions; 21 evidence.

The document contains no program code and no pseudo-code.

## 1. How to read this document

### 1.1 Provenance

- There is no official on-disk specification of this file system. This is
  a **prose specification** derived from existing source code and from
  measurements on real disk images, in isolation: its author read the
  sources listed below, the implementer reads only this document.
- Sources, in the order they were relied on:
  1. Measurements on 27 real volumes (section 21) with a throwaway reader
     written by this document's author from the rules below. It is kept
     outside the repository and is not part of the specification.
  2. The Lisa file system plug-in and the Lisa tag decoder of the Aaru Data
     Preservation Suite (LGPL-2.1; called **Aaru** here), and the Lisa
     character table of its companion library Claunia.Encoding (MIT).
  3. `lisafsh-tool` and the BLU conversion tool of the LisaEm emulator
     project (GPL; called **LisaEm** here), used for the reserved tag
     values only.
  4. The Lisa OS 3.1 source code published by the Computer History Museum
     under the Apple Academic License Agreement (called **the Apple
     source** here). It was consulted where the other sources leave gaps
     or disagree with real data, as authorised by the project owner. Every
     statement that rests on it alone carries the label APPLE-SOURCE.
- The work is a **clean-room process**. Sources are kept outside the
  repository. Nothing here is to be compared with, or completed from, the
  source of any Lisa file system implementation.
- Where Aaru is wrong, incomplete or unsafe the document says so in
  section 18 and specifies the correct behaviour instead. Nothing in that
  section is to be imitated.

### 1.2 Evidence labels

| Label | Meaning |
|---|---|
| CONFIRMED (real data) | Checked on the real volumes of section 21 by the author's throwaway reader. For file contents this includes a byte-for-byte comparison of 196 files with an independent extraction (section 21.2). |
| REFERENCE | Stated by Aaru or LisaEm; not separately observed. |
| APPLE-SOURCE | Rests **solely** on the Apple Lisa OS source: neither Aaru nor LisaEm states it and no examined volume exercises it. Section 21.4 lists every such statement. |
| INFERRED | Deduced; unconfirmed. |
| DECISION | Chosen by this specification (listed in section 19). No decision changes how a genuine, undamaged volume is read. |

Many facts were first learnt from the Apple source and then observed on
real volumes (for example every MDDF offset of section 5 that a reader
uses). Those are labelled CONFIRMED (real data); the field *names* are the
Apple source's. A rule without a label in sections 3 to 9 is CONFIRMED
(real data).

### 1.3 Terms and numbers

| Term | Meaning |
|:--|:--|
| block | 512 data bytes of the device, numbered from 0 in device order, plus its tag. |
| tag | The 12 or 20 extra bytes stored with each block (section 3). |
| **N** | Number of blocks in the image. Untrusted. |
| **B** | The device block that holds the MDDF (section 4.2). |
| page | A block of the file system area, numbered from 0 at the MDDF. Page *p* is device block B + *p*. Every address stored inside the file system is a page number. |
| MDDF | Media Descriptor Data File: the volume header, one page. |
| S-file | A file as the file system sees it, identified by a small positive number, its **S-file number** (also "file ID"). |
| S-list | The table of S-list entries (Aaru: "S-records"), indexed by S-file number. |
| leader | The page or pages that describe one S-file: the hint entry, the label and the first part of the file map (Aaru: "extents file"). |
| hint entry | The fixed record at the start of the leader with the file's name, type, dates and flags. |
| file map | The list of extents (runs of consecutive pages) holding a file's data. |
| catalog | The directory: names to S-file numbers. |
| node | A 2048-byte (4-page) unit of the version-17 catalog tree. |
| **L** | The logical length of a file in bytes (section 9.2). |
| skipped entry | An entry that is not delivered, with a one-line reason for the caller's diagnostics. |

All multi-byte integers are **big-endian**. Offsets and sizes are in
bytes. Numbers are decimal unless prefixed `0x`. Byte sequences in code
style are hexadecimal. "Signed" means two's complement. A **string[n]**
is a length byte followed by n character bytes, padded with one more byte
when that makes its total size even; so string[32] occupies 34 bytes and
string[8] occupies 10. Bytes of a string beyond its length are not
cleared and contain garbage.

## 2. What the reader is given and what it returns

- Input: N blocks of 512 bytes in device order and, for each block, a tag
  of 12 bytes (floppy and Twiggy images) or 20 bytes (ProFile and Widget
  images). The tag size is the same for all blocks of an image. An image
  without tags cannot be read as Lisa FS: the MDDF is found through tags
  (section 4.2).
- Output: for each user file, a path and its contents; nothing for
  system structures, pipes, event channels, deleted entries.
- The public metadata of this crate carries a modification time: the
  reader reports DTM of the hint entry (section 7.1), converted as section
  12 says and marked as zone-less. It has no fields for the other
  timestamps, labels, passwords or tags; sections 7 and 12 describe them so
  they can be added later.

## 3. Sector tags

### 3.1 The page label

The operating system thinks of every block as carrying a 24-byte *page
label* with these fields (APPLE-SOURCE for the in-memory form; the two
on-disk forms below are CONFIRMED):

| Field | Meaning |
|:--|:--|
| version | 16 bits. For data pages and leaders it equals the `version` word of the file's S-list entry (0, 1, 3 seen). Not needed by a reader. |
| volume | 16 bits. The top two bits of the first byte are a data status (0 good, 1 "maybe", 2 bad) written by the repair utility; the rest is not interpreted. Values seen in the first byte on healthy disks include 0x00, 0x01, 0x02, 0x03, 0x09, 0x0C, 0x10, 0x21 and 0x3E, so a reader must not interpret this field at all. |
| file ID | 16 bits, signed. Who owns the block (section 3.4). |
| data used | Number of data bytes of the block that are in use, 0 to 512. |
| absolute page | The block's own page number. |
| relative page | The block's index within its owner, from 0. |
| forward link | Page number of the owner's next block, or "none". |
| backward link | Page number of the owner's previous block, or "none". |

Links and the absolute page are **page numbers** (relative to B), not
device block numbers.

### 3.2 The 12-byte tag (3.5-inch and Twiggy diskettes)

| Offset | Size | Field |
|--:|--:|:--|
| 0 | 2 | version |
| 2 | 2 | volume |
| 4 | 2 | file ID, signed |
| 6 | 2 | relative page (the low 16 bits) |
| 8 | 2 | top 5 bits: the high 5 bits of the 10-bit *data used*; low 11 bits: forward link |
| 10 | 2 | top 5 bits: the low 5 bits of *data used*; low 11 bits: backward link |

- *data used* = (top 5 bits of the word at 8) × 32 + (top 5 bits of the
  word at 10).
- A link value of 0x7FF means "none".
- The absolute page is not stored: it is the block's position.
- There is no checksum.
- Unused blocks have an all-zero tag.

### 3.3 The 20-byte tag (ProFile and Widget)

| Offset | Size | Field |
|--:|--:|:--|
| 0 | 2 | version |
| 2 | 2 | volume |
| 4 | 2 | file ID, signed |
| 6 | 2 | bit 15: "checksum valid" flag; low 15 bits: *data used* |
| 8 | 3 | absolute page |
| 11 | 1 | checksum |
| 12 | 2 | relative page |
| 14 | 3 | forward link |
| 17 | 3 | backward link |

- A link or absolute-page value of 0xFFFFFF means "none". The boot block
  and the loader blocks have absolute page 0xFFFFFF.
- **Checksum**: when the flag at bit 15 of offset 6 is set, the checksum
  byte is chosen so that the exclusive-or of all 532 bytes of the block
  (512 data bytes and 20 tag bytes) is zero. CONFIRMED (real data) on
  every flagged block tested of two hard-disk volumes (29,719 blocks,
  used and free; no exception).
- Free blocks keep the flag and a valid checksum; their other tag fields
  are stale.
- Aaru also knows a 24-byte tag for Priam disks: the 20-byte layout
  followed by a 4-byte disk size. REFERENCE; no specimen; out of scope
  (DECISION D1).

### 3.4 File ID values

| File ID (signed) | As bytes | Owner |
|--:|:--|:--|
| 0 | `00 00` | Free block. |
| 1 | `00 01` | MDDF (and its backup copy, section 4.3). |
| 2 | `00 02` | Allocation bitmap. |
| 3 | `00 03` | S-list. |
| 4 | `00 04` | Root catalog: the catalog file's data in versions 14 and 15, catalog tree nodes in version 17. |
| 5 to 32766 | | Data pages of the S-file with that number. |
| −4 to −32766 | `FF FC` … | Leader pages of the S-file whose number is the negation: the leader of S-file 6 is tagged −6 (`FF FA`). |
| −21846 | `AA AA` | Boot block (device block 0). |
| −17477 | `BB BB` | Operating system loader blocks. |
| 32767 | `7F FF` | Erased or temporary. REFERENCE (LisaEm, Aaru: erased blocks) and APPLE-SOURCE (number reserved for temporary S-files). Not seen. Never a user file. |

Relative pages: data pages of a file count 0, 1, 2 …; a leader page has
relative page 0 and further leader or file-map pages count on from 1;
loader blocks count from 1 because the boot block is relative page 0 of
the same sequence.

### 3.5 How the two forms map onto each other

| Field | 12-byte tag | 20-byte tag |
|:--|:--|:--|
| version, volume, file ID | offsets 0, 2, 4 | same offsets |
| data used | 10 bits split over the words at 8 and 10 | low 15 bits of the word at 6 |
| checksum-valid flag, checksum | absent | bit 15 at offset 6, byte at 11 |
| absolute page | absent (the block's position minus B) | 3 bytes at 8 |
| relative page | 16 bits at 6 | 16 bits at 12 |
| forward link | 11 bits at 8, none = 0x7FF | 24 bits at 14, none = 0xFFFFFF |
| backward link | 11 bits at 10, none = 0x7FF | 24 bits at 17, none = 0xFFFFFF |

DECISION D2: the reader decodes either form into one internal record
(file ID, relative page, data used, forward link with "none" normalised)
and uses nothing else. It never requires the 20-byte checksum to be
valid and never requires absolute-page or backward-link fields to be
consistent.

## 4. Volume layout

### 4.1 Areas

In device order:

| Area | Tag file ID | Notes |
|:--|:--|:--|
| Boot block | `AA AA` | Device block 0. |
| Volume start gap | | Hard disks only: device blocks 1 to 8 are outside the volume (section 13). |
| OS loader | `BB BB` | Relative pages 1 upward. Present even on non-bootable disks. |
| (unused) | 0 | One free block may sit between the loader and the MDDF. |
| MDDF | 1 | Page 0, one page. |
| Allocation bitmap | 2 | From page 1. One bit per page. Not needed for reading. |
| S-list | 3 | Located by the MDDF. |
| Catalog, leaders, data | 4, negative, positive | Anywhere after that; located through the S-list and the catalog. |

### 4.2 Finding the MDDF

- B is the lowest-numbered block, among the first 100 blocks of the
  device, whose tag has file ID 1 and relative page 0 and whose contents
  pass the checks of section 5.3. If there is none, the image is not a
  Lisa FS volume. (Aaru searches the same 100 blocks. DECISION D3 for the
  exact bound and for continuing the search after a failed candidate.)
- Observed positions of B: 28 on 400K 3.5-inch diskettes, 41 on Twiggy
  diskettes, 38 on hard disks.
- The MDDF records its own position in the field `firstblock`
  (section 5.1), counted from the **volume start**: 28 and 41 on
  diskettes, where the volume starts at device block 0, and 30 on hard
  disks, where it starts at device block 8. The difference
  S = B − `firstblock` is therefore 0 or 8 on every examined volume.
  CONFIRMED (real data); the reason (a configurable "virtual volume
  start" per device, with the boot block written at absolute block 0
  regardless) is APPLE-SOURCE.
- The boot block also holds the value of `firstblock` as a 16-bit number,
  at byte offset 14 on version 15 and 17 volumes and at offset 10 on the
  version 14 volumes examined. INFERRED from five volumes; not to be used.

### 4.3 The backup MDDF

Version 17 volumes keep a second MDDF at page 270, also tagged with file
ID 1. CONFIRMED (real data): present on all 19 version-17 volumes
examined, absent on versions 14 and 15; the constant 270 is also in the
Apple source. Its contents may be older than the primary's. The reader
never uses it (DECISION D4); the rule "lowest-numbered block within the
first 100" of section 4.2 cannot reach it.

## 5. The MDDF

### 5.1 Fields

The MDDF occupies the start of page 0. Its length is 298 bytes in
versions 14 and 15 and 314 bytes in version 17 (the field `MDDFsize`
says so); the rest of the page is unused.

| Offset | Size | Field | Meaning |
|--:|--:|:--|:--|
| 0x00 | 2 | fsversion | On-disk version: 14 (0x0E), 15 (0x0F) or 17 (0x11). |
| 0x02 | 8 | volid | Unique volume identifier. |
| 0x0A | 2 | volnum | |
| 0x0C | 34 | volname | string[32], volume name. |
| 0x2E | 34 | password | string[32], volume password. Garbage when unset: the length byte can exceed 32. |
| 0x50 | 4 | init_machine_id | Serial number of the machine that initialised the volume. |
| 0x54 | 4 | master_machine_id | Copy protection. |
| 0x58 | 4 | DT_created | Timestamp (section 12). |
| 0x5C | 4 | DT_copy_created | Timestamp. |
| 0x60 | 4 | DT_copied | Timestamp of last backup. |
| 0x64 | 4 | DT_scavenged | Timestamp of last repair. |
| 0x68 | 4 | copy_thread | |
| 0x6C | 4 | firstblock | Block of the MDDF, counted from the volume start. |
| 0x70 | 4 | lastblock | Last block of the volume, counted from the volume start. |
| 0x74 | 4 | lastfspage | Last page of the file system: `lastblock` − `firstblock`. |
| 0x78 | 4 | blockcount | Number of blocks of the volume: `lastblock` + 1. |
| 0x7C | 2 | blocksize | 536 on every volume examined (512 + 24). |
| 0x7E | 2 | datasize | 512. |
| 0x80 | 2 | cluster_size | 1. |
| 0x82 | 4 | MDDFaddr | Page of the MDDF: 0. |
| 0x86 | 2 | MDDFsize | 298 or 314. |
| 0x88 | 4 | bitmap_addr | Page of the bitmap: 1. |
| 0x8C | 4 | bitmap_size | Bits in the bitmap: `lastfspage` + 1. |
| 0x90 | 2 | bitmap_bytes | |
| 0x92 | 2 | bitmap_pages | |
| 0x94 | 4 | slist_addr | First page of the S-list. |
| 0x98 | 2 | slist_packing | S-list entries per page: 36. |
| 0x9A | 2 | slist_block_count | Pages in the S-list. |
| 0x9C | 2 | first_file | Lowest S-file number that is not a fixed system file: 4 in versions 14 and 15, 5 in version 17. |
| 0x9E | 2 | empty_file | Allocation hint. |
| 0xA0 | 2 | maxfiles | Highest usable S-file number. |
| 0xA2 | 2 | hintsize | Pages per leader: 2 in version 14, 1 in versions 15 and 17. |
| 0xA4 | 2 | leader_offset | 0. |
| 0xA6 | 2 | leader_pages | 1. |
| 0xA8 | 2 | flabel_offset | Byte offset of the file label in the first leader page: 384. |
| 0xAA | 2 | (unused) | |
| 0xAC | 2 | map_offset | Page offset, within the leader, of the first file-map page: 1 in version 14, 0 in versions 15 and 17. |
| 0xAE | 2 | map_size | File-map entries per full map page: 84. |
| 0xB0 | 2 | filecount | S-files in use. |
| 0xB2 | 4 | freestart | Allocation hint. |
| 0xB6 | 4 | (unused) | |
| 0xBA | 4 | freecount | Free pages. |
| 0xBE | 2 | rootsnum | S-file number of the root catalog: 4. |
| 0xC0 | 2 | rootmaxentries | Number of slots of the flat root catalog (versions 14, 15); equals `maxfiles`. |
| 0xC2 | 2 | mountinfo | Not interpreted. |
| 0xC4 | 8 | overmount_stamp | |
| 0xCC | 4 | pmem_id | |
| 0xD0 | 66 | pmem | Saved parameter memory. |
| 0x112 | 1 | vol_scavenged | Boolean. |
| 0x113 | 1 | tbt_copied | Boolean. |
| 0x114 | 2 | smallmap_offset | Byte offset of the small file map in the leader page: 128 (0x80) in versions 15 and 17, 0 in version 14. |
| 0x116 | 2 | hentry_offset | Byte offset of the hint entry in the leader page: 0. |
| 0x118 | 8 | backup_volid | |
| 0x120 | 2 | flabel_size | Size of the file label: 128. |
| 0x122 | 2 | fs_overhead | |
| 0x124 | 2 | result_scavenge | |
| 0x126 | 2 | boot_code | |
| 0x128 | 2 | boot_environ | Last field of versions 14 and 15. |
| 0x12A | 4 | oem_id | Version 17 only, as are all fields below. |
| 0x12E | 4 | root_page | Page of the root node of the catalog tree; 0xFFFFFFFF when the tree is empty. |
| 0x132 | 2 | tree_depth | Number of levels of the catalog tree; 1 when the root is a leaf. |
| 0x134 | 2 | node_id | Highest directory identifier handed out. |
| 0x136 | 2 | vol_seq_no | |
| 0x138 | 1 | vol_mounted | Boolean: volume was not cleanly unmounted. |

- Offsets 0x00 to 0xC0, 0x114, 0x120, 0x12E and 0x132 are CONFIRMED (real
  data): the values found there are the ones the rest of the volume
  requires (positions, counts, sizes), on all three versions.
- The meaning of the fields a reader does not use (identifiers, machine
  serial numbers, `pmem`, the flags and everything after 0x114 except
  those just named) is APPLE-SOURCE.
- Aaru's table of MDDF offsets differs from this one between 0x80 and
  0x93 and after 0x112 (section 18). The table above is the one that
  matches real volumes.

### 5.2 Fields a reader uses

`fsversion`, `volname` (optionally, as a label for the container),
`firstblock`, `lastblock`, `lastfspage`, `blockcount`, `datasize`,
`slist_addr`, `slist_packing`, `slist_block_count`, `maxfiles`,
`hintsize`, `map_offset`, `smallmap_offset`, `rootsnum`,
`rootmaxentries` (versions 14, 15), `root_page` and `tree_depth`
(version 17).

### 5.3 Sanity checks (content detection)

A candidate block is accepted as the MDDF only if **all** of these hold
(DECISION D5; each is true of all 27 examined volumes, and the set
rejects the four examined volumes of section 15):

1. Its tag has file ID 1 and relative page 0.
2. `fsversion` is 14, 15 or 17.
3. `datasize` is 512 and `blocksize` is at least 512.
4. `blockcount` is at least 2, `lastblock` = `blockcount` − 1, and
   `lastfspage` = `lastblock` − `firstblock` (so `firstblock` ≤
   `lastblock`).
5. `firstblock` ≤ the candidate's block number. With S = block number −
   `firstblock`: S + `blockcount` ≤ N.
6. `MDDFaddr` is 0.
7. The length byte of `volname` is at most 32. It may be 0: two
   installed hard-disk volumes (`profile_LisaEM_WorkshopFullAll9Disks`
   and `profile_IDLE_WorkshopFullAll9disks`) have an empty volume name
   at MDDF offset 0x0C and are otherwise ordinary. CONFIRMED (real data).
8. `slist_packing` is between 1 and 36, `slist_block_count` is at least
   1, `slist_addr` is at least 1, `slist_addr` + `slist_block_count` − 1
   ≤ `lastfspage`, and `maxfiles` < `slist_packing` ×
   `slist_block_count`.
9. `rootsnum` is 4.
10. The layout parameters have a combination this document describes:
    `hintsize` 2, `map_offset` 1 for version 14; `hintsize` 1,
    `map_offset` 0, `smallmap_offset` 128 for versions 15 and 17.

Aaru performs checks 1, 3 (partly), 4 and 5 and accepts on those alone.
The stronger set is needed here because detection runs on arbitrary
input and before the Macintosh detection (a MacWorks disk in the same
wrapper must fall through).

- After acceptance, every page number *p* used anywhere must satisfy
  0 ≤ *p* ≤ `lastfspage` **and** B + *p* < N. Both are required: the
  first is the volume's own limit, the second protects against a
  truncated image (section 17).
- A version other than 14, 15, 17 is not a Lisa FS volume as far as this
  reader is concerned. No other version was found in any source: the
  Apple source names exactly these three (release 1.0, release 2.0,
  release 3.x); Aaru and LisaEm know the same three. Versions 14 and 15
  differ only in the leader layout (section 7.3); 17 adds the catalog
  tree.

## 6. The S-list

- The S-list is a plain table indexed by S-file number, without hashing:
  the entry of S-file *f* is in page `slist_addr` + (*f* div
  `slist_packing`), at byte offset (*f* mod `slist_packing`) × 14.
  With 36 entries per page the last 8 bytes of each page are unused.
- Valid S-file numbers are 0 to `maxfiles`. A catalog entry naming a
  number outside 5 … `maxfiles` is a skipped entry (numbers 1 to 4 are
  the fixed system files).

| Offset | Size | Field | Meaning |
|--:|--:|:--|:--|
| 0 | 4 | hintaddr | Page of the file's leader. Signed. |
| 4 | 4 | fileaddr | Page of the file's first data page; 0 when the file has no data. Signed. Not needed. |
| 8 | 4 | filesize | Logical size of the file in bytes. Signed. |
| 12 | 2 | version | Small counter (0, 1, 2, 3, 5 seen). Not needed. |

- An unused entry is all zero, or keeps stale values with `hintaddr` 0.
- Version 17: the entry of S-file 4 has `hintaddr` and `fileaddr`
  0xFFFFFFFF; the root catalog is not an S-file there.
- **An S-list entry is trusted only if** `hintaddr` ≥ 1, the page is in
  range, and the tag of that page has file ID −*f* and relative page 0.
  Real volumes contain stale entries that fail this test (section 14).
- Aaru describes the entry as "pointer, unknown, size, flags"; the second
  field is the first data page (CONFIRMED on all volumes: it equals the
  first extent's address) and the fourth is the counter that also
  appears as the tag `version` of the file's pages.

## 7. The file leader

Every S-file has a leader of `hintsize` pages starting at page
`hintaddr`, tagged with the negated S-file number. Its first page holds
the hint entry at offset 0, the file label at offset `flabel_offset`
(384), and, in versions 15 and 17, the small file map at offset
`smallmap_offset` (128).

### 7.1 The hint entry

| Offset | Size | Field | Meaning |
|--:|--:|:--|:--|
| 0x00 | 34 | name | string[32]: the file's name, the same bytes as in the catalog. |
| 0x22 | 8 | unique_ID | |
| 0x2A | 2 | version | File format version: 14 on version-14 volumes, 15 on version-15 volumes, 21 on version-17 volumes. |
| 0x2C | 1 | ftype | File type (section 11). |
| 0x2D | 1 | (padding) | Garbage. |
| 0x2E | 4 | DTC | Created. Timestamps: section 12. |
| 0x32 | 4 | DTA | Last accessed. |
| 0x36 | 4 | DTM | Last modified. |
| 0x3A | 4 | DTB | Last backed up. |
| 0x3E | 4 | DTS | Last scavenged. |
| 0x42 | 4 | machine_id | Serial number of the only machine allowed to open a protected file. |
| 0x46 | 1 | killed | Boolean. |
| 0x47 | 1 | safety_on | Boolean: the "safety" (delete-protect) attribute. |
| 0x48 | 1 | protected | Boolean: copy protection is enforced. |
| 0x49 | 1 | master | Boolean: protected master copy. |
| 0x4A | 1 | scavenged | Boolean: touched by the repair utility. |
| 0x4B | 1 | closed_by_OS | Boolean. |
| 0x4C | 1 | file_open | Boolean: was open when the volume was last written. |
| 0x4D | 1 | (padding) | |
| 0x4E | 2 | result_scavenge | |
| 0x50 | 2 | (unused) | |
| 0x52 | 2 | system_type | |
| 0x54 | 2 | user_type | |
| 0x56 | 2 | user_subtype | |
| 0x58 | 8 | build_info | Four 16-bit numbers (release, build, compatibility level, revision; REFERENCE for the order). |
| 0x60 | 2 | file_portion | |
| 0x62 | 10 | password | string[8]: the file's password after a one-way scramble. Length byte 0 when the file has none. |
| 0x6C | 2 | parentID | Identifier of the containing directory (version 17). |
| 0x6E | 2 | fsOverhead | |

- CONFIRMED (real data): `name` (equal to the catalog name for every
  file), `version` at 0x2A, `ftype` at 0x2C, the three dates DTC, DTA,
  DTM (plausible dates; DTC and DTM equal the catalog tree's copies in
  version 17), `password` length byte at 0x62 (always 0 on examined
  volumes), and that the layout is identical in all three versions.
- The meaning of the fields from 0x42 to 0x60 and of 0x6C and 0x6E is
  APPLE-SOURCE (Aaru lists the same names with the same offsets up to
  0x62, so those are also REFERENCE).
- Aaru places a "version" word at 0x20 and the parent identifier at 0x6B.
  Real data contradicts the first (0x20 and 0x21 are the unused tail of
  the name field and hold garbage) and alignment contradicts the second.
- **Passwords and protection.** The file password and the
  machine-serial protection are checks made by the operating system when
  a file is opened; nothing indicates that file contents are transformed
  (APPLE-SOURCE and INFERRED; no password-protected file was available).
  The reader ignores the password, the volume password and the protection
  flags and delivers the contents (DECISION D6).

### 7.2 The file label

The `flabel_size` (128) bytes at offset `flabel_offset` (384) of the
first leader page are a per-file area that applications may use. The
Office System stores there, at label offset 2, a string holding the
name the document or tool shows on the desktop (for example the file
`{T3}obj` carries `LisaCalc`); the rest was not analysed. CONFIRMED
(real data) for position and size and for the desktop name; the reader
does not use the label (DECISION D7) and it is listed as a follow-up
because the names in the catalog of an Office System disk are the
internal ones.

### 7.3 The file map

A file map section has an 8-byte header followed by 6-byte entries:

| Offset | Size | Field | Meaning |
|--:|--:|:--|:--|
| 0 | 4 | size | Number of pages allocated to the file (the sum of all entries' lengths). |
| 4 | 2 | max_entries | **Index of the last entry** of this section: 9 for a small map, 83 for a full map. |
| 6 | 2 | ecount | Number of entries in use. |
| 8 | 6 × (max_entries + 1) | entries | |

Each entry:

| Offset | Size | Field | Meaning |
|--:|--:|:--|:--|
| 0 | 4 | address | First page of the extent. Signed. Zero or negative: unused entry, end of the map. |
| 4 | 2 | cpages | Number of consecutive pages. Signed. |

Where the first section is:

| Version | First section | Capacity |
|:--|:--|:--|
| 14 | A **full map** filling page `hintaddr` + `map_offset` (the second leader page): header at offset 0, 84 entries, 512 bytes in all. | 84 extents |
| 15, 17 | A **small map** inside the leader page at offset `smallmap_offset` (128): header and 10 entries, 68 bytes. | 10 extents |

- The entries are in file order: the first extent holds the file's first
  pages.
- The map ends at the first entry whose `address` is zero or negative,
  or after entry `max_entries` when no continuation exists.
- DECISION D8: the reader walks entries until that end condition and
  ignores `ecount`, and ignores `size` except as a diagnostic. (On every
  examined file `ecount` equals the number of entries before the first
  unused one and `size` equals the sum of their lengths.)
- **Continuation.** When a file needs more extents than the first
  section holds, the map continues on further pages, each a full map
  section (header and 84 entries). They are chained through the tag's
  **forward link**, starting from the page that holds the first section
  (page `hintaddr` + `map_offset`): its forward link names the first
  continuation page, whose forward link names the next, and "none" ends
  the chain. Continuation pages are tagged with the negated S-file
  number like the leader. APPLE-SOURCE: no examined file has more than 7
  extents, so no continuation page was observed. Aaru does not implement
  continuation (it reads up to 41 entries from the leader page, which
  for a small map runs past the map into unrelated bytes).
- In version 14 the first leader page's forward link names the second
  leader page (the map page); that link is part of the leader, not a
  continuation. The continuation chain of a version-14 file starts at
  the forward link of the *second* leader page.
- CONFIRMED (real data): the first-section rules, for 925 catalog entries
  on 27 volumes; multi-extent files with 2, 3, 5 and 7 extents.

## 8. Catalogs

### 8.1 Versions 14 and 15: the flat catalog

- The root catalog is an ordinary S-file, number `rootsnum` (4). Its
  leader has `ftype` 2 and the name `rootcatalog`; its data pages are
  tagged with file ID 4; its file map is read like any other
  (section 7.3).
- Its contents are slots of 54 bytes, packed without regard to page
  boundaries (a slot may straddle two pages). The number of slots is the
  S-list `filesize` of S-file 4 divided by 54. On most volumes that
  equals `rootmaxentries`, but not on all: `app_QUICKPORT_2.0_1OF5` has
  `rootmaxentries` = `maxfiles` = 71 and a catalog of 5,778 bytes (107
  slots) with six of its 22 files in slots 75 to 93. CONFIRMED (real
  data). The reader therefore examines every slot the catalog file
  holds and takes no bound from `rootmaxentries` or `maxfiles`.
- The slot a name occupies is chosen by a hash of the name, so used
  slots are scattered. A reader that lists the catalog does not need the
  hash function: it examines every slot.
- There is one catalog per volume and no directory level.

Slot layout:

| Offset | Size | Field | Meaning |
|--:|--:|:--|:--|
| 0x00 | 34 | name | string[32]. |
| 0x22 | 1 | type | Entry type (below). |
| 0x23 | 1 | (padding) | |
| 0x24 | 2 | sfile | S-file number. Signed. |
| 0x26 | 4 | attributes | Unused. |
| 0x2A | 12 | pipe state | Read page, read offset, write page, write offset; meaningful for pipes only. |

Entry types (the same numbering is used in the version-17 tree):

| Value | Name | Seen | Listed |
|--:|:--|:--|:--|
| 0 | empty | yes (slot never used; name length 0) | no |
| 1 | directory | version 17 only, not seen | as a path component |
| 2 | link | no | no |
| 3 | file | yes | **yes** |
| 4 | pipe | yes (`SYSTEM.TIMER_PIPE`) | no |
| 5 | event channel | yes (`resident_channel`) | no |
| 6 | killed | no | no |
| 7 | removed | yes (deleted slot; name length 0, S-file 0) | no |
| 8 | thread | version 17 only | no |

- Values 0, 3, 4, 5, 7 and 8 are CONFIRMED (real data); 1, 2 and 6 are
  APPLE-SOURCE (Aaru knows 1, 3 and 8 by their record sizes).
- A slot is a file entry when its type is 3, its name length is 1 to 32
  and its S-file number is greater than 4 and at most `maxfiles`. Every
  other slot is passed over silently, except that a type-3 slot with an
  invalid name length or S-file number is a skipped entry.
- Garbage follows the name inside the name field and may fill the
  attribute bytes.

### 8.2 Version 17: the catalog tree

The catalog is a B-tree with all records in the leaves and the leaves
chained in key order.

**Nodes.** A node is 4 consecutive pages (2048 bytes), all tagged with
file ID 4, relative pages 0 to 3. `root_page` in the MDDF is the first
page of the root node; `tree_depth` is the number of levels. The last 12
bytes of a node are its descriptor:

| Node offset | Size | Field | Meaning |
|--:|--:|:--|:--|
| 0x7F4 | 2 | nkeys | Number of records in the node. |
| 0x7F6 | 4 | prior | First page of the previous leaf, 0xFFFFFFFF if none. Meaningful in leaves only. |
| 0x7FA | 4 | next | First page of the next leaf, 0xFFFFFFFF if none. Meaningful in leaves only. |
| 0x7FE | 1 | kind | 0 leaf, 1 interior. |
| 0x7FF | 1 | cksum | Not a usable checksum (0x00 and 0xFF both occur on healthy nodes). Ignored. |

- Records are packed from offset 0 upward. An **offset table** of 16-bit
  values grows downward from the descriptor: entry *i* is at node offset
  0x7F2 − 2 × *i*. Entry *i* (0 ≤ *i* < nkeys) is the offset of record
  *i*; entry nkeys is the offset just past the last record. Record *i*
  therefore occupies the bytes from entry *i* up to entry *i* + 1, and
  its size is their difference.
- In interior nodes the `prior` and `next` fields hold garbage and must
  not be followed.

**Keys.** Every record starts with a 36-byte key:

| Offset | Size | Field |
|--:|--:|:--|
| 0 | 1 | Constant 0x24 (36). |
| 1 | 2 | Identifier of the directory that contains the object; 0 for the root. |
| 3 | 32 | Name, padded with zero bytes. |
| 35 | 1 | Zero. |

The name's length is the number of bytes before the first zero byte, at
most 32. Keys are ordered by directory identifier, then by name, comparing
bytes after folding ASCII `a` to `z` to upper case (APPLE-SOURCE for the
exact ordering; real leaves are consistent with it). A lister does not
need the ordering.

**Interior nodes.** Each record is 40 bytes: a 4-byte page number (the
first page of a child node) followed by a 36-byte key. The key of record
0 is not significant; the key of record *i* > 0 is the lowest key
reachable through child *i*. `nkeys` counts children. CONFIRMED (real
data) on two volumes with depth 2 and one with 15 leaves.

**Leaf records.** After the key, at record offset 0x24, comes a type byte
(the entry types of section 8.1) and one byte of padding (garbage), then
a body that depends on the type:

File record, type 3, 64 bytes:

| Offset | Size | Field | Meaning |
|--:|--:|:--|:--|
| 0x00 | 36 | key | Parent directory identifier and name. |
| 0x24 | 1 | type | 3. |
| 0x25 | 1 | (padding) | |
| 0x26 | 2 | sfile | S-file number. Signed. |
| 0x28 | 4 | fileDTC | Created (copy of the hint entry's DTC). |
| 0x2C | 4 | fileDTM | Modified (copy of DTM). |
| 0x30 | 4 | size | Logical size in bytes (copy of the S-list `filesize`). |
| 0x34 | 4 | physSize | Allocated size in bytes. |
| 0x38 | 2 | fsOvrhd | Leader pages. |
| 0x3A | 2 | flags | Status bits; the low bits hold garbage on real volumes. Ignored. |
| 0x3C | 4 | (unused) | Garbage. |

Directory record, type 1, 48 bytes (APPLE-SOURCE and REFERENCE; no
examined volume contains one):

| Offset | Size | Field | Meaning |
|--:|--:|:--|:--|
| 0x00 | 36 | key | Parent directory identifier and the directory's name. |
| 0x24 | 1 | type | 1. |
| 0x25 | 1 | (padding) | |
| 0x26 | 2 | id | The directory's own identifier: the value found in the key of everything it contains. |
| 0x28 | 4 | dirDTC | Created. |
| 0x2C | 4 | (unused) | |

Thread record, type 8, 78 bytes:

| Offset | Size | Field | Meaning |
|--:|--:|:--|:--|
| 0x00 | 36 | key | A directory's own identifier and an empty name. |
| 0x24 | 1 | type | 8. |
| 0x25 | 1 | (padding) | |
| 0x26 | 2 | parID | Identifier of that directory's parent. |
| 0x28 | 34 | myName | string[32]: that directory's name. |
| 0x4A | 4 | (unused) | |

- Every volume has one thread record with key (0, empty name): it sorts
  first and belongs to the root. CONFIRMED (real data): present as the
  first record of all 19 version-17 volumes, 78 bytes; its body is not
  meaningful for the root. That each directory has a thread record of
  its own, keyed by its identifier, is APPLE-SOURCE. A lister ignores
  thread records.
- Record sizes are taken from the offset table, never from the type. A
  record shorter than its type requires (64, 48, 78; 38 for any other
  type) makes the node invalid.

**Enumerating the catalog** (DECISION D9):

1. If `root_page` is 0xFFFFFFFF or `tree_depth` is 0, the catalog is
   empty.
2. Start at the root node. While the current level is above the leaf
   level (`tree_depth` − 1 times), take the child page of record 0 of
   the interior node.
3. Read the leaf reached, deliver its records in order, then follow
   `next` until it is 0xFFFFFFFF.
4. Every node read is validated (section 17.3), and each node is read at
   most once.

Aaru instead scans the whole device for a block tagged file ID 4 with
relative page 0, walks `prior` links back to the first leaf and then
`next` links forward. That relies on the first such block being a leaf
(on a depth-2 volume it may be the interior root, whose links are
garbage). It may be used as a fallback only with the leaf check
(`kind` = 0) added; it is not required.

**Directories and paths.**

- A file record's path is the chain of directory names from the root to
  its parent identifier, then its own name.
- To build it, collect all directory records first: identifier → (parent
  identifier, name). Identifier 0 is the root and has no record.
- A parent identifier that has no directory record, a chain longer than
  16 levels, or a chain that revisits an identifier is broken: the file
  is listed directly under the root under its own name (DECISION D10).
- Real disks rarely have directories. What looks like a directory tree
  on Workshop disks is a naming habit: names such as
  `ALEX/MAKE/DISK1.TEXT` are single 32-byte names containing `/`
  (section 10).

## 9. From catalog entry to file contents

### 9.1 Steps

1. Take the S-file number *f* from the catalog entry (type 3).
2. Read S-list entry *f* (section 6) and apply the trust test: leader
   page in range and tagged −*f*, relative page 0. If it fails, the
   entry is skipped ("stale S-list entry").
3. Determine L (section 9.2). If L is 0, deliver an empty file and stop:
   the file map is not consulted.
4. Read the file map (section 7.3), validating every extent
   (section 17.2), until it covers ⌈L / 512⌉ pages or ends.
5. The contents are the first L bytes of the listed pages' data, in map
   order.
6. Verify the tags of the pages used (section 14, rule 4).

### 9.2 The logical length

- L is the `filesize` of the S-list entry. It is exact to the byte.
  CONFIRMED (real data): for all 196 files compared with an independent
  extraction; and on every examined file with a non-zero size the
  *data used* field of the last page's tag equals ((L − 1) mod 512) + 1.
- In version 17 the catalog record repeats the size at 0x30; the two
  were equal for every file examined. DECISION D11: the S-list value is
  used; if the catalog value differs the smaller of the two is used and
  a diagnostic is raised.
- The file map's `size` and the catalog's `physSize` describe allocated
  space, not content. Files exist with L = 0 and a non-empty or stale
  map (section 14, rule 3).
- If the map covers fewer than ⌈L / 512⌉ pages, L is reduced to the
  number of bytes the map covers and a diagnostic is raised (DECISION
  D12). One genuine file does this: `{D112T1}AmericanDict.data` on the
  LisaWrite 3.1 disk has L = 241,152 (471 pages) but a 470-page map on a
  completely full diskette; the independent extraction also has 240,640
  bytes.
- A negative `filesize` makes the entry a skipped entry.

## 10. File names and the character set

- A name is 1 to 32 bytes. The catalog stores the bytes as the creator
  typed them: case is **preserved**. Lookup is case-insensitive for the
  ASCII letters only (APPLE-SOURCE for "ASCII only"; REFERENCE for
  case-insensitive).
- In an operating system path the characters `-` and `+` separate
  components, so a name created through the operating system contains
  neither (APPLE-SOURCE). None of the 925 examined names contains
  either. Every other byte value is permitted, including `/`, `{`, `}`,
  `!`, `$`, space and `.`, all of which occur.
- There are no extensions as a file system concept; `.TEXT`, `.OBJ` and
  so on are conventions of the Workshop.
- Office System disks use internal names: `{T3}obj` (tool 3's program),
  `{D109T3}` (a document), `{F108}` (a folder), `{!CATALOG}`. The
  visible names are in the labels (section 7.2).

**Character set.** Bytes 0x20 to 0x7E are ASCII. Bytes 0x80 to 0xD8 are
the same characters as in the Macintosh Roman set (REFERENCE: Aaru's
table). Byte 0x7F is U+2588 FULL BLOCK in Aaru's table. Bytes 0xD9 to
0xFF are undefined. Only ASCII occurred in the examined names.

| | x0 | x1 | x2 | x3 | x4 | x5 | x6 | x7 | x8 | x9 | xA | xB | xC | xD | xE | xF |
|:--|:--|:--|:--|:--|:--|:--|:--|:--|:--|:--|:--|:--|:--|:--|:--|:--|
| 8x | 00C4 | 00C5 | 00C7 | 00C9 | 00D1 | 00D6 | 00DC | 00E1 | 00E0 | 00E2 | 00E4 | 00E3 | 00E5 | 00E7 | 00E9 | 00E8 |
| 9x | 00EA | 00EB | 00ED | 00EC | 00EE | 00EF | 00F1 | 00F3 | 00F2 | 00F4 | 00F6 | 00F5 | 00FA | 00F9 | 00FB | 00FC |
| Ax | 2020 | 00B0 | 00A2 | 00A3 | 00A7 | 2022 | 00B6 | 00DF | 00AE | 00A9 | 2122 | 00B4 | 00A8 | 2260 | 00C6 | 00D8 |
| Bx | 221E | 00B1 | 2264 | 2265 | 00A5 | 00B5 | 2202 | 2211 | 220F | 03C0 | 222B | 00AA | 00BA | 03A9 | 00E6 | 00F8 |
| Cx | 00BF | 00A1 | 00AC | 221A | 0192 | 2248 | 2206 | 00AB | 00BB | 2026 | 00A0 | 00C0 | 00C3 | 00D5 | 0152 | 0153 |
| Dx | 2013 | 2014 | 201C | 201D | 2018 | 2019 | 00F7 | 25CA | 00FF | – | – | – | – | – | – | – |

(Unicode code points in hexadecimal; row = high nibble, column = low
nibble.)

**From name to delivered path** (DECISION D13):

1. Convert each byte by the table; bytes below 0x20 and undefined bytes
   become U+FFFD.
2. Replace `/` inside a name by `-`. Both existing extraction tools the
   author could compare with (Aaru, and the tool that produced the
   reference extraction of section 21.2) do the same, and `-` cannot
   occur in a genuine name, so the mapping is reversible. Keeping `/` as
   a path separator would turn the Workshop naming habit into
   directories, but a volume may hold both `A` and `A/B`, which cannot
   coexist as a file and a directory.
3. Join real directory components (version 17, section 8.2) with `/`.
4. Pass the result through the crate's archive-path sanitiser like every
   other container does.

## 11. File types and objects that are not user files

The `ftype` byte of the hint entry:

| Value | Name | Seen |
|--:|:--|:--|
| 0 | undefined | no |
| 1 | MDDF | no |
| 2 | root catalog | yes, S-file 4 of versions 14 and 15 |
| 3 | free list (bitmap) | no |
| 4 | bad blocks | no |
| 5 | system data | no |
| 6 | spool | no |
| 7 | executable | no |
| 8 | user directory | no |
| 9 | pipe | yes, for pipes and event channels |
| 10 | boot file | no |
| 11 | swap data | no |
| 12 | swap code | no |
| 13 | (RAM application) | no |
| 14 | user file | yes, every listed file including programs and system files |
| 15 | killed object | no |
| 16 | temporary file | no |

Values 2, 9 and 14 are CONFIRMED (real data); the rest is REFERENCE and
APPLE-SOURCE (16 is APPLE-SOURCE only).

What is and is not a user file (DECISION D14):

- A user file is a catalog entry of type 3. Its `ftype` is not used as a
  filter, because the catalog type already separates files from pipes,
  event channels and directories, and every type-3 entry examined has
  `ftype` 14.
- Not user files, never listed: the boot block and loader; the MDDF and
  its backup; the bitmap; the S-list; the catalog itself; leaders; pipes
  (type 4); event channels (type 5); deleted slots (types 0, 6, 7);
  thread records (type 8); anything reachable only through tags and not
  through the catalog.
- System files of the operating system (`system.os`, `SYSTEM.LLD`,
  `{!CATALOG}` …) are ordinary catalog entries and **are** listed.

## 12. Timestamps

A timestamp is an unsigned 32-bit count of seconds since 1 January 1901,
00:00, in the local time of the machine that wrote it (no time zone is
recorded). Subtracting 2,177,452,800 gives seconds since 1 January 1970.
Zero means "never". CONFIRMED (real data): a volume initialised by the
installer with the name `AOS 8:55 am 4/15/1983` has `DT_created`
0x9AC67799 = 15 April 1983, 09:00:09. REFERENCE: Aaru uses the same
epoch.

Where they are: MDDF 0x58 to 0x64; hint entry 0x2E to 0x3E; version-17
catalog records 0x28 and 0x2C.

## 13. Devices and sizes

| Device | Blocks (N) | Tag | Volume start | B | `blockcount` |
|:--|--:|:--|--:|--:|--:|
| 3.5-inch 400K diskette, version 15 | 800 | 12 | 0 | 28 | 792 |
| 3.5-inch 400K diskette, version 17 | 800 | 12 | 0 | 28 | 800 |
| Twiggy (FileWare) diskette, version 14 | 1,702 | 12 | 0 | 41 | 1,684 |
| ProFile 5 MB | 9,728 | 20 | 8 | 38 | 9,720 |
| Larger ProFile-protocol disk (one 94,208-block image examined) | 94,208 | 20 | 8 | 38 | 94,200 |
| ProFile 10 MB, Widget 10 MB | 19,456 | 20 | 8 (INFERRED) | 38 (INFERRED) | 19,448 (INFERRED) |

- The data size of a block is always 512 bytes; `cluster_size` is always
  1. No volume with another block or cluster size was found in any
  source.
- On diskettes the blocks after `blockcount` (8 on a version-15
  3.5-inch diskette, 18 on a Twiggy diskette) are outside the file
  system. On Twiggy media that region is the drive's spare area
  (section 13.1).
- The reader takes no parameter from this table. It derives everything
  from B and the MDDF; the table exists to make test expectations and
  detection plausible. The one exception is the Twiggy geometry that
  section 13.1 requires.

### 13.1 The Twiggy bad-block table

REFERENCE (Aaru): the spare area of a Twiggy diskette may hold a
bad-block table at block 1,684 (signature `4A FC`, version 1) that
relocates up to 16 blocks. The four Twiggy images first examined have
no such table. A later one, `twiggy_LisaTerminal_1_Scott_Lewis`, has
one, and its layout was measured on that image (CONFIRMED (real data), one
specimen, three entries):

| Offset | Size | Field | Value there |
|--:|--:|:--|:--|
| 0 | 2 | signature | `4A FC` |
| 2 | 2 | version | 1 |
| 4 | 2 | number of entries, *n* | 3 |
| 6 | 2 × *n* | device block numbers of the relocated blocks | 55, 223, 210 |
| | | rest of the block | `FF` |

- Entry *i* (from 0) is replaced by device block 1,685 + *i*. On the
  specimen, blocks 55 (catalog relative page 5), 223 and 210 (leaders of
  S-files 13 and 10) carry all-zero tags, and blocks 1,685, 1,686 and
  1,687 carry the tags those blocks should have (file ID 4 relative
  page 5; file IDs −13 and −10). With the relocation applied all nine
  files of the volume are read and match an independent extraction;
  without it the catalog is unreadable.
- The block numbers are device blocks (page + B), not pages.
- DECISION D15: the reader applies the table when **all** of these hold,
  and otherwise relocates nothing: the image has exactly 1,702 blocks;
  the volume starts at device block 0 and `blockcount` is 1,684; block
  1,684 starts with `4A FC`, version 1; *n* is 1 to 16; every entry is
  below 1,684 and no block is listed twice. A relocated block is
  replaced for every read of its data and its tag. The MDDF search of
  section 4.2 does not consult the table.
- Not known: whether a table can list fewer blocks than it has spare
  slots in use (holes), and what version values other than 1 mean.

## 14. When structures disagree

The file system is redundant: the catalog names an S-file, the S-list
points at its leader, the leader's map points at data, and every block's
tag says whom it belongs to. Real images disagree in the ways below. The
rules are DECISION D16 unless labelled otherwise.

1. **The lookup structures locate; tags verify.** The reader always
   navigates by MDDF, S-list, map and catalog. It never reconstructs a
   file from tags alone and never lists a file that the catalog does not
   name.
2. **Stale S-list entry.** If the leader page named by the S-list is not
   tagged with the negated S-file number at relative page 0, the entry
   is skipped. Seen on three Twiggy images (21, 4 and 16 entries); the
   cause there is the order of blocks in the image, not the file system
   (section 20). Aaru reacts by searching every tag of the device for
   the leader, once per file; this reader does not search.
3. **Zero-length file with a map.** A file whose `filesize` is 0 may
   have a map that lists pages; those pages may be untagged or belong to
   other files (`system.IUDirectory`, the `system.stack` and
   `system.sysloc` files, pipes). The file is empty. Its map is not read.
4. **Data page with a foreign tag.** Each page that contributes bytes to
   a file (the first ⌈L / 512⌉ pages) must have a tag whose file ID
   equals the S-file number. If any does not, the whole entry is skipped
   ("page tags do not match"). On the 24 volumes whose images are in
   block order this never happens. Pages past L are not
   checked: allocated-but-unwritten pages are commonly untagged.
5. **Relative page, links, data used, checksum.** Not checked. On
   healthy volumes relative pages count from 0 without gaps and the last
   page's *data used* matches L, which makes them good material for
   tests but poor grounds for rejecting data.
6. **Catalog name versus hint-entry name.** Equal on every examined
   file. The catalog name is used; a difference is ignored.
7. **Catalog-tree node pages not tagged file ID 4**: the node is invalid
   and enumeration stops with the entries found so far (section 17.3).
8. **Two catalog entries naming the same S-file**: both are delivered;
   the output budget (section 17.4) bounds the cost. Entries naming a
   file that is then skipped are bounded by the work budget of the same
   section.
9. **MDDF and backup MDDF differ**: the primary is used (D4).
10. **`vol_mounted` set, or scavenger flags set**: ignored. Many genuine
    images were taken from disks that were not cleanly unmounted.

## 15. Volumes that are not Lisa FS

A Lisa disk image with tags may hold something else. None of these has a
block tagged with file ID 1 among its first 100 blocks, so section 4.2
rejects them; they are described so that tests can include them and so
that nobody mistakes them for damaged Lisa FS.

| System | What the image looks like | Evidence |
|:--|:--|:--|
| Lisa Monitor, UCSD p-System (also early Workshop "Monitor" disks) | Block 0: boot code; its tag is twelve `AA` bytes. **All other tags are zero.** Block 2 starts a UCSD directory: 16-bit first block 0, 16-bit last block 6, 16-bit kind 0, then the volume name as a length byte and up to 7 characters (`BLANK`, `ROOT1`, `PREBOOT`), followed by 26-byte file entries with names like `CHANGEMEM.OBJ`. | CONFIRMED (real data), three images. |
| Xenix | Block 0: boot code, tag twelve `AA` bytes; all other tags zero. Block 1 is a Unix System III/V-style superblock (first words: inode-area size, then the volume size in blocks, here 9 and 800), followed by inode blocks. | CONFIRMED (real data), one image; the superblock reading is INFERRED. |
| UniPlus (Unix System V) | A Unix file system on a ProFile; no Lisa tags of meaning. | INFERRED; no specimen. |
| MacWorks (Macintosh disks used on a Lisa) | A Macintosh MFS or HFS volume: signature `D2 D7` (MFS) or `42 44` (HFS) at byte offset 1024, and Macintosh-style tags (a 32-bit file number first), not Lisa tags. The MacWorks boot diskette itself can be a Monitor-format disk (the one examined is, with volume name `PREBOOT`). | Signatures are common knowledge and already implemented in this crate; the boot disk is CONFIRMED (real data). |

A reader must not try to "repair" detection by looking for an MDDF
without tags: the rule of section 4.2 is what keeps these volumes, and
Macintosh volumes in particular, flowing to their own readers.

## 16. Worked examples

All offsets in 16.1 to 16.5 are for the DiskCopy 4.2 file
`688-0036_Pascal_Workshop_3.0_1.dc42` (419,284 bytes, SHA-256
`e6ed166c82e7c1ab1ed77b31dd01ead7d74afcfada5582e313dd1e0ef6af6128`,
from `bitsavers.org/bits/Apple/Lisa/workshop_3.0/`). In that wrapper the
data of block *n* is at file offset 84 + 512 × *n* and its 12-byte tag
at file offset 409,684 + 12 × *n* (800 blocks). The volume is version
17, named `Workshop Pascal 1 3.0`.

### 16.1 Tags and finding the MDDF

| Block | Tag at file offset | Tag bytes | Reading |
|--:|--:|:--|:--|
| 0 | 409,684 | `00 00 00 24 AA AA 00 00 87 FF 07 FF` | Boot block; relative page 0; data used 16 × 32 + 0 = 512; no links. |
| 1 | 409,696 | `00 00 00 24 BB BB 00 01 87 FF 07 FF` | Loader, relative page 1. Blocks 1 to 26 are loader. |
| 27 | 410,008 | twelve zero bytes | Free. |
| 28 | 410,020 | `00 00 00 00 00 01 00 00 4F FF D7 FF` | File ID 1, relative page 0: **MDDF, B = 28**. Data used: word `4F FF` → top 5 bits 01001 = 9; word `D7 FF` → top 5 bits 11010 = 26; 9 × 32 + 26 = 314, the size of a version-17 MDDF. Both links 0x7FF: none. |
| 29 | 410,032 | `00 00 00 00 00 02 00 00 1F FF 08 00` | Bitmap; data used 3 × 32 + 1 = 97 bytes (772 bits). |
| 30 | 410,044 | `00 00 00 00 00 03 00 00 80 03 07 FF` | S-list, relative page 0, data used 512, forward link 3 (page 3 = block 31), no backward link. |
| 34 | 410,092 | `00 00 03 00 00 04 00 00 80 07 07 FF` | Catalog node, first of 4 pages (page 6), forward link page 7. |
| 39 | 410,152 | `00 01 01 00 FF FA 00 00 07 FF 07 FF` | File ID −6: leader of S-file 6; data used 0. |
| 298 | 413,260 | `00 00 00 00 00 01 00 00 4F FF D7 FF` | Backup MDDF at page 270 (28 + 270). |

### 16.2 The MDDF (block 28, file offset 14,420)

| Offset | Bytes | Field = value |
|--:|:--|:--|
| 0x00 | `00 11` | fsversion = 17 |
| 0x0C | `15 57 6F 72 6B …` | volname, length 21: `Workshop Pascal 1 3.0` |
| 0x58 | `9D 0C 22 80` | DT_created = 29 June 1984, 13:56:16 |
| 0x6C | `00 00 00 1C` | firstblock = 28 (= B, so the volume starts at block 0) |
| 0x70 | `00 00 03 1F` | lastblock = 799 |
| 0x74 | `00 00 03 03` | lastfspage = 771 = 799 − 28 |
| 0x78 | `00 00 03 20` | blockcount = 800 |
| 0x7C | `02 18` | blocksize = 536 |
| 0x7E | `02 00` | datasize = 512 |
| 0x82 | `00 00 00 00` | MDDFaddr = 0 |
| 0x86 | `01 3A` | MDDFsize = 314 |
| 0x94 | `00 00 00 02` | slist_addr = page 2 (block 30) |
| 0x98 | `00 24` | slist_packing = 36 |
| 0x9A | `00 04` | slist_block_count = 4 |
| 0x9C | `00 05` | first_file = 5 |
| 0xA0 | `00 6B` | maxfiles = 107 (< 36 × 4) |
| 0xA2 | `00 01` | hintsize = 1 |
| 0xA8 | `01 80` | flabel_offset = 384 |
| 0xAC | `00 00` | map_offset = 0 |
| 0xAE | `00 54` | map_size = 84 |
| 0xBE | `00 04` | rootsnum = 4 |
| 0x114 | `00 80` | smallmap_offset = 128 |
| 0x120 | `00 80` | flabel_size = 128 |
| 0x12E | `00 00 00 06` | root_page = page 6 (block 34) |
| 0x132 | `00 01` | tree_depth = 1 |

All ten checks of section 5.3 pass.

### 16.3 The catalog (node at page 6, block 34, file offset 17,492)

- Descriptor at node offset 0x7F4: `00 16 FF FF FF FF FF FF FF FF 00 FF`
  → nkeys = 22, no prior, no next, kind 0 (leaf).
- Offset table, read downward from 0x7F2: `00 00`, `00 4E`, `00 8E`,
  `00 CE`, `01 0E` … so record 0 is 0x00 to 0x4E (78 bytes), record 1 is
  0x4E to 0x8E (64 bytes), record 3 is 0xCE to 0x10E (64 bytes).
- Record 0: key `24 00 00` followed by 33 zero bytes; type byte at 0x24
  is `08`: the root's thread record. Ignored.
- Record 3, at node offset 0xCE:

| Record offset | Bytes | Field = value |
|--:|:--|:--|
| 0x00 | `24` | key marker |
| 0x01 | `00 00` | parent directory 0 (root) |
| 0x03 | `66 6F 6E 74 2E 6C 69 62 00 …` | name `font.lib` |
| 0x24 | `03` | type 3: file |
| 0x25 | `5A` | padding (garbage) |
| 0x26 | `00 06` | sfile = 6 |
| 0x28 | `9C 6F 59 50` | created 2 March 1984, 15:44:16 |
| 0x2C | `9C 6F 59 5D` | modified 2 March 1984, 15:44:29 |
| 0x30 | `00 00 16 72` | size = 5,746 |
| 0x34 | `00 00 18 00` | physSize = 6,144 (12 pages) |
| 0x38 | `00 01` | fsOvrhd = 1 |
| 0x3A | `20 00` | flags |

The 21 file records of this node are, in order: `apin/syslib.obj`,
`font.heur`, `font.lib`, `installwsphrase`, `Intrinsic.Lib`,
`iospaslib.obj`, `system.bt_Priam Disk`, `system.bt_Profile`,
`system.cdd`, `system.cd_2 Port Card`, `system.cd_Archive Tape`,
`system.cd_Console`, `system.cd_Priam Card`, `system.cd_Priam Disk`,
`system.cd_Profile`, `system.cd_Sony`, `system.IUDirectory`,
`system.lld`, `system.os`, `system.shell`, `SYSTEM.UNPACK` (note the
case-insensitive order and the preserved case). The first is delivered
as `apin-syslib.obj`.

### 16.4 S-list entry and leader of S-file 6

- S-list entry 6: page 2 + (6 div 36) = page 2 (block 30), byte offset
  6 × 14 = 84; file offset 15,528:
  `00 00 00 0B 00 00 00 FC 00 00 16 72 00 01` → hintaddr = page 11,
  fileaddr = page 252, filesize = 5,746, version = 1.
- Leader: page 11 = block 39, file offset 20,052. Its tag (16.1) has
  file ID −6 and relative page 0, so the entry is trusted.

| Leader offset | Bytes | Field = value |
|--:|:--|:--|
| 0x00 | `08 66 6F 6E 74 2E 6C 69 62` | name, length 8: `font.lib` (bytes 0x09 to 0x21 are garbage) |
| 0x2A | `00 15` | version = 21 |
| 0x2C | `0E` | ftype = 14, user file |
| 0x2E | `9C 6F 59 50` | DTC |
| 0x32 | `A0 6C 0C 19` | DTA |
| 0x36 | `9C 6F 59 5D` | DTM |
| 0x62 | `00` | password length 0 |
| 0x80 | `00 00 00 0C` | map: size = 12 pages |
| 0x84 | `00 09` | max_entries = 9 (ten entries) |
| 0x86 | `00 01` | ecount = 1 |
| 0x88 | `00 00 00 FC 00 0C` | entry 0: address = page 252, cpages = 12 |
| 0x8E | `00 00 00 00 00 00` | entry 1: address 0, end of map |
| 0x180 | 128 zero bytes | label (empty) |

### 16.5 The contents of `font.lib`

- L = 5,746 → 12 pages needed; the map supplies pages 252 to 263 =
  blocks 280 to 291.
- First page, block 280, tag `00 01 00 00 00 06 00 00 80 FD 07 FF`: file
  ID 6, relative page 0, data used 512, forward link page 253.
- Last page, block 291, tag `00 01 00 00 00 06 00 0B 1F FF 91 06`: file
  ID 6, relative page 11; data used: `1F FF` → 3, `91 06` → top 5 bits
  10010 = 18; 3 × 32 + 18 = 114 = ((5,746 − 1) mod 512) + 1; no forward
  link; backward link 0x106 = page 262.
- Contents: file offsets 143,444 to 149,189 (11 full blocks and 114
  bytes). First 16 bytes `00 2A 00 03 09 53 59 53 50 41 54 53 2E 46 00
  00`. SHA-256 of the 5,746 bytes:
  `4170c235ce83b566f83e03e7f88937985992c2d59ad05de15260690e05fd72e1`.

### 16.6 A flat catalog (version 15)

File `ws2.0_1.dc42` (SHA-256
`3a32a983745e6090126a7161301927b3f9887d72b8ef3fce076769e2c9847d31`,
from `bitsavers.org/bits/Apple/Lisa/workshop_2.0/`), volume
`MacWS 1 2.0`, B = 28, `blockcount` 792, `rootmaxentries` 107,
`first_file` 4.

- S-list entry 4: hintaddr 6, fileaddr 7, filesize 5,778 = 107 × 54.
- Leader at page 6 (tag file ID −4): name `rootcatalog`, ftype 2; small
  map at 0x80: `00 00 00 0C 00 09 00 01 00 00 00 07 00 0C` → one extent,
  pages 7 to 18 (blocks 35 to 46, all tagged file ID 4; the last has
  data used 146 = 5,778 − 11 × 512).
- Used slots: 21, 22, 24, 27, 29, 39, 42, 45, 46, 48, 54, 55, 85, 86, 87,
  91 (16 entries, all type 3).
- Slot 42 is at catalog byte offset 2,268 (file offset 18,004 + 2,268):
  `08 66 6F 6E 74 2E 6C 69 62` (name `font.lib`), 25 garbage bytes, then
  at slot offset 0x22 `03 00` (type 3 and padding) and at 0x24 `00 06`
  (sfile = 6). S-file 6 has filesize 4,746 and one extent of 10 pages;
  its last page's tag has data used 138.
- Slot 87 names `SYSTEM.LOG`, S-file 14, filesize 16: one page, data
  used 16.
- The entry `system.IUDirectory` (S-file 12) has filesize 0 and fileaddr
  0, yet its map lists 20 pages at page 725 whose tags are all zero: an
  empty file (section 14, rule 3).

### 16.7 A version-14 leader

File `t021_Lisa_Draw.dc42` (SHA-256
`ffe41f9110195180ef6c250d0decd5307f3e1228fd6f1100b84cef7eaf7a403d`,
from `bitsavers.org/bits/Apple/Lisa/twiggy/`), a Twiggy image: B = 41,
`hintsize` 2, `map_offset` 1, `rootmaxentries` 287.

- S-file 5 (`{!CATALOG}`): S-list entry hintaddr 44, fileaddr 46,
  filesize 2,560.
- Leader pages 44 and 45 (blocks 85, 86): tags file ID −5, relative
  pages 0 and 1; the first page's forward link is 45.
- The map is the whole of page 45: size 5, max_entries 83, ecount 2,
  entries (46, 1) and (50, 4): five pages for 2,560 bytes.
- The root catalog is S-file 4: hintaddr 11, one extent of 31 pages at
  page 13, filesize 15,498 = 287 × 54.

### 16.8 20-byte tags and the hard-disk offset

From a 94,208-block ProFile-format image holding a version-17 volume
(section 21.1, specimen H2), after the image layer has put the blocks in
device order:

| Device block | Tag bytes | Reading |
|--:|:--|:--|
| 0 | `00 00 00 24 AA AA 82 00 FF FF FF 53 00 00 FF FF FF FF FF FF` | Boot block; flag set, data used 0x200 = 512; absolute page none; checksum 0x53; relative page 0; no links. |
| 38 | `00 00 00 00 00 01 81 3A 00 00 00 32 00 00 FF FF FF FF FF FF` | MDDF: B = 38. Data used 0x13A = 314; absolute page 0; checksum 0x32. The MDDF says firstblock = 30, blockcount = 94,200: the volume starts at device block 8. |
| 232 | `00 01 01 00 FF FB 80 00 00 00 C2 AE 00 00 FF FF FF FF FF FF` | Leader of S-file 5; data used 0; absolute page 0xC2 = 194 = 232 − 38. |
| 9,454 | `00 01 00 00 00 05 82 00 00 24 C8 1A 00 00 00 24 C9 FF FF FF` | First data page of S-file 5: absolute page 0x24C8 = 9,416; relative page 0; forward link 9,417; no backward link. |
| 9,475 | `00 01 00 00 00 05 82 00 00 24 DD 75 00 15 FF FF FF 00 24 DC` | Its 22nd and last page: relative page 21, backward link 9,436; the file is 11,264 bytes, so data used is 512. |

Blocks 9 to 37 of that image are the loader (relative pages 1 to 29) and
blocks 1 to 8 lie outside the volume.

## 17. Safety requirements for untrusted input

Every field read from the image is untrusted, including the MDDF after it
passed section 5.3. The reader must terminate, must never read outside
the image, and must bound its memory and output.

### 17.1 Addresses

- Every page number is checked against **both** `lastfspage` and N
  (section 5.3) before the block or its tag is read. Page arithmetic
  (B + page, address + count) is done without wrap-around.
- A catalog node needs 4 pages in range; a version-14 leader needs
  `hintsize` pages in range.
- S-list access: the page `slist_addr` + (*f* div packing) must lie
  within the S-list's `slist_block_count` pages.

### 17.2 File maps

- An extent is valid when address ≥ 1, cpages ≥ 1 and address + cpages −
  1 ≤ `lastfspage` (and within N). The first invalid extent, other than
  the terminating address ≤ 0, makes the entry a skipped entry.
- The reader stops reading the map as soon as the extents collected
  cover ⌈L / 512⌉ pages; it never materialises pages beyond L.
- The total number of pages collected for one file may not exceed
  `lastfspage` + 1. This bounds maps whose extents overlap or repeat.
- Continuation chain: each continuation page must be in range and tagged
  with the negated S-file number; the chain is abandoned (keeping what
  was collected, which then falls under D12) when a page fails this, and
  the number of continuation pages followed for one file may not exceed
  ⌈(`lastfspage` + 1) / 84⌉. Since each page of a valid chain adds at
  least one extent of at least one page, this bound also defeats cycles.
- A map section's `max_entries` is not trusted: the reader uses 10
  entries for a small map and 84 for a full map regardless.

### 17.3 Catalogs

- Flat catalog: the number of slots examined is what the catalog file's
  logical length holds (`filesize` div 54; section 8.1 explains why
  `rootmaxentries` and `maxfiles` are not bounds). The catalog file is
  read through the same map rules as any file and charged to the same
  budget, which bounds the number of slots by the size of the image.
- Catalog tree, per node, all of: the 4 pages are tagged file ID 4;
  nkeys is at most 50 (no more 38-byte records with their offset-table
  entries fit in a node); offset-table entry 0 is 0 and the entries 0 to
  nkeys are strictly increasing; entry nkeys is at most
  2,036 − 2 × (nkeys + 1), so that records and offset table do not
  overlap; each record is exactly 40 bytes in an interior node, and in a
  leaf at least 38 bytes and at least the size its type requires; `kind`
  agrees with the level being read (1 above the leaf level, 0 at it).
  A leaf with nkeys 0 is valid and empty. All offset-table entries are
  checked before any record is read through them.
- An invalid node ends enumeration; entries already found are kept and a
  diagnostic is raised.
- Descent is limited to `tree_depth` − 1 steps and `tree_depth` may not
  exceed 16 (the operating system's own traversal stack has 16 frames;
  APPLE-SOURCE). A larger value makes the catalog unreadable.
- The leaf chain is limited to ⌈(`lastfspage` + 1) / 4⌉ nodes, and a
  node already visited ends the walk. `prior` is never followed.
- Directory resolution: at most 16 levels, with a visited set
  (section 8.2).
- Names: a length byte above 32 (flat catalog) is invalid; a tree key
  without a zero byte in its 33 name-and-terminator bytes is cut at 32.

### 17.4 Sizes and budgets

- L is a signed 32-bit value: negative values are rejected, and no
  buffer is sized from L before it has been reduced to what the map and
  the image can supply.
- Every delivered file is charged against the container's output budget,
  as the other file system readers of this crate do; two catalog entries
  naming the same S-file are charged twice.
- A skipped entry delivers nothing and so is not charged to the output
  budget, yet finding out that it must be skipped can cost a walk over
  its map and the tags of all its pages. The reader therefore counts
  the map sections, extents and tags examined for entries that end up
  skipped, and stops reading files (keeping those already delivered,
  with a diagnostic) once the count exceeds 4 × (`lastfspage` + 1), or
  4,096 if that is larger. Distinct files of a genuine volume cannot
  reach this: together they cover each page at most once. Without it,
  many catalog entries naming one large unreadable file make the work
  quadratic in the size of the image.
- The number of catalog entries is bounded by the structures above
  (`maxfiles` ≤ 32,766 in practice; the S-list bound of check 8 limits
  it to 36 × `slist_block_count`).
- Detection (sections 4.2 and 5.3) reads at most 100 tags and the blocks
  that have file ID 1 among them; it allocates nothing proportional to
  the image.

## 18. Observed implementation gaps

Nothing in this section is to be imitated.

1. **Aaru: MDDF offsets.** Its table names the bytes 0x80 to 0x93 as
   three unknowns, a cluster size at 0x8A and a file system size at
   0x8C, and shifts the fields after 0x112 (scavenger result at 0x11C,
   small-map offset at 0x11E, label size at 0x126 …). Real volumes show
   the layout of section 5.1 (cluster size at 0x80, small-map offset at
   0x114, label size at 0x120). Aaru does not use the misplaced fields
   for reading.
2. **Aaru: file size and file map header.** It calls the map header's
   `size` "length" and the two words after it "physical length"; the
   latter are `max_entries` and `ecount`. It sizes its output from the
   map (allocated pages × 512) and reports the S-list size separately,
   so its file reads can return the allocation, not the content.
3. **Aaru: extent count.** It scans up to 41 six-byte entries from the
   start of the map in every version, stopping at a zero length. A small
   map has 10 entries; entries 11 to 41 are other leader bytes. A full
   map has 84. Continuation pages are not followed.
4. **Aaru: no bounds.** Extent addresses and lengths are used without
   range checks; a negative 16-bit length is converted to a large
   unsigned count.
5. **Aaru: version-17 catalog discovery** by scanning all tags and
   following `prior`, without checking the node kind (section 8.2); its
   record walk steps by fixed sizes chosen from the type byte, treats an
   unknown value 0x7C as "50 bytes of garbage", and never consults the
   offset table or nkeys.
6. **Aaru: flat catalog loop** stops one slot early when the catalog
   length is an exact multiple of 54 bytes.
7. **Aaru: leader search.** When an S-list entry looks wrong it scans
   every tag of the device, for each such file: quadratic on a damaged
   or hostile volume.
8. **Aaru: path lookup** compares the first path element at every level.
9. **Aaru: hint entry.** "Version" at 0x20 and parent identifier at 0x6B
   are misplaced (section 7.1).
10. **Aaru: 12-byte tag.** It masks the link words to 11 bits but does
    not recover *data used* from the top bits, and reports it as 0.
11. **LisaEm `lisafsh-tool`**: its tag comparisons treat the 3-byte
    fields as 23-bit; it guesses file names from leader pages found by
    tag, which lists deleted files as present.

## 19. Decisions

| # | Decision | Where |
|:--|:--|:--|
| D1 | 24-byte (Priam) tags are not supported. | 3.3 |
| D2 | Both tag forms decode to (file ID, relative page, data used, forward link); checksum, absolute page and backward link are never required to be valid. | 3.5 |
| D3 | The MDDF is the lowest block among the first 100 whose tag is (1, 0) and which passes all checks; a failing candidate does not end the search. | 4.2 |
| D4 | The backup MDDF is never used. | 4.3 |
| D5 | The ten acceptance checks. | 5.3 |
| D6 | File and volume passwords and protection flags are ignored; contents are delivered. | 7.1 |
| D7 | The label is not used; desktop names are a follow-up. | 7.2 |
| D8 | The map is walked to its first unused entry; `ecount`, `size` and `max_entries` are not trusted. | 7.3, 17.2 |
| D9 | The version-17 catalog is enumerated by descending through first children and following `next`. | 8.2 |
| D10 | A file with an unresolvable directory chain is listed under the root. | 8.2 |
| D11 | L is the S-list `filesize`; a differing catalog size yields the smaller and a diagnostic. | 9.2 |
| D12 | L is cut to what the map covers, with a diagnostic. | 9.2 |
| D13 | Name mapping: character table, `/` → `-`, directories joined with `/`, then the crate's sanitiser. | 10 |
| D14 | User files are the type-3 catalog entries; `ftype` is not a filter. | 11 |
| D15 | The Twiggy bad-block table is applied only when it is exactly as measured; otherwise nothing is relocated. | 13.1 |
| D16 | The disagreement rules: navigate by structures, verify by tags, skip stale S-list entries and files with foreign data tags, never search tags. | 14 |

## 20. Open questions

1. **Twiggy image block order.** In three of the four Twiggy DiskCopy
   images examined (`t001`, `t008`, Pascal Workshop 1.0 disk 1) blocks 0
   to 850 are in file system order but the blocks from 851 on are not:
   the forward and backward links of their tags show that the second
   half is stored track by track in **descending** logical order (runs
   of 15 to 22 blocks, each run ascending). The file system reader then
   sees stale S-list entries (section 14, rule 2). Whether the image
   layer should reorder such images, and by which rule, belongs to
   `images.md`. The fourth image (`t021`) has no data beyond block 615
   and cannot decide it.
2. **The hard-disk volume start.** S = 8 on both hard-disk images. One
   of them (specimen H1, a BLU-format dump) has non-file-system contents
   in device blocks 1 to 7 whose 532 bytes still exclusive-or to zero.
   What the operating system or the ROM keeps there was not
   investigated. Whether a Widget or a 10 MB ProFile also has S = 8 is
   INFERRED.
3. **Directories in version 17.** No specimen has a directory record
   (type 1). The record layout, the meaning of the identifiers and the
   thread records rest on the Apple source (and, for the 48-byte size,
   on Aaru). A specimen made by the Workshop 3.0 with real
   subdirectories would confirm section 8.2. None was found in the 120
   images of the bitsavers Lisa collection or elsewhere. Three released
   diskettes do hold leftovers of directories from the machines they
   were mastered on, in catalog node space that is no longer in use:
   `LOS_800K-DRIVER` (thread records for identifiers 1 and 4, named
   `Label` and `temp`, both with parent 0, and two file records keyed
   with parent 4) and the Toolkit 3.0 diskettes `TK_Sources_1` and
   `ToolKit_V3.0_4` (file records keyed with parents 2 and 7). These
   OBSERVED leftovers agree with section 8.2 on the thread record (type
   8, parent identifier at 0x26, name as a length-prefixed string after
   it) and on a file's key carrying the identifier of its directory. No
   directory record (type 1) survives in them.
4. **File-map continuation pages** (more than 10 extents on versions 15
   and 17, more than 84 on version 14): APPLE-SOURCE only. A heavily
   fragmented hard-disk specimen would confirm section 7.3.
5. **Password-protected files** and volumes: no specimen. D6 assumes the
   contents are stored in clear.
6. **Files left open.** A file whose `file_open` flag is set might have
   a `filesize` that lags behind its written pages. Not observed; if a
   specimen shows it, the tags' *data used* and relative pages could
   recover the true length.
7. **Bytes 0x7F and 0xD9 to 0xFF of the character set**, and whether
   0xD8 to 0xFF follow later Macintosh Roman assignments on the Lisa.
8. **Desktop names.** The label's layout beyond the string at offset 2
   (section 7.2), and the `{!CATALOG}` files that map tool and document
   numbers to names, would let a reader present Office System disks the
   way the Lisa desktop does.
9. **The `flags` word of version-17 file records**: bit assignments are
   APPLE-SOURCE (master, protect, safety, crashed, scavenged, closed)
   and the observed values (0x0000, 0x2000, and garbage-like 0x4058,
   0x005F) do not settle the bit order.

## 21. Evidence

### 21.1 Volumes examined

All images are kept outside the repository. "bitsavers" is
`bitsavers.org/bits/Apple/Lisa/`.

| Id | Image | Device | Version | Notes |
|:--|:--|:--|:--|:--|
| F1 | bitsavers `workshop_3.0/688-0036_Pascal_Workshop_3.0_1.dc42` | 3.5-inch | 17 | Worked example 16.1 to 16.5. |
| F2 | bitsavers `workshop_3.0/688-0037_Pascal_Workshop_3.0_2.dc42` | 3.5-inch | 17 | |
| F3 | bitsavers `toolkit_3.0/TK_Sources_1.dc42` | 3.5-inch | 17 | |
| F4 | bitsavers `applications/lisa_calendar_for_7_7.dc42` | 3.5-inch | 17 | |
| F5–F18 | The 14 `.dc42` images inside bitsavers `office_3.x/Lisa_Office_System_3.1.7z` | 3.5-inch | 17 | Two with tree depth 2. Reference extraction, section 21.2. |
| F19 | bitsavers `workshop_2.0/ws2.0_1.dc42` | 3.5-inch | 15 | Worked example 16.6. |
| F20 | bitsavers `workshop_2.0/682-0073-A_Pascal_2.0_1_cpy1.dc42` | 3.5-inch | 15 | |
| F21 | bitsavers `5-85_sw_suppl/5-85_sw_suppl_1.dc42` | 3.5-inch | 15 | |
| T1 | bitsavers `twiggy/t021_Lisa_Draw.dc42` | Twiggy | 14 | Worked example 16.7. |
| T2 | bitsavers `office_1.0/t001.dc42` | Twiggy | 14 | Block order anomaly (section 20). |
| T3 | bitsavers `office_1.0/t008.dc42` | Twiggy | 14 | Same anomaly. |
| T4 | bitsavers `workshop_1.0/PascalWorkshop1.0_1of3_682-0016-A.dc42` | Twiggy | 14 | Same anomaly. |
| H1 | bitsavers `office_1.0/los1.0_noTools.img` | ProFile 5 MB, BLU dump | 14 | 60 files; 20-byte tags. |
| H2 | `LOS Compilation Base.image` from `github.com/alexthecat123/LisaSourceCompilation` | 94,208-block ProFile-format dump, interleaved, tag first | 17 | 389 files, 15 leaves, tree depth 2; worked example 16.8. |
| X1 | bitsavers `xenix_3.0_rel1.0/XenixBoot.dc42` | 3.5-inch | – | Xenix; not Lisa FS. |
| X2 | bitsavers `monitor/lk023_mon12boot.dc42` | Twiggy | – | Monitor; not Lisa FS. |
| X3 | bitsavers `monitor_12.3/root1.dc42` | 3.5-inch | – | Monitor; not Lisa FS. |
| X4 | `macworks_lisabug.dc42` inside bitsavers `macworks/MacWorks_LisaBug.7z` | 3.5-inch | – | Monitor-format boot disk; not Lisa FS. |

That is 27 Lisa FS volumes (21 3.5-inch diskettes, 4 Twiggy diskettes, 2
hard disks; 19 of version 17, 3 of version 15, 5 of version 14) and 4
volumes that are not.

### 21.2 Independent comparison

The archive behind F5 to F18 also contains, for each diskette, a
directory of files extracted by a different tool (its log names the
Applesauce software). For all 196 non-empty files on the 14 diskettes
the bytes produced by the rules of section 9 (first L bytes of the
mapped pages) are identical to that tool's files; the 9 remaining
catalog entries have L = 0 and are absent from the other tool's output.
That tool writes `apin/syslib.obj` as `apin-syslib.obj`.

### 21.3 Aggregate observations

- 925 catalog entries read: 878 files, 3 pipes, 1 event channel, 24
  removed slots, 19 root thread records.
- On the 24 volumes in block order: every trusted file's data pages
  within L carry the file's ID, relative pages count from 0, and the
  last page's *data used* matches L. No exceptions.
- Name bytes seen: space, `!`, `$`, `.`, `/`, digits, letters, `_`, `{`,
  `|`, `}`. Longest name 30 bytes.
- 20-byte tag checksum: zero exclusive-or over 532 bytes on all 29,719
  flagged blocks tested (all of H1, the first 20,000 blocks of H2).

### 21.4 Statements that rest solely on the Apple Lisa OS source

Should the owner decide not to rely on that source, these are the
statements to remove or re-derive. None is needed to read any of the
examined volumes.

1. The in-memory 24-byte page label and the names of its fields (3.1).
   The on-disk tag layouts themselves are confirmed by real data.
2. File ID 32767 as the number of temporary S-files (3.4).
3. The reason for the hard-disk volume start (4.2): a per-device
   configured volume start with the boot block written at absolute
   block 0.
4. The meaning of MDDF fields a reader does not use (5.1).
5. The meaning of hint-entry fields 0x42 to 0x60, 0x6C, 0x6E (7.1), and
   that passwords are checked at open time with a one-way scramble.
6. File-map continuation through forward links, and the tag of
   continuation pages (7.3).
7. Entry types 1, 2 and 6 (8.1); file type 16 (11).
8. Key ordering and ASCII-only case folding (8.2, 10).
9. Directory records: the `id` field's role, and one thread record per
   directory (8.2). (The 48-byte and 78-byte sizes and the type values 1
   and 8 are also in Aaru.)
10. `-` and `+` as path delimiters that cannot occur in names (10).
11. The 16-level limit of the tree (17.3).
12. The bit names of the `flags` word (20, item 9).

### 21.5 What other sources say about the statements of 21.4

Checked after this document was written, against Apple's published
manuals and against disk images. The manuals are the *Operating System
Reference Manual for the Lisa* (1983, 029-0414-A), the *Release 3.0
Notes* to it, and the Workshop 3.0 release notes bound with the
*Development System Internals Documentation* (February 1984), all as
scanned by bitsavers. None of them describes the on-disk layout, so they
back meanings and names, never offsets.

| Item of 21.4 | Other source | What remains from the Apple source only |
|:--|:--|:--|
| 1. In-memory page label | None. | All of it. A reader does not need it: the on-disk tags are CONFIRMED. |
| 2. File ID 32767 as temporary | LisaEm and Aaru give 32767 as erased blocks. | The "temporary S-file" meaning. Not needed. |
| 3. Reason for the hard-disk volume start | None. The start itself is CONFIRMED on real disks. | The reason. Not needed. |
| 4. Meaning of unused MDDF fields | The reference manual's volume information record names and explains the same attributes (dates of creation, backup and scavenging, password, master copy identifier, scavenge flag). | Which offset holds which attribute. |
| 5. Hint-entry fields, passwords | The Release 3.0 Notes list the per-file attributes (creation and modification dates, size, physical size, file system overhead, the six flags). Password protection of files is described in the Workshop 3.0 notes. | Which offset holds which attribute; that the password is stored scrambled and checked at open time. |
| 6. File-map continuation pages | None, and no specimen. | All of it. This is the only item that changes what a reader can read: without it a file with more extents than one map page holds is skipped. |
| 7. Entry types 1, 2, 6; file type 16 | The Release 3.0 Notes give two object types for a lookup, file entry and catalog entry, and say pipes were removed in 3.0. Type 8 is OBSERVED. | Types 2 and 6 and file type 16. A reader lists none of them. |
| 8. Key ordering, ASCII-only folding | The Release 3.0 Notes limit names to 32 characters. | The comparison rule. A reader enumerates every record and never compares keys. |
| 9. Directory records | The Release 3.0 Notes describe `MAKE_CATALOG` and the directory hierarchy. The leftovers of section 20 item 3 show, on real disks, a thread record per directory identifier and file keys that carry it. | The directory record (type 1) itself: no copy survives on any disk examined. |
| 10. `-` and `+` as delimiters | Both are in the Release 3.0 Notes: the dash separates the parts of a path name, and a plus stands for it to name the parent directory. The Workshop 3.0 notes call the dash "the catalog delimiter". | Nothing. |
| 11. 16-level limit | None. | The number. As a bound on the reader's own work it could equally be a DECISION. |
| 12. Names of the `flags` bits | The Release 3.0 Notes name and explain all six: master, protected, safety, left open (after a crash), scavenged, closed by the Operating System. | Which bit is which. |

Items 10 and 12 no longer depend on the Apple source for anything a
reader shows. Items 1, 2, 3 and 11 can be deleted or restated without
changing the reader. Items 4, 5, 7, 8 and 9 keep an Apple-source part
that a lister does not use. Item 6 is the one rule a reader implements
that has no other backing.
The owner has decided to keep it (2026-11-13): the reader follows
continuation pages as section 7.3 describes, and the rule stays labelled
APPLE-SOURCE until a specimen or another document confirms it.
