# RAR 5.0 archive container (signature `Rar!` 1A 07 01 00)

Format specification for clean-room implementation. This document describes
the archive container introduced by RAR 5.0 and still written by RAR 7:
how the archive is found inside a file, the variable-length integers, the
block structure and its checksum, every header type and every extra record,
names, directories, links, file checksums (CRC-32 and BLAKE2sp),
multi-volume sets, which members of a solid archive share decoder state,
and how encrypted archives and members are recognised and skipped.

It does not describe the compressed data of a member (see
`rar5-compression.md`; the interface this document assumes is in section
2.3), the older container with the 7-byte signature (`rar4-container.md`),
or any cipher or key derivation.

Contents: 1 how to read this document; 2 what the container layer is given
and returns; 3 primitive encodings; 4 finding the archive; 5 blocks; 6
block types; 7 extra records of file and service headers; 8 service
headers; 9 member kinds and their contents; 10 checksums; 11 multi-volume
sets; 12 solid archives; 13 encryption; 14 reading order, summary; 15
worked examples; 16 test vectors; 17 safety requirements; 18 observed
implementation gaps; 19 decisions; 20 open questions; 21 evidence.

## 1. How to read this document

### 1.1 Provenance

- This is a **prose specification**. Its primary source is RARLAB's public
  technical note "RAR 5.0 archive format"
  (`https://www.rarlab.com/technote.htm`; called the *technical note*),
  which describes data structures only. What the note leaves open was
  settled by studying the RAR 5 archive parser of The Unarchiver's
  XADMaster library (LGPL, revision `7cb9ee0a` of
  `github.com/MacPaw/XADMaster`; the *reference implementation*), by
  black-box testing of three independent readers (`unar`/`lsar` 1.10.7,
  built from the reference implementation; `7zz` 26.01; `bsdtar` 3.5.3)
  with purpose-built archives, and by measurements on archives written by
  the vendor's own `rar` 7.21 and on real archives (section 21).
- The official `unrar` source was **not** consulted; its licence forbids
  using it to recreate the algorithm. Neither was the source of any reader
  other than the reference implementation.
- The work is a **clean-room process**. The author of this document could
  read the reference implementation; the implementation is written **from
  this document only**. The document therefore contains no program code
  and no pseudo-code, only prose, tables, worked examples and test
  vectors.
- Where the reference implementation is wrong or unsafe the document says
  so (section 18) and specifies the correct behaviour instead. Being
  compatible with any particular reader is not a goal; reading genuine
  archives correctly and rejecting hostile input safely is.

### 1.2 Evidence labels

| Label | Meaning |
|---|---|
| CONFIRMED (real data) | Observed in archives written by `rar` 7.21 for this document or in real archives written by WinRAR (section 21), and consistent with how the black-box readers read them. |
| CONFIRMED (crafted) | Checked by giving the black-box readers a purpose-built archive and comparing their result with the result predicted by this document. |
| TECHNOTE | Stated by the technical note; not separately observed. |
| REFERENCE-ONLY | Taken from the reference implementation; not separately observable. |
| INFERRED | Deduced; not confirmed. |
| DECISION | A choice made by this specification, not a fact about the format (section 19). |

A rule without a label in sections 3 to 8 is **stated by the technical
note and CONFIRMED (real data)**. A rule without a label in sections 9
to 14 inherits the label of the paragraph or list that contains it; the
numbered rules of 12.2 are, taken together, a DECISION (K26) built on
the evidence listed in 12.3.

### 1.3 Terms and numbers

| Term | Meaning |
|---|---|
| volume | One file of an archive. An archive that is not split has one volume. |
| block | A header, optionally followed by a data area (section 5). |
| header | The checksummed part of a block. **H** is the value of its size field. |
| data area | The bytes that follow a header. **S** is their number. |
| file header | A block of type 2. |
| piece | The data area of one file header. A file split across volumes has one piece per volume. |
| member | One archived object (file, directory or link) after the pieces of a split file have been joined. |
| **P** | The packed size of a member: the sum of the sizes of its pieces. |
| **U** | The unpacked size the member's header declares. Untrusted. |
| **D** | The dictionary size the member's header declares (section 6.3). Untrusted. |
| stored | Compression method 0: the packed bytes are the unpacked bytes. |
| decoder | The decompressor of `rar5-compression.md`. |
| solid group | A run of members that share one decoder state (section 12). |
| vint | The variable-length integer of section 3.2. |
| malformed | A header that passes its checksum but breaks a rule of this document. |
| diagnostic | A warning the reader records; reading continues. |

Numbers are decimal unless prefixed `0x`. Byte strings are written as
hexadecimal digits in code style (`0a ac`). Ranges written `a..b` include
both ends. All offsets are byte offsets from the start of a volume unless
stated otherwise. "Bit n" of a number is the bit of weight 2^n.

## 2. What the container layer is given and returns

### 2.1 Input

- The bytes of the first volume, with random access or as a forward
  stream (the reading order of section 14 never moves backwards within a
  volume, except to fetch the pieces of a member when it is extracted).
- For a multi-volume set, a way to obtain the following volumes in order
  (section 11.5).

### 2.2 Output

An ordered list of members, in the order of their (last) file headers, and
an optional **stop reason** for the archive as a whole. A member carries:

| Field | Source |
|---|---|
| name | 6.5; with a version suffix when 7.5 applies |
| kind | regular file, directory, Unix symbolic link, Windows symbolic link, Windows junction, hard link, file copy, unknown redirection (9.1) |
| link target | 7.6, for the link kinds |
| U, or "unknown" | 6.2, 9.4 |
| pieces | list of (volume index, offset, length); P is the sum of the lengths |
| host OS and attributes | 6.4 |
| modification, creation and access time | 6.2, 7.4; each optional, seconds and nanoseconds |
| owner | 7.7, optional |
| CRC-32 and/or BLAKE2sp of the contents | 6.2, 7.3, 10; each optional |
| compression: algorithm, method, D, solid bit | 6.3 |
| encrypted | 7.2 |
| status | readable, or failed with one of the reasons below |

Member failure reasons used in this document: **encrypted** (13),
**unsupported** (6.3), **incomplete** (pieces missing, 11),
**truncated** (data area cut off, 5.4), **corrupt** (size rules of 9.2
and 9.3, a checksum mismatch, or a decoder error), **no solid state**
and **poisoned group** (12).

Stop reasons: **not a RAR 5 archive** (4), **encrypted headers** (13.1),
**corrupt header** and **malformed header** (5), **truncated** (5),
**missing volume** (11). When reading stops, the members found before the
stop remain valid results. A missing end-of-archive header is only a
diagnostic (6.7).

### 2.3 Interface to the decoder

The decoder is specified in `rar5-compression.md`. This document assumes
it is used as follows, for every member that section 12 calls a
**decoder member**:

- It is given the packed bytes of the member (P bytes; the pieces
  concatenated in volume order), U or the fact that U is unknown (9.4),
  the algorithm (6.3), D, and either a fresh state or the state left by
  the previous decoder member of the same solid group.
- It returns exactly U bytes (or, when U is unknown, bytes until its
  stream says the member has ended) or an error.
- A decoder member may have **U = 0 and P greater than 0** (CONFIRMED
  (real data), section 12.3). The decoder must still read that member's
  stream as far as needed to establish the state (the transmitted tables)
  that the next member of the group relies on. Skipping such a member
  breaks the members after it.
- The container layer never interprets packed bytes of a compressed
  member, and verifies the checksums of section 10 over the bytes the
  decoder returns.

## 3. Primitive encodings

### 3.1 Fixed-width integers

`byte`, `uint16`, `uint32` and `uint64` are unsigned and stored least
significant byte first.

### 3.2 vint

A vint is 1 to 10 bytes. The low 7 bits of each byte are data, least
significant group first; bit 7 of a byte is set when another byte follows.
The value is a 64-bit unsigned integer.

Rules for readers:

- Byte k (counting from 0) contributes its low 7 bits at weight 2^(7k).
- The tenth byte may only be `00` or `01` (it supplies bit 63). A tenth
  byte with bit 7 set, or with any of bits 1..6 set, makes the header
  malformed (DECISION; `7zz` rejects an 11-byte vint and silently drops
  the excess bits of a 10-byte one, the reference implementation accepts
  any length; CONFIRMED (crafted), vectors E3 and E4).
- **Encodings longer than necessary are valid.** RAR reserves space
  before it knows a value and pads with high-order zero groups: `80 00`
  is 0, `ad 00` is 45, `f5 98 80 00` is 3189. Real archives contain such
  fields throughout (data size, unpacked size, compression information,
  locator offsets). A vint padded to the full 10 bytes is valid
  (CONFIRMED (crafted), vector C10).
- A vint must end inside the region that contains it (the header, the
  type-specific fields, or an extra record). Running out of bytes makes
  the header malformed, or the extra record invalid (7.1).

| Bytes | Value |
|---|--:|
| `00` | 0 |
| `7f` | 127 |
| `80 01` | 128 |
| `a4 83 02` | 33188 (octal 100644) |
| `80 00` | 0 |
| `8e 80 80 80 80 80 80 80 80 00` | 14 |
| `ff ff ff ff f7 ff ff ff 7f` | 9223372034707292159 (`0x7FFFFFFF7FFFFFFF`) |
| `ff ff ff ff ff ff ff ff ff 01` | 18446744073709551615 |
| `ff ff ff ff ff ff ff ff ff 02` | invalid (bit 64) |
| `80 80 80 80 80 80 80 80 80 80 00` | invalid (11 bytes) |

### 3.3 CRC-32

Every CRC-32 in this format is the common one: reflected polynomial
`0xEDB88320`, initial value `0xFFFFFFFF`, final complement; the ASCII
string `123456789` gives `0xCBF43926`; the empty string gives 0. It is
stored as a `uint32`.

## 4. Finding the archive

- A RAR 5 archive starts with the 8 bytes
  `52 61 72 21 1a 07 01 00` (`Rar!`, 1A, 07, 01, 00). The first block
  follows immediately.
- The 7 bytes `52 61 72 21 1a 07 00` are the older format
  (`rar4-container.md`). A seventh byte other than `00` or `01`, or
  `01` not followed by `00`, is not a format this document describes
  (DECISION: not a RAR 5 archive).
- Data may precede the signature: a self-extracting module (SFX). Its
  size and contents are undefined; RAR assumes at most 1 MB (TECHNOTE).
  The module shipped with `rar` 7.21 for macOS is 243,416 bytes
  (CONFIRMED (real data)). In a multi-volume self-extracting set only
  the first volume has a module (11.5).
- DECISION: the signature may **start at any offset from 0 to 1,048,576
  inclusive**. Later offsets are not searched.
- DECISION: a position where the 8 bytes match is only a **candidate**.
  It is accepted when the block that follows it is well-formed under
  section 5.1 to 5.3 (size field of at most 3 bytes, H at least 2, header
  inside the file, CRC-32 correct, extra size inside the header) and has
  type 1 or 4. The type-specific fields of that block are **not**
  examined for acceptance: a candidate whose first block passes these
  checks is the archive, and an error found later in that block is
  reported as usual (5.3). Otherwise the search continues one byte
  later. At most 64 candidates are examined, accepted or not; after the
  64th has been rejected, or when the search range is exhausted, the
  file is **not a RAR 5 archive**. A file shorter than 15 bytes (the
  signature and the smallest block) is never one. The black-box readers
  take the first match without checking it (vector C9 shows the
  difference); the check keeps an SFX module that happens to contain the
  8 bytes from hiding the archive, and the limit bounds the work.
- The same search is applied to every volume of a set (DECISION; the
  reference implementation assumes the signature of every later volume is
  at offset 0).

## 5. Blocks

After the signature a volume is a sequence of blocks. Nothing separates
them; each is found from the sizes in the one before it.

### 5.1 Layout

| Field | Size | Meaning |
|---|---|---|
| header CRC | uint32 | CRC-32 of everything from the first byte of the header size field to the last byte of the header (the size field, then H bytes). |
| header size | vint, **at most 3 bytes** | H: number of bytes from the header type field to the end of the header, extra area included. At most 2,097,151. |
| header type | vint | 1 main archive header, 2 file header, 3 service header, 4 archive encryption header, 5 end of archive header. |
| header flags | vint | Table below. |
| extra area size | vint | E. Present only if flag `0x0001` is set. |
| data size | vint | S. Present only if flag `0x0002` is set. |
| type-specific fields | | Section 6. |
| extra area | E bytes | The **last E bytes of the header**. Records of section 5.5. |
| data area | S bytes | Follows the header. Not covered by H or by the header CRC. |

Header flags:

| Bit | Meaning | Reader |
|---|---|---|
| `0x0001` | extra area present | selects the E field |
| `0x0002` | data area present | selects the S field |
| `0x0004` | a block of unknown type carrying this flag is dropped when an archive is rewritten | ignore |
| `0x0008` | data area continues from the previous volume | section 11 |
| `0x0010` | data area continues in the next volume | section 11 |
| `0x0020` | block depends on the preceding file block | ignore |
| `0x0040` | preserve a child block if the host block is modified | ignore |
| others | undefined | ignore |

Observed flag values in real archives: main header `0x05` or `0x04`; file
headers `0x03`, `0x02`, and with the volume bits `0x13`, `0x1b`, `0x0b`,
`0x12`, `0x1a`, `0x0a`; end of archive `0x04`; comment `0x02`; quick open
`0x06`; recovery record `0x07`.

### 5.2 Reading one block

At a block position p in a volume of length N:

1. If p equals N, the volume ended without an end-of-archive header: a
   diagnostic; reading of this volume ends normally (6.7).
2. Fewer than 7 bytes left: stop, **truncated**.
3. Read the CRC, then the header size vint from at most the next 3 bytes.
   A size field whose third byte still has bit 7 set: stop, **malformed
   header** (CONFIRMED (crafted): `7zz` rejects a 4-byte field; DECISION
   to do the same; vector E2). H less than 2: stop, **malformed header**
   (vector E7).
4. If the header does not end inside the volume: stop, **truncated**.
5. Compute the CRC-32 of the size field and the H header bytes. If it
   differs from the stored value: stop, **corrupt header** (DECISION, as
   `7zz` and `bsdtar`; the reference implementation never checks it;
   vector E1). No attempt is made to find a later block.
6. Inside the H bytes read the type, the flags, then E and S when their
   flags are set. E greater than the number of header bytes that remain
   after these fields: stop, **malformed header** (vector E5).
7. The type-specific fields occupy the bytes between the S field (or the
   last common field present) and the extra area. They are parsed
   strictly inside that region.
8. If the data area does not end inside the volume (use arithmetic that
   cannot wrap; S may be any 64-bit value): stop, **truncated** (5.4).
9. The next block starts at the end of the data area.

Every block advances the position by at least 7 bytes, so the walk ends.

### 5.3 Rules common to all block types

- **Bytes between the last field this document defines and the extra
  area are ignored** (DECISION, for compatibility with fields added
  later; the reference implementation and `bsdtar` do the same, `7zz`
  reports a header error; CONFIRMED (crafted), vector C11). The extra
  area is located from the end of the header, never from where the known
  fields end.
- A type-specific field that does not fit in its region: stop,
  **malformed header** (vector E6).
- **Unknown header types** (anything other than 1..5, including 0) are
  skipped, header and data area, with a diagnostic (DECISION; flag
  `0x0004` shows the format expects readers to meet them; the reference
  implementation skips them, `7zz` and `bsdtar` refuse the archive;
  CONFIRMED (crafted), vector C12).
- The **first block** of a volume must be of type 1, or of type 4
  (13.1). Section 4 already guarantees this for an accepted candidate.
  A block of type 1 or 4 anywhere else: stop, **malformed header**
  (DECISION; vector E14).
