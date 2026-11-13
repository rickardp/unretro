# RAR 2.9 compression, LZ part (unpack versions 29 and 36)

Format specification for clean-room implementation. This document describes
the compression method used by RAR 2.9, 3.x and 4.x for the files of a
RAR4-format archive whose file header carries **unpack version 29 or 36**:
a sliding-window LZ77 coder with a 4 MiB window and Huffman-coded symbols,
which can switch, block by block, to a PPMd text model, and whose output
can be post-processed in places by small *filters* transmitted in the
stream.

It specifies the framework and the LZ coder completely: the bit stream,
blocks and how their type is chosen, table transmission, every LZ item, the
window, where filters are placed and when their output replaces window
output, the end of a member, solid groups and volumes. Two subjects are
specified elsewhere and only their interfaces are defined here:

- the PPMd model and its range decoder: `rar29-ppmd.md` (interface:
  section 9);
- the contents of a filter record, the filter programs and their virtual
  machine: `rar29-filters.md` (interface: section 11).

It does not describe the archive container (`rar4-container.md`), the
RAR 2.0 method (`rar20.md`), RAR5 (`rar5-container.md`,
`rar5-compression.md`), or encryption.

Contents: 1 how to read this document; 2 what the decoder is given and
returns; 3 the bit stream; 4 prefix codes; 5 decoder state; 6 blocks; 7
LZ table transmission; 8 LZ items; 9 PPMd blocks (interface); 10 the
window; 11 filters (interface); 12 members, solid groups, volumes and end
of data; 13 worked examples; 14 test vectors; 15 safety requirements; 16
observed implementation gaps; 17 decisions; 18 open questions; 19
evidence.

## 1. How to read this document

### 1.1 Provenance

- This is a **prose specification**. It was derived by studying the
  behaviour of an existing open decoder (the RAR 3 support of The
  Unarchiver's XADMaster library, LGPL, revision `7cb9ee0a` of
  `github.com/MacPaw/XADMaster`; called the *reference implementation*),
  by black-box testing of the extraction tool built from that library
  (`unar`/`lsar` 1.10.7) with purpose-built archives, by decoding real
  archives, and by decoding archives written for the purpose with the
  vendor's own archiver used as a black box (section 19).
- The official `unrar` source was **not** consulted; its licence forbids
  using it to recreate the algorithm. No other RAR decoder source was
  consulted either.
- The work is a **clean-room process**. The author of this document could
  read the reference implementation; the implementation is written **from
  this document only**, by implementers who do not read the source of any
  RAR decoder. The document therefore contains no program code and no
  pseudo-code, only prose, tables of constants, worked examples and test
  vectors.
- Where the reference implementation is wrong or unsafe the document says
  so (section 16) and specifies the correct behaviour instead. Being
  compatible with the reference implementation is not a goal; reading
  genuine archives correctly and rejecting hostile input safely is.

### 1.2 Evidence labels

| Label | Meaning |
|---|---|
| CONFIRMED (real data) | The rule was exercised when decoding archives written by RAR itself (public archives, and archives made with the vendor's RAR 6.24 in RAR4 mode; section 19), and every decoded file matched the CRC-32 stored in its header. |
| CONFIRMED (crafted) | Checked by giving `unar` a purpose-built archive and comparing its output with the output predicted by this document. Where the text adds "both tools", a second, unrelated extractor (`bsdtar` 3.5.3, used as a black box) gave the same result. |
| REFERENCE-ONLY | Stated by the reference implementation; not separately observable. |
| INFERRED | Deduced; not confirmed. |
| DECISION | A choice made by this specification, not a fact about the format (section 17). |

A rule without a label in sections 3 to 12 is CONFIRMED (real data).

### 1.3 Terms and numbers

| Term | Meaning |
|---|---|
| member | One file stored in the archive, after the container layer has joined the pieces of a file that is split across volumes. |
| packed bytes | The data area of a member as stored (all pieces concatenated). **P** is their number. |
| **U** | The unpacked size the member's header declares. Untrusted. |
| solid group | A maximal run of members that share one decoder state (section 12.2). A member of an archive that is not solid is a group of one. |
| block | A stretch of a member's stream that starts with a block header (section 6) and is either an LZ block or a PPMd block. |
| item | One decoding step: a literal, a match, a filter record, a block switch or an end marker. |
| match | An instruction to copy *length* bytes starting *distance* bytes back in the output. |
| LZ position | The number of bytes the group has produced so far, counted before any filter is applied. The first byte of a group has LZ position 0. A 64-bit count. |
| member start | The LZ position at which a member's first byte is produced. |
| pre-code | The 20-symbol prefix code used only to transmit code lengths. |
| length table | The array of 404 code lengths the decoder keeps between table transmissions. |
| register | One of four remembered match distances (section 8.5). |
| end marker | The item that ends a member's stream (sections 8.6 and 9.2). |
| filter | A transformation applied to one stretch of the output after it has been decoded (section 11). |
| corrupt input | The decoder's error result. No contents are delivered for the member. |

Numbers are decimal unless prefixed `0x`. Byte strings are written as
hexadecimal digits in code style (`0a ac`). Ranges written `a..b` include
both ends. "Bit n" of a stream counts from 0 in reading order (section 3).
1 MiB is 1,048,576 bytes.

## 2. What the decoder is given and what it returns

The container layer (`rar4-container.md`) hands the decoder, for each
member whose unpack version is 29 or 36 and whose method byte is
`0x31`..`0x35`:

- the packed bytes of the member (P bytes; the pieces of a split file
  already concatenated in volume order, section 12.5; deciphered if the
  member was encrypted);
- U;
- whether the member **continues the solid group** of the preceding
  member (file header flag bit `0x0010`, section 12.2);
- the CRC-32 stored in the header of the member's last piece.

It returns exactly U bytes, or the error "corrupt input", and it says
whether the group's state can be continued by a further member (section
12.4).

The same decoder also unpacks a packed archive comment (the `CMT`
sub-block of `rar4-container.md`, which carries unpack version 29): the
container layer presents it as a member that starts a group of its own,
and its state is thrown away afterwards. Nothing in this document is
special for it.

Header facts this decoder relies on (the container document is
authoritative for the header layout; all CONFIRMED (real data) unless
noted):

- **Unpack version** is the byte at offset 24 of the file header block
  (counting the block's CRC field as offset 0), **method** the byte at
  offset 25. Method `0x30` means stored: the packed bytes are the
  unpacked bytes and this decoder is not involved. Methods `0x31`..`0x35`
  are compression levels of the *encoder*; the decoder ignores the value.
  `unar` also decodes method `0x36` identically (CONFIRMED (crafted));
  DECISION: accept `0x31`..`0x35` only.
- Versions 29 and 36 select this decoder, and there is **no difference
  between them** in the stream format (REFERENCE-ONLY; `unar` decodes a
  crafted version-36 member exactly like the same member marked 29,
  CONFIRMED (crafted); every one of the 1,422 real members surveyed is
  marked 29, section 18). Versions 30 and 35 are *not* accepted by `unar`
  (CONFIRMED (crafted)). DECISION: only 29 and 36 are handled by this
  decoder.
- File header flag bits `0x00e0` give the **dictionary size** the encoder
  used: value 0 = 64 KiB, 1 = 128 KiB, 2 = 256 KiB, 3 = 512 KiB, 4 =
  1024 KiB, 5 = 2048 KiB, 6 = 4096 KiB; value 7 marks a directory entry
  (no data). (Vendor technical note `technote.txt`, section 19.) All
  seven sizes occur in the surveyed archives and no match distance
  exceeds the declared size. The decoder does not need the value: it
  always uses a 4 MiB window (section 10). DECISION: the declared
  dictionary size is not enforced.
- The **CRC-32** is the common one (polynomial `0xEDB88320` reflected,
  initial value and final XOR `0xFFFFFFFF`; the ASCII string `123456789`
  gives `0xCBF43926`), computed over the member's U unpacked bytes
  **after filters have been applied**. For a file split across volumes
  the header of the last piece holds it (section 12.5).
- DECISION: after decoding, the container layer compares the CRC-32 of
  the U bytes with the stored value and reports a mismatch as an error
  for that member ("checksum mismatch"); the bytes are not delivered as
  good contents. A mismatch does not by itself invalidate the group's
  state.

## 3. The bit stream

- The packed bytes of one member form one stream. In LZ blocks and in
  block headers it is read as a **bit stream**: bits are taken from each
  byte starting with the **most significant bit**. Bit 0 of the stream is
  the top bit of the first byte.
- A field of n bits is read as n successive bits; the **first bit read
  is the most significant** bit of the field's value. Fields in this
  format are 1 to 16 bits wide. A field of 0 bits reads nothing and has
  the value 0.
- **Byte alignment happens in exactly one place: immediately before a
  block header** (section 6.2). There the reader skips to the next byte
  boundary: if the number of bits consumed so far is not a multiple of
  8, the remaining 1 to 7 bits of the current byte are discarded; if it
  is a multiple of 8, nothing is skipped. The discarded bits are zero in
  every real stream examined (9,161 padded headers); their value is not
  checked (CONFIRMED (crafted): a stream with all-ones padding decodes,
  vector V4). Nothing else in an LZ block is aligned: items follow one
  another bit by bit, and so do the bytes of a filter record.
- Inside a **PPMd block** the stream is consumed as **whole bytes** by
  the PPMd range decoder (section 9). A PPMd block's data begins on a
  byte boundary (its header is a whole number of bytes read from an
  aligned position), and when a PPMd block ends, the next thing read
  starts at the first byte the range decoder has not consumed.
- **Reading a bit or byte that lies beyond the last packed byte is
  corrupt input**, with one tolerated exception stated in section 12.1
  (after the member's output is already complete). An implementation
  that decodes prefix codes by looking ahead a fixed number of bits must
  pad the look-ahead with zero bits and fail only if the bits actually
  consumed extend past the end. CONFIRMED (crafted).
- After the end marker of a member, the rest of its packed bytes is not
  read. In LZ mode 0 to 7 unused bits remain in the last byte and are
  zero in all 1,140 real cases; after a member that ends in a PPMd
  block several whole bytes remain unread. DECISION: whatever follows
  the end marker is neither read nor checked (CONFIRMED (crafted) that
  both tools ignore it, vector N4).

Each member has **its own stream starting at bit 0 of its own packed
bytes**, also inside a solid group (section 12.2).

## 4. Prefix codes

All Huffman codes in the format (the pre-code and the four LZ codes) are
built the same way from an array of code lengths, one per symbol, each
length 0..15. Length 0 means the symbol does not occur and has no code.

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

Example: lengths 2, 2, 3, 3, 2 for the symbols 97, 98, 99, 256 and 271
(all others 0) give 97 = `00`, 98 = `01`, 271 = `10`, 99 = `110`,
256 = `111`. Lengths 1, 2, 3, 3 for symbols 19, 2, 1, 3 give 19 = `0`,
2 = `10`, 1 = `110`, 3 = `111`.

### 4.2 Codes that are not complete, and codes that are impossible

Let the *weight* of a code be the sum, over the symbols with a non-zero
length L, of 2 raised to the power (15 − L). A complete code has weight
32768.

- **Weight above 32768 (over-subscribed)**: the lengths cannot form a
  prefix code. This is corrupt input **as soon as the table is read**,
  whether or not the code would ever be used (vectors E2, E3, E4). (In
  the assignment procedure it shows as the running value reaching 2 to
  the power L while a symbol of length L still needs a code.) CONFIRMED
  (crafted, both tools); never seen in real data.
- **Weight below 32768 (incomplete)**: accepted. Real data contains it
  in exactly one form: a code with **no symbols at all** (in the 11,838
  real LZ table transmissions the distance code is empty 2,760 times,
  the low-distance code 3,110 times and the length code 2,889 times: the
  block simply contains no item that would use it). Every non-empty real
  code is complete; in particular the encoder never sends a code with a
  single symbol. Other incomplete codes (a single symbol of length 1,
  whose code is the bit `0`; any other under-full set of lengths) are
  accepted by `unar` (CONFIRMED (crafted)) and rejected by `bsdtar`.
  DECISION: accept them.
- When, while decoding a symbol, the bits read match no assigned code
  (always the case for an empty code; any unassigned pattern of an
  incomplete code), the result is corrupt input (vectors E11, E12).
  CONFIRMED (crafted).

A decoder never needs more than 15 bits to decode one symbol.

## 5. Decoder state

One decoder state exists per solid group. Everything in it is set to the
initial value when a group starts (section 12.2) and is carried from
member to member inside the group.

| Item | Contents | Initial value |
|---|---|---|
| window | 4,194,304 bytes of most recent unfiltered output (section 10) | empty |
| LZ position | bytes produced in this group so far (64-bit) | 0 |
| header-pending flag | whether the next member's stream begins with a block header (section 12.2) | set |
| block type | LZ or PPMd | none; set by the first block header |
| length table | 404 code lengths, each 0..15 (section 7) | all 0 |
| current codes | main, distance, low-distance and length code | none; set by an LZ block header |
| registers 0..3 | four match distances, register 0 the most recent (section 8.5) | all unset |
| last distance, last length | distance and length of the most recent LZ match | none |
| low-distance value, low-distance repeat counter | section 8.2 | 0 and 0 |
| PPMd model | the model of `rar29-ppmd.md` with its memory | none |
| PPMd escape value | a byte value (section 9) | 2 |
| filter state | the stored filter programs with their per-program memory, the number of the last program used, the queue of filters waiting to run and the order mark T (section 11) | no programs, empty queue, T = 0 |

Not part of the carried state: the position in the bit stream (each
member starts reading at bit 0 of its own packed bytes) and the state of
the PPMd range decoder (it is started afresh by every PPMd block header).

What resets what, in one place:

| Event | Effect on the state |
|---|---|
| Start of a group | everything to its initial value |
| LZ block header | low-distance value and repeat counter become 0; the length table is cleared first if the keep flag is 0; the four codes are rebuilt. Nothing else. |
| PPMd block header | the PPMd model is created anew if the header says so; the escape value is replaced if the header carries one; the range decoder is restarted. Nothing else: the length table, the registers, the last match and the low-distance state are untouched. |
| End marker, start of a further member of the group | the end marker sets or clears the header-pending flag (section 12.2). Nothing else; in particular filters queued for positions not yet reached stay queued (section 11.4). |

In particular the registers, the last match, the length table, the PPMd
model and escape value and the stored filter programs **survive block
changes and member boundaries**; the low-distance value and counter
survive member boundaries when the new member does not begin with an LZ
block header. (CONFIRMED (real data) for the length table across PPMd
blocks and across members, for the PPMd model and escape value across
members, and for filter programs across members; CONFIRMED (crafted) for
the registers, the last match and the low-distance state: vectors V5,
V7b, V7d.)

## 6. Blocks

### 6.1 When a block header is read

A block header is read

1. at the very start of the stream of a member **when the
   header-pending flag is set**; this is always the case for the first
   member of a group that has packed bytes;
2. in an LZ block, after main symbol 256 followed by a `1` bit (section
   8.6);
3. in a PPMd block, after the escape value followed by the code 0
   (section 9.2).

Nothing else triggers one. A member that starts with the flag clear
begins directly with LZ items, decoded with the codes the previous member
left. There is no limit on the number of blocks in a member; real
encoders start a new LZ block every few thousand to few tens of thousand
items.

### 6.2 The block header

First **align to a byte boundary** (section 3). Then read one bit, the
**block type**: 0 means an LZ block, 1 a PPMd block. Then:

**LZ block.** The table transmission of section 7 follows immediately
(its first field is the keep flag, bit 1 of the same byte). The block
type becomes LZ.

**PPMd block.** Read the remaining 7 bits of the byte; together with the
block-type bit they form one header byte H whose top bit is 1. Then, as
whole bytes, in this order:

| Condition | Byte | Meaning |
|---|---|---|
| bit `0x20` of H set | M | The PPMd model is discarded and a new one is created with (M + 1) MiB of model memory and the order given by the low five bits of H (below). |
| bit `0x40` of H set | E | The PPMd escape value becomes E. |

- With bit `0x20` set, let v be the low five bits of H (`0x1f`). The
  model order is v + 1 when that is 16 or less, and 16 + 3 × (v + 1 − 16)
  otherwise (so v = 16 gives 19 and v = 31 gives 64). **v = 0 (order 1)
  is corrupt input** (vector E16).
- With bit `0x20` clear, the existing model is kept as it is, with
  everything it has learnt, and the low five bits of H are ignored
  (they are zero in real data). **If no model exists in this group, this
  is corrupt input** (vector E15).
- With bit `0x40` clear the escape value is unchanged (2 if none was
  ever set in this group).
- The bytes are read in the order of the table (M before E) and both are
  read before either check above is made; the checks are made **before
  any model memory is obtained** (section 15).
- Then the range decoder is started on the bytes that follow, as
  `rar29-ppmd.md` describes, and the block type becomes PPMd.
- The LZ part of the state is not touched: after the PPMd block, an LZ
  block header with the keep flag set builds on the length table as the
  last LZ block left it, and a symbol 258 or 259..262 in that later block
  still refers to the last LZ match and the registers (vector V7b).

All four combinations of the two bits occur in real data (`0x60` 81
times, `0x20` 53, `0x00` 49, `0x40` 5), also in members of a solid group
whose model was created by an earlier member.

## 7. LZ table transmission

### 7.1 Layout

In reading order, directly after the block-type bit:

| Bits | Field | Meaning |
|--:|---|---|
| 1 | keep flag | 0: set all 404 entries of the length table to 0 before reading the new lengths. 1: keep the table as it is; the new lengths are coded as differences from it. |
| variable | pre-code lengths | The code lengths of pre-code symbols 0, 1, ..., 19 (section 7.2). |
| variable | coded lengths | All 404 entries of the length table, coded with the pre-code (section 7.3). |

Reading a table transmission also sets the low-distance value and the
low-distance repeat counter to 0 (section 8.2).

The keep flag is 1 in 9,160 of the 11,838 real transmissions, including
transmissions that follow a PPMd block (40 cases) and the first
transmission of a later member of a solid group (4 cases): the length
table is carried through both. It is never 1 on the first transmission
of a group in real data; if it is, the table is all zeros anyway and the
flag has no effect.

### 7.2 The pre-code lengths

The 20 lengths are filled in order of symbol number. Until all 20 are
known, read a 4-bit field v:

- v = 0..14: the next pre-code symbol has length v.
- v = 15: read another 4-bit field z.
  - z = 0: the next pre-code symbol has length **15**.
  - z = 1..15: the next **z + 2** pre-code symbols (3..17 symbols) have
    length 0. If fewer than z + 2 symbols remain, the run is cut off at
    symbol 19 and the excess is ignored (CONFIRMED (crafted, both
    tools); not seen in real data).

The pre-code is then built as in section 4. An over-subscribed pre-code
is corrupt input (vector E2). An incomplete pre-code is accepted (real
encoders always send a complete one; `unar` accepts an incomplete one,
CONFIRMED (crafted)). The zero-run form is used in 10,114 real
transmissions; a pre-code length of 15 does not occur in real data
(CONFIRMED (crafted, both tools), vector V4).

### 7.3 The coded lengths

The decoder fills positions 0, 1, ..., 403 of the length table, in order.
Let i be the next position to fill. Until i reaches 404, decode one
pre-code symbol and act on it:

| Pre-code symbol | Extra bits read after it | Action |
|--:|--:|---|
| 0..15 | none | Position i receives (its present value + the symbol) modulo 16. i advances by 1. |
| 16 | 3, value r | The value now stored at position i − 1 is copied into the next r + 3 positions (3..10 positions). **If i is 0 this is corrupt input.** |
| 17 | 7, value r | The value now stored at position i − 1 is copied into the next r + 11 positions (11..138). **If i is 0 this is corrupt input.** |
| 18 | 3, value r | The next r + 3 positions (3..10) are set to 0. |
| 19 | 7, value r | The next r + 11 positions (11..138) are set to 0. |

Details:

- "Its present value" is whatever the length table holds at that
  position: 0 if the keep flag was 0, otherwise the length left there by
  the previous transmission of the group. With the keep flag set, a
  symbol 0 therefore means "unchanged" and a symbol 15 means "one less,
  wrapping from 0 to 15".
- Symbols 18 and 19 store 0 outright and symbols 16 and 17 store a copy;
  the "add modulo 16" rule applies to symbols 0..15 only.
- For symbols 16 and 17, "the position before" is simply position i − 1
  of the array, also when that position belongs to a different code (for
  example the last main-code length when i is 299).
- A run (symbols 16..19) that would go past position 403 is **cut off at
  403**; the excess is ignored and the transmission is complete. Not seen
  in real data; accepted by both tools (CONFIRMED (crafted)). DECISION:
  accept and cut off.
- The transmission ends when i reaches 404; no terminator follows. The
  first item of the block follows immediately, without alignment.

### 7.4 Building the codes

After the lengths are read, four codes are built from the length table as
in section 4. An over-subscribed code is corrupt input.

| Code | Length-table positions | Symbols |
|---|---|--:|
| main code | 0..298 | 299 |
| distance code | 299..358 | 60 |
| low-distance code | 359..375 | 17 |
| length code | 376..403 | 28 |

## 8. LZ items

In an LZ block, decode one symbol with the **main code** and act on it;
repeat until the member ends (section 12.1).

| Main symbol | Kind | What follows in the stream | Section |
|--:|---|---|---|
| 0..255 | literal | nothing. Output the byte whose value is the symbol. | |
| 256 | end-of-block symbol | 1 or 2 bits | 8.6 |
| 257 | filter record | a flags byte, a size, the record bytes | 8.7 |
| 258 | repeat last match | nothing | 8.4 |
| 259..262 | match with the distance of register 0..3 | a length-code symbol, then its extra bits | 8.3 |
| 263..270 | two-byte match with a short distance | extra bits | 8.4 |
| 271..298 | match with an explicit distance | length extra bits, then a distance-code symbol, then its extra bits | 8.1, 8.2 |

### 8.1 Symbols 271..298: match with an explicit distance

1. The **length slot** is the symbol minus 271 (0..27). Read the slot's
   extra bits (table below). The length so far is base + extra + **3**.
2. Decode one symbol with the **distance code**: the distance slot
   (0..59). Obtain the slot's extra value as section 8.2 describes. The
   distance is base + extra + **1**.
3. Adjust the length by the distance: add 1 if the distance is 8,192
   (`0x2000`) or more, and add 1 more if it is 262,144 (`0x40000`) or
   more.

The resulting length is 3..260 and the distance 1..4,194,304. The
adjustment of step 3 applies to this kind of match **only**.

Length slots (used for main symbols 271..298 and, in section 8.3, for the
symbols of the length code):

| Slot | Main symbol | Base | Extra bits | Base + extra |
|--:|--:|--:|--:|--:|
| 0 | 271 | 0 | 0 | 0 |
| 1 | 272 | 1 | 0 | 1 |
| 2 | 273 | 2 | 0 | 2 |
| 3 | 274 | 3 | 0 | 3 |
| 4 | 275 | 4 | 0 | 4 |
| 5 | 276 | 5 | 0 | 5 |
| 6 | 277 | 6 | 0 | 6 |
| 7 | 278 | 7 | 0 | 7 |
| 8 | 279 | 8 | 1 | 8..9 |
| 9 | 280 | 10 | 1 | 10..11 |
| 10 | 281 | 12 | 1 | 12..13 |
| 11 | 282 | 14 | 1 | 14..15 |
| 12 | 283 | 16 | 2 | 16..19 |
| 13 | 284 | 20 | 2 | 20..23 |
| 14 | 285 | 24 | 2 | 24..27 |
| 15 | 286 | 28 | 2 | 28..31 |
| 16 | 287 | 32 | 3 | 32..39 |
| 17 | 288 | 40 | 3 | 40..47 |
| 18 | 289 | 48 | 3 | 48..55 |
| 19 | 290 | 56 | 3 | 56..63 |
| 20 | 291 | 64 | 4 | 64..79 |
| 21 | 292 | 80 | 4 | 80..95 |
| 22 | 293 | 96 | 4 | 96..111 |
| 23 | 294 | 112 | 4 | 112..127 |
| 24 | 295 | 128 | 5 | 128..159 |
| 25 | 296 | 160 | 5 | 160..191 |
| 26 | 297 | 192 | 5 | 192..223 |
| 27 | 298 | 224 | 5 | 224..255 |

Distance slots (symbols of the distance code). "Direct bits" is the
number of bits read from the stream as a plain field; "low part" says
whether four further bits come from the low-distance mechanism of
section 8.2:

| Slot | Base | Extra bits | Direct bits | Low part | Distance (base + extra + 1) |
|--:|--:|--:|--:|---|--:|
| 0 | 0 | 0 | 0 | no | 1 |
| 1 | 1 | 0 | 0 | no | 2 |
| 2 | 2 | 0 | 0 | no | 3 |
| 3 | 3 | 0 | 0 | no | 4 |
| 4 | 4 | 1 | 1 | no | 5..6 |
| 5 | 6 | 1 | 1 | no | 7..8 |
| 6 | 8 | 2 | 2 | no | 9..12 |
| 7 | 12 | 2 | 2 | no | 13..16 |
| 8 | 16 | 3 | 3 | no | 17..24 |
| 9 | 24 | 3 | 3 | no | 25..32 |
| 10 | 32 | 4 | 0 | yes | 33..48 |
| 11 | 48 | 4 | 0 | yes | 49..64 |
| 12 | 64 | 5 | 1 | yes | 65..96 |
| 13 | 96 | 5 | 1 | yes | 97..128 |
| 14 | 128 | 6 | 2 | yes | 129..192 |
| 15 | 192 | 6 | 2 | yes | 193..256 |
| 16 | 256 | 7 | 3 | yes | 257..384 |
| 17 | 384 | 7 | 3 | yes | 385..512 |
| 18 | 512 | 8 | 4 | yes | 513..768 |
| 19 | 768 | 8 | 4 | yes | 769..1024 |
| 20 | 1024 | 9 | 5 | yes | 1025..1536 |
| 21 | 1536 | 9 | 5 | yes | 1537..2048 |
| 22 | 2048 | 10 | 6 | yes | 2049..3072 |
| 23 | 3072 | 10 | 6 | yes | 3073..4096 |
| 24 | 4096 | 11 | 7 | yes | 4097..6144 |
| 25 | 6144 | 11 | 7 | yes | 6145..8192 |
| 26 | 8192 | 12 | 8 | yes | 8193..12288 |
| 27 | 12288 | 12 | 8 | yes | 12289..16384 |
| 28 | 16384 | 13 | 9 | yes | 16385..24576 |
| 29 | 24576 | 13 | 9 | yes | 24577..32768 |
| 30 | 32768 | 14 | 10 | yes | 32769..49152 |
| 31 | 49152 | 14 | 10 | yes | 49153..65536 |
| 32 | 65536 | 15 | 11 | yes | 65537..98304 |
| 33 | 98304 | 15 | 11 | yes | 98305..131072 |
| 34 | 131072 | 16 | 12 | yes | 131073..196608 |
| 35 | 196608 | 16 | 12 | yes | 196609..262144 |
| 36 | 262144 | 16 | 12 | yes | 262145..327680 |
| 37 | 327680 | 16 | 12 | yes | 327681..393216 |
| 38 | 393216 | 16 | 12 | yes | 393217..458752 |
| 39 | 458752 | 16 | 12 | yes | 458753..524288 |
| 40 | 524288 | 16 | 12 | yes | 524289..589824 |
| 41 | 589824 | 16 | 12 | yes | 589825..655360 |
| 42 | 655360 | 16 | 12 | yes | 655361..720896 |
| 43 | 720896 | 16 | 12 | yes | 720897..786432 |
| 44 | 786432 | 16 | 12 | yes | 786433..851968 |
| 45 | 851968 | 16 | 12 | yes | 851969..917504 |
| 46 | 917504 | 16 | 12 | yes | 917505..983040 |
| 47 | 983040 | 16 | 12 | yes | 983041..1048576 |
| 48 | 1048576 | 18 | 14 | yes | 1048577..1310720 |
| 49 | 1310720 | 18 | 14 | yes | 1310721..1572864 |
| 50 | 1572864 | 18 | 14 | yes | 1572865..1835008 |
| 51 | 1835008 | 18 | 14 | yes | 1835009..2097152 |
| 52 | 2097152 | 18 | 14 | yes | 2097153..2359296 |
| 53 | 2359296 | 18 | 14 | yes | 2359297..2621440 |
| 54 | 2621440 | 18 | 14 | yes | 2621441..2883584 |
| 55 | 2883584 | 18 | 14 | yes | 2883585..3145728 |
| 56 | 3145728 | 18 | 14 | yes | 3145729..3407872 |
| 57 | 3407872 | 18 | 14 | yes | 3407873..3670016 |
| 58 | 3670016 | 18 | 14 | yes | 3670017..3932160 |
| 59 | 3932160 | 18 | 14 | yes | 3932161..4194304 |

(Slots 0..33 follow the pattern "two slots per extra-bit count"; slots
34..47 all have 16 extra bits and bases 65,536 apart; slots 48..59 have
18 extra bits and bases 262,144 apart. For slots 10..59 the direct bits
are the extra bits minus 4. All 60 slots occur in real data.)

### 8.2 The extra value of a distance slot, and the low-distance code

**Slots 0..9**: read the slot's extra bits as one plain field (0 to 3
bits). That is the extra value. The low-distance state is neither used
nor changed.

**Slots 10..59**: the extra value has two parts.

1. The **high part**: read the slot's direct bits (0 to 14 bits; nothing
   for slots 10 and 11) as one plain field h.
