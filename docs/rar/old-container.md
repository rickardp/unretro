# RAR: the old `RE~^` container, version dispatch, checksums, encryption

Format specification for clean-room implementation. This document covers
four things a RAR reader needs around the method decoders:

- **Part A** (sections 2 to 4): the archive container written by RAR 1.3x
  and 1.4x, recognised by the signature `RE~^`. It is a different layout
  from the `Rar!` container and is called the **old container** here.
- **Part B** (section 5): for archives in the RAR4 format (signature
  `Rar!\x1a\x07\x00`, written by RAR 1.50 to 4.x), which decoder each value
  of the file header's unpack-version byte selects, which files share
  decoder state, and how directories are marked by the oldest writers.
- **Part C** (section 6): how the stored file checksum relates to a file's
  contents, for whole files and for files split across volumes.
- **Part D** (section 7): how encrypted entries are recognised in each
  generation, to the extent needed to skip them and report them.

The compression method of the old container, which is also the method of
unpack version 15, is specified in [`rar15.md`](rar15.md). Unpack versions
20 and 26 are specified in [`rar20.md`](rar20.md). The general block
structure of the RAR4 format (block header, main header, file header
fields, volume naming) is already implemented in this repository and is
restated here only where a rule depends on it.

The document contains no program code and no pseudo-code.

## 1. How to read this document

### 1.1 Provenance

- The old container is described from three sources: the technical note
  shipped with RAR 1.40 (`TECHNOTE.DOC`, February 1994, in the RAR 1.40.2
  distribution, read in its Russian and English editions); the comments of
  the RAR module of the Deark utility (which reads the layout of old
  archives but does not unpack them); and measurements on real old
  archives.
- The dispatch, solid and checksum rules for the RAR4 format are described
  from the behaviour of The Unarchiver's XADMaster library (LGPL), called
  the **reference implementation**, from black-box tests of `unar`/`lsar`
  1.10.7, which is built on it, and from measurements on real archives.
- The official `unrar` source was **not** consulted.
- One fact has a weaker pedigree and is marked where it occurs: the three
  starting key bytes of the comment obfuscation (section 2.5) were supplied
  from the specification author's general knowledge of the format and were
  then confirmed by experiment on four real archives. Nothing else depends
  on them; a reader that does not show archive comments never needs them.
- No black-box tool was available for the old container: `unar`/`lsar`
  1.10.7 refuse `RE~^` archives ("File is not fully supported") and 7-Zip
  (p7zip 17.05) does not open them. Every statement of Part A was checked
  with a throwaway reader written by the specification's author from the
  rules below (kept outside the repository; it is not part of the
  specification). Its evidence is that the files it extracts match the
  checksums stored in the archives, and that extracted text files are
  readable text.

### 1.2 Evidence labels

- **CONFIRMED (real data)**: checked on real archives.
  - Old container: 19 distinct archives (22 volume files) containing 296
    file headers: 275 packed non-empty files in one piece, 6 stored files,
    10 directories, and 5 parts of two files split across volumes. Eight
    archives are solid, two are self-extracting, one is a complete
    four-volume solid set and one is a single volume of an incomplete set.
    Every one of the 282 complete files (276 packed, 6 stored) was
    extracted and matches its stored checksum.
  - RAR4 format, unpack version 15: 31 archives (33 volume files) with
    2,021 file headers: 1,950 packed non-empty files (all extracted, all
    match their CRC-32), 9 stored files, 59 directory entries.
  - RAR4 format, unpack version 20: header-level observations on 33
    archives (75 volume files) with 1,453 file headers (decoding of this
    version is the subject of `rar20.md`).
  The surveys were made while the collection of archives was still
  growing; counts quoted elsewhere in this document set may differ
  slightly. No rule depends on a count.
- **CONFIRMED (crafted)**: checked by giving `unar` a purpose-built
  archive. Possible only for the RAR4 format.
- **TECHNOTE**: stated by the RAR 1.40 technical note, not observed.
- **REFERENCE-ONLY**: stated by the reference implementation, not
  observed.
- **INFERRED**: deduced, unconfirmed.
- **DECISION**: chosen by this specification (listed in section 11). No
  decision changes how a genuine, undamaged archive is read.

### 1.3 Terms

| Term | Meaning |
|:--|:--|
| old container | The archive layout with signature `RE~^` (RAR 1.3x, 1.4x). |
| RAR4 format | The archive layout with signature `Rar!\x1a\x07\x00` (RAR 1.50 to 4.x). Despite the name it starts with RAR 1.50. |
| entry | One file or directory as listed to the caller. |
| part | The portion of a file stored in one volume, with its own file header. |
| **P** | The number of packed bytes of an entry, all parts added up. Untrusted. |
| **U** | The unpacked length an entry's header declares. Untrusted. |
| **L** | The number of bytes actually available in the volume being read. |
| chain | A sequence of files sharing one decoder state (`rar15.md` section 11). |
| sum16 | The 16-bit checksum of section 3. |
| CRC-32 | The standard CRC-32 (reflected polynomial 0xEDB88320, initial value and final XOR 0xFFFFFFFF; `123456789` gives 0xCBF43926). |
| skipped entry | An entry that is listed but whose contents are not delivered, with a one-line reason. |

All multi-byte integers in both containers are **little-endian**. Numbers
are decimal unless prefixed `0x`. Byte sequences in code style are
hexadecimal.

## 2. The old container: layout

An old archive is: an archive header, then zero or more file blocks one
after the other up to the end of the volume. A file block is a file header
immediately followed by that file's (or part's) data. There is no end
marker, no central directory, no header checksum of any kind, and no
padding or alignment.

### 2.1 Detection

- The archive begins with the four bytes `52 45 7E 5E` (ASCII `RE~^`).
- A self-extracting archive is a DOS executable with the archive appended;
  the archive is found by searching for the signature. The technical note
  guarantees that the signature does not occur inside the executable stub.
  CONFIRMED (real data) for two self-extracting archives made by RAR 1.40;
  in both the archive begins at offset 6,491.