- The data area of a block whose type has no use for one (main header,
  end of archive header, a directory's file header) is skipped
  (CONFIRMED (crafted) with the reference implementation).

### 5.4 Truncated data areas

When the data area of a **file header** runs past the end of its volume,
the header is still parsed; if it parses, its member is reported with
status failed, **truncated**, and without pieces. It is never joined to
an open member (11.2); an open member is closed as incomplete first.
Reading then stops (vectors E8 and E9).

### 5.5 Extra area, general form

The extra area is a sequence of records:

| Field | Size | Meaning |
|---|---|---|
| size | vint | Number of bytes from the type field to the end of the record. |
| type | vint | Meaning depends on the block type (6.1, 7). |
| data | | Depends on the type; may be empty. |

Rules (DECISION where the technical note is silent):

- A record needs a size of at least 1 and must end inside the extra area.
  At the first record that breaks this, or whose size or type vint is
  invalid, **the rest of the extra area is ignored** with a diagnostic;
  the block itself stays valid (CONFIRMED (crafted): all three black-box
  readers keep the file; vector E18).
- Records of unknown type are skipped (TECHNOTE; CONFIRMED (crafted)).
- When a record type occurs more than once, **the first occurrence is
  the only one considered**, even if it is then ignored as too short;
  later records of that type are skipped. Two refinements: for the hash
  record (7.3) "type" means the pair of record type and hash type, so
  the first record with hash type 0 is used whatever precedes it; and
  every encryption record counts (7.2). (DECISION. Genuine archives
  never repeat a record. CONFIRMED (crafted) that readers differ: `7zz`
  shows the first of two time records, `lsar` and `bsdtar` the last;
  vector C13.)
- The fields of a record are parsed strictly inside the record. If a
  known record is too short for its fields, that record is ignored with
  a diagnostic, with the one exception of 7.2. Bytes left over after the
  fields this document defines are ignored.

## 6. Block types

### 6.1 Main archive header (type 1)

| Field | Size | Meaning |
|---|---|---|
| archive flags | vint | Table below. |
| volume number | vint | Present only if archive flag `0x0002` is set. |

| Archive flag | Meaning |
|---|---|
| `0x0001` | The archive is a volume of a multi-volume set. |
| `0x0002` | Volume number field present. Set in every volume except the first. |
| `0x0004` | Solid archive. |
| `0x0008` | A recovery record is present. |
| `0x0010` | Locked archive. |

- The volume number is 1 for the second volume, 2 for the third, and so
  on; the first volume has no field and counts as 0.
- The solid flag is informational: which members share state is decided
  per member (section 12). Flags `0x0008` and `0x0010` are ignored.
- Observed values: `0x00`, `0x04` (solid), `0x08`, `0x10`, `0x01`,
  `0x03`, `0x05`, `0x07`, `0x09`, `0x0b`.

Extra records of the main header (both optional, both ignored by a
sequential reader):

**Type 1, locator.**

| Field | Size | Meaning |
|---|---|---|
| flags | vint | `0x0001` quick open offset present; `0x0002` recovery record offset present. |
| quick open offset | vint | If flag `0x0001`. Distance from the first byte of the main archive header block (its CRC field) to the first byte of the quick open service block. 0 means "not known". |
| recovery record offset | vint | If flag `0x0002`. The same for the recovery record service block. 0 means "not known". |

DECISION: the locator is never used. Its offsets are untrusted and a
sequential walk finds every block anyway. `rar` writes the record with
padded offsets when quick open information is enabled, and omits it when
quick open is disabled.

**Type 2, metadata.**

| Field | Size | Meaning |
|---|---|---|
| flags | vint | `0x0001` name present; `0x0002` creation time present; `0x0004` time is Unix, otherwise Windows FILETIME; `0x0008` Unix time is in nanoseconds, otherwise seconds. |
| name length | vint | If flag `0x0001`. |
| name | bytes | If flag `0x0001`. Original archive name, UTF-8. It ends at the first zero byte if it has one; a first byte of zero means no name (TECHNOTE). |
| time | 4 or 8 bytes | If flag `0x0002`. FILETIME: uint64. Unix seconds: uint32. Unix nanoseconds since 1970: uint64. |

Only flag value `0x0f` was observed (name, then a uint64 of nanoseconds).
The record is informational; a reader may ignore it.

### 6.2 File header (type 2) and service header (type 3)

Both use the same layout.

| Field | Size | Meaning |
|---|---|---|
| file flags | vint | Table below. |
| unpacked size | vint | U. |
| attributes | vint | 6.4. In service headers the value is reserved or specific to the service (0 in every one observed); ignored. |
| mtime | uint32 | Present only if file flag `0x0002`. Unix time, seconds. |
| data CRC-32 | uint32 | Present only if file flag `0x0004`. Section 10. |
| compression information | vint | 6.3. |
| host OS | vint | 6.4. |
| name length | vint | Number of name bytes. |
| name | bytes | 6.5; for a service header, 8. |

The packed size of the piece is the block's data size S; a file header
without header flag `0x0002` has S = 0 and is valid (CONFIRMED
(crafted): `unar` and `7zz` read such an empty file, `bsdtar` refuses
it; vector C14). `rar` itself always sets the flag on file headers and
writes S as 0 where there are no data.

| File flag | Meaning |
|---|---|
| `0x0001` | Directory (file headers only). |
| `0x0002` | mtime field present. |
| `0x0004` | data CRC-32 field present. |
| `0x0008` | Unpacked size unknown: the U field is present but must be ignored (9.4). |
| others | undefined; ignored. |

Observed file flag values: `0x04` (the usual case: time in an extra
record, 7.4), `0x06` (time in the header; `rar` uses it when the time has
no sub-second part, and for data read from standard input), `0x00` and
`0x02` (no CRC-32: archives made with the BLAKE2 option, and service
headers), `0x01`, `0x05` (directories), `0x0e` (unknown size).

### 6.3 Compression information

A vint; its bits:

| Bits | Mask | Meaning |
|---|---|---|
| 0..5 | `0x00003f` | **version** of the compression algorithm. 0 and 1 exist. |
| 6 | `0x000040` | **solid bit**: the member continues with the decoder state left by the preceding members (section 12). Never set in service headers. |
| 7..9 | `0x000380` | **method**: 0 stored, 1..5 compressed (fastest to best; the decoder does not need the value). |
| 10..14 | `0x007c00` | **N**, the dictionary size exponent. |
| 15..19 | `0x0f8000` | **F**, the dictionary size fraction. Only when version is 1. |
| 20 | `0x100000` | Only when version is 1: the dictionary size is in version 1 form but the **data use the version 0 algorithm** (TECHNOTE; written when version 1 files are appended to a version 0 solid stream). |

Dictionary size D:

| Version | Valid N | D |
|---|---|---|
| 0 | 0..15 | 131,072 × 2^N (128 KiB .. 4 GiB). Bits 15 and above are not used. |
| 1 | 0..23 | 131,072 × 2^N + F × 4,096 × 2^N, that is, the power of two plus F thirty-seconds of it. |

Which algorithm decodes a compressed member:

| Version | Bit 20 | Algorithm |
|---|---|---|
| 0 | (ignored) | RAR 5.0 |
| 1 | clear | RAR 7.0 |
| 1 | set | RAR 5.0 |

What the two algorithms are is the subject of `rar5-compression.md`.

Rules:

- DECISION: a member is **unsupported** when its version is greater than
  1 (even if it is stored: later versions may redefine these fields, as
  version 1 did), when its method is 6 or 7, when version is 0 and N is
  greater than 15, or when version is 1 and N is greater than 23
  (CONFIRMED (crafted) for version 2 and for method 6, each alone in an
  archive: `unar` and `7zz` fail the member and continue; vector E16
  combines the cases). An implementation may also
  declare unsupported any D above the largest dictionary it is prepared
  to handle.
- For stored members, directories and links, N, F and bit 20 mean
  nothing; `rar` writes them as 0 (one real archive written by WinRAR
  has N = 2 on its stored members).
- D is a claim. It must never by itself cause an allocation (section
  17).
- `rar` 7.21 writes version 0 unless the dictionary is larger than
  4 GiB; one version 1 archive was produced (value `0x2bc81`: version 1,
  method 1, N = 15, F = 5, D = 4,966,055,936) (CONFIRMED (real data)).
  Bit 20 was not seen in a genuine archive.
- A **stored** member of version 1 is read like any other stored member,
  with or without bit 20 (CONFIRMED (crafted), `unar` and `7zz`; vector
  C14).
- Observed version 0 values: `0x0000`, `0x0040`, `0x0180`, `0x0800`,
  `0x0980`, `0x0d80`, `0x0dc0`, `0x0e80`, `0x0ec0`, `0x2180`.

### 6.4 Host OS and attributes

| Host OS | Meaning | Attributes |
|---|---|---|
| 0 | Windows | Windows file attribute bits. Observed: `0x20` (archive) on files, `0x10` (directory) on directories. |
| 1 | Unix | The Unix mode: file type and permission bits. Observed: octal 100644 files, 40755 directories, 120755 symbolic links. |
| other | undefined | DECISION: keep the member, treat the attributes as unknown. |

The attribute bits do not decide what a member is; file flag `0x0001`
and the redirection record do (9.1).

### 6.5 Names

- The name is the stored bytes, in UTF-8, without a terminating zero.
  `/` separates path components for both host systems. Names are
  relative; directories have no trailing separator.
- For Unix names a backslash is an ordinary character; for Windows names
  it is not valid in a name (TECHNOTE).
- TECHNOTE: when a Unix name held bytes that are not valid text, RAR
  stores each such byte as a code point in U+E080..U+E0FF and adds the
  non-character U+FFFE somewhere in the name as a marker. DECISION: no
  reverse mapping is required; the name is delivered as stored.
- DECISION: bytes that are not valid UTF-8 do not make the header
  malformed; the name is delivered with each invalid sequence replaced
  by U+FFFD (how many replacement characters a run of invalid bytes
  yields is not prescribed), or as raw bytes if the caller's interface
  allows (vector C14; `7zz` drops the invalid byte, `lsar` escapes it).
- DECISION: if the name contains a zero byte it ends before the first
  one (`7zz` and `bsdtar` do this; the reference implementation
  substitutes another character; CONFIRMED (crafted)). A name that is
  empty, as stored or after this cut, makes the header **malformed**
  (stop) (vector E20).
- The name length is bounded only by its region of the header, so by
  2 MiB.
- Making a name safe to extract (absolute paths, `..` components, drive
  letters, backslashes in Windows names, reserved names) is the job of
  the caller's common path rules and is not specified here; section 17
  lists what must be guaranteed.

### 6.6 Directories

A file header with file flag `0x0001` is a directory. Real ones have
U = 0, S = 0, method 0 and no checksum of any meaning (the CRC-32 field
may be present and 0). Rules: a directory never has contents; a data
area, if present, is skipped; its U, checksums and compression
information, solid bit included (12.2), are ignored. In solid archives
`rar` writes directories with compression information `0x40`, elsewhere
`0x00`. `rar` writes directory headers after the files they contain;
nothing depends on the order.

### 6.7 End of archive header (type 5)

| Field | Size | Meaning |
|---|---|---|
| end of archive flags | vint | `0x0001`: the archive is a volume and it is not the last one. |

Reading of a volume ends at this block. **Nothing after it is read**:
the technical note allows other tools to append data there, and `rar`
pads every volume but the last with zero bytes after it (8 to 80 seen)
(CONFIRMED (real data)). A volume that ends at a block boundary without
this block is accepted with a diagnostic (DECISION; `unar` and `bsdtar`
accept it silently, `7zz` reports "unexpected end of archive"; vector
E19); it counts as an end header with flags 0.

### 6.8 Archive encryption header (type 4)

Only valid as the first block of a volume.

| Field | Size | Meaning |
|---|---|---|
| encryption version | vint | 0 (AES-256). |
| encryption flags | vint | `0x0001` password check data present. |
| KDF count | byte | Binary logarithm of the key-derivation iteration count. |
| salt | 16 bytes | |
| check value | 12 bytes | Only if flag `0x0001`. |

With the flag set, H is 33 (CONFIRMED (real data)). Every block after
this one is preceded by a 16-byte initialisation vector and is enciphered,
padded to a multiple of 16 bytes (TECHNOTE). See 13.1 for what a reader
does.

## 7. Extra records of file and service headers

| Type | Record |
|---|---|
| 1 | file encryption |
| 2 | file hash |
| 3 | file time |
| 4 | file version |
| 5 | redirection |
| 6 | Unix owner |
| 7 | service data |

### 7.1 General

Section 5.5 applies. `rar` writes the records in ascending type order;
readers must not rely on it.

### 7.2 File encryption (type 1)

| Field | Size | Meaning |
|---|---|---|
| version | vint | 0 (AES-256). |
| flags | vint | `0x0001` password check data present; `0x0002` the stored checksums are transformed with a key ("tweaked") instead of plain. |
| KDF count | byte | |
| salt | 16 bytes | |
| IV | 16 bytes | |
| check value | 12 bytes | Only if flag `0x0001`. |

The presence of a record of type 1 marks the member **encrypted**, even
if the record is too short, has an unknown version, or is otherwise
unusable (DECISION: a damaged encryption record must not turn an
enciphered member into one that is decoded as plain data; vector E18).
Real records have flags `0x03` and a record size of 48. See 13.2.

### 7.3 File hash (type 2)

| Field | Size | Meaning |
|---|---|---|
| hash type | vint | 0: BLAKE2sp. |
| hash | 32 bytes for type 0 | |

Other hash types, and a type 0 record with fewer than 32 hash bytes, are
ignored (the second with a diagnostic). A record of another hash type
does not prevent a later type 0 record from being used; of several type
0 records only the first counts (5.5; vector C13). What the hash covers:
section 10.
Archives made with `rar`'s BLAKE2 option carry this record and **no
CRC-32 field** (file flags `0x00` or `0x02`).

### 7.4 File time (type 3)

| Field | Size | Meaning |
|---|---|---|
| flags | vint | `0x0001` times are Unix, otherwise Windows FILETIME; `0x0002` mtime present; `0x0004` creation time present; `0x0008` last access time present; `0x0010` Unix times have nanosecond fields. |
| mtime | uint32 or uint64 | If `0x0002`. |
| ctime | uint32 or uint64 | If `0x0004`. |
| atime | uint32 or uint64 | If `0x0008`. |
| mtime nanoseconds | uint32 | If `0x0001`, `0x0002` and `0x0010` are all set. |
| ctime nanoseconds | uint32 | If `0x0001`, `0x0004` and `0x0010` are all set. |
| atime nanoseconds | uint32 | If `0x0001`, `0x0008` and `0x0010` are all set. |

- With flag `0x0001` each time is a uint32 of seconds since 1970-01-01
  00:00:00 UTC (unsigned), and the nanosecond fields, which follow **all**
  the seconds fields, are added to them.
- Without it each time is a uint64 FILETIME: units of 100 ns since
  1601-01-01 00:00:00 UTC. Unix seconds = FILETIME ÷ 10,000,000 (integer
  part) − 11,644,473,600; nanoseconds = (FILETIME mod 10,000,000) × 100.
  The result can be negative; use signed 64-bit seconds. Flag `0x0010`
  has no effect.
- DECISION: a nanosecond field of 1,000,000,000 or more is ignored
  (treated as 0) (`7zz` does the same; CONFIRMED (crafted), vector C3).
- A time from this record replaces the header's mtime field (DECISION;
  `7zz` agrees; vector C3). Creation and access time exist only here.
- Observed: `0x13` (macOS: mtime with nanoseconds), `0x1f` (all three
  with nanoseconds), `0x02` (Windows: mtime as FILETIME). FILETIME
  creation and access times were checked with crafted archives only.

### 7.5 File version (type 4)

| Field | Size | Meaning |
|---|---|---|
| flags | vint | 0. |
| version number | vint | |

Written when old versions of a file are kept in the archive: the old
version carries the record, the current one does not, and both have the
same name. DECISION: a member with this record is delivered under its
name followed by `;` and the decimal version number (`hello.txt;1`), as
`bsdtar` shows it, so that names stay distinct.

### 7.6 Redirection (type 5)

| Field | Size | Meaning |
|---|---|---|
| redirection type | vint | 1 Unix symbolic link; 2 Windows symbolic link; 3 Windows junction; 4 hard link; 5 file copy. |
| flags | vint | `0x0001`: the target is a directory. |
| name length | vint | |
| name | bytes | The target, UTF-8, no terminating zero. |

Observed (types 1, 4 and 5, written by `rar` on macOS):

| Type | U | S | CRC-32 field | Attributes | Target |
|---|---|---|---|---|---|
| 1 | 0 | 0 | 0 | octal 120755 | the link text as the file system holds it |
| 4 | size of the target file | 0 | 0 | those of the file | name of an earlier member of the archive |
| 5 | size of the target file | 0 | 0 | those of the file | name of an earlier member of the archive |

A **file copy** is how RAR stores a file whose contents are identical to
an earlier file when asked to save identical files as references; a
**hard link** is one more name of an earlier file. Types 2 and 3 are
TECHNOTE; their target syntax was not observed. Member treatment: 9.1.

The record is valid when its three vints and the target bytes fit
inside it; then it decides the member's kind even if the target is
empty. DECISION: the target is delivered as the stored bytes with the
same treatment as a name (6.5: cut before the first zero byte, invalid
UTF-8 tolerated), but an empty target does not make anything malformed:
a symbolic link or junction with an empty target is delivered as it is,
and a hard link or file copy with an empty target is failed, corrupt
(9.1). A record whose target length exceeds the record is ignored like
any other record that is too short (5.5), so the member is then an
ordinary file or directory (vector E18).

### 7.7 Unix owner (type 6)

| Field | Size | Meaning |
|---|---|---|
| flags | vint | `0x0001` user name present; `0x0002` group name present; `0x0004` numeric user id present; `0x0008` numeric group id present. |
| user name length, user name | vint, bytes | If `0x0001`. Not zero terminated. |
| group name length, group name | vint, bytes | If `0x0002`. |
| user id | vint | If `0x0004`. |
| group id | vint | If `0x0008`. |

Observed with flags `0x03` (names only). Informational; a reader may
ignore the record. It must never be used to change the ownership of
extracted files without the caller asking for it.

### 7.8 Service data (type 7)

Opaque bytes whose meaning depends on the service header. Observed in
recovery record headers: one byte equal to the recovery percentage that
was requested (INFERRED). Ignored.

## 8. Service headers

A service header is laid out exactly like a file header (6.2) and its
name field says what it is:

| Name | Contents of the data area | Position |
|---|---|---|
| `CMT` | Archive comment: UTF-8 text, stored (method 0, S = U), CRC-32 present. | After the main header, before the first file header; first volume only. |
| `QO` | Quick open data (8.1), stored, no CRC-32. | After the last file header of a volume (most volumes of a set have one). |
| `RR` | Recovery record. | After `QO`, before the end of archive header. |
| `ACL` | NTFS permissions of the preceding file (TECHNOTE). | After the file header it belongs to. |
| `STM` | An NTFS alternate data stream of the preceding file (TECHNOTE). | After the file header it belongs to. |

**A reader skips every service header together with its data area**,
whatever its name, flags or compression information, and whether or not
it is split across volumes (TECHNOTE: using quick open data is
optional). Service headers never take part in solid groups, never join
across volumes into members, and never change which file header is "the
first file header of a volume" (11.2).

DECISION: a reader may expose the archive comment. If it does, it takes
it from the first `CMT` header of the first volume, only when that
header has method 0, no encryption record, S equal to U, S at most
262,144, and a CRC-32 field that matches the data; the text ends before
the first zero byte, if any. Otherwise there is no comment.

### 8.1 Quick open data

For information; DECISION: **never used**. It is a sequence of
structures:

| Field | Size | Meaning |
|---|---|---|
| structure CRC | uint32 | CRC-32 from the size field to the end of the structure. |
| structure size | vint, at most 3 bytes | Bytes from the flags field to the end. |
| flags | vint | 0. |
| offset | vint | Distance from the first byte of the quick open service block back to the first byte of the block this structure caches. |
| data size | vint | |
| data | bytes | A copy of that block's header (CRC field, size field and header), without its data area. |

RAR uses it to list an archive without seeking through it. The technical
note warns that a reader must use the same source for listing and for
extraction, because a hostile archive can make the copies differ from the
real headers. Reading only the real headers avoids the problem entirely.

## 9. Member kinds and their contents

### 9.1 Kind

Decided in this order:

1. A valid redirection record (7.6) makes the member a link of the
   record's type. Types other than 1..5 give the kind "unknown
   redirection". This applies whether or not the directory flag is set
   (a junction, or a link to a directory, has both).
2. Otherwise, file flag `0x0001` makes it a directory.
3. Otherwise it is a regular file.

Links and directories have no contents of their own: their data areas
(none in genuine archives) are skipped, and their U and checksum fields
are not checked. DECISION:

- Symbolic links and junctions are delivered as links with their target
  text. Following or creating them is the caller's decision.
- Hard links and file copies are delivered as links whose target is a
  member name. Their contents, if the caller wants them, are those of
  the **nearest earlier regular-file member whose name equals the target
  byte for byte**; if there is none, or its U differs from the link's U,
  the link is reported as failed, corrupt. The reader does not invent
  contents. Details: the comparison uses names as stored (after the
  zero-byte cut of 6.5, before any version suffix of 7.5); a member that
  carries a version record is never a target; only regular files are
  targets, never other links or directories; if the target member is
  itself failed, the link is failed with the same reason. The contents
  count against the caller's output limits once per link (section 17).
- An unknown redirection is delivered as failed, unsupported.

### 9.2 Stored files (method 0)

The contents are the packed bytes: the pieces concatenated in volume
order. DECISION: U must equal P (unless U is unknown, 9.4); otherwise
the member is failed, **corrupt** (`7zz` reports an error when U is
larger; both `unar` and `7zz` silently deliver a prefix when U is
smaller; vector E10). The checksums of section 10 are verified.

