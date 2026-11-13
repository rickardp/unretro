//! CUE/BIN CD images: a cue sheet naming the track files next to it.
//!
//! Each readable track is one entry: data tracks as `trackNN.img` holding
//! the 2048 user bytes of every sector, audio tracks as `trackNN.pcm` holding
//! headerless 16-bit little-endian stereo samples. See `docs/cue/cue.md`.

use core::sync::atomic::{AtomicUsize, Ordering};

use crate::compat::{Cow, FastMap, String, ToString, Vec, format};
use crate::error::{Error, Result};
use crate::format::ContainerFormat;
use crate::limits::{OutputBudget, checked_output_size};
use crate::{Container, ContainerInfo, Entry};

const MAX_SHEET_SIZE: usize = 1024 * 1024;
const MAX_FILE_COMMANDS: usize = 99;
const MAX_TRACK_NUMBER: u64 = 99;

const RAW_SECTOR: usize = 2352;
const USER_DATA: usize = 2048;
const CDG_SECTOR: usize = 2448;
const HEADERLESS_MODE2_SECTOR: usize = 2336;
const SYNC: [u8; 12] = [
    0, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0,
];

// Entries never exceed the track files they are cut from.
const OUTPUT_BUDGET_RATIO: u64 = 1;

const COMMANDS: [&str; 13] = [
    "FILE",
    "TRACK",
    "INDEX",
    "PREGAP",
    "POSTGAP",
    "FLAGS",
    "CATALOG",
    "ISRC",
    "CDTEXTFILE",
    "TITLE",
    "PERFORMER",
    "SONGWRITER",
    "REM",
];

const fn is_blank(c: char) -> bool {
    c == ' ' || c == '\t'
}

// The sheet as text, cut at the DOS end-of-file mark.
fn decode(data: &[u8]) -> Option<String> {
    if data.is_empty() || data.len() > MAX_SHEET_SIZE {
        return None;
    }
    let utf16 = |bytes: &[u8], unit: fn([u8; 2]) -> u16| {
        if bytes.len() % 2 != 0 {
            return None;
        }
        let units = bytes.chunks_exact(2).map(|pair| unit([pair[0], pair[1]]));
        char::decode_utf16(units)
            .collect::<core::result::Result<String, _>>()
            .ok()
    };
    let mut text = if let Some(rest) = data.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
        core::str::from_utf8(rest).ok()?.to_string()
    } else if let Some(rest) = data.strip_prefix(&[0xFF, 0xFE]) {
        utf16(rest, u16::from_le_bytes)?
    } else if let Some(rest) = data.strip_prefix(&[0xFE, 0xFF]) {
        utf16(rest, u16::from_be_bytes)?
    } else if let Ok(text) = core::str::from_utf8(data) {
        text.to_string()
    } else {
        data.iter().map(|&byte| char::from(byte)).collect()
    };
    if let Some(end) = text.find('\u{1A}') {
        text.truncate(end);
    }
    (!text.contains('\0')).then_some(text)
}

// Non-empty lines as (command word, rest of the line).
fn commands(text: &str) -> impl Iterator<Item = (&str, &str)> {
    text.split(['\n', '\r']).filter_map(|line| {
        let line = line.trim_matches(is_blank);
        if line.is_empty() {
            return None;
        }
        Some(match line.find(is_blank) {
            Some(end) => (&line[..end], line[end..].trim_matches(is_blank)),
            None => (line, ""),
        })
    })
}

fn words(rest: &str) -> impl Iterator<Item = &str> {
    rest.split(is_blank).filter(|word| !word.is_empty())
}

// Name and type of a FILE command. There is no escape character: a quoted
// name runs to the last quote of the line.
fn file_arguments(rest: &str) -> Option<(&str, Option<&str>)> {
    let quoted = rest.strip_prefix('"').filter(|after| after.contains('"'));
    let (name, file_type) = if let Some(after) = quoted {
        let last = after.rfind('"')?;
        (&after[..last], words(&after[last + 1..]).next())
    } else if let Some(split) = rest.rfind(is_blank) {
        let name = rest[..split].trim_matches(is_blank);
        (
            name.strip_prefix('"').unwrap_or(name),
            Some(&rest[split + 1..]),
        )
    } else {
        (rest, None)
    };
    (!name.is_empty()).then_some((name, file_type))
}

// Unsigned decimal; values too long for the type saturate.
fn decimal(word: &str) -> Option<u64> {
    if word.is_empty() || !word.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    Some(word.bytes().fold(0u64, |value, digit| {
        value
            .saturating_mul(10)
            .saturating_add(u64::from(digit - b'0'))
    }))
}

// `mm:ss:ff` in frames. Fields have 1 to 4 digits and are not range-checked.
fn time_frames(word: &str) -> Option<u64> {
    let mut fields = word.split(':').map(|field| {
        (1..=4)
            .contains(&field.len())
            .then(|| decimal(field))
            .flatten()
    });
    let (minutes, seconds, frames) = (fields.next()??, fields.next()??, fields.next()??);
    if fields.next().is_some() {
        return None;
    }
    Some(minutes * 4500 + seconds * 75 + frames)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    Audio,
    // Audio followed by 96 bytes of sub-channel data per sector.
    Cdg,
    // User data only, 2048 bytes per sector.
    Cooked,
    // 2352-byte sectors; `fallback` is the user-data offset for a sector
    // without a sync pattern and mode byte of its own.
    Raw { fallback: usize },
    // Mode 2 sectors without sync and header.
    HeaderlessMode2,
    // Not readable; the sector size, when known, still locates later tracks.
    Unsupported { sector_size: Option<usize> },
}

impl Mode {
    fn parse(word: &str) -> Self {
        const KNOWN: [(&str, Mode); 10] = [
            ("AUDIO", Mode::Audio),
            ("CDG", Mode::Cdg),
            ("MODE1/2048", Mode::Cooked),
            ("MODE2/2048", Mode::Cooked),
            ("MODE1/2352", Mode::Raw { fallback: 16 }),
            ("MODE2/2352", Mode::Raw { fallback: 24 }),
            ("CDI/2352", Mode::Raw { fallback: 24 }),
            ("MODE2/2336", Mode::HeaderlessMode2),
            ("CDI/2336", Mode::HeaderlessMode2),
            (
                "MODE2/2324",
                Mode::Unsupported {
                    sector_size: Some(2324),
                },
            ),
        ];
        if let Some(&(_, mode)) = KNOWN
            .iter()
            .find(|(name, _)| word.eq_ignore_ascii_case(name))
        {
            return mode;
        }
        let sector_size = word
            .split_once('/')
            .filter(|(kind, _)| !kind.is_empty())
            .and_then(|(_, size)| decimal(size))
            .filter(|size| (2048..=2448).contains(size))
            .and_then(|size| usize::try_from(size).ok());
        Self::Unsupported { sector_size }
    }

    const fn sector_size(self) -> Option<usize> {
        match self {
            Self::Audio | Self::Raw { .. } => Some(RAW_SECTOR),
            Self::Cdg => Some(CDG_SECTOR),
            Self::Cooked => Some(USER_DATA),
            Self::HeaderlessMode2 => Some(HEADERLESS_MODE2_SECTOR),
            Self::Unsupported { sector_size } => sector_size,
        }
    }

    const fn is_audio(self) -> bool {
        matches!(self, Self::Audio | Self::Cdg)
    }

