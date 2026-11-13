# UFS1 (Berkeley Fast File System), read-only

Format specification for clean-room implementation. This document
describes everything a reader needs to list and extract the files of a
UFS1 volume as written by NEXTSTEP, OPENSTEP, Rhapsody and early
Mac OS X, and by 4.2BSD, 4.3BSD, 4.4BSD, SunOS, NetBSD and FreeBSD, in
either byte order:

- where a volume is found, the superblock, byte order and the checks
  that make recognition safe (sections 2 and 3);
- fragment and block addressing, cylinder groups and the arithmetic
  that locates an inode (sections 4 and 5);
- the on-disk inode in its two generations, file types and the block
  map with direct and indirect blocks, holes and the last-block rule
  (sections 6 and 7);
- directories in their two generations (section 8), symbolic links,
  hard links and special files (section 9);
- differences between writers, including what the Linux kernel calls
  the `nextstep`, `nextstep-cd` and `openstep` variants (section 10);
- UFS2 and other look-alikes that must be rejected (section 11);
- worked examples, test vectors, decisions and safety requirements
  (sections 12 to 16).

The partition table that usually surrounds a NeXT volume is specified in
[`disklabel.md`](disklabel.md).

The document contains no program code and no pseudo-code.

## 1. How to read this document

### 1.1 Provenance

- The structures are described from the published design of the file
  system (McKusick, Joy, Leffler and Fabry, "A Fast File System for
  UNIX", and the 4.4BSD design book), from the on-disk definitions of
  NetBSD, of Apple's Darwin 0.3 and xnu-123.5, and of the Linux kernel's
  UFS driver. All sources were read for facts only; the index of this
  directory ([`README.md`](README.md)) lists the exact files.
- Every structural statement that could be checked was then checked on
  real and purpose-built volumes with a throwaway reader written by the
  specification's author from the rules below (kept outside the
  repository; not part of the specification).
- Volumes written by NeXT and Apple systems that were walked completely
  (names as in [`disklabel.md`](disklabel.md) section 1.1):
  - **NS32-CD**: NEXTSTEP 3.2 CD-ROM. Big-endian, block 8192, fragment
    2048, 38 cylinder groups, 75,392 inode slots of which 15,548 are
    allocated; 15,615 names.
  - **NS32-FD-68K**, **NS32-FD-X86a**, **NS32-FD-X86b**,
    **OS42-FD-68K**, **OS42-FD-X86a**, **OS42-FD-X86b**: NEXTSTEP 3.2
    and OPENSTEP 4.2 floppies for NeXT and Intel machines. All
    big-endian, block 8192, fragment 1024, 1 or 3 cylinder groups.
  - **RH-FD-X86a**, **RH-FD-X86b**: Rhapsody DR2 floppies for Intel.
    **Little-endian**, 4.4BSD generation, block 8192, fragment 1024.
- **OS42-CD** (first 8 MiB of the OPENSTEP 4.2 CD): superblock, root
  inode and root directory only.
- Volumes built for this specification with NetBSD's `makefs`
  (**crafted volumes**, section 13): 4.4BSD generation, both byte
  orders, block/fragment sizes 4096/512, 8192/1024 and 16384/2048; and
  two UFS2 volumes for section 11.
- Not examined: any hard disk; any volume written by 4.2BSD, 4.3BSD,
  SunOS, Solaris or Mac OS X; any little-endian volume of the old
  generation; any volume with holes; any triple-indirect file
  (section 17).

### 1.2 Evidence labels

- **CONFIRMED (NeXT)**: observed on the NeXT and Apple volumes above.
  Where a statement is a "for every" statement, it held for every
  allocated inode and every directory of every one of them.
- **CONFIRMED (crafted)**: observed on the crafted volumes. For file
  contents the evidence is that the bytes read by following this
  document have the SHA-256 of the file that was copied in.
- **REFERENCE-ONLY**: stated by, or deduced from, the sources; not
  observed.
- **INFERRED**: deduced, unconfirmed.
- **DECISION**: chosen by this specification (listed in section 15).

A statement without a label in sections 3 to 9 is CONFIRMED (NeXT) and
CONFIRMED (crafted).

### 1.3 Terms and conventions

| Term | Meaning |
|:--|:--|
| volume | One file system: the bytes of one partition. All offsets are relative to the first byte of the volume unless stated otherwise. |
| **L** | The number of bytes of the volume that are actually available. Untrusted and possibly smaller than the volume claims. |
| volume byte order | The byte order of every multi-byte integer of the volume: big-endian or little-endian (section 3.2). |
| fragment | The addressing unit. **F** is the fragment size in bytes. Fragment *n* occupies volume bytes *n* × F to (*n* + 1) × F − 1. |
| block | The allocation unit: 1, 2, 4 or 8 consecutive fragments. **B** is the block size in bytes. |
| address | A signed 32-bit **fragment number** counted from the start of the volume. Every pointer in the file system (to a data block, an indirect block, an inode table) is an address, including pointers to whole blocks. |
| cylinder group | One of `ncg` equal slices of the volume, each with its own inode table. |
| inode number | Index of an inode, counted through all cylinder groups. |
| old generation | The inode and directory formats of 4.2BSD and 4.3BSD. Used by NEXTSTEP and OPENSTEP. |
| new generation | The formats of 4.4BSD. Used by Rhapsody, Mac OS X, NetBSD, FreeBSD. |

"s32"/"u32"/"s16"/"u16"/"u64" are signed or unsigned integers of that
many bits in the volume byte order. Numbers are decimal unless prefixed
`0x`; mode values are also given in octal, prefixed `0o`. Byte sequences
in code style are hexadecimal, in disk order.

## 2. Where a volume is found

A reader is handed a byte sequence that is supposed to be one volume. It
does not search for volumes.

| Carrier | What the reader is handed |
|:--|:--|
| Partition of a NeXT disk label (floppies, CDs and disks of NEXTSTEP, OPENSTEP, Rhapsody) | The partition's byte range ([`disklabel.md`](disklabel.md) section 7) |
| Bare partition dump, or a disk whose first file system starts at byte 0 (SunOS disks, many BSD installation images) | The whole file |
| Partition of a PC partition table of a BSD type (0xA5 FreeBSD, 0xA6 OpenBSD, 0xA9 NetBSD, 0xA8 Darwin UFS) whose first BSD partition starts at the partition's first sector | The PC partition. REFERENCE-ONLY |

- A volume never starts with its superblock; the first 8192 bytes are
  boot area. The reader recognises a volume by its superblock at byte
  8192 only (section 3.1).
- BSD and Sun disk labels are not interpreted. A volume that does not
  start at the first byte of the data handed in is not found
  (DECISION).

## 3. The superblock

### 3.1 Position and detection

- The superblock of a UFS1 volume is at volume **byte 8192**, whatever
  the fragment size, block size or device sector size. CONFIRMED (NeXT)
  for fragment sizes 1024 and 2048; CONFIRMED (crafted) for 512, 1024
  and 2048.
- Its defined part is 1376 bytes long; the reader needs L ≥ 9568.
- Backup copies exist in every cylinder group (at address
  S(*c*) + `sblkno`, section 5.2). DECISION: the reader does not use
  them, and never scans for superblocks. (RH-FD-X86b contains two stale
  superblocks of an earlier file system in its data area; a scanning
  reader would be misled.)

### 3.2 Magic and byte order

The 32-bit magic number 0x00011954 is at superblock offset 1372:

| Bytes at volume offset 9564 | Volume byte order |
|:--|:--|
| `00 01 19 54` | big-endian |
| `54 19 01 00` | little-endian |
| anything else | not a UFS1 volume (see section 11 for look-alikes) |

- The byte order belongs to the volume, not to the machine type:
  NEXTSTEP 3.2 and OPENSTEP 4.2 media for Intel are **big-endian**
  (CONFIRMED on five Intel floppies), Rhapsody DR2 media for Intel are
  **little-endian** (CONFIRMED on two), and Mac OS X writes big-endian
  volumes on every processor (REFERENCE-ONLY).
- Every multi-byte field of every structure of the volume (superblock,
  inodes, indirect blocks, directory entries) uses the volume byte
  order. Single bytes and text are unaffected.

### 3.3 Fields

Offsets are from the first byte of the superblock (volume byte 8192).
Only fields a reader uses or checks are listed; everything else is
allocation state or disk geometry.

