# BFS (Be File System) fixtures

Every volume here was initialised and populated by Haiku's own BFS code, built as the host tool `bfs_shell`; no byte of a good image was edited afterwards. All file contents are generated (numbered word-list lines, numbered zero-padded blocks, short fixed strings) by a throwaway Python script that is deliberately not part of this repository. All volumes are little-endian. No current tool writes big-endian BFS, so the big-endian case exists only as a real specimen outside the repository (last section).

Tools:

| Tool | Version | Role |
|---|---|---|
| Haiku `bfs_shell`, `addattr`, `listattr`, `catattr` | `https://review.haiku-os.org/haiku` revision `a8694efb7c64c84072a83d405531bed559680bb8` (12 November 2026), built with Haiku's Jam 2.5-haiku-20211029 and gcc 14.2.0 in a `debian:stable-slim` container (Debian 13.7, arm64) | Writer; first reader |
| GRUB `grub-fstest` | 2.12-9+deb13u2 (`grub-common`) | Second reader, independent code |
| Linux `befs` driver 0.9.3 | Debian kernel 6.12.116+deb13-arm64 (and its `-16k` build for the 8192-byte-block volume), booted in QEMU 11.0.0 with a busybox initramfs | Third reader, independent code |
| `sfdisk`, `mkfs.bfs` | util-linux 2.41.5 | MBR around `mbr-eb`; the SCO volume |
| `gzip` | 1.13, `gzip -9 -n` | Packing |

## Images

| Fixture | Size | Decoded | Volume |
|---|---|---|---|
| `bs1024.img.gz` | 52154 | 20 MiB | `Fixture 1024`, 1024-byte blocks, 20480 blocks, 3 allocation groups of 8192 blocks, root directory at group 2 block 0, three bitmap blocks, log of 512 blocks at block 4 |
| `bs2048.img.gz` | 117519 | 12 MiB | `Fixture 2048`, 2048-byte blocks, 6144 blocks, 1 group; holds the large directory |
| `bs4096.img.gz` | 32908 | 8 MiB | `Fixtüre 4096 ✓` (UTF-8 volume name, 17 bytes), 4096-byte blocks, 2048 blocks, 1 group |
| `bs8192.img.gz` | 41292 | 12 MiB | `Fixture 8192`, 8192-byte blocks, 1536 blocks, 1 group, initialised with `noindex`: the superblock's index-directory run is zero |
| `tiny.img.gz` | 6216 | 1 MiB | `Tiny`, 1024-byte blocks, 1024 blocks; the base of the reject table |
| `mbr-eb.img.gz` | 8548 | 3 MiB | MBR with one partition of type `0xEB` at sector 64 (6080 sectors); the partition is the volume `In MBR`, 2048-byte blocks, 1520 blocks, superblock at image byte 33280 |
| `not-bfs-sco.img.gz` | 145 | 32 KiB | Not Be's BFS: an SCO/UnixWare "Boot File System" from `mkfs.bfs` (magic `CE FA AD 1B` at byte 0). Must not be detected as BFS |

In every good volume the superblock is at byte 512 of the volume, bytes 0 to 511 are zero, the log is empty (start = end) and the flags word says clean. SHA-256 of the decoded images:

```
45a70709873783ed11736000bfda479aff5e88073a51aff03ceb81ff081c0de0  bs1024.img
1923fe1318e33e4eaa3977d5c5abe99bb5f73ee6e4216e25a97d361f8a29ed02  bs2048.img
1e3c45790c7e87be2c68002466932fc94a0682896e9997cc8c833483ff5ae402  bs4096.img
1f48238486b7ceaa0b95c5933e3c466fa0427404926cb992eb0bd8186e89bcef  bs8192.img
2177479dbf2e8c1841e98c437b8812d7a439b9d8fce09139f6f6fee75e253ea3  tiny.img
2ba9e37af3b9580406a817ff5eaba68bbdca0a8ede72a9128c2a0e67a0e18b12  mbr-eb.img
28e01238c0b2ab1402d2031222095bb2dc9201f1c182200f5ba6dd4fe2858934  not-bfs-sco.img
```

