# Unix `compress` (`.Z`) fixtures

Every stream here was written by an existing `compress` program used as a black box on generated content, or is such a stream with bytes cut off, appended or changed as listed below. No byte comes from a real-world file. The generator and the tools that traced and edited the streams are deliberately not part of this repository.

Producers:

| Short name | Program | Where |
|---|---|---|
| `compress` | `/usr/bin/compress` of macOS 27.0.1 (26A434), `file_cmds-487.0.1` (the FreeBSD `compress`) | host |
| compress 4.0 | `compress.c` 4.0 (85/07/30) as shipped in 4.3BSD (`Berkeley 5.9 5/11/86`), built unmodified with GCC 14.2.0 (`-std=gnu89 -fcommon -DUSERMEM=4000000`) | Debian 13.7 container, arm64 |
| ncompress | Debian package `ncompress` 5.0-3 | same container |

compress 4.0 writes byte for byte the same stream as the macOS `compress` for every input used here (all of `one` ... `shift-b16` below), so each of those fixtures stands for both. The one exception is the empty input: macOS `compress` writes a file of zero bytes, compress 4.0 and ncompress write the three header bytes. ncompress 5.0 writes the same bytes as the other two for every input without a CLEAR code.

`manifest.tsv` holds what a reader has to produce. Every row was checked against the decoders listed under "Decoders".

## Generated content

One generator makes everything. It keeps a 32-bit state that starts at a seed. For each byte it replaces the state by state XOR (state shifted left 13, kept to 32 bits), then by state XOR (state shifted right 17), then by state XOR (state shifted left 5, kept to 32 bits); the byte is the low 8 bits of the state.

- `RAND(seed, n)`: n generator bytes.
- `LETTERS(seed, n)`: n characters, each from one generator byte taken modulo 27: 0 to 25 give `a` to `z`, 26 gives a space.
- `TEXT(seed, n)`: lines numbered from 0. A line is the line number as five decimal digits, then seven times a space and a word, then LF. Each word takes one generator byte; its low five bits index this list: amber basin cedar delta ember fjord glade harbor island jungle kettle lagoon meadow nectar orchard pebble quarry ripple summit timber upland valley willow xenon yonder zephyr anchor beacon canyon dynamo engine falcon. The result is cut to n bytes.
- `BIG` is `TEXT(0x2545F491, 700000)`; its first line is `00000 anchor lagoon meadow glade pebble delta anchor`. `BIG[:n]` is its first n bytes.

## Valid streams

"Codes" counts every code in the stream including CLEAR codes. Bit positions count from the start of the file, bit 0 being the low bit of byte 0.

| Fixture | Bytes | Made with | Plaintext | Covers |
|---|---|---|---|---|
| `empty.Z` | 3 | compress 4.0, ncompress (identical) | nothing | header only: `1F 9D 90` |
| `one.Z` | 5 | `compress -c` | `A` | one 9-bit code, 7 unused bits: `1F 9D 90 41 00` |
| `aaa.Z` | 6 | `compress -c` | `aaa` | codes 97, 257: the second code is the entry being defined (KwKwK) at the first possible place: `1F 9D 90 61 02 02` |
| `run-a.Z` | 116 | `compress -c` | 5000 x `a` | 100 codes, 98 of them KwKwK |
| `run-ab.Z` | 177 | `compress -c` | 3000 x `ab` | 154 codes, 75 KwKwK |
| `zeros.Z` | 530 | `compress -c` | 100000 zero bytes | 447 codes, 445 KwKwK, strings of several hundred bytes, into 10-bit codes; 189:1 |
| `text.Z` | 1469 | `compress -c` | `BIG[:4000]` | 1159 codes, widths 9 to 11, 3 unused bits at the end |
| `random-b16.Z`, `random-b10.Z` | 4063, 3689 | `compress -f -b 16 random.bin`, `compress -f -b 10 random.bin` | `RAND(0x9E3779B9, 3000)` | grows instead of shrinking; without `-f` `compress` exits with status 2 and writes nothing (`compress -c` writes the same bytes without `-f`). The first 256 codes are all literals. With `-b 10` the table is full after 768 codes and 2206 more 10-bit codes follow with no CLEAR |
| `wN-at.Z`, `wN-after.Z` for N = 10, 11, 12, 13 | 291/293, 931/933, 2339/2341, 5411/5413 | `compress -c` | `BIG[:L]`, L = 428/430, 2075/2078, 7451/7454, 19457/19464 | `at`: the last code is the last one of width N-1 (256, 768, 1792, 3840 codes) and the file ends on a byte boundary. `after`: one more code, the first of width N, then unused bits |
| `shift-bN.Z` for N = 9 ... 16 | see below | `compress -b N -c` | see below | every maximum width; table filled; codes with a full table; one CLEAR; widths starting again at 9 |
| `nc-shift-b12.Z` | 13303 | ncompress `compress -b 12 -c` | as `shift-b12.Z` | second producer: CLEAR one code earlier, padding of zero bits |
| `nb-*.Z` | see below | compress 4.0 `-C` | see below | header without the block-mode bit |
| `files.tar.Z` | 5243 | GNU tar 1.35, then `compress -c` | a 20480-byte ustar archive | see below |

