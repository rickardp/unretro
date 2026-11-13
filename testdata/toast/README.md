# Toast image fixtures

One small image per layout that [`docs/toast/README.md`](../../docs/toast/README.md) says occurs, or must be accepted, under the `.toast` extension. Everything is generated: file contents are numbered word-list lines and byte counters, resource forks are built from those, and nothing comes from a real disc or from Toast itself. The volumes were written by `hdiutil makehybrid` (to a file), hfsutils and genisoimage; partition maps in Toast's style, the concatenated hybrid and the sector framings were added by `make_layouts.py`, which is part of this directory. The content generator and the scripts that drove the tools and wrote `manifest.tsv` are throwaway and deliberately not part of this repository.

No image was mounted or attached at any point, on the host or in the container.

Made on macOS 27.0.1 (26A434, arm64) with `hdiutil` (DiskImages framework 704), Apple gzip 487.0.1, Info-ZIP Zip 3.0 and Python 3.14.4, and in a Debian 12.15 container (arm64) with hfsutils 3.2.6 (package 3.2.6-15) and genisoimage 1.1.11 (package 9:1.1.11-3.4).

`manifest.tsv` holds what a reader has to produce. Every image is gzip-compressed on its own (`gzip -9 -n`), `archived.toast.zip` excepted; the directory is about 230 KB.

## Images

"Variant" is the row of the overview table in `docs/toast/README.md`, section 2.