| Offset | Type | Name | Meaning |
|--:|:--|:--|:--|
| 8 | s32 | `sblkno` | Address of the backup superblock, relative to a group's start. |
| 12 | s32 | `cblkno` | Address of the group header, relative to a group's start. |
| 16 | s32 | `iblkno` | Address of the inode table, relative to a group's start. |
| 20 | s32 | `dblkno` | Address of the first data block, relative to a group's start. |
| 24 | s32 | `cgoffset` | Stagger of the group contents, in fragments (section 5.2). |
| 28 | u32 | `cgmask` | Mask used with `cgoffset` (section 5.2). |
| 36 | s32 | `size` | Number of fragments in the volume. |
| 44 | s32 | `ncg` | Number of cylinder groups. |
| 48 | s32 | `bsize` | **B**, block size in bytes. |
| 52 | s32 | `fsize` | **F**, fragment size in bytes. |
| 56 | s32 | `frag` | Fragments per block, B ÷ F. |
| 72 | s32 | `bmask` | −B as a 32-bit value. |
| 76 | s32 | `fmask` | −F as a 32-bit value. |
| 80 | s32 | `bshift` | log2 of B. |
| 84 | s32 | `fshift` | log2 of F. |
| 96 | s32 | `fragshift` | log2 of `frag`. |
| 100 | s32 | `fsbtodb` | Shift from fragments to device sectors. **Not used** (section 4.2). |
| 104 | s32 | `sbsize` | Size of the superblock in bytes. |
| 116 | s32 | `nindir` | Addresses per indirect block, B ÷ 4. |
| 120 | s32 | `inopb` | Inodes per block, B ÷ 128. |
| 184 | s32 | `ipg` | Inodes per cylinder group. |
| 188 | s32 | `fpg` | Fragments per cylinder group. |
| 212 | 512 bytes | `fsmnt` | Path the volume was last mounted on, zero-terminated text (`/flop`, `/temp-disk`, `/RhapsodyInstall`; empty on crafted volumes). Informational. |
| 1320 | s32 | `maxsymlinklen` | New generation only: an inline symbolic link is shorter than this (60). |
| 1324 | s32 | `inodefmt` | 2 on the new generation (section 3.5). |
| 1356 | s32 | `postblformat` | −1 on a superblock of the 4.2BSD layout (section 3.5). |
| 1372 | u32 | `magic` | 0x00011954. |

Other fields, for orientation only: offsets 0 and 4 are two unused
words; 32 is the time of last write; 40 the number of data fragments;
60 to 68 and 88 to 92 tuning parameters; 108, 112 two obsolete masks;
124 to 148 geometry; 152 to 160 the summary area; 164 to 180 geometry
(`ntrak`, `nsect`, `spc`, `ncyl`, `cpg`); 192 to 207 free-space totals;
208 to 211 four status bytes (modified, clean, read-only, flags); 724 to
859 allocator state; 860 to 1371 the rotational layout tables or, on the
new generation, further fields.

### 3.4 Validity checks

A volume is recognised as UFS1 only when **all** of these hold. They are
also the checks to repeat when opening.

1. L ≥ 9568 and the magic is present (section 3.2).
2. B is a power of two from 4096 to 65,536. F is a power of two from 512
   to B. B ÷ F is 1, 2, 4 or 8 and equals `frag`.
3. `bshift`, `fshift` and `fragshift` are the base-2 logarithms of B, F
   and `frag`; `bmask` = −B and `fmask` = −F (as 32-bit two's complement
   values).
4. `nindir` = B ÷ 4 and `inopb` = B ÷ 128.
5. 1376 ≤ `sbsize` ≤ 8192.
6. `ncg` ≥ 1, `fpg` ≥ 1, `size` ≥ 1, and
   (`ncg` − 1) × `fpg` < `size` ≤ `ncg` × `fpg` (computed in 64 bits).
7. `ipg` ≥ `inopb`, `ipg` is a multiple of `inopb`, and `ncg` × `ipg`
   fits in an unsigned 32-bit number.
8. 0 < `sblkno` < `cblkno` < `iblkno` < `dblkno`, and
   `dblkno` = `iblkno` + (`ipg` ÷ `inopb`) × `frag`, and `dblkno` ≤
   `fpg`.
9. `cgoffset` ≥ 0.
10. The root directory is plausible: inode 2 can be located and read
    inside L (section 5), its type is "directory", its size is a
    non-zero multiple of 512, its first direct address is valid
    (section 7.4), and the first 12 bytes at that address are a
    directory entry for inode 2 with record length 12 and the one-byte
    name `.` (section 8).

- Checks 2 to 8 held on every volume examined. Observed values:

| Volume | B | F | `sblkno` | `cblkno` | `iblkno` | `dblkno` | `cgoffset` | `cgmask` | `ncg` | `ipg` | `fpg` | `size` | `sbsize` |
|:--|--:|--:|--:|--:|--:|--:|--:|:--|--:|--:|--:|--:|--:|
| NS32-FD-68K, OS42-FD-68K | 8192 | 1024 | 16 | 24 | 32 | 40 | 16 | 0xFFFFFFFE | 3 | 64 | 576 | 1344 | 2048 |
| NS32-FD-X86b | 8192 | 1024 | 16 | 24 | 32 | 48 | 16 | 0xFFFFFFFE | 3 | 128 | 576 | 1344 | 2048 |
| NS32-FD-X86a, OS42-FD-X86a, OS42-FD-X86b | 8192 | 1024 | 16 | 24 | 32 | 56 | 16 | 0xFFFFFF80 | 1 | 192 | 23,040 | 1376 | 2048 |
| RH-FD-X86a, RH-FD-X86b | 8192 | 1024 | 16 | 24 | 32 | 80 | 16 | 0xFFFFFFFE | 1 | 384 | 2304 | 1344 | 2048 |
| NS32-CD | 8192 | 2048 | 8 | 12 | 16 | 140 | 64 | 0xFFFFFFE0 | 38 | 1984 | 4096 | 153,600 | 2048 |
| OS42-CD | 8192 | 2048 | 8 | 12 | 16 | 140 | 64 | 0xFFFFFFE0 | 61 | 1984 | 4096 | 249,856 | 2048 |
| crafted 4096/512 | 4096 | 512 | 32 | 48 | 56 | 144 | 0 | 0xFFFFFFFF | 1 | 352 | 24,576 | 24,576 | 1536 |
| crafted 8192/1024 | 8192 | 1024 | 16 | 24 | 32 | 80 | 0 | 0xFFFFFFFF | 1 | 384 | 16,384 | 16,384 | 2048 |
| crafted 16384/2048 | 16,384 | 2048 | 8 | 16 | 24 | 48 | 0 | 0xFFFFFFFF | 1 | 384 | 12,288 | 12,288 | 2048 |

- `fpg` may be larger than `size` on a one-group volume (the Intel
  floppies).
- DECISION: `size` × F may exceed L (a truncated image). That alone does
  not make the volume invalid; every later read is bounds-checked
  against L and fails individually.
- DECISION: the geometry fields, the free-space totals, the status bytes
  and the time are not checked. Several are meaningless on modern
  volumes and on NeXT media.
- The range 4096 to 65,536 for B and the lower bound 512 for F are a
  DECISION; the sources allow B from 4096 and F from 512 and cap B at
  8192 (old systems) or 65,536 (new ones).

### 3.5 Which generation

Two questions must be answered from the superblock: which directory
entry format is used (section 8.2) and how inline symbolic links are
recognised (section 9.1). The inode layout itself is read the same way
in both generations as far as a reader is concerned (section 6).

| Condition | Generation | Directory entries | `maxsymlinklen` to use |
|:--|:--|:--|:--|
| `postblformat` = −1 | old; the superblock has the 4.2BSD layout, and the bytes at 1316 to 1371 are part of a table and **must not be interpreted** | old | none |
| `postblformat` ≠ −1 and `inodefmt` ≠ 2 | old (4.3BSD-Tahoe and later, SunOS, Solaris) | old | none |
| `postblformat` ≠ −1 and `inodefmt` = 2 and `maxsymlinklen` > 0 | new | new | the stored value, at most 60 |
| `postblformat` ≠ −1 and `inodefmt` = 2 and `maxsymlinklen` ≤ 0 | new inodes with old directories | old | none |

