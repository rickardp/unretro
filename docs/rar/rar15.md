# RAR 1.5 compression method (unpack versions 13 and 15)

Format specification for clean-room implementation. This document describes
the compression method used by RAR 1.3x, 1.4x and 1.5x: the only method of
the old `RE~^` container, and the method of every packed file whose
unpack-version byte is 15 in a RAR4-format (`Rar!\x1a\x07\x00`) archive.

How the packed bytes, the declared unpacked length and the checksum of a
file are found, how volumes are joined, which files form a solid chain and
which version byte selects this decoder are the business of the container
specification, [`old-container.md`](old-container.md). `README.md` in this
directory is the index of the document set.

The document contains no program code and no pseudo-code. Procedures are
written as numbered steps in prose; every constant table an implementer
needs is written out in full.

## 1. How to read this document

### 1.1 Provenance

- The rules were derived by studying the behaviour of an existing open
  decoder, the RAR 1.5 reader of The Unarchiver's XADMaster library (LGPL),
  called the **reference implementation** below, and by experiment.
- The official `unrar` source was **not** consulted: its licence forbids
  using it to re-create the algorithm.
- A throwaway decoder that follows the rules of this document literally was
  written by the specification's author, kept outside the repository, and
  run over real archives (section 1.2). It is not part of the specification
  and must not be looked for.
- The implementation in this repository is written **from this document
  only**. Implementers do not read the reference implementation or any
  other RAR decoder.

### 1.2 Evidence labels

- **CONFIRMED (real data)**: checked on files taken from real archives. The
  throwaway decoder, with every situation this document calls an error
  treated as an error, decoded each file to exactly its declared length and
  the result matched the checksum stored in the archive. The files were:
  - 1,950 packed, non-empty files with version byte 15 from 31 RAR4-format
    archives (one of them a three-volume set with two files split across
    volumes, twelve of them solid archives in which 334 files continue a
    chain), verified by CRC-32. `wrar155.rar` of the repository's sample
    collection is one of them (section 13.3).
  - 276 packed, non-empty files from 19 old-container (`RE~^`) archives
    (eight of them solid, with 77 files continuing a chain; one a
    four-volume solid set with a file split across all four volumes; two
    self-extracting), verified by the 16-bit checksum of
    `old-container.md` section 3.
  Together these 2,226 files contain about 9.5 million literals, 2.8
  million long matches, 1.4 million short matches, 350,000 recent-distance
  matches, 141,000 repeats of the previous match, 104,000 far matches,
  11,330 stored runs with 33,449 matches inside them, and 199 uses of the
  code-set switch. No file was excluded and none failed.
- **CONFIRMED (crafted)**: checked by building a packed stream by hand from
  the rules of this document, wrapping it in a minimal RAR4-format archive
  and extracting it with `unar` 1.10.7 used as a black box. `unar` is built
  on the reference implementation, so this confirms that the document
  describes the reference implementation's behaviour, not that two
  independent decoders agree. All test vectors of section 13 are of this
  kind unless marked otherwise.
- **REFERENCE-ONLY**: what the reference implementation does in a situation
  that no real file reaches and that was not worth crafting.
- **INFERRED**: deduced, not confirmed.
- **DECISION**: a behaviour chosen by this specification, usually for
  malformed input. No decision changes how a genuine stream decodes: none
  of the 2,226 real files reaches any of them. All decisions are listed in
  section 14.

### 1.3 Notation

- Numbers are decimal unless written with a `0x` prefix. Bit strings and
  codes are written in code style, first bit on the left: `1101`.
- `a >> n` means the integer a divided by 2 to the power n, rounded down.
- **P** is the number of packed bytes of one file (all its volume parts
  joined), **U** its declared unpacked length. Both are untrusted.
- A **place** is an index 0..255 into one of the adaptive tables of
  section 6. A **symbol** is the value decoded from one prefix code.
- An **item** is one step of the decoder: a literal, a match of some kind,
  or a control item that produces no output.
- A **chain** is a sequence of files that share decoder state: one
  non-solid file, or the first file of a solid archive followed by all the
  files after it (section 11).

## 2. The contract with the container layer

The decoder is given:

1. the packed bytes of one file, as one contiguous byte sequence (the
   container layer has already joined the parts of a file split across
   volumes, in volume order, with nothing inserted or removed);
2. the declared unpacked length U;
3. whether the file starts a new chain or continues the chain of the
   previous file (section 11).

It returns exactly U bytes, or an error. Error kinds:

- **truncated input**: a bit was needed beyond the end of the packed bytes;
- **corrupt input**: any rule of this document marked "corrupt input";
- **output limit exceeded**: a limit set by the caller (section 12).

Rules shared by all cases:

- A file with U = 0 is empty. Nothing is decoded, the packed bytes (there
  are normally none) are ignored, and the decoder state of the chain is
  left completely untouched, including the per-file part of it. CONFIRMED
  (real data): zero-length members occur inside solid chains.
- P = 0 with U greater than 0 is truncated input.
- The stream has **no end marker and no header**. Decoding stops when U
  bytes have been produced (section 10).
- Packed bytes left unread are never an error. Genuine streams leave
  between 0 and 8 unused bits after the last item, which means that the
  last packed byte is sometimes not needed at all. CONFIRMED (real data).
- Verifying the result against the stored checksum is the container
  layer's job (`old-container.md` sections 3 and 6). The decoder itself
  has no integrity check, so a decoder result must never be delivered
  unverified.
- When a file ends in a decoder error, the state of its chain is unusable:
  every later file of the same chain fails too (section 11.3).

## 3. Bit order

The packed bytes are read as a stream of bits, **most significant bit of
each byte first**. A field of n bits is read first bit = most significant
bit of the field. Example: the bytes `57 E4` are the bits
`0101 0111 1110 0100`; a 7-bit field read first has the value `0101011`
= 43, and the next 3-bit field has the value `111` = 7.

All prefix codes are read one bit at a time in the same order; the codes in
the tables of section 5 are written first bit on the left.

Reading starts at the first bit of the first packed byte of the file, also
for a file that continues a solid chain: the previous file's unused bits
are discarded and the new file starts on its own first byte.

