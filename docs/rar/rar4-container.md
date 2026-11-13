# RAR 1.50 to 4.x: the `Rar!` archive container

Format specification for clean-room implementation. This document
describes the archive container written by RAR 1.50 to 4.x and by later
versions when asked for the old format: the layout recognised by the
seven-byte signature `52 61 72 21 1A 07 00` (`Rar!`, 0x1A, 0x07, 0x00),
called the **RAR4 format** in this document set.

It covers: finding the archive (also behind a self-extracting module), the
block structure and header checksum, the archive header, file headers in
full, names (including the Unicode name encoding), times, comments old and
new, every other block type and how to step over it, the end-of-archive
block, multi-volume sets, how members are joined, verified and handed to
the method decoders, how stored members are read, and how encryption is
recognised so that it can be reported and skipped.

It does not describe: any compression method, any cipher, the older `RE~^`
container ([`old-container.md`](old-container.md)), or the RAR5 container
(`rar5-container.md`). [`old-container.md`](old-container.md) already
specifies, for the RAR4 format, the decoder selected by each unpack-version
byte (its section 5.2), solid chains of version-15 archives (5.3),
directories written by RAR 1.5x (5.4), the file checksum of whole and split
files (6.2) and the recognition of encryption (7). Those sections remain
valid; this document refers to them instead of repeating them, and says so
explicitly in the two places where it adds to them (sections 13.3 and
15.2).

The document was written by one member of the specification team and
then verified by a second, who wrote a separate throwaway reader from the
text alone before looking at the reference implementation (section 23).

The document contains no program code and no pseudo-code.

Contents: 1 how to read this document; 2 what the container layer does;
3 finding the archive; 4 blocks; 5 the archive header; 6 the file header;
7 names; 8 times; 9 comments; 10 other blocks; 11 the end-of-archive
block; 12 volumes; 13 entries: joining, checking, reading; 14 stored
members; 15 packed members: decoder selection and solid chains;
16 encryption; 17 worked examples; 18 test vectors; 19 safety
requirements; 20 observed implementation gaps; 21 decisions; 22 open
questions; 23 evidence.

## 1. How to read this document

### 1.1 Provenance

- This is a **prose specification** for a clean-room implementation. Its
  author could read one existing open implementation; the implementers
  read only this document.
- Sources:
  1. The technical note distributed with RAR and WinRAR (`technote.txt`,
     "RAR archive file format"), in the editions shipped with RAR 2.50,
     2.90, 3.00, 3.51, 3.93 and 4.20. The older editions describe the
     comment block, the old sub-block and the "extra info" block, which
     the 4.20 edition omits. Called the **technical note**.
  2. The user manual shipped with RAR (`rar.txt`), for volume naming only.
  3. The RAR parser of The Unarchiver's XADMaster library (LGPL, revision
     `7cb9ee0a`), called the **reference implementation**. It reads less
     of the container than this document specifies: it does not check
     header checksums, does not read extended times, new-style comments or
     the end-of-archive block's fields.
  4. Black-box behaviour of four tools: `unar`/`lsar` 1.10.7 (built from
     the reference implementation), 7-Zip 26.01 (`7zz`), p7zip 17.05 and
     `bsdtar` 3.5.3. Only their behaviour was observed.
  5. Archives written for this specification by the vendor's own programs,
     used purely as archive creators: RAR 6.24 for macOS with the switch
     that selects the RAR4 format, and the DOS editions of RAR 2.50, 3.00
     and 3.93 run in an emulator. Called **genuine** archives.
  6. Real archives that existed before this work (section 23).
- The official `unrar` source, the RAR code of 7-Zip and of libarchive, and
  every other RAR decoder source were **not** consulted.
- Some facts are in none of the readable sources: the layout of the
  extended time field (the technical note says only "variable size"), the
  flags and fields of the end-of-archive block (the technical note does
  not list block type 0x7B at all), the fields of the recovery-record and
  comment sub-blocks, and the volume extensions after `.r99`. They were
  supplied from the specification author's general knowledge of the format
  and then **confirmed by experiment**; each such rule says how. The few
  parts that could not be confirmed are labelled INFERRED and are needed
  only to step over bytes.
- Where the reference implementation or another tool is wrong or unsafe,
  section 20 says so and the document specifies safe behaviour instead.
  Compatibility with any tool is not a goal; reading genuine archives
  correctly and rejecting hostile input safely is.

### 1.2 Evidence labels

| Label | Meaning |
|:--|:--|
| CONFIRMED (genuine) | Observed in archives written by RAR itself for this specification (2.50, 3.00, 3.93, 6.24) and reproduced by a throwaway reader written from this document whose listings agree with `lsar` and 7-Zip. |
| CONFIRMED (real data) | The same, on archives that existed before this work. |
| CONFIRMED (crafted) | Checked by giving the black-box tools a hand-built archive. The tools that agree are named when they do not all agree. |
| TECHNOTE | Stated by the technical note, not observed. |
| REFERENCE-ONLY | Stated by the reference implementation, not observed. |
| INFERRED | Deduced or remembered, unconfirmed. |
| DECISION | Chosen by this specification (section 21). No decision changes how a genuine, undamaged archive is read. |

A rule without a label in sections 3 to 14 is CONFIRMED (genuine) and
CONFIRMED (real data): it held for every one of the 279 volume files and
923 file headers of section 23.

### 1.3 Terms and conventions

| Term | Meaning |
|:--|:--|
| volume | One archive file. A single-volume archive is one volume. |
| **L** | The number of bytes in the volume being read. |
| **B** | The offset of the signature in the volume (0 unless the archive is self-extracting). |
| block | A unit of the archive: a header, optionally followed by a data area. |
| header size | The 16-bit size field of a block: the length of the whole header, counted from the first byte of its checksum field. |
| data area | The bytes that follow a header and belong to its block. |
| part | The portion of a member stored in one volume, with its own file header. |
| member, entry | One file, directory or link as listed to the caller, after parts have been joined. |
| **P** | The number of packed bytes of a member, all parts added up. Untrusted. |
| **U** | The unpacked size a member's header declares. Untrusted. |
| chain | A sequence of packed members sharing one decoder state ("solid"). |
| CRC-32 | The standard CRC-32: reflected polynomial 0xEDB88320, initial value and final XOR 0xFFFFFFFF; the ASCII string `123456789` gives 0xCBF43926. |
| CRC-16 | The **low 16 bits of the CRC-32** of the same bytes. There is no separate 16-bit algorithm. |
| finding | A note attached to the archive or to an entry; it does not stop anything. |
| skipped entry | An entry that is listed but whose contents are not delivered, with a one-line reason. |

All multi-byte integers are **little-endian**. Numbers are decimal unless
prefixed `0x`. Byte sequences in code style are hexadecimal. Offsets inside
a block count from the first byte of the block (the low byte of its
checksum field). "Bit 0x0010 of the flags" means the flags value has that
bit set.

## 2. What the container layer does

Given the bytes of one volume (and, for a multi-volume set, a way to obtain
the other volumes by name), the container layer:

1. finds the signature (section 3) and reads the archive header (5);
2. walks the blocks of each volume (4), producing one **part** per file
   header (6) and stepping over every other block (9 to 11);
3. joins parts into **entries** (13) and lists them: name, kind
   (file, directory, symbolic link), U, P, times, host system, attributes,
   method, unpack version, encryption, findings;
4. delivers the contents of an entry on request:
   - stored members itself (14);
   - packed members by handing their packed bytes to a method decoder
     chosen by the unpack-version byte (15), which is specified elsewhere:

     | Unpack version | Decoder document |
     |:--|:--|
     | 13, 15 | [`rar15.md`](rar15.md) |
     | 20, 26 | [`rar20.md`](rar20.md) |
     | 29, 36 | `rar29-lz.md`, with `rar29-ppmd.md` and `rar29-filters.md` |

5. verifies size and CRC-32 of everything it delivers (13.3).

Interface assumed towards every method decoder: the decoder is given, for
one member, the P packed bytes (the parts' data areas concatenated in
volume order), U, and whether the member continues the chain of the
preceding packed member; it returns exactly U bytes or "corrupt input".
The container layer never looks inside packed bytes.

## 3. Finding the archive

### 3.1 Signatures

| First bytes | Format | Document |
|:--|:--|:--|
| `52 61 72 21 1A 07 00` | RAR4 format | this one |
| `52 61 72 21 1A 07 01 00` | RAR5 format | `rar5-container.md` |
| `52 45 7E 5E` | old container | [`old-container.md`](old-container.md) |

The seventh byte tells RAR4 (`00`) from RAR5 (`01`). The seven bytes are
at the same time a block: read as a block header they give checksum
0x6152, type 0x72, flags 0x1A21 and header size 7 (the **marker block**).
Its "checksum" is not a checksum and is never verified.

### 3.2 The archive header check

A signature at offset B is accepted when all of the following hold. This
is called the **archive header check**:

- at least 20 bytes are available from B (signature plus 13);
- the byte at B + 9 (block type) is 0x73;
- the 16-bit value at B + 12 (header size) is at least 13, and B + 7 plus
  that size does not exceed L;
- the archive header's checksum is correct (section 5.3).

### 3.3 Plain and self-extracting archives

- A plain archive has its signature at offset 0.
- A self-extracting archive is an executable (the **module**) with the
  archive appended. The technical note: the reader searches for the
  signature, and the module itself does not contain it. Observed offsets
  of B: 9,461 (DOS module of RAR 2.50), 29,489 to 105,984 (the vendor's
  own installers for versions 2.50 to 4.20), 210,432 (RAR 3.93 for DOS)
  and 221,592 (module of RAR 6.24 for macOS).
- DECISION D1. When the signature is not at offset 0, the reader searches
  for it at offsets 1 to 1,048,575 in increasing order. At each position
  where the seven bytes match it applies the archive header check; the
  first position that passes is B. A position that fails is passed over
  and the search continues. If none passes, the input is not this format.
  (The reference implementation takes the first seven-byte match without
  any check, within the first 524,288 bytes; a module that happens to
  contain the seven bytes makes it fail. Test vector S1.)
- DECISION D2. When the seven bytes are at offset 0 and the archive header
  check fails, the input is this format but unreadable: the result is the
  archive-level error "corrupt archive", with no entries. No search is
  made in that case.
- All offsets stored in an archive (there is only one, section 5.2) count
  from the start of the **file**, not from B. All block walking starts at
  B + 7.

## 4. Blocks

### 4.1 The common header

Every block starts with seven bytes:

| Offset | Size | Field | Meaning |
|--:|--:|:--|:--|
| 0 | 2 | HEAD_CRC | CRC-16 over part of the header (4.4). |
| 2 | 1 | HEAD_TYPE | Block type (4.2). |
| 3 | 2 | HEAD_FLAGS | Flags. Two bits mean the same in all blocks (below); the others depend on the type. |
| 5 | 2 | HEAD_SIZE | Header size in bytes, counted from offset 0, at least 7. |

Flag bits with a common meaning:

| Bit | Meaning |
|:--|:--|
| 0x8000 | A 4-byte data size follows at offset 7 and that many bytes of data follow the header. See 4.3 for when the bit is believed. |
| 0x4000 | Advice to an updating program ("older versions may drop this block"). No meaning for a reader. |

The block occupies the header (HEAD_SIZE bytes) plus its data area. The
next block starts immediately after; there is no alignment or padding.

### 4.2 Block types

| Type | Name in this document | Data area | Specified in |
|:--|:--|:--|:--|
| 0x72 | marker (the signature) | none | 3.1 |
| 0x73 | archive header | none | 5 |
| 0x74 | file header | the part's packed bytes | 6 |
| 0x75 | old comment | none; occurs only nested inside 0x73 and 0x74 headers | 9.1 |
| 0x76 | old authenticity information | none | 10.4 |
| 0x77 | old sub-block | yes | 10.3 |
| 0x78 | old recovery record | yes | 10.3 |
| 0x79 | old authenticity information, second form | none | 10.4 |
| 0x7A | sub-block ("service header") | yes, possibly empty | 10.1 |
| 0x7B | end of archive | none | 11 |
| other | unknown | by flag 0x8000 | 10.5 |

Types 0x72 to 0x7A are listed by the technical note; 0x7B is not.

### 4.3 The size of the data area

The size of a block's data area is found as follows (DECISION D3; it
agrees with every genuine and real block of the survey):

| Type | Data size |
|:--|:--|
| 0x74, 0x7A | **Always** the 32-bit value at offset 7, whatever flag 0x8000 says; when flag 0x0100 is set, a further 32-bit value at offset 32 supplies bits 32 to 63 (section 6.1). |
| 0x77, 0x78 | **Always** the 32-bit value at offset 7, whatever flag 0x8000 says. |
| 0x72, 0x73, 0x7B | **Always** 0, whatever flag 0x8000 says. |
| every other type | The 32-bit value at offset 7 if flag 0x8000 is set, otherwise 0. |

Background: genuine writers always set 0x8000 on 0x74, 0x77, 0x78 and
0x7A and never on 0x73 or 0x7B. The tools disagree about blocks that
break this habit (CONFIRMED (crafted)): all three accept a file header
without the bit; `lsar` reads a data size from an archive header that
carries the bit and then loses the first file, the others ignore the bit;
`lsar` and 7-Zip stop at a 0x7A block without the bit, `bsdtar` reads it.
The table above gives one answer per type that does not depend on an
attacker-controlled flag.

### 4.4 The header checksum

HEAD_CRC is the CRC-16 (low 16 bits of the CRC-32) of header bytes
starting at offset 2 (the type byte). Where the covered range ends depends
on the type:

| Type | Bytes covered | Evidence |
|:--|:--|:--|
| 0x73 | Offsets 2 to 12: the 11 bytes from the type byte to the end of the fixed part, **even when the header is longer** because a comment block is nested behind it. | 279 of 279 archive headers, 4 of them with a nested comment |
| 0x74, 0x7A | Offsets 2 to HEAD_SIZE − 1: the whole header, including salt and extended time. **Exception, type 0x74 only:** when file header flag 0x0008 is set (a comment block is nested), only offsets 2 up to and including the last byte of the name. For type 0x7A flag 0x0008 has no such meaning and the whole header is always covered. | 923 of 923 file headers (one with a nested comment), 50 of 50 sub-blocks |
| 0x7B | Offsets 2 to HEAD_SIZE − 1. | 237 of 237 |
| 0x75 | Offsets 2 to 12 (type byte up to and including its comment-CRC field). | 5 of 5 nested comment blocks |
| 0x78 | Offsets 2 to HEAD_SIZE − 1. | 5 of 5 |
| 0x76, 0x79 | Not reproducible: the stored value matched no simple range in 3 of 3 real blocks. | |
| 0x77 | Unknown (no sample in this survey). | |

DECISION D4, verification:

- The checksum is verified for types **0x73, 0x74, 0x7A and 0x7B**, and
  for a nested 0x75 block before its comment is used.
- For 0x73 a header is accepted when the stored value equals the CRC-16 of
  offsets 2 to 12 **or** of offsets 2 to HEAD_SIZE − 1. For 0x74 with flag
  0x0008 it is accepted when it equals the CRC-16 up to the end of the
  name **or** of the whole header. (The second alternative in each case is
  a tolerance for writers not seen; it costs nothing.)
- For every other type the checksum is **not verified**. Those blocks are
  only stepped over, their sizes are bounds-checked (4.5), and a wrong
  size in one of them is caught by the checksum of the next verified
  header.
- A mismatch is a "corrupt header" (4.5).

The reference implementation verifies no header checksum (its author
found the coverage "completely inconsistent"); the table shows the
inconsistency is limited to nested comments and the authenticity blocks.
7-Zip and `bsdtar` reject a file header with a wrong checksum (CONFIRMED
(crafted), test vector R1).

### 4.5 Walking the blocks of a volume

The walk starts at position B + 7, where the archive header must be
(section 5), and continues at B + 7 plus the archive header's HEAD_SIZE.
From there on, at each position p:

1. If p equals L, the volume ends here. This is a normal end: archives
   written before RAR 3.0, or with the end block switched off, have no end
   block.
2. If fewer than 7 bytes remain, the volume ends with the finding "stray
   bytes after the last block".
3. The common header is read. The block is a **corrupt header** when any
   of these holds:
   - HEAD_SIZE is less than 7;
   - p + HEAD_SIZE exceeds L;
   - HEAD_SIZE is less than the minimum for its type: 32 for 0x74 and
     0x7A (40 when flag 0x0100 is set), 11 for 0x77 and 0x78, 11 for a
     block of any other type that carries a data size by 4.3;
   - for 0x74 and 0x7A, the name does not fit: 32 (or 40) plus NAME_SIZE
     exceeds HEAD_SIZE;
   - the type is 0x72 or 0x73 (a second marker or archive header inside
     the archive; DECISION D5);
   - the checksum is wrong, for a type whose checksum is verified.
4. On a corrupt header the walk of this volume **stops**. Everything
   listed so far stays listed; the archive gets the finding "corrupt
   header at offset p". Nothing after p in this volume is used.
5. Otherwise the data size D is determined (4.3). If p + HEAD_SIZE + D
   exceeds L, the block is **truncated**: for a file header the part is
   recorded with the bytes that are actually present and marked
   truncated (13.2); for any other block there is nothing to record. In
   both cases the archive gets the finding "truncated" and the walk of
   this volume stops.
