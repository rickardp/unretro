# UDF fixtures

Every image here except the `syn-*` ones (see "Synthetic volumes") was written by macOS tools into a regular file; no image byte was edited afterwards and **no image was ever attached or mounted** (mounting UDF panics the kernel of the machine they were made on). All file contents are generated (numbered word-list lines, short fixed strings) by a throwaway Python script that is deliberately not part of this repository. Made on macOS 27.0.1 (26A434, Darwin 27.0.0) with `hdiutil makehybrid` (DiskImages framework 704; it identifies itself in the images as `*Apple Mac OS X DiscRec`), `/sbin/newfs_udf` from `udf.fs` (`*Apple macOS UDF FS`), Apple gzip 487.0.1 and Python 3.14.4.

`manifest.tsv` holds what a reader has to produce. The script wrote each row from the bytes it had just written to the source folder. Second opinions, both in user space:

- `7zz x` (7-Zip 26.01) of `udf-only-102.iso` and of `bridge.iso` (opened as UDF, and again with `-tiso` for the Joliet tree) gives exactly the manifest's files, sizes and hashes. 7-Zip refuses a UDF volume that contains a symlink ("Unsupported feature"), so for `udf-only-150.iso` it was run on a twin made from the same folder without the three symlinks: equal to the manifest for all 323 files.
- A 70-line walk of the descriptors written from ECMA-167 (anchor at 256, partition and logical volume descriptors, file set descriptor, file entries with short allocation descriptors, file identifier descriptors) reproduces the manifest for all three populated images, symlinks included (ICB file type 12).

7-Zip also opens the empty 2048- and 512-byte-block volumes with physical and metadata partitions and lists no files; it cannot open the sparable, virtual, 1024- and 4096-byte-block ones, so for those "the root is empty" rests on `newfs_udf` having just formatted them.

## Populated images (`hdiutil makehybrid`)

| Fixture | Size | Decoded | Volume |
|---|---|---|---|
| `udf-only-150.iso.gz` | 53498 | 2932736 (1432 sectors) | UDF 1.50 only: `BEA01`, `NSR02`, `TEA01` at sectors 16-18; set `full` |
| `udf-only-102.iso.gz` | 26273 | 1458176 (712 sectors) | UDF 1.02 only; set `small` |
| `bridge.iso.gz` | 30367 | 1458176 (712 sectors) | ISO 9660 + Joliet + UDF 1.50 over one copy of the data: `CD001` three times at sectors 16-18, then `BEA01`, `NSR02`, `TEA01` at 19-21; set `small`, made from the same folder as `udf-only-102` |

```
hdiutil makehybrid -quiet -udf -udf-version 1.50 -udf-volume-name "UDF Only" -o udf-only-150.iso src-full
hdiutil makehybrid -quiet -udf -udf-version 1.02 -udf-volume-name "UDF 1.02" -o udf-only-102.iso src-small
hdiutil makehybrid -quiet -udf -iso -joliet -udf-version 1.50 -default-volume-name "BRIDGE" -o bridge.iso src-small
gzip -9 -n *.iso
```

`-udf-version` accepts only `1.02` and `1.50`. What `makehybrid` writes, in all three images:

- 2048-byte logical blocks, one type 1 (physical) partition starting at sector 257, main and reserve volume descriptor sequences, anchors at sectors 256 and N-257 only (none at N-1).
- File entries (tag 261, never extended file entries), ICB strategy 4, short allocation descriptors. Every file is **one recorded extent**; nothing is embedded in an ICB (the 5-byte `tiny.txt` has its own block), nothing is fragmented and nothing is sparse (the hole in `Big/sparse.bin` is recorded as 256 KiB of zeros). Directories are never embedded either.
- Names in OSTA compressed Unicode, precomposed as given: compression ID 8 when every character is below U+0100 (so accented Latin-1 names are 8-bit), otherwise ID 16; U+1F389 is stored as a UTF-16 surrogate pair.
- Symlinks as ICB file type 12. Hard links as independent file entries (link count 1) with the same content.
- No `.DS_Store` or other system files: the source folders were never opened in Finder. No extended attributes or named streams beyond what `makehybrid` puts into every file entry.

### Set `full` (323 files, 3 symlinks)

