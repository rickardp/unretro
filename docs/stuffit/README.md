# StuffIt format specification: index

This directory specifies how to read StuffIt archives (`.sit`): the two
container generations and the compression methods found inside them. It is
the only source the StuffIt reader in this repository is written from.

## 1. Purpose and provenance

- These documents are a **prose specification**. They were derived by
  studying the behaviour of an existing decoder (a third-party StuffIt
  library, version 0.1.8, called the *reference implementation*), by
  black-box testing of that library and of an independent extraction tool
  (`unar`/`lsar` 1.10.7) with purpose-built inputs, and by measurements on
  real archives.
- The work is a **clean-room process**. The people or agents who wrote
  these documents could read the reference implementation; the
  implementation is written **from these documents only**, by implementers
  who do not read the reference implementation's source. The documents
  therefore contain no program code and no pseudo-code, only prose,
  tables of constants, worked examples and test vectors.
- Where the reference implementation is wrong or unsafe, the documents say
  so (each has a section "Observed implementation gaps") and specify the
  correct behaviour instead. Being compatible with the reference
  implementation is not a goal; reading genuine archives correctly and
  rejecting hostile input safely is.
- Every statement carries an evidence label (section 5). A statement
  labelled **DECISION** is a choice made by this specification, not a fact
  about the format (section 6).
- Synthetic test archives built from these documents are in
  `testdata/stuffit/` (see its `README.md`). Expected results for real
  archives are kept outside the repository and are never committed.

The three surveys of real archives (one per document) were made
separately and selected their archives differently: byte-identical copies
are counted once in some counts and each time in others, and
self-extracting and MacBinary-wrapped files are included in some and not
in others. The corpus sizes quoted in the three documents therefore
differ. Each number is stated with its unit where it appears; none of the
rules depends on which survey is taken.

## 2. Documents and reading order

| Order | Document | Contents |
|--:|---|---|
| 1 | `README.md` (this file) | Index, method table, glossary, decisions, scope. |
| 2 | [`containers.md`](containers.md) | Both container layouts (classic `SIT!` and StuffIt 5), detection, the entry list, names, dates, the fork checksum, **the contract between the container layer and the method decoders (section 2.2)**, method 0 and the framing of method 14, safety requirements for the container layer. |
| 3 | [`lz-methods.md`](lz-methods.md) | Methods 1 (run-length), 2 (LZW), 3 (Huffman) and 13 (LZ77 with Huffman codes), with all constant tables, worked examples and test vectors. |
| 4 | [`arsenic.md`](arsenic.md) | Method 15 ("Arsenic": arithmetic coding, block sorting), with its tables, a fully traced example and test vectors. |

Read `containers.md` sections 1 and 2 first, whatever you implement:
section 2.2 is the single normative statement of what a decoder receives
and returns and of who verifies what. Then:

- for the container layer: `containers.md` sections 3 (classic), 4
  (StuffIt 5), 5 (detection), 6 (safety) and 8 (worked examples);
- for a method decoder: sections 1 and 2 of `lz-methods.md`, then the
  section of the method; or `arsenic.md` from the start.

Sections called "Observed implementation gaps" describe faults of the
reference implementation. They explain why a rule exists; nothing in them
is to be imitated.

## 3. Compression methods

The method number is one byte per fork (classic: the low 4 bits of the
method byte; StuffIt 5: the whole byte). **One numbering serves both
container generations**, and the decoder is selected by the number alone.