    const fn output_per_sector(self) -> usize {
        if self.is_audio() {
            RAW_SECTOR
        } else {
            USER_DATA
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
enum FileType {
    Binary,
    Motorola,
    Other(String),
}

// An INDEX 00 or INDEX 01 position in the file current at that command.
#[derive(Clone, Copy, Debug)]
struct Mark {
    // Position among all marks of the sheet.
    order: usize,
    track: usize,
    data_start: bool,
    frame: u64,
}

#[derive(Debug)]
struct SheetFile {
    name: String,
    file_type: FileType,
    marks: Vec<Mark>,
}

#[derive(Debug)]
struct SheetTrack {
    number: u8,
    mode: Mode,
    mode_word: String,
    has_index0: bool,
    has_index1: bool,
    // INDEX 01 lies in a MOTOROLA file.
    swap: bool,
}

#[derive(Debug)]
struct Sheet {
    files: Vec<SheetFile>,
    tracks: Vec<SheetTrack>,
}

fn invalid(what: &str) -> Error {
    Error::invalid_format(format!("Invalid cue sheet: {what}"))
}

fn parse(text: &str) -> Result<Sheet> {
    let mut sheet = Sheet {
        files: Vec::new(),
        tracks: Vec::new(),
    };
    let mut file_commands = 0usize;
    let mut marks = 0usize;
    let mut current_file: Option<usize> = None;

    for (command, rest) in commands(text) {
        if command.eq_ignore_ascii_case("FILE") {
            file_commands += 1;
            if file_commands > MAX_FILE_COMMANDS {
                return Err(invalid("more than 99 FILE commands"));
            }
            let (name, file_type) =
                file_arguments(rest).ok_or_else(|| invalid("FILE without a name"))?;
            // FILE commands with the same name are one file.
            let index = sheet.files.iter().position(|file| file.name == name);
            current_file = Some(index.unwrap_or_else(|| {
                let file_type = match file_type {
                    None => FileType::Binary,
                    Some(word) if word.eq_ignore_ascii_case("BINARY") => FileType::Binary,
                    Some(word) if word.eq_ignore_ascii_case("MOTOROLA") => FileType::Motorola,
                    Some(word) => FileType::Other(word.to_ascii_uppercase()),
                };
                sheet.files.push(SheetFile {
                    name: name.to_string(),
                    file_type,
                    marks: Vec::new(),
                });
                sheet.files.len() - 1
            }));
        } else if command.eq_ignore_ascii_case("TRACK") {
            let mut arguments = words(rest);
            let number = arguments
                .next()
                .and_then(decimal)
                .ok_or_else(|| invalid("TRACK without a number"))?;
            let mode_word = arguments
                .next()
                .ok_or_else(|| invalid("TRACK without a mode"))?;
            if current_file.is_none() {
                return Err(invalid("TRACK before the first FILE"));
            }
            let previous = sheet
                .tracks
                .last()
                .map_or(0, |track| u64::from(track.number));
            if number > MAX_TRACK_NUMBER || number <= previous {
                return Err(invalid("track numbers must be 1 to 99 and increasing"));
            }
            sheet.tracks.push(SheetTrack {
                number: u8::try_from(number).map_err(|_| invalid("track number"))?,
                mode: Mode::parse(mode_word),
                mode_word: mode_word.to_ascii_uppercase(),
                has_index0: false,
                has_index1: false,
                swap: false,
            });
        } else if command.eq_ignore_ascii_case("INDEX") {
            let mut arguments = words(rest);
            let number = arguments.next().and_then(decimal);
            let frame = arguments.next().and_then(time_frames);
            let (Some(number), Some(frame)) = (number, frame) else {
                continue;
            };
            let (Some(file), Some(track)) = (current_file, sheet.tracks.len().checked_sub(1))
            else {
                continue;
            };
            // Only the first INDEX 00 and INDEX 01 of a track count.
            let seen = match number {
                0 => &mut sheet.tracks[track].has_index0,
                1 => &mut sheet.tracks[track].has_index1,
                _ => continue,
            };
            if !core::mem::replace(seen, true) {
                if number == 1 {
                    sheet.tracks[track].swap = sheet.files[file].file_type == FileType::Motorola;
                }
                marks += 1;
                sheet.files[file].marks.push(Mark {
                    order: marks,
                    track,
                    data_start: number == 1,
                    frame,
                });
            }
        }
    }
    Ok(sheet)
}

/// Check if data looks like a cue sheet.
///
/// A cue sheet has no signature: this requires text whose first line is a
/// cue sheet command and which has a FILE command followed by a TRACK
/// command.
#[must_use]
pub fn is_cue_sheet(data: &[u8]) -> bool {
    let Some(text) = decode(data) else {
        return false;
    };
    let mut lines = commands(&text).peekable();
    let known_first = lines.peek().is_some_and(|(command, _)| {
        COMMANDS
            .iter()
            .any(|known| command.eq_ignore_ascii_case(known))
    });
    if !known_first {
        return false;
    }
    let mut seen_file = false;
    for (command, rest) in lines {
        if command.eq_ignore_ascii_case("FILE") {
            seen_file |= file_arguments(rest).is_some();
        } else if seen_file && command.eq_ignore_ascii_case("TRACK") {
            let mut arguments = words(rest);
            if arguments.next().and_then(decimal).is_some() && arguments.next().is_some() {
                return true;
            }
        }
    }
    false
}

// A single path component that may be passed to the sibling lookup.
fn usable_component(component: &str) -> bool {
    !component.is_empty()
        && component != "."
        && component != ".."
        && !component.contains([':', '/', '\\', '\0'])
}

// Stem and extension; a name without a dot, or whose only dot leads or
// trails it, has no extension.
fn split_extension(name: &str) -> (&str, Option<&str>) {
    match name.rsplit_once('.') {
        Some((stem, extension)) if !stem.is_empty() && !extension.is_empty() => {
            (stem, Some(extension))
        }
        _ => (name, None),
    }
}

fn last_component(path: &str) -> &str {
    path.rsplit(['/', '\\']).next().unwrap_or(path)
}

// Names to ask the lookup for, in order, for the name a FILE command gives.
// All are relative to the sheet's directory and none can leave it.
// `sheet_stem` is given for sheets with a single readable track file.
fn candidates(written: &str, sheet_stem: Option<&str>) -> Vec<String> {
    let normalized = written.replace('\\', "/");
    let components: Vec<&str> = normalized.split('/').collect();
    let first = components.first().copied().unwrap_or("");
    let absolute = normalized.starts_with('/')
        || (first.len() == 2 && first.ends_with(':') && first.as_bytes()[0].is_ascii_alphabetic());
    let base = components
        .last()
        .copied()
        .filter(|base| usable_component(base));

    let mut names: Vec<String> = Vec::new();
    let mut push = |name: String| {
        if !names.contains(&name) {
            names.push(name);
        }
    };
    if !absolute && components.len() > 1 && components.iter().all(|c| usable_component(c)) {
        push(components.join("/"));
    }
    if let Some(base) = base {
        push(base.to_string());
        push(base.to_ascii_lowercase());
        push(base.to_ascii_uppercase());
    }
    if let Some(stem) = sheet_stem.filter(|stem| usable_component(stem)) {
        if let Some(extension) = base.and_then(|base| split_extension(base).1) {
            push(format!("{stem}.{extension}"));
            push(format!("{stem}.{}", extension.to_ascii_lowercase()));
            push(format!("{stem}.{}", extension.to_ascii_uppercase()));
        }
        push(format!("{stem}.bin"));
        push(format!("{stem}.BIN"));
    }
    names
}

// The stem of the sheet's own name, for sheets with exactly one track file
// of a readable type.
fn single_file_stem<'a>(sheet: &Sheet, sheet_path: &'a str) -> Option<&'a str> {
    let readable = sheet
        .files
        .iter()
        .filter(|file| !matches!(file.file_type, FileType::Other(_)))
        .count();
    (readable == 1).then(|| split_extension(last_component(sheet_path)).0)
}

// The part of a file that belongs to one track, from its first mark there.
struct Span {
    track: usize,
    frame: u64,
    data_frame: Option<u64>,
}

// `None` when the marks of a file go backwards.
fn spans(marks: &[Mark]) -> Option<Vec<Span>> {
    let mut spans: Vec<Span> = Vec::new();
    let mut previous = 0u64;
    for mark in marks {
        if mark.frame < previous {
            return None;
        }
        previous = mark.frame;
        match spans.last_mut() {
            Some(span) if span.track == mark.track => {
                // INDEX 00 written after INDEX 01 of the same track.
                if !mark.data_start {
                    return None;
                }
                span.data_frame = Some(mark.frame);
            }
            _ => spans.push(Span {
                track: mark.track,
                frame: mark.frame,
                data_frame: mark.data_start.then_some(mark.frame),
            }),
        }
    }
    Some(spans)
}

// Where a track's data lies in its file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Extent {
    start: usize,
    sectors: usize,
}

// The extent, or the reason there is none, of every track whose INDEX 01
// is in the file of `spans`, which is `len` bytes long.
fn layout(
    spans: &[Span],
    tracks: &[SheetTrack],
    len: usize,
) -> Vec<(usize, core::result::Result<Extent, String>)> {
    let len = len as u64;
    let mut results = Vec::new();
    // Byte offset of the current span; unknown after a track of unknown
    // sector size.
    let mut start = spans.first().and_then(|span| {
        let size = tracks[span.track].mode.sector_size()?;
        span.frame.checked_mul(size as u64)
    });
    for (index, span) in spans.iter().enumerate() {
        let track = &tracks[span.track];
        let size = track.mode.sector_size().map(|size| size as u64);
        let next_start = spans.get(index + 1).and_then(|next| {
            (next.frame - span.frame)
                .checked_mul(size?)?
                .checked_add(start?)
        });
        if let Some(data_frame) = span.data_frame {
            let extent = || {
                if matches!(track.mode, Mode::Unsupported { .. }) {
                    return Err(format!("has unsupported mode {}", clip(&track.mode_word)));
                }
                let (Some(start), Some(size)) = (start, size) else {
                    return Err("follows a track of unknown sector size".to_string());
                };
                let data_start = (data_frame - span.frame)
                    .checked_mul(size)
                    .and_then(|offset| offset.checked_add(start))
                    .filter(|&data_start| data_start < len)
                    .ok_or_else(|| "starts beyond the end of its file".to_string())?;
                let end = next_start.map_or(len, |next| next.min(len));
                let sectors = end.saturating_sub(data_start) / size;
                match (usize::try_from(data_start), usize::try_from(sectors)) {
                    (Ok(start), Ok(sectors)) if sectors > 0 => Ok(Extent { start, sectors }),
                    _ => Err("is empty".to_string()),
                }
            };
            results.push((span.track, extent()));
        }
        start = next_start;
    }
    results
}

// Untrusted sheet text made fit for a message.
fn clip(text: &str) -> String {
    let mut clipped: String = text
        .chars()
        .take(64)
        .map(|c| if c.is_control() { '?' } else { c })
        .collect();
    if text.chars().nth(64).is_some() {
        clipped.push_str("...");
    }
    clipped
}

// Tracks left out of the listing.
#[derive(Clone, Default)]
struct Skipped {
    count: usize,
    first: Option<String>,
}

impl Skipped {
    fn note(&mut self, message: String) {
        self.count += 1;
        self.first.get_or_insert(message);
    }

    fn into_message(self) -> Option<String> {
        let first = self.first?;
        Some(if self.count > 1 {
            format!("CUE: {first} (and {} more problems)", self.count - 1)
        } else {
            format!("CUE: {first}")
        })
    }
}

struct Track {
    number: u8,
    file: usize,
    extent: Extent,
    mode: Mode,
    // Audio samples stored most significant byte first.
    swap: bool,
}

// A track file after the lookup. FILE commands that resolve to the same
// name are one file.
struct Group {
    // The candidate that was found, relative to the sheet's directory.
    name: String,
    // The name the sheet gives, for messages.
    label: String,
    // In sheet order.
    marks: Vec<Mark>,
}

struct Resolved {
    groups: Vec<Group>,
    // Tracks that cannot be read, whatever the files hold.
    problems: Vec<(usize, String)>,
}

// Asks `lookup` for the candidates of every file that has readable tracks,
// each name at most once. A successful lookup adds one group.
fn resolve(
    sheet: &Sheet,
    sheet_stem: Option<&str>,
    lookup: &mut dyn FnMut(&str) -> bool,
) -> Resolved {
    enum Found {
        Group(usize),
        CaseClash,
        Nowhere,
    }
    let mut groups: Vec<Group> = Vec::new();
    let mut problems = Vec::new();
    let mut missing: Vec<String> = Vec::new();
    for file in &sheet.files {
        let label = clip(&file.name);
        let Some(spans) = spans(&file.marks) else {
            problems.extend(file.marks.iter().map(|mark| {
                (
                    mark.track,
                    format!("is in file '{label}' whose INDEX times go backwards"),
                )
            }));
            continue;
        };
        let with_data = || spans.iter().filter(|span| span.data_frame.is_some());
        if with_data().next().is_none() {
            continue;
        }
        if let FileType::Other(file_type) = &file.file_type {
            let file_type = clip(file_type);
            problems.extend(with_data().map(|span| {
                (
                    span.track,
                    format!("is in file '{label}' of unsupported type {file_type}"),
                )
            }));
            continue;
        }
        let mut found = Found::Nowhere;
        for candidate in candidates(&file.name, sheet_stem) {
            if let Some(index) = groups.iter().position(|group| group.name == candidate) {
                found = Found::Group(index);
                break;
            }
            // Whether this is the same file depends on the lookup.
            if groups
                .iter()
                .any(|group| group.name.eq_ignore_ascii_case(&candidate))
            {
                found = Found::CaseClash;
                break;
            }
            if missing.contains(&candidate) {
                continue;
            }
            if lookup(&candidate) {
                found = Found::Group(groups.len());
                groups.push(Group {
                    name: candidate,
                    label: label.clone(),
                    marks: Vec::new(),
                });
                break;
            }
            missing.push(candidate);
        }
        let problem = match found {
            Found::Group(index) => {
                groups[index].marks.extend_from_slice(&file.marks);
                continue;
            }
            Found::CaseClash => {
                format!("is in file '{label}' named like another file in different letter case")
            }
            Found::Nowhere => format!("file '{label}' was not found"),
        };
        problems.extend(with_data().map(|span| (span.track, problem.clone())));
    }
    for group in &mut groups {
        group.marks.sort_by_key(|mark| mark.order);
    }
    Resolved { groups, problems }
}

