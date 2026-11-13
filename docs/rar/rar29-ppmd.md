# RAR 2.9 compression: PPMd blocks (unpack versions 29 and 36)

Format specification for clean-room implementation. This document describes
the **PPMd block type** of the compression method used by RAR 2.9, 3.x and
4.x (file header unpack version 29 or 36): a byte-oriented context-mixing
coder of the PPM family ("PPMd, variant H") driven by a range coder, and
the small RAR-specific layer that turns the decoded bytes into literals,
matches, filter records, block switches and the end of a member.

It does not describe the archive container (`rar4-container.md`), the LZ
block type of the same method with its window and match memory
(`rar29-lz.md`), the filter records and the filter virtual machine
(`rar29-filters.md`), RAR5 (`rar5-container.md`, `rar5-compression.md`),
or encryption. Section 2 states what this document assumes of those.

Contents: 1 how to read this document; 2 where PPMd blocks occur, and the
interfaces; 3 the block header; 4 the range decoder; 5 the model: data;
6 model memory; 7 starting a model; 8 decoding one symbol; 9 updating the
model; 10 the RAR layer on top of the symbols; 11 members, solid groups,
end of data; 12 worked examples; 13 test vectors; 14 safety requirements;
15 observed implementation gaps; 16 decisions; 17 open questions; 18
evidence.

## 1. How to read this document

### 1.1 Provenance

- This is a **prose specification**. It was derived by studying the
  behaviour of an existing open decoder (the PPMd and RAR 3.0 support of
  The Unarchiver's XADMaster library, LGPL, revision `7cb9ee0a` of
  `github.com/MacPaw/XADMaster`; called the *reference implementation*),
  by decoding archives written for the purpose by the vendor's own `rar`
  program (used only as a black-box archive creator), by black-box testing
  of two extraction tools with purpose-built streams (`unar` 1.10.7, built
  from the reference implementation, and `bsdtar` 3.5.3, an unrelated
  decoder used only through its command line), and by the public
  description of the PPMd algorithm family: D. Shkarin, "PPM: one step to
  practicality" (2002).
- The official `unrar` source was **not** consulted, nor the RAR code of
  7-Zip, nor the source of libarchive or of any other RAR decoder.
- The work is a **clean-room process**. The author of this document could
  read the reference implementation; the implementation is written **from
  this document only**. The document therefore contains no program code
  and no pseudo-code, only prose, tables of constants, worked examples and
  test vectors. Its names for things are its own.
- Where the reference implementation is wrong or unsafe the document says
  so (section 15) and specifies the correct behaviour instead.

### 1.2 Evidence labels

| Label | Meaning |
|---|---|
| CONFIRMED (RAR-written) | The rule was exercised when decoding archives written by the vendor's `rar` program (versions 4.20 and 6.24, RAR4 format), and every decoded file matched the CRC-32 in its header (section 18). |
| CONFIRMED (crafted) | Checked by giving `unar` (and `bsdtar` where it supports the feature) a purpose-built stream and comparing the output with the output predicted by this document. |
| REFERENCE-ONLY | Stated by the reference implementation; not separately observable. |
| INFERRED | Deduced; not confirmed. |
| DECISION | A choice made by this specification, not a fact about the format (section 16). |

**A rule without a label in sections 4 to 9 is CONFIRMED (RAR-written).**
The model of sections 5 to 9 is one indivisible mechanism: a single wrong
constant, comparison or ordering anywhere in it makes every later byte
wrong, so a file that decodes to its CRC-32 confirms all the rules it
exercised at once. Section 18 lists how often each rare path was taken.
Rules in sections 3, 10 and 11 carry their own labels.

### 1.3 Terms and numbers

| Term | Meaning |
|---|---|
| member | One file stored in the archive, the pieces of a file split across volumes already joined by the container layer. |
| packed bytes | The data area of a member as stored. **P** is their number. |
| **U** | The unpacked size the member's header declares. Untrusted. |
| solid group | A maximal run of members that share one decoder state (`rar4-container.md`). A member of an archive that is not solid is a group of one. |
| block | A stretch of a member's packed bytes coded with one block type, LZ or PPMd. It starts with a block header. |
| symbol | A value 0..255 delivered by the model. Not yet an output byte: section 10. |
| model | Everything of sections 5 to 9: contexts, tables, counters and the memory they live in. |
| corrupt input | The decoder's error result. No contents are delivered for the member. |

Numbers are decimal unless prefixed `0x`. Byte strings are written as
hexadecimal digits in code style (`a3 00`). Ranges written `a..b` include
both ends. "x >> k" means x divided by 2 to the power k, rounded toward
negative infinity. Divisions written "floor(a / b)" round down; all such
operands are non-negative.

## 2. Where PPMd blocks occur, and the interfaces

A member whose unpack version is 29 or 36 and whose method byte is
`0x31`..`0x35` is a sequence of blocks. `rar29-lz.md` owns that framing;
this document assumes of it:

1. **Block headers are byte-aligned.** A block header starts at a byte
   boundary of the member's packed bytes. The **most significant bit of
   its first byte** selects the block type: 1 = PPMd block (section 3),
   0 = LZ block (`rar29-lz.md`). When an LZ block ends with its "new
   table" item, the LZ decoder skips to the next byte boundary and the
   next block header is read there. (CONFIRMED (RAR-written) for LZ
   followed by PPMd; CONFIRMED (crafted).)
2. **One output stream, one window.** Bytes produced inside a PPMd block,
   by literals and by matches alike, are appended to the group's output
   exactly like the output of LZ blocks: they enter the same sliding
   window (4 MiB = 4,194,304 bytes, `rar29-lz.md`), are
   visible to later LZ matches, and are subject to pending filters. A
   match of distance d and length n appends n bytes, each equal to the
   byte d positions before it, one byte at a time in increasing order.
3. **Filter records** are opaque here: the PPMd block delivers a flags
   byte and a string of payload bytes (section 10.5) to the filter layer
   of `rar29-filters.md`, together with the current output position, in
   exactly the form in which an LZ block delivers them.
4. **A PPMd block does not touch the LZ state**: the LZ match memory
   (last distance and length, the four remembered distances) is neither
   used nor changed by the matches of section 10 (CONFIRMED (crafted),
   vector of section 18), and the LZ code-length table kept for "keep
   table" transmissions is not changed (REFERENCE-ONLY here;
   `rar29-lz.md` confirms it on real archives).
5. The **CRC-32** of a member's U output bytes (after filters) is checked
   by the container layer.

What the PPMd decoder keeps **per solid group** (section 11): the model,
if one has been created, and the **escape value** (section 3.3). Both
survive LZ blocks and member boundaries inside the group.

## 3. The block header

### 3.1 Layout

All fields are whole bytes, read in this order from the member's packed
bytes:

| Byte | Present | Contents |
|---|---|---|
| flags | always | bit 7 (`0x80`): 1, "this is a PPMd block". Bit 6 (`0x40`): an escape-value byte follows. Bit 5 (`0x20`): **new model**: a memory-size byte follows, and bits 4..0 hold the order code. Bits 4..0: order code (0..31); **ignored when bit 5 is 0**. |
| memory size | only if flags bit 5 is 1 | value b (0..255); the model memory is **(b + 1) × 1,048,576 bytes** (1 to 256 MiB). |
| escape value | only if flags bit 6 is 1 | the new escape value (0..255). |
| 4 bytes | always | the first four bytes of range-coder data (section 4.2). |

Note the order: **memory-size byte first, escape byte second**, although
the escape flag is the higher bit. Example from a RAR-written archive:
`e4 00 cd` = new model with order code 4 (order 5), 1 MiB, escape value
`0xcd`. (CONFIRMED (RAR-written).)

### 3.2 Order code and new model

When flags bit 5 is 1:

- The **maximum order M** is derived from the order code c: if c is
  0..15, M = c + 1; if c is 16..31, M = 16 + 3 × (c − 15), that is 19,
  22, 25, 28, 31, 34, 37, 40, 43, 46, 49, 52, 55, 58, 61, 64.
  (CONFIRMED (RAR-written) for codes 1, 2, 3, 4, 5, 6, 7, 9, 11, 15, 16,
  20, 25 and 30; CONFIRMED (crafted) for 1, 15, 16, 17 and 31.)
- **M = 1 (order code 0) is corrupt input.** (CONFIRMED (crafted): both
  black-box tools reject it. `rar` never writes it: its smallest order
  is 2.)
- The model existing so far, if any, is discarded with its memory. A new
  model is created with the given memory size and M, and started
  (section 7).

When flags bit 5 is 0, the existing model continues **completely
unchanged**: contexts, statistics, tables, counters, the current
context, the text area, everything of sections 5 to 9. Only the range
decoder is re-initialised. **If no model exists in this solid group,
this is corrupt input.** (CONFIRMED (crafted) for both halves; keeping a
model across a member boundary is CONFIRMED (RAR-written).)

`rar` writes the order it was asked for rounded down to a representable
one (asked for 17: code 15, order 16; 20: code 16, order 19; 32: code
20, order 28; 48: code 25, order 43; 63: code 30, order 61), a default
of order 8 with 25 MiB (memory byte 24) when given no numbers, and
memory sizes of 1 to 128 MiB. A decoder accepts every order code 1..31
with every memory byte 0..255 (DECISION; a memory byte of 255 is
CONFIRMED (crafted)); section 14 says how to do that without trusting
the size.

### 3.3 The escape value

- One symbol value, the **escape value**, is reserved to introduce the
  control sequences of section 10. It is **2 at the start of every solid
  group**.
- A block header with flags bit 6 set replaces it. It then stays in
  force **until another header replaces it**: through later PPMd blocks
  with or without the new-model bit, through LZ blocks, and across
  member boundaries inside the solid group. (CONFIRMED (RAR-written): a
  solid archive whose first member sets the value `0xcd` and whose later
  members start with the headers `a4 00` (new model, no escape byte)
  and `84` (model kept), decodes only with `0xcd` retained. CONFIRMED
  (crafted).)
- `rar` appears to pick a value that is rare in the data (`0xff` for
  text, `0xcd` for a file containing every byte value); nothing depends
  on how.

### 3.4 After the header

After the header bytes the range decoder is initialised (section 4.2),
which consumes the four bytes listed in the table, and symbols are
decoded until a control sequence ends the block (section 10).

## 4. The range decoder

### 4.1 State

Three unsigned 32-bit quantities; all arithmetic on them is modulo 2 to
the power 32:

| Name | Meaning |
|---|---|
| low | lower bound of the current interval |
| range | width of the current interval |
| code | the next 32 bits of input, as far as read |

and the read position in the member's packed bytes. **The decoder reads
whole bytes only, and reading a byte beyond the last packed byte is
corrupt input.**

### 4.2 Initialisation

At the start of every PPMd block, after the header bytes: low = 0,
range = `0xFFFFFFFF`, and code = the next four bytes, the first being
the most significant. No byte is skipped before them.

### 4.3 The operations

**Query.** Given an integer *scale* (1..65,535; below 32,768 in every
stream examined): set range to floor(range / scale); the result is
v = floor(((code − low) modulo 2^32) / range).

- **A scale of 0 is corrupt input** and must be tested before dividing.
  It can only arise from a 16-bit context total that has wrapped to 0
  (sections 5.2 and 14), which no valid stream produces. (DECISION; the
  reference implementation divides by zero.)
- If the new range is 0 (scale larger than range), the input is
  corrupt. (DECISION; the reference implementation divides by zero.)
  Between symbols the range is always at least 32,768, so this needs a
  scale above 32,767.
- If v ≥ scale, the input is corrupt. (DECISION. In a stream made by a
  correct encoder v is always below scale; it never happened in the
  RAR-written archives. The reference implementation carries on with a
  meaningless value, section 15.)

**Narrow.** Given integers lo < hi ≤ scale, following a query with that
scale: low = low + range × lo; range = range × (hi − lo). Then
**normalise**, immediately, before anything else happens.

**Normalise.** Repeat the following until it stops:

1. Compare the most significant bytes of low and of (low + range)
   modulo 2^32, i.e. test whether (low XOR (low + range)) ≥ `0x1000000`.
2. If they differ: if range ≥ 32,768 (`0x8000`), **stop**. Otherwise
   replace range by (2^32 − low) AND `0x7FFF` (the distance from low up
   to the next multiple of 32,768; for low itself a multiple, 0) and
   continue with step 3.
3. (Reached when the top bytes are equal, or after the replacement of
   step 2.) Read one byte b: code = code × 256 + b; range = range × 256;
   low = low × 256 (each modulo 2^32). Go back to step 1.

If the replacement in step 2 yields range 0, the loop can never stop
and consumes input until it runs out: corrupt input (an implementation
may report it at once).

**Binary decision** with probability p out of 16,384 (0 < p < 16,384):
a query with scale 16,384; if v < p the outcome is "yes" and the
interval is narrowed to (0, p); otherwise "no", narrowed to
(p, 16,384).

The constants are exact: initial range `0xFFFFFFFF`; top-byte threshold
`0x1000000`; minimum range 32,768; binary scale 16,384.

**The read position is part of the format.** The decoder has consumed,
at any moment, exactly the header bytes, the four initial bytes and one
byte per execution of step 3. Because normalisation is done right after
every narrowing, this position is well defined after each symbol, and
the block switch of section 10.3 depends on it. An implementation that
postpones normalisation until the next query must still report the same
position.

## 5. The model: data

The model predicts the next symbol from the symbols before it. It keeps
a tree of **contexts**; the context of order k stands for one particular
string of k symbols that has occurred, and holds statistics about the
symbols that followed that string.

### 5.1 States

A **state** is the record of one symbol inside one context:

| Field | Range | Meaning |
|---|---|---|
| symbol | 0..255 | |
| frequency | 1..128 at rest (8 bits suffice) | adaptive count |
| successor | none, a *text reference*, or a *context reference* | where the model goes after this symbol in this context |

A **text reference** is a position in the text area (section 6.2): the
position of the symbol that followed when this state was created; the
context it stands for does not exist yet. A **context reference**
designates an existing context. A decoder must be able to tell the three
kinds apart; how is its own business (the reference implementation
compares addresses: everything below the units area is text).

### 5.2 Contexts

| Field | Meaning |
|---|---|
| n | number of states, 1..256; no two states of a context have the same symbol |
| states | the states in a definite **order** (it matters: sections 8.2, 8.3) |
| total T | only when n ≥ 2: an unsigned 16-bit number (arithmetic modulo 65,536), roughly the sum of the frequencies plus an allowance for escapes |
| suffix | the context of the string shortened by its oldest symbol; none for the one context of order 0 (the **root**) |

A context with n = 1 is a **binary context**; it has no total.

Facts that always hold (an implementation need not check the first
three, but see section 14 for the ones it must not rely on blindly):

- The root has 256 states at all times.
- Every symbol of a context also occurs in its suffix context; so n
  never decreases along the suffix chain.
- The suffix chain from any context ends at the root after at most M
  steps.
- A context occupies one **unit** of model memory; the states of a
  context with n ≥ 2 occupy a separate block of ceil(n / 2) units (two
  states per unit); the single state of a binary context lives inside
  the context's own unit (section 6).

### 5.3 Other model variables

| Name | Type | Meaning |
|---|---|---|
| M | 2..64 | maximum order, from the block header |
| current context | context reference | where decoding of the next symbol starts |
| order deficit D | integer ≥ 0 | M minus the order of the context the model is looking at; 0 means "at maximum order" |
| run counter R | signed 32-bit, wrapping | counts successes in a row; only its sign is used |
| R0 | −(min(M, 12)) − 1 | the value R is reset to (−3 for M = 2, −13 for M ≥ 12) |
| success flag S | 0 or 1 | whether the previous symbol was predicted confidently |
| previous symbol L | 0..255 | the symbol decoded last; 0 after a model start |
| escape seed E | 2..25 | set by an escape from a binary context (section 8.2), used in section 9.5 |
| binary table | 128 × 64 numbers, each 1..16,383 | probabilities for binary contexts (section 7.2) |
| escape estimators | 25 × 16 records | section 7.3 |
| masked set | set of symbols | symbols already ruled out while one symbol is being decoded |
| text pointer, memory state | | section 6 |

## 6. Model memory

The model lives in a fixed arena whose size comes from the block header.
When the arena is full the model is thrown away and started afresh
(section 9.7), **at a moment the encoder and the decoder must agree on
to the symbol**. The moment depends on how the arena is managed. This
section is therefore part of the format, down to the order in which
free blocks are reused. Section 6.8 says what an implementation is free
to do differently.

### 6.1 Units and the arena

- The arena is **A = (b + 1) × 1,048,576 bytes**; positions in it are
  byte offsets 0..A − 1.
- The **unit** is 12 bytes. A context costs 1 unit. A block of states
  for a context with n ≥ 2 states costs ceil(n / 2) units, rounded up to
  a size class (section 6.3).
- The arena is divided once, at every model start:
  - units area size **X = 12 × (floor(floor(A / 8) / 12) × 7)** bytes;
  - the **text area** is offsets 0 .. A − X − 1; the **units area** is
    offsets A − X .. A − 1.

  For A = 1 MiB: X = 917,448, so the text area is offsets 0..131,127 and
  the units area starts at offset 131,128. For A = 2 MiB: X = 1,834,980,
  start 262,172. (Roughly one eighth text, seven eighths units.)

Four pointers (byte offsets) describe the arena:

| Pointer | At model start | Meaning |
|---|---|---|
| text pointer **TP** | 0 | next text position to be written |
| units start **US** | A − X | lowest offset used for units; can move down (6.5) |
| low **LO** | A − X | start of the untouched gap |
| high **HI** | A | end of the untouched gap |

Initially the whole units area is one untouched gap LO..HI. Blocks of
states are carved from its low end, contexts from its high end.

### 6.2 The text area

The text area receives a copy of the decoded symbols, one byte each, at
TP (section 9.3 says exactly when a byte is written and when TP steps
back by one). A text reference is an offset into it; the smallest text
reference ever stored is 1, so 0 can represent "none". Text bytes are
read only in section 9.4, and only at offsets below TP at that moment
(checked over every symbol of the survey of section 18: the byte read
is at most the byte just written). The model is restarted when TP
reaches US (section 9.3).

### 6.3 Size classes

There are 38 size classes. Blocks exist only in these sizes:

| Class | Units | Class | Units | Class | Units | Class | Units |
|--:|--:|--:|--:|--:|--:|--:|--:|
| 0 | 1 | 10 | 21 | 20 | 60 | 30 | 100 |
| 1 | 2 | 11 | 24 | 21 | 64 | 31 | 104 |
| 2 | 3 | 12 | 28 | 22 | 68 | 32 | 108 |
| 3 | 4 | 13 | 32 | 23 | 72 | 33 | 112 |
| 4 | 6 | 14 | 36 | 24 | 76 | 34 | 116 |
| 5 | 8 | 15 | 40 | 25 | 80 | 35 | 120 |
| 6 | 10 | 16 | 44 | 26 | 84 | 36 | 124 |
| 7 | 12 | 17 | 48 | 27 | 88 | 37 | 128 |
| 8 | 15 | 18 | 52 | 28 | 92 | | |
| 9 | 18 | 19 | 56 | 29 | 96 | | |

(Four classes in steps of 1, four in steps of 2, four in steps of 3,
then steps of 4 up to 128.) The **class of a unit count** u (1..128) is
the smallest class whose size is at least u: 5 units are served by class
4 (6 units), 13 by class 8 (15 units), 122 by class 36.

For each class there is a **free list**: a last-in, first-out stack of
free blocks of exactly that size. All 38 are empty at model start.

A second per-model value, the **merge countdown**, an integer 0..255,
is 0 at model start.

### 6.4 Allocating

**A context** (1 unit):

1. If HI ≠ LO: HI decreases by 12 and the context is at the new HI.
2. Otherwise, if the free list of class 0 is not empty, its top block is
   taken.
3. Otherwise the slow path of 6.5 for class 0.

Note that step 1 does not consult the free list: while any gap is left,
contexts come from the gap even if 1-unit blocks are free.

**A block of u units** (class c):

1. If the free list of class c is not empty, its top block is taken.
2. Otherwise, if LO + 12 × size(c) ≤ HI: the block is at LO, and LO
   increases by 12 × size(c).
3. Otherwise the slow path of 6.5 for class c.

### 6.5 The slow path

For a request of class c:

1. If the merge countdown is 0: set it to 255, run the **merge pass** of
   6.7, and if the free list of class c is now not empty, take its top
   block and finish.
2. Look at the free lists of the classes c + 1, c + 2, ..., 37 in this
   order. At the first one that is not empty: take its top block, keep
   its first size(c) units as the result and return the rest to the free
   lists by the **split rule** below. Finish. (The countdown is not
   changed.)
3. Otherwise decrease the merge countdown by 1. Then, if
   US − TP **> 12 × size(c)** (strictly greater): US decreases by
   12 × size(c) and the block is at the new US: the units area grows
   downward into unused text space. Otherwise the allocation **fails**.

**Split rule** (a block of class "old" keeps its first size(new) units;
the remaining r = size(old) − size(new) units, which start right after
the kept part, are returned): let k be the class of r. If size(k) = r,
push the remainder on the free list of class k. Otherwise push its first
size(k − 1) units on the free list of class k − 1, **then** push what is
left (1, 2 or 3 units) on the free list of that size. Example: shrinking
a 128-unit block to class 4 (6 units) leaves 122 units: 120 units go to
class 35, then the last 2 units to class 1.

