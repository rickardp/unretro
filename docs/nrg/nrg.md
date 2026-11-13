# NRG: Nero Burning ROM disc images

Format specification for clean-room implementation. This document covers
what a reader needs to find the tracks inside a Nero disc image and to
hand each track on as one entry:

- **Part A** (sections 3 to 5): the footer at the end of the file, how a
  file is recognised, the chain of chunks the footer points to, and every
  known chunk.
- **Part B** (section 6): the track mode codes, what a sector of each
  mode looks like in the file, and where its user data sits.
- **Part C** (sections 7 and 8): where track data, pregaps and session
  gaps are in the file, and how the byte range of each track is derived
  and validated.
- **Part D** (sections 9 and 10): the real images that were measured,
  and how each track is exposed as an entry.

The reader is read-only and forward-only. It does not decode audio, does
not verify or use error-correction data, and does not read sub-channel
data.

The document contains no program code and no pseudo-code. Hexadecimal
dumps quoted in it are example data.

The entry model is the one of the CUE/BIN reader
([`../cue/cue.md`](../cue/cue.md)). An NRG image and a CUE/BIN image of
the same disc produce the same entries with the same contents, except
where section 10 says otherwise.

## 1. How to read this document

### 1.1 Provenance

- Nero never published the format. Everything known about it comes from
  reverse engineering by others.
- The chunk layouts are described from the Wikipedia article "NRG (file
  format)" in its last full revision, and from the structure definitions
  of three independent readers (libcdio, libMirage, Aaru) and of MAME's
  `chdman`.
- The sector layouts are those of ECMA-130 section 14 and of the CD-ROM
  XA extension, as restated in section 6 of `cue.md`.
- Real-world behaviour was measured on 171 NRG files: two whole files
  from the libcdio test suite and the footers of 169 files from the
  Internet Archive, with sectors read from 17 of them (section 9).
- One existing tool, `cd-info` 2.2.0, was run on the purpose-built
  images of section 12.
- The full list of sources with licences is in
  [`README.md`](README.md), section 3.

### 1.2 Evidence labels

- **CONFIRMED (real data)**: observed on the real images of section 9.
  A count in parentheses says on how many.
- **CONFIRMED (crafted)**: observed by giving `cd-info` 2.2.0 a
  purpose-built image of section 12.
- **DOCUMENTED**: stated by the Wikipedia article, not observed.
- **REFERENCE-ONLY**: stated by the source of one or more existing
  tools, not observed. The tools are named.
- **ECMA-130**: stated by the standard (through `cue.md` section 6).
- **INFERRED**: deduced, unconfirmed.
- **DECISION**: chosen by this specification (listed in section 15). No
  decision changes which bytes of a well-formed image belong to which
  track.

Where sources disagree the text says so and names them.

### 1.3 Terms

| Term | Meaning |
|:--|:--|
| image | The NRG file. By convention with the extension `.nrg`. |
| footer | The last 8 or 12 bytes of the image (section 3). |
| chain | The sequence of chunks that the footer points to. It lies between the track data and the footer. |
| chain offset | The file offset stored in the footer: where the first chunk starts. |
| data area | The bytes of the image before the chain offset. All track data lies here. |
| chunk | One element of the chain: a four-byte identifier, a length and that many bytes of content. |
| track chunk | A chunk that describes tracks: `DAOI`, `DAOX`, `ETNF`, `ETN2`, `TINF`. |
| DAO, disc-at-once | A session written in one pass. Described by a `CUES`/`CUEX` chunk and a `DAOI`/`DAOX` chunk. |
| TAO, track-at-once | A session written track by track. Described by an `ETNF`/`ETN2` chunk. |
| sector | One addressable unit of a CD. 75 sectors make one second of playing time. |
| LBA | The address of a sector on the disc, counted from the first sector of the first track's data (index 1). The 150 sectors before it have the addresses −150 to −1. |
| stored sector size | The number of bytes one sector of a track occupies in the image: 2048, 2336, 2352 or 2448. |
| raw sector | A 2352-byte sector: for a data track it starts with a 12-byte sync pattern and a 4-byte header. |
| cooked data | The user data of data sectors, 2048 bytes per sector, with nothing in between. A file system addresses cooked data. |
| pregap | The sectors of a track before its index 1. |
| entry | One track as listed to the caller. |

All integers in the footer and in chunks are **big-endian** and unsigned
unless a field says otherwise. CONFIRMED (real data, 171). All
quantities derived in this document (offsets, sizes, counts) are to be
held in unsigned 64-bit integers and computed with overflow checks.

## 2. Overview

An NRG image is a single file. It begins, without any header, with the
sectors of the disc's tracks. After the last track comes the chain: a
handful of chunks that say where each track is, how large its sectors
are and what kind of data it holds. The file ends with the footer, which
identifies the format and gives the chain offset.

    +--------------------------------------+ 0
    | data area: sectors of all tracks     |
    +--------------------------------------+ chain offset
    | chain: chunk, chunk, ..., "END!"     |
    +--------------------------------------+ file size - 8 or - 12
    | footer: "NERO" or "NER5", offset     |
    +--------------------------------------+ file size

The smallest real chain seen, of a track-at-once image with one data
track, is 48 bytes followed by a 12-byte footer (section 11.1). The
largest is 5,248 bytes (a disc of 89 tracks).

There are two versions of the format. They differ in the width of file
offsets and nothing else:

| | Version 1 | Version 2 |
|:--|:--|:--|
| Written by | Nero before 5.5 | Nero 5.5 and later, and other tools |
| Footer | `NERO`, 32-bit chain offset | `NER5`, 64-bit chain offset |
| DAO chunks | `CUES`, `DAOI` | `CUEX`, `DAOX` |
| TAO chunk | `ETNF` (earlier: `TINF`) | `ETN2` |

Reading has four steps:

1. Read the footer and validate it (section 3).
2. Walk the chain and collect the tracks from the track chunks (sections
   4 and 5).
3. Validate the tracks' byte ranges against the data area (section 8).
4. Produce each track's content: cooked data for a data track, PCM
   samples for an audio track (sections 6 and 10).

## 3. The footer

### 3.1 Layout

**Version 2**: the last 12 bytes of the file.

| Offset from end | Size | Field |
|--:|--:|:--|
| −12 | 4 | The ASCII bytes `NER5` (`4E 45 52 35`). |
| −8 | 8 | Chain offset. |

**Version 1**: the last 8 bytes of the file.

| Offset from end | Size | Field |
|--:|--:|:--|
| −8 | 4 | The ASCII bytes `NERO` (`4E 45 52 4F`). |
| −4 | 4 | Chain offset. |

Version 2: CONFIRMED (real data, 170). Version 1: CONFIRMED (real data,
1: `videocd.nrg` of the libcdio test suite); DOCUMENTED; libcdio,
libMirage and Aaru agree.

Nothing follows the footer and nothing precedes the track data: there is
no signature at the start of the file. In every real image the chain
ends exactly where the footer begins and the last track ends exactly at
the chain offset. CONFIRMED (real data, 171).

### 3.2 Recognising an image

The signature is at the end of the file, so the test cannot look at the
first bytes. It needs the file's length, its last 12 bytes and the bytes
of the chain. **DECISION**: a file is an NRG image when all of the
following hold.

1. **Footer.** The file has at least 20 bytes and bytes −12 to −9 are
   `NER5` (version 2), or it has at least 16 bytes and bytes −8 to −5
   are `NERO` (version 1). Version 2 is tested first. If the version 2
   test finds its signature but one of the conditions below fails, the
   version 1 test is still made.
2. **Chain offset.** With *E* the offset of the footer (file size minus
   12 or minus 8): the chain offset is at most *E* − 8 (there is room
   for one chunk header), and *E* minus the chain offset is at most
   1,048,576.
3. **Chain.** Walking the chain from the chain offset as section 4.2
   describes reaches an `END!` chunk without a violation: every chunk
   header and every chunk's content lies below *E*, every identifier
   consists of four bytes in the range `20` to `7E` hexadecimal, and at
   most 256 chunks are met, `END!` included.
4. **Tracks.** The chain contains at least one track chunk with at least
   one whole track entry, and at most 99 track entries in all (section
   8.1).

Notes:

- The test is strict on purpose. It is meant to be applied to any file
  whatever its name; the four bytes `NER5` at a fixed distance from the
  end are not enough on their own. A valid chain is.
- An image is recognised by content, not by name. Of the 176 files with
  the extension `.nrg` that were examined, seven were not NRG images:
  four are plain dumps of 2352-byte sectors (their sizes are multiples
  of 2352; one was checked), one starts with the signature of an Alcohol
  120% descriptor (`MEDIA DESCRIPTOR`), two are unidentified. CONFIRMED
  (real data). They are not recognised by this test and are left to the other
  detectors.
- **Order of tests. DECISION**: this test is made before the tests for
  the file systems and partition schemes a track can hold. A
  track-at-once image with one data track, and an image of a DVD, start
  with the track's cooked data at offset 0 and are therefore also valid
  ISO 9660 images with some bytes appended (section 7.3). They are
  NRG images; presenting them as such gives the same result as for every
  other NRG image.
- A truncated image has lost its footer and cannot be recognised. Its
  tracks cannot be located.
- Trailing bytes after the footer make the file unrecognisable. None was
  seen.
- Bytes between the `END!` chunk and the footer are allowed: the walk of
  condition 3 stops at `END!` and does not require the chain to end
  where the footer begins. Real chains do end there (section 3.1), so
  the case arises only in a built image; allowing it is also what makes
  "the version 1 test is still made" observable, since four bytes `NER5`
  can then sit before a `NERO` footer. **DECISION** (added when the
  reader and the fixtures of `testdata/nrg/`, written independently from
  this document, were compared: both had read the test this way).

## 4. The chain

### 4.1 Chunk header

Every chunk starts with an 8-byte header.

| Offset | Size | Field |
|--:|--:|:--|
| 0 | 4 | Identifier: four ASCII characters. |
| 4 | 4 | Length *L* of the content in bytes, not counting the header. |
| 8 | *L* | Content. |

The next chunk starts at offset 8 + *L* from the start of this one.
There is no padding and no alignment: chunks of odd length are followed
directly by the next header. CONFIRMED (real data, 171; a `VOLM` chunk
of 9 bytes and `DAOX` chunks of 22 + 42 × *n* bytes occur).

### 4.2 Walking the chain

- Start at the chain offset. Read a header; if the identifier is `END!`
  the chain is complete. Otherwise note the chunk if it is one the
  reader uses and continue with the next header.
- The chain always ends with `END!`. CONFIRMED (real data, 171). A chain
  that runs into the footer without one is invalid (section 3.2).
- A chunk with an identifier the reader does not know is skipped by its
  length. **Unknown chunks are never an error.** Real images contain
  chunks that no public description lists (`AFNM`, `VOLM`: section 5.8).
- The same identifier can occur several times. In a multi-session image
  each session has its own track chunk (section 7.5). The order of
  chunks in the chain is the order of the sessions. CONFIRMED (real
  data, 9 two-session images).
