# RAR 2.9 filters (unpack versions 29 and 36)

Format specification for clean-room implementation. This document
describes the **filters** of the compression method used by RAR 2.9, 3.x
and 4.x (file header unpack version 29 or 36): small programs for a
virtual machine, sent inside the compressed stream, each of which
post-processes one block of the decoded output (undoing a transformation
the archiver applied before compressing: x86 call translation, delta
coding of tabular data, picture and sound prediction).

It covers three things, in three parts:

- **Part I** (sections 3 to 8): how a filter is transmitted in the LZ
  stream and in the PPMd stream, how programs are numbered and reused,
  how filters queue up against output positions and when they run.
- **Part II** (sections 9 to 14): the six *standard programs* that RAR
  emits, how each is recognised, and its exact effect on a block as a
  mathematical transformation.
- **Part III** (sections 15 to 18): the complete virtual machine, so that
  a program other than the standard ones can also be run.

It does not describe the archive container (`rar4-container.md`), the LZ
layer (`rar29-lz.md`), the PPMd layer (`rar29-ppmd.md`), RAR5
(`rar5-container.md`, `rar5-compression.md`) or encryption.

Contents: 1 how to read this document; 2 where filters sit and what is
assumed of the other layers; 3 carrying a filter record; 4 numbers; 5 the
record; 6 the program store; 7 blocks, the queue and running a filter; 8
members and solid groups; 9 standard programs: recognition and common
rules; 10 x86 call translation; 11 Itanium; 12 delta; 13 RGB; 14 audio;
15 machine state; 16 program encoding; 17 instructions; 18 one
invocation; 19 worked examples; 20 test vectors; 21 safety requirements;
22 observed implementation gaps; 23 decisions; 24 open questions; 25
evidence.

## 1. How to read this document

### 1.1 Provenance

- This is a **prose specification**. It was derived by studying the
  behaviour of an existing open decoder (the RAR 3 filter and virtual
  machine support of The Unarchiver's XADMaster library, LGPL, revision
  `7cb9ee0a` of `github.com/MacPaw/XADMaster`; called the *reference
  implementation*), by black-box testing of the extraction tool built
  from that library (`unar` 1.10.7) and of `bsdtar` 3.5.3 with
  purpose-built archives, by measurements on real archives, and by
  examining archives written for the purpose with the vendor's own
  command-line RAR 4.20, used only as an archive creator.
- The official `unrar` source was **not** consulted; its licence forbids
  using it to recreate the algorithm. Neither was any other RAR decoder
  source.
- The work is a **clean-room process**. The author of this document
  could read the reference implementation; the implementation is written
  **from this document only**. The document therefore contains no program
  code and no pseudo-code, only prose, tables of constants, worked
  examples and test vectors. The byte strings of the six standard
  programs appear in section 20.1 as data: they are part of every
  archive that uses a filter, and a decoder needs them only to compute
  the fingerprints of section 9.1 and as test input.
- Where the reference implementation is wrong or unsafe the document
  says so (section 22) and specifies the correct behaviour instead.
  Being compatible with the reference implementation is not a goal;
  reading genuine archives correctly and rejecting hostile input safely
  is.

### 1.2 Evidence labels

| Label | Meaning |
|---|---|
| CONFIRMED (real data) | The rule was exercised when decoding real archives (section 25), and every decoded file matched the CRC-32 stored in its header. |
| CONFIRMED (crafted) | Checked by giving `unar` (and, where it applies, `bsdtar`) a purpose-built archive and comparing the output with the output predicted by this document. |
| REFERENCE-ONLY | Stated by the reference implementation; not separately observable. |
| INFERRED | Deduced; not confirmed. |
| DECISION | A choice made by this specification, not a fact about the format (section 23). |

Defaults, for rules that carry no label of their own:

- sections 3 to 8, section 9.1 and sections 10 to 14: CONFIRMED (real
  data);
- sections 15 to 18: CONFIRMED (crafted). The parts of the machine that
  the six standard programs use are in addition CONFIRMED (real data),
  because running those programs on the machine of Part III reproduces
  every real file (section 25); section 17.6 lists what they use.

### 1.3 Terms and numbers

| Term | Meaning |
|---|---|
| member | One file stored in the archive (after the container layer has joined the pieces of a split file). |
| solid group | A maximal run of members that share one decoder state. A member of a non-solid archive is a group of one. |
| raw output | The bytes produced by the LZ and PPMd layers, before any filter. These are the bytes that enter the sliding window. |
| delivered output | The bytes of the member as handed to the caller: raw output with every filtered block replaced by the filter's result. |
| group position | The number of raw output bytes produced in the solid group so far. A 64-bit count. The first byte of the group has position 0. |
| **G** | The group position at the moment under discussion. |
| **M** | The group position at which the current member starts. |
| record | One filter transmission (section 5). |
| program | The byte string that encodes virtual machine instructions (section 16). |
| block | The run of raw output bytes one filter applies to: start position **s**, length **L**. |
| file position **F** | s − M: the offset of the block's first byte inside the member. |
| corrupt input | The decoder's error result for the member. |

Numbers are decimal unless prefixed `0x`. Byte strings are written as
hexadecimal digits in code style. Ranges written `a..b` include both
ends. All arithmetic called "32-bit" is modulo 2^32 on unsigned values;
"signed" interpretations are two's complement. "Little-endian 32-bit
value at x" means byte x is the least significant.

## 2. Where filters sit, and what is assumed of the other layers

A filter never changes how the stream is decoded. It only changes what
is delivered:

- The LZ and PPMd layers produce raw output and keep it in the sliding
  window. **Matches always copy raw output**, also from a region that a
  filter has transformed or will transform. (CONFIRMED (crafted).)
- The filter layer sits between the raw output and the caller. Raw bytes
  outside every block are delivered unchanged; a block is delivered as
  the result of running its filter on the block's raw bytes. A filter
  never changes the number of bytes (section 18.3).

Interfaces assumed (stated here so that the documents can be checked
against each other):

1. `rar29-lz.md`: in an LZ block, **main-code symbol 257** announces a
   filter record. What follows it in the bit stream is defined in
   section 3.1 of this document; after the record the LZ layer continues
   with the next main-code symbol. The record produces no raw output.
2. `rar29-ppmd.md`: in a PPMd block, the escape symbol followed by the
   **control code 3** announces a filter record. What follows is defined
   in section 3.2; after the record the PPMd layer continues with the
   next symbol.
3. Both layers tell the filter layer the group position G at which a
   record is met (the count of raw bytes produced by everything before
   the record), and hand it every raw byte in order.
4. The LZ layer decides where a member's raw output ends (the member's
   unpacked size and the end-of-member marker are its business) and
   tells the filter layer "the member ends at group position E" and "a
   new member starts" (then M = E). This document assumes the raw output
   of a member is exactly the positions M..E − 1. In the words of
   `rar29-lz.md`: its "LZ position" is G here, its "member start" is M,
   its "filter block start S" is s.
5. The window holds at least the most recent 0x3C000 (245,760) raw
   bytes, so the raw bytes of a complete block can be read back from it.
   (The window of this method is 4 MiB.)
6. `rar4-container.md`: which members form a solid group, and the
   CRC-32 check of the delivered output of each member. A member whose
   stream ends in corrupt input poisons the rest of its group, as for
   every other part of the decoder.

The state of the filter layer (sections 6 and 7) belongs to the solid
group: it is created empty when a group starts and is carried from
member to member inside the group.

`rar29-lz.md` (its section 11) describes the same queue from the LZ
side, and the two statements are meant to be identical. That document
gives the rule of precedence should they ever differ: this document
prevails on everything about a record, a block's start and length, the
order check and the queue; `rar29-lz.md` prevails on what a member is,
where it starts and ends, and on the order of events in the decoding
loop. Section 24 (item 11) lists the points on which the two texts were
compared.

# Part I: transmission and scheduling

## 3. Carrying a filter record

A record is a sequence of bytes: one **flags byte**, a **payload size**
of zero, one or two bytes, and the **payload**.

The low three bits of the flags byte give the payload size n:

| flags & 7 | Size field that follows | n |
|--:|---|---|
| 0..5 | none | (flags & 7) + 1, that is 1..6 |
| 6 | one byte b | b + 7, that is 7..262 |
| 7 | two bytes, high byte first | the 16-bit value, 0..65535 |

Any of the three forms may be used for any size it can express; the
form does not have to be the shortest one (CONFIRMED (crafted)). A
payload size of 0 is corrupt input (it cannot hold the mandatory offset
field; `unar` fails on it, CONFIRMED (crafted)).

The upper five bits of the flags byte are described in section 5.

### 3.1 In an LZ block

After main-code symbol 257 the bytes of the record follow directly in
the LZ bit stream, **each as an 8-bit field, most significant bit
first, not aligned to a byte boundary**: the flags byte, then the size
byte or the two size bytes (equivalently one 16-bit field), then the n
payload bytes. Reading past the end of the member's packed data is
corrupt input, as everywhere in the LZ layer.

### 3.2 In a PPMd block

After the escape symbol and the control code 3, the bytes of the record
are the next symbols decoded by the PPMd model, one symbol per byte, in
the same order: flags byte, size byte(s), n payload bytes. These symbols
are ordinary model symbols (they update the model like any other) but
they are **not** interpreted: a byte equal to the escape character is
just a record byte here. A PPMd decoding error while reading them is
corrupt input.

(CONFIRMED (real data): 91 records carried this way; CONFIRMED (crafted)
for all three size forms.)

## 4. Numbers

Records and programs store most values in a variable-length form,
called a **number** in this document. A number is read from a bit
string, most significant bit first, and yields a 32-bit value:

| First 2 bits | Then | Value |
|---|---|---|
| `00` | 4 bits v | v (0..15) |
| `01` | 8 bits v; **if v is less than 16, 4 more bits w** | v when v is 16 or more (16..255); otherwise 0xFFFFFF00 + 16·v + w (0xFFFFFF00..0xFFFFFFFF) |
| `10` | 16 bits v | v (0..65535) |
| `11` | 32 bits v | v |

Every value has at least one encoding, many have several; a decoder
accepts all of them (CONFIRMED (crafted)). Real records use the shortest
form, and the 32-bit form for 65,536 and above.

Examples (bits in reading order):

| Bits | Value |
|---|--:|
| `00 1001` | 9 |
| `01 00010000` | 16 |
| `01 11001000` | 200 |
| `10 0000000100000000` | 256 |
| `11 00000000000000010000000000000000` | 65,536 |
| `01 00000000 0000` | 0xFFFFFF00 |
| `01 00001111 1000` | 0xFFFFFFF8 |
| `01 00001111 1111` | 0xFFFFFFFF |

## 5. The record

### 5.1 Flags byte

| Bit | Meaning when set |
|--:|---|
| 0x80 | A program number field is present (section 6). When clear, the record uses the program used by the previous record. |
| 0x40 | Add 258 to the block start (section 5.3). |
| 0x20 | A block length field is present. When clear, the block length is the one this program was last given. |
| 0x10 | A register field is present (section 5.4). |
| 0x08 | A data field is present (section 5.6). |
| 0x07 | Payload size code (section 3). |

Every combination of the bits 0x80, 0x40, 0x20 and 0x10 occurs in real
records except 0x80 with 0x10 alone (which vector S6 exercises). Bit
0x08 was never seen set in 2,558 real records.

### 5.2 Payload layout

The n payload bytes form one bit string, read most significant bit
first. The fields follow one another **without any alignment**, in this
order:

| # | Field | Present when | Form |
|--:|---|---|---|
| 1 | program number | flag 0x80 | number |
| 2 | block start offset | always | number |
| 3 | block length | flag 0x20 | number |
| 4 | register mask and values | flag 0x10 | 7 bits, then one number per set bit |
| 5 | program length and program bytes | the record defines a new program (section 6) | number, then that many 8-bit fields |
| 6 | data length and data bytes | flag 0x08 | number, then that many 8-bit fields |

- Reading any field beyond the end of the payload is corrupt input.
  (CONFIRMED (crafted).)
- Whatever remains of the payload after the last field is ignored: in
  real records this is 0 to 7 zero bits; whole surplus bytes are also
  ignored (CONFIRMED (crafted)).

### 5.3 Block start and block length

- The **block start** is s = G + offset, plus 258 when flag 0x40 is set,
  where G is the group position at which the record is met. The sum is
  formed without wrap-around (the offset is an unsigned 32-bit value,
  positions are 64-bit); an offset given in the `0xFFFFFFxx` form is
  simply a very large offset. A block can therefore never start before
  the position at which its record is met.
- The **block length** L is the value of the block length field when
  present; the field's value also becomes the *remembered length* of the
  selected program. Without the field, L is the remembered length of the
  selected program (each program has its own; a program that was never
  given a length has 0).
- DECISION: L greater than 0x3C000 (245,760) is corrupt input. (Real
  blocks are 1 to 65,536 bytes long. Both tools fail on longer blocks:
  CONFIRMED (crafted) with 0x3C001.)
- DECISION: **L = 0 is corrupt input**, whether the 0 comes from the
  block length field or from a program that was never given a length.
  The check is made after the whole record has been read (so a record
  that is malformed in another way fails for that reason first; the
  outcome is the same). (Not seen in real data: every one of the 2,558
  real records of section 25 has L of 1 or more. The two tools disagree
  on such a record, vector E21: `unar` ends the member's output at the
  block and reports an error, `bsdtar` skips the record.)

Real encoders announce the filters of a stretch of data in advance: RAR
4.20 emits, at the start of a file and then every 4 MiB of raw output,
the records of all blocks of the next 4 MiB, in ascending order of
position. Up to 119 records were seen pending at once.

### 5.4 Register field

Seven bits m, most significant first, taken as a number 0..127: **bit
value 2^i of m stands for register Ri** (i = 0..6). After the mask, one
number follows for each set bit, **in ascending register order** (R0
first). Each number replaces the default initial value of its register.

The initial values of a filter's registers, before the replacement:

| Register | Default |
|---|---|
| R0, R1, R2 | 0 |
| R3 | 0x3C000 |
| R4 | L |
| R5 | the program's use counter (section 6) |
| R6 | 0 (see below) |

The seven values after replacement are stored with the queued filter.
When the filter runs, R6 is replaced by the low 32 bits of the file
position F whatever the record said, and R7 is 0x40000 (section 18).

In real records the mask is 1 (R0) for the delta and audio programs, 3
(R0, R1) for the RGB program and the field is absent for the others.

### 5.5 Program bytes

Present exactly when the record defines a new program (section 6.1).
The length is a number; then follow that many bytes, each an 8-bit
field of the payload bit string (they are generally not aligned to the
payload's bytes).

- A length below 2 is corrupt input (a program needs its check byte and
  at least one more byte, section 16.1; CONFIRMED (crafted) for 0 and
  1). The length cannot exceed what the payload holds, so it is below
  65,535.
- The program is checked and decoded when it is defined (section 16).
  An invalid program is corrupt input at that moment, even if its filter
  would never run.

### 5.6 Data field

The data length is a number; a value above 0x1FC0 (8,128) is corrupt
input (CONFIRMED (crafted)). Then follow that many 8-bit fields.

DECISION: the data bytes are read and discarded. (This is what `unar`
does, CONFIRMED (crafted): they never become visible to the program.
No real record carries the field; section 24.)

## 6. The program store

The filter layer keeps, per solid group:

- an ordered list of **stored programs**, numbered from 0, initially
  empty; for each program its **use counter** and its **remembered
  length** (initially 0);
- the **current program number**, initially "none".

### 6.1 Selecting the program of a record

1. If flag 0x80 is clear: the record uses the current program. If there
   is none, the record is corrupt input (CONFIRMED (crafted)).
2. If flag 0x80 is set, read the program number field v:
   - **v = 0**: the store is emptied (all programs, use counters and
     remembered lengths are forgotten) and all filters still queued
     (section 7) are discarded; raw bytes that were held back for them
     are delivered unchanged. The record then selects program 0, which
     it defines.
   - **v greater than 0**: the record selects program v − 1.
   - Let k be the number of stored programs (after the emptying, if
     any). If the selected number is greater than k, the record is
     corrupt input. If it equals k, **the record defines a new program**
     with that number; its bytes come in field 5. If it is less than k,
     the record reuses the stored program; field 5 is absent.
   - The selected number becomes the current program number.
3. The use counter of the selected program is the number of earlier
   records that used the same stored program: 0 for the record that
   defines it, 1 for the next record that uses it, and so on. It is the
   default of R5 and is also given to the program at address 0x3C02C
   (section 18.1).

Notes:

- In real archives the first record of a group always has v = 0 (85
  groups surveyed, plus all RAR 4.20 archives); v = 1 on an empty store
  is equivalent and accepted (CONFIRMED (crafted)). No real record has
  v = 0 when the store is not empty.
- The store survives member boundaries inside a solid group: a later
  member reuses programs defined by an earlier one, and the use counters
  keep counting (CONFIRMED (real data): RAR 4.20 solid archives).
- The same program bytes may be stored under two numbers; each has its
  own counter and remembered length (CONFIRMED (crafted)).
- DECISION: v = 0 on a non-empty store behaves as written above.
  (CONFIRMED (crafted) with `bsdtar` when the queue is empty and when
  the queued blocks have not started; with a block partly produced
  `bsdtar` delivers something else. `unar` fails on every v = 0 that
  meets a non-empty store, section 22.)
- The order of events matters for v = 0: a block that was complete when
  the record was met has already run (section 7.2) and its result stays
  delivered; only filters whose blocks are not complete are discarded.

### 6.2 Limits

DECISION, all "corrupt input" when exceeded:

- at most **1,024** stored programs (numbers 0..1023). Real groups hold
  at most 4;
- the lengths of all stored programs of a group add up to at most
  **1,048,576** bytes;
- section 16.4 limits a single program.

## 7. Blocks, the queue, and running a filter

### 7.1 Queueing

After a record has been read completely (section 5.2) and its program
selected or defined, the filter (program, s, L, the seven register
values, the use counter) is appended to the **queue** of pending
filters, subject to these checks, each of which fails as corrupt input:

1. L is at least 1 and at most 0x3C000 (section 5.3).
2. **Order.** If the queue is not empty, let T be the end position
   (start + length) of the block of its **last** entry. If s is less
   than T, the record is corrupt input. DECISION. Blocks therefore never
   overlap, are never nested, never repeat a range, and arrive in
   ascending order. (In all 2,173 real records s is at least T. `unar`
   and `bsdtar` both fail on an overlapping block, on a second filter
   for exactly the same range, and on a block that lies before the
   previous one: CONFIRMED (crafted), section 22.) When the queue is
   empty there is nothing to check: s is at least G, and every block
   that has run ended at or before G. (`rar29-lz.md` words the same
   check with an "order mark": the end of the block queued most
   recently in the group, kept after that block has run and across
   members, forgotten on a program number 0. For the reason just given
   the two wordings accept and reject exactly the same streams.)
3. The queue may hold at most **65,536** filters; one more is corrupt
   input (DECISION).
4. If the program is a standard one, the checks of section 9.2.

A block may start exactly at G (offset 0 without the 258) and may start
exactly where the previous block ends. The queue belongs to the solid
group, not to the member: it may still hold filters when a member ends
(section 8).

### 7.2 Delivering and running

Let the *head* be the oldest filter in the queue.

- A raw byte whose position is below the head's s (or any raw byte when
  the queue is empty) is delivered unchanged, in order.
