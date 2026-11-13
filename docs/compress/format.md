# The Unix `compress` (`.Z`) file format

Format specification for clean-room implementation. This document describes
the file written by the Unix `compress` program, versions 2 to 4 and their
descendants ((N)compress, the BSD `compress`): a 3-byte header followed by a
stream of variable-width LZW codes. It covers reading only.

`README.md` in this directory is the index and lists every source that was
consulted.

The document contains no program code and no pseudo-code. Procedures are
written as numbered steps in prose; every constant an implementer needs is
written out.

## 1. How to read this document

### 1.1 Provenance

- The rules were derived from public documentation, from the source code
  of four existing implementations ((N)compress, GNU gzip's LZW reader,
  the BSD `compress` library routine, the LZW reader of zlib's `gun`
  example) and from experiments with existing tools run as black boxes.
  The exact list, with licences and tool versions, is in `README.md`
  section 3.
- A throwaway decoder that follows the rules of this document literally,
  and a throwaway packer that turns a list of codes into a file, were
  written by the specification's author, kept outside the repository, and
  used for the experiments of section 1.2. They are not part of the
  specification and must not be looked for.
- The implementation in this repository is written **from this document
  only**. Implementers do not read the source of any `compress`
  implementation.

### 1.2 Evidence labels

- **CONFIRMED (real tools)**: checked on files produced by a real encoder
  from generated input. Two encoders were used, the BSD `compress` shipped
  with macOS and (N)compress 4.2.4.6. The throwaway decoder, with every
  situation this document calls an error treated as an error, reproduced
  the input exactly. The black-box decoders were run as well, with the
  same output, on four files of the second group below and on the ten
  files of section 17.4 (except where a decoder refuses the header,
  section 19; the dialect B file of section 11 is the stated exception).
  The files were:
  - 72 files made by the BSD encoder from random, two-symbol, textual and
    mixed input of 1 to 60,000 bytes with maximum widths 9 to 16
    (434,654 input bytes in total, 6 CLEAR codes);
  - 14 files made by both encoders with maximum widths 10 to 16 from one
    150,000-byte input that forces 0 to 4 CLEAR codes per file;
  - the 26 files of section 17.4 and 17.5.
- **CONFIRMED (crafted)**: checked by building a file by hand from the
  rules of this document and decoding it with the black-box decoders. A
  result is quoted as agreed when the five decoders that implement the
  whole format (the BSD `uncompress`, Apple's `gzip`, (N)compress, GNU
  gzip, BusyBox) give the same output. They are separately maintained
  programs, although most descend from the same original. libarchive's
  reader disagrees on some crafted files and is discussed in section 19.
- **CONFIRMED (differential)**: 440 damaged variants of eleven valid files
  (one bit flipped, one byte replaced, truncated, or 1 to 5 random bytes
  appended) were decoded by the throwaway decoder, by GNU gzip and by
  (N)compress. All three accepted the same 357 files with identical
  output and rejected the same 83.
- **SOURCE**: read in the source of at least one implementation and not
  separately tested.
- **DOCUMENTED**: stated by documentation (manual pages, release notes).
- **INFERRED**: deduced, not confirmed.
- **DECISION**: a behaviour chosen by this specification, usually for
  malformed or ambiguous input. All decisions are listed in section 18.

### 1.3 Notation

- Numbers are decimal unless written with a `0x` prefix. Bytes of a file
  are written as two hexadecimal digits each: `1F 9D 90`.
- File offsets count bytes from 0.
- **M** is the maximum code width declared in the header, **L** the table
  limit, 2 to the power M.
- **n** is the current code width in bits.
- **free** is the next code number that will be assigned to a new table
  entry.
- **previous code** is the code decoded immediately before the current
  one. There is none before the first code of the stream and none
  immediately after a CLEAR code.
- A **literal** is a code from 0 to 255; it stands for that one byte.
- **Block mode** is the variant with a CLEAR code (header bit 7 set);
  **non-block mode** is the variant without one.

## 2. The contract

The decoder is given the complete bytes of one file. It returns the
decompressed bytes, or an error. Error kinds:

- **not this format**: the header rules of section 3 are not met;
- **corrupt input**: any rule of this document marked "corrupt input";
- **output limit exceeded**: the limit of section 16 was reached.

Properties that shape everything else:

- The file stores **no length, no checksum, no end marker, no file name
  and no timestamp**. The compressed data ends where the file ends.
  CONFIRMED (real tools).
- Consequently a damaged file usually decodes without any error to wrong
  output. In the differential test 357 of 440 damaged files were accepted
  by every decoder. The GNU gzip manual says the same in words: the
  format was not designed to allow consistency checks. A caller must not
  treat a successful decode as proof of integrity.
- One file holds exactly one compressed stream of one unnamed payload.
  Files are not concatenated: bytes following a stream are read as more
  codes of the same stream (section 12).

## 3. Header

The file starts with three bytes.

| Offset | Size | Content |
|---|---|---|
| 0 | 1 | `0x1F` |
| 1 | 1 | `0x9D` |
| 2 | 1 | flags |
| 3 | to end of file | code stream (section 4), possibly empty |

The flags byte:

| Bits | Mask | Meaning |
|---|---|---|
| 0 to 4 | `0x1F` | M, the maximum code width in bits |
| 5 | `0x20` | reserved, zero |
| 6 | `0x40` | reserved, zero |
| 7 | `0x80` | block mode: code 256 is the CLEAR code |

Facts:

- Every encoder examined writes bit 7 set and bits 5 and 6 clear. The
  default M is 16, so the usual header is `1F 9D 90`. CONFIRMED (real
  tools). The other values seen from real encoders are `0x89` to `0x8F`
  (option `-b 9` to `-b 15`).
- Bit 7 clear marks the older format without a CLEAR code. Later
  versions could still write it on request through a compatibility
  option, which (N)compress 4.2.4.6 still offers (in broken form, section
  19). INFERRED: the option is remembered as "compatible with version
  2.0"; this was not re-checked. Non-block files are rare; they must be
  read (section 5).
