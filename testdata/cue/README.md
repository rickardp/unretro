# CUE/BIN fixtures

Everything here is generated: the file systems were made by `genisoimage` and `mformat` from generated text and counter files, the audio by `sox` (sine tones), and nothing comes from a real disc. The sheets were typed by hand (through a throwaway shell script that is deliberately not part of this repository). Made in a Debian 13.7 container (arm64) with genisoimage 1.1.11 (package 9:1.1.11-4), GNU mtools 4.0.48, SoX 14.4.2, GNU tar 1.35, gzip 1.13 and Python 3.13.5.

No packaged tool turns a 2048-byte image into raw 2352-byte sectors, so a throwaway Python script did that step: 12 sync bytes, the BCD minute/second/frame address (absolute sector + 150) and the mode byte, the user data, and the EDC and P/Q parity of ECMA-130 (for Mode 2 Form 1 an 8-byte subheader `00 00 08 00` twice, `00 00 89 00` twice on the last sector of the file, and parity computed over a zeroed address). `edccchk` 1.27 and `bin2ecm` (both built from source in the container) accept every data sector written this way with no error or warning, and three independent converters give back the 2048-byte images that went in (see Verification).

`manifest.tsv`, `files.tsv` and `broken.tsv` hold what a reader has to produce.

## Layout

A track file has to be found as a sibling of its sheet. Loose in this directory are one uncompressed pair and the sheets that share its image; the rest is in `discs.tar.gz` (65322 bytes, 2.6 MiB unpacked), so the sheets there are reached by nested traversal and their siblings are entries of the tar archive. Total size of the directory: under 200 KB.

All ISO 9660 volumes are level 1 without Rock Ridge or Joliet, 30 sectors (`genisoimage -no-pad`), and hold `README.TXT`, `DATA/COUNTER.BIN` (3000 bytes) and `DATA/DEEP/NESTED.TXT`; label and file contents differ per volume. A CD sector is 1/75 second, so `00:02:30` is sector 180.

### Loose files

| Sheet | Track file(s) | Exercises |
|---|---|---|
| `mode1-2048.cue` | `mode1-2048.iso` (61440 bytes) | one `MODE1/2048` track; unquoted FILE name |
| `case.cue` | the same file, named `MODE1-2048.ISO` in the sheet | FILE name differs in case from the real file (a case-insensitive host file system hides the difference; `CaseDiff.cue` in the archive does not depend on the host) |
| `wave.cue` | `mode1-2048.iso` `BINARY`, then `tone.wav` `WAVE` | track 2 is an AUDIO track in a RIFF/WAVE file (4 sectors of 44.1 kHz 16-bit stereo, 44-byte header): unsupported; track 1 is an ordinary data track |
| `bad-*.cue` | | broken sheets, see `broken.tsv` |

### `discs.tar.gz`

| Sheet | Track file(s) | Exercises |
|---|---|---|
| `mode1-2352.cue` | `mode1-2352.bin` (30 raw sectors) | one `MODE1/2352` track |
| `CaseDiff.cue` | the same file, named `MODE1-2352.BIN` in the sheet | FILE name differs in case from the archive member |
| `mode2-2352.cue` | `mode2-2352.bin` (30 raw sectors) | one `MODE2/2352` track, every sector XA Form 1 (user data at byte 24) |
| `mixed.cue` | `mixed.bin` (220 raw sectors) | `MODE1/2352` track, then two AUDIO tracks. Track 2 has `INDEX 00` at sector 30, `INDEX 01` at 180 and `INDEX 02` at 190; the 150 sectors between index 0 and 1 are in the file (digital silence). Track 3 has `PREGAP 00:02:00` and `INDEX 01` at 200; that gap is not in the file. Also `REM`, `CATALOG`, `PERFORMER`, `TITLE`, `FLAGS DCP`, `ISRC` |
| `multifile.cue` | `Multi File (Track 1).bin` (30 raw sectors), `Multi File (Track 2).bin`, `Multi File (Track 3).bin` (170 sectors each) | one FILE per track, the layout of Redump-style dumps: each audio file starts with its own pregap (`INDEX 00 00:00:00`, `INDEX 01 00:02:00`), 150 sectors of silence |
| `twodata.cue` | `twodata.bin` (260 raw sectors) | two `MODE1/2352` data tracks as on a BeOS install disc: ISO 9660 in track 1, another file system in track 2 (here FAT12, a 160 KiB floppy layout of 80 CD sectors, label `SECONDTRACK`, files `HELLO.TXT` and `DIR/INNER.TXT`). Track 2 has `INDEX 00` at sector 30 and `INDEX 01` at 180; the 150 gap sectors are well-formed empty Mode 1 sectors. Sector headers carry the position in the file, so the first sector of the FAT volume is addressed 00:04:30 |
| `twodata-2048.cue` | `twodata-a.iso`, `twodata-b.img` | the same two volumes as cooked `MODE1/2048` tracks, one FILE each; the tracks are byte-identical to those of `twodata.cue` |
| `motorola.cue` | `motorola.bin` (40 sectors) | `FILE ... MOTOROLA`: two AUDIO tracks whose samples are stored big-endian |
| `Odd Sheet.cue` | `My Disc Image.bin` (30 raw Mode 1 sectors + 10 audio sectors) | UTF-8 byte order mark, CRLF line ends, no line end after the last line, commands in lower case, tab indentation, trailing blanks, an empty line, quoted names with spaces, a non-ASCII title, and three REM lines: `rem GENRE ...`, `REM COMMENT "FILE \"decoy.bin\" BINARY"` and an indented `rem TRACK 09 AUDIO`, none of which may be taken for a command |
| `sub/deep.cue` | `sub/deep.bin` (4 audio sectors) | sheet in a subdirectory; a different `deep.bin` of the same size lies at the top of the archive and must not be used |
| `sub/bad-escape.cue` | | broken, see `broken.tsv` |

