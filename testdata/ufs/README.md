# UFS1 and NeXT disk label fixtures

The UFS1 volumes here were written by existing tools used as black boxes: NetBSD's `makefs` and the NetBSD 10.2 kernel. No byte of them was edited afterwards. The `label-*` images are synthetic: no current tool writes a NeXT disk label, so a throwaway script wrapped tool-made volumes in labels (see NeXT disk labels below). All file contents are generated (numbered word-list lines, numbered chunk headers padded with zeros, short fixed strings) by a throwaway Python script that is deliberately not part of this repository.

`manifest.tsv` holds what a reader has to produce. It was built from what two independent readers found in the finished images, not from what the script meant to write.

Every image is gzip-compressed (level 9, no name, no time stamp). The images are about 330 KB together, the manifest 265 KB.

## Tools

| Tool | Version | Used for |
|---|---|---|
| `makefs` | 20190105 (Debian 13.7 package `makefs` 20190105-3+b1, a port of NetBSD's `makefs`), in a `debian:stable-slim` container | the `makefs-*`, `ufs2-*` and wrapped `tiny` volumes |
| NetBSD | 10.2 `GENERIC64` (`evbarm-aarch64/binary/gzimg/arm64.img.gz`), under `qemu-system-aarch64` 11.0.0 (`-M virt`, HVF) | `newfs`, `vnconfig`, kernel FFS, `fsck_ffs`, `dumpfs` for the `netbsd-*` volumes |
| The Sleuth Kit | 4.12.1 (Debian package `sleuthkit` 4.12.1+dfsg-3): `fls -r -p -m`, `icat`, `fsstat`, `-f ufs1` | first reader |
| Linux `ufs` driver | kernel 6.12.116 (Debian 13 `linux-image-arm64`), busybox `tar`, in a qemu virtual machine; image attached read-only through a loop device | second reader |
| Python | 3.13.5 (container), 3.14.4 (host, macOS 27.0.1 arm64) | content tree, label wrapper, hashing, gzip |

## UFS1 volumes

| Fixture | Decoded | Writer | Order | Block / fragment | Generation | Groups | Names |
|---|---|---|---|---|---|---|---|
| `makefs-be-4k.img.gz` | 5263360 | `makefs` | big | 4096 / 512 | 4.4BSD | 1 | 420 |
| `makefs-le-8k.img.gz` | 1835008 | `makefs` | little | 8192 / 1024 | 4.4BSD | 7 | 419 |
| `makefs-be-16k.img.gz` | 3145728 | `makefs` | big | 16384 / 2048 | 4.4BSD | 1 | 419 |
| `makefs-le-nofrag.img.gz` | 6291456 | `makefs` | little | 4096 / 4096 | 4.4BSD | 2 | 415 |
| `netbsd-old-be-8k.img.gz` | 4194304 | NetBSD kernel, `newfs -O 0` | big | 8192 / 1024 | old | 4 | 536 |
| `netbsd-old-le-4k.img.gz` | 4194304 | NetBSD kernel, `newfs -O 0` | little | 4096 / 512 | old | 4 | 537 |
| `netbsd-44-be-4k.img.gz` | 4194304 | NetBSD kernel, `newfs -O 1` | big | 4096 / 512 | 4.4BSD | 4 | 537 |
| `netbsd-44-le-8k.img.gz` | 4194304 | NetBSD kernel, `newfs -O 1` | little | 8192 / 1024 | 4.4BSD | 4 | 532 |

"4.4BSD" means superblock `inodefmt` 2, `maxsymlinklen` 60, `postblformat` 1: directory records with a type byte, symbolic links shorter than 60 bytes inside the inode. "old" means `inodefmt` -1, `maxsymlinklen` 0, `postblformat` 1: directory records with a 16-bit name length in the volume's byte order and no type byte, every symbolic link in a data block. On all eight `cgoffset` is 0 and `cgmask` is 0xFFFFFFFF.

Not covered by any tool-made fixture, because no current tool writes them: the 4.2BSD superblock layout of NEXTSTEP (`postblformat` -1), staggered cylinder groups (`cgoffset` not 0), 1024-byte directory chunks, and inline symbolic links on an old-generation volume. Only the real media in the specimen cache have these.

### Commands

`makefs` (in the container; `T` is the generated tree):

```
makefs -t ffs -T 1000000000 -o version=1,bsize=4096,fsize=512 -B be makefs-be-4k.img T
makefs -t ffs -T 1000000000 -o version=1,bsize=8192,fsize=1024,maxbpcg=256 -M 1792k -B le makefs-le-8k.img T
makefs -t ffs -T 1000000000 -o version=1,bsize=16384,fsize=2048 -M 3m -B be makefs-be-16k.img T
makefs -t ffs -T 1000000000 -o version=1,bsize=4096,fsize=4096 -M 6m -B le makefs-le-nofrag.img T
makefs -t ffs -T 1000000000 -o version=2,bsize=4096,fsize=512 -M 1m -B le ufs2-le.img U
makefs -t ffs -T 1000000000 -o version=2,bsize=4096,fsize=512 -M 1m -B be ufs2-be.img U
```

NetBSD (in the guest, once per image; the tree arrives as a GNU tar with sparse members on a second virtual disk):

```
newfs -F -s 4096k -O 0 -B be -b 8192 -f 1024 -n 1024 netbsd-old-be-8k.img
newfs -F -s 4096k -O 0 -B le -b 4096 -f 512  -n 1024 netbsd-old-le-4k.img
newfs -F -s 4096k -O 1 -B be -b 4096 -f 512  -n 1024 netbsd-44-be-4k.img
newfs -F -s 4096k -O 1 -B le -b 8192 -f 1024 -n 1024 netbsd-44-le-8k.img

vnconfig vnd0 IMAGE; mount /dev/vnd0a /mnt
tar xpf /dev/rld5c -C /mnt --strip-components 1 TREE
mkdir /mnt/churn
  120 times: echo "kept N" > churn/kept-N; echo "gone N" > churn/gone-with-a-longer-name-N
rm /mnt/churn/gone-*
mv /mnt/churn/kept-7 /mnt/churn/renamed-from-kept-7
mkdir /mnt/churn/tmpdir; echo x > /mnt/churn/tmpdir/x; rm -r /mnt/churn/tmpdir
dd if=/mnt/big/single.bin of=/mnt/churn/shrunk.bin bs=1k count=100
dd if=/dev/null of=/mnt/churn/shrunk.bin bs=1 seek=3000
rm /mnt/many/file-000.txt /mnt/many/file-17[0-9].txt
umount /mnt; vnconfig -u vnd0
fsck_ffs -n -f IMAGE
```

`fsck_ffs` reports no problem on any of the four. The kernel mounted the non-native byte order (the guest is little-endian) through its `FFS_EI` support.

### Contents

One tree, with B the block size and F the fragment size of the volume it is made for:

| Path | Exercises |
|---|---|
| `README.txt` (1500 bytes), `.hidden`, `empty.txt` (0 bytes) | plain files, an empty file |
| `docs/readme.txt`, `docs/notes/deep/deeper/nested.txt`, `emptydir` | nested directories, an empty directory |
| `many/file-000.txt` ... `file-349.txt` | 350 files of 14 bytes (`many file NNN` and a line feed) in one directory |
| `big/direct.bin` (40000), `big/single.bin` (300000; 346608 when B is 16384) | direct blocks only; one single indirect block |
| `big/double.bin` (4265170), only in `makefs-be-4k` | a double indirect block: 12 + 1024 + 5 blocks and 1234 bytes |
| `sizes/one-byte`, `frag-minus-1`, `frag-exact`, `frag-plus-1`, `block-exact`, `block-plus-1`, `direct-max` (12 x B), `direct-max-plus-1` | every boundary of the fragment tail and of the direct block list |
| `links/original.txt`, `links/hardlink.txt`, `links/sub/third.txt` | three names of one 5000-byte file |
| `links/short-symlink` (`original.txt`), `dir-symlink` (`sub`), `dangling` (`/nonexistent/target`) | short symbolic links: inside the inode on the 4.4BSD volumes, in a fragment on the old ones |
| `links/sym59`, `sym60`, `sym61` | targets of 59, 60 and 61 bytes around the inline limit of 60 |
| `links/long-symlink` | a 209-byte target, always in a data fragment |
| `names/long-...text` (200 bytes), `names/NNNN...end-of-255-name` (255 bytes) | long names |
| `names/with space.txt`, `colon:back\slash.txt`, `Ångström café.txt` (precomposed UTF-8), `日本語.txt`, `a`, `UPPER.TXT`, `upper.txt` | spaces, `:` and `\`, UTF-8, a one-letter name, names differing in case only |
| `names/latin1-caf<E9>.txt` | a name with the single byte 0xE9, which is not valid UTF-8 |
| `perms/mode-0000`, `0400`, `0001`, `0644`, `0755`, `0777`, `4755`, `2755`, `6711`, `1644`, `7777` | permission bits including setuid, setgid and sticky |
| `perms/owner-501-20`, `owner-65534`, `owner-100000` | owners 501:20, 65534:65534 and 100000:70000 (the last does not fit 16 bits) |
| `perms/sticky-dir` (1777), `perms/private-dir` (0700), each with `inside.txt` | directory modes |
| `special/fifo`, `special/chr-2-2`, `special/blk-7-1` | a FIFO, a character and a block device; not in `makefs-le-nofrag` and `netbsd-44-le-8k` |

Only in the `netbsd-*` images:

| Path | Exercises |
|---|---|
| `sparse/hole-middle.bin` (21 x B - 100) | data in block 0 and block 20, a hole between them that spans the direct list and the single indirect block |
| `sparse/hole-start.bin` (6.5 x B) | five blocks of hole, then data |
| `sparse/hole-end.bin` (30 x B + 7) | one block of data, then hole to the end, past the direct list |
| `sparse/all-hole.bin` (3 x B + 5) | no block at all |
| `sparse/frag-tail.bin` (3 x B + F + 10) | 100 bytes, two blocks of hole, a tail in fragments |
| `sparse/double-sparse.bin` (8462015), only in the two 4096-byte-block images | 300 bytes at 0, one block at block 12 + 1024 + 3 and 1000 bytes at block 12 + 1024 + 1024 + 5: a double indirect block whose second-level blocks are mostly absent |
| `churn/kept-N` (119 files), `churn/renamed-from-kept-7`, `churn/shrunk.bin` (3000) | a directory that had 240 entries and lost half: the removed names `gone-with-a-longer-name-N` and `tmpdir` remain in the directory blocks as slack and must not be listed; a file truncated from 100 KB |
| `many` | has 339 files; `file-000.txt` and `file-170.txt` to `file-179.txt` were removed |

All time stamps are 1000000000 in the `makefs` images.

### Verification

1. The Sleuth Kit walked every image (`fls -r -p -m`) and extracted every regular file (`icat`).
2. The Linux kernel mounted every image read-only (`ufstype=44bsd`, or `ufstype=old` for the two old-generation images) and `tar` copied the tree out.
3. Both give the same set of names, kinds, sizes, permission bits, symbolic link targets and FNV-1a 64 file hashes for the seven images The Sleuth Kit can read. The manifest rows are the Linux reader's.
4. The Sleuth Kit cannot list `netbsd-old-le-4k`: it reads the first byte of the little-endian 16-bit name length as a type and finds no entries. That image has the same tree as `netbsd-44-be-4k`, which both readers agree on, and the Linux reader's rows for the two images are identical.
5. The Sleuth Kit returns zeros for the contents of every inline symbolic link, and for the targets of the short links on `netbsd-old-be-8k`. The targets in the manifest are the Linux reader's, which equal the ones created.

## Images that are not UFS1

| Fixture | Decoded | What it is | Expected |
|---|---|---|---|
| `ufs2-le.img.gz`, `ufs2-be.img.gz` | 1048576 | UFS2 volumes from `makefs` (`readme.txt`, `dir/file.txt`). `makefs` puts the UFS2 superblock at byte 8192, so bytes 9564 to 9567, where the UFS1 magic would be, hold the UFS2 magic `19 01 54 19` and `19 54 01 19`. Neither reader opens them (both look for a UFS2 superblock at byte 65536); they are identified by `makefs`'s option and the magic alone | rejected |
| `decoy-magic-be.img.gz`, `decoy-magic-le.img.gz` | 65536 | zeros with only the UFS1 magic at byte 9564 (`00 01 19 54`, `54 19 01 00`) | rejected |
| `truncated-16k.img.gz` | 16384 | the first 16 KiB of the big-endian 8192/1024 `tiny` volume below: a complete, valid superblock, but the inode table (byte 32768) is beyond the end | rejected (`docs/ufs/ufs.md` 3.4, check 10) |

The three `decoy`/`truncated` files are synthetic.

## NeXT disk labels

Synthetic. The labels were written by a throwaway script from the field table, the checksum rule and the copy positions in `docs/ufs/disklabel.md` (sections 2 to 8), which in turn follow the NeXT label definition published in the NetBSD and Apple Darwin sources. The volumes inside are tool-made and unedited: three `makefs` volumes of 327680 bytes with the same small tree.

```
makefs -t ffs -T 1000000000 -o version=1,bsize=8192,fsize=1024 -M 320k -m 320k -B be tiny-a.img A
makefs -t ffs -T 1000000000 -o version=1,bsize=4096,fsize=512  -M 320k -m 320k -B le tiny-b.img B
makefs -t ffs -T 1000000000 -o version=1,bsize=8192,fsize=2048 -M 320k -m 320k -B be tiny-c.img C
```

Tree: `volume.txt` (`volume tiny-a` and a line feed, and likewise), `payload.txt` (20000 bytes), `empty`, `docs/inner/note.txt`, and `link`, a symbolic link to `volume.txt`.

| Fixture | Decoded | Label | Copies at byte (`label_blkno`) | Sector size, front porch | Partitions |
|---|---|---|---|---|---|
| `label-68k.img.gz` | 753664 | `dlV3`, `Fixture_68k` | 0 (0), 7680 (15), 15360 (30), 23040 (45) | 1024, 96 | `a`: base 0, size 320, bytes 98304 to 425983, `tiny-a`; `b`: base 320, size 320, bytes 425984 to 753663, `tiny-b` |
| `label-x86.img.gz` | 393216 | `dlV3`, `Fixture_x86` | 7680 (15), 15360 (30), 23040 (45); byte 0 holds a boot sector (`eb fe`, `55 aa` at 510, empty partition table) | 1024, 64 | `a`: bytes 65536 to 393215, `tiny-a` |
| `label-v2.img.gz` | 491520 | `dlV2`, `Fixture_v2`, checksum at label offset 7238 | 0, 7680, 15360, 23040 | 1024, 160 | `a`: bytes 163840 to 491519, `tiny-a` |
| `label-v1.img.gz` | 491520 | `NeXT`, `Fixture_v1`, checksum at label offset 7238 | 0, 7680, 15360, 23040 | 1024, 160 | `a`: bytes 163840 to 491519, `tiny-a` |
| `label-cd2048.img.gz` | 491520 | `dlV3`, `Fixture_CD` | 8192 (4) and 24576 (12) only; byte 0 holds `80 00 00 00` | 2048, 80 | `a`: base 0, size 160, bytes 163840 to 491519, `tiny-c` |
| `label-mbr-a7.img.gz` | 523776 | `dlV3`, `Fixture_A7`, inside a PC partition of type 0xA7 that starts at sector 63 (960 sectors) | bytes 7680, 15360, 23040 of the partition (78, 93, 108) | 512, 320 | `a`: base 63, size 640, bytes 196096 to 523775 of the disk (163840 of the partition), `tiny-a` |
| `label-badsum.img.gz` | 425984 | `label-68k` with one partition and one bit flipped in the volume name of all four copies; stored checksums not updated | | | rejected: not a label, and not UFS1 either |
| `label-uninit.img.gz` | 425984 | as above with correct checksums but flag bit 0x80000000 (uninitialised) set in all copies | | | rejected |

The layouts of the first five follow real media: NeXT floppies and hard disks, Intel floppies and whole-disk Intel hard disks, NEXTSTEP 0.8 hard disks (`dlV2`), and the OPENSTEP 4.2 CD. `label-v1` has no real counterpart in the specimen cache. `label-mbr-a7` follows `docs/ufs/disklabel.md` section 10.4 only: no real disk with a 0xA7 partition was found, so its absolute `label_blkno` and `base` are an untested reading of the sources.

Unused partition entries hold the pattern real labels have (`ff` in the numeric fields, zeros elsewhere). The geometry fields hold arbitrary numbers.

Verification: a second throwaway script decoded each label and checked each checksum; the Linux kernel then mounted the volume at the byte offset that decoding gave (`ufstype=44bsd`), and the tree it read equals what The Sleuth Kit read from the unwrapped `tiny` volume.

## manifest.tsv

Stored gzipped, as `manifest.tsv.gz`.

Tab-separated with a header line; one row per name, in the order the directories store them, depth first.

| Column | Content |
|---|---|
| `image` | fixture name without `.gz`; for a label image followed by `#` and the partition letter (`a` is entry 0) |
| `path` | path below the volume root, raw bytes. `\` is written `\\`. A name that is not valid UTF-8 is written with `\xNN` for each byte outside 0x20 to 0x7E; the only one is `names/latin1-caf\xE9.txt` |
| `kind` | `file`, `dir`, `symlink`, `fifo`, `chr`, `blk`, or `rejected` |
| `size` | bytes of a file; length of a symbolic link's target; 0 otherwise |
| `fnv64` | FNV-1a 64 (offset basis `cbf29ce484222325`, prime `100000001b3`) as 16 hex digits, of the file contents or of the link target; empty otherwise. An empty file has `cbf29ce484222325` |
| `mode` | permission bits with setuid, setgid and sticky, four octal digits |
| `target` | target of a symbolic link; `major,minor` of a device as Linux decodes it (reference only) |

| `kind` | Expected of the reader |
|---|---|
| `file` | an entry `<path>` with these contents; every name of a hard-linked file has its own row |
| `dir` | no entry of its own; listed here so that empty directories are known |
| `symlink`, `fifo`, `chr`, `blk` | not listed by a reader that follows `Iso9660Container`; given for reference |
| `rejected` | the image (after gunzip) is not recognised as UFS1 or as a NeXT disk label; `path` is empty |

Sparse files hash as their full length with zeros in the holes.