- Bits 5 and 6 were never assigned. The source comments call them free
  and suggest that `0x20` could one day announce a fourth header byte;
  no version did so. SOURCE. Existing decoders differ: the BSD decoders
  and BusyBox ignore them; GNU gzip 1.12 and (N)compress 4.2.4.6 decode
  the file and exit with a warning status (CONFIRMED (crafted), E12 and
  E13); zlib's `gun` rejects the file (SOURCE).
- The very first versions of `compress` wrote no header at all. SOURCE.
  Such files cannot be recognised and are out of scope.
- Neighbouring magic numbers that are **not** this format: `1F 8B` gzip,
  `1F 1E` `pack`, `1F A0` the LZH variant of SCO `compress -H`. (The
  gzip manual names these formats; the byte values are recalled and were
  not re-checked for this work.)

Rules:

1. A file shorter than 3 bytes, or not starting with `1F 9D`, is **not
   this format**. This includes the zero-length file (section 13).
2. If bit 5 or bit 6 of the flags byte is set, the file is **not this
   format**. (DECISION D1.)
3. If M is less than 9 or greater than 16, the file is **not this
   format**. (DECISION D2. What existing decoders accept is listed in
   section 19.)
4. Block mode is on when bit 7 is set.

## 4. Bit packing

The code stream is a sequence of bits. Bits are numbered from the **least
significant bit of each byte upward**: bit 0 of the stream is the lowest
bit of the byte at offset 3, bit 8 is the lowest bit of the byte at
offset 4.

A code of width n occupies n consecutive stream bits; the first of them is
the code's least significant bit. A code therefore spans two or three
bytes.

Example: the two 9-bit codes 97 and 98 (the letters `a` and `b`) are the
bytes `61 C4 00`.

| Stream bits | Content | Where |
|---|---|---|
| 0 to 7 | low eight bits of 97, `0x61` | byte 0, all of it |
| 8 | ninth bit of 97, zero | byte 1, bit 0 |
| 9 to 15 | low seven bits of 98 (`1100010`) | byte 1, bits 1 to 7: 98 times 2 is `0xC4` |
| 16 to 17 | top two bits of 98, zero | byte 2, bits 0 and 1 |
| 18 to 23 | unused, zero | byte 2, bits 2 to 7 |

Codes follow each other without gaps, **except** at the two places
described in section 8, where the stream jumps forward to a byte boundary.

## 5. Code space and string table

Every code stands for a string of one or more bytes.

| Codes | Block mode | Non-block mode |
|---|---|---|
| 0 to 255 | literals: the single byte of that value | the same |
| 256 | CLEAR (section 9); never a table entry | first table entry |
| first table entry | 257 | 256 |
| last possible table entry | L − 1 | L − 1 |

- The table is empty at the start. **free** starts at 257 in block mode
  and at 256 in non-block mode.
- A table entry is a pair: an existing code (its *prefix*) and one byte
  (its *suffix*). Its string is the string of the prefix followed by the
  suffix byte.
- Entries are created by the decoder as it decodes (section 6); the file
  contains none.
- The prefix of an entry is always a smaller code than the entry itself.
  Following prefixes from any entry therefore reaches a literal after a
  bounded number of steps. The string of entry c is at most c − 255 bytes
  long in block mode and c − 254 bytes long in non-block mode; the longest
  string of all is 65,280 bytes in block mode and 65,281 in non-block
  mode (entry 65,535 with M = 16).
- With M = 16 the table has at most 65,279 entries in block mode, so a
  fixed table of 65,536 slots (a 16-bit prefix and an 8-bit suffix each)
  always suffices, whatever the input.

CONFIRMED (crafted) for non-block mode: test vectors T7, T8 and T11.
CONFIRMED (real tools) for block mode.

## 6. Decoding

### 6.1 State

| Item | Initial value |
|---|---|
| n, current code width | 9 |
| free | 257 in block mode, 256 in non-block mode |
| previous code | none |
| last first byte: the first byte of the string produced by the previous code | undefined |
| group origin (section 8) | file offset 3 |
| "a code has been read" flag | false |

### 6.2 One step

Repeat the following until the stream ends.

1. **Width check.** If free is greater than or equal to 2 to the power n,
   and n is less than the width limit of section 7, then skip to the next
   group boundary (section 8) and add one to n.
2. **End check.** If fewer than n bits remain between the current
   position and the end of the file, the stream has ended normally
   (section 12). Stop.
3. **Read** one code c of n bits (section 4).
4. **CLEAR.** If block mode is on and c is 256: if no code has been read
   before in this stream, this is **corrupt input** (DECISION D3);
   otherwise perform section 9 and go back to step 1.
5. **No previous code.** If there is no previous code (this is the first
   code of the stream, or the code right after a CLEAR): c must be a
   literal. If c is 256 or greater it is **corrupt input**. Output the
   byte c; set previous code to c and last first byte to c; create no
   table entry; go back to step 1.
6. **Find the string of c.**
   - If c is less than free: the string is that of c as defined in
     section 5 (the byte itself for a literal).
   - If c equals free, and free is less than L: this is the KwKwK case of
     section 6.3. The string is the string of the previous code followed
     by the last first byte.
   - Otherwise (c greater than free; or c equal to free while the table
     is full): **corrupt input**.
7. **Output** the string, subject to the limit of section 16.
8. **New entry.** If free is less than L, create entry number free with
   prefix = the previous code and suffix = the first byte of the string
   just output, and add one to free. If free equals L the table is full
   and nothing is created (section 10).
9. Set previous code to c and last first byte to the first byte of the
   string just output. Go back to step 1.

Remarks:

- In step 6, a code is compared with free **before** the entry of step 8
  is created.
- In non-block mode code 256 is an ordinary code and step 4 never
  applies.
- The first code of a stream, and the first code after a CLEAR, create
  no entry. In block mode the first entry (257) is therefore created by
  the second code.

CONFIRMED (real tools, crafted, differential).

### 6.3 The KwKwK case

An encoder can emit the code of an entry it created in its immediately
preceding step. The decoder is always one entry behind the encoder, so
that code equals the decoder's free: it names the entry the decoder is
about to create in step 8 of this very step. This happens when the input
contains a string, the same string again, and its first byte once more
(for example a run of one byte).

