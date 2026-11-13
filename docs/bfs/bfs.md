# BFS: the Be File System, read-only

Format specification for clean-room implementation. This document
describes everything a reader needs to list and extract the files of a
Be File System volume:

- where a volume is found and how it is recognised (sections 2 and 3);
- block runs, i-nodes and the data stream that maps a file to disk
  blocks (sections 4 to 6);
- the small-data area of an i-node, which holds the file name and small
  attributes, and the attribute directory (section 7);
- the B+tree file that implements directories, attribute directories and
  indices (section 8), and how a directory is enumerated (section 9);
- symbolic links, and everything a reader skips (section 10);
- differences between writers, byte orders and media (section 11);
- worked examples, test vectors, decisions and safety requirements
  (sections 13 to 17).

The file system is called BFS by its makers; other systems call it
"befs" to tell it apart from the UnixWare boot file system. The file
system of BeOS DR1 to DR8, which preceded BFS, is a different format and
is out of scope (section 12).

The document contains no program code and no pseudo-code.

## 1. How to read this document

### 1.1 Provenance

- The structures are described from Dominic Giampaolo's book *Practical
  File System Design with the Be File System* (**the book**), from the
  on-disk definitions and behaviour of Haiku's BFS implementation (MIT;
  **the reference implementation**), and from the Linux kernel's
  read-only driver (**the Linux driver**), read for facts only. The
  index of this directory ([`README.md`](README.md)) lists the exact
  files and revisions.
- Every structural statement was then checked on real volumes with a
  throwaway reader written by the specification's author from the rules
  below (kept outside the repository; not part of the specification).
- Volumes written by BeOS that were examined in full:
  - **R5-x86**: `image.be` of BeOS 5 Personal Edition. Little-endian,
    block size 2048, 256,000 blocks, 2,677 i-nodes reachable from the
    root (2,168 files, 252 directories, 257 symbolic links).
  - **R3-PPC**: the `Be_BFS` partition of the BeOS Release 3 for PowerPC
    CD. Big-endian, block size 2048, 233,450 blocks, 8,678 i-nodes
    reachable from the root (8,001 files, 570 directories, 107 symbolic
    links).
- Two further volumes were walked completely with the same throwaway
  reader after the document was written, as a cross-check, and agree
  with it; counts quoted in the document do not include them:
  - **AA-PPC**: the `Be_BFS` partition of the BeOS Advanced Access
    Preview Release CD for PowerPC (1997). Big-endian, superblock at
    byte 0, block size 2048, 253,536 blocks, 3,678 i-nodes reachable
    from the root. The same Apple Partition Map also has a partition of
    type `be_dr9`, which is not BFS.
  - **Haiku-x86**: the BFS partition of a Haiku nightly image
    (hrev60410, x86_64), written by Haiku itself rather than by
    `bfs_shell`. Little-endian, block size 2048, 332,800 blocks.
- Volumes written by BeOS of which only the superblock was examined: the
  x86 and PowerPC BFS tracks of the BeOS 5.0.3 Professional CD.
- Volumes written by the reference implementation's tool `bfs_shell`
  for this specification (**crafted volumes**, section 14): block sizes
  1024, 2048, 4096 and 8192, little-endian.
- No volume written by Dano or Zeta, no volume written by BeOS R4 or
  R4.5, and no big-endian volume written by Haiku was examined
  (section 18).

### 1.2 Evidence labels

- **CONFIRMED (BeOS)**: observed on R5-x86 and R3-PPC. Where a statement
  is a "for every" statement, it held for every i-node, every directory
  and every B+tree node reachable from the root of both volumes.
- **CONFIRMED (crafted)**: observed on the crafted volumes. For file
  contents the evidence is that the bytes read by following this
  document have the SHA-256 of the file that was copied in.
- **BOOK**: stated by the book, not observed.
- **REFERENCE-ONLY**: stated by, or deduced from, the reference
  implementation, not observed.
- **INFERRED**: deduced, unconfirmed.
- **DECISION**: chosen by this specification (listed in section 16). No
  decision changes how a consistent volume is read.

A statement without a label in sections 3 to 9 is CONFIRMED (BeOS) and
CONFIRMED (crafted).

### 1.3 Terms and conventions

| Term | Meaning |
|:--|:--|
| volume | One BFS file system: a sequence of equal-sized blocks. All offsets in this document are relative to the first byte of the volume unless stated otherwise. |
| block | The allocation unit. **B** is the block size in bytes (1024, 2048, 4096 or 8192). Block *n* occupies volume bytes *n*·B to (*n*+1)·B − 1. |
| **L** | The number of bytes of the volume that are actually available to the reader. Untrusted and possibly smaller than the volume claims. |
| volume byte order | The byte order of every multi-byte integer of every structure of the volume: little-endian on volumes written on Intel machines, big-endian on volumes written on PowerPC machines (section 3.2). |
| block run | An 8-byte reference to 1 to 65,535 consecutive blocks (section 4). |
| i-node address | A block run of length 1 that names the block holding an i-node. |
| i-node number | The block number of an i-node's block. Directory values are i-node numbers. |
| stream | The contents of an i-node as a sequence of bytes, mapped to blocks by its data stream structure (section 6). |
| container | An i-node whose stream is a B+tree file: a directory, an attribute directory, the index directory or an index. |
| type code | A 32-bit number usually written as four ASCII characters, most significant first: `CSTR` is 0x43535452. Stored as a 32-bit integer in the volume byte order, so `CSTR` appears on disk as the bytes `52 54 53 43` on a little-endian volume and `43 53 54 52` on a big-endian one. |

Numbers are decimal unless prefixed `0x`; mode values are also given in
octal, prefixed `0o`. Byte sequences in code style are hexadecimal, in
disk order. "Zero run" means a block run whose 8 bytes are all zero.
Signed 64-bit fields that hold −1 or −2 are stored as `FF…FF` and
`FE FF…FF` (little-endian) or `FF…FE` (big-endian).

## 2. Where a volume is found

A reader is handed a byte sequence that is supposed to be one volume. It
does not search for volumes.

| Carrier | What the reader is handed | Superblock position (section 3.1) |
|:--|:--|:--|
| Bare image of an Intel volume: `image.be` of BeOS 5 Personal Edition, a floppy image, a dump of a partition | The whole file | byte 512 |
| Partition of an Intel (MBR) partition table, partition type 0xEB | The partition | byte 512 |
| Partition of an Apple Partition Map with type string `Be_BFS` | The partition | byte 0 on PowerPC volumes |
| GUID Partition Table entry with type GUID `42465331-3BA3-10F1-802A-4861696B7521` (Haiku) | The partition | byte 512 |
| Data track of a BeOS CD | The user data of the track (2048 bytes per sector), starting at the track's INDEX 01 | byte 512 (x86 track), byte 0 (PowerPC track) |

- The partition types are REFERENCE-ONLY except `Be_BFS`, which is
  CONFIRMED (BeOS): the R3 PowerPC CD carries an Apple Partition Map
  whose sixth entry has type `Be_BFS`, starts at 512-byte block 131,424
  and is 933,801 blocks long; the BFS superblock is at the first byte of
  that partition.
- The Apple type string does not tell the byte order. The reader
  determines the byte order and the superblock position from the
  contents (section 3.1), never from the carrier.
- BeOS CDs for Intel machines are multi-track discs, not ISO 9660 images
  with an embedded partition table. Track 1 is an ISO 9660 file system
  (with an El Torito boot image); track 2 is a little-endian BFS volume;
  dual-platform discs have a track 3 with a big-endian BFS volume.
  CONFIRMED (BeOS) for the BeOS 5.0.3 Professional CD: its cue sheet
  lists three `MODE1/2352` tracks; the user data of the first sector of
  track 2 has a little-endian superblock at byte 512 (after 512 bytes of
  boot code) and the user data of the first sector of track 3 has a
  big-endian superblock at byte 0. The cue sheets of the R3 (two
  tracks), R4, R4.5 and R4.5.2 (three tracks each) Intel CDs have the
  same shape. The first 512-byte sector of track 1 holds no partition
  table.
- A raw CD image stores 2352 bytes per sector; extracting the 2048 user
  bytes of each sector and cutting the tracks is the business of the
  CD-image layer, not of the BFS reader.
- The block size of a volume is independent of the sector size of the
  medium. All three CD volumes examined, and `image.be`, use 2048-byte
  blocks. The superblock lies entirely inside the first block and inside
  the first 2048-byte sector for every block size.

## 3. The superblock

### 3.1 Position and detection

The superblock is a 164-byte structure followed by padding. It is found
at one of two positions:

| Position | Found on |
|--:|:--|
| volume byte 512 | Volumes written on Intel machines by BeOS, and all volumes written by Haiku (the first 512 bytes hold boot code or zeros). |
| volume byte 0 | Volumes written by BeOS on PowerPC machines. |

- CONFIRMED (BeOS) for R5-x86 and the x86 track of the R5 Professional
  CD (byte 512, little-endian) and for R3-PPC and the PowerPC track of
  the R5 Professional CD (byte 0, big-endian). On both PowerPC volumes
  the 164 bytes at byte 512 are zero.
- Block 0 starts at volume byte 0 in both cases: the superblock position
  shifts nothing else. CONFIRMED (BeOS): on R3-PPC the root directory's
  i-node, at block 131,072, is at partition byte 131,072 · 2048.
- DECISION D1: the reader tries position 512 first and position 0
  second, and at each position tries the byte order that the first magic
  number indicates (section 3.2). It accepts the first position at which
  **all** checks of section 3.4 pass. Any byte order is accepted at any
  position.

### 3.2 Byte order

- The field MAGIC1 holds the number 0x42465331. On a little-endian
  volume its bytes are `31 53 46 42` (ASCII `1SFB`), on a big-endian
  volume `42 46 53 31` (ASCII `BFS1`). This decides the volume byte
  order.
- The field BYTE_ORDER holds the number 0x42494745 (`BIGE` as a type
  code) written **in the volume byte order**. It therefore appears as
  `45 47 49 42` on a little-endian volume and as `42 49 47 45` on a
  big-endian one. Despite the name, its presence does not mean
  "big-endian"; it is a second witness of the byte order and must read
  as 0x42494745 in the order that MAGIC1 selected.
- Once the order is known, **every** multi-byte integer of the volume is
  read in it: superblock, i-nodes, block runs wherever they occur,
  small-data item headers, B+tree headers and nodes including the key
  length index and the values, numeric B+tree keys, and the log.
  CONFIRMED (BeOS) by reading R3-PPC completely with all fields
  byte-swapped relative to R5-x86.
- Not affected by the byte order: the volume name, file names, B+tree
  string keys, symbolic link targets (all byte strings), and the
  contents of files and attributes, which are application data.

### 3.3 Fields

Offsets are relative to the superblock position.

| Offset | Size | Field | Meaning |
|--:|--:|:--|:--|
| 0 | 32 | NAME | Volume name, UTF-8, padded with zero bytes. At most 31 bytes are significant. |
| 32 | 4 | MAGIC1 | 0x42465331. |
| 36 | 4 | BYTE_ORDER | 0x42494745 (section 3.2). |
| 40 | 4 | BLOCK_SIZE | B: 1024, 2048, 4096 or 8192. Unsigned. |
| 44 | 4 | BLOCK_SHIFT | Base-2 logarithm of B: 10 to 13. Unsigned. |
| 48 | 8 | NUM_BLOCKS | Number of blocks of the volume. Signed. |
| 56 | 8 | USED_BLOCKS | Number of allocated blocks. Signed. Informational. |
| 64 | 4 | INODE_SIZE | Size of an i-node in bytes. Always equal to B. Signed. |
| 68 | 4 | MAGIC2 | 0xDD121031. |
| 72 | 4 | BLOCKS_PER_AG | Number of **bitmap blocks** per allocation group (not the number of blocks in a group). Signed. Informational. |
| 76 | 4 | AG_SHIFT | Base-2 logarithm of the number of blocks in an allocation group. Signed. |
| 80 | 4 | NUM_AGS | Number of allocation groups. Signed. |
| 84 | 4 | FLAGS | 0x434C454E (`CLEN`, clean) or 0x44495254 (`DIRT`, dirty). |
| 88 | 8 | LOG_BLOCKS | Block run of the log area (section 3.6). |
| 96 | 8 | LOG_START | Position in the log of the first entry not yet written back. Signed. |
| 104 | 8 | LOG_END | Position in the log after the last entry. Signed. |
| 112 | 4 | MAGIC3 | 0x15B6830E. |
| 116 | 8 | ROOT_DIR | I-node address of the root directory. |
| 124 | 8 | INDICES | I-node address of the index directory, or a zero run when the volume has no indices. |
| 132 | 32 | (reserved) | Zero on every volume examined. Not checked. |

