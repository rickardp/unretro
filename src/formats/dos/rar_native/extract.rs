//! Test-only end-to-end path over the clean-room RAR modules: the container
//! readers (`docs/rar/rar4-container.md`, `docs/rar/rar5-container.md`)
//! feeding the decoders (`docs/rar/rar29-lz.md`, `docs/rar/rar5-compression.md`).
//! The regression suite at the end uses only bytes printed in those
//! documents; raw member streams are wrapped in minimal archives built from
//! the documents' header layouts.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use super::{container4, container5, rar5, rar29};
use crate::error::{Error, Result};

const DEFAULT_LIMIT: u64 = 1 << 30;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Outcome {
    // Declared size and checksum verified.
    Ok { size: u64, crc32: u32 },
    // Directory or link.
    NoContents,
    Encrypted,
    // Outside this path: needs the RAR 1.5 or 2.0 decoder.
    Skipped(String),
    Failed(String),
}

#[derive(Debug, Clone)]
pub(crate) struct Entry {
    pub name: String,
    // Space-separated properties for coverage counts.
    pub kind: String,
    pub outcome: Outcome,
}

#[derive(Debug, Clone)]
pub(crate) struct Listing {
    pub format: &'static str,
    pub entries: Vec<Entry>,
    pub notes: Vec<String>,
}

const RAR4_SIGNATURE: &[u8] = b"Rar!\x1a\x07\x00";
const RAR5_SIGNATURE: &[u8] = b"Rar!\x1a\x07\x01\x00";

fn is_rar4(first: &[u8]) -> bool {
    first.starts_with(RAR4_SIGNATURE) || container4::signature_offset(first).is_some()
}

// Lists and decodes every member of the set; `limit` bounds each member.
pub(crate) fn extract(volumes: &[Vec<u8>], limit: u64) -> Result<Listing> {
    let slices: Vec<&[u8]> = volumes.iter().map(Vec::as_slice).collect();
    let first = slices
        .first()
        .ok_or_else(|| Error::invalid_format("RAR: no volume given"))?;
    if first.starts_with(RAR5_SIGNATURE) || !is_rar4(first) {
        extract5(&slices, limit)
    } else {
        extract4(&slices, limit)
    }
}

fn skipped4(reason: container4::SkipReason) -> Outcome {
    match reason {
        container4::SkipReason::Encrypted => Outcome::Encrypted,
        other => Outcome::Failed(other.message()),
    }
}

fn extract4(volumes: &[&[u8]], limit: u64) -> Result<Listing> {
    use container4::{Content, DecoderKind};

    let archive = container4::read_archive(volumes)?;
    let mut decoder = rar29::Decoder::new();
    // The decoder state left by member i cannot be continued (15.3).
    let mut unusable = vec![false; archive.members.len()];
    let mut last_decoded = None;
    let mut entries = Vec::new();
    for (index, member) in archive.members.iter().enumerate() {
        let solid = member.chain.is_some_and(|link| link.continues);
        let mut kind = format!("v{} m{:02x}", member.unpack_version, member.method);
        if solid {
            kind.push_str(" solid");
        }
        if member.pieces.len() > 1 {
            kind.push_str(" split");
        }
        let outcome = match member.content {
            Content::None => Outcome::NoContents,
            Content::Empty => Outcome::Ok { size: 0, crc32: 0 },
            Content::Stored => match member.check_stored(volumes) {
                Ok(()) => stored4(member, volumes, limit),
                Err(reason) => skipped4(reason),
            },
            Content::Skipped(reason) => {
                unusable[index] = true;
                skipped4(reason)
            }
            Content::Packed(DecoderKind::Rar29) => {
                // rar29-lz.md 6: the first bit of a block header is the block type.
                let first = member.pieces.first().and_then(|piece| piece.bytes(volumes));
                if !solid && first.and_then(<[u8]>::first).is_some_and(|b| b & 0x80 != 0) {
                    kind.push_str(" ppmd");
                }
                let outcome = packed4(
                    member,
                    volumes,
                    limit,
                    &mut decoder,
                    &unusable,
                    &mut last_decoded,
                    index,
                );
                if outcome.is_err() || !decoder.can_continue() {
                    unusable[index] = true;
                }
                match outcome {
                    Ok(outcome) => outcome,
                    // 15.2 and vector C1 (`Z0`): the entry is an empty file
                    // whatever the decoder does with its packed bytes.
                    Err(_) if member.unpacked_size == 0 => Outcome::Ok { size: 0, crc32: 0 },
                    Err(failure) => failure,
                }
            }
            Content::Packed(other) => {
                unusable[index] = true;
                Outcome::Skipped(format!("needs the {other:?} decoder"))
            }
        };
        entries.push(Entry {
            name: member.path(),
            kind,
            outcome,
        });
    }
    let mut notes: Vec<String> = archive.findings.iter().map(|f| f.message()).collect();
    if archive.earlier_volumes_missing {
        notes.push("earlier volumes missing".into());
    }
    Ok(Listing {
        format: "rar4",
        entries,
        notes,
    })
}

fn stored4(member: &container4::Member, volumes: &[&[u8]], limit: u64) -> Outcome {
    if member.unpacked_size > limit {
        return Outcome::Failed("extract: member exceeds the test limit".into());
    }
    let mut crc = container4::Crc32::new();
    let mut size = 0u64;
    for piece in &member.pieces {
        let Some(bytes) = piece.bytes(volumes) else {
            return Outcome::Failed("extract: stored piece outside its volume".into());
        };
        crc.update(bytes);
        size += bytes.len() as u64;
    }
    if size != member.unpacked_size {
        return Outcome::Failed(format!(
            "extract: check_stored passed {size} bytes for a declared size of {}",
            member.unpacked_size
        ));
    }
    Outcome::Ok {
        size,
        crc32: crc.finish(),
    }
}

// Ok: the decoder returned (whatever the checksum says); Err: it was not
// run or failed, so the chain is broken (rar4-container.md 15.3).
fn packed4(
    member: &container4::Member,
    volumes: &[&[u8]],
    limit: u64,
    decoder: &mut rar29::Decoder,
    unusable: &[bool],
    last_decoded: &mut Option<usize>,
    index: usize,
) -> core::result::Result<Outcome, Outcome> {
    let link = member.chain;
    if link.is_some_and(|link| link.continues) {
        match link.and_then(|link| link.predecessor) {
            None => {
                return Err(Outcome::Failed(
                    "extract: continuing member without a predecessor".into(),
                ));
            }
            Some(previous) if unusable.get(previous).copied().unwrap_or(true) => {
                return Err(Outcome::Failed(
                    container4::SkipReason::DependsOnUnreadable.message(),
                ));
            }
            Some(previous) if *last_decoded != Some(previous) => {
                return Err(Outcome::Failed(format!(
                    "extract: predecessor {previous} is not the member decoded last"
                )));
            }
            Some(_) => {}
        }
    }
    // 13.3 rule 3.
    if !member.part_checksums_match(volumes) {
        return Err(Outcome::Failed(container4::SkipReason::Corrupt.message()));
    }
    let mut packed = Vec::new();
    for piece in &member.pieces {
        let bytes = piece
            .bytes(volumes)
            .ok_or_else(|| Outcome::Failed("extract: packed piece outside its volume".into()))?;
        packed.extend_from_slice(bytes);
    }
    let solid = link.is_some_and(|link| link.continues);
    let data = decoder
        .decode_member(&packed, member.unpacked_size, solid, limit)
        .map_err(|error| Outcome::Failed(format!("decoder: {error}")))?;
    *last_decoded = Some(index);
    let size = data.len() as u64;
    if size != member.unpacked_size {
        return Ok(Outcome::Failed(format!(
            "extract: decoder returned {size} bytes for a declared size of {}",
            member.unpacked_size
        )));
    }
    let crc32 = container4::crc32(&data);
    // 13.3 rules 2 and 4.
    if size != 0 && crc32 != member.crc32 {
        return Ok(Outcome::Failed(
            container4::SkipReason::ChecksumMismatch.message(),
        ));
    }
    Ok(Outcome::Ok { size, crc32 })
}

fn extract5(volumes: &[&[u8]], limit: u64) -> Result<Listing> {
    use container5::{Algorithm, Failure, Handling, StopReason};

    let archive = container5::read_archive(volumes);
    let mut notes = Vec::new();
    if let Some(stop) = archive.stop {
        if archive.members.is_empty()
            && matches!(
                stop.reason,
                StopReason::NotRar5 | StopReason::EncryptedHeaders
            )
        {
            return Err(stop.to_error());
        }
        notes.push(format!(
            "stopped: {:?} in volume {} at offset {}",
            stop.reason, stop.volume, stop.offset
        ));
    }
    if archive.diagnostics.count > 0 {
        let kinds: Vec<String> = archive
            .diagnostics
            .first
            .iter()
            .map(|d| format!("{:?}", d.kind))
            .collect();
        notes.push(format!(
            "{} diagnostics: {}",
            archive.diagnostics.count,
            kinds.join(",")
        ));
    }
    let mut decoder = rar5::Decoder::new();
    // rar5-container.md 12.2: members whose decoding failed poison the group.
    let mut failed = vec![false; archive.members.len()];
    let mut last_decoded = None;
    let mut entries: Vec<Entry> = Vec::new();
    for (index, member) in archive.members.iter().enumerate() {
        let compression = member.compression;
        let mut kind = format!(
            "alg{} m{} d{}",
            match compression.algorithm {
                Some(Algorithm::Rar50) => "50",
                Some(Algorithm::Rar70) => "70",
                None => "?",
            },
            compression.method,
            compression.dictionary_size
        );
        if compression.solid {
            kind.push_str(" solid");
        }
        if member.pieces.len() > 1 {
            kind.push_str(" split");
        }
        if member.blake2sp.is_some() {
            kind.push_str(" blake2");
        }
        let outcome = match member.handling {
            Handling::NoContents => Outcome::NoContents,
            Handling::Failed(Failure::Encrypted) => Outcome::Encrypted,
            Handling::Failed(failure) => Outcome::Failed(format!("{}", failure.to_error())),
            Handling::Stored | Handling::Empty => {
                match container5::read_stored(member, volumes, limit) {
                    Ok(data) => {
                        let size = data.len() as u64;
                        if member
                            .unpacked_size
                            .is_some_and(|declared| declared != size)
                        {
                            Outcome::Failed(format!(
                                "extract: read_stored returned {size} bytes for a declared size of {:?}",
                                member.unpacked_size
                            ))
                        } else {
                            Outcome::Ok {
                                size,
                                crc32: container5::crc32(&data),
                            }
                        }
                    }
                    Err(error) => Outcome::Failed(format!("{error}")),
                }
            }
            Handling::Decode { previous, .. } => {
                kind.push_str(" decode");
                let outcome = if previous.is_some_and(|p| failed.get(p).copied().unwrap_or(true)) {
                    Err(Outcome::Failed(format!(
                        "{}",
                        Failure::PoisonedGroup.to_error()
                    )))
                } else if previous.is_some() && previous != last_decoded {
                    Err(Outcome::Failed(format!(
                        "extract: predecessor {previous:?} is not the member decoded last"
                    )))
                } else {
                    decode5(member, volumes, limit, &mut decoder, previous.is_some())
                };
                match outcome {
                    Ok(outcome) => {
                        last_decoded = Some(index);
                        outcome
                    }
                    Err(outcome) => {
                        failed[index] = true;
                        outcome
                    }
                }
            }
            Handling::CopyOf(source) => match entries.get(source).map(|entry| &entry.outcome) {
                Some(outcome @ Outcome::Ok { .. }) => outcome.clone(),
                Some(other) => Outcome::Failed(format!("link source failed: {other:?}")),
                None => Outcome::Failed("extract: link source does not precede the link".into()),
            },
        };
        entries.push(Entry {
            name: member.path(),
            kind,
            outcome,
        });
    }
    Ok(Listing {
        format: "rar5",
        entries,
        notes,
    })
}

