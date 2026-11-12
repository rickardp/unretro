# LHA compression methods

Format specification for clean-room implementation. This document describes
how the packed bytes of one archive entry are turned into its unpacked
bytes, for every method in scope:

| Method identifier | Common description | Section |
|---|---|---|
| "-lh0-", "-lz4-" | stored | 3 |
| "-lh4-", "-lh5-", "-lh6-", "-lh7-" | LZ77 with static Huffman codes sent per block | 4 |
| "-lh1-" | LZ77 with an adaptive Huffman code | 5 |
| "-lzs-" | LArc, bit-oriented LZ77 | 6 |
| "-lz5-" | LArc, byte-oriented LZ77 | 7 |
| "-lhd-" | directory marker, no data | `container.md` section 6.8 |

How the method identifier, the two lengths and the CRC-16 are found in an
archive is the business of [`container.md`](container.md).
[`README.md`](README.md) is the index of the document set and holds the
glossary and the numbered list of decisions.

## 1. How to read this document

### 1.1 Evidence labels

The labels are those of `container.md` section 1.1. In short:

- **CONFIRMED (real data)**: checked on the one genuine archive available
  (one "-lh5-" stream, about 250 KB packed expanding to about 550 KB in some
  twenty blocks, match lengths up to 256, distances up to 8,191). A throwaway
  decoder that follows this document literally, and treats everything this
  document calls an error as an error, reproduced the stored CRC-16.
- **CONFIRMED (crafted)**: streams were built bit by bit from the rules in
  this document and given to independent decoders used as black boxes: the
  **reference implementation** (the third-party LHA library, version 0.6.2,
  which the new implementation replaces), **`unar`** 1.10.7 and **`7z`**
  (p7zip 17.05). `7z` decodes only the stored and static Huffman methods;
  `unar` does not know "-lzs-". Besides the small crafted streams, a
  483,352-byte test file (text, random bytes, long runs) was compressed
  with encoders written from this document, in many blocks, and expanded
  correctly by every black box that supports the method: "-lh4-" to "-lh7-"
  by all three, "-lh1-" (ten rebuilds of the adaptive code) by the
  reference implementation and `unar`, "-lz5-" by the same two, "-lzs-" by
  the reference implementation alone.
- **PUBLISHED**: from public documentation or the published source of other
  LHA tools (`README.md` section 7).
- **INFERRED**: deduced, or supported by one source only.
- **DECISION n**: a choice of this specification, listed with its reason in
  `README.md` section 5. Decisions concern malformed or unusual input; the
  one exception that affects well-formed streams is the initial window of
  the static Huffman methods (section 4.9).

### 1.2 Terms

- **Packed bytes**: the P bytes stored in the archive for one entry.
  **Packed stream**: the same bytes looked at as a sequence of bits.
- **Unpacked bytes**, **output**: what the decoder produces. **U** is the
  unpacked length declared in the header. U is an input to every decoder:
  none of these formats marks its own end.
- **Decoder**: the component that turns the packed bytes of one entry into
  its unpacked bytes for one method. The **container layer** calls it and
  afterwards verifies the CRC-16 (`container.md` section 3.2).
- **Window**: the most recent W unpacked bytes, which copy items refer
  back to. Before any output exists the window has defined **initial
  contents**, different per method.
- **Literal**: an item that produces one byte given in the stream.
  **Copy item**, **match**: an item that produces *length* bytes by
  repeating bytes that are already in the window.
- **Distance**: for the Huffman methods, how far back a copy starts: a
  distance of 1 means "the byte produced last".
- **Symbol**: one value decoded from a prefix code.

### 1.3 Numbers and bit strings

Hexadecimal bytes are written in code style (`E6`). Bit strings are written
in code style in the order the bits are read (`0010` means: first a 0, then
a 0, then a 1, then a 0).

## 2. Things common to all methods

### 2.1 The contract between container layer and decoder

1. The decoder receives the method, the P packed bytes and U.
2. If U = 0 the decoder is not called; the output is empty.
3. Otherwise the decoder produces exactly U bytes or reports an error. It
   stops as soon as U bytes exist (section 2.4).
4. The decoder never reads beyond the P packed bytes. The byte after them
   belongs to the next header.
5. The container layer verifies the CRC-16 of the U bytes. No method has a
   checksum of its own.
6. Each entry is decoded independently, starting from the initial state of
   its method.

### 2.2 Bit order

"-lh1-", "-lh4-" to "-lh7-" and "-lzs-" read the packed bytes as a stream
of bits, **most significant bit first**: bit position *p* of the stream is
bit (7 − (*p* mod 8)) of byte (*p* div 8), where bit 7 of a byte has the
value 128. When a field of *n* bits is read as a number, the first bit read
is the most significant bit of the number.

Example: the bytes `00 05 30` begin with the 16-bit field 5, followed by
the 5-bit field `00110` = 6.

A field of 0 bits has the value 0 and consumes nothing. The widest field in
any method is 16 bits.

The stream has exactly 8 × P bits. **Reading a bit at or beyond position
8 × P is an error**: the entry is truncated or corrupt (**DECISION 31**).
Encoders pad the last byte with zero bits; the decoder never looks at the
padding because it stops at U bytes. (A decoder that looks ahead in the
stream for speed may treat the missing bits as zeros internally, but must
report the error if a symbol or field actually consumes one of them.)

"-lz5-" is byte-oriented and has its own rule (section 7).

### 2.3 Copying from the window

All compressed methods copy **byte by byte, in increasing order**: each
byte copied is appended to the output and becomes part of the window before
the next byte is copied. A copy may therefore overlap its own output. With
a distance of 1 and a length of 5, the last byte is repeated five times;
with a distance of 2 and a length of 5 after "AB", the result is "ABABA".

A copy may refer to a position before the first byte of the output. Those
positions hold the **initial contents** of the window (sections 4.9, 5.6,
6.1, 7.2). This is not an error: genuine encoders rely on it.

### 2.4 How decoding ends

- Decoding ends when U bytes have been produced. There is no end marker in
  any method.
- If a copy item would produce more than the bytes still missing, only the
  missing bytes are produced and decoding ends (**DECISION 32**; CONFIRMED
  (crafted) for all methods and all black boxes that support them).
- Packed bits and bytes left unread are ignored and are not an error
  (**DECISION 32**; the reference implementation and `unar` agree; `7z`
  reports an error when a static Huffman stream is followed by spare bytes,
  which must not be imitated: nothing in the format forbids padding after
  the last item).
- If the packed stream ends first, the entry is truncated or corrupt.
  Output produced so far is discarded.