A request for a bit that lies beyond the last packed byte is **truncated
input**. No substitute bits are supplied. (DECISION D1. The reference
implementation behaves the same way: it fails with "attempted to read more
data than was available", test vector T9.) An implementation that looks
ahead, for instance by peeking 12 bits to index a decoding table, may pad
the look-ahead with zero bits, but must report truncated input as soon as a
code or field it actually consumes extends beyond the end.

## 4. Overview

The method is a byte-oriented LZ77 scheme with a 65,536-byte window. There
are no code tables in the stream. Instead:

- every prefix code is one of a small set of **fixed** codes (section 5);
- which fixed code is used at a given moment is chosen by **running
  averages** that both the packer and the unpacker maintain (section 7);
- what the decoded symbol means is found through **adaptive place tables**
  that keep frequently used byte values at low places (section 6).

Items are of three families, corresponding to the three coding modes:

- **Huffman mode**: a literal byte.
- **Long LZ**: a match with a distance of up to 32,767 and a length of 3 to
  266.
- **Short LZ**: a group of short forms selected by a *selector*: a match
  with a distance of 1 to 256 and length 2 to 10, a repeat of the previous
  match, a match at one of the four most recent distances, a far match with
  a distance of 32,768 to 65,535, and a control item.

Which family the next item belongs to is told by **flag bits**, delivered
eight at a time in *flag bytes* that are themselves coded in the stream.
When many literals follow one another the stream switches to **stored
mode**, in which no flag bits are used at all (section 9).

## 5. Fixed prefix codes

### 5.1 The seven sequential codes

Seven codes are defined by tables of the following kind. In every one of
them the symbols are numbered from 0, the code lengths never decrease as
the symbol number grows, and the codes are assigned in the canonical way:

1. Symbol 0 gets the code consisting of zero bits only, of the length the
   table gives it.
2. Each following symbol gets the code of the previous symbol plus one
   (as a binary number). If the symbol is longer than the previous one by
   k bits, k zero bits are appended on the right after adding one.

Each table lists, for every run of symbols of equal code length: the
length in bits, the number of symbols, the symbol numbers, and the first
and last code of the run (which rule 2 reproduces; they are given so that
an implementation can be checked). Every code is complete: every bit
sequence leads to a symbol after at most 12 bits (10 for H2 and H3, 9 for
H4), so "no symbol matches" cannot happen for these codes.

**L1**: length values, 256 symbols.

| Bits | Symbols | Symbol numbers | First code | Last code |
|--:|--:|:--|:--|:--|
| 2 | 2 | 0..1 | `00` | `01` |
| 3 | 1 | 2 | `100` | `100` |
| 4 | 2 | 3..4 | `1010` | `1011` |
| 5 | 2 | 5..6 | `11000` | `11001` |
| 6 | 4 | 7..10 | `110100` | `110111` |
| 7 | 5 | 11..15 | `1110000` | `1110100` |
| 8 | 4 | 16..19 | `11101010` | `11101101` |
| 9 | 4 | 20..23 | `111011100` | `111011111` |
| 10 | 8 | 24..31 | `1111000000` | `1111000111` |
| 12 | 224 | 32..255 | `111100100000` | `111111111111` |

**L2**: length values, 256 symbols.

| Bits | Symbols | Symbol numbers | First code | Last code |
|--:|--:|:--|:--|:--|
| 3 | 5 | 0..4 | `000` | `100` |
| 4 | 2 | 5..6 | `1010` | `1011` |
| 5 | 2 | 7..8 | `11000` | `11001` |
| 6 | 4 | 9..12 | `110100` | `110111` |
| 7 | 5 | 13..17 | `1110000` | `1110100` |
| 8 | 4 | 18..21 | `11101010` | `11101101` |
| 9 | 4 | 22..25 | `111011100` | `111011111` |
| 10 | 8 | 26..33 | `1111000000` | `1111000111` |
| 11 | 2 | 34..35 | `11110010000` | `11110010001` |
| 12 | 220 | 36..255 | `111100100100` | `111111111111` |

**H0**: 257 symbols.

| Bits | Symbols | Symbol numbers | First code | Last code |
|--:|--:|:--|:--|:--|
| 4 | 8 | 0..7 | `0000` | `0111` |
| 5 | 8 | 8..15 | `10000` | `10111` |
| 6 | 8 | 16..23 | `110000` | `110111` |
| 7 | 9 | 24..32 | `1110000` | `1111000` |
| 12 | 224 | 33..256 | `111100100000` | `111111111111` |

**H1**: 257 symbols.

| Bits | Symbols | Symbol numbers | First code | Last code |
|--:|--:|:--|:--|:--|
| 5 | 4 | 0..3 | `00000` | `00011` |
| 6 | 40 | 4..43 | `001000` | `101111` |
| 7 | 16 | 44..59 | `1100000` | `1101111` |
| 8 | 16 | 60..75 | `11100000` | `11101111` |
| 9 | 4 | 76..79 | `111100000` | `111100011` |
| 11 | 47 | 80..126 | `11110010000` | `11110111110` |
| 12 | 130 | 127..256 | `111101111110` | `111111111111` |

**H2**: 257 symbols.

| Bits | Symbols | Symbol numbers | First code | Last code |
|--:|--:|:--|:--|:--|
| 5 | 2 | 0..1 | `00000` | `00001` |
| 6 | 5 | 2..6 | `000100` | `001000` |
| 7 | 46 | 7..52 | `0010010` | `0111111` |
| 8 | 64 | 53..116 | `10000000` | `10111111` |
| 9 | 116 | 117..232 | `110000000` | `111110011` |
| 10 | 24 | 233..256 | `1111101000` | `1111111111` |

**H3**: 257 symbols.

| Bits | Symbols | Symbol numbers | First code | Last code |
|--:|--:|:--|:--|:--|
| 6 | 2 | 0..1 | `000000` | `000001` |
| 7 | 14 | 2..15 | `0000100` | `0010001` |
| 8 | 202 | 16..217 | `00100100` | `11101101` |
| 9 | 33 | 218..250 | `111011100` | `111111100` |
| 10 | 6 | 251..256 | `1111111010` | `1111111111` |

**H4**: 257 symbols.

| Bits | Symbols | Symbol numbers | First code | Last code |
|--:|--:|:--|:--|:--|
| 8 | 255 | 0..254 | `00000000` | `11111110` |
| 9 | 2 | 255..256 | `111111110` | `111111111` |

Check values for an implementation of rule 2: in H1 symbol 82 is
`11110010010` and symbol 65 is `11100101`; in H2 symbol 32 is `0101011`,
symbol 224 is `111101011` and symbol 256 is `1111111111`; in L1 symbol 255
is `111111111111`.

The H codes serve several purposes (literal places, distance places, flag
byte places, raw distance parts); which one is used where is stated in
sections 8 and 9.

### 5.2 The four selector codes

The selector of a short item is read with one of four codes that are
**not** sequential. They are given code by code. S-A and S-C have 15
symbols; S-B and S-D have 14 (they lack selector 14). All four are
complete.

| Selector | S-A | S-B | S-C | S-D |
|--:|:--|:--|:--|:--|
| 0 | `0` | `0` | `00` | `00` |
| 1 | `1010` | `101` | `010` | `010` |
| 2 | `1101` | `1101` | `011` | `011` |
| 3 | `1110` | `1110` | `1010` | `101` |
| 4 | `11110` | `11110` | `1101` | `1101` |
| 5 | `111110` | `111110` | `1110` | `1110` |
| 6 | `1111110` | `1111110` | `11110` | `11110` |
| 7 | `11111110` | `11111110` | `111110` | `111110` |
| 8 | `11111111` | `11111111` | `111111` | `111111` |
| 9 | `1100` | `1100` | `1100` | `1100` |
| 10 | `1000` | `1000` | `1000` | `1000` |
| 11 | `10010` | `10010` | `10010` | `10010` |
| 12 | `100110` | `100110` | `100110` | `100110` |
| 13 | `100111` | `100111` | `100111` | `100111` |
| 14 | `1011` | (none) | `1011` | (none) |

S-B differs from S-A only in selector 1 (`101` instead of `1010`, which
absorbs the code `1011` of selector 14). S-D differs from S-C in the same
way in selector 3.

### 5.3 The unary length code

Used for the length value of a long match when the average length is low
(section 8.2). Read bits until a 1 bit has been read or eight 0 bits have
been read:

- a 1 bit preceded by n zero bits (n = 0..7) gives the value n; the 1 bit
  is consumed;
- eight 0 bits are followed by an 8-bit field that gives the value
  (0..255) directly.

So `1` is 0, `01` is 1, `00000001` is 7, and `00000000 00001000` is 8.

## 6. Adaptive tables

### 6.1 Place tables

There are three place tables with the same structure and procedure:

| Table | Purpose | Regroup limit | Initial contents |
|:--|:--|--:|:--|
| PL | literal bytes | 161 (0xA1) | place p holds byte value p; all counts 0; all class starts 0 |
| PF | flag bytes | 255 | place p holds byte value (256 − p) mod 256, that is 0x00 at place 0, 0xFF at place 1, 0xFE at place 2, ..., 0x01 at place 255; all counts 0; all class starts 0 |
| PD | high byte of a long-match distance | 255 | place p holds byte value p; then the table is regrouped (procedure below), so counts and class starts are those of a regrouped table |

A place table consists of:

- 256 **entries**, one per place 0..255. An entry is a pair: a byte value
  and a **count** in 0..255.
- 256 **class starts**, one per count value 0..255. Each is a number in
  0..256.

The entries always hold each byte value exactly once. The table is always
ordered by count: counts never increase from place 0 to place 255, and the
class start of count c equals the number of entries whose count is greater
than c, which is the place of the first entry having count c (if any).
These two properties hold initially and are preserved by the procedures
below; an implementation need not rely on them but may check them
(section 12, rule R6).

**Taking a place.** Given a place p (0..255), the table returns a byte
value and updates itself:

1. Let b and c be the byte value and the count of the entry at place p.
2. If c is greater than or equal to the table's regroup limit, *regroup*
   the table (below), then let c be the new count of the entry at place p.
   (Regrouping does not move entries, so b is unchanged.)
3. Let q be the class start of count c. Add 1 to that class start.
4. The entry at place p becomes a copy of the entry at place q; then the
   entry at place q becomes byte value b with count c + 1.
5. The result is b.

In words: the entry that was used moves to the front of the group of
entries that had the same count as it, the entry that was there takes its
old place, and its count goes up by one. When q equals p nothing moves and
only the count changes. q is never greater than p.

**Regrouping.** Counts are replaced by eight coarse classes according to
the current place; byte values stay where they are:

1. For every place p, the count becomes 7 − (p >> 5): places 0..31 get
   count 7, places 32..63 count 6, and so on down to places 224..255,
   which get count 0.
2. The class start of count c becomes (7 − c) × 32 for c = 0..6, that is
   224, 192, 160, 128, 96, 64, 32 for c = 0, 1, 2, 3, 4, 5, 6; the class
   starts of counts 7..255 become 0.

The regroup limit keeps counts from overflowing: a count is at most the
limit when step 4 adds one, so it never exceeds 255.

*Example (PL, initial state).* Taking place 82 returns byte 0x52. Its
count 0 has class start 0, so the entry moves to place 0 with count 1, the
entry that was at place 0 (byte 0x00, count 0) moves to place 82, and the
class start of count 0 becomes 1. Taking place 65 next returns 0x41, which
moves to place 1 (class start of count 0 is now 1) with count 1; byte 0x01
goes to place 65. Byte 0x52 is now at place 0: taking place 0 returns
0x52, leaves it at place 0 (class start of count 1 is 0) and raises its
count to 2. This is exactly what happens in test vector T1.

*Example (regrouping).* Test vector T7 uses the byte 0x41 164 times. The
first use takes it from place 65 to place 0; from then on it stays at
place 0 and its count grows by one per use. Before the 162nd use its count
is 161, so the table is regrouped: 0x41 gets count 7, the class start of
count 7 is 0, and the use leaves it at place 0 with count 8 and raises the
class start of count 7 to 1. The byte 0x64 was never used and is at place
100, which after regrouping has count 4 (class start 96). Taking it four
times moves it to places 96, 64, 32 and finally 1 (the class start of
count 7, behind 0x41).

### 6.2 Short-distance list

A list SD of 256 entries, each a number 0..255, initially entry i = i.

**Taking a place** p: let v be entry p. If p is not 0, exchange entries p
and p − 1. The result is v. (The entry used moves one place toward the
front.)

### 6.3 Recent distances

- An array RD of four distances, initially all 0, and a write counter RC,
  initially 0.
- **Recording** a distance d: store d in RD at index RC mod 4, then add 1
  to RC. (Only the low two bits of RC matter.)
- The **k-th most recent** distance, k = 1..4, is RD at index
  (RC − k) mod 4.
- The **last match**: a distance LD and a length LN, both initially 0.

## 7. Decoder state

### 7.1 Chain state

Created when a chain starts, carried from file to file inside a chain.

| Name | Meaning | Initial value |
|:--|:--|:--|
| window | 65,536 bytes of history | empty (section 10) |
| PL, PF, PD | place tables | section 6.1 |
| SD | short-distance list | section 6.2 |
| RD, RC, LD, LN | recent distances, last match | all 0 |
| LW | literal weight | 128 |
| MW | match weight | 128 |
| NL | count of consecutive literals | 0 |
| AL | running average of literal places | 13,568 (0x3500) |
| AS | running average of short selectors | 0 |
| AN | running average of long-match length values | 0 |
| AD | running average of long-match distance places | 0 |
| AB | counter of minimum-length long matches | 0 |
| MAXD | distance threshold | 8,193 (0x2001) |
| X | code-set switch (off or on) | off |

MAXD only ever holds 8,193 or 32,512 (0x7F00). LW and MW never exceed 255,
AB never exceeds 256, and the four averages settle below 70,000 whatever
the input. 32-bit unsigned arithmetic is ample.

### 7.2 File state

Reset at the start of **every** file, also of a file that continues a
chain:

| Name | Meaning | Initial value |
|:--|:--|:--|
| FB | current flag byte | (none) |
| NF | number of unused bits in FB | 0 |
| SM | stored mode (off or on) | off |
| NR | repeat counter | 0 |

and the bit reader, which starts at the first bit of the file's packed
bytes (section 3).

Note that NL is chain state: a run of literals may begin at the end of one
file and count on in the next. CONFIRMED (real data): when NL is reset to
0 at the start of each file instead, 108 of the 411 real files that
continue a chain fail (decode error or wrong checksum, including the
files that follow a failed one in the same chain).

## 8. Decoding

### 8.1 Main loop

Repeat until U bytes have been produced for this file:

1. If SM is on: decode one **stored-mode item** (section 9) and start over
   at step 1.
2. Let "match expected" be true if MW is greater than LW, false otherwise.
   (Evaluated now, before any flag bit is read.)
3. Read one flag bit (section 8.2).
   - If it is 1: the item is the expected kind. If match expected, decode a
     **long match** (section 8.4); otherwise decode a **literal**
     (section 8.3).
   - If it is 0: read a second flag bit.
     - If the second bit is 1: the item is the unexpected kind. If match
       expected, decode a **literal**; otherwise decode a **long match**.
     - If the second bit is 0: decode a **short item** (section 8.5).

So the flag patterns are: `1` = the kind currently expected, `01` = the
other of literal and long match, `00` = short item.

The test "U bytes have been produced" is made before each item. Items that
produce no output (the control items of sections 8.5 and 9) do not end the
loop.

### 8.2 Flag bits

To read one flag bit:

1. If NF is 0: read a symbol with code **H2**. If the symbol is 256, this
   is corrupt input (REFERENCE-ONLY; the reference implementation rejects
   it too). Otherwise take place *symbol* from table **PF**; the result is
   the new flag byte FB. Set NF to 8.
2. Subtract 1 from NF. The flag bit is bit number NF of FB, where bit 7 is
   the most significant bit. (The bits of a flag byte are used most
   significant first.)

A flag byte is fetched only when a flag bit is actually needed. The two
flag bits of one item may come from two different flag bytes. Flag bits
left over at the end of a file are discarded (NF is file state).

### 8.3 Literal (not in stored mode)

1. Choose the code by AL: **H0** if AL < 3,584 (0xE00); **H1** if
   AL < 13,824 (0x3600); **H2** if AL < 24,064 (0x5E00); **H3** if
   AL < 30,208 (0x7600); otherwise **H4**.
2. Read a symbol s with that code. The place is p = s, except that symbol
   256 gives place 0. (REFERENCE-ONLY for symbol 256: it never occurs in
   the 9.5 million real literals; the reference implementation treats it
   as place 0. DECISION D2: accept it as place 0.)
3. Stored-mode trigger: if NL is 16 or more **and** NF is 0, turn SM on.
   Then add 1 to NL, whether or not SM was turned on. (The test uses the
   value NL had before the addition: the trigger can fire from the 17th
   consecutive literal on, at a moment when the current flag byte has been
   used up completely. The current literal is still decoded as described
   here; stored mode begins with the next item.)
4. Add p to AL, then subtract AL >> 8 from AL.
5. Add 16 to LW. If LW is now greater than 255: set LW to 144 (0x90) and
   replace MW by MW >> 1.
6. Take place p from table **PL**. The result is the literal byte. Append
   it to the output (section 10).

### 8.4 Long match

1. Set NL to 0.
2. Add 16 to MW. If MW is now greater than 255: set MW to 144 (0x90) and
   replace LW by LW >> 1.
3. Read the **length value** v (0..255): with code **L2** if AN is 122 or
   more; with code **L1** if AN is 64 or more (and less than 122);
   otherwise with the unary length code of section 5.3.
4. Read the **distance place** symbol s: with code **H0** if AD < 1,792
   (0x700); with **H1** if AD < 10,496 (0x2900); otherwise with **H2**.
   If s is 256 this is corrupt input (REFERENCE-ONLY; the reference
   implementation rejects it too).
5. Take place s from table **PD**; the result h is the high part of the
   distance. Read a 7-bit field g. The distance is d = h × 128 + g
   (0..32,767).
6. The length is n = v + 3; add 1 if d is greater than or equal to MAXD;
   add 8 if d is less than or equal to 256. (The two additions exclude one
   another. n is 3..266.)
7. Remember the present value of MAXD as M. Then set MAXD: to 32,512
   (0x7F00) if AB is greater than 176, or if AL is 10,752 (0x2A00) or more
   and AN is less than 64; otherwise to 8,193 (0x2001). (AB, AL and AN are
   used with the values they have before step 8.)
8. Add v to AN, then subtract AN >> 5 from AN. Add s to AD, then subtract
   AD >> 8 from AD.
9. Update AB:
   - if v is 0 and d is less than or equal to M: add 1 to AB, then subtract
     AB >> 8 from AB;
   - otherwise, if v is neither 1 nor 4, and AB is greater than 0: subtract
     1 from AB;
   - otherwise leave AB unchanged.
10. Record d in the recent distances (section 6.3). Set LD to d and LN
    to n.
11. Copy a match of distance d and length n (section 10).

A distance of 0 can be expressed (h = 0, g = 0) but is never produced by a
packer; it is corrupt input (section 10, DECISION D4).

### 8.5 Short item

1. Set NL to 0.
2. If NR is 2: read one bit.
   - If it is 1: copy a match of distance LD and length LN. The item is
     complete; nothing else changes (NR stays 2, so the next short item
     starts with this bit again).
   - If it is 0: set NR to 0 and continue with step 3.
3. Read the **selector** with the code chosen by AS and X:

   | | X off | X on |
   |:--|:--|:--|
   | AS less than 37 | S-B | S-A |
   | AS 37 or more | S-D | S-C |

4. Act on the selector:

**Selector 0..8: short match.**

1. Set NR to 0.
2. Add the selector to AS, then subtract AS >> 4 from AS.
3. Read a symbol s with code **H2**; symbol 256 counts as 0
   (REFERENCE-ONLY for symbol 256; DECISION D2: accept it as 0). Take
   place s from the short-distance list **SD**; the result plus 1 is the
   distance d (1..256).
4. The length is n = selector + 2 (2..10).
5. Record d in the recent distances. Set LD to d and LN to n.
6. Copy a match of distance d and length n.

**Selector 9: repeat the last match.**

1. Add 1 to NR.
2. Copy a match of distance LD and length LN. Nothing is recorded.

(After two selector-9 items in a row NR is 2 and each following short item
begins with the one-bit question of step 2 above.)

**Selector 10..13: match at a recent distance, or the switch.**

1. Set NR to 0.
2. Let d be the k-th most recent distance, k = selector − 9 (selector 10
   is the most recently recorded distance, 13 the fourth most recent).
3. Read a length value v with code **L1** (always L1 here).
4. If v is 255 **and** the selector is 10: this item is the **code-set
   switch**. Toggle X (off becomes on, on becomes off). The item is
   complete; it produces no output and changes nothing else.
5. Otherwise the length is n = v + 2; add 1 if d is greater than 256; add
   1 more if d is greater than or equal to MAXD. (n is 2..259. For
   selectors 11..13 the value v = 255 is an ordinary length.)
6. Record d in the recent distances (it is recorded again, becoming the
   most recent). Set LD to d and LN to n.
7. Copy a match of distance d and length n.

**Selector 14: far match.** (Only codes S-A and S-C contain it, so it can
occur only while X is on.)

1. Set NR to 0.
2. Read a length value v with code **L2**. The length is n = v + 5
   (5..260).
3. Read a 15-bit field g. The distance is d = g + 32,768
   (32,768..65,535).
4. Set LD to d and LN to n. The distance is **not** recorded in the recent
   distances.
5. Copy a match of distance d and length n.

A packer turns X on once, the first time it needs a far match; X then
normally stays on for the rest of the chain. CONFIRMED (real data): far
matches occur only in files in or after which a switch item occurred, and
in later members of the same solid chain.

Before any match has been made LN is 0 and the recent distances are 0.
A repeat of the last match with LN = 0 produces nothing; that is harmless
and is accepted. A recent-distance match whose distance is 0 is corrupt
input like any other distance of 0 (section 10).

## 9. Stored mode

Stored mode is entered by the trigger of section 8.3 step 3, during a long
run of literals. While SM is on, no flag bits are read; every item starts
with a literal symbol, and symbol 0 is an escape.

One stored-mode item:

1. Choose the code by AL exactly as in section 8.3 step 1, and read a
   symbol s with it.
2. If s is 0, read one bit.
   - If the bit is 1: **leave stored mode**. Set SM off and NL to 0. The
     item is complete and produces no output. (NF is 0 at this point, so
     the next item begins by fetching a flag byte.)
   - If the bit is 0: **stored-mode match**. Read one bit: 1 means length
     4, 0 means length 3. Read a symbol t with code **H2** and a 5-bit
     field g. The distance is d = t × 32 + g. Here t is used directly, not
     through any place table, and may be 256, so d is 0..8,223. Copy a
     match of distance d and length 3 or 4. Nothing else changes: the
     recent distances, LD, LN, the weights, the averages and NL are left
     as they are.
3. If s is 1..256: a **stored-mode literal** with place p = s − 1. Do
   steps 4, 5 and 6 of section 8.3 (update AL, update LW and MW, take place
   p from PL and append the byte). Step 3 of section 8.3 is not done: NL
   is not changed in stored mode.

Stored mode ends only through the leave item, or at the end of the file
(SM is file state: the next file of a chain starts with SM off, whatever
the state at the end of the previous file).

## 10. Window and output

- The window holds the most recent output of the chain. Its size is always
  65,536 bytes; nothing in a header changes it.
- **Appending a byte** adds it to the file's output and to the window.
  Let T be the number of bytes appended in the chain so far (over all its
  files). An implementation with a circular buffer writes at index
  T mod 65,536.
- **Copying a match** of distance d and length n appends n bytes one at a
  time, each being the byte that was appended d bytes before it. Because
  the copy proceeds byte by byte, a match may overlap its own output: with
  d = 1 it repeats the previous byte n times; with d = 3 and n = 6 after
  `abc` it appends `abcabc` (test vector T2).
- **Valid distances.** A distance is valid if it is at least 1 and at most
  T, the number of bytes in the chain's history at that moment (T can be
  capped at 65,535, the largest distance the format can express). A match
  with distance 0, or with a distance greater than T, is **corrupt
  input**. DECISION D4. The reference implementation performs no such
  check: its window starts out filled with zero bytes and it copies
  whatever is there (test vector T8 yields zero bytes). No real file
  contains such a match (0 of 4.9 million), and treating the uninitialised
  window as data is how a decoder leaks stale memory, so the stricter rule
  is specified. The check applies equally to long, short, recent-distance,
  far, repeated and stored-mode matches; a repeat with LN = 0 copies
  nothing and is not checked.
