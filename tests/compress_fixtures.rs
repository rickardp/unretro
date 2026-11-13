//! Unix `compress` (`.Z`) fixtures from `testdata/compress/` (see its README
//! for how they were made).
//!
//! Every row of `manifest.tsv` is checked through the public loader. The
//! expected sizes and FNV-1a hashes come from existing `compress` decoders,
//! not from this crate.
#![cfg(feature = "common")]

use std::collections::BTreeSet;

use unretro::{ContainerFormat, EntryType, Loader, VisitAction, detect_format};

const DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/testdata/compress");

fn fixture(name: &str) -> Vec<u8> {
    let path = format!("{DIR}/{name}");
    std::fs::read(&path).unwrap_or_else(|e| panic!("{path}: {e}"))
}

fn fnv1a(data: &[u8]) -> u64 {
    data.iter().fold(0xcbf2_9ce4_8422_2325, |hash, &byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3)
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Expect {
    Ok,
    Cut,
    Lenient,
    Reject,
}

struct Row {
    fixture: String,
    take: Option<usize>,
    expect: Expect,
    // Length and FNV-1a 64 of the expected bytes.
    content: Option<(usize, u64)>,
    member: Option<String>,
}

impl Row {
    fn label(&self) -> String {
        match (self.take, &self.member) {
            (Some(take), _) => format!("{}[..{take}]", self.fixture),
            (None, Some(member)) => format!("{}:{member}", self.fixture),
            (None, None) => self.fixture.clone(),
        }
    }

    fn input(&self) -> Vec<u8> {
        let mut data = fixture(&self.fixture);
        if let Some(take) = self.take {
            assert!(take <= data.len(), "{}", self.label());
            data.truncate(take);
        }
        data
    }
}

fn manifest() -> Vec<Row> {
    let text = String::from_utf8(fixture("manifest.tsv")).unwrap();
    let mut lines = text.lines();
    assert_eq!(
        lines.next(),
        Some("fixture\ttake\texpect\tsize\tfnv64\tmember")
    );
    let optional = |field: &str| (field != "-").then(|| field.to_string());
    lines
        .map(|line| {
            let fields: Vec<&str> = line.split('\t').collect();
            let [fixture, take, expect, size, hash, member] = fields[..] else {
                panic!("manifest row: {line:?}");
            };
            let content = match (optional(size), optional(hash)) {
                (Some(size), Some(hash)) => Some((
                    size.parse().unwrap(),
                    u64::from_str_radix(&hash, 16).unwrap(),
                )),
                (None, None) => None,
                _ => panic!("manifest row: {line:?}"),
            };
            Row {
                fixture: fixture.to_string(),
                take: optional(take).map(|take| take.parse().unwrap()),
                expect: match expect {
                    "ok" => Expect::Ok,
                    "cut" => Expect::Cut,
                    "lenient" => Expect::Lenient,
                    "reject" => Expect::Reject,
                    other => panic!("manifest expect {other:?}"),
                },
                content,
                member: optional(member),
            }
        })
        .collect()
}

type Listing = Vec<(String, Vec<u8>)>;

// Entries below `name`, or the first error or diagnostic.
fn entries(data: Vec<u8>, name: &str, depth: u32, types: EntryType) -> Result<Listing, String> {
    let mut entries = Listing::new();
    let report = Loader::from_bytes(data, name)
        .with_max_depth(depth)
        .visit_with_report(types, |entry| {
            entries.push((entry.path.to_string(), entry.data.to_vec()));
            Ok(VisitAction::Continue)
        })
        .map_err(|error| error.to_string())?;
    match report.diagnostics.first() {
        Some(diagnostic) => Err(diagnostic.message.clone()),
        None => Ok(entries),
    }
}

// The decompressed payload of a `.Z` file, without descending into it.
fn payload(data: Vec<u8>, name: &str) -> Result<Vec<u8>, String> {
    if detect_format(name, Some(&data)) != Some(ContainerFormat::Compress) {
        return Err("not detected as compress".to_string());
    }
    // The payload may itself be a container, which is not a leaf.
    let mut entries = entries(data, name, 1, EntryType::All)?;
    assert_eq!(entries.len(), 1, "{name} yields exactly one entry");
    let (path, bytes) = entries.remove(0);
    let stem = name.strip_suffix(".Z").unwrap();
    assert_eq!(path, format!("{name}/{stem}"));
    Ok(bytes)
}

#[test]
fn manifest_rows_decode_as_expected() {
    let rows = manifest();
    assert!(rows.len() >= 76, "{} rows", rows.len());

    for row in rows.iter().filter(|row| row.member.is_none()) {
        let label = row.label();
        let result = payload(row.input(), &row.fixture);
        match (row.expect, result) {
            // A stream cut inside a code or inside skipped bits yields the
            // whole codes before the cut, like a stream that simply ends.
            (Expect::Ok | Expect::Cut, Ok(bytes)) | (Expect::Lenient, Ok(bytes)) => {
                let (size, hash) = row.content.unwrap();
                assert_eq!(bytes.len(), size, "size of {label}");
                assert_eq!(fnv1a(&bytes), hash, "content of {label}");
            }
            (Expect::Ok | Expect::Cut, Err(error)) => panic!("{label}: {error}"),
            (Expect::Lenient | Expect::Reject, Err(_)) => {}
            (Expect::Reject, Ok(bytes)) => {
                panic!(
                    "{label}: decoded {} bytes from an invalid stream",
                    bytes.len()
                )
            }
        }
    }
}

#[test]
fn every_fixture_is_in_the_manifest() {
    let listed: BTreeSet<String> = manifest().into_iter().map(|row| row.fixture).collect();
    let present: BTreeSet<String> = std::fs::read_dir(DIR)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .filter(|name| name.ends_with(".Z"))
        .collect();
    assert_eq!(listed, present);
}

#[test]
fn tar_z_is_traversed_down_to_its_files() {
    let rows = manifest();
    let members: Vec<&Row> = rows.iter().filter(|row| row.member.is_some()).collect();
    assert_eq!(members.len(), 5);

    for (name, tar) in [
        ("files.tar.Z", "files.tar"),
        ("FILES.TAZ", "FILES.tar"),
        ("files.Z", "files"),
        ("files", "files"),
    ] {
        let data = fixture("files.tar.Z");
        assert_eq!(
            detect_format(name, Some(&data)),
            Some(ContainerFormat::Compress),
            "{name}"
        );
        let found = entries(data, name, 8, EntryType::Leaves).unwrap();
        let got: BTreeSet<&str> = found.iter().map(|(path, _)| path.as_str()).collect();
        let want: BTreeSet<String> = members
            .iter()
            .map(|row| format!("{name}/{tar}/{}", row.member.as_ref().unwrap()))
            .collect();
        assert_eq!(
            got,
            want.iter().map(String::as_str).collect::<BTreeSet<_>>(),
            "paths below {name}"
        );
        for row in &members {
            assert_eq!(row.fixture, "files.tar.Z");
            assert_eq!(row.expect, Expect::Ok);
            let member = row.member.as_ref().unwrap();
            let path = format!("{name}/{tar}/{member}");
            let (_, bytes) = found.iter().find(|(found, _)| *found == path).unwrap();
            let (size, hash) = row.content.unwrap();
            assert_eq!(bytes.len(), size, "size of {member}");
            assert_eq!(fnv1a(bytes), hash, "content of {member}");
        }
    }
}

#[test]
fn payload_reports_its_compression_method() {
    let mut methods = Vec::new();
    Loader::from_bytes(fixture("text.Z"), "text.Z")
        .visit(EntryType::Leaves, |entry| {
            methods.push(entry.metadata.and_then(|m| m.compression_method.clone()));
            Ok(VisitAction::Continue)
        })
        .unwrap();
    assert_eq!(methods, [Some("lzw".to_string())]);
}

#[test]
fn invalid_headers_are_not_detected() {
    for name in [
        "trunc-header.Z",
        "flag-20.Z",
        "flag-40.Z",
        "bad-bits-0.Z",
        "bad-bits-8.Z",
        "bad-bits-17.Z",
        "bad-bits-31.Z",
        "bad-first-code.Z",
        "bad-first-clear.Z",
        "bad-code-beyond.Z",
    ] {
        let data = fixture(name);
        assert_eq!(detect_format(name, Some(&data)), None, "{name}");
        assert_eq!(detect_format("no-extension", Some(&data)), None, "{name}");
    }
}

#[test]
fn corrupt_streams_fail_without_partial_output() {
    for name in ["trail-text.Z", "bad-code-late.Z", "bad-after-clear.Z"] {
        let data = fixture(name);
        assert_eq!(
            detect_format(name, Some(&data)),
            Some(ContainerFormat::Compress),
            "{name}"
        );
        let mut visited = 0;
        let outcome = Loader::from_bytes(data, name).visit_with_report(EntryType::All, |_| {
            visited += 1;
            Ok(VisitAction::Continue)
        });
        assert_eq!(visited, 0, "{name}");
        match outcome {
            Ok(report) => {
                assert!(report.has_root_failures(), "{name}");
                assert!(
                    report.diagnostics[0].message.contains("compress (.Z)"),
                    "{}",
                    report.diagnostics[0].message
                );
            }
            Err(error) => assert!(error.to_string().contains("compress (.Z)"), "{error}"),
        }
    }
}

#[test]
fn every_prefix_of_a_stream_decodes_to_a_prefix_of_its_plaintext() {
    // Truncation is indistinguishable from a normal end: each cut returns
    // the whole codes before it and never fails or invents bytes.
    for name in ["text.Z", "nb-text.Z", "shift-b9.Z", "shift-b10.Z"] {
        let data = fixture(name);
        let full = payload(data.clone(), name).unwrap();
        let mut previous = 0;
        let step = (data.len() / 1500).max(1);
        for take in (3..data.len()).step_by(step) {
            let part = payload(data[..take].to_vec(), name)
                .unwrap_or_else(|error| panic!("{name}[..{take}]: {error}"));
            assert!(full.starts_with(&part), "{name}[..{take}]");
            assert!(part.len() >= previous, "{name}[..{take}]");
            previous = part.len();
        }
    }
}

// ---------------------------------------------------------------------------
// Streams built here from `docs/compress/format.md`, for the cases no committed
// fixture reaches: large non-block streams, CLEAR below the maximum width, and
// the T13/T14 files of spec section 17.5, which are rebuilt byte for byte and
// compared with the SHA-256 values the spec gives for real encoder output.
// ---------------------------------------------------------------------------

struct Stream {
    file: Vec<u8>,
    // All codes written, CLEAR codes included.
    codes: usize,
    // Codes written while the table was full.
    codes_on_full_table: usize,
    // File offsets at which the width grew.
    growth_offsets: Vec<usize>,
    // (width of the CLEAR code, bits skipped after it).
    clears: Vec<(u32, u64)>,
}

struct Encoder {
    block: bool,
    limit: u32,
    width_limit: u32,
    // Stream bits written after the 3-byte header.
    bits: u64,
    origin: u64,
    width: u32,
    run_codes: u32,
    out: Stream,
}

impl Encoder {
    fn first_free(&self) -> u32 {
        if self.block { 257 } else { 256 }
    }

    // The decoder's `free` after `run_codes` codes of this run.
    fn free(&self) -> u32 {
        (self.first_free() + self.run_codes.saturating_sub(1)).min(self.limit)
    }

    fn bit(&mut self, bit: bool) {
        if self.bits % 8 == 0 {
            self.out.file.push(0);
        }
        if bit {
            *self.out.file.last_mut().unwrap() |= 1 << (self.bits % 8);
        }
        self.bits += 1;
    }

    // Section 8.1: fills up the group of eight codes. Returns the bits added.
    fn skip(&mut self, padding: &mut dyn FnMut() -> bool) -> u64 {
        let before = self.bits;
        while (self.bits - self.origin) % (u64::from(self.width) * 8) != 0 {
            let bit = padding();
            self.bit(bit);
        }
        self.origin = self.bits;
        self.bits - before
    }

    fn code(&mut self, code: u32, padding: &mut dyn FnMut() -> bool) {
        if self.free() >= 1 << self.width && self.width < self.width_limit {
            self.skip(padding);
            self.width += 1;
            self.out.growth_offsets.push(self.out.file.len());
        }
        assert!(code < 1 << self.width);
        for bit in 0..self.width {
            self.bit(code >> bit & 1 != 0);
        }
        if self.free() == self.limit {
            self.out.codes_on_full_table += 1;
        }
        self.run_codes += 1;
        self.out.codes += 1;
    }

    fn clear(&mut self, padding: &mut dyn FnMut() -> bool) {
        assert!(self.block && self.out.codes > 0);
        self.code(256, padding);
        let skipped = self.skip(padding);
        self.out.clears.push((self.width, skipped));
        self.width = 9;
        self.run_codes = 0;
    }
}

#[derive(Clone, Copy)]
struct Options<'a> {
    max_bits: u32,
    block: bool,
    // Maximum width 9 written with 9-bit codes throughout (spec section 11,
    // dialect B) instead of the historical 10-bit codes on a full table.
    dialect_b: bool,
    // A CLEAR follows after this many codes, counted from the start of the
    // stream or from the previous CLEAR.
    runs: &'a [u32],
}

