# StuffIt compression method 15 ("Arsenic") — format specification

Clean-room specification of the packed stream of forks whose compression
method number is 15. Every genuine example comes from a StuffIt 5 (SIT5)
archive, but the stream does not depend on the container: a method 15 fork in
a classic archive is decoded in exactly the same way (section 1.3). This
document describes the stream format and the decoding process only. It
contains no program code. `README.md` in this directory is the index of the
document set and holds the shared glossary.

## 1. Status, sources and conventions

### 1.1 How the facts were established

| Mark | Meaning |
|---|---|
| (none) | Read from the reference implementation **and** confirmed by an independent decoder written from this text, whose output matched the stream's own checksum and the container's unpacked length on every genuine stream of the sample set below. |
| EMPIRICAL | Not present in the reference implementation; established by measurement on the genuine sample set. |
| BLACK-BOX | Established by feeding purpose-built streams to an independent extraction tool run purely as a black box (its source was not consulted) and observing whether it accepted them and what it produced. |
| INFERRED | None of the above; a reasoned conclusion. Section 18 lists every such point. |
| DECISION | A behaviour this specification prescribes where the reference implementation and the independent tool disagree, or where both are more lenient than is safe. A decision never changes how a genuine stream decodes. Section 18 lists them. |

The **reference implementation** is the third-party StuffIt library, version
0.1.8, which the implementation written from this document replaces. The
**independent tool** is `unar`/`lsar`, version 1.10.7. These are the same two
programs that `containers.md` and `lz-methods.md` use.

The genuine sample set: 1,929 method-15 forks taken from 37 real StuffIt 5
archives; 886 of them are distinct streams (several archives were present in
more than one copy). Their properties, useful for judging coverage:

- Every stream has block-size field 10 (block capacity 524,288).
- 26 distinct streams have more than one block; together they contain 1,984 blocks and 1,098 interior block boundaries. The longest stream has 500 blocks.
- No block reaches the block capacity. The longest block is 524,261 bytes; the shortest is 8 bytes.
- 28 distinct streams contain at least one randomised block (39 such blocks; the highest flip position reached is 524,164).
- Every stream ends with an end flag and a checksum that matches, and decodes to exactly the unpacked length.
- Every model of section 5 is used and every model is halved (section 4.4) somewhere in the set.
- No stream decodes to zero bytes and no stream has zero packed bytes.

### 1.2 Conventions

- All numbers are decimal unless prefixed `0x`.
- "Bit *k* of a byte" numbers the least significant bit as 0.
- Integer division always truncates toward zero; all quantities are unsigned.
- `2^n` means two raised to the power *n*.
- Hex dumps list bytes in stream order.

### 1.3 What the decoder is given and what it returns

The division of work between the container layer and a method decoder is
defined once, for every method, in `containers.md` section 2.2. For method 15
it comes to this.

**Given.** The decoder receives the method number (15), the packed bytes of
one fork (their number, the packed length, is known from the container) and
the fork's unpacked length as declared in the container. It is not given the
container's fork checksum.

**Returned.** Either exactly as many bytes as the unpacked length says, or one
of the failures of section 12.2. Nothing else is a success.

The stream is self-terminating and self-checking (sections 6 and 11); the
unpacked length is a cross-check, not the stop condition.

**Checksums.** The stream ends with its own **stream checksum**, a CRC-32 of
the decoded output (section 11.4). The decoder verifies it; that is part of
decoding the stream. In this document "checksum" without qualification means
the stream checksum. The container's **fork checksum** (the 16-bit CRC-16
field of `containers.md` section 2.2) is not used for method 15: the container
layer does not compare it, whatever it holds and whichever container the fork
is in. Genuine SIT5 archives store 0 there for every method 15 fork. BLACK-BOX:
for a method 15 fork in a classic archive the independent tool accepts a fork
checksum field of 0, the correct CRC-16 and a wrong value alike, and reports a
failure only when the stream checksum fails.

**Which container.** DECISION: a method 15 fork is decoded as specified here
in both container generations. No genuine classic archive with method 15 was
found, and the reference implementation decodes method 15 only for SIT5
(section 17, item 12); BLACK-BOX the independent tool decodes a method 15
stream placed in a classic archive exactly as specified here.

**Cases the container layer settles before decoding.** They are the same for
every method (`containers.md` section 2.2) and the decoder is not called in
any of them:

| Packed length | Unpacked length | Result |
|---|---|---|
| any | 0 | The fork is empty: success, empty output. Nothing is decoded, and packed bytes that may be present are ignored. (BLACK-BOX: the independent tool produces nothing for such a method 15 fork and does not examine its packed bytes; it accepts vectors R5 and R6, which are invalid streams. The reference implementation decodes only the signature and the block-size field.) DECISION: this follows the general rule for empty forks; no genuine archive has such a fork with a packed length other than 0. |
| 0 | greater than 0 | Fork error "corrupt". A fork with zero packed bytes is not a stream. (BLACK-BOX: fails. The reference implementation returns empty output, which is a gap.) |
| greater than 0 | greater than 0 | The decoder is called and decodes the stream as specified here. |

The unpacked length has also been checked against the host's output limit
before the decoder is called. The limit belongs to the container layer, and
nothing here requires the decoder to know it.

**A decoder called directly** (not through the container layer) with an
unpacked length above that limit has two conforming behaviours, the same two
that `lz-methods.md` section 2.6 allows for every method (DECISION):

- It has no cap of its own. It then decodes the stream as specified, under requirement 9 of section 16 (output memory is never reserved from the unpacked length), and the outcome is whatever section 12.2 gives: success if the stream really produces that many bytes, otherwise normally "length mismatch" or "output overflow".
- It applies an output cap of its own as a second line of defence. The cap is never lower than the limit the container layer enforces, so through the container layer it cannot be reached. An unpacked length above the cap is refused with the "output limit exceeded" outcome of section 12.2 **before anything is read**: before the 26 bits are loaded, before the first-bit check and before the signature. Not a single packed byte is examined, so the refusal is the same for a valid stream, an invalid one and one of a single byte.

Refusing up front is the recommended behaviour. Decoding first and failing
later is permitted only in the first form, where there is no cap at all; a
decoder that has a cap must not decode part of the stream and then report
the cap.

A stream that decodes to zero bytes exists and is well-formed (the zero-block
stream of section 6, vector P1), but because of the first row the container
layer never asks for one to be decoded. The vectors of section 15 whose
unpacked length is 0 (P1, R5, R6) are for exercising a decoder directly: a
decoder called directly with an unpacked length of 0 decodes the stream like
any other, and succeeds only for a zero-block stream.

## 2. Overview

An Arsenic stream is a single arithmetic-coded bit stream. Everything in it —
signature, header, per-block fields, block contents, terminator and checksum —
is a sequence of symbols decoded by one arithmetic decoder using a small set of
adaptive frequency models.

The compressor applied these stages to the original data, in this order:

1. Run-length coding of the original bytes (section 10 describes the inverse).
2. Cutting the run-coded bytes into blocks of at most the block capacity.
3. Optional "randomisation" of a block: inverting the lowest bit of bytes at fixed, table-driven positions (section 9).
4. Block-sorting (Burrows–Wheeler) transform of the block, producing a permuted block plus an origin index (section 8).
5. Move-to-front ranking of the permuted block, with runs of rank 0 folded into run symbols (section 7).
6. Arithmetic coding of the resulting symbols (sections 4 and 5).

A decoder undoes them in the reverse order. The decoding order is a fact (it is
what the reference implementation does, and it reproduces every genuine stream,
including the 39 randomised blocks); the compressor's order above is simply its
mirror image.

Terms used throughout:

| Term | Meaning |
|---|---|
| packed bytes | The bytes of one fork as stored in the archive: the whole Arsenic stream. Same meaning as in `containers.md`. |
| unpacked length | The length the container declares for the decoded fork. Untrusted. |
| stream checksum | The CRC-32 that ends the stream (section 11.4). "Checksum" alone means this. |
| run-coded bytes | The byte sequence of one block after stage 3 has been undone, i.e. the input to run expansion. (Not the packed bytes: those are the whole stream as stored.) |
| sorted block | The byte sequence of one block as produced by undoing stages 6 and 5, i.e. the input to the inverse block-sorting transform. It has the same length as the block's run-coded bytes. |
| block length | The number of bytes in a block's sorted block (equally, in its run-coded bytes). Written *n*. |
| block capacity | The largest permitted block length; fixed for the whole stream by the header. |
| origin index | Per-block number selecting the starting point of the inverse block-sorting transform. |
| primary model, class model, rank models | The adaptive models of section 5. |

## 3. Bit input

The packed bytes are consumed as a sequence of bits in **high-bit-first
order** (the term of `lz-methods.md` section 2.1). Bytes are taken in
order; within each byte the **most significant bit (bit 7) is taken first**,
bit 0 last. There is no byte alignment anywhere in the stream and no other way
of reading the input: every bit is requested by the arithmetic decoder, one at
a time — 26 bits when decoding starts, and afterwards one bit for every
doubling of section 4.3.

**End of input.** Let *B* be eight times the number of packed bytes. A
valid stream supplies every bit the decoder requests: the total number of
requests — 26 plus all doublings, up to and including the doublings that follow
the last checksum symbol — is at most *B*.

- EMPIRICAL: in all 886 distinct genuine streams that total lies between *B* − 7 and *B*. In other words the packed bytes end exactly with the byte that holds the last requested bit; at most 7 bits of that byte are left over, and there is never a spare byte.
- BLACK-BOX: the independent tool fails a stream from which the final byte has been removed, even when the only bits lost are those requested by the doublings after the last checksum symbol, which no longer influence any decoded value.

DECISION: **a request for a bit beyond bit *B* is an error ("truncated");
no substitute bits are supplied.** This applies to every request, including
those made after the last checksum symbol, so the doublings after that symbol
must be performed like any others. (The reference implementation instead supplies 0
bits without limit; see section 17.)

Packed bytes left over after the last request are ignored (section 11.3).

## 4. The arithmetic decoder

### 4.1 State and constants

| Quantity | Value |
|---|---|
| Range (R), initial value | 2^25 = 33,554,432 |
| Renormalisation threshold | 2^24 = 16,777,216 |
| Code (C), initial value | the first 26 bits of the stream read as one number, first bit most significant |

R and C are the only decoder state apart from the models and the input
position.

The number 26 has no other role: it is only the count of bits loaded into C
at the start. R never exceeds 2^25. In a valid stream C is always less than R,
so C never needs more than 25 bits, and the very first bit of a valid stream is
therefore 0 (every valid stream in fact begins with byte 0x42, section 6).

Invariants (they follow from section 4.3 and were also measured on every
symbol of the genuine sample set):

- Before every symbol, R is greater than 2^24 and at most 2^25.
- If C is less than R before a symbol, C is less than R after it, including after the doublings. So the relation needs to be checked only once, immediately after the initial 26 bits are loaded: **if C is 2^25 or more (the first input bit is 1), the input is not an Arsenic stream.** The check is made on the loaded value, so all 26 bits must exist first: packed bytes of fewer than 26 bits (one, two or three bytes) are "truncated" whatever their first bit is (section 12.2). After that check C stays below 2^25 for the rest of the stream, whatever the input contains; no masking or truncation of C is ever needed, and none must be applied.
- The subtraction in rule 5 of section 4.3 can never go below zero.
- One symbol causes at most 11 doublings.

### 4.2 A model

A model is an ordered list of *n* symbols, each with a positive integer
frequency, plus:

- its **total** (the sum of all frequencies),
- its **increment** (added to a symbol's frequency each time that symbol is decoded),
- its **limit** (when the total exceeds it, all frequencies are halved),
- its **base value** (the number represented by the first symbol; the *i*-th symbol, counting from 0, represents base + *i*).

Initial state of any model: every frequency equals the increment; the total is
therefore *n* × increment. "Resetting" a model means returning it to exactly
this state.

### 4.3 Decoding one symbol with a model

The model's frequencies divide the current range into consecutive slices, one
per symbol in list order, and the code value says which slice the stream
selects. Exactly:

1. The **unit** is R divided by the model's total.
2. The **target** is C divided by the unit.
3. For each symbol, its **below** value is the sum of the frequencies of all symbols before it in the list (0 for the first symbol) and its **width** is its own frequency. The decoded symbol is the first symbol in list order whose below + width is greater than the target. If there is no such symbol, the decoded symbol is the **last** symbol in the list. That case is legitimate and not an error: because R is generally not an exact multiple of the total, the target can be equal to the total or larger (in the genuine sample set it happens 1,858 times, in 138 of the 886 distinct streams; vectors P9, P10 and P11 of section 15 contain it).
4. From here on, below and width are those of the decoded symbol, taken before any model update.
5. C decreases by unit × below.
6. The new R depends on whether the decoded symbol is the last in the list:
   - not the last: R becomes unit × width;
   - the last: R decreases by unit × below. The last symbol thus also owns the remainder left by the truncating division in rule 1.
7. Renormalisation: as long as R is less than **or equal to** 2^24, R doubles, C doubles, and the next input bit is added to C (it becomes C's least significant bit). Each doubling consumes exactly one input bit.
8. The model is then updated (section 4.4).
9. The decoded value is the model's base value plus the zero-based position of the decoded symbol in the list.

Rules 1–6 use the frequencies and total as they were before rule 8.

### 4.4 Updating a model after a symbol

1. The increment is added to the decoded symbol's frequency and to the total.
2. If the total is now **strictly greater** than the limit (equal is not enough): every frequency *f* in the model is replaced by (*f* + 1) divided by 2 — that is, halved, rounding up — and the total becomes the sum of the new frequencies (it is recomputed, not halved).

Rounding up guarantees a frequency never becomes zero. Consequently, before
any symbol is decoded a model's total is at most its limit.

Examples of rule 2:

- A two-symbol model with limit 256 holding frequencies (200, 56) receives its first symbol with increment 1, giving (201, 56) and total 257; 257 is greater than 256, so the frequencies become (101, 28) and the total 129.
- Taken from vector P11 of section 15 (its 1,953rd symbol): the primary model holds (220, 36), total 256, and its second symbol is decoded. The frequencies become (220, 37), total 257, and are then halved to (110, 19), total 129. Note 37 → 19.
- Also from vector P11 (its 179th symbol): the class model holds (400, 392, 8, 24, 24, 32, 24, 32, 24, 56, 8), total 1,024 — equal to the limit, so no halving has happened yet — and its first symbol is decoded. The total becomes 1,032, and the frequencies are halved to (204, 196, 4, 12, 12, 16, 12, 16, 12, 28, 4), total 516.

### 4.5 Decoding a multi-bit number

Several fields are *k*-bit unsigned numbers. Such a number is decoded as *k*
successive symbols from one model (always the primary model in this format),
each symbol being 0 or 1. **The first symbol decoded is the least significant
bit** (value 1), the second has value 2, and so on; the *k*-th has value
2^(k−1).

## 5. The models

There are nine models. All exist from the start of the stream.

| Name | Symbols | Base value | Values represented | Increment | Limit | Initial frequency of each symbol | Initial total | Lifetime |
|---|--:|--:|---|--:|--:|--:|--:|---|
| primary | 2 | 0 | 0, 1 | 1 | 256 | 1 | 2 | Never reset; adapts across the entire stream. |
| class | 11 | 0 | 0 … 10 | 8 | 1024 | 8 | 88 | Reset after every block. |
| rank 1 | 2 | 2 | 2, 3 | 8 | 1024 | 8 | 16 | Reset after every block. |
| rank 2 | 4 | 4 | 4 … 7 | 4 | 1024 | 4 | 16 | Reset after every block. |
| rank 3 | 8 | 8 | 8 … 15 | 4 | 1024 | 4 | 32 | Reset after every block. |
| rank 4 | 16 | 16 | 16 … 31 | 4 | 1024 | 4 | 64 | Reset after every block. |
| rank 5 | 32 | 32 | 32 … 63 | 2 | 1024 | 2 | 64 | Reset after every block. |
| rank 6 | 64 | 64 | 64 … 127 | 2 | 1024 | 2 | 128 | Reset after every block. |
| rank 7 | 128 | 128 | 128 … 255 | 1 | 1024 | 1 | 128 | Reset after every block. |

What each decodes:

- **primary** — every header and framing bit: the two signature bytes, the block-size field, each end flag, each randomisation flag, each origin index, and the trailing checksum.
- **class** — the symbol stream of a block's contents (section 7).
- **rank 1 … rank 7** — the exact move-to-front rank, when the class symbol only identifies a range of ranks (section 7).

"Reset after every block" means: once a block's end-of-block class symbol
has been decoded, the class model and all seven rank models are returned to
their initial state before the next block (if any) begins. The arithmetic
decoder's R and C are never reset, and neither is the primary model.

## 6. Stream layout

In decoding order:

| # | Field | Coding | Meaning |
|--:|---|---|---|
| 1 | Signature byte 1 | 8-bit number, primary model | Must equal 0x41 (ASCII `A`). |
| 2 | Signature byte 2 | 8-bit number, primary model | Must equal 0x73 (ASCII `s`). |
| 3 | Block-size field *s* | 4-bit number, primary model | Block capacity is 2^(*s* + 9) bytes. *s* = 0 gives 512; *s* = 15 gives 16,777,216. All sixteen values are valid. |
| 4 | Zero or more blocks | see below | |
| 5 | End flag = 1 | one symbol, primary model | Marks the end of the block sequence. |
| 6 | Checksum | 32-bit number, primary model | CRC-32 of the entire decoded output (section 11.4). |

Each block consists of, in decoding order:

| # | Field | Coding | Meaning |
|--:|---|---|---|
| a | End flag = 0 | one symbol, primary model | Another block follows. (This is the same field as item 5 above: one end flag is decoded, and its value decides whether a block or the checksum comes next.) |
| b | Randomisation flag | one symbol, primary model | 1 if the block was randomised (section 9), else 0. |
| c | Origin index | (*s* + 9)-bit number, primary model | Starting point for the inverse block-sorting transform (section 8). Its width in bits always equals the base-2 logarithm of the block capacity, whatever the block's actual length. |
| d | Block contents | class and rank models | Symbols producing the sorted block, terminated by the end-of-block class symbol (section 7). |

There is no explicit block length field: the length of a block is the number of
bytes its contents produce before the end-of-block symbol. It must be at least
1 (section 8) and at most the block capacity.

Evidence for the points above that the genuine sample set cannot show:

- **Block-size field.** Only *s* = 10 occurs in genuine streams. The reference implementation accepts all sixteen values, and BLACK-BOX the independent tool decodes the same data correctly with every value from 0 to 15.
- **Block capacity as a limit.** The reference implementation does not enforce it (section 17). EMPIRICAL: no genuine block is longer than 524,261 bytes, 27 short of the capacity. BLACK-BOX: with *s* = 0 the independent tool accepts a block of exactly 512 bytes (vector P8) and fails a block of 513 bytes (vector R3). DECISION: the limit is "at most the capacity", and a longer block is an error.
- **Zero blocks.** A stream whose first end flag is 1 has no blocks; it decodes to zero bytes and its checksum is 0x00000000 (vector P1). No genuine example exists. BLACK-BOX: the independent tool accepts it. This is the only stream that decodes to zero bytes; section 1.3 explains why the container layer never needs it.

### 6.1 Leading bytes

Because the signature and the block-size field are the first symbols and are
coded with a freshly initialised model, they confine the first bytes of the
stream to narrow ranges. These ranges were computed from sections 4 and 5 and
checked by decoding; they are a consequence of the coding, not a separate
magic number.

- A stream decodes to the signature `A`, `s` **if and only if** its first 43 bits, read as one unsigned number, lie between 0x2160E1AE43C and 0x2160F73F5B1 inclusive. Equivalently: the first six bytes, read as a 48-bit big-endian number, lie between 0x42C1C35C8780 and 0x42C1EE7EB63F inclusive.
- Hence every valid stream begins with the two bytes 0x42 0xC1, and its third byte lies between 0xC3 and 0xEE. The converse does not hold: 0x42 0xC1 followed by a third byte outside that range is never a valid stream, and at the two ends of the range (0xC3, 0xEE) the fourth and later bytes decide.
- The block-size field narrows the range further. The table gives, for each *s*, the smallest and largest values of the first five bytes. Adjacent ranges share their boundary value because the exact boundary falls inside the sixth byte.

| *s* | Block capacity | First five bytes, smallest | First five bytes, largest |
|--:|--:|---|---|
| 0 | 512 | `42 C1 C3 5C 87` | `42 C1 C8 83 AF` |
| 8 | 131,072 | `42 C1 C8 83 AF` | `42 C1 CB AF 79` |
| 4 | 8,192 | `42 C1 CB AF 79` | `42 C1 CE DB 43` |
| 12 | 2,097,152 | `42 C1 CE DB 43` | `42 C1 D1 3C 1B` |
| 2 | 2,048 | `42 C1 D1 3C 1B` | `42 C1 D4 67 E5` |
| 10 | 524,288 | `42 C1 D4 67 E5` | `42 C1 D6 C8 BD` |
| 6 | 32,768 | `42 C1 D6 C8 BD` | `42 C1 D9 29 94` |
| 14 | 8,388,608 | `42 C1 D9 29 94` | `42 C1 DB 53 13` |
| 1 | 1,024 | `42 C1 DB 53 13` | `42 C1 DE 7E DD` |
| 9 | 262,144 | `42 C1 DE 7E DD` | `42 C1 E0 DF B4` |
| 5 | 16,384 | `42 C1 E0 DF B4` | `42 C1 E3 40 8C` |
| 13 | 4,194,304 | `42 C1 E3 40 8C` | `42 C1 E5 6A 0A` |
| 3 | 4,096 | `42 C1 E5 6A 0A` | `42 C1 E7 CA E2` |
| 11 | 1,048,576 | `42 C1 E7 CA E2` | `42 C1 E9 F4 60` |
| 7 | 65,536 | `42 C1 E9 F4 60` | `42 C1 EC 1D DE` |
| 15 | 16,777,216 | `42 C1 EC 1D DE` | `42 C1 EE 7E B6` |

  For *s* = 10 the third byte can thus be 0xD4, 0xD5 or 0xD6. EMPIRICAL: genuine streams have 0xD4 (634 of 886) or 0xD5 (252 of 886); 0xD6 is possible by the rules but was not seen.

These ranges may be used as a quick plausibility test (necessary, not
sufficient, except for the exact 43-bit rule). The authoritative test is to
decode the signature.

## 7. Block contents: class symbols, zero runs and move-to-front

### 7.1 The move-to-front list

At the start of **every block** the move-to-front list holds the 256 byte values
in ascending order: position 0 holds 0x00, position 255 holds 0xFF. It is not
carried over between blocks.

"Applying rank *r*" means: take the byte value at position *r* of the list,
remove it from that position, insert it at position 0 (shifting the values that
were at positions 0 … *r*−1 up by one), and append that byte value to the
sorted block.

### 7.2 The class alphabet

The contents of a block are coded with an alphabet of eleven symbols, here
called class symbols and decoded with the class model. A class symbol is a
zero-run digit, or names a class of ranks (alone or together with one further
symbol from the rank model of that class), or ends the block.

| Class symbol | Meaning |
|--:|---|
| 0 | Zero-run digit with value 1 (section 7.3). |
| 1 | Zero-run digit with value 2 (section 7.3). |
| 2 | Apply rank 1. No further symbol is read. |
| 3 | Decode one symbol from model rank 1 (result 2 … 3) and apply it as a rank. |
| 4 | Decode one symbol from model rank 2 (result 4 … 7) and apply it as a rank. |
| 5 | Decode one symbol from model rank 3 (result 8 … 15) and apply it as a rank. |
| 6 | Decode one symbol from model rank 4 (result 16 … 31) and apply it as a rank. |
| 7 | Decode one symbol from model rank 5 (result 32 … 63) and apply it as a rank. |
| 8 | Decode one symbol from model rank 6 (result 64 … 127) and apply it as a rank. |
| 9 | Decode one symbol from model rank 7 (result 128 … 255) and apply it as a rank. |
| 10 | End of block. |

The value delivered by a rank model is the rank itself (the base values in
section 5 already include the offset), so no further arithmetic is needed.

### 7.3 Zero runs

Rank 0 (repeat the byte currently at the front of the list) is never coded
individually. A maximal sequence of consecutive class symbols 0 and 1 codes
one run length, as a number written in bijective base 2, least significant
digit first:

- the first symbol of the sequence has weight 1, the second weight 2, the third weight 4, and so on, doubling each time;
- a symbol 0 contributes 1 × its weight; a symbol 1 contributes 2 × its weight;
- the run length is the sum of the contributions.

| Symbols (in decoding order) | Run length |
|---|--:|
| 0 | 1 |
| 1 | 2 |
| 0 0 | 3 |
| 1 0 | 4 |
| 0 1 | 5 |
| 1 1 | 6 |
| 0 0 0 | 7 |
| 1 1 1 | 14 |

The sequence ends at the first class symbol that is 2 or greater. At that
moment — before acting on that symbol — *run length* copies of the byte at
position 0 of the move-to-front list are appended to the sorted block. The list
itself is not changed by a run. The terminating symbol is then processed
normally (it may be a rank symbol or the end-of-block symbol). A new run starts
with weight 1 again.

A run may occur at the very start of a block, where it produces copies of byte
0x00 (the front of the initial list).

The run length is bounded by the space left in the block; see section 16,
requirement 3, for how the bound must be applied while the digits arrive.

### 7.4 End of block

Class symbol 10 ends the block contents. Any zero run in progress is
appended first (section 7.3). After symbol 10 the class model and the seven
rank models are reset (section 5).

## 8. Inverse block-sorting transform

Input: the sorted block of length *n*, whose bytes are numbered 0 … *n*−1, and
the block's origin index. Output: *n* bytes.

Definition of the forward transform, for reference: all *n* cyclic rotations of
the block are sorted in ascending order, comparing bytes as unsigned values;
the sorted block is the last byte of each rotation, taken in sorted order; the
origin index is the position, in that sorted order, of the rotation that equals
the unrotated block.

**The origin index must be less than *n*.** Since the smallest origin index is
0, a block of length 0 can never satisfy this: an empty block is invalid, and
either case is a failure of the whole fork (DECISION; the reference
implementation instead stops quietly and returns what it has, section 17). (No
genuine stream contains one; the reference implementation stops at one; BLACK-BOX the
independent tool fails when anything follows one.)

The inverse:

1. For each byte value *v* (0 … 255), count how many times it occurs in the sorted block.
2. For each byte value *v*, its **first slot** is the number of bytes in the sorted block whose value is less than *v*.
3. Build a **successor table** of *n* entries, numbered 0 … *n*−1. Go through the sorted block from byte 0 to byte *n*−1. For byte number *i*, whose value is *v*: write *i* into the successor-table entry numbered by *v*'s current slot, then increase *v*'s slot by one. (Equal byte values therefore receive consecutive entries, in the order in which they appear in the sorted block.)
4. Start with a position *p* equal to the origin index.
5. Do the following *n* times: first replace *p* by the value held in successor-table entry *p*; then output the byte that the sorted block holds at byte number *p*.

Note the order within step 5: the table is consulted first, then a byte is
output. The first output byte is therefore not the sorted-block byte at the
origin index, but the one at the position the table gives for the origin
index.

Step 5 is always performed exactly *n* times. For a sorted block produced by a
genuine forward transform the walk visits every entry once. For arbitrary
(corrupt) input the walk may revisit entries; that is harmless, yields *n*
bytes, and is caught by the checksum.

## 9. Randomisation

Randomisation applies only to blocks whose randomisation flag is 1. It acts on
the *n* bytes delivered by the inverse block-sorting transform, numbered 0 …
*n*−1 in output order, and it must be undone before run expansion.

A fixed, endless, strictly increasing sequence of **flip positions** is defined
by the randomisation table below:

- the first flip position is table entry 0, that is 238;
- each further flip position is the previous one plus the next table entry, the entries being used in index order 1, 2, …, 255 and then again from 0, cyclically.

So the flip positions are 238, 324, 572, 767, 924, 1083, 1257, 1301, … The
256th is 32,740 (the sum of the whole table), the 257th is 32,740 + 238 =
32,978, the 258th is 33,064, and in general each flip position reappears
32,740 higher 256 positions later.

Undoing randomisation: **in a randomised block, every byte whose number within
the block is a flip position has its bit 0 inverted.** All other bytes, and all
other bits, are unchanged.

- The sequence starts afresh from 238 in every randomised block; it does not continue from the previous block.
- A randomised block of 238 bytes or fewer contains no flip position; its flag has no effect.
- The byte numbering counts every byte delivered by the inverse transform in this block, including bytes that the run-expansion stage will interpret as repeat counts.

Randomisation table (256 entries, read left to right, top to bottom; entry
index = row label + column offset). Six entries exceed 255 (indices 13, 41, 45,
95, 233, 236); the largest entry is 0x101 = 257 and the smallest is 2.
Self-check: the sum of all 256 entries is 32,740; the sum of the first 128 is
16,950.

| Index | +0 | +1 | +2 | +3 | +4 | +5 | +6 | +7 |
|--:|--:|--:|--:|--:|--:|--:|--:|--:|
| 0 | 0x0EE | 0x056 | 0x0F8 | 0x0C3 | 0x09D | 0x09F | 0x0AE | 0x02C |
| 8 | 0x0AD | 0x0CD | 0x024 | 0x09D | 0x0A6 | 0x101 | 0x018 | 0x0B9 |
| 16 | 0x0A1 | 0x082 | 0x075 | 0x0E9 | 0x09F | 0x055 | 0x066 | 0x06A |
| 24 | 0x086 | 0x071 | 0x0DC | 0x084 | 0x056 | 0x096 | 0x056 | 0x0A1 |
| 32 | 0x084 | 0x078 | 0x0B7 | 0x032 | 0x06A | 0x003 | 0x0E3 | 0x002 |
| 40 | 0x011 | 0x101 | 0x008 | 0x044 | 0x083 | 0x100 | 0x043 | 0x0E3 |
| 48 | 0x01C | 0x0F0 | 0x086 | 0x06A | 0x06B | 0x00F | 0x003 | 0x02D |
| 56 | 0x086 | 0x017 | 0x07B | 0x010 | 0x0F6 | 0x080 | 0x078 | 0x07A |
| 64 | 0x0A1 | 0x0E1 | 0x0EF | 0x08C | 0x0F6 | 0x087 | 0x04B | 0x0A7 |
| 72 | 0x0E2 | 0x077 | 0x0FA | 0x0B8 | 0x081 | 0x0EE | 0x077 | 0x0C0 |
| 80 | 0x09D | 0x029 | 0x020 | 0x027 | 0x071 | 0x012 | 0x0E0 | 0x06B |
| 88 | 0x0D1 | 0x07C | 0x00A | 0x089 | 0x07D | 0x087 | 0x0C4 | 0x101 |
| 96 | 0x0C1 | 0x031 | 0x0AF | 0x038 | 0x003 | 0x068 | 0x01B | 0x076 |
| 104 | 0x079 | 0x03F | 0x0DB | 0x0C7 | 0x01B | 0x036 | 0x07B | 0x0E2 |
| 112 | 0x063 | 0x081 | 0x0EE | 0x00C | 0x063 | 0x08B | 0x078 | 0x038 |
| 120 | 0x097 | 0x09B | 0x0D7 | 0x08F | 0x0DD | 0x0F2 | 0x0A3 | 0x077 |
| 128 | 0x08C | 0x0C3 | 0x039 | 0x020 | 0x0B3 | 0x012 | 0x011 | 0x00E |
| 136 | 0x017 | 0x042 | 0x080 | 0x02C | 0x0C4 | 0x092 | 0x059 | 0x0C8 |
| 144 | 0x0DB | 0x040 | 0x076 | 0x064 | 0x0B4 | 0x055 | 0x01A | 0x09E |
| 152 | 0x0FE | 0x05F | 0x006 | 0x03C | 0x041 | 0x0EF | 0x0D4 | 0x0AA |
| 160 | 0x098 | 0x029 | 0x0CD | 0x01F | 0x002 | 0x0A8 | 0x087 | 0x0D2 |
| 168 | 0x0A0 | 0x093 | 0x098 | 0x0EF | 0x00C | 0x043 | 0x0ED | 0x09D |
| 176 | 0x0C2 | 0x0EB | 0x081 | 0x0E9 | 0x064 | 0x023 | 0x068 | 0x01E |
| 184 | 0x025 | 0x057 | 0x0DE | 0x09A | 0x0CF | 0x07F | 0x0E5 | 0x0BA |
| 192 | 0x041 | 0x0EA | 0x0EA | 0x036 | 0x01A | 0x028 | 0x079 | 0x020 |
| 200 | 0x05E | 0x018 | 0x04E | 0x07C | 0x08E | 0x058 | 0x07A | 0x0EF |
| 208 | 0x091 | 0x002 | 0x093 | 0x0BB | 0x056 | 0x0A1 | 0x049 | 0x01B |
| 216 | 0x079 | 0x092 | 0x0F3 | 0x058 | 0x04F | 0x052 | 0x09C | 0x002 |
| 224 | 0x077 | 0x0AF | 0x02A | 0x08F | 0x049 | 0x0D0 | 0x099 | 0x04D |
| 232 | 0x098 | 0x101 | 0x060 | 0x093 | 0x100 | 0x075 | 0x031 | 0x0CE |
| 240 | 0x049 | 0x020 | 0x056 | 0x057 | 0x0E2 | 0x0F5 | 0x026 | 0x02B |
| 248 | 0x08A | 0x0BF | 0x0DE | 0x0D0 | 0x083 | 0x034 | 0x0F4 | 0x017 |

## 10. Run expansion (final stage)

Input: the run-coded bytes of a block. Output: bytes appended to the decoded
output.

The stage keeps two items of state: the **previous byte** and a **repeat
counter** (0 … 4). **At the start of each block the counter is 0**; the state
is not carried from one block to the next (section 11.2).

For each run-coded byte *b*, in order:

- If the counter is 4: *b* is a **repeat count**, not data. *b* further copies of the previous byte are appended to the output (*b* may be 0, appending nothing; the maximum is 255). The counter becomes 0. The previous byte is unchanged.
- Otherwise *b* is a literal and is appended to the output. If the counter is greater than 0 and *b* equals the previous byte, the counter increases by 1; if not, the previous byte becomes *b* and the counter becomes 1.

In words: after four identical literal bytes in a row, the next byte tells how
many additional copies follow. A run of exactly four bytes is normally coded
as those four bytes plus a count byte of 0. After a count byte the counter
starts again from 0 while the previous byte is remembered, so a literal equal to
the previous byte that follows a count byte counts as the first of a new group
of four. For example, 300 copies of 0x61 are coded as: four 0x61, count 255
(259 so far), four 0x61, count 37 (300).

When the counter is 0, the remembered previous byte is irrelevant: the next
literal always sets it. This is the situation at the start of every block.

**A block whose run-coded bytes end while the counter is 4** (four identical
literals with no count byte after them) is valid. The pending count is simply
dropped: nothing more is appended for that block, and the next block starts
with counter 0, so its first byte is a literal. The same holds when it is the
last block of the stream. Evidence: this is what the reference implementation does;
BLACK-BOX the independent tool accepts such streams when their checksum is
computed over the output obtained this way (vectors P4 and P5) and reports a
checksum failure when the checksum is instead computed as if the next block's
first byte were the count. EMPIRICAL: genuine compressors do not produce this
situation (0 of 1,098 interior block boundaries, 0 of 886 final blocks).

## 11. Assembling the output, termination and checksum

### 11.1 Concatenation

The bytes produced by run expansion for each block are appended to the output
in block order. The complete output is the unpacked fork.

Blocks are independent except for: the arithmetic decoder state (R, C, input
position), the primary model, and the running checksum.

### 11.2 Run-expansion state at block boundaries

The repeat counter restarts at 0 in every block. This is the reference
implementation's behaviour and, BLACK-BOX, that of the independent tool: a stream
whose first block has run-coded bytes `61 61` and whose second block has run-coded
bytes `61 61 05` is accepted with the checksum of the five bytes
`61 61 61 61 05` (vector P6) and rejected with the checksum of the nine bytes
that carrying the counter across the boundary would give.

The genuine sample set cannot distinguish the two readings (decoding all of it
with the state carried across boundaries gives identical output), because
genuine compressors run-length code each block separately. EMPIRICAL, at the 1,098 interior
block boundaries:

| Counter when the earlier block ends | Boundaries | Next block's first byte equals the remembered byte |
|--:|--:|--:|
| 0 (the block ends with a count byte) | 67 | 8 |
| 1, 2 or 3 | 1,031 | 0 |
| 4 (count byte pending) | 0 | — |

### 11.3 Termination

The block sequence ends when an end flag of 1 is decoded. The 32-bit checksum
follows immediately, and the doublings after its last symbol complete the
stream (section 3).

Any packed bytes remaining after that point are not part of the stream and
are **ignored; they are not an error, however many there are**. EMPIRICAL: no
genuine stream has any (only 0 to 7 unused bits in the final byte). BLACK-BOX:
the independent tool accepts a valid stream followed by 1 or by 100 extra
bytes. Integrity is established by the checksum and the length, not by the
absence of trailing bytes.

In every genuine stream the number of output bytes at the final end flag is
exactly the unpacked length from the container. Section 12.3
states what a decoder must do when it is not.

### 11.4 Stream checksum (EMPIRICAL and BLACK-BOX)

After the final end flag, a 32-bit number is decoded with the primary model,
least significant bit first (section 4.5). It equals the CRC-32 of all output
bytes of the stream, in order:

| Parameter | Value |
|---|---|
| Width | 32 bits |
| Polynomial | 0x04C11DB7 (reflected form 0xEDB88320) |
| Initial register value | 0xFFFFFFFF |
| Input reflection | yes (bytes are processed least significant bit first) |
| Output reflection | yes |
| Final exclusive-or | 0xFFFFFFFF |
| Check value | the nine ASCII bytes `123456789` give 0xCBF43926 |
| Bytes covered | the complete decoded output of the stream (all blocks, after run expansion) |

This is the same CRC-32 used by zip and gzip. The CRC of an empty output is
0x00000000.

The reference implementation does not read this field at all. EMPIRICAL: decoding 32
further primary-model symbols after the final end flag gives the CRC-32 of the
decoded output in all 886 distinct genuine streams. BLACK-BOX: the independent
tool reports a checksum failure when one bit of the field is changed (vector
R1) and fails a stream that ends after the final end flag without the field.

The container's fork checksum field (CRC-16) is not used for method 15, in
either container generation (section 1.3; `containers.md` section 2.2).

### 11.5 Trailing bits that do not influence the result

Let *Q* be the total number of bits a stream requests (section 3) and number
the requested bits from the end: the last requested bit is "1st from the
end" and has weight 1, the one before it is 2nd from the end with weight 2,
and the *k*-th from the end has weight 2^(*k*−1).

After the doublings that follow the last checksum symbol, R is greater than
2^24 and at most 2^25, and C may hold any value from 0 to R − 1. The symbols
of the stream fix R and fix everything about C except this final value. Read
the *Q* requested bits as one *Q*-bit number: the streams that decode to
**the same symbol sequence** are exactly those whose number lies in one
unbroken range of R consecutive values, and the final C is the position of
the number within that range (0 for the lowest). Adding to the number adds
the same amount to the final C. This follows from section 4.3 (each symbol owns one slice of the
range, and the last symbol of a model also owns the remainder, so the slices
leave no gaps) and was confirmed by inverting every requested bit of the
streams listed below, one at a time.

Consequences for a test that inverts single bits of a valid stream:

- **Only the last 25 requested bits can be insensitive.** Inverting a requested bit that is 26th from the end or earlier changes the number by 2^25 or more, which is at least R, so the symbol sequence always changes. The bound is exact: 25 is reached (vector P9 below), 26 never.
- Inverting the *k*-th bit from the end, *k* ≤ 25, leaves the symbol sequence unchanged exactly when the final C stays within 0 … R − 1: for a bit that is 0, when final C + 2^(*k*−1) is less than R; for a bit that is 1, when final C is at least 2^(*k*−1).
- A changed symbol sequence is not by itself a failure, but it differs in at least one decoded symbol, so either some field or the checksum differs. In every single-bit inversion of the five streams below that changed the symbol sequence, decoding failed (in one of the ways of section 12.2); none produced the same output with a matching checksum.
- Bits of the final byte beyond the *Q*-th are never requested and are ignored entirely (section 3), in addition to the above.

**The synthetic streams of sections 13 to 15 all end with final C = 0**
(their encoder writes the lowest code value of the final interval). For them
the rule becomes simple: among the last 25 requested bits, inverting any bit
that is 0 leaves the decode unchanged (its weight is at most 2^24, which is
less than R), and inverting any bit that is 1 changes it; the 25th bit from
the end may be either. Measured:

| Stream | Bits requested, *Q* | Final R | Final C | Requested bits whose inversion leaves the decode unchanged, counted from the end |
|---|--:|--:|--:|---|
| Section 13 | 144 | 19,814,544 | 0 | 1–6, 13, 15, 18, 19, 20, 22, 23, 24 |
| P1 | 61 | 24,088,050 | 0 | 1–16, 18, 23 |
| P2 | 106 | 17,273,488 | 0 | 1–6, 8, 9, 11, 12, 13, 17, 19, 21, 24 |
| P3 | 150 | 27,532,804 | 0 | 1, 2, 7, 9, 11, 12, 14, 15, 17, 18, 19, 24 |
| P9 | 108 | 28,339,506 | 0 | 1–5, 8, 10–13, 20, 21, 24, 25 |

In the section 13 stream the 24th bit from the end is the bit numbered 120
when the 144 bits are numbered from 0, and the 25th from the end (numbered
119) is a 1, so inverting it changes the decode.

What final C a genuine compressor leaves is not known (section 18); for a
genuine stream only the bound of 25 bits and the second rule above can be
relied on.

## 12. Decoding order, failures and partial output

### 12.1 Order of the fields and stages

The stream is decoded strictly in the order of the layout in section 6: the
signature bytes, the block-size field, then end flags, each end flag of 0
being followed by one complete block and the end flag of 1 by the checksum.

For one block, the stages are undone in this order:

1. Decode the randomisation flag (primary model).
2. Decode the origin index as an (*s* + 9)-bit number (primary model).
3. Start with the move-to-front list holding 0x00 … 0xFF in ascending order, an empty sorted block and no zero run in progress.
4. Decode class symbols, and rank symbols where the class symbol calls for one, acting on each as section 7 describes, until the end-of-block symbol. The result is the sorted block of *n* bytes. — *undoes arithmetic coding, zero-run folding and move-to-front*
5. Reset the class model and the seven rank models.
6. Check that the origin index is less than *n*.
7. Apply the inverse block-sorting transform (section 8), giving *n* bytes. — *undoes block sorting*
8. If the randomisation flag is 1, invert bit 0 of the bytes at the flip positions (section 9). — *undoes randomisation*
9. Run-expand those *n* bytes (section 10), starting with repeat counter 0, and append the result to the output. — *undoes run-length coding*

### 12.2 Kinds of failure

The last column is the fork error the container layer reports for the
failure (`containers.md` section 2.2). The decoder keeps the kinds apart for
diagnostics.

| Kind | Detected when | Fork error |
|---|---|---|
| Not an Arsenic stream | All of the first 26 bits exist and the first input bit is 1 (section 4.1), or a signature byte decodes to the wrong value. Each signature byte is compared as soon as its eight symbols have been decoded. | "corrupt" |
| Truncated | A bit beyond the end of the packed bytes is requested (section 3). This includes packed bytes shorter than 26 bits, **whatever their first bit is**: the single byte 0x80 is "truncated", not "not an Arsenic stream" (vectors R13 to R15). | "corrupt" |
| Corrupt block | The sorted block would exceed the block capacity — whether by a rank symbol or by a zero run, the latter detected while its digits are still arriving (section 16) — or the origin index is not less than the block length. | "corrupt" |
| Output overflow | Run expansion is about to append a byte that would make the output longer than the unpacked length. Bytes that append nothing (a repeat count of 0) do not trigger it. | "corrupt" |
| Length mismatch | The final end flag and the stream checksum have been decoded and the output is shorter than the unpacked length. | "corrupt" |
| Checksum mismatch | The final end flag and the stream checksum have been decoded, the output length equals the unpacked length, and the decoded stream checksum differs from the CRC-32 of the output. | "checksum mismatch" |

The six kinds above are faults of the stream or of the container's claim
about it. Two further outcomes are not; they say that this decoder, on this
host, declined to do the work. Both are reported as the fork error "output
limit exceeded", never as "corrupt" and never as a success with fewer bytes.
They are the same exception that `lz-methods.md` section 2.6 makes for the
other methods. (DECISION.)

| Kind | Detected when | Fork error |
|---|---|---|
| Output limit exceeded | The decoder has an output cap of its own and the unpacked length is above it (section 1.3). Tested before anything is read. Unreachable through the container layer. | "output limit exceeded" |
| Out of memory | The host cannot supply memory the decoder needs at that moment: for the growing sorted block, for the successor table of a block whose symbols have all been decoded, or for the output. | "output limit exceeded" |

"Out of memory" says nothing about the stream: the same stream may decode on
a host with more memory, and a stream that would have turned out corrupt a
few symbols later is still reported this way if memory runs out first. The
decoder keeps it apart from "output limit exceeded" for diagnostics (its
message should say that memory for a block could not be obtained), must
report it rather than abort the program, and must not report it as
"corrupt". The amounts involved are bounded by the format: for a block of
*n* bytes, *n* bytes of sorted block and *n* successor-table entries
(section 16, requirement 2), so at most 2^24 bytes plus 2^24 entries when
the block-size field is 15 and the block is full. Because memory follows the
bytes actually decoded and is never reserved from the header, the case
arises only for blocks that really are that long. Neither black box has such
an outcome: the reference implementation reserves the full block capacity
for every block and does not recover from a refusal (section 17, item 6),
and how the independent tool behaves when memory runs out was not tested.

Output overflow and length mismatch are DECISIONS: the reference
implementation silently drops excess output and returns short output as a
success; BLACK-BOX the independent tool stops at the unpacked length and then
fails its checksum test in the first case (vector R11) and accepts the short
output in the second (vector R12).

**Precedence.** Decoding is sequential, so the failure reported is the one
that occurs first when the stream is processed in the order of section 12.1,
each block being taken completely through steps 1–9 before the next end flag
is decoded. In particular:

- Within a block, a truncation met while decoding its symbols comes before a capacity or origin-index failure of that block, which comes before an output overflow caused by that block.
- An output overflow in a block comes before anything in later blocks; decoding stops there.
- At the end of the stream the order is: truncation (while decoding the checksum or performing the doublings after it), then length mismatch, then checksum mismatch.
- At the start of the stream the order is: a decoder's own output cap, if it has one (nothing has been read yet); then truncation while the 26 bits are loaded; then the first-bit check; then the signature, during which truncation can occur again.
- "Out of memory" has no fixed place in the order: it is reported at the moment the memory is refused, in place of whatever the stream would have led to next.

**Every failure fails the whole fork.** There is no per-block recovery: a
failure in one block does not leave the earlier blocks as a valid result.

### 12.3 Partial output

A stream is valid only if the final end flag was decoded, the checksum and the
doublings after it were decoded without running out of input, the output
length equals the unpacked length, and the checksum matches. Nothing less may
be presented as the content of the fork.

As with every method (`containers.md` section 2.2), a decoder may nevertheless
make the bytes produced before a failure available through a separate,
explicitly marked salvage facility, provided the failure is reported with
them. For that purpose the
partial output is: the complete output of all blocks before the failing one (of
all blocks, when the failure occurs after the last block);
for an output overflow, additionally the failing block's output up to the
unpacked length; for a length mismatch or a checksum mismatch, the entire
output. A decoder that delivers output incrementally, block by block, may
already have delivered such bytes when the failure is detected; it must then
report the failure no later than the point at which the consumer would
otherwise see the end of the data.

The reference implementation returns partial or unverified data as a success in
several of these situations (section 17); nothing in the format calls for
that, and callers must not be given that behaviour.

## 13. Worked example

### 13.1 Provenance of the synthetic streams

No genuine StuffIt compressor was available. The streams in sections 13 to 15
were produced by an arithmetic encoder that is the exact inverse of section 4,
driven with the symbols this specification prescribes, including the trailing
checksum, and writing out the complete final code value so that no requested
bit is missing (section 3). Every stream marked as valid is decoded
identically by the reference implementation, by the independent tool (BLACK-BOX,
which also verifies the checksum) and by the independent decoder that
reproduces all genuine streams. Their last bits (at most the last 25
requested ones, section 11.5) are not necessarily those a genuine compressor
would emit: these streams all end with a final code value of 0.

### 13.2 Stream

Packed stream, 18 bytes (144 bits):

```
42 C1 C5 67 E8 66 E4 9E 54 4A 2B 0C 44 6D 9D 11 AF C0
```

Decoded output, 11 bytes, ASCII `bananaaaaaa`:

```
62 61 6E 61 6E 61 61 61 61 61 61
```

### 13.3 Initialisation and first symbol

The first four bytes in binary are `01000010 11000001 11000101 01100111`. The
first 26 bits, `01000010110000011100010101`, give C = 17,499,925, which is less
than 2^25. R = 33,554,432. The next unread bit is bit 5 of the fourth byte
(value 1).

First symbol (primary model, frequencies (1, 1), total 2):

- unit = 33,554,432 / 2 = 16,777,216; target = 17,499,925 / 16,777,216 = 1.
- Symbol 0 has below + width = 0 + 1 = 1, not greater than 1; symbol 1 has below + width = 1 + 1 = 2, greater than 1, so symbol 1 is decoded, with below = 1, width = 1.
- C becomes 17,499,925 − 16,777,216 × 1 = 722,709. Symbol 1 is the last symbol in the list, so R becomes 33,554,432 − 16,777,216 = 16,777,216.
- R is equal to 2^24, so one doubling takes place: R = 33,554,432; C = 2 × 722,709 + 1 (the next input bit) = 1,445,419. R is now above 2^24.
- Model update: frequencies become (1, 2), total 3.
- Decoded value: 1. This is bit 0 of the first signature byte.

### 13.4 Complete symbol trace

Each row shows the state **before** the symbol is decoded. Total, Below and
Width refer to the model named; Unit and Target are as defined in section 4.3;
Symbol is the value delivered (base value included); Doublings is the number of
renormalisation doublings, i.e. input bits consumed, after the symbol.

| # | Decoding | Model | Total | Range | Code | Unit | Target | Below | Width | Symbol | Doublings |
|--:|---|---|--:|--:|--:|--:|--:|--:|--:|--:|--:|
| 1 | signature 1 bit 0 | primary | 2 | 33554432 | 17499925 | 16777216 | 1 | 1 | 1 | 1 | 1 |
| 2 | signature 1 bit 1 | primary | 3 | 33554432 | 1445419 | 11184810 | 0 | 0 | 1 | 0 | 1 |
| 3 | signature 1 bit 2 | primary | 4 | 22369620 | 2890838 | 5592405 | 0 | 0 | 2 | 0 | 1 |
| 4 | signature 1 bit 3 | primary | 5 | 22369620 | 5781676 | 4473924 | 1 | 0 | 3 | 0 | 1 |
| 5 | signature 1 bit 4 | primary | 6 | 26843544 | 11563353 | 4473924 | 2 | 0 | 4 | 0 | 0 |
| 6 | signature 1 bit 5 | primary | 7 | 17895696 | 11563353 | 2556528 | 4 | 0 | 5 | 0 | 1 |
| 7 | signature 1 bit 6 | primary | 8 | 25565280 | 23126707 | 3195660 | 7 | 6 | 2 | 1 | 2 |
| 8 | signature 1 bit 7 | primary | 9 | 25565280 | 15810991 | 2840586 | 5 | 0 | 6 | 0 | 0 |
| 9 | signature 2 bit 0 | primary | 10 | 17043516 | 15810991 | 1704351 | 9 | 7 | 3 | 1 | 2 |
| 10 | signature 2 bit 1 | primary | 11 | 20452236 | 15522139 | 1859294 | 8 | 7 | 4 | 1 | 2 |
| 11 | signature 2 bit 2 | primary | 12 | 29748712 | 10028325 | 2479059 | 4 | 0 | 7 | 0 | 0 |
| 12 | signature 2 bit 3 | primary | 13 | 17353413 | 10028325 | 1334877 | 7 | 0 | 8 | 0 | 1 |
| 13 | signature 2 bit 4 | primary | 14 | 21358032 | 20056650 | 1525573 | 13 | 9 | 5 | 1 | 2 |
| 14 | signature 2 bit 5 | primary | 15 | 30511500 | 25305972 | 2034100 | 12 | 9 | 6 | 1 | 1 |
| 15 | signature 2 bit 6 | primary | 16 | 24409200 | 13998144 | 1525575 | 9 | 9 | 7 | 1 | 1 |
| 16 | signature 2 bit 7 | primary | 17 | 21358050 | 535939 | 1256355 | 0 | 0 | 9 | 0 | 1 |
| 17 | block-size field bit 0 | primary | 18 | 22614390 | 1071879 | 1256355 | 0 | 0 | 10 | 0 | 1 |
| 18 | block-size field bit 1 | primary | 19 | 25127100 | 2143758 | 1322478 | 1 | 0 | 11 | 0 | 1 |
| 19 | block-size field bit 2 | primary | 20 | 29094516 | 4287516 | 1454725 | 2 | 0 | 12 | 0 | 0 |
| 20 | block-size field bit 3 | primary | 21 | 17456700 | 4287516 | 831271 | 5 | 0 | 13 | 0 | 1 |
| 21 | end flag | primary | 22 | 21613046 | 8575033 | 982411 | 8 | 0 | 14 | 0 | 1 |
| 22 | randomisation flag | primary | 23 | 27507508 | 17150067 | 1195978 | 14 | 0 | 15 | 0 | 0 |
| 23 | origin index bit 0 | primary | 24 | 17939670 | 17150067 | 747486 | 22 | 16 | 8 | 1 | 2 |
| 24 | origin index bit 1 | primary | 25 | 23919576 | 20761165 | 956783 | 21 | 16 | 9 | 1 | 1 |
| 25 | origin index bit 2 | primary | 26 | 17222096 | 10905275 | 662388 | 16 | 16 | 10 | 1 | 2 |
| 26 | origin index bit 3 | primary | 27 | 26495552 | 1228270 | 981316 | 1 | 0 | 16 | 0 | 1 |
| 27 | origin index bit 4 | primary | 28 | 31402112 | 2456540 | 1121504 | 2 | 0 | 17 | 0 | 0 |
| 28 | origin index bit 5 | primary | 29 | 19065568 | 2456540 | 657433 | 3 | 0 | 18 | 0 | 1 |
| 29 | origin index bit 6 | primary | 30 | 23667588 | 4913081 | 788919 | 6 | 0 | 19 | 0 | 1 |
| 30 | origin index bit 7 | primary | 31 | 29978922 | 9826162 | 967062 | 10 | 0 | 20 | 0 | 0 |
| 31 | origin index bit 8 | primary | 32 | 19341240 | 9826162 | 604413 | 16 | 0 | 21 | 0 | 1 |
| 32 | class symbol | class | 88 | 25385346 | 19652324 | 288469 | 68 | 64 | 8 | 8 | 3 |
| 33 | rank symbol | rank 6 | 128 | 18462016 | 9522468 | 144234 | 66 | 66 | 2 | 97 | 6 |
| 34 | class symbol | class | 96 | 18461952 | 193596 | 192312 | 1 | 0 | 8 | 0 | 4 |
| 35 | class symbol | class | 104 | 24615936 | 3097546 | 236691 | 13 | 0 | 16 | 0 | 3 |
| 36 | class symbol | class | 112 | 30296448 | 24780372 | 270504 | 91 | 80 | 16 | 8 | 2 |
| 37 | rank symbol | rank 6 | 130 | 17312256 | 12560209 | 133171 | 94 | 94 | 2 | 110 | 6 |
| 38 | class symbol | class | 120 | 17045888 | 2696650 | 142049 | 18 | 0 | 24 | 0 | 3 |
| 39 | class symbol | class | 128 | 27273408 | 21573201 | 213073 | 101 | 88 | 24 | 8 | 2 |
| 40 | rank symbol | rank 6 | 132 | 20455008 | 11291109 | 154962 | 72 | 72 | 2 | 99 | 6 |
| 41 | class symbol | class | 136 | 19835136 | 8566104 | 145846 | 58 | 56 | 8 | 4 | 4 |
| 42 | rank symbol | rank 2 | 16 | 18668288 | 6379654 | 1166768 | 5 | 4 | 4 | 5 | 2 |
| 43 | class symbol | class | 144 | 18668288 | 6850328 | 129640 | 52 | 48 | 8 | 3 | 5 |
| 44 | rank symbol | rank 1 | 16 | 33187840 | 20083473 | 2074240 | 9 | 8 | 8 | 3 | 1 |
| 45 | class symbol | class | 152 | 33187840 | 6979106 | 218341 | 31 | 0 | 32 | 0 | 2 |
| 46 | class symbol | class | 160 | 27947648 | 27916424 | 174672 | 159 | 152 | 8 | 10 | 4 |
| 47 | end flag | primary | 33 | 22360064 | 21860493 | 677577 | 32 | 22 | 11 | 1 | 2 |
| 48 | checksum bit 0 | primary | 34 | 29813480 | 27815198 | 876867 | 31 | 22 | 12 | 1 | 1 |
| 49 | checksum bit 1 | primary | 35 | 21044812 | 17048249 | 601280 | 28 | 22 | 13 | 1 | 2 |
| 50 | checksum bit 2 | primary | 36 | 31266608 | 15280358 | 868516 | 17 | 0 | 22 | 0 | 0 |
| 51 | checksum bit 3 | primary | 37 | 19107352 | 15280358 | 516414 | 29 | 23 | 14 | 1 | 2 |
| 52 | checksum bit 4 | primary | 38 | 28919320 | 13611345 | 761034 | 17 | 0 | 23 | 0 | 0 |
| 53 | checksum bit 5 | primary | 39 | 17503782 | 13611345 | 448814 | 30 | 24 | 15 | 1 | 2 |
| 54 | checksum bit 6 | primary | 40 | 26928984 | 11359239 | 673224 | 16 | 0 | 24 | 0 | 1 |
| 55 | checksum bit 7 | primary | 41 | 32314752 | 22718478 | 788164 | 28 | 25 | 16 | 1 | 1 |
| 56 | checksum bit 8 | primary | 42 | 25221304 | 6028757 | 600507 | 10 | 0 | 25 | 0 | 1 |
| 57 | checksum bit 9 | primary | 43 | 30025350 | 12057514 | 698263 | 17 | 0 | 26 | 0 | 0 |
| 58 | checksum bit 10 | primary | 44 | 18154838 | 12057514 | 412609 | 29 | 27 | 17 | 1 | 2 |
| 59 | checksum bit 11 | primary | 45 | 28057580 | 3668284 | 623501 | 5 | 0 | 27 | 0 | 0 |
| 60 | checksum bit 12 | primary | 46 | 16834527 | 3668284 | 365967 | 10 | 0 | 28 | 0 | 1 |
| 61 | checksum bit 13 | primary | 47 | 20494152 | 7336569 | 436045 | 16 | 0 | 29 | 0 | 1 |
| 62 | checksum bit 14 | primary | 48 | 25290610 | 14673138 | 526887 | 27 | 0 | 30 | 0 | 1 |
| 63 | checksum bit 15 | primary | 49 | 31613220 | 29346276 | 645167 | 45 | 31 | 18 | 1 | 1 |
| 64 | checksum bit 16 | primary | 50 | 23226086 | 18692198 | 464521 | 40 | 31 | 19 | 1 | 1 |
| 65 | checksum bit 17 | primary | 51 | 17651870 | 8584095 | 346115 | 24 | 0 | 31 | 0 | 1 |
| 66 | checksum bit 18 | primary | 52 | 21459130 | 17168191 | 412675 | 41 | 32 | 20 | 1 | 2 |
| 67 | checksum bit 19 | primary | 53 | 33014120 | 15850365 | 622907 | 25 | 0 | 32 | 0 | 0 |
| 68 | checksum bit 20 | primary | 54 | 19933024 | 15850365 | 369130 | 42 | 33 | 21 | 1 | 2 |
| 69 | checksum bit 21 | primary | 55 | 31006936 | 14676301 | 563762 | 26 | 0 | 33 | 0 | 0 |
| 70 | checksum bit 22 | primary | 56 | 18604146 | 14676301 | 332216 | 44 | 34 | 22 | 1 | 2 |
| 71 | checksum bit 23 | primary | 57 | 29235208 | 13523831 | 512898 | 26 | 0 | 34 | 0 | 0 |
| 72 | checksum bit 24 | primary | 58 | 17438532 | 13523831 | 300664 | 44 | 35 | 23 | 1 | 2 |
| 73 | checksum bit 25 | primary | 59 | 27661168 | 12002367 | 468833 | 25 | 0 | 35 | 0 | 1 |
| 74 | checksum bit 26 | primary | 60 | 32818310 | 24004735 | 546971 | 43 | 36 | 24 | 1 | 1 |
| 75 | checksum bit 27 | primary | 61 | 26254708 | 8627558 | 430405 | 20 | 0 | 36 | 0 | 1 |
| 76 | checksum bit 28 | primary | 62 | 30989160 | 17255116 | 499825 | 34 | 0 | 37 | 0 | 0 |
| 77 | checksum bit 29 | primary | 63 | 18493525 | 17255116 | 293548 | 58 | 38 | 25 | 1 | 2 |
| 78 | checksum bit 30 | primary | 64 | 29354804 | 24401168 | 458668 | 53 | 38 | 26 | 1 | 1 |
| 79 | checksum bit 31 | primary | 65 | 23850840 | 13943568 | 366936 | 38 | 38 | 27 | 1 | 1 |

Bit accounting: 26 bits are loaded at the start and the Doublings column sums
to 118, so the decoder requests 26 + 118 = 144 bits in all — exactly the 144
bits of the 18 packed bytes. After row 31, 56 bits have been requested;
after row 46, 109; after row 47, 111; after row 78, 143; after row 79, 144.

### 13.5 Interpreting the symbols

- Rows 1–8, least significant bit first: 1,0,0,0,0,0,1,0 → 0x41. Rows 9–16: 1,1,0,0,1,1,1,0 → 0x73. Signature accepted.
- Rows 17–20: 0,0,0,0 → block-size field 0 → block capacity 512; origin indices are 9 bits wide.
- Row 21: end flag 0 → a block follows. Row 22: randomisation flag 0.
- Rows 23–31: 1,1,1,0,0,0,0,0,0 → origin index 7.
- Rows 32–46, block contents:

| Rows | Symbols | Action | Byte(s) appended | Front of the list afterwards |
|---|---|---|---|---|
| 32–33 | class 8, rank 97 | apply rank 97 | 61 | 61 00 01 02 … |
| 34–35 | class 0, 0 | zero run, length 1 + 2 = 3 (appended when row 36 arrives) | 61 61 61 | unchanged |
| 36–37 | class 8, rank 110 | apply rank 110 | 6E | 6E 61 00 01 … |
| 38 | class 0 | zero run, length 1 (appended when row 39 arrives) | 6E | unchanged |
| 39–40 | class 8, rank 99 | apply rank 99 | 62 | 62 6E 61 00 01 02 … |
| 41–42 | class 4, rank 5 | apply rank 5 | 02 | 02 62 6E 61 00 01 … |
| 43–44 | class 3, rank 3 | apply rank 3 | 61 | 61 02 62 6E 00 01 … |
| 45 | class 0 | zero run, length 1 (appended when row 46 arrives) | 61 | unchanged |
| 46 | class 10 | end of block | | |

  Sorted block (*n* = 10): `61 61 61 61 6E 6E 62 02 61 61`. The origin index 7 is less than 10.

- Row 47: end flag 1 → no more blocks. Rows 48–79: the checksum, least significant bit first → 0xE55584AB.

### 13.6 Inverse block-sorting transform

Counts: 0x02 ×1, 0x61 ×6, 0x62 ×1, 0x6E ×2. First slots: 0x02 → 0, 0x61 → 1,
0x62 → 7, 0x6E → 8.

Building the successor table by going through the sorted block in order:

| Byte number *i* in sorted block | 0 | 1 | 2 | 3 | 4 | 5 | 6 | 7 | 8 | 9 |
|---|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|
| Byte value | 61 | 61 | 61 | 61 | 6E | 6E | 62 | 02 | 61 | 61 |
| *i* is written into successor entry | 1 | 2 | 3 | 4 | 8 | 9 | 7 | 0 | 5 | 6 |

Resulting successor table:

| Entry | 0 | 1 | 2 | 3 | 4 | 5 | 6 | 7 | 8 | 9 |
|---|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|
| Value | 7 | 0 | 1 | 2 | 3 | 8 | 9 | 6 | 4 | 5 |

Walk, starting from *p* = 7 (the origin index):

| Step | 1 | 2 | 3 | 4 | 5 | 6 | 7 | 8 | 9 | 10 |
|---|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|
| New *p* | 6 | 9 | 5 | 8 | 4 | 3 | 2 | 1 | 0 | 7 |
| Output byte | 62 | 61 | 6E | 61 | 6E | 61 | 61 | 61 | 61 | 02 |

The block is not randomised, so these are the run-coded bytes:
`62 61 6E 61 6E 61 61 61 61 02`.

### 13.7 Run expansion

| Run-coded byte | 62 | 61 | 6E | 61 | 6E | 61 | 61 | 61 | 61 | 02 |
|---|---|---|---|---|---|---|---|---|---|---|
| Counter before | 0 | 1 | 1 | 1 | 1 | 1 | 1 | 2 | 3 | 4 |
| Treated as | literal | literal | literal | literal | literal | literal | literal | literal | literal | count |
| Counter after | 1 | 1 | 1 | 1 | 1 | 1 | 2 | 3 | 4 | 0 |
| Output | 62 | 61 | 6E | 61 | 6E | 61 | 61 | 61 | 61 | 61 61 |

Output: `62 61 6E 61 6E 61 61 61 61 61 61`, 11 bytes. Its CRC-32 is 0xE55584AB,
equal to the decoded checksum.

## 14. A fallback case in detail

Rule 3 of section 4.3 says the last symbol is decoded when no symbol's slice
contains the target. Vector P9 of section 15 shows it at its 36th symbol, the
end-of-block class symbol of its only block:

| Quantity | Value |
|---|--:|
| Model | class, total 104, frequencies (8, 8, 8, 16, 8, 8, 8, 8, 16, 8, 8) |
| R | 25,949,312 |
| C | 25,949,280 |
| unit = R / 104 | 249,512 |
| target = C / unit | 104 |
| below, width of the last symbol | 96, 8 |

The target equals the total, so no symbol has below + width greater than it,
and the last symbol (value 10, end of block) is decoded. C becomes 25,949,280 −
249,512 × 96 = 1,996,128; R becomes 25,949,312 − 249,512 × 96 = 1,996,160; four
doublings follow, giving R = 31,938,560.

Vector P10 shows the same thing twice in the primary model, at its 46th and
47th symbols (checksum bits 8 and 9): total 42, R = 29,034,568, C = 29,034,564,
unit 691,299, target 42, decoded value 1; then total 43, R = 33,182,392, C =
33,182,378, unit 771,683, target 43, decoded value 1.

## 15. Test vectors

Same provenance as section 13.1. "P" vectors are valid streams; "R" vectors
must be rejected. The unpacked length is the value the container would supply.
The vectors with unpacked length 0 (P1, R5, R6) apply to a decoder called
directly; the container layer never has such a fork decoded (section 1.3).

### 15.1 Valid streams

All use block-size field 0 (capacity 512, 9-bit origin index) except P11.

| # | Packed stream (hex) | Unpacked length | Checksum | Purpose |
|--:|---|--:|---|---|
| P1 | `42 C1 C6 A3 FD E8 00 00` | 0 | 0x00000000 | No blocks: end flag 1 directly after the header. 61 bits requested. |
| P2 | `42 C1 C3 61 8F 71 3D E7 5F 0C DA B8 90 00` | 5 | 0xC622F71D | Zero run at the start of a block; repeat count. |
| P3 | `42 C1 C3 70 59 89 7C DE 5B 1A C6 78 2D 00 D4 D5 E2 4A F0` | 7 | 0xE487AEF7 | Two blocks; model reset and fresh move-to-front list in the second. |
| P4 | `42 C1 C3 70 49 53 A3 A4 47 09 48 43 0B 1E B7 77 00` | 6 | 0x94264167 | First block ends with the repeat counter at 4; the count is dropped and the next block starts with a literal. |
| P5 | `42 C1 C4 0F 39 89 F7 EE 35 01 6A 96 46 E6 40` | 5 | 0x835C664A | Last block of the stream ends with the repeat counter at 4. |
| P6 | `42 C1 C3 70 49 F3 78 B8 EA 10 20 10 D1 5C 94 4B 62 00` | 5 | 0xA47336F8 | Repeat counter is not carried across a block boundary. |
| P7 | `42 C1 C6 76 47 C5 FD C0 61 B9 F8 8B 8F 41 96 C0` | 240 | 0xBC612413 | Randomised block of 300 bytes: one flip, at position 238. |
| P8 | `42 C1 C3 70 6C 13 89 51 3E 27 BC 90 23 23 33 DA E3 A0 0D B0 00` | 512 | 0xA5E8BF85 | Block length exactly equal to the block capacity. |
| P9 | `42 C1 C3 70 67 C3 5B D8 03 72 46 7E 16 00` | 2 | 0x121D86FF | Fallback of section 4.3 rule 3 in the class model (section 14). |
| P10 | `42 C1 C3 73 A9 EB 37 A3 90 D9 2E 46 84 10` | 2 | 0xDEFFFF0B | The same fallback, twice, in the primary model (section 14). |

Contents of these vectors:

| # | Block | Randomised | Origin index | Sorted block | Run-coded bytes | Output of the block |
|--:|--:|--:|--:|---|---|---|
| P2 | 1 | 0 | 0 | `01 00 00 00 00` | `00 00 00 00 01` | `00 00 00 00 00` |
| P3 | 1 | 0 | 0 | `62 61` | `61 62` | `61 62` |
| P3 | 2 | 0 | 2 | `62 62 61 61 61` | `61 62 61 62 61` | `61 62 61 62 61` |
| P4 | 1 | 0 | 0 | `61 61 61 61` | `61 61 61 61` | `61 61 61 61` |
| P4 | 2 | 0 | 0 | `62 03` | `03 62` | `03 62` |
| P5 | 1 | 0 | 4 | `78 61 61 61 61` | `78 61 61 61 61` | `78 61 61 61 61` |
| P6 | 1 | 0 | 0 | `61 61` | `61 61` | `61 61` |
| P6 | 2 | 0 | 2 | `61 61 05` | `61 61 05` | `61 61 05` |
| P7 | 1 | 1 | 61 | 300 bytes: 0x00 except one 0x01 (CRC-32 0x95063067) | after undoing randomisation: 300 × 0x00 | 240 × 0x00 (sixty groups of four literals, each followed by a count of 0) |
| P8 | 1 | 0 | 0 | 512 bytes (CRC-32 0xAFE833BE) | 255 × `61 62`, then `63 64` | the same 512 bytes |
| P9 | 1 | 0 | 0 | `64 01` | `01 64` | `01 64` |
| P10 | 1 | 0 | 0 | `FC 03` | `03 FC` | `03 FC` |

In P7 the inverse block-sorting transform delivers 0x01 at position 238 and
0x00 everywhere else; undoing randomisation turns that byte into 0x00.

### 15.2 A larger stream (P11)

Block-size field 10 (capacity 524,288, 19-bit origin index), 14 blocks, 487
packed bytes, 3,891 of its 3,896 bits requested. Unpacked length 36,070;
checksum 0xF7F792D7. The output begins `B8 E0 40 82 9A 6C 8A 92 C2 4A 30 4E 72
44 DE 22` and ends with the ASCII text `overthelazydog`.

```
42 C1 D5 88 4C 15 5C 86 3B 6C A1 A5 71 3C 1C 84
F9 4B D4 A2 F5 36 36 14 75 89 07 93 0E 1C 78 3E
B9 7E E4 75 08 D0 30 05 B9 EF 40 2A DB A5 9A 78
B2 59 8A B9 1A EA 06 11 A1 4E 63 BC 33 A0 FA D3
30 A1 A8 E1 5B 4B 97 1F 97 A0 F1 C6 10 75 44 C2
E2 D4 59 17 42 6C 0D 8F 1C 74 F0 D5 B9 DC 99 BD
2C 28 13 9F 28 31 27 07 6C 03 37 2E 40 9B 27 F4
3C 2A 62 DF 74 AA 4D 42 61 64 98 B5 AA E4 58 24
55 8E 2D D6 7B C5 28 00 21 4D 24 E1 5C F8 89 AB
39 E1 45 71 88 32 8F D1 78 5D BC E5 38 F4 4B 29
36 AB 61 43 10 EB 7F 9E C0 65 4B 9C AE D7 E2 9C
38 42 38 9D D1 90 9F D9 19 2C 8F D3 E7 F2 99 F0
3F 5D 08 47 60 B5 DB A9 B0 71 CE 28 BB 0A E7 D1
4A 79 DA AF ED BF 46 D0 53 87 D5 81 68 6C 05 A6
C1 21 9B FA C1 5F 71 ED 1F 60 E2 26 61 7A 1D 8B
DC 00 2F D9 9A D4 24 0B 5E 5A 05 7F 7D CF 0A B6
89 DA 0F 59 F2 1C 55 2E 43 D9 86 55 6A 3D 29 D3
04 AA 4F 7C C4 08 33 0D 09 A2 44 FD 45 22 9D 19
EE DC 81 A7 69 C9 C9 81 50 CA 57 B0 95 2E 6E EC
4B 33 0C 91 BE B7 9B 23 89 6D 50 7A 2A 93 BF 56
10 A0 A2 4C 2F DC B3 D0 1B 12 47 FE 9A 56 0A 75
D3 04 15 38 6B 9D A1 D6 1B 5E 5F AA 71 CF C5 46
BB 91 B4 5E 7B F6 19 11 FE C8 37 CC F8 7B C2 27
68 A7 CB B5 68 3D 74 96 B6 E3 07 69 C7 70 A3 E1
32 2D 84 39 04 F5 4F 11 98 6E D4 BC B1 B7 4C E9
2F 12 C1 1F 67 23 30 E1 9D BF 9D EE 67 AF 23 88
FC D5 1A 17 32 B6 02 04 4E 43 2C C8 EC D5 76 D6
FA E8 FD F4 EC FB 67 02 79 A1 B4 A6 14 2B 01 60
46 E7 0C 8B 7D CA C3 F6 BA F9 CB 0F 15 B5 51 E5
8C 9A B8 EB 52 EC CD 5B 5C FC 90 22 AB D5 14 95
BF 8B AD 1B 75 2A 00
```

| Block | Randomised | Origin index | Block length *n* | CRC-32 of sorted block | CRC-32 of run-coded bytes | Output bytes | Output offset |
|--:|--:|--:|--:|---|---|--:|--:|
| 1 | 1 | 24,421 | 33,100 | 0x28DB190A | 0x0AD25B23 | 33,100 | 0 |
| 2 | 0 | 137 | 520 | 0x96757ECC | 0x129589FC | 520 | 33,100 |
| 3 | 0 | 22 | 64 | 0xBCA9081E | 0x6AFFAB8C | 2,138 | 33,620 |
| 4 | 1 | 26 | 37 | 0x7F437034 | 0xE8A4A4EA | 37 | 35,758 |
| 5 | 1 | 0 | 240 | 0x5D5C405F | 0x719CB611 | 240 | 35,795 |
| 6 | 0 | 2 | 3 | 0xCF0E6AAF | 0x3C456DE6 | 3 | 36,035 |
| 7 | 0 | 3 | 5 | 0xE57B3430 | 0x8DC71ED7 | 5 | 36,038 |
| 8 | 0 | 0 | 5 | 0xCB7B6FC6 | 0x6D199454 | 5 | 36,043 |
| 9 | 0 | 0 | 3 | 0x4B095012 | 0x0FA0E0E6 | 3 | 36,048 |
| 10 | 0 | 0 | 5 | 0xE6E6946F | 0xEF7DC95C | 5 | 36,051 |
| 11 | 0 | 1 | 4 | 0xFDE27F49 | 0xB4FCA359 | 4 | 36,056 |
| 12 | 0 | 2 | 3 | 0xCF0E6AAF | 0x3C456DE6 | 3 | 36,060 |
| 13 | 0 | 1 | 4 | 0xE4F104CA | 0xE5A86E54 | 4 | 36,063 |
| 14 | 0 | 0 | 3 | 0x1A63D69E | 0x812C397D | 3 | 36,067 |

What this vector exercises:

- **Symbols.** 2,068 symbols in all: primary 347, class 1,293, rank 1 258, rank 2 17, rank 3 11, rank 4 2, rank 5 22, rank 6 81, rank 7 37. Every class symbol 0 … 10 and every rank model is used.
- **Randomisation.** Block 1 contains 258 flip positions; the last four are 32,717, 32,740, 32,978 and 33,064, so the table is used past its end and restarted from entry 0. Block 4 is flagged as randomised but is too short to contain a flip position. Block 5 is randomised and contains exactly one flip position (238), which shows that the sequence restarts in each block.
- **Model halving.** The class model is halved 8 times in block 1 (first at the 179th symbol of the stream, section 4.4) and 6 times in block 2; rank 1 is halved twice in block 2. The primary model is halved once, at the 1,953rd symbol (bit 1 of the origin index of block 12, section 4.4); the symbols after it, including the checksum, depend on the result.
- **Fallback (section 4.3 rule 3).** Twice: the 2,035th symbol (the end-of-block symbol of block 14: class total 112, R = 20,328,064, C = 20,328,030, unit 181,500, target 112) and the 2,057th symbol (checksum bit 20: primary total 209, R = 24,579,625, C = 24,579,606, unit 117,605, target 209).
- **Run expansion.** Block 3 expands 64 run-coded bytes to 2,138 output bytes, with repeat counts of 0, 1, 255 and other values; its output begins `41 41 41 41 42 42 42 42 42 43`.
- **Model reset.** Blocks 6 and 12 have identical contents (`74 68 65`) and identical sorted blocks, decoded after very different histories.
- **Position of the end flags.** The end flags are the 21st, 734th, 1,502nd, 1,597th, 1,686th, 1,768th, 1,796th, 1,828th, 1,860th, 1,888th, 1,920th, 1,950th, 1,978th and 2,008th symbols (value 0) and the 2,036th (value 1).

Not exercised by any vector here: halving of rank models 2 to 7 (the rule is
the same for every model), and a block-size field other than 0 and 10 (all
sixteen were tested BLACK-BOX with the data of section 13).

### 15.3 Streams that must be rejected

| # | Packed stream (hex) | Unpacked length | Failure (section 12.2) | What is wrong |
|--:|---|--:|---|---|
| R1 | `42 C1 C5 67 E8 66 E4 9E 54 4A 29 A8 FA E5 D3 ED 7F 20` | 11 | Checksum mismatch | The stream of section 13 with bit 0 of the checksum inverted (decoded checksum 0xE55584AA). |
| R2 | `42 C1 C4 5B 53 25 F9 B2 50 1B 5C 49 08 70` | 2 | Corrupt block | Sorted block `62 61`, origin index 2: not less than the block length 2. |
| R3 | `42 C1 C3 70 62 CC BE D0 41 A0 A0 5C A9 A4 05 AF C4 2B DB 60` | 513 | Corrupt block | Block-size field 0 and a block of 513 bytes. |
| R4 | `42 C1 C3 5C EA 7F 20 1A A0 E2 EB 3C 38 A0` | 4 | Corrupt block | Block-size field 0; the block starts with a zero run of length 513. |
| R5 | `42 C1 EC 1D DE 96 71 F8 59 40 76 10 E8 00 00` | 0 | Corrupt block | Block-size field 15; the block consists of forty zero-run digits of value 2, describing a run of 2 × (2^40 − 1) bytes. The run exceeds the capacity at the 24th digit; it must be rejected there, without numeric overflow and without reserving memory. |
| R6 | `42 C1 C3 75 7C CC 0A 00 00` | 0 | Corrupt block | One block of length 0 (origin index 0), then end flag 1 and checksum 0. |
| R7 | `42 C1 C5 67 E8 66 E4 9E 54 4A 2B 0C 44 6D 9D 11 AF` | 11 | Truncated | The stream of section 13 without its last byte (136 bits). Decoding proceeds exactly as in section 13.4 up to row 72; the second doubling after that row requests bit 137. |
| R8 | `42 C1 C5 67 E8 66 E4 9E 54 4A` | 11 | Truncated | The stream of section 13 cut after 10 bytes (80 bits). The first doubling after row 38 of section 13.4 requests bit 81. |
| R9 | `C2 C1 C5 67 E8 66 E4 9E 54 4A 2B 0C 44 6D 9D 11 AF C0` | 11 | Not an Arsenic stream | First input bit is 1. |
| R10 | `42 85 DB A1 89 24 00 DC AE 34 56 27 DA DD 03 3D 6D E0` | 11 | Not an Arsenic stream | Second signature byte decodes to 0x74. |
| R11 | `42 C1 C5 67 E8 66 E4 9E 54 4A 2B 0C 44 6D 9D 11 AF C0` | 10 | Output overflow | The valid stream of section 13 (11 bytes of output) with an unpacked length of 10. |
| R12 | `42 C1 C5 67 E8 66 E4 9E 54 4A 2B 0C 44 6D 9D 11 AF C0` | 12 | Length mismatch | The same stream with an unpacked length of 12. |
| R13 | `80` | 1 | Truncated | 8 bits, first bit 1. Fewer than 26 bits exist, so the first-bit check is never reached (section 12.2). |
| R14 | `C2 C1 C5` | 11 | Truncated | The first three bytes of R9: 24 bits, first bit 1. Still truncated. |
| R15 | `C2 C1 C5 67` | 11 | Not an Arsenic stream | The first four bytes of R9: 32 bits, so the 26 bits load and the first-bit check fails. |

BLACK-BOX behaviour of the independent tool on these: it fails R1, R2, R3, R4,
R7, R8, R9, R10 and R11. It does not decode R5 and R6 at all (it produces
nothing when the unpacked length is 0), and it accepts R12, returning the 11
bytes. The classification of R12 as a failure is this specification's DECISION
(section 18). R13, R14 and R15 were not given to the independent tool: they
differ only in which kind of failure is named, which a black box does not
show, and all three are the fork error "corrupt".

## 16. Safety requirements for untrusted input

A decoder must not trust any quantity in the stream or the container. Required
checks and bounds:

1. **Start of stream.** Packed bytes of zero length are rejected before decoding, by the container layer (section 1.3). Packed bytes of fewer than 26 bits are "truncated", whatever they contain. Reject the stream if the first input bit is 1 (section 4.1). Reject it unless the two signature bytes decode to 0x41, 0x73.
2. **Block capacity.** The field is 4 bits, so the capacity is between 512 and 2^24 = 16,777,216 bytes and needs no range check. It is a bound, not a size to prepare for: memory for a block must grow with the bytes actually decoded and must not be reserved merely because the header announces a large capacity. The working data for a block of *n* bytes is the *n*-byte sorted block and a successor table of *n* entries, each holding a number below *n* (so below 2^24). If the host refuses that memory, the result is the "out of memory" outcome of section 12.2: reported, not fatal to the program, and not "corrupt".
3. **Block length.** Reject the stream as soon as the sorted block would exceed the block capacity. This must also cover zero runs **while their digits are arriving**: the weight doubles with every digit, so a hostile stream can describe a run of astronomically many bytes in a few dozen symbols (vector R5). After each digit, compare the accumulated run length with the space remaining in the block (capacity minus bytes already in the sorted block) and reject if it is larger. Applied this way, the weight of a digit never exceeds 2^24 and the accumulated run length never exceeds 3 × 2^24 at the moment it is compared, so neither can overflow a 32-bit quantity; and no byte of a run is appended before its length has passed the check.
4. **Origin index.** Reject the stream unless the origin index is less than the block length *n*. This is required, not optional, and it is also what rejects a block of length 0.
5. **Ranks.** Ranks delivered by the models are at most 255 by construction; the move-to-front list always has exactly 256 entries.
6. **Class symbols.** The class model can only deliver 0 … 10, all of which are defined.
7. **Arithmetic state.** With the model limits given, a total never exceeds 1,024 when a symbol is decoded and R is always greater than 2^24, so the unit is at least 16,384 and division by zero cannot occur. With the first-bit check of requirement 1, C stays below R and below 2^25 for the whole stream, for any input.
8. **End of input.** Fail with "truncated" at the first request for a bit beyond the end of the packed bytes (section 3). Never substitute bits: with substituted zero bits a truncated stream decodes as an endless sequence of plausible symbols.
9. **Output length.** Run expansion can turn 5 run-coded bytes into 259 output bytes, so one block can produce nearly 52 times its length. Never append beyond the unpacked length; an attempt to do so is the "output overflow" failure. Do not reserve output memory from the container's unpacked length without an independent limit, since that length is itself untrusted.
10. **Completion.** Treat the stream as valid only under the conditions of section 12.3.
11. **Progress.** Every accepted block has at least one byte (requirement 4), and at most one run-coded byte in every five is a repeat count, so a block of *n* run-coded bytes appends at least four fifths of *n* bytes, and never fewer than one, to the output. The output is bounded by the unpacked length (requirement 9), each block by the capacity (requirement 3), and the input cannot be read past its end (requirement 8). Together these bound the number of blocks and the total work by the unpacked length plus one block capacity.
12. **Flip positions.** The table index wraps from 255 to 0. The next flip position to be compared can exceed the block length by up to 257 (the largest table entry), so it can reach 2^24 + 256 and needs 25 bits.

## 17. Observed implementation gaps

Behaviour of the reference implementation that is not part of the format and must not
be inherited.

1. **The trailing checksum is never read or verified.** Corrupt data is returned as if it were good.
2. **Termination is driven by the unpacked length, not by the stream.** Decoding stops as soon as the output reaches the unpacked length — in the middle of a block if need be — and the remaining run-coded bytes, the final end flag and the checksum are never examined. With an unpacked length of 0 and non-empty packed bytes, only the signature and the block-size field are decoded.
3. **Empty packed data is never examined.** It yields empty output whatever the unpacked length.
4. **Short output is not an error.** If an end flag of 1 arrives early, the shorter output is returned as a success.
5. **An out-of-range origin index silently ends decoding.** If the origin index is not less than the block length (including an empty block), decoding stops and whatever was produced so far is returned as a success.
6. **No block-length bound.** The sorted block may grow without limit, while memory for the full block capacity (up to 2^24 bytes) is reserved for every block before any of its symbols has been decoded.
7. **Zero-run accumulation is unbounded.** The weight and run length are doubled and summed with no limit, so a long sequence of digits either exceeds the number width used (with a result that depends on how the program was built) or asks for an enormous number of bytes.
8. **Truncated input is not detected.** Beyond the end of the data, 0 bits are supplied for ever. A stream cut short is decoded into wrong data, which is returned as a success.
9. **No check of the first bit**, and so no guarantee that the code value stays below the range on invalid input.
10. **Output memory is reserved from the untrusted unpacked length** before any data is decoded.
11. **Excess output is silently dropped** rather than reported (a consequence of item 2).
12. **Method 15 is decoded only for the SIT5 container.** For the classic container the packed bytes are passed through as if they were the unpacked contents.
13. **The companion writer for this method does not produce valid streams**: it omits the run-length coding stage (so any input containing four identical consecutive bytes decodes to different data), emits no checksum, never randomises, always uses block-size field 8, and writes nothing at all for empty input. Its output must not be used as test data; the independent tool rejects it.

## 18. Decisions, remaining uncertainties and evidence summary

DECISION points of this document (each is marked where it occurs):

| # | Section | Situation | Required result | Independent tool | Reference implementation |
|--:|---|---|---|---|---|
| 1 | 1.3 | Method 15 fork in a classic archive | Decoded as in SIT5; fork checksum field not compared | Does exactly this | Passes the packed bytes through |
| 2 | 1.3 | Unpacked length 0 with packed bytes present | Empty fork; nothing decoded | Produces nothing | Decodes only the header |
| 3 | 3 | A bit is requested beyond the end of the packed bytes | Failure: truncated | Fails | Supplies 0 bits without limit |
| 4 | 6 | Block longer than the block capacity | Failure: corrupt block | Fails | No limit |
| 5 | 8 | Origin index not less than the block length, including an empty block | Failure: corrupt block | Fails when anything follows an empty block | Stops and returns what it has as a success |
| 6 | 12.2 | Output would exceed the unpacked length | Failure: output overflow | Stops at the unpacked length, then fails its checksum test | Stops at the unpacked length and returns a success |
| 7 | 12.2 | Stream ends with fewer bytes than the unpacked length | Failure: length mismatch | Accepts the short output | Returns the short output as a success |
| 8 | 4.1, 12.2 | Fewer than 26 bits of packed bytes, first bit 1 | Failure: truncated (not "not an Arsenic stream") | Not tested; a black box does not show which kind | Supplies 0 bits and goes on |
| 9 | 1.3, 12.2 | Decoder called directly with an unpacked length above its own output cap | "Output limit exceeded", before anything is read; a decoder without a cap decodes normally | Not applicable | No cap; reserves output memory from the unpacked length |
| 10 | 12.2 | Memory for a block or for the output cannot be obtained | "Out of memory", reported as fork error "output limit exceeded" | Not tested | Does not recover |

Decision 7 was marked INFERRED in an earlier draft: in all 1,929 genuine
forks the lengths agree exactly, and a fork that is shorter than its container
says cannot be right, so the strict rule loses nothing.

Points still marked INFERRED:

1. **What a genuine compressor emits in the last bits** is unknown: which of the permitted final code values (0 to R − 1, section 11.5) it chooses. Any of them decodes identically, so it does not affect decoding. The extent of the freedom is not INFERRED: it is exactly the final code value, which involves at most the last 25 requested bits (section 11.5).

Points that an earlier draft of this document marked INFERRED and that are now
settled:

| Point | Now established by |
|---|---|
| All sixteen block-size field values are valid. | Reference implementation; BLACK-BOX (section 6). |
| A stream with zero blocks is the valid encoding of empty data. | Layout; BLACK-BOX (section 6, vector P1). Still no genuine example. |
| Empty blocks are invalid and must be rejected. | Follows from the origin-index rule of the reference implementation (section 8); none in the genuine set; BLACK-BOX. |
| Run-expansion state restarts in every block; a pending repeat count at the end of a block is dropped. | Reference implementation; BLACK-BOX with streams whose checksum distinguishes the readings (sections 10 and 11.2). |
| Input past the end. | Replaced: EMPIRICAL and BLACK-BOX show that valid streams never need it (section 3). The earlier allowance of 26 substitute bits is withdrawn. |
| Rejecting a stream whose code value is not below the range. | Proven from section 4.3 to reduce to a single check of the first input bit (section 4.1). |
| Output longer than the unpacked length is corruption. | Lengths agree in all genuine forks; BLACK-BOX the independent tool stops at the unpacked length and then fails the checksum (vector R11). |
| The block capacity is a hard upper limit, inclusive. | EMPIRICAL and BLACK-BOX (section 6, vectors P8 and R3). Not enforced by the reference implementation. |
| The order in which the compressor applied its stages. | Mirror image of the confirmed decoding order (section 2). |

Outside the stream format: whether method 15 occurs in genuine classic
archives is unknown (no example was found). It does not matter to a reader:
by decision 1 the fork is decoded in the same way in either container.