| Fixture | Decoded | Variant | Layout | Top level |
|---|--:|--:|---|---|
| `apm-hfs.toast.gz` | 825344 | 1 | Toast's map (`MRKS`, blocks 1 to 2; `Toast 5.0.2 HFS Builder`, type `Apple_HFS`, block 9, so the volume starts at byte 4608, a quarter into a sector) around an 800 KiB HFS volume; padded to a whole sector | `Apm` |
| `apm-ddm2048.toast.gz` | 839680 | 1 | as a bootable Toast 5.2.3 disc: driver descriptor declares 2048-byte blocks, the map is in 512-byte blocks: `MRKS` (blocks 1 to 7), `Macintosh` (`Apple_Driver43`, block 8, 8192 counter bytes), `Toast 5.2.3 HFS Builder` (block 37), the same HFS volume | `Apm` |
| `apm-hfsplus.toast.gz` | 921600 | 2 | as `hdiutil makehybrid -hfs` wrote it: driver descriptor declares 2048-byte blocks; map `Apple` / `DiscRecording 9.0.3d5` (`Apple_HFS`) present twice, in 512-byte blocks from byte 512 (volume at block 64, 92 blocks) and in 2048-byte blocks from byte 2048 (block 16, 23 blocks), both naming bytes 32768 to 79872; HFS Plus, 2048-byte allocation blocks, no wrapper. Sectors 39 to 449 are zero | `Apm` |
| `apm-map2048.toast.gz` | 102400 | 9 | map in 2048-byte units only (nothing at byte 512): `Apple` (blocks 1 to 15), `Mac_OS_X` (`Apple_HFS`, block 16, the HFS Plus volume of `apm-hfsplus`), an unnamed `Apple_Free` (block 39, 11 blocks) | `Apm` |
| `hfs-bare.toast.gz` | 819200 | 3 | the HFS volume alone, master directory block at byte 1024 | `Hfs` |
| `hfsplus-bare.toast.gz` | 47104 | 3 | the HFS Plus volume alone | `Hfs` |
| `iso-joliet.toast.gz` | 102400 | 4 | ISO 9660 level 1 with Joliet, no Rock Ridge; system identifier `APPLE COMPUTER, INC., TYPE: 0002` and application identifier `TOAST ISO 9660 BUILDER COPYRIGHT (C) 1997-2002 ROXIO`, set with `-sysid` and `-A` | `Iso9660` |
| `hybrid-shared.toast.gz` | 921600 | 5 | `hdiutil makehybrid -hfs -iso -joliet`: map as in `apm-hfsplus` (volume at byte 92160, 49152 bytes); the ISO volume space (69 sectors) ends where the HFS Plus partition ends and the ISO directories point into it. `PC/` is hidden from the Mac side, `App/` from the ISO and Joliet trees | `Apm` |
| `hybrid-concat.toast.gz` | 1208320 | 6 | ISO 9660 with Joliet (39 sectors, volume space 39), Toast's map written into its unused first sectors, `Toast 5.1.3 HFS/Joliet Builder` at block 158: an HFS volume starting 1024 bytes after the ISO volume ends. The two sides share no file. 150.5 zero sectors follow the partition | `Apm` |
| `hybrid-nomap.toast.gz` | 1210368 | 5 | `genisoimage -hfs`: no partition map, HFS master directory block at byte 1024, ISO 9660 with Joliet and Rock Ridge over the same extents (volume space is the whole file). `PC/` is hidden from the Mac side, `Mac/` from the ISO side. The last 150 sectors are zero | `Hfs` |
| `udf.toast.gz` | 1148928 | 8 | `hdiutil makehybrid -udf`: UDF 1.50 only (`BEA01` at sector 16) | `Udf` |
| `udf-bridge.toast.gz` | 1148928 | 7 | `hdiutil makehybrid -iso -joliet -udf` | `Iso9660` |
| `apm-hfs.m1.toast.gz`, `apm-hfs.m2.toast.gz` | 947856 | 10 | `apm-hfs.toast` as 403 raw 2352-byte sectors, Mode 1 and Mode 2 Form 1 | `Apm` |
| `apm-map2048.m1.toast.gz`, `.m2.toast.gz` | 117600 | 10 | `apm-map2048.toast` as 50 raw sectors | `Apm` |
| `iso-joliet.m1.toast.gz`, `.m2.toast.gz` | 117600 | 10 | `iso-joliet.toast` as 50 raw sectors | `Iso9660` |
| `apm-map2048.xa2056.toast.gz`, `iso-joliet.xa2056.toast.gz` | 102800 | 11 | 2056-byte sectors: sub-header and user data | `Apm`, `Iso9660` |
| `apm-map2048.m2-2336.toast.gz`, `iso-joliet.m2-2336.toast.gz` | 116800 | 12 | 2336-byte sectors: sub-header, user data, error detection and correction | `Apm`, `Iso9660` |
| `archived.toast.zip` | | 15 | ZIP (deflate) of `iso-joliet.toast` and `apm-map2048.toast` | `Zip` |