fn block(max_bits: u32) -> Options<'static> {
    Options {
        max_bits,
        block: true,
        dialect_b: false,
        runs: &[],
    }
}

// Greedy LZW. Without CLEAR codes the stream for a given input is unique.
fn encode(plain: &[u8], options: Options, padding: &mut dyn FnMut() -> bool) -> Stream {
    let Options {
        max_bits,
        block,
        dialect_b,
        runs,
    } = options;
    assert!((9..=16).contains(&max_bits));
    let mut encoder = Encoder {
        block,
        limit: 1 << max_bits,
        width_limit: if dialect_b {
            max_bits
        } else {
            max_bits.max(10)
        },
        bits: 0,
        origin: 0,
        width: 9,
        run_codes: 0,
        out: Stream {
            file: vec![0x1F, 0x9D, max_bits as u8 | if block { 0x80 } else { 0 }],
            codes: 0,
            codes_on_full_table: 0,
            growth_offsets: Vec::new(),
            clears: Vec::new(),
        },
    };
    let mut table: std::collections::HashMap<(u32, u8), u32> = std::collections::HashMap::new();
    let mut runs = runs.iter().copied();
    let mut next_clear = runs.next();
    assert_ne!(next_clear, Some(0), "a stream cannot start with CLEAR");

    let mut pending: Option<u32> = None;
    for &byte in plain {
        let Some(prefix) = pending else {
            pending = Some(u32::from(byte));
            continue;
        };
        if let Some(&code) = table.get(&(prefix, byte)) {
            pending = Some(code);
            continue;
        }
        encoder.code(prefix, padding);
        // The entry the decoder creates when it reads the next code.
        let entry = encoder.first_free() + encoder.run_codes - 1;
        if entry < encoder.limit {
            table.insert((prefix, byte), entry);
        }
        pending = Some(u32::from(byte));
        while next_clear == Some(encoder.run_codes) {
            encoder.clear(padding);
            table.clear();
            next_clear = runs.next();
        }
    }
    if let Some(code) = pending {
        encoder.code(code, padding);
    }
    assert_eq!(next_clear, None, "input too short for the CLEAR plan");
    encoder.out
}