- Raw bytes at positions s and above are **held back** (they stay in
  the window; nothing needs to be copied yet).
- As soon as raw output exists up to position s + L − 1, the block is
  complete. Its L raw bytes (positions s..s + L − 1, no more and no
  less) are taken from the window and the filter is run on them (Part
  II for a standard program, section 18 otherwise) with the file
  position F = s − M, M being the start of the member that is being
  decoded when the block completes (a block always lies inside one
  member, section 8). The L result bytes are delivered in place of the
  raw bytes, the filter is removed from the queue, and delivery
  continues with the next head, whose block starts at or after the end
  of this one.

One LZ match can produce bytes on both sides of a block's start or end
(up to 258 bytes at once); the rules above apply byte by byte. A
complete block is run **before the next record is processed and before
the member is declared ended**, so that the queue never contains a
complete block when a record (in particular one with program number 0)
or the end of the member is handled.

A record met while a block is being held back is queued like any other
(its block starts at or after the end of the held block).

A failed run (section 18.4, or an invalid result) is corrupt input.

### 7.3 What a filter receives

| Input | Value |
|---|---|
| block | the L raw bytes at positions s..s + L − 1 |
| R0..R5 | the values stored with the filter (section 5.4) |
| F | s − M, a 64-bit value; most uses take its low 32 bits |
| use counter | section 6.1 |

## 8. Members and solid groups

- When a group starts: empty store, no current program, empty queue.
- **File positions are relative to the member**: F = s − M, where M is
  the group position of the first byte of the member in which the block
  lies. (CONFIRMED (real data): x86 filters in the later members of RAR
  4.20 solid archives decode correctly only with member-relative
  positions. CONFIRMED (crafted): vectors S4 and S7.)
- When a member ends at group position E:
  - if the head's block has started but is not complete (s less than E
    and s + L greater than E), the member is corrupt input. **A block
    never straddles a member boundary and never extends past the end of
    the data.** (CONFIRMED (crafted): both tools fail, vector E4.)
  - a block that ends exactly at E has been run in the normal way;
  - filters whose blocks start at or after E **stay queued**. If the
    group has a further member they run when their positions are
    reached in that member (or a later one), with the file position
    counted from the start of the member in which the block lies; a
    block that starts exactly at E is the first block of the next
    member and gets F = 0. Records met in the later member are queued
    behind them, and the order check of section 7.1 applies against
    them. When the group ends, whatever is still queued is discarded
    without error. (CONFIRMED (crafted): `unar` applies a filter
    announced in one member to its block in the next, vector S4, and
    both tools deliver a member unharmed whose record describes a block
    beyond the end of the group. Not seen in real data, where the
    records of a member always describe blocks of that member.
    `rar29-lz.md` states the same rule in its section 11.4.)
  - the program store, the use counters, the remembered lengths and the
    current program number are **not** affected by a member boundary.
- The LZ layer's rule for poisoned groups applies: after a member that
  ended in corrupt input, the filter state is discarded with the rest of
  the group state, and no later member of the group is decoded.

# Part II: the standard programs

## 9. Recognition and common rules

### 9.1 Fingerprints

RAR emits six programs. A stored program is a standard one when its
**length in bytes and the CRC-32 of all its bytes** (the common CRC-32,
as for file contents: `123456789` gives `0xCBF43926`) both match a row:

| Program | Length | CRC-32 | Filtered block lies at | Largest L |
|---|--:|---|---|--:|
| x86 E8 | 53 | `0xAD576887` | address 0 (in place) | 0x3C000 |
| x86 E8/E9 | 57 | `0x3CD7E57E` | address 0 (in place) | 0x3C000 |
| Itanium | 120 | `0x3769893F` | address 0 (in place) | 0x3C000 |
| delta | 29 | `0x0E06077D` | address L | 0x1E000 |
| RGB | 149 | `0x1C2C5DC8` | address L | 0x1E000 |
| audio | 216 | `0xBC85E701` | address L | 0x1E000 |

The program bytes are given in section 20.1. "Filtered block lies at"
is where the program leaves its result in the memory of the virtual
machine (section 18.3); the last three write their output behind the
input, which is why they can only handle half as much (2·L must fit
below the address 0x3C000). An implementation that computes the
transformations of sections 10 to 14 directly does not need the memory
at all.

In the survey (section 25) these six are the only programs that occur:
delta 1,193 times, audio 519, E8 277, E8/E9 145, RGB 34, Itanium 5.

### 9.2 Common rules

- DECISION: a standard program is **defined by sections 10 to 14**, not
  by executing its bytes. Within the limits stated there the two are
  identical: this was verified by running the program bytes on the
  machine of Part III against the direct computation for some 30,000
  random blocks and parameter sets (section 25). Outside the limits the
  program's own result would depend on memory it never initialised, so
  no encoder can have produced such a record.
- DECISION: a record that selects a standard program is corrupt input
  unless, after the register field has been applied,
  - R2 = 0, R3 = 0x3C000 and R4 = L,
  - R1 = 0 (all programs except RGB, where R1 is a parameter),
  - L does not exceed the largest L of the table above, and
  - the parameters satisfy the limits of the program's own section.

  The values given for R5 and R6 do not matter, nor does R0 for the x86
  and Itanium programs.
- The instruction budget of section 18.4 does not apply to standard
  programs.
- Matching by length and CRC-32 means that a different byte string with
  the same fingerprint is treated as the standard program. That is
  harmless.

In sections 10 to 14, **B** is the block (B[0..L − 1], the raw bytes),
**O** the result (O[0..L − 1]), and F32 the low 32 bits of the file
position F.

## 10. x86 call translation (E8 and E8/E9)

Parameters: none (F32 only). Limit: L up to 0x3C000.

The transformation works in place. Let C = 0x01000000. A *marker* is the
byte value 0xE8; in the E8/E9 program the byte value 0xE9 is a marker
too.

Scan the block with an index i that starts at 0. **While i + 5 is at
most L**:

- If B[i] is not a marker, add 1 to i.
- If B[i] is a marker, let A be the little-endian 32-bit value at
  B[i + 1..i + 4] and let P = (F32 + i + 1) modulo 2^32 (the file
  position of the first operand byte). Then
  - if A is below C: replace the four bytes by (A − P) modulo 2^32;
  - otherwise, if A is 0x80000000 or more **and** (A + P) modulo 2^32
    is below 0x80000000: replace the four bytes by (A + C) modulo 2^32;
  - otherwise leave them unchanged.

  In all three cases add 5 to i (the four operand bytes are never
  examined as markers).

Consequences: a marker in the last four bytes of the block is never
translated; a block of 4 bytes or fewer is returned unchanged.

Seen as signed numbers: an absolute target in 0..C − 1 becomes a
relative displacement again, and a small negative value −P..−1 (stored
by the archiver for a target that would have been "before the file") is
moved up by C.

## 11. Itanium

Parameters: none (F32 only). Limit: L up to 0x3C000.

The block is treated as a sequence of 16-byte units (Itanium instruction
bundles); unit k (k = 0, 1, 2, ...) starts at byte offset 16·k. The
transformation works in place.

If L is 21 or less, the block is returned unchanged. Otherwise process
the units k = 0, 1, 2, ... **for which 16·k + 21 is at most L**, in
ascending order (the last 5 to 20 bytes of a block are never the start
of a processed unit).

Bits of a unit are numbered as in a little-endian number: bit b of the
unit is bit (b modulo 8) of the byte at offset 16·k + (b div 8), bit 0
being the least significant bit of that byte. A "field of w bits at bit
b" is the w-bit number whose least significant bit is bit b. All fields
used below lie inside the unit's 16 bytes (bits 18..127).

For unit k:

1. Let t = (first byte of the unit, low 5 bits) − 16. If t is negative
   the unit is left unchanged.
2. Otherwise look up the **slot mask** for t:

   | t | 0 | 1 | 2 | 3 | 4 | 5 | 6 | 7 | 8 | 9 | 10 | 11 | 12 | 13 | 14 | 15 |
   |---|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|
   | mask | 4 | 4 | 6 | 6 | 0 | 0 | 7 | 7 | 4 | 4 | 0 | 0 | 4 | 4 | 0 | 0 |

3. For each slot j = 0, 1, 2 **whose bit (value 2^j) is set in the
   mask**, in ascending order:
   - the 4-bit field at bit 42 + 41·j must equal 5; if it does not, the
     slot is left unchanged;
   - if it does, let V be the 20-bit field at bit 18 + 41·j. Replace it
     by (V − U) modulo 2^20, where U = ((F32 div 16) + k) modulo 2^32.
     No other bit of the unit changes.

(The mask table is the 16 bytes of constant data carried by the
program, section 16.2. The Itanium program occurs only in archives made
with the corresponding RAR switch; it was checked on such archives and
on crafted data, `unar` running the program bytes: section 25.)

## 12. Delta

Parameter: **N = R0**, the number of channels. Limits: N in 1..0x1E000
(122,880), L up to 0x1E000; DECISION for N = 0 and for larger N, which
are corrupt input (section 9.2).

The block holds the channels one after the other, each as differences;
the result interleaves them. Read the input bytes in order with a
position q that starts at 0. For each channel c = 0, 1, ..., N − 1 in
turn:

- set the running value v to 0;
- for each output position d = c, c + N, c + 2·N, ... that is below L:
  set v to (v − B[q]) modulo 256, store O[d] = v, and add 1 to q.

A channel c that is L or more has no positions and consumes no input;
an implementation must not loop over such channels one by one. N may
exceed L (CONFIRMED (real data): RAR 4.20 emits delta blocks of 1 and
5 bytes with N = 31).

Real values of N: 1 to 32.

## 13. RGB (24-bit picture predictor)

Parameters: **W = R0**, the stride in bytes of one picture row;
**R = R1**, the offset (0, 1 or 2) of the first colour component of a
pixel inside the block's 3-byte grid. Limits: W in 3..L, R in 0..2, L
in 3..0x1E000; anything else is corrupt input (DECISION, section 9.2).

The block holds three colour planes one after the other, each as
prediction errors. The result is built in two steps. O starts as **L
zero bytes**; "the current value of O[x]" below means whatever has been
stored there so far (0 if nothing has).

**Step 1, prediction.** Read the input with a position q that starts
at 0. For each plane c = 0, 1, 2 in turn:

- set the previous value v to 0;
- for each position j = c, c + 3, c + 6, ... that is below L:
  - if j is less than W, the prediction is v;
  - otherwise let a = the current value of O[j − W + 3] and b = the
    current value of O[j − W] (the byte above-right and the byte above,
    in the previous row). Let p = v + a − b as an ordinary integer, and
    let dv = |p − v|, da = |p − a|, db = |p − b|. The prediction is
    - v if dv is at most da and dv is at most db;
    - otherwise a if da is at most db;
    - otherwise b;
  - set v to (prediction − B[q]) modulo 256, store O[j] = v, add 1 to q.

**Step 2, colour decorrelation.** For i = R, R + 3, R + 6, ... as long
as i is at most L − 3: add O[i + 1] to O[i] and to O[i + 2] (each
modulo 256; O[i + 1], the middle component, is not changed).

One special case completes step 2: if R is greater than L − 3 (only
possible for L = 3 with R = 1 or 2, and L = 4 with R = 2), the step is
still carried out once, for i = R, with every position at or beyond L
treated as absent: when R = L − 2, O[R + 1] is added to O[R]; when
R = L − 1, nothing changes.

Notes:

- With W a multiple of 3 and at least 6, which is what real encoders
  send (600, 711, 1716, 7854 in the RAR 4.20 archives; always a multiple
  of 3), a and b are bytes of the same plane that were stored earlier in
  the same pass. With other W, a (and for W = 3 also the position j
  itself) can be a byte of a plane not yet computed; the "L zero bytes"
  rule then makes it 0. This is a DECISION: the two tools agree with it
  only when the filter is the first to run in a group, because both keep
  whatever an earlier filter left in memory (section 22).
- Real values of R: 0, 1 and 2 all occur.
- Why W may not exceed L: the program finds the byte above by
  subtracting W from an address and treats the result as "no row above"
  only while it stays non-negative; with W greater than L the
  subtraction wraps for the first positions and the program reads
  unrelated memory. No encoder can rely on that. RAR 4.20 never sends
  such a stride: for pictures whose rows are longer than a block (rows
  of 65,520 to 300,000 bytes were tried) it sends W = 51, and for
  narrow pictures a multiple of 3 near 4,500 (2,451, 4,515, 4,527,
  6,255 were seen); L was 6,518 to 65,536 in all of these.

## 14. Audio

Parameter: **N = R0**, the number of channels. Limits: N in 1..254, L
up to 0x1E000; DECISION for N = 0 and N above 254, which are corrupt
input (section 9.2). Real values of N: 1, 2, 3, 4 and 31.

As in the delta program the block holds the channels one after the
other and the result interleaves them: read the input with a position q
starting at 0; for each channel c = 0, 1, ..., N − 1 in turn, for each
output position d = c, c + N, c + 2·N, ... below L, one **sample step**
consumes B[q] (then q increases by 1) and stores O[d]. Channels at or
beyond L are empty and must not be looped over.

Each channel starts with a fresh predictor state, **all zero**:

| Item | Meaning | Range |
|---|---|---|
| K1, K2, K3 | weights | −17..16 |
| D1, D2, D3 | difference terms | signed |
| LD | last difference | −128..127 |
| LB | last output byte | 0..255 |
| E0..E6 | seven error sums | non-negative |
| n | number of sample steps done in this channel | counts from 0 |

One sample step with input byte x:

1. **Shift the differences**: D3 takes the value of D2; then D2 becomes
   LD − D1 (using the old D1); then D1 becomes LD.
2. **Predict**: let t = 8·LB + K1·D1 + K2·D2 + K3·D3 (an ordinary
   signed integer; its magnitude stays below 16,384). The predicted
   byte is (t div 8) modulo 256, where "div" rounds toward negative
   infinity (an arithmetic shift right by 3).
3. **Output**: y = (predicted byte − x) modulo 256. Store O[d] = y.
4. **Update the last values**: LD becomes (y − LB) reduced to a signed
   byte (the value in −128..127 congruent to y − LB modulo 256); then
   LB becomes y.
5. **Accumulate errors**: let e = 8 times x taken as a signed byte
   (so e is in −1024..1016). Add |e| to E0, |e − D1| to E1, |e + D1| to
   E2, |e − D2| to E3, |e + D2| to E4, |e − D3| to E5, |e + D3| to E6
   (D1..D3 as they are after step 1).
6. **Adapt, on the 1st, 33rd, 65th, ... step of the channel**: if n is
   a multiple of 32 (n counts the steps completed before this one), find
   the smallest of E0..E6; among equal sums the one with the lowest
   index wins. Then set all seven sums to 0, and
   - E0 smallest: nothing more;
   - E1 smallest: K1 decreases by 1, unless it is already below −16;
   - E2 smallest: K1 increases by 1, unless it is already 16;
   - E3: K2 decreases likewise; E4: K2 increases likewise;
   - E5: K3 decreases likewise; E6: K3 increases likewise.

   (A weight thus stays within −17..16. On the very first step all
   differences are zero, all sums are equal and nothing changes.)
7. n increases by 1.

This predictor resembles the multimedia predictor of the RAR 2.0 method
(`rar20.md`) but is not the same: three terms instead of five, no value
shared between channels, the adaptation tested before the counter is
increased, and a fresh state for every block and channel.

# Part III: the virtual machine

A program that is not one of the six standard programs is run on the
machine described here. Nothing in real data needs this part (section
25); it exists so that such a record can be honoured instead of being
rejected, and so that the standard programs' byte strings can be used
as a cross-check of an implementation.

## 15. Machine state

### 15.1 Memory

- The machine has **0x40000 (262,144) bytes** of memory, addresses
  0..0x3FFFF, all readable and writable. Conventional use:

  | Addresses | Use |
  |---|---|
  | 0..0x3BFFF | work area; the block is placed at address 0 |
  | 0x3C000..0x3C03F | system area: parameters and results (section 18) |
  | 0x3C040..0x3DFFF | the program's constant data (section 16.2) |
  | 0x3E000..0x3FFFF | free; the stack grows downward from 0x40000 |

- Every address used by an instruction is **reduced modulo 0x40000**
  first (only its low 18 bits count).
- A byte access reads or writes the one byte at the reduced address.
- A 32-bit access at the reduced address a reads or writes the four
  bytes a, a + 1, a + 2, a + 3 as a little-endian value. If a is above
  0x3FFFC these bytes run past the end of the memory: the machine has
  **three extra bytes** at 0x40000, 0x40001 and 0x40002 that can only be
  reached this way. They are ordinary storage (they are *not* the bytes
  at addresses 0..2). DECISION, following the reference implementation;
  CONFIRMED (crafted).

### 15.2 Registers and flags

- Eight 32-bit registers R0..R7. R7 is the stack pointer.
- A 32-bit **flags word**. Three of its bits have a meaning:

  | Name | Bit value | Meaning |
  |---|---|---|
  | C | 0x00000001 | carry / borrow |
  | Z | 0x00000002 | zero |
  | S | 0x80000000 | sign |

  An instruction that "sets the flags from a result r with carry c"
  replaces the whole word by: Z if r is 0, otherwise the bit 31 of r as
  S; plus C if c is 1. All other bits become 0, and Z and S are never
  set together. The remaining bits of the word can only be non-zero
  after the instruction that loads the word from the stack (section
  17.4).

## 16. Program encoding

### 16.1 Check byte

The first byte of a program must equal the **exclusive-or of all its
other bytes**. If it does not, the program is invalid. A program has at
least 2 bytes. (For the fingerprints of section 9.1 the CRC-32 is taken
over all bytes including this one.)

### 16.2 Body

The bytes after the check byte form one bit string, read most
significant bit first:

1. **One bit**: constant data flag. If it is 1, a number c follows and
   then **c + 1 bytes** of constant data, each an 8-bit field. More than
   0x1F80 (8,064) bytes of constant data make the program invalid
   (DECISION; the reference implementation silently drops the excess).
2. **Instructions**, as long as **at least 8 bits remain** in the bit
   string. Each instruction is decoded as in section 16.3. Fewer than 8
   remaining bits are ignored (real programs end with 2 to 7 unused
   bits).

Reading beyond the end of the bit string, in the constant data or
inside an instruction, makes the program invalid (CONFIRMED
(crafted)).

After decoding, **if the program has no instructions or its last
instruction is not the return instruction (number 22), one return
instruction is appended.** The instructions are numbered from 0 in the
order decoded; these *instruction indexes* are what jumps refer to.