### 9.3 Compressed files (methods 1..5)

Section 12 decides how the decoder is used. Two cases never reach it:

- P = 0 and U = 0 (and U known): an empty file. Delivered empty; its
  checksums are verified against the empty string.
- P = 0 and U greater than 0, or U unknown: failed, **corrupt**.

### 9.4 Unknown unpacked size

File flag `0x0008` says the U field is not to be trusted. RAR sets it
while the size is not yet known: when data come from standard input into
a multi-volume set, every piece but the last has the flag and
U = `0x7FFFFFFF7FFFFFFF`, and the last piece has the true size and no
flag (CONFIRMED (real data)). A single-volume archive made from standard
input has the true size and no flag.

Rules: for a joined member, U and the flag are those of the **last
piece** (11.3). If the flag is set there: a stored member has U = P
(DECISION; the black-box readers disagree with each other on this case
and no genuine example exists; vector C5); a compressed member is decoded
until the decoder reports the end of its stream (TECHNOTE), under the
caller's output limit.

## 10. Checksums

### 10.1 What is checked

For a regular file that is not encrypted:

- If its (last) header has file flag `0x0004`, the CRC-32 of the
  **unpacked contents** must equal the data CRC-32 field.
- If its (last) header has a BLAKE2sp hash record, the BLAKE2sp hash of
  the unpacked contents must equal the record.
- Both are checked when both are present (not seen in genuine archives;
  CONFIRMED (crafted), `7zz`). A member with neither is delivered
  unchecked (CONFIRMED (crafted)).
- A mismatch makes the member failed, **corrupt**; nothing of it is
  delivered as valid.
- For a file split across volumes, the header of **every piece except
  the last** carries, in the same fields, the CRC-32 or BLAKE2sp hash of
  **that piece's packed bytes** (its data area), and the header of the
  last piece carries the checksum of the whole unpacked file
  (CONFIRMED (real data) for both checksum kinds). DECISION: the piece
  checksums are verified too, and a mismatch fails the member (`unar`
  and `7zz` do the same; vector E17a).
- For encrypted members nothing can be checked (13.2).
- The CRC-32 field of a directory or link is ignored. An empty file has
  CRC-32 0.

DECISION: BLAKE2sp is implemented and verified, not skipped, because
archives made with the BLAKE2 option have no CRC-32 at all; without it
their contents would be delivered unverified (as the reference
implementation does, section 18).

### 10.2 BLAKE2sp

BLAKE2sp is a fixed tree of nine BLAKE2s computations: eight **leaves**
that share the input between them and one **root** that hashes the eight
leaf results. The output is 32 bytes. No key is used.

**Words.** All arithmetic is on 32-bit words, additions wrap modulo 2^32,
"rotate right by r" moves each bit r places towards the least
significant end with wrap-around. Bytes are grouped into words least
significant byte first.

**Constants.** Eight words:

| i | C[i] |
|--:|---|
| 0 | `0x6A09E667` |
| 1 | `0xBB67AE85` |
| 2 | `0x3C6EF372` |
| 3 | `0xA54FF53A` |
| 4 | `0x510E527F` |
| 5 | `0x9B05688C` |
| 6 | `0x1F83D9AB` |
| 7 | `0x5BE0CD19` |

Ten message schedules, each a permutation of 0..15:

| Round | Schedule |
|--:|---|
| 0 | 0 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 |
| 1 | 14 10 4 8 9 15 13 6 1 12 0 2 11 7 5 3 |
| 2 | 11 8 12 0 5 2 15 13 10 14 3 6 7 1 9 4 |
| 3 | 7 9 3 1 13 12 11 14 2 6 5 10 4 0 15 8 |
| 4 | 9 0 5 7 2 4 10 15 14 1 11 12 6 8 3 13 |
| 5 | 2 12 6 10 0 11 8 3 4 13 7 5 15 14 1 9 |
| 6 | 12 5 1 15 14 13 4 10 0 7 6 3 9 2 8 11 |
| 7 | 13 11 7 14 12 1 3 9 5 0 15 4 8 6 2 10 |
| 8 | 6 15 14 9 11 3 0 8 12 2 13 7 1 4 10 5 |
| 9 | 10 2 8 4 7 6 1 5 15 11 9 14 3 12 13 0 |

**One node.** A node has a chaining value of eight words h[0..7], a
64-bit byte counter t starting at 0, a message, and two properties: its
*offset* and *depth* (below) and whether it is a *last node*.

Initial chaining value: h[i] = C[i] for every i, then

- h[0] is XORed with `0x02080020` (output length 32, no key, fan-out 8,
  tree depth 2);
- h[2] is XORed with the node's offset;
- h[3] is XORed with `0x20000000` for a leaf and with `0x20010000` for
  the root (node depth 0 or 1 in bits 16..23, inner hash length 32 in
  bits 24..31).

So a leaf with offset 0 starts from `6801e647 bb67ae85 3c6ef372
854ff53a 510e527f 9b05688c 1f83d9ab 5be0cd19`, the leaf with offset 7
has `3c6ef375` as its third word, and the root has `854ef53a` as its
fourth.

The message is cut into blocks of 64 bytes. The **final block** is the
last one, which holds 1 to 64 bytes and is filled up with zero bytes; an
empty message has one final block of 64 zero bytes. (A message whose
length is a positive multiple of 64 has no extra empty block: its last
full block is the final block. A streaming implementation therefore
cannot compress a full block until it knows whether more input
follows.) For each block in order: add the number of message bytes in
the block (64, or fewer in the final block, 0 for the empty message) to
t, then apply the compression step.

**Compression step**, for a block seen as sixteen words m[0..15]:

1. Make sixteen working words: v[0..7] = h[0..7] and v[8..15] = C[0..7].
2. XOR v[12] with the low 32 bits of t and v[13] with the high 32 bits.
3. If this is the final block, complement v[14] (XOR with
   `0xFFFFFFFF`). If this is the final block of a last node, also
   complement v[15].
4. Do ten rounds, numbered 0..9. Round r applies the mixing step to
   eight quadruples of working words, in this order, the k-th quadruple
   (k = 0..7) using the message words x = m[s[2k]] and y = m[s[2k+1]],
   where s is the schedule of round r:

   | k | a, b, c, d |
   |--:|---|
   | 0 | v[0], v[4], v[8], v[12] |
   | 1 | v[1], v[5], v[9], v[13] |
   | 2 | v[2], v[6], v[10], v[14] |
   | 3 | v[3], v[7], v[11], v[15] |
   | 4 | v[0], v[5], v[10], v[15] |
   | 5 | v[1], v[6], v[11], v[12] |
   | 6 | v[2], v[7], v[8], v[13] |
   | 7 | v[3], v[4], v[9], v[14] |

   The mixing step changes a, b, c and d in eight moves:

   | Move | Effect |
   |--:|---|
   | 1 | a becomes a + b + x |
   | 2 | d becomes (d XOR a) rotated right by 16 |
   | 3 | c becomes c + d |
   | 4 | b becomes (b XOR c) rotated right by 12 |
   | 5 | a becomes a + b + y |
   | 6 | d becomes (d XOR a) rotated right by 8 |
   | 7 | c becomes c + d |
   | 8 | b becomes (b XOR c) rotated right by 7 |

5. For i = 0..7, h[i] becomes h[i] XOR v[i] XOR v[i+8].

The node's result is h[0..7] after the final block, written as 32 bytes,
each word least significant byte first.

**The tree.**

- Number the input's 64-byte chunks 0, 1, 2, ... (the last may be
  short). Chunk j belongs to leaf j mod 8; a leaf's message is its
  chunks in order. With less than 449 bytes of input some leaves have an
  empty message.
- Leaf i (0..7) has offset i and depth 0. Only **leaf 7** is a last
  node.
- The root has offset 0 and depth 1, is a last node, and its message is
  the 256 bytes made of the results of leaves 0, 1, ..., 7 in that
  order.
- The BLAKE2sp hash is the root's result.

Memory is constant: nine chaining values and one pending block per leaf.

Check values are in sections 15.7 and 16.4.

## 11. Multi-volume sets

### 11.1 Structure

Each volume is a complete block sequence of its own: signature, main
header (with archive flag `0x0001`, and with the volume number in every
volume but the first), blocks, end of archive header. A file that does
not fit continues in the next volume under a new file header with the
same name. Header flags mark the split: `0x0010` on a file header means
its data continue in the next volume; `0x0008` means they continue from
the previous one. A middle piece has both.

In real sets the piece that continues into the next volume is the last
file header of its volume and the piece that continues from the previous
one is the first file header of its volume; most volumes carry their
own quick open header, and only the first has the comment.

### 11.2 Joining pieces

The reader keeps at most one **open member**: a file header seen with
flag `0x0010` whose continuation has not been seen yet.

For each file header H, in reading order:

- If there is an open member, and H is the first file header of its
  volume, and H has flag `0x0008`, and H's name is byte-for-byte equal
  to the open member's: H's data area is appended as the next piece.
  If the open member is already failed (it began with flag `0x0008`
  itself), it stays failed. (CONFIRMED (crafted): with a different name
  the reference implementation and `7zz` both treat the pieces as
  separate, damaged files; vector E17b. The condition "first file
  header of its volume" is a DECISION: the reference implementation
  compares only the flag and the name, and `7zz` joins a continuation
  even when another file header precedes it in the volume; genuine sets
  never have one there; vector E17g.)
- Otherwise: an open member, if any, is closed as failed,
  **incomplete**; H starts a new member; and if H has flag `0x0008` the
  new member is itself failed, **incomplete** (its beginning is in a
  volume that was not read, or the set is damaged).
- Then, if H has flag `0x0010`, the member stays open; otherwise it is
  complete and is added to the list.

At the end of the last volume read, an open member is closed as failed,
**incomplete** (the reference implementation drops it silently, section
18).

### 11.3 Which header describes a joined member

- **U, the unknown-size flag, the CRC-32 field, the hash record, times,
  attributes, owner and every other extra record (version, redirection)
  are taken from the last piece's header.** Earlier pieces may carry
  placeholder sizes (9.4) and carry piece checksums instead of file
  checksums (10.1). (CONFIRMED (real data). In genuine sets every piece
  repeats the same time record; sets written from standard input have
  the header mtime field in every piece.)
- Name, directory flag and compression information are the same in every
  piece of genuine sets. DECISION: if a later piece's compression
  information or directory flag differs from the first piece's, the
  member is failed, corrupt (vector E17f; `unar` and `7zz` do not
  compare them).
- The encryption record is per piece; a member is encrypted if any piece
  has one.

### 11.4 Moving to the next volume

After the end of archive header of a volume, the next volume is read if
and only if the volume's main header has archive flag `0x0001` **and**
the end header has flag `0x0001` (CONFIRMED (crafted): the reference
implementation and `7zz` stop after one volume when either is missing).
If the next volume cannot be obtained, the stop reason is **missing
volume**; members already complete are kept.

DECISION: the volume number of each volume is compared with the number
expected (the first volume read has number n, absent meaning 0; the
k-th volume after it must have n + k). A mismatch is a diagnostic only:
no black-box reader checks the field, and the name test of 11.2 and the
piece checksums already protect the contents (vector E17e).

Opening a set at a volume other than the first is allowed; members that
began earlier come out incomplete (vector E17d).

Solid groups run across volume boundaries (12); the decoder state is
kept while the next volume is opened.

### 11.5 Volume file names

Not part of the format; needed only to find the next volume when the
caller does not supply the list. `rar` names RAR 5 volumes
`name.partN.rar` with N starting at 1 and padded with zeros to a fixed
width (`.part1.rar` .. `.part5.rar`, `.part01.rar` .. `.part34.rar`)
(CONFIRMED (real data)). The older scheme `name.rar`, `name.r00`,
`name.r01` is not used for this format: `rar` 7.21 ignores its
old-naming option and writes `.partN.rar` (CONFIRMED (real data)).

In a **self-extracting set** only the first volume carries the module
and a different extension: `rar` on macOS writes `name.part1.sfx`,
`name.part2.rar`, `name.part3.rar` (CONFIRMED (real data); on Windows
the first is expected to be `.exe`: INFERRED, not observed).
The later volumes have their signature at offset 0.

REFERENCE-ONLY rule: take the last run of decimal digits in the file
name before the extension (it must be preceded by at least one
character that is not a digit); the volumes of the set are the files in
the same directory whose names differ from it only in that run, with the
same number of digits there, and in the extension, which for each file
independently may be `rar`, `exe` or `sfx`, compared without regard to
case; they are taken in ascending order of the number. (CONFIRMED (real
data) for the three-volume self-extracting set above, which `lsar`
opens as three volumes.)

**Recovery volumes** are not archive volumes. `rar` names them
`name.partN.rev`; they begin with the 8 bytes `52 61 72 21 1a 52 65 76`
(`Rar!`, 1A, `Rev`), which is not the signature of section 4, and their
contents are not described here (CONFIRMED (real data) for the name and
the first bytes). A reader never treats one as a volume; the name rule
above excludes them by extension, and section 4 rejects them by
signature.

## 12. Solid archives

### 12.1 Principle

In a solid archive the decoder is not reset between files: a file may
refer to data of the files before it. The solid bit of a member's
compression information (6.3) says that the member continues from the
state the preceding files left. A member whose solid bit is clear starts
afresh. The archive flag (6.1) is not needed to decide anything.

### 12.2 The rule

The reader carries one **group state**, which is one of: *none*, a
*live* decoder state, or *poisoned*. It starts as *none* and is carried
across volumes. Members are taken in archive order (a split file counts
once, at the position of its last piece, with the solid bit of its first
piece):

1. A **directory** (file flag `0x0001`) is passed over. It changes
   nothing, whatever its solid bit.
2. For any other member, **if its solid bit is clear the group state
   becomes *none***. This happens whatever the member is (compressed,
   stored, empty, link) and before anything else is done with it.
3. A **link** (9.1) is then passed over.
4. The member is failed without looking at its data if it is
   unsupported (6.3), encrypted (13.2), incomplete (11.2) or truncated
   (5.4), tested in that order.
5. A **stored** member (method 0) is read as in 9.2 unless rule 4 failed
   it. Either way it does not use and does not change the group state.
6. An **empty compressed** member (P = 0, U = 0 and known, 9.3) that
   rule 4 did not fail is delivered empty and does not use or change
   the group state.
7. Every other member is a **decoder member**:
   - If rule 4 failed it, or it is failed as corrupt by 9.3 (P = 0 with
     U not 0 or unknown): the group state becomes *poisoned*.
   - Otherwise, if its solid bit is clear: it is decoded with a fresh
     decoder state, which becomes the *live* group state.
   - Otherwise (solid bit set): if the group state is *live*, it is
     decoded continuing from that state. If the state is *none*, the
     member is failed, **no solid state**; if it is *poisoned*, the
     member is failed, **poisoned group**. The state becomes (or stays)
     *poisoned* in both failure cases.
   - If decoding fails, the member is failed, corrupt, and the group
     state becomes *poisoned*. A checksum mismatch on correctly decoded
     data does not poison the group.

Consequences:

- To deliver one member of a group, every earlier decoder member of the
  group must be decoded first, in order, even if its output is thrown
  away.
- The decoder is handed D, the algorithm and U of each member
  separately; they may differ between members of a group (bit 20 of the
  compression information exists for that, 6.3).

### 12.3 Evidence and the cases that matter

All of the following are CONFIRMED (real data) with archives written by
`rar` 7.21 and extracted correctly by `bsdtar` (compared byte for byte
with the original files), unless marked.

- In an archive that is not solid every file has the solid bit clear.
  In a solid archive the first file of each group has it clear and the
  others have it set; with RAR's options that limit groups (by count or
  by extension) one archive holds several groups.
- **Directories** in solid archives carry the solid bit (and method 0);
  they sit between or after the files without effect. A directory with
  the solid bit clear inserted into the middle of a group does not
  disturb it (CONFIRMED (crafted), `unar` and `bsdtar`).
- **A stored file inside a group** is written with compression
  information `0x40`: method 0 with the solid bit set. It is not part of
  the dictionary: the compressed member after it continues from the
  compressed member before it. (Removing the stored member's block from
  such an archive leaves the others decodable; and a compressed file
  that follows a stored file with identical contents is not smaller for
  it.) Vector G3.
- **An empty file in the middle of a group** has the group's method, the
  solid bit, and P = 0. Vector G1.
- **An empty file that starts a group** has the solid bit clear, U = 0
  and **P greater than 0**: its packed bytes are a complete compressed
  stream that produces nothing but transmits the tables. The next member
  has the solid bit set and cannot be decoded unless that stream was
  decoded first: decoding it afresh fails, and so does decoding it as a
  continuation of the previous group (CONFIRMED (crafted), both with
  `unar` and `bsdtar`). Vector G2. This is why rule 7 does not exempt
  members with U = 0.
- **Links** inside a group (symbolic links, hard links and file copies)
  carry the solid bit and method 0 (`0x40`), and the compressed member
  after one continues from the compressed member before it. A symbolic
  link that is the **first** member of a solid archive has compression
  information `0x00`, and the first file after it starts the group with
  its solid bit clear. One genuine archive shows all of this at once:
  a symbolic link (`0x00`), an empty file (`0x0d80`, U = 0, P = 57), a
  file (`0x0dc0`), a file copy (`0x40`), a file (`0x0dc0`), a hard link
  (`0x40`), two more files (`0x0dc0`); `bsdtar` extracts every file
  identical to its original.
