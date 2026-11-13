# RAR 2.0 compression (unpack versions 20 and 26)

Format specification for clean-room implementation. This document describes
the compression method used by RAR 2.x for the files of a RAR archive whose
file header carries **unpack version 20 or 26**: a 1 MiB sliding-window
LZ77 coder with Huffman-coded symbols, which can switch, block by block, to
a "multimedia" mode that codes bytes as prediction errors of an adaptive
linear predictor over one to four interleaved channels.

It does not describe the archive container (signatures, block headers,
volumes), the RAR 1.5 method (unpack version 15), the RAR 2.9/3.x method
(unpack versions 29 and 36), RAR5, or encryption.

Contents: 1 how to read this document; 2 what the decoder is given and
returns; 3 the bit stream; 4 prefix codes; 5 decoder state; 6 reading
tables; 7 LZ mode; 8 multimedia mode; 9 the window; 10 members, solid
groups, volumes and end of data; 11 worked examples; 12 test vectors; 13
safety requirements; 14 observed implementation gaps; 15 decisions; 16
open questions; 17 evidence.

## 1. How to read this document

### 1.1 Provenance

- This is a **prose specification**. It was derived by studying the
  behaviour of an existing open decoder (the RAR 2.0 support of The
  Unarchiver's XADMaster library, LGPL, revision `7cb9ee0a` of
  `github.com/MacPaw/XADMaster`; called the *reference implementation*),
  by black-box testing of the extraction tool built from that library
  (`unar`/`lsar` 1.10.7) with purpose-built archives, and by measurements
  on real archives.
- The official `unrar` source was **not** consulted; its licence forbids
  using it to recreate the algorithm.
- The work is a **clean-room process**. The author of this document could
  read the reference implementation; the implementation is written **from
  this document only**, by implementers who do not read the source of any
  RAR decoder. The document therefore contains no program code and no
  pseudo-code, only prose, tables of constants, worked examples and test
  vectors.
- Where the reference implementation is wrong or unsafe the document says
  so (section 14) and specifies the correct behaviour instead. Being
  compatible with the reference implementation is not a goal; reading
  genuine archives correctly and rejecting hostile input safely is.

### 1.2 Evidence labels

| Label | Meaning |
|---|---|
| CONFIRMED (real data) | The rule was exercised when decoding real archives written by RAR 2.x, and every decoded file matched the CRC-32 stored in its header (section 17). |
| CONFIRMED (crafted) | Checked by giving `unar` a purpose-built archive and comparing its output with the output predicted by this document. |
| REFERENCE-ONLY | Stated by the reference implementation; not separately observable. |
| INFERRED | Deduced; not confirmed. |
| DECISION | A choice made by this specification, not a fact about the format (section 15). |

A rule without a label in sections 3 to 9 is CONFIRMED (real data).

### 1.3 Terms and numbers

| Term | Meaning |
|---|---|
| member | One file stored in the archive, after the container layer has joined the pieces of a file that is split across volumes. |
| packed bytes | The data area of a member as stored (all pieces concatenated). **P** is their number. |
| **U** | The unpacked size the member's header declares. Untrusted. |
| solid group | A maximal run of members that share one decoder state (section 10.2). A member of an archive that is not solid is a group of one. |
| item | One decoding step in LZ mode: a literal, a match, or a table switch. |
| match | An instruction to copy *length* bytes starting *distance* bytes back in the output. |
| pre-code | The 19-symbol prefix code used only to transmit code lengths. |
| length table | The array of 1028 code lengths the decoder keeps between table transmissions. |
| corrupt input | The decoder's error result. No contents are delivered for the member. |

Numbers are decimal unless prefixed `0x`. Byte strings are written as
hexadecimal digits in code style (`0a ac`). Ranges written `a..b` include
both ends. "Bit n" of a stream counts from 0 in reading order (section 3).

## 2. What the decoder is given and what it returns

The container layer hands the decoder, for each member whose unpack
version is 20 or 26 and whose method byte is `0x31`..`0x35`:

- the packed bytes of the member (P bytes; the pieces of a split file
  already concatenated in volume order, section 10.4);
- U;
- whether the member **continues the solid group** of the preceding
  member (file header flag bit `0x0010`, section 10.2);
- the CRC-32 stored in the header of the member's last piece.

It returns exactly U bytes, or the error "corrupt input".

Header facts the container layer needs (all CONFIRMED (real data) unless
noted):

