# NeXT disk label (`NeXT`, `dlV2`, `dlV3`), read-only

Format specification for clean-room implementation. This document
describes everything a reader needs to find the partitions of a disk,
floppy or CD image that carries a NeXT disk label:

- where the label and its copies are stored on each kind of medium, and
  how a label is recognised and validated (sections 2 to 6);
- the partition table inside the label and how each partition's byte
  range is computed, including the front porch and the units of every
  quantity (sections 4 and 7);
- how the label sits inside a PC (MBR) partition of type 0xA7 on
  NEXTSTEP and OPENSTEP for Intel processors (section 8);
- what the reader yields (section 9), worked byte-level examples
  (section 10), test vectors, decisions and safety requirements
  (sections 11 to 14).

The file system inside a partition is specified in [`ufs.md`](ufs.md).

The document contains no program code and no pseudo-code.

## 1. How to read this document

### 1.1 Provenance

- The layout is described from the label definitions published by Apple
  in Darwin 0.3 and in xnu-123.5 (`disk_label.h`, `disktab.h`), from the
  label handling of Darwin 0.3's `driverkit`, boot loader and `disk`
  utility, from Mac OS X 10.0's partition-scheme driver, and from
  NetBSD/next68k. All were read for facts only. The index of this
  directory ([`README.md`](README.md)) lists the exact files.
- Every structural statement that could be checked was then checked on
  real media images with a throwaway reader written by the
  specification's author from the rules below (kept outside the
  repository; not part of the specification).
- Images examined (all carry version `dlV3` labels):
  - **NS32-FD-68K**: NEXTSTEP 3.2 boot floppy for NeXT hardware.
  - **NS32-FD-X86a**, **NS32-FD-X86b**: NEXTSTEP 3.2 boot and driver
    floppies for Intel.
  - **OS42-FD-68K**: OPENSTEP 4.2 install floppy for NeXT hardware.
  - **OS42-FD-X86a**, **OS42-FD-X86b**: OPENSTEP 4.2 install and driver
    floppies for Intel.
  - **RH-FD-X86a**, **RH-FD-X86b**: Rhapsody DR2 (5.1) boot and driver
    floppies for Intel.
  - **NS32-CD**: the NEXTSTEP 3.2 CD-ROM (whole image).
  - **OS42-CD**: the first 8 MiB of the OPENSTEP 4.2 user CD-ROM (for
    NeXT, Intel, SPARC and PA-RISC machines).
- No hard disk image, no magneto-optical disk image, and no label of
  version `NeXT` or `dlV2` was examined when this document was written;
  section 15 lists what later specimens showed.

### 1.2 Evidence labels

- **CONFIRMED**: observed on every one of the ten images above, unless a
  narrower set is named.
- **REFERENCE-ONLY**: stated by, or deduced from, the sources of
  section 1.1; not observed.
- **INFERRED**: deduced, unconfirmed.
- **DECISION**: chosen by this specification (listed in section 13).

A statement without a label in sections 3 to 5 is CONFIRMED.

### 1.3 Terms and conventions

| Term | Meaning |
|:--|:--|
| data | The byte sequence handed to the reader: a whole image file, or one partition of an outer partition table. |
| **L** | The number of bytes of data. Untrusted, possibly smaller than the label claims. |
| label | One 7240-byte structure (section 3). A medium carries up to four copies. |
| **R** | The byte offset, within the data, at which a label copy was found. |
| device sector | The sector of the physical device: 512 bytes on floppies and hard disks, 1024 bytes on NeXT magneto-optical disks, 2048 bytes on CD-ROM drives. Not stored in the label. Written **dss**. |
| label sector size | The field `secsize` of the label (section 3.2): the unit of the front porch and of every partition base and size. It may differ from the device sector size. Written **S**. |
| front porch | A reserved area before the first partition that holds the labels and the boot blocks. Its size is the field `front`, in units of S. |
| **A** | The absolute byte position, on the whole medium, of the first byte of the data (0 unless the data is a partition of an outer table; section 7.2). |

All multi-byte integers of the label are **big-endian on every
platform**, including Intel (REFERENCE-ONLY for the general rule: the
Intel sources convert every field from big-endian; CONFIRMED on the six
Intel floppies). The structure is packed as a Motorola 68000 compiler
lays it out: 16-bit and 32-bit fields are aligned to 2 bytes only.

Numbers are decimal unless prefixed `0x`. Byte sequences in code style
are hexadecimal, in disk order. Text fields are fixed-size byte arrays
padded with zero bytes; their text ends at the first zero byte or at the
end of the field (a field may be completely full, without terminator).

## 2. Where labels are stored

### 2.1 The four copies

A medium has room for four label copies. Copy *k* (0 to 3) starts at
device sector *k* × N of the area that holds the label, where N is the
number of device sectors needed to hold 7240 bytes:

| Device sector size | N | Byte offsets of copies 0 to 3 |
|--:|--:|:--|
| 512 | 15 | 0, 7680, 15360, 23040 |
| 1024 | 8 | 0, 8192, 16384, 24576 |
| 2048 | 4 | 0, 8192, 16384, 24576 |

- CONFIRMED for 512-byte sectors on all eight floppies and on NS32-CD;
  for 2048-byte sectors on OS42-CD (copies at 8192 and 24576).
  REFERENCE-ONLY for 1024.
