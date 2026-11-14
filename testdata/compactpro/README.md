# Compact Pro fixtures

| File | Contents |
|---|---|
| `test-image.cpt` | One file, `Test Image.img`, with both forks stored as run-length plus LZH: a 1,474,560-byte data fork (an HFS floppy image) and a 563-byte resource fork. It is the `test-archive.cpt` nested inside `testdata/test-archive-mac.tar.xz`, copied out unchanged. |

The expected fork contents in `tests/compactpro_fixtures.rs` are FNV-1a 64 hashes of what `unar` 1.10.7 extracts from the archive.
