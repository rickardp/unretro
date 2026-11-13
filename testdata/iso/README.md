# ISO 9660 extension fixtures: El Torito, multi-session, High Sierra

The El Torito and multi-session images were written by `hdiutil makehybrid` and `xorriso`; the FAT images inside them were formatted by `newfs_msdos` or `hdiutil create` and populated through the macOS kernel. The two High Sierra images are synthetic: no current tool writes that format, and no genuine High Sierra disc small enough and free to redistribute was found, so `make_highsierra.py` writes them from the working paper (see High Sierra below). All file contents are generated (numbered word-list lines and byte counters) by a throwaway Python script that is deliberately not part of this repository.

Made on macOS 27.0.1 (26A434, arm64) with `hdiutil` (DiskImages framework 704), the system `newfs_msdos`, GNU xorriso 1.5.8.pl02 (Homebrew, already installed), Apple gzip 487.0.1, bsdtar 3.5.3 and Python 3.14.4. Verified with p7zip 17.05 (`7z l`, `7z x`), `xorriso -report_el_torito plain`, `isoinfo` from genisoimage 1.1.11 (Debian 13.7 package 9:1.1.11-4, in a container) and a read-only loop mount in that container (Linux 7.0.14 `isofs`). No image here contains UDF structures.

`manifest.tsv` holds what a reader has to produce. Every image is gzip-compressed (`gzip -9 -n`); the directory is about 160 KB.

## Images

| Fixture | Decoded | Writer | Names presented |
|---|---|---|---|
| `et-noemul.iso.gz` | 921600 | `hdiutil makehybrid -iso -joliet` | Joliet tree (see Names) |
| `et-floppy.iso.gz` | 1536000 | `hdiutil makehybrid -iso -joliet` | Joliet tree |
| `et-floppy-nojoliet.iso.gz` | 1536000 | `hdiutil makehybrid -iso -joliet` | Joliet tree |
| `et-harddisk.iso.gz` | 1116160 | `hdiutil makehybrid -iso -joliet` | Joliet tree |
| `et-harddisk-hidden.iso.gz` | 1128448 | `xorriso -as mkisofs --norock -iso-level 1` | plain ISO 9660, lower case |
| `et-multi.iso.gz` | 446464 | `xorriso -as mkisofs -R -J` | Rock Ridge |
| `et-floppies.iso.gz` | 5728256 | `xorriso -as mkisofs -iso-level 1` | Rock Ridge (upper case source names) |
| `ms-gap.iso.gz` | 23609344 | `xorriso`, two sessions | Rock Ridge |
| `ms-three.iso.gz` | 327680 | `xorriso`, three sessions | Rock Ridge |
| `ms-grown.iso.gz` | 327680 | `xorriso`, three sessions | Rock Ridge |
| `ms-cue.tar.gz` | 265216 | the sessions of `ms-gap.iso` as two track files and a sheet | Rock Ridge |
| `nested.iso.gz` | 921600 | `hdiutil makehybrid -iso -joliet` | Joliet tree |
| `concat.iso.gz` | 294912 | two `xorriso` images, `cat` | Rock Ridge (upper case source names) |
| `highsierra.iso.gz` | 208896 | `make_highsierra.py` | plain, lower case |
| `highsierra-512.iso.gz` | 206848 | `make_highsierra.py <image> 512` | plain, lower case |

## Names

The expected names follow `select_names` and `name` in `src/formats/dos/iso9660.rs`: a root directory whose `.` record carries `SP`, `RR`, `PX` or `ER` and whose entries have `NM` names selects the primary tree, each record is named by its `NM` entry, and a record without one gets its plain name in lower case; otherwise the Joliet tree is used when there is one, and the plain names when there is none.