- The bytes from offset 164 to the end of the 512-byte sector are **not**
  reserved zeros: on Intel volumes written by BeOS they hold boot code,
  and on R3-PPC they hold what looks like leftover memory (readable
  text). A detector must not test them.
- An allocation group is a run of 2^AG_SHIFT consecutive blocks; it has
  no structure on disk and exists only to make block runs short
  (section 4). On all volumes examined, BLOCKS_PER_AG · B · 8 equals
  2^AG_SHIFT (one bitmap bit per block). The Linux driver's expectation
  that BLOCKS_PER_AG equals 2^AG_SHIFT is wrong (section 15).
- Volume layout, for orientation only: block 0 holds the boot sector and
  the superblock; the block bitmap follows from block 1 (one bit per
  block, ⌈NUM_BLOCKS / (8·B)⌉ blocks); the log area follows the bitmap;
  everything after the log is i-nodes, data and B+tree nodes in no
  particular order. On R5-x86: bitmap in blocks 1 to 16, log in blocks
  17 to 2064, index directory i-node at block 2065. A reader needs
  neither the bitmap nor the log.

### 3.4 Validity checks

The same checks serve content detection and opening. BFS is probed
before other file systems that have weaker signatures, so all of them
are required.

| # | Check |
|--:|:--|
| 1 | At least 164 bytes are available at the position. |
| 2 | MAGIC1 selects a byte order (section 3.2); BYTE_ORDER, MAGIC2 and MAGIC3 have their values in that order. |
| 3 | BLOCK_SIZE is 1024, 2048, 4096 or 8192, and 2^BLOCK_SHIFT = BLOCK_SIZE. |
| 4 | INODE_SIZE = BLOCK_SIZE. |
| 5 | FLAGS is `CLEN` or `DIRT`. |
| 6 | 1 ≤ AG_SHIFT ≤ 16, NUM_AGS ≥ 1, BLOCKS_PER_AG ≥ 1. |
| 7 | NUM_BLOCKS ≥ 10, and NUM_AGS = ⌈NUM_BLOCKS / 2^AG_SHIFT⌉. |
| 8 | ROOT_DIR is a valid block run (section 4.2) of length 1. |
| 9 | LOG_BLOCKS is a valid block run (section 4.2). |
| 10 | The block named by ROOT_DIR lies within L, and holds an i-node that passes the checks of section 5.5 and is a directory (section 5.3). |

- Checks 2 to 7 are the checks of the reference implementation
  (REFERENCE-ONLY as checks; every examined volume passes them). The
  values of the two BeOS volumes for check 7: R5-x86 has 256,000 blocks,
  AG_SHIFT 14, 16 groups; R3-PPC has 233,450 blocks, AG_SHIFT 14, 15
  groups. The two CD tracks: 158,720 blocks, AG_SHIFT 14, 10 groups;
  120,832 blocks, AG_SHIFT 14, 8 groups.
- DECISION D2: checks 5, 8, 9 and 10 are added by this specification.
  Check 10 is what makes detection reliable: it requires a second,
  independent magic number at a position computed from the first
  structure.
- INDICES is not checked: it may be a zero run, and the reader never
  follows it (section 10.4).
- DECISION D3: NUM_BLOCKS · B is **not** compared with L. A volume
  inside a larger partition or track is normal, and a truncated image is
  still readable up to the truncation. Every block read is individually
  checked against both NUM_BLOCKS and L (section 4.2).

### 3.5 Volume name

The name is the bytes of NAME up to the first zero byte (at most 32
bytes if there is none). `Personal Edition` on R5-x86, `BeOS PPC R3` on
R3-PPC. A slash cannot occur in a name written by the reference
implementation. The name is informational; file paths do not include it.

### 3.6 The log, and unclean volumes

- BFS is a journaling file system. The log area is the block run
  LOG_BLOCKS, used as a circular buffer. LOG_START and LOG_END are block
  positions **within the log area** (0 to length − 1), not volume block
  numbers. CONFIRMED (BeOS): R5-x86 has a log of 2048 blocks and
  LOG_START = LOG_END = 1138; R3-PPC has a log of 2048 blocks and both
  positions 1710. CONFIRMED (crafted): 743 of 2048, 267 of 512.
- A volume is **clean** when LOG_START = LOG_END. FLAGS is then `CLEN`
  on every volume examined; the book says `DIRT` exists only in memory
  while a transaction is open.
- When LOG_START ≠ LOG_END the log holds transactions that were
  committed but whose blocks were not yet written to their final
  places. REFERENCE-ONLY: each log entry is one block holding a count, a
  capacity and an array of block runs, followed by copies of the blocks
  those runs name, in order; replaying an entry means copying the block
  images to the named blocks. The reference implementation replays the
  log when mounting; the Linux driver refuses to mount such a volume.
- DECISION D4: the reader does **not** replay the log and does not
  refuse an unclean volume. It reads the volume as it is. On an unclean
  volume some metadata blocks may be stale; the checks of sections 5.5,
  6.6 and 8 and the safety requirements of section 17 are what protect
  the reader, exactly as they do on a damaged clean volume. An
  implementation may report that the volume is unclean if it has a way
  to do so.

## 4. Block runs and i-node addresses

### 4.1 Layout

A block run is 8 bytes:

| Offset | Size | Field | Meaning |
|--:|--:|:--|:--|
| 0 | 4 | GROUP | Allocation group number. Signed. |
| 4 | 2 | START | Block number within the group. Unsigned. |
| 6 | 2 | LEN | Number of blocks, 1 to 65,535. Unsigned. |

- First block of the run: GROUP · 2^AG_SHIFT + START. The run covers LEN
  consecutive blocks from there; a run may extend past the end of its
  group only if a writer made it so, which the reference implementation
  rejects (next section).
- Byte offset of the run in the volume: first block · B. Length in
  bytes: LEN · B.
- An **i-node address** is a block run with LEN = 1. The i-node number
  is the run's first block. Conversely, i-node number *n* is the address
  (*n* shifted right by AG_SHIFT, *n* modulo 2^AG_SHIFT, 1).
- A **zero run** (all 8 bytes zero) means "none" wherever a run is
  optional, and ends a list of runs (section 6).

### 4.2 Validity of a run

A run that the reader is about to follow is valid when all of these
hold; otherwise the structure containing it is corrupt:

| # | Check |
|--:|:--|
| 1 | 0 ≤ GROUP < NUM_AGS. |
| 2 | LEN ≥ 1. |
| 3 | START + LEN ≤ 2^AG_SHIFT. |
| 4 | first block ≥ 1, and first block + LEN ≤ NUM_BLOCKS. |
| 5 | (first block + LEN) · B ≤ L, computed without overflow. |

- Checks 1 to 3 are those of the reference implementation, which is
  off by one in two of them (section 15). Checks 4 and 5 are DECISION
  D5. Block 0 is the superblock's block and is never file data or an
  i-node.
- Checks 1 to 4 hold for every run of every stream reachable on the
  BeOS volumes (12,825 runs, including attribute directories and
  attributes) and on the crafted volumes. In particular no run crosses
  the end of its allocation group.
- With AG_SHIFT ≤ 16 and GROUP < NUM_AGS, first block fits easily in 64
  bits, but the product with B must still be computed in 64 bits and
  compared against L before any conversion to a native index.

## 5. The i-node

### 5.1 Layout

An i-node occupies one whole block. The fixed part is 232 bytes; the
rest of the block is the small-data area (section 7). Offsets are
relative to the start of the block.

| Offset | Size | Field | Meaning |
|--:|--:|:--|:--|
| 0 | 4 | MAGIC | 0x3BBE0AD9. |
| 4 | 8 | INODE_NUM | The i-node's own address. |
| 12 | 4 | UID | Owner. Signed. |
| 16 | 4 | GID | Group. Signed. |
| 20 | 4 | MODE | File type and permissions (section 5.3). |
| 24 | 4 | FLAGS | Section 5.2. |
| 28 | 8 | CREATE_TIME | Section 5.4. |
| 36 | 8 | MODIFIED_TIME | Section 5.4. |
| 44 | 8 | PARENT | I-node address of the containing directory; of the file for an attribute directory; of the attribute directory for an attribute. The root directory and the index directory name themselves. |
| 52 | 8 | ATTRIBUTES | I-node address of the attribute directory (section 7.5), or a zero run. |
| 60 | 4 | TYPE | For an attribute i-node: the attribute's type code. Zero otherwise. |
| 64 | 4 | INODE_SIZE | Equal to the superblock's INODE_SIZE. Signed. |
| 68 | 4 | ETC | A memory pointer of the writing system. Non-zero garbage on volumes written by BeOS, zero on volumes written by Haiku. Ignore. |
| 72 | 144 | DATA | The data stream (section 6), or, for a short symbolic link, the link text (section 10.1). |
| 216 | 8 | STATUS_CHANGE_TIME | Haiku only (section 5.4). Zero on volumes written by BeOS. |
| 224 | 8 | (padding) | Zero on every i-node examined. Ignore. |
| 232 | | SMALL_DATA | Start of the small-data area. |

The book and the Linux driver describe offsets 216 to 231 as 16 bytes
of padding; the reference implementation uses the first 8.

### 5.2 Flags

| Bit | Name | Meaning |
|--:|:--|:--|
| 0x00000001 | IN_USE | Set on every live i-node. |
| 0x00000004 | ATTR_INODE | BOOK: set on attribute i-nodes. Not set on any of the 766 attribute i-nodes of the BeOS volumes (their flags are 0x00020001). Do not rely on it; attribute i-nodes are recognised by MODE (section 5.3). |
| 0x00000008 | LOGGED | Changes to the stream are journaled. Set on directories and other containers written by BeOS, and on long symbolic links written by Haiku. No meaning for a reader. |
| 0x00000010 | DELETED | The i-node has been removed. The book says BeOS sets it in memory only. |
| 0x00000020 | NOT_READY | REFERENCE-ONLY: set by Haiku while an i-node is being created. |
| 0x00000040 | LONG_SYMLINK | For a symbolic link: the target is in the stream, not in the i-node (section 10.1). |
| 0x0000FFFF | | Mask of the flags that are meant to be permanent. |
| 0x00010000 and above | | In-memory state that leaks to disk. Ignore. |

Flag words seen: on the BeOS volumes 0x00000001 (files, symbolic
links), 0x00000009 and 0x00020009 (containers), 0x00020001 (attribute
i-nodes); on the crafted volumes 0x00000001 and 0x00040001 (everything,
directories included) and 0x00040049 (long symbolic link). A reader
tests only IN_USE, DELETED, NOT_READY and LONG_SYMLINK.

### 5.3 Mode and file type

MODE is a 32-bit word. Its low 16 bits are the POSIX mode; the high bits
mark the BFS-specific kinds.