## Commands

Build of the tools, inside the container (`configure` and `jam` are Haiku's; `<build>` is literal):

```
git clone --depth 1 https://review.haiku-os.org/buildtools   # jam only: make in buildtools/jam, then ./jam0 install
git clone --depth 1 https://review.haiku-os.org/haiku
cd haiku/generated && ../configure --host-only
jam -q "<build>bfs_shell"
jam -q "<build>addattr" "<build>listattr" "<build>catattr"
```

`bfs_shell` mounts the image at `/myfs` and reads commands from standard input; a path that starts with `:` is a path on the host. `S` below means "one `bfs_shell <image>` process fed these lines and `quit`". The host tools keep attributes of host files in an emulation directory (`generated/attributes`), and `cp` inside `bfs_shell` carries them into the volume.

```
truncate -s <size> X.img
bfs_shell --initialize X.img "<volume name>" "block_size <1024|2048|4096|8192>"     # bs8192: second parameter line "noindex"
addattr -t <string|mime|int32|int64|bool> <name> <value> <host file>                 # on the host staging tree
addattr -f <value file> -t raw <name> <host file>
S: cp -r :<staging>/<top-level name> /myfs/<top-level name>                          # one line per top-level name, sorted
S: mkdir, cp, ln -s, rm, rm -r, chmod, sync                                          # the steps described under Contents
S: info ; checkfs -c                                                                 # "0 blocks could be freed" on every volume

# mbr-eb
truncate -s 3M mbr-eb.img
printf 'label: dos\nstart=64, type=eb\n' | sfdisk -q mbr-eb.img
bfs_shell --start-offset 32768 --end-offset 3145728 --initialize mbr-eb.img "In MBR" "block_size 2048"
# every later bfs_shell call on it carries the same two offset options

# not-bfs-sco
dd if=/dev/zero of=sco.img bs=1k count=32
mkfs.bfs -V scovol -F scofs sco.img

gzip -9 -n <each image>
```

## Contents

`bs1024`, `bs2048`, `bs4096` and `bs8192` share one tree; `B` is the block size.

| Path | Exercises |
|---|---|
| `Read Me.txt`, `Docs/readme.txt`, `Docs/Notes/Deep/nested.txt` | plain files, nested folders |
| `empty.txt`, `Attrs/empty-with-attr`, `Empty Folder` | empty files (no block runs), an empty folder |
| `Sizes/one-byte.bin`, `block-minus-one.bin`, `one-block.bin`, `block-plus-one.bin` | 1, `B`-1, `B`, `B`+1 bytes |
| `Sizes/contiguous.bin` | 40 blocks + 123 bytes in a single run |
| `Frag/indirect.bin` | 70 blocks + 55 bytes in 4-block runs: all 12 direct runs are used and 6 more runs sit in the indirect array (the array itself is a 4-block run) |
| `Frag/double.bin` (`bs1024` only) | 2326829 bytes in 4-block runs: 12 direct runs, 512 runs in the indirect array (which is then full), 45 runs in the double-indirect range (one first-level array run, one second-level array, every run 4 blocks long) |
| `Names/Café déjà vu Ångström.txt`, `Names/Café decomposed.txt` | precomposed, and `e` + U+0301 as written; BFS stores name bytes unchanged |
| `Names/日本語のファイル.txt`, `Names/Astral/party 🎉 time.txt` | CJK, a 4-byte UTF-8 sequence |
| `Names/long-name-0123…` and `Names/長い名前長い…` | 255-byte names (ASCII; 85 characters of 3 bytes) |
| `Names/a`, `Names/ leading and trailing space `, `Names/UPPER.TXT` + `Names/upper.txt`, `Names/dots...and.more..`, `Names/.hidden` | 1-byte name, spaces at both ends, case-sensitive pair, dots, dot file |
| `Links/short`, `to-dir`, `absolute`, `dangling`, `utf8 → ziel` | short symlinks, target inside the i-node |
| `Links/len143`, `len144`, `len145` | targets of 143, 144 and 145 bytes: 143 is the longest target Haiku stores inside the i-node; 144 and 145 are stored in a data stream of exactly that many bytes with no terminating zero |
| `Links/long`, `Links/long-over-block` | 319 and 1019 byte targets in a data stream |
| `Modes/exec.sh` 0755, `private.txt` 0600, `readonly.txt` 0444, `setuid` 4755, `closed dir` 0700 with `inside.txt` 0640 | permission bits |
| `Attrs/typed.txt` | six small attributes inside the i-node: `BEOS:TYPE` (`MIMS`), string, int32 (123456), int64, bool, and one with a UTF-8 attribute name |
| `Attrs/large.bin` | `demo:blob` of 3`B`+517 bytes and `demo:block` of `B` bytes, both out of line in the file's attribute directory, plus two small ones |
| `Attrs/many.txt` | 20·`B`/1024 string attributes of 39 bytes (`demo:n000` …): more than the i-node's small-data area holds, so they spill into an attribute directory |
| `Attrs/dir with attrs` | a folder with a small and a 600-byte attribute |
| `Deleted/survivor.txt`, `Deleted/replaced.txt` | live neighbours of the deleted entries; `replaced.txt` was deleted and created again with other contents (`second version`) |

`bs1024` also has `Late/after-ballast.txt`, `Late/Sub/deeper.bin`, `Late/empty` and the symlink `Late/link`. They were written after a ballast file had filled the first allocation group, so their i-nodes and data lie in the second and third groups (live i-nodes per group: 257, 65, 5).

`bs2048` also has `Many`: 1002 files (334 of them empty) and 12 subfolders `subdir 00` … `subdir 11` with one file each, 1014 directory entries in all. Names are 1 to 255 bytes long, ASCII and Japanese mixed, generated from a fixed linear congruential sequence; `!` sorts first and 255 × `z` last. The directory's B+tree has 4 levels and is 140288 bytes long; its file uses all 12 direct runs and the indirect array.

`tiny` and `mbr-eb`: `Read Me.txt`, `Docs/readme.txt`, `Docs/Notes/Deep/nested.txt`, `Docs/link to readme` (symlink to `readme.txt`), `empty.txt`, `Empty Folder`, `Names/日本語のファイル.txt`, `data.bin` (20579 bytes), `typed.txt` with a `BEOS:TYPE` attribute, and a deleted `gone.txt`.

How the fragmentation was made: 640 (`bs1024`) or 40 (others) pairs of a 4-block file and a 1-block file were written, the rest of the volume was filled with one ballast file and 80 small files, the 4-block files were deleted, and the `Frag` files were then copied in, so the allocator had only 4-block holes. All scaffolding was deleted again. A second ballast file then overwrote every free block with zeros and was deleted, and 1400 `chmod` + `sync` pairs on `Modes/private.txt` cycled the log, so that stale metadata of the scaffolding does not bloat the compressed images.

Deleted entries were made last, after the zero fill: `Deleted/gone.txt` (2`B`+10 bytes), `Deleted/gone-empty`, `Deleted/gone dir/inside.txt`, `Deleted/gone dir/sub/deep.txt` and the symlink `Deleted/gone-link` were created in one `bfs_shell` session and removed in the next. Their i-node blocks are still in the image with the in-use flag cleared and the deleted flag set (8 such blocks in `bs1024`, 13 in `bs2048`, 1 in `tiny`), and the log area holds further stale copies of i-nodes (250 blocks with the i-node magic in `bs1024`). A reader that walks the directory trees sees none of them; a reader that scans for i-node magic would.

## manifest.tsv

Stored gzipped, as `manifest.tsv.gz`.

Tab-separated, UTF-8, one header line, no quoting or escaping (no fixture name contains a tab, line feed or backslash). Columns:

| Column | Meaning |
|---|---|
| `image` | fixture name without `.img.gz` |
| `path` | path below the volume root, `/` between components, name bytes exactly as stored |
| `kind` | `dir`, `file`, `empty`, `symlink`, `attr`, `deleted` |
| `size` | byte length of what `fnv64` covers; empty for `dir` and `deleted` |
| `fnv64` | FNV-1a 64 (offset basis `cbf29ce484222325`, prime `100000001b3`), 16 lower-case hex digits; empty for `dir` and `deleted` |
| `mode` | the permission bits as four octal digits; empty for `attr` and `deleted` |
| `detail` | `symlink`: the target text. `attr`: the four-character type code, a space, the attribute name. Otherwise empty |

| `kind` | Expected of the reader |
|---|---|
| `file` | a leaf at `path` with exactly these bytes |
| `empty` | a regular file of 0 bytes; the test accepts it listed with no data or not listed, whichever the container does for other file systems |
| `dir` | not an entry of its own; appears only as a path prefix. `Empty Folder` appears nowhere |
| `symlink` | not listed. `size` and `fnv64` cover the target bytes, for a reader that resolves or reports links later |
| `attr` | not visible in any entry today (the public metadata has no place for attributes). `path` is the file or folder that owns the attribute; `size` and `fnv64` cover the attribute's value bytes, which for `CSTR` and `MIMS` include the terminating zero byte |
| `deleted` | `path` must not be listed |

Each row was written from the bytes, link targets, modes and attribute values the generator had put into the staging tree, not from a reader's output. Row counts: 1697 rows; `file` 827, `empty` 345, `dir` 81, `symlink` 43, `attr` 354, `deleted` 47. The `deleted` rows also name some of the removed scaffolding (`fill`, `fill/x0000`, `ballast0`, `ballast1`, `ballast2`).

## Verification

Readers were run on the finished images, each in a fresh process:

| Reader | What was compared with the manifest | Result |
|---|---|---|
| `bfs_shell` | every top-level name copied out with `cp -r` to the host: set of folders, files, symlinks; file bytes; link targets; permission bits; attribute names and raw values (`listattr`, `catattr -r`); the root listing; `checkfs -c` | equal for all six volumes (files/links/attributes: `bs1024` 39/11/33, `bs2048` 1049/10/53, `bs4096` 35/10/93, `bs8192` 35/10/173, `tiny` and `mbr-eb` 7/1/1); 0 blocks not allocated, already set or freeable |
| GRUB `grub-fstest` (`ls` of every folder, contents of every file; `(loop0,msdos1)` for `mbr-eb`) | names per folder; file bytes; the `deleted` paths cannot be read | equal for all six volumes. GRUB's `ls` leaves out names that start with a dot, so `Names/.hidden` was checked by content only; GRUB reports neither modes nor attributes |
| Linux `befs` (mounted read-only with `iocharset=utf8`, the whole tree written to a tar archive on a second virtual disk) | folders, file bytes, link targets, permission bits | equal, with three known limits of that driver, below |

The `bfs_shell` and GRUB rows were repeated on the committed `.gz` files on 13 November 2026, with the manifest as the only expectation. The Linux run dates from the build on 12 November 2026.

Limits of the Linux driver seen here (they are defects of that reader, not of the images; `bfs_shell` and GRUB agree with the manifest in each case):

- `Frag/double.bin` has the right length and the right bytes up to the end of the indirect range (byte 2146304); in the double-indirect range it returns blocks from the wrong runs.
- A symlink whose target lives in a data stream (144 bytes and more) comes back one byte short.
- It refuses the name with a 4-byte UTF-8 sequence (`Names/Astral/party 🎉 time.txt`), so that one file was not compared.

It also needs a page size of at least the block size (hence the 16 KiB-page kernel for `bs8192`) and logs "ag_shift disagrees with blocks_per_ag" for every volume, including those written by BeOS.

`bfs_shell` always mounts read-write. A session of `info`, `checkfs -c`, `ls` and copying out left five images byte-identical but changed `bs1024`; work on copies.

## rejects.tsv: images that are not BFS

Each of the 18 rows (15 `reject`, 3 `either`) is `tiny.img` with one field of the superblock overwritten. The test builds the variant in memory: take the decoded `tiny.img`, check that the bytes at `offset` (decimal, from the start of the image) equal `original` (hex), and replace them with `patched` (hex, same length).

| Column | Meaning |
|---|---|
| `name`, `base`, `offset`, `original`, `patched` | as above; `base` is always `tiny` |
| `expect` | `reject`: must not be detected as BFS. `either`: detection may accept or refuse, but the open must not panic, loop or allocate without bound, and must list nothing wrong |
| `haiku`, `linux`, `grub` | what the three existing readers did (`refuses`, or `mounts` / `lists`) |
| `reason` | the change in words |

All 18 variants are refused by Haiku. Linux mounts 8 of them and GRUB lists 10, which shows how little those two check; neither is a model for the detector. The superblock fields touched, as byte offsets from the start of the superblock (image offset minus 512): magic 1 at 32, byte-order mark at 36, block size at 40, block shift at 44, block count at 48 (8 bytes), i-node size at 64, magic 2 at 68, blocks per allocation group at 72, allocation group shift at 76, group count at 80, magic 3 at 112, root directory run at 116 (group, 4 bytes) and 120 (start, 2 bytes).

`not-bfs-sco.img.gz` is the unrelated file system that shares the name "BFS" and must fall through to "unknown".

## Not covered

- A big-endian volume. Nothing was byte-swapped to fake one. The real one is `BeOS_Advanced_Access.iso` in the specimen directory.
- A volume with a non-empty log. Killing `bfs_shell` in the middle of a session was tried; the image it left still had log start = end and a clean flag, with only the synced files present, so it adds nothing.
- Hard links (BFS has none), special files, a directory B+tree with duplicate keys (directories cannot have them; only indices do), a B+tree deeper than 4 levels, a double-indirect file at block sizes other than 1024, a first-level double-indirect array with more than one run.
- Volumes written by BeOS itself: i-node flag combinations, attribute layouts and root directories in group 8 differ from Haiku's. See the specimens.

## Real specimens (not in the repository)

Three real volumes were listed with independent tools while this reader was written, each into a manifest in the format above with only `dir`, `file`, `empty` and `symlink` rows. No test reads them; a cut-down copy of the first is in the `unretro-samples` repository (`data/beos/`):

| File | Volume |
|---|---|
| `BeOS_Advanced_Access.iso` | BeOS Advanced Access Preview Release CD for PowerPC, 1997. Apple Partition Map; partition 4 `Be_BFS` at byte 26157056, length 519243264: big-endian BFS `BeOS`, superblock at partition byte 0, 2048-byte blocks. 221 folders, 3456 files |
| `image.be` | BeOS 5 Personal Edition for x86, 2000 (from `beospe.exe`): little-endian BFS `Personal Edition`, superblock at byte 512 behind boot code, 2048-byte blocks. 251 folders, 2168 files, 257 symlinks |
| `haiku-hrev60410-x86_64-bfs.img` | the `0xEB` partition of a Haiku nightly anyboot image: little-endian BFS `Haiku`, 2048-byte blocks, 21 allocation groups. 97 folders, 221 files, 24 symlinks, one file of 79680562 bytes |

Those manifests were written from the Linux driver's output and compared without a difference against `bfs_shell` (for the big-endian volume, a `bfs_shell` built with the `BFS_BIG_ENDIAN_ONLY` switch of its Jamfile) and, for the two little-endian volumes, GRUB.

## Sources consulted for this directory

| Source | Licence | Used for |
|---|---|---|
| Haiku source tree, revision above (`src/tools/bfs_shell`, `src/tools/fs_shell`, `src/add-ons/kernel/file_systems/bfs`) | MIT | built and run as a black box; its command help; its superblock checks, read to choose the reject rows |
| Linux `fs/befs` (`datastream.c`, `super.c`), 6.12 | GPL-2.0 | read to explain the two differences listed under Verification; run as a black box |
| GRUB 2.12, util-linux 2.41.5, QEMU 11.0.0 | GPL | run as black boxes |