The string is known nevertheless: it is the string of the previous code
followed by that string's own first byte.

Example, test vector T3: the codes 97, 257, 258, 97 in block mode.

| Code | free before | Case | Output | Entry created |
|---|---|---|---|---|
| 97 | 257 | first code, literal | `a` | none |
| 257 | 257 | equals free: string of 97 plus `a` | `aa` | 257 = (97, `a`) |
| 258 | 258 | equals free: string of 257 plus `a` | `aaa` | 258 = (257, `a`) |
| 97 | 259 | literal | `a` | 259 = (258, `a`) |

The output is seven letters `a`.

A code equal to free is valid **only** when step 8 will really create
that entry, that is while free is less than L, and only when a previous
code exists. A code greater than free is never valid.

## 7. Code width

- n starts at 9, also when M is larger. CONFIRMED (real tools).
- n grows by one in step 1 of section 6.2: when free has reached 2 to the
  power n, the next code is read with one more bit. Put differently, the
  code that creates entry 511 is the last 9-bit code, the code that
  creates entry 1023 is the last 10-bit code, and so on.
- n never shrinks, except that a CLEAR sets it back to 9 (section 9).
- The **width limit** used in step 1 is M, with one exception: when M is
  9 the width limit is 10 (section 11).
- When n equals M (and M is 10 or more) and free reaches L, n stays at M:
  the table is full (section 10).

Number of codes read at each width in a run that starts from the initial
state or from a CLEAR and is not interrupted by a CLEAR:

| Width | Block mode | Non-block mode |
|---|---|---|
| 9 | 256 (one without an entry, then entries 257 to 511) | 257 (one without an entry, then entries 256 to 511) |
| n, for n from 10 to M − 1 | 2 to the power (n − 1): 512 at width 10, 1,024 at width 11, ... | the same |
| M | unlimited | unlimited |

CONFIRMED (real tools): in every file tested the widths changed after
exactly 256, 512, 1024, ... codes; file offsets of the changes for one
file are given in T13.

## 8. Groups of eight codes and the padding

This is the part of the format that a plain LZW reader gets wrong.

### 8.1 The rule

The original encoder collected codes in a buffer of **eight codes of the
current width**, that is n bytes, and wrote the buffer when it was full.
Whenever the width was about to change, it wrote the **whole** n-byte
buffer even if fewer than eight codes were in it, and started a new
buffer. The unused part of such a buffer is padding that the decoder must
skip.

State it from the decoder's side:

- The **group origin** is a file offset. It is 3 at the start.
- A **group** is n bytes, where n is the width in force. Group boundaries
  lie at origin, origin + n, origin + 2n, and so on.
- **Skipping to the next group boundary** means: let u be the number of
  bits consumed since the group origin. If u is a multiple of 8n the
  position does not change. Otherwise the position moves forward to the
  next multiple of 8n bits from the origin. In both cases the group
  origin becomes the (new) current position, and the width may then
  change.
- The skip happens in exactly two places:
  1. in step 1 of section 6.2, when the width grows; n in the rule above
     is the width **before** it grows;
  2. after a CLEAR code has been read (section 9); n is the width at
     which the CLEAR code itself was read.
- Nothing is skipped anywhere else. In particular nothing is skipped when
  the table becomes full.

Equivalent formulation by counting: if k codes (including a CLEAR code,
if that is what triggered the skip) have been read since the group
origin, and k is not a multiple of 8, then as many n-bit slots are
skipped as it takes to make k a multiple of 8.

Consequences:

- The group origin is always a whole number of bytes from the start of
  the file, so after every skip the next code starts at bit 0 of a byte.
- If the skip moves the position to or beyond the end of the file, the
  stream has ended normally (section 12); it is not an error when the
  padding is cut short. (DECISION D4; SOURCE: the existing decoders
  behave this way.)

### 8.2 The padding bytes are arbitrary

The skipped bits are **not** always zero. (N)compress writes zeros; the
BSD encoder leaves stale bytes of the previous group in them. Example
from a real file (T14): a 10-bit CLEAR is followed by six padding bytes
`C6 F8 6D 34 DD 49` from the BSD encoder and `00 00 00 00 00 00` from
(N)compress, in otherwise identical files. The decoder must skip the
padding without looking at it. CONFIRMED (real tools).

### 8.3 Where padding actually occurs

- In **block mode**, a width growth never needs padding in a well-formed
  stream: by the table of section 7 the number of codes at each width is
  a multiple of 8. In all real files tested, every one of the width
  changes skipped zero bits. The skip rule must still be implemented for
  width growth, because it is what makes non-block files and crafted
  files decode as the existing decoders decode them.
- In block mode padding occurs **after CLEAR codes**. Observed amounts
  include 0, 10, 24, 36, 50, 60, 72 and 84 bits.
- In **non-block mode** the first width change always has padding: 257
  codes of 9 bits are 2,313 bits; the next multiple of 72 is 2,376; 63
  bits (seven empty code slots) are skipped. CONFIRMED (crafted), T11.

### 8.4 Worked example: padding after CLEAR

Test vector T5. Block mode, M = 16. The codes are 97, 98, 256 (CLEAR),
99, 100. The file is 15 bytes:

`1F 9D 90 61 C4 00 04 00 00 00 00 00 63 C8 00`

| File offset | Bytes | Content |
|---|---|---|
| 0 to 2 | `1F 9D 90` | header; group origin is offset 3 |
| 3 to 6 | `61 C4 00 04` | three 9-bit codes in stream bits 0 to 26: 97, 98, and 256 (256 starts at bit 18: its low six bits are the zero bits 2 to 7 of the byte at offset 5, its top three bits `100` are bits 0 to 2 of the byte at offset 6, giving `04`) |
| 6 to 11 | rest of `04`, then `00 00 00 00 00` | padding: 27 bits are used, the group is 72 bits, 45 bits are skipped |
| 12 to 14 | `63 C8 00` | new group origin at offset 12; two 9-bit codes 99 and 100 |

Output: `abcd`. A reader that does not skip would read the code 0 at
stream bit 27 and produce wrong output. All five full decoders agree.