- **Unpack version** is the byte at offset 24 of the file header block
  (counting the block's CRC field as offset 0), **method** the byte at
  offset 25. Method `0x30` means stored: the packed bytes are the
  unpacked bytes and this decoder is not involved. Methods `0x31`..`0x35`
  are compression levels of the *encoder*; the decoder ignores the value.
  `unar` also decodes method `0x36` identically (CONFIRMED (crafted));
  DECISION: accept `0x31`..`0x35` only.
- Versions 20 and 26 select this decoder; there is **no difference**
  between them in the stream format (REFERENCE-ONLY; `unar` decodes a
  crafted version-26 member identically, CONFIRMED (crafted); no real
  version-26 archive was found, section 16). Version 15 and versions 29
  and 36 are other methods. Any other value: `unar` decodes nothing
  useful (a crafted version 21 gives a checksum failure); DECISION: only
  20 and 26 are handled by this decoder.
- File header flag bit `0x0004` marks an **encrypted** member. The
  packed bytes are then enciphered and must be deciphered before this
  decoder sees them. The cipher is out of scope; DECISION: report such a
  member as "encrypted, unsupported" and treat it like a failed member
  for the purposes of section 10.3.
- File header flag bits `0x00e0` give the **dictionary size** the
  encoder used: value 0 = 64 KiB, 1 = 128 KiB, 2 = 256 KiB, 3 = 512 KiB,
  4 = 1024 KiB; value 7 marks a directory entry (no data). In the real
  archives surveyed the values 0, 2, 3 and 4 occur, and no match
  distance exceeds the declared size. The decoder does not need the
  value: it always uses a 1 MiB window (section 9). DECISION: the
  declared dictionary size is not enforced.
- The **CRC-32** is the common one (polynomial `0xEDB88320` reflected,
  initial value and final XOR `0xFFFFFFFF`; the ASCII string `123456789`
  gives `0xCBF43926`), computed over the member's U unpacked bytes. For a
  file split across volumes, the header of each piece except the last
  holds instead the CRC-32 of *that piece's packed bytes*; the header of
  the last piece holds the CRC-32 of the whole unpacked file.
- DECISION: after decoding, the container layer compares the CRC-32 of
  the U bytes with the stored value and reports a mismatch as an error
  for that member ("checksum mismatch"); the bytes are not delivered as
  good contents. A mismatch does not by itself invalidate the solid
  group's state (the next member may still decode correctly), but see
  section 10.3.

## 3. The bit stream

- The packed bytes of one member form one bit stream. Bits are taken
  from each byte starting with the **most significant bit**. Bit 0 of
  the stream is the top bit of the first byte.
- A field of n bits is read as n successive bits; the **first bit read
  is the most significant** bit of the field's value. Fields in this
  format are 1 to 16 bits wide. A field of 0 bits reads nothing and has
  the value 0.
- There is no byte alignment anywhere inside a member's stream: table
  transmissions and items follow one another bit by bit.
- **Reading a bit that lies beyond the last packed byte is corrupt
  input.** (The reference implementation lets a decoder *look ahead*
  past the end, seeing zero bits, and fails only when such a bit is
  consumed. An implementation that decodes prefix codes by looking ahead
  a fixed number of bits must behave the same way: pad the look-ahead
  with zero bits, and fail if the bits actually consumed extend past the
  end.) CONFIRMED (crafted).
- The stream of a member ends without any marker. After the item that
  completes the member's U bytes, between 0 and 7 unused bits remain in
  the last byte; in every real member examined they are zero. DECISION:
  whatever follows the item that completes U bytes (unused bits, or any
  number of further bytes) is neither read nor checked.

Each member has **its own bit stream starting at bit 0 of its own packed
bytes**, also inside a solid group (section 10.2).

## 4. Prefix codes

All Huffman codes in the format (the pre-code, the three LZ codes, the
one to four channel codes) are built the same way from an array of code
lengths, one per symbol, each length 0..15. Length 0 means the symbol
does not occur and has no code.

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

Example: lengths 2, 2, 2, 2 for the symbols 97, 98, 99 and 270 (all others
0) give 97 = `00`, 98 = `01`, 99 = `10`, 270 = `11`. Lengths 1, 2, 2 for
symbols 1, 255, 256 give 1 = `0`, 255 = `10`, 256 = `11`. Lengths 1, 2, 3,
3 for symbols 5, 6, 7, 8 give 5 = `0`, 6 = `10`, 7 = `110`, 8 = `111`.

### 4.2 Codes that are not complete, and codes that are impossible

Let the *weight* of a code be the sum, over the symbols with a non-zero
length L, of 2 raised to the power (15 − L). A complete code has weight
32768.

- **Weight above 32768 (over-subscribed)**: the lengths cannot form a
  prefix code. This is corrupt input **as soon as the table is read**,
  whether or not the code would ever be used. (In the assignment
  procedure it shows as the running value reaching 2 to the power L
  while a symbol of length L still needs a code.) CONFIRMED (crafted);
  never seen in real data.
- **Weight below 32768 (incomplete)**: accepted. This is normal in real
  data in one form: a code with **no symbols at all** (the length code
  in 561 of about 6,200 real table transmissions, the distance code in
  19: the block simply contains no item that would use it). It is also
  accepted for a code with a **single symbol**, whose length is then 1
  and whose code is the single bit `0` (CONFIRMED (crafted)).
- When, while decoding a symbol, the bits read match no assigned code
  (always the case for an empty code; the bit `1` for a single-symbol
  code; any unassigned pattern of an incomplete code), the result is
  corrupt input. CONFIRMED (crafted).

A decoder never needs more than 15 bits to decode one symbol.

## 5. Decoder state

One decoder state exists per solid group. Everything in it is set to the
initial value when a group starts (section 10.2) and is carried from
member to member inside the group.

| Item | Contents | Initial value |
|---|---|---|
| window | 1,048,576 bytes of most recent output (section 9) | empty (see 9) |
| total output count | number of bytes produced in this group so far (a 64-bit count) | 0 |
| last distance, last length | distance and final length of the most recent match | none |
| distance history | the distances of the four most recent matches, most recent first | empty |
| length table | 1028 code lengths, each 0..15 | all 0 |
| mode | LZ or multimedia | set by the first table transmission |
| current codes | LZ mode: main, distance and length code. Multimedia mode: one code per channel | set by the first table transmission |
| channel count | 1..4 (multimedia mode) | set by a multimedia table transmission |
| current channel | 0..3 | 0 |
| channel delta | a signed value shared by all channels (section 8) | 0 |
| four channel predictors | per channel: five weights, four difference terms, last delta, last byte, a sample counter, eleven error sums (section 8) | all 0 |

Not part of the carried state: the position in the bit stream. Each
member starts reading at bit 0 of its own packed bytes.

Nothing in the state is reset by a table transmission, by a change of
mode or by the start of a further member of the same group, except as
stated in section 6 (the length table when the keep flag is 0; the
current channel when it is out of range). In particular the predictors
of all four channels, the channel delta, the distance history and the
last distance and length **survive mode changes and table changes**.

## 6. Reading tables

### 6.1 When tables are read

A table transmission is read

1. at the very start of the stream of the **first member of a solid
   group**, before anything else (even when that member has U = 0,
   section 10.2); and
2. whenever the **table-switch symbol** is decoded: symbol 269 of the
   main code in LZ mode, symbol 256 of the current channel's code in
   multimedia mode. The transmission follows that symbol immediately.

Nothing else triggers one. In particular the start of a later member of
a solid group does not: its stream begins directly with an item or
sample coded with the codes in force at the end of the previous member.
(The encoder often makes that first symbol a table switch: 847 of the
roughly 2,000 solid continuation members surveyed begin with symbol 269.
To the decoder this is an ordinary table switch.) There is no limit on how many
transmissions a member contains; real encoders emit a new one every few
thousand to few tens of thousands of output bytes (the 133 MB real file
of section 17 contains 4,398).

### 6.2 Layout

In reading order:

| Bits | Field | Meaning |
|--:|---|---|
| 1 | multimedia flag | 1: the tables that follow are channel codes and the mode becomes multimedia. 0: they are the three LZ codes and the mode becomes LZ. |
| 1 | keep flag | 0: set all 1028 entries of the length table to 0 before reading the new lengths. 1: keep the table as it is; the new lengths are coded as differences from it. |
| 2 | channel count − 1 | **Present only when the multimedia flag is 1.** The channel count is the value plus 1 (1..4). |
| 19 × 4 | pre-code lengths | The code lengths (0..15) of pre-code symbols 0, 1, ..., 18, in that order. |
| variable | coded lengths | N entries of the length table, coded with the pre-code (section 6.3). |

N is **374** in LZ mode and **257 × channel count** in multimedia mode
(257, 514, 771 or 1028).

Immediately after the channel count is read: **if the current channel is
greater than or equal to the new channel count, the current channel
becomes 0**; otherwise it is left unchanged. (CONFIRMED (real data):
transitions from 3 or 4 channels to fewer, with the current channel at
1, 2 or 3 at the switch, occur in the survey; CONFIRMED (crafted).)

The pre-code is built from its 19 lengths as in section 4. An
over-subscribed pre-code is corrupt input. An incomplete pre-code is
accepted (real encoders always send a complete one; `unar` accepts an
incomplete one, CONFIRMED (crafted)).

### 6.3 The coded lengths

The decoder fills positions 0, 1, ..., N − 1 of the length table, in
order. Let i be the next position to fill. Until i reaches N, decode one
pre-code symbol and act on it:

| Pre-code symbol | Extra bits read after it | Action |
|--:|--:|---|
| 0..15 | none | Position i receives (its present value + the symbol) modulo 16. i advances by 1. |
| 16 | 2, value r | The value now stored at position i − 1 is copied into the next r + 3 positions (3..6 positions). **If i is 0 this is corrupt input.** |
| 17 | 3, value r | The next r + 3 positions (3..10) are set to 0. |
| 18 | 7, value r | The next r + 11 positions (11..138) are set to 0. |

Details:

- "Its present value" is whatever the length table holds at that
  position: 0 if the keep flag was 0, otherwise the length left there by
  the previous transmission **regardless of which mode that transmission
  was for**. The length table is one array shared by both modes; an LZ
  transmission writes positions 0..373, a multimedia transmission
  positions 0..(257 × channels − 1). Positions at or beyond N are not
  touched by the transmission and keep their values for a possible later
  transmission with the keep flag set. (Real encoders set the keep flag
  only when the mode and the channel count are unchanged, and never on
  the first transmission of a group; the general rule is REFERENCE-ONLY
  and CONFIRMED (crafted) for LZ followed by multimedia.)
- Symbols 17 and 18 store 0 outright and symbol 16 stores a copy; the
  "add modulo 16" rule applies to symbols 0..15 only.
- For symbol 16, "the position before" is simply position i − 1 of the
  array, also when that position belongs to a different alphabet (for
  example the last main-code length when i is 298, or the last length of
  the previous channel).
- A run (symbols 16, 17, 18) that would go past position N − 1 is
  **cut off at N − 1**; the excess is ignored and the transmission is
  complete. Never seen in about 6,200 real transmissions; `unar` accepts
  it (CONFIRMED (crafted)). DECISION: accept and cut off.
- The transmission ends when i reaches N; no terminator follows.

### 6.4 Building the codes

After the lengths are read, the codes of the new mode are built from the
length table as in section 4. An over-subscribed code is corrupt input.

LZ mode (N = 374):

| Code | Length-table positions | Symbols |
|---|---|--:|
| main code | 0..297 | 298 |
| distance code | 298..345 | 48 |
| length code | 346..373 | 28 |

Multimedia mode: the code of channel c (0-based) is built from positions
257 × c .. 257 × c + 256 (257 symbols). Only the codes of channels
0..channel count − 1 exist.

The codes of the other mode cease to exist: after a multimedia
transmission there is no main code, and so on.

## 7. LZ mode

Repeat until the member's U bytes are complete (section 10.1): decode one
symbol with the **main code** and act on it.

| Main symbol | Kind | What follows in the stream | Effect |
|--:|---|---|---|
| 0..255 | literal | nothing | Output the byte whose value is the symbol. |
| 256 | repeat last match | nothing | A match with the last distance and the last length. |
| 257..260 | match with a recent distance | a length-code symbol, then its extra bits | Section 7.2. |
| 261..268 | two-byte match with a short distance | extra bits | Section 7.3. |
| 269 | table switch | a table transmission (section 6) | No output. |
| 270..297 | match | length extra bits, then a distance-code symbol, then its extra bits | Section 7.1. |

Every match of every kind, **including symbols 256 and 257..260**, ends
by updating the match memory (section 7.4).

### 7.1 Symbols 270..297: match with an explicit distance

1. The **length slot** is the symbol minus 270 (0..27). Read the slot's
   extra bits (table below). The length so far is base + extra + **3**.
2. Decode one symbol with the **distance code**: the distance slot
   (0..47). Read that slot's extra bits (second table below). The
   distance is base + extra + **1**.
3. Adjust the length by the distance: add 1 if the distance is 8,192
   (`0x2000`) or more, and add 1 more if it is 262,144 (`0x40000`) or
   more.

The resulting length is 3..260 and the distance 1..1,048,576.

Length slots (used for main symbols 270..297 and, in section 7.2, for
the symbols of the length code):

| Slot | Main symbol | Base | Extra bits | Base + extra |
|--:|--:|--:|--:|--:|
| 0 | 270 | 0 | 0 | 0 |
| 1 | 271 | 1 | 0 | 1 |
| 2 | 272 | 2 | 0 | 2 |
| 3 | 273 | 3 | 0 | 3 |
| 4 | 274 | 4 | 0 | 4 |
| 5 | 275 | 5 | 0 | 5 |
| 6 | 276 | 6 | 0 | 6 |
| 7 | 277 | 7 | 0 | 7 |
| 8 | 278 | 8 | 1 | 8..9 |
| 9 | 279 | 10 | 1 | 10..11 |
| 10 | 280 | 12 | 1 | 12..13 |
| 11 | 281 | 14 | 1 | 14..15 |
| 12 | 282 | 16 | 2 | 16..19 |
| 13 | 283 | 20 | 2 | 20..23 |
| 14 | 284 | 24 | 2 | 24..27 |
| 15 | 285 | 28 | 2 | 28..31 |
| 16 | 286 | 32 | 3 | 32..39 |
| 17 | 287 | 40 | 3 | 40..47 |
| 18 | 288 | 48 | 3 | 48..55 |
| 19 | 289 | 56 | 3 | 56..63 |
| 20 | 290 | 64 | 4 | 64..79 |
| 21 | 291 | 80 | 4 | 80..95 |
| 22 | 292 | 96 | 4 | 96..111 |
| 23 | 293 | 112 | 4 | 112..127 |
| 24 | 294 | 128 | 5 | 128..159 |
| 25 | 295 | 160 | 5 | 160..191 |
| 26 | 296 | 192 | 5 | 192..223 |
| 27 | 297 | 224 | 5 | 224..255 |

Distance slots (symbols of the distance code):

| Slot | Base | Extra bits | Distance (base + extra + 1) |
|--:|--:|--:|--:|
| 0 | 0 | 0 | 1 |
| 1 | 1 | 0 | 2 |
| 2 | 2 | 0 | 3 |
| 3 | 3 | 0 | 4 |
| 4 | 4 | 1 | 5..6 |
| 5 | 6 | 1 | 7..8 |
| 6 | 8 | 2 | 9..12 |
| 7 | 12 | 2 | 13..16 |
| 8 | 16 | 3 | 17..24 |
| 9 | 24 | 3 | 25..32 |
| 10 | 32 | 4 | 33..48 |
| 11 | 48 | 4 | 49..64 |
| 12 | 64 | 5 | 65..96 |
| 13 | 96 | 5 | 97..128 |
| 14 | 128 | 6 | 129..192 |
| 15 | 192 | 6 | 193..256 |
| 16 | 256 | 7 | 257..384 |
| 17 | 384 | 7 | 385..512 |
| 18 | 512 | 8 | 513..768 |
| 19 | 768 | 8 | 769..1024 |
| 20 | 1024 | 9 | 1025..1536 |
| 21 | 1536 | 9 | 1537..2048 |
| 22 | 2048 | 10 | 2049..3072 |
| 23 | 3072 | 10 | 3073..4096 |
| 24 | 4096 | 11 | 4097..6144 |
| 25 | 6144 | 11 | 6145..8192 |
| 26 | 8192 | 12 | 8193..12288 |
| 27 | 12288 | 12 | 12289..16384 |
| 28 | 16384 | 13 | 16385..24576 |
| 29 | 24576 | 13 | 24577..32768 |
| 30 | 32768 | 14 | 32769..49152 |
| 31 | 49152 | 14 | 49153..65536 |
| 32 | 65536 | 15 | 65537..98304 |
| 33 | 98304 | 15 | 98305..131072 |
| 34 | 131072 | 16 | 131073..196608 |
| 35 | 196608 | 16 | 196609..262144 |
| 36 | 262144 | 16 | 262145..327680 |
| 37 | 327680 | 16 | 327681..393216 |
| 38 | 393216 | 16 | 393217..458752 |
| 39 | 458752 | 16 | 458753..524288 |
| 40 | 524288 | 16 | 524289..589824 |
| 41 | 589824 | 16 | 589825..655360 |
| 42 | 655360 | 16 | 655361..720896 |
| 43 | 720896 | 16 | 720897..786432 |
| 44 | 786432 | 16 | 786433..851968 |
| 45 | 851968 | 16 | 851969..917504 |
| 46 | 917504 | 16 | 917505..983040 |
| 47 | 983040 | 16 | 983041..1048576 |

(Slots 0..33 follow the pattern "two slots per extra-bit count"; slots
34..47 all have 16 extra bits and bases 65,536 apart.)

### 7.2 Symbols 257..260: match with a recent distance

1. The distance is taken from the distance history: symbol 257 selects
   the **most recent** match's distance, 258 the second most recent, 259
   the third, 260 the fourth (section 7.4).
2. Decode one symbol with the **length code**: a length slot 0..27. Read
   the slot's extra bits (length-slot table of section 7.1; the "main
   symbol" column does not apply). The length so far is base + extra +
   **2**.
3. Adjust the length by the distance: add 1 if the distance is 257
   (`0x101`) or more, add 1 more if it is 8,192 (`0x2000`) or more, and
   add 1 more if it is 262,144 (`0x40000`) or more.

The resulting length is 2..260. Note the differences from section 7.1:
the minimum is 2 instead of 3, there are three distance thresholds
instead of two, and the length slot comes from a separate code.

### 7.3 Symbols 261..268: two-byte match

The length is 2. The distance is base + extra + 1 from this table:

| Main symbol | Base | Extra bits | Distance |
|--:|--:|--:|--:|
| 261 | 0 | 2 | 1..4 |
| 262 | 4 | 2 | 5..8 |
| 263 | 8 | 3 | 9..16 |
| 264 | 16 | 4 | 17..32 |
| 265 | 32 | 5 | 33..64 |
| 266 | 64 | 6 | 65..128 |
| 267 | 128 | 6 | 129..192 |
| 268 | 192 | 6 | 193..256 |

No length adjustment applies.

### 7.4 Match memory

After the distance and final length of a match are known, and whatever
kind of match it was:

- **last distance** and **last length** become this match's distance and
  final length (the length after the adjustments of 7.1 or 7.2);
- the distance is inserted at the front of the **distance history**; the
  previous entries move back one place and the oldest of four is
  dropped. Nothing is searched or removed: the same distance may occupy
  several places.

Consequences, all CONFIRMED (crafted) and exercised throughout the real
data: after matches with the distances 3, 5, 7, 9 in that order, symbol
257 gives 9, 258 gives 7, 259 gives 5 and 260 gives 3. After matches A
then B, a symbol 256 repeats B and inserts B again, so that 258 now
gives B, not A. A symbol 258 used after A then B copies with distance A
and inserts A, so that the history reads A, B, A.

Symbol 256 uses the last *final* length unchanged; no adjustment is
applied again.

**Missing memory.** DECISION: symbol 256 when no match has been decoded
in this group, and a symbol 257..260 that selects a history place no
match has filled yet, are corrupt input. (Real data never does either;
section 14 describes what the reference implementation does.)

### 7.5 Executing a match

A match of distance d and length n appends n bytes to the output, each
equal to the byte d positions before it in the group's output (so a
distance smaller than the length repeats a pattern: distance 1 repeats
the previous byte n times). Section 9 states the window rules and
section 13 the checks that precede the copy.