2. The **low part**, a value 0..15, obtained as follows:
   - If the low-distance repeat counter is greater than 0: subtract 1
     from the counter; the low part is the low-distance value. **No
     symbol is read.**
   - Otherwise decode one symbol s with the **low-distance code**
     (0..16).
     - s = 0..15: the low part is s, and the low-distance value becomes
       s.
     - s = 16: the low part is the low-distance value (which is left
       unchanged), and the repeat counter becomes 15.

The extra value is h × 16 + low part, and the distance is base + h × 16
+ low part + 1.

So a symbol 16 says "this match and the next 15 matches that use slots
10..59 all have the same low part as the previous such match"; those 15
matches carry no low-distance symbol at all. Matches that use slots 0..9,
and items of every other kind, neither consume the counter nor change the
value.

The low-distance value and the counter are both set to 0 by every LZ
table transmission, and only then (section 5). A symbol 16 as the first
low-distance symbol after a table transmission therefore gives the low
part 0. (CONFIRMED (real data): symbol 16 occurs 49 times in the survey,
followed by 735 matches without a low symbol; CONFIRMED (crafted, both
tools) for every statement of this section; CONFIRMED (crafted, `unar`)
for the carry of value and counter into a member that starts without a
block header, vector V5.)

### 8.3 Symbols 259..262: match with the distance of a register

1. The distance is the contents of **register k**, k = symbol − 259
   (section 8.5).
2. Decode one symbol with the **length code**: a length slot 0..27. Read
   the slot's extra bits (length-slot table of section 8.1; the "main
   symbol" column does not apply). The length is base + extra + **2**.
   No adjustment by the distance is made.
3. Register k is **moved to the front**: the contents of registers
   0..k − 1 move back one place each (register k − 1 into register k, and
   so on) and register 0 receives the distance. Registers above k are
   unchanged. For k = 0 nothing moves.

The resulting length is 2..257.

**Unset register.** DECISION: a symbol 259..262 that selects a register
no match has filled yet in this group is corrupt input (vector E6). (Real
data never does it; both tools copy zero bytes, section 16.)

### 8.4 Symbols 263..270 and 258

**Symbols 263..270: two-byte match.** The length is 2. The distance is
base + extra + 1 from this table, the extra bits read as one plain field:

| Main symbol | Base | Extra bits | Distance |
|--:|--:|--:|--:|
| 263 | 0 | 2 | 1..4 |
| 264 | 4 | 2 | 5..8 |
| 265 | 8 | 3 | 9..16 |
| 266 | 16 | 4 | 17..32 |
| 267 | 32 | 5 | 33..64 |
| 268 | 64 | 6 | 65..128 |
| 269 | 128 | 6 | 129..192 |
| 270 | 192 | 6 | 193..256 |

The low-distance mechanism is not involved, and no length adjustment
applies.

**Symbol 258: repeat last match.** A match with the last distance and
the last length, exactly as stored (no adjustment is applied again). The
registers are not changed.

If no LZ match has been decoded in this group yet, symbol 258 **does
nothing**: no output, no change of state, and decoding continues with
the next item. (CONFIRMED (crafted, both tools), vector N3; never seen in
real data. DECISION: keep this behaviour.)

### 8.5 Match memory

The four registers and the last distance and length change as follows.
Nothing else changes them; in particular literals, filter records, block
headers of either type, end markers and the two match forms of a PPMd
block (section 9.2) leave them alone.

| Item | Registers | Last distance and length |
|---|---|---|
| 271..298 (explicit distance) | The distance is inserted at the front: registers 0, 1, 2 move to 1, 2, 3, the old register 3 is dropped, register 0 receives the distance. Nothing is searched or removed: the same distance may occupy several registers. | become this match's distance and its final length (after the adjustment of 8.1) |
| 263..270 (two-byte) | inserted at the front in the same way | become this distance and 2 |
| 259..262 (register k) | register k moved to the front (8.3) | become this distance and length |
| 258 (repeat) | unchanged | unchanged |

Consequences, all CONFIRMED (crafted, both tools) and exercised
throughout the real data: after explicit matches with the distances 9,
41, 4 in that order, register 0 holds 4, register 1 holds 41 and register
2 holds 9. With the registers holding (93, 61, 77, 45), symbol 262 copies
with distance 45 and leaves (45, 93, 61, 77); symbol 260 then copies with
93 and leaves (93, 45, 61, 77); a symbol 258 after it repeats that match
and leaves the registers as they are.

### 8.6 Symbol 256: end of block

Read one bit.

- **1**: a new block follows in this member. Read a block header
  (section 6.2: align, block type, and so on) and continue with the
  block it announces.
- **0**: this is the **end marker** of the member. Read one more bit: it
  is the new value of the header-pending flag (1: the next member of the
  group begins with a block header; 0: it begins directly with LZ items
  decoded with the present codes). The member's stream ends here (section
  12.1).

In the real data symbol 256 is followed by `1` 10,590 times, by `0` `1`
1,248 times and by `0` `0` 53 times.

### 8.7 Symbol 257: filter record

Read, as plain fields from the bit stream (no alignment):

1. 8 bits: the **flags** byte F.
2. The size n of the record body: let c = (F and 7) + 1.
   - c = 1..6: n = c.
   - c = 7: read 8 more bits, value b; n = b + 7 (7..262).
   - c = 8: read 16 more bits; n is their value (0..65,535).
3. n fields of 8 bits each: the record body.

F and the body are handed to the filter layer as section 11.2 describes.
The item produces no output. (CONFIRMED (real data): 3,064 records.)

### 8.8 Executing a match

A match of distance d and length n appends n bytes to the output, each
equal to the byte d positions before it in the group's unfiltered output
(so a distance smaller than the length repeats a pattern: distance 1
repeats the previous byte n times). Section 10 states the window rules
and section 15 the checks that precede the copy.

## 9. PPMd blocks (interface)

`rar29-ppmd.md` specifies the PPMd model (variant H) and its range
decoder. This document needs exactly three things from it:

- **Create a model** with a given order (2..64) and a given amount of
  model memory ((M + 1) MiB, M = 0..255), discarding any previous model
  (section 6.2).
- **Start the range decoder** at the current position of the member's
  byte stream. This happens at every PPMd block header, whether the model
  is new or kept.
- **Decode the next byte**: yields a value 0..255 and updates the model,
  or fails. It consumes zero or more whole bytes from the member's
  stream, in order. A failure of any kind (the stream runs out, the
  model reports an impossible state) is corrupt input.

The bytes the PPMd decoder reads are bytes of the same stream the bit
reader uses: after the header byte H and its optional bytes M and E the
range decoder reads on from the next byte, and when the block ends the
next block header starts at the first byte the range decoder has not
read (section 3). The range decoder never reads further ahead than
`rar29-ppmd.md` says; an implementation must not let it buffer bytes it
has not been asked to consume. (CONFIRMED (real data): 70 PPMd blocks
are followed by an LZ block in the same member.)

### 9.1 Decoding loop

In a PPMd block, repeat until the member ends (section 12.1): decode the
next byte b.

- If b is **not** the escape value: output b as a literal.
- If b **is** the escape value: decode one more byte, the **code** c,
  and act on it as section 9.2 says.

The escape value is 2 at the start of a group and changes only when a
PPMd block header carries a new one (section 6.2). It is a property of
the group, not of the block or the model: it survives LZ blocks, member
boundaries and the creation of a new model. (CONFIRMED (real data):
headers that set it and headers that leave it, in solid groups;
CONFIRMED (crafted, both tools) that a new group starts with 2 again,
whatever the previous group used.)

### 9.2 Codes after the escape value

Every "byte" below is decoded with the PPMd model like any other byte
(and trains the model in the same way).

| Code c | What follows | Effect |
|--:|---|---|
| 0 | nothing | **New block.** Read a block header from the byte stream (section 6.2) and continue with the block it announces. |
| 1 | nothing | Output one literal byte equal to the escape value. |
| 2 | nothing | **End marker** of the member. The header-pending flag is set. The member's stream ends here (section 12.1). |
| 3 | a filter record | Decode the flags byte F. Let c' = (F and 7) + 1. If c' is 1..6, n = c'. If c' is 7, decode one byte b; n = b + 7. If c' is 8, decode two bytes, the first being the high byte; n is the 16-bit value. Then decode n bytes: the record body. F and the body go to the filter layer exactly as for main symbol 257 (section 11.2). No output. |
| 4 | four bytes | Decode three bytes forming a 24-bit number D, first byte most significant, and one byte L. A **match** with distance D + 2 and length L + 32 (distance 2..16,777,217, length 32..287). |
| 5 | one byte | Decode one byte L. A **match** with distance 1 and length L + 4 (4..259). |
| 6..255 | nothing | Output one literal byte equal to the escape value. |

