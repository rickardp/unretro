# LHA / LZH format specification: index

This directory specifies how to read LHA archives (`.lha`, `.lzh`; also
LArc `.lzs`): the archive container with its four header levels, and the
compression methods found inside. It is the only source the LHA reader in
this repository is written from.

## 1. Purpose and provenance

- These documents are a **prose specification**. They were derived by
  studying the behaviour of an existing decoder (a third-party LHA library,
  version 0.6.2, called the *reference implementation*), from public
  documentation and published descriptions of the format (section 7), and
  by black-box testing of that library and of two independent extraction
  tools (`unar`/`lsar` 1.10.7 and `7z` from p7zip 17.05) with purpose-built
  inputs.
- The work is a **clean-room process**. The people or agents who wrote
  these documents could read the reference implementation; the
  implementation is written **from these documents only**, by implementers
  who do not read the reference implementation's source. The documents
  therefore contain no program code and no pseudo-code, only prose, tables
  of constants, worked examples and test vectors.
- Where the reference implementation is wrong, lenient or unsafe, the
  documents say so (each has a section "Observed implementation gaps") and
  specify the required behaviour instead. Being compatible with the
  reference implementation is not a goal; reading genuine archives
  correctly and rejecting hostile input safely is.
- Every statement carries an evidence label (`container.md` section 1.1).
  A statement labelled **DECISION** is a choice made by this specification,
  not a fact about the format (section 5).
- Synthetic test archives built from these documents are in
  `testdata/lha/` (see its `README.md`). Expected results for real archives
  are kept outside the repository and are never committed.

**Evidence base, stated plainly.** Only one genuine archive was available
while these documents were written (one level-1 header from a Unix writer,
one "-lh5-" entry). Everything else rests on crafted inputs on which
independent decoders agree, and on published descriptions. The points that
rest on limited evidence are collected in section 8.

## 2. Documents and reading order

| Order | Document | Contents |
|--:|---|---|
| 1 | `README.md` (this file) | Index, method table, glossary, decisions, scope, sources. |
| 2 | [`container.md`](container.md) | Archive framing; header levels 0 to 3 field by field; header checksum and header CRC; extended headers; names, paths, entry kinds; timestamps; OS identifiers; end of archive; detection and leading junk; the data CRC-16; safety requirements for the container layer; worked examples. |
| 3 | [`methods.md`](methods.md) | The contract between container layer and decoder; bit order; stored entries; the static Huffman methods "-lh4-" to "-lh7-"; the adaptive Huffman method "-lh1-" with its fixed distance code; the LArc methods "-lzs-" and "-lz5-"; safety requirements for decoders; worked examples and test vectors. |

Reading order for an implementer:

- container layer: `container.md` sections 1 to 3, then the level sections
  4 to 6, then 7 to 10; section 11 has complete archives in hexadecimal to
  test against;
- a decoder: `methods.md` sections 1 and 2, then the section of the method,
  then section 8.

Sections called "Observed implementation gaps" describe faults of the
black boxes. They explain why a rule exists; nothing in them is to be
imitated.

## 3. Compression methods

The method is selected by the five-byte identifier in the entry header and
by nothing else (one exception: `container.md` section 3.3).