## 8. Multimedia mode

In multimedia mode the output is treated as samples of 1 to 4
interleaved channels: byte k of the block belongs to the channel after
the one of byte k − 1, cyclically. (The encoder chooses it for
uncompressed sound and for true-colour pictures; it also turns up inside
module music and executables.) Each output byte is coded as the
difference between a prediction and the byte.

### 8.1 Decoding loop

Repeat until the member's U bytes are complete (section 10.1):

1. Decode one symbol with the code of the **current channel**.
2. If the symbol is **256**: read a table transmission (section 6); the
   mode after it is whatever the transmission says. No byte is output
   and the current channel does not advance (but see the range rule of
   section 6.2).
3. Otherwise the symbol s is 0..255: run the predictor step of section
   8.2 for the current channel, output the resulting byte (it enters the
   window like any other output byte), then advance the current channel
   by 1, wrapping from channel count − 1 to 0.

The current channel is **not** reset at a table switch, at a change of
mode or at the start of a later member of a solid group: a multimedia
block that follows an LZ block continues with the channel that was next
when the previous multimedia block stopped. (CONFIRMED (real data);
CONFIRMED (crafted).) Bytes output in LZ mode do not advance the channel
and do not touch the predictors.

### 8.2 The predictor step

State of one channel (all values are signed integers; a 32-bit signed
type is ample for all of them):