| Bits (hex) | Octal | Name | Meaning |
|--:|--:|:--|:--|
| 0x000001FF | 0o777 | | Permission bits, POSIX layout (owner, group, other; read, write, execute). |
| 0x00000E00 | 0o7000 | | Set-user-id, set-group-id, sticky. |
| 0x0000F000 | 0o170000 | S_IFMT | File type field, below. |
| 0x00040000 | 0o1000000 | S_DOUBLE_INDEX | Index with 64-bit floating-point keys. |
| 0x00080000 | 0o2000000 | S_ALLOW_DUPS | Container allows duplicate keys (unused by BeOS). |
| 0x00200000 | 0o10000000 | S_LONG_LONG_INDEX | Index with signed 64-bit keys. |
| 0x00400000 | 0o20000000 | S_ULONG_LONG_INDEX | Index with unsigned 64-bit keys. |
| 0x00800000 | 0o40000000 | S_FLOAT_INDEX | Index with 32-bit floating-point keys. |
| 0x01000000 | 0o100000000 | S_STR_INDEX | Container with string keys. |
| 0x02000000 | 0o200000000 | S_INT_INDEX | Index with signed 32-bit keys. |
| 0x04000000 | 0o400000000 | S_UINT_INDEX | Index with unsigned 32-bit keys. |
| 0x08000000 | 0o1000000000 | S_ATTR_DIR | Attribute directory. |
| 0x10000000 | 0o2000000000 | S_ATTR | Attribute. |
| 0x20000000 | 0o4000000000 | S_INDEX_DIR | The index directory, or an index. |

Values of the file type field (MODE and 0xF000):

| Value | Kind |
|--:|:--|
| 0x8000 | Regular file |
| 0x4000 | Directory (any container) |
| 0xA000 | Symbolic link |
| 0x1000, 0x2000, 0x6000, 0xC000 | FIFO, character device, block device, socket (section 10.3) |

The kind of an i-node is decided in this order:

| Test | Kind | Seen as MODE |
|:--|:--|:--|
| S_INDEX_DIR set | Index directory or index. Never listed. | 0o4100040700 (index directory, R5-x86), 0o4100040000 (string index), 0o4010040000 (64-bit index) |
| S_ATTR_DIR set | Attribute directory. Never listed. | 0o1100040666 |
| S_ATTR set | Attribute. Never listed. | 0o2000100666 |
| type field 0x4000 | Directory | 0o100040755, and 0o040755 on 105 of the 570 directories of R3-PPC |
| type field 0x8000 | Regular file | 0o100644, 0o100755 |
| type field 0xA000 | Symbolic link | 0o120777 |
| anything else | Special node or unknown; not listed | none seen |

- A directory normally has S_STR_INDEX set as well as the directory
  type, but not always (105 of 570 directories on R3-PPC, 207 of 222 on
  AA-PPC lack it). A reader must not require it and must
  mask it out before interpreting the low bits. Because 0x01000000 is
  outside the low 16 bits, the plain POSIX reading of the low 16 bits is
  unaffected.
- The three extended kinds are tested first because an attribute
  directory, the index directory and an index all carry the directory
  type as well, and an attribute carries the regular-file type.
- The Unix mode string that this repository reports for an entry is
  built from the type field and the low 12 bits in the usual way
  (`-rwxr-xr-x` for 0o100755).

### 5.4 Times

- CREATE_TIME and MODIFIED_TIME are signed 64-bit values: the POSIX time
  in seconds, shifted left by 16 bits, with the low 16 bits used as a
  uniquifier (BeOS) or as a mixture of sub-second time and uniquifier
  (Haiku). Seconds = value shifted right arithmetically by 16.
  CONFIRMED (BeOS): the root directory of R5-x86 has CREATE_TIME bytes
  `00 00 37 50 DE 38 00 00`, value 0x38DE50370000, seconds 954,093,623,
  which is 26 March 2000 18:00:23 UTC.
- REFERENCE-ONLY for the low 16 bits written by Haiku: if they are
  0xF000 or above, the sub-second part is zero; otherwise the
  nanoseconds are the low 16 bits with their lowest four bits cleared,
  shifted left by 14.
- STATUS_CHANGE_TIME has the same encoding. It is zero on volumes
  written by BeOS; the reference implementation substitutes
  MODIFIED_TIME when it is smaller than MODIFIED_TIME.
- There is no access time.
- This repository's entry metadata has no time fields; a reader does not
  need the times at all.

### 5.5 Validity checks

An i-node read from block *n* is accepted when all of these hold:

| # | Check |
|--:|:--|
| 1 | MAGIC = 0x3BBE0AD9. |
| 2 | FLAGS has IN_USE set and has neither DELETED nor NOT_READY set. |
| 3 | INODE_SIZE = B. |
| 4 | INODE_NUM is the address of block *n*: GROUP = *n* shifted right by AG_SHIFT, START = *n* modulo 2^AG_SHIFT, LEN = 1. |

- Checks 1 to 3 are those of the reference implementation, which
  instead of check 4 only requires LEN = 1 and also range-checks PARENT
  and ATTRIBUTES.
- DECISION D6: check 4 is required. It held for all 11,355 i-nodes of
  the two BeOS volumes and for every i-node of the crafted volumes, and
  it means a directory value that points into the middle of file data
  is rejected even when the data happens to contain the magic number.
- PARENT and ATTRIBUTES are not checked: the reader never follows them
  (sections 7.5 and 9.4).
- An i-node that fails is not listed, and what refers to it is treated
  as section 9.4 says.

## 6. The data stream

### 6.1 Layout

The 144 bytes at i-node offset 72. Offsets below are relative to the
start of the i-node block.

| Offset | Size | Field | Meaning |
|--:|--:|:--|:--|
| 72 | 96 | DIRECT | 12 block runs. |
| 168 | 8 | MAX_DIRECT_RANGE | Stream offset at which the direct range ends: the total byte length of the direct runs. Signed. |
| 176 | 8 | INDIRECT | Block run of the indirect array, or a zero run. |
| 184 | 8 | MAX_INDIRECT_RANGE | Stream offset at which the indirect range ends, or 0 when there is no indirect range. Signed. |
| 192 | 8 | DOUBLE_INDIRECT | Block run of the double-indirect array, or a zero run. |
| 200 | 8 | MAX_DOUBLE_INDIRECT_RANGE | Stream offset at which the double-indirect range ends, or 0 when there is none. Signed. |
| 208 | 8 | SIZE | Length of the stream in bytes. Signed. |

- The three MAX fields are **cumulative** stream offsets, not range
  lengths. CONFIRMED (crafted): a file with 63 direct blocks, 2,045
  indirect blocks and 108 double-indirect blocks of 1024 bytes has the
  values 64,512, 2,158,592 and 2,269,184 (section 13.7).
- The stream is the concatenation, in order, of the blocks of the direct
  runs, then of the runs listed in the indirect array, then of the runs
  listed through the double-indirect array. There are no holes: a
  stream offset maps to a block if and only if it is below the end of
  the last range.
- Regular files, directories and other containers, attributes and long
  symbolic links have a data stream. A short symbolic link does not: its
  144 bytes hold text and must not be read as runs (section 10.1).

### 6.2 Direct range

- DIRECT holds up to 12 runs in stream order. The list ends at the first
  zero run, or after the twelfth run.
- Runs have any length from 1 to 65,535 blocks and need not be in
  ascending disk order.

### 6.3 Indirect range

- INDIRECT is a block run. Its blocks, concatenated, form an array of
  block runs: (LEN · B) / 8 entries.
- The array lists further runs of the stream in order. It ends at the
  first zero run, or at the end of the array when every entry is used.
  CONFIRMED (crafted): arrays with all 512, 1,024 and 2,048 entries
  used and no terminator. CONFIRMED (BeOS): three files of R3-PPC; one
  has 67 of 512 entries used and the rest zero.
- The length of the INDIRECT run is the writer's choice and must be
  taken from the run. BeOS allocates 4096 bytes (2 blocks at B = 2048,
  CONFIRMED (BeOS); BOOK: "always at least 4K"). Haiku always allocates
  4 blocks whatever the block size (CONFIRMED (crafted) for B = 1024,
  2048, 4096).
- Runs in the array have any length, like direct runs.
- On every file examined that has an indirect range, all 12 direct runs
  are in use. A reader does not depend on that.
- Directories can have an indirect range too. CONFIRMED (crafted): a
  directory of about 1,200 entries whose B+tree file has 12 direct runs
  and an indirect array.

### 6.4 Double-indirect range

- DOUBLE_INDIRECT is a block run whose blocks form an array of block
  runs, as in section 6.3. Each entry of this array is the run of
  **another array of block runs** (an indirect array), and each entry of
  those arrays is a run of the stream. Both levels end at the first
  zero run or at the end of the array.
- The stream continues, after the indirect range, with the runs of the
  first indirect array in order, then those of the second, and so on.
- Fixed-size rule. So that a writer can find a stream position by
  division instead of a linear search, all data runs of the
  double-indirect range have the same length, and all the arrays have
  the same length:

  | Writer | B | Length of DOUBLE_INDIRECT | Length of each indirect array | Length of each data run | Evidence |
  |:--|--:|--:|--:|--:|:--|
  | Haiku | 1024 | 4 blocks | 4 blocks | 4 blocks | CONFIRMED (crafted) |
  | Haiku | 2048 | 2 blocks | 2 blocks | 2 blocks | CONFIRMED (crafted) |
  | Haiku | 4096 | 1 block | 1 block | 1 block | CONFIRMED (crafted) |
  | Haiku | 8192 | 1 block | 1 block | 1 block | REFERENCE-ONLY |
  | BeOS | any | not observed | not observed | 4 blocks | BOOK ("fixed-length runs of blocks (currently four)"); the Linux driver's authors were unsure whether it is 4 blocks or 4096 bytes |

  For Haiku the common length is 4096 / B blocks, at least 1, and the
  reference implementation takes it from the LEN of DOUBLE_INDIRECT when
  reading.
- No BeOS-written file with a double-indirect range was available
  (section 18). The reading procedure below does not depend on the
  fixed-size rule and is therefore correct for either interpretation.

### 6.5 Size and allocation

- SIZE is the number of meaningful bytes. The blocks of the ranges
  normally cover SIZE rounded up to a block, and may cover more:
  - Container streams are grown in steps: on R5-x86 the index directory
    has SIZE 2048 and one direct run of 64 blocks (MAX_DIRECT_RANGE
    131,072). Thirteen directories of the BeOS volumes and some of the
    crafted ones also have more blocks than SIZE needs.
  - Writers preallocate while a file is open and trim on close; a volume
    that was not cleanly unmounted can show the surplus.
- Bytes of the last block beyond SIZE, and whole blocks beyond SIZE,
  are not part of the stream.
- On every regular file examined, SIZE is at most the end of the last
  range. A SIZE beyond it is corruption (section 6.6).
- An empty file has SIZE 0 and all runs zero (454 examples on the BeOS
  volumes).

### 6.6 Reading a stream

DECISION D7: a stream is read by walking the runs in order and stopping
as soon as SIZE bytes have been produced. Positions are never computed
by division, and the MAX fields are not used to decide where a range
ends.

1. If SIZE is negative, the i-node is corrupt. If SIZE is 0, the stream
   is empty and no run is examined.
2. Walk DIRECT (section 6.2). Each run must be valid (section 4.2). Take
   its blocks in order until SIZE bytes have been produced.
3. If more bytes are needed and INDIRECT is not a zero run: INDIRECT
   must be valid; read its array and walk it (section 6.3) in the same
   way.
4. If more bytes are still needed and DOUBLE_INDIRECT is not a zero run:
   it must be valid; walk its array; each entry must be valid; read the
   indirect array it names and walk that (section 6.4).
5. If the runs are exhausted before SIZE bytes were produced, the stream
   is corrupt.

- DECISION D8: a corrupt stream makes the entry fail. Missing bytes are
  not replaced by zeros and a short file is not delivered as if it were
  complete.