struct XorShift(u32);

impl XorShift {
    fn next(&mut self) -> u32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 17;
        self.0 ^= self.0 << 5;
        self.0
    }

    fn below(&mut self, bound: u32) -> u32 {
        (self.next() >> 8) % bound
    }

    // `len` bytes over an alphabet of `symbols` values.
    fn bytes(&mut self, len: usize, symbols: u32) -> Vec<u8> {
        (0..len)
            .map(|_| (self.below(symbols) as u8).wrapping_add(b'a'))
            .collect()
    }
}

fn zeros() -> impl FnMut() -> bool {
    || false
}

fn noise(seed: u32) -> impl FnMut() -> bool {
    let mut random = XorShift(seed);
    move || random.next() & 0x100 != 0
}

fn sha256(data: &[u8]) -> String {
    // Fractional bits of the roots of the first primes, as the standard
    // defines the constants.
    let primes: Vec<u32> = (2u32..)
        .filter(|n| (2..*n).take_while(|d| d * d <= *n).all(|d| n % d != 0))
        .take(64)
        .collect();
    let fraction = |root: f64| ((root - root.floor()) * 4_294_967_296.0) as u32;
    let k: Vec<u32> = primes
        .iter()
        .map(|&p| fraction(f64::from(p).cbrt()))
        .collect();
    let mut state: Vec<u32> = primes[..8]
        .iter()
        .map(|&p| fraction(f64::from(p).sqrt()))
        .collect();

    let mut message = data.to_vec();
    message.push(0x80);
    while message.len() % 64 != 56 {
        message.push(0);
    }
    message.extend_from_slice(&(data.len() as u64 * 8).to_be_bytes());

    for chunk in message.chunks_exact(64) {
        let mut w = [0u32; 64];
        for (word, bytes) in w.iter_mut().zip(chunk.chunks_exact(4)) {
            *word = u32::from_be_bytes(bytes.try_into().unwrap());
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }
        let mut v: [u32; 8] = state[..].try_into().unwrap();
        for i in 0..64 {
            let [a, b, c, d, e, f, g, h] = v;
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let t1 = h
                .wrapping_add(s1)
                .wrapping_add((e & f) ^ (!e & g))
                .wrapping_add(k[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let t2 = s0.wrapping_add((a & b) ^ (a & c) ^ (b & c));
            v = [t1.wrapping_add(t2), a, b, c, d.wrapping_add(t1), e, f, g];
        }
        for (word, add) in state.iter_mut().zip(v) {
            *word = word.wrapping_add(add);
        }
    }
    state.iter().map(|word| format!("{word:08x}")).collect()
}

// SEQ of spec section 17.5: the output of `seq 1 2000`.
fn seq_input() -> Vec<u8> {
    let text: String = (1..=2000).map(|n| format!("{n}\n")).collect();
    assert_eq!(text.len(), 8893);
    assert_eq!(
        sha256(text.as_bytes()),
        "6251e5743b6fd6a7d606130bdf7c15077ce85ebd3a0fdee284d15a46df199e38"
    );
    text.into_bytes()
}

// LCG of spec section 17.5.
fn lcg_input() -> Vec<u8> {
    let mut text: String = (1..=3000).map(|n| format!("{n}\n")).collect();
    let mut s = 1u32;
    for _ in 0..3000 {
        s = s.wrapping_mul(69069).wrapping_add(1);
        text.push_str(&format!("{s}\n"));
    }
    assert!(text.ends_with("\n2675762664\n4103660233\n"));
    assert_eq!(text.len(), 46120);
    assert_eq!(
        sha256(text.as_bytes()),
        "ecc284ad38f97e4582954a0c67ffc326bc1ccf46c5e44f287513c84b59f54007"
    );
    text.into_bytes()
}

fn hex(text: &str) -> Vec<u8> {
    (0..text.len())
        .step_by(2)
        .map(|at| u8::from_str_radix(&text[at..at + 2], 16).unwrap())
        .collect()
}

#[test]
fn spec_t13_files_decode_to_seq() {
    let seq = seq_input();
    // (maximum width, file size, SHA-256, codes, offsets of width growth)
    let files: [(u32, usize, &str, usize, &[usize]); 4] = [
        (
            10,
            4370,
            "fe5d10578546a7318f948a05998c889fa389678a54b1ed80704b54f9463dcbc9",
            3519,
            &[291],
        ),
        (
            12,
            4270,
            "201bb0822d27ae5841259112fd39e888d2f868c44b3042bdfa42e0f3c3c0b068",
            3079,
            &[291, 931, 2339],
        ),
        (
            16,
            4270,
            "1bb2f1945177f8b8f00812ce86273ecef076499693f5e8efbf39a01f34a7750b",
            3079,
            &[291, 931, 2339],
        ),
        // Dialect A: 10-bit codes on the full 512-entry table.
        (
            9,
            4806,
            "4536546422af96fce0f6932bc12c69ee003e6baf44de12a303491baddb09e9f0",
            3868,
            &[291],
        ),
    ];
    for (max_bits, size, digest, codes, growth) in files {
        let stream = encode(&seq, block(max_bits), &mut zeros());
        assert_eq!(stream.file.len(), size, "-b {max_bits}");
        assert_eq!(sha256(&stream.file), digest, "-b {max_bits}");
        assert_eq!(stream.codes, codes, "-b {max_bits}");
        assert_eq!(stream.growth_offsets, growth, "-b {max_bits}");
        assert!(stream.clears.is_empty());
        if max_bits == 10 {
            // Spec section 10: more than 2,700 codes on the frozen table.
            assert_eq!(stream.codes_on_full_table, 3519 - 768);
        }
        assert_eq!(payload(stream.file, "seq.Z").unwrap(), seq, "-b {max_bits}");
    }
}

#[test]
fn nine_bit_codes_on_a_full_table_are_corrupt_at_maximum_width_nine() {
    // Maximum width 9 with 9-bit codes throughout (spec section 11, dialect
    // B). Read the historical way it is refused, not decoded to wrong bytes.
    // This is not the dialect B file of T13: that one could not be rebuilt.
    let options = Options {
        dialect_b: true,
        ..block(9)
    };
    let stream = encode(&seq_input(), options, &mut zeros());
    assert!(stream.growth_offsets.is_empty());
    assert!(stream.codes_on_full_table > 3000);
    let error = payload(stream.file, "seq.Z").unwrap_err();
    assert!(error.contains("compress (.Z)"), "{error}");
}

#[test]
fn spec_t14_files_with_clear_codes_decode_to_lcg() {
    struct File {
        label: &'static str,
        max_bits: u32,
        runs: &'static [u32],
        // Bytes the BSD encoder leaves in and around the skipped bits.
        stale: &'static [(usize, &'static str)],
        size: usize,
        digest: &'static str,
        clears: &'static [(u32, u64)],
        growth: &'static [usize],
    }
    let files = [
        File {
            label: "ncompress -b 10",
            max_bits: 10,
            runs: &[256 + 11706],
            stale: &[],
            size: 23211,
            digest: "c12ba8e2b54e0097a8ae5e344dd262f4a2481056311a567b4989504da54939c8",
            clears: &[(10, 50)],
            growth: &[291, 15219],
        },
        File {
            label: "BSD -b 10",
            max_bits: 10,
            runs: &[256 + 11706],
            stale: &[(14924, "10c6f86d34dd49")],
            size: 23211,
            digest: "a53b917f0d43aba19dfd7b565ba9ec15d151b34c5c1c25271ed2dc53e937db5f",
            clears: &[(10, 50)],
            growth: &[291, 15219],
        },
        File {
            label: "BSD -b 12",
            max_bits: 12,
            runs: &[1792 + 5258],
            stale: &[(10227, "0153f5281faf69df")],
            size: 22308,
            digest: "eba8aec8219f2b857f540cd24157ced3a90aed5ad6ecf855bba527b6757f3afc",
            clears: &[(12, 60)],
            growth: &[291, 931, 2339, 10523, 11163, 12571],
        },
        File {
            label: "ncompress -b 12",
            max_bits: 12,
            runs: &[1792 + 5257],
            stale: &[],
            size: 22296,
            digest: "fd6790c458afac48821359090b818f40af9a66a91a450884636d17af143c59b7",
            clears: &[(12, 72)],
            growth: &[291, 931, 2339, 10523, 11163, 12571],
        },
        File {
            label: "-b 16",
            max_bits: 16,
            runs: &[],
            stale: &[],
            size: 22366,
            digest: "1b00f63ad4df9cf90d0795b784407a4ed08d10a0ca5a77ac61d6a63e7ca7ea6c",
            clears: &[],
            growth: &[291, 931, 2339, 5411, 12067],
        },
        File {
            label: "BSD -b 9",
            max_bits: 9,
            runs: &[256 + 8494, 256 + 9634],
            stale: &[(10909, "1073"), (23242, "10764a4de7d46d")],
            size: 27036,
            digest: "7d02e01d5f8ec92dc27bf64f05559cab0084983f8259409ee727a9c76c422618",
            clears: &[(10, 10), (10, 50)],
            growth: &[291, 11199, 23537],
        },
    ];

    let lcg = lcg_input();
    for file in files {
        let label = file.label;
        let options = Options {
            runs: file.runs,
            ..block(file.max_bits)
        };
        let mut stream = encode(&lcg, options, &mut zeros());
        for (offset, bytes) in file.stale {
            let bytes = hex(bytes);
            stream.file[*offset..*offset + bytes.len()].copy_from_slice(&bytes);
        }
        assert_eq!(stream.file.len(), file.size, "{label}");
        assert_eq!(sha256(&stream.file), file.digest, "{label}");
        assert_eq!(stream.clears, file.clears, "{label}");
        assert_eq!(stream.growth_offsets, file.growth, "{label}");
        assert_eq!(payload(stream.file, "lcg.Z").unwrap(), lcg, "{label}");
    }
}