6. The block is processed by type: a file header yields a part; an end
   block ends the volume (section 11); all others yield nothing.
7. The walk continues at p + HEAD_SIZE + D.

A walk that stopped at a corrupt header or at a truncated block also ends
the **set**: no further volume is read (DECISION D22), because blocks,
and with them members of chains, may have been lost at that point. A
member that was open at that moment is closed by the rules of 13.2.

Every step advances by at least 7 bytes, so the walk terminates. All
position arithmetic is done in at least 64 bits, with the comparison
against L made **before** any addition that could overflow (D can be up
to 2^64 − 1).

## 5. The archive header (type 0x73)

### 5.1 Layout

| Offset | Size | Field | Meaning |
|--:|--:|:--|:--|
| 0 | 2 | HEAD_CRC | Section 5.3. |
| 2 | 1 | HEAD_TYPE | 0x73 |
| 3 | 2 | HEAD_FLAGS | Below. |
| 5 | 2 | HEAD_SIZE | 13, or more when something is appended. |
| 7 | 2 | RESERVED1 | High part of the position below. Zero in every archive seen. |
| 9 | 4 | RESERVED2 | Zero, or the position of the authenticity block (5.2). |
| 13 | 1 | ENCRYPT_VER | Present only if flag 0x0200 is set. REFERENCE-ONLY; never observed. Informational. |
| 13 or 14 | | nested comment block | Present only if flag 0x0002 is set (section 9.1). |

The archive header has no data area. Bytes between the end of the fields
a reader understands and HEAD_SIZE are skipped. DECISION D6: flag 0x8000
on an archive header is ignored (4.3).

Flags:

| Bit | Meaning | Evidence |
|:--|:--|:--|
| 0x0001 | The archive is one volume of a multi-volume set (section 12). | observed in 220 volumes |
| 0x0002 | An old-style comment block is nested in this header (9.1). Not set by RAR 3.0 and later, which write a `CMT` sub-block instead. | genuine (RAR 2.50), real (RAR 1.55, 2.50, 2.90) |
| 0x0004 | The archive is locked against modification. No effect on reading. | genuine, real |
| 0x0008 | Solid archive. Decides chains only for unpack versions below 20 (section 15.2). | genuine, real |
| 0x0010 | Only meaningful together with 0x0001: the volumes are named by the new scheme (12.2). The technical note of RAR 2.50 calls the bit "unused"; the reference implementation records a second, older meaning for it ("packed comment", REFERENCE-ONLY), which no archive of the survey shows (the RAR 1.55 archive with a packed comment has flags 0x002A). See 12.2 for the consequence. | genuine |
| 0x0020 | Old-style authenticity information is present (10.4). Not set by RAR 3.0 and later. | real (RAR 1.55, 2.50) |
| 0x0040 | A recovery record is present. Informational. | genuine |
| 0x0080 | **The block headers are encrypted** (section 16.2). | genuine (RAR 3.00, 3.93, 6.24) |
| 0x0100 | This is the first volume of the set. Written by RAR 3.0 and later only. | genuine |
| 0x0200 | The ENCRYPT_VER byte is present. | REFERENCE-ONLY |
| others | Not seen. Ignored (DECISION D6). | |

### 5.2 The reserved fields

In archives that carry authenticity information (blocks 0x76, 0x79, or
the `AV` sub-block), RESERVED2 holds the offset of that block **from the
start of the file**, also when the archive is self-extracting. CONFIRMED
(real data): 8 of 8 such archives. RESERVED1 would be the high 16 bits.
A reader does not use either field.

### 5.3 Validation

The archive header is valid when the archive header check of 3.2 passes;
its checksum is accepted as described in 4.4 (offsets 2 to 12, or the
whole header). The first block after the signature **must** be an archive
header: anything else is "corrupt archive" (DECISION D2; the reference
implementation and `bsdtar` list such an archive, 7-Zip refuses it;
test vector R7).

If flag 0x0080 is set, the walk ends right after the archive header: see
section 16.2.

## 6. The file header (type 0x74)

### 6.1 Layout

| Offset | Size | Field | Meaning |
|--:|--:|:--|:--|
| 0 | 2 | HEAD_CRC | Section 4.4. |
| 2 | 1 | HEAD_TYPE | 0x74 |
| 3 | 2 | HEAD_FLAGS | Section 6.2. |
| 5 | 2 | HEAD_SIZE | Size of the whole header including name and everything after it. |
| 7 | 4 | PACK_SIZE | Low 32 bits of the size of the data area: the packed bytes of **this part**. |
| 11 | 4 | UNP_SIZE | Low 32 bits of U: the unpacked size of the **whole member**, repeated in every part. |
| 15 | 1 | HOST_OS | System the member was archived on (6.4). |
| 16 | 4 | FILE_CRC | CRC-32; what it covers depends on the part (13.3). |
| 20 | 4 | FTIME | Modification time, MS-DOS format (8.1). |
| 24 | 1 | UNP_VER | Unpack version: selects the decoder (15.1). |
| 25 | 1 | METHOD | 0x30 stored, 0x31 to 0x35 packed (6.3). |
| 26 | 2 | NAME_SIZE | Length of the name field in bytes. |
| 28 | 4 | ATTR | File attributes of the host system (6.4). |
| 32 | 4 | HIGH_PACK_SIZE | Present only if flag 0x0100 is set: bits 32 to 63 of the data size. |
| 36 | 4 | HIGH_UNP_SIZE | Present only if flag 0x0100 is set: bits 32 to 63 of U. |
| 32 or 40 | NAME_SIZE | FILE_NAME | Section 7. |
| | 8 | SALT | Present only if flag 0x0400 is set. Used by the cipher; a non-decrypting reader ignores it. |
| | variable | EXT_TIME | Present only if flag 0x1000 is set (8.2). |
| | | nested comment block | Present only if flag 0x0008 is set (9.1). |

The optional areas appear in the order of the table. CONFIRMED (genuine)
for name, salt, extended time in that order (39 headers carry both salt
and extended time). The comment block was seen only directly after the
name, in a header without salt and extended time (RAR 2.50 writes
neither). DECISION D7a: should a header have flag 0x0008 together with
0x0400 or 0x1000, the reader takes the salt and the extended time first,
as the table says, and looks for the comment block after them; if no
valid comment block is found there, the comment is dropped (D7) and
nothing else changes.

Rules:

- The data area starts at offset HEAD_SIZE, never at the end of the fields
  the reader understands. Bytes between the last understood field and
  HEAD_SIZE are skipped (the technical note: "other new fields may appear
  here"). CONFIRMED (crafted): `lsar` and `bsdtar` accept four unexplained
  bytes after the name; 7-Zip rejects such a header (section 20).
- Flag 0x0100: CONFIRMED (genuine) by a file of 4,613,734,400 bytes
  (header in test vector H1: PACK_SIZE 0x00233298, UNP_SIZE 0x13000000,
  HIGH_PACK_SIZE 0, HIGH_UNP_SIZE 1). RAR sets the flag only when a size
  needs it; a header with the flag and both high fields zero is read the
  same way (CONFIRMED (crafted), test vector A5).
- DECISION D7. Optional areas that do not fit inside HEAD_SIZE (a salt
  with fewer than 8 bytes left, an extended time field that runs past the
  header, a comment block that is malformed) are treated as absent, with
  a finding; the part itself stays valid. The areas are taken in the
  order of the table, and **the first one that does not fit makes every
  later one absent too** (their positions are no longer known): a salt
  that does not fit also removes the extended time and the comment; an
  extended time field that does not fit also removes the comment. One
  finding is enough. Test vector R15.
- DECISION D8. NAME_SIZE 0 is not a corrupt header: the part is consumed
  normally and its entry is a skipped entry "empty name".

### 6.2 Flags

| Bits | Meaning | Evidence |
|:--|:--|:--|
| 0x0001 | This part continues a member from the previous volume. | genuine, real |
| 0x0002 | The member continues in the next volume. | genuine, real |
| 0x0004 | The data is encrypted (section 16.1). | genuine |
| 0x0008 | An old-style comment block is nested in the header (9.1). Not set by RAR 3.0 and later. | genuine (RAR 2.50) |
| 0x0010 | For unpack version 20 and above: this member continues the chain of the preceding packed member (15.2). Meaningless below version 20. | genuine, real |
| 0x00E0 | A three-bit number (flags shifted right by 5, masked with 7). 7 means **directory** (6.5). 0 to 6 give the dictionary size the packer used: 64, 128, 256, 512, 1,024, 2,048, 4,096 KiB. Informational: no decoder needs it. Not used below version 20 (always 0 there). | values 0, 1, 4, 5, 6, 7 observed |
| 0x0100 | The two HIGH fields are present. | genuine |
| 0x0200 | The name field has the Unicode form (7.2). | genuine, real |
| 0x0400 | An 8-byte salt follows the name. Written by RAR 3.0 and later for every encrypted file. | genuine |
| 0x0800 | The member is an old version of a file; the name carries the suffix `;n` with a decimal version number (7.5). | genuine (RAR 3.00) |
| 0x1000 | The extended time field is present. | genuine (RAR 3.93, 6.24), real |
| 0x2000 | Not seen. Ignored. | |
| 0x4000 | Common meaning (4.1). Not seen on file headers. | |
| 0x8000 | Always set by genuine writers. Ignored: a file header always has a data size (4.3). | 923 of 923 |

### 6.3 Method and unpack version

- METHOD 0x30: **stored** (section 14). METHOD 0x31 to 0x35: **packed**;
  the five values are effort levels of the packer and tell the decoder
  nothing. Any other value: skipped entry "unsupported method"
  ([`old-container.md`](old-container.md) decision V1; `unar` decodes
  0x36 like the others, section 20).
- UNP_VER holds ten times the major plus the minor version of the oldest
  RAR able to unpack the member. It matters only for packed members
  (15.1). For stored members and directories it is irrelevant; writers
  put 20 or 29 there.
- The combinations observed: version 15 with method 0x35;
  version 20 with 0x30, 0x33, 0x35; version 29 with 0x30, 0x31, 0x33,
  0x34, 0x35. RAR 3.x and later write directories, and RAR 6.24 writes
  stored files, as "version 20, method 0x30" inside archives whose packed
  files are version 29: versions are mixed freely within one archive.

### 6.4 Host system and attributes

| HOST_OS | System | ATTR holds |
|--:|:--|:--|
| 0 | MS-DOS | DOS attribute bits. |
| 1 | OS/2 | DOS attribute bits. |
| 2 | Windows | Windows attribute bits (the low byte is the DOS set). |
| 3 | Unix | The Unix mode word: permission bits in the low 12 bits, file type in bits 0xF000. |
| 4 | Mac OS | Not interpreted (TECHNOTE; not observed). |
| 5 | BeOS | Not interpreted (TECHNOTE; not observed). |
| other | unknown | Not interpreted (DECISION D9). |

- DOS attribute bits: 0x01 read-only, 0x02 hidden, 0x04 system, 0x08
  volume label, 0x10 directory, 0x20 archive. Windows adds higher bits
  (0x2000 was seen on files in one WinRAR 4.20 archive); they are passed
  on uninterpreted.
- Unix file types in bits 0xF000: 0x8000 regular file, 0x4000 directory,
  0xA000 symbolic link. Observed: 0x81A4 (regular, permissions 644
  octal), 0x41ED (directory, 755), 0xA1ED (symbolic link). One genuine
  header written from standard input has no type bits (0x01A4): absent
  type bits mean a regular file.
- Hosts 0, 2 and 3 were observed; host 1 is covered by
  [`old-container.md`](old-container.md).

### 6.5 Directories and symbolic links

- **Directory.** For unpack version 20 and above, an entry is a directory
  when, and only when, flag bits 0x00E0 are all set. Genuine directories
  have PACK_SIZE 0, UNP_SIZE 0, FILE_CRC 0, the directory attribute of
  their host (0x10, or Unix type 0x4000), method 0x30 (0x33 or 0x35 from
  RAR 2.50), and a name without a trailing separator (108 directory
  headers from five RAR versions). For unpack versions below 20 the rule
  of [`old-container.md`](old-container.md) section 5.4 applies instead
  (attribute bit 0x10 with host 0 or 1).
- DECISION D10. The attribute word is not used to recognise directories of
  version 20 and above. (7-Zip also treats an entry as a directory when
  only its attributes say so; `lsar` does not; test vector A6 pins the
  answer.) A directory entry whose data size is not 0 is listed as a
  directory with a finding; its data area is skipped. A directory takes
  no part in decoding and has no contents.
- Directories are written **after** the files they contain by RAR 3.x and
  later and **before** them by RAR 2.50; a reader must not depend on
  either order, and must accept files whose directory has no entry.
- **Symbolic link.** With HOST_OS 3 and Unix type 0xA000 the member is a
  symbolic link and its **contents are the link target**: U bytes holding
  a path, without terminator. CONFIRMED (genuine): one header, stored
  (method 0x30), U = 5, contents `a.txt`, FILE_CRC = CRC-32 of those five
  bytes; and CONFIRMED (crafted) with `bsdtar`. DECISION D11: the entry is
  listed as a symbolic link; its target is obtained by reading the
  member's contents like any file's (stored or packed), is limited to
  4,096 bytes (a longer one makes the entry a plain file with a finding),
  and is never followed or interpreted by the reader.

## 7. Names

The name field is NAME_SIZE bytes. Its interpretation depends on flag
0x0200.

### 7.1 Without the Unicode flag

The field is one byte string: the path in the character set of the
archiving system. No terminator.

- RAR for DOS and Windows writes the **OEM code page** of the machine
  (genuine archives from RAR 2.50, 3.00 and 3.93 for DOS hold `Ä` as 0x8E,
  which is code page 437 or 850).
- RAR 6.24 for macOS writes pure ASCII names this way and gives every name
  with a non-ASCII character the Unicode form instead.
- DECISION D12. The bytes are decoded as UTF-8 when they are valid UTF-8
  (this covers ASCII, the overwhelmingly common case, and Unix writers
  with a UTF-8 locale); otherwise with the legacy code page the caller
  configured, by default code page 437. The archive does not record the
  code page; no tool can do better than guess (`lsar` and 7-Zip show
  three different wrong names for one such header).

### 7.2 With the Unicode flag

Let Z be the offset of the first zero byte in the field.

- **No zero byte:** the whole field is the name in **UTF-8**. TECHNOTE
  (editions 3.51 and later); CONFIRMED (crafted) with `lsar`, 7-Zip and
  `bsdtar`. Invalid UTF-8 sequences are replaced by U+FFFD each.
- **A zero byte at Z:** the field has two parts. Bytes 0 to Z − 1 are the
  **plain name**, in the form of 7.1 (RAR 6.24 for macOS writes UTF-8
  there; Windows versions write the OEM code page). Bytes Z + 1 to the end
  are the **encoded name**: a compact encoding of the same name as UTF-16
  code units, which partly refers back to the plain name's bytes.

  The plain name's bytes are used as raw bytes by the decoding below,
  whatever their character set.

Decoding the encoded name (E bytes, E = NAME_SIZE − Z − 1) into a
sequence of 16-bit code units:

1. If E is less than 2, there is no encoded name: use the plain name
   (7.1).
2. The first byte is the **high byte** H, used by some items below.
3. The rest is a sequence of **groups**. A group starts with one **flag
   byte** that describes up to four **items**: its bits 7 and 6 give the
   mode of the first item, bits 5 and 4 of the second, bits 3 and 2 of the
   third, bits 1 and 0 of the fourth. The bytes of the items follow the
   flag byte in the same order. After the fourth item the next byte is a
   new flag byte.
4. Decoding ends when all E bytes have been consumed. This can be after a
   flag byte, or after any item; unused mode bits of the last flag byte
   mean nothing.
5. Items, by mode:

   | Mode | Item bytes | Code units produced |
   |--:|:--|:--|
   | 0 | one byte b | one unit: b |
   | 1 | one byte b | one unit: H × 256 + b |
   | 2 | two bytes b0, b1 | one unit: b0 + 256 × b1 |
   | 3 | one byte n; then, only if bit 7 of n is set, one byte c | a **run** of (n masked with 0x7F) + 2 units taken from the plain name, see below |

6. A run produces its units one at a time. Let k be the number of code
   units produced so far (by all items, including earlier units of this
   run), and let q be byte k of the plain name. When bit 7 of n is clear
   the unit is q. When bit 7 is set the unit is
   H × 256 + ((q + c) modulo 256).

   So a run copies, position for position, characters whose code unit
   equals (or, with a correction, relates simply to) the byte at the same
   index in the plain name. A run is 2 to 129 units long.
7. The encoded name is **malformed** when: an item's bytes are missing
   (the data ends inside an item); a run needs a plain-name byte at an
   index k that is not less than Z; any produced unit is 0; or no unit at
   all is produced. DECISION D13: a malformed encoded name is discarded
   whole and the plain name is used (7.1), with a finding.
8. The units are UTF-16. A high surrogate followed by a low surrogate
   forms one character; any other surrogate becomes U+FFFD (DECISION
   D13).
9. Z = 0 (the field starts with a zero byte) is allowed: the plain name
   is empty, so no run is possible. If the encoded name is then missing
   or malformed, the name is empty and D8 applies.

Evidence: CONFIRMED (genuine) on 66 headers from RAR 6.24 and CONFIRMED
(real data) on 3 headers from a WinRAR-made set; CONFIRMED (crafted) for
each mode, runs with and without correction, the modulo-256 wrap of the
correction, and surrogate pairs, with `lsar` and 7-Zip agreeing with each
other and with this text (section 18.2). Genuine writers do not produce
malformed encodings; the tools differ on them (section 20).

### 7.3 Separators and safety

- The path separator in every stored name is the **backslash** (0x5C),
  also in archives made on Unix and macOS, in the plain name and in the
  decoded Unicode name alike. A genuine name never starts or ends with a
  separator and has no drive prefix.
- DECISION D14. After decoding to text, the name is split at every
  backslash **and** every forward slash (7-Zip, `lsar` and `bsdtar` all
  show a crafted `a/b.txt` as a path). The components then go through the
  repository's safe-path handling like any archive name: empty
  components, `.` and `..`, leading separators and drive prefixes get no
  special treatment here. A Unix file name that contains a backslash
  cannot be told from a path; it becomes a path.
- The tools list crafted names `\abs.txt` and `..\up.txt` as `/abs.txt`
  and `../up.txt`; nothing in the container prevents such names.

### 7.4 Limits

NAME_SIZE is 16 bits and must fit in the header (4.5), so a name field is
at most 65,503 bytes. A run can produce up to 129 code units from two
encoded bytes, but every unit of a run needs its own plain-name byte and
every other unit its own encoded byte, so a decoded Unicode name never
has more than Z + E code units, which is less than NAME_SIZE. No further
limit is imposed here.

### 7.5 File versions

With flag 0x0800 the stored name ends in `;` and a decimal number
(`hello.txt;1`). CONFIRMED (genuine): 6 headers from RAR 3.00, which
stores an older version next to the current one. DECISION D15: the name
is listed exactly as stored, suffix included, so that the versions do not
collide. Without the flag a `;` in a name means nothing.

## 8. Times

### 8.1 The DOS time

FTIME is the MS-DOS packed date and time: bits 0 to 4 hold the seconds
divided by 2, bits 5 to 10 the minutes, bits 11 to 15 the hours, bits 16
to 20 the day, bits 21 to 24 the month, bits 25 to 31 the years since
1980. It is a **local time without zone** of the archiving machine.

DECISION D16. The time is invalid, and the entry has no modification time,
when the month is 0 or above 12, the day is 0 or beyond the end of that
month, the hour is above 23, the minute above 59 or the doubled seconds
above 58. An all-zero FTIME is invalid by this rule. (7-Zip shows no time
for such headers, except that it turns a day beyond the end of the month
into a date of the following month; `lsar` and `bsdtar` normalise all of
them into some other date. Test vectors T1 to T10.) A valid time is passed on as local time; the reader does not guess
a zone.

### 8.2 The extended time field

Present when flag 0x1000 is set, after the name and the salt. It refines
the modification time to 100-nanosecond precision and can add up to three
more times. Its layout was supplied from general knowledge (section 1.1)
and then CONFIRMED (genuine) on 497 headers against the times 7-Zip
prints, and CONFIRMED (crafted) with 7-Zip for the fraction widths that
RAR 6.24 does not write.

The field starts with a 16-bit little-endian **flags word** holding four
4-bit groups, from the most significant group to the least:

| Bits of the flags word | Time |
|:--|:--|
| 15 to 12 | modification time |
| 11 to 8 | creation time (on Unix: the status-change time) |
| 7 to 4 | last access time |
| 3 to 0 | archiving time (INFERRED; never observed) |

Within a group: bit 3 (value 8) means the time is **present**; bit 2
(value 4) means **add one second**; bits 1 and 0 give a number n (0 to 3)
of **fraction bytes**. A group whose bit 3 is clear contributes no bytes
and no time; its other bits are ignored.

After the flags word come the bytes of the present groups, in the order
of the table:

1. For every time **except the modification time**: 4 bytes, a DOS time
   in the format of 8.1. The modification time has none: its DOS time is
   FTIME.
2. Then n fraction bytes. They are the **most significant** n bytes of a
   24-bit number F, least significant of them first; the missing low
   bytes are 0. That is: with n = 3 and bytes b0 b1 b2,
   F = b0 + 256 × b1 + 65,536 × b2; with n = 2, F = 256 × b0 +
   65,536 × b1; with n = 1, F = 65,536 × b0; with n = 0, F = 0.

The time is: the DOS time, plus one second if bit 2 is set, plus F units
of 100 nanoseconds. (The DOS time has two-second resolution; bit 2
supplies the odd second.) Genuine F is below 10,000,000; a larger F
simply carries into the seconds (7-Zip does the same, CONFIRMED
(crafted)).

Rules:

- A DOS time that is invalid by 8.1 makes that one time absent.
- DECISION D7 applies: if the field needs more bytes than the header
  holds, the whole extended time field is ignored (the modification time
  is then FTIME alone), with a finding.
- Bytes after the last group, up to HEAD_SIZE, are skipped.
- Observed flags words: 0xF000, 0xC000 and 0xB000 (modification time
  only, with and without fraction), 0xFFF0, 0xCFC0 and 0xCFF0 (three
  times) from RAR 6.24 and WinRAR, and 0x0880 from RAR 3.93 for DOS
  (creation and access time in whole seconds, no modification group at
  all).

## 9. Comments

Reading comments is optional. A reader that does not show comments needs
this section only to know that comment data takes no part in anything
else. DECISION D17: a comment that is malformed, fails its checksum,
needs an unavailable decoder, or declares more than 262,144 unpacked
bytes is dropped with a finding; it never affects the listing or any
entry. Comment data never belongs to a chain: it is decoded with a fresh
decoder state of its own and leaves the files' state untouched.

### 9.1 The old comment block (type 0x75)

Written by RAR 1.5x and 2.x. It never stands alone between other blocks:
it is **nested** at the end of an archive header (archive comment, archive
flag 0x0002) or of a file header (file comment, file flag 0x0008), inside
that header's HEAD_SIZE.

| Offset in the nested block | Size | Field | Meaning |
|--:|--:|:--|:--|
| 0 | 2 | HEAD_CRC | CRC-16 of offsets 2 to 12. |
| 2 | 1 | HEAD_TYPE | 0x75 |
| 3 | 2 | HEAD_FLAGS | 0 in every block seen. |
| 5 | 2 | HEAD_SIZE | 13 plus the number of comment data bytes. |
| 7 | 2 | UNP_SIZE | Unpacked length of the comment. |
| 9 | 1 | UNP_VER | Unpack version, as in a file header. |
| 10 | 1 | METHOD | 0x30 stored, 0x31 to 0x35 packed. |
| 11 | 2 | COMM_CRC | CRC-16 (low 16 bits of the CRC-32) of the **unpacked** comment. |
| 13 | HEAD_SIZE − 13 | | Comment data. |

- A block of type 0x75 met between other blocks during the walk (never
  written by RAR) is stepped over like an unknown block (10.5).
- The nested block must lie entirely inside the enclosing header: it
  starts where the enclosing header's own fields end (offset 13 of an
  archive header, or 14 with ENCRYPT_VER; directly after the name of a
  file header) and its HEAD_SIZE must be at least 13 and must not reach
  past the enclosing HEAD_SIZE.