- The MAX fields may be used as a consistency check (MAX_DIRECT_RANGE
  equals the byte length of the direct runs on every i-node examined),
  but a mismatch alone is not a reason to reject a stream that the walk
  can satisfy.
- Random access to a stream, which the B+tree needs (section 8), uses
  the same walk: to read bytes at stream offset *p*, skip runs until the
  one that contains *p*. An implementation will normally build the list
  of runs once per container and reuse it.
- All the limits of section 17 apply to the walk.

### 6.7 Limits

- BOOK, for B = 1024: the direct range maps at most 12 · 65,535 blocks;
  an indirect array of 4096 bytes lists 512 runs; the double-indirect
  range maps 512 · 512 runs of 4 blocks, 1 GiB. The largest possible
  file is about 34 GiB at B = 1024 and grows with the block size.
- The largest stream on the BeOS volumes is 36,143,634 bytes.

## 7. The small-data area and attributes

### 7.1 Items

The small-data area runs from i-node offset 232 to INODE_SIZE. It is a
packed sequence of items with no alignment between them. One item:

| Offset in item | Size | Field | Meaning |
|--:|--:|:--|:--|
| 0 | 4 | TYPE | Type code of the value. |
| 4 | 2 | NAME_SIZE | Length of the name in bytes, not counting any terminator. Unsigned. |
| 6 | 2 | DATA_SIZE | Length of the value in bytes. Unsigned. |
| 8 | NAME_SIZE | NAME | Attribute name, UTF-8, not terminated. |
| 8 + NAME_SIZE | 3 | | Three zero bytes. |
| 11 + NAME_SIZE | DATA_SIZE | DATA | The value. |
| 11 + NAME_SIZE + DATA_SIZE | 1 | | One zero byte. |

- The size of an item is NAME_SIZE + DATA_SIZE + 12. The next item
  starts immediately after.
- The three bytes after the name and the one byte after the value are
  zero on every one of the about 34,000 items of the BeOS volumes and on
  every item of the crafted volumes. They act as
  terminators for the name and for string values; a reader skips them
  without testing them. The book's description of the item omits them;
  an implementation that follows the book alone misreads every value.
- The fields are in the volume byte order. On R3-PPC the name item of a
  file begins `43 53 54 52 00 01 00 0C`; on R5-x86 a name item begins
  `52 54 53 43 01 00 05 00`.

### 7.2 End of the list

The list ends at the first of:

- an item whose NAME_SIZE is 0;
- a position from which fewer than 8 bytes remain before INODE_SIZE.

What the terminating item looks like depends on the writer:

- BeOS writes a **free-space item**: TYPE 0, NAME_SIZE 0, and DATA_SIZE
  equal to the number of bytes from the item to the end of the i-node
  minus 12, so that by the size rule of section 7.1 it extends exactly
  to the end of the i-node. CONFIRMED (BeOS) on every i-node; for
  example an i-node of 2048 bytes with nothing in the small-data area
  has the item `00 00 00 00 00 00 0C 07` at offset 232 (DATA_SIZE 1804
  = 2048 − 232 − 12).
- Haiku writes eight zero bytes (DATA_SIZE 0). CONFIRMED (crafted).

An area that is filled to the last bytes has no terminator at all: the
list then ends by the second rule (two i-nodes of AA-PPC).

A reader must stop at NAME_SIZE = 0 and must not use the DATA_SIZE of
the terminator for anything. Before using an item it checks that the
whole item (NAME_SIZE + DATA_SIZE + 12 bytes) ends at or before
INODE_SIZE; if not, the list ends there and the remaining bytes are
ignored (DECISION D9: a damaged small-data area costs the attributes,
not the file).

### 7.3 The file name

- Every i-node that is entered in a directory carries its own name as a
  small-data item: TYPE `CSTR` (0x43535452), NAME_SIZE 1, NAME the
  single byte 0x13, DATA the name without a terminator, DATA_SIZE the
  length of the name.
- It is usually the first item but not always (18 i-nodes on the BeOS
  volumes have other items before it).
- The root directory has no name item (on the BeOS volumes its first
  item is the attribute `be:volume_id`, type `ULLG`, 8 bytes). Attribute
  directories, attributes, the index directory and indices have none
  either.
- On all 11,353 named i-nodes of the BeOS volumes, and on the crafted
  volumes, the name item equals the key under which the i-node is
  entered in its directory.
- DECISION D10: the reader takes names from directory keys (section 9)
  and does not need the name item. It may ignore the small-data area
  completely.

### 7.4 Attributes in the small-data area

Every other item is an attribute of the file: NAME is the attribute
name, TYPE its type code, DATA its value. Examples from the BeOS
volumes:

| Name | Type | Value |
|:--|:--|:--|
| `BEOS:TYPE` | `MIMS` | The file's MIME type as a zero-terminated string (the terminator is counted in DATA_SIZE), e.g. `application/x-vnd.Be-elfexecutable`. |
| `BEOS:APP_SIG` | `MIMS` | Application signature. |
| `BEOS:M:STD_ICON` | `MICN` | 16 × 16 icon, 256 bytes. |
| `BEOS:L:STD_ICON` | `ICON` | 32 × 32 icon, 1024 bytes. Often does not fit and is then found in the attribute directory (section 7.5). |
| `_trk/pinfo_le`, `_trk/pinfo` | `RAWT` | Tracker's icon position; the suffix tells the byte order of the value. |
| `MACOS:CREATOR` | `ULNG` | On R3-PPC, on files that came from a Macintosh: the four-character creator code as a 32-bit number. `MACOS:HFS_FLAGS` (`USHT`) accompanies it. |

Type codes seen (name of the constant in the Be API, from the reference
implementation's headers): `CSTR` string, `MIMS` MIME string, `LONG`
int32, `ULNG` uint32, `LLNG` int64, `ULLG` uint64, `SHRT` int16, `USHT`
uint16, `BOOL`, `FLOT` float, `DBLE` double, `TIME`, `RAWT` raw bytes,
`RECT`, `BPNT` point, `MSGG` flattened message, `ICON`, `MICN`, `CLRB`,
`MIME`. Applications use private codes freely (`pref`, `type`, `info`
were seen). The type code does not affect parsing.

Attribute values are application data in the byte order of the machine
that wrote them.

This repository's entry metadata cannot carry attributes; a reader does
not surface them (section 18 lists this as a follow-up).

### 7.5 The attribute directory

- Attributes that do not fit in the small-data area are stored outside
  the i-node. The i-node's ATTRIBUTES field is then the address of an
  **attribute directory**: an i-node with S_ATTR_DIR set (seen:
  0o1100040666), PARENT naming the file, whose stream is a B+tree with
  string keys (section 8).
- Each key is an attribute name; each value is the i-node number of an
  **attribute i-node**: S_ATTR set (seen: 0o2000100666), TYPE field
  holding the attribute's type code, PARENT naming the attribute
  directory, and the attribute's value as its stream (SIZE bytes).
- An attribute directory has no `.` and `..` entries. Attribute
  directories and attribute i-nodes have no name item.
- An attribute is either in the small-data area or in the attribute
  directory, never both (BOOK).
- CONFIRMED (BeOS): 337 i-nodes have an attribute directory. Example,
  R5-x86: the file `beos/apps/3dmiX` (i-node 131,141) has ATTRIBUTES
  (0, 2612, 1); i-node 2612 has MODE 0o1100040666; its tree has the two
  keys `BEOS:L:STD_ICON` → 2614 and
  `BEOS:L:application/x-vnd.be.3dsoundmixer_master` → 2615; i-node 2614
  has MODE 0o2000100666, TYPE 0x49434F4E (`ICON`), SIZE 1024.
- A reader that does not surface attributes never follows ATTRIBUTES.

## 8. The B+tree file

Directories, attribute directories, the index directory and indices are
containers: i-nodes whose stream is a B+tree file. The tree maps keys
to 64-bit values. In a directory the key is a file name and the value
an i-node number.

### 8.1 File layout and header

The stream is an array of **nodes** of 1024 bytes. Node *k* is at stream
offset 1024 · *k*; nodes are named by their stream offset. The node size
is 1024 whatever the block size, so at B = 2048 two nodes share a block
and at B = 1024 a node can straddle two runs of the stream.

The first 1024 bytes are the header node. Only its first 40 bytes are
used:

| Offset | Size | Field | Meaning |
|--:|--:|:--|:--|
| 0 | 4 | MAGIC | 0x69F6C2E8. |
| 4 | 4 | NODE_SIZE | 1024. Unsigned. |
| 8 | 4 | MAX_LEVELS | Depth of the tree: 1 when the root is a leaf. Unsigned. |
| 12 | 4 | DATA_TYPE | Key type (section 8.5). Unsigned. |
| 16 | 8 | ROOT | Stream offset of the root node. Signed. |
| 24 | 8 | FREE | Stream offset of the first free node, or −1. Signed. |
| 32 | 8 | MAX_SIZE | Size of the tree file in bytes. Signed. |

- MAX_SIZE equals the stream's SIZE on every container examined (the
  allocated length of the stream can be larger, section 6.5). The
  reference implementation only logs a difference.
- MAX_LEVELS equals the real depth on every tree examined (1, 2 and 3).
- A newly created container has SIZE 2048: the header and an empty or
  nearly empty root at offset 1024.

Validity of the header:

| # | Check |
|--:|:--|
| 1 | The stream's SIZE is at least 2048 and the first 40 bytes can be read. |
| 2 | MAGIC = 0x69F6C2E8. |
| 3 | NODE_SIZE = 1024 (DECISION D11; the duplicate structures of section 8.6 are defined for this size only). |
| 4 | DATA_TYPE ≤ 6; for a directory or attribute directory, DATA_TYPE = 0. |
| 5 | ROOT is a valid node offset (below). |

A **valid node offset** is a multiple of 1024 that is at least 1024 and
at most S − 1024, where S is the smaller of the stream's SIZE and, if it
is positive, MAX_SIZE. DECISION D12: both bounds are applied; the
reference implementation uses MAX_SIZE alone and does not require the
multiple.

### 8.2 Node layout

Interior nodes and leaf nodes have the same layout. Offsets are relative
to the start of the node.

| Offset | Size | Field | Meaning |
|--:|--:|:--|:--|
| 0 | 8 | LEFT | Stream offset of the previous node of the same level, or −1. Signed. |
| 8 | 8 | RIGHT | Stream offset of the next node of the same level, or −1. Signed. |
| 16 | 8 | OVERFLOW | −1 in a leaf. In an interior node, the offset of the last child. −2 in a free node. Signed. |
| 24 | 2 | KEY_COUNT | Number of keys *n*. Unsigned. |
| 26 | 2 | KEYS_LENGTH | Total length of all keys in bytes. Unsigned. |
| 28 | KEYS_LENGTH | KEYS | The keys, concatenated in ascending order, with no separators or terminators. |
| | | (padding) | Zero bytes up to the next multiple of 8. |
| *I* | 2 · *n* | KEY_INDEX | For each key, the offset **of its end** within KEYS, as an unsigned 16-bit number. |
| *I* + 2 · *n* | 8 · *n* | VALUES | For each key, a signed 64-bit value. |

- *I* = 28 + KEYS_LENGTH, rounded up to a multiple of **8**. The book
  says "a multiple of four"; that is wrong for the format as written by
  BeOS: of the 96 leaves of the name index of R5-x86, 41 have a
  KEYS_LENGTH for which the two rules differ, and only rounding to 8
  yields sorted keys and valid values. Same on the crafted volumes.
- Key *i* (counting from 0) occupies KEYS bytes from KEY_INDEX[*i* − 1]
  (0 for the first key) up to but not including KEY_INDEX[*i*].
- The values are **not** aligned to 8 bytes when *n* is odd: they follow
  the index directly.
- A node is full when *I* + 10 · *n* would exceed 1024. Unused bytes at
  the end of a node are arbitrary.

Validity of a node:

| # | Check |
|--:|:--|
| 1 | *I* + 10 · *n* ≤ 1024. |
| 2 | KEY_INDEX is non-decreasing and its last entry is at most KEYS_LENGTH. |
| 3 | Every key length is at most 256. In a directory or attribute directory every key length is at least 1 (section 9.2 limits names further). |
| 4 | OVERFLOW is −1 (leaf) or a valid node offset (interior). |

- LEFT and RIGHT are not checked because they are not used (section
  8.7).
- A node that fails is corrupt; section 8.7 says what follows.

### 8.3 Leaf nodes

OVERFLOW = −1. Each value belongs to its key:

- in a directory, the i-node number of the entry;
- in an attribute directory, the i-node number of the attribute i-node;
- in the index directory, the i-node number of an index;
- in an index, the i-node number of a file that has the key as its
  attribute value, or a duplicate link (section 8.6).

The leaves form a doubly linked list in key order through LEFT and
RIGHT; the first leaf has LEFT = −1 and the last RIGHT = −1.

### 8.4 Interior nodes

OVERFLOW is a valid node offset. An interior node with *n* keys has
*n* + 1 children:

- VALUES[*i*] is the stream offset of the child whose keys are all less
  than or equal to key *i* (and greater than key *i* − 1). Key *i* is a
  copy of the **largest key in that child's subtree**.
- OVERFLOW is the offset of the last child, whose keys are all greater
  than key *n* − 1.

CONFIRMED (BeOS): the directory `beos/bin` of R5-x86 (section 13.6).
Interior nodes of one level are linked through LEFT and RIGHT like
leaves (CONFIRMED on three-level trees of both writers). An interior
node may have zero keys, in which case OVERFLOW is its only child
(REFERENCE-ONLY).

### 8.5 Key types and comparison

| DATA_TYPE | Keys | Mode bit of the container |
|--:|:--|:--|
| 0 | Byte strings of 1 to 256 bytes, not terminated | S_STR_INDEX, or none (plain directories) |
| 1 | Signed 32-bit integers (4 bytes) | S_INT_INDEX |
| 2 | Unsigned 32-bit integers (4 bytes) | S_UINT_INDEX |
| 3 | Signed 64-bit integers (8 bytes) | S_LONG_LONG_INDEX |
| 4 | Unsigned 64-bit integers (8 bytes) | S_ULONG_LONG_INDEX |
| 5 | 32-bit IEEE floating point (4 bytes) | S_FLOAT_INDEX |
| 6 | 64-bit IEEE floating point (8 bytes) | S_DOUBLE_INDEX |

- Directories, attribute directories and the index directory use type
  0. The standard indices `name` and `BEOS:APP_SIG` use type 0, `size`
  and `last_modified` type 3.
- Numeric keys are stored in the volume byte order and compared
  numerically.
- String keys are compared byte by byte over the shorter length; if
  that part is equal, the shorter key sorts first. REFERENCE-ONLY that
  bytes compare as unsigned values. CONFIRMED (BeOS and crafted) for
  ASCII: in all 839 directories examined (822 on the BeOS volumes, 17 on a
  crafted one) the keys of each directory,
  read in tree order, are in ascending unsigned byte order; the names
  `.` and `..` therefore come first unless a name starts with a byte
  below 0x2E (space, `!`, `-` and so on). No directory examined mixes
  names with bytes of 0x80 and above with others, so the ordering of
  such names by BeOS is unconfirmed (section 18).
- Comparison is exact: there is no case folding and no Unicode
  normalisation. `Makefile` and `makefile` are different names.
- A reader that enumerates (section 8.7) never compares keys and does
  not depend on the order. DECISION D13: the reader does not reject a
  tree because its keys are out of order.

### 8.6 Duplicates

A tree that allows duplicates can hold several values under one key.
Only indices do. Directories and attribute directories never hold
duplicates (BOOK; CONFIRMED: none in 839 directories), so this section
matters to a reader only for recognising and rejecting them.

The two most significant bits of a leaf value say what it is. They are
free because an i-node number is a block number and never needs them.

| Top two bits | Meaning of the value |
|:--|:--|
| 00 | A plain value (an i-node number). |
| 11 | **Duplicate fragment** link. |
| 10 | **Duplicate node** link. |
| 01 | Not used. |

For both kinds of link, clear the top two bits and split the rest:

- bits 10 to 61 (the value with its low 10 bits cleared): the stream
  offset of a node of the same tree file;
- bits 0 to 9: for a fragment link, the fragment number; zero for a
  duplicate node link.

**Duplicate fragment.** A fragment node is 1024 bytes divided into 16
fragments of 64 bytes; it has no node header. Fragment *f* is at node
offset 64 · *f* and holds eight signed 64-bit numbers: a count *c* (1 to
7) followed by up to seven values, of which the first *c* are the
duplicates of the key, in ascending order. One fragment node serves
several keys.

**Duplicate node.** Used when a key has more than seven values. Layout:

| Offset | Size | Field |
|--:|--:|:--|
| 0 | 8 | LEFT: previous duplicate node of the same key, or −1. |
| 8 | 8 | RIGHT: next duplicate node of the same key, or −1. |
| 16 | 8 | Count *c* of values in this node, 1 to 125. |
| 24 | 8 · *c* | The values, ascending. |

The duplicates of a key are the values of the linked node followed by
those of the nodes reached through RIGHT.

- CONFIRMED (BeOS), name index of R5-x86: the key `3dmiX` has the value
  bytes `01 08 00 00 00 00 00 C0` (0xC000000000000801): a fragment link
  to node 2048, fragment 1; that fragment holds count 3 and the values
  2521, 131,141 and 141,186, which are the i-nodes of a symbolic link
  and two files called `3dmiX`. The key `index.html` has the value
  0x800000000001C400: a duplicate node at 115,712 with LEFT −1, RIGHT
  −1, count 9. That index has 2,093 plain values, 206 fragment links
  and 4 duplicate node links.
- CONFIRMED (crafted): three files with one name give a fragment link
  with count 3; twelve give a duplicate node with count 12.
- The book says a fragment "can only hold eight items"; it holds a
  count and seven values.
- DECISION D14: in a directory or attribute directory, a value whose
  top two bits are not 00 is corrupt. The entry is skipped; the fragment
  or duplicate node is not read.

### 8.7 Free nodes

When a node becomes empty the writer unlinks it and puts it on a free
list: the header's FREE is the offset of the first free node, each free
node's LEFT is the offset of the next one (−1 at the end), and its
OVERFLOW is −2. The other fields of a free node are stale. CONFIRMED
(BeOS): the name index of R5-x86 has five free nodes; CONFIRMED
(crafted): a directory from which 300 of 1,500 entries were removed has
28. A reader never follows FREE. Reaching a node whose OVERFLOW is −2
while descending is corruption.

### 8.8 Enumerating a tree

DECISION D15: a tree is enumerated by descending from the root, never
by following LEFT and RIGHT. Sibling links are redundant information
that a damaged or hostile volume can turn into a loop or a detour, and
a leaf reached only through them would not have been validated as
belonging to this tree.

1. Check the header (section 8.1).
2. Visit the node at ROOT at depth 1.
3. To visit a node: check that its offset is a valid node offset and
   that it has not been visited before in this enumeration; read it and
   check it (section 8.2).
   - If OVERFLOW = −1 it is a leaf: deliver its keys and values in
     order.
   - Otherwise it is an interior node: visit the children VALUES[0] to
     VALUES[*n* − 1] in order, then OVERFLOW, each at depth + 1.
4. The enumeration ends when the visit of the root returns.

- On a consistent tree this delivers every key once, in ascending
  order, and visits exactly the nodes that following RIGHT from the
  first leaf would visit. CONFIRMED on every container of every volume
  examined, including the equality with the sibling chain.
- Every node is visited at most once, so the work is bounded by the
  number of nodes in the file (SIZE / 1024). A second visit to a node
  means two parents claim it or there is a cycle: the tree is corrupt.
- DECISION D16: the depth is limited to 16 levels (three is the deepest
  tree seen; a tree with fan-out as low as 8 would exceed any real
  volume before reaching 16). Exceeding it is corruption. The descent
  must not consume native call stack in proportion to anything an
  attacker controls beyond this limit.
- DECISION D17: when a node is corrupt (invalid offset, failed check,
  second visit, depth exceeded, read beyond the stream), the subtree
  below it is abandoned and the enumeration **continues** with the next
  child of the parent. Entries already delivered stay delivered. The
  container is reported as damaged if the implementation has a way to
  report it.
- The MAX_LEVELS field is not used.

### 8.9 Sizes

- A key is at most 256 bytes (REFERENCE-ONLY); a file name is at most
  255 bytes.
- A leaf holds at most 90 keys (one byte each); with typical names of
  10 to 20 bytes, 30 to 40. Leaves of R5-x86's `beos/bin` hold 30 to
  37 keys.
- The number of entries of a container is bounded by its node count
  times 90.

## 9. Directories

### 9.1 Structure

- A directory is an i-node of kind Directory (section 5.3) whose stream
  is a B+tree file with string keys (DATA_TYPE 0). Each leaf entry maps
  a name to the i-node number of a file, directory, symbolic link or
  special node.
- Every directory has the entries `.` (value: the directory's own
  i-node number) and `..` (value: the i-node number of the parent; in
  the root directory, of the root itself). CONFIRMED (BeOS and crafted)
  for all 839 directories; `..` always equals the directory's PARENT
  field.
- An empty directory has exactly those two entries.
- The root directory is named by the superblock's ROOT_DIR. Its PARENT
  is itself.

### 9.2 Names

- A name is the key: 1 to 255 bytes, UTF-8 by convention, any byte
  except zero and `/` (0x2F). BeOS and Haiku do not restrict names
  further: spaces, colons, backslashes, `<`, `>` and control characters
  occur in real volumes (R3-PPC has a file named
  `<Martijn>,<tigrfhur@xs4all.nl>:1`).
- DECISION D18: the reader skips the entries `.` and `..` wherever they
  occur, without looking at their values. It skips, as corrupt, an
  entry whose key is empty, longer than 255 bytes, or contains a zero
  byte or `/`. Making names safe for the host file system (invalid
  UTF-8, reserved characters, names such as `..` produced by
  sanitising) is the business of this repository's common path
  handling, which must be applied to every component.

### 9.3 What is listed

DECISION D19, following what this repository does for other Unix-like
file systems:

| I-node kind | Listed |
|:--|:--|
| Regular file | Yes, as an entry with its stream as contents, the path from the root as name, and the Unix mode string. |
| Directory | Traversed; its files are listed under its path. |
| Symbolic link | No (section 10.1). |
| Special node | No (section 10.3). |
| Attribute directory, attribute, index directory, index | No. They are not reachable from the root directory; if a directory entry names one, the entry is skipped as corrupt. |

Entries are delivered in tree order within a directory. Paths use `/`
between components and do not include the volume name.

### 9.4 Walking the hierarchy

1. Start at the root directory's i-node.
2. Enumerate the directory's tree (section 8.8). For each entry that
   section 9.2 does not skip, the value must have its top two bits clear
   (section 8.6), be at least 1 and less than NUM_BLOCKS, and name a
   block within L. Read the i-node there and check it (section 5.5).
3. Act on its kind (section 9.3).

- DECISION D20: an entry whose value or i-node is invalid is skipped,
  and the walk continues with the next entry. One bad entry does not
  hide the rest of the volume.
- DECISION D21: a directory i-node is traversed at most once per
  volume. BFS has no hard links (section 10.2), so on a consistent
  volume no i-node is entered twice; a second entry naming an
  already-traversed directory is a cycle or cross-link and is skipped.
  The set of traversed directory i-node numbers must be kept for the
  whole walk, not per path.
- DECISION D22: directory nesting is limited to 128 levels; deeper
  directories are skipped. The deepest path on the BeOS volumes has 10
  components. The walk must not recurse on the native stack without
  this bound.