- CONFIRMED (NeXT): every NEXTSTEP 3.2 and OPENSTEP 4.2 volume has
  `postblformat` = −1. On the floppies all bytes from 1116 to 1371 are
  `ff`; on the CDs the table at 860 has real entries
  (`00 00 00 02 00 04 00 06 …`), which shows that this region is a
  table of 16-bit numbers and not the new generation's fields.
- CONFIRMED (NeXT) on the Rhapsody floppies and CONFIRMED (crafted):
  `postblformat` = 1, `inodefmt` = 2, `maxsymlinklen` = 60.
- Rows 2 and 4 are REFERENCE-ONLY.
- A stored `maxsymlinklen` greater than 60 is treated as 60 (DECISION;
  the inline area is 60 bytes).

## 4. Addresses

### 4.1 Fragments and blocks

- Every pointer is a fragment number. A pointer to a whole block is the
  number of the block's first fragment; such numbers are multiples of
  `frag` only by the allocator's habit, and the reader does not require
  it (DECISION).
- The byte offset of address *a* in the volume is *a* × F. With a signed
  32-bit *a* and F up to 65,536 the product needs 64 bits.
- Address 0 never refers to data: fragment 0 is in the boot area. In a
  block map, 0 means "no block" (section 7.3).

### 4.2 Device sector sizes of 1024 and 2048 bytes

- NeXT machines address their disks in 1024-byte units and CD-ROMs in
  2048-byte units, and their file system code converts fragment numbers
  to device blocks with the `fsbtodb` shift. A reader that works on a
  byte image **never needs the device sector size**: *a* × F is the
  byte offset on every variant.
- CONFIRMED (NeXT): NS32-CD has F = 2048 and `fsbtodb` = 0; the floppies
  have F = 1024 and `fsbtodb` = 0; all files are found at *a* × F.
  CONFIRMED (crafted): volumes with `fsbtodb` 0, 1 and 2 (F = 512, 1024,
  2048 on 512-byte sectors) are read identically.
- The superblock is at byte 8192 in all of them (section 3.1).
- The only field whose unit depends on the device is the inode's block
  count (section 6.1).

## 5. Cylinder groups and inode location

### 5.1 Layout of a group

Group *c* (0 ≤ *c* < `ncg`) nominally covers addresses *c* × `fpg` to
(*c* + 1) × `fpg` − 1 (the last group ends at `size`). Inside it, at
addresses relative to the group's **start address** S(*c*):

| Relative address | Contents |
|:--|:--|
| `sblkno` | backup superblock |
| `cblkno` | group header with free maps (not needed for reading) |
| `iblkno` | inode table: `ipg` inodes of 128 bytes, that is `ipg` ÷ `inopb` blocks |
| `dblkno` | first data block |

Group 0 additionally has the boot area and the primary superblock in
front (bytes 0 to 16,383).

### 5.2 The start address, with the old stagger

S(*c*) = `fpg` × *c* + `cgoffset` × (*c* AND NOT `cgmask`)

where AND and NOT are bitwise on 32-bit values.

- On old volumes the group contents are staggered so that they do not
  all sit on the same disk platter: `cgmask` has its low bits clear, so
  *c* AND NOT `cgmask` is *c* modulo a power of two.
  CONFIRMED (NeXT): NS32-CD has `cgoffset` 64 and `cgmask` 0xFFFFFFE0,
  so the term is 64 × (*c* modulo 32); inodes were read correctly from
  all 38 groups. The 3-group floppies have `cgoffset` 16 and `cgmask`
  0xFFFFFFFE: the contents of group 1 start 16 fragments into the group.
- On modern volumes `cgoffset` is 0 and `cgmask` is 0xFFFFFFFF; the term
  vanishes. CONFIRMED (crafted).
- The formula is applied uniformly; there is no flag that selects it.

### 5.3 Locating inode *n*

1. *n* must satisfy 2 ≤ *n* < `ncg` × `ipg`. (Inode 0 does not exist and
   inode 1 is reserved; neither appears in a healthy directory.)
2. *c* = *n* ÷ `ipg` (integer division); *r* = *n* modulo `ipg`.
3. Address of the block that holds the inode:
   *a* = S(*c*) + `iblkno` + `frag` × (*r* ÷ `inopb`).
4. Byte offset of the inode in the volume:
   *a* × F + (*r* modulo `inopb`) × 128.
5. The 128 bytes must lie inside L, and *a* must be less than `size`.

The root directory is **inode 2**. On the volumes examined inode 3 is a
directory named `lost+found` (it has been removed from the NS32-CD
root).

## 6. The inode

### 6.1 Layout

An inode is 128 bytes.

| Offset | Type | Name | Old generation | New generation |
|--:|:--|:--|:--|:--|
| 0 | u16 | `mode` | type and permissions (section 6.2) | same |
| 2 | s16 | `nlink` | number of names | same |
| 4 | u16 | | owner (16-bit) | unused; owner is at 112 |
| 6 | u16 | | group (16-bit) | unused; group is at 116 |
| 8 | u64 | `size` | length in bytes | same |
| 16 | s32 | | access time, seconds | same |
| 20 | s32 | | unused (0) | nanoseconds |
| 24 | s32 | | modification time, seconds | same |
| 28 | s32 | | unused | nanoseconds |
| 32 | s32 | | inode change time, seconds | same |
| 36 | s32 | | unused | nanoseconds |
| 40 | s32 × 12 | `db` | direct addresses | same |
| 88 | s32 × 3 | `ib` | single, double, triple indirect address | same |
| 100 | u32 | `flags` | on NeXT: bit 0x00000001 marks an inline symbolic link | file flags (immutable, append-only, …) |
| 104 | s32 | `blocks` | sectors held by the file | same |
| 108 | s32 | | generation number | same |
| 112 | u32 | | unused (0) | owner (32-bit) |
| 116 | u32 | | unused (0) | group (32-bit) |
| 120 | 8 bytes | | unused | unused |

- A reader needs `mode`, `size`, `db`, `ib` and, for symbolic links,
  `blocks`. These are at the same offsets in both generations, so the
  inode can be parsed without knowing the generation.
- For symbolic links, and only for them, the 60 bytes at offsets 40 to
  99 may hold the link target instead of addresses (section 9.1). For
  character and block devices the first four bytes of the area (offset
  40) hold the device number, not an address; the rest is zero.
  CONFIRMED (NeXT): `/private/dev/drum` on NS32-CD has `00 00 07 00`
  there.
- `size` is a 64-bit number in the volume byte order in both
  generations. On old big-endian volumes this places the length in
  bytes 12 to 15 with bytes 8 to 11 zero. CONFIRMED (NeXT): the upper
  half is zero in all 15,548 allocated inodes of NS32-CD.
  REFERENCE-ONLY: 4.2BSD and 4.3BSD defined only the lower half.
  DECISION: the reader uses all 64 bits in both generations.
- `blocks` counts in units that depend on the writer: 512-byte sectors
  on BSD (CONFIRMED (crafted): a 4796-byte file holding one 4096-byte
  block and two 512-byte fragments has 10); the label sector size on
  NeXT (CONFIRMED (NeXT): a 1024-byte directory has 1 on a floppy with
  1024-byte fragments, and a directory holding one 2048-byte fragment
  has 1 on NS32-CD). The reader only ever tests it for zero.
- The owner and time fields are listed for completeness; nothing this
  reader reports depends on them.

### 6.2 Mode and file type

`mode` is a 16-bit number. Its upper four bits (mask 0o170000) give the
type; the lower twelve are the permission bits in the usual Unix
meaning (0o4000 set-user-id, 0o2000 set-group-id, 0o1000 sticky, then
three groups of read, write, execute for owner, group, others).

| `mode` AND 0o170000 | Type | Reader's treatment |
|:--|:--|:--|
| 0o010000 | FIFO | not listed (section 9.3) |
| 0o020000 | character device | not listed |
| 0o040000 | directory | walked |
| 0o060000 | block device | not listed |
| 0o100000 | regular file | listed and extracted |
| 0o120000 | symbolic link | not listed (section 9.1) |
| 0o140000 | socket | not listed |
| 0o160000 | whiteout (4.4BSD) | not listed. REFERENCE-ONLY |
| anything else, including 0 | unallocated or corrupt | the directory entry is skipped |

- An unallocated inode has `mode` 0. CONFIRMED (NeXT) and CONFIRMED
  (crafted).
