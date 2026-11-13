# CUE/BIN: cue sheets and the track files they describe

Format specification for clean-room implementation. This document covers
what a reader needs to split the track files named by a cue sheet into
tracks and to hand each track on as one entry:

- **Part A** (sections 3 to 5): the cue sheet as text: encodings, lines,
  quoting, the commands and which of them matter, the time format.
- **Part B** (section 6): what a sector of each track mode looks like in a
  track file, and where its user data sits.
- **Part C** (sections 7 and 8): how the byte range of each track is
  derived from the sheet and the file sizes, and how the file names in the
  sheet are turned into lookups.
- **Part D** (sections 9 and 10): the disc layouts that matter for retro
  software, and how each track is exposed as an entry.

The reader is read-only and forward-only. It does not decode audio, does
not verify or use error-correction data, and does not read sub-channel
data.

The document contains no program code and no pseudo-code. Cue sheets
quoted in it are example data.

## 1. How to read this document

### 1.1 Provenance

- The command set is described from Appendix A of the CDRWIN User's
  Guide, which is the original definition of the format, and from three
  public descriptions of it (the GNU libcdio manual, the Hydrogenaudio
  Knowledgebase, Wikipedia).
- The sector layouts are those of ECMA-130 section 14 (Mode 1 and Mode 2)
  and of the CD-ROM XA extension (Mode 2 Form 1 and Form 2), taken as
  restated in the comments and constants of libcdio's `sector.h` and
  checked on real images.
- Facts about what existing readers accept were taken from the source of
  libcdio, bchunk, MAME, DOSBox Staging and libcue, and from running
  bchunk 1.2.2 and `cd-info` 2.2.0 on purpose-built images.
- Real-world behaviour was measured on twelve cue sheets of BeOS-related
  discs and on sectors read from four of their track files (section 9.4).
- The full list of sources with licences is in
  [`README.md`](README.md), section 3.

### 1.2 Evidence labels

- **CDRWIN**: stated by Appendix A of the CDRWIN User's Guide.
- **ECMA-130**: stated by the standard (through the restatement named in
  section 1.1).
- **CONFIRMED (real data)**: observed on the real sheets and track files
  of section 9.4.
- **CONFIRMED (crafted)**: observed by giving bchunk 1.2.2 or `cd-info`
  2.2.0 the purpose-built image of section 11.3 or a variant of its
  sheet.
- **REFERENCE-ONLY**: stated by the source of one or more existing tools,
  not observed. The tools are named.
- **INFERRED**: deduced, unconfirmed.
- **DECISION**: chosen by this specification (listed in section 14). No
  decision changes which bytes of a well-formed image belong to which
  track.

### 1.3 Terms

| Term | Meaning |
|:--|:--|
| sheet | The cue sheet: a small text file, by convention with the extension `.cue`. |
| track file | A file named by a FILE command of the sheet. By convention `.bin`, also `.img`, `.iso`, `.raw`. |
| sector | One addressable unit of a CD. 75 sectors make one second of playing time. |
| frame | The unit of the sheet's time values. One frame is one sector. |
| stored sector size | The number of bytes one sector of a track occupies in its track file: 2048, 2336, 2352 or 2448 depending on the track mode. |
| raw sector | A 2352-byte sector: for a data track it starts with a 12-byte sync pattern and a 4-byte header. |
| cooked data | The user data of data sectors, 2048 bytes per sector, with nothing in between. A file system addresses cooked data. |
| mark | A position given by an INDEX command: a frame count relative to the start of the track file that is current at that command. |
| span | The part of a track file that belongs to one track (section 7.2). |
| gap | The part of a track's span before its INDEX 01 mark. |
| entry | One track as listed to the caller. |
| lookup | The caller-supplied function that returns the contents of a file beside the sheet, given a name. |

All multi-byte quantities derived in this document (frame counts, byte
offsets, sizes) are to be held in unsigned 64-bit integers and computed
with overflow checks.

## 2. Overview

A cue sheet is a line-oriented text file. It names one or more track
files and, for each, the tracks whose data the file holds. For every
track it gives a number, a mode (audio, or one of several data sector
formats) and one or more marks. Example (CONFIRMED (real data), sheet 5
of section 9.4):

    FILE "BeOS_4_5.bin" BINARY
      TRACK 01 MODE1/2352
        INDEX 01 00:00:00
      TRACK 02 MODE1/2352
        INDEX 00 04:00:00
        INDEX 01 04:02:00
      TRACK 03 MODE1/2352
        INDEX 00 38:50:34
        INDEX 01 38:52:34

The track files contain nothing but sectors, one after the other, with no
header, no trailer and no index of their own. Everything about their
structure comes from the sheet; the only other input is each file's
length.

Reading therefore has four steps:

1. Decode and parse the sheet into files, tracks and marks (sections 3 to
   5).
2. Obtain each track file through the lookup (section 8).
3. Derive each track's byte range in its file (section 7).
4. Produce each track's content: cooked data for a data track, PCM
   samples for an audio track (sections 6 and 10).

## 3. The sheet as text

### 3.1 Size and character encoding

- A sheet larger than 1 MiB (1,048,576 bytes) is not a cue sheet.
  **DECISION.** The largest real sheet seen is 238 bytes; a 99-track sheet
  with full CD-Text stays below 64 KiB.
- The encoding is not declared anywhere. CDRWIN predates Unicode use and
  wrote the system's 8-bit code page. Later writers produce UTF-8 with or
  without a byte order mark, and occasionally UTF-16 with a byte order
  mark. The encoding is chosen as follows. **DECISION.**

  | First bytes | Encoding | Byte order mark |
  |:--|:--|:--|
  | `EF BB BF` | UTF-8 | removed |
  | `FF FE` | UTF-16, little-endian | removed |
  | `FE FF` | UTF-16, big-endian | removed |
  | anything else, and the whole sheet is valid UTF-8 | UTF-8 | none |
  | anything else | each byte is the character with that code point (ISO 8859-1) | none |

- A sheet that declares UTF-8 or UTF-16 by its mark and then fails to
  decode is not a cue sheet. **DECISION.**
- The fallback row exists only so that the command words, numbers and
  times of a legacy sheet still parse. A file name with non-ASCII
  characters from an unknown code page will usually not match any file;
  section 8 then falls back to the sheet's own name.
- None of the 12 real sheets has a byte order mark or a non-ASCII byte.
  CONFIRMED (real data). libcdio rejects a sheet that starts with a byte
  order mark; CONFIRMED (crafted).

### 3.2 Lines

- A line ends at LF, at CR LF, or at a lone CR. Of the real sheets, 9 use
  CR LF and 3 use LF. CONFIRMED (real data).
- The character U+001A (the DOS end-of-file mark) ends the sheet; it and
  everything after it are ignored. **DECISION.**
- After decoding, a sheet that contains U+0000 is not a cue sheet.
  **DECISION.**
- Leading and trailing spaces and tabs of a line are ignored. Indentation
  is customary (commands under a FILE are indented, commands under a TRACK
  are indented further) and carries no meaning. CDRWIN, Hydrogenaudio.
- Empty lines are ignored.
- There is no line continuation and no comment character. A comment is a
  line whose command is REM.

### 3.3 Words and quoting

- A line consists of a command word followed by arguments, separated by
  one or more spaces or tabs.
- A string argument (a file name, a title) that contains spaces is
  enclosed in double quotes (U+0022). CDRWIN. Writers quote names
  whether or not they contain spaces; all 12 real sheets do. CONFIRMED
  (real data).