| Path | Exercises |
|---|---|
| `Read Me.txt`, `Docs/readme.txt`, `Docs/medium.bin`, `Docs/Notes/Deep/nested.txt` | nested directories |
| `empty.txt`, `Empty Folder` | a file entry without extents; a directory with only its parent entry |
| `tiny.txt` | 5 bytes |
| `Names/Café déjà vu Ångström.txt`, `Names/Mixed Case Name.TXT` | 8-bit names, case kept |
| `Names/日本語のファイル.txt`, `Names/Łódź жук.txt`, `Names/Über Ordner 中/inside.txt`, `Names/party 🎉 time.txt` | 16-bit names next to 8-bit ones; a 16-bit directory name; a surrogate pair |
| `Names/long name 0123...txt` | 250 characters |
| `Names/max 8-bit name xxx...txt`, `Names/ жжж...txt` | 254 8-bit and 127 16-bit characters: the 255-byte identifier limit |
| `Links/original.txt`, `Links/hardlink.txt`, `Links/Elsewhere/third name.txt` | three hard links in the source folder |
| `Links/symlink.txt`, `Links/symlink to folder`, `Links/dangling` | symlinks to a file, to a directory, and to `/nonexistent/absolute/target` |
| `Many/tiny 000.txt` ... `tiny 299.txt` | 300 files of 14 bytes; the directory is 15640 bytes of file identifier descriptors in 8 blocks |
| `Big/big.bin`, `Big/block.bin`, `Big/block+1.bin`, `Big/sparse.bin` | 151675 bytes; exactly one block; one block and a byte; 267240 bytes with a 256 KiB run of zeros |

### Set `small` (55 files)

The same without the emoji, the three longest names, the links, and `Big/block*.bin`/`sparse.bin`; `Many` has 40 files, `Big/big.bin` is 75899 bytes, and `Names/sixty-four characters 0123...txt` is 64 characters long (the Joliet limit `makehybrid` applies). It has no symlinks because `makehybrid` writes a symlink into the Joliet tree as a regular file holding the target path. The Joliet and UDF trees of `bridge.iso` therefore have identical names and contents.