| Name | Count | Range reached |
|---|--:|---|
| weights K1..K5 | 5 | −17..16 |
| difference terms D1..D4 | 4 | D1: −128..127; D2, D3, D4: −255..255 |
| last delta LD | 1 | −128..127 |
| last byte LB | 1 | 0..255 |
| sample counter | 1 | only its value modulo 32 matters |
| error sums E0..E10 | 11 | 0..40,928 |

Shared by all channels: the **channel delta CD** (−128..127).

"As a signed byte" below means: a value 0..255 is mapped to itself when
below 128 and to the value minus 256 otherwise.

For a symbol s (0..255) on the current channel, in this order:

1. **Count.** Add 1 to the sample counter.
2. **Shift the difference terms.** D4 takes the value of D3; D3 takes
   the value of D2; D2 becomes LD − D1 (using the value D1 had before
   this step); D1 becomes LD.
3. **Predict.** Form the sum
   8 × LB + K1 × D1 + K2 × D2 + K3 × D3 + K4 × D4 + K5 × CD,
   using the difference terms as just updated and the channel delta as
   left by the previous sample (of whichever channel). Divide the sum by
   8 **rounding toward negative infinity** (an arithmetic shift right by
   3; −1 gives −1, −9 gives −2), and keep the low 8 bits of the result
   (its value modulo 256, 0..255). This is the predicted byte.
4. **Reconstruct.** The output byte is (predicted byte − s) modulo 256.
5. **Accumulate errors.** Let e be s as a signed byte, multiplied by 8
   (−1024..1016). Add to the error sums the absolute values:
   - E0: |e|
   - E1: |e − D1|, E2: |e + D1|
   - E3: |e − D2|, E4: |e + D2|
   - E5: |e − D3|, E6: |e + D3|
   - E7: |e − D4|, E8: |e + D4|
   - E9: |e − CD|, E10: |e + CD|

   (D1..D4 as updated in step 2; CD still the value from the previous
   sample.)
6. **Remember.** LD becomes (output byte − LB) modulo 256 taken as a
   signed byte, using LB's value before this step. CD becomes the same
   value. LB becomes the output byte.
7. **Adapt, every 32nd sample of this channel.** If the sample counter
   is now a multiple of 32: find the smallest of E0..E10; if several are
   equal the one with the **lowest index** wins. Set all eleven error
   sums to 0. Then, by the winning index:

   | Winner | Change |
   |--:|---|
   | E0 | none |
   | E1 | K1 decreases by 1, if K1 is −16 or greater |
   | E2 | K1 increases by 1, if K1 is less than 16 |
   | E3 | K2 decreases by 1, if K2 is −16 or greater |
   | E4 | K2 increases by 1, if K2 is less than 16 |
   | E5 | K3 decreases by 1, if K3 is −16 or greater |
   | E6 | K3 increases by 1, if K3 is less than 16 |
   | E7 | K4 decreases by 1, if K4 is −16 or greater |
   | E8 | K4 increases by 1, if K4 is less than 16 |
   | E9 | K5 decreases by 1, if K5 is −16 or greater |
   | E10 | K5 increases by 1, if K5 is less than 16 |

   The limits are deliberately **not symmetric**: a weight can reach
   −17 but not 17. (CONFIRMED (real data): weights of −17 and of 16 are
   reached in four of the surveyed archives, whose files all match their
   CRC-32.)

The sample counter, like everything else in the channel, is never reset
inside a group: the 32-sample rhythm of a channel runs across table
switches, LZ blocks and member boundaries.

### 8.3 Remarks

- With all state zero the prediction is LB, so the first byte of a
  channel is (0 − s) modulo 256 and, until a weight changes, each byte
  is (previous byte of the same channel − s) modulo 256.
- CD links the channels: for a channel count above 1 it is the last
  delta of the *previous channel's* most recent sample; with one channel
  it equals that channel's own LD.
- Section 11.3 traces eight samples.

## 9. The window

- The window holds the most recent **1,048,576 bytes** (1 MiB, `0x100000`)
  of the group's output, for both versions 20 and 26, whatever dictionary
  size the header declares. (CONFIRMED (real data): distances up to
  1,046,214 occur; REFERENCE-ONLY for the fixed size.)
- It is used as a ring: the byte with total output count t (0-based)
  lives at ring position t modulo 1,048,576. Every output byte of the
  group goes into it: literals, match copies and multimedia bytes alike.
- A match copies **one byte at a time in increasing order**, reading the
  byte d positions back and writing it at the current position, so that
  source and destination may overlap.
- The largest distance, 1,048,576, equals the ring size. It is valid
  once at least 1,048,576 bytes have been produced; the byte to copy is
  then the one at the ring position about to be overwritten, so a ring of
  exactly 1 MiB suffices provided each byte is **read before the position
  is written**. (CONFIRMED (crafted).)
- **A distance greater than the total output count of the group so far
  is corrupt input** (DECISION; it would refer to data before the start
  of the group. Never seen in real data. Section 14.) For a match the
  check is made once, before copying, against the count at the start of
  the match.
- The window is not cleared and the count is not reset between the
  members of a solid group; both start afresh with each new group.

## 10. Members, solid groups, volumes, end of data

### 10.1 One member

A member is complete when exactly U bytes have been output for it. The
decoder checks for completion **before decoding each symbol**: when the
member's count has reached U it stops, without reading further. There
is no end-of-data symbol and no trailer.

- U = 0: nothing is decoded for the member (but see 10.2 for the first
  member of a group).
- DECISION: a match whose length exceeds the number of bytes still
  missing from the member is corrupt input. (Never seen in about 3,150
  real members. The reference implementation delivers the first part of
  such a match and then loses synchronisation with the next member of a
  solid group, section 14.)
- Running out of packed bytes before U bytes exist is corrupt input
  (section 3).

### 10.2 Solid groups

The file header flag bit `0x0010` ("solid") on a member of unpack version
20 or 26 means: **this member continues the decoder state left by the
preceding member**. A member without the bit starts a new group with a
fresh state (section 5). The archive-level solid flag (main header bit
`0x0008`) is not consulted for these versions; the per-file bit decides
(REFERENCE-ONLY; `unar` honours a per-file bit in an archive whose main
header bit is clear, CONFIRMED (crafted). In real solid archives the main
header bit is set, the first file lacks the per-file bit and all later
files carry it.)

Processing one group, member by member, in archive order:

1. **First member** (no solid bit). Create a fresh state. If P is
   greater than 0, read a table transmission from the start of its
   stream, **even when U is 0**. Then decode items until U bytes are
   complete.

   Real RAR 2.x archives contain such members: a zero-length first file
   of a solid archive is stored with method `0x31`..`0x35`, U = 0 and
   about a hundred packed bytes that hold nothing but the first table
   transmission, and the next file's stream starts directly with items
   (three decoded archives in the survey, and two more seen in header
   probes; `unar` agrees, CONFIRMED (crafted)).

   If P is 0 the state has no codes. If U is also 0 the member is simply
   empty; a later member that continues this group then finds no codes,
   which is corrupt input for that member (DECISION; not seen in real
   data).