### `shift-bN.Z`

Plaintext: `BIG[:P]`, then `RAND(0x9E3779B9, 1000)`, then `LETTERS(0x1234ABCD, 2000)`, then `TEXT(0x0BADC0DE, 13000)`. P is 500 bytes past the point where the code table becomes full, and at least 10500.

| Fixture | Bytes | Flag byte | P | Plaintext size | Codes | CLEAR at bit | CLEAR width | Padding bits after CLEAR | Plaintext bytes before CLEAR |
|---|---|---|---|---|---|---|---|---|---|
| `shift-b9.Z` | 16769 | `89` | 10500 | 26500 | 13461 | 104448 | 10 | 30 | 20000 |
| `shift-b10.Z` | 10943 | `8A` | 10500 | 26500 | 8796 | 69288 | 10 | 70 | 20009 |
| `shift-b11.Z` | 10522 | `8B` | 10500 | 26500 | 7830 | 67211 | 11 | 66 | 20009 |
| `shift-b12.Z` | 13287 | `8C` | 19957 | 35957 | 9310 | 89332 | 12 | 24 | 29460 |
| `shift-b13.Z` | 20358 | `8D` | 45187 | 61187 | 13367 | 145910 | 13 | 13 | 54690 |
| `shift-b14.Z` | 35007 | `8E` | 98738 | 114738 | 21463 | 262996 | 14 | 70 | 108242 |
| `shift-b15.Z` | 65864 | `8F` | 222960 | 238960 | 37669 | 509904 | 15 | 105 | 232469 |
| `shift-b16.Z` | 131565 | `90` | 532768 | 548768 | 70303 | 1035448 | 16 | 80 | 542272 |
| `nc-shift-b12.Z` | 13303 | `8C` | 19957 | 35957 | 9320 | 89320 | 12 | 36 | 29456 |

The CLEAR codes were confirmed by decoding each stream code by code with a tracing tool: code 256 appears once in each stream, at the bit position given, and every decoder listed below only reproduces the plaintext because it resets there. Each stream first fills its table (the last free code, 2^N - 1, is assigned), goes on with a full table for about 10000 plaintext bytes (the text ends, random bytes and random letters follow and the ratio drops), gets the CLEAR at the next ratio check, and after it runs through the widths 9, 10 and 11 again in the last 6500 or so bytes.

After a CLEAR the stream skips to the next multiple of 8 codes of the width in use, counted from the last width change. In the streams of `compress` and compress 4.0 those skipped bits are leftovers of the previous 8 codes, not zeros. ncompress writes zeros. With `-b 10` and `-b 11` ncompress writes the same codes as `compress` for these inputs and the two files differ only in those skipped bytes; with `-b 12` and up it also places the CLEAR differently, so only `nc-shift-b12.Z` is kept.

`shift-b9.Z` is special. With a maximum of 9 bits both `compress` and compress 4.0 switch to 10-bit codes once code 511 has been assigned, and stay there (no code above 511 is ever sent) until the CLEAR brings 9-bit codes back for the next 256 codes. So the stream has 256 codes of 9 bits, 10213 of 10 bits (the CLEAR is the last of them), 30 skipped bits, 256 codes of 9 bits and 2736 of 10 bits. The decoders of the same family read it that way; decoders that keep 9 bits fail on it (see below).

### Prefixes of `shift-b16.Z`

The first `take` bytes of `shift-b16.Z` are a complete stream of their own: for each row below, `compress -c` on the first `size` bytes of the plaintext writes exactly those bytes (compared for all 14). `at` ends with the last code of the old width; `+8` ends with the first 8 codes of the new width.

| Change to width | Codes before it | `take` at | `size` at | `take` +8 | `size` +8 |
|---|---|---|---|---|---|
| 10 | 256 | 291 | 428 | 301 | 448 |
| 11 | 768 | 931 | 2075 | 942 | 2108 |
| 12 | 1792 | 2339 | 7451 | 2351 | 7498 |
| 13 | 3840 | 5411 | 19457 | 5424 | 19508 |
| 14 | 7936 | 12067 | 44687 | 12081 | 44736 |
| 15 | 16128 | 26403 | 98238 | 26418 | 98294 |
| 16 | 32512 | 57123 | 222460 | 57139 | 222520 |

