# LHA / LZH archive container

Format specification for clean-room implementation. This document describes
the archive structure of LHA archives (`.lha`, `.lzh`, and the LArc
predecessor `.lzs`): how an archive is recognised, how the per-entry headers
of levels 0, 1, 2 and 3 are laid out, how names, paths, timestamps and sizes
are obtained from them, how entries follow each other, and how the stored
checksums are verified. The compression methods are specified in
[`methods.md`](methods.md). [`README.md`](README.md) is the index of the
document set and holds the glossary and the numbered list of decisions.

## 1. Conventions

- **Offsets** are byte offsets counted from 0. "Header offset" means an offset
  from the first byte of one entry header. "Archive offset" means an offset
  from the first byte of the archive (after any leading junk has been
  removed, section 9).
- **Multi-byte integers are little-endian** everywhere in this format: the
  first byte is the least significant. There is no big-endian field.
- Sizes of fields are given in bytes. "u8", "u16", "u32", "u64" mean unsigned
  integers of 1, 2, 4 and 8 bytes.
- Hexadecimal byte values are written as two digits in code style, for
  example `2D`. Four-digit values in code style such as `4953` are 16-bit
  values in hexadecimal. ASCII text stored in the archive is written in
  quotes, for example "-lh5-".
- **L** is the number of bytes of the archive actually available. Every
  length and offset read from the archive is untrusted until it has been
  checked against L.
- **P** is the packed length of an entry (the number of bytes its data
  occupies in the archive), **U** its unpacked (original) length.
- Words such as "must" and "must not" state requirements on the new
  implementation.

### 1.1 Evidence labels

Every statement carries, explicitly or through the "Evidence" paragraph of
its section, one of these labels:

- **CONFIRMED (real data)**: checked on a genuine archive. Only one genuine
  archive was available for this work (about 250 KB; one level-1 header
  written on Unix with three extended headers; one "-lh5-" entry of about
  550 KB in some twenty blocks). A throwaway reader that follows this document and `methods.md`
  literally parsed it and reproduced the stored CRC-16. Statements about the
  level-1 layout, the extended-header chain, the "skip size" rule, the
  CRC-16 and the end marker rest on it in addition to crafted tests.
- **CONFIRMED (crafted)**: checked by building archives byte by byte from
  this description and giving them to independent readers used purely as
  black boxes. The black boxes are:
  - the **reference implementation**: the third-party LHA library, version
    0.6.2, which the implementation written from this document replaces;
  - **`unar`**: the `unar`/`lsar` command-line tools, version 1.10.7;
  - **`7z`**: p7zip 17.05 (it reads header levels 0 to 2 only and decodes
    only "-lh0-", "-lz4-" and "-lh4-" to "-lh7-").
  Unless a section says otherwise, all black boxes that support the feature
  agreed.
- **PUBLISHED**: taken from public documentation or from the published
  source of other LHA tools (listed in `README.md` section 7), not
  separately observable with the black boxes.
- **INFERRED**: deduced, or supported by a single source.
- **DECISION n**: a choice made by this specification where sources
  disagree or behaviour on malformed input must be fixed. The number refers
  to the list in `README.md` section 5, which gives the reason.

### 1.2 Outcomes a reader reports

- **Not an LHA archive**: detection failed (section 9). Nothing is listed.
- **Entry delivered**: the entry's header was valid, its method is
  supported, decoding produced exactly U bytes and the CRC-16 matched (or no
  CRC is stored, section 3.2).