- `hdiutil makehybrid -iso -joliet` writes `SP`, `ER` (`IEEE_P1282`), `PX`, `TF` and Apple `AA` entries but no `NM` entry. Its discs are therefore listed from the Joliet tree (`Read Me.txt`, `Images/boot144.img`), as `7z` and macOS show them, not from the primary tree in lower case (`read me.txt`, `images/boot144.img`). The source files have the Joliet spelling. An earlier revision of the reader chose the primary tree for any Rock Ridge disc, with or without `NM`, and the expectations were corrected against `7z l` when that was fixed.
- `xorriso -as mkisofs` writes Rock Ridge unless `--norock` is given, `-iso-level 1` included. `et-floppies.iso` and `concat.iso` have `NM` entries with the upper-case names of their source files (`README.TXT`, `DISKS/FD1440.IMG`).
- `et-harddisk-hidden.iso` (`--norock`, no Joliet) and the High Sierra images have plain names only.

## El Torito

Boot catalog contents as `xorriso -report_el_torito plain` reports them. "Entry" counts the initial/default entry as 0 and section entries after it; the validation entry and section headers are not counted. "Size" follows the rule the reader implements: 1228800, 1474560 or 2949120 bytes for floppy emulation, the end of the last partition in the image's partition table for hard disk emulation, sector count times 512 without emulation. `7z` extracts a `[BOOT]/...` file of exactly that size and content for every entry below except that of `et-harddisk-hidden`.

| Fixture | Entry | Platform, emulation | Sector count | Start sector | Size | Same extent as a listed file | Expected entry |
|---|---|---|---|---|---|---|---|
| `et-noemul` | 0 | BIOS, none | 4 | 34 | 2048 | `Boot/loader.bin` (2048 bytes) | none |
| `et-floppy` | 0 | BIOS, 1.44 MB | 0 | 30 | 1474560 | `Images/boot144.img` | none |
| `et-floppy-nojoliet` | 0 | BIOS, 1.44 MB | 0 | 30 | 1474560 | no, in the primary tree only (see below) | `..eltorito/0.img` |
| `et-harddisk` | 0 | BIOS, hard disk | 2048 | 30 | 1048576 | no, in the primary tree only (512 blocks of 2048) | `..eltorito/0.img` |
| `et-harddisk-hidden` | 0 | BIOS, hard disk | 1 | 34 | 1048576 | no, in no tree | `..eltorito/0.img` |
| `et-multi` | 0 | BIOS, none | 4 | 214 | 2048 | the first 2048 bytes of `boot/bios.bin` (4096 bytes), which starts there | not listed: a no-emulation image that starts a longer listed file |
| `et-multi` | 1 | UEFI, none | 720 | 34 | 368640 | no, in no tree | `..eltorito/1.img` |
| `et-floppies` | 0 | BIOS, 1.2 MB | 1 | 34 | 1228800 | no, in no tree | `..eltorito/0.img` |
| `et-floppies` | 1 | BIOS, 2.88 MB | 1 | 634 | 2949120 | no, in no tree | `..eltorito/1.img` |
| `et-floppies` | 2 | BIOS, 1.44 MB | 1 | 2076 | 1474560 | `DISKS/FD1440.IMG` | none |
| `et-floppies` | 3 | BIOS, none | 6 | 2074 | 3072 | no, in no tree | `..eltorito/3.img` |

- `hdiutil` writes sector count 0 for floppy emulation and the image's sector count for hard disk emulation; `xorriso` writes 1 for both.
- `hdiutil` cannot leave its boot image out of the disc (hiding it from every tree makes it write an empty file). In `et-floppy-nojoliet` and `et-harddisk` the boot image is hidden from the Joliet tree only (`-hide-joliet`): the primary tree still has `IMAGES/BOOT144.IMG` or `HARDDISK.IMG` at the catalog's start sector with the image's length, but the Joliet tree is the one presented (see Names). `7z`, which also shows the Joliet tree, lists the image only as `[BOOT]`; here it is expected once, as `..eltorito/0.img`.
- In `et-harddisk-hidden`, `et-multi` and `et-floppies` the hidden images and the boot catalog (`-hide`, `-hide-joliet`) are in no directory at all.
- `et-harddisk-hidden` is the disc that tells the size rules for hard disk emulation apart: sector count times 512 is 512 bytes, which is what `7z` extracts; the partition table gives 1048576, which is what `xorriso -report_el_torito` reports (`hdsiz/512: 2048`) and what is expected. In `et-harddisk` both rules give 1048576.
- `et-multi` entry 0 covers the first 2048 bytes of the visible 4096-byte `boot/bios.bin`: a no-emulation image that starts where a longer listed file starts. It is expected only as `boot/bios.bin`; entry 1 keeps its number.
- The boot catalog is never an expected entry.