### 2.5 Output cap

U is untrusted. The decoder must not reserve U bytes in advance. It grows
its output as bytes are produced and stops with "output limit exceeded"
when a caller-supplied limit is passed. Section 8 lists how much output a
given P can legitimately produce per method; U above that bound can be
rejected as corrupt before decoding.

## 3. Stored entries ("-lh0-", "-lz4-")

The unpacked bytes are the packed bytes.

- If P = U: the output is the P packed bytes.
- If P > U: the output is the first U packed bytes (**DECISION 33**).
- If P < U: the entry is corrupt (truncated).

The CRC-16 is verified as for every method.

Evidence: CONFIRMED (crafted) with all three black boxes for P = U. For P
above U the reference implementation and `unar` return the first U bytes
and `7z` returns all P bytes; for P below U the reference implementation
reports an error, the other two return P bytes.

## 4. Static Huffman methods ("-lh4-", "-lh5-", "-lh6-", "-lh7-")

One format with two parameter sets. It is the format of nearly all LHA
archives.

### 4.1 Parameters

| Method | Nominal window of the encoder | Window W the decoder allows | Width of the count field of the distance table | Largest number of distance codes |
|---|--:|--:|--:|--:|
| "-lh4-" | 4,096 | 8,192 | 4 bits | 14 |
| "-lh5-" | 8,192 | 8,192 | 4 bits | 14 |
| "-lh6-" | 32,768 | 65,536 | 5 bits | 17 |
| "-lh7-" | 65,536 | 65,536 | 5 bits | 17 |

"-lh4-" is decoded exactly like "-lh5-" and "-lh6-" exactly like "-lh7-"
(**DECISION 24**). A well-formed stream never uses a distance above the
nominal window of its encoder, so the larger allowance changes nothing for
genuine data.

Constants shared by all four:

| Quantity | Value |
|---|---|
| Symbols of the main code (table C) | 510: values 0 to 509 |
| Symbols of the length-of-codes code (table T) | 19: values 0 to 18 |
| Longest code in any table | 16 bits |
| Match lengths | 3 to 256 |
| Largest block | 65,535 symbols |

### 4.2 Stream structure

The stream is a sequence of **blocks**. Each block is:

| Order | Part | Size |
|--:|---|---|
| 1 | block size B: the number of main-code symbols in the block | 16 bits |
| 2 | table T: the code used to transmit table C | section 4.4 |
| 3 | table C: the main code | section 4.5 |
| 4 | table P: the distance code | section 4.6 |
| 5 | B symbols of the main code, each match symbol followed by its distance | sections 4.7, 4.8 |

There is no alignment between the parts or between blocks: everything is
one continuous bit stream. A new block begins when the previous block's B
symbols have all been decoded **and** more output is still needed. The
tables of a new block replace those of the previous one completely; the
window carries over.

**B = 0 is an error** (**DECISION 25**). Evidence: `unar` and `7z` reject
it, the reference implementation reads the three tables and then expects
another block, and one published tool treats it as 65,536. No encoder
writes it.

### 4.3 Prefix codes

All three tables are prefix codes described by a list of **code lengths**,
one per symbol, where 0 means "symbol not used".