| Identifier | Name | Window | Match lengths | Specified in | Result |
|---|---|--:|---|---|---|
| "-lh0-" | stored | | | `methods.md` 3 | decoded |
| "-lz4-" | stored (LArc) | | | `methods.md` 3 | decoded |
| "-lh1-" | adaptive Huffman | 4,096 | 3 to 60 | `methods.md` 5 | decoded |
| "-lh4-" | static Huffman | 4,096 nominal, 8,192 allowed | 3 to 256 | `methods.md` 4 | decoded |
| "-lh5-" | static Huffman | 8,192 | 3 to 256 | `methods.md` 4 | decoded |
| "-lh6-" | static Huffman | 32,768 nominal, 65,536 allowed | 3 to 256 | `methods.md` 4 | decoded |
| "-lh7-" | static Huffman | 65,536 | 3 to 256 | `methods.md` 4 | decoded |
| "-lzs-" | LArc, bit-oriented | 2,048 | 2 to 17 | `methods.md` 6 | decoded |
| "-lz5-" | LArc, byte-oriented | 4,096 | 3 to 18 | `methods.md` 7 | decoded |
| "-lhd-" | directory or symbolic link | | | `container.md` 6.8 | no data |
| "-lh2-", "-lh3-", "-lhx-", "-pm0-", "-pm1-", "-pm2-", any other | | | | `container.md` 3.3 | entry error "unsupported method"; the rest of the archive is read normally |

What every decoder has in common (`methods.md` section 2):

- It is given the method, the P packed bytes of one entry and the declared
  unpacked length U, and returns exactly U bytes or an error.
- An entry with U = 0 is empty and is never decoded.
- Decoding stops at U bytes. A final match is clipped. Packed bytes left
  unread are not an error. Running out of packed bytes is.
- The container layer verifies the CRC-16 of the unpacked bytes after
  decoding. No method has its own checksum.
- Nothing is delivered as an entry's contents unless every step succeeded.

## 4. Glossary

Terms used with the same meaning in all documents.

| Term | Meaning |
|---|---|
| archive | One LHA archive as a byte sequence starting at the first byte of its first entry header. A self-extractor program or other junk in front of it is removed first (`container.md` section 9). |
| entry | One file, directory or symbolic link: an entry header followed by its data. |
| entry header, header | The variable-length structure that describes one entry. Its layout depends on the level. |
| level | The byte at header offset 20 (0, 1, 2 or 3) that selects the header layout. |
| base header | Levels 0 and 1: the part of the header covered by the size byte and the 8-bit checksum. Levels 2 and 3: the fixed part of 26 or 32 bytes. |
| extended header | A typed, length-prefixed block appended to the header at levels 1 to 3. Extended headers form a chain; each ends with the size of the next. |
| extended area | Extra bytes inside the base header of levels 0 and 1, between the fixed fields and its end. Not the same thing as extended headers. |
| skip size | The level-1 field at header offset 7: packed length plus the total size of the extended headers. |
| method identifier | The five ASCII bytes at header offsets 2 to 6, such as "-lh5-". |
| container layer | The part of a reader that parses headers, lists entries, locates data, calls decoders and verifies the CRC-16. Specified in `container.md`. |
| decoder | The part of a reader that turns the packed bytes of one entry into its unpacked bytes for one method. Specified in `methods.md`. |
| packed bytes, packed stream | An entry's data as stored. "Packed stream" when viewed as a sequence of bits. |
| packed length, **P** | The number of packed bytes of an entry. Untrusted. |
| unpacked bytes, output | The original contents of an entry; what a decoder produces. |
| unpacked length, original size, **U** | The length the header declares for the unpacked entry. Untrusted: a claim to be checked, never an amount of memory to reserve. |
| **L** | The number of archive bytes actually available. |
| header checksum | The 8-bit sum protecting the base header at levels 0 and 1 (`container.md` section 4.1). |
| header CRC | The CRC-16 of the whole entry header, stored in extended header type `00` (`container.md` section 6.3). |
| data CRC, CRC-16 | The checksum algorithm of `container.md` section 3.2 (polynomial `8005`, reflected, initial value 0, no final XOR; "123456789" gives `BB3D`); also the header field holding it for the unpacked bytes. |
| end marker | A single `00` byte where the next header would start. |
| OS identifier | The header byte naming the system that wrote the entry (`container.md` section 3.5). |
| window | The most recent unpacked bytes, which matches copy from. |
| initial contents | What the window holds before any output exists; defined per method. |
| literal, match, distance | `methods.md` section 1.2. |
| table T, table C, table P | The three prefix codes of a static Huffman block: the code for code lengths, the main code, and the distance code (`methods.md` section 4). |
| single-symbol form | A table sent as "count 0, one symbol": its only code has length zero and consumes no bits. |
| black box | An existing decoder used only by feeding it inputs and observing outputs: the reference implementation, `unar`, `7z`. |
| finding | A recorded irregularity that does not change the result of reading. |