- DECISION C1: the reader recognises an old archive at offset 0 by the
  signature **and** the structural checks of section 2.2 (the signature is
  only four printable bytes). Whether and how far to search inside an
  executable is the business of the layer that handles self-extracting
  files; when it searches, the same structural checks apply at the found
  offset, and all offsets below are relative to the first signature byte.

### 2.2 Archive header

| Offset | Size | Field | Meaning |
|--:|--:|:--|:--|
| 0 | 4 | HEAD_ID | `52 45 7E 5E` |
| 4 | 2 | HEAD_LEN | Length of the whole archive header in bytes, counted from offset 0, including the optional fields. At least 7. |
| 6 | 1 | HEAD_FLAGS | Flag bits, below. |
| 7 | 2 | COMM_LEN | Present only if flag 0x02 is set: length of the comment field. |
| 9 | COMM_LEN | MAIN_COMMENT | Present only if flag 0x02 is set (section 2.5). |
| | 2 | EXT1_LEN | Present only if flag 0x20 is set: length of the EXT1 field. Follows the comment, or follows HEAD_FLAGS when there is no comment. |
| | EXT1_LEN | EXT1 | Present only if flag 0x20 is set. Contents not interpreted. |

The first file header is at offset HEAD_LEN. A reader must use HEAD_LEN,
not the sum of the fields it understands, to find it.

HEAD_FLAGS:

| Bit | Meaning | Evidence |
|:--|:--|:--|
| 0x01 | The archive is one volume of a multi-volume set (section 4.2). | CONFIRMED (5 volume files) |
| 0x02 | An archive comment is present. | CONFIRMED (5 archives) |
| 0x04 | The archive is locked against modification. No effect on reading. | TECHNOTE; set in one real archive |
| 0x08 | The archive is solid (section 4.1). | CONFIRMED (8 archives, 11 volume files) |
| 0x10 | The archive comment is packed (section 2.5). | CONFIRMED (4 archives) |
| 0x20 | The EXT1 field is present. | CONFIRMED (2 archives, EXT1 lengths 63 and 68) |
| 0x40 | Undocumented ("reserved for internal use"). Set in 8 of the 22 volume files. No effect on reading. | observed |
| 0x80 | Undocumented. Set in all 22 volume files. No effect on reading. | observed |

In all 22 volume files the optional fields end exactly at HEAD_LEN.

Validation (DECISION C2). The header is rejected, and the input is "not
this format", when: fewer than 7 bytes are available; HEAD_LEN is less than
7; HEAD_LEN is greater than L; or the optional fields announced by the
flags do not fit inside HEAD_LEN (7, plus 2 + COMM_LEN if flag 0x02, plus
2 + EXT1_LEN if flag 0x20, must not exceed HEAD_LEN). Bytes between the
end of the known fields and HEAD_LEN are skipped.

### 2.3 File header

A file header starts at offset HEAD_LEN, and after each file block at the
offset where the previous block's data ended.

| Offset in header | Size | Field | Meaning |
|--:|--:|:--|:--|
| 0 | 4 | PACK_LEN | Number of data bytes that follow this header in this volume. |
| 4 | 4 | UNP_LEN | Unpacked length of the whole file (U), also in the headers of a split file's parts. |
| 8 | 2 | CHECK_SUM | sum16 of the unpacked file (section 3; for parts, section 6.1). |
| 10 | 2 | HEAD_LEN | Length of this file header in bytes, counted from its offset 0, including the name and optional fields. |
| 12 | 4 | FTIME | Modification time, MS-DOS format (below). |
| 16 | 1 | ATTR | MS-DOS file attributes. |
| 17 | 1 | FLAGS | Flag bits, below. |
| 18 | 1 | VER | Minimum version needed to unpack. Always 2 in real archives. |
| 19 | 1 | NAME_LEN | Length of the name in bytes. |
| 20 | 1 | METHOD | 0 = stored, 1..5 = packed (section 2.6). |
| 21 | NAME_LEN | FILE_NAME | Name bytes, not terminated. |
| | 2 | FILE_COMM_LEN | Present only if flag 0x08 is set. |
| | FILE_COMM_LEN | FILE_COMMENT | Present only if flag 0x08 is set. |

The data of the block starts at header offset HEAD_LEN and is PACK_LEN
bytes long; the next file header follows immediately.

CONFIRMED (real data): in all 296 real file headers HEAD_LEN equals
21 + NAME_LEN, VER is 2 and flag 0x08 is clear.

The technical note lists the comment fields *before* the name; the Deark
module reports that RAR 1.40.2 writes the name first, as in the table. No
real header with a file comment was available, so the position of the
comment fields is INFERRED. DECISION C3: the name is always read at offset
21; whatever lies between the end of the name and HEAD_LEN is skipped
without interpretation. (File comments are therefore not extracted.)

FLAGS:

| Bit | Meaning | Evidence |
|:--|:--|:--|
| 0x01 | This is not the first part: the file continues from the previous volume. | CONFIRMED |
| 0x02 | This is not the last part: the file continues in the next volume. | CONFIRMED |
| 0x04 | The file is encrypted with a password (section 7). | TECHNOTE; not observed |
| 0x08 | A file comment is present. | TECHNOTE; not observed |

Other bits were zero in every real header. DECISION C4: they are ignored.