- The matches of codes 4 and 5 are executed as in section 8.8 and
  obey the checks of sections 10 and 15 (a distance above 4,194,304 or
  above the LZ position is corrupt input). They **do not** change the
  registers or the last distance and length (CONFIRMED (crafted),
  vector V7b).
- Code 1 is how a data byte equal to the escape value is transmitted.
  Codes 6..255 behaving the same way is CONFIRMED (crafted, both tools;
  vector V7a uses codes 6 and 9) and not seen in real data.
- Observed in real data: code 0 70 times, code 1 17,906 times, code 2
  118 times, code 3 70 times, code 4 1,325 times, code 5 58,676 times.

## 10. The window

- The window holds the most recent **4,194,304 bytes** (4 MiB,
  `0x400000`) of the group's **unfiltered** output, for both versions 29
  and 36, whatever dictionary size the header declares. (CONFIRMED (real
  data): distances up to 4,193,665 occur; REFERENCE-ONLY for the fixed
  size.)
- It is used as a ring: the byte with LZ position t lives at ring
  position t modulo 4,194,304. Every byte the group produces goes into
  it: LZ literals and matches, PPMd literals and PPMd matches alike.
  **Filters never write into the window**: a later match that reaches
  back into a filtered stretch copies the bytes as they were before the
  filter ran.
- A match copies **one byte at a time in increasing order**, reading the
  byte d positions back and writing it at the current position, so that
  source and destination may overlap, and so that both may wrap around
  the end of the ring independently.
- The largest distance, 4,194,304, equals the ring size. It is valid
  once at least 4,194,304 bytes have been produced; the byte to copy is
  then the one at the ring position about to be overwritten, so a ring of
  exactly 4 MiB suffices provided each byte is **read before the position
  is written**. (CONFIRMED (crafted, both tools), vector V8.)
- **A distance greater than the LZ position at the start of the match is
  corrupt input** (DECISION; it would refer to data before the start of
  the group. Never seen in real data; both tools read zero bytes
  instead, section 16; vector E5.) The check is made once per match,
  before copying.
- **A distance greater than 4,194,304 is corrupt input** (only the PPMd
  match of code 4 can express one). DECISION.
- The window is not cleared and the LZ position is not reset between the
  members of a solid group; both start afresh with each new group.

## 11. Filters (interface)

A filter is a small program, transmitted in the stream, that transforms
one stretch (a *filter block*) of the decoded output before it is
delivered: for example it undoes a byte-wise delta coding or rewrites the
operands of x86 call instructions. `rar29-filters.md` specifies the
record format, the programs and the machine that runs them. This section
specifies what the LZ and PPMd layers hand over, where a filter block
lies, when the filter runs and when its output replaces window output.

`rar29-filters.md` (its sections 5.3, 7 and 8) states the placement and
queueing rules as well, in its own words; the two statements are meant
to be identical. **Should they ever differ**, `rar29-filters.md` prevails
on everything about a record, a block's start and length, the order
check and the queue; this document prevails on what a member is, where
it starts and ends, and on the order of events in the decoding loop
(section 12.1).

### 11.1 What the filter layer provides

This document treats the filter layer as two operations. (An
implementation may equally let the filter layer own the queue, as
`rar29-filters.md` describes it; the behaviour is the same.)

**Parse a record.** Input: the flags byte F and the n body bytes, as read
in section 8.7 or 9.2. It reads and updates the filter part of the group
state (the stored programs and their per-program memory). It returns
either "corrupt input" or these results:

| Result | Meaning |
|---|---|
| reset | Whether the record ordered all stored programs to be discarded (the record's program-number field held 0). |
| offset | The record's block start field, an unsigned number below 2 to the power 32. |
| block length | 1..245,760 (`0x3c000`). The filter layer rejects a record whose block length is 0 or larger than that as corrupt input. |
| invocation | Whatever the filter layer needs to run this filter later (the program, its start-up register values, its data). Opaque here. |

**Run a filter.** Input: an invocation, the filter block's bytes, and the
block's **file offset** (a 64-bit number, section 11.3). Output: "corrupt
input" or the transformed bytes, exactly as many as it was given.

What the two layers exchange, in the terms of `rar29-filters.md`: the LZ
position p at which a record is met is its "group position G"; the
member start of section 1.3 is its "M"; the LZ position at which a
member ends is its "E".

### 11.2 Placing a filter block

When a filter record has been read completely, at LZ position p (no
output is produced while a record is read, so p is well defined):

1. Apply every filter that is ready (section 11.3), exactly as before any
   other item.
2. Parse the record. If the result says **reset**, discard every entry of
   the filter queue, whether its block has not started yet or is partly
   produced (they will never run; the output they covered is delivered
   unfiltered), and forget the order mark T of step 4. The filter this
   same record announces is then queued by the steps below like any
   other. (In real data a reset occurs only in the first filter record
   of a group, 192 times, when the queue is empty. REFERENCE-ONLY that
   the queue is emptied. CONFIRMED (crafted) with `bsdtar` when the first
   queued block has not started, vector V6d; the reference
   implementation fails on every reset after the first record, and both
   tools fail when a block is partly produced, section 16. DECISION for
   the partly produced block.)
3. The filter block starts at LZ position **S = p + offset**, plus
   **258** more if bit `0x40` of F is set, and has the returned block
   length L. The 258 is added exactly once (`rar29-filters.md` words the
   same sum as s = G + offset + 258; vector V6b fixes the outcome: a
   record read at LZ position 3 with a start field of 4 and the bit set
   gives a block starting at LZ position 265). S can exceed 32 bits; it
   is a 64-bit number, formed without wrap-around.
4. Checks, each failing as corrupt input:
   - **Order.** Let the order mark T be the end (start plus length) of
     the block queued most recently in this group, or 0 if none was
     queued yet or a reset has occurred since. S must be at least T.
     Blocks therefore never overlap, are never nested, arrive in
     ascending order, and **no second filter can be given for a range
     that already has one** (vector E14). T is kept from member to
     member. DECISION; both tools reject every such stream, and in all
     real records S is at least T.
   - The queue already holds 65,536 entries. DECISION. (The longest
     queue in real data is 37.)
5. Append (S, L, invocation) to the queue and set T to S + L.

Because S is at least p, a filter block never begins inside output that
already exists; it may begin exactly at p, and exactly where the
previous block ends. Real encoders announce blocks well in advance: the
record of a block may precede the block's first byte by more than
1.6 MiB, with other records in between. Bit `0x40` is set in 2,400 of
the 3,134 real records.

### 11.3 Running filters

A queue entry is **ready** when the LZ position has reached S + L, that
is, when every byte of its block has been produced. **Before decoding
each item** (in either block type), and when a member ends (section
12.1), the decoder runs ready filters as follows, repeating while the
first entry of the queue is ready:

1. Remove the first entry (S, L, invocation) and take the L bytes of its
   block from the window: the unfiltered bytes at LZ positions S..S + L −
   1, no more and no less, even if the item that completed the block
   produced bytes beyond it. (They are still in the window: at most
   L + 286 bytes have been produced since S, far fewer than the window
   holds.)
2. Run the filter on them with the file offset **S − member start**, the
   position of the block's first byte within the output of the member
   being decoded, not within the group. (CONFIRMED (real data): filters
   whose result depends on this offset occur in later members of solid
   groups. CONFIRMED (crafted): vector V6c.)
3. The run must return exactly L bytes; anything else is corrupt input
   (DECISION; every filter real encoders use keeps the length).
4. The result **replaces the bytes at LZ positions S..S + L − 1 in the
   member's delivered output**. The window is not modified.

Consequences an implementation must respect:

- Output bytes are final, and may be handed on, as soon as their LZ
  position is below the start of the first queued filter block (or, with
  an empty queue, at once). Bytes from the first queued block's start
  onward must be held back until that filter has run. With the limits
  above at most 245,760 + 286 bytes are ever held back.
- Filters run in queue order, and every block is complete before the
  next begins, so the output is produced strictly in order.
- A filter runs as soon as its block is complete, not later: when a
  filter record or an end marker is handled, the queue never begins with
  a complete block. This matters for a record that orders a reset:
  filters whose blocks were already complete have run and stay applied;
  only those not yet ready are discarded (CONFIRMED (crafted) with
  `bsdtar`, vector V6e; the reference implementation fails on a second
  reset, section 16).
- Only one filter ever applies to a byte. (The reference implementation
  contains a provision for running several filters in turn on the same
  block, which a defect keeps from ever taking effect; `bsdtar` rejects
  such streams and no real stream contains one. Sections 16 and 18.)

### 11.4 Filters and member boundaries

- A filter block must lie within one member. If, when a member ends, the
  first queue entry has started but is not ready (S is below the LZ
  position, S + L above it), that is corrupt input (vector E13;
  CONFIRMED (crafted) that both tools fail). A block that ends exactly
  where the member ends has run in the normal way.
- Queue entries whose block has not started when the member ends (S at
  or above the LZ position) **stay queued**. In a following member of
  the same group they run when their positions are reached, with the
  file offset counted from the start of the member in which the block
  lies. When the group ends they are discarded without error.
  (CONFIRMED (crafted): `unar` applies a filter announced in one member
  to its block in the next, vector V6f, and both tools deliver a member
  unharmed whose filter lies beyond the end of the last member, vector
  N5. Never seen in real data, where the records of a member always
  describe blocks of that member.)
- The stored programs and everything else the filter layer keeps
  **survive member boundaries** inside a solid group: a later member can
  use a program an earlier member transmitted, without sending it again.
  (CONFIRMED (real data).) They are discarded when a new group starts.

## 12. Members, solid groups, volumes, end of data

### 12.1 One member

Let the *member count* be the LZ position minus the member start: the
number of bytes this member has produced.

Decoding a member (after the start-up of section 12.2) repeats this step:

1. Run ready filters (section 11.3).
2. Decode one item of the current block type and act on it.

until an **end marker** is decoded: main symbol 256 followed by a `0` bit
(and one more bit) in an LZ block, or the escape value followed by code 2
in a PPMd block. Then run ready filters once more, apply section 11.4,
and the member is complete.

The end marker and U must agree:

- **Every real member ends with an end marker, decoded immediately after
  the item that completes its U bytes** (1,419 members, section 19). U is
  therefore redundant in a well-formed stream, but it is what the
  container promised and it is what the CRC-32 covers.
- An end marker while the member count is **below U** is corrupt input
  (vector E8).
- A match (of any kind, in either block type) whose length exceeds U
  minus the member count, while the member count is below U, is corrupt
  input (DECISION; vector E7. Never seen in real data. Both tools
  deliver the first part of the match.)
- When the member count **equals U**, the decoder goes on decoding items,
  because it still needs the end marker: it carries the header-pending
  flag for the next member. Items that produce no output (a block switch
  with its block header, a filter record) are processed normally.
  (Neither occurs at this point in real data.) Then:
  - the end marker arrives: the normal case;
  - or an item that would produce output arrives (a literal, a match
    other than a symbol 258 that does nothing), or decoding fails in any
    way, including running out of packed bytes: the member has **no end
    marker**. DECISION: its U bytes are still delivered (subject to the
    CRC-32), but the group's state is marked unusable (section 12.4).
    Nothing is output for the offending item. (Vectors N1, N2. `unar`
    fails such a member; `bsdtar` delivers N2.)
- U = 0 does not change any of this: a member with U = 0 and packed
  bytes consists of an end marker, possibly preceded by a block header
  (section 12.2).

### 12.2 Solid groups

The file header flag bit `0x0010` ("solid") on a member of unpack version
29 or 36 means: **this member continues the decoder state left by the
preceding member**. A member without the bit starts a new group with a
fresh state (section 5). The archive-level solid flag (main header bit
`0x0008`) is not consulted; the per-file bit decides. (CONFIRMED
(crafted): `unar` honours the per-file bit whatever the main header
says, and ignores the main header bit when the per-file bit is clear. In
real solid archives the main header bit is set, the first file of each
group lacks the per-file bit and the other files carry it; an archive
can hold several groups.)

Processing one group, member by member, in archive order:

1. **First member** (no solid bit). Create a fresh state. If P is 0: the
   member is empty; U must be 0 (otherwise corrupt input); the state
   stays fresh, with the header-pending flag set. Otherwise read a block
   header at bit 0 of its stream and decode as in section 12.1.
2. **Continuation member with P = 0 and U = 0.** Nothing is read and the
   state is unchanged, including the header-pending flag. (DECISION;
   CONFIRMED (crafted) that `unar` behaves so. Real RAR does not write
   such members into a solid group: it writes the form of the next
   paragraph.) A continuation member with P = 0 and U greater than 0 is
   corrupt input.
3. **Any other continuation member** (P greater than 0, whatever U is).
   Start a new stream at bit 0 of this member's packed bytes. If the
   header-pending flag is set, read a block header there. If it is
   clear, continue directly with LZ items, using the codes, the
   low-distance value and the repeat counter the previous member left.
   In both cases the window, LZ position, registers, last match, length
   table, PPMd model, escape value, filter programs and filter queue are
   those the previous member left. Then decode as in section 12.1. Unused bits and
   bytes at the end of the previous member's stream are discarded.

The header-pending flag is set at the start of a group; set or cleared
by the second bit of an LZ end marker (section 8.6); set by a PPMd end
marker (section 9.2). In real data 1,248 LZ end markers set it and 53
clear it; of the 225 real continuation members with packed bytes, 172
start with a block header and 53 start directly with items.

**Empty files inside a solid group are real and need care.** RAR stores
a zero-length file of a solid archive as a compressed member with U = 0
and a few packed bytes that hold only an end marker (1 or 2 bytes when
the flag was clear: the marker is coded with the previous member's main
code; 8 bytes when it was set: a block header, a table transmission and
the marker). That marker's second bit decides how the *following* member
starts. 55 such members occur in the archives made for this document,
two of them as the first member of their group, and every member after
them decodes correctly under the rules above (CONFIRMED (real data)),
**although `unar` fails on the members that follow them** (section 16).

Members that are not handled by this decoder but appear between members
of a group:

- **Directory entries** (dictionary bits = 7) have no data and no effect
  on the state.
- **Stored members** (method `0x30`) are **transparent**: a stored
  member is delivered as stored; its bytes do not enter the window, the
  LZ position does not advance and nothing in the state changes,
  including the header-pending flag; a later member with the solid bit
  continues from the last *compressed* member. RAR writes such members
  when told to store certain files of a solid archive: their headers
  carry method `0x30`, unpack version 20 and the solid bit, and P = U
  (also P = U = 0 for an empty file stored this way). (CONFIRMED (real
  data): a solid archive written by RAR 6.24 with two stored members,
  one of 5,000 bytes and one empty, between compressed members; all four
  compressed members decode with matching CRC-32 under this rule, and
  the 5,000 bytes, packed again as the next member, take 5,131 packed
  bytes, so they were not in the window. In that archive both members
  that follow a stored one begin with a block header, as the end marker
  before the stored member announced; that the flag is carried across a
  stored member when it is clear is INFERRED.
  `rar4-container.md` reports the same observation. `unar` fails on the
  members that follow a stored one, section 16.)
- DECISION: a member with the solid bit whose unpack version is 29 or
  36, when the state it would continue belongs to another method (the
  preceding compressed member had version 15, 20 or 26) or does not
  exist (it is the first compressed member seen, as happens when a
  later volume of a set is opened alone), is reported as an error for
  that member ("solid predecessor unavailable") without decoding.

### 12.3 Summary of member start and end

| Situation | Result |
|---|---|
| First member, P = 0, U = 0 | Nothing decoded. State fresh. |
| Continuation member, P = 0, U = 0 | Nothing decoded. State unchanged. |
| Any member, P = 0, U greater than 0 | Corrupt input. |
| Member with packed bytes, header-pending flag set (always so for a first member) | Block header at bit 0, then items. |
| Continuation member with packed bytes, flag clear | LZ items from bit 0 with the previous codes. (The flag can only have been cleared by an LZ end marker, so codes exist; an implementation must nevertheless fail cleanly, as corrupt input, if it ever finds itself decoding an LZ item without codes.) |
| End marker with member count = U | Member complete. Rest of the packed bytes ignored. |
| End marker with member count below U | Corrupt input. |
| Match longer than the bytes still missing | Corrupt input (DECISION). |
| Member count = U and no end marker follows | U bytes delivered; group state unusable (DECISION). |
| A bit or byte is needed beyond the packed bytes while the member count is below U | Corrupt input. |
| A filter block is incomplete when the member ends | Corrupt input. |

### 12.4 Failures inside a group

DECISION: when a member of a group ends in corrupt input (or is
encrypted and cannot be deciphered, or cannot be read at all), or ends
without an end marker (section 12.1), the state is no longer
trustworthy. Every later member that continues the group is reported as
failed without being decoded ("depends on a damaged member"), up to the
next member that starts a new group. A CRC-32 mismatch alone, with the
stream otherwise decoded to an end marker at exactly U bytes, does not
have this effect.

To deliver any member of a group, all members before it in the group
must be decoded, in order, even if the caller does not want them.

### 12.5 Files split across volumes

- A member may be split over several volumes: file header flag bit
  `0x0002` says "continues in the next volume", bit `0x0001` "continued
  from the previous volume". Each piece has its own file header
  (`rar4-container.md`).
- The packed bytes of the member are the pieces' data areas
  **concatenated in volume order**. The pieces are cut at arbitrary byte
  positions of one continuous stream: nothing is aligned, repeated or
  reset at a volume boundary, in LZ blocks, PPMd blocks or filter
  records alike, and the decoder cannot tell where the boundaries were.
  (CONFIRMED (real data): three volume sets of 6, 9 and 10 volumes with
  13 members split into up to 5 pieces, with and without solid mode and
  PPMd.)
- A solid group can extend across volumes in the same way; the solid
  bit of the first file header of a later volume refers to the last
  member of the previous volume.
- If a piece is missing the member cannot be decoded: corrupt input (or
  a container-level error), and section 12.4 applies.

### 12.6 Differences between unpack versions 29 and 36

None in this decoder (section 2). Whatever distinguishes the two
versions lies outside the compressed stream; `rar4-container.md` covers
what the container does with the value.

## 13. Worked examples

### 13.1 LZ block: `abcabc` (vector V1)

Packed bytes (14 bytes, 111 bits used), U = 6:

`00 c8 ff 45 5a bb f8 3f 03 84 b6 5a 1d 3a`

In binary, grouped by byte:

`00000000 11001000 11111111 01000101 01011010 10111011 11111000 00111111 00000011 10000100 10110110 01011010 00011101 00111010`

| Bits | Value | Meaning |
|--:|---|---|
| 0 | `0` | block type 0: LZ block (no alignment needed at bit 0) |
| 1 | `0` | keep flag 0: length table cleared |
| 2..5 | `0000` | pre-code symbol 0: length 0 |
| 6..9 | `0011` | pre-code symbol 1: length 3 |
| 10..13 | `0010` | pre-code symbol 2: length 2 |
| 14..17 | `0011` | pre-code symbol 3: length 3 |
| 18..21 | `1111` | 15: look at the next field |
| 22..25 | `1101` | 13: 13 + 2 = 15 zero lengths, pre-code symbols 4..18 |
| 26..29 | `0001` | pre-code symbol 19: length 1 |

Pre-code (section 4): 19 = `0`, 2 = `10`, 1 = `110`, 3 = `111`.

| Bits | Value | Meaning |
|--:|---|---|
| 30 | `0` | pre-code symbol 19 |
| 31..37 | `1010110` = 86 | 86 + 11 = 97 zeros: positions 0..96 |
| 38..39 | `10` | symbol 2: position 97 = 0 + 2 = 2 |
| 40..41 | `10` | symbol 2: position 98 = 2 |
| 42..44 | `111` | symbol 3: position 99 = 3 |
| 45 | `0` | symbol 19 |
| 46..52 | `1111111` = 127 | 138 zeros: positions 100..237 |
| 53 | `0` | symbol 19 |
| 54..60 | `0000111` = 7 | 18 zeros: positions 238..255 |
| 61..63 | `111` | symbol 3: position 256 = 3 |
| 64 | `0` | symbol 19 |
| 65..71 | `0000011` = 3 | 14 zeros: positions 257..270 |
| 72..73 | `10` | symbol 2: position 271 = 2 |
| 74 | `0` | symbol 19 |
| 75..81 | `0010010` = 18 | 29 zeros: positions 272..300 |
| 82..84 | `110` | symbol 1: position 301 = 1 |
| 85..87 | `110` | symbol 1: position 302 = 1 |
| 88 | `0` | symbol 19 |
| 89..95 | `1011010` = 90 | 101 zeros: positions 303..403. Position 404 reached. |

Codes: main code from positions 0..298: symbols 97, 98 and 271 with
length 2, symbols 99 and 256 with length 3, so 97 = `00`, 98 = `01`,
271 = `10`, 99 = `110`, 256 = `111`. Distance code from positions
299..358: positions 301 and 302 are distance symbols 2 and 3, length 1
each: 2 = `0`, 3 = `1`. Low-distance code and length code: empty.

| Bits | Value | Meaning |
|--:|---|---|
| 96..97 | `00` | main 97: literal `a` |
| 98..99 | `01` | main 98: literal `b` |
| 100..102 | `110` | main 99: literal `c` |
| 103..104 | `10` | main 271: length slot 0, no extra bits, length 0 + 0 + 3 = 3 |
| 105 | `0` | distance symbol 2: slot 2, no extra bits, no low part: distance 2 + 0 + 1 = 3. No adjustment (3 is below 8192). |
| 106..108 | `111` | main 256 |
| 109 | `0` | end marker |
| 110 | `1` | header-pending flag set |

The match copies 3 bytes from 3 back: `abc`. Output `61 62 63 61 62 63`
(`abcabc`), CRC-32 `0x726E994C`. The member count is 6 = U when the end
marker arrives. Bit 111 is unused. After the member, register 0 holds 3
and the last match is (3, 3).

### 13.2 Two blocks: pre-code details, repeat symbols, keep flag, alignment (vector V4)

Packed bytes (38 bytes, 297 bits used), U = 7:

`18 5e 0a 6a f3 7b c3 c3 c5 41 0f 36 bc 3c 77 fc 89 bf f6 89 ab 7f 48 be c8 11 0f 36 aa 00 3b fe 42 ef ff b1 78 80`

**Block 1.**

| Bits | Value | Meaning |
|--:|---|---|
| 0 | `0` | LZ block |
| 1 | `0` | keep flag 0 |
| 2..5 | `0110` | pre-code symbol 0: length 6 |
| 6..9 | `0001` | symbol 1: length 1 |
| 10..13 | `0111` | symbol 2: length 7 |
| 14..17 | `1000` | symbol 3: length 8 |
| 18..21 | `0010` | symbol 4: length 2 |
| 22..45 | `1001 1010 1011 1100 1101 1110` | symbols 5..10: lengths 9, 10, 11, 12, 13, 14 |
| 46..53 | `1111 0000` | 15 then 0: symbol 11 has length 15 |
| 54..61 | `1111 0000` | 15 then 0: symbol 12 has length 15 |
| 62..69 | `1111 0001` | 15 then 1: 1 + 2 = 3 zero lengths, symbols 13..15 |
| 70..73 | `0101` | symbol 16: length 5 |
| 74..77 | `0000` | symbol 17: length 0 |
| 78..81 | `0100` | symbol 18: length 4 |
| 82..85 | `0011` | symbol 19: length 3 |

Pre-code: 1 = `0`, 4 = `10`, 19 = `110`, 18 = `1110`, 16 = `11110`,
0 = `111110`, 2 = `1111110`, 3 = `11111110`, and so on down to
11 = `111111111111110` and 12 = `111111111111111` (a complete code with
lengths 1 to 15).

| Bits | Value | Meaning |
|--:|---|---|
| 86..88, 89..95 | `110`, `0110110` = 54 | symbol 19: 65 zeros, positions 0..64 |
| 96..97 | `10` | symbol 4: position 65 = 4 |
| 98..102, 103..105 | `11110`, `000` | symbol 16, r = 0: 3 copies of the previous length (4): positions 66..68 |
| 106..110, 111..113 | `11110`, `001` | symbol 16, r = 1: 4 copies: positions 69..72 |
| 114..116, 117..123 | `110`, `1111111` | symbol 19: 138 zeros, positions 73..210 |
| 124..126, 127..133 | `110`, `0100010` = 34 | symbol 19: 45 zeros, positions 211..255 |
| 134 | `0` | symbol 1: position 256 = 1 |
| 135..137, 138..144 | `110`, `1111111` | symbol 19: 138 zeros, positions 257..394 |
| 145..148, 149..151 | `1110`, `110` = 6 | symbol 18: 9 zeros, positions 395..403 |

Main code: 256 = `0`; 65..72 (`A`..`H`) = `1000`, `1001`, `1010`, `1011`,
`1100`, `1101`, `1110`, `1111`. The other three codes are empty.

| Bits | Value | Meaning |
|--:|---|---|
| 152..167 | `1000 1001 1010 1011` | literals `A` `B` `C` `D` |
| 168 | `0` | main 256 |
| 169 | `1` | a new block follows |
| 170..175 | `111111` | 170 is not a multiple of 8: six bits are discarded to reach bit 176. Their value is not checked. |

**Block 2.**

| Bits | Value | Meaning |
|--:|---|---|
| 176 | `0` | LZ block |
| 177 | `1` | keep flag 1: the length table keeps the values of block 1 |
| 178..181 | `0010` | pre-code symbol 0: length 2 |
| 182..185 | `0010` | symbol 1: length 2 |
| 186..193 | `1111 1011` | 15 then 11: 13 zero lengths, symbols 2..14 |
| 194..197 | `0010` | symbol 15: length 2 |
| 198..201 | `0000` | symbol 16: length 0 |
| 202..205 | `0100` | symbol 17: length 4 |
| 206..209 | `0100` | symbol 18: length 4 |
| 210..213 | `0011` | symbol 19: length 3 |

Pre-code: 0 = `00`, 1 = `01`, 15 = `10`, 19 = `110`, 17 = `1110`,
18 = `1111`.

| Bits | Value | Meaning |
|--:|---|---|
| 214..216, 217..223 | `110`, `0110110` | symbol 19: 65 zeros, positions 0..64 |
| 224..231 | `10 10 10 10` | symbol 15 four times: positions 65..68 = (4 + 15) modulo 16 = 3 |
| 232..239 | `00 00 00 00` | symbol 0 four times: positions 69..72 stay 4 |
| 240..241 | `00` | symbol 0: position 73 stays 0 |
| 242..245, 246..252 | `1110`, `1111111` | symbol 17, r = 127: 138 copies of the previous length (0): positions 74..211 |
| 253..255, 256..262 | `110`, `0100001` = 33 | symbol 19: 44 zeros, positions 212..255 |
| 263..264 | `01` | symbol 1: position 256 = (1 + 1) = 2 |
| 265..267, 268..274 | `110`, `1111111` | symbol 19: 138 zeros, positions 257..394 |
| 275..278, 279..281 | `1111`, `110` | symbol 18: 9 zeros, positions 395..403 |

New main code: 256 = `00`; 65..68 = `010`, `011`, `100`, `101`; 69..72 =
`1100`, `1101`, `1110`, `1111`.

| Bits | Value | Meaning |
|--:|---|---|
| 282..285 | `1100` | literal `E` |
| 286..288 | `010` | literal `A` |
| 289..292 | `1111` | literal `H` |
| 293..294 | `00` | main 256 |
| 295 | `0` | end marker |
| 296 | `1` | header-pending flag set |

Output: `ABCDEAH`, CRC-32 `0xD1911FEA`.

### 13.3 Every kind of match (vector V2)

Packed bytes (116 bytes, 923 bits used), U = 148: see V2 in section 14.

The table transmission occupies bits 0..356 and gives these codes:

- main code: 32 = `00`; 101 = `0100`, 111 = `0101`; 104 = `01100`,
  105 = `01101`, 114 = `01110`, 117 = `01111`; with six bits, in this
  order from `100000`: 46, 82, 97, 98, 99, 100, 102, 103, 106, 107, 108,
  109, 110, 113, 115, 116, 118, 119, 120, 121, 122, 272, 273, 274, 275
  (so 82 = `100001`, 272 = `110101`, 273 = `110110`, 274 = `110111`,
  275 = `111000`); with seven bits, from `1110010`: 33, 65, 80, 84, 112,
  256, 258, 259, 260, 261, 262, 263, 267, 271 (so 65 = `1110011`,
  256 = `1110111`, 258 = `1111000`, 259 = `1111001`, 260 = `1111010`,
  261 = `1111011`, 262 = `1111100`, 263 = `1111101`, 267 = `1111110`,
  271 = `1111111`);
- distance code: 11 = `00`, 12 = `01`, 0 = `100`, 1 = `101`, 6 = `110`,
  10 = `111`;
- low-distance code: 0 = `00`, 1 = `01`, 12 = `10`, 16 = `11`;
- length code: 0 = `00`, 1 = `01`, 2 = `10`, 8 = `11`.

Bits 357..769 are 86 literals: the text `The quick brown fox jumps over
the lazy dog. Pack my box with five dozen liquor jugs. ` (ending with a
space). Then:

| Bits | Values | Meaning | Registers 0..3 afterwards | Member count |
|--:|---|---|---|--:|
| 770..778 | `1111101` `11` | main 263: two-byte match, base 0, 2 extra bits = 3: distance 4 | 4, –, –, – | 88 |
| 779..790 | `1111110` `01000` | main 267: two-byte match, base 32, 5 extra bits = 8: distance 41 | 41, 4, –, – | 90 |
| 791..801 | `110110` `110` `00` | main 273: length slot 2, length 2 + 3 = 5. Distance symbol 6 (base 8, 2 plain extra bits = 0): distance 9 | 9, 41, 4, – | 95 |
| 802..812 | `111000` `111` `10` | main 275: length slot 4, length 7. Distance symbol 10 (base 32, no direct bits); low-distance symbol 12: distance 32 + 12 + 1 = 45. Low-distance value is now 12 | 45, 9, 41, 4 | 102 |
| 813..831 | `100001` `1110011` `100001` | literals `R` `A` `R` | | 105 |
| 832..842 | `110101` `01` `0` `10` | main 272: length slot 1, length 4. Distance symbol 12 (base 64, 1 direct bit = 0); low-distance symbol 12: distance 64 + 0 × 16 + 12 + 1 = 77 | 77, 45, 9, 41 | 109 |
| 843..853 | `1111111` `00` `11` | main 271: length 3. Distance symbol 11 (base 48, no direct bits); low-distance symbol **16**: low part = low-distance value = 12, repeat counter = 15: distance 48 + 12 + 1 = 61 | 61, 77, 45, 9 | 112 |
| 854..862 | `110111` `01` `1` | main 274: length slot 3, length 6. Distance symbol 12 (base 64, 1 direct bit = 1); the repeat counter is 15, so **no low-distance symbol is read**: low part 12, counter 14: distance 64 + 16 + 12 + 1 = 93 | 93, 61, 77, 45 | 118 |
| 863..871 | `1111100` `00` | main 262: register 3 = 45. Length symbol 0: length 0 + 2 = 2. Register 3 moves to the front | 45, 93, 61, 77 | 120 |
| 872..881 | `1111010` `11` `0` | main 260: register 1 = 93. Length symbol 8 (base 8, 1 extra bit = 0): length 8 + 0 + 2 = 10 | 93, 45, 61, 77 | 130 |
| 882..888 | `1111000` | main 258: repeat distance 93, length 10 | unchanged | 140 |
| 889..897 | `1111001` `01` | main 259: register 0 = 93. Length symbol 1: length 3 | 93, 45, 61, 77 | 143 |
| 898..906 | `1111011` `10` | main 261: register 2 = 61. Length symbol 2: length 4 | 61, 93, 45, 77 | 147 |
| 907..913 | `1110010` | literal `!` | | 148 |
| 914..922 | `1110111` `0` `1` | main 256, end marker, header-pending flag set | | 148 |

Output (148 bytes): `The quick brown fox jumps over the lazy dog. Pack
my box with five dozen liquor jugs. gsckugs. my box RARer tk m
jumpsliver the lazy dog. Pack gs. !` (one space between words exactly as
written; the text ends `gs. !`). CRC-32 `0x75240148`.

### 13.4 Solid group of four members (vector V5)

**Member 1** (no solid bit), U = 73, P = 75, 593 bits used. Its table
transmission gives, among others, these codes:

- main code: 46 (`.`) = `101100`, 256 = `110101`, 258 = `110110`,
  259 = `110111`, 260 = `111000`, 261 = `111001`, 262 = `111010`,
  273 = `111110`;
- distance code: 11 = `00`, 12 = `01`, 0 = `100`, 1 = `101`, 8 = `110`,
  10 = `111`;
- low-distance code: 0 = `00`, 1 = `01`, 6 = `10`, 16 = `11`;
- length code: 0 = `00`, 1 = `01`, 2 = `10`, 3 = `11`.

Its items are 60 literals (`solid groups share one decoder state between
their members; `), then explicit matches (distance 19, length 6),
(distance 39, length 4: slot 10, low-distance symbol 6) and (distance 55,
length 3: slot 11, low-distance symbol 16), then main 256 with the bits
`0` `0`: end marker, **header-pending flag clear**. Output: the 60
literals followed by `een thder are`.

State handed on: registers (55, 39, 19, unset); last match (55, 3);
low-distance value 6, repeat counter 15; LZ position 73.

**Member 2** (solid bit), U = 18, packed bytes `db 9f e5 67 56 a0` (45
bits used). No block header is read; the stream starts with an item:

| Bits | Values | Meaning | Output |
|--:|---|---|---|
| 0..5 | `110110` | main 258: repeat distance 55, length 3, copying from member 1's output | ` on` |
| 6..13 | `111001` `11` | main 261: register 2 = 19; length symbol 3: length 5. Registers become (19, 55, 39, unset) | `s; ee` |
| 14..22 | `111110` `01` `0` | main 273: length 5; distance symbol 12, direct bit 0; repeat counter is 15: no low-distance symbol, low part 6, counter 14: distance 64 + 6 + 1 = 71. Registers (71, 19, 55, 39) | `ps sh` |
| 23..28 | `101100` | literal `.` | `.` |
| 29..36 | `111010` `10` | main 262: register 3 = 39; length symbol 2: length 4. Registers (39, 71, 19, 55) | `ir m` |
| 37..44 | `110101` `0` `0` | end marker, header-pending flag clear | |

Output of member 2: ` ons; eeps sh.ir m` (18 bytes, beginning with a
space).

**Member 3** (solid bit), U = 0, packed byte `d5` = `11010101`: main 256
(`110101`), `0`, `1`: an end marker and nothing else, **setting** the
header-pending flag. Output: nothing. The registers and the last match
(39, 4) are unchanged.

**Member 4** (solid bit), U = 14, P = 24, 191 bits used. Because the
flag is set, its stream starts with a block header: LZ block, **keep
flag 1** (the lengths are differences from member 1's table). New codes:
main 258 = `00`, 260 = `01`, 274 = `10`, 47 (`/`) = `110`, 256 = `111`;
distance 12 = `0`, 0 = `10`, 1 = `11`; low-distance 16 = `0`, 0 = `10`,
1 = `11`; length 0 = `0`, 1 = `1`. Items:

- main 258: repeat (39, 4): output `embe`;
- main 260: register 1 = 71, length symbol 1: length 3: output `eco`;
  registers (71, 39, 19, 55);
- main 274: length 6; distance symbol 12, direct bit 1, low-distance
  symbol **16**. The table transmission has reset the low-distance value
  to 0, so the low part is 0: distance 64 + 16 + 0 + 1 = 81: output
  `e one `;
- literal `/`;
- end marker, flag set.

Output of member 4: `embeecoe one /`.

### 13.5 Runs that pass the end of a table (vector V9)

Packed bytes (33 bytes, 259 bits used), U = 237:

`09 0f ed 45 7f e5 5d b4 fe 0d a7 ff f1 b8 49 0f ed 45 7f e5 5b ba 7f 06 de 1f 1a 7c de ed f9 66 20`

Both blocks send the same pre-code lengths. Their last two fields
announce a zero run of 17 symbols starting at symbol 19, of which only
symbol 19 exists. Pre-code: 17 = `0`, 0 = `10`, 2 = `110`, 1 = `1110`,
16 = `11110`, 18 = `11111`.

**Block 1** ends its table with a zero run (symbol 18) that passes
position 403.

| Bits | Value | Meaning |
|--:|---|---|
| 0 | `0` | LZ block |
| 1 | `0` | keep flag 0 |
| 2..5 | `0010` | pre-code symbol 0: length 2 |
| 6..9 | `0100` | symbol 1: length 4 |
| 10..13 | `0011` | symbol 2: length 3 |
| 14..21 | `1111` `1011` | 15, then 11 + 2 = 13 zero lengths, symbols 3..15 |
| 22..25 | `0101` | symbol 16: length 5 |
| 26..29 | `0001` | symbol 17: length 1 |
| 30..33 | `0101` | symbol 18: length 5 |
| 34..41 | `1111` `1111` | 15, then 15 + 2 = 17 zero lengths starting at symbol 19: only symbol 19 exists, the other 16 are ignored |
| 42..43 | `10` | symbol 0: position 0 = 0 + 0 = 0 |
| 44, 45..51 | `0`, `1010101` | symbol 17, r = 85: 96 copies of the previous length (0), positions 1..96 |
| 52..54 | `110` | symbol 2: position 97 = 0 + 2 = 2 |
| 55..57 | `110` | symbol 2: position 98 = 0 + 2 = 2 |
| 58..60 | `110` | symbol 2: position 99 = 0 + 2 = 2 |
| 61..62 | `10` | symbol 0: position 100 = 0 + 0 = 0 |
| 63, 64..70 | `0`, `1111111` | symbol 17, r = 127: 138 copies of the previous length (0), positions 101..238 |
| 71, 72..78 | `0`, `0000110` | symbol 17, r = 6: 17 copies of the previous length (0), positions 239..255 |
| 79..81 | `110` | symbol 2: position 256 = 0 + 2 = 2 |
| 82..83 | `10` | symbol 0: position 257 = 0 + 0 = 0 |
| 84, 85..91 | `0`, `1111111` | symbol 17, r = 127: 138 copies of the previous length (0), positions 258..395 |
| 92..96, 97..99 | `11111`, `111` | symbol 18, r = 7: 10 zeros, positions 396..403; 8 fit, the other 2 are ignored |
| 100..101 | `00` | literal `a` |
| 102..103 | `01` | literal `b` |
| 104..105 | `10` | literal `c` |
| 106..107 | `11` | main 256 |
| 108 | `1` | a new block follows |
| 109..111 | `000` | padding |

Main code of block 1: 97 = `00`, 98 = `01`, 99 = `10`, 256 = `11`.

**Block 2** (keep flag 1) ends its table with a copy run (symbol 16)
that passes position 403; the three copies that fit give the length-code
symbols 25, 26 and 27 their lengths.

| Bits | Value | Meaning |
|--:|---|---|
| 112 | `0` | LZ block |
| 113 | `1` | keep flag 1 |
| 114..117 | `0010` | pre-code symbol 0: length 2 |
| 118..121 | `0100` | symbol 1: length 4 |
| 122..125 | `0011` | symbol 2: length 3 |
| 126..133 | `1111` `1011` | 15, then 11 + 2 = 13 zero lengths, symbols 3..15 |
| 134..137 | `0101` | symbol 16: length 5 |
| 138..141 | `0001` | symbol 17: length 1 |
| 142..145 | `0101` | symbol 18: length 5 |
| 146..153 | `1111` `1111` | 15, then 15 + 2 = 17 zero lengths starting at symbol 19: only symbol 19 exists, the other 16 are ignored |
| 154..155 | `10` | symbol 0: position 0 = 0 + 0 = 0 |
| 156, 157..163 | `0`, `1010101` | symbol 17, r = 85: 96 copies of the previous length (0), positions 1..96 |
| 164..165 | `10` | symbol 0: position 97 = 2 + 0 = 2 |
| 166..169 | `1110` | symbol 1: position 98 = 2 + 1 = 3 |
| 170..173 | `1110` | symbol 1: position 99 = 2 + 1 = 3 |
| 174..175 | `10` | symbol 0: position 100 = 0 + 0 = 0 |
| 176, 177..183 | `0`, `1111111` | symbol 17, r = 127: 138 copies of the previous length (0), positions 101..238 |
| 184, 185..191 | `0`, `0000110` | symbol 17, r = 6: 17 copies of the previous length (0), positions 239..255 |
| 192..194 | `110` | symbol 2: position 256 = 2 + 2 = 4 |
| 195..199, 200..202 | `11110`, `000` | symbol 16, r = 0: 3 copies of the previous length (4), positions 257..259 |
| 203..207, 208..210 | `11111`, `000` | symbol 18, r = 0: 3 zeros, positions 260..262 |
| 211..213 | `110` | symbol 2: position 263 = 0 + 2 = 2 |
| 214..215 | `10` | symbol 0: position 264 = 0 + 0 = 0 |
| 216, 217..223 | `0`, `1111100` | symbol 17, r = 124: 135 copies of the previous length (0), positions 265..399 |
| 224..226 | `110` | symbol 2: position 400 = 0 + 2 = 2 |
| 227..231, 232..234 | `11110`, `111` | symbol 16, r = 7: 10 copies of the previous length (2), positions 401..403; 3 fit, the other 7 are ignored |
| 235..236 | `01` | main 263: two-byte match |
| 237..238 | `10` | extra 2: distance 3 |
| 239..242 | `1111` | main 259: register 0 = 3 |
| 243..244 | `11` | length symbol 27 |
| 245..249 | `00101` | extra 5: length 224 + 5 + 2 = 231 |
| 250..252 | `100` | literal `b` |
| 253..256 | `1100` | main 256 |
| 257 | `0` | end marker |
| 258 | `1` | header-pending flag set |

Main code of block 2: 97 = `00`, 263 = `01`, 98 = `100`, 99 = `101`,
256 = `1100`, 257 = `1101`, 258 = `1110`, 259 = `1111`. Length code:
24 = `00`, 25 = `01`, 26 = `10`, 27 = `11`. Distance and low-distance
codes: empty.

Output: `abc`, then `ab` (two-byte match, distance 3), then 231 bytes
continuing the same pattern (register match, distance 3), then `b`:
altogether `abc` repeated 78 times, then `abb`. 237 bytes, CRC-32 `0x8E71368A`. A decoder
that rejects either run, or that does not stop the copy run at position
403, fails this vector.

## 14. Test vectors

Each vector was built by hand-written tooling from this document's rules,
decoded by a throw-away decoder written from this document, and given to
`unar` 1.10.7 and `bsdtar` 3.5.3 inside a minimal RAR4 archive (unpack
version 29, method `0x33`). "Both tools" in the last column means both
produced the expected output and reported success; other entries say
what happened. "Bits used" counts the bits read up to and including the
end marker.

### 14.1 Valid streams, LZ only

| # | Packed bytes | U | Expected output | CRC-32 of output | Tools |
|--:|---|--:|---|---|---|
| V1 | `00 c8 ff 45 5a bb f8 3f 03 84 b6 5a 1d 3a` (111 bits) | 6 | `abcabc` | `0x726E994C` | both |
| V2 | below (923 bits) | 148 | section 13.3 | `0x75240148` | both |
| V3 | below (1750 bits) | 267,090 | below | `0xA123495B` | both |
| V4 | the bytes of section 13.2 (297 bits) | 7 | `ABCDEAH` | `0xD1911FEA` | both |
| V8 | below (16,642 bits) | 4,194,861 | below | `0xE0345843` | both |
| V9 | the bytes of section 13.5 (259 bits) | 237 | `abc` 78 times, then `abb` | `0x8E71368A` | both |
| V10 | below (20,580 bits) | 4,380,438 | below | `0xD90EF7FB` | both |

**V2** packed bytes (116):

`14 0d 15 08 be 10 e9 58 d0 25 0e d0 6f 8f 34 08 07 c3 74 00 fa 68 68 01 7d 3e 55 5e 0f 04 02 8c cc e3 9c 32 4a 92 4e f3 84 92 72 94 47 56 21 6b db 25 24 6e 5c 6c 26 5c 8a 1f 5f 6b 85 c1 1c 5e c4 2a 8b 4c c9 56 78 0e 91 49 49 5e 64 6b 91 8b 6f 61 33 70 42 55 d1 2c 2a 6d af 57 14 3e 7b a0 3e ff 91 b6 38 f4 3c e1 d5 5f cf 77 f0 f5 bc 79 7d dc bb a0`

Output in hexadecimal:

`54 68 65 20 71 75 69 63 6b 20 62 72 6f 77 6e 20 66 6f 78 20 6a 75 6d 70 73 20 6f 76 65 72 20 74 68 65 20 6c 61 7a 79 20 64 6f 67 2e 20 50 61 63 6b 20 6d 79 20 62 6f 78 20 77 69 74 68 20 66 69 76 65 20 64 6f 7a 65 6e 20 6c 69 71 75 6f 72 20 6a 75 67 73 2e 20 67 73 63 6b 75 67 73 2e 20 6d 79 20 62 6f 78 20 52 41 52 65 72 20 74 6b 20 6d 20 6a 75 6d 70 73 6c 69 76 65 72 20 74 68 65 20 6c 61 7a 79 20 64 6f 67 2e 20 50 61 63 6b 20 67 73 2e 20 21`

**V3** (length adjustments, longest match, repeated use of symbol 258):
219 packed bytes, namely the 53 bytes

`11 4c d5 0c be 10 ea 2f ff 00 00 02 9c 09 24 bf e8 49 9e 74 06 ca 3b 5b c7 c1 d0 cd f5 6e 8c 4a 81 4b 12 a3 bd d3 af 6e fe 3c fa f7 f3 ef ef fa 6b b6 f9 f3 c0`

then 126 bytes `00`, then the 40 bytes

`01 89 fe a9 47 d7 89 ff a9 47 d7 9f d0 02 a5 1f 5f 3b ff ea 94 7d 78 df ff d4 a3 eb cf f8 00 15 28 fa f3 f5 52 8f af 84`

The table transmission ends at bit 294. Codes: main 258 = `0`,
259 = `100`, 45 (`-`) = `1010`, 46 (`.`) = `1011`, 271 = `11000`,
298 = `11001`, then with six bits 99, 100, 101, 102, 256, 272 from
`110100` (256 = `111000`, 272 = `111001`), then with seven bits 48..57,
97, 98 from `1110100`; distance 0 = `000`, 1 = `001`, 3 = `010`,
7 = `011`, 25 = `100`, 26 = `101`, 35 = `110`, 36 = `111`; low-distance
0 = `00`, 1 = `01`, 14 = `10`, 15 = `11`; length 27 = `0`, 0 = `10`,
1 = `11`.

Items: the 16 literals `0123456789abcdef`; an explicit match of length
257 (slot 27, extra 30) and distance 16; symbol 258 a total of 1,021
times (each repeating distance 16, length 257: this is the long run of
zero bits); then seven groups, each consisting of one explicit match,
the literal `-`, symbol 259 with length 2, symbol 259 with length 257,
symbol 258, and the literal `.`. The explicit matches of the seven
groups are, as (distance, slot length before adjustment, final length):

| Group | Distance | How the distance is coded | Length before adjustment | Final length |
|--:|--:|---|--:|--:|
| 1 | 8,191 | slot 25, direct 127, low symbol 14 | 3 | 3 |
| 2 | 8,192 | slot 25, direct 127, low symbol 15 | 3 | 4 |
| 3 | 8,193 | slot 26, direct 0, low symbol 0 | 258 | 259 |
| 4 | 262,143 | slot 35, direct 4095, low symbol 14 | 4 | 5 |
| 5 | 262,144 | slot 35, direct 4095, low symbol 15 | 3 | 5 |
| 6 | 262,145 | slot 36, direct 0, low symbol 0 | 258 | 260 |
| 7 | 4 | slot 3 | 258 | 258 |

In every group the two symbols 259 produce exactly 2 and 257 bytes (no
adjustment for register matches) and the symbol 258 repeats 257 bytes
with the group's distance. The end marker follows. The output begins
`30 31 32 33 34 35 36 37 38 39 61 62 63 64 65 66 30 31 32 33` and ends
`35 36 2d 2e 35 36 2d 2e 35 36 2d 2e`.

**V8** (window wrap, largest distance): 2,081 packed bytes, namely the
35 bytes

`11 0c c0 0b e4 c9 2c 2d 00 00 7f a0 ce f1 00 00 15 13 6d c2 2a be dd e7 49 d2 65 e8 9c 72 cf 4d 76 de fe`

then 2,032 bytes `00`, then the 14 bytes

`3f 7f ff 61 fb ff f7 0b d7 ff e7 2c 38 40`

The table transmission ends at bit 227. Codes: main 258 = `0`,
277 = `100`, 298 = `101`, then with six bits 33, 97..103, 256, 259,
271..276 from `110000` (256 = `111000`, 259 = `111001`); distance
59 = `0`, 5 = `10`, 0 = `110`, 1 = `111`; low-distance 12 = `00`,
14 = `01`, 15 = `10`, 0 = `110`, 1 = `111`; length 7 = `0`, 0 = `10`,
1 = `11`.

Items: the literals `abcdefg`; an explicit match of length 258 and
distance 7; symbol 258 a total of 16,257 times, which brings the LZ
position to 4,194,571 (the copy in progress when the position passes
4,194,304 wraps around the ring); an explicit match of final
length 10 (slot 5, 5 + 3, plus 2 for the distance) and distance
**4,194,304** (slot 59, direct 16383, low symbol 15); the literal `!`;
an explicit match of final length 10 and distance 4,194,303; `!`; an
explicit match of final length 258 (slot 27, extra 29) and distance
4,194,301; symbol 259 with length 9; `!`; end marker. The output is
`abcdefg` repeated up to position 4,194,570 and then no longer in phase
with it; it begins `61 62 63 64 65 66 67 61` and ends
`65 66 67 61 62 63 64 65 66 67 61 62 63 64 65 66 67 61 62 63 64 65 66 21`.

**V10** (every distance slot, every length slot in both of its uses,
every two-byte symbol, every low-distance symbol, the repeat counter
running out): 2,573 packed bytes in three parts.

Part 1, the table transmission and three padding bits (65 bytes):

`1d c1 d0 c9 47 e1 ff f5 00 00 00 00 00 00 00 00 00 00 01 fc f3 e7 d0 00 00 00 00 0f d5 7b de f7 bd 55 55 55 55 55 55 55 55 55 55 55 55 5d dd dd dd dd dd dd dd b7 77 76 db 6d b6 db 6d b6 db 6d b0`

The table transmission ends at bit 516. Main code: 258 = `0`; every other
symbol present has 8 bits: the literals 128..214 have the codes `0x80`
to `0xD6` (**the code of each of these literals, written as a byte, is
the literal's own value**), 256 = `0xD7`, 259..262 = `0xD8`..`0xDB`,
263..270 = `0xDC`..`0xE3`, 271..298 = `0xE4`..`0xFF`; symbol 257 has no
code. Distance code: slot 0 = `000`; slots 1, 2, 3 and 10..59 have 6
bits, in this order from `001000`; slots 4..9 have 7 bits, from
`1111010`. Low-distance code: 0..14 have 4 bits (`0000`..`1110`),
15 = `11110`, 16 = `11111`. Length code: 0..3 have 4 bits, 4..27 have 5
bits from `01000`. Bits 517..519 are three symbols 258 with no previous
match (they do nothing, section 8.4), which brings the stream to a byte
boundary.

Part 2 (2,241 bytes) is regular and is given by rule:

- the three bytes `80 ff f8` (literal `0x80`; an explicit match of
  length 258 and distance 1: main 298, extra 31, distance slot 0),
  followed by 24 bytes `00` (192 symbols 258);
- then, for each byte value v from `0x81` to `0xD6` in ascending order:
  the byte v (a literal) followed by 24 bytes `00`;
- then, for each byte value v from `0xD5` down to `0xB6`: the byte v
  followed by one byte `00` (8 symbols 258).

After part 2 the LZ position is 4,376,057: the output so far is 49,795
bytes `0x80`, then 49,537 bytes of each value `0x81`..`0xD6`, then 2,065
bytes of each value `0xD5` down to `0xB6`. The window has wrapped.

Part 3 (267 bytes):

`ec f9 df 7f 72 77 fb bf bb 5d 3d 70 dd 1c f4 e1 9c bc 1b 30 fc db d2 a1 1c fb 71 e2 3d 3a 80 02 1d 5a 4f a2 5d 79 d6 b3 5d dc ba fa 5e f6 3f 41 ef 83 82 fa 4f 89 7c ee df 97 70 d7 a7 9b 6a cd 1f a7 b2 60 ff ea ab 15 7f 6d 52 a0 7b 8a 64 b3 e0 a8 d1 7f 9e 9d 3b 3e b2 68 da fb c9 42 cb f2 92 25 0f ee 23 89 3f e4 45 21 bf c1 08 7b e4 80 80 e4 7e 7c e4 7b ef c8 ef cf ad ce fa e4 6d b2 7b 4d 6c 6e cb 3b 0b a9 8a eb 91 63 f7 22 b6 7e 65 33 dd 49 d5 fc ea 42 3f fa b8 39 c8 8f 5f e5 20 0f dc 9e 07 cd 9c 4f 7e 68 fd f1 9b e8 cb 9f 1b f8 ef fe 1e 3f d3 a7 e1 d1 ed d7 e9 d2 57 21 3c a4 72 09 1a 2b 3c 4d cf 76 de 1b fb c0 fe 15 78 a9 e3 ab 6c 36 47 68 b6 0f 6d 1b 69 da 56 ca f6 18 da 6f 65 cd b7 f6 20 ec c6 da 95 b7 3b 66 8f 62 bb 6e db 6a ff 6b 0b b1 9b d9 d3 6d ed 76 39 dd ae af 6f cd d9 fb e0 35 d0`

Part 3 holds 60 explicit matches, one per distance slot, in the order
of the table below; then the literals `91 a2 b3 c4`; then the eight
two-byte matches (263 with extra 3: distance 4; 264 with extra 2: distance 7; 265 with extra 0: distance 9; 266 with extra 13: distance 30; 267 with extra 15: distance 48; 268 with extra 21: distance 86; 269 with extra 41: distance 170; 270 with extra 42: distance 235); then 28 register matches, one per length-code
symbol (second table); then the literal `80` and the end marker. The
distances were chosen so that each copy crosses a place where the
output changes value: a decoder with a wrong base value or extra-bit
count in any slot produces different bytes.

| # | Main symbol | Length extra | Distance slot | Direct bits (value) | Low part | Distance | Final length |
|--:|--:|--:|--:|--:|---|--:|--:|
| 1 | 279 | 1 | 59 | 14 (15343) | symbol 14 | 4,177,663 | 14 |
| 2 | 271 | 0 | 58 | 14 (16247) | symbol 15 | 3,929,984 | 5 |
| 3 | 280 | 0 | 57 | 14 (7864) | symbol 6 | 3,533,703 | 15 |
| 4 | 275 | 0 | 56 | 14 (14960) | symbol 12 | 3,385,101 | 9 |
| 5 | 272 | 0 | 55 | 14 (3480) | symbol 7 | 2,939,272 | 6 |
| 6 | 273 | 0 | 54 | 14 (10576) | symbol 8 | 2,790,665 | 7 |
| 7 | 274 | 0 | 53 | 14 (14577) | symbol 1 | 2,592,530 | 8 |
| 8 | 276 | 0 | 52 | 14 (1) | symbol 0 | 2,097,169 | 10 |
| 9 | 277 | 0 | 51 | 14 (10193) | symbol 2 | 1,998,099 | 11 |
| 10 | 278 | 0 | 50 | 14 (11097) | symbol 10 | 1,750,427 | 12 |
| 11 | 281 | 1 | 49 | 14 (15098) | symbol 5 | 1,552,294 | 18 |
| 12 | 282 | 0 | 48 | 14 (16003) | symbol 13 | 1,304,638 | 19 |
| 13 | 283 | 1 | 47 | 12 (1524) | symbol 9 | 1,007,434 | 22 |
| 14 | 284 | 0 | 46 | 12 (2525) | symbol 11 | 957,916 | 25 |
| 15 | 285 | 3 | 45 | 12 (431) | symbol 4 | 858,869 | 32 |
| 16 | 286 | 1 | 44 | 12 (1434) | symbol 3 | 809,380 | 34 |
| 17 | 287 | 7 | 43 | 12 (2435) | symbol 16: 3 | 759,860 | 44 |
| 18 | 288 | 2 | 42 | 12 (343) | counter: 3 | 660,852 | 47 |
| 19 | 289 | 6 | 41 | 12 (1344) | counter: 3 | 611,332 | 59 |
| 20 | 290 | 0 | 40 | 12 (2348) | counter: 3 | 561,860 | 61 |
| 21 | 291 | 2 | 39 | 12 (3351) | counter: 3 | 512,372 | 71 |
| 22 | 292 | 14 | 38 | 12 (1260) | counter: 3 | 413,380 | 99 |
| 23 | 293 | 12 | 37 | 12 (2266) | counter: 3 | 363,940 | 113 |
| 24 | 294 | 12 | 36 | 12 (178) | counter: 3 | 264,996 | 129 |
| 25 | 295 | 20 | 35 | 12 (1185) | counter: 3 | 215,572 | 152 |
| 26 | 296 | 24 | 34 | 12 (2195) | counter: 3 | 166,196 | 188 |
| 27 | 297 | 8 | 33 | 11 (1158) | counter: 3 | 116,836 | 204 |
| 28 | 298 | 0 | 32 | 11 (123) | counter: 3 | 67,508 | 228 |
| 29 | 271 | 0 | 31 | 10 (128) | counter: 3 | 51,204 | 4 |
| 30 | 271 | 0 | 30 | 10 (636) | counter: 3 | 42,948 | 4 |
| 31 | 271 | 0 | 29 | 9 (503) | counter: 3 | 32,628 | 4 |
| 32 | 271 | 0 | 28 | 9 (499) | counter: 3 | 24,372 | 4 |
| 33 | 278 | 0 | 27 | 8 (239) | symbol 10 | 16,123 | 11 |
| 34 | 271 | 0 | 26 | 8 (108) | symbol 9 | 9,930 | 4 |
| 35 | 280 | 0 | 25 | 7 (108) | symbol 6 | 7,879 | 13 |
| 36 | 279 | 1 | 24 | 7 (108) | symbol 2 | 5,827 | 12 |
| 37 | 277 | 0 | 23 | 6 (43) | symbol 10 | 3,771 | 9 |
| 38 | 271 | 0 | 21 | 5 (7) | symbol 14 | 1,663 | 3 |
| 39 | 271 | 0 | 20 | 5 (22) | symbol 7 | 1,384 | 3 |
| 40 | 273 | 0 | 19 | 4 (12) | symbol 15 | 976 | 5 |
| 41 | 277 | 0 | 18 | 4 (10) | symbol 11 | 684 | 9 |
| 42 | 292 | 13 | 17 | 3 (0) | symbol 8 | 393 | 96 |
| 43 | 298 | 29 | 22 | 6 (1) | symbol 12 | 2,077 | 256 |
| 44 | 271 | 0 | 16 | 3 (7) | symbol 5 | 374 | 3 |
| 45 | 297 | 10 | 15 | 2 (0) | symbol 1 | 194 | 205 |
| 46 | 294 | 9 | 14 | 2 (0) | symbol 0 | 129 | 124 |
| 47 | 292 | 11 | 13 | 1 (0) | symbol 4 | 101 | 94 |
| 48 | 290 | 7 | 12 | 1 (0) | symbol 3 | 68 | 66 |
| 49 | 290 | 6 | 11 | 0 | symbol 13 | 62 | 65 |
| 50 | 287 | 3 | 10 | 0 | symbol 9 | 42 | 38 |
| 51 | 284 | 2 | 9 | 3 (0) | – | 25 | 25 |
| 52 | 282 | 1 | 8 | 3 (0) | – | 17 | 18 |
| 53 | 284 | 3 | 7 | 2 (0) | – | 13 | 26 |
| 54 | 276 | 0 | 6 | 2 (0) | – | 9 | 8 |
| 55 | 275 | 0 | 5 | 1 (0) | – | 7 | 7 |
| 56 | 278 | 0 | 4 | 1 (0) | – | 5 | 10 |
| 57 | 276 | 0 | 3 | 0 | – | 4 | 8 |
| 58 | 271 | 0 | 2 | 0 | – | 3 | 3 |
| 59 | 272 | 0 | 1 | 0 | – | 2 | 4 |
| 60 | 271 | 0 | 0 | 0 | – | 1 | 3 |

Low-distance symbol 16 in match 17 starts the repeat counter; matches 18
to 32 read no low-distance symbol; match 33 reads one again.

| # | Main symbol | Length symbol | Length extra | Distance | Length |
|--:|--:|--:|--:|--:|--:|
| 1 | 262 | 0 | 0 | 48 | 2 |
| 2 | 260 | 1 | 0 | 235 | 3 |
| 3 | 261 | 2 | 0 | 170 | 4 |
| 4 | 259 | 3 | 0 | 170 | 5 |
| 5 | 262 | 4 | 0 | 86 | 6 |
| 6 | 262 | 5 | 0 | 48 | 7 |
| 7 | 261 | 6 | 0 | 170 | 8 |
| 8 | 260 | 7 | 0 | 48 | 9 |
| 9 | 259 | 8 | 0 | 48 | 10 |
| 10 | 261 | 9 | 1 | 86 | 13 |
| 11 | 260 | 10 | 0 | 48 | 14 |
| 12 | 262 | 11 | 1 | 235 | 17 |
| 13 | 259 | 12 | 1 | 235 | 19 |
| 14 | 260 | 13 | 2 | 48 | 24 |
| 15 | 261 | 14 | 2 | 86 | 28 |
| 16 | 262 | 15 | 2 | 170 | 32 |
| 17 | 260 | 16 | 3 | 86 | 37 |
| 18 | 259 | 17 | 6 | 86 | 48 |
| 19 | 262 | 18 | 6 | 235 | 56 |
| 20 | 261 | 19 | 7 | 170 | 65 |
| 21 | 261 | 20 | 5 | 86 | 71 |
| 22 | 259 | 21 | 11 | 86 | 93 |
| 23 | 260 | 22 | 6 | 170 | 104 |
| 24 | 262 | 23 | 5 | 48 | 119 |
| 25 | 259 | 24 | 29 | 48 | 159 |
| 26 | 261 | 25 | 11 | 86 | 173 |
| 27 | 262 | 26 | 13 | 235 | 207 |
| 28 | 260 | 27 | 15 | 86 | 241 |

After the last match the registers hold (86, 235, 48, 170). The last
4,381 bytes of the output (everything after part 2) have the CRC-32
`0xC990BF75`; they begin `83 83 83 83 83 83 83 83 83 83 83 83 84 84 88
88 88 88 89 90` and the whole output ends `83 b6 83 b6 83 83 83 b6 b6 b6
b6 b6 b6 80`.

### 14.2 Valid streams: solid group

**V5.** Four members of one group (sections 13.4). `unar` decodes
members 1 to 3 and fails on member 4 (section 16, gap 1); it decodes the
two-member group formed by members 1 and 2 alone correctly.

| Member | Solid bit | Packed bytes | U | Output | CRC-32 |
|--:|---|---|--:|---|---|
| 1 | no | below (593 bits) | 73 | `solid groups share one decoder state between their members; een thder are` | `0xDC1B9F23` |
| 2 | yes | `db 9f e5 67 56 a0` (45 bits) | 18 | ` ons; eeps sh.ir m` (first byte is a space) | `0x71213E5B` |
| 3 | yes | `d5` (8 bits) | 0 | nothing | `0x00000000` |
| 4 | yes | `4f dd 00 cc 08 01 09 49 9f eb bd 2f 54 15 46 87 f1 49 22 0c 04 78 e5 ba` (191 bits) | 14 | `embeecoe one /` | `0x40EB3CE4` |

Member 1 packed bytes (75):

`0c 0c d0 cb e5 0f 15 98 10 60 06 35 69 62 2d 48 b6 8b bb 84 6f a2 00 0f 80 18 6c 9e e2 8d e4 8d f9 7f 9b 6d e1 ab 5c 65 01 82 57 3c 94 2c 5c 84 56 84 40 df 58 14 0a ce 62 1e 5b 42 68 1a 26 48 26 66 f2 8b 70 ff 2f 7d e1 ea 00`

A decoder that mishandles any of the following fails this vector: the
clear header-pending flag (member 2 has no tables), the registers, last
match and low-distance counter crossing a member boundary, the empty
member whose only content is an end marker, the keep flag across
members, and the reset of the low-distance value by a table
transmission.

### 14.3 Valid streams that involve the filter layer

The expected outputs assume the standard delta filter of
`rar29-filters.md`, whose 29-byte program is transmitted inside the
first record of each vector (V6c uses another standard program, as
stated there). A decoder without a filter layer can still
check that it finds the records at the stated positions.

**V6** (three records; program reuse; block length reuse). U = 40, 103
packed bytes, 820 bits used, both tools:

`01 00 14 87 e5 4f 36 00 00 02 ab ee aa aa aa b7 ff cb d8 f7 76 5f dd 82 77 61 e1 6c 38 00 85 01 05 1d 2f 01 9a 41 80 ec 27 48 2f 09 76 6d d3 ea 41 5b 59 44 e8 17 5c e1 6c 91 4c 4e 3f 77 00 00 02 48 a0 10 95 57 65 b7 5f 86 39 67 a6 bb 04 98 28 88 10 46 fc 73 d7 7e 7b f7 e2 19 0a 63 a1 2a 5b 1a e7 c2 32 9d 10`

The unfiltered output is the 40 bytes
`ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmn`. The items are: a filter
record, a second filter record, 13 literals, a third filter record, 27
literals, the end marker.

| Record | Read at LZ position | Flags byte | Body | Offset | Block | Remarks |
|--:|--:|---|---|--:|---|---|
| 1 | 0 | `b6`, then size byte `1c` (28 + 7 = 35 bytes) | `00 42 80 82 8e 97 80 cd 20 c0 76 13 a4 17 84 bb 36 e9 f5 20 ad ac a2 74 0b ae 70 b6 48 a6 27 1f bb 80 00` | 4 | positions 4..13 (length 10) | resets the stored programs, transmits the delta program, one channel |
| 2 | 0 | `12` (3 bytes) | `45 00 84` | 20 | positions 20..29 | same program and length as before, two channels |
| 3 | 13 | `93` (4 bytes) | `05 11 02 08` | 17 | positions 30..39 | names the stored program explicitly, same length, one channel |

Expected output:

`41 42 43 44 bb 75 2e e6 9d 53 08 bc 6f 21 4f 50 51 52 53 54 ab a6 55 45 fe e3 a6 80 4d 1c 9b 35 ce 66 fd 93 28 bc 4f e1`

CRC-32 `0x58CECA18`.

**V6b** (flag bit `0x40`). U = 291, 87 packed bytes, 690 bits used, both
tools:

`00 d0 00 4f e5 0f 36 b6 db 6d 00 00 3d 86 10 37 a3 1d 77 4c ba 4c 12 4c 3e d7 c6 bd 87 00 11 14 02 22 3a 5e 03 34 83 01 d8 4e 90 5e 12 ec db a7 d4 82 b6 b2 89 d0 2e b9 c2 d9 22 98 9c 7e ee 00 37 f7 1c f5 df 9e fd f8 02 21 90 a6 3a 12 a5 b1 ae 7c 23 29 d2 b9 40`

Items: literals `xyz`; a filter record read at LZ position 3 (flags byte
`f6`, size byte `1c`, 35 body bytes
`00 44 50 08 88 e9 78 0c d2 0c 07 61 3a 41 78 4b b3 6e 9f 52 0a da ca 27 40 ba e7 0b 64 8a 62 71 fb b8 00`)
with offset 4 and length 20, four channels; an explicit match of length
258 and distance 3; the 30 literals `ABCDEFGHIJKLMNOPQRSTUVWXYZabcd`; the
end marker. Because bit `0x40` is set the block starts at 3 + 4 + 258 =
265 and covers positions 265..284. Expected output: `xyz` repeated 87
times (261 bytes), then `41 42 43 44`, then the 20 filtered bytes
`bb b6 b1 ac 75 6b 61 57 2e 1f 10 01 e6 d2 be aa 9d 84 6b 52`, then
`59 5a 61 62 63 64`. CRC-32 `0x77A882C1`.

**V6c** (solid group; the file offset is relative to the member; a
program stored by one member is used by the next). The filter is the
standard 57-byte x86 call-and-jump program of `rar29-filters.md` (it
rewrites the four bytes after each `e8` or `e9` byte, using the byte's
file offset). `unar` decodes both members as stated; `bsdtar` does not
read solid archives.

| Member | Solid bit | Packed bytes | U | Unfiltered output | Expected output | CRC-32 |
|--:|---|---|--:|---|---|---|
| 1 | no | below (84 bytes, 670 bits) | 16 | `41 42 e8 00 01 00 00 43 e9 00 02 00 00 44 45 46` | `41 42 e8 fd 00 00 00 43 e9 f7 01 00 00 44 45 46` | `0x1BB20D41` |
| 2 | yes | `f0 00 cb a9 c1 02 34 50 97 6e 40` (82 bits) | 17 | `46 45 44 e8 00 01 00 00 43 e9 00 02 00 00 44 42 41` | `46 45 44 e8 fc 00 00 00 43 e9 f6 01 00 00 44 42 41` | `0x989B2C75` |

Member 1 packed bytes:

`00 0c 07 ec cb 13 30 2f f0 c2 16 5f fd fa 63 60 02 31 39 84 1b 01 28 11 10 69 80 80 00 00 0d 13 a1 01 c6 89 d2 80 ac 97 62 85 5c c9 05 c9 2f 81 48 c8 aa 98 18 95 72 88 81 aa c9 5b 00 20 ab 6a 03 35 58 11 a2 48 21 b0 12 91 f4 b8 06 7c 10 23 45 09 ab e0`

Member 1: a filter record read at LZ position 0 (flags byte `a6`, size
byte `36`, 61 body bytes
`00 23 13 98 41 b0 12 81 11 06 98 08 00 00 00 d1 3a 10 1c 68 9d 28 0a c9 76 28 55 cc 90 5c 92 f8 14 8c 8a a9 81 89 57 28 88 1a ac 95 b0 02 0a b6 a0 33 55 81 1a 24 82 1b 01 29 1f 4b 80`:
reset, offset 2, length 12, the program), 16 literals, an end marker
that **clears** the header-pending flag. The block is LZ positions
2..13, file offset 2. Main code of member 1: 0 = `00`, 1 = `0100`,
2 = `0101`, 65..70 = `0110`, `0111`, `1000`, `1001`, `1010`, `1011`,
232 = `1100`, 233 = `1101`, 256 = `1110`, 257 = `1111`.

Member 2 starts directly with items: main 257 (`1111`), flags byte `00`
(a one-byte body, no program number, no length: the last program and its
last block length are used), body `0c` (offset 3), then 17 literals and
the end marker. The record is read at LZ position 16, so the block is LZ
positions 19..30, which are bytes 3..14 **of member 2**: the file offset
handed to the filter is 3, not 19. (With 19 the output would contain
`ec 00 00 00` and `e6 01 00 00` instead of `fc 00 00 00` and
`f6 01 00 00`.)

**V6f** (a filter announced in one member whose block lies in the
next). `unar` decodes both members as stated; `bsdtar` does not read
solid archives.

| Member | Solid bit | Packed bytes | U | Expected output | CRC-32 |
|--:|---|---|--:|---|---|
| 1 | no | below (69 bytes, 546 bits) | 16 | `ABCDEFGHIJKLMNOP` | `0xE0E8FF4D` |
| 2 | yes | `f3 df bf 00 44 32 14 c7 42 54 b6 35 cf 84 65 3a 20` (131 bits) | 24 | `61 62 63 64 9b 35 ce 66 fd 93 28 bc 4f e1 6f 70 71 72 73 74 75 76 77 78` | `0xFF373421` |

Member 1 packed bytes:

`3c c4 be 4c fb 6a aa aa aa ae 16 a8 00 00 3f c3 ff d6 b6 c3 80 22 85 01 05 1d 2f 01 9a 41 80 ec 27 48 2f 09 76 6d d3 ea 41 5b 59 44 e8 17 5c e1 6c 91 4c 4e 3f 77 00 16 5b 75 f8 63 96 7a 6b b6 fc 73 d7 74 00`

Member 1: a filter record at LZ position 0 (flags byte `b6`, size byte
`1c`, body
`01 14 28 08 28 e9 78 0c d2 0c 07 61 3a 41 78 4b b3 6e 9f 52 0a da ca 27 40 ba e7 0b 64 8a 62 71 fb b8 00`:
reset, offset 20, length 10, delta program, one channel), 16 literals,
an end marker that clears the header-pending flag. The block, LZ
positions 20..29, has not started when member 1 ends, so the entry stays
queued. Member 2 starts directly with items: the 24 literals
`abcdefghijklmnopqrstuvwx` and the end marker. Its bytes 4..13 are LZ
positions 20..29 and are delivered filtered (file offset 4).

**V6d** (a reset discards a queued filter that has not started). U = 40,
122 packed bytes, 973 bits used. `bsdtar` gives the expected output;
`unar` fails (section 16, gap 5).

`3c c4 be 4c fb 6a aa aa aa aa a0 33 00 03 ff d0 ff f5 ad b0 e0 02 94 04 14 74 bc 06 69 06 03 b0 9d 20 bc 25 d9 b7 4f a9 05 6d 65 13 a0 5d 73 85 b2 45 31 38 fd dc 00 05 96 dd 5b 61 c0 11 12 80 82 8e 97 80 cd 20 c0 76 13 a4 17 84 bb 36 e9 f5 20 ad ac a2 74 0b ae 70 b6 48 a6 27 1f bb 80 0b f0 c7 2c f4 d7 6d f8 e7 ae fc f7 ef c0 11 0c 85 31 d0 95 2d 8d 73 e1 19 4e 88`

The unfiltered output is the 40 bytes of V6. Items: a filter record at
LZ position 0 (flags byte `b6`, size byte `1c`, body
`00 52 80 82 8e 97 80 cd 20 c0 76 13 a4 17 84 bb 36 e9 f5 20 ad ac a2 74 0b ae 70 b6 48 a6 27 1f bb 80 00`:
reset, offset 5, length 10, delta program, one channel; block 5..14);
the literals `ABC`; a second record at LZ position 3 (flags byte `b6`,
size byte `1c`, body
`01 11 28 08 28 e9 78 0c d2 0c 07 61 3a 41 78 4b b3 6e 9f 52 0a da ca 27 40 ba e7 0b 64 8a 62 71 fb b8 00`:
**reset again**, offset 17, length 10, the program again, one channel;
block 20..29); 37 literals; the end marker. The second record empties
the queue, so block 5..14 is delivered unfiltered. Expected output:

`41 42 43 44 45 46 47 48 49 4a 4b 4c 4d 4e 4f 50 51 52 53 54 ab 55 fe a6 4d f3 92 30 cd 69 65 66 67 68 69 6a 6b 6c 6d 6e`

CRC-32 `0x40C8ABEC`.

**V6e** (a reset after a filter has run leaves its result in place).
U = 40, 122 packed bytes, 973 bits used. `bsdtar` gives the expected
output; `unar` fails (section 16, gap 5).

`3c c4 be 4c fb 6a aa aa aa aa a0 33 00 03 ff d0 ff f5 ad b0 e0 00 14 04 14 74 bc 06 69 06 03 b0 9d 20 bc 25 d9 b7 4f a9 05 6d 65 13 a0 5d 73 85 b2 45 31 38 fd dc 00 05 96 dd 7e 18 e5 9e 9a ed bd 6d 87 00 20 a0 20 a3 a5 e0 33 48 30 1d 84 e9 05 e1 2e cd ba 7d 48 2b 6b 28 9d 02 eb 9c 2d 92 29 89 c7 ee e0 00 38 e7 ae fc f7 ef c0 11 0c 85 31 d0 95 2d 8d 73 e1 19 4e 88`

Items: a filter record at LZ position 0 (reset, offset 0, length 10,
delta program, one channel; block 0..9); the 12 literals `ABCDEFGHIJKL`
(the first filter runs as soon as position 10 is reached); a second
record at LZ position 12 (reset, offset 8, length 10, the program again,
one channel; block 20..29); 28 literals; the end marker. Expected
output:

`bf 7d 3a f6 b1 6b 24 dc 93 49 4b 4c 4d 4e 4f 50 51 52 53 54 ab 55 fe a6 4d f3 92 30 cd 69 65 66 67 68 69 6a 6b 6c 6d 6e`

CRC-32 `0x20BB7497`.

### 14.4 Valid streams that involve the PPMd model

These need the model of `rar29-ppmd.md`. They were produced with a
throw-away PPMd encoder written from that document's draft.

| # | Packed bytes | U | Expected output | CRC-32 | Tools |
|--:|---|--:|---|---|---|
| V7a | `a5 00 67 fb 83 54 d7 35 97 59 23 b2 ac fa 58 31 51 81 36 a7 cc b1 4d d0 d4 c6 33 e4 af c1 25 9d f1 90 c2 26 72 fc 9a 4c b9 de 10 91 b2 00` | 89 | below | `0xFB888F1C` | both |
| V7b | `0c c8 bf 4a 98 ab 7f 88 39 60 2c 3c 3c bb f8 20 78 71 4c a1 d2 e5 00 40 70 03 32 de 0e 7a a4 8b f0 6c 17 1e 81 43 c5 47 f0 bc 4b 9a 3f f9 02 b3 95 af 30 83 1c 36 e8` | 57 | `XYZXYWXYWXpqrs` then 42 more `s` then `Z` | `0xC71586A4` | `unar` (`bsdtar` loses the first ten bytes) |
| V7c | below | 30 | below | `0xFE835A31` | `unar` |
| V7d | two members, below | 13, 12 | `first member ` and `seconddddddd` | `0xD13E3D71`, `0x451310E3` | `unar` |

- **V7a**: one PPMd block. Header `a5`: PPMd, new model (bit `0x20`),
  order 5 + 1 = 6; next byte `00`: 1 MiB; no escape byte, so the escape
  value is 2. The decoded bytes are: the 43 bytes
  `hello hello hello, said the yellow fellow. `; then `02 01` (literal
  2); `02 05 06` (distance 1, length 6 + 4 = 10: ten more bytes 2);
  `02 04 00 00 03 00` (distance 3 + 2 = 5, length 0 + 32 = 32: 32 bytes
  2); `02 06` (literal 2); `02 09` (literal 2); `21`; `02 02` (end
  marker). Output: the 43 text bytes, then 45 bytes `02`, then `21`.
- **V7b**: an LZ block of 21 bytes (literals `XYZXYW`, an explicit match
  of distance 3 and length 4, then main 256 and `1`), a PPMd block of 18
  bytes starting `e5 00 40` (new model of order 6 and 1 MiB, escape
  value `0x40`), and an LZ block of 16 bytes with keep flag 1. The PPMd
  block decodes `pqrs`, then `40 05 00` (distance 1, length 4), `40 04
  00 00 00 00` (distance 2, length 32), `40 00` (new block). The second
  LZ block contains main 258, main 259 with length 2, the literal `Z`
  and the end marker; the 258 repeats the match (3, 4) of the **first**
  LZ block and the 259 uses register 0 = 3, because the PPMd matches do
  not touch the match memory: the last eight bytes before `Z` are `s`.
- **V7c**: a filter record through escape code 3. Packed bytes (78):

  `a5 00 61 03 6d f5 70 fd 6f 05 a5 42 2b 46 24 5c 1e 27 4a cf f0 02 25 d8 a9 61 29 37 89 91 54 10 87 9a e4 19 b1 5c e1 2d d7 75 53 8b dd 2e f7 57 ef a9 aa 94 a9 61 cd 62 a3 ad 68 f0 86 00 7b 5c 50 6d f3 9f 78 5a 6b db 5f b6 82 ad ef 00`

  Decoded bytes: `abcde`, then `02 03`, the flags byte `b6`, the size
  byte `1c`, the 35 body bytes
  `00 43 00 84 8e 97 80 cd 20 c0 76 13 a4 17 84 bb 36 e9 f5 20 ad ac a2 74 0b ae 70 b6 48 a6 27 1f bb 80 00`
  (delta program, offset 4, length 12, two channels; read at LZ position
  5, so the block is positions 9..20), then `fghijklmnopqrstuvwxyzABCD`,
  then `02 02`. Expected output:
  `61 62 63 64 65 66 67 68 69 96 90 2b 1f bf ad 52 3a e4 c6 75 51 76 77 78 79 7a 41 42 43 44`.
- **V7d**: a solid group. Member 1 (no solid bit, U = 13):
  `e5 00 07 66 05 57 f8 47 d6 3a d5 0b a3 b9 14 ea 6e eb 56 b2`: new
  model, escape value 7; decodes `first member ` then `07 02`. Member 2
  (solid bit, U = 12): `80 88 2a 72 af 84 c3 4a 6d df 1d f7 2b 00`:
  header `80`: PPMd block, **model kept, escape value kept**; decodes
  `second`, `07 05 02` (distance 1, length 6), `07 02`.

### 14.5 Streams a decoder must reject

"Tools" gives what the two tools do with the same bytes.

| # | Packed bytes | U | Why it is corrupt input | Tools |
|--:|---|--:|---|---|
| E1 | `00 80 08 00 00 00 00 00 0c 0c b0 00 00 00 00 00` | 4 | Pre-code: symbols 1, 4 and 19 with length 2, 16 and 18 with length 3. The first coded length is symbol 16 (copy previous) at position 0. | both fail |
| E2 | `00 40 04 00 00 00 00 00 00 00 80 00 00 00 00 00` | 4 | Pre-code over-subscribed: symbols 1 and 4 have length 1 and symbol 19 has length 2. | both fail |
| E3 | `00 80 0b ec 8b 6c aa aa ff f4 27 fe c0 00 00 00 00 00` | 4 | Main code over-subscribed: symbols 65..73 have length 4 and symbol 256 has length 1. | both fail |
| E4 | `00 80 0b ec 8b 6c aa ab ff d1 1e c0 31 d1 35 64` | 4 | Length code over-subscribed (symbols 0, 1, 2 all have length 1); the items (`ABCD`, end marker) never use it. | both fail |
| E5 | `00 8c bf 0c aa ca c2 db d6 82 e8 76 dc 92 a0 82 04 3e 5c 9e 20` | 9 | `a` `b` `c`, then an explicit match of length 5 and distance 10 with only 3 bytes produced (DECISION, section 10). | both output `61 62 63 00 00 00 00 00 7a` |
| E6 | `08 80 bf 4b ad 55 5f fc 24 16 71 75 eb 84 5c 78 29 cb f9` | 9 | `abcdef`, then main 259 while register 0 is unset (DECISION, section 8.3). | both output `abcdef` and three zero bytes |
| E7 | `0c 8c bf 4a ac aa b7 f8 23 06 61 bf ec 4b 02 08 10 f0 53 97 b9` | 9 | `abcdef`, then a match of length 6 and distance 6 when only 3 bytes are missing (DECISION, section 12.1). | both output `abcdefabc` |
| E8 | `00 88 bf 4b ad 55 5f fc 23 9f 0d 78 61 03 1e 9c bb 88` | 10 | `abcdef`, then the end marker with 4 bytes missing. | both fail |
| E9 | `00 88 bf 4b ad 55 5f fc 23 9f 0d 78` | 6 | Ends inside the table transmission (the first 12 bytes of E8). | both fail |
| E10 | no bytes | 6 | P = 0 with U greater than 0. | `unar` fails |
| E11 | `00 08 0b ec 8b 6c aa ab ff d1 1f fb 22 b3 e0 00 00 00 00` | 8 | Main code incomplete: symbols 65..72 have length 4 and symbol 256 has length 2, leaving the patterns starting `11` unassigned. Items `A` `B` `C` `D`, then the bits `11`. | both fail |
| E12 | `00 07 f8 75 87 fc b3 e4 6c 00 00 00 00 00` | 6 | `a` `b` `c`, then main 271 while the distance code is empty. | both fail |
| E13 | below | 40 | A filter record at LZ position 0 with offset 30 and length 20 in a member of 40 bytes: the block is incomplete when the member ends (section 11.4). | both fail |
| E14 | below | 40 | Two filter records at LZ position 0: offset 4 length 12, then offset 10 with the same length: the blocks overlap (section 11.2). | both fail |
| E17 | below | 40 | Two filter records at LZ position 0 for the same block: offset 4 length 12, then offset 4 with the same length (flags byte `12`, body `10 08 40`). A range cannot have a second filter (section 11.2). | both fail |
| E15 | `80` followed by 20 bytes `00` | 5 | PPMd block that keeps a model when the group has none. | both fail |
| E16 | `a0 00` followed by 20 bytes `00` | 5 | PPMd block with a new model of order 1. | both fail |

E13 packed bytes (93):

`01 3c 44 be 50 f3 6a aa aa aa aa a0 3d 80 01 bf fc 31 ee ec bf bb 04 ee c3 eb 6c 3a 02 3c 8a 01 05 1d 2f 01 9a 41 80 ec 27 48 2f 09 76 6d d3 ea 41 5b 59 44 e8 17 5c e1 6c 91 4c 4e 3f 77 00 01 65 b7 5f 86 39 67 a6 bb 6f c7 3d 77 e7 bf 7e 00 88 64 29 8e 84 a9 6c 6b 9f 08 ca 74 40`

E14 packed bytes (96):

`01 3c 44 be 50 f3 6a aa aa aa aa a0 3d 80 01 bf fc 31 ee ec bf bb 04 ee c3 eb 6c 38 00 86 01 05 1d 2f 01 9a 41 80 ec 27 48 2f 09 76 6d d3 ea 41 5b 59 44 e8 17 5c e1 6c 91 4c 4e 3f 77 00 01 51 22 80 82 0b 2d ba fc 31 cb 3d 35 db 7e 39 eb bf 3d fb f0 04 43 21 4c 74 25 4b 63 5c f8 46 53 a2`

E17 packed bytes (89):

`3c c4 be 4c fb 6a aa aa aa aa a0 33 00 03 ff d0 ff f5 ad b0 e0 02 18 04 14 74 bc 06 69 06 03 b0 9d 20 bc 25 d9 b7 4f a9 05 6d 65 13 a0 5d 73 85 b2 45 31 38 fd dc 00 05 44 84 02 10 2c b6 eb f0 c7 2c f4 d7 6d f8 e7 ae fc f7 ef c0 11 0c 85 31 d0 95 2d 8d 73 e1 19 4e 88`

For E5, E6 and E7 the CRC-32 in a test archive must be that of the
output the tools produce if the vector is also to be shown to them; a
conforming decoder reports corrupt input regardless.

### 14.6 Streams with a defined result that is not an error

| # | Packed bytes | U | Result | Tools |
|--:|---|--:|---|---|
| N1 | `00 88 bf 4b ad 54 bf fd 78 6b c3 08 18 f9 77 10` | 6 | The stream holds the six literals `abcdef` and two padding bits; the main code has no symbol 256. Output `abcdef` (CRC-32 `0x4B8E39EF`), **group state unusable** (section 12.1). | both fail |
| N2 | `00 88 bf 4b ad 55 58 87 ff 9b 0d 78 61 03 1e 9c bb 80 00` | 6 | `abcdef`, then four literals `z` instead of an end marker. Output `abcdef`, group state unusable. | `unar` fails; `bsdtar` outputs `abcdef` |
| N3 | `0c 80 8f f0 aa df fe ab 7f 81 39 8e 84 bc 20 81 0f cd ef 06 a7 10` | 8 | `abcdef`, main 258 with no previous match (does nothing), `g` `h`, end marker. Output `abcdefgh` (CRC-32 `0xAEEF2A50`). | both |
| N4 | `00 80 8b f0 ba d5 54 ab ff c0 39 f0 d7 86 10 31 f5 79 bd f1 05 38 80` | 6 | `abcdef`, end marker, then further bits (four literals and a second end marker) that must not be read. Output `abcdef`. | both |
| N5 | below | 40 | A filter record at LZ position 0 with offset 50 and length 20 in a member of 40 bytes: the block never starts; the entry is still queued when the group ends and is discarded (section 11.4). Output `ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmn` (CRC-32 `0x6EB9BC73`). | both |

N5 packed bytes (93):

`01 3c 44 be 50 f3 6a aa aa aa aa a0 3d 80 01 bf fc 31 ee ec bf bb 04 ee c3 eb 6c 3a 02 64 8a 01 05 1d 2f 01 9a 41 80 ec 27 48 2f 09 76 6d d3 ea 41 5b 59 44 e8 17 5c e1 6c 91 4c 4e 3f 77 00 01 65 b7 5f 86 39 67 a6 bb 6f c7 3d 77 e7 bf 7e 00 88 64 29 8e 84 a9 6c 6b 9f 08 ca 74 40`

## 15. Safety requirements for untrusted input

1. **Never size a buffer from U.** U is a claim. Output is produced
   incrementally; the fixed allocations of this decoder are the 4 MiB
   window, the 404-entry length table, the code tables and a hold-back
   area of at most 246,046 bytes for an incomplete filter block (section
   11.3). The PPMd model memory and the filter machine are bounded in
   their own documents. The caller's limits on the unpacked size of a
   member and of an archive apply as for every other format.
2. **U cannot be bounded by P.** One LZ item can produce 260 bytes from
   a single bit (symbol 258 with a one-bit code), and PPMd blocks
   compress long runs far below a bit per item (a real member of the
   survey expands 26 packed bytes to 300,000). Do not reject a member
   for its ratio; rely on incremental output and the caller's limits.
   The only size check available before decoding is P = 0 with U greater
   than 0.
3. **Every read is bounded by the member's packed bytes** (section 3),
   for the bit reader and for the bytes the PPMd range decoder takes.
   In LZ blocks every item consumes at least one bit, a block header at
   least one byte and a filter record at least 17 bits, so no sequence
   of block switches, empty tables, filter records or anything else can
   loop without end: the work is bounded by a constant times P plus the
   output produced. For PPMd blocks `rar29-ppmd.md` states the
   corresponding bound.
   That constant must stay small **per block header** as well. An LZ
   block header with its table transmission can be as short as 7 bytes
   and a stream may consist of nothing else, so the cost of building the
   four codes must be proportional to the 404 symbols, not to a large
   fixed table: a decoder that fills a 32,768-entry lookup table for
   each code on every transmission spends tens of thousands of steps per
   input byte. Use lookup tables of a few hundred to a few thousand
   entries with a slower path for longer codes, or build lazily.
   Likewise a PPMd block header that asks for a new model can follow the
   previous one after a handful of bytes: creating a model must not cost
   time proportional to the declared memory size (`rar29-ppmd.md`
   requires the memory to be obtained on demand).
4. **Length-table writes stay inside positions 0..403** and pre-code
   length writes inside symbols 0..19 (sections 7.2, 7.3: runs are cut
   off; symbols 16 and 17 at position 0 are rejected because there is no
   position −1).
5. **Reject over-subscribed codes when they are built**, including the
   pre-code and including codes the block never uses. Decoding with an
   empty or incomplete code must fail cleanly on an unassigned pattern
   and must not index outside a lookup structure. Decoding an LZ item
   when the group has no codes at all (section 12.3) must fail cleanly
   too.
6. **Check every match before copying**, whatever produced it (main
   symbols 258..298, PPMd codes 4 and 5): distance at least 1 (an unset
   register is rejected, section 8.3); distance not greater than the LZ
   position and not greater than 4,194,304 (section 10); length not
   greater than the bytes missing from the member (section 12.1). All
   ring positions are reduced modulo 4,194,304.
7. **Filter queue.** At most 65,536 entries; block length 1..245,760;
   blocks in ascending order without overlap or repetition (section
   11.2). A queue entry needs only its start, its length and what the
   filter layer keeps for the run: nothing is allocated for the block
   itself until it is complete. The start
   of a block is a 64-bit sum (LZ position + up to 2 to the power 32 +
   258) and must not be truncated to 32 bits. A filter's output must
   have its input's length. A record body is at most 65,535 bytes; read
   it into a buffer that grows with the bytes actually present, or that
   is at most that size.
8. **The window is written only by the decoder itself**, never by a
   filter, and a filter is given a copy of its block, so a hostile
   filter program cannot alter what later matches copy.
9. **Totals are 64-bit.** A group can exceed 4 GiB of output; ring
   positions are derived from the LZ position modulo the window size,
   and file offsets passed to filters are 64-bit.
10. **PPMd block headers** can request up to 256 MiB of model memory
    with two bytes of input. The policy for that allocation belongs to
    `rar29-ppmd.md`; this decoder must at least make it fallible (an
    allocation failure is an error for the member, not a crash) and must
    not make it before the header's own validity checks (order at least
    2). A header that keeps the model when the group has none must be
    rejected before anything is decoded with it (section 6.2).
11. **A failed member poisons its group** (section 12.4); the state is
    discarded, never reused.
12. Verify the CRC-32 of every delivered member (section 2).
13. **A reset ordered by a filter record** (section 11.2) empties the
    queue while the decoder may be holding back output for the queue's
    first block. Release that output as unfiltered; do not keep any
    reference to a discarded entry.
14. **Stored members and directory entries between the members of a
    group** never reach this decoder and never touch its state (section
    12.2).

## 16. Observed implementation gaps

Faults and leniencies of the reference implementation (`unar` 1.10.7).
They explain why rules exist; nothing here is to be imitated.

1. **Empty members are skipped without being read, which breaks the
   members after them.** The reference implementation never opens the
   packed bytes of a member with U = 0. In a solid group it thereby
   misses the end marker such a member contains, with its header-pending
   bit (section 12.2), and when the empty member is the first of its
   group, the tables. Real archives are affected: in 14 of the 18 solid
   archive sets written by RAR 6.24 from a file set containing three
   empty files, `unar` reports 4 of 13 files as failed ("Attempted to
   read more data than was available"), while the rules of section 12.2
   decode every file with a matching CRC-32. Vector V5 shows the same.
2. **U plays no part in decoding.** The group is treated as one
   continuous output stream cut at the declared sizes; only the end
   marker switches to the next member's packed bytes. A match that
   crosses the end of a member is split silently between the two files
   (vector E7 for a last member); a member whose marker comes late or
   never makes the decoder read on past the member's data (vectors N1,
   N2: the member fails although its bytes were decoded).
3. **A distance beyond the start of the output reads zero bytes.** The
   window starts zero-filled and no check is made (vector E5). **An
   unset register** yields distance 0, and the copy then reads the ring
   position it is about to write: zeros in a fresh window, stale output
   after a wrap (vector E6). **A PPMd match distance above 4 MiB** is
   reduced modulo the window size without complaint.
4. **Its provision for several filters on one block never takes
   effect.** The test that decides whether the next queued filter
   applies to the same block compares its start with a value that has
   just been overwritten by a sentinel, so it is never true; the next
   step then finds a filter starting before the end of the block just
   processed and fails. A crafted stream with two filters on one block
   fails in `unar` (and in `bsdtar`), vector E17; this specification
   rejects it when the second record is read.
5. **A second "discard all programs" record fails.** The count of
   stored programs is taken before the discard and compared afterwards,
   so the program the record itself transmits is not recognised as new
   and a lookup in the emptied store fails. (`bsdtar` handles the same
   stream as section 11.3 predicts; vectors V6d and V6e.)
6. **A filter block of length 0 ends the output**: the empty block is
   returned as an end-of-data condition, and the rest of the member is
   lost.
7. **The filter block length is not checked against the filter
   machine's memory** before the block is copied into it: a record can
   declare a block of up to 4 GiB for a 256 KiB memory. (Stated from the
   source; not tried.)
8. **Filters queued but not started when a member ends stay queued** in
   a solid group and are applied, at their position in the group, to the
   output of a later member. This is not a fault and this specification
   keeps it (section 11.4, vector V6f). The fault is that the start of
   the member, from which the file offset is counted, moves only when an
   end marker is decoded: after a member whose marker is missing or
   late, every later filter of the group gets a wrong offset. Section
   12.4 stops the group at such a member.
9. **PPMd escape sequences are not checked for errors.** A failure of
   the model while the bytes of a filter record or a match are being
   decoded is used as the value −1.
10. **The whole group is decoded with the method of its first member.**
    The unpack version of later members is not looked at, and a stored
    member inside a group is not skipped properly: on the RAR 6.24
    archive of section 12.2 `unar` extracts the stored members but fails
    on every compressed member after the first stored one.
11. **Look-ahead past the end of the packed bytes sees zeros**; only
    consuming such bits fails. A truncated stream can therefore decode
    one or more spurious symbols from padding before the failure is
    noticed. Harmless as long as nothing is delivered for a failed
    member.
12. **Runs past the end of the length table or of the pre-code are
    accepted** and cut off, and **incomplete codes are accepted**. This
    specification keeps both (sections 4.2, 7.2, 7.3).
13. **A CRC-32 mismatch is reported but the bytes are written** to the
    output file all the same.
14. **Method byte and version are barely checked**: any method byte
    other than `0x30` is decoded.
15. **A PPMd block header that keeps the model is not checked for a
    model existing.** The range decoder is restarted and bytes are then
    decoded with whatever the never-initialised model structure
    contains. Vector E15 fails in `unar` only because that happens to
    run out of input.
16. **A reset while a filter block is partly produced** fails in `unar`
    for the reason of gap 5, before anything else can happen. Without
    that fault the code would go on to run the entry it had been working
    on although the reset has dropped it from the queue, and would then
    remove the newly queued filter in its place. `bsdtar` applies the
    first filter to the partly produced block and then fails.
17. **A filter that returns a different length than its block** has
    that many bytes delivered in place of the block, so everything after
    it is displaced within the file, while positions are still counted
    as if the length were unchanged.
18. **The record body is copied to a buffer on the call stack** sized by
    the record's own length field (up to 65,535 bytes), and the stored
    program number is checked against one more than the size of two
    fixed 1,024-entry tables it indexes (`rar29-filters.md` covers the
    record's contents).

## 17. Decisions

| # | Section | Decision |
|--:|---|---|
| L1 | 2 | Only unpack versions 29 and 36 with method `0x31`..`0x35` reach this decoder. |
| L2 | 2, 10 | The window is always 4 MiB; the dictionary size declared in the header is not enforced. |
| L3 | 2 | The container layer verifies the CRC-32 of every member; a mismatch is an error for the member. |
| L4 | 3 | Padding bits before a block header and everything after the end marker are not checked. |
| L5 | 4.2 | Incomplete codes are accepted; using an unassigned pattern is corrupt input. |
| L6 | 7.2, 7.3 | A run that passes the end of the pre-code lengths or of the length table is cut off, not rejected. |
| L7 | 8.3 | A register match that selects an unset register is corrupt input. |
| L8 | 8.4 | Symbol 258 with no previous match does nothing. |
| L9 | 10 | A distance greater than the LZ position, or greater than 4,194,304, is corrupt input. |
| L10 | 11.1 | A filter block longer than 245,760 bytes is corrupt input (`rar29-filters.md`, its decision F1). |
| L11 | 11.2 | A filter block must not start before the end of the block queued before it; the check is made when the record is read. The queue holds at most 65,536 entries. (F6 and F7 there.) |
| L11a | 11.2 | A reset empties the whole queue, including an entry whose block is partly produced; that block is delivered unfiltered. |
| L12 | 11.1 | A filter record with block length 0 is corrupt input (F1 there). |
| L13 | 11.2 | A second filter for a range that already has one is corrupt input: filters are never run one after another on the same block. |
| L14 | 11.3 | A filter whose output length differs from its block length is corrupt input. |
| L15 | 11.4 | Queue entries not started when a member ends stay queued for the following members of the group and are discarded when the group ends; a block in progress when a member ends is corrupt input. (F8 there.) |
| L16 | 12.1 | A match longer than the member's missing bytes is corrupt input. |
| L17 | 12.1 | A member whose U bytes are complete but which has no end marker is delivered; its group cannot be continued. |
| L18 | 12.2 | A continuation member with P = 0 and U = 0 changes nothing. |
| L19 | 12.2 | A stored member inside a group is transparent to the decoder state. (No longer only a decision: observed in an archive written by RAR 6.24. Kept in this list because the header-pending flag crossing a stored member while clear is inferred.) |
| L20 | 12.2 | A solid-bit member whose predecessor state is of another method or absent is an error, not decoded. |
| L21 | 12.4 | After a failed member, later members of the same group are reported failed without decoding. A CRC-32 mismatch alone does not poison the group. |

None of them changes how a genuine, undamaged archive in the survey is
read. (L11a is numbered so to keep the other numbers stable.)

## 18. Open questions

They do not block an implementation; the document says what to do in
each case.

1. **Unpack version 36.** No real member with this version was found
   (all 1,422 surveyed members carry 29; RAR 6.24 in RAR4 mode writes
   29). The reference implementation treats it exactly like 29 and
   `unar` decodes a crafted one. Whether real version-36 streams differ
   in any way is unconfirmed.
2. **Several filters on one block** (L13): the reference
   implementation shows that the format's designers provided for a
   second filter working on the output of the first when both name
   exactly the same range, but its own defect and `bsdtar`'s rejection
   make such streams unreadable by both tools, and none of the 3,134
   real records does it. This specification rejects them. If a genuine
   archive of that kind is ever found, it will fail with corrupt input
   rather than decode wrongly.
3. **Filters still queued at a member boundary** (L15), **a reset while
   filters are queued** (L11a) and **filter records or block switches
   between the last byte and the end marker**: not observed in 1,419
   real members.
4. **Members without an end marker** (L17): not observed. If archives
   from very early RAR 2.9x versions omit it, L17 delivers their last
   member but not the following members of a solid group.
5. **Continuation members with P = 0** (L18): a compressed member of
   this kind was not observed in archives written by RAR. **Stored
   members inside a solid group** (L19) were observed once, with RAR
   6.24 (section 12.2); whether older RAR versions write them the same
   way is unconfirmed, and no real case of a stored member following an
   end marker that cleared the header-pending flag was seen.
6. **PPMd escape codes 6..255**: both tools treat them like code 1; real
   streams do not contain them.
7. **Largest match length.** The format allows 260 (vector V3); the
   longest in the survey is 257. Nothing depends on it.
8. **Matches crossing a member boundary** (L16) and **references before
   the start of a group** (L9): not observed. The behaviour of the tools
   is described in section 16.

## 19. Evidence

**Reference implementation.** XADMaster, revision `7cb9ee0a`: the RAR 3
decompression handle, the shared sliding-window, prefix-code and
bit-reader helpers, the filter and virtual-machine glue as far as it
places and runs filters, the entry points of the PPMd variant H model,
and the parts of the RAR archive parser that group files into solid
streams and concatenate the pieces of split files. Kept outside this
repository.

**Vendor documents.** `technote.txt` as shipped with RAR 3.93 and 4.20
(file header fields, flag bits, dictionary size codes).

**Independent re-statement.** Everything in sections 3 to 12 was
re-implemented as a throw-away experimental decoder (kept outside the
repository, not part of any deliverable) from the rules as worded here.
For PPMd blocks and for running filter programs it used the throw-away
implementations made the same way for `rar29-ppmd.md` and
`rar29-filters.md`. The figures below come from running it.

**Second, independent check.** A second member of the specification
team, before reading the reference implementation, wrote another
throw-away decoder from this document alone (taking the length,
distance and two-byte tables directly from the tables printed in
section 8) and ran it on every archive described below and on every
test vector. It reproduced all results: the same 1,058 public and 361
purpose-written members decode with matching CRC-32, and its counts of
table transmissions, keep flags, PPMd headers of each kind, escape
codes, filter records, low-distance symbol 16 and end markers of both
kinds equal the figures quoted in this document. The document was then
compared clause by clause with the reference implementation. Vectors
V6c to V6f, V9, V10 and E17, the stored-member observation of section
12.2 and gaps 15 to 18 of section 16 were added in that pass. In the
same pass section 11 was brought into line with `rar29-filters.md`,
which states the queueing rules too: a block length of 0 and a second
filter on one range are corrupt input, the queue holds up to 65,536
entries, and filters whose blocks lie beyond the end of a member stay
queued for the next member (the first version of this document dropped
them and allowed two filters on one block). The second decoder was
changed accordingly and run again on all archives and vectors with the
same results.

**Public archives.** 458 files with the extension `.rar` sampled from
the public collection at `files.scene.org` (demos, music, graphics and
disk magazines, 1995 to 2026); 411 are RAR4-format archives. Among their
members:

- 1,061 compressed members of unpack version 29; 1,058 decode to bytes
  matching the header CRC-32. The three exceptions are single volumes of
  multi-volume sets fetched without their neighbours (the first piece of
  a file is missing). 5,509 members of version 20 and 13 of version 15
  were left to their own decoders; 518 stored members.
- 9,120 LZ table transmissions, 7,184 with the keep flag set; 146 PPMd
  block headers (62 with a new model and a new escape value, 40 with a
  new model only, 41 keeping both, 3 with a new escape value only); 58
  PPMd blocks followed by an LZ block in the same member.
- 101.7 million literals, 2.94 million explicit matches, 864,000
  two-byte matches, 532,751 register matches (247,990, 138,025, 83,771
  and 62,965 for registers 0 to 3), 107,270 uses of symbol 258; all 60
  distance slots; low-distance symbol 16 46 times with 690 matches using
  the counter; largest distance 4,160,732; largest match length 257.
- 2,293 filter records (70 of them through PPMd escape code 3), up to
  28 queued at once, announced up to 1,703,678 bytes ahead; largest
  block 65,536.
- 12 continuation members of solid groups.

**Archives written for this document.** The vendor's command-line RAR
6.24 for macOS (the file `rarmacos-arm-624.tar.gz` from
`www.rarlab.com`), run purely as an archive creator in RAR4 mode (switch
`-ma4`), produced 38 archive sets (60 files) from 16 test files (text,
executable code, a 16-bit stereo sound file, a 24-bit picture, a table
of binary records, random bytes, zeros, files of 0, 1 and 18 bytes, and
two files of 11.5 and 4.8 MB with repetitions about 4 MB apart):
compression levels 1 to 5, each with and without solid mode; all seven
dictionary sizes; forced text compression (PPMd) with several orders and
memory sizes; filters disabled and filters forced; three multi-volume
sets; solid groups restarted by extension and by count.

- 361 compressed members, all of version 29, **all** decoding to bytes
  matching the header CRC-32; 213 of them continuation members of solid
  groups, 47 of which start without a block header.
- 55 members with U = 0 and 1, 2 or 8 packed bytes (section 12.2), 53 of
  them inside a group and 2 at its start.
- 2,718 LZ table transmissions (1,976 with the keep flag, 4 of those
  directly after a PPMd block, 1 as the first block of a continuation
  member); 42 PPMd block headers of all four kinds; 12 PPMd blocks
  followed by an LZ block.
- 841 filter records of five different programs, up to 37 queued at
  once; filters in continuation members that reuse a program
  transmitted by an earlier member, and filters whose result depends on
  the file offset in such members.
- 13 members split across volumes into up to 5 pieces.
- Largest distance 4,193,665 (a 4 MiB dictionary, with the window
  wrapped several times).
- One further archive, made in the second check with the same program
  (switches `-ma4 -s -m3` and `-ms` naming an extension to be stored):
  four compressed members and two stored ones in one solid group
  (section 12.2). It is not included in the counts above.

**In both sets together** (1,419 decoded members): every member ends
with an end marker decoded immediately after its last byte; every LZ
stream ends 0 to 7 bits before the end of its packed bytes and those
bits are zero; every padding before a block header is zero. Never
observed: an over-subscribed code, a non-empty incomplete code, a
pre-code length of 15, a run passing the end of the pre-code or of the
length table, symbol 16 or 17 at position 0, a distance beyond the
output so far or beyond the declared dictionary size, a register match
on an unset register, symbol 258 before any match, a match crossing a
member boundary, overlapping, stacked or zero-length filter blocks, a
filter changing the length of its block, a filter queued across a member
boundary, a second reset of the stored programs in one group, a stored
member inside a solid group, a PPMd escape code above 5, unpack version
36.

**Crafted archives given to `unar` 1.10.7 and `bsdtar` 3.5.3** (each
compared with the output predicted by this document; identical for
`unar` in every case where the document does not deliberately differ,
sections 16 and 17): the vectors of section 14; explicit-match and
register-match lengths at every extra-bit boundary from 2 to 260; every
two-byte distance range boundary; distances at and around 8191, 8192,
8193, 262143, 262144 and 262145, each followed by register matches and
a repeat, to fix the length adjustments; the order of the registers
under every kind of match; the low-distance code with symbol 16 as the
first symbol of a block, with the counter running out, with small-slot
matches in between, across a table transmission and across a member
boundary; code lengths 1 to 15 in the pre-code; pre-code symbols 16 to
19 including runs past the end; the keep flag within a member, after a
PPMd block and across members; non-zero padding before a block header;
single-symbol, incomplete and over-subscribed codes; end markers before,
at and after U and missing altogether; filter records at several
positions, with and without the 258 flag, reusing program and length,
overlapping, out of order, stacked, of length 0, past the member end,
with a second reset; PPMd blocks with every escape code, with filter
records, between LZ blocks, with kept model and escape value across
members; distances up to 4,194,304 after the window has wrapped; solid
groups with and without the archive-level flag; empty members with and
without packed bytes at the start and in the middle of a group; unpack
versions 20, 26, 29, 30, 35, 36 and 40; method bytes `0x31`, `0x35` and
`0x36`.

## Clean-room feedback

Notes from the implementation side (clean room). They record where the
documents disagree, how the disagreement was resolved, and what was
observed on real archives; prose only, not written from any decoder's
source.

1. **Section 11.4 against `rar29-filters.md` section 8: queued filters
   at the end of a member.** This document drops queue entries whose
   block has not started when a member ends; the filter document keeps
   them queued for the following members of the group and reports that
   `unar` applies such a filter there. Vector V6f of this very document
   (14.3) requires the second reading. Implemented: **the entries stay
   queued**; they are dropped when the group ends. A block that has
   started but is not complete when its member ends is corrupt input in
   both documents.
2. **Section 11.2 against `rar29-filters.md` sections 5.3 and 7.1:
   block length 0 and queue size.** A record whose block length is 0 is
   parsed and not queued (this document), not rejected (the filter
   document). The queue limit is 65,536 entries (the filter document).
3. **Programs other than the six standard ones are not executed.** The
   implementation recognises the standard programs by their fingerprint
   and computes them directly; it has no virtual machine. A block whose
   program is not a standard one is delivered unfiltered, which the
   CRC-32 check of the member then reports. No such program occurs in
   the archives decoded so far.
4. **Real-data evidence for section 12.2 (the per-file solid bit) and
   for PPMd blocks.** 54 different RAR 2.9 to 4.x archives fetched from
   public collections (all unpack version 29) were decoded and compared
   file by file, by size and MD5, with what `unar` extracts: 52 are
   identical, one is encrypted and is skipped, one has a damaged archive
   header and is refused by `unar` as well. Eight of them are solid,
   with 60,170 members between them; in each the first member lacks the
   bit and every other member carries it. Nine contain members whose
   stream starts with a PPMd block (42,396 such members, most of them in
   the solid archives, where the model is carried from member to
   member). Files longer than the 4 MiB window (up to 9.8 MiB) are among
   them.
5. **Ratio of unpacked to packed size.** Section 15 gives no usable
   bound and none is enforced by the decoder. The container's own limit
   (1,000 to 1) is applied to a solid group as a whole, not to each
   member: a member that repeats earlier members of its group
   legitimately packs to a few hundred bytes (seen: 2.2 MiB from about
   1 KiB).

### Notes from the native implementation (`rar_native/rar29.rs`)

Items 2 and 3 above describe an earlier reader and no longer hold: the
filter layer now rejects a block length of 0, as `rar29-filters.md`
decides, and runs programs other than the six standard ones.

6. **Section 12.1, "decoding fails in any way" once the member count
   equals U.** Read literally this also covers a filter record that the
   filter layer rejects, and a filter whose run fails. The filter layer
   does not undo a rejected record (a record that orders a reset has
   already emptied the store and the queue), and a failed run leaves its
   block unfiltered. Chosen: both are corrupt input for the member, also
   when its U bytes are complete; only failures of the LZ and PPMd
   layers and of block headers turn into "no end marker". The document
   could say so.
7. **A member without an end marker and section 11.4.** The document
   does not say whether the check for a block in progress applies to a
   member that ends without its marker. Chosen: it does; such a member
   is corrupt input, not delivered.
8. **U = 0 with packed bytes that do not decode.** By 12.1 the member
   count equals U from the start, so a damaged block header gives an
   empty member and an unusable group, not corrupt input. Implemented as
   written; noted because it is the one case where arbitrary bytes are
   "delivered".
9. **A member that starts with the header-pending flag clear** when the
   group's block type is not LZ (cannot arise from a decoded stream) is
   corrupt input, like the missing codes of 12.3.
10. **P = 0 with U above 0 in a continuation member** and **U above the
    caller's output limit** are refused before decoding; whether the
    group survives is not stated. Chosen: the group is unusable
    afterwards, because the member's contribution to the window is
    missing.
11. **A member with the solid bit handed to a decoder that has no group
    state** is reported as unsupported ("without its predecessor"), the
    data possibly being intact; after a failed member the report is
    corrupt input ("depends on a damaged member").
12. **Order of checks that cannot be observed**: for symbols 259..262 the
    length symbol is read before the unset register is rejected; for
    pre-code symbols 16 and 17 at position 0 the extra bits are not
    read.
13. **Window memory.** The window is obtained in steps (64 KiB doubling
    up to 4 MiB) as the group's output grows, and is not cleared between
    groups; the distance check of section 10 keeps stale bytes
    unreachable. The member is returned whole, so the hold-back area of
    section 15 item 1 is not needed: a filter's result overwrites its
    block in the member's output.
14. **Real data.** Four RAR 3.x/4.x archives found on the implementer's
    machine (202 compressed members of version 29, 12 to 81 MB per
    archive, none solid) decode with matching CRC-32. No local archive
    exercises PPMd blocks, solid groups or split members; those rest on
    the vectors of section 14, all of which pass, including V5, V6c to
    V6f and V7a to V7d.