- The copies are identical except for the field `label_blkno`
  (section 3.2), which holds each copy's own position. CONFIRMED: on
  NS32-FD-68K the four copies differ in exactly one byte, byte 7.
- Any copy may be missing or overwritten. A reader must be prepared to
  use any of them (section 6).

### 2.2 Per medium

| Medium | What is at byte 0 | Copies present |
|:--|:--|:--|
| Floppy for NeXT hardware (NS32-FD-68K, OS42-FD-68K) | copy 0 | 0, 7680, 15360, 23040. CONFIRMED |
| Floppy for Intel (the six Intel floppies) | a PC boot sector (first bytes `b8 00 00 fa`, signature `55 aa` at 510, all four partition-table slots zero) | 7680, 15360, 23040 only. CONFIRMED |
| Hard disk of a NeXT machine | copy 0 | 0, 7680, 15360, 23040. REFERENCE-ONLY |
| Hard disk of an Intel machine | the master boot record | none at these offsets; the label is inside the partition of type 0xA7 (section 8). REFERENCE-ONLY |
| NEXTSTEP 3.2 CD-ROM (NS32-CD) | copy 0 | 0, 7680, 15360, 23040: the positions of a 512-byte-sector device although the label sector size is 2048. CONFIRMED |
| OPENSTEP 4.2 CD-ROM (OS42-CD) | a boot header of another architecture (first bytes `80 00 00 00`) | 8192 and 24576 (2048-byte device sectors 4 and 12), plus a left-over copy at 15360 whose `label_blkno` is 30. Nothing at 0, 7680, 16384 or 23040. CONFIRMED |
| Magneto-optical disk | copy 0 | REFERENCE-ONLY: positions for 1024-byte sectors; the optical driver may place the copies elsewhere (section 15) |

- On Intel machines copy 0 is never written, so that the boot sector at
  device sector 0 survives (REFERENCE-ONLY; consistent with the six
  Intel floppies).
- The quad-architecture OPENSTEP CD shows that copy 0 can be replaced by
  a foreign boot header, and that copies written for two different
  device sector sizes can coexist on one medium.
- The rest of the front porch holds boot blocks; their position is in
  the label (`boot_blkno`, section 3.2) and is of no interest to a
  reader. On NS32-FD-68K the boot program at byte 32768 starts with the
  Mach-O magic `fe ed fa ce`.

### 2.3 Candidate offsets

DECISION: the reader looks for a label at exactly these seven offsets of
the data, in this order, and uses the first that passes section 6:

0, 7680, 8192, 15360, 16384, 23040, 24576.

It never scans for the magic numbers elsewhere.

## 3. The label

### 3.1 Versions

The first four bytes select the version:

| Bytes | As a number | Text | Name here |
|:--|:--|:--|:--|
| `4e 65 58 54` | 0x4E655854 | `NeXT` | version 1 |
| `64 6c 56 32` | 0x646C5632 | `dlV2` | version 2 |
| `64 6c 56 33` | 0x646C5633 | `dlV3` | version 3 |

The three versions have the **same field layout** for bytes 0 to 557.
They differ in what follows:

| | Versions 1 and 2 | Version 3 |
|:--|:--|:--|
| Bytes 558 to 7237 | bad-block table: 1670 signed 32-bit entries | bytes 558 to 559 are the checksum; the rest is unused (all zero on NS32-FD-68K) |
| Checksum field | bytes 7238 to 7239 | bytes 558 to 559 |
| Bytes covered by the checksum (section 5) | 0 to 7237 | 0 to 557 |
| Minimum bytes needed to validate | 7240 | 560 |
| Bad-block table | in the label | a separate structure outside the label |

- Versions 1 and 2 differ from each other only in the meaning of a
  bad-block entry (version 1 counts in half tracks of an optical disk,
  version 2 in sectors). A reader ignores the bad-block table in every
  version (REFERENCE-ONLY).
- Everything about versions 1 and 2 is REFERENCE-ONLY.

### 3.2 Fields

Offsets are from the first byte of the label. "s32" is a signed and
"u32" an unsigned 32-bit big-endian integer; "s16"/"u16" likewise.