- **Entry error**: the header was valid but the contents cannot be
  delivered. The kinds are **unsupported method**, **corrupt data** (the
  decoder rejected the stream or the CRC-16 differs), **truncated** (the
  archive ends inside the entry's data) and **output limit exceeded**. An
  entry error never stops the reading of later entries unless the archive
  itself ends (section 8).
- **Finding**: a non-fatal irregularity worth recording but which does not
  change the result, for example trailing bytes after the last entry.

Nothing is delivered as an entry's contents unless every step succeeded.

## 2. Overall structure

An archive is a sequence of **entries** with nothing between them:

| Part | Size | Notes |
|---|---|---|
| entry header | variable | One of four layouts, selected by the level byte at header offset 20. |
| entry data | P bytes | The packed contents. P comes from the header. May be 0. |
| ... | | More entries, each a header followed by its data. |
| end marker | 1 byte, optional | A single `00` byte. |

There is no archive-wide header, no central directory and no archive-wide
checksum. Each header describes exactly one entry: a file, a directory, or
(Unix) a symbolic link. Entries are independent: no compression state is
carried from one entry to the next.

Headers of different levels may be mixed inside one archive (CONFIRMED
(crafted): a level-2 entry followed by a level-1 entry is read by all black
boxes).

The first seven bytes of every header, at every level, have the same shape:
two bytes whose meaning depends on the level, then the five-byte **method
identifier** at header offsets 2 to 6, which always begins and ends with
`2D` ("-"). The level byte is always at header offset 20. A reader therefore
reads at least 21 bytes, looks at offset 20, and continues according to the
level.

## 3. Fields common to all levels

### 3.1 The first 21 bytes

| Header offset | Size | Field | Notes |
|--:|--:|---|---|
| 0 | 2 | level-dependent | Levels 0 and 1: header size (u8) and header checksum (u8). Level 2: total header size (u16). Level 3: word size (u16), always 4. |
| 2 | 5 | method identifier | Five ASCII bytes, for example "-lh5-". Section 3.3. |
| 7 | 4 | packed size field (u32) | Levels 0, 2, 3: P. Level 1: the **skip size**, P plus the total size of all extended headers (section 5.3). |
| 11 | 4 | original size (u32) | U. May be replaced by a 64-bit value (section 6.6). |
| 15 | 4 | timestamp (u32) | Levels 0 and 1: MS-DOS date and time. Levels 2 and 3: seconds since 1970-01-01 00:00:00 UTC. Section 7. |
| 19 | 1 | attribute (u8) | MS-DOS file attribute bits (section 3.4). Writers of levels 1 to 3 normally store `20`. |
| 20 | 1 | level (u8) | 0, 1, 2 or 3. Any other value: not a valid header. |

Evidence: CONFIRMED (crafted) for all four levels; CONFIRMED (real data) for
level 1.

### 3.2 Data CRC-16

Every header (with one exception, below) stores a 16-bit checksum of the
entry's **unpacked** bytes. Its position depends on the level. The algorithm
is the same one used for the header CRC of section 6.1:

| Property | Value |
|---|---|
| Width | 16 bits |
| Polynomial | x^16 + x^15 + x^2 + 1, written `8005` in the normal (most-significant-bit-first) form |
| Bit order | reflected: each data byte is processed least significant bit first. The reflected form of the polynomial is `A001` |
| Initial value | `0000` |
| Final XOR | none |
| Stored as | u16, little-endian |
| Check value | the nine ASCII bytes "123456789" give `BB3D` |
| Other test values | no bytes give `0000`; the single byte `61` gives `E8C1`; the twelve bytes "Hello, LHA!" followed by `0A` give `4953` |

Described without a table: keep a 16-bit register, initially zero. For each
data byte, XOR the byte into the low 8 bits of the register; then eight
times: if the lowest bit of the register is 1, shift the register right by
one bit and XOR it with `A001`, otherwise only shift it right by one bit.
After the last byte the register is the checksum. (This is the checksum
widely catalogued as "CRC-16/ARC".)

The reader computes the CRC-16 of the U bytes it decoded and compares it
with the stored value. A difference is an entry error of kind corrupt data.

Rules for special cases:

- **Directories and symbolic links** carry no data; their CRC field is
  normally `0000` and is not checked.
- **U = 0** for a file: no decoding takes place; the output is empty and its
  CRC-16 is `0000`. The stored value is compared like any other
  (**DECISION 21**).
- **Level-0 headers without a CRC field** (section 4.2): nothing is
  verified.

Evidence: polynomial, reflection, initial value and check value CONFIRMED
(crafted) and CONFIRMED (real data). A wrong stored value is reported as a
checksum failure by all three black boxes.

### 3.3 Method identifier

Five bytes at header offsets 2 to 6. Byte 2 and byte 6 are `2D` ("-") in
every known archive; the three bytes between them name the method.
Comparison is exact and case-sensitive: "-lZ5-" is not "-lz5-".

| Identifier | Meaning | Handling |
|---|---|---|
| "-lh0-" | stored | contents are the packed bytes (`methods.md` section 3) |
| "-lh1-" | 4 KiB window, adaptive Huffman | `methods.md` section 5 |
| "-lh4-" | static Huffman, nominal 4 KiB window | `methods.md` section 4 |
| "-lh5-" | static Huffman, 8 KiB window | `methods.md` section 4 |
| "-lh6-" | static Huffman, nominal 32 KiB window | `methods.md` section 4 |
| "-lh7-" | static Huffman, 64 KiB window | `methods.md` section 4; but see the exception below |
| "-lhd-" | directory entry (also symbolic link) | no data; section 6.8 |
| "-lz4-" | stored (LArc) | as "-lh0-" |
| "-lzs-" | LArc, 2 KiB window | `methods.md` section 6 |
| "-lz5-" | LArc, 4 KiB window | `methods.md` section 7 |
| "-lh2-", "-lh3-" | early experimental LHA methods | entry error: unsupported method |
| "-lhx-" | large-window extension of one Windows library | unsupported method |
| "-pm0-", "-pm1-", "-pm2-", "-pms-" | PMarc methods | unsupported method (**DECISION 19**) |
| anything else | unknown | unsupported method |

Requirements:

- An unsupported or unknown identifier is **not** a header error. The header
  is parsed completely, the entry is listed with its name, sizes and
  timestamp, its contents are reported as an entry error of kind unsupported
  method, its P data bytes are skipped, and reading continues with the next
  entry. CONFIRMED (crafted): the reference implementation and `7z` do
  exactly this for "-lh2-", "-lh3-", "-lhx-", "-lh9-" and "-lZ5-"; `unar`
  refuses the whole archive when the *first* identifier is one it does not
  know, which must not be imitated.
- For a header to be valid at all, bytes 2 and 6 must be `2D`
  (**DECISION 2**). The three middle bytes are unrestricted.
- **Exception for one "-lh7-" variant.** A DOS tool called LHARK wrote
  entries labelled "-lh7-" with a different, incompatible bit stream. They
  are recognised by the combination: level 1, OS identifier `20` (a space),
  identifier "-lh7-". Such an entry is reported as unsupported method
  (**DECISION 20**; PUBLISHED). Decoding it as ordinary "-lh7-" would only
  produce a checksum failure.

### 3.4 Attribute byte

Header offset 19. The bits are those of an MS-DOS directory entry:

| Bit value | Meaning |
|---|---|
| `01` | read-only |
| `02` | hidden |
| `04` | system |
| `08` | volume label |
| `10` | directory |
| `20` | archive (modified since last backup) |

The byte is informational. This specification does not use it to decide
whether an entry is a directory (section 6.8). A 16-bit replacement can
appear in extended header `40` (section 6.5).

### 3.5 OS identifier

One byte, present in level 1, 2 and 3 headers and, optionally, as the first
byte of the level-0 extended area (section 4.3). It names the system the
archive was written on.

| Byte | ASCII | System |
|---|---|---|
| `00` | | generic, unspecified |
| `20` | space | written by LHARK (section 3.3) |
| `32` | "2" | OS/2 |
| `33` | "3" | OS/386 (OS-9000) |
| `39` | "9" | OS-9 |
| `41` | "A" | Amiga |
| `43` | "C" | CP/M |
| `46` | "F" | FLEX |
| `48` | "H" | Human68K (Sharp X68000) |
| `4A` | "J" | Java |
| `4B` | "K" | OS-9/68K |
| `4D` | "M" | MS-DOS |
| `52` | "R" | Runser |
| `54` | "T" | FM Towns OS |
| `55` | "U" | Unix |
| `57` | "W" | Windows NT |
| `58` | "X" | OS-9 on X68000 |
| `61` | "a" | Atari ST |
| `6D` | "m" | classic Mac OS |
| `77` | "w" | Windows 95 |

Any byte value is accepted. The identifier influences only three things in
this specification: the LHARK exception (section 3.3), the level-0 Unix
extended area (section 4.3), and nothing else; names are processed the same
way for every identifier (section 6.7).

Evidence: PUBLISHED (the three public sources agree on every value they
list); the values `4D`, `55`, `41`, `4B` CONFIRMED (crafted) through the
system names `lsar` prints.

## 4. Level 0 header

The original LHarc and LArc layout.

### 4.1 Layout

| Header offset | Size | Field |
|--:|--:|---|
| 0 | 1 | header size **H** (u8): the number of header bytes that follow the first two |
| 1 | 1 | header checksum (u8) |
| 2 | 5 | method identifier |
| 7 | 4 | packed size P (u32) |
| 11 | 4 | original size U (u32) |
| 15 | 4 | MS-DOS timestamp (u32) |
| 19 | 1 | attribute |
| 20 | 1 | level, `00` |
| 21 | 1 | name length **N** (u8) |
| 22 | N | path name (section 6.7) |
| 22+N | 2 | data CRC-16 (u16) |
| 24+N | H−22−N | extended area, usually empty (section 4.3) |

The whole header is H+2 bytes long. The entry data starts at header offset
H+2 and is P bytes long. The next header starts at header offset H+2+P.

**Header checksum**: the sum of the H bytes at header offsets 2 to H+1 (all
header bytes except the first two), taken modulo 256. It must equal the byte
at offset 1 (**DECISION 3**: a mismatch means "this is not a valid
header").

### 4.2 Validity rules

- H = 0 is the end marker, not a header (section 8).
- H+2 must be at least 24+N, **or exactly 22+N**. In the second case the
  header ends directly after the name: there is **no CRC field** and no
  extended area. Some old LArc-era writers produced this. The entry is read
  normally and its contents are delivered without a CRC check
  (**DECISION 4**).
- H+2 = 23+N is invalid (half a CRC field).
- The header must lie entirely inside the archive: header start + H + 2 ≤ L.

Evidence: layout and checksum CONFIRMED (crafted). The form without CRC:
`unar` lists and extracts it (then complains about a checksum it took from
bytes that are not a CRC), the reference implementation and `7z` reject the
header; PUBLISHED as a known variant by the LHa for UNIX source.

### 4.3 Extended area of level 0

Bytes from header offset 24+N to the end of the header. When it is not
empty, its **first byte is an OS identifier** (section 3.5). The remaining
bytes depend on that identifier; only one form is interpreted:

**Unix form** (first byte `55` "U" or `4B` "K", and the area is at least 12
bytes long). Offsets are relative to the start of the area; **E** is the
length of the area.

| Area offset | Size | Field |
|--:|--:|---|
| 0 | 1 | OS identifier |
| 1 | 1 | minor version, normally `00` |
| 2 | 4 | last-modified time, seconds since 1970-01-01 00:00:00 UTC (u32) |
| E−6 | 2 | Unix mode bits (u16) |
| E−4 | 2 | user id (u16) |
| E−2 | 2 | group id (u16) |

For the usual 12-byte area, E−6 = 6, so the fields are simply consecutive.
The three last fields are located from the end because OS-9/68K writers
store the timestamp twice (PUBLISHED). The time, when present, takes
precedence over the MS-DOS timestamp at offset 15 (section 7.3). The mode
bits are used only to recognise symbolic links (section 6.8).

Any other content of the extended area is ignored. An area shorter than 12
bytes contributes only its OS identifier.

Evidence: CONFIRMED (crafted) with the reference implementation (it reports
the Unix time for "U" and for "K"); `unar` and `7z` ignore the area and show
the MS-DOS time.

## 5. Level 1 header

The default of LHA 2.x and of most Amiga and Unix tools. It is a level-0
header with three additions: an OS identifier, a chain of extended headers,
and a changed meaning of the packed size field.

### 5.1 Layout of the base header

| Header offset | Size | Field |
|--:|--:|---|
| 0 | 1 | header size **H** (u8): the number of **base header** bytes that follow the first two |
| 1 | 1 | header checksum (u8) |
| 2 | 5 | method identifier |
| 7 | 4 | skip size **S** (u32) |
| 11 | 4 | original size U (u32) |
| 15 | 4 | MS-DOS timestamp (u32) |
| 19 | 1 | attribute, normally `20` |
| 20 | 1 | level, `01` |
| 21 | 1 | name length **N** (u8) |
| 22 | N | file name; may be empty; may also hold a whole path (section 6.7) |
| 22+N | 2 | data CRC-16 (u16) |
| 24+N | 1 | OS identifier |
| 25+N | H−25−N | extended area; ignored (**DECISION 8**) |
| H | 2 | size of the first extended header (u16); 0 if there is none |

The base header is H+2 bytes long; the "next size" field is always its last
two bytes.

**Header checksum**: exactly as for level 0: the sum modulo 256 of the H
bytes at header offsets 2 to H+1. It covers the base header only, not the
extended headers.

### 5.2 Validity rules

- H+2 must be at least 27+N.
- The base header must lie inside the archive.
- The checksum must match (**DECISION 3**).

### 5.3 Extended headers and the skip size

The extended headers follow the base header immediately, starting at header
offset H+2. Each has this shape (the same at level 2):

| Offset in the extended header | Size | Field |
|--:|--:|---|
| 0 | 1 | type (u8) |
| 1 | Z−3 | contents |
| Z−2 | 2 | size of the next extended header (u16); 0 if this is the last |

**Z**, the size of an extended header, is given by the "next size" field
that precedes it (the one in the base header for the first). Z counts the
whole extended header: the type byte, the contents and the trailing next
size field. The chain ends at the first next size field that holds 0. That
terminating field is part of the last extended header (or of the base
header if there are none); no bytes follow it.

Reading procedure:

1. Let *remaining* be the skip size S and *next* the field at header offset
   H.
2. While *next* is not 0: *next* must be at least 3 and at most *remaining*,
   and the *next* bytes must lie inside the archive; otherwise the header is
   invalid. Take the extended header of *next* bytes, subtract *next* from
   *remaining*, and replace *next* by the u16 in its last two bytes.
3. The packed length is **P = *remaining***, that is, S minus the total
   size of all extended headers. The entry data starts directly after the
   last extended header.

The types and their contents are described in section 6.

Evidence: CONFIRMED (real data): the genuine archive has three extended
headers of 5, 7 and 7 bytes; with P taken as S − 19 the end marker is found
exactly at the computed position. CONFIRMED (crafted) with all three
black boxes. A first extended header of size 2 is rejected by the reference
implementation and `7z` and silently ignored by `unar`; a size of exactly 3
(type byte and no contents) is accepted by all.

## 6. Extended headers, level 2 and level 3

### 6.1 Level 2 header

There is no name field and no 8-bit checksum; the name travels in extended
headers and the header is protected by a CRC-16.

| Header offset | Size | Field |
|--:|--:|---|
| 0 | 2 | total header size **T** (u16): the size of the whole header including all extended headers and the padding byte |
| 2 | 5 | method identifier |
| 7 | 4 | packed size P (u32) |
| 11 | 4 | original size U (u32) |
| 15 | 4 | last-modified time, seconds since 1970-01-01 00:00:00 UTC (u32) |
| 19 | 1 | attribute, normally `20` |
| 20 | 1 | level, `02` |
| 21 | 2 | data CRC-16 (u16) |
| 23 | 1 | OS identifier |
| 24 | 2 | size of the first extended header (u16); 0 if there is none |
| 26 | | extended headers, shaped as in section 5.3 |
| | 0 or 1 | padding byte `00` |

The entry data starts at header offset T (but see the tolerance rule below)
and is P bytes long.

**Why padding exists.** The first byte of a level-2 header is the low byte
of T. A reader recognises the end of the archive by a first byte of `00`
(section 8), so a writer whose header would have a size that is a multiple
of 256 appends one `00` byte and counts it in T. The padding byte is
included in the header CRC.

**Reading procedure.** The extended headers are walked by their own size
fields, as at level 1; T is used to validate the walk.

1. Let *consumed* be 26 and *next* the field at header offset 24.
2. While *next* is not 0: *next* must be at least 3, *consumed* + *next*
   must not exceed T + 2, and the bytes must lie inside the archive;
   otherwise the header is invalid. Take the extended header, add *next* to
   *consumed*, and replace *next* by the u16 in its last two bytes.
3. Compare *consumed* with T:
   - equal: the header is complete;
   - *consumed* = T − 1: one padding byte follows and is part of the header
     (and of the header CRC); *consumed* becomes T;
   - *consumed* = T + 2: accepted as it is. Some OS-9/68K writers store T
     without counting the two bytes of the T field itself. The header is
     then *consumed* bytes long and the entry data starts at header offset
     T + 2;
   - anything else: the header is invalid.
   (**DECISION 6**.)
4. T must be at least 26. T below 26 with a valid chain cannot satisfy step
   3 and is therefore invalid.

Evidence: CONFIRMED (crafted). The exact, padded and two-short forms are all
accepted by the reference implementation. `unar` accepts the padded form and
misreads the two-short form; `7z` does the opposite (it ignores T and walks
the chain only, so it misreads every padded header). T larger than the chain
by 3 is rejected by the reference implementation.

### 6.2 Level 3 header

Like level 2 with 32-bit size fields. Rare; written by a few Windows tools.

| Header offset | Size | Field |
|--:|--:|---|
| 0 | 2 | word size (u16), must be 4 (**DECISION 7**) |
| 2 | 5 | method identifier |
| 7 | 4 | packed size P (u32) |
| 11 | 4 | original size U (u32) |
| 15 | 4 | last-modified time, seconds since 1970-01-01 00:00:00 UTC (u32) |
| 19 | 1 | attribute, normally `20` |
| 20 | 1 | level, `03` |
| 21 | 2 | data CRC-16 (u16) |
| 23 | 1 | OS identifier |
| 24 | 4 | total header size **T** (u32), including all extended headers |
| 28 | 4 | size of the first extended header (u32); 0 if there is none |
| 32 | | extended headers |

Extended headers at level 3 have the shape of section 5.3 except that the
trailing next size field is a **u32**, so the minimum size Z of an extended
header is 5 and its contents are Z − 5 bytes.

Reading procedure: as for level 2 with *consumed* starting at 32, each
*next* at least 5, and *consumed* + *next* never exceeding T. At the end
*consumed* must equal T exactly: there is no padding and no tolerance. The
entry data starts at header offset T.

The first byte of a level-3 header is `04`, never `00`, so the end marker
cannot be confused with it.

Evidence: CONFIRMED (crafted) with the reference implementation and `unar`
(`7z` does not read level 3). A word size of 8 is rejected by both. A next
size or a total size of `7FFFFFFF` is rejected by the reference
implementation without any large allocation.

### 6.3 Header CRC-16 (extended header type `00`)

An extended header of type `00`, the "common header", holds a CRC-16 of the
entire entry header.

| Contents offset | Size | Field |
|--:|--:|---|
| 0 | 2 | header CRC-16 (u16) |
| 2 | any | optional information bytes; ignored |

The CRC is the algorithm of section 3.2 computed over **every byte of the
entry header**: from header offset 0 through the last byte of the last
extended header, including the level-2 padding byte when present (and
including the two extra bytes of the two-short form of section 6.1), with
the two bytes of the CRC field itself taken as `00 00`.

Rules (**DECISION 5**):

- The common header may appear at level 1, 2 or 3. Writers of level 2 and 3
  always include it and put it first; level-1 writers usually omit it.
- If it is present with at least 2 content bytes, the CRC must match;
  otherwise the header is invalid.
- If it is absent, or present with fewer than 2 content bytes, the header
  is accepted without this check.
- More than one common header in the same entry header: invalid.

Worked value: in the level-2 example of section 11.3 the header is 50 bytes
and the stored header CRC is `B066`.

Evidence: CONFIRMED (crafted) with the reference implementation, which
rejects a header whose CRC is off by one bit, accepts a level-2 header
without a common header, and rejects a duplicate. `unar` and `7z` do not
verify the header CRC at all. PUBLISHED: LHa for UNIX verifies it at levels
2 and 3 and only prints a warning.

### 6.4 Name and directory (types `01` and `02`)

| Type | Contents |
|---|---|
| `01` | **File name**: the bytes of the last path component. No separator, no terminator. |
| `02` | **Directory name**: the directory part of the path. Each directory name is followed by the separator byte `FF`, including the last one. Example: `64 69 72 FF 73 75 62 FF` is the directory "dir/sub". |

Some Amiga writers omit the final `FF` of type `02`; the reader must not
depend on it. How the two are combined into a path is specified in section
6.7.

### 6.5 Other extended header types

Only the types marked "used" influence the result of a read-only traversal.
All other types, and all types not listed, are skipped without error. A
header whose contents are shorter than the minimum shown is ignored, not an
error.

| Type | Name | Contents | Min. contents | Use |
|---|---|---|--:|---|
| `00` | common | header CRC-16, optional information | 2 | used (6.3) |
| `01` | file name | name bytes | 0 | used (6.4, 6.7) |
| `02` | directory name | directories separated and ended by `FF` | 0 | used (6.4, 6.7) |
| `39` | multi-volume | information on split archives | | ignored |
| `3F` | comment | uncompressed comment text | 0 | optional metadata |
| `40` | MS-DOS attribute | attribute bits (u16), low byte as in 3.4 | 2 | ignored |
| `41` | Windows timestamps | creation, last modification, last access: three u64 values in units of 100 ns since 1601-01-01 00:00:00 UTC | 24 | used (7.3) |
| `42` | 64-bit sizes | packed size (u64), then original size (u64) | 16 | used (6.6) |
| `48` to `4F` | reserved (authenticity information) | | | ignored |
| `50` | Unix permission | mode bits (u16) | 2 | used for symbolic links (6.8) |
| `51` | Unix group and user id | **group id** (u16), then **user id** (u16) | 4 | ignored |
| `52` | Unix group name | text | | ignored |
| `53` | Unix user name | text | | ignored |
| `54` | Unix last-modified time | seconds since 1970-01-01 00:00:00 UTC (u32) | 4 | used (7.3) |
| `7D` | capsule | offset and size of an encapsulated archive | | ignored |
| `7E` | OS/2 extended attribute | | | ignored |
| `7F` | extended attribute (level 3, OS/2 style) | permission, owner id, timestamps | | ignored |
| `C4` to `C8` | compressed comment | dictionary size 4 KiB to 64 KiB | | ignored |
| `CC` | OS-9 | file system metadata | | ignored |
| `D0` to `DF` | system-specific | | | ignored |
| `FF` | extended attribute (level 3, Windows library style) | permission, owner id, timestamps | | ignored |

Evidence: types `00`, `01`, `02`, `3F`, `41`, `42`, `50`, `51`, `54`
CONFIRMED (crafted) (`lsar` prints the permission, the comment and the
times; the reference implementation exposes the raw headers). Types `50`,
`51`, `54` CONFIRMED (real data). The remaining rows are PUBLISHED and are
listed so that an implementer recognises them in a hex dump; none needs
code.

### 6.6 64-bit sizes (type `42`)

When an entry header of **level 2 or 3** contains a type `42` extended
header with at least 16 content bytes, its two values replace P and U from
the fixed fields at offsets 7 and 11 (**DECISION 9**). At level 0 and 1 the
type is ignored: the level-1 skip-size arithmetic of section 5.3 is defined
on the 32-bit field.

The replaced P is subject to the same checks as any P: the data must lie
inside the archive.

Evidence: CONFIRMED (crafted) with the reference implementation: at level 2
the 64-bit values are used (the 32-bit fields held `FFFFFFFF`), at level 1
they are not. `unar` refuses archives containing the type; `7z` ignores it
at both levels.

### 6.7 Names and paths

The result of this section is a **path**: a sequence of zero or more
components, each a non-empty byte string, plus an optional comment. The
path is always relative.

**Step 1: collect the raw pieces.**

- *base name field*: the N bytes at header offset 22 (levels 0 and 1 only).
- *name header*: the contents of the type `01` extended header, if any.
- *directory header*: the contents of the type `02` extended header, if
  any.

If a type occurs more than once in one entry header, the **last occurrence
is used** (**DECISION 10**).

**Step 2: cut at the first `00` byte.** In each piece, the first `00` byte
ends the piece. For the base name field and the name header, the bytes
after that `00` are a **comment** (Amiga tools store the file note this
way); for the directory header they are discarded. This applies to every OS
identifier and every level (**DECISION 13**).

**Step 3: build the component list.**

1. Start with the components of the directory header: split it at every
   byte `FF`, `2F` ("/") or `5C` ("\\") (**DECISION 11**).
2. If a name header is present and, after step 2, not empty: it supplies
   exactly **one** final component. Bytes `2F` in it are replaced by `5F`
   ("_"); it is not split (**DECISION 11**). The base name field is not
   used at all.
3. Otherwise, append the components of the base name field, split at every
   byte `FF`, `2F` or `5C`. The base name field is a path: level-0 writers
   store the whole path in it with "\\" (MS-DOS) or "/" (Unix, Amiga)
   between directories, and level-1 writers sometimes do too.
4. Remove every empty component and every component that is exactly "."
   or ".." (**DECISION 12**). This also removes leading separators, so
   "\\abs\\z.txt" becomes the two components "abs", "z.txt".

**Step 4: note whether the path names a directory.** The flag "the path
ends with a separator" is determined from the pieces as they are after
step 2:

- a non-empty name header is present: the flag is false;
- otherwise, the base name field is not empty: the flag is true exactly
  when its last byte is `FF`, `2F` or `5C`;
- otherwise (no name piece at all): the flag is true.

The flag is used in section 6.8.

**Step 5: convert components to text** (**DECISION 14**). For each
component separately: if all its bytes are below `80`, it is ASCII. If it
is well-formed UTF-8, it is UTF-8. Otherwise each byte is one character of
ISO 8859-1 (the byte value is the code point). Control characters and
characters that are unsafe in file names are left in place; replacing them
is the business of the layer that uses the path.

Things a reader must not do:

- It must not treat a drive prefix specially. "C:\\abs\\w.txt" yields the
  components "C:", "abs", "w.txt".
- It must not interpret "|" except as described for symbolic links in
  section 6.8.
- It must not change letter case. (Some tools lower-case all-capital MS-DOS
  names; this specification does not.)

**Known limitation.** Japanese archives store names in Shift-JIS, where the
second byte of a two-byte character can be `5C`. Splitting a base name
field or a directory header at `5C` then cuts such a character in half.
Telling the two cases apart needs knowledge of the encoding that the header
does not provide; the black boxes all split as specified here. The name
header (type `01`) is not split, so level-2 names are not affected in their
last component.

Evidence: CONFIRMED (crafted) for the pieces, the `FF` separator, the
missing final `FF`, "\\" and "/" in level-0 names, and the comment after
`00` (reported as comment by `unar` at every OS identifier and by the
reference implementation for Amiga). The black boxes disagree on duplicates
(reference implementation: last name, all directories concatenated; `unar`:
last of each; `7z`: first of each), on separators inside a name header
(replaced by "_", kept, or split), and on "." and ".." (dropped by the
reference implementation, kept by `7z`; `unar` terminates abnormally on a
leading ".."). The combination "directory header plus a base name field
containing a path" gives "ext/DIR/BASE.TXT" in the reference implementation
and `7z`.

### 6.8 Entry kinds: file, directory, symbolic link

Apply the first rule that matches (**DECISION 15**):

1. **Symbolic link**: the method is "-lhd-", Unix mode bits are known (from
   extended header `50` or the level-0 Unix area) with (mode AND `F000`) =
   `A000`, and the raw name contains the byte `7C` ("|"). The bytes before
   the first "|" of the complete raw path (directory header followed by
   name) are the link's own path, processed as in 6.7; the bytes after it
   are the link target, kept as text and never followed. A symbolic link
   has no data.
2. **Directory**: the method is "-lhd-". The whole path names the
   directory; a name header, if present, is its last component.
3. **Directory, mislabelled**: the method is "-lh0-", U = 0, and the path
   ends with a separator in the sense of section 6.7 step 4, and the path
   is not empty. Old Amiga tools wrote directories this way.
4. **File**: everything else.

For directories and symbolic links:

- P is normally 0. If it is not, the P bytes are skipped like any entry
  data and are not interpreted.
- The CRC field is not checked.
- A directory whose path has no components is dropped silently.

For files:

- A file whose path has no components is delivered under the placeholder
  name "unnamed" (**DECISION 16**). Genuine archives with nameless files
  exist (PUBLISHED).

Evidence: CONFIRMED (crafted): "-lhd-" entries are directories for all
black boxes at levels 0, 1 and 2, with or without the final `FF`, with a
name header, and with P = 5 (the five bytes are skipped). The mislabelled
form is listed as an empty file by all three black boxes and as a directory
by two published tools; rule 3 follows the latter. None of the black boxes
resolves symbolic links (they show "link|target" as a directory name); the
"|" convention is PUBLISHED.

## 7. Timestamps

### 7.1 MS-DOS timestamp (levels 0 and 1, header offset 15)

The u32 is read little-endian; its high 16 bits are the date and its low 16
bits the time:

| Bits (31 = most significant) | Width | Meaning |
|---|--:|---|
| 31 to 25 | 7 | year minus 1980 (0 to 127) |
| 24 to 21 | 4 | month, 1 to 12 |
| 20 to 16 | 5 | day of month, 1 to 31 |
| 15 to 11 | 5 | hour, 0 to 23 |
| 10 to 5 | 6 | minute, 0 to 59 |
| 4 to 0 | 5 | seconds divided by 2, 0 to 29 |

It is a **local civil time without a time zone**: the archive does not say
which zone. Example: the bytes `5C 64 6F 1E` are the value `1E6F645C`, which
is 1995-03-15 12:34:56.

A value whose fields do not form a real date and time (month 0 or above 12,
day 0 or beyond the month's length, hour above 23, minute above 59, seconds
field above 29), including the value 0, means "no timestamp"
(**DECISION 18**).

### 7.2 Unix timestamp (levels 2 and 3, header offset 15)

An **unsigned** 32-bit count of seconds since 1970-01-01 00:00:00 UTC. It
identifies an instant; no zone conversion is needed. Example: the bytes
`F0 DE 66 2F` are the value `2F66DEF0` = 795,270,896, which is 1995-03-15
12:34:56 UTC.

### 7.3 Which timestamp is the entry's last-modified time

Use the first of these that is present (**DECISION 17**):

1. extended header `54` (Unix time, UTC);
2. extended header `41` with at least 24 content bytes: its second u64, the
   last-modification time. The instant is that value divided by 10,000,000
   seconds after 1601-01-01 00:00:00 UTC; subtract 11,644,473,600 seconds to
   obtain seconds since 1970. A value before 1970 or not representable
   means this source is absent;
3. the Unix time of the level-0 extended area (section 4.3);
4. the fixed field at header offset 15, interpreted by level (7.1 or 7.2).

Sources 1 to 3 and the level-2/3 field are instants in UTC; the level-0/1
field is a zone-less local time. A reader that has only one kind of
timestamp to offer treats the zone-less value as if it were UTC and says so
in its documentation.

Evidence: CONFIRMED (crafted): with header `54` at level 1 and at level 2,
and with header `41` at level 1 and at level 2, the reference implementation
and `unar` both report the extended header's time, not the fixed field. A
zero or impossible MS-DOS value yields "no timestamp" in the reference
implementation and an invented date in `unar`.

## 8. Sequence of entries and end of archive

The reader keeps a position *pos*, initially the archive offset of the first
header. It repeats:

1. **End by exhaustion.** If *pos* = L, the archive ends normally. An end
   marker is optional (CONFIRMED (crafted): all black boxes read an archive
   without one).
2. **End marker.** If the byte at *pos* is `00`, the archive ends normally.
   Bytes after it are ignored and never examined (CONFIRMED (crafted):
   twelve junk bytes and a further complete entry after the marker are
   ignored by the reference implementation and `unar`).
3. **Parse a header** at *pos* by the rules of sections 3 to 6. If fewer
   than 21 bytes remain, or the header is invalid in any way (level above
   3, bad checksum or header CRC, sizes that do not fit, bytes 2 or 6 not
   `2D`):
   - if no entry has been read yet, the input is **not an LHA archive**;
   - otherwise the archive **ends here** with a finding "unreadable data
     after the last entry"; the entries already read stay valid
     (**DECISION 22**). This is what happens with files padded by a
     transfer protocol (for example with `1A` bytes).
4. **Locate the data.** The data occupies archive offsets *data* to
   *data* + P, where *data* is the end of the header. If *data* + P exceeds
   L, the entry is listed and reported as an entry error of kind truncated,
   and the archive ends. (The comparison must be made without overflow; P
   can be a 64-bit value.)
5. **Deliver or report the entry** (section 6.8 and `methods.md`). Whatever
   the outcome of decoding, set *pos* to *data* + P and continue with step
   1. The decoder never determines where the next header is.

Consequences:

- An entry whose method is unsupported, whose stream is corrupt or whose
  CRC fails does not affect later entries.
- The reader makes progress in every round: a valid header is at least 22
  bytes long, so *pos* strictly increases and the loop ends after at most
  L / 22 rounds.

## 9. Detection, leading junk and self-extracting archives

### 9.1 Detection at the start

Input is recognised as an LHA archive when all of the following hold at
header offset 0 of the input (**DECISION 1**):

1. at least 21 bytes are available;
2. byte 2 is `2D`, byte 3 is `6C` ("l"), byte 4 is `68` ("h") or `7A`
   ("z"), byte 6 is `2D`;
3. byte 20 is at most 3;
4. the header parses as a valid header by the rules of sections 3 to 6,
   including its checksum or header CRC.

Conditions 1 to 3 are a cheap signature suitable for format sniffing;
condition 4 is applied when the archive is opened. An archive whose first
entry uses a "-pm" method is deliberately not recognised: PMarc archives
are out of scope.

Evidence: `unar` and `7z` recognise archives only at offset 0, by tests of
this kind. The reference implementation requires a valid header at offset
0.

### 9.2 Leading junk and self-extracting prefixes

A self-extracting archive is an executable program followed by an ordinary
archive. Files that passed through some transfer tools carry a short prefix.
In both cases the archive proper starts at a later offset.

If detection at offset 0 fails, a reader that has been asked to open the
input as LHA searches for the start (**DECISION 1**):

1. Consider each candidate offset *c* from 1 upward, in increasing order,
   as long as *c* is below 262,144 and *c* + 21 ≤ L.
2. A candidate passes the **quick test** if conditions 2 and 3 of section
   9.1 hold at *c*.
3. A candidate that passes the quick test is given the **strict test**:
   - the header at *c* is valid (section 9.1 condition 4);
   - its data lies inside the input (*data* + P ≤ L);
   - the position after its data is either L, or holds a `00` byte, or
     passes the quick test itself (with "-pm" and any other three middle
     bytes also allowed there, that is: bytes 2 and 6 are `2D` and byte 20
     is at most 3).
4. The first candidate that passes the strict test is the start of the
   archive; all offsets are counted from it from then on. If none passes,
   the input is not an LHA archive.

Some self-extractor programs contain text that looks like a method
identifier (for example the string "-lh5-" in a message, or a decoy header
in two known stubs). The strict test rejects these because their checksum
or their sizes do not fit; no stub-specific knowledge is needed.

The limit of 262,144 bytes is larger than every known self-extractor stub
(PUBLISHED: the largest known is below that size).

Wrappers that are separate formats (MacBinary, BinHex, and so on) are
removed by other layers before the LHA reader is called.

Evidence: none of the three black boxes searches for a prefix (all three
reject an archive preceded by 302 bytes); the search is PUBLISHED behaviour
of two other LHA tools, which use a quick test only, plus special cases for
two stubs. The strict test is this specification's replacement for those
special cases.

## 10. Safety requirements for untrusted input

Everything in an archive is untrusted. The following list is normative.

**Bounds**

1. Every read of header bytes is preceded by a check against L. A header
   that would extend past L is invalid (or, for the 21-byte minimum,
   handled by section 8 step 3).
2. Level 0: N ≤ H − 20 must hold before the name is read; H+2 must be
   22+N or at least 24+N. Level 1: H+2 ≥ 27+N.
3. Each extended header size is checked against its minimum (3, or 5 at
   level 3) **before** its last bytes are read as the next size. A size of
   1 or 2 would otherwise make the reader take the next size from bytes
   that are not part of the header.
4. Level 1: each extended header size is checked against the remaining
   skip size before it is subtracted. The subtraction must never go below
   zero.
5. Levels 2 and 3: the running total of consumed bytes is checked against T
   (with the two-byte tolerance at level 2 only) before each extended
   header is taken.
6. Contents of extended headers are read only after checking the minimum
   content length in the table of section 6.5. A type `42` header with 15
   content bytes, a type `54` with 3, a type `00` with 1 are ignored, not
   read.
7. The level-0 Unix area is interpreted only when it is at least 12 bytes
   long.

**Arithmetic**

8. P and U can be 64-bit values (section 6.6). All sums such as
   *data* + P must be computed so that they cannot wrap: compare P with
   L − *data* instead of adding.
9. At level 3, T and the extended header sizes are 32-bit values chosen by
   the writer. They are compared with the bytes available; they are never
   used as an amount of memory to reserve. A header that claims `7FFFFFFF`
   bytes must be rejected by comparison alone.
10. On platforms where the native size type is 32 bits wide, 64-bit sizes
    are compared as 64-bit numbers before any conversion.

**Allocation**

11. U is a claim, never an amount of memory to reserve up front. Output
    buffers grow with the bytes actually produced, up to the caller's
    output limit; exceeding the limit is an entry error of kind output
    limit exceeded, not corrupt data. `methods.md` section 8 gives, per
    method, how much output a given amount of packed data can produce.
12. Names are at most 255 bytes at levels 0 and 1, but at levels 2 and 3 a
    name or directory header can be as long as the header (up to 65,535
    bytes at level 2 and limited only by L at level 3). A reader may refuse
    a path longer than a limit of its choice (not less than 4,096 bytes) by
    treating the header as invalid.
13. The number of extended headers in one entry header is bounded only by
    the header size; the walk is linear and needs no per-header allocation.

**Loops and termination**

14. The entry loop of section 8 always advances by at least 22 bytes.
15. The extended header walk always advances by at least 3 bytes.
16. The prefix search of section 9.2 examines at most 262,144 candidates,
    and the strict test of one candidate is linear in the size of one
    header. The strict test does not decode data.
17. Only one entry header is examined per entry; a reader must not follow
    any offset stored inside a header to another place in the archive
    (there is none in this format).

**Paths**

18. A path never contains "." or ".." components, never an empty
    component, and never starts with a separator (section 6.7). The caller
    must still apply its own file-name policy to the text of each
    component: components can contain ":", "\\" (from a name header),
    control characters, and names reserved on some systems.
19. A symbolic link's target is data. It is never resolved, followed or
    used to locate anything.

**Checksums**

20. The 8-bit header checksum of levels 0 and 1 is weak (one random header
    in 256 passes). It is a plausibility test, not an integrity guarantee;
    all other bounds above must hold independently of it.
21. Contents are delivered only after the CRC-16 comparison (except where
    no CRC exists, section 4.2).

## 11. Worked examples

All examples store the twelve bytes "Hello, LHA!" plus a line feed
(`48 65 6C 6C 6F 2C 20 4C 48 41 21 0A`, CRC-16 `4953`) with method "-lh0-"
and the time 1995-03-15 12:34:56. Each was accepted by every black box that
supports its level, with the listed name, time and contents.

### 11.1 Level 0: path "DIR\\HELLO.TXT"

50 bytes:

```
0000  23 69 2d 6c 68 30 2d 0c 00 00 00 0c 00 00 00 5c
0010  64 6f 1e 20 00 0d 44 49 52 5c 48 45 4c 4c 4f 2e
0020  54 58 54 53 49 48 65 6c 6c 6f 2c 20 4c 48 41 21
0030  0a 00
```

| Offset | Bytes | Meaning |
|--:|---|---|
| 0 | `23` | H = 35: the header is 37 bytes (offsets 0 to 36) |
| 1 | `69` | checksum: the sum of the 35 bytes at offsets 2 to 36 is congruent to `69` modulo 256 |
| 2 | `2D 6C 68 30 2D` | "-lh0-" |
| 7 | `0C 00 00 00` | P = 12 |
| 11 | `0C 00 00 00` | U = 12 |
| 15 | `5C 64 6F 1E` | MS-DOS timestamp `1E6F645C`: year field 15 (1995), month 3, day 15, hour 12, minute 34, seconds field 28 (56 s) |
| 19 | `20` | attribute: archive |
| 20 | `00` | level 0 |
| 21 | `0D` | N = 13 |
| 22 | `44 49 52 5C 48 45 4C 4C 4F 2E 54 58 54` | "DIR\\HELLO.TXT" |
| 35 | `53 49` | data CRC-16 `4953` |
| 37 | 12 bytes | the data. H+2 = 37 = 24+N, so there is no extended area |
| 49 | `00` | end marker |

Result: one file, path components "DIR", "HELLO.TXT", 12 bytes.

### 11.2 Level 1: name "HELLO.TXT", directory header "dir", Unix time header

63 bytes:

```
0000  22 95 2d 6c 68 30 2d 1a 00 00 00 0c 00 00 00 5c
0010  64 6f 1e 20 01 09 48 45 4c 4c 4f 2e 54 58 54 53
0020  49 55 07 00 02 64 69 72 ff 07 00 54 f0 de 66 2f
0030  00 00 48 65 6c 6c 6f 2c 20 4c 48 41 21 0a 00
```

| Offset | Bytes | Meaning |
|--:|---|---|
| 0 | `22` | H = 34: the base header is 36 bytes (offsets 0 to 35) |
| 1 | `95` | checksum of the 34 bytes at offsets 2 to 35 |
| 2 | `2D 6C 68 30 2D` | "-lh0-" |
| 7 | `1A 00 00 00` | skip size S = 26 |
| 11 | `0C 00 00 00` | U = 12 |
| 15 | `5C 64 6F 1E` | MS-DOS timestamp, as in 11.1 |
| 19 | `20` | attribute |
| 20 | `01` | level 1 |
| 21 | `09` | N = 9 |
| 22 | 9 bytes | "HELLO.TXT" |
| 31 | `53 49` | data CRC-16 `4953` |
| 33 | `55` | OS identifier "U" |
| 34 | `07 00` | size of the first extended header: 7. (H = 34 is the offset of this field; there is no extended area because 25+N = 34) |
| 36 | `02 64 69 72 FF` `07 00` | extended header of 7 bytes: type `02`, contents "dir" and `FF`; next size 7 |
| 43 | `54 F0 DE 66 2F` `00 00` | extended header of 7 bytes: type `54`, Unix time `2F66DEF0`; next size 0: end of chain |
| 50 | 12 bytes | the data: P = S − 7 − 7 = 12 |
| 62 | `00` | end marker |

Result: one file, components "dir", "HELLO.TXT"; last-modified time
1995-03-15 12:34:56 UTC (from header `54`, which takes precedence over the
fixed field).

### 11.3 Level 2: name "hello.txt", directory "dir"

63 bytes:

```
0000  32 00 2d 6c 68 30 2d 0c 00 00 00 0c 00 00 00 f0
0010  de 66 2f 20 02 53 49 55 05 00 00 66 b0 0c 00 01
0020  68 65 6c 6c 6f 2e 74 78 74 07 00 02 64 69 72 ff
0030  00 00 48 65 6c 6c 6f 2c 20 4c 48 41 21 0a 00
```

| Offset | Bytes | Meaning |
|--:|---|---|
| 0 | `32 00` | T = 50 |
| 2 | `2D 6C 68 30 2D` | "-lh0-" |
| 7 | `0C 00 00 00` | P = 12 |
| 11 | `0C 00 00 00` | U = 12 |
| 15 | `F0 DE 66 2F` | Unix time 795,270,896 |
| 19 | `20` | attribute |
| 20 | `02` | level 2 |
| 21 | `53 49` | data CRC-16 `4953` |
| 23 | `55` | OS identifier "U" |
| 24 | `05 00` | size of the first extended header: 5 |
| 26 | `00 66 B0` `0C 00` | type `00`: header CRC-16 `B066`; next size 12 |
| 31 | `01 68 65 6C 6C 6F 2E 74 78 74` `07 00` | extended header of 12 bytes: type `01`, contents "hello.txt"; next size 7 |
| 43 | `02 64 69 72 FF` `00 00` | type `02`: "dir" and `FF`; next size 0 |
| 50 | 12 bytes | the data: *consumed* = 26 + 5 + 12 + 7 = 50 = T, no padding |
| 62 | `00` | end marker |

Header CRC: replace the bytes at offsets 27 and 28 by `00 00` and compute
the CRC-16 of the 50 bytes at offsets 0 to 49; the result is `B066`.

Result: one file, components "dir", "hello.txt".

### 11.4 Level 3: name "hello.txt"

66 bytes:

```
0000  04 00 2d 6c 68 30 2d 0c 00 00 00 0c 00 00 00 f0
0010  de 66 2f 20 03 53 49 55 35 00 00 00 07 00 00 00
0020  00 fa e9 0e 00 00 00 01 68 65 6c 6c 6f 2e 74 78
0030  74 00 00 00 00 48 65 6c 6c 6f 2c 20 4c 48 41 21
0040  0a 00
```

| Offset | Bytes | Meaning |
|--:|---|---|
| 0 | `04 00` | word size 4 |
| 2 to 23 | | as in level 2, with level byte `03` |
| 24 | `35 00 00 00` | T = 53 |
| 28 | `07 00 00 00` | size of the first extended header: 7 |
| 32 | `00 FA E9` `0E 00 00 00` | type `00`: header CRC-16 `E9FA`; next size 14 |
| 39 | `01` + "hello.txt" + `00 00 00 00` | type `01`; next size 0 |
| 53 | 12 bytes | the data: *consumed* = 32 + 7 + 14 = 53 = T |
| 65 | `00` | end marker |

The header CRC `E9FA` is the CRC-16 of the 53 header bytes with offsets 33
and 34 taken as `00 00`.

### 11.5 Level 1 directory entry from an Amiga writer

40 bytes:

```
0000  19 59 2d 6c 68 64 2d 0c 00 00 00 00 00 00 00 5c
0010  64 6f 1e 20 01 00 00 00 41 0c 00 02 44 6f 63 73
0020  ff 4f 6c 64 ff 00 00 00
```

H = 25, so the base header is 27 bytes: method "-lhd-", skip size 12, U =
0, N = 0 (no name), CRC field `0000`, OS identifier "A", first extended
header size 12. The extended header at offset 27 is type `02` with the
contents `44 6F 63 73 FF 4F 6C 64 FF` ("Docs", "Old") and next size 0. P =
12 − 12 = 0. The byte at offset 39 is the end marker.

Result: one directory with the components "Docs", "Old".

## 12. Observed implementation gaps

Faults and limits of the black boxes seen during this work. They explain
why some rules exist; none is to be imitated.

**Reference implementation**

- Rejects the whole archive, including all valid entries before it, when
  any later header position holds something that is not a header (padding,
  junk without an end marker).
- Rejects level-0 headers without a CRC field.
- Does not search for a prefix; a self-extracting archive is "not an
  archive".
- Converts every byte outside printable ASCII in names to a percent
  escape, so names in any 8-bit encoding become unreadable.
- For a stored entry with U above P it reports an error only when read; for
  U below P it returns the first U bytes.
- Treats "-pm0-" as stored (this specification reports it as unsupported,
  **DECISION 19**).

**`unar`**

- Ignores the header checksum and the header CRC entirely.
- Refuses an archive whose first method identifier it does not know, and
  does not know "-lzs-".
- Refuses archives containing extended header `42`.
- Terminates abnormally on a path that begins with "..".
- Misreads level-2 headers of the two-short form.
- Extracts nothing for a file without a name.

**`7z`**

- Does not read level 3.
- Ignores the total header size of level 2 and therefore misreads padded
  headers.
- Copies P bytes for stored entries regardless of U.
