# HFS Plus and HFSX fixtures

Every image here was created, populated and unmounted by macOS itself; no image byte was edited afterwards. All file contents are generated (numbered word-list lines, short fixed strings) by a throwaway Python script that is deliberately not part of this repository. Made on macOS 27.0.1 (26A434, Darwin 27.0.0) with `hdiutil` (DiskImages framework 704), the system `newfs_hfs` that `hdiutil create` runs, Apple gzip 487.0.1 and Python 3.14.4.

`manifest.tsv` holds what a reader has to produce. The script wrote each row from the bytes it had just written to the mounted volume. Afterwards every image was attached again read-only and walked through the kernel: the set of files, symlinks, data fork and resource fork sizes and FNV-1a 64 hashes equals the manifest for all six images. `fsck_hfs -n -f` reports every volume as OK. `7z x` (p7zip 17.05) of `plain.img` gives the same data forks for the 430 files it can name and extract; it writes hard links as empty files and spells a `/` in a name differently.

## Images

| Fixture | Size | Decoded | Volume |
|---|---|---|---|
| `plain.img.gz` | 72026 | 3 MiB | HFS+ (`H+`, version 4), no partition map, 4096-byte blocks, not journaled |
| `apm.img.gz` | 14817 | 2 MiB | Apple Partition Map, partition type `Apple_HFS` at block 64: HFS+ (`H+`), 512-byte blocks, journaled (512 KiB journal, empty: start = end) |
| `hfsx.img.gz` | 21304 | 1 MiB | Apple Partition Map, partition type `Apple_HFSX` at block 64: HFSX (`HX`, version 5, binary key compare `0xBC`), 4096-byte blocks |
| `hfsx.dmg` | 25380 | 1 MiB | `hfsx.img` converted to UDZO (zlib); `hdiutil convert` back to UDRW is byte-identical to `hfsx.img` |
| `hfsx-bare.dmg` | 23748 | 1 MiB | UDZO of a second HFSX volume with the same contents and no partition map (`HX` at byte 1024 of the decoded image) |
| `spread.dmg` | 12456 | 3 MiB | UDZO of an Apple Partition Map image, partition type `Apple_HFS` at block 64: HFS+ (`H+`), 4096-byte blocks, with files in the second megabyte of the partition; `hdiutil convert` back to UDRW is byte-identical to the image it was made from |

Commands (`A` = `hdiutil attach <image> -nobrowse -noautofsck -noverify -mountpoint <dir>`, `D` = `hdiutil detach <dir>`):

```
hdiutil create -size 3m -fs HFS+ -layout NONE -volname Plain plain.dmg
A; populate "plain"; D; mv plain.dmg plain.img

hdiutil create -size 1m -fs "Case-sensitive HFS+" -layout SPUD -volname CaseSens hfsx.img
A; populate "hfsx"; D
hdiutil convert hfsx.img -format UDZO -imagekey zlib-level=9 -o hfsx.dmg

hdiutil create -size 1m -fs "Case-sensitive HFS+" -layout NONE -volname CaseSens hfsx-bare-rw.dmg
A; populate "hfsx"; D
hdiutil convert hfsx-bare-rw.dmg -format UDZO -imagekey zlib-level=9 -o hfsx-bare.dmg

hdiutil create -size 2m -fs "Journaled HFS+" -fsargs "-b 512 -J 512k" -layout SPUD -volname Journaled apm.dmg
A; populate "apm"; D; mv apm.dmg apm.img

gzip -9 -n plain.img apm.img hfsx.img

hdiutil create -size 3m -fs HFS+ -layout SPUD -volname Spread spread-rw.dmg
A; populate "spread"; D
hdiutil convert spread-rw.dmg -format UDZO -imagekey zlib-level=9 -o spread.dmg
```

`spread.dmg` was added later, on the same system, with its own throwaway script. A UDIF image of a partitioned disk has one block table per partition. Each table has a first sector, and its chunks count sectors from there. `hdiutil` cuts a partition into chunks of at most 2048 sectors. In `hfsx.dmg` everything in use fits the first chunk of the `Apple_HFSX` table, so a reader that ignores the first sectors, or reads only the first chunk, still finds the whole volume at byte 0. `spread.dmg` has three zlib chunks in its `Apple_HFS` table (2048, 2048 and 704 sectors) and files in the second one.

Populating used plain POSIX calls on the mount point: `open`/`write`, `mkdir`, `link`, `symlink`, a write to `<file>/..namedfork/rsrc` for resource forks, and `xattr -wx com.apple.FinderInfo` for type and creator.

## Contents

The first three sets have `Read Me.txt` (type `TEXT`, creator `ttxt`), `Docs/readme.txt`, `Docs/Notes/Deep/nested.txt`, an empty file `empty.txt` and an empty folder `Empty Folder`.

`plain` (439 files, 13 folders; catalog B-tree depth 2 with 906 leaf records in 4096-byte nodes):

