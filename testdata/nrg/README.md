# Nero NRG fixtures

Everything here is generated. `make_nrg.py` (Python 3 standard library only) writes every image from `docs/nrg/nrg.md` alone, independently of the reader in `src/formats/dos/nrg.rs`; no NRG tool source was read and nothing comes from a real disc. The file systems inside the data tracks were made by `xorriso` and `hdiutil makehybrid` from generated text and counter files and are kept in `volumes.tar.gz`; the audio is sine tones whose periods are whole numbers of samples; tracks without a file system hold numbered text sectors.

Made on macOS 27.0.1 (26A434, arm64) with GNU xorriso 1.5.8.pl02, `hdiutil` and Python 3.14.4. Verified with 7-Zip 26.01, xorriso, and in a Debian 13.7 container (arm64) with `cd-info`/`cd-read` (libcdio-utils 2.2.0-4.1), `isoinfo` (genisoimage 1.1.11-4), `iat` 0.1.7 and `nrg2iso` 0.4. No image was mounted or attached at any point.

`manifest.tsv`, `files.tsv` and `broken.tsv` hold what a reader has to produce. The directory is about 250 KB; the images are 14.5 MiB unpacked.

## Layout

| File | Contents |
|---|---|
| `<name>.nrg.gz` | one well-formed image each (`gzip -9`, no name, no time stamp), 29 images |
| `broken.tar.gz` | 52 images `bad-*.nrg` that are not NRG images or have tracks a reader must leave out |
| `volumes.tar.gz` | the 17 file systems the generator wraps (input, not a fixture) |
| `make_nrg.py`, `make_volumes.sh` | generator and the commands that made `volumes.tar.gz` |

Every image is reached by nested traversal: `<name>.nrg.gz` holds `<name>.nrg`, which lists `trackNN.img` and `trackNN.pcm`.

## Good images

DAO = `CUEX`/`DAOX` (version 1: `CUES`/`DAOI`), TAO = `ETN2` (version 1: `ETNF`). Unless noted, the first track of a DAO image has a stored pregap of 150 sectors and the chain is the common `CUEX`, `DAOX`, `SINF`, `MTYP`, `END!`.