- Observed on NS32-CD: 2301 directories, 12,540 regular files, 272
  symbolic links, 273 character and 162 block devices. Observed on the
  crafted volumes: all of the above except devices, plus one FIFO.

## 7. The block map

### 7.1 Logical blocks

A file or directory of `size` bytes consists of logical blocks
0, 1, 2, …, each B bytes except possibly the last. The number of
logical blocks is `size` ÷ B rounded up. With N = `nindir` = B ÷ 4:

| Logical block index | Found through |
|:--|:--|
| 0 to 11 | `db[0]` to `db[11]` |
| 12 to 12 + N − 1 | the single indirect block `ib[0]` |
| 12 + N to 12 + N + N² − 1 | the double indirect block `ib[1]` |
| 12 + N + N² to 12 + N + N² + N³ − 1 | the triple indirect block `ib[2]` |

- An indirect block is one block of B bytes holding N addresses (s32,
  volume byte order).
- Single indirect: entry *i* is the address of logical block 12 + *i*.
- Double indirect: entry *i* is the address of a single indirect block
  that covers the N logical blocks starting at 12 + N + *i* × N.
- Triple indirect: entry *i* is the address of a double indirect block
  that covers N² logical blocks, in the same manner. REFERENCE-ONLY (no
  volume examined has a file that large).
- Entries beyond the end of the file are zero and are never looked at.
- CONFIRMED (NeXT): 196 single and 3 double indirect blocks on NS32-CD.
  CONFIRMED (crafted): single and double, with N = 1024, 2048 and 4096.

### 7.2 How many bytes a logical block holds

- Every logical block except the last holds B bytes.
- The last logical block holds `size` − (index × B) bytes, between 1 and
  B.
- **The last-block rule**: when the last logical block has an index
  below 12 (the file has no indirect blocks) and holds fewer than B
  bytes, the file system has allocated for it only as many fragments as
  the bytes need (that count × F, rounded up), not a whole block. When
  its index is 12 or more, a whole block is allocated.
  CONFIRMED (crafted): a 4796-byte file with B = 4096, F = 512 has
  `db[0]` covering 8 fragments and `db[1]` covering 2; a 100,000-byte
  file (25 logical blocks) has a full block for index 24.
- Consequence for the reader: it reads exactly the number of bytes the
  logical block holds, starting at address × F, and never assumes that
  B bytes are readable at the address of a last block. The bytes after
  the end of the file inside the last fragment are not part of the
  file.

### 7.3 Holes

- An address of 0 for a logical block inside the file means the block
  was never written: it reads as zero bytes.
- An address of 0 for an indirect block (in `ib`, or inside a double or
  triple indirect block) means that every logical block it would cover
  is a hole.
- REFERENCE-ONLY: no volume examined contains a hole. The rule is
  common to every source.

### 7.4 Validity of an address

A non-zero address *a* that is to be read for *k* bytes is valid when

- 0 < *a* < `size` (so negative values are invalid), and
- *a* × F + *k* ≤ L.

For an indirect block *k* is B. DECISION: the reader does not check
that the address lies in a data area, is block-aligned, or is used only
once.

### 7.5 Reading a file

In prose: for each logical block index from 0 to the last, find its
address through section 7.1, emit zeros for a hole, otherwise check the
address (section 7.4) and copy the block's bytes (section 7.2). The
result has exactly `size` bytes. A file of `size` 0 has no blocks; its
addresses are not looked at.

## 8. Directories

### 8.1 Structure

- A directory is a file (read through section 7) whose content is a
  sequence of **chunks** of equal size D, each filled completely by
  variable-length **records**. No record crosses a chunk boundary, and
  the records of a chunk add up to exactly D.
- D is 512 on BSD volumes (CONFIRMED (crafted)) and **1024 on volumes
  written by NEXTSTEP, OPENSTEP and Rhapsody** (CONFIRMED (NeXT): on
  every NeXT and Rhapsody volume directory sizes are multiples of 1024,
  records cross 512-byte boundaries, and no record crosses a 1024-byte
  boundary).
- **A reader does not need D.** Because records tile every chunk
  exactly, walking the records one after another from offset 0 to the
  directory's `size` visits the same records for any D. DECISION: the
  reader walks this way and does not enforce chunk boundaries.
- A directory's `size` is a non-zero multiple of D (hence of 512).

### 8.2 Record layout

| Offset | Size | Old format | New format |
|--:|--:|:--|:--|
| 0 | 4 | u32 inode number; 0 = unused record | same |
| 4 | 2 | u16 record length | same |
| 6 | 2 | u16 name length | byte 6: file type; byte 7: name length |
| 8 | name length | the name | same |
| 8 + name length | ≥ 1 | a zero byte, then padding to a multiple of 4 | same |

Which format applies is decided by the superblock (section 3.5), for the
whole volume.

**The name length and byte order.** The two formats place the name
length differently, and the difference is visible only on little-endian
volumes:

| Volume | Format | Byte 6 | Byte 7 |
|:--|:--|:--|:--|
| big-endian | old | 0 (high byte of the length) | name length |
| big-endian | new | file type | name length |
| little-endian | old | name length | 0 (high byte of the length) |
| little-endian | new | file type | name length |

- In the new format the two bytes are separate fields and are **not**
  swapped with the byte order: the type is always byte 6 and the length
  always byte 7. CONFIRMED (crafted) in both byte orders and CONFIRMED
  (NeXT) on the little-endian Rhapsody floppies.
- In the old format the length is a 16-bit integer in the volume byte
  order. CONFIRMED (NeXT) for big-endian; the little-endian row is
  REFERENCE-ONLY.
- The file type byte of the new format uses the inode type shifted
  right by 12 bits: 1 FIFO, 2 character device, 4 directory, 6 block
  device, 8 regular file, 10 symbolic link, 12 socket, 14 whiteout; 0
  means unknown. CONFIRMED (crafted) for 1, 4, 8, 10. DECISION: the
  reader ignores this byte and takes the type from the inode.

### 8.3 Record rules

For a record at offset *p* of a directory of *s* bytes:

- The record length must be at least 12, a multiple of 4, and
  *p* + length ≤ *s*. Otherwise the directory is damaged from here on:
  DECISION: the reader stops reading this directory and keeps the
  entries it already has.
- The next record is at *p* + length.
- If the inode number is 0 the record is unused; skip it. Its name
  fields are meaningless.
- Otherwise the name length must be from 1 to 255 and
  8 + name length + 1 ≤ record length. Otherwise skip the record
  (DECISION).
- The record length may be larger than the name needs. The surplus is
  free space and may contain the remains of removed entries; it must
  not be parsed.
- Names are at most 255 bytes, contain no `/` and no zero byte, and are
  not unique-checked by this reader. CONFIRMED (crafted): a 255-byte
  name.

### 8.4 `.` and `..`, removed entries, empty chunks

- The first record of a directory is `.` (its own inode number) and the
  second, at offset 12, is `..` (the parent's; the root's own number in
  the root). CONFIRMED for every directory examined. The reader skips
  any record whose name is `.` or `..` wherever it appears, and does
  not rely on their position.
- Removing an entry adds its record to the length of the record before
  it; its bytes stay behind. CONFIRMED (NeXT): in the root directory of
  NS32-CD the `..` record at offset 12 has length 32 and its surplus
  still contains a complete record for `lost+found`; the next live
  record is at offset 44.
- Removing the first entry of a chunk sets its inode number to 0
  instead. A chunk with no entries is a single unused record of length
  D. CONFIRMED (NeXT): `lost+found` on the floppies is 8192 bytes long,
  with seven records of inode 0 and length 1024 after its first chunk.
- The last record of a chunk is extended to the chunk's end.

### 8.5 Walking the hierarchy

- Start at inode 2. For each live record other than `.` and `..`, read
  the inode it names (section 5.3) and act on its type (section 6.2).
- A record whose inode number is out of range, or whose inode has an
  unknown type, is skipped (DECISION).
- Directories are walked recursively. DECISION: the reader remembers
  the inode numbers of the directories on the path from the root to the
  current directory, or of all directories visited, and does not enter
  a directory twice; it also enforces a depth limit. A healthy volume
  has no directory with two names. (Deepest path on NS32-CD: 12
  components.)