- The identifier decides the layout of a chunk, not the footer version.
  **DECISION.** A `DAOX` chunk is read with 64-bit fields even under a
  `NERO` footer. (libcdio does the same; libMirage looks only for the
  identifiers of the footer's version.) No image mixing the two families
  was seen.
- The chain cannot loop: every chunk is at least 8 bytes long and the
  position only moves forward. The limit of 256 chunks bounds the work
  for a chain of empty chunks.

### 4.3 The chunks

| Identifier | Bytes | Version | Contents | Use |
|:--|:--|:--|:--|:--|
| `CUES` | `43 55 45 53` | 1 | Cue points of a DAO session (section 5.3) | skipped |
| `CUEX` | `43 55 45 58` | 2 | The same with 32-bit LBA positions | skipped |
| `DAOI` | `44 41 4F 49` | 1 | Tracks of a DAO session, 32-bit offsets (section 5.1) | **used** |
| `DAOX` | `44 41 4F 58` | 2 | Tracks of a DAO session, 64-bit offsets | **used** |
| `TINF` | `54 49 4E 46` | before 1 | Tracks of a TAO session, short form (section 5.2) | **used** |
| `ETNF` | `45 54 4E 46` | 1 | Tracks of a TAO session, 32-bit offsets | **used** |
| `ETN2` | `45 54 4E 32` | 2 | Tracks of a TAO session, 64-bit offsets | **used** |
| `SINF` | `53 49 4E 46` | both | Number of tracks of one session (section 5.4) | skipped |
| `MTYP` | `4D 54 59 50` | both | Medium type (section 5.5) | skipped |
| `CDTX` | `43 44 54 58` | 2 | CD-TEXT packs (section 5.6) | skipped |
| `DINF` | `44 49 4E 46` | both | Disc information (section 5.7) | skipped |
| `TOCT` | `54 4F 43 54` | both | Table-of-contents type (section 5.7) | skipped |
| `RELO` | `52 45 4C 4F` | both | Unknown (section 5.7) | skipped |
| `AFNM` | `41 46 4E 4D` | 2 | Names of the audio source files (section 5.8) | skipped |
| `VOLM` | `56 4F 4C 4D` | 2 | Compilation name (section 5.8) | skipped |
| `END!` | `45 4E 44 21` | both | End of the chain (section 5.9) | ends the walk |
| anything else | | | | skipped |

The typical chain of a DAO image is `CUEX`, `DAOX`, `SINF`, `MTYP`,
`END!` (148 of the 165 DAO images; the others add a second
`CUEX`/`DAOX` pair and `SINF`, or `CDTX`, or `AFNM` and `VOLM`, or lack
`SINF` and `MTYP`). The chains of the five TAO images are `ETN2`,
`END!` (three), `ETN2`, `MTYP`, `END!` (one) and `ETN2`, `RELO`, `TOCT`,
`SINF`, `DINF`, `MTYP`, `END!` (one); the version 1 image has `ETNF`,
`SINF`, `END!`. CONFIRMED (real data).

## 5. Chunks in detail

### 5.1 DAOI and DAOX: the tracks of a disc-at-once session

The content is a 22-byte header followed by one entry per track. The
header is the same in both versions.

| Offset | Size | Field | Use |
|--:|--:|:--|:--|
| 0 | 4 | The chunk's length *L* again. | ignored |
| 4 | 13 | Media catalogue number (UPC/EAN) as ASCII digits, or 13 zero bytes, or 13 spaces. | ignored |
| 17 | 1 | Zero. | ignored |
| 18 | 1 | Session type: `00` CD-DA or CD-ROM, `10` CD-i, `20` CD-ROM XA, `40` on DVD images. | ignored |
| 19 | 1 | `01` in the last session of the image, `00` in a session that is followed by another. | ignored |
| 20 | 1 | Number of the first track of the session, binary. | ignored |
| 21 | 1 | Number of the last track of the session, binary. | ignored |

- Field 0: big-endian and equal to *L* in all 174 `DAOX` chunks seen.
  CONFIRMED (real data). The Wikipedia article and the field's name in
  libcdio call it little-endian; that was not seen. It is not needed.
- Field 18: `00` in 137 chunks, `20` in 31, `40` in 6 chunks, all of
  them in DVD images (section 7.4). CONFIRMED (real data). The values'
  meaning is libMirage's (REFERENCE-ONLY); `10` was not seen. The reader
  takes the kind of each track from the track's own mode (section 6),
  never from this byte.
- Field 19: `01` in 164 last sessions, `00` in all 9 sessions that are
  followed by another, and `00` in one single-session image. CONFIRMED
  (real data); the interpretation is INFERRED. The Wikipedia article
  treats bytes 18 and 19 as one 16-bit "Toc type".
- Fields 20 and 21: last − first + 1 equals the number of entries, and
  the first track of each session is one more than the last track of
  the session before, in all 174 chunks. CONFIRMED (real data). The
  reader does not use them (section 8.1).

A track entry is 42 bytes in `DAOX` and 30 bytes in `DAOI`.

| Offset | Size in `DAOX` | Size in `DAOI` | Field | Use |
|--:|--:|--:|:--|:--|
| 0 | 12 | 12 | ISRC as ASCII, or 12 zero bytes. | ignored |
| 12 | 2 | 2 | Stored sector size *S* in bytes. | **used** |
| 14 | 1 | 1 | Mode code *M* (section 6.1). | **used** |
| 15 | 1 | 1 | Zero. | ignored |
| 16 | 2 | 2 | `00 01`. | ignored |
| 18 | 8 | 4 | *index 0*: file offset of the first sector of the track's pregap. | **used** |
| 26 | 8 | 4 | *index 1*: file offset of the first sector of the track's data. | **used** |
| 34 | 8 | 4 | *end*: file offset one past the track's last sector. | **used** |

(In `DAOI` the last two fields are at offsets 22 and 26.)

- `DAOX`: CONFIRMED (real data, 174 chunks, 1,384 entries). Bytes 15 to
  17 are `00 00 01` in every entry. Wikipedia and MAME read bytes 14 and
  15 as one 16-bit mode whose values are the codes of section 6.1 times
  256; that is the same thing.
- `DAOI`: REFERENCE-ONLY (libcdio, libMirage and Aaru agree on a 22-byte
  header and 30-byte entries). The Wikipedia article's table for version
  1 has wider fields (a 14-byte catalogue number, 32-bit type, sector
  size and mode) that add up to a 24-byte header and 32-byte entries.
  **DECISION**: the layout of the three tools is used. No `DAOI` chunk
  was seen (section 14).
- The number of entries is (*L* − 22) divided by the entry size, rounded
  down. The division was exact in all 174 chunks. A remainder is
  ignored; a chunk shorter than 22 bytes has no entries. **DECISION.**
- The three offsets are **byte offsets from the start of the file**, not
  sector numbers. In every entry seen, *index 0* ≤ *index 1* ≤ *end*,
  and both *index 1* − *index 0* and *end* − *index 1* are multiples of
  *S*. CONFIRMED (real data, 1,384).

### 5.2 ETNF, ETN2 and TINF: the tracks of a track-at-once session

The content is one entry per track with no header.

**`ETN2`**, 32 bytes per entry:

| Offset | Size | Field | Use |
|--:|--:|:--|:--|
| 0 | 8 | File offset of the track's first sector. | **used** |
| 8 | 8 | Length of the track in bytes. | **used** |
| 16 | 4 | Mode code *M* (section 6.1), a 32-bit number. | **used** |
| 20 | 4 | LBA of the track's first sector on the disc. | ignored |
| 24 | 4 | Zero. | ignored |
| 28 | 4 | Length of the track in sectors, or zero. | ignored |

**`ETNF`**, 20 bytes per entry:

| Offset | Size | Field | Use |
|--:|--:|:--|:--|
| 0 | 4 | File offset of the track's first sector. | **used** |
| 4 | 4 | Length of the track in bytes. | **used** |
| 8 | 4 | Mode code *M*. | **used** |
| 12 | 4 | Sector number of the track's first sector (see below). | ignored |
| 16 | 4 | Zero. | ignored |

**`TINF`**, 12 bytes per entry: the first three fields of `ETNF` and
nothing else.

- `ETN2`: CONFIRMED (real data, 5 images, 15 entries). The last field
  holds the sector count in 14 entries and zero in one.
- `ETNF`: CONFIRMED (real data, 1 image, 5 entries).
- `TINF`: REFERENCE-ONLY (Aaru alone knows it; it is said to precede
  `ETNF`). **DECISION**: it is read as specified here. No such chunk was
  seen.
- The number of entries is *L* divided by the entry size, rounded down;
  a remainder is ignored. **DECISION.**
- There is no stored sector size; it follows from the mode code (section
  6.1).
- A TAO track has no pregap in the image (section 7.3). Its data is the
  bytes from the offset to offset + length.
- The sector number field is not needed and its meaning differs between
  the two versions. In the `ETN2` image with eleven tracks every track
  starts 150 sectors after the end of the one before (lengths 600 and
  35,656 sectors, starts 0, 750 and 36,556): the field is the LBA on the
  disc, where every track is preceded by a pregap of 150 sectors that
  the image does not hold. In the `ETNF` image the tracks follow one
  another without that distance (lengths 676 and 75, starts 0, 676 and
  751), although the files of the disc's own ISO 9660 volume lie at 826
  and 1,051: there the field counts the sectors stored in the image, and
  the disc address of track *k* is the field plus 150 × (*k* − 1).
  CONFIRMED (real data); libcdio's expected output for the image lists
  the tracks at 826, 1,051, 1,276 and 1,501. The tools read the field
  in different ways. It is not needed.

### 5.3 CUES and CUEX: cue points

A DAO session's `DAOI`/`DAOX` chunk is preceded by a chunk of cue
points, 8 bytes each. **The reader does not need this chunk**: the DAO
chunk gives every track's position as a byte offset, and the reader
exposes neither disc addresses nor index points above 1. **DECISION**:
it is skipped. It is described because it explains the layout of the
image.

**`CUEX`** entry:

| Offset | Size | Field |
|--:|--:|:--|
| 0 | 1 | Control and address type as in the disc's table of contents: `41` data track, `01` audio track, `21` audio track with copy permitted. |
| 1 | 1 | Track number in binary-coded decimal; `00` for the lead-in, `AA` for the lead-out. |
| 2 | 1 | Index number in binary-coded decimal. |
| 3 | 1 | Zero. |
| 4 | 4 | LBA of the cue point, a **signed** 32-bit number. |

CONFIRMED (real data, 174 chunks). Track numbers above 9 appear as `10`
to `99` hexadecimal, which shows the coding.

The entries of a session are, in this order: one for track `00` index
`00`; for each track one for index `00` and one for index `01`; one for
track `AA` index `01`. That is 2 × (tracks + 1) entries. CONFIRMED (real
data, 174). No index above `01` was seen.

- The first entry has the LBA −150 (`FF FF FF 6A`) in the first session
  of a CD image, 0 in a DVD image and the LBA of the session's start in
  a later session. The second entry (track 1, index `00`) repeats it.
- The `AA` entry gives the LBA one past the session's last sector.
- The LBAs relate to the offsets of the DAO chunk in the obvious way:
  a track's index 1 LBA minus its index 0 LBA equals (*index 1* − *index
  0*) / *S*, and the distance to the next cue point equals (*end* −
  *index 1*) / *S*. This holds for all tracks of 137 images. In 28
  images written by a tool other than Nero it does not (section 7.2).

**`CUES`**: 8-byte entries with the same first three bytes. Sources
disagree about the rest. Aaru: bytes 3 and 4 unused, bytes 5 to 7 the
position as minute, second, frame in binary-coded decimal, counted from
the start of the lead-in (so that LBA = minutes × 4500 + seconds × 75 +
frames − 150). libMirage: the same three bytes, read as binary numbers.
libcdio and the Wikipedia article: a 32-bit LBA as in `CUEX`. No `CUES`
chunk was seen. Since the chunk is skipped the question is left open.

### 5.4 SINF: session information

Content: one 32-bit number, the count of tracks in one session. An image
has one `SINF` chunk per session, in session order, after all track
chunks. CONFIRMED (real data: 175 `SINF` chunks, each equal to the
number of entries of the corresponding track chunk). Four TAO images and
one DAO image have none. The reader does not use it: the track chunks
already say which tracks exist. **DECISION.**

### 5.5 MTYP: medium type

