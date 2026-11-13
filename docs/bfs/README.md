# Be File System (BFS) specification: index

This directory specifies the on-disk format of the Be File System, as far
as a **read-only** reader needs it, for volumes written by BeOS Preview
Release, R3, R4, R4.5 and R5 (Intel little-endian and PowerPC big-endian),
by Dano/Zeta, and by Haiku. The reader in this repository is implemented
from these documents rather than from existing code.

## 1. Purpose and provenance

- These documents are **prose specifications**. They were derived from
  the published description of the file system by its author, from the
  behaviour and on-disk definitions of an existing open implementation
  (Haiku's BFS add-on, MIT licence; the *reference implementation*), from
  volumes built with the tool compiled from it (`bfs_shell`), and from
  measurements on volumes written by BeOS itself.
- The work is a **clean-room process**. The writers of these documents
  could read the sources listed below; the implementation is written
  **from these documents only**, by implementers who do not read the
  source of any BFS implementation. The documents therefore contain no
  program code and no pseudo-code, only prose, tables of constants and
  offsets, worked examples and test vectors. Reference sources, tool
  builds and downloaded disk images are kept outside this repository.
- Where a source is wrong, incomplete or unsafe, the document says so in
  its section "Observed implementation gaps" and specifies the correct
  behaviour instead. Nothing in that section is to be imitated.
- Statements carry evidence labels, defined in section 1.2 of
  [`bfs.md`](bfs.md). A statement labelled **DECISION** is a choice made
  by the specification, not a fact about the format.
- Real disk images and the results expected for them are kept outside the
  repository and are never committed.

## 2. Documents

| Document | Contents |
|---|---|
| [`bfs.md`](bfs.md) | The whole reader: where a volume is found (bare images, MBR and Apple Partition Map partitions, CD tracks), the superblock and its checks, byte order, block runs, the i-node, the data stream (direct, indirect and double-indirect ranges), the small-data area and attributes, the B+tree file used by directories, attribute directories and indices (including duplicate fragments and duplicate nodes), symbolic links, what is not listed, differences between writers, worked byte-level examples, test vectors, safety requirements. |

Other documents in this directory add their own row here.

## 3. Sources consulted

| Source | Licence / status | Used for |
|---|---|---|
| Dominic Giampaolo, *Practical File System Design with the Be File System*, Morgan Kaufmann 1999 (PDF distributed by the author and mirrored at `https://www.haiku-os.org/legacy-docs/practical-file-system-design.pdf`; read from the Internet Archive copy of `http://www.nobius.org/~dbg/practical-file-system-design.pdf`) | Copyrighted book, made freely available by its author | Design, structure definitions of chapters 4, 5 and 7, the rules for indirect and double-indirect ranges, duplicate handling. Called **the book**. |
| Haiku, `src/add-ons/kernel/file_systems/bfs/` (`bfs.h`, `bfs_endian.h`, `Volume.cpp`, `Volume.h`, `Inode.cpp`, `Inode.h`, `BPlusTree.cpp`, `BPlusTree.h`, `Utility.h`, `Journal.cpp`, `kernel_interface.cpp`, `bfs_disk_system.cpp`, `ToDo`), `src/add-ons/kernel/file_systems/shared/QueryParserUtils.cpp`, `headers/posix/sys/stat.h`, `headers/os/support/TypeConstants.h`, the partition-type tables of the `intel`, `apple` and `gpt` partitioning systems, `docs/develop/file_systems/befs/resources.rst`; `https://git.haiku-os.org/haiku`, revision `a8694efb7c64c84072a83d405531bed559680bb8` (12 November 2026) | MIT | Offsets, constants, validity checks, traversal behaviour. Called **the reference implementation**. |
| Linux kernel, `fs/befs/` (`befs_fs_types.h`, `super.c`, `linuxvfs.c`, `datastream.c`, `btree.c`) and `Documentation/filesystems/befs.rst`; `https://github.com/torvalds/linux`, master, November 2026 | GPL-2.0 | Cross-check of offsets and constants; superblock location on PowerPC volumes; behaviour on an unclean log. Read for facts only. Called **the Linux driver**. |
| The Be Book (BeOS R5 developer documentation), Storage Kit and `sys/stat.h` descriptions | Be, Inc. documentation | Meaning of attribute type codes and of the index mode bits. From the specification author's knowledge of the documentation; nothing depends on it that is not also confirmed by one of the sources above or by data. |

## 4. Tools and data used for the experiments

| Tool or data | Use |
|---|---|
| `bfs_shell`, built from the Haiku revision above with `configure --host-only` and `jam "<build>bfs_shell"` in a Debian (stable, arm64) container | Building purpose-made little-endian volumes with block sizes 1024, 2048, 4096 and 8192: multi-level directory trees, fragmented files reaching the indirect and double-indirect ranges, long and short symbolic links, duplicate names for the name index. |
| A throwaway reader written by the specification's author from the rules of `bfs.md` (kept outside the repository; not part of the specification) | Checking every rule against the volumes below. Its evidence is that every directory tree it walks is consistent in the ways listed in `bfs.md` section 1.2 and that the files it extracts from the purpose-made volumes have the SHA-256 of the files that were copied in. |
| BeOS 5 Personal Edition, `image.be` (524,288,000 bytes, SHA-256 `d77b444282533701c44491bab88004063b074268d21901e1530b91551a66e66a`, from the installer `beospe.exe` at `https://archive.org/details/beospe`, SHA-256 of the installer `8459704eae2cf7b7dbe61a62aca23ae76903fdbb2446b442a1bae1a95ba16553`) | A little-endian volume written by BeOS R5: 2,677 i-nodes reachable from the root. |
| BeOS Release 3 for PowerPC CD (`https://archive.org/details/be-os-3`, `BeOS_3.iso`, SHA-1 `6765a2e6af08e76b29e9eee4884183bcf3b0ffba` as published by the archive), the `Be_BFS` partition of its Apple Partition Map (bytes 67,289,088 to 545,395,199 of the file; SHA-256 of the partition `09c4806e5c35dc2b420b9abc826034ba290b2335a53aedbe9223369f0057ac54`) | A big-endian volume written by BeOS R3. |
| BeOS 5.0.3 Professional CD (`https://archive.org/details/beos-5.0.3-professional-gobe`), first sectors of tracks 2 and 3 only, together with its cue sheet and the cue sheets of four other BeOS x86 CDs in the same archive | Where BFS volumes sit on BeOS CDs; superblock position per platform. |
| BeOS 1.1d7.1 for BeBox CD (`https://archive.org/details/be-os-1-1d-7-1`), first 16 KiB only | What a pre-BFS disc looks like (`bfs.md` section 12). |

| BeOS Advanced Access Preview Release CD for PowerPC and a Haiku nightly image (hrev60410, x86_64), as held in the project's specimen cache outside the repository (their origins are recorded there) | Cross-check after writing: a 1997 big-endian volume and a volume written by Haiku itself (`bfs.md` section 1.1). |

None of these images is in the repository.