// Ok: the decoder returned; Err: it was not run or failed (group poisoned).
fn decode5(
    member: &container5::Member,
    volumes: &[&[u8]],
    limit: u64,
    decoder: &mut rar5::Decoder,
    solid: bool,
) -> core::result::Result<Outcome, Outcome> {
    let fail = |error: Error| Outcome::Failed(format!("{error}"));
    // rar5-container.md 10.1: piece checksums before decoding.
    container5::verify_pieces(member, volumes).map_err(fail)?;
    let packed = member.packed(volumes).map_err(fail)?.concat();
    let params = rar5::MemberParams {
        dict_size: member.compression.dictionary_size,
        version: match member.compression.algorithm {
            Some(container5::Algorithm::Rar70) => 1,
            _ => 0,
        },
        unpacked_size: member.unpacked_size,
        solid,
    };
    let data = decoder
        .decode_member(&packed, &params, limit)
        .map_err(|error| Outcome::Failed(format!("decoder: {error}")))?;
    let size = data.len() as u64;
    if member
        .unpacked_size
        .is_some_and(|declared| declared != size)
    {
        return Ok(Outcome::Failed(format!(
            "extract: decoder returned {size} bytes for a declared size of {:?}",
            member.unpacked_size
        )));
    }
    if size > limit {
        return Ok(Outcome::Failed(format!(
            "extract: decoder returned {size} bytes above the limit {limit}"
        )));
    }
    let mut verifier = member.verifier();
    verifier.update(&data);
    if let Err(error) = verifier.finish() {
        return Ok(Outcome::Failed(format!("{error}")));
    }
    Ok(Outcome::Ok {
        size,
        crc32: container5::crc32(&data),
    })
}

// ---------------------------------------------------------------------------
// Drivers (ignored): real archives named by the environment
// ---------------------------------------------------------------------------

fn escape(text: &str) -> String {
    text.chars()
        .flat_map(|c| match c {
            '\t' | '\n' | '\r' | '\\' => c.escape_default().collect::<Vec<_>>(),
            c => vec![c],
        })
        .collect()
}

// The archive sets of `corpus/rar/sets.tsv` in the samples repository that
// `UNRETRO_SAMPLES` names, when `switch` is set: one set per line, volume
// paths relative to `corpus/rar` and separated by tabs.
fn read_list(switch: &str) -> Vec<String> {
    if std::env::var_os(switch).is_none() {
        return Vec::new();
    }
    let corpus = crate::test_samples::corpus("rar").expect("corpus/rar of the samples");
    let text = std::fs::read_to_string(corpus.join("sets.tsv")).expect("sets.tsv");
    text.lines()
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(str::to_string)
        .collect()
}

fn read_volumes(line: &str) -> std::io::Result<Vec<Vec<u8>>> {
    let corpus = crate::test_samples::corpus("rar").unwrap_or_default();
    line.split('\t')
        .map(|volume| std::fs::read(corpus.join(volume)))
        .collect()
}