### 8.5 Worked example: padding at a width change

Test vector T11. Non-block mode, M = 16. The input is the 256 byte values
0 to 255 in ascending order followed by the bytes 0, 1, 2, 3. The codes
are the 256 literals 0 to 255, then 256 (the entry for the pair 0, 1),
then 258 (the entry for the pair 2, 3).

| File offset | Length | Content |
|---|---|---|
| 0 | 3 | header `1F 9D 10` |
| 3 | 288 | 256 literal codes of 9 bits: 32 full groups of 9 bytes |
| 291 | 9 | the 33rd group: `00 01 00 00 00 00 00 00 00`. Its first 9 bits are code 256, the 257th code. Reading it creates entry 511, so free becomes 512 and the width must grow. The other 63 bits are padding. |
| 300 | 2 | `02 01`: code 258 read as a 10-bit code, 6 unused bits |

Total 302 bytes. A reader that does not skip the 63 bits produces 265
bytes instead of 260 (this is what libarchive 3.6.2 does, section 19).

For comparison, the same input in block mode (T9) has the codes 0 to 255,
257, 259. The 256 literals fill exactly 32 groups; reading the last of
them creates entry 511; the width grows at offset 291 without padding and
the file ends with the three bytes `01 0D 04`: two 10-bit codes 257 and
259 and four unused bits.

## 9. The CLEAR code

Block mode only. When step 4 of section 6.2 reads code 256:

1. Skip to the next group boundary (section 8), using the width at which
   the CLEAR was read.
2. Set n to 9.
3. Empty the table: set free to 257.
4. Set previous code to none.

No output is produced. The next code is then handled by step 4 or step 5
of section 6.2: it must be a literal or another CLEAR.

Facts:

- Real encoders emit CLEAR only after the table is full and the
  compression ratio has dropped, so only at width M and not within the
  first 10,000 input bytes. CONFIRMED (real tools), DOCUMENTED. A decoder
  must nevertheless accept CLEAR at any width. CONFIRMED (crafted), T5.
- Two CLEAR codes in a row are valid; each one causes its own skip.
  CONFIRMED (crafted), T6: `1F 9D 90`, then the 9-byte group
  `61 00 02 00 00 00 00 00 00` (codes 97 and 256), then the 9-byte group
  `00 01 00 00 00 00 00 00 00` (code 256), then `62 00` (code 98);
  output `ab`.
- A CLEAR as the last code of the file is valid. T6b.
- Old entries need not be erased from memory when the table is emptied:
  after free is reset, step 6 never reads an entry numbered free or
  higher.
- Several existing decoders implement the reset differently (they set
  free to 256, keep the previous code, and let the next code create a
  meaningless entry 256 that can never be referenced). The result is the
  same as the rules above for every input. CONFIRMED (differential).

## 10. The full table

When free equals L the table is full.

- No more entries are created (step 8).
- Codes keep being read at width M and are decoded with the frozen
  table. This continues to the end of the file or, in block mode, until
  a CLEAR.
- A code equal to free cannot occur at width M when M is 10 or more,
  because L does not fit in M bits.
- No padding is skipped when the table becomes full.

CONFIRMED (real tools): T13 with M = 10 reads 3,519 codes, of which more
than 2,700 after the table filled, without a CLEAR.

## 11. Maximum width 9: two incompatible readings

Files with M = 9 exist in two dialects that cannot be told apart from the
header.

| | Dialect A (historical) | Dialect B |
|---|---|---|
| Written by | `compress` 4.0 and the BSD `compress` (`-b 9`) | (N)compress since the 4.2.4 series (`-b 9`) |
| Table limit | 512 | 512 |
| After entry 511 is created | the width grows to 10 like in any other file, with the usual skip; the table stays frozen at 512 entries; all later codes are 10 bits wide | the width stays 9 |
| After a CLEAR | width 9 again, growing to 10 again at entry 511 | width 9 |
| Decoded correctly by | GNU gzip 1.12, (N)compress 4.2.4.6 (decoder), BusyBox, libarchive, zlib `gun` | current (N)compress only |

Observations:

- (N)compress 4.2.4.6 writes dialect B and reads dialect A: it cannot
  read its own `-b 9` output once that exceeds 256 codes. CONFIRMED
  (real tools), T10.
- Reading a dialect A file as dialect B gave wrong output of the wrong
  length **without any error** in the test. Reading a dialect B file as
  dialect A failed with a code beyond free in the tests.
- Files that end before entry 511 is created (at most 256 codes) are the
  same in both dialects.

Rules (DECISION D5):

1. A file with M = 9 is decoded as dialect A: the width limit of section
   7 is 10 while L stays 512. In this dialect, and only here, a 10-bit
   code can be 512 or larger; by step 6 of section 6.2 such a code is
   **corrupt input**, because the table is full.
2. If, and only if, that decoding ends in **corrupt input**, the decoder
   may decode the file once more from the start as dialect B (width
   limit 9) and return that result if it completes without error. This
   fallback is optional. The opposite order is not permitted.

## 12. End of the stream

- The stream ends when fewer than n bits remain (step 2 of section 6.2)
  or when a skip reaches the end of the file (section 8.1).
- The last byte normally contains up to 7 unused high bits; encoders
  write them as zero. When the codes end exactly on a byte boundary
  there are none.
- Left-over bits that do not form a whole code are ignored without
  error, whatever their value. (DECISION D6.) Example T15: the eight
  9-bit codes of `abcdefgh` end on a byte boundary; one appended byte
  `FF` is 8 bits, less than a code, and is ignored. (N)compress, GNU
  gzip, BusyBox and libarchive agree; the BSD decoders differ, section
  19.
- **Trailing garbage cannot be detected.** Any bytes after the genuine
  data that amount to at least n bits are read as codes. Example T16:
  the file for `ab`, `1F 9D 90 61 C4 00`, has 6 unused bits; appending
  one byte `00` makes 14 bits, which is one more 9-bit code 0, and every
  decoder outputs `ab` followed by a zero byte. Appending `FF FF` instead
  gives the code 448, which is beyond free: corrupt input.