- CDRWIN defines no escape character inside quotes. A backslash is an
  ordinary character: it is the directory separator of the system the
  format comes from (CDRWIN's own example names a file `C:\DATA\TRACK1.ISO`).
  It must never be treated as an escape. **DECISION.** (libcue treats a
  backslash before a quote as an escape, REFERENCE-ONLY; that reading is
  rejected because CDRWIN does not define it and it misreads a quoted
  path that ends in a backslash.)
- Typographic quotation marks (U+201C, U+201D) are not quotes. They occur
  in web copies of the CDRWIN text, not in sheets.
- Numbers are unsigned decimal, written with leading zeros by convention
  (`TRACK 01`, `INDEX 00`). Leading zeros carry no meaning.
- Arguments beyond those a command defines are ignored. **DECISION.**
  (Some writers append further words, for instance a sub-channel type
  after the track mode; MAME reads one. REFERENCE-ONLY.)

Only the FILE command needs its string argument. Its arguments are
separated as follows; let *rest* be the line after the command word, with
surrounding spaces and tabs removed. **DECISION.**

1. If *rest* starts with a double quote and contains a later double
   quote: the name is everything between the first and the **last**
   double quote of the line, and the type is the first word after that
   last quote. If no word follows, the type is absent.
2. Otherwise, if *rest* has two or more words: the type is the last word
   and the name is everything before it with surrounding spaces and tabs
   removed. A single stray double quote at the start of the name is
   removed.
3. Otherwise *rest* is the name and the type is absent.

Rule 1 uses the last quote so that a name containing quotes survives.
Rule 2 accepts unquoted names with spaces, which CDRWIN forbids and
which occur in hand-written sheets. An empty name makes the FILE command
malformed (section 4.6).

### 3.4 Letter case

Command words, FILE types, TRACK modes and FLAGS values are compared
without regard to ASCII letter case. **DECISION.** Writers produce upper
case; all 12 real sheets do. Existing readers disagree: libcue and DOSBox
Staging ignore case (REFERENCE-ONLY), bchunk and libcdio accept upper
case only (CONFIRMED (crafted): given a lower-case sheet, bchunk finds no
tracks and `cd-info` reports that the file is not a cue sheet). File
names keep their case; section 8 deals with it.

### 3.5 Recognising a cue sheet

A cue sheet has no signature. The repository probes for one only when the
file's name ends in `.cue`, so the test has to reject files that carry
the extension by accident, not to find sheets among arbitrary data. A
byte sequence is a cue sheet when all of the following hold. **DECISION.**

1. It is not empty and not larger than 1 MiB.
2. It decodes by section 3.1 and, after decoding and cutting at U+001A,
   contains no U+0000.
3. The command word of its first non-empty line is one of the thirteen
   CDRWIN commands of section 4.
4. It contains at least one well-formed FILE command and, after it, at
   least one well-formed TRACK command.

A sheet that passes this test can still fail to open (section 4.6) or
yield no entries (missing track files).

## 4. Commands

CDRWIN defines thirteen commands. A reader that splits tracks needs
three of them and has to recognise the rest only to skip them.

| Command | Arguments | Scope | Use to this reader |
|:--|:--|:--|:--|
| `FILE` | name, type | sheet | Starts a track file; needed. |
| `TRACK` | number, mode | file | Starts a track; needed. |
| `INDEX` | number, time | track | Sets a mark; indexes 00 and 01 are needed. |
| `PREGAP` | time | track | Ignored: describes sectors that are **not** in the file. |
| `POSTGAP` | time | track | Ignored, for the same reason. |
| `FLAGS` | one or more of `DCP`, `4CH`, `PRE`, `SCMS` | track | Ignored. |
| `CATALOG` | 13 digits | sheet | Ignored. |
| `ISRC` | 12 characters | track | Ignored. |
| `CDTEXTFILE` | name | sheet | Ignored. The named file is never looked up. |
| `TITLE` | string | sheet or track | Ignored. |
| `PERFORMER` | string | sheet or track | Ignored. |
| `SONGWRITER` | string | sheet or track | Ignored. |
| `REM` | free text | anywhere | Ignored. |

A line whose command word is none of these is ignored. **DECISION.**
Writers have added commands over the years and none of them changes where
track data lies. (DOSBox Staging rejects a sheet with an unknown command;
MAME ignores it. REFERENCE-ONLY.)

### 4.1 FILE

- Syntax: `FILE` name type. CDRWIN.
- The command names a track file and makes it the **current file**. All
  following TRACK and INDEX commands refer to it until the next FILE
  command. CDRWIN.
- A sheet has one FILE command when the whole disc is in one file, or one
  per track, or anything in between (section 7.6).
- The types:

  | Type | Meaning (CDRWIN) | In scope |
  |:--|:--|:--|
  | `BINARY` | "Intel binary file (least significant byte first)": sectors as they are, 16-bit audio samples little-endian | yes |
  | `MOTOROLA` | "Motorola binary file (most significant byte first)": the same, but 16-bit audio samples big-endian | yes |
  | `WAVE` | audio in a RIFF WAVE file; in practice also any other audio container | no |
  | `AIFF` | audio in an AIFF file | no |
  | `MP3` | audio in an MP3 file | no |
  | anything else (`FLAC` and others occur) | not defined by CDRWIN | no |

- An absent type is read as `BINARY`. **DECISION.**
- A file whose type is not in scope is an **unsupported file**. It is
  never looked up; each of its tracks is reported as unsupported
  (section 10.4). **DECISION.** Reading them would require decoding audio
  containers, which is outside the scope of this reader.
- The byte order named by the type applies to audio samples only. A data
  sector is a sequence of bytes with no byte order, so a data track of a
  `MOTOROLA` file is read exactly like one of a `BINARY` file. INFERRED;
  MAME applies the swap to audio tracks only (REFERENCE-ONLY). No real
  `MOTOROLA` sheet was available (section 13).
- CDRWIN states that FILE commands come before every other command
  except CATALOG. Real sheets put REM, TITLE and PERFORMER lines before
  the first FILE (CONFIRMED (real data): sheet 2 starts with a REM line),
  so no ordering is required of the ignored commands.

### 4.2 TRACK

- Syntax: `TRACK` number mode. CDRWIN.
- The command starts a new track in the current file and makes it the
  **current track**.
- The number is 1 to 99. The first track of a sheet may have a number
  above 1; after it, numbers increase by one. CDRWIN. This specification
  requires only that numbers are in 1 to 99 and strictly increasing
  (section 4.6).
- The modes, with the stored sector size each implies:

  | Mode | Stored sector size | Content | Origin |
  |:--|--:|:--|:--|
  | `AUDIO` | 2352 | audio samples | CDRWIN |
  | `CDG` | 2448 | audio samples (2352) followed by 96 bytes of sub-channel data per sector | CDRWIN; the order of the two parts is REFERENCE-ONLY (MAME) |
  | `MODE1/2048` | 2048 | Mode 1 user data only ("cooked") | CDRWIN |
  | `MODE1/2352` | 2352 | raw Mode 1 sectors | CDRWIN |
  | `MODE2/2336` | 2336 | Mode 2 sectors without sync and header | CDRWIN |
  | `MODE2/2352` | 2352 | raw Mode 2 sectors | CDRWIN |
  | `CDI/2336` | 2336 | as `MODE2/2336`, on a CD-i disc | CDRWIN |
  | `CDI/2352` | 2352 | as `MODE2/2352`, on a CD-i disc | CDRWIN |
  | `MODE2/2048` | 2048 | Mode 2 Form 1 user data only | not in CDRWIN; accepted by libcdio, MAME, libcue (REFERENCE-ONLY) |
  | `MODE2/2324` | 2324 | Mode 2 Form 2 user data only | not in CDRWIN; accepted by libcdio, MAME (REFERENCE-ONLY) |

- Any other mode is an **unknown mode**. If it has the shape of a word, a
  slash and a decimal number between 2048 and 2448, that number is taken
  as its stored sector size, so that the tracks after it in the same file
  can still be located; the track itself is reported as unsupported.
  Otherwise its stored sector size is unknown, with the consequence given
  in section 7.3. **DECISION.**
- The 12 real sheets use `MODE1/2352` (11 sheets) and `MODE1/2048` (1
  sheet) only. CONFIRMED (real data).

### 4.3 INDEX

- Syntax: `INDEX` number time. CDRWIN.
- The number is 0 to 99. The time is a position **relative to the start
  of the current file**, in the format of section 5. CDRWIN.
- INDEX 01 is where the track's own data starts. It is the position the
  disc's table of contents records for the track. Every track needs it.
  CDRWIN.
- INDEX 00, when present, comes before INDEX 01 and marks where the
  track's gap starts: sectors that belong to the track by the disc's
  bookkeeping but precede its start. On an audio disc this is the pause a
  player counts down through; before a data track it is filler.
- INDEX 02 to 99 are positions inside the track. They are ignored.
- CDRWIN states that the first index of a file is at 00:00:00. Real
  sheets obey this; section 7.3 says what happens otherwise.
- An INDEX command belongs to the current track and to the file that is
  current **at the INDEX command**, which is not always the file that was
  current at the TRACK command (section 7.6, layout D).
- Only the first INDEX 00 and the first INDEX 01 of a track count; a
  repeated index number is ignored. **DECISION.**

### 4.4 PREGAP and POSTGAP

- Syntax: `PREGAP` time, `POSTGAP` time. The time is a length. CDRWIN.
- They tell a disc writer to generate that many empty sectors before the
  track's INDEX 01 or after the track's last sector. In CDRWIN's words,
  "no data is consumed from the current data file".
- They therefore shift every later position **on the disc** and leave
  every position **in the file** unchanged. Positions on the disc are of
  no interest to this reader, so both commands are ignored. They must not
  be added to or subtracted from any file offset.
- A gap that **is** in the file is described by INDEX 00, not by PREGAP.
  A track can have both (a generated silence followed by a stored gap),
  with the same consequences.

### 4.5 REM and its conventional uses

REM lines are comments, and writers use them to carry data that the
format has no command for. None of it changes where track data lies in a
track file, and all of it is ignored.

| REM line | Written by | Meaning |
|:--|:--|:--|
| `REM GENRE`, `REM DATE`, `REM DISCID`, `REM COMMENT` | audio rippers | tags (Hydrogenaudio) |
| `REM ORIGINAL MEDIA-TYPE` | disc imaging tools | the kind of disc (CONFIRMED (real data), sheet 2) |
| `REM SESSION` number | disc imaging tools | the following tracks belong to that session of a multi-session disc (CONFIRMED (real data), sheet 2, where it stands between the FILE line and the first TRACK line) |
| `REM LEAD-OUT`, `REM LEAD-IN`, `REM PREGAP`, `REM MSF` | disc imaging tools | positions or lengths on the disc (REFERENCE-ONLY, MAME) |
| `REM SINGLE-DENSITY AREA`, `REM HIGH-DENSITY AREA` | GD-ROM dumps | the two areas of a Dreamcast disc (REFERENCE-ONLY, MAME) |

A REM line may stand anywhere, including between a FILE and its first
TRACK and between a TRACK and its INDEX lines.

### 4.6 Rules of context

The sheet is read top to bottom while keeping a current file and a
current track. **DECISION** for every rule that goes beyond CDRWIN:

- A **malformed** command is one whose required arguments are missing or
  do not parse: a FILE without a name; a TRACK whose number is not a
  decimal number or whose mode is missing; an INDEX whose number or time
  does not parse (section 5).
- A malformed FILE or TRACK command makes the sheet invalid: opening it
  fails.
- A TRACK command before the first FILE command makes the sheet invalid.
- A track number outside 1 to 99, or not greater than the number of the
  track before it, makes the sheet invalid. (Entry names are derived from
  the numbers and must be unique; a sheet that breaks this rule is not
  describing a disc.)
- More than 99 FILE commands make the sheet invalid.
- An INDEX command before the first TRACK command is ignored.
- A malformed INDEX command, or an INDEX number above 99, is ignored; if
  it would have been the track's INDEX 01 the track ends up without one
  and is reported as unusable (section 7.5).
- A FILE command ends the current file but **not** the current track: an
  INDEX command that follows a FILE command directly, with no TRACK
  command in between, still belongs to the last track (section 7.6,
  layout D).
- A FILE command with no tracks and no marks is allowed and contributes
  nothing; it is not looked up.

## 5. Times

- A time is written as three decimal fields separated by colons: minutes,
  seconds, frames. CDRWIN writes each field with two digits
  (`04:02:00`). There are 75 frames in a second and 60 seconds in a
  minute. CDRWIN.
- The value of a time in frames is

  minutes × 4500 + seconds × 75 + frames.

- The minutes field is not limited to 99 in practice: images longer than
  100 minutes are written with three digits. Each field is accepted with
  1 to 4 digits. **DECISION.**
- A seconds field above 59 or a frames field above 74 is still evaluated
  by the formula (as bchunk, MAME and DOSBox Staging do; REFERENCE-ONLY).
  **DECISION.** A sheet with such a value is damaged, but the rules of
  section 7 keep any outcome inside the file.
- A time with fewer or more than three fields, an empty field, a
  non-digit, or a field of more than 4 digits is malformed.
- One frame is one sector **of the track the frames belong to**. A frame
  count is turned into a byte count by multiplying with that track's
  stored sector size, never with a fixed 2352 (section 7.3).

Worked values (the byte offsets assume a file in which every track is
stored in 2352-byte sectors):

| Time | Frames | × 2352 |
|:--|--:|--:|
| `00:00:00` | 0 | 0 |
| `00:02:00` | 150 | 352,800 |
| `00:38:08` | 2,858 | 6,722,016 |
| `04:00:00` | 18,000 | 42,336,000 |
| `04:02:00` | 18,150 | 42,688,800 |
| `10:48:58` | 48,658 | 114,443,616 |
| `38:52:34` | 174,934 | 411,444,768 |
| `46:07:03` | 207,528 | 488,105,856 |

With 2048-byte sectors: `10:57:27` is 49,302 frames and 100,970,496
bytes. CONFIRMED (real data): the cdrdao table of contents shipped
beside sheet 11 gives the same track start as a byte offset,
`#100970496`.

## 6. Sectors

### 6.1 Audio sectors

- An audio sector is 2352 bytes of sample data and nothing else: 588
  stereo sample pairs, each pair a left then a right sample, each sample
  a signed 16-bit integer, at 44,100 pairs per second. There is no sync
  pattern and no header.
- In a `BINARY` file each sample is stored least significant byte first;
  in a `MOTOROLA` file most significant byte first. CDRWIN.
- A `CDG` sector is 2448 bytes: an audio sector followed by 96 bytes of
  sub-channel data.

### 6.2 Raw data sectors

A raw data sector is 2352 bytes. Its first 16 bytes are the same in all
modes. ECMA-130; CONFIRMED (real data) for Mode 1.

| Offset | Size | Field |
|--:|--:|:--|
| 0 | 12 | Sync pattern: `00`, then ten times `FF`, then `00`. |
| 12 | 1 | Minute of the sector's address, in binary-coded decimal. |
| 13 | 1 | Second of the address, in binary-coded decimal. |
| 14 | 1 | Frame of the address, in binary-coded decimal. |
| 15 | 1 | Mode: `01` or `02`. (`00` is an empty sector.) |

The address counts from the start of the disc's lead-in area, which is
150 frames before the first sector of the first track. The sector at
frame *n* of a single-file image therefore carries the address *n* + 150:
the first sector of every real image starts with the header `00 02 00
01`, and the sector at frame 18,150 (`04:02:00`) carries `04 04 00 01`.
CONFIRMED (real data). The reader does not need the address; it is given
here as a debugging aid and because it shows that the times of a sheet
are file positions, not disc addresses.

On a disc, bytes 12 to 2351 are scrambled. Track files hold them
unscrambled. CONFIRMED (real data).

The remaining 2336 bytes depend on the mode.

**Mode 1** (ECMA-130):

| Offset | Size | Field |
|--:|--:|:--|
| 16 | 2048 | User data. |
| 2064 | 4 | Error detection code. |
| 2068 | 8 | Zero. |
| 2076 | 276 | Error correction code (172 bytes P parity, 104 bytes Q parity). |

**Mode 2 without forms** (ECMA-130): bytes 16 to 2351 are 2336 bytes of
user data with no error correction. This layout is rare and is not
distinguishable from the next two by the header.

**Mode 2 Form 1** (CD-ROM XA and CD-i):

| Offset | Size | Field |
|--:|--:|:--|
| 16 | 4 | Sub-header: file number, channel number, sub-mode, coding information. |
| 20 | 4 | The same four bytes repeated. |
| 24 | 2048 | User data. |
| 2072 | 4 | Error detection code. |
| 2076 | 276 | Error correction code. |

**Mode 2 Form 2** (CD-ROM XA and CD-i):

| Offset | Size | Field |
|--:|--:|:--|
| 16 | 8 | Sub-header, as for Form 1. |
| 24 | 2324 | User data. |
| 2348 | 4 | Error detection code, or zero. |

The sub-mode byte (offset 18, repeated at offset 22) tells the two forms
apart: bit 5 (value `20` hexadecimal) is set in a Form 2 sector and clear
in a Form 1 sector. Its other bits (end of record `01`, video `02`, audio
`04`, data `08`, trigger `10`, real time `40`, end of file `80`) are not
needed. Form 2 sectors carry interleaved audio and video streams (Video
CD, PlayStation); file system structures and ordinary files are always in
Form 1 sectors.

### 6.3 Where the user data sits, by track mode

| Mode | Stored sector size | The stored sector is | User data at | User data size |
|:--|--:|:--|--:|--:|
| `AUDIO` | 2352 | an audio sector | 0 | 2352 |
| `CDG` | 2448 | an audio sector plus sub-channel | 0 | 2352 |
| `MODE1/2048` | 2048 | bytes 16 to 2063 of a Mode 1 sector | 0 | 2048 |
| `MODE1/2352` | 2352 | a raw Mode 1 sector | 16 | 2048 |
| `MODE2/2352`, `CDI/2352` | 2352 | a raw Mode 2 sector | 24 | 2048 (Form 1), 2324 (Form 2) |
| `MODE2/2336`, `CDI/2336` | 2336 | bytes 16 to 2351 of a Mode 2 sector | 8 | 2048 (Form 1), 2324 (Form 2) |
| `MODE2/2048` | 2048 | bytes 24 to 2071 of a Form 1 sector | 0 | 2048 |
| `MODE2/2324` | 2324 | bytes 24 to 2347 of a Form 2 sector | 0 | 2324 |

In a `MODE2/2336` sector the sub-mode byte is at offset 2 (repeated at
offset 6).

### 6.4 Cooking a data track

A data track is handed on as cooked data: exactly 2048 bytes for every
stored sector, in order. **DECISION** (section 10.2 gives the reason).
The 2048 bytes of one sector are found as follows.

- `MODE1/2048` and `MODE2/2048`: the stored sector itself.
- `MODE1/2352`, `MODE2/2352`, `CDI/2352`: if the sector's first 12 bytes
  are the sync pattern and its mode byte (offset 15) is `01`, the 2048
  bytes at offset 16; if they are the sync pattern and the mode byte is
  `02`, the 2048 bytes at offset 24; otherwise the 2048 bytes at the
  offset the track's declared mode implies (16 for `MODE1/2352`, 24 for
  the other two). **DECISION.**

  The sector's own mode byte is preferred over the sheet because sheets
  that declare the wrong one of the two raw modes exist, and because the
  repository's existing raw-sector code (`src/formats/cd_sector.rs`)
  decides the same way. The fallback matters for sectors without a
  header, such as all-zero filler.