- In a later file of a solid chain, bytes of earlier files are legitimate
  match sources; T counts them.
- **End of file.** The file is complete when exactly U bytes have been
  produced. If a match would carry the output beyond U, only the bytes up
  to U are appended, to the output and to the window alike, and the file
  is complete. DECISION D5. (The reference implementation likewise cuts
  the output at U, test vector T10. No real file ends inside a match: in
  all 2,226 the last item ends exactly at U.)
- Whatever follows the item that completes U bytes is neither read nor
  checked.

The longest possible match is 266 bytes, far below the window size, so a
match never overwrites its own source in a circular buffer of 65,536
bytes.

## 11. Chains: solid archives and volumes

### 11.1 Volumes

A file split across volumes is **one** packed stream. The container layer
concatenates the parts; the decoder sees no boundary. There is no padding,
no alignment and no state change at a volume boundary; a code may straddle
it. CONFIRMED (real data) in both container generations.

### 11.2 Solid chains

Which files form a chain is decided by the container layer
(`old-container.md` sections 4.3 and 5.3). For the decoder:

- The **first file of a chain** starts with fresh chain state (section
  7.1) and fresh file state (section 7.2). The window is empty (T = 0).
- **Every later file** keeps the chain state exactly as the previous file
  of the chain left it, including the window contents and T, and starts
  with fresh file state: NF = 0, SM off, NR = 0, and the bit reader at the
  first bit of the new file's packed bytes.