- Stored (0x30): the data is the comment; its length must equal UNP_SIZE.
  CONFIRMED (genuine): a file comment written by RAR 2.50 (UNP_VER 15,
  method 0x30, 4 bytes).
- Packed: the data is one stream for the decoder that UNP_VER selects
  (15.1), U = UNP_SIZE, fresh state. CONFIRMED (genuine and real data):
  archive comments with version 20, method 0x34 (RAR 2.50, 2.90) and
  version 15, method 0x34 (RAR 1.55); `lsar` prints them.
- The comment is text in the OEM code page (7.1), usually with CR LF line
  ends; old archives use ANSI escape sequences in it.
- COMM_CRC confirmed for the two genuine blocks whose text is known (it
  equals the low 16 bits of the CRC-32 of the text), and for one real
  block in [`old-container.md`](old-container.md) section 5.5.

### 9.2 The comment sub-block (`CMT`)

Written by RAR 3.0 and later: a sub-block (section 10.1) named `CMT`,
placed directly after the archive header. Its fields, in the file-header
layout:

- data area (PACK_SIZE bytes): the comment, stored or packed;
- UNP_SIZE: unpacked length; FILE_CRC: **CRC-32** (all 32 bits) of the
  unpacked comment; UNP_VER and METHOD as for a file (observed: version
  29, method 0x33);
- ATTR bit 0x0001: when set the unpacked comment is **UTF-16
  little-endian** text; when clear it is narrow text as in 7.1.

CONFIRMED (crafted, on genuine data): changing the type byte of a genuine
`CMT` block to 0x74 (and recomputing HEAD_CRC) turns it into a file that
`unar` extracts with a correct CRC-32; done for three archives from RAR
3.00 and 6.24. The extracted bytes are the comment text exactly; a
comment containing non-ASCII characters came out as UTF-16LE from the one
block that had ATTR bit 0 set.

Decoding a packed `CMT` needs the version-29 decoder (`rar29-lz.md`),
with a fresh state. File comments in sub-block form were not observed.

## 10. Other blocks

None of these produces an entry. All are stepped over by header size plus
data size (4.3). Their data areas are never given to a file's decoder and
never change a chain, also when they are packed with the same method as
files.

### 10.1 The sub-block (type 0x7A)

A sub-block has **exactly the layout of a file header** (6.1): the same
fixed fields at the same offsets, a name of NAME_SIZE bytes, optional
further bytes up to HEAD_SIZE, and a data area of PACK_SIZE bytes (64-bit
with flag 0x0100, INFERRED for sub-blocks). Its "name" is a short ASCII
tag that says what the block is. Its header checksum covers the whole
header.

| Tag | Contents | Bytes after the name | Data area | Evidence |
|:--|:--|:--|:--|:--|
| `CMT` | Archive comment (9.2). | none | the comment | genuine, real |
| `RR` | Recovery record. | 8 bytes `Protect+`; 4 bytes: number of recovery sectors R; 8 bytes: number of protected blocks N | R × 512 + N × 2 bytes | genuine (10 blocks: the data size equals R × 512 + N × 2 in each) |
| `AV` | Authenticity information. | all of it (about 190 bytes) | empty | real (5 vendor installers) |
| `UOW` | Unix owner and group of the preceding file. | owner name, a zero byte, group name | empty | genuine (24 blocks) |
| `STM`, `ACL`, `EA2`, `EABE`, `MAC` | NTFS alternate data stream, NTFS permissions, OS/2 and BeOS extended attributes, Mac OS information. | varies | varies; an `STM` block's data is the stream's contents, packed like a file | INFERRED (general knowledge; not observed, cannot be produced on the systems available) |
| any other | unknown | | | skipped the same way |

Rules:

- A sub-block that follows a file header belongs to that file (`UOW`
  follows each file and directory header it describes); `CMT` follows the
  archive header; `RR` and `AV` stand at the end of the volume, before
  the end block.
- The flags of a sub-block are not interpreted, except 0x0100 (sizes).
  Observed: 0x8000 (`CMT`), 0x8010 (`UOW`), 0xC000 (`RR`, `AV`). Bit
  0x0010 on a sub-block has nothing to do with chains.
- The reader does not interpret the tag when stepping over the block: the
  rule is the same for every tag.
- DECISION D18. A reader exposes none of this except, optionally, the
  comment.

### 10.2 The marker inside a volume

Block type 0x72 after the start is a corrupt header (4.5, D5). `lsar` and
`bsdtar` step over it; 7-Zip reports a header error.

### 10.3 Old sub-block (0x77) and old recovery record (0x78)

Both always have a 4-byte data size at offset 7 and a data area of that
size.

Old recovery record, CONFIRMED (genuine): 5 blocks from RAR 2.50, flags
0xC000, HEAD_SIZE 26:

| Offset | Size | Field |
|--:|--:|:--|
| 7 | 4 | Data size: R × 512 + N × 2. |
| 11 | 1 | Version (20). |
| 12 | 2 | Number of recovery sectors R. |
| 14 | 4 | Number of protected blocks N. |
| 18 | 8 | `Protect!` |

It stands after the last file header of its volume.

Old sub-block, TECHNOTE (not in this survey; one real archive with 96 of
them is described in [`old-container.md`](old-container.md) section 5.5):
flags 0xC000; data size (4 bytes) at offset 7, sub-type (2 bytes) at 11,
a reserved byte at 13, then fields that depend on the sub-type. Sub-type
0x0100 is "OS/2 extended attributes" and continues with unpacked size
(4), unpack version (1), method (1) and a CRC-32 of the unpacked
attributes (4); its data area is the packed attributes. It follows the
file header it belongs to.

### 10.4 Old authenticity information (0x76, 0x79)

Header only, no data area. Observed: type 0x76 with HEAD_SIZE 113 and
flags 0x0000 (RAR 1.55), type 0x76 with HEAD_SIZE 94 and flags 0x4000
(RAR 2.50), type 0x79 with HEAD_SIZE 206 and flags 0x4000 (RAR 2.90);
each is the last block of its archive, and the archive header's RESERVED2
points at it. Their HEAD_CRC is not verified (4.4). Stepped over by 4.3:
header size, plus a data size only if flag 0x8000 were set.

### 10.5 Unknown block types

DECISION D19. A block of any type not in the table of 4.2 is stepped over
by header size plus, if flag 0x8000 is set, the data size at offset 7;
its checksum is not verified. (`lsar` does the same; 7-Zip and `bsdtar`
stop with an error; test vector A3.) If the size of such a block is
garbage, the next verified header fails its checksum and the walk stops
there.

## 11. The end-of-archive block (type 0x7B)

Written by RAR 3.0 and later as the last block of every volume, unless
the user switched it off; never written by RAR 2.x. Its layout was
supplied from general knowledge (section 1.1) and confirmed on 237
blocks (225 genuine, 12 real) as described below.

| Offset | Size | Field | Present when |
|--:|--:|:--|:--|
| 0 | 2 | HEAD_CRC (whole header from offset 2) | always |
| 2 | 1 | HEAD_TYPE = 0x7B | always |
| 3 | 2 | HEAD_FLAGS | always |
| 5 | 2 | HEAD_SIZE | always |
| 7 | 4 | DATA_CRC | flag 0x0002 |
| next | 2 | VOLUME_NUMBER | flag 0x0008 |
| next | 7 | reserved, zero | flag 0x0004 |

Flags:

| Bit | Meaning | Evidence |
|:--|:--|:--|
| 0x0001 | **Another volume follows** this one. | set on every volume but the last in 13 sets (185 blocks), clear on the last |
| 0x0002 | DATA_CRC is present: the CRC-32 of the **whole volume file from its first byte up to, not including, this block** (for a self-extracting first volume this includes the module). | recomputed and equal in 194 of 194 blocks |
| 0x0004 | Seven reserved bytes are present. | always together with 0x0002 and 0x0008 in genuine blocks; the bytes are zero |
| 0x0008 | VOLUME_NUMBER is present: the zero-based index of this volume in its set. | equal to the volume's position in 194 of 194 blocks |
| 0x4000 | Always set. | |

Observed values: 0x4000 (HEAD_SIZE 7; single archives, and last volumes
from RAR 3.00), 0x4001 (RAR 3.00, volume with a successor), 0x400F
(HEAD_SIZE 20; RAR 6.24 and WinRAR 4.x, volume with a successor), 0x400E
(the same, last volume).

Rules:

- DECISION D20. The end block ends the walk of its volume. **Nothing
  after it is read**: recovery data added later, padding (18 zero bytes
  were seen behind the end block of a genuine volume with a recovery
  record) and anything else, including bytes that look like more blocks
  (`lsar`, 7-Zip and `bsdtar` all ignore a file header placed after the
  end block; test vector A3).
- The end block has no data area (4.3).
- Optional fields that do not fit in HEAD_SIZE are treated as absent.
- Verifying DATA_CRC is optional (it requires reading the whole volume).
  If it is verified, a mismatch is a finding on the volume, not an error.
- An end block is not required. A volume that simply ends after a block
  is complete (4.5).

## 12. Volumes

### 12.1 Structure of a set

- Every volume is a complete archive file: signature, archive header,
  blocks, optional end block. Archive header flag 0x0001 is set in every
  volume of a set. The other archive flags (solid, naming scheme,
  recovery record, encrypted headers) are repeated in every volume; flag
  0x0100 is set in the first volume only, by RAR 3.0 and later.
- A member that does not fit continues in the next volume. Each part has
  its own file header; all parts repeat the same name, U, FTIME, ATTR,
  HOST_OS, UNP_VER, METHOD and extended time, and the same flags except
  bits 0x0001 and 0x0002. Bit 0x0002 is set on every part but the last,
  bit 0x0001 on every part but the first. PACK_SIZE is the size of the
  part in its volume. (198 continuation parts compared field by field.)
- A part with bit 0x0002 is the **last file header** of its volume (a
  recovery record and the end block may follow it); a part with bit
  0x0001 is the **first file header** of its volume. (198 of 198.)
- The packed bytes of the member are the parts' data areas concatenated
  in volume order, with nothing added, removed or aligned at the joints.
- Only the first volume of a set is self-extracting (2 genuine sets;
  the later volumes are plain archives).
- Recovery volumes (files with the extension `.rev`) are not archives:
  they do not start with the signature and are ignored.

### 12.2 Volume names