| Offset | Size | Type | Name | Meaning |
|--:|--:|:--|:--|:--|
| 0 | 4 | u32 | `version` | Section 3.1. |
| 4 | 4 | s32 | `label_blkno` | Position of this copy in **device sectors, counted from the start of the whole medium** (section 7.2). 0, 15, 30, 45 on the floppies and on NS32-CD; 4 and 12 (and 30 in the left-over copy) on OS42-CD. |
| 8 | 4 | s32 | `size` | Size of the medium in sectors. 0 on every image examined; not usable. |
| 12 | 24 | text | `label` | Volume name, for example `Install_3.2`, `NEXTSTEP_3.2`, `RhapsodyInstall`. |
| 36 | 4 | u32 | `flags` | Bit 0x80000000: the label is uninitialised. 0 on every image examined. |
| 40 | 4 | u32 | `tag` | Volume tag (a time stamp taken when the label was written). |
| 44 | 24 | text | `name` | Drive name, for example `Sony MPX-111N 2880-512`, `Floppy Drive-512`. |
| 68 | 24 | text | `type` | Drive type, for example `removable_rw_floppy`, `removable_rw_scsi`, `fixed_rw_scsi`. |
| 92 | 4 | s32 | `secsize` | **S**, the label sector size in bytes. 1024 on the floppies, 2048 on the CDs. |
| 96 | 4 | s32 | `ntracks` | Tracks per cylinder. Not needed. |
| 100 | 4 | s32 | `nsectors` | Sectors per track. Not needed. |
| 104 | 4 | s32 | `ncylinders` | Cylinders. Not needed. |
| 108 | 4 | s32 | `rpm` | Rotational speed. Not needed. |
| 112 | 2 | s16 | `front` | Front porch, in units of S. |
| 114 | 2 | s16 | `back` | Back porch, in units of S. 0 on every image examined. |
| 116 | 2 | s16 | `ngroups` | Number of alternate-sector groups. 0 on every image examined. |
| 118 | 2 | s16 | `ag_size` | Alternate group size. 0. |
| 120 | 2 | s16 | `ag_alts` | Alternates per group. 0. |
| 122 | 2 | s16 | `ag_off` | Offset of the first alternate. 0. |
| 124 | 8 | s32 × 2 | `boot_blkno` | Positions of the two boot blocks in units of S; −1 for "none". Not needed. |
| 132 | 24 | text | `kernel` | Default kernel name, for example `mach_kernel`, `sdmach`, `fdmach`. |
| 156 | 32 | text | `hostname` | Name of the host that wrote the label. |
| 188 | 1 | char | `rootpartition` | Letter of the root partition, `a` on every image examined. |
| 189 | 1 | char | `rwpartition` | Letter of the read-write partition, `b` on every image examined. |
| 190 | 368 | | `partitions` | Eight partition entries of 46 bytes each (section 4). |
| 558 | 2 | u16 | version 3 checksum | Version 3 only (section 5). |
| 558 | 6680 | s32 × 1670 | bad-block table | Versions 1 and 2 only. |
| 7238 | 2 | u16 | version 1/2 checksum | Versions 1 and 2 only (section 5). |

The label is 7240 bytes long in every version.

- Observed front porch values: 96 on NS32-FD-68K, OS42-FD-68K,
  NS32-FD-X86b and both Rhapsody floppies (98,304 bytes); 64 on the
  other three Intel floppies (65,536 bytes); 80 on both CDs
  (163,840 bytes).
- REFERENCE-ONLY: the `disk` utility of Darwin 0.3 gives hard disks a
  front porch of 160 KiB (320 when S is 512, 160 when S is 1024) and
  floppies one of 96 KiB.
- The geometry fields are sometimes nonsense (three Intel floppies claim
  80 tracks per cylinder and 2 cylinders). A reader must not use them.

## 4. Partition entries

### 4.1 Layout

Entry *i* (0 to 7, called partition `a` to `h`) starts at label offset
190 + 46 × *i*.

| Offset in entry | Size | Type | Name | Meaning |
|--:|--:|:--|:--|:--|
| 0 | 4 | s32 | `base` | First sector of the partition, in units of S, **not counting the front porch** (section 7). |
| 4 | 4 | s32 | `size` | Length of the partition in units of S. |
| 8 | 2 | s16 | `bsize` | File system block size in bytes. Advisory. |
| 10 | 2 | s16 | `fsize` | File system fragment size in bytes. Advisory. |
| 12 | 1 | char | `opt` | Optimisation preference: `s` (space) or `t` (time). |
| 13 | 1 | | | Padding. |
| 14 | 2 | s16 | `cpg` | Cylinders per cylinder group. Advisory. |
| 16 | 2 | s16 | `density` | Bytes per inode. Advisory. |
| 18 | 1 | s8 | `minfree` | Minimum free space in percent. |
| 19 | 1 | s8 | `newfs` | Non-zero: create a file system when the disk is initialised. |
| 20 | 16 | text | `mountpt` | Default mount point. Empty on every image examined. |
| 36 | 1 | s8 | `automnt` | Non-zero: mount automatically on insertion. |
| 37 | 8 | text | `type` | File system type name (section 4.3). |
| 45 | 1 | | | Padding. |

### 4.2 Which entries are in use

- An entry is **in use** when `base` ≥ 0 and `size` > 0 (REFERENCE-ONLY
  for the rule; consistent with every image).
- An unused entry has `base`, `size`, `bsize`, `fsize`, `cpg`, `density`
  and `minfree` all set to −1 and every other byte zero. CONFIRMED: on
  every image examined entries `b` to `h` are the 46 bytes
  `ff ff ff ff ff ff ff ff ff ff ff ff 00 00 ff ff ff ff ff` followed by
  27 zero bytes.
- Every image examined has exactly one partition in use, `a`.
- REFERENCE-ONLY: hard disks may have several; the `disk` utility of
  Darwin 0.3 creates `a` and optionally `b`, with `b` following `a`
  directly.

### 4.3 File system type, block and fragment sizes

- Observed type strings: `4.3BSD` (all NEXTSTEP 3.2 and OPENSTEP 4.2
  media) and `4.4BSD` (both Rhapsody floppies). Both mean a UFS1 volume
  as specified in [`ufs.md`](ufs.md); the string does not reliably tell
  which on-disk generation is inside, and the reader does not use it.