#[test]
fn stream_that_expands_past_the_limit_is_refused() {
    // A literal zero, KwKwK codes until the 65,536-entry table is full, then
    // the longest string (65,280 bytes) over and over: about 190 KiB that
    // would decode to just over 4 GiB. The reader measures the output before
    // it allocates any, so this fails at once and without the memory.
    let mut encoder = Encoder {
        block: true,
        limit: 1 << 16,
        width_limit: 16,
        bits: 0,
        origin: 0,
        width: 9,
        run_codes: 0,
        out: Stream {
            file: vec![0x1F, 0x9D, 0x90],
            codes: 0,
            codes_on_full_table: 0,
            growth_offsets: Vec::new(),
            clears: Vec::new(),
        },
    };
    let codes = std::iter::once(0)
        .chain(257..=65535)
        .chain(std::iter::repeat_n(65535, 33_200));
    let mut size = 0u64;
    for (index, code) in codes.enumerate() {
        encoder.code(code, &mut zeros());
        size += (index as u64 + 1).min(65280);
    }
    let stream = encoder.out;
    assert_eq!(stream.codes_on_full_table, 33_200);
    assert!(stream.file.len() < 200_000);
    assert!(size > unretro::MAX_DECOMPRESSED_SIZE);

    let error = payload(stream.file.clone(), "zeros.Z").unwrap_err();
    assert!(error.contains("4294967296 byte limit"), "{error}");

    // One code more than the first 2,000 fit: 2,001,000 bytes.
    let cut = 3 + (256 * 9 + 512 * 10 + 1024 * 11 + 208 * 12) / 8;
    let plain = payload(stream.file[..cut].to_vec(), "zeros.Z").unwrap();
    assert_eq!(plain.len(), 2000 * 2001 / 2);
    assert!(plain.iter().all(|&byte| byte == 0));
}