- When decoding fails with corrupt input, the bytes produced before the
  failure are not returned. (DECISION D7. (N)compress and GNU gzip write
  the partial output and then report failure.)

## 13. The empty file

Compressing zero bytes gives one of two results, depending on the tool:

| Tool | Output for empty input |
|---|---|
| (N)compress | 3 bytes: the header `1F 9D 90` and no codes |
| BSD `compress` (macOS) | a zero-length file |

CONFIRMED (real tools).

Rules:

- A header with no code stream, or with fewer than 9 bits after the
  header, decodes to zero bytes of output and is valid. (N)compress, GNU
  gzip, BusyBox and libarchive agree. T1.
- A zero-length file is **not this format** (section 3 rule 1): there is
  no magic number to recognise. It is not an error of this decoder; it is
  simply not offered to it.

## 14. Detection

The magic number is only two bytes, so a detector must use everything
else that is cheap to check. A buffer is accepted as a `compress` file
when all of the following hold (DECISION D8):

1. It is at least 3 bytes long and starts with `1F 9D`.
2. Bits 5 and 6 of the flags byte are zero.
3. M is 9 to 16.
4. If at least 9 bits follow the header, the first code is a literal:
   bit 0 of the byte at offset 4 is zero.
5. If at least 18 bits follow the header, the second 9-bit code (bits 1
   to 7 of offset 4 as its low seven bits, bits 0 and 1 of offset 5 as
   its top two) is at most 257 in block mode and at most 256 in
   non-block mode.

Rules 4 and 5 are exactly what section 6.2 demands of the first two
codes, so no valid file is rejected. A detector may apply the rules of
section 6.2 to more codes, provided it never rejects a file the decoder
would accept.

## 15. Names

The file stores no name. The name of the payload is derived from the
name of the compressed file.

| Compressed name | Payload name | Evidence |
|---|---|---|
| `name.Z` | `name` | DOCUMENTED: `compress` appends `.Z`, `uncompress` removes it. The suffix is an upper-case Z. |
| `name.tar.Z` | `name.tar`, a tar archive | DOCUMENTED |
| `name.taz` | `name.tar` | DOCUMENTED: the gzip manual names `.taz` as the shorthand for `.tar.Z`, for file systems with short names |
| `name.z` (lower case) | ambiguous | DOCUMENTED: lower-case `.z` was the suffix of `pack` and of early gzip; on case-insensitive media a `.Z` file can appear as `.z`. Decide by the magic number, not by the suffix. |
| `name.compressed` | a tar archive of the file or folder `name` | DOCUMENTED (archive notes): the NeXTSTEP Workspace Manager's Compress command produced a file with this suffix, "archived and compressed"; INFERRED: the content is a tar stream passed through `compress`, with the same `1F 9D` header. See section 20. |
| `Name.pkg/Name.tar.Z` | `Name.tar` | DOCUMENTED: a NeXT Installer package is a directory `Name.pkg` holding the payload as `Name.tar.Z` beside `Name.bom`, `Name.info` and related files; the Installer release notes call the `.tar.Z` file the package archive. |

Guidance:

- Recognition is always by content (section 14). Suffixes only choose
  the payload name.
- Suffix comparison should ignore case.
- For `.compressed` the payload is a tar archive but the name left after
  removing the suffix has no `.tar`. A reader that recognises tar by
  content needs nothing more; a reader that wants a descriptive name may
  append `.tar`. (DECISION D9: either is acceptable.)
- A payload name that would be empty after removing the suffix is
  replaced by a fixed placeholder chosen by the caller.

## 16. Safety requirements

1. **Header.** Apply section 3 before anything else. Never index a table
   sized by M before M has been checked to be 9 to 16.
2. **Fixed memory for the table.** The table has at most 65,536 slots
   regardless of input size. Its size must not depend on any other value
   read from the file.
3. **Every code is checked against free** as in step 6 of section 6.2,
   before the table is consulted:
   - a code greater than free is corrupt input;
   - a code equal to free is accepted only when there is a previous code
     and the table is not full;
   - with no previous code, only a literal is accepted.
   With these checks no table slot is read before it was written, and
   stale slots left over from before a CLEAR are never read.
4. **String expansion terminates.** Because each entry's prefix is a
   smaller code, following prefixes takes at most 65,280 steps. An
   implementation that expands a string into a temporary buffer needs
   65,281 bytes at most (section 5).
5. **Output cap.** The format expands enormously: one 16-bit code can
   stand for 65,280 bytes, so a file of repeated bytes approaches a
   ratio of 32,000 to 1 and a 1 MiB file can demand more than 30 GiB.
   Before each string is appended, check that the total output would not
   exceed the caller's limit (`MAX_DECOMPRESSED_SIZE` in this
   repository); if it would, stop with **output limit exceeded**. Do not
   reserve output memory in proportion to that limit in advance; grow
   the output as it is produced.
6. **Bit reader.** Never read beyond the end of the file. A code is read
   only when all n of its bits lie inside the file (step 2). The
   position arithmetic of section 8 must not overflow for a file of any
   permitted size; positions counted in bits need 64-bit arithmetic for
   files of 512 MiB and more on 32-bit targets.
7. **Skips.** A skip that passes the end of the file ends the stream; it
   must not wrap or read.
8. **No recursion** on input-controlled depth: string expansion and the
   handling of consecutive CLEAR codes are loops.
9. **Progress.** Every step of section 6.2 consumes at least 9 bits, so
   the number of steps is bounded by the file size.
10. **No integrity claim.** A successful decode is not evidence that the
    file is undamaged (section 2).

## 17. Test vectors

Each vector gives the whole file in hexadecimal. "Agreed" means the five
full decoders of section 1.2 produced the stated output, except that the
two BSD decoders refuse every file with M below 12 and every header-only
file (section 19) and are then not counted.

### 17.1 Real encoder output, small

Produced by the BSD `compress` with `-b 16`; (N)compress output is
identical. With `-b 9`, `-b 10` or `-b 12` only the third byte changes
(`89`, `8A`, `8C`).