Tones (left/right, Hz): `mixed` 441/882 and 1470/2205; `multifile` 630/1260 and 2940/3675; `motorola` 441/1323 and 2205/4410; `Odd Sheet` 735/1470; `sub/deep.bin` 980/1960; the decoy `deep.bin` 1225/2450; `tone.wav` 441/1764.

## Commands

```
genisoimage -quiet -no-pad -iso-level 1 -V <LABEL> -o <name>.iso <tree>
mformat -i twodata-b.img -C -f 160 -v SECONDTRACK -N 12345678 ::
mcopy -i twodata-b.img HELLO.TXT ::HELLO.TXT;  mmd -i twodata-b.img ::DIR;  mcopy -i twodata-b.img INNER.TXT ::DIR/INNER.TXT
sox -D -r 44100 -c 2 -n -b 16 -e signed-integer -L -t raw <out> synth <sectors*588>s sine <left> sine <right> gain -6     (-B for motorola.bin)
sox -D -r 44100 -c 2 -n -b 16 -e signed-integer tone.wav synth 2352s sine 441 sine 1764 gain -6
tar --sort=name --owner=0 --group=0 --numeric-owner --mtime='2026-01-01 00:00:00Z' --format=ustar -cf discs.tar -T <sorted list>
gzip -9 -n discs.tar
```

Images of several tracks are plain concatenations (`cat`) of the wrapped data track, zero-filled or empty-sector gaps and the `sox` output. `genisoimage` stamps the current time into the volume, so a rebuild gives different ISO bytes and hashes.

## Verification

The sizes and hashes in `manifest.tsv` are those of files written by converters that read the sheet themselves, not values computed by the generator. The manifest was only written after all results for a track were identical and equal to the `genisoimage`, `mformat` or `sox` output that went in.

| Tool | Used for |
|---|---|
| `bchunk` 1.2.2 (`bchunk <bin> <cue> <base>`, `-s` for `motorola`) | every 2352-byte track: cooked data (`.iso`) or audio (`.cdr`) per track |
| `iat` 0.1.7 (`iat -i <bin> -o <iso> --iso`) | `mode1-2352.bin`, `mode2-2352.bin`, `Multi File (Track 1).bin` |
| `cd-read` (libcdio-utils 2.2.0; `-c <cue> -m m1f1|m2f1|audio -s <lsn> -n <count>`) | `mode1-2352`, `mode2-2352`, `mixed`, `twodata` |
| `cdrdao show-toc` 1.2.5 | track modes, start sectors, lengths and gaps of every valid sheet |
| `edccchk` 1.27 | EDC and ECC of all raw data sectors: 0 errors, 0 warnings |
| `isoinfo -f`/`-x` (genisoimage), `mdir`/`mcopy` (mtools) on the `bchunk` output | `files.tsv` |

Where a tool could not read a sheet as it stands, it was given a reduced one:

- `bchunk` takes one image, so each FILE block of `multifile.cue` was fed on its own.
- `bchunk` starts a track at its last INDEX, and `cdrdao` rejects the `INDEX 02` line of this short track; both were given `mixed.cue` without that line. With it, `bchunk` gives the same tracks 1 and 3.
- `bchunk` reads `Odd Sheet.cue` as one AUDIO track 9 (it matches the `rem TRACK 09 AUDIO` line and no lower-case command), `cdrdao` stops at line 1 (`Unknown command`, the byte order mark) and `cd-info` does not recognise it as a cue sheet. A copy without byte order mark, carriage returns and REM lines, with the commands in upper case, was used instead.
- `bchunk` does not know `MODE1/2048`. Such a track is the whole file; `cdrdao` confirms mode and length (30 and 80 sectors) and the hash is that of the file, which for `twodata-2048.cue` equals what `bchunk` extracted from `twodata.bin`.
- `cd-read` returns `motorola.bin` unswapped, so it does not count for that sheet; `sox` converting the big-endian source to little-endian agrees with `bchunk -s`.