- Files must be decoded in archive order. A member of a chain cannot be
  decoded without decoding every packed member before it.

CONFIRMED (real data): the second file of `wrar155.rar` and 410 other files
that continue a chain decode correctly with these rules; decoded with fresh chain
state they fail or give wrong data (test vector T11 shows both results).

Files that take no part in a chain although they lie inside one:

- A file with U = 0 (section 2).
- A directory entry.
- A stored file (method "store"): its bytes are copied by the container
  layer and, as far as is known, pass neither through the decoder nor
  through the window. INFERRED: no real solid archive of these two
  container generations with a stored member was available.
  `old-container.md` section 8 lists this as an open point and says what
  to do.

### 11.3 After an error

If a file of a chain cannot be decoded (truncated input, corrupt input, an
entry that is encrypted or otherwise not decodable), the chain state no
longer matches what the packer had. Every later file of the same chain is
reported as failed without being decoded. DECISION D6. A file that decoded
to exactly U bytes but fails the container layer's checksum comparison is
not delivered; the files after it are still decoded, and each is judged by
its own checksum (`old-container.md` section 4.1).

## 12. Safety requirements

- **R1. Termination.** Every item consumes at least one bit of packed
  data (control items and empty repeats included), so decoding a finite
  input always terminates: either U bytes are reached or the bits run out
  (truncated input). An implementation must not contain a loop that can
  spin without consuming input.
