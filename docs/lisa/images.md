# Tag-preserving disk image wrappers for Apple Lisa disks

Format specification for clean-room implementation. This document describes
the three families of image files that carry the *tag bytes* of a Lisa disk
alongside its block data, and how each is turned into one in-memory form for
the file-system layer:

- **DiskCopy 4.2** images (floppies, Twiggy disks, and the hard-disk images
  written by the LisaEm emulator);
- **DART** images (compressed floppy images);
- **raw 532-byte-block dumps** of ProFile and Widget hard disks, with or
  without the one-block header written by the Basic Lisa Utility (BLU).

What the tag fields *mean* to the Lisa file system, and how a volume is
recognised and read, is the business of `lisafs.md`. `README.md` in this
directory is the index of the document set.

The document contains no program code and no pseudo-code. Procedures are
written as numbered steps in prose; every constant table an implementer
needs is written out in full.

## 1. How to read this document

### 1.1 Provenance

- The rules were derived from published format descriptions, from studying
  the behaviour of existing open implementations (the *reference
  implementations*, section 13), and from measurements on real image files
  (section 11).
- A throwaway reader that follows the rules of this document literally was
  written by the specification's author, kept outside the repository, and
  run over the real files of section 11. It is not part of the
  specification and must not be looked for.
- The implementation in this repository is written **from this document
  only**. Implementers do not read the reference implementations.
- Where a reference implementation is wrong or unsafe, section 10 says so
  and the body of the document specifies the correct behaviour instead.
  Nothing in section 10 is to be imitated.

### 1.2 Evidence labels

- **CONFIRMED (real data)**: checked on the real files listed in section
  11, by a stored checksum that matched or by an internal consistency
  measure that is stated with the claim.
- **CONFIRMED (crafted)**: checked by building input by hand from the rules
  of this document and decoding it with an independent existing tool, or
  the reverse.
- **PUBLISHED**: stated by a published description (section 13) and not
  contradicted by any file seen, but no file seen exercises it.
- **REFERENCE-ONLY**: what a reference implementation does, with no
  published statement and no real file to check it against.
- **INFERRED**: deduced from data, with the deduction stated.
- **DECISION**: a behaviour chosen by this specification. All decisions are
  listed in section 12.

### 1.3 Notation

- Numbers are decimal unless written with a `0x` prefix. Byte sequences are
  written in hexadecimal, in file order: `01 00`.
- All multi-byte integers in all three formats are **big-endian**. A
  *word* is 16 bits.
- A **block** is 512 bytes of disk data. Block numbers start at 0.
- A **tag** is the small record stored on the medium next to each block: 12
  bytes on floppy media, 20 bytes on ProFile and Widget hard disks.
- **Logical order** is the order in which the Lisa operating system numbers
  blocks. **Device order** is the order in which a ProFile numbers them
  (section 6.3). `lisafs.md` uses the words "device block" for what this
  document calls a block in logical order: the image layer has already
  undone the interleave by the time the file-system layer counts blocks, so
  the two documents number blocks identically.
- Offsets are counted from the first byte of the image file (after any
  outer wrapper has been removed, section 7).

## 2. The contract with the file-system layer

### 2.1 What the image layer delivers

For an input it recognises, the image layer delivers:

1. **the block data**: N × 512 bytes, block 0 first, in logical order;
2. **the tag data**: either N × 20 bytes in the *canonical tag layout* of
   section 2.3, tag of block 0 first, or nothing at all when the image
   carries no tags (**DECISION D1**). No other length is ever delivered.

The image layer does not decide whether the blocks hold a Lisa file system.
It delivers Macintosh disks (MFS and HFS volumes written by MacWorks, and
ordinary Macintosh floppies) in exactly the same way; the file-system layer
rejects them and the caller then offers the original input to the
Macintosh reader. The image layer therefore must not alter its input.

An input that is recognised by none of sections 4, 5 and 6 is "not a tagged
image". This is the normal result for almost every input, and it must be
reached cheaply (section 9.1).

### 2.2 The two tag layouts on the medium

**The 20-byte hard-disk tag** (ProFile, Widget). CONFIRMED (real data) for
bytes 4 to 7 and 12 to 19 on two ProFile dumps (section 11); the remaining
fields are PUBLISHED (the Aaru decoder agrees with the Lisa documentation
it cites).

| Offset | Size | Field |
|--:|--:|---|
| 0 | 2 | version |
| 2 | 1 | kind (top two bits) and reserved bits |
| 3 | 1 | volume |
| 4 | 2 | file identifier |
| 6 | 2 | bit 15: "checksum valid" flag; bits 14 to 0: bytes of the block in use |
| 8 | 3 | absolute page |
| 11 | 1 | checksum |
| 12 | 2 | relative page: position of this block within its file |
| 14 | 3 | forward link: block number of the next block of the file, `FF FF FF` for none |
| 17 | 3 | backward link: block number of the previous block, `FF FF FF` for none |

Example, the tag of block 0 of a ProFile holding the Lisa Office System:
`00 00 00 00 AA AA 82 00 FF FF FF 48 00 00 FF FF FF FF FF FF`: file
identifier `AAAA`, checksum-valid flag set, 512 bytes in use, relative page
0, no links.

**The 12-byte floppy tag** (3.5-inch "Sony" disks and Twiggy disks).

| Offset | Size | Field |
|--:|--:|---|
| 0 | 2 | version |
| 2 | 1 | kind (top two bits) and reserved bits |
| 3 | 1 | volume |
| 4 | 2 | file identifier |
| 6 | 2 | relative page |
| 8 | 2 | bits 15 to 11: upper five bits of the bytes-in-use count; bits 10 to 0: forward link, `7FF` for none |
| 10 | 2 | bits 15 to 11: lower five bits of the bytes-in-use count; bits 10 to 0: backward link, `7FF` for none |

The layout of bytes 0 to 7 and the 11-bit links are PUBLISHED and CONFIRMED
(real data). The meaning of the upper five bits of the two link words is
INFERRED: on 17 Lisa floppies, combining them as (upper five bits of word
at 8) × 32 + (upper five bits of word at 10) gives exactly 512 for all
8,981 blocks that have a successor in their file, and a value from 0 to 512
for every one of the 1,110 last blocks of a file. No reference
implementation decodes this count; they mask the five bits away.

Examples from a Twiggy disk:

- `00 00 00 00 00 04 00 0A 80 14 00 12`: file 4, relative page 10, forward
  link `014` = 20, backward link `012` = 18, bytes in use 16 × 32 + 0 =
  512.
- `00 00 00 00 00 04 00 0F 0F FF 50 17`: file 4, relative page 15, no
  forward link, backward link `017` = 23, bytes in use 1 × 32 + 10 = 42.

The link values are block numbers as the file system counts them, which is
not always the block number within the image (on the Twiggy disks measured,
link value = image block number − 41). Interpreting them belongs to
`lisafs.md`.

### 2.3 The canonical tag layout

**DECISION D2.** The file-system layer sees one layout only: the 20-byte
hard-disk layout of section 2.2.

- A 20-byte tag is delivered unchanged.
- A 12-byte tag is widened as follows.

| Canonical bytes | Value |
|---|---|
| 0 to 5 | bytes 0 to 5 of the 12-byte tag, unchanged |
| 6 to 7 | the bytes-in-use count as a 16-bit number (0 to 1,023); bit 15 is always clear |
| 8 to 10 | `FF FF FF` (absolute page unknown) |
| 11 | `00` |
| 12 to 13 | bytes 6 to 7 of the 12-byte tag (relative page) |
| 14 to 16 | the 11-bit forward link as a 24-bit number, except that `7FF` becomes `FF FF FF` |
| 17 to 19 | the 11-bit backward link, converted the same way |

Worked examples:

- `00 00 00 24 AA AA 00 00 87 FF 07 FF` becomes
  `00 00 00 24 AA AA 02 00 FF FF FF 00 00 00 FF FF FF FF FF FF`.
- `00 00 00 00 00 04 00 0F 0F FF 50 17` becomes
  `00 00 00 00 00 04 00 2A FF FF FF 00 00 0F FF FF FF 00 00 17`.

Consequences the file-system layer may rely on: the file identifier is
always at canonical bytes 4 to 5 and the relative page at 12 to 13,
whatever the medium; a link of `FF FF FF` always means "none"; the
checksum-valid flag, absolute page and checksum are meaningful only for
images that came from a hard disk, and the file-system layer must not
require them.

Coordination with `lisafs.md`: its section 3 describes both layouts and its
decision D2 reduces either to four values (file identifier, relative page,
bytes in use, forward link). Because of D2 here, the file-system layer only
ever has to decode its section 3.3 (the 20-byte layout); the widening above
preserves exactly those four values, and maps "no link" (`7FF`) to the
20-byte "no link" (`FF FF FF`). Three things differ from a genuine
hard-disk tag and must not be relied on: the checksum-valid flag is clear
and the checksum byte is `00`; the absolute page is `FF FF FF`; and an
all-zero floppy tag (an unused block) widens to links of 0, not "none".
Link values stay *page numbers* as `lisafs.md` defines them; the image
layer does not rebase them.