// The extent, or the reason there is none, of the tracks of a file that is
// `len` bytes long.
fn group_tracks(
    group: &Group,
    tracks: &[SheetTrack],
    len: usize,
) -> Vec<(usize, core::result::Result<Extent, String>)> {
    let Some(spans) = spans(&group.marks) else {
        let label = &group.label;
        return group
            .marks
            .iter()
            .map(|mark| {
                (
                    mark.track,
                    Err(format!(
                        "is in file '{label}' whose INDEX times go backwards"
                    )),
                )
            })
            .collect();
    };
    let mut results = layout(&spans, tracks, len);
    for (track, extent) in &mut results {
        let Ok(found) = extent else { continue };
        let size =
            (found.sectors as u64).saturating_mul(tracks[*track].mode.output_per_sector() as u64);
        if checked_output_size(size, "track").is_err() {
            *extent = Err("exceeds the size limit".to_string());
        }
    }
    results
}

// What opening a sheet will read, worked out from names alone.
struct Plan {
    // The sheet's directory in its container: empty or ending in a slash.
    directory: String,
    tracks: Vec<SheetTrack>,
    groups: Vec<Group>,
    // Per track: unreadable whatever the files hold.
    blocked: Vec<bool>,
}

impl Plan {
    // `None` when the loader would not open `data` as a cue sheet.
    fn new(sheet_path: &str, data: &[u8], exists: &dyn Fn(&str) -> bool) -> Option<Self> {
        if !is_cue_sheet(data) {
            return None;
        }
        let sheet = parse(&decode(data)?).ok()?;
        let name = last_component(sheet_path);
        let directory = &sheet_path[..sheet_path.len() - name.len()];
        let resolved = resolve(&sheet, single_file_stem(&sheet, name), &mut |candidate| {
            exists(&format!("{directory}{candidate}"))
        });
        let mut blocked: Vec<bool> = sheet.tracks.iter().map(|_| false).collect();
        for (track, _) in &resolved.problems {
            blocked[*track] = true;
        }
        for group in &resolved.groups {
            if spans(&group.marks).is_none() {
                for mark in &group.marks {
                    blocked[mark.track] = true;
                }
            }
        }
        Some(Self {
            directory: directory.to_string(),
            tracks: sheet.tracks,
            groups: resolved.groups,
            blocked,
        })
    }

    fn path_of(&self, group: &Group) -> String {
        format!("{}{}", self.directory, group.name)
    }

    // Whether the sheet lists a track from the file at `path` with the
    // contents `data` as its group `group`.
    fn reads<'c>(
        &self,
        group: usize,
        path: &str,
        data: &[u8],
        read: &dyn Fn(&str) -> Option<Cow<'c, [u8]>>,
    ) -> bool {
        let Some(group) = self.groups.get(group) else {
            return false;
        };
        let found = self.path_of(group);
        // A lookup that ignores letter case may pick another file than this.
        let same = found == path
            || (found.eq_ignore_ascii_case(path)
                && read(&found).is_some_and(|other| *other == *data));
        same && group_tracks(group, &self.tracks, data.len())
            .iter()
            .any(|(track, extent)| extent.is_ok() && !self.blocked[*track])
    }
}

struct KnownSheet {
    // `None` for a file that is not opened as a cue sheet.
    plan: Option<Plan>,
    // The container has yielded the sheet.
    visited: bool,
    // The sheet was opened and its tracks were visited.
    opened: bool,
}

/// An entry held back because a cue sheet not visited yet reads it.
pub(crate) struct HeldBack {
    pub(crate) path: String,
    pub(crate) container_path: String,
    // Relative to the container.
    pub(crate) file: String,
    sheets: Vec<usize>,
}

// Sheets recorded per track file; further ones do not hide it.
const MAX_READERS: usize = 8;

/// The cue sheets of one container and the track files they read, so that a
/// track file is not listed a second time on its own (and is, when its sheet
/// turns out not to be opened). Paths are relative to the container.
#[derive(Default)]
pub(crate) struct SheetIndex {
    sheets: Vec<KnownSheet>,
    by_path: FastMap<String, usize>,
    // Lower-case track file path to the (sheet, group) pairs reading it.
    readers: FastMap<String, Vec<(usize, usize)>>,
    // Directories on disk that were searched for sheets.
    #[cfg(feature = "std")]
    directories: FastMap<String, ()>,
    held_back: Vec<HeldBack>,
}

fn has_cue_extension(name: &str) -> bool {
    split_extension(name)
        .1
        .is_some_and(|extension| extension.eq_ignore_ascii_case("cue"))
}

impl SheetIndex {
    /// Records the sheet at `sheet_path`, unless it is known already.
    #[cfg(feature = "std")]
    pub(crate) fn add_sheet(
        &mut self,
        sheet_path: &str,
        data: Option<&[u8]>,
        exists: &dyn Fn(&str) -> bool,
    ) {
        self.learn(sheet_path, data, exists);
    }

    fn learn(
        &mut self,
        sheet_path: &str,
        data: Option<&[u8]>,
        exists: &dyn Fn(&str) -> bool,
    ) -> usize {
        if let Some(&index) = self.by_path.get(sheet_path) {
            return index;
        }
        let index = self.sheets.len();
        let plan = data.and_then(|data| Plan::new(sheet_path, data, exists));
        if let Some(plan) = &plan {
            for (number, group) in plan.groups.iter().enumerate() {
                let key = plan.path_of(group).to_ascii_lowercase();
                let readers = self.readers.entry(key).or_default();
                if readers.len() < MAX_READERS {
                    readers.push((index, number));
                }
            }
        }
        self.sheets.push(KnownSheet {
            plan,
            visited: false,
            opened: false,
        });
        self.by_path.insert(sheet_path.to_string(), index);
        index
    }

    /// Whether the directory of `file` is seen for the first time.
    #[cfg(feature = "std")]
    pub(crate) fn enters_directory(&mut self, file: &str) -> bool {
        let directory = &file[..file.len() - last_component(file).len()];
        if self.directories.contains_key(directory) {
            return false;
        }
        self.directories.insert(directory.to_string(), ());
        true
    }

    /// Whether `entry`, at `file` in its container, must not be visited now:
    /// a sheet that was opened has listed its tracks, or a sheet still to be
    /// visited will, in which case the entry is held back.
    pub(crate) fn hides<'c>(
        &mut self,
        file: &str,
        entry: &Entry<'_>,
        exists: &dyn Fn(&str) -> bool,
        read: &dyn Fn(&str) -> Option<Cow<'c, [u8]>>,
    ) -> bool {
        let name = last_component(file);
        let directory = &file[..file.len() - name.len()];
        if has_cue_extension(name) {
            let index = self.learn(file, Some(entry.data), exists);
            self.sheets[index].visited = true;
            return false;
        }
        // A sheet named after the file, also for `name (Track 2).bin`.
        let stem = split_extension(name).0;
        let dump_stem = stem
            .strip_suffix(')')
            .and_then(|rest| {
                rest.trim_end_matches(|c: char| c.is_ascii_digit())
                    .strip_suffix(" (Track ")
            })
            .filter(|rest| rest.len() + " (Track )".len() < stem.len());
        for stem in [Some(stem), dump_stem].into_iter().flatten() {
            for extension in ["cue", "CUE"] {
                let sheet_path = format!("{directory}{stem}.{extension}");
                if self.by_path.contains_key(&sheet_path) {
                    break;
                }
                if exists(&sheet_path) {
                    let data = read(&sheet_path);
                    self.learn(&sheet_path, data.as_deref(), exists);
                    break;
                }
            }
        }

        let Some(readers) = self.readers.get(&file.to_ascii_lowercase()) else {
            return false;
        };
        let sheets: Vec<usize> = readers
            .iter()
            .filter(|&&(sheet, group)| {
                self.sheets[sheet]
                    .plan
                    .as_ref()
                    .is_some_and(|plan| plan.reads(group, file, entry.data, read))
            })
            .map(|&(sheet, _)| sheet)
            .collect();
        if sheets.iter().any(|&sheet| self.sheets[sheet].opened) {
            return true;
        }
        if sheets.iter().all(|&sheet| self.sheets[sheet].visited) {
            return false;
        }
        self.held_back.push(HeldBack {
            path: entry.path.to_string(),
            container_path: entry.container_path.to_string(),
            file: file.to_string(),
            sheets,
        });
        true
    }

    /// Notes that the sheet at `sheet_path` was opened as a cue sheet.
    pub(crate) fn sheet_opened(&mut self, sheet_path: &str) {
        if let Some(&index) = self.by_path.get(sheet_path) {
            self.sheets[index].opened = true;
        }
    }

    /// The entries held back for sheets that were not opened after all.
    pub(crate) fn take_unlisted(&mut self) -> Vec<HeldBack> {
        let mut held_back = core::mem::take(&mut self.held_back);
        held_back.retain(|held| !held.sheets.iter().any(|&sheet| self.sheets[sheet].opened));
        held_back
    }
}

/// Container yielding the tracks of a CUE/BIN CD image.
pub struct CueBinContainer {
    prefix: String,
    // Track files, each held once; empty when none of its tracks is read.
    files: Vec<Vec<u8>>,
    tracks: Vec<Track>,
    skipped: Skipped,
    // Tracks the last visit could not allocate a buffer for.
    unallocated: AtomicUsize,
}

impl CueBinContainer {
    /// Open a cue sheet whose track files cannot be looked up.
    ///
    /// # Errors
    ///
    /// Fails if `data` is not a cue sheet or `depth` is zero.
    pub fn from_bytes(data: &[u8], prefix: String, depth: u32) -> Result<Self> {
        Self::from_bytes_with_siblings(data, prefix, depth, &|_| None)
    }