- **R2. No allocation from U.** U is a claim. Memory proportional to U
  must not be reserved before the data exists. The decoder itself needs
  only the 65,536-byte window and the tables of sections 5 to 7 (a few
  kilobytes).
- **R3. Plausibility of U** (optional pre-check, DECISION D7). An item
  yields at most 266 bytes and costs more than two bits, so P packed bytes
  can never produce more than 2,136 × P bytes. A file with U greater than
  2,136 × P may be rejected as corrupt input before decoding. The largest
  ratio of U to P among the real files is 438.
- **R4. Caller limits.** The caller may limit the unpacked size of one
  file and of one archive; exceeding a limit is "output limit exceeded".
- **R5. Distances** are checked as in section 10 (D4).
- **R6. Table indices.** Places come from codes of at most 257 symbols;
  symbol 256 is handled explicitly wherever it can occur (sections 8.2,
  8.3, 8.4, 8.5, 9). In the place-table procedure the place q of step 3 is
  always 0..255 when the table is in a state reachable from the initial
  state; an implementation must nevertheless bound-check q (treating a
  violation as an internal error or as corrupt input) rather than index
  blindly.
- **R7. Truncated input** is an error, never a short success (D1).
- **R8. Nothing is delivered** for a file unless U bytes were produced and
  the container layer's checksum comparison succeeded.