Content: one 32-bit number, a set of flags naming the kind of medium the
image was made from or for. The value is 1 (CD) in 157 images and `1C`
hexadecimal (the three DVD flags) in the nine DVD images. CONFIRMED (real
data). Flag values, REFERENCE-ONLY (libMirage and Aaru give the same
list): `1` CD, `2` DDCD, `4` DVD-R/RW, `8` DVD+RW, `10` DVD-RAM, `200`
DVD-ROM, `400` CD-ROM, `40000` and `80000` double-layer DVD, `100000`
to `400000` Blu-ray, `800000` to `2000000` HD DVD. Not used: a DVD image
has a single track of 2048-byte sectors and is read like any other
(section 7.4).

### 5.6 CDTX: CD-TEXT

Content: CD-TEXT packs of 18 bytes each (pack type, track number,
sequence number, block and character position, 12 bytes of text, 2 bytes
of CRC) exactly as on the disc. CONFIRMED (real data, 6 images; the
chunk follows the `DAOX` chunk). Not used: entries carry no titles
(section 10.6).

### 5.7 DINF, TOCT and RELO

Seen together in one TAO image, between the `ETN2` chunk and `MTYP`, in
the order `RELO`, `TOCT`, `SINF`, `DINF`. CONFIRMED (real data, 1).

| Chunk | Length | Content seen | Meaning |
|:--|--:|:--|:--|
| `RELO` | 4 | `00 00 00 00` | Unknown. |
| `TOCT` | 2 | `00 20` | INFERRED: the session type of section 5.1 (the image is a Super Video CD, a CD-ROM XA disc). The Wikipedia article saw only zero. |
| `DINF` | 4 | `00 00 00 00` | DOCUMENTED: 1 for a disc that was not closed, otherwise 0. |

None is used.

### 5.8 AFNM and VOLM

Seen in one image of an audio disc, after `MTYP`. CONFIRMED (real data,
1). No public description mentions them.

- `AFNM` (256 bytes for 12 tracks): the names of the audio files the
  tracks were made from, each ended by a zero byte, in track order (`01
  ….wav`, `02 ….wav`, and so on).
- `VOLM` (9 bytes): the text `Audio CD` and a zero byte; INFERRED to be
  the name of the compilation.

Neither is used. They are named here as proof that unknown chunks occur.

### 5.9 END!

Length 0, no content. CONFIRMED (real data, 171). It ends the chain. Its
length field is bounds-checked like any other and otherwise ignored:
an `END!` chunk whose stated content lies below the footer ends the
chain whatever its length, and one whose content would reach into or
beyond the footer makes the file "not an NRG image". **DECISION.**

## 6. Track modes and sectors

### 6.1 Mode codes

The mode code says what kind of sectors a track holds and how many bytes
of each sector are stored.

| Code | Track holds | Stored sector size | Stored sector is | Evidence |
|--:|:--|--:|:--|:--|
| `00` | Mode 1 data | 2048 | the user data | CONFIRMED (real data: 34 DAO tracks, 4 TAO tracks) |
| `02` | Mode 2 Form 1 data | 2048 | the user data | CONFIRMED (real data: 69 DAO tracks) |
| `03` | Mode 2 data, both forms | 2336 | sub-header and what follows it | CONFIRMED (real data: 270 DAO tracks, 16 TAO tracks) |
| `05` | Mode 1 data, raw | 2352 | a raw sector | CONFIRMED (real data: 1 DAO track) |
| `06` | Mode 2 data, raw | 2352 | a raw sector | REFERENCE-ONLY (libMirage, Aaru, MAME); DOCUMENTED |
| `07` | Audio | 2352 | an audio sector | CONFIRMED (real data: 982 DAO tracks); in TAO chunks DOCUMENTED |
| `08` | Audio | 2352 | an audio sector | REFERENCE-ONLY (Aaru alone, "alternative audio") |
| `0F` | Mode 1 data, raw, with sub-channel | 2448 | a raw sector, then 96 bytes | CONFIRMED (real data: 1 DAO track) |
| `10` | Audio with sub-channel | 2448 | an audio sector, then 96 bytes | CONFIRMED (real data: 12 DAO tracks) |
| `11` | Mode 2 data, raw, with sub-channel | 2448 | a raw sector, then 96 bytes | CONFIRMED (real data: 15 DAO tracks) |

- In every DAO entry seen, the stored sector size field *S* has the
  value this table gives for the entry's mode. CONFIRMED (real data,
  1,384).
- Codes `00` and `02` store the same thing. They differ in what the
  sectors were on the disc, which the reader does not need.
- Code `03` is named "Mode 2 Form 2" by libMirage and Aaru. The real
  tracks of this mode hold Form 1 sectors (file system structures) and
  Form 2 sectors (video streams) side by side. CONFIRMED (real data:
  `videocd.nrg`, track 1).
- There is no code for Mode 2 without forms and none is known for
  2324-byte sectors.
- libcdio also accepts a code `20` hexadecimal as some Mode 2 kind,
  with a question mark in its source. It is not in this table.
- An unknown code: libMirage treats it as audio, MAME refuses the
  image. **DECISION**: the track is not readable and is reported
  (section 8.3). Tracks after it are still found, because every track's
  position is an absolute file offset.

**Which field decides. DECISION.**

- In a TAO chunk the mode code is all there is: it gives the kind and,
  through the table, the stored sector size.
- In a DAO chunk the mode code gives the **kind** (audio for `07`, `08`
  and `10`; data for the others) and the stored sector size **field**
  *S* gives the size. *S* must be 2048, 2336, 2352 or 2448. For an audio
  track it must be 2352 or 2448. Any other value makes the track
  unreadable.

  The reason is that images exist in which the two disagree: Aaru's
  source says that MagicISO writes raw 2352-byte sectors under the codes
  `00`, `02` and `03` (REFERENCE-ONLY). Using *S* for the size and
  looking at the sectors themselves (section 6.3) reads such images
  correctly; trusting the code would read them in steps of the wrong
  size.

### 6.2 Sector layouts

The layouts are those of `cue.md` section 6, restated briefly.

**Audio sector, 2352 bytes**: 588 stereo sample pairs, each a left then
a right sample, each sample a signed 16-bit integer stored **least
significant byte first**, at 44,100 pairs per second. No sync pattern,
no header. CONFIRMED (real data: in two images the sample values form
smooth waveforms only when read least significant byte first). MAME
states the same order.

**Raw data sector, 2352 bytes** (ECMA-130):

| Offset | Size | Field |
|--:|--:|:--|
| 0 | 12 | Sync pattern: `00`, then ten times `FF`, then `00`. |
| 12 | 3 | Address as minute, second, frame in binary-coded decimal: LBA + 150. |
| 15 | 1 | Mode: `01` or `02`. |
| 16 | 2336 | Mode 1: 2048 bytes of user data, 4 of error detection code, 8 zero, 276 of error correction code. Mode 2: the 2336 bytes described next. |

CONFIRMED (real data): the sector at LBA 16 of a `05` track and of a
`0F` track starts with the sync pattern and `00 02 16 01`; that of an
`11` track with the sync pattern and `00 02 16 02`. The first pregap
sector of a `05` track (LBA −150) has the header `00 00 00 01`. The
sectors are stored unscrambled.

**Mode 2 sector without sync and header, 2336 bytes** (bytes 16 to 2351
of a raw Mode 2 sector):

| Offset | Size | Field |
|--:|--:|:--|
| 0 | 4 | Sub-header: file number, channel number, sub-mode, coding information. |
| 4 | 4 | The same four bytes repeated. |
| 8 | 2048 | Form 1: user data. (Form 2: the first 2048 of 2324 bytes of user data.) |
| 2056 | 280 | Form 1: error detection and correction codes. (Form 2: 276 more bytes of user data and 4 bytes of error detection code.) |

Bit 5 of the sub-mode byte (offset 2, value `20` hexadecimal) is set in
a Form 2 sector. CONFIRMED (real data): sector 16 of every `03` track 1
examined is `00 00 09 00 00 00 09 00` followed by the ISO 9660 volume
descriptor (`01 43 44 30 30 31`, the text `CD001` at offset 9).

**Sub-channel bytes.** In a 2448-byte sector the 96 bytes after the
2352-byte sector hold the eight sub-channels P to W interleaved: one
byte per sub-channel symbol, bit 7 is P, bit 6 is Q. CONFIRMED (real
data: in the 96 bytes of data-track sectors only the values `00`, `40`,
`80` and `C0` occur). They are dropped.

### 6.3 Cooking a data track

A data track is handed on as cooked data: exactly 2048 bytes for every
stored sector, in order. **DECISION**, the same as `cue.md` section
6.4. The 2048 bytes of one stored sector are found from the stored
sector size:

| Stored sector size | The 2048 bytes are |
|--:|:--|
| 2048 | the stored sector itself |
| 2336 | the 2048 bytes at offset 8 |
| 2352 | found in the raw sector as described below |
| 2448 | found in the first 2352 bytes as described below; the last 96 bytes are dropped |

For a raw sector: if its first 12 bytes are the sync pattern and its
mode byte (offset 15) is `01`, the 2048 bytes at offset 16; if they are
the sync pattern and the mode byte is `02`, the 2048 bytes at offset 24;
otherwise the 2048 bytes at the offset the track's mode code implies: 16
for the codes `00`, `05` and `0F`, 24 for `02`, `03`, `06` and `11`.

- The sector's own mode byte is preferred over the mode code for the
  reasons given in `cue.md`: it is what the repository's existing
  raw-sector code (`src/formats/cd_sector.rs`) does, and it makes a
  wrong code harmless.
- A Form 2 sector contributes the first 2048 of its 2324 user bytes. The
  consequence, as for CUE/BIN, is that a file stored in Form 2 sectors
  (the video streams of a Video CD) is not recoverable from the cooked
  track.
- Mode 2 sectors are always read as XA sectors (sub-header present).
- The error detection and correction codes are neither checked nor used.

An audio track is handed on as its samples: the first 2352 bytes of
every stored sector, unchanged.

## 7. Where things are in the file

### 7.1 A disc-at-once session

The data area holds, for each track in order, the track's pregap and
then its data. The DAO entry's *index 0* is where the pregap starts,
*index 1* where the data starts, *end* where the track ends.

In an image written by Nero the tracks tile the data area: the first
*index 0* is 0, every later *index 0* equals the *end* of the track
before, and the last *end* is the chain offset. CONFIRMED (real data,
137 of the 165 DAO images).

Pregaps as stored, in whole sectors of the track's own stored size
(CONFIRMED (real data), all 174 `DAOX` chunks):

| Track | Pregap stored |
|:--|:--|
| First track of the first session, CD image | 150 sectors in 154 images; 300 in one (a hidden pre-track passage on an audio disc); 151 in one |
| First track of a later session | 150 sectors (all 9) |
| Only track of a DVD image | none: *index 0* = *index 1* = 0 (all 9) |
| A later audio track | none in 936 tracks (*index 0* = *index 1*); 150 sectors in 8; 75 in one |
| A later data track | 150 sectors in 252 tracks; none in 12; 302 in one |

What the pregap bytes are:

- Before a cooked data track (codes `00`, `02`, `03`): zero bytes.
- Before a raw data track: sectors with sync pattern, header and
  all-zero user data.
- Before an audio track: silence (zero bytes), or whatever the disc had
  there.

The consequence that matters most: **the first track's data does not
start at offset 0 of the file.** In the common image of a data CD it
starts at 150 × 2048 = 307,200. An NRG image is therefore not an ISO
9660 image with a trailer, and a tool that treats it as one finds the
volume descriptor 150 sectors late.

### 7.2 Bytes that belong to no track

In 28 of the DAO images the tracks do not tile the data area. They come
from four Internet Archive items and were written by a conversion tool,
not by Nero. CONFIRMED (real data). In them:

- every audio track has *index 0* = *index 1*;
- between the *end* of each track and the *index 0* of the next lie 150
  sectors of the next track's size (352,800 bytes before an audio
  track), all zero; where a cooked data track is followed by an audio
  track there are 660,000 bytes: 150 sectors of 2048 bytes and 150 of
  2352;
- the `CUEX` chunk of the same image counts those sectors to the track
  before.

