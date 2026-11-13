# Unix `compress` (`.Z`) format specification: index

This directory specifies the file format written by the Unix `compress`
program (magic bytes `1F 9D`, variable-width LZW). The reader in this
repository is implemented from the document listed in section 2 and from
nothing else.

## 1. Purpose and provenance

- The document is a **prose specification**. It was derived from the
  public documentation and the source code of existing implementations
  (section 3), and from black-box experiments with six existing decoders
  and two existing encoders on purpose-built input.
- The work is a **clean-room process**. The writer of the document could
  read the sources of section 3; the implementation is written **from the
  document only**, by an implementer who does not read the source of any
  `compress` implementation. The document therefore contains no program
  code and no pseudo-code, only prose, tables of constants, worked
  examples and test vectors. The consulted sources and the tools built
  for the experiments are kept outside this repository.
- Where existing implementations disagree with each other, or are unsafe,
  the document says so in its section "Observed implementation gaps" and
  specifies one behaviour. Nothing in that section is to be imitated.
- Statements carry evidence labels, defined in section 1.2 of the
  document. A statement labelled **DECISION** is a choice made by the
  specification, not a fact about the format.
- All test vectors are generated content (short ASCII strings, ascending
  byte values, output of `seq` and of a stated linear congruential
  generator). No file from the internet is involved.

## 2. Documents

| Document | Contents |
|---|---|
| [`format.md`](format.md) | The whole format: the 3-byte header and its flag bits, bit packing, the code space in block and non-block mode, decoding including the KwKwK case, code-width growth, the groups of eight codes and the padding skipped at a width change and after a CLEAR code, the full table, the two incompatible readings of maximum width 9, how a stream ends, the empty file, detection, naming conventions (`.Z`, `.tar.Z`, `.taz`, NeXT `.compressed` and `.pkg`), safety requirements, worked byte-level examples and test vectors, decisions, implementation gaps, open points. |

## 3. Sources consulted

| Source | Licence | Used for |
|---|---|---|
| (N)compress, `compress.c` and `compress.1`, <https://github.com/vapier/ncompress> (main branch, fetched 2026-11-12) | Public domain (Unlicense) | Header layout, constants, decoder and encoder behaviour, the newer reading of maximum width 9 |
| GNU gzip, `unlzw.c`, <https://git.savannah.gnu.org/cgit/gzip.git> (current, and the 1.6-era copy) | GPL-3.0-or-later; the file states it is derived from the public-domain `compress` | Decoder behaviour, treatment of reserved flag bits, the older reading of maximum width 9 |
| GNU gzip manual page `gzip.1` (1.6-era copy) | GPL-3.0-or-later | Suffix conventions (`.taz`), the statement that the format allows no consistency check |
| FreeBSD `usr.bin/compress/zopen.c`, <https://github.com/freebsd/freebsd-src> | BSD-3-Clause | The original 4.0 encoder and decoder logic, non-block mode, the minimum width that decoder accepts |
| zlib `examples/gun.c` (function for LZW input), <https://github.com/madler/zlib> | zlib licence | An independent decoder's validity checks and its remark on maximum width 9 |
| T. A. Welch, "A Technique for High-Performance Data Compression", IEEE Computer 17(6), June 1984, pp. 8-19 | Journal article | Named by all of the above as the origin of the algorithm; cited, not re-read for this work |
| NeXT Installer release notes (OPENSTEP 4.2 `NextLibrary/Documentation/NextAdmin/ReleaseNotes/Installer.rtf`) and the "Naming Convention" file of the Peanuts NeXT archive CD-ROM, as indexed at <https://discmaster.textfiles.com> (search-result summaries only) | Vendor documentation / archive notes | Section 15: the `.tar.Z` member of a `.pkg`, the meaning of `.compressed` |

Tools run as black boxes:

| Tool | Version |
|---|---|
| `/usr/bin/compress`, `/usr/bin/uncompress` | macOS 27.0.1, `file_cmds-487.0.1` (the BSD implementation) |
| `/usr/bin/gzip -dc` | Apple gzip 487.0.1 (BSD gzip) |
| `compress`, `uncompress` | (N)compress 4.2.4.6, Debian 12 package `ncompress 4.2.4.6-6` |
| `gzip -dc` | GNU gzip 1.12, Debian 12 package `gzip 1.12-1` |
| `busybox zcat` | BusyBox 1.35.0, Debian 12 package |
| `bsdcat` | libarchive 3.6.2, Debian 12 package `libarchive-tools` |

Other documents in this directory add their own row to section 2.