Boot image contents:

| Image | Made by | Files (as created) |
|---|---|---|
| 1.44 MB floppy, label `BOOTFLOPPY` | `newfs_msdos -F 12 -f 1440` | `AUTOEXEC.BAT` (348), `SYS/KERNEL.SYS` (5000), `SYS/CONFIG.TXT` (72) |
| 1.2 MB floppy, label `FLOPPY12` | `newfs_msdos -F 12 -f 1200` | `DISK12.TXT` (146) |
| 2.88 MB floppy, label `FLOPPY288` | `newfs_msdos -F 12 -f 2880` | `DISK288.TXT` (214), `BIG.DAT` (9000) |
| UEFI image, 720 sectors, label `EFIBOOT` | `newfs_msdos -F 12 -S 512 -s 720` | `EFI/BOOT/BOOTX64.EFI` (6000), `STARTUP.NSH` (52) |
| hard disk, 2048 sectors | `hdiutil create -size 1m -fs "MS-DOS FAT12" -layout MBRSPUD -volname HDBOOT` | `COMMAND.TXT` (557), `DOS/DRIVER.BIN` (7000) |

`et-harddisk-hidden.iso` was written by `xorriso -as mkisofs --norock -iso-level 1 -V ET_HDHID -b HDBOOT.IMG -hard-disk-boot -hide HDBOOT.IMG -c BOOT.CAT -hide BOOT.CAT -no-pad` from `README.TXT`, `DOCS/NOTES.TXT`, `DOCS/COUNTER.BIN` and the hard disk image of `et-harddisk.iso`. The hard disk image, used by both hard disk discs, has one partition table entry: type `01`, first sector 1, 2047 sectors. The no-emulation "loaders" are counter bytes, not code. Files were written with `cat > <mount point>/<name>`; the `._*` AppleDouble files the kernel adds on FAT were deleted before unmounting, so the directories contain deleted entries. `7z x` of every FAT image gives back exactly the source files.

## Multi-session

Session contents (Rock Ridge names, no Joliet):

| Session | Change | Tree afterwards |
|---|---|---|
| 1 (`MS_ONE`) | | `readme.txt`, `keep/stay.bin`, `keep/also.txt`, `docs/changed.txt`, `docs/removed.txt`, `gone/inside.txt` |
| 2 (`MS_TWO`) | replaces `docs/changed.txt` (300 to 749 bytes), removes `docs/removed.txt` and the directory `gone`, adds `added/new.txt` and `second.bin` | `readme.txt`, `keep/stay.bin`, `keep/also.txt`, `docs/changed.txt`, `added/new.txt`, `second.bin` |
| 3 (`MS_THREE`) | replaces `readme.txt`, adds `third/last.txt`, removes `added/new.txt` (the directory `added` stays, empty) | `readme.txt`, `keep/stay.bin`, `keep/also.txt`, `docs/changed.txt`, `second.bin`, `third/last.txt` |

Layout xorriso produced:

- Session 1 (`xorriso -outdev s1.iso`) is 96 sectors. xorriso starts its first session at sector 32 and also puts a descriptor set at sector 16: the primary descriptor at 16 says volume space 66 and root directory at sector 50; a second descriptor set at sector 48 describes the same tree.
- A later session's descriptors use absolute sector numbers. Session 2 of `ms-gap.iso` starts at sector 11496: descriptors at 11512, root directory at 11514, new file data at 11522 to 11524, while `readme.txt` (sector 65) and `keep/*` (61, 62) still point into session 1.
- The primary descriptor of a later session gives the size of that session alone as its volume space size (31 for session 2, 29 for session 3), not an absolute end.