- Observed `bsize`/`fsize`: 8192/1024 on all floppies, 8192/2048 on both
  CDs. In every case they equal the block and fragment size recorded in
  the file system's own superblock. They are advisory: the file system
  reader takes its sizes from the superblock, never from the label.
- DECISION: the reader yields every partition that is in use, whatever
  its type string, and leaves it to content detection to recognise what
  is inside.

## 5. Checksum

### 5.1 Algorithm

1. Take the covered bytes (section 3.1: bytes 0 to 557 for version 3,
   bytes 0 to 7237 for versions 1 and 2) **with the four bytes of
   `label_blkno` (label offsets 4 to 7) replaced by zero**.
2. Read them as consecutive big-endian unsigned 16-bit words (279 words
   for version 3, 3619 for versions 1 and 2) and add them up in an
   accumulator of at least 32 bits.
3. Add the upper 16 bits of the sum to its lower 16 bits. If the result
   is greater than 65,535, subtract 65,535 from it.
4. The result, a 16-bit number, must equal the stored checksum, which is
   a big-endian unsigned 16-bit integer.

- CONFIRMED for version 3: on every label copy of the ten images the
  stored value equals the value computed this way. In particular the
  stored checksum is the same in all copies of one medium although their
  `label_blkno` differs, which proves that the field is taken as zero.
- The sum of step 2 cannot overflow 32 bits (at most 3619 × 65,535).
- REFERENCE-ONLY for versions 1 and 2.

### 5.2 A disagreement between sources

The sources disagree on two details; both are harmless on every label
examined:

- Mac OS X 10.0's driver passes a length that is two bytes shorter than
  the range above (it stops at byte 555 for version 3). Bytes 556 and
  557 are the last two bytes of partition entry `h` (the end of its type
  text and its padding byte) and are zero on every label examined, so
  both ranges give the same sum.
- NetBSD folds the sum differently in step 3 (it adds the upper half to
  the whole sum and keeps the lower 16 bits). The two folds differ only
  when the lower 16 bits of the sum plus its upper half reach or exceed
  65,536.

DECISION: the reader uses the algorithm of section 5.1. It does not
accept alternatives.

## 6. Recognising and validating a label

A label copy found at offset R of the data is **valid** when all of the
following hold. Each check must be made without reading beyond L.

1. R is one of the seven candidate offsets (section 2.3).
2. The four bytes at R are one of the three version values.
3. The bytes the version needs are available: R + 560 ≤ L for version 3,
   R + 7240 ≤ L for versions 1 and 2.
4. The checksum is correct (section 5).
5. `secsize` is 512, 1024, 2048 or 4096 (DECISION; 1024 and 2048
   CONFIRMED, 512 REFERENCE-ONLY, 4096 allowed for safety margin).
6. `front` ≥ 0.
7. `label_blkno` ≥ 0 and is consistent with R under one of the device
   sector sizes of section 7.2 (that section defines the test and yields
   the value A).
8. The uninitialised flag (bit 0x80000000 of `flags`) is clear.
9. At least one partition entry is in use and, after section 7, starts
   inside the data: its first byte offset is less than L.

DECISION: the data **is a NeXT disk label** exactly when at least one
candidate offset holds a valid label. The first valid one in the order
of section 2.3 is used; the others are not compared with it.

Why this is strong enough to be probed before other formats: a 32-bit
magic at one of seven fixed offsets, a 16-bit checksum over at least 558
bytes, a consistent self-position and a sane partition are required
together. Note that byte 0 may legitimately hold a PC boot sector with
the `55 aa` signature (Intel floppies) or another foreign header
(OS42-CD); the label still takes precedence because the PC partition
table of such a floppy is empty. The converse is applied too: when the
boot sector has the `55 aa` signature and a partition table entry in
use, a label at any candidate offset other than 0 is taken as a leftover
and ignored, so that the PC partitions are listed.

## 7. Computing a partition's byte range

### 7.1 Units

| Quantity | Unit |
|:--|:--|
| `label_blkno` | device sectors (dss bytes), absolute on the whole medium |
| `front`, `back`, partition `base` and `size`, `boot_blkno` | label sectors (S bytes) |
| file system addresses inside a partition | fragments of the file system, relative to the partition's first byte ([`ufs.md`](ufs.md)) |

- CONFIRMED: on the floppies the device sector is 512 bytes
  (`label_blkno` 15 for the copy at byte 7680) while S is 1024 (the
  file system starts at byte 98,304 = 96 × 1024 or 65,536 = 64 × 1024).
- CONFIRMED: on NS32-CD the labels sit at 512-byte-sector positions
  while S is 2048 (the file system starts at byte 163,840 = 80 × 2048).

### 7.2 The absolute position of the data (A)

The label gives positions that are absolute on the whole medium. When
the data handed to the reader is itself a partition of an outer table
(section 8), the reader needs A, the absolute byte position of the
data's first byte. It is derived from the label copy itself:

| R | Device sector size assumed | A |
|:--|:--|:--|
| 0 | 512 (DECISION; irrelevant when `label_blkno` is 0) | `label_blkno` × 512 |
| 7680, 15360 or 23040 | 512 | `label_blkno` × 512 − R |
| 8192, 16384 or 24576 | 2048 if `label_blkno` × 2048 = R; otherwise 1024 if `label_blkno` × 1024 = R; otherwise the copy is not valid | 0 |