### 16.3 One instruction

1. **Instruction number.** Read 4 bits. If the value is below 8, it is
   the instruction number (0..7). Otherwise read 2 more bits; the six
   bits together are a value 32..63, and the instruction number is that
   value minus 24 (8..39).
2. **Width bit**, only for the instructions that have a byte form
   (column "B" in section 17.2): 1 bit; 1 selects the byte form, 0 the
   32-bit form.
3. **Operands**: none, one or two (section 17.2), each encoded as:

   | Bits | Operand |
   |---|---|
   | `1`, then 3 bits r | register Rr |
   | `00`, then a value | immediate: an **8-bit field** in the byte form, otherwise a **number** (section 4) |
   | `010`, then 3 bits r | memory at the address held in Rr |
   | `0110`, then 3 bits r, then a number d | memory at address Rr + d (32-bit sum, then reduced) |
   | `0111`, then a number d | memory at address d |

4. **Jump targets.** When the first operand of a jump instruction
   (column "J") is an immediate, its number v is converted at decoding
   time into an instruction index:

   | v | Target index |
   |---|---|
   | 256 or more | v − 256 (absolute) |
   | 0..7 | own index + v |
   | 8..15 | own index + v − 16 (that is −8..−1) |
   | 16..135 | own index + v − 8 (that is +8..+127) |
   | 136..255 | own index + v − 264 (that is −128..−9) |

   "Own index" is the index of the jump instruction itself; the sum is
   taken modulo 2^32, so a target before instruction 0 becomes a huge
   index that fails when the jump is taken. A jump operand of any other
   kind (register, memory) is not converted: its value at run time is
   the target index.

### 16.4 Validity

A program is invalid, and the record that defines it corrupt input,
when

- the check byte is wrong, the length is below 2, or decoding reads
  past the end (above);
- an operand that an instruction **writes** is an immediate: the first
  operand of instructions 0, 2, 3, 6, 7, 9, 10, 11, 20, 23..27 and
  32..38, or the second operand of instruction 34. (CONFIRMED
  (crafted) except for instruction 20, where `unar` crashes; DECISION
  there.)

A program of n bytes has at most 8·(n − 1)/6 + 1 instructions (the
shortest instruction is 6 bits), so at most 87,380.

## 17. Instructions

### 17.1 Operand values

How an operand is read and written depends on the form of the
instruction:

| | 32-bit form | Byte form |
|---|---|---|
| read a register | its 32 bits | its low 8 bits (0..255) |
| read an immediate | the number | the 8-bit field (0..255) |
| read memory | 32-bit little-endian value | one byte |
| write a register | all 32 bits | the register becomes the value's low 8 bits; **its upper 24 bits become 0** |
| write memory | four bytes | one byte (the low 8 bits) |

The address of a memory operand is computed from the register's value
at the moment of each read or write.

Below, **a** is the value of the first operand, **b** of the second,
and "store r" means: write r to the first operand.

### 17.2 Table

"Ops" is the number of operands, "B" marks instructions that have a
width bit and a byte form, "J" marks jump instructions (their operand
is always read as a 32-bit value).

| No. | Name | Ops | B | J | Effect | Flags |
|--:|---|--:|:-:|:-:|---|---|
| 0 | mov | 2 | B | | store b | unchanged |
| 1 | cmp | 2 | B | | r = (a − b) modulo 2^32; nothing stored | from r, C = 1 if a is below b |
| 2 | add | 2 | B | | r = a + b, modulo 2^32 (byte form: modulo 256); store r | section 17.3 |
| 3 | sub | 2 | B | | r = (a − b) modulo 2^32; store r | as cmp |
| 4 | jz | 1 | | J | jump if Z is set | unchanged |
| 5 | jnz | 1 | | J | jump if Z is clear | unchanged |
| 6 | inc | 1 | B | | r = a + 1, modulo 2^32 (byte form: modulo 256); store r | from r, C = 0 |
| 7 | dec | 1 | B | | r = a − 1, likewise; store r | from r, C = 0 |
| 8 | jmp | 1 | | J | jump | unchanged |
| 9 | xor | 2 | B | | r = a exclusive-or b; store r | from r, C = 0 |
| 10 | and | 2 | B | | r = a and b; store r | from r, C = 0 |
| 11 | or | 2 | B | | r = a or b; store r | from r, C = 0 |
| 12 | test | 2 | B | | r = a and b; nothing stored | from r, C = 0 |
| 13 | js | 1 | | J | jump if S is set | unchanged |
| 14 | jns | 1 | | J | jump if S is clear | unchanged |
| 15 | jb | 1 | | J | jump if C is set | unchanged |
| 16 | jbe | 1 | | J | jump if C or Z is set | unchanged |
| 17 | ja | 1 | | J | jump if C and Z are both clear | unchanged |
| 18 | jae | 1 | | J | jump if C is clear | unchanged |
| 19 | push | 1 | | | section 17.4 | unchanged |
| 20 | pop | 1 | | | section 17.4 | unchanged |
| 21 | call | 1 | | J | section 17.4 | unchanged |
| 22 | ret | 0 | | | section 17.4 | unchanged |
| 23 | not | 1 | B | | store the bitwise complement of a | unchanged |
| 24 | shl | 2 | B | | shift left, section 17.3 | section 17.3 |
| 25 | shr | 2 | B | | shift right, section 17.3 | section 17.3 |
| 26 | sar | 2 | B | | arithmetic shift right, section 17.3 | section 17.3 |
| 27 | neg | 1 | B | | r = (0 − a) modulo 2^32; store r | from r, C = 1 if r is not 0 |
| 28 | pusha | 0 | | | section 17.4 | unchanged |
| 29 | popa | 0 | | | section 17.4 | unchanged |
| 30 | pushf | 0 | | | section 17.4 | unchanged |
| 31 | popf | 0 | | | section 17.4 | loaded |
| 32 | movzx | 2 | | | section 17.5 | unchanged |
| 33 | movsx | 2 | | | section 17.5 | unchanged |
| 34 | xchg | 2 | B | | write b's value to the first operand, then a's old value to the second | unchanged |
| 35 | mul | 2 | B | | store (a × b) modulo 2^32 | unchanged |
| 36 | div | 2 | B | | if b is not 0: store the unsigned quotient a div b; if b is 0 nothing happens | unchanged |
| 37 | adc | 2 | B | | add with carry, section 17.3 | section 17.3 |
| 38 | sbb | 2 | B | | subtract with borrow, section 17.3 | section 17.3 |
| 39 | print | 0 | | | nothing | unchanged |

"From r" means: set the flags from r as defined in section 15.2, with
the carry stated.

Points that follow from the table and section 17.1 but are easy to get
wrong (all CONFIRMED (crafted) unless noted):

- **cmp, sub and neg in the byte form compute a 32-bit result from the
  two byte values** and set the flags from that 32-bit result. So for
  byte values a below b, cmp and sub set S and C (the result is a
  "negative" 32-bit number) although the byte stored by sub is its low
  8 bits; neg of a non-zero byte sets S and C.
- **inc, dec, xor, and, or, test in the byte form never set S**: their
  result is a value 0..255, whose bit 31 is clear.
- inc and dec clear C.
- A byte-form write to a register clears the register's upper 24 bits,
  for every instruction (mov, add, xchg, ...).
- xchg in the byte form: the rule of the table with the byte read and
  write rules. INFERRED (`unar` crashes on it, section 22).
- A jump to a target index that is not below the number of instructions
  (after the appended return, section 16.2) makes the run fail (section
  18.4). An untaken conditional jump never fails.

### 17.3 Additions, subtractions and shifts in detail

**add.** 32-bit form: r = (a + b) modulo 2^32; flags from r; C = 1 if r
is below a. Byte form: r = (a + b) modulo 256; Z if r is 0, otherwise
**S if bit 7 of r is set**; C = 1 if r is below a.

**adc.** Let c be the C flag before the instruction (0 or 1). r =
a + b + c, modulo 2^32 in the 32-bit form and modulo 256 in the byte
form. Store r. Flags from r (so the byte form never sets S); C = 1 if r
is below a, or if r equals a and c is 1.

**sbb.** r = a − b − c, modulo 2^32 or modulo 256 likewise. Store r.
Flags from r (the byte form never sets S); C = 1 if r is above a, or if
r equals a and c is 1.

**Shifts.** Let n = b modulo 32 and m = (b − 1) modulo 32 (so m = 31
when b is 0 or a multiple of 32). In the byte form a and b are byte
values and the computation is still done on 32-bit values.

| | Result r | Carry |
|---|---|---|
| shl | (a × 2^n) modulo 2^32 | bit 31 of ((a × 2^m) modulo 2^32) |
| shr | a div 2^n | bit m of a |
| sar | a taken as a signed 32-bit number, shifted right arithmetically by n (sign bit repeated) | bit m of a |

Store r (the byte form stores its low 8 bits); flags from the 32-bit r
with that carry. In the byte form sar behaves exactly like shr, because
a byte value is never negative as a 32-bit number. Taking the shift
count modulo 32 is a DECISION (it is what the reference tool does on
the machine it was tested on; the reference source leaves counts of 32
and more undefined).

### 17.4 Stack instructions

All stack accesses are 32-bit accesses at the address in R7 (reduced as
in section 15.1). R7 itself is a full 32-bit register and is not
reduced.

- **push**: first R7 decreases by 4; then the operand is read (32-bit;
  an operand that involves R7 therefore sees the decreased value) and
  written at address R7.
- **pop**: the 32-bit value at address R7 is read and written to the
  operand; then R7 increases by 4. (If the operand is R7 itself, R7
  ends up as the popped value plus 4.)
- **call**: R7 decreases by 4; the index of the instruction after the
  call (own index + 1) is written at address R7; then the operand is
  read and execution jumps to that index.
- **ret**: if R7, as an unsigned 32-bit number, is **0x40000 or more,
  the run ends successfully**. Otherwise the 32-bit value at address R7
  is read, R7 increases by 4, and execution jumps to that value as an
  instruction index.
- **pusha**: with R the value of R7 before the instruction, R0 is
  written at address R − 4, R1 at R − 8, R2 at R − 12, R3 at R − 16,
  R4 at R − 20, R5 at R − 24, R6 at R − 28 and R (the old R7) at
  R − 32. Then R7 becomes R − 32.
- **popa**: with R the value of R7 before the instruction, R0 is read
  from address R + 28, R1 from R + 24, R2 from R + 20, R3 from R + 16,
  R4 from R + 12, R5 from R + 8, R6 from R + 4 and **R7 from R**. R7 is
  not otherwise adjusted: after a matching pusha it returns to its
  value before that pusha because that value was saved.
- **pushf**: R7 decreases by 4; the flags word is written at address
  R7.
- **popf**: the flags word is loaded with the 32-bit value at address
  R7 (all 32 bits); R7 increases by 4.

### 17.5 movzx and movsx

Neither has a width bit. The second operand is read **as in the byte
form** when it is a register (low 8 bits) or memory (one byte); an
immediate second operand is a number.

- movzx stores that value into the first operand with a 32-bit write.
  (For an immediate source this is the whole number, not only its low
  8 bits.)
- movsx takes the low 8 bits of that value, extends bit 7 through bits
  8..31, and stores the result with a 32-bit write.

### 17.6 What the standard programs use

The six standard programs together use the instructions mov, cmp, add,
sub, jz, jnz, inc, dec, jmp, and, or, test, js, jns, jb, jbe, ja, jae,
push, pop, call, ret, not, shl, shr, neg, pusha, popa, movzx, movsx,
xchg and mul; the byte forms of mov, cmp, add and sub; and the operand
kinds register, immediate, memory at a register, and memory at a
register plus a number. Never used by them, and therefore CONFIRMED
(crafted) only: xor, sar, pushf, popf, div, adc, sbb, print, every
other byte form, and the absolute memory operand.

## 18. One invocation

### 18.1 Setting up

1. **All of memory is zero**, including the three extra bytes (DECISION;
   the reference implementation leaves whatever earlier invocations
   wrote, section 22). An implementation need not clear 256 KiB each
   time, see section 21.