| Fixture | Sessions start at | How assembled | Expected tree |
|---|---|---|---|
| `ms-gap.iso` | 0, 11496 | `s1.iso`, zeros up to sector 11496, then the session written by `-grow_blindly 11496`. 11400 sectors is the distance a CD-R leaves between sessions (lead-out 6750, lead-in 4500, pregap 150) | session 2 |
| `ms-three.iso` | 0, 96, 128 | `s1.iso` grown in place twice by `xorriso -dev`, then sector 16 put back from `s1.iso` (see below) | session 3 |
| `ms-grown.iso` | 0, 96, 128 | the same file as xorriso left it: sector 16 describes session 3 (volume space 157, root directory at 146) | session 3 |

When xorriso appends a session to an image file it also rewrites the primary descriptor in sector 16 to describe the new session, which a write-once disc cannot have. `ms-three.iso` restores that one sector from `s1.iso`; it is the only sector in which `ms-three.iso` and `ms-grown.iso` differ, and the only one in which the grown file differs from `s1.iso` within the first 96 sectors. A session appended in place and one written by `-grow_blindly` for the same address differ only in their primary descriptor sector.

Commands (`X` = `xorriso -report_about SORRY`, `S2ARGS` = `-volid MS_TWO -padding 0 -map s2/changed.txt /docs/changed.txt -rm /docs/removed.txt -- -rm_r /gone -- -map s2/new.txt /added/new.txt -map s2/second.bin /second.bin`):

```
X -outdev s1.iso -volid MS_ONE -padding 0 -map s1 /
X -indev s1.iso -outdev s2.bin -grow_blindly 11496 S2ARGS
cp s1.iso ms-gap.iso; truncate -s $((11496 * 2048)) ms-gap.iso; cat s2.bin >> ms-gap.iso

cp s1.iso grow.iso
X -dev grow.iso S2ARGS
X -dev grow.iso -volid MS_THREE -padding 0 -map s3/readme.txt /readme.txt -map s3/last.txt /third/last.txt -rm /added/new.txt --
cp grow.iso ms-grown.iso; cp grow.iso ms-three.iso
dd if=s1.iso of=ms-three.iso bs=2048 skip=16 seek=16 count=1 conv=notrunc
```

Verification: `xorriso -load sbsector <start> -indev <image> -find / -type f -exec lsdl --` and `isoinfo -T <start> -R` ("examine multi-session images", `<start>` the first sector of a session) list (and `isoinfo` extracts), for `ms-gap.iso` at 0 and 11496, for `ms-three.iso` at 0, 96 and 128 and for `ms-grown.iso` at 0, exactly the tree of the table above with the content of the source files. `7z` and `xorriso -indev` read only the descriptor at sector 16 and show session 1 for `ms-gap.iso` and `ms-three.iso`, session 3 for `ms-grown.iso`. No tool on this system mounts the last session of a multi-session image file without a table of contents.

### `ms-cue.tar.gz`

`disc.cue`, `session1.bin` (`s1.iso`, 96 sectors) and `session2.bin` (the 32 sectors of session 2 of `ms-gap.iso`), in the form Redump-style dumps use for multi-session discs:

```
REM SESSION 01
FILE "session1.bin" BINARY
  TRACK 01 MODE1/2048
    INDEX 01 00:00:00
REM LEAD-OUT 00:01:21
REM SESSION 02
REM LEAD-IN 01:00:00
REM PREGAP 00:02:00
FILE "session2.bin" BINARY
  TRACK 02 MODE1/2048
    INDEX 01 00:00:00
```

Track 2 starts at disc sector 96 + 6750 + 4500 + 150 = 11496, which only the `REM` lines say. Expected: `track01.img` lists the session 1 tree. What `track02.img` lists on its own is not pinned down: its directories are at file sectors 18 to 22 but call themselves 11514 to 11518, and four of its six files (`readme.txt`, `keep/*`) lie in the other file. The ideal listing is the session 2 tree; a reader without access to track 1 can give `docs/changed.txt`, `added/new.txt` and `second.bin` at most. The test only requires that nothing listed below `track02.img` has content other than that of session 2. No tool was found that reads this sheet as one multi-session disc.