#[test]
fn non_block_streams_fill_the_table_at_every_maximum_width() {
    let mut random = XorShift(0x0BAD_5EED);
    for max_bits in 9..=16 {
        // Enough to fill the table and go on with it frozen.
        let plain = random.bytes((4usize << max_bits) + 40_000, 27);
        let options = Options {
            block: false,
            ..block(max_bits)
        };
        let stream = encode(&plain, options, &mut noise(max_bits));
        assert_eq!(stream.file[2], max_bits as u8);
        // 257 codes of 9 bits leave 63 bits to skip; later widths none.
        let growths = (max_bits.max(10) - 9) as usize;
        assert_eq!(stream.growth_offsets.len(), growths, "-b {max_bits}");
        assert_eq!(stream.growth_offsets[0], 300);
        assert!(
            stream.codes_on_full_table > 5000,
            "-b {max_bits}: {} codes on the full table",
            stream.codes_on_full_table
        );
        assert_eq!(
            payload(stream.file.clone(), "nb.Z").unwrap(),
            plain,
            "-b {max_bits}"
        );

        // The same codes with other bits in the skipped slots.
        let zeroed = encode(&plain, options, &mut zeros());
        assert_ne!(zeroed.file, stream.file);
        assert_eq!(
            payload(zeroed.file, "nb.Z").unwrap(),
            plain,
            "-b {max_bits}"
        );
    }
}

