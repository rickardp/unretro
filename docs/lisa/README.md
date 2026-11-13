# Apple Lisa disk specifications: index

This directory specifies the Apple Lisa formats that the reader in this
repository implements from a written specification rather than from
existing code. Each document is the only source its implementation is
written from.

## 1. Purpose and provenance

- These documents are **prose specifications**. There is no official
  on-disk specification of the Lisa file system; the documents were
  derived by studying existing source code and by measurements on real
  disk images.
- The work is a **clean-room process**. The writers of these documents
  could read the sources listed in section 3; the implementation is
  written **from these documents only**, by implementers who do not read
  the source of any Lisa file system or disk image implementation. The
  documents therefore contain no program code and no pseudo-code, only
  prose, tables of constants and offsets, worked examples and test
  vectors. Sources are kept outside this repository.
- Where an existing implementation is wrong or unsafe, a document says so
  in its section "Observed implementation gaps" and specifies the correct
  behaviour instead. Nothing in those sections is to be imitated.
- Statements carry evidence labels, defined in each document's section 1.
  A statement labelled **DECISION** is a choice made by the
  specification, not a fact about the format. A statement labelled
  **APPLE-SOURCE** rests solely on the Lisa OS source code published by
  the Computer History Museum; each document lists all of them in one
  place so that the dependency on that source is visible.
- Real disk images and the results expected for them are kept outside
  the repository and are never committed.

## 2. Documents

| Document | Contents |
|---|---|
| [`lisafs.md`](lisafs.md) | The Lisa file system as written by the Lisa Office System 1.x, 2.x, 3.x and the Lisa Workshop (on-disk versions 14, 15 and 17): the 12-byte and 20-byte sector tags and how they map onto each other, reserved file IDs, the tag checksum; volume layout and how the MDDF is found; the MDDF with detection checks; the S-list; the file leader (hint entry, label, file map); the flat catalog of versions 14 and 15 and the catalog tree of version 17; logical file length; names and the Lisa character set; file types; timestamps; device sizes; what to do when catalog, S-list and tags disagree; how Monitor, Xenix, UniPlus and MacWorks volumes differ; worked byte-level examples from real images; safety requirements. |
| [`images.md`](images.md) | The image files that carry a Lisa disk's tag bytes, and how each becomes blocks plus tags for `lisafs.md`: DiskCopy 4.2 (header, the two checksums and which tag bytes they skip, encoding and format bytes, Macintosh, Lisa 3.5-inch, Twiggy and LisaEm hard-disk representations, the Twiggy second-side track order, known deviations, detection); DART (header variants, block-length table, run-length and LZH chunk compression with test vectors, checksum resources, detection); raw 532-byte-block ProFile and Widget dumps (BLU header block, tag before or after the data, the ProFile 5:1 interleave, a scoring test that recognises the layout); the single 20-byte tag form handed to the file-system layer; safety requirements. |

Other documents in this directory add their own row here.

## 3. Sources and licences

Sources consulted for `lisafs.md`. Other documents add their own
sources below.