| Path as created | Exercises |
|---|---|
| `Forks/both.bin` | data fork and a resource fork with `TEXT` 128, `TEXT` 129, `STR#` -5 |
| `Forks/rsrc-only` | empty data fork, resource fork with `DATA` 0 |
| `Names/Café déjà vu Ångström.txt` | stored decomposed (`e` + U+0301, `a` + U+0300, `A` + U+030A, `o` + U+0308) |
| `Names/日本語のファイル.txt`, `Names/party 🎉 time.txt` | CJK; U+1F389, a surrogate pair in the stored UTF-16 |
| `Names/Date 11:12:98.txt`, `Names/Über:Ordner/inside.txt` | POSIX `:`, stored as `/` (Finder shows `Date 11/12/98.txt`); in a file and a folder name |
| `Links/original.txt`, `Links/hardlink.txt`, `Links/Elsewhere/third name.txt` | three hard links to one 5000-byte file (one indirect node file in the private data directory) |
| `Links/symlink.txt`, `Links/symlink to folder` | symlinks to `original.txt` and `Elsewhere` |
| `Many/tiny 000.txt` ... `tiny 419.txt` | 420 files of 14 bytes |
| `Fragmented/big.bin` | 151675 bytes in 38 one-block extents (the kernel's `F_LOG2PHYS_EXT` reports 38 separate runs) |
| `Fragmented/fork.bin` | 8197-byte data fork; resource fork of 45366 bytes (`BLOB` 1000, 45056 bytes) in 12 extents |

The fragmentation was made by filling the volume with 226 one-block files, deleting every other one, writing the two files, then deleting the rest and the folder. The extents overflow B-tree ends up with 5 leaf records: 4 for the data fork of `big.bin`, 1 for the resource fork of `fork.bin`.

`hfsx`: `README.TXT`, `readme.txt`, `ReadMe.txt`; folders `Case` and `case` with `Case/file.txt`, `case/file.txt`, `case/FILE.txt`; `École école.txt` and `école École.txt`; `Zebra.txt` and `apple.txt` (which sort the other way round on a case-insensitive volume; `apple.txt` has a resource fork with `TEXT` 128); `big.bin` (70000 bytes).

`spread`: `Read Me.txt` (type `TEXT`, creator `ttxt`), then `filler.bin` (1310720 bytes: 320 blocks of 4096 bytes, each a numbered line padded with zeros), written before `After/late.txt` (10500 bytes), `After/tail.bin` (5000 bytes) and `After/hard link.txt` (a hard link to `late.txt`), so that these three lie more than 1 MiB into the partition. No empty file, folder, symlink or resource fork.

`apm`: `System Folder/Finder` (type `FNDR`, creator `MACS`, 9000-byte data fork, resources `CODE` 0 and 1), `Icon\r` (empty data fork, resource `icns` -16455), `data.bin` (20000 bytes).

## manifest.tsv

Columns `set`, `kind`, `path`, `size`, `fnv64` (FNV-1a 64, hex). `path` is the expected entry path below the volume: names decomposed as stored, and `/` or a control character inside a name replaced by `_`, as `HfsContainer` does for HFS.

| `kind` | Expected |
|---|---|
| `data` | a leaf `<path>` holding the data fork |
| `rsrc` | a container entry `<path>/..namedfork/rsrc` holding the whole resource fork (seen with `EntryType::All`) |
| `res` | a leaf holding one resource's data |
| `empty` | a file with two empty forks: not listed, as `HfsContainer` does for HFS |
| `symlink` | not listed |

Folders are not entries, so `Empty Folder` appears nowhere.

## System leftovers

Attaching with `-nobrowse` kept Spotlight, fseventsd and Finder away: no `.fseventsd`, `.Spotlight-V100`, `.Trashes` or `.DS_Store` exists in any image. What the system did add:

- `\0\0\0\0HFS+ Private Data` and `.HFS+ Private Directory Data\r` in every root (the first holds `iNode38` in `plain`). Must not be listed.
- `.journal` (524288 bytes) and `.journal_info_block` (512 bytes) in the root of `apm`. They are ordinary catalog files that the macOS kernel hides. The test accepts them listed or not.
- A `com.apple.provenance` extended attribute on every file and folder, so every volume has an attributes B-tree. Not visible in any entry.

## Not covered

- HFS wrapper with an embedded HFS+ volume: `newfs_hfs` on this system rejects `-w` and `-h` (the man page still lists them for `-N` only), and `hdiutil create -fs HFS` fails, so no wrapper could be produced.
- DiskCopy 4.2, NDIF and raw 2352-byte CD sector images: `hdiutil convert -format DC42`, `Rdxx` and `ROCo` are rejected, and `UDTO` writes plain 2048-byte sectors.
- Directory hard links, special files, transparent compression (decmpfs), a journal that needs replay, a catalog B-tree deeper than 2, a fragmented catalog or extents overflow file.