2. **Continuation member with U = 0.** Nothing is read and the state is
   unchanged. In real archives such members have P = 0 (17 members in
   three surveyed archives: zero-length "divider" files in the middle of
   a solid archive); the members after them decode correctly under this
   rule (CONFIRMED (real data): every file of those archives matches its
   CRC-32) **although `unar` fails on some of them** (section 14). DECISION: if such a member has P greater than 0, its
   packed bytes are ignored.
3. **Continuation member with U greater than 0.** Start a new bit
   stream at bit 0 of this member's packed bytes. **Do not read a table
   transmission.** Continue decoding in the mode and with the codes,
   window, match memory, channel and predictors the previous member
   left, until U more bytes are complete. Unused bits at the end of the
   previous member's stream are discarded.

Members that are not handled by this decoder but appear between members
of a group:

- **Directory entries** (dictionary bits = 7) have no data and no effect
  on the state. In real solid archives they precede the first file.
- **Stored members** (method `0x30`): none occurs inside a real solid
  group of the survey (RAR 2.x compresses every file of a solid archive,
  even empty ones). DECISION: a stored member is delivered as stored;
  it does not enter the window and does not change the state; a later
  member with the solid bit continues from the last *compressed* member.
  The CRC-32 check of that later member shows whether this guess holds
  for a particular archive (section 16).
- DECISION: a member with the solid bit whose unpack version is 20 or
  26, when the state it would continue belongs to another method (the
  preceding compressed member had version 15, 29 or 36) or does not
  exist (it is the first compressed member seen, as happens when a
  later volume of a set is opened alone), is reported as an error for
  that member ("solid predecessor unavailable") without decoding.

### 10.3 Failures inside a group

DECISION: when a member of a group ends in corrupt input (or is
encrypted, or cannot be read at all), the state is no longer
trustworthy. Every later member that continues the group is reported as
failed without being decoded ("depends on a damaged member"), up to the
next member that starts a new group. A CRC-32 mismatch alone, with the
stream otherwise decoded to exactly U bytes, does not have this effect.

To deliver any member of a group, all members before it in the group
must be decoded, in order, even if the caller does not want them.

### 10.4 Files split across volumes

- A member may be split over several volumes: file header flag bit
  `0x0002` says "continues in the next volume", bit `0x0001` "continued
  from the previous volume". Each piece has its own file header, with
  the same name and the same U (the size of the whole file).
- The packed bytes of the member are the pieces' data areas
  **concatenated in volume order**. The pieces are cut at arbitrary byte
  positions of one continuous bit stream: nothing is aligned, repeated
  or reset at a volume boundary, and the decoder cannot tell where the
  boundaries were. (CONFIRMED (real data): a 132,951,504-byte file split
  over 28 volumes.)
- A solid group can extend across volumes in the same way; the solid
  bit of the first file header of a later volume refers to the last
  member of the previous volume.
- The header CRC-32 of every piece but the last covers that piece's
  packed bytes (section 2) and can be used to detect a damaged volume
  before decoding.
- If a piece is missing the member cannot be decoded: corrupt input (or
  a container-level error), and section 10.3 applies.

### 10.5 End of data, summary

| Situation | Result |
|---|---|
| U bytes complete | Stop. Remaining bits and bytes of the member are ignored. |
| A bit is needed beyond the packed bytes | Corrupt input. |
| A match is longer than the bytes still missing | Corrupt input (DECISION). |
| U = 0, continuation member | Nothing decoded. |
| U = 0, first member of a group, P greater than 0 | Table transmission read, nothing output. |
| U greater than 0, P = 0 | Corrupt input. |

## 11. Worked examples

### 11.1 LZ block: `abcabc`

Packed bytes (18 bytes, 142 bits used), U = 6:

`00 88 00 00 00 00 00 00 00 0a ac ad fe 2a c4 89 f0 d8`

In binary, grouped by byte:

`00000000 10001000 00000000 00000000 00000000 00000000 00000000 00000000 00000000 00001010 10101100 10101101 11111110 00101010 11000100 10001001 11110000 11011000`

| Bits | Value | Meaning |
|--:|---|---|
| 0 | `0` | multimedia flag 0: LZ tables, N = 374 |
| 1 | `0` | keep flag 0: length table cleared |
| 2..77 | 19 fields of 4 bits | pre-code lengths: symbol 1 = 2, symbol 2 = 2, symbol 18 = 2, all others 0. Pre-code: 1 = `00`, 2 = `01`, 18 = `10` (pattern `11` unassigned) |
| 78..79 | `10` | pre-code symbol 18 |
| 80..86 | `1010110` = 86 | 86 + 11 = 97 zeros: positions 0..96 |
| 87..88 | `01` | symbol 2: position 97 = 0 + 2 = 2 |
| 89..90 | `01` | position 98 = 2 |
| 91..92 | `01` | position 99 = 2 |
| 93..94 | `10` | symbol 18 |
| 95..101 | `1111111` = 127 | 138 zeros: positions 100..237 |
| 102..103 | `10` | symbol 18 |
| 104..110 | `0010101` = 21 | 32 zeros: positions 238..269 |
| 111..112 | `01` | position 270 = 2 |
| 113..114 | `10` | symbol 18 |
| 115..121 | `0010010` = 18 | 29 zeros: positions 271..299 |
| 122..123 | `00` | symbol 1: position 300 = 1 |
| 124..125 | `10` | symbol 18 |
| 126..132 | `0111110` = 62 | 73 zeros: positions 301..373. N reached. |

Codes: main code from positions 0..297: symbols 97, 98, 99, 270 with
length 2, so 97 = `00`, 98 = `01`, 99 = `10`, 270 = `11`. Distance code
from positions 298..345: position 300 is distance symbol 2, length 1,
code `0`. Length code: empty.

| Bits | Value | Meaning |
|--:|---|---|
| 133..134 | `00` | main 97: literal `a` |
| 135..136 | `01` | main 98: literal `b` |
| 137..138 | `10` | main 99: literal `c` |
| 139..140 | `11` | main 270: length slot 0, no extra bits, length 0 + 0 + 3 = 3 |
| 141 | `0` | distance symbol 2: base 2, no extra bits, distance 3. No adjustment (3 is below 8192). |

The match copies 3 bytes from 3 back: `abc`. Output `61 62 63 61 62 63`
(`abcabc`), CRC-32 `0x726E994C`. Six bytes are complete; bits 142 and 143
are unused.

### 11.2 All match kinds

Packed bytes (33 bytes, 263 bits used), U = 24:

`0c c0 0c 00 00 00 00 00 0c ce 96 54 1a 7b 2f fa c5 04 02 9d 52 86 9f 34 78 91 a2 b3 c4 d6 fc 71 60`

The table transmission occupies bits 0..196. Pre-code lengths: symbols
0, 1, 4, 16, 17, 18 have length 3, giving 0 = `000`, 1 = `001`,
4 = `010`, 16 = `011`, 17 = `100`, 18 = `101`. It contains a use of
symbol 16: position 48 receives 4, then symbol 16 with extra bits `11`
copies it into the 6 positions 49..54, and symbol 16 with extra bits `00`
into the 3 positions 55..57.

Resulting non-zero lengths: main symbols 33, 48..57, 256, 258, 261 and
272 have length 4; distance symbol 6 (position 304) has length 1; length
symbol 1 (position 347) has length 1.

Main code (ascending symbol number): 33 = `0000`, 48 = `0001`,
49 = `0010`, 50 = `0011`, 51 = `0100`, 52 = `0101`, 53 = `0110`,
54 = `0111`, 55 = `1000`, 56 = `1001`, 57 = `1010`, 256 = `1011`,
258 = `1100`, 261 = `1101`, 272 = `1110`. Distance symbol 6 = `0`.
Length symbol 1 = `0`.

Items from bit 197:

| Bits read | Meaning | Output so far |
|---|---|---|
| ten 4-bit codes `0001` .. `1010` | literals `0`..`9` | `0123456789` |
| `1101`, `11` | main 261, extra 3: two-byte match, distance 0 + 3 + 1 = 4 | `...67` |
| `1110`, `0`, `01` | main 272: length slot 2, length 2 + 3 = 5; distance symbol 6, 2 extra bits = 1: distance 8 + 1 + 1 = 10 | `...23456` |
| `1100`, `0` | main 258: second most recent distance. History is (10, 4), so distance 4. Length symbol 1: slot 1, length 1 + 2 = 3; distance below 257, no adjustment | `...345` |
| `1011` | main 256: repeat distance 4, length 3 | `...634` |
| `0000` | literal `!` | `...!` |