- `MODE2/2336`, `CDI/2336`: the 2048 bytes at offset 8.
- A Form 2 sector contributes the first 2048 of its 2324 user bytes; the
  remaining 276 are dropped. **DECISION.** bchunk does the same;
  CONFIRMED (crafted). Every sector has to contribute the same number of
  bytes or the block numbers of the file system on the track would no
  longer match byte offsets. The consequence is that a file stored in
  Form 2 sectors (a Video CD stream, a PlayStation movie) is not
  recoverable from the cooked track. This is accepted: such files are
  streams that need a format-specific demultiplexer anyway.
- Mode 2 sectors are always read as XA sectors (sub-header present). A
  Mode 2 track without forms would be read 8 bytes off. This is accepted;
  the existing raw-sector code makes the same assumption.
- `MODE2/2324` cannot be cooked to 2048 bytes per sector and is reported
  as unsupported. **DECISION.**
- The error detection and correction codes are neither checked nor used.

## 7. From the sheet to byte ranges

### 7.1 What the sheet gives

After parsing there is, in sheet order, a list of files; for each file,
in sheet order, the marks set while it was current; and for each track
its number, its mode, and at most one INDEX 00 mark and one INDEX 01
mark, each in some file. Usually both marks of a track are in the same
file; section 7.6 shows the exception.