The bytes are the next track's pregap (and the data track's postgap),
which the tool stored but left out of both tracks. They are in no
track's range and so in no entry, which is the same result as for a
pregap that a DAO entry does describe (section 10.5).

The reader must therefore not require that tracks tile the data area. It
requires only that they do not overlap and do not go backwards (section
8.2).

### 7.3 A track-at-once session

The data area holds the data of each track and nothing else: no pregaps,
no gaps between tracks. Each entry gives its track's offset and length.
In all five `ETN2` images and the `ETNF` image the first track starts at
offset 0, each track starts where the one before ends, and the last
ends at the chain offset. CONFIRMED (real data, 6 images, 20 tracks).

A TAO image with a single cooked data track is therefore byte for byte
the track's cooked data followed by the chain and the footer: an ISO
9660 image with 60 or 72 bytes appended (section 3.2 says why it is
nevertheless an NRG image).

On the disc every TAO track is preceded by a pregap of 150 sectors. The
image does not hold them (section 5.2).

### 7.4 DVD images

A DVD image is a DAO image with one track of mode `00`, stored sector
size 2048 and *index 0* = *index 1* = 0; its `CUEX` chunk starts at LBA
0 instead of −150 and its `MTYP` value is `1C`. The session type is
`40` in six of the nine images seen and `00` in three. CONFIRMED (real
data, 9 images of 0.7 to 4.5 GiB). It needs no special handling. The
64-bit offsets of version 2 exist for these images; a version 1 image
cannot exceed 4 GiB.

### 7.5 Multi-session images

A session is described by one DAO chunk (with its cue chunk before it)
or one TAO chunk. A multi-session image has several, in session order,
followed by one `SINF` per session:

`CUEX`, `DAOX`, `CUEX`, `DAOX`, `SINF`, `SINF`, `MTYP`, `END!`

CONFIRMED (real data: 9 images with two DAO sessions. Eight are audio
discs with a data session at the end, "CD Extra"; one has two data
sessions.) No image with more than two sessions, with two TAO sessions
or with sessions of both kinds was seen; the rules below do not depend
on it.

- The offsets of the second session's tracks continue where the first
  session ends: its first *index 0* equals the last *end* of the first
  session. Its first track has a stored pregap of 150 sectors.
- **The gap between the sessions is not in the file.** On the disc the
  first session is followed by its lead-out, the second session's
  lead-in and that session's first pregap; the `CUEX` chunks show a
  distance of 11,250 sectors between the first session's `AA` entry and
  the second session's first entry in all 9 images. The data area holds
  none of those sectors.
- Track numbers continue across sessions (the second session of an
  audio disc with 17 tracks starts with track 18).
- The file system of a later data session uses disc addresses: in the
  image with two data sessions the second session's volume descriptor
  gives the root directory at sector 79,873, and the session's data
  starts at LBA 79,854 (from its `CUEX` chunk). CONFIRMED (real data).
  The reader does not translate addresses; section 10.7 says what this
  means for the entries.

## 8. From the chain to byte ranges

### 8.1 Collecting the tracks

Walk the chain once. Every track chunk contributes its entries, in
order, to one list of tracks for the whole image. The other chunks
contribute nothing.

- **Track numbers. DECISION**: the tracks are numbered 1, 2, 3, … in
  the order of the list, across sessions. The first-track and last-track
  fields of the DAO header are not used. In every real image the two
  agree (section 5.1); numbering by position cannot produce duplicate or
  out-of-range numbers whatever the header says.
- **Limit. DECISION**: at most 99 tracks in all. A chain with more is
  not an NRG image (section 3.2). A CD cannot have more.
- Sessions are not modelled: after this step nothing remembers which
  chunk a track came from.

For each track the reader now has:

| Quantity | From a DAO entry | From a TAO entry |
|:--|:--|:--|
| mode code *M* | byte 14 | the low 8 bits of the mode field, if the other 24 bits are zero; otherwise unknown |
| stored sector size *S* | the sector size field | from *M* by the table of section 6.1 |
| *first* | *index 0* | the offset |
| *start* | *index 1* | the offset |
| *end* | *end* | offset + length (with overflow check) |

### 8.2 Consistency of the layout

Before any track is read the whole list is checked. With the tracks in
list order, the sequence

*first*₁, *start*₁, *end*₁, *first*₂, *start*₂, *end*₂, …

must never decrease. Equal neighbours are allowed (a track without
pregap, tracks that tile the data area, an empty track); gaps between
one track's *end* and the next track's *first* are allowed (section
7.2).

If the sequence decreases anywhere, or a TAO entry's offset + length
overflows, the layout is inconsistent: **no track is read and every
track is reported**. **DECISION**, the counterpart of the rule in
`cue.md` section 7.2 for marks that go backwards. Two tracks that
overlap cannot both be right and there is no telling which is. The rule
held for all 1,404 tracks of the real images.

This check uses only the numbers in the chain. It does not look at *S*
or *M*, so a track with an unknown mode still takes part in it.

### 8.3 The range of a track

With *D* the chain offset (the end of the data area), a track of a
consistent layout is read as follows.

1. If *M* is not in the table of section 6.1, or *S* is not allowed for
   it (section 6.1, "Which field decides"), the track is not readable.
   When both hold, the unknown mode code is the reason reported.
2. If *start* ≥ *D* the track is not readable: it starts beyond the data
   area.
3. *end* is lowered to *D* if it is greater. **DECISION**: ends are
   clamped, starts are not, as in `cue.md` section 7.4.
4. The number of sectors is (*end* − *start*) divided by *S*, rounded
   down. A trailing partial sector is ignored. If the number is 0 the
   track is not readable: it is empty.
5. The track's sectors are the bytes from *start*, *S* bytes each.

A track that is not readable produces no entry and is reported (section
10.4). The other tracks are not affected.

The pregap, the bytes from *first* to *start*, is not part of the range.

Because ranges cannot overlap, lie inside the data area and are read in
whole sectors, the total of all entries is at most *D* bytes: the reader
never produces more data than the image holds.

## 9. The real images

None of this data is in the repository. Item names are identifiers at
the Internet Archive (`https://archive.org/details/` followed by the
identifier). Of each file the last 8,192 bytes were fetched; "sectors"
marks the files of which single sectors at track starts were also read.
The two libcdio files were read whole.

### 9.1 Summary

| | Count |
|:--|--:|
| Files with the extension `.nrg` examined | 178 |
| … of which NRG images | 171 |
| Version 2 (`NER5`) | 170 |
| Version 1 (`NERO`) | 1 |
| One DAO session | 156 |
| Two DAO sessions | 9 |
| One TAO session (`ETN2`: 5, `ETNF`: 1) | 6 |
| `DAOX` chunks / track entries | 174 / 1,384 |
| TAO entries | 20 |
| Images with `CDTX` | 6 |
| Largest chain | 5,248 bytes, 5 chunks, 89 tracks |
| Most chunks in a chain | 8 |

### 9.2 Representative images

Digest: the first 16 hexadecimal digits of the SHA-256 of the file's
last 8,192 bytes (of the whole file for the libcdio files).

| # | Item identifier (file) | File size | Digest | What it shows |
|--:|:--|--:|:--|:--|
| 1 | libcdio `test/data/videocd.nrg` | 2,630,472 | `4d7f7e3e7cd73dc1` | Version 1, `ETNF`, five tracks of mode `03`; whole file |
| 2 | libcdio `test/data/p1.nrg` | 1,063,470 | `224f08c2330fc109` | DAO, two audio tracks, pregap of 300 sectors, `CDTX`; whole file |
| 3 | `wonderwitch-magical-cdrom` | 18,874,500 | `aa3e6a9e44af3aef` | DAO, one track of mode `00`; chain without `SINF` and `MTYP` |
| 4 | `diamond_mako` | 31,817,884 | `ba2d7bf8127d47da` | DAO, one track of mode `02`; sectors |
| 5 | `PFAFF_DEMO_CD` | 53,454,908 | `712e04aa6fa17f6e` | TAO, `ETN2`, one track of mode `00`, chain of 48 bytes; sectors |
| 6 | `virt-ddt` | 679,430,216 | `3e0e2107aa6eb7a2` | TAO, `ETN2` and `MTYP`, sector count field zero; sectors |
| 7 | `music-factory-1991-karaoke-ld2svcd` (side B) | 826,876,694 | `a081b6a99be99bbc` | TAO, `ETN2` with eleven tracks of mode `03`; `RELO`, `TOCT`, `DINF`; sectors |
| 8 | `fvm-003` | 671,082,776 | `e9b08137cbaf4ab6` | Two DAO sessions, each one data track of mode `00`; sectors |
| 9 | `bejeweled-2.iso_202608` | 671,566,120 | `b6ffe2b9284b8b56` | Two DAO sessions: 17 audio tracks, then one data track of mode `02`; sectors |
| 10 | `Hardwarism001_FullCDImage` | 805,445,022 | `ad511ac5b8e1afdc` | Mixed mode: data track of mode `00`, seven audio tracks with stored pregaps, `CDTX`; sectors |
| 11 | `MidtownMadness2` | 467,235,158 | `eafa21ffb7b9bfa5` | Data track and audio track with 352,800 bytes in no track; sectors |
| 12 | `sony-playstationx-pad-babak1-with-nrg` (`1-Xtreme (USA).nrg`) | 530,421,170 | `cd7289848d29d844` | Mode `02` data track, seven audio tracks, bytes in no track between all of them; sectors |
| 13 | `21_20260130_20260130_1341` (fourth file) | 280,236,988 | `7c2bb7674472a1fe` | DAO, 17 tracks of mode `03` (Video CD); sectors |
| 14 | `usbsoundscans1-63` (vol. 21) | 313,808,700 | `5c1f0dfbb16c3790` | DAO, one track of mode `05`; sectors |
| 15 | `marsha-lifter-education-graphics` | 188,628,348 | `e9bee16db4687739` | DAO, one track of mode `0F`; sectors |
| 16 | `the-story-of-hongmao-and-lantu` (disc 10) | 485,651,764 | `d85c8b2be9117f88` | DAO, five tracks of mode `11`; sectors |
| 17 | `armonia-10-la-primerisima-1998-535` | 527,836,106 | `265e71d43a5f463d` | DAO, twelve tracks of mode `10`; sectors |
| 18 | `1LenovoThinkPadWin8Recovery` (Operating System 1) | 4,534,112,412 | `f41bfb9acd0915d8` | DVD image above 4 GiB: session type `40`, `MTYP` `1C`, no pregap, 64-bit offsets in use |
| 19 | `sniper-v-1.10.277097-b-277097-arm-7-arm-64` | 599,763,427 | `1663501fb7466a49` | Audio disc with `AFNM` and `VOLM` chunks |
| 20 | `20gzswx001` | 705,579,164 | `ebab99157262fc05` | CD-sized image with `MTYP` `1C`, session type `00` and no pregap; the single-session image whose header byte 19 is `00`; sectors |
| 21 | `Coil-SpoilerWorkshopSVCD` | 738,365,430 | `70d6a690985ab039` | DAO, second track with a stored pregap of 302 sectors |
| 22 | `ex-7-cd` | 548,214,956 | `dc8a41d710f52a17` | The largest chain: 5,248 bytes for one data track and 88 audio tracks |

### 9.3 Sector checks

Each line was read from the image at the stated file offset.