| Id | Input | File | Codes |
|---|---|---|---|
| T1 | empty | `1F 9D 90` ((N)compress only; the BSD tool writes nothing) | none |
| T2 | `a` | `1F 9D 90 61 00` | 97 |
| T2b | `ab` | `1F 9D 90 61 C4 00` | 97, 98 |
| T3 | `aaaaaaa` (7 bytes) | `1F 9D 90 61 02 0A 0C 03` | 97, 257, 258, 97 (section 6.3) |
| T3b | `abcdefgh` | `1F 9D 90 61 C4 8C 21 53 C6 CC 19 34` | 97 to 104; exactly one full group, no unused bits |
| T4 | `TOBEORNOTTOBEORTOBEORNOT` (24 bytes) | `1F 9D 90 54 9E 08 29 F2 44 8A 93 27 54 02 0E 2C A8 90 A0 41 84` | 84, 79, 66, 69, 79, 82, 78, 79, 84, 257, 259, 261, 266, 260, 262, 264 |

T4 step by step: the nine literals create the entries 257 `TO`, 258
`OB`, 259 `BE`, 260 `EO`, 261 `OR`, 262 `RN`, 263 `NO`, 264 `OT`. Then
257 outputs `TO` and creates 265 `TT`; 259 outputs `BE` and creates 266
`TOB`; 261 outputs `OR` and creates 267 `BEO`; 266 outputs `TOB` and
creates 268 `ORT`; 260 outputs `EO` and creates 269 `TOBE`; 262 outputs
`RN` and creates 270 `EOR`; 264 outputs `OT` and creates 271 `RNO`.

### 17.2 Crafted, valid

| Id | File | Expected output | Purpose |
|---|---|---|---|
| T5 | `1F 9D 90 61 C4 00 04 00 00 00 00 00 63 C8 00` | `abcd` | CLEAR at width 9 with 45 bits of padding (section 8.4) |
| T6 | `1F 9D 90 61 00 02 00 00 00 00 00 00 00 01 00 00 00 00 00 00 00 62 00` | `ab` | two CLEAR codes in a row |
| T6b | `1F 9D 90 61 C4 00 04 00 00 00 00 00` | `ab` | CLEAR as the last code |
| T7 | `1F 9D 10 61 00 06 0C 03` | `aaaaaaa` | non-block mode; codes 97, 256, 257, 97; code 256 is a table entry and is used in the KwKwK case |
| T8 | `1F 9D 10 61 C4 00 14 28 06` | `abababab` | non-block mode; codes 97, 98, 256, 258, 98 |
| T8b | `1F 9D 10 54 9E 08 29 F2 44 8A 93 27 54 00 0A 24 98 70 60 C1 83` | the 24 bytes of T4 | non-block mode; codes as T4 with every table code one lower: 256, 258, 260, 265, 259, 261, 263 |
| T15 | `1F 9D 90 61 C4 8C 21 53 C6 CC 19 34 FF` | `abcdefgh` | 8 left-over bits are ignored |
| T16 | `1F 9D 90 61 C4 00 00` | `ab` and one zero byte (3 bytes) | trailing byte is read as a code |
| T17 | `1F 9D 90 61` | empty | 8 bits after the header: no code |

All agreed (T15: see section 12).

### 17.3 Crafted, invalid

Each must be rejected with the stated result.

| Id | File | Result | Reason |
|---|---|---|---|
| E1 | `1F 9D 90 61 04 02` | corrupt input | codes 97, 258: 258 is greater than free (257) |
| E2 | `1F 9D 90 01 01` | corrupt input | first code 257 is not a literal |
| E3 | `1F 9D 90 2C C3 00` | corrupt input | first code 300 is not a literal |
| E4 | `1F 9D 90 00 01 00 00 00 00 00 00 00 61 00` | corrupt input | first code is CLEAR (D3) |
| E5 | `1F 9D 90 61 C4 00 04 00 00 00 00 00 01 01` | corrupt input | 97, 98, CLEAR, 257: the code after a CLEAR is not a literal |
| E6 | `1F 9D 10 00 01` | corrupt input | non-block mode, first code 256 |
| E7 | `1F 9D 10 61 02 02` | corrupt input | non-block mode, codes 97, 257: greater than free (256) |
| E8 | `1F 9D 90 61 C4 00 FF FF` | corrupt input | T2b with two garbage bytes: third code 448 |
| E9 | `1F 9D 88 61 C4 00` | not this format | M = 8 (this header is what the BSD encoder writes for `-b 8`) |
| E10 | `1F 9D 80 61 C4 00` | not this format | M = 0 |
| E11 | `1F 9D 91 61 C4 00` | not this format | M = 17 |
| E12 | `1F 9D B0 61 C4 00` | not this format | reserved bit `0x20` |
| E13 | `1F 9D D0 61 C4 00` | not this format | reserved bit `0x40` |
| E14 | `1F 9D` | not this format | too short |
| E15 | zero-length file | not this format | no header |

Under the detection rules of section 14, E2, E3, E4 and E6 are already
refused by the detector (rule 4) and E1 and E7 by rule 5; a decoder
called directly reports corrupt input.

What the existing decoders do with these files is recorded in section
19; GNU gzip and (N)compress report an error for E1 to E8 and E11.

### 17.4 Width change: ascending bytes

Input **ASC**: the 256 byte values `00` to `FF` in ascending order
followed by `00 01 02 03`; 260 bytes; SHA-256
`e834297d45be0ae3d42422c3071c57590c65759366d7c2e1871d3d4a15f5759f`.

**T9**: ASC through `compress -b 16` (both encoders, identical). 294
bytes, SHA-256
`1f31f806ae85ec4909d692e7c95df41ada243ec7e18010867f1b9b1c720c3dc4`.
Complete file, 24 bytes per line, offset of the first byte on the left:

```
  0  1f 9d 90 00 02 08 18 40 a0 80 81 03 08 12 28 58 c0 a0 81 83 07 10 22 48
 24  98 40 a1 82 85 0b 18 32 68 d8 c0 a1 83 87 0f 20 42 88 18 41 a2 84 89 13
 48  28 52 a8 58 c1 a2 85 8b 17 30 62 c8 98 41 a3 86 8d 1b 38 72 e8 d8 c1 a3
 72  87 8f 1f 40 82 08 19 42 a4 88 91 23 48 92 28 59 c2 a4 89 93 27 50 a2 48
 96  99 42 a5 8a 95 2b 58 b2 68 d9 c2 a5 8b 97 2f 60 c2 88 19 43 a6 8c 99 33
120  68 d2 a8 59 c3 a6 8d 9b 37 70 e2 c8 99 43 a7 8e 9d 3b 78 f2 e8 d9 c3 a7
144  8f 9f 3f 80 02 09 1a 44 a8 90 a1 43 88 12 29 5a c4 a8 91 a3 47 90 22 49
168  9a 44 a9 92 a5 4b 98 32 69 da c4 a9 93 a7 4f a0 42 89 1a 45 aa 94 a9 53
192  a8 52 a9 5a c5 aa 95 ab 57 b0 62 c9 9a 45 ab 96 ad 5b b8 72 e9 da c5 ab
216  97 af 5f c0 82 09 1b 46 ac 98 b1 63 c8 92 29 5b c6 ac 99 b3 67 d0 a2 49
240  9b 46 ad 9a b5 6b d8 b2 69 db c6 ad 9b b7 6f e0 c2 89 1b 47 ae 9c b9 73
264  e8 d2 a9 5b c7 ae 9d bb 77 f0 e2 c9 9b 47 af 9e bd 7b f8 f2 e9 db c7 af
288  9f bf 7f 01 0d 04
```

Offsets 3 to 290 are the 256 literal codes (9 bits each, 32 groups);
offsets 291 to 293 are the 10-bit codes 257 and 259.

Variants of T9, all 294 bytes, all decoding to ASC:

| Id | Difference from T9 | SHA-256 of the file |
|---|---|---|
| T9-12 | byte 2 is `8C` (`-b 12`) | `cc8550d68b63edf7fd4921d515af65850b4f594568321d3766c54a1d8efdf40d` |
| T9-10 | byte 2 is `8A` (`-b 10`) | `e70428b339bb9f6e1fc7de1d70978cf1d287deddd5ae03da4e3e66840cc05c1d` |
| T10-A | byte 2 is `89` (BSD `compress -b 9`, dialect A) | `0fd8851f0f716782106fb535694c2ddbf3a36b8957471b5120b4688814dd2ea6` |
| T10-B | byte 2 is `89` and the last three bytes are `01 07 02` instead of `01 0D 04` ((N)compress 4.2.4.6 `-b 9`, dialect B: 257 and 259 as 9-bit codes) | `e36bb692a086d7bae926bb9818a6ec9cf3d08ff058ff2b44a6491ddacbf82ba9` |

T10-B decoded as dialect A fails: after the skip-free width change the
10-bit code read at offset 291 is 769 (corrupt input, table full). With
the optional fallback of section 11 it decodes to ASC. T10-A decoded as
dialect B would give wrong output, which is why the order of section 11
is fixed.

**T11**: ASC in non-block mode, crafted. 302 bytes: byte 2 is `10`;
offsets 3 to 290 are identical to T9; offsets 291 to 301 are
`00 01 00 00 00 00 00 00 00 02 01` (section 8.5). SHA-256
`c633a4ae7f12e5ec169a67118eaebf4bb36f58eb7cfd223af32d4b77cb7ed350`.
Agreed by the five full decoders. The same file with byte 2 = `0C`
(M = 12) has SHA-256
`b281a175ed36516621e94c5a1beaa10b437355a6da5e38f687e12f3581f64e51` and
the same output.

### 17.5 Larger files from real encoders

These can be regenerated with the commands given; the inputs are
deterministic.

Input **SEQ**: the output of `seq 1 2000` (the decimal numbers 1 to 2000,
one per line, newline-terminated); 8,893 bytes; SHA-256
`6251e5743b6fd6a7d606130bdf7c15077ce85ebd3a0fdee284d15a46df199e38`.

Input **LCG**: the output of `seq 1 3000` followed by 3,000 lines, each
the decimal value of s, where s starts at 1 and is replaced before each
line by (s × 69069 + 1) modulo 4,294,967,296. The first of these lines
is `69070`, the last two are `2675762664` and `4103660233`. 46,120
bytes; SHA-256
`ecc284ad38f97e4582954a0c67ffc326bc1ccf46c5e44f287513c84b59f54007`.

**T13**, SEQ compressed. Both encoders give identical files.

| Option | Size | SHA-256 of the `.Z` file | Codes | Width changes at file offset |
|---|---|---|---|---|
| `-b 10` | 4,370 | `fe5d10578546a7318f948a05998c889fa389678a54b1ed80704b54f9463dcbc9` | 3,519 | 291 (to 10); table full from the 768th code on |
| `-b 12` | 4,270 | `201bb0822d27ae5841259112fd39e888d2f868c44b3042bdfa42e0f3c3c0b068` | 3,079 | 291, 931 (to 11), 2,339 (to 12) |
| `-b 16` | 4,270 | `1bb2f1945177f8b8f00812ce86273ecef076499693f5e8efbf39a01f34a7750b` | 3,079 | 291, 931, 2,339 |
| `-b 9`, BSD encoder (dialect A) | 4,806 | `4536546422af96fce0f6932bc12c69ee003e6baf44de12a303491baddb09e9f0` | 3,868 | 291 (to 10) |
| `-b 9`, (N)compress 4.2.4.6 (dialect B) | 4,347 | `54cca98dc6977841c32f296258fe2f696109a303260cfe5600b99e15800da27a` | | none; as dialect A it fails at the 258th code (911) |

The offsets follow from section 7: 3 + 256 × 9 / 8 = 291; 291 + 512 × 10
/ 8 = 931; 931 + 1024 × 11 / 8 = 2,339.

**T14**, LCG compressed: files with CLEAR codes.