In block mode a width change always falls on a multiple of 8 codes, so no bits are skipped there. Skipped bits only occur after a CLEAR and in the streams without the block-mode bit.

### No block mode: `nb-*.Z`

Written by compress 4.0 with `-C` ("compatible with compress 2.0"): the flag byte has bit 7 clear, there is no CLEAR code, and 256 is the first code assigned to a string. The first run of 9-bit codes is therefore 257 codes long and 63 bits are skipped before the first 10-bit code.

| Fixture | Bytes | Made with | Plaintext | Covers |
|---|---|---|---|---|
| `nb-empty.Z` | 3 | `-C` | nothing | `1F 9D 10` |
| `nb-one.Z` | 5 | `-C` | `A` | `1F 9D 10 41 00` |
| `nb-aaa.Z` | 6 | `-C` | `aaa` | codes 97, 256: code 256 used as a string, and as KwKwK: `1F 9D 10 61 00 02` |
| `nb-run-a.Z` | 116 | `-C` | 5000 x `a` | 100 codes, 98 KwKwK, starting at 256 |
| `nb-text.Z` | 1477 | `-C` | `BIG[:4000]` | 1159 codes; 63 bits skipped at bit 2337 after 257 codes; width 11 after 769 codes with nothing skipped; code 256 is used 8 times |
| `nb-b10.Z` | 1498 | `-C -b 10` | `BIG[:4000]` | flag byte `0A`; table full after 769 codes, 446 more 10-bit codes |
| `nb-w10-at.Z` | 300 | `-C` | `BIG[:430]` | exactly 257 codes; the file ends with the 63 skipped bits, which hold leftovers of the previous 8 codes |
| `nb-w10-after.Z` | 302 | `-C` | `BIG[:432]` | 258 codes: one 10-bit code after the skipped bits |

### `files.tar.Z`

Five generated files, archived in the container with `tar --format=ustar --sort=name --owner=0 --group=0 --numeric-owner --mode=u=rwX,go=rX --mtime=@946684800 -cf files.tar readme.txt empty.dat data docs`, then `compress -c < files.tar` on the host. The archive also has the directory entries `data/`, `docs/` and `docs/notes/`.

| Member | Size | Content |
|---|---|---|
| `readme.txt` | 600 | `TEXT(0x00C0FFEE, 600)` |
| `empty.dat` | 0 | |
| `data/random.bin` | 2000 | `RAND(0x00C0FFEE, 2000)` |
| `data/table.bin` | 1024 | the bytes 0 to 255, four times |
| `docs/notes/nested.txt` | 1500 | `TEXT(0x0000BEEF, 1500)` |

## Edited streams

Byte offsets count from 0; values are hexadecimal.