## 13. Worked examples and test vectors

All vectors are complete packed streams for the first file of a chain
(fresh state). "CRC-32" is the standard CRC-32 of the unpacked bytes;
"sum16" is the 16-bit checksum of `old-container.md` section 3. The vectors
T1 to T7 and T11 were each wrapped in a RAR4-format archive (file header
with unpack-version byte 15, method byte 0x33) and extracted with `unar`
1.10.7, which produced exactly the bytes shown and accepted the CRC-32.

### T1. Three literals

Packed (4 bytes): `57 E4 B9 40`. U = 3. Unpacked: `52 41 52` ("RAR").
CRC-32 0xFC7E6D50, sum16 0x0438. 31 bits are used.

| Bits | Meaning |
|:--|:--|
| `0101011` | Flag byte needed (NF = 0): H2 symbol 32. PF place 32 holds 0xE0; flag byte 0xE0 = `11100000`. |
| | Flag bit `1`; LW = MW = 128, so a match is not expected: literal. AL = 13,568, so code H1. |
| `11110010010` | H1 symbol 82: PL place 82 holds 0x52. It moves to place 0. AL becomes 13,650 − 53 = 13,597. LW becomes 144. |
| | Flag bit `1`: literal. |
| `11100101` | H1 symbol 65: PL place 65 holds 0x41; it moves to place 1. |
| | Flag bit `1`: literal. |
| `00000` | H1 symbol 0: PL place 0 holds 0x52. |

The remaining bit of the fourth byte and the remaining five flag bits are
unused.

### T2. Short match that overlaps itself

Packed (7 bytes): `57 E8 7D 17 A3 F0 80`. U = 9. Unpacked:
`61 62 63 61 62 63 61 62 63` ("abcabcabc"). CRC-32 0x462D4818, sum16
0x8587. 51 bits are used.

| Bits | Meaning |
|:--|:--|
| `0101011` | Flag byte 0xE0 (H2 symbol 32, PF place 32). |
| `11110100001` | Flag `1`: literal, H1 symbol 97, byte 0x61. |
| `11110100010` | Flag `1`: literal, H1 symbol 98, byte 0x62. |
| `11110100011` | Flag `1`: literal, H1 symbol 99, byte 0x63. |
| `11110` | Flags `0`, `0`: short item. AS = 0 and X is off, so code S-B: selector 4, length 6. |
| `000100` | H2 symbol 2: SD place 2 holds 2, distance 3. Entries 1 and 2 of SD are exchanged. |

### T3. Long match

Packed (24 bytes):
`0E 4C B9 B3 E8 D3 AB 58 36 6D E5 E6 E7 E8 E9 EA 2B D7 D9 DB DC 81 4C 40`.
U = 33. Unpacked: the 20 bytes "0123456789ABCDEFGHIJ", the 12 bytes
"0123456789AB", then ".". CRC-32 0x861280FC, sum16 0x4779. 187 bits are
used.

The stream is: flag byte 0xFF (H2 symbol 1, `00001`), eight literals; flag
byte 0xFF again (now at PF place 0, `00000`), eight literals; flag byte
0xF6 (H2 symbol 10, `0010101`), four literals, and then:

| Bits | Meaning |
|:--|:--|
| | Flag bits `0`, `1`: after 20 literals LW is 224 and MW is 32, a match is not expected, so `01` is a long match. MW becomes 48. |
| `01` | AN = 0: unary length code, value 1. |
| `0000` | AD = 0: code H0, symbol 0. PD place 0 holds 0: high part 0. |
| `0010100` | Low 7 bits: 20. Distance 20, which is at most 256, so the length is 1 + 3 + 8 = 12. |
| | Flag bit `1`: MW (48) is still not greater than LW (224): literal. |
| `1100010` | H1 symbol 46: byte 0x2E. |

