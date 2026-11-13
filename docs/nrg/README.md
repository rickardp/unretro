# Nero NRG disc image specification: index

This directory specifies how the reader in this repository splits a Nero
Burning ROM disc image (`.nrg`) into tracks. The implementation is
written from these documents only.

## 1. Purpose and provenance

- These documents are **prose specifications**. Nero never published the
  format. They were derived from public descriptions of it, from reading
  the source of existing open tools for facts about the layout and about
  their behaviour, from black-box runs of one such tool on purpose-built
  images, and from measurements on real disc images.
- The work is a **clean-room process**. The writer of these documents
  could read existing implementations; the implementation is written
  **from these documents only**, by implementers who do not read the
  source of any NRG reader. The documents therefore contain no program
  code and no pseudo-code, only prose, tables of constants, field
  layouts, worked examples and test vectors. Hexadecimal dumps quoted as
  examples are data. Reference sources and downloaded images are kept
  outside this repository.
- Where an existing tool is wrong or unsafe, the document says so in its
  section "Observed implementation gaps" and specifies the correct
  behaviour instead. Nothing in that section is to be imitated.
- Statements carry evidence labels, defined in section 1.2 of
  [`nrg.md`](nrg.md). A statement labelled **DECISION** is a choice made
  by the specification, not a fact about the format.
- Real disc images are never committed. Expected results for them are
  kept outside the repository.
- The entry model (one entry per track, `trackNN.img` and `trackNN.pcm`,
  cooked data) is that of the CUE/BIN reader, specified in
  [`../cue/cue.md`](../cue/cue.md). Where this specification repeats a
  rule of that document it says so.

## 2. Documents

| Document | Contents |
|---|---|
| [`nrg.md`](nrg.md) | The footer of both format versions and how a file is recognised, the chunk chain, every known chunk with its fields and whether it is used, the track mode codes and the sector layouts behind them, where track data and pregaps are in the file for disc-at-once and track-at-once images, multi-session images, how byte ranges are derived and validated, how tracks are exposed as entries, real images measured, worked examples, test vectors, safety requirements. |

## 3. Sources consulted

Every source below was read by the specification writer. None of them is
to be read by the implementer.

| Source | Kind | Licence / status |
|---|---|---|
| Wikipedia, "NRG (file format)", revision 1307992662 of 2025-08-26 (the last before the article became a redirect to "Nero Burning ROM") | documentation; the article itself is marked as original research | CC BY-SA 4.0 |
| GNU libcdio manual, section "NRG Format" (`doc/libcdio.texi`, `https://github.com/libcdio/libcdio`) | documentation | GFDL 1.2+ |
| ECMA-130, section 14 (sector structure), as restated in [`../cue/cue.md`](../cue/cue.md) section 6 | standard | freely available ECMA standard |
| libcdio `lib/driver/image/nrg.h` and `nrg.c` (development head as fetched in November 2026), `test/check_nrg.sh.in`, `test/videocd.right` | tool source and test expectations, read for facts | GPL-3.0-or-later |
| libMirage (CDemu) `images/image-nrg/image-nrg.h` and `parser.c` (development head, November 2026) | tool source, read for facts | GPL-2.0-or-later |
| Aaru `Aaru.Images/Nero/` (`Constants.cs`, `Enums.cs`, `Structs.cs`, `Helpers.cs`, `Identify.cs`, `Read.cs`; branch `devel`, November 2026) | tool source, read for facts | LGPL-2.1 |
| MAME `src/lib/util/cdrom.cpp` (function reading Nero images for `chdman`) | tool source, read for facts | BSD-3-Clause |
| `cd-info` (libcdio-utils 2.2.0) from Debian stable, run in a container on the purpose-built images of section 12 of `nrg.md` and on the two libcdio test images | black-box experiments | as above |
| Two whole NRG files from the libcdio repository, `test/data/p1.nrg` and `test/data/videocd.nrg` | measurements on real data | distributed with libcdio |
| 169 NRG files from 87 Internet Archive items: the last 8 KiB of each (footer and chunk chain) and HTTP range reads of sectors at track starts of 17 of them; seven further files with the extension `.nrg` that are not NRG images | measurements on real data | the images are not redistributed; see section 9 of `nrg.md` for identifiers and digests |

Other documents in this directory add their own row to section 2.