### 6.6 Growing, shrinking, releasing

**Grow by one unit** (a block that holds u units' worth of states must
hold u + 1): if the class of u + 1 equals the class of u, nothing
happens and the block stays where it is. Otherwise allocate a block for
u + 1 units (6.4; this may fail), move the contents, and **then** push
the old block on the free list of its class.

**Shrink** from u units to v < u units: if the classes are equal,
nothing happens. Otherwise, if the free list of the class of v is not
empty: take its top block, move the contents there, and push the old
block on the free list of the class of u. Otherwise the block stays in
place, keeps its first size(class of v) units, and the rest is returned
by the split rule.

**Release** of a block of u units: push it on the free list of the class
of u.

Contexts are never released. Blocks of states are released only by
growing, shrinking and the conversion of section 8.5.

### 6.7 The merge pass

It joins free blocks that lie next to each other and redistributes them.

1. **Collect.** Start with an empty working list. Go through the classes
   0, 1, ..., 37 in this order; for each, pop blocks from its free list
   (top first) until it is empty, inserting each popped block **at the
   front** of the working list and noting its size in units. (So the
   working list ends up in the reverse of the order in which blocks were
   popped: the block popped last is first.)
2. **Join.** Go through the working list from front to back. For the
   block B being visited, repeat: let N be the block that begins exactly
   where B ends. If N is a free block on the working list (not in use,
   not the untouched gap, not beyond the arena) **and**
   units(B) + units(N) < 65,536, remove N from the working list and add
   its units to B; then look again at what now follows B. Otherwise go
   on to the next block of the list. A block that has been absorbed is
   no longer on the list and is not visited. B keeps its place in the
   list.
3. **Redistribute.** Take the blocks off the working list from front to
   back. For a block of s units at offset q:
   - while s > 128: push the 128 units at q on the free list of class
     37; q increases by 1536, s decreases by 128;
   - then let k be the class of s. If size(k) = s, push the block on
     list k. Otherwise let t = s − size(k − 1) (1, 2 or 3): first push
     the **last** t units (at offset q + 12 × size(k − 1)) on the free
     list of size t, then push the first size(k − 1) units (at q) on the
     free list of class k − 1.

The merge pass runs only from step 1 of the slow path: at the first
slow-path request after a model start, and thereafter at the first
slow-path request after the countdown has been decreased 255 times.

### 6.8 What is observable, and what is free

Observable, and therefore mandatory (each was varied experimentally;
the variants stop decoding RAR-written archives correctly, section 18):

- the sizes: 12-byte units, 1 unit per context, ceil(n / 2) units per
  block of states, the 38 classes, the division of the arena, the strict
  inequality in step 3 of the slow path;
- that contexts come from the high end of the gap first and blocks of
  states from the free list first;
- the last-in, first-out order of every free list;
- when the merge pass runs, the order in which it collects, and the
  order and form in which it puts blocks back;
- which block a grow or shrink ends up in, and the order "allocate, then
  release the old block";
- when the text pointer advances and steps back, and the two exhaustion
  conditions (an allocation fails; TP reaches US).

All of this is reproduced automatically by an implementation that keeps
a real arena of A bytes addressed by offsets and performs the steps as
written. Free:

- how contexts, states and references are encoded inside the units, or
  whether they are stored in the arena at all, provided the offsets that
  drive adjacency in the merge pass are tracked;
- how the free lists and the working list are represented (the reference
  implementation threads them through the free blocks themselves and
  recognises a free neighbour by a marker written into the block; any
  representation that gives the same answers will do);
- the contents of free blocks and of unwritten text;
- whether the arena is backed by memory before it is used (section 14
  requires that it is not).

### 6.9 Allocator test sequences

Offsets from the start of a 1 MiB arena (A = 1,048,576). LO, HI and US
are given as they stand after the step; free lists are shown top
first, "6u" meaning the list of 6-unit blocks; lists not shown are
empty. TP is 0 unless a step sets it. Sequence A:

| Step | Operation | Result (offset) | Pointers after | Free lists after (top first) |
|---|---|---|---|---|
| A1 | initialise, 1 MiB |  | LO=131128 HI=1048576 US=131128 | all empty |
| A2 | allocate a context | 1048564 | LO=131128 HI=1048564 US=131128 | all empty |
| A3 | allocate 128 units | 131128 | LO=132664 HI=1048564 US=131128 | all empty |
| A4 | allocate 1 unit, three times | 132664, 132676, 132688 | LO=132700 HI=1048564 US=131128 | all empty |
| A5 | allocate 2 units | 132700 | LO=132724 HI=1048564 US=131128 | all empty |
| A6 | release 132676 (1 unit), then 132664 (1 unit) |  | LO=132724 HI=1048564 US=131128 | 1u: 132664 132676 |
| A7 | allocate 1 unit | 132664 | LO=132724 HI=1048564 US=131128 | 1u: 132676 |
| A8 | grow 132664 from 1 unit to 2 | 132724 | LO=132748 HI=1048564 US=131128 | 1u: 132664 132676 |
| A9 | grow 132724 from 2 units to 3 | 132748 | LO=132784 HI=1048564 US=131128 | 1u: 132664 132676, 2u: 132724 |
| A10 | grow 132748 from 3 units to 4 | 132784 | LO=132832 HI=1048564 US=131128 | 1u: 132664 132676, 2u: 132724, 3u: 132748 |
| A11 | grow 132784 from 4 units to 5 | 132832 | LO=132904 HI=1048564 US=131128 | 1u: 132664 132676, 2u: 132724, 3u: 132748, 4u: 132784 |
| A12 | grow 132832 from 5 units to 6 | 132832 | LO=132904 HI=1048564 US=131128 | 1u: 132664 132676, 2u: 132724, 3u: 132748, 4u: 132784 |
| A13 | shrink 132832 from 6 units to 3 | 132748 | LO=132904 HI=1048564 US=131128 | 1u: 132664 132676, 2u: 132724, 4u: 132784, 6u: 132832 |
| A14 | allocate 6 units | 132832 | LO=132904 HI=1048564 US=131128 | 1u: 132664 132676, 2u: 132724, 4u: 132784 |
| A15 | shrink 132832 from 6 units to 3 | 132832 | LO=132904 HI=1048564 US=131128 | 1u: 132664 132676, 2u: 132724, 3u: 132868, 4u: 132784 |
| A16 | allocate 128 units | 132904 | LO=134440 HI=1048564 US=131128 | 1u: 132664 132676, 2u: 132724, 3u: 132868, 4u: 132784 |
| A17 | shrink 132904 from 128 units to 5 | 132904 | LO=134440 HI=1048564 US=131128 | 1u: 132664 132676, 2u: 134416 132724, 3u: 132868, 4u: 132784, 120u: 132976 |
| A18 | allocate 13 units | 134440 | LO=134620 HI=1048564 US=131128 | 1u: 132664 132676, 2u: 134416 132724, 3u: 132868, 4u: 132784, 120u: 132976 |
| A19 | allocate a context | 1048552 | LO=134620 HI=1048552 US=131128 | 1u: 132664 132676, 2u: 134416 132724, 3u: 132868, 4u: 132784, 120u: 132976 |
| A20 | allocate 128 units, 595 times | last: 1047004 | LO=1048540 HI=1048552 US=131128 | 1u: 132664 132676, 2u: 134416 132724, 3u: 132868, 4u: 132784, 120u: 132976 |
| A21 | allocate a context, 1 times | last: 1048540 | LO=1048540 HI=1048540 US=131128 | 1u: 132664 132676, 2u: 134416 132724, 3u: 132868, 4u: 132784, 120u: 132976 |
| A22 | release 132700 (2 units), then 132688 (1 unit) |  | LO=1048540 HI=1048540 US=131128 | 1u: 132688 132664 132676, 2u: 132700 134416 132724, 3u: 132868, 4u: 132784, 120u: 132976 |
| A23 | text pointer set to 1000; allocate 8 units (merge pass runs) | 132976 | LO=1048540 HI=1048540 US=131128 | 1u: 132736, 2u: 134416, 3u: 132868, 4u: 132784, 6u: 132664, 112u: 133072 |
|  | merge countdown now 255 |  | LO=1048540 HI=1048540 US=131128 | 1u: 132736, 2u: 134416, 3u: 132868, 4u: 132784, 6u: 132664, 112u: 133072 |
| A24 | allocate a context | 132736 | LO=1048540 HI=1048540 US=131128 | 2u: 134416, 3u: 132868, 4u: 132784, 6u: 132664, 112u: 133072 |
| A25 | allocate 128 units | 129592 | LO=1048540 HI=1048540 US=129592 | 2u: 134416, 3u: 132868, 4u: 132784, 6u: 132664, 112u: 133072 |
|  | merge countdown now 254 |  | LO=1048540 HI=1048540 US=129592 | 2u: 134416, 3u: 132868, 4u: 132784, 6u: 132664, 112u: 133072 |
| A26 | text pointer set to 128056; allocate 128 units | fails | LO=1048540 HI=1048540 US=129592 | 2u: 134416, 3u: 132868, 4u: 132784, 6u: 132664, 112u: 133072 |
|  | merge countdown now 253 |  | LO=1048540 HI=1048540 US=129592 | 2u: 134416, 3u: 132868, 4u: 132784, 6u: 132664, 112u: 133072 |
| A27 | allocate 100 units | 133072 | LO=1048540 HI=1048540 US=129592 | 2u: 134416, 3u: 132868, 4u: 132784, 6u: 132664, 12u: 134272 |

Notes on sequence A. A8 to A11 each move the block (classes 0, 1, 2, 3,
then 4) and release the old one; A12 stays in place (5 and 6 units share
class 4). A13 moves into the free 3-unit block; A15 finds no free 3-unit
block and splits in place. A17 shows the two-piece split (122 = 120 +
2). In A23 the gap is used up and the countdown is 0, so the merge pass
runs: it collects 132688, 132664, 132676 (class 0), 132700, 134416,
132724 (class 1), 132868, 132784, 132976, giving the working list
132976, 132784, 132868, 132724, 134416, 132700, 132676, 132664, 132688;
joining turns 132976 into 122 units (it absorbs 134416), 132700 into 4
units (absorbs 132724), 132676 into 6 units (absorbs 132688, then
132700) and finally 132664 into 7 units (absorbs 132676);
redistribution pushes 134416 (2 units) and 132976 (120 units), then
132784, then 132868, then 132736 (1 unit) and 132664 (6 units). The
8-unit request is then served by step 2 of the slow path from the
120-unit block, whose remaining 112 units go to class 33. A25 takes
text space; A26 fails because US − TP is exactly 1536, not more.

Sequence B (a fresh 1 MiB arena) shows a joined run longer than 128
units:

| Step | Operation | Result (offset) | Pointers after | Free lists after (top first) |
|---|---|---|---|---|
| B1 | initialise; allocate a context; allocate 128, 128, 128, 2, 128 and 1 units | 131128, 132664, 134200, 135736, 135760, 137296 | LO=137308 HI=1048564 US=131128 | all empty |
| B2 | allocate 128 units 593 times, then a context 34 times | last: 1046620, 1048156 | LO=1048156 HI=1048156 US=131128 | all empty |
| B3 | release 132664 (128 units), 135736 (2 units), 131128 (128 units), 134200 (128 units) |  | LO=1048156 HI=1048156 US=131128 | 2u: 135736, 128u: 134200 131128 132664 |
| B4 | allocate 3 units (merge pass runs) | 134200 | LO=1048156 HI=1048156 US=131128 | 1u: 135724, 2u: 135736, 124u: 134236, 128u: 132664 131128 |
| B5 | allocate 128 units | 132664 | LO=1048156 HI=1048156 US=131128 | 1u: 135724, 2u: 135736, 124u: 134236, 128u: 131128 |
| B6 | allocate 128 units | 131128 | LO=1048156 HI=1048156 US=131128 | 1u: 135724, 2u: 135736, 124u: 134236 |
| B7 | allocate 20 units | 134236 | LO=1048156 HI=1048156 US=131128 | 1u: 135724, 2u: 135736, 3u: 135688, 100u: 134488 |

In B4 the working list is 132664, 131128, 134200, 135736. 132664 (128
units) absorbs 134200 and then 135736 and becomes 258 units; 131128
absorbs that and becomes 386 units; it is the only block left on the
list. Redistribution cuts it into 128 units at 131128, 128 at 132664
and 128 at 134200 (pushed in this order) and a rest of 2 units at
135736, which is an exact class. The 3-unit request is then served
from the 128-unit block on top, 134200, whose remaining 125 units are
split into 124 units (134236) and 1 unit (135724).

## 7. Starting a model

A model start happens when a block header asks for a new model (section
3.2) and whenever the model memory is exhausted (section 9.7). It sets:

- the memory state of section 6.1 and 6.3 (pointers, empty free lists,
  merge countdown 0);
- the masked set: empty;
- S = 0; D = M; R = R0; L = 0;
- the root context: allocate one context (it lands on the last unit of
  the arena, offset A − 12), then a block of 128 units (it lands at
  offset A − X). The root has n = 256, T = 257, no suffix, and its
  states are, in order, the symbols 0, 1, ..., 255, each with frequency
  1 and no successor;
- the current context: the root;
- the binary table (7.2) and the escape estimators (7.3).

E is left as it is (its value cannot matter before it is set). The
range decoder is not touched by a model start caused by exhaustion.

### 7.1 Constants

**Escape seeds**, indexed 0..15:

| Index | 0 | 1 | 2 | 3 | 4 | 5 | 6 | 7 | 8 | 9 | 10 | 11 | 12 | 13 | 14 | 15 |
|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|
| Seed | 25 | 14 | 9 | 7 | 5 | 5 | 4 | 4 | 4 | 3 | 3 | 3 | 2 | 2 | 2 | 2 |

**Limits**: frequency ceiling 124; the binary scale 16,384; the binary
adaptation step 128 with shift 7 and rounding term 32.

### 7.2 The binary table