- A copy whose A would be negative is not valid.
- On every image examined A is 0.
- DECISION: for the 1024- and 2048-byte positions only A = 0 is
  accepted, because no medium with such sectors carries an outer
  partition table.

### 7.3 The range

For a partition entry in use, with all arithmetic in at least 64 bits:

- absolute first byte = (`base` + `front`) × S;
- first byte within the data = absolute first byte − A;
- length in bytes = `size` × S.

Then (DECISION):

- If the first byte within the data is negative or not less than L, the
  partition is skipped.
- If first byte + length exceeds L, the length is reduced so that the
  range ends at L. Truncated images are common (OS42-CD is one) and the
  file system reader copes with a short partition.
- `back` is not subtracted from anything: the partition's own `size`
  already excludes it (REFERENCE-ONLY).
- Overlapping partitions are not an error in the format. This
  repository's reader nevertheless yields a range only once: a partition
  that shares bytes with one already yielded is left out and counted in a
  diagnostic, and so is a partition that would contain the label copy
  describing it (it would be found inside itself again). Without this a
  crafted label multiplies its data at every level of nesting.

CONFIRMED for `base` = 0: on all ten images the UFS superblock magic is
found at (first byte within the data) + 8192 + 1372, and on the nine
untruncated images `size` × S equals the file system's own size
(fragment count × fragment size from its superblock).

## 8. The label inside a PC partition (Intel hard disks)

Everything in this section is **REFERENCE-ONLY**: no Intel hard disk
image was examined (section 15). It is the common reading of three
independent pieces of Apple code (kernel driver, boot loader, `disk`
utility) and of Mac OS X 10.0's later driver.

- An Intel hard disk starts with a standard master boot record. The
  NEXTSTEP, OPENSTEP or Rhapsody area is a primary partition whose type
  byte is **0xA7**. Let *relsect* be its starting sector (512-byte
  sectors) from the partition table.
- Device sector 0 of that partition holds boot code, so label copy 0 is
  not written. Copies 1 to 3 are at device sectors *relsect* + 15,
  *relsect* + 30 and *relsect* + 45 of the disk, that is at bytes 7680,
  15360 and 23040 **of the partition**.
- `label_blkno` of each copy is its **absolute** sector on the disk
  (*relsect* + 15, and so on), not its sector within the partition.
- Partition `base` values are **absolute** too: the `disk` utility sets
  the `base` of partition `a` to *relsect*, and the kernel computes a
  partition's position as (`base` + `front`) × S from the start of the
  disk without adding the PC partition's start again. `front` itself is
  the plain 160 KiB (320 sectors of 512 bytes).
- The file system of partition `a` therefore starts 160 KiB after the
  first byte of the 0xA7 partition.

How the rules of section 7 handle this when the reader is handed the
bytes of the 0xA7 partition (which is what an MBR reader yields): the
copy at R = 7680 has `label_blkno` = *relsect* + 15, so
A = (*relsect* + 15) × 512 − 7680 = *relsect* × 512. Partition `a` has
absolute first byte (*relsect* + 320) × 512 and therefore first byte
within the data 320 × 512 = 163,840. Worked numbers are in section 10.4.

When the reader is handed the whole disk image instead, no label is
found at the seven candidate offsets (the 0xA7 partition does not start
at sector 0), so the image is treated as a PC-partitioned disk by the
MBR reader, which yields the 0xA7 partition; the label is then found
inside it as described.

REFERENCE-ONLY: Rhapsody on PowerPC puts the same label at the start of
an Apple Partition Map partition, with `label_blkno` and `base` absolute
in the same way. The rule for R = 0 in section 7.2 covers it.

## 9. What the reader yields

- One entry per partition in use that survives section 7.3, in table
  order. The entry's contents are the bytes of the range; the entry is
  an ordinary file as far as the caller is concerned, and content
  detection then recognises the file system inside.
- DECISION: entries are named after the convention the repository
  already uses for PC partition tables, with the partition's index 0 to
  7 (`a` is 0). The label text (`label`, section 3.2) is the natural
  human-readable name of a single-partition medium.
- The front porch, the boot blocks and the label copies are not yielded.
- Nothing in the label is executed, and no field other than `version`,
  `label_blkno`, `flags`, `secsize`, `front`, the checksum and the
  partition `base` and `size` influences the result.

## 10. Worked examples

### 10.1 A version 3 label at byte 0 (NS32-FD-68K)

Label bytes 0 to 235 (the header and partition entry `a`):