### T4. Recent distance, repeats and the repeat bit

Packed (19 bytes):
`0F A1 F4 5E 8F D2 7A 5F 4D 65 67 EB D2 54 70 E7 AF 20 00`. U = 27.
Unpacked: "abcdefghabcdXfghabcdXfghaaa". CRC-32 0x35D3E988, sum16 0x17AD.
146 bits are used.

| Bits | Meaning |
|:--|:--|
| `00001` | Flag byte 0xFF. |
| (six codes of 11 bits) | Literals "abcdef" with code H1 (symbols 97..102). |
| `10110010`, `10110011` | Literals "g", "h". AL has reached 13,824 or more, so the code is now H2 (symbols 103, 104). |
| `111101011` | Flag byte 0x20 = `00100000` (H2 symbol 224). |
| `1101` | Flags `00`: short item, S-B selector 2: length 4. |
| `0010010` | H2 symbol 7: SD place 7, distance 8. Output "abcd". AS becomes 2. |
| `10100011` | Flag `1`: literal, H2 symbol 88: "X". |
| `1000` | Flags `00`: S-B selector 10: most recent distance, 8. |
| `01` | L1 value 1: length 1 + 2 = 3 (distance not above 256). Output "fgh". |
| `1100` | Flags `00`: selector 9, repeat the last match (distance 8, length 3): "abc". NR = 1. |
| | First flag bit of the next item: `0`, the last bit of flag byte 0x20. A second flag bit is needed and NF is 0. |
| `111101011` | Flag byte 0x00 (H2 symbol 224 again: the place holds 0x00 now). Second flag bit `0`: short item. |
| `1100` | Selector 9 again: "dXf". NR = 2. |
| `1` | Flags `00`: short item; NR is 2, so one bit is read: 1 = repeat again: "gha". |
| `0` | Flags `00`: short item; the bit is 0: NR = 0, go on to a selector. |
| `0` | S-B selector 0: length 2. |
| `00000` | H2 symbol 0: SD place 0, distance 1. Output "aa". |

### T5. Stored mode

Packed (34 bytes):
`0F 2F 37 3F 47 4F 57 5F 60 3B 7B BB FC 1E 1F 14 D4 D8 27 27 67 A7 E8 28 68 A8 E9 40 03 34 C0 E2 D6 00`.
U = 30. Unpacked: "ABCDEFGHIJKLMNOPQRSTUVWXYABCZ!". CRC-32 0x33A82C67,
sum16 0xB962. 265 bits are used.

Three flag bytes 0xFF (`00001`, `00000`, `00000`) each precede eight
literals, "A" to "X". The 24th literal, "X", is decoded with NL = 23 and
NF = 0, so it turns stored mode on. From there:

| Bits | Meaning |
|:--|:--|
| `10100101` | Stored literal: code H2 (by AL), symbol 90, place 89: "Y". |
| `00000` | H2 symbol 0: escape. |
| `0` | Not "leave": a stored-mode match follows. |
| `0` | Length 3. |
| `00000` | H2 symbol 0: distance high part 0. |
| `11001` | Low 5 bits 25: distance 25. Output "ABC". |
| `10100110` | Stored literal: H2 symbol 91, place 90: "Z". |
| `00000` | Escape. |
| `1` | Leave stored mode; NL = 0. |
| `110001011` | A flag byte is needed: H2 symbol 128, PF place 128 holds 0x80. |
| `0101100` | Flag `1`: literal, H2 symbol 33: "!". |

### T6. The code-set switch

Packed (8 bytes): `89 F7 1E E6 3F FC 1F 74`. U = 5. Unpacked:
`78 79 78 79 7A` ("xyxyz"). CRC-32 0x0D78D070, sum16 0x1D28. 63 bits are
used.

| Bits | Meaning |
|:--|:--|
| `10001001` | Flag byte 0xC2 = `11000010` (H2 symbol 62). |
| `11110111000`, `11110111001` | Literals "x", "y" (H1 symbols 120, 121). |
| `1000` | Flags `00`: S-B selector 10. |
| `111111111111` | L1 value 255 with selector 10: the switch. X is now on. No output. |
| `0` | Flags `00`: short item, now read with S-A: selector 0, length 2. |
| `00001` | H2 symbol 1: SD place 1, distance 2. Output "xy". |
| `11110111010` | Flag `1`: literal "z". |

### T7. Regrouping of the literal table

Packed (110 bytes):

    0F 28 00 00 00 00 00 00 00 00 00 00 00 00 00 00
    00 02 10 84 21 08 42 10 84 21 08 42 10 84 21 08
    42 10 84 21 08 42 10 84 21 08 42 10 84 21 08 42
    10 84 21 08 42 10 84 21 08 42 10 84 21 08 42 10
    84 21 08 42 10 84 21 08 42 10 84 21 08 42 10 84
    21 08 42 10 84 21 08 42 10 84 21 08 42 10 84 21
    08 42 10 84 21 08 42 10 87 D2 FA 1E 59 44

U = 169. Unpacked: 164 bytes 0x41 followed by 5 bytes 0x64. CRC-32
0xD2F87959, sum16 0xF7F8.

The stream is 24 ordinary literals 0x41 (the first as H1 symbol 65, the
others as H1 symbol 0), which turn stored mode on; 140 stored literals 0x41
(H1 symbol 1 each, `00001`); and five stored literals 0x64, coded as H1
symbols 101, 97, 65, 33 and 2, that is places 100, 96, 64, 32 and 1. The
places of the last five are those explained in the second example of
section 6.1; a decoder that does not regroup PL when the count of 0x41
reaches 161 fails on this vector.

### T8. Distance beyond the start of the data (must be rejected)

Packed (7 bytes): `83 F4 3E 8B 44 F4 60`. Declared U = 7.

| Bits | Meaning |
|:--|:--|
| `10000011` | Flag byte 0xC8 = `11001000` (H2 symbol 56). |
| `11110100001`, `11110100010` | Literals "a", "b". |
| `1101` | Flags `00`: S-B selector 2, length 4. |
| `000100` | H2 symbol 2: distance 3, but only 2 bytes exist. |

Required result: **corrupt input** (D4). The reference implementation
(`unar`) instead outputs `61 62 00 61 62 00 63`.

### T9. Truncated input

