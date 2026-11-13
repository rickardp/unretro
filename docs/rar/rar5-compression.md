# RAR 5.0 compression (algorithm versions 0 and 1)

Format specification for clean-room implementation. This document describes
the compression method used for the compressed files of an archive in the
RAR 5.0 container format (signature `52 61 72 21 1a 07 01 00`): a
sliding-window LZ77 coder with Huffman-coded symbols whose dictionary can
be 128 KiB to 4 GiB (algorithm version 0, written since RAR 5.0) or up to
1 TiB (algorithm version 1, written since RAR 7.0), organised in
self-delimiting blocks, followed by an output stage that reverses one of
four fixed byte transformations ("filters") on marked ranges of the data.

It does not describe the archive container (signature, headers, volumes,
checksums, encryption, service records): see `rar5-container.md`. It does
not describe the methods of the older RAR4 container (`rar29-lz.md`,
`rar29-ppmd.md`, `rar29-filters.md`, `rar20.md`, `rar15.md`). RAR 5.0 has
no PPMd mode and no programmable filter machine.

Contents: 1 how to read this document; 2 what the decoder is given and
returns; 3 the bit stream; 4 prefix codes; 5 decoder state; 6 blocks; 7
table transmission; 8 items; 9 the window; 10 filters; 11 members, solid
groups, volumes and end of data; 12 worked examples; 13 test vectors; 14
safety requirements; 15 observed implementation gaps; 16 decisions; 17
open questions; 18 evidence.

## 1. How to read this document

### 1.1 Provenance

- This is a **prose specification**. It was derived by studying the
  behaviour of an existing open decoder (the RAR 5.0 support of The
  Unarchiver's XADMaster library, LGPL, revision `7cb9ee0a` of
  `github.com/MacPaw/XADMaster`; called the *reference implementation*),
  by black-box testing of the extraction tool built from that library
  (`unar`/`lsar` 1.10.7) and of a second, unrelated extraction tool
  (`bsdtar` 3.5.3, used only as a program, its source not read) with
  purpose-built archives, by measurements on archives written by the
  vendor's own archiver (RAR 5.91 and RAR 7.23, run only as a black-box
  archive creator), and from the vendor's public technical note on the
  RAR 5.0 archive format (which describes the header fields quoted in
  section 2 and nothing of the algorithm).
- The official `unrar` source was **not** consulted; its licence forbids
  using it to recreate the algorithm. The RAR code of 7-Zip and of
  libarchive was not read either.
- The work is a **clean-room process**. The author of this document could
  read the reference implementation; the implementation is written **from
  this document only**, by implementers who do not read the source of any
  RAR decoder. The document therefore contains no program code and no
  pseudo-code, only prose, tables of constants, worked examples and test
  vectors.
- Where the reference implementation is wrong or unsafe the document says
  so (section 15) and specifies the correct behaviour instead. Being
  compatible with the reference implementation is not a goal; reading
  genuine archives correctly and rejecting hostile input safely is.

### 1.2 Evidence labels

| Label | Meaning |
|---|---|
| CONFIRMED (real data) | The rule was exercised when decoding archives written by RAR 5.91 or RAR 7.23 (and two third-party archives), with an experimental decoder written from the rules as worded here, and every decoded file matched the CRC-32 stored in its header (section 18). |
| CONFIRMED (crafted) | Checked by giving `unar` and `bsdtar` a purpose-built archive and comparing their output with the output predicted by this document. Where only one of the two tools agrees, or neither can decode the case at all, the text says so. |
| REFERENCE-ONLY | Stated by the reference implementation; not separately observable. |
| INFERRED | Deduced; not confirmed. |
| DECISION | A choice made by this specification, not a fact about the format (section 16). |

A rule without a label in sections 3 to 11 is CONFIRMED (real data).

### 1.3 Terms and numbers

| Term | Meaning |
|---|---|
| member | One file stored in the archive, after the container layer has joined the pieces of a file that is split across volumes. |
| packed bytes | The data area of a member as stored (all pieces concatenated, deciphered if the member was encrypted). **P** is their number. |
| **U** | The unpacked size the member's header declares. Untrusted. May be flagged "unknown" by the container. |
| **D** | The dictionary size the member's header declares, in bytes (section 2). Untrusted. |
| algorithm version | 0 or 1; decides the size of the distance alphabet and nothing else (section 2). |
| solid group | A maximal run of members that share one decoder state (section 11.2). A member of an archive that is not solid is a group of one. |
| block | A unit of the packed bytes: a header of 3 to 5 bytes followed by the number of data bytes the header announces (section 6). The packed bytes of a member are a chain of blocks. |
| item | One decoding step inside a block: a literal, a match of one of three kinds, or a filter declaration (section 8). |
| match | An instruction to copy *length* bytes starting *distance* bytes back in the LZ output. |
| LZ output | The bytes produced by literals and matches, before filters. The **group count** is the number of LZ output bytes produced so far in the solid group (a 64-bit count). The *LZ position* of a byte is the group count at the moment before the byte was produced (the first byte of a group has LZ position 0). |
| member output | The LZ output of the member after the filter stage (section 10): exactly as many bytes, some ranges transformed. This is the file's content. |
| pre-code | The 20-symbol prefix code used only to transmit code lengths. |
| registers | The four most recently used match distances, R0 (most recent) to R3 (section 8.5). |
| corrupt input | The decoder's error result. No contents are delivered for the member. |
| unsupported | An error result distinct from corrupt input: the data may be valid but this decoder declines it (unknown algorithm version, dictionary larger than the configured limit). No contents are delivered for the member. |

Numbers are decimal unless prefixed `0x`. Byte strings are written as
hexadecimal digits in code style (`c6 8e`). Bit strings are written in
code style in reading order (`1101`). Ranges written `a..b` include both
ends. "Bit n" of a stream counts from 0 in reading order (section 3).
"mod" is the remainder after division, always taken as a non-negative
value. KiB, MiB, GiB and TiB are 2 to the power 10, 20, 30 and 40 bytes.

## 2. What the decoder is given and what it returns

The container layer (`rar5-container.md`) hands the decoder, for each
file header whose compression method (below) is 1..5:

- the packed bytes of the member (P bytes; the data areas of the pieces
  of a split file already concatenated in volume order, section 11.5;
  already deciphered if the member is encrypted);
- U, or the fact that U is unknown (file flag `0x0008` of the file
  header);
- the three values taken from the **compression information** field of
  the file header, as described below: algorithm version, solid flag and
  dictionary size D;