```
     0  64 6c 56 33 00 00 00 00 00 00 00 00 49 6e 73 74
    16  61 6c 6c 5f 33 2e 32 00 00 00 00 00 00 00 00 00
    32  00 00 00 00 00 00 00 00 5b 84 ff b2 53 6f 6e 79
    48  20 4d 50 58 2d 31 31 31 4e 20 32 38 38 30 2d 35
    64  31 32 00 00 72 65 6d 6f 76 61 62 6c 65 5f 72 77
    80  5f 66 6c 6f 70 70 79 00 00 00 00 00 00 00 04 00
    96  00 00 00 02 00 00 00 09 00 00 00 50 00 00 01 2c
   112  00 60 00 00 00 00 00 00 00 00 00 00 00 00 00 20
   128  ff ff ff ff 66 64 6d 61 63 68 00 00 00 00 00 00
   144  00 00 00 00 00 00 00 00 00 00 00 00 6c 6f 63 61
   160  6c 68 6f 73 74 00 00 00 00 00 00 00 00 00 00 00
   176  00 00 00 00 00 00 00 00 00 00 00 00 61 62 00 00
   192  00 00 00 00 05 40 20 00 04 00 74 00 00 20 08 00
   208  00 01 00 00 00 00 00 00 00 00 00 00 00 00 00 00
   224  00 00 01 34 2e 33 42 53 44 00 00 00
```

| Offset | Bytes | Field | Value |
|--:|:--|:--|:--|
| 0 | `64 6c 56 33` | `version` | `dlV3`, version 3 |
| 4 | `00 00 00 00` | `label_blkno` | 0 |
| 8 | `00 00 00 00` | `size` | 0 |
| 12 | `49 6e 73 74 61 6c 6c 5f 33 2e 32 00 …` | `label` | `Install_3.2` |
| 36 | `00 00 00 00` | `flags` | 0 |
| 40 | `5b 84 ff b2` | `tag` | 0x5B84FFB2 |
| 44 | `53 6f 6e 79 …` | `name` | `Sony MPX-111N 2880-512` |
| 68 | `72 65 6d 6f …` | `type` | `removable_rw_floppy` |
| 92 | `00 00 04 00` | `secsize` | 1024 |
| 96 | `00 00 00 02` | `ntracks` | 2 |
| 100 | `00 00 00 09` | `nsectors` | 9 |
| 104 | `00 00 00 50` | `ncylinders` | 80 |
| 108 | `00 00 01 2c` | `rpm` | 300 |
| 112 | `00 60` | `front` | 96 |
| 114 | `00 00` | `back` | 0 |
| 116 | 8 zero bytes | alternate groups | 0 |
| 124 | `00 00 00 20`, `ff ff ff ff` | `boot_blkno` | 32 and none |
| 132 | `66 64 6d 61 63 68 00 …` | `kernel` | `fdmach` |
| 156 | `6c 6f 63 61 6c 68 6f 73 74 00 …` | `hostname` | `localhost` |
| 188 | `61`, `62` | root, read-write partition | `a`, `b` |

Partition entry `a` (label offsets 190 to 235):

| Label offset | Bytes | Field | Value |
|--:|:--|:--|:--|
| 190 | `00 00 00 00` | `base` | 0 |
| 194 | `00 00 05 40` | `size` | 1344 |
| 198 | `20 00` | `bsize` | 8192 |
| 200 | `04 00` | `fsize` | 1024 |
| 202 | `74` | `opt` | `t` |
| 204 | `00 20` | `cpg` | 32 |
| 206 | `08 00` | `density` | 2048 |
| 208 | `00` | `minfree` | 0 |
| 209 | `01` | `newfs` | 1 |
| 210 | 16 zero bytes | `mountpt` | empty |
| 226 | `01` | `automnt` | 1 |
| 227 | `34 2e 33 42 53 44 00 00` | `type` | `4.3BSD` |

Entries `b` to `h` (label offsets 236 to 557) are seven copies of the
unused pattern of section 4.2. Bytes 558 to 559 are `05 bd`; bytes 560
to 7239 are zero.

Checksum: the sum of the 279 big-endian words of bytes 0 to 557 is
0x51056C (`label_blkno` is already zero here). Upper half 0x51 plus
lower half 0x056C is 0x05BD, which is not greater than 65,535 and equals
the stored `05 bd`.

Partition `a`: A = 0; first byte (0 + 96) × 1024 = 98,304; length
1344 × 1024 = 1,376,256; the range ends at byte 1,474,560, exactly the
size of the floppy image. The UFS superblock magic `00 01 19 54` is at
byte 98,304 + 8192 + 1372 = 107,868.

The copies at bytes 7680, 15360 and 23040 are identical except for byte
7, which is `0f`, `1e` and `2d` (`label_blkno` 15, 30, 45).

### 10.2 A label behind a PC boot sector (NS32-FD-X86a)

- Bytes 0 to 7 of the image are `b8 00 00 fa 8e d0 bc f0` (boot code);
  bytes 446 to 509 are zero; bytes 510 to 511 are `55 aa`.
- Offset 0 holds no version value. Offset 7680 holds
  `64 6c 56 33 00 00 00 0f`: version 3, `label_blkno` 15.
- With bytes 7684 to 7687 taken as zero, the 279 words of bytes 7680 to
  8237 sum to 0x5F7555; 0x5F + 0x7555 = 0x75B4, equal to the stored
  checksum `75 b4` at bytes 8238 to 8239. (Summing the bytes as they are
  on disk would give 0x75C3 and fail.)
- R = 7680, so dss = 512 and A = 15 × 512 − 7680 = 0.
- `secsize` 1024, `front` 64, partition `a` `base` 0, `size` 1376, type
  `4.3BSD`: first byte 65,536, length 1,409,024, end 1,474,560.