The archive does not name its neighbours. The order of the volumes comes
from their **file names**, by one of two schemes; archive header flag
0x0010 says which.

**Old scheme** (flag 0x0010 clear). The first volume has the extension
`.rar` (or is a self-extracting file, `.exe`); the following volumes
replace the extension by `.r00`, `.r01`, and so on to `.r99`, then
`.s00` to `.s99`, and so on through the alphabet. Volume number v (the
first volume being 0) therefore has, for v at least 1, the extension made
of the letter that lies (v − 1) divided by 100 places after `r`, followed
by (v − 1) modulo 100 as two digits. CONFIRMED (genuine) up to `.s31`
(a set of 133 volumes) and with RAR 2.50 and 3.00; INFERRED beyond `.s99`.
DECISION D21: the scheme ends at `.z99` (volume 900); the reference
implementation accepts the same range. Extensions are compared without
regard to letter case (RAR for DOS writes `.RAR`, `.R00`).

**New scheme** (flag 0x0010 set). The volumes are named
`name.partN.rar` with N counting from 1 and written with the same number
of digits in every volume of the set (`part1` … `part6`; `part01` …
`part11`). A self-extracting first volume has another extension
(`name.part1.exe`; RAR for Unix writes `.sfx`) while the others end in
`.rar`. The next volume's name is found by taking the **last run of
decimal digits in the name before the extension** and adding 1, keeping
the width (and widening only if the number no longer fits). CONFIRMED
(genuine): 9 sets; CONFIRMED (real data): 1 set.

DECISION D28. The naming flag is advice, not proof: bit 0x0010 had
another use before RAR 3.0 (5.1), and files get renamed. When the name
the flagged scheme predicts does not exist, or the first volume's own name
does not fit the flagged scheme (for the new scheme: no decimal digit
before the extension), the reader may try the other scheme. Whatever file
is found is used only if it passes the checks of 12.3, so a wrong guess
costs nothing.

Name vectors (volume 0 is the first):

| Scheme | Given | Next or n-th name |
|:--|:--|:--|
| old | `set.rar`, volume 1 | `set.r00` |
| old | `set.rar`, volume 100 | `set.r99` |
| old | `set.rar`, volume 101 | `set.s00` |
| old | `set.rar`, volume 132 | `set.s31` (genuine, the last of 133 volumes) |
| old | `set.rar`, volume 900 | `set.z99`; there is no volume 901 |
| old | `SET.EXE`, volume 1 | `SET.R00` (genuine, RAR 2.50) |
| new | `a.part1.rar` | `a.part2.rar` |
| new | `a.part09.rar` | `a.part10.rar` |
| new | `a.part9.rar` | `a.part10.rar` (widened) |
| new | `v2.part01.rar` | `v2.part02.rar` (the last digit run, not the `2` of `v2`) |
| new | `a.part1.exe`, `a.part1.sfx` | `a.part2.rar` |

How volumes are located (by computing the next name, or by matching the
names in a directory) is the caller's business. This document requires
only that volumes are processed in the order the scheme defines.

### 12.3 Reading a set

DECISION D22:

- If the first volume the reader is given has archive flag 0x0001 clear,
  it is a single archive; no other file is consulted.
- Otherwise, after walking a volume, the reader asks for the next volume
  when that volume's end block has flag 0x0001 set, **or** when the
  volume has no end block at all (RAR 2.x sets have none, and nothing
  else in the volume says whether a successor exists). It does not ask
  when an end block is present with flag 0x0001 clear.
- It never asks after a volume whose walk stopped at a corrupt header or
  a truncated block (4.5).
- A next volume that cannot be found ends the set. That is normal after
  a volume without end block whose last file header has bit 0x0002
  clear; otherwise it leaves a member incomplete (13.2) or, when the end
  block promised a successor, earns the finding "volume missing".
- A next volume must pass the archive header check and have flag 0x0001
  set; otherwise it is not used and the set ends there with a finding.
  **A next volume whose archive header has flag 0x0080 (encrypted
  headers) is never walked**, whatever the first volume said: its blocks
  are ciphertext. It is not used and the set ends there with the finding
  "encrypted volume" (test vector V7). Differences in the other archive
  flags, or an end-block VOLUME_NUMBER that differs from the volume's
  position, are findings only.
- The signature of a next volume is looked for exactly as in section 3
  (offset 0, else the search of D1); every volume has its own B.
- A volume read on its own, without its neighbours, is a valid input:
  the members that are complete inside it are readable (subject to
  15.3), the others are skipped entries.
- Whether the first volume given really is the first of its set is known
  when its archive header has flag 0x0100, or its end block has
  VOLUME_NUMBER 0; it is known not to be when VOLUME_NUMBER is present
  and not 0, or when its first file header has bit 0x0001. In all other
  cases (RAR 2.x sets) it is assumed to be the first.

## 13. Entries: joining, checking, reading

### 13.1 One part or several

The parts of all volumes, in order, are turned into entries:

- a part with neither bit 0x0001 nor 0x0002 is a complete member;
- a part with bit 0x0002 and without 0x0001 opens a member; parts with
  both bits continue it; a part with 0x0001 and without 0x0002 completes
  it.

A continuation part belongs to the open member when its raw name field
(all NAME_SIZE bytes) and its U are equal to those of the previous part.

The entry takes its properties from the header of its **last** part
(which is the one carrying the checksum of the whole member, 13.3), and
P is the sum of the parts' data sizes.

### 13.2 Incomplete members

DECISION D23. The member being collected is closed as a skipped entry
**"missing volume part"** when:

- a part without bit 0x0001 arrives while a member is open (the open
  member is closed; the new part is then treated normally);
- a part with bit 0x0001 arrives whose name or U differs from the open
  member's (the open member is closed; the new part is treated as the
  next case);
- a part with bit 0x0001 arrives while no member is open: it becomes an
  entry of its own that is "missing volume part", collecting further
  parts until one without bit 0x0002;
- the volumes end while a member is open.

A part that was truncated (4.5) closes its member as a skipped entry
**"truncated"**.

Such an entry is listed with the properties of the last part seen. If it
is a packed member it also breaks its chain (15.3).

### 13.3 Size and checksum

What FILE_CRC holds is specified in
[`old-container.md`](old-container.md) section 6.2 and is restated here
in one table, with the additional evidence of this survey:

| Header | FILE_CRC | Evidence added here |
|:--|:--|:--|
| Complete member in one part, or **last** part | CRC-32 of the complete **unpacked** member. | 163 stored members recomputed, 7 of them split |
| Non-final part, unpack version 20 and above | CRC-32 of that part's **data area as stored** (packed, and if encrypted, enciphered), or the constant 0xFFFFFFFF. | 198 of 198 non-final parts from RAR 2.50, 3.00, 6.24 and WinRAR hold the CRC of the stored bytes, including parts of encrypted members |
| Non-final part, unpack version below 20 | The constant 0xFFFFFFFF. | (old-container.md) |
| Directory, empty member | 0. | 108 directories |

Rules for delivering contents (they repeat
[`old-container.md`](old-container.md) decisions V7 and rule 3 of its
section 6.2, and add the last one):

1. The result must be exactly U bytes: for a packed member the decoder
   must have produced U bytes; for a stored member section 14 applies.
2. The CRC-32 of the U bytes must equal FILE_CRC of the **last** part.
   Otherwise the entry is skipped: "checksum mismatch". Nothing
   unverified is delivered as good contents.
3. A non-final part's FILE_CRC, when the member's unpack version is 20 or
   more and the value is not 0xFFFFFFFF, should be compared with the
   CRC-32 of that part's data area; a mismatch makes the entry "corrupt"
   before any decoding. Optional for packed members, **required for
   stored members** (DECISION D24; it costs nothing there because the
   bytes are read anyway). 7-Zip applies the comparison also when the
   value is 0xFFFFFFFF and then reports a CRC error on archives of the
   RAR 1.5 generation (test vector V3); that is wrong.
4. An empty member (U = 0) has no contents and needs no checksum
   comparison; its data area, which is not always empty (15.2), is not
   read by the container layer.

### 13.4 What an entry is: order of the tests

Several properties of a header can apply at once (an encrypted directory,
a stored member with an unknown unpack version, a nameless packed
member). DECISION D27: the tests are made in this order and the first
that applies decides.

| Order | Test | Result |
|--:|:--|:--|
| 1 | The member is incomplete or truncated (13.2). | skipped entry "missing volume part" or "truncated" |
| 2 | NAME_SIZE is 0, or the decoded name is empty (D8). | skipped entry "empty name" |
| 3 | The member is a directory (6.5). | directory; no contents; method, version, encryption flag and CRC are ignored |
| 4 | File flag 0x0004 is set (16.1). | skipped entry "encrypted" |
| 5 | METHOD is 0x30. | stored member (14); UNP_VER is ignored |
| 6 | METHOD is 0x31 to 0x35 and UNP_VER is one of 13, 15, 20, 26, 29, 36. | packed member (15) |
| 7 | METHOD is 0x31 to 0x35, any other UNP_VER. | skipped entry "unsupported unpack version N" |
| 8 | Any other METHOD. | skipped entry "unsupported method" |

Whether the entry is a file or a symbolic link (6.5) is independent of
this table: a link is a file whose contents are its target, and it is
readable or skipped by the same tests.

For chains (15.2) the members that take part are those that are not
directories and whose method byte is not 0x30, whichever row decided
them. One that reaches row 6 is decodable. One stopped at row 1, 2, 4, 7
or 8 is an unreadable member of the sequence: it breaks the chain for
the members that continue it (15.3).

Two entries may carry the same name (RAR does not prevent it, and file
versions without flag 0x0800 look like that). Each is listed; none
replaces another.

## 14. Stored members

A member with METHOD 0x30 that is not a directory and is not encrypted:

- Its contents are the P bytes of its data areas, in volume order.
- **P must equal U.** Otherwise the entry is skipped: "corrupt" (DECISION
  D25). The reader must not deliver min(P, U) bytes: `unar` reports "OK"
  for a stored member declared as 100 bytes that has 18, and extracts 18
  (test vector R5); with P greater than U the tools truncate and then
  fail the checksum.
- The checks of 13.3 apply: per-part checksums of non-final parts, then
  the CRC-32 of all U bytes against the last part's FILE_CRC.
- A stored member can be delivered as a stream: memory use is independent
  of U. Since nothing unverified may be delivered, the implementation
  either checks the CRC-32 in a first pass over the data areas, or
  delivers through a mechanism that reports the mismatch at the end of
  the stream as an error for the entry; which one is a property of the
  repository's loader, not of the format.
- A stored member is never part of a chain (15.2), also when its header
  has the chain bit set.
- UNP_VER is ignored for stored members.
- A stored member with file flag 0x0004 is encrypted and unreadable
  (16.1).

CONFIRMED (genuine): 142 stored members from RAR 2.50 and 6.24, 5 of them
split over 2 to 133 volumes; CONFIRMED (real data): 21, 2 of them split.

## 15. Packed members: decoder selection and solid chains

### 15.1 Selecting the decoder

The unpack-version byte alone selects the decoder, per member, as
specified in [`old-container.md`](old-container.md) section 5.2:

| UNP_VER | Decoder |
|--:|:--|
| 13, 15 | RAR 1.5 method, [`rar15.md`](rar15.md) |
| 20, 26 | RAR 2.0 method, [`rar20.md`](rar20.md) |
| 29, 36 | RAR 2.9/3.x method, `rar29-lz.md` |
| other | none: skipped entry "unsupported unpack version N" |

Values observed in this survey: 15 (2 headers), 20 (481), 29 (440). The
reference implementation accepts exactly 15, 20, 26, 29 and 36.

### 15.2 Which members share decoder state

The method layer is given the packed members of the archive **in archive
order**, each with a yes/no statement "continues the chain of the
preceding packed member". The container layer derives that statement as
follows.

**Who takes part.** Only members that are not directories and whose
METHOD is not 0x30 (packed members, and members with an unsupported
method byte, which are unreadable members of the sequence; see 13.4);
for unpack versions below 20, additionally only those with U greater
than 0 ([`old-container.md`](old-container.md) section 5.3).
Directories, stored members, sub-blocks and comments are invisible to
chains: they neither continue nor interrupt one. Test vectors C1 and C2
(18.4).

- For stored members this replaces the guess of
  [`old-container.md`](old-container.md) decision V5 and `rar20.md`
  section 10.2 by an observation, for unpack version 29: CONFIRMED
  (genuine and crafted). RAR 6.24 wrote a solid archive of a packed text
  file, a stored file of 5,000 random bytes (with chain bit 0x0010 set in
  its header), a packed copy of the same 5,000 bytes, and a packed text
  file. The packed copy occupies 5,060 bytes, so the stored bytes were
  not available to the packer as history. After **removing the stored
  member's block** from the archive, `unar` decodes the two members that
  follow it with correct CRC-32: the state they continue is the one left
  by the packed member before the stored one. (On the unmodified archive
  `unar` fails on them, section 20.)
- **Packed members of version 20 and above with U = 0 do take part.**
  They are handed to the method layer like any other, with their packed
  bytes, because what those bytes mean differs by method: in a RAR 2.0
  chain the first member's packed bytes hold the initial tables even
  when U is 0 (`rar20.md` section 10.2; RAR 2.50 wrote such a member, 44
  packed bytes for an empty file, as the first file of a genuine solid
  archive here); for version 29 see `rar29-lz.md` (genuine empty members
  there have 2 or 8 packed bytes). The container layer must not drop
  them. The entry itself is an empty file whatever the decoder does with
  those bytes; a decoder error on such a member breaks the chain (15.3)
  but the empty entry stays readable.

**The statement.**

- Unpack version 20 and above: the member continues the chain when its
  file header flag 0x0010 is set. The archive header's solid flag is not
  consulted.
- Unpack version below 20: [`old-container.md`](old-container.md)
  section 5.3 (the archive header's flag 0x0008 decides; the first packed
  member starts the chain).

In genuine solid archives of version 20 and above the first packed member
has bit 0x0010 clear and every later packed member has it set (and so do
the stored members and directories written between them, where it means
nothing).

**Across volumes.** A chain runs across volume boundaries: the chain bit
of the first member of a later volume refers to the last packed member of
the earlier volume. For a split member the bit is the same in all parts.

### 15.3 When a chain cannot be continued

These rules restate, for the container's side, decisions V4 and V8 of
[`old-container.md`](old-container.md) and section 10.3 of `rar20.md`:

- A member that continues a chain needs the state left by the nearest
  preceding packed member. It is a skipped entry **"solid predecessor
  unavailable"** when that member does not exist (no packed member
  precedes it in the volumes given), when it belongs to a different
  decoder (15.1 maps both to different rows), or when the reader knows
  that volumes before the first one given are missing (12.3).
- When a packed member cannot be decoded (encrypted; unsupported method
  or version; missing volume part; truncated; corrupt per-part checksum;
  the decoder reports corrupt input), every later member that continues
  its chain is skipped, "depends on an unreadable earlier entry", up to
  the next packed member that does not continue. A mere "checksum
  mismatch" of a member that decoded to exactly U bytes does not break
  the chain.
- To deliver one member of a chain, all packed members before it in the
  chain are decoded in order, wanted or not.
- Stored members and directories stay readable whatever happens to the
  chains around them.

## 16. Encryption: recognising and skipping

No cipher is specified and no password is accepted
([`old-container.md`](old-container.md) section 7 has the background by
generation).

### 16.1 Encrypted members

- File header flag 0x0004 marks the member's data as encrypted. The entry
  is listed normally (name, sizes, times are readable) and is a skipped
  entry "encrypted". Its data area is stepped over like any other; its
  packed or stored bytes are never given to a decoder.
- Flag 0x0400 (a salt after the name) accompanies flag 0x0004 in archives
  from RAR 3.0 on. The salt lies inside HEAD_SIZE and needs no handling.
- Observed facts, for orientation only: P of an encrypted member (all
  parts added) is a multiple of 16 in archives from RAR 2.50, 3.00, 3.93
  and 6.24 (65 members; an empty file has 16 bytes), while the parts of a
  split member are cut at arbitrary positions; U and FILE_CRC of the last
  part describe the plain contents. A reader must not reject an entry on
  these grounds.
- An encrypted packed member breaks its chain (15.3). In a genuine solid
  archive made with a password every member is encrypted.
- An encrypted comment or sub-block was not observed; flag 0x0004 on a
  sub-block makes it one more block to step over.

### 16.2 Encrypted headers

Archive header flag 0x0080 (RAR 3.0 and later, the "encrypt file names"
option): everything after the archive header is enciphered, block headers
included. Nothing after offset B + 7 + HEAD_SIZE can be interpreted: each
block is preceded by an 8-byte salt and its header is enciphered in
16-byte units (REFERENCE-ONLY).

DECISION D26. The walk stops after the archive header. The archive-level
result is "encrypted archive: the file list cannot be read", with no
entries. The reader must not try to parse the following bytes as blocks.
In a multi-volume set every volume has the flag; the archive headers
themselves stay readable (9 genuine volumes). The flag is honoured per
volume: a later volume that has it is never walked, even when the first
volume did not have it (12.3). `lsar` asks for a password,
`bsdtar` reports that encryption is unsupported, 7-Zip lists nothing.

## 17. Worked examples

### 17.1 A genuine archive, block by block

Test vector G1 (section 18.1), 246 bytes, written by RAR 6.24 without
compression from two files and one empty directory.