    /// Like [`from_bytes`](Self::from_bytes); `get_sibling` is asked for each
    /// track file by a name relative to the directory of the cue sheet. The
    /// names are sanitized: no absolute paths and no `..` components.
    ///
    /// # Errors
    ///
    /// Fails if `data` is not a cue sheet or `depth` is zero.
    pub fn from_bytes_with_siblings(
        data: &[u8],
        prefix: String,
        depth: u32,
        get_sibling: &dyn Fn(&str) -> Option<Vec<u8>>,
    ) -> Result<Self> {
        if depth == 0 {
            return Err(Error::MaxDepthExceeded);
        }
        if !is_cue_sheet(data) {
            return Err(Error::invalid_format("Not a cue sheet"));
        }
        let text = decode(data).ok_or_else(|| Error::invalid_format("Not a cue sheet"))?;
        let sheet = parse(&text)?;
        let sheet_stem = single_file_stem(&sheet, &prefix);

        // Track files in the order of `resolved.groups`.
        let mut files: Vec<Vec<u8>> = Vec::new();
        let resolved = resolve(&sheet, sheet_stem, &mut |candidate| {
            get_sibling(candidate)
                .map(|bytes| files.push(bytes))
                .is_some()
        });

        // Per sheet track: its file and extent, or why it cannot be read.
        let mut outcomes: Vec<Option<core::result::Result<(usize, Extent), String>>> =
            sheet.tracks.iter().map(|_| None).collect();
        // A problem is never replaced, and replaces an extent.
        let mut settle = |track: usize, outcome: core::result::Result<(usize, Extent), String>| {
            let slot = &mut outcomes[track];
            if slot.is_none() || (outcome.is_err() && matches!(slot, Some(Ok(_)))) {
                *slot = Some(outcome);
            }
        };
        for (track, problem) in resolved.problems {
            settle(track, Err(problem));
        }
        for (index, (group, file)) in resolved.groups.iter().zip(&files).enumerate() {
            for (track, extent) in group_tracks(group, &sheet.tracks, file.len()) {
                settle(track, extent.map(|extent| (index, extent)));
            }
        }

        let input_len = files
            .iter()
            .fold(data.len(), |total, file| total.saturating_add(file.len()));
        let mut budget = OutputBudget::for_input(input_len, OUTPUT_BUDGET_RATIO);
        let mut skipped = Skipped::default();
        let mut tracks = Vec::new();
        for (declared, outcome) in sheet.tracks.iter().zip(outcomes) {
            let number = declared.number;
            let (file, extent) = match outcome {
                Some(Ok(found)) => found,
                Some(Err(problem)) => {
                    skipped.note(format!("track {number} {problem}"));
                    continue;
                }
                None => {
                    skipped.note(format!("track {number} has no INDEX 01"));
                    continue;
                }
            };
            let size = extent
                .sectors
                .saturating_mul(declared.mode.output_per_sector());
            if budget.charge(size, "track").is_err() {
                skipped.note(format!("track {number} exceeds the size limit"));
                continue;
            }
            tracks.push(Track {
                number,
                file,
                extent,
                mode: declared.mode,
                swap: declared.swap,
            });
        }
        for (index, file) in files.iter_mut().enumerate() {
            if !tracks.iter().any(|track| track.file == index) {
                *file = Vec::new();
            }
        }

        Ok(Self {
            prefix,
            files,
            tracks,
            skipped,
            unallocated: AtomicUsize::new(0),
        })
    }

    // The entry data of a track; `None` when a buffer cannot be allocated.
    fn content(&self, track: &Track) -> Option<Cow<'_, [u8]>> {
        let stored = track.mode.sector_size()?;
        let file = self.files.get(track.file)?;
        let end = track
            .extent
            .sectors
            .checked_mul(stored)?
            .checked_add(track.extent.start)?;
        let source = file.get(track.extent.start..end)?;
        let borrowed = match track.mode {
            Mode::Cooked => true,
            Mode::Audio => !track.swap,
            _ => false,
        };
        if borrowed {
            return Some(Cow::Borrowed(source));
        }

        let per_sector = track.mode.output_per_sector();
        let mut out = Vec::new();
        out.try_reserve_exact(track.extent.sectors.checked_mul(per_sector)?)
            .ok()?;
        for sector in source.chunks_exact(stored) {
            let offset = match track.mode {
                // The sector's own mode byte decides when it has one.
                Mode::Raw { fallback } => match (sector[..12] == SYNC, sector[15]) {
                    (true, 1) => 16,
                    (true, 2) => 24,
                    _ => fallback,
                },
                Mode::HeaderlessMode2 => 8,
                _ => 0,
            };
            out.extend_from_slice(&sector[offset..offset + per_sector]);
        }
        if track.swap && track.mode.is_audio() {
            for pair in out.chunks_exact_mut(2) {
                pair.swap(0, 1);
            }
        }
        Some(Cow::Owned(out))
    }
}

impl Container for CueBinContainer {
    fn prefix(&self) -> &str {
        &self.prefix
    }