- the checksum stored for the member (CRC-32, optionally a BLAKE2sp hash;
  both are the container's business).

It returns exactly U bytes (or, when U is unknown, however many bytes
the stream produces), or an error.

The "unknown" flag occurs in real archives on the **earlier pieces of a
split member whose input was a pipe**: both RAR 5.91 and RAR 7.23, told
to archive standard input into volumes, set file flag `0x0008` (with a
meaningless size field) on every piece but the last, and write the true
size, without the flag, on the last piece. Which piece's values the
decoder is given is the container's business (`rar5-container.md`); for
this decoder the two cases behave alike, because the block chain alone
says where the member ends (section 11.1). CONFIRMED (real data): the
four-piece members of two such volume sets decode to their stored
CRC-32 both with U taken from the last piece and with U treated as
unknown.

### 2.1 The compression information field

The field is one variable-length integer of the file header. Its layout
is quoted here from the vendor's technical note because the decoder's
behaviour depends on every part of it; reading the integer itself is the
container's task. Bit 0 is the least significant bit of the value.

| Bits | Mask | Name | Meaning |
|---|---|---|---|
| 0..5 | `0x00003f` | version | 0 or 1. See below. |
| 6 | `0x000040` | solid | 1: the member continues the decoder state left by the preceding member (section 11.2). |
| 7..9 | `0x000380` | method | 0: stored, the packed bytes are the member's bytes and this decoder is not involved. 1..5: compressed; the value is the *encoder's* effort level and the decoder ignores it. |
| 10..14 | `0x007c00` | N | Dictionary size exponent. |
| 15..19 | `0x0f8000` | F | Dictionary size fraction, in 32nds. **Meaningful only when version is 1.** |
| 20 | `0x100000` | compatibility | **Meaningful only when version is 1.** 1: the dictionary size is given in the version 1 form (N and F) but the stream uses algorithm version 0. |

Derived values:

- **Algorithm version.** If version is 0: algorithm version 0. If
  version is 1 and the compatibility bit is 0: algorithm version 1. If
  version is 1 and the compatibility bit is 1: algorithm version 0.
  (CONFIRMED (real data) for all three: section 18 describes the archive
  in which RAR 7.23 itself set the compatibility bit.)
- **Dictionary size.** Let B be 131,072 (128 KiB) multiplied by 2 to the
  power N.
  - Version 0: D = B. N must be 0..15, so D is a power of two from
    128 KiB to 4 GiB. (N = 0, 2, 3, 5, 7, 8 and 15 occur in the survey.)
  - Version 1 (with or without the compatibility bit): D = B + (B / 32)
    × F. N may be 0..23 and F 0..31. D need not be a power of two.
    (CONFIRMED (real data) for N = 15 with F = 1 and F = 4, giving
    4,429,185,024 and 4,831,838,208.)
- DECISION: version greater than 1 is *unsupported*. Version 0 with N
  greater than 15, version 0 with any of the bits 15..20 set, version 1
  with N greater than 23, and any bit above bit 20 set, are
  *unsupported* too. (Both black-box tools ignore the version field and
  decode a crafted member marked version 2 as if it were version 0;
  section 15.)
- DECISION: method 6 or 7 is *unsupported*. (`unar` decodes them like
  1..5; `bsdtar` refuses them.)

The vendor's archiver lowers D to fit the data: a 2.7 MB file archived
with a requested dictionary of 4 GiB is stored with D = 4 MiB. A large D
therefore normally means a large solid group.

### 2.2 Other header facts the decoder relies on

- **Stored members** (method 0) never reach this decoder, also when they
  sit in the middle of a solid group (section 11.2).
- **Directory entries** and other headers without data never reach this
  decoder.
- **Service headers** (archive comment, quick-open record and so on) may
  in principle carry compressed data with method 1..5. DECISION: such
  data is decoded as a solid group of its own, consisting of that one
  member, with a fresh state; it neither uses nor disturbs the state of
  the file members around it. (The technical note says the solid flag is
  never set for service headers; no compressed service data was seen in
  the survey.)
- **Encrypted members** are deciphered by the container layer before
  decoding. If the container cannot do so (cipher unsupported, password
  missing) the member is reported as "encrypted, unsupported" and is
  treated like a failed member for the purposes of section 11.4.
- **Checksums.** The CRC-32 is the common one (polynomial `0xEDB88320`
  reflected, initial value and final XOR `0xFFFFFFFF`; the ASCII string
  `123456789` gives `0xCBF43926`), computed over the member output.
  DECISION: after decoding, the container layer compares the checksum of
  the member output with the stored one and reports a mismatch as an
  error for that member ("checksum mismatch"); the bytes are not
  delivered as good contents. A mismatch alone does not invalidate the
  solid group's state (section 11.4). How the stored value is found,
  which hash is used, and how it is transformed for encrypted members,
  is specified in `rar5-container.md`.

## 3. The bit stream

- The packed bytes of one member form one bit stream. Bits are taken
  from each byte starting with the **most significant bit**. Bit 0 of
  the stream is the top bit of the first byte.
- A field of n bits is read as n successive bits; the **first bit read
  is the most significant** bit of the field's value. A field of 0 bits
  reads nothing and has the value 0. Fields in this format are 1 to 34
  bits wide (the widest are the directly-read distance bits of section
  8.3: at most 26 bits for algorithm version 0, at most 34 for version
  1); an implementation must not assume that a field fits in 16 or 32
  bits.
- The stream is **byte-aligned at exactly one kind of place**: every
  block header starts on a byte boundary (section 6). Inside a block's
  data nothing is aligned: the table transmission and the items follow
  one another bit by bit. In particular the 8-bit quantities of a
  filter declaration (section 8.7) are ordinary 8-bit fields that start
  at whatever bit position the stream has reached.
- **Reading a bit that lies beyond the last packed byte is corrupt
  input.** An implementation that decodes prefix codes by looking ahead
  a fixed number of bits must pad the look-ahead with zero bits and fail
  only if the bits actually consumed extend past the limit. (CONFIRMED
  (crafted) with `unar`, vectors E11 and E12.) The same applies, more
  strictly, to the end of each block: section 6.3.

Each member has **its own bit stream starting at bit 0 of its own packed
bytes**, also inside a solid group (section 11.2).

## 4. Prefix codes

All five Huffman codes in the format (the pre-code, the main code, the
distance code, the low-distance code and the length code) are built the
same way from an array of code lengths, one per symbol, each length
0..15. Length 0 means the symbol does not occur and has no code.

### 4.1 Assigning the codes

The code is the canonical one in which **shorter codes come first, and
among codes of equal length the lower symbol number comes first, counting
upward from the all-zero code**:

- Keep a running code value, initially 0.
- Go through the lengths 1, 2, ..., 15 in this order. For one length L,
  go through all symbols in ascending symbol number; each symbol whose
  length is L receives the running code value written as an L-bit number,
  and the running value is then increased by 1. When all symbols of
  length L are done, the running value is doubled before moving on to
  length L + 1.
- A code is matched against the stream with its most significant bit
  first (the first bit read from the stream is the top bit of the L-bit
  number).

Example: lengths 2, 2, 2, 2 for the symbols 97, 98, 99 and 263 (all others
0) give 97 = `00`, 98 = `01`, 99 = `10`, 263 = `11`. Lengths 3, 3, 2, 2,
2 for the symbols 1, 2, 3, 18, 19 give 3 = `00`, 18 = `01`, 19 = `10`,
1 = `110`, 2 = `111`. Lengths 2, 2, 2, 3, 3 for the symbols 97, 98, 99,
100, 263 give 97 = `00`, 98 = `01`, 99 = `10`, 100 = `110`, 263 = `111`.

### 4.2 Codes that are not complete, and codes that are impossible

Let the *weight* of a code be the sum, over the symbols with a non-zero
length L, of 2 raised to the power (15 − L). A complete code has weight
32768.

- **Weight above 32768 (over-subscribed)**: the lengths cannot form a
  prefix code. This is corrupt input **as soon as the table is read**,
  whether or not the code would ever be used. (In the assignment
  procedure it shows as the running value reaching 2 to the power L
  while a symbol of length L still needs a code.) CONFIRMED (crafted)
  with `unar` for the pre-code, the main code and an unused length code;
  `bsdtar` accepts an over-subscribed code that is never used. Never
  seen in real data.
- **Weight below 32768 (incomplete)**: accepted. This is normal in real
  data in one form: a code with **no symbols at all** (among 8,983 real
  table transmissions the length code is empty in 2,689, the
  low-distance code in 473 and the distance code in 138: the block
  simply contains no item that would use it; the main code is never
  empty). It is also accepted for a code with a **single symbol**, whose
  length is then 1 and whose code is the single bit `0`, and for any
  other incomplete code (CONFIRMED (crafted), vector V16; in real data
  every non-empty code is complete).
- When, while decoding a symbol, the bits read match no assigned code
  (always the case for an empty code; the bit `1` for a single-symbol
  code; any unassigned pattern of an incomplete code), the result is
  corrupt input. CONFIRMED (crafted).

A decoder never needs more than 15 bits to decode one symbol.

## 5. Decoder state

One decoder state exists per solid group. Everything in it is set to the
initial value when a group starts (section 11.2) and is carried from
member to member inside the group.

| Item | Contents | Initial value |
|---|---|---|
| window | the most recent LZ output of the group (section 9) | empty |
| group count | number of LZ output bytes produced in this group so far (64-bit) | 0 |
| registers R0..R3 | the four most recently used match distances, most recent first (up to 2 to the power 32 for algorithm version 0 and 2 to the power 40 for version 1: 64-bit values) | none filled |
| filled-register count | how many of the registers hold a distance: 0..4 | 0 |
| last length | the final length of the most recent match of the kinds of sections 8.3 and 8.4 | 0, meaning "none" |
| the four codes | main, distance, low-distance and length code, as built by the most recent table transmission | none |
| table version | the algorithm version of the member that carried the most recent table transmission | none |

Not part of the carried state, and started afresh for every member:

- the position in the bit stream and everything about the current block
  (section 6);
- the member's start position, which is the group count at the moment
  the member starts (needed for filters, section 10);
- the filter queue (section 10.2), which must be empty when a member
  ends.

Unlike the older RAR methods there is **no array of code lengths kept
between table transmissions**: every transmission states all lengths
outright (section 7.3).

## 6. Blocks

The packed bytes of a member are a chain of **blocks**. Each block is a
header followed by data bytes; the header says how many data bytes
follow, how many bits of the last data byte are used, whether the data
begins with a table transmission and whether the block is the member's
last. There is no end-of-data symbol: a member ends where its last block
ends.

### 6.1 Block header

The header starts at a byte boundary: at byte 0 of the packed bytes for
the first block, and at the byte that follows the previous block's last
data byte for every other block.

| Byte | Field | Meaning |
|--:|---|---|
| 0 | flags | See the bit table below. |
| 1 | checksum | Must equal `0x5A` XOR the flags byte XOR every size byte. |
| 2.. | size | 1, 2 or 3 bytes, least significant byte first: **S**, the number of data bytes of the block (0..16,777,215). |

Bits of the flags byte (bit 0 is the least significant):

| Bits | Mask | Meaning |
|---|---|---|
| 0..2 | `0x07` | **k − 1**, where k (1..8) is the number of bits of the block's last data byte that belong to the block. |
| 3..4 | `0x18` | (number of size bytes) − 1. The values 0, 1 and 2 give 1, 2 and 3 size bytes. **The value 3 is corrupt input.** |
| 5 | `0x20` | Not used. DECISION: ignored. |
| 6 | `0x40` | 1: this is the **last block** of the member. |
| 7 | `0x80` | 1: the block's data begins with a **table transmission** (section 7). |

Rules:

- A wrong checksum is corrupt input. (CONFIRMED (crafted).)
- Running out of packed bytes inside a header is corrupt input
  (CONFIRMED (crafted) with `unar`).
- If S is greater than the number of packed bytes that remain after the
  header, that is corrupt input. (CONFIRMED (crafted) with `unar`.)
- Real encoders use one size byte for blocks under 256 bytes and two
  otherwise (147 and 8,859 of 9,006 real blocks; the largest real block
  has 31,423 data bytes). Three size bytes: CONFIRMED (crafted). A real
  encoder always uses the fewest size bytes that hold S; a decoder does
  not require that (CONFIRMED (crafted): a small block with a three-byte
  size is accepted by both tools).
- The value 3 in bits 3..4: both tools reject it (CONFIRMED (crafted)).
- Bit 5 is 0 in every real block. `unar` ignores it; `bsdtar` rejects a
  block in which it is set (section 17).

### 6.2 The extent of a block

Let h be the bit position just after the header (a multiple of 8).

- If S is 0 the block has **no data bits**; the field k is then
  meaningless and is ignored. (CONFIRMED (crafted); no real block has
  S = 0.)
- Otherwise the block's data bits are the bits h .. e − 1, where the
  **block end** e = h + 8 × (S − 1) + k. The remaining 8 − k bits of the
  last data byte are padding; they are zero in every real block and are
  not checked.
- The next block's header, if there is a next block, starts at bit
  h + 8 × S.

### 6.3 Decoding a block

1. Read and check the header.
2. If the tables flag is set, read a table transmission (section 7)
   starting at bit h. It replaces all four codes. **The transmission
   must end at or before e**; otherwise corrupt input (CONFIRMED
   (crafted)). It follows that the tables flag on a block with S = 0 is
   corrupt input (vector E25). The transmission may end exactly at e, leaving a block
   with tables and no items (CONFIRMED (real data): the 20-byte member
   of section 12.4).
3. While the current bit position is less than e: decode one item
   (section 8).
   - **An item needs codes.** If no table transmission has been read so
     far in this solid group (neither in this block, nor in an earlier
     block of this member, nor in an earlier member of the group), that
     is corrupt input (CONFIRMED (crafted)). The same applies when the
     codes in force were transmitted for the other algorithm version
     (section 11.2).
   - **Every bit of an item must lie before e.** If the position after
     an item is greater than e, that is corrupt input (DECISION: in all
     9,006 real blocks the last item ends exactly at e; both tools only
     test the position before each item and therefore accept an item
     that runs past e, section 15).
4. When the position equals e the block is done. (A block without data
   bits is done at once, also when no codes exist.)
5. If the last-block flag is set, the member's stream is finished
   (section 11.1). Otherwise continue with the next block header at bit
   h + 8 × S; if there are no packed bytes left for it, that is corrupt
   input (CONFIRMED (crafted) with `unar`; `bsdtar` accepts a chain that
   simply stops).

Remarks:

- The codes stay in force from block to block, and from member to
  member inside a solid group, until a block with the tables flag
  replaces them. In real data every block except, sometimes, the first
  block of a solid continuation member carries tables (8,983 of 9,006
  blocks; the 23 without are first blocks of continuation members). A
  block without tables in the middle of a member is CONFIRMED (crafted).
- Registers, last length, window and filter queue are not affected by
  block boundaries in any way.
- A real encoder starts a new block every few tens of kilobytes of
  packed data. The format sets no limit on the number of blocks; because
  each block costs at least 3 bytes, a member cannot contain more than
  P / 3 of them.

## 7. Table transmission

A table transmission states the code lengths of all four codes. It
consists of the pre-code lengths followed by the coded lengths.

### 7.1 Pre-code lengths

The pre-code has **20 symbols**, numbered 0..19. Their code lengths are
read one after another, starting with symbol 0, until all 20 are known:

- Read a 4-bit field v.
- If v is 0..14: the next symbol's length is v.
- If v is 15: read another 4-bit field z.
  - If z is 0: the next symbol's length is **15**.
  - Otherwise: the next **z + 2** symbols (3..17 symbols) have length 0.
    If fewer than z + 2 symbols remain, the run stops at symbol 19 and
    the excess is ignored (CONFIRMED (crafted)).

So a length of 15 costs 8 bits, and a run of zero lengths can be written
compactly. Real encoders use the zero run freely (7,868 times in 8,983
real transmissions; the member of section 12.4 contains one); a pre-code
length of 15 never occurs in the survey and is CONFIRMED (crafted).

The pre-code is then built as in section 4. An over-subscribed pre-code
is corrupt input. An incomplete pre-code is accepted (CONFIRMED
(crafted); real encoders always send a complete one).

### 7.2 Number of coded lengths

Let **N** be the total number of code lengths:

| Algorithm version | Main | Distance | Low-distance | Length | N |
|--:|--:|--:|--:|--:|--:|
| 0 | 306 | 64 | 16 | 44 | 430 |
| 1 | 306 | 80 | 16 | 44 | 446 |

The algorithm version is that of the member being decoded (section 2.1).
The size of the distance alphabet is the **only** difference between the
two versions; in particular N does not depend on the dictionary size.
(CONFIRMED (real data): members of version 1, including distances above
4 GiB coded with distance symbol 64, decode to the stored CRC-32 with 80
distance symbols, and fail with 64; members of version 0, and members of
version 1 with the compatibility bit, decode with 64 and fail with 80.)

### 7.3 The coded lengths

The decoder fills a list of N lengths, positions 0, 1, ..., N − 1 in
order. Let i be the next position to fill. Until i reaches N, decode one
pre-code symbol and act on it:

| Pre-code symbol | Extra bits read after it | Action |
|--:|--:|---|
| 0..15 | none | Position i receives the symbol's value as its length. i advances by 1. |
| 16 | 3, value r | The length stored at position i − 1 is copied into the next r + 3 positions (3..10 positions). **If i is 0 this is corrupt input.** |
| 17 | 7, value r | The length stored at position i − 1 is copied into the next r + 11 positions (11..138). **If i is 0 this is corrupt input.** |
| 18 | 3, value r | The next r + 3 positions (3..10) are set to 0. |
| 19 | 7, value r | The next r + 11 positions (11..138) are set to 0. |

Details:

- The lengths are **absolute**. Nothing of a previous transmission is
  used: there is no "keep" flag and no adding to old lengths.
- For symbols 16 and 17, "the position before" is simply position i − 1
  of the list, also when that position belongs to a different code (for
  example the last main-code length when i is 306). (CONFIRMED
  (crafted): in vector V4 a repeat run crosses from the main code into
  the distance code.)
- A run (symbols 16..19) that would go past position N − 1 is **cut off
  at N − 1**; the excess is ignored and the transmission is complete.
  (CONFIRMED (crafted) for a zero run and for a repeat run; never seen in
  real data. DECISION: accept and cut off.)
- Symbol 16 or 17 at position 0 is rejected by both tools (CONFIRMED
  (crafted)).
- The transmission ends when i reaches N; no terminator follows.

### 7.4 Building the codes

The list is split, in this order, into the lengths of the four codes,
and each is built as in section 4. An over-subscribed code is corrupt
input.

| Code | Positions (version 0) | Positions (version 1) | Symbols |
|---|---|---|--:|
| main code | 0..305 | 0..305 | 306 |
| distance code | 306..369 | 306..385 | 64 or 80 |
| low-distance code | 370..385 | 386..401 | 16 |
| length code | 386..429 | 402..445 | 44 |

The four new codes replace the previous ones completely.

## 8. Items

Inside a block, after the table transmission if there is one, items
follow until the block end. Each item starts with one symbol of the
**main code**:

| Main symbol | Kind | What follows in the stream | Section |
|--:|---|---|---|
| 0..255 | literal | nothing | 8.1 |
| 256 | filter declaration | two byte-counted integers, a 3-bit type, for one type 5 more bits | 8.7 |
| 257 | repeat last match | nothing | 8.6 |
| 258..261 | match with the distance in register R0..R3 | a length-code symbol, then its extra bits | 8.4 |
| 262..305 | match with a new distance | length extra bits, a distance-code symbol, directly-read distance bits, a low-distance-code symbol | 8.3 |

### 8.1 Literals

A main symbol 0..255 appends the byte whose value is the symbol to the
LZ output. Registers and last length are not touched.

### 8.2 Length slots

A match length is coded as a **length slot** 0..43 plus extra bits. The
same table serves main symbols 262..305 (slot = symbol − 262) and the 44
symbols of the length code (slot = symbol).

- Slots 0..7 have no extra bits; the length is slot + 2.
- For slot s of 8 or more: the number of extra bits is b = (s divided by
  4, rounded down) − 1, and the smallest length of the slot is
  (4 + (s mod 4)) × 2 to the power b, plus 2. The extra bits are read as
  one b-bit field and added.

Written out in full ("Lengths" is the value before the adjustment of
section 8.3):

| Slot | Main symbol | Extra bits | Lengths |
|--:|--:|--:|--:|
| 0 | 262 | 0 | 2 |
| 1 | 263 | 0 | 3 |
| 2 | 264 | 0 | 4 |
| 3 | 265 | 0 | 5 |
| 4 | 266 | 0 | 6 |
| 5 | 267 | 0 | 7 |
| 6 | 268 | 0 | 8 |
| 7 | 269 | 0 | 9 |
| 8 | 270 | 1 | 10..11 |
| 9 | 271 | 1 | 12..13 |
| 10 | 272 | 1 | 14..15 |
| 11 | 273 | 1 | 16..17 |
| 12 | 274 | 2 | 18..21 |
| 13 | 275 | 2 | 22..25 |
| 14 | 276 | 2 | 26..29 |
| 15 | 277 | 2 | 30..33 |
| 16 | 278 | 3 | 34..41 |
| 17 | 279 | 3 | 42..49 |
| 18 | 280 | 3 | 50..57 |
| 19 | 281 | 3 | 58..65 |
| 20 | 282 | 4 | 66..81 |
| 21 | 283 | 4 | 82..97 |
| 22 | 284 | 4 | 98..113 |
| 23 | 285 | 4 | 114..129 |
| 24 | 286 | 5 | 130..161 |
| 25 | 287 | 5 | 162..193 |
| 26 | 288 | 5 | 194..225 |
| 27 | 289 | 5 | 226..257 |
| 28 | 290 | 6 | 258..321 |
| 29 | 291 | 6 | 322..385 |
| 30 | 292 | 6 | 386..449 |
| 31 | 293 | 6 | 450..513 |
| 32 | 294 | 7 | 514..641 |
| 33 | 295 | 7 | 642..769 |
| 34 | 296 | 7 | 770..897 |
| 35 | 297 | 7 | 898..1025 |
| 36 | 298 | 8 | 1026..1281 |
| 37 | 299 | 8 | 1282..1537 |
| 38 | 300 | 8 | 1538..1793 |
| 39 | 301 | 8 | 1794..2049 |
| 40 | 302 | 9 | 2050..2561 |
| 41 | 303 | 9 | 2562..3073 |
| 42 | 304 | 9 | 3074..3585 |
| 43 | 305 | 9 | 3586..4097 |

The smallest length is 2 and the largest, before adjustment, 4097. All
44 slots occur in real data, each at least 292 times in the survey.

### 8.3 Symbols 262..305: match with a new distance

In this order:

1. The **length slot** is the symbol minus 262. Read its extra bits
   (section 8.2). This gives the unadjusted length.
2. Decode one symbol with the **distance code**: the **distance slot**
   (0..63 for algorithm version 0, 0..79 for version 1).
3. Work out the distance from the slot:
   - Slots 0..3 have no extra bits; the distance is slot + 1.
   - For slot s of 4 or more: the number of extra bits is b = (s divided
     by 2, rounded down) − 1, and the smallest distance of the slot is
     (2 + (s mod 2)) × 2 to the power b, plus 1.
   - If b is less than 4 (slots 4..9), read the b extra bits as one
     field.
   - If b is 4 or more (slots 10 and up), the extra value has two
     parts. First read **b − 4 bits directly** as one field (nothing is
     read when b is 4); this is the upper part. Then decode one symbol
     (0..15) with the **low-distance code**; this is the lower 4 bits.
     The extra value is (upper part × 16) + low symbol.
   - The distance is the slot's smallest distance plus the extra value.
4. **Adjust the length by the distance**: add 1 if the distance is
   greater than 256 (`0x100`), add 1 more if it is greater than 8,192
   (`0x2000`), and add 1 more if it is greater than 262,144 (`0x40000`).
   (Boundaries CONFIRMED (crafted): distances 256, 8,192 and 262,144
   take the smaller adjustment, 257, 8,193 and 262,145 the larger.)
5. Update the match memory: R3 takes the value of R2, R2 that of R1, R1
   that of R0, and R0 becomes the new distance; the filled-register
   count increases by 1 unless it is already 4. The **last length**
   becomes the adjusted length.
6. Copy (section 9).

The adjusted length is 2..4100 (4100 is CONFIRMED (crafted); the longest
real match is 4097 bytes).

Every low-distance symbol is decoded from the stream. Unlike the RAR 2.9
method (`rar29-lz.md`) this method keeps **no memory of the previous
low-distance value and no repeat count for it**, and the low-distance
alphabet has exactly 16 symbols, none of them special.

Distance slots written out in full. "Read directly" and "From
low-distance code" give how the extra bits are split. Slots 64..79 exist
only in algorithm version 1.

| Slot | Extra bits | Read directly | From low-distance code | Distances |
|--:|--:|--:|--:|--:|
| 0 | 0 | 0 | 0 | 1 |
| 1 | 0 | 0 | 0 | 2 |
| 2 | 0 | 0 | 0 | 3 |
| 3 | 0 | 0 | 0 | 4 |
| 4 | 1 | 1 | 0 | 5..6 |
| 5 | 1 | 1 | 0 | 7..8 |
| 6 | 2 | 2 | 0 | 9..12 |
| 7 | 2 | 2 | 0 | 13..16 |
| 8 | 3 | 3 | 0 | 17..24 |
| 9 | 3 | 3 | 0 | 25..32 |
| 10 | 4 | 0 | 4 | 33..48 |
| 11 | 4 | 0 | 4 | 49..64 |
| 12 | 5 | 1 | 4 | 65..96 |
| 13 | 5 | 1 | 4 | 97..128 |
| 14 | 6 | 2 | 4 | 129..192 |
| 15 | 6 | 2 | 4 | 193..256 |
| 16 | 7 | 3 | 4 | 257..384 |
| 17 | 7 | 3 | 4 | 385..512 |
| 18 | 8 | 4 | 4 | 513..768 |
| 19 | 8 | 4 | 4 | 769..1,024 |
| 20 | 9 | 5 | 4 | 1,025..1,536 |
| 21 | 9 | 5 | 4 | 1,537..2,048 |
| 22 | 10 | 6 | 4 | 2,049..3,072 |
| 23 | 10 | 6 | 4 | 3,073..4,096 |
| 24 | 11 | 7 | 4 | 4,097..6,144 |
| 25 | 11 | 7 | 4 | 6,145..8,192 |
| 26 | 12 | 8 | 4 | 8,193..12,288 |
| 27 | 12 | 8 | 4 | 12,289..16,384 |
| 28 | 13 | 9 | 4 | 16,385..24,576 |
| 29 | 13 | 9 | 4 | 24,577..32,768 |
| 30 | 14 | 10 | 4 | 32,769..49,152 |
| 31 | 14 | 10 | 4 | 49,153..65,536 |
| 32 | 15 | 11 | 4 | 65,537..98,304 |
| 33 | 15 | 11 | 4 | 98,305..131,072 |
| 34 | 16 | 12 | 4 | 131,073..196,608 |
| 35 | 16 | 12 | 4 | 196,609..262,144 |
| 36 | 17 | 13 | 4 | 262,145..393,216 |
| 37 | 17 | 13 | 4 | 393,217..524,288 |
| 38 | 18 | 14 | 4 | 524,289..786,432 |
| 39 | 18 | 14 | 4 | 786,433..1,048,576 |
| 40 | 19 | 15 | 4 | 1,048,577..1,572,864 |
| 41 | 19 | 15 | 4 | 1,572,865..2,097,152 |
| 42 | 20 | 16 | 4 | 2,097,153..3,145,728 |
| 43 | 20 | 16 | 4 | 3,145,729..4,194,304 |
| 44 | 21 | 17 | 4 | 4,194,305..6,291,456 |
| 45 | 21 | 17 | 4 | 6,291,457..8,388,608 |
| 46 | 22 | 18 | 4 | 8,388,609..12,582,912 |
| 47 | 22 | 18 | 4 | 12,582,913..16,777,216 |
| 48 | 23 | 19 | 4 | 16,777,217..25,165,824 |
| 49 | 23 | 19 | 4 | 25,165,825..33,554,432 |
| 50 | 24 | 20 | 4 | 33,554,433..50,331,648 |
| 51 | 24 | 20 | 4 | 50,331,649..67,108,864 |
| 52 | 25 | 21 | 4 | 67,108,865..100,663,296 |
| 53 | 25 | 21 | 4 | 100,663,297..134,217,728 |
| 54 | 26 | 22 | 4 | 134,217,729..201,326,592 |
| 55 | 26 | 22 | 4 | 201,326,593..268,435,456 |
| 56 | 27 | 23 | 4 | 268,435,457..402,653,184 |
| 57 | 27 | 23 | 4 | 402,653,185..536,870,912 |
| 58 | 28 | 24 | 4 | 536,870,913..805,306,368 |
| 59 | 28 | 24 | 4 | 805,306,369..1,073,741,824 |
| 60 | 29 | 25 | 4 | 1,073,741,825..1,610,612,736 |
| 61 | 29 | 25 | 4 | 1,610,612,737..2,147,483,648 |
| 62 | 30 | 26 | 4 | 2,147,483,649..3,221,225,472 |
| 63 | 30 | 26 | 4 | 3,221,225,473..4,294,967,296 |
| 64 | 31 | 27 | 4 | 4,294,967,297..6,442,450,944 |
| 65 | 31 | 27 | 4 | 6,442,450,945..8,589,934,592 |
| 66 | 32 | 28 | 4 | 8,589,934,593..12,884,901,888 |
| 67 | 32 | 28 | 4 | 12,884,901,889..17,179,869,184 |
| 68 | 33 | 29 | 4 | 17,179,869,185..25,769,803,776 |
| 69 | 33 | 29 | 4 | 25,769,803,777..34,359,738,368 |
| 70 | 34 | 30 | 4 | 34,359,738,369..51,539,607,552 |
| 71 | 34 | 30 | 4 | 51,539,607,553..68,719,476,736 |
| 72 | 35 | 31 | 4 | 68,719,476,737..103,079,215,104 |
| 73 | 35 | 31 | 4 | 103,079,215,105..137,438,953,472 |
| 74 | 36 | 32 | 4 | 137,438,953,473..206,158,430,208 |
| 75 | 36 | 32 | 4 | 206,158,430,209..274,877,906,944 |
| 76 | 37 | 33 | 4 | 274,877,906,945..412,316,860,416 |
| 77 | 37 | 33 | 4 | 412,316,860,417..549,755,813,888 |
| 78 | 38 | 34 | 4 | 549,755,813,889..824,633,720,832 |
| 79 | 38 | 34 | 4 | 824,633,720,833..1,099,511,627,776 |

Evidence for the table: slots 0..41 at both ends of each range are
CONFIRMED (crafted) with both tools (vector V7) and occur throughout the
real data; slots 42..48, 52, 53, 57 and 61..63 occur in real version 0
members and slot 64 in a real version 1 member, all with matching
CRC-32; the largest real distance is 4,617,928,704. Slot 50 (its
smallest distance) and slot 59 (its largest) are CONFIRMED (crafted)
with `unar` (vector V8). Slots 49, 51, 54..56, 58, 60 and 65..79 are
INFERRED from the rule, which every tested slot obeys.

Arithmetic: a distance of algorithm version 0 can be 4,294,967,296,
which does not fit in 32 bits; version 1 needs 41 bits. Distances and
the group count must be held in 64-bit integers.

### 8.4 Symbols 258..261: match with a register distance

1. Let j be the symbol minus 258 (0..3). The distance is the value of
   register Rj. **If j is not less than the filled-register count, that
   is corrupt input** (DECISION, section 8.5).
2. Decode one symbol with the **length code**: a length slot 0..43. Read
   its extra bits (section 8.2). The length is the slot's length;
   **no adjustment by distance is applied** (CONFIRMED (crafted) with a
   distance of 300, where an adjustment would have added 1).
3. Move the register to the front: the values of R0..R(j−1) each move
   one place back (R(j−1) into Rj, ..., R0 into R1) and R0 becomes the
   distance just used. For j = 0 nothing changes. Registers behind Rj
   are not touched.
4. The **last length** becomes this length.
5. Copy (section 9).

### 8.5 The match memory in detail

- The registers are filled only by matches with a new distance (8.3),
  one per match, from the front. Symbols 258..261 reorder them; symbol
  257 and literals leave them alone. Nothing is searched or removed: the
  same distance may sit in several registers.
- Consequences, all CONFIRMED (crafted): after new-distance matches with
  the distances 3, 5, 7, 9 in that order, R0..R3 hold 9, 7, 5, 3. A
  symbol 260 (register R2) then copies with distance 5 and leaves 5, 9,
  7, 3. A symbol 258 (R0) changes nothing in the registers.
- **Unfilled registers.** DECISION: selecting a register that no match
  has filled yet in this solid group is corrupt input. (Never seen in
  real data. In the reference implementation such a register holds 0 and
  the copy then reads stale window contents; `bsdtar` outputs zero bytes
  for it.) Because registers fill from the front and reordering only
  permutes filled places, "Rj is filled" is the same as "j is less than
  the number of new-distance matches decoded so far in the group, capped
  at 4".
- The last length is set by 8.3 (the *adjusted* length) and by 8.4. It
  is not changed by symbol 257.
- Registers and last length survive block boundaries, table
  transmissions and member boundaries inside a solid group.

### 8.6 Symbol 257: repeat the last match

- If the last length is 0 (no match of the kinds 8.3 or 8.4 has been
  decoded in this group), the symbol **does nothing**: no output, no
  state change. (CONFIRMED (crafted) with both tools, vector V12; never
  seen in real data.)
- Otherwise it is a match with the distance in R0 and the last length,
  used unchanged (no adjustment is applied again). The registers and the
  last length stay as they are. Copy (section 9).

A non-zero last length implies that R0 is filled.

Symbol 257 is frequent in real data (4.7 million uses in the survey
against 26.8 million new-distance matches and 3.5 million register
matches).

### 8.7 Symbol 256: filter declaration

The symbol announces that a range of the LZ output **which has not been
produced yet** must be transformed before it is delivered (section 10).
It produces no output itself. Following the symbol, in this order:

| Field | Coding |
|---|---|
| block start offset | byte-counted integer (below) |
| block length | byte-counted integer |
| type | 3-bit field |
| channels − 1 | 5-bit field, **present only when type is 0** |

A **byte-counted integer** is a 2-bit field c followed by c + 1 fields
of 8 bits each (1..4 bytes). The first 8-bit field is the least
significant byte of the value, the second the next more significant, and
so on. The value is 0..4,294,967,295. Each 8-bit field is read like any
other field, most significant bit first, at the current bit position.
(All sixteen combinations of 1..4 bytes for the two integers: CONFIRMED
(crafted). A value need not use the fewest bytes.)

Meaning:

- The filter's **start** is the group count at the moment the symbol is
  decoded (the LZ position of the next byte to be produced) plus the
  block start offset. Its range is the **length** LZ output bytes from
  there.
- **Type** selects the transformation: 0 delta, 1 x86 E8, 2 x86 E8/E9,
  3 ARM (section 10.3). For type 0 the channel count is the 5-bit value
  plus 1 (1..32).

Checks, each made when the declaration is read (all DECISION except
where noted; section 10.2 explains the queue):

- Type 4..7 is corrupt input. (CONFIRMED (crafted): both tools reject.)
- Length 0 is corrupt input. (Never seen in real data, where the
  shortest filter has 256 bytes; both tools fail on it.)
- Length greater than **4,194,304** (4 MiB, `0x400000`) is corrupt
  input. (The longest real filter has 65,536 bytes. `bsdtar` has exactly
  this limit; `unar` has none, section 15.)
- If the start is less than the end (start + length) of the filter
  declared most recently before it in the same member, that is corrupt
  input: ranges must be declared in ascending order and must not
  overlap. A start equal to the previous end is allowed (in real data
  5,655 of 7,590 filters start exactly where the previous one ends).
  (It makes no difference whether that earlier filter has already been
  applied: if it has, its end is not above the group count and the new
  start cannot be below it.)
- If **8,192** filters are pending, that is corrupt input. A filter is
  *pending* from its declaration until the last byte of its range has
  been produced, that is, while its end is greater than the group
  count. The count is therefore a property of the stream and must not
  depend on when an implementation happens to hand LZ output to the
  output stage (section 9.4): an implementation whose queue can still
  hold filters whose ranges are complete must apply them, or leave them
  out of the count, before it applies this limit. (The largest number
  pending at once in real data is 52. Vectors V19 and E26.)

Otherwise the filter is appended to the queue.

In real data the block start offset is at most 2,744,320 and the
declaration always precedes the first byte of its range, as the format
requires by construction (the offset cannot be negative).

## 9. The window

### 9.1 What a match copies

A match of distance d and length n appends n bytes to the LZ output,
each equal to the byte d positions before it in the group's LZ output.
The copy proceeds **one byte at a time in increasing order**, so source
and destination may overlap: a distance smaller than the length repeats
a pattern (distance 1 repeats the previous byte n times). Literals and
copied bytes alike become part of the history. The history is the LZ
output, **before** filtering: a filter never changes what later matches
see (section 10).

### 9.2 Which distances are valid

Before copying, every match (of the kinds 8.3, 8.4 and 8.6) is checked
once, against the state at the start of the match. The checks are made
**in this order, and the first one that fails decides the result**:

1. **d must not exceed the group count.** A larger distance would refer
   to data before the start of the group: corrupt input. A distance
   *equal* to the group count is valid and copies from the first byte of
   the group (vector V1: distance 3 after 3 bytes). (DECISION. Never
   seen in real data. Both tools read zero bytes instead, section 15.)
2. **d must not exceed D**, the dictionary size declared by the member
   being decoded. Otherwise corrupt input. (DECISION. In real data no
   member uses a distance above its own D, also where members of one
   group declare different sizes; in fact the largest distances stay
   some 4,400 bytes below D, for example 126,703 with D = 131,072 and
   1,044,191 with D = 1,048,576. A distance equal to D is accepted:
   CONFIRMED (crafted) with `bsdtar` for 131,072 and with `unar` for
   1,073,741,824; `unar` mis-decodes distances within about 4 KiB of a
   small D, section 15.)
3. **d must not exceed the window limit M** (9.3). Otherwise the result
   is *unsupported* ("dictionary too large"), not corrupt input. (So a
   distance that is within the group count and within D but beyond M is
   *unsupported*, although the bytes it names are no longer held.)
4. **d must not exceed the number of history bytes the window still
   holds.** After checks 1 to 3 this can fail in one situation only: the
   group's window capacity was raised by a later member after older
   bytes had already been discarded (9.3). Corrupt input. (DECISION;
   crafted archives only.)

### 9.3 Size of the window, and the largest size accepted

The format lets a member declare D from 128 KiB to 4 GiB (algorithm
version 0) or to about 2 TiB (version 1; the vendor's archiver writes up
to 64 GiB). A decoder cannot promise that much memory, and must not
allocate what a header merely claims.

DECISION, in four parts:

- **Window limit.** The decoder has a configurable limit M on the window
  it will hold. The default is **1 GiB (1,073,741,824 bytes)**. An
  embedding application may set it lower (down to 128 KiB) or higher (up
  to what the platform can address); nothing else in this document
  depends on the value. (The vendor's own extractor has a limit of the
  same kind: its manual states that by default it refuses to unpack
  archives with a dictionary exceeding 4 GiB "to prevent the unexpected
  large memory allocation" unless a switch raises the limit.)
- **Declared sizes above the limit are not rejected up front.** A member
  whose D exceeds M is decoded with a window of M bytes. It decodes
  correctly as long as no match reaches further back than M; the first
  match that does fails as *unsupported* (9.2, rule 3). (Reason: the
  declared size says how far matches may reach, not how far they do.)
- **Group dictionary size.** Within a solid group the window capacity
  is governed by the largest D declared by any member of the group
  decoded so far, capped at M. When a continuation member declares a
  larger D than its predecessors the capacity rises from that member on;
  history already discarded does not come back (check 4 of 9.2 covers
  that case). When it declares a smaller D the capacity stays. (Real
  archives contain both: after files were added to a solid archive with
  a larger dictionary, the archiver rewrote the first member's header to
  the larger size, and a later member carries a smaller one. `unar`
  refuses a member whose D exceeds that of the group's first member;
  `bsdtar` accepts it.)
- **Memory follows the data, not the header.** The window is allocated
  step by step as LZ output is produced: at any moment its allocation
  need not, and must not, exceed the larger of 256 KiB and twice the
  group count, and never exceeds the capacity. A 60-byte archive
  declaring a 1 GiB dictionary therefore costs 256 KiB.

How the window is stored is up to the implementation. The usual form is
a ring: the byte with LZ position t lives at index t mod C, where C is
the capacity. Three points need care:

- C need not be a power of two (version 1 sizes, and M itself, may not
  be), so the index is a true remainder, not a bit mask.
- A distance equal to C reads the index that is about to be written;
  each byte must be read before its position is overwritten.
- While the group count is below the capacity, no wrap has happened yet
  and the window is simply the whole LZ output so far, which makes
  growing the allocation a plain extension.

The number of history bytes the window holds (check 4 of 9.2) is the
smaller of the group count and the capacity, with one refinement for a
capacity that rises in mid-group after the ring has already wrapped:
then only the bytes actually retained are held, and the number grows
again by one per output byte up to the new capacity. As long as the
capacity has never risen after a wrap, checks 1 to 3 already imply
check 4 (the capacity is the smaller of M and the largest D so far, and
d exceeds neither).

The window is not cleared and the group count is not reset between the
members of a solid group; both start afresh with each new group. Stored
members and empty members of a solid group add nothing to the window or
the count (section 11.2).

### 9.4 Handing LZ output on

Every LZ output byte must be handed to the filter stage (section 10)
exactly once, in order, **before its place in the window is reused**.
Handing over whenever 64 KiB or so has accumulated, and at the end of
the member, satisfies this for every permitted capacity. The filter
stage keeps its own copy of bytes it still needs; it never reads the
window, so a filter range may be longer than the window (CONFIRMED
(crafted) with `unar`: a 256 KiB filter with D = 128 KiB; `bsdtar` does
not terminate on that archive).

## 10. Filters

A filter marks a range of the LZ output that the encoder transformed
before compressing it, to make it more compressible; the decoder applies
the inverse transformation to the range before delivering it. There are
four fixed transformations. Filters do not nest and do not overlap; each
LZ output byte is covered by at most one.

### 10.1 Positions

A filter's range is given in LZ positions of the solid group: start and
length, as computed in section 8.7. Two transformations also depend on
the **file offset** of the range, which is

> file offset of the range = filter start − member start position,

the member start position being the group count at the moment the member
began (0 for the first or only member of a group). In other words the
offset of the range's first byte inside the member output.
(CONFIRMED (real data): x86 filters in later members of solid groups.
CONFIRMED (crafted): a filter in the second member of a group decodes
correctly with the offset counted from the member's start and wrongly
with the offset counted from the group's start.)

### 10.2 The queue and the output stage

The decoder keeps a queue of declared filters whose ranges are not yet
complete, in the order of declaration, which by the checks of section
8.7 is also ascending order of position without overlap.

The output stage receives the member's LZ output in order (9.4) and
turns it into the member output:

- A byte whose LZ position is below the start of the first queued filter
  (or any byte, when the queue is empty) is passed through unchanged.
- A byte inside the range of the first queued filter is collected in a
  buffer. When the buffer holds the whole range (length bytes), the
  transformation is applied to the buffer as one unit, the result is
  passed on in place of the original bytes, and the filter leaves the
  queue. The next queued filter, if any, may start at the very next
  byte.
- The buffer grows as bytes arrive; it is never larger than 4 MiB
  (section 8.7) and is not allocated from the declared length in
  advance.

A declaration names only positions that have not been produced yet
(its start is the current group count or later), so the LZ output bytes
that already exist when it is decoded are never part of its range. The
output stage may therefore be given bytes at any time after they are
produced, early or late; it never has to take a byte back.

At the end of the member (section 11.1) the queue must be empty.
DECISION: a filter that is still queued then, because its range extends
past the last LZ output byte of the member or starts at or beyond it, is
corrupt input. A filter range never extends from one member of a solid
group into the next. (In all real members the queue is empty at the end.
`unar` and `bsdtar` silently drop a filter whose start is never
reached; for one that is cut short `unar` reports an error; for one
declared in one member with a range in the next, `unar` applies it there
and `bsdtar` drops it. Section 15.)

The count of member output bytes equals the count of LZ output bytes:
every transformation maps n bytes to n bytes.

### 10.3 The four transformations

In all four, the input is the n collected bytes of the range (n is the
filter length, 1..4,194,304), positions are counted from 0 within the
range, F is the file offset of the range (10.1), and the result has n
bytes.

#### Type 0: delta

Parameter: the channel count c (1..32).

The n input bytes are **not** interleaved: they hold all bytes of
channel 0 first, then all bytes of channel 1, and so on. The output
interleaves the channels again: channel j owns the output positions j,
j + c, j + 2c, ... below n.

Take the input bytes in order, one after another. For channel 0, then
channel 1, ..., then channel c − 1: start with a previous value of 0;
for each output position of the channel in ascending order, take the
next input byte, subtract it from the previous value modulo 256, store
the result at that output position, and make it the new previous value.

- Channel j receives as many bytes as it has positions: the number of
  integers p with 0 ≤ p < n and p mod c = j. When n is not a multiple of
  c the first channels get one byte more; when n is less than c the
  channels n..c − 1 get none.
- The output must not overwrite input that is still to be read: use a
  second buffer of n bytes.
- F is not used.

Example (section 12.5 traces it): c = 3, input `0a f6 05 fb 01 02 03 00
ff ff 07 09 04` gives `f6 fe 01 00 fb fa fb fb f1 00 fc ed ff`.

Channel counts 1, 2, 3, 4, 6, 7, 8, 9, 11, 12, 16, 24, 25, 31 and 32
occur in real data (2,418 delta filters); all others behave the same
(CONFIRMED (crafted) for 1, 2, 3, 4, 7, 31, 32 and for n smaller than
c).

#### Type 1: x86 E8, and type 2: x86 E8/E9

These undo the conversion of the relative operand of x86 `CALL`
(opcode byte `0xE8`) and, for type 2 only, `JMP` (opcode byte `0xE9`)
instructions into absolute form. The transformation is done in place.

If n is less than 5 the data is unchanged. Otherwise scan with an index
i starting at 0, **while i ≤ n − 5**:

- If the byte at i is not an opcode byte (`0xE8`; for type 2 also
  `0xE9`), advance i by 1.
- If it is, let A be the 32-bit unsigned value formed from the four
  bytes at i + 1 .. i + 4, least significant byte first, and let

  > c = (F + i + 1) mod 16,777,216

  (the file offset of the byte after the opcode, reduced modulo 2 to the
  power 24; compute F + i + 1 in 64 bits). Then:

  | Condition on A | New value of the four bytes |
  |---|---|
  | A ≥ 2,147,483,648 (`0x80000000`) and A + c ≥ 4,294,967,296 | A + 16,777,216 − 4,294,967,296 |
  | A ≥ 2,147,483,648 and A + c < 4,294,967,296 | unchanged |
  | A < 16,777,216 (`0x1000000`) | (A − c) mod 4,294,967,296 |
  | 16,777,216 ≤ A < 2,147,483,648 | unchanged |

  The new value is stored least significant byte first. In terms of A
  read as a signed 32-bit number: a negative A is replaced by
  A + 16,777,216 when A + c is not negative; a non-negative A below
  16,777,216 is replaced by A − c.

  **Whether or not the bytes were changed, advance i by 5**: the four
  operand bytes are never examined as opcodes.

Consequences: an opcode byte in the last four bytes of the range is not
converted (its operand would not fit); the bytes of a converted or
unconverted operand are skipped even if they are `0xE8`.

Evidence. 4,820 type 1 and 352 type 2 filters in real data, at file
offsets up to just below 2 GiB, including offsets on both sides of
16,777,216, which fixes the reduction modulo 2 to the power 24. The
vendor's archiver emitted no x86 filter at a file offset of 2 GiB or
more, even when told to filter everything in a 4 GiB file; for such
offsets the rule above is CONFIRMED (crafted) with `bsdtar` only,
`unar` producing different bytes there (section 15). Every row of the
table, type 2 against type 1, n = 4, 5 and 6, and an opcode in the last
four bytes: CONFIRMED (crafted) with both tools.

#### Type 3: ARM

This undoes the conversion of the 24-bit operand of 32-bit ARM `BL`
instructions (little-endian words whose most significant byte is
`0xEB`). In place.

If n is less than 4 the data is unchanged. Otherwise for i = 0, 4, 8,
... **while i ≤ n − 4**: if the byte at i + 3 equals `0xEB`, let V be
the 24-bit value formed from the bytes at i, i + 1, i + 2, least
significant byte first; replace it by

> (V − Q) mod 16,777,216, where Q = (F + i) divided by 4, rounded down

stored back into the same three bytes, least significant byte first. The
byte at i + 3 is not changed. Compute F + i in 64 bits; because the
result is reduced modulo 2 to the power 24, only bits 2..25 of F + i
matter, so reducing F or F + i modulo 2 to the power 32 first (as the
reference implementation does) gives the same bytes at every offset.
Words are aligned to the **start of the range**, not to the file: i
always advances by 4 from 0, whatever F is.
Up to three bytes at the end of the range that do not form a whole word
are left alone.

Evidence. Neither RAR 5.91 nor RAR 7.23 could be made to emit this
filter, even for ARM code; no type 3 filter occurs in the survey. The
rule is that of the reference implementation, CONFIRMED (crafted) with
both tools for F = 0, 4, 1000, 1001, 1002 and 1003 (so (F + i) / 4 is
rounded down), for an operand that wraps below zero and for n = 4; for
n = 3 with `unar` only (`bsdtar` refuses every filter shorter than 4
bytes). File offsets of 64 MiB and more, where F + i no longer fits in
the 26 bits that matter, are REFERENCE-ONLY (section 13.3 gives
expected bytes for such offsets, computed from the rule).

## 11. Members, solid groups, volumes, end of data

### 11.1 One member

A member's stream is the chain of blocks of section 6. **It ends with
the block that has the last-block flag**; nothing in the item stream
marks the end, and U plays no part in finding it.

When the last block is done:

1. All remaining LZ output is handed to the output stage (9.4).
2. The filter queue must be empty (10.2), else corrupt input.
3. If U is known, the number of member output bytes must equal U, else
   corrupt input (DECISION; `unar` silently cuts a longer output down to
   U and fails on a shorter one).
4. Whatever follows the last block in the packed bytes is neither read
   nor checked (DECISION; real members have nothing there; `unar`
   ignores extra bytes, `bsdtar` reports an error after delivering the
   file).

During decoding, DECISION: as soon as the member's LZ output exceeds U
(U known), stop with corrupt input rather than producing the surplus;
and the caller's own limits on the size of a member and of an archive
apply as for every other format, in particular when U is unknown.

Special cases:

- **P = 0.** There is no stream and the member output is empty. If U is
  known and not 0, that is corrupt input. With the solid flag set the
  decoder state is not touched; real archives contain such members in
  the middle of solid groups (method 1..5, U = 0, P = 0, solid flag set:
  an empty file). With the solid flag clear the member starts a new
  group like any other compressed member without the flag (section
  11.2), whose state is then fresh and without codes (REFERENCE-ONLY;
  not seen in real data: in an archive that is not solid both archivers
  store an empty file with method 0, which never reaches this decoder,
  and in a solid archive an empty first member has P greater than 0,
  next case).
- **U = 0 with P greater than 0.** Decoded like any other member: the
  block chain is read and must produce no output. Real archives contain
  this as the **first member of a solid group**: an empty file stored
  with method 3 and 20 packed bytes that hold one block with a table
  transmission and no items; the next member's first block then has no
  tables (section 12.4). RAR 5.91 and RAR 7.23 both write this form.

### 11.2 Solid groups

The solid flag of the compression information field (section 2.1) on a
compressed member means: **this member continues the decoder state left
by the preceding compressed member**. A compressed member without the
flag starts a new group with a fresh state (section 5). The archive-level
"solid" flag of the main header is not consulted.

What a continuation member inherits (section 5): the window and group
count, the registers and their filled count, the last length, and the
four codes together with the algorithm version they were transmitted
for. What it does not inherit: the bit position (it starts at bit 0 of
its own packed bytes with a block header), and filters (the queue is
empty at every member boundary).

Processing one group, member by member, in archive order:

1. **First member** (compressed, solid flag clear). Create a fresh
   state. Decode its block chain. Its first block that has data bits
   must carry tables (section 6.3).
2. **Continuation member** (compressed, solid flag set). Record the
   member start position (the current group count). Raise the window
   capacity if this member's D is larger (9.3). Decode its block chain
   from bit 0 of its packed bytes. Its first block **may or may not**
   carry tables; without them the inherited codes are used. In real
   archives 23 continuation members start without tables, all of them
   small (for example 7, 10 and 14 packed bytes).
3. **Algorithm version inside a group.** Members of one group may differ
   in algorithm version (CONFIRMED (real data): the archiver appended
   version 1 members to a version 0 group, marking the old members with
   the compatibility bit). The window, the count, the registers and the
   last length carry over regardless. The codes carry over only between
   members of the same algorithm version: DECISION: an item decoded
   while the codes in force were transmitted for a different algorithm
   version than that of the current member is corrupt input (in the real
   archive the first block of the first version 1 member carries
   tables; vectors V18 and E27).

Members that are not decoded by this decoder but appear between the
members of a group:

- **Stored members** (method 0) are delivered as stored. They do **not**
  enter the window, do not advance the group count and do not change any
  state, whatever their solid flag says; the next compressed member with
  the solid flag continues from the last *compressed* member.
  (CONFIRMED (real data): a solid archive to which stored files were
  added in the middle; the compressed member after them reaches back
  across them with distances that count only compressed members'
  output. In that archive the stored members carry the solid flag.
  CONFIRMED (crafted) with `bsdtar` for a stored member with and without
  the flag; `unar` fails on both, section 15.) DECISION: a stored member
  never starts or ends a group.
- **Empty members with P = 0 and the solid flag set** (11.1),
  **directory entries** and **service headers** have no effect on the
  state.
- DECISION: a compressed member with the solid flag for which no state
  exists (it is the first compressed member seen, as happens when a
  later volume of a set is opened alone, or when the archive's first
  compressed member carries the flag) is reported as an error for that
  member ("solid predecessor unavailable") without decoding. (Both tools
  fail on a crafted archive whose only member has the flag.)

### 11.3 What survives, summary

| | Next block of the same member | Next member of the same group | New group |
|---|---|---|---|
| bit position | continues at the next byte boundary | restarts at bit 0 of the new packed bytes | restarts |
| the four codes | kept until a block with tables | kept until a block with tables (same algorithm version only) | none |
| window, group count | kept | kept | empty, 0 |
| registers, filled count, last length | kept | kept | none, 0, 0 |
| filter queue | kept | must be empty | empty |
| member start position | unchanged | set to the group count | 0 |

### 11.4 Failures inside a group

DECISION: when a member of a group ends in corrupt input or
*unsupported* (or is encrypted and cannot be deciphered, or cannot be
read at all), the state is no longer trustworthy. Every later member
that continues the group is reported as failed without being decoded
("depends on a damaged member"), up to the next compressed member that
starts a new group. A checksum mismatch alone, with the stream otherwise
decoded completely, does not have this effect.

To deliver any member of a group, all compressed members before it in
the group must be decoded, in order, even if the caller does not want
them.

### 11.5 Files split across volumes

A member may be split over several volumes; each piece has its own file
header (the container document describes the flags and how the pieces
are recognised and ordered). For this decoder:

- The packed bytes of the member are the pieces' data areas
  **concatenated in volume order**. The pieces are cut at arbitrary byte
  positions of one continuous chain of blocks: nothing is aligned,
  repeated or reset at a volume boundary, a block (header or data) may
  straddle it, and the decoder cannot tell where the boundaries were.
  (CONFIRMED (real data): three sets of 12, 12 and 18 volumes, members
  in up to 7 pieces, solid and not.)
- A solid group can extend across volumes in the same way.
- If a piece is missing the member cannot be decoded: corrupt input (or
  a container-level error), and section 11.4 applies.

### 11.6 End of data, summary

| Situation | Result |
|---|---|
| Last block done, queue empty, output count equals U (or U unknown) | Success. Remaining packed bytes are ignored. |
| A bit is needed beyond the packed bytes, or beyond the block end | Corrupt input. |
| Packed bytes end where a block header is expected (no last block seen) | Corrupt input. |
| Output count would exceed U | Corrupt input (DECISION). |
| Last block done, output count below U | Corrupt input. |
| Last block done, a filter still queued | Corrupt input (DECISION). |
| P = 0, U = 0 or unknown | Empty member; state untouched if the solid flag is set, fresh group otherwise. |
| P = 0, U greater than 0 | Corrupt input. |

## 12. Worked examples

### 12.1 One block: `abcabc`

Packed bytes (21 bytes), algorithm version 0, U = 6:

`c6 8e 12 01 20 00 00 00 00 00 00 00 02 eb 55 ff c7 5a 17 b8 6c`

In binary, grouped by byte:

`11000110 10001110 00010010 00000001 00100000 00000000 00000000 00000000 00000000 00000000 00000000 00000000 00000010 11101011 01010101 11111111 11000111 01011010 00010111 10111000 01101100`

Block header (bytes 0..2):

| Byte | Value | Meaning |
|--:|---|---|
| 0 | `c6` = `11000110` | bit 7 = 1: tables; bit 6 = 1: last block; bit 5 = 0; bits 3..4 = `00`: one size byte; bits 0..2 = `110` = 6: k = 7 |
| 1 | `8e` | checksum: `5a` XOR `c6` XOR `12` = `8e` |
| 2 | `12` | S = 18 data bytes |

The data bits are bits 24 .. 166 (h = 24, e = 24 + 8 × 17 + 7 = 167).

| Bits | Value | Meaning |
|--:|---|---|
| 24..103 | twenty 4-bit fields `0 1 2 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 2` | pre-code lengths: symbol 1 = 1, symbol 2 = 2, symbol 19 = 2, all others 0 (no field is 15, so no escape occurs). Pre-code: 1 = `0`, 2 = `10`, 19 = `11` |
| 104..105 | `11` | pre-code symbol 19 |
| 106..112 | `1010110` = 86 | 86 + 11 = 97 zeros: positions 0..96 |
| 113..114 | `10` | symbol 2: position 97 = 2 |
| 115..116 | `10` | position 98 = 2 |
| 117..118 | `10` | position 99 = 2 |
| 119..120 | `11` | symbol 19 |
| 121..127 | `1111111` = 127 | 138 zeros: positions 100..237 |
| 128..129 | `11` | symbol 19 |
| 130..136 | `0001110` = 14 | 25 zeros: positions 238..262 |
| 137..138 | `10` | symbol 2: position 263 = 2 |
| 139..140 | `11` | symbol 19 |
| 141..147 | `0100001` = 33 | 44 zeros: positions 264..307 |
| 148 | `0` | symbol 1: position 308 = 1 |
| 149..150 | `11` | symbol 19 |
| 151..157 | `1101110` = 110 | 121 zeros: positions 309..429. N = 430 reached. |

Codes: main code from positions 0..305: symbols 97, 98, 99, 263 with
length 2, so 97 = `00`, 98 = `01`, 99 = `10`, 263 = `11`. Distance code
from positions 306..369: position 308 is distance symbol 2, length 1,
code `0`. Low-distance code and length code: empty.

| Bits | Value | Meaning |
|--:|---|---|
| 158..159 | `00` | main 97: literal `a` |
| 160..161 | `01` | main 98: literal `b` |
| 162..163 | `10` | main 99: literal `c` |
| 164..165 | `11` | main 263: length slot 1, no extra bits, length 3 |
| 166 | `0` | distance symbol 2: slot 2, no extra bits, distance 3. No adjustment (3 is not greater than 256). |

The match copies 3 bytes from 3 back: `abc`. The position is now 167 =
e, so the block is done; it is the last block. Bit 167 is padding.
Output `61 62 63 61 62 63` (`abcabc`), CRC-32 `0x726E994C`. Afterwards
R0 = 3, one register filled, last length 3.

### 12.2 Every kind of LZ item

Packed bytes (61 bytes), algorithm version 0, U = 93:

`c2 a2 3a 33 33 33 00 00 00 00 00 30 44 f2 d3 cf 4c 3c 3c ba ff ff 03 71 17 0e 37 95 f2 6b 82 01 30 fd 41 08 05 dd 61 e7 55 8d 73 e1 19 4e 9e 28 91 e8 dc 13 99 f3 7d eb 26 3b b6 aa 00`

Header: flags `c2` (tables, last, one size byte, k = 3), checksum
`5a` XOR `c2` XOR `3a` = `a2`, S = 58; data bits 24..482.

Pre-code lengths (bits 24..103): symbols 0..5 and 16 have length 3,
symbols 18 and 19 length 4, giving 0 = `000`, 1 = `001`, 2 = `010`,
3 = `011`, 4 = `100`, 5 = `101`, 16 = `110`, 18 = `1110`, 19 = `1111`.
The coded lengths occupy bits 104..321 and use symbol 16 five times
(for example at bit 132: position 75 has just received 4, symbol 16
with extra bits `000` copies it into positions 76..78) and symbols 18
and 19 for zero runs.

Resulting codes (symbol = code):

- Main: 33 = `0000`, 75 = `0001`, 76 = `0010`, 77 = `0011`, 78 = `0100`,
  97 = `0101`, 98 = `01100`, 99 = `01101`, 100 = `01110`, 101 = `01111`,
  102 = `10000`, 103 = `10001`, 104 = `10010`, 105 = `10011`,
  106 = `10100`, 257 = `10101`, 258 = `10110`, 259 = `10111`,
  260 = `11000`, 261 = `11001`, 263 = `11010`, 264 = `11011`,
  265 = `11100`, 266 = `11101`, 271 = `11110`, 277 = `11111`.
- Distance: 2 = `00`, 6 = `01`, 9 = `10`, 10 = `110`, 12 = `111`.
- Low-distance: 0 = `0`, 2 = `10`, 5 = `11`.
- Length: 0 = `00`, 1 = `01`, 2 = `10`, 3 = `11`.

Items from bit 322. Spaces in "Bits read" separate the fields; "regs"
lists R0, R1, ... after the item.

| Bit | Bits read | Meaning | Output added | regs |
|--:|---|---|---|---|
| 322 | `0101`, then nine 5-bit codes `01100` .. `10100` | literals `a` .. `j` | `abcdefghij` | none |
| 371 | `11110` `0` `01` `01` | main 271: length slot 9 (lengths 12..13), extra bit 0: length 12. Distance symbol 6: slot 6 (distances 9..12, two extra bits read as one field), extra 1: distance 10 | `abcdefghijab` | 10 |
| 381 | `0001` `0010` `0011` | literals `K` `L` `M` | `KLM` | 10 |
| 393 | `11010` `00` | main 263: slot 1, length 3. Distance symbol 2: distance 3 | `KLM` | 3, 10 |
| 400 | `11011` `10` `000` | main 264: slot 2, length 4. Distance symbol 9: slot 9 (25..32, three extra bits), extra 0: distance 25 | `defg` | 25, 3, 10 |
| 410 | `0100` | literal `N` | `N` | 25, 3, 10 |
| 414 | `11100` `110` `0` | main 265: slot 3, length 5. Distance symbol 10: slot 10 (33..48, four extra bits: none read directly); low-distance symbol 0: distance 33 + 0 = 33 | `abcde` | 33, 25, 3, 10 |
| 423 | `11111` `00` `110` `11` | main 277: slot 15 (lengths 30..33), extra 0: length 30. Distance symbol 10 again; low-distance symbol 5 (`11`): distance 33 + 5 = 38 | `abcdefghijabcdefghijabKLMKLMde` | 38, 33, 25, 3 |
| 435 | `11101` `111` `0` `10` | main 266: slot 4, length 6. Distance symbol 12: slot 12 (65..96, five extra bits: one read directly, value 0); low-distance symbol 2 (`10`): extra 0 × 16 + 2 = 2: distance 67 | `bcdefg` | 67, 38, 33, 25 |
| 446 | `11001` `00` | main 261: register R3 = 25. Length-code symbol 0: length 2. R3 moves to the front | `bc` | 25, 67, 38, 33 |
| 453 | `11000` `11` | main 260: register R2 = 38. Length-code symbol 3: length 5 | `abcde` | 38, 25, 67, 33 |
| 460 | `10111` `01` | main 259: register R1 = 25. Length-code symbol 1: length 3 | `ija` | 25, 38, 67, 33 |
| 467 | `10110` `10` | main 258: register R0 = 25. Length-code symbol 2: length 4. Registers unchanged | `bKLM` | 25, 38, 67, 33 |
| 474 | `10101` | main 257: repeat distance 25 (R0) with the last length, 4 | `KLMd` | 25, 38, 67, 33 |
| 479 | `0000` | literal `!` | `!` | 25, 38, 67, 33 |

The position is now 483 = e (24 + 8 × 57 + 3). No distance exceeds 256,
so no length is adjusted. Output (93 bytes, CRC-32 `0x299786D1`):

`abcdefghijabcdefghijabKLMKLMdefgNabcdeabcdefghijabcdefghijabKLMKLMdebcdefgbcabcdeijabKLMKLMd!`

### 12.3 Three blocks

Packed bytes (30 bytes), algorithm version 0, U = 7:

`96 de 12 00 00 02 22 00 00 00 00 00 00 00 02 eb 2b 7f f1 b6 84 f7 0c 04 5e 00 46 1d 01 ec`

**Block 1**, bytes 0..22. Flags `96` = `10010110`: tables, not last,
bits 3..4 = `10`: three size bytes, k = 7. Checksum `5a` XOR `96` XOR
`12` XOR `00` XOR `00` = `de`. Size bytes `12 00 00`: S = 18. The table
transmission gives the main code 97 = `00`, 98 = `01`, 99 = `10`,
100 = `110`, 263 = `111` (lengths 2, 2, 2, 3, 3) and the distance code
the single symbol 2 = `0`. Items: literals `a`, `b`, `c`. The block ends
with the third literal.

**Block 2**, bytes 23..25: `04 5e 00`. Flags `04`: no tables, not last,
one size byte, k field 4 (meaningless here). Checksum `5a` XOR `04` XOR
`00` = `5e`. S = 0: no data, no items.

**Block 3**, bytes 26..29: `46 1d 01 ec`. Flags `46`: no tables, last,
one size byte, k = 7. Checksum `5a` XOR `46` XOR `01` = `1d`. S = 1: the
data is the first 7 bits of `ec` = `1110110`. With the codes of block 1:
`111` main 263 (length 3), `0` distance symbol 2 (distance 3): copies
`abc`; `110` main 100: literal `d`.

Output `abcabcd`, CRC-32 `0xE7C529EE`.

### 12.4 A real solid group of two members

These are the first two members of a solid archive written by RAR 7.23
from an empty file and a 120-byte text file (algorithm version 0,
D = 4 MiB).

**Member 1**: U = 0, solid flag clear, 20 packed bytes:

`c3 88 11 03 32 fc 22 79 4b 02 ff 1d e8 f0 ec cd 2a cb d2 90`

Header: flags `c3`: tables, last, one size byte, k = 4. Checksum `5a`
XOR `c3` XOR `11` = `88`. S = 17. Data bits 24..155 (e = 24 + 8 × 16 + 4
= 156).

| Bits | Value | Meaning |
|--:|---|---|
| 24..39 | `0000` `0011` `0011` `0010` | pre-code lengths of symbols 0..3: 0, 3, 3, 2 |
| 40..47 | `1111` `1100` | 15 followed by 12: the next 14 symbols (4..17) have length 0 |
| 48..55 | `0010` `0010` | symbols 18 and 19: length 2. All 20 known. Pre-code: 3 = `00`, 18 = `01`, 19 = `10`, 1 = `110`, 2 = `111` |
| 56..60 | `01` `111` | symbol 18, extra 7: 10 zeros, positions 0..9 |
| 61..62 | `00` | symbol 3: position 10 = 3 |
| 63..71 | `10` `1001011` | symbol 19, extra 75: 86 zeros, positions 11..96 |
| 72..77 | `00` `00` `00` | positions 97, 98, 99 = 3 |
| 78..86 | `10` `1111111` | symbol 19, extra 127: 138 zeros, positions 100..237 |
| 87..95 | `10` `0011101` | symbol 19, extra 29: 40 zeros, positions 238..277 |
| 96..98 | `111` | symbol 2: position 278 = 2 |
| 99..103 | `01` `000` | symbol 18, extra 0: 3 zeros, positions 279..281 |
| 104..106 | `111` | position 282 = 2 |
| 107..115 | `10` `0001110` | 25 zeros, positions 283..307 |
| 116..118 | `110` | symbol 1: position 308 = 1 |
| 119..123 | `01` `100` | 7 zeros, positions 309..315 |
| 124..126 | `110` | position 316 = 1 |
| 127..135 | `10` `0101010` | 53 zeros, positions 317..369 |
| 136..138 | `110` | position 370 = 1 |
| 139..143 | `01` `011` | 6 zeros, positions 371..376 |
| 144..146 | `110` | position 377 = 1 |
| 147..155 | `10` `0101001` | 52 zeros, positions 378..429. N reached at bit 156 = e. |

Codes: main 278 = `00`, 282 = `01`, 10 = `100`, 97 = `101`, 98 = `110`,
99 = `111`. Distance (positions 306..369): symbol 2 (position 308) =
`0`, symbol 10 (position 316) = `1`. Low-distance (positions 370..385):
symbol 0 = `0`, symbol 7 (position 377) = `1`. Length code: empty.

The block has no items and is the last. Member 1 delivers nothing; the
group count stays 0.

**Member 2**: U = 120, solid flag set, 7 packed bytes:

`41 1f 04 bb 89 1e c0`

Header: flags `41`: **no tables**, last, one size byte, k = 2. Checksum
`5a` XOR `41` XOR `04` = `1f`. S = 4. Data bits 24..49. The codes of
member 1 are in force.

| Bits | Value | Meaning |
|--:|---|---|
| 24..32 | `101` `110` `111` | literals `a`, `b`, `c` |
| 33..34 | `00` | main 278: length slot 16 (three extra bits, lengths 34..41) |
| 35..37 | `010` | extra 2: length 36 |
| 38 | `0` | distance symbol 2: distance 3. The match writes `abc` twelve times |
| 39..41 | `100` | main 10: literal, a line feed (`0a`) |
| 42..43 | `01` | main 282: length slot 20 (four extra bits, lengths 66..81) |
| 44..47 | `1110` | extra 14: length 80 |
| 48 | `1` | distance symbol 10: slot 10 (33..48), four extra bits, none read directly |
| 49 | `1` | low-distance symbol 7: distance 33 + 7 = 40. No adjustment. |

Output: forty bytes (`abc` thirteen times and a line feed), then the 80
bytes copied from 40 back: the same forty bytes twice more. 120 bytes,
CRC-32 `0x0FF880CE`.

### 12.5 Filters

**Delta, three channels.** Packed bytes (44 bytes), U = 20:

`c0 b3 29 20 00 23 00 00 00 00 00 30 33 6a 11 7a e5 41 c6 61 3f fe 3a 63 30 93 ff e6 37 97 7f 00 00 d0 28 e2 f4 48 c3 de 67 4a d4 80`

The items are: literals `R` `I` `F` `F`; a filter declaration; 13
literals; literals `E` `N` `D`. The declaration is decoded when the
group count is 4 and reads: 2-bit count `00` and one byte `00`: offset
0; 2-bit count `00` and one byte `0d`: length 13; type `000`; channel
field `00010`: 3 channels. So the range is LZ positions 4..16, file
offset 4.

LZ output: `52 49 46 46 | 0a f6 05 fb 01 02 03 00 ff ff 07 09 04 | 45 4e 44`.

The 13 range bytes are split 5 + 4 + 4 among the channels (positions 0,
3, 6, 9, 12; then 1, 4, 7, 10; then 2, 5, 8, 11):

| Channel | Input bytes | Running value (previous − input, mod 256) | Stored at positions |
|--:|---|---|---|
| 0 | `0a f6 05 fb 01` | `f6 00 fb 00 ff` | 0, 3, 6, 9, 12 |
| 1 | `02 03 00 ff` | `fe fb fb fc` | 1, 4, 7, 10 |
| 2 | `ff 07 09 04` | `01 fa f1 ed` | 2, 5, 8, 11 |

Range after the filter: `f6 fe 01 00 fb fa fb fb f1 00 fc ed ff`.
Member output: `52 49 46 46 f6 fe 01 00 fb fa fb fb f1 00 fc ed ff 45 4e
44`, CRC-32 `0xB1F65218`.

**x86.** A range of 35 bytes at file offset F = 200 (vectors V9b and
V9c):

`e8 00 10 00 00 e8 f0 ff ff ff e9 00 20 00 00 e8 ff ff ff 00 e8 00 00 00 01 e8 00 ff ff ff 90 e8 01 02 03`

| i | Byte | A | c = F + i + 1 | Type 1 result |
|--:|---|---|--:|---|
| 0 | `e8` | `0x00001000` | 201 | A < 2^24: A − 201 = `0x00000f37`: bytes `37 0f 00 00` |
| 5 | `e8` | `0xfffffff0` | 206 | A ≥ 2^31 and A + 206 ≥ 2^32: A + 2^24 − 2^32 = `0x00fffff0`: bytes `f0 ff ff 00` |
| 10 | `e9` | | | type 1: not an opcode, i advances by 1; the bytes at 11..14 are not opcodes either |
| 15 | `e8` | `0x00ffffff` | 216 | A − 216 = `0x00ffff27`: bytes `27 ff ff 00` |
| 20 | `e8` | `0x01000000` | 221 | 2^24 ≤ A < 2^31: unchanged |
| 25 | `e8` | `0xffffff00` | 226 | A ≥ 2^31 but A + 226 < 2^32: unchanged |
| 30 | `90` | | | not an opcode |
| 31 | `e8` | | | i = 31 is greater than n − 5 = 30: the scan has ended |

Type 1 result: `e8 37 0f 00 00 e8 f0 ff ff 00 e9 00 20 00 00 e8 27 ff ff
00 e8 00 00 00 01 e8 00 ff ff ff 90 e8 01 02 03`.

With type 2 the byte at i = 10 is an opcode too: A = `0x00002000`,
c = 211, A − 211 = `0x00001f2d`, bytes `2d 1f 00 00`, and i advances to
15; everything else is the same.

**ARM.** A range of 19 bytes at file offset F = 6 (vector V9d):

`10 00 00 eb 00 00 a0 e1 f0 ff ff eb 00 00 00 eb 01 02 eb`

| i | Byte at i + 3 | V | (F + i) / 4 rounded down | Result |
|--:|---|---|--:|---|
| 0 | `eb` | `0x000010` | 1 | `0x00000f`: bytes `0f 00 00` |
| 4 | `e1` | | | unchanged |
| 8 | `eb` | `0xfffff0` | 3 | `0xffffed`: bytes `ed ff ff` |
| 12 | `eb` | `0x000000` | 4 | `0xfffffc`: bytes `fc ff ff` |
| 16 | | | | i = 16 is greater than n − 4 = 15: the last three bytes are left alone |

Result: `0f 00 00 eb 00 00 a0 e1 ed ff ff eb fc ff ff eb 01 02 eb`.

### 12.6 A crafted solid group

Member 1 (solid flag clear), U = 17, 40 packed bytes:

`c0 bf 25 23 30 33 00 00 00 00 00 30 03 9c 24 e1 67 07 33 b6 ff fc cd e3 2e 45 bd 12 5d fb c3 9d 34 56 78 9a 56 df bc 00`

Its table transmission defines more symbols than its own items use
(among others main 0, 16, 33, 232 and 256..261 and length symbols
0..3). Items: literals `0` .. `9`; a new-distance match of length 4 and
distance 7 (`3456`); one of length 3 and distance 5 (`934`). Output
`01234567893456934`, CRC-32 `0x56AE2E2D`. State afterwards: group count
17, R0 = 5, R1 = 7, two registers filled, last length 3.

Member 2 (solid flag set), U = 15, 12 packed bytes:

`47 14 09 ce ed 85 80 00 05 37 10 00`

Header: flags `47`: no tables, last, one size byte, k = 8; S = 9. Member
start position 17. Items, with member 1's codes:

| Bit | Item | Meaning | Output added |
|--:|---|---|---|
| 24 | main 257 | repeat: distance 5 (R0), last length 3; reaches into member 1's output | `569` |
| 29 | main 259, length symbol 2 | register R1 = 7, length 4; the registers become 7, 5 | `6934` |
| 36 | main 259, length symbol 0 | register R1 = 5, length 2; the registers become 5, 7 | `96` |
| 43 | main 33 | literal | `!` |
| 47 | main 256 | filter declaration: offset 0, length 5, type 1. The group count is 27, so the range is LZ positions 27..31; file offset 27 − 17 = 10 | |
| 75 | five literals | `e8 10 00 00 00` | (collected by the filter) |

The x86 filter finds the opcode at i = 0: A = `0x00000010`, c = 10 + 0 +
1 = 11, A − 11 = 5: `e8 05 00 00 00`.

Output of member 2: `35 36 39 36 39 33 34 39 36 21 e8 05 00 00 00`
(`569693496!` and the five filtered bytes), CRC-32 `0x119AC861`.

The same two members decode identically when a stored member and an
empty member (P = 0) with the solid flag are placed between them
(section 11.2).

## 13. Test vectors

Each vector gives the packed bytes of one member (or of each member of a
group) and the header values the decoder needs. Unless an entry says
otherwise: algorithm version 0, D = 128 KiB (N = 0), solid flag clear, U
known. To show a vector to an extraction tool it must be wrapped in a
RAR 5.0 archive (`rar5-container.md`); a unit test of the decoder feeds
the packed bytes directly.

"Checked with" names the tools that produced exactly the expected
output from an archive built around the vector: S = the experimental
decoders written from this document (two, written independently of
each other; every vector was run through both), U = `unar` 1.10.7, B =
`bsdtar` 3.5.3. When a group of members is wrapped for `bsdtar`, the
archive's main header must carry the archive-level solid flag, or that
tool refuses the continuation members; this document's rules do not
consult that flag (section 11.2).

### 13.1 Valid streams

| # | Exercises | P | U | Expected output | CRC-32 | Checked with |
|--:|---|--:|--:|---|---|---|
| V1 | one block, tables, literals, new-distance match | 21 | 6 | `abcabc` | `0x726E994C` | S U B |
| V2 | every LZ item kind; direct and low-distance bits; register order | 61 | 93 | section 12.2 | `0x299786D1` | S U B |
| V3 | three blocks: three size bytes, S = 0, block without tables | 30 | 7 | `abcabcd` | `0xE7C529EE` | S U B |
| V4 | pre-code: length 15, zero run, zero run cut off at symbol 19; lengths written with symbols 0, 16, 17 only | 25 | 6 | `abcabc` | `0x726E994C` | S U B |
| V5 | pre-code symbols 16, 17, 18, 19; a zero run cut off at N; codes of length 8 and 9 | 283 | 456 | bytes `00`..`ff`, then `00`..`c7` | `0x986732AB` | S U B |
| V6 | all 44 length slots at smallest and largest length, through the main code and through the length code | 455 | 98,577 | below | `0x88DB91A4` | S U B |
| V7 | distance slots 0..41 at smallest and largest distance; the three length adjustments on both sides of each boundary (D = 4 MiB, N = 5) | 1638 | 2,097,164 | below | `0x61C5C930` | S U B |
| V8 | optional, large: distances 1,073,741,824 (slot 59, largest) and 33,554,433 (slot 50, smallest); D = 1 GiB (N = 13) | 491,461 | 1,073,741,848 | below | `0xBBE862BA` | S U |
| V9a | delta filter, 3 channels, range not a multiple of 3 | 44 | 20 | section 12.5 | `0xB1F65218` | S U B |
| V9b | x86 E8 filter at file offset 200 | 123 | 238 | below | `0xBA6849F4` | S U B |
| V9c | x86 E8/E9 filter at file offset 200 | 123 | 238 | below | `0x63A30483` | S U B |
| V9d | ARM filter at file offset 6 | 45 | 28 | below | `0x2332187C` | S U B |
| V9e | two filters queued at once, adjacent ranges, integers written with more bytes than needed | 42 | 18 | below | `0xC64F1D0F` | S U B |
| V10 | solid group of two: inherited codes, registers, last length, window; filter offset counted from the member start | 40 and 12 | 17 and 15 | section 12.6 | `0x56AE2E2D`, `0x119AC861` | S U B |
| V11 | algorithm version 1 (80 distance symbols) | 59 | 49 | below | `0x1A205CA3` | S |
| V12 | symbol 257 before any match does nothing | 23 | 6 | `abcdef` | `0x4B8E39EF` | S U B |
| V13 | bytes after the last block are ignored | 24 | 6 | `abcabc` | `0x726E994C` | S U |
| V14 | real data: empty first member carrying the tables, continuation member without tables (D = 4 MiB, N = 5) | 20 and 7 | 0 and 120 | section 12.4 | none, `0x0FF880CE` | S B |
| V15 | two blocks that both carry tables: the second transmission replaces every code, while R0 and the last length survive it | 44 | 15 | `abcabcabcabcdef` | `0xEAC72C00` | S U B |
| V16 | incomplete main code (thirteen codes of 4 bits); register matches through R1 and R2 | 44 | 58 | below | `0x406222F2` | S U B |
| V17 | solid group: V1, then a continuation member of 4 bytes without tables | 21 and 4 | 6 and 3 | `abcabc`, `abc` | `0x726E994C`, `0x352441C2` | S U B |
| V18 | solid group with both algorithm versions: V1 (version 0), then V11 as a continuation member (version 1; its first block carries tables) | 21 and 59 | 6 and 49 | as V1, as V11 | `0x726E994C`, `0x1A205CA3` | S |
| V19 | 8,192 filters pending at once (the limit of section 8.7) | 34,841 | 8,196 | below | `0x65788AE2` | S U |
| V20 | a member made of two blocks without data and without tables, at the start of a group | 6 | 0 | empty | none | S |
| V21 | a distance of 131,073 (the bytes of E23) with D = 256 KiB (N = 1) | 97 | 140,010 | below | `0xA290E3AA` | S U B |

Packed bytes and notes:

- **V1**: section 12.1.

  `c6 8e 12 01 20 00 00 00 00 00 00 00 02 eb 55 ff c7 5a 17 b8 6c`

- **V2**: section 12.2.

  `c2 a2 3a 33 33 33 00 00 00 00 00 30 44 f2 d3 cf 4c 3c 3c ba ff ff 03 71 17 0e 37 95 f2 6b 82 01 30 fd 41 08 05 dd 61 e7 55 8d 73 e1 19 4e 9e 28 91 e8 dc 13 99 f3 7d eb 26 3b b6 aa 00`

- **V3**: section 12.3.

  `96 de 12 00 00 02 22 00 00 00 00 00 00 00 02 eb 2b 7f f1 b6 84 f7 0c 04 5e 00 46 1d 01 ec`

- **V4**:

  `c6 8a 16 12 36 78 9a bc de f0 f1 45 f0 ff 7a ae d9 ef fe 1b 9e 41 3d b4 6c`

  After the header `c6 8a 16` the pre-code lengths are written as the
  4-bit fields `1 2 3 6 7 8 9 a b c d e`, then `f 0` (symbol 12:
  length 15), `f 1` (symbols 13, 14, 15: three zeros), `4 5` (symbols 16
  and 17), `f 0` (symbol 18: length 15), `f f` (a run of 17 zeros of
  which only one, symbol 19, remains): 22 fields. The pre-code lengths
  are therefore 1, 2, 3, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 0, 0, 0,
  4, 5, 15, 0, a complete code. The coded lengths use only pre-code
  symbols 0, 1, 2, 16 and 17: zeros are written as symbol 0 followed by
  repeats (the first two entries are symbol 0, then symbol 17 with
  extra 85: positions 0..96 are zero). The codes and items are those of
  V1.

- **V5**:

      cf 83 17 01 02 00 00 00 33 00 00 00 33 33 50 5f 97 4b c3 92 7c a9 da 1f
      f8 00 08 10 18 20 28 30 38 40 48 50 58 60 68 70 78 80 88 90 98 a0 a8 b0
      b8 c0 c8 d0 d8 e0 e8 f0 f9 01 09 11 19 21 29 31 39 41 49 51 59 61 69 71
      79 81 89 91 99 a1 a9 b1 b9 c1 c9 d1 d9 e1 e9 f1 fa 02 0a 12 1a 22 2a 32
      3a 42 4a 52 5a 62 6a 72 7a 82 8a 92 9a a2 aa b2 ba c2 ca d2 da e2 ea f2
      fb 03 0b 13 1b 23 2b 33 3b 43 4b 53 5b 63 6b 73 7b 83 8b 93 9b a3 ab b3
      bb c3 cb d3 db e3 eb f3 fc 04 0c 14 1c 24 2c 34 3c 44 4c 54 5c 64 6c 74
      7c 84 8c 94 9c a4 ac b4 bc c4 cc d4 dc e4 ec f4 fd 05 0d 15 1d 25 2d 35
      3d 45 4d 55 5d 65 6d 75 7d 85 8d 95 9d a5 ad b5 bd c5 cd d5 dd e5 ed f5
      fe 06 0e 16 1e 26 2e 36 3e 46 4e 56 5e 66 6e 76 7e 86 8e 96 9e a6 ae b6
      be c6 ce d6 de e6 ee f6 ff 07 0f 17 1f 27 2f 37 3f 47 4f 57 5f 67 6f 77
      7f 87 8f 97 9f a7 af b7 bf c7 cf d7 df e7 ef f7 fb fe 66

  Pre-code lengths: symbol 1 = 2; symbols 8, 9, 16, 17, 18, 19 = 3. The
  coded lengths are, in order: symbol 8 (position 0); symbol 16 extra 0
  (3 repeats, positions 1..3); symbol 17 extra 124 (135 repeats,
  4..138); symbol 17 extra 105 (116 repeats, 139..254); symbol 9
  (position 255); symbol 18 extra 0 (3 zeros, 256..258); symbol 19 extra
  18 (29 zeros, 259..287); symbol 9 (position 288); symbol 19 extra 21
  (32 zeros, 289..320); symbol 1 (position 321, distance symbol 15);
  symbol 19 extra 52 (63 zeros, 322..384); symbol 1 (position 385,
  low-distance symbol 15); symbol 19 extra 127 (138 zeros, of which only
  the 44 positions 386..429 exist). Main code: symbols 0..254 with
  length 8, symbols 255 and 288 with length 9. Items: the 256 literals
  `00`..`ff`, then main 288 (length slot 26, extra 6: length 200),
  distance symbol 15 (two bits read directly, value 3; low-distance
  symbol 15: distance 193 + 63 = 256; no adjustment).

- **V6**:

      c9 51 c3 01 03 00 03 33 00 00 00 00 33 33 ed 95 1d 9a 85 d1 3f e3 c1 d4
      03 c4 34 42 a3 28 54 01 54 0d 60 55 c1 d8 09 64 2d a0 d6 c3 dc 08 b9 11
      74 26 ec 4d e0 ab d1 57 c2 ef c5 e0 06 60 c6 61 06 e1 c6 e2 07 62 c7 63
      07 e3 c7 e4 04 32 72 19 41 1c bc 8e 60 4b 37 25 9c 13 cf c9 e8 02 9a 3c
      a6 90 2b a7 ca ea 02 da bc b6 b0 2f af cb ec 01 8d 9f 31 b4 06 76 fc ce
      e0 00 dd f0 1b c0 07 7f c0 f0 00 17 0f c1 71 00 1f 1f c1 f2 00 27 2f c2
      73 00 2f 3f c2 f4 00 1b a7 f0 dd 40 07 eb fc 3f 60 02 3b 7f 11 dc 00 9e
      ff c4 f8 00 15 e3 fc 57 90 01 7e 7f c5 fa 00 19 eb fc 67 b0 01 be ff c6
      fc 00 0e f9 ff 1d f4 00 3f ef fc 7f e0 01 0f df f2 1f c0 04 7f ff c8 d3
      03 54 c2 dd 31 39 4c 6e d3 23 d4 ca fa 99 9f d3 3c 0a 68 41 a6 8c 1a 69
      42 a6 9c 2a 6a 43 a6 ac 3a 6b 44 a6 bc 4a 6c 22 d3 67 16 9b 48 d4 db c6
      a6 e2 3d 37 71 e9 bc 91 4d fc 8a 70 12 69 c3 c9 a7 11 2a 9c 7c aa 72 12
      e9 cb cb a7 31 32 9c fc ca 74 04 da 74 7c da 74 84 ea 74 fc ea 75 04 fa
      75 7c fa 75 85 0a 75 fd 0a 76 02 8d 3b 3f 46 9d a0 a5 4e df d2 a7 70 1a
      a7 77 da a7 78 1b a7 7f db a7 80 0e 53 c3 f7 29 e2 03 b4 f1 fd da 79 00
      f5 3c bf 7a 9e 60 3e a7 9f ef a9 e8 01 fd 3d 3f bf a7 a8 08 14 f5 ff 02
      9e c0 20 d3 db fc 1a 7b 80 85 4f 7f f0 a9 f0 01 0e 9f 1f f0 e9 f2 01 12
      9f 3f f1 29 f4 01 16 9f 5f f1 69 f6 01 1a 9f 7f f1 a9 f8 00 8f 4f cf fc
      7a 7e 80 24 53 f7 ff 22 9f c0 09 34 fe ff c9 a7 f8 02 55 3f ff f2 80

  The output starts with a literal `x`; then for each length slot 0..43
  in turn, for its smallest and (if different) its largest length: a
  new-distance match of that length with distance 1, followed by one
  literal (`A` + slot mod 26). Then the same again with register
  matches (main 258, slot from the length code) and lower-case literals.
  The output begins `78 78 78 41 41 41 41 42 42 42 42 42 43` (`x`, a
  match of length 2, `A`, a match of length 3, `B`, ...) and its last
  4,099 bytes are all `72` (`r`). Every distance is 1, so no length is
  adjusted.

- **V7**:

      c8 f6 62 06 33 00 03 30 00 00 00 00 33 33 5f ff 14 a8 07 07 0a e6 01 b0
      1c c0 37 cf 3c 5e ed e0 f1 78 3c 1e 0f 06 a8 a7 44 71 67 83 3d 08 44 32
      14 c7 42 54 b6 34 db 8e 81 fa f6 04 16 6c 18 6d d8 51 5d b0 e3 bf 62 24
      c3 62 65 c7 62 a6 cb 62 e7 cf 63 14 69 b1 ba 75 d8 e3 d5 6c 7d fa f6 40
      9b 2d 91 e7 cf 64 87 aa d9 3d fa f6 51 ea b6 55 fa f6 59 ea b6 5d fa f6
      60 f5 5b 33 7e bd 9a 6e dc 0f d7 b3 87 10 1b bb 81 fa f6 78 e4 03 77 f0
      3f 5e d0 0e bc 1b c3 c0 fd 7b 44 3a e0 6f 1f 03 f5 ed 20 76 e0 6f 2f 81
      fa f6 98 3b 70 37 9f c0 fd 7b 50 0f 2e 06 f4 fc 0f d7 b5 40 f2 e0 6f 5f
      c0 fd 7b 58 03 dd c0 de cf e0 7e bd ad 01 ee e0 6f 6f f0 3f 5e d7 00 7d
      dc 0d ee ff 03 f5 ed 78 07 dd c0 de ff f0 3f 5e d8 00 3f f6 83 7c 3f e0
      7e bd b1 00 7f ec 06 f8 ff c0 fd 7b 64 00 7f ff 07 fe b0 6f 97 fe 07 eb
      db 30 03 ff f8 3f f5 83 7c ff f0 3f 5e da 00 0f ff e0 ff fe 0f ff e0 ff
      d2 0d f4 ff e0 7e bd b5 00 1f ff c1 ff fc 1f ff c1 ff a4 1b eb ff c0 fd
      7b 6c 00 1f ff c1 ff fc 1f ff c1 ff fc 1f ff c1 ff fc 1f ff c1 ff 94 1b
      ed ff e0 7e bd b7 00 0f ff e0 ff fe 0f ff e0 ff fe 0f ff e0 ff fe 0f ff
      e0 ff ca 0d f7 ff f0 3f 5e dc 00 03 ff f8 3f ff 83 ff f8 3f ff 83 ff f8
      3f ff 83 ff f8 3f ff 83 ff f8 3f ff 83 ff f8 3f ff 83 ff f8 3f ff 83 ff
      f8 3f ee 83 7e 3f fe 07 eb db 90 00 7f ff 07 ff f0 7f ff 07 ff f0 7f ff
      07 ff f0 7f ff 07 ff f0 7f ff 07 ff f0 7f ff 07 ff f0 7f ff 07 ff f0 7f
      ff 07 fd d0 6f cf ff c0 fd 7b 74 00 07 ff f0 7f ff 07 ff f0 7f ff 07 ff
      f0 7f ff 07 ff f0 7f ff 07 ff f0 7f ff 07 ff f0 7f ff 07 ff f0 7f ff 07
      ff f0 7f ff 07 ff f0 7f ff 07 ff f0 7f ff 07 ff f0 7f ff 07 ff f0 7f ff
      07 ff f0 7f ff 07 ff f0 7f ff 07 ff f0 7f ff 07 ff f0 7f cc 06 fd 7f fe
      07 eb db b0 00 3f ff 83 ff f8 3f ff 83 ff f8 3f ff 83 ff f8 3f ff 83 ff
      f8 3f ff 83 ff f8 3f ff 83 ff f8 3f ff 83 ff f8 3f ff 83 ff f8 3f ff 83
      ff f8 3f ff 83 ff f8 3f ff 83 ff f8 3f ff 83 ff f8 3f ff 83 ff f8 3f ff
      83 ff f8 3f ff 83 ff f8 3f ff 83 fe 58 37 ef ff f0 3f 5e de 00 00 ff fe
      0f ff e0 ff fe 0f ff e0 ff fe 0f ff e0 ff fe 0f ff e0 ff fe 0f ff e0 ff
      fe 0f ff e0 ff fe 0f ff e0 ff fe 0f ff e0 ff fe 0f ff e0 ff fe 0f ff e0
      ff fe 0f ff e0 ff fe 0f ff e0 ff fe 0f ff e0 ff fe 0f ff e0 ff fe 0f ff
      e0 ff fe 0f ff e0 ff fe 0f ff e0 ff fe 0f ff e0 ff fe 0f ff e0 ff fe 0f
      ff e0 ff fe 0f ff e0 ff fe 0f ff e0 ff fe 0f ff e0 ff fe 0f ff e0 ff fe
      0f ff e0 ff fe 0f ff e0 ff fe 0f ff e0 ff fe 0f ff e0 ff fe 0f ff e0 ff
      fe 0f ff e0 ff fe 0f ff e0 ff fe 0f f5 60 df cf ff e0 7e bd bd 00 01 ff
      fc 1f ff c1 ff fc 1f ff c1 ff fc 1f ff c1 ff fc 1f ff c1 ff fc 1f ff c1
      ff fc 1f ff c1 ff fc 1f ff c1 ff fc 1f ff c1 ff fc 1f ff c1 ff fc 1f ff
      c1 ff fc 1f ff c1 ff fc 1f ff c1 ff fc 1f ff c1 ff fc 1f ff c1 ff fc 1f
      ff c1 ff fc 1f ff c1 ff fc 1f ff c1 ff fc 1f ff c1 ff fc 1f ff c1 ff fc
      1f ff c1 ff fc 1f ff c1 ff fc 1f ff c1 ff fc 1f ff c1 ff fc 1f ff c1 ff
      fc 1f ff c1 ff fc 1f ff c1 ff fc 1f ff c1 ff fc 1f ff c1 ff fc 1f ff c1
      ff fc 1f ff c1 ff fc 1f ff c1 ff fc 1f ea c1 bf bf ff c0 fd 7b 7c 00 01
      ff fc 1f ff c1 ff fc 1f ff c1 ff fc 1f ff c1 ff fc 1f ff c1 ff fc 1f ff
      c1 ff fc 1f ff c1 ff fc 1f ff c1 ff fc 1f ff c1 ff fc 1f ff c1 ff fc 1f
      ff c1 ff fc 1f ff c1 ff fc 1f ff c1 ff fc 1f ff c1 ff fc 1f ff c1 ff fc
      1f ff c1 ff fc 1f ff c1 ff fc 1f ff c1 ff fc 1f ff c1 ff fc 1f ff c1 ff
      fc 1f ff c1 ff fc 1f ff c1 ff fc 1f ff c1 ff fc 1f ff c1 ff fc 1f ff c1
      ff fc 1f ff c1 ff fc 1f ff c1 ff fc 1f ff c1 ff fc 1f ff c1 ff fc 1f ff
      c1 ff fc 1f ff c1 ff fc 1f ff c1 ff fc 1f ff c1 ff fc 1f ff c1 ff fc 1f
      ff c1 ff fc 1f ff c1 ff fc 1f ff c1 ff fc 1f ff c1 ff fc 1f ff c1 ff fc
      1f ff c1 ff fc 1f ff c1 ff fc 1f ff c1 ff fc 1f ff c1 ff fc 1f ff c1 ff
      fc 1f ff c1 ff fc 1f ff c1 ff fc 1f ff c1 ff fc 1f ff c1 ff fc 1f ff c1
      ff fc 1f ff c1 ff fc 1f ff c1 ff fc 1f ff c1 ff fc 1f ff c1 ff fc 1f ff
      c1 ff fc 1f ff c1 ff fc 1f ff c1 ff fc 1f ff c1 ff fc 1f ff c1 ff fc 1f
      ff c1 ff fc 1f ff c1 ff fc 1f ff c1 ff fc 1f ff c1 ff fc 1f ff c1 ff fc
      1f ff c1 ff fc 1f da c1 bf df ff e0 7e bd bf 00 00 ff fe 0f ff e0 ff fe
      0f ff e0 ff fe 0f ff e0 ff fe 0f ff e0 ff fe 0f ff e0 ff fe 0f ff e0 ff
      fe 0f ff e0 ff fe 0f ff e0 ff fe 0f ff e0 ff fe 0f ff e0 ff fe 0f ff e0
      ff fe 0f ff e0 ff fe 0f ff e0 ff fe 0f ff e0 ff fe 0f ff e0 ff fe 0f ff
      e0 ff fe 0f ff e0 ff fe 0f ff e0 ff fe 0f ff e0 ff fe 0f ff e0 ff fe 0f
      ff e0 ff fe 0f ff e0 ff fe 0f ff e0 ff fe 0f ff e0 ff fe 0f ff e0 ff fe
      0f ff e0 ff fe 0f ff e0 ff fe 0f ff e0 ff fe 0f ff e0 ff fe 0f ff e0 ff
      fe 0f ff e0 ff fe 0f ff e0 ff fe 0f ff e0 ff fe 0f ff e0 ff fe 0f ff e0
      ff fe 0f ff e0 ff fe 0f ff e0 ff fe 0f ff e0 ff fe 0f ff e0 ff fe 0f ff
      e0 ff fe 0f ff e0 ff fe 0f ff e0 ff fe 0f ff e0 ff fe 0f ff e0 ff fe 0f
      ff e0 ff fe 0f ff e0 ff fe 0f ff e0 ff fe 0f ff e0 ff fe 0f ff e0 ff fe
      0f ff e0 ff fe 0f ff e0 ff fe 0f ff e0 ff fe 0f ff e0 ff fe 0f ff e0 ff
      fe 0f ff e0 ff fe 0f ff e0 ff fe 0f ff e0 ff fe 0f ff e0 ff fe 0f ff e0
      ff fe 0f ff e0 ff fe 0f ff e0 ff fe 0f ff e0 ff fe 0f ff e0 ff fe 0f ed
      60 df ff ff f0 00

  The output begins with 16 marker bytes `a0`..`af` and a `00`. For each
  tested distance the stream either emits two fresh literals and copies
  3 bytes (small distances), or pads with distance-1 matches up to an
  LZ position p and then copies 8 bytes from the marker with a
  new-distance match (lengths as coded: 8 plus the adjustment, i.e. 8,
  9, 10 or 11 bytes). A wrong distance, or a wrong adjustment, changes
  the output. The first 48 output bytes are

  `a0 a1 a2 a3 a4 a5 a6 a7 a8 a9 aa ab ac ad ae af 00 c1 d1 d1 d1 d1 c2 d2 c2 d2 c2 c3 d3 c2 c3 d3 c4 d4 c3 d3 c4 c5 d5 c3 d3 c4 c6 d6 d5 c3 d3 c7`

  and the last 16 are `d1 d1 d1 d1 a0 a1 a2 a3 a4 a5 a6 a7 a8 a9 aa 00`
  (the final match has distance 2,097,152 and length 11, copying
  `a0`..`aa`). The tested distances are 1..9, 12, 13, 16, 17, 24, 25,
  32, 33, 48, 49, 64, 65, 96, 97, 128, 129, 192, 193, 256, 257, and so
  on: for every slot s from 4 to 41 the smallest and the largest
  distance of the table in section 8.3, plus 8,192 / 8,193 and
  262,144 / 262,145 (which are slot boundaries too).

- **V8** (optional; needs a window of 1 GiB and writes 1 GiB). The
  packed bytes are, in this order:

  1. the 45 bytes
     `d0 32 c0 7f 07 03 30 33 00 00 00 00 00 33 33 5c aa fc d3 a0 f8 57 27 72 f8 07 c6 b1 d3 1d 4e 23 83 1d 09 18 e8 4a 96 c6 b9 f0 8c a7 40`
  2. the 15 bytes `ff fd ff fb ff f7 ff ef ff df ff bf ff 7f fe`
     repeated 31,736 times;
  3. the 11 bytes `ff fd ff fb d9 5b 5f 19 d6 f8 1f`;
  4. the 15 bytes `ff bf ff 7f fe ff fd ff fb ff f7 ff ef ff df`
     repeated 1,023 times;
  5. the 20 bytes
     `ff bf ff 7f fe ff fd ff fb fe d7 7f ff ff f8 76 00 00 00 80`.

  That is one block with a three-byte size (S = 491,456). The output is:
  the 16 bytes `a0`..`af`; zeros up to LZ position 1,040,187,402; the 8
  bytes `c0`..`c7` at positions 1,040,187,403..1,040,187,410; zeros up
  to position 1,073,741,823; then `a0 a1 a2 a3 a4 a5 a6 a7 a8 a9 aa`
  (a match of length 8 + 3 with distance 1,073,741,824); `00`; then
  `c0 c1 c2 c3 c4 c5 c6 c7 00 00 00` (length 8 + 3, distance
  33,554,433); and a final `21`.

- **V9a**: section 12.5.

  `c0 b3 29 20 00 23 00 00 00 00 00 30 33 6a 11 7a e5 41 c6 61 3f fe 3a 63 30 93 ff e6 37 97 7f 00 00 d0 28 e2 f4 48 c3 de 67 4a d4 80`

- **V9b** (type 1) and **V9c** (type 2) differ in one byte (`36` against
  `56`, the 3-bit type field). V9b:

  `c7 e5 78 00 02 20 00 00 00 00 00 00 22 15 c0 b8 47 30 b5 76 cf 31 66 e0 d7 ff 98 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 f0 00 23 36 14 0b de ee c0 c0 5f 77 0b 00 15 8e ee ab 23 48 97`

  V9c: the same bytes with the byte at offset 105 (`36`) replaced by
  `56`.

  Items: 200 literal zero bytes; the filter declaration (offset 0,
  length 35); the 35 literals of section 12.5; literals `E` `N` `D`.
  Expected output: 200 zero bytes, the type 1 (V9b) or type 2 (V9c)
  result of section 12.5, `45 4e 44`.

- **V9d**: section 12.5.

  `c4 b4 2a 00 00 22 00 00 00 00 00 20 33 27 b1 d4 06 a7 8c 76 a6 c6 27 06 bf ff 30 24 68 ad f0 00 13 6e 01 a0 17 9d ef 68 00 68 96 cd 40`

  LZ output `01 02 03 04 05 06`, the 19 range bytes of section 12.5,
  `45 4e 44`. Expected output:
  `01 02 03 04 05 06 0f 00 00 eb 00 00 a0 e1 ed ff ff eb fc ff ff eb 01 02 eb 45 4e 44`.

- **V9e**:

  `c7 ba 27 20 02 20 00 00 00 00 00 30 03 16 cf cc ab 8b bf ab ff e6 2f 3c 01 02 00 7d 09 00 81 80 00 00 34 14 56 78 9a 98 76 5e`

  Items: literals `a` `b`; a declaration decoded at group count 2 with
  offset 1, length 8, type 0, 2 channels (range 3..10); a second
  declaration, still at group count 2, with offset 9 written in two
  bytes (`09 00`) and length 6 written in three (`06 00 00`), type 0, 1
  channel (range 11..16, adjacent to the first); literal `c`; the
  literals `01 02 03 04 05 06 07 08`; the literals `09 08 07 06 05 04`;
  literal `z`. Expected output:
  `61 62 63 ff fb fd f5 fa ee f6 e6 f7 ef e8 e2 dd d9 7a`.

- **V10**: section 12.6. Member 1:

  `c0 bf 25 23 30 33 00 00 00 00 00 30 03 9c 24 e1 67 07 33 b6 ff fc cd e3 2e 45 bd 12 5d fb c3 9d 34 56 78 9a 56 df bc 00`

  Member 2 (solid flag set): `47 14 09 ce ed 85 80 00 05 37 10 00`

- **V11** (algorithm version 1):

  `c4 a6 38 02 00 02 30 00 00 00 00 03 33 e5 9e 0d a2 25 11 ff f2 b3 39 d3 9d e1 c1 9c f8 44 32 14 c7 42 54 b6 35 cf 84 65 3a 55 5d 96 dd 7e 18 e5 9e 9a ed bf 1c f5 df 9e fe 7c 00`

  The transmission has 446 lengths: distance symbol 10 at position 316,
  low-distance symbol 2 at position 388, length symbol 1 at position
  403. Items: the 40 literals `30`..`57`; a new-distance match (length
  slot 3: length 5; distance symbol 10, low-distance symbol 2: distance
  35); a register match (R0, length symbol 1: length 3); literal `!`.
  Expected output: `0123456789:;<=>?@ABCDEFGHIJKLMNOPQRSTUVW56789:;<!`.
  The same bytes presented as algorithm version 0 (or as version 1 with
  the compatibility bit) must **fail**: the low-distance and length
  codes are then taken from the wrong positions, and the first match
  finds an empty low-distance code. Neither black-box tool implements
  version 1;
  the layout was established on real archives (section 18).

- **V12**:

  `c3 8d 14 00 22 00 00 00 00 00 00 20 02 eb 0c 7f f8 57 ff 97 13 f2 e0`

  Items: literals `a` `b` `c`, main 257, literals `d` `e` `f`.

- **V13**: the bytes of V1 followed by `ff ee dd`.

- **V14**: section 12.4. Member 1 (U = 0):
  `c3 88 11 03 32 fc 22 79 4b 02 ff 1d e8 f0 ec cd 2a cb d2 90`.
  Member 2 (solid flag set, U = 120): `41 1f 04 bb 89 1e c0`.

- **V15**: the block of V1 with the last-block flag cleared (flags `86`,
  checksum `ce`), followed by the block of V12:

  `86 ce 12 01 20 00 00 00 00 00 00 00 02 eb 55 ff c7 5a 17 b8 6c c3 8d 14 00 22 00 00 00 00 00 00 20 02 eb 0c 7f f8 57 ff 97 13 f2 e0`

  Block 1 produces `abcabc` and leaves R0 = 3 and last length 3. Block 2
  brings a different main code (literals `a`..`f` and symbol 257) and
  its items are `a` `b` `c`, main 257, `d` `e` `f`; here symbol 257 is
  not the no-op of V12 but repeats the match of block 1 (distance 3,
  length 3: `abc`).

- **V16**:

  `c0 b3 29 44 44 44 44 44 55 55 55 55 44 ba c8 88 88 89 7f ec 32 20 02 22 58 14 b2 24 40 04 05 6f 44 6c f8 09 4a 69 65 57 45 5b 06 00`

  Pre-code lengths: 4 for symbols 0..9, 18 and 19, 5 for symbols 10..17
  (complete). Main code: the thirteen symbols 97..103, 259, 260, 263,
  264, 265 and 278 all have length 4, so 97 = `0000`, ..., 278 = `1100`
  and the patterns `1101`, `1110`, `1111` are unassigned. Distance code:
  symbols 1, 2, 5, 7 with length 2. Low-distance code: empty. Length
  code: symbols 0 and 1 with length 1. Items from bit 277: `a` `b` `c`;
  match length 3 distance 3; `d` `e`; match length 5 distance 2; match
  length 4 distance 8; main 259 (R1 = 2) with length 2; main 260 (R2 =
  3) with length 3; `f` `g`; match length 34 distance 13. The registers
  end as 13, 3, 2, 8. Expected output:

  `abcabcdedededcdededdedfgedcdededdedfgedcdededdedfgedcdeded`

- **V17**: member 1 is V1. Member 2 (solid flag set, U = 3):
  `45 1e 01 18`: one block without tables, last, S = 1, k = 6; its six
  data bits `00 01 10` are three literals in the main code inherited
  from member 1. The same two members with member 2 marked as algorithm
  version 1 are the rejection vector E27.

- **V18**: member 1 is V1 (algorithm version 0). Member 2 is the bytes
  of V11 with the solid flag set and algorithm version 1. Its output is
  that of V11 (its matches reach only into its own output); afterwards
  the group count is 55 and the registers are 35, 3 (the 3 left by
  member 1). Neither black-box tool implements version 1.

- **V19** (34,841 bytes, given by construction). In this order:

  1. the 24 bytes
     `ca 0d 15 88 44 44 44 44 44 55 55 55 55 44 b6 c4 67 7f ed 38 df fb 31 b6`
     (block header: tables, last, two size bytes, k = 3, S = 34,837;
     then the table transmission and three literals `B`);
  2. for j = 0, 1, ..., 8,191 in turn, four bytes holding the number
     536,870,921 + 2,097,152 × (j mod 256) + 8,192 × (j divided by 256,
     rounded down), most significant byte first (the first three groups
     are `20 00 00 09`, `20 20 00 09`, `20 40 00 09`);
  3. the byte `aa` 2,048 times;
  4. the byte `c0`.

  Main code: 256 = `0`, 65 = `10`, 66 = `110`, 67 = `111`. Each
  four-byte group is one filter declaration: main 256, a two-byte block
  start offset j, a one-byte length 1, type 1. All 8,192 declarations
  are read while the group count is 3, so the ranges are the single
  bytes at LZ positions 3, 4, ..., 8,194, adjacent and ascending, and
  all 8,192 are pending when the last one is read. Then follow 8,192
  literals `A` and one `B`. An x86 filter leaves a one-byte range
  unchanged, so the expected output is `BBB`, 8,192 times `A`, `B`.
  `bsdtar` fails on this stream (it has a smaller limit of its own).

- **V20**: `00 5a 00 40 1a 00`. Two block headers with S = 0; the second
  has the last-block flag. No tables, no codes, no items, no output
  (section 6.3 step 4). The black-box tools do not read the stream of a
  member with U = 0 in this position, so this is checked with the
  experimental decoders only.

- **V21**: the 97 bytes of E23 (section 13.2), decoded with D = 262,144.
  The items are the literals `abcdefg`, 35 matches of distance 7 that
  extend the pattern to 140,000 bytes, and one new-distance match of
  distance 131,073 and length 8 + 2 that copies `cdefgabcde` from LZ
  position 8,927. The output is `abcdefg` 20,000 times followed by
  `cdefgabcde`. The same bytes with the same D and a window limit M
  configured to 131,072 must end as *unsupported* at the last match
  (section 9.2, check 3).

### 13.2 Streams a decoder must reject

All with algorithm version 0, D = 128 KiB, solid flag clear, unless the
entry says otherwise. The result is corrupt input in every case. "`unar`" and "`bsdtar`" give what the
tools do with the same bytes ("fails" = reports an error; otherwise the
bytes they deliver).

| # | Packed bytes | U | Why it is corrupt input | `unar` | `bsdtar` |
|--:|---|--:|---|---|---|
| E1 | `c6 8f 12 01 20 00 00 00 00 00 00 00 02 eb 55 ff c7 5a 17 b8 6c` | 6 | V1 with a wrong block header checksum. | fails | fails |
| E2 | `de 96 12 00 00 00 01 20 00 00 00 00 00 00 00 02 eb 55 ff c7 5a 17 b8 6c` | 6 | Flags bits 3..4 = 3 (four size bytes). | fails | fails |
| E3 | `44 1f 01 58` | 3 | The first block of a group has data but no tables. | fails | fails |
| E4 | `c1 8a 11 01 00 00 00 00 00 00 00 00 01 d6 1f f1 ca 17 72 80` | 6 | Main code over-subscribed: symbols 97, 98, 99 and 263 all have length 1. | fails | fails |
| E5 | `c6 8e 12 11 20 00 00 00 00 00 00 00 02 6b 00 ff 47 0a 1b b8 6c` | 6 | Pre-code over-subscribed: lengths 1, 1, 2 and 2. | fails | fails |
| E6 | `c4 8e 10 00 00 00 00 00 00 00 00 10 01 0f ff ff ff f1 b0` | 6 | The first coded length is pre-code symbol 16 (repeat) at position 0. | fails | fails |
| E7 | `c4 91 0f 01 00 00 00 00 00 00 00 00 01 d6 7f ff d6 90` | 3 | Main code has the single symbol 97 (code `0`); the items are `0`, `0`, `1`. | fails | fails |
| E8 | `c1 8e 15 02 22 00 00 00 00 00 00 00 33 f5 95 e2 ef ff 0b a2 3e c1 b9 80` | 8 | `a` `b` `c`, then a match of length 4 and distance 5 with only 3 bytes produced (DECISION). | `61 62 63 00 00 61 62 7a` | the same |
| E9 | `c3 8e 17 02 22 00 00 00 00 00 00 00 33 f5 96 e2 ef f6 c2 e8 4f 08 e8 06 76 50` | 9 | `abc`, a match (length 3, distance 3), then main 259 (register R1) with one register filled (DECISION). | `61 62 63 61 62 63 00 00 7a` | the same |
| E10 | `c5 8d 12 01 20 00 00 00 00 00 00 00 02 eb 55 ff c7 5a 17 b8 6c` | 6 | V1 with k = 6 instead of 7: the last item ends one bit after the block end (DECISION). | `abcabc` | `abcabc` |
| E11 | `86 ce 12 01 20 00 00 00 00 00 00 00 02 eb 55 ff c7 5a 17 b8 6c` | 6 | V1 without the last-block flag: the packed bytes end where the next header should be. | fails | `abcabc` |
| E12 | `c6 8e 12 01 20 00 00 00 00 00 00 00 02 eb 55 ff c7 5a 17 b8` | 6 | V1 without its last byte: S exceeds the packed bytes. | fails | six zero bytes |
| E13 | `c7 98 05 01 20 00 00 00 00 00 00 00 02 eb 55 ff c7 5a 17 b8 6c` | 6 | V1's data with a header saying S = 5: the table transmission runs past the block end. | fails | fails |
| E14 | `c5 b9 26 20 02 20 00 00 00 00 00 00 33 c9 e3 de 35 bb ab 4b 82 b8 cb 89 8b 87 b8 bb 85 bf fe 63 c0 20 12 69 cb e7 0d 14 a4` | 12 | Filter declaration with type 4. | fails | fails |
| E15 | `c2 b3 2b 20 02 20 00 00 00 00 00 00 33 c9 e3 de 35 bb ab 4b 82 b8 cb 89 8b 87 b8 bb 85 bf fe 63 c0 00 18 03 c0 30 18 03 4e 5f 38 68 a5 20` | 12 | Two declarations at group count 0: range 0..5, then range 3..8 (starts inside the first). | fails | fails |
| E16 | `c2 bf 27 20 02 20 00 00 00 00 00 00 33 c9 e3 de 35 bb ab 4b 82 b8 cb 89 8b 87 b8 bb 85 bf fe 63 c0 80 18 03 4e 5f 38 68 a5 20` | 12 | Filter range 8..13 in a member of 12 bytes: still queued at the end (DECISION). | fails | fails |
| E17 | `c2 bf 27 20 02 20 00 00 00 00 00 00 33 c9 e3 de 35 bb ab 4b 82 b8 cb 89 8b 87 b8 bb 85 bf fe 63 c0 20 00 03 4e 5f 38 68 a5 20` | 12 | Filter of length 0 (DECISION). | fails | fails |
| E18 | `c2 b1 29 20 02 20 00 00 00 00 00 00 33 c9 e3 de 35 bb ab 4b 82 b8 cb 89 8b 87 b8 bb 85 bf fe 63 c0 08 04 01 00 03 4e 5f 38 68 a5 20` | 12 | Filter of length 4,194,305 (DECISION). | fails | fails |
| E19 | `c2 bf 27 20 02 20 00 00 00 00 00 00 33 c9 e3 de 35 bb ab 4b 82 b8 cb 89 8b 87 b8 bb 85 bf fe 63 c1 40 10 03 4e 5f 38 68 a5 20` | 12 | Filter range 20..23 in a member of 12 bytes: never reached, still queued at the end (DECISION). | the 12 unfiltered bytes | the same |
| E20 | the bytes of V1 | 5 | The stream produces 6 bytes (DECISION). | `abcab` | fails |
| E21 | the bytes of V1 | 7 | The stream produces 6 bytes. | fails | fails |
| E22 | `c4 8f 11 00 10 00 00 00 00 00 00 00 01 d6 1f f1 cf f9 11 b0` | 6 | `abc`, then main 263 while the distance code is empty. | fails | fails |
| E23 | below | 140,010 | A match of distance 131,073 with D = 131,072, after 140,000 bytes of output (DECISION). | 140,010 bytes, checksum error | 140,010 bytes, checksum error |
| E24 | `c6 8e 12 01 20 00 00 00 00 00 00 00 02 eb 55 ff c4 df 56 7c 6c` | 6 | `abc`, then main 258 (register R0) before any match (DECISION). | `61 62 63 00 00 00` | the same |
| E25 | `c0 9a 00` | 0 | Tables flag on a block with S = 0: the transmission cannot end before the block end (DECISION). | stream not read | stream not read |
| E26 | below | 8,197 | A filter declaration read while 8,192 filters are pending (DECISION). | the 8,197 bytes | fails |
| E27 | member 1: the bytes of V1 (U = 6); member 2, solid flag set, **algorithm version 1**: `45 1e 01 18` | 3 | Member 1 decodes; member 2 has an item while the codes in force were transmitted for version 0 (DECISION; with version 0 this is V17). | not applicable | not applicable |

E23 (97 bytes):

    c6 c2 5e 02 02 30 00 00 00 00 00 30 33 f5 9a a7 ff c7 ce 3e 6d 32 39 13
    92 3b 00 53 97 3f fe 7f fc ff f9 ff f3 ff e7 ff cf ff 9f ff 3f fe 7f fc
    ff f9 ff f3 ff e7 ff cf ff 9f ff 3f fe 7f fc ff f9 ff f3 ff e7 ff cf ff
    9f ff 3f fe 7f fc ff f9 ff f3 ff e7 ff cf ff 9f ff 3f fe 7f fc e6 a6 c0
    00

In E14..E19 the 12 literals that follow the declaration(s) are
`d8 20 c5 c3 ef 80 05 3a 88 ae 39 96`.

For E8, E9, E10, E19, E20 and E24 the CRC-32 in a test archive must be
that of the output the tool produces if the vector is also to be shown
to that tool; a conforming decoder reports corrupt input regardless.

E26 (34,845 bytes, given by construction like V19): the 24 bytes
`cc 07 19 88 44 44 44 44 44 55 55 55 55 44 b6 c4 67 7f ed 38 df fb 31 b6`;
then the four-byte groups of V19 for j = 0, 1, ..., 8,192 (one more than
in V19; the last group is `20 04 00 09`); then the byte `aa` 2,048
times; then the byte `b0`. The 8,193rd declaration is read while 8,192
are pending. (Without the limit the stream would give `BBB`, 8,193 times
`A`, `B`.)

A match whose distance exceeds the window limit M is *unsupported*, not
corrupt input: see V21.

### 13.3 Transformation vectors

These test the four transformations of section 10.3 directly, as
functions of the range bytes, the file offset F of the range and, for
delta, the channel count. They cover file offsets that a stream vector
of embeddable size cannot reach.

x86. The range is these 57 bytes (call it X):

`e8 00 10 00 00 e8 f0 ff ff ff e8 ff ff ff 00 e8 00 00 00 01 e8 00 ff ff ff e8 05 00 00 00 e8 fb ff ff ff e8 ff ff ff ff e8 00 00 00 00 e8 01 00 00 00 90 90 e8 01 02 03 04`

| # | Type | F | Result |
|--:|--:|--:|---|
| X1 | 1 | 0 | `e8 ff 0f 00 00 e8 f0 ff ff ff e8 f4 ff ff 00 e8 00 00 00 01 e8 00 ff ff ff e8 eb ff ff ff e8 fb ff ff 00 e8 ff ff ff 00 e8 d7 ff ff ff e8 d3 ff ff ff 90 90 e8 01 02 03 04` |
| X2 | 1 | 16,777,196 | `e8 13 10 00 ff e8 f0 ff ff 00 e8 08 00 00 00 e8 00 00 00 01 e8 00 ff ff ff e8 ff ff ff ff e8 fb ff ff 00 e8 ff ff ff 00 e8 eb ff ff ff e8 e7 ff ff ff 90 90 e8 01 02 03 04` |
| X3 | 1 | 2,147,484,648 | `e8 17 0c 00 00 e8 f0 ff ff 00 e8 0c fc ff 00 e8 00 00 00 01 e8 00 ff ff 00 e8 03 fc ff ff e8 fb ff ff 00 e8 ff ff ff 00 e8 ef fb ff ff e8 eb fb ff ff 90 90 e8 01 02 03 04` |
| X4 | 1 | 4,294,967,303 | `e8 f8 0f 00 00 e8 f0 ff ff ff e8 ed ff ff 00 e8 00 00 00 01 e8 00 ff ff ff e8 e4 ff ff ff e8 fb ff ff 00 e8 ff ff ff 00 e8 d0 ff ff ff e8 cc ff ff ff 90 90 e8 01 02 03 04` |

X contains no `e9`, so type 2 gives the same results. In X2 the range
straddles file offset 16,777,216: for the opcodes at i = 0, 5, 10 the
value c is 16,777,197, 16,777,202 and 16,777,207, for the opcode at
i = 15 it would be 16,777,212 (operand unchanged), and from i = 20 on it
has wrapped to 1, 6, 11, ... In X3 c is 1,001 at i = 0 (2,147,484,649
mod 16,777,216); in X4 it is 8.

| # | Type | F | Range | Result |
|--:|--:|--:|---|---|
| X5 | 2 | 0 | `e9 05 00 00 00` | `e9 04 00 00 00` |
| X6 | 1 | 0 | `e9 05 00 00 00` | unchanged |
| X7 | 1 or 2 | 0 | `e8 05 00 00` (n = 4) | unchanged |

ARM. The range is the 19 bytes of section 12.5,
`10 00 00 eb 00 00 a0 e1 f0 ff ff eb 00 00 00 eb 01 02 eb`:

| # | F | Result |
|--:|--:|---|
| A1 | 6 | `0f 00 00 eb 00 00 a0 e1 ed ff ff eb fc ff ff eb 01 02 eb` |
| A2 | 67,108,866 | `10 00 00 eb 00 00 a0 e1 ee ff ff eb fd ff ff eb 01 02 eb` |
| A3 | 4,294,967,302 | the result of A1 |
| A4 | 4,362,076,162 | the result of A2 |
| A5 | 8, range `00 00 eb` (n = 3) | unchanged |
| A6 | 8, range `10 00 00 eb` (n = 4) | `0e 00 00 eb` |

In A2, (F + i) / 4 is 16,777,216 at i = 0, which is 0 modulo 2 to the
power 24: the first word is unchanged, and the words at i = 8 and 12
are lowered by 2 and 3.

Delta:

| # | Channels | Range | Result |
|--:|--:|---|---|
| D1 | 5 | `01 02 03` (n less than the channel count) | `ff fe fd` |
| D2 | 1 | `01 01 01 01` | `ff fe fd fc` |
| D3 | 32 | `0a` | `f6` |
| D4 | 3 | the 13 bytes of section 12.5 | as in section 12.5 |

How checked. All rows were computed with the transformation code of the
second experimental decoder. X1 and A1 are the cases of section 12.5
and vector V9d at other or the same offsets. X2 is CONFIRMED (crafted)
with both tools: a stream that produces 16,777,196 zero bytes, declares
a type 1 (and, in a second archive, type 2) filter over X and ends with
one more literal decodes to the same 16,777,254 bytes in `unar`,
`bsdtar` and the experimental decoder. X3 is CONFIRMED (crafted) with
`bsdtar` only (the same construction with 2,147,484,648 zero bytes;
`unar` reports a checksum failure there, section 15 item 5). X4 and
A2..A4 follow from the rule alone (INFERRED for x86, REFERENCE-ONLY for
ARM). A5, A6 and D1..D3: the cases named in section 10.3 as CONFIRMED
(crafted), recomputed.

## 14. Safety requirements for untrusted input

Every value named here comes from the archive and may be hostile.

1. **Never size a buffer from U, D, S or a filter length.** Output is
   produced incrementally. The window grows with the LZ output actually
   produced (section 9.3): at most the larger of 256 KiB and twice the
   group count, at most the window limit M, at most the group's
   dictionary size. The filter buffer grows with the bytes actually
   collected, at most 4 MiB. The filter queue holds at most 8,192 small
   entries. Besides these the decoder needs only fixed-size tables for
   five prefix codes of at most 306 symbols.
2. **The window limit M is enforced on distances, not on headers**
   (section 9.2 rule 3): no member can make the decoder hold more than M
   bytes of history, whatever D it declares. The default M is 1 GiB;
   embeddings with less memory must lower it.
3. **Every read is bounded twice**: by the packed bytes of the member
   and by the end of the current block (section 6.3). A block's S must
   fit in the packed bytes that remain. The table transmission and every
   item must end at or before the block end.
4. **Termination.** Every block consumes at least 3 packed bytes and
   every item at least 1 bit, and nothing is read twice, so the work is
   bounded by a constant times P plus the output produced. An item
   produces at most 4,100 bytes, so a member cannot expand by more than
   32,800 to 1; a decoder stops with corrupt input as soon as the
   member's output exceeds a known U, and the caller's limits on member
   and archive size apply as for every other format (they are the only
   limit when U is unknown). Symbol 257 with no last length, filter
   declarations and empty blocks produce nothing but consume input, so
   they cannot loop.
5. **Pre-code and code-length writes stay inside their arrays**: the
   pre-code zero run is cut off at symbol 19, runs of code lengths are
   cut off at N − 1, and a repeat at position 0 is rejected because
   there is no position −1 (sections 7.1, 7.3).
6. **Reject over-subscribed codes when they are built**, including the
   pre-code and including codes the block never uses. Decoding with an
   empty or incomplete code must fail cleanly on an unassigned pattern
   and must not index outside a lookup structure.
7. **Check every match before copying** (section 9.2): the register is
   filled (8.4); the distance is at least 1 (always true for decoded
   distances and for filled registers); it does not exceed the group
   count, nor D, nor M, nor the bytes the window still holds, tested in
   that order. All ring indices are reduced modulo the capacity, which
   need not be a power of two.
8. **Use 64-bit integers** for the group count, the member start
   position, distances, registers, filter starts and ends (a start is a
   64-bit count plus a 32-bit offset; an end adds up to 4 MiB more), the
   file offset in the filters, and U. Distances of algorithm version 0
   reach 2 to the power 32 and those of version 1 2 to the power 40;
   directly-read bit fields reach 34 bits. Shifts by the slot's bit
   count (up to 38) must be done in 64 bits.
9. **Filters.** Type 0..3 only; length 1..4,194,304; ranges ascending
   and disjoint; at most 8,192 pending (counted as section 8.7 defines,
   independently of when output is flushed); none left at the end of
   the member. The delta filter needs a second buffer of the range's length
   and must handle a range shorter than the channel count. The x86 and
   ARM filters must not read or write outside the range: the scans stop
   at n − 5 and n − 4, and ranges shorter than 5 or 4 bytes are left
   alone (note that n − 5 is negative for a short range: compare before
   subtracting, or test i + 5 ≤ n). The x86 additions and subtractions
   are modulo 2 to the power 32 and must not trap on overflow.
10. **A failed member poisons its group** (section 11.4); the state is
    discarded, never reused.
11. Verify the checksum of every delivered member (section 2.2).
12. **Do not trust the solid flag for existence of state** (section
    11.2): a continuation member without a predecessor is an error, not
    a reason to decode with an uninitialised state.

## 15. Observed implementation gaps

Faults and leniencies of the reference implementation (`unar` 1.10.7).
They explain why rules exist; nothing here is to be imitated. Where
`bsdtar` is mentioned it is as a second observation, not as a model.

1. **An empty first member that carries the tables is skipped.**
   Zero-length files are left out of the solid stream, so the table
   transmission in the packed bytes of such a member (section 12.4) is
   never read. On the real archive of section 12.4 `unar` extracts the
   empty file and fails on every file after it. The rules of section 11
   decode all of them (and so does `bsdtar`).
2. **Stored members break solid groups.** A stored member without the
   solid flag starts a new solid stream; one with the flag is fed to the
   decompressor. On a real solid archive with stored files in the middle
   `unar` reports "Unsupported" for the compressed members after them.
3. **The algorithm version is not looked at.** Version 1 members are
   decoded with 64 distance symbols and fail (a real RAR 7 archive with
   a dictionary above 4 GiB: "Unpacking failed"); a crafted version 2
   member is decoded as version 0. The dictionary fraction of version 1
   headers is ignored. Method values 6 and 7 are decoded like 1..5.
4. **The window is allocated and cleared from the declared D before any
   data is read**, 4 GiB if the header says so. A later member of a
   group that declares a larger D than the first is refused ("not
   supported").
5. **Distances and filter positions are held in 32-bit signed
   integers.** A distance above 2 GiB cannot work (not exercised by
   experiment). The x86 filter computes the file offset in 32 signed
   bits: at offsets of 2 GiB and more it produces wrong bytes (crafted
   archive: `unar` reports a checksum failure where `bsdtar` and this
   document's rule give the bytes the archive's CRC-32 was computed
   from).
6. **A distance beyond the start of the output reads zero bytes** (the
   window starts zero-filled and no check is made; vector E8). **An
   unfilled register** holds 0, and the copy then reads the ring
   position it is about to write (vectors E9, E24).
7. **Distances close to a small D are mis-decoded.** With D = 128 KiB,
   matches of distance 131,071 and 131,072 after 300,000 bytes of output
   give a checksum failure in `unar` but decode as specified in
   `bsdtar`. (Real encoders stay more than 4 KiB below D, so real
   archives are not affected; the cause was not investigated. A distance
   of exactly D = 1 GiB works in `unar`.)
8. **The block end is tested only before an item.** An item may run
   past the declared end (vector E10 is accepted). The next header is
   then looked for at the next byte boundary after the bits actually
   read.
9. **Filters are barely validated.** No limit on the length: a buffer of
   the declared length (up to 4 GiB) is allocated. No limit on the
   number queued (20,000 pending filters were accepted). A range longer
   than the window is copied out of the window after the fact. Ranges
   out of order or overlapping lead to partial output and an error
   instead of a clean rejection; a zero-length filter stops the output
   at its position; a filter whose range is never reached is dropped
   silently (vector E19); a filter left over at the end of a member is
   applied to the next member of the group.
10. **Output longer than U is cut off silently** (vector E20).
11. **Members of unknown size fail** (file flag `0x0008`).
12. **Look-ahead past the end of the packed bytes sees zeros**; only
    consuming such bits fails. Harmless as long as nothing is delivered
    for a failed member.
13. **A CRC-32 mismatch is reported but the bytes are written** to the
    output file all the same.

Leniencies this specification keeps: runs cut off at the end of the
pre-code and of the length list; symbol 257 without a last length as a
no-op; trailing bytes after the last block; bit 5 of the block flags.

## 16. Decisions

| # | Section | Decision |
|--:|---|---|
| R1 | 2.1 | Version above 1, N out of range, and undefined bits of the compression information field are *unsupported*. So are methods 6 and 7. |
| R2 | 2.2 | Compressed service-header data is a solid group of its own. |
| R3 | 2.2 | The container layer verifies the checksum of every member; a mismatch is an error for the member but does not poison the group. |
| R4 | 6.1 | Bit 5 of the block flags is ignored. |
| R5 | 6.3 | A table transmission and every item must end at or before the block end. |
| R6 | 7.1, 7.3 | Runs that pass the end of the pre-code or of the length list are cut off, not rejected. |
| R7 | 8.4, 8.5 | Selecting a register that has not been filled is corrupt input. |
| R8 | 8.7 | A filter of length 0, or longer than 4 MiB, is corrupt input. |
| R9 | 8.7 | Filter ranges must be declared in ascending order without overlap; at most 8,192 may be pending, a filter counting as pending until its whole range has been produced. |
| R10 | 9.2 | A distance greater than the group count, greater than the member's D, or (after a capacity rise in mid-group) greater than the history still held, is corrupt input. The checks of 9.2 are made in the stated order. |
| R11 | 9.2, 9.3 | The window limit M defaults to 1 GiB and is configurable. A larger declared D is accepted; a distance greater than M is *unsupported*. |
| R12 | 9.3 | The window capacity of a group follows the largest D declared so far in the group, capped at M; the allocation follows the output produced. |
| R13 | 10.2, 11.1 | A filter still queued when the member ends is corrupt input. |
| R14 | 11.1 | With U known, producing more or fewer bytes than U is corrupt input; what follows the last block is ignored. |
| R15 | 11.2 | Codes are not inherited between members of different algorithm versions. |
| R16 | 11.2 | A stored member never starts or ends a solid group and is invisible to the decoder state, whatever its solid flag. |
| R17 | 11.2 | A continuation member for which no state exists is an error, not decoded. |
| R18 | 11.4 | After a failed member, later members of the same group are reported failed without decoding. |

None of them changes how a genuine, undamaged archive in the survey is
read.

## 17. Open questions

They do not block an implementation; the document says what to do in
each case.

1. **ARM filter (type 3).** No real stream contains one: RAR 5.91 and
   7.23 did not emit it for 32-bit ARM code, with or without forcing
   filters. The transformation is known only from the reference
   implementation, on which `bsdtar` agrees; file offsets of 64 MiB and
   more were not tested with any tool.
2. **x86 filters at file offsets of 2 GiB and more.** The archiver
   avoids them. The rule (offset reduced modulo 2 to the power 24,
   computed without 32-bit overflow) is supported by `bsdtar` and
   contradicted by `unar` (re-checked independently at offset
   2,147,484,648: section 13.3, row X3). Nothing was tested at 4 GiB and
   beyond (row X4).
3. **Distance slots 65..79 and dictionaries above about 4.8 GiB.** Not
   exercised; the slot rule and the reading of more than 27 direct bits
   as one field are extrapolated. Slots 49, 51, 54..56, 58 and 60 are
   untested individually.
4. **Bit 5 of the block flags** (R4). Always 0 in real data; `bsdtar`
   treats a set bit as an error. If a later RAR version gives it a
   meaning, members using it will fail their checksum here.
5. **Members of unknown size.** The flag was obtained only on the
   earlier pieces of split members archived from a pipe, whose last
   piece states the size (section 2); such members decode correctly with
   U treated as unknown, so the block chain alone delimits a member. A
   member whose *final* header still carries the flag was not obtained:
   the archiver fills in the size whenever it can seek in the archive.
6. **A stored member with the solid flag clear between compressed
   members of one group** (R16). In the one real archive with stored
   members inside a group they carry the flag. The checksum of the
   member after it shows whether the rule holds for a given archive.
7. **Filter limits** (R8, R9). 4 MiB and 8,192 are choices; real data
   stays at 65,536 bytes and 52 queued. Zero-length filters, filters
   never reached and filters crossing a member boundary were not seen.
8. **Codes across algorithm versions** (R15) and **compressed service
   data** (R2): not seen.

## 18. Evidence

**Reference implementation.** XADMaster, revision `7cb9ee0a`: the RAR 5.0
decompression handle, the filter transformations, the shared
sliding-window, prefix-code and bit-reader helpers, and the parts of the
RAR 5 archive parser that read the compression information field, group
files into solid streams and concatenate the pieces of split files. Kept
outside this repository.

**Vendor document.** The "RAR 5.0 archive format" technical note
published on the vendor's site (the compression information field of
section 2.1; it refers to the unrar source for everything else, and that
source was not opened).

**Independent re-statement.** Everything in sections 3 to 11 was
implemented as a throw-away experimental decoder, and the stream syntax
as a throw-away encoder for building test archives (both kept outside
the repository, not part of any deliverable); the figures below come
from running them. The filter transformations were written a second time
from the text of section 10.3 to compute the expected outputs of the
crafted tests.

**Real archives.** 35 archives made for this survey with the vendor's
command-line archiver, used purely as a black-box creator: RAR 7.23 for
macOS (arm64; `rarmacos-arm-723.tar.gz` from `www.rarlab.com`) and
RAR 5.91 for macOS (x64; `rarosx-5.9.1.tar.gz` from the same site), plus
two third-party RAR 5.0 archives found on the specification writer's
machine. The inputs were a 2.5 MB word list, two x86-64 executables, an
ARM64 executable, synthetic 32-bit ARM code, synthetic 16-bit stereo
audio, a synthetic RGB bitmap, a table of binary records, zeros, random
bytes, a 6-byte file and an empty file; a 4.3 GB and a 4.7 GB file assembled from them;
with every method 1..5, dictionaries from 128 KiB to 6 GiB, solid and
not, in volumes of 200 and 300 KiB, with filters forced on, forced to
7 and to 31 delta channels, and off, and with files added to existing
solid archives using other settings.

- 737 compressed members; **all decode to the CRC-32 in their header**.
  457 of them are in the two third-party archives.
- 9,006 blocks; 8,983 carry tables; 23 members (all continuation members
  of solid groups) start with a block without tables. Block sizes 4 to
  31,423 bytes; size fields of 1 and 2 bytes; every value of k; padding
  bits always zero; nothing after the last block; no item crossing a
  block end.
- Pre-code: zero-run escape 7,868 times, length 15 never. Pre-code
  symbols 16, 17, 18, 19 used 76,368, 17,083, 15,541 and 32,657 times.
  No over-subscribed, single-symbol or otherwise incomplete code; empty
  distance, low-distance and length codes as in section 4.2.
- 88.0 million literals, 26.8 million new-distance matches, 3.5 million
  register matches (R0..R3: 1.53, 1.09, 0.50, 0.39 million), 4.7 million
  repeats, never a repeat without a last length. All 44 length slots;
  distance slots 0..48, 52, 53, 57, 61, 62, 63 (version 0) and 64
  (version 1). Longest match 4,097; largest distance 4,617,928,704.
- 7,590 filters: 2,418 delta (channel counts 1, 2, 3, 4, 6, 7, 8, 9,
  11, 12, 16, 24, 25, 31, 32), 4,820 x86 E8, 352 x86 E8/E9, no ARM.
  Lengths 256..65,536; block start offsets up to 2,744,320; at most 52
  queued at once; 5,655 start where the previous one ends; none left
  over at the end of a member. x86 filters at file offsets from 0 to
  just under 2 GiB, in first and in later members of solid groups.
- Dictionary sizes 128 KiB, 512 KiB, 1, 4, 16 and 32 MiB and 4 GiB
  (version 0); 4,429,185,024 and 4,831,838,208 (version 1). No member
  uses a distance above its own D.
- Algorithm version 1: a 4.7 GB file archived with a 6 GiB dictionary
  (one member, 667 blocks, 85 uses of distance symbol 64), and a solid
  archive first written as version 0 and then extended with a 4.3 GB
  file and a 6 GiB dictionary: its two old members became "version 1
  with the compatibility bit" and still decode with 64 distance
  symbols, the two new members are version 1 and decode with 80, one of
  them in 763 packed bytes by reaching 4.3 GiB back into the group.
- Solid groups: 128 continuation members, in groups of up to 12
  members; stored members with the
  solid flag in the middle of a group (transparent, section 11.2);
  empty members with P = 0 in the middle; an empty first member with
  P = 20 carrying the tables; a group whose members declare 1 MiB,
  1 MiB and 128 KiB.
- Split members: three volume sets (12, 12 and 18 volumes), members in
  up to 7 pieces, decoded from the concatenated pieces.
- Never observed: three size bytes, S = 0, bit 5 of the flags, a block
  without tables other than the first of a continuation member, a
  pre-code length of 15, a run passing the end of a list, a distance
  beyond the output so far or beyond D, an unfilled register, a filter
  of type 3 or above, an x86 filter at 2 GiB or beyond, algorithm
  version 1 below a 4 GiB dictionary.

**Crafted archives given to `unar` 1.10.7 and `bsdtar` 3.5.3** (each
compared with the output predicted by this document; identical in every
case where the document does not deliberately differ and the tool
implements the feature; sections 13 and 15 list the differences): block
chains with 1, 2 and 3 size bytes, the invalid size count, empty blocks,
blocks without tables, wrong checksums, wrong bit counts, truncation,
trailing bytes, a missing last block; pre-code lengths of 15, zero runs
of every length class and past the end; all four run symbols at their
extremes and past the end; a repeat at position 0; over-subscribed,
single-symbol, incomplete and empty codes; every length slot at both
ends through both codes; distance slots 0..41 at both ends, slot 50 and
slot 59; the three adjustment boundaries; the largest length; register
order and the fact that register matches are unadjusted; symbol 257
after adjusted matches and before any match; unfilled registers;
distances beyond the output, at D and beyond D; delta filters with 1,
2, 3, 4, 7, 31 and 32 channels and ranges of 1 byte to 16 MiB; x86 filters
over every case of the table in section 10.3 at offsets 0, 200, 1000
and 2 GiB; ARM filters at six offsets; filter integers of 1 to 4 bytes;
queued, adjacent, overlapping, out-of-order, zero-length, unreachable
and cut-short filters; 1,000 to 20,000 pending filters; a filter longer
than the window and one crossing the ring's wrap; solid groups with
inherited codes, registers, last length and window, with stored and
empty members in between, with growing and shrinking D, with filters in
the second member and across the member boundary; version and method
values outside the defined range; U smaller and larger than the output;
unknown U.

`7z`/`7zz` could not be used: the builds available do not include a RAR
decoder.

**Independent verification.** A second member of the specification
team, before reading the reference implementation, wrote a second
throw-away decoder from sections 2 to 11 of this document alone (again kept outside
the repository) and ran it on:

- every vector of section 13 (all valid vectors give the stated CRC-32,
  including V8 rebuilt from its description; all rejection vectors are
  rejected), with the bit positions of the items in sections 12.1, 12.2,
  12.4 and 12.6 compared against a trace;
- all archives of the survey above, including the three volume sets,
  the version 1 archives (window limit raised for the purpose: with the
  default limit of 1 GiB they end as *unsupported* at the first distance
  above it, as section 9.2 says), the archive that mixes both versions
  in one group, and both third-party archives: every compressed member
  matched its stored CRC-32;
- ten further archives (two of them sets of four volumes) made for the
  purpose with RAR 5.91 and RAR 7.23: stored members followed by
  compressed ones in a solid archive (the first compressed member then
  has the solid flag clear), an empty
  first member of a solid archive (20 packed bytes with the tables, from
  both versions), an empty file in a non-solid archive (method 0, P =
  0), and standard input archived into volumes (the unknown-size flag of
  section 2).

It needed no guess to decode genuine data. The points where the text
had to be made more exact are the order of the distance checks (9.2),
the definition of "pending" for the filter limit (8.7) and the ARM
offset arithmetic (10.3). All valid vectors were wrapped in archives
again and given to `unar` and `bsdtar`, with the results listed in the
"Checked with" column; vectors V15 to V21, E25 to E27 and section 13.3
were added in this pass, together with crafted streams of 16 MiB and
2 GiB of output for rows X2 and X3.

## Clean-room feedback

Notes from the implementation of `src/formats/dos/rar_native/rar5.rs`,
written from this document alone.

**Result.** Every vector of section 13 behaves as stated: V1 to V21
(V8 included, also with the window limit lowered to 1 MiB, where it ends
as *unsupported* at the first long distance), E1 to E27, and every row
of section 13.3. No rule had to be guessed to decode genuine data.

**Real archives (black box).** Three RAR 5.0 archives found on the
implementer's machine were decoded member by member and each output
compared with the CRC-32 of its file header: one archive with a single
member, and two third-party archives that `lsar` lists with 363 and 94
compressed members. All match.

**Points where the document leaves room, and what was chosen.**

1. *An empty member (P = 0) with the solid flag when the group has
   already failed, or when no group exists.* Sections 11.1 and 11.2 say
   such a member does not touch the state; they do not say whether it is
   reported as failed in these two situations. Chosen: it is delivered
   as an empty member in both.
2. *P = 0 with U greater than 0 and the solid flag.* Corrupt input by
   11.1. Whether that failure poisons the group is not said (the state
   was not touched). Chosen: it does, as for every other failed member
   (11.4 read literally).
3. *A continuation member without a predecessor* (R17). The document
   calls it "an error for that member" without naming the kind. Chosen:
   *unsupported* rather than corrupt input, because the data may be
   intact (a later volume opened alone).
4. *The caller's output limit against a known U.* When U is larger than
   the caller's limit the member is not refused up front; decoding stops
   with an error at the first item that would pass the limit.
5. *D below 128 KiB.* The container cannot express it, but the decoder
   interface can be handed any number. Chosen: check 2 of 9.2 uses D as
   given; the window capacity is never below 128 KiB.
6. *Where an over-long table transmission is detected* (6.3 step 2).
   Bits past the block end read as zero, so the transmission is read to
   its end (or to the first invalid code) and the position is compared
   with the block end afterwards. The outcome is corrupt input either
   way; only the reported reason can differ.
7. *An item that runs past the block end.* For matches and filter
   declarations the position is tested before the item takes effect, for
   a literal after it. Since nothing is delivered for a failed member
   and its group state is discarded, this is not observable.
8. *The window limit M on a 32-bit target.* "Up to what the platform can
   address" was taken as half the address space.

**Small inconsistencies noticed.**

- The introduction gives the largest dictionary of algorithm version 1
  as 1 TiB; section 9.3 says "about 2 TiB" (which is what N = 23 with
  F = 31 gives). Nothing depends on it.
- Section 8.7 says a filter's start "cannot be below" the end of an
  applied earlier filter. That holds, but an implementation still needs
  the end of the most recently declared filter of the member after that
  filter has left the queue; a sentence saying so would save a reader
  the deduction.

**Implementation notes that may help the next reader.**

- Check 4 of 9.2 needs no separate count of held bytes. It is enough to
  remember, when the capacity rises on a ring that has already wrapped,
  the LZ position of the oldest byte kept; a distance is then within
  the held history when it does not exceed the group count minus that
  position, and checks 2 and 3 already bound it by the capacity.
- With the member output collected in one buffer, the output stage of
  10.2 can apply each transformation in place on that buffer once the
  range is complete, which also covers ranges longer than the window
  (9.4) without a second copy; only the delta transformation needs
  scratch space.