| Offset | Bytes | Meaning |
|--:|:--|:--|
| 0 | `52 61 72 21 1A 07 00` | Signature. |
| 7 | `CF 90` | Archive header CRC-16 0x90CF = low 16 bits of the CRC-32 of the 11 bytes at offsets 9 to 19. |
| 9 | `73` | Type: archive header. |
| 10 | `00 00` | Flags 0. |
| 12 | `0D 00` | Header size 13. |
| 14 | `00 00 00 00 00 00` | Reserved. |
| 20 | `34 40` | File header CRC-16 0x4034, over offsets 22 to 69. |
| 22 | `74` | Type: file header. |
| 23 | `20 90` | Flags 0x9020: 0x8000, extended time (0x1000), dictionary number 1. |
| 25 | `32 00` | Header size 50: the data area starts at 70. |
| 27 | `18 00 00 00` | PACK_SIZE 24. |
| 31 | `18 00 00 00` | UNP_SIZE 24. |
| 35 | `03` | Host: Unix. |
| 36 | `00 88 59 0B` | FILE_CRC 0x0B598800. |
| 40 | `A3 20 43 2A` | FTIME 0x2A4320A3 (17.3). |
| 44 | `14` | Unpack version 20. |
| 45 | `30` | Method: stored. |
| 46 | `05 00` | NAME_SIZE 5. |
| 48 | `A4 81 00 00` | ATTR 0x81A4: regular file, permissions 644 octal. |
| 52 | `61 2E 74 78 74` | Name `a.txt` (no Unicode flag). |
| 57 | `C0 CF 0F 41 6D 5D EF 2E 42 A3 20 43 2A` | Extended time, 13 bytes (17.3). |
| 70 | 24 bytes | Data: ASCII `hello hello hello hello` and a line feed. CRC-32 0x0B598800. |
| 94 | `6D A7 74 20 92 4D 00` | CRC-16 0xA76D, file header, flags 0x9220 (as before plus Unicode name), header size 77. |
| 101 | `09 00 00 00 09 00 00 00 03` | PACK_SIZE 9, UNP_SIZE 9, host Unix. |
| 110 | `50 7F 19 51` | FILE_CRC 0x51197F50. |
| 114 | `0F 41 6D 5D 14 30` | FTIME 0x5D6D410F, version 20, stored. |
| 120 | `1A 00 A4 81 00 00` | NAME_SIZE 26, ATTR 0x81A4. |
| 126 | 26 bytes | Name field in the Unicode form (17.2). |
| 152 | 19 bytes | Extended time: `F0 FF`, then `C9 0B 40`, then `0F 41 6D 5D C9 0B 40`, then `19 41 6D 5D 22 09 5F`. |
| 171 | `63 6A 6B 20 6E 61 6D 65 0A` | Data, 9 bytes. |
| 180 | `20 1B 74 E0 90 3B 00` | CRC-16 0x1B20, file header, flags 0x90E0: **directory**; header size 59. |
| 187 | `00 00 00 00 00 00 00 00 03 00 00 00 00` | PACK_SIZE 0, UNP_SIZE 0, host Unix, FILE_CRC 0. |
| 200 | `0F 41 6D 5D 14 30 08 00 ED 41 00 00` | FTIME, version 20, method 0x30, NAME_SIZE 8, ATTR 0x41ED (directory, 755). |
| 212 | `65 6D 70 74 79 64 69 72` | Name `emptydir`. |
| 220 | 19 bytes | Extended time. |
| 239 | `C4 3D 7B 00 40 07 00` | End block: CRC-16 0x3DC4, flags 0x4000, header size 7. The volume ends at 246. |

Expected listing:

| Name | Kind | U | CRC-32 | Modified | Created / changed | Accessed |
|:--|:--|--:|:--|:--|:--|:--|
| `a.txt` | file | 24 | 0x0B598800 | 2001-02-03 04:05:07.0000000 | 2026-11-13 08:08:31.4337391 | 2001-02-03 04:05:07.0000000 |
| `日本語.txt` | file | 9 | 0x51197F50 | 2026-11-13 08:08:31.4197321 | 2026-11-13 08:08:31.4197321 | 2026-11-13 08:08:51.6228258 |
| `emptydir` | directory | 0 | | 2026-11-13 08:08:31.3830010 | 2026-11-13 08:08:31.3830162 | 2026-11-13 08:08:31.3830010 |

(All times are local times of the archiving machine. 7-Zip prints exactly
these values; `lsar` prints the DOS time only.)

### 17.2 A Unicode name, item by item

First example: the 26-byte name field of the second file above.

`E6 97 A5 E6 9C AC E8 AA 9E 2E 74 78 74 00 65 68 E5 2C 67 9E 8A 2E 00 74 78 74`

- The first zero byte is at Z = 13. The plain name is bytes 0 to 12:
  `E6 97 A5 E6 9C AC E8 AA 9E 2E 74 78 74`, which is `日本語.txt` in
  UTF-8.
- The encoded name is the 12 bytes after the zero:
  `65 68 E5 2C 67 9E 8A 2E 00 74 78 74`.
- High byte H = 0x65.
- Flag byte 0x68 = binary 01 10 10 00: modes 1, 2, 2, 0.
  - Mode 1, byte `E5`: unit 0x65 × 256 + 0xE5 = 0x65E5 (日).
  - Mode 2, bytes `2C 67`: unit 0x672C (本).
  - Mode 2, bytes `9E 8A`: unit 0x8A9E (語).
  - Mode 0, byte `2E`: unit 0x002E (`.`).
- Flag byte 0x00: modes 0, 0, 0, 0.
  - `74`, `78`, `74`: units `t`, `x`, `t`. The data is now exhausted;
    the fourth mode of this flag byte is unused.
- Result: 7 units, `日本語.txt`.

Second example, with a run (field U6 of section 18.2, from a genuine
header): plain name `dir\Δελτα file.txt` in UTF-8 (23 bytes), a zero
byte, then the encoded name

`03 D5 02 94 B5 BB 50 C4 B1 20 66 00 69 6C 65 2E 00 74 78 74`