**Canonical assignment.** Codes are assigned in order of increasing length
and, among codes of the same length, in order of increasing symbol value.
The first code is all zeros. Each following code is the previous code plus
one (as a binary number of the previous code's length), extended with zero
bits on the right if its length is greater.

Example: lengths 3, 3, 2, 2, 2 for the symbols 65, 66, 67, 68, 259. The
order is 67, 68, 259 (length 2) and then 65, 66 (length 3):

| Symbol | Length | Code |
|--:|--:|---|
| 67 | 2 | `00` |
| 68 | 2 | `01` |
| 259 | 2 | `10` |
| 65 | 3 | `110` |
| 66 | 3 | `111` |

**Decoding** reads bits one at a time, first bit first, until the bits read
form one of the codes.

**Validity** (**DECISION 28**). Let the used lengths be l(1), ..., l(k).
The code is valid exactly when the sum of 2^(−l(i)) over all used symbols
equals 1. In other words every possible bit sequence leads to a symbol. A
table is rejected as corrupt when

- the sum is below 1 (incomplete code; this includes "a single symbol of
  length 1" and "no symbol used at all"),
- the sum is above 1 (over-subscribed code),
- or any length is above 16 (**DECISION 29**).

The test is made when the table has been read, whether or not the block
goes on to use the table.

Evidence: the canonical order is CONFIRMED (real data) and CONFIRMED
(crafted, including a code using all lengths from 1 to 16). For incomplete
codes the reference implementation and `7z` report an error and `unar`
decodes with the codes that exist; over-subscribed codes are rejected by
all three.

**The single-symbol form.** Each table can instead be sent as "count 0"
followed by one symbol value. Then the table has exactly one symbol whose
code has **length zero**: decoding that table consumes no bits and always
yields that symbol. This is how encoders represent a table with fewer than
two used symbols. It is not subject to the validity sum.

### 4.4 Table T

Table T has 19 symbols. It is only used to read table C of the same block.

1. Read *n*, 5 bits: the number of lengths that follow.
2. If *n* = 0: read 5 more bits, *c*. Table T is the single-symbol form
   with symbol *c*. *c* must be at most 18 (**DECISION 27**). Done.
3. If *n* > 19: corrupt (**DECISION 26**).
4. Otherwise read the lengths of symbols 0, 1, 2, ... in order, each as a
   **length field** (below), with one interruption: **immediately after
   the length of symbol 2 has been read**, read a 2-bit number *z*; the
   symbols 3 to 3 + *z* − 1 get length 0 without anything being read for
   them, and reading continues with symbol 3 + *z*. Reading stops when the
   next symbol index is *n* or more.
5. Symbols from *n* to 18 have length 0. Apply the validity test of 4.3.

Details of step 4 that matter (**DECISION 26**):

- The 2-bit number is present exactly when *n* is 3 or more. For *n* = 1
  and *n* = 2 the table ends before symbol 2 has been passed and **no 2-bit
  number is read**.
- If 3 + *z* is greater than *n*, the skipped symbols beyond *n* − 1 are
  simply not part of the table. This is accepted. (For *n* = 3 the 2-bit
  number is read and has no effect at all.)

**Length field.** Read 3 bits as a number *v*. If *v* is less than 7 the
length is *v*. If *v* is 7, read further single bits until a 0 bit has been
read; the length is 7 plus the number of 1 bits read before that 0. (The 0
bit is consumed.) A length above 16 is corrupt; a decoder stops reading 1
bits as soon as the length would exceed 16.

| Bits | Length |
|---|--:|
| `000` to `110` | 0 to 6 |
| `1110` | 7 |
| `11110` | 8 |
| `111110` | 9 |
| ... | ... |
| `111` followed by nine 1 bits and a `0` | 16 |

Evidence: CONFIRMED (real data) for the general form. CONFIRMED (crafted):
*n* = 2 without the 2-bit number and *n* = 3 with *z* = 2 are decoded by
`unar` and `7z` and match the published encoders; the reference
implementation wrongly reads a 2-bit number for *n* = 1 and 2 and rejects
3 + *z* > *n*. *n* = 20 is accepted by the reference implementation and
`unar` and rejected by `7z`; only 19 symbols have a meaning.

### 4.5 Table C

Table C has 510 symbols: 0 to 255 are literals, 256 to 509 are match
lengths (section 4.7).

1. Read *n*, 9 bits.
2. If *n* = 0: read 9 more bits, *c*. Table C is the single-symbol form
   with symbol *c*. *c* must be at most 509 (**DECISION 27**). Done.
3. If *n* > 510: corrupt (**DECISION 26**).
4. Set an index *i* to 0. While *i* < *n*, decode one symbol *t* with
   table T and act on it:

   | *t* | Meaning | Extra bits read after the symbol |
   |--:|---|---|
   | 0 | one symbol of length 0 | none |
   | 1 | a run of 3 to 18 symbols of length 0 | 4 bits *v*; run = *v* + 3 |
   | 2 | a run of 20 to 531 symbols of length 0 | 9 bits *v*; run = *v* + 20 |
   | 3 to 18 | symbol *i* has length *t* − 2 (1 to 16) | none |

   A length advances *i* by one; a run advances it by the run length. **A
   run that would carry *i* beyond *n* is corrupt** (**DECISION 26**).
   (There is no code for a run of exactly 19: encoders write a single zero
   followed by a run of 18.)
5. Symbols from *n* to 509 have length 0. Apply the validity test of 4.3.

If table T is in the single-symbol form, step 4 consumes no bits for the
symbols themselves; this terminates because *i* grows in every round.

Evidence: CONFIRMED (real data). CONFIRMED (crafted): *n* = 510 accepted by
all, *n* = 511 rejected by the reference implementation and `7z`; an
overrunning run is rejected by `unar` and `7z` and clipped by the reference
implementation.

### 4.6 Table P

Table P has as many symbols as the method allows distance codes (14 or 17,
section 4.1). The width of its count field, *w*, is 4 or 5 bits.

1. Read *n*, *w* bits.
2. If *n* = 0: read *w* more bits, *c*. Table P is the single-symbol form
   with symbol *c*. *c* must be less than the largest number of distance
   codes (at most 13, or at most 16) (**DECISION 27**). Done.
3. If *n* is greater than the largest number of distance codes: corrupt
   (**DECISION 26**).
4. Read *n* length fields (section 4.4) for the symbols 0 to *n* − 1.
   There is no 2-bit interruption in this table.
5. The remaining symbols have length 0. Apply the validity test of 4.3.

A block that contains no match at all still carries a table P; encoders
send the single-symbol form with symbol 0.

Evidence: CONFIRMED (real data). CONFIRMED (crafted): for "-lh5-", *n* = 14
is accepted and *n* = 15 rejected by the reference implementation.

### 4.7 Symbols of the main code

Decode one symbol *s* with table C.

- *s* from 0 to 255: a **literal**. The byte *s* is appended to the
  output.
- *s* from 256 to 509: a **match** of length *s* − 253, that is 3 for 256
  and 256 for 509. A distance follows (section 4.8).

Each symbol, literal or match, counts as one of the block's B symbols. The
distance does not count separately.

### 4.8 Distances

Decode one symbol *q* with table P. Then:

| *q* | Extra bits | Offset |
|--:|--:|---|
| 0 | 0 | 0 |
| 1 | 0 | 1 |
| 2 or more | *q* − 1 | 2^(*q*−1) plus the value of the extra bits |

So *q* = 2 gives offsets 2 and 3, *q* = 3 gives 4 to 7, *q* = 13 gives
4,096 to 8,191, *q* = 16 gives 32,768 to 65,535. The extra bits are read as
one field, first bit most significant.

The **distance is offset + 1**. The copy starts *distance* bytes before the
current end of the output and proceeds as in section 2.3. Distances run
from 1 to W.

With the limits of sections 4.1 and 4.6 the offset can never reach W, so no
separate range check is needed; an implementation may still assert it.

### 4.9 Initial window contents

Before the first byte of output, the window holds **W bytes of value `20`**
(ASCII space). A match whose distance is greater than the number of bytes
produced so far reads these spaces for the part that lies before the start
of the output (**DECISION 23**).

This is the one place where the black boxes disagree on streams that
genuine encoders can produce:

| Reader | Bytes "before the start" |
|---|---|
| reference implementation | `20` |
| two published tools, including the format's Unix implementation | `20` |
| `unar` | `00` |
| `7z` | error |

Encoders of the original family initialise their own window with spaces and
may emit such matches for files that begin with spaces.

**Zero-window retry** (**DECISION 23**). One published tool states that
some versions of it (from 2003 onward, until corrected) wrote archives that
need `00` instead of `20` here, and offers a switch to read them. To read
those archives without a switch:

- The decoder records whether any match read at least one byte from before
  the start of the output.
- If decoding succeeds, the CRC-16 does **not** match, and that record is
  set, the container layer decodes the entry once more with the initial
  window filled with `00`. If the CRC-16 of the second result matches, the
  second result is delivered and a finding "zero-filled initial window" is
  recorded. Otherwise the entry is corrupt.
- No retry takes place in any other situation, and never more than one.

### 4.10 Worked example 1: literals only

Nine packed bytes, U = 4, method "-lh5-":

```
00 04 20 04 24 30 b7 00 50
```

| Bit position | Bits | Meaning |
|--:|---|---|
| 0 | `0000000000000100` | B = 4 |
| 16 | `00100` | table T: *n* = 4 |
| 21 | `000` | length of T symbol 0 = 0 |
| 24 | `000` | length of T symbol 1 = 0 |
| 27 | `001` | length of T symbol 2 = 1 |
| 30 | `00` | *z* = 0: nothing skipped |
| 32 | `001` | length of T symbol 3 = 1 |
| | | T codes: symbol 2 is `0`, symbol 3 is `1`. Sum: 1/2 + 1/2 = 1 |
| 35 | `001000011` | table C: *n* = 67 |
| 44 | `0` | T symbol 2: a run of 20 or more zeros |
| 45 | `000101101` | *v* = 45: run of 65; *i* goes from 0 to 65 |
| 54 | `1` | T symbol 3: C symbol 65 has length 1 |
| 55 | `1` | T symbol 3: C symbol 66 has length 1; *i* = 67 = *n* |
| | | C codes: 65 ("A") is `0`, 66 ("B") is `1` |
| 56 | `0000` | table P: *n* = 0 |
| 60 | `0000` | single symbol 0 |
| 64 | `0` `1` `0` `1` | four symbols: 65, 66, 65, 66 |
| 68 | `0000` | padding, never read |

Output: `41 42 41 42` ("ABAB"), CRC-16 computed by the container layer.
Accepted with this output by all three black boxes.

### 4.11 Worked example 2: a match, the 2-bit number, single-symbol P

Thirteen packed bytes, U = 10, method "-lh5-":

```
00 05 30 09 2a 09 0b 7c 95 40 2d c8 80
```

| Bit position | Bits | Meaning |
|--:|---|---|
| 0 | `0000000000000101` | B = 5 |
| 16 | `00110` | table T: *n* = 6 |
| 21 | `000` | length of T symbol 0 = 0 |
| 24 | `000` | length of T symbol 1 = 0 |
| 27 | `010` | length of T symbol 2 = 2 |
| 30 | `01` | *z* = 1: T symbol 3 gets length 0; continue with symbol 4 |
| 32 | `001` | length of T symbol 4 = 1 |
| 35 | `010` | length of T symbol 5 = 2 |
| | | T codes: symbol 4 is `0`, symbol 2 is `10`, symbol 5 is `11` |
| 38 | `100000100` | table C: *n* = 260 |
| 47 | `10` | T symbol 2: run of 20 or more |
| 49 | `000101101` | *v* = 45: run of 65; *i* = 65 |
| 58 | `11` | T symbol 5: C symbol 65 has length 3 |
| 60 | `11` | T symbol 5: C symbol 66 has length 3 |
| 62 | `0` | T symbol 4: C symbol 67 has length 2 |
| 63 | `0` | T symbol 4: C symbol 68 has length 2; *i* = 69 |
| 64 | `10` | T symbol 2: run of 20 or more |
| 66 | `010101010` | *v* = 170: run of 190; *i* = 259 |
| 75 | `0` | T symbol 4: C symbol 259 has length 2; *i* = 260 = *n* |
| | | C codes: the table of section 4.3 |
| 76 | `0000` | table P: *n* = 0 |
| 80 | `0010` | single symbol 2: every distance has *q* = 2 |
| 84 | `110` | C symbol 65: literal "A" |
| 87 | `111` | C symbol 66: literal "B" |
| 90 | `00` | C symbol 67: literal "C" |
| 92 | `10` | C symbol 259: match of length 259 − 253 = 6 |
| 94 | (none) | P symbol 2, zero bits |
| 94 | `0` | one extra bit, value 0: offset 2 + 0 = 2, distance 3 |
| 95 | `01` | C symbol 68: literal "D" |
| 97 | | end: 10 bytes produced; 7 padding bits unread |

The match copies six bytes starting three back: "ABC" then, overlapping its
own output, "ABC" again. Output: "ABCABCABCD"
(`41 42 43 41 42 43 41 42 43 44`), CRC-16 `55A1`. Accepted with this output
by all three black boxes.

### 4.12 Further test vectors

Each line is a complete packed stream for "-lh5-" unless stated. "Spaces"
means bytes `20` taken from the initial window.

| Packed bytes | U | Expected result | Covers |
|---|--:|---|---|
| `00 05 00 00 04 10 00` | 5 | "AAAAA" | T, C and P all in single-symbol form; C symbol 65; no bits per symbol |
| `00 02 20 04 24 30 b7 00 40 00 88 01 09 0c 2d c0 20` | 4 | "ABBA" | two blocks of two symbols, tables repeated |
| `00 03 11 20 02 10 00` | 3 | "BBB" | T with *n* = 2 and no 2-bit number |
| `00 03 19 22 00 10 80 00` | 3 | "BBB" | T with *n* = 3 and *z* = 2 (skip beyond *n*) |
| `00 02 00 00 10 00 00` | 6 | six spaces | C single symbol 256, P single symbol 0: two matches of length 3 at distance 1, entirely from the initial window |
| `00 03 28 08 46 07 0b 52 ad a1 00 97 80` | 10 | five spaces, "A", four spaces | first item is a match of length 5 at distance 1; then "A"; then a match of length 4 at distance 7 |
| `00 00 20 04 24 30 b7 00 00 04 20 04 24 30 b7 00 50` | 4 | **corrupt** | B = 0 |
| `00 04 a0 04 20 00 00 00 00 00 04 30 b7 00 50` | 4 | **corrupt** | T with *n* = 20 |
| `00 04 20 04 24 20 b6 00 00` | 4 | **corrupt** | table C with a single symbol of length 1 (incomplete) |
| `00 04 20 04 24 40 b7 80 28` | 4 | **corrupt** | table C with three symbols of length 1 (over-subscribed) |
| `00 01 00 00 1f f0 00` | 258 | **corrupt** | C single symbol 511 |
| `00 01 00 00 10 00 f0 00 40` | 3 | **corrupt** | P single symbol 15 for "-lh5-" |
| the nine bytes of example 1 without the last two | 4 | **corrupt** (truncated) | stream ends inside the tables |

How the black boxes treat the valid lines: lines 1 and 2 give the expected
result in all three; lines 3 and 4 in `unar` and `7z` (the reference
implementation rejects them, section 9); lines 5 and 6 in the reference
implementation (`unar` returns `00` bytes instead of spaces and `7z` an
error, section 4.9). The corrupt lines follow from the decisions; how each
black box reacts to them is recorded in the sections above. A decoder
written literally from this document gives every result in the table.

## 5. Adaptive Huffman method ("-lh1-")

The method of LHarc 1.x. Still common in Amiga and MS-DOS archives from
before 1991.

### 5.1 Parameters

| Quantity | Value |
|---|---|
| Window W | 4,096 bytes |
| Symbols of the adaptive code | 314: values 0 to 313 |
| Literals | symbols 0 to 255 |
| Match lengths | symbols 256 to 313 mean lengths 3 to 60 (length = symbol − 253) |
| Distances | 1 to 4,096, sent as a 12-bit offset = distance − 1 |
| Blocks, tables | none: one continuous stream of items |

### 5.2 Stream structure

The stream is a sequence of items, most significant bit first (section
2.2). Each item is:

1. one symbol of the adaptive code (section 5.3). After decoding it, the
   code is updated for that symbol (section 5.4);
2. only if the symbol is 256 or more: a distance. The upper six bits of the
   offset are sent with a fixed prefix code (section 5.5), followed by the
   lower six bits as a plain 6-bit field. The offset is upper × 64 + lower;
   the distance is offset + 1.

A literal appends its byte; a match copies as in section 2.3. The distance
bits do not touch the adaptive code.

### 5.3 The adaptive code: structure and initial state

The code is a binary tree with 314 leaves and 313 internal nodes, 627 nodes
in all. The nodes are kept in a **list with positions 0 to 626**. Each
position holds:

- a **weight** (a count of how often the node was used; 16 bits suffice),
- and either a **symbol** (the node is a leaf) or the position of its
  **first child** (the node is internal). The two children of an internal
  node always occupy **adjacent positions**: first child at position *k*,
  second child at position *k* + 1.

Two properties hold at all times and define the algorithm:

1. **Order**: weights never decrease along the list. The node at position
   0 has the smallest weight, the root at position 626 the largest.
2. **Sum**: the weight of an internal node is the sum of the weights of its
   two children.

**Initial state.**

- Positions 0 to 313 are leaves. Position *i* holds symbol *i* with weight
  1.
- Positions 314 to 626 are internal. For *j* from 0 to 312, the node at
  position 314 + *j* has its first child at position 2 × *j* (and so its
  second child at 2 × *j* + 1), and its weight is the sum of the weights of
  those two.
- The root is position 626; its children are positions 624 and 625.

Some values for checking an implementation of the initial state: position
314 has weight 2; positions 314 to 470 all have weight 2; position 471 has
weight 4; position 625 has weight 186, position 624 weight 128, and the
root 314.

**Decoding a symbol.** Start at the root. While the current node is
internal: read one bit; move to its first child if the bit is 0, to its
second child if the bit is 1. The symbol of the leaf reached is the result.

In the initial state the symbols 116 to 313 have 8-bit codes and the
symbols 0 to 115 have 9-bit codes. Initial codes of some symbols:

| Symbol | Code |
|--:|---|
| 0 | `110001100` |
| 1 | `110001101` |
| 65 | `111001101` |
| 66 | `111001110` |
| 115 | `111111111` |
| 116 | `00000000` |
| 255 | `10001011` |
| 256 | `10001100` |
| 313 | `11000101` |

### 5.4 The adaptive code: update after each symbol

After each decoded symbol *s* (literal or match length alike):

**Step A, rebuild test.** If the weight of the root is 32,768 or more,
rebuild the whole tree first (section 5.4.1).

**Step B, increment along the path.** Let the *current node* be the leaf
holding *s*. Repeat:

1. Let *w* be the weight of the current node, at position *c*. Find the
   **highest** position *h* whose weight is also *w* (because of the order
   property, positions *c* to *h* then all have weight *w*).
2. If *h* is not *c*, **exchange the nodes at positions *c* and *h***: the
   two positions swap what they hold (symbol, or first-child position).
   Their weights are equal, so the weights need not move. The children of
   an exchanged internal node stay where they are; only which position is
   their parent changes. The current node is now at position *h*.
3. Add 1 to the weight of the current node.
4. If the current node is the root, stop. Otherwise the current node
   becomes its parent, and the steps repeat.

The parent of a node is the internal node whose first-child position is the
node's position or the position just before it. An implementation keeps
whatever index it needs (a parent position per node and a leaf position per
symbol, updated at every exchange and rebuild) to find parents and leaves
without searching.

Notes that follow from the rules and help when testing:

- The node exchanged with is never the current node's own parent or child
  (a parent is always strictly heavier than its children, because every
  weight is at least 1).
- The root is always the highest position of its weight; it is never
  exchanged.
- After step B both properties (order, sum) hold again.
- The root's weight is 314 plus the number of symbols decoded since the
  start or, after a rebuild, the value given below plus the symbols decoded
  since.

#### 5.4.1 Rebuild

When the root's weight has reached 32,768 the tree is rebuilt, before the
increment for the current symbol is applied:

1. **Collect the leaves** in the order of their current positions, from
   position 0 upward. This gives 314 (symbol, weight) pairs in
   non-decreasing weight order.
2. **Halve the weights**, rounding up: each weight *w* becomes (*w* + 1)
   div 2. No weight becomes 0 and the order is preserved.
3. **Form a new list**, starting with these 314 leaves at positions 0 to
   313.
4. **Add the 313 internal nodes one at a time.** For *j* = 0, 1, ..., 312:
   the new node's children are the nodes currently at positions 2 × *j*
   and 2 × *j* + 1 of the new list; its weight *f* is the sum of their
   weights. Insert the new node into the list **directly after the last
   node whose weight is less than or equal to *f***, moving every later
   node up by one position. (The inserted node always lands above position
   2 × *j* + 1, so the children it refers to do not move, and neither do
   the children of nodes inserted earlier.)
5. After the 313th insertion the list has 627 nodes and the root is at
   position 626.

Unlike the initial state, leaves and internal nodes are interleaved after a
rebuild.

The rebuild test uses "32,768 or more"; with these rules the weight is
exactly 32,768 whenever a rebuild happens. The first rebuild happens while
processing the 32,455th symbol of a stream (after 32,454 increments of the
root from 314).

Evidence for 5.3 and 5.4: CONFIRMED (crafted) with the reference
implementation and `unar` on streams with up to ten rebuilds; the method is
also PUBLISHED as part of the classic public-domain description of this
compression scheme.

### 5.5 The fixed distance code

The upper six bits of the offset (a value from 0 to 63) are sent with this
prefix code. It is a canonical code (section 4.3) with one code of length
3, three of length 4, eight of length 5, twelve of length 6, twenty-four of
length 7 and sixteen of length 8, assigned to the values 0 to 63 in order:

| Value | Code | Value | Code | Value | Code | Value | Code |
|--:|---|--:|---|--:|---|--:|---|
| 0 | `000` | 16 | `101000` | 32 | `1101000` | 48 | `11110000` |
| 1 | `0010` | 17 | `101001` | 33 | `1101001` | 49 | `11110001` |
| 2 | `0011` | 18 | `101010` | 34 | `1101010` | 50 | `11110010` |
| 3 | `0100` | 19 | `101011` | 35 | `1101011` | 51 | `11110011` |
| 4 | `01010` | 20 | `101100` | 36 | `1101100` | 52 | `11110100` |
| 5 | `01011` | 21 | `101101` | 37 | `1101101` | 53 | `11110101` |
| 6 | `01100` | 22 | `101110` | 38 | `1101110` | 54 | `11110110` |
| 7 | `01101` | 23 | `101111` | 39 | `1101111` | 55 | `11110111` |
| 8 | `01110` | 24 | `1100000` | 40 | `1110000` | 56 | `11111000` |
| 9 | `01111` | 25 | `1100001` | 41 | `1110001` | 57 | `11111001` |
| 10 | `10000` | 26 | `1100010` | 42 | `1110010` | 58 | `11111010` |
| 11 | `10001` | 27 | `1100011` | 43 | `1110011` | 59 | `11111011` |
| 12 | `100100` | 28 | `1100100` | 44 | `1110100` | 60 | `11111100` |
| 13 | `100101` | 29 | `1100101` | 45 | `1110101` | 61 | `11111101` |
| 14 | `100110` | 30 | `1100110` | 46 | `1110110` | 62 | `11111110` |
| 15 | `100111` | 31 | `1100111` | 47 | `1110111` | 63 | `11111111` |

The code is complete: every bit sequence decodes. After the code come the
six low bits of the offset, most significant first.

Example: `000` `000001` is upper 0, lower 1: offset 1, distance 2.
`11111111` `111111` is offset 4,095, distance 4,096.

Evidence: CONFIRMED (crafted) with the reference implementation and `unar`
(distances across the whole range occur in the large test stream).

### 5.6 Initial window contents

The window initially holds 4,096 bytes of value `20` (**DECISION 30**).
Genuine "-lh1-" encoders do emit matches into this area (for files that
begin with spaces), so this is required for correct output.

Evidence: a first item "match of length 5 at distance 1" yields five spaces
in the reference implementation and in `unar`. A first item "match of
length 60 at distance 4,096" yields sixty `20` bytes in the reference
implementation and sixty `00` bytes in `unar`: the two differ on the 60
ring positions that the first 60 output bytes are about to overwrite, which
only a distance above 4,036 used within the first 60 bytes of output can
reach. An encoder cannot produce such a match (it would refer to data it
has not seen yet), so the difference is unobservable on genuine data; this
specification follows the reference implementation and one published
tool.

### 5.7 Worked example

Seven packed bytes, U = 8:

```
e6 f3 b1 63 40 38 40
```

| Bit position | Bits | Meaning |
|--:|---|---|
| 0 | `111001101` | symbol 65: literal "A" (initial code of 65) |
| 9 | `111001110` | symbol 66: literal "B" |
| 18 | `11000101` | symbol 65 again; its code has changed |
| 26 | `10001101` | symbol 257: match of length 4 |
| 34 | `000` | upper part of the offset: 0 |
| 37 | `000001` | lower part: 1. Offset 1, distance 2 |
| 43 | `1100001` | symbol 65: literal "A", now a 7-bit code |
| 50 | | end: 8 bytes produced; 6 padding bits unread |

Output: "ABA", then four bytes copied from distance 2 ("BABA"), then "A":
"ABABABAA" (`41 42 41 42 41 42 41 41`), CRC-16 `AE94`. Accepted with this
output by the reference implementation and `unar`.

How the code changed, step by step:

- **First "A".** The walk for `111001101` visits positions 626, 625, 623,
  619, 610, 592, 557, 487, 346 and ends at the leaf at position 65. Update:
  the leaf at position 65 has weight 1; the highest position of weight 1 is
  313. Positions 65 and 313 are exchanged: symbol 65 is now at position
  313 and symbol 313 at position 65. The weight at position 313 becomes 2.
  Its parent is position 470 (children 312 and 313), weight 2, which is
  already the highest position of weight 2 (position 471 has weight 4); it
  becomes 3. The walk continues through positions 549, 588, 608, 618, 623,
  625 and 626, none of which needs an exchange; their weights become 7, 11,
  27, 59, 123, 187 and 315.
- **"B".** Symbol 66 is at position 66 with weight 1; the highest position
  of weight 1 is now 312. Exchange; position 312 gets weight 2. Its parent,
  position 470, goes from 3 to 4, and so on up to the root (316).
- **Second "A".** Symbol 65 is at position 313, reached by the code
  `11000101` (the code symbol 313 had at the start). Its weight is 2; the
  highest position of weight 2 is 469, an internal node whose children are
  at 310 and 311. Positions 313 and 469 are exchanged: position 469 now
  holds the leaf for symbol 65, position 313 the internal node. Position
  469 gets weight 3. Its parent is now position 548 (children 468 and 469),
  weight 4 and the highest of its weight; it becomes 5. The walk continues
  to the root (317).
- **Symbol 257** is still where it started (code `10001101`). After its
  update the root has weight 318.
- **Last "A".** Symbol 65 at position 469 has the 7-bit code `1100001`.

### 5.8 Test vectors for the adaptive code

These let an implementation of sections 5.3 and 5.4 be tested without any
packed data. "Update number *i*" means: apply the update of section 5.4 for
the given symbol; updates are numbered from 0.

**Vector 1** (no rebuild). Apply 2,000 updates; update *i* uses the symbol
(7 × *i* + (*i* × *i* mod 13)) mod 314. Afterwards the root has weight
2,314 and these symbols have these codes:

| Symbol | Code |
|--:|---|
| 0 | `110100000` |
| 1 | `00000001` |
| 2 | `00011111` |
| 100 | `110000000` |
| 255 | `101101000` |
| 256 | `00100000` |
| 313 | `00100100` |

**Vector 2** (one rebuild). Update *i* uses the symbol (5 × *i*) mod 7 when
*i* is not a multiple of 3, and (*i* div 3) mod 314 when it is.

After 32,454 updates (numbers 0 to 32,453) the root has weight 32,768, no
rebuild has happened yet, and:

| Symbol | Code |
|--:|---|
| 0 | `1111` |
| 1 | `1110` |
| 2 | `1101` |
| 3 | `010` |
| 6 | `001` |
| 7 | `100001100` |
| 100 | `1011101111` |
| 313 | `1000011010` |

Update number 32,454 triggers the rebuild. After it has completed (rebuild,
then increment), the root has weight 16,473 and:

| Symbol | Code |
|--:|---|
| 0 | `1111` |
| 1 | `1110` |
| 2 | `1101` |
| 3 | `010` |
| 6 | `001` |
| 7 | `1011000101` |
| 100 | `1011101111` |
| 313 | `1000011010` |

Both vectors were produced by the encoder whose output the reference
implementation and `unar` decode correctly across ten rebuilds.

## 6. LArc method "-lzs-"

A plain LZ77 format without entropy coding, from the LArc archiver that
preceded LHarc.

### 6.1 Parameters and initial state

| Quantity | Value |
|---|---|
| Window | a ring of 2,048 positions, numbered 0 to 2,047 |
| Initial contents | every position holds `20` |
| Initial write position | 2,031 (that is, 2,048 − 17) |
| Match lengths | 2 to 17 |
| Match source | an **absolute position** in the ring, not a distance |

Every byte produced, literal or copied, is stored at the write position,
which then advances by one and wraps from 2,047 to 0.

### 6.2 Stream structure

A stream of bits, most significant bit first (section 2.2). Each item
starts with a 1-bit flag:

| Flag | Following fields | Meaning |
|---|---|---|
| `1` | 8 bits | a literal byte |
| `0` | 11 bits *p*, then 4 bits *v* | a match of length *v* + 2 starting at ring position *p* |

A match produces the bytes at ring positions *p*, *p* + 1, ... (wrapping
at 2,048), one at a time, each being appended to the output and stored at
the write position before the next is read. If the source runs into
positions written by the same match, the freshly written bytes are read.

Every value of *p* and *v* is valid; the stream cannot be malformed other
than by ending too early.

### 6.3 Worked example

Eight packed bytes, U = 11:

```
b0 d8 ac 6f de 80 00 00
```

| Bit position | Bits | Meaning |
|--:|---|---|
| 0 | `1` `01100001` | literal `61` "a", stored at position 2,031 |
| 9 | `1` `01100010` | literal "b", stored at 2,032 |
| 18 | `1` `01100011` | literal "c", stored at 2,033 |
| 27 | `0` `11111101111` `0100` | match: *p* = 2,031, length 4 + 2 = 6 |
| 43 | `0` `00000000000` `0000` | match: *p* = 0, length 0 + 2 = 2 |
| 59 | | end: 11 bytes produced; 5 padding bits unread |

The first match reads positions 2,031 to 2,036 while writing positions
2,034 to 2,039: "abc", then the "abc" it has just written. The second match
reads positions 0 and 1, which still hold their initial spaces. Output:
"abcabcabc" and two spaces (`61 62 63 61 62 63 61 62 63 20 20`), CRC-16
`BC23`.

Further vectors (U = 18 each; the first item is the literal "Q", the second
a match of length 17):

| Packed bytes | *p* | Expected output |
|---|--:|---|
| `a8 80 07 80` | 0 | "Q" and 17 spaces |
| `a8 bf 6f 80` | 2,029 | "Q", then "  Q" repeated: `51 20 20 51 20 20 51 ...` (18 bytes) |
| `a8 bf 7f 80` | 2,031 | 18 times "Q" |
| `a8 bf ff 80` | 2,047 | "Q" and 17 spaces (the source wraps from 2,047 to 0) |

Evidence: CONFIRMED (crafted) with the reference implementation only
(`unar` and `7z` do not decode this method); the field order, the widths,
the initial position and the initial contents are in addition PUBLISHED by
two other tools, which agree. A 40,000-byte test file compressed with an
encoder written from this section is expanded correctly by the reference
implementation.

## 7. LArc method "-lz5-"

### 7.1 Parameters

| Quantity | Value |
|---|---|
| Window | a ring of 4,096 positions, numbered 0 to 4,095 |
| Initial write position | 4,078 (that is, 4,096 − 18) |
| Match lengths | 3 to 18 |
| Match source | an absolute position in the ring |

### 7.2 Initial window contents

The ring starts with fixed contents (**DECISION 30** for the last row):

| Positions | Count | Contents |
|---|--:|---|
| 0 to 3,327 | 3,328 | for each byte value *v* from 0 to 255 in order, 13 copies of *v*: positions 13 × *v* to 13 × *v* + 12 hold *v* |
| 3,328 to 3,583 | 256 | the values 0, 1, 2, ..., 255 in ascending order: position 3,328 + *v* holds *v* |
| 3,584 to 3,839 | 256 | the values 255, 254, ..., 0 in descending order: position 3,584 + *k* holds 255 − *k* |
| 3,840 to 3,967 | 128 | `00` |
| 3,968 to 4,077 | 110 | `20` |
| 4,078 to 4,095 | 18 | `00` |

Evidence: every row CONFIRMED (crafted) with the reference implementation
and `unar`, by streams whose single item copies 18 bytes from positions 0,
845, 3,393, 3,584, 3,840, 3,968, 4,075, 4,078 and 4,090. The last row (the
18 positions at and after the initial write position, which the first 18
output bytes overwrite) is `00` in both black boxes and in one published
tool, and `20` in another.

### 7.3 Stream structure

The stream is byte-oriented. It is a sequence of **groups**. A group is one
**flag byte** followed by up to eight items. The flag bits are used from
the **least significant bit upward**: bit 0 belongs to the first item of
the group, bit 7 to the eighth.

| Flag bit | Item bytes | Meaning |
|---|---|---|
| 1 | one byte | a literal |
| 0 | two bytes *b0*, *b1* | a match: position *p* = *b0* + 256 × (upper four bits of *b1*); length = (lower four bits of *b1*) + 3 |

A match copies from ring positions *p*, *p* + 1, ... (wrapping at 4,096)
exactly as in section 6.2. Every byte produced is stored at the write
position, which advances and wraps.

Decoding ends when U bytes exist; the unused flag bits of the last group
and any following bytes are ignored. Needing a flag byte or an item byte
when the P packed bytes are exhausted is an error (truncated).

### 7.4 Worked example

Seven packed bytes, U = 11:

```
0c 61 d0 ee f3 20 20
```

| Offset | Bytes | Meaning |
|--:|---|---|
| 0 | `0C` | flag byte, binary 00001100: items 1 and 2 are matches (bits 0 and 1 are 0), items 3 and 4 are literals (bits 2 and 3 are 1) |
| 1 | `61 D0` | match: *p* = `61` + 256 × `D` = 3,425; length 0 + 3 = 3 |
| 3 | `EE F3` | match: *p* = `EE` + 256 × `F` = 4,078; length 3 + 3 = 6 |
| 5 | `20` | literal space |
| 6 | `20` | literal space |

Position 3,425 lies in the ascending region: 3,425 − 3,328 = 97, so
positions 3,425 to 3,427 hold `61 62 63` ("abc"). These three bytes are
output and stored at positions 4,078 to 4,080. The second match reads from
position 4,078: "abc" and then the "abc" it writes itself. Output:
"abcabcabc" and two spaces, CRC-16 `BC23`. Accepted with this output by the
reference implementation and `unar`.

Further vectors (one group with a single match of length 18; U = 18):

| Packed bytes | *p* | Expected output |
|---|--:|---|
| `00 00 0f` | 0 | thirteen `00`, five `01` |
| `00 4d 3f` | 845 | thirteen `41`, five `42` |
| `00 41 df` | 3,393 | `41 42 43 ... 52` (ascending) |
| `00 00 ef` | 3,584 | `FF FE FD ... EE` (descending) |
| `00 80 ff` | 3,968 | eighteen `20` |
| `00 eb ff` | 4,075 | eighteen `20` (three initial spaces, then the copy reads what it wrote) |
| `00 ee ff` | 4,078 | eighteen `00` |

## 8. Safety requirements for untrusted input

Normative for every decoder.

**Input bounds**

1. No bit or byte is read beyond the P packed bytes (section 2.2). This is
   checked on every read, including the extra bits of distances, the
   unary part of a length field and the item bytes of "-lz5-".
2. A decoder that fetches several bytes at a time for speed must not fetch
   beyond the packed bytes of its entry.

**Table bounds (static Huffman)**

3. The counts are checked before any length is stored: at most 19 for T,
   510 for C, 14 or 17 for P.
4. In table T the index after the 2-bit skip can exceed the count; entries
   at or beyond the count must not be written.
5. In table C a zero run is compared with the remaining count before it is
   applied.
6. The unary part of a length field is cut off at length 16. Without the
   cut, a long run of 1 bits would overflow a small counter.
7. Single-symbol values are range-checked (18, 509, 13 or 16).
8. Every transmitted table passes the validity sum of section 4.3 before
   any symbol is decoded with it. With a complete code, a decoding walk or
   lookup can never reach an undefined entry; a decoder must not rely on
   this for tables it has not validated.
9. Lookup structures are sized for the largest table (510 symbols, 16-bit
   codes) once per decoder, not per block from stream values.

**Window bounds**

10. Window positions are always reduced modulo the ring size. For the
    static Huffman methods the ring is at least W bytes (8,192 or 65,536)
    and the offset is below W by construction; for "-lh1-" the offset is
    12 bits; for the LArc methods the position field is 11 or 12 bits.
11. Reads "before the start of the output" return the initial contents;
    they never index outside the ring. A decoder that keeps the whole
    output in one buffer instead of a ring must handle a distance greater
    than the bytes produced so far explicitly.
12. A copy is clipped to the bytes still missing from U before it starts,
    so the output never exceeds U.

**Loops and progress**

13. Static Huffman: a block with single-symbol tables produces output
    without consuming bits. That is legitimate, and bounded: a block has at
    most 65,535 symbols and each produces at most 256 bytes. Every block
    consumes at least 52 bits of header ("-lh4-", "-lh5-") or 54 bits
    ("-lh6-", "-lh7-"), and a block size of 0 is an error, so the number of
    blocks is bounded by the packed length and the decoder cannot loop
    without progress.
14. Reading table C with a single-symbol table T consumes no bits per
    entry but advances the index every round; it ends after at most 510
    rounds.
15. Adaptive Huffman: the symbol walk descends one level per bit and ends
    at a leaf after at most 313 steps; every step consumes a bit. The
    update walk ascends one level per round. The search for the highest
    position of equal weight is bounded by the list length. The rebuild is
    a fixed amount of work (627 nodes) and happens at most once per 16,000
    or so symbols.
16. Weights in the adaptive code never exceed 32,768 plus the increments of
    one symbol's path before the next rebuild test; a 16-bit unsigned
    weight cannot overflow. Use an explicit sentinel or bound check, not a
    wrapped value, when searching for the highest position of equal weight
    at the end of the list.
17. LArc: every item consumes at least 9 bits ("-lzs-") or 1 byte plus a
    share of a flag byte ("-lz5-").

**Output and allocation**

18. Memory per decoder is fixed: a ring of at most 65,536 bytes and tables
    of a few kilobytes. Nothing is sized from U or from a stream field.
19. The output grows with the bytes produced, subject to the caller's
    limit (section 2.5).
20. How much output P packed bytes can legitimately produce:

    | Method | Upper bound on U | Reason |
    |---|---|---|
    | stored | P | |
    | "-lzs-" | 8.5 × P + 1 | 16 bits per match of at most 17 bytes |
    | "-lz5-" | 9 × P | 17 bytes per group of eight matches of 18 bytes |
    | "-lh1-" | 48 × P | a match needs at least 10 bits (1 for the symbol, 3 + 6 for the distance) and gives at most 60 bytes |
    | "-lh4-" to "-lh7-" | 16,776,960 bytes per 52 bits | 65,535 symbols × 256 bytes per block |

    For the first four rows a header that declares more is corrupt and can
    be rejected before decoding. For the static Huffman methods the bound
    is so large (about 2.5 million to 1) that only the output limit
    protects the caller: **seven packed bytes can legitimately expand to
    16 MB**.
21. The zero-window retry of section 4.9 decodes an entry at most twice.

**Results**

22. A decoder error discards the partial output of that entry. Nothing is
    delivered for an entry unless decoding produced exactly U bytes and the
    container layer's CRC-16 comparison succeeded.
23. A decoder error is confined to its entry (`container.md` section 8).

## 9. Observed implementation gaps

Faults and leniencies of the black boxes found during this work. They
explain why rules exist; none is to be imitated.

**Reference implementation**

- Reads a 2-bit skip number in table T even when the count is 1 or 2, and
  rejects a skip that passes the count. Both contradict the published
  encoders and the other decoders (section 4.4). No genuine stream is known
  to be affected, since a real table T has at least four entries whenever
  it is not in single-symbol form.
- Accepts a block size of 0, a table T count of 20, zero runs beyond the
  count of table C, and single-symbol values outside the symbol range (C
  symbols 510 and 511 produce matches of length 257 and 258; P symbols 14
  and 15 for "-lh5-" produce offsets that are silently reduced modulo the
  window).
- Allows code lengths up to 255 in the unary length field.

**`unar`**

- Fills the initial window of the static Huffman methods with `00`
  (section 4.9).
- Decodes incomplete codes instead of rejecting them, and accepts a table
  C count of 511.
- Does not decode "-lzs-".

**`7z`**

- Treats any distance that reaches before the start of the output as a
  data error (section 4.9), so it fails on genuine archives whose files
  begin with spaces if the encoder used that.
- Reports an error when spare bytes follow a static Huffman stream.
- Decodes only the stored and static Huffman methods.