- The path of an entry is the names from the root joined by `/`.
  Names are byte strings; NEXTSTEP does not define their encoding
  beyond ASCII and every name on the volumes examined is ASCII.
  DECISION: non-ASCII names are converted the way the repository
  converts Rock Ridge names.

## 9. Other objects

### 9.1 Symbolic links

The target of a symbolic link is a byte string of `size` bytes, not
zero-terminated in general, stored in one of two places.

**Inline ("fast") links**: the target is in the inode itself, at inode
offsets 40 to 40 + `size` − 1 (the area of the addresses), followed by
zero bytes.

**Ordinary links**: the target is the file content, read through the
block map like a regular file.

Which one applies:

| Volume | Rule | Evidence |
|:--|:--|:--|
| new generation with `maxsymlinklen` > 0 (section 3.5) | inline exactly when `size` < `maxsymlinklen` | CONFIRMED (crafted): links of 9, 23 and 59 bytes are inline, links of 60, 61 and 200 bytes are not. In every case `blocks` is 0 for the inline ones and non-zero for the others. |
| NEXTSTEP and OPENSTEP (old generation) | inline exactly when `blocks` = 0; such links also have bit 0x00000001 set in `flags` | CONFIRMED (NeXT): of the 272 links on NS32-CD, 263 have `blocks` 0 and the flag (sizes up to 57), and 9 have `blocks` 1 and no flag (sizes from 61). |
| other old-generation volumes (4.3BSD, SunOS) | inline exactly when `blocks` = 0 | REFERENCE-ONLY. 4.3BSD has no inline links; its links always hold a block. |

DECISION, one rule for all volumes: a symbolic link is inline when
either (a) the volume is of the new generation with `maxsymlinklen` > 0
and `size` < `maxsymlinklen`, or (b) the volume is not such a volume and
`blocks` = 0. An inline link whose `size` exceeds 60 is corrupt. The
NeXT flag bit is not consulted.

This matters to a reader even though it does not list symbolic links:
an inline link's address area contains text, and treating it as
addresses yields wild fragment numbers (the first one on NS32-CD would
be 0x2E2E2F2E).

DECISION: symbolic links are not listed and not followed, like Rock
Ridge symbolic links elsewhere in this repository. A reader therefore
never needs to read a link's target at all; the rules above are given
so that an implementation that wants to report targets does it
correctly, and so that nobody is tempted to run section 7 over an
inline link.

### 9.2 Hard links

- Several directory records, in the same or in different directories,
  may name the same inode; `nlink` counts them. CONFIRMED (NeXT):
  NS32-CD has 12,608 names for 12,540 regular files (for example
  `/sdmach` and `/odmach` are the same inode with `nlink` 3).
  CONFIRMED (crafted).
- DECISION: every name is listed as its own file with the full
  contents. The names are not marked as related. Each listing is
  charged to the output limits separately.

### 9.3 Special files

- Character and block devices carry their device number in the first
  four bytes of the address area (section 6.1); their `size` is 0.
  FIFOs and sockets have no data.
- DECISION: none of them is listed, like their Rock Ridge counterparts
  elsewhere in this repository. Their address area must not be
  interpreted as addresses.

### 9.4 Regular files

- Listed with their path, their `size`, and the Unix mode
  (`mode` AND 0o7777, together with the type, rendered as the
  repository renders modes).
- Contents through section 7.5. Zero-length files are listed with empty
  contents (52 on NS32-CD).

## 10. Differences between writers

### 10.1 Summary

| Writer | Byte order | Superblock | Generation | Directory records | Inline links | F seen | D |
|:--|:--|:--|:--|:--|:--|:--|--:|
| NEXTSTEP 3.2, floppies for NeXT and Intel | big | 4.2BSD layout | old | old | `blocks` = 0 (flag set) | 1024 | 1024 |
| NEXTSTEP 3.2 CD-ROM | big | 4.2BSD layout | old | old | `blocks` = 0 (flag set) | 2048 | 1024 |
| OPENSTEP 4.2, floppies for NeXT and Intel | big | 4.2BSD layout | old | old | none on the specimens | 1024 | 1024 |
| OPENSTEP 4.2 CD-ROM (NeXT, Intel, SPARC, PA-RISC) | big | 4.2BSD layout | old | old | not examined | 2048 | 1024 |
| Rhapsody DR2, floppies for Intel | **little** | new | new | new | none on the specimens; `maxsymlinklen` 60 | 1024 | 1024 |
| Mac OS X (REFERENCE-ONLY) | big on every processor | new | new | new | `size` < 60 | | 1024 |
| NetBSD `makefs` (as 4.4BSD, NetBSD, FreeBSD) | either | new | new | new | `size` < 60 | 512 to 2048 | 512 |
| 4.3BSD-Tahoe and later, SunOS 4, Solaris (REFERENCE-ONLY) | machine's | newer table format, `inodefmt` not 2 | old | old | `blocks` = 0 on SunOS, none on BSD | | 512 |
| 4.2BSD, 4.3BSD (REFERENCE-ONLY) | machine's | 4.2BSD layout | old | old | none | | 512 |

- NEXTSTEP 0.8 to 3.1 and 3.3, OPENSTEP 4.0 and 4.1, and all hard disks
  were not examined. INFERRED: they follow the NEXTSTEP 3.2 row; the
  Rhapsody `disk` utility labels its partitions `4.4BSD` and Rhapsody
  writes the new generation.
- Nothing in the reader is selected by the writer. Byte order, F, B,
  generation and the link rule all come from the superblock, and D is
  not needed.

### 10.2 The Linux variants `nextstep`, `nextstep-cd` and `openstep`

The Linux kernel cannot derive these properties before it has chosen a
device block size, so it asks the user to name a variant. What the
names stand for (REFERENCE-ONLY, from the Linux driver):

| Linux variant | Fragment size assumed before the superblock is read | Inode owner fields | Directory records | D | Access |
|:--|--:|:--|:--|--:|:--|
| `nextstep` | 1024 | old (16-bit) | old | 1024 | read-only |
| `nextstep-cd` | 2048 | old (16-bit) | old | 1024 | read-only |
| `openstep` | 1024 | new (32-bit) | new | 1024 | read-only |

- For all three, Linux also looks for the superblock not only at byte
  8192 of the device but at every further 8192 bytes up to 256 blocks
  on, which lets it mount a whole NeXT disk by stepping over the front
  porch. This reader does not do that: the disk label gives the
  partition's start exactly ([`disklabel.md`](disklabel.md)).
- In the terms of this document, `nextstep` and `nextstep-cd` are the
  same format with F = 1024 and F = 2048 (the NEXTSTEP 3.2 rows above),
  and `openstep` is the new generation with D = 1024 (the Rhapsody
  row). CONFIRMED (NeXT) that the names are not reliable guides to the
  operating system: the OPENSTEP 4.2 media examined are of the
  `nextstep` and `nextstep-cd` kind.
- Linux recognises inline links on all variants by `blocks` = 0.

## 11. What is not UFS1

| Signature | What it is | Reader's answer |
|:--|:--|:--|
| 0x19540119 at superblock offset 1372 (bytes `19 54 01 19` or `19 01 54 19`), with the superblock at volume byte 65,536, 8192, 0 or 262,144 | UFS2 (FreeBSD 5 and later, NetBSD) | not UFS1; rejected |
| 0x00095014, 0x00195612, 0x05231994, 0x00612195 at offset 1372 of the superblock at 8192 | HP-UX variants | rejected |
| 0x0F242697 | a firewall vendor's variant | rejected |
| the magic 0x00011954 at 8192 + 1372 but failing section 3.4 | damaged, or a coincidence | rejected |

- CONFIRMED (crafted): the UFS2 volumes built by `makefs` have
  `19 01 54 19` (little-endian) and `19 54 01 19` (big-endian) at volume
  byte 9564, the UFS1 position. FreeBSD's own tools put the UFS2
  superblock at byte 65,536 (REFERENCE-ONLY).
- UFS2 has 256-byte inodes with 64-bit addresses; nothing of this
  document applies to it. Because the UFS1 magic is required at one
  fixed place, no special code is needed to reject UFS2; the table is
  given so that tests can assert it.
- A NeXT label, a PC boot sector or any other boot code in the first
  8192 bytes does not affect recognition; only the superblock counts.
  Whole-disk NeXT images are not UFS volumes (their superblock is
  behind the front porch) and are handled by the label reader.

## 12. Worked examples