## 5. Decisions

A DECISION is a point where the sources disagree, or where behaviour on
malformed or unusual input had to be chosen. Each is marked where it
applies. Decisions 1 to 22 are in `container.md`, 23 to 33 in `methods.md`.
"Ref." is the reference implementation.

### 5.1 Container

| # | Section | Decision | Reason |
|--:|---|---|---|
| 1 | 9.1, 9.2 | An archive is recognised by a fully valid header at offset 0. Failing that, the opener searches the first 262,144 bytes for a header that passes a strict test (valid header, data inside the input, a plausible successor). | None of the black boxes searches; two published tools do, with a weak signature test plus special cases for two self-extractor stubs. A strict test needs no stub-specific knowledge and cannot be fooled by text that merely looks like a method identifier. |
| 2 | 3.3 | Bytes 2 and 6 of a header must be "-". The three bytes between are unrestricted. | Every known identifier has this shape. Ref. accepts any five bytes; requiring the hyphens costs nothing and strengthens every header test. |
| 3 | 4.1, 5.1 | A level-0/1 header whose 8-bit checksum does not match is not a header. | Ref., `7z` and published tools reject; `unar` ignores the checksum. The checksum is the only protection these headers have. |
| 4 | 4.2 | A level-0 header that ends directly after the name (no CRC field) is valid; its data is delivered unchecked. | Published as a genuine old variant; `unar` reads it. Ref. and `7z` reject it, which loses real files. |
| 5 | 6.3 | If a header CRC is present it must match, at levels 1, 2 and 3; if absent, the header is accepted. Two header CRC fields in one header are invalid. | Ref. and one published tool do exactly this. `unar` and `7z` never check; another published tool only warns. A wrong header CRC means the sizes and names cannot be trusted. |
| 6 | 6.1 | Level 2: the walk of the extended headers must end exactly at the total size, or one byte before it (padding), or two bytes after it (writers that do not count the size field). Otherwise invalid. | Ref. accepts exactly these three, independent of the OS identifier. `unar` and `7z` each misread one of the forms. The two-short form is published as OS-9/68K behaviour. |
| 7 | 6.2 | Level 3: the word size must be 4; the walk must end exactly at the total size. Levels above 3 are invalid. | Agreed by Ref. and `unar`. No other word size has ever been defined. |
| 8 | 5.1 | The extended area of a level-1 base header is skipped. | Ref. and one published tool skip it; `unar` and `7z` cannot read such headers. Nothing useful is defined there. |
| 9 | 6.6 | The 64-bit size header (type `42`) is honoured at levels 2 and 3 only. | Ref. behaviour. At level 1 the published tools disagree with each other on how it interacts with the skip size, and no archive is known. |
| 10 | 6.7 | When an extended header type occurs twice, the last occurrence is used. | Three black boxes, three behaviours. "Last wins" is what one published tool and `unar` do and is the simplest rule. |
| 11 | 6.7 | The base name field and the directory header are split at `FF`, "/" and "\\". A name header is one component; "/" in it becomes "_", "\\" stays. | Level-0 writers use "\\" or "/" by platform; the directory header officially uses `FF` but writers exist that use the others. Treating all three as separators there leaves no way to smuggle a separator into a component. A name header is by definition a single component; replacing "/" keeps it one. "\\" is kept there because it is frequent as the second byte of Shift-JIS characters. |
| 12 | 6.7 | Empty components, "." and ".." are removed; paths are always relative. | Ref. behaviour. `7z` keeps them; `unar` fails on them. A read-only traversal must never produce a path that escapes. |
| 13 | 6.7 | A `00` byte ends a name; what follows in a name piece is a comment. For every OS identifier. | Amiga tools store file notes this way. Ref. applies it to Amiga only, `unar` to all; a `00` is never part of a legitimate name on any system. |
| 14 | 6.7 | Name bytes become text as: ASCII; else UTF-8 if well-formed; else ISO 8859-1. | The header does not name its encoding. The archives this repository targets are mostly Amiga (ISO 8859-1) and Unix. Shift-JIS and DOS code pages are not decoded; their bytes survive unambiguously as ISO 8859-1 characters. Ref. turns every non-ASCII byte into a percent escape. |
| 15 | 6.8 | Entry kinds: "-lhd-" is a directory, or a symbolic link when the Unix mode says so and the name contains "\|"; a "-lh0-" entry with U = 0 whose path ends with a separator is a directory; everything else is a file. | Directories as "-lhd-": all agree. The mislabelled "-lh0-" directory is listed as an empty file by the black boxes and as a directory by two published tools, which know the Amiga writers that produce it; the rule here drops their "Amiga only" condition because a file cannot have a name ending in a separator on any system. |
| 16 | 6.8 | A file with an empty path is delivered as "unnamed"; a directory with an empty path is dropped. | Such files exist (published). The black boxes drop them, fail, or invent a name from the archive's own file name, which a library does not know. |
| 17 | 7.3 | Last-modified time: extended header `54`, else `41`, else the level-0 Unix area, else the fixed field. | Ref. and `unar` both prefer the extended headers over the fixed field, at level 1 and at level 2. The fixed field of levels 0 and 1 has no zone and two-second resolution. |
| 18 | 7.1 | An MS-DOS timestamp that is zero or not a real date and time means "no timestamp". | Ref. behaviour. `unar` normalises impossible values into some other date. |
| 19 | 3.3 | "-pm0-", "-pm1-", "-pm2-" are reported as unsupported. | PMarc is outside the agreed scope. Note: Ref. and `unar` treat "-pm0-" as stored, so stored PMarc entries that the replaced reader delivered are now reported as unsupported; see section 8. |
| 20 | 3.3 | A level-1 entry with OS identifier `20` and identifier "-lh7-" is reported as unsupported. | Published: one DOS tool wrote an incompatible stream under this identifier. Decoding it as "-lh7-" can only end in a checksum failure; "unsupported" is the accurate report. |
| 21 | 3.2 | For a file with U = 0 the stored CRC-16 is compared with `0000`. | Uniform treatment; Ref. does the same. Directories and links are exempt. |
| 22 | 8 | After at least one valid entry, anything at a header position that is not a valid header ends the archive with a finding, keeping the entries already read. A header whose data runs past the end of the input is listed and reported as truncated. | Files padded by transfer protocols are common for formats of this era. Ref. and `unar` fail on them, Ref. to the point of delivering nothing at all. |