The primary ISO 9660 tree of `bridge.iso` is a third view of the same extents. Its names are upper case with every character outside ASCII dropped (`CAF DJ VU NGSTRM.TXT`, `_.TXT` for the CJK name, `BER ORDNER ` with a trailing space), and its records carry Rock Ridge system use entries (`SP` and `ER` on the root, `PX`, `TF` and Apple's `AA` everywhere) but **no `NM` names**. Rock Ridge without `NM` adds nothing to the names, so the reader lists the disc from its Joliet tree, as `7z` does, and those are the UDF names: the test requires set `small` for `bridge.iso` too.

## Empty volumes (`newfs_udf` on a regular file)

```
mkfile -n <size> <name>.img; newfs_udf <options> <name>.img; gzip -9 -n <name>.img
```

| Fixture | Size | `mkfile` | `newfs_udf` options | What was written |
|---|---|---|---|---|
| `empty-102.img.gz` | 2373 | 1m | `-r 1.02 -v "Empty 102"` | `NSR02`, type 1 partition, file entry (tag 261) |
| `empty-150.img.gz` | 2378 | 1m | `-r 1.50 -v "Empty 150"` | as above |
| `empty-150-sparable.img.gz` | 2612 | 1m | `-r 1.50 -d -p 32 -v "Sparable 150"` | `*UDF Sparable Partition` map, packet length 32, sparing tables at sectors 128 and 480 with 4 entries each, none remapped |
| `empty-150-vat.img.gz` | 2802 | 1500k | `-r 1.50 -t wo -v "VAT 150"` | type 1 map plus `*UDF Virtual Partition`; VAT ICB (file type 0) in sector 259, UDF 1.50 layout: two entries, then the `*UDF Virtual Alloc Tbl` identifier |
| `empty-200.img.gz` | 2376 | 1m | `-r 2.00 -v "Empty 200"` | `NSR03`, extended file entry (tag 266) |
| `empty-201.img.gz` | 2382 | 1m | `-r 2.01 -v "Empty 201"` | as above |
| `empty-201-512.img.gz` | 2377 | 1m | `-r 2.01 -b 512 -v "Empty 512"` | 512-byte blocks; anchors at blocks 256, 1791, 2047 |
| `empty-201-1024.img.gz` | 2377 | 1m | `-r 2.01 -b 1024 -v "Empty 1024"` | 1024-byte blocks |
| `empty-201-4096.img.gz` | 3440 | 2m | `-r 2.01 -b 4096 -v "Empty 4096"` | 4096-byte blocks: the recognition sequence still starts at byte 32768, with 4096-byte steps; first anchor at byte 1048576 |
| `empty-201-sparable.img.gz` | 2668 | 1m | `-r 2.01 -d -p 32 -v "Sparable 201"` | sparable map as above; a system stream directory with `*UDF Non-Allocatable Space` |
| `empty-201-vat.img.gz` | 2812 | 1500k | `-r 2.01 -t wo -v "VAT 201"` | virtual partition; VAT ICB (file type 248) in sector 259, UDF 2.00 layout: 152-byte header, then two entries |
| `empty-250.img.gz` | 2617 | 1m | `-r 2.50 -v "Metadata 250"` | type 1 map plus `*UDF Metadata Partition`: metadata file at partition block 1, mirror file entry at 237 sharing the same extent, bitmap file at 2 |
| `empty-250-mirror.img.gz` | 2870 | 1m | `-r 2.50 --dup-meta-part-file -v "Mirror 250"` | as above with the duplicate flag set and the mirror file (entry at 205) holding its own copy of the file set descriptor and root directory |
| `empty-260.img.gz` | 2621 | 1m | `-r 2.60 --enc utf16 -v "Metadata 260"` | metadata partition, UDF 2.60 |

Common to all: main and reserve volume descriptor sequences; anchors at blocks 256, N-257 and N-1 (write-once volumes: 256 only); a root directory embedded in its ICB holding nothing but the parent entry. From 2.00 on `newfs_udf` always writes extended file entries, so `-e` changes nothing and has no fixture of its own.

Things to know:

- **Write-once volumes end before the image does.** `newfs_udf -t wo` reports "last written block address: 259"; that block is the VAT ICB, and the 490 blocks after it in the 750-block file were never written. A dump of such a disc ends with the VAT ICB, so the test cuts the image after block 259 (`image[..260 * 2048]`) and requires an empty, cleanly read volume there. For the file as `newfs_udf` left it the test only requires that it is detected as UDF and that no entry is invented.
- `empty-150-sparable` has one name in its root: `Non-Allocatable Space`, a zero-length file marked hidden (file characteristics 0x01), as UDF 1.50 2.3.13 prescribes. It belongs to the file system (its ICB has the System flag), so the reader does not list it and the test requires an empty root.
- `empty-260`: `--enc utf16` writes the logical volume identifier with its 16-bit units byte-swapped (compression ID 16 followed by `eM` `at` `ad` ...), so the label of this volume is garbage. That is the tool's doing.
- `-r 1.02 -t wo` is rejected ("UDF 1.02 only supports overwrite media"). `-t wo` needs a file of at least about 1.5 MB ("not enough space to close the disc" below that; at 1100k-1200k it succeeds but writes no file set). The smallest overwritable volume is 300 blocks; below that it warns that an anchor cannot be written.

## Synthetic volumes (`make_synthetic.py`)

No tool on this system writes files into a virtual, sparable or metadata partition, or into a volume with other than 2048-byte blocks, without mounting it. These five images are therefore **not tool output**: `make_synthetic.py` lays the descriptors out itself, from ECMA-167 and the OSTA UDF specifications, and prints the manifest rows from the bytes it placed. It was written by a reviewer of the reader, without the reader's source, as a differential test; it is evidence that two independent readings of the specifications agree, not that a real writer's discs are read. 7-Zip opens none of them (it also refuses the unmodified generator's physical 2048-byte volume, with "data after the end of archive"), so there is no second opinion.

| Fixture | Size | Decoded | Mode, block size | Volume |
|---|---|---|---|---|
| `syn-physical-512.img.gz` | 12764 | 2048000 | `phys 512` | `NSR02`, type 1 partition, file entries |
| `syn-vat150-512.img.gz` | 9152 | 474112 | `vat15 512` | `NSR02`, virtual partition, VAT in the UDF 1.50 layout (file type 0); the image ends with the VAT ICB |
| `syn-vat200-2048.img.gz` | 12047 | 1857536 | `vat20 2048` | `NSR03`, extended file entries, VAT in the UDF 2.00 layout (file type 248) |
| `syn-sparable-1024.img.gz` | 17180 | 4096000 | `spar 1024` | `NSR02`, sparable partition with packet length 32; the packets at partition blocks 32 (holding the root directory's ICB) and 416 are remapped to the spare area and their original blocks overwritten |
| `syn-metadata-4096.img.gz` | 59610 | 16384000 | `meta 4096` | `NSR03`, extended file entries, metadata partition whose file has two separate extents; main and mirror file entries share them |

```
python3 make_synthetic.py syn-physical-512.img  syn-physical-512  phys  512  >> manifest.tsv
python3 make_synthetic.py syn-vat150-512.img    syn-vat150-512    vat15 512  >> manifest.tsv
python3 make_synthetic.py syn-vat200-2048.img   syn-vat200-2048   vat20 2048 >> manifest.tsv
python3 make_synthetic.py syn-sparable-1024.img syn-sparable-1024 spar  1024 >> manifest.tsv
python3 make_synthetic.py syn-metadata-4096.img syn-metadata-4096 meta  4096 >> manifest.tsv
gzip -9 -n syn-*.img
```

The output is deterministic (fixed seed). Every volume holds the same tree, 135 files (134 on the write-once volumes, which have no `cont.bin`):

| Path | Exercises |
|---|---|
| `embedded.txt`, `embedded_ea.txt` | data embedded in the ICB, the second after 52 bytes of extended attribute space |
| `empty` | a file entry without extents |
| `short.bin`, `long.bin`, `ext.bin` | 5, 4 and 3 separate extents with short, long and extended allocation descriptors (on the non-physical volumes `short.bin` uses long descriptors, since its data is in the physical partition) |
| `cont.bin` | 70 extents (300 with 2048-byte blocks and up): three descriptors in the file entry, the rest in a chain of allocation extent descriptors |
| `sparse.bin`, `sparse_tail.bin` | recorded, unallocated (type 2) and allocated-but-unrecorded (type 1) extents; the unrecorded blocks hold `0xA5` on the disc and must read as zeros; a file ending in an unallocated extent |
| `hidden.txt` | hidden characteristic and implementation use bytes in the file identifier |
| `deleted.txt` | deleted characteristic: not in the manifest, must not be listed |
| `hardlink.bin` | a second name for the ICB of `short.bin` |
| `Dir16 日本語 😀/...` | a 16-bit directory name with a surrogate pair, 8-bit and 16-bit names inside, and a directory embedded in its ICB |
| `Many/` | 120 names; the directory is two separate extents and descriptors straddle blocks |
| `EmptyDir`, `deep/d0/.../d11/bottom.txt` | an empty embedded directory; 13 levels, alternately embedded |

Unallocated VAT entries (`0xFFFFFFFF`) are interleaved in the tables. Not produced: a damaged main metadata file (the generator's `badmain` variant is not among the fixtures), strategy 4096, named streams.

## manifest.tsv

Columns `set`, `kind`, `path`, `size`, `fnv64` (FNV-1a 64, hex). `path` is the expected entry path below the volume, exactly as created (precomposed, case kept).

| `kind` | Expected |
|---|---|
| `data` | a leaf `<path>` with these bytes |
| `empty` | a leaf of zero bytes (as `Iso9660Container` lists empty files) |
| `symlink` | not listed |
| `dir` | an empty directory: not an entry, and nothing listed below it |

Sets: `full` (`udf-only-150`), `small` (`udf-only-102`, and `bridge` by its Joliet names), and one set per synthetic image, named like the image (`syn-physical-512`, ...).

## Not covered, and why

Nothing can be written into a UDF volume on this system without mounting it, and mounting is ruled out. Therefore:

- **Populated virtual (VAT), sparable and metadata-partition volumes written by a tool** do not exist here; from a real writer those partition types are covered only by the empty volumes above, and populated ones only by the synthetic images. Likewise populated volumes with 512-, 1024- or 4096-byte blocks, with extended file entries, or of UDF 2.00-2.60.
- **Multi-extent, fragmented and sparse files, long and extended allocation descriptors, allocation extent descriptors (continuations), data embedded in an ICB**: `makehybrid` writes one short allocation descriptor per file and never embeds, and a hole in a source file comes out as recorded zeros. A file above 1 GiB would need a second descriptor but does not fit here. Only the synthetic images and the reader's unit tests have them.
- Deleted entries, device nodes, named streams, extended attributes with content.
- `newfs_udf -m var-packet`, `-m fix-packet` (any access type) answer "Could not find a device with media inside" and leave the file empty; `-m pow` writes nothing to a regular file either. No pseudo-overwrite volume.
- Raw 2352-byte sector dumps: `hdiutil` cannot write them. The test wraps `udf-only-102.iso` in MODE1 sectors itself (sync, address, mode byte, zero EDC/ECC).
- A sparing table that actually remaps a packet, a VAT that remaps anything beyond its two initial entries, a damaged main metadata file that forces the mirror: no tool-written image has them. `syn-sparable-1024` and the two `syn-vat*` images do the first two; the mirror fallback is covered by the reader's unit tests only.