| Image | Offset | Found |
|--:|--:|:--|
| 4 | 339,968 = 307,200 + 16 × 2048 | `01 43 44 30 30 31` (`CD001`): the volume descriptor is at sector 16 of the track's data, so the stored sectors of mode `02` are user data only. Offset 0: zero bytes. |
| 5 | 32,768 | `CD001`: a TAO track starts at offset 0 without pregap. Volume size 26,101 sectors = track length. |
| 7 | 37,376 = 16 × 2336 | `00 00 09 00 00 00 09 00 01 43 44 30 30 31`: sub-header, then user data at offset 8. |
| 7 | 1,401,600 (track 2's offset) | `01 00 60 00 01 00 60 00`, then zero: the first sector of the second track follows the last sector of the first directly. |
| 8 | 339,968 | `CD001`, volume size 68,304 sectors, root directory at sector 29. |
| 8 | 140,840,960 = 140,808,192 + 32,768 | `CD001`, volume size 258,923 sectors (the track's length), root directory at sector 79,873. The 4,096 bytes at 140,500,992 (the pregap) are zero. |
| 9 | 377,556,080 = 377,523,312 + 32,768 | `CD001`, root directory at sector 171,650; the session starts at LBA 171,631. |
| 10 | 119,556,096 (track 2's *index 0*) | 2,352 zero bytes. At 126,964,896 (3,000 sectors into the track): `82 F4 1B F4 3F F0 D4 EF …`, samples that are smooth only as little-endian. |
| 12 | 92,735,488 and 152,550,640 | Zero bytes: the regions in no track are silence. |
| 13 | 387,776 = 350,400 + 16 × 2336 | `00 00 09 00 00 00 09 00 01 43 44 30 30 31`. |
| 14 | 390,432 = 352,800 + 16 × 2352 | Sync pattern, `00 02 16 01`. Offset 0: sync pattern, `00 00 00 01`, zero user data. |
| 15 | 406,368 = 367,200 + 16 × 2448 | Sync pattern, `00 02 16 01`, `01 43 44 30 30 31` at offset 16; bytes 2352 to 2447 hold only `00` and `40`. |
| 16 | 406,368 | Sync pattern, `00 02 16 02`, sub-header `00 00 09 00 00 00 09 00`, `CD001` at offset 25. |
| 17 | 12,607,200 = 367,200 + 5,000 × 2448 | 2,352 bytes of samples, then 96 bytes holding only `00` and `40`; the next sector starts with samples again. |
| 20 | 32,768 | `CD001`: no pregap when *index 0* = *index 1* = 0. |

## 10. Entries

All of this section is **DECISION**. It is the entry model of `cue.md`
section 10, applied to an image that needs no second file.

### 10.1 One entry per track

Each track that can be read becomes one entry, in track order. Tracks
that cannot be read produce no entry (section 10.4). There are no
directories and no session level. Nothing else is listed: not the
chain, not the pregaps, not the bytes in no track.

The entry's path is the container's prefix, a slash, and the entry
name.

The container format is named `Nrg`. Its conventional extension is
`nrg`.

### 10.2 Data tracks

- Name: `track`, the track number as two decimal digits, `.img`. Track 2
  is `track02.img`.
- Content: the cooked data of the track (section 6.3): 2048 bytes per
  stored sector, from the track's *start* (index 1) to its *end*.
- For a track with a stored sector size of 2048 the content is the bytes
  of the range unchanged. A reader may hand out that range of the image
  without copying it.
- The name carries no claim about the content and the extension `img`
  is no hint: the track is recognised by content like any other file,
  as ISO 9660, HFS, UDF, a partition map or nothing. The reasons are
  those of `cue.md` section 10.2. Tracks are not named after volume
  labels.

### 10.3 Audio tracks

- Name: `track`, the two-digit track number, `.pcm`. Track 3 is
  `track03.pcm`.
- Content: the track's samples as stored, 16-bit signed, little-endian,
  two channels interleaved left then right, 44,100 sample pairs per
  second; 2352 bytes per sector. There is no header of any kind. From a
  track of 2448-byte sectors the first 2352 bytes of each sector are
  taken and the 96 sub-channel bytes dropped.
- Nothing is decoded, resampled, byte-swapped or wrapped. A `.pcm` entry
  is not a container and is not descended into. This holds whatever the
  samples look like: content detection is not run on the entry, so
  samples that happen to form an ISO 9660 volume or a gzip stream are
  still listed as one file.

### 10.4 Tracks that cannot be read

A track of section 8.3 that is not readable, and every track of an
inconsistent layout (section 8.2), produces no entry. The reader reports
what it left out the way the CUE/BIN reader does: one message that names
the first problem and counts the rest. A message names the track number
and the reason: an unknown mode code, a sector size that is not
supported, a track that starts beyond the data area, an empty track, an
inconsistent layout, an entry over a size limit.

An image all of whose tracks are left out opens successfully and lists
nothing.

### 10.5 Pregaps and bytes in no track

The sectors between a track's *index 0* and *index 1* are in no entry,
and neither are the bytes that no DAO entry covers (section 7.2).

- Before a data track they are filler, and a file system starts at
  index 1. This is what makes `track01.img` of the common data-CD image
  start with the volume's first sector and not with 150 sectors of
  zeros.
- Before an audio track they are usually silence. Occasionally they
  hold audio (the pregap of 300 sectors in image 2 of section 9.2 is a
  hidden passage before track 1). That audio is not listed. This is
  accepted for a reader whose purpose is file extraction, as for
  CUE/BIN.

### 10.6 Metadata

Entries carry no mode string, no compression information and no
type/creator codes. The entry's size is the size of its content. The
catalogue number, ISRC codes, CD-TEXT, `AFNM` names, control flags and
disc addresses are not surfaced: the entry metadata has no fields for
them.

### 10.7 Multi-session images

Sessions are not modelled. A multi-session image lists its tracks in
order, numbered through, exactly like the cue sheet of the same disc
with one file per track would.

- A data track of the **first** session is a complete file system image
  that starts at its own sector 0. It opens as the file system it holds.
- A data track of a **later** session holds a file system whose block
  addresses are disc addresses (section 7.5). Handed out on its own it
  is the case that `docs/USAGE.md` describes for CUE/BIN under "ISO 9660
  CD-ROM Images": the existing ISO 9660 reader opens such a track and
  reads the files recorded in it; files that the session kept from an
  earlier session lie outside the track, are left out and are reported.
  The NRG reader does nothing special for this. It must hand out the
  track's cooked data starting at index 1, unpadded, so that the track
  looks the same as the track file of a cue sheet.
- The NRG reader does not rebuild a disc-wide image with the session
  gaps filled in, and does not pass the session's LBA on. Section 14
  notes what that would make possible.
- The image as a whole is never given to the ISO 9660 reader. Its
  multi-session search, which looks for later sessions inside one image
  under absolute addressing, therefore never runs on NRG data, where the
  session gaps are missing and that addressing does not hold.

## 11. Worked examples

### 11.1 The smallest chain (image 5 of section 9.2)

A TAO image with one data track. The file has 53,454,908 bytes. Its last
60 bytes, with their file offsets:

    032fa800  45 54 4e 32 00 00 00 20 00 00 00 00 00 00 00 00
    032fa810  00 00 00 00 03 2f a8 00 00 00 00 00 00 00 00 00
    032fa820  00 00 00 00 00 00 65 f5 45 4e 44 21 00 00 00 00
    032fa830  4e 45 52 35 00 00 00 00 03 2f a8 00

- Footer: `NER5` at file size − 12; chain offset `00 00 00 00 03 2F A8
  00` = 53,454,848. *E* = 53,454,896; the chain is 48 bytes.
- Chunk `ETN2`, length 32: one entry. Offset 0; length 53,454,848; mode
  0; LBA 0; zero; sector count `65 F5` = 26,101.
- Chunk `END!`, length 0, ends at *E*.
- Track 1: mode `00`, *S* = 2048, range 0 to 53,454,848: 26,101
  sectors. Entry `track01.img`, 53,454,848 bytes, identical to the first
  53,454,848 bytes of the file. It has `CD001` at offset 32,769.

### 11.2 A version 1 image (image 1)

The file has 2,630,472 bytes. Its last 136 bytes:

    002822c0  45 54 4e 46 00 00 00 64 00 00 00 00 00 18 18 80
    002822d0  00 00 00 03 00 00 00 00 00 00 00 00 00 18 18 80
    002822e0  00 02 ac 60 00 00 00 03 00 00 02 a4 00 00 00 00
    002822f0  00 1a c4 e0 00 02 ac 60 00 00 00 03 00 00 02 ef
    00282300  00 00 00 00 00 1d 71 40 00 02 ac 60 00 00 00 03
    00282310  00 00 03 3a 00 00 00 00 00 20 1d a0 00 08 05 20
    00282320  00 00 00 03 00 00 03 85 00 00 00 00 53 49 4e 46
    00282330  00 00 00 04 00 00 00 05 45 4e 44 21 00 00 00 00
    00282340  4e 45 52 4f 00 28 22 c0

- Footer: `NERO` at file size − 8; chain offset `00 28 22 C0` =
  2,630,336.
- Chunk `ETNF`, length 100: five entries of 20 bytes. Chunk `SINF`:
  5. Chunk `END!`.

| Track | Offset | Length | Mode | Sector number | Sectors of 2336 bytes | Entry | Entry size | SHA-256 |
|--:|--:|--:|--:|--:|--:|:--|--:|:--|
| 1 | 0 | 1,579,136 | 3 | 0 | 676 | `track01.img` | 1,384,448 | `c179c0655eab6ff9824083d49a9719222e5aa45afb86a9a51095ab53c78d450f` |
| 2 | 1,579,136 | 175,200 | 3 | 676 | 75 | `track02.img` | 153,600 | `5ae57d12f9e6ad72cb987ef88c5bad3cf3082c912fb7d772191ac871ea2d7ea2` |
| 3 | 1,754,336 | 175,200 | 3 | 751 | 75 | `track03.img` | 153,600 | the same |
| 4 | 1,929,536 | 175,200 | 3 | 826 | 75 | `track04.img` | 153,600 | the same |
| 5 | 2,104,736 | 525,600 | 3 | 901 | 225 | `track05.img` | 460,800 | `b9a804dc858005593f028c04f2d125902894fc30b61d044deaa866ccb4d581ef` |

`track01.img` is an ISO 9660 volume (`CD001` at offset 32,769, volume
size 676 sectors). Tracks 2 to 5 consist of Form 2 sectors only; their
entries hold the first 2048 bytes of each sector's 2324 and are of no
use, as section 6.3 says. The whole file's SHA-256 is
`4d7f7e3e7cd73dc10ac1d81bb76e3706ce0d2a27aa2ad76dd7c9ceaee3631135`.

### 11.3 A DAO image with one data track (image 3)

The file has 18,874,500 bytes. Its last 132 bytes:

    01200000  43 55 45 58 00 00 00 20 41 00 00 00 ff ff ff 6a
    01200010  41 01 00 00 ff ff ff 6a 41 01 01 00 00 00 00 00
    01200020  41 aa 01 00 00 00 23 6a 44 41 4f 58 00 00 00 40
    01200030  00 00 00 40 00 00 00 00 00 00 00 00 00 00 00 00
    01200040  00 00 00 01 01 01 00 00 00 00 00 00 00 00 00 00
    01200050  00 00 08 00 00 00 00 01 00 00 00 00 00 00 00 00
    01200060  00 00 00 00 00 04 b0 00 00 00 00 00 01 20 00 00
    01200070  45 4e 44 21 00 00 00 00 4e 45 52 35 00 00 00 00
    01200080  01 20 00 00

- Footer: `NER5`, chain offset 18,874,368.
- Chunk `CUEX`, length 32, four entries: track `00` index `00` at LBA
  −150; track `01` index `00` at −150; track `01` index `01` at 0;
  track `AA` index `01` at 9,066. Skipped.
- Chunk `DAOX`, length 64. Header: length again (64); 13 zero bytes; 0;
  session type `00`; last-session byte `01`; first track 1; last track
  1. One entry (64 − 22 = 42): ISRC zero; *S* = `08 00` = 2048; *M* =
  `00`; `00 00 01`; *index 0* = 0; *index 1* = `04 B0 00` = 307,200;
  *end* = `01 20 00 00` = 18,874,368.
- Chunk `END!`.
- Track 1: range 307,200 to 18,874,368: 9,066 sectors (the same number
  as the `AA` cue point). Entry `track01.img`, 18,567,168 bytes. The
  first 307,200 bytes of the file are in no entry.

### 11.4 Two data sessions (image 8)

The file has 671,082,776 bytes. Its last 280 bytes:

    27ffe800  43 55 45 58 00 00 00 20 41 00 00 00 ff ff ff 6a
    27ffe810  41 01 00 00 ff ff ff 6a 41 01 01 00 00 00 00 00
    27ffe820  41 aa 01 00 00 01 0b 66 44 41 4f 58 00 00 00 40
    27ffe830  00 00 00 40 00 00 00 00 00 00 00 00 00 00 00 00
    27ffe840  00 00 20 00 01 01 00 00 00 00 00 00 00 00 00 00
    27ffe850  00 00 08 00 00 00 00 01 00 00 00 00 00 00 00 00
    27ffe860  00 00 00 00 00 04 b0 00 00 00 00 00 08 5f e0 00
    27ffe870  43 55 45 58 00 00 00 20 41 00 00 00 00 01 37 58
    27ffe880  41 02 00 00 00 01 37 58 41 02 01 00 00 01 37 ee
    27ffe890  41 aa 01 00 00 05 2b 59 44 41 4f 58 00 00 00 40
    27ffe8a0  00 00 00 40 00 00 00 00 00 00 00 00 00 00 00 00
    27ffe8b0  00 00 20 01 02 02 00 00 00 00 00 00 00 00 00 00
    27ffe8c0  00 00 08 00 00 00 00 01 00 00 00 00 08 5f e0 00
    27ffe8d0  00 00 00 00 08 64 90 00 00 00 00 00 27 ff e8 00
    27ffe8e0  53 49 4e 46 00 00 00 04 00 00 00 01 53 49 4e 46
    27ffe8f0  00 00 00 04 00 00 00 01 4d 54 59 50 00 00 00 04
    27ffe900  00 00 00 01 45 4e 44 21 00 00 00 00 4e 45 52 35
    27ffe910  00 00 00 00 27 ff e8 00

Chain offset 671,082,496. Chunks: `CUEX`, `DAOX`, `CUEX`, `DAOX`,
`SINF` (1), `SINF` (1), `MTYP` (1), `END!`.

| | Track 1 | Track 2 |
|:--|--:|--:|
| From | first `DAOX`, entry 1 | second `DAOX`, entry 1 |
| Header bytes 18 to 21 | `20 00 01 01` | `20 01 02 02` |
| *S*, *M* | 2048, `00` | 2048, `00` |
| *index 0* | 0 | 140,500,992 |
| *index 1* | 307,200 | 140,808,192 |
| *end* | 140,500,992 | 671,082,496 |
| Sectors | 68,454 | 258,923 |
| Cue points (index 0, index 1, next) | −150, 0, 68,454 | 79,704, 79,854, 338,777 |
| Entry | `track01.img` | `track02.img` |
| Entry size | 140,193,792 | 530,274,304 |

- The first session ends at LBA 68,454 and the second begins at 79,704:
  11,250 sectors that are not in the file, where track 2's pregap
  follows track 1's last sector directly.
- `track01.img` has `CD001` at offset 32,769 and its root directory at
  sector 29: a self-contained volume.
- `track02.img` has `CD001` at offset 32,769 and its root directory at
  sector 79,873, which is sector 79,873 − 79,854 = 19 of the entry
  (byte 38,912). Opening it is the case of section 10.7.

### 11.5 Mixed mode with bytes in no track (image 12)

The first three of the eight `DAOX` entries:

| Track | *S* | *M* | *index 0* | *index 1* | *end* | Sectors | Entry | Entry size |
|--:|--:|--:|--:|--:|--:|--:|:--|--:|
| 1 | 2048 | `02` | 0 | 307,200 | 92,735,488 | 45,131 | `track01.img` | 92,428,288 |
| 2 | 2352 | `07` | 93,395,488 | 93,395,488 | 152,550,640 | 25,151 | `track02.pcm` | 59,155,152 |
| 3 | 2352 | `07` | 152,903,440 | 152,903,440 | 218,921,728 | 28,069 | `track03.pcm` | 66,018,288 |

- Between tracks 1 and 2 lie 93,395,488 − 92,735,488 = 660,000 bytes in
  no track; between tracks 2 and 3, 352,800 bytes. Both are zero.
- The image's `CUEX` chunk puts track 2's index 1 at LBA 45,431 and
  track 3's at 70,732, a distance of 25,301 sectors, 150 more than the
  `DAOX` entry's 25,151. The reader follows `DAOX`.
- The sequence of section 8.2 is 0, 307,200, 92,735,488, 93,395,488,
  93,395,488, 152,550,640, 152,903,440, …: it never decreases.

## 12. Test vectors

### 12.1 Building blocks

The purpose-built images use the sector contents of the image in
`cue.md` section 11.3, so that the expected entries have the digests
given there.

- **Data sector D(*i*)**, *i* from 0 to 19: 2048 bytes, the 10-character
  ASCII text `T1-`, *i* as six decimal digits, `|` (for example
  `T1-000007|`), repeated and cut off at 2048 bytes.
- **Audio sector A(*i*)**, *i* from 0 to 29: 588 sample pairs; pair *k*
  (0 to 587) is the left sample *v* and the right sample −*v*, where *v*
  = (*i* × 588 + *k*) modulo 30,000; both as signed 16-bit
  little-endian. (A(0) starts `00 00 00 00 01 00 FF FF`.)
- **Mode 2 sector X(*i*)**, *i* from 0 to 11: 2336 bytes.
  - *i* = 7 is a Form 2 sector: sub-header `00 00 20 00 00 00 20 00`;
    2324 bytes, the text `F2-000007|` repeated and cut off; 4 zero
    bytes.
  - All others are Form 1 sectors: sub-header `00 00 08 00 00 00 08
    00`; 2048 bytes, the text `T3-`, *i* as six digits, `|`, repeated
    and cut off; 280 zero bytes.
- **Raw Mode 1 sector** at LBA *n* with user data *u*: sync pattern;
  header with the address *n* + 150 as binary-coded decimal minute,
  second, frame, and mode `01`; *u*; 288 zero bytes.
- **Raw Mode 2 sector** at LBA *n* with body *x* (2336 bytes): sync
  pattern; the same header with mode `02`; *x*.
- **Sub-channel filler**: 96 bytes `53` (the letter `S`).

Expected contents, by name:

| Name | Content | Size | SHA-256 |
|:--|:--|--:|:--|
| *T1* | D(0) … D(19) | 40,960 | `29a24186333aed9ef03ffbfd4ab452d397d634c03d967a27b30895f8575aaf3f` |
| *AU* | A(0) … A(29) | 70,560 | `e7461c0534743f4e9f7833e84c20327dd9959436e44bcf9acf10fa5d0dac83d5` |
| *T3* | bytes 8 to 2055 of X(0) … X(11) | 24,576 | `25a6f1093fdf46c5acac4800b2b485dfe079845a8afdf784a37150805bdd7f98` |

### 12.2 Image A: version 2, disc-at-once, modes `00`, `07`, `03`

482,224 bytes, SHA-256
`bb401e8fec56a9a4556a5bf165d7f75c70fe177ffb358ad71add20b73e12db5f`.

Data area, 481,952 bytes:

| Bytes | Content |
|:--|:--|
| 0 to 307,199 | 150 sectors of 2048 zero bytes (pregap of track 1) |
| 307,200 to 348,159 | D(0) … D(19) |
| 348,160 to 371,679 | 10 sectors of 2352 zero bytes (pregap of track 2) |
| 371,680 to 442,239 | A(0) … A(29) |
| 442,240 to 453,919 | 5 sectors of 2336 bytes, each `00 00 08 00 00 00 08 00` and 2328 zero bytes (pregap of track 3) |
| 453,920 to 481,951 | X(0) … X(11) |

Chain and footer, 272 bytes:

    00075aa0  43 55 45 58 00 00 00 40 41 00 00 00 ff ff ff 6a
    00075ab0  41 01 00 00 ff ff ff 6a 41 01 01 00 00 00 00 00
    00075ac0  01 02 00 00 00 00 00 14 01 02 01 00 00 00 00 1e
    00075ad0  41 03 00 00 00 00 00 3c 41 03 01 00 00 00 00 41
    00075ae0  41 aa 01 00 00 00 00 4d 44 41 4f 58 00 00 00 94
    00075af0  00 00 00 94 00 00 00 00 00 00 00 00 00 00 00 00
    00075b00  00 00 20 01 01 03 00 00 00 00 00 00 00 00 00 00
    00075b10  00 00 08 00 00 00 00 01 00 00 00 00 00 00 00 00
    00075b20  00 00 00 00 00 04 b0 00 00 00 00 00 00 05 50 00
    00075b30  00 00 00 00 00 00 00 00 00 00 00 00 09 30 07 00
    00075b40  00 01 00 00 00 00 00 05 50 00 00 00 00 00 00 05
    00075b50  ab e0 00 00 00 00 00 06 bf 80 00 00 00 00 00 00
    00075b60  00 00 00 00 00 00 09 20 03 00 00 01 00 00 00 00
    00075b70  00 06 bf 80 00 00 00 00 00 06 ed 20 00 00 00 00
    00075b80  00 07 5a a0 53 49 4e 46 00 00 00 04 00 00 00 03
    00075b90  4d 54 59 50 00 00 00 04 00 00 00 01 45 4e 44 21
    00075ba0  00 00 00 00 4e 45 52 35 00 00 00 00 00 07 5a a0

Expected parse:

- Version 2; chain offset 481,952; chunks `CUEX` (64 bytes, 8 entries),
  `DAOX` (148 bytes, 3 entries), `SINF` (3), `MTYP` (1), `END!`.

| Track | *S* | *M* | *index 0* | *index 1* | *end* | Entry | Content |
|--:|--:|--:|--:|--:|--:|:--|:--|
| 1 | 2048 | `00` | 0 | 307,200 | 348,160 | `track01.img` | *T1* |
| 2 | 2352 | `07` | 348,160 | 371,680 | 442,240 | `track02.pcm` | *AU* |
| 3 | 2336 | `03` | 442,240 | 453,920 | 481,952 | `track03.img` | *T3* |

`cd-info` 2.2.0 opens this image and lists three tracks at LBA 0, 30
and 65 with the lead-out at 77. CONFIRMED (crafted).

Field positions for the variants below: footer offset field at file
offset `75BA8`; `SINF` length at `75B88`; `END!` identifier at `75B9C`;
track 1's *S* at `75B12`, *M* at `75B14`, *index 1* at `75B20`; track
2's *M* at `75B3E`, *index 0* at `75B42`; track 3's *S* at `75B66`, *M*
at `75B68`, *index 1* at `75B74`, *end* at `75B7C` (all hexadecimal).

| # | Change to image A | Expected |
|--:|:--|:--|
| 1 | Footer offset set to 482,224 (the file size), to 0, or to sixteen `F` digits | Not an NRG image. |
| 2 | Footer offset set to 481,953 | Not an NRG image (the first identifier would contain a zero byte). |
| 3 | `END!` replaced by `XXXX` | Not an NRG image (the chain reaches the footer without `END!`). |
| 4 | `SINF` length set to `FF FF FF FF` | Not an NRG image (chunk beyond the chain). |
| 5 | The last byte removed, or one zero byte appended | Not an NRG image. |
| 6 | `NER5` replaced by `NERO` | Not an NRG image (the version 1 footer is the last 8 bytes, which do not start with `NERO`). |
| 7 | 251 chunks `JUNK` of length 0 inserted before `END!` (256 chunks in all) | The same three entries. |
| 8 | 252 such chunks inserted (257 in all) | Not an NRG image. |
| 9 | Track 2's *M* set to `09` | `track01.img` and `track03.img` unchanged; track 2 reported (unknown mode). |
| 10 | Track 3's *S* set to 0 | Tracks 1 and 2 unchanged; track 3 reported (sector size). |
| 11 | Track 1's *M* set to `07` | Track 1 reported (an audio track cannot have 2048-byte sectors); tracks 2 and 3 unchanged. |
| 12 | Track 3's *M* set to `02` | Unchanged output: *S* = 2336 decides. |
| 13 | Track 1's *S* set to 2352 (`09 30`) | `track01.img` is 34,816 bytes (17 sectors of 2352 bytes without sync pattern, 2048 bytes taken at offset 16 of each), SHA-256 `3b176a3cd893cfd37db51dad5b4a32bdee848d44ab1c8127cf137f489e29a7f7`; tracks 2 and 3 unchanged. |
| 14 | Track 3's *end* set to `7F FF FF FF FF FF FF FF` | Unchanged output: the end is clamped to the chain offset. |
| 15 | Track 3's *end* set to 481,951 | `track03.img` is 22,528 bytes (11 sectors; the partial twelfth is ignored), SHA-256 `ee7514fbca6ee143c047819498f85611b1a9c97258ee135d621f37c4846ef38a`. |
| 16 | Track 3's *index 1* and *end* both set to 524,288 | Tracks 1 and 2 unchanged; track 3 reported (starts beyond the data area). |
| 17 | Track 2's *index 0* set to 348,159 | Inconsistent layout: no entries, three tracks reported. |
| 18 | Track 1's *index 1* set to 348,161 | Inconsistent layout: no entries, three tracks reported. |
| 19 | The whole `DAOX` chunk (156 bytes from `75AE8`) inserted a second time directly after itself | Six tracks; the fourth starts before the third ends: inconsistent layout, no entries, six tracks reported. |

### 12.3 Image B: version 1, track-at-once

139,648 bytes, SHA-256
`d45ed70fb4bcd3d7c8cc5bc82cc7168fc9422e06d4a77e00922283cfde5fa7cc`.

Data area, 139,552 bytes: D(0) … D(19) (bytes 0 to 40,959); X(0) …
X(11) (40,960 to 68,991); A(0) … A(29) (68,992 to 139,551).

Chain and footer, 96 bytes:

    00022120  45 54 4e 46 00 00 00 3c 00 00 00 00 00 00 a0 00
    00022130  00 00 00 00 00 00 00 00 00 00 00 00 00 00 a0 00
    00022140  00 00 6d 80 00 00 00 03 00 00 00 14 00 00 00 00
    00022150  00 01 0d 80 00 01 13 a0 00 00 00 07 00 00 00 20
    00022160  00 00 00 00 53 49 4e 46 00 00 00 04 00 00 00 03
    00022170  45 4e 44 21 00 00 00 00 4e 45 52 4f 00 02 21 20

Expected parse: version 1; chain offset 139,552; chunks `ETNF` (60
bytes, 3 entries), `SINF` (3), `END!`.

| Track | Offset | Length | *M* | *S* | Entry | Content |
|--:|--:|--:|--:|--:|:--|:--|
| 1 | 0 | 40,960 | `00` | 2048 | `track01.img` | *T1* |
| 2 | 40,960 | 28,032 | `03` | 2336 | `track02.img` | *T3* |
| 3 | 68,992 | 70,560 | `07` | 2352 | `track03.pcm` | *AU* |

`cd-info` 2.2.0 aborts on this image with a failed assertion (section
13). CONFIRMED (crafted).

### 12.4 Image C: two sessions, raw and sub-channel modes, an unknown chunk

233,219 bytes, SHA-256
`ac97914aca3f08f591301e5c34ead7a709f30029f340255984dc049a54765b94`.

Data area, 232,744 bytes:

| Bytes | Content |
|:--|:--|
| 0 to 4,703 | 2 raw Mode 1 sectors at LBA −2 and −1 with all-zero user data (pregap of track 1) |
| 4,704 to 51,743 | 20 raw Mode 1 sectors at LBA 0 to 19 with user data D(0) … D(19) |
| 51,744 to 52,743 | 1,000 bytes `EE`, in no track |
| 52,744 to 126,183 | 30 sectors of 2448 bytes: A(*i*) and the sub-channel filler |
| 126,184 to 155,559 | 12 sectors of 2448 bytes: the raw Mode 2 sector at LBA 11,300 + *i* with body X(*i*), and the sub-channel filler |
| 155,560 to 204,519 | 20 sectors of 2448 bytes: the raw Mode 1 sector at LBA 11,312 + *i* with user data D(*i*), and the sub-channel filler |
| 204,520 to 232,743 | 12 raw Mode 2 sectors at LBA 11,332 + *i* with body X(*i*) |

Chain and footer, 475 bytes:

    00038d28  43 55 45 58 00 00 00 30 41 00 00 00 ff ff ff fe
    00038d38  41 01 00 00 ff ff ff fe 41 01 01 00 00 00 00 00
    00038d48  01 02 00 00 00 00 00 14 01 02 01 00 00 00 00 14
    00038d58  01 aa 01 00 00 00 00 32 44 41 4f 58 00 00 00 6a
    00038d68  00 00 00 6a 00 00 00 00 00 00 00 00 00 00 00 00
    00038d78  00 00 00 00 01 02 00 00 00 00 00 00 00 00 00 00
    00038d88  00 00 09 30 05 00 00 01 00 00 00 00 00 00 00 00
    00038d98  00 00 00 00 00 00 12 60 00 00 00 00 00 00 ca 20
    00038da8  00 00 00 00 00 00 00 00 00 00 00 00 09 90 10 00
    00038db8  00 01 00 00 00 00 00 00 ce 08 00 00 00 00 00 00
    00038dc8  ce 08 00 00 00 00 00 01 ec e8 5a 5a 5a 5a 00 00
    00038dd8  00 0d 75 6e 6b 6e 6f 77 6e 20 63 68 75 6e 6b 43
    00038de8  55 45 58 00 00 00 40 41 00 00 00 00 00 2c 24 41
    00038df8  03 00 00 00 00 2c 24 41 03 01 00 00 00 2c 24 41
    00038e08  04 00 00 00 00 2c 30 41 04 01 00 00 00 2c 30 41
    00038e18  05 00 00 00 00 2c 44 41 05 01 00 00 00 2c 44 41
    00038e28  aa 01 00 00 00 2c 50 44 41 4f 58 00 00 00 94 00
    00038e38  00 00 94 00 00 00 00 00 00 00 00 00 00 00 00 00
    00038e48  00 20 01 03 05 00 00 00 00 00 00 00 00 00 00 00
    00038e58  00 09 90 11 00 00 01 00 00 00 00 00 01 ec e8 00
    00038e68  00 00 00 00 01 ec e8 00 00 00 00 00 02 5f a8 00
    00038e78  00 00 00 00 00 00 00 00 00 00 00 09 90 0f 00 00
    00038e88  01 00 00 00 00 00 02 5f a8 00 00 00 00 00 02 5f
    00038e98  a8 00 00 00 00 00 03 1e e8 00 00 00 00 00 00 00
    00038ea8  00 00 00 00 00 09 30 06 00 00 01 00 00 00 00 00
    00038eb8  03 1e e8 00 00 00 00 00 03 1e e8 00 00 00 00 00
    00038ec8  03 8d 28 53 49 4e 46 00 00 00 04 00 00 00 02 53
    00038ed8  49 4e 46 00 00 00 04 00 00 00 03 4d 54 59 50 00
    00038ee8  00 00 04 00 00 00 01 45 4e 44 21 00 00 00 00 4e
    00038ef8  45 52 35 00 00 00 00 00 03 8d 28

Expected parse: version 2; chain offset 232,744; chunks `CUEX` (48),
`DAOX` (106, 2 entries), `ZZZZ` (13, skipped), `CUEX` (64), `DAOX` (148,
3 entries), `SINF` (2), `SINF` (3), `MTYP` (1), `END!`. The chunks are
not aligned: the `ZZZZ` chunk has an odd length.

| Track | *S* | *M* | *index 0* | *index 1* | *end* | Entry | Content |
|--:|--:|--:|--:|--:|--:|:--|:--|
| 1 | 2352 | `05` | 0 | 4,704 | 51,744 | `track01.img` | *T1* |
| 2 | 2448 | `10` | 52,744 | 52,744 | 126,184 | `track02.pcm` | *AU* |
| 3 | 2448 | `11` | 126,184 | 126,184 | 155,560 | `track03.img` | *T3* |
| 4 | 2448 | `0F` | 155,560 | 155,560 | 204,520 | `track04.img` | *T1* |
| 5 | 2352 | `06` | 204,520 | 204,520 | 232,744 | `track05.img` | *T3* |

The tracks are numbered through although they come from two `DAOX`
chunks. No entry contains a byte `EE` or a run of the letter `S`.

Variant: with the mode bytes of tracks 1, 4 and 5 set to `02`, `02` and
`00` (wrong for the sectors they hold) the output is unchanged, because
every sector has a sync pattern and its own mode byte decides.

`cd-info` 2.2.0 aborts on this image with a failed assertion because
its first cue point is not at LBA −150. CONFIRMED (crafted).

### 12.5 Image D: version 1, disc-at-once

418,894 bytes, SHA-256
`06fdc93f479547a17089f247c8144e11b67fee343fefe1fae6dc22369ecc46c7`.
This image follows the `DAOI` layout of section 5.1, which is
REFERENCE-ONLY; no real image of the kind was available.

Data area, 418,720 bytes: 150 sectors of 2048 zero bytes (0 to
307,199); D(0) … D(19) (307,200 to 348,159); A(0) … A(29) (348,160 to
418,719).

Chain and footer, 174 bytes:

    000663a0  43 55 45 53 00 00 00 30 41 00 00 00 00 00 00 00
    000663b0  41 01 00 00 00 00 00 00 41 01 01 00 00 00 02 00
    000663c0  01 02 00 00 00 00 02 20 01 02 01 00 00 00 02 20
    000663d0  01 aa 01 00 00 00 02 50 44 41 4f 49 00 00 00 52
    000663e0  00 00 00 52 00 00 00 00 00 00 00 00 00 00 00 00
    000663f0  00 00 00 01 01 02 00 00 00 00 00 00 00 00 00 00
    00066400  00 00 08 00 00 00 00 01 00 00 00 00 00 04 b0 00
    00066410  00 05 50 00 00 00 00 00 00 00 00 00 00 00 00 00
    00066420  09 30 07 00 00 01 00 05 50 00 00 05 50 00 00 06
    00066430  63 a0 53 49 4e 46 00 00 00 04 00 00 00 02 45 4e
    00066440  44 21 00 00 00 00 4e 45 52 4f 00 06 63 a0

Expected parse: version 1; chain offset 418,720; chunks `CUES` (48,
skipped), `DAOI` (82 bytes: 22 + 2 × 30), `SINF` (2), `END!`.

| Track | *S* | *M* | *index 0* | *index 1* | *end* | Entry | Content |
|--:|--:|--:|--:|--:|--:|:--|:--|
| 1 | 2048 | `00` | 0 | 307,200 | 348,160 | `track01.img` | *T1* |
| 2 | 2352 | `07` | 348,160 | 348,160 | 418,720 | `track02.pcm` | *AU* |

### 12.6 The two libcdio images

`videocd.nrg`: section 11.2.

`p1.nrg` (1,063,470 bytes, SHA-256
`224f08c2330fc1091649f9ca6bd7ce44e3522b571967237a4440e4667fa015d5`):
version 2, chain offset 1,063,104, chunks `CUEX`, `DAOX`, `CDTX` (144
bytes), `SINF` (2), `MTYP` (1), `END!`.

| Track | *S* | *M* | *index 0* | *index 1* | *end* | Entry | Size | SHA-256 |
|--:|--:|--:|--:|--:|--:|:--|--:|:--|
| 1 | 2352 | `07` | 0 | 705,600 | 882,000 | `track01.pcm` | 176,400 | `fd6f479534cdd14635e88dfedf25c3859c01062b645f2a85570f20451b4a95bc` |
| 2 | 2352 | `07` | 882,000 | 1,058,400 | 1,063,104 | `track02.pcm` | 4,704 | `2b59c3dc353d8dc0b81e3d88dbc92c2301d686b96c02bc2fb0f2e5be037b3d5a` |

Track 1 has a stored pregap of 300 sectors and track 2 one of 75; both
are in no entry.

## 13. Observed implementation gaps

Nothing in this section is to be imitated.

- **libcdio** (development head and `cd-info` 2.2.0):
  - takes the kind of every track of a DAO session from the first
    track's mode code, so an image with a data track and audio tracks is
    listed as all data (CONFIRMED (crafted), image A);
  - requires the first `CUEX` entry to be at LBA −150 and aborts
    otherwise (CONFIRMED (crafted), image C), which every DVD image and
    every later session violates;
  - aborts with a failed assertion when an `ETNF` entry's offset is not
    its sector number times its sector size (CONFIRMED (crafted), image
    B): an abort on untrusted input;
  - reads the positions of a `CUES` chunk as 32-bit LBAs and lists image
    D with tracks at sectors 0 and 544 (CONFIRMED (crafted));
  - takes track positions from the cue chunk and not from the byte
    offsets of the DAO chunk (REFERENCE-ONLY);
  - in its development head refuses a chain longer than 4,096 bytes
    (REFERENCE-ONLY). Real chains reach 5,248 bytes.
- **libMirage** reads the whole region between the chain offset and the
  footer into memory without an upper bound, does not stop at `END!`,
  copies cue entries by the chunk's stated length into a buffer sized
  from a division of it, and treats an unknown mode code as audio
  (REFERENCE-ONLY).
- **MAME** reads only version 2 images below 4 GiB, uses the track
  numbers of the `DAOX` header as table positions without checking them
  against the size of its table, keeps walking until it meets `END!`
  with no check against the end of the file, and refuses modes `02`,
  `03`, `05`, `0F`, `10` and `11` (REFERENCE-ONLY).
- **Aaru** moves the start of every `ETN2` track whose LBA is at least
  150 back by 150 sectors, on the assumption that the image holds a
  pregap there (REFERENCE-ONLY). In the real `ETN2` image with eleven
  tracks the 150 sectors before each track are the last sectors of the
  track before (section 9.3, image 7).
- Several tools, among them conversion utilities named after the
  format, treat an NRG image as an ISO 9660 image with a fixed number
  of bytes in front (307,200) and ignore the chain. That is right for
  exactly one layout (one DAO session with one cooked data track) and
  silently wrong for the others.

## 14. Open points

- **No version 1 disc-at-once image was available.** The `DAOI` layout
  is REFERENCE-ONLY, with three tools agreeing against the Wikipedia
  article (section 5.1). The `CUES` entry layout is disputed (section
  5.3) and not needed.
- **No `TINF` chunk was seen.** Its layout rests on Aaru alone.
- **Modes `06` and `08` were not seen**, nor any TAO track of a mode
  other than `00` and `03`. The sizes behind TAO modes `02`, `05`,
  `06`, `07`, `0F`, `10` and `11` are assumed to be those of the DAO
  table. The raw and sub-channel layouts themselves are confirmed on DAO
  tracks.
- **No image whose sector size field disagrees with its mode code was
  seen.** The rule that the field wins (section 6.1) follows Aaru's
  report about MagicISO.
- **Images with more than two sessions, with several TAO sessions, or
  with DAO and TAO sessions together were not seen.** The rules do not
  depend on the combination, but the claim that the chain order is the
  session order rests on the two-session images.
- **A later session's data track could be presented better than CUE/BIN
  allows.** The chain gives the LBA of every session (the `CUEX` chunk,
  or the LBA field of `ETN2`), so a reader could tell the ISO 9660
  reader where the track lies on the disc, or present the earlier
  sessions' sectors at their disc addresses so that files kept from
  them resolve. That needs an interface the entry model does not have
  and is left for a later revision; it would then also need the `CUES`
  question settled.
- **Pregap audio** (hidden passages, the 300-sector pregap of image 2)
  is not listed.
- **DVD images exceed the size limits that suit CD tracks.** Whether an
  entry of more than 4 GiB is produced is a matter of the repository's
  limits (section 16, requirement 9); the format rules are the same.
  With the limit of 4 GiB per entry in force, the single track of two of
  the 169 real images (4,534,112,412 and 4,301,193,372 bytes) is
  reported as over the limit and not listed; the other 167 images list
  every track with the size their chain implies. MEASURED on files of
  the real size holding the real chain and the sampled sectors, zeros
  elsewhere.
- **Mode 2 without forms** would be read 8 bytes off, as for CUE/BIN.
  No mode code for it is known.
- **Other tools' images.** All 28 images with bytes in no track come
  from one family of uploads. Images written by UltraISO, PowerISO,
  MagicISO or Alcohol 120% under the name NRG may have further
  peculiarities; none could be attributed to a tool with certainty.

## 15. Decisions

| # | Decision | Section |
|--:|:--|:--|
| 1 | The recognition test: footer, bounded chain offset, a chain that walks to `END!`, at least one track entry; version 2 tested first. | 3.2 |
| 2 | The test runs before the tests for file systems and partition schemes. | 3.2 |
| 3 | The chain is at most 1 MiB and at most 256 chunks; identifiers are printable ASCII. | 3.2 |
| 4 | Unknown chunks are skipped; the identifier, not the footer version, decides a chunk's layout. | 4.2 |
| 5 | `DAOI` has a 22-byte header and 30-byte entries. | 5.1 |
| 6 | Incomplete trailing entries of a track chunk are ignored. | 5.1, 5.2 |
| 7 | `TINF` is read as a TAO chunk with 12-byte entries. | 5.2 |
| 8 | `CUES`/`CUEX`, `SINF`, `MTYP`, `CDTX`, `DINF`, `TOCT`, `RELO`, `AFNM` and `VOLM` are skipped. | 5.3 to 5.8 |
| 9 | The mode table; an unknown mode makes its track unreadable and no other. | 6.1 |
| 10 | In a DAO entry the sector size field gives the size and the mode code the kind; allowed sizes are 2048, 2336, 2352 and 2448, for audio 2352 and 2448. | 6.1 |
| 11 | Data tracks are cooked to 2048 bytes per sector; in raw sectors the sector's own mode byte decides between offsets 16 and 24 when it has a sync pattern; a Form 2 sector contributes its first 2048 bytes; sub-channel bytes are dropped. | 6.3 |
| 12 | Tracks are numbered by position, across sessions; at most 99. | 8.1 |
| 13 | A layout whose offsets decrease is inconsistent as a whole. Gaps between tracks are allowed. | 8.2 |
| 14 | A track runs from index 1 to its end; ends are clamped to the chain offset, starts are not; a trailing partial sector is ignored; empty tracks are reported. | 8.3 |
| 15 | One entry per readable track; names `trackNN.img` and `trackNN.pcm`; cooked data and little-endian PCM; pregaps and bytes in no track in no entry; unreadable tracks reported, not listed; container format `Nrg`, extension `nrg`. | 10 |
| 16 | Sessions are not modelled; a later session's data track is handed out like any other. | 10.7 |
| 17 | The limits of section 16. | 16 |

## 16. Safety requirements

An NRG image is untrusted data whose last bytes steer the reader to
arbitrary offsets. The following are requirements.

1. **Nothing is read before the footer is validated**, and nothing
   outside the file. The chain offset is compared with the footer's
   position before it is used. A chain offset of sixteen `F` digits, of
   zero, or beyond the file is "not an NRG image", not an error path
   that seeks.
2. **The chain is bounded** three ways: it lies between the chain offset
   and the footer; that region is at most 1 MiB; at most 256 chunks are
   walked. A chunk's length is compared with the bytes remaining before
   the footer **before** it is added to the position, so that a length
   of `FF FF FF FF` cannot wrap a 32-bit position. The walk only moves
   forward; there are no offsets inside chunks that point to other
   chunks, so there is no loop to detect beyond the forward progress of
   at least 8 bytes per chunk.
3. **No allocation from a stated size.** The chain may be read into
   memory once it has been bounded by the 1 MiB limit against the real
   file size. Entry counts come from dividing a chunk's validated
   length; the list of tracks holds at most 99 elements. The repeated
   length field of the DAO header, the track counts of `SINF` and of the
   DAO header and the sector count of `ETN2` are never used for sizing
   or looping.
4. **All arithmetic is checked.** Offsets are up to 64 bits wide. Offset
   + length of a TAO entry, and every product of a sector count and a
   sector size, are computed with overflow checks; an overflow makes the
   layout inconsistent (section 8.2).
5. **Sector sizes come from a closed set.** The stored sector size is
   one of 2048, 2336, 2352 and 2448 or the track is not read. No
   division by a size from the file happens before this check, so a
   size of zero cannot divide.
6. **Every range is validated against the data area before reading**:
   start below the chain offset, end clamped to it, whole sectors only
   (section 8.3). No sector is read partially, no byte of the chain or
   the footer is ever part of a track, and no byte outside the file is
   assumed to be zero.
7. **Tracks never overlap.** The check of section 8.2 covers pregaps as
   well as data and runs over all sessions together. Together with
   requirement 6 it guarantees that the total size of all entries is at
   most the size of the data area, and cooked entries from 2336-, 2352-
   and 2448-byte sectors are smaller still. An image cannot make the
   reader produce more data than it was given: there is no
   amplification, whatever the chain says and however many tracks it
   declares over the same bytes.
8. **Names never come from the image.** Entry names are built from
   track numbers assigned by position (1 to 99). The catalogue number,
   ISRC codes, CD-TEXT and the file names of an `AFNM` chunk are never
   used as names, never looked up as files and never interpreted.
9. **Output is still budgeted.** Entry sizes are checked against the
   repository's limit for a single output and charged to an output
   budget derived from the size of the image, like the entries of the
   CUE/BIN reader. An entry that exceeds a limit is reported and
   skipped; the others are still produced.
10. **Memory.** Real images reach 800 MiB for a CD and several GiB for a
    DVD. The reader should produce one entry at a time and release it
    before producing the next, should not hold cooked copies of all
    tracks at once, and can hand out a track of 2048-byte sectors as a
    range of the image without copying.

    Implemented differently, for a reason outside this format: in this
    repository a container does not keep a reference to the image it
    was opened from, so an entry cannot be a range of the image. The
    reader therefore holds the entry data of every readable track, and
    nothing else, from opening until the container is released: 2048 or
    2352 bytes per sector whatever the stored sector size, each track
    copied once and cooked while it is copied. What is held is exactly
    what requirement 9 charges, and by requirement 7 at most the size
    of the data area. It is one copy of the image, not one entry at a
    time; the CUE/BIN reader holds its track files the same way.
11. **Sector contents are data.** Header addresses, modes and
    sub-headers found in sectors are not trusted for anything except
    the choice between user-data offsets 16 and 24 (section 6.3); a
    wrong value there selects different bytes of the same sector and
    nothing else. Sub-channel bytes are never interpreted.
12. **No recursion from the image.** The reader never interprets a
    track as an NRG image itself; nesting happens only through the
    ordinary content detection of entries, under the repository's depth
    limit. A track that happens to end with a valid NRG footer is then
    an NRG image inside an NRG image, each level strictly smaller than
    the one above.
13. **Failure is local.** A track that cannot be read does not prevent
    the other tracks from being listed. Only an inconsistent layout
    (section 8.2) withholds all tracks, and even then the image opens
    and the problem is reported.
14. **No aborts.** Every malformed field leads to "not an NRG image" or
    to a reported track, never to a panic or a failed assertion (compare
    section 13).
