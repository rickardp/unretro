# CUE/BIN CD image specification: index

This directory specifies how the reader in this repository splits the
track files named by a cue sheet into tracks. The implementation is
written from these documents only.

## 1. Purpose and provenance

- These documents are **prose specifications**. They were derived from
  public documentation of the cue sheet format, from the published
  standard for CD-ROM sectors, from reading the source of existing open
  tools for facts about their behaviour, from black-box tests of two such
  tools with purpose-built images, and from measurements on real disc
  images.
- The work is a **clean-room process**. The writer of these documents
  could read existing implementations; the implementation is written
  **from these documents only**, by implementers who do not read the
  source of any cue sheet reader. The documents therefore contain no
  program code and no pseudo-code, only prose, tables of constants,
  worked examples and test vectors. Cue sheets quoted as examples are
  data. Reference sources and downloaded images are kept outside this
  repository.
- Where an existing tool is wrong or unsafe, the document says so in its
  section "Observed implementation gaps" and specifies the correct
  behaviour instead. Nothing in that section is to be imitated.
- Statements carry evidence labels, defined in section 1.2 of
  [`cue.md`](cue.md). A statement labelled **DECISION** is a choice made
  by the specification, not a fact about the format.
- Real disc images are never committed. Expected results for them are
  kept outside the repository.

## 2. Documents

| Document | Contents |
|---|---|
| [`cue.md`](cue.md) | The cue sheet text format (encodings, lines, quoting, commands and which of them matter), FILE types, TRACK modes and the raw sector layouts behind them, the time format, how track boundaries and byte offsets are derived, how track file names are resolved, mixed-mode layouts including BeOS install discs, how tracks are exposed as entries, worked examples, test vectors, safety requirements. |

## 3. Sources consulted

Every source below was read by the specification writer. None of them is
to be read by the implementer.

| Source | Kind | Licence / status |
|---|---|---|
| CDRWIN User's Guide, Appendix A "Cue sheet commands" (Golden Hawk Technology), as mirrored at `digitalx.org/cue-sheet/syntax/` (Internet Archive capture of 2016-01-13) | documentation | proprietary manual, quoted for facts only |
| GNU libcdio manual, chapters "CD Image Formats" and "The units that make up a CD" (`doc/libcdio.texi`, `https://github.com/libcdio/libcdio`) | documentation | GFDL 1.2+ |
| Hydrogenaudio Knowledgebase, "Cue sheet" (`https://wiki.hydrogenaudio.org/index.php?title=Cue_sheet`, Internet Archive capture of 2023) | documentation | GFDL / CC BY-SA as stated by the site |
| Wikipedia, "Cue sheet (computing)" | documentation | CC BY-SA 4.0 |
| ECMA-130, "Data interchange on read-only 120 mm optical data disks (CD-ROM)", 2nd edition, section 14 (sector structure). Its field sizes were taken as restated in the header comment and constants of libcdio's `include/cdio/sector.h` and then checked on real images. | standard | freely available ECMA standard |
| libcdio `include/cdio/sector.h` and `lib/driver/image/bincue.c` (release 2.2.0 and the development head of November 2026) | tool source, read for facts | GPL-3.0-or-later |
| bchunk `bchunk.c` (version 1.2.2, `https://github.com/hessu/bchunk`) | tool source, read for facts | GPL-2.0-or-later |
| MAME `src/lib/util/cdrom.cpp` (cue sheet parsing used by `chdman`) | tool source, read for facts | BSD-3-Clause |
| DOSBox Staging `src/dos/cdrom_image.cpp` | tool source, read for facts | GPL-2.0-or-later |
| libcue `cue_scanner.l` (keyword list only) | tool source, read for facts | GPL-2.0 |
| bchunk 1.2.2 and `cd-info` (libcdio-utils 2.2.0) from Debian stable, run in a container on purpose-built images | black-box experiments | as above |
| Twelve cue sheets and one cdrdao `.toc` file from twelve Internet Archive items holding BeOS and BeOS application discs, and HTTP range reads of sectors at the track boundaries of four of the track files | measurements on real data | the images are not redistributed; see section 9.4 of `cue.md` for identifiers and digests |

Other documents in this directory add their own row to section 2.
