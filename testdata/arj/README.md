# Synthetic ARJ fixtures

Every archive here is synthetic: all file contents are generated (word-list text, runs, counters, xorshift pseudo-random bytes, an empty file) and no byte comes from a real archive or file. The archives were written by the `arj` command-line tool used as a black box: ARJ32 v 3.10 (Debian package `arj` 3.10.22-28, Debian 13.7, in a container), so every header says host OS 2 (UNIX) and carries the newer 46-byte first header. Each archive was then extracted with `arj x` and with `7z x` (p7zip 17.05); both give byte-identical files, and the sizes and FNV-1a 64 hashes in `tests/arj_fixtures.rs` were computed from those extracted files. The generator is deliberately not part of this repository.

## Source files

| File | Size | Content |
|---|---|---|
| `text.txt` | 3903 | 60 CRLF lines of numbered word-list text |
| `runs.bin` | 3394 | runs of 700, 300 and 1500 equal bytes, `abc` x 200, the bytes 0..255, short runs of 1..7 |
| `random.bin` | 2000 | xorshift bytes (incompressible; ARJ stores it with method 0 whatever method is asked for) |
| `empty.dat` | 0 | |
| `big.bin` | 90000 | text, a 3000-byte random island, 5000 zero bytes, then the first 20000 bytes and the island again (more than 50 KiB back, beyond what ARJ matches), more text, counters; longer than 64 KiB |
| `docs/readme.txt`, `docs/notes/deep/nested.txt` | 29, 725 | text in nested directories |
| `data/table.bin`, `data/one.bin` | 4096, 1 | a counter pattern; the single byte `A` |
| `hollow/inner/` | | empty directory |
| `names/Größe.txt`, `names/Üïcödé π.dat` | 19, 64 | names created as raw code page 437 bytes (`47 72 94 E1 65 ...`, `9A 8B 63 94 64 82 20 E3 ...`), which are not valid UTF-8 |
| `letter.txt` | 1631 | 25 lines of text |
| `plain.txt`, `secret.txt` | 13, 1444 | text |
| `split.bin` | 30000 | xorshift bytes |

## Archives

`arj a -i -y` is abbreviated `A`.

| Archive | Made with | Covers |
|---|---|---|
| `m0.arj` | `A -m0 m0.arj text.txt runs.bin random.bin empty.dat` | method 0 |
| `m1.arj` ... `m4.arj` | `A -mN mN.arj text.txt runs.bin random.bin empty.dat big.bin` | methods 1, 2, 3 and 4; `big.bin` takes two Huffman blocks in methods 1-3 |
| `multi.arj` | `A -r -a1 -m1 -zcmt.txt multi.arj docs`, `A -r -m0 multi.arj data`, `A -r -a1 -m3 multi.arj hollow`, `A -r -m3 multi.arj names`, `A -m4 multi.arj text.txt`, `A -m2 -t1 multi.arj letter.txt` | 11 entries: nested paths, three directory entries (file type 3), an archive comment, code page 437 names, methods 0-4 mixed, a text-mode file (file type 1), entries with and without the path-translated flag |
| `garbled.arj` | `A -m1 garbled.arj plain.txt`, `A -m1 -gfixture garbled.arj secret.txt`, `A -m1 garbled.arj text.txt` | a garbled (password `fixture`) entry between two readable ones |
| `split.arj`, `split.a01` ... `split.a03` | `A -m0 -v12k split.arj plain.txt split.bin text.txt` | complete four-volume set, stored. `split.arj` holds `plain.txt` whole and the first 11194 bytes of `split.bin` (continued flag). `split.a01` holds the next 11274 bytes. `split.a02` holds the last 7532 bytes of `split.bin` (start-position field 22468, no continued flag) and the first 3676 bytes of `text.txt` (continued flag). `split.a03` holds the last 227 bytes of `text.txt` |
| `vol.arj`, `vol.a01` ... `vol.a04` | `A -m1 -v12k vol.arj plain.txt big.bin runs.bin split.bin text.txt` | complete five-volume set, method 1. `big.bin` is split 54093 + 33960 + 1947 bytes over the first three volumes, each piece packed on its own; `split.bin` (stored, incompressible) is split 9592 + 11276 + 9132 over the last three; `runs.bin` and `text.txt` sit whole in `vol.a02` and `vol.a04` |
| `chapter.arj` | `A -m1 chapter.arj plain.txt`, `arj cc chapter.arj`, `arj ac chapter.arj text.txt plain.txt` | chapter archive: two chapter-label entries (file type 5, names `<<<001>>>` and `<<<002>>>`) |

## Expected results

- `m0.arj` ... `m4.arj`, `multi.arj`, `chapter.arj`: every file is listed with the extracted content and no diagnostic. Directory and chapter-label entries are not listed.
- `garbled.arj`: `plain.txt` and `text.txt` are listed; `secret.txt` is reported as skipped.
- `split.arj` and `vol.arj`, opened next to their later volumes: every file of the set is listed once, split files joined, with no diagnostic. The joined files were compared with the generator's originals (SHA-256).
- `split.arj` on its own: `plain.txt` is listed; `split.bin` is reported as needing volume 1. `split.a02` on its own: nothing is listed and both pieces are reported. The CRC stored for a piece covers that piece only, so the flags are the only sign that it is not a whole file.

No fixture has a DOS host OS, self-extractor stub, extended header, volume label or ARJ-PROTECT data; the first three header variants are covered by unit tests in `src/formats/dos/arj.rs` with headers built in code.