### 12.1 Big-endian, old generation: NS32-CD

The volume is partition `a` of the CD and starts at image byte 163,840.
Offsets below are volume offsets unless called image offsets.

Superblock (volume byte 8192), first 64 bytes:

```
     0  00 00 00 00 00 00 00 00 00 00 00 08 00 00 00 0c
    16  00 00 00 10 00 00 00 8c 00 00 00 40 ff ff ff e0
    32  2c c8 25 d9 00 02 58 00 00 02 44 5f 00 00 00 26
    48  00 00 20 00 00 00 08 00 00 00 00 04 00 00 00 0a
```

`sblkno` 8, `cblkno` 12, `iblkno` 16, `dblkno` 0x8C = 140, `cgoffset`
0x40 = 64, `cgmask` 0xFFFFFFE0, `size` 0x025800 = 153,600, `ncg` 0x26 =
38, B 0x2000 = 8192, F 0x0800 = 2048, `frag` 4. At 180: `00 00 00 02`
(`cpg`), `00 00 07 c0` (`ipg` 1984), `00 00 10 00` (`fpg` 4096). At
1352 to 1375: twenty `ff` bytes and `00 01 19 54`; so `postblformat` is
−1 (old generation) and the volume is big-endian. Check 8:
16 + (1984 ÷ 64) × 4 = 140.

Root inode (inode 2): *c* = 0, *r* = 2, S(0) = 0, block address
0 + 16 + 4 × 0 = 16, byte 16 × 2048 + 2 × 128 = 33,024 (image byte
196,864):

```
     0  43 ff 00 0d 00 00 00 00 00 00 00 00 00 00 04 00
    16  2c c8 17 e8 00 00 00 00 2c c8 25 d7 00 00 00 00
    32  2c c8 25 d7 00 00 00 00 00 00 00 94 00 00 00 00
    48  (zero to 103)
   104  00 00 00 01 49 05 7d c9 (zero to 127)
```

`mode` 0x43FF = 0o041777 (directory, sticky, rwxrwxrwx), `nlink` 13,
owner and group 0, `size` 1024, `db[0]` 0x94 = 148, `flags` 0, `blocks`
1. The directory's single logical block holds 1024 bytes at volume byte
148 × 2048 = 303,104:

```
     0  00 00 00 02 00 0c 00 01 2e 00 00 00 00 00 00 02
    16  00 20 00 02 2e 2e 00 00 00 00 00 03 00 14 00 0a
    32  6c 6f 73 74 2b 66 6f 75 6e 64 00 00 00 00 07 c0
    48  00 0c 00 03 75 73 72 00 00 00 00 45 00 0c 00 03
    64  64 65 76 00 00 00 00 46 00 0c 00 03 65 74 63 00
```

| Offset | Inode | Record length | Name length | Name | Note |
|--:|--:|--:|--:|:--|:--|
| 0 | 2 | 12 | 1 | `.` | |
| 12 | 2 | 32 | 2 | `..` | 20 bytes of surplus |
| (24) | (3) | (20) | (10) | (`lost+found`) | inside the surplus of the record at 12: a removed entry, **not** a record |
| 44 | 1984 | 12 | 3 | `usr` | inode 1984 is the first inode of group 1 |
| 56 | 69 | 12 | 3 | `dev` | |
| 68 | 70 | 12 | 3 | `etc` | |

The name length is the 16-bit value `00 01`, `00 02`, `00 03`: old
format, big-endian.

An inode in a staggered group, inode 37,764 (`/private/dev/console`):
*c* = 37,764 ÷ 1984 = 19; *r* = 37,764 − 19 × 1984 = 68;
S(19) = 4096 × 19 + 64 × (19 AND 31) = 77,824 + 1216 = 79,040; block
address 79,040 + 16 + 4 × (68 ÷ 64) = 79,060; byte
79,060 × 2048 + (68 modulo 64) × 128 = 161,914,880 + 512 = 161,915,392
(image byte 162,079,232). The inode there starts `21 92 00 01`: `mode`
0x2192 = 0o020622, a character device, `nlink` 1.

An inline symbolic link, inode 60: byte 16 × 2048 + 60 × 128 = 40,448.
`mode` 0o120777, `size` 23, `blocks` 0, `flags` 1; inode bytes 40 to 62
are the text `../../independentFilter` and bytes 63 to 99 are zero.

An ordinary symbolic link, inode 71,636: `size` 61, `blocks` 1, `flags`
0, `db[0]` 148,307; the 61 bytes at volume byte 148,307 × 2048 are
`../Courier-BoldOblique.font/Screen-Courier-BoldOblique.10.afm`.

### 12.2 Little-endian, new generation: crafted volume `le-4096-512`

The volume starts at byte 0 of its image. Superblock (byte 8192), first
64 bytes and bytes 1316 to 1375:

```
     0  00 00 00 00 00 00 00 00 20 00 00 00 30 00 00 00
    16  38 00 00 00 90 00 00 00 00 00 00 00 ff ff ff ff
    32  b3 b6 f6 6a 00 60 00 00 6f 5f 00 00 01 00 00 00
    48  00 10 00 00 00 02 00 00 08 00 00 00 05 00 00 00

  1316  10 00 00 00 3c 00 00 00 02 00 00 00 ff bf 40 00
  1332  01 04 00 00 ff 0f 00 00 00 00 00 00 ff 01 00 00
  1348  00 00 00 00 00 00 00 00 01 00 00 00 01 00 00 00
  1364  00 00 00 00 00 00 00 00 54 19 01 00
```

`sblkno` 32, `cblkno` 48, `iblkno` 56, `dblkno` 144, `cgoffset` 0,
`cgmask` 0xFFFFFFFF, `size` 24,576, `ncg` 1, B 4096, F 512, `frag` 8;
`maxsymlinklen` (1320) 60, `inodefmt` (1324) 2, `postblformat` (1356) 1,
magic `54 19 01 00`: little-endian, new generation. (The time stamp at
32 differs from build to build.)

Root inode: byte 56 × 512 + 2 × 128 = 28,928. `mode` `ed 41` = 0x41ED =
0o040755, `nlink` `05 00` = 5, `size` `00 04 00 00 00 00 00 00` = 1024,
`db[0]` `91 00 00 00` = 145, `blocks` 2. First 96 bytes of the directory
at byte 145 × 512 = 74,240:

```
     0  02 00 00 00 0c 00 04 01 2e 00 00 00 02 00 00 00
    16  0c 00 04 02 2e 2e 00 00 03 00 00 00 14 00 08 09
    32  68 65 6c 6c 6f 2e 74 78 74 00 00 00 04 00 00 00
    48  10 00 08 05 65 6d 70 74 79 00 00 00 05 00 00 00
    64  0c 00 04 03 64 69 72 00 06 00 00 00 14 00 04 08
    80  65 6d 70 74 79 64 69 72 00 00 00 00 07 00 00 00
```

| Offset | Inode | Record length | Type (byte 6) | Name length (byte 7) | Name |
|--:|--:|--:|--:|--:|:--|
| 0 | 2 | 12 | 4 | 1 | `.` |
| 12 | 2 | 12 | 4 | 2 | `..` |
| 24 | 3 | 20 | 8 | 9 | `hello.txt` |
| 44 | 4 | 16 | 8 | 5 | `empty` |
| 56 | 5 | 12 | 4 | 3 | `dir` |
| 68 | 6 | 20 | 4 | 8 | `emptydir` |

The same six records on the big-endian twin `be-4096-512` begin
`00 00 00 02 00 0c 04 01 2e 00 00 00`: the inode number and record
length are swapped, bytes 6 and 7 are not.

The last-block rule, inode 9 (`frag.bin`, 4796 bytes) at byte
56 × 512 + 9 × 128 = 29,824:

```
     0  a4 81 01 00 00 00 00 00 bc 12 00 00 00 00 00 00
    16  b3 b6 f6 6a 00 00 00 00 b2 b6 f6 6a 00 00 00 00
    32  b2 b6 f6 6a 00 00 00 00 c8 01 00 00 94 00 00 00
    48  (zero to 103)
   104  0a 00 00 00 (zero to 127)
```

`mode` 0x81A4 = 0o100644, `nlink` 1, `size` 0x12BC = 4796, `db[0]` 456,
`db[1]` 148, `blocks` 10. Two logical blocks: block 0 is 4096 bytes at
byte 456 × 512 = 233,472; block 1 is the remaining 700 bytes at byte
148 × 512 = 75,776 (two fragments are allocated there; only 700 bytes
belong to the file).