### 10.3 CD-ROMs

- NS32-CD: valid label at offset 0, `label_blkno` 0, checksum `eb e0`,
  `secsize` 2048, `front` 80, partition `a` `base` 0, `size` 153,600,
  `bsize` 8192, `fsize` 2048, type `4.3BSD`. First byte 163,840, length
  314,572,800, end 314,736,640 (the image file is 315,047,936 bytes).
  Further copies at 7680, 15360, 23040 with `label_blkno` 15, 30, 45.
- OS42-CD: offset 0 holds `80 00 00 00` and no label; offset 7680 holds
  no label. The first valid copy is at offset 8192: `label_blkno` 4,
  4 × 2048 = 8192, so dss = 2048 and A = 0; stored checksum `fa 11`
  (the words as stored on disk sum to 0xFA15, four more, because of the
  4 in `label_blkno`). `secsize` 2048, `front` 80, partition `a` `base`
  0, `size` 249,856: first byte 163,840, length 511,705,088. The
  specimen is only 8,388,608 bytes long, so the yielded range is clamped
  to 163,840 … 8,388,607.

### 10.4 An Intel hard disk (constructed numbers, REFERENCE-ONLY)

A disk whose master boot record has a partition of type 0xA7 starting at
sector 63, initialised with S = 512:

- label copies at absolute bytes (63 + 15) × 512 = 39,936,
  (63 + 30) × 512 = 47,616 and (63 + 45) × 512 = 55,296, with
  `label_blkno` 78, 93 and 108;
- `front` 320; partition `a` `base` 63;
- handed the partition (which starts at absolute byte 32,256): the copy
  at R = 7680 gives A = 78 × 512 − 7680 = 32,256; partition `a` has
  absolute first byte (63 + 320) × 512 = 196,096 and first byte within
  the data 196,096 − 32,256 = 163,840.

## 11. Test vectors

Expected results for the specimens. They are kept outside the repository
(see [`README.md`](README.md) for their identification).

| Specimen | First valid R | `label_blkno` | Stored checksum | S | `front` | `a`: `base`, `size` | Range of `a` within the image | Type |
|:--|--:|--:|:--|--:|--:|:--|:--|:--|
| NS32-FD-68K | 0 | 0 | `05 bd` | 1024 | 96 | 0, 1344 | 98,304 … 1,474,559 | `4.3BSD` |
| NS32-FD-X86a | 7680 | 15 | `75 b4` | 1024 | 64 | 0, 1376 | 65,536 … 1,474,559 | `4.3BSD` |
| NS32-FD-X86b | 7680 | 15 | `b1 cd` | 1024 | 96 | 0, 1344 | 98,304 … 1,474,559 | `4.3BSD` |
| OS42-FD-68K | 0 | 0 | `79 9b` | 1024 | 96 | 0, 1344 | 98,304 … 1,474,559 | `4.3BSD` |
| OS42-FD-X86a (install) | 7680 | 15 | `b7 6b` | 1024 | 64 | 0, 1376 | 65,536 … 1,474,559 | `4.3BSD` |
| OS42-FD-X86b (drivers) | 7680 | 15 | `2f 7f` | 1024 | 64 | 0, 1376 | 65,536 … 1,474,559 | `4.3BSD` |
| RH-FD-X86a (boot) | 7680 | 15 | `ea ce` | 1024 | 96 | 0, 1344 | 98,304 … 1,474,559 | `4.4BSD` |
| RH-FD-X86b (drivers) | 7680 | 15 | `02 f8` | 1024 | 96 | 0, 1344 | 98,304 … 1,474,559 | `4.4BSD` |
| NS32-CD | 0 | 0 | `eb e0` | 2048 | 80 | 0, 153,600 | 163,840 … 314,736,639 | `4.3BSD` |
| OS42-CD (first 8 MiB) | 8192 | 4 | `fa 11` | 2048 | 80 | 0, 249,856 | 163,840 … 8,388,607 (clamped) | `4.3BSD` |

Every specimen has exactly one partition in use.

Negative vectors a fixture builder can derive from any valid label
(each must make the data "not a NeXT disk label" unless another copy is
still valid, so all copies must be damaged alike):

1. Flip one bit in `label` (byte 12): checksum fails.
2. Change `label_blkno` of the copy at R = 7680 to 14 and leave the
   checksum alone: the checksum still passes (the field is excluded) but
   A would be negative, so the copy is not valid.
3. Set `secsize` to 0, or to 3000, and recompute the checksum: check 5
   of section 6 fails.
4. Set the `size` of every partition to 0 and recompute the checksum: no
   partition in use.
5. Set `base` of partition `a` so that its first byte is at or beyond L
   and recompute the checksum: check 9 fails.
6. Truncate the data to R + 559 bytes: check 3 fails.
7. A file that merely contains the text `dlV3` at offset 0 followed by
   arbitrary bytes: the checksum fails.

## 12. Observed implementation gaps

Nothing in this section is to be imitated.

- Mac OS X 10.0's partition-scheme driver looks only at copy 0. It
  cannot read Intel media (no copy 0) or the OPENSTEP 4.2 CD.
- NetBSD/next68k looks only at the first sector, does not zero
  `label_blkno` before summing (so it accepts only copy 0), ignores
  partition `h`, and does not check that a partition lies on the disk.