#[test]
fn clear_is_honoured_at_every_width() {
    let mut random = XorShift(0x5EED_CAFE);
    for max_bits in 10..=16u32 {
        // One CLEAR at each width from 9 to the maximum, as the fourth code
        // of that width, so four code slots of that width are skipped. Then
        // two CLEAR codes in a row at width 9.
        let mut runs: Vec<u32> = vec![3];
        runs.extend((10..=max_bits).map(|width| (1 << (width - 1)) - 256 + 3));
        runs.extend([5, 0]);
        let plain = random.bytes((5usize << max_bits) + 20_000, 27);
        let options = Options {
            runs: &runs,
            ..block(max_bits)
        };
        let mut expected: Vec<(u32, u64)> = (9..=max_bits)
            .map(|width| (width, u64::from(width) * 4))
            .collect();
        expected.extend([(9, 9 * 2), (9, 9 * 7)]);

        let stream = encode(&plain, options, &mut noise(max_bits));
        assert_eq!(stream.clears, expected, "-b {max_bits}");
        assert_eq!(
            payload(stream.file.clone(), "clear.Z").unwrap(),
            plain,
            "-b {max_bits}"
        );

        let zeroed = encode(&plain, options, &mut zeros());
        assert_ne!(zeroed.file, stream.file);
        assert_eq!(
            payload(zeroed.file, "clear.Z").unwrap(),
            plain,
            "-b {max_bits}"
        );
    }
}