| Encoder and option | Size | SHA-256 of the `.Z` file | CLEAR codes |
|---|---|---|---|
| BSD `-b 10` | 23,211 | `a53b917f0d43aba19dfd7b565ba9ec15d151b34c5c1c25271ed2dc53e937db5f` | one; it is the 11,707th 10-bit code of the group run that starts at offset 291; it ends 6 bits into the byte at offset 14,924; 50 bits are skipped; the next code is a 9-bit code at offset 14,931; the width grows to 10 again at offset 15,219 |
| (N)compress `-b 10` | 23,211 | `c12ba8e2b54e0097a8ae5e344dd262f4a2481056311a567b4989504da54939c8` | as above; the file differs from the BSD one only in the six padding bytes at offsets 14,925 to 14,930 (section 8.2) |
| BSD `-b 12` | 22,308 | `eba8aec8219f2b857f540cd24157ced3a90aed5ad6ecf855bba527b6757f3afc` | one, the 5,259th 12-bit code after offset 2,339; 60 bits skipped; next group at offset 10,235 |
| (N)compress `-b 12` | 22,296 | `fd6790c458afac48821359090b818f40af9a66a91a450884636d17af143c59b7` | one, the 5,258th 12-bit code; 72 bits skipped; next group at offset 10,235 (the two encoders choose the moment differently) |
| either, `-b 16` | 22,366 | `1b00f63ad4df9cf90d0795b784407a4ed08d10a0ca5a77ac61d6a63e7ca7ea6c` | none; widths grow at offsets 291, 931, 2,339, 5,411 and 12,067 |
| BSD `-b 9` (dialect A) | 27,036 | `7d02e01d5f8ec92dc27bf64f05559cab0084983f8259409ee727a9c76c422618` | two, both read as 10-bit codes; 10 and 50 bits skipped; new groups at offsets 10,911 and 23,249; after each the width is 9 and grows to 10 again 288 bytes later |

Every file of T13 and T14 except the two dialect B files decodes to its
input under the rules of this document.

A fixture set for the repository can be generated from SEQ and LCG with
either encoder; all content is generated.

## 18. Decisions

| Id | Section | Decision |
|---|---|---|
| D1 | 3 | Reserved flag bits `0x20` and `0x40` set: not this format. No known writer sets them; refusing strengthens a two-byte magic. |
| D2 | 3 | M outside 9 to 16: not this format. 17 and above are refused by every decoder; below 9 is meaningless (codes are at least 9 bits) even though some decoders tolerate it. |
| D3 | 6.2 | A CLEAR as the very first code is corrupt input, as in (N)compress, GNU gzip and `gun`. A CLEAR directly after a CLEAR is valid. |
| D4 | 8.1 | Padding cut short by the end of the file ends the stream without error. |
| D5 | 11 | M = 9 is read as dialect A; dialect B only as an optional second attempt after corrupt input. |
| D6 | 12 | Left-over bits fewer than one code are ignored. |
| D7 | 12 | No partial output is returned when decoding fails. |
| D8 | 14 | Detection checks the header fields and the first two codes. |
| D9 | 15 | Naming of the payload of `.compressed` files is left to the caller. |

No decision changes how a file written by a real encoder decodes, with
the stated exception of dialect B files of more than 256 codes when the
optional fallback is not implemented.

## 19. Observed implementation gaps

Not to be imitated. Results are from the crafted vectors of section 17.

| Implementation | Behaviour |
|---|---|
| BSD `uncompress` (macOS) | Refuses every file with M below 12 and every header-only file. Does not check that the first code is a literal: E2 yields one byte `01`, E3 yields two bytes, E4 yields nine bytes of garbage, E6 one zero byte, all with success status. Consumes a partial trailing code: T15 yields nine bytes. Its encoder accepts `-b 8` and writes header byte `88`. Writes a zero-length file for empty input and cannot read that file back. |
| Apple `gzip` (BSD gzip) | Same refusals, but silently: empty output and success status for M below 12 and for most corrupt files (E1 to E3, E5 to E8). Accepts CLEAR as first code (E4 yields `a`). Consumes a partial trailing code (T15). |
| (N)compress 4.2.4.6 | Decoder accepts M below 9 (E9 and E10 decode to `ab`) and reserved bits (with a warning status). Writes partial output before reporting corrupt input. Its `-b 9` output is dialect B, which its own decoder cannot read. Its compatibility option for non-block output writes a header without the block bit but numbers entries from 257 as in block mode, so every decoder, itself included, misreads the result. Accepts a 10-bit code 512 in a dialect A file as a KwKwK case although the table is full. |
| GNU gzip 1.12 | As (N)compress for M below 9, reserved bits (warning), partial output and code 512. |
| BusyBox 1.35 `zcat` | Reports success, with empty output, for every corrupt file tested (E1 to E8) and for a two-byte file. |
| libarchive 3.6.2 `bsdcat` | Does not skip padding in all cases: T5 fails, T6 yields 13 bytes, T6b yields `ab` and two zero bytes, T11 yields 265 bytes. Real encoder output, including the files of T14 with CLEAR codes, decodes correctly. Passes foreign data through unchanged. |
| zlib `gun` (source only) | Reads M = 9 as 10 and then also lets the table grow to 1,024 entries, which accepts codes no dialect A encoder can produce. Treats a trailing partial code that spans a byte boundary as an error. |

## 20. Open points

1. **`.compressed`.** That NeXTSTEP `.compressed` files are a tar stream
   passed through `compress` with the ordinary `1F 9D` header is
   inferred from secondary descriptions; no specimen was examined. If a
   specimen shows a different wrapper, section 15 needs a correction;
   the decoder is unaffected.
2. **Genuine non-block files.** No encoder that writes correct non-block
   output was available ((N)compress 4.2.4.6's option is broken, section
   19). Non-block behaviour rests on the source of the original
   implementation and on five decoders agreeing on crafted files.
3. **Dialect B in the wild.** How many M = 9 files of dialect B exist is
   unknown; they can only come from recent (N)compress with an explicit
   `-b 9`. Current (N)compress 5.x was read in source but not run.
4. **Headerless files** of the first `compress` versions are not
   handled.
5. **M below 9.** The BSD encoder can be made to write M = 8 (E9). Such
   files hold only 9-bit literal codes and are refused by D2 and by the
   BSD decoder itself; no real specimen is known.