### 5.2 Methods

| # | Section | Decision | Reason |
|--:|---|---|---|
| 23 | 4.9 | Static Huffman methods: the window initially holds spaces. If the CRC-16 fails and some match read from before the start of the output, the entry is decoded once more with a zero-filled initial window and accepted if the CRC-16 then matches. | Spaces: Ref. and two published tools, one of them the Unix implementation of the format's own lineage, whose encoder can produce such matches. Zeros: `unar`. Error: `7z`. The retry exists because that Unix implementation documents that some of its own versions wrote archives needing zeros. The CRC-16 arbitrates, so the retry cannot turn a correct result into a wrong one. |
| 24 | 4.1 | "-lh4-" is decoded with the limits of "-lh5-" (window 8,192, 14 distance codes) and "-lh6-" with those of "-lh7-" (65,536, 17). | Ref. behaviour, and that of a published tool. A conforming stream decodes identically under the wider limits; a stream from an encoder that used the larger window under the smaller name still decodes. A published tool instead wraps distances at the nominal size, which only produces garbage. |
| 25 | 4.2 | A block size of 0 is corrupt. | `unar` and `7z` reject; Ref. reads the tables and expects another block; a published tool decodes 65,536 symbols. No encoder writes it, and rejecting removes a class of streams that make no progress. |
| 26 | 4.4 to 4.6 | Table counts above 19 (T), 510 (C) or the method's distance-code count (P) are corrupt. In T the 2-bit number is read only when the count is at least 3, and a skip beyond the count is accepted. In C a zero run beyond the count is corrupt. | Count limits: the symbols beyond have no meaning; `7z` and (for C and P) Ref. reject. The 2-bit number: published encoders, `unar` and `7z` agree; Ref. is wrong. Skip beyond the count: published encoders can write exactly this for a count of 3. Run beyond the count: `unar` and `7z` reject, Ref. clips; no encoder writes it. |
| 27 | 4.4 to 4.6 | In the single-symbol form the symbol must exist: at most 18 (T), 509 (C), 13 or 16 (P). | Ref. accepts out-of-range values and then produces match lengths of 257 and 258 or offsets beyond the window. Those are not part of the format. |
| 28 | 4.3 | A transmitted code must be complete and not over-subscribed. | Ref. and `7z` reject incomplete codes; `unar` decodes them. All three reject over-subscribed codes. Encoders derived from the original always write complete codes or the single-symbol form. Completeness also guarantees that decoding can never reach an undefined state. |
| 29 | 4.3, 4.4 | Code lengths above 16 are corrupt. | The format's encoders limit codes to 16 bits. Ref. allows up to 255 and then fails on the table size. |
| 30 | 5.6, 6.1, 7.2 | Initial windows: "-lh1-" and "-lzs-" all spaces; "-lz5-" the pattern of `methods.md` section 7.2 with `00` in the last 18 positions. | "-lh1-": Ref. and one published tool; `unar` differs only in 60 positions no encoder can reference. "-lz5-": Ref., `unar` and one published tool agree on `00`; another published tool has spaces there, again in positions an encoder cannot reference before they are overwritten. "-lzs-": all sources agree. |
| 31 | 2.2 | Consuming a bit beyond the packed bytes is an error. | Ref. and `unar` report it. A published tool supplies zero bits forever, which turns truncation into garbage output. |
| 32 | 2.4 | Decoding ends at U bytes; a final match is clipped; unread packed data is ignored. | Ref. and `unar` agree. `7z` rejects unread bytes after a static Huffman stream. |
| 33 | 3 | Stored entries: P above U delivers the first U bytes; P below U is corrupt. | Ref. behaviour. `7z` delivers P bytes whatever U says; `unar` delivers the smaller of the two. The CRC-16 over exactly U bytes arbitrates. |