Output: `01234567896723456345634!` (24 bytes). After the items the
distance history reads, most recent first, 4, 4, 10, 4.

### 11.3 Multimedia block, one channel

Packed bytes (18 bytes, 142 bits used), U = 8:

`82 00 20 00 00 00 00 00 00 02 54 1b fd 89 16 40 9d 04`

Bit 0 = `1`: multimedia. Bit 1 = `0`: table cleared. Bits 2..3 = `00`:
one channel, N = 257. The table transmission ends at bit 117 and gives
the symbols 0, 1, 2, 5, 253, 255 and 256 the length 3, so the channel
code is 0 = `000`, 1 = `001`, 2 = `010`, 5 = `011`, 253 = `100`,
255 = `101`, 256 = `110`.

From bit 118 the symbols are 253, 253, 0, 2, 5, 255, 0, 1. All weights
stay 0 (fewer than 32 samples), so each prediction is the previous byte.

| # | s | D1, D2, D3, D4 after step 2 | CD used | Predicted | Output byte | LD after step 6 |
|--:|--:|---|--:|--:|--:|--:|
| 1 | 253 | 0, 0, 0, 0 | 0 | 0 | 3 | 3 |
| 2 | 253 | 3, 3, 0, 0 | 3 | 3 | 6 | 3 |
| 3 | 0 | 3, 0, 3, 0 | 3 | 6 | 6 | 0 |
| 4 | 2 | 0, −3, 0, 3 | 0 | 6 | 4 | −2 |
| 5 | 5 | −2, −2, −3, 0 | −2 | 4 | 255 | −5 |
| 6 | 255 | −5, −3, −2, −3 | −5 | 255 | 0 | 1 |
| 7 | 0 | 1, 6, −3, −2 | 1 | 0 | 0 | 0 |
| 8 | 1 | 0, −1, 6, −3 | 0 | 0 | 255 | −1 |

Output: `03 06 06 04 ff 00 00 ff`. (Sample 5: output 4 − 5 = −1, modulo
256 = 255; LD = 255 − 4 = 251, as a signed byte −5. Sample 6: output
255 − 255 = 0; LD = 0 − 255 = −255, modulo 256 = 1.)

### 11.4 Solid group of two members

Member 1 (no solid bit), U = 9, packed bytes (215 bits used):

`0c c0 0c 00 00 00 00 00 0c ce d6 4e bf e8 44 a8 14 15 20 60 35 01 a3 80 48 d1 60`

The table transmission occupies bits 0..185. Non-zero lengths: main symbols 97..102,
256, 257, 270 and 272 have length 4, giving 97 = `0000`, 98 = `0001`,
99 = `0010`, 100 = `0011`, 101 = `0100`, 102 = `0101`, 256 = `0110`,
257 = `0111`, 270 = `1000`, 272 = `1001`; distance symbols 2 and 6 have
length 1 (2 = `0`, 6 = `1`); length symbol 2 has length 1 (`0`).

Items: literals `a` `b` `c` `d` `e` `f`; main 270 (length 3), distance
symbol 2 (distance 3). Output `abcdefdef`.

Member 2 (solid bit set), U = 13, packed bytes (3 bytes, 20 bits used):

`67 4f 50` = `01100111 01001111 0101....`

No table transmission is read. With member 1's codes:

| Bits | Meaning | Output |
|---|---|---|
| `0110` | main 256: repeat distance 3, length 3. Copies from member 1's output. | `def` |
| `0111`, `0` | main 257: most recent distance, 3. Length symbol 2: length 2 + 2 = 4 | `defd` |
| `1001`, `1`, `11` | main 272: length 5; distance symbol 6 with extra bits 3: distance 8 + 3 + 1 = 12 | `efdef` |
| `0101` | literal `f` | `f` |

Output of member 2: `defdefdefdeff`.

Variant with an empty first member: three members with the packed bytes

- `0c c0 0c 00 00 00 00 00 0c ce d6 4e bf e8 44 a8 14 15 20 60 35 01 a3 80`
  (24 bytes: the table transmission alone, 186 bits), U = 0, no solid
  bit;
- `01 23 45 80` (29 bits used), U = 9, solid bit: the seven items of
  member 1 above without the tables;
- `67 4f 50`, U = 13, solid bit;

decode to nothing, `abcdefdef` and `defdefdefdeff`.

## 12. Test vectors

All of these were decoded by `unar` 1.10.7 from a minimal archive built
around the packed bytes (unpack version 20, method `0x33`), with the
result shown, unless the entry says otherwise. "Bits used" counts the
bits a decoder reads before U bytes are complete.

### 12.1 Valid streams

| # | Packed bytes | U | Expected output | CRC-32 of output |
|--:|---|--:|---|---|
| V1 | `00 88 00 00 00 00 00 00 00 0a ac ad fe 2a c4 89 f0 d8` (142 bits) | 6 | `abcabc` | `0x726E994C` |
| V2 | the bytes of section 11.2 (263 bits) | 24 | `01234567896723456345634!` | `0xBC39F25B` |
| V3 | `00 04 00 00 00 00 00 00 00 07 58 7f ca 37 46 1b 40 80 80 00 00 00 00 00 00 8b ac 06 2d ff d4 ee b6 08` (269 bits) | 10 | `abcabczzab` | `0xC58F1A33` |
| V4 | the bytes of section 11.3 (142 bits) | 8 | `03 06 06 04 ff 00 00 ff` | `0x58A34111` |
| V5 | `92 00 20 00 00 00 00 00 00 02 51 1b fd 89 15 11 6f f6 14 44 94 ad 90 06 c0` (194 bits) | 12 | `ff fe fc fa fd fc 00 00 00 00 fb fb` | `0x85469CA7` |
| V6 | `a0 22 00 00 00 00 00 00 00 22 bb 92 7f fc 27 12 7f fa c6 4c ff f3 00 61 11 00 00 00 00 00 00 00 01 77 6b ff 8e 8b 24 ef 47 54 66 60 00 00 00 00 00 00 06 6c 19 fe 67 21 00 a7 f9 99 00 80 53 fc ca 80 a0` (531 bits) | 15 | `f6 ec e2 ce 68 69 e2 ce 68 69 eb e0 cb e7 db` | `0xD0ED1577` |
| V7 | `82 20 00 00 00 00 00 00 00 02 1b fd a5` followed by 500 bytes `00` (513 bytes, 4104 bits) | 4000 | begins `ff fe fd fc fb fa f9 f8 f7 f6 f5 f4 f3 f2 f1 f0 ef ee ed ec eb ea e9 e8 e7 e6 e5 e4 e3 e2 e1 e0 de dc da d8 d6 d4 d2 d0`, ends `06 07 c5 3f 74 63 0b 6b` | `0xF55FA861` |
| V8 | `82 22 00 00 00 00 00 00 00 02 1f ff a2`, then the three bytes `92 49 24` repeated 500 times, then `92` (1514 bytes, 12106 bits) | 8000 | begins with `ff 00` sixteen times, then `fe ff fd fe fc fd fb fc`; ends `4e d5 57 6a 1e 58 0c 59` | `0xFCF09BB4` |
| V9 | real data, below | 136 | below | `0xF51E61FD` |

Notes:

- **V3** exercises the keep flag. The first transmission (bits 0..113)
  gives main symbols 97, 98, 99 and 269 the length 2. Items: `a` `b` `c`
  `a` `b` `c`, then symbol 269. The second transmission starts at bit
  128 with flags `0` `1` (LZ, keep). Its pre-code has length 2 for the
  symbols 1, 3, 17 and 18. Its coded lengths are: 97 zeros (symbol 18);
  three times symbol 1 (positions 97..99 become 2 + 1 = 3); 22 zeros
  (symbol 18; positions 100..121); symbol 3 at position 122 (0 + 3 = 3);
  138 zeros; 8 zeros (symbol 17; positions 261..268); symbol 1 at
  position 269 (2 + 1 = 3); 104 zeros. The new main code has length 3 for
  97, 98, 99, 122 and 269: 97 = `000`, 98 = `001`, 99 = `010`,
  122 = `011`, 269 = `100`. Items: `z` `z` `a` `b`.
  Note that zero runs overwrite and are not "kept": had a zero run
  covered position 97 its length would have become 0, not stayed 2.
- **V5**: two channels. Symbols in stream order 1, 2, 3, 4, 255, 254,
  253, 252, 0, 0, 5, 5; channel 0 receives the 1st, 3rd, 5th, ... of
  them.