128 rows (index = frequency of the binary context's state, minus 1) by
64 columns (index built in section 8.2). The initial value depends on
the row r and on the column **modulo 8** only:

initial(r, c) = 16,384 − floor(B[c modulo 8] / (r + 2)),

with B = 15581, 7999, 22975, 18675, 25761, 23228, 26162, 24657 for c
modulo 8 = 0..7 (`0x3cdd`, `0x1f3f`, `0x59bf`, `0x48f3`, `0x64a1`,
`0x5abc`, `0x6632`, `0x6051`). In full, for columns 0..7 (columns 8..15,
16..23, ..., 56..63 repeat them):

| Row | c=0 | c=1 | c=2 | c=3 | c=4 | c=5 | c=6 | c=7 |
|--:|--:|--:|--:|--:|--:|--:|--:|--:|
| 0 | 8594 | 12385 | 4897 | 7047 | 3504 | 4770 | 3303 | 4056 |
| 1 | 11191 | 13718 | 8726 | 10159 | 7797 | 8642 | 7664 | 8165 |
| 2 | 12489 | 14385 | 10641 | 11716 | 9944 | 10577 | 9844 | 10220 |
| 3 | 13268 | 14785 | 11789 | 12649 | 11232 | 11739 | 11152 | 11453 |
| 4 | 13788 | 15051 | 12555 | 13272 | 12091 | 12513 | 12024 | 12275 |
| 5 | 14159 | 15242 | 13102 | 13717 | 12704 | 13066 | 12647 | 12862 |
| 6 | 14437 | 15385 | 13513 | 14050 | 13164 | 13481 | 13114 | 13302 |
| 7 | 14653 | 15496 | 13832 | 14309 | 13522 | 13804 | 13478 | 13645 |
| 8 | 14826 | 15585 | 14087 | 14517 | 13808 | 14062 | 13768 | 13919 |
| 9 | 14968 | 15657 | 14296 | 14687 | 14043 | 14273 | 14006 | 14143 |
| 10 | 15086 | 15718 | 14470 | 14828 | 14238 | 14449 | 14204 | 14330 |
| 11 | 15186 | 15769 | 14617 | 14948 | 14403 | 14598 | 14372 | 14488 |
| 12 | 15272 | 15813 | 14743 | 15051 | 14544 | 14725 | 14516 | 14623 |
| 13 | 15346 | 15851 | 14853 | 15139 | 14667 | 14836 | 14640 | 14741 |
| 14 | 15411 | 15885 | 14949 | 15217 | 14774 | 14933 | 14749 | 14843 |
| 15 | 15468 | 15914 | 15033 | 15286 | 14869 | 15018 | 14846 | 14934 |
| 16 | 15519 | 15940 | 15108 | 15347 | 14953 | 15094 | 14931 | 15015 |
| 17 | 15564 | 15963 | 15175 | 15402 | 15029 | 15162 | 15008 | 15087 |
| 18 | 15605 | 15985 | 15236 | 15451 | 15096 | 15223 | 15076 | 15152 |
| 19 | 15643 | 16004 | 15290 | 15495 | 15158 | 15278 | 15139 | 15210 |
| 20 | 15676 | 16021 | 15340 | 15536 | 15214 | 15329 | 15195 | 15264 |
| 21 | 15707 | 16037 | 15386 | 15573 | 15264 | 15375 | 15247 | 15312 |
| 22 | 15735 | 16051 | 15427 | 15606 | 15311 | 15417 | 15294 | 15357 |
| 23 | 15761 | 16065 | 15465 | 15637 | 15354 | 15455 | 15338 | 15398 |
| 24 | 15785 | 16077 | 15501 | 15666 | 15394 | 15491 | 15378 | 15436 |
| 25 | 15807 | 16088 | 15534 | 15693 | 15430 | 15524 | 15416 | 15471 |
| 26 | 15828 | 16099 | 15564 | 15718 | 15464 | 15555 | 15450 | 15504 |
| 27 | 15847 | 16109 | 15592 | 15741 | 15496 | 15584 | 15482 | 15534 |
| 28 | 15865 | 16118 | 15619 | 15762 | 15526 | 15610 | 15512 | 15563 |
| 29 | 15882 | 16126 | 15643 | 15782 | 15553 | 15635 | 15541 | 15589 |
| 30 | 15898 | 16135 | 15667 | 15801 | 15579 | 15659 | 15567 | 15614 |
| 31 | 15912 | 16142 | 15688 | 15819 | 15604 | 15681 | 15592 | 15637 |
| 32 | 15926 | 16149 | 15709 | 15835 | 15627 | 15701 | 15615 | 15659 |
| 33 | 15939 | 16156 | 15728 | 15851 | 15648 | 15721 | 15637 | 15680 |
| 34 | 15952 | 16162 | 15746 | 15866 | 15669 | 15739 | 15658 | 15700 |
| 35 | 15963 | 16168 | 15764 | 15880 | 15688 | 15757 | 15677 | 15718 |
| 36 | 15974 | 16174 | 15780 | 15893 | 15707 | 15773 | 15696 | 15736 |
| 37 | 15985 | 16179 | 15795 | 15906 | 15724 | 15789 | 15714 | 15752 |
| 38 | 15995 | 16185 | 15810 | 15918 | 15740 | 15804 | 15730 | 15768 |
| 39 | 16004 | 16189 | 15824 | 15929 | 15756 | 15818 | 15746 | 15783 |
| 40 | 16014 | 16194 | 15837 | 15940 | 15771 | 15831 | 15762 | 15797 |
| 41 | 16022 | 16198 | 15850 | 15950 | 15785 | 15844 | 15776 | 15811 |
| 42 | 16030 | 16203 | 15862 | 15960 | 15799 | 15857 | 15790 | 15824 |
| 43 | 16038 | 16207 | 15874 | 15969 | 15812 | 15868 | 15803 | 15837 |
| 44 | 16046 | 16211 | 15885 | 15979 | 15824 | 15880 | 15816 | 15848 |
| 45 | 16053 | 16214 | 15896 | 15987 | 15836 | 15890 | 15828 | 15860 |
| 46 | 16060 | 16218 | 15906 | 15995 | 15848 | 15901 | 15839 | 15871 |
| 47 | 16067 | 16221 | 15916 | 16003 | 15859 | 15910 | 15851 | 15881 |
| 48 | 16073 | 16225 | 15925 | 16011 | 15869 | 15920 | 15861 | 15891 |
| 49 | 16079 | 16228 | 15934 | 16018 | 15879 | 15929 | 15872 | 15901 |
| 50 | 16085 | 16231 | 15943 | 16025 | 15889 | 15938 | 15881 | 15910 |
| 51 | 16091 | 16234 | 15951 | 16032 | 15898 | 15946 | 15891 | 15919 |
| 52 | 16096 | 16236 | 15959 | 16039 | 15907 | 15954 | 15900 | 15928 |
| 53 | 16101 | 16239 | 15967 | 16045 | 15916 | 15962 | 15909 | 15936 |
| 54 | 16106 | 16242 | 15974 | 16051 | 15924 | 15970 | 15917 | 15944 |
| 55 | 16111 | 16244 | 15981 | 16057 | 15933 | 15977 | 15926 | 15952 |
| 56 | 16116 | 16247 | 15988 | 16063 | 15940 | 15984 | 15933 | 15959 |
| 57 | 16120 | 16249 | 15995 | 16068 | 15948 | 15991 | 15941 | 15967 |
| 58 | 16125 | 16251 | 16002 | 16073 | 15955 | 15997 | 15948 | 15974 |
| 59 | 16129 | 16253 | 16008 | 16078 | 15962 | 16004 | 15956 | 15980 |
| 60 | 16133 | 16255 | 16014 | 16083 | 15969 | 16010 | 15963 | 15987 |
| 61 | 16137 | 16258 | 16020 | 16088 | 15976 | 16016 | 15969 | 15993 |
| 62 | 16141 | 16260 | 16026 | 16093 | 15982 | 16022 | 15976 | 15999 |
| 63 | 16145 | 16261 | 16031 | 16097 | 15988 | 16027 | 15982 | 16005 |
| 64 | 16148 | 16263 | 16036 | 16102 | 15994 | 16033 | 15988 | 16011 |
| 65 | 16152 | 16265 | 16042 | 16106 | 16000 | 16038 | 15994 | 16016 |
| 66 | 16155 | 16267 | 16047 | 16110 | 16006 | 16043 | 16000 | 16022 |
| 67 | 16159 | 16269 | 16052 | 16114 | 16011 | 16048 | 16005 | 16027 |
| 68 | 16162 | 16270 | 16056 | 16118 | 16016 | 16053 | 16011 | 16032 |
| 69 | 16165 | 16272 | 16061 | 16121 | 16022 | 16057 | 16016 | 16037 |
| 70 | 16168 | 16273 | 16065 | 16125 | 16027 | 16062 | 16021 | 16042 |
| 71 | 16171 | 16275 | 16070 | 16129 | 16032 | 16066 | 16026 | 16047 |
| 72 | 16174 | 16276 | 16074 | 16132 | 16036 | 16071 | 16031 | 16051 |
| 73 | 16177 | 16278 | 16078 | 16135 | 16041 | 16075 | 16036 | 16056 |
| 74 | 16179 | 16279 | 16082 | 16139 | 16046 | 16079 | 16040 | 16060 |
| 75 | 16182 | 16281 | 16086 | 16142 | 16050 | 16083 | 16045 | 16064 |
| 76 | 16185 | 16282 | 16090 | 16145 | 16054 | 16087 | 16049 | 16068 |
| 77 | 16187 | 16283 | 16094 | 16148 | 16058 | 16090 | 16053 | 16072 |
| 78 | 16190 | 16285 | 16097 | 16151 | 16062 | 16094 | 16057 | 16076 |
| 79 | 16192 | 16286 | 16101 | 16154 | 16066 | 16098 | 16062 | 16080 |
| 80 | 16194 | 16287 | 16104 | 16157 | 16070 | 16101 | 16065 | 16084 |
| 81 | 16197 | 16288 | 16108 | 16159 | 16074 | 16105 | 16069 | 16087 |
| 82 | 16199 | 16289 | 16111 | 16162 | 16078 | 16108 | 16073 | 16091 |
| 83 | 16201 | 16290 | 16114 | 16165 | 16081 | 16111 | 16077 | 16094 |
| 84 | 16203 | 16291 | 16117 | 16167 | 16085 | 16114 | 16080 | 16098 |
| 85 | 16205 | 16293 | 16120 | 16170 | 16088 | 16118 | 16084 | 16101 |
| 86 | 16207 | 16294 | 16123 | 16172 | 16092 | 16121 | 16087 | 16104 |
| 87 | 16209 | 16295 | 16126 | 16175 | 16095 | 16124 | 16091 | 16107 |
| 88 | 16211 | 16296 | 16129 | 16177 | 16098 | 16126 | 16094 | 16111 |
| 89 | 16213 | 16297 | 16132 | 16179 | 16101 | 16129 | 16097 | 16114 |
| 90 | 16215 | 16298 | 16135 | 16182 | 16104 | 16132 | 16100 | 16116 |
| 91 | 16217 | 16298 | 16137 | 16184 | 16107 | 16135 | 16103 | 16119 |
| 92 | 16219 | 16299 | 16140 | 16186 | 16110 | 16137 | 16106 | 16122 |
| 93 | 16220 | 16300 | 16143 | 16188 | 16113 | 16140 | 16109 | 16125 |
| 94 | 16222 | 16301 | 16145 | 16190 | 16116 | 16143 | 16112 | 16128 |
| 95 | 16224 | 16302 | 16148 | 16192 | 16119 | 16145 | 16115 | 16130 |
| 96 | 16226 | 16303 | 16150 | 16194 | 16122 | 16147 | 16118 | 16133 |
| 97 | 16227 | 16304 | 16152 | 16196 | 16124 | 16150 | 16120 | 16135 |
| 98 | 16229 | 16305 | 16155 | 16198 | 16127 | 16152 | 16123 | 16138 |
| 99 | 16230 | 16305 | 16157 | 16200 | 16129 | 16155 | 16125 | 16140 |
| 100 | 16232 | 16306 | 16159 | 16201 | 16132 | 16157 | 16128 | 16143 |
| 101 | 16233 | 16307 | 16161 | 16203 | 16134 | 16159 | 16130 | 16145 |
| 102 | 16235 | 16308 | 16164 | 16205 | 16137 | 16161 | 16133 | 16147 |
| 103 | 16236 | 16308 | 16166 | 16207 | 16139 | 16163 | 16135 | 16150 |
| 104 | 16238 | 16309 | 16168 | 16208 | 16141 | 16165 | 16138 | 16152 |
| 105 | 16239 | 16310 | 16170 | 16210 | 16144 | 16167 | 16140 | 16154 |
| 106 | 16240 | 16310 | 16172 | 16212 | 16146 | 16169 | 16142 | 16156 |
| 107 | 16242 | 16311 | 16174 | 16213 | 16148 | 16171 | 16144 | 16158 |
| 108 | 16243 | 16312 | 16176 | 16215 | 16150 | 16173 | 16147 | 16160 |
| 109 | 16244 | 16312 | 16178 | 16216 | 16152 | 16175 | 16149 | 16162 |
| 110 | 16245 | 16313 | 16179 | 16218 | 16154 | 16177 | 16151 | 16164 |
| 111 | 16247 | 16314 | 16181 | 16219 | 16157 | 16179 | 16153 | 16166 |
| 112 | 16248 | 16314 | 16183 | 16221 | 16159 | 16181 | 16155 | 16168 |
| 113 | 16249 | 16315 | 16185 | 16222 | 16160 | 16183 | 16157 | 16170 |
| 114 | 16250 | 16316 | 16186 | 16224 | 16162 | 16184 | 16159 | 16172 |
| 115 | 16251 | 16316 | 16188 | 16225 | 16164 | 16186 | 16161 | 16174 |
| 116 | 16252 | 16317 | 16190 | 16226 | 16166 | 16188 | 16163 | 16176 |
| 117 | 16254 | 16317 | 16191 | 16228 | 16168 | 16189 | 16165 | 16177 |
| 118 | 16255 | 16318 | 16193 | 16229 | 16170 | 16191 | 16166 | 16179 |
| 119 | 16256 | 16318 | 16195 | 16230 | 16172 | 16193 | 16168 | 16181 |
| 120 | 16257 | 16319 | 16196 | 16231 | 16173 | 16194 | 16170 | 16182 |
| 121 | 16258 | 16319 | 16198 | 16233 | 16175 | 16196 | 16172 | 16184 |
| 122 | 16259 | 16320 | 16199 | 16234 | 16177 | 16197 | 16174 | 16186 |
| 123 | 16260 | 16321 | 16201 | 16235 | 16178 | 16199 | 16175 | 16187 |
| 124 | 16261 | 16321 | 16202 | 16236 | 16180 | 16200 | 16177 | 16189 |
| 125 | 16262 | 16322 | 16204 | 16237 | 16182 | 16202 | 16178 | 16190 |
| 126 | 16263 | 16322 | 16205 | 16239 | 16183 | 16203 | 16180 | 16192 |
| 127 | 16264 | 16322 | 16206 | 16240 | 16185 | 16204 | 16182 | 16193 |

### 7.3 The escape estimators

A 25 × 16 table of records, each with three fields:

| Field | Width | Initial value in row r (0..24), every column |
|---|---|---|
| sum | unsigned 16 bits, arithmetic modulo 65,536 | (5 × r + 10) × 8, that is 80, 120, 160, ..., 1040 |
| shift | small integer | 3 |
| count | unsigned 8 bits | 4 |

The row is chosen by the number u of unmasked symbols (1..255):

| Unmasked symbols | Row |
|--:|--:|
| 1 | 0 |
| 2 | 1 |
| 3 | 2 |
| 4 | 3 |
| 5..6 | 4 |
| 7..9 | 5 |
| 10..13 | 6 |
| 14..18 | 7 |
| 19..24 | 8 |
| 25..31 | 9 |
| 32..39 | 10 |
| 40..48 | 11 |
| 49..58 | 12 |
| 59..69 | 13 |
| 70..81 | 14 |
| 82..94 | 15 |
| 95..108 | 16 |
| 109..123 | 17 |
| 124..139 | 18 |
| 140..156 | 19 |
| 157..174 | 20 |
| 175..193 | 21 |
| 194..213 | 22 |
| 214..234 | 23 |
| 235..256 | 24 |

(Rows 0, 1, 2 hold one value each; from row 3 on, row g holds g − 2
consecutive values.)

## 8. Decoding one symbol

### 8.1 Outline

1. Empty the masked set. Let F be the current context.
2. If F is binary, do the binary step (8.2), otherwise the full step
   (8.3). Either a state is found, or the step **escapes**.
3. After an escape: add 1 to D and replace F by its suffix; **if F had
   no suffix (it was the root), the input is corrupt**. Repeat this
   until F has **more states than there are masked symbols**. Then do
   the masked step (8.4) in F; it finds a state or escapes again (go
   back to the start of this step).
4. A state X of context F has been found. Its symbol is the decoded
   symbol. Update the model (section 9), then set L to the symbol;
   but if that update restarted the model (9.7), L is 0.

The context in which step 1 started is called the **start context**
below; F at the end is the **found context**. They are the same exactly
when no escape occurred.

### 8.2 Binary step

F has the single state (symbol s, frequency f). Its suffix G always
exists.

- Row: f − 1.
- Column: the sum of
  - S (0 or 1);
  - 0, 2, 4 or 6 according to the number of states of G: 1 → 0; 2 → 2;
    3..11 → 4; 12 or more → 6;
  - 8 if L ≥ 64;
  - 16 if s ≥ 64;
  - 32 if R < 0.
- Let p be the table entry. Make a binary decision with probability p
  (section 4.3).
- **Yes** (the symbol is s): S = 1; R increases by 1; if f < 128, f
  increases by 1; the table entry becomes
  p + 128 − ((p + 32) >> 7). The state is found.
- **No** (escape): S = 0; the table entry becomes
  p' = p − ((p + 32) >> 7); E becomes the escape seed whose index is
  p' >> 10 (table of 7.1; the index is 0..15); the masked set becomes
  the single symbol s.

(The entry is the one selected before f changed. Entries stay within
95..16,352, so both outcomes always have a non-empty interval.)

### 8.3 Full step

F has n ≥ 2 states with frequencies f0, f1, ... in list order, and total
T. Query the range decoder with scale T, giving v.

- **v < f0**: narrow to (0, f0). S becomes 1 if 2 × f0 > T (values
  before the update), else 0; if it became 1, R increases by 1. Then f0
  and T each increase by 4. If f0 is now greater than 124, rescale F
  (8.5). The first state is found.
- Otherwise S = 0, and the states are examined in order from the second:
  with c the sum of the frequencies before state i, the first state i
  with v < c + fi is the one. Narrow to (c, c + fi). fi and T each
  increase by 4. **If fi is now greater than the frequency of the state
  just before it in the list, the two states exchange places**, and
  then, if fi is greater than 124, rescale F. (No exchange: no rescale
  check; the frequency cannot exceed 124 in that case.) The state is
  found.
- **v at or beyond the sum of all frequencies**: escape. S = 0. Narrow
  to (sum, T). The masked set becomes all n symbols of F.

### 8.4 Masked step

F has n states of which m are masked; u = n − m ≥ 1 are not.

**Escape estimate.** If n = 256, the estimate e is 1 and no estimator is
involved. Otherwise select the estimator in the row given by u (7.3) and
the column that is the sum of

- 1 if u < (number of states of F's suffix) − n;
- 2 if T < 11 × n;
- 4 if m > u;
- 8 if L ≥ 64;

then e = sum >> shift; the estimator's sum decreases by e; and if e is
0 it is taken as 1. (A context with fewer than 256 states always has a
suffix.)

**Decoding.** Let Q be the sum of the frequencies of the unmasked
states. Query with scale e + Q, giving v.

- **v < Q**: going through the unmasked states in list order with
  running sum c, the first with v < c + f is the one. Narrow to
  (c, c + f). Then:
  - the estimator, if one was used and its shift is below 7: its count
    decreases by 1; if the count is now 0, then sum = 2 × sum, count =
    3 × 2^shift (24, 48, 96, 192 for shift 3, 4, 5, 6), and shift
    increases by 1;
  - f and T each increase by 4; if f is greater than 124, rescale F
    (8.5). **No exchange of places is made in this step**;
  - R = R0. (S stays 0, as the escape left it.)

  The state is found.
- **Otherwise**: escape. Narrow to (Q, e + Q). The estimator, if one was
  used, has e + Q added to its sum. All unmasked symbols of F join the
  masked set (so that now all n are masked).

### 8.5 Rescaling a context

Rescaling is applied to a context F with n ≥ 2 states immediately after
the frequency of its state X has been raised above 124 (8.3, 8.4). D
has the value it has at that moment, including the increments of this
symbol's escapes.

1. X's frequency increases by 4 **again**.
2. Let esc = T + 4 − (the sum of all frequencies, X's included as just
   raised). (Signed arithmetic; never below 1 in the survey.)
3. Let a = 0 if D = 0, otherwise 1. Go through the states in list order.
   For each: its frequency f becomes (f + a) >> 1. Then, if it is not
   the first and its new frequency is **greater** than that of the state
   before it, move it toward the front, past every immediately preceding
   state whose frequency is smaller than its own, and insert it there
   (states of equal frequency keep their relative order).
4. T becomes the sum of the new frequencies.
5. If the last state now has frequency 0: let z be the number of states
   of frequency 0 at the end of the list. Remove them (n decreases by
   z) and add z to esc.
   - **If one state is left** (it is X): halve both numbers together,
     its frequency f becoming (f + 1) >> 1 and esc becoming esc >> 1;
     do this **once in any case, and again as long as esc is still
     greater than 1** after a halving. Release the block of states (it
     was a block of ceil(n_before / 2) units, n_before being the number
     of states before the removal). F is now a binary context holding
     X with that frequency and its successor. Rescaling is finished; T
     no longer exists.
   - Otherwise, if ceil(n_before / 2) ≠ ceil(n / 2), shrink the block
     of states from the former to the latter number of units (6.6).
6. T increases by (esc + 1) >> 1.
7. X is now the first state of the list (its halved frequency, at least
   64, exceeds every other, at most 62); it remains the found state.

Frequencies of 0 arise only when a = 0, that is, in contexts of maximum
order; elsewhere a frequency of 1 stays 1.

## 9. Updating the model

Input: the start context, the found context F, the found state X with
its symbol s, its frequency q **as left by section 8** (including a
rescale) and its successor, and D.

### 9.1 The short cut

If D = 0 and X's successor is a context reference: that context becomes
the current context, and **nothing else changes**. (Maximum order, and
the next context exists already.)

Otherwise continue with 9.2 to 9.5. If at any point below "the model is
restarted", do section 9.7 and stop; whatever was half done is
irrelevant.

### 9.2 Credit the suffix context

Let V be "no state". If q < 31 and F has a suffix G:

- If G has n ≥ 2 states: find the state of G whose symbol is s (it
  exists). If it is not the first of the list and its frequency is
  **greater than or equal to** that of the state before it, the two
  exchange places. Then, if its frequency is less than 115, its
  frequency and G's total each increase by 2. V is this state.
- If G is binary: V is its state; if its frequency is less than 32, it
  increases by 1.

### 9.3 Three cases

**Case A: D = 0.** (X's successor is a text reference here.) Build the
successor chain (9.4) *without* X itself; if that fails, the model is
restarted. Otherwise the resulting context becomes X's successor and
the current context. No text byte is written. Done.

**Otherwise** write s into the text area at TP and increase TP by 1.
**If TP is now at or beyond US, the model is restarted.** Let W be the
text reference TP (the position after the byte just written).

**Case B: X has a successor.**

1. If it is a text reference: build the successor chain (9.4)
   *including* X; if that fails, the model is restarted. X's successor
   is now a context. (If it already was a context, nothing is built.)
2. Call that context N. Decrease D by 1.
3. If D is now 0: W becomes a reference to N instead of a text
   reference, and, **if the start context differs from the found
   context, TP decreases by 1** (the byte just written will be
   overwritten by the next one).

**Case C: X has no successor.** X's successor becomes W. Let N be F
itself: the model stays in the found context, and D stays as the
escapes left it.

Then, in cases B and C, add the symbol to the contexts that escaped
(9.5), and make N the current context.

### 9.4 Building the successor chain

X's successor is a text reference to offset t. The contexts that should
follow s, from the found context down toward the root, do not exist as
long as their states still hold this same text reference; this step
creates them.

1. Start an empty list. Let K = F.
2. If X is to be included: put X on the list; if K has no suffix, go to
   step 5.
3. If V is a state (9.2): let K be the suffix of F (the context V lives
   in). If V's successor is **not** the text reference t: let K be the
   context V's successor designates, and go to step 5. Otherwise put V
   on the list; if K has no suffix, go to step 5.
4. Repeat: let K be the suffix of K. Take the state of K whose symbol
   is s (the only state if K is binary; it always exists). If its
   successor is not the text reference t: let K be the context that
   successor designates, and leave the loop. Otherwise put the state on
   the list, and leave the loop if K has no suffix.
5. If the list is empty, the result is K. Otherwise:
   - the new contexts' single symbol is c = the text byte at offset t,
     with successor: text reference t + 1;
   - its frequency g: if K is binary, g is the frequency of K's state.
     Otherwise take the state of K whose symbol is c (it exists), with
     frequency h; let a = h − 1 and b = T(K) − n(K) − a. If 2a ≤ b: g is
     2 if 5a > b, else 1. If 2a > b: g = 1 + floor((2a + 3b − 1) / (2b));
   - go through the list **from the last entry to the first**. For each
     entry: allocate a context (6.4); if that fails, the chain fails.
     The new context is binary with the state (c, g, t + 1) and has K
     as its suffix; the listed state's successor becomes the new
     context; K becomes the new context.
   - The result is K (the context created for the first entry).

The list never has more than M entries. In step 5, b is at least 1 in
valid data; see section 14 for what to check.

### 9.5 Adding the symbol to the contexts that escaped

Skip this when the start context is the found context. Otherwise let
nF = the number of states of F (at least 2 here), and
s0 = T(F) − nF − (q − 1).

For each context K from the start context along the suffix links up to,
**but not including**, F:

1. If K has n ≥ 2 states:
   - if n is even, grow its block of states from n / 2 to n / 2 + 1
     units (6.6); if that fails, the model is restarted;
   - if 4n ≤ nF **and** T(K) ≤ 8n, T(K) increases by 2;
   - if 2n < nF, T(K) increases by 1.

   If K is binary: allocate a block of 1 unit for its states (failure:
   the model is restarted); its state's frequency y becomes 2y if
   y < 30, otherwise 120; K is no longer binary and gets
   T(K) = (new y) + E + (1 if nF > 3, else 0).
2. Let cf = 2 × q × (T(K) + 6) and sf = s0 + T(K). The new frequency:

   | Condition | New frequency | T(K) increases by |
   |---|--:|--:|
   | cf ≤ sf | 1 | 3 |
   | sf < cf < 4 sf | 2 | 3 |
   | 4 sf ≤ cf < 6 sf | 3 | 3 |
   | 6 sf ≤ cf < 9 sf | 4 | 4 |
   | 9 sf ≤ cf < 12 sf | 5 | 5 |
   | 12 sf ≤ cf < 15 sf | 6 | 6 |
   | 15 sf ≤ cf | 7 | 7 |

3. Append a state (s, that frequency, successor W) at the **end** of
   K's list.

### 9.6 Summary of the counters

- D: set to M at a model start; +1 for every step down the suffix chain
  after an escape (8.1); −1 in case B. It is not changed in cases A and
  C or by the short cut.
- R: +1 on a binary "yes" and on a confident first-state hit (8.3); set
  to R0 by a masked-step hit; set to R0 at a model start.
- S: set by the binary and full steps only.
- L: the last decoded symbol; 0 after a model start, **including one
  caused by exhaustion**: the symbol whose update restarted the model
  does not become L.

### 9.7 Restart on exhaustion

When the text pointer reaches the units area (9.3), or an allocation
fails (9.3 case A, 9.4, 9.5): the symbol just decoded is still
delivered; then the whole model is started anew as in section 7, with
the same M and the same arena size. Decoding continues in the same
block with the range decoder as it stands. The masked set is empty for
the next symbol in any case.

## 10. The RAR layer on top of the symbols

### 10.1 Items

Inside a PPMd block the decoder repeats: decode one symbol y (section
8).

- **If y is not the escape value** (section 3.3), y is a **literal**:
  one output byte.
- **If y is the escape value**, decode one more symbol, the
  **sub-code**, and act on it:

| Sub-code | Further symbols | Meaning |
|--:|---|---|
| 0 | none | **Block switch**: this PPMd block ends; a block header follows (10.3). No output. |
| 1 | none | **Literal escape**: one output byte equal to the escape value. |
| 2 | none | **End of member** (section 11). No output. |
| 3 | a filter record (10.5) | **Filter transmission**. No output. |
| 4 | four: d1, d2, d3, n | **Match**: distance (d1 × 65,536 + d2 × 256 + d3) + 2, length n + 32. |
| 5 | one: n | **Run**: a match of distance 1 and length n + 4. |
| 6..255 | none | As sub-code 1: one output byte equal to the escape value. |

Every symbol mentioned here, the sub-code and the operand symbols
included, is decoded by the model in the ordinary way and updates the
model like any other symbol. Operand symbols are **values**: they are
not compared with the escape value. The escape value in force when a
symbol is decoded is the one that counts; a block header inside the
member can change it (section 3.3).

Evidence: sub-codes 1, 2, 4 and 5 are CONFIRMED (RAR-written) (30,272,
2,445, 24,213 and 42,669 occurrences in the survey); sub-code 0 is
CONFIRMED (RAR-written) once and CONFIRMED (crafted); sub-code 3 and
sub-codes 6, 7 and 255 are CONFIRMED (crafted) only in this document's
own experiments (`rar` 4.20 and 6.24, as driven here, sent filter
records in an LZ block placed before the PPMd block, and they write
sub-code 1 for a literal escape). `rar29-lz.md`, whose survey covers
other archives, reports sub-codes 0 and 3 seventy times each in real
data; in the real archives of section 18 every PPMd block that holds a
sub-code 3 ends with sub-code 0 and is followed by an LZ block: the
encoder announces a filter and switches to LZ for the data the filter
is meant for.
Accepting 6..255 follows the reference implementation and `bsdtar`
(DECISION).

### 10.2 Matches

- Sub-code 4: distances 2..16,777,217, lengths 32..287. Sub-code 5:
  distance 1, lengths 4..259. (CONFIRMED (crafted) at both ends of both
  length ranges and for distance 2; lengths 257 and distances up to
  several hundred thousand are CONFIRMED (RAR-written).)
- A match is executed on the group's window as described in section 2,
  item 2.
- **A distance greater than the number of bytes the group has produced
  so far, or greater than the window size of `rar29-lz.md`, is corrupt
  input.** This includes sub-code 5 when the group has produced nothing.
  (DECISION. Both black-box tools copy zero bytes from an untouched
  window instead: vectors E6 and E7.)
- A match that would produce more bytes than the member still lacks is
  corrupt input (DECISION, section 11.2).
- Matches of a PPMd block do not enter the LZ match memory (section 2,
  item 4).

### 10.3 Block switch

After sub-code 0 the PPMd block is over. **The next block header starts
at the range decoder's read position** (section 4.3): the byte after
the last byte the range decoder has consumed. Nothing is skipped and
nothing is re-read; a `rar`-written range-coder stream ends exactly
there. That byte is interpreted as in section 2, item 1: top bit 1, a
PPMd block header (section 3), with or without a new model; top bit 0,
an LZ block (`rar29-lz.md`), which the LZ decoder reads as a bit stream
starting at the top bit of that byte.

The model, if the next header does not replace it, and the escape value
stay as they are, also while LZ blocks are being decoded. (CONFIRMED
(RAR-written): PPMd block, sub-code 0, LZ block. CONFIRMED (crafted):
PPMd to PPMd keeping the model; PPMd to PPMd with a new model; PPMd to
LZ and back to PPMd keeping the model, the model then continuing as if
the LZ block had not been there.)

### 10.4 What a PPMd block cannot do

There is no way to end a PPMd block other than sub-codes 0 and 2, and
no length field: the block lasts until one of them is decoded.

### 10.5 Filter record

After sub-code 3:

1. Decode one symbol: the **flags byte** g.
2. The payload length k: let c = (g AND 7) + 1.
   - c = 1..6: k = c.
   - c = 7: decode one symbol x; k = x + 7 (7..262).
   - c = 8: decode two symbols x1, x2; k = x1 × 256 + x2 (0..65,535).
3. Decode k symbols: the payload bytes, in order.

The flags byte and the payload are handed to `rar29-filters.md` exactly
as when an LZ block transmits a filter record with its own symbol; the
record refers to the current output position, that is, the number of
bytes the group has produced when the record is complete. This document
does not interpret them. Only the low three bits of g matter here; the
other five belong to the filter layer. (CONFIRMED (crafted) with `unar`
for all three forms: c = 7 in vector V7, c = 8 and c = 3 in vector V7b,
and c = 1, 2, 4, 5 and 6 in streams built like V7b. `rar29-filters.md`
decides what k = 0 means.)

The symbols of a filter record, like all operand symbols, are never
compared with the escape value: a payload byte equal to it is sent as
itself (the payloads of V7 and V7b contain the byte `02` several
times).

## 11. Members, solid groups, end of data

### 11.1 Start of a member

- Each member has its own packed bytes, read from their first byte.
- A member of unpack version 29 or 36 starts with a **block header**
  when it is the first compressed member of its solid group, and
  whenever the previous member of the group ended in a PPMd block
  (sub-code 2). (After a member that ended in an LZ block,
  `rar29-lz.md` says whether a header follows.) A member that starts
  with a PPMd block header without the new-model bit **continues the
  model of the previous members**, wherever that model was left, even
  if LZ blocks or whole LZ members lie in between. (CONFIRMED
  (RAR-written): 2,246 such headers in the survey. CONFIRMED (crafted).)
- At the start of a solid group there is no model, and the escape value
  is 2.
- **Members with U = 0** follow `rar29-lz.md` (its section on solid
  groups): with P = 0 nothing is read and nothing changes; with P > 0
  the member is decoded like any other, a block header first if one is
  due, then items up to the end marker, which must come before any
  output. After a member that ended in a PPMd block, `rar` stores an
  empty file as 8 packed bytes forming an LZ block that only ends the
  member; a crafted empty member may equally be a PPMd block header
  followed by sub-code 2 (`a3 00 01 fe fc 1c`), and that header creates
  or keeps the model like any other. (CONFIRMED (RAR-written) for the
  8-byte form in the first position and in the middle of a group;
  CONFIRMED (crafted) for the PPMd form.)

### 11.2 End of a member

Let "missing" be U minus the number of bytes this member has produced.

| Situation | Result |
|---|---|
| Sub-code 2 while missing = 0 | **Clean end.** The rest of the member's packed bytes is neither read nor checked. The next member of the group starts with a block header. |
| Sub-code 2 while missing > 0 | Corrupt input (DECISION; `unar` fails, `bsdtar` pads with zeros). |
| A literal, literal escape, match or run while missing = 0 | **Unterminated member** (DECISION, below). |
| A match or run longer than missing, while missing > 0 | Corrupt input (DECISION; `unar` cuts the match off, `bsdtar` fails). |
| Any corrupt input while missing > 0 | Corrupt input. |
| Any corrupt input, including the end of the packed bytes, while missing = 0 | Unterminated member. |

Every member written by `rar` 4.20 and 6.24 ends with sub-code 2
exactly when missing = 0 (2,445 PPMd-ended members in the survey).
After it, `rar` writes further packed bytes which no decoder needs: the
bytes the range coder flushes and then, in every member, the same eight
bytes `00 bf 88 67 f6 a9 ff d4` (they have the form of an LZ block that
ends the member at once). They are ignored under the first row.

So the decoder does **not** stop by itself when missing reaches 0: it
goes on decoding items, expecting sub-code 2. Items that produce no
output (sub-code 3, and sub-code 0 with the blocks after it) are
processed normally meanwhile. If something else comes, the member is
**unterminated**: its U bytes are complete and are delivered, subject to
the CRC-32 check, but the decoder state is no longer trustworthy, and
every later member of the same solid group is reported as failed
("depends on a damaged member") without being decoded. (DECISION. The
reference implementation cannot decode such a member at all, `bsdtar`
delivers it; no `rar`-written member is of this kind. The rule lets a
last or only member that lacks its end marker be read, and never lets
doubtful state reach another member.)

### 11.3 Failures

When a member ends in corrupt input, nothing is delivered for it and
the group's state (model, escape value, window, LZ state) is discarded;
later members of the group are reported as failed without decoding
(DECISION, as in `rar20.md`). A CRC-32 mismatch alone, after a clean
end, does not have this effect.

### 11.4 Files split across volumes

The container layer concatenates the data areas of the pieces of a
split member in volume order (`rar4-container.md`), and the result is
decoded as one member: the block header is in the first piece only,
nothing is aligned, repeated or reset where a piece ends, and the range
decoder cannot tell where the boundaries were. (CONFIRMED
(RAR-written): a PPMd-coded member of 132,779 bytes written by `rar`
6.24 into volumes of 20,480 bytes (2 pieces) and again into volumes of
3,072 bytes (11 pieces), each time followed in the last volume by a
second member of the same
solid group that keeps the model; both decode to their CRC-32 from the
concatenated pieces.)

## 12. Worked examples

### 12.1 A complete member

Packed bytes (16 bytes), U = 12, first member of a group:

`a1 00 61 03 6d e8 96 2e d0 81 5a 1f aa d2 55 6c`

Header: `a1` = 1010 0001: PPMd block, no escape byte, new model, order
code 1, so M = 2 and R0 = −3. `00`: 1 MiB, so the text area is offsets
0..131,127, US = LO = 131,128 (`0x20038`), HI = 1,048,576. The escape
value stays 2. Model start: the root context at offset 1,048,564
(`0xFFFF4`), its 128-unit block at 131,128, LO = 132,664 (`0x20638`),
D = 2, R = −3, S = 0, L = 0. Range decoder: low = 0, range =
`ffffffff`, code = `61036de8`; 6 bytes have been read.

Fourteen symbols are decoded. "unit" is the range after the division of
the query; contexts are written as their state lists with frequencies.
In the root, only the order of the first states and the states with a
frequency above 1 are shown; "(k)" stands for the symbol with byte value
k.

**Symbol 1.** Current context: root, 256 states of frequency 1, T =
257. Full step: unit = floor(`ffffffff` / 257) = `00ff00ff`; v =
floor(`61036de8` / `00ff00ff`) = 97. The first state has frequency 1,
v ≥ 1; cumulative sums run 1 per state, so state number 97 (symbol 97,
`a`) covers (97, 98). Narrow: low = 97 × `00ff00ff` = `609f609f`, range
= `00ff00ff`. Normalise: the top bytes of low (`60`) and low + range
(`61`) differ and range ≥ 32,768: stop, nothing read. `a` gets
frequency 5, T = 261; 5 is greater than the frequency 1 of the state
before it (symbol 96), so they exchange places: the root's list is now
(0), ..., (95), `a`, (96), `b`, `c`, ... S = 0.

Update: q = 5; the root has no suffix (9.2 does nothing); D = 2, not 0.
Text offset 0 receives `a`, TP = 1, W = text reference 1. Case C (no
successor): `a`'s successor becomes text reference 1; N is the root.
Start context = found context, so 9.5 is skipped. Current context:
root. L = `a`.

**Symbol 2.** Root, T = 261. unit = `0000fa1e`, v = 102. Sums: 96
states of frequency 1 (0..95) reach 96; `a` (5) reaches 101; (96)
reaches 102; `b` covers (102, 103). Narrow: low = `609f609f` + 102 ×
`fa1e` = `61030893`, range = `0000fa1e`. Normalise: low + range =
`610402b1`; the top bytes are equal (`61`), so one byte is read: code =
`036de896`, range = `00fa1e00`, low = `03089300`. Now the top bytes
differ (`03`, `04`) and the range is at least 32,768: stop. `b`:
frequency 5, T = 265, exchanged with (96). Update as for symbol 1:
text offset 1 = `b`, TP = 2, `b`'s successor = text reference 2.

**Symbol 3.** Root, T = 265. unit = `0000f19f`, v = 107: 96 + `a` 5 +
`b` 5 + (96) 1 = 107, so `c` covers (107, 108). After narrowing and one
byte: low = `6d907500`, range = `00f19f00`, code = `6de8962e`. `c`:
frequency 5, T = 269, exchanged with (96): list (0)..(95), `a`, `b`,
`c`, (96), `d`, ... Text offset 2 = `c`, TP = 3, successor text
reference 3.

**Symbol 4.** Root, T = 269. unit = `0000e5f1`, v = 98: `a` covers
(96, 101). low = `6d907500` + 96 × `e5f1` = `6de6af60`, range = 5 ×
`e5f1` = `00047db5`; one byte read: low = `e6af6000`, range =
`047db500`, code = `e8962ed0`. `a`: frequency 9, T = 273; 9 > 1, so `a`
exchanges places with (95): list (0)..(94), `a`, (95), `b`, `c`, (96),
`d`, ...

Update: q = 9, no suffix, D = 2. Text offset 3 = `a`, TP = 4, W = text
reference 4. Case B: `a`'s successor is text reference 1, so the chain
is built including X: the list is [`a` of the root]; the root has no
suffix. The new context's symbol is the text byte at offset 1, `b`,
with successor text reference 2. Its frequency: the root is not binary;
`b` has h = 5, a = 4, b = T − n − a = 273 − 256 − 4 = 13; 2a = 8 ≤ 13
and 5a = 20 > 13, so g = 2. A context is allocated: HI = 1,048,552
(`0xFFFE8`); it is the binary context {`b`:2} with the root as suffix
(it stands for the string "a"), and it becomes the successor of the
root's `a`. D becomes 1. Not 0, so W stays. Start = found: nothing to
add. Current context: {`b`:2}. L = `a`.

**Symbol 5.** Binary step in {`b`:2}. Row 2 − 1 = 1. Column: S = 0;
the suffix (root) has 256 states: 6; L = `a` ≥ 64: 8; `b` ≥ 64: 16;
R = −3 < 0: 32; total 62. Entry (row 1, column 62, i.e. column 6 of the
table in 7.2) = 7664. Query with scale 16,384: unit = `000011f6`, v =
floor((`e8962ed0` − `e6af6000`) / `11f6`) = 6938 < 7664: **yes**.
Narrow to (0, 7664): low unchanged, range = `0219b4a0`; nothing read.
S = 1, R = −2, `b`'s frequency in this context becomes 3, the entry
becomes 7664 + 128 − ((7664 + 32) >> 7) = 7664 + 128 − 60 = 7732.

Update: q = 3 < 31 and the suffix is the root: its `b` has frequency 5,
not less than the 1 of (95) before it: they exchange places (list
..., `a`, `b`, (95), `c`, ...); 5 < 115, so `b` becomes 7 and T = 275.
V = the root's `b`. D = 1. Text offset 4 = `b`, TP = 5, W = text
reference 5. Case B: X's successor is text reference 2; build including
X: list [X]; K has a suffix; V's successor is the same text reference
2, so V joins the list: [X, V]; K = root has no suffix. New symbol: text
byte at offset 2, `c`, successor text reference 3; frequency from the
root: h = 5, a = 4, b = 275 − 256 − 4 = 15, g = 2. Contexts are created
from the last list entry: first for V, at `0xFFFDC`: {`c`:2}, suffix
root (the string "b"), now the successor of the root's `b`; then for X,
at `0xFFFD0`: {`c`:2}, suffix the context at `0xFFFDC` (the string
"ab"), now the successor of X. D becomes 0, so W becomes a reference to
the context at `0xFFFD0`; start = found, so TP stays 5. Current
context: `0xFFFD0`.

**Symbol 6.** Binary step in {`c`:2} at `0xFFFD0`. Row 1. Column: S =
1; suffix has 1 state: 0; L = `b`: 8; `c`: 16; R = −2: 32; total 57 =
column 1 of the table: 13,718. unit = `00000866`, v = 14,838 ≥ 13,718:
**no**, escape. Narrow to (13,718, 16,384); one byte read: low =
`7169c400`, range = `57763c00`, code = `962ed081`. S = 0; the entry
becomes 13,718 − ((13,718 + 32) >> 7) = 13,718 − 107 = 13,611; E =
the seed with index 13,611 >> 10 = 13, which is 2; masked set {`c`}.

Escape: D = 1, F = `0xFFFDC` {`c`:2}: one state, one masked symbol, not
more: go on. D = 2, F = root: 256 > 1. Masked step in the root: n = 256,
so e = 1 and no estimator. Q = 274 − 5 = 269 (`c` is masked); scale
270. unit = `0052ed42`, v = 113. Unmasked states in order: (0)..(94)
reach 95; `a` 9: 104; `b` 7: 111; (95): 112; `c` is skipped; (96): 113;
`d` covers (113, 114). After narrowing and one byte: low = `047e2200`,
range = `52ed4200`, code = `2ed0815a`. `d`: frequency 5, root T = 279,
**no exchange**; R = −3.

Update: found context root, start context `0xFFFD0`. q = 5, no suffix.
D = 2. Text offset 5 = `d`, TP = 6, W = text reference 6. Case C: `d`'s
successor = text reference 6, N = root. 9.5: nF = 256, s0 = 279 − 256 −
4 = 19. First K = `0xFFFD0`, binary: a 1-unit block is allocated at
131,128 + 1536 = 132,664 (`0x20638`); `c`'s frequency 2 becomes 4; T =
4 + E (2) + 1 = 7. cf = 2 × 5 × (7 + 6) = 130, sf = 19 + 7 = 26: 4 sf =
104 ≤ 130 < 156: new frequency 3, T = 10. The context is now {`c`:4,
`d`:3}, `d` with successor text reference 6. Second K = `0xFFFDC`: the
same computation, its block at `0x20644`; LO = `0x20650`. Current
context: root. L = `d`.

**Symbol 7.** Root, T = 279, v = 142 (unit `004c172c`): 95 + `a` 9 +
`b` 7 + (95) 1 + `c` 5 + (96) 1 + `d` 5 = 123, then the symbols 101 to
119, one each: 142; `x` (120) covers (142, 143). One byte read: low =
`b2fc6800`, range = `4c172c00`, code = `d0815a1f`. `x`: 5, T = 283,
exchanged with (119). Text offset 6 = `x`, TP = 7, successor text
reference 7.

**Symbol 8.** Root, T = 283, v = 109 (unit `0044d4bc`): `b` covers
(104, 111). low = `cef2d460`, range = `01e1d124`, nothing read. `b`:
11, T = 287; 11 > 9, so `b` and `a` exchange places (list ..., `b`,
`a`, (95), `c`, (96), `d`, ...). Update: q = 11, D = 2. Text offset 7 =
`b`, TP = 8. Case B: `b`'s successor is already a context (`0xFFFDC`):
nothing is built; D = 1. Current context: `0xFFFDC` = {`c`:4, `d`:3},
T = 10.

**Symbol 9.** Full step, scale 10: unit = `00302e83`, v = 8. `c` covers
(0, 4), `d` (4, 7); 8 is beyond: escape, narrow to (7, 10); one byte:
low = `4419f500`, range = `908b8900`, code = `815a1faa`. S = 0; masked
{`c`, `d`}. D = 2, F = root. Masked step, n = 256: Q = 286 − 10 = 276,
scale 277, unit `00859636`, v = 117: 95 + `b` 11 + `a` 9 + (95) 1 +
(96) 1 = 117; `e` covers (117, 118). One byte: low = `279bae00`, range
= `85963600`, code = `5a1faad2`. `e`: 5, T = 291.

Update: text offset 8 = `e`, TP = 9, W = text reference 9; case C. 9.5:
nF = 256, s0 = 291 − 256 − 4 = 31. K = `0xFFFDC`, n = 2 (even): its
block grows from 1 unit to 2: classes differ, a 2-unit block is taken
at LO (`0x20650`, LO = `0x20668`) and the old block `0x20644` goes on
the 1-unit free list. 4 × 2 ≤ 256 and T = 10 ≤ 16: T = 12; 2 × 2 <
256: T = 13. cf = 2 × 5 × 19 = 190, sf = 31 + 13 = 44: 176 ≤ 190 <
264: frequency 3, T = 16. Context: {`c`:4, `d`:3, `e`:3}.

**Symbol 10.** Root, T = 291, v = 110 (unit `00758506`): `a` covers
(106, 115). low = `5844c27c`, range = `0421ad36`, nothing read. `a`:
13, exchanged with `b` (11). Text offset 9 = `a`, TP = 10; successor is
a context: D = 1. Current context: `0xFFFE8` = {`b`:3}.

**Symbol 11.** Binary step: row 2, column 0 + 6 + 8 + 16 + 32 = 62:
entry 9844. unit = `00001086`, v = 7357 < 9844: yes. range =
`027b60b8`. S = 1, R = −2, frequency 4, entry 9844 + 128 − 77 = 9895.
Update: q = 4; the root's `b` (11) is preceded by `a` (13): no
exchange; it becomes 13, T = 297. Text offset 10 = `b`, TP = 11.
Case B, successor is the context `0xFFFD0`: D = 0, W irrelevant (start
= found). Current context: `0xFFFD0` = {`c`:4, `d`:3}, T = 10.

**Symbol 12.** Full step, scale 10: unit = `003f89ac`, v = 7: escape,
(7, 10); one byte: low = `01863000`, range = `be9d0400`, code =
`1faad255`. Masked {`c`, `d`}; D = 1; F = `0xFFFDC` = {`c`:4, `d`:3,
`e`:3}, T = 16: 3 states > 2 masked. Masked step with an estimator: u =
1: row 0. Column: u = 1 < 256 − 3: 1; T = 16 < 33: 2; m = 2 > 1: 4; L =
`b`: 8; total 15. The estimator holds sum 80, shift 3, count 4: e = 10,
sum becomes 70. Q = 3, scale 13: unit = `0ea99dd8`, v = 2 < 3: `e`,
narrow to (0, 3): range = `2bfcd988`, nothing read. Estimator: count 3.
`e`: frequency 7, T = 20. R = −3.

Update: found context `0xFFFDC`, start context `0xFFFD0`. q = 7 < 31;
the suffix is the root: its `e` (5) is preceded by `d` (5): equal, so
they exchange places; `e` becomes 7, T = 299. V = the root's `e`. D =
1. Text offset 11 = `e`, TP = 12, W = text reference 12. Case B: X's
successor is text reference 9; build including X: [X]; V's successor is
also text reference 9: [X, V]; root reached. New symbol: text byte at
offset 9, `a`, successor text reference 10; frequency from the root:
h = 13, a = 12, b = 299 − 256 − 12 = 31; 24 ≤ 31, 60 > 31: g = 2.
Contexts: `0xFFFC4` = {`a`:2}, suffix root (the string "e"); `0xFFFB8`
= {`a`:2}, suffix `0xFFFC4` ("be"). D = 0: W = reference to `0xFFFB8`,
and since start ≠ found, **TP steps back to 11**. 9.5: nF = 3, s0 =
20 − 3 − 6 = 11. K = `0xFFFD0`, n = 2: grows to a 2-unit block at
`0x20668` (LO = `0x20680`), old block `0x20638` pushed on the 1-unit
list (which now holds `0x20638` on top of `0x20644`); neither total
adjustment applies (8 > 3; 4 > 3). cf = 2 × 7 × 16 = 224, sf = 11 + 10
= 21: 189 ≤ 224 < 252: frequency 5, T = 15: {`c`:4, `d`:3, `e`:5}, `e`
with successor `0xFFFB8`. Current context: `0xFFFB8`.

**Symbol 13.** Binary step in {`a`:2} at `0xFFFB8`: row 1, column 0 +
0 + 8 + 16 + 32 = 56 (column 0 of the table): 11,191. unit =
`0000aff3`, v = 11,227: no. Narrow to (11,191, 16,384): low =
`1f91c7b5`, range = `0df1284b`, nothing read. Entry becomes 11,191 − 87
= 11,104; E = the seed with index 10, which is 3; masked {`a`}. D = 1:
`0xFFFC4` has one
state: skip; D = 2: root. Q = 298 − 13 = 285, scale 286, unit
`000c7ac5`, v = 2: symbol 2 covers (2, 3). One byte: low = `aabd3f00`,
range = `0c7ac500`, code = `aad2556c`. Symbol 2: frequency 5, T = 303.

Update: text offset 11 = 2 (overwriting the `e`), TP = 12, case C. 9.5:
nF = 256, s0 = 303 − 256 − 4 = 43. K = `0xFFFB8`, binary: a 1-unit
block from the free list: `0x20638`; frequency 2 becomes 4; T = 4 + 3 +
1 = 8; cf = 2 × 5 × 14 = 140, sf = 51: 51 < 140 < 204: frequency 2, T =
11. K = `0xFFFC4`: block `0x20644`, likewise. The decoded symbol equals
the escape value 2: a control sequence begins.

**Symbol 14.** Root, T = 303: unit `000a8b36`, v = 2: symbol 2 covers
(2, 7). low = `aad2556c`, range = `0034b80e`, nothing read. Frequency
9, exchanged with (1). Update: text offset 12 = 2, TP = 13; case B with
successor text reference 12: the list is [X]; the new symbol is the
text byte at offset 12, which is **the byte just written**, 2, with
successor text reference 13; frequency: h = 9, a = 8, b = 307 − 256 − 8
= 43; 16 ≤ 43 but 40 is not greater than 43: g = 1. Context `0xFFFAC` =
{(2):1}. D = 1.

RAR layer: symbols 1 to 12 are literals: `abcabdxbeabe`. Symbol 13 is
the escape value; symbol 14, the sub-code, is 2: end of member, with
missing = 0: a clean end. All 16 packed bytes have been read. CRC-32 of
the output: `0x7718BD00`.

### 12.2 Control sequences

The 20 packed bytes
`e2 00 41 42 02 a7 34 b5 36 9a 64 69 8f 0a 42 83 ce 06 ed 00`
have the header `e2 00 41`: new model, order code 2 (M = 3), 1 MiB,
escape value `0x41` (`A`). The symbols decoded are, in hexadecimal,

`42 41 01 4e 41 01 4e 41 01 41 06 41 ff 41 07 21 41 02`

that is: `B`; escape, 1 = literal `A`; `N`; `A`; `N`; `A` (each `A` as
escape, 1); escape, 6 = `A`; escape, `0xff` = `A`; escape, 7 = `A`;
`!`; escape, 2 = end. Output `BANANAAAA!` (U = 10).

The 37 packed bytes of vector V4 (section 13) decode to the symbols
`0123456789`; `02 04 00 00 00 00` (match, distance 0 + 2 = 2, length
0 + 32); `02 04 00 00 08 ff` (distance 8 + 2 = 10, length 255 + 32 =
287); `02 05 00` (run of 0 + 4); `x`; `02 05 ff` (run of 259);
`02 04 00 02 4f 01` (distance `0x00024f` + 2 = 593, the very first
output byte; length 33); `02 04 00 00 01 44` (distance 3, length 100);
`02 02`.

### 12.3 Block switches and a solid group

V5a (29 bytes): `a3 00`, 19 bytes of range-coder data, `80`, 7 bytes of
range-coder data. The first block (new model, M = 4) decodes the 24
symbols of `hello hello hello world` and a line feed, then `02 00`. At
that point the range decoder has read exactly 2 + 19 bytes, so the next
header is the byte at offset 21, `80`: a PPMd block that keeps the
model. Its range decoder starts with code = `66e2d90f`, and the same
24 symbols and `02 02` follow, now coded in 7 bytes because the model
has seen them.

V6: member 1 is the 20 bytes of V2. Member 2 (solid) is 18 bytes,
`80 9f fb cb 54 df 15 15 a4 b7 30 31 cf 01 32 00 00 00`: header `80`
keeps the model of member 1; symbols `second `, then
`02 04 00 00 16 08`: a match of distance 24 and length 40 that begins
in member 1's output, then `02 02`.

## 13. Test vectors

How they were made: the symbol sequences were coded by a throw-away
encoder built from sections 4 to 9 of this document (the model run in
reverse, with a range encoder matching section 4), outside the
repository. How they were checked: each was wrapped in a minimal RAR4
archive (unpack version 29, method `0x33`) and extracted with `unar`
1.10.7 and with `bsdtar` 3.5.3; **both produced the expected output for
every vector of 13.1 to 13.3**, except that `bsdtar` does not support
solid archives (V6) or filters (V7, V7b). Vectors of 13.4 list what the
two tools do.

Every vector was then checked a second time by a reviewer who wrote a
second throw-away decoder and encoder from the text of this document
alone, before reading the reference implementation: that decoder
reproduces every result below from the bytes as printed here, that
encoder reproduces V1, V2 and V9 byte for byte, and the wrapped vectors
were extracted again with both tools. V7b and V12 to V14 were made with
that second encoder.

Unless stated, a vector is one member, the first of its group, and ends
cleanly (section 11.2).

### 13.1 Small valid vectors

| # | Packed bytes | U | Output | CRC-32 |
|--:|---|--:|---|---|
| V1 | `a1 00 61 03 6d e8 96 2e d0 81 5a 1f aa d2 55 6c` | 12 | `abcabdxbeabe` | `0x7718BD00` |
| V2 | `a3 00 67 fb 83 54 d7 35 97 52 71 14 02 98 5d 33 1b eb 77 eb` | 24 | `hello hello hello world` + `0a` | `0xE05BD988` |
| V3 | `e2 00 41 42 02 a7 34 b5 36 9a 64 69 8f 0a 42 83 ce 06 ed 00` | 10 | `BANANAAAA!` | `0xC97DAAED` |
| V4 | `a1 00 30 04 2e fc d5 ce 46 7b 88 ef 24 95 75 d5 3f a7 a6 1d 7c 5f d3 07 bc 76 02 fd 3a db 9a fa c6 7f 1f c0 00` | 726 | see below | `0x29F13248` |
| V5a | `a3 00 67 fb 83 54 d7 35 97 52 71 14 02 98 5d 33 1b cd 52 00 00 80 66 e2 d9 0f 82 00 00` | 48 | the output of V2, twice | `0x591103FE` |
| V5b | `a3 00 67 fb 83 54 d7 35 97 52 71 14 02 98 5d 33 1b cd 52 00 00 b0 01 67 fb 83 54 d7 35 97 52 cc 75 54 dc 95 37 52 2b 5e 73 00` | 48 | the output of V2, twice | `0x591103FE` |
| V5c | `a3 00 67 fb 83 54 d7 35 97 52 71 14 02 98 5d 33 1b cd 52 00 00 00 bf 88 67 f6 a9 ff d4` | 24 | the output of V2 | `0xE05BD988` |
| V5d | `a3 00 67 fb 83 54 d7 35 97 52 71 14 02 98 5d 33 1b cd 52 00 00 df 68 6a 39 a6 cd cc f3 1b 5f 2f 00` | 48 | the output of V2, twice | `0x591103FE` |
| V5e | `a3 00 67 fb 83 54 d7 35 97 52 71 14 02 98 5d 33 1b cd 52 00 00 00 0b f4 84 5a 1f 80 a7 f1 09 ff cc 70 80 66 e2 d9 0f 82 00 00` | 51 | the output of V2, `LZ!`, the output of V2 | `0xDE6C42DB` |
| V5f | `a3 00 67 fb 83 54 d7 35 97 52 71 14 02 98 5d 33 1b cd 52 00 00 80 04 d2 40 97 00` | 24 | the output of V2 | `0xE05BD988` |
| V6 | member 1: the bytes of V2. Member 2, continuing the solid group: `80 9f fb cb 54 df 15 15 a4 b7 30 31 cf 01 32 00 00 00` | 24, 47 | member 2: `second ` followed by 40 bytes copied from 24 back: `second ello hello world` + `0a` + `second ello hello world` | member 2: `0x423FB5D5` |
| V9 | `a1 00 79 fd 5e c3 c4 aa 36 70 ac 5a 00` | 341 | 300 × `z`, `q`, 40 × `z` | `0x29E89153` |

Notes:

- **V1** is traced in section 12.1; **V3** and **V4** in 12.2; **V5a**
  and **V6** in 12.3.
- **V4** output: `0123456789`; `89` sixteen times (the match of
  distance 2); 287 bytes copied from 10 back, which continue the same
  alternation (`89` 143 times, then `8`); four more `8`; `x`; 259 more
  `x`; the first 33 output bytes again (`0123456789`, `89` eleven
  times, `8`); then 100 bytes of period 3 (`898` 33 times, then `8`).
- **V5b**: the second header `b0 01` asks for a new model (order code
  16: M = 19; 2 MiB); the second half is coded from scratch, and its
  range-coder data begin with the same eight bytes as V2's.
- **V5c**: after sub-code 0 comes an LZ block (first byte `00`); these
  eight bytes are the LZ block `rar` itself writes to end a member with
  no further output. It needs `rar29-lz.md`.
- **V5d**: the second header is `df 68`: keep the model, new escape
  value `0x68` (`h`), and the ignored order-code bits all set. The
  second copy of the text is therefore coded with every `h` as escape,
  1, and ends with `68 02`.
- **V5e**: PPMd block, sub-code 0, an LZ block
  (`00 0b f4 84 5a 1f 80 a7 f1 09 ff cc 70`, 13 bytes: the literals
  `LZ!` and the LZ "new table" item, `rar29-lz.md`), then a PPMd block
  with header `80` that continues the model: its 8 bytes are identical
  to the last 8 bytes of V5a.
- **V5f**: the first block is that of V5a. When its sub-code 0 arrives
  the member's 24 bytes are already complete; the second block (header
  `80`, model kept) holds nothing but the end marker `02 02`. A decoder
  that stops by itself at U never sees the second block and is still
  correct here; a decoder that treats "sub-code 0 with nothing missing"
  as an error is wrong (section 11.2).
- **V9**: M = 2. The run drives the frequency of the binary context of
  maximum order to its ceiling of 128 and R far above zero (295). The
  `q` escapes from it down to the root; the context becomes {`z`:120,
  `q`:5}. The following `z`s rescale it three times with a = 0 (after
  the 4th, 19th and 34th of them): `q` goes 5, 2, 1, 0; the third
  rescale removes it and the context turns binary again with `z` at
  frequency 32 (section 8.5, the one-state branch).

### 13.2 Filter records (V7, V7b)

**V7**: packed bytes (305), U = 68, M = 4:

```
a3 00 02 05 8c ef e6 c0 3c 99 63 3b 6f 73 83 ab ad f8 98 25 b3 3f 83 96 d3 6a 38 87 40 f7 fb ae
ed 9b c7 70 69 58 2f b8 23 19 83 2a 4c 05 75 b0 17 e5 6b c8 6c c3 84 63 ce 41 d0 66 34 dc e1 ec
85 4d 2b b7 b1 8b 67 61 c6 ce be 31 34 d0 5c 8f 92 4a c4 03 db 23 c6 c0 f5 d4 99 e8 fe 20 cf c8
4b 15 e3 78 cc ae f2 18 2e b7 a0 5f 2e 70 1e fb c7 04 ad 43 c2 0f 41 18 2d 97 8d e0 97 ac 7c 8a
19 45 ca 14 b7 29 81 24 d7 c1 d8 cc 03 97 a8 44 c2 be 7a 46 38 aa 75 08 8d 09 7a ae 06 f6 8c 5f
c2 5e 77 f3 30 91 1a 53 88 df 07 4c 65 c5 7b b7 92 2c 26 aa 1e b5 13 02 f4 1b 0d 01 8c f9 4c ef
5f fc 29 d0 26 d8 b6 40 0b 33 17 75 b5 59 ab 61 30 8d ff 33 31 7a 65 3b f5 ec f3 b6 f6 97 e8 44
a8 22 f3 12 45 f2 c3 df 44 1f 54 8d a1 41 87 41 61 37 4a 2a 96 0d b8 1b 73 85 93 32 ff 2d 68 63
9f e8 61 af 35 a2 e7 df 53 6f 33 c9 19 b7 5c 92 f2 0e 4a 58 f0 8f 32 2e df 7e e5 4b 4a 46 be 76
a1 6f bb 65 33 34 be 89 67 56 a4 3e 03 33 8a a3 00
```

Symbols: `02 03` (escape, filter), flags byte `b6`, one length symbol
`d7` (k = 215 + 7 = 222), 222 payload symbols, then 64 literals with
the byte values (7 i + floor(i / 8)) modulo 256 for i = 0..63, the
literals `tail`, and `02 02`. Expected at the interface to
`rar29-filters.md`: flags `0xb6`, a payload of 222 bytes whose CRC-32
is `0x00842232`, delivered at output position 0. The 68 bytes before
filtering have CRC-32 `0xA484D8C7`; the payload defines a two-channel
delta filter over the first 64 bytes, and the member's final output (as
`unar` produces it) has CRC-32 `0x43195E29`.

**V7b** (316 packed bytes, U = 132, M = 4) carries two records, in the
two length forms V7 does not use:

```
a3 00 02 05 8d 16 69 76 d7 67 48 ea 32 36 9e 61 47 1d 09 27 70 b5 d3 9d d5 dd 3c 62 4b 5b 50 9d
0a 4f ac 04 96 e3 3f ad da 54 34 ed 2b af e7 ca 88 fb 3d 28 df 4f 70 a2 17 7b 3f 7d ad e6 62 75
28 9b db 64 b6 04 4d b8 46 38 d6 25 4f 34 6b 0b da a7 00 08 95 9d 3e 50 57 aa db e6 b4 92 61 1f
e3 fd 7e 3e 42 74 c5 d9 9e ff c1 d5 93 f4 fc 4f 5f 29 ba e0 7f cc 59 91 aa 65 a9 7c 98 f6 e7 63
d3 ed ab 9d 56 d3 11 a2 6c 52 46 42 01 f3 cf 93 56 e4 78 24 1b cb c7 95 82 d2 e9 59 40 4e 05 2d
42 c8 37 71 70 23 6d b9 5e 18 70 0e 56 b3 ca d0 73 06 5c e5 fe 4e 32 f6 0a bc d9 13 06 5c a4 ea
e4 35 99 d2 11 ca 05 76 92 83 73 35 db ce 01 0e df 6c 29 ef 88 5a 3e 26 70 0e a8 b5 e5 40 8e 21
a8 86 ef 32 ee f8 a5 9c 35 80 b9 ca 8e 72 69 2a ee 17 3f 2e 17 14 16 bb 65 ee 56 91 cd 8b d1 79
16 10 0e c3 15 38 76 f6 0a 98 31 fb dc 90 c3 f2 1a 13 3d 81 b9 56 43 e4 9d 89 76 92 04 4c 42 c8
75 8e 3b ca 39 fe a2 2f d8 b6 85 4c b5 31 13 cc 18 73 69 81 d4 e4 77 2a 1a 9e 91 95
```

Symbols: `02 03`, flags byte `b7` (c = 8), two length symbols `00 de`
(k = 222), the same 222 payload symbols as in V7; the same 64 literals;
`02 03`, flags byte `12` (c = 3, k = 3), payload `00 08 40`; the same
64 literals again; `tail`; `02 02`. Expected at the interface: first
record flags `0xb7`, the 222-byte payload (CRC-32 `0x00842232`), at
output position 0; second record flags `0x12`, payload `00 08 40`, at
output position 64. The 132 bytes before filtering have CRC-32
`0x309F6C75`. To the filter layer the second record means "the previous
program again, starting here, same length, first register 2", so the
member's final output as `unar` produces it is the first 64 filtered
bytes of V7 twice, then `tail`: CRC-32 `0xB1CF103F`.

### 13.3 Larger vectors: statistics, restart, model memory

**V8** (1599 packed bytes, U = 6000, M = 2, CRC-32 `0x079754E5`). The
output comes from a number x that starts at 1. For each of the 6000
bytes, x is first replaced by (x × 1103515245 + 12345) modulo 2^31;
with t = (x >> 16) AND 255, the byte is 97 + (the number of consecutive
1 bits at the low end of t). It exercises 149 rescales, of which 56
remove states and 33 shrink a block, 220 text step-backs, and estimator
adaptation up to shift 5. The first 16 output bytes are
`61 61 62 63 63 63 61 63 61 61 62 66 61 61 62 64`.

```
a1 00 61 01 7e 89 d7 e6 7b 2b d0 37 87 b3 fa d0 ca 0e 4e 68 61 04 8d 56 20 25 ae 37 c0 0e 7d c1
e1 1a cc cd f1 b4 9c cd eb 52 c0 da 8c b6 79 bb d3 fe c8 ba 38 3b 26 c7 61 25 e4 93 a5 c8 9a 60
61 76 0f 6c 20 6a 20 f2 ca 76 e8 70 bc 41 8d 9f 3a c9 4d a7 18 61 ad d1 f8 32 65 f5 eb 1a 60 0a
8b d9 64 57 85 17 f7 57 b5 7b 1d d8 54 42 4b 41 c8 1e 59 87 65 fa 81 3d f2 c5 50 92 fe 53 68 27
5b ab 55 a7 66 d6 5d 4a f9 09 1a fa fa c3 28 81 60 f5 5d 74 a4 17 50 82 af 4d 77 d6 05 92 21 42
17 e7 c2 33 02 c7 e6 17 29 52 bd 67 d9 c0 7a 8a 6b 3e 22 d6 31 ec 37 a8 0d 8c d5 a4 d5 b9 3b 46
86 f8 04 40 b3 57 17 5f 52 a1 d6 3e 7f 4d 00 a0 a3 e5 bf 42 86 c1 ee af 17 60 02 d3 4d a5 cd 7a
3d 8c 0e 89 ca eb 10 be b2 0f b1 70 be 05 c7 3c d9 aa 9f f0 1b 47 b5 0c 84 0c df 9d e5 62 30 ca
a9 d1 ed 72 5c 11 39 5f 66 37 db 0d ae 8d 89 a4 86 4f 1a 33 29 da 70 e8 cd 33 26 85 71 11 e6 06
6b 3a 33 83 00 c6 5a 64 02 47 e8 a8 cb 3d 54 b5 78 d5 5c db 6a 05 e6 f1 7e 98 b6 57 78 a2 97 df
e3 b6 a6 3c cb 11 e8 2e d9 f5 e5 ca 77 67 5a a4 80 ee d2 68 d8 c7 51 15 55 50 2d 5a a2 78 d2 3f
f6 b4 29 f9 b7 fa ab 95 8a 02 27 80 35 45 4d 76 44 0b 94 ef 60 cc 17 ae da df 82 17 a9 4a 79 98
3c ce d5 90 bc 64 19 7e e8 00 c5 f9 e2 e8 63 fe 03 fc ba f1 5e fb 5e d6 2d 80 28 13 af 5f 1e 17
ce 93 d7 0a d3 af 3e b1 07 2d 2e ad 9b 97 f1 6c 60 5c a8 09 47 56 99 dc 56 d7 17 a0 e4 b4 30 c5
c1 99 ee 5e ac 25 5e 15 96 66 8b 80 e5 dc 82 56 64 f3 bb b7 5c 3a 61 a1 cc 9e e0 58 a0 22 67 04
26 34 7a a0 2f b4 4a 49 f9 8a df f3 a7 78 13 34 06 7c 3d 6d be c8 9f 68 c6 20 ab d9 e3 34 c8 0e
86 32 ec 2f ab 11 36 93 45 1b 3d de 50 4d 42 78 bd e5 cc 86 6f 85 f6 f1 29 3a 3a b0 1b 79 5d bf
52 29 2a d0 a6 d6 95 90 74 bf 19 4b d0 e6 34 f6 d0 b6 6c dc 9f e1 8b 0b 44 9f 6e de 46 fb 9f e8
3d eb de 59 73 71 1f 0d 26 ab 88 f2 91 41 d3 da 1b 84 bd 7a 0a e0 e6 00 ec 95 04 5d bd 46 f3 5a
1c cc 8c b1 dd 29 db 77 aa 22 0e a3 82 56 b0 fd 1e 76 05 89 94 c7 ee 27 e8 fb d4 6d 7a fe e5 1d
1e 4d e5 f9 e2 a8 65 04 fe 10 4c 67 0c b8 1c 82 65 d8 6a 3b 7a 2c 31 e5 6f cc f1 6a 2d 47 a3 99
99 3c 59 02 10 93 84 8d ac d6 10 f2 0c 55 bf bd 68 80 ea 65 ee 1a 53 56 f3 75 83 2d b5 b0 ad f8
31 70 48 44 e6 9d c4 5e ac 3d 90 a6 9c c6 ff af 23 93 c2 65 f5 44 db 2f 87 07 e9 72 43 3b 71 f1
8c 6e 5d 80 3d ef 4c e6 9c d4 05 78 43 ac 09 2d 08 bd d9 c0 72 a5 23 9a e3 3d e1 3a 11 17 6e 15
87 88 0a 2e 62 21 48 45 1c 53 9d 0f 36 3b 89 bd 8c 02 7e 39 9e e5 2b 6d 8b ac 5e dc a8 8b c1 36
f2 48 19 12 13 30 f3 8d f0 d0 2b 44 ed 07 e5 0c 8a c8 9b 89 48 9b 6f 1d ac a4 f1 6b 60 16 b0 ad
c2 b9 d1 e8 c0 9d 26 4e 5e 5a e9 b4 f5 9a f2 f8 f0 77 ef 9e 54 a6 3e b1 d6 28 3e da e7 91 7e 18
19 1a 93 e9 31 de 11 06 9d ea 5b 79 18 44 b3 66 e6 57 be 3a 7e 17 95 dd e5 64 4d 7e 85 14 d6 ce
09 c7 be c8 ee 6e aa 80 0f 5e 90 03 6a c7 e1 45 d0 8f 55 86 bf 80 78 cd a7 1f aa 3c 52 08 fe d9
c2 a9 c3 ed d0 0b d3 a7 52 ad ea 49 e1 af 04 71 0b 21 61 4c 36 6b 3f 88 e8 6f db 2c 8e 85 07 7c
b5 79 7b a3 b2 65 d9 c8 85 bd a1 8b 74 5a 9b 27 bf 18 07 26 cb 08 04 86 ec 7d c8 72 15 ce 2c b8
43 22 a3 2f 76 d3 49 ec a6 04 70 15 aa f5 21 14 a5 e6 27 2d 43 e5 c1 57 a0 d7 04 40 d0 27 5d b6
46 97 1e b2 e5 5d 3b 35 07 56 e8 d9 b7 3e 28 a6 9a 78 a1 94 ab 84 c6 25 f2 a3 1d d5 66 3e f1 3e
74 3e eb d6 f7 84 48 04 58 ca 3b 4c 1c 71 9e 13 4f e8 5e 81 1e 36 7d bf 26 a1 86 6d d9 88 0d 57
54 f9 6f cc 47 51 6c a6 3b 46 14 71 2f 9e ac 0f 7f 21 38 0a fc 03 3f 78 bb 12 f6 83 8d 41 f4 ef
a8 b5 f3 28 69 61 79 5e 86 4b 76 6a 79 5d 77 ba 57 21 3c 4a 1a 8b ec 83 05 65 f6 ea 2c 93 8f 3c
26 25 e1 2f 87 56 aa 4a f2 68 f3 8b 51 78 46 d7 ad 74 31 22 d1 ca 00 37 48 e9 a3 0e 34 84 45 a2
25 3d 51 04 7d ec ea 3f 59 cb e5 e7 b0 50 d1 7c a1 bb 9e ab 10 4e 8d 1f f9 f4 6f 1d 2b 52 31 fb
dc 5a 77 35 2b ef 33 78 1c 29 94 7d 83 ed 2d 6b fa ed cd da 77 01 47 d0 7f eb 37 5f fe eb 56 5a
06 69 75 0d 3f 4e 3a f9 5f 86 9d 74 9c 98 75 f8 c5 26 b6 32 08 57 04 b8 2d 19 d5 4e 88 13 19 a4
3c 4b 4f b6 d7 70 3a 56 ca c5 56 5b 99 db d4 70 85 c3 8a 48 7b 3a 08 89 f3 89 80 56 b4 63 0f 9e
04 c1 a4 ea d9 4c 89 28 69 23 b8 96 fb 06 51 76 47 fb 88 f3 9d 26 30 31 9e 7a 28 f2 1b 2d d6 ce
76 cf 98 59 24 fd ea 87 0b b0 48 11 ad 9c 55 55 2c 60 ea 61 4f ac 03 90 76 9c 67 cb 45 78 35 14
07 7b 3a 68 3f 0e 5b 35 99 6b 50 81 26 d4 02 b2 35 81 81 36 72 1d f5 2e 0c 84 cc fe 2e 09 3a 33
ff 26 ef ef 7d fa c5 d7 df fd 68 43 9f 73 b6 ca db bc cf fc ff 8b 63 99 ab 8c 0a e9 23 c0 7b 3e
d1 1c 1e 6c d1 f0 0d 77 55 8f c9 88 5d 82 8e 50 e9 f8 44 75 69 02 62 a9 5e 72 9d 85 07 c3 8f 5b
c1 ee 2c 8a fe 22 a7 f6 b3 71 25 71 d6 8c ab bb 89 b8 e9 a8 a3 8c 9d 7d 2d f8 70 3c f8 46 e5 3c
49 e3 56 77 10 15 44 52 c8 18 27 85 86 74 8c 7c c5 ab 1b fc 57 02 ea 4c 0e ff a1 49 69 15 7b 40
4d fd 67 9a e3 ae 04 ab 19 61 6c db bf bb f0 7f 9c 2c 8f 0e 5b 85 c6 3a c6 fa 44 77 cd fe 77 3d
a8 89 28 cb 61 f8 bc 54 dc b8 0b 1e 6b 75 ff 34 59 a5 c0 32 01 14 0d e4 1c 8a 7d 6c c8 46 00
```

**V10** (565 packed bytes, U = 21,303, M = 64 with header `bf 00`,
CRC-32 `0xA250B646`). The output is the numbers i = 0, 1, 2, ... each
written as 64 base-8 digits, **least significant digit first**, each
digit as the byte 48 + digit; cut off after 21,303 bytes. The model
memory (1 MiB) is exhausted while the 21,004th symbol is being
processed, and the model restarts there. On the way the merge pass
runs 26 times and 6,469 blocks are taken from text space. **A decoder
whose merge pass does not join blocks restarts 649 symbols too early
and decodes wrong bytes from output offset 20,355 on.**

```
bf 00 2f ff 06 0b af c7 5d 61 f5 89 5c f5 1a b0 e3 d9 a7 0a 32 b3 0b 05 b2 e8 0e 8e 28 51 c6 9c
2b 46 06 86 7c 00 00 0a c9 a7 51 3c 0e 94 a5 5b d3 f8 f3 2d f5 b1 be de 7e ff f8 c9 f5 13 1a fc
8c bb e3 89 b4 50 54 76 f5 f5 56 06 04 2a 41 d9 f9 e0 11 60 f1 5a a2 34 70 6f a7 95 35 f5 da 1c
00 d7 72 65 73 d0 2d ec 66 79 fa 72 5f bf 3a fc b5 12 6b fa 44 bb 8f 2b 36 f7 26 37 98 c6 c3 f1
f1 3e bd c5 b5 cb bf 0c 85 6d 66 4f c6 6e 35 15 ee 56 01 37 db 29 06 a5 d1 bf 23 d2 7e e1 ab 9e
c9 34 e7 f6 de 5a 0e 8a 33 23 76 49 15 db 6d 63 33 9d 6d b5 84 86 35 4c 54 e3 cf d7 b4 21 b0 54
df 95 54 a6 98 8b ff 4a 70 cf f8 3d 51 18 df 36 df 41 dc c2 d1 1b 09 be 67 49 63 98 da 3f 1a 6f
05 06 74 1e 68 19 db f3 22 a5 f2 4f a5 ca a7 ca aa ac 77 a4 df 83 11 3d e3 80 9b a2 93 a9 f8 47
79 97 71 70 8b 4f 28 ac 85 40 17 14 75 6e 89 e5 16 ea 0a 95 e9 a3 03 da 30 0f 38 55 75 67 79 10
49 33 95 4f 94 1f 4a af 49 22 31 91 54 f9 f2 a7 82 b2 a5 de e1 f9 de df 33 e0 e0 17 d1 b8 ff 87
05 2c a0 1a 8f b0 47 30 66 90 a0 c0 2c c7 9e 7c 01 e7 60 e4 4a e0 dd 29 9a 05 58 ab 15 8a f9 14
2a 31 4c f6 5d 4a 71 38 8a 40 4e 7f a1 95 ef 3e 27 01 d4 a1 d9 0a af 23 e2 cb 30 eb 95 8c 6b 5a
b6 ec 42 e1 69 3e 45 d4 8b e7 bb 9c 68 91 54 40 a8 55 d8 f8 8c 92 d6 29 74 e3 04 79 97 4a ca fa
1e e8 47 7d 7f 9c cb 2b 28 5b 42 7d dd 15 5a 7d a9 c3 8d db 4f f8 b0 01 9e ab 41 18 13 bd 04 d2
98 cb 14 5a 67 e9 4c 37 5f 8c 4d 65 a8 08 c0 be a1 4f 56 79 88 ab 5b 3d 05 0b 56 dd bd 26 da ee
0f 88 83 f4 bc c4 ef a0 7d 6d 53 36 a9 5c 94 fd ca cd 58 1d 64 3c d8 93 0c 67 df fc 61 4a 51 bd
9c 4e f8 84 8b f0 6a 9e 22 9b 34 15 03 7c 2b d0 df 26 89 d1 42 1f 3b 82 c5 54 cd 64 ab dd 7f ca
7e db 26 4e e1 b1 3e 30 e3 9d 7b 01 50 f2 5b 1d bc 54 e7 9e 34
```

**V11** (1776 packed bytes, U = 40,308, M = 64, CRC-32 `0xA6D43CBA`).
As V10 with 40 base-32 digits per number (bytes 48..79). The model
restarts while the 40,009th symbol is processed. **A decoder without
joining decodes wrong bytes from offset 26,682; a decoder whose free
lists are first-in, first-out instead of last-in, first-out decodes
wrong bytes from offset 40,040.**

```
bf 00 2f ff 3b 39 db 21 aa 25 85 42 83 f1 49 1c 1b 46 ac de 4e 91 4c 99 4c 01 a9 1d 14 18 59 de
5c ea 8b 4f 07 89 4e bd 72 4b c0 11 73 95 09 f8 44 73 c4 6e f2 d9 ef 26 01 89 cc 57 b1 40 50 b2
69 89 9e 0b 63 b6 39 66 01 28 20 4d 72 91 16 5c 1c 37 34 01 4c 5e b9 e9 e6 07 cb 46 e1 86 00 1a
b7 7a 2f 4d 00 35 33 f9 7a d8 00 3e 83 e5 06 b9 00 2b 33 29 5d da 00 11 d2 02 2a 3e 00 04 5a e5
79 74 00 00 a3 e5 66 56 09 00 0d 3b 01 91 82 00 00 c1 05 35 10 cb 00 08 0a 25 20 00 00 00 25 23
05 4c 00 00 00 74 68 6b c8 00 00 00 00 3e 8d 9a e0 e9 08 05 ff e9 00 83 03 c8 ed 7d 1f 02 36 9b
d7 9d 65 f4 4b f7 b7 cc d1 5d a4 71 8c 20 c5 8a 06 c1 37 4b 73 28 d1 21 19 1b 0e 1e d1 1d 7f 8f
52 88 6a 46 ad 3a ec 1e 4b 27 f7 f7 d3 59 06 18 70 ef 11 50 6b 2f f3 56 b4 21 f4 26 ea 38 c7 fe
2b 0f b9 9e bd ad ee e4 4e fe c4 49 a5 96 8d 2d 21 6b 48 fc 5f 1c 7a 7f 0b 8e fb c7 c9 b3 6e 93
f6 fe e9 9d bf 2b df 78 79 b0 be 87 b4 4e 4b 29 e5 f2 3b 01 65 49 35 7c 46 bc 0e 09 97 13 23 56
07 43 41 01 ed 6e be 36 b4 68 3a 76 92 ee 76 ac fe 12 5b 96 c6 b5 36 80 8a d3 26 cb e1 d0 20 aa
d3 fe 9c f3 62 04 dd 85 d9 a5 ec 64 b4 3d 94 99 5b b2 b0 ca b6 4f 22 84 11 7f 90 fc c7 49 5a 38
43 49 39 68 3e f5 d0 9e f2 2d 25 76 96 52 54 bc 86 dc 72 d9 e1 fe ae 2c 52 ac 3f 0d 0b ba d8 f3
5b e4 a3 0d f8 2f 81 11 72 e1 79 8b b8 c8 f4 d5 15 6a 95 76 29 05 2d 83 b2 64 52 41 64 ad 5d bc
7b 41 34 0e 15 b3 6d f8 a1 6d 60 53 27 ed 74 58 4a 1f ac f6 53 89 ed 8a 5b 14 34 29 3c ed 2a b7
1b 40 67 3d 8e 6f 79 b7 10 80 50 6e e1 90 6e 30 72 27 06 ac 6c 91 cd b2 1f d1 83 f3 1f c4 1e 03
4c 3e d7 c8 f5 39 f3 6b b6 6e e3 74 df 35 ff 19 ed aa 92 62 fe 2a f4 de 7a ae 09 fc 88 ab 45 4d
a9 74 06 0e 8a 59 19 9b 6a f0 a7 5d d2 37 6f 10 d4 d3 de b7 fd 1a 47 78 76 a9 83 d5 4a 72 99 a9
d8 30 04 9c 87 04 0f 88 6b 50 37 a6 a2 a4 c7 6c c9 8f d2 52 4a d7 50 4f d4 6c 12 9e f9 64 62 67
81 5a a4 a8 28 d4 6f f0 46 4a 23 79 49 06 72 26 31 8b 94 37 ce 00 d2 64 3d fc 29 51 57 3e 51 e8
f4 2a 8c 6e 3b 53 7c 37 da f1 77 8b d1 d5 de d3 a0 1b a7 df 3d 2a b4 9c 43 1d c0 b5 6a 20 85 5c
21 1e 7d f4 5c 68 f8 f8 04 96 2d 05 ec 18 54 1d 8f 89 55 31 66 7d f9 15 d0 90 27 f4 b2 89 99 b2
d5 c5 e2 b7 8a a4 8f 13 53 44 b7 5b f3 45 b2 6b 4f 59 bf 4b e4 64 74 c8 16 f0 65 6a 42 e3 5d b9
89 fa 80 5b 9e 3a 5e 83 ba 72 50 84 fe f4 31 23 62 e1 ff 71 7d 90 da 20 8b f7 d9 e9 70 61 50 6c
a1 82 4a a0 f9 7b 08 b1 37 b5 29 be 91 f1 3d d2 55 90 0a b7 d5 27 c7 ca 62 d0 da 03 4f f5 19 44
f6 55 2e 20 20 58 7a 46 78 47 65 03 83 2a 8a 0b 48 20 05 c7 84 2c fb 6c dc f5 13 97 5e 53 7d 06
30 49 ae b7 3e 8f fe 50 1f b0 e4 49 58 44 0c a8 ce e2 e4 3a 61 f7 d8 f9 4b 26 37 ef a8 d9 dc 49
01 6d db bc a3 b8 6a 46 85 d7 20 44 b2 dd 0a c4 8d 8d 18 c6 34 28 63 cb 94 49 d8 3c c8 41 c7 f0
ab b6 bc 58 96 0a cd 33 37 fb 84 a9 6d 68 3a e2 a2 f1 79 d3 c2 b3 09 77 f8 32 64 56 2a 72 d1 78
3d 0c 31 c0 8f 03 52 76 2b 98 e9 2f 64 07 2b ff 60 a3 58 c8 1e 0c c5 87 ce 6f ab 82 17 7e a8 f3
4e 2b dd ab 26 b9 e1 8e 5b f5 20 e1 ec a0 a5 2c 28 43 db 58 19 b5 c6 b9 f3 47 a1 a3 c4 7e bc 1e
21 f6 bc 51 5a bb 52 27 4e 73 a1 56 49 67 53 22 6e 80 e0 2a c4 f1 95 5f 39 90 e8 e3 a4 5c fc cf
86 10 ae e0 25 9a df 0d 40 56 21 ad ea 51 ff cf 06 89 57 46 60 ac 68 98 87 35 d1 c1 b6 10 b4 dd
06 b2 8b 64 5f 44 8d 3b 33 82 ff f9 10 92 ae 24 62 49 c1 81 ea 08 be 42 8a b8 c2 a2 d7 95 4e a9
30 0f 91 0c 4f 5e 4e 22 2f 02 3f 95 b4 7a 38 72 b1 86 65 d3 ee 50 b8 1b c1 3e a1 7e bb 70 2a a1
84 5e 4a 1d 65 16 87 e9 35 a2 9e 7c 19 ff fd b4 35 22 df 9a aa 84 06 83 18 d4 9f d7 1b 86 ee fe
4b a1 21 31 7d 6d 84 e7 30 13 f1 77 dc 8b e6 e3 ad a3 5d 89 5b 4b fa 62 a6 b3 27 52 4c c7 47 4e
c8 10 ad 73 b0 ec 85 33 49 2e d8 65 6d 1d 13 79 d0 53 dd f9 37 c8 01 71 23 20 1e a0 b9 bc c4 be
f0 e3 90 ff 88 5d 64 63 70 6f fd 49 a2 88 7d a2 aa 6f d5 65 3e 61 28 17 59 60 5d 94 a5 b2 fe c6
be d6 57 0d f8 de c4 7a dc f8 9e c1 92 fc 24 06 a2 93 0f d2 58 1e f1 75 28 f6 96 29 1a 72 07 2f
de a6 f5 92 3b 2a 38 be f2 a0 dd dc 85 af 32 f6 5c 3a bf 8a f0 17 5f a4 83 d7 81 14 c9 16 71 ff
6f de 46 ae 43 ad 70 07 f5 16 01 49 65 cd 74 63 15 9c 83 4c 5e 68 53 a0 b5 aa 2f 04 57 23 08 a6
90 d4 3a 8c f7 d7 a2 58 4c 59 36 2a e4 af 9f cd a8 50 26 a6 0c ca 25 43 05 75 fb c4 43 be 00 63
b5 d0 6a c8 8f 36 52 5e d2 ec 63 43 6e 71 3a 55 3c 3b c0 94 9c d3 4b 9d 39 e6 ac 52 af ed 45 ef
ff 63 7a 35 c0 33 91 a2 e4 1e 96 16 6e 6d 27 5f d2 ba 0b 64 b0 11 de e0 0f 64 68 fd 0b 4c 86 43
6d 20 71 45 33 6d b8 26 65 cf 07 5a 7a 7e ef d9 fa 05 3a be 7d 3d 9d 54 73 0e 2f 86 5d 04 01 ff
5e c3 f3 a6 67 f8 37 87 c5 47 d5 f3 0d a1 b7 bc ae a8 d4 02 e5 74 d5 5c 6c 31 6c b2 d8 24 6d 48
b1 5a 11 0d 82 05 fe 1d f1 dc e7 6b c7 6b 02 d2 dd be e9 23 7c e5 7a 71 66 b3 df 97 0d 20 7d ac
1b 09 df 61 29 56 c0 e7 e2 b5 25 66 6b 06 8c dd 49 75 91 9b 44 1d c9 1f 92 b1 33 3a 60 7b cb 3a
aa 7f 6b e5 98 a0 c0 d0 ff 47 3b 4f 3e 0a 60 68 15 56 0f 8c bf af 1b c3 15 bb 61 c8 c8 d1 27 7f
5f c1 d2 77 27 4b 42 24 55 0b c2 71 f0 7a b4 d3 c7 7c e5 10 5f 27 b9 20 1e 23 8a ca fb 24 ed 64
aa 50 c4 0a e4 0f 21 6c 4c 2e 28 31 5c b3 36 2e d8 51 3f 17 ac cf 69 28 28 33 ff d6 cf 81 3a 76
51 ee 19 50 4c 0c 58 32 31 cf e2 25 b9 20 bb 92 d9 e4 45 27 18 32 e0 b6 6e dc a7 35 0f ef 71 4c
46 0d 23 d0 2f 1e 57 ba 2c be 94 0c 99 62 ad 55 b2 b2 c5 77 b4 d4 0b e8 16 11 67 e1 b8 e7 3f 70
66 6c 8e dc ac 29 f9 66 a3 b2 f2 78 c1 52 25 2a 83 5e 3f 5d 3f 14 0e 29 a0 f1 c6 b0 14 cb a4 80
82 b4 da 9e 8c ce f8 75 b0 5b 0b db e3 2e 68 00
```

**V12**, **V13**, **V14**: three more streams of the V10 kind (header
`bf 00`, M = 64, 1 MiB; numbers written least significant digit first,
each digit as the byte 48 + digit, cut off after U bytes). Each makes
the model restart for a different reason, so that together with V10
(an allocation failing in case A of 9.3) and V11 (the text pointer
reaching the units area) every way into section 9.7 is exercised. In
each, 300 output bytes follow the symbol that triggers the restart.

| # | Digits per number | Base | U | Packed bytes | CRC-32 | Restart while processing symbol | Cause of the restart |
|--:|--:|--:|--:|--:|---|--:|---|
| V12 | 40 | 4 | 12,148 | 483 | `0x568836D1` | 11,848 | a context allocation fails while a successor chain is built in case B of 9.3 |
| V13 | 64 | 16 | 29,231 | 836 | `0x5B289324` | 28,931 | the 1-unit block for a binary context fails in 9.5 |
| V14 | 64 | 32 | 49,966 | 1492 | `0x1D3B7CEF` | 49,666 | growing a block of states fails in 9.5 |

A decoder that restarts one symbol early or late, or that mishandles
the symbol being processed when the restart happens (it is still
delivered; L becomes 0), fails the CRC-32 of these vectors. V14 also
detects a merge pass that does not join blocks (wrong bytes from
offset 33,730).

V12:

```
bf 00 2f ff 3b 39 db 21 aa 25 85 42 83 f1 2c 74 3c 0d 78 b5 e5 eb ed cd de 62 a0 95 e1 32 11 98
83 6b 64 78 0f a9 ad ed 8c 7d e4 11 15 e1 6c 07 eb c2 dc 8f 9d d1 8d 58 af fd f4 77 4b f0 e6 6d
b9 e9 40 b0 df 1f 17 31 36 f2 44 a1 21 43 8b 1d 6e 8a 9d 71 ec f8 45 27 c2 e1 36 f2 c1 03 70 a6
a8 03 29 7e b7 66 88 1c 00 80 3a e6 2f 9f ac d2 e8 51 bb 0c 37 e2 18 de b1 73 44 07 f2 c2 c6 0e
7d 51 df f3 15 07 3f 58 62 46 d3 4d df f8 fa e2 9a 83 ce 76 53 4a 4a 86 0f 69 d4 7c 5e 29 9b 64
90 ec 35 70 a4 05 8d 84 f0 1c e4 80 44 ea e9 c0 3e 77 95 94 e7 a3 b9 c1 e9 75 c3 81 a1 6a fb 2b
21 df b6 b6 9d b7 90 cc 30 1c ca 4a b4 fa 41 aa ed 0b e5 ae 1b db 77 61 3c 3a f0 49 72 7a 60 42
67 04 9c fe 37 c6 f8 ca e1 c1 52 b7 f3 e1 cd 00 ec 0d 8e d0 2f 19 66 35 8c 70 fa 21 d9 15 d0 0b
9e ba 6f 10 8c 93 57 d5 9c 21 14 13 ee f1 77 71 bc a1 84 df 9c 8a c3 72 d5 ec 48 8d df 9b ad f4
6b 15 76 48 58 06 99 e1 40 01 e1 ff 89 36 df 5c d5 d0 16 5e 21 8d 0a c4 55 ef bb 2f a1 1d 7b 80
19 17 45 00 92 5e cf 31 e9 3f 1f 37 eb 5b b2 cc ee e3 04 54 44 53 b8 46 d8 5d dd 52 d7 40 f2 f8
60 43 bd 33 dd 13 a3 5b 66 ca 54 f3 9d 2e 4b a6 f3 09 cc c1 8b 69 ec 13 fd 80 2d 0e c4 9a 74 4a
c0 a9 d7 5a 1c cd c6 ab 02 39 09 30 ae e1 21 7c 2c 40 91 d0 e6 d4 d9 46 04 08 9e 9a 3c 3c 07 8e
58 80 37 94 2b 34 2f cc 51 2b 46 bb 32 19 49 34 54 65 a1 05 7b f6 e0 53 2b 07 5f a5 2e f7 8d 84
d1 31 cc 7f b7 9f 06 4f 81 fb 80 5e b7 02 10 76 5a 7c 78 37 92 7b 50 37 9e 32 e5 8c e8 97 15 70
9e fe 00
```

V13:

```
bf 00 2f ff 06 0b af c7 5d 61 f5 89 5c f5 1a b0 e3 d9 a7 0a 32 b3 0b 05 b2 e8 0e 8e 28 51 c6 9c
2b 46 06 86 7c 00 17 c8 98 d5 3e 00 02 fa ad aa 9a ec 00 19 dd f3 35 4d 00 00 48 1b 8d 79 4e 00
00 49 27 9f 27 48 00 00 1e 42 2b a3 40 00 00 05 95 96 be 31 00 00 00 7f 41 33 54 c0 00 00 00 01
70 e3 ae 8e d0 08 2f fb 5e 7b 63 81 c8 3c b4 92 56 70 34 bf d4 0d 60 2f bd b6 f3 52 6b 2f 5e fc
fc 78 51 ff 1d 51 64 17 a6 0a f1 98 f0 6d ba 14 b0 35 db 5b 15 1a 0f 28 16 16 60 94 f0 ae d5 d3
c4 32 3d 63 cb 1c d9 ec 50 c3 78 80 71 5e 96 a7 b9 6f 7b b3 c1 42 4e 1d e8 25 0e c1 f9 ee 46 28
f0 6d 91 aa 13 50 0a ca ae 50 18 07 c5 ea d5 7c 55 ac 0f e8 97 1a bb 7e d8 53 bd ee e9 10 a5 ae
cd 49 6c c6 e0 30 06 ae c2 92 f7 dc 39 8a b1 19 07 fe a3 55 3a d8 cc 54 2b 7d 34 b3 d9 a0 e0 d3
87 95 df 8c fd 8c a0 88 13 5d 58 b1 fb 99 63 b9 98 39 8c b1 70 d9 f4 30 9e 97 4f 59 60 a2 c9 64
62 6e 62 93 d8 fb 02 7f b9 f7 e7 04 25 98 c9 a7 2f f4 35 e3 65 2f dc f9 e2 8a 9f 9c b1 15 27 eb
9c 10 27 b8 5c da d9 df c8 76 07 97 44 2c 8b 0e b2 07 6d f7 34 bd ae 4f 38 cc 5b 92 75 f2 c6 b1
56 c8 c0 9f 2a 09 2a 32 6a fe a9 4e 17 7a 46 f7 a1 30 6e 3a 08 45 5a 2d 23 7d 18 06 ca 0b 1b 89
f9 28 0e 12 eb 24 66 22 18 98 b3 97 ff e6 1f fb bb 27 da 38 fc 24 fa dc cc 92 f2 67 ba 77 21 ef
02 41 47 de b6 cc 09 c4 b9 27 07 96 3b 6f e1 b5 f9 4c e0 6d e5 f4 a6 61 fd bc ec 08 4d 59 54 74
57 f6 c3 6e 46 9c 74 ce bf 91 16 a8 cd a8 d9 c8 17 e5 2e e5 6f b0 22 64 c6 10 ca 3e 5f 57 19 8d
c9 31 b7 94 56 44 f4 33 61 91 37 1e 75 9a 79 40 1d cf 34 78 7a 2b ec e8 a4 2d 90 17 e3 e8 25 94
f5 c8 62 a3 31 12 86 45 f8 05 06 04 ff 4c 52 b5 5e 5e fa 3f 22 05 e5 f3 ae 95 7e ef 15 44 fc f7
bb a7 44 01 e3 67 f7 f9 d1 cb f4 13 43 fb 86 72 6c a7 42 66 9a 77 a1 f5 26 a6 da 8a c1 f6 24 03
03 2c 8b bb 0a c1 eb 24 3d 1d a4 e7 e9 27 90 71 06 6a c4 c9 b4 34 23 f9 32 91 47 0a c6 fd b0 92
e7 b0 63 48 a7 c5 db e5 d5 e9 88 9d 00 b4 06 c1 40 0e 87 5f 7e 8a 46 8c 2c 75 b1 0c 2b 4b d4 ef
a8 b9 f6 8b e7 7b c0 6d 96 c1 bd 26 38 55 b7 dc 4f 4f c1 91 a4 12 06 8b 5c 82 2c 37 af 47 ef aa
51 fe 2f 54 22 ec da a4 e3 05 09 f9 a8 0e 37 44 74 36 24 01 f1 a3 33 f8 5b f4 95 88 a4 22 ea 91
9d c2 e3 8c f7 ff 26 70 47 4c 2e 4e ff f1 22 0d 37 df a8 aa 89 fe f7 76 cb 30 bb e2 75 5c f5 1d
ab a7 70 d0 4e c7 0c 31 02 1d 94 b0 de f0 c8 b5 98 fd 7a e2 46 da 14 b1 00 ee 09 9e 26 50 03 3c
e0 06 5a a6 40 88 e1 8c 50 a3 03 75 dc 58 cc 67 a2 32 1d 39 30 21 40 9e cf 1b 29 91 ab 12 88 52
94 8b 29 cd cd d6 02 c6 42 39 33 94 91 c5 a8 ed 87 b5 5b 49 e8 c3 e3 0d 1f 7a 07 18 7b fa 08 84
f1 90 8a 00
```

V14:

```
bf 00 2f ff 06 0b af c7 5d 61 f5 89 5c f5 1a b0 e3 d9 a7 0a 32 b3 0b 05 b2 e8 0e 8e 28 51 c6 9c
2b 46 06 86 7c 00 17 c8 98 d5 3e 00 02 fa ad aa 9a ec 00 19 dd f3 35 4d 00 00 48 1b 8d 79 4e 00
00 49 27 9f 27 48 00 00 1e 42 2b a3 40 00 00 05 95 96 be 31 00 00 00 7f 41 33 54 c0 00 00 05 e1
57 cf d4 f6 00 00 26 46 4e 0b ec 00 00 00 91 37 33 77 1e 00 00 00 4f a2 dc 26 52 00 00 00 0d e4
fc 02 1c 00 00 00 00 d8 a6 51 9e 38 00 00 00 04 fb 9d 9a 75 00 00 00 00 0b 9c 66 95 0e 00 00 00
00 0b 58 93 ac 70 00 00 00 00 04 c8 b7 4b b0 00 00 00 00 00 e9 60 75 cf 19 00 00 00 00 12 d6 bd
94 78 00 00 00 00 00 d8 f5 f8 8a 00 00 00 00 00 05 be 83 00 00 00 00 00 00 00 0c 6d 3c 2c 00 00
00 00 00 00 0d c7 87 d0 00 00 00 00 00 00 00 00 36 04 24 ed 00 00 00 5d 1c 0b f3 00 15 5d a7 2f
c1 84 e5 16 59 05 d9 01 16 19 77 9d bf 9d 8f 45 ba 54 fd f3 11 7f d0 89 21 93 30 b8 71 a2 20 89
e3 df 7c ba 48 50 a0 f1 e3 7a b7 dc 37 00 f5 86 7e 54 0f 89 da d9 49 cb 03 09 78 f2 14 b2 02 8c
8e 70 24 7e 00 bb 80 ab 42 38 31 dd 2a 2a d4 d1 90 bc 06 fe 9c ae ca 4f a3 ed 5d 15 08 51 fc 88
03 7a c1 00 05 88 ab 1f 1a ac b9 6a 6b b5 b4 0e 5f 35 34 fd 50 5b d9 cb 5c 6d 42 ab fe 62 99 df
14 a4 f6 28 17 26 d0 33 22 0d 23 2c b8 b9 52 18 6c 4c 19 b0 71 e2 66 83 77 1f b3 a7 13 56 7d 07
02 ac 4c 07 9f 25 85 ff a0 94 8e 60 8a 72 16 3c 73 f0 00 ee 95 c7 ce fe 05 11 22 2e 56 12 2f a2
f4 c0 25 95 e9 50 5a a8 d7 3d 1f cd b4 23 41 4a 3f fe 78 ad 6a 55 e0 b2 3d 58 94 43 9e a2 0d 99
e2 b8 2b 5e 2e 49 90 dd b3 81 62 42 1b 69 2b fe 28 51 02 cb 74 1b 62 16 ec ba fd 90 15 bc b5 ee
02 c6 da ce c9 c5 7e 34 05 8c 85 be 87 01 ab 3b 3f eb 47 9c b8 81 40 f6 d4 7e 41 25 6c 02 9c 94
eb 0d 70 4d 6e 68 ac 77 a9 05 f8 87 0b 4b 8c b3 58 96 81 74 bc af 39 12 5f 55 d9 e1 8a 8c 20 08
86 20 a3 3c 2c 54 ae 4d 64 ef 3d 9d d0 ce a7 40 14 b1 5b be f0 09 ae be 43 cd 0e de 2d ab a8 bd
0a a8 81 0f a2 ae e7 44 e0 05 27 14 48 80 ef 26 15 6a 4e b3 7b 2b 63 e1 ee 5a 49 aa 4c d6 dc 19
c9 0b 58 ca 8f 8f 64 b7 a5 36 47 f0 51 9b e0 9e 42 96 5b 58 e9 e7 ac a0 4d b7 8b 3a 46 52 6c d5
09 89 3c d4 45 02 ba 13 74 1b cd 28 ed 28 bf 8b 69 66 e7 35 24 c9 1d 2a 3a 16 05 41 44 1b 4e 8f
f0 d1 d8 2d 24 a6 57 51 b4 49 41 24 da b0 f6 2c 79 63 25 81 2d 30 ed 2d 86 10 4f d9 35 6d b8 22
4f bf b1 96 40 bf 96 49 a0 f1 b6 c4 62 d3 db db ff f3 67 0e c0 f6 38 00 e3 dd 7c a4 e7 88 37 4c
06 b5 39 33 30 7c a4 14 b5 58 1d e1 7d 5a a6 7f ea 13 82 7a 28 60 ec 18 29 05 9c 6e 80 37 fd ff
36 7f e5 38 3c eb 2f de a5 5c e3 6d fe 39 0f 5b c7 f2 40 3d 2d f8 62 1f e3 4e 00 6f f5 2a b0 7b
7b de 61 e7 d9 15 28 d9 05 4d 38 d3 ce 45 dd 4a a0 8e d5 87 fe f0 ef 86 07 68 30 74 5b 90 f0 6e
99 09 2d d0 e2 32 67 95 9b b3 90 8f d5 e8 af bc e9 7d 63 61 91 9b 1c 87 e3 0e dd 9e 6c 85 f1 f6
0f d6 5b 14 5d 49 16 14 bb d5 ac 96 29 1a 65 2e 69 5b 76 09 47 96 10 17 95 74 f1 d8 d0 7e 37 40
cb b7 de cb bf a6 3a 4b 10 d3 3d 9e 13 99 e3 45 13 4a f4 64 12 27 dd cd d3 1a 83 25 25 36 2f fe
00 df 99 c9 86 32 66 1a d9 3e 76 7e dc 1e 9d c3 00 16 44 6b 40 99 5f d6 0f 15 7e 9e c5 29 29 38
d9 86 ca e2 ff 59 70 f5 ba 2e 10 5d f8 b5 65 d1 20 73 40 95 7d 60 d3 4e ba 14 86 6b 46 1a cf d8
05 09 85 e6 29 ed e0 e0 7e 18 22 d8 61 c7 d8 0f 32 20 1a 33 a3 b7 59 4b 1e f5 d7 62 dd 30 e8 ae
36 b8 7d 6b 5f 1a 3f 61 80 e2 08 dc 23 25 91 68 70 3a e1 ab dc da f2 53 3a c0 b5 2c cf 96 f0 c3
ce c7 7d 90 a3 d4 be ea ec da db eb de 10 73 79 e9 39 68 1e 71 a1 2a c6 c4 37 f7 4c d4 5e 29 6d
7f 7e ce 1c 64 e5 af bf 63 2a df af 37 4f 00 fe 40 40 fe 39 33 80 d6 96 15 c8 fe ba 36 11 f3 4e
13 a1 8e 34 06 a9 e7 60 70 6f 63 10 e6 27 3e 49 aa c6 de 26 0a b8 55 e4 8d f1 db 3d ec b8 b3 a5
ea bd 6e 3d 7e 94 f4 5a 6f b3 55 10 cd 92 90 4a 78 22 d4 e6 65 2e 7b db 37 d2 20 1c ba 13 c1 3a
da 94 e4 3b f0 26 89 be 90 99 87 e0 ed 63 25 36 79 fc 7e 1f ea 2d 51 fc 2e 77 57 fb 50 b7 9d 45
cd ea ae 51 2c 35 86 45 e7 45 a3 7a d9 86 80 70 d4 0f 3d 93 b9 39 44 3a a5 ef 48 93 de fc 6a bf
b1 78 15 b5 3c 10 48 86 5c 60 34 4c 64 65 92 98 ab fb 77 99 22 80 da 74 c6 02 c2 4b 28 be 86 a5
a5 40 83 2d 0a a4 be 7a f4 f3 98 c5 21 9a 8d 78 91 58 0c 65 3f ee 89 1d a9 2a 9f 4f 93 c4 49 07
cb 6f d7 0a b7 3d ee 29 ea f5 be eb 82 70 0b 2b 20 8e e1 2c f3 74 f0 3b 11 c5 75 fb e5 85 84 6b
6a 68 a9 ef f5 03 91 58 9b fa 76 61 34 48 c2 8c 79 bb d0 c6 69 1b 58 c0 fc 8a da b3 3a ed 50 01
9b c8 54 1e bf 33 d6 f5 fc 28 a2 e3 0b a3 50 71 08 c6 48 65 07 37 bc 55 e3 ff 59 47 78 df 8b 7f
ca b8 c6 17 db c1 2c 5b 4d 71 1c d1 58 23 66 09 51 4b ca 81 4d e1 c6 e7 3f fa cd e3 21 e6 2c 65
70 93 53 93 94 91 de 51 14 3d 66 bd 0f 98 68 ee cc bd 82 00
```

### 13.4 Streams that are not clean

"`unar`" and "`bsdtar`" give what the tools do with the same bytes.

| # | Packed bytes | U | Required result | `unar` | `bsdtar` |
|--:|---|--:|---|---|---|
| E1 | `a0 00 67 fb 83 54 d7 35 97 42 2b 3b 8f 7d 90 0e b0 d1 30 46 11 00` | 24 | Corrupt: order code 0 (M = 1). | fails | fails |
| E2 | `83 67 fb 83 54 d7 35 97 52 71 14 02 98 5d 33 1b eb 77 eb` | 24 | Corrupt: header keeps a model, none exists. | fails | fails |
| E3 | `a3 00 67 fb 83 54 d7 35 97 52` | 24 | Corrupt: the packed bytes end (the first 10 bytes of V2). | fails | fails |
| E4 | `a3 00 61 04 01 be f8 00` | 3 | Corrupt: after `ab`, an escape in the root with every symbol available (8.1 step 3). | fails | fails |
| E5 | `a3 00 ff ff ff ff 00 00 00 00` | 1 | Corrupt: the first query (scale 257) gives v = 257. | fails | fails |
| E6 | `a3 00 61 03 0a 7d 6d 89 ef 4b be b9 3a 26` | 34 | Corrupt: `ab`, then a match of distance 10 and length 32 with 2 bytes produced. | 34 bytes: `ab`, 8 zero bytes, `ab`, 8 zero bytes, and so on | same as `unar` |
| E7 | `a3 00 02 06 cd 12 97 6d 3d 00` | 4 | Corrupt: a run as the first item of a group. | four zero bytes | four zero bytes |
| E8 | the bytes of V2 | 30 | Corrupt: end marker with 6 bytes missing. | fails | delivers the 24 bytes and reports no error when the stored CRC-32 is that of the 24 bytes (a file it writes to disk is extended to 30 bytes with zeros) |
| E9 | `a3 00 61 03 6d 92 47 9e f9 b1 6c 8b 20 a2 5e 00` | 20 | Corrupt: `abc`, then a match of distance 3 and length 40 with 17 bytes missing. | the first 20 bytes | fails |
| U1 | `a3 00 67 fb 83 54 d7 35 97 52 71 14 02 98 5d 32 d1 9c 00` | 24 | **Unterminated member** (11.2): the 24 bytes of V2's output are delivered, the group is not continued. | fails | the 24 bytes |

## 14. Safety requirements for untrusted input

1. **Never allocate the declared model memory up front.** Two bytes of
   input can declare 256 MiB. The arena must be backed on demand: only
   the parts that have been written (the text below TP, the units below
   LO and from US, the contexts from HI up) need to exist. Every decoded
   symbol writes at most one text byte and allocates at most M + 1
   contexts and M grown blocks, so the memory in use is bounded by the
   work done, which is bounded by item 3. Because the arena is used
   from three places at once (text upward from offset 0, blocks upward
   from A − X and downward from it, contexts downward from A), a single
   growing buffer does not achieve this; backing the arena in fixed-size
   pieces that come into existence when first written, or keeping the
   model's records outside the arena and tracking only offsets (6.8),
   does. An implementation may also
   refuse, with a distinct "unsupported" error, a declared size above a
   limit it documents; it must not silently use a smaller arena, because
   the arena size is part of the format (section 6). (DECISION.)
2. **Never size anything from U.** Output is produced incrementally
   into the window of `rar29-lz.md`.
3. **Bound the work.** Every symbol narrows the range, and every item
   consumes at least one symbol, so a block cannot loop without
   consuming input; but a symbol can cost far less than one bit and a
   run produces 259 bytes from three symbols, so U cannot be bounded by
   P in any useful way. The caller's limits on unpacked size apply, and
   decoding stops at the end marker or at the first item after U bytes
   are complete (section 11.2), so the work is bounded by U plus a
   constant times the number of symbols; the number of symbols a member
   can yield without output (filter records, block switches) is bounded
   by about 3,000 per packed byte. Implementations that need a hard
   bound should count symbols. One symbol costs at most a walk down the
   suffix chain (M contexts, 256 states each) plus, rarely, a merge
   pass, whose cost is proportional to the number of free blocks and
   which runs at most once per 255 blocks taken from text space; a
   model start costs a fixed amount (the 8,192 table entries, 400
   estimators and 256 root states).
4. **Range decoder**: every read is bounded by the member's packed
   bytes; a query whose scale is 0 or exceeds the range, a result
   v ≥ scale, and a zero range are corrupt input (section 4.3). Test
   the scale before dividing by it and the new range before dividing
   by that.
5. **Do not trust the model's invariants with memory safety.** They
   hold for every symbol sequence in the survey and in several hundred
   randomised runs, and hostile input can only choose symbols and
   escapes, which an encoder could choose too; but a decoder must still
   fail cleanly, with corrupt input, rather than read or write outside
   its structures if any of these is ever violated:
   - a symbol looked up in a context (9.2, 9.4) is not there: bound
     every search by n;
   - a successor that must designate a context (9.1, 9.4) is a text
     reference or none; a successor that must be a text reference (9.3
     case A, 9.4) is none;
   - a context that must have a suffix has none (8.2; 8.4 with n below
     256; 9.4 step 4);
   - a text reference at or beyond TP is read (9.4);
   - b ≤ 0 in 9.4 step 5 when the division is needed (division by
     zero), or the computed frequency does not fit 1..255;
   - the list of 9.4 exceeds M entries;
   - a binary context's frequency is 0 or above 128 when used as a row
     index (8.2);
   - n would exceed 256 in 9.5, or a block would exceed 128 units;
   - rescaling leaves no state (8.5 step 5).
6. **Totals wrap.** T and the estimators' sums are 16-bit quantities
   with wrapping arithmetic; esc in 8.5 and s0, cf, sf in 9.5 are
   computed in at least 32-bit signed arithmetic (values stay below
   2^24). In the survey T never exceeded 11,878 and a query scale never
   exceeded 18,981. In a valid model T exceeds the sum of the
   frequencies of its context by at least 1 at all times, which keeps
   esc in 8.5, b in 9.4 and s0 in 9.5 at 1 or more; a decoder must not
   depend on that for memory safety (item 5), and a total that has
   wrapped to 0 must not be used as a divisor (item 4).
7. **Allocator**: every offset stays within the arena; LO never passes
   HI; US never falls to or below TP; block sizes are from the class
   table only. The merge pass must not look at memory beyond the arena
   when a free block ends at its end.
8. **Matches**: check distance and length before copying (10.2, 11.2).
9. **Filter records**: at most 65,535 payload bytes; they are data for
   `rar29-filters.md`, whose own checks apply.
10. **A failed or unterminated member poisons its group** (11.2, 11.3):
    the model and the rest of the state are discarded, never reused.
11. **Release the model with the group.** The arena is needed only
    while the group is being decoded; a new-model header replaces it,
    it is never kept alongside a second one.

## 15. Observed implementation gaps

Faults and leniencies of the reference implementation (`unar` 1.10.7).
They explain why rules exist; nothing here is to be imitated.

1. **It does not stop at U.** Inside a PPMd block it decodes until the
   end marker or up to a fixed amount beyond the current position,
   whatever the member's size. A member without end marker therefore
   fails ("attempted to read more data than was available", vector U1),
   and in a solid group surplus output would be attributed to the next
   member.
2. **A match that crosses the end of the member is cut off silently**
   for a last member (vector E9).
3. **A distance beyond the output so far copies zeros** from the
   untouched window (E6, E7); a distance beyond the window size wraps
   around inside the window. No check is made.
4. **Errors while decoding operand symbols are ignored**: the "no
   symbol" result (−1) of a dead model is used as a value for match
   operands and filter lengths (a negative length then sizes a buffer).
5. **A query result at or beyond the scale is accepted** and treated as
   an escape or as the last symbol; decoding continues with a
   meaningless interval. A scale larger than the range, and a scale of
   0, divide by zero. The quantities cf and sf of 9.5 are computed
   unsigned, so that an impossible negative s0 would compare as a huge
   number.
6. **The declared model memory is allocated at once** from two header
   bytes, up to 256 MiB, and the result of the allocation is not
   checked.
7. **A header that keeps a model when none exists** runs the decoder on
   a zero-filled model record and happens to fail (E2); nothing checks
   it.
8. **The merge pass reads two bytes at the address just after each free
   block** to recognise a free neighbour. It stays inside the arena
   only because the last unit is always the root context. The marker
   test also relies on no context or state block ever starting with the
   marker's byte pattern.
9. **Symbol searches are unbounded loops** and successors are followed
   without checking their kind (section 14, item 5). No hostile input
   was found that breaks the invariants they rely on.
10. **The run counter** is a signed 32-bit integer incremented without
    bound; after 2^31 increments without a reset its behaviour is
    undefined in the reference implementation's language.
11. **Members with U = 0 are skipped unread**, whatever their packed
    bytes contain. `rar29-lz.md` explains why that is wrong when the
    skipped bytes hold an LZ end marker that changes how the next
    member starts; after a PPMd-ended member it happens to be
    harmless.
12. **A CRC-32 mismatch is reported but the bytes are written** all the
    same.

`bsdtar` 3.5.3, used here only as a second opinion: it accepts a member
whose end marker comes early, delivering the bytes decoded so far
(E8; the stored CRC-32 is then compared with those bytes alone), accepts
a member without end marker (U1), rejects a match that crosses the end
(E9), copies zeros for impossible distances (E6, E7), and supports
neither solid archives nor filters.

## 16. Decisions

| # | Section | Decision |
|--:|---|---|
| P1 | 3.2 | All order codes 1..31 and all memory bytes 0..255 are accepted; a decoder may refuse a memory size above a documented limit as "unsupported", never substitute a smaller one. |
| P2 | 4.3 | A query with scale 0 or above the range, a result v ≥ scale, and a zero range are corrupt input. |
| P3 | 5.3 | The run counter is a wrapping signed 32-bit integer. |
| P4 | 10.1 | Sub-codes 6..255 are literal escapes, like sub-code 1. |
| P5 | 10.2 | A distance beyond the group's output so far or beyond the window size is corrupt input. |
| P6 | 11.1 | Members with U = 0 are handled as `rar29-lz.md` says: decoded if they have packed bytes. |
| P7 | 11.2 | The end marker with bytes missing is corrupt input. |
| P8 | 11.2 | A match or run longer than the bytes missing (while some are missing) is corrupt input. |
| P9 | 11.2 | After U bytes are complete the decoder looks for the end marker; anything else makes the member "unterminated": delivered subject to CRC-32, group not continued. |
| P10 | 11.2 | What follows the end marker in the member's packed bytes is neither read nor checked. |
| P11 | 11.3 | After a failed member, later members of the group are reported failed without decoding. |
| P12 | 14 | The model memory is backed on demand; invariant violations are corrupt input. |

None of them changes how any archive of the survey is read.

## 17. Open questions

They do not block an implementation; the document says what to do in
each case.

1. **Few PPMd archives "from the wild" were examined.** Most
   RAR-written evidence comes from `rar` 4.20 (Linux) and 6.24 (macOS,
   switch `-ma4`), run for this purpose. The real archives of section
   18 add 83 small members written by unknown RAR versions, none of
   which restarts the model. Encoders of RAR 2.9 to 3.9 may use the
   format differently within the rules: in particular they send filter
   records with sub-code 3 and switch blocks with sub-code 0 more often
   (the two tested versions, as driven here, wrote sub-code 0 once and
   sub-code 3 never; `rar29-lz.md` found both in its own survey).
2. **The eight bytes after the end marker** (11.2). They look like an
   LZ block that ends the member. If an encoder exists whose decoder
   must read them (for instance because the next member's need for a
   block header is announced there rather than implied), rule P10 and
   the first bullet of 11.1 would be wrong for it. In all 2,445
   observed cases the two readings agree, and the same eight bytes
   follow the end marker of all 83 real-archive members of section 18.
3. **Members without an end marker** (P9): not observed.
4. **Unpack version 36** was not produced by the tested encoders; the
   reference implementation treats it as 29.
5. **Model memory above 128 MiB** and the order codes `rar` does not
   offer (P1): accepted on the strength of the reference implementation
   and two crafted streams.
6. **Run counter overflow** (P3) needs more than 2^31 consecutive
   confident predictions without an escape; what a genuine encoder does
   then is unknown.
7. **Encryption** is not specified.
8. **A filter record or a block switch decoded after the member's U
   bytes are complete** (11.2 processes them normally) was not seen in
   any archive. Both black-box tools accept a block switch there
   (vector V5f) and a filter record there (the record of V7 placed
   between the last literal of V2 and its end marker; the output is
   unchanged); what the filter layer does with a record that refers to
   positions beyond the member is for `rar29-filters.md` to say.

## 18. Evidence

**Reference implementation.** XADMaster, revision `7cb9ee0a`: the PPMd
variant H model with its context, escape-estimator, range-coder and
memory-manager parts, and the RAR 3.0 decompression handle. Kept outside
this repository.

**Independent re-statement.** Sections 4 to 11 were re-implemented as a
throw-away experimental decoder and matching encoder (kept outside the
repository, not part of any deliverable); the figures below come from
running it. It keeps contexts as objects and tracks arena offsets
separately, so it also demonstrates the freedom claimed in 6.8.

**The reference model in isolation.** The reference implementation's
model was built unchanged into a small driver and compared with the
experimental decoder on (a) 150 streams of random bytes and (b) 420
streams made by the experimental encoder from random, repetitive,
text-like and mixed data, with M from 2 to 64 and arenas from 2,200
bytes to 1 MiB (the small arenas force a restart every few hundred
symbols: up to 2,105 restarts in one stream). Every stream decoded
identically.

**RAR-written archives.** 48 archives written by `rar` 4.20 and 6.24
(inputs: a 2.4 MB word list, generated text with long repetitions, 1,120
manual pages as solid and non-solid archives, an x86 executable,
synthetic audio, files of random bytes restricted to 128 of the 256 values,
single-byte and empty files), with orders 2 to 61 and memory sizes 1 to
128 MiB:

- 2,488 members in 229 solid groups, 2,442 of them compressed members
  beginning with a PPMd block (58.8 MB of output); all decode to the
  CRC-32 in their header.
- 2,446 PPMd block headers: 200 with a new model (198 of them with an
  escape byte), 2,246 keeping the model of the previous member. Escape
  values 255, 205 and 143.
- 604 model restarts through exhaustion; 16,613 merge passes; 4.2
  million blocks taken from text space; 152,546 rescales, 30,274 of them
  removing states, 19,296 shrinking a block, 4,183 turning a context
  binary; 4.3 million maximum-order successor creations (9.3 case A);
  983,268 text step-backs; successor chains of up to 21 contexts.
- Never observed: a query result at or beyond the scale; a text read at
  or beyond TP; b below 1 in 9.4; esc below 1 in 8.5; a member not ended
  by sub-code 2; sub-codes 3 and 6..255.
- **Allocator variants.** Two archives of 300,000 near-random bytes
  written with orders 2 and 3 and 1 MiB (by both `rar` versions) decode
  correctly with the allocator of section 6 and fail with each of three
  altered allocators: no joining in the merge pass; first-in, first-out
  free lists; a merge pass that processes blocks in address order
  instead of the order of 6.7.

**Crafted streams given to `unar` 1.10.7 and `bsdtar` 3.5.3** (each
compared with the output predicted by this document; identical in every
case where the document does not deliberately differ, sections 15 and
16): all vectors of section 13; every order code 0, 1, 15, 16, 17, 31;
memory byte 255; headers keeping the model with and without an escape
change and with non-zero ignored bits; block switches PPMd to PPMd, to
LZ and back; a solid pair with the model kept, with a new model, and
with an LZ member between two PPMd members; matches at both ends of the
length ranges and at the largest possible distance for the output so
far; filter records in all three length forms; literal escapes by
sub-codes 1, 6, 7 and 255; a member with U = 0 holding only a PPMd
block header and an end marker; trailing bytes after the end marker;
and, for
section 2 item 4, a member consisting of an LZ block with a match of
distance 8 and length 8, a PPMd block with a match (distance 2) and a
run, and a second LZ block that repeats "the last match" and then uses
the most recent remembered distance: both items reproduce the LZ
block's distance 8, not the PPMd block's (with `unar`; `bsdtar` rejects
this stream).

**Independent review.** A reviewer wrote a second throw-away decoder
and a matching encoder from the text of this document alone, before
reading the reference implementation, and then audited the document
against the reference implementation clause by clause. With that
decoder and encoder:

- every vector of section 13 decodes as stated from the bytes printed
  here, the traces of section 12.1 and the allocator sequences of 6.9
  are reproduced value for value, and the tables of 6.3, 7.2 and 7.3
  agree with their generating rules;
- the first survey's archives that consist of PPMd blocks only were
  decoded again: 1,303 members, 32.8 MB, every CRC-32 correct;
- 96 streams made by the second encoder (random data over 4, 16 and 256
  symbols, skewed data and data with long repetitions; 3,000 to 250,000
  bytes; M from 2 to 64; 1, 2 and 4 MiB; escape values 2, 0, `0x41` and
  `0xff`; 54 model restarts and 680 merge passes in all) were extracted
  by `unar` and by `bsdtar`: both return the original data for all 96;
- **real archives**: in a collection of 458 archives from public
  software and art-scene collections (the survey of `rar29-lz.md`; 411
  of them are plain RAR4 files the test harness could read), 83
  members of 16 archives begin with a PPMd block and consist of PPMd
  blocks only (189,195 bytes; unpack version 29; 34 headers with a new
  model and no escape byte, 53 with a new model and an escape byte, 1
  keeping the model). All 83 decode to the CRC-32 of their header, each
  ends with sub-code 2 exactly at U, and each is followed by the eight
  bytes `00 bf 88 67 f6 a9 ff d4`. Seven further members leave their
  first PPMd block through sub-code 0 for an LZ block (three of them
  after a sub-code 3 record); those are covered by `rar29-lz.md`;
- a member split across 2 and across 11 volumes (section 11.4);
- 3,200 streams of random bytes and of damaged vectors, with arenas
  from 2,200 bytes to 1 MiB: every one ends in one of the three
  corrupt-input results of sections 4.3, 8.1 and 4.1 (a query result at
  or beyond the scale, an escape from the root, the end of the packed
  bytes), and none violates an invariant of section 14, item 5;
- the property "T exceeds the sum of the frequencies by at least 1" of
  section 14, item 6, checked after every symbol of three RAR-written
  archives and three vectors (2.4 million checks; the minimum is 1).

**Vendor tools used as black boxes.** `rarlinux-x64-4.2.0.tar.gz` and
`rarmacos-arm-624.tar.gz` from the vendor's site; only the `rar`
program, only to create archives.

## Clean-room feedback

Notes from the implementation in `src/formats/dos/rar_native/ppmd.rs`,
written from this document alone.

**Results.** Every vector of section 13 that does not need another
document decodes as stated on the first complete build: V1 to V4, V5a,
V5b, V5d, V5f, V6, V7, V7b, V8 to V14, and E1 to E9 and U1 with the
member rules of 11.2 applied by a small test harness. The range-decoder
values of all fourteen symbols of 12.1, the allocator sequences A and B
of 6.9 and the spot-checked entries of the tables of 6.3, 7.2 and 7.3
are reproduced. For V5c and V5e the test stops at the block switch and
checks that the read position is byte 21 and that the model continues
after the LZ block. No archive on the implementer's machine contains a
PPMd block (26 archives looked at), and no archive creator was
available, so no additional real-data check was made.

**Decisions taken where the document leaves room.**

1. Model memory (section 14 item 1): the arena is a real arena of A
   bytes addressed by offsets, backed in pieces of 64 KiB that come into
   existence when first written. Every memory byte 0..255 is accepted;
   no "unsupported" limit is applied. A piece that cannot be obtained
   from the host fails the member. Pieces are kept when the model
   restarts or when a new-model header asks for the same arena size.
2. Kinds of successor (5.1): a context reference is stored with a tag
   bit instead of being recognised by its address. Section 6.2 says the
   smallest text reference is 1 and the reference implementation treats
   everything below the units area as text; because the units area can
   grow downward (6.5 step 3) to within one byte of the text pointer,
   and a state can hold the text reference "one past the byte just
   written" at the moment the text pointer steps back (9.3 case B), an
   address comparison looked as if it could, in a corner, take such a
   reference for a context. The tag avoids the question. If the address
   test is in fact always unambiguous, a sentence saying why would help.
3. Free neighbours in the merge pass (6.7, 6.8): a free block carries a
   marker value that no context and no block of states can begin with
   (in this implementation's own layout), every allocation clears it,
   and the untouched gap and the end of the arena are excluded by
   comparing offsets, as section 14 item 7 asks.
4. 9.2 and 9.4 say that a binary context's only state is "the" state for
   the symbol. The implementation also checks that its symbol is the one
   looked for and reports corrupt input otherwise.
5. 8.4: the number of unmasked states is counted while summing their
   frequencies instead of being derived as n minus the number of masked
   symbols; the two agree in a consistent model.
6. 9.4: a text reference of 0 or at or beyond the text pointer is
   corrupt input before the text byte is read (section 14 item 5).
7. D above M after a step down the suffix chain, a frequency of 0 or
   above 128 in a binary context, a state frequency that would not fit
   8 bits, and a context reference that does not point into the units
   area at a record with 1..256 states are all corrupt input.
8. Header (3.1, 3.2): both optional bytes are read, then the two checks
   are made, then the escape value is replaced, then the four
   range-coder bytes are read, and only then is model memory obtained.
   The order of the last two steps is not observable.
9. Interface: the component returns items (literal, match, filter
   record, block switch, end of member) and leaves to the caller the
   window, the checks of 10.2 and all of 11.2 and 11.3, including the
   "unterminated member" rule. E, which section 7 leaves as it is at a
   model start, starts as 0 in a newly created model.

**Points that could be stated more explicitly.**

- 8.1 step 3 compares the number of states with "the number of masked
  symbols". It may help to say that this number is always the number of
  states of the context just escaped from.
- 8.5 step 2: "T" there is the total after the increase by 4 of 8.3 or
  8.4. The text is consistent with that reading (and V8, V9 confirm
  it), but it has to be inferred.
- 9.5: the row "cf ≤ sf" and the following rows are evaluated in
  signed arithmetic; with a negative sf every comparison fails and the
  last row applies. Section 15 item 5 hints at this; the table could
  say so.
- 6.6 "Grow": how many units' worth of contents are moved is left
  implicit (the u units of the old block suffice).

**From the end-to-end tests (container, PPMd, LZ and filter layers
together).**

- 13.2 says the 222-byte payload of V7 and V7b "defines a two-channel
  delta filter". The filter layer recognises the program that this
  payload defines as the standard **audio** program of
  `rar29-filters.md` 9.1 (216 program bytes; the delta program has 29
  and could not fill a payload of this size), run with two channels.
  Both vectors give the stated final CRC-32 (`0x43195E29`,
  `0xB1CF103F`), so only the word is wrong: it should say audio.
- Three archives of the verification set that are variants of V7b are
  rejected, although `unar` extracts them with the declared CRC-32
  (two with U = 132 and `0xB1CF103F`, one with U = 64). In each the
  second record reuses the audio program **without a register field**
  (flags `0x00` or `0x01`), so R0, the channel count, is its default 0.
  That is corrupt input by `rar29-filters.md` 9.2 and decision F11
  (audio needs 1 to 254 channels). `unar` accepts a count of 0 and
  returns what the previous run left in its memory (section 22 item 9
  there), which here evidently is what the checksum expects. The
  rejection was kept; see the note in `rar29-filters.md`. These
  archives should not be listed as "must decode".

**Speed (black-box comparison with the reader this one replaces).**

- The first version decoded PPMd blocks 1.6 to 2.1 times slower. The
  piecewise arena of section 14 item 1 was not the cause: with one
  piece covering the whole arena the time changed by a few percent
  only. What cost time was reading a context field by field and its
  states one at a time, each read finding its piece again, and above
  all the test "is this symbol masked" as a branch inside the two
  passes of 8.4, which the processor cannot predict. The model now
  reads a context's unit once, looks at the n states of a context as
  one run of 6n bytes (copied out first in the rare case that the run
  lies in two pieces), and counts a masked state as frequency 0
  without branching. Results are unchanged for every vector and for
  the PPMd archives of the corpus; the time is now within 1.25 times
  that of the earlier reader. Section 6.8 already allows all of it; a
  hint in 8.4 that the masked test is the hot spot would have saved
  the detour.