| Image | Footer | Session(s) | Exercises |
|---|---|---|---|
| `tao-v2` | `NER5` | TAO | one cooked 2048 track (mode `00`), ISO 9660; the smallest chain, `ETN2`, `END!`. The file is also an ISO 9660 image with 60 bytes appended (section 3.2, order of tests) |
| `tao-v2-mixed` | `NER5` | TAO | ISO 9660 track and two audio tracks; `ETN2` sector counts zero; `MTYP` |
| `tao-v1` | `NERO` | TAO | `ETNF`: ISO 9660 track and an audio track; `SINF` |
| `tao-modes` | `NER5` | TAO | modes `02`, `03`, `05`, `06`, `0F`, `10`, `11`, `08` in a TAO chunk (sizes from the mode table); `RELO`, `TOCT`, `SINF`, `DINF`, `MTYP` |
| `tinf` | `NERO` | TAO | `TINF` with 12-byte entries, data and audio |
| `tao-clamp` | `NER5` | TAO | one audio track whose stated length is ten times the data area, which ends with a partial sector: 6 whole sectors |
| `dao-v2` | `NER5` | DAO | one cooked 2048 track, ISO 9660 at offset 307,200 |
| `dao-v1` | `NERO` | DAO | `CUES` (minute, second, frame in BCD, as in image D of the specification) and `DAOI` with 30-byte entries: ISO 9660 track and an audio track |
| `dao-raw-mode1` | `NER5` | DAO | mode `05`: raw 2352 Mode 1 sectors with EDC and P/Q parity, ISO 9660; the pregap is 150 well-formed empty sectors addressed from 00:00:00 |
| `dao-xa` | `NER5` | DAO, type `20` | three ISO 9660 volumes: mode `03` (2336: sub-header, data, EDC, parity), mode `02` (cooked 2048), mode `06` (raw 2352 Mode 2/XA Form 1). Tracks 2 and 3 have stored pregaps of 150 sectors of their own kind |
| `mixed` | `NER5` | DAO | ISO 9660 track, then three audio tracks: track 2 with 150 pregap sectors of silence stored in the file, track 3 without pregap, track 4 with 75 pregap sectors holding a different tone (not part of any entry). Catalogue number, ISRC codes, `CDTX` |
| `audio` | `NER5` | DAO | two audio tracks; `AFNM` and `VOLM` after `MTYP` |
| `multisession` | `NER5` | DAO, DAO | two data sessions; the second ISO 9660 session keeps three files of the first. Chain `CUEX`, `DAOX`, `CUEX`, `DAOX`, `SINF`, `SINF`, `MTYP`, `END!`. See Multi-session |
| `cd-extra` | `NER5` | DAO, DAO | two audio tracks, then a data session of mode `02` whose ISO 9660 volume is self-contained but uses disc addresses (index 1 at LBA 11,422) |
| `hfs` | `NER5` | TAO | Apple partition map with one HFS+ volume (`hdiutil makehybrid -hfs`) |
| `hybrid` | `NER5` | DAO | ISO 9660 and HFS+ hybrid (`hdiutil makehybrid -hfs -iso`) |
| `dvd` | `NER5` | DAO, type `40` | one track with *index 0* = *index 1* = 0, cue points from LBA 0, `MTYP` `1C` |
| `gaps` | `NER5` | DAO | section 7.2: data track of mode `02`, 660,000 zero bytes in no track, audio track, 352,800 bytes in no track, audio track; audio tracks have *index 0* = *index 1*; `CUEX` counts the gap sectors to the track before |
| `subchannel` | `NER5` | DAO | 2448-byte sectors: mode `0F` (data), `10` (audio), `11` (data); the 96 sub-channel bytes hold `00`, `40` and `80` and are never all zero |
| `clamp` | `NER5` | DAO | track 1's range ends with 1000 bytes of a sixth sector (5 sectors expected); track 2's end is `7FFFFFFFFFFFFFFF` and the data area ends with 700 bytes of a fifth sector (4 expected) |
| `mismatch` | `NER5` | DAO | 15 tracks whose sector size field, mode code and sectors disagree (section 6.1 "Which field decides", 6.3): see below |
| `families` | `NERO` | DAO, TAO, TAO, TAO, DAO | `DAOX` (mode `08`), `ETN2`, `ETNF`, `TINF` and `DAOI` in one chain under a version 1 footer; five tracks numbered through |
| `v2-then-v1` | `NERO` | TAO | the four bytes before the version 1 footer are `NER5`, so the version 2 test finds its signature, fails on the offset, and the version 1 test must still be made. Those four bytes lie between `END!` and the footer |
| `unknown-chunks` | `NER5` | DAO | `ZZZZ` (13 bytes), four spaces, `DECO` (holds `END!`, a footer and a copy of the `DAOX` content), `~~~~`, `nero`, `end!`, `END?`, `VOLM`, `AFNM`, `DAOY` (a copy of the `DAOX` content); `DAOX` before its `CUEX` |
| `chunks-256` | `NER5` | DAO | 251 empty `JUNK` chunks: exactly 256 chunks, `END!` included |
| `chain-1mib` | `NER5` | TAO | a `PADD` chunk makes the chain exactly 1,048,576 bytes |
| `lying-fields` | `NER5` | DAO, TAO | every ignored field is wrong: repeated length `FFFFFFFF`, first/last track 9 and 3, session type `10`, catalogue number `../../etc/pwd`, ISRC `/etc/passwd`, entry bytes 15 to 17 `FF`, a `CUEX` of 21 garbage bytes, 41 and 31 surplus bytes after the track entries, `ETN2` LBA and sector count `FFFFFFFF`, `SINF` 99 and an empty `SINF`, `MTYP` `FFFFFFFF`, `END!` with 4 bytes of content, 5 bytes between `END!` and the footer |
| `tracks-99` | `NER5` | TAO | 99 tracks of one sector, data and audio alternating |
| `nested` | `NER5` | DAO | the data track is itself an NRG image (TAO, data and audio track, padded to whole sectors by a `PADD` chunk) |