fn env_number(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

// Aborts the process when one case runs longer than `timeout` seconds.
fn watchdog(timeout: u64) -> Arc<Mutex<(Instant, String)>> {
    let current = Arc::new(Mutex::new((Instant::now(), String::new())));
    let watched = Arc::clone(&current);
    std::thread::spawn(move || {
        loop {
            std::thread::sleep(Duration::from_secs(1));
            let (since, label) = watched.lock().unwrap().clone();
            if !label.is_empty() && since.elapsed() > Duration::from_secs(timeout) {
                println!("E2EHANG\t{label}");
                std::process::abort();
            }
        }
    });
    current
}

fn describe(outcome: &Outcome) -> (&'static str, String, String, String) {
    match outcome {
        Outcome::Ok { size, crc32 } => (
            "ok",
            size.to_string(),
            format!("{crc32:08x}"),
            String::new(),
        ),
        Outcome::NoContents => ("none", String::new(), String::new(), String::new()),
        Outcome::Encrypted => ("encrypted", String::new(), String::new(), String::new()),
        Outcome::Skipped(why) => ("skipped", String::new(), String::new(), why.clone()),
        Outcome::Failed(why) => ("failed", String::new(), String::new(), why.clone()),
    }
}

// RAR_E2E_LIST=1 reads the sets of the samples corpus (`read_list`);
// RAR_E2E_START skips that many lines (to resume after an abort);
// RAR_E2E_TIMEOUT is seconds per set before the process aborts;
// RAR_E2E_LIMIT bounds each member. RAR_E2E_BUILTIN adds the document
// vectors, and RAR_E2E_DUMP=1 writes those to `unretro-rar-vectors` in the
// system's temporary directory, for other tools. Prints one `E2ESET` line per set (number, format or error, ms,
// notes, paths) and one `E2E` line per member:
// set, index, status, size, CRC-32, kind, name, detail.
#[test]
#[ignore = "driver for real archives; set RAR_E2E_LIST or RAR_E2E_BUILTIN"]
fn e2e_list() {
    let limit = env_number("RAR_E2E_LIMIT", DEFAULT_LIMIT);
    let start_at = env_number("RAR_E2E_START", 0) as usize;
    let current = watchdog(env_number("RAR_E2E_TIMEOUT", 600));
    let lines = read_list("RAR_E2E_LIST");
    for (number, line) in lines.iter().enumerate().skip(start_at) {
        match read_volumes(line) {
            Ok(volumes) => report_set(number, line, &volumes, limit, &current),
            Err(error) => println!("E2ESET\t{number}\tunreadable\t0\t{error}\t{line}"),
        }
    }
    if std::env::var("RAR_E2E_BUILTIN").is_ok() {
        let dump = std::env::var_os("RAR_E2E_DUMP")
            .map(|_| std::env::temp_dir().join("unretro-rar-vectors"));
        if let Some(directory) = &dump {
            std::fs::create_dir_all(directory).expect("dump directory");
        }
        for (offset, (name, volumes)) in vectors::sets().into_iter().enumerate() {
            let mut paths = Vec::new();
            for (index, volume) in volumes.iter().enumerate() {
                let stem = name.replace(' ', "_");
                // rar4-container.md 12.2: V3 uses the old naming scheme.
                let file = match (volumes.len(), name.ends_with("V3"), index) {
                    (1, _, _) | (_, true, 0) => format!("{stem}.rar"),
                    (_, true, _) => format!("{stem}.r{:02}", index - 1),
                    _ => format!("{stem}.part{}.rar", index + 1),
                };
                if let Some(directory) = &dump {
                    let path = directory.join(&file);
                    std::fs::write(&path, volume).expect("dump");
                    paths.push(path.to_string_lossy().into_owned());
                } else {
                    paths.push(file);
                }
            }
            report_set(
                lines.len() + offset,
                &paths.join("\t"),
                &volumes,
                limit,
                &current,
            );
        }
    }
}

fn report_set(
    number: usize,
    paths: &str,
    volumes: &[Vec<u8>],
    limit: u64,
    current: &Mutex<(Instant, String)>,
) {
    *current.lock().unwrap() = (Instant::now(), format!("set {number} {paths}"));
    let start = Instant::now();
    let result = catch_unwind(AssertUnwindSafe(|| extract(volumes, limit)));
    let elapsed = start.elapsed().as_millis();
    *current.lock().unwrap() = (Instant::now(), String::new());
    match result {
        Err(_) => println!("E2ESET\t{number}\tpanic\t{elapsed}\t\t{paths}"),
        Ok(Err(error)) => println!("E2ESET\t{number}\terror\t{elapsed}\t{error}\t{paths}"),
        Ok(Ok(listing)) => {
            println!(
                "E2ESET\t{number}\t{}\t{elapsed}\t{}\t{paths}",
                listing.format,
                escape(&listing.notes.join("; "))
            );
            for (index, entry) in listing.entries.iter().enumerate() {
                let (status, size, crc, detail) = describe(&entry.outcome);
                println!(
                    "E2E\t{number}\t{index}\t{status}\t{size}\t{crc}\t{}\t{}\t{}",
                    entry.kind,
                    escape(&entry.name),
                    escape(&detail)
                );
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Mutations
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
enum Op {
    Set {
        volume: usize,
        offset: usize,
        value: u8,
    },
    Truncate {
        volume: usize,
        len: usize,
    },
    DropVolume(usize),
    // Recompute every header checksum reachable by walking the blocks, so a
    // changed field reaches the code behind the checksum test.
    FixChecksums,
}

fn apply(base: &[Vec<u8>], ops: &[Op]) -> Vec<Vec<u8>> {
    let mut volumes = base.to_vec();
    for op in ops {
        match *op {
            Op::Set {
                volume,
                offset,
                value,
            } => {
                if let Some(byte) = volumes.get_mut(volume).and_then(|v| v.get_mut(offset)) {
                    *byte = value;
                }
            }
            Op::Truncate { volume, len } => {
                if let Some(v) = volumes.get_mut(volume) {
                    v.truncate(len);
                }
            }
            Op::DropVolume(volume) => {
                if volume < volumes.len() && volumes.len() > 1 {
                    volumes.remove(volume);
                }
            }
            Op::FixChecksums => {
                for volume in &mut volumes {
                    fix_checksums(volume);
                }
            }
        }
    }
    volumes
}

fn fix_checksums(volume: &mut [u8]) {
    if volume.starts_with(RAR5_SIGNATURE) {
        fix_checksums5(volume);
    } else if volume.starts_with(RAR4_SIGNATURE) {
        fix_checksums4(volume);
    }
}

fn le16(data: &[u8], at: usize) -> Option<usize> {
    let bytes = data.get(at..at.checked_add(2)?)?;
    Some(usize::from(u16::from_le_bytes([bytes[0], bytes[1]])))
}

fn le32(data: &[u8], at: usize) -> Option<u64> {
    let bytes = data.get(at..at.checked_add(4)?)?;
    Some(u64::from(u32::from_le_bytes([
        bytes[0], bytes[1], bytes[2], bytes[3],
    ])))
}

// rar4-container.md 4.1, 4.3, 4.4.
fn fix_checksums4(volume: &mut [u8]) {
    let mut position = RAR4_SIGNATURE.len();
    while let (Some(&kind), Some(flags), Some(size)) = (
        volume.get(position + 2),
        le16(volume, position + 3),
        le16(volume, position + 5),
    ) {
        let Some(end) = position.checked_add(size).filter(|&e| e <= volume.len()) else {
            return;
        };
        if size < 7 {
            return;
        }
        let large = flags & 0x0100 != 0;
        let covered = match kind {
            0x73 => Some(position + 13),
            0x74 if flags & 0x0008 != 0 => le16(volume, position + 26)
                .map(|name| position + if large { 40 } else { 32 } + name),
            0x74 | 0x7a | 0x7b => Some(end),
            _ => None,
        };
        if let Some(covered) = covered.filter(|&c| c <= end && c > position + 2) {
            let crc = container4::crc32(&volume[position + 2..covered]);
            volume[position] = crc as u8;
            volume[position + 1] = (crc >> 8) as u8;
        }
        let data = match kind {
            0x74 | 0x7a => {
                let low = le32(volume, position + 7).unwrap_or(0);
                let high = if large {
                    le32(volume, position + 32).unwrap_or(0)
                } else {
                    0
                };
                low | (high << 32)
            }
            0x77 | 0x78 => le32(volume, position + 7).unwrap_or(0),
            0x72 | 0x73 | 0x7b => 0,
            _ if flags & 0x8000 != 0 => le32(volume, position + 7).unwrap_or(0),
            _ => 0,
        };
        let Some(next) = usize::try_from(data).ok().and_then(|d| end.checked_add(d)) else {
            return;
        };
        if next >= volume.len() {
            return;
        }
        position = next;
    }
}

fn vint(data: &[u8], at: &mut usize) -> Option<u64> {
    let mut value = 0u64;
    for index in 0..10 {
        let byte = *data.get(*at)?;
        *at += 1;
        value |= u64::from(byte & 0x7f).checked_shl(7 * index)?;
        if byte & 0x80 == 0 {
            return Some(value);
        }
    }
    None
}

// rar5-container.md 3.2, 5.1.
fn fix_checksums5(volume: &mut [u8]) {
    let mut position = RAR5_SIGNATURE.len();
    loop {
        let mut cursor = position + 4;
        let Some(size) = vint(volume, &mut cursor) else {
            return;
        };
        if cursor - (position + 4) > 3 {
            return;
        }
        let Some(end) = usize::try_from(size)
            .ok()
            .and_then(|s| cursor.checked_add(s))
            .filter(|&e| e <= volume.len())
        else {
            return;
        };
        let crc = container5::crc32(&volume[position + 4..end]);
        volume[position..position + 4].copy_from_slice(&crc.to_le_bytes());
        let header = &volume[..end];
        let (Some(_kind), Some(flags)) = (vint(header, &mut cursor), vint(header, &mut cursor))
        else {
            return;
        };
        if flags & 1 != 0 && vint(header, &mut cursor).is_none() {
            return;
        }
        let data = if flags & 2 != 0 {
            match vint(header, &mut cursor) {
                Some(data) => data,
                None => return,
            }
        } else {
            0
        };
        let Some(next) = usize::try_from(data).ok().and_then(|d| end.checked_add(d)) else {
            return;
        };
        if next >= volume.len() {
            return;
        }
        position = next;
    }
}

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    fn below(&mut self, bound: usize) -> usize {
        if bound == 0 {
            0
        } else {
            (self.next() % bound as u64) as usize
        }
    }
}

// Positions of the pristine set, split into packed data and everything else.
struct Layout {
    headers: Vec<(usize, usize)>,
    data: Vec<(usize, usize)>,
}

fn layout(base: &[Vec<u8>]) -> Layout {
    let slices: Vec<&[u8]> = base.iter().map(Vec::as_slice).collect();
    let mut is_data: Vec<Vec<bool>> = base.iter().map(|v| vec![false; v.len()]).collect();
    let mut mark = |volume: usize, range: core::ops::Range<usize>| {
        if let Some(flags) = is_data.get_mut(volume).and_then(|v| v.get_mut(range)) {
            flags.fill(true);
        }
    };
    if slices.first().is_some_and(|first| is_rar4(first)) {
        if let Ok(archive) = container4::read_archive(&slices) {
            for piece in archive.members.iter().flat_map(|m| &m.pieces) {
                mark(piece.volume, piece.range.clone());
            }
        }
    } else {
        let archive = container5::read_archive(&slices);
        for piece in archive.members.iter().flat_map(|m| &m.pieces) {
            mark(
                piece.volume,
                piece.offset..piece.offset.saturating_add(piece.len),
            );
        }
    }
    let mut layout = Layout {
        headers: Vec::new(),
        data: Vec::new(),
    };
    for (volume, flags) in is_data.iter().enumerate() {
        for (offset, &data) in flags.iter().enumerate() {
            if data {
                layout.data.push((volume, offset));
            } else {
                layout.headers.push((volume, offset));
            }
        }
    }
    layout
}

fn mutation(rng: &mut Rng, base: &[Vec<u8>], layout: &Layout) -> Vec<Op> {
    let byte_at = |(volume, offset): (usize, usize)| base[volume][offset];
    let pick = |rng: &mut Rng, positions: &[(usize, usize)]| -> Option<(usize, usize)> {
        if positions.is_empty() {
            None
        } else {
            Some(positions[rng.below(positions.len())])
        }
    };
    let anywhere = |rng: &mut Rng| -> Option<(usize, usize)> {
        let volume = rng.below(base.len());
        let len = base.get(volume)?.len();
        (len > 0).then(|| (volume, rng.below(len)))
    };
    let flip = |rng: &mut Rng, at: (usize, usize)| Op::Set {
        volume: at.0,
        offset: at.1,
        value: byte_at(at) ^ (1 << rng.below(8)),
    };
    let field = |rng: &mut Rng, at: (usize, usize)| {
        let old = byte_at(at);
        let value = match rng.below(7) {
            0 => rng.next() as u8,
            1 => old.wrapping_add(1),
            2 => old.wrapping_sub(1),
            3 => 0,
            4 => 0xff,
            5 => old ^ 0x80,
            _ => old ^ (1 << rng.below(8)),
        };
        Op::Set {
            volume: at.0,
            offset: at.1,
            value,
        }
    };
    let mut ops = Vec::new();
    match rng.below(10) {
        0 => {
            for _ in 0..=rng.below(3) {
                if let Some(at) = anywhere(rng) {
                    ops.push(flip(rng, at));
                }
            }
        }
        1 => {
            let volume = rng.below(base.len());
            let len = rng.below(base[volume].len() + 1);
            ops.push(Op::Truncate { volume, len });
        }
        2 | 3 => {
            if let Some(at) = pick(rng, &layout.headers) {
                ops.push(field(rng, at));
            }
            ops.push(Op::FixChecksums);
        }
        4 => {
            if let Some((volume, offset)) = pick(rng, &layout.headers) {
                let value: u32 = match rng.below(5) {
                    0 => 0xffff_ffff,
                    1 => 0x7fff_ffff,
                    2 => 0x8000_0000,
                    3 => 0,
                    _ => rng.next() as u32,
                };
                for (index, byte) in value.to_le_bytes().into_iter().enumerate() {
                    ops.push(Op::Set {
                        volume,
                        offset: offset + index,
                        value: byte,
                    });
                }
            }
            ops.push(Op::FixChecksums);
        }
        5..=7 => {
            for _ in 0..=rng.below(2) {
                if let Some(at) = pick(rng, &layout.data).or_else(|| anywhere(rng)) {
                    ops.push(if rng.below(4) == 0 {
                        field(rng, at)
                    } else {
                        flip(rng, at)
                    });
                }
            }
        }
        8 => {
            if let Some(at) = pick(rng, &layout.headers) {
                ops.push(field(rng, at));
            }
            if let Some(at) = pick(rng, &layout.data) {
                ops.push(flip(rng, at));
            }
            ops.push(Op::FixChecksums);
        }
        _ => {
            if base.len() > 1 && rng.below(2) == 0 {
                ops.push(Op::DropVolume(rng.below(base.len())));
            } else {
                let volume = rng.below(base.len());
                let len = base[volume].len();
                ops.push(Op::Truncate {
                    volume,
                    len: len.saturating_sub(1 + rng.below(16)),
                });
                ops.push(Op::FixChecksums);
            }
        }
    }
    ops
}

#[derive(Default)]
struct FuzzStats {
    cases: u64,
    archive_errors: u64,
    members_ok: u64,
    members_failed: u64,
    panics: Vec<String>,
    slowest: Option<(Duration, String)>,
}

fn hex_of(volumes: &[Vec<u8>]) -> String {
    volumes
        .iter()
        .map(|v| v.iter().map(|b| format!("{b:02x}")).collect::<String>())
        .collect::<Vec<_>>()
        .join(" | ")
}

// Runs `cases` mutations of `base`; `current` is told what is running.
fn fuzz(
    name: &str,
    base: &[Vec<u8>],
    seed: u64,
    cases: u64,
    limit: u64,
    current: Option<&Mutex<(Instant, String)>>,
) -> FuzzStats {
    let layout = layout(base);
    let mut rng = Rng(seed | 1);
    let mut stats = FuzzStats::default();
    for case in 0..cases {
        let ops = mutation(&mut rng, base, &layout);
        let volumes = apply(base, &ops);
        let label = format!("{name} seed {seed} case {case} {ops:?}");
        if let Some(current) = current {
            *current.lock().unwrap() = (Instant::now(), label.clone());
        }
        let start = Instant::now();
        let result = catch_unwind(AssertUnwindSafe(|| extract(&volumes, limit)));
        let elapsed = start.elapsed();
        stats.cases += 1;
        match result {
            Err(_) => {
                let total: usize = volumes.iter().map(Vec::len).sum();
                let bytes = if total <= 2048 {
                    hex_of(&volumes)
                } else {
                    String::from("(large)")
                };
                stats.panics.push(format!("{label} bytes {bytes}"));
            }
            Ok(Err(_)) => stats.archive_errors += 1,
            Ok(Ok(listing)) => {
                for entry in &listing.entries {
                    match entry.outcome {
                        Outcome::Ok { size, .. } => {
                            assert!(size <= limit, "{label}: output above the limit");
                            stats.members_ok += 1;
                        }
                        Outcome::NoContents => {}
                        _ => stats.members_failed += 1,
                    }
                }
            }
        }
        if stats.slowest.as_ref().is_none_or(|(d, _)| elapsed > *d) {
            stats.slowest = Some((elapsed, label));
        }
    }
    stats
}

// RAR_E2E_FUZZ_LIST=1: sets as for `e2e_list`; RAR_E2E_FUZZ_BUILTIN=1 adds the
// document vectors; RAR_E2E_FUZZ_CASES (default 3000), RAR_E2E_FUZZ_SEED,
// RAR_E2E_FUZZ_TIMEOUT seconds per case before the process aborts.
#[test]
#[ignore = "mutation driver; set RAR_E2E_FUZZ_LIST or RAR_E2E_FUZZ_BUILTIN"]
fn e2e_fuzz() {
    let cases = env_number("RAR_E2E_FUZZ_CASES", 3000);
    let seed = env_number("RAR_E2E_FUZZ_SEED", 1);
    let limit = env_number("RAR_E2E_LIMIT", 64 << 20);
    let mut sets: Vec<(String, Vec<Vec<u8>>)> = read_list("RAR_E2E_FUZZ_LIST")
        .into_iter()
        .map(|line| {
            let volumes = read_volumes(&line).unwrap_or_else(|e| panic!("{line}: {e}"));
            (line, volumes)
        })
        .collect();
    if std::env::var("RAR_E2E_FUZZ_BUILTIN").is_ok() {
        sets.extend(
            vectors::sets()
                .into_iter()
                .map(|(name, volumes)| (name.to_string(), volumes)),
        );
    }
    let current = watchdog(env_number("RAR_E2E_FUZZ_TIMEOUT", 120));
    let mut panics = 0;
    for (name, base) in &sets {
        let start = Instant::now();
        let stats = fuzz(name, base, seed, cases, limit, Some(&current));
        *current.lock().unwrap() = (Instant::now(), String::new());
        let (slowest, label) = stats.slowest.unwrap_or_default();
        println!(
            "E2EFUZZ\t{}\tcases {}\tarchive errors {}\tmembers ok {}\tmembers failed {}\tpanics {}\ttotal {} ms\tslowest {} ms\t{}",
            escape(name),
            stats.cases,
            stats.archive_errors,
            stats.members_ok,
            stats.members_failed,
            stats.panics.len(),
            start.elapsed().as_millis(),
            slowest.as_millis(),
            escape(&label)
        );
        for panic in stats.panics.iter().take(20) {
            println!("E2EPANIC\t{}", escape(panic));
        }
        panics += stats.panics.len();
    }
    assert_eq!(panics, 0, "panics under mutation");
}

// ---------------------------------------------------------------------------
// Regression suite: document vectors only
// ---------------------------------------------------------------------------

mod vectors {
    use super::{RAR4_SIGNATURE, RAR5_SIGNATURE, fix_checksums4, fix_checksums5};

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub(super) enum Want {
        Ok(u64, u32),
        NoContents,
        Encrypted,
        // Failed with a reason containing this text.
        Failed(&'static str),
    }

    pub(super) struct Case {
        pub name: &'static str,
        pub volumes: Vec<Vec<u8>>,
        pub want: Vec<(&'static str, Want)>,
    }

    // One member stream as a document prints it.
    struct Raw {
        name: &'static str,
        packed: &'static str,
        size: u32,
        crc32: u32,
        solid: bool,
        // RAR 5 only: algorithm version and dictionary exponent N.
        version: u8,
        exponent: u8,
    }

    const fn raw(name: &'static str, packed: &'static str, size: u32, crc32: u32) -> Raw {
        Raw {
            name,
            packed,
            size,
            crc32,
            solid: false,
            version: 0,
            exponent: 0,
        }
    }

    const fn solid(mut member: Raw) -> Raw {
        member.solid = true;
        member
    }

    pub(super) fn hex(text: &str) -> Vec<u8> {
        let digits: Vec<u8> = text
            .bytes()
            .filter(|b| !b.is_ascii_whitespace())
            .map(|b| (b as char).to_digit(16).expect("hex digit") as u8)
            .collect();
        assert_eq!(digits.len() % 2, 0);
        digits
            .chunks(2)
            .map(|pair| (pair[0] << 4) | pair[1])
            .collect()
    }

    // Minimal RAR4 archive around member streams of unpack version 29,
    // method 0x33 (rar4-container.md 5.1, 6.1; rar29-lz.md 14).
    fn wrap4(members: &[Raw]) -> Vec<u8> {
        let mut out = RAR4_SIGNATURE.to_vec();
        let archive_flags = if members.iter().any(|m| m.solid) {
            8
        } else {
            0
        };
        out.extend([0, 0, 0x73, archive_flags, 0, 13, 0, 0, 0, 0, 0, 0, 0]);
        for member in members {
            let packed = hex(member.packed);
            let flags: u16 = 0x8000 | if member.solid { 0x0010 } else { 0 };
            out.extend([0, 0, 0x74]);
            out.extend(flags.to_le_bytes());
            out.extend((32 + member.name.len() as u16).to_le_bytes());
            out.extend((packed.len() as u32).to_le_bytes());
            out.extend(member.size.to_le_bytes());
            out.push(0);
            out.extend(member.crc32.to_le_bytes());
            out.extend(0x2a43_20a3u32.to_le_bytes());
            out.extend([29, 0x33]);
            out.extend((member.name.len() as u16).to_le_bytes());
            out.extend(0x20u32.to_le_bytes());
            out.extend(member.name.bytes());
            out.extend(packed);
        }
        fix_checksums4(&mut out);
        out
    }

    fn push_vint(out: &mut Vec<u8>, mut value: u64) {
        while value >= 0x80 {
            out.push(value as u8 | 0x80);
            value >>= 7;
        }
        out.push(value as u8);
    }

    // Minimal RAR 5 archive around member streams, after the layout of
    // vector C1 (rar5-container.md 15.1) with method 3.
    fn wrap5(members: &[Raw]) -> Vec<u8> {
        let mut out = RAR5_SIGNATURE.to_vec();
        let archive_flags = if members.iter().any(|m| m.solid) {
            4
        } else {
            0
        };
        out.extend([0, 0, 0, 0, 3, 1, 0, archive_flags]);
        for member in members {
            let packed = hex(member.packed);
            let info = u64::from(member.version)
                | if member.solid { 0x40 } else { 0 }
                | (3 << 7)
                | (u64::from(member.exponent) << 10);
            let mut body = vec![2, 2];
            push_vint(&mut body, packed.len() as u64);
            body.push(4);
            push_vint(&mut body, u64::from(member.size));
            body.extend([0xa4, 0x83, 0x02]);
            body.extend(member.crc32.to_le_bytes());
            push_vint(&mut body, info);
            body.push(1);
            push_vint(&mut body, member.name.len() as u64);
            body.extend(member.name.bytes());
            out.extend([0; 4]);
            push_vint(&mut out, body.len() as u64);
            out.extend(body);
            out.extend(packed);
        }
        out.extend([0, 0, 0, 0, 3, 5, 0, 0]);
        fix_checksums5(&mut out);
        out
    }

    fn case4(name: &'static str, members: &[Raw]) -> Case {
        Case {
            name,
            volumes: vec![wrap4(members)],
            want: members
                .iter()
                .map(|m| (m.name, Want::Ok(u64::from(m.size), m.crc32)))
                .collect(),
        }
    }

    fn case5(name: &'static str, members: &[Raw]) -> Case {
        Case {
            name,
            volumes: vec![wrap5(members)],
            want: members
                .iter()
                .map(|m| (m.name, Want::Ok(u64::from(m.size), m.crc32)))
                .collect(),
        }
    }

    const A: Want = Want::Ok(18, 0xdf8a_7c3b);
    const B: Want = Want::Ok(12, 0xe472_ff82);
    const BIG: Want = Want::Ok(40, 0xe730_b7ea);

    pub(super) fn cases() -> Vec<Case> {
        vec![
            // rar4-container.md 18.1.
            Case {
                name: "rar4 stored G1",
                volumes: vec![hex(R4_G1)],
                want: vec![
                    ("a.txt", Want::Ok(24, 0x0b59_8800)),
                    ("日本語.txt", Want::Ok(9, 0x5119_7f50)),
                    ("emptydir", Want::NoContents),
                ],
            },
            Case {
                name: "rar4 stored A1",
                volumes: vec![hex(R4_A1)],
                want: vec![("A.TXT", A), ("DIR/B.TXT", B)],
            },
            Case {
                name: "rar4 volumes V1",
                volumes: vec![hex(R4_V1_PART1), hex(R4_V1_PART2)],
                want: vec![("A.TXT", A), ("BIG.BIN", BIG), ("B.TXT", B)],
            },
            Case {
                name: "rar4 volumes V2",
                volumes: vec![hex(R4_V1_PART2)],
                want: vec![
                    ("BIG.BIN", Want::Failed("missing volume part")),
                    ("B.TXT", B),
                ],
            },
            Case {
                name: "rar4 volumes V3",
                volumes: vec![hex(R4_V3_RAR), hex(R4_V3_R00)],
                want: vec![("BIG.BIN", BIG)],
            },
            // rar4-container.md 18.1, C1: chain decisions; every packed
            // member holds dummy data that its decoder rejects.
            Case {
                name: "rar4 solid C1",
                volumes: vec![hex(R4_C1)],
                want: vec![
                    ("P1", Want::Failed("decoder: ")),
                    ("S1", Want::Ok(4, 0x5758_779a)),
                    ("DD", Want::NoContents),
                    ("P2", Want::Failed("depends on an unreadable earlier entry")),
                    ("P3", Want::Failed("solid predecessor unavailable")),
                    ("P4", Want::Failed("depends on an unreadable earlier entry")),
                    ("P5", Want::Failed("decoder: ")),
                    ("P6", Want::Encrypted),
                    ("P7", Want::Failed("depends on an unreadable earlier entry")),
                    ("P8", Want::Failed("decoder: ")),
                    ("P9", Want::Failed("depends on an unreadable earlier entry")),
                    ("Z0", Want::Ok(0, 0)),
                    ("PA", Want::Failed("depends on an unreadable earlier entry")),
                ],
            },
            // rar29-lz.md 14.1.
            case4("rar4 lz V1", &[raw("v1", LZ_V1, 6, 0x726e_994c)]),
            case4("rar4 lz V2", &[raw("v2", LZ_V2, 148, 0x7524_0148)]),
            // rar29-lz.md 14.4.
            case4("rar4 ppmd V7a", &[raw("v7a", LZ_V7A, 89, 0xfb88_8f1c)]),
            case4("rar4 ppmd V7b", &[raw("v7b", LZ_V7B, 57, 0xc715_86a4)]),
            case4("rar4 ppmd V7c", &[raw("v7c", LZ_V7C, 30, 0xfe83_5a31)]),
            // rar29-lz.md 14.3.
            case4("rar4 filters V6", &[raw("v6", LZ_V6, 40, 0x58ce_ca18)]),
            case4("rar4 filters V6b", &[raw("v6b", LZ_V6B, 291, 0x77a8_82c1)]),
            // rar29-lz.md 14.2 and 14.3.
            case4(
                "rar4 solid V5",
                &[
                    raw("m1", LZ_V5_M1, 73, 0xdc1b_9f23),
                    solid(raw("m2", LZ_V5_M2, 18, 0x7121_3e5b)),
                    solid(raw("m3", "d5", 0, 0)),
                    solid(raw("m4", LZ_V5_M4, 14, 0x40eb_3ce4)),
                ],
            ),
            case4(
                "rar4 solid V6c",
                &[
                    raw("m1", LZ_V6C_M1, 16, 0x1bb2_0d41),
                    solid(raw("m2", LZ_V6C_M2, 17, 0x989b_2c75)),
                ],
            ),
            case4(
                "rar4 solid V6f",
                &[
                    raw("m1", LZ_V6F_M1, 16, 0xe0e8_ff4d),
                    solid(raw("m2", LZ_V6F_M2, 24, 0xff37_3421)),
                ],
            ),
            // rar29-filters.md 20.4.
            case4("rar4 filters S1", &[raw("m0", F_S1_M0, 60, 0x0bcd_453f)]),
            case4("rar4 filters S2", &[raw("m0", F_S2_M0, 420, 0xac79_8647)]),
            case4("rar4 filters S3", &[raw("m0", F_S3_M0, 120, 0x4ad1_fd39)]),
            case4(
                "rar4 solid filters S4",
                &[
                    raw("m0", F_S4_M0, 80, 0xab7f_cad2),
                    solid(raw("m1", F_S4_M1, 80, 0x7d14_9e4a)),
                ],
            ),
            case4("rar4 filters S5", &[raw("m0", F_S5_M0, 300, 0x06e1_d4f7)]),
            case4(
                "rar4 solid filters S6",
                &[
                    raw("m0", F_S6_M0, 80, 0x7592_bf1a),
                    solid(raw("m1", F_S6_M1, 100, 0xa67b_6346)),
                ],
            ),
            // rar29-ppmd.md 13.1, 13.2.
            case4("rar4 ppmd PV1", &[raw("v1", P_V1, 12, 0x7718_bd00)]),
            case4("rar4 ppmd PV2", &[raw("v2", P_V2, 24, 0xe05b_d988)]),
            case4("rar4 ppmd PV3", &[raw("v3", P_V3, 10, 0xc97d_aaed)]),
            case4("rar4 ppmd PV4", &[raw("v4", P_V4, 726, 0x29f1_3248)]),
            case4("rar4 ppmd PV5a", &[raw("v5a", P_V5A, 48, 0x5911_03fe)]),
            case4("rar4 ppmd PV5b", &[raw("v5b", P_V5B, 48, 0x5911_03fe)]),
            case4("rar4 ppmd PV5c", &[raw("v5c", P_V5C, 24, 0xe05b_d988)]),
            case4("rar4 ppmd PV5d", &[raw("v5d", P_V5D, 48, 0x5911_03fe)]),
            case4("rar4 ppmd PV5e", &[raw("v5e", P_V5E, 51, 0xde6c_42db)]),
            case4("rar4 ppmd PV5f", &[raw("v5f", P_V5F, 24, 0xe05b_d988)]),
            case4("rar4 ppmd PV9", &[raw("v9", P_V9, 341, 0x29e8_9153)]),
            case4(
                "rar4 solid ppmd PV6",
                &[
                    raw("m1", P_V2, 24, 0xe05b_d988),
                    solid(raw("m2", P_V6_M2, 47, 0x423f_b5d5)),
                ],
            ),
            case4("rar4 ppmd filters PV7", &[raw("v7", P_V7, 68, 0x4319_5e29)]),
            case4(
                "rar4 ppmd filters PV7b",
                &[raw("v7b", P_V7B, 132, 0xb1cf_103f)],
            ),
            // rar5-container.md 15.1, 16.1, 16.2.
            Case {
                name: "rar5 stored C1",
                volumes: vec![hex(R5_C1)],
                want: vec![("a.txt", Want::Ok(14, 0x7b55_a718))],
            },
            Case {
                name: "rar5 solid G1",
                volumes: vec![hex(R5_G1)],
                want: G_MEMBERS.to_vec(),
            },
            Case {
                name: "rar5 solid G2",
                volumes: vec![hex(R5_G2)],
                want: G_MEMBERS.to_vec(),
            },
            Case {
                name: "rar5 volumes C8",
                volumes: vec![hex(R5_C8_VOL1), hex(R5_C8_VOL2), hex(R5_C8_VOL3)],
                want: vec![("f.txt", Want::Ok(45, 0xeb50_cc6a)), ("e.txt", B)],
            },
            // rar5-compression.md 13.1.
            case5("rar5 lz V1", &[raw("v1", C5_V1, 6, 0x726e_994c)]),
            case5("rar5 lz V2", &[raw("v2", C5_V2, 93, 0x2997_86d1)]),
            case5(
                "rar5 lz V11",
                &[Raw {
                    version: 1,
                    ..raw("v11", C5_V11, 49, 0x1a20_5ca3)
                }],
            ),
            case5("rar5 filters V9a", &[raw("v9a", C5_V9A, 20, 0xb1f6_5218)]),
            case5("rar5 filters V9b", &[raw("v9b", C5_V9B, 238, 0xba68_49f4)]),
            case5("rar5 filters V9d", &[raw("v9d", C5_V9D, 28, 0x2332_187c)]),
            case5("rar5 filters V9e", &[raw("v9e", C5_V9E, 18, 0xc64f_1d0f)]),
            case5(
                "rar5 solid V10",
                &[
                    raw("m1", C5_V10_M1, 17, 0x56ae_2e2d),
                    solid(raw("m2", C5_V10_M2, 15, 0x119a_c861)),
                ],
            ),
            case5(
                "rar5 solid V14",
                &[
                    Raw {
                        exponent: 5,
                        ..raw("m1", C5_V14_M1, 0, 0)
                    },
                    Raw {
                        exponent: 5,
                        ..solid(raw("m2", C5_V14_M2, 120, 0x0ff8_80ce))
                    },
                ],
            ),
            case5(
                "rar5 solid V17",
                &[
                    raw("m1", C5_V1, 6, 0x726e_994c),
                    solid(raw("m2", C5_V17_M2, 3, 0x3524_41c2)),
                ],
            ),
        ]
    }

    const G_MEMBERS: [(&str, Want); 4] = [
        ("a.txt", Want::Ok(46, 0x065e_149c)),
        ("b.txt", Want::Ok(52, 0x873a_bb5a)),
        ("e.txt", Want::Ok(0, 0)),
        ("c.txt", Want::Ok(30, 0x4140_b65a)),
    ];

    pub(super) fn sets() -> Vec<(&'static str, Vec<Vec<u8>>)> {
        cases().into_iter().map(|c| (c.name, c.volumes)).collect()
    }

    // Expected outputs printed next to the vectors, checked directly
    // against the decoders by the tests below.
    pub(super) const OUTPUTS4: [(&str, &str); 4] = [
        (LZ_V6, LZ_V6_OUT),
        (LZ_V6C_M1, LZ_V6C_OUT1),
        (LZ_V6C_M2, LZ_V6C_OUT2),
        (LZ_V6F_M2, LZ_V6F_OUT2),
    ];
    pub(super) const OUTPUTS5: [(&str, &str); 2] = [(C5_V9D, C5_V9D_OUT), (C5_V9E, C5_V9E_OUT)];

    pub(super) const R4_G1: &str = concat!(
        "526172211a0700cf907300000d00000000000000344074209032001800000018",
        "000000030088590ba320432a14300500a4810000612e747874c0cf0f416d5def",
        "2e42a320432a68656c6c6f2068656c6c6f2068656c6c6f2068656c6c6f0a6da7",
        "7420924d00090000000900000003507f19510f416d5d14301a00a4810000e697",
        "a5e69cace8aa9e2e747874006568e52c679e8a2e00747874f0ffc90b400f416d",
        "5dc90b4019416d5d22095f636a6b206e616d650a201b74e0903b000000000000",
        "00000003000000000f416d5d14300800ed410000656d707479646972f0fffa70",
        "3a0f416d5d92713a0f416d5dfa703ac43d7b00400700"
    );
    pub(super) const R4_C1: &str = concat!(
        "526172211a0700cf907300000d000000000000002e1c7400802200040000000a",
        "0000000000000000a320432a1d330200200000005031deadbeef839574108022",
        "000400000004000000009a775857a320432a1430020020000000533173746f72",
        "d77d74f080220000000000000000000000000000a320432a1430020010000000",
        "444484c07410802200040000000a0000000000000000a320432a1d3302002000",
        "00005032deadbeefd8db7410802200040000000a0000000000000000a320432a",
        "14330200200000005033deadbeefb1657410802200040000000a000000000000",
        "0000a320432a1d330200200000005034deadbeefbda17400802200040000000a",
        "0000000000000000a320432a1d350200200000005035deadbeef6cf87414842a",
        "00040000000a0000000000000000a320432a1d33020020000000503601020304",
        "05060708deadbeef0b347410802200040000000a0000000000000000a320432a",
        "1d330200200000005037deadbeef780d7400802200040000000a000000000000",
        "0000a320432a24330200200000005038deadbeef8a317410802200040000000a",
        "0000000000000000a320432a1d310200200000005039deadbeefedd974108022",
        "0004000000000000000000000000a320432a1d330200200000005a30deadbeef",
        "02e07410802200040000000a0000000000000000a320432a1d33020020000000",
        "5041deadbeef",
    );
    pub(super) const R4_A1: &str = concat!(
        "526172211a0700cf907300000d000000000000004c5174008025001200000012",
        "000000003b7c8adfa320432a1430050020000000412e54585468656c6c6f2068",
        "656c6c6f2068656c6c6f0abe4174008029000c0000000c0000000082ff72e4a3",
        "20432a14300900200000004449525c422e5458547365636f6e642066696c650a"
    );
    pub(super) const R4_V1_PART1: &str = concat!(
        "526172211a07005a6e7311010d000000000000004c5174008025001200000012",
        "000000003b7c8adfa320432a1430050020000000412e54585468656c6c6f2068",
        "656c6c6f2068656c6c6f0ad402740280270019000000280000000037d4a7a5a3",
        "20432a14300700200000004249472e42494e030a11181f262d343b424950575e",
        "656c737a81888f969da4ab1b047b0b400d00ddff03c60000"
    );
    pub(super) const R4_V1_PART2: &str = concat!(
        "526172211a0700197a7311000d00000000000000486674018027000f00000028",
        "00000000eab730e7a320432a14300700200000004249472e42494eb2b9c0c7ce",
        "d5dce3eaf1f8ff060d14e63674008025000c0000000c0000000082ff72e4a320",
        "432a1430050020000000422e5458547365636f6e642066696c650afd3b7b0a40",
        "0d00bea8243d0100"
    );
    pub(super) const R4_V3_RAR: &str = concat!(
        "526172211a0700f1fb7301000d00000000000000f6b774028027001900000028",
        "00000000ffffffffa320432a0f300700200000004249472e42494e030a11181f",
        "262d343b424950575e656c737a81888f969da4ab"
    );
    pub(super) const R4_V3_R00: &str = concat!(
        "526172211a0700f1fb7301000d00000000000000f95f74018027000f00000028",
        "00000000eab730e7a320432a0f300700200000004249472e42494eb2b9c0c7ce",
        "d5dce3eaf1f8ff060d14"
    );
    pub(super) const LZ_V1: &str = "00c8ff455abbf83f0384b65a1d3a";
    pub(super) const LZ_V2: &str = concat!(
        "140d1508be10e958d0250ed06f8f340807c37400fa6868017d3e555e0f04028c",
        "cce39c324a924ef3849272944756216bdb25246e5c6c265c8a1f5f6b85c11c5e",
        "c42a8b4cc956780e9149495e646b918b6f6133704255d12c2a6daf57143e7ba0",
        "3eff91b638f43ce1d55fcf77f0f5bc797ddcbba0"
    );
    pub(super) const LZ_V5_M1: &str = concat!(
        "0c0cd0cbe50f15981060063569622d48b68bbb846fa2000f80186c9ee28de48d",
        "f97f9b6de1ab5c650182573c942c5c84568440df58140ace621e5b42681a2648",
        "2666f28b70ff2f7de1ea00"
    );
    pub(super) const LZ_V5_M2: &str = "db9fe56756a0";
    pub(super) const LZ_V5_M4: &str = "4fdd00cc080109499febbd2f54154687f149220c0478e5ba";
    pub(super) const LZ_V6: &str = concat!(
        "01001487e54f36000002abeeaaaaaab7ffcbd8f7765fdd827761e16c38008501",
        "051d2f019a4180ec27482f09766dd3ea415b5944e8175ce16c914c4e3f770000",
        "0248a010955765b75f863967a6bb049828881046fc73d77e7bf7e2190a63a12a",
        "5b1ae7c2329d10"
    );
    pub(super) const LZ_V6_OUT: &str = concat!(
        "41424344bb752ee69d5308bc6f214f5051525354aba65545fee3a6804d1c9b35",
        "ce66fd9328bc4fe1"
    );
    pub(super) const LZ_V6B: &str = concat!(
        "00d0004fe50f36b6db6d00003d861037a31d774cba4c124c3ed7c6bd87001114",
        "02223a5e03348301d84e905e12ecdba7d482b6b289d02eb9c2d922989c7eee00",
        "37f71cf5df9efdf8022190a63a12a5b1ae7c2329d2b940"
    );
    pub(super) const LZ_V6C_M1: &str = concat!(
        "000c07eccb13302ff0c2165ffdfa6360023139841b0128111069808000000d13",
        "a101c689d280ac9762855cc905c92f8148c8aa981895728881aac95b0020ab6a",
        "03355811a24821b01291f4b8067c10234509abe0"
    );
    pub(super) const LZ_V6C_M2: &str = "f000cba9c1023450976e40";
    pub(super) const LZ_V6C_OUT1: &str = "4142e8fd00000043e9f7010000444546";
    pub(super) const LZ_V6C_OUT2: &str = "464544e8fc00000043e9f6010000444241";
    pub(super) const LZ_V6F_M1: &str = concat!(
        "3cc4be4cfb6aaaaaaaae16a800003fc3ffd6b6c380228501051d2f019a4180ec",
        "27482f09766dd3ea415b5944e8175ce16c914c4e3f7700165b75f863967a6bb6",
        "fc73d77400"
    );
    pub(super) const LZ_V6F_M2: &str = "f3dfbf00443214c74254b635cf84653a20";
    pub(super) const LZ_V6F_OUT2: &str = "616263649b35ce66fd9328bc4fe16f707172737475767778";
    pub(super) const LZ_V7A: &str = concat!(
        "a50067fb8354d735975923b2acfa5831518136a7ccb14dd0d4c633e4afc1259d",
        "f190c22672fc9a4cb9de1091b200"
    );
    pub(super) const LZ_V7B: &str = concat!(
        "0cc8bf4a98ab7f8839602c3c3cbbf82078714ca1d2e50040700332de0e7aa48b",
        "f06c171e8143c547f0bc4b9a3ff902b395af30831c36e8"
    );
    pub(super) const LZ_V7C: &str = concat!(
        "a50061036df570fd6f05a5422b46245c1e274acff00225d8a961293789915410",
        "879ae419b15ce12dd775538bdd2ef757efa9aa94a961cd62a3ad68f086007b5c",
        "506df39f785a6bdb5fb682adef00"
    );
    pub(super) const F_S1_M0: &str = concat!(
        "0c199987e493c493666c9a0d934db229a69a8e049a69a69a84934d34dc894f9c",
        "151f5ff7faba98c8031244d65c6c04a01c1a602000000344e840564bb1432e64",
        "82e497c0a464554c0c4ab94440d564ad801055b5019aac08d12410d80948fa5c",
        "007e08e49a7a2aaecb766250601e6f924c7897b080cc921aea62bf0c72cf4d77",
        "e39ebbfd",
    );
    pub(super) const F_S1_M0_OUT: &str = concat!(
        "00070e151c232a313940474e90e8f21f0000e800000001e8faffff00e800e0ff",
        "ffe910000000e8e8ffff0000e8563412646b727980878f969da4abb2",
    );
    pub(super) const F_S2_M0: &str = concat!(
        "0bcd4c7dd5008080808000000202020202000000080808080800000020202020",
        "20202021b6adb6cb6db2db6cb6db27bbffe816c380008a010d1d2f019a4180ec",
        "27482f09766dd3ea415b5944e8175ce16c914c4e3f7700000923c041016db012",
        "78940109d847019a4195e5720dc26482749324b14006d8384400a8013411dca1",
        "ba01990cc4033119a4066622604d9a400d668e60d030401826c1c8f6e6261378",
        "9208e850bc5a07c6e9f520a9a0ed3733473966907019a39bcf258380c1bd3016",
        "6e233493811609b050183b4dc84c059b88c528e0769390980b37118a59c48042",
        "4843a947ee43346047d44a0dbbd359a486ee05094026c9342476a0306a20ea02",
        "2004a041509e503fe6e128944601bd8b40f0681136c9a1923811419ca89510ee",
        "50662b00209511040262ac668c6aca2640b2671b4b26cc648a6271a2b8006470",
        "7c8894a2b0beccd8e4f0fd0b192735414d596573818f9da9b5c1cdd9e5f01450",
        "f2864707c8896a4b2c0ccd8e4f0ff0d1b2935414d59677583919da9b5c1cfdbe",
        "7f21450f2866727e8a96a4b2c0cedae6f2ff0d1b2937434f5b677583919fabb7",
        "c207a02123c0450016d2c1c025889031816571406690657269936e92c502bd26",
        "e013000569aa01c98ab27122e1988c83e1990901999c46663310cccc664019a2",
        "0ccc333443a602cccd100334519986668a3312602cccd140334619986668c332",
        "2602cccd181334419a29481899a20cd18a0143cc831334519a3140107923f221",
        "57809f3249a060ec2770b7a71e2b359a2d039c76ec924e1b808a8b104ac42620",
        "9240c53d3865c01cfdbe7f2185902a66727e8a98a6b4c2cedae6f3010f1d2b37",
        "434f5b697785939fabb7c3d1dde9f4185902a6874808c98a6b4c2d0dce8f5010",
        "f1d2b3945515d69778593a1adb9c5d1dde9f41c6112c6874808c9aa8b6c4d0dc",
        "e8f503111f2d3945515d6b798795a1adb9c5d3dfea0838c2258d4ed051d35516",
        "d89a5bdd5ee06223e5a768ea6bed6f30f2b475f778fa7bfd41081a48b9a9da0a",
        "3a72aae31b4b7babdc144c84bced1d4d7db5ee265e8ebeef1f5787b031034917",
        "363c42484e555c636a70767c828990979ea4aab0b6bdc4cbd2d8dee4eaf0f606",
        "24713306c7884909eacbac8d4e0ecf9071523313d4955616f7d8b99a5b1bdc9d",
        "7e3ee1048e2660dcf50d253d597591adc5ddf60e2a46627e96aec6defb17334f",
        "677f97afc7dc27dfe7eff1b9ea1a4a82baf32b5b8bbbec245c94ccfd2d5d8dc5",
        "fe366ffa",
    );
    pub(super) const F_S2_M0_OUT: &str = concat!(
        "00cf9df99633eb56c1d60f48bac1c8976c416d108e959ca3abb2b9c0c7ce2b4f",
        "6b808e95958e806b4e2affcd94540dbf690c646b727980878f969da455c6a385",
        "ea3d2aee6298933bbdd7e06cfcf9117f1ffe267625e71d510eb4f810db64b7b1",
        "8cf75a36c8cfd6dde5ecf3fa01080f161e252c333a41484f575e656c737a8188",
        "90979ea5acb3bac1c9d0d7dee5ecf3fa020910171e252c333b424950575e656c",
        "747b828990979ea5adb4bbc2c9d0d7dee6edf4fb020910171f262d343b424950",
        "585f666d747b828991989fa6adb4bbc2cad1d8dfe6edf4fb030a11181f262d34",
        "3c434a51585f666d757c838a91989fa6aeb5bcc3cad1d8dfe7eef5fc030a1118",
        "20272e353c434a515960676e757c838a9299a0a7aeb5bcc3cbd2d9e0e7eef5fc",
        "040b121920272e353d444b525960676e767d848b9299a0a7afb6bdc4cbd2d9e0",
        "e8eff6fd040b121921282f363d444b525a61686f767d848b939aa1a8afb6bdc4",
        "ccd3dae1e8eff6fdfbd1a67b50ef9b45ef99dc5ddd5cdbc2186ec216a1ccf721",
        "4a7979797977dae1e9f0f7fe050c131ade3d893e6c0b908c86d49efa0aa16631",
        "cbcba2a9",
    );
    pub(super) const F_S3_M0: &str = concat!(
        "a30102058c4458707db6fe6e2f054afe9970f505d0433ae38722ced15372bf9d",
        "c1d9223c197dcb1c530b75e57207c418aeb6a48453ddb98130fd1ae4dc354b7e",
        "86474267a637dbdb7d736cc85a9bf57d0f3ca75e481f1319bd356041b98320c9",
        "f183b03eafd2eeae247963b4f6c051a129fa3b47b3cb1be2217e10d2dec49d06",
        "298b3e90f5716c7b3aaf736cfef378b5a0471fc64d4f2957fa30069ed58a79a1",
        "f9a77536e392392708c89e2c293d2f2594bd62a966a84e6cc7bc65c4cbf15fb2",
        "d7c3ed3f00",
    );
    pub(super) const F_S3_M0_OUT: &str = concat!(
        "00070e151c232a313940b94e6b9516d5ba0e5740ed6b7b8f02ab82c0fbce6dd5",
        "d8d53cce99c0eeab1d242b323940474e565d90e8cc1f0000e800000001e8faff",
        "ff00e800e0ffffe9c8ffffffe8e8ffff0000e856341240474e555c636a727980",
        "878e959ca3abb2b9c0c7ced5dce4ebf2f900070e151d242b",
    );
    pub(super) const F_S4_M0: &str = concat!(
        "0c015547e4978926ccd9341b269b64534d34c9269a89a69a6c8a69a89a69a69a",
        "69a0821d64d323d7bffea8a63200a49135971b012807069808000000d13a1015",
        "92ec50cb9920b925f0291915530312ae511035592b6004156d4066ab02344904",
        "3602523e970045b0e84ad228042474bc06690603b09d20bc25d9b74fa9056d65",
        "13a05d7385b2453138fddc00004d45d869b71e7ec20c8040048be33321c19f4a",
        "80088ccc146eacbb0d36e3cfc10c524b34f4535576dd7e18e59e9aedbf3df9e8",
        "fd",
    );
    pub(super) const F_S4_M0_OUT: &str = concat!(
        "00070e151c232a31394090e8f41f0000e800000001e8faffff00e800e0ffffe9",
        "10000000e8e8ffff0000e8563412151c232a313940474e555c636a727980878e",
        "959ca3abb2b9c0c7ced5dce4ebf2f900",
    );
    pub(super) const F_S4_M1: &str = concat!(
        "10019404fe09be4f4f6ed0d4dda19224249a69a64d34d34d34d44d34d34d34d3",
        "244891c8bbbcee4d347c7ffeb65d865aefc72878f3f3ea29aabb0cb5dd1828a2",
        "48038ebcfc10c51c92cd3d14d55db7e39c843f0040040b933321a19ede800446",
        "6609b5bdff4945fa40",
    );
    pub(super) const F_S4_M1_OUT: &str = concat!(
        "c8cfd6dde5ecf3fa0108f1a9db4bbde6987a6c07398dff0cbe8476f4275d9ea5",
        "acb3bac1c9d0d7de90e8d61f0000e800000001e8faffff00e800e0ffffe91000",
        "0000e8e8ffff0000e8563412242b323a",
    );
    pub(super) const F_S5_M0: &str = concat!(
        "0fd04bdd12aadaaadaaadaaadaaadaaadaaadaaae1556d556d556d52da16d0b6",
        "85b42e01c0380700e10b685b42da1700e01c03807085b42da16d0b80700e01c0",
        "384fffd6b2c6daef03172b3f5366ad3128002a0943200e0401c49039140732c0",
        "e88601d14d03a31c07473c12001629031c000f000192291309c80426e5515592",
        "08f091a00ab6c020b90a82443c19901bdc7cfd3d7da3489533dfe3e7eaa56b16",
        "beff3f7fb164cda359636d77838d97a1a9b3bdc604207143489532750234ca56",
        "b16ae5e3f85164cda35b656f79838d97a1abb5bfc804207143891634750234ca",
        "97326ce5e3f851a54ea55b656f79858f99a3abb5bfc808288163891634790a44",
        "ea97326ce9ec0871a54ea55d67717b858f99a3adb7c1ca08288163c99736790a",
        "44ead7b36ee9ec0871e5cfa75d67717b87919ba5adb7c1ca0c309183c997367d",
        "12550ad7b36eedf41891e5cfa75f69737d87919ba5afb9c20018612308143070",
        "fa24aa163068e1dbe831244ca152bed2e6fb13273b4f5f738407475767710286",
        "0e78797a7b2c60d1c7c7d7e7f489942a585add5fe264e769ec6ef1737fa0",
    );
    pub(super) const F_S5_M0_OUT: &str = concat!(
        "00070e151c232a31394000000000000000000000000000c00300500000000000",
        "00000a0000000000040000000000000000000000000000c00300500000000000",
        "00000000000050000000000000000a000000000000000000000081888f969da4",
        "acb3bac1c8cfd6dde5ecf3fa010807000000000000000000000000c003005000",
        "0000010000006e0000000000040007000000000000000000000000c003005000",
        "0000010000003412000050000000000000006e00000000000000010000004950",
        "585f666d747b828991989fa6adb4bbc2cad10000000000000000f0ffffff00c0",
        "03005000000099000000d2000000000004000000000000000000f0ffffff00c0",
        "03005000000099000000000000005000000000000000d2000000000000000200",
        "0000121920272e353d444b52",
    );
    pub(super) const F_S6_M0: &str = concat!(
        "0c015547e4978926ccd9341b269b64534d34c9269a89a69a6c8a69a89a69a69a",
        "69a0821d64d323d7bffea8a63200a49135971b012807069808000000d13a1015",
        "92ec50cb9920b925f0291915530312ae511035592b6004156d4066ab02344904",
        "3602523e970045b0e8499228042474bc06690603b09d20bc25d9b74fa9056d65",
        "13a05d7385b2453138fddc00004d45d869b71e7ec20c8040048be33321c19f4a",
        "80088ccc146eacbb0d36e3cfc10c524b34f4535576dd7e18e59e9aedbf3df9e8",
        "fd",
    );
    pub(super) const F_S6_M0_OUT: &str = concat!(
        "00070e151c232a31394090e8f41f0000e800000001e8faffff00e800e0ffffe9",
        "10000000e8e8ffff0000e8563412151c232acf87960756800ff2c15d6cc1101e",
        "ad7343c1d108c0c7ced5dce4ebf2f900",
    );
    pub(super) const F_S6_M1: &str = concat!(
        "10019404fe09be4f4f6ed0d4dda1b4b696da49a69a6d2da5b4b69a69a69e4891",
        "2269a69a6489123917779dc9a68f8fffd659e9aeec31c90d5af76fe5d3b22926",
        "a69020a29e3d7cfcb2ec32d38f410c51c9351558d10e00100102c8ccc85a677e",
        "c00111998257bedd24c200086038f3ea8a49a8af9f7f565d867fb6ebf0c51c92",
        "e680",
    );
    pub(super) const F_S6_M1_OUT: &str = concat!(
        "c8cfd6dde5ecf3fa01080f161e252c333a41484f575e656c737a818890979ea5",
        "acb3bac1c9d0d7de90e8d61f0000e800000001e8faffff00e800e0ffffe91000",
        "0000e8e8ffff0000e8563412dcaa78b14de97fe953457eb6040b12bc91666d10",
        "b3bac1c8",
    );
    pub(super) const P_V1: &str = "a10061036de8962ed0815a1faad2556c";
    pub(super) const P_V2: &str = "a30067fb8354d7359752711402985d331beb77eb";
    pub(super) const P_V3: &str = "e200414202a734b5369a64698f0a4283ce06ed00";
    pub(super) const P_V4: &str = concat!(
        "a10030042efcd5ce467b88ef249575d53fa7a61d7c5fd307bc7602fd3adb9afa",
        "c67f1fc000",
    );
    pub(super) const P_V5A: &str = "a30067fb8354d7359752711402985d331bcd5200008066e2d90f820000";
    pub(super) const P_V5B: &str = concat!(
        "a30067fb8354d7359752711402985d331bcd520000b00167fb8354d7359752cc",
        "7554dc9537522b5e7300",
    );
    pub(super) const P_V5C: &str = "a30067fb8354d7359752711402985d331bcd52000000bf8867f6a9ffd4";
    pub(super) const P_V5D: &str = concat!(
        "a30067fb8354d7359752711402985d331bcd520000df686a39a6cdccf31b5f2f",
        "00",
    );
    pub(super) const P_V5E: &str = concat!(
        "a30067fb8354d7359752711402985d331bcd520000000bf4845a1f80a7f109ff",
        "cc708066e2d90f820000",
    );
    pub(super) const P_V5F: &str = "a30067fb8354d7359752711402985d331bcd5200008004d2409700";
    pub(super) const P_V9: &str = "a10079fd5ec3c4aa3670ac5a00";
    pub(super) const P_V6_M2: &str = "809ffbcb54df1515a4b73031cf0132000000";
    pub(super) const P_V7: &str = concat!(
        "a30002058cefe6c03c99633b6f7383abadf89825b33f8396d36a388740f7fbae",
        "ed9bc77069582fb82319832a4c0575b017e56bc86cc38463ce41d06634dce1ec",
        "854d2bb7b18b6761c6cebe3134d05c8f924ac403db23c6c0f5d499e8fe20cfc8",
        "4b15e378ccaef2182eb7a05f2e701efbc704ad43c20f41182d978de097ac7c8a",
        "1945ca14b7298124d7c1d8cc0397a844c2be7a4638aa75088d097aae06f68c5f",
        "c25e77f330911a5388df074c65c57bb7922c26aa1eb51302f41b0d018cf94cef",
        "5ffc29d026d8b6400b331775b559ab61308dff33317a653bf5ecf3b6f697e844",
        "a822f31245f2c3df441f548da141874161374a2a960db81b73859332ff2d6863",
        "9fe861af35a2e7df536f33c919b75c92f20e4a58f08f322edf7ee54b4a46be76",
        "a16fbb653334be896756a43e03338aa300",
    );
    pub(super) const P_V7B: &str = concat!(
        "a30002058d166976d76748ea32369e61471d092770b5d39dd5dd3c624b5b509d",
        "0a4fac0496e33fadda5434ed2bafe7ca88fb3d28df4f70a2177b3f7dade66275",
        "289bdb64b6044db84638d6254f346b0bdaa70008959d3e5057aadbe6b492611f",
        "e3fd7e3e4274c5d99effc1d593f4fc4f5f29bae07fcc5991aa65a97c98f6e763",
        "d3edab9d56d311a26c52464201f3cf9356e478241bcbc79582d2e959404e052d",
        "42c8377170236db95e18700e56b3cad073065ce5fe4e32f60abcd913065ca4ea",
        "e43599d211ca057692837335dbce010edf6c29ef885a3e26700ea8b5e5408e21",
        "a886ef32eef8a59c3580b9ca8e72692aee173f2e171416bb65ee5691cd8bd179",
        "16100ec3153876f60a9831fbdc90c3f21a133d81b95643e49d897692044c42c8",
        "758e3bca39fea22fd8b6854cb53113cc18736981d4e4772a1a9e9195",
    );
    // Member streams of rar29-filters.md 20.4 with their delivered output;
    // `true` marks a member that continues the group.
    pub(super) const FILTER_STREAMS: [(&str, &str, bool); 8] = [
        (F_S1_M0, F_S1_M0_OUT, false),
        (F_S2_M0, F_S2_M0_OUT, false),
        (F_S3_M0, F_S3_M0_OUT, false),
        (F_S4_M0, F_S4_M0_OUT, false),
        (F_S4_M1, F_S4_M1_OUT, true),
        (F_S5_M0, F_S5_M0_OUT, false),
        (F_S6_M0, F_S6_M0_OUT, false),
        (F_S6_M1, F_S6_M1_OUT, true),
    ];
    // rar29-ppmd.md 13.4: packed bytes and U of streams that are corrupt.
    pub(super) const PPMD_REJECTED: [(&str, &str, u32); 9] = [
        ("E1", "a00067fb8354d73597422b3b8f7d900eb0d130461100", 24),
        ("E2", "8367fb8354d7359752711402985d331beb77eb", 24),
        ("E3", "a30067fb8354d7359752", 24),
        ("E4", "a300610401bef800", 3),
        ("E5", "a300ffffffff00000000", 1),
        ("E6", "a30061030a7d6d89ef4bbeb93a26", 34),
        ("E7", "a3000206cd12976d3d00", 4),
        ("E9", "a30061036d92479ef9b16c8b20a25e00", 20),
        ("E8", P_V2, 30),
    ];
    pub(super) const P_U1: &str = "a30067fb8354d7359752711402985d32d19c00";
    pub(super) const R5_C1: &str = concat!(
        "526172211a070100c51a333203010000150a5e311402020e040ea4830218a755",
        "7b000105612e74787448656c6c6f2c20776f726c64210a19b23a3503050000"
    );
    pub(super) const R5_G1: &str = concat!(
        "526172211a070100d81b3251030104044e0fdd281b0202ad0006ae00a4830295",
        "4f0d5e9c145e06801b0105612e747874c7b72a254532fa33bff854734fa305a2",
        "406877fb16e49098a71c71d967aff6465b7a2975e14c977e9c2b79e0b0ed7f69",
        "781b0202860006b400a48302954f0d5e5abb3a87c01b0105622e747874471e03",
        "a1bfb08429ee241b02028000068000a48302954f0d5e00000000c01b0105652e",
        "747874615e06ad1b02028600069e00a48302954f0d5e5ab64041c01b0105632e",
        "7478744019039bd8001d77565103050400"
    );
    pub(super) const R5_G2: &str = concat!(
        "526172211a070100d81b325103010404409f568d1b0202ac0006ae00a4830295",
        "4f0d5e9c145e06801b0105612e747874c0b329254523fa33bdc29cd7f089250a",
        "0d0eff62dc8838a71c71d9578f65c15fe0f8900fc19a4f5169f0b100ed7f6978",
        "1b0202860006b400a48302954f0d5e5abb3a87c01b0105622e747874471e03d1",
        "ef6209288dbb1b02029b00068000a48302954f0d5e00000000801b0105652e74",
        "7874c78518330424fa32dfd147ad3881030090c07fe0624147655e5a9fa915ca",
        "9e1b02029000069e00a48302954f0d5e5ab64041c01b0105632e74787441160d",
        "458b04229c11aea61338eeff801d77565103050400"
    );
    pub(super) const R5_C8_VOL1: &str = concat!(
        "526172211a070100532a34450301000115bfa0a3380213230a042da483023414",
        "eca3000105662e74787422020086a6d799ff785e55e977b27717b248f997f2ad",
        "233ec8a11868fdea3cff13a93654686520717569636b208f823d420305000100",
        "00000000"
    );
    pub(super) const R5_C8_VOL2: &str = concat!(
        "526172211a070100ed5534d704010003019a0601b438021b2314042da483020c",
        "c90fd9000105662e74787422020065a2bc8b2074574af34d9b6df90bd4846a70",
        "38a10076907afdb4a4a3e190d81462726f776e20666f78206a756d7073206f76",
        "65728f823d4203050001"
    );
    pub(super) const R5_C8_VOL3: &str = concat!(
        "526172211a07010057043d4e040100030232e103d238020b230f042da483026a",
        "cc50eb000105662e747874220200c97e0ae0f60c80bff15fab26f306c93d5f32",
        "7bd8e49d02b0c0cac9d8104d8ac020746865206c617a7920646f672e0a6f2192",
        "ba1402020c040ca4830282ff72e4000105652e7478747365636f6e642066696c",
        "650a19b23a3503050000"
    );
    pub(super) const C5_V1: &str = "c68e1201200000000000000002eb55ffc75a17b86c";
    pub(super) const C5_V2: &str = concat!(
        "c2a23a33333300000000003044f2d3cf4c3c3cbaffff0371170e3795f26b8201",
        "30fd410805dd61e7558d73e1194e9e2891e8dc1399f37deb263bb6aa00"
    );
    pub(super) const C5_V9A: &str = concat!(
        "c0b329200023000000000030336a117ae541c6613ffe3a633093ffe637977f00",
        "00d028e2f448c3de674ad480"
    );
    pub(super) const C5_V9B: &str = concat!(
        "c7e5780002200000000000002215c0b84730b576cf3166e0d7ff980000000000",
        "0000000000000000000000000000000000000000000000000000000000000000",
        "0000000000000000000000000000000000000000000000000000000000000000",
        "000000000000f0002336140bdeeec0c05f770b00158eeeab234897"
    );
    pub(super) const C5_V9D: &str = concat!(
        "c4b42a0000220000000000203327b1d406a78c76a6c62706bfff302468adf000",
        "136e01a0179def68006896cd40"
    );
    pub(super) const C5_V9D_OUT: &str = "0102030405060f0000eb0000a0e1edffffebfcffffeb0102eb454e44";
    pub(super) const C5_V9E: &str = concat!(
        "c7ba272002200000000000300316cfccab8bbfabffe62f3c0102007d09008180",
        "0000341456789a98765e"
    );
    pub(super) const C5_V9E_OUT: &str = "616263fffbfdf5faeef6e6f7efe8e2ddd97a";
    pub(super) const C5_V10_M1: &str = concat!(
        "c0bf25233033000000000030039c24e1670733b6fffccde32e45bd125dfbc39d",
        "3456789a56dfbc00"
    );
    pub(super) const C5_V10_M2: &str = "471409ceed85800005371000";
    pub(super) const C5_V11: &str = concat!(
        "c4a63802000230000000000333e59e0da22511fff2b339d39de1c19cf8443214",
        "c74254b635cf84653a555d96dd7e18e59e9aedbf1cf5df9efe7c00"
    );
    pub(super) const C5_V14_M1: &str = "c388110332fc22794b02ff1de8f0eccd2acbd290";
    pub(super) const C5_V14_M2: &str = "411f04bb891ec0";
    pub(super) const C5_V17_M2: &str = "451e0118";
}

fn run_cases(prefix: &str) {
    let cases: Vec<_> = vectors::cases()
        .into_iter()
        .filter(|case| case.name.starts_with(prefix))
        .collect();
    assert!(!cases.is_empty(), "no case named {prefix}*");
    for case in cases {
        let listing = extract(&case.volumes, DEFAULT_LIMIT)
            .unwrap_or_else(|error| panic!("{}: {error}", case.name));
        let got: Vec<String> = listing
            .entries
            .iter()
            .map(|entry| format!("{} {:?}", entry.name, entry.outcome))
            .collect();
        assert_eq!(
            listing.entries.len(),
            case.want.len(),
            "{}: {got:?}",
            case.name
        );
        for (entry, (name, want)) in listing.entries.iter().zip(&case.want) {
            assert_eq!(entry.name, *name, "{}", case.name);
            let matches = match (&entry.outcome, want) {
                (Outcome::Ok { size, crc32 }, vectors::Want::Ok(s, c)) => size == s && crc32 == c,
                (Outcome::NoContents, vectors::Want::NoContents) => true,
                (Outcome::Encrypted, vectors::Want::Encrypted) => true,
                (Outcome::Failed(why), vectors::Want::Failed(text)) => why.contains(text),
                _ => false,
            };
            assert!(
                matches,
                "{} {name}: {:?}, want {want:?}",
                case.name, entry.outcome
            );
        }
    }
}

#[test]
fn rar4_stored_and_volumes() {
    run_cases("rar4 stored");
    run_cases("rar4 volumes");
}

#[test]
fn rar4_lz() {
    run_cases("rar4 lz");
}

#[test]
fn rar4_ppmd() {
    run_cases("rar4 ppmd");
}

#[test]
fn rar4_filters() {
    run_cases("rar4 filters");
}

#[test]
fn rar4_solid() {
    run_cases("rar4 solid");
}

#[test]
fn rar5_stored_and_volumes() {
    run_cases("rar5 stored");
    run_cases("rar5 volumes");
}

#[test]
fn rar5_lz() {
    run_cases("rar5 lz");
}

#[test]
fn rar5_filters() {
    run_cases("rar5 filters");
}

#[test]
fn rar5_solid() {
    run_cases("rar5 solid");
}

// A wrong per-part checksum (rar4-container.md 18.1, V1x) and a damaged
// member inside a solid group (15.3; rar5-container.md 12.2).
#[test]
fn damage_is_reported_per_member() {
    let mut v1 = vec![
        vectors::hex(vectors::R4_V1_PART1),
        vectors::hex(vectors::R4_V1_PART2),
    ];
    v1[0][91] = 0x36;
    v1[0][75] = 0x00;
    v1[0][76] = 0xe8;
    let listing = extract(&v1, DEFAULT_LIMIT).unwrap();
    let outcomes: Vec<_> = listing.entries.iter().map(|e| &e.outcome).collect();
    assert!(matches!(outcomes[0], Outcome::Ok { .. }));
    assert_eq!(*outcomes[1], Outcome::Failed("corrupt".into()));
    assert!(matches!(outcomes[2], Outcome::Ok { .. }));

    for prefix in ["rar4 solid V5", "rar5 solid V17"] {
        let case = vectors::cases()
            .into_iter()
            .find(|case| case.name == prefix)
            .unwrap();
        let layout = layout(&case.volumes);
        // First packed byte of the first member.
        let (volume, offset) = layout.data[0];
        let mut damaged = case.volumes.clone();
        damaged[volume][offset] ^= 0xff;
        let listing = extract(&damaged, DEFAULT_LIMIT).unwrap();
        assert!(
            listing
                .entries
                .iter()
                .all(|entry| !matches!(entry.outcome, Outcome::Ok { size, .. } if size > 0)),
            "{prefix}: {:?}",
            listing.entries
        );
    }
}

// The filtered bytes printed in the documents, not only their CRC-32.
#[test]
fn printed_outputs() {
    let mut decoder = rar29::Decoder::new();
    for (index, (packed, output)) in vectors::OUTPUTS4.iter().enumerate() {
        let want = vectors::hex(output);
        // The second and fourth entries continue a group; the fourth needs
        // the first member of V6f, whose output is not printed in hex.
        let solid = index == 2;
        if index == 3 {
            decoder
                .decode_member(&vectors::hex(vectors::LZ_V6F_M1), 16, false, DEFAULT_LIMIT)
                .unwrap();
        }
        let got = decoder
            .decode_member(
                &vectors::hex(packed),
                want.len() as u64,
                solid || index == 3,
                DEFAULT_LIMIT,
            )
            .unwrap();
        assert_eq!(got, want, "RAR 2.9 output {index}");
    }
    for (index, (packed, output)) in vectors::OUTPUTS5.iter().enumerate() {
        let want = vectors::hex(output);
        let params = rar5::MemberParams {
            dict_size: 128 << 10,
            version: 0,
            unpacked_size: Some(want.len() as u64),
            solid: false,
        };
        let got = rar5::Decoder::new()
            .decode_member(&vectors::hex(packed), &params, DEFAULT_LIMIT)
            .unwrap();
        assert_eq!(got, want, "RAR 5 output {index}");
    }
}

// rar29-filters.md 20.4: the delivered bytes, not only their CRC-32.
#[test]
fn printed_filter_stream_outputs() {
    let mut decoder = rar29::Decoder::new();
    for (index, (packed, output, solid)) in vectors::FILTER_STREAMS.iter().enumerate() {
        let want = vectors::hex(output);
        let got = decoder
            .decode_member(
                &vectors::hex(packed),
                want.len() as u64,
                *solid,
                DEFAULT_LIMIT,
            )
            .unwrap();
        assert_eq!(got, want, "filter stream {index}");
    }
}

// rar29-ppmd.md 13.4: corrupt streams fail in the decoder; the unterminated
// member U1 is delivered but its group cannot be continued (11.2).
#[test]
fn rar4_ppmd_rejected_streams() {
    for (name, packed, size) in vectors::PPMD_REJECTED {
        let mut decoder = rar29::Decoder::new();
        let result =
            decoder.decode_member(&vectors::hex(packed), u64::from(size), false, DEFAULT_LIMIT);
        let error = result.expect_err(name).to_string();
        assert!(error.contains("RAR: "), "{name}: {error}");
        assert!(!decoder.can_continue(), "{name}");
    }
    let mut decoder = rar29::Decoder::new();
    let output = decoder
        .decode_member(&vectors::hex(vectors::P_U1), 24, false, DEFAULT_LIMIT)
        .unwrap();
    assert_eq!(container4::crc32(&output), 0xe05b_d988);
    assert!(!decoder.can_continue());
    assert!(
        decoder
            .decode_member(&vectors::hex(vectors::P_V6_M2), 47, true, DEFAULT_LIMIT)
            .is_err()
    );
}

#[test]
fn truncations_never_panic() {
    for (name, volumes) in vectors::sets() {
        for volume in 0..volumes.len() {
            for len in 0..volumes[volume].len() {
                let mut cut = volumes.clone();
                cut[volume].truncate(len);
                let _ = extract(&cut, DEFAULT_LIMIT);
                for v in &mut cut {
                    fix_checksums(v);
                }
                let _ = extract(&cut, DEFAULT_LIMIT);
            }
        }
        let _ = name;
    }
}

#[test]
fn mutations_never_panic() {
    for (name, volumes) in vectors::sets() {
        let stats = fuzz(name, &volumes, 0x5eed, 150, 1 << 20, None);
        assert!(stats.panics.is_empty(), "{:?}", stats.panics);
    }
}