- **V6**: a three-channel multimedia block of four samples (symbols 10,
  20, 30, 40) ended by symbol 256; an LZ block (`h`, `i`, a match of
  length 4 and distance 4) ended by symbol 269; a second three-channel
  multimedia block with the symbols 1, 2, 3, 4, 5. The second block
  starts on **channel 1**, where the first one stopped, and the channel
  delta carried into it is the one left by the fourth sample; the LZ
  bytes in between touch neither.
- **V7**: one channel; code: symbol 1 = `0`, symbol 256 = `1`; 4000
  samples with s = 1. The weights K1..K5 after the last sample are
  8, 8, 0, 0, 0. The first 32 output bytes fall by 1 per byte; from the
  33rd on the adapted weights change the slope.
- **V8**: one channel; code: symbol 1 = `0`, symbol 255 = `10`,
  symbol 256 = `11`; the table transmission is 106 bits long, the
  samples alternate s = 1, s = 255, 8000 in all. Final weights
  −1, −4, 0, 0, 0.
- **V9** is the member `pdx-coll.cue` of the first volume of the public
  archive set named in section 17 (unpack version 20, method `0x35`,
  P = 111, U = 136; 885 bits used). Packed bytes:

  `09 99 14 c8 c0 00 00 00 01 13 7d 0b c1 fc 4d ea cb e6 20 8e bf 59 22 90 62 c8 41 a0 de ff c4 ad 0a 23 a1 92 5d 33 85 61 9a 33 ff f3 8c fd a7 ee 17 8b 1a b0 33 d5 bd 49 73 5b 5f 96 6a 60 8d 14 3d f7 ec 01 b4 09 39 81 fc 06 8e 52 c6 c1 b6 e1 96 df 8e 43 c7 de 40 39 f0 9c a1 d1 02 f4 a6 3b fb 0f 82 f2 27 cd 61 f9 a7 ed f8 21 11 0f d0`

  Output: the six text lines below, each ended by the two bytes `0d 0a`
  (the leading spaces are part of the data: 2 before `TRACK`, 4 before
  `INDEX` and `PREGAP`):

  | Line | Text |
  |--:|---|
  | 1 | `FILE "PDX-COLL.BIN" BINARY` |
  | 2 | `  TRACK 01 MODE2/2352` |
  | 3 | `    INDEX 01 00:00:00` |
  | 4 | `  TRACK 02 AUDIO` |
  | 5 | `    PREGAP 00:02:00` |
  | 6 | `    INDEX 01 08:44:02` |

  It uses 9 explicit-distance matches, 3 two-byte matches and one each
  of symbols 257 and 258; all three LZ codes are complete.

Solid vectors: the two-member and three-member groups of section 11.4.

### 12.2 Streams a decoder must reject

"`unar`" gives what the reference tool does with the same bytes.

| # | Packed bytes | U | Why it is corrupt input | `unar` |
|--:|---|--:|---|---|
| E1 | `00 00 00 00 00 00 00 00 04 04 7f ff ed 00` | 1 | Pre-code: symbols 16 and 18 with length 1. The first coded length is symbol 16 (repeat) at position 0. | fails |
| E2 | `00 40 00 00 00 00 00 00 00 04 d4 3f ff 80 00` | 1 | Main code over-subscribed: symbols 0, 1, 97 and 98 all have length 1. | fails |
| E3 | `00 80 80 00 00 00 00 00 00 0a ac ac 2d bf c0 18 a1 3a 05 42 c0` | 9 | `a` `b` `c`, then a match of length 5 and distance 10 with only 3 bytes produced (DECISION, section 9). | outputs `61 62 63 00 00 00 00 00 7a` |
| E4 | `00 00 80 00 00 00 00 00 08 0a ac 35 fe 08 5a 81 59 ca` | 6 | `a` `b` `c`, then symbol 256 with no previous match (DECISION, section 7.4). | outputs `abcdef` (the symbol produces nothing) |
| E5 | `00 80 80 00 00 00 00 00 08 0b ac d7 ff 32 e3 4d e0 29 cb 90` | 10 | `abcdef`, then a match of length 10 and distance 6 when only 4 bytes are missing (DECISION, section 10.1). | outputs `abcdefabcd` |
| E6 | `00 00 80 00 00 00 00 00 08 0a ac 35 fe f4 0a 72 80` | 20 | The stream holds the six literals `abcdef` and then ends. | fails |
| E7 | `00 40 00 00 00 00 00 00 00 07 59 ff fe 40` | 4 | Main code has the single symbol 97 (code `0`); the items are `0`, `0`, `1`: the third matches no code. | fails |
| E8 | the first 10 bytes of V1 | 6 | Ends inside the table transmission. | fails |
| E9 | no bytes | 6 | P = 0 with U greater than 0. | fails |

For E3, E4 and E5 the CRC-32 in a test archive must be that of the
output `unar` produces if the vector is also to be shown to `unar`; a
conforming decoder reports corrupt input regardless.

## 13. Safety requirements for untrusted input

1. **Never size a buffer from U.** U is a claim. Output is produced
   incrementally; the only large allocation is the 1 MiB window. The
   caller's limits on the unpacked size of a member and of an archive
   apply as for every other format.
2. **Bound U by P before decoding.** No item produces more than 260
   bytes and every item consumes at least 1 bit, so a member cannot
   yield more than 2080 bytes per packed byte. DECISION: U greater than
   2080 × P is corrupt input without decoding (this includes P = 0 with
   U greater than 0).
3. **Every read is bounded by the member's packed bytes** (section 3).
   Because every symbol consumes at least one bit and reading past the
   end fails, no sequence of table switches, empty tables or anything
   else can loop without end: the work is bounded by a constant times P
   plus U.
4. **Length-table writes stay inside positions 0..N − 1** (section 6.3:
   runs are cut off; symbol 16 at position 0 is rejected because there
   is no position −1).
5. **Reject over-subscribed codes when they are built**, including the
   pre-code and including codes the block never uses. Decoding with an
   empty or incomplete code must fail cleanly on an unassigned pattern
   and must not index outside a lookup structure.
6. **Check every match before copying**: distance at least 1 (always
   true for decoded distances; relevant for history entries, see 7.4),
   distance not greater than the group's total output count, length not
   greater than the bytes missing from the member. All ring positions
   are reduced modulo 1,048,576.
7. **Predictor arithmetic cannot overflow** a 32-bit signed integer with
   the ranges of section 8.2 (the prediction sum stays within ±20,000;
   error sums are cleared every 32 samples). The sample counter may wrap;
   only its low 5 bits matter. Shifting a negative sum must round toward
   negative infinity.
8. **Totals are 64-bit.** A group can exceed 4 GiB of output; ring
   positions are derived from the total count modulo the window size.
9. **A failed member poisons its group** (section 10.3); the state is
   discarded, never reused.
10. Verify the CRC-32 of every delivered member (section 2).

## 14. Observed implementation gaps

Faults and leniencies of the reference implementation (`unar` 1.10.7).
They explain why rules exist; nothing here is to be imitated.

1. **Empty continuation members break the members after them.** At a
   member boundary the reference implementation switches to the next
   member's stream and then decodes a symbol from it before checking
   whether that member is complete. For a member with U = 0 and P = 0
   this reads past the end. Real archives are affected: in the three
   surveyed archives that contain such members `unar` reports
   "Unpacking failed" or "Unsupported" for the files that directly
   follow the empty ones (5 of 81, 5 of 34 and 7 of 64 files), while
   the rule of section 10.2 decodes every file with a matching CRC-32.
   A crafted group with an empty middle member shows the same (the
   member after it fails).
2. **A distance beyond the start of the output reads zero bytes.** The
   window starts zero-filled and no check is made (vector E3; a match as
   the very first item outputs zeros).
3. **Symbol 256 before any match** copies nothing (distance 0, length 0)
   and inserts a distance of 0 into the history. **A history place never
   filled** yields distance 0, and the copy then reads the ring position
   it is about to write, that is, stale window contents (zeros in a
   fresh window, old output after a wrap).
4. **A match longer than the member's remaining bytes** is cut off
   silently at U for the last or only member. Inside a solid group the
   surplus bytes are counted towards the next member's boundary, the
   boundary test (an equality) is then never met, and the next member
   fails.
5. **The whole group is decoded with the method of its first member.**
   The unpack version of later members is not looked at, and a stored
   member that follows a member without the solid bit would be fed to
   the decompressor if the member after it carried the solid bit.