`cdrdao` places track 3 of `mixed.cue` at disc sector 350 for file sector 200, track 2 at 180: a `PREGAP` command moves the following disc addresses by its length and consumes nothing from the file, an `INDEX 00` gap is file content.

## manifest.tsv

One row per track of every valid sheet.

| Column | Meaning |
|---|---|
| `container` | `-` for a sheet in this directory, `discs.tar.gz` for a member of the archive |
| `sheet`, `track` | sheet path and track number |
| `mode` | track mode as the sheet gives it, in upper case |
| `file`, `file_type` | the track file by its real name (not the spelling in `case.cue` and `CaseDiff.cue`) and the type on the FILE line |
| `start`, `sectors` | sector of `INDEX 01` in the file (sector size 2048 or 2352 by mode) and the length of the expected entry in sectors |
| `gap` | sectors between `INDEX 00` and `INDEX 01`, present in the file before `start` |
| `pregap` | sectors of a `PREGAP` command, absent from the file |
| `size`, `fnv64`, `sha256` | expected entry: bytes, FNV-1a 64 and SHA-256 |
| `content` | `iso9660:<label>` or `fat12:<label>` for a file system track, `pcm` for audio, `unsupported` for the WAVE track (no size or hash) |

The expected entry of a track runs from its `INDEX 01` to the first index of the next track in the same file (its `INDEX 00` if it has one), or to the end of the file. The gap before `INDEX 01` belongs to no entry. This is what `bchunk` writes. For a data track the entry is the 2048 user bytes of each sector; for an audio track it is the 2352 bytes of each sector as little-endian 16-bit stereo samples, so for `motorola.cue` every byte pair is swapped relative to the file.

For reference, the samples in `tone.wav` are 9408 bytes with FNV-1a 64 `f8a573a0f33e1a1c`.

## files.tsv

Columns `container`, `sheet`, `track`, `path`, `size`, `fnv64`: the files inside each file system track, extracted with `isoinfo` and `mcopy` from the tracks `bchunk` wrote. `path` is in lower case without the `;1` version, as the ISO 9660 and FAT readers of this crate present names; on the volumes the names are upper case (`README.TXT;1`).

## broken.tsv

Columns `container`, `sheet`, `defect`. None of these sheets may yield a track from the named file, and none may read outside the directory (or archive directory) the sheet is in. `bad-dotdot.cue`, `bad-dotdot-backslash.cue` and `sub/bad-escape.cue` name files that exist, so a reader that resolves the path opens them. `bad-backwards-track.cue`, `bad-backwards-index.cue` and `bad-index-beyond-eof.cue` use `mode1-2048.iso` (30 sectors); in each, track 1 on its own is well-formed. Existing tools are no model here: `cdrdao` 1.2.5 opens `/etc/hosts`, `../cue/mode1-2048.iso` and `../deep.bin` without complaint, and `bchunk` writes output for every one of these sheets. `cdrdao` does reject the index beyond the end of the file and the missing file, and prints no table of contents for the two backwards sheets.

## Sources

| Source | Licence |
|---|---|
| ECMA-130, Data Interchange on Read-only 120 mm Optical Data Disks (sector layout, EDC, P/Q parity), https://ecma-international.org/publications-and-standards/standards/ecma-130/ | Ecma standard, free to read |
| Cue sheet syntax as described in the CDRWIN user guide (appendix A, cue sheet commands) and summarised at https://wiki.hydrogenaud.io/index.php?title=Cue_sheet | documentation |
| bchunk, https://github.com/hessu/bchunk (run only) | GPL-2.0-or-later |
| cdrdao, https://github.com/cdrdao/cdrdao (run only) | GPL-2.0-or-later |
| GNU libcdio utilities `cd-info`, `cd-read`, https://www.gnu.org/software/libcdio/ (run only) | GPL-3.0-or-later |
| iat 0.1.7, Debian package `iat` (run only) | GPL |
| edccchk 1.27, https://github.com/claunia/edccchk, commit 2c59f19 (built and run) | GPL-3.0 |
| ecm-tools, https://github.com/alucryd/ecm-tools, commit 2765930 (built and run) | GPL-3.0 |
| cdrkit genisoimage/isoinfo, GNU mtools, SoX, GNU tar, gzip (run only) | GPL |