- The reader does not use the PARENT field and does not require it to
  match (it matched on every i-node examined).
- The reader does not require the child's name item to match the key.

## 10. Other objects

### 10.1 Symbolic links

A symbolic link is an i-node with file type 0xA000. MODE is 0o120777 on
every link examined. The target is a path, absolute or relative, in
UTF-8.

- **Short link** (flag LONG_SYMLINK clear): the target is stored in the
  144 bytes at i-node offset 72, where other i-nodes have the data
  stream, terminated by a zero byte and padded with zeros. At most 143
  bytes. The data stream fields do not exist; SIZE (offset 208) reads as
  whatever the text put there, normally 0. CONFIRMED (BeOS): 364 links,
  the longest target 52 bytes; section 13.5.
- **Long link** (flag LONG_SYMLINK set): the i-node has a normal data
  stream, SIZE is the length of the target, and the target is the first
  SIZE bytes of the stream, without a terminator within SIZE.
  CONFIRMED (crafted): a 164-byte target gives flags 0x00040049, SIZE
  164, one direct run of one block; the byte after the target in the
  block is zero. No BeOS-written long link was available; the flag is
  in the Linux driver's list of BeOS flags.
- A reader that wanted the target would take, for a short link, the
  bytes up to the first zero within the 144 (DECISION D23: at most 143;
  if there is no zero byte the link is corrupt), and for a long link
  the stream, cut at the first zero byte if one occurs.
- Links may dangle and often point outside the volume (`/boot/…`).
- DECISION D19 applies: symbolic links are not listed and never
  followed. The reader must take care not to read a short link's 144
  bytes as block runs.

### 10.2 Hard links

BFS has none. A directory entry is the only reference to an i-node, the
i-node stores a single PARENT and a single name, and there is no link
count field. BOOK: hard links were deliberately left out. CONFIRMED
(crafted): the reference tool's `ln` without `-s` fails with "not
supported". Two directory entries with the same value are therefore
corruption, not a link; D21 covers directories, and for files the
output budget of section 17 bounds the damage.

### 10.3 Special nodes

The mode's type field can in principle hold FIFO, device or socket
types. None occurs on any volume examined; BeOS keeps devices in a
separate virtual file system. They have no contents and are not listed.

### 10.4 Indices and queries

- The superblock's INDICES names the **index directory**: an i-node with
  S_INDEX_DIR set whose tree maps index names to the i-node numbers of
  **indices**. An index is an i-node with S_INDEX_DIR and one of the key
  type bits set, whose tree maps attribute values to the i-node numbers
  of the files having them, with duplicates (section 8.6).
- CONFIRMED (BeOS): R5-x86 has the indices `BEOS:APP_SIG`,
  `last_modified`, `name` and `size`; Haiku creates the same four.
- A volume can be initialised without indices; INDICES is then a zero
  run (REFERENCE-ONLY).
- Queries are evaluated at run time against the indices and leave
  nothing on disk.
- Indices are redundant: every file they mention is also reachable from
  the root. A reader ignores INDICES entirely.

### 10.5 Deleted files and the Trash

- Deleting a file removes its directory entry and frees its blocks in
  the bitmap. The i-node block is not erased, so stale i-nodes with a
  valid magic number exist in free space. A reader that only follows
  directory entries never sees them. Undeleting is out of scope.
- The DELETED flag marks an i-node that was removed while still open.
  A directory entry naming such an i-node cannot exist on a consistent
  volume; check 2 of section 5.5 rejects it.
- The Trash of the desktop is an ordinary directory (`home/Desktop/Trash`
  on R5-x86, `desktop/Trash` on AA-PPC; INFERRED for Haiku: a directory named `trash` in the root
  of each volume). Its contents are ordinary files and are listed like any
  others (DECISION D24).

## 11. Differences between writers and media

| Aspect | BeOS PR to R5 | Haiku |
|:--|:--|:--|
| Byte order | Native: little-endian on Intel, big-endian on PowerPC | Little-endian (the big-endian build is not the default; not examined) |
| Superblock position | 512 on Intel, 0 on PowerPC | 512 |
| Block sizes | 1024, 2048, 4096, 8192 (BOOK); 2048 on every BeOS volume examined | 1024, 2048 (default), 4096, 8192 |
| Bytes 164 to 511 after the superblock | Boot code or leftovers | Zero |
| I-node ETC | Garbage | 0 |
| I-node STATUS_CHANGE_TIME | 0 | A time |
| I-node flags above 0xFFFF | 0x00020000 on containers and attributes | 0x00040000 on most i-nodes |
| LOGGED flag on directories | Set | Not set |
| Low 16 bits of times | Counter | Sub-second time and counter |
| Small-data terminator | Free-space item spanning the rest | Eight zero bytes |
| Indirect array | 4096 bytes | 4 blocks |
| Double-indirect arrays and runs | 4 blocks per data run (BOOK) | 4096 / B blocks, at least 1 |
| Directory MODE | Usually with S_STR_INDEX, sometimes without | With S_STR_INDEX |

- None of these differences changes the reading rules of this document;
  they are listed so that an implementer does not turn an accident of
  one writer into a check.
- Volumes written by Haiku are meant to be readable by BeOS R5 and the
  other way round; there is one on-disk format and no version field.