| Source | Used for | Licence |
|---|---|---|
| *Operating System Reference Manual for the Lisa* (Apple, 1983, 029-0414-A); *Release 3.0 Notes* to it; Workshop 3.0 release notes in *Lisa Development System Internals Documentation* (February 1984); bitsavers.org `pdf/apple/lisa/os/` and `pdf/apple/lisa/workshop_3.0/` | Path name delimiters, directories (`MAKE_CATALOG`), the names and meanings of file and volume attributes; cross-check of the Apple-source statements (`lisafs.md` section 21.5) | Published product documentation, Apple copyright |
| Aaru Data Preservation Suite, `Aaru.Filesystems/LisaFS` and `Aaru.Decoders/LisaTag.cs` (`github.com/aaru-dps/Aaru`, revision `828d9a8`), by Natalia Portillo | Structure tables, tag layouts, detection rule, record sizes of the version-17 catalog; reference for observed implementation gaps | LGPL-2.1-or-later |
| Claunia.Encoding, `LisaRoman.cs` (`github.com/claunia/Claunia.Encoding`) | Lisa character set to Unicode table | MIT |
| LisaEm, `src/tools/src/lisafsh-tool.c` and `blu-to-dc42.c` (`github.com/rayarachelian/lisaem`, revision `a3ee183`), by Ray Arachelian | Reserved tag file IDs; description of the BLU hard-disk dump layout | GPL (the tool states version 2, the project README version 3) |
| Lisa OS 3.1 source code, published by the Computer History Museum (read from the unofficial mirror `github.com/azumanga/apple-lisa`, revision `80e7f6b`): the file system, directory, driver and loader units of `LISA_OS/OS` | Field names and meanings of MDDF, S-list entry, hint entry, file map and catalog records; the packing of the 12-byte tag; the catalog tree node layout; file-map continuation; path delimiters | Apple Academic License Agreement (non-commercial research, teaching and personal study; no redistribution). Statements depending on it alone are labelled APPLE-SOURCE and listed in `lisafs.md` section 21.4 |
| Disk images from `bitsavers.org/bits/Apple/Lisa/` and one hard-disk image from `github.com/alexthecat123/LisaSourceCompilation` (listed in `lisafs.md` section 21.1) | Measurements; worked examples quote short byte sequences | Third-party software images; not redistributed, not committed |
| The file trees extracted with the Applesauce software that accompany the Lisa Office System 3.1 images on bitsavers | Independent byte-for-byte check of 196 extracted files | Not redistributed |

Sources consulted for `images.md` (details in its section 13):

| Source | Used for | Licence |
|---|---|---|
| Apple II File Type Note `$E0/$0005`, "Apple Disk Copy 4.2 image" | DiskCopy 4.2 header and published encoding/format values | Apple technical note |
| `DartIntf.h` and "Some notes on reading a DART image file", DART 1.5.3 (Apple, 1994), from the MungeImage 1.2.0 source distribution | DART header, block-length units, chunk size, checksum resources | Apple; published "for read-only use" |
| Aaru Data Preservation Suite (`github.com/aaru-dps/Aaru`: DiskCopy 4.2, DART and BLU image readers) and `github.com/aaru-dps/Aaru.Compression.Native` (Apple run-length and LZH decoders) | Field values seen in the wild, Twiggy track order, reference for observed implementation gaps | LGPL-2.1 (Aaru); MIT (decoder library) |
| LisaEm 1.2.7 (`github.com/rayarachelian/lisaem`): `libdc42`, the dump conversion tools, ProFile emulation | LisaEm hard-disk image conventions, ProFile interleave table, dump layouts | GPL v2/v3; `libdc42` LGPL; IDEFile converter BSD-style |
| Cameo/Aphid documentation (`github.com/stepleton/cameo`) | `*.image` layout and sizes | Public domain |
| ESProFile/ArduinoFile documentation (`github.com/alexthecat123/ESProFile`) and Lisa-ProFile-Image-Tools README (`github.com/RolandJuno/Lisa-ProFile-Image-Tools`) | Tag-first layout and 5:1 interleave of emulator images | See repositories |
| The Unarchiver command-line tools (`unar` 1.10.7), run as a black box | Cross-check of an LZH test vector | LGPL-2.1 |
| Disk images from `bitsavers.org/bits/Apple/Lisa/`, Macintosh DiskCopy 4.2 and DART images from public software archives, one hard-disk image from `github.com/alexthecat123/LisaSourceCompilation` (`images.md` section 11) | Measurements; worked examples quote short byte sequences | Third-party software images; not redistributed, not committed |

Not consulted for `lisafs.md`: the Lisa Operating System Reference
Manual and the Lisa hardware manual (the facts they could supply are
covered by the sources above and by measurement).