`mismatch`, 3 sectors per track:

| Track | Mode code | Size field | Sectors are | User data at |
|---|---|---|---|---|
| 1, 2 | `00`, `03` | 2352 | raw Mode 1, raw Mode 2 (the layout ascribed to MagicISO) | 16, 24 by the sector's mode byte |
| 3, 4 | `05`, `06` | 2352 | raw Mode 2, raw Mode 1 (code wrong) | 24, 16 by the sector's mode byte |
| 5, 6, 12, 13 | `05`, `06`, `00`, `02` | 2352 | raw Mode 1, 2, 1, 2 with the first sync byte `01` | 16, 24, 16, 24 by the code |
| 7, 8 | `02`, `00` | 2336 | sub-header and Form 1 body | 8 |
| 9 | `03` | 2048 | user data | 0 |
| 10, 11 | `07`, `10` | 2448, 2352 | audio with and without sub-channel bytes | |
| 14, 15 | `05`, `11` | 2352, 2448 | sync pattern present, mode byte `00` and `03` | 16, 24 by the code |

Tones (left/right period in samples at 44,100 Hz) differ per track; see `make_nrg.py`.

### Multi-session

Session 1 of `multisession` (`NRG_MS1`, 64 sectors, Rock Ridge) holds `readme.txt`, `keep/stay.bin`, `keep/also.txt`, `docs/changed.txt`, `docs/removed.txt`. Session 2 (`NRG_MS2`, 32 sectors) replaces `docs/changed.txt`, removes `docs/removed.txt` and adds `added/new.txt` and `second.bin`. Its index 1 is at LBA 64 + 11,250 + 150 = 11,464 (the `CUEX` chunks say so; the 11,250 sectors are not in the file). Its descriptors and directories use disc addresses: the new files are at sectors 11,490 to 11,492, the kept ones at 59 to 62, inside track 1.

`files.tsv` lists the session 2 tree with a `where` column: `track` for files whose data is in the track, `earlier` for the three files that lie in track 1. A reader that hands the track out on its own (section 10.7) can give the `track` files at most; nothing listed below `track02.img` may have other content than the row says. The same holds for `cd-extra`, where all files are `track`.

## Broken images

`broken.tsv` has one row per member of `broken.tar.gz`. `outcome` is one of:

| Outcome | Meaning | Count |
|---|---|---|
| `not-nrg` | the recognition test of section 3.2 fails: the file is not an NRG image and no `trackNN` entry may appear | 33 |
| `partial` | recognised; the tracks in `entries` are listed (rows in `manifest.tsv`), `reported` tracks are left out and reported | 10 |
| `no-entries` | recognised, opens, lists nothing; `reported` tracks are reported | 9 |

`reasons` names why tracks are left out: `mode`, `sector-size`, `beyond` (starts at or beyond the chain offset), `empty` (no whole sector), `layout` (inconsistent layout, section 8.2: every track is reported). `rule` cites the sections of `nrg.md`; `16.n` is safety requirement *n*.

All but a few are changes to one three-track image (data mode `00` with 2 pregap sectors, audio with 1, data mode `03`; 20 KB) or to a two-track `ETN2` image, so that each has one defect: truncation at three places, files of 0, 8, 12 and 20 bytes, bytes after the footer, nine wrong chain offsets, two wrong signatures, a chain without `END!`, `END!` and other chunks reaching into or beyond the footer, identifiers that are not printable, 257 chunks, no track chunk, track chunks too short for an entry, a chain of 1 MiB + 1, 100 tracks in one and in two chunks; tracks beyond the data area, sector sizes 0, 1, 2324, 2340 and 65535, an audio track of 2048-byte sectors, unknown modes (also a 32-bit TAO mode with high bits), empty and short tracks; overlapping, reversed and exchanged tracks, a `DAOX` chunk repeated, 99 tracks over the same four sectors, and an `ETN2` entry whose offset plus length overflows 64 bits.

The data areas of the `not-nrg` images begin with zero bytes or plain text, so no other detector should claim them.

## Generation

