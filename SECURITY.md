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

- **Decompression size limits**: All decompressed data is capped at `MAX_DECOMPRESSED_SIZE` (default 256 MiB) to prevent zip-bomb-style attacks.
- **Compression ratio checks**: RAR archives enforce a maximum compression ratio to block decompression bombs.
- **Path sanitisation**: All archive entry paths are sanitised to prevent directory traversal (`../`, absolute paths, null bytes).
- **No shell execution**: The library never invokes external processes or shells.
- **No file I/O in core**: The core library operates on in-memory byte slices; only the optional mmap path reads from the filesystem.

### Known Limitations

- Archive parsing relies on third-party crates (`zip`, `tar`, `rar-stream`, `delharc`). Vulnerabilities in those crates may affect unretro.
- The library does not validate cryptographic signatures or checksums beyond what the underlying format parsers provide.