- H = 0x03.
- Flag byte 0xD5 = binary 11 01 01 01: modes 3, 1, 1, 1.
  - Mode 3, length byte `02`: bit 7 clear, so a run of 2 + 2 = 4 units
    without correction. No units exist yet, so the run takes plain-name
    bytes 0, 1, 2, 3: `64 69 72 5C`, the units `d`, `i`, `r`, `\`.
  - Mode 1, `94`: 0x0394 (Δ). Mode 1, `B5`: 0x03B5 (ε). Mode 1, `BB`:
    0x03BB (λ).
- Flag byte 0x50 = binary 01 01 00 00: modes 1, 1, 0, 0.
  - `C4`: 0x03C4 (τ). `B1`: 0x03B1 (α). `20`: space. `66`: `f`.
- Flag byte 0x00: `69 6C 65 2E` give `i`, `l`, `e`, `.`.
- Flag byte 0x00: `74 78 74` give `t`, `x`, `t`; end of data.
- Result: 18 units, `dir\Δελτα file.txt`; split at the backslash it is
  the path `dir` / `Δελτα file.txt`.

Third example, a run with correction (field U8, crafted): plain name
`10 11 12 2E 74 78 74`, zero byte, encoded name `04 F0 81 00 02`.

- H = 0x04. Flag byte 0xF0 = binary 11 11 00 00: modes 3, 3, 0, 0.
- Mode 3, length byte `81`: bit 7 set, so a correction byte follows:
  `00`. Run length (0x81 masked with 0x7F) + 2 = 3. Units: for k = 0, 1,
  2 the plain bytes 0x10, 0x11, 0x12, plus correction 0, with high byte
  4: 0x0410, 0x0411, 0x0412 (the Cyrillic letters А, Б, В).
- Mode 3, length byte `02`: no correction, run of 4. Three units exist,
  so the run takes plain bytes 3, 4, 5, 6: `.`, `t`, `x`, `t`.
- The data is exhausted. Result: `АБВ.txt`.

### 17.3 Times, bit by bit

FTIME of the first file, 0x2A4320A3, in binary
`0010101 0010 00011 00100 000101 00011`: years since 1980 = 21 (2001),
month 2, day 3, hour 4, minute 5, seconds field 3 (6 seconds):
2001-02-03 04:05:06.

Its extended time field `C0 CF 0F 41 6D 5D EF 2E 42 A3 20 43 2A`:

- Flags word 0xCFC0: groups 0xC (modification), 0xF (creation), 0xC
  (access), 0x0 (archiving).
- Modification, group 0xC = present, add one second, 0 fraction bytes:
  no bytes are read; the time is FTIME plus 1 second:
  2001-02-03 04:05:07.0000000.
- Creation, group 0xF = present, add one second, 3 fraction bytes: DOS
  time `0F 41 6D 5D` = 0x5D6D410F, binary
  `0101110 1011 01101 01000 001000 01111`: 2026-11-13 08:08:30; fraction
  bytes `EF 2E 42` give F = 0x422EEF = 4,337,391; plus one second:
  2026-11-13 08:08:31.4337391.
- Access, group 0xC: DOS time `A3 20 43 2A`, no fraction bytes, plus one
  second: 2001-02-03 04:05:07.0000000.
- Archiving, group 0: absent. All 13 bytes are used.

## 18. Test vectors

Hexadecimal, 32 bytes per line. "Crafted" vectors were built by hand from
this document; "genuine" ones were written by RAR. For each, the listing
produced by the author's throwaway reader (written from this document) is
the expected result, and the black-box tools named agree with it unless a
difference is stated.

`A` is the 18 bytes `hello hello hello` plus a line feed (CRC-32
0xDF8A7C3B); `B` is the 12 bytes `second file` plus a line feed (CRC-32
0xE472FF82). Crafted headers use FTIME 0x2A4320A3 (2001-02-03 04:05:06),
host 0, attributes 0x20, unpack version 20 and method 0x30 unless stated.

### 18.1 Whole archives that must be read

**G1** (genuine, RAR 6.24; 246 bytes; walked in 17.1; listed identically
by `lsar`, 7-Zip and `bsdtar`, extracted by all three):

```
526172211a0700cf907300000d00000000000000344074209032001800000018
000000030088590ba320432a14300500a4810000612e747874c0cf0f416d5def
2e42a320432a68656c6c6f2068656c6c6f2068656c6c6f2068656c6c6f0a6da7
7420924d00090000000900000003507f19510f416d5d14301a00a4810000e697
a5e69cace8aa9e2e747874006568e52c679e8a2e00747874f0ffc90b400f416d
5dc90b4019416d5d22095f636a6b206e616d650a201b74e0903b000000000000
00000003000000000f416d5d14300800ed410000656d707479646972f0fffa70
3a0f416d5d92713a0f416d5dfa703ac43d7b00400700
```

**A1** (crafted; 128 bytes; no end block): two stored files. Expected:
`A.TXT` (contents `A`) and `DIR/B.TXT` (contents `B`). `unar`, 7-Zip and
`bsdtar` extract both. The second file header starts at offset 75.

```
526172211a0700cf907300000d000000000000004c5174008025001200000012
000000003b7c8adfa320432a1430050020000000412e54585468656c6c6f2068
656c6c6f2068656c6c6f0abe4174008029000c0000000c0000000082ff72e4a3
20432a14300900200000004449525c422e5458547365636f6e642066696c650a
```

**V1** (crafted; two volumes, new naming scheme, end blocks with data
CRC and volume number). `BIG.BIN` is the 40 bytes whose byte i is
(7 × i + 3) modulo 256 (CRC-32 0xE730B7EA), split 25 + 15.

Volume `v1.part1.rar`, 152 bytes: archive flags 0x0111; `A.TXT`; part 1
of `BIG.BIN` at offset 75 (flags 0x8002, PACK_SIZE 25, UNP_SIZE 40,
FILE_CRC 0xA5A7D437 = CRC-32 of the 25 stored bytes); end block at 139
(flags 0x400B, DATA_CRC 0xC603FFDD = CRC-32 of bytes 0 to 138, volume
number 0).

```
526172211a07005a6e7311010d000000000000004c5174008025001200000012
000000003b7c8adfa320432a1430050020000000412e54585468656c6c6f2068
656c6c6f2068656c6c6f0ad402740280270019000000280000000037d4a7a5a3
20432a14300700200000004249472e42494e030a11181f262d343b424950575e
656c737a81888f969da4ab1b047b0b400d00ddff03c60000
```

Volume `v1.part2.rar`, 136 bytes: archive flags 0x0011; part 2 of
`BIG.BIN` at offset 20 (flags 0x8001, PACK_SIZE 15, FILE_CRC 0xE730B7EA);
`B.TXT` at 74; end block at 123 (flags 0x400A, DATA_CRC 0x3D24A8BE,
volume number 1).

```
526172211a0700197a7311000d00000000000000486674018027000f00000028
00000000eab730e7a320432a14300700200000004249472e42494eb2b9c0c7ce
d5dce3eaf1f8ff060d14e63674008025000c0000000c0000000082ff72e4a320
432a1430050020000000422e5458547365636f6e642066696c650afd3b7b0a40
0d00bea8243d0100
```

Expected with both volumes: `A.TXT`, `BIG.BIN` (40 bytes, verified),
`B.TXT`; `unar` and 7-Zip extract all three. Expected with the second
volume alone (**V2**): `BIG.BIN` skipped as "missing volume part", `B.TXT`
readable (`lsar` marks `BIG.BIN` as corrupted). Changing the byte at
offset 91 of the first volume from `37` to `36` and its header checksum
at offsets 75 and 76 from `d4 02` to `00 e8` makes the per-part checksum
wrong (**V1x**): `BIG.BIN` must be skipped as "corrupt", `A.TXT` and
`B.TXT` stay readable (`unar` reports a wrong checksum for `BIG.BIN`).
The edit also invalidates that volume's DATA_CRC, which is a finding
only if DATA_CRC is verified.

**V3** (crafted; two volumes, old naming scheme `v3.rar` and `v3.r00`,
no end blocks, unpack version 15, non-final FILE_CRC 0xFFFFFFFF). The
same `BIG.BIN`, 25 + 15.

`v3.rar`, 84 bytes (archive flags 0x0001):

```
526172211a0700f1fb7301000d00000000000000f6b774028027001900000028
00000000ffffffffa320432a0f300700200000004249472e42494e030a11181f
262d343b424950575e656c737a81888f969da4ab
```

`v3.r00`, 74 bytes:

```
526172211a0700f1fb7301000d00000000000000f95f74018027000f00000028
00000000eab730e7a320432a0f300700200000004249472e42494eb2b9c0c7ce
d5dce3eaf1f8ff060d14
```

Expected: `BIG.BIN`, 40 bytes, CRC-32 0xE730B7EA; the 0xFFFFFFFF of the
first part is not compared with anything. `unar` extracts it; 7-Zip
extracts the right bytes but reports a CRC error (section 13.3).

**A2** (crafted; 499 bytes): every kind of block a reader must step
over, and both nested comments.

| Offset | Block | Details |
|--:|:--|:--|
| 7 | archive header | flags 0x0002, header size 36: nested comment block at offset 13 of the header (stored, version 15, 10 bytes `Hi there` CR LF, COMM_CRC 0x0D36). HEAD_CRC 0x398F covers offsets 2 to 12 only. |
| 43 | sub-block `CMT` | flags 0x8000, stored, 11 data bytes `new comment`, FILE_CRC 0x3A4782BE, ATTR 0 |
| 89 | file header `A.TXT` | flags 0x8008, header size 54: nested comment block after the name (stored, 4 bytes `note`, COMM_CRC 0xFA14). HEAD_CRC 0xB8EB covers offsets 2 to 36 only. 18 data bytes. |
| 161 | old sub-block 0x77 | flags 0xC000, header size 24, 4 data bytes |
| 189 | sub-block `UOW` | flags 0x8010, header size 45, no data |
| 234 | unknown type 0x7F | flags 0xC000, header size 14, 5 data bytes; HEAD_CRC wrong on purpose |
| 253 | unknown type 0x60 | flags 0x4000, header size 9, no data; HEAD_CRC wrong on purpose |
| 262 | file header `B.TXT` | 12 data bytes |
| 311 | old recovery record 0x78 | flags 0xC000, header size 26, 6 data bytes |
| 343 | sub-block `RR` | flags 0xC000, header size 54, 12 data bytes |
| 409 | old authenticity 0x76 | flags 0x4000, header size 30; HEAD_CRC wrong on purpose |
| 439 | old authenticity 0x79 | flags 0x4000, header size 37; HEAD_CRC wrong on purpose |
| 476 | end block | flags 0x4000 |
| 483 | 16 bytes `trailing garbage` | not read |

```
526172211a07008f397302002400000000000000074c75000017000a000f3036
0d48692074686572650d0a71967a008023000b0000000b00000000be82473aa3
20432a1d30030000000000434d546e657720636f6d6d656e74ebb87408803600
1200000012000000003b7c8adfa320432a1430050020000000412e5458546f05
750000110004000f3014fa6e6f746568656c6c6f2068656c6c6f2068656c6c6f
0a330a7700c0180004000000000100040000001430e36b186945414441d6e17a
10802d0000000000000000000000000000a320432a1d30030000000000554f57
757365720067726f757033337f00c00e000500000078797a4441544121444460
004009007071e63674008025000c0000000c0000000082ff72e4a320432a1430
050020000000422e5458547365636f6e642066696c650aecc17800c01a000600
00001400000300000050726f7465637421525244415441a06d7a00c036000c00
00000c000000002917a2fba320432a1d30020000000000525250726f74656374
2b000000000600000000000000aaaaaaaaaaaaaaaaaaaaaaaa11117600401e00
0102030405060708090a0b0c0d0e0f1011121314151617222279004025000102
030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1ec43d7b00
400700747261696c696e672067617262616765
```

Expected: exactly two entries, `A.TXT` (contents `A`) and `B.TXT`
(contents `B`), no findings. If comments are read: old archive comment
`Hi there` CR LF, file comment of `A.TXT` `note`, and the `CMT` comment
`new comment` (which of the two archive comments a reader shows when both
exist is its own choice; genuine archives have one). Checked: `unar`
extracts both files. Split into four archives with one feature group
each, 7-Zip lists both files for the nested comments and for the blocks
of genuine types, and stops at the unknown types (section 20); `bsdtar`
rejects nested comments and type 0x76 for their checksums.

**A3** (crafted; described as edits of A1). A1 followed by the end block
`c4 3d 7b 00 40 07 00`, then by a copy of bytes 75 to 127 of A1 (a
complete file header with data), then by the bytes `00 01 02`. Expected:
the two entries of A1 and nothing else, no finding; the copy after the
end block is not listed (`lsar`, 7-Zip and `bsdtar` agree).

**S1** (crafted; 139 bytes): a self-extracting archive whose module
contains the seven signature bytes. Offset 32 holds a signature followed
by a byte 0x73 and zeros (it fails the archive header check: the type
position holds 0x00); the real archive starts at B = 64 and is the first
75 bytes of A1.

```
4d5a909090909090909090909090909090909090909090909090909090909090
526172211a070073000000000000000000000000000000000000000000000000
526172211a0700cf907300000d000000000000004c5174008025001200000012
000000003b7c8adfa320432a1430050020000000412e54585468656c6c6f2068
656c6c6f2068656c6c6f0a
```

Expected: B = 64, one entry `A.TXT`. 7-Zip agrees; `lsar` fails ("data is
corrupted") because it stops at offset 32.

**A4** (crafted; 135 bytes): an encrypted member followed by a plain
one. First file header at 20: flags 0x8404, header size 50, PACK_SIZE 16,
UNP_SIZE 5, FILE_CRC 0x12345678, version 29, method 0x33, name
`SECRET.TXT`, salt `01 02 03 04 05 06 07 08`; 16 data bytes of 0x5A.

```
526172211a0700cf907300000d000000000000008a8474048432001000000005
0000000078563412a320432a1d330a00200000005345435245542e5458540102
0304050607085a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5ae63674008025000c0000
000c0000000082ff72e4a320432a1430050020000000422e5458547365636f6e
642066696c650a
```

Expected: `SECRET.TXT` (U = 5) listed and skipped as "encrypted";
`B.TXT` readable. `lsar` and 7-Zip list the first as encrypted.

**E1** (crafted from the first 28 bytes of a genuine archive; 60 bytes):
encrypted headers. Archive header flags 0x0080; the 40 bytes after it are
arbitrary.

```
526172211a0700ce997380000d00000000000000be3e8a1f2fc5d3790b30557a
9fc4e90e33587da2c7ec11365b80a5caef14395e83a8cdf2173c6186
```

Expected: the archive-level result "encrypted archive", no entries, and
no attempt to interpret offset 20 onwards (read as a block header those
bytes would give header size 0xD3C5).

**A5** (crafted; 132 bytes): flag 0x0100 with zero high fields. The
first file header has flags 0x8100 and header size 45; the name starts at
offset 40 of the header.

```
526172211a0700cf907300000d0000000000000052f37400812d001200000012
000000003b7c8adfa320432a14300500200000000000000000000000412e5458
5468656c6c6f2068656c6c6f2068656c6c6f0ae63674008025000c0000000c00
00000082ff72e4a320432a1430050020000000422e5458547365636f6e642066
696c650a
```

Expected: `A.TXT` (18 bytes) and `B.TXT`, both readable; all three tools
agree.

**A6** (crafted; 223 bytes): kinds of entries.

| Offset | Header | Expected entry |
|--:|:--|:--|
| 20 | `D1`: flags 0x80E0, version 20, host 0, ATTR 0x10 | directory `D1` |
| 54 | `D3`: flags 0x8000, version 15, method 0x33, host 1, ATTR 0x10 | directory `D3` ([`old-container.md`](old-container.md) 5.4) |
| 88 | `D1\ln`: flags 0x8000, host 3, ATTR 0xA1ED, 5 data bytes `A.TXT` | symbolic link `D1/ln` with target `A.TXT` |
| 130 | `old.txt;2`: flags 0x8800, data `A` | file named `old.txt;2` |
| 189 | `D2`: flags 0x8000, version 20, host 2, ATTR 0x10 | **file** `D2` of length 0 (D10) |

```
526172211a0700cf907300000d00000000000000747574e08022000000000000
0000000000000000a320432a14300200100000004431fd877400802200000000
00000000000100000000a320432a0f3302001000000044332db8740080250005
0000000500000003349a8d96a320432a14300500eda1000044315c6c6e412e54
5854d0a974008829001200000012000000003b7c8adfa320432a143009002000
00006f6c642e7478743b3268656c6c6f2068656c6c6f2068656c6c6f0a187774
0080220000000000000000000200000000a320432a14300200100000004432
```

Tools: `bsdtar` shows `D1/ln -> A.TXT`; 7-Zip shows `D2` and `D3` as
directories, `lsar` shows both as files; all show `old.txt;2`.

**A7** (crafted; 163 bytes): data-size rules that do not follow flag
0x8000 (4.3, D3, D6). The archive header has flags 0x8000 (to be
ignored: it has no data area and its reserved bytes are not a size);
after `A.TXT` comes a sub-block `XYZ` at offset 75 with flags 0x0000,
header size 35 and 4 data bytes `DATA` (a sub-block always has a data
area); then `B.TXT` at 114.

```
526172211a070080907300800d000000000000004c5174008025001200000012
000000003b7c8adfa320432a1430050020000000412e54585468656c6c6f2068
656c6c6f2068656c6c6f0a977b7a00002300040000000400000000d71e669ba3
20432a1d3003000000000058595a44415441e63674008025000c0000000c0000
000082ff72e4a320432a1430050020000000422e5458547365636f6e64206669
6c650a
```

Expected: `A.TXT` and `B.TXT`, both readable, no finding. Of the tools
only `bsdtar` lists both; `lsar` and 7-Zip stop after `A.TXT` (section
20).

**V5 to V7** (further readings of the V1 volumes; no new bytes except
V7's second volume).

- **V5**: `v1.part1.rar` alone, no second volume to be found. Expected:
  `A.TXT` readable; `BIG.BIN` listed (U = 40, P = 25) and skipped as
  "missing volume part"; finding "volume missing" (the end block has
  flag 0x0001).
- **V6**: the bytes of `v1.part1.rar` supplied twice, as first and as
  second volume (two different files with the same contents). Expected
  entries, in order: `A.TXT` readable; `BIG.BIN` "missing volume part"
  (closed when the second volume's `A.TXT`, a part without bit 0x0001,
  arrives); `A.TXT` readable again; `BIG.BIN` "missing volume part"
  again. Finding: the second volume's VOLUME_NUMBER is 0, not 1.
- **V7**: `v1.part1.rar` followed by this second volume, which is
  `v1.part2.rar` with archive flags 0x0091 (encrypted headers claimed)
  and the archive header checksum recomputed to 0x7318; its remaining
  bytes are unchanged and would parse as blocks:

  ```
  526172211a070018737391000d00000000000000486674018027000f00000028
  00000000eab730e7a320432a14300700200000004249472e42494eb2b9c0c7ce
  d5dce3eaf1f8ff060d14e63674008025000c0000000c0000000082ff72e4a320
  432a1430050020000000422e5458547365636f6e642066696c650afd3b7b0a40
  0d00bea8243d0100
  ```

  Expected: the second volume is not walked; `A.TXT` readable;
  `BIG.BIN` "missing volume part"; **no** `B.TXT`; finding "encrypted
  volume". Supplied on its own, this volume gives the archive-level
  result "encrypted archive".

### 18.2 Field vectors

**Name fields with the Unicode flag set.** Input: the name field; output:
the name before separator splitting. U1 to U12, U14 and U15 were checked
with `lsar` and 7-Zip inside an archive (both print the expected name);
U5 to U7 are from genuine headers. For U13 and U16 to U19 the expected
value is this document's decision; the tools' answers are noted.

| # | Name field | Expected name | Shows |
|:--|:--|:--|:--|
| U1 | `72c3a46b2e747874` | `räk.txt` | no zero byte: UTF-8 |
| U2 | `61622e74787400000061e42e74007874` | `aä.txt` | mode 0 |
| U3 | `61622e74787400035094b52e74007874` | `Δε.txt` | mode 1, H = 3 |
| U4 | `61622e7478740000a0e5652c672e74007874` | `日本.txt` | mode 2 |
| U5 | `72c3a46b736dc3b67267c3a5732e74787400010072e46b73006df6726700e5732e74007874` | `räksmörgås.txt` | genuine; H = 1 unused |
| U6 | `6469725cce94ceb5cebbcf84ceb12066696c652e7478740003d50294b5bb50c4b1206600696c652e00747874` | `dir\Δελτα file.txt` | genuine; run (17.2) |
| U7 | `e697a5e69cace8aa9e2e747874006568e52c679e8a2e00747874` | `日本語.txt` | genuine (17.2) |
| U8 | `1011122e7478740004f0810002` | `АБВ.txt` | run with correction 0, H = 4 (17.2) |
| U9 | `4142432e7478740000f0812002` | `abc.txt` | run with correction 0x20, H = 0 |
| U10 | `6162636465662e7478740000cc04e401` | `abcdefätxt` | a run of 6, a mode-0 unit, then a run of 3 that starts at plain index 7; the last mode of flag byte 0xCC is unused |
| U11 | `f0f12e740001c080202e74` | `Đđ.t` (U+0110, U+0111) | correction wraps modulo 256: 0xF0 + 0x20 gives 0x10 |
| U12 | `782e7478740000a03dd800de2e74` | U+1F600, then `.t` | surrogate pair |
| U13 | `782e7478740000803dd82e74` | U+FFFD, then `.t` | lone high surrogate (D13; 7-Zip agrees, `lsar` shows an empty name) |
| U14 | `61622e7478740005` | `ab.txt` | encoded part of 1 byte: plain name |
| U15 | `61622e74787400` | `ab.txt` | zero byte last: plain name |
| U16 | `61622e747874000000` | `ab.txt` | H and one flag byte, no items: plain name (D13; 7-Zip agrees) |
| U17 | `61620000c07f` | `ab` | run of 129 with a 2-byte plain name: malformed, plain name (D13; 7-Zip shows `ab`, `lsar` garbage) |
| U18 | `61622e7478740000206141` | `ab.txt` | mode 2 with one byte left: malformed, plain name (D13; both tools show the partial result `a`) |
| U19 | `782e747874000000610062` | `x.txt` | a zero unit: malformed, plain name (D13) |
| U20 | `72e46b2e747874` | `r`, U+FFFD, `k.txt` | no zero byte, invalid UTF-8 |

**Extended time fields.** Input: FTIME and the bytes after the name (and
salt); output: the times. X1 to X4 are from genuine headers and agree
with 7-Zip; X5 to X8 are crafted and agree with 7-Zip.

| # | FTIME | Field bytes | Modified | Created | Accessed |
|:--|:--|:--|:--|:--|:--|
| X1 | 0x2A4320A3 | `c0cf0f416d5def2e42a320432a` | 2001-02-03 04:05:07.0000000 | 2026-11-13 08:08:31.4337391 | 2001-02-03 04:05:07.0000000 |
| X2 | 0x5D6D410F | `f0ffc90b400f416d5dc90b4019416d5d22095f` | 2026-11-13 08:08:31.4197321 | 2026-11-13 08:08:31.4197321 | 2026-11-13 08:08:51.6228258 |
| X3 | 0x5D6D410F | `00f0a50440` | 2026-11-13 08:08:31.4195493 | none | none |
| X4 | 0x5D6D4174 | `800874416d5d74416d5d` | 2026-11-13 08:11:40.0000000 | 2026-11-13 08:11:40.0000000 | 2026-11-13 08:11:40.0000000 |
| X5 | 0x279FBF7D | `e0a934120060cf3c8042085d505060` | 1999-12-31 23:59:58.1192960 | 2010-06-15 12:00:00.8388608 | 2020-02-29 01:02:05.6311936 |
| X6 | 0x279FBF7D | `009077` | 1999-12-31 23:59:58.7798784 | none | none |
| X7 | 0x279FBF7D | `00c0` | 1999-12-31 23:59:59.0000000 | none | none |
| X8 | 0x279FBF7D | `0040` | 1999-12-31 23:59:58.0000000 | none | none |
| X9 | 0x279FBF7D | `e0a934120060cf3c` | 1999-12-31 23:59:58.0000000 | none | none |

Notes: X4 has no modification group (the modification time is FTIME
alone). X5 uses 2 fraction bytes (`34 12`: F = 0x123400), 1 fraction
byte (`80`: F = 0x800000) and 2 fraction bytes with the extra second
(`50 60`: F = 0x605000). X8: the "add one second" bit without the
"present" bit does nothing. X9 is X5 cut short: the access group's bytes
are missing, so the whole field is ignored (D7).

**DOS times** (D16). Computed from 8.1 and checked with a crafted
archive of ten headers: 7-Zip prints the same time for every valid value
and no time for every invalid one except T4, which it normalises to
2001-03-01; `lsar` normalises all six invalid values into other dates.

| # | FTIME | Expected |
|:--|:--|:--|
| T1 | 0x00000000 | none (month 0, day 0) |
| T2 | 0x00210000 | 1980-01-01 00:00:00 |
| T3 | 0x285D6000 | 2000-02-29 12:00:00 |
| T4 | 0x2A5D0000 | none (29 February 2001) |
| T5 | 0x2A43C000 | none (hour 24) |
| T6 | 0x2A430780 | none (minute 60) |
| T7 | 0x2A43001E | none (seconds field 30) |
| T8 | 0x2A43001D | 2001-02-03 00:00:58 |
| T9 | 0x2BA10000 | none (month 13) |
| T10 | 0xFF9FBF7D | 2107-12-31 23:59:58 |

**File header with 64-bit sizes** (H1; genuine, RAR 6.24; the 54 header
bytes only). Expected: flags 0x91C0, header size 54, data size 2,306,712,
U = 4,613,734,400, host 3, FILE_CRC 0xDA138266, version 29, method 0x31,
name `zeros.bin`, ATTR 0x8180, extended time `00 B0 D0 4D 2D`; HEAD_CRC
0x0D47 is the CRC-16 of bytes 2 to 53. `lsar`, 7-Zip and `bsdtar` print
the same two sizes for the archive it came from.

```
470d74c0913600983223000000001303668213da53456d5d1d31090080810000
00000000010000007a65726f732e62696e00b0d04d2d
```

**Header checksums.** CRC-16 of `73 00 00 0d 00 00 00 00 00 00 00` is
0x90CF (the archive header of every single-volume archive without
flags). CRC-16 of `7b 00 40 07 00` is 0x3DC4 (the plain end block).

### 18.3 Archives that must be rejected or partly rejected

All are edits of A1 (128 bytes; file headers at 20 and 75). "Recomputed"
means: after the edit, the two HEAD_CRC bytes of the edited header are
replaced by the value given.

| # | Edit | Expected result | Tools |
|:--|:--|:--|:--|
| R1 | bytes 20, 21 := `34 12` (wrong file header checksum) | no entries; finding "corrupt header at offset 20" | 7-Zip, `bsdtar`: header error; `lsar` lists both files |
| R2 | append `00 00 74 00 80 06 00` (header size 6) | both entries; finding "corrupt header at offset 128" | all report an error after two files |
| R3 | bytes 101, 102 := `f4 01` (NAME_SIZE 500); bytes 75, 76 := `d3 f8` (recomputed) | `A.TXT` only; "corrupt header at offset 75" | `bsdtar`: invalid filename size |
| R4 | remove the last 5 bytes | `A.TXT` readable; `DIR/B.TXT` listed and skipped as "truncated"; finding "truncated" | 7-Zip: unexpected end of archive |
| R5 | bytes 31 to 34 := `64 00 00 00` (U = 100, P = 18); bytes 20, 21 := `17 00` | `A.TXT` skipped as "corrupt" (stored, P differs from U); `DIR/B.TXT` readable | `unar` reports OK and writes 18 bytes |
| R6 | bytes 36 to 39 := `01 00 00 00` (FILE_CRC 1); bytes 20, 21 := `15 a5` | `A.TXT` skipped as "checksum mismatch"; `DIR/B.TXT` readable | `unar`, 7-Zip: wrong checksum |
| R7 | remove bytes 7 to 19 (no archive header) | archive-level error "corrupt archive" | 7-Zip refuses; `lsar`, `bsdtar` list both files |
| R8 | bytes 7, 8 := `34 12` (wrong archive header checksum) | archive-level error "corrupt archive" | 7-Zip, `bsdtar` refuse; `lsar` lists |
| R9 | insert a copy of bytes 7 to 19 at offset 75 (second archive header) | `A.TXT` only; "corrupt header at offset 75" | 7-Zip: header error |
| R10 | append `00 01 02` | both entries; finding "stray bytes after the last block" | 7-Zip: unexpected end |
| R11 | byte 45 := `36` (method 0x36); bytes 20, 21 := `ac f4` | `A.TXT` skipped as "unsupported method"; `DIR/B.TXT` readable | |
| R12 | byte 44 := `15`, byte 45 := `33` (version 21, packed); bytes 20, 21 := `39 5a` | `A.TXT` skipped as "unsupported unpack version 21"; `DIR/B.TXT` readable | |
| R13 | byte 24 := `00` (file header flags 0x0000: bit 0x8000 cleared); bytes 20, 21 := `99 53` | both entries readable: a file header has a data area regardless of the bit | all three tools list and extract both |
| R14 | remove bytes 52 to 56 (the name `A.TXT`); byte 25 := `20` (header size 32); byte 46 := `00` (NAME_SIZE 0); bytes 20, 21 := `72 4d` | first entry skipped as "empty name" (D8), its 18 data bytes stepped over; `DIR/B.TXT` readable | `lsar` shows `.`; 7-Zip an empty name; `bsdtar` skips it |
| R15 | byte 24 := `94` (flags 0x9400: salt and extended time claimed); insert `00 f0 01 02` after the name (at offset 57); byte 25 := `29` (header size 41); bytes 20, 21 := `99 8a` | `A.TXT` readable (not encrypted: flag 0x0004 is clear), modified 2001-02-03 04:05:06, no other times; one finding: the salt does not fit, so salt and extended time are absent (D7); `DIR/B.TXT` readable | `lsar` lists both; 7-Zip and `bsdtar` reject the header |
| R16 | replace bytes 20 to 74 (the first file header and its data) by the 38 bytes `31 1e 74 e0 80 22 00 04 00 00 00 00 00 00 00 00 00 00 00 00 a3 20 43 2a 14 30 02 00 10 00 00 00 44 31 44 41 54 41` (a directory `D1` with flags 0x80E0, PACK_SIZE 4 and the 4 data bytes `DATA`) | directory `D1` with the finding "directory with data" (D10), the 4 bytes stepped over; `DIR/B.TXT` readable | all three list `D1` as a directory and then `B.TXT` |

### 18.4 Chains: what the method layer is told

Header sequences taken from genuine archives (names shortened). "First"
means "does not continue"; "continues" refers to the nearest preceding
packed member.

Solid archive from RAR 2.50 (all version 20):

| Header | Method | Flags | U | P | Handed to the RAR 2.0 decoder as |
|:--|:--|:--|--:|--:|:--|
| `SUB` | 0x35 | 0x80E0 | 0 | 0 | not handed over (directory) |
| `EMPTY.BIN` | 0x35 | 0x8000 | 0 | 44 | first; U = 0, 44 packed bytes |
| `RND.BIN` | 0x35 | 0x8010 | 30,000 | 30,068 | continues |
| `HELLO.TXT` | 0x35 | 0x8010 | 25 | 13 | continues |

Solid archive from RAR 6.24 with a stored member in the middle:

| Header | Version | Method | Flags | Handed over as |
|:--|--:|:--|:--|:--|
| `f1.txt` | 29 | 0x33 | 0x90C0 | first |
| `f2.bin` | 20 | 0x30 | 0x90D0 | not handed over: read as stored (14) |
| `f3.bin` | 29 | 0x33 | 0x90D0 | continues `f1.txt` |
| `f4.txt` | 29 | 0x33 | 0x90D0 | continues `f3.bin` |

Solid archive from RAR 6.24 that begins with an empty file:

| Header | Version | Method | Flags | U | P | Handed over as |
|:--|--:|:--|:--|--:|--:|:--|
| `empty.bin` | 29 | 0x33 | 0x9080 | 0 | 8 | first; U = 0, 8 packed bytes |
| `f1.txt` | 29 | 0x33 | 0x9090 | 23,298 | 1,199 | continues |
| `f2.txt` | 29 | 0x33 | 0x9090 | 23,207 | 145 | continues |

**C1** (crafted; 518 bytes): the chain rules for version 20 and above,
in a form small enough for a unit test. Archive flags 0 (the archive's
solid bit is deliberately clear: it is not consulted). Every packed
member has U = 10, FILE_CRC 0 and the four data bytes `DE AD BE EF`,
which are **not** a valid stream for any decoder: the vector tests what
the container layer decides *before* any decoder runs. `S1` is a real
stored member (`stor`, CRC-32 0x5758779A).

| # | Name | Version | Method | Flags | Container's decision |
|--:|:--|--:|:--|:--|:--|
| 1 | `P1` | 29 | 0x33 | 0x8000 | packed; first of a chain |
| 2 | `S1` | 20 | 0x30 | 0x8010 | stored, readable; not in any chain |
| 3 | `DD` | 20 | 0x30 | 0x80F0 | directory; not in any chain |
| 4 | `P2` | 29 | 0x33 | 0x8010 | packed; continues `P1` |
| 5 | `P3` | 20 | 0x33 | 0x8010 | skipped, "solid predecessor unavailable" (`P2` belongs to another decoder) |
| 6 | `P4` | 29 | 0x33 | 0x8010 | skipped, "depends on an unreadable earlier entry" (`P3`) |
| 7 | `P5` | 29 | 0x35 | 0x8000 | packed; first of a new chain |
| 8 | `P6` | 29 | 0x33 | 0x8414 | skipped, "encrypted" (8-byte salt after the name) |
| 9 | `P7` | 29 | 0x33 | 0x8010 | skipped, "depends on an unreadable earlier entry" (`P6`) |
| 10 | `P8` | 36 | 0x33 | 0x8000 | packed; first of a new chain |
| 11 | `P9` | 29 | 0x31 | 0x8010 | packed; continues `P8` (36 and 29 are one decoder) |
| 12 | `Z0` | 29 | 0x33 | 0x8010 | packed with U = 0; continues `P9`; handed over with its 4 packed bytes; the entry is an empty file |
| 13 | `PA` | 29 | 0x33 | 0x8010 | packed; continues `Z0` |

With the dummy data, every member that is handed to a decoder then fails
there with "corrupt input", and by 15.3 the members that continue it
become "depends on an unreadable earlier entry"; `S1`, `DD` and the
empty entry `Z0` stay readable. 7-Zip's "Solid" column for this archive
follows flag 0x0010 member by member, as the table does.

```
526172211a0700cf907300000d000000000000002e1c7400802200040000000a
0000000000000000a320432a1d330200200000005031deadbeef839574108022
000400000004000000009a775857a320432a1430020020000000533173746f72
d77d74f080220000000000000000000000000000a320432a1430020010000000
444484c07410802200040000000a0000000000000000a320432a1d3302002000
00005032deadbeefd8db7410802200040000000a0000000000000000a320432a
14330200200000005033deadbeefb1657410802200040000000a000000000000
0000a320432a1d330200200000005034deadbeefbda17400802200040000000a
0000000000000000a320432a1d350200200000005035deadbeef6cf87414842a
00040000000a0000000000000000a320432a1d33020020000000503601020304
05060708deadbeef0b347410802200040000000a0000000000000000a320432a
1d330200200000005037deadbeef780d7400802200040000000a000000000000
0000a320432a24330200200000005038deadbeef8a317410802200040000000a
0000000000000000a320432a1d310200200000005039deadbeefedd974108022
0004000000000000000000000000a320432a1d330200200000005a30deadbeef
02e07410802200040000000a0000000000000000a320432a1d33020020000000
5041deadbeef
```

**C2** (crafted; 276 bytes): the chain rules below version 20
([`old-container.md`](old-container.md) section 5.3). Archive flags
0x0008 (solid). All file headers have flags 0x8000 and host 0; the
packed ones have U = 10 and the same dummy data as in C1.

| # | Name | Version | Method | U | P | Container's decision |
|--:|:--|--:|:--|--:|--:|:--|
| 1 | `D` | 15 | 0x33 | 0 | 0 | directory (attribute 0x10, host 0) |
| 2 | `E0` | 15 | 0x33 | 0 | 0 | empty file; takes no part (version below 20, U = 0) |
| 3 | `Q1` | 15 | 0x33 | 10 | 4 | packed; first of the chain |
| 4 | `S` | 15 | 0x30 | 4 | 4 | stored (`stor`), readable; not in the chain |
| 5 | `Q2` | 15 | 0x35 | 10 | 4 | packed; continues `Q1` (archive flag 0x0008) |
| 6 | `Q3` | 20 | 0x33 | 10 | 4 | packed; first of a chain of its own (flag 0x0010 clear; the archive flag is not consulted for version 20) |
| 7 | `Q4` | 15 | 0x33 | 10 | 4 | skipped, "solid predecessor unavailable" (the preceding packed member belongs to another decoder) |

The same archive with byte 10 changed from `08` to `00` and bytes 7, 8
from `3b d0` to `cf 90` (not solid): `Q1`, `Q2` and `Q4` are each the
first of a chain of their own; nothing is skipped for chain reasons.

```
526172211a07003bd07308000d00000000000000873274008021000000000000
0000000000000000a320432a0f3301001000000044f360740080220000000000
000000000000000000a320432a0f330200200000004530ba7c74008022000400
00000a0000000000000000a320432a0f330200200000005131deadbeefb38274
008021000400000004000000009a775857a320432a0f30010020000000537374
6f728a547400802200040000000a0000000000000000a320432a0f3502002000
00005132deadbeef89677400802200040000000a0000000000000000a320432a
14330200200000005133deadbeef35887400802200040000000a000000000000
0000a320432a0f330200200000005134deadbeef
```

## 19. Safety requirements

Everything in an archive is untrusted. An implementation must enforce the
following.

1. **Bounds before use.** Every size and length (HEAD_SIZE, data size,
   NAME_SIZE, the nested comment block's HEAD_SIZE, the lengths implied
   by flags) is checked against the bytes actually present (L, or the
   enclosing header) before the bytes are touched. The rules are in 4.5,
   6.1 (D7), 9.1 and 11. No field is read from beyond its header.
2. **Overflow-free arithmetic.** Offsets and sizes are handled as
   unsigned 64-bit values. A data size can be any 64-bit value; the test
   "does the data area fit" is made as "data size is not greater than L
   minus the data start", never by adding first. Sums of part sizes (P)
   are bounded by the sum of the real volume sizes because every part
   was bounds-checked or truncated to what exists.
3. **Progress and termination.** Every accepted block advances the
   position by at least 7 bytes; a volume of L bytes has at most L / 7
   blocks and at most L / 32 file headers. The signature search examines
   at most 1,048,576 positions. Unicode-name decoding consumes at least
   one input byte per item and ends with the input. Extended-time parsing
   reads at most 26 bytes.
4. **No allocation from declared sizes.** U, P, UNP_SIZE of comments and
   the counts in recovery records never size a buffer. Only headers are
   read into memory whole (HEAD_SIZE bytes, after the check against L);
   data areas of any block type are streamed or stepped over, never
   buffered whole. A symbolic link target is buffered up to 4,096 bytes
   (D11). A header is at
   most 65,535 bytes; a name at most 65,503 bytes; a decoded Unicode name
   at most as many code units as the name field has bytes (7.4). Stored
   members are streamed. A decoded comment is limited to 262,144 bytes
   (D17) and the limit is checked against the declared size before
   decoding and enforced on the output during decoding.
5. **Entry and volume counts.** The number of entries is bounded only by
   the input size (one per 32 bytes); the caller's limits on entries and
   on total unpacked size apply, as for every format of the repository.
   A set has at most 901 volumes by the old naming scheme; for the new
   scheme the caller's limit on the number of volumes applies. A volume
   is never opened twice in one set, and a "next volume" that is the same
   file as an earlier one ends the set.
6. **Names.** Decoded names go through the repository's safe-path
   handling (7.3). Symbolic link targets are data (D11): never followed,
   never used to open files.
7. **Nothing unverified is delivered.** Contents are released only after
   the size check and the CRC-32 comparison (13.3, 14). An encrypted
   member's bytes are never decoded "to see what happens".
8. **Per-entry failure.** Every failure in this document is either a
   skipped entry with a reason, a finding, the end of the listing with
   what was listed so far, or one of the two archive-level results
   ("corrupt archive", "encrypted archive"). None is a panic, and none
   makes entries that are independent of it unreadable.
9. **Chains.** A member is given to a decoder only with the state the
   rules of 15 allow; a decoder is never run on bytes of a member whose
   method, version, encryption or completeness rules it out.
10. **Hostile sets.** Volumes of a set may contradict each other
    (different flags, repeated or reordered parts). The joining rules of
    13 decide by flags, name and U only, and every disagreement ends in a
    skipped entry; no rule trusts a later volume to make an earlier
    decision safe. Every volume's own archive header decides whether its
    blocks are walked at all (flag 0x0080, 12.3); the first volume's
    flags are never applied to another volume's bytes.
11. **Signature search cost.** The search of D1 reads at most the first
    1,048,576 + 20 bytes of a file; a file made of repeated signature
    bytes costs one archive header check (at most two CRC-16 computations
    over at most 65,533 bytes) per match and each failed match advances the search by
    one byte. An implementation that finds this too slow may bound the
    number of failed checks (for example 4,096) and then report "not
    this format"; no genuine module contains the signature even once.
12. **Order of tests.** An entry is classified by the fixed order of
    13.4, so that no combination of flags reaches a decoder by a path
    the simple cases do not take (an encrypted or incomplete member is
    never decoded because its method byte happened to be tested
    first).

## 20. Observed implementation gaps

Faults and limits of the tools examined. They explain rules; none is to
be imitated.

The reference implementation (`unar`/`lsar` 1.10.7):

1. Verifies no header checksum: R1 and R8 are listed as if intact.
2. Does not require an archive header (R7) and obeys flag 0x8000 on an
   archive header, reading four reserved bytes as a data size and losing
   the blocks that follow.
3. Takes the first seven-byte signature match in a self-extracting file
   without checking what follows (S1 fails), and searches only 524,288
   bytes.
4. Treats a 0x7A block without flag 0x8000 as having no data and then
   loses synchronisation.
5. Stored members: delivers min(P, U) bytes and reports success when P is
   smaller than U (R5; a member declared as 4,294,967,314 bytes with 18
   present is reported "OK").
6. Builds solid chains from all file headers regardless of method and
   feeds a stored member's bytes to the decoder: on the genuine solid
   archive of 18.4 with a stored member it fails on every packed member
   after it.
7. Old comments: ignores the method byte (a stored comment is run
   through a decoder and comes out as garbage, A2), ignores COMM_CRC, and
   computes the comment's packed length from an unchecked header size.
   New-style (`CMT`) comments and file comments are not read at all.
8. Unicode names: bounds a run against the whole name field instead of
   the plain name, so a run can copy bytes of the encoded part (U17);
   accepts items cut off by the end of the data (U18) and zero units;
   returns an empty name for a lone surrogate (U13).
9. Does not read the extended time field, the end block's fields or
   sub-blocks; shows only the DOS modification time, and normalises
   invalid DOS times into other dates instead of rejecting them.
10. Recognises directories of version-15 archives only for host 0
    ([`old-container.md`](old-container.md) section 10).
11. A file header whose data size points beyond the end of the file is
    listed without any error (R4), and the walk then ends silently.
12. Does not find the later volumes of an old-scheme set whose first
    volume is self-extracting, and joins parts by name alone.
13. Fails on some members of genuine solid version-29 archives written
    by RAR 3.00 and 6.24 for reasons inside the decoder (section 22,
    point 5).
14. A header size below 7, or a header cut off by the end of the file,
    raises an error for the whole archive instead of ending the listing
    with a finding.
15. Does not check that the block nested in an archive header with flag
    0x0002 has type 0x75 before reading it as a comment.
16. Keeps the archive flags of the archive header it read last and
    clears them only at an end block; in a set without end blocks the
    encrypted-headers flag of one volume is applied to the signature
    and archive header of the next. It never compares the flags of the
    volumes of a set.

Other tools:

17. 7-Zip 26.01 rejects any header with bytes it does not understand
    after the last field (four spare bytes after a name; a fourth
    extended-time group), stops at unknown block types, applies the
    per-part checksum also to the constant 0xFFFFFFFF (V3), and in the
    build examined cannot decode packed RAR members at all.
18. `bsdtar` 3.5.3 rejects archive and file headers with nested comments
    and the old authenticity blocks for their checksums, has no solid
    and no encryption support, and for a stored member with P smaller
    than U writes U bytes.
19. `lsar`, 7-Zip and `bsdtar` each decode a name without the Unicode
    flag that is not ASCII differently, and none correctly.
20. `lsar` and 7-Zip stop at a sub-block (0x7A) whose flag 0x8000 is
    clear, and `lsar` reads a data size out of an archive header whose
    flag 0x8000 is set (A7: both list `A.TXT` only; `bsdtar` lists both
    files).

## 21. Decisions

| # | Section | Decision |
|--:|:--|:--|
| D1 | 3.3 | Self-extracting archives: search offsets 1 to 1,048,575; a match counts only if the archive header check passes. |
| D2 | 3.3, 5.3 | Signature at offset 0 with a failing archive header check, or a first block that is not an archive header: archive-level error "corrupt archive". |
| D3 | 4.3 | Data size by block type: always present for 0x74, 0x7A, 0x77, 0x78; never for 0x72, 0x73, 0x7B; by flag 0x8000 for all others. |
| D4 | 4.4 | Header checksums are verified for 0x73, 0x74, 0x7A, 0x7B (and nested 0x75 before use), with the stated coverage alternatives; not for other types. |
| D5 | 4.5 | A marker or archive header inside the archive is a corrupt header. The walk stops at the first corrupt header, keeping what was listed. |
| D6 | 5.1 | Unknown archive flags, flag 0x8000 on the archive header and extra bytes in it are ignored. |
| D7 | 6.1, 8.2 | Optional areas of a file header that do not fit are treated as absent, with a finding; the first that does not fit removes the later ones too. D7a: a nested comment block is looked for after salt and extended time. |
| D8 | 6.1 | NAME_SIZE 0: skipped entry "empty name". |
| D9 | 6.4 | Attributes of hosts other than 0 to 3 are not interpreted. |
| D10 | 6.5 | Version 20 and above: directory if and only if flag bits 0x00E0 are all set; a directory with data is listed with a finding and its data skipped. |
| D11 | 6.5 | Unix symbolic links are listed as links; the target is the member's contents, at most 4,096 bytes, never followed. |
| D12 | 7.1 | Names without the Unicode flag: UTF-8 if valid, else the configured legacy code page (default 437). |
| D13 | 7.2 | A malformed encoded name is discarded in favour of the plain name; unpaired surrogates become U+FFFD. |
| D14 | 7.3 | Names are split at backslash and forward slash, then passed to the safe-path handling. |
| D15 | 7.5 | Version suffixes `;n` are kept in the name. |
| D16 | 8.1 | Invalid DOS times give no time. Times are local, without zone. |
| D17 | 9 | Comments are optional, limited to 262,144 bytes, and can never fail anything else. |
| D18 | 10.1 | Sub-blocks produce no entries. |
| D19 | 10.5 | Unknown block types are stepped over without checksum verification. |
| D20 | 11 | The end block ends its volume; nothing after it is read. DATA_CRC verification is optional. |
| D21 | 12.2 | The old naming scheme runs from `.r00` to `.z99`. |
| D22 | 12.3 | When to ask for a next volume, what a next volume must satisfy, and when the first volume is known to be first. |
| D23 | 13.2 | Joining rules; incomplete members are skipped as "missing volume part", truncated ones as "truncated". |
| D24 | 13.3 | The per-part checksum of non-final parts is verified for stored members (version 20 and above, value not 0xFFFFFFFF). |
| D25 | 14 | Stored members with P different from U are skipped as "corrupt". |
| D26 | 16.2 | Encrypted headers: archive-level result "encrypted archive", no entries. A later volume with the flag is not walked and ends the set (12.3). |
| D27 | 13.4 | Fixed order of the tests that classify an entry: incomplete, empty name, directory, encrypted, stored, packed, unsupported version, unsupported method. |
| D28 | 12.2 | The volume-naming flag is advice; the other scheme may be tried when the flagged one finds nothing. |

Decisions of [`old-container.md`](old-container.md) that this document
relies on unchanged: V1 (method bytes), V2 and V3 (unpack versions), V4
(chains of different decoders), V6 (version-15 directories), V7 (CRC-32
verification), V8 (encrypted members and chains). Its decision V5 (stored
members do not touch a chain) is confirmed by observation for version 29
(15.2).

## 22. Open questions

None blocks an implementation; each says what to do meanwhile.

1. **The archiving-time group of the extended time field** (8.2): never
   observed. Parsed as specified so that the field's length is right;
   the value need not be exposed.
2. **ENCRYPT_VER** (archive flag 0x0200): never observed; skipped.
3. **Sub-blocks other than `CMT`, `RR`, `AV`, `UOW`**, and the old
   sub-block 0x77: not observed in this survey; stepped over by size.
   NTFS streams in particular are therefore not listed.
4. **Unpack versions 26 and 36** and method bytes other than those listed
   in 6.3: not observed in this survey; handled by the dispatch of
   [`old-container.md`](old-container.md).
5. **For the authors of `rar29-lz.md` and `rar29-ppmd.md`:** `unar` fails
   on members of two genuine solid version-29 archives. In one (RAR 6.24:
   a 24-byte text file, 3,000 random bytes, an empty file with 2 packed
   bytes, then the same 24-byte text again with 41 packed bytes) the
   fourth and later members fail with "attempted to read more data than
   was available", also after the empty member's block is removed. In the
   other (RAR 3.00: four text files, an empty file with 2 packed bytes,
   30,000 random bytes) the member after the empty one fails. Whether a
   U = 0 member's packed bytes carry state for version 29, and how the
   decoder moves from one member's packed bytes to the next, is for those
   documents to settle; this document only guarantees that such members
   are handed over (15.2).
6. **Stored members inside version-15 and version-20 chains**: RAR 2.50
   packs every file of a solid archive, so none was produced; decision V5
   stands for them unconfirmed.
7. **Names from RAR for Unix before the Unicode form was used**, and the
   plain names written by Windows versions in code pages other than 437
   and 850: handled by D12; the right code page cannot be known.
8. **Volume names beyond `.s99`** in the old scheme: inferred (D21).
9. **Mac OS and BeOS hosts**: attribute format unknown; not interpreted.
10. **A member whose U is unknown to the writer**: RAR 6.24 writing from
    standard input stores the real size, so no "unknown size" marker was
    observed; U is always taken literally.

## 23. Evidence

Archives examined (all kept outside the repository):

| Group | Sets | Volume files | File headers | Notes |
|:--|--:|--:|--:|:--|
| Genuine, RAR 6.24 for macOS in RAR4 mode | 38 | 219 | 490 | every option reachable from the command line: stored and packed, solid, dictionary sizes, time precisions, comments (ASCII and non-ASCII), recovery record, lock, per-file and header encryption, self-extracting, new and old volume naming (a set of 133 volumes), recovery volumes, owner records, a 4.3 GiB file, input from standard input |
| Genuine, RAR 2.50 for DOS | 15 | 28 | 113 | stored, packed, solid, archive and file comment, recovery record, encryption, volumes (plain, solid, self-extracting, with recovery record), self-extracting |
| Genuine, RAR 3.00 for DOS | 8 | 11 | 58 | comment, recovery record, per-file and header encryption, solid, file versions, volumes |
| Genuine, RAR 3.93 for DOS | 5 | 5 | 28 | per-file and header encryption, no end block, extended times |
| Real | 11 | 16 | 234 | the vendor's self-extracting distributions of RAR 2.50, WinRAR 2.90, 3.00, 3.93 and 4.20 and RAR for DOS 3.00 and 3.93; a WinRAR 1.55 archive; two archives and one six-volume set made with WinRAR 3.x or 4.x; one private RAR 2.x solid archive |
| Total | 77 | 279 | 923 | |

What was checked on them:

- **Structure.** The throwaway reader written from this document walks
  every volume to its end without a finding. All 279 archive header
  checksums, 923 file header checksums, 50 sub-block checksums and 237
  end block checksums verify under the coverage rules of 4.4.
- **Listings.** For every set, the reader's entries were compared with
  `lsar` and with 7-Zip: count, names (for names in the Unicode form),
  U, P, directory flag, encryption flag, FILE_CRC, and with 7-Zip also
  the modification, creation and access times to 100 ns and the solid
  flag. The only differences are names without the Unicode flag that
  hold non-ASCII bytes (section 20, point 19) and two self-extracting
  first volumes whose later volumes the tools do not locate.
- **Contents.** 163 stored members (7 of them split) were read and
  match their CRC-32; all 198 non-final parts match their per-part
  checksum. Packed members are outside this document; `unar` extracted
  the non-solid ones as a check that the data areas are located
  correctly.
- **Crafted archives.** About 90 hand-built archives probed with `lsar`,
  `unar`, 7-Zip and `bsdtar`: the vectors of section 18 and variations
  of them (block flags, unknown and misplaced blocks, sizes, names,
  times, directories, self-extracting offsets).

**Independent verification.** A second throwaway reader was written
from this document alone by a different member of the specification
team, before reading the reference implementation, and run on:

- every vector of section 18 (the whole archives, U1 to U20, X1 to X9,
  T1 to T10, H1, R1 to R16, the V and C vectors): all expected results
  reproduced;
- 88 sets (290 volume files, 961 file headers, 763 entries) made of the
  genuine and real archives above and the cut-down variants made from
  them: every volume walks to its end without a finding; 758 of 758
  entries compared with 7-Zip agree in name, U, P, directory flag,
  encryption flag, FILE_CRC and all three times, 756 of 756 compared
  with `lsar` agree in name, U, FILE_CRC and directory flag (the
  entries not compared belong to the two self-extracting sets whose
  later volumes the tools do not locate); 180 stored members match
  their CRC-32; DATA_CRC matches in 194 of 194 end blocks; the `RR`
  and 0x78 size formulas hold in 10 and 5 blocks; RESERVED2 points at
  the authenticity block in 8 of 8 archives.

The places where that reader's author had to guess became sections 13.4
(D27), the ordering rule of D7, the scope of the checksum exception in
4.4 and the rule for a later volume with encrypted headers (12.3). The
stored-member experiment of 15.2 was repeated with the same result
(`unar` fails on the two members after the stored one in the unmodified
archive and extracts all three packed members once the stored block is
cut out).

What is reference-only or inferred: ENCRYPT_VER; the older meaning of
archive flag 0x0010; the layout of encrypted
headers; the archiving-time group; sub-block tags not observed; the old
sub-block's fields; old-scheme volume names beyond `.s99`.

Tools and their versions: `unar`/`lsar` 1.10.7; 7-Zip 26.01 (`7zz`);
p7zip 17.05; `bsdtar` 3.5.3 (libarchive 3.7.4); RAR 6.24 for macOS, RAR
2.50, 3.00 and 3.93 for DOS (the latter three run in the DOSBox-X
emulator), all used only to create archives.

## Clean-room feedback

Notes from the implementation side (clean room). They record decisions
taken where this document leaves room, deviations from it with their
reasons, and observations on real archives; prose only, not written
from any reader's source.

1. **D1, signature search: only inside executables.** The search of
   section 3.3 is made only when the file starts like a program: `MZ`
   (DOS, Windows, OS/2), the ELF magic, or one of the Mach-O magics
   (thin, either byte order, 32 or 64 bit; universal). Reason: the
   reader is one of many format probes of a general loader, and a
   search in every unrecognised file would make any disk image or
   foreign container that merely stores a RAR archive in its first
   megabyte pass for that archive. Within an executable the search is as
   specified: offsets 1 to 1,048,575, the archive header check at every
   match, at most 64 matches examined. The same rule finds RAR 5
   archives (first-block check of `rar5-container.md` section 4) and
   old-container archives (structural checks of `old-container.md`
   section 2.2). All five self-extracting corpus archives are read
   (archive offsets 6,491 to 29,489).
2. **D2 and the archive header checksum on real archives.** The check of
   section 3.2 is applied to every volume. All 99 RAR4-format archives
   of the test corpus pass it, with every volume of their sets. One archive
   found in the wild fails it (first volume of a four-volume set whose
   archive header declares 13 bytes but holds 12): `unar` and 7-Zip
   refuse it as well.
3. **Section 4.4, file header checksums are not verified yet.** The
   walk of 4.5 is implemented with every other test of step 3; a wrong
   checksum in a 0x74, 0x7A or 0x7B header is not detected. A damaged
   name or size is therefore not noticed by itself; damaged contents
   are, by size and CRC-32.
4. **Section 6.5, names with a trailing separator.** An entry of version
   20 or above without the directory bits whose name ends in a path
   separator and which has neither packed nor unpacked bytes is treated
   as a directory. No file can have such a name, and listing it as an
   empty file would collide with the directory of the same name. With
   data it is a file, as section 6.5 says. Attributes are not consulted
   (D10).
5. **D22 and missing volumes.** After a corrupt or truncated block no
   further volume is read, as specified. A volume that is **absent** is
   a different case, which section 12.3 does not single out: when up to
   eight further volume names are tried and one of them exists and is a
   volume of the same format, reading continues there. The file that was
   waiting for the absent volume is reported incomplete, its orphaned
   last piece is dropped without a second report, every chain is treated
   as broken (15.3), and files that lie entirely in the later volumes
   are delivered. A six-volume real set with its third volume removed
   yields 12 of 13 files this way instead of 2.
6. **Findings are summarised.** The container reports one line per
   archive: the first three findings in full and a count of the rest.

### Notes from the second implementation (`rar_native/container4.rs`)

Items 1 to 6 above describe an earlier reader. The notes below are from
the reader written afterwards from this document alone. It verifies the
header checksums of section 4.4 (item 3 above no longer applies to it),
searches for the signature in any file and not only in executables
(whether to restrict the search is left to the loader that calls it),
and does not apply the trailing-separator rule of item 4.

7. **All vectors reproduce.** G1, A1 to A7, S1, E1, V1 to V7 (with V1x),
   H1, U1 to U20, X1 to X9, T1 to T10, R1 to R16, C1, C2 (both forms)
   and the solid pair of `old-container.md` section 9.2 give the
   expected results. Every prefix and every single-byte change of every
   whole-archive vector is read without failure of the reader itself.
8. **Section 3.3, cost of the search.** The bound that safety item 11
   offers was taken: after 4,096 signature matches that fail the archive
   header check the file is "not this format".
9. **Section 4.5, step 3, type 0x7B.** The minimum header size for the
   end block is taken as 7: by 4.3 it never carries a data size, so the
   "11 for a block that carries a data size" rule does not apply even
   when its flag 0x8000 is set. The text allows this reading but does
   not say it.
10. **Section 13.2, a truncated first part.** A part that is truncated
    closes its member as "truncated" whatever its continuation bits say.
    P of such a member counts the bytes present, not the declared size,
    so that it stays bounded by the real volume sizes (safety item 2).
11. **Section 13.1, where a continuation may stand.** The joining rule
    was implemented as written: flags, name and U only. A continuation
    part therefore also joins an open member when it stands in the same
    volume, or not first in its volume. Section 12.1 says genuine
    writers never do that; the document might say whether a reader
    should insist on it. It makes no difference to safety (the pieces
    are bounds-checked ranges and the checksum decides).
12. **Section 15.2, version below 20: "the first packed member".** Read
    as: in a solid archive such a member continues exactly when any
    member that takes part in chains precedes it, of whatever version.
    A version-15 member that follows only version-29 members in a solid
    archive is therefore "solid predecessor unavailable", not the start
    of a chain. When the first volume given is known not to be the first
    of its set, the first such member is treated as continuing and is
    skipped likewise. The solid flag is taken from the archive header of
    the volume that holds the member's last part.
13. **Section 15.3, order of the two reasons.** When the nearest
    preceding member is both unreadable and of another decoder, the
    reason given is "depends on an unreadable earlier entry" (C1, `P4`);
    "solid predecessor unavailable" is used only when the predecessor is
    absent or is a decodable member of another decoder. An unreadable
    predecessor with an unsupported version byte counts as unreadable.
14. **Section 13.4 and 5.3 of `old-container.md`, empty packed members
    below version 20.** Such a member (C2, `E0`) is described as an
    empty file that is never handed to a decoder; its data area, should
    it have one, is ignored. For a stored member with U = 0 and P not 0
    decision D25 was applied (skipped as "corrupt") rather than rule 4
    of 13.3; the two could be read as contradicting each other there.
15. **Section 12.3, a gap inside a set.** When volumes are supplied with
    one missing in the middle, the end block's volume number shows it
    and the finding is raised, and the split member is closed as
    "missing volume part"; a chain that runs across the gap is not
    detected by any rule of the document (a member with the chain bit in
    the later volume finds a predecessor in the earlier one). The reader
    leaves this to the CRC-32 comparison. The document could make a
    volume-number gap break every chain.
16. **Section 12.2, names.** A name without any extension has no next
    name in either scheme. In the old scheme an extension that is not a
    letter from `r` to `z` followed by two digits marks the first
    volume; the new extension is written in upper case when the
    replaced one has letters and none of them is lower case. In the new
    scheme the extension is kept when it is `rar` in any letter case and
    replaced by `rar` otherwise.
17. **Comments.** Archive comments (nested block and `CMT`) are located
    and described but not decoded here; file comments are not looked
    for, so decision D7's finding for a malformed file comment is never
    raised. A nested archive comment that fails its checks, and a `CMT`
    block declaring more than 262,144 bytes, are dropped with a finding.
18. **DATA_CRC** is reported, not verified (section 11 allows either).
    Stored members are checked by a separate step that the caller runs
    before delivering bytes: per-part checksums first ("corrupt"), then
    the CRC-32 of the whole member ("checksum mismatch").
19. **Real archives** (black box, compared with `lsar` and 7-Zip): a
    RAR 1.55 solid archive with a packed nested comment, a WinRAR 3.x
    archive of eight stored members with a `CMT` block, a six-volume
    new-scheme set of stored members (one member in five parts, all
    per-part checksums and the final CRC-32 match) and a private RAR
    2.x solid archive of 21 members: names, sizes, CRC-32, directory
    flags and times (to 100 ns against 7-Zip) agree, no findings. The
    six-volume set read from its second volume, and with its second
    volume left out, gives the expected "missing volume part" entries.