| Number | Name | Specified in | Real examples | Fork checksum (CRC-16) verified by the container layer |
|--:|---|---|---|---|
| 0 | Stored (no compression) | `containers.md` section 2.2, step 7 | classic and StuffIt 5 | yes |
| 1 | Run-length (marker byte `90`) | `lz-methods.md` section 3 | none | yes |
| 2 | LZW, 9 to 14 bit codes | `lz-methods.md` section 4 | classic | yes |
| 3 | Huffman, tree stored in the stream | `lz-methods.md` section 5 | classic (one fork) | yes |
| 13 | LZ77 with Huffman codes | `lz-methods.md` section 6 | classic and StuffIt 5 | yes |
| 14 | Deflate, raw RFC 1951 stream | `containers.md` section 2.3 ("Method 14 framing"); the algorithm itself is RFC 1951 and is not restated | none (only archives written by the reference implementation) | yes |
| 15 | "Arsenic" | `arsenic.md` | StuffIt 5 | **no**: the stream carries its own CRC-32, verified by the decoder |
| 5, 6, 8 | "LZAH", "Fixed Huffman", "MW" | not specified; known by name only (`containers.md` section 2.3) | none | fork error "unsupported method" |
| 4, 7, 9 to 12, and above 15 | unassigned | `containers.md` section 2.3 | none | fork error "unsupported method" |

What every decoder has in common (`containers.md` section 2.2):

- It is given the method number, the packed bytes of one fork and the
  declared unpacked length U, and returns exactly U bytes or an error.
- A fork with U = 0 is empty and is never decoded; a fork with no packed
  bytes and U greater than 0 is corrupt. Both are settled by the container
  layer.
- Fewer than U bytes is always an error. Packed bytes left unread are never
  an error. Output beyond U is an error for methods 14 and 15 (whose
  streams end themselves) and cannot arise for methods 1, 2, 3 and 13
  (whose decoders stop at U).
- The container layer verifies the fork checksum after decoding, for every
  method except 15.
- Nothing is delivered as a fork's contents unless every step succeeded.

## 4. Glossary

Terms used with the same meaning in all documents. Where a document needs
a term of its own it defines it locally.

| Term | Meaning |
|---|---|
| archive | One StuffIt archive as a byte sequence starting at its first signature byte. Wrappers around it (MacBinary, BinHex, a self-extracting application) are removed by another layer. |
| classic | The first container generation (StuffIt 1.x to 4.x, signature `SIT!`). |
| SIT5, StuffIt 5 | The second container generation (StuffIt 5 and later; still `.sit`). Not StuffIt X (`.sitx`). |
| container layer | The part of a reader that parses the archive structure, lists entries, locates forks, calls method decoders and verifies fork checksums. Specified in `containers.md`. |
| method decoder, decoder | The part of a reader that turns the packed bytes of one fork into its unpacked bytes for one method number. |
| entry | One file or one folder in the resulting list. |
| record | In the classic container: one 112-byte record header plus the fork data that follows it. A record is a file, a folder start marker or a folder end marker. |
| fork | One of the two byte streams of a classic Mac file: data fork, resource fork. Each is packed separately. |
| packed bytes, packed stream | A fork as stored in the archive. "Packed stream" when viewed as a sequence of bits or symbols. |
| packed length, **P** | The number of packed bytes of a fork, from the entry header. Untrusted. |
| unpacked bytes, output | The original contents of a fork; what a decoder produces. |
| unpacked length, **U** | The length the entry header declares for the unpacked fork. Untrusted: it is a claim to be checked, never an amount of memory to reserve. |
| **L** | The number of archive bytes actually available to the reader (`containers.md`). |
| archive offset | A byte offset from the first byte of the archive. Every offset stored in an archive is one. |
| CRC-16 | The checksum algorithm of `containers.md` section 2.1 (polynomial 0x8005, reflected, initial value 0, no final XOR; `123456789` gives 0xBB3D). Used for all header checksums and for the fork checksum. |
| fork checksum | The 16-bit field an entry header holds for each fork: the CRC-16 of the fork's unpacked bytes. Verified by the container layer. Not used for method 15. |
| header checksum, name checksum, file header checksum, second-block checksum | CRC-16 fields protecting container structures (`containers.md` sections 3.4, 4.2, 4.5, 4.6). |
| stream checksum | The CRC-32 at the end of a method 15 stream (`arsenic.md` section 11.4). Verified by the method 15 decoder. |
| low-bit-first order | Bits of a packed stream are taken from each byte starting with the least significant bit, and a multi-bit field's first bit is its least significant (`lz-methods.md` section 2.1). Methods 2, 13 and 14. |
| high-bit-first order | Bits are taken from each byte starting with the most significant bit. Methods 3 and 15. |
| finding | A warning recorded for the caller; the listing is unaffected (`containers.md` section 1.2). |
| incomplete | The listing stops early or may lack entries; what was listed is delivered together with an indication of damage. |
| fork error | The entry is listed but that fork cannot be delivered. The fork errors are: "encrypted, unsupported", "corrupt", "truncated" (the packed bytes lie partly outside the available bytes, L), "unsupported method", "output limit exceeded", "checksum mismatch" (`containers.md` section 2.2). |
| truncated input, corrupt input | Error kinds inside a decoder (`lz-methods.md` section 2.4; `arsenic.md` section 12.2 has a finer list). The container layer reports all of them as the fork error "corrupt", except a stream checksum mismatch, which it reports as "checksum mismatch". A decoder's own output cap, or memory it cannot obtain (for method 15 this includes a block's working buffers), is neither kind and is reported as "output limit exceeded" (`lz-methods.md` section 2.6; `arsenic.md` section 12.2). |
| not this format | Detection fails; nothing is listed. |
| reference implementation | The third-party StuffIt library, version 0.1.8, that the new implementation replaces. A source of facts, and of faults not to copy. |
| independent tool | `unar`/`lsar` 1.10.7, used only as a black box. `lz-methods.md` calls it `unar`. |
| genuine, real | Written by actual StuffIt software, as opposed to synthetic (built for a test) or written by the reference implementation. |