Tags of any other size are not delivered. An image with 24 bytes of tag per
block (the Priam DataTower convention, REFERENCE-ONLY) is "not a tagged
image" (**DECISION D3**): no specimen was available and the Lisa file
system is not known to live on such a device.

## 3. The Apple checksum

DiskCopy 4.2 and DART use the same 32-bit checksum. PUBLISHED; CONFIRMED
(real data) on every image of section 11.

1. Start with a 32-bit value of 0.
2. Take the input two bytes at a time, as big-endian words, first word
   first. For each word: add it to the value (keeping the low 32 bits of
   the sum), then rotate the 32-bit value right by one bit (the lowest bit
   moves to the highest position).
3. The result after the last word is the checksum.

The input always has an even number of bytes in a valid image. An odd
length is a property of a corrupt image; the last odd byte is ignored
(**DECISION D4**).

Worked examples:

| Input | After each word (sum, then rotated) | Checksum |
|---|---|---|
| `00 01` | `00000001` → `80000000` | `80000000` |
| `00 01 00 02` | `80000000`; `80000002` → `40000001` | `40000001` |
| `12 34 56 78` | `00001234` → `0000091A`; `00005F92` → `00002FC9` | `00002FC9` |
| `FF FF FF FF` | `0000FFFF` → `80007FFF`; `80017FFE` → `4000BFFF` | `4000BFFF` |
| 512 bytes of `FF` | | `FFF9FFFD` |
| any number of `00` bytes | | `00000000` |

Which bytes are fed to the checksum differs between the two formats; see
sections 4.4 and 5.7.

## 4. DiskCopy 4.2

### 4.1 Layout

A DiskCopy 4.2 image is an 84-byte header, followed by the block data,
followed by the tag data. Nothing is compressed.

| Offset | Size | Field |
|--:|--:|---|
| `0x00` | 1 | length of the disk name, 0 to 63 |
| `0x01` | 63 | disk name (Mac OS Roman); bytes beyond the name length are undefined |
| `0x40` | 4 | data size D, in bytes |
| `0x44` | 4 | tag size T, in bytes |
| `0x48` | 4 | data checksum |
| `0x4C` | 4 | tag checksum |
| `0x50` | 1 | disk encoding byte (section 4.3) |
| `0x51` | 1 | format byte (section 4.3) |
| `0x52` | 2 | magic: `01 00` |
| `0x54` | D | block data, block 0 first |
| `0x54` + D | T | tag data, tag of block 0 first |

PUBLISHED; CONFIRMED (real data) on 52 images.

Facts about the name field, all CONFIRMED (real data):

- Disks that are not Macintosh volumes carry the name
  `-not a Macintosh disk-` (length 22). Lisa disks imaged with DiskCopy 4.2
  have this name, and so do MFS disks written by MacWorks on a Lisa, so the
  name says nothing about the contents.
- The name can be empty (length 0, all 64 bytes zero): images written by a
  flux-imaging tool chain.
- The bytes after the name are not zeroed by DiskCopy 4.2; they hold
  leftover memory contents. BLU writes the text ` BLUV0.10 Twiggy Image`
  followed by spaces there.
- The name itself can begin with a second length byte (seen:
  length 8, then `07` and `PREBOOT`).
- LisaEm names its hard-disk images `-lisaem.sunder.net hd-`
  (REFERENCE-ONLY).

The name is never used to accept, reject or interpret an image
(**DECISION D5**), with one exception in the acceptance rule of section
4.6: the length byte must not exceed 63.

### 4.2 Block count and tag size per block

- The number of blocks is N = D ÷ 512. D must be a non-zero multiple of
  512.
- T is either 0 (no tags) or N × 12 or N × 20. The quotient T ÷ N is the
  tag size per block. Floppy images written by DiskCopy 4.2 have 12
  (CONFIRMED); hard-disk images written by LisaEm and its conversion tools
  have 20 (REFERENCE-ONLY, section 4.5).
- The tag of block *b* is the T ÷ N bytes at offset `0x54` + D + *b* ×
  (T ÷ N).

### 4.3 The encoding byte and the format byte

Values of the disk encoding byte at `0x50`:

| Value | Meaning | Evidence |
|---|---|---|
| `00` | 400K GCR disk (Macintosh 400K, Lisa 3.5-inch 400K) | CONFIRMED, 40 images |
| `01` | 800K GCR disk | CONFIRMED, 1 image |
| `02` | 720K MFM disk | PUBLISHED |
| `03` | 1440K MFM disk | PUBLISHED |
| `04` | 1680K (DMF) disk | REFERENCE-ONLY |
| `54` | Lisa Twiggy disk, written by BLU | CONFIRMED, 11 images |
| `5D` | "not a standard disk": hard-disk images written by LisaEm | REFERENCE-ONLY |

Values of the format byte at `0x51`:

| Value | Meaning | Evidence |
|---|---|---|
| `02` | what DiskCopy 4.2 really writes for 400K disks, Macintosh and Lisa alike; also 720K and 1440K | CONFIRMED, 40 images (400K) |
| `12` | 400K according to the published description; written by LisaEm for 400K images | PUBLISHED |
| `22` | 800K Macintosh disk; also the published value for "anything larger than 400K" | CONFIRMED, 1 image |
| `24` | 800K Apple II (ProDOS) disk | PUBLISHED |
| `01` | Twiggy disk, written by BLU | CONFIRMED, 11 images |
| `93` | hard-disk images written by LisaEm | REFERENCE-ONLY |
| `96` | "unformatted" | REFERENCE-ONLY |
| `00` | written by some later Mac OS tools | REFERENCE-ONLY |

Neither byte is used to accept or reject an image, and neither describes
the contents reliably (**DECISION D6**). The only use this specification
makes of them is the Twiggy rule of section 4.5.

### 4.4 The two checksums

- The **data checksum** is the checksum of section 3 over all D data
  bytes.
- The **tag checksum** is the checksum of section 3 over the tag data
  **without its first 12 bytes**: bytes 12 to T − 1 of the tag area. The
  tag of block 0 does not take part. When T is 0 the stored value is 0.

CONFIRMED (real data): on all 52 images both stored values equal the values
computed this way. The checksum over all T bytes equals the stored value on
none of the images whose first tag is not zero.

Worked example. A 400K Lisa disk has D = 409,600 and T = 9,600; its header
bytes `0x40` to `0x53` are

```
00 06 40 00  00 00 25 80  C8 B5 97 35  02 01 48 6C  00 02 01 00
```

The checksum of the 409,600 data bytes is `C8B59735`; the checksum of tag
bytes 12 to 9,599 is `0201486C`; the checksum of all 9,600 tag bytes would
be `020D732C`, which is not what is stored.

For an image with 20 bytes of tag per block, the two reference
implementations disagree on whether the first 12 bytes, the first 20 bytes
or nothing is skipped (section 10), and no specimen was available.

**DECISION D7.** A reader never rejects an image because a checksum does
not match, and need not compute the checksums at all. Reasons: several
writers leave zero or stale values (LisaEm writes zero checksums when it
creates an image and updates them only when the image is closed cleanly);
the rule for 20-byte tags is unsettled; and the file-system layer validates
what it reads anyway. A reader that does compute them, to report damage,
treats the data checksum as authoritative and accepts a tag checksum that
matches under any of the three skipping rules.

### 4.5 How each kind of disk is represented

| Disk | D | T | File length | Encoding, format | Block order |
|---|--:|--:|--:|---|---|
| Macintosh 400K | 409,600 | 9,600 or 0 | 419,284 or 409,684 | `00`, `02` (or `12`) | logical |
| Macintosh or Apple II 800K | 819,200 | 19,200 or 0 | 838,484 or 819,284 | `01`, `22` (or `24`) | logical |
| 720K MFM | 737,280 | 0 | 737,364 | `02`, `02` | logical |
| 1440K MFM | 1,474,560 | 0 | 1,474,644 | `03`, `02` | logical |
| Lisa 3.5-inch 400K | 409,600 | 9,600 | 419,284 | `00`, `02` | logical |
| Lisa Twiggy (871K) | 871,424 | 20,424 | 891,932, or 892,928 from BLU | `54`, `01` | see below |
| LisaEm ProFile 5 MB | 4,980,736 | 194,560 | 5,175,380 | `5D`, `93` | logical |
| LisaEm ProFile 10 MB | 9,961,472 | 389,120 | 10,350,676 | `5D`, `93` | logical |

Notes:

- A Lisa 3.5-inch disk and a Macintosh 400K disk are indistinguishable by
  their headers. CONFIRMED (real data).