- When a group would start with a stored file, `rar` clears the solid
  bit of the stored file **and** of the compressed file after it: the
  writer, too, treats a stored file with a clear solid bit as leaving
  nothing to continue from. Rule 2 follows this (and the reference
  implementation's grouping). It is a DECISION for input `rar` does not
  write: a compressed member with the solid bit set directly after a
  stored, empty or link member with the solid bit clear is failed, no
  solid state. (`bsdtar` instead lets such a member continue from the
  group before; `unar` fails it. CONFIRMED (crafted).)
- A group that spans volumes, with a split member in the middle, decodes
  across the volume boundary.

## 13. Encryption: recognising and skipping

Deciphering is out of scope. A reader must recognise both forms so that
it neither misreads ciphertext as structure nor decodes it as data.

### 13.1 Encrypted headers

If the first block after the signature has type 4 (6.8), all later
blocks are enciphered and cannot be walked. The reader stops with the
stop reason **encrypted headers** and an empty member list. It does not
look at the bytes after that block. (CONFIRMED (real data): `unar` asks
for a password, `bsdtar` reports that encryption is not supported.)

### 13.2 Encrypted members

A file header with an encryption record (7.2) describes a member whose
data area is ciphertext (AES-256 in CBC mode, so S is a multiple of 16;
TECHNOTE and CONFIRMED (real data) for the sizes). The header itself is
readable: name, sizes, times and S are in the clear, so the block is
skipped like any other.

- The member is reported with status failed, **encrypted**. Its packed
  bytes are never given to the decoder and never delivered as stored
  contents.
- Its checksums are not verified: with encryption flag `0x0002` (set in
  every real record) the stored CRC-32 and hash are transformed with a
  key.
- In a solid group an encrypted decoder member poisons the group (12.2).
- Service headers can be encrypted too; they are skipped anyway.

## 14. Reading order, summary

1. Find the archive in the first volume (4).
2. Read the first block. Type 4: stop, encrypted headers. Type 1: note
   the archive flags and volume number.
3. Read blocks in order (5.2). Type 2: parse (6.2, 7), then join (11.2).
   Type 3 and unknown types: skip. Type 1 or 4: stop, malformed header.
   Type 5: end of this volume.
4. At the end of a volume decide whether another follows (11.4); if so
   find the archive in it (4), check its first block as in step 2, and
   continue with step 3. The open member and the solid group state
   survive.
5. Close an open member as incomplete (11.2).
6. Classify the members (9.1) and walk them once in order to assign
   their handling (12.2). Extraction then follows that assignment and
   verifies the checksums (10).

Steps 1 to 5 need no decompression and deliver the complete listing.

## 15. Worked examples

### 15.1 A minimal archive, byte by byte (vector C1, 63 bytes)

```text
52 61 72 21 1a 07 01 00 c5 1a 33 32 03 01 00 00 15 0a 5e 31 14 02 02 0e
04 0e a4 83 02 18 a7 55 7b 00 01 05 61 2e 74 78 74 48 65 6c 6c 6f 2c 20
77 6f 72 6c 64 21 0a 19 b2 3a 35 03 05 00 00
```

| Offset | Bytes | Meaning |
|--:|---|---|
| 0 | `52 61 72 21 1a 07 01 00` | signature |
| 8 | `c5 1a 33 32` | header CRC `0x32331AC5` = CRC-32 of the 4 bytes `03 01 00 00` |
| 12 | `03` | H = 3 |
| 13 | `01` | type 1: main archive header |
| 14 | `00` | header flags 0: no extra area, no data area |
| 15 | `00` | archive flags 0. Header ends at 16; S = 0; next block at 16 |
| 16 | `15 0a 5e 31` | header CRC `0x315E0A15` = CRC-32 of the 21 bytes at 20..40 |
| 20 | `14` | H = 20: header is bytes 21..40 |
| 21 | `02` | type 2: file header |
| 22 | `02` | header flags: data area present |
| 23 | `0e` | S = 14 |
| 24 | `04` | file flags: CRC-32 present |
| 25 | `0e` | U = 14 |
| 26 | `a4 83 02` | attributes 33188 = octal 100644 |
| 29 | `18 a7 55 7b` | data CRC-32 `0x7B55A718` |
| 33 | `00` | compression information 0: version 0, not solid, stored, N = 0 |
| 34 | `01` | host OS 1: Unix |
| 35 | `05` | name length 5 |
| 36 | `61 2e 74 78 74` | name `a.txt`. Header ends at 41 |
| 41 | 14 bytes | data area: `Hello, world!` and a line feed. Next block at 55 |
| 55 | `19 b2 3a 35` | header CRC `0x353AB219` = CRC-32 of `03 05 00 00` |
| 59 | `03` | H = 3 |
| 60 | `05` | type 5: end of archive |
| 61 | `00` | header flags 0 |
| 62 | `00` | end of archive flags 0: last volume |

Result: one member, `a.txt`, regular file, stored, U = P = 14, one piece
(volume 0, offset 41, length 14), CRC-32 matches.

### 15.2 A file header written by `rar` (from vector G1)

The second block of G1, at offset 16:

```text
4e 0f dd 28 1b 02 02 ad 00 06 ae 00 a4 83 02 95 4f 0d 5e 9c 14 5e 06 80
1b 01 05 61 2e 74 78 74
```

| Bytes | Meaning |
|---|---|
| `4e 0f dd 28` | header CRC `0x28DD0F4E` |
| `1b` | H = 27 |
| `02` | file header |
| `02` | data area present |
| `ad 00` | S = 45, as a padded two-byte vint |
| `06` | file flags: mtime and CRC-32 present |
| `ae 00` | U = 46 |
| `a4 83 02` | attributes octal 100644 |
| `95 4f 0d 5e` | mtime 1577930645 (2020-01-02 02:04:05 UTC) |
| `9c 14 5e 06` | data CRC-32 `0x065E149C` |
| `80 1b` | compression information `0x0d80`: version 0, solid bit clear, method 3, N = 3 (D = 1,048,576) |
| `01` | Unix |
| `05`, `61 2e 74 78 74` | name `a.txt` |

### 15.3 Compression information values

| Value | Bytes | Version | Solid | Method | N | F | Bit 20 | Meaning |
|---|---|--:|--:|--:|--:|--:|--:|---|
| `0x0000` | `00` or `80 00` | 0 | 0 | 0 | 0 | | | stored |
| `0x0040` | `40` or `c0 00` | 0 | 1 | 0 | 0 | | | stored member or directory inside a solid archive |
| `0x0180` | `80 03` | 0 | 0 | 3 | 0 | | | RAR 5.0, D = 131,072 |
| `0x0d80` | `80 1b` | 0 | 0 | 3 | 3 | | | RAR 5.0, D = 1,048,576, starts a group |
| `0x0dc0` | `c0 1b` | 0 | 1 | 3 | 3 | | | RAR 5.0, D = 1,048,576, continues |
| `0x2180` | `80 43` | 0 | 0 | 3 | 8 | | | RAR 5.0, D = 33,554,432 |
| `0x3d80` | `80 7b` | 0 | 0 | 3 | 15 | | | RAR 5.0, D = 4,294,967,296 |
| `0x4180` | `80 83 01` | 0 | 0 | 3 | 16 | | | unsupported (N above 15 in version 0) |
| `0x0d81` | `81 1b` | 1 | 0 | 3 | 3 | 0 | 0 | RAR 7.0, D = 1,048,576 |
| `0x38d81` | `81 9b 0e` | 1 | 0 | 3 | 3 | 7 | 0 | RAR 7.0, D = 1,048,576 + 7 × 32,768 = 1,277,952 |
| `0x2bc81` | `81 f9 0a` | 1 | 0 | 1 | 15 | 5 | 0 | RAR 7.0, D = 4,294,967,296 + 5 × 134,217,728 = 4,966,055,936 (real archive) |
| `0x4d81` | `81 9b 01` | 1 | 0 | 3 | 19 | 0 | 0 | RAR 7.0, D = 68,719,476,736 |
| `0xfdd81` | `81 bb 3f` | 1 | 0 | 3 | 23 | 31 | 0 | RAR 7.0, D = 2,164,663,517,184 (the largest expressible) |
| `0x100d81` | `81 9b 40` | 1 | 0 | 3 | 3 | 0 | 1 | RAR 5.0 algorithm, D = 1,048,576 |
| `0x6181` | `81 c3 01` | 1 | 0 | 3 | 24 | 0 | 0 | unsupported (N above 23) |
| `0x0182` | `82 03` | 2 | 0 | 3 | 0 | | | unsupported (version) |
| `0x0300` | `80 06` | 0 | 0 | 6 | 0 | | | unsupported (method) |

The D column for version 1 agrees with what `7zz` displays for crafted
archives carrying these values (1248K, 4736M, 64G, 2016G).

### 15.4 File time records

Record data after the type byte:

| Bytes | Decoding |
|---|---|
| `13 16 b7 f6 6a ae eb 2b 32` | flags `0x13`: Unix, mtime, nanoseconds. mtime 1794553622 s, then 841739182 ns. (real, macOS) |
| `1f 95 4f 0d 5e 6e c0 f6 6a 95 4f 0d 5e 00 00 00 00 48 e8 f9 29 00 00 00 00` | flags `0x1f`. Seconds: mtime 1577930645, ctime 1794556014, atime 1577930645. Then nanoseconds in the same order: 0, 704243784, 0. (real, vector G5) |
| `02 00 a6 2d 14 11 79 c8 01` | flags `0x02`: FILETIME mtime 128485703640000000 → 1204096764 s, 0 ns (2008-02-27 07:19:24 UTC). (real, Windows) |
| `0e 87 56 12 45 d1 38 c1 01 00 00 c6 e9 4f c6 c4 01 00 80 8c 8e ce 53 c8 01` | flags `0x0e`: three FILETIMEs. mtime 126444736001234567 → 1000000000 s and 123456700 ns; ctime → 1100000000 s; atime → 1200000000 s. (crafted, vector C3) |
| `03 00 ca 9a 3b` | flags `0x03`: Unix mtime 1000000000, no nanoseconds. (crafted, vector E18) |

### 15.5 A quick open structure (real)

The first 53 bytes of the data area of a `QO` service block that starts
at offset 3197 of its archive:

```text
aa 63 e8 35 30 00 e5 18 2c 63 d3 cd 92 27 02 03 0b 97 00 04 97 00 a4 83
02 55 55 f1 2f 80 00 01 09 68 65 6c 6c 6f 2e 74 78 74 0a 03 13 16 b7 f6
6a ae eb 2b 32
```

CRC `0x35E863AA`; structure size `30` = 48; flags 0; offset `e5 18` =
3173, so the cached block starts at 3197 − 3173 = 24; data size `2c` =
44; then 44 bytes that are identical to the bytes at offset 24..67 of
the archive: the complete header of the file `hello.txt` (CRC, size
field `27`, and 39 header bytes including an 11-byte extra area with a
time record).

### 15.6 A three-volume set (vector C8)

| Volume | Blocks |
|---|---|
| 1 (100 bytes) | signature; main header, archive flags `0x01`; file header `f.txt`, header flags `0x13`, S = 10, U = 45, CRC-32 field `0xA3EC1434` = CRC-32 of the 10 data bytes `The quick `; end header, flags `0x01`; 5 zero bytes |
| 2 (106 bytes) | signature; main header, archive flags `0x03`, volume number 1; file header `f.txt`, header flags `0x1b`, S = 20, CRC-32 field `0xD90FC90C` = CRC-32 of the 20 data bytes; end header, flags `0x01` |
| 3 (138 bytes) | signature; main header, archive flags `0x03`, volume number 2; file header `f.txt`, header flags `0x0b`, S = 15, CRC-32 field `0xEB50CC6A` = CRC-32 of the whole 45-byte file; file header `e.txt`, S = U = 12; end header, flags `0x00` |

Each `f.txt` header also has a hash record: in volumes 1 and 2 the
BLAKE2sp hash of the piece, in volume 3 that of the whole file.

Result: `f.txt`, stored, three pieces (volume 0 offset 77 length 10;
volume 1 offset 78 length 20; volume 2 offset 78 length 15), contents
`The quick brown fox jumps over the lazy dog.` and a line feed; then
`e.txt`, contents `second file` and a line feed.

### 15.7 BLAKE2sp of `abc`

The input is one chunk of 3 bytes, so leaf 0 has the message `abc` and
leaves 1..7 have empty messages. Leaf results:

| Leaf | Result |
|--:|---|
| 0 | `13a6f89112b5e30be639225761cceeca57300521be8209bd0ab7927370da7402` |
| 1 | `8b9a894622cf1622301253db066f29a8ec56e8f8ca4d9de17e8bb855ead93877` |
| 2 | `854206d356d3d97eeb441432d904e14a72acc4eaac76fe7ad3f5bc73fc9281e5` |
| 3 | `079422064c11b0f97b9e4dc123717cef0eed7f0e33cf28e3e588db5c39f980d7` |
| 4 | `f18276fce4f07a35a139f756461c67b2988b3e256ad7ae1d2c076e776b6668da` |
| 5 | `aafc5963280a3679404981e611b5e4942e9e7cc74d76caa732b48e5adea86a42` |
| 6 | `1c5d4bd35dc827cb75823cf3fb912fea708973a36e5fb2cf43c21311c0aa26ff` |
| 7 | `ae22c7b25ded3555c8e3328fd20e6061325ff3d43f37cac6b8be509713f3a716` |

For leaf 0 the single block is `61 62 63` and 61 zero bytes, t = 3, and
v[14] is complemented. For leaves 1..6 the block is all zero, t = 0,
v[14] complemented. For leaf 7 v[15] is complemented as well. The root
compresses four blocks (t = 64, 128, 192, 256), complementing v[14] and
v[15] in the fourth. Root result, the BLAKE2sp hash:
`70f75b58f1fecab821db43c88ad84edde5a52600616cd22517b7bb14d440a7d5`.

### 15.8 Solid handling of three real archives

Vector G1 (`rar` solid, four files):

| Member | Compression information | U | P | Handling |
|---|---|--:|--:|---|
| `a.txt` | `0x0d80` | 46 | 45 | decode, fresh state |
| `b.txt` | `0x0dc0` | 52 | 6 | decode, continue |
| `e.txt` | `0x0dc0` | 0 | 0 | empty; state untouched |
| `c.txt` | `0x0dc0` | 30 | 6 | decode, continue from `b.txt` |

Vector G2 (the same files, groups limited to two files):

| Member | Compression information | U | P | Handling |
|---|---|--:|--:|---|
| `a.txt` | `0x0d80` | 46 | 44 | decode, fresh state |
| `b.txt` | `0x0dc0` | 52 | 6 | decode, continue |
| `e.txt` | `0x0d80` | 0 | 27 | **decode**, fresh state, zero bytes of output |
| `c.txt` | `0x0dc0` | 30 | 16 | decode, continue from `e.txt` |

Vector G3 (a stored file in the middle):

| Member | Compression information | U | P | Handling |
|---|---|--:|--:|---|
| `a.txt` | `0x0d80` | 46 | 37 | decode, fresh state |
| `st.bin` | `0x0040` | 17 | 17 | stored; state untouched |
| `b.txt` | `0x0dc0` | 52 | 18 | decode, continue from `a.txt` |

## 16. Test vectors

Vectors named G were written by `rar` 7.21 (macOS, command-line trial
version) from small files made for the purpose; vectors named C and E
were assembled by hand from the rules of this document. Every vector was
read by a throw-away reader written from this document, which gives
exactly the results stated, and was also given to `lsar` and `unar`
1.10.7, `7zz` 26.01 and `bsdtar` 3.5.3. "All agree" below means these
three tools list the same members and extract the same contents as
stated. Where a tool differs, the difference is noted; each is a fault
or limit of that tool (section 18) or a decision of this document
(section 19). The `7zz` build used cannot decompress RAR 5 data, so for
the G vectors with compressed members only `unar` and `bsdtar` extract.

In the expected results, "as in C1" means: one regular file `a.txt`,
stored, 14 bytes H14, CRC-32 verified. A hexadecimal block is the
complete file; line breaks are not part of it.

The file contents used:

| Name | Bytes | CRC-32 |
|---|---|---|
| H14 | `Hello, world!` + line feed (14 bytes) | `0x7B55A718` |
| `a.txt` (G vectors) | `alpha beta gamma delta alpha beta gamma delta` + line feed (46 bytes) | `0x065E149C` |
| `b.txt` (G vectors) | `gamma delta alpha beta gamma delta alpha beta gamma` + line feed (52 bytes) | `0x873ABB5A` |
| `c.txt` (G vectors) | `delta alpha beta gamma delta!` + line feed (30 bytes) | `0x4140B65A` |
| `e.txt` (G vectors) | empty | `0x00000000` |
| `st.bin` | `stored raw bytes` + line feed (17 bytes) | `0x79C16B3C` |
| `h.txt` | `Hello, RAR5!` + line feed (13 bytes) | `0x209B3B07` |

### 16.1 Archives written by `rar`

**G1** (209 bytes): solid archive, section 15.8. All members extract
correctly with `unar` and `bsdtar`. The main header has no extra area
(quick open disabled); times are in the header mtime field.

```text
52 61 72 21 1a 07 01 00 d8 1b 32 51 03 01 04 04 4e 0f dd 28 1b 02 02 ad
00 06 ae 00 a4 83 02 95 4f 0d 5e 9c 14 5e 06 80 1b 01 05 61 2e 74 78 74
c7 b7 2a 25 45 32 fa 33 bf f8 54 73 4f a3 05 a2 40 68 77 fb 16 e4 90 98
a7 1c 71 d9 67 af f6 46 5b 7a 29 75 e1 4c 97 7e 9c 2b 79 e0 b0 ed 7f 69
78 1b 02 02 86 00 06 b4 00 a4 83 02 95 4f 0d 5e 5a bb 3a 87 c0 1b 01 05
62 2e 74 78 74 47 1e 03 a1 bf b0 84 29 ee 24 1b 02 02 80 00 06 80 00 a4
83 02 95 4f 0d 5e 00 00 00 00 c0 1b 01 05 65 2e 74 78 74 61 5e 06 ad 1b
02 02 86 00 06 9e 00 a4 83 02 95 4f 0d 5e 5a b6 40 41 c0 1b 01 05 63 2e
74 78 74 40 19 03 9b d8 00 1d 77 56 51 03 05 04 00
```

Blocks at offsets 8 (main, archive flags `0x04`), 16, 93, 131, 163
(files), 201 (end). Pieces: `a.txt` offset 48 length 45; `b.txt` offset
125 length 6; `e.txt` length 0; `c.txt` offset 195 length 6.

**G2** (245 bytes): the empty file starts a group and has 27 packed
bytes. `bsdtar` extracts all four files correctly; `unar` fails `c.txt`
(section 18).

```text
52 61 72 21 1a 07 01 00 d8 1b 32 51 03 01 04 04 40 9f 56 8d 1b 02 02 ac
00 06 ae 00 a4 83 02 95 4f 0d 5e 9c 14 5e 06 80 1b 01 05 61 2e 74 78 74
c0 b3 29 25 45 23 fa 33 bd c2 9c d7 f0 89 25 0a 0d 0e ff 62 dc 88 38 a7
1c 71 d9 57 8f 65 c1 5f e0 f8 90 0f c1 9a 4f 51 69 f0 b1 00 ed 7f 69 78
1b 02 02 86 00 06 b4 00 a4 83 02 95 4f 0d 5e 5a bb 3a 87 c0 1b 01 05 62
2e 74 78 74 47 1e 03 d1 ef 62 09 28 8d bb 1b 02 02 9b 00 06 80 00 a4 83
02 95 4f 0d 5e 00 00 00 00 80 1b 01 05 65 2e 74 78 74 c7 85 18 33 04 24
fa 32 df d1 47 ad 38 81 03 00 90 c0 7f e0 62 41 47 65 5e 5a 9f a9 15 ca
9e 1b 02 02 90 00 06 9e 00 a4 83 02 95 4f 0d 5e 5a b6 40 41 c0 1b 01 05
63 2e 74 78 74 41 16 0d 45 8b 04 22 9c 11 ae a6 13 38 ee ff 80 1d 77 56
51 03 05 04 00
```

Pieces: `a.txt` offset 48 length 44; `b.txt` offset 124 length 6;
`e.txt` offset 162 length 27; `c.txt` offset 221 length 16.

**G3** (193 bytes): a stored member with compression information `0x40`
between two compressed ones. `bsdtar` extracts all three correctly;
`unar` fails `b.txt` (section 18).

```text
52 61 72 21 1a 07 01 00 d8 1b 32 51 03 01 04 04 f9 9c d6 04 1b 02 02 a5
00 06 ae 00 a4 83 02 95 4f 0d 5e 9c 14 5e 06 80 1b 01 05 61 2e 74 78 74
c2 ba 22 35 45 1f b3 3b d8 53 35 fa 10 a0 91 43 7f b0 c0 18 bf 4e 9e da
04 dc 03 46 c0 6e 2a b6 ab 4f 7f 8c 40 c4 8f 9a 63 1c 02 02 91 00 06 91
00 a4 83 02 95 4f 0d 5e 3c 6b c1 79 c0 00 01 06 73 74 2e 62 69 6e 73 74
6f 72 65 64 20 72 61 77 20 62 79 74 65 73 0a 78 12 c3 db 1b 02 02 92 00
06 b4 00 a4 83 02 95 4f 0d 5e 5a bb 3a 87 c0 1b 01 05 62 2e 74 78 74 c7
92 0f 32 3f d2 27 fb fd c7 a4 8c 18 95 18 97 07 da 1d 77 56 51 03 05 04
00
```

Pieces: `a.txt` offset 48 length 37; `st.bin` offset 118 length 17;
`b.txt` offset 167 length 18.

**G4** (134 bytes): BLAKE2 option, stored. One member `a.txt`, file
flags `0x02` (no CRC-32 field), hash record
`cb995f7bc2c4a1f91f57555e6c1aa0da3393926e6e61885dfdff5c5667ad7f8e`,
which is the BLAKE2sp hash of the 46 content bytes. `unar` reports the
checksum as "unknown".

```text
52 61 72 21 1a 07 01 00 c1 df 5f 56 03 01 04 00 ff 2c 16 c8 3b 02 03 23
ae 00 02 ae 00 a4 83 02 95 4f 0d 5e 80 00 01 05 61 2e 74 78 74 22 02 00
cb 99 5f 7b c2 c4 a1 f9 1f 57 55 5e 6c 1a a0 da 33 93 92 6e 6e 61 88 5d
fd ff 5c 56 67 ad 7f 8e 61 6c 70 68 61 20 62 65 74 61 20 67 61 6d 6d 61
20 64 65 6c 74 61 20 61 6c 70 68 61 20 62 65 74 61 20 67 61 6d 6d 61 20
64 65 6c 74 61 0a 1d 77 56 51 03 05 04 00
```

**G5** (147 bytes): metadata record in the main header (name `g5.rar`,
creation time 1794556526677346199 ns), a `CMT` service block with the
11 bytes `A comment.` + line feed (CRC-32 `0x33331277`), and the stored
file `h.txt` with a time record of flags `0x1f` (section 15.4). Members:
`h.txt` only; mtime 1577930645.000000000, ctime 1794556014.704243784,
atime 1577930645.000000000. `unar` and `7zz` agree (`7zz` shows the
comment); this `bsdtar` refuses any archive with a metadata record.

```text
52 61 72 21 1a 07 01 00 da f4 42 b2 16 01 05 12 00 11 02 0f 06 67 35 2e
72 61 72 97 47 f6 2d a7 8b e7 18 6a bb 7b bb 13 03 02 8b 00 04 8b 00 00
77 12 33 33 80 00 01 03 43 4d 54 41 20 63 6f 6d 6d 65 6e 74 2e 0a 4f 3d
d6 e1 33 02 03 1b 8d 00 04 8d 00 a4 83 02 07 3b 9b 20 80 00 01 05 68 2e
74 78 74 1a 03 1f 95 4f 0d 5e 6e c0 f6 6a 95 4f 0d 5e 00 00 00 00 48 e8
f9 29 00 00 00 00 48 65 6c 6c 6f 2c 20 52 41 52 35 21 0a 1d 77 56 51 03
05 04 00
```

**G6** (199 bytes): links. All agree.

| Member | Kind | Target | U | P |
|---|---|---|--:|--:|
| `h.txt` | regular file, stored | | 13 | 13 |
| `sym.lnk` | Unix symbolic link, attributes octal 120755 | `h.txt` | 0 | 0 |
| `hard.txt` | hard link | `h.txt` | 13 | 0 |
| `dup.txt` | file copy | `h.txt` | 13 | 0 |

```text
52 61 72 21 1a 07 01 00 c1 df 5f 56 03 01 04 00 5f ba 69 0e 1b 02 02 8d
00 06 8d 00 a4 83 02 95 4f 0d 5e 07 3b 9b 20 80 00 01 05 68 2e 74 78 74
48 65 6c 6c 6f 2c 20 52 41 52 35 21 0a b4 2b 37 9a 26 02 03 0a 00 06 00
ed c3 02 95 4f 0d 5e 00 00 00 00 80 00 01 07 73 79 6d 2e 6c 6e 6b 09 05
01 00 05 68 2e 74 78 74 5a 21 a4 66 27 02 03 0a 00 06 0d a4 83 02 95 4f
0d 5e 00 00 00 00 80 00 01 08 68 61 72 64 2e 74 78 74 09 05 04 00 05 68
2e 74 78 74 2b 13 b1 ef 26 02 03 0a 00 06 0d a4 83 02 95 4f 0d 5e 00 00
00 00 80 00 01 07 64 75 70 2e 74 78 74 09 05 05 00 05 68 2e 74 78 74 1d
77 56 51 03 05 04 00
```

### 16.2 Crafted archives a reader must accept

Unless stated, files are stored, host OS Unix, attributes octal 100644,
file flags `0x04`, contents H14.

**C1** (63 bytes): section 15.1. All agree.

**C2** (215 bytes): main header with a 3-byte size field (`a1 80 00`,
H = 33) and an extra area holding a locator (flags 3, both offsets 0 in
padded vints) and a metadata record (name `c02.rar`, time
1577930645123456789 ns); a `CMT` block (`A comment.` + line feed); the
file `a.txt` under a 2-byte size field (`94 00`); an `ACL` service block
with header flags `0x27`, a service data record and 3 data bytes; a
`QO` block with one structure; an end header with flags `0x04`; then 14
bytes of trailing text. Members: `a.txt` only. `unar` and `7zz` agree;
`7zz` shows the comment and warns about data after the end.

```text
52 61 72 21 1a 07 01 00 89 ca 09 18 a1 80 00 01 01 1d 00 09 01 03 80 80
80 00 80 80 00 12 02 0f 07 63 30 32 2e 72 61 72 15 5f c9 be 8f ef e5 15
f3 96 51 d1 10 03 02 0b 04 0b 00 77 12 33 33 00 01 03 43 4d 54 41 20 63
6f 6d 6d 65 6e 74 2e 0a a7 f0 d5 3b 94 00 02 02 0e 04 0e a4 83 02 18 a7
55 7b 00 01 05 61 2e 74 78 74 48 65 6c 6c 6f 2c 20 77 6f 72 6c 64 21 0a
79 3e 35 7e 10 03 27 03 03 00 03 00 00 01 03 41 43 4c 02 07 09 01 02 03
f0 53 00 9c 0b 03 06 21 00 21 00 00 01 02 51 4f d5 ef 8e ce 1c 00 00 19
15 0a 5e 31 14 02 02 0e 04 0e a4 83 02 18 a7 55 7b 00 01 05 61 2e 74 78
74 1d 77 56 51 03 05 04 00 74 72 61 69 6c 69 6e 67 20 62 79 74 65 73
```

**C3** (254 bytes): extra records. All agree where they show the field
(`7zz` shows every time to full precision, attributes, CRC-32 and the
BLAKE2sp value; `bsdtar` the version suffix).

| Member | Details |
|---|---|
| `dir` | directory; Windows; attributes `0x10`; file flags `0x03`; header mtime 1500000000 |
| `dir/w.txt` | Windows; attributes `0x21`; file flags `0x06` with header mtime 1500000001, **overridden** by the time record: mtime 1000000000.123456700, ctime 1100000000.0, atime 1200000000.0 (FILETIME, flags `0x0e`); an unknown record of type `0x55` (skipped); owner record flags `0x0f`: user `user`, group `group`, user id 1000, group id 100; hash record `b7171e34094c38df9434e344d9f079ab091cf51c48e3764bc20e9ca08c54ec9f`; CRC-32 `0x7B55A718`; both checksums match |
| `u.txt;3` | Unix; attributes octal 100600; U written as the 5-byte vint `8e 80 80 80 00`; time record flags `0x1f`: mtime 1000000000.000000001, ctime 1100000000.999999999, atime 1200000000 with a nanosecond field of 1000000000, which is ignored; version record with number 3 |

```text
52 61 72 21 1a 07 01 00 c5 1a 33 32 03 01 00 00 c3 02 f0 5d 10 02 02 00
03 00 10 00 2f 68 59 00 00 03 64 69 72 f2 c8 4e 4b 73 02 03 58 0e 06 0e
21 01 2f 68 59 18 a7 55 7b 00 00 09 64 69 72 2f 77 2e 74 78 74 08 55 75
6e 6b 6e 6f 77 6e 1a 03 0e 87 56 12 45 d1 38 c1 01 00 00 c6 e9 4f c6 c4
01 00 80 8c 8e ce 53 c8 01 10 06 0f 04 75 73 65 72 05 67 72 6f 75 70 e8
07 64 22 02 00 b7 17 1e 34 09 4c 38 df 94 34 e3 44 d9 f0 79 ab 09 1c f5
1c 48 e3 76 4b c2 0e 9c a0 8c 54 ec 9f 48 65 6c 6c 6f 2c 20 77 6f 72 6c
64 21 0a b1 2e 0e a2 38 02 03 1f 0e 04 8e 80 80 80 00 80 83 02 18 a7 55
7b 00 01 05 75 2e 74 78 74 1a 03 1f 00 ca 9a 3b 00 ab 90 41 00 8c 86 47
01 00 00 00 ff c9 9a 3b 00 ca 9a 3b 03 04 00 03 48 65 6c 6c 6f 2c 20 77
6f 72 6c 64 21 0a 19 b2 3a 35 03 05 00 00
```

**C4** (268 bytes): every redirection type. The three tools list the
same kinds and targets, except that `unar` treats every redirection as a
symbolic link and `7zz` reports the unknown type as a data error.

| Member | Kind | Target | U | Notes |
|---|---|---|--:|---|
| `t.txt` | regular file | | 14 | |
| `sym` | Unix symbolic link | `t.txt` | 0 | attributes octal 120777 |
| `wsym` | Windows symbolic link | `t.txt` | 0 | Windows, attributes `0x420` |
| `junc` | Windows junction | `\??\C:\x` | 0 | Windows, attributes `0x410`, directory flag set, redirection flags 1 |
| `hard` | hard link | `t.txt` | 14 | contents, if asked for, are those of `t.txt` |
| `copy` | file copy | `t.txt` | 14 | likewise |
| `unk` | unknown redirection (type 9) | `t.txt` | 14 | failed, unsupported |

```text
52 61 72 21 1a 07 01 00 c5 1a 33 32 03 01 00 00 e7 12 5e 99 14 02 02 0e
04 0e a4 83 02 18 a7 55 7b 00 01 05 74 2e 74 78 74 48 65 6c 6c 6f 2c 20
77 6f 72 6c 64 21 0a e5 b1 76 e3 1d 02 03 0a 00 04 00 ff c3 02 00 00 00
00 00 01 03 73 79 6d 09 05 01 00 05 74 2e 74 78 74 12 98 59 3e 1d 02 03
0a 00 04 00 a0 08 00 00 00 00 00 00 04 77 73 79 6d 09 05 02 00 05 74 2e
74 78 74 36 3d 12 dd 1c 02 03 0d 00 01 00 90 08 00 00 04 6a 75 6e 63 0c
05 03 01 08 5c 3f 3f 5c 43 3a 5c 78 43 e1 5c 4c 1e 02 03 0a 00 04 0e a4
83 02 00 00 00 00 00 01 04 68 61 72 64 09 05 04 00 05 74 2e 74 78 74 d3
c1 71 bc 1e 02 03 0a 00 04 0e a4 83 02 00 00 00 00 00 01 04 63 6f 70 79
09 05 05 00 05 74 2e 74 78 74 f3 13 d9 da 1d 02 03 0a 00 04 0e a4 83 02
00 00 00 00 00 01 03 75 6e 6b 09 05 09 00 05 74 2e 74 78 74 19 b2 3a 35
03 05 00 00
```

**C5** (71 bytes): unknown size. File flags `0x0c`, U field
`0x7FFFFFFF7FFFFFFF`, stored, S = 14. Expected: `a.txt`, 14 bytes H14,
CRC-32 matches (DECISION, 9.4). The tools disagree here: `unar` fails
the file, `7zz` reports success with a size of 0, `bsdtar` refuses
unknown sizes.

```text
52 61 72 21 1a 07 01 00 c5 1a 33 32 03 01 00 00 cc 33 c5 aa 1c 02 02 0e
0c ff ff ff ff f7 ff ff ff 7f a4 83 02 18 a7 55 7b 00 01 05 61 2e 74 78
74 48 65 6c 6c 6f 2c 20 77 6f 72 6c 64 21 0a 19 b2 3a 35 03 05 00 00
```

**C6** (154 bytes): `a.txt` with an encryption record (version 0, flags
3, KDF count 15, salt `00`..`0f`, IV `10`..`1f`, twelve zero check
bytes), U = 14, S = 16, CRC-32 field `0x12345678`; then a plain `b.txt`.
Expected: `a.txt` failed, encrypted (no checksum complaint); `b.txt`
14 bytes H14. All three tools list both and mark or refuse the first.

```text
52 61 72 21 1a 07 01 00 c5 1a 33 32 03 01 00 00 4e 41 54 0e 46 02 03 31
10 04 0e a4 83 02 78 56 34 12 00 01 05 61 2e 74 78 74 30 01 00 03 0f 00
01 02 03 04 05 06 07 08 09 0a 0b 0c 0d 0e 0f 10 11 12 13 14 15 16 17 18
19 1a 1b 1c 1d 1e 1f 00 00 00 00 00 00 00 00 00 00 00 00 40 41 42 43 44
45 46 47 48 49 4a 4b 4c 4d 4e 4f c5 70 fe 76 14 02 02 0e 04 0e a4 83 02
18 a7 55 7b 00 01 05 62 2e 74 78 74 48 65 6c 6c 6f 2c 20 77 6f 72 6c 64
21 0a 19 b2 3a 35 03 05 00 00
```

**C7** (78 bytes): encrypted headers. The first block has type 4
(version 0, flags 1, KDF count 15, salt `00`..`0f`, twelve zero check
bytes; H = 33); 32 arbitrary bytes follow. Expected: stop reason
encrypted headers, no members.

```text
52 61 72 21 1a 07 01 00 b7 36 cb 61 21 04 00 00 01 0f 00 01 02 03 04 05
06 07 08 09 0a 0b 0c 0d 0e 0f 00 00 00 00 00 00 00 00 00 00 00 00 80 81
82 83 84 85 86 87 88 89 8a 8b 8c 8d 8e 8f 90 91 92 93 94 95 96 97 98 99
9a 9b 9c 9d 9e 9f
```

**C8** (three volumes: 100, 106 and 138 bytes): section 15.6. `unar` and
`7zz` agree.

Volume 1:

```text
52 61 72 21 1a 07 01 00 53 2a 34 45 03 01 00 01 15 bf a0 a3 38 02 13 23
0a 04 2d a4 83 02 34 14 ec a3 00 01 05 66 2e 74 78 74 22 02 00 86 a6 d7
99 ff 78 5e 55 e9 77 b2 77 17 b2 48 f9 97 f2 ad 23 3e c8 a1 18 68 fd ea
3c ff 13 a9 36 54 68 65 20 71 75 69 63 6b 20 8f 82 3d 42 03 05 00 01 00
00 00 00 00
```

Volume 2:

```text
52 61 72 21 1a 07 01 00 ed 55 34 d7 04 01 00 03 01 9a 06 01 b4 38 02 1b
23 14 04 2d a4 83 02 0c c9 0f d9 00 01 05 66 2e 74 78 74 22 02 00 65 a2
bc 8b 20 74 57 4a f3 4d 9b 6d f9 0b d4 84 6a 70 38 a1 00 76 90 7a fd b4
a4 a3 e1 90 d8 14 62 72 6f 77 6e 20 66 6f 78 20 6a 75 6d 70 73 20 6f 76
65 72 8f 82 3d 42 03 05 00 01
```

Volume 3:

```text
52 61 72 21 1a 07 01 00 57 04 3d 4e 04 01 00 03 02 32 e1 03 d2 38 02 0b
23 0f 04 2d a4 83 02 6a cc 50 eb 00 01 05 66 2e 74 78 74 22 02 00 c9 7e
0a e0 f6 0c 80 bf f1 5f ab 26 f3 06 c9 3d 5f 32 7b d8 e4 9d 02 b0 c0 ca
c9 d8 10 4d 8a c0 20 74 68 65 20 6c 61 7a 79 20 64 6f 67 2e 0a 6f 21 92
ba 14 02 02 0c 04 0c a4 83 02 82 ff 72 e4 00 01 05 65 2e 74 78 74 73 65
63 6f 6e 64 20 66 69 6c 65 0a 19 b2 3a 35 03 05 00 00
```

**C9** (79 bytes): `4d 5a`, then the 8 signature bytes followed by 6
zero bytes (a false candidate: the "block" after it has H = 0), then C1
at offset 16. Expected: archive found at offset 16; `a.txt` as in C1.
(`lsar` takes the first match and reads nothing.)

```text
4d 5a 52 61 72 21 1a 07 01 00 00 00 00 00 00 00 52 61 72 21 1a 07 01 00
c5 1a 33 32 03 01 00 00 15 0a 5e 31 14 02 02 0e 04 0e a4 83 02 18 a7 55
7b 00 01 05 61 2e 74 78 74 48 65 6c 6c 6f 2c 20 77 6f 72 6c 64 21 0a 19
b2 3a 35 03 05 00 00
```

Also: C1 preceded by exactly 1,048,576 zero bytes is found; preceded by
1,048,577 zero bytes it is not a RAR 5 archive. The 14 bytes
`52 61 72 21 1a 07 01 00 00 00 00 00 00 00` repeated 63 times and
followed by C1 give `a.txt` as in C1 (the 64th candidate is accepted);
repeated 64 times and followed by C1 they give "not a RAR 5 archive".

**C10** (72 bytes): as C1 with U written as a 10-byte vint. Expected as
C1. `unar` and `7zz` agree.

```text
52 61 72 21 1a 07 01 00 c5 1a 33 32 03 01 00 00 f7 2c 42 fd 1d 02 02 0e
04 8e 80 80 80 80 80 80 80 80 00 a4 83 02 18 a7 55 7b 00 01 05 61 2e 74
78 74 48 65 6c 6c 6f 2c 20 77 6f 72 6c 64 21 0a 19 b2 3a 35 03 05 00 00
```

**C11** (78 bytes): 6 unknown bytes (`future`) between the name and the
extra area of the file header, whose extra area is a version record
(number 2); and an end header with H = 7 holding 4 extra bytes after its
flags. Expected: one member `a.txt;2`, H14. `unar` and `bsdtar` agree;
`7zz` reports a header error.

```text
52 61 72 21 1a 07 01 00 c5 1a 33 32 03 01 00 00 a6 3a 62 b6 1f 02 03 04
0e 04 0e a4 83 02 18 a7 55 7b 00 01 05 61 2e 74 78 74 66 75 74 75 72 65
03 04 00 02 48 65 6c 6c 6f 2c 20 77 6f 72 6c 64 21 0a f2 ba 60 1a 07 05
00 00 6a 75 6e 6b
```

**C12** (98 bytes): a block of type 9 (header flags `0x06`, 3 body
bytes, 12 data bytes) and a block of type 0 (4 data bytes) before the
file. Expected: `a.txt` as in C1, two diagnostics. `unar` agrees; `7zz`
and `bsdtar` refuse the archive.

```text
52 61 72 21 1a 07 01 00 c5 1a 33 32 03 01 00 00 e1 92 ea 08 06 09 06 0c
78 79 7a 44 41 54 41 44 41 54 41 44 41 54 41 69 d6 aa 06 03 00 02 04 7a
65 72 6f 15 0a 5e 31 14 02 02 0e 04 0e a4 83 02 18 a7 55 7b 00 01 05 61
2e 74 78 74 48 65 6c 6c 6f 2c 20 77 6f 72 6c 64 21 0a 19 b2 3a 35 03 05
00 00
```

**C13** (159 bytes): repeated extra records. `a.txt`, file flags `0x00`
(no CRC-32 field), with these records in order: a time record (flags 3,
mtime 1000000000); a second time record (mtime 1100000000); a hash
record of the unknown hash type 1 with 4 data bytes; a hash record of
type 0 holding the BLAKE2sp hash of H14; a second hash record of type 0
holding 32 zero bytes; a version record with number 5; a version record
with number 6. Expected: one member `a.txt;5`, mtime 1000000000, 14
bytes H14, BLAKE2sp verified against `b7171e34...` (5.5, 7.3; a reader
that used the last hash record would report a mismatch). The tools
differ: `7zz` shows the first time, `lsar` and `bsdtar` the second;
`bsdtar` names the member `a.txt;5;6`; none of the three verifies the
hash it lists.

```text
52 61 72 21 1a 07 01 00 c5 1a 33 32 03 01 00 00 61 aa 68 ca 74 02 03 63
0e 00 0e a4 83 02 00 01 05 61 2e 74 78 74 06 03 03 00 ca 9a 3b 06 03 03
00 ab 90 41 06 02 01 aa bb cc dd 22 02 00 b7 17 1e 34 09 4c 38 df 94 34
e3 44 d9 f0 79 ab 09 1c f5 1c 48 e3 76 4b c2 0e 9c a0 8c 54 ec 9f 22 02
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 03 04 00 05 03 04 00 06 48 65 6c 6c 6f 2c 20
77 6f 72 6c 64 21 0a 19 b2 3a 35 03 05 00 00
```

**C14** (276 bytes): seven small cases in one archive. All members are
delivered without failure.

| Member | What it exercises | Expected |
|---|---|---|
| `v1.txt` | compression information `0xf8c01`: version 1, stored, N = 3, F = 31 | 14 bytes H14; D, if reported, 2,064,384 (`7zz` shows 2016K) |
| `v1b.txt` | `0x1f8c01`: the same with bit 20 | 14 bytes H14 |
| `empty.txt` | header flags `0x00`: no data size field; U = 0; CRC-32 field 0 | empty file, CRC-32 verified |
| `d` | directory (file flags `0x01`, Unix, octal 40755) with a data area of 4 bytes | directory; the 4 bytes are skipped |
| name bytes `66 ff 2e 74` | invalid UTF-8 | 14 bytes H14 under the name `f`, U+FFFD, `.t` (or the raw bytes) |
| `os7.txt` | host OS 7, attributes `0x1234` | 14 bytes H14; attributes unknown |
| `ff.txt` | file flags `0x24` (an undefined bit) | 14 bytes H14, CRC-32 verified |

`unar` and `7zz` read all seven; `bsdtar` stops at `empty.txt` ("no
data found in file/service block").

```text
52 61 72 21 1a 07 01 00 c5 1a 33 32 03 01 00 00 78 b7 6f 9f 17 02 02 0e
04 0e a4 83 02 18 a7 55 7b 81 98 3e 01 06 76 31 2e 74 78 74 48 65 6c 6c
6f 2c 20 77 6f 72 6c 64 21 0a d6 48 7c f7 18 02 02 0e 04 0e a4 83 02 18
a7 55 7b 81 98 7e 01 07 76 31 62 2e 74 78 74 48 65 6c 6c 6f 2c 20 77 6f
72 6c 64 21 0a 29 9b f2 9c 17 02 00 04 00 a4 83 02 00 00 00 00 00 01 09
65 6d 70 74 79 2e 74 78 74 38 1a 3c aa 0c 02 02 04 01 00 ed 83 01 00 01
01 64 44 41 54 41 94 9b b4 bb 13 02 02 0e 04 0e a4 83 02 18 a7 55 7b 00
01 04 66 ff 2e 74 48 65 6c 6c 6f 2c 20 77 6f 72 6c 64 21 0a cf 52 e0 ad
15 02 02 0e 04 0e b4 24 18 a7 55 7b 00 07 07 6f 73 37 2e 74 78 74 48 65
6c 6c 6f 2c 20 77 6f 72 6c 64 21 0a ba 5c 99 e7 15 02 02 0e 24 0e a4 83
02 18 a7 55 7b 00 01 06 66 66 2e 74 78 74 48 65 6c 6c 6f 2c 20 77 6f 72
6c 64 21 0a 19 b2 3a 35 03 05 00 00
```

### 16.3 Archives with errors

"Offset" is where reading stops. All header CRCs in these vectors are
correct unless the vector is about the CRC.

**E1** (102 bytes): the header CRC of the first file header is wrong.
Expected: stop at 16, corrupt header; no members. (`unar` lists and
extracts both files.)

```text
52 61 72 21 1a 07 01 00 c5 1a 33 32 03 01 00 00 6d 5c 6a 23 14 02 02 0e
04 0e a4 83 02 18 a7 55 7b 00 01 05 61 2e 74 78 74 48 65 6c 6c 6f 2c 20
77 6f 72 6c 64 21 0a c5 70 fe 76 14 02 02 0e 04 0e a4 83 02 18 a7 55 7b
00 01 05 62 2e 74 78 74 48 65 6c 6c 6f 2c 20 77 6f 72 6c 64 21 0a 19 b2
3a 35 03 05 00 00
```

**E2** (66 bytes): header size field of 4 bytes (`94 80 80 00`).
Expected: stop at 16, malformed header; no members.

```text
52 61 72 21 1a 07 01 00 c5 1a 33 32 03 01 00 00 4e 74 2b 95 94 80 80 00
02 02 0e 04 0e a4 83 02 18 a7 55 7b 00 01 05 61 2e 74 78 74 48 65 6c 6c
6f 2c 20 77 6f 72 6c 64 21 0a 19 b2 3a 35 03 05 00 00
```

**E3** (73 bytes): U is an 11-byte vint. Expected: stop at 16, malformed
header.

```text
52 61 72 21 1a 07 01 00 c5 1a 33 32 03 01 00 00 e6 21 72 97 1e 02 02 0e
04 8e 80 80 80 80 80 80 80 80 80 00 a4 83 02 18 a7 55 7b 00 01 05 61 2e
74 78 74 48 65 6c 6c 6f 2c 20 77 6f 72 6c 64 21 0a 19 b2 3a 35 03 05 00
00
```

**E4** (72 bytes): U is a 10-byte vint whose last byte is `02`.
Expected: stop at 16, malformed header.

```text
52 61 72 21 1a 07 01 00 c5 1a 33 32 03 01 00 00 94 09 e2 7a 1d 02 02 0e
04 8e 80 80 80 80 80 80 80 80 02 a4 83 02 18 a7 55 7b 00 01 05 61 2e 74
78 74 48 65 6c 6c 6f 2c 20 77 6f 72 6c 64 21 0a 19 b2 3a 35 03 05 00 00
```

**E5** (102 bytes): extra area size 200 in a header of 20 bytes.
Expected: stop at 16, malformed header.

```text
52 61 72 21 1a 07 01 00 c5 1a 33 32 03 01 00 00 72 a8 1f 9a 14 02 03 c8
01 0e 04 0e 00 18 a7 55 7b 00 01 05 61 2e 74 78 74 48 65 6c 6c 6f 2c 20
77 6f 72 6c 64 21 0a c5 70 fe 76 14 02 02 0e 04 0e a4 83 02 18 a7 55 7b
00 01 05 62 2e 74 78 74 48 65 6c 6c 6f 2c 20 77 6f 72 6c 64 21 0a 19 b2
3a 35 03 05 00 00
```

**E6** (100 bytes): name length 50 with 5 bytes left in the header.
Expected: stop at 16, malformed header. (`unar` lists a name made of the
five bytes and the 45 bytes that follow the header.)

```text
52 61 72 21 1a 07 01 00 c5 1a 33 32 03 01 00 00 e0 40 e6 1a 12 02 02 0e
04 0e 00 18 a7 55 7b 00 01 32 61 2e 74 78 74 48 65 6c 6c 6f 2c 20 77 6f
72 6c 64 21 0a c5 70 fe 76 14 02 02 0e 04 0e a4 83 02 18 a7 55 7b 00 01
05 62 2e 74 78 74 48 65 6c 6c 6f 2c 20 77 6f 72 6c 64 21 0a 19 b2 3a 35
03 05 00 00
```

**E7** (107 bytes): after `a.txt`, a block with H = 0 (and a correct CRC
over its one size byte). Expected: `a.txt` as in C1; stop at 55,
malformed header.

```text
52 61 72 21 1a 07 01 00 c5 1a 33 32 03 01 00 00 15 0a 5e 31 14 02 02 0e
04 0e a4 83 02 18 a7 55 7b 00 01 05 61 2e 74 78 74 48 65 6c 6c 6f 2c 20
77 6f 72 6c 64 21 0a 8d ef 02 d2 00 c5 70 fe 76 14 02 02 0e 04 0e a4 83
02 18 a7 55 7b 00 01 05 62 2e 74 78 74 48 65 6c 6c 6f 2c 20 77 6f 72 6c
64 21 0a 19 b2 3a 35 03 05 00 00
```

**E8** (89 bytes): the file ends inside the data area of `b.txt` (9 of
its 14 data bytes are present). Expected: `a.txt` as
in C1; `b.txt` failed, truncated; stop at 55, truncated.

```text
52 61 72 21 1a 07 01 00 c5 1a 33 32 03 01 00 00 15 0a 5e 31 14 02 02 0e
04 0e a4 83 02 18 a7 55 7b 00 01 05 61 2e 74 78 74 48 65 6c 6c 6f 2c 20
77 6f 72 6c 64 21 0a c5 70 fe 76 14 02 02 0e 04 0e a4 83 02 18 a7 55 7b
00 01 05 62 2e 74 78 74 48 65 6c 6c 6f 2c 20 77 6f
```

**E9** (68 bytes): S = 2^40 (`80 80 80 80 80 20`). Expected: `a.txt`
failed, truncated; stop at 16, truncated. Nothing is allocated.

```text
52 61 72 21 1a 07 01 00 c5 1a 33 32 03 01 00 00 4e f1 ab aa 19 02 02 80
80 80 80 80 20 04 0e a4 83 02 18 a7 55 7b 00 01 05 61 2e 74 78 74 48 65
6c 6c 6f 2c 20 77 6f 72 6c 64 21 0a 19 b2 3a 35 03 05 00 00
```

**E10** (103 bytes): two stored files with S = 14: `a.txt` with
U = 1000 and `b.txt` with U = 5 (and the CRC-32 of the first 5 bytes).
Expected: both failed, corrupt; no stop reason.

```text
52 61 72 21 1a 07 01 00 c5 1a 33 32 03 01 00 00 54 fb 92 f4 15 02 02 0e
04 e8 07 a4 83 02 18 a7 55 7b 00 01 05 61 2e 74 78 74 48 65 6c 6c 6f 2c
20 77 6f 72 6c 64 21 0a f4 cf a8 11 14 02 02 0e 04 05 a4 83 02 82 89 d1
f7 00 01 05 62 2e 74 78 74 48 65 6c 6c 6f 2c 20 77 6f 72 6c 64 21 0a 19
b2 3a 35 03 05 00 00
```

**E11** (63 bytes): data CRC-32 field 1. Expected: `a.txt` failed,
corrupt (CRC-32 mismatch).

```text
52 61 72 21 1a 07 01 00 c5 1a 33 32 03 01 00 00 b7 62 62 55 14 02 02 0e
04 0e a4 83 02 01 00 00 00 00 01 05 61 2e 74 78 74 48 65 6c 6c 6f 2c 20
77 6f 72 6c 64 21 0a 19 b2 3a 35 03 05 00 00
```

**E12** (95 bytes): no CRC-32 field; a hash record whose first byte is
`b6` where the BLAKE2sp hash of H14 has `b7`. Expected: `a.txt` failed,
corrupt (BLAKE2sp mismatch). (`7zz` reports the failure; `unar` and
`bsdtar` accept the file.)

```text
52 61 72 21 1a 07 01 00 c5 1a 33 32 03 01 00 00 bd 3e c1 d7 34 02 03 23
0e 00 0e a4 83 02 00 01 05 61 2e 74 78 74 22 02 00 b6 17 1e 34 09 4c 38
df 94 34 e3 44 d9 f0 79 ab 09 1c f5 1c 48 e3 76 4b c2 0e 9c a0 8c 54 ec
9f 48 65 6c 6c 6f 2c 20 77 6f 72 6c 64 21 0a 19 b2 3a 35 03 05 00 00
```

**E13** (55 bytes): C1 without its main header. Expected: not a RAR 5
archive (the only candidate is followed by a block of type 2).

```text
52 61 72 21 1a 07 01 00 15 0a 5e 31 14 02 02 0e 04 0e a4 83 02 18 a7 55
7b 00 01 05 61 2e 74 78 74 48 65 6c 6c 6f 2c 20 77 6f 72 6c 64 21 0a 19
b2 3a 35 03 05 00 00
```

**E14** (110 bytes): a second main header after `a.txt`. Expected:
`a.txt` as in C1; stop at 55, malformed header.

```text
52 61 72 21 1a 07 01 00 c5 1a 33 32 03 01 00 00 15 0a 5e 31 14 02 02 0e
04 0e a4 83 02 18 a7 55 7b 00 01 05 61 2e 74 78 74 48 65 6c 6c 6f 2c 20
77 6f 72 6c 64 21 0a c5 1a 33 32 03 01 00 00 c5 70 fe 76 14 02 02 0e 04
0e a4 83 02 18 a7 55 7b 00 01 05 62 2e 74 78 74 48 65 6c 6c 6f 2c 20 77
6f 72 6c 64 21 0a 19 b2 3a 35 03 05 00 00
```

**E15** (132 bytes): G1 with the block of `a.txt` removed, so the
archive begins with a member whose solid bit is set. Expected: `b.txt`
failed, no solid state; `e.txt` empty, CRC-32 0, delivered; `c.txt`
failed, poisoned group. (`bsdtar`: "declared solid file, but no window
buffer initialized yet".)

```text
52 61 72 21 1a 07 01 00 d8 1b 32 51 03 01 04 04 ed 7f 69 78 1b 02 02 86
00 06 b4 00 a4 83 02 95 4f 0d 5e 5a bb 3a 87 c0 1b 01 05 62 2e 74 78 74
47 1e 03 a1 bf b0 84 29 ee 24 1b 02 02 80 00 06 80 00 a4 83 02 95 4f 0d
5e 00 00 00 00 c0 1b 01 05 65 2e 74 78 74 61 5e 06 ad 1b 02 02 86 00 06
9e 00 a4 83 02 95 4f 0d 5e 5a b6 40 41 c0 1b 01 05 63 2e 74 78 74 40 19
03 9b d8 00 1d 77 56 51 03 05 04 00
```

**E16** (225 bytes): five files with S = 14 and compression information
`0x0182` (version 2), `0x0300` (method 6), `0x4180` (version 0, N = 16),
`0x6181` (version 1, N = 24) and `0x0000`. Expected: `a.txt`, `b.txt`,
`c.txt`, `d.txt` failed, unsupported; `e.txt` 14 bytes H14.

```text
52 61 72 21 1a 07 01 00 c5 1a 33 32 03 01 00 00 54 54 be 49 15 02 02 0e
04 0e a4 83 02 18 a7 55 7b 82 03 01 05 61 2e 74 78 74 48 65 6c 6c 6f 2c
20 77 6f 72 6c 64 21 0a 66 08 08 68 15 02 02 0e 04 0e a4 83 02 18 a7 55
7b 80 06 01 05 62 2e 74 78 74 48 65 6c 6c 6f 2c 20 77 6f 72 6c 64 21 0a
57 34 36 3b 16 02 02 0e 04 0e a4 83 02 18 a7 55 7b 80 83 01 01 05 63 2e
74 78 74 48 65 6c 6c 6f 2c 20 77 6f 72 6c 64 21 0a 7e 80 46 b8 16 02 02
0e 04 0e a4 83 02 18 a7 55 7b 81 c3 01 01 05 64 2e 74 78 74 48 65 6c 6c
6f 2c 20 77 6f 72 6c 64 21 0a d5 ac de c4 14 02 02 0e 04 0e a4 83 02 18
a7 55 7b 00 01 05 65 2e 74 78 74 48 65 6c 6c 6f 2c 20 77 6f 72 6c 64 21
0a 19 b2 3a 35 03 05 00 00
```

**E17** (volume sets derived from C8):

| # | Set | Expected |
|---|---|---|
| E17a | volume 1 of C8 replaced by the 95 bytes below (CRC-32 field of the first piece is 1) | `f.txt` failed, corrupt (piece CRC-32 mismatch); `e.txt` correct |
| E17b | volume 2 of C8 replaced by the 106 bytes below (the piece is named `g.txt`) | three failed, incomplete members: `f.txt` (10 bytes, continuation missing), `g.txt` (starts earlier and continuation missing), `f.txt` (starts earlier); then `e.txt` correct |
| E17c | volumes 1 and 2 of C8 only | `f.txt` failed, incomplete; stop reason missing volume |
| E17d | volumes 2 and 3 of C8 only | `f.txt` failed, incomplete (starts in an earlier volume); `e.txt` correct; no volume-number diagnostic |
| E17e | volume 2 of C8 replaced by the second 106 bytes below (volume number 7) | as C8, with one diagnostic |
| E17f | volume 3 of C8 replaced by the 138 bytes below (the last piece of `f.txt` has compression information `0x40`) | `f.txt` failed, corrupt (11.3); `e.txt` correct. (`unar` and `7zz` accept `f.txt`.) |
| E17g | volume 2 of C8 replaced by the 134 bytes below (a file `x.txt`, 3 bytes `xx` + line feed, precedes the piece) | four members: `f.txt` failed, incomplete (10 bytes present); `x.txt` correct; `f.txt` failed, incomplete (pieces of 20 and 15 bytes, beginning missing); `e.txt` correct. (`unar` reports the same four; `7zz` joins the three pieces.) |

E17a, volume 1:

```text
52 61 72 21 1a 07 01 00 53 2a 34 45 03 01 00 01 1e ae a3 e9 38 02 13 23
0a 04 2d a4 83 02 01 00 00 00 00 01 05 66 2e 74 78 74 22 02 00 86 a6 d7
99 ff 78 5e 55 e9 77 b2 77 17 b2 48 f9 97 f2 ad 23 3e c8 a1 18 68 fd ea
3c ff 13 a9 36 54 68 65 20 71 75 69 63 6b 20 8f 82 3d 42 03 05 00 01
```

E17b, volume 2:

```text
52 61 72 21 1a 07 01 00 ed 55 34 d7 04 01 00 03 01 dd 94 45 1b 38 02 1b
23 14 04 2d a4 83 02 0c c9 0f d9 00 01 05 67 2e 74 78 74 22 02 00 65 a2
bc 8b 20 74 57 4a f3 4d 9b 6d f9 0b d4 84 6a 70 38 a1 00 76 90 7a fd b4
a4 a3 e1 90 d8 14 62 72 6f 77 6e 20 66 6f 78 20 6a 75 6d 70 73 20 6f 76
65 72 8f 82 3d 42 03 05 00 01
```

E17e, volume 2:

```text
52 61 72 21 1a 07 01 00 d8 f0 57 3e 04 01 00 03 07 9a 06 01 b4 38 02 1b
23 14 04 2d a4 83 02 0c c9 0f d9 00 01 05 66 2e 74 78 74 22 02 00 65 a2
bc 8b 20 74 57 4a f3 4d 9b 6d f9 0b d4 84 6a 70 38 a1 00 76 90 7a fd b4
a4 a3 e1 90 d8 14 62 72 6f 77 6e 20 66 6f 78 20 6a 75 6d 70 73 20 6f 76
65 72 8f 82 3d 42 03 05 00 01
```

E17f, volume 3:

```text
52 61 72 21 1a 07 01 00 57 04 3d 4e 04 01 00 03 02 29 03 f5 17 38 02 0b
23 0f 04 2d a4 83 02 6a cc 50 eb 40 01 05 66 2e 74 78 74 22 02 00 c9 7e
0a e0 f6 0c 80 bf f1 5f ab 26 f3 06 c9 3d 5f 32 7b d8 e4 9d 02 b0 c0 ca
c9 d8 10 4d 8a c0 20 74 68 65 20 6c 61 7a 79 20 64 6f 67 2e 0a 6f 21 92
ba 14 02 02 0c 04 0c a4 83 02 82 ff 72 e4 00 01 05 65 2e 74 78 74 73 65
63 6f 6e 64 20 66 69 6c 65 0a 19 b2 3a 35 03 05 00 00
```

E17g, volume 2:

```text
52 61 72 21 1a 07 01 00 ed 55 34 d7 04 01 00 03 01 6f 4e 6c df 14 02 02
03 04 03 a4 83 02 1a fa 90 a2 00 01 05 78 2e 74 78 74 78 78 0a 9a 06 01
b4 38 02 1b 23 14 04 2d a4 83 02 0c c9 0f d9 00 01 05 66 2e 74 78 74 22
02 00 65 a2 bc 8b 20 74 57 4a f3 4d 9b 6d f9 0b d4 84 6a 70 38 a1 00 76
90 7a fd b4 a4 a3 e1 90 d8 14 62 72 6f 77 6e 20 66 6f 78 20 6a 75 6d 70
73 20 6f 76 65 72 8f 82 3d 42 03 05 00 01
```

**E18** (142 bytes): damaged extra areas. `a.txt`: a valid time record
(flags 3, mtime 1000000000), then a record that claims 100 bytes with 3
left. `b.txt`: a time record too short for its three FILETIMEs, a hash
record with 10 hash bytes, a redirection record whose target length is
99, and an encryption record cut off after its version. Expected:
`a.txt` 14 bytes H14 with mtime 1000000000 and one diagnostic; `b.txt`
a regular file, no times, failed, **encrypted**, three diagnostics.

```text
52 61 72 21 1a 07 01 00 c5 1a 33 32 03 01 00 00 cf 65 82 3c 20 02 03 0b
0e 04 0e a4 83 02 18 a7 55 7b 00 01 05 61 2e 74 78 74 06 03 03 00 ca 9a
3b 64 55 61 62 48 65 6c 6c 6f 2c 20 77 6f 72 6c 64 21 0a 6b 5b fb 99 30
02 03 1b 0e 04 0e a4 83 02 18 a7 55 7b 00 01 05 62 2e 74 78 74 04 03 0e
01 02 0c 02 00 b7 17 1e 34 09 4c 38 df 94 34 05 05 01 00 63 74 02 01 00
48 65 6c 6c 6f 2c 20 77 6f 72 6c 64 21 0a 19 b2 3a 35 03 05 00 00
```

**E19** (55 bytes): the first 55 bytes of C1, that is, C1 without its
end header. Expected: `a.txt` as in C1, one diagnostic, no stop reason.

**E20** (142 bytes): names. The first file's name bytes are
`6e 75 6c 00 68 69 64 2e 74 78 74`; the second file's name length is 0.
Expected: a member named `nul` (14 bytes H14); stop at 61, malformed
header; the third file is not reached.

```text
52 61 72 21 1a 07 01 00 c5 1a 33 32 03 01 00 00 85 41 f5 cb 1a 02 02 0e
04 0e a4 83 02 18 a7 55 7b 00 01 0b 6e 75 6c 00 68 69 64 2e 74 78 74 48
65 6c 6c 6f 2c 20 77 6f 72 6c 64 21 0a 5e 71 9f 38 0f 02 02 0e 04 0e a4
83 02 18 a7 55 7b 00 01 00 48 65 6c 6c 6f 2c 20 77 6f 72 6c 64 21 0a c5
70 fe 76 14 02 02 0e 04 0e a4 83 02 18 a7 55 7b 00 01 05 62 2e 74 78 74
48 65 6c 6c 6f 2c 20 77 6f 72 6c 64 21 0a 19 b2 3a 35 03 05 00 00
```

**E21** (295 bytes): the solid rules of 12.2 on members that never
reach the decoder. Main header with archive flags `0x04`. No reader can
decode anything here, so this vector checks only the status given to
each member; it follows from the rules of this document (DECISION) and
the tools merely list the same seven names.

| Member | Compression information | U | P | Other | Expected |
|---|---|--:|--:|---|---|
| `a.bin` | `0x0180` | 20 | 16 | encryption record | failed, encrypted; the group state becomes poisoned |
| `b.bin` | `0x01c0` | 20 | 5 | | failed, poisoned group |
| `s.txt` | `0x0040` | 14 | 14 | stored, H14 | delivered, CRC-32 verified |
| `z.bin` | `0x01c0` | 0 | 0 | CRC-32 field 0 | delivered empty |
| `n.bin` | `0x0180` | 5 | 0 | | failed, corrupt (9.3); the state, reset by the clear solid bit, becomes poisoned |
| `c.bin` | `0x01c0` | 20 | 5 | | failed, poisoned group |
| `u.bin` | `0x0180` | unknown (file flags `0x0c`) | 0 | | failed, corrupt (9.3) |

```text
52 61 72 21 1a 07 01 00 dc de 5e 35 03 01 00 04 3e bd 15 44 47 02 03 31
10 04 14 a4 83 02 11 11 11 11 80 03 01 05 61 2e 62 69 6e 30 01 00 03 0f
00 01 02 03 04 05 06 07 08 09 0a 0b 0c 0d 0e 0f 10 11 12 13 14 15 16 17
18 19 1a 1b 1c 1d 1e 1f 00 00 00 00 00 00 00 00 00 00 00 00 40 41 42 43
44 45 46 47 48 49 4a 4b 4c 4d 4e 4f ae a4 ba 90 15 02 02 05 04 14 a4 83
02 22 22 22 22 c0 03 01 05 62 2e 62 69 6e 01 02 03 04 05 1a 0c e3 02 14
02 02 0e 04 0e a4 83 02 18 a7 55 7b 40 01 05 73 2e 74 78 74 48 65 6c 6c
6f 2c 20 77 6f 72 6c 64 21 0a c7 15 89 28 15 02 02 00 04 00 a4 83 02 00
00 00 00 c0 03 01 05 7a 2e 62 69 6e 3d ba 9d 7c 15 02 02 00 04 05 a4 83
02 33 33 33 33 80 03 01 05 6e 2e 62 69 6e 1e 8d da ad 15 02 02 05 04 14
a4 83 02 22 22 22 22 c0 03 01 05 63 2e 62 69 6e 01 02 03 04 05 1d 2f 1a
63 15 02 02 00 0c 00 a4 83 02 00 00 00 00 80 03 01 05 75 2e 62 69 6e 19
b2 3a 35 03 05 00 00
```

**E22** (derived from C1, no hexadecimal block): truncation inside
headers.

| Input | Expected |
|---|---|
| the first 8 bytes of C1 | not a RAR 5 archive |
| the first 12 bytes of C1 | not a RAR 5 archive (the candidate has no complete block after it) |
| the first 16 bytes of C1 | no members, one diagnostic (no end of archive header), no stop reason |
| the first 22 bytes of C1 | no members; stop at 16, truncated (fewer than 7 bytes left) |
| the first 30 bytes of C1 | no members; stop at 16, truncated (the header would end at 41) |
| the first 55 bytes of C1 followed by `61 62 63` | `a.txt` as in C1; stop at 55, truncated |

### 16.4 BLAKE2sp check values

"Pattern n" is the n bytes whose i-th byte (from 0) is (7 × i + 3)
mod 256. All values were produced by a throw-away implementation of
section 10.2 and agree with an independent BLAKE2s tree-mode
implementation (Python's standard library configured as the tree of
10.2); the values marked real are also the ones stored by `rar`.

| Input | Length | BLAKE2sp | CRC-32 |
|---|--:|---|---|
| empty (real) | 0 | `dd0e891776933f43c7d032b08a917e25741f8aa9a12c12e1cac8801500f2ca4f` | `0x00000000` |
| `abc` | 3 | `70f75b58f1fecab821db43c88ad84edde5a52600616cd22517b7bb14d440a7d5` | `0x352441C2` |
| H14 | 14 | `b7171e34094c38df9434e344d9f079ab091cf51c48e3764bc20e9ca08c54ec9f` | `0x7B55A718` |
| `a.txt` of G4 (real) | 46 | `cb995f7bc2c4a1f91f57555e6c1aa0da3393926e6e61885dfdff5c5667ad7f8e` | `0x065E149C` |
| pattern 64 | 64 | `e375bba69fb1c84c7f202f06e71090695f81786b7413da0cd0126e15a0f80181` | `0xCBD9ECF0` |
| pattern 65 | 65 | `11a6af420901450dd27b0babec0c484c4f7710330040136a17e79df6c54106ae` | `0x6D195777` |
| pattern 512 | 512 | `201135254f94f0cf5307da6ae28daf819258fd08df8249684ffbbded6c23e0e5` | `0x0F498B0E` |
| pattern 513 | 513 | `4524c7cfa3a7cde180691ba1a3c0d4b9fc20ced214f27e65f5c76dbb0ff44c91` | `0xACBCDABB` |
| pattern 1000 | 1000 | `f970123bbbae325434ca917f79d3724ce95d1074f8ce94c9038a70ab68dd419d` | `0x17BC2A46` |
| pattern 4097 | 4097 | `bbb0935c0ad88571fece336d8d4a3782c795821491ba9d86392236920288c8dd` | `0xCB3097E5` |

Pattern 64 exercises a leaf whose only block is full; 512 and 513 the
case where every leaf's last block is full, and a second block in leaf 0
only; 1000 and 4097 several blocks per leaf.

## 17. Safety requirements for untrusted input

1. **Declared sizes never drive allocation.** U, S, D, P, name lengths,
   record sizes and the extra area size are claims. The only buffers a
   container reader needs are one header (at most 2,097,151 bytes, and
   only after checking that the volume holds that many) and constant
   BLAKE2sp state. Stored contents are streamed. The member list grows
   with the number of blocks actually present.
2. **Every header field is read inside its region** (3.2, 5.2, 5.3,
   5.5): the header inside the volume, the type-specific fields before
   the extra area, each record inside the extra area, each record's
   fields inside the record. Lengths are compared with the bytes that
   remain, never added to a position first.
3. **64-bit arithmetic that cannot wrap.** A data area end, a record
   end, the sum of piece lengths and FILETIME conversion are computed
   with overflow checks or in a wider type. S = 2^64 − 1 must end as
   "truncated", not as a small offset.
4. **Header CRC before trust.** No field of a header is acted upon
   before its CRC-32 has been verified (the size field itself excepted).
5. **Bounded search.** The signature search covers offsets 0..1,048,576
   and validates at most 64 candidates, so it costs at most about
   64 × 2 MiB of checksumming.
6. **Progress.** Each block advances by at least 7 bytes and each extra
   record by at least 2; no loop in this document can run without
   consuming input.
7. **No backward or out-of-band reads.** Locator offsets and quick open
   data are never followed (6.1, 8.1); nothing after the end of archive
   header is read (6.7).
8. **Encrypted data never reach the decoder** (13), including when the
   encryption record is damaged (7.2).
9. **Solid state is used only as section 12 allows.** A failed decoder
   member poisons its group; state is never reused after a failure and
   never carried into a member whose solid bit is clear.
10. **D is passed to the decoder, not allocated by the container.** The
    decoder's own document says how the window is sized; whatever it
    does must be bounded by the implementation's limit and must not
    reserve D bytes merely because a 30-byte header asks for 1 TB.
11. **Output limits.** The caller's limits on the size of one member and
    of all members apply to stored and decoded contents alike, and are
    the only bound when U is unknown (9.4). Contents delivered through a
    hard link or file copy (9.1) count again for every link: a small
    archive can name one large member thousands of times.
12. **Checksums.** Every delivered regular file is verified against each
    checksum its header carries (10.1), piece checksums included.
13. **Names and links.** The container delivers names and link targets
    as data. The caller must ensure that no member is written outside
    the extraction root: reject or neutralise absolute names, `..`
    components, drive letters and (for Windows names) backslashes;
    never write through a symbolic link created from the same archive;
    resolve hard link and file copy targets only to members of the
    archive (9.1), never to paths on the file system.
14. **Owner, attributes and times** are data; applying them is the
    caller's choice. Set-user-id and similar mode bits must not be
    applied by default.
15. **Volume sets.** The number of volumes read is bounded by what the
    caller supplies; a member has at most one piece per volume.
16. **Work per member is bounded.** Finding the target of a hard link or
    file copy (9.1) must not rescan the member list for every link (a
    hostile archive can hold as many links as it has blocks); keep an
    index by name. Delivering one member of a solid group costs the
    decoding of every decoder member before it in the group (12.2); a
    caller that extracts members one by one in arbitrary order must
    either decode each group once, in order, or account for the
    repeated work in its limits.
17. **Diagnostics are bounded.** A hostile archive can provoke one
    diagnostic per extra record or block. Keep a count and a bounded
    number of messages, not one stored message per event.
18. **Names.** A name can be as long as a header (2 MiB) and every block
    can carry one. An implementation may fail, as unsupported, a member
    whose name is longer than its own path limit, provided the limit is
    at least 4,096 bytes; it must not truncate the name silently.

## 18. Observed implementation gaps

Faults and leniencies of the reference implementation (`unar`/`lsar`
1.10.7), found by reading it and confirmed with the vectors named.
Nothing here is to be imitated.

1. **The header CRC is never checked.** A block with a wrong CRC is
   used as if valid (E1).
2. **vints have no length limit.** Any number of continuation bytes is
   accepted (a field with 31 of them was read as 14), with shift counts
   beyond the width of the result.
3. **Fields are read without regard to the end of the header or of the
   extra record.** A name length larger than the header takes the bytes
   that follow it, data area included, as the name (E6); a time record
   that is too short takes its time from the bytes after it; a header
   size of 2^50 is accepted; and a redirection record whose target
   length exceeds what is left of the file makes the whole listing fail
   (E18: "attempted to read more data than was available").
4. **The size field's length and a size of zero are not checked** (E2,
   E7).
5. **Signature search.** Recognition examines only the first 512 KiB
   (the last offset found is 524,279); the first match is taken
   unverified (C9); and in every volume after the first the signature
   is assumed to be at offset 0 and 8 bytes are skipped blindly.
6. **No main header is required** (E13 is read), repeated ones are
   accepted (E14), and the main header's flags and volume number are
   never read.
7. **Dictionary size.** Only 4 bits of N are used and F is ignored: the
   real version 1 archive with D = 4,966,055,936 is described as
   4,294,967,296, and N = 19 as 1,048,576.
8. **Solid groups, empty members.** Members with U = 0 are left out of
   the group, so the stream of an empty file that starts a group is
   never decoded and the member after it fails. Genuine archive G2:
   `unar` reports `c.txt` as failed ("Unknown error") and leaves it
   empty.
9. **Solid groups, stored members.** A stored file inside a group is
   added to the list of the group's compressed streams. Genuine archive
   G3: `unar` reports `b.txt` as failed ("Error on decrunching") and
   leaves it empty.
10. **BLAKE2sp is not verified.** A file protected only by BLAKE2sp is
    reported "unknown" (G4) and a wrong hash is not noticed (E12).
11. **Unix owner.** The numeric group id is stored as the user id (C3
    lists user number 100).
12. **Nanoseconds** of Unix times are dropped.
13. **Incomplete files are lost or fatal.** A file still open when the
    set ends is silently omitted from the listing; a missing volume
    makes the whole listing fail (E17c).
14. **Stored size mismatch** is not detected when U is larger than the
    data (E10, `a.txt` is delivered as 14 bytes), and an unknown-size
    stored file fails with a checksum error (C5).
15. **An unknown encryption version aborts the whole archive** instead
    of failing one member.
16. **All redirection types are treated as symbolic links**, including
    file copies and unknown types (C4).
17. **Names.** A zero byte inside a name is replaced, and an empty name
    becomes `.`.
18. **A checksum mismatch is reported but the output file is kept.**
19. **Repeated extra records: the last one wins**, silently (C13 lists
    the second time record).
20. **Pieces are joined on the continuation flag and the name alone**,
    wherever the continuing header stands in its volume, and without
    comparing compression information (E17f is read as a good file).
21. **Members of unknown size have no size at all**: they are treated as
    empty and left out of the solid group, so nothing is decoded for
    them.

Notes on the other black-box readers, for whoever compares results:
`7zz` 26.01 rejects unknown block types (C12) and unknown bytes before
the extra area (C11), drops the excess bits of an over-long 10-byte vint
(E4), tries to allocate whatever dictionary a header declares (E16), and
in the build used here cannot decompress RAR 5 data at all (it lists
archives and extracts stored members only). `bsdtar` 3.5.3 rejects
unknown block types, archives with a metadata record (G5, C2, and the
genuine archives that have one), files whose U is a 10-byte vint (C10)
and unknown-size files; accepts a 4-byte header size field (E2); does
not verify BLAKE2sp; tries to decode a member of unknown version (E16);
and lets a solid member continue across a stored or empty member whose
solid bit is clear (12.3).

## 19. Decisions

| # | Section | Decision |
|--:|---|---|
| K1 | 3.2 | A vint has at most 10 bytes and may not carry bits above bit 63; otherwise the header is malformed. Padded encodings are valid. |
| K2 | 4 | The signature is searched at offsets 0..1,048,576; a candidate must be followed by a valid block of type 1 or 4; at most 64 candidates. The same in every volume. |
| K3 | 4 | Only the 8-byte signature ending `01 00` is this format. |
| K4 | 5.2 | A header size field longer than 3 bytes, or H less than 2, is a malformed header. |
| K5 | 5.2 | A header CRC mismatch stops reading; no resynchronisation. Members before it are kept. |
| K6 | 5.3 | Bytes between the known fields and the extra area are ignored. |
| K7 | 5.3 | Unknown block types are skipped with a diagnostic. |
| K8 | 5.3 | A main or encryption header anywhere but first is a malformed header. |
| K9 | 5.4 | A file header whose data area is cut off yields a failed (truncated) member, then reading stops. |
| K10 | 5.5 | A broken extra area is ignored from the first bad record on; a known record that is too short is ignored; the first of duplicate records wins (for hash records, the first of hash type 0). |
| K11 | 6.1, 8.1 | The locator and quick open data are never used. |
| K12 | 6.3 | Unsupported: version above 1, method 6 or 7, N above 15 in version 0, N above 23 in version 1, or D above the implementation's limit. |
| K13 | 6.4 | An unknown host OS keeps the member with unknown attributes. |
| K14 | 6.5 | Names: no reverse private-use mapping; invalid UTF-8 tolerated; cut at the first zero byte; an empty name is a malformed header. |
| K15 | 6.7 | A missing end of archive header is a diagnostic. |
| K16 | 7.2 | Any record of type 1 marks the member encrypted. |
| K17 | 7.4 | Nanosecond fields of 10^9 or more are ignored; a time record overrides the header mtime. |
| K18 | 7.5 | A versioned member is named `name;n`. |
| K19 | 8 | Service headers are skipped; the comment may be exposed under the stated conditions. |
| K20 | 7.6, 9.1 | Hard links and file copies are delivered as links to the nearest earlier regular file of that stored name (never a versioned member); a missing, mismatching or failed target fails the link; unknown redirections are unsupported; an empty target is allowed for symbolic links and junctions only. |
| K21 | 9.2 | A stored member needs U = P. |
| K22 | 9.4 | A stored member of unknown size has U = P. |
| K23 | 10.1 | Both CRC-32 and BLAKE2sp are verified when present, and the checksums of pieces too. |
| K24 | 11.3 | The last piece's header describes a joined member; pieces whose compression information or directory flag differ make it corrupt. |
| K25 | 11.4 | A volume number that does not match is a diagnostic only. |
| K26 | 12.2 | A member with the solid bit clear, other than a directory, ends the previous group whatever its kind; a solid member with no live state fails; a failed decoder member poisons its group; a checksum mismatch alone does not. |
| K27 | 13 | Encrypted archives and members are reported and skipped, never deciphered. |
| K28 | 4 | A signature candidate is judged by its first block's framing only (size field, H, bounds, CRC, extra size, type 1 or 4), not by that block's type-specific fields. |
| K29 | 11.2 | A continuation is joined only if it is the first file header of its volume. |
| K30 | 17 | Link contents count against output limits per link; a name longer than the implementation's path limit (at least 4,096 bytes) may fail its member as unsupported. |

None of them changes how any genuine archive in the survey is read.

## 20. Open questions

They do not block an implementation; the document says what to do in
each case.

1. **`ACL` and `STM` service headers, Windows symbolic links and
   junctions** were not observed (no archive written with those Windows
   features was available). They are skipped or delivered as described;
   the syntax of their targets and data is unknown.
2. **The unknown-size flag on a last piece**, stored or compressed, was
   not observed in a genuine archive; K22 is a guess on which the
   black-box readers disagree.
3. **The private-use mapping of undecodable Unix names** (6.5) was not
   observed.
4. **Version 1** was seen in one genuine archive; **bit 20** of the
   compression information in none.
5. **Whether RAR itself accepts unknown bytes before the extra area**
   (K6) is unknown; `7zz` does not.
6. **A solid member directly after a stored, empty or link member whose
   solid bit is clear** (K26): `rar` does not write this; `bsdtar` and
   the reference implementation treat it differently.
7. **A member with both CRC-32 and BLAKE2sp** was not seen in a genuine
   archive.
8. **Volume names:** the first volume of a self-extracting set was
   observed only with the extension `sfx` (written on macOS), not `exe`;
   no naming other than `.partN.` was observed for this format, and
   `rar` 7.21 does not write the older one.
9. **Coordination with `rar5-compression.md`:** that document must
   accept a member with U = 0 and P greater than 0 (2.3, 12.3) and a
   member of unknown size (9.4), and must define the window policy for
   D (17).
10. **Deciphering** (key derivation, AES, the keyed transformation of
    checksums, the layout of enciphered headers beyond 6.8) is not
    specified.
11. **The technical note could not be fetched a second time**: both the
    writing and the verification pass worked from one saved copy
    (SHA-256 `37fe890e2c02c4d3bc6f5e8c617f368175ebbb924e0608c52a99b986cea4dc96`),
    because later requests to the vendor's site timed out. One
    comparison with the live page is still owed.
12. **Repeated extra records** (K10) and **a continuation that is not
    the first file header of its volume** (K29) do not occur in genuine
    archives; the black-box readers disagree on both, and the decisions
    here take the stricter reading.

## 21. Evidence

**Technical note.** RARLAB, "RAR 5.0 archive format", as published at
`https://www.rarlab.com/technote.htm`, read in full (copy retrieved on
2026-11-13). The copy used describes compression algorithm version 1 and its dictionary fields
(RAR 7.0) and the metadata record. Every structure in it is covered in
sections 3 to 8.

**Reference implementation.** XADMaster, revision `7cb9ee0a`: the RAR 5
archive parser (block walk, file header and extra record parsing, the
joining of pieces, the construction of solid streams, volume discovery).
Kept outside this repository.

**Independent re-statement.** Everything in sections 3 to 13, including
BLAKE2sp, was re-implemented as a throw-away reader (kept outside the
repository, not part of any deliverable) directly from the rules as
worded here. It reads every archive named below and produces the results
stated in section 16.

**Archives written by the vendor's `rar` 7.21** (official command-line
distribution for macOS, used only to create archives): about 60 archives
and volume sets, among them: stored and compressed files; empty files;
directories and nested paths with non-ASCII names; archive comment;
quick open on and off; recovery record with two percentages; locked
archive; metadata record; file versions; symbolic links, hard links and
file copies; owner records; modification, creation and access times;
BLAKE2sp instead of CRC-32, stored and compressed, single and
multi-volume; data from standard input, single and multi-volume
(unknown-size flag on the earlier pieces); password-protected files, a
password-protected solid archive, and password-protected headers, single
and multi-volume; a self-extracting archive; dictionary sizes from
128 KiB to a requested 64 GiB and one version 1 archive with a
4.6 GiB dictionary; solid archives of several shapes (whole archive,
groups by count, groups by extension, with stored members, with empty
members in the middle and at the start of a group, with links and
directories); multi-volume sets of 3 to 34 volumes, stored, compressed,
solid, with recovery records.

Together with the two Windows archives below these are 119 volume
files with 1,052 blocks: 733 file headers, 87 service headers (`QO` 76,
`RR` 8, `CMT` 3), 6 encryption headers. Measured on all of them:

- Every header CRC verifies under section 5.1, and every header size
  field is one byte long.
- Every stored member's contents match its CRC-32 or BLAKE2sp; every
  piece checksum of every split member matches the piece's packed bytes;
  the last piece's checksum matches the whole file. For compressed
  members the unpacked contents were obtained with `unar` or `bsdtar`
  and compared with the original files and with the stored BLAKE2sp
  values.
- No block has bytes between its known fields and its extra area; no
  vint is longer than 9 bytes; 438 vints are longer than their value
  needs.
- All 51 volumes that are not the last of their set end with zero bytes
  after the end header; no last volume has anything after it.
- All 483 quick open structures parse as in 8.1, and the data of each
  equal the header it points back to.
- Extra records seen: locator 107, metadata 2 (main header); time 710,
  hash 11, redirection 10, encryption 5, version 1, owner 1 (file
  headers); service data 8 (recovery record headers).
- The solid findings of 12.3, including the two reference failures on
  genuine archives.

**Real archives written by WinRAR on Windows** (two, found on the
author's machine; 95 and 415 entries, 3.6 and 8.7 MB): host OS 0,
attributes `0x20` and `0x10`, FILETIME modification times, 51 stored and
457 compressed members, directory headers with and without a CRC-32
field, quick open blocks of 5.6 and 17.3 KB. The throw-away reader lists
both identically to `lsar` and `7zz`; `lsar` tests every member
successfully.

**Independent verification.** A second member of the specification
team, before reading the reference implementation, wrote a second
throw-away reader (including BLAKE2sp) from this document alone and ran
it on every vector of section 16 (each hexadecimal block was taken from
this document's text), on all the archives above, and on the two
Windows archives. It gave the stated result for every vector and the
same members, sizes and CRC-32 values as `7zz` for every archive (57
archives and sets; one differs only in how the tools name a versioned
member), with every piece checksum, stored-file checksum and
quick open structure verifying. `bsdtar` and `unar` were re-run on G1,
G2 and G3 (results as stated in 16.1), and on G3 with the stored
member's block removed (both extract `a.txt` and `b.txt` correctly,
which confirms that the stored member is not part of the dictionary).
The locator offsets of two archives were recomputed by hand. Further
archives were then written with `rar` 7.21: a solid archive that begins
with a symbolic link and an empty file and holds a file copy and a hard
link between compressed files (12.3); a three-volume self-extracting
set and a set with recovery volumes (11.5); a set made with the
old-naming option. Vectors C13, C14, E17f, E17g, E21 and E22 were added
in this pass and given to the three tools.

**Crafted archives given to the black-box readers** (about 150): every
vector of section 16; each header flag; padded and over-long vints;
size fields of 1 to 4 bytes; wrong header and data checksums; truncation
at every kind of boundary; unknown block types and extra record types;
oversized extra areas, names, record sizes and link targets; all
dictionary encodings of 15.3; signature offsets around 524,280 and
1,048,576; every redirection type; hash records that are wrong, short or
of unknown type; time records in both forms; hostile names; volume sets
with wrong piece checksums, differing names, missing flags, missing
volumes, wrong volume numbers and padding; solid archives rearranged
block by block (members removed, reordered, inserted, solid bits
changed).

## Clean-room feedback

Notes from the implementer of `src/formats/dos/rar_native/container5.rs`,
written from this document alone. Every vector of section 16 (and C1 of
15.1) gives the stated result, including the BLAKE2sp values of 15.7 and
16.4.

### Points where the document leaves room, and what was chosen

1. **Fields of the archive encryption header (6.8).** 13.1 says to stop
   with "encrypted headers" when the first block has type 4; 5.3 says a
   type-specific field that does not fit makes the header malformed. The
   implementation does not parse the fields of that block at all and
   always reports encrypted headers, since either way no member is
   delivered.
2. **A later volume that is not a RAR 5 archive** (no accepted signature
   candidate). The stop reasons of 2.2 have no case for it; it is
   reported as missing volume, at that volume's index. A later volume
   whose first block has type 4 is reported as encrypted headers, and
   the members already found are kept (13.1 speaks of an empty list,
   which is read as applying to the first volume).
3. **Volumes supplied beyond the end of the set** (the end header or the
   main header says no volume follows, 11.4) are not read; one
   diagnostic is recorded.
4. **Service headers (8).** Their fields are parsed with the rules of
   5.3, so a service header whose fields do not fit its region stops
   reading as malformed; but an empty service name is accepted (6.5's
   empty-name rule is applied to file headers only). The extra records
   of service headers other than the first `CMT` are not looked at and
   raise no diagnostics. A `CMT` header carrying either volume flag is
   not used as the comment.
5. **Order of the 11.3 mismatch among the failures.** A member whose
   pieces disagree on compression information or directory flag is
   failed corrupt only if rule 4 of 12.2 (unsupported, encrypted,
   incomplete, truncated) did not fail it first. If such a member would
   have been a decoder member, the group state becomes poisoned, as for
   the corrupt case of 9.3; the document does not say so explicitly.
   The compression information reported for a joined member is that of
   its first piece.
6. **Directories and links that are incomplete, truncated or
   inconsistent** are reported failed with that reason. An encryption
   record on a directory or link does not fail it (it has no contents);
   the encrypted mark is still reported. Compression information is
   never a reason to fail a link (12.2 rule 3 passes links over before
   rule 4), only an unknown redirection type is.
7. **Hard links and file copies whose size cannot be compared.** If the
   link's header has the unknown-size flag, or the target is a
   compressed member of unknown size, the link is failed corrupt (9.1
   only speaks of sizes that differ). A stored target of unknown size
   counts with U = P (9.4).
8. **Failures known only after decoding.** The listing cannot know that
   a decoder member will fail to decode or that a target of a hard link
   will fail its checksum. The listing therefore names, for each decoder
   member, its group and the member it continues from, and for each link
   its source member; applying "poisoned group" and "failed with the
   same reason" to later members after a run-time failure is left to the
   caller, and the code says so.
9. **Duplicate hash records.** A hash record too short to hold its hash
   type vint raises the too-short diagnostic and does not count as the
   first type 0 record. A type 0 record with fewer than 32 hash bytes
   does count as the first (5.5), so a later complete one is skipped.
10. **Diagnostics for the extra area of the main header** follow 5.5
    like those of file headers (the document states the rules generally
    but the vectors only exercise file headers). Main header records are
    read from the first volume only.
11. **Locator offsets of 0** are reported as absent ("not known").
12. **Next volume name (11.5).** The helper returns one name: the last
    run of digits before the extension counted up by one with the same
    width, and the extension `rar` (kept as written when the given name
    already ends in `rar` in any case, replaced when it is `exe` or
    `sfx`). When the run cannot count up without growing (`part9`,
    `part99`) there is no next name. The helper cannot tell a volume
    name from an ordinary name that happens to hold digits; the caller
    asks for a next name only when the set says another volume follows.
13. **Name length limit (17.18)** is not applied here; names are
    delivered whole and the caller's path rules decide.
14. **The signature search and short files.** With the 15-byte minimum
    and the 7-byte minimum block, a volume of exactly 16 bytes holding
    the signature and an 8-byte main header block is accepted (E22
    confirms this case).

### Points that were clear only after a second reading

- 5.2 step 8 and 5.4 together: the type-specific fields are parsed
  before the data area is tested against the volume, for every block
  type; only then does a file header yield its truncated member.
- 10.1: a member closed as incomplete keeps the checksum of its last
  present piece as a piece checksum and has no checksum of its
  contents; a member that begins with a continuation and ends in the
  volumes read carries the whole-file checksum, which cannot be
  verified because it is failed anyway.
- 11.4: the expected volume number counts up from the first volume
  read, not from the number the previous volume claimed (E17e gives one
  diagnostic only under this reading).

### Black-box results

Three genuine RAR 5 archives found on this machine (one of 18,757 bytes
with a single compressed member; the two WinRAR archives of 3.6 and
8.7 MB mentioned in section 21, with 95 and 415 entries) were listed
with this implementation and with `7zz l -slt`: every name, directory
flag, unpacked size, packed size and CRC-32 agrees, there is no stop
reason and no diagnostic, and all 51 stored members of the larger
archive pass their CRC-32 when read back. `lsar` recognises all three
as RAR 5. No compressed member was decoded (that is the decoder's
module).

### Notes from the black-box comparison

- **9.1, symbolic links and junctions.** The first version of the
  caller dropped them without a word, because the library it serves
  has no entry kind for a link. That made a member disappear and
  disagreed with the RAR 1.50 to 4.x reader, where a link is a file
  whose contents are its target (`rar4-container.md` 6.5). The caller
  now yields a symbolic link or junction as a file whose contents are
  the target bytes of the redirection record (after the zero-byte cut
  of 7.6, otherwise as stored; the junction of C4 gives the 8 bytes
  `\??\C:\x`), also when the directory flag is set. A target of
  more than 4,096 bytes is not delivered and is reported, as decision
  D11 of the other document does; this document sets no limit, and a
  sentence in 7.6 or 17 would settle it. Nothing is ever followed.
  G6 now yields four entries: `sym.lnk` holds `h.txt`.
- **11.5, self-extracting sets.** `name.part1.sfx` and
  `name.part1.exe` now have `name.part2.rar` as their next name (the
  REFERENCE-ONLY rule; item 12 above already allowed for it but the
  caller never asked). See item 22 of the feedback in
  `rar4-container.md` for the reverse direction.
- **5.2 and 11.4 seen from outside.** The comparison counted as losses
  twelve crafted archives whose file header fails its checksum (E2,
  E6, E14, E20 and relatives) and two crafted sets whose first volume
  says that no volume follows (11.4) while a file is still open: the
  reference tool, or the reader this one replaces, delivers entries
  there. Both rules were kept as specified. No genuine archive of the
  corpus is affected.