Trailing zero sectors (section 3.1 of the document) are in `hybrid-concat` (appended), `hybrid-nomap` (genisoimage's padding) and the `hdiutil` images. `apm-hfs`, `apm-ddm2048` and `hybrid-concat` have a volume that does not start on a sector boundary. `Toast 5.1.3 HFS/Joliet Builder` contains a `/`, so the entry is expected as `p1.img`.

Not covered: an HFS wrapper around HFS Plus, a map with several volume partitions or an `ISO` partition, Form 2 sectors, a StuffIt archive of an image, and anything written by Toast itself.

## Contents

The Mac tree, in every image with a Mac volume (each image family has its own label in the text, so hashes differ between families):

| Path | Type/creator | Data fork | Resource fork |
|---|---|--:|---|
| `Read Me` | `TEXT`/`ttxt` | 900 | |
| `Docs/Notes/nested.txt` | `TEXT`/`ttxt` | 300 | |
| `Docs/counter.bin` | `BINA`/`hDmp` | 3000 | |
| `Docs/big.txt` | `TEXT`/`R*ch` | 9000 | |
| `empty` | `TEXT`/`ttxt` | 0 | |
| `App/Sample App` | `APPL`/`Smpl` | 0 | 1935 bytes: `CODE` 0 and 1, `STR#` 128, `vers` 1 |
| `Forks/both.bin` | `BINA`/`hDmp` | 2500 | 1038 bytes: `TEXT` 128 and 129 |
| `Names/Café Menu.txt` | `TEXT`/`ttxt` | 120 | |

- HFS stores `é` as Mac OS Roman `8E`; HFS Plus stores it decomposed (`e` + U+0301); Joliet and UDF store U+00E9.
- In `hybrid-nomap` the tree is under `Mac/`, and genisoimage wrote the name as `Caf_ Menu.txt`. It also added `Desktop DB` (8192 bytes, `BTFL`/`DMGR`) and an empty `Desktop DF` to the root.
- `iso-joliet`, `udf` and `udf-bridge` hold the data forks only, without `App/Sample App`.
- The PC tree (`hybrid-shared` and `hybrid-nomap` under `PC/`, `hybrid-concat` at the ISO root): `README.TXT` (500), `WIN/SETUP.DAT` (4000), `WIN/DRIVERS/Long Driver Name.inf` (260), `AUTORUN.INF` (31), `EMPTY.TXT` (0).
- The ISO side of `hybrid-shared` records the resource fork of `Forks/both.bin` as an associated file (file flag `04`, 1038 bytes, the same Joliet name). It is not a file of the tree and is not in the manifest.

## Commands

`M` = `python3 make_layouts.py`. Sources: `osx/<set>` are folders on APFS (resource forks written to `<file>/..namedfork/rsrc`, type and creator with `xattr -wx com.apple.FinderInfo`), `mb/<set>` hold one MacBinary II file per Mac file, `pc/<set>` are plain folders, `nomap/` has MacBinary files under `Mac/` and plain files under `PC/`.

```
# container: classic HFS volumes
dd if=/dev/zero of=hfs.hfs bs=1024 count=800
hformat -l 'Toast HFS' hfs.hfs
hmkdir :Docs; hmkdir :Docs:Notes; ...; hcopy -m mb/hfs/00.bin :; hcopy -m mb/hfs/01.bin :Docs:Notes:; ...; humount
(the same for concat-mac.hfs, label 'Concat Mac')

# container: ISO 9660
genisoimage -quiet -no-pad -J -iso-level 1 -input-charset utf-8 -sysid 'APPLE COMPUTER, INC., TYPE: 0002' \
  -A 'TOAST ISO 9660 BUILDER COPYRIGHT (C) 1997-2002 ROXIO' -V TOAST_ISO -o iso-joliet.toast pc/iso
genisoimage (same options) -V CONCAT_PC -o concat-pc.iso pc/concat-pc
genisoimage -quiet -hfs --macbin -J -r -input-charset iso8859-1 -V NOMAP -hfs-volid 'Nomap Mac' \
  -hide-hfs PC -hide Mac -hide-joliet Mac -o hybrid-nomap.toast nomap

# host
hdiutil makehybrid -hfs -hfs-volume-name "Toast HFS Plus" -o apm-hfsplus osx/hfsplus
hdiutil makehybrid -hfs -iso -joliet -default-volume-name SHARED -hfs-volume-name "Shared Mac" \
  -hide-hfs osx/shared/PC -hide-iso osx/shared/App -hide-joliet osx/shared/App -o hybrid-shared osx/shared
hdiutil makehybrid -udf -udf-volume-name TOAST_UDF -o udf osx/udf
hdiutil makehybrid -iso -joliet -udf -default-volume-name TOAST_BRIDGE -o udf-bridge osx/bridge

cp hfs.hfs hfs-bare.toast
M apm apm-hfs.toast "Apple_HFS,Toast 5.0.2 HFS Builder,9,hfs.hfs"
M apm apm-ddm2048.toast --ddm-block 2048 --map-blocks 7 \
  "Apple_Driver43,Macintosh,8,driver.bin" "Apple_HFS,Toast 5.2.3 HFS Builder,37,hfs.hfs"
M carve apm-hfsplus.toast 1 hfsplus-bare.toast
M apm apm-map2048.toast --ddm-block 2048 --unit 2048 --map-name Apple --map-blocks 15 \
  "Apple_HFS,Mac_OS_X,16,hfsplus-bare.toast" "Apple_Free,,39,@11"
M apm hybrid-concat.toast --base concat-pc.iso --tail 307200 \
  "Apple_HFS,Toast 5.1.3 HFS/Joliet Builder,158,concat-mac.hfs"
M frame m1|m2 apm-hfs.toast apm-hfs.<framing>.toast
M frame m1|m2|m2-2336|xa2056 <base>.toast <base>.<framing>.toast      (base: apm-map2048, iso-joliet)
zip -9 -X archived.toast.zip iso-joliet.toast apm-map2048.toast
gzip -9 -n *.toast
```

`hformat` refuses a volume under 800 KiB, which is why the raw Mode 1 version of `apm-hfs` is the largest file here: every Mode 1 sector, empty ones included, carries parity that depends on its address. `hdiutil`, `hformat` and `genisoimage` stamp the current time into the volume, so a rebuild gives different bytes and hashes.

### Sector framings

`make_layouts.py frame` writes, per 2048-byte sector: 12 sync bytes, the BCD minute/second/frame address (sector + 150) and the mode byte; for Mode 1 the user data, the EDC over bytes 0 to 2063, eight zero bytes and P/Q parity; for Mode 2 Form 1 the sub-header `00 00 08 00` twice (`00 00 89 00` twice on the last sector), the user data, the EDC over sub-header and data, and parity computed over a zeroed address, as ECMA-130 defines them. `m2-2336` is bytes 16 to 2351 of that Mode 2 sector and `xa2056` bytes 16 to 2071.

## Verification

Expected rows were computed from the bytes handed to the tools and then compared with what reference tools read back from the finished images; the manifest was written only after both agreed for every image. None of it comes from this crate.

| Tool | Used for |
|---|---|
| machfs 1.3 with macresources 1.2 (Python 3.11.2, container) | every classic HFS volume (bare, and each partition as located by the checking script's own reading of the map): paths, type/creator, data and resource forks, and the single resources of each fork |
| hfsutils 3.2.6 `hmount <image> [1]`, `hls -lRa`, `hcopy -r` | the same volumes in place, through hfsutils' own reading of the partition map: paths, type/creator and both fork sizes equal machfs. machfs leaves out the desktop files of `hybrid-nomap`; their rows come from `hls` and `hcopy` |
| 7-Zip 26.02 (`7z x -sns`, container; `7zz l` 26.01 on the host) | HFS Plus data and resource forks (`apm-hfsplus`, `hfsplus-bare`, the carved partitions of `apm-map2048` and `hybrid-shared`); ISO 9660/Joliet trees (`-tiso`) of `iso-joliet`, the three hybrids and `udf-bridge`; UDF trees (`-tudf`) of `udf` and `udf-bridge`; the members of `archived.toast.zip`, which equal the bare images |
| GNU xorriso 1.5.8.pl02 (`-indev <image> -find / -type f`) | file counts of the ISO trees: 7, 13 (with the associated file), 5, 5 and 7 |
| `isoinfo -d` (genisoimage) | identifiers and volume space of `iso-joliet` and `hybrid-concat` |
| edccchk 1.27 (commit 2c59f19, built in the container) | every `m1` and `m2` image: all sectors Mode 1 or Mode 2 Form 1, 0 errors, 0 warnings |
| bchunk 1.2.2 with a one-track sheet (`MODE1/2352`, `MODE2/2352`) | every `m1` and `m2` image gives back its cooked image byte for byte |
| iat 0.1.3 | `iso-joliet.m1`, `.m2` and `.m2-2336` give back `iso-joliet.toast` (iat recognises ISO 9660 content only) |
| `make_layouts.py cook` | all ten framed images, the 2056-byte ones included, for which no other tool was found |

- 7-Zip reports "Unexpected end of archive" for the HFS Plus volumes `hdiutil` writes and extracts every file.
- No tool at hand shows HFS Plus type and creator without mounting. The checking script read them from the catalog file record that follows each file's key; they equal the codes set on the source files.
- bchunk does not handle a file of 2336-byte sectors (it expects 2352-byte sectors for `MODE2/2336`).

When the fixtures were made (release build of this repository on 2026-11-13: `unretro -tf`, `--list=tsv -f` and `-xf ... --exec` to hash every leaf), 16 of the 23 agreed with the manifest in top-level format, paths, sizes, hashes and type/creator; the four `xa2056` and `m2-2336` images were not recognised and the ISO 9660 side of the three hybrids could not be reached (gaps G1, G2 and G3 of the document). Those gaps are closed: `tests/toast_fixtures.rs` checks every row of the manifest for all 23 fixtures, and that nothing else is listed.

## manifest.tsv

Columns `fixture`, `kind`, `path`, `size`, `fnv64` (FNV-1a 64, hex), `format`, as in `testdata/iso/manifest.tsv`. `fixture` is the file name without `.gz`. `path` is the expected entry path below the image.

| `kind` | Expected | `format` |
|---|---|---|
| `top` | the image itself (`path` is `.`): size and hash of the decoded file | top-level container: `apm`, `hfs`, `iso9660`, `udf`, `zip` |
| `cooked` | framed images only: the 2048-byte-sector view, equal to the `top` row of the unframed image | as `top` |
| `image` | an entry that is a container: a partition, or an image inside the archive; size and hash of its bytes | its container format |
| `file` | a leaf without Mac metadata: a file of an ISO 9660 or UDF tree (empty files included), or an opaque partition | `-` |
| `data` | a leaf holding a data fork (possibly empty, when the file has a resource fork) | `TYPE/CREA` |
| `rsrc` | a container entry `<path>/..namedfork/rsrc` holding the whole resource fork (seen with `EntryType::All`) | `-` |
| `res` | a leaf holding one resource's data, `<file>/..namedfork/rsrc/<TYPE>/#<id>` | `-` |
| `empty` | a Mac file with two empty forks: not listed, as `HfsContainer` does | `TYPE/CREA` |
| `iso` | hybrids only: a file of the ISO 9660 tree, `path` relative to the ISO root. Listed as `iso9660.img/<path>`, or `..iso9660.img/<path>` in a volume without a partition map (`docs/toast/README.md`, section 5.3) | `-` |

Partition entries are named as `docs/USAGE.md` ("Apple Partition Map Disk Images") says: the map's name, or `p<n>.img`. HFS names are given precomposed, HFS Plus names decomposed as stored. Folders are not entries. A framed image has the rows of its cooked image; `archived.toast.zip` has the rows of its two members below `<member>/`.

## Sources

| Source | Licence |
|---|---|
| `docs/toast/README.md`, `docs/USAGE.md` of this repository | this repository |
| Inside Macintosh: Devices, chapter 3 (driver descriptor record, partition map entry); Inside Macintosh: More Macintosh Toolbox, chapter 1 (resource fork layout, for the generated forks) | Apple documentation |
| MacBinary II standard (header layout and CRC, for the files given to `hcopy -m` and `genisoimage --macbin`) | public specification |
| ECMA-130, Data Interchange on Read-only 120 mm Optical Data Disks (sector layout, EDC, P/Q parity), https://ecma-international.org/publications-and-standards/standards/ecma-130/ | Ecma standard, free to read |
| hfsutils, cdrkit genisoimage/isoinfo, bchunk, iat, GNU xorriso, 7-Zip (run only) | as distributed (GPL, LGPL) |
| machfs and macresources, PyPI packages (run only) | as distributed |
| edccchk 1.27, https://github.com/claunia/edccchk, commit 2c59f19 (built and run) | GPL-3.0 |