#[test]
fn generated_streams_round_trip() {
    let mut random = XorShift(0x1234_ABCD);
    let (mut with_clear, mut non_block, mut full) = (0, 0, 0);
    for round in 0..240u32 {
        let max_bits = 9 + round % 8;
        let block_mode = random.below(3) != 0;
        let symbols = [2, 3, 7, 27, 64, 256][random.below(6) as usize];
        let len = 1 + random.below(40_000) as usize;
        let plain = random.bytes(len, symbols);

        // The CLEAR plan has to fit the input: each code covers a byte or more.
        let mut runs = Vec::new();
        if block_mode && random.below(4) != 0 {
            let mut budget = (len / 40) as u32;
            while budget > 0 && runs.len() < 12 {
                let run = match random.below(8) {
                    0 if !runs.is_empty() => 0,
                    1 => 1 + random.below(8),
                    _ => 1 + random.below(budget.min(6000)),
                };
                if run > budget {
                    break;
                }
                budget -= run;
                runs.push(run);
            }
        }
        let options = Options {
            max_bits,
            block: block_mode,
            dialect_b: false,
            runs: &runs,
        };
        let stream = encode(&plain, options, &mut noise(round + 1));
        with_clear += usize::from(!stream.clears.is_empty());
        non_block += usize::from(!block_mode);
        full += usize::from(stream.codes_on_full_table > 0);
        let label = format!(
            "round {round}: -b {max_bits}, block {block_mode}, {len} bytes, CLEAR after {runs:?}"
        );
        match payload(stream.file, "gen.Z") {
            Ok(bytes) => assert!(bytes == plain, "{label}"),
            Err(error) => panic!("{label}: {error}"),
        }
    }
    assert!(with_clear > 60 && non_block > 40 && full > 40);
}