- A Macintosh volume in a DiskCopy 4.2 image normally **has tags**: an MFS
  system disk written by MacWorks and an 800K HFS installer disk both carry
  N × 12 tag bytes after the data, most of them non-zero. A reader of
  Macintosh volumes must take the volume to be the D data bytes, not
  everything after the header.
- LisaEm creates hard-disk images of 9,728, 19,456, 32,768, 40,960, 65,536,
  81,920 or 131,072 blocks, always with 20 tag bytes per block; its
  conversion tools create images of whatever block count their input has.
  LisaEm translates the block numbers the Lisa asks for from device order
  to logical order before it touches the image, so the image is in
  **logical order**. REFERENCE-ONLY; no LisaEm-written image was available.
  A reader does not rely on this: it applies the order test of section 6.5
  to every image with 20-byte tags (**DECISION D8**).

**Twiggy disks.** A Twiggy disk has two sides of 851 blocks. Each side has
46 tracks; the number of blocks per track is:

| Tracks | Blocks per track | First block of the first of these tracks, within its side |
|---|--:|--:|
| 0 to 3 | 22 | 0 |
| 4 to 10 | 21 | 88 |
| 11 to 16 | 20 | 235 |
| 17 to 22 | 19 | 355 |
| 23 to 28 | 18 | 469 |
| 29 to 34 | 17 | 577 |
| 35 to 41 | 16 | 679 |
| 42 to 45 | 15 | 791 |

(4 × 22 + 7 × 21 + 6 × 20 + 6 × 19 + 6 × 18 + 6 × 17 + 7 × 16 + 4 × 15 =
851.)

In an image with encoding byte `54`, the first 851 blocks of the data area
(and the first 851 tags) are logical blocks 0 to 850, in order. The second
851 blocks are the second side stored track by track, track 0 first, each
track's blocks in ascending order; but the logical order of those tracks is
different. Logical blocks 851 to 1,701 are obtained by taking the tracks of
the second half in this order, each track's blocks in ascending order:

1. tracks 45, 44, 43, …, 24 (descending);
2. then tracks 22, 21, 20, …, 0 (descending);
3. then track 23.

So logical block 851 is the first block of track 45 of the second half
(image block 851 + 836 = 1,687), logical block 866 is image block 1,672,
logical blocks 1,215 to 1,233 are track 22 (image blocks 1,301 to 1,319),
logical blocks 1,662 to 1,683 are track 0 (image blocks 851 to 872) and
logical blocks 1,684 to 1,701 are track 23 (image blocks 1,320 to 1,337).
Tags move with their blocks.

Evidence: CONFIRMED (real data). On the three Twiggy images that have files
on the second side, 1,154 pairs of logically adjacent blocks of the same
file with consecutive relative pages have forward and backward links that
agree with this order, and none disagree. Storing the tracks in plain
ascending order contradicts 1,060 of them, plain descending order 383. The
position of track 23 looks arbitrary; it is what the data shows and what
the Aaru reader does.

**DECISION D9.** The reorder is applied when, and only when, the encoding
byte is `54`, D is 871,424 and T is 20,424 or 0. An image with encoding
byte `54` and any other size is delivered in file order.

A second Twiggy convention is REFERENCE-ONLY: for a *Macintosh* Twiggy
disk, Aaru swaps the two halves instead (it recognises the case by an MFS
signature `D2 D7` at offset 1,024 of the second half together with an MFS
block count of 422). Such a disk is not a Lisa file system and reaches the
image layer's caller only as a rejected candidate, so the image layer
ignores this convention (**DECISION D10**).

### 4.6 Known deviations

- **Trailing bytes.** Images written by BLU and transferred by XMODEM are
  padded to a multiple of 1,024 bytes: all ten BLU Twiggy images are
  892,928 bytes long, 996 bytes more than `0x54` + D + T. CONFIRMED (real
  data). Bytes after the tag area are ignored.
- **Short files.** LisaEm's library accepts a file up to 1,024 bytes
  shorter than the header promises. This specification does not
  (section 4.7).
- **Wrong or zero checksums.** Section 4.4.
- **Odd names.** Section 4.1.
- **Little-endian size fields.** Aaru retries with the four 32-bit fields
  read as little-endian when the big-endian sizes do not fit the file,
  with the comment that "some versions" write them so. No specimen;
  not supported (**DECISION D11**).
- **MacBinary.** DiskCopy 4.2 images were routinely distributed in
  MacBinary, file type `dImg`, creator `dCpy`. The image is the data fork.
  The resource fork holds a resource of type `dCpy`, ID 0, a Pascal string
  such as `4.2, tag chksum=$0201486C, data chksum=$94C04ED9`, repeating the
  two header checksums; it adds no information. CONFIRMED (real data), 14
  images. Section 7.
- **Images without tags.** Later tools (Disk Copy 6 and conversions through
  it) drop the tags: T = 0. Such an image is delivered with no tag data.

### 4.7 Detection

An input is a DiskCopy 4.2 image, for the purposes of the image layer,
when all of the following hold (**DECISION D12**):

1. It is at least 84 bytes long.
2. The byte at `0x00` is at most 63.
3. The bytes at `0x52` and `0x53` are `01` and `00`.
4. D is not 0 and is a multiple of 512.
5. T is 0, or N × 12, or N × 20, where N = D ÷ 512.
6. 84 + D + T, computed without overflow, is at most the input length.
7. The input length exceeds 84 + D + T by fewer than 2,048 bytes.

Rules 4 to 7 are what make the test reliable: the two magic bytes alone
match one input in 65,536 by chance, and match every file that happens to
have `01 00` at offset 82. No checksum is computed for detection.

## 5. DART

DART ("Disk Archive/Retrieval Tool") stores a floppy as a sequence of
independently compressed chunks of 40 blocks. PUBLISHED: the layout below
is the read-only description Apple shipped with DART 1.5.3. CONFIRMED (real
data) on 21 images for everything not marked otherwise.

### 5.1 File layout

| Offset | Size | Field |
|--:|--:|---|
| 0 | 1 | compression kind: 0, 1 or 2 (section 5.3) |
| 1 | 1 | disk kind (section 5.2) |
| 2 | 2 | disk size in kilobytes: 400, 720, 800 or 1,440 |
| 4 | 2 × E | block-length table: E signed 16-bit entries |
| 4 + 2 × E | | the stored chunks, one after another, with nothing between them |

E is 40 or 72, depending on the disk kind. The header is therefore 84 or
148 bytes long. (That the short header has the same length as a DiskCopy
4.2 header is a coincidence.)

There is no magic number. Section 5.8 gives the detection rule.

### 5.2 Disk kinds

| Disk kind | Meaning | Disk size | E | Chunks | Tags |
|--:|---|--:|--:|--:|---|
| 1 | Macintosh | 400 or 800 | 40 | 20 or 40 | yes |
| 2 | Lisa | 400 | 40 | 20 | yes |
| 3 | Apple II | 800 | 40 | 40 | yes |
| 16 | Macintosh high density | 1,440 | 72 | 72 | no (all zero) |
| 17 | MS-DOS 720K | 720 | 40 | 36 | no (all zero) |
| 18 | MS-DOS 1440K | 1,440 | 72 | 72 | no (all zero) |

The number of chunks is the disk size divided by 20 (each chunk holds 40
blocks, 20 kilobytes). Kinds 1 with size 800 and 16 are CONFIRMED (real
data); the others are PUBLISHED. No Lisa-kind DART image was available.

The Macintosh file type of a DART image tells the same story and is not
needed: `DMd1` Macintosh 400K, `DMd2` Lisa 400K, `DMd3` Macintosh 800K,
`DMd4` Apple II 800K, `DMd5` MS-DOS 720K, `DMd6` Macintosh 1440K, `DMd7`
MS-DOS 1440K, `DMdf` older versions; creator `DART`.

### 5.3 Chunks and the block-length table

Every chunk, once expanded, is exactly **20,960 bytes**: 20,480 bytes of
block data (40 blocks) followed by 480 bytes of tag data (40 tags of 12
bytes). The tags of a chunk follow *its* data; they are not collected at
the end of the file.

Entry *i* of the block-length table describes chunk *i*:

| Compression kind | Entry value L | Stored form of the chunk |
|---|---|---|
| any | −1 (`FF FF`) | 20,960 bytes, not compressed |
| 0 ("fast") | 1 to 10,480 | 2 × L bytes of run-length data (section 5.4) |
| 1 ("best") | 1 to 20,960 | L bytes of LZH data (section 5.5) |
| 2 (not compressed) | any non-zero value | 20,960 bytes, not compressed |
| any | 0 | no chunk: the end of the image |

Note the unit: for the run-length kind the entry counts **words**, for the
LZH kind **bytes**. PUBLISHED and CONFIRMED (real data): with these units
the stored chunks of all 21 images end exactly at the end of the file.

Observed ranges: run-length entries from 2 to 10,480 with −1 entries mixed
in (39 of the 232 chunks of the five run-length images are stored
uncompressed); LZH entries from 478 to 20,780, never −1.