Symbols: **P**, **U** and **L** mean the same everywhere. Every other
letter is local to the section that introduces it (for example C is a
comment length in `containers.md` section 4.5 and the arithmetic decoder's
code value in `arsenic.md`; R is a resource fork's packed length in
`containers.md` and the arithmetic decoder's range in `arsenic.md`).

Number notation: `containers.md` and `arsenic.md` write hexadecimal values
with a `0x` prefix; `lz-methods.md` writes byte values as two hexadecimal
digits in code style (`90`) and 16-bit checksums as four (`B120`).
Ranges written `a..b` include both ends.

`arsenic.md` uses one term that must not be confused with a shared one:
its **run-coded bytes** are the bytes of one block just before the final
run-expansion stage, not the packed bytes of the fork.

## 5. Evidence labels

Each document defines its labels in its section 1. They correspond as
follows.

| Meaning | `containers.md` | `lz-methods.md` | `arsenic.md` |
|---|---|---|---|
| Checked on real archives | (no label) | CONFIRMED (real data) | (no mark), EMPIRICAL |
| Checked with purpose-built input given to a black box | BLACK-BOX (the independent tool) | CONFIRMED (crafted) (both black boxes) | BLACK-BOX (the independent tool) |
| Stated by the reference implementation only | REFERENCE-ONLY | (said in words) | (said in words) |
| Deduced, unconfirmed | INFERRED | INFERRED | INFERRED |
| Chosen by this specification | DECISION | DECISION | DECISION |

## 6. Decisions

Every place where the format is ambiguous, the existing readers disagree,
or both are more lenient than is safe, and the specification prescribes
one behaviour. None of them changes how a genuine, undamaged archive is
read. Most are marked DECISION where they are specified; a few are parts
of a procedure that is labelled as a whole, or follow from one.

### 6.1 Container layer and decoders (`containers.md`)

| # | Section | Decision |
|--:|---|---|
| C1 | 2.2 | The container layer hands a decoder the method number, the packed bytes and U; the decoder returns exactly U bytes or an error; the container layer verifies the fork checksum. |
| C2 | 2.2 step 1 | A fork with U = 0 is empty: nothing is decoded, the method number is not examined, packed bytes that are present are skipped with a finding. The finding is recorded when the entry is listed, once per fork, and stays a finding under strict checking. |
| C3 | 2.2 steps 2, 5 | Encrypted forks and unknown method numbers give distinct fork errors; packed bytes are never passed through as contents. |
| C4 | 2.2 step 3 | P = 0 with U greater than 0 is "corrupt". |
| C5 | 2.2 step 4; 3.6; 4.8 | A fork whose packed bytes lie partly outside the available bytes (L, in both generations) is "truncated"; the entry is still listed. A classic record that extends beyond the declared archive length ends the listing (incomplete), but its forks are judged against L. |
| C6 | 2.2 step 6; 6 | The caller sets limits on the unpacked size of one fork and of one archive; exceeding either is "output limit exceeded". |
| C7 | 2.2 step 7 | Method 0 requires P = U. |
| C8 | 2.2 step 8 | Short output is an error for every method; unread packed bytes are never an error; output beyond U is an error for methods 14 and 15. |
| C9 | 2.2 step 9 | A fork checksum mismatch is a fork error for every method except 15; for method 15 the field is not compared, in either generation. |
| C10 | 2.2 | No contents are delivered for a fork that ended in a fork error. |
| C11 | 2.3 | The decoder is selected by method number alone in both generations. In particular method 15 in a classic archive is decoded as in StuffIt 5, and methods 1, 2, 3 in a StuffIt 5 archive as in classic. |
| C12 | 2.3 | A StuffIt 5 method byte above 15 is "unsupported method" (the whole byte is the number). |
| C13 | 2.3 | Method 14 is a raw RFC 1951 stream that must end at its final block and produce exactly U bytes; failures are reported as "unsupported method 14 variant or corrupt data". |
| C14 | 2.4 | Every non-zero timestamp is a date, including values before 1970; times are zoneless local times and are not shifted. |
| C15 | 2.6 | StuffIt 5 names and comments: all bytes below 0x80 is ASCII; otherwise well-formed UTF-8 is UTF-8; otherwise MacRoman. Classic names are MacRoman. No normalisation. |
| C16 | 2.6 | Names are single path components and are never split; the safe-component steps for joined paths; duplicate paths are listed unchanged. |
| C17 | 3.4 | Classic name length above 31 is treated as 31 with a finding; a name checksum mismatch is a finding. |
| C18 | 3.6 | A classic record header that is all zero is padding and ends the list. A header checksum mismatch stops the listing (incomplete). |
| C19 | 3.7; 2.2 step 2 | An encrypted classic fork with P below 16 or not a multiple of 8 is "corrupt" rather than "encrypted". The classic record parser decides this when it lists the file and records it as the fork's encryption state. |
| C20 | 4.3 | StuffIt 5 bounds are checked against L; the declared archive length is informational. Entries start at offset 114 or later. |
| C21 | 4.3 | The first entry is found by validating candidate offsets in a fixed order (plain fields first, the XOR-obfuscated readings only when the archive flag allows). The stored parent offset is never used to build paths. |
| C22 | 4.5, 4.6, 4.10 | Key lengths K and KR count only for files with the encrypted flag (a non-zero key length byte without the flag is a finding, for either fork); the flag has no effect on folders; when it is set every fork with P greater than 0 is encrypted. |
| C23 | 4.5 | The entry header validation list; what is and is not validated. |
| C24 | 4.6 | A resource-fork bit on a folder is ignored with a finding, and a folder's second-block checksum covers the second block alone. The second-block checksum is not verified for encrypted entries. |
| C25 | 4.7 | The three-condition end-of-folder marker test; headers of ambiguous form are unusable. |
| C26 | 4.9 | The entry list is defined by links and counts together: chain membership, the visited rule, the extent rule, the outcomes when they disagree, and the order in which the outcomes are tested (a damaged link is reported before a count disagreement). |
| C27 | 4.5, 4.11 | StuffIt 5 header checksum mismatches (file header, entry header, second block) are findings unless the caller asks for strict checking. End-of-folder markers are treated like entries in this respect. Strict checking affects nothing else. |
| C28 | 5 | Detection looks only at offset 0 and uses structural checks, not the signature alone. |
| C29 | 5.3 | Multi-part archives: the reader handles one byte sequence; a caller may join parts end to end. |
| C30 | 6 | Nesting depth limit: at least 32 must be accepted, 64 recommended; what happens at the limit in each generation. |
| C31 | 6, requirement 9 | StuffIt 5 path size limit: the stored name bytes of all components of all listed entries, added up, may not exceed 16 x L; at the limit the listing stops, incomplete. Not applied to classic archives. |

### 6.2 Methods 1, 2, 3 and 13 (`lz-methods.md` section 9.2)

| # | Section | Decision |
|--:|---|---|
| L1 | 2.3 | P = 0 with U greater than 0 is corrupt (same as C4). |
| L2 | 2.4 | Whenever decoding cannot continue before U bytes exist, the result is truncated or corrupt input, never a short success; no contents are delivered. |
| L3 | 2.6 | A U larger than P packed bytes could ever produce is corrupt input, before decoding. |
| L4 | 3.1 | Method 1: the pair `90 01` is corrupt input. (That the specified run-length rule is the genuine one is INFERRED, section 9.1.) |
| L5 | 5.2 | Method 3: a tree with more than 256 leaves (or a 256th branch) is corrupt input; duplicate leaf values are accepted. |
| L6 | 6.6.2 | Method 13: an assigned code length of 32 or more is corrupt input. |
| L7 | 6.6.2 | Method 13: a repeat command that runs past the last symbol is corrupt input. |
| L8 | 6.8 | Method 13: a match distance greater than the number of bytes produced so far is corrupt input. |
| L9 | 6.9 | Method 13: the end-of-data symbol before U bytes exist is corrupt input. |
| L10 | 6.9 | Whatever follows the item that completes U bytes is neither read nor checked. |

### 6.3 Method 15 (`arsenic.md` section 18)

| # | Section | Decision |
|--:|---|---|
| A1 | 1.3 | A method 15 fork in a classic archive is decoded as in StuffIt 5; the fork checksum field is not compared (same as C9, C11). |
| A2 | 1.3 | U = 0 with packed bytes present is an empty fork; the stream is not decoded (same as C2). |
| A3 | 3 | A request for a bit beyond the end of the packed bytes is a failure; no substitute bits are supplied, also after the last checksum symbol. |
| A4 | 6 | A block longer than the block capacity is a failure. |
| A5 | 8 | An origin index not less than the block length, which includes every empty block, is a failure of the whole fork. |
| A6 | 12.2 | Output that would exceed U is a failure (output overflow). |
| A7 | 12.2 | A stream that ends with fewer than U bytes is a failure (length mismatch). |

### 6.4 Points that remain open

They do not block an implementation; the documents say what to do in each
case.

- Whether genuine StuffIt software frames method 14 as a raw RFC 1951
  stream (`containers.md` section 9, point 1).
- Whether the run-length rule specified for method 1 is the one genuine
  software used; no real method 1 fork was available (`lz-methods.md`
  section 9.1).
- The StuffIt 5 offset obfuscation (XOR with 0xA5A5A5A5), never observed
  (`containers.md` section 4.3).
- The layout of encrypted StuffIt 5 entries, known from black-box tests
  only (`containers.md` section 4.10).
- Whether genuine multi-part archives carry per-part headers
  (`containers.md` section 5.3).

## 7. Out of scope

Deliberately not specified, and not to be implemented from these
documents:

- **Encryption.** Encrypted forks are recognised, their entries are
  listed, and each encrypted fork is reported with the fork error
  "encrypted, unsupported". The ciphers and key derivation are not
  specified; no password is accepted.
- **Segmented (multi-part) archives.** A reader handles one byte sequence.
  A single part is read as a truncated archive. Joining parts is left to
  the caller.
- **Writing.** Nothing here describes how to create or modify an archive
  or how to compress. The encoders used to make test vectors are not part
  of the specification.
- **StuffIt X** (`.sitx`) and other formats that merely share the name.
- **Methods 5, 6 and 8**, known by name only, and any unassigned method
  number: reported as "unsupported method".
- **Wrappers.** Locating an archive inside MacBinary, BinHex, a
  self-extracting application or a disk image belongs to the layers that
  understand those formats.
- **Interpreting fork contents** (resource maps, file types) and anything
  the host does with entries: extraction to a file system, file naming
  beyond the safe-component steps, time zone presentation.
- **Salvage of damaged forks.** Permitted as a separately marked facility,
  never required.
