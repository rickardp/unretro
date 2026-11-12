# StuffIt compression methods 1, 2, 3 and 13

Format specification for clean-room implementation. This document describes
four of the compression methods that can be applied to a fork stored in a
StuffIt archive of either container generation:

| Method number | Common name | Section |
|---:|:---|:---|
| 1 | Run-length | [3](#3-method-1-run-length) |
| 2 | LZW | [4](#4-method-2-lzw) |
| 3 | Static Huffman | [5](#5-method-3-static-huffman) |
| 13 | LZ77 with Huffman codes | [6](#6-method-13-lz77-with-huffman-codes) |

Method 0 (stored: the packed bytes are the unpacked bytes) and method 14
(Deflate) are specified in `containers.md` (sections 2.2 and 2.3); method 15
is specified in `arsenic.md`. How a method number, the two lengths and the
fork checksum are found in an archive is the business of the container
specification (`containers.md`). `README.md` in this directory is the index
of the document set and holds the shared glossary.

## 1. How to read this document

### 1.1 Evidence labels

Every statement about the format carries, explicitly or through the
"Evidence" paragraph of its section, one of these labels:

- **CONFIRMED (real data)**: checked against forks taken from real archives.
  The survey covered 47 distinct archive files (byte-identical copies counted
  once). 17 of them contain method 13 forks: 1,080 unencrypted forks in all
  (1,021 in the classic container, 59 in the SIT5 container; 1,029 distinct
  after removing identical copies), expanding to 41.5 MB. Two archives
  contain the 7 method 2 forks. Two further archives (two editions of one
  password-protected self-extracting archive) hold 527 encrypted method 13
  forks, which could not be examined. No real archive using method 1 was
  available. One real method 3 fork was found after the survey (section
  5.6); it is not included in the counts of this paragraph.
  "Confirmed on real data" means precisely this: a throwaway decoder that
  follows the rules of this document literally, and treats every situation
  the document calls an error as an error, was run over every one of the
  1,080 + 7 forks; each produced exactly U bytes whose CRC-16 equals the
  fork checksum stored in the archive (section 2.5). No fork was excluded
  and none failed.
- **CONFIRMED (crafted)**: checked by building byte streams by hand from the
  rules in this document and giving them to two independent decoders used
  purely as black boxes. Both had to produce the expected output. The two
  black boxes are:
  - the **reference implementation**: the third-party StuffIt library,
    version 0.1.8, which the implementation written from this document
    replaces;
  - **`unar`**: the `unar` command-line tool, version 1.10.7.
- **INFERRED**: deduced, or supported by only one of the two black boxes,
  with no real data to settle it. Every inferred point is marked with the
  word INFERRED where it occurs and is listed in
  [section 9](#9-inferred-points-and-decisions).
- **DECISION**: a place where the evidence shows what the black boxes do
  with *malformed* input, they disagree or are needlessly lenient, and this
  document prescribes one behaviour for the new implementation. Decisions
  never affect how a well-formed stream decodes: none of the 1,087 real
  forks reaches any of them. Each is marked DECISION where it occurs and is
  listed in section 9.

The black boxes agree with each other and with the real data on every rule
for well-formed streams, except for three faults of the reference
implementation: three wrong constants in predefined table set 4 of method
13 (section 6.5), a wrong rule for method 1 (section 3.4), and the
rejection of some valid method 2 resets (section 8.2). On the 1,029
distinct real method 13 forks it reproduces the fork checksum for all 998
that use modes 0, 1, 2, 3 and 5 and for none of the 31 that use mode 4;
those 31 (53 forks when identical copies are counted) are the only real
forks it gets wrong, and it returns each of them shorter than U without
reporting an error.

Words such as "must" and "must not" state requirements on the new
implementation.

### 1.2 Terms

The terms below are those of `containers.md` section 1 and of the glossary
in `README.md`; they are repeated so that this document can be read alone.

- **Packed bytes**, **packed stream**: the bytes stored in the archive for
  one fork. "Packed stream" is used when they are looked at as a sequence
  of bits. Their number is the **packed length**, called **P**; it comes
  from the container.
- **Unpacked bytes**: the original contents of the fork; what a decoder
  produces. Also called the output.
- **Unpacked length**: the number of unpacked bytes the fork must expand
  to, called **U**. It is the value declared in the container (an unsigned
  32-bit number) and is untrusted. U is an input to every decoder in this
  document; none of the four formats can be decoded correctly without it.
- **Fork**: one of the two byte sequences of a Macintosh file (data fork,
  resource fork). Each fork is packed independently, with its own
  method number, its own P and its own U. No state is carried from one fork
  to another.
- **Fork checksum**: the 16-bit field the container stores for each fork,
  a CRC-16 of the unpacked bytes (section 2.5). "Checksum" without
  qualification means a CRC-16 of this kind throughout this document.
- **Decoder**: the component that turns the packed bytes of one fork into
  its unpacked bytes for one method number. The **container layer** is the
  component that reads the archive structure and calls a decoder.
- **Bit position**: bits of the packed stream are numbered from 0. Which
  bit of which byte a bit position refers to depends on the method and is
  defined in section 2.1. The packed stream has exactly 8 × P bits; "the end of the
  stream" is bit position 8 × P.

### 1.3 Numbers

Hexadecimal byte values are written as two digits in code style, for example
`90`. Four-digit values in code style such as `B120` are 16-bit checksums in
hexadecimal. Bit strings are written in code style in the order the bits are
read, for example `00100` means "first a 0, then a 0, then a 1, ...".

## 2. Things common to all four methods

### 2.1 Two bit orders

Two different ways of taking bits out of bytes are used. They are never mixed
inside one stream.

**Low-bit-first order** (methods 2 and 13). Bit position *p* of the stream is
bit (*p* mod 8) of byte (*p* div 8), where bit 0 of a byte is its least
significant bit (value 1) and bit 7 its most significant (value 128). When a
field of *n* bits is read as a number, the first bit read is the least
significant bit of the number and the last bit read is the most significant.
Equivalently: treat the whole stream as one huge little-endian integer and
take fields from its low end.

Example: the two bytes `41 84` hold, as a 9-bit low-bit-first field starting
at bit position 0, the eight bits of `41` as the low eight bits and bit 0 of
`84` (which is 0) as the ninth bit. The value is 65.

**High-bit-first order** (method 3). Bit position *p* of the stream is bit
(7 - (*p* mod 8)) of byte (*p* div 8): the most significant bit of each byte
comes first. When a field of *n* bits is read as a number, the first bit read
is the most significant bit of the number.

Method 1 is byte-oriented and has no bit order.

These two names are used throughout the document set. The bit input of
method 15 (`arsenic.md` section 3) is in high-bit-first order; a Deflate
stream (method 14) is in low-bit-first order. The CRC-16 and CRC-32 are
"reflected" checksums, a property of the checksum algorithm that is
described where each is defined and is unrelated to how a packed stream is
read.

### 2.2 What a decoder is given and what it returns

The division of work between the container layer and a decoder is defined
once, in `containers.md` section 2.2, for every method. This section
restates the part that concerns the four decoders of this document; should
the two ever seem to differ, `containers.md` section 2.2 governs.

**Given.** A decoder receives exactly three things: the method number, the
P packed bytes of one fork, and the unpacked length U. It is not given the
fork checksum.

**Returned.** A decoder returns either exactly U unpacked bytes, or one of
the errors of section 2.4. It never returns fewer or more than U bytes as a
success.

**Not the decoder's business.** The container layer has dealt with all of
the following before a decoder is called, and deals with the fork checksum
after it returns:

- **Method number.** In the classic container the method is carried in a
  byte whose low four bits are the method number and whose other bits are
  flags. In the SIT5 container the method number is a whole byte.
  Extracting the number, and reporting "unsupported method" for a number
  that has no decoder, is done by the container layer. The same number
  selects the same decoder in both containers.
- **Encryption.** The container can mark a fork as encrypted. Such a fork is
  refused by the container layer and never reaches the decoders of
  this document; the packed stream described here is always plain.
- **Empty and impossible forks.** A fork with U = 0 is empty and is never
  decoded; a fork with P = 0 and U greater than 0 is corrupt and is never
  decoded (section 2.3).
- **The host's output limit.** A fork whose U exceeds the limit the host
  application allows is refused before a decoder is called (section 2.6).
- **The fork checksum.** The container layer computes the CRC-16 of the U
  bytes a decoder returned and compares it with the fork checksum
  (section 2.5).

**One method, one transformation.** Each method number denotes exactly one
transformation from packed stream to output. The methods are never
layered: the output of method 2, 3 or 13 is the fork itself and is not
passed through the run-length decoder of method 1 or through anything
else. (CONFIRMED (real data) for methods 2 and 13: the direct output has
the stored fork checksum. CONFIRMED (crafted) for method 3 in both black
boxes.)

### 2.3 Order of checks, and zero-length forks

The following checks are made in this order before any method-specific
decoding. Checks 1 to 3 belong to the container layer and are listed, with
the same outcomes, in `containers.md` section 2.2 (which interleaves them
with its own checks for encryption and for packed bytes that lie outside
the archive); they are repeated here because the evidence for them was
gathered with these four methods. Check 4 is likewise the container
layer's. Check 5 is the first thing a decoder does.

1. **U = 0.** The output is empty and the result is success. Nothing is read
   from the packed stream, whatever P is and whatever the bytes are, and
   the method number is not examined: a fork with U = 0 and an unknown
   method number, or with a nonsensical first byte, is an empty fork.
   (CONFIRMED (crafted) in `unar` for the method numbers 4 to 12, which it
   otherwise refuses, and for methods 2 and 13 with garbage streams. In
   real archives every empty fork carries method 0 and has P = 0.)
2. **P = 0** (with U greater than 0). The fork is corrupt. None of the four
   methods can produce a byte from an empty stream: method 1 needs at least
   one byte, methods 2 and 3 at least 9 bits, method 13 at least its header
   byte. (DECISION. The reference implementation returns an empty output as
   success for every method; `unar` does the same for methods 1 and 2 and
   reports an error for methods 3 and 13.)
3. **Unsupported method number.** The container layer reports "unsupported
   method". The packed bytes must never be delivered as if they were the
   contents.
4. **Host limit** of section 2.6: the container layer reports "output limit
   exceeded".
5. **Format limit** of section 2.6: the decoder reports "corrupt input".

A decoder is therefore never called with U = 0 or with P = 0. A decoder
of this document that is nevertheless called directly with U = 0 (in a
test, say) returns an empty output without reading anything.

**The format limit comes before every read.** Check 5 is evaluated from P
and U alone, before the first bit or byte of the stream is looked at, and
its result takes precedence over any truncation the stream would also show.
"Truncated input" is therefore only ever reported for a stream that has
passed its method's format limit. (DECISION, of no consequence for the
container layer, which reports both kinds as "corrupt".) The consequences
for very short streams, which follow from the table of section 2.6:

| Method | Stream | Result for any U greater than 0 |
|---:|:---|:---|
| 1 | P = 0 (direct call only) | corrupt input: the limit is 0 |
| 2 | P = 0 (direct call only) or P = 1 | corrupt input: no whole 9-bit code fits, so the limit is 0 |
| 13 | P = 0 (direct call only) | corrupt input: the limit is not met by any U (P - 1 is negative; the test must not be computed in a way that wraps around) |
| 13 | P = 1, the header byte alone | corrupt input: the limit is 0. The header byte is not examined, so this holds even when its mode is valid; the single byte `11` with U = 10 is corrupt input, not truncated input |
| 3 | P = 0 (direct call only) | truncated input: method 3 has no limit before its tree, and the first bit of the tree is missing |
| 3 | P = 1 | truncated input: the smallest tree needs 9 bits, and the leaf and branch limits of section 5.2 cannot be reached within 8 |

For method 3 the limit that applies after the tree (two or more leaves)
is tested when the tree is complete and before the first data bit is read,
so it likewise wins over a truncation in the coded data; a truncation
inside the tree comes first in stream order and is truncated input.

### 2.4 Results and error kinds

A decoder ends with exactly one of these results:

| Result | Meaning | Fork error the container layer reports for it |
|:---|:---|:---|
| success | exactly U bytes were produced | none yet: the fork checksum is verified next (section 2.5) |
| truncated input | the decoder needed a bit or byte at or beyond the end of the packed stream | "corrupt" |
| corrupt input | the packed stream contains something this document calls invalid or corrupt, or U exceeds what P bytes can produce (section 2.6) | "corrupt" |

Three further outcomes of delivering a fork are produced by the container
layer, not by a decoder: "unsupported method" (section 2.3 step 3),
"output limit exceeded" (section 2.3 step 4) and "checksum mismatch"
(section 2.5). The single exception is a decoder that applies an output
cap of its own when called directly; it reports "output limit exceeded"
itself (section 2.6).

Rules that apply to every method:

- **Fewer than U bytes is never success.** Every situation in which a
  decoder cannot continue before U bytes exist is "truncated input" or
  "corrupt input". (DECISION. In several of these situations both black
  boxes stop quietly and hand back the shorter output; `unar` then reports a
  checksum failure, the reference implementation reports nothing.)
- **No contents on error.** When the result is anything other than success,
  or when the container layer then finds a checksum mismatch, the bytes
  produced so far are not delivered as the fork's contents. (DECISION.) An
  implementation may expose them through a separate, explicitly marked
  salvage facility; nothing in this document requires one.
- **Which error wins.** Decoding proceeds strictly in stream order and stops
  at the first error. Where one read could be judged either way, the
  method's section says which kind applies.
- **Never more than U bytes.** Decoding stops the moment U bytes exist, even
  in the middle of a run, a string or a match. Bytes and bits of the packed
  stream that have not been read by then are ignored, whatever they contain
  and however many they are; they are not an error. A decoder must not read
  beyond the item that completes the output. None of the four methods has
  an end mark that a decoder reads, so "the stream would have produced more
  than U bytes" cannot be observed and is not an error for these methods.
  (Methods 14 and 15 differ: their streams end themselves, and output
  beyond U is an error there; see `containers.md` section 2.2.)
- The container layer reports both "truncated input" and "corrupt input"
  from a decoder as its single fork error "corrupt". Keeping the two apart
  inside the decoder is still required, for diagnostics. (The fork error
  "truncated" of `containers.md` means something else: the packed bytes
  themselves lie partly outside the archive, so no decoder is called.)

### 2.5 The fork checksum

The container stores a 16-bit fork checksum for each fork. It is computed
over the **unpacked** bytes of that fork (all U of them, nothing else),
whatever the compression method. The algorithm is the CRC-16 defined in
`containers.md` section 2.1 (commonly called CRC-16/ARC). Its parameters
are repeated here because section 6.5 also uses it as a transcription
check on tables:

| Property | Value |
|:---|:---|
| Width | 16 bits |
| Polynomial | x^16 + x^15 + x^2 + 1, i.e. `8005` in normal notation, `A001` in bit-reflected notation |
| Bit reflection | input and output both reflected: each byte is fed least significant bit first and the register shifts right |
| Initial value | `0000` |
| Final XOR | none |
| Check value | the nine ASCII bytes `123456789` give `BB3D` |

Step by step: start with a 16-bit register of 0. For each byte, XOR the
byte into the low eight bits of the register; then eight times: if the
lowest bit of the register is 1, shift the register right one bit and XOR it
with `A001`, otherwise only shift it right one bit.

**Who verifies it.** The container layer, not the decoder: after a decoder
has returned U bytes, the container layer computes their CRC-16 and
compares it with the fork checksum. A difference is the fork error
"checksum mismatch", and the bytes are not delivered. The fork checksum is
the only means of detecting most corruption in these four methods, because
none of them has an internal integrity check; verification is mandatory
for methods 1, 2, 3 and 13 in both containers. The header offset of the
field is given in `containers.md`. The "checksum" printed with each worked
example and test vector of this document is the CRC-16 of the expected
output, that is, the value the fork checksum field of an archive carrying
that example holds.

Evidence: CONFIRMED (real data) for method 13 in both containers (1,021
forks in the classic container and 59 in the SIT5 container, all matching),
for method 2 in the classic container (7 forks, all matching) and for
method 3 in the classic container (1 fork, section 5.6). CONFIRMED
(crafted) for methods 1 and 3 in the classic container: `unar` accepts
archives carrying the checksums printed in the worked examples of this
document and reports "wrong checksum" when the field is altered.

### 2.6 Upper bounds on U

U comes from an untrusted header. Two limits apply before decoding starts.

**Host limit.** The host application supplies a maximum output size for one
fork (in this repository, the library-wide maximum decompressed size). If U
exceeds it, the container layer reports the fork error "output limit
exceeded" and no decoder is called (`containers.md` section 2.2). This is
the only defence against a stream that legitimately expands without bound,
which exists in method 3 (see the table).

**Format limit.** For each method there is a largest U that P bytes can
possibly produce. If U is larger, the fork cannot be decoded successfully,
and the result is "corrupt input" without any decoding. (DECISION: the black
boxes make no such check; it rejects nothing that could have decoded.) The
limits, with all arithmetic in integers of at least 64 bits:

| Method | U is acceptable only if | Why |
|---:|:---|:---|
| 1 | U ≤ 127 × P | the densest construct is the two-byte pair marker + count, which adds at most 254 bytes |
| 2 | with *n* = (8 × P) div 9: U ≤ *n* × (*n* + 1) / 2 and U ≤ 16,128 × *n* | a code is at least 9 bits; the *k*-th code after a start or reset yields at most *k* bytes and never more than 16,128 (section 4.2) |
| 3 | no format limit can be applied before the tree has been read. After the tree: if it has two or more leaves, U ≤ 8 × P - 19; if it is a single leaf, any U is valid | a tree with two leaves occupies 19 bits and every output byte then costs at least one bit; a single-leaf tree costs no bits per byte |
| 13 | 17 × U ≤ 262,656 × (P - 1) | after the header byte, the densest item is symbol 319 with a one-bit codeword, its 15-bit field and a one-bit distance codeword: 17 bits for at most 32,832 bytes. Every other item yields fewer bytes per bit (64 bytes for 2 bits, 1,088 for 12, 1 for 1) |

These bounds are loose for real data (the largest real expansion seen is 49
output bytes per packed byte, in method 13), but they turn a header
claiming 4 GiB for a handful of bytes into an immediate error for methods 1,
2 and 13. For method 3 with a single-leaf tree, a 2-byte stream may
legitimately stand for any number of copies of one byte; only the host limit
bounds it.

Whatever U is, memory for the output must not be committed on the strength
of U alone (section 7.1).

**A decoder's own output cap.** The host limit is enforced by the container
layer, and nothing in this document requires a decoder to know it. A
decoder may nevertheless apply a cap of its own as a second line of
defence for callers that reach it directly, and may refuse a fork whose
output memory it cannot obtain. This is permitted for every method (it
matters most for the single-leaf tree of method 3, where no format limit
exists) under three conditions. (DECISION.)

- The cap is never lower than the limit the container layer enforces, so
  that a fork admitted by the container layer is never refused by the cap.
  Through the container layer the cap is unreachable.
- The refusal is reported as the outcome "output limit exceeded", the same
  one the container layer gives, and not as "corrupt input" or "truncated
  input": the stream is well formed, and the two decoder error kinds are
  reserved for faults of the stream. This is the one case in which a
  decoder ends with something other than the three results of section 2.4.
  A failure to obtain memory is reported the same way; it is never a
  success with fewer bytes and never "corrupt input".
- The cap is tested where the host limit would be, before the format limit
  and before anything is read. For method 3 it is thus tested before the
  tree is read and applies whatever shape the tree turns out to have.

The rule of section 7.1 stands beside this: with or without a cap, output
memory is reserved ahead of the bytes produced only up to a fixed bound.

## 3. Method 1: run-length

### 3.1 Format

The packed stream is a sequence of bytes. One byte value, `90`, is
reserved as the **marker**. The meaning of the stream depends on one byte of
context, the **last byte**, which is `00` at the start of the stream.

| Input | Meaning | Effect on the last byte |
|:---|:---|:---|
| any byte *b* other than `90` | output *b* | becomes *b* |
| `90` followed by `00` | output one byte `90` | becomes `90` |
| `90` followed by *n*, with *n* from 2 to 255 | output the last byte a further *n* - 1 times, so that together with the copy already output before the marker it appears *n* times in a row | unchanged |
| `90` followed by `01` | invalid: corrupt input | - |

The count byte *n* is unsigned. It counts total occurrences including the
one already emitted, so the longest run expressed by one marker pair is 255
bytes (1 already out plus 254 more). Longer runs are written as several
marker pairs in a row, each one continuing the same last byte; for example
`41 90 FF 90 FF` is 509 bytes of `41`.

A marker pair at the very start of the stream, before any byte has been
output, repeats the initial last byte `00`: `90 03` at the start produces two
`00` bytes.

After `90 00` the last byte is `90`, so `90 00 90 03` produces three bytes of
`90`.

The pair `90 01` would mean "zero further copies". It is invalid: `unar`
rejects a stream containing it, at the point where the pair is read, even
when the pair is followed by valid data. A decoder must report corrupt
input. (DECISION, following `unar`.)

### 3.2 Decoding steps

1. The last byte is `00`. The output is empty.
2. If the output holds U bytes, stop: success.
3. Read one byte *b*. If no byte remains: truncated input.
4. If *b* is not `90`: append *b* to the output, set the last byte to *b*,
   and continue at step 2.
5. Otherwise read one more byte *n*. If no byte remains: truncated input.
6. If *n* is 0: append `90`, set the last byte to `90`, continue at step 2.
7. If *n* is 1: corrupt input.
8. Otherwise append the last byte *n* - 1 times, but stop appending as soon
   as the output holds U bytes. Continue at step 2.

Decoding ends when U bytes have been produced. A run may be cut short by U;
that is not an error (CONFIRMED (crafted) in `unar`: `41 90 05` with U = 3
gives `41 41 41` and is accepted). Input bytes left over after U bytes have
been produced are ignored.

### 3.3 Worked example

Packed stream (9 bytes), U = 10:

`41 90 05 42 90 00 43 90 03`

| Step | Input consumed | Action | Output so far (hex) | Last byte |
|---:|:---|:---|:---|:---|
| 1 | `41` | ordinary byte | `41` | `41` |
| 2 | `90 05` | 5 occurrences in total, so 4 more copies of `41` | `41 41 41 41 41` | `41` |
| 3 | `42` | ordinary byte | `41 41 41 41 41 42` | `42` |
| 4 | `90 00` | literal marker value | `41 41 41 41 41 42 90` | `90` |
| 5 | `43` | ordinary byte | `41 41 41 41 41 42 90 43` | `43` |
| 6 | `90 03` | 3 occurrences in total, so 2 more copies of `43` | `41 41 41 41 41 42 90 43 43 43` | `43` |

Output: the ten bytes `41 41 41 41 41 42 90 43 43 43` ("AAAAAB", then `90`,
then "CCC"). Their CRC-16 (section 2.5) is `B120`.

Smaller cases. Each is a complete fork when U is the number of bytes shown:

| Input | U | Output |
|:---|---:|:---|
| `41 90 02 42` | 3 | `41 41 42` |
| `90 03 41` | 3 | `00 00 41` |
| `90 00 90 03 41` | 4 | `90 90 90 41` |
| `41 90 04 90 03 42` | 7 | `41 41 41 41 41 41 42` |
| `41 90 FF 90 FF` | 509 | 509 bytes of `41` |
| `41 90 05` | 3 | `41 41 41` (run cut by U) |
| `41 90 01 42` | 2 | corrupt input |
| `41 90` | 5 | truncated input |
| `41` | 5 | truncated input |

### 3.4 Evidence

CONFIRMED (crafted) in `unar` only: every successful row of the tables
above, and the rejection of `90 01`, was obtained by giving crafted archives
to `unar`, which accepted the checksum of each successful row. (The two
"truncated input" rows follow section 2.4.) The rule is also the well-known
"RLE 90" scheme used by several formats of the same era. The reference
implementation applies a **different and incompatible** rule (section 8.1),
so the two black boxes disagree and no real method 1 fork was available to
arbitrate. That `unar`'s rule is the one the original software used is
therefore INFERRED. It is the rule a new decoder must implement.

## 4. Method 2: LZW

### 4.1 Overview

The packed stream is a sequence of **codes** in low-bit-first order
(section 2.1), packed back to back. Each code is between 9 and 14 bits wide
and names a string of bytes; the output is the concatenation of those
strings. Which string a code names depends on the codes before it, in the
manner of LZW.

| Parameter | Value |
|:---|:---|
| Bit order | low-bit-first |
| Smallest code width | 9 bits |
| Largest code width | 14 bits |
| Code values 0 to 255 | the one-byte string consisting of that byte value |
| Code value 256 | the **reset code**; never a string |
| Code values 257 to 16,383 | **entries**: strings defined by earlier codes |
| Stream header | none; the first code starts at bit position 0 |
| End-of-data code | none; the stream ends when U bytes have been produced |

### 4.2 Segments, code numbers, widths and entries

The stream is divided into **segments**. The first segment starts at bit
position 0. A segment ends with a reset code (and the padding that follows
it, section 4.3) or with the end of the data; the next segment starts
immediately after that padding. Nothing is carried from one segment to the
next.

Within a segment the codes are numbered 1, 2, 3, ... in the order they
appear. This number is called the code's **position** and is written *k*. A
reset code has a position like any other code. Call the string named by the
code at position *k* S(*k*).

**Width.** The width of a code depends only on its position:

| Position *k* in the segment | Width in bits |
|:---|---:|
| 1 to 256 | 9 |
| 257 to 768 | 10 |
| 769 to 1,792 | 11 |
| 1,793 to 3,840 | 12 |
| 3,841 to 7,936 | 13 |
| 7,937 and above | 14 |

So a segment has 256 codes of 9 bits, then 512 of 10, 1,024 of 11, 2,048 of
12, 4,096 of 13, and everything after that is 14 bits wide for as long as
the segment lasts.

**Entries.** Each code from position 2 onward defines one new entry, until
the entry numbers run out:

- for every *k* from 2 to 16,128, entry number 255 + *k* is the string
  S(*k* - 1) followed by the first byte of S(*k*);
- positions above 16,128 define nothing: entry 16,383 is the last one, and
  the segment simply continues with the entries it has.

Position 1 defines nothing. Consequently a string is always "some earlier
string plus one byte", entry 255 + *k* is at most *k* bytes long, and no
string is longer than 16,128 bytes.

**Which values a code may have.** The code at position *k* may be:

| Value *c* | Meaning |
|:---|:---|
| 0 to 255 | the one-byte string *c* |
| 256 | reset (section 4.3) |
| 257 to 254 + *k* (but never above 16,383) | an entry defined by an earlier position of this segment |
| 255 + *k*, when *k* is at least 2 and at most 16,128 | the entry this very code defines. Its string is S(*k* - 1) followed by the first byte of S(*k* - 1). (The definition above needs the first byte of S(*k*), which is the first byte of S(*k* - 1) because S(*k*) begins with S(*k* - 1).) |
| anything else | corrupt input |

In particular the first code of a segment must be 0 to 255 (or a reset): a
first code of 257 or more is corrupt. A code that refers to an entry number
higher than the one it defines itself is corrupt.

The width rule and the entry rule fit together: the first code that can name
entry 512 or above is at position 257, which is where the width becomes 10,
and so on for 1,024, 2,048, 4,096 and 8,192.

### 4.3 Reset code and padding

A reset code (value 256) may appear at any position of a segment, including
position 1, and at any width. It ends the segment. Encoders send one when
they judge that compression has degraded; nothing in the stream announces
it in advance.

Encoders write codes in groups of eight (eight codes of width *w* occupy
exactly *w* bytes), and after a reset code they abandon the rest of the
current group. Therefore, if the reset code is at position *k* and has width
*w*:

- if *k* is a multiple of 8, the next segment starts at the bit after the
  reset code;
- otherwise (8 - (*k* mod 8)) × *w* bits follow the reset code that carry no
  information. Their values are arbitrary and must be ignored. The next
  segment starts after them.

Because every width below 14 is used for a number of positions that is a
multiple of 8 (256, 512, 1,024, 2,048, 4,096), a group of eight codes never
straddles a change of width, and "position in the segment mod 8" is the
same thing as "position in the current group of eight". The same fact
stated in bit positions: let *s* be the bit position of the first code of
the current width in this segment; a reset code that ends at bit position
*e* is followed by the next segment at the smallest position
*s* + *j* × 8 × *w* (*j* a whole number) that is not less than *e*. The
two formulations are arithmetically identical.

The padding amounts that can occur are 0 to 7 codes, that is 0 to 63 bits at
width 9 and up to 98 bits at width 14.

Two reset codes in a row are valid. The second one is at position 1 of its
segment, so it is followed by 7 padding codes of 9 bits.

### 4.4 Decoding steps

1. Start a segment: *k* = 0, no entries.
2. If the output holds U bytes, stop: success.
3. Add 1 to *k*. Let *w* be the width for position *k* (section 4.2). If
   fewer than *w* bits remain in the stream: truncated input. Otherwise read
   *w* bits as a number *c*.
4. If *c* is 256: if *k* mod 8 is not 0, skip (8 - (*k* mod 8)) × *w* bits;
   if fewer than that many bits remain: truncated input. Then continue at
   step 1.
5. Determine S(*k*) from the table in section 4.2; a value not allowed there
   is corrupt input.
6. Append S(*k*) to the output; if that would exceed U, append only as many
   of its leading bytes as are needed to reach U.
7. If *k* is between 2 and 16,128, entry 255 + *k* now exists as defined in
   section 4.2. Continue at step 2.

The check in step 3 is deliberately "fewer than *w* bits remain", not
"fewer bits remain than the code happens to need": a code is never formed
from a partial field.

### 4.5 Worked examples

**Example 1: entries and the self-defining code.**

Packed stream (5 bytes), U = 7: `41 84 04 1C 08`

Bits in the order read (low bit of each byte first):

| Byte | Value | Bit positions | Bits in the order read |
|---:|:---|:---|:---|
| 0 | `41` | 0-7 | `10000010` |
| 1 | `84` | 8-15 | `00100001` |
| 2 | `04` | 16-23 | `00100000` |
| 3 | `1C` | 24-31 | `00111000` |
| 4 | `08` | 32-39 | `00010000` |

| Position *k* | Bit positions | Bits in the order read | Value | String S(*k*) | Entry defined | Output so far |
|---:|:---|:---|---:|:---|:---|:---|
| 1 | 0-8 | `100000100` | 65 | "A" | none | A |
| 2 | 9-17 | `010000100` | 66 | "B" | 257 = "AB" | AB |
| 3 | 18-26 | `100000001` | 257 | "AB" | 258 = "BA" | ABAB |
| 4 | 27-35 | `110000001` | 259 | 259 = 255 + 4 is the entry this code defines: S(3) "AB" plus its first byte = "ABA" | 259 = "ABA" | ABABABA |

After position 4 the output holds 7 bytes = U, so decoding stops. Bits 36 to
39 are unused. Output: "ABABABA" (checksum `D5D6`).

(To turn "bits in the order read" into the value, remember the first bit is
the least significant: `100000100` is 1 + 64 = 65.)

With U = 6 the same stream gives "ABABAB": the string of position 4 is cut
short by U.

**Example 2: reset and padding.**

Packed stream (13 bytes), U = 5:
`41 84 00 1C 42 A4 88 91 23 48 92 28 01`

| Segment | Position *k* | Bit positions | Value | Meaning |
|---:|---:|:---|---:|:---|
| 1 | 1 | 0-8 | 65 | "A" |
| 1 | 2 | 9-17 | 66 | "B"; entry 257 = "AB" |
| 1 | 3 | 18-26 | 256 | reset at position 3, width 9. 3 mod 8 is not 0, so (8 - 3) × 9 = 45 bits of padding follow, positions 27 to 71 |
| 2 | 1 | 72-80 | 72 | "H" (position 1 defines no entry) |
| 2 | 2 | 81-89 | 73 | "I"; entry 257 = "HI" |
| 2 | 3 | 90-98 | 74 | "J"; entry 258 = "IJ" |

Output: "ABHIJ" (checksum `061E`). The 45 padding bits in this hand-made
stream happen to hold the codes for C, D, E, F and G; a decoder that failed
to skip them would output "ABCDE" instead.

**Example 3: a reset at position 1.**

Packed stream (12 bytes), U = 2: `00 AB 56 AD 5A B5 6A D5 AA 41 84 00`

The first code (bits 0-8) is 256: a reset at position 1. 1 mod 8 is not 0,
so 7 × 9 = 63 bits of padding follow (positions 9 to 71; here they hold
seven copies of the value 341, which mean nothing). The second segment
starts at bit 72: codes 65 and 66. Output: "AB".

**Example 4: a reset at width 10 (described, not printed).** Build a stream
from these codes in this order: 300 codes whose values are 0, 1, 2, ...,
250, 0, 1, ..., 48 (the value at index *i*, counting from 0, is *i* mod
251); then the reset code 256; then three padding codes of value 0; then
the three codes 88, 89, 90 ("XYZ"). By section 4.2 the first 256 codes are
9 bits wide, the next 44 and the reset code (position 301) are 10 bits
wide, 301 mod 8 = 5 so the padding is 3 × 10 = 30 bits, and the last three
codes are 9 bits wide again. The stream is 2,811 bits, zero-padded to 352
bytes; the CRC-16 of those 352 packed bytes is `08EB`. With U = 303 it
decodes to the 300 bytes 0, 1, ..., 250, 0, ..., 48 followed by "XYZ"
(checksum `3F9F`).

### 4.6 End of data and errors

- The decoder stops when U bytes have been produced. The final string may
  be cut short by U. Bits after the last code read are ignored; real
  streams have fewer than 8 of them (the stream is padded to a whole byte).
- If the stream has fewer bits left than the next code or the reset padding
  needs, the result is truncated input. (DECISION, see section 2.4: both
  black boxes stop quietly here and return the shorter output.)
- A code value not allowed by section 4.2 is corrupt input. (CONFIRMED
  (crafted): both black boxes report an error for a first code of 257 and
  for a code above the entry it defines.)

### 4.7 Evidence

CONFIRMED (real data): bit order, every width from 9 to 14 and the
positions where it changes, continuing after entry 16,383 with nothing new
defined, and the reset code with its padding at width 14 (seven resets in
three of the seven real forks, at positions 16,130 and 16,131 of their
segments, so both 84 and 70 padding bits occur). Three more real method 2
forks, found after the survey in the archive described in section 5.6
(U = 332, 359 and 419,284; the largest contains eight resets), also decode
by these rules to their fork checksums.

CONFIRMED (crafted), both black boxes: all four worked examples; the
self-defining code; a reset at positions 1 and 3 and two resets in a row;
and a reset followed by further data at every width from 9 to 14 with every
possible amount of padding (0 to 7 codes) at each width. `unar` decoded all
of these as this section says. The reference implementation agreed on all
but a few of them, which it wrongly rejects because of a fault of its own
(section 8.2). The statement that the two formulations of the padding rule
in section 4.3 are identical is proven by the arithmetic given there, not
inferred.

## 5. Method 3: static Huffman

### 5.1 Format

The packed stream is read in high-bit-first order (section 2.1). It has
two parts with no alignment or padding between them:

1. A serialized binary code tree.
2. The coded data: one tree walk per output byte, U of them.

**Tree serialization.** The tree is written in pre-order. A node is
serialized as:

- a **leaf**: one bit 1, followed by 8 bits that give, most significant bit
  first, the byte value (0 to 255) the leaf stands for;
- a **branch**: one bit 0, followed by the complete serialization of the
  subtree reached by a 0 bit, followed immediately by the complete
  serialization of the subtree reached by a 1 bit.

The tree is the single node whose serialization starts at bit position 0.
There is no count of nodes or symbols in the stream; the tree is complete
when the root node's serialization is complete. Every branch has exactly two
children. A tree with *n* leaves has *n* - 1 branches and occupies exactly
10 × *n* - 1 bits.

**Coded data.** Starting at the bit immediately after the tree, for each
output byte: start at the root; while the current node is a branch, read one
bit and move to the 0-child or the 1-child accordingly; when a leaf is
reached, output its byte value.

**Single-leaf tree.** If the root itself is a leaf (the stream starts with a
1 bit), every output byte is that leaf's value and decoding a byte consumes
no bits at all. The whole fork is U copies of one byte and only the first 9
bits of the stream (2 bytes) are used.

There is no end-of-data symbol. The decoder stops after U bytes. Bits after
the last needed one are padding and are ignored.

Because 10 × *n* - 1 is odd, a tree never ends on a byte boundary: at least
one bit of the stream always follows it.

### 5.2 Limits on the tree

A tree produced by a Huffman encoder for bytes has at most 256 leaves (one
per byte value), hence at most 511 nodes, a serialized size of at most
2,559 bits (320 bytes), and at most 255 branches on the path from the root
to any leaf. The serialization itself, however, can describe any binary
tree. The rules for a decoder are:

- **More than 256 leaves: corrupt input.** Leaves and branches are counted
  while the tree is read, and the stream is corrupt as soon as a 257th leaf
  or a 256th branch is read, whichever comes first. (A complete tree with
  *n* leaves has *n* - 1 branches, so the two counts describe the same
  limit; counting branches as well catches a tree that opens branch after
  branch without ever reaching a leaf, and bounds the depth at 255.)
  DECISION: `unar` accepts
  trees of any size (trees with 512, 1,024, 4,096 and 5,001 leaves were
  decoded by position) and crashes on a tree deep enough to exhaust its
  stack; the reference implementation silently mis-decodes any tree with
  more than 256 leaves. No encoder has a reason to produce such a tree,
  because it would have to repeat a byte value.
- **Duplicate leaf values are accepted.** Two or more leaves may carry the
  same byte value, as long as the tree has at most 256 leaves. Decoding is
  by position in the tree, so nothing special happens: each leaf outputs its
  own value. (CONFIRMED (crafted): both black boxes decode the tree with
  leaves `41`, `41`, `42` this way.) A decoder must not reject duplicates
  and need not detect them.
- A tree need not contain every byte value, and need not be shaped like an
  optimal code.

### 5.3 Decoding steps

1. Read the tree as described in 5.1, counting leaves and branches. If a
   bit is needed and none remains: truncated input. If a 257th leaf or a
   256th branch is read: corrupt input.
2. If the tree has two or more leaves and U exceeds 8 × P - 19: corrupt
   input (section 2.6).
3. Repeat U times: walk from the root to a leaf, reading one bit per branch
   passed, and output the leaf's value. If a bit is needed and none
   remains: truncated input.

### 5.4 Worked example

Packed stream (5 bytes), U = 6: `50 54 2A 1A F8`

Bits in the order read (high bit of each byte first):

| Byte | Value | Bit positions | Bits in the order read |
|---:|:---|:---|:---|
| 0 | `50` | 0-7 | `01010000` |
| 1 | `54` | 8-15 | `01010100` |
| 2 | `2A` | 16-23 | `00101010` |
| 3 | `1A` | 24-31 | `00011010` |
| 4 | `F8` | 32-39 | `11111000` |

Reading the tree:

| Bit positions | Bits | Meaning |
|:---|:---|:---|
| 0 | `0` | root is a branch; its 0-child follows |
| 1 | `1` | 0-child of root is a leaf |
| 2-9 | `01000001` | leaf value `41` ("A") |
| 10 | `0` | 1-child of root is a branch; its 0-child follows |
| 11 | `1` | leaf |
| 12-19 | `01000010` | leaf value `42` ("B") |
| 20 | `1` | leaf (the 1-child of the inner branch) |
| 21-28 | `01000011` | leaf value `43` ("C") |

The tree is now complete (3 leaves, 29 bits). The resulting code is
A = `0`, B = `10`, C = `11`.

Reading the data, starting at bit position 29:

| Bit positions | Bits | Output byte |
|:---|:---|:---|
| 29 | `0` | A |
| 30-31 | `10` | B |
| 32-33 | `11` | C |
| 34-35 | `11` | C |
| 36-37 | `10` | B |
| 38 | `0` | A |

Six bytes have been produced, equal to U: stop. Bit 39 is padding. Output:
"ABCCBA" (checksum `DE63`).

Further cases:

| Stream | U | Result |
|:---|---:|:---|
| `AD 00` | 5 | bits `1`, then `01011010` = `5A`: a single-leaf tree. Output: five bytes of `5A` ("ZZZZZ"); no further bits are read |
| `50 54 2A 1A F8` | 7 | the padding bit 39 is `0` and is now read as a seventh codeword: output "ABCCBAA" (checksum `195E`) |
| `50 54 2A 1A F8` | 8 | truncated input (the stream has 11 data bits, positions 29 to 39, which give exactly seven bytes; the eighth byte needs a twelfth bit) |
| `50 54 2A 1A F8` | 9 | truncated input, for the same reason: decoding fails at the eighth byte |
| `50 54 2A` | 8 | truncated input (the stream ends inside the tree, at bit 24 of 29) |
| `AD` | 5 | truncated input (the leaf value needs bit 8) |

### 5.5 End of input

If the stream ends while the tree is being read, or while a data walk is in
progress, the result is truncated input. (`unar` reports an error in the
truncated rows above; the reference implementation does not, see
section 8.3.)

### 5.6 Evidence

CONFIRMED (crafted): both black boxes decode the worked example, the
single-leaf case, a tree with 256 leaves and a maximally lopsided tree with
256 leaves (255 branches deep) identically and as described.

CONFIRMED (real data), on one fork. No method 3 fork was in the survey of
section 1.1; one was found afterwards, when the documents of this set were
reconciled: the data fork of a text file in a classic archive carried in a
MacBinary wrapper (U = 1,673, P = 1,032; the archive's other three forks
use method 2). Decoded literally by the rules of this section it gives a
tree of 56 leaves occupying 559 bits (10 × 56 - 1), then 1,673 tree walks
that end exactly at the last bit of the packed stream (bit 8,256), and
output whose CRC-16 equals the fork checksum. `unar` and the reference
implementation produce the same bytes. A single fork exercises one tree
shape only; the limits of section 5.2 rest on the crafted streams.

## 6. Method 13: LZ77 with Huffman codes

### 6.1 Overview

Method 13 is a sliding-window (LZ77) compressor whose output items are coded
with three prefix codes:

- the **primary code**, over an alphabet of 321 symbols, used to read an item
  when the previous item was a literal byte, and for the very first item;
- the **secondary code**, over the same 321-symbol alphabet, used to read an
  item when the previous item was a match;
- the **distance code**, over an alphabet of 10 to 17 symbols, used to read
  the distance of each match.

The 321-symbol alphabet is:

| Symbol | Meaning |
|:---|:---|
| 0 to 255 | literal byte with that value |
| 256 to 317 | match of length (symbol - 253), that is 3 to 64 bytes |
| 318 | match whose length follows as a 10-bit number plus 65 (65 to 1,088) |
| 319 | match whose length follows as a 15-bit number plus 65 (65 to 32,832) |
| 320 | end of data |

The three codes are either taken from one of five **predefined table sets**
built into every decoder, or transmitted at the start of the stream
(**dynamic tables**). A one-byte header chooses. A stream has exactly one
header and one set of codes; there are no blocks, and the codes never change
in mid-stream.

Everything in the stream is read in low-bit-first order (section 2.1).

### 6.2 Stream layout

| Part | Size | Present |
|:---|:---|:---|
| Header byte | 8 bits | always |
| Primary code description | variable | dynamic mode only |
| Secondary code description | variable | dynamic mode only, and only if the header's "shared" flag is clear |
| Distance code description | variable | dynamic mode only |
| Items | variable | always; together they produce exactly U bytes |
| End-of-data symbol | one codeword | written by encoders; never read by a decoder (section 6.9) |
| Padding | any number of bits | whatever follows; never read by a decoder (section 6.9) |

No part is byte-aligned except the header, which is the first byte. Each
part starts at the bit after the previous one.

### 6.3 Header byte

The header is the first byte of the stream (bit positions 0 to 7, read as an
8-bit low-bit-first field, which is simply the byte's value).

| Bits of the byte | Mask | Name | Meaning |
|:---|:---|:---|:---|
| 7-4 | `F0` | mode | 0: dynamic tables. 1 to 5: predefined table set 1 to 5. 6 to 15: corrupt input. |
| 3 | `08` | shared flag | Dynamic mode: if set, no secondary code description is present and the secondary code is identical to the primary code. Predefined modes: ignored. |
| 2-0 | `07` | distance alphabet size minus 10 | Dynamic mode: the distance code has (this value + 10) symbols, that is 10 to 17. Predefined modes: ignored. |

Evidence for each row:

- Modes 6 to 15 are rejected by both black boxes (CONFIRMED (crafted): header
  bytes `60`, `61`, `70`, `80`, `90`, `A0`, `B0`, `C0`, `D0`, `E0`, `F0` and
  `FF` each give an error in both). No real fork uses them.
- In predefined modes the low four bits have no effect on decoding
  (CONFIRMED (crafted), both black boxes, for all five sets with the low
  four bits set to 0, 7, 8 and 15). A decoder must take the distance
  alphabet size from the table set (section 6.5) and must accept any value
  in the low four bits.
- Real encoders nevertheless fill bits 2-0 in predefined modes with the size
  of the set's distance alphabet minus 10 and leave bit 3 clear, so the only
  predefined header bytes that occur in real archives are `11`, `23`, `34`,
  `41` and `51` (CONFIRMED (real data), as an observation about encoders).
- All sixteen dynamic header values `00` to `0F` decode in both black boxes
  (CONFIRMED (crafted), each with matches at the largest distance its
  alphabet allows).

Header values in the 1,080 real forks, with the range of U for each:

| Header | Meaning | Forks | Smallest U | Largest U |
|:---|:---|---:|---:|---:|
| `06` | dynamic, separate secondary code, 16 distance symbols | 63 | 28,876 | 7,472,966 |
| `0A` | dynamic, shared, 12 distance symbols | 173 | 1,027 | 2,033 |
| `0B` | dynamic, shared, 13 distance symbols | 186 | 2,048 | 4,060 |
| `0C` | dynamic, shared, 14 distance symbols | 90 | 4,103 | 8,186 |
| `0D` | dynamic, shared, 15 distance symbols | 147 | 8,241 | 16,222 |
| `0E` | dynamic, shared, 16 distance symbols | 56 | 16,384 | 155,228 |
| `11` | predefined set 1 | 191 | 286 | 952 |
| `23` | predefined set 2 | 76 | 1,032 | 2,994 |
| `34` | predefined set 3 | 3 | 3,482 | 3,616 |
| `41` | predefined set 4 | 53 | 596 | 1,001 |
| `51` | predefined set 5 | 42 | 60 | 999 |

Dynamic alphabets of 10, 11 and 17 symbols, and a separate secondary code
with fewer than 16 distance symbols, do not occur in the real forks; they
are valid all the same.

### 6.4 Building a prefix code from code lengths

Each of the three codes is defined by a list of **code lengths**, one per
symbol of its alphabet, in symbol order. A length of 1 to 31 means the symbol
has a codeword of that many bits. A symbol can also be **absent** (it has no
codeword and cannot occur); the predefined sets have no absent symbols, the
dynamic tables usually have many.

**Weight.** The weight of a code is the sum, over its present symbols, of
2 to the power minus length. A code is **complete** if its weight is exactly
1, **over-subscribed** if it is more than 1, **incomplete** if it is less
than 1 (this includes a code with no present symbol at all, whose weight
is 0). The weight must be computed exactly: in units of 2^-31 each symbol
contributes a whole number between 1 and 2^30, and with up to 321 symbols
the sum can exceed 32 bits, so an accumulator of at least 64 bits is needed.

**Over-subscribed codes are corrupt input.** The weight test is made before
any codeword is assigned; the assignment rule below is defined only for
codes of weight at most 1. (CONFIRMED (crafted): `unar` reports an error for
code lengths 1, 1, 1 and for 1, 2, 2, 2. The reference implementation does
not check, see section 8.4.)

**Assignment of codewords** (the usual canonical rule). Let the **next
codeword** be a number that starts at 0. For each length *n* = 1, 2, 3, ...
up to the longest length in use, in increasing order:

1. Each symbol whose length is *n*, taken in increasing symbol order,
   receives as its codeword the next codeword written as an *n*-digit binary
   number; the next codeword is then increased by 1.
2. Then, whether or not any symbol had length *n*, the next codeword is
   doubled before moving on to length *n* + 1.

So codewords of the same length are consecutive binary numbers in symbol
order, a shorter codeword padded with zero bits to the length of a longer
one is always numerically smaller than the longer one, and the very first
codeword is all zeros. The doubling in step 2 happens at every length,
including lengths that no symbol has: in a code whose shortest length is 4,
the first codeword is `0000`, and if the next length in use is 5 the first
codeword of that length is the successor of the last 4-bit codeword with a
zero bit appended. For a code of weight at most 1 the next codeword always
fits in *n* digits at the moment it is assigned.

**How codewords appear in the stream.** A codeword is read one bit at a
time, and the first bit read is the **most significant** digit of the
*n*-digit binary number assigned above. This is the opposite of how
multi-bit numeric fields are read in the same stream (first bit read is the
least significant, section 2.1). Both rules apply to the same low-bit-first
sequence of bit positions; only the interpretation of the bits differs.

Small example. A code over four symbols W, X, Y, Z (in that symbol order)
with lengths 1, 2, 3, 3:

| Length | Symbol | Next codeword before | Codeword (first bit read on the left) |
|---:|:---|---:|:---|
| 1 | W | 0 | `0` |
| (double) | | 1 becomes 2 | |
| 2 | X | 2 | `10` |
| (double) | | 3 becomes 6 | |
| 3 | Y | 6 | `110` |
| 3 | Z | 7 | `111` |

A second example with a gap. A code over three symbols P, Q, R with lengths
1, 3, 3 (weight 3/4, incomplete): P gets `0`; the next codeword 1 is doubled
to 2 after length 1 and, there being no symbol of length 2, doubled again to
4; Q gets `100` and R gets `101`. The bit strings `11` and beyond are not
codewords.

**Reading one symbol.** The symbol is defined by the bits of the stream
taken one at a time. Call a bit string **viable** if it is shorter than, and
a prefix of, at least one codeword of the code (the empty string is viable
exactly when the code has at least one present symbol).

1. Start with the empty string.
2. If the string is not viable: corrupt input.
3. If no bit remains in the stream: truncated input.
4. Read one bit and append it. If the string now equals a codeword, the
   symbol with that codeword is the result. Otherwise continue at step 2.

This fixes which error is reported: "corrupt" as soon as the bits read rule
out every codeword, even if the stream also ends there; "truncated" only if
the stream ends while the bits read so far could still become a codeword.
For a complete code step 2 never fails. A symbol is found after at most 31
bits. An implementation may decode by any technique that gives the same
symbol, consumes the same number of bits and reports the same error.

**Incomplete codes are valid.** A decoder must accept an incomplete code
when it is built and fail only if a bit string that is not viable is
actually read (step 2 above). (CONFIRMED (crafted): both black boxes accept
all of the following and decode every stream that stays within the
codewords; `unar` reports an error, and the reference implementation stops,
exactly when an unassigned bit string is read.) The cases:

| Code | Behaviour |
|:---|:---|
| exactly one present symbol, of length 1 | its codeword is `0`. Reading a `0` bit gives the symbol; reading a `1` bit is corrupt input. It is *not* a code that consumes no bits. |
| exactly one present symbol, of length *n* greater than 1 | its codeword is *n* zero bits. Any `1` bit among them is corrupt input. |
| several present symbols, weight below 1 | codewords by the canonical rule; bit strings beyond the last codeword are corrupt input when read. |
| no present symbol | valid as long as no symbol is ever read with this code: a distance code in a fork without matches, or a secondary code in a fork whose matches are never followed by anything read. The first attempt to read a symbol with it is corrupt input (step 2, before any bit is read). |

Every one of the three codes in every dynamic real fork (715 forks) is
complete, and the predefined codes are complete, so incomplete codes are
only to be expected from other encoders.

### 6.5 Predefined table sets

Mode *k* (1 to 5) selects table set *k*: a primary code of 321 lengths, a
secondary code of 321 lengths, and a distance code. All fifteen codes are
complete. The data follows; a decoder must contain it exactly.

**Distance codes** (lengths listed in symbol order, starting with symbol 0):

| Set | Number of distance symbols | Distance code lengths |
|---:|---:|:---|
| 1 | 11 | 5 6 3 3 3 3 3 3 3 4 6 |
| 2 | 13 | 5 6 4 4 3 3 3 3 3 4 4 4 6 |
| 3 | 14 | 6 7 4 4 3 3 3 3 3 4 4 4 5 7 |
| 4 | 11 | 3 6 5 4 2 3 3 3 4 4 6 |
| 5 | 11 | 6 7 7 6 4 3 2 2 3 3 6 |

**Primary and secondary codes.** Each table below lists 321 code lengths in
symbol order, 16 per row. The number before the colon is the symbol number
of the first entry in that row (so the row labelled 256 starts with the
length of symbol 256, and the last row, labelled 320, holds the single
length of symbol 320). After each table a summary line gives the number of
entries, the sum of all entries, and the smallest and largest entry.
Order-sensitive checks follow the tables.

Set 1, primary code (entries 321, sum 2954, shortest 4, longest 10):

```text
  0:  4  5  7  8  8  9  9  9  9  7  9  9  9  8  9  9
 16:  9  9  9  9  9  9  9 10  9  9 10 10  9 10  9  9
 32:  5  9  9  9  9 10  9  9  9  9  9  9  9  9  7  9
 48:  9  8  9  9  9  9  9  9  9  9  9  9  9  9  9  9
 64:  9  8  9  9  8  8  9  9  9  9  9  9  9  7  8  9
 80:  7  9  9  7  7  9  9  9  9 10  9 10 10 10  9  9
 96:  9  5  9  8  7  5  9  8  8  7  9  9  8  8  5  5
112:  7 10  5  8  5  8  9  9  9  9  9 10  9  9 10  9
128:  9 10 10 10 10 10 10 10  9 10 10 10 10 10 10 10
144:  9 10 10 10 10 10 10 10 10 10 10 10 10 10 10 10
160:  9 10 10 10 10 10 10 10  9  9 10 10 10 10 10 10
176: 10 10 10 10 10 10 10 10 10 10  9 10 10 10 10 10
192:  9 10 10 10 10 10 10 10 10 10 10 10 10 10 10 10
208: 10 10 10 10 10 10 10 10 10 10 10 10 10 10 10 10
224:  9 10 10 10 10 10 10 10 10 10 10 10  9  9 10 10
240:  9 10 10 10 10 10 10 10  9 10 10 10  9 10  9  5
256:  6  5  5  8  9  9  9  9  9  9 10 10 10  9 10 10
272: 10 10 10 10 10 10 10 10 10 10 10 10 10 10 10 10
288: 10 10 10  9 10  9  9  9 10  9 10  9 10  9 10  9
304: 10 10 10  9 10  9 10 10  9  9  9  6  9  9 10  9
320:  5
```

Set 1, secondary code (entries 321, sum 3123, shortest 4, longest 18):

```text
  0:  4  5  6  6  7  7  6  7  7  7  6  8  7  8  8  8
 16:  8  9  6  9  8  9  8  9  9  9  8 10  5  9  7  9
 32:  6  9  8 10  9 10  8  8  9  9  7  9  8  9  8  9
 48:  8  8  6  9  9  8  8  9  9 10  8  9  9 10  8 10
 64:  8  8  8  8  8  9  7 10  6  9  9 11  7  8  8  9
 80:  8 10  7  8  6  9 10  9  9 10  8 11  9 11  9 10
 96:  9  8  9  8  8  8  8 10  9  9 10 10  8  9  8  8
112:  8 11  9  8  8  9  9 10  8 11 10 10  8 10  9 10
128:  8  9  9 11  9 11  9 10 10 11 10 12  9 12 10 11
144: 10 11  9 10 10 11 10 11 10 11 10 11 10 10 10  9
160:  9  9  8  7  6  8 11 11  9 12 10 12  9 11 11 11
176: 10 12 11 11 10 12 10 11 10 10 10 11 10 11 11 11
192:  9 12 10 12 11 12 10 11 10 12 11 12 11 12 11 12
208: 10 12 11 12 11 11 10 12 10 11 10 12 10 12 10 12
224: 10 11 11 11 10 11 11 11 10 12 11 12 10 10 11 11
240:  9 12 11 12 10 11 10 12 10 11 10 12 10 11 10  7
256:  5  4  6  6  7  7  7  8  8  7  7  6  8  6  7  7
272:  9  8  9  9 10 11 11 11 12 11 10 11 12 11 12 11
288: 12 12 12 12 11 12 12 11 12 11 12 11 13 11 12 10
304: 13 10 14 14 13 14 15 14 16 15 15 18 18 18  9 18
320:  8
```

Set 2, primary code (entries 321, sum 3178, shortest 3, longest 16):

```text
  0:  4  7  7  8  7  8  8  8  8  7  8  7  8  7  9  8
 16:  8  8  9  9  9  9 10 10  9 10 10 10 10 10  9  9
 32:  5  9  8  9  9 11 10  9  8  9  9  9  8  9  7  8
 48:  8  8  9  9  9  9  9 10  9  9  9 10  9  9 10  9
 64:  8  8  7  7  7  8  8  9  8  8  9  9  8  8  7  8
 80:  7 10  8  7  7  9  9  9  9 10 10 11 11 11 10  9
 96:  8  6  8  7  7  5  7  7  7  6  9  8  6  7  6  6
112:  7  9  6  6  6  7  8  8  8  8  9 10  9 10  9  9
128:  8  9 10 10  9 10 10  9  9 10 10 10 10 10 10 10
144:  9 10 10 11 10 10 10 10 10 10 10 11 10 11 10 10
160:  9 11 10 10 10 10 10 10  9  9 10 11 10 11 10 11
176: 10 12 10 11 10 12 11 12 10 12 10 11 10 11 11 11
192:  9 10 11 11 11 12 12 10 10 10 11 11 10 11 10 10
208:  9 11 10 11 10 11 11 11 10 11 11 12 11 11 10 10
224: 10 11 10 10 11 11 12 10 10 11 11 12 11 11 10 11
240:  9 12 10 11 11 11 10 11 10 11 10 11  9 10  9  7
256:  3  5  6  6  7  7  8  8  8  9  9  9 11 10 10 10
272: 12 13 11 12 12 11 13 12 12 11 12 12 13 12 14 13
288: 14 13 15 13 14 15 15 14 13 15 15 14 15 14 15 15
304: 14 15 13 13 14 15 15 14 14 16 16 15 15 15 12 15
320: 10
```

Set 2, secondary code (entries 321, sum 3003, shortest 4, longest 18):

```text
  0:  5  6  6  6  6  7  7  7  7  7  7  8  7  8  7  7
 16:  7  8  8  8  8  9  8  9  8  9  9  9  7  9  8  8
 32:  6  9  8  9  8  9  8  9  8  9  8  9  8  9  8  8
 48:  8  8  8  9  8  9  8  9  9 10  8 10  8  9  9  8
 64:  8  8  7  8  8  9  8  9  7  9  8 10  8  9  8  9
 80:  8  9  8  8  8  9  9  9  9 10  9 11  9 10  9 10
 96:  8  8  8  9  8  8  8  9  9  8  9 10  8  9  8  8
112:  8 11  8  7  8  9  9  9  9 10  9 10  9 10  9  8
128:  8  9  9 10  9 10  9 10  8 10  9 10  9 11 10 11
144:  9 11 10 10 10 11  9 11  9 10  9 11  9 11 10 10
160:  9 10  9  9  8 10  9 11  9  9  9 11 10 11  9 11
176:  9 11  9 11 10 11 10 11 10 11  9 10 10 11 10 10
192:  8 10  9 10 10 11  9 11  9 10 10 11  9 10 10  9
208:  9 10  9 10  9 10  9 10  9 11  9 11 10 10  9 10
224:  9 11  9 11  9 11  9 10  9 11  9 11  9 11  9 10
240:  8 11  9 10  9 10  9 10  8 10  8  9  8  9  8  7
256:  4  4  5  6  6  6  7  7  7  7  8  8  8  7  8  8
272:  9  9 10 10 10 10 10 10 11 11 10 10 12 11 11 12
288: 12 11 12 12 11 12 12 12 12 12 12 11 12 11 13 12
304: 13 12 13 14 14 14 15 13 14 13 14 18 18 17  7 16
320:  9
```

Set 3, primary code (entries 321, sum 2914, shortest 4, longest 10):

```text
  0:  6  6  6  6  6  9  8  8  4  9  8  9  8  9  9  9
 16:  8  9  9 10  8 10 10 10  9 10 10 10  9 10 10  9
 32:  9  9  8 10  9 10  9 10  9 10  9 10  9  9  8  9
 48:  8  9  9  9 10 10 10 10  9  9  9 10  9 10  9  9
 64:  7  8  8  9  8  9  9  9  8  9  9 10  9  9  8  9
 80:  8  9  8  8  8  9  9  9  9  9 10 10 10 10 10  9
 96:  8  8  9  8  9  7  8  8  9  8 10 10  8  9  8  8
112:  8 10  8  8  8  8  9  9  9  9 10 10 10 10 10  9
128:  7  9  9 10 10 10 10 10  9 10 10 10 10 10 10  9
144:  9 10 10 10 10 10 10 10 10  9 10 10 10 10 10 10
160:  9 10 10 10 10 10 10 10  9  9  9 10 10 10 10 10
176: 10 10 10 10 10 10 10 10 10 10  9 10 10 10 10  9
192:  8  9 10 10 10 10 10 10 10 10 10 10  9 10 10 10
208:  9 10 10 10 10 10 10 10 10 10 10 10 10 10 10  9
224:  9 10 10 10 10 10 10  9 10 10 10 10 10 10  9  9
240:  9 10 10 10 10 10 10  9  9 10  9  9  8  9  8  9
256:  4  6  6  6  7  8  8  9  9 10 10 10  9 10 10 10
272: 10 10 10 10 10 10 10 10 10 10 10 10 10 10  7 10
288: 10 10  7 10 10  7  7  7  7  7  6  7 10  7  7 10
304:  7  7  7  6  7  6  6  7  7  6  6  9  6  9 10  6
320: 10
```

Set 3, secondary code (entries 321, sum 3150, shortest 3, longest 18):

```text
  0:  5  6  6  6  6  7  7  7  6  8  7  8  7  9  8  8
 16:  7  7  8  9  9  9  9 10  8  9  9 10  8 10  9  8
 32:  6 10  8 10  8 10  9  9  9  9  9 10  9  9  8  9
 48:  8  9  8  9  9 10  9 10  9  9  8 10  9 11 10  8
 64:  8  8  8  9  7  9  9 10  8  9  8 11  9 10  9 10
 80:  8  9  9  9  9  8  9  9 10 10 10 12 10 11 10 10
 96:  8  9  9  9  8  9  8  8 10  9 10 11  8 10  9  9
112:  8 12  8  9  9  9  9  8  9 10  9 12 10 10 10  8
128:  7 11 10  9 10 11  9 11  7 11 10 12 10 12 10 11
144:  9 11  9 12 10 12 10 12 10  9 11 12 10 12 10 11
160:  9 10  9 10  9 11 11 12  9 10  8 12 11 12  9 12
176: 10 12 10 13 10 12 10 12 10 12 10  9 10 12 10  9
192:  8 11 10 12 10 12 10 12 10 11 10 12  8 12 10 11
208: 10 10 10 12  9 11 10 12 10 12 11 12 10  9 10 12
224:  9 10 10 12 10 11 10 11 10 12  8 12  9 12  8 12
240:  8 11 10 11 10 11  9 10  8 10  9  9  8  9  8  7
256:  4  3  5  5  6  5  6  6  7  7  8  8  8  7  7  7
272:  9  8  9  9 11  9 11  9  8  9  9 11 12 11 12 12
288: 13 13 12 13 14 13 14 13 14 13 13 13 12 13 13 12
304: 13 13 14 14 13 13 14 14 14 14 15 18 17 18  8 16
320: 10
```

Set 4, primary code (entries 321, sum 3205, shortest 2, longest 18):

```text
  0:  2  6  6  7  7  8  7  8  7  8  8  9  8  9  9  9
 16:  8  8  9  9  9 10 10  9  8 10  9 10  9 10  9  9
 32:  6  9  8  9  9 10  9  9  9 10  9  9  9  9  8  8
 48:  8  8  8  9  9  9  9  9  9  9  9  9  9 10 10  9
 64:  7  7  8  8  8  8  9  9  7  8  9 10  8  8  7  8
 80:  8 10  8  8  8  9  8  9  9 10  9 11 10 11  9  9
 96:  8  7  9  8  8  6  8  8  8  7 10  9  7  8  7  7
112:  8 10  7  7  7  8  9  9  9  9 10 11  9 11 10  9
128:  7  9 10 10 10 11 11 10 10 11 10 10 10 11 11 10
144:  9 10 10 11 10 11 10 11 10 10 10 11 10 11 10 10
160:  9 10 10 11 10 10 10 10  9 10 10 10 10 11 10 11
176: 10 11 10 11 11 11 10 12 10 11 10 11 10 11 11 10
192:  8 10 10 11 10 11 11 11 10 11 10 11 10 11 11 11
208:  9 10 11 11 10 11 11 11 10 11 11 11 10 10 10 10
224: 10 11 10 10 11 11 10 10  9 11 10 10 11 11 10 10
240: 10 11 10 10 10 10 10 10  9 11 10 10  8 10  8  6
256:  5  6  6  7  7  8  8  8  9 10 11 10 10 11 11 12
272: 12 10 11 12 12 12 12 13 13 13 13 13 12 13 13 15
288: 14 12 14 15 16 12 12 13 15 14 16 15 17 18 15 17
304: 16 15 15 15 15 13 13 10 14 12 13 17 17 18 10 17
320:  4
```

Set 4, secondary code (entries 321, sum 3100, shortest 4, longest 15):

```text
  0:  4  5  6  6  6  6  7  7  6  7  7  9  6  8  8  7
 16:  7  8  8  8  6  9  8  8  7  9  8  9  8  9  8  9
 32:  6  9  8  9  8 10  9  9  8 10  8 10  8  9  8  9
 48:  8  8  7  9  9  9  9  9  8 10  9 10  9 10  9  8
 64:  7  8  9  9  8  9  9  9  7 10  9 10  9  9  8  9
 80:  8  9  8  8  8  9  9 10  9  9  8 11  9 11 10 10
 96:  8  8 10  8  8  9  9  9 10  9 10 11  9  9  9  9
112:  8  9  8  8  8 10 10  9  9  8 10 11 10 11 11  9
128:  8  9 10 11  9 10 11 11  9 12 10 10 10 12 11 11
144:  9 11 11 12  9 11  9 10 10 10 10 12  9 11 10 11
160:  9 11 11 11 10 11 11 12  9 10 10 12 11 11 10 11
176:  9 11 10 11 10 11  9 11 11  9  8 11 10 11 11 10
192:  7 12 11 11 11 11 11 12 10 12 11 13 11 10 12 11
208: 10 11 10 11 10 11 11 11 10 12 11 11 10 11 10 10
224: 10 11 10 12 11 12 10 11  9 11 10 11 10 11 10 12
240:  9 11 11 11  9 11 10 10  9 11 10 10  9 10  9  7
256:  4  5  5  5  6  6  7  6  8  7  8  9  9  7  8  8
272: 10  9 10 10 12 10 11 11 11 11 10 11 12 11 11 11
288: 11 11 13 12 11 12 13 12 12 12 13 11  9 12 13  7
304: 13 11 13 11 10 11 13 15 15 12 14 15 15 15  6 15
320:  5
```

Set 5, primary code (entries 321, sum 2885, shortest 5, longest 10):

```text
  0:  7  9  9  9  9  9  9  9  9  8  9  9  9  7  9  9
 16:  9  9  9  9  9  9  9 10  9 10  9 10  9 10  9  9
 32:  5  9  7  9  9  9  9  9  7  7  7  9  7  7  8  7
 48:  8  8  7  7  9  9  9  9  7  7  7  9  9  9  9  9
 64:  9  7  9  7  7  7  7  9  9  7  9  9  7  7  7  7
 80:  7  9  7  8  7  9  9  9  9  9  9  9  9  9  9  9
 96:  9  7  8  7  7  7  8  8  6  7  9  7  7  8  7  5
112:  6  9  5  7  5  6  7  7  9  8  9  9  9  9  9  9
128:  9  9 10  9 10 10 10  9  9 10 10 10 10 10 10 10
144:  9 10 10 10 10 10 10 10 10 10 10 10  9 10 10 10
160:  9 10 10 10  9  9 10  9  9  9  9 10 10 10 10 10
176: 10 10 10 10 10 10  9 10 10 10 10 10 10 10 10 10
192:  9 10 10 10  9 10 10 10  9  9  9 10 10 10 10 10
208:  9 10  9 10 10  9 10 10  9 10 10 10 10 10 10 10
224:  9 10 10 10 10 10 10 10 10 10 10 10 10 10 10 10
240:  9 10 10 10 10 10 10 10  9 10  9 10  9 10 10  9
256:  5  6  8  8  7  7  7  9  9  9  9  9  9  9  9  9
272:  9  9  9  9  9  9  9  9  9  9  9  9  9  9  9  9
288:  9 10 10 10 10 10 10 10 10 10 10 10 10 10 10 10
304: 10 10 10 10 10 10 10 10  9 10 10  5 10  8  9  8
320:  9
```

Set 5, secondary code (entries 321, sum 3808, shortest 4, longest 17):

```text
  0:  8 10 11 11 11 12 11 11 12  6 11 12 10  5 12 12
 16: 12 12 12 12 12 13 13 14 13 13 12 13 12 13 12 15
 32:  4 10  7  9 11 11 10  9  6  7  8  9  6  7  6  7
 48:  8  7  7  8  8  8  8  8  8  9  8  7 10  9 10 10
 64: 11  7  8  6  7  8  8  9  8  7 10 10  8  7  8  8
 80:  7 10  7  6  7  9  9  8 11 11 11 10 11 11 11  8
 96: 11  6  7  6  6  6  6  8  7  6 10  9  6  7  6  6
112:  7 10  6  5  6  7  7  7 10  8 11  9 13  7 14 16
128: 12 14 14 15 15 16 16 14 15 15 15 15 15 15 15 15
144: 14 15 13 14 14 16 15 17 14 17 15 17 12 14 13 16
160: 12 17 13 17 14 13 13 14 14 12 13 15 15 14 15 17
176: 14 17 15 14 15 16 12 16 15 14 15 16 15 16 17 17
192: 15 15 17 17 13 14 15 15 13 12 16 16 17 14 15 16
208: 15 15 13 13 15 13 16 17 15 17 17 17 16 17 14 17
224: 14 16 15 17 15 15 14 17 15 17 15 16 15 15 16 16
240: 14 17 17 15 15 16 15 17 15 14 16 16 16 16 16 12
256:  4  4  5  5  6  6  6  7  7  7  8  8  8  8  9  9
272:  9  9  9 10 10 10 11 10 11 11 11 11 11 12 12 12
288: 13 13 12 13 12 14 14 12 13 13 13 13 14 12 13 13
304: 14 14 14 13 14 14 15 15 13 15 13 17 17 17  9 17
320:  7
```

**Checks against transcription errors.** An implementation should verify
all of the following in a unit test.

1. Every one of the fifteen codes has weight exactly 1 (section 6.4).
2. Checksum of each table. Write each code length as one byte, in symbol
   order (321 bytes for a primary or secondary code; 11, 13 or 14 bytes for
   a distance code), and compute the CRC-16 of section 2.5 over those bytes.
   Unlike the sums, this detects two entries being exchanged.

   | Set | Primary | Secondary | Distance |
   |---:|:---|:---|:---|
   | 1 | `11A2` | `9CA6` | `83A0` |
   | 2 | `4F80` | `2D2A` | `0DB1` |
   | 3 | `6E00` | `D3F9` | `C48D` |
   | 4 | `43F3` | `05E8` | `4C75` |
   | 5 | `1716` | `80D1` | `3144` |

3. Selected codewords produced by the canonical rule (first bit read on the
   left):

   | Code | Symbol 0 | Symbol 32 | Symbol 101 | Symbol 255 | Symbol 256 | Symbol 318 | Symbol 319 | Symbol 320 |
   |:---|:---|:---|:---|:---|:---|:---|:---|:---|
   | Set 1 primary | `0000` | `00011` | `00101` | `01010` | `011100` | `1111111111` | `110101111` | `01101` |
   | Set 1 secondary | `0000` | `010011` | `10111000` | `1000100` | `00110` | `111001011` | `111111111111111111` | `11001001` |
   | Set 2 primary | `0010` | `00110` | `00111` | `1001110` | `000` | `111111110110` | `111111111111110` | `1111011111` |
   | Set 2 secondary | `00100` | `010000` | `10100100` | `0110110` | `0000` | `0111100` | `1111111111111110` | `111010101` |
   | Set 3 primary | `001000` | `101100101` | `0110001` | `110101010` | `0001` | `1111111110` | `010111` | `1111111111` |
   | Set 3 secondary | `00110` | `011001` | `110101010` | `1000100` | `0010` | `11000001` | `1111111111111110` | `1111100100` |
   | Set 4 primary | `00` | `011000` | `011001` | `011010` | `01010` | `1111011111` | `11111111111111110` | `0100` |
   | Set 4 secondary | `0000` | `011001` | `110101001` | `1000111` | `0001` | `011101` | `111111111111111` | `01000` |
   | Set 5 primary | `0100000` | `00000` | `0111110` | `110100010` | `00100` | `110111110` | `10011111` | `110111111` |
   | Set 5 secondary | `11010100` | `0000` | `011101` | `111111011001` | `0001` | `111101001` | `11111111111111111` | `1101001` |

   All distance codewords, symbol 0 first:

   | Set | Distance codewords |
   |---:|:---|
   | 1 | `11110` `111110` `000` `001` `010` `011` `100` `101` `110` `1110` `111111` |
   | 2 | `11110` `111110` `1010` `1011` `000` `001` `010` `011` `100` `1100` `1101` `1110` `111111` |
   | 3 | `111110` `1111110` `1010` `1011` `000` `001` `010` `011` `100` `1100` `1101` `1110` `11110` `1111111` |
   | 4 | `010` `111110` `11110` `1100` `00` `011` `100` `101` `1101` `1110` `111111` |
   | 5 | `111100` `1111110` `1111111` `111101` `1110` `100` `00` `01` `101` `110` `111110` |

4. The test vectors of section 6.12, one for each set.

**How these tables were verified.** Each table was checked in four
independent ways.

- Entry by entry against the constants carried by the reference
  implementation. Sets 1, 2, 3 and 5 are identical. Set 4 differs in three
  entries, listed below.
- Weight: all fifteen codes printed here have weight exactly 1.
- Real data: every real fork using a predefined mode decodes with these
  tables to its fork checksum: 191 forks in mode 1, 76 in mode 2, 3 in
  mode 3, 53 in mode 4 (31 distinct) and 42 in mode 5.
- Crafted streams through `unar`: for each of the five sets, one stream
  that uses every literal and every match symbol (0 to 319) of the primary
  code, every one of them again in the secondary code, and every distance
  symbol with its smallest and largest distance, decodes in `unar` to the
  expected output; and for each set a stream with symbol 320 placed early,
  once in the primary and once in the secondary code, stops in `unar`
  exactly there. Because codewords are assigned canonically, a wrong length
  anywhere in a table would shift other codewords and break this test.

**Set 4 differs from the reference implementation in three entries:**

| Table | Symbol | Value in the reference implementation | Correct value (used above) |
|:---|---:|---:|---:|
| Set 4 primary | 165 | 11 | 10 |
| Set 4 primary | 167 | 11 | 10 |
| Set 4 secondary | 214 | 10 | 11 |

With its own values the reference implementation's set 4 primary code has
weight 1 - 1/1024 and its secondary code 1 + 1/2048 (neither is complete);
it fails on every real mode 4 fork and on the crafted all-symbol stream for
set 4, while it passes the all-symbol streams for the other four sets. The
values printed above are authoritative.

### 6.6 Dynamic tables

In mode 0 the code lengths are transmitted. The descriptions appear in this
order, immediately after the header byte, each starting at the bit where the
previous one ended:

1. primary code: 321 lengths;
2. secondary code: 321 lengths. Omitted if the shared flag is set, in which
   case the secondary code is the same as the primary code;
3. distance code: (header bits 2-0, plus 10) lengths.

Each description is a sequence of **commands**. Each command is one symbol of
a fixed prefix code, the **meta code**, sometimes followed by a few extra
bits. All three descriptions use the same meta code.

#### 6.6.1 The meta code

The meta code has 37 symbols, numbered 0 to 36. It is **not** canonical: its
codewords are fixed constants and must be taken from this table, not derived
from the lengths. The bit strings are given in the order the bits are read
from the stream (leftmost bit read first). It is a complete prefix code: no
codeword is a prefix of another and every sufficiently long bit sequence
begins with exactly one of them. The "Command" column is explained in 6.6.2.

| Meta symbol | Code length | Code bits (in the order read) | Command |
|---:|---:|:---|:---|
| 0 | 11 | `00011011101` | set current length to 1 |
| 1 | 8 | `00011010` | set current length to 2 |
| 2 | 8 | `00000010` | set current length to 3 |
| 3 | 8 | `00000011` | set current length to 4 |
| 4 | 8 | `00000000` | set current length to 5 |
| 5 | 7 | `0001111` | set current length to 6 |
| 6 | 6 | `110101` | set current length to 7 |
| 7 | 5 | `00101` | set current length to 8 |
| 8 | 5 | `00110` | set current length to 9 |
| 9 | 5 | `00111` | set current length to 10 |
| 10 | 5 | `11011` | set current length to 11 |
| 11 | 6 | `110100` | set current length to 12 |
| 12 | 5 | `00001` | set current length to 13 |
| 13 | 6 | `000001` | set current length to 14 |
| 14 | 7 | `0001110` | set current length to 15 |
| 15 | 7 | `0001100` | set current length to 16 |
| 16 | 9 | `000110110` | set current length to 17 |
| 17 | 12 | `000110111101` | set current length to 18 |
| 18 | 10 | `0000000110` | set current length to 19 |
| 19 | 11 | `00000001011` | set current length to 20 |
| 20 | 11 | `00000001110` | set current length to 21 |
| 21 | 12 | `000000011111` | set current length to 22 |
| 22 | 12 | `000000011110` | set current length to 23 |
| 23 | 11 | `00000001001` | set current length to 24 |
| 24 | 11 | `00000001000` | set current length to 25 |
| 25 | 11 | `00000001010` | set current length to 26 |
| 26 | 12 | `000110111100` | set current length to 27 |
| 27 | 12 | `000110111111` | set current length to 28 |
| 28 | 12 | `000110111110` | set current length to 29 |
| 29 | 12 | `000110111001` | set current length to 30 |
| 30 | 12 | `000110111000` | set current length to 31 |
| 31 | 5 | `00100` | mark absent |
| 32 | 2 | `10` | one longer |
| 33 | 2 | `01` | one shorter |
| 34 | 3 | `111` | repeat, 1 extra bit |
| 35 | 4 | `1100` | repeat, 3 extra bits |
| 36 | 5 | `00010` | repeat, 6 extra bits |

Check values for this table: the sum of 2 to the power minus length over the
37 rows is exactly 1; and the CRC-16 (section 2.5) of the 37 code lengths
written as one byte each, in symbol order, is `E925`.

A meta symbol is read like any other symbol (section 6.4, "Reading one
symbol"); since the meta code is complete the only possible error is
truncated input.

#### 6.6.2 One description

A description assigns a value to each of the *N* symbols of its code, in
increasing symbol order (*N* is 321, or the distance alphabet size). It does
so through a quantity called the **current length**. The current length is
an integer; it is 0 at the start of *each* description (nothing is carried
over from the previous description), and commands can make it negative.

Every command does two things in this order: it may change the current
length, and it then assigns the current length to the next one or more
symbols that have not been assigned yet.

| Meta symbol *m* | Change to the current length | Extra bits after the meta symbol | Number of symbols assigned |
|:---|:---|:---|:---|
| 0 to 30 | becomes *m* + 1 | none | 1 |
| 31 | becomes -1 | none | 1 |
| 32 | increases by 1 | none | 1 |
| 33 | decreases by 1 | none | 1 |
| 34 | none | 1 bit *f* | *f* + 1 (that is 1 or 2) |
| 35 | none | 3 bits, read as a number *r* (0 to 7) | *r* + 3 (that is 3 to 10) |
| 36 | none | 6 bits, read as a number *r* (0 to 63) | *r* + 11 (that is 11 to 74) |

Extra bits are numeric fields: first bit read is the least significant.
Every command assigns at least one symbol. The description is complete when
all *N* symbols have been assigned; the next part of the stream begins at
the very next bit. There is no terminator and no count.

**Meaning of the assigned values.**

- 1 to 31: the symbol's code length.
- 0 or negative: the symbol is absent. Any value below 1 means absent; the
  current length is ordinary integer arithmetic, with no clamping at 0 or
  -1. For example the commands 33, 33, 33, 32, 32, 32, 32 assign -1, -2,
  -3, -2, -1, 0, 1 to seven consecutive symbols: six absent symbols and one
  of length 1. (CONFIRMED (crafted), both black boxes, with exactly this
  sequence.) Since a description has at most 321 commands, the current
  length always lies between -321 and 351.
- 32 or more: corrupt input. Such a value can only arise through command 32
  (31 is the largest value that can be set directly). The test is on the
  values actually assigned; because command 32 always assigns the value it
  has just produced, this is the same as testing the current length after
  each command 32. (DECISION. Both black boxes accept an assigned value of
  exactly 32 as a 32-bit codeword and silently treat 33 and above as
  absent. No real stream has a length above 21, and supporting 32 would
  require the canonical rule to work with 33-bit numbers.)

In the real forks absent symbols always carry the value -1 (command 31
starts every run of absent symbols, including a run at the very beginning
of a description); the value 0 for absent symbols is CONFIRMED (crafted) in
both black boxes, and worked example 2 uses it.

**A repeat that goes past the last symbol is corrupt input.** If command 34,
35 or 36 would assign more symbols than remain unassigned, the stream is
corrupt. (DECISION. The two black boxes disagree: the reference
implementation assigns as many as remain, consumes the command's extra bits
in full and carries on with the next part of the stream; `unar` does the
same when the excess is one symbol or when it happens in the distance
description, but aborts abnormally when a primary description is overrun
by 9 or by 73 symbols. None of the 715 real dynamic forks contains such an
overrun.)

**When each code is tested.** Each code is tested and built from its
length list by the rules of section 6.4 (weight test first) as soon as its
own description is complete, before the first bit of the next description
or of the first item is read. The order is therefore: read the primary
description, weight-test the primary code; if the shared flag is clear,
read the secondary description, weight-test the secondary code; read the
distance description, weight-test the distance code; then decode items.
With the shared flag set the secondary code is the primary code and is not
tested a second time. This is the rule of section 2.4 (stream order, first
error wins) applied to the descriptions: an over-subscribed primary code
is corrupt input even if the stream then ends inside a later description,
and a stream that ends inside the primary description is truncated input
whatever follows. An error inside a description (truncation, a repeat past
the last symbol, a value of 32 or more) is likewise reported where it
occurs, before that description's weight test, which is never reached.
(DECISION as to which of two errors is reported; both are errors in `unar`,
and the reference implementation tests no weight at all.) Section 6.12 has
a vector that tells the two orders apart.

Code lengths as long as 21 bits occur in real dynamic streams. Lengths up to
31 bits are valid and work in both black boxes (CONFIRMED (crafted): a
complete code with lengths 1, 2, 3, ..., 30, 31, 31, whose 30- and 31-bit
codewords were then read). A decoder must not assume the 15- or 16-bit
limit familiar from other formats.

Meta symbols 0 to 20 and 31 to 36 all occur in real streams; 21 to 30 occur
only in crafted ones. In the real forks all the descriptions of a fork
together take between 328 and 2,600 bits.

### 6.7 Decoding items

Two things determine how the next item is read: the bytes output so far,
and the **current code**, which is "primary" at the start.

Steps, after the header and any table descriptions have been read:

1. If the output holds U bytes, stop: success. Nothing more is read.
2. Read one symbol *s* using the current code.
3. **Literal.** If *s* is 0 to 255: append the byte *s* to the output. The
   current code becomes "primary". Continue at step 1.
4. **End of data.** If *s* is 320: corrupt input (the data ends before U
   bytes exist; see 6.9).
5. **Match.** Otherwise *s* is 256 to 319.
   1. Determine the match length *L*:
      - *s* from 256 to 317: *L* = *s* - 253 (so 256 gives 3 and 317 gives
        64);
      - *s* = 318: read a 10-bit number *x*; *L* = *x* + 65;
      - *s* = 319: read a 15-bit number *x*; *L* = *x* + 65.
   2. Read one symbol *d* using the distance code.
   3. Determine the match distance *D*:
      - *d* = 0: *D* = 1;
      - *d* = 1: *D* = 2;
      - *d* of 2 or more: read a (*d* - 1)-bit number *y*;
        *D* = 2^(*d* - 1) + *y* + 1.
   4. If *D* is greater than the number of bytes output so far: corrupt
      input (section 6.8).
   5. Copy: *L* times, append to the output the byte that lies *D* positions
      before the current end of the output. Do this one byte at a time so
      that when *D* is less than *L* the bytes just appended are themselves
      copied (a distance of 1 repeats the last byte *L* times). Stop copying
      early if the output reaches U bytes.
   6. The current code becomes "secondary". Continue at step 1.

The order of fields for a match is therefore: length symbol, length extra
bits (only for 318 and 319), distance symbol, distance extra bits (only for
distance symbols 2 and up). All extra-bit fields are numeric: first bit read
is least significant. If the stream has fewer bits left than an extra-bit
field needs, the result is truncated input.

Notes:

- The switch between the two codes depends only on what kind of item was
  just decoded. After a literal the next item, whatever it is, is read with
  the primary code; after a match the next item, whatever it is (a literal,
  another match, or the end-of-data symbol), is read with the secondary
  code. The distance code is never affected.
- Symbols 318 and 319 overlap each other and the short forms: a length of
  65 can be written with either, and nothing forbids writing a length of 70
  with symbol 319. All are valid.

**Distance symbols in full**:

| Distance symbol *d* | Extra bits | Distances covered |
|---:|---:|:---|
| 0 | 0 | 1 |
| 1 | 0 | 2 |
| 2 | 1 | 3 to 4 |
| 3 | 2 | 5 to 8 |
| 4 | 3 | 9 to 16 |
| 5 | 4 | 17 to 32 |
| 6 | 5 | 33 to 64 |
| 7 | 6 | 65 to 128 |
| 8 | 7 | 129 to 256 |
| 9 | 8 | 257 to 512 |
| 10 | 9 | 513 to 1,024 |
| 11 | 10 | 1,025 to 2,048 |
| 12 | 11 | 2,049 to 4,096 |
| 13 | 12 | 4,097 to 8,192 |
| 14 | 13 | 8,193 to 16,384 |
| 15 | 14 | 16,385 to 32,768 |
| 16 | 15 | 32,769 to 65,536 |

A distance alphabet of *n* symbols therefore reaches back at most
2^(*n* - 1) bytes: 512 for the smallest alphabet (10 symbols), 1,024 for
predefined sets 1, 4 and 5, 4,096 for set 2, 8,192 for set 3, and 65,536 for
the largest dynamic alphabet (17 symbols).

### 6.8 Window

- **Size.** The format's largest expressible distance is 65,536, so the
  most recent 65,536 output bytes are all a decoder ever needs to look
  back at. Both black boxes accept 17-symbol alphabets and distances of
  65,535 and 65,536 (CONFIRMED (crafted)). In the real forks the largest
  distance is 32,763 and no distance alphabet has more than 16 symbols
  (largest expressible distance 32,768).
- **A match never reaches back before the start of the output.** A distance
  greater than the number of bytes produced so far is corrupt input. The
  test is made once, when the distance is known and before the first byte
  is copied; if it passes, every byte of the match, including those of an
  overlapping match, has a source at or after the start. (DECISION. The
  black boxes differ: `unar` behaves as if 65,536 zero bytes preceded the
  output and copies `00` for every position before the start, byte by
  byte, for any distance up to 65,536; the reference implementation stops
  quietly and returns the shorter output. None of the 1,080 real forks
  contains such a match. The four real forks that begin with a run of zero
  bytes all start with a literal `00` followed by a long match at distance
  1, which is what an encoder does that cannot refer to anything before the
  start.)
- The window is not shared between forks or between files.

### 6.9 End of data, and the role of U

U alone decides when decoding is finished.

- **Normal ending.** In all 1,080 real forks the items produce exactly U
  bytes, the last item being a complete literal or a complete match, and
  the very next symbol in the stream, read with whichever code is current
  (the secondary code in 725 forks, the primary code in 355), is 320. The
  reference implementation's own encoder writes symbol 320 in the same way.
  A decoder nevertheless stops at step 1 of section 6.7 as soon as U bytes
  exist and does **not** read that symbol. It must not check for it:
  streams without it are accepted by both black boxes (CONFIRMED (crafted):
  worked example 1 with its last 6 bits cleared, so that no end-of-data
  codeword follows, decodes normally in both), and after a match that was
  cut short by U there is no reason for one to follow.
- **Match cut by U.** If a match would run past U, only the bytes up to U
  are produced and decoding stops with success. This never happens in the
  real forks (the last item always ends exactly at U); both black boxes
  behave this way on crafted streams.
- **Symbol 320 before U bytes.** Corrupt input. Nothing after symbol 320 is
  ever decoded: there is no continuation, second block or restart.
  (CONFIRMED (crafted) that both black boxes stop decoding at an early 320,
  in either code and in all five predefined sets, and ignore the valid
  items placed after it. That this is an error rather than a short success
  is the DECISION of section 2.4.)
- **Padding.** After the end-of-data codeword real encoders append 0 to 31
  zero bits, so that P - 1 (the stream length without the header byte) is a
  multiple of 4 bytes. This holds for all 1,080 real forks, in both containers, and the padding bits are zero in every one of them. A decoder
  never reads these bits. Any value, and any number of further bytes, is
  acceptable there (CONFIRMED (crafted), both black boxes: worked example 1
  with its padding bit set, and with 40 arbitrary bytes appended, decodes
  normally). Other encoders, including the reference implementation's, pad
  only to the next whole byte.
- **Running out of input.** If the stream ends where a codeword bit, an
  extra-bit field or part of a table description is needed, the result is
  truncated input (or corrupt input in the one case defined in section 6.4,
  "Reading one symbol"). Missing input is never replaced by zero bits.
- **U = 0.** Nothing is read, not even the header byte (section 2.3).

That symbol 320 means "end of data" is a description of this behaviour: it
follows the last item in every real fork and both black boxes stop when
they read it. Nothing else is known or needed about it.

### 6.10 Worked example 1: predefined table set

Packed stream (7 bytes), U = 10: `11 A4 5F 4A 36 B4 5B`

| Byte index | Value | Stream bit positions | Bits, in the order read |
|---:|:---|:---|:---|
| 0 | `11` | 0-7 | `10001000` |
| 1 | `A4` | 8-15 | `00100101` |
| 2 | `5F` | 16-23 | `11111010` |
| 3 | `4A` | 24-31 | `01010010` |
| 4 | `36` | 32-39 | `01101100` |
| 5 | `B4` | 40-47 | `00101101` |
| 6 | `5B` | 48-55 | `11011010` |

Header byte `11`: mode 1, so predefined set 1. The low bits are ignored. The
distance alphabet has 11 symbols.

The codewords needed, obtained by applying the canonical rule of section 6.4
to the set 1 tables of section 6.5:

| Code | Symbol | Length (from the table) | Codeword |
|:---|:---|---:|:---|
| primary | 97 (literal "a") | 5 | `00100` |
| primary | 98 (literal "b") | 9 | `101111110` |
| primary | 99 (literal "c") | 8 | `10010100` |
| primary | 259 (match, length 6) | 8 | `10011011` |
| primary | 320 (end of data) | 5 | `01101` |
| secondary | 100 (literal "d") | 8 | `10110111` |
| distance | 2 | 3 | `000` |

As a check on an implementation of the canonical rule: in the set 1 primary
code the only symbol of length 4 is symbol 0, which gets `0000`. The symbols
of length 5 are, in symbol order, 1, 32, 97, 101, 110, 111, 114, 116, 255,
257, 258 and 320. They get the twelve consecutive codewords `00010` (symbol
1), `00011` (symbol 32), `00100` (symbol 97) and so on up to `01101`
(symbol 320).

Decoding, starting at bit position 8:

| Step | Current code | Bit positions | Bits read | Meaning | Output so far |
|---:|:---|:---|:---|:---|:---|
| 1 | primary | 8-12 | `00100` | symbol 97: literal "a" | a |
| 2 | primary | 13-21 | `101111110` | symbol 98: literal "b" | ab |
| 3 | primary | 22-29 | `10010100` | symbol 99: literal "c" | abc |
| 4 | primary | 30-37 | `10011011` | symbol 259: match, length 259 - 253 = 6 | |
| | distance | 38-40 | `000` | distance symbol 2: one extra bit follows | |
| | | 41 | `0` | extra value 0: distance = 2 + 0 + 1 = 3 | |
| | | | | 3 bytes have been output, so distance 3 is valid; copy 6 bytes from 3 back, one at a time: a, b, c, a, b, c | abcabcabc |
| 5 | secondary | 42-49 | `10110111` | symbol 100: literal "d" | abcabcabcd |

The output now holds 10 bytes = U: stop. Output: "abcabcabcd" (checksum
`918E`).

The match in step 4 overlaps itself: its length (6) exceeds its distance
(3), so its last three bytes are copies of its own first three.

What remains in the stream, for information only (a decoder does not read
it): bit positions 50 to 54 hold `01101`, which is symbol 320 in the primary
code (primary, because the last item was a literal). Bit 55 is padding.

Variants of this stream, all with U = 10 unless stated:

| Stream | Result |
|:---|:---|
| `11 A4 5F 4A 36 B4 5B 00 00` | "abcabcabcd". This is how a real encoder lays the same data out: zero padding up to P - 1 = 8, a multiple of 4. |
| `1F A4 5F 4A 36 B4 5B` | "abcabcabcd" (low header bits ignored in predefined modes) |
| `11 A4 5F 4A 36 B4 DB` | "abcabcabcd" (padding bit set) |
| `11 A4 5F 4A 36 B4 03` | "abcabcabcd" (no end-of-data codeword after the last item) |
| `11 A4 5F 4A 36 B4 5B`, U = 7 | "abcabca" (the match is cut by U) |
| `11 A4 5F 4A 36 B4 5B`, U = 11 | corrupt input: symbol 320 is read as the sixth item when only 10 bytes exist |
| `11 A4 5F 4A 36` | truncated input: the stream ends at bit 40, inside the distance codeword of step 4 |
| `61 A4 5F 4A 36 B4 5B` | corrupt input: mode 6 |

### 6.11 Worked example 2: dynamic tables

Packed stream (18 bytes), U = 7:
`08 E8 47 0C 76 23 E8 47 FF 1C 88 A0 18 08 BB E7 D0 74`

| Byte index | Value | Stream bit positions | Bits, in the order read |
|---:|:---|:---|:---|
| 0 | `08` | 0-7 | `00010000` |
| 1 | `E8` | 8-15 | `00010111` |
| 2 | `47` | 16-23 | `11100010` |
| 3 | `0C` | 24-31 | `00110000` |
| 4 | `76` | 32-39 | `01101110` |
| 5 | `23` | 40-47 | `11000100` |
| 6 | `E8` | 48-55 | `00010111` |
| 7 | `47` | 56-63 | `11100010` |
| 8 | `FF` | 64-71 | `11111111` |
| 9 | `1C` | 72-79 | `00111000` |
| 10 | `88` | 80-87 | `00010001` |
| 11 | `A0` | 88-95 | `00000101` |
| 12 | `18` | 96-103 | `00011000` |
| 13 | `08` | 104-111 | `00010000` |
| 14 | `BB` | 112-119 | `11011101` |
| 15 | `E7` | 120-127 | `11100111` |
| 16 | `D0` | 128-135 | `00001011` |
| 17 | `74` | 136-143 | `00101110` |

**Header** `08`: mode 0 (dynamic); shared flag set, so there is no secondary
description and the secondary code equals the primary code; low three bits
0, so the distance alphabet has 10 symbols.

**Primary code description** (321 symbols, current length starts at 0):

| Bit positions | Bits read | Meta symbol | Extra bits | Current length after | Symbols assigned |
|:---|:---|---:|:---|---:|:---|
| 8-18 | `00010` `111111` | 36 | *r* = 63 | 0 | 74 symbols: 0 to 73 |
| 19-29 | `00010` `001100` | 36 | *r* = 12 | 0 | 23 symbols: 74 to 96 |
| 30-40 | `00011011101` | 0 | | 1 | symbol 97 |
| 41-42 | `10` | 32 | | 2 | symbol 98 |
| 43-47 | `00100` | 31 | | -1 | symbol 99 |
| 48-58 | `00010` `111111` | 36 | *r* = 63 | -1 | 74 symbols: 100 to 173 |
| 59-69 | `00010` `111111` | 36 | *r* = 63 | -1 | 74 symbols: 174 to 247 |
| 70-76 | `1100` `111` | 35 | *r* = 7 | -1 | 10 symbols: 248 to 257 |
| 77-84 | `00000010` | 2 | | 3 | symbol 258 |
| 85-89 | `00100` | 31 | | -1 | symbol 259 |
| 90-100 | `00010` `100011` | 36 | *r* = 49 | -1 | 60 symbols: 260 to 319 |
| 101-108 | `00000010` | 2 | | 3 | symbol 320 |

(The extra-bit fields are shown as read; the first bit is the least
significant, so `001100` is 4 + 8 = 12 and `100011` is 1 + 16 + 32 = 49.)

All 321 symbols are now assigned. The present symbols are 97 with length 1,
98 with length 2, 258 with length 3 and 320 with length 3; every other
symbol was assigned 0 or -1, meaning absent. The weight is 1/2 + 1/4 + 1/8
+ 1/8 = 1. The canonical rule gives:

| Symbol | Length | Codeword |
|:---|---:|:---|
| 97 (literal "a") | 1 | `0` |
| 98 (literal "b") | 2 | `10` |
| 258 (match, length 5) | 3 | `110` |
| 320 (end of data) | 3 | `111` |

**Secondary code**: same as the primary code (shared flag).

**Distance code description** (10 symbols, current length starts again at
0):

| Bit positions | Bits read | Meta symbol | Extra bits | Current length after | Symbols assigned |
|:---|:---|---:|:---|---:|:---|
| 109-119 | `00011011101` | 0 | | 1 | symbol 0 |
| 120-123 | `111` `0` | 34 | *f* = 0 | 1 | 1 symbol: symbol 1 |
| 124-125 | `01` | 33 | | 0 | symbol 2 |
| 126-132 | `1100` `001` | 35 | *r* = 4 | 0 | 7 symbols: 3 to 9 |

Distance symbols 0 and 1 have length 1; the rest are absent. Codewords:
distance symbol 0 is `0`, distance symbol 1 is `1`.

**Items**, starting at bit position 133:

| Step | Current code | Bit positions | Bits read | Meaning | Output so far |
|---:|:---|:---|:---|:---|:---|
| 1 | primary | 133 | `0` | symbol 97: literal "a" | a |
| 2 | primary | 134-136 | `110` | symbol 258: match, length 258 - 253 = 5 | |
| | distance | 137 | `0` | distance symbol 0: distance 1, no extra bits | |
| | | | | copy 5 bytes from 1 back: a, a, a, a, a | aaaaaa |
| 3 | secondary | 138-139 | `10` | symbol 98: literal "b" | aaaaaab |

Seven bytes = U: stop. Output: "aaaaaab" (checksum `2423`).

Remaining bits, for information only: positions 140 to 142 hold `111`,
symbol 320 in the primary code; position 143 is padding.

Both worked examples, and every row of the variants table in 6.10, were
given to both black boxes. They produce the outputs shown for the
successful rows, and `unar` accepts the checksums given. For the error rows
see section 6.13.

### 6.12 Further test vectors

These streams exercise parts of the format the two worked examples do not.
Each was decoded by both black boxes with the result shown, except where
noted. "Checksum" is the CRC-16 of the expected output.

**One vector per remaining predefined set.** The items are the same in all
four: the literals "hello " (six bytes, the last a space), a match of length
12 at distance 6, and the literal "!". The match symbol is read with the
primary code, the final literal with the secondary code. U = 19; the output
is "hello hello hello !" (checksum `B7C3`).

| Set | Packed stream |
|---:|:---|
| 2 | `23 29 AE 28 1A 3B D2 F5 7B 1F` |
| 3 | `34 23 8D 05 05 45 4D 3F EE 2E FC 1F` |
| 4 | `41 5D 66 24 12 33 DE 9A C6 08` |
| 5 | `51 8C 2F 18 84 60 E9 DB 6B FB 01` |

(The reference implementation fails on the set 4 row, section 6.5; `unar`
decodes all four.)

**Long match lengths.**

| Purpose | Packed stream | U | Output | Checksum |
|:---|:---|---:|:---|:---|
| symbol 318 and its 10-bit field | `11 E4 FF 11 DE 24` | 101 | 101 bytes of "a": literal "a", then symbol 318 with field value 35 (length 100) at distance 1 | `4238` |
| symbol 319 and its 15-bit field | `11 A4 DF FA 05 C2 D7 66 01` | 1,103 | "ab" repeated 551 times, then "c": literals "a", "b", then symbol 319 with field value 1,035 (length 1,100) at distance 2, then literal "c" read with the secondary code | `A0E4` |

**A distance with several extra bits.** Set 3; the literals
"0123456789abcdefghijklmnopqrstuvwxyz" (36 bytes), then a match of length 4
at distance 27, then the literal "!". Distance 27 is distance symbol 5 with
the 4-bit field value 10, whose bits appear in the stream in the order
`0`, `1`, `0`, `1` (least significant first) directly after the distance
codeword `001`, which is read most significant digit first. A decoder that
read the field in the wrong order would get the value 5 and the distance 22.
U = 41; output "0123456789abcdefghijklmnopqrstuvwxyz9abc!" (checksum `402E`).

`34 09 ED DA 77 D8 6E FB 1D 76 EC D8 29 7B 78 CE 08 63 6E DE C8 FC BD F6 2E
18 55 58 54 BC 4F 49 69 59 F9 E8 31 63 C7 ED CB 52 2F FC 1F`

**Dynamic tables with a separate secondary code.** Header `00`: dynamic,
shared flag clear, 10 distance symbols.

`00 E8 47 0C D6 13 F4 A3 7F 08 44 D0 0C 84 7E F4 60 97 A0 1F FD 43 2C 82 64
2C EC 76 64 86 6D 1D 07` (33 bytes), U = 12.

| Part | Bit positions | Commands, as "meta symbol" or "meta symbol(extra value)" | Result |
|:---|:---|:---|:---|
| primary description | 8-107 | 36(63), 36(12), 1, 34(1), 31, 36(63), 36(63), 35(4), 2, 31, 36(51), 2 | symbols 97, 98, 99 length 2; 256 and 320 length 3; others absent |
| secondary description | 108-206 | 36(63), 36(15), 0, 31, 36(63), 36(63), 35(4), 1, 31, 36(50), 1 | symbol 100 length 1; 257 and 320 length 2; others absent |
| distance description | 207-235 | 0, 32, 34(0), 31, 35(3) | symbols 0, 1, 2 with lengths 1, 2, 2; others absent |

Points to note: in the primary description command 34 with *f* = 1 assigns
two symbols (98 and 99). The primary description ends with the current
length at 3; the secondary description begins with command 36, which
assigns the current length to 74 symbols, and those symbols are absent
because the current length has started again at 0. A decoder that carried
the 3 over would build an over-subscribed secondary code.

Codewords: primary 97 = `00`, 98 = `01`, 99 = `10`, 256 = `110`,
320 = `111`; secondary 100 = `0`, 257 = `10`, 320 = `11`; distance 0 = `0`,
1 = `10`, 2 = `11`.

| Current code | Bit positions | Bits read | Meaning | Output so far |
|:---|:---|:---|:---|:---|
| primary | 236-237 | `00` | literal "a" | a |
| primary | 238-239 | `01` | literal "b" | ab |
| primary | 240-241 | `10` | literal "c" | abc |
| primary | 242-244 | `110` | symbol 256: match, length 3 | |
| distance | 245-247 | `11` `0` | distance symbol 2, extra value 0: distance 3 | abcabc |
| secondary | 248-249 | `10` | symbol 257: match, length 4 (a match read with the secondary code) | |
| distance | 250-252 | `11` `1` | distance symbol 2, extra value 1: distance 4 | abcabccabc |
| secondary | 253 | `0` | literal "d" | abcabccabcd |
| primary | 254-255 | `00` | literal "a" | abcabccabcda |

Output "abcabccabcda" (checksum `FBBA`). Bits 256-258 hold `111`, symbol
320 in the primary code. With U = 13 or more the same stream is corrupt
input, because that symbol is then read.

**End-of-data symbol in the secondary code.** The same three descriptions,
followed by: literal "a" (`00`), symbol 256 (`110`) with distance symbol 0
(`0`), then `11`, which is symbol 320 in the secondary code.

`00 E8 47 0C D6 13 F4 A3 7F 08 44 D0 0C 84 7E F4 60 97 A0 1F FD 43 2C 82 64
2C EC 76 64 C6 0C` (31 bytes).

With U = 4 the output is "aaaa" (checksum `5AA6`) and the `11` at bit
positions 242-243 is not read. With U = 5 or more the result is corrupt
input: the item after the match is read with the secondary code and is
symbol 320. (Both black boxes stop there with 4 bytes.)

**Over-subscribed primary code in a stream that ends early.** This vector
is derived from the rules and was not given to the black boxes. Header
`00`: dynamic, shared flag clear, 10 distance symbols.

`00 D8 7D 82 7E F4 A3 1F FD 48 01` (11 bytes), U = 1.

The primary description occupies bit positions 8 to 82 and consists of the
commands 0, 34(1), 31, 36(63), 36(63), 36(63), 36(63), 36(10): symbols 0,
1 and 2 have length 1 and the other 318 symbols are absent. The weight is
3/2, so the primary code is over-subscribed and the result is **corrupt
input**, decided at bit position 83 without reading further (section
6.6.2). Only the five zero bits 83 to 87 follow; a decoder that postponed
the weight test until all descriptions had been read would run out of
input inside the secondary description (five zero bits begin a meta
codeword but complete none) and report truncated input, which is wrong.

### 6.13 Evidence

CONFIRMED (real data), on all 1,080 forks: the header layout, both bit
conventions, the canonical rule, all five predefined sets, the meta code
(meta symbols 0 to 20 and 31 to 36), separate and shared secondary codes,
the alternation between primary and secondary code, both long-length
symbols, every distance symbol from 0 to 15, stopping at U, symbol 320
directly after the last item, and the zero padding to a multiple of 32 bits
after the header byte.

CONFIRMED (crafted), both black boxes unless a difference is stated:

- everything in the previous paragraph again, for all five predefined sets
  with every symbol exercised (the reference implementation fails for set
  4 only), for all sixteen dynamic header values, and for code lengths up
  to 31, which exercises meta symbols 21 to 30;
- distance symbol 16 and distances 65,535 and 65,536;
- every successful row in sections 6.10 to 6.12;
- incomplete codes (section 6.4), absent symbols with value 0 and with
  values below -1 (section 6.6.2), rejection of modes 6 to 15;
- the behaviour reported next to each DECISION.

How the black boxes respond to the error rows of this document, for
orientation (the required results are those in the rows):

| Situation | `unar` | Reference implementation |
|:---|:---|:---|
| mode 6 to 15 | error | error |
| over-subscribed code | error | no error; wrong output or abnormal termination |
| unassigned bit string read from an incomplete code | error | stops; short output returned as success |
| symbol 320 before U bytes | stops; short output, then its checksum test fails | stops; short output returned as success |
| stream ends early | error | section 8.4 |
| distance before the start of the output | copies zero bytes | stops; short output returned as success |
| repeat command past the last symbol | as section 6.6.2 | assigns what fits and continues |
| assigned length 32 | accepted as a 32-bit codeword | the same, or abnormal termination |

INFERRED points: none in this section. DECISION points are marked in
sections 6.6.2, 6.8 and 6.9 and listed in section 9.

## 7. Safety requirements for untrusted input

A decoder for these methods will be given hostile data. It must never read
or write out of bounds, run for a time not bounded by P and U, use stack
depth that grows with the input, or commit memory in proportion to a number
it has not validated. This section states the limits; how an implementation
meets them is its own business.

### 7.1 All methods

1. **Validate before decoding**, in the order of section 2.3: U = 0, P = 0,
   the method number and the host limit (all four by the container layer),
   then the format limit (by the decoder; section 2.6).
2. **U is not an allocation size.** U is a 32-bit header field and can
   claim 4 GiB. Memory for the output may be reserved ahead of the bytes
   actually produced only up to a fixed bound that the implementation
   chooses independently of the archive; beyond that it grows as bytes are
   produced.
3. **Every read is bounds-checked against P.** Needing a bit or byte at or
   beyond position 8 × P is "truncated input"; it is never satisfied with
   zero bits or any other invented data.
4. **Never more than U bytes** are produced, including in the middle of a
   run, a string or a match.
5. **Progress.** Every step of every method either consumes at least one
   bit of input or produces at least one byte of output, with one exception:
   the single-leaf tree of method 3 produces bytes without consuming input.
   Total work is therefore bounded by a constant times (8 × P + U), and U is
   bounded by section 2.6.
6. **Fewer than U bytes is an error**, never success (section 2.4).
7. **The fork checksum is verified** over the U bytes produced, by the
   container layer (section 2.5), and a mismatch is reported; a fork whose
   checksum has not been verified is not delivered.
8. **Keep the error kinds apart**: truncated input and corrupt input in
   the decoder; unsupported method, output limit exceeded and checksum
   mismatch in the container layer.
9. **No state survives a fork.** Every table, code and window is built
   afresh for each fork.

### 7.2 Method 1

- A marker `90` as the last byte of the stream (no count byte) is truncated
  input.
- Count `01` is corrupt input.
- One marker pair produces at most 254 bytes.

### 7.3 Method 2

- The code width is always 9 to 14 and is a function of the position in the
  segment only (section 4.2); it never depends on a value read from the
  stream.
- At most 16,127 entries (257 to 16,383) exist at any time.
- The only acceptable code values at position *k* are those of the table in
  section 4.2. In particular 256 is never looked up as a string, a value
  above 255 + *k* is corrupt, and so is a value of 257 or more at
  position 1.
- A string can be as long as 16,128 bytes, and a hostile stream can make
  every entry about as long as its number allows. If every entry were held
  as a separate copy of its bytes the table would need about 130 MiB.
  Since every entry is by definition an earlier string plus one byte, the
  information in the table is one earlier code value and one byte per
  entry; working memory for method 2 must be bounded by a small constant
  times the number of entries, independent of the string lengths.
- Producing a string must not use stack depth proportional to its length.
- The reset padding (up to 98 bits) is bounds-checked like any other read.

### 7.4 Method 3

- The tree has at most 256 leaves, 255 branches and 511 nodes
  (section 5.2); a 257th leaf or a 256th branch is corrupt input. This
  bounds memory and depth.
- A branch can be 255 levels below the root. Reading the tree and walking it
  must not use stack depth that an attacker controls: a stream of zero bits
  describes an endless chain of branches, and must end in "corrupt input"
  at the 256th branch (bit position 255), or in "truncated input" if the
  stream is shorter than that.
- End of input during the tree or during a walk is truncated input.
- A single-leaf tree lets 2 bytes of input stand for any U. That is
  legitimate (a fork consisting of one repeated byte), so it is bounded
  only by the host limit of section 2.6.
- For a tree with two or more leaves, U above 8 × P - 19 is corrupt input.

### 7.5 Method 13

- **Header.** Modes 6 to 15 are corrupt input.
- **Dynamic descriptions.** The table sizes are fixed by the header (321,
  321, and 10 to 17), so no allocation depends on later stream content. A
  repeat command that would assign more symbols than remain is corrupt
  input. An assigned value above 31 is corrupt input. The current length
  stays between -321 and 351 and must be held in a signed type wide enough
  for that range. A description cannot fail to terminate: each command
  assigns at least one symbol, so there are at most 321 commands.
- **Code construction.** The weight is computed exactly, with an
  accumulator of at least 64 bits in units of 2^-31, before codewords are
  assigned; an over-subscribed code is corrupt input. Incomplete and empty
  codes are accepted (section 6.4).
- **Code lengths reach 31 bits.** Any decoding structure indexed directly by
  a whole codeword is therefore ruled out (2^31 entries). Whatever
  structure is used, its size must be bounded by the number of symbols (at
  most 321), not by the code lengths: a complete code over 321 symbols has
  320 internal branching points, but an incomplete code with a few 31-bit
  codewords has long unbranched paths, up to 31 bits per present symbol.
- **Reading a symbol terminates** after at most 31 bits, with a symbol,
  "corrupt input" or "truncated input" as defined in section 6.4.
- **Distances.** A distance greater than the number of bytes produced so
  far is corrupt input; no byte before the start of the output is ever
  read. A distance never exceeds 65,536.
- **Lengths.** A single match can be 32,832 bytes; the copy is cut at U.
- **Symbol 320 before U bytes** is corrupt input.
- **Nothing is read after the item that completes U bytes.**

## 8. Observed implementation gaps

These are behaviours of the reference implementation (version 0.1.8),
observed by giving it the inputs described, that a new implementation must
not inherit. They are facts about one program, not about the format. Where
the program can be built with or without run-time arithmetic checking, both
outcomes are given.

### 8.1 Method 1

- **A different format is decoded.** The program treats the byte after the
  count as the value to repeat and outputs it *count* times, where the
  format (section 3) has no value byte and repeats the *preceding* byte so
  that it occurs *count* times in total. For the example of section 3.3 it
  returns the eight bytes `41 42 42 42 42 42 90 43` as a success, where the
  correct output is the ten bytes `41 41 41 41 41 42 90 43 43 43`. Of the
  rows in the second table of section 3.3 it reproduces none that contains
  a run. Any real method 1 fork containing a run would be mis-decoded.
- A stream that ends after a marker or after a count byte yields a short
  output reported as success.
- `90 01` is not rejected.

### 8.2 Method 2

- Time and memory grow with the square of the input on adversarial streams
  (long strings): the cost of section 7.3 is not bounded.
- Running out of input, including inside reset padding, yields a short
  output reported as success.
- **Some valid resets are rejected.** When the padding after a reset code
  is 64 bits or longer, the program can lose its place in the stream. This
  was reproduced with a reset at positions 1,794, 1,802, 1,810 and 1,818 of
  a segment (width 12, six padding codes, 72 bits): a build without
  arithmetic checking reports the valid data that follows as corrupt; a build with arithmetic checking terminates abnormally. `unar`
  decodes the same streams correctly. Whether the fault strikes depends on
  the alignment of the reset within the stream, not on the width as such;
  the real forks, whose resets have 70 and 84 padding bits, happen to
  decode correctly.

### 8.3 Method 3

- **End of input is not detected.** Past the end of the stream every bit
  reads as 0, so a truncated stream is "decoded" along the all-zeros path
  until U bytes exist, and returned as success. Examples: the first three
  bytes of the stream of section 5.4 (`50 54 2A`) with U = 8 give eight
  bytes "AAAAAAAA"; the first four bytes (`50 54 2A 1A`) with U = 8 give
  "AB" followed by six "A"; the complete stream with U = 9 gives "ABCCBA"
  followed by "AAA".
- **Large trees are mis-decoded without an error.** A tree with 257 or more
  leaves yields wrong bytes or an empty output, reported as success. (A
  lopsided tree with 257 leaves gave `01 00 00` where the tree defines
  `00 00 01`; balanced trees with 1,024 and 4,096 leaves gave an empty
  output.)
- An endless chain of branches (a stream of zero bytes) yields an empty
  output reported as success.

### 8.4 Method 13

- **Predefined set 4 is wrong in three entries** (section 6.5). Every real
  mode 4 fork is mis-decoded: all 31 distinct ones in the survey (53
  counting copies) come out shorter than U, for example 19 bytes of 936 and
  309 of 596, and are reported as success. These are the only real forks
  the program gets wrong: all 998 distinct real forks in the other modes
  decode to their fork checksum. (The truncated method 13 forks mentioned
  in section 7 of `containers.md` are these same 53 mode 4 forks. They lie
  in five distinct archives: 27, 17, 4, 3 and 2 forks. No rule of this
  document is involved other than the three table entries.)
- **End of input is not detected**, in the table descriptions or in the
  items. In a build with arithmetic checking the program terminates
  abnormally at the first read past the end. In a build without it, every
  later bit reads as 0, with these observed results: the first three bytes
  of worked example 1 with U = 10 give ten bytes (`61 62 50` and seven
  `00`) as a success; a stream consisting of the single byte `11` with
  U = 10 gives ten `00` bytes as a success; worked example 2 cut to 5 or to
  16 bytes terminates abnormally; cut to 13 bytes it gives seven bytes "a"
  as a success. A few bytes of input can thus be made to produce U bytes of
  output.
- **Quiet stops.** A match distance beyond the start of the output, a bit
  string that is not a codeword in any of the three codes, and symbol 320
  before U bytes all end decoding without an error; the short output is
  returned as success. The caller cannot tell these from a correct decode.
- **Code lengths are not validated.** Over-subscribed length lists are
  accepted: a primary code with three symbols of length 1 "decodes" to
  bytes; one with lengths 1, 2, 2, 2 terminates abnormally. Assigned values
  of 33 and above are silently treated as absent; a value of 32 is used as
  a 32-bit codeword and, in a build with arithmetic checking, can terminate
  the program abnormally.
- **Repeat commands past the last symbol are accepted** (the excess is
  dropped).
- **The program's own encoder**, relevant when its output is used as test
  material: it always uses mode 1 with header byte `10` (where real
  encoders write `11`), ends every stream with symbol 320, pads with zero
  bits to a whole byte only, and for an empty input writes the header byte
  alone. Its streams are valid but exercise a small part of the format: no
  dynamic tables, no other table set, no distance beyond 1,024.

### 8.5 Across methods

- **The fork checksum is never verified.** Together with the quiet stops
  above, corrupt or truncated forks are returned as successful results.
- **A short output is success.** The number of bytes produced is never
  compared with U.
- **Unsupported method numbers** are answered by returning the packed
  bytes as if they were the decoded fork (observed for method numbers 4 to
  12, with U = 0 and with U greater than 0). This must be an error
  (section 2.3).
- **P = 0 with U greater than 0** is returned as an empty fork, as success,
  for every method.
- **Memory proportional to U is requested** for all four methods before
  anything in the stream has been examined.

### 8.6 The second black box

`unar` was used only as an oracle for well-formed streams and to see how a
second decoder reacts to malformed ones. Three of its reactions must not be
taken as a model either: it terminates abnormally on a method 13 primary
description whose repeat command overruns the table by 9 or more symbols;
it crashes on a method 3 stream of 100,000 zero bytes (an endless chain of
branches); and it accepts a fork that decodes to fewer than U bytes when
the CRC-16 of the shorter output happens to equal the fork checksum (seen
with worked example 1 and U = 11).

## 9. INFERRED points and decisions

### 9.1 INFERRED

One point remains inferred. It concerns a method for which no real archive
was available, and could not be settled with the material at hand.

1. **Method 1** (section 3): that the rule implemented by `unar` (the count
   applies to the preceding byte and gives total occurrences; the last byte
   starts as `00`; a literal `90` becomes the last byte) is the rule the
   original software used. The two black boxes disagree; `unar`'s rule is
   the widely described scheme of the era and is the one specified.

Method 3 (section 5) was inferred in an earlier draft; one real fork now
confirms the tree serialization, the bit order and the walk (section 5.6).

### 9.2 DECISIONS

Each of these prescribes one behaviour for malformed input where the black
boxes are lenient or disagree. None is reached by any of the 1,087 real
forks.

| # | Section | Situation | Required result | `unar` | Reference implementation |
|---:|:---|:---|:---|:---|:---|
| 1 | 2.3 | P = 0, U greater than 0 | corrupt input | empty success (methods 1, 2); error (3, 13) | empty success |
| 2 | 2.4 | decoding cannot continue before U bytes exist | truncated or corrupt input; no contents delivered | short output, then usually a checksum failure | short output as success |
| 3 | 2.6 | U larger than P bytes can produce | corrupt input, before decoding | no such check | no such check |
| 4 | 3.1 | method 1 pair `90 01` | corrupt input | error | accepted |
| 5 | 5.2 | method 3 tree with more than 256 leaves | corrupt input | accepted | wrong output as success |
| 6 | 6.6.2 | assigned code length of 32 or more | corrupt input | 32 accepted; 33 and above treated as absent | the same |
| 7 | 6.6.2 | repeat command past the last symbol | corrupt input | accepted in some cases, abnormal termination in others | excess dropped |
| 8 | 6.8 | match distance greater than the bytes produced so far | corrupt input | zero bytes supplied | short output as success |
| 9 | 6.9 | symbol 320 before U bytes | corrupt input | short output, then usually a checksum failure | short output as success |
| 10 | 6.9 | what follows the item that completes U bytes | not read, not checked | not read | not read |
| 11 | 2.3 | U above the format limit in a stream that is also too short | corrupt input (the limit is tested before any read) | no such check | no such check |
| 12 | 2.6 | a decoder called directly applies its own output cap | permitted; reported as "output limit exceeded", never as corrupt input | not applicable | not applicable |
| 13 | 6.6.2 | over-subscribed code followed by a truncated later description | corrupt input (each code is tested when its description ends) | error | no weight test |

### 9.3 Points formerly open, now settled

For the record, these were open in an earlier draft and are now CONFIRMED
as stated in the sections named: reset padding at code widths 10 to 13
(4.7); duplicate leaf values in method 3 (5.2); incomplete codes (6.4);
absent symbols with values of 0 and below -1 (6.6.2); modes 6 to 15 (6.3);
the header's low bits in predefined modes (6.3); symbol 320 after the last
item and the 32-bit padding (6.9); the three corrected entries of
predefined set 4 (6.5), which rest on 53 real forks with matching
checksums, on the weight of the codes, and on every-symbol crafted streams
in `unar`.