Each file is processed on its own. Nothing is carried from one file to
the next: times restart at zero in every file.

### 7.2 Spans

Within one file, group the marks by track. The **first mark** of a track
in the file is its lowest-numbered one there (INDEX 00 if it is in this
file, else INDEX 01). The tracks that have a mark in the file, in sheet
order, divide the file into **spans**:

- the span of a track starts at its first mark in the file;
- it ends where the span of the next track with a mark in the file
  starts;
- the span of the last such track ends at the end of the file.

A span holds sectors of its own track's stored sector size, from its
first sector to its last. This includes the gap: the sectors between
INDEX 00 and INDEX 01 of a track are stored in that track's sector size,
not the preceding track's. REFERENCE-ONLY (MAME and DOSBox Staging agree
on this); it only matters when a file mixes sector sizes (section 7.3).

Every mark of a file must be at a frame greater than or equal to the
mark before it in sheet order. If one is not, the file's layout is
**inconsistent**: all tracks with a mark in that file are reported as
unusable. **DECISION.** (Marks that go backwards mean overlapping
tracks; no reading of such a sheet is trustworthy.) Two equal marks are
allowed.

### 7.3 Byte offsets

The frame numbers of a file translate to byte offsets span by span:

- The first span of a file starts at byte

  (its starting frame) × (its track's stored sector size).

  When the first mark of the file is at 00:00:00, as CDRWIN requires,
  this is 0. When it is not, the bytes before it belong to no track and
  are skipped. **DECISION.**
- Every later span starts at the byte where the previous span ends: the
  previous span's starting byte plus

  (this span's starting frame − the previous span's starting frame) ×
  (the previous track's stored sector size).

- A mark inside a span is at the span's starting byte plus

  (the mark's frame − the span's starting frame) × (the track's stored
  sector size).

When every track of the file has the same stored sector size *S*, all of
this collapses to "byte offset = frame × *S*". That is the case in every
real sheet examined (CONFIRMED (real data): eleven declare 2352 for all
their tracks, one declares 2048) and in nearly all images in circulation. The span-by-span
rule is what MAME and DOSBox Staging implement for a file that mixes
sizes (REFERENCE-ONLY); the typical case is a `MODE1/2048` data track
followed by `AUDIO` tracks in one file (worked example in section 11.2).
bchunk and libcdio multiply every time by 2352 regardless (section 12).

If a track's stored sector size is unknown (section 4.2), the end of its
span cannot be converted to a byte offset: that track and every track
whose span starts later in the same file are reported as unusable.
**DECISION.**

### 7.4 The range of a track

A track's data is the part of **the span that contains its INDEX 01
mark**, from that mark to the end of the span:

- it starts at the byte offset of the INDEX 01 mark;
- it ends at the start of the next span of that file, or, for the last
  span, at the end of the file.

The gap (from INDEX 00 to INDEX 01) is in the file but in no track's
data. **DECISION** (section 10.5). bchunk cuts the same way: with INDEX
00 present the preceding track stops at INDEX 00 and the track starts at
INDEX 01; with INDEX 00 absent the preceding track runs up to INDEX 01.
CONFIRMED (crafted), and CONFIRMED (real data) in the sense that the
file-system volume of every real data track starts exactly at INDEX 01
(section 9.4).

Against the file's real length *L*:

- If the track's start is at or beyond *L*, the track is reported as
  unusable (the file is shorter than the sheet says).
- If the track's end, taken from the next span, is beyond *L*, the end
  is *L*. The track is kept, and the tracks whose spans start at or
  beyond *L* are reported as unusable. **DECISION.**
- The number of sectors of the track is the length of the range divided
  by the stored sector size, rounded down. A partial sector at the end of
  the range is ignored. **DECISION.** For a range that ends at another
  span the division is exact by construction; a remainder can only occur
  at the end of the file.
- A track with zero sectors yields no entry and is reported.
  **DECISION.**

Because spans do not overlap, the ranges of all tracks of a file are
disjoint and their total length is at most the file's length.

### 7.5 Tracks that cannot be read

A track is reported instead of listed (section 10.4) when:

- its file is an unsupported file (section 4.1) or was not found
  (section 8);
- its mode is unknown or is `MODE2/2324`;
- it has no INDEX 01 mark;
- its file's layout is inconsistent (section 7.2), its start cannot be
  computed (section 7.3), or its range is empty or outside the file
  (section 7.4).

A track without an INDEX 01 mark still divides its file if it has an
INDEX 00 mark there: the preceding track ends at that mark.

### 7.6 Layouts with more than one file

In each example the two tracks are the first two of a disc.

**Layout A: one file for the whole disc.** One FILE command; all marks
are positions in that file. This is what disc imaging tools write and
what all 12 real sheets are.

**Layout B: one file per track, gaps left out or attached to the next
track.** Each FILE command is followed by one TRACK. The track's INDEX 01
is at 00:00:00, or its INDEX 00 is at 00:00:00 and its INDEX 01 a little
later (the gap is stored at the head of the track's own file). A PREGAP
command may stand in for a gap that was not stored.

    FILE "disc (Track 1).bin" BINARY
      TRACK 01 MODE1/2352
        INDEX 01 00:00:00
    FILE "disc (Track 2).bin" BINARY
      TRACK 02 AUDIO
        INDEX 00 00:00:00
        INDEX 01 00:02:00

Each file has one span covering the whole file. Track 2's data starts
150 sectors (352,800 bytes) into its file. This is the layout of the
large public dump collections of console and PC game discs.

**Layout C: one file per track, mixed sector sizes.** As B, with a
cooked data track:

    FILE "game.iso" BINARY
      TRACK 01 MODE1/2048
        INDEX 01 00:00:00
    FILE "track02.bin" BINARY
      TRACK 02 AUDIO
        PREGAP 00:02:00
        INDEX 01 00:00:00

The PREGAP line changes nothing in either file.

**Layout D: one file per track, gaps attached to the previous track.**
Audio rippers store a track's gap at the end of the previous track's
file and write a sheet that CDRWIN does not allow (Hydrogenaudio calls it
non-compliant): the track's INDEX 00 is given while the previous file is
still current, then comes a FILE command, then INDEX 01 with no new
TRACK command.

    FILE "01.bin" BINARY
      TRACK 01 AUDIO
        INDEX 01 00:00:00
      TRACK 02 AUDIO
        INDEX 00 02:47:74
    FILE "02.bin" BINARY
        INDEX 01 00:00:00

By sections 4.6 and 7.2, file `01.bin` has two spans (track 1 from frame
0, track 2 from frame 12,599) and file `02.bin` has one (track 2 from
frame 0). Track 1's data is `01.bin` up to frame 12,599. Track 2's data
is the span that contains its INDEX 01: all of `02.bin`. The tail of
`01.bin` is track 2's gap and is in no entry. Such sheets normally use
the types `WAVE` and so on and are then out of scope as a whole, but the
rules must not break on the layout.

**Mixed.** Any combination is possible, for instance one file holding
the data track and a second file holding all audio tracks. The rules of
sections 7.2 to 7.4 cover all of them because they work per file.

## 8. Finding the track files

The name in a FILE command was written by some program on some machine,
often long ago, and the files have been renamed, moved between operating
systems and re-uploaded since. Of the 12 real sheets, 3 do not name their
track file correctly: one gives an absolute path on the ripping machine
(`/home/ashley/BeOS_Tools.bin`; the file of that base name sits beside
the sheet), and two give a name that no file beside them has
(`BeOS_Tools.bin` and `BEOS_TOOLS.BIN`, while sheet and track file had
both been renamed to the disc's title). CONFIRMED (real data). Sheets
written on DOS and Windows use backslashes and drive letters (CDRWIN's
examples), and their case often differs from the files' case once those
have passed through a case-sensitive system (DOSBox Staging handles
exactly this for re-released DOS games; REFERENCE-ONLY).

### 8.1 Candidate names

For the name *W* of a FILE command, the reader asks the lookup for the
following candidates, in this order, and uses the first one that exists.
A candidate equal to an earlier one is skipped. **DECISION.**

Preparation:

- Replace every backslash in *W* by a forward slash, then split at
  forward slashes into components.
- *W* is **absolute** if it starts with a slash (which includes network
  paths starting with two) or if its first component is a single ASCII
  letter followed by a colon (a drive).
- The **base name** *B* is the last component. It is **unusable** if it is
  empty, is `.` or `..`, or contains a colon.
- The **stem** of a name is the part before its last dot, the
  **extension** the part after it. A name without a dot, or whose only
  dot is its first character, has no extension.

Candidates:

1. The relative path: only if *W* is not absolute, has more than one
   component, and none of its components is empty, `.`, `..` or contains
   a colon. The components joined by forward slashes.
2. *B*, if usable.
3. *B* in ASCII lower case, then *B* in ASCII upper case, if usable.
4. Only if the sheet has exactly one FILE command of a type in scope:
   the stem of the sheet's own file name, followed by a dot and, in turn,
   *B*'s extension as written, in ASCII lower case, in ASCII upper case
   (these three only if *B* is usable and has an extension), then `bin`,
   then `BIN`.

Every candidate is relative to the directory that contains the sheet.
Candidates 2 to 4 are single names in that directory.

Step 4 is the rule that repairs the two renamed real images (one of them
needs the upper-case extension: the sheet is `… (1998).CUE`, names
`BEOS_TOOLS.BIN`, and the file is `… (1998).BIN`). It is restricted to
single-file sheets because with several files the sheet's own name cannot
tell them apart. `cd-info` goes further and uses the sheet's name with
`.bin` **instead of** the FILE name; CONFIRMED (crafted). bchunk takes
the track file from its command line and never reads the FILE name.

If no candidate exists, the file is **not found** and its tracks are
reported (section 7.5). The reader goes on with the other files.

### 8.2 What the lookup should do beyond exact names

Two refinements belong to the code that implements the lookup, because
they need a directory listing, which the reader does not have:

- **Case-insensitive match.** When no file has exactly the requested
  name, a file whose name equals it without regard to letter case should
  be returned, provided exactly one such file exists.
- **The only track file.** For a sheet with exactly one FILE command of a
  type in scope, when all candidates fail and the sheet's directory
  contains exactly one file whose extension is `bin` (in any case), that
  file may be used.

Both are **DECISION**s of this specification about the loader; they are
optional refinements, and the reader must work, with fewer images found,
when the lookup matches exact names only.

### 8.3 Each file once

The same name may appear in several FILE commands (some writers repeat
the FILE line before every track of a single-file image; MAME tests for
this, REFERENCE-ONLY). FILE commands with the same name *W* are the same
file: their marks are merged into one list in sheet order, as if the
later FILE lines were absent, and the file is looked up once.
**DECISION.**

## 9. Disc layouts that matter

### 9.1 A single data track

One FILE, one TRACK of a data mode, `INDEX 01 00:00:00`. The track is the
whole file. Its cooked data is an ISO 9660 volume, an HFS volume or
Apple partition map, or a hybrid of them.

### 9.2 Data followed by audio: game discs

The first track is a data track with the game's files; tracks 2 and up
are `AUDIO` and hold the music. In a single-file image:

    FILE "GAME.BIN" BINARY
      TRACK 01 MODE1/2352
        INDEX 01 00:00:00
      TRACK 02 AUDIO
        INDEX 00 28:14:37
        INDEX 01 28:16:37
      TRACK 03 AUDIO
        INDEX 00 31:02:10
        INDEX 01 31:04:10

- The file system of track 1 knows nothing about the audio tracks. Its
  volume size normally ends at or before the next track's INDEX 00.
- The change from a data track to an audio track has a gap of 150
  frames or more, part of which may be recorded in the data track's
  sector format and part as audio (INFERRED from the general
  descriptions; not measured). In an image it shows as an INDEX 00 150 or
  more frames before INDEX 01, as a PREGAP line, or as neither (the gap
  sectors are then the tail of the data track, which harms nothing).
- When such an image is passed to a reader that knows only ISO 9660, the
  raw-sector detection of `src/formats/cd_sector.rs` requires the file's
  length to be a multiple of 2352 and its first sectors to carry sync
  patterns; that works for the single file but then exposes no audio.
  Going through the sheet exposes all tracks.

Playing audio first and data last (an "enhanced" disc with a second
session) also exists; it needs no rule of its own.

### 9.3 BeOS install discs

A BeOS install disc has two or three data tracks. CONFIRMED (real data),
on the four track files of section 9.4 whose sectors were read:

| Track | Content of its cooked data |
|:--|:--|
| 1 | The tools for starting the installation from another operating system, as an ISO 9660 volume: primary volume descriptor at cooked sector 16 (`CD001`) on R3, R4.5 and R5. On R4.5 and R5 it is a hybrid with an HFS volume (master directory block `BD` at cooked byte 1024); on R4.5 an El Torito boot record was seen at sector 17. |
| 2 | A BFS volume for Intel machines. Cooked bytes 0 to 511 are a boot block (boot code on R4.5 and R5, zero on R3); the BFS superblock is at cooked byte 512 (volume name at 512, the bytes `1SFB` at 544, `EGIB` at 548: a little-endian volume). |
| 3 (dual-platform discs: R4, R4.5, R5) | A BFS volume for PowerPC machines. The BFS superblock is at cooked byte **0** (volume name at 0, the bytes `BFS1` at 32, `BIGE` at 36: a big-endian volume). |

- R3 for Intel has tracks 1 and 2 only. An application disc for both
  platforms (sheet 11) has the big-endian volume in track 1 and the
  little-endian volume in track 2, and no ISO 9660 track; its track 1
  also carries Apple partition map entries (`PM`) at cooked bytes 512
  and 1024, after the superblock at byte 0.
- The BFS volume starts exactly at the track's INDEX 01. A reader that
  treats the whole track file as one ISO 9660 image sees track 1 only;
  the BeOS system itself is reachable only through the sheet.
- The sheets come in two styles for the same track file. CONFIRMED (real
  data): sheets 6 and 8 describe the byte-identical file (same SHA-1),
  one with `INDEX 00` lines and one without.
  - With INDEX 00 (sheets 1, 3, 5, 6): the 150 sectors between INDEX 00
    and INDEX 01 are Mode 1 sectors with valid headers and all-zero user
    data (read on sheet 5's file). On sheet 3's disc the stored gap is
    176 sectors, so a gap is not always 150.
  - Without INDEX 00 (the other eight): the same sectors are the tail of
    the previous track. On the R5 disc they are not zero: every sector
    sampled among the 150 before each of tracks 2 and 3 starts with the
    bytes `54 44 49 01 50 01 01 01` (`TDI`; INFERRED: the track
    descriptor a recorder writes into the gap of a track that was
    recorded on its own).
  Either way the junk is outside or at the very end of a file-system
  volume and harms nothing.
- All tracks are Mode 1. The sheets say `MODE1/2352`, except sheet 11,
  whose file is cooked (`MODE1/2048`).
- A BFS track of these discs is 300 to 525 MiB of cooked data; the track
  file is 600 to 740 MiB. Section 15 has the consequences.

### 9.4 The real images

The sheets were downloaded whole. Of the track files only individual
sectors at the track boundaries were read; "sectors read" marks those.
None of this data is in the repository. Item names are identifiers at
the Internet Archive (`https://archive.org/details/` followed by the
identifier).

| # | Item identifier | Sheet SHA-256 (first 16 hex digits) | Track file size | FILE name is correct | Tracks (INDEX 01 frame; INDEX 00 frame if given) |
|--:|:--|:--|--:|:--|:--|
| 1 | `BEOS_Be_Operating_System_Release_4.5_Be_Inc._1999` | `dd2f93fbef98f299` | 773,198,832 | no (`BeOS_Tools.bin`) | 0; 18,150 (18,000); 174,934 (174,784) |
| 2 | `BeOS_503` | `bee267fd24927821` | 772,302,720, sectors read | yes | 0; 48,658; 207,528 |
| 3 | `BeOS_For_Intel_Pentium_Release_3_100221-30_Be_Inc._1998` | `22cbb7c1d1757edc` | 633,786,384 | no (`BEOS_TOOLS.BIN`) | 0; 2,858 (2,682) |
| 4 | `BeosVersion5.0.3IntelAndPpc` | `6f4fd9fbf7dff01d` | 772,302,720 | base name only (absolute path) | 0; 48,658; 207,528 |
| 5 | `be-os-4-5` | `4f553366a9f61d64` | 773,198,832, sectors read | yes | 0; 18,150 (18,000); 174,934 (174,784) |
| 6 | `be-os-4` | `c260b63ac325979a` | 739,228,896 | yes | 0; 3,402 (3,252); 160,186 (160,036) |
| 7 | `beos-3.0` | `a0b9a8e70904fe24` | 633,786,384, sectors read | yes | 0; 2,858 |
| 8 | `beos-4.0` | `58b6970c779eda0f` | 739,228,896 | yes | 0; 3,402; 160,186 |
| 9 | `beos-4.5.2` | `e961fff86f0edae8` | 772,650,816 | yes | 0; 18,158; 185,012 |
| 10 | `beos-5.0.3-professional-gobe` | `a57d9552cdadbbdb` | 772,302,720 | yes | 0; 48,658; 207,528 |
| 11 | `gobe-productive-2.0` | `6a4114664844cf06` | 207,900,672, sectors read | yes | 0; 49,302 (2048-byte sectors) |
| 12 | `thebeosarchive-beos-r5.1-dano` | `20994360002b2bc1` | 363,374,592 | yes | 0; 896 |

Sheets 2, 4 and 10 describe the same track file (SHA-1
`a4bfd56ca3ada6b33ba74951bd10cce2589afaf3` as listed by the archive);
sheets 6 and 8 likewise (`21ef9c9092372ab69a099efd4b810dd3d0c73708`).

Expected track sizes under this specification, for the three files that
were measured most closely (sectors; cooked bytes are sectors × 2048):

| Sheet | Track 1 | Track 2 | Track 3 |
|--:|--:|--:|--:|
| 5 | 18,000 | 156,634 | 153,807 |
| 2 | 48,658 | 158,870 | 120,832 |
| 7 | 2,858 | 266,609 | none |

(Sheet 5: the file has 328,741 sectors; track 2 runs from 18,150 to
174,784. Sheet 2: 328,360 sectors. Sheet 7: 269,467 sectors.)

## 10. Entries

All of this section is **DECISION**.

### 10.1 One entry per track

Each track that can be read becomes one entry, in sheet order. Tracks
that cannot be read produce no entry (section 10.4). There are no
directories. Nothing else is listed: not the sheet, not the track files
as such, not the gaps.

The entry's path is the container's prefix, a slash, and the entry name.

### 10.2 Data tracks

- Name: `track`, the track number as two decimal digits, `.img`. Track 2
  is `track02.img`.
- Content: the cooked data of the track (section 6.4): 2048 bytes per
  sector.
- Reasons:
  - Cooked data is what every file-system reader in the repository
    addresses. The BFS reader has no raw-sector support, and the ISO
    9660 and HFS readers detect raw sectors only when the whole input is
    raw from its first byte and a multiple of 2352 bytes long, which a
    track cut out of a mixed file need not satisfy for long. Cooking in
    one place removes the question.
  - The name carries no claim about the content. The partition
    containers name their entries `p0.img`, `p1.img` and leave
    recognition to content detection (`src/formats/dos/mbr.rs`,
    `gpt.rs`); a track is the same kind of thing. The extension `iso`
    would be wrong for a BFS track and is, in builds with Macintosh
    support, a hint towards HFS. The extension `bin` is a hint towards
    MacBinary. `img` is no hint at all, so the content decides: BFS, HFS,
    FAT, a partition map or ISO 9660.
  - The partition containers replace `pN.img` by the volume label when
    one is found. Tracks are **not** named after labels: two tracks of a
    BeOS disc can carry the same volume name, and track numbers are what
    a user sees in every other tool.

### 10.3 Audio tracks

- Name: `track`, the two-digit track number, `.pcm`. Track 3 is
  `track03.pcm`.
- Content: the track's samples, unchanged in order, 16-bit signed,
  **little-endian**, two channels interleaved left then right, 44,100
  sample pairs per second; 2352 bytes per sector. There is no header of
  any kind.
  - From a `BINARY` file: the stored bytes as they are.
  - From a `MOTOROLA` file: the stored bytes with each pair of bytes
    exchanged (bytes 0 and 1, bytes 2 and 3, and so on).
  - From a `CDG` track: the first 2352 bytes of each 2448-byte stored
    sector, treated as above; the sub-channel bytes are dropped.
- Nothing is decoded, resampled or wrapped. A `.pcm` entry is not a
  container and is not descended into. This holds whatever the samples
  look like: content detection is not run on the entry.

### 10.4 Tracks that cannot be read

A track of section 7.5 produces no entry. The reader reports what it
left out in the way the repository's other readers report skipped
entries (compare the ARJ reader's summary of missing volumes): a message
that names the first problem and counts the rest. A message names the
track number and the reason, for instance that the file `X` named by the
sheet was not found, that the track's file is of type `WAVE`, or that
the track starts beyond the end of its file.

A sheet all of whose tracks are left out opens successfully and lists
nothing; this is the normal result of opening a sheet without a lookup.

### 10.5 Gaps

The sectors between a track's INDEX 00 and INDEX 01 are in no entry.

- Before a data track they are filler, and a file system starts at
  INDEX 01; including them in either neighbour would be wrong for the
  following track and useless for the preceding one.
- Before an audio track they are usually silence. Occasionally they hold
  audio (a hidden passage before track 1, or applause between tracks of
  a live recording). That audio is not listed. This is accepted for a
  reader whose purpose is file extraction.

### 10.6 Metadata

Entries carry no mode string, no compression information and no
type/creator codes. The entry's size is the size of its content. TITLE,
PERFORMER, ISRC, CATALOG and FLAGS are not surfaced: the entry metadata
has no fields for them.

### 10.7 Track files that the sheet consumed

When a sheet is opened inside a directory or archive whose listing also
contains its track files, the files that were found and used through the
sheet should not be listed a second time as entries of their own, as
later volumes of a multi-volume archive are not. Otherwise a single-file
image whose track 1 is ISO 9660 would be listed twice, once per track
and once as a raw image holding track 1 only.

## 11. Worked examples and test vectors

### 11.1 A three-track BeOS disc (sheet 5 of section 9.4)

The sheet is the one quoted in section 2. The file has 773,198,832
bytes, which is 328,741 sectors of 2352 bytes with no remainder. All
tracks have stored sector size 2352, so byte offset = frame × 2352.

| | Track 1 | Track 2 | Track 3 |
|:--|--:|--:|--:|
| First mark | INDEX 01, frame 0 | INDEX 00, frame 18,000 | INDEX 00, frame 174,784 |
| Span | 0 to 18,000 | 18,000 to 174,784 | 174,784 to 328,741 |
| INDEX 01 frame | 0 | 18,150 | 174,934 |
| Data, frames | 0 to 18,000 | 18,150 to 174,784 | 174,934 to 328,741 |
| Data, first byte in file | 0 | 42,688,800 | 411,444,768 |
| Sectors | 18,000 | 156,634 | 153,807 |
| Entry | `track01.img` | `track02.img` | `track03.img` |
| Entry size | 36,864,000 | 320,786,432 | 314,996,736 |

Checks against the file (CONFIRMED (real data)):

- Byte 42,688,800 starts a sector with sync pattern and header `04 04 00
  01`. Its user data starts at byte 42,688,816. The bytes `1SFB` are at
  42,688,816 + 544 = 42,689,360, which is offset 544 of `track02.img`.
- Byte 411,444,768 starts a sector with header `38 54 34 01`. Its user
  data starts with the 13 bytes `BeOS R4.5 PPC` and has `BFS1` at offset
  32: offsets 0 and 32 of `track03.img`.
- `track01.img` has `CD001` at offset 32,769 (cooked sector 16, byte 1)
  and `BD` at offset 1024.
- The sectors at frames 18,000 to 18,149 have valid headers and all-zero
  user data. They are in no entry.

Had the sheet no INDEX 00 lines (the style of sheet 8), the same file
would give 18,150 sectors for track 1 and 156,784 for track 2, each with
150 filler sectors at its end, and track 3 unchanged.

### 11.2 A file that mixes sector sizes

    FILE "MIXED.BIN" BINARY
      TRACK 01 MODE1/2048
        INDEX 01 00:00:00
      TRACK 02 AUDIO
        INDEX 00 00:00:20
        INDEX 01 00:00:30

with a file of 135,040 bytes.

- Span of track 1: frames 0 to 20, sector size 2048: bytes 0 to 40,960.
- Span of track 2: starts at frame 20, at byte 40,960 (not 20 × 2352 =
  47,040). Sector size 2352.
- Track 2's INDEX 01 is 10 frames into its span: byte 40,960 + 10 × 2352
  = 64,480.
- Track 2's data runs to the end of the file: 135,040 − 64,480 = 70,560
  bytes = 30 sectors exactly.
- Entries: `track01.img`, 40,960 bytes (the first 40,960 bytes of the
  file unchanged); `track02.pcm`, 70,560 bytes.

### 11.3 A purpose-built image

The image below was built for the experiments and is small enough to
build again inside a test. It has 77 sectors of 2352 bytes (181,104
bytes).

Construction, sector by sector (frame numbers from 0):

- **Frames 0 to 19: track 1, Mode 1.** Sector at frame *n*: sync
  pattern; header with the address *n* + 150 in binary-coded decimal
  minute, second, frame and mode `01` (frame 0: `00 02 00 01`; frame 19:
  `00 02 19 01`); 2048 bytes of user data; 288 zero bytes. The user data
  of the sector with index *i* (0 to 19) is the 10-character ASCII text
  `T1-`, *i* as six decimal digits, `|` (for example `T1-000007|`),
  repeated and cut off at 2048 bytes.
- **Frames 20 to 29: gap of track 2.** 2352 zero bytes each.
- **Frames 30 to 59: track 2, audio.** Sector with index *i* (0 to 29)
  holds 588 sample pairs; pair *k* (0 to 587) is the left sample *v* and
  the right sample −*v*, where *v* = (*i* × 588 + *k*) modulo 30,000;
  both as signed 16-bit little-endian. (The first sector starts `00 00
  00 00 01 00 FF FF`.)
- **Frames 60 to 64: gap of track 3.** Mode 2 Form 1 sectors as below
  with all-zero user data.
- **Frames 65 to 76: track 3, Mode 2.** Sector at frame *n*, index *i* =
  *n* − 65 (0 to 11): sync pattern; header with the address *n* + 150
  and mode `02` (frame 65: `00 02 65 02`); 8 bytes of sub-header; user
  data; zero bytes up to 2352.
  - Index 7 is a Form 2 sector: sub-header `00 00 20 00 00 00 20 00`;
    2324 bytes of user data, the text `F2-000007|` repeated and cut off;
    4 zero bytes.
  - All others are Form 1 sectors: sub-header `00 00 08 00 00 00 08 00`;
    2048 bytes of user data, the text `T3-`, *i* as six digits, `|`,
    repeated and cut off; 280 zero bytes.

The sheet:

    FILE "disc.bin" BINARY
      TRACK 01 MODE1/2352
        INDEX 01 00:00:00
      TRACK 02 AUDIO
        INDEX 00 00:00:20
        INDEX 01 00:00:30
      TRACK 03 MODE2/2352
        INDEX 00 00:00:60
        INDEX 01 00:00:65

Expected results. The first three rows are also what bchunk 1.2.2 writes
for this image (CONFIRMED (crafted); it names them `…01.iso`, `…02.cdr`,
`…03.iso`).

| Item | Size | SHA-256 |
|:--|--:|:--|
| the image `disc.bin` | 181,104 | `81d8fd63a9088a94d0c4260230696536eb2d667b8cb0ef2a889eeab990e4527c` |
| `track01.img` | 40,960 | `29a24186333aed9ef03ffbfd4ab452d397d634c03d967a27b30895f8575aaf3f` |
| `track02.pcm` | 70,560 | `e7461c0534743f4e9f7833e84c20327dd9959436e44bcf9acf10fa5d0dac83d5` |
| `track03.img` | 24,576 | `25a6f1093fdf46c5acac4800b2b485dfe079845a8afdf784a37150805bdd7f98` |

`track03.img` consists of the 2048 user bytes of sectors 0 to 6 and 8 to
11 and, in seventh position, the first 2048 bytes of the Form 2 sector's
user data.

Variants of the sheet over the same image:

| Variant | Expected |
|:--|:--|
| Both `INDEX 00` lines removed | `track01.img` is 61,440 bytes (30 sectors, the last 10 cooked from all-zero sectors by the declared-mode fallback: offset 16), SHA-256 `fa739a414a99027a4692097cbaf402ed71a794fffff0b3864a627bfa589f7aa1`. `track02.pcm` is 82,320 bytes (35 sectors, the last 5 being the raw Mode 2 gap sectors), SHA-256 `def3ef792f95f9eeae0218b7784f9061dc1cf6dae42320e3bca5fdfd721f4522`. `track03.img` unchanged. bchunk agrees; CONFIRMED (crafted). |
| `BINARY` replaced by `MOTOROLA` | `track01.img` and `track03.img` unchanged. `track02.pcm` has every byte pair exchanged: SHA-256 `eb1fb2325d4cacdbfe319b5d129ad059e2761bb0d55da619362175ceedcc28bc`. |
| The whole sheet in lower case, with CR LF line ends and a UTF-8 byte order mark, naming `DISC.BIN` | The same three entries as the unmodified sheet, provided the lookup finds `disc.bin` (candidate 3 of section 8.1 asks for `disc.bin`). |
| Track 2's `INDEX 00` line replaced by `PREGAP 00:02:00` | `track01.img` as in the first variant (30 sectors); `track02.pcm` and `track03.img` as for the unmodified sheet. The PREGAP line changes nothing. |
| `MODE1/2352` replaced by `MODE2/2352` in track 1 | Unchanged output: the sectors' own mode byte `01` decides. |
| `INDEX 01 00:00:65` replaced by `INDEX 01 00:02:00` (frame 150, beyond the 77 sectors) | Tracks 1 and 2 unchanged; track 3 reported, no entry. |
| `INDEX 01 00:00:30` replaced by `INDEX 01 00:00:10` (before the INDEX 00 at frame 20) | The file's layout is inconsistent: no entries, three tracks reported. |
| FILE name replaced by `C:\IMAGES\disc.bin`, by `/tmp/x/disc.bin`, or by `..\..\disc.bin` | The lookup is asked for `disc.bin` (and its case variants, and the sheet-stem candidates) only, never for a path with a directory part. |
| FILE name replaced by `sub\disc.bin` | The lookup is asked for `sub/disc.bin` first, then `disc.bin`. |

Two derived files test the other stored sizes:

| File | Construction | Sheet | Expected |
|:--|:--|:--|:--|
| 39,712 bytes, SHA-256 `37c9cbd9fe2ff0a31bffc8fb0cc1f98a2375f21315d181b47ef8964118593160` | bytes 16 to 2351 of each of the image's sectors at frames 60 to 76 | one track `MODE2/2336` with `INDEX 00 00:00:00` and `INDEX 01 00:00:05` | one entry equal to `track03.img` above (named by its track number) |
| 135,040 bytes, SHA-256 `7b03ee51a590fa757899983234885905205ea7b2ab177b675e529c8faf9c90ce` | `track01.img` above followed by the image's sectors at frames 20 to 59 | the sheet of section 11.2 | `track01.img` and `track02.pcm` as in the table above |

## 12. Observed implementation gaps

Nothing in this section is to be imitated.

- **bchunk 1.2.2** recognises commands by searching each line for the
  upper-case text `TRACK` or `INDEX` anywhere in it, so a file name or
  title containing either word is misread, and a lower-case sheet yields
  nothing (CONFIRMED (crafted)). It multiplies every time by 2352
  whatever the track mode and reads `MODE2/2336` tracks in 2352-byte
  steps. It ignores FILE lines and so cannot read multi-file sheets. It
  skips the first line of the sheet unread.
- **libcdio 2.2.0** compares keywords in upper case only and rejects a
  sheet with a byte order mark (CONFIRMED (crafted)). `cd-info` opens the
  track file under the sheet's name with `.bin` and crashes when that
  file does not exist (CONFIRMED (crafted): segmentation fault). It
  addresses the file in 2352-byte steps for every mode and warns when a
  stored gap is shorter than 150 sectors, which is not an error.
- **DOSBox Staging** and **libcue** read sheet lines into fixed-size
  buffers; DOSBox Staging rejects sheets with any command it does not
  know. libcue's keyword list contains the misspelt modes `MODE2/2342`
  and `MODE2/2332`.
- **MAME** decides how a multi-session image is padded from the track
  file's extension.
- Several readers resolve the FILE name by joining it to the sheet's
  directory without removing `..` components or absolute paths, so a
  sheet can make them open any file the process can read (section 15).

## 13. Open points

- **No real `MOTOROLA` image was available.** That the type exchanges
  the bytes of audio samples and leaves data tracks alone is CDRWIN's
  wording plus MAME's reading of it.
- **No real image with a file of mixed sector sizes was available.** The
  span rule of section 7.3 for that case is REFERENCE-ONLY (two
  independent tools agree).
- **`CDG`, `CDI/…`, `MODE2/2336`, `MODE2/2048` tracks were not seen in
  real sheets.** Their layouts are CDRWIN's and ECMA-130's; `MODE2/2352`
  cooking is CONFIRMED (crafted) only.
- **Mode 2 without forms** is read as XA (section 6.4). A disc of that
  kind would be read 8 bytes off. None was seen.
- **Multi-session images in one file.** Imaging tools differ in whether
  the lead-out and lead-in between two sessions are stored in the file
  (MAME documents both, REFERENCE-ONLY). The marks of the sheet are file
  positions in either case, so the rules here need no session
  knowledge; this was confirmed only on sheet 2, whose single `REM
  SESSION 1` line changes nothing.
- **Sub-channel data stored beside the track file** (a `.sub` file) and
  **sub-channel data interleaved in every sector of a data track** (2448
  bytes per sector under a mode word that does not say so) are not
  handled. The second would make every track after the first start at
  the wrong byte; no such sheet was seen.
- **Cue sheets written for audio containers** (`WAVE` and others) are
  recognised as sheets and list nothing. Whether they should be
  recognised at all is a product question; recognising them makes the
  report of section 10.4 available.

## 14. Decisions

| # | Decision | Section |
|--:|:--|:--|
| 1 | A sheet is at most 1 MiB. | 3.1 |
| 2 | Encoding by byte order mark, else UTF-8 if valid, else one byte per character. | 3.1 |
| 3 | U+001A ends the sheet; U+0000 disqualifies it. | 3.2 |
| 4 | No escape character inside quotes; a FILE name runs to the last quote of the line; unquoted names with spaces are accepted. | 3.3 |
| 5 | Extra arguments are ignored. | 3.3 |
| 6 | Command words, types, modes and flags are case-insensitive. | 3.4 |
| 7 | The recognition test. | 3.5 |
| 8 | Unknown commands are ignored. | 4 |
| 9 | An absent FILE type is `BINARY`; files of other types are never looked up and their tracks are reported. | 4.1 |
| 10 | Byte order applies to audio tracks only. | 4.1 |
| 11 | An unknown mode of the form word/number keeps later tracks locatable. | 4.2 |
| 12 | Only the first INDEX 00 and INDEX 01 of a track count. | 4.3 |
| 13 | PREGAP and POSTGAP never change a file offset. | 4.4 |
| 14 | Which defects make a sheet invalid and which are skipped. | 4.6 |
| 15 | Time fields have 1 to 4 digits and are not range-checked. | 5 |
| 16 | Data tracks are cooked to 2048 bytes per sector; the sector's own mode byte decides between offsets 16 and 24 when it has a sync pattern; a Form 2 sector contributes its first 2048 bytes; `MODE2/2324` is unsupported. | 6.4 |
| 17 | Marks that go backwards make the whole file unusable. | 7.2 |
| 18 | Bytes before a file's first mark are skipped. | 7.3 |
| 19 | An unknown sector size makes the track and all later tracks of the file unusable. | 7.3 |
| 20 | A track ends at the next span or the end of the file; ends are clamped to the file, starts are not; a trailing partial sector is ignored; empty tracks are reported. | 7.4 |
| 21 | The candidate names and their order; no candidate ever leaves the sheet's directory tree. | 8.1 |
| 22 | What the lookup may do beyond exact names. | 8.2 |
| 23 | FILE commands with the same name are one file. | 8.3 |
| 24 | One entry per readable track; names `trackNN.img` and `trackNN.pcm`; cooked data and little-endian PCM; gaps in no entry; unreadable tracks reported, not listed; consumed track files not listed again. | 10 |
| 25 | The limits of section 15. | 15 |

## 15. Safety requirements

A cue sheet is untrusted text that names files and offsets. The
following are requirements.

1. **File names are lookup keys and nothing else.** The reader never
   opens a file itself; it passes candidate names to the lookup. It must
   pass only the candidates of section 8.1: never an absolute path,
   never a drive, never a component that is empty, `.` or `..`, never a
   name containing a colon or U+0000. A sheet naming
   `..\..\..\etc\passwd` leads to a lookup of `passwd` beside the sheet
   and of nothing else. The code implementing the lookup must apply the
   same rule on its side and must not follow a requested name out of the
   sheet's directory tree.
2. **Names from the sheet never become entry names.** Entry names are
   built from track numbers only (section 10), so they cannot contain
   separators or traversal components.
3. **Bounded input.** A sheet above 1 MiB is rejected before decoding.
   At most 99 FILE commands and 99 tracks are accepted (section 4.6);
   this also bounds the number of lookups at 99 files times the at most
   nine candidates of section 8.1.
4. **Each file is fetched once** (section 8.3), however many FILE
   commands name it, and a file of an unsupported type or without tracks
   is not fetched at all.
5. **All arithmetic is checked.** Frame counts come from up to
   four-digit fields (at most 45,755,424 frames); offsets are frames
   times a sector size of at most 2448 and are compared with the real
   file length before any byte is read. An overflow is treated as a
   position beyond the end of the file.
6. **Every range is validated against the real file length before
   reading**: start below the length, end clamped to the length, whole
   sectors only (section 7.4). No sector is read partially and no byte
   outside the file is assumed to be zero.
7. **Tracks never overlap.** Marks that go backwards disqualify the file
   (section 7.2). Together with requirement 6 this guarantees that the
   total size of all entries of a file is at most the file's size, and
   cooked entries are smaller still (2048 of every 2352 bytes). A sheet
   cannot make the reader produce more data than it was given; there is
   no amplification.
8. **Output is still budgeted.** Entry sizes are checked against the
   repository's limit for a single output and charged to an output
   budget derived from the total input size (sheet plus track files),
   like the entries of the file-system readers. An entry that exceeds a
   limit is reported and skipped; the others are still produced.
9. **No allocation from the sheet alone.** Buffers are sized from the
   validated range of a track, never from a time value or count in the
   sheet before validation.
10. **Memory.** Real track files reach 740 MiB and one cooked track 520
    MiB (section 9.3). The reader should produce one entry at a time and
    release it before producing the next, and should not hold cooked
    copies of all tracks at once.
11. **Sector contents are data.** Header addresses, modes and
    sub-headers found in sectors are not trusted for anything except the
    choice between user-data offsets 16 and 24 (section 6.4); a wrong
    value there selects different bytes of the same sector and nothing
    else.
12. **No recursion from the sheet.** `CDTEXTFILE` names and any other
    file names in ignored commands are never looked up. A track file is
    never interpreted as a cue sheet by this reader.
13. **Failure is local.** A track that cannot be read does not prevent
    the other tracks from being listed. Only the defects of section 4.6
    fail the whole sheet.
