# RAR format specifications: index

This directory specifies the parts of the RAR format that the reader in
this repository implements from a written specification rather than from
existing code. Each document is the only source its implementation is
written from.

## 1. Purpose and provenance

- These documents are **prose specifications**. They were derived by
  studying the behaviour of an existing open decoder (the RAR support of
  The Unarchiver's XADMaster library, LGPL; the *reference
  implementation*), by black-box testing of the tool built from it
  (`unar`/`lsar` 1.10.7) with purpose-built archives, and by measurements
  on real archives.
- The official `unrar` source is **not** a source for any of them: its
  licence forbids using it to recreate the RAR algorithm.
- The work is a **clean-room process**. The writers of these documents
  could read the reference implementation; the implementation is written
  **from these documents only**, by implementers who do not read the
  source of any RAR decoder. The documents therefore contain no program
  code and no pseudo-code, only prose, tables of constants, worked
  examples and test vectors. Reference sources are kept outside this
  repository.
- Where the reference implementation is wrong or unsafe, a document says
  so in its section "Observed implementation gaps" and specifies the
  correct behaviour instead. Nothing in those sections is to be imitated.
- Statements carry evidence labels, defined in each document's section 1.
  A statement labelled **DECISION** is a choice made by the
  specification, not a fact about the format.
- Expected results for real archives are kept outside the repository and
  are never committed.

## 2. Documents

| Document | Contents |
|---|---|
| [`rar20.md`](rar20.md) | The RAR 2.0 compression method (unpack versions 20 and 26): bit stream, table transmission, LZ items and their constant tables, the multimedia (audio) predictor, the 1 MiB window, solid groups, files split across volumes, test vectors, safety requirements. |
| [`rar15.md`](rar15.md) | The RAR 1.5 compression method (unpack versions 13 and 15, and every packed file of the old `RE~^` container): bit order, the seven fixed prefix codes and four selector codes, the adaptive place tables, literal, long-match, short-item and stored modes, the 64 KiB window, solid chains, test vectors, safety requirements. |
| [`old-container.md`](old-container.md) | The old RAR 1.3/1.4 archive container (signature `RE~^`) with its 16-bit checksum, comments, solid archives and volumes; for RAR4-format archives, which decoder each unpack-version byte selects and which files share decoder state; how the stored checksum relates to whole and split files; how encrypted entries are recognised and skipped. Additional sources for this document: the technical note shipped with RAR 1.40 and the comments of the Deark utility's RAR module (section 1.1 there). |

| [`rar4-container.md`](rar4-container.md) | The `Rar!` container of RAR 1.50 to 4.x: finding the archive (also inside a self-extracting executable), blocks and their checksums, file headers, names, times, comments, volumes, joining and checking entries, decoder selection and solid chains, encryption as far as recognising it. |
| [`rar29-lz.md`](rar29-lz.md) | The RAR 2.9 compression method (unpack versions 29 and 36), LZ part: blocks, table transmission, items, the 4 MiB window, the interface to PPMd blocks and to filters, members and solid groups. |
| [`rar29-ppmd.md`](rar29-ppmd.md) | PPMd blocks of the RAR 2.9 method: block header, range decoder, the model and its memory management, the control sequences layered on the symbols. |
| [`rar29-filters.md`](rar29-filters.md) | Filters of the RAR 2.9 method: filter records, the program store, scheduling, the six standard programs, the virtual machine. |
| [`rar5-container.md`](rar5-container.md), [`rar5-compression.md`](rar5-compression.md) | The RAR 5 container and compression method. |

Other documents in this directory add their own row here.

A document may end with a section "Clean-room feedback": notes written
by the implementers, in prose, about points where the document was
found unclear, about decisions taken where it leaves room, and about
what black-box experiments showed.