- Dano and Zeta: INFERRED to be identical on disk to R5 (same magic
  numbers, mounted by the same reference implementation without special
  cases; the reference implementation's source mentions no difference).
  Not examined.
- BeOS PR and R3: R3-PPC is fully consistent with this document. PR is
  INFERRED to be the same; the book describes the R3/R4 format.
- Big-endian volumes are read with the same rules and the other byte
  order (section 3.2). The reference implementation mounts volumes of
  the non-native order read-only; the Linux driver reads both.
- CD and partition carriers: section 2.

## 12. What is not BFS

- A volume is BFS only if section 3.4 passes. In particular the bytes
  `BFS1`/`1SFB` at offset 32 or 544 alone prove nothing.
- The file system of BeOS DR1 to DR8 (the "old file system", used
  before the Preview Release) has no BFS superblock and is not readable
  with this document. The BeOS 1.1d7.1 BeBox CD begins with the bytes
  `00 03 00 00 31 AD D0 93 00 00 00 01 00 00 01 02`, has no Apple
  Partition Map, and has neither BFS magic number at offset 32 or 544.
  Such images simply fail detection.
- The BeOS boot floppy (`Floppy.img` of BeOS 5 Personal Edition) is not
  a BFS volume either: it has boot code at byte 512 and no superblock.
- The UnixWare/SCO "bfs" boot file system is unrelated.

## 13. Worked examples

All bytes are in disk order.

### 13.1 Little-endian superblock (R5-x86, volume bytes 512 to 643)

```
50 65 72 73 6F 6E 61 6C 20 45 64 69 74 69 6F 6E   NAME "Personal Edition"
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
31 53 46 42                                       MAGIC1 0x42465331, little-endian
45 47 49 42                                       BYTE_ORDER 0x42494745
00 08 00 00                                       BLOCK_SIZE 2048
0B 00 00 00                                       BLOCK_SHIFT 11
00 E8 03 00 00 00 00 00                           NUM_BLOCKS 256,000
FA CD 00 00 00 00 00 00                           USED_BLOCKS 52,730
00 08 00 00                                       INODE_SIZE 2048
31 10 12 DD                                       MAGIC2 0xDD121031
01 00 00 00                                       BLOCKS_PER_AG 1
0E 00 00 00                                       AG_SHIFT 14
10 00 00 00                                       NUM_AGS 16
4E 45 4C 43                                       FLAGS 0x434C454E "CLEN"
00 00 00 00 11 00 00 08                           LOG_BLOCKS (0, 17, 2048)
72 04 00 00 00 00 00 00                           LOG_START 1138
72 04 00 00 00 00 00 00                           LOG_END 1138
0E 83 B6 15                                       MAGIC3 0x15B6830E
08 00 00 00 00 00 01 00                           ROOT_DIR (8, 0, 1)
00 00 00 00 11 08 01 00                           INDICES (0, 2065, 1)
```

- Check 7: ⌈256,000 / 16,384⌉ = 16.
- Root directory i-node: block 8 · 16,384 + 0 = 131,072, volume byte
  268,435,456 (0x10000000).
- Index directory i-node: block 2065.
- The bytes at volume offset 676 onwards are boot code (`52 8D 77 18…`).

### 13.2 Big-endian superblock (R3-PPC, volume bytes 0 to 131)

```
42 65 4F 53 20 50 50 43 20 52 33 00 …             NAME "BeOS PPC R3"
42 46 53 31                                       MAGIC1, big-endian
42 49 47 45                                       BYTE_ORDER 0x42494745
00 00 08 00                                       BLOCK_SIZE 2048
00 00 00 0B                                       BLOCK_SHIFT 11
00 00 00 00 00 03 8F EA                           NUM_BLOCKS 233,450
00 00 00 00 00 02 BE 42                           USED_BLOCKS 179,778
00 00 08 00                                       INODE_SIZE 2048
DD 12 10 31                                       MAGIC2
00 00 00 01                                       BLOCKS_PER_AG 1
00 00 00 0E                                       AG_SHIFT 14
00 00 00 0F                                       NUM_AGS 15
43 4C 45 4E                                       FLAGS "CLEN"
00 00 00 00 00 10 08 00                           LOG_BLOCKS (0, 16, 2048)
00 00 00 00 00 00 06 AE                           LOG_START 1710
00 00 00 00 00 00 06 AE                           LOG_END 1710
15 B6 83 0E                                       MAGIC3
00 00 00 08 00 00 00 01                           ROOT_DIR (8, 0, 1)
00 00 00 00 08 10 00 01                           INDICES (0, 2064, 1)
```

The 164 bytes at volume offset 512 are zero. An i-node of this volume
(block 16,416 = 1 · 16,384 + 32) begins `3B BE 0A D9 00 00 00 01 00 20
00 01`: MAGIC, then INODE_NUM (1, 32, 1).

### 13.3 A directory i-node (R5-x86 root, block 131,072)

```
offset   0: D9 0A BE 3B                 MAGIC
offset   4: 08 00 00 00 00 00 01 00     INODE_NUM (8, 0, 1) = this block
offset  12: 00 00 00 00                 UID 0
offset  16: 00 00 00 00                 GID 0
offset  20: ED 41 00 01                 MODE 0x010041ED = 0o100040755
offset  24: 09 00 00 00                 FLAGS IN_USE, LOGGED
offset  28: 00 00 37 50 DE 38 00 00     CREATE_TIME, seconds 0x38DE5037
offset  36: ED 44 71 51 DE 38 00 00     MODIFIED_TIME, seconds 0x38DE5171
offset  44: 08 00 00 00 00 00 01 00     PARENT (8, 0, 1) = itself
offset  52: 00 00 00 00 00 00 00 00     ATTRIBUTES none
offset  60: 00 00 00 00                 TYPE 0
offset  64: 00 08 00 00                 INODE_SIZE 2048
offset  68: B0 1F 7A 00                 ETC (garbage)
offset  72: 08 00 00 00 01 00 01 00     DIRECT[0] (8, 1, 1) = block 131,073
offset  80: 88 zero bytes               DIRECT[1] to DIRECT[11]
offset 168: 00 08 00 00 00 00 00 00     MAX_DIRECT_RANGE 2048
offset 176: 32 zero bytes               INDIRECT, MAX_INDIRECT_RANGE,
                                        DOUBLE_INDIRECT, MAX_DOUBLE_…
offset 208: 00 08 00 00 00 00 00 00     SIZE 2048
offset 216: 16 zero bytes
offset 232: 47 4C 4C 55                 item TYPE 0x554C4C47 "ULLG"
offset 236: 0C 00                       NAME_SIZE 12
offset 238: 08 00                       DATA_SIZE 8
offset 240: 62 65 3A 76 6F 6C 75 6D 65 5F 69 64      "be:volume_id"
offset 252: 00 00 00
offset 255: 9B BA 74 1B A5 03 BE 63     value
offset 263: 00
offset 264: 54 43 45 52 0E 00 10 00 …   next item: "RECT", name 14 bytes,
                                        data 16 bytes ("_trk/windframe")
```

The item at 232 has size 12 + 8 + 12 = 32, so the next is at 264. Five
more items follow; the list ends with a free-space item at offset 771:
`00 00 00 00 00 00 F1 04` (NAME_SIZE 0, DATA_SIZE 1265 = 2048 − 771 −
12).

### 13.4 A one-level directory tree (the stream of 13.3)

Header, stream offset 0 (volume block 131,073):

```
E8 C2 F6 69               MAGIC 0x69F6C2E8
00 04 00 00               NODE_SIZE 1024
01 00 00 00               MAX_LEVELS 1
00 00 00 00               DATA_TYPE 0 (string)
00 04 00 00 00 00 00 00   ROOT 1024
FF FF FF FF FF FF FF FF   FREE −1
00 08 00 00 00 00 00 00   MAX_SIZE 2048
```

Root node, stream offset 1024:

```
offset  0: FF × 8                      LEFT −1
offset  8: FF × 8                      RIGHT −1
offset 16: FF × 8                      OVERFLOW −1: a leaf
offset 24: 08 00                       KEY_COUNT 8
offset 26: 22 00                       KEYS_LENGTH 34
offset 28: 2E 2E 2E 61 70 70 73 62 65 6F 73 64 65 6D 6F 73 68 6F 6D 65
           70 72 65 66 65 72 65 6E 63 65 73 76 61 72
                                       "." ".." "apps" "beos" "demos"
                                       "home" "preferences" "var"
offset 62: 00 00                       padding: 28 + 34 = 62, rounded to 64
offset 64: 01 00 03 00 07 00 0B 00 10 00 14 00 1F 00 22 00
                                       KEY_INDEX 1, 3, 7, 11, 16, 20, 31, 34
offset 80: 00 00 02 00 00 00 00 00     "."           → 131,072
           00 00 02 00 00 00 00 00     ".."          → 131,072
           58 09 00 00 00 00 00 00     "apps"        → 2392
           F3 09 00 00 00 00 00 00     "beos"        → 2547
           E3 21 00 00 00 00 00 00     "demos"       → 8675
           56 09 00 00 00 00 00 00     "home"        → 2390
           04 35 00 00 00 00 00 00     "preferences" → 13,572
           5C 35 00 00 00 00 00 00     "var"         → 13,660
```

The reader skips `.` and `..` and reads the i-nodes at blocks 2392,
2547, 8675, 2390, 13,572 and 13,660, all directories.

### 13.5 A short symbolic link (R5-x86, block 2521, entry `apps/3dmiX`)

```
offset  0: D9 0A BE 3B                 MAGIC
offset  4: 00 00 00 00 D9 09 01 00     INODE_NUM (0, 2521, 1)
offset 20: FF A1 00 00                 MODE 0o120777
offset 24: 01 00 00 00                 FLAGS IN_USE; LONG_SYMLINK clear
offset 44: 00 00 00 00 58 09 01 00     PARENT (0, 2392, 1)
offset 72: 2F 62 6F 6F 74 2F 62 65 6F 73 2F 61 70 70 73 2F 33 64 6D 69 58
           00 …                        "/boot/beos/apps/3dmiX", zero-padded to 144
offset 232: 52 54 53 43 01 00 05 00 13 00 00 00 33 64 6D 69 58 00
                                       name item: "CSTR", NAME_SIZE 1,
                                       DATA_SIZE 5, name 0x13, "3dmiX"
```

Read as block runs, the text would give DIRECT[0] = (1,869,570,607,
12,148, 25,954): a reader must test the kind before touching the
stream.

### 13.6 A two-level directory tree (R5-x86, `beos/bin`, block 131,425)

- I-node: one direct run (8, 354, 5), SIZE 10,240. Header: MAX_LEVELS
  2, ROOT 3072, FREE −1, MAX_SIZE 10,240.
- Root node at 3072: LEFT −1, RIGHT −1, OVERFLOW 1024, 7 keys,
  KEYS_LENGTH 43:

  | Key | Value (child offset) | Child's first key | Child's last key | Child's keys |
  |:--|--:|:--|:--|--:|
  | `craw` | 2048 | `.` | `craw` | 33 |
  | `factor` | 4096 | `csplit` | `factor` | 32 |
  | `iroster` | 5120 | `false` | `iroster` | 32 |
  | `mcd` | 6144 | `isvolume` | `mcd` | 30 |
  | `paste` | 7168 | `mcopy` | `paste` | 32 |
  | `serial_route` | 8192 | `patch` | `serial_route` | 32 |
  | `unchop` | 9216 | `setmime` | `unchop` | 33 |
  | (OVERFLOW) | 1024 | `unexpand` | `znew` | 37 |

- Leaf chain: 2048 → 4096 → 5120 → 6144 → 7168 → 8192 → 9216 → 1024,
  the same order as the descent.
- The enumeration delivers 261 keys; 259 remain after `.` and `..`.

### 13.7 A data stream with all three ranges (crafted, B = 1024)

File `big.bin`, i-node block 517, SIZE 2,268,407.

| Field | Value |
|:--|:--|
| DIRECT | (0, 8061, 19), then eleven runs of 4 blocks: (0, 789, 4), (0, 797, 4), … (0, 869, 4) |
| MAX_DIRECT_RANGE | 64,512 = 63 · 1024 |
| INDIRECT | (0, 885, 4): 4096 bytes, 512 entries, all used (no terminator); first entry (0, 877, 1), last (0, 5005, 4); 2,045 blocks in total |
| MAX_INDIRECT_RANGE | 2,158,592 = 64,512 + 2,045 · 1024 |
| DOUBLE_INDIRECT | (0, 5021, 4): bytes `00 00 00 00 A5 13 04 00` then zeros: one entry, (0, 5029, 4) |
| the indirect array at (0, 5029, 4) | 27 entries, each of 4 blocks, first (0, 5013, 4), then a zero run |
| MAX_DOUBLE_INDIRECT_RANGE | 2,269,184 = 2,158,592 + 27 · 4 · 1024 |

- The stream is 63 + 2,045 + 108 = 2,216 blocks; SIZE needs 2,216
  blocks (2,215 full blocks and 247 bytes of the last).
- Stream offset 64,512 is the first byte of block 877; stream offset
  2,158,592 is the first byte of block 5013.
- The SHA-256 of the 2,268,407 bytes read this way equals that of the
  source file.

### 13.8 A long symbolic link (crafted, B = 1024, block 2331)

```
offset  20: FF A1 00 00                MODE 0o120777
offset  24: 49 00 04 00                FLAGS 0x00040049: IN_USE, LOGGED,
                                       LONG_SYMLINK, and in-memory bit 0x40000
offset  72: 00 00 00 00 1C 09 01 00    DIRECT[0] (0, 2332, 1)
offset 168: 00 04 00 00 00 00 00 00    MAX_DIRECT_RANGE 1024
offset 208: A4 00 00 00 00 00 00 00    SIZE 164
```

Block 2332 holds the 164 bytes of the target followed by zeros.

### 13.9 An index i-node, for recognition only (R5-x86)

- Index directory, block 2065: MODE bytes `C0 41 00 21` = 0x210041C0 =
  0o4100040700; PARENT itself; SIZE 2048; one direct run of 64 blocks.
  Its tree has the keys `BEOS:APP_SIG` → 2325, `last_modified` → 2260,
  `name` → 2130, `size` → 2195 and no `.` or `..`.
- Index `size`, block 2195: MODE 0x20204000 = 0o4010040000; tree
  DATA_TYPE 3.

## 14. Test vectors

The byte-level examples of section 13 are the primary vectors for the
parsers. The volumes below exercise the reader as a whole. They were
built with `bfs_shell` of the reference implementation and are not in
the repository; whoever builds the committed fixtures recreates
equivalent, smaller ones and records the exact commands next to them.

The tool is given an image file of zeros and is run once with
`--initialize`, the image, a volume name and the parameter
`block_size N`, then again with the image and a list of its commands
(`mkdir`, `cp` with a host path prefixed by a colon, `touch`, `ln -s`,
`rm`, `sync`, `quit`) on standard input. Inside the tool the volume is
mounted at `/myfs`.

### 14.1 Volumes `h1024`, `h2048`, `h4096`, `h8192` (32 MiB each)

Contents, in creation order: a directory `dir`; `small.txt` (10 bytes,
`hello bfs` and a line feed); an empty file `empty`; a symbolic link
`short-link` to `small.txt`; a symbolic link `long-link` with a
164-byte target; a directory `frag` with 1,400 files of 1,000 bytes,
`f0000` to `f1399`, of which the even-numbered ones are then removed;
`big.bin` (3,000,000 bytes); `dir/mid.bin` (200,000 bytes); a directory
`many` with 1,500 empty files named `entry-number-NNNNN-with-a-longish-name`
(NNNNN from 00000 to 01499), of which 00100 to 00399 are then removed;
a directory `dups` with subdirectories `a` to `l`, each holding an empty
file `same-name`, and `a`, `b`, `c` also an empty file `few`.

Expected, identical for all four block sizes:

| Property | Value |
|:--|:--|
| Directories below the root | 16 (`dir`, `frag`, `many`, `dups`, `dups/a` … `dups/l`) |
| Regular files | 1,919, of which 1,216 are empty |
| Symbolic links (not listed) | 2 |
| `frag` | 700 files, odd numbers only; tree depth 2 |
| `many` | 1,200 files; tree depth 3, 120 live nodes (1 root, 9 interior, 110 leaves at B = 1024), 28 free nodes, FREE ≠ −1; its stream has an indirect range at B = 1024 |
| Superblock at B = 1024 | 32,768 blocks, AG_SHIFT 13, 4 groups, log (0, 5, 2048), root (0, 2053, 1) |
| Superblock at B = 8192 | 4,096 blocks, AG_SHIFT 16, 1 group, log (0, 2, 512), root (0, 514, 1) |
| Name index | `few` has a duplicate fragment (count 3), `same-name` a duplicate node (count 12) |

### 14.2 Volumes `frag1024`, `frag2048`, `frag4096`

Built to force a fragmented file: a directory `frag` is filled with 2·H
files of 3 blocks each; the remaining free space is filled with one
file `filler.bin`; the even-numbered files of `frag` are removed,
leaving H holes of 4 blocks (i-node plus 3 data blocks); then `big.bin`
is copied in and has to use the holes.

| Volume | B | H | Volume blocks | Size of `big.bin` | Ranges used | DOUBLE_INDIRECT length | Double-indirect data runs |
|:--|--:|--:|--:|--:|:--|--:|:--|
| `frag1024` | 1024 | 560 | 8,080 | 2,268,407 | direct, indirect (512 of 512 entries), double-indirect | 4 | 27 runs of 4 blocks |
| `frag2048` | 2048 | 1,300 | 14,200 | 10,484,983 | direct, indirect (1,024 of 1,024), double-indirect | 2 | 480 runs of 2 blocks |
| `frag4096` | 4096 | 2,400 | 23,000 | 38,993,143 | direct, indirect (2,048 of 2,048), double-indirect | 1 | 1,264 runs of 1 block in three indirect arrays (512, 512, 240) |

In all three the bytes of `big.bin` read according to section 6.6 have
the SHA-256 of the source file. The source contents were pseudo-random;
any contents whose every block differs will do (a fixture should use
contents that compress well, for example a counter in every 8 bytes).

Note for whoever rebuilds these: at B = 1024 the volume must fit in one
allocation group (fewer than 8,192 blocks), otherwise the reference
tool's allocator reports a full disk before the file reaches the
double-indirect range.

### 14.3 Negative vectors

Derived from any good volume by changing bytes; each must be handled as
stated without a crash, hang or unbounded allocation.

| Change | Expected |
|:--|:--|
| Any one of MAGIC1, MAGIC2, MAGIC3, BYTE_ORDER altered | Not detected as BFS. |
| BLOCK_SHIFT altered, or INODE_SIZE altered, or NUM_AGS altered | Not detected. |
| Root directory i-node's MAGIC altered | Not detected (check 10). |
| A file's directory value changed to the block of a B+tree node | Entry skipped (i-node magic). |
| A file i-node's INODE_NUM altered | Entry skipped (D6). |
| A file's SIZE increased beyond its runs | That entry fails; others unaffected. |
| A direct run's GROUP set to NUM_AGS, or its LEN set to 0 with later runs present | LEN 0 with GROUP and START zero ends the list (zero run) and the file fails for missing bytes; an invalid run makes the entry fail. |
| A leaf's RIGHT pointed at itself or at an earlier leaf | No effect (D15). |
| An interior node's VALUES[0] pointed at the root | Subtree abandoned as second visit; other children still enumerated (D17). |
| An interior node's OVERFLOW set to an offset beyond SIZE | That child abandoned; others enumerated. |
| KEY_COUNT of a leaf set to 0xFFFF | Node corrupt (check 1 of section 8.2). |
| A KEY_INDEX entry decreased below its predecessor | Node corrupt. |
| A directory value's top bits set to 11 or 10 | Entry skipped (D14). |
| A subdirectory's entry pointed back at an ancestor directory | Skipped as already traversed (D21). |
| ROOT_DIR's tree header NODE_SIZE set to 512 | Root directory yields no entries; the volume opens (it passed detection) but is empty. |
| Image truncated in the middle of a file's data | Volume opens (D3); that file and later-placed files fail individually. |
| LOG_END altered so that the log is not empty, or FLAGS set to `DIRT` | Volume is read as before (D4). |

## 15. Observed implementation gaps

Nothing in this section is to be imitated.

- **Reference implementation, run validation**: accepts GROUP equal to
  NUM_AGS and START equal to 2^AG_SHIFT (two off-by-one comparisons),
  and never compares a run with NUM_BLOCKS. Section 4.2 is stricter.
- **Reference implementation, i-node validation**: does not compare
  INODE_NUM with the block the i-node was read from, and carries a note
  that the data stream is not validated at all.
- **Reference implementation, B+tree**: accepts node offsets that are
  not multiples of the node size (only the root is tested for that);
  its iterator follows RIGHT links from leaf to leaf with no protection
  against a link that points backwards, so a crafted volume makes a
  directory listing endless; its descent to the first leaf stops only
  when a child offset equals its own node's offset, so a longer cycle
  among interior nodes is endless as well. Its duplicate-count reader
  takes an 8-bit count from a 64-bit field without a range check.
- **Reference implementation, double-indirect range**: computes
  positions by division using the LEN of DOUBLE_INDIRECT and then
  rejects a data run of any other length. If BeOS really writes 4-block
  runs under a 2-block array at B = 2048 (unknown, section 18), such
  files would be unreadable to it. The sequential walk of section 6.6
  has no such dependency.
- **Linux driver, superblock**: reports an error when BLOCKS_PER_AG
  differs from 2^AG_SHIFT, which is the case on every volume (the field
  counts bitmap blocks); the check is non-fatal there and must not be
  copied. It refuses unclean volumes outright.
- **The book**: says the key length index starts at a multiple of four
  (it is eight, section 8.2); describes the small-data item without the
  three and one zero bytes (section 7.1); says a duplicate fragment
  holds eight values (seven and a count, section 8.6); lists the i-node
  flag 0x00010000 and omits LONG_SYMLINK.

## 16. Decisions

| # | Decision | Section |
|:--|:--|:--|
| D1 | Probe the superblock at 512, then at 0; accept either byte order at either position. | 3.1 |
| D2 | Detection additionally requires a valid FLAGS word, valid ROOT_DIR and LOG_BLOCKS runs, and a valid directory i-node at ROOT_DIR. | 3.4 |
| D3 | The volume's claimed size is not compared with the available bytes; each read is. | 3.4 |
| D4 | The log is never replayed; unclean volumes are read as they are. | 3.6 |
| D5 | Runs are also checked against NUM_BLOCKS, block 0 and the available bytes. | 4.2 |
| D6 | An i-node must name its own block in INODE_NUM. | 5.5 |
| D7 | Streams are read by a sequential walk of the runs; no division, no use of the MAX fields, no fixed-size assumption. | 6.6 |
| D8 | A stream whose runs do not cover SIZE makes the entry fail; no zero filling. | 6.6 |
| D9 | A damaged small-data area ends the attribute list and nothing else. | 7.2 |
| D10 | Names come from directory keys, not from the name item. | 7.3 |
| D11 | Only B+trees with a node size of 1024 are read. | 8.1 |
| D12 | Node offsets must be multiples of 1024 within both SIZE and MAX_SIZE. | 8.1 |
| D13 | Key order is not verified. | 8.5 |
| D14 | Duplicate links in a directory are corrupt entries and are not followed. | 8.6 |
| D15 | Trees are enumerated by descent; sibling links are ignored. | 8.8 |
| D16 | Tree depth is limited to 16. | 8.8 |
| D17 | A corrupt node loses its subtree only. | 8.8 |
| D18 | `.` and `..` are skipped; empty names and names with a zero byte or `/` are corrupt entries. | 9.2 |
| D19 | Only regular files are listed; directories are traversed; symbolic links, special nodes, attributes and indices are not listed. | 9.3 |
| D20 | An invalid entry is skipped and the walk continues. | 9.4 |
| D21 | Each directory i-node is traversed at most once per volume. | 9.4 |
| D22 | Directory nesting is limited to 128 levels. | 9.4 |
| D23 | A short symbolic link without a zero byte in its 144 bytes is corrupt. | 10.1 |
| D24 | The Trash directory is listed like any other. | 10.5 |

## 17. Safety requirements

A BFS volume is untrusted input. Every number in it is attacker
controlled, including the ones that passed detection.

1. **Bounds before reads.** Every read of a block, i-node, array of
   runs or node is preceded by a check that the bytes lie within L. All
   offset arithmetic (block number times B, run start plus length,
   stream offset plus node size) is done in 64 bits with overflow
   treated as failure, before any narrowing to a native index.
2. **Runs.** Every run is validated (section 4.2) before use, including
   INDIRECT, DOUBLE_INDIRECT and each entry of the double-indirect
   array. A run of length 0 that is not a zero run is invalid.
3. **Run lists are finite by construction.** The direct list has at
   most 12 entries; an indirect array at most 65,535 · B / 8 entries; a
   double-indirect array the same number of indirect arrays. That
   product is about 4 · 10^15 at B = 8192, so the construction alone is
   not a sufficient bound: the walk must stop as soon as SIZE bytes are
   covered (section 6.6), and the output budget below bounds SIZE.
4. **Do not materialise run lists blindly.** The number of runs to be
   remembered for a stream is bounded by the number of blocks needed
   for SIZE (each run covers at least one block), and SIZE is bounded
   by requirement 6. An implementation must not allocate space for the
   declared capacity of an indirect array (LEN · B / 8 entries) before
   reading it, and must read arrays block by block.
5. **Overlapping and repeated runs.** Nothing prevents a crafted volume
   from listing the same blocks many times in one file, or the same
   blocks in many files, or blocks that belong to the bitmap, the log
   or an i-node. Reading them is harmless (the reader only reads); the
   danger is amplification: a small image declaring huge or many
   outputs. Therefore:
6. **Output budget.** Every file delivered is charged against a budget
   derived from L with this repository's standard mechanism
   (`OutputBudget::for_input` and `charge`, as the other file system
   readers do), before its contents are produced. In addition a single
   stream is rejected outright when SIZE exceeds NUM_BLOCKS · B or
   exceeds L (a file cannot be larger than its volume). Buffers are
   sized with the repository's checked helpers, never directly from
   SIZE.
7. **Container streams.** A container's stream is bounded the same way
   before its tree is read: SIZE at most L and at most NUM_BLOCKS · B.
   Tree nodes are read one at a time; the whole stream is not copied
   into memory on the strength of SIZE alone.
8. **Tree cycles.** Enumeration is by descent with a visited set over
   node offsets (at most SIZE / 1024 entries), a depth limit (D16) and
   no use of sibling links (D15). Free nodes (OVERFLOW = −2) are never
   entered. Duplicate links are never followed in directories (D14).
9. **Node contents.** KEY_COUNT, KEYS_LENGTH and every KEY_INDEX entry
   are checked against the node size before any key or value is
   touched (section 8.2). A key is never read past KEYS_LENGTH.
10. **Directory cycles.** A visited set over directory i-node numbers
    for the whole walk (D21) and a nesting limit (D22). `.` and `..`
    are skipped by name, never followed.
11. **Entry counts.** The number of entries delivered by one container
    is bounded by 90 · SIZE / 1024. The number of i-nodes read during
    the whole walk is bounded by the number of directory entries; an
    implementation should additionally stop after reading more i-nodes
    than NUM_BLOCKS, which cannot happen on a consistent volume.
12. **Names.** Keys are arbitrary bytes. Each path component goes
    through the repository's path sanitising; `.` and `..` components
    and components containing `/` or zero bytes never reach the output
    path (D18). Paths are bounded in length by the nesting limit times
    255 bytes.
13. **Kinds.** The kind of an i-node is decided from MODE before any
    other field is interpreted (section 5.3). In particular the 144
    bytes at offset 72 of a short symbolic link are text, and the
    stream of an attribute, attribute directory or index is never read
    by a reader that does not surface them.
14. **Small data.** If the small-data area is parsed at all, each item
    is bounds-checked against INODE_SIZE before use (section 7.2), and
    the walk ends at the first violation.
15. **Failure is local.** A corrupt file fails alone; a corrupt node
    loses its subtree; a corrupt directory loses its entries. Only a
    failed superblock check or a failed root i-node makes the volume
    unreadable. No input may cause a panic, an out-of-bounds access, an
    unbounded loop or an allocation proportional to a declared (rather
    than available) size.

## 18. Open points

| # | Point | Effect on the reader |
|--:|:--|:--|
| 1 | No BeOS-written file with a double-indirect range was examined, so the lengths BeOS uses there (4 blocks, or 4096 bytes) are known only from the book. | None: section 6.6 does not depend on them. A specimen would turn the BOOK row of section 6.4 into a confirmed one. |
| 2 | No BeOS-written long symbolic link was examined. | None for listing (links are not listed). |
| 3 | The order BeOS gives to names containing bytes of 0x80 and above (signed or unsigned comparison) is unconfirmed. | None: the reader does not compare keys. |
| 4 | No volume written by BeOS R4/R4.5, Dano or Zeta was examined in full, and none written by BeOS with B = 1024, 4096 or 8192 (all three BeOS volumes walked have B = 2048). | Believed none. Worth one specimen each. |
| 5 | No big-endian volume written by Haiku was examined; whether such a volume has its superblock at 512 or 0 is unknown. | None: D1 probes both. |
| 6 | Whether a "cooked" single-file image of a whole BeOS CD (all tracks concatenated at 2048 bytes per sector, no cue sheet) exists in the wild, and how its BFS track would be located. | Such a file is detected as ISO 9660 only; the BFS volume inside is not found, because the reader does not search. |
| 7 | Attributes (including `BEOS:TYPE`, and `MACOS:CREATOR` and its companions on PowerPC volumes, which could feed the existing Macintosh type and creator fields) are not surfaced, because the entry metadata has no place for them. | Follow-up for the metadata design, not for this reader. |
| 8 | Log replay is specified only in outline (section 3.6). | A volume that was not cleanly unmounted may list stale directory contents. No specimen of an unclean volume was examined. |