```
sh make_volumes.sh <empty work directory>     # xorriso, hdiutil makehybrid -> volumes.tar.gz
python3 make_nrg.py build                     # images, broken.tar.gz, *.tsv
python3 make_nrg.py check                     # rebuild in memory, compare with the files here
python3 make_nrg.py spec                      # images A to D of nrg.md section 12 against its digests
python3 make_nrg.py unpack <directory>        # plain images and expected entries, for other tools
```

`build` is deterministic for a given `volumes.tar.gz`. `check` compares unpacked content, because the deflate stream depends on the zlib in use. `make_volumes.sh` is not reproducible byte for byte: xorriso and hdiutil stamp the current time into the volumes, so rebuilding them changes the hashes of every file system track (the file rows stay the same).

Volumes: `vol01` to `vol12` by `xorriso -as mkisofs --norock -iso-level 1 -no-pad` (37 sectors; `README.TXT`, `DATA/COUNTER.BIN`, `DATA/DEEP/NESTED.TXT`); `extra` the same with `-C 0,11422`; `ms1` and `ms2` by native `xorriso` with `-grow_blindly 11464` as in `testdata/iso`; `hfs` and `hybrid` by `hdiutil makehybrid -o <file>` (450 sectors each).

Raw sectors are written by the generator: sync, BCD address (LBA + 150), mode, user data, EDC and P/Q parity of ECMA-130; for Mode 2 Form 1 the sub-header `00 00 08 00` twice (`00 00 89 00` on a track's last sector) and parity over a zeroed address.

## Verification

Expected sizes and hashes are computed from what was fed in (the volume, tone or text sectors of each track, and the source files of each volume), not by reading the images back.

| Check | Result |
|---|---|
| `make_nrg.py spec`: images A, B, C, D of `nrg.md` section 12 rebuilt with the generator's chunk and footer writers | all four SHA-256 digests match the specification |
| Raw sector writer against `mode1-2352.bin` and `mode2-2352.bin` of `testdata/cue/discs.tar.gz`, which `edccchk` accepted | all 60 sectors identical |
| `7zz x` on each expected file system track | every `files.tsv` row of 15 tracks found with the listed size and hash (ISO 9660 names in upper case); `hfs` also, with the volume name as top directory |
| `7zz l` on `tao-v2.nrg`, `dvd.nrg`, `dao-v2.nrg` themselves | ISO 9660 with a tail of 60 or 156 bytes |
| `xorriso -load sbsector 11464` on tracks 1 and 2 of `multisession` placed at their disc addresses; `isoinfo -N 11464 -R` on track 2 alone | the session 2 tree with the listed content; `report_lba` gives the extents quoted above |
| `isoinfo -N 11422` on track 3 of `cd-extra` | lists the three files |
| `nrg2iso` (skips 307,200 bytes) | output starts with the expected `track01.img` for `dao-v2`, `dao-v1`, `hybrid`, `mixed`, `gaps`, `nested`, `multisession`, `unknown-chunks`, `chunks-256` |
| `iat --iso` | `dao-raw-mode1`: exactly the expected track; `dao-xa`, `dao-v2`, `dao-v1`, `hybrid`, `mixed`, `gaps`, `multisession`: output starts with the expected track 1 |
| `cd-read -m audio` | `audio` tracks 1 and 2 and `tao-clamp` identical; `dao-raw-mode1`: the raw sectors, whose user data is the expected track |
| `cd-read -m m2f1` | `dao-xa` track 1 identical |

`cd-info -N` lists the expected number of tracks at the expected LBAs for `audio`, `dao-v2`, `dao-xa`, `gaps`, `hfs`, `hybrid`, `nested`, `subchannel`, `tao-v2`, `tao-clamp`, `unknown-chunks` and `chunks-256`. Where it differs it shows the gaps `nrg.md` section 13 records, and three more:

- It aborts (`lsn == -150`) on every image whose first cue point is not at −150: `dvd`, `clamp`, `mismatch`, `families`, `lying-fields`, `multisession`, `cd-extra`, and most broken images. It aborts on `tao-v1` and `v2-then-v1` (`_start * blocksize == _start2`), on `chain-1mib` (`size - footer_start <= 4096`) and on `mixed` (`double free or corruption` after reading the invented `CDTX` packs).
- `dao-v1`: track 2 at sector 567, the BCD `00 02 37` read as a number. `dao-raw-mode1` and `subchannel`: tracks listed as audio ("Unknown track format"). `tinf` and `tao-modes`: not opened.
- New: it warns that an `ETN2` LBA times the sector size is not the offset (`tao-v2-mixed`, `tracks-99`), that is, it expects the field to count stored sectors, where the specification's real image has disc addresses; the field is ignored either way.
- New: `cd-read -m m1f1` reads data sectors of a cooked 2048 track at (LBA + 150) × 2336 in a DAO image and 16 bytes late in a TAO image, on image A of the specification as well, and `cd-info` reports "unknown filesystem" for every image here. libcdio is therefore no witness for cooked 2048 tracks; `nrg2iso`, `iat` and `7zz` are.
- New: it crashes (signal 11) on `bad-tracks-100.nrg`.

A scratch reading of the fixtures by a second script that follows sections 3, 4, 6.3 and 8 of `nrg.md` gives the entries of `manifest.tsv` and the outcomes of `broken.tsv` for all 81 images. It has the same author as the generator and is no independent witness; it is not part of the repository.

## manifest.tsv

One row per expected entry, for the good images and for the surviving tracks of `partial` images.

| Column | Meaning |
|---|---|
| `archive` | `-` for `<image>.gz` in this directory, `broken.tar.gz` for a member of that archive |
| `image`, `track`, `entry` | image name, track number by position, entry name (`trackNN.img` or `trackNN.pcm`) |
| `chunk` | the track chunk the entry comes from |
| `mode`, `sector_size` | mode code (hexadecimal) and stored sector size as the chain gives them (for TAO, from the mode table) |
| `index0`, `index1`, `end` | byte offsets as the chain gives them (TAO: offset, offset, offset + length); `end` may lie beyond the data area |
| `lba` | disc address of index 1 |
| `sectors`, `size`, `fnv64` | expected entry: whole sectors, bytes (2048 per sector for `.img`, 2352 for `.pcm`), FNV-1a 64 |
| `content` | `iso9660:<label>`, `iso9660-later-session:<label>` (addresses are disc addresses, see Multi-session), `apm+hfsplus:<label>`, `iso9660+apm+hfsplus:<label>`, `nrg`, `pcm`, or `data` (no file system) |

## files.tsv

Columns `archive`, `image`, `track`, `path`, `size`, `fnv64`, `where`: the files inside each file system track, from the source files. ISO 9660 level 1 names are in lower case without version, as the reader of this crate presents them; Rock Ridge names (`multisession`) as recorded; `hfs` and `hybrid` in the source spelling (`Read Me`, `README.TXT`), where the ISO 9660 side of the hybrid has upper-case plain names without `NM` entries and an HFS+ reader may add the volume name as a directory. For `nested`, the rows are the two tracks of the inner image.

## broken.tsv

Columns `archive`, `image`, `outcome`, `entries` (expected entry names, comma-separated, or `-`), `reported` (number of tracks left out), `reasons`, `rule`, `defect`.

## Not covered

- Offsets above 4 GiB, output budgets and size limits (section 16, requirements 9 and 10).
- Form 2 sectors and Mode 2 without sub-header.
- A `CUES` chunk in the 32-bit LBA reading; the chunk is skipped.
- Audio stored big-endian; no such NRG image is described.

## Sources

| Source | Use |
|---|---|
| `docs/nrg/nrg.md`, `docs/nrg/README.md` | every NRG structure |
| ECMA-130 (sector layout, EDC, P/Q parity) as used for `testdata/cue` | raw sectors |
| `testdata/cue`, `testdata/iso`, `testdata/udf` READMEs and manifests | conventions, the multi-session commands |
| xorriso, `hdiutil makehybrid`, 7-Zip, libcdio `cd-info`/`cd-read`, genisoimage `isoinfo`, `iat`, `nrg2iso` (run only) | making and checking |