## 6. Scope

In scope: header levels 0, 1, 2 and 3 with extended headers, 64-bit sizes,
directories, the name and path conventions of MS-DOS, Amiga and Unix
writers; the methods of section 3 marked "decoded".

Out of scope, and what a reader does instead:

| Item | Handling |
|---|---|
| methods "-lh2-", "-lh3-", "-lhx-", PMarc "-pm?-" and unknown identifiers | entry error "unsupported method"; other entries unaffected |
| the incompatible "-lh7-" variant of one DOS tool | detected and reported as unsupported (`container.md` 3.3) |
| archives whose first entry is a PMarc entry | not recognised as LHA |
| multi-volume archives | each volume is read as it stands; the entry cut by the volume end is truncated |
| Shift-JIS and DOS code page names | bytes pass through as ISO 8859-1 characters (`container.md` 6.7) |
| Unix ownership, permissions (other than recognising symbolic links), OS-9 and OS/2 attributes | ignored |
| following or creating symbolic links | never; the target is reported as text |
| MacBinary and similar wrappers | removed by other layers before the LHA reader is called |
| writing archives | not specified |

## 7. Sources consulted

Implementation source (read by the specification side only; nothing was
copied):

1. The reference implementation: the third-party LHA library for Rust,
   version 0.6.2 (all source files, including its change log and built-in
   documentation).

