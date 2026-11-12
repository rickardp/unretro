# StuffIt archive containers (classic "SIT!" family and StuffIt 5)

Clean-room format specification. It describes the two container layouts used by
`.sit` archives and the process for turning one into a list of entries with
fork locations, and it defines how the container layer and the method
decoders work together (section 2.2). Of the compression algorithms it
specifies only method 0 (section 2.2) and the framing of method 14 (section
2.3); methods 1, 2, 3 and 13 are specified in `lz-methods.md` and method 15
in `arsenic.md`. `README.md` in this directory is the index of the document
set and holds the shared glossary.

## 1. Conventions

- All multi-byte integers are **big-endian** and **unsigned** unless stated.
- An **archive offset** is a byte offset from the first byte of the archive
  (the first signature byte), starting at 0. Every offset *stored in* an
  archive is an archive offset. Tables that describe one structure (a record
  header, an entry header, a second block) give **structure offsets**, counted
  from the first byte of that structure; each such table says so.
- Ranges are written `a..b` and include both ends.
- **L** is the number of archive bytes actually available to the reader,
  counted from archive offset 0.
- A **fork** is one of the two byte streams of a classic Mac file: the **data
  fork** and the **resource fork**. Either may be empty.
- **Packed** bytes are a fork as stored in the archive (after compression);
  **unpacked** bytes are the original fork contents. The **packed length**
  of a fork is written **P** and its **unpacked length**, as declared in the
  entry header, **U**. Both are untrusted header values.
- The **fork checksum** is the 16-bit checksum field an entry header holds
  for each fork (section 2.2). **CRC-16** always means the algorithm of
  section 2.1.
- The **container layer** is the part of a reader that this document
  specifies; a **method decoder** is the part that undoes one compression
  method (section 2.2).
- **Classic** means the first container generation (StuffIt 1.x to 4.x).
  **SIT5** means the second generation (StuffIt 5 and later, still `.sit`).
- An **entry** is one file or one folder in the result list.

### 1.1 Evidence labels

| Label | Meaning |
|---|---|
| (no label) | Checked against real archives, and in agreement with the reference implementation wherever it reads the field in question. The real corpus is 19 distinct classic archives (1,050 record headers: 914 files, 68 folder start markers, 68 folder end markers; one archive is password-protected) and 26 distinct SIT5 archives (979 entries: 872 files, 107 folders; 107 end-of-folder markers). |
| BLACK-BOX | Checked by feeding hand-built archives to an independent extraction tool run as an opaque program and observing its listing and its integrity test. |
| REFERENCE-ONLY | Stated by the reference implementation only; no real archive exercises it. |
| INFERRED | Deduced from patterns in real archives; not confirmed by any implementation. |
| DECISION | A decision made by this specification for input that no real archive exhibits, or on which the existing readers disagree. It is chosen to be deterministic and safe; it is not a fact about the format. (`lz-methods.md` and `arsenic.md` use the same label.) |

The **reference implementation** is the third-party StuffIt library, version
0.1.8, which the implementation written from these documents replaces. The
**independent tool** is `unar`/`lsar`, version 1.10.7. `lz-methods.md` calls
the latter by its name.

### 1.2 Outcomes a reader reports

This document uses four outcome words with fixed meanings.

| Word | Meaning |
|---|---|
| **not this format** | Detection (section 5) fails. Nothing is listed. |
| **finding** | A note recorded for the caller (a warning). The listing is unaffected. |
| **incomplete** | The listing stops early or may lack entries. Entries already listed stay valid and are delivered **together with** an indication that the archive is damaged or truncated. A reader must never present an incomplete listing as plain success. A reader whose interface cannot deliver both must deliver the error. |
| **fork error** | The entry stays in the list and listing continues, but that fork cannot be delivered; an attempt to read it fails with the stated reason. |

## 2. Elements common to both generations

### 2.1 Checksum (CRC-16)

One checksum algorithm is used everywhere in both containers (header
checksums, name checksum, fork checksums):

| Parameter | Value |
|---|---|
| Width | 16 bits |
| Polynomial | x^16 + x^15 + x^2 + 1; 0x8005 in normal form, **0xA001 in reflected form** |
| Bit order | Reflected: input bytes are consumed least significant bit first, and the register shifts right |
| Initial value | 0x0000 |
| Final XOR | 0x0000 (none) |
| Storage | The 16-bit result is stored big-endian (high byte first) |

Bit-level procedure:

1. Set the 16-bit register to 0.
2. For each input byte in order: XOR the byte into the low 8 bits of the
   register. Then repeat 8 times: note the lowest bit of the register, shift
   the register right by one bit, and if the noted bit was 1, XOR the register
   with 0xA001.
3. The register is the checksum.

Check values:

| Input | Checksum |
|---|---|
| empty | 0x0000 |
| ASCII `123456789` (9 bytes) | 0xBB3D |
| ASCII `Hello World` followed by LF (12 bytes) | 0x48FE |
| ASCII `RSRC` (4 bytes) | 0x389C |
| ASCII `Hi!` followed by CR (4 bytes) | 0x291E |

Any run of zero bytes has checksum 0x0000. A checksum match on an all-zero
region proves nothing (see sections 3.6 and 5).

### 2.2 Fork checksum and fork delivery

This section is the single normative statement of how a fork's unpacked
bytes are obtained: what the container layer does, what it hands to a method
decoder, what it gets back, and who checks what. `lz-methods.md` and
`arsenic.md` restate the parts that concern their decoders and defer to this
section.

**The fork checksum.** Each fork has a 16-bit checksum field in its entry
header, called the **fork checksum** throughout the document set.

- It is the CRC-16 of section 2.1 computed over the **unpacked** fork bytes
  (all of them, in order), never over the packed bytes. Checked by hand on
  every method 0 fork of the corpus, and BLACK-BOX on every unencrypted fork
  of the corpus archives under 12 MB (more than 2,200 forks; methods 0, 2 and
  13 in classic archives, 0, 13 and 15 in SIT5 archives): the independent
  tool's integrity test passes on all of them and fails when the field or the
  data is altered.
- **Exception, method 15:** real SIT5 archives store 0x0000 in the fork
  checksum field of every method 15 fork (1,192 of 1,192). Method 15 carries
  its own integrity check, the **stream checksum** (a CRC-32), inside its
  packed stream (`arsenic.md` section 11.4). The container layer must not
  compare the fork checksum field for method 15 forks, whatever it contains
  and whichever generation the archive belongs to (archives written by the
  reference implementation store a CRC-16 there; BLACK-BOX: for a method 15
  fork in a classic archive the independent tool accepts 0, the correct
  CRC-16 and a wrong value alike, and fails only when the stream checksum
  fails).
- For an empty fork the field is 0x0000 in every real archive.
- What the checksum of an encrypted fork covers is unknown.

**The two components.** The **container layer** reads the archive structure
(sections 3 and 4). A **method decoder** (decoder for short) turns the packed
bytes of one fork into its unpacked bytes for one method number.

- The container layer hands a decoder exactly three things: the **method
  number**, the **P packed bytes** of one fork, and the **unpacked length U**
  declared in the entry header. A decoder is not given the fork checksum, the
  entry, or anything of another fork; no state is carried from one fork to
  the next.
- A decoder returns either **exactly U bytes** or an error. It never returns
  fewer or more than U bytes as a success.
- The **container layer verifies the fork checksum** over the U bytes
  returned (step 9). The one integrity check a decoder performs itself is the
  stream checksum of method 15, because it is part of that stream.

**Delivery steps.** A fork is described by five header values: method number,
packed length P, unpacked length U, fork checksum, and its **encryption
state**. The encryption state has three values: not encrypted; encrypted;
and encrypted with a malformed packed length, which only a classic archive
can produce (section 3.7). It is settled once, when the entry is listed, by
the parser of the container generation concerned, and is part of what the
listing records for the fork; the delivery steps are the same for both
generations and never need to know which generation a fork came from.
Delivering the unpacked bytes of a fork follows these steps, tested in this
order (all DECISION except where a label says otherwise):

1. If U is 0 the fork is **empty**. Nothing is decoded and the method number
   is not examined (real archives give empty forks method 0 and P = 0;
   archives written by the reference implementation give them the method of
   the rest of the archive, for example 13). If P is not 0 the P packed bytes
   still occupy their place in the archive (they count when locating whatever
   follows) but are ignored; record a finding (who records it, and when, is
   stated after these steps). This holds for every method number, 15
   included, and whatever the encryption state. (BLACK-BOX, mixed: for a classic archive the
   independent tool lists no such fork at all; for a SIT5 archive it extracts
   an empty file and does not examine the packed bytes of a method 15 fork
   whose U is 0, but its integrity test reports a failure for a method 13
   fork with U = 0 and P = 0, as the reference implementation's writer
   produces for an empty file.)
2. If the encryption state is "encrypted": fork error "encrypted,
   unsupported". If it is "encrypted with a malformed packed length": fork
   error "corrupt" (section 3.7).
3. If P is 0 (and U is not): fork error "corrupt".
4. If the packed byte range (sections 3.5 and 4.8) does not lie entirely
   within **L**, the bytes actually available: fork error "truncated". The
   bound is L in both generations. In particular the declared archive length
   of a classic archive plays no part in this test (section 3.6, step 7), so
   the test needs nothing but the fork's offset, P and L, and may be made
   when the fork is read.
5. If the method number is not one this reader implements: fork error
   "unsupported method" naming the number. The packed bytes must never be
   passed through as if they were the contents.
6. If U exceeds the caller's limit for one fork, or would take the running
   total of unpacked bytes of the archive above the caller's limit for one
   archive (section 6, requirement 3): fork error "output limit exceeded".
7. Method 0: P must equal U, otherwise fork error "corrupt". The unpacked
   bytes are the packed bytes; no decoder is involved. (Real archives: equal
   in all 19 non-empty method 0 forks. BLACK-BOX: the independent tool
   reports a size or checksum failure when they differ.)
8. Any other method: call the decoder with the method number, exactly the P
   packed bytes, and U. A decoder error is a fork error, as the table below
   shows.