    fn visit(&self, visitor: &mut dyn FnMut(&Entry<'_>) -> Result<bool>) -> Result<()> {
        // One track is cooked at a time and released before the next.
        let mut unallocated = 0;
        let mut result = Ok(());
        for track in &self.tracks {
            let Some(data) = self.content(track) else {
                unallocated += 1;
                continue;
            };
            let extension = if track.mode.is_audio() { "pcm" } else { "img" };
            let path = format!("{}/track{:02}.{extension}", self.prefix, track.number);
            match visitor(&Entry::new(&path, &self.prefix, &data)) {
                Ok(true) => {}
                Ok(false) => break,
                Err(err) => {
                    result = Err(err);
                    break;
                }
            }
        }
        self.unallocated.store(unallocated, Ordering::Relaxed);
        result
    }

    fn skipped_entries(&self) -> Option<String> {
        let mut skipped = self.skipped.clone();
        let unallocated = self.unallocated.load(Ordering::Relaxed);
        if unallocated > 0 {
            skipped.count += unallocated - 1;
            skipped.note(format!(
                "{unallocated} tracks are too large to be held in memory"
            ));
        }
        skipped.into_message()
    }

    fn info(&self) -> ContainerInfo {
        ContainerInfo {
            path: self.prefix.clone(),
            format: ContainerFormat::CueBin,
            entry_count: Some(self.tracks.len()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    // FIPS 180-4, for the digests the specification gives.
    fn sha256(data: &[u8]) -> String {
        let mut k = [0u32; 64];
        let mut h = [0u32; 8];
        let mut primes =
            (2u32..).filter(|n| (2..*n).take_while(|d| d * d <= *n).all(|d| n % d != 0));
        for (i, prime) in primes.by_ref().take(64).enumerate() {
            let fraction = |root: f64| ((root - root.floor()) * 4_294_967_296.0) as u32;
            k[i] = fraction(f64::from(prime).cbrt());
            if i < 8 {
                h[i] = fraction(f64::from(prime).sqrt());
            }
        }
        let mut message = data.to_vec();
        message.push(0x80);
        while message.len() % 64 != 56 {
            message.push(0);
        }
        message.extend_from_slice(&(data.len() as u64 * 8).to_be_bytes());
        for block in message.chunks_exact(64) {
            let mut w = [0u32; 64];
            for (i, word) in block.chunks_exact(4).enumerate() {
                w[i] = u32::from_be_bytes([word[0], word[1], word[2], word[3]]);
            }
            for i in 16..64 {
                let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
                let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
                w[i] = w[i - 16]
                    .wrapping_add(s0)
                    .wrapping_add(w[i - 7])
                    .wrapping_add(s1);
            }
            let mut v = h;
            for i in 0..64 {
                let s1 = v[4].rotate_right(6) ^ v[4].rotate_right(11) ^ v[4].rotate_right(25);
                let ch = (v[4] & v[5]) ^ (!v[4] & v[6]);
                let t1 = v[7]
                    .wrapping_add(s1)
                    .wrapping_add(ch)
                    .wrapping_add(k[i])
                    .wrapping_add(w[i]);
                let s0 = v[0].rotate_right(2) ^ v[0].rotate_right(13) ^ v[0].rotate_right(22);
                let maj = (v[0] & v[1]) ^ (v[0] & v[2]) ^ (v[1] & v[2]);
                v = [
                    t1.wrapping_add(s0.wrapping_add(maj)),
                    v[0],
                    v[1],
                    v[2],
                    v[3].wrapping_add(t1),
                    v[4],
                    v[5],
                    v[6],
                ];
            }
            for (state, value) in h.iter_mut().zip(v) {
                *state = state.wrapping_add(value);
            }
        }
        h.iter().map(|word| format!("{word:08x}")).collect()
    }

    fn repeated(text: &str, len: usize) -> Vec<u8> {
        text.bytes().cycle().take(len).collect()
    }

    fn bcd(value: usize) -> u8 {
        (value / 10 * 16 + value % 10) as u8
    }

    fn data_sector(frame: usize, mode: u8, body: &[u8]) -> Vec<u8> {
        let address = frame + 150;
        let mut sector = SYNC.to_vec();
        sector.extend([
            bcd(address / 4500),
            bcd(address / 75 % 60),
            bcd(address % 75),
            mode,
        ]);
        sector.extend_from_slice(body);
        sector.resize(RAW_SECTOR, 0);
        sector
    }

    // The 77-sector image of section 11.3 of the specification.
    fn disc() -> Vec<u8> {
        let mut image = Vec::new();
        for i in 0..20 {
            image.extend(data_sector(i, 1, &repeated(&format!("T1-{i:06}|"), 2048)));
        }
        image.resize(30 * RAW_SECTOR, 0);
        for i in 0..30usize {
            for k in 0..588usize {
                let v = ((i * 588 + k) % 30000) as i16;
                image.extend(v.to_le_bytes());
                image.extend((-v).to_le_bytes());
            }
        }
        for frame in 60..65 {
            image.extend(data_sector(frame, 2, &[0, 0, 8, 0, 0, 0, 8, 0]));
        }
        for i in 0..12 {
            let mut body = if i == 7 {
                vec![0, 0, 0x20, 0, 0, 0, 0x20, 0]
            } else {
                vec![0, 0, 8, 0, 0, 0, 8, 0]
            };
            if i == 7 {
                body.extend(repeated("F2-000007|", 2324));
            } else {
                body.extend(repeated(&format!("T3-{i:06}|"), 2048));
            }
            image.extend(data_sector(65 + i, 2, &body));
        }
        image
    }

    const SHEET: &str = "FILE \"disc.bin\" BINARY\n  TRACK 01 MODE1/2352\n    INDEX 01 00:00:00\n  TRACK 02 AUDIO\n    INDEX 00 00:00:20\n    INDEX 01 00:00:30\n  TRACK 03 MODE2/2352\n    INDEX 00 00:00:60\n    INDEX 01 00:00:65\n";

    const TRACK1: &str = "29a24186333aed9ef03ffbfd4ab452d397d634c03d967a27b30895f8575aaf3f";
    const TRACK2: &str = "e7461c0534743f4e9f7833e84c20327dd9959436e44bcf9acf10fa5d0dac83d5";
    const TRACK3: &str = "25a6f1093fdf46c5acac4800b2b485dfe079845a8afdf784a37150805bdd7f98";

    struct Listing {
        // (name, size, SHA-256)
        entries: Vec<(String, usize, String)>,
        skipped: String,
        asked: Vec<String>,
    }

    fn open_as(sheet_name: &str, sheet: &[u8], siblings: &[(&str, &[u8])]) -> Listing {
        let asked = RefCell::new(Vec::new());
        let container = CueBinContainer::from_bytes_with_siblings(
            sheet,
            format!("dir/{sheet_name}"),
            8,
            &|name| {
                asked.borrow_mut().push(name.to_string());
                siblings
                    .iter()
                    .find(|(sibling, _)| *sibling == name)
                    .map(|(_, data)| data.to_vec())
            },
        )
        .unwrap();
        let mut entries = Vec::new();
        container
            .visit(&mut |entry| {
                assert_eq!(entry.container_path, format!("dir/{sheet_name}"));
                let name = entry.path.strip_prefix(entry.container_path).unwrap();
                entries.push((
                    name.trim_start_matches('/').to_string(),
                    entry.data.len(),
                    sha256(entry.data),
                ));
                Ok(true)
            })
            .unwrap();
        assert_eq!(container.info().entry_count, Some(entries.len()));
        Listing {
            entries,
            skipped: container.skipped_entries().unwrap_or_default(),
            asked: asked.into_inner(),
        }
    }

    fn open(sheet: &str, siblings: &[(&str, &[u8])]) -> Listing {
        open_as("sheet.cue", sheet.as_bytes(), siblings)
    }

    fn names(listing: &Listing) -> Vec<&str> {
        listing.entries.iter().map(|e| e.0.as_str()).collect()
    }

    #[test]
    fn sha256_helper_matches_known_digest() {
        assert_eq!(
            sha256(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn times_convert_to_frames() {
        for (time, frames) in [
            ("00:00:00", 0),
            ("00:02:00", 150),
            ("00:38:08", 2858),
            ("04:00:00", 18_000),
            ("04:02:00", 18_150),
            ("10:48:58", 48_658),
            ("38:52:34", 174_934),
            ("46:07:03", 207_528),
            ("10:57:27", 49_302),
            ("0:2:0", 150),
            ("100:00:00", 450_000),
            // Out-of-range fields are still evaluated.
            ("00:75:80", 75 * 75 + 80),
            ("9999:9999:9999", 9999 * 4500 + 9999 * 75 + 9999),
        ] {
            assert_eq!(time_frames(time), Some(frames), "{time}");
        }
        assert_eq!(18_150 * RAW_SECTOR, 42_688_800);
        assert_eq!(49_302 * USER_DATA, 100_970_496);
        for time in [
            "",
            "00:00",
            "00:00:00:00",
            "00::00",
            "0a:00:00",
            "00:00:-1",
            "10000:00:00",
            "00:00:00000",
            "00 00 00",
            "+1:00:00",
        ] {
            assert_eq!(time_frames(time), None, "{time}");
        }
    }

    #[test]
    fn file_arguments_follow_the_quoting_rules() {
        type Arguments = Option<(&'static str, Option<&'static str>)>;
        let cases: [(&str, Arguments); 12] = [
            ("\"a b.bin\" BINARY", Some(("a b.bin", Some("BINARY")))),
            ("\"a.bin\"", Some(("a.bin", None))),
            ("\"a.bin\"   WAVE extra", Some(("a.bin", Some("WAVE")))),
            // The name runs to the last quote; a backslash is no escape.
            (
                "\"say \"hi\".bin\" BINARY",
                Some(("say \"hi\".bin", Some("BINARY"))),
            ),
            ("\"C:\\DIR\\\" BINARY", Some(("C:\\DIR\\", Some("BINARY")))),
            ("a.bin BINARY", Some(("a.bin", Some("BINARY")))),
            ("my disc.bin\tBINARY", Some(("my disc.bin", Some("BINARY")))),
            ("\"stray.bin BINARY", Some(("stray.bin", Some("BINARY")))),
            ("a.bin", Some(("a.bin", None))),
            ("\"\" BINARY", None),
            ("", None),
            ("\" BINARY", None),
        ];
        for (rest, expected) in cases {
            assert_eq!(file_arguments(rest), expected, "{rest}");
        }
    }

    #[test]
    fn sheets_are_decoded_by_byte_order_mark() {
        let text = "FILE \"é.bin\" BINARY\r\nTRACK 01 AUDIO\r\n";
        let utf16 = |unit: fn(u16) -> [u8; 2], mark: [u8; 2]| {
            let mut bytes = mark.to_vec();
            bytes.extend(text.encode_utf16().flat_map(unit));
            bytes
        };
        let mut with_mark = vec![0xEF, 0xBB, 0xBF];
        with_mark.extend(text.bytes());
        assert_eq!(decode(&with_mark).as_deref(), Some(text));
        assert_eq!(decode(text.as_bytes()).as_deref(), Some(text));
        assert_eq!(
            decode(&utf16(u16::to_le_bytes, [0xFF, 0xFE])).as_deref(),
            Some(text)
        );
        assert_eq!(
            decode(&utf16(u16::to_be_bytes, [0xFE, 0xFF])).as_deref(),
            Some(text)
        );
        // Not UTF-8: one character per byte.
        assert_eq!(decode(b"FILE \xE9.bin").as_deref(), Some("FILE é.bin"));
        // A declared encoding that does not decode.
        assert_eq!(decode(b"\xEF\xBB\xBFFILE \xE9"), None);
        assert_eq!(decode(b"\xFF\xFEF\0I"), None);
        assert_eq!(decode(b"\xFF\xFE\x00\xD8"), None);
        // End-of-file mark and NUL.
        assert_eq!(decode(b"REM a\x1Agarbage\0").as_deref(), Some("REM a"));
        assert_eq!(decode(b"REM a\0"), None);
        assert_eq!(decode(b""), None);
        assert_eq!(decode(&vec![b' '; MAX_SHEET_SIZE + 1]), None);
        assert!(decode(&vec![b' '; MAX_SHEET_SIZE]).is_some());
    }

    #[test]
    fn recognises_cue_sheets_only() {
        assert!(is_cue_sheet(SHEET.as_bytes()));
        assert!(is_cue_sheet(b"REM x\r\nfile a.bin\r\n\ttrack 1 audio"));
        assert!(is_cue_sheet(
            b"\xEF\xBB\xBFFILE \"a.wav\" WAVE\rTRACK 01 AUDIO\r"
        ));
        assert!(is_cue_sheet(b"TITLE \"x\"\nFILE a BINARY\nTRACK 1 FOO\n"));
        for not in [
            &b""[..],
            b"\n\n",
            b"Hello, this is a text FILE with a TRACK record.\n",
            b"README\nFILE a.bin BINARY\nTRACK 01 AUDIO\n",
            b"TRACK 01 AUDIO\nFILE \"a.bin\" BINARY\n",
            b"FILE \"a.bin\" BINARY\n",
            b"FILE \"a.bin\" BINARY\nTRACK one AUDIO\n",
            b"FILE \"a.bin\" BINARY\nTRACK 01\n",
            b"FILE \"\" BINARY\nTRACK 01 AUDIO\n",
            b"REM only\nREM FILE a.bin BINARY\nREM TRACK 01 AUDIO\n",
            b"FILE \"a.bin\" BINARY\nTRACK 01 AUDIO\n\0",
            b"\x7FELF\x02\x01\x01\0FILE a BINARY\nTRACK 1 AUDIO\n",
        ] {
            assert!(!is_cue_sheet(not), "{}", String::from_utf8_lossy(not));
        }
        let mut large = SHEET.as_bytes().to_vec();
        large.resize(MAX_SHEET_SIZE + 1, b'\n');
        assert!(!is_cue_sheet(&large));
    }

    #[test]
    fn modes_and_their_sector_sizes() {
        for (word, size, readable) in [
            ("AUDIO", Some(2352), true),
            ("cdg", Some(2448), true),
            ("MODE1/2048", Some(2048), true),
            ("MODE1/2352", Some(2352), true),
            ("MODE2/2336", Some(2336), true),
            ("Mode2/2352", Some(2352), true),
            ("CDI/2336", Some(2336), true),
            ("CDI/2352", Some(2352), true),
            ("MODE2/2048", Some(2048), true),
            ("MODE2/2324", Some(2324), false),
            ("MODE3/2400", Some(2400), false),
            ("MODE1/2047", None, false),
            ("MODE1/2449", None, false),
            ("/2352", None, false),
            ("WHATEVER", None, false),
        ] {
            let mode = Mode::parse(word);
            assert_eq!(mode.sector_size(), size, "{word}");
            assert_eq!(
                !matches!(mode, Mode::Unsupported { .. }),
                readable,
                "{word}"
            );
        }
    }

    #[test]
    fn context_rules_decide_validity() {
        let parse = |text: &str| parse(text).map_err(|e| e.to_string());
        for bad in [
            "TRACK 01 AUDIO\nFILE a BINARY\n",
            "FILE \"\" BINARY\nTRACK 01 AUDIO\n",
            "FILE a BINARY\nTRACK xx AUDIO\n",
            "FILE a BINARY\nTRACK 01\n",
            "FILE a BINARY\nTRACK 00 AUDIO\n",
            "FILE a BINARY\nTRACK 100 AUDIO\n",
            "FILE a BINARY\nTRACK 99999999999999999999999 AUDIO\n",
            "FILE a BINARY\nTRACK 02 AUDIO\nTRACK 02 AUDIO\n",
            "FILE a BINARY\nTRACK 03 AUDIO\nTRACK 02 AUDIO\n",
        ] {
            assert!(parse(bad).is_err(), "{bad}");
        }
        let many = |count: usize| {
            (0..count)
                .map(|i| format!("FILE f{i}.bin BINARY\n"))
                .collect::<String>()
        };
        assert!(parse(&many(99)).is_ok());
        assert!(parse(&many(100)).is_err());

        // A first track above 1, a gap in the numbers, ignored and unknown
        // commands, a malformed INDEX and repeated indexes.
        let sheet = parse(
            "CATALOG 1234567890123\nINDEX 01 00:00:00\nFILE a BINARY\nREM TRACK 09 AUDIO\n\
             FLAGS DCP\nTRACK 05 AUDIO\nBOGUS 1 2 3\nINDEX 01 00:00:xx\nINDEX 100 00:00:00\n\
             INDEX 01 00:01:00\nINDEX 01 00:02:00\nINDEX 02 00:03:00\nPREGAP 00:02:00\n\
             TRACK 07 MODE1/2352 extra words\nINDEX 00 00:04:00 extra\nPOSTGAP 00:02:00\n",
        )
        .unwrap();
        assert_eq!(sheet.tracks.len(), 2);
        assert_eq!((sheet.tracks[0].number, sheet.tracks[1].number), (5, 7));
        let marks: Vec<_> = sheet.files[0]
            .marks
            .iter()
            .map(|m| (m.track, m.data_start, m.frame))
            .collect();
        assert_eq!(marks, [(0, true, 75), (1, false, 300)]);
    }

    #[test]
    fn candidate_names_never_leave_the_directory() {
        let stem = Some("sheet");
        let plain = ["disc.bin", "DISC.BIN", "sheet.bin", "sheet.BIN"];
        assert_eq!(candidates("disc.bin", stem), plain);
        for written in [
            "C:\\IMAGES\\disc.bin",
            "/tmp/x/disc.bin",
            "..\\..\\disc.bin",
            "../disc.bin",
            "\\\\server\\share\\disc.bin",
            "sub//disc.bin",
            "./disc.bin",
            "a:b/disc.bin",
        ] {
            assert_eq!(candidates(written, stem), plain, "{written}");
        }
        assert_eq!(
            candidates("sub\\Disc.Img", stem),
            [
                "sub/Disc.Img",
                "Disc.Img",
                "disc.img",
                "DISC.IMG",
                "sheet.Img",
                "sheet.img",
                "sheet.IMG",
                "sheet.bin",
                "sheet.BIN"
            ]
        );
        assert_eq!(
            candidates("sub/disc.bin", None),
            ["sub/disc.bin", "disc.bin", "DISC.BIN"]
        );
        // No usable base name: only the sheet's own name is left.
        for written in ["..", "dir/", "C:", "/", "a/..", "x:y"] {
            assert_eq!(
                candidates(written, stem),
                ["sheet.bin", "sheet.BIN"],
                "{written}"
            );
            assert!(candidates(written, None).is_empty(), "{written}");
        }
        assert_eq!(
            candidates(".hidden", stem),
            [".hidden", ".HIDDEN", "sheet.bin", "sheet.BIN"]
        );
        for written in [
            "../../../etc/passwd",
            "C:\\a\\..\\b",
            "a/./b",
            "..",
            "/abs",
            "x\0y",
        ] {
            for name in candidates(written, Some(".."))
                .iter()
                .chain(&candidates(written, stem))
            {
                assert!(name.split('/').all(usable_component), "{written} -> {name}");
            }
        }
    }

    // Section 11.1: a real three-track BeOS disc, by its sheet and length.
    #[test]
    fn real_disc_layout_from_sheet_and_length() {
        let sheet = parse(
            "FILE \"BeOS_4_5.bin\" BINARY\r\n  TRACK 01 MODE1/2352\r\n    INDEX 01 00:00:00\r\n\
             \x20 TRACK 02 MODE1/2352\r\n    INDEX 00 04:00:00\r\n    INDEX 01 04:02:00\r\n\
             \x20 TRACK 03 MODE1/2352\r\n    INDEX 00 38:50:34\r\n    INDEX 01 38:52:34\r\n",
        )
        .unwrap();
        let spans = spans(&sheet.files[0].marks).unwrap();
        let extents: Vec<_> = layout(&spans, &sheet.tracks, 773_198_832)
            .into_iter()
            .map(|(track, extent)| {
                let extent = extent.unwrap();
                (track, extent.start, extent.sectors)
            })
            .collect();
        assert_eq!(
            extents,
            [
                (0, 0, 18_000),
                (1, 42_688_800, 156_634),
                (2, 411_444_768, 153_807)
            ]
        );
        assert_eq!(156_634 * USER_DATA, 320_786_432);
    }

    #[test]
    fn crafted_image_matches_the_specification() {
        let image = disc();
        assert_eq!(image.len(), 181_104);
        assert_eq!(
            sha256(&image),
            "81d8fd63a9088a94d0c4260230696536eb2d667b8cb0ef2a889eeab990e4527c"
        );
        let listing = open(SHEET, &[("disc.bin", &image)]);
        assert_eq!(
            listing.entries,
            [
                ("track01.img".to_string(), 40_960, TRACK1.to_string()),
                ("track02.pcm".to_string(), 70_560, TRACK2.to_string()),
                ("track03.img".to_string(), 24_576, TRACK3.to_string()),
            ]
        );
        assert_eq!(listing.skipped, "");
        assert_eq!(listing.asked, ["disc.bin"]);
    }

    #[test]
    fn sheet_variants_over_the_crafted_image() {
        let image = disc();
        let siblings: &[(&str, &[u8])] = &[("disc.bin", &image)];
        let digests = |listing: &Listing| -> Vec<String> {
            listing.entries.iter().map(|e| e.2.clone()).collect()
        };
        const TRACK1_LONG: &str =
            "fa739a414a99027a4692097cbaf402ed71a794fffff0b3864a627bfa589f7aa1";

        let no_gaps: String = SHEET
            .lines()
            .filter(|l| !l.contains("INDEX 00"))
            .map(|l| format!("{l}\n"))
            .collect();
        let listing = open(&no_gaps, siblings);
        assert_eq!(
            listing.entries,
            [
                ("track01.img".to_string(), 61_440, TRACK1_LONG.to_string()),
                (
                    "track02.pcm".to_string(),
                    82_320,
                    "def3ef792f95f9eeae0218b7784f9061dc1cf6dae42320e3bca5fdfd721f4522".to_string()
                ),
                ("track03.img".to_string(), 24_576, TRACK3.to_string()),
            ]
        );

        let listing = open(&SHEET.replace("BINARY", "MOTOROLA"), siblings);
        assert_eq!(
            digests(&listing),
            [
                TRACK1,
                "eb1fb2325d4cacdbfe319b5d129ad059e2761bb0d55da619362175ceedcc28bc",
                TRACK3
            ]
        );

        let mut odd = vec![0xEF, 0xBB, 0xBF];
        odd.extend(
            SHEET
                .to_lowercase()
                .replace("disc.bin", "DISC.BIN")
                .replace('\n', "\r\n")
                .bytes(),
        );
        let listing = open_as("sheet.cue", &odd, siblings);
        assert_eq!(digests(&listing), [TRACK1, TRACK2, TRACK3]);
        assert_eq!(listing.asked, ["DISC.BIN", "disc.bin"]);

        let listing = open(
            &SHEET.replace("INDEX 00 00:00:20", "PREGAP 00:02:00"),
            siblings,
        );
        assert_eq!(digests(&listing), [TRACK1_LONG, TRACK2, TRACK3]);

        let listing = open(&SHEET.replace("MODE1/2352", "MODE2/2352"), siblings);
        assert_eq!(digests(&listing), [TRACK1, TRACK2, TRACK3]);

        let listing = open(
            &SHEET.replace("INDEX 01 00:00:65", "INDEX 01 00:02:00"),
            siblings,
        );
        assert_eq!(digests(&listing), [TRACK1, TRACK2]);
        assert_eq!(
            listing.skipped,
            "CUE: track 3 starts beyond the end of its file"
        );

        let listing = open(
            &SHEET.replace("INDEX 01 00:00:30", "INDEX 01 00:00:10"),
            siblings,
        );
        assert!(listing.entries.is_empty());
        assert_eq!(
            listing.skipped,
            "CUE: track 1 is in file 'disc.bin' whose INDEX times go backwards (and 2 more problems)"
        );
        // The file of an inconsistent layout is not even fetched.
        assert!(listing.asked.is_empty());

        for name in [
            "C:\\IMAGES\\disc.bin",
            "/tmp/x/disc.bin",
            "..\\..\\disc.bin",
        ] {
            let listing = open(&SHEET.replace("disc.bin", name), &[]);
            assert_eq!(
                listing.asked,
                ["disc.bin", "DISC.BIN", "sheet.bin", "sheet.BIN"],
                "{name}"
            );
            let listing = open(&SHEET.replace("disc.bin", name), siblings);
            assert_eq!(digests(&listing), [TRACK1, TRACK2, TRACK3]);
        }
        let listing = open(&SHEET.replace("disc.bin", "sub\\disc.bin"), &[]);
        assert_eq!(listing.asked[..2], ["sub/disc.bin", "disc.bin"]);
        assert_eq!(
            listing.skipped,
            "CUE: track 1 file 'sub\\disc.bin' was not found (and 2 more problems)"
        );

        // A stale name: the sheet's own name finds the file.
        let listing = open_as(
            "Disc (1998).CUE",
            SHEET.replace("disc.bin", "OLD.BIN").as_bytes(),
            &[("Disc (1998).BIN", &image)],
        );
        assert_eq!(digests(&listing), [TRACK1, TRACK2, TRACK3]);
    }

    #[test]
    fn other_stored_sector_sizes() {
        let image = disc();
        let headerless: Vec<u8> = image[60 * RAW_SECTOR..]
            .chunks_exact(RAW_SECTOR)
            .flat_map(|sector| sector[16..].to_vec())
            .collect();
        assert_eq!(headerless.len(), 39_712);
        assert_eq!(
            sha256(&headerless),
            "37c9cbd9fe2ff0a31bffc8fb0cc1f98a2375f21315d181b47ef8964118593160"
        );
        let listing = open(
            "FILE m2.bin BINARY\nTRACK 04 MODE2/2336\nINDEX 00 00:00:00\nINDEX 01 00:00:05\n",
            &[("m2.bin", &headerless)],
        );
        assert_eq!(
            listing.entries,
            [("track04.img".to_string(), 24_576, TRACK3.to_string())]
        );

        // Section 11.2: cooked data followed by audio in one file.
        let mut mixed = Vec::new();
        for sector in image[..20 * RAW_SECTOR].chunks_exact(RAW_SECTOR) {
            mixed.extend_from_slice(&sector[16..16 + USER_DATA]);
        }
        mixed.extend_from_slice(&image[20 * RAW_SECTOR..60 * RAW_SECTOR]);
        assert_eq!(mixed.len(), 135_040);
        assert_eq!(
            sha256(&mixed),
            "7b03ee51a590fa757899983234885905205ea7b2ab177b675e529c8faf9c90ce"
        );
        let sheet = "FILE \"MIXED.BIN\" BINARY\n  TRACK 01 MODE1/2048\n    INDEX 01 00:00:00\n  TRACK 02 AUDIO\n    INDEX 00 00:00:20\n    INDEX 01 00:00:30\n";
        let listing = open(sheet, &[("MIXED.BIN", &mixed)]);
        assert_eq!(
            listing.entries,
            [
                ("track01.img".to_string(), 40_960, TRACK1.to_string()),
                ("track02.pcm".to_string(), 70_560, TRACK2.to_string()),
            ]
        );

        // CDG: the 96 sub-channel bytes of every sector are dropped.
        let audio = &image[30 * RAW_SECTOR..60 * RAW_SECTOR];
        let cdg: Vec<u8> = audio
            .chunks_exact(RAW_SECTOR)
            .flat_map(|sector| sector.iter().copied().chain([0xAA; 96]))
            .collect();
        let listing = open(
            "FILE c.bin BINARY\nTRACK 01 CDG\nINDEX 01 00:00:00\n",
            &[("c.bin", &cdg)],
        );
        assert_eq!(
            listing.entries,
            [("track01.pcm".to_string(), 70_560, TRACK2.to_string())]
        );

        // A trailing partial sector is ignored.
        let listing = open(
            "FILE c.bin BINARY\nTRACK 01 AUDIO\nINDEX 01 00:00:00\n",
            &[("c.bin", &audio[..audio.len() - 1])],
        );
        assert_eq!(listing.entries[0].1, 29 * RAW_SECTOR);
    }

    #[test]
    fn unknown_modes_and_missing_indexes_are_reported() {
        let image = disc();
        let siblings: &[(&str, &[u8])] = &[("disc.bin", &image)];
        // A mode of known sector size keeps the next track locatable.
        let listing = open(&SHEET.replace("AUDIO", "FOO/2352"), siblings);
        assert_eq!(names(&listing), ["track01.img", "track03.img"]);
        assert_eq!(
            listing.skipped,
            "CUE: track 2 has unsupported mode FOO/2352"
        );
        // One of unknown size does not.
        let listing = open(&SHEET.replace("AUDIO", "FOO"), siblings);
        assert_eq!(names(&listing), ["track01.img"]);
        assert_eq!(
            listing.skipped,
            "CUE: track 2 has unsupported mode FOO (and 1 more problems)"
        );
        // MODE2/2324 is not cooked, but its sector size is known.
        let listing = open(&SHEET.replace("AUDIO", "MODE2/2324"), siblings);
        assert_eq!(names(&listing), ["track01.img", "track03.img"]);
        assert_eq!(
            listing.skipped,
            "CUE: track 2 has unsupported mode MODE2/2324"
        );

        // Without INDEX 01 the track is reported, and its INDEX 00 still
        // ends the track before it.
        let listing = open(&SHEET.replace("    INDEX 01 00:00:30\n", ""), siblings);
        assert_eq!(
            listing.entries[0],
            ("track01.img".to_string(), 40_960, TRACK1.to_string())
        );
        assert_eq!(names(&listing), ["track01.img", "track03.img"]);
        assert_eq!(listing.skipped, "CUE: track 2 has no INDEX 01");

        // INDEX 00 written after INDEX 01.
        let swapped = SHEET.replace(
            "    INDEX 00 00:00:20\n    INDEX 01 00:00:30\n",
            "    INDEX 01 00:00:20\n    INDEX 00 00:00:30\n",
        );
        assert!(open(&swapped, siblings).entries.is_empty());

        // An empty track and an empty file.
        let listing = open(
            &SHEET.replace("INDEX 00 00:00:60", "INDEX 00 00:00:30"),
            siblings,
        );
        assert_eq!(names(&listing), ["track01.img", "track03.img"]);
        assert_eq!(listing.skipped, "CUE: track 2 is empty");
        let listing = open(SHEET, &[("disc.bin", &[])]);
        assert!(listing.entries.is_empty());
        assert!(
            listing.skipped.contains("track 1 starts beyond the end"),
            "{}",
            listing.skipped
        );
    }

    #[test]
    fn several_files_are_read_independently() {
        let image = disc();
        let data = &image[..20 * RAW_SECTOR];
        let audio = &image[20 * RAW_SECTOR..60 * RAW_SECTOR];
        // One file per track, the gap at the head of the track's own file.
        let sheet = "FILE \"d (Track 1).bin\" BINARY\nTRACK 01 MODE1/2352\nINDEX 01 00:00:00\n\
                     FILE \"d (Track 2).bin\" BINARY\nTRACK 02 AUDIO\nINDEX 00 00:00:00\nINDEX 01 00:00:10\n\
                     FILE \"d (Track 3).wav\" WAVE\nTRACK 03 AUDIO\nINDEX 01 00:00:00\n\
                     FILE \"gone.bin\" BINARY\nTRACK 04 AUDIO\nINDEX 01 00:00:00\n\
                     FILE \"empty.bin\" BINARY\n";
        let listing = open(
            sheet,
            &[("d (Track 1).bin", data), ("d (Track 2).bin", audio)],
        );
        assert_eq!(
            listing.entries,
            [
                ("track01.img".to_string(), 40_960, TRACK1.to_string()),
                ("track02.pcm".to_string(), 70_560, TRACK2.to_string()),
            ]
        );
        assert_eq!(
            listing.skipped,
            "CUE: track 3 is in file 'd (Track 3).wav' of unsupported type WAVE (and 1 more problems)"
        );
        // Neither the WAVE file nor the file without tracks is asked for, and
        // the sheet's own name is no candidate with several files.
        assert_eq!(
            listing.asked,
            ["d (Track 1).bin", "d (Track 2).bin", "gone.bin", "GONE.BIN"]
        );

        // Layout D: INDEX 00 in the previous file, INDEX 01 after the FILE.
        let sheet = "FILE \"01.bin\" BINARY\nTRACK 01 AUDIO\nINDEX 01 00:00:00\nTRACK 02 AUDIO\nINDEX 00 00:00:25\n\
                     FILE \"02.bin\" BINARY\nINDEX 01 00:00:00\n";
        let listing = open(
            sheet,
            &[
                ("01.bin", &audio[..30 * RAW_SECTOR]),
                ("02.bin", &audio[10 * RAW_SECTOR..]),
            ],
        );
        assert_eq!(listing.entries[0].1, 25 * RAW_SECTOR);
        assert_eq!(
            listing.entries[1],
            ("track02.pcm".to_string(), 70_560, TRACK2.to_string())
        );
        assert_eq!(listing.skipped, "");

        // The same FILE line before every track: one file, fetched once.
        let repeated = SHEET.replace("  TRACK", "FILE \"disc.bin\" BINARY\n  TRACK");
        let listing = open(&repeated, &[("disc.bin", &image)]);
        assert_eq!(listing.asked, ["disc.bin"]);
        assert_eq!(listing.entries.len(), 3);
        assert_eq!(listing.entries[2].2, TRACK3);
    }

    #[test]
    fn without_a_lookup_nothing_is_listed() {
        let container = CueBinContainer::from_bytes(SHEET.as_bytes(), "x.cue".into(), 4).unwrap();
        assert_eq!(container.info().entry_count, Some(0));
        assert_eq!(
            container.skipped_entries().unwrap(),
            "CUE: track 1 file 'disc.bin' was not found (and 2 more problems)"
        );
        assert!(matches!(
            CueBinContainer::from_bytes(SHEET.as_bytes(), "x.cue".into(), 0),
            Err(Error::MaxDepthExceeded)
        ));
        assert!(CueBinContainer::from_bytes(b"not a sheet", "x.cue".into(), 4).is_err());
        assert!(
            CueBinContainer::from_bytes(
                b"FILE a BINARY\nTRACK 1 AUDIO\nTRACK 1 AUDIO\n",
                "x.cue".into(),
                4
            )
            .is_err()
        );
    }

    #[test]
    fn messages_do_not_echo_hostile_names() {
        let name = format!("a\u{1b}[31m{}", "x".repeat(5000));
        let sheet = format!("FILE \"{name}\" BINARY\nTRACK 01 AUDIO\nINDEX 01 00:00:00\n");
        let message = open(&sheet, &[]).skipped;
        assert!(message.len() < 200, "{message}");
        assert!(!message.contains('\u{1b}'));
    }

    #[test]
    fn visit_stops_early() {
        let image = disc();
        let container =
            CueBinContainer::from_bytes_with_siblings(SHEET.as_bytes(), "x.cue".into(), 4, &|_| {
                Some(image.clone())
            })
            .unwrap();
        let mut seen = 0;
        container
            .visit(&mut |_| {
                seen += 1;
                Ok(false)
            })
            .unwrap();
        assert_eq!(seen, 1);
        assert!(
            container
                .visit(&mut |_| Err(Error::invalid_format("stop")))
                .is_err()
        );
    }

    #[test]
    fn files_resolving_to_one_name_are_one_file() {
        let image = disc();
        let audio = &image[20 * RAW_SECTOR..30 * RAW_SECTOR];

        // 99 names that all fall back to the base name beside the sheet.
        let sheet: String = (1..=99)
            .map(|n| {
                format!("FILE \"d{n}/big.bin\" BINARY\nTRACK {n:02} AUDIO\nINDEX 01 00:00:00\n")
            })
            .collect();
        let listing = open(&sheet, &[("big.bin", audio)]);
        assert_eq!(
            listing
                .asked
                .iter()
                .filter(|name| *name == "big.bin")
                .count(),
            1
        );
        assert_eq!(listing.asked.len(), 100);
        // The marks are those of one file: every track but the last is empty.
        assert_eq!(names(&listing), ["track99.pcm"]);
        assert_eq!(listing.entries[0].1, audio.len());
        assert_eq!(
            listing.skipped,
            "CUE: track 1 is empty (and 97 more problems)"
        );

        // Two spellings of one file give the layout of a single-file sheet.
        let sheet = SHEET
            .replace("\"disc.bin\"", "\"x/disc.bin\"")
            .replace("  TRACK 02", "FILE \"y\\disc.bin\" BINARY\n  TRACK 02");
        let listing = open(&sheet, &[("disc.bin", &image)]);
        assert_eq!(listing.asked, ["x/disc.bin", "disc.bin", "y/disc.bin"]);
        assert_eq!(
            listing.entries,
            [
                ("track01.img".to_string(), 40_960, TRACK1.to_string()),
                ("track02.pcm".to_string(), 70_560, TRACK2.to_string()),
                ("track03.img".to_string(), 24_576, TRACK3.to_string()),
            ]
        );
        assert_eq!(listing.skipped, "");

        // Merged marks that go backwards disqualify the file.
        let sheet = "FILE \"x/a.bin\" BINARY\nTRACK 01 AUDIO\nINDEX 01 00:00:05\n\
                     FILE \"y/a.bin\" BINARY\nTRACK 02 AUDIO\nINDEX 01 00:00:00\n";
        let listing = open(sheet, &[("a.bin", audio)]);
        assert!(listing.entries.is_empty());
        assert_eq!(
            listing.skipped,
            "CUE: track 1 is in file 'x/a.bin' whose INDEX times go backwards (and 1 more problems)"
        );

        // Whether names differing in letter case are one file depends on the
        // lookup, so the second is not fetched.
        let sheet = "FILE \"a.bin\" BINARY\nTRACK 01 AUDIO\nINDEX 01 00:00:00\n\
                     FILE \"A.BIN\" BINARY\nTRACK 02 AUDIO\nINDEX 01 00:00:00\n";
        let listing = open(sheet, &[("a.bin", audio), ("A.BIN", audio)]);
        assert_eq!(listing.asked, ["a.bin"]);
        assert_eq!(names(&listing), ["track01.pcm"]);
        assert_eq!(
            listing.skipped,
            "CUE: track 2 is in file 'A.BIN' named like another file in different letter case"
        );

        // The byte order is that of the FILE command a track is under.
        let sheet = "FILE \"x/a.bin\" MOTOROLA\nTRACK 01 AUDIO\nINDEX 01 00:00:00\n\
                     FILE \"y/a.bin\" BINARY\nTRACK 02 AUDIO\nINDEX 01 00:00:05\n";
        let listing = open(sheet, &[("a.bin", audio)]);
        let swapped: Vec<u8> = audio[..5 * RAW_SECTOR]
            .chunks_exact(2)
            .flat_map(|pair| [pair[1], pair[0]])
            .collect();
        assert_eq!(listing.entries[0].2, sha256(&swapped));
        assert_eq!(listing.entries[1].2, sha256(&audio[5 * RAW_SECTOR..]));
    }

    #[cfg(target_pointer_width = "64")]
    #[test]
    fn oversized_tracks_are_not_read() {
        let sheet = parse("FILE a.bin BINARY\nTRACK 01 AUDIO\nINDEX 01 00:00:00\nTRACK 02 AUDIO\nINDEX 01 00:00:10\n").unwrap();
        let resolved = resolve(&sheet, None, &mut |_| true);
        let results = group_tracks(&resolved.groups[0], &sheet.tracks, 1 << 40);
        assert_eq!(
            results[0].1,
            Ok(Extent {
                start: 0,
                sectors: 10
            })
        );
        assert_eq!(results[1].1, Err("exceeds the size limit".to_string()));
    }

    // A container for `SheetIndex`: files by path, found without regard to
    // letter case as the loader's containers do, an exact name first.
    struct Files {
        files: Vec<(&'static str, Vec<u8>)>,
        reads: RefCell<Vec<String>>,
        probes: RefCell<usize>,
    }

    impl Files {
        fn new(files: Vec<(&'static str, Vec<u8>)>) -> Self {
            Self {
                files,
                reads: RefCell::new(Vec::new()),
                probes: RefCell::new(0),
            }
        }

        fn get(&self, path: &str) -> Option<&[u8]> {
            let exact = self.files.iter().find(|(name, _)| *name == path);
            exact
                .or_else(|| {
                    self.files
                        .iter()
                        .find(|(name, _)| name.eq_ignore_ascii_case(path))
                })
                .map(|(_, data)| data.as_slice())
        }

        // What the index says about the file at `path` when it is visited.
        fn visit(&self, index: &mut SheetIndex, path: &str) -> bool {
            let data = self.get(path).unwrap();
            index.hides(
                path,
                &Entry::new(&format!("c/{path}"), "c", data),
                &|path| {
                    *self.probes.borrow_mut() += 1;
                    self.get(path).is_some()
                },
                &|path| {
                    self.reads.borrow_mut().push(path.to_string());
                    self.get(path).map(Cow::Borrowed)
                },
            )
        }
    }

    fn unlisted(index: &mut SheetIndex) -> Vec<String> {
        index
            .take_unlisted()
            .into_iter()
            .map(|held| {
                assert_eq!(held.path, format!("c/{}", held.file));
                assert_eq!(held.container_path, "c");
                held.file
            })
            .collect()
    }

    #[test]
    fn track_files_wait_for_their_sheet() {
        let image = disc();
        let sheet = |text: &str| text.as_bytes().to_vec();
        let files = Files::new(vec![
            ("d/disc.bin", image.clone()),
            ("d/disc.cue", sheet(SHEET)),
            ("d/other.bin", image.clone()),
            ("d/other.cue", sheet(SHEET)),
            ("d/broken.bin", image.clone()),
            (
                "d/broken.cue",
                sheet(
                    &SHEET
                        .replace("disc.bin", "broken.bin")
                        .replace("00:00:30", "00:00:10"),
                ),
            ),
            ("d/wave.wav", image.clone()),
            (
                "d/wave.cue",
                sheet("FILE wave.wav WAVE\nTRACK 01 AUDIO\nINDEX 01 00:00:00\n"),
            ),
            ("d/renamed.BIN", image.clone()),
            (
                "d/renamed.cue",
                sheet(&SHEET.replace("disc.bin", "stale.bin")),
            ),
            ("d/empty.bin", Vec::new()),
            (
                "d/empty.cue",
                sheet(&SHEET.replace("disc.bin", "empty.bin")),
            ),
            ("d/unrelated.bin", image.clone()),
            ("d/Game (Track ).bin", image.clone()),
            ("disc.bin", image.clone()),
            ("top.bin", image.clone()),
            ("top.cue", sheet(&SHEET.replace("disc.bin", "top.bin"))),
        ]);
        let mut index = SheetIndex::default();
        let held: Vec<&str> = files
            .files
            .iter()
            .map(|(path, _)| *path)
            .filter(|path| files.visit(&mut index, path))
            .collect();
        // Not held back: files of sheets that read another file or no track
        // of this one, files in another directory, files without a sheet.
        assert_eq!(held, ["d/disc.bin", "d/renamed.BIN", "top.bin"]);

        // Only the sheets that were opened keep their files out.
        index.sheet_opened("d/disc.cue");
        index.sheet_opened("top.cue");
        assert_eq!(unlisted(&mut index), ["d/renamed.BIN"]);
        assert!(unlisted(&mut index).is_empty());
        // Afterwards the answer is immediate.
        assert!(files.visit(&mut index, "d/disc.bin"));
        assert!(!files.visit(&mut index, "d/renamed.BIN"));
        assert!(unlisted(&mut index).is_empty());
    }

    #[test]
    fn only_files_that_open_as_sheets_claim_track_files() {
        let image = disc();
        // `parse` ignores the unknown first command; recognition does not.
        let files = Files::new(vec![
            ("game.bin", image.clone()),
            (
                "game.cue",
                format!("GENRE Rock\n{SHEET}")
                    .replace("disc.bin", "game.bin")
                    .into_bytes(),
            ),
            ("twice.bin", image.clone()),
            (
                "twice.cue",
                SHEET
                    .replace("disc.bin", "twice.bin")
                    .replace("TRACK 02", "TRACK 01")
                    .into_bytes(),
            ),
            ("fine.bin", image),
            (
                "fine.cue",
                format!("REM x\n{SHEET}")
                    .replace("disc.bin", "fine.bin")
                    .into_bytes(),
            ),
        ]);
        assert!(!is_cue_sheet(files.get("game.cue").unwrap()));
        let mut index = SheetIndex::default();
        assert!(!files.visit(&mut index, "game.bin"));
        assert!(!files.visit(&mut index, "twice.bin"));
        assert!(files.visit(&mut index, "fine.bin"));
    }

    #[test]
    fn a_sheet_is_read_once_for_all_files_named_after_it() {
        let mut sheet: String = (1..=99)
            .map(|n| {
                format!(
                    "FILE \"x (Track {n}).bin\" BINARY\nTRACK {n:02} AUDIO\nINDEX 01 00:00:00\n"
                )
            })
            .collect();
        sheet.push_str(&"REM padding\n".repeat(1000));
        let mut entries = vec![("x.cue", sheet.into_bytes())];
        let names: Vec<String> = (100..400).map(|n| format!("x (Track {n}).bin")).collect();
        for name in &names {
            entries.push((Box::leak(name.clone().into_boxed_str()), vec![0]));
        }
        let files = Files::new(entries);
        let mut index = SheetIndex::default();
        for name in &names {
            assert!(!files.visit(&mut index, name));
        }
        assert_eq!(*files.reads.borrow(), ["x.cue"]);
        // The 99 files of the sheet under three names each, once; then two
        // probes per entry for a sheet named exactly after it.
        assert!(*files.probes.borrow() <= 99 * 3 + 1 + 2 * names.len());
        assert!(!files.visit(&mut index, "x.cue"));
        assert_eq!(files.reads.borrow().len(), 1);
    }

    #[test]
    fn a_file_differing_in_letter_case_is_not_claimed() {
        let image = disc();
        let audio = &image[20 * RAW_SECTOR..];
        let sheet = b"FILE \"game.bin\" BINARY\nTRACK 01 AUDIO\nINDEX 01 00:00:00\n".to_vec();
        let files = Files::new(vec![
            ("GAME.BIN", audio[..3 * RAW_SECTOR].to_vec()),
            ("game.bin", audio[..5 * RAW_SECTOR].to_vec()),
            ("game.cue", sheet.clone()),
        ]);
        let mut index = SheetIndex::default();
        assert!(!files.visit(&mut index, "GAME.BIN"));
        assert!(files.visit(&mut index, "game.bin"));

        // The file the sheet's name finds, whatever its case.
        let files = Files::new(vec![("GAME.BIN", audio.to_vec()), ("game.cue", sheet)]);
        let mut index = SheetIndex::default();
        assert!(files.visit(&mut index, "GAME.BIN"));
    }

    #[test]
    fn sheets_of_any_name_claim_their_files_once_known() {
        let image = disc();
        let data = &image[..20 * RAW_SECTOR];
        let sheet = b"FILE \"My Disc Image.bin\" BINARY\nTRACK 01 MODE1/2352\nINDEX 01 00:00:00\n\
                      FILE \"sub/more.bin\" BINARY\nTRACK 02 MODE1/2352\nINDEX 01 00:00:00\n";
        let files = Files::new(vec![
            ("d/My Disc Image.bin", data.to_vec()),
            ("d/Odd Sheet.cue", sheet.to_vec()),
            ("d/sub/more.bin", data.to_vec()),
            ("My Disc Image.bin", data.to_vec()),
        ]);
        // The sheet comes later and nothing names it: the file is listed.
        let mut index = SheetIndex::default();
        assert!(!files.visit(&mut index, "d/My Disc Image.bin"));

        // The sheet was found in a listing of the directory.
        let mut index = SheetIndex::default();
        assert!(index.enters_directory("d/My Disc Image.bin"));
        assert!(!index.enters_directory("d/Odd Sheet.cue"));
        assert!(index.enters_directory("d/sub/more.bin"));
        index.add_sheet("d/Odd Sheet.cue", files.get("d/Odd Sheet.cue"), &|path| {
            files.get(path).is_some()
        });
        assert!(files.visit(&mut index, "d/My Disc Image.bin"));
        assert!(!files.visit(&mut index, "d/Odd Sheet.cue"));
        // Visited but not opened so far: listed at once.
        assert!(!files.visit(&mut index, "d/sub/more.bin"));
        assert!(!files.visit(&mut index, "My Disc Image.bin"));
        index.sheet_opened("d/Odd Sheet.cue");
        assert!(files.visit(&mut index, "d/sub/more.bin"));
        assert!(unlisted(&mut index).is_empty());
    }

    #[test]
    fn hostile_sheets_do_not_panic() {
        let image = disc();
        let mut state = 0x9E37_79B9_7F4A_7C15u64;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        let pieces = [
            "FILE ",
            "TRACK ",
            "INDEX ",
            "\"",
            "disc.bin",
            " BINARY",
            " MOTOROLA",
            " WAVE",
            "\n",
            "\r",
            " ",
            "\t",
            "00",
            "01",
            "02",
            "99",
            "100",
            ":",
            "00:00:10",
            "00:00:40",
            "99:99:99",
            "9999:9999:9999",
            "AUDIO",
            "MODE1/2352",
            "MODE2/2336",
            "MODE1/2048",
            "CDG",
            "X/2400",
            "X/9",
            "..",
            "/",
            "\\",
            "C:",
            "\u{1A}",
            "é",
            "PREGAP ",
            "REM ",
        ];
        for round in 0..4000 {
            let mut sheet = String::from("FILE \"disc.bin\" BINARY\nTRACK 01 MODE1/2352\n");
            for _ in 0..(next() % 40) {
                sheet.push_str(pieces[(next() % pieces.len() as u64) as usize]);
            }
            let mut bytes = sheet.into_bytes();
            if round % 5 == 0 && !bytes.is_empty() {
                let at = (next() % bytes.len() as u64) as usize;
                bytes[at] = next() as u8;
            }
            let _ = is_cue_sheet(&bytes);
            let cut = (next() % (image.len() as u64 + 1)) as usize;
            let Ok(container) = CueBinContainer::from_bytes_with_siblings(
                &bytes,
                "dir/sheet.cue".into(),
                4,
                &|name| {
                    assert!(name.split('/').all(usable_component), "{name}");
                    Some(image[..cut].to_vec())
                },
            ) else {
                continue;
            };
            let mut total = 0;
            container
                .visit(&mut |entry| {
                    total += entry.data.len();
                    Ok(true)
                })
                .unwrap();
            // Tracks of one file never overlap.
            let files = container.files.iter().filter(|f| !f.is_empty()).count();
            assert!(total <= files * cut, "{total} from {files} x {cut}");
            let _ = container.skipped_entries();
        }
    }
}