## Nested and concatenated volumes

- `nested.iso`: `Read Me.txt`, `Images/inner.iso` (73728 bytes, itself an ISO 9660 volume with Rock Ridge and Joliet by `xorriso -as mkisofs -J -no-pad` holding `Inner Read Me.txt` and `Sub/payload.bin`) and `Images/zz after.txt`. The inner volume's primary descriptor is in sector 44 of the outer image. The inner volume's descriptors lie inside the outer volume space. Expected: the outer tree, with `Images/inner.iso` as a nested container.
- `concat.iso`: `cat first.iso inner.iso`. `first.iso` is 108 sectors (`FIRST.TXT`, `FILLER.BIN` of 150000 bytes, volume space 108), so `inner.iso` has a descriptor set at sector 108 + 16, exactly where a second session could start. Its root directory address is 19, relative to its own start, so it is not a session of this disc. Expected: the tree of `first.iso` only. `7z` shows the same.

## High Sierra

Synthetic. `make_highsierra.py` (Python 3 standard library only) writes the image and prints the expected listing; `python3 make_highsierra.py highsierra.iso` and `python3 make_highsierra.py highsierra-512.iso 512` reproduce both images byte for byte. Every offset comes from the working paper of the CDROM Ad Hoc Advisory Committee of 28 May 1986 (the High Sierra Group proposal); the script cites the section for each field. BP is the paper's byte position, counted from 1.

| Structure | Section | Layout used |
|---|---|---|
| Descriptor header | 11.1 | BP 1-8 descriptor LBN (both byte orders), BP 9 type (1 standard file structure, 255 terminator), BP 10-14 `CDROM`, BP 15 version 1 |
| Standard File Structure Volume Descriptor | 11.4 | BP 17-48 system id, 49-80 volume id, 89-96 volume space size, 129-132 set size, 133-136 sequence number, 137-140 logical block size, 141-148 path table size, 149-152 first L path table, 165-168 first M path table, 181-214 root directory record, 215-726 identifiers, 727-790 copyright and abstract file, 791-854 four 16-digit dates without a zone byte, 855 file structure version |
| Terminator | 11.3 | header only, type 255, in sector 17 |
| Directory record | 13.1 | BP 1 length, 2 extended attribute record length, 3-10 extent, 11-18 data length, 19-24 date (six bytes, no zone), 25 file flags, 26 reserved, 27 interleave size, 28 interleave skip, 29-32 volume sequence number, 33 identifier length, 34 identifier, pad byte when the identifier length is even |
| File flags | 13.1.6 | bit 0 existence, 1 directory, 2 associated file, 7 multi-extent |
| Path table record | 12.1.1 | BP 1-4 extent, 5 extended attribute record length, 6 identifier length, 7-8 parent number, 9 identifier, pad byte when the identifier length is odd |
| Numbers | 10.2, 10.3 | both-byte-order fields little-endian first |
| Record order | 8.3.2, 8.2.1 | by identifier; an associated file after the file of the same name |

The table of 11.4 prints the root directory record as BP 171-214; it is 34 bytes long and follows BP 177-180, so 181-214 is used, which the Linux mount below confirms. Compared with ISO 9660 the descriptor starts 8 bytes later, the directory record date is one byte shorter so the flags are at BP 25 instead of 26, and path table records begin with the extent.

Contents (71 files): `AUTOEXEC.BAT;1`, `BIG.BIN;1` (5000 bytes), `EMPTY.DAT;1` (0 bytes), `HIDDEN.TXT;1` (existence bit set; listed, as `Iso9660Container` lists such files on ISO 9660 volumes), `NOEXT.;1`, `README.TXT;1`, `RESOURCE.DAT;1` (700 bytes) followed by an associated file of the same name (not listed), `SPLIT.BIN;1` (4096 + 1500 bytes in two extents that are not adjacent, the first record with the multi-extent bit), `XATTR.TXT;1` (one logical block of extended attribute record before the data), `DOCS/GUIDE.TXT;1`, `DOCS/DEEP/NESTED.TXT;1`, and `MANY/F00.TXT;1` to `F59.TXT;1` (a directory of two sectors). The 512-byte variant has the same tree with every address in 512-byte blocks.