9. Verify the fork checksum: compute the CRC-16 of the U unpacked bytes and
   compare it with the field, for every method except 15. A mismatch is the
   fork error "checksum mismatch". Method 14 is verified like the others (the
   reference implementation's writer stores a correct CRC-16 for it). For
   method 15 this step is skipped; its decoder has already verified the
   stream checksum, and a failure of that is reported as the same fork error
   "checksum mismatch".

What step 8 means for each group of methods. "Corrupt" is the fork error
"corrupt"; the decoder documents distinguish finer kinds (truncated input,
corrupt input, and the failure kinds of `arsenic.md` section 12.2) for
diagnostics, and all of them are reported as "corrupt" except a stream
checksum mismatch. (A decoder's own refusal for lack of resources, an output
cap of its own or memory it cannot obtain, is not a fault of the stream and
is reported as "output limit exceeded": `lz-methods.md` section 2.6 and
`arsenic.md` section 12.2.)

| Situation | Methods 1, 2, 3, 13 (`lz-methods.md`) | Method 14 (section 2.3) | Method 15 (`arsenic.md`) |
|---|---|---|---|
| The stream needs a bit or byte beyond the P packed bytes | corrupt | corrupt | corrupt |
| The stream contains something its specification calls invalid | corrupt | corrupt | corrupt |
| The stream marks its own end before U bytes exist | corrupt (only method 13 has such a mark, and only an early one is ever read) | corrupt | corrupt |
| U bytes exist | Decoding stops at once, also in the middle of a run, string or match. Success. Nothing further is read. | Decoding continues to the end of the final block; if that produces one more byte: corrupt. If the input ends first: corrupt. | Decoding continues through the final end flag and the stream checksum; if a block would append one more byte: corrupt. If the input ends first: corrupt. |
| Packed bytes are left over when the decoder has finished | Ignored, however many; not an error (several methods pad their stream). | Ignored; not an error. | Ignored; not an error. |
| U is larger than P packed bytes could ever produce | corrupt, before decoding (`lz-methods.md` section 2.6) | no such test | no such test |

So **short output is always an error**, for every method; **trailing unused
input is never an error**, for any method; and **surplus output** is an error
exactly for the two methods whose streams end themselves (14 and 15), while
for methods 1, 2, 3 and 13 it cannot be observed, because their decoders stop
reading at U.

**Where the finding of step 1 is recorded.** The condition "U is 0 and P is
not 0" is visible in the entry header, so the finding belongs to the
**listing**: the container parser records it when it lists the entry, once
for each fork concerned, whether or not that fork is ever read. Reading the
fork records nothing further. It is the only finding that the delivery
steps give rise to: the conditions of steps 2 to 9 are fork errors, reported
when the fork is read, and none of them is also recorded as a finding at
listing time, whatever the encryption state (section 3.7 says the same of a
malformed encrypted packed length). It is a finding like any other: it does not
make the listing incomplete, and strict checking (section 4.11) does not
turn it into an error, because strict checking concerns the three SIT5
header checksums and nothing else. (The synthetic archive
`sit5-m15-vectors.sit` contains one such fork on purpose, entry 1 with U = 0
and P = 8; listing it yields exactly one finding of this kind, with or
without strict checking, and the listing is complete.)

**No contents on error.** When any step ends in a fork error, including a
checksum mismatch found after all U bytes were produced, the bytes produced
so far are not delivered as the fork's contents. A reader may expose them
through a separate, explicitly marked salvage facility; nothing requires
one. A reader that delivers a fork incrementally may already have handed out
bytes when the error is found; it must then report the error no later than
the point at which its consumer would otherwise see the end of the fork.

### 2.3 Compression method numbers

The method number is one byte per fork. In classic archives only the low 4
bits of the byte are the method number (the high bits are flags, section
3.3). In SIT5 the whole byte is the method number; every real SIT5 method
byte is 0, 13 or 15.

There is **one numbering shared by both generations**. The table is not a
per-generation whitelist: the "seen" columns record observation only.

| Number | Name | Seen in real classic | Seen in real SIT5 | Notes |
|---|---|---|---|---|
| 0 | Store (no compression) | yes | yes | Packed length equals unpacked length. |
| 1 | Run-length encoding (escape byte 0x90) | no | no | |
| 2 | LZW (variable-width codes) | yes | no | |
| 3 | Huffman (tree stored in stream) | yes (1 fork) | no | Found in one MacBinary-wrapped archive that is not part of the corpus of section 1.1; see `lz-methods.md` section 5.6. |
| 5 | "LZAH" | no | no | Name only (BLACK-BOX). Not specified anywhere in this document set. |
| 6 | "Fixed Huffman" | no | no | Name only (BLACK-BOX). Not specified. |
| 8 | "MW" | no | no | Name only (BLACK-BOX). Not specified. |
| 13 | LZ77 + Huffman ("LZ+Huffman") | yes | yes | The dominant method of classic version 2 archives. |
| 14 | Deflate (the independent tool calls it "Installer") | no | no | See below. |
| 15 | "Arsenic": block sorting (BWT) + arithmetic coding | no | yes | The dominant method of SIT5 archives. Valid in a classic archive too (below). Fork checksum field is not used in either generation. |

Numbers 4, 7, 9, 10, 11 and 12 are not assigned as far as is known
(BLACK-BOX: the independent tool has no name for them and reports them as
unsupported). No number above 15 can occur in a classic archive.

DECISION: a reader selects the decoder by the method number alone, whichever
generation the archive belongs to. A SIT5 fork carrying method 1, 2 or 3, or a
classic fork carrying 14 or 15, is decoded with the same algorithm as in the
other generation; the fork checksum (for method 15: the stream checksum)
then decides whether the result is accepted. (BLACK-BOX: the independent tool
applies one method table to both generations. The reference implementation
instead knows 0, 1, 2, 3, 13 for classic and 0, 13, 14, 15 for SIT5.)

In particular **method 15 in a classic archive** is decoded exactly as in a
SIT5 archive, and its fork checksum field is not compared (section 2.2). No
real classic archive uses method 15. BLACK-BOX: the independent tool unpacks
a method 15 stream placed in a classic archive correctly, whatever the fork
checksum field holds, and fails it when the stream checksum is wrong. The
reference implementation returns the packed bytes of such a fork as if they
were its contents (section 7, item 2).

DECISION (continued): a number with no decoder available,
including 5, 6, 8, the unassigned numbers, and any SIT5 method byte above 15,
gives the fork error "unsupported method" of section 2.2. (BLACK-BOX: the
independent tool looks only at the low 4 bits of a SIT5 method byte; the
reference implementation looks at the whole byte. No real archive has a value
above 15, so rejecting is the safe choice.)

Classic methods 1 and 3 do not occur in the corpus of section 1.1; their
container handling (method number in the low 4 bits, checksum over unpacked
bytes) is confirmed BLACK-BOX with synthetic archives that the independent
tool extracts with matching checksums, and for method 3 by the one real fork
mentioned in the table.

**Method 14 framing.** In archives written by the reference implementation a
method 14 fork is exactly one **raw DEFLATE stream as defined by RFC 1951**:
no zlib header (RFC 1950), no Adler-32 trailer, no gzip wrapper (RFC 1952),
no length prefix. The first packed byte is the first byte of the first
DEFLATE block. The stream ends at the end of the block whose final-block bit
is set. This was confirmed by inflating method 14 forks written by the
reference implementation with a general-purpose raw-DEFLATE decoder: the
output has exactly the unpacked length, the stream ends exactly at the last
packed byte, and its CRC-16 equals the fork checksum field.

Whether archives written by genuine StuffIt software use this framing for
method 14 is **unknown**: no real archive in the corpus uses method 14, and
the independent tool (BLACK-BOX) fails to unpack method 14 forks in this
framing as well as in zlib and gzip framing. DECISION: decode method 14 as raw
RFC 1951 (a low-bit-first bit stream, in the terms of `lz-methods.md`
section 2.1). The stream must be decoded to the end of its final block and
must produce exactly U bytes; packed bytes after the end of the final block
are ignored. Apply steps 8 and 9 of section 2.2. Because the framing of
genuine method 14 forks is unknown, every fork error those two steps give
for a method 14 fork ("corrupt" or "checksum mismatch") is reported with the
message "unsupported method 14 variant or corrupt data".

### 2.4 Timestamps

Creation and modification times are 32-bit counts of seconds since
**1904-01-01 00:00:00** in the creator's **local time**. No time zone is
stored.

- The value 0 means "no date". DECISION: every other value is a date, including
  values that fall before 1970.
- To convert to a count of seconds since 1970-01-01 00:00:00, subtract
  2,082,844,800 using signed arithmetic of at least 64 bits (the result is
  negative for dates before 1970).
- The result is a **zoneless local timestamp** (year, month, day, hour,
  minute, second as written on the creator's clock). It must not be labelled
  UTC and must not be shifted by the reader's own time zone. A consumer that
  can only store UTC instants should store the same calendar fields as if they
  were UTC. (BLACK-BOX: the independent tool prints the same calendar fields
  regardless of the time zone it runs in.)

Example: 0xB8515FAD = 3,092,340,653; minus 2,082,844,800 = 1,009,495,853;
that is **2001-12-27 23:30:53** local time.

The representable range is 1904-01-01 00:00:00 to 2040-02-06 06:28:15. In the
corpus two SIT5 entries carry values before 1970 that are evidently not real
dates (0x0012981C, and 0x453EE76A, which looks like a count from 1970 written
by mistake). A reader converts them like any other value and does not guess.

### 2.5 File type, creator, Finder flags

- **Type** and **creator** are 4 raw bytes each (conventionally printable
  MacRoman such as `TEXT` / `ttxt`). Keep them as bytes; do not decode.
- **Finder flags** are the 16-bit classic Finder flag word. Bits a consumer
  may care about: 0x4000 invisible, 0x8000 alias, 0x0400 has custom icon,
  0x2000 has bundle, 0x0100 has been inited.
- Folders have no type and no creator. The bytes that hold type and creator
  for a file hold other things for a folder (sections 3.4 and 4.6). Report
  type and creator as absent for folders; only the Finder flags are
  meaningful.

### 2.6 Names and paths

Each stored name is exactly **one path component**. Nesting is expressed by
the container structure, never by separator characters inside a name. Classic
Mac names may legally contain `/` and any byte except `:`; real archives
contain 65 names with `/` (for example `Order Form US/CAN`) and 78 names with
a control character (all of them `Icon` followed by CR, 0x0D). A reader must
therefore **never split a stored name on `/`, `:` or `\`**.

A reader delivers, for each entry, the **list of components** (ancestor
folder names, outermost first, then the entry's own name), each available
both as raw stored bytes and as decoded text.

**Decoding (bytes to text):**

- **Classic:** names are **MacRoman** (table below). Real archives contain 59
  non-ASCII classic names.
- **SIT5:** DECISION, in this order: if every byte is below 0x80 the name is
  ASCII; otherwise, if the whole name is well-formed UTF-8, it is UTF-8;
  otherwise it is MacRoman. "Well-formed" is the Unicode definition: no
  overlong encodings, no encodings of U+D800..U+DFFF, nothing above U+10FFFF,
  no truncated sequence. Evidence: all 38 non-ASCII names in the real SIT5
  corpus are well-formed UTF-8 (for example the micro sign as C2 B5, the
  ellipsis as E2 80 A6), including archives whose banner says 1998. No real
  SIT5 name in MacRoman was found, but test archives exist that store
  MacRoman, and the rule decodes both. A MacRoman name whose bytes happen to
  form well-formed UTF-8 is misread by this rule; that ambiguity cannot be
  resolved from the archive.
- **SIT5 comments** are decoded with the SIT5 name rule (DECISION; the one real
  comment is ASCII).
- No Unicode normalisation is applied: the decoded code points are delivered
  as they are.

**Safe component (text to a component usable in a joined path).** When a
reader joins components into one path string it uses `/` as the separator
and first transforms each decoded component by exactly these steps, in this
order (DECISION):

1. Replace every `/` (U+002F) with `:` (U+003A).
2. Replace every U+0000 with `_` (U+005F).
3. If the result is empty, it becomes `_`.
4. If the result is exactly `.`, it becomes `_`. If it is exactly `..`, it
   becomes `__`.

Nothing else is changed: `\`, `:` and control characters other than U+0000
(such as the CR in `Icon` CR) are kept. A joined path built this way never
begins with `/`, never contains an empty, `.` or `..` component, and has
exactly one component per folder level. A consumer that writes to a file
system applies that file system's further restrictions itself.

Two entries may end up with the same path (two siblings with the same stored
name, which the format does not prevent, or names that differ only in
characters replaced above). The reader lists both, in order, unchanged; it
does not rename or merge. No duplicate sibling names occur in the corpus.

MacRoman maps bytes 0x00..0x7F to the same ASCII code points. Bytes
0x80..0xFF map to these Unicode code points (hexadecimal; row = high nibble,
column = low nibble):

| | 0 | 1 | 2 | 3 | 4 | 5 | 6 | 7 | 8 | 9 | A | B | C | D | E | F |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| 8x | 00C4 | 00C5 | 00C7 | 00C9 | 00D1 | 00D6 | 00DC | 00E1 | 00E0 | 00E2 | 00E4 | 00E3 | 00E5 | 00E7 | 00E9 | 00E8 |
| 9x | 00EA | 00EB | 00ED | 00EC | 00EE | 00EF | 00F1 | 00F3 | 00F2 | 00F4 | 00F6 | 00F5 | 00FA | 00F9 | 00FB | 00FC |
| Ax | 2020 | 00B0 | 00A2 | 00A3 | 00A7 | 2022 | 00B6 | 00DF | 00AE | 00A9 | 2122 | 00B4 | 00A8 | 2260 | 00C6 | 00D8 |
| Bx | 221E | 00B1 | 2264 | 2265 | 00A5 | 00B5 | 2202 | 2211 | 220F | 03C0 | 222B | 00AA | 00BA | 03A9 | 00E6 | 00F8 |
| Cx | 00BF | 00A1 | 00AC | 221A | 0192 | 2248 | 2206 | 00AB | 00BB | 2026 | 00A0 | 00C0 | 00C3 | 00D5 | 0152 | 0153 |
| Dx | 2013 | 2014 | 201C | 201D | 2018 | 2019 | 00F7 | 25CA | 00FF | 0178 | 2044 | 20AC | 2039 | 203A | FB01 | FB02 |
| Ex | 2021 | 00B7 | 201A | 201E | 2030 | 00C2 | 00CA | 00C1 | 00CB | 00C8 | 00CD | 00CE | 00CF | 00CC | 00D3 | 00D4 |
| Fx | F8FF | 00D2 | 00DA | 00DB | 00D9 | 0131 | 02C6 | 02DC | 00AF | 02D8 | 02D9 | 02DA | 00B8 | 02DD | 02DB | 02C7 |

(0xDB is the euro sign in the current mapping and was the currency sign
U+00A4 before Mac OS 8.5; 0xF0 is the Apple logo, a private-use code point.)

## 3. Classic container ("SIT!" family, StuffIt 1.x to 4.x)

### 3.1 Overall layout

    archive header (22 bytes)
    record, record, record, ... up to the declared archive length

Each **record** begins with a fixed 112-byte **record header**. There are
three kinds of record, told apart by the method bytes (section 3.3):

| Kind | Bytes after the 112-byte header |
|---|---|
| File | The packed resource fork, immediately followed by the packed data fork. |
| Folder start marker | Nothing. |
| Folder end marker | Nothing. |

Records are strictly sequential: the next record header starts at the byte
after the current record. Because of this no two records, and no two forks,
can share or overlap bytes. Folder nesting is expressed by bracketing: all
records between a folder start marker and its matching end marker are inside
that folder.

### 3.2 Archive header (22 bytes)

Archive offsets.

| Offset | Size | Field |
|---|---|---|
| 0 | 4 | Signature. ASCII `SIT!` in every real archive. BLACK-BOX: the independent tool also accepts, as the same format, `STin`, `STi` followed by one ASCII digit, and `ST` followed by two ASCII digits (tried: `STi0`..`STi5`, `STi9`, `ST40`, `ST45`, `ST46`, `ST47`, `ST50`, `ST51`, `ST55`, `ST60`, `ST61`, `ST65`, `ST66`, `ST99`), in every case only when the second signature is present. |
| 4 | 2 | Number of **top-level** records that are files or folder start markers (records nested in folders and end markers are not counted). Correct in every real archive. Informational; never used to stop reading. |
| 6 | 4 | Declared archive length in bytes, counted from offset 0. Equals the size of the archive in every real archive that is not wrapped; for archives carried inside a MacBinary wrapper it is smaller than the bytes that follow the archive start (the wrapper pads). |
| 10 | 4 | Second signature, ASCII `rLau`. Present in every real archive; required by the independent tool (BLACK-BOX). Absent (zero) in a hand-built test archive shipped with the reference implementation. |
| 14 | 1 | Format version. Observed 1 and 2. No layout difference between them was found; the version 1 archives use method 2, version 2 archives mostly method 13. Ignore. |
| 15 | 1 | Unknown; varies widely. Ignore. |
| 16 | 4 | 22 in every real archive; 0 in the hand-built test archive. INFERRED meaning: length of this header. A reader ignores it and uses the constant 22. |
| 20 | 2 | Unknown; non-zero in most real archives. It is not the CRC-16 of any contiguous range of the header. Ignore. |

### 3.3 The two method bytes

Bytes 0 and 1 of the record header are the resource fork's and the data
fork's method byte. Each is a bit field:

| Bits | Mask | Meaning |
|---|---|---|
| 7 | 0x80 | On a file: this fork is encrypted (section 3.7). Meaningless on a marker. |
| 6 | 0x40 | Never set in a real archive. Ignored. |
| 5 | 0x20 | Never set on a real file. It is set in the two marker values below. |
| 4 | 0x10 | On a folder start marker: the folder contains encrypted files (set on all 9 folders of the real password-protected archive, on no other folder). Never set on a real file. Ignored. |
| 3..0 | 0x0F | On a file: compression method number (section 2.3). |

Record kind. For each of the two method bytes compute its **kind value**: the
byte with bits 0x80 and 0x10 cleared.

1. If **either** kind value is exactly 0x20, the record is a **folder start
   marker**.
2. Otherwise, if either kind value is exactly 0x21, the record is a **folder
   end marker**.
3. Otherwise the record is a **file**. Each fork's method number is its
   method byte's low 4 bits; each fork's encrypted flag is its bit 0x80. Bits
   0x40 and 0x20 are ignored (for example 0x2D is a file fork with method 13,
   0x60 a file fork with method 0); if either is set, record a finding.

Rule 1 is tested before rule 2 on purpose: a record with 0x20 in one byte and
0x21 in the other is a start marker. In every real archive both method bytes
of a marker are equal (0x20/0x20, 0x30/0x30 or 0x21/0x21). The treatment of
all other combinations above is what both the reference implementation and
the independent tool do (BLACK-BOX for the latter: a marker value in only one
byte is honoured; start wins over end in either order; bit 0x80 on a start
marker changes nothing; 0x2D, 0x22, 0x4D, 0x60, 0x28 are files).

### 3.4 Record header (112 bytes)

Structure offsets (from the first byte of the record header). "S", "F" and
"E" say for which kinds a field is meaningful (folder Start marker, File,
folder End marker). A field is to be ignored for the kinds not named.

| Offset | Size | Kinds | Field |
|---|---|---|---|
| 0 | 1 | S F E | Resource fork method byte (section 3.3). |
| 1 | 1 | S F E | Data fork method byte. |
| 2 | 1 | S F | Name length in bytes, 0..31. DECISION: a value above 31 is treated as 31, with a finding (both existing readers do this). |
| 3 | 31 | S F | Name bytes (MacRoman). Only the first "name length" bytes are the name. The remainder is padding and usually **contains garbage** rather than zeros (859 of 982 real start and file headers). In end markers the length and name are stale copies and must be ignored. |
| 34 | 2 | S F | Name checksum: CRC-16 of header bytes 2..33 (the length byte and all 31 name bytes, padding included), always this range whatever the name length. Correct in every real start and file record; **wrong in every real end marker**. DECISION: a mismatch in a start or file record is a finding only (neither existing reader checks it). |
| 36 | 2 | | Zero, except 0x0986 in 10 real headers (two archives). Ignore. |
| 38 | 8 | S | Four 16-bit values; non-zero in most real start markers. INFERRED: the folder's Finder window rectangle (top, left, bottom, right). Zero for files. Ignore. |
| 46 | 2 | | Zero. Ignore. |
| 48 | 2 | S | Number of direct children of the folder (files and folder start markers directly inside it). Correct in every real folder. Zero in all but 2 real file records. Informational; not used for reading. |
| 50 | 4 | S F E | Archive offset of the **previous sibling** record header. For the first child of a folder: the offset of the folder's start marker. For the first top-level record: 0. For an end marker: the offset of the folder's last child. |
| 54 | 4 | S F | Archive offset of the **next sibling** record header. For the last child of a folder: the offset of the folder's end marker. For the last top-level record: 0. Zero in end markers. |
| 58 | 4 | S F E | Archive offset of the **enclosing folder's start marker**, 0 at top level. For an end marker: the offset of its own matching start marker. |
| 62 | 4 | S | Archive offset of the folder's first child; always this header's offset + 112 in real archives. For files this is **not** an offset (0, 0xFFFFFFFF, or other values). |
| 66 | 4 | F | File type. For a start marker these 4 bytes are not a type: bytes 66..67 are zero and bytes 68..69 hold a small value such as 0x0100, 0x0107, 0x0120 or 0x0200 (INFERRED: the folder's Finder view setting). Ignore for S. |
| 70 | 4 | F | Creator. For a start marker these 4 bytes are not a creator: they are FF FF FF FF, zero, or two small 16-bit numbers (INFERRED: the folder's icon position). Ignore for S. |
| 74 | 2 | S F | Finder flags. |
| 76 | 4 | S F | Creation time (section 2.4). |
| 80 | 4 | S F | Modification time. |
| 84 | 4 | F | Resource fork unpacked length. Zero in real start markers. |
| 88 | 4 | S F | File: data fork unpacked length. Start marker: sum of the unpacked lengths of both forks of every file anywhere inside the folder, modulo 2^32 (correct in every real folder). Informational. |
| 92 | 4 | F | Resource fork packed length. Zero in real start markers. |
| 96 | 4 | S F | File: data fork packed length. Start marker: number of bytes from the end of this header up to and including the matching end marker's header (correct in every real folder), which allows a whole folder to be skipped. Informational. |
| 100 | 2 | F | Resource fork checksum (section 2.2). Meaningless in markers (often non-zero). |
| 102 | 2 | F | Data fork checksum. |
| 104 | 1 | F | Zero in every unencrypted real file. When the resource fork is encrypted: INFERRED to be the number of padding bytes, 0..7 (section 3.7). Ignore. |
| 105 | 1 | F | The same for the data fork. |
| 106 | 4 | F | Zero unless a fork is encrypted. In the real password-protected archive bytes 108..109 are 0x0004 when the resource fork is encrypted and bytes 106..107 are 0x0004 when the data fork is encrypted. Meaning unknown. Ignore. |
| 110 | 2 | S F E | **Header checksum:** CRC-16 of header bytes 0..109. Correct in all 1,050 real headers, end markers included. |

End markers carry stale copies of other fields (name, dates, lengths); only
the fields marked E are meaningful. For both kinds of marker the four length
fields at 84..99 must **not** be used to find the next record: no fork bytes
follow a marker regardless of what those fields contain.

### 3.5 Fork locations

For a file record whose header starts at archive offset H, with resource
fork packed length R and data fork packed length D:

- the packed resource fork occupies archive offsets H+112 .. H+112+R-1
  (nothing if R = 0);
- the packed data fork occupies the next D bytes, starting at H+112+R
  (nothing if D = 0);
- the next record header starts at H+112+R+D.

This holds for encrypted forks too: R and D include every byte belonging to
the fork (section 3.7), confirmed by walking all 147 file records of the real
password-protected archive.

### 3.6 Reading procedure

Terms. **END** is the declared archive length, or L if L is smaller. END
bounds the **record headers**: a record header is read only where all 112 of
its bytes lie before END. It does not bound fork data, which is bounded by L
(step 7). The
**enclosing folders** of a record are the folders whose start marker has been
read and whose end marker has not, outermost first; this list is empty at the
start. The **nesting depth** of a record is the number of its enclosing
folders plus one.

Records are read one after another, the first at archive offset 22. For the
record at archive offset H:

1. If END - H is 0, the list is complete. If END - H is 1..111, the list is
   complete and the leftover bytes are recorded as a finding ("trailing
   bytes"); they are not an error (BLACK-BOX: accepted silently by the
   independent tool; no real archive has any).
2. Read the 112-byte header at H and verify the header checksum at 110. On a
   mismatch the listing stops and is **incomplete**: nothing after a damaged
   header can be located, because record positions depend on every earlier
   record.
3. If all 112 bytes are zero, the list is complete; record a finding ("zero
   padding"). DECISION: such a header has a valid checksum and would otherwise be
   read as a file with an empty name and empty forks; it is treated as
   padding, never as an entry.
4. Determine the record kind (section 3.3).
5. **Folder start marker.** If its nesting depth exceeds the reader's depth
   limit (section 6), the listing stops and is incomplete. Otherwise list a
   folder entry whose components are the names of the enclosing folders
   followed by this name, with Finder flags and dates from the header. The
   folder joins the enclosing folders. The next record is at H + 112.
6. **Folder end marker.** The innermost enclosing folder is closed (removed
   from the enclosing folders). If there is none, the marker is unbalanced:
   record a finding and otherwise ignore it. The next record is at H + 112.
7. **File.** List a file entry whose components are the names of the
   enclosing folders followed by this name, with type, creator, Finder flags,
   dates, and for each fork: method number, encrypted flag, packed location,
   packed length, unpacked length and checksum. Compute the fork locations
   (section 3.5). If H+112+R+D is not greater than END, the next record is
   at H+112+R+D. If it is greater than END, the entry is still listed, with
   its fork locations and lengths exactly as computed, and the listing stops
   and is **incomplete**, with a finding ("record extends beyond the archive
   length"): continuing is impossible because the next record would lie
   beyond END. Whether each fork of that last entry can be delivered is then
   decided by section 2.2 like any other fork, against **L**, not against
   END: a fork whose range lies entirely within L is delivered normally (its
   fork checksum protects it), and a fork whose range does not gets the fork
   error "truncated" (step 4 of section 2.2). No fork error is attached at
   listing time and the listing needs no per-fork error for this. The two
   cases differ only when the declared archive length is smaller than L:
   - declared length at least L (so END = L): the record runs past the
     available bytes, and at least one of its forks is "truncated";
   - declared length below L, record end above the declared length but not
     above L: the declared length contradicts the records. Both forks are
     within L and are delivered if they pass their checks; the listing is
     nevertheless incomplete, because nothing after this record is read.

   (Both existing readers behave this way: they use the declared length only
   to decide whether another record header follows, and take fork bytes from
   whatever is available. BLACK-BOX with a two-file archive whose declared
   length ends inside the first file's fork data: the independent tool
   extracts the first file with a matching checksum and does not list the
   second. An earlier draft of this step tested the forks against END; that
   was stricter than either reader and is withdrawn. No real archive has a
   record that crosses its declared length.)

When the list is complete (steps 1 and 3):

- if any enclosing folder is still open, record a finding ("unterminated
  folder"); the entries stand;
- if the declared archive length is greater than L, the listing is
  **incomplete** (the archive is truncated or is one part of a multi-part
  archive), even when the last available record ended exactly at L.

A name of length 0 on a file or start marker whose header is not all zero is
valid: the entry is listed with an empty name, which section 2.6 turns into
`_` in a joined path. (BLACK-BOX: the independent tool lists such records.)
An **empty folder** is a start marker immediately followed by its end marker
(BLACK-BOX; none occurs in the corpus).

The resulting order is the stored order: a folder, then its contents in
stored order (depth first), then whatever follows its end marker. The
sibling, parent and first-child offsets at 50..65, the child count at 48 and
the totals at 88 and 96 of a start marker are redundant with this bracketing
and are not used for reading. They are correct in every real record and may
serve as a consistency check that produces findings.

### 3.7 Encryption

A file fork is **encrypted** when its method byte has bit 0x80 set and its
packed length is not 0. (A fork with bit 0x80 and packed length 0 is an
ordinary empty fork; real encrypted archives give empty forks the method byte
0x00.)

Layout of an encrypted fork, from the real password-protected archive (289
encrypted forks) and BLACK-BOX:

- The packed length field counts the whole stored region of the fork, so the
  fork locations of section 3.5 and the position of the next record are
  unaffected by encryption.
- The **last 16 bytes** of the region are per-fork key material, not fork
  data. The bytes before them are the encrypted packed stream; their number
  (packed length - 16) is a multiple of 8.
- INFERRED: header byte 104 (resource fork) or 105 (data fork) is the number
  of padding bytes, 0..7, that were appended to the packed stream to reach a
  multiple of 8 before encryption. Evidence: the one real encrypted method 0
  fork has unpacked length 30, the value 2 in byte 105 and packed length 48
  (30 + 2 + 16); the method 13 forks have the values 3 or 7; the bytes are
  zero for forks that are not encrypted. A reader that does not decrypt does
  not need them.
- The unpacked length field is the true unpacked length, as for any fork.
- The method number in the low 4 bits is the compression applied before
  encryption.

DECISION: an encrypted fork whose packed length is below 16, or whose packed
length is not a multiple of 8, gets the fork error "corrupt" instead of
"encrypted" (BLACK-BOX: the independent tool reports corrupt data when the
length is below 16).

This test belongs to the **classic record parser and is made when the file
is listed**, because it is a rule of the classic container only (SIT5 keeps
its key material outside the packed length, section 4.10, and has no such
rule). For each fork of a file record the parser settles the encryption
state of section 2.2:

| Method byte bit 0x80 | Packed length P | Encryption state |
|---|---|---|
| clear | any | not encrypted |
| set | 0 | not encrypted (an ordinary empty or corrupt fork) |
| set | 16 or more, and a multiple of 8 | encrypted |
| set | 1..15, or not a multiple of 8 | encrypted with a malformed packed length |

The listing records that state with the fork. A listing that shows whether a
fork is encrypted shows "encrypted" for both of the last two rows; the
difference appears only as the fork error given when the fork is read
(section 2.2, step 2: "encrypted, unsupported" for the third row, "corrupt"
for the fourth). Step 1 of section 2.2 still comes first: a fork with U = 0
is empty whatever its state.

**No finding for the malformed state** (DECISION). Settling the state "encrypted
with a malformed packed length" records **no finding** when the file is
listed. The rule of this document is that a header condition which already
has an outcome of its own when the fork is read (here the fork error
"corrupt"; likewise P = 0 with U not 0, a packed range beyond the available
bytes as far as the fork is concerned, or an unimplemented method number) is
reported through that outcome and not a second time as a finding; a finding
is for a condition that would otherwise leave no trace, such as the ignored
packed bytes of step 1. The state itself is the trace: it must travel with
the listed fork, so that step 2 of section 2.2 can give the right error
without looking at the record again. A finding is not a substitute for
carrying the state, and a reader must not derive the fork error from the
presence of a finding.

When such a fork also has U = 0 (bit 0x80 set, U = 0, P in 1..15 or not a
multiple of 8), the table still gives the malformed state, step 1 makes the
fork empty, and reading it succeeds with no contents and no error. Listing
it records exactly **one** finding for that fork, the one of step 1 ("U is 0
and P is not 0", section 2.2); the malformed length is not noted separately,
and in this case it never becomes visible at all, since step 2 is not
reached. The same holds for a fork in the state "encrypted" with U = 0: one
finding, that of step 1, and empty contents.

The cipher, the derivation of keys from the password, and where the archive
keeps anything needed to check a password are **not known** to this
specification. A reader lists encrypted archives normally, reports each
encrypted fork with the fork error "encrypted, unsupported", and never feeds
such a fork's bytes to a decoder. A folder start marker with bit 0x10 may
be reported as "contains encrypted files"; it changes nothing else.

## 4. StuffIt 5 container

### 4.1 Overall layout

    file header (114 bytes)
    entry structures, linked by archive offsets

Unlike the classic container, SIT5 entries form a linked tree: every entry
header holds the archive offsets of its previous sibling, next sibling and
parent, and a folder holds the archive offset of its first child.

### 4.2 File header (114 bytes)

Archive offsets.

| Offset | Size | Field |
|---|---|---|
| 0 | 80 | Banner text, ASCII: `StuffIt (c)1997-` (16 bytes), a 4-digit year (observed `1998`, `2001`, `2002`), then ` Aladdin Systems, Inc., http://www.aladdinsys.com/StuffIt/` (58 bytes, note the leading space), then CR LF (0x0D 0x0A). |
| 80 | 2 | 0x1A 0x00. |
| 82 | 1 | Container version. Always 5. Any other value: not this format. |
| 83 | 1 | Archive flags. 0x10 in every real archive. See section 4.3. Other bits unknown; ignore. |
| 84 | 4 | Declared archive length in bytes. Equals the size of the archive in every real archive that is not wrapped. See section 4.3 for its use. |
| 88 | 4 | Archive offset of the first top-level entry (114 in every real archive). |
| 92 | 2 | Number of top-level entries. |
| 94 | 4 | Archive offset of the first top-level entry, second copy. Equal to the field at 88 in every real archive. |
| 98 | 2 | **File header checksum:** CRC-16 of bytes 0..113 computed with bytes 98 and 99 taken as zero. Correct in every real archive. |
| 100 | 14 | Constant filler: 0x0D, 0xA5, 0xA5, ASCII `Reserved`, 0xA5, 0xA5, 0x00. |

### 4.3 Archive length, first entry, offset obfuscation

**Archive length.** DECISION: every bounds check in the SIT5 container is made
against **L**, the bytes actually available. The declared length at offset 84
is informational only: if it differs from L, record a finding (declared
greater than L suggests truncation; declared smaller than L is normal for an
archive followed by wrapper padding). It may be 0 or any other value without
further consequence, and entries or forks lying beyond the declared length
but within L are valid. (BLACK-BOX: the independent tool lists the same
entries whatever the field contains; the reference implementation does not
read it. This differs from the classic container, where the declared length
is the only thing that ends the record sequence.)

**Entry offsets.** DECISION: an entry or marker can start only at an archive
offset X with X at least 114 (the file header is 114 bytes long and nothing
may overlap it) and X + 48 not greater than L.

**Obfuscation.** In all real archives archive-flag bit 0x10 is set and both
first-entry fields hold the plain offset 114. REFERENCE-ONLY, never observed:
when bit 0x10 is **clear**, the reference implementation takes the field at
88 and every entry's parent offset (entry header structure offset 26) as
stored XORed with 0xA5A5A5A5; it does not transform previous, next or
first-child offsets, and it never reads the copy at 94. BLACK-BOX: the
independent tool knows no such transformation: it takes the first-entry
offset from the copy at 94 only, in plain form, whatever bit 0x10 says.

Because the two readers disagree and no real archive decides the matter, the
first entry is located by validation (DECISION):

1. If the top-level entry count (offset 92) is 0, the archive has no entries,
   even if a valid entry exists at offset 114 (BLACK-BOX agrees).
2. Form the candidates in this order: the field at 94; the field at 88; then,
   only if archive-flag bit 0x10 is clear, the field at 88 XOR 0xA5A5A5A5 and
   the field at 94 XOR 0xA5A5A5A5; finally the constant 114.
3. The first-entry offset is the first candidate that is a valid entry
   offset (above), at which the 4 bytes A5 A5 A5 A5 are found, and whose
   entry header passes every check of section 4.5 ("Validation").
4. If no candidate qualifies, the archive is not listed: for detection this
   means "not this format"; for an archive already detected otherwise it
   means an empty, incomplete listing.

The stored **parent offset** of an entry is never needed to build paths: the
tree of section 4.9 determines every entry's parent. It is a cross-check
only: it should equal the offset of the folder whose chain the entry is in
(0 at top level), either directly or after XOR with 0xA5A5A5A5; anything
else is a finding.

### 4.4 Entry structure

An entry consists of, in order:

1. the **entry header**: 48 fixed bytes, then (encrypted files only) key
   bytes, then the name, then an optional comment; its total size is the
   "header size" field;
2. the **second block**: Finder information, 36 or 32 bytes;
3. for files whose second block announces a resource fork: the 14-byte
   **resource fork sub-block**, then (encrypted files only) key bytes;
4. for files only: the packed resource fork, then the packed data fork.

An **end-of-folder marker** (section 4.7) consists of a 48-byte entry header
alone.

### 4.5 Entry header

Structure offsets (from the entry's first byte; E denotes the entry's archive
offset).

| Offset | Size | Field |
|---|---|---|
| 0 | 4 | Entry identifier, always A5 A5 A5 A5. |
| 4 | 1 | Entry version. 1 in every real entry and marker. Selects the second block size (section 4.6). Any value is accepted. |
| 5 | 1 | Zero in every real entry. Ignore. |
| 6 | 2 | **Header size** HS: number of bytes from E to the end of the entry header (fixed part + key + name + comment). The second block starts at E + HS. |
| 8 | 1 | Zero in every real entry. Ignore. |
| 9 | 1 | Entry flags (below). |
| 10 | 4 | Creation time (section 2.4). In markers: zero or not; ignore. |
| 14 | 4 | Modification time. In markers: ignore. |
| 18 | 4 | **Previous** offset: the previous sibling's entry offset; for the first child of a folder, the folder's own offset; 0 for the first top-level entry. For a marker: the folder's last child, or the folder itself if it has no children. |
| 22 | 4 | **Next** offset: the next sibling's entry offset; for the last child of a folder, the offset of the folder's end-of-folder marker; 0 for the last top-level entry and for markers. |
| 26 | 4 | **Parent** offset: the enclosing folder's entry offset, 0 at top level (section 4.3). For a marker: the folder it terminates. |
| 30 | 2 | Name length N in bytes (0 in markers). |
| 32 | 2 | **Header checksum:** CRC-16 of the HS bytes E..E+HS-1 computed with bytes 32 and 33 taken as zero. Correct in every real entry and marker. |
| 34 | 4 | File: data fork unpacked length. Folder: archive offset of the folder's first child. Marker: 0xFFFFFFFF. |
| 38 | 4 | File: data fork packed length. Folder: sum of the unpacked lengths of all forks of all files anywhere inside the folder, modulo 2^32 (correct in every real folder). Marker: 0. |
| 42 | 2 | File: data fork checksum (section 2.2). Folder: unknown; ignore. |
| 44 | 2 | File: zero in every real file; ignore. Folder: unknown; ignore. |
| 46 | 1 | File: data fork method number. |
| 47 | 1 | File: data fork key length byte (0 in every real file). |
| 46 | 2 | Folder (instead of the two one-byte fields above): number of direct children, **not counting** the end-of-folder marker (correct in every real folder). |
| 48 | K | Key bytes of the data fork; K is defined below and is 0 for every unencrypted entry. |
| 48+K | N | Name bytes (section 2.6). |
| 48+K+N | 4+C | **Comment**, present only if HS is greater than 48+K+N: a 16-bit comment length C, 2 bytes that are zero in the one real comment (ignore), then C comment bytes. |

**The key length K.** K is the byte at structure offset 47 **if the entry is
a file and entry flag 0x20 is set**; otherwise K is 0. In particular K is 0
for every folder and marker (their bytes 46..47 are the child count), and K
is 0 for a file without flag 0x20 whatever byte 47 contains (DECISION; if byte 47
is non-zero in that case, record a finding. The reference implementation
never skips data fork key bytes; BLACK-BOX: the independent tool refuses such
an archive). The same rule defines KR in section 4.6.

So HS = 48 + K + N when there is no comment, and 48 + K + N + 4 + C when
there is one. The second block is always found at E + HS.

Entry flags (structure offset 9):

| Mask | Meaning |
|---|---|
| 0x40 | Entry is a **folder**, or an end-of-folder marker (section 4.7). |
| 0x20 | Entry is **encrypted** (BLACK-BOX; section 4.10). Not set on any real entry. Has no effect on a folder or marker (DECISION). |
| 0x10 | Set on 81 of 872 real files; meaning unknown. Ignore. |
| 0x08 | Set on the one real entry that has a comment (a folder). Not required for a comment to be present (BLACK-BOX); the presence of a comment is decided by HS alone. Ignore. |
| 0x80, 0x04, 0x02, 0x01 | Not set on any real entry in the corpus; meaning unknown. Ignore. |

**Validation** (DECISION). An archive offset X holds a usable entry header only if
all of the following are true; otherwise X is an **unusable offset**:

1. X is a valid entry offset (section 4.3) and the bytes at X are
   A5 A5 A5 A5.
2. HS is at least 48 and X + HS is not greater than L.
3. If the header is an end-of-folder marker by the test of section 4.7:
   HS is exactly 48. Checks 4 to 7 do not apply to a marker. (The header
   checksum rule below does apply to it.)
4. If flag 0x40 is set and the header is not a marker, it must not be of an
   ambiguous form (section 4.7).
5. Let A = HS - (48 + K + N). A must be 0, or at least 4. (A below 0: the
   name does not fit. A of 1, 2 or 3: there is room for neither nothing nor a
   comment; BLACK-BOX: the independent tool also fails on this.)
6. If A is at least 4, the comment length C must not exceed A - 4. If C is
   smaller than A - 4 the surplus bytes after the comment are ignored and a
   finding is recorded.
7. For an entry that is not a marker, the second block, and the resource
   fork sub-block and its key bytes when present (section 4.6), lie entirely
   within L.

Not validated: the bytes described as zero, the entry version, unknown flag
bits. A file with N = 0 is valid and has an empty name (BLACK-BOX: listed by
the independent tool). The header checksum is verified when the header is
usable; a mismatch is a finding, not a reason to reject (section 4.11
explains why), unless the caller asked for strict checking, in which case a
mismatch makes the offset unusable.

The header checksum rule covers **end-of-folder markers exactly as it
covers entries**: a marker's checksum is the CRC-16 of its 48 bytes with
bytes 32 and 33 taken as zero, it is correct in all 107 real markers, a
mismatch is a finding, and under strict checking a marker with a wrong
header checksum is an unusable offset. A chain that reaches such a marker
under strict checking therefore ends in a "damaged link" (section 4.9), not
in a normal end. (Archives written by the reference implementation contain
no markers, so nothing is lost by this.)

### 4.6 Second block and resource fork sub-block

The second block starts at E + HS. Its size B depends on the entry version:
**36 bytes when the entry version is 1, 32 bytes for any other version,
including 0.** (Only version 1 occurs in real archives. The other size is
stated by the reference implementation and confirmed BLACK-BOX for versions
0, 2 and 3.) The difference is 4 fewer bytes at the end.

Structure offsets (from the first byte of the second block).

| Offset | Size | Field |
|---|---|---|
| 0 | 2 | Second-block flags. Bit 0x0001: the file **has a resource fork sub-block**. Bit 0x0010: set on 29 of the 31 real folders whose marker directly follows the second block (section 4.7) and on no other real entry; meaning unknown; ignore. No other bit is set in any real entry; ignore them. |
| 2 | 2 | **Second-block checksum:** CRC-16 of the B bytes of the second block followed by the 14 bytes of the resource fork sub-block when one is present, computed with these two bytes taken as zero. That is 36 or 50 bytes for entry version 1 (correct in every real entry) and 32 or 46 bytes for other versions (INFERRED by analogy). |
| 4 | 4 | File: type. Folder: not a type, see below. |
| 8 | 4 | File: creator. Folder: not a creator, see below. |
| 12 | 2 | Finder flags (files and folders). |
| 14 | B - 14 | Remaining Finder information (22 or 18 bytes). Ignore. INFERRED layout for files: icon position (two 16-bit values), a 16-bit folder identifier, then extended Finder information. |

For a **folder**, structure offsets 4..11 are INFERRED to be the folder's
window rectangle (four 16-bit values: top, left, bottom, right; non-zero in
90 of 107 real folders), and the bytes after the Finder flags hold the icon
position, a view setting and scroll position. Report none of it.

If and only if the entry is a **file** and second-block flag bit 0x0001 is
set, the **resource fork sub-block** (14 bytes) follows immediately.
Structure offsets from its first byte:

| Offset | Size | Field |
|---|---|---|
| 0 | 4 | Resource fork unpacked length. |
| 4 | 4 | Resource fork packed length. |
| 8 | 2 | Resource fork checksum (section 2.2). |
| 10 | 2 | Zero in every real entry. Ignore. |
| 12 | 1 | Resource fork method number. |
| 13 | 1 | Resource fork key length byte. |
| 14 | KR | Key bytes of the resource fork. KR is the byte at offset 13 if entry flag 0x20 is set, otherwise 0 (same DECISION as K in section 4.5). |

The resource fork key length byte (offset 13) is 0 in every real sub-block
(589 examined). As for byte 47 of the entry header: if it is non-zero while
entry flag 0x20 is clear, KR is 0 and a finding is recorded (DECISION; one
finding for each of the two bytes that is non-zero).

- A file without bit 0x0001 has no resource fork: its resource fork is empty.
- A file may have the sub-block with both resource lengths 0 (BLACK-BOX:
  accepted); its resource fork is empty.
- DECISION: for a **folder**, bit 0x0001 is ignored and no sub-block is read;
  record a finding. The second-block checksum of a folder therefore always
  covers the B bytes of the second block alone (36 or 32), whatever bit
  0x0001 says: for the purposes of that checksum a sub-block is "present"
  only for a file with the bit set. (No real folder has it. The reference implementation
  ignores it; BLACK-BOX: the independent tool reads 14 more bytes. Because
  the reader of section 4.9 finds entries by links, the disagreement does not
  affect which entries are listed.)
- A second-block checksum mismatch is a finding (section 4.11). Whether the
  checksum of an encrypted file also covers the KR key bytes is unknown, so
  for entries with flag 0x20 the second-block checksum is not verified, not
  even under strict checking (DECISION).

### 4.7 Folders and end-of-folder markers

A **folder entry** has flag 0x40. It has a header and a second block but no
fork data. Its children form a **chain**: the first child is at the
first-child offset (header structure offset 34), and each child's next offset
leads to the following child.

The chain of a folder is terminated by an **end-of-folder marker**. In every
one of the 107 real markers (one per real folder):

- the marker is 48 bytes: HS = 48, flag byte 0x40, name length 0, structure
  offset 34 holds 0xFFFFFFFF, structure offset 38 holds 0, the header
  checksum is correct, the entry version is 1, and there is **no second
  block**;
- its parent offset is the folder; its previous offset is the folder's last
  child (the folder itself when the folder is empty); its next offset is 0;
- it is not an entry and is not counted in the folder's child count.

**Marker test** (DECISION). A header is an end-of-folder marker if and only if
**all three** hold: flag 0x40 is set, the name length is 0, and structure
offset 34 holds 0xFFFFFFFF. A header with flag 0x40 for which **exactly one**
of "name length is 0" and "offset 34 holds 0xFFFFFFFF" holds is of an
**ambiguous form** and is unusable (section 4.5, check 4). Reason: the two
existing readers disagree on such headers and therefore on whether a second
block follows (the reference implementation decides by the empty name alone;
BLACK-BOX: the independent tool decides by 0xFFFFFFFF alone, listing a folder
with an empty name and failing on a named folder whose offset 34 is
0xFFFFFFFF). No real header has an ambiguous form. A folder that is not a
marker therefore always has a non-empty name.

Where the marker is stored varies between real archives:

- **Trailing layout** (76 of the 105 real non-empty folders): folder header,
  second block, children (each with everything inside it), marker. The
  first-child offset equals the offset just after the folder's second block.
  The last child's next offset points forward to the marker.
- **Leading layout** (29 of 105): folder header, second block, marker (48
  bytes), children. The first-child offset is the offset just after the
  marker, and the last child's next offset points **backwards** to the marker.

**Empty folders** (2 real ones): the child count is 0; the marker follows the
folder's second block directly; the **first-child offset is the offset of
that marker**; the marker's previous offset and parent offset are both the
folder; the folder's next offset is the offset just after the marker. One of
the two has second-block flag 0x0010, the other does not. Archives written by
the reference implementation store an empty folder differently: child count
0, first-child offset 0, no marker (BLACK-BOX: also accepted by the
independent tool). Both shapes are covered by section 4.9: a chain that
starts at 0 or at a marker has no members. The first-child offset of a folder
is never 0xFFFFFFFF in a valid archive (that value would make the header a
marker or ambiguous).

There is no marker at the top level: the top-level chain ends with a next
offset of 0. In archives written by the reference implementation there are no
markers at all: every chain ends with a next offset of 0, and the previous
offset of a first child is 0 rather than the folder.

### 4.8 Fork locations

For a file entry at archive offset E with header size HS, second block size B
(36 or 32), and sub-block size S (0 if the file has no resource fork
sub-block; otherwise 14 + KR), resource fork packed length R (0 without a
sub-block) and data fork packed length D:

- the packed resource fork occupies R bytes starting at E + HS + B + S;
- the packed data fork occupies the next D bytes, starting at
  E + HS + B + S + R;
- the entry's **extent** is HS + B + S + R + D bytes starting at E. For a
  folder the extent is HS + B bytes.

The resource fork comes first, exactly as in the classic container.

A fork whose range is not entirely within L gets the fork error "truncated";
the entry is still listed and listing continues with the entry's next offset
(DECISION). (BLACK-BOX: the independent tool also lists such an entry and fails
only when the fork is read.)

In every real archive the entries and markers are stored without gaps or
overlaps: each non-last sibling's next offset equals the offset just after
its extent (for a folder: just after everything inside it and its marker),
and the last entry ends exactly at the declared archive length.

### 4.9 The entry tree and the order of the list

The entry list is defined by links, with the counts as a cross-check.

**Chains.** A chain has a **start offset** and an **expected count**. The
top-level chain starts at the first-entry offset (section 4.3) and its
expected count is the top-level entry count of the file header. The chain of
a folder starts at the folder's first-child offset and its expected count is
the folder's child count.

The **members** of a chain are found by examining a sequence of candidate
offsets. The first candidate is the start offset; after a candidate has
become a member, the next candidate is that member's next offset. A candidate
offset X **becomes a member** when all of these hold:

1. the chain has fewer members than its expected count;
2. X is not 0;
3. X holds a usable entry header (section 4.5, "Validation");
4. that header is not an end-of-folder marker;
5. X is not already a member of any chain of this archive (the **visited
   rule**: every entry is listed at most once);
6. adding the entry's extent (section 4.8, with a fork that has the
   "truncated" error counted as 0 bytes) to the extents of all members so far
   does not exceed L - 114 (the **extent rule**, section 6).

The chain ends at the first candidate that does not become a member. How it
ends matters:

| The chain has | The candidate that ended it is | Outcome |
|---|---|---|
| as many members as expected | 0, or a marker | Normal end. |
| fewer members than expected | 0, or a marker | **Incomplete** ("count exceeds chain"). |
| as many members as expected | a usable entry that is not a marker | **Incomplete** ("chain exceeds count"); that entry and whatever follows it in the chain are **not** listed. |
| any number | an unusable offset, or an offset that is already a member | **Incomplete** ("damaged link"). |
| any number | an entry that fails the extent rule | **Incomplete** ("overlapping entries"), and the **whole listing stops** here: no further candidate of any chain is examined. |

More than one row can describe the same candidate (for example a chain that
already has its expected count and whose next candidate is an entry already
listed elsewhere). The outcome is the **first** of the following that
applies, tested in this order (DECISION):

1. The candidate is 0: normal end if the chain has its expected count,
   otherwise "count exceeds chain".
2. The candidate is an unusable offset: "damaged link".
3. The candidate is a marker: normal end if the chain has its expected
   count, otherwise "count exceeds chain".
4. The candidate is already a member of some chain: "damaged link", also
   when the chain has its expected count.
5. The chain has its expected count: "chain exceeds count".
6. The entry fails the extent rule: "overlapping entries".
7. Otherwise the candidate becomes a member.

So a damaged link is reported in preference to a count disagreement, and the
extent rule is applied only to an entry that would otherwise become a
member: a surplus entry beyond the expected count ends its chain as "chain
exceeds count" without stopping the whole listing, however large its extent.
All the outcomes other than the normal end make the listing incomplete; the
order decides only which reason is reported, and whether the whole listing
stops.

A candidate that reaches point 7 is still subject to the path size limit of
section 6, requirement 9, which is tested last: if listing it would exceed
that limit it does not become a member, and the whole listing stops,
incomplete ("path size limit reached"), as for the extent rule.

Except in the two cases that stop the whole listing ("overlapping entries"
and "path size limit reached"), the end of one chain does not affect any
other chain: the chain of the enclosing folder continues with the folder's own next
offset, which does not depend on the folder's children. "Incomplete" marks
the listing as a whole (section 1.2); entries already listed stand.

So the count and the links must **both** vouch for every listed entry, and a
disagreement in either direction has the same consequence. (The top-level
count 0 with entries present therefore gives an empty list, and a top-level
next offset of 0 ends the top-level chain: normally if the count is reached,
as incomplete if not.) Counts and links agree in every real archive and in
the archives written by the reference implementation.

A marker met as a candidate should have a parent offset equal to the folder
whose chain it ends; if it does not, or if a marker ends the top-level chain,
record a finding. This does not change the outcome above. Markers are never
members, so the visited rule does not apply to them.

**Depth.** Top-level entries have depth 1; the children of a folder have the
folder's depth plus 1. If a folder's depth equals the reader's depth limit
(section 6), the folder is listed but its chain is not examined (it is
treated as having no members), and the listing is incomplete ("nesting too
deep"). The folder's own chain continues normally.

**A folder with child count 0** has no members whatever its first-child
offset holds; its chain is not examined at all. If the first-child offset is
neither 0 nor the offset of a marker, record a finding.

**Order.** The entry list is the pre-order, depth-first sequence: the members
of the top-level chain in chain order, each folder being followed
immediately by the members of its own chain (each of those followed by its
own descendants) before the folder's next sibling. The components of an entry
are the names of the folders on the way down to it, outermost first, then its
own name.

**Counts.** The total number of entries is the top-level count plus the sum
of all folder child counts. Markers are in none of these counts.

For every real archive and every available archive written by the reference
implementation this definition gives the same entries with the same paths in
the same order as the independent tool lists, with no finding (checked on all
26 real archives and on 26 written ones). The existing readers do not work
this way; see section 7.

### 4.10 Encryption

No real SIT5 archive in the corpus is encrypted; everything here is
BLACK-BOX.

- Entry flag 0x20 marks an encrypted file. For such a file the data fork key
  length byte (header structure offset 47) and the resource fork key length
  byte (sub-block structure offset 13) give the numbers K and KR of
  key-material bytes stored after the 48 fixed header bytes (before the
  name) and after the resource fork sub-block (before the packed forks).
- The independent tool accepts an encrypted file only when each key length is
  exactly 5; it refuses the whole archive for any other value, including 0.
- A reader **must skip K and KR bytes** as described in sections 4.5, 4.6 and
  4.8 to find the name and the forks, whatever their values, even though it
  cannot decrypt.
- DECISION: when flag 0x20 is set, **every fork of the file with a non-zero
  packed length is encrypted**, regardless of K and KR (also when they are
  0). When flag 0x20 is clear no fork is encrypted and the key length bytes
  are ignored.

The cipher and key derivation are not specified. A reader lists encrypted
entries normally (name, lengths, locations), reports each encrypted fork with
the fork error "encrypted, unsupported", and never feeds such a fork's packed
bytes to a decoder.

### 4.11 Version differences and non-conforming writers

- Container version (file header offset 82) must be 5.
- Entry version 1: 36-byte second block. Any other entry version: 32-byte
  second block. No other difference is known.
- Banner year varies and carries no structural meaning.

Archives produced by the **writer of the reference implementation** are not
what genuine StuffIt produces, and test archives derived from it exist. They
differ in these ways:

- file header bytes 98..113 hold neither a valid file header checksum nor the
  constant filler;
- the entry header checksum is the CRC-16 of the first 32 header bytes only
  (not of all HS bytes), and is computed before the next offset is filled in,
  so it is wrong even by that definition for every entry that has a next
  sibling;
- the second-block checksum field is zero (this is also true of test
  archives in which the other two checksums were corrected afterwards);
- there are no end-of-folder markers, a first child's previous offset is 0,
  and an empty folder has first-child offset 0;
- method 15 forks carry a CRC-16 in the fork checksum field, and their
  streams are not valid method 15 streams (`arsenic.md` section 17, item 13);
- all dates are one fixed value.

Its writer stores names as UTF-8. That is not a difference: the non-ASCII
names of real SIT5 archives are UTF-8 as well (section 2.6). The synthetic
SIT5 test archives of this repository that were derived from its output had
their names rewritten as MacRoman on purpose, so that the MacRoman branch of
the name rule of section 2.6 is exercised.

Consequence (DECISION): the three SIT5 header checksums (file header, entry
header, second block) are verified, but a mismatch is a **finding**, never a
reason to reject an archive or an entry, unless the caller explicitly asks
for strict checking. Under strict checking a file header mismatch makes the
archive "not this format", an entry header mismatch makes the offset
unusable, and a second-block mismatch (not evaluated for encrypted entries)
makes the offset unusable as well. The entry header rule includes
end-of-folder markers (section 4.5). Strict checking changes nothing else:
every other finding of this document remains a finding under it.

## 5. Detection

The first bytes alone are weak evidence: `SIT!` is 4 bytes, and the 7 bytes
`StuffIt` also begin the unrelated StuffIt X format (BLACK-BOX: data starting
`StuffIt!` is identified as StuffIt X by the independent tool). Use the
structural checks below. Detection needs no more than the first 22 + 112
bytes (classic) or the first 114 bytes plus one entry header (SIT5).

Detection looks **only at archive offset 0** of the bytes it is given. It
never scans for a signature at other positions (BLACK-BOX: the independent
tool does not either); see section 5.3.

### 5.1 Classic

Let **DL** be the declared archive length (offset 6), used here exactly as
stored, not reduced to L. Accept as a classic archive only if all of these
hold:

1. At least 22 bytes are available.
2. Bytes 0..3 are `SIT!`, **or** bytes 0..3 match one of the alternative
   signature patterns of section 3.2 **and** bytes 10..13 are `rLau`.
3. DL is at least 22.
4. One of:
   - (a) DL is less than 134 (an archive with no records) **and** bytes
     10..13 are `rLau`. Bytes after the header are not examined.
   - (b) DL is at least 134, at least 134 bytes are available, and the
     record header at offset 22 passes all of: its header checksum (CRC-16 of
     its bytes 0..109 against bytes 110..111) matches; it is a folder start
     marker or a file, not an end marker (section 3.3); its name length is
     1..31; and, for a file, 134 + R + D is not greater than DL, where R and
     D are its two packed lengths (summed without overflow).
   - (c) DL is at least 134, fewer than 134 bytes are available, **and**
     bytes 10..13 are `rLau` (a truncated archive; reading will report it
     incomplete).

The record test of (b) matters because 112 zero bytes carry a valid checksum.
Detection is deliberately stricter than reading: an archive whose first
record is a file with an empty name is not detected under (b), although
section 3.6 could read it. `rLau` is present in every real archive and
required by the independent tool, but a hand-built archive without it must
still be accepted when it begins with `SIT!` and passes (b). DL greater than
L indicates truncation or a multi-part archive, not a different format.

### 5.2 StuffIt 5

Accept as SIT5 only if all of these hold:

1. At least 114 bytes are available.
2. Bytes 0..15 are `StuffIt (c)1997-`, bytes 16..19 are ASCII digits, bytes
   20..77 are ` Aladdin Systems, Inc., http://www.aladdinsys.com/StuffIt/`,
   bytes 78..79 are CR LF. (BLACK-BOX: the independent tool requires the text
   and the CR LF, and accepts a different year.)
3. Byte 82 is 5.
4. If the top-level count (offset 92) is non-zero: a first-entry offset can
   be located by section 4.3. (That includes the full entry header
   validation of section 4.5, where K is 0 for folders.)

Bytes 80..81 being 0x1A 0x00 and the file header checksum at 98 are
additional positive evidence but are not required (section 4.11).

### 5.3 Wrapped, embedded and multi-part archives

**Wrapped and embedded archives.** A `.sit` file is sometimes not a bare
archive: 5 corpus files are MacBinary wrappers with the archive starting at
file offset 128, and archives also travel inside BinHex files and in the data
fork of self-extracting applications. Finding the archive's first byte is the
job of the layer that understands the wrapper; that layer then hands this
reader the bytes from the archive's first byte onward. All offsets stored in
an archive are relative to the archive's own first byte, wherever the archive
sits in its file (confirmed on the 5 wrapped archives, both generations).
Bytes following the archive (wrapper padding, a wrapper's resource fork) are
harmless: the classic reader stops at the declared length and the SIT5 reader
follows links.

**Multi-part ("segmented") archives.** No multi-part archive was available.
REFERENCE-ONLY: the reference implementation, given a list of part files in
order by its caller, joins their contents end to end with nothing removed and
reads the result as one archive; it recognises no per-part header and has no
means of ordering or identifying parts itself. Whether genuine multi-part
archives carry per-part headers is unknown. DECISION: this reader handles one
byte sequence. A caller that has all parts may join them end to end and
present the result; a single part of a classic archive is read as a truncated
archive (declared length greater than L: incomplete), and a single first part
of a SIT5 archive yields damaged links or truncated forks.

## 6. Safety requirements for untrusted input

General:

1. Do all offset and length arithmetic in at least 64 bits, or with checked
   addition, so that sums of 32-bit fields cannot wrap.
2. Every byte range (header, name, comment, second block, sub-block, key
   bytes, each packed fork) must be verified to lie entirely within L before
   any byte of it is used.
3. An unpacked length field is an untrusted claim. Memory must not be
   reserved in proportion to it before unpacking. The amount of output
   produced for one fork must never exceed its unpacked length field (section
   2.2, step 8), and a reader must let its caller set an upper bound on the
   unpacked size of one fork and on the total unpacked size of one archive;
   exceeding either is the fork error "output limit exceeded" (section 2.2,
   step 6).
4. Apply the delivery steps of section 2.2 completely: length checks, method
   checks, output limits, checksum.
5. Encrypted forks and unsupported method numbers produce distinct fork
   errors, never data.
6. Treat names as untrusted (section 2.6): the safe-component steps are
   mandatory wherever components are joined into a path.
7. **Nesting depth limit.** A reader must enforce a limit on nesting depth,
   must accept at least depth 32, and should use a limit of 64 (real archives
   reach depth 5). What happens at the limit is defined in section 3.6 step 5
   (classic: the listing stops, incomplete) and section 4.9 (SIT5: the folder
   is listed without children, incomplete, and listing continues).
8. **Entry count.** No separate cap is needed: the classic record sequence
   yields at most (END - 22) / 112 entries, and the SIT5 extent rule yields at
   most (L - 114) / 84 entries (84 = 48 + 36, the smallest extent). A reader
   may impose a lower cap; reaching it stops the listing, incomplete.
9. **Path size (SIT5).** Every entry is delivered with the names of all its
   enclosing folders (section 2.6), and a SIT5 name may be 65,487 bytes
   long. A reader that keeps a complete component list for each entry could
   therefore be made to hold memory growing with the square of the archive
   size: one folder with a very long name and many 84-byte children costs
   that name once per child. DECISION: define the **path size** of an entry as
   the sum of the **stored** lengths in bytes (before decoding) of the names
   of all its components, enclosing folders and the entry itself, with
   nothing added for separators. A SIT5 reader keeps a running total of the
   path sizes of the entries it has listed. When listing an entry would take
   that total above **16 x L**, the entry is not listed and the **whole
   listing stops** there, incomplete, with the reason "path size limit
   reached" (the same kind of stop as the extent rule; the test is made
   after the candidate has passed every membership condition of section
   4.9). Stored bytes are counted, not decoded text, so that the result does
   not depend on the name encoding; decoded text is at most 3 times as long
   (MacRoman bytes become at most 3 bytes of UTF-8), which keeps the memory
   for names linear in L. The largest ratio of total path size to L is 0.04
   in the real corpus and 0.15 in the synthetic archives of this repository,
   so the limit leaves a margin of about a hundred. A reader may use a lower
   limit, as for the entry count, but should not go below 16 x L without a
   reason; a reader that stores each folder name once and shares it among
   the folder's descendants applies the same limit, so that all readers list
   the same entries. **The classic container needs no such limit and none is
   applied to it**: a classic name is at most 31 bytes, so the path size of
   an entry is at most 31 times the depth limit and the total is linear in
   END by construction.

Classic:

10. Use END (the smaller of the declared length and L) as the bound for
    record headers, and L as the bound for fork data (section 3.6, step 7;
    section 2.2, step 4).
11. Verify the header checksum of every record before using any of its
    fields.
12. Progress is guaranteed because every record advances by at least 112
    bytes.
13. An end marker with no open folder, and open folders at the end of the
    archive, are findings (section 3.6).
14. Records and forks cannot overlap or be shared, by construction; no
    further rule is needed: the listing stops at the first record that
    extends beyond END, so the sum of all packed lengths that lie within the
    archive cannot exceed L.

SIT5:

15. Apply the validation of section 4.5 to every candidate offset before
    using any field beyond those the validation itself reads.
16. Enforce the visited rule (section 4.9). It is what prevents cycles: next
    offsets legitimately point backwards (leading layout), so "offsets must
    increase" is not a valid rule.
17. Enforce the **extent rule** (section 4.9): the extents of all listed
    entries, added up, must not exceed L - 114. Entries of a well-formed
    archive never overlap, so their extents always fit. Without this rule a
    small archive could contain many 84-byte entries that each declare a
    packed fork covering most of the archive (overlapping each other and
    other entries), and the total packed input would grow with the square of
    the archive size. The rule bounds the sum of all packed fork lengths by
    L. A reader may instead reject overlap exactly, by remembering every
    extent and every marker; the simpler sum is sufficient for safety.
18. Entry and marker offsets are at least 114.
19. Name length N and comment length C are 16-bit and are bounded only by HS
    (also 16-bit), so a name can be up to 65,487 bytes. That is accepted as
    stored; a consumer may refuse over-long names when it creates files.
    The total held for names is bounded by requirement 9.
20. Never use the stored parent offset to build paths (section 4.3).
21. Header checksum mismatches are findings unless strict checking is
    requested (section 4.11).

## 7. Observed implementation gaps

Ways in which the reference implementation's behaviour falls short of, or
departs from, what this document specifies. A new reader must not inherit
them.

Both generations:

1. Fork checksums are never verified, and the amount of unpacked data is
   never compared with the unpacked length field. For 53 small method 13
   forks in real archives (31 distinct packed streams, in five distinct
   archives) it returns truncated data without any error, where the
   independent tool unpacks the same forks with matching checksums. They are
   exactly the real forks that use predefined table set 4 of method 13, for
   which the reference implementation carries three wrong constants
   (`lz-methods.md` sections 6.5 and 8.4). (An earlier draft of this document
   said 33: that count covered three of the five archives.) The checksum
   verification of section 2.2 is therefore the essential acceptance test for
   every decoder.
2. A fork with a method number it does not know is returned as its packed
   bytes, as though they were the contents, without an error. It knows 0, 1,
   2, 3, 13 for classic and 0, 13, 14, 15 for SIT5.
3. Encryption is not supported, and encrypted input is not refused. Supplying
   a password changes nothing. Classic encrypted forks are decoded as if
   they were not encrypted (key material included). SIT5 encrypted files are
   misread: the data fork key bytes are not skipped, so the name is taken
   from the wrong place.
4. Paths are delivered as the names joined with `/`, with no treatment of `/`
   inside a name, of `.` and `..`, of NUL, or of empty names.
5. Detection refuses any input shorter than 80 bytes, which excludes small
   valid classic archives (one with no records is 22 bytes), and otherwise
   accepts any input beginning with the 4 bytes `SIT!` or the 7 bytes
   `StuffIt` (which includes StuffIt X data); for the latter it then requires
   only that byte 82 be 5. The alternative classic signatures are not
   accepted.
6. Dates and SIT5 comments are not delivered.
7. For method 0 the two lengths are not compared; method 14 output is not
   limited to the unpacked length.
8. A fork with packed length 0 yields empty contents even when its unpacked
   length is not 0. A fork with unpacked length 0 and packed length above 0 is
   run through the decoder.

Classic:

9. The declared archive length is not reduced to the available bytes. When
   the data ends inside a record header the listing ends and is reported as
   complete.
10. A header checksum mismatch, or a fork extending past the available data,
    makes the whole archive unreadable: the entries before the fault are
    discarded rather than delivered as an incomplete listing.
11. The second signature, the version, the top-level count, the name
    checksum, the sibling, parent and first-child offsets and all folder
    totals are ignored (none is needed for reading; none is cross-checked).
12. Folder dates are not delivered. An unbalanced end marker, folders left
    open at the end, trailing bytes and all-zero headers produce no
    diagnostic; an all-zero header is listed as a file with an empty name.
13. Sums of an offset and 32-bit lengths are not protected against overflow
    where the native integer is 32 bits wide.

SIT5:

14. **Input that never finishes.** It keeps no record of the offsets it has
    read. An archive in which a folder's first-child offset is the folder's
    own offset makes it list that folder again and again without end, using
    ever more memory. An archive containing a marker whose next offset is the
    marker's own offset makes it read that marker without end. Both were
    reproduced with hand-built archives of under 200 bytes.
15. It reads entries in stored (physical) order: after a file, the next entry
    is taken to start right after the file's fork data; after a folder, at the
    folder's first-child offset. Previous and next offsets are not used,
    except that after a marker reading continues at the marker's next offset
    when that is not 0. It reads until a single running total is reached: the
    top-level count plus the child count of every folder met so far. Real
    archives happen to be stored in an order for which this gives the right
    list. (BLACK-BOX: the independent tool reads the same way.)
16. End-of-folder markers are recognised by the folder flag and an empty name
    only; the 0xFFFFFFFF value is not examined.
17. If the running total has not been reached when fewer than 4 bytes remain,
    it delivers the entries so far as a complete listing.
18. Names are decoded as UTF-8 with substitution characters for invalid
    bytes, never as MacRoman.
19. The header size field is not used. The second block is taken to start
    after the name and after a comment whose presence is inferred from HS but
    whose extent is taken from the comment length alone; the key length byte
    of the data fork is never honoured.
20. None of the three SIT5 checksums is verified. The declared archive length
    and the second first-entry offset are not read.
21. The XOR transformation of section 4.3 is applied to the first-entry
    offset and to parent offsets but not to the first-child and next offsets
    it also follows; nothing confirms that any of this exists in real
    archives.
22. An entry's path is derived from its stored parent offset. A parent offset
    that is not the offset of a folder already read silently places the entry
    at top level. (BLACK-BOX: the independent tool does the same.)
23. For folders, the bytes at the type and creator positions are delivered as
    type and creator although they are window data.
24. The resource fork key bytes are skipped only when the encrypted flag is
    set and the length is not 0; a set resource-fork bit on a folder is
    ignored without a diagnostic.
25. Its writer produces the non-conforming archives described in section
    4.11.

## 8. Worked examples

All examples are synthetic archives built for this document. Each was listed
and integrity-tested successfully by the independent tool. The fork contents
are ASCII `RSRC` (resource fork, checksum 0x389C) and `Hi!` plus CR (data
fork, checksum 0x291E), stored with method 0. The timestamp 0xB8515FAD
(2001-12-27 23:30:53) is used throughout.

### 8.1 Classic: a folder "Dir" containing a file "Both"

Total length 366 bytes (0x16E).

Archive header, archive offsets 0..21:

    53 49 54 21  00 01  00 00 01 6E  72 4C 61 75  02  00  00 00 00 16  00 00

Signature `SIT!`; 1 top-level record; declared length 366; `rLau`; version 2;
unknown byte 0; value 22; unknown 0.

Record 1 at archive offset 22, folder start marker (only non-zero bytes
listed; structure offsets):

| Header offset | Bytes | Meaning |
|---|---|---|
| 0, 1 | 20 20 | Both method bytes 0x20: folder start. |
| 2 | 03 | Name length 3. |
| 3..5 | 44 69 72 | `Dir`. |
| 34..35 | 95 87 | CRC-16 of header bytes 2..33. |
| 48..49 | 00 01 | One child. |
| 62..65 | 00 00 00 86 | First child at archive offset 134. |
| 76..79, 80..83 | B8 51 5F AD (twice) | Creation, modification. |
| 88..91 | 00 00 00 08 | Total unpacked size of contents: 8. |
| 96..99 | 00 00 00 E8 | 232 bytes from offset 134 to the end of the end marker (112 + 8 + 112). |
| 110..111 | 12 E0 | CRC-16 of header bytes 0..109. |

A folder entry `Dir` is listed; `Dir` is now the one enclosing folder. The
next record is at 22 + 112 = 134.

Record 2 at archive offset 134, file:

| Header offset | Bytes | Meaning |
|---|---|---|
| 0, 1 | 00 00 | Both forks method 0, no flags. |
| 2 | 04 | Name length 4. |
| 3..6 | 42 6F 74 68 | `Both`. |
| 34..35 | 36 96 | Name checksum. |
| 58..61 | 00 00 00 16 | Enclosing folder's start marker at 22. |
| 62..65 | FF FF FF FF | Not an offset for files. |
| 66..69 | 54 45 58 54 | Type `TEXT`. |
| 70..73 | 74 74 78 74 | Creator `ttxt`. |
| 74..75 | 01 00 | Finder flags. |
| 76..83 | B8 51 5F AD (twice) | Dates. |
| 84..87 | 00 00 00 04 | Resource unpacked length 4. |
| 88..91 | 00 00 00 04 | Data unpacked length 4. |
| 92..95 | 00 00 00 04 | Resource packed length 4. |
| 96..99 | 00 00 00 04 | Data packed length 4. |
| 100..101 | 38 9C | Resource checksum. |
| 102..103 | 29 1E | Data checksum. |
| 110..111 | 50 C0 | Header checksum. |

Resource fork at 134 + 112 = 246..249: `52 53 52 43`. Data fork at 250..253:
`48 69 21 0D`. The entry's components are `Dir`, `Both`. The next record is
at 254.

Record 3 at archive offset 254, folder end marker: bytes 0, 1 are `21 21`;
bytes 50..53 are `00 00 00 86` (last child at 134); bytes 58..61 are
`00 00 00 16` (matching start marker at 22); bytes 76..83 hold the dates as
above; the header checksum at 110..111 is `87 A6`; everything else is zero.
The innermost enclosing folder, `Dir`, is closed; no folder remains open. The next record would be at 366, which
equals END: the list is complete.

(The sibling offsets at 50..57 of records 1 and 2 are left zero in this
example; real archives fill them in as described in section 3.4.)

Result: folder `Dir`; file `Dir` / `Both` with resource fork at 246 (4 bytes)
and data fork at 250 (4 bytes).

### 8.2 SIT5: folder "Dir" with file "Both", then top-level file "Note"

Total length 458 bytes (0x1CA). Trailing layout. Bytes 0..79 are the banner
with year `2001`.

File header, archive offsets 80..113:

    1A 00  05  10  00 00 01 CA  00 00 00 72  00 02  00 00 00 72  AC D0
    0D A5 A5 52 65 73 65 72 76 65 64 A5 A5 00

Version 5; flags 0x10; declared length 458; first entry at 114; 2 top-level
entries; first entry at 114 again; file header checksum 0xACD0.

Entry at 114 (folder `Dir`), 51 bytes:

    A5 A5 A5 A5  01  00  00 33  00  40  B8 51 5F AD  B8 51 5F AD
    00 00 00 00  00 00 01 67  00 00 00 00  00 03  D5 6B
    00 00 00 C9  00 00 00 08  00 00  00 00  00 01
    44 69 72

Version 1; header size 0x33 = 51; flags 0x40 (folder); dates; previous 0;
next 359; parent 0; name length 3; header checksum 0xD56B; first child at
201; total unpacked size of contents 8; child count 1; name `Dir`. K is 0
(folder), so A = 51 - (48 + 0 + 3) = 0: no comment.

Second block at 114 + 51 = 165, all 36 bytes:

    00 00  95 5D  00 00 00 00  00 00 00 00  01 00
    00 00 00 00 00 00 00 00 00 00 00
    00 00 00 00 00 00 00 00 00 00 00

Flags 0x0000; checksum 0x955D (CRC-16 of these 36 bytes with bytes 2..3
taken as zero); 8 zero bytes where a file would have type and creator;
Finder flags 0x0100; 22 zero bytes. The block ends at 201, which equals the
first-child offset. The folder's extent is 51 + 36 = 87 bytes.

Entry at 201 (file `Both`), 52 bytes:

    A5 A5 A5 A5  01  00  00 34  00  00  B8 51 5F AD  B8 51 5F AD
    00 00 00 72  00 00 01 37  00 00 00 72  00 04  8D CE
    00 00 00 04  00 00 00 04  29 1E  00 00  00  00
    42 6F 74 68

Header size 52; flags 0; previous 114 (a first child points at its folder);
next 311 (the marker); parent 114; name length 4; header checksum 0x8DCE;
data unpacked 4, packed 4, checksum 0x291E; method 0; key length byte 0;
name `Both`.

Second block at 201 + 52 = 253, 36 bytes:

    00 01  83 09  54 45 58 54  74 74 78 74  01 00  (22 zero bytes)

Flags 0x0001 (has resource fork sub-block); checksum 0x8309 (covers these 36
bytes and the 14 that follow); type `TEXT`; creator `ttxt`; Finder flags
0x0100.

Resource fork sub-block at 289, 14 bytes:

    00 00 00 04  00 00 00 04  38 9C  00 00  00  00

Unpacked 4, packed 4, checksum 0x389C, method 0, key length byte 0.

Packed resource fork at 303..306 (`52 53 52 43`); packed data fork at
307..310 (`48 69 21 0D`). The extent is 52 + 36 + 14 + 4 + 4 = 110 bytes and
ends at 311.

Marker at 311, 48 bytes:

    A5 A5 A5 A5  01  00  00 30  00  40  B8 51 5F AD  B8 51 5F AD
    00 00 00 C9  00 00 00 00  00 00 00 72  00 00  EA 5D
    FF FF FF FF  (10 zero bytes)

Header size 48; folder flag; previous 201; next 0; parent 114; name length 0;
header checksum 0xEA5D; structure offset 34 is 0xFFFFFFFF. All three
conditions of the marker test hold. No second block; the marker ends at 359.
(Real trailing markers have zero dates.)

Entry at 359 (file `Note`, with a comment), 59 bytes:

    A5 A5 A5 A5  01  00  00 3B  00  00  B8 51 5F AD  B8 51 5F AD
    00 00 00 72  00 00 00 00  00 00 00 00  00 04  5C 1A
    00 00 00 04  00 00 00 04  29 1E  00 00  00  00
    4E 6F 74 65
    00 03  00 00  48 65 79

Header size 0x3B = 59; previous 114 (the preceding top-level sibling); next
0; parent 0; header checksum 0x5C1A. A = 59 - (48 + 0 + 4) = 7, which is at
least 4: comment length 3 (not more than A - 4 = 3), comment `Hey`.

Second block at 359 + 59 = 418, 36 bytes:

    00 00  E2 D9  54 45 58 54  74 74 78 74  01 00  (22 zero bytes)

Flags 0x0000 (no resource fork); checksum 0xE2D9. Packed data fork at
454..457. The extent is 59 + 36 + 4 = 99 bytes and ends at 458 = archive
length.

Building the list (section 4.9). The top-level count is 2. The candidates of
section 4.3 are 114 (field at 94), 114 (field at 88) and 114; the first
qualifies. The top-level chain starts at 114 with expected count 2.

- Candidate 114 becomes member 1: folder `Dir` is listed (extent total 87).
  Its child count is 1, so its chain is examined, starting at 201 with
  expected count 1.
  - Candidate 201 becomes member 1: file `Dir` / `Both` is listed (extent
    total 197).
  - The next candidate is 311. The chain already has its 1 expected member
    and 311 is a marker: normal end. Its parent offset, 114, is this folder.
- Back in the top-level chain, the next candidate is the folder's next
  offset, 359. It becomes member 2: file `Note` is listed (extent total 296,
  not above 458 - 114 = 344).
- The next candidate is 0 and the chain has its 2 expected members: normal
  end.

Result, in order: folder `Dir`; file `Dir` / `Both` (resource fork at 303,
data fork at 307, 4 bytes each); file `Note` (data fork at 454, 4 bytes,
comment "Hey"). No findings; the listing is complete.

### 8.3 SIT5: an empty folder in the shape real archives use

Total length 343 bytes; top-level count 2; first-entry offset 114.

| Archive offset | Size | What |
|---|---|---|
| 114 | 53 | Folder `Empty`: HS 53, flags 0x40, previous 0, next 251, parent 0, name length 5, header checksum 0x13FD, **first-child offset 203**, total unpacked size 0, child count 0. |
| 167 | 36 | Its second block: flags 0x0000, checksum 0x955D, Finder flags 0x0100, rest zero. |
| 203 | 48 | End-of-folder marker: HS 48, flags 0x40, zero dates, **previous 114**, next 0, **parent 114**, name length 0, header checksum 0x7013, structure offset 34 = 0xFFFFFFFF. |
| 251 | 52 | File `Note`: HS 52, flags 0, previous 114, next 0, parent 0, header checksum 0x3B0B, data fork 4/4 bytes, checksum 0x291E, method 0. |
| 303 | 36 | Its second block: flags 0x0000, checksum 0xE2D9, `TEXT`, `ttxt`, 0x0100. |
| 339 | 4 | Packed data fork `48 69 21 0D`. |

The folder header at 114 in full:

    A5 A5 A5 A5  01  00  00 35  00  40  B8 51 5F AD  B8 51 5F AD
    00 00 00 00  00 00 00 FB  00 00 00 00  00 05  13 FD
    00 00 00 CB  00 00 00 00  00 00  00 00  00 00
    45 6D 70 74 79

The folder at 114 has a name and its structure offset 34 is 203, not
0xFFFFFFFF: it is an ordinary folder. Its child count is 0, so it has no
members and its chain is not examined; its first-child offset is the offset
of a marker, so no finding is recorded. The top-level chain continues with
the folder's next offset 251. Result: folder `Empty`; file `Note` (data fork
at 339).

## 9. Status of points that rest on limited evidence

Each point below was examined against the reference implementation, the real
corpus and black-box tests. "Settled" means the document states it without
reservation; the others keep the label shown.

| # | Point | Status |
|---|---|---|
| 1 | Method 14 is a raw RFC 1951 stream. | Settled for archives written by the reference implementation (inflated successfully as raw DEFLATE). Unknown for genuine archives: none available, and the independent tool cannot unpack this framing. REFERENCE-ONLY. |
| 2 | SIT5 offset XOR with 0xA5A5A5A5 when archive flag 0x10 is clear, applied to the field at 88 and to parent offsets. | REFERENCE-ONLY; never observed; the independent tool has no such transformation. Section 4.3 makes the reader independent of it. |
| 3 | SIT5 name and comment encoding: ASCII, else well-formed UTF-8, else MacRoman. | DECISION. All 38 real non-ASCII SIT5 names are UTF-8; no real MacRoman SIT5 name was found. |
| 4 | Second block is 32 bytes for entry versions other than 1. | Settled as far as two independent readers can settle it (reference implementation; BLACK-BOX for versions 0, 2, 3). No real sample. |
| 5 | Layout of the Finder information tail of the SIT5 second block; classic header bytes 38..45 as a window rectangle; classic start-marker bytes 66..73. | INFERRED; all are ignored by the reader. Classic bytes 48..49 as the folder child count: settled (correct in all 68 real folders). |
| 6 | Classic archive header bytes 16..19 mean "header length". | INFERRED (always 22 in real archives); the reader uses the constant 22. Bytes 15 and 20..21 remain unknown and ignored. |
| 7 | Classic start-marker field 88..91 is the total unpacked size of the folder's contents; the end marker's previous offset is the folder's last child. | Settled (68 of 68 real folders each). |
| 8 | Classic flag bits 0x80 (fork encrypted) and 0x10 (folder contains encrypted files). | Settled on a real password-protected archive, and BLACK-BOX. Alternative classic signatures: BLACK-BOX only. |
| 9 | SIT5 encrypted-entry layout (flag 0x20; key bytes before the name and after the resource fork sub-block). | BLACK-BOX only. Whether the second-block checksum covers resource key bytes: unknown; section 4.6 says what to do. |
| 10 | What the fork checksum of an encrypted fork covers; the ciphers and key derivation of both generations. | Unknown. Not needed: encrypted forks are reported, not unpacked. |
| 11 | Meaning of SIT5 entry flags other than 0x40 and 0x20, second-block flag 0x0010, folder header fields at 42..45, archive flag bits other than 0x10; classic header bytes 36..37 and 106..109. | Unknown; all ignored. |
| 12 | Shape of an empty SIT5 folder. | Settled: 2 real examples (section 4.7), and section 8.3. |
| 13 | Container-level handling of classic methods 1 and 3. | Settled BLACK-BOX with synthetic archives, and for method 3 by one real fork. Methods 5, 6 and 8 are known by name only. |
| 14 | Offsets in wrapped or embedded archives are relative to the archive's first byte. | Settled (5 real MacBinary-wrapped archives). Multi-part archives as plain end-to-end joins: REFERENCE-ONLY. |
| 15 | Method 15 in a classic archive: decoded as in SIT5, fork checksum field not compared. | DECISION, following the independent tool (BLACK-BOX with a synthetic archive). No real example; the reference implementation does not decode it. |
| 16 | Division of work between container layer and method decoders; treatment of short output, surplus output and trailing packed bytes. | DECISION, stated once in section 2.2. |