| Fixture | Made from | Change | Result |
|---|---|---|---|
| `trail-zeros.Z` | `text.Z` | 16 zero bytes appended | the 3 unused bits and the new bytes hold eleven 11-bit codes 0: the plaintext plus 11 zero bytes, then 10 unused bits |
| `trail-text.Z` | `text.Z` | LF, `-- trailing garbage --`, LF appended (24 bytes) | five more codes decode to 15 more bytes; the code at bit 11804 is 1686 while the next free code is 1420 |
| `trail-ff.Z` | `w11-at.Z` | one byte `FF` appended | 8 bits where an 11-bit code would start |
| `trunc-group.Z` | `text.Z` | first 491 bytes | ends after 416 codes (256 of 9 bits, 160 of 10), on a multiple of 8 codes: identical to what `compress -c` writes for `BIG[:819]` |
| `trunc-midcode.Z` | `text.Z` | first 492 bytes | the same plus 8 bits of a 10-bit code |
| `trunc-midgroup.Z` | `text.Z` | first 497 bytes | four more whole codes (833 bytes of plaintext), then 8 bits of a 10-bit code |
| `shift-b10.Z`, first 8665 bytes | | manifest row only | ends 22 bits into the 70 skipped bits after the CLEAR |
| `trunc-header.Z` | | the two bytes `1F 9D` | no flag byte |
| `flag-20.Z`, `flag-40.Z` | `run-ab.Z` | byte 2: `90` to `B0`, `D0` | a reserved flag bit set; the codes are untouched |
| `bad-bits-8.Z`, `bad-bits-17.Z`, `bad-bits-0.Z`, `bad-bits-31.Z` | `run-ab.Z` | byte 2: `90` to `88`, `91`, `80`, `9F` | maximum width outside 9 ... 16 |
| `bad-first-code.Z` | `run-ab.Z` | byte 4: `C4` to `C5` | first code 353 instead of 97 |
| `bad-first-clear.Z` | `run-ab.Z` | byte 3: `61` to `00`; byte 4: `C4` to `C5` | first code 256 |
| `bad-code-beyond.Z` | `run-ab.Z` | byte 4: `C4` to `04`; byte 5: `04` to `06` | second code 258 instead of 98, while the next free code is 257 |
| `bad-code-late.Z` | `text.Z` | byte 1467: `C0` to `FC`; byte 1468: `01` to `1F` | last code (11 bits at bit 11738) 2047 instead of 112, while the next free code is 1414; 3999 bytes decode before it |
| `early-clear-aligned.Z` | first 291 bytes of `random-b16.Z` (256 literal codes) | byte 10: `B4` to `34`; byte 11: `5F` to `80` | code number 7 (counting from 0) becomes CLEAR, the last of its 8 codes, so nothing is skipped: plaintext bytes 0 ... 6 and 8 ... 255 |
| `early-clear-padded.Z` | the same 291 bytes | byte 6: `A8` to `00`; byte 7: `F5` to `F8` | code number 3 becomes CLEAR and codes 4 ... 7 are skipped: plaintext bytes 0 ... 2 and 8 ... 255 |
| `double-clear.Z` | `early-clear-aligned.Z` | byte 12: `39` to `00`; byte 13: `70` to `71` | code number 8 is a second CLEAR right after the first and codes 9 ... 15 are skipped: plaintext bytes 0 ... 6 and 16 ... 255 |
| `bad-after-clear.Z` | `early-clear-aligned.Z` | byte 12: `39` to `01`; byte 13: `70` to `71` | the code after the CLEAR is 257: a string code with no table and no previous code |

No producer writes a CLEAR before its table is full, so the three `*-clear*` streams with `expect` other than `reject` are hand-made valid streams, not program output.

## manifest.tsv

Columns `fixture`, `take`, `expect`, `size`, `fnv64`, `member`. A `-` means not applicable.

- `take`: use only the first `take` bytes of the fixture. `-` means the whole file.
- `size`, `fnv64`: length and FNV-1a 64 hash (hex) of the expected bytes.
- `member`: `-` for the decompressed stream itself; otherwise the path of a file inside the tar archive the stream holds, with `size` and `fnv64` of that file.

| `expect` | Meaning |
|---|---|
| `ok` | decodes to exactly `size` bytes with hash `fnv64`, with no error |
| `cut` | the data ends inside a code or inside skipped bits. The whole codes decode to exactly `size`/`fnv64`, and that is what a reader should return, no byte more; it may also report the truncation |
| `lenient` | existing decoders are split between decoding and refusing. A reader may refuse; if it returns data it must be exactly `size`/`fnv64` |
| `reject` | not a valid stream: a reader must fail or report it. Where `size` is given, it is the plaintext that decodes before the offending code, for readers that keep partial output |

## Decoders

Every fixture and every `take` prefix was run through ten decoders: compress 4.0 (`-d`), ncompress 5.0, GNU gzip 1.13, BusyBox 1.37.0 `uncompress`, libarchive 3.7.4 `bsdcat` and 7-Zip 25.01 in the container; macOS `uncompress` (`file_cmds-487.0.1`), Apple gzip 487.0.1, p7zip 17.05 and The Unarchiver's `unar` 1.10.7 on the host. The two 7-Zip versions behaved the same everywhere.

All ten give the manifest bytes with exit status 0 for: `one`, `aaa`, `run-a`, `run-ab`, `zeros`, `text`, `random-b16`, all `wN-*`, `shift-b12` ... `shift-b16`, `nc-shift-b12`, `files.tar`, `trunc-group`, `nb-one`, `nb-aaa`, `nb-run-a` and all 14 prefixes of `shift-b16.Z`. For every other `ok` row at least five agree. The exceptions:

| Fixtures | Who differs |
|---|---|
| `empty.Z`, `nb-empty.Z` | macOS `uncompress` and Apple gzip write nothing but exit with status 1 (gzip: "unexpected end of file") |
| flag byte with maximum width 9, 10 or 11: `shift-b9`, `shift-b10`, `shift-b11`, `random-b10`, `nb-b10` | macOS `uncompress` and Apple gzip refuse any maximum width below 12 ("Inappropriate file type or format"), including files macOS `compress` has just written |
| `shift-b9.Z` | 7-Zip and `unar` keep reading 9-bit codes after code 511 and fail (7-Zip after 979 bytes). compress 4.0, ncompress, GNU gzip, BusyBox and libarchive switch to 10 bits and reproduce the plaintext |
| `nb-text.Z`, `nb-b10.Z`, `nb-w10-at.Z`, `nb-w10-after.Z` | libarchive and `unar` fail ("Invalid compressed data", "Error on decrunching"); they handle the `nb-*` streams that stay below 257 codes |
| `early-clear-aligned.Z`, `early-clear-padded.Z` | libarchive fails; the other nine agree |
| `double-clear.Z` | libarchive fails; compress 4.0 takes the second 256 as a string code and writes 256 bytes of something else with status 0; the other eight agree |
| `trail-zeros.Z` | `unar` fails ("Attempted to read more data than was available"); the other nine write the 4011 bytes |

`cut` rows: ncompress, GNU gzip, BusyBox, libarchive and 7-Zip write exactly the manifest bytes with status 0 and no message for all four. `unar` fails on the three that end inside a code. For `trunc-midcode.Z`, compress 4.0, macOS `uncompress` and Apple gzip decode one more code out of the 8 leftover bits and bytes of the previous 8 codes still in their buffer, and write 6 wrong extra bytes with status 0; for `trail-ff.Z` compress 4.0 writes 3 extra bytes the same way, and macOS `uncompress` and Apple gzip fail. `trunc-midgroup.Z` is read correctly by all but `unar`, the cut `shift-b10.Z` by all that accept a maximum width of 10.

`lenient` rows `flag-20.Z`, `flag-40.Z`: compress 4.0, ncompress, BusyBox, `unar`, macOS `uncompress` and Apple gzip ignore the bit; GNU gzip decodes and exits with status 2 (warning); 7-Zip refuses; libarchive does not take the file for `compress` data and passes its 177 bytes through.

`reject` rows:

| Fixture | Who does not fail |
|---|---|
| `trunc-header.Z` | all fail (libarchive passes the 2 bytes through) |
| `bad-bits-8.Z`, `bad-bits-0.Z` | compress 4.0 writes 457 bytes of garbage with status 0 |
| `bad-bits-17.Z` | `unar` ignores the value and decodes the 6000 bytes |
| `bad-bits-31.Z` | all fail |
| `bad-first-code.Z`, `bad-first-clear.Z` | compress 4.0 and macOS `uncompress` write garbage (7392 and 6076 bytes) with status 0 |
| `bad-code-beyond.Z` | all fail |
| `bad-code-late.Z`, `bad-after-clear.Z`, `trail-text.Z` | compress 4.0 writes garbage with status 0 (4003, 256, 4058 bytes) |

Where a decoder writes output before failing, GNU gzip and 7-Zip stop exactly at the manifest's `size` (`bad-code-late.Z`, `trail-text.Z`; GNU gzip also for `bad-code-beyond.Z` and `bad-after-clear.Z`); BusyBox and macOS `uncompress` write only whole buffers (2048 or 3072 bytes); ncompress, libarchive and `unar` write nothing.

## Not covered

- A stream of zero bytes, which macOS `compress` writes for empty input: it has no magic number.
- ncompress 5.0 with `-b 9`: it keeps 9-bit codes after the table is full, which no decoder of its own family reads. Its output for `RAND(…, 3000)` decodes in 7-Zip and `unar` only; its output for 150 KB of text decodes to the wrong bytes there and fails everywhere else, ncompress included. Not a usable producer.
- ncompress 4.2.4.6 with `-C`: it clears the block-mode bit in the header but writes the same codes as without `-C` (first string code 257, CLEAR codes), and its own decoder rejects the result. The `nb-*` streams come from compress 4.0 instead.
- Streams longer than the first CLEAR by more than one table generation at 13 bits and up, more than one CLEAR in a file, inputs above 8 MiB (where the producers change how they compute the ratio), and concatenated streams.

## Sources consulted while building the fixtures

Read to understand why producers and decoders differ; nothing from them is in this directory.

| Source | Licence |
|---|---|
| `compress(1)` manual page, macOS 27 | BSD |
| FreeBSD `usr.bin/compress/zopen.c`, https://github.com/freebsd/freebsd-src | BSD-3-Clause |
| ncompress 5.0 `compress.c` and ncompress 4.2.4.6 `compress42.c`, https://github.com/vapier/ncompress | public domain (Unlicense) |
| 4.3BSD `usr/src/ucb/compress/compress.c` (compress 4.0), https://github.com/dspinellis/unix-history-repo, branch `BSD-4_3` | BSD (Caldera/UC Berkeley ancient-Unix terms) |
