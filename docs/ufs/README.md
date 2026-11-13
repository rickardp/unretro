# NeXT disk label and UFS1 specifications: index

This directory specifies, as far as a **read-only** reader needs them,
the NeXT disk label (the partition table of NEXTSTEP, OPENSTEP and
Rhapsody media) and the UFS1 file system (the Berkeley Fast File System
in its 4.2BSD/4.3BSD and 4.4BSD generations, in both byte orders). The
reader in this repository is implemented from these documents rather
than from existing code.

## 1. Purpose and provenance

- These documents are **prose specifications**. They were derived from
  published descriptions of the formats, from the on-disk definitions
  and the behaviour of existing open implementations (listed in
  section 3), from volumes built with an existing tool (`makefs`), and
  from measurements on media written by NEXTSTEP, OPENSTEP and Rhapsody
  themselves.
- The work is a **clean-room process**. The writers of these documents
  could read the sources listed below; the implementation is written
  **from these documents only**, by implementers who do not read the
  source of any disk-label or UFS implementation. The documents
  therefore contain no program code and no pseudo-code, only prose,
  tables of constants and offsets, worked examples and test vectors.
  Reference sources, tool builds and downloaded media images are kept
  outside this repository.
- Where a source is wrong, incomplete or unsafe, the document says so in
  its section "Observed implementation gaps" and specifies the correct
  behaviour instead. Nothing in that section is to be imitated.
- Statements carry evidence labels, defined in section 1.2 of each
  document. A statement labelled **DECISION** is a choice made by the
  specification, not a fact about the format.
- Real media images and the results expected for them are kept outside
  the repository and are never committed.

## 2. Documents

| Document | Contents |
|---|---|
| [`disklabel.md`](disklabel.md) | The NeXT disk label in its three versions (`NeXT`, `dlV2`, `dlV3`): where the four copies live on floppies, hard disks, optical disks and CD-ROMs, every field with offset, size and byte order, the checksum and the bytes it covers, partition entries, the front porch, device sectors versus label sectors, the label inside a PC partition of type 0xA7, how each partition's byte range is computed, worked byte-level examples, test vectors, safety requirements. |
| [`ufs.md`](ufs.md) | The UFS1 reader: the superblock and the checks that make recognition safe, byte-order detection, fragment and block addressing (including media with 1024- and 2048-byte sectors), cylinder groups and inode location with the old group stagger, the inode in both generations, direct and indirect blocks, holes, the last-block fragment rule, directory records in both formats and both byte orders, removed entries, inline and ordinary symbolic links per writer, hard links, special files, the Linux `nextstep`, `nextstep-cd` and `openstep` variants, recognising UFS2 so that it is rejected, worked examples, test vectors, safety requirements. |

Other documents in this directory add their own row here.

## 3. Sources consulted

| Source | Licence / status | Used for |
|---|---|---|
| M. K. McKusick, W. N. Joy, S. J. Leffler, R. S. Fabry, "A Fast File System for UNIX", ACM Transactions on Computer Systems 2(3), 1984; and M. K. McKusick, K. Bostic, M. J. Karels, J. S. Quarterman, *The Design and Implementation of the 4.4BSD Operating System*, Addison-Wesley 1996, chapters 7 and 8 | Published paper and book | Design: blocks and fragments, cylinder groups, the inode and its indirect blocks, directory chunks, inline symbolic links. From the specification author's knowledge of these texts; nothing depends on them that is not also confirmed by one of the sources below or by data. |
| NetBSD, `sys/ufs/ffs/fs.h`, `sys/ufs/ufs/dinode.h`, `sys/ufs/ufs/dir.h`, `sys/ufs/ffs/ffs_vfsops.c`, `sys/sys/bootblock.h` (the next68k label), `sys/arch/next68k/include/disklabel.h`, `sys/arch/next68k/next68k/disksubr.c`; `https://github.com/NetBSD/src`, trunk, November 2026 | BSD (2- and 3-clause) | Superblock, inode and directory definitions; the 4.2BSD superblock layout test; Apple/NeXT particularities (1024-byte directory chunks, inline links on old-generation volumes); a second definition of the NeXT label and of its checksum. |
| Apple, xnu-123.5 (Mac OS X 10.0): `bsd/dev/disk_label.h`, `bsd/dev/disk.h`, `bsd/sys/disktab.h`, `bsd/ufs/ffs/fs.h`, `bsd/ufs/ufs/dinode.h`, `bsd/ufs/ufs/dir.h`, `iokit/Families/IOStorage/IONeXTPartitionScheme.cpp`; `https://github.com/apple-oss-distributions/xnu`, tag `xnu-123.5` | Apple Public Source License 1.1 | The label structure as NeXT defined it; label validation and partition placement in Mac OS X; the file system definitions of the NeXT lineage; the statement that Mac OS X volumes are big-endian on every processor. |
| Apple, Darwin 0.3 (Rhapsody-era sources): `driverkit-3/libDriver/IODiskPartition.m`, `driverkit-3/libDriver/label_subr.c`, `driverkit-3/libDriver/i386/disk_label.c`, `boot-2/i386/libsaio/disk.c`, `diskdev_cmds-1/disk.tproj/disk.c`, `hd.c`, `fd.c`; read from the mirror `https://github.com/neozeed/Darwin_0.3` | Apple Public Source License 1.x | Where the four label copies are written and read, the copy that is skipped on Intel, the checksum (with the self-position taken as zero), big-endian labels on Intel, the label inside a PC partition of type 0xA7, absolute partition bases, front porch sizes. |
| Apple, boot-80.1: `i386/libsaio/legacy/fdisk.h`, `i386/libsaio/legacy/disk.h`; `https://github.com/apple-oss-distributions/boot` | Apple Public Source License | The PC partition type 0xA7. |
| Linux kernel, `fs/ufs/ufs_fs.h`, `fs/ufs/super.c`, `fs/ufs/inode.c`, `fs/ufs/dir.c`, `fs/ufs/util.h`, and `Documentation/admin-guide/ufs.rst`; `https://github.com/torvalds/linux`, master, November 2026 | GPL-2.0 | The `nextstep`, `nextstep-cd` and `openstep` variants; magic numbers of look-alike formats; the `blocks` = 0 test for inline links; the superblock search past the front porch. Read for facts only. |