The first C entries, where C is the number of chunks of section 5.2, must
be non-zero and the remaining E − C entries must be 0. CONFIRMED (real
data) for C = E; PUBLISHED for the 400K and 720K cases ("until the entry is
zero or the file ends").

Compression kind 2 is PUBLISHED as "DART 1.4 and earlier"; no specimen.
**DECISION D13**: with compression kind 2 every non-zero entry stands for
20,960 stored bytes.

Assembling the disk: the block data of the image is the first 20,480 bytes
of each expanded chunk, concatenated in chunk order; the tag data is the
last 480 bytes of each, concatenated in chunk order. Blocks are in logical
order.

### 5.4 The run-length kind

The stored chunk is a sequence of records. Each record starts with a signed
16-bit count *n*:

- *n* > 0: the next *n* words (2 × *n* bytes) are copied to the output
  unchanged;
- *n* < 0: the next single word (2 bytes) is written to the output −*n*
  times;
- *n* = 0: does not occur.

Records follow each other until the 2 × L stored bytes are used up. A
genuine chunk expands to exactly 20,960 bytes and ends exactly at a record
boundary. CONFIRMED (real data): 193 run-length chunks, 18,536 repeat
records and 17,773 literal records, every chunk ending with both conditions
met; the data and tag checksums of all five images match.

Errors (**DECISION D14**), each "corrupt image":

- a count of 0;
- a record that needs more stored bytes than remain in the chunk;
- a record that would take the output past 20,960 bytes;
- stored bytes used up with fewer than 20,960 bytes of output;
- 20,960 bytes of output with stored bytes left over.

Worked example. A chunk with table entry 5 and the ten stored bytes

```
00 02  4C 4B 00 00  D7 12  00 00
```

is: count 2, so the two words `4C 4B` and `00 00` are copied (4 bytes);
count `D712` = −10,478, so the word `00 00` is written 10,478 times (20,956
bytes). Output: `4C 4B 00 00` followed by 20,956 zero bytes, 20,960 in all.
The checksum of its 20,480 data bytes is `00004C4B`.

The chunk of an all-zero part of a disk has table entry 2 and the stored
bytes `D7 10 00 00` (count −10,480). CONFIRMED (real data): 30 such
chunks.

### 5.5 The LZH kind

The LZH kind is the adaptive-Huffman sliding-window method known from
LHarc's "-lh1-" (the public "LZHUF" scheme), applied to each chunk
separately, with one difference: **the window starts filled with zero
bytes**, not with spaces. Section 5 of [`../lha/methods.md`](../lha/methods.md)
describes the same code in more detail, with test vectors for the adaptive
tree; sections 5.3, 5.4 and 5.5 there apply unchanged, and the description
below is complete on its own.

Evidence: CONFIRMED (real data). All 704 LZH chunks of the 16 LZH images
decode to exactly 20,960 bytes using all but at most 7 bits of their stored
bytes, and the data and tag checksums of all 16 images match (5.0 million
literals, 1.0 million matches). With a window of spaces instead, none of
the 16 matches.

**Parameters.**

| Quantity | Value |
|---|---|
| Output of one chunk | exactly 20,960 bytes |
| Window | 4,096 bytes, initially all `00` |
| Symbols of the adaptive code | 314: values 0 to 313 |
| Literals | symbols 0 to 255 |
| Match lengths | symbols 256 to 313 mean lengths 3 to 60 (length = symbol − 253) |
| Distances | 1 to 4,096 |
| State carried from chunk to chunk | none: every chunk starts from the initial state |

**Bit order.** The stored bytes are read as a stream of bits, most
significant bit of each byte first. A field of several bits is read most
significant bit first.

**Stream structure.** The stream is a sequence of items. Each item is one
symbol of the adaptive code; after the symbol is decoded the code is
updated for it (below). If the symbol is 256 or more, a distance follows:
first a value from 0 to 63 sent with the fixed distance code (below), then
six plain bits; the distance is that value × 64 + the six-bit number + 1.

- A literal appends its byte to the output.
- A match appends *length* bytes, each one a copy of the byte *distance*
  positions before the position being written, one byte at a time, so that
  a match may overlap its own output. A position before the start of the
  chunk's output reads as `00`.

Matches reaching before the start of the output are normal: 523 of the 704
real chunks contain them, always reaching back at most 60 positions before
the start (the encoder's look-ahead length).

**The adaptive code.** The code is a binary tree with 314 leaves and 313
internal nodes, kept as a list of 627 positions, 0 to 626. Each position
holds a weight, and either a symbol (a leaf) or the position of its first
child (an internal node); the second child is always at the next position.
Weights never decrease along the list, and the weight of an internal node
is the sum of its children's weights.

- *Initial state.* Positions 0 to 313 are leaves; position *i* holds symbol
  *i* with weight 1. Positions 314 to 626 are internal; the node at
  position 314 + *j* has its first child at position 2 × *j* and weight
  equal to the sum of the weights at 2 × *j* and 2 × *j* + 1. The root is
  position 626.
- *Decoding a symbol.* Start at the root. While at an internal node, read
  one bit and move to the first child for a 0, the second child for a 1.
  The symbol of the leaf reached is the result.
- *Update after each symbol.* Starting at the leaf just decoded and moving
  up to the root: let *w* be the weight of the current node; find the
  highest position whose weight is also *w*; if that is a different
  position, exchange the contents (symbol or first-child position) of the
  two positions, and continue from the higher one; add 1 to its weight;
  then move to its parent. The parent of a position is the internal node
  whose first child is that position or the one just before it. Stop after
  the root's weight has been increased.

In the initial state symbols 116 to 313 have 8-bit codes and symbols 0 to
115 have 9-bit codes; for example symbol 0 is `110001100`, symbol 1 is
`110001101`, symbol 65 is `111001101`, symbol 256 is `10001100` and symbol
313 is `11000101`.

The LZHUF scheme rebuilds the tree when the root's weight reaches 32,768.
**That can never happen in a DART chunk**: the root starts at 314 and
gains 1 per symbol, and a chunk has at most 20,960 symbols because each
symbol produces at least one byte. An implementation needs no rebuild; if
it shares code with an "-lh1-" decoder the rebuild is simply never
reached.

**The fixed distance code.** The value 0 to 63 is sent with a prefix code
of these lengths:

| Values | Code length | Code of the first value of the group |
|---|--:|---|
| 0 | 3 | `000` |
| 1 to 3 | 4 | `0010` |
| 4 to 11 | 5 | `01010` |
| 12 to 23 | 6 | `100100` |
| 24 to 47 | 7 | `1100000` |
| 48 to 63 | 8 | `11110000` |

Within a group, each following value has the next higher binary code of
the same length (value 2 is `0011`, value 3 is `0100`, value 63 is
`11111111`). The code is complete: every bit sequence decodes.

**How a chunk ends.**

1. Decoding stops as soon as 20,960 bytes have been produced.
2. A match that would go past 20,960 bytes is cut off there
   (**DECISION D15**; no real chunk was seen to need it).
3. Stored bits left unread are ignored (**DECISION D15**; real chunks
   leave 0 to 7 bits).
4. A bit needed beyond the last stored byte is an error: "corrupt image".
   No substitute bits are supplied.

Worked example of the first item. The stored bytes `C5 00` begin with the
bits `11000101 000 000000`: symbol 313, a match of length 60; distance
value 0 and six-bit number 0, so distance 1. Sixty bytes are copied from
one position before the start of the output: sixty `00` bytes. (An "-lh1-"
decoder would produce sixty spaces here.) Section 8 has complete chunks.

### 5.6 The uncompressed forms

A chunk whose table entry is −1, and every chunk of an image with
compression kind 2, is stored as its 20,960 bytes, data first, tags last.
CONFIRMED (real data) for −1 entries in run-length images (39 chunks).

### 5.7 Checksums

A DART image carries no checksum in its data fork. Its Macintosh resource
fork holds:

- resource `CKSM`, ID 1: four bytes, the **tag** checksum;
- resource `CKSM`, ID 2: four bytes, the **data** checksum;
- resource `DART`, ID 0: a Pascal string repeating both, for example
  `1.5, tag checksum=$2CF2D5F1, data checksum=$46F79F68` (one older image
  has the form `T: $8F7A0C84 D: $89928878`).

Both are checksums of section 3: the data checksum over all block data of
the image, the tag checksum over **all** tag data of the image, including
the first 12 bytes. This is not the DiskCopy 4.2 rule. CONFIRMED (real
data): 21 of 21 images that came with a resource fork match on both values;
for images of kinds without tags the tag checksum is 0 and the tag bytes
are zero.

The image layer normally sees only the data fork, so it cannot verify a
DART image (**DECISION D16**): a DART image is accepted on the strength of
section 5.8 and of every chunk decoding without error. The run-length and
LZH rules above are strict enough that random damage is very likely to be
caught as an error.

### 5.8 Detection

An input is a DART image when all of the following hold
(**DECISION D17**):

1. It is at least 84 bytes long.
2. The compression kind is 0, 1 or 2.
3. The disk kind and disk size are one of the six combinations of
   section 5.2 (seven, counting both sizes of kind 1).
4. The input is at least as long as the header (84 or 148 bytes).
5. The first C table entries are each −1 or within the range section 5.3
   gives for the compression kind, and the remaining E − C entries are 0.
6. The header length plus the stored lengths of all C chunks is at most
   the input length, and the input is fewer than 128 bytes longer than
   that.

These tests read at most 148 bytes and involve no decompression. They are
strong: 40 or 72 constrained entries whose total must match the file
length.

The image layer delivers a DART image to the Lisa file-system layer only
for disk kinds 1, 2 and 3 (**DECISION D18**): the other kinds have no tags
and their sizes do not exist as Lisa disks. A Lisa disk may well have been
archived as kind 1, so kind 2 is not required.

The block data of a DART image of **any** kind is also what the crate's
Macintosh volume reader needs: when the first chunk decodes and block 2
carries an HFS, MFS or HFS+ signature, that reader is given the decoded
blocks without the tags. CONFIRMED (real data): four kind-1 images (one
MFS, three HFS) and two kind-16 images (HFS, 1,440K) are read this way
and match independent extractions.

## 6. Raw 532-byte-block hard-disk dumps

A ProFile or Widget presents blocks of 532 bytes to its host: 512 data
bytes and 20 tag bytes. Dump tools write these blocks to a file with no
container around them, or with a single header block. The tools differ in
two respects, and file names and extensions say nothing reliable about
either:

- whether the 20 tag bytes come **before** or **after** the 512 data bytes
  of each block;
- whether the blocks are in **logical order** or in **device order**.

### 6.1 Device sizes

| Device | Blocks | Raw length (blocks × 532) |
|---|--:|--:|
| ProFile 5 MB | 9,728 (`0x2600`) | 5,175,296 |
| ProFile 10 MB | 19,456 (`0x4C00`) | 10,350,592 |
| Widget 10 MB | 19,456 (`0x4C00`) | 10,350,592 |

Emulated drives can have any number of blocks (Cameo/Aphid documents
images of up to several gigabytes). A reader does not require one of the
sizes above.

### 6.2 What each tool writes

| Tool | Header | Within each block | Block order | Evidence |
|---|---|---|---|---|
| BLU (Basic Lisa Utility) | one block (section 6.4) | data, then tag | logical | CONFIRMED, 2 images |
| Cameo/Aphid, ArduinoFile, ESProFile (`*.image`) | none | tag, then data | device | CONFIRMED, 1 image holding a Lisa file system (94,208 blocks); tag position also on 1 Cameo/Aphid image without one |
| IDEFile | none | tag, then data | device | REFERENCE-ONLY |
| IDLE emulator "raw" images; LisaEm `raw-to-dc42` input | none | tag, then data | device by default | REFERENCE-ONLY |
| LisaEm `dc42-to-raw` output | none, or one spare-table block | data, then tag | logical by default | REFERENCE-ONLY |
| LisaEm `dc42-to-rraw` output | none | tag, then data | logical by default | REFERENCE-ONLY |

The LisaEm tools have options that change the order, so their output can
be any of the four combinations.

BLU images received by XMODEM are padded to a multiple of 1,024 bytes with
`1A` and `00` bytes: both BLU ProFile images measured are 5,177,344 bytes
long, 1,516 bytes more than 9,729 × 532. CONFIRMED (real data).

For a Widget, BLU is reported to behave the same way (data then tag,
logical order), and LisaEm's BLU converter contains special handling that
shifts Widget tags by two bytes for all blocks but the first. No Widget
image was available; see section 14.

### 6.3 The ProFile interleave

The Lisa operating system does not use ProFile blocks in device order. It
spreads consecutive logical blocks over each group of 16 device blocks:
logical block L is device block

    (L − L mod 16) + P[L mod 16]

where P is this table (P[k] = 5 × k mod 16):

| k | 0 | 1 | 2 | 3 | 4 | 5 | 6 | 7 | 8 | 9 | 10 | 11 | 12 | 13 | 14 | 15 |
|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|
| P[k] | 0 | 5 | 10 | 15 | 4 | 9 | 14 | 3 | 8 | 13 | 2 | 7 | 12 | 1 | 6 | 11 |

So logical blocks 0, 1, 2, 3, 4 are device blocks 0, 5, 10, 15, 4, and
logical block 17 is device block 21. The inverse table (device position
within the group to logical position) is 0, 13, 10, 7, 4, 1, 14, 11, 8, 5,
2, 15, 12, 9, 6, 3.

PUBLISHED by three independent tools (LisaEm, the IDEFile converter and the
Lisa-ProFile-Image-Tools), which agree on the table. CONFIRMED (real data)
on one device-order dump holding a Lisa file system (94,208 blocks, tag
first, section 11): read through this table it scores 7,658 in the test of
section 6.5 and, among others, logical block 38 carries the tag of the
volume header (file identifier `00 01`, relative page 0) that `lisafs.md`
expects there; read without the table it scores 3. Also CONFIRMED (crafted)
by re-ordering a real logical-order dump with this table.

The table applies to the whole device whatever its size: the 94,208-block
dump is interleaved in groups of 16 from the first block to the last.

A Widget does its interleaving inside the drive; its device order is the
logical order.

A dump whose block count is not a multiple of 16 cannot be in interleaved
device order (**DECISION D19**).

### 6.4 The header block

BLU puts one 532-byte block in front of the disk blocks. It is the drive's
own identification block (the block a ProFile returns for block number
`FFFFFF`), with BLU's signature in the place of its tag:

| Offset | Size | Field | Value seen |
|--:|--:|---|---|
| `0x00` | 13 | device name, ASCII, padded with spaces | `PROFILE      ` |
| `0x0D` | 3 | device number | `00 00 00` |
| `0x10` | 2 | firmware revision | `03 98` |
| `0x12` | 3 | number of blocks B | `00 26 00` |
| `0x15` | 2 | bytes per block | `02 14` (532) |
| `0x17` | 1 | spare blocks available | `20` |
| `0x18` | 1 | spare blocks allocated | `00` |
| `0x19` | 1 | bad blocks | `00` |
| `0x1A` | | spare and bad block lists, ended by `FF FF FF`; rest of the block `FF` | |
| `0x200` | 20 | `Lisa HD Img BLUV0.10` (the version digits vary) | |

CONFIRMED (real data), 2 images of 5 MB ProFiles. PUBLISHED: device names
`PROFILE 10M  ` and `Widget-10    ` and device numbers `00 00 10` (10 MB
ProFile) and `00 10 00` or `00 01 00` (Widget; the sources disagree) for
the other drives. Only B and the bytes-per-block field are used.

Disk block *b* of the image is at offset (*b* + 1) × 532.

LisaEm's `dc42-to-raw` can write the same block without the BLU signature.

### 6.5 Recognising the layout

Because the tools cannot be told apart by anything they write, a reader
decides the layout by looking at the tags themselves. The test uses only
two tag fields, the file identifier (tag bytes 4 to 5) and the relative
page (tag bytes 12 to 13), and one property of every Lisa volume: files are
mostly stored in runs of consecutive blocks, so that in the right layout
very many blocks are followed by the next page of the same file.

A **layout** is a choice of tag position (tag first, or data first) and of
block order (logical, or device order with the interleave of section 6.3).
Its **score** is the number of logical block numbers L, from 0 to N − 2,
for which, reading tags according to that layout, the tag of block L and
the tag of block L + 1 have the same file identifier, that identifier is
not `00 00`, and the relative page of block L + 1 is exactly one more than
that of block L.

The scores measured on a real 5 MB ProFile (9,728 blocks), written out in
each of the four layouts and then read back under each of the four:

| Read as → | tag first, logical | tag first, device | data first, logical | data first, device |
|---|--:|--:|--:|--:|
| true layout | 3,887 | 3,887 | 3,887 | 3,887 |
| best wrong layout | 1 | 3 | 3 | 1 |

Two real dumps, each read under all four layouts:

| Dump | tag first, logical | tag first, device | data first, logical | data first, device |
|---|--:|--:|--:|--:|
| BLU dump of a 5 MB ProFile (9,728 blocks after the header block) | 0 | 1 | 2,224 | 0 |
| ArduinoFile-style image of 94,208 blocks | 3 | 7,658 | 1 | 0 |

Random bytes and a non-Lisa hard-disk image (a boot-menu program for
Cameo/Aphid) score 0 in all four.

A second property exists and is deliberately not part of the rule: on a
ProFile written by the Lisa operating system the exclusive-or of all 532
bytes of a block is zero whenever the tag's checksum-valid flag is set
(`lisafs.md` section 3.3; CONFIRMED on the first 3,000 blocks of both dumps
above). It holds for either tag position and either block order, so it
cannot choose a layout; and it is not written by every system that uses a
ProFile. An implementation may use it only as an extra reason to *reject*
(for example, requiring that at least half of the first 64 blocks whose
flag is set satisfy it), never as a reason to accept.

**DECISION D20.** The layout with the highest score is used, provided that
score is at least 16 and at least four times the second-highest score.
Otherwise the input is not a tagged image. (A freshly initialised Lisa
volume already scores more than 16 from its boot blocks alone.)

The same test decides the block order of a DiskCopy 4.2 image with 20-byte
tags (decision D8): there the tag position is known, so only the two
orders are scored; if neither qualifies, the image is delivered in file
order.

### 6.6 Detection

An input is examined as a raw dump as follows (**DECISION D21**).

1. **Header block.** If the input is at least 1,064 bytes long, the two
   bytes at `0x15` are `02 14`, the 13 bytes at `0x00` are all in the range
   `20` to `7E`, and B (the 24-bit number at `0x12`) is not 0, the first
   block is a header. Then (B + 1) × 532 must be at most the input length,
   and the input must be fewer than 2,048 bytes longer than that; otherwise
   the input is not a tagged image. The disk blocks are the B blocks after
   the header; padding after them is ignored. The signature at `0x200` is
   not required.
2. **No header.** Otherwise the input length must be a multiple of 532 and
   at least 64 × 532 bytes; the whole input is disk blocks.
3. **Layout.** The four layouts of section 6.5 are scored (two, if the
   block count is not a multiple of 16) and decision D20 is applied. For a
   BLU image this normally confirms "data first, logical".
4. The blocks and tags are delivered in logical order.

A raw dump has no magic number and no checksum. Step 3 is what keeps this
detection from claiming unrelated files whose length happens to be a
multiple of 532; it must run before any large allocation (section 9.1).

Dumps with 536-byte blocks (24-byte tags) are not recognised (decision
D3).

## 7. Outer wrappers and order of detection

- **MacBinary.** DiskCopy 4.2 and DART images are Macintosh files and
  travel in MacBinary. The image is the data fork. Unwrapping MacBinary is
  the job of the crate's existing MacBinary reader, which hands the data
  fork to format detection again; the image layer itself looks only at
  offset 0 of what it is given (**DECISION D22**). The resource fork is not
  needed for anything.
- **Order.** The image layer tries DiskCopy 4.2 first, then DART, then the
  raw dump (**DECISION D23**). The three tests cannot both succeed on one
  input in practice: a DART header puts a byte of at most 2 at offset 0 and
  a DiskCopy 4.2 name of that length would need `01 00` at `0x52`, which in
  a DART file is the last table entry (256 would have to be a valid stored
  length and everything else would have to add up).

## 8. Test vectors

### 8.1 LZH chunks

Both vectors were produced by a purpose-built encoder that follows section
5.5. Vector V2, which does not depend on the initial window contents, was
wrapped as an "-lh1-" member of an LHA archive and extracted with `unar`
1.10.7, giving the expected bytes (CONFIRMED (crafted)); vector V1 wrapped
the same way gives 20,960 **spaces** in `unar`, which shows the one
difference between the two methods.

**V1: an all-zero chunk.** 478 stored bytes (table entry 478). Items: 349
matches of length 60 at distance 1, then one match of length 20 at distance
1. Output: 20,960 bytes of `00`. Data checksum `00000000`, tag checksum
`00000000`.

```
c5 00 62 80 30 80 31 00 2f 00 60 00 c0 01 80 02 c0 0b 80 2e 00 b8 02 e0
0b 80 2e 00 b8 02 80 14 00 a0 05 00 28 01 40 0a 00 50 02 80 16 00 b0 05
80 2c 01 60 0b 00 58 02 00 20 02 00 20 02 00 20 02 00 20 02 00 20 02 00
20 02 00 20 02 00 20 02 00 20 02 00 20 02 00 20 02 00 20 02 00 28 02 80
28 02 80 28 02 80 28 01 00 20 04 00 80 10 02 00 40 08 01 00 20 04 00 80
10 02 00 40 08 01 00 20 04 00 80 10 02 00 40 08 01 00 20 04 00 80 10 02
00 40 08 01 00 20 04 00 80 10 02 00 40 08 01 00 20 04 00 80 10 02 00 40
08 01 00 20 04 00 80 10 02 00 40 08 01 00 40 08 01 00 20 04 00 80 10 03
00 60 0c 01 80 30 06 00 c0 18 03 00 60 0c 01 80 30 06 00 c0 18 03 00 60
0c 01 80 30 06 00 c0 18 03 00 60 0c 01 80 30 06 00 c0 18 03 00 60 0c 01
80 30 06 00 c0 18 03 00 60 0c 01 80 30 06 00 c0 18 03 00 60 0c 01 80 30
06 00 c0 18 03 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 40 10
04 01 00 40 10 04 01 00 40 10 04 01 00 40 10 04 01 00 40 10 04 01 00 40
10 04 01 00 40 10 04 01 00 40 10 04 01 00 40 10 04 01 00 37 a0 00
```

SHA-256 of the stored bytes:
`719f6c226a8f9dede48167cf67e3d56e583ac20d2117c7b2403e12cfe28e8539`.
The stream has 3,820 bits; the last 4 bits are padding.

**V2: literals and far matches.** 820 stored bytes (table entry 820).
Items: the 251 literals 0, 1, …, 250, then 346 matches at distance 251: 345
of length 60 and a last one of length 9. Output: byte *n* of the chunk is
*n* mod 251, for *n* from 0 to 20,959. Data checksum (first 20,480 bytes)
`19FDC5C3`, tag checksum (last 480 bytes) `6E77D24E`. SHA-256 of the
20,960 output bytes:
`5c964e158fa5fe7193158f6c0f920a6e5267bc3c92ca1bf3f68655a9210ee62c`.

```
c6 63 71 d8 fc 86 47 25 93 ca 65 72 d9 7c c6 67 35 9b ce 67 73 d9 fd 06
87 45 a3 d2 69 74 da 7d 46 a7 55 ab d6 6b 75 da fd 86 c7 65 b3 da 6d 76
db 7d c6 e7 75 bb de 6f 77 db fe 07 07 85 c3 e2 71 78 dc 7e 47 27 95 cb
e6 73 79 dc fe 87 47 a5 d3 ea 75 7a dd 7e c7 67 b5 db ee 77 7b dd ff 07
87 c5 e3 f2 79 7c de 7f 47 a7 d5 eb f6 7b 7d de ff 87 c7 e5 f3 fa 7d 7e
df 7f c7 e7 f5 fb fe 7f 7f df f0 00 10 20 30 40 50 60 70 80 90 a0 b0 c0
d0 e0 f1 01 11 21 31 41 51 61 71 81 91 a1 b1 c1 d1 e1 f2 02 12 22 32 42
52 62 72 82 82 72 62 52 42 32 22 12 01 f1 e1 d1 c1 b1 a1 91 81 71 61 51
41 31 21 11 00 f0 e0 d0 c0 b0 a0 90 80 70 60 50 40 30 20 10 0f ff fb fb
fc fd fe bf 3f 8f bf db eb f4 f9 fc be 3f 0f 7f bb db ec f5 fa bd 3e 8f
3f 9b cb e4 f1 f8 bc 3e 0e ff 7b bb dc ed f6 bb 3d 8e bf 5b ab d4 e9 f4
ba 3d 0e 7f 3b 9b cc e5 e3 13 ae 52 75 c8 9d 72 27 5c 53 ae 29 d7 14 eb
8a 75 ba 75 ba 75 ba 75 ba 75 c2 75 c2 75 c2 75 c2 75 a4 eb 49 d6 93 ad
27 5b 4e b6 9d 6d 3a da 75 b4 eb 69 d6 d3 ad a7 5b 4e b6 9d 6d 3a da 75
89 d6 27 58 9d 62 75 89 d6 27 58 9d 62 75 89 d6 27 58 9d 62 75 89 d6 27
58 9d 62 75 89 d6 27 58 9d 62 75 a9 d6 a7 5a 9d 6a 75 a9 d6 a7 5a 9d 6a
75 a9 d6 a7 5a 9d 6a 75 53 aa 9d 54 ea a7 55 3a a9 d5 4e aa 75 53 aa 9d
54 ea a7 55 3a a9 d5 4e aa 75 53 aa 9d 54 ea a7 55 3a a9 d5 4e aa 75 53
aa 9d 54 ea a7 55 3a a9 d5 4e aa 75 53 aa 9d 54 ea a7 55 3a a9 d5 4e aa
75 53 aa 9d 54 ea a7 55 3a a9 d5 4e aa 75 53 aa 9d 54 ea a7 59 3a c9 d6
4e b2 75 93 ac 9d 64 eb 27 59 3a c9 d6 4e b2 74 a7 4a 74 a7 4a 74 a7 4a
74 a7 4a 74 a7 4a 74 a7 4a 74 a7 4a 74 a7 4a 74 a7 4a 74 a7 4a 74 a7 4a
74 a7 4a 74 a7 4a 74 a7 4a 74 a7 4a 74 a7 4a 74 a7 4a 74 a7 4a 74 a7 4a
74 a7 4a 74 a7 4a 74 a7 4a 74 a7 4a 74 a7 4a 74 a7 4a 74 a7 4a 75 27 52
75 27 52 75 27 52 75 27 52 75 27 52 75 27 52 75 27 52 75 27 52 75 27 52
75 27 52 75 27 52 75 27 52 75 27 52 75 27 52 75 27 52 75 27 52 75 27 52
75 27 52 75 27 52 75 27 52 75 27 52 75 27 52 75 27 52 75 27 52 75 27 52
75 27 52 75 27 52 75 27 52 75 27 52 75 27 52 75 27 52 75 27 52 75 27 52
75 27 52 75 27 52 75 27 52 75 27 52 75 27 52 75 a7 5a 75 a7 5a 75 a7 5a
75 a7 5a 75 a7 5a 75 a7 5a 75 a7 5a 75 a7 5a 75 a7 5a 75 a7 5a 75 a7 5a
75 a7 5a 75 a7 5a 75 a7 5a 75 a7 5a 75 a7 5a 75 a7 5a 75 a7 5a 75 a7 5a
75 a7 5a 75 a7 5a 75 a7 5a 75 a7 5a 75 a7 5a 75 a7 5a 75 a7 5a 74 4e 89
d1 3a 27 44 e8 9d 13 a2 74 4e 89 d1 3a 27 44 e8 9d 13 a2 74 4e 89 d1 3a
27 44 e8 9d 13 a2 74 4e 89 d1 3a 27 44 e8 9d 13 a2 74 4e 89 d1 3a 27 44
eb cf a7 40
```

SHA-256 of the stored bytes:
`ad3c4408a9d19af82ebd1f90872131f2bd4c34d206c5b18b417d6838d76506a2`.
The stream has 6,555 bits.

Error vectors derived from these: V1 or V2 with its last stored byte
removed must fail with "corrupt image" (a bit is needed beyond the end);
V1 with one extra stored byte appended must decode to the same output.

### 8.2 Whole images that can be built from the rules

These are recipes, not files; each fixes every byte.

1. **DART, Lisa kind, LZH.** Header `01 02 01 90`; table: twenty entries of
   478 (`01 DE`), then twenty entries of 0; then vector V1 twenty times.
   Length 84 + 20 × 478 = 9,644 bytes. Result: 800 blocks of zeros and
   9,600 zero tag bytes, delivered as 16,000 canonical tag bytes in which
   every tag is `00 00 00 00 00 00 00 00 FF FF FF 00 00 00 00 00 00 00 00
   00`. (An all-zero 12-byte tag has link fields 0, not `7FF`, so the
   canonical links are 0.)
2. **DART, Macintosh 800K, run-length.** Header `00 01 03 20`; table: forty
   entries of 2; then `D7 10 00 00` forty times. Length 244 bytes. Result:
   1,600 zero blocks.
3. **DART with a raw chunk.** As recipe 2, but entry 0 is `FF FF` and chunk
   0 is 20,960 stored bytes of the implementer's choosing. Length 84 +
   20,960 + 39 × 4 = 21,200 bytes.
4. **DART rejections.** Recipe 2 with one table entry changed to 3 (the
   total no longer matches the file length: not a DART image); recipe 2
   with the stored bytes of one chunk changed to `D7 11 00 00` (count
   −10,479: output falls short, corrupt image) or to `00 00 00 00` (zero
   count, corrupt image); recipe 1 with disk kind 4 (not a DART image).
5. **DiskCopy 4.2, 12-byte tags.** Name length 22 and the name
   `-not a Macintosh disk-`; D = 409,600; T = 9,600; encoding `00`, format
   `02`; magic `01 00`. With all-zero data and all-zero tags both checksums
   are 0. Changing tag byte 0 to `AA` leaves the correct tag checksum at 0
   (the first 12 tag bytes are skipped); changing tag byte 12 to `00` and
   tag byte 13 to `01` makes the correct tag checksum the value obtained
   by rotating `00000001` right 4,794 times in all (once for its own word
   and once for each of the 4,793 words after it), which is `00000040`.
6. **DiskCopy 4.2 rejections.** Recipe 5 with the byte at `0x00` set to 64;
   with D set to 409,601; with T set to 9,601; with the file cut one byte
   short; with 2,048 extra bytes appended.
7. **Twiggy.** Recipe 5 with D = 871,424, T = 20,424, encoding `54`, format
   `01`, and block *b* of the data area filled with the 16-bit number *b*
   repeated. The delivered logical block 851 then holds 1,687, block 866
   holds 1,672, block 1,215 holds 1,301, block 1,662 holds 851, block
   1,684 holds 1,320 and block 1,701 holds 1,337; blocks 0 to 850 hold
   their own numbers.
8. **Raw dumps.** Take any tagged Lisa volume of at least 64 blocks whose
   block count is a multiple of 16, with 20-byte tags, and write it in
   each of the four layouts of section 6.5, with and without a header
   block of section 6.4, with and without padding to a multiple of 1,024.
   All sixteen files must deliver the same blocks and tags.

## 9. Safety requirements for untrusted input

Every field of every format is untrusted.

### 9.1 Cost of saying no

The image layer is asked about every input the loader sees, before the
Macintosh readers. For an input that is not a tagged image it must answer
after reading a bounded number of header bytes (sections 4.7 and 5.8) or,
for the raw-dump test, after reading 20 bytes per 532 of the input (section
6.6); it must not copy, allocate in proportion to the input, or decompress
anything before the applicable detection rule has passed. For raw dumps the
length test (a multiple of 532, or a plausible header block) comes before
the scoring pass.

Where the surrounding code allows it, recognising an image and producing
its blocks should be separate steps, so that asking "is this a Lisa disk"
about a DiskCopy 4.2 image of a Macintosh disk does not copy the image.

### 9.2 Arithmetic and bounds

- 84 + D + T, (B + 1) × 532 and the sum of DART chunk lengths are computed
  with overflow checks, in a type wider than 32 bits or with checked
  operations; an overflow is "not a tagged image".
- Every read is bounds-checked against the input. No offset derived from a
  header is used before the detection rule that validates it has passed.
- The Twiggy reorder and the interleave are permutations of block numbers
  below N; an implementation computes each source index from the tables of
  sections 4.5 and 6.3 and never from image contents.

### 9.3 Output size

- DiskCopy 4.2 and raw dumps: the delivered data and tags are never larger
  than the input (canonical tags are at most 20 bytes per 512-byte block
  that is present in the input).
- DART: the output is fixed by the header: at most 72 chunks × 20,960
  bytes = 1,509,120 bytes, whatever the input says. An implementation
  reserves memory only after section 5.8 has passed, and may reserve the
  exact size.
- All delivered bytes are charged against the caller's output budget like
  any other decoded data.

### 9.4 Decoder termination

- Run-length: every record consumes at least 4 stored bytes or fails; the
  number of records is bounded by the stored length.
- LZH: every item produces at least one output byte or fails; a chunk is at
  most 20,960 items. A walk down the adaptive tree is at most 313 steps;
  an implementation that reaches a position outside 0 to 626, or a walk
  longer than that, has a bug, and must fail rather than loop.
- An error in any chunk fails the whole image. No partial image is
  delivered (**DECISION D24**): the file-system layer has no way to know
  which blocks are missing.

### 9.5 Nothing is trusted because it looks like a known tool

Names, signatures, encoding and format bytes, device names and file sizes
are never grounds for skipping a check.

## 10. Observed implementation gaps

Nothing in this section is to be imitated.

1. **Aaru, DiskCopy 4.2 tag checksum.** Its verification computes the tag
   checksum over all tag bytes. That matches no genuine image with a
   non-zero first tag (section 4.4).
2. **Aaru, DART chunk lengths.** It reads 2 × L stored bytes for LZH
   chunks as well as for run-length chunks. The published description and
   all 16 LZH images say L bytes. Its LZH tests are disabled.
3. **Aaru, DART compression kind 2.** A non-negative entry is sent to the
   LZH decoder.
4. **Aaru, DART LZH window.** Its initial window is a pattern of byte runs
   borrowed from a different LHA method, with zeros only in the last 238
   positions. Real data reaches back at most 60 positions, where the two
   agree, so the difference is not observable; this specification says all
   zeros.
5. **Aaru and LisaEm, run-length count 0.** Both skip a zero count and
   carry on. Neither checks that a chunk expands to exactly 20,960 bytes;
   both index past the end of the stored bytes on a truncated record.
6. **LisaEm, DART.** Any negative table entry is treated as "not
   compressed"; LZH is not supported in the default build; the file is
   read without checking for its end.
7. **LisaEm, DiskCopy 4.2 sizes.** A file up to 1,024 bytes shorter than
   its header claims is accepted; the tag size per block is the integer
   quotient T ÷ N with no check that it divides.
8. **LisaEm, tag checksum with 20-byte tags.** The first block's whole tag
   (20 bytes) is skipped, where Aaru skips nothing and DiskCopy 4.2 itself
   skips 12 bytes. Images are created with both checksums zero.
9. **LisaEm, MacBinary test.** Its test for a MacBinary header checks four
   bytes and can mistake other files for MacBinary.
10. **LisaEm BLU converter.** The signature comparison happens after the
    header has been used; the Widget tag shift is a guess documented as
    such in the tool.
11. **Aaru, BLU detection.** Any file whose first 13 bytes are printable
    and whose bytes-per-block field has `02` in its upper byte is accepted;
    the block count is not compared with the file length.
12. **All dump converters.** The layout (tag position, interleave) is
    taken from the command line or assumed from the tool name, which is why
    wrongly converted images circulate. Section 6.5 exists for this
    reason.

## 11. Real files measured

None of these files is in the repository. They are kept, with their
origins and digests, in the specimen cache outside it.

| Kind | Files | What was checked |
|---|--:|---|
| DiskCopy 4.2, 400K, 12-byte tags, encoding `00`, format `02` | 40 | both checksums (tag checksum without the first 12 bytes); 26 bare, 14 in MacBinary with a `dCpy` resource; Lisa Office System 1.0 to 3.1, Lisa Workshop, Pascal Workshop, Monitor, Xenix, MacWorks boot disks, one MFS volume |
| DiskCopy 4.2, 800K, 12-byte tags, encoding `01`, format `22` | 1 | both checksums; an HFS volume |
| DiskCopy 4.2, Twiggy, encoding `54`, format `01` | 11 | both checksums; 10 written by BLU with 996 padding bytes; the block order of section 4.5 on the 3 that use the second side |
| DART, LZH, Macintosh 800K | 14 | every chunk decodes exactly; data and tag checksums from the resource fork |
| DART, LZH, Macintosh 1440K | 2 (+2) | as above; two further files had no resource fork and a damaged tail: 70 and 69 of their 72 chunks decode exactly, the rest fail as "corrupt image" |
| DART, run-length, Macintosh 800K | 4 | as above; includes −1 chunks |
| DART, run-length, Macintosh 1440K | 1 | as above |
| BLU ProFile 5 MB dump | 2 | header fields, padding, tag position, logical order (3,887 and 2,224 scoring pairs, 0 under the interleave) |
| Cameo/Aphid 5 MB image (not a Lisa volume) | 1 | tag position (`AA AA` at bytes 4 to 5 of the block) |
| ArduinoFile-style image, 94,208 blocks, no header, 50,118,656 bytes (Lisa Office System 3 volume) | 1 | tag first; device order with the interleave of section 6.3 (7,658 scoring pairs, 3 without the interleave, at most 1 with the tag after the data) |

Not available, and therefore not CONFIRMED anywhere above: a DART image of
disk kind 2, 3, 17 or 18 or of 400K; a DART image of compression kind 2; a
DiskCopy 4.2 image with 20-byte tags; a DiskCopy 4.2 image of a 720K or
1440K disk; a Widget dump; an IDEFile image; a 10 MB ProFile dump.

## 12. Decisions

| No. | Section | Decision |
|---|---|---|
| D1 | 2.1 | Tags are delivered as N × 20 canonical bytes or not at all. |
| D2 | 2.3 | The canonical tag is the 20-byte hard-disk layout; 12-byte tags are widened by the table of section 2.3. |
| D3 | 2.3, 6.6 | 24-byte tags and 536-byte blocks are not supported. |
| D4 | 3 | A trailing odd byte is left out of a checksum. |
| D5 | 4.1 | The disk name is not interpreted. |
| D6 | 4.3 | Encoding and format bytes do not accept or reject an image. |
| D7 | 4.4 | Checksums never reject an image and need not be computed. |
| D8 | 4.5, 6.5 | Images with 20-byte tags get the block-order test; file order if it is inconclusive. |
| D9 | 4.5 | The Twiggy reorder applies exactly for encoding `54` with D = 871,424. |
| D10 | 4.5 | The Macintosh-Twiggy half swap is not applied. |
| D11 | 4.6 | Little-endian header fields are not supported. |
| D12 | 4.7 | The seven-part DiskCopy 4.2 detection rule. |
| D13 | 5.3 | Compression kind 2: every non-zero entry is 20,960 stored bytes. |
| D14 | 5.4 | The five run-length errors. |
| D15 | 5.5 | An LZH match is cut at the chunk end; unread bits are ignored. |
| D16 | 5.7 | DART images are not checksum-verified by the image layer. |
| D17 | 5.8 | The six-part DART detection rule. |
| D18 | 5.8 | Only disk kinds 1, 2 and 3 are delivered to the Lisa file-system layer; block data of every kind goes to the Macintosh reader. |
| D19 | 6.3 | A block count that is not a multiple of 16 rules out device order. |
| D20 | 6.5 | Highest score wins if it is at least 16 and four times the runner-up. |
| D21 | 6.6 | The raw-dump detection procedure. |
| D22 | 7 | The image layer does not unwrap MacBinary itself. |
| D23 | 7 | Order of tests: DiskCopy 4.2, DART, raw dump. |
| D24 | 9.4 | A failed chunk fails the image. |

## 13. Sources

Reference implementations and descriptions consulted by the author of this
document. Implementers do not read the implementations.

| Source | Used for | Licence |
|---|---|---|
| Apple II File Type Note `$E0/$0005`, "Apple Disk Copy 4.2 image" (as quoted in LisaEm's library) | DiskCopy 4.2 header, published encoding and format values | Apple technical note |
| `DartIntf.h`, "Some notes on reading a DART image file", DART 1.5.3 (Ken McLeod, Apple, 1994), from the MungeImage 1.2.0 source distribution | DART header, units of the block lengths, chunk size, checksum resources | Apple; published "for read-only use" |
| Aaru Data Preservation Suite, <https://github.com/aaru-dps/Aaru> (DiskCopy 4.2, DART, BLU image readers; Lisa tag decoder) and <https://github.com/aaru-dps/Aaru.Compression.Native> (Apple run-length and LZH decoders) | field values seen in the wild, Twiggy order, tag layouts, behaviour compared in section 10 | LGPL-2.1 (image readers and compression), MIT (decoders library) |
| LisaEm 1.2.7, <https://github.com/rayarachelian/lisaem> (`libdc42`, the `blu-to-dc42`, `raw-to-dc42`, `rraw-to-dc42`, `dc42-to-raw`, `dc42-to-rraw`, `dc42-to-blu` and `idefile-to-dc42` tools, ProFile emulation) | LisaEm image conventions, interleave table, dump layouts, behaviour compared in section 10 | GPL v2/v3 (LisaEm), LGPL (`libdc42`), BSD-style (IDEFile converter, Natalia Portillo) |
| Cameo/Aphid documentation, <https://github.com/stepleton/cameo> | `*.image` layout and sizes | public domain |
| ESProFile and ArduinoFile documentation, <https://github.com/alexthecat123/ESProFile> | image naming, the note that BLU images differ from its own by the 5:1 interleave | see repository |
| Lisa-ProFile-Image-Tools README, <https://github.com/RolandJuno/Lisa-ProFile-Image-Tools> | tag-first layout and 5:1 interleave of emulator images | see repository |
| The Unarchiver command-line tools (`unar` 1.10.7), used as a black box | cross-check of test vector V2 | LGPL-2.1 |
| [`../lha/methods.md`](../lha/methods.md), section 5 | wording and test vectors of the adaptive code | this repository |
| Disk images from `bitsavers.org/bits/Apple/Lisa/` (DiskCopy 4.2 floppy and Twiggy images, BLU ProFile dumps), Macintosh DiskCopy 4.2 and DART images from public software archives, and `LOS Compilation Base.image` from <https://github.com/alexthecat123/LisaSourceCompilation> | measurements of section 11; worked examples quote short byte sequences | third-party software images; not redistributed, not committed |

## 14. Open questions

1. **Widget dumps made with BLU.** Whether the tag of each block really
   follows the data unchanged. The scoring test of section 6.5 would reject
   a shifted layout rather than misread it.
2. **Tag checksum of LisaEm images** (20-byte tags): which bytes are
   skipped. Harmless under decision D7.
3. **Device numbers** of the 10 MB ProFile and the Widget in the header
   block; not used.
4. **Lisa-kind DART images.** Everything about DART was confirmed on
   Macintosh-kind images only. The format gives no reason to expect a
   difference: the disk kind byte does not influence decoding.
5. **Track 23 of the Twiggy second side.** Confirmed by data on three
   disks, unexplained.
6. **The upper five bits of the floppy link words** as a bytes-in-use
   count: inferred from data, not found in any description consulted.