VER: the technical note assigns 0 to RAR 0.99, 1 to RAR 1.00 (both "no
longer supported" already in 1994) and 2 to RAR 1.30. DECISION C5: an
entry whose VER is not 2 is listed and skipped with the reason
"unsupported version"; in a solid archive this ends the usable part of the
chain (section 4.1).

FTIME is the usual MS-DOS packed date and time: bits 0..4 seconds divided
by 2, bits 5..10 minutes, bits 11..15 hours, bits 16..20 day, bits 21..24
month, bits 25..31 years since 1980. It is a local time without zone.

Validation of a file header (DECISION C6). Reading stops, and the listing
is complete as far as it got, when fewer than 21 bytes remain in the
volume (zero bytes remaining is the normal end; 1 to 20 stray bytes are a
finding). The listing stops with an error "corrupt header" when HEAD_LEN is
less than 21 + NAME_LEN, when NAME_LEN is 0, or when the header itself
extends beyond L. When the header is sound but its data extends beyond L,
the entry is listed, skipped with the reason "truncated", and the listing
stops, since the next header cannot be located.

### 2.4 Names, directories, attributes

- The name is a path in the DOS code page of the machine that made the
  archive (CP437 or CP866 in the samples), with **backslash** as the
  separator and no leading separator or drive in any real archive.
  CONFIRMED (real data): 12 names with backslashes, none with a forward
  slash. The usual safe-path rules of the repository apply to it like to
  any other archive name (no absolute paths, no `..` components).
- A **directory** is an entry with ATTR bit 0x10 set. CONFIRMED (real
  data): 10 entries, all with UNP_LEN = 0, PACK_LEN = 0 and METHOD 5, the
  name without a trailing separator, and files below them later in the
  archive. A directory has no data and takes no part in decoding.
  DECISION C7: an entry with ATTR bit 0x10 and UNP_LEN or PACK_LEN other
  than 0 is listed as a directory with a finding, and its PACK_LEN bytes
  are skipped.
- ATTR is the DOS attribute byte (0x01 read-only, 0x02 hidden, 0x04 system,
  0x08 volume label, 0x10 directory, 0x20 archive).

### 2.5 Archive comment

When flag 0x02 is set the header holds a comment field of COMM_LEN bytes.

- **Flag 0x10 clear: plain comment.** The field is the comment text, in
  the DOS code page, typically with ANSI escape sequences. CONFIRMED (one
  archive).
- **Flag 0x10 set: packed comment.** The field consists of:
  1. a 2-byte little-endian number: the unpacked length of the comment;
  2. COMM_LEN − 2 bytes: a packed stream of the method of `rar15.md`,
     **obfuscated** by the byte-wise procedure below.

  To read it: de-obfuscate the COMM_LEN − 2 bytes, then decode them with
  the RAR 1.5 decoder as the first and only file of a chain of its own
  (fresh state), with U = the unpacked length. The comment is independent
  of the files: it neither uses nor changes the decoder state of the
  archive's files, in solid archives too. There is no checksum for the
  comment.

  De-obfuscation works on three 8-bit values A, B and C, all arithmetic
  modulo 256. They start as A = 0, B = 7, C = 77. For each byte of the
  field, in order: add C to B; then add B to A; the plain byte is the
  stored byte minus A. (C never changes.) The first plain byte is
  therefore the stored byte minus 84, the second the stored byte minus
  245 (that is, plus 11), the third the stored byte minus 227.

  CONFIRMED (real data), with the reservation of section 1.1 about where
  the constants came from: the four packed comments of the survey
  (unpacked lengths 1,202 to 1,795) decode without error to text that
  consists entirely of printable characters and ANSI escape sequences,
  using all but 4 to 6 bits of the field.

A reader may ignore comments altogether. If it reads them it must apply
the limits of section 12 (the unpacked length is at most 65,535 by
construction).

### 2.6 File data and methods

- METHOD 0: **stored**. The data bytes are the file's bytes; PACK_LEN
  (all parts added) must equal UNP_LEN, otherwise the entry is skipped as
  "corrupt". CONFIRMED (6 files).
- METHOD 1 to 5: **packed** with the method of `rar15.md`. The five values
  are effort levels of the packer ("fastest" to "maximal"); the stream
  format and the decoder are the same for all. CONFIRMED (real data) for 3,
  4 and 5; INFERRED for 1 and 2. The decoder has no parameters: the window
  is always 65,536 bytes.
- METHOD above 5: DECISION C8: the entry is skipped with the reason
  "unsupported method"; in a solid archive this ends the usable part of the
  chain.
- A packed file can be larger than the original; RAR 1.4 does not fall
  back to storing. (One real file has PACK_LEN 61,665 for UNP_LEN 61,655.)
- UNP_LEN = 0: an empty file. Nothing is decoded (`rar15.md` section 2).

## 3. The 16-bit checksum (sum16)

The CHECK_SUM field of the old container is computed over the **unpacked**
bytes of the file as follows.

1. Start with a 16-bit value S = 0.
2. For each byte of the file, in order:
   - add the byte (0..255) to S, keeping the low 16 bits;
   - then rotate S left by one bit as a 16-bit quantity: the new S is
     twice the old S, low 16 bits kept, plus 1 if the old S was 0x8000 or
     more.
3. The checksum is the final S.

The addition comes first, the rotation second, for every byte including
the last. An empty file has checksum 0.

Worked example, the three bytes `61 62 63`: S = 0; add 0x61: 0x0061;
rotate: 0x00C2; add 0x62: 0x0124; rotate: 0x0248; add 0x63: 0x02AB;
rotate: 0x0556. The checksum is 0x0556.

Test values:

| Input | sum16 |
|:--|:--|
| (empty) | 0x0000 |
| `01` | 0x0002 |
| `80 80` | 0x0300 |
| ASCII `123456789` | 0xC78A |
| ASCII `hello hello ` (12 bytes, trailing space) | 0xE1C8 |
| ASCII `RAR` | 0x0438 |
| the 256 bytes `00`, `01`, ..., `FF` | 0xC1FF |

Evidence: CONFIRMED (real data). The 282 complete files of the survey
(276 packed, 6 stored) all match. The algorithm is also described in a
comment of the Deark module.

The checksum is weak (16 bits, and insensitive to some rearrangements).
It is nevertheless the only integrity check the old container has, and the
decoder of `rar15.md` has none of its own.

Verification (DECISION C9): the container layer computes sum16 over the
bytes it is about to deliver for a file, packed or stored, and compares it
with CHECK_SUM of the file's **last** part (section 6.1). On a mismatch
the entry is skipped with the reason "checksum mismatch" and nothing is
delivered for it.

## 4. The old container: solid archives and volumes

### 4.1 Solid archives

Solidity is a property of the whole archive (HEAD_FLAGS bit 0x08); the
file headers carry no solid flag.

- In a solid archive all packed files form **one chain**, in archive
  order: the first packed file starts with a fresh decoder state, every
  later packed file continues the state left by the one before it
  (`rar15.md` section 11.2).
- In a non-solid archive every packed file is a chain of its own.
- Directories, files with UNP_LEN = 0, and stored files are not part of
  the chain and do not affect it. (For stored files this is INFERRED; no
  solid old archive with a stored member was found. Section 8.)
- The archive comment is not part of the chain (section 2.5).
- The chain runs on across volume boundaries (section 4.2).

CONFIRMED (real data): 8 solid archives with 85 packed files. Every file
after the first decodes correctly when the state is carried over, and
fails when it is decoded with a fresh state (tried on two archives: every
file after the first fails).

Consequences for a reader:

- Files must be decoded in order, including files the caller does not
  want.
- When a packed file of the chain cannot be decoded (encrypted,
  unsupported VER or METHOD, truncated, corrupt input reported by the
  decoder), no later packed file of the archive can be decoded either.
  DECISION C10: they are listed and skipped with the reason "depends on an
  unreadable earlier entry". A checksum mismatch on a file that decoded to
  exactly U bytes does not by itself stop the chain: later files are
  decoded and each is judged by its own checksum.
- Stored files and directories remain readable whatever happened to the
  chain.

### 4.2 Multi-volume sets

- Every volume is a complete old archive by itself: signature, archive
  header with flag 0x01 set, file blocks. The archive flags (solid and so
  on) are repeated in every volume.
- A file that does not fit continues in the next volume. Each part has its
  own file header with the same name, UNP_LEN, FTIME, ATTR, VER and METHOD.
  FLAGS bit 0x02 is set on every part but the last, bit 0x01 on every part
  but the first. PACK_LEN is the size of the part in that volume.
- The packed bytes of the file are the parts' data **concatenated in
  volume order**, with nothing added, removed or aligned at the joints,
  and the result is decoded once as one stream. The same holds for a
  stored file.
- A split file's part is always the last block of its volume when bit 0x02
  is set, and the first block of its volume when bit 0x01 is set.
- CHECK_SUM of the last part is the checksum of the whole file; see
  section 6.1 for the other parts.
- Volume names: RAR 1.39 and later name the volumes `name.RAR`,
  `name.R00`, `name.R01`, ... (stated by the RAR 1.40 change log; "names of
  earlier versions' volumes are also supported on extraction", without
  saying what they were). The first volume can be a self-extracting
  executable instead: the real set is `GAME.EXE`, `GAME.R00`, `GAME.R01`,
  `GAME.R02`. There is no volume number inside the archive; the order
  comes from the names. There is no "last volume" marker either: the set
  ends with the volume whose last block does not have bit 0x02.

CONFIRMED (real data) on one complete set: four volumes of 500,000,
500,000, 500,000 and 316,564 bytes, solid, 10 files; one file of
5,532,688 bytes is split across all four (parts of 466,007, 499,961,
499,961 and 303,995 packed bytes, flags 0x02, 0x03, 0x03, 0x01); the five
files after it in the last volume continue the solid chain. All ten match
their checksums.

DECISION C11, joining rules. When a part with bit 0x01 arrives while no
file is being collected, or its name or UNP_LEN differs from the part
being collected, or a part without bit 0x01 arrives while a file is still
being collected, or the volumes end while a file is being collected, the
incomplete file is listed and skipped with the reason "missing volume
part". In a solid archive such a file also stops the chain (C10). A
volume read on its own is a valid archive whose split entries are skipped
in this way.

## 5. RAR4-format archives: selecting the decoder

This section applies to file headers (block type 0x74) of archives with
the signature `Rar!\x1a\x07\x00`. The fields referred to are the usual
ones: the header flags (16 bits), the host OS byte, the file CRC (32
bits), the **unpack-version** byte, the **method** byte and the attributes
(32 bits).

### 5.1 The method byte

| Method byte | Meaning |
|:--|:--|
| 0x30 | Stored: the data bytes are the file's bytes. The unpack-version byte is irrelevant. P must equal U. |
| 0x31 .. 0x35 | Packed. Five effort levels ("fastest" to "best") of the packer; the stream format depends only on the unpack-version byte, never on the method byte. |
| anything else | DECISION V1: skipped entry, "unsupported method". |

### 5.2 The unpack-version byte

The byte holds ten times the major version plus the minor version of the
oldest RAR able to unpack the file.

| Version byte | Written by | Decoder for methods 0x31..0x35 | Window | Evidence |
|--:|:--|:--|:--|:--|
| 13 | (see note 1) | RAR 1.5 method, `rar15.md` | 64 KiB | DECISION V2 |
| 15 | RAR 1.50 to 1.5x | RAR 1.5 method, `rar15.md` | 64 KiB, fixed | CONFIRMED (real data): 1,950 files |
| 20 | RAR 2.0x | RAR 2.0 method, `rar20.md` | up to 1 MiB | CONFIRMED (real data), see `rar20.md` |
| 26 | RAR 2.6 to 2.8 (files larger than 2 GiB and later 2.x features) | RAR 2.0 method, `rar20.md` (same decoder as 20) | up to 1 MiB | REFERENCE-ONLY |
| 29 | RAR 2.9, 3.x, 4.x | RAR 2.9/3.x method (the decoder already present in this repository) | up to 4 MiB | REFERENCE-ONLY for the mapping |
| 36 | RAR 3.6 and later, for some archives | RAR 2.9/3.x method (same decoder as 29) | up to 4 MiB | REFERENCE-ONLY |
| any other value | | none: DECISION V3, skipped entry "unsupported unpack version N" | | |

Notes:

1. 13 is the number the RAR 1.3 generation is known by; the reference
   implementation uses it only to select that generation's cipher. Files
   in the old container carry VER = 2, not 13, and no RAR4-format archive
   with version byte 13 was seen. DECISION V2: should one occur, it is
   decoded with the RAR 1.5 decoder; the CRC-32 comparison shows whether
   that was right. (The reference implementation reports such a file as
   unsupported.)
2. In the surveyed archives only 15 and 20 occur (no archive written by
   RAR 2.9 or later was in that survey). One archive never mixes 15 and
   20 in the survey, but mixing is possible when an archive is updated by
   a newer RAR; the decoder is always chosen per file.
3. The decoder is selected by the version byte **alone**. In particular a
   version-15 file must never be given to the RAR 2.9/3.x decoder: that
   decoder does not fail on such data, it produces garbage of some length
   (this is the fault this specification was written to remove). Feeding a
   version-20 file to it fails with a decode error.
4. After decoding, the result must be exactly U bytes and must match the
   stored CRC-32 (section 6.2) for every version. Without that comparison
   a wrong dispatch or a damaged stream goes unnoticed.

### 5.3 Which files share decoder state (solid)

The rule depends on the version byte of the file.

**Version below 20 (that is, 13 and 15).** The file header has no solid
flag. Solidity is the archive's: main header (block type 0x73) flag bit
0x0008.

- Main header bit 0x0008 clear: every file is a chain of its own.
- Main header bit 0x0008 set: all packed files of the archive form one
  chain. The first packed, non-empty file starts with fresh state; every
  later one continues it. Directories and files with U = 0 take no part.

CONFIRMED (real data): 12 solid version-15 archives, 334 files continuing
a chain; file header flags of all 2,021 version-15 headers are 0x8000 or
0x8000 plus split bits, never with bit 0x0010. `wrar155.rar` is such an
archive (main header flags 0x002A; its second file needs the state left by
the first). CONFIRMED (crafted): section 9.2.

**Version 20 and above.** The file header flag bit 0x0010 says "this file
continues the state of the preceding file"; the main header bit is not
consulted. See `rar20.md` section 10.2.

Common rules:

- A chain belongs to one decoder. DECISION V4: a file that should continue
  a chain whose state belongs to a different decoder (for example a
  version-15 file after a version-20 file in a solid archive), or whose
  predecessor is unavailable, is a skipped entry "solid predecessor
  unavailable".
- A chain continues across volume boundaries. For version 15 this is
  INFERRED from the old container (section 4.2), where it is confirmed;
  no solid multi-volume version-15 set was available.
- When a member of a chain cannot be decoded, the later members are
  skipped as in section 4.1 (C10).
- Stored files (method 0x30) inside a version-15 solid archive: not
  observed (the nine stored version-15 files of the survey are in
  non-solid archives). DECISION V5, the same as `rar20.md` makes for its
  versions: a stored file is delivered as stored, does not enter the
  window and does not change the chain state. The CRC-32 of the following
  files shows whether this holds (section 8).

### 5.4 Directories in version-15 archives

From RAR 2.0 on, a directory is marked in the file header flags: the three
"dictionary size" bits 0x00E0 all set. RAR 1.5x does not use those bits
(they are 0 in all 2,021 version-15 headers). A version-15 entry is a
directory when **attribute bit 0x10 is set and the host OS byte is 0
(MS-DOS) or 1 (OS/2)**.

CONFIRMED (real data): 8 such entries with host OS 0 and 51 with host OS
1; all have U = 0, P = 0, method 0x33 or 0x35 and a name without a
trailing separator, and every one of the 51 OS/2 entries has files below
it in the same archive. The reference implementation applies the rule
only for host OS 0 and lists the OS/2 directories as empty files
(section 10).

DECISION V6: for version 15 (and 13), the rule above; an entry so marked
with U or P other than 0 is listed as a directory with a finding and its
data is skipped. For other host OS values with version 15 the attribute is
not interpreted and the entry is a file (never observed).

### 5.5 Other blocks seen in version-15 archives

These need no interpretation but a reader must step over them correctly.
Every block has the common 7-byte block header (CRC-16, type, flags,
header size); when flag bit 0x8000 is set, or the block is a file header,
a 4-byte data size follows and that many data bytes follow the header.

| Type | Seen as | Notes |
|:--|:--|:--|
| 0x75 | Old-style archive comment, nested **inside** the main header (the main header's size is 13 plus the comment block's size; main header flag 0x0002). | Layout after its 7-byte block header: unpacked size (2 bytes), unpack version (1), method (1), comment CRC (2), then the packed comment. In `wrar155.rar`: version 15, method 0x34, unpacked size 1,855; the data is a plain (not obfuscated) RAR 1.5 stream decoded with a fresh state of its own, and the CRC field is the low 16 bits of the CRC-32 of the unpacked comment (0x8E91). Independent of the files' chain. |
| 0x76 | Old-style authenticity information, after the last file. | Skipped by header size. Its stored header CRC does not match the usual computation; it must not be verified. |
| 0x77 | Old-style sub-block (flags 0xC000, header size 24, with data), following a file header. 96 occurrences in one OS/2 archive (extended attributes), which is not solid. | Skipped by header size plus data size. Its packed data is not part of any chain (INFERRED: the archive is not solid; its files decode correctly with the blocks skipped). |
| 0x78 | Recovery record (version-20 archives). | Skipped by header size plus data size. |

The header CRC-16 of a block is the low 16 bits of the CRC-32 of the
header bytes from the type byte to the end of the header. CONFIRMED (real
data) for all 3,474 file headers. For a main header it covers only the 11
bytes from the type byte to the end of the fixed 13-byte part, even when a
comment block is nested behind it (24 of 24 such headers).

## 6. Checksums and file contents

### 6.1 Old container (sum16)

| Header | CHECK_SUM holds |
|:--|:--|
| File stored in one volume | sum16 of the complete unpacked file. CONFIRMED. |
| **Last** part of a split file (bit 0x01 set, bit 0x02 clear) | sum16 of the complete unpacked file, all parts together. CONFIRMED (one file of 5.5 MB in four parts). |
| Any other part (bit 0x02 set) | A different value, of no use to a reader. It is not the sum16 of the part's packed bytes. In the one real set, each of the three values equals the sum16 of a prefix of the unpacked file that ends 4,393 to 35,221 bytes beyond the point the decoder has reached when that part's packed bytes are used up; presumably it is the running checksum of what the packer had read from the source file when it closed the volume. INFERRED. |

DECISION C12: only the last part's CHECK_SUM is compared; the others are
ignored.

### 6.2 RAR4 format (CRC-32)

For every unpack version the 32-bit file CRC field relates to the data as
follows.

| Header | File CRC field holds | Evidence |
|:--|:--|:--|
| File stored in one volume (any version, packed or stored) | CRC-32 of the complete **unpacked** file. | CONFIRMED (real data): 1,950 packed and 9 stored version-15 files; 68 stored version-20 files; `rar20.md` for packed version-20 files |
| **Last** part of a split file (flag 0x0001 set, 0x0002 clear) | CRC-32 of the complete **unpacked** file, all parts together. It is not the CRC of the last part's packed bytes. | CONFIRMED (real data): 2 version-15 files; version 20 in `rar20.md` |
| Other parts, **version 15** | The constant 0xFFFFFFFF. No per-part check exists. | CONFIRMED (real data): 2 of 2 parts in a three-volume set written by RAR 1.5x |
| Other parts, **version 20 and above** | Either the constant 0xFFFFFFFF (early RAR 2.0x writers), or the CRC-32 of that part's **packed** bytes as stored in the volume (for an encrypted file: of the encrypted bytes). | CONFIRMED (real data) for version 20: 12 parts in three sets carry 0xFFFFFFFF, 30 parts carry the packed CRC, none anything else; REFERENCE-ONLY for 29 and 36 |
| Directory, empty file | 0 (the CRC-32 of no bytes). | CONFIRMED |

Rules for a reader:

1. The CRC-32 to verify a file against is the one in the header of its
   **last** part. It is compared with the CRC-32 of the U bytes produced by
   the decoder (or copied, for a stored file). DECISION V7: on a mismatch
   the entry is skipped with the reason "checksum mismatch"; nothing is
   delivered.
2. The CRC fields of non-final parts must **not** be compared with the
   unpacked data.
3. A non-final part's CRC may be used to detect a damaged volume early:
   for version 20 and above, when the field is not 0xFFFFFFFF, it should
   equal the CRC-32 of the part's packed bytes; a mismatch means the entry
   is "corrupt" before any decoding. For version 15, and whenever the
   field is 0xFFFFFFFF in any version, there is nothing to check. (The reference
   implementation treats 0xFFFFFFFF as "no CRC available" for every
   version.) This check is optional; rule 1 is not.
4. In a solid chain each file has its own CRC over its own U bytes. The
   CRC does not cover earlier members.
5. A file's result is checked against its declared size first: the
   decoder must have produced exactly U bytes (for stored files, P must
   equal U).

## 7. Encryption: recognising and skipping

No cipher is specified here and no password is accepted. Encrypted
entries are listed and skipped with the reason "encrypted".

### 7.1 Where encryption is flagged

| Container | Flag | Meaning |
|:--|:--|:--|
| Old container | File header FLAGS bit 0x04 | This file's data is encrypted. TECHNOTE; not observed. |
| RAR4 format | File header flag bit 0x0004 | This file's data is encrypted. |
| RAR4 format | File header flag bit 0x0400 | An 8-byte salt is stored in the file header after the name (version 29 and above only). It is inside the header size, so skipping by header size needs no special handling. |
| RAR4 format | Main header flag bit 0x0080 | **The block headers themselves are encrypted** (RAR 3.x and later). Everything after the main header is unreadable without the password: no entry can be listed. The archive is reported as encrypted as a whole. |
| RAR4 format | Main header flag bit 0x0200 | A one-byte "encryption version" follows the main header's reserved fields. Informational. |

Encryption is per file. An archive can mix encrypted and plain files.
Names and sizes of encrypted files are readable (unless main header bit
0x0080 is set).

### 7.2 What differs by version

Only the facts a non-decrypting reader can use:

| Generation | Cipher | Effect on sizes and checksums |
|:--|:--|:--|
| Old container (the "1.3" scheme) | A byte-wise stream transformation keyed by the password (the same family as the comment obfuscation of section 2.5, with password-derived starting values). | P is unchanged. CHECK_SUM is of the plain unpacked file. |
| Version 15 | A byte-wise stream cipher keyed by the password. | P is unchanged. The file CRC is of the plain unpacked file. |
| Versions 20, 26 | A block cipher working on 16-byte blocks. | The file CRC of a whole file or last part is of the plain unpacked file. |
| Versions 29, 36 | AES in 16-byte blocks, key derived from the password and the optional salt. | As for 20; non-final parts carry the CRC-32 of the encrypted bytes. |

All of this table is REFERENCE-ONLY. In every generation the data is
packed first and encrypted afterwards, so the encrypted bytes cannot be
given to a method decoder: they must not be "tried". A stored encrypted
file is equally unreadable.

### 7.3 Encrypted entries and chains

A packed encrypted file inside a solid chain changes the decoder state in
a way that cannot be reproduced without the password. Therefore, once an
encrypted packed member of a chain has been skipped, every later member of
the same chain is skipped as well ("depends on an unreadable earlier
entry"), up to the next file that starts a new chain: in a version-15 or
old-container solid archive that means every later packed file of the
archive. DECISION V8. Stored plain files and directories stay readable.

## 8. Open points

None blocks an implementation; each says what to do meanwhile.

1. **Stored files inside a solid archive** of the old container or of
   version 15: whether they pass through the window. Not observed.
   Specified: they do not (sections 4.1, 5.3). If a real archive
   contradicts this, the packed files after the stored one will fail their
   checksums, and the alternative to try is to append the stored bytes to
   the chain's window (and count them in the chain's history) without
   touching any other state.
2. **File comments in the old container** (flag 0x08): field order
   unconfirmed; they are skipped by HEAD_LEN (C3).
3. **Encrypted old-container files** (flag 0x04): never observed; skipped.
4. **VER values 0 and 1** of the old container (RAR 0.99, 1.00): no
   samples, method unknown; skipped (C5).
5. **Volume names before RAR 1.39.** Unknown. A caller that joins volumes
   by name can only support the `.RAR`/`.R00` scheme (and an `.EXE` first
   volume).
6. **The CHECK_SUM of non-final parts** (section 6.1): meaning inferred;
   ignored.
7. **Unpack-version byte 13 in a RAR4-format archive**: never observed
   (V2).
8. **Solid version-15 sets across volumes**: inferred from the old
   container.
9. **Header flag bits 0x40 and 0x80 of the old archive header**: meaning
   unknown; ignored.

## 9. Worked examples

Both examples contain the same two files, the solid pair of `rar15.md`
test vector T11: `A.TXT` = "hello hello " (12 bytes, CRC-32 0x9FB1A373,
sum16 0xE1C8, packed `1B D4 7A 5F 58 2F 5F 27 87`) and `B.TXT` =
"hello!hellh" (11 bytes, CRC-32 0x5415E046, sum16 0x70F6, packed
`F3 F0 D2 CA 08`), the second continuing the chain of the first.

### 9.1 A solid old-container archive (73 bytes)

Synthetic, built from this document; checked only with the author's
throwaway reader (no black-box tool reads this container).

| Offset | Bytes | Meaning |
|--:|:--|:--|
| 0 | `52 45 7E 5E` | Signature. |
| 4 | `07 00` | Archive HEAD_LEN = 7. |
| 6 | `08` | HEAD_FLAGS: solid. |
| 7 | `09 00 00 00` | PACK_LEN = 9. |
| 11 | `0C 00 00 00` | UNP_LEN = 12. |
| 15 | `C8 E1` | CHECK_SUM = 0xE1C8. |
| 17 | `1A 00` | File HEAD_LEN = 26 = 21 + 5. |
| 19 | `00 A0 21 1C` | FTIME 0x1C21A000: 1994-01-01 20:00:00. |
| 23 | `20` | ATTR: archive. |
| 24 | `00` | FLAGS. |
| 25 | `02` | VER. |
| 26 | `05` | NAME_LEN. |
| 27 | `03` | METHOD 3. |
| 28 | `41 2E 54 58 54` | `A.TXT` |
| 33 | `1B D4 7A 5F 58 2F 5F 27 87` | Data (9 bytes). |
| 42 | `05 00 00 00` | PACK_LEN = 5. |
| 46 | `0B 00 00 00` | UNP_LEN = 11. |
| 50 | `F6 70` | CHECK_SUM = 0x70F6. |
| 52 | `1A 00` | File HEAD_LEN = 26. |
| 54 | `00 A0 21 1C` | FTIME. |
| 58 | `20 00 02 05 03` | ATTR, FLAGS, VER, NAME_LEN, METHOD. |
| 63 | `42 2E 54 58 54` | `B.TXT` |
| 68 | `F3 F0 D2 CA 08` | Data (5 bytes). End of archive at 73. |

Expected listing: `A.TXT` (12 bytes) and `B.TXT` (11 bytes), both
extracted and verified. With HEAD_FLAGS changed to `00` (not solid),
`B.TXT` must fail: decoding it with a fresh state is corrupt input
(`rar15.md` T11).

### 9.2 The same pair as a solid RAR4-format archive (108 bytes)

CONFIRMED (crafted): `unar` 1.10.7 extracts both files from this archive
and accepts both CRCs.

| Offset | Bytes | Meaning |
|--:|:--|:--|
| 0 | `52 61 72 21 1A 07 00` | Signature. |
| 7 | `3B D0` | Main header CRC-16 0xD03B. |
| 9 | `73` | Block type: main header. |
| 10 | `08 00` | Flags 0x0008: solid. |
| 12 | `0D 00` | Header size 13. |
| 14 | `00 00 00 00 00 00` | Reserved. |
| 20 | `34 F9` | File header CRC-16 0xF934. |
| 22 | `74` | Block type: file header. |
| 23 | `00 80` | Flags 0x8000 (no solid bit: version 15 has none). |
| 25 | `25 00` | Header size 37. |
| 27 | `09 00 00 00` | Packed size 9. |
| 31 | `0C 00 00 00` | Unpacked size 12. |
| 35 | `00` | Host OS 0 (MS-DOS). |
| 36 | `73 A3 B1 9F` | File CRC-32 0x9FB1A373. |
| 40 | `00 00 21 21` | Time 0x21210000: 1996-09-01 00:00:00. |
| 44 | `0F` | Unpack version 15. |
| 45 | `33` | Method 0x33. |
| 46 | `05 00` | Name length 5. |
| 48 | `20 00 00 00` | Attributes 0x20. |
| 52 | `41 2E 54 58 54` | `A.TXT` |
| 57 | `1B D4 7A 5F 58 2F 5F 27 87` | Data (9 bytes). |
| 66 | `07 71` | File header CRC-16 0x7107. |
| 68 | `74 00 80 25 00` | Type, flags 0x8000, header size 37. |
| 73 | `05 00 00 00 0B 00 00 00` | Packed size 5, unpacked size 11. |
| 81 | `00` | Host OS 0. |
| 82 | `46 E0 15 54` | File CRC-32 0x5415E046. |
| 86 | `00 00 21 21 0F 33 05 00 20 00 00 00` | Time, version 15, method 0x33, name length 5, attributes. |
| 98 | `42 2E 54 58 54` | `B.TXT` |
| 103 | `F3 F0 D2 CA 08` | Data (5 bytes). End of archive at 108. |

Expected listing: as in 9.1. `B.TXT` is solid only because of the main
header flag; a reader that looks for file header flag 0x0010 decodes it
with a fresh state and fails.

### 9.3 Real archives

| Archive | Container | What it shows |
|:--|:--|:--|
| `wrar155.rar` (repository sample collection) | RAR4 format, main flags 0x002A (comment, solid, authenticity) | Two version-15 files forming one chain: `WINRAR.EXE` 182,528 bytes, CRC-32 0xDDB4B9DD; `RAR.HLP` 20,272 bytes, CRC-32 0x3F1276DF. A nested 0x75 comment block of 1,855 unpacked bytes. |
| `RAR1_402.EXE` (the RAR 1.40.2 distribution, 106,408 bytes) | Old container at offset 6,491, flags 0xBE (comment, locked, solid, packed comment, EXT1) | Ten files in one chain, among them `RAR.EXE` 74,812 bytes with sum16 0x8CF8 and `TECHNOTE.DOC` 4,758 bytes with sum16 0xE106. Packed comment of 1,638 bytes. |
| `ANY2RAR.RAR` (inside the former, 3,968 bytes) | Old container, flags 0xBA | Seven files in one chain; the first, `README.A2R`, is 1,281 bytes with sum16 0x07DE. Packed comment of 1,435 bytes. |

Real archives are kept outside the repository.

## 10. Observed implementation gaps

Faults of the tools examined. They explain rules; none is to be imitated.

1. **The reference implementation does not read the old container.** It
   recognises the signature and then reports the archive as unsupported.
2. **OS/2 directories of version-15 archives** are listed by the
   reference implementation as zero-length files (section 5.4).
3. **Unpack version 13** is known to the reference implementation only as
   a cipher selector; a packed file with that version byte is unsupported
   there (V2).
4. **Stored members and solid chains.** The reference implementation
   builds its chains from all files regardless of method, and would hand a
   stored member's bytes to the decoder when a later member is read
   (section 8, point 1).
5. **Header CRCs are not verified** by the reference implementation at
   all ("RAR uses it completely inconsistently" is its stated reason;
   section 5.5 shows where the inconsistency lies: nested comment blocks
   and type 0x76).
6. **The existing reader of this repository** (before this specification)
   ignores the unpack-version byte and gives every packed file to the RAR
   2.9/3.x decoder, and compares neither the size nor the CRC-32 of the
   result. Version-15 files come out as garbage of the wrong length
   without any error; version-20 files abort the archive.
7. **Deark** lists old-container members and documents sum16 but does not
   unpack them; its statement about the position of file comments could
   not be checked (C3).

## 11. Decisions

| # | Section | Decision |
|--:|:--|:--|
| C1 | 2.1 | An old archive is recognised by signature plus structural checks; searching inside executables is left to the layer that handles self-extracting files. |
| C2 | 2.2 | Archive header validation; HEAD_LEN, not the known fields, locates the first file header. |
| C3 | 2.3 | The name is at file header offset 21; anything between the name and HEAD_LEN is skipped. File comments are not extracted. |
| C4 | 2.3 | Unknown file flag bits are ignored. |
| C5 | 2.3 | VER other than 2: skipped entry "unsupported version". |
| C6 | 2.3 | File header validation and the three ways a listing can stop (clean end, corrupt header, truncated data). |
| C7 | 2.4 | ATTR bit 0x10 makes a directory; sizes other than 0 are a finding and the data is skipped. |
| C8 | 2.6 | METHOD above 5: skipped entry "unsupported method". |
| C9 | 3 | sum16 is verified for every file, packed or stored; a mismatch means nothing is delivered. |
| C10 | 4.1 | After an undecodable member of a chain, later members are skipped; a bare checksum mismatch does not stop the chain. |
| C11 | 4.2 | Joining rules for split files; an incomplete file is skipped as "missing volume part". |
| C12 | 6.1 | Only the last part's CHECK_SUM is used. |
| V1 | 5.1 | Method bytes other than 0x30..0x35: "unsupported method". |
| V2 | 5.2 | Unpack version 13 is decoded like 15. |
| V3 | 5.2 | Unknown unpack versions: "unsupported unpack version N". The version byte alone selects the decoder. |
| V4 | 5.3 | A file that would continue a chain of another decoder, or a missing one: "solid predecessor unavailable". |
| V5 | 5.3 | Stored members do not touch a chain. |
| V6 | 5.4 | Version-15 directories: attribute bit 0x10 with host OS 0 or 1. |
| V7 | 6.2 | The CRC-32 of the last part is verified against the unpacked data for every version; a mismatch means nothing is delivered. Non-final CRCs are never compared with unpacked data. |
| V8 | 7.3 | An encrypted packed member ends the usable part of its chain. |

## 12. Safety requirements for the container layer

1. **Every length is untrusted.** HEAD_LEN, COMM_LEN, EXT1_LEN, NAME_LEN,
   PACK_LEN and UNP_LEN are checked against the bytes actually available
   (L) before use. All offset arithmetic is done without overflow: with
   32-bit PACK_LEN and 16-bit HEAD_LEN the sums fit easily in 64 bits.
2. **Progress.** Every file block advances the read position by at least
   21 bytes (C6 rejects shorter headers), so a listing loop always
   terminates within L / 21 steps.
3. **No allocation from declared sizes.** UNP_LEN is never used to reserve
   memory before data exists. The joined packed bytes of a split file are
   bounded by the real sizes of the volumes.
4. **Limits.** The caller's limits on the unpacked size of one entry and
   of one archive apply (`rar15.md` section 12, R4); exceeding one is a
   skipped entry "output limit exceeded". In a solid archive the files
   before a wanted file must still be decoded; their output counts toward
   the archive limit but need not be kept.
5. **Names** go through the repository's safe-path handling; a name is at
   most 255 bytes in the old container.
6. **Nothing unverified is delivered**: an entry's contents are released
   only after the size check and the checksum comparison (C9, V7).
7. **Per-entry failure.** Every failure listed in this document is a
   skipped entry with a reason (or, where stated, the end of the listing
   with what was listed so far); none aborts the entries that are
   independent of it.

## Clean-room feedback

Notes from the implementation side (clean room); prose only, not written
from any decoder's source.

1. **C1, self-extracting archives.** The search that C1 leaves to "the
   layer that handles self-extracting files" is done by the RAR reader
   itself, with one rule for all three containers: see the feedback
   section of `rar4-container.md`. At the found offset the structural
   checks of section 2.2 apply unchanged. The RAR 1.402 distribution
   (`RAR1_402.EXE`, archive at offset 6,491) is read this way; its nine
   members match their references.
2. **C11 together with C10.** A split file that is abandoned because
   the next volume brings a different file stops the chain when it was
   packed, and does not when it was stored (a stored file takes no part
   in the chain, section 4.1). The same holds when the next volume is
   missing altogether.
3. **Names of the results of C10 and V4**: see item 2 of the feedback
   section of `rar20.md`.