Public documentation and published source of other tools, used for facts
and to establish where implementations disagree:

2. "header.doc", the header description distributed with LHa for UNIX
   (original by Masaru Oki, 1992, with later additions; English
   translation by Koji Arai), in the public repository of LHa for UNIX
   (`jca02266/lha`, file `header.doc.md`).
3. LHa for UNIX, same repository: the published source files for header
   reading, sliding-window decoding, static and dynamic Huffman decoding,
   LArc decoding, and its table of constants. Consulted for: header layouts
   and tolerances, the level-0 form without CRC, the list of extended
   header types, the initial window and its documented "broken archive"
   history, the table-reading order of the static Huffman methods, the
   LArc position conventions.
4. Lhasa, a free LHA reader (`fragglet/lhasa`): the published source files
   for header reading, extended headers, the input stream (self-extractor
   skipping), the decoders for the static Huffman, adaptive Huffman and
   LArc methods, and its public header of OS identifiers. Consulted for:
   the same points as item 3, the OS-9/68K level-2 quirk, the Amiga
   directory quirk, the incompatible "-lh7-" variant, the "-lz5-" initial
   window, the limit of the self-extractor search.
5. General knowledge of the classic public-domain description of LZSS with
   adaptive Huffman coding on which "-lh1-" is based, and of the CRC
   catalogued as "CRC-16/ARC".

Black boxes (behaviour observed, source not read):

6. `unar` and `lsar`, version 1.10.7.
7. `7z`, p7zip version 17.05.

Data:

8. One genuine archive (level 1, Unix, "-lh5-", about 550 KB unpacked),
   used read-only to confirm the level-1 layout and the static Huffman
   method.
9. About 150 archives built byte by byte from these documents by scratch
   tools kept outside the repository.

## 8. Points that rest on limited evidence

To be revisited when more genuine archives are available (the local oracle
of real archives did not exist yet when this was written).

1. **Real data coverage is one archive.** Levels 0, 2 and 3, the methods
   "-lh1-", "-lh4-", "-lh6-", "-lh7-", "-lzs-" and "-lz5-", directories,
   and names from Amiga and MS-DOS writers are confirmed on crafted inputs
   and published descriptions only.
2. **"-lzs-"** is decoded by only one black box (the reference
   implementation). Two published tools describe the same format.
3. **The level-0 header without CRC** (DECISION 4), **the zero-window
   retry** (DECISION 23), **symbolic links** and **the incompatible
   "-lh7-" variant** (DECISION 20) are specified from published statements;
   no example was available.
4. **Incomplete codes** (DECISION 28). `unar` and one published tool decode
   them, which suggests somebody once met a writer that produces them. The
   reference implementation, which this reader replaces, rejects them, so
   the decision cannot make anything worse than it is today. If a genuine
   archive with an incomplete code turns up, relax the rule to "the sum
   must not exceed 1, and reaching an unassigned code while decoding is
   corrupt".
5. **"-pm0-"** (DECISION 19) is trivially "stored" and is delivered by the
   reader being replaced. It is reported as unsupported here because PMarc
   was placed out of scope. Treating it like "-lh0-" would be a one-line
   change to `container.md` section 3.3 and `methods.md` section 3.
6. **Name encodings** (DECISION 14) and **splitting at "\\"** (DECISION 11)
   are wrong for Shift-JIS names in level-0 and level-1 headers. No
   dependable way to recognise the encoding from the header is known.
7. **The prefix search** (DECISION 1) was designed here; it has been tested
   only against the absence of false positives in the crafted set, not
   against real self-extractors.
