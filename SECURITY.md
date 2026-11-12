# Security Policy

## Supported Versions

| Version | Supported          |
| ------- | ------------------ |
| 0.1.x   | :white_check_mark: |

## Reporting a Vulnerability

If you discover a security vulnerability in unretro, please report it responsibly:

1. **Do not** open a public GitHub issue for security vulnerabilities.
2. Email the maintainer directly or use [GitHub's private vulnerability reporting](https://github.com/ricardolstephen/unretro/security/advisories/new).
3. Include a clear description of the issue, steps to reproduce, and any relevant sample files.

You can expect an initial response within 72 hours. Confirmed vulnerabilities will be patched and disclosed in a timely manner.

## Security Model

unretro is a **read-only** archive extraction library. It does not write files to disk or execute content from archives. The primary attack surface is malformed or malicious archive data.

### Built-in Protections

- **Decompression size limits**: All decompressed data is capped at `MAX_DECOMPRESSED_SIZE` (4 GiB) to prevent zip-bomb-style attacks.
- **Header sizes are not trusted for allocation**: A size declared in an archive header reserves at most 1 MiB up front; buffers grow only as data is actually decoded. Formats that must allocate the declared size in full (UDIF/NDIF disk images, RAR) reject sizes above `MAX_DECOMPRESSED_SIZE` first.
- **Cumulative output budgets**: Formats whose entries can point at the same stored bytes (HFS/MFS forks, resource forks, iMUSE bundles) charge everything they copy out against a budget proportional to the input size (minimum 16 MiB), so aliased entries cannot multiply output. UDIF/NDIF images additionally bound stored and compressed chunks by their backing size; zero-fill chunks are exempt so sparse images still open.
- **Bounded structure traversal**: B-tree node links, directory parent chains, allocation chains and Huffman trees are walked with visited-set or step limits, so cyclic or over-deep structures end in an error rather than a hang or stack overflow.
- **StuffIt**: Parsed and decoded natively with no third-party decoder, from the format specification in `docs/stuffit/`. Classic records are strictly sequential and bounded by the archive length. StuffIt 5 entries are reached through stored offsets, so each entry is listed at most once, the extents of all listed entries together may not exceed the archive length, folder nesting stops at 64 levels and every name is held once however many entries lie below it (joined paths are bounded by 16 times the archive length), and each entry header is validated once however many links point at it; a chain that loops, overlaps or leaves the archive ends the listing with what was found so far. Every fork must decode to exactly its declared length and match its CRC-16 (for method 15 the CRC-32 inside its stream); a declared length that the packed bytes cannot produce is rejected before decoding for methods 1, 2 and 13, and a method 15 block longer than the output still owed is not held in memory. All forks of one archive are charged against a budget of 1024 times the archive length (minimum 16 MiB) before they are decoded. Encrypted forks, unsupported methods and forks failing any of these checks are never delivered, not even partially: the file (or only its resource fork) is skipped with a `ContainerEntriesSkipped` diagnostic while the readable files are still listed.
- **BinHex**: Decoded natively with no third-party decoder. Header, data-fork and resource-fork CRCs must match or the file is rejected; run-length expansion is produced only up to the fork lengths declared in the header, which together may not exceed `MAX_DECOMPRESSED_SIZE`.
- **ZIP**: Parsed and inflated natively with no third-party decoder. The central directory is authoritative; every entry must decode to exactly its declared size, which may not exceed `MAX_DECOMPRESSED_SIZE` and is also the decoder's output limit, and must match its stored CRC-32. Central headers are read from directory bytes that are really present, delimited by the declared directory size; the declared entry count is only a lower bound. Each entry read is charged (compressed size plus local header) against the archive length, so central headers that alias the same local data cannot multiply its output; entries beyond that total are skipped. Encrypted entries, entries on another disk, unknown compression methods and entries failing the size or CRC check are skipped with a `ContainerEntriesSkipped` diagnostic while the readable entries are still listed. Multi-disk archives are rejected.
- **GZIP**: Parsed and inflated natively with no third-party decoder. Only the deflate method is accepted and reserved header flags are rejected; the optional header CRC-16 and each member's CRC-32 and length trailer must match or the file is rejected. All members of a multi-member file are decoded, and their combined output may not exceed `MAX_DECOMPRESSED_SIZE`.
- **BZIP2**: Decoded natively with no third-party decoder, as a `.bz2` file, as ZIP method 12 and as UDIF disk image blocks. Every block CRC and stream CRC must match or the data is rejected, and nothing is delivered partially. All streams of a multi-stream file are decoded, and their combined output may not exceed `MAX_DECOMPRESSED_SIZE`; a ZIP entry or UDIF block is decoded with its declared size as the output limit and must reach exactly that size.
- **TAR**: Parsed natively with no third-party reader. Members are strictly sequential, and a member's data is copied only once all of it is found present in the archive, so a declared size never allocates more than the archive holds; an archive with a file member larger than `MAX_DECOMPRESSED_SIZE` is rejected. Every header checksum is verified. pax extended headers and GNU long-name records larger than 1 MiB are treated as damage. A bad checksum, a truncated member or an oversized extension record ends the listing: an archive with no readable member before that point is rejected, otherwise the members already read are listed and the problem is reported with a `ContainerEntriesSkipped` diagnostic. Sparse members (old GNU and pax `GNU.sparse` 0.0, 0.1, 1.0) are expanded, with the output of all of them together charged against a budget of 1024 times the archive length, with no minimum, so nested archives cannot each claim an allowance; a sparse member over that budget or with an invalid map is skipped with the same diagnostic. A global pax `path` applies to every following member, so the entry paths of an archive together may not exceed the archive length; members beyond that are skipped with the same diagnostic. An archive without the `ustar` magic is recognised only under a `.tar` name, by its first header's checksum, or as an archive without members when it consists of zero blocks alone. Links, devices, FIFOs and directories are not listed.
- **Compression ratio checks**: RAR archives enforce a maximum compression ratio to block decompression bombs, including for entries with only a few packed bytes.
- **Path sanitisation**: All archive entry paths are sanitised to prevent directory traversal (`../`, absolute paths, null bytes).
- **No shell execution**: The library never invokes external processes or shells.
- **No file I/O in core**: The core library operates on in-memory byte slices; only the optional mmap path reads from the filesystem.

### Known Limitations

- Archive parsing relies on third-party crates (`rar-stream`, `delharc`, and `xz2` for XZ/LZMA). Vulnerabilities in those crates may affect unretro.
- The library does not validate cryptographic signatures or checksums beyond what the underlying format parsers provide.