The stream of T2 without its last two bytes, `57 E8 7D 17 A3`, with U = 9:
**truncated input** (the selector code is cut off). `unar` fails likewise.
The whole stream of T2 with U = 12 is also truncated input: after the nine
bytes, decoding continues into the padding and the bits run out.

### T10. U ends inside a match

The stream of T2 with U = 5: the result is `61 62 63 61 62` (D5). `unar`
gives the same.

### T11. A solid pair

First file, packed (9 bytes): `1B D4 7A 5F 58 2F 5F 27 87`, U = 12,
unpacked "hello hello " (with the trailing space). CRC-32 0x9FB1A373, sum16
0xE1C8.

| Bits | Meaning |
|:--|:--|
| `000110` | Flag byte 0xFC (H2 symbol 4). |
| `11110101000` | Literal "h" (H1 symbol 104); it moves to PL place 0. |
| `11110100101` | Literal "e" (symbol 101); to place 1. |
| `11110101100` | Literal "l" (symbol 108); to place 2. |
| `00010` | Literal "l" again: H1 symbol 2. It moves to place 0 with count 2; "h" goes to place 2. |
| `11110101111` | Literal "o" (symbol 111). |
| `100100` | Literal " " (symbol 32). |
| `11110` | Flags `00`: S-B selector 4, length 6. |
| `000111` | H2 symbol 5: SD place 5, distance 6. SD entries 4 and 5 are exchanged. |

Second file, continuing the chain, packed (5 bytes): `F3 F0 D2 CA 08`,
U = 11, unpacked "hello!hellh". CRC-32 0x5415E046, sum16 0x70F6.

| Bits | Meaning |
|:--|:--|
| `111100111` | NF was reset to 0: a flag byte is fetched. H2 symbol 220; PF place 220 holds 0x24 = `00100100`. |
| `1110` | Flags `00`: S-B selector 3, length 5. |
| `000110` | H2 symbol 4: SD place 4 now holds 5 (exchanged in the first file): distance 6. The source is in the first file's output: "hello". |
| `100101` | Flag `1`: literal. H1 symbol 33: "!". |
| `10010` | Flags `00`: S-B selector 11: second most recent distance. The recorded distances are 6 (first file) and 6 (this file): distance 6. |
| `100` | L1 value 2: length 4. Output "hell". |
| `00010` | Flag `1`: literal, H1 symbol 2: PL place 2 holds "h" (left there by the first file). |

Decoded with fresh chain state instead, the second stream yields
`00 00 00 00 00 21 00 00 00 00 02` under the reference implementation's
lenient rules and corrupt input (a distance beyond the start) under this
specification. The complete RAR4-format and old-container archives built
from this pair are given in `old-container.md` section 9.

### 13.3 Real data

`wrar155.rar` (the repository's sample collection; a solid RAR4-format
archive with two files, both version 15, method 0x35):

| File | Packed bytes | U | CRC-32 | Bits used | Items |
|:--|--:|--:|:--|--:|:--|
| `WINRAR.EXE` (starts the chain) | 79,835 | 182,528 | 0xDDB4B9DD | 638,673 | 37,460 literals; 9,565 long; 7,302 short; 2,410 recent-distance; 581 selector-9 repeats; 227 repeat bits; 505 far; 48 stored runs with 60 matches; 1 switch |
| `RAR.HLP` (continues it) | 9,275 | 20,272 | 0x3F1276DF | 74,198 | 3,300 literals; 1,552 long; 925 short; 193 recent-distance; 54 selector-9 repeats; 7 repeat bits; 46 far; 7 stored runs |

`RAR.HLP` contains far matches but no switch item: X was turned on in
`WINRAR.EXE` and carried over.

## 14. Decisions

| # | Section | Decision |
|--:|:--|:--|
| D1 | 3 | A bit requested beyond the packed bytes is truncated input; no padding bits are invented. |
| D2 | 8.3, 8.5 | Symbol 256 of a literal code outside stored mode, and of the short-distance code, is accepted and means place 0, as in the reference implementation. (Symbol 256 as a flag-byte place or a long-distance place is corrupt input, also as in the reference implementation.) |
| D3 | 2 | U = 0 means an empty file; nothing is decoded and no state changes. P = 0 with U greater than 0 is truncated input. |
| D4 | 10 | A match distance of 0, or greater than the number of bytes in the chain's history, is corrupt input. |
| D5 | 10 | A match that would pass U is cut at U; this alone is not an error. |
| D6 | 11.3 | After a member of a chain that could not be decoded, no later member of that chain is decoded or delivered. A bare checksum mismatch does not stop the chain. |
| D7 | 12 | U greater than 2,136 × P may be rejected before decoding. |

## 15. Observed implementation gaps

Faults and leniencies of the reference implementation. They explain why
rules exist; none is to be imitated.

1. **No distance check.** Matches may reach before the start of the data;
   the zero-filled window is copied (T8). In a long-running process a
   window reused between chains without clearing would leak earlier
   contents. Specified instead: D4.
2. **Chain boundaries are tested for equality only.** The reference
   implementation switches to the next file's packed bytes when the output
   position *equals* the end of the current file. A crafted match that
   steps over that position makes it continue reading the old file's bits
   for the new file's output. Specified instead: D5 (cut at U), then fresh
   file state.
3. **Stored files inside a solid chain** are handed to the decoder as if
   they were packed streams when a later member needs the chain (the list
   of chain members does not look at the method). Whether genuine archives
   of this generation can contain such members is unknown
   (`old-container.md` section 8).
4. **Symbol 256** is silently folded to 0 in two places and rejected in
   two others (D2 keeps this, since it is harmless).
5. **The 16-bit header checksum and the old container** are not handled at
   all: the reference implementation refuses `RE~^` archives outright
   (`old-container.md` section 10).

## 16. Open points

None of them blocks an implementation.

- What a genuine unpacker does with symbol 256 (D2); never observed.
- Stored members inside solid chains (section 11.2).

## Clean-room feedback

Notes from the implementation side (clean room). They record where this
document was found unclear or inexact; they describe behaviour in prose
and were not written from any decoder's source.

1. **Section 12, R3 and D7: the bound and its justification do not
   match.** "At most 266 bytes for more than two bits" gives at most
   133 bytes per packed bit, that is 1,064 × P. The constant 2,136 is
   267 × 8, about twice that. It is kept as it stands: as an optional
   pre-check it only has to be an upper bound, and a looser one rejects
   nothing a tighter one would accept. The implementation uses 2,136 and
   says in its comment that the value is deliberately loose. Both figures
   are far above the largest real ratio the section reports (438).