Single indirect, inode 8 (`indirect.bin`, 100,000 bytes): 25 logical
blocks. `db` = 248, 256, 264, …, 336 (twelve addresses, 8 apart);
`ib[0]` = 344. The indirect block at byte 344 × 512 = 176,128 begins
`60 01 00 00 68 01 00 00 70 01 00 00 …` and holds thirteen addresses
352, 360, …, 448 followed by zeros. Logical block 24 holds
100,000 − 24 × 4096 = 1696 bytes at byte 448 × 512 = 229,376.

Double indirect, inode 10 (`big.bin`, 5,243,003 bytes): 1281 logical
blocks, N = 1024. Blocks 0 to 11 through `db` (464 to 552), blocks 12 to
1035 through `ib[0]` = 560, blocks 1036 to 1280 through `ib[1]` = 8760,
whose first entry `40 22 00 00` = 8768 is the single indirect block for
blocks 1036 onward; its other entries are zero. The last logical block
holds 123 bytes.

## 13. Test vectors

### 13.1 Crafted volumes

Built with `makefs` version 20190105 (the Debian 13 package `makefs`
20190105-3, a port of NetBSD's `makefs`) from one directory tree, with
file system type `ffs`, option `version=1`, and:

| Name | Byte order option | Image size | `bsize` | `fsize` |
|:--|:--|--:|--:|--:|
| `le-4096-512` | `-B le` | 12 MiB | 4096 | 512 |
| `be-4096-512` | `-B be` | 12 MiB | 4096 | 512 |
| `be-8192-1024` | `-B be` | 16 MiB | 8192 | 1024 |
| `le-16384-2048` | `-B le` | 24 MiB | 16384 | 2048 |
| `ufs2-le`, `ufs2-be` | `-B le`, `-B be`, `version=2` | 12 MiB | 4096 | 512 |

The images embed the build time, so their own hashes are not
reproducible; the expected **contents** are. The tree (321 names):

| Path | Kind | Mode | Size | SHA-256 of contents, or note |
|:--|:--|:--|--:|:--|
| `hello.txt` | file | 0o4755 | 11 | `3da6b22a1f75cc791f1fc8300949082ba61f28b3b6f891648327897273ded16b` (the text `hello, ufs` and a line feed) |
| `hardlink` | file | 0o4755 | 11 | same inode as `hello.txt` (`nlink` 2) |
| `empty` | file | 0o600 | 0 | `e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855` |
| `twelve.bin` | file | 0o644 | 49,152 | `18e0ed74588a90b6578eb224cd012d63ccb9684777da0fac9a9963166e44ba5d` |
| `indirect.bin` | file | 0o644 | 100,000 | `3614b0e9f298d1c6c11b6ec4aaddb867ac5174932147ed28f96d9c990f776a46` |
| `frag.bin` | file | 0o644 | 4796 | `1a84021718bba75919109b5c31e2bbbea981fff0d366f847161ae643ee2dc0e4` |
| `big.bin` | file | 0o644 | 5,243,003 | `bb92a1516b2eae901d871fe1465f249a0aa8dbd79e801276fedb6fa117713e34` |
| a name of 255 letters `a` | file | 0o644 | 9 | `6f85ff6de64fd9ad54ac79632fe59450bc5b26f9099ca33b8f064021004543ff` |
| `dir`, `dir/sub`, `dir/sub/deeper` | directories | 0o755 | | |
| `dir/sub/deeper/leaf.txt` | file | 0o644 | 7 | `370a8c04b8a65bb4494275eec227f1b694db04c76da6b0b8ae88ed1ab19790a3` |
| `emptydir` | directory | 0o1777 | | no entries |
| `many` | directory | 0o755 | | 300 files `file000` to `file299`; directory size 5120 bytes (two logical blocks when B = 4096) |
| `many/file000` | file | 0o644 | 2 | `9a271f2a916b0b6ee6cecb2426f0b3206ef074578be55d9bc94f6f3fe3ab86aa` |
| `many/file299` | file | 0o644 | 4 | `0f3d5add13e3e2b7d1387d7790fe16545c72cdd3d4d27dd5175de4322fae192b` |
| `shortlink` | symbolic link | | 9 | inline; target `hello.txt` |
| `link23` | symbolic link | | 23 | inline; target `dir/sub/deeper/leaf.txt` |
| `link59` | symbolic link | | 59 | inline; 59 letters `x` |
| `link60` | symbolic link | | 60 | in a block; 60 letters `y` |
| `link61` | symbolic link | | 61 | in a block; 61 letters `z` |
| `link200` | symbolic link | | 200 | in a block; `dir/` and 196 letters `w` |
| `fifo` | FIFO | 0o644 | 0 | |

- Each `many/fileNNN` contains the decimal number NNN without leading
  zeros and a line feed.
- The four `.bin` files are pseudo-random: starting from a seed x (1 for
  `twelve.bin`, 2 for `indirect.bin`, 3 for `frag.bin`, 4 for
  `big.bin`), each output byte is obtained by replacing x with
  (x × 1,103,515,245 + 12,345) modulo 2^31 and taking bits 16 to 23 of
  the new x.
- Expected listing with the decisions of this document: 309 regular
  file names (308 inodes) and 5 directories below the root; the six
  links and the FIFO are not listed.
- Verified: on all four UFS1 volumes every file's contents, size and
  permission bits match the tree. Structure used per volume: with
  B = 4096 three single indirect blocks and one double; with B = 8192
  two single; with B = 16,384 one single.
- The two UFS2 volumes must be rejected (section 11).
- A fixture builder who wants committed fixtures should use a much
  smaller tree and image (the 12 MiB images compress well but
  `big.bin` does not); `indirect.bin` with B = 4096 already exercises a
  single indirect block, and a file larger than 12 × B + N × B bytes
  (4,243,456 with B = 4096) is needed for a double indirect block.

### 13.2 NeXT and Apple volumes

Kept outside the repository. Expected results, excluding `.` and `..`:

| Volume | Names | Directories | Regular file names | Notes |
|:--|--:|--:|--:|:--|
| NS32-FD-68K | 11 | 3 | 8 | `/.dir.tiff` is 2524 bytes, SHA-256 `08d0e7bf333748d63f553730b443e4f04730779790ffbe0a71c59e156105264b`; `/UpgradePrep.app/sdmach.Z` is 530,737 bytes, SHA-256 `d532c2b99308b164aabdd33c87c9f746aba24becfd8131cf85a3214869a1b206` |
| NS32-FD-X86a | 37 | 17 | 20 | |
| NS32-FD-X86b | 69 | 20 | 49 | |
| OS42-FD-68K | 1 | 1 | 0 | only `/lost+found` |
| OS42-FD-X86a | 26 | 13 | 13 | |
| OS42-FD-X86b | 135 | 44 | 91 | |
| RH-FD-X86a | 20 | 11 | 9 | `/usr/standalone/i386/sarld` is 148,108 bytes, SHA-256 `bbfb53f28725908dcc54e247edefd6bd78c9b5405d0c95e4b0d8b33d131afeba` |
| RH-FD-X86b | 110 | 36 | 74 | |
| NS32-CD | 15,615 | 2300 | 12,608 | plus 272 symbolic links, 273 character and 162 block devices (not listed). Sum of the sizes of the regular file names: 279,032,075 bytes. `/private/etc/passwd`: 434 bytes, SHA-256 `4280154709337a46e96e4e6ab92e70f01a73b9af1c004fee99c6b4c8b1d20ea0`. `/sdmach` and `/odmach`: 779,288 bytes, SHA-256 `df6442af6f3a7178515b485061472496688d2e7c3e37fb196ad6065fe6eefd3c`. `/NextCD/Packages/Literature.pkg/Literature.tar.Z`: 30,962,381 bytes (uses a double indirect block), SHA-256 `bc2268c9ce683a0fab769f55b788393f40509a07106ffc897dfa6c7ac8a14bea`. `/mach` is a symbolic link to `$BOOTFILE`. |
| OS42-CD (first 8 MiB) | | | | Recognised (root inode and root directory are inside the 8 MiB); most files fail individually because their blocks lie beyond L. |

The file hashes are those produced by the specification author's
throwaway reader; they are evidence of self-consistency (the `.Z` files
are valid `compress` streams, the text files are text), not an
independent reference.

### 13.3 Negative vectors

Derivable from any small valid volume by patching bytes:

1. Change one byte of the magic: not recognised.
2. Set `fsize` to 0, to 768, or to a value larger than `bsize`: not
   recognised (no division by zero may occur).
3. Set `ipg` to 0 or to `inopb` + 1: not recognised.
4. Set `ncg` to 0x7FFFFFFF: not recognised (check 6).
5. Set `iblkno` so that the root inode lies beyond L: not recognised.
6. Clear the root inode's `mode`: not recognised (check 10).
7. Set a regular file's `db[0]` to −1 or to `size`: that file fails,
   the others are still listed.
8. Set a regular file's `db[0]` to 0: its first block reads as zeros
   (a hole).
9. Set a regular file's `size` to 2^40 while leaving its addresses:
   the file is refused by the output limits before anything large is
   allocated.
10. Make `ib[1]` of a large file equal to `ib[0]`, or point an indirect
    block's entry at the indirect block itself: reading terminates (the
    walk is bounded by the file's block count) and yields garbage or a
    range error, never a hang.
11. In a directory, set a record length to 0, to 6, or to a value that
    passes the directory's end: the directory's listing stops there
    without error to the other directories.
12. Make a directory record point at the directory's own inode or at
    the root under a new name: the cycle is not followed.
13. Truncate the image in the middle of the data area: files whose
    blocks are missing fail individually.

## 14. Observed implementation gaps

Nothing in this section is to be imitated.

- The Linux driver trusts `maxsymlinklen` and the inode table geometry
  after only light checks of the superblock, requires the user to name
  the variant, and decides the byte order by trying both.
- BSD kernels decide between the superblock layouts by the single field
  at 1356, which on a 4.2BSD-layout superblock is part of a table; a
  volume whose table has 32 rows in use could be misread. No such
  volume is known. This specification inherits the test because nothing
  better exists.
- NetBSD recognises Apple volumes by a separate label at volume byte
  7168 (the text `LABL`) and then assumes D = 1024 and inline links
  shorter than 60 bytes. This reader needs neither, and the NEXTSTEP
  media examined have no such label.
- None of the sources bounds a file's `size` against what the volume
  can hold, since holes make any size legal.

## 15. Decisions

| # | Decision | Section |
|--:|:--|:--|
| D1 | The superblock is read at byte 8192 only; no backups, no scanning, no disk labels other than the NeXT label handled outside. | 2, 3.1 |
| D2 | The checks of section 3.4 are all required for recognition, including the root directory probe; geometry, free counts, status bytes and time are ignored. | 3.4 |
| D3 | B from 4096 to 65,536, F from 512. | 3.4 |
| D4 | A volume longer than the available bytes is accepted; reads fail individually. | 3.4 |
| D5 | Generation is decided by `postblformat` and `inodefmt` as in section 3.5; `maxsymlinklen` is capped at 60. | 3.5 |
| D6 | Addresses need not be block-aligned, in a data area, or unique. | 4.1, 7.4 |
| D7 | The 64-bit `size` is used in both generations. | 6.1 |
| D8 | Directories are walked record by record without knowing the chunk size. | 8.1 |
| D9 | A bad record length ends that directory; a bad name length, inode number or inode type skips that record. | 8.3, 8.5 |
| D10 | The type byte of new-format records is ignored. | 8.2 |
| D11 | Directories are never entered twice; a depth limit applies. | 8.5 |
| D12 | One rule for inline symbolic links (section 9.1); the NeXT flag is not consulted. | 9.1 |
| D13 | Symbolic links, devices, FIFOs, sockets and whiteouts are not listed. | 9.1, 9.3 |
| D14 | Each hard link is listed as an independent file. | 9.2 |
| D15 | Non-ASCII names are converted as the repository converts Rock Ridge names. | 8.5 |

## 16. Safety requirements

1. **Bounds.** Every read is checked against L before it happens. No
   offset is formed from an unchecked field. All offset arithmetic is in
   at least 64 bits: address × F, S(*c*), *n* ÷ `ipg`, `ncg` × `fpg` and
   `ncg` × `ipg` can all exceed 32 bits with hostile values.
2. **No division by an unchecked value.** B, F, `frag`, `ipg`, `inopb`,
   `nindir` and `fpg` are verified to be positive (section 3.4) before
   anything is divided by them. The checks are ordered so that B and F
   are validated before `nindir` and `inopb` are compared.
3. **Signed fields.** Addresses, `nlink`, `sblkno` to `dblkno`,
   `cgoffset`, `ncg`, `ipg`, `fpg` and `size` are signed on disk.
   Negative values are rejected where a range is checked; they never
   become large unsigned offsets silently.
4. **File size.** `size` is a 64-bit number under the writer's control
   and holes make huge sizes cheap. The reader charges the file's size
   to the output budget **before** allocating or producing contents and
   refuses the file when the budget does not allow it. It never
   allocates `size` bytes merely because the inode says so, and it
   never preallocates more than the bytes it can still account for.
5. **Indirect blocks.** The walk is driven by the logical block count
   derived from `size`, never by following pointers until something
   stops it. Each indirect block read is bounds-checked. A hostile
   volume can make every indirect entry point at the same block; the
   cost is then bounded by the block count, which the budget of item 4
   already limits. Indirect blocks may be cached, but the cache must be
   bounded.
6. **Directories.** A directory is read through the same bounded
   mechanism as a file and its `size` is limited (DECISION: a
   directory larger than the available bytes L is refused). Every
   record advances by at least 12 bytes, so a directory of *s* bytes
   yields at most *s* ÷ 12 records.
7. **Cycles and depth.** Directory inode numbers already entered are
   remembered and not re-entered; the depth of recursion is limited
   independently, and the implementation must not rely on native stack
   depth for hostile inputs. The number of entries listed is bounded by
   the sum of the directory sizes.
8. **Hard links** multiply output without multiplying input: a small
   volume can name one large file thousands of times. Each listing is
   charged to the budget (item 4).
9. **Names.** A name containing `/` or a zero byte, the names `.` and
   `..`, and empty names never reach the output path. Names are at most
   255 bytes; the path length is bounded by the depth limit.
10. **Non-files.** The address area of symbolic links, devices, FIFOs
    and sockets is never interpreted as addresses. Inline link text is
    never read beyond the 60-byte area.
11. **Detection has no side effects** and reads a bounded amount: the
    superblock, one inode and 12 bytes of one directory block.
12. **Failure is local.** An unreadable inode, file or directory makes
    that object fail or be skipped; it does not abort the listing of
    its siblings. Nothing in the volume can cause a panic, an unbounded
    loop or an unbounded allocation.
13. Nothing from the boot area, the group headers, the free maps or the
    summary area is read; their contents cannot influence the result.

## 17. Open points

1. **Little-endian old-generation volumes** (4.2BSD and 4.3BSD on VAX,
   early 386BSD, SunOS and Solaris for Intel) were not examined. The
   one rule that is specific to them is the name length in byte 6 of a
   directory record (section 8.2).
2. **Holes** (section 7.3) are specified from the sources only. A
   fixture can be made by zeroing one direct address, and one entry of
   an indirect block, in a crafted volume.
3. **Triple indirect blocks** were not exercised (a file of more than
   about 4 GiB with B = 4096 is needed, or a sparse one).
4. **The boundary of NeXT inline links.** Links of 58, 59 and 60 bytes
   were not seen on NEXTSTEP media (57 is the longest inline link and
   61 the shortest ordinary one on NS32-CD). The `blocks` = 0 rule does
   not depend on the boundary.
5. **OPENSTEP 4.x hard disks and Mac OS X volumes**: whether OPENSTEP's
   own disk initialisation writes the old or the new generation is
   unknown (its installation media are old-generation). The reader
   handles both without being told.
6. **SunOS and Solaris** volumes have further fields after offset 1316
   of the superblock and a different use of the inode's spare words;
   they should read correctly as old-generation volumes with the
   `blocks` = 0 link rule, but none was tested.
7. **The 4.2BSD-layout test** (`postblformat` = −1) can in theory be
   defeated by a volume whose rotational table uses its last row
   (section 14).
8. **Volumes inside BSD or Sun disk labels** that do not start at the
   first byte of the data are not found (section 2).