6. **Look-ahead past the end of the packed bytes sees zeros**; only
   consuming such bits fails. A truncated stream can therefore decode
   one or more spurious symbols from padding before the failure is
   noticed (vector E6 fails only after 8 bytes). Harmless as long as
   nothing is delivered for a failed member.
7. **Runs past the end of the length table are accepted** and cut off.
   This specification keeps that behaviour (section 6.3).
8. **A CRC-32 mismatch is reported but the bytes are written** to the
   output file all the same.
9. **Method byte and version are barely checked**: any method byte other
   than `0x30` is decoded; an unknown version in the same family is
   passed to no decoder.

## 15. Decisions

| # | Section | Decision |
|--:|---|---|
| R1 | 2 | Only unpack versions 20 and 26 with method `0x31`..`0x35` reach this decoder. |
| R2 | 2 | Encrypted members are reported "encrypted, unsupported" and count as failed members of their group. |
| R3 | 2, 9 | The window is always 1 MiB; the dictionary size declared in the header is not enforced. |
| R4 | 2 | The container layer verifies the CRC-32 of every member; a mismatch is an error for the member. |
| R5 | 3 | Bits beyond the packed bytes are never supplied; what follows the item that completes U bytes is not read or checked. |
| R6 | 6.3 | A run that passes the end of the length table is cut off, not rejected. |
| R7 | 7.4 | Symbol 256 with no previous match, and symbols 257..260 selecting an unfilled history place, are corrupt input. |
| R8 | 9 | A distance greater than the group's output so far is corrupt input. |
| R9 | 10.1 | A match longer than the member's missing bytes is corrupt input. |
| R10 | 10.2 | A first member with P = 0 leaves the group without codes; a continuation that needs them is corrupt input. |
| R11 | 10.2 | A continuation member with U = 0 changes nothing; packed bytes it may have are ignored. |
| R12 | 10.2 | A stored member inside a group is transparent to the decoder state. |
| R13 | 10.2 | A solid-bit member whose predecessor state is of another method or absent is an error, not decoded. |
| R14 | 10.3 | After a failed member, later members of the same group are reported failed without decoding. A CRC-32 mismatch alone does not poison the group. |
| R15 | 13 | U greater than 2080 × P is corrupt input before decoding. |

None of them changes how a genuine, undamaged archive in the survey is
read.

## 16. Open questions

They do not block an implementation; the document says what to do in
each case.

1. **Unpack version 26.** No real archive with this version was found
   among about 1,100 archives whose headers were probed (571 of them
   contain version 20 members). The reference implementation
   treats it exactly like 20 and `unar` decodes a crafted one. Whether
   real version-26 streams differ in any way is unconfirmed.
2. **Stored members inside a solid group** (R12) and **continuation
   members with U = 0 and P greater than 0** (R11): not observed; the
   rules are guesses protected by the CRC-32 check.
3. **Largest match length.** The format allows 260; the longest in the
   survey is 257. Nothing depends on it.
4. **Matches crossing a member boundary** (R9) and **references before
   the start of a group** (R8): not observed in about 3,150 real
   members. If a genuine archive is ever found to need either, the
   reference behaviour is described in section 14.
5. **Encryption** of version 20 members (a block cipher over the packed
   bytes, keyed by a password) is not specified.

## 17. Evidence

**Reference implementation.** XADMaster, revision `7cb9ee0a`: the RAR 2.0
decompression handle, the shared sliding-window, prefix-code and
bit-reader helpers, the multimedia predictor, and the parts of the RAR
archive parser that group files into solid streams and concatenate the
pieces of split files. Kept outside this repository.

**Independent re-statement.** Everything in sections 3 to 10 was
re-implemented as a throw-away experimental decoder (kept outside the
repository, not part of any deliverable) directly from the rules as
worded here; the figures below come from running it.

**Real archives.** 104 archives written by RAR 2.x between about 1995 and
2001, taken from the public collection at `files.scene.org` (art packs,
disk magazines, music disks, demo party releases), plus the multi-volume
set `pdx-coll.rar`, `pdx-coll.r00` .. `pdx-coll.r26` from
`demos/compilations/paradox_psx_intro_collection_no1/` of the same site:

- 3,158 compressed members of unpack version 20; 3,157 decode to bytes
  matching the header CRC-32. The one exception is the first member of a
  later volume opened without its predecessor (solid bit set, nothing to
  continue): the case of R13.
- 2,028 of the members are continuation members of solid groups; 3
  groups begin with an empty member that carries the tables; 17 are
  empty continuation members.
- About 6,200 table transmissions in the 103 single archives (and 4,401
  more in the multi-volume set), 952 of them multimedia, with 1, 2, 3
  and 4 channels (392, 364, 58 and 138); 3,565 with the keep flag set. Mode changes in both directions, channel
  count changes, and switches with the current channel at every possible
  value occur.
- All eight two-byte symbols, symbols 256..260 and explicit matches are
  used heavily (the 133 MB member alone: 2.4 million explicit matches,
  1.2 million two-byte matches, 390,000 history matches, 209,000
  repeats).
- Dictionary flag values 0, 2, 3 and 4; largest distance 1,046,214;
  largest match length 257.
- Every member's stream ends 0 to 7 bits before the end of its packed
  bytes, and those bits are zero.
- Never observed: an over-subscribed or single-symbol code, an
  incomplete pre-code, a run passing the end of the length table,
  symbol 16 at position 0, a distance beyond the output so far, symbol
  256 or 257..260 without the memory they need, a match crossing a
  member boundary, a distance of exactly 1,048,576, a stored member in a
  solid archive, unpack version 26.
- The split file: `pdx-coll.bin`, 132,951,504 bytes, in 28 pieces;
  decoded from the concatenated pieces to the CRC-32 of the last piece's
  header; the header CRC-32 of the first piece equals the CRC-32 of that
  piece's packed bytes.

**Crafted archives given to `unar` 1.10.7** (each compared with the
output predicted by this document; identical in every case where the
document does not deliberately differ, sections 14 and 15): every
two-byte distance range boundary; explicit-match lengths at every
extra-bit boundary from 3 to 258; distances at and around 256, 257,
8191, 8192, 8193, 262143, 262144, 262145, 1048575 and 1048576, each
followed by a history match and a repeat, to fix the length adjustments;
the order of the distance history and the fact that every match kind
inserts into it; code lengths 1 to 15 in one code; single-symbol and
over-subscribed codes; the keep flag within LZ mode and from LZ to
multimedia; multimedia blocks of 5,000 random, small and constant
symbols on 1 to 4 channels; channel carry-over and channel reset across
blocks; solid groups with state carry-over, with an empty first member,
with an empty middle member, with a table switch as first symbol; the
error cases of section 12.2; unpack version 26; method bytes `0x31` to
`0x36`; a wrong CRC-32.

## Clean-room feedback

Notes from the implementation side (clean room). They record where this
document was found silent or inconsistent and what was observed with
black-box tools; they describe behaviour in prose and were not written
from any decoder's source.

1. **Section 10.3 and empty continuation members.** The section says
   that every later member that continues a damaged group is reported
   failed, and does not say whether a continuation member with U = 0
   (section 10.2, "state unchanged") is exempt. Experiment with `unar`
   1.10.7 on a crafted archive of three members (a first member whose
   packed bytes are cut short; a continuation member with P = 0 and
   U = 0; a further continuation member): the first and third members
   fail, the empty one is reported OK and is created as an empty file.
   Resolution: **an empty continuation member is exempt from 10.3**. It
   reads nothing and therefore depends on nothing; it is delivered as an
   empty file (subject to its CRC-32, which is 0) and leaves the state,
   damaged or not, as it is. The container layer and the decoder both
   follow this now.
2. **Names of the two "cannot continue" results.** Three wordings are in
   use for two different conditions; they are meant as follows, here and
   in `old-container.md` and `rar29-lz.md`:
   - an **earlier member of the group could not be decoded** (10.3; C10
     in `old-container.md`; 12.4 in `rar29-lz.md`): "depends on a damaged
     member" and "depends on an unreadable earlier entry" are the same
     result. The implementation reports "depends on an unreadable earlier
     entry of the solid archive";
   - the **state to continue is not there** (10.2, R13; V4 in
     `old-container.md`): "solid predecessor unavailable". The
     implementation reports "has no solid predecessor: the files it
     continues are not available". This is also the result for a solid
     member of a later volume opened without the volumes before it, when
     the volume starts with the orphaned last piece of a split file.