2. The block's L raw bytes are placed at addresses 0..L − 1.
3. The system area is filled in, each item a little-endian 32-bit value:

   | Address | Value |
   |---|---|
   | 0x3C000, 0x3C004, ..., 0x3C018 | the seven register values R0..R6 stored with the filter (section 5.4: defaults replaced by the record's values; the R6 entry is 0 unless the record gave one) |
   | 0x3C01C | L |
   | 0x3C020 | 0 |
   | 0x3C024 | the low 32 bits of the file position F |
   | 0x3C028 | the high 32 bits of F |
   | 0x3C02C | the use counter |
   | 0x3C030..0x3C03F | 0 |

4. The program's constant data, if any, is placed at 0x3C040 onward.
5. Registers: R0..R5 take the stored values; **R6 = the low 32 bits of
   F**; R7 = 0x40000. The flags word is 0.

### 18.2 Running

Execution starts at instruction 0 and proceeds to the next index unless
an instruction jumps. It ends when a ret is executed with R7 at
0x40000 or above (section 17.4). Because of the appended return,
execution can never run past the last instruction.

### 18.3 Result

After a successful end:

- let A = (the 32-bit value at 0x3C020) modulo 0x40000 and Q = (the
  32-bit value at 0x3C01C) modulo 0x40000;
- DECISION: if Q is not L, or if A + L is 0x40000 or more, the result
  is corrupt input;
- the filter's result is the L bytes at addresses A..A + L − 1.

A program that leaves both words alone thus returns the block as
modified in place at address 0; the delta, RGB and audio programs store
L at 0x3C020 and build their output behind the input.

Nothing else survives the invocation: no memory content, register or
flag is carried to the next invocation of the same or any other
program. The value a program may leave at 0x3C030 is ignored (DECISION;
sections 22 and 24).

### 18.4 Failure and the instruction budget

A run **fails**, and the member is corrupt input, when

- a taken jump, a call or a ret targets an index that is not below the
  number of instructions; or
- the run executes more than **65,536 + 256 × L** instructions
  (DECISION). Every executed instruction counts, including the final
  ret.

There are no other run-time errors: every address is reduced, division
by zero does nothing, and the stack pointer may hold any value.

How the budget was chosen. The six standard programs, run on this
machine, are the only genuine programs known, and they show what a
genuine program costs (instructions executed, worst case observed over
random and adversarial blocks; N is the channel count):

| Program | Instructions for a block of L bytes |
|---|---|
| x86 E8, E8/E9 | up to 7 × L |
| Itanium | up to 4 × L |
| delta | about 6 × L + 14 × N |
| RGB | up to 36 × L |
| audio | about 93 × L + 222 × N (56,517 for L = 1 and N = 254) |

The per-byte allowance of 256 is thus more than twice the dearest
program, and the fixed allowance covers a set-up of the size the audio
program needs for its largest channel count. The budget does not apply
to the standard programs themselves (section 9.2): the delta program
with a very large N would exceed it although its result is trivial.

# Examples, vectors, safety and status

## 19. Worked examples

### 19.1 A record that defines a program

The first record of a real RAR 4.20 archive of a table of 32-bit
values. Record bytes: flags `b6`, size byte `1f`, then 38 payload bytes

`00 0c 00 04 00 00 08 88 e9 78 0c d2 0c 07 61 3a 41 78 4b b3 6e 9f 52 0a da ca 27 40 ba e7 0b 64 8a 62 71 fb b8 00`

Flags `b6` = `1011 0110`: program number field (0x80), block length
field (0x20), register field (0x10); size code 6, so one size byte
follows: 0x1f + 7 = 38 payload bytes. The payload begins, in binary,

`00000000 00001100 00000000 00000100 00000000 00000000 00001000 10001000 11101001 01111000 ...`

| Payload bits | Value | Meaning |
|--:|---|---|
| 0..5 | `00 0000` | program number field: number 0. The store is emptied; the record defines program 0. |
| 6..11 | `00 0000` | block start offset: number 0. The record was met at G = 0, flag 0x40 is clear: s = 0. |
| 12..45 | `11 00000000000000010000000000000000` | block length: 32-bit form, 65,536. L = 65,536; program 0 remembers it. |
| 46..52 | `0000001` | register mask 1: only R0 follows. |
| 53..58 | `00 0100` | R0 = 4. |
| 59..68 | `01 00011101` | program length: 29. |
| 69..300 | 29 × 8 bits | the program bytes `2f 01 9a 41 ...` (the delta program of section 20.1; note that they straddle the payload's byte boundaries). |
| 301..303 | `000` | unused. |

Result: program 0 (delta) is stored, use counter 0; a filter with s = 0,
L = 65,536 and N = R0 = 4 is queued.

### 19.2 Records that reuse a program

The next two records of the same archive, both still met at G = 0:

Flags `76` = `0111 0110`: no program number (current program 0), add
258, length field, register field; size byte `02`, 9 payload bytes
`c0 00 40 3f a0 60 00 24 00`:

| Payload bits | Value | Meaning |
|--:|---|---|
| 0..33 | `11 00000000000000010000000011111110` | offset 65,790. s = 0 + 65,790 + 258 = 66,048. |
| 34..51 | `10 0000011000000000` | block length 1,536. |
| 52..58 | `0000001` | register mask 1. |
| 59..64 | `00 1000` | R0 = 8. |
| 65..71 | | unused. |

Use counter 1. The last queue entry ends at 65,536, so the order check
of section 7.1 holds (66,048 is not below 65,536).

Flags `55` = `0101 0101`: add 258, register field, no length field;
size code 5: 6 payload bytes `c0 00 42 3f 80 90`:

| Payload bits | Value | Meaning |
|--:|---|---|
| 0..33 | `11 00000000000000010000100011111110` | offset 67,838. s = 68,096. |
| 34..40 | `0000001` | register mask 1. |
| 41..46 | `00 1000` | R0 = 8. |
| 47 | | unused. |

L is the remembered length of program 0: 1,536. Use counter 2.

The shortest possible record occurs in real data too: flags `00` and
the single payload byte `00` (offset 0, everything else as for the
previous record of the same program; six payload bits used).

### 19.3 Decoding a program

The delta program (section 20.1), 29 bytes: `2f 01 9a 41 80 ec 27 48 2f
09 76 6d d3 ea 41 5b 59 44 e8 17 5c e1 6c 91 4c 4e 3f 77 00`. The
exclusive-or of the 28 bytes after the first is 0x2f, equal to the
first byte. The body is 224 bits:

`00000001 10011010 01000001 10000000 11101100 00100111 01001000 00101111 00001001 01110110 01101101 11010011 11101010 01000001 01011011 01011001 01000100 11101000 00010111 01011100 11100001 01101100 10010001 01001100 01001110 00111111 01110111 00000000`

Bit 0 is `0`: no constant data. Then, with the bits grouped as
instruction number and width bit, first operand, second operand:

| Index | Body bits | Bits | Instruction | Operands |
|--:|--:|---|---|---|
| 0 | 1..26 | `0000 0`, `0110 011 01 00100000`, `1 100` | mov | memory at R3 + 32; R4 |
| 1 | 27..39 | `0000 0`, `1 110`, `1 100` | mov | R6; R4 |
| 2 | 40..52 | `0010 0`, `1 110`, `1 001` | add | R6; R1 |
| 3 | 53..65 | `0000 0`, `1 011`, `1 100` | mov | R3; R4 |
| 4 | 66..78 | `0010 0`, `1 011`, `1 011` | add | R3; R3 |
| 5 | 79..91 | `0011 0`, `1 101`, `1 101` | sub | R5; R5 |
| 6 | 92..106 | `0011 1`, `1 101`, `010 010` | sub, byte form | R5; memory at R2 |
| 7 | 107..121 | `0000 1`, `010 110`, `1 101` | mov, byte form | memory at R6; R5 |
| 8 | 122..130 | `0110 0`, `1 010` | inc | R2 |
| 9 | 131..143 | `0010 0`, `1 110`, `1 000` | add | R6; R0 |
| 10 | 144..156 | `0001 0`, `1 110`, `1 011` | cmp | R6; R3 |
| 11 | 157..170 | `100111`, `00 00 1011` | jb | immediate number 11: in 8..15, target = 11 + 11 − 16 = 6 |
| 12 | 171..179 | `0110 0`, `1 001` | inc | R1 |
| 13 | 180..192 | `0001 0`, `1 001`, `1 000` | cmp | R1; R0 |
| 14 | 193..210 | `100111`, `00 01 11111011` | jb | immediate number 251: in 136..255, target = 14 + 251 − 264 = 1 |
| 15 | 211..216 | `101110` | ret | |

`100111` is 39, instruction 39 − 24 = 15 (jb); `101110` is 46,
instruction 22 (ret). After instruction 15 only 7 bits remain (fewer
than 8), so decoding stops; the last instruction is a return and
nothing is appended.

Reading the listing: instruction 0 stores L at 0x3C020 (the result will
start at address L); R6 walks the output positions of one channel from
L + R1 in steps of R0 up to 2·L (kept in R3); R5 holds the running
byte; R2 walks the input; R1 counts channels up to R0. This is the
transformation of section 12, and it shows why the record must leave
R1 and R2 at 0 and R3, R4 at their defaults.

### 19.4 x86 translation

The 36-byte block of vector S1 (section 20.4), at file position F = 12,
E8 program:

`90 e8 00 20 00 00 e8 00 00 00 01 e8 fa ff ff ff e8 00 e0 ff ff e9 10 00 00 00 e8 e8 ff ff ff 00 e8 56 34 12`

| i | Byte | A | P = 12 + i + 1 | Action |
|--:|---|---|--:|---|
| 0 | `90` | | | not a marker; i becomes 1 |
| 1 | `e8` | 0x00002000 | 14 | A below 0x01000000: store 0x2000 − 14 = 0x00001FF2 (`f2 1f 00 00`); i becomes 6 |
| 6 | `e8` | 0x01000000 | 19 | not below C, not 0x80000000 or more: unchanged; i becomes 11 |
| 11 | `e8` | 0xFFFFFFFA | 24 | A + P = 18 (modulo 2^32), below 0x80000000: store A + C = 0x00FFFFFA (`fa ff ff 00`); i becomes 16 |
| 16 | `e8` | 0xFFFFE000 | 29 | A + P = 0xFFFFE01D, not below 0x80000000: unchanged; i becomes 21 |
| 21 | `e9` | | | not a marker for the E8 program; i becomes 22 |
| 22..25 | `10 00 00 00` | | | not markers; i becomes 26 |
| 26 | `e8` | 0xFFFFFFE8 | 39 | A + P = 15: store 0x00FFFFE8 (`e8 ff ff 00`); i becomes 31. The `e8` at offset 27 was an operand byte and is not examined. |
| 31 | `00` | | | not a marker; i becomes 32 |
| 32 | | | | 32 + 5 is more than 36: stop. The `e8` at offset 32 stays. |

Result: `90 e8 f2 1f 00 00 e8 00 00 00 01 e8 fa ff ff 00 e8 00 e0 ff ff e9 10 00 00 00 e8 e8 ff ff 00 00 e8 56 34 12`.
With the E8/E9 program the marker at offset 21 is translated as well:
A = 0x10, P = 34, stored value 0x10 − 34 = 0xFFFFFFEE.

### 19.5 One Itanium unit

Unit 0 of the block of section 20.2 (I1), at file position 0x50:

`30 4d 1a b8 65 56 27 17 11 05 2e 8c ea 4a cc 5d`

- First byte 0x30: low 5 bits 16, t = 0, slot mask 4: only slot 2.
- Slot 2: the 4-bit field at bit 42 + 82 = 124 is the high half of
  byte 15 (0x5d): 5. The slot qualifies.
- The 20-bit field at bit 18 + 82 = 100 starts in byte 12 at bit 4.
  Bytes 12..14 are `ea 4a cc`, as a little-endian number 0xCC4AEA;
  dropping the low 4 bits gives V = 0xCC4AE.
- U = (0x50 div 16) + 0 = 5. New value 0xCC4AE − 5 = 0xCC4A9.
- Written back, bytes 12..14 become `9a 4a cc` (the low 4 bits of byte
  12, `a`, are kept).

### 19.6 First steps of the delta and audio programs

Delta, input `07 24 41 5e 7b 98 b5 d2 ef 0c ...` (vector D1), N = 3,
L = 20. Channel 0 has the positions 0, 3, 6, 9, 12, 15, 18 and takes
input bytes 0..6: v = 0 − 0x07 = 0xf9 at O[0]; 0xf9 − 0x24 = 0xd5 at
O[3]; 0xd5 − 0x41 = 0x94 at O[6]; and so on. Channel 1 (positions 1, 4,
..., 19) starts again from 0 with input byte 7: 0 − 0xd2 = 0x2e at
O[1].

Audio, input `00 0c 18 25 ...` (vector A1), N = 1:

| Step | x | D1, D2, D3 after step 1 | Predicted | y | LD after | e |
|--:|---|---|--:|---|--:|--:|
| 1 | `00` | 0, 0, 0 | 0 | `00` | 0 | 0 |
| 2 | `0c` | 0, 0, 0 | 0 | `f4` | −12 | 96 |
| 3 | `18` | −12, −12, 0 | 0xf4 | `dc` | −24 | 192 |
| 4 | `25` | −24, −12, −12 | 0xdc | `b7` | −37 | 296 |

All weights are still 0, so each prediction is the previous output
byte. In step 3, D2 = LD − old D1 = −12 − 0. The first adaptation that
can change a weight happens in step 33.

### 19.7 A queue over time

Vector S2 (section 20.4), 420 bytes of raw output:

| At G | Event | Queue afterwards (s, L) |
|--:|---|---|
| 0 | record: new program 0 (delta), offset 0, L = 20, N = 3 | (0, 20) |
| 0 | record: current program, offset 30, no length (20 remembered), N = 1 | (0, 20) (30, 20) |
| 0 | record: new program 1 (audio), offset 60, L = 40, N = 2 | (0, 20) (30, 20) (60, 40) |
| 20 | block 0..19 complete: delta, F = 0; delivered | (30, 20) (60, 40) |
| 30 | raw bytes 20..29 have been delivered as they came | |
| 50 | block 30..49 complete: delta, N = 1; delivered | (60, 40) |
| 100 | block 60..99 complete: audio; delivered | empty |
| 100 | record: program number field 1 (program 0, delta), flag 0x40, offset 2, L = 30, N = 5: s = 100 + 2 + 258 = 360 | (360, 30) |
| 100 | record: new program 2 (RGB), offset 300, L = 18, W = 6, R = 1: s = 400 | (360, 30) (400, 18) |
| 390 | raw bytes 100..359 delivered unchanged; block 360..389: delta | (400, 18) |
| 418 | block 400..417: RGB | empty |
| 420 | member ends; bytes 418, 419 delivered unchanged | |

The use counters are 0, 1, 0, 2, 0 for the five records.

## 20. Test vectors

How they were produced: the expected results come from a throw-away
experimental decoder written from this document (kept outside the
repository). Unless an entry says otherwise each vector was also given
to `unar` 1.10.7, and those that use only the E8, E8/E9, delta, RGB and
audio programs in an LZ stream also to `bsdtar` 3.5.3, inside a minimal
archive (unpack version 29, method `0x33`), and the tools' output was
identical to the expected result. (`bsdtar` did not extract the second
member of the solid archives built for S4 and S6, so those members rest
on `unar` alone.) All vectors were run a second time, with an
independently written second experimental decoder and both tools, when
this document was verified (section 25).

Long byte strings are written as continuous hexadecimal digits.

### 20.1 The standard programs

**x86 E8** (53 bytes, CRC-32 `0xAD576887`, check byte `97`, 28 instructions):

```
971b012807069808000000d13a101592ec50cb9920b925f0291915530312ae511035592b6004156d4066ab0234490436
02523e9700
```

**x86 E8/E9** (57 bytes, CRC-32 `0x3CD7E57E`, check byte `84`, 30 instructions):

```
841b0128111069808000000d13a101c689d280ac9762855cc905c92f8148c8aa981895728881aac95b0020ab6a033558
11a24821b01291f4b8
```

**Itanium** (120 bytes, CRC-32 `0x3769893F`, check byte `46`, 45 instructions, 16 bytes of constant data):

```
469e08080c0c00000e0e08080000080800006c115a04ac0cc4cc5c0818462408f9a0442512124585990c140026255899
9003381a08dc02300c4ed11d89a1e2d0551133608c5a2306de0618007ffffc4dcc1917b306c444b2325a44c4a601f424
888338ccc4110987a6e04602b22403e2a032548352c5b170
```

**delta** (29 bytes, CRC-32 `0x0E06077D`, check byte `2f`, 16 instructions):

```
2f019a4180ec27482f09766dd3ea415b5944e8175ce16c914c4e3f7700
```

**RGB** (149 bytes, CRC-32 `0x1C2C5DC8`, check byte `c5`, 60 instructions):

```
c5019a4195c9a64dba4b140af49b804c0015a6a807262ac9c48b8662320f8664240666711998cc433331990066883330
ccd10e980b3334400cd14666199a28cc4980b3334500cd18666199a30cc8980b3334604cd10668a52062668833462805
0f320c4cd14668c50041e48fc8855e027cc9268183b09dc2de9c78acd668b40e71dbb249386e022a2c412b1098824903
14f4e19700
```

**audio** (216 bytes, CRC-32 `0xBC85E701`, check byte `47`, 105 instructions):

```
47019a4195e5720dc26482749324b14006d8384400a8013411dca1ba01990cc4033119a4066622604d9a400d668e60d0
30401826c1c8f6e62613789208e850bc5a07c6e9f520a9a0ed3733473966907019a39bcf258380c1bd30166e23349381
1609b050183b4dc84c059b88c528e0769390980b37118a59c480424843a947ee43346047d44a0dbbd359a486ee050940
26c9342476a0306a20ea022004a041509e503fe6e128944601bd8b40f0681136c9a1923811419ca89510ee50662b0020
9511040262ac668c6aca2640b2671b4b26cc648a6271a2b8
```

### 20.2 Standard programs on single blocks

Each row gives a block, the parameters, the file position F and the
result. For the rows marked "VM" the result was in addition obtained by
running the program's bytes on the experimental machine of Part III.
The tools were given X1, X2, X4, X5, I1, I2 (with the block at file
position F) and all D, P and A rows; X3, X6, X7, X8 and I3 to I6 were
checked on the machine only.

**x86.** Block X (36 bytes):

`90e800200000e800000001e8faffffffe800e0ffffe910000000e8e8ffffff00e8563412`

| # | Program | F | Result | CRC-32 |
|---|---|--:|---|---|
| X1 | E8 | 0 | `90e8fe1f0000e800000001e8faffff00e800e0ffffe910000000e8e8ffff0000e8563412` | `0x94581A12` |
| X2 | E8 | 0x100 | `90e8fe1e0000e800000001e8faffff00e800e0ffffe910000000e8e8ffff0000e8563412` | `0xA2AA8AE1` |
| X3 | E8 | 0xFFFFFFF0 | `90e80e200000e800000001e8faffffffe800e0ffffe910000000e8e8ffffff00e8563412` | `0x75202F31` |
| X4 | E8/E9 | 0 | `90e8fe1f0000e800000001e8faffff00e800e0ffffe9faffffffe8e8ffff0000e8563412` | `0x580DCE2B` |
| X5 | E8/E9 | 0x100 | `90e8fe1e0000e800000001e8faffff00e800e0ffffe9fafeffffe8e8ffff0000e8563412` | `0xB369875D` |
| X6 | E8/E9 | 0xFFFFFFF0 | `90e80e200000e800000001e8faffffffe800e0ffffe90a000000e8e8ffffff00e8563412` | `0xC1A5C9C6` |
| X7 | E8 | 0x100 | block `e810000000` (5 bytes) gives `e80fffffff` | |
| X8 | E8 | 0x100 | block `e8100000` (4 bytes) is unchanged | |

All VM. X3 and X6 exercise the wrap of P modulo 2^32 (P = 0xFFFFFFF2
for the first marker; for the marker at offset 11, P = 0xFFFFFFFC and
A + P is not below 0x80000000) and were checked on the machine only: a
file position that large cannot be reached with a small archive.

**Itanium.** Block I (53 bytes):

`304d1ab86556271711052e8cea4acc5dd60f3885895446c72cd32b4336f1ed56d1a4ce42cc55312a4f4aa8debbf5da59f05f16d89b`

| # | F | Result | CRC-32 |
|---|--:|---|---|
| I1 | 0x50 | `304d1ab86556271711052e8c9a4acc5dd60f2085895446972cd32b43d6f0ed56d1a4ce42cc55312a4f4aa8de4bf5da59f05f16d89b` | `0x3B9114F2` |
| I2 | 0 | `304d1ab86556271711052e8cea4acc5dd60f3485895446bf2cd32b4326f1ed56d1a4ce42cc55312a4f4aa8de9bf5da59f05f16d89b` | `0x11E52CF1` |
| I3 | 0x12345678 | `304d1ab86556271711052e8c7af4975dd60f986fbc54468701b92b43b69ab956d1a4ce42cc55312a4f4aa8de2b9fa659f05f16d89b` | `0xDAB4E373` |
| I4 | 0x50 | the first 21 bytes of block I: unchanged | |
| I5 | 0x50 | the first 22 bytes of block I: only byte 12 changes, to `9a` | |
| I6 | 0x50 | the first 37 bytes of block I: equal to the first 37 bytes of I1 | |

All VM; I1 and I2 also `unar` (`bsdtar` does not know this program).
Units 0, 1, 2 have t = 0, 6, 1 (masks 4, 7, 4); 53 bytes allow units 0,
1 and 2 (32 + 21 = 53).

**Delta.** Block D (20 bytes): `0724415e7b98b5d2ef0c294663809dbad7f4112e`

| # | N | Result | CRC-32 |
|---|--:|---|---|
| D1 | 3 | `f92e63d53fa99433d2360adebbc4cd23619f6ee1` | `0x1B071722` |
| D2 | 1 | `f9d59436bb236e9cada17832cf4fb2f8212d1cee` | `0x9F942202` |
| D3 | 31 | `f9dcbfa285684b2e11f4d7ba9d806346290cefd2` | `0x3E5B3A39` |
| D4 | 31 | the first 5 bytes of block D give `f9dcbfa285` | |

All VM. D3 and D4 have more channels than bytes.

**RGB.** Block P (30 bytes):
`052a4f74a4c9ee1343688db2e2072c5181a6cbf020456a8fbfe4092e5e83`

| # | L (first L bytes of P) | W | R | Result | CRC-32 |
|---|--:|--:|--:|---|---|
| P1 | 30 | 9 | 1 | `fb53e0b15c9b1d10313f7aa20c2572b720be3a7a99cb9d90c9c932fc1daf` | `0x69F0C195` |
| P2 | 30 | 6 | 0 | `6e735392c15c61df10e6d87a16ac8ffc5b5a8ddad0d434fcc669d36e7960` | `0x7B76B44D` |
| P3 | 30 | 30 | 2 | `fb73b1d1921d82613f0ee60c6a1684a1fcb2b38d96a0d4255dc65ff56ee7` | `0xF1F30D46` |
| P4 | 24 | 12 | 2 | `fbbd50d1265b824a1c0e24886a9e00a162e9200ab657f0d0` | `0x3D9E80A3` |
| P5 | 3 | 3 | 1 | `fb87b1` | `0x8E74C8B5` |
| P6 | 4 | 3 | 2 | `fbb162d6` | `0x8CC4A459` |
| P7 | 16 | 4 | 2 | `fb121fd1d09f8210240eb4546a83c8a1` | `0xB1FDAB8E` |
| P8 | 16 | 3 | 0 | `0d1260c3ed0b6ebdb624986ccf732237` | `0x47F1E179` |

All VM. P5 and P6 exercise the special case of step 2 (`bsdtar` differs
on these two, section 22). P7 and P8 use a stride that is not a
multiple of 3 or is 3 and depend on the "L zero bytes" rule; the tools
agree because the filter is the first of its group.

**Audio.** Block A is the 100 bytes whose byte i (i = 0..99) is
((i·i) div 7 + 3·i + 9·(i modulo 5)) modulo 256; it begins
`000c18253212202e3c4a2c3b4a5a6a4d` and ends `264d759dc5`. Block A4000
is the same formula for i = 0..3999.

| # | Block | N | Result | CRC-32 |
|---|---|--:|---|---|
| A1 | A | 1 | `00f4dcb785735325e99f7338ee942add801293038e076ec30561aadf000d33444026f3d4a257f27307850854a1024d7c8d81846f3cea791597fa5da3f52c413405db93258e0d9ed413332b25fdab2c80135299beb7ae8024981b68c2f901d7a64cbb5257` | `0xFBB1D2D4` |
| A2 | A | 2 | `0005f4f0dcc0b775850f73ba534925bce9139f4d739738c4eed394c42a97dd79803c12e0936503ca8e3c078e6ec0c3d105c161bdaa97df4f00e50d5933d74432406a2685f378d46fa24257ecf26c73c20739858e08b954b9a18d02614d0e7c8e8de18126` | `0x81E7CF8F` |
| A3 | A4000 | 1 | begins `00f4dcb785735325e99f7338ee942add`, ends `24533f0aebb4c503` | `0x4C62D028` |
| A4 | A4000 | 3 | begins `002c6ef4fcd6dc4238b7fec085304173`, ends `867290b2c8dfc10f` | `0x91254A41` |
| A5 | A4000 | 4 | begins `003f6c86f45490a7dc3f6c62b70000b7`, ends `8e942002092e24da` | `0x8E078AEC` |
| A6 | the first 3 bytes of A | 31 | `00f4e8` | |

All VM.

### 20.3 Virtual machine vectors

Every row is one program, run as in section 18 on the 48-byte block

`031425364758697a8b9cadbecfe0f102` followed by 32 zero bytes

with the default registers (R0 = R1 = R2 = 0, R3 = 0x3C000, R4 = 48,
R5 = 0), use counter 0 and F = 0. The expected result is the 48 bytes
at address 0, except for "output relocated". "Steps" is the number of
instructions executed, including the final return. Each row was run
through `unar` as the only filter of an archive; all agree.

| Name | Bytes | Program | Result | Steps |
|---|--:|---|---|--:|
| mov | 97 | `d85603b820c4488cd10107557799bbb58e8801d148ac0b7041889119a2060dddac74600e8e45601b81c0311223344049d55de66e85c09dac74800e9238159c00000006e0a040588801c431122334403907557799ba0499a1f1b58e9401d2c712e0` | `dd332211ddccbbaaddccbbaacfe0f10201000000ddccbbaa02000000dd00000000000000dd33221100000080ddccbbaa` | 33 |
| cmp | 79 | `f85601b82014500fb58e8801d148ac07703800e090713813b58e8c01d1c702b00dc14080b1100388700000000072004493343e36b1d2003a48e25601b81c02010504901038dc09dac74a00e9638170` | `050100000000008001000000cfe0f1020100008005000000020000000700000000000000000000800100008005010000` | 34 |
| add | 76 | `b05601b82043fc9003b58e8801d148ac07703806fffffffe090123813b58e8c01d1c702b00dc10403e0580836b1d2003a4915806e0a040588801c417003902405499a1f1b58e9401d2c712e0` | `000000809000000020000000cfe0f1020300000000000000000000800000008001000000100000000000008090000000` | 33 |
| sub | 66 | `c25601b82014d00fb58e8801d148ac07703800e090733813b58e8c01d1c702b00dc0e0100828248081cee04ed63a4007491c0ac03704100c01e010dac74a00e9645c` | `fe0100004758697a8b9cadbecfe0f10201000080feffffff020000000000000001000080fe0100000000000070000000` | 31 |
| xor | 94 | `7b5603b820fc3c3c3c2141b87878786d63a20074522b01dc0e01091a024848d213813b58e8c01d1c702b00dc0e0100ff824809fe1b813b58e9001d24702b00dc14080b11003887fe01fe000720c03fc03fe124cd0f8dac74a00e96389700` | `80010000ffffffffff00ff00cfe0f10200000080000000800200000000000000000000008001000000000080ffffffff` | 34 |
| and | 94 | `745603b820fc3c3c3c2241c7ffffffed63a20074522b01dc0e01091a0248405223813b58e8c01d1c702b00dc0e0100ff82480a022b813b58e9001d24702b00dc14080b11003887fe01fe000720c03fc03fe224cd0f8dac74a00e96389700` | `8001000000000000ff00ff00cfe0f10200000080f0f0f080000000001410000000000000800100000200000000000000` | 34 |
| or | 87 | `455603b820e000000023400ed63a20074522b01dc0e0000240233813b58e8c01d1c702b00dc0e01008002480a023b813b58e9001d24702b00dc14080b11003887fe01fe000720c03fc03fe324cd0f8dac74a00e9638970` | `80010000ffffffffff00ff00cfe0f10200000080010000800200000000000000000000008001000000000080ffffffff` | 34 |
| test | 94 | `655603b820fc3c3c3c2441c7ffffffed63a20074522b01dc0e01091a0249000243813b58e8c01d1c702b00dc0e0100ff82480a024b813b58e9001d24702b00dc14080b11003887fe01fe000720c03fc03fe424cd0f8dac74a00e96389700` | `ff01000000ff00ffff00ff00cfe0f10200000080f0f0f0f0020000003412000000000000ff0100000200000000ff00ff` | 34 |
| shl | 71 | `485601b820f000000070400ed63a20074522b00dc0e01a00000000240b03813b58e8c01d1c702b00dc1062468acf18204936b1d2003a4915806e0820181c3000ed63a50074b22e` | `000000004758697a8b9cadbecfe0f1020100008002000080030000000000000001000000806745230000000002000000` | 30 |
| shl count 0, 32 | 70 | `215601b820e0000000704006d63a20074522b00dc107000000038204836b1d1803a3915806e0700e00000008120181c09dac74800e923815806e08155c3004ed63a50074b22e` | `020000804758697a8b9cadbecfe0f1020100008001000080010000800100008000000080020000800000000000000000` | 30 |
| shr | 71 | `175601b820e0000000f1400ed63a20074522b00dc0e01c00000018240b13813b58e8c01d1c702b00dc1062468acf18a04936b1d2003a4915806e0820181c7000ed63a50074b22e` | `000000204758697a8b9cadbecfe0f1020100000001000040010000000000002001000000674523010100000040000000` | 30 |
| shr count 0, 32 | 67 | `245601b820e0000000714006d63a20074522b00dc106800000038a04836b1d1803a3915806e070010120789c09dac74800e923815806e081aac70046d63a50074b22e0` | `000000004758697a8b9cadbecfe0f1020100008001000080000000000100004003000000000000000300000000000000` | 30 |
| sar | 71 | `795601b820e0000000f2400ed63a20074522b00dc0e01c00000018240b23813b58e8c01d1c702b00dc107e468acf19204936b1d2003a4915806e0820181cb000ed63a50074b22e` | `000000e04758697a8b9cadbecfe0f10201000080010000c001000080000000e001000080674523ff0100000040000000` | 30 |
| mul | 84 | `7e5603b820c00040007b4180008001ed63a20074522b02dc0e0087f82443ffb3813b58e8c01d1c702b380000000dc1040221de011dac74800e9245601b828101622007101c0e40376499a1f1b58e9401d2c712e0` | `010000002a00000006000000cfe0f102010000000300040002000000010000000000008010000000000000002a000000` | 33 |
| div | 81 | `035603b820593c403ed63a20074522b02dc0e0087f82403c3813b58e8c01d1c702b380000000dc1040391e600adac74800e9245601b82810162200710e000000000e4087ff8499a1f1b58e9401d2c712e0` | `ffffffff00000000ffffffffcfe0f102010000000e00000002000000ffffffff00000080140000000000000000000000` | 33 |
| adc | 77 | `c85603b82043ffd4006d63a20074522b01dc0e00282443ffd3813b58e8c01d1c702b00dc14080b11003886fffffffe072007d24cd0f8dac74800e92389580ee08201fff70006d63a50074b22e0` | `050000000000008001000000cfe0f1020300000000000000010000000500000000000080000000800300000000000000` | 33 |
| adc byte | 76 | `ab5603b820603dc0ffb58e8801d148ac037038030009190f6e04ed63a3007471c0ac077050202c4400e20bf81c800f693343e36b1d2003a48e256700000003b820443dc1ffb58e9401d2c8b8` | `100000008000000000000000cfe0f1020300000000000000010000001000000000000000800000000100000010000000` | 33 |
| sbb | 74 | `b25603b82003e4006d63a20074522b01dc0e00282443ffe3813b58e8c01d1c702b00dc14080b1100388700000000072007e24cd0f8dac74800e92389580ee0805f9009b58e9401d2c8b8` | `05000000ffffff7f01000000cfe0f10201000080ffffffff010000000500000000000000ffffff7f0200000000000000` | 33 |
| sbb byte | 74 | `725603b82080403ec001b58e8801d148ac037038030009190fae04ed63a3007471c0ac077050202c4400e20bf81c817efa93343e36b1d2003a48e25603b820443ec1ffb58e9401d2c8b8` | `f0000000000000007e000000cfe0f10201000000ff00000001000000f000000002000000000000000100000010000000` | 33 |
| movzx | 95 | `865603b81c0311223344049d55de66ef0e04ed63a2007451c0ac0b7050202c4400e21889119a201c83aabbcc7de126687c6d63a3007471c4ac037041889119a27107557799bbb58e9001d248ac03703800009180e1c09dac74a00e96381700` | `800000007d0000007dccbbaacfe0f10201000000dd000000020000007d00000000000000ddccbbaa0000000080000000` | 34 |
| movsx | 95 | `a45603b81c0311223344049d55de66ef2e04ed63a2007451c0ac0b7050202c4400e21889119a201c83aabbcc7de526687c6d63a3007471c4ac037041889119a27307557799bbb58e9001d248ac03703800009180e5c09dac74a00e96381700` | `80ffffff7d0000007dccbbaacfe0f10201000000ddffffff020000007d00000000000000ddffffff0000000080ffffff` | 34 |
| xchg | 37 | `ae020c4488cd100e01d55de66ed607bf487036b1d1003a29009010501749a03a31207472ae` | `443322114758697a8b9cadbecfe0f10203000000ddccbbaa020000000100000000000000000000000000000000000000` | 14 |
| inc | 65 | `975603b82043fd91b58e8801d148ac07703806fffffffe0a00625b58e8c01d1c702b01dc10403fed8dac74800e9245603b81c02017f0500352dac74a00e9638170` | `800100004758697a8b9cadbecfe0f1020200000000000000000000800000008002000000000000000000000080010000` | 31 |
| dec | 61 | `f65603b82001d1b58e8801d148ac0770380020a00725b58e8c01d1c702b01dc1040200f8dac74800e9245603b81c02010105003d2dac74a00e96381700` | `000100004758697a8b9cadbecfe0f10200000080ffffffff020000000000000000000000ff0000000200000000010000` | 31 |
| not | 71 | `cb5607b820c3c3c3c3ef46d63a20074522b380000000dc0e00002802f25b58e8c01d1c702b01dc10403e17e36b1d2003a4915806e0700c48d159e014017d2dac74a00e96381700` | `875634124758697a8b9cadbecfe0f10203000000f0f0f0f000000080ffffffff010000000f0000000000000087563412` | 31 |
| neg | 60 | `a85603b82003346d63a20074522b00dc0e00082803325b58e8c01d1c702b00dc107000000019a36b1d2003a4915806e0820105cf1b58e9401d2c8b80` | `ffffffff4758697a8b9cadbecfe0f102020000000000000001000080ffffffff010000800000008001000080fb000000` | 30 |
| neg byte memory | 63 | `715603b81c0201000500674b6b1d1003a28e05601b81c0201ff0500674b6b1d1803a38e05601b820806033c6d63a40074922b00dc10019e36b1d2803a59170` | `010100004758697a8b9cadbecfe0f1020200000000010000010000800101000001000080800000000200000000000000` | 31 |
| push pop | 44 | `2956795fc00035f81d10fb22c74500e8c401d1cf0498001fff801137766554407cd9e07483c1e60008000170` | `031425364758697a8b9cadbecfe0f102f8ff03000100fecaf8ff03000000040048556677000000000000000000000000` | 14 |
| pusha popa | 51 | `2b020440122220a1120708b6807443c0e8a37000e8c37100e8e374500e90374700dd18199040002803503a492074a200e967dc` | `031425364758697a8b9cadbecfe0f102e0ff030000000400160000001200000010000000990000001000000000000400` | 18 |
| pushf popf | 51 | `965621ffbed63a2084020c17450019c804202e8a80329008505d16004511b58e8c566fffffff9bffb58e8e52402281745c01b8` | `031425364758697a8b9cadbecfe0f102ffffffff0000000002000000fcffff7f00000000000000000000000000000000` | 17 |
| call ret | 33 | `305a40208174441103a31f701d145ed201080ba24115c1744c3303a39e074817b8` | `031425364758697a8b9cadbecfe0f102001122330100000000000400f8ff030006000000000000000000000000000000` | 12 |
| computed jump, no final ret | 21 | `20020100e00341017440ee81c00ba227705d122a80` | `060000004758697a8b9cadbecfe0f1020000aa0000000000000000000000000000000000000000000000000000000000` | 6 |
| memory edges | 78 | `2801f0003fffe3a1b2c3d403a20f8001ffff01d147c000ffff0126000ffffc0746110408780182586aaccef1007c00100080c04080c101e0559889119a201d24f03a50f8001fffe03cc00100002e` | `031425364758697a8b9cadbecfe0f102d4c3b2a10000d4c3d4c3b2a18877665504030201feffffff0000443300000000` | 14 |
| static data | 33 | `7e90a6a882a8928642433207441f0003c04003a28f8001e02205d187c000f0122e` | `031425364758697a8b9cadbecfe0f1025354415449432121990000000000000000000000000000000000000000000000` | 4 |
| output relocated | 32 | `ca01f0003c020201000400058402010c8140984e402040780441f0003c01cb80` | `000102030405060708090a0b0c0d0e0f300000001415161718191a1b1c1d1e1f202122232425262728292a2b2c2d2e2f` | 196 |
| conditional jumps | 790 | below | `33cc0ff05511eeaa009cadbecfe0f1020000000000000000000000000000000000000000000000000000000000000000` | 252 |
| relative jump forms | 187 | below | `aa142536370000008b9cadbecfe0f1020000000000000000000000000000000000000000000000000000000000000000` | 14 |

Program "conditional jumps" (790 bytes):

```
6701c000038800172000ac0374201078ee0003580ee8402171dc000ab02dd08043e3b80025607ba10089c770008ace00
00000374201178ee002159c0000000ee8402371dc0082b380000002dd08047e3b802056700000007ba10091c770080ac
0375201278ee0803580eea402571dc100ab02dd4804be3b82025607ba90099c770408ace0000000375201378ee082159
c0000000eea402771dc1082b380000002dd4804fe3b822056700000007ba900a1c770480ac037948051e3b8400d603bc
a402971dc200ab02de52014f8ee1009581ef2900a9c770808ace000000037948055e3b840856700000003bca402b71dc
2082b380000002de52015f8ee108159c0000001ef2900b1c770880ac037988059e3b8600d603bcc402d71dc300ab02de
62016f8ee1809581ef3100b9c770c08ace00000003798805de3b860856700000003bcc402f71dc3082b380000002de62
017f8ee188159c0000001ef3100c1c770c80ac0379c8061e3b8800d603bce403171dc400ab02de72018f8ee2009581ef
3900c9c771008ace0000000379c8065e3b880856700000003bce403371dc4082b380000002de72019f8ee208159c0000
001ef3900d1c771080ac037a08069e3b8a00d603bd0403571dc500ab02de8201af8ee2809581ef4100d9c771408ace00
0000037a0806de3b8a0856700000003bd0403771dc5082b380000002de8201bf8ee288159c0000001ef4100e1c771480
ac037a48071e3b8c00d603bd2403971dc600ab02de9201cf8ee3009581ef4900e9c771808ace000000037a48075e3b8c
0856700000003bd2403b71dc6082b380000002de9201df8ee308159c0000001ef4900f1c771880ac037a88079e3b8e00
d603bd4403d71dc700ab02dea201ef8ee3809581ef5100f9c771c08ace000000037a8807de3b8e0856700000003bd440
3f71dc7082b380000002dea201ff8ee388159c0000001ef510101c771c80ac037808081e3b9000d603bc0404171dc800
ab02de02020f8ee4009581ef010109c772008ace000000037808085e3b900856700000003bc0404371dc8082b3800000
02de02021f8ee408159c0000001ef010111c772080b8
```

Program "relative jump forms" (187 bytes):

```
934023ca041704ee80804a02e09dc1704ee0b827705c13b82e09dc1704ee0b827705c13b82e09dc1704ee0b8277403f4
1704ee0b827705c13b82e09dc1704ee0b827740121704ee0b827705c13b82e09dc1704ee0b827705c13b82e09dc1704e
e0b827705c13b82e09dc1704ee0b827705c13b82e09dc1704eea84402e09dc1704ee0b827705c13b82e09dd01009e4c0
f00785c13b82e09dc1704ee0b827705c13bad080b827705c13b82e09dc1704ee0b827705c02aac80389170
```

How the rows from "mov" to "neg byte memory" except "xchg" (26 rows)
are built, to help locate a failure. Each holds four sub-tests; sub-test k (k = 0..3) leaves the **flags word** at
offset 16 + 8·k and the **result** at offset 20 + 8·k of the block.
Before each sub-test the flags word is loaded with a chosen value (by
pushing it and executing popf). The table lists, per sub-test: the
flags loaded (`–` for 0), the operand values, `d` for the 32-bit form
or `b` for the byte form, and a letter for the operand kinds:

- **A**: R0 is loaded with the first value (32-bit); the instruction
  has R0 and an immediate (in the byte form the immediate is the low 8
  bits of the listed value); the result is R0.
- **B**: the first value is written to the four bytes at address 0 and
  the second to R1; the instruction has the absolute memory operand 0
  and R1 (for one-operand instructions: memory at R2, with R2 = 0); the
  result is the 32-bit value at address 0.
- **C**: R2 = 4, R3 = 16, the first value is written at address 4 and
  the second at address 8; the instruction has "memory at R2" and
  "memory at R3 + 0xFFFFFFF8" (that is, address 8); the result is the
  32-bit value at address 4.

| Row | Sub-tests 0 to 3 |
|---|---|
| mov | C, 0x11223344, 0xAABBCCDD, d, A; Z, 0x11223344, 0xAABBCCDD, b, A; –, 0x11223344, 0xAABBCCDD, b, B; S, 0x11223344, 0xAABBCCDD, d, C |
| cmp | –, 0x5, 0x7, d, A; C, 0x7, 0x7, d, B; –, 0x80000000, 0x1, d, C; –, 0x105, 0x207, b, B |
| add | –, 0xFFFFFFFF, 0x1, d, A; C, 0x7FFFFFFF, 0x1, d, B; –, 0x1F0, 0x20, b, A; –, 0x70, 0x20, b, C |
| sub | –, 0x5, 0x7, d, A; C, 0x7, 0x7, d, B; –, 0x105, 0x207, b, B; –, 0x180, 0x10, b, A |
| xor | C, 0xF0F0F0F0, 0x70F0F0F0, d, A; C, 0x1234, 0x1234, d, B; –, 0x1FF, 0x27F, b, B; –, 0xFF00FF00, 0xFF00FF, d, C |
| and | C, 0xF0F0F0F0, 0x8FFFFFFF, d, A; C, 0x1234, 0x1014, d, B; –, 0x1FF, 0x280, b, B; –, 0xFF00FF00, 0xFF00FF, d, C |
| or | C, 0x80000000, 0x1, d, A; C, 0x0, 0x0, d, B; –, 0x100, 0x280, b, B; –, 0xFF00FF00, 0xFF00FF, d, C |
| test | C, 0xF0F0F0F0, 0x8FFFFFFF, d, A; C, 0x1234, 0x4000, d, B; –, 0x1FF, 0x280, b, B; –, 0xFF00FF00, 0xFF00FF, d, C |
| shl | –, 0xC0000001, 0x1, d, A; –, 0x40000000, 0x2, d, B; –, 0x12345678, 0x24, d, A; –, 0x181, 0x1, b, A |
| shl count 0, 32 | –, 0x80000001, 0x0, d, A; –, 0x80000001, 0x20, d, A; –, 0x80000002, 0x0, d, B; –, 0x55, 0x9, b, A |
| shr | –, 0x80000003, 0x1, d, A; –, 0x80000003, 0x2, d, B; –, 0x12345678, 0x24, d, A; –, 0x181, 0x1, b, A |
| shr count 0, 32 | –, 0x80000001, 0x0, d, A; –, 0x40000001, 0x20, d, A; –, 0x4, 0x3, d, B; –, 0xAA, 0x8, b, A |
| sar | –, 0x80000003, 0x1, d, A; –, 0x80000003, 0x2, d, B; –, 0xF2345678, 0x24, d, A; –, 0x181, 0x1, b, A |
| mul | C, 0x10001, 0x10003, d, A; Z, 0xFFFFFFFF, 0xFFFFFFFF, d, B; S, 0x110, 0x211, b, A; –, 0x7, 0x6, d, C |
| div | C, 0x64, 0x7, d, A; Z, 0xFFFFFFFF, 0x0, d, B; S, 0x1C8, 0x10A, b, A; –, 0x80000000, 0xFFFFFFFF, d, C |
| adc | C, 0xFFFFFFFF, 0x0, d, A; C, 0x5, 0xFFFFFFFF, d, B; –, 0x7FFFFFFF, 0x1, d, C; C, 0x1FF, 0x100, b, A |
| adc byte | C, 0x80, 0x7F, b, A; –, 0x80, 0x90, b, B; C, 0x7F, 0x0, b, C; CS, 0x10, 0xFF, b, A |
| sbb | C, 0x0, 0x0, d, A; C, 0x5, 0xFFFFFFFF, d, B; –, 0x80000000, 0x1, d, C; C, 0x5, 0x4, d, A |
| sbb byte | C, 0x100, 0x0, b, A; –, 0x80, 0x90, b, B; C, 0x7F, 0x7E, b, C; C, 0x10, 0xFF, b, A |
| movzx | C, 0x11223344, 0xAABBCCDD, d, B; Z, 0x11223344, 0xAABBCC7D, d, C; –, 0x11223344, 0xAABBCCDD, d, A; –, 0x0, 0x80, d, B |
| movsx | C, 0x11223344, 0xAABBCCDD, d, B; Z, 0x11223344, 0xAABBCC7D, d, C; –, 0x11223344, 0xAABBCCDD, d, A; –, 0x0, 0x80, d, B |
| inc | C, 0xFFFFFFFF, d, A; C, 0x7FFFFFFF, d, B; C, 0x1FF, b, A; C, 0x17F, b, B |
| dec | C, 0x0, d, A; C, 0x1, d, B; C, 0x100, b, A; C, 0x101, b, B |
| not | CZ, 0xF0F0F0F, d, A; S, 0x0, d, B; C, 0x1F0, b, A; –, 0x12345678, b, B |
| neg | C, 0x0, d, A; –, 0x1, d, B; –, 0x80000000, d, A; –, 0x105, b, A |
| neg byte memory | C, 0x100, b, B; –, 0x1FF, b, B; –, 0x180, b, A; –, 0x0, b, A |

The remaining rows:

- **xchg**: R0 = 0x11223344 and the value 0xAABBCCDD at address 0 are
  exchanged with C and Z loaded; flags word at 16, R0 at 20; then R1 = 1
  and R2 = 2 are exchanged and stored at 24 and 28.
- **push pop**, **pusha popa**, **pushf popf**, **call ret**: sequences
  of the stack instructions of section 17.4 that store R7 and stack
  contents into the block. In "pushf popf" the flags word 0xFFFFFFFF is
  loaded and stored back unchanged (offset 16), three conditional jumps
  on it are taken, a cmp of equal values gives the word 2 (offset 24),
  and the word 0x7FFFFFFC survives a print (offset 28) and lets ja
  jump.
- **computed jump, no final ret**: a jump through a register and one
  through memory; the program does not end in a return, so one is
  appended.
- **memory edges**: 32-bit accesses at 0x3FFFE (the extra bytes), an
  address above 0x40000 through a register, an address formed by a
  wrapping sum, an absolute address with bits above bit 17, and a push
  with R7 = 2.
- **static data**: 9 bytes of constant data read back from 0x3C040.
- **output relocated**: stores 0x100 at 0x3C020 and writes the bytes
  0..47 at 0x100, then the 32-bit value found at 0x3C01C (48) at
  0x110; the result is taken from address 0x100.
- **conditional jumps**: bytes 0..8 of the result are, for jz, jnz, js,
  jns, jb, jbe, ja, jae and jmp in this order, a mask of the flag
  combinations for which the jump was **not** taken: bit j stands for
  the j-th of the flags words 0, C, Z, C+Z, S, S+C, S+Z, S+C+Z.
- **relative jump forms**: a chain of jumps using the immediate numbers
  30, 9, 250, 2, 296, 16, 15, 7, a call with 8, and 316 (instruction
  indexes 0, 22, 15, 1, 3, 40, 48, 47, 54, 46 in the order executed).
  Reaching the end stores 0xAA at offset 0 and the call's return index
  (55) at offset 4; any wrong target stores 0xEE at offset 1.

### 20.4 Whole streams

Each vector gives the packed bytes of a member (unpack version 29), its
unpacked size U, and the delivered output. The raw output can be
recovered from the description, so that an error can be attributed to
the LZ or PPMd layer or to the filter layer. "D(a..b)" stands for the
bytes whose value at index i (i = a..b − 1) is (7·i + (i div 8)) modulo
256. "X" is block X of section 20.2. The LZ streams consist of one
table transmission, literals, filter records and the end-of-member
marker.

**S1: one x86 filter, LZ stream.**

Raw output: D(0..12), X, D(50..62). One record at G = 0: new program 0 (E8), offset 12, L = 36. The filter runs at G = 48 with F = 12 (section 19.4).

Record as carried:

- at G = 0 (59 bytes):

  ```
  a63200c49135971b012807069808000000d13a101592ec50cb9920b925f0291915530312ae511035592b6004156d4066
  ab023449043602523e9700
  ```

Packed bytes (132), U = 60:

```
0c199987e493c493666c9a0d934db229a69a8e049a69a69a84934d34dc894f9c151f5ff7faba98c8031244d65c6c04a0
1c1a602000000344e840564bb1432e6482e497c0a464554c0c4ab94440d564ad801055b5019aac08d12410d80948fa5c
007e08e49a7a2aaecb766250601e6f924c7897b080cc921aea62bf0c72cf4d77e39ebbfd
```

Delivered output (CRC-32 `0x0BCD453F`; the raw output has CRC-32 `0x0A1FCC8B`):

```
00070e151c232a313940474e90e8f21f0000e800000001e8faffff00e800e0ffffe910000000e8e8ffff0000e8563412
646b727980878f969da4abb2
```

Checked with: `unar`, `bsdtar`.

**S2: three programs, reuse, remembered length, the 258 flag, LZ stream.**

Raw output: D(0..420). Five records, traced in section 19.7; the third, fourth and fifth define or select programs by number, the second and fourth rely on the current program and the remembered length.

Records as carried:

- at G = 0: `b61c0004500868e9780cd20c07613a41784bb36e9f520adaca2740bae70b648a6271fbb800`
- at G = 0: `12478082`
- at G = 0 (225 bytes):

  ```
  b6d8093c4a0084ec2380cd20caf2b906e132413a499258a0036c1c220054009a08ee50dd00cc866201988cd203331130
  26cd2006b347306818200c1360e47b731309bc490474285e2d03e374fa9054d0769b99a39cb348380cd1cde792c1c060
  de980b37119a49c08b04d8280c1da6e42602cdc46294703b49c84c059b88c52ce240212421d4a3f7219a3023ea2506dd
  e9acd243770284a013649a123b50183510750110025020a84f281ff370944a2300dec5a07834089b64d0c91c08a0ce54
  4a887728331580104a888201315633463565132059338da5936632453138d15c00
  ```
- at G = 100: `f404247808a0`
- at G = 100 (159 bytes):

  ```
  b6960e012c44818c0b2b8a0334832b934c9b74962815e9370098002b4d500e4c559389170cc4641f0cc8480ccce23331
  988666633200cd10666199a21d301666688019a28ccc33345198930166668a019a30ccc33346199130166668c099a20c
  d14a40c4cd10668c500a1e641899a28cd18a0083c91f910abc04f9924d0307613b85bd38f159acd1681ce3b7649270dc
  0454588256213104920629e9c32e00
  ```

Packed bytes (900), U = 420:

```
0bcd4c7dd500808080800000020202020200000008080808080000002020202020202021b6adb6cb6db2db6cb6db27bb
ffe816c380008a010d1d2f019a4180ec27482f09766dd3ea415b5944e8175ce16c914c4e3f7700000923c041016db012
78940109d847019a4195e5720dc26482749324b14006d8384400a8013411dca1ba01990cc4033119a4066622604d9a40
0d668e60d030401826c1c8f6e62613789208e850bc5a07c6e9f520a9a0ed3733473966907019a39bcf258380c1bd3016
6e233493811609b050183b4dc84c059b88c528e0769390980b37118a59c480424843a947ee43346047d44a0dbbd359a4
86ee05094026c9342476a0306a20ea022004a041509e503fe6e128944601bd8b40f0681136c9a1923811419ca89510ee
50662b00209511040262ac668c6aca2640b2671b4b26cc648a6271a2b80064707c8894a2b0beccd8e4f0fd0b19273541
4d596573818f9da9b5c1cdd9e5f01450f2864707c8896a4b2c0ccd8e4f0ff0d1b2935414d59677583919da9b5c1cfdbe
7f21450f2866727e8a96a4b2c0cedae6f2ff0d1b2937434f5b677583919fabb7c207a02123c0450016d2c1c025889031
816571406690657269936e92c502bd26e013000569aa01c98ab27122e1988c83e1990901999c46663310cccc664019a2
0ccc333443a602cccd100334519986668a3312602cccd140334619986668c3322602cccd181334419a29481899a20cd1
8a0143cc831334519a3140107923f22157809f3249a060ec2770b7a71e2b359a2d039c76ec924e1b808a8b104ac42620
9240c53d3865c01cfdbe7f2185902a66727e8a98a6b4c2cedae6f3010f1d2b37434f5b697785939fabb7c3d1dde9f418
5902a6874808c98a6b4c2d0dce8f5010f1d2b3945515d69778593a1adb9c5d1dde9f41c6112c6874808c9aa8b6c4d0dc
e8f503111f2d3945515d6b798795a1adb9c5d3dfea0838c2258d4ed051d35516d89a5bdd5ee06223e5a768ea6bed6f30
f2b475f778fa7bfd41081a48b9a9da0a3a72aae31b4b7babdc144c84bced1d4d7db5ee265e8ebeef1f5787b031034917
363c42484e555c636a70767c828990979ea4aab0b6bdc4cbd2d8dee4eaf0f60624713306c7884909eacbac8d4e0ecf90
71523313d4955616f7d8b99a5b1bdc9d7e3ee1048e2660dcf50d253d597591adc5ddf60e2a46627e96aec6defb17334f
677f97afc7dc27dfe7eff1b9ea1a4a82baf32b5b8bbbec245c94ccfd2d5d8dc5fe366ffa
```

Delivered output (CRC-32 `0xAC798647`; the raw output has CRC-32 `0x3BAFF543`):

```
00cf9df99633eb56c1d60f48bac1c8976c416d108e959ca3abb2b9c0c7ce2b4f6b808e95958e806b4e2affcd94540dbf
690c646b727980878f969da455c6a385ea3d2aee6298933bbdd7e06cfcf9117f1ffe267625e71d510eb4f810db64b7b1
8cf75a36c8cfd6dde5ecf3fa01080f161e252c333a41484f575e656c737a818890979ea5acb3bac1c9d0d7dee5ecf3fa
020910171e252c333b424950575e656c747b828990979ea5adb4bbc2c9d0d7dee6edf4fb020910171f262d343b424950
585f666d747b828991989fa6adb4bbc2cad1d8dfe6edf4fb030a11181f262d343c434a51585f666d757c838a91989fa6
aeb5bcc3cad1d8dfe7eef5fc030a111820272e353c434a515960676e757c838a9299a0a7aeb5bcc3cbd2d9e0e7eef5fc
040b121920272e353d444b525960676e767d848b9299a0a7afb6bdc4cbd2d9e0e8eff6fd040b121921282f363d444b52
5a61686f767d848b939aa1a8afb6bdc4ccd3dae1e8eff6fdfbd1a67b50ef9b45ef99dc5ddd5cdbc2186ec216a1ccf721
4a7979797977dae1e9f0f7fe050c131ade3d893e6c0b908c86d49efa0aa16631cbcba2a9
```

Checked with: `unar`, `bsdtar`.

**S3: records carried in a PPMd block.**

The member is a single PPMd block (model order 4, 2 MiB, default escape character) closed by the end-of-member code. Raw output: D(0..50), X, D(9..43). Records: at G = 0, new program 0 (delta), offset 10, L = 30, N = 2; at G = 20, new program 1 (E8/E9), offset 30, L = 36, so s = 50 and F = 50.

Records as carried:

- at G = 0: `b61c00a4780848e9780cd20c07613a41784bb36e9f520adaca2740bae70b648a6271fbb800`
- at G = 20 (64 bytes):

  ```
  a637091e49139841b0128111069808000000d13a101c689d280ac9762855cc905c92f8148c8aa981895728881aac95b0
  020ab6a03355811a24821b01291f4b80
  ```

Packed bytes (197), U = 120:

```
a30102058c4458707db6fe6e2f054afe9970f505d0433ae38722ced15372bf9dc1d9223c197dcb1c530b75e57207c418
aeb6a48453ddb98130fd1ae4dc354b7e86474267a637dbdb7d736cc85a9bf57d0f3ca75e481f1319bd356041b98320c9
f183b03eafd2eeae247963b4f6c051a129fa3b47b3cb1be2217e10d2dec49d06298b3e90f5716c7b3aaf736cfef378b5
a0471fc64d4f2957fa30069ed58a79a1f9a77536e392392708c89e2c293d2f2594bd62a966a84e6cc7bc65c4cbf15fb2
d7c3ed3f00
```

Delivered output (CRC-32 `0x4AD1FD39`; the raw output has CRC-32 `0xD8FE5337`):

```
00070e151c232a313940b94e6b9516d5ba0e5740ed6b7b8f02ab82c0fbce6dd5d8d53cce99c0eeab1d242b323940474e
565d90e8cc1f0000e800000001e8faffff00e800e0ffffe9c8ffffffe8e8ffff0000e856341240474e555c636a727980
878e959ca3abb2b9c0c7ced5dce4ebf2f900070e151d242b
```

Checked with: `unar`.

**S4: solid group of two members; a filter announced in one member for a block of the next, LZ streams.**

Member 0: raw output D(0..10), X, D(3..37). Records at G = 0: new program 0 (E8), offset 10, L = 36; new program 1 (delta), offset 90, L = 20, N = 2. The second block (group positions 90..109) lies in member 1; it has not started when member 0 ends at 80 and stays queued (section 8).

Records as carried:

- at G = 0 (59 bytes):

  ```
  a63200a49135971b012807069808000000d13a101592ec50cb9920b925f0291915530312ae511035592b6004156d4066
  ab023449043602523e9700
  ```
- at G = 0: `b61d095a4500848e9780cd20c07613a41784bb36e9f520adaca2740bae70b648a6271fbb8000`

Packed bytes (193), U = 80:

```
0c015547e4978926ccd9341b269b64534d34c9269a89a69a6c8a69a89a69a69a69a0821d64d323d7bffea8a63200a491
35971b012807069808000000d13a101592ec50cb9920b925f0291915530312ae511035592b6004156d4066ab02344904
3602523e970045b0e84ad228042474bc06690603b09d20bc25d9b74fa9056d6513a05d7385b2453138fddc00004d45d8
69b71e7ec20c8040048be33321c19f4a80088ccc146eacbb0d36e3cfc10c524b34f4535576dd7e18e59e9aedbf3df9e8
fd
```

Delivered output (CRC-32 `0xAB7FCAD2`; the raw output has CRC-32 `0x37DF3C92`):

```
00070e151c232a31394090e8f41f0000e800000001e8faffff00e800e0ffffe910000000e8e8ffff0000e8563412151c
232a313940474e555c636a727980878e959ca3abb2b9c0c7ced5dce4ebf2f900
```

Member 1 (continues the group, M = 80; its stream begins with a table transmission of its own): raw output D(100..140), X, D(77..81). The filter carried over from member 0 runs when group position 110 is reached: delta with N = 2 on the member's bytes 10..29, F = 10. One record at G = 100: program number field 1 (program 0, E8, defined in member 0; use counter 1), offset 20, no length field (36 remembered): s = 120, which passes the order check against the carried block's end, 110. The x86 filter runs with F = 40, not 120.

Record as carried:

- at G = 100: `a305144900`

Packed bytes (105), U = 80:

```
10019404fe09be4f4f6ed0d4dda19224249a69a64d34d34d34d44d34d34d34d3244891c8bbbcee4d347c7ffeb65d865a
efc72878f3f3ea29aabb0cb5dd1828a248038ebcfc10c51c92cd3d14d55db7e39c843f0040040b933321a19ede800446
6609b5bdff4945fa40
```

Delivered output (CRC-32 `0x7D149E4A`; the raw output has CRC-32 `0x8C86996E`):

```
c8cfd6dde5ecf3fa0108f1a9db4bbde6987a6c07398dff0cbe8476f4275d9ea5acb3bac1c9d0d7de90e8d61f0000e800
000001e8faffff00e800e0ffffe910000000e8e8ffff0000e8563412242b323a
```

Checked with: `unar` (both members), `bsdtar` (member 0).

**S5: a non-standard program that shows its inputs, LZ stream.**

Raw output: D(0..300). The program (40 bytes, CRC-32 `0xC5A49359`) stores R0..R7 at offsets 0..28 of its block and copies the 48 bytes at 0x3C000..0x3C02F to offsets 32..79. Three records, all met at G = 10: new program 0, offset 0, L = 80 (s = 10); current program, offset 100, register field with R0 = 7 and R6 = 0x1234 (s = 110); program number field 1, offset 200, L = 80, register field with R2 = 0xFFFFFFF0 and R5 = 0x99 (s = 210). In the delivered output the three 80-byte blocks show, among other things: R6 = F (10, 110, 210) regardless of the record, the record's R6 at system address 0x3C018, the use counters 0, 1, 2 at 0x3C02C while R5 holds 0x99 in the third, and R7 = 0x40000.

Records as carried:

- at G = 10 (46 bytes):

  ```
  a625000541286401c0803892072280e6581d10c03a29a0746380e8e7824002c520638001e00032452261390084dc
  ```
- at G = 10: `1559208f091a00`
- at G = 10: `b60105c8541221e0cc80`

Packed bytes (414), U = 300:

```
0fd04bdd12aadaaadaaadaaadaaadaaadaaadaaae1556d556d556d52da16d0b685b42e01c0380700e10b685b42da1700
e01c03807085b42da16d0b80700e01c0384fffd6b2c6daef03172b3f5366ad3128002a0943200e0401c49039140732c0
e88601d14d03a31c07473c12001629031c000f000192291309c80426e551559208f091a00ab6c020b90a82443c19901b
dc7cfd3d7da3489533dfe3e7eaa56b16beff3f7fb164cda359636d77838d97a1a9b3bdc604207143489532750234ca56
b16ae5e3f85164cda35b656f79838d97a1abb5bfc804207143891634750234ca97326ce5e3f851a54ea55b656f79858f
99a3abb5bfc808288163891634790a44ea97326ce9ec0871a54ea55d67717b858f99a3adb7c1ca08288163c99736790a
44ead7b36ee9ec0871e5cfa75d67717b87919ba5adb7c1ca0c309183c997367d12550ad7b36eedf41891e5cfa75f6973
7d87919ba5afb9c20018612308143070fa24aa163068e1dbe831244ca152bed2e6fb13273b4f5f738407475767710286
0e78797a7b2c60d1c7c7d7e7f489942a585add5fe264e769ec6ef1737fa0
```

Delivered output (CRC-32 `0x06E1D4F7`; the raw output has CRC-32 `0xB0F955AF`):

```
00070e151c232a31394000000000000000000000000000c0030050000000000000000a00000000000400000000000000
00000000000000c0030050000000000000000000000050000000000000000a000000000000000000000081888f969da4
acb3bac1c8cfd6dde5ecf3fa010807000000000000000000000000c0030050000000010000006e000000000004000700
0000000000000000000000c0030050000000010000003412000050000000000000006e00000000000000010000004950
585f666d747b828991989fa6adb4bbc2cad10000000000000000f0ffffff00c003005000000099000000d20000000000
04000000000000000000f0ffffff00c003005000000099000000000000005000000000000000d2000000000000000200
0000121920272e353d444b52
```

Checked with: `unar`.

**S6: solid group of two members; programs, use counters and remembered lengths carried over, LZ streams.**

Member 0: raw output D(0..10), X, D(3..37) (as in S4). Records at G = 0: new program 0 (E8), offset 10, L = 36; new program 1 (delta), offset 50, L = 20, N = 2. Both blocks lie in member 0 and run there (F = 10 and F = 50); the queue is empty when the member ends.

Records as carried:

- at G = 0 (59 bytes):

  ```
  a63200a49135971b012807069808000000d13a101592ec50cb9920b925f0291915530312ae511035592b6004156d4066
  ab023449043602523e9700
  ```
- at G = 0: `b61d09324500848e9780cd20c07613a41784bb36e9f520adaca2740bae70b648a6271fbb8000`

Packed bytes (193), U = 80:

```
0c015547e4978926ccd9341b269b64534d34c9269a89a69a6c8a69a89a69a69a69a0821d64d323d7bffea8a63200a491
35971b012807069808000000d13a101592ec50cb9920b925f0291915530312ae511035592b6004156d4066ab02344904
3602523e970045b0e8499228042474bc06690603b09d20bc25d9b74fa9056d6513a05d7385b2453138fddc00004d45d8
69b71e7ec20c8040048be33321c19f4a80088ccc146eacbb0d36e3cfc10c524b34f4535576dd7e18e59e9aedbf3df9e8
fd
```

Delivered output (CRC-32 `0x7592BF1A`; the raw output has CRC-32 `0x37DF3C92`):

```
00070e151c232a31394090e8f41f0000e800000001e8faffff00e800e0ffffe910000000e8e8ffff0000e8563412151c
232acf87960756800ff2c15d6cc1101ead7343c1d108c0c7ced5dce4ebf2f900
```

Member 1 (continues the group, M = 80): raw output D(100..140), X, D(77..101). Two records, both with a program number field and without a block length field: at G = 100, number 1 (program 0, E8; use counter 1; 36 remembered), offset 20: s = 120, F = 40; at G = 156, number 2 (program 1, delta; use counter 1; 20 remembered), offset 0, register field with R0 = 3 (flags `0x93`: the combination of 0x80 and 0x10 that real data never showed): s = 156, F = 76.

Records as carried:

- at G = 100: `810514`
- at G = 156: `9308002180`

Packed bytes (130), U = 100:

```
10019404fe09be4f4f6ed0d4dda1b4b696da49a69a6d2da5b4b69a69a69e48912269a69a6489123917779dc9a68f8fff
d659e9aeec31c90d5af76fe5d3b22926a69020a29e3d7cfcb2ec32d38f410c51c9351558d10e00100102c8ccc85a677e
c00111998257bedd24c200086038f3ea8a49a8af9f7f565d867fb6ebf0c51c92e680
```

Delivered output (CRC-32 `0xA67B6346`; the raw output has CRC-32 `0x7D01E4F1`):

```
c8cfd6dde5ecf3fa01080f161e252c333a41484f575e656c737a818890979ea5acb3bac1c9d0d7de90e8d61f0000e800
000001e8faffff00e800e0ffffe910000000e8e8ffff0000e8563412dcaa78b14de97fe953457eb6040b12bc91666d10
b3bac1c8
```

Checked with: `unar` (both members), `bsdtar` (member 0).

### 20.5 Records and streams a decoder must reject

For all of these the member has U = 100 and raw output D(0..100), and
the records are met at G = 0 in the order listed, before any raw
output. The record bytes are given as carried (flags byte, size
byte(s), payload). "Tools" says what the two tools do with the same
stream: "fail" means an error and no usable output. `bsdtar` fails on
every program it does not know, so for E14 to E20 only `unar` is
informative. E10 to E13, E21 and E22 are rejected by decisions of this
document; the tools accept them in one way or another, as stated.

| # | Records, in order | Why it is corrupt input | Section | Tools |
|---|---|---|---|---|
| E1 | `b61c00a4780848e9780cd20c07613a41784bb36e9f520adaca2740bae70b648a6271fbb800`; `344791402180` | second block starts inside the first (10..39, then 30..49) | 7.1 | both fail |
| E2 | `b61c00a4780848e9780cd20c07613a41784bb36e9f520adaca2740bae70b648a6271fbb800`; `12280860` | second filter for the same range 10..39 | 7.1 | both fail |
| E3 | `b61d01324500848e9780cd20c07613a41784bb36e9f520adaca2740bae70b648a6271fbb8000`; `12280860` | second block (10..29) lies before the first (50..69) | 7.1 | both fail |
| E4 | `b61d015a4500848e9780cd20c07613a41784bb36e9f520adaca2740bae70b648a6271fbb8000` | block 90..109 runs past the member's end at 100 | 8 | both fail |
| E5 | `a63500ac000f0005`..., 62 bytes, below | block length 0x3C001 | 5.3 | both fail |
| E6 | `3329140210` | flag 0x80 clear but no program was ever selected | 6.1 | both fail |
| E7 | `b61c00a4500848e9780cd20c07613a41784bb36e9f520adaca2740bae70b648a6271fbb800`; `b61d0d284500848e9780cd20c07613a41784bb36e9f520adaca2740bae70b648a6271fbb8000` | program number field 3 when one program is stored | 6.1 | both fail |
| E8 | `b61c00a4500848e9700cd20c07613a41784bb36e9f520adaca2740bae70b648a6271fbb800` | the delta program with a wrong check byte | 16.1 | both fail |
| E9 | `b61a00a4500848e9780cd20c07613a41784bb36e9f520adaca2740bae70b648a6271fb` | payload two bytes too short: the program bytes run past its end | 5.2 | both fail |
| E10 | `b61c00a4500808e9780cd20c07613a41784bb36e9f520adaca2740bae70b648a6271fbb800` | delta program with N = 0 | 12 | both deliver a zero-filled block |
| E11 | `b6d800a4500bfeec`..., 225 bytes, below | audio program with N = 255 | 14 | both deliver a result |
| E12 | `b69500a4501a2a03`..., 158 bytes, below | RGB program with W = 21 and L = 20 | 13 | `unar` delivers a result, `bsdtar` fails |
| E13 | `b61d00a4501840a3a5e03348301d84e905e12ecdba7d482b6b289d02eb9c2d922989c7eee000` | delta program, register field sets R1 = 1 | 9.2 | both ignore R1 and deliver a result |
| E14 | `a60300a450697404020b7000` | program: jump to index 5 of a 2-instruction program | 18.4 | both fail |
| E15 | `a60200a450520404020000` | program: a jump to itself | 18.4 | `unar` never returns; `bsdtar` fails (unknown program) |
| E16 | `a60100a45042800062e0` | program: mov with an immediate first operand | 16.4 | both fail |
| E17 | `a60600a45094905c10440e21891a20` | program whose last instruction is cut off | 16.2 | both fail |
| E18 | `a60900a450c7801f0003c02030003fff0b80` | program stores 0x3FFF0 at 0x3C020: A + L reaches past 0x40000 | 18.3 | `unar` cuts the output off at the block; `bsdtar` fails |
| E19 | `a60600a4509a601f0003c01c113b80` | program stores 19 at 0x3C01C: result length differs from L = 20 | 18.3 | both fail |
| E20 | `a60300a4506f056036005c00` | program: pop with an immediate operand | 16.4 | `unar` crashes; `bsdtar` fails |
| E21 | `b61c00a000848e9780cd20c07613a41784bb36e9f520adaca2740bae70b648a6271fbb8000` | delta program, block length field 0 | 5.3 | `unar` delivers the 10 bytes before the block and fails; `bsdtar` delivers the member unfiltered |
| E22 | `961b00a02123a5e03348301d84e905e12ecdba7d482b6b289d02eb9c2d922989c7eee000` | new delta program without a block length field: the remembered length is 0 | 5.3 | as E21 |

The long records in full:

E5 (62 bytes):

```
a63500ac000f000535971b012807069808000000d13a101592ec50cb9920b925f0291915530312ae511035592b600415
6d4066ab023449043602523e9700
```

E11 (225 bytes):

```
b6d800a4500bfeec2380cd20caf2b906e132413a499258a0036c1c220054009a08ee50dd00cc866201988cd203331130
26cd2006b347306818200c1360e47b731309bc490474285e2d03e374fa9054d0769b99a39cb348380cd1cde792c1c060
de980b37119a49c08b04d8280c1da6e42602cdc46294703b49c84c059b88c52ce240212421d4a3f7219a3023ea2506dd
e9acd243770284a013649a123b50183510750110025020a84f281ff370944a2300dec5a07834089b64d0c91c08a0ce54
4a887728331580104a888201315633463565132059338da5936632453138d15c00
```

E12 (158 bytes):

```
b69500a4501a2a032b8a0334832b934c9b74962815e9370098002b4d500e4c559389170cc4641f0cc8480ccce2333198
8666633200cd10666199a21d301666688019a28ccc33345198930166668a019a30ccc33346199130166668c099a20cd1
4a40c4cd10668c500a1e641899a28cd18a0083c91f910abc04f9924d0307613b85bd38f159acd1681ce3b7649270dc04
54588256213104920629e9c32e00
```

Four of them as complete packed LZ streams:

E1, packed bytes (163), U = 100:

```
3d04fe08da4d26934b2592c964da4d269348912244c9122448912244c9122448912244d34d34cffebcdb0e00523c0424
74bc06690603b09d20bc25d9b74fa9056d6513a05d7385b2453138fddc0072688f22804300002107e3cfaf641251661c
824a2cc34e3d04524d45565d865a6bb6e00210620a30e41251669e8a6a2cc34ebf0c724d4556df8e7a6bb6fd3af6ee20
a30ff3efefe69e8a6aaecb6ebf0c72cf4d7c20
```

E4, packed bytes (157), U = 100:

```
3d04fe08da4d26934b2592c964da4d269348912244c9122448912244c9122448912244d34d34cffebcdb0e80ad228042
474bc06690603b09d20bc25d9b74fa9056d6513a05d7385b2453138fddc000000841f8f3ebd90494598720928b30d38f
41149351559761969aedb8008418828c39049459a7a29a8b30d3afc31c935155b7e39e9aedbf4ebdbb8828c3fcfbfbf9
a7a29aabb2dbafc31cb3d35f08
```

E14, packed bytes (131), U = 100:

```
3d04fe08da4d26934b2592c964da4d269348912244c9122448912244c9122448912244d34d34cffebcd30180522834ba
020105b800000841f8f3ebd90494598720928b30d38f41149351559761969aedb8008418828c39049459a7a29a8b30d3
afc31c935155b7e39e9aedbf4ebdbb8828c3fcfbfbf9a7a29aabb2dbafc31cb3d35f08
```

E21, packed bytes (169), U = 100:

```
3cd04fe091a4d26934b2592c964da4d26934b2592244c9122448912244c9122448912244d34d34ddffea0b61c00a0008
48e9780cd20c07613a41784bb36e9f520adaca2740bae70b648a6271fbb800000922804000d11e4500840020c417b77f
1e4a2cc37f7f41251661a71e822926a2ab2ec32d35db708310518720928b30d38f45351661a75f863926a2ab6fc73d35
db7e5cfa75187209f5efe7d38f45355765b75f863967a6be10
```

In the E21 stream the record is followed by two more records (current
program, offset 20, no length field; current program, offset 30,
L = 20), which are never reached.

## 21. Safety requirements for untrusted input

Everything a record says is a claim. The bounds below are all that an
implementation needs; each is stated where it belongs and repeated
here.

1. **Records.** A record is at most 3 + 65,535 bytes and can be read
   into a buffer of that size. Every field read is bounded by the
   payload (section 5.2); in the LZ route every record byte costs 8 bits
   of packed input. A record produces no output, so limits on the
   output size do not bound the work spent on records: that work is
   bounded only by the packed input the LZ or PPMd layer consumes, and
   those layers must fail when their input is exhausted (in the PPMd
   route a record byte is a decoded symbol and may cost far less than
   8 bits).
2. **Never size anything from a declared length before the bytes are
   there.** The program length and the data length are checked against
   the bits that remain in the payload before anything is reserved.
   The block length L is only a number until the block's bytes have
   been produced: nothing is allocated for a queued filter except its
   small fixed-size entry.
3. **Program store** (section 6.2): at most 1,024 programs and
   1,048,576 program bytes per solid group. A program of n bytes
   decodes to at most 8·(n − 1)/6 + 1 instructions, so decoded programs
   are bounded too. The constant data of a program is at most 8,064
   bytes.
4. **Queue** (section 7.1): at most 65,536 pending filters; blocks in
   ascending order without overlap. Because blocks never overlap and
   every block is at most 0x3C000 bytes, the raw bytes held back at any
   moment are at most 0x3C000 and lie in the window; no separate
   buffering proportional to the input is needed.
5. **Block length** at least 1 and at most 0x3C000 (0x1E000 for delta,
   RGB and audio). Because every queued block holds at least one byte
   and blocks do not overlap, a member of U bytes runs at most U
   filters.
   The working copy of a block is at most 240 KiB (or the machine's
   256 KiB memory).
6. **Positions are 64-bit.** s = G + offset (+ 258) cannot overflow 64
   bits. A block far beyond the end of the data is never reached and
   costs nothing.
7. **Standard programs** are computed directly; their cost is
   proportional to L. The delta and audio loops run over at most
   min(N, L) channels, never over N (N can be 122,880 for a 1-byte
   block). All index arithmetic of sections 10 to 14 stays inside
   0..L − 1 when the stated loop bounds are used; the audio sums stay
   far inside 32 bits (each error sum grows by at most 1,279 per step
   and is cleared every 32 steps).
8. **Virtual machine.** Memory is a fixed 262,147 bytes; every address
   is reduced before use; there is no access outside it. A run is
   limited to 65,536 + 256·L instructions (section 18.4), so the total
   work of all runs is bounded by a constant times the output size plus
   a constant times the number of filters. The constants are large: a
   hostile stream can make the machine execute some 30,000 to 45,000
   instructions per byte of packed input (many one-byte blocks, or
   large blocks of highly compressible data, each with a program that
   uses its whole budget). A caller that limits decoding time must
   count this in; the limit on the unpacked size alone does not express
   it. Jump targets are checked
   against the instruction count when taken. Division by zero is
   defined (no effect). Shift counts are reduced modulo 32 before any
   native shift is performed.
9. **Zeroed memory without a 256 KiB clear per run.** Section 18.1
   requires the memory to read as zero. A hostile stream can run a
   filter every few bytes, so clearing the whole memory each time must
   be avoided: remember which address ranges the previous run could
   have written (the block, the system area, the constant data, and the
   extent of the run's memory writes) and clear only those, or keep an
   equivalent record. The cost of a run must be proportional to L plus
   the number of instructions it executes.
10. **Results.** A filter's result has exactly L bytes; the address it
    is taken from is checked (section 18.3). A filter can therefore
    never make a member longer or shorter than the LZ layer says.
11. **Member boundaries** (section 8): a block that straddles the end
    of a member is an error, so a filter never sees bytes of two
    members. A filter left queued at a member's end applies only to its
    own block in a later member of the same solid group, and it is
    discarded with the rest of the filter state when the group ends or
    fails. The queue is never carried from one solid group to
    another.
12. **Failure is final for the group**: after corrupt input in a
    member, the filter state is discarded with the rest of the group
    state and no later member of the group is decoded with it.
13. The delivered output of every member is verified against the
    CRC-32 of its header by the container layer; the filter layer does
    not weaken that check, and a filter that was mis-specified here
    would show up as a CRC-32 mismatch, not as silent corruption.

## 22. Observed implementation gaps

Faults and leniencies of the reference implementation (`unar` 1.10.7),
found by reading it and confirmed with crafted archives unless marked
"source". They explain why rules exist; nothing here is to be imitated.

1. **No execution limit.** A program that loops forever hangs the tool
   (vector E15).
2. **Program number 0 on a non-empty store fails.** The store is
   emptied but the count taken before the emptying is used to decide
   whether the record defines a program; the record is then treated as
   a reuse of a program that no longer exists.
3. **One program too many.** Program numbers up to 1,024 are accepted
   for tables of 1,024 entries (source): the 1,025th program writes past
   the end of two arrays.
4. **Block length is not checked against the machine's memory**
   (source): the block's bytes are copied into the 256 KiB memory
   whatever L is. In tests, blocks of 0x3C001 to 0x40001 bytes give a
   failed or shortened extraction.
5. **A second filter for the same range was meant to work but never
   does**: the test for it compares against a marker that has just been
   reset, so the case always ends in the "blocks out of order" error.
   The same code would also pass a wrong file position to the second
   filter.
6. **Zero-length blocks and discarded results cut the output short.**
   A block of length 0 (vector E21), a result whose address is out of
   range (the block is silently replaced by nothing), and the shortcuts
   of item 9 make the tool deliver fewer bytes than the member has,
   without a clear error.
7. **Crashes**: the byte form of xchg (no handler exists); a program
   whose last instruction is an untaken jz (jz is mistaken for an
   unconditional jump, no return is appended, and execution runs past
   the end; for the same reason a taken jz to the index just past such
   a program fails instead of returning); pop with an immediate operand
   (not rejected, no handler);
   a non-zero value at 0x3C030 after a run (the code that would keep
   global data between runs passes a wrong pointer).
8. **Memory is never cleared between runs**, so a program can read
   what earlier filters left: stack contents, old blocks, old outputs.
   The RGB program with a stride that is not a multiple of 3 (and its
   one-step special case with L = 3) reads such bytes; its result then
   depends on the filters that ran before. `bsdtar` behaves the same
   way.
9. **Direct implementations that differ from the program they
   replace.** The tool computes delta, audio, E8 and E8/E9 directly
   (RGB and Itanium run on its machine) and: returns an empty result
   for an x86 block shorter than 4 bytes and for a delta or audio block
   longer than 0x1E000 instead of failing or returning the block;
   accepts N = 0 (the "result" is then stale memory) and any larger N,
   looping N times; takes the length from R4 as overridden by the
   record; ignores overrides of R1 and R2.
10. **The data field of a record never reaches the program**, and
    constant data beyond 8,064 bytes is silently dropped (the limit is
    computed from the wrong length).
11. **32-bit accesses at the last three addresses** read and write
    three bytes beyond the memory instead of failing or wrapping. This
    specification keeps the behaviour (section 15.1) because it is
    harmless when the three bytes exist.
12. **Shift counts of 32 or more** are passed to the processor's shift
    unchanged (undefined in the source language; modulo 32 on the
    machines tested).
13. **Filters queued for positions beyond a member's end are applied
    in the next member** of a solid group, with that member's file
    positions (vector S4). This is not a fault and this specification
    keeps it (section 8). The related fault lies in the LZ layer: the
    member start from which file positions are counted moves only when
    an end-of-member marker is decoded (`rar29-lz.md`, its section on
    implementation gaps).
14. **Truncated or oversized fields**: a record whose fields run past
    its payload, a wrong check byte, an instruction cut off by the end
    of the program and constant data longer than the program are all
    rejected, as specified here.

`bsdtar` 3.5.3, for comparison (not a reference for this document):
knows only the E8, E8/E9, delta, RGB and audio programs and fails on
any other, including the Itanium program; rejects overlapping,
repeated and out-of-order blocks and blocks past the end; accepts a
program number 0 on a non-empty store (but delivers a different result
when a block is partly produced at that moment); skips a record with a
zero-length block instead of rejecting it; omits the special case of
RGB step 2; delivered wrong bytes after a match that copied from a
filtered region in one crafted stream; did not extract the second
member of the crafted solid archives; and could not decode the PPMd
members tested, real or crafted.

## 23. Decisions

| # | Section | Decision |
|--:|---|---|
| F1 | 5.3 | Block length above 0x3C000 is corrupt input; so is block length 0. |
| F2 | 5.5, 16 | Programs are validated when defined; a program shorter than 2 bytes is invalid. |
| F3 | 5.6 | The data field of a record is length-checked, read and discarded. |
| F4 | 6.1 | Program number 0 empties the store and discards the queue, also when the store is not empty. |
| F5 | 6.2 | At most 1,024 stored programs and 1,048,576 stored program bytes per solid group. |
| F6 | 7.1 | A block must not start before the end of the last block in the queue: no overlap, no nesting, no second filter on the same range, no descending order. |
| F7 | 7.1 | At most 65,536 pending filters. |
| F8 | 8 | A block that straddles the end of a member is corrupt input; filters whose blocks have not started when a member ends stay queued for the later members of the solid group and are discarded when the group ends. |
| F9 | 9.2 | Standard programs are defined by their transformations, recognised by length and CRC-32, and never executed; the instruction budget does not apply to them. |
| F10 | 9.2 | A record for a standard program with R2, R3, R4 (and R1, except RGB) different from the defaults is corrupt input. |
| F11 | 12, 14 | Delta needs N in 1..0x1E000, audio N in 1..254; L at most 0x1E000 for both. |
| F12 | 13 | RGB needs W in 3..L, R in 0..2, L in 3..0x1E000; output bytes not yet computed read as 0. |
| F13 | 15.1 | Three extra bytes exist behind the memory for 32-bit accesses at its last addresses. |
| F14 | 16.2 | Constant data of more than 8,064 bytes makes a program invalid. A return is appended unless the last instruction is a return. |
| F15 | 16.4 | pop with an immediate operand makes a program invalid, like every other write to an immediate. |
| F16 | 17.2 | xchg in the byte form exchanges with the byte read and write rules. |
| F17 | 17.3 | Shift counts are taken modulo 32. |
| F18 | 18.1 | The machine's memory is all zero at the start of every run; nothing persists between runs; the word at 0x3C030 is ignored. |
| F19 | 18.3 | The result must have length L and lie below 0x40000 − L; otherwise corrupt input. |
| F20 | 18.4 | A run may execute at most 65,536 + 256·L instructions. |

None of them changes how a genuine archive of the survey is read: all
2,558 real records satisfy every limit, with wide margins (blocks of 1
to 65,536 bytes, at most 4 programs of at most 216 bytes, at most 119
pending filters, N at most 32, only standard programs, every block
inside the member that announces it).

F1 (for length 0), F6, F7 and F8 are stated in `rar29-lz.md` as well
(its decisions L10 to L15); the two lists are meant to agree.

## 24. Open questions

They do not block an implementation; the document says what to do in
each case.

1. **Programs other than the six.** None occurs in the survey, and the
   only encoder that could be run was RAR 4.20. Whether other RAR
   versions emitted other programs is not known. Such a program runs on
   the machine of Part III under the budget F20, which was sized on
   the six known programs (section 18.4); if a genuine one needs more,
   the member fails with corrupt input and the budget would have to be
   raised.
2. **The data field (flag 0x08) and the word at 0x3C030.** The
   reference implementation evidently intended a program to receive
   data from the record and to keep data between its runs, but neither
   path works there, and no real record uses them. The official
   semantics are unknown (F3, F18).
3. **Several filters on one range, and blocks out of order** (F6). No
   real record needs either; both tools reject them. The official
   decoder may accept them.
4. **Filters across member boundaries** (F8): kept queued, as the
   reference tool does; no real archive has a record that describes a
   block of a later member, so the official behaviour is not known.
   **Zero-length blocks** (F1): rejected here; the two tools neither
   agree with each other nor handle them cleanly, and no real record
   has one.
5. **Program number 0 while filters are pending** (F4): not seen;
   `bsdtar` agrees with this document while no queued block has
   started and differs when one is partly produced; `unar` fails on
   every non-empty store.
6. **RGB with a stride that is not a multiple of 3, or with W above L**
   (F12): real encoders send multiples of 3 that are at most L. The
   existing decoders disagree with each other and with themselves
   (their result depends on earlier filters), so there is no behaviour
   to match.
7. **Audio with more than 254 channels, delta with more than 122,880**
   (F11): rejected because the programs themselves misbehave there.
8. **Byte-form xchg, shift counts of 32 and more, the three extra
   bytes** (F13, F16, F17): defined here; only reachable by
   non-standard programs.
9. **File positions of 4 GiB and more.** The x86 and Itanium rules use
   the low 32 bits of F. This was checked on the machine (vectors X3,
   X6, I3) but not with a real file of that size.
10. **Unpack version 36** is assumed not to differ from 29 in anything
    this document describes (see `rar4-container.md`).
11. **Agreement with `rar29-lz.md` section 11.** That document words
    the queue rules from the LZ side. The two texts were compared
    clause by clause when this document was verified, and this document
    was brought into line where they differed. They now state the same
    rule on every point: s = G + offset (+ 258) without wrap-around;
    member-relative file positions; block lengths of 1 to 0x3C000, with
    0 and anything longer corrupt input; the order check, made when the
    record is read, which also excludes a second filter on the same
    range; a queue of at most 65,536 entries; running a block as soon
    as it is complete; discarding the whole queue on program number 0,
    including a partly produced block; a result of exactly L bytes; a
    block in progress at a member's end being corrupt input; and
    unstarted filters staying queued for the later members of the
    group. Two remarks for a reader of both:
    - earlier drafts of the two documents disagreed on zero-length
      blocks and on unstarted filters at a member's end, each in turn
      taking the other's side; a note or comment elsewhere that says
      "ignored" for the first or "dropped" for the second is out of
      date;
    - the order check is worded differently (last queue entry here, an
      order mark there) with the same effect, section 7.1.

## 25. Evidence

**Reference implementation.** XADMaster, revision `7cb9ee0a`: the RAR 3
decompression handle (the parts that read filter records and schedule
filters), the filter classes, the virtual machine with its program
decoder, and the audio predictor. Kept outside this repository.

**Independent re-statement.** Everything in Parts I to III was
re-implemented as a throw-away experimental decoder (kept outside the
repository, not part of any deliverable) from the rules as worded
here, together with a minimal LZ decoder and a PPMd decoder borrowed
from the experiments behind `rar29-lz.md` and `rar29-ppmd.md`. The
figures below come from running it.

**Real archives, survey.** 113 archives from the public collection at
`files.scene.org` that contain members of unpack version 29 (all such
archives of a sample of 352; demo-scene releases of many years and of
unknown RAR versions):

- 678 compressed members, 109 MB unpacked; every one decodes to bytes
  matching the header CRC-32 (55 stored members and 3 split members
  were left out);
- 25 of the archives contain filters: 1,574 records, 1,527 in LZ
  blocks and 47 in PPMd blocks; 124 define a program, 1,450 reuse one;
- programs: delta 1,001, audio 487, E8 58, RGB 16, E8/E9 12; no other
  program;
- flags: every combination of 0x80, 0x40, 0x20 and 0x10 occurs except
  0x90 and 0xC0 (0xC0 occurs in the RAR 4.20 archives below, 0x90
  nowhere); 0x08 never;
- all three payload size forms occur; payloads end in 0 to 7 unused
  zero bits;
- register masks 0 (70), 1 (1,488) and 3 (16) only;
- block lengths 512 to 46,592; at most 17 filters pending; use
  counters up to 260; program number 0 only on an empty store (85
  times);
- blocks always in ascending order without overlap; no block touches a
  member boundary.

**Verification.** The document was then checked by a second person who
first wrote a second throw-away decoder (record parser, program store,
queue, the six transformations and the whole machine) from the text
alone, without the reference implementation, and only afterwards read
the reference implementation and compared it with the text clause by
clause. With that decoder:

- every vector of section 20 gave the stated result at the first
  attempt, the standard-program rows both by direct computation and by
  running the program bytes on the machine;
- all archives of the two real-data paragraphs here were decoded again:
  750 members, 2,173 records, every member matching its CRC-32;
- 14 more archives were written with RAR 4.20 from 24-bit pictures 1
  to 100,000 pixels wide, with and without the picture filter forced:
  14 members, 23 MB, 385 records (RGB 182, delta 202, audio 1), all
  correct; they supplied the stride values quoted in section 13;
- 2,100 further random programs (straight-line code over every
  instruction except call and ret, every operand kind, byte and 32-bit
  forms, conditional jumps, with a dump of registers, flags and
  memory) gave the same output in `unar` and on that decoder's
  machine;
- the stream vectors and some 40 further crafted streams (tiny
  programs, programs ending in a jump or call to the appended return,
  size forms, surplus payload bytes, the data field at and above its
  limit, records in PPMd blocks, resets with and without queued
  filters, blocks at and beyond the member end, solid groups) were
  given to both tools again;
- the instruction counts of section 18.4 were measured.

What this changed: block length 0 became corrupt input and unstarted
filters stay queued across members (both to agree with `rar29-lz.md`,
section 24 item 11; vectors S4, S6, E21 and E22 were rewritten or added
for it), and the instruction budget's fixed part was raised from 16,384
to 65,536 after the audio program was found to need 56,517
instructions for 254 channels.

**Real archives, made for the purpose.** 28 archives written by the
vendor's command-line RAR 4.20 for Linux (`rarlinux-x64-4.2.0.tar.gz`
from `rarlab.com`, run in a container as an archive creator only) from
synthetic sound, picture, table and text files and from executable
files, with the default settings and with each filter forced by the
`-mc` switch, solid and not solid, LZ and PPMd:

- 72 members, 28 MB; every one decodes to the original file;
- 599 records (555 LZ, 44 PPMd): E8 219, delta 192, E8/E9 133, audio
  32, RGB 18, Itanium 5; 30 of them in later members of solid groups
  (programs and use counters carried over; file positions relative to
  the member);
- records are emitted at the start of a member and then at every 4 MiB
  of raw output, for all blocks of the next 4 MiB; up to 119 pending;
- blocks of 1, 3 and 5 bytes with 31 channels (delta and audio); RGB
  strides 600, 711, 1,716 and 7,854, with R = 0, 1 and 2.

**Standard programs against the machine.** The six program byte
strings were run on the experimental machine of Part III and compared
with the direct transformations of sections 10 to 14: more than 30,000
random blocks of 1 to 245,760 bytes with random parameters inside the
limits, random file positions including values around 2^32, and data
biased towards the cases that matter (markers with operands around the
thresholds, bundles with qualifying slots, extreme sample values).
No difference. This is what established the limits of sections 11 to
14: the length-21 rule of the Itanium program, the special case of RGB
step 2, the 254-channel limit of the audio program (each channel's
state occupies 64 bytes of machine memory above 0x3C040; the 255th
collides with the stack), and the dependence of RGB on unwritten
memory for strides that are not multiples of 3.

**Crafted archives given to `unar` 1.10.7 (and `bsdtar` 3.5.3 where it
applies)**, each compared with the output predicted by this document;
identical in every case where the document does not deliberately
differ (sections 22 and 23):

- 4,800 random programs of one to three instructions with random
  operands of every kind, each followed by a dump of registers, flags
  and memory: this fixed every flag rule of section 17, the byte-form
  rules, the shift rules, the stack order and movzx/movsx;
- every conditional jump under every flag combination; 126 programs
  with random relative and absolute jump numbers; calls and returns;
  jumps to and past the end; programs with and without a final return;
  empty programs;
- registers, the system area, use counters and file positions as seen
  by a program (vector S5), with every register overridden, in a second
  member of a solid group, and through the 258 flag;
- memory edges, result address and length, constant data of various
  sizes, the data field, truncated programs and records, wrong check
  bytes, writes to immediates;
- all payload size forms and non-minimal numbers, in LZ and in PPMd
  blocks; records in PPMd blocks with a non-default escape character;
- about 800 random blocks through the delta, RGB (strides that are
  multiples of 3) and audio programs and about 270 through the two x86
  programs, with both tools; about 90 through the Itanium program with
  `unar`;
- adjacent, overlapping, nested, repeated and out-of-order blocks;
  blocks ending at, straddling and lying beyond the end of a member and
  of a solid group; zero-length and oversized blocks; matches that copy
  from a filtered region; program numbers out of range, reuse without
  a program, redefinition by number 0;
- the vectors of section 20.

**Not available.** No tool on the test machine other than the two named
could decode this method (the 7-Zip builds present are compiled without
their RAR decoders), and the vendor's site could not be reached for
older RAR releases during this work, so RAR 4.20 is the only encoder
whose output was produced on purpose.

## Clean-room feedback

Notes from the implementation side (clean room), for
`src/formats/dos/rar_native/filters.rs`. Prose only; nothing here was
written from any decoder's source.

1. **Result of implementing from the text.** Every vector of sections 4,
   19 and 20 gave the stated result at the first attempt: the six
   fingerprints, check bytes, instruction counts and the Itanium
   constant data (20.1); all rows of 20.2, each both by direct
   computation and by running the program's bytes on the machine; all
   37 rows of the table in 20.3, including the step counts; the records
   and delivered outputs of S1 to S6 (the records as carried were fed to
   the filter layer together with the raw output rebuilt from each
   description; the packed bytes belong to the LZ and PPMd layers and
   were not used here); and E1 to E22, each rejected at the stage the
   table names. In addition the direct transformations and the program
   bytes agree on some 400 pseudo-random blocks. No black-box tool was
   needed and none was run; no real archive was read, because the
   filter layer cannot be driven without the LZ layer.
2. **Section 20.3 counts.** The table has 37 rows. The text nowhere
   says so; an implementer who wants to assert the count has to count
   them.
3. **Order check (7.1 item 2).** Implemented in the wording of
   `rar29-lz.md` 11.2: an order mark holding the end of the block queued
   most recently, kept after that block has run and across members,
   forgotten on program number 0 and at the start of a group. As 7.1
   says, the two wordings accept the same streams as long as the caller
   reports true positions; the mark also rejects a caller that reports
   a position inside a block that has already run.
4. **Is a standard program validated (5.5, 9.2, 16)?** Section 9.2 says
   a standard program is never executed, section 5.5 that every program
   is checked and decoded when defined, and 9.2 that a different byte
   string with the same fingerprint is treated as the standard program.
   The text does not say whether such a look-alike must also pass the
   checks of section 16. Chosen, as the safest reading: every program
   is checked and decoded when defined; a program with a standard
   fingerprint that fails section 16 is corrupt input. The genuine six
   pass.
5. **State after a rejected record.** The text orders the checks of a
   record (5.3, 7.1) but not what state a rejected record leaves
   behind. Because corrupt input poisons the group (section 8), the
   implementation does not roll anything back: a record with program
   number 0 that is rejected later in its payload has already emptied
   the store and the queue. A caller must not reuse the state after an
   error.
6. **Section 18.3, "A + L is 0x40000 or more".** Taken literally: a
   result that would end exactly at the last byte of the memory
   (A + L = 0x40000) is rejected; the highest accepted A is
   0x3FFFF minus L. Section 23 (F19) words the same limit as "lie below
   0x40000 minus L", which agrees.
7. **Size of decoded programs (21 item 3).** The bound "8 times (n minus
   1) over 6, plus 1 instructions" is per program. With the store limit
   of 1,048,576 program bytes a group can hold about 1.4 million decoded
   instructions, which is roughly 28 MB in this implementation (20
   bytes per instruction). That is bounded and is paid for with actual
   input bytes, but it is an amplification of about 27 to 1 that the
   safety section does not spell out. Decoded instructions of the
   standard programs are not kept.
8. **Zeroed memory (21 item 9).** Implemented as a record of written
   64-byte pages; a run costs the block length plus 64 bytes per page
   it touched, and nothing else is cleared. A test checks that the
   memory is all zero after clearing, following each program of 20.3.
9. **Member end (section 8).** The interface treats any block that has
   started and is still queued when the member ends as corrupt input,
   also one that is complete: section 7.2 requires the caller to have
   run a complete block before declaring the member ended, so such a
   block can only be left by a caller's mistake.
10. **Payload size 0 (section 3).** `rar29-lz.md` 8.7 lets the 16-bit
    size form carry 0 and hands the record over; this layer rejects it,
    as section 3 says.
11. **Direct transformations outside their limits.** The functions for
    sections 10 to 14 are only reached with validated parameters, but
    they are written so that any stride, channel count or offset is
    harmless (no panic, result of the block's length). For RGB with R
    at or beyond L the second step does nothing.