Verification: the Linux kernel mounts both images as High Sierra (`mount -o loop,ro -t iso9660`, with `block=512` for the second) and shows the same 71 paths in lower case without versions (`noext` for `NOEXT.;1`), the same sizes and the same content, one `resource.dat` of 700 bytes, `split.bin` of 5596 bytes and all dates as 15 June 1987. `isoinfo -d` reports "CD-ROM is in High Sierra format" but cannot list it; `7z` does not open High Sierra images. The images were not mounted on macOS.

## manifest.tsv

Columns `fixture`, `kind`, `path`, `size`, `fnv64` (FNV-1a 64, hex), `format`. `path` is the expected entry path below the image, with names as `Iso9660Container` presents them (see Names): Rock Ridge and Joliet names as stored, plain names in lower case without version and trailing full stop, FAT names in lower case.

| `kind` | Expected |
|---|---|
| `file` | a leaf of the disc |
| `image` | an entry of the disc that is a container of `format` (`fat`, `mbr`, `iso9660`); `size` and `fnv64` are those of the whole image |
| `inside` | a leaf inside such an image, reached by nested traversal. `*` stands for the one path component the MBR reader gives the partition |

Rows of the pseudo-fixture `ms-session1` are the tree of session 1, which is what the first 96 sectors of `ms-gap.iso` and `track01.img` of the cue sheet list.

File rows were written from the source files, boot image rows from the bytes at the catalog's start sector with the size rule above. `7z x` agrees in size and content, and in name apart from letter case, with every `file` and `image` row outside `..eltorito/` for the images it reads from sector 16 (all but `ms-gap`, `ms-three` and the High Sierra pair, which `isoinfo -T`, `xorriso -load sbsector <start>` and the Linux mount cover), and `7z x` of each FAT image with every `inside` row. `7z` also shows the volume label of a FAT image as an empty file (`BOOTFLOP.PY` for `BOOTFLOPPY`); that is not an expected entry.

## Not covered

- A genuine High Sierra disc, and High Sierra descriptors other than the standard one (coded character set, boot record, unspecified structure).
- Raw 2352-byte sector dumps of any of these discs.
- A multi-session disc written by another program than xorriso, and one whose later sessions give an absolute volume space size.
- Hard disk emulation with a boot file larger than its partition table covers; a catalog with section entry extensions or more than one section; an image listed twice by two catalog entries.
- A no-emulation boot file whose length is not a multiple of 512 bytes (the catalog then counts the sectors it rounds up to, and the image is still that file: `xorriso -as mkisofs -b boot/odd.bin -no-emul-boot` records a 1500-byte file with load size 4, a whole 2048-byte block, and other writers may count three 512-byte sectors; both are covered by unit tests in `iso9660.rs`, the first also checked by hand against that xorriso output), and a no-emulation entry with sector count 0.
- A cue sheet whose single file holds both sessions.

## Sources

| Source | Use |
|---|---|
| Working Paper for Information Processing - Volume and File Structure of Compact Read Only Optical Discs for Information Interchange, CDROM Ad Hoc Advisory Committee, 28 May 1986; transcription at https://www.os2museum.com/files/docs/cdrom/hsg_wp.html | every High Sierra offset |
| "El Torito" Bootable CD-ROM Format Specification 1.0, Phoenix Technologies and IBM, 1995 | reading the catalogs the tools wrote |
| ECMA-119 (ISO 9660) | reading the descriptors the tools wrote |
| `hdiutil`, `newfs_msdos`, the macOS FAT driver (run only) | Apple, part of macOS |
| GNU xorriso 1.5.8.pl02 (run only) | GPL-3.0-or-later |
| p7zip 17.05 (run only) | LGPL-2.1-or-later |
| genisoimage `isoinfo` 1.1.11, Linux `isofs` (run only) | GPL-2.0 |