| The Sleuth Kit, `tsk/fs/tsk_ffs.h`; `https://github.com/sleuthkit/sleuthkit` | IBM Public License / Common Public License | Fetched as a further cross-check of superblock and inode field offsets; nothing in the documents rests on it alone. |

No NeXT developer documentation or emulator documentation was needed in
the end; the media themselves answered the questions those would have
been consulted for.

## 4. Tools and data used for the experiments

| Tool or data | Use |
|---|---|
| `makefs` 20190105 (Debian 13 package `makefs` 20190105-3, a port of NetBSD's `makefs`), run in a Debian container | Building the crafted UFS1 volumes of `ufs.md` section 13.1 in both byte orders and with three block/fragment size pairs, and two UFS2 volumes. |
| A throwaway reader written by the specification's author from the rules of the two documents (kept outside the repository; not part of the specification) | Checking every rule against the volumes below. Its evidence is that every label checksum verifies, that every allocated inode and every directory of the volumes can be read consistently, and that the files it extracts from the crafted volumes have the SHA-256 of the files that were copied in. |
| NEXTSTEP 3.2 CD-ROM image (315,047,936 bytes, SHA-256 `5c091635340632ce1a609eccfeb94b2bc8404ea49659fdaa4f1d92525747b1ac`); called NS32-CD | A complete big-endian old-generation volume with 2048-byte fragments: 15,548 inodes, staggered cylinder groups, inline and ordinary symbolic links, devices, hard links, double indirect blocks. |
| NEXTSTEP 3.2 floppies (1,474,560 bytes each): boot floppy for NeXT hardware (SHA-256 `4688dc64e04c04994599bf8819c9e83ea63480f0131d470220a3b6641ce8e259`; NS32-FD-68K), boot floppy for Intel (`1cc2a0b25264280ab58fc202ea3dcfe26e594565b73639b2e8545eaac0151c64`; NS32-FD-X86a), driver floppy for Intel (`8236efeca6ebcbe014cf60cd192dd138924d11b77730c8b943ef9c3e8c11f465`; NS32-FD-X86b) | Labels at byte 0 and behind a PC boot sector; big-endian volumes on Intel media; 1024-byte fragments and directory chunks. |
| OPENSTEP 4.2 floppies (1,474,560 bytes each): install floppy for NeXT hardware (`15fd5c28b00bd8411170272418302089d77b739de63ff238abe226a1a715ceeb`; OS42-FD-68K), install floppy for Intel (`58e391565c3dab98e01a8e5ff0c2a67ecbc67e0e5376a211a4a40761f20a238a`; OS42-FD-X86a), driver floppy for Intel (`cdadbf6ab28572e69543144452193ecc70939973626f46b38910068aabbc7cf2`; OS42-FD-X86b) | The same for OPENSTEP. |
| Rhapsody DR2 (5.1) floppies for Intel: boot (`587e7f55f338c8750fc5c74391285f5136a7f8b8bdee273f74a610af8501769c`; RH-FD-X86a) and drivers (`44e345fc12dd3b4fdbcc74e135189e62638d51484ffe93572f0b402655850afa`; RH-FD-X86b), from `https://archive.org/download/rhapsody5.1/` | Little-endian new-generation volumes written by an Apple system; type string `4.4BSD`. |
| First 8,388,608 bytes of the OPENSTEP 4.2 user CD-ROM (`26ffecc483acb87cf3187c235b61c242b7294d8a5930cdedfcd9af0e51b4992e`; OS42-CD), from `https://archive.org/download/Openstep42/` | A label that is not at byte 0 and uses 2048-byte device sectors; a truncated volume. |

The images are held in the project's specimen cache outside the
repository; the cache records where each came from. None of them is in
the repository.