- Darwin 0.3's boot loader does not verify the checksum at all.
- None of the sources bounds `secsize`, `front` or the partition range
  against the medium.

## 13. Decisions

| # | Decision | Section |
|--:|:--|:--|
| D1 | Labels are looked for at seven fixed offsets only, in ascending order; the first valid one wins. | 2.3, 6 |
| D2 | The checksum algorithm of section 5.1 is the only one accepted. | 5.2 |
| D3 | `secsize` must be 512, 1024, 2048 or 4096. | 6 |
| D4 | A label with the uninitialised flag set is not valid. | 6 |
| D5 | At least one partition in use that starts inside the data is required for recognition. | 6 |
| D6 | A is derived from `label_blkno` and R as in section 7.2; R = 0 assumes 512-byte device sectors; the 1024/2048 positions require A = 0. | 7.2 |
| D7 | Partitions starting outside the data are skipped; partitions ending outside it are clamped to L. | 7.3 |
| D8 | Every partition in use is yielded regardless of its type string; the label's advisory block and fragment sizes are ignored. | 4.3 |
| D9 | The bad-block table of versions 1 and 2 is ignored. | 3.1 |
| D10 | Entry naming follows the repository's convention for PC partition tables. | 9 |

## 14. Safety requirements

1. Every read is bounds-checked against L before it happens. The
   candidate offsets are fixed, so at most 7 × 7240 bytes are ever
   inspected to recognise a label.
2. All fields are untrusted. `front` is a signed 16-bit and `base` and
   `size` are signed 32-bit values; negative values never reach an
   offset computation (`front` < 0 invalidates the label, `base` < 0 or
   `size` ≤ 0 makes the entry unused).
3. Offsets and lengths are computed in at least 64 bits:
   (`base` + `front`) × S can reach about 2^43 and `label_blkno` × 2048
   about 2^42.
4. A negative or out-of-range partition start skips that partition; an
   over-long partition is clamped. Neither aborts the others.
5. The reader allocates nothing whose size comes from the label other
   than the yielded ranges, each of which is at most L bytes; if the
   implementation copies partition bytes, the total copied is at most
   8 × L and is subject to the repository's general limits.
6. Text fields are not assumed to be terminated; at most the field's
   size is read. Bytes outside printable ASCII in `label` must not reach
   a path unescaped; a path separator or NUL in it must not create
   directories.
7. Nesting: the yielded partition goes through content detection again.
   A partition that itself contains a label (possible only with crafted
   data) is bounded by the caller's depth limit, which the reader must
   honour and decrement like every other container.
8. The reader never follows `boot_blkno`, never interprets boot code and
   never trusts `size` (medium size), the geometry or the advisory file
   system parameters.

## 15. Open points

1. **Intel hard disks** (section 8) are specified from sources only.
   Thirteen preinstalled virtual-machine disks found online (NEXTSTEP
   3.3, OPENSTEP 4.2, Rhapsody DR1 and DR2) were examined and none has
   a PC partition table entry: each uses the whole disk, with `55 aa`
   at byte 510, an empty table and labels at bytes 7680, 15360 and
   23040. A disk with a type 0xA7 partition has not been found. A
   real NEXTSTEP 3.3, OPENSTEP 4.x or Rhapsody hard disk image for Intel
   should be used to confirm: copies at partition bytes 7680, 15360 and
   23040; `label_blkno` absolute; `base` of `a` equal to the PC
   partition's starting sector; the UFS superblock at partition byte
   163,840 + 8192. If `base` turned out to be relative to the PC
   partition, section 7.3 would compute a negative or wrong start and
   needs revisiting. The sources only show this arrangement with
   S = 512, where `base` (in units of S) and the PC partition's starting
   sector (in 512-byte sectors) are the same unit. An Intel hard disk
   whose label has S = 1024 would show whether `base` is then scaled;
   with the rules as written, an unscaled `base` would place partition
   `a` at partition byte `front` × S + *relsect* × (S − 512) instead of
   `front` × S.
2. **Version 2** (`dlV2`) is CONFIRMED since this document was written:
   the NeXTSTEP 0.8 hard disk image for the Previous emulator
   (archive.org item `nextstep-0.8-hd-image-with-previous.-7z`) carries
   `dlV2` labels at bytes 0, 7680, 15360 and 23040, and a reader written
   from this document accepts their checksum (position 7238 to 7239,
   range 0 to 7237) and reads the volume. **Version 1** (`NeXT`) was
   not seen and probably exists on pre-release media only: the earliest
   released system, 0.8, already writes `dlV2`.
3. **Magneto-optical disks**: whether all four copies sit at multiples
   of 8192 bytes is unknown; copy 0 at byte 0 is expected.
4. **Hard disks of NeXT machines with 512-byte sectors and S = 1024**
   (the usual case on NeXT hardware) were not seen; the floppies have
   the same combination and confirm the unit rules.
5. **Several partitions in use**: no specimen has more than one.
6. **SPARC and PA-RISC hard disks** of OPENSTEP 4.x were not seen. The
   OPENSTEP 4.2 CD shows a foreign header at byte 0 with the label at
   byte 8192; hard disks of those machines presumably also keep the
   label at one of the candidate offsets.
