use crate::compat::Vec;
use crate::error::{Error, Result};
#[cfg(any(feature = "common", feature = "xz", test))]
use crate::limits::prealloc_hint;

const PROB_INIT: u16 = 1 << 10;
const PROB_BITS: u32 = 11;
const PROB_SHIFT: u32 = 5;
const RANGE_TOP: u32 = 1 << 24;

const STATES: usize = 12;
const POS_STATES: usize = 16;
const LITERAL_PROBS: usize = 0x300;
const MATCH_MIN: usize = 2;
const MATCH_MAX: usize = MATCH_MIN + 16 + 255;
const LEN_STATES: usize = 4;
const FIRST_DIRECT_SLOT: usize = 14;
const ALIGN_BITS: u32 = 4;
// Reverse trees of distance slots 4..14 share one array; the tree of a slot
// starts at `base distance - slot` and never uses its own index 0.
const SLOT_TREE_PROBS: usize = 115;
const END_MARKER: u32 = u32::MAX;

// Zeroed scratch added to the output at a time while decoding.
const SCRATCH_STEP: usize = 1 << 16;
const WIDE_COPY: usize = 16;

#[cfg(any(feature = "common", feature = "xz", test))]
const MIN_DICT_SIZE: u32 = 1 << 12;
const LZMA2_MAX_DICT_BITS: u8 = 40;
const LZMA2_MAX_LC_LP: u8 = 4;

fn malformed(what: &'static str) -> Error {
    Error::decompression(what)
}

fn truncated() -> Error {
    malformed("lzma: unexpected end of compressed data")
}

fn too_large() -> Error {
    malformed("lzma: output exceeds the size limit")
}

fn out_of_memory() -> Error {
    malformed("lzma: cannot allocate buffer")
}

fn bad_distance() -> Error {
    malformed("lzma: match distance reaches outside the dictionary")
}

fn chunk_truncated() -> Error {
    malformed("lzma2: unexpected end of chunk data")
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct LzmaProps {
    pub lc: u8,
    pub lp: u8,
    pub pb: u8,
}

impl LzmaProps {
    // Splits the encoded `(pb * 5 + lp) * 9 + lc` byte.
    pub(crate) fn from_byte(byte: u8) -> Result<Self> {
        if byte >= 9 * 5 * 5 {
            return Err(malformed("lzma: invalid properties byte"));
        }
        let rest = byte / 9;
        Ok(Self {
            lc: byte % 9,
            lp: rest % 5,
            pb: rest / 5,
        })
    }
}

// Parses the 5-byte properties block shared by `.lzma` files and ZIP method
// 14: the properties byte and the little-endian dictionary size.
#[cfg(any(feature = "common", feature = "xz", test))]
pub(crate) fn lzma_parse_props(bytes: &[u8]) -> Result<(LzmaProps, u32)> {
    let Some(&[props, d0, d1, d2, d3]) = bytes.first_chunk::<5>() else {
        return Err(malformed("lzma: properties are truncated"));
    };
    Ok((
        LzmaProps::from_byte(props)?,
        u32::from_le_bytes([d0, d1, d2, d3]),
    ))
}

// Dictionary size from the one-byte LZMA2 filter properties of `.xz`.
pub(crate) fn lzma2_dict_size(props: u8) -> Result<u32> {
    if props & 0xC0 != 0 {
        return Err(malformed("lzma2: reserved dictionary size bits are set"));
    }
    match props {
        LZMA2_MAX_DICT_BITS => Ok(u32::MAX),
        bits if bits < LZMA2_MAX_DICT_BITS => {
            Ok((2 | u32::from(bits & 1)) << (u32::from(bits) / 2 + 11))
        }
        _ => Err(malformed("lzma2: dictionary size is out of range")),
    }
}

#[cfg(any(feature = "common", feature = "xz", test))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum EndMarker {
    // The stream ends at the known size, with or without a marker.
    Optional,
    // The stream ends only at a marker.
    #[cfg_attr(not(any(feature = "common", test)), allow(dead_code))]
    Required,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Marker {
    Forbidden,
    Optional,
    Required,
}

#[cfg(any(feature = "common", feature = "xz", test))]
pub(crate) struct Unlzma {
    pub data: Vec<u8>,
    #[cfg_attr(not(test), allow(dead_code))]
    pub consumed: usize,
}

#[derive(Clone)]
struct RangeDecoder<'a> {
    input: &'a [u8],
    pos: usize,
    range: u32,
    code: u32,
    // Set when a read went past the input; zeros are supplied from then on
    // and the caller reports the error at its next check.
    overrun: bool,
}

impl<'a> RangeDecoder<'a> {
    fn new(input: &'a [u8]) -> Result<Self> {
        let Some(&[first, c0, c1, c2, c3]) = input.first_chunk::<5>() else {
            return Err(truncated());
        };
        if first != 0 {
            return Err(malformed("lzma: range coder does not start with zero"));
        }
        Ok(Self {
            input,
            pos: 5,
            range: u32::MAX,
            code: u32::from_be_bytes([c0, c1, c2, c3]),
            overrun: false,
        })
    }

    #[inline(always)]
    fn normalize(&mut self) {
        if self.range < RANGE_TOP {
            let byte = match self.input.get(self.pos) {
                Some(&byte) => {
                    self.pos += 1;
                    byte
                }
                None => {
                    self.overrun = true;
                    0
                }
            };
            self.range <<= 8;
            self.code = (self.code << 8) | u32::from(byte);
        }
    }

    #[inline(always)]
    fn bit(&mut self, prob: &mut u16) -> u32 {
        let p = u32::from(*prob);
        let bound = (self.range >> PROB_BITS) * p;
        let bit = if self.code < bound {
            *prob = (p + (((1 << PROB_BITS) - p) >> PROB_SHIFT)) as u16;
            self.range = bound;
            0
        } else {
            *prob = (p - (p >> PROB_SHIFT)) as u16;
            self.code -= bound;
            self.range -= bound;
            1
        };
        self.normalize();
        bit
    }

    // Most significant bit first; `N` is a power of two and slot 0 is unused.
    #[inline(always)]
    fn tree<const N: usize>(&mut self, probs: &mut [u16; N]) -> usize {
        let mut node = 1;
        while node < N {
            node = (node << 1) | self.bit(&mut probs[node]) as usize;
        }
        node - N
    }

    // Least significant bit first; needs `probs.len() >= 1 << bits`.
    #[inline(always)]
    fn reverse_tree(&mut self, probs: &mut [u16], bits: u32) -> u32 {
        let mut node = 1;
        let mut value = 0;
        for shift in 0..bits {
            let bit = self.bit(&mut probs[node]);
            node = (node << 1) | bit as usize;
            value |= bit << shift;
        }
        value
    }

    // Equiprobable bits. A code that is out of range in a corrupt stream is
    // carried along the way the specification defines, not rejected here.
    #[inline(always)]
    fn direct(&mut self, bits: u32) -> u32 {
        let mut value = 0;
        for _ in 0..bits {
            self.range >>= 1;
            let reduced = self.code.wrapping_sub(self.range);
            let bit = 1 - (reduced >> 31);
            if bit == 1 {
                self.code = reduced;
            }
            self.normalize();
            value = (value << 1) | bit;
        }
        value
    }
}

struct LengthModel {
    choice: u16,
    choice2: u16,
    low: [[u16; 8]; POS_STATES],
    mid: [[u16; 8]; POS_STATES],
    high: [u16; 256],
}

impl LengthModel {
    const fn new() -> Self {
        Self {
            choice: PROB_INIT,
            choice2: PROB_INIT,
            low: [[PROB_INIT; 8]; POS_STATES],
            mid: [[PROB_INIT; 8]; POS_STATES],
            high: [PROB_INIT; 256],
        }
    }

    // Length minus `MATCH_MIN`; `pos_state < POS_STATES`.
    #[inline(always)]
    fn decode(&mut self, rc: &mut RangeDecoder<'_>, pos_state: usize) -> usize {
        if rc.bit(&mut self.choice) == 0 {
            rc.tree(&mut self.low[pos_state])
        } else if rc.bit(&mut self.choice2) == 0 {
            8 + rc.tree(&mut self.mid[pos_state])
        } else {
            16 + rc.tree(&mut self.high)
        }
    }
}

// Everything a state reset clears.
struct Model {
    is_match: [u16; STATES * POS_STATES],
    is_rep: [u16; STATES],
    is_rep_g0: [u16; STATES],
    is_rep_g1: [u16; STATES],
    is_rep_g2: [u16; STATES],
    is_rep0_long: [u16; STATES * POS_STATES],
    slot: [[u16; 64]; LEN_STATES],
    slot_trees: [u16; SLOT_TREE_PROBS],
    align: [u16; 1 << ALIGN_BITS],
    match_len: LengthModel,
    rep_len: LengthModel,
    state: usize,
    // Distances minus one, most recent first.
    reps: [u32; 4],
}

impl Model {
    const fn new() -> Self {
        Self {
            is_match: [PROB_INIT; STATES * POS_STATES],
            is_rep: [PROB_INIT; STATES],
            is_rep_g0: [PROB_INIT; STATES],
            is_rep_g1: [PROB_INIT; STATES],
            is_rep_g2: [PROB_INIT; STATES],
            is_rep0_long: [PROB_INIT; STATES * POS_STATES],
            slot: [[PROB_INIT; 64]; LEN_STATES],
            slot_trees: [PROB_INIT; SLOT_TREE_PROBS],
            align: [PROB_INIT; 1 << ALIGN_BITS],
            match_len: LengthModel::new(),
            rep_len: LengthModel::new(),
            state: 0,
            reps: [0; 4],
        }
    }

    // Distance minus one for a match of `len + MATCH_MIN` bytes.
    #[inline(always)]
    fn distance(&mut self, rc: &mut RangeDecoder<'_>, len: usize) -> u32 {
        let slot = rc.tree(&mut self.slot[len.min(LEN_STATES - 1)]);
        if slot < 4 {
            return slot as u32;
        }
        let low_bits = (slot as u32 >> 1) - 1;
        let base = (2 | (slot as u32 & 1)) << low_bits;
        if slot < FIRST_DIRECT_SLOT {
            // At most 96 - 13 + 31, inside the array.
            let tree = &mut self.slot_trees[base as usize - slot..];
            base + rc.reverse_tree(tree, low_bits)
        } else {
            let middle = rc.direct(low_bits - ALIGN_BITS) << ALIGN_BITS;
            base + middle + rc.reverse_tree(&mut self.align, ALIGN_BITS)
        }
    }
}

// Where a run of symbols has to stop.
#[derive(Clone, Copy)]
struct Bounds {
    // First output byte the dictionary may reach.
    dict_start: usize,
    dict_size: u32,
    // Output length to stop at; at least the current length.
    stop: usize,
    // `stop` is a declared size and not just the caller's limit.
    sized: bool,
    marker: Marker,
}

impl Bounds {
    // The error for a symbol that does not fit in `room` more bytes.
    fn overflow(&self, room: usize) -> Error {
        if !self.sized {
            too_large()
        } else if room == 0 && self.marker == Marker::Required {
            malformed("lzma: no end marker at the declared size")
        } else {
            malformed("lzma: data continues past the declared size")
        }
    }
}

struct Decoder {
    model: Model,
    literal: Vec<u16>,
    lc: u32,
    lp_mask: usize,
    pb_mask: usize,
}

impl Decoder {
    fn new() -> Self {
        Self {
            model: Model::new(),
            literal: Vec::new(),
            lc: 0,
            lp_mask: 0,
            pb_mask: 0,
        }
    }

    // Installs new properties and resets the state.
    fn configure(&mut self, props: LzmaProps) -> Result<()> {
        if props.lc > 8 || props.lp > 4 || props.pb > 4 {
            return Err(malformed("lzma: properties are out of range"));
        }
        let tables = LITERAL_PROBS << (props.lc + props.lp);
        self.literal.clear();
        self.literal
            .try_reserve_exact(tables)
            .map_err(|_| out_of_memory())?;
        self.literal.resize(tables, PROB_INIT);
        self.lc = u32::from(props.lc);
        self.lp_mask = (1 << props.lp) - 1;
        self.pb_mask = (1 << props.pb) - 1;
        self.model = Model::new();
        Ok(())
    }

    fn reset_state(&mut self) {
        self.model = Model::new();
        self.literal.fill(PROB_INIT);
    }

    // Decodes symbols until the output reaches `bounds.stop` or an end marker
    // is read. Returns whether a marker ended the run.
    fn run(
        &mut self,
        coder: &mut RangeDecoder<'_>,
        out: &mut Vec<u8>,
        bounds: Bounds,
    ) -> Result<bool> {
        // Working copies, so the loop keeps them out of memory.
        let mut local = coder.clone();
        let rc = &mut local;
        let (lc, lp_mask, pb_mask) = (self.lc, self.lp_mask, self.pb_mask);
        let literal = self.literal.as_mut_slice();
        let Bounds {
            dict_start,
            dict_size,
            stop,
            sized,
            marker,
        } = bounds;
        let model = &mut self.model;
        let mut state = model.state;
        let mut reps = model.reps;
        // `out` is kept longer than the decoded data by a bounded amount of
        // zeroed scratch, so symbols are stored by index; `end` is the real
        // length and `out` is cut back to it on every way out.
        let mut end = out.len();

        let result = loop {
            if rc.overrun {
                break Err(truncated());
            }
            let room = stop.saturating_sub(end);
            if room == 0
                && sized
                && (marker == Marker::Forbidden || (marker == Marker::Optional && rc.code == 0))
            {
                model.state = state;
                model.reps = reps;
                break Ok(false);
            }
            if out.len() - end < MATCH_MAX && out.len() < stop {
                if let Err(err) = extend_scratch(out, stop) {
                    break Err(err);
                }
            }
            let pos = end.saturating_sub(dict_start);
            let pos_state = pos & pb_mask;
            let context = (state << 4) | pos_state;

            if rc.bit(&mut model.is_match[context]) == 0 {
                if room == 0 {
                    break Err(bounds.overflow(room));
                }
                let previous = if pos == 0 {
                    0
                } else {
                    usize::from(out[end - 1])
                };
                let table = ((pos & lp_mask) << lc) | (previous >> (8 - lc));
                let Some(probs) = literal
                    .get_mut(table * LITERAL_PROBS..)
                    .and_then(|rest| rest.first_chunk_mut::<LITERAL_PROBS>())
                else {
                    break Err(malformed("lzma: literal context is out of range"));
                };

                let mut symbol = 1;
                if state >= 7 {
                    // After a match the literal is coded against the byte the
                    // match would have continued with, while the two agree.
                    let back = reps[0] as usize;
                    if back >= pos {
                        break Err(bad_distance());
                    }
                    let mut expected = usize::from(out[end - 1 - back]);
                    while symbol < 0x100 {
                        let expected_bit = (expected >> 7) & 1;
                        expected <<= 1;
                        let prob = &mut probs[((1 + expected_bit) << 8) + symbol];
                        let bit = rc.bit(prob) as usize;
                        symbol = (symbol << 1) | bit;
                        if bit != expected_bit {
                            break;
                        }
                    }
                }
                while symbol < 0x100 {
                    symbol = (symbol << 1) | rc.bit(&mut probs[symbol]) as usize;
                }
                // `room != 0`, so the scratch reaches past `end`.
                out[end] = symbol as u8;
                end += 1;
                state = match state {
                    0..=3 => 0,
                    4..=9 => state - 3,
                    _ => state - 6,
                };
                continue;
            }

            let len;
            if rc.bit(&mut model.is_rep[state]) == 0 {
                len = model.match_len.decode(rc, pos_state);
                let distance = model.distance(rc, len);
                reps = [distance, reps[0], reps[1], reps[2]];
                state = if state < 7 { 7 } else { 10 };
                if distance == END_MARKER {
                    break if rc.overrun {
                        Err(truncated())
                    } else if marker == Marker::Forbidden {
                        Err(malformed("lzma: end marker is not allowed here"))
                    } else if sized && room != 0 {
                        Err(malformed("lzma: end marker before the declared size"))
                    } else if rc.code != 0 {
                        Err(malformed("lzma: range coder is not finished at end marker"))
                    } else {
                        Ok(true)
                    };
                }
                if room == 0 {
                    break Err(bounds.overflow(room));
                }
            } else {
                if room == 0 {
                    break Err(bounds.overflow(room));
                }
                if pos == 0 {
                    break Err(malformed("lzma: repeated match with an empty dictionary"));
                }
                if rc.bit(&mut model.is_rep_g0[state]) == 0 {
                    if rc.bit(&mut model.is_rep0_long[context]) == 0 {
                        let back = reps[0] as usize;
                        if back >= pos {
                            break Err(bad_distance());
                        }
                        out[end] = out[end - 1 - back];
                        end += 1;
                        state = if state < 7 { 9 } else { 11 };
                        continue;
                    }
                } else {
                    let index = if rc.bit(&mut model.is_rep_g1[state]) == 0 {
                        1
                    } else if rc.bit(&mut model.is_rep_g2[state]) == 0 {
                        2
                    } else {
                        3
                    };
                    let distance = reps[index];
                    reps.copy_within(0..index, 1);
                    reps[0] = distance;
                }
                len = model.rep_len.decode(rc, pos_state);
                state = if state < 7 { 8 } else { 11 };
            }

            let back = reps[0];
            if back >= dict_size || back as usize >= pos {
                break Err(bad_distance());
            }
            let len = len + MATCH_MIN;
            if len > room {
                break Err(bounds.overflow(room));
            }
            // `len <= MATCH_MAX` and `len <= room`, so the scratch covers it.
            copy_match(out, end, back as usize + 1, len);
            end += len;
        };
        out.truncate(end);
        *coder = local;
        result
    }
}

// Lengthens `out` with zeros towards `stop`, a bounded step at a time.
#[cold]
#[inline(never)]
fn extend_scratch(out: &mut Vec<u8>, stop: usize) -> Result<()> {
    let target = stop.min(out.len().saturating_add(SCRATCH_STEP));
    out.try_reserve(target - out.len())
        .map_err(|_| out_of_memory())?;
    out.resize(target, 0);
    Ok(())
}

// Copies `len` bytes to `end` from `distance` bytes before it. Needs
// `1 <= distance <= end` and `end + len <= buf.len()`; bytes after the copy
// are scratch and may be overwritten.
#[inline(always)]
fn copy_match(buf: &mut [u8], end: usize, distance: usize, len: usize) {
    let start = end - distance;
    let (head, tail) = buf.split_at_mut(end);
    if len <= WIDE_COPY {
        // Short matches dominate; a fixed-width copy avoids a variable-length
        // one whenever the source and the scratch are both wide enough.
        if let (Some(from), Some(to)) = (
            head[start..].first_chunk::<WIDE_COPY>(),
            tail.first_chunk_mut::<WIDE_COPY>(),
        ) {
            *to = *from;
            return;
        }
    }
    if distance >= len {
        tail[..len].copy_from_slice(&head[start..start + len]);
    } else if distance == 1 {
        tail[..len].fill(head[start]);
    } else if len <= WIDE_COPY {
        let window = &mut buf[start..end + len];
        for index in distance..window.len() {
            window[index] = window[index - distance];
        }
    } else {
        // Overlapping source: the output repeats with period `distance`, so
        // each round can copy everything written since `start`.
        let mut done = 0;
        while done < len {
            let chunk = (len - done).min(distance + done);
            buf.copy_within(start..start + chunk, end + done);
            done += chunk;
        }
    }
}

// Decodes one raw LZMA stream from the start of `input`.
//
// `dict_size` below 4 KiB is raised to 4 KiB. With `known_size` the stream
// ends exactly there; `marker` says whether an end marker may or must follow,
// and one that is present is consumed. A marker before `known_size` is an
// error: the specification is not explicit, the reference decoders tried as
// black boxes all reject it. Without `known_size` the stream runs
// to its end marker whatever `marker` says. `size_hint` is an untrusted
// declared output size used when `known_size` is absent. `consumed` counts
// the bytes the range coder read; anything after them is left alone.
#[cfg(any(feature = "common", feature = "xz", test))]
pub(crate) fn lzma_decode(
    input: &[u8],
    props: LzmaProps,
    dict_size: u32,
    known_size: Option<u64>,
    marker: EndMarker,
    size_hint: u64,
    max_output: u64,
) -> Result<Unlzma> {
    let limit = usize::try_from(max_output).unwrap_or(usize::MAX);
    let stop = match known_size {
        Some(size) => usize::try_from(size)
            .ok()
            .filter(|&size| size <= limit)
            .ok_or_else(too_large)?,
        None => limit,
    };

    let mut decoder = Decoder::new();
    decoder.configure(props)?;
    let mut rc = RangeDecoder::new(input)?;

    let mut out = Vec::new();
    out.try_reserve(prealloc_hint(known_size.unwrap_or(size_hint)).min(limit))
        .map_err(|_| out_of_memory())?;

    let bounds = Bounds {
        dict_start: 0,
        dict_size: dict_size.max(MIN_DICT_SIZE),
        stop,
        sized: known_size.is_some(),
        marker: match (known_size, marker) {
            (Some(_), EndMarker::Optional) => Marker::Optional,
            _ => Marker::Required,
        },
    };
    decoder.run(&mut rc, &mut out, bounds)?;
    Ok(Unlzma {
        data: out,
        consumed: rc.pos,
    })
}

// Decodes a legacy `.lzma` file: the 5-byte properties, a 64-bit
// little-endian uncompressed size (all ones: unknown, ended by a marker) and
// the stream. `consumed` includes the 13 header bytes.
#[cfg(any(feature = "xz", test))]
pub(crate) fn lzma_alone_decode(input: &[u8], max_output: u64) -> Result<Unlzma> {
    let (props, dict_size) = lzma_parse_props(input)?;
    let Some((size, stream)) = input
        .get(5..)
        .and_then(|rest| rest.split_first_chunk::<8>())
    else {
        return Err(malformed("lzma: header is truncated"));
    };
    let size = u64::from_le_bytes(*size);
    let known_size = (size != u64::MAX).then_some(size);
    let mut decoded = lzma_decode(
        stream,
        props,
        dict_size,
        known_size,
        EndMarker::Optional,
        0,
        max_output,
    )?;
    decoded.consumed += 13;
    Ok(decoded)
}

fn be16(input: &[u8], pos: usize) -> Result<usize> {
    match input.get(pos..).and_then(|rest| rest.first_chunk::<2>()) {
        Some(&bytes) => Ok(usize::from(u16::from_be_bytes(bytes))),
        None => Err(chunk_truncated()),
    }
}

fn take(input: &[u8], pos: usize, len: usize) -> Result<&[u8]> {
    pos.checked_add(len)
        .and_then(|end| input.get(pos..end))
        .ok_or_else(chunk_truncated)
}

// Decodes LZMA2 chunks from the start of `input` up to and including the end
// marker, appending to `out`, and returns the input bytes consumed.
//
// `dict_size` is the decoded filter property (`lzma2_dict_size`). Bytes
// already in `out` are not part of the dictionary. `out` never grows past
// `max_output` bytes in total; on error it keeps what was decoded so far.
//
// The chunk format has no specification and its description leaves the
// following open. Each was settled by feeding crafted streams to reference
// decoders used as black boxes, which reject every violation:
// - the first chunk has to reset the dictionary;
// - the first LZMA chunk after an uncompressed chunk that resets the
//   dictionary has to carry a properties byte;
// - an LZMA chunk holds no end marker, uses exactly its compressed size and
//   leaves the range coder with a zero code;
// - the first byte of an LZMA chunk's range coder data has to be zero, as in
//   a plain LZMA stream;
// - uncompressed chunks leave the LZMA state, repeat distances and
//   probabilities alone.
pub(crate) fn lzma2_decode(
    input: &[u8],
    dict_size: u32,
    out: &mut Vec<u8>,
    max_output: u64,
) -> Result<usize> {
    let limit = usize::try_from(max_output).unwrap_or(usize::MAX);
    let mut pos = 0usize;
    let mut dict_start = out.len();
    let mut need_dict_reset = true;
    let mut need_props = true;
    let mut decoder = Decoder::new();

    loop {
        let Some(&control) = input.get(pos) else {
            return Err(malformed("lzma2: missing end marker"));
        };
        pos += 1;
        match control {
            0 => return Ok(pos),
            1 | 2 => {
                if control == 1 {
                    dict_start = out.len();
                    need_dict_reset = false;
                    need_props = true;
                } else if need_dict_reset {
                    return Err(malformed(
                        "lzma2: first chunk does not reset the dictionary",
                    ));
                }
                let size = be16(input, pos)? + 1;
                let data = take(input, pos + 2, size)?;
                pos += 2 + size;
                if size > limit.saturating_sub(out.len()) {
                    return Err(malformed("lzma2: output exceeds the size limit"));
                }
                out.try_reserve(size).map_err(|_| out_of_memory())?;
                out.extend_from_slice(data);
            }
            3..=0x7F => return Err(malformed("lzma2: invalid control byte")),
            _ => {
                let unpacked = ((usize::from(control & 0x1F) << 16) | be16(input, pos)?) + 1;
                let packed = be16(input, pos + 2)? + 1;
                pos += 4;

                let reset = (control >> 5) & 3;
                if reset == 3 {
                    dict_start = out.len();
                    need_dict_reset = false;
                } else if need_dict_reset {
                    return Err(malformed(
                        "lzma2: first chunk does not reset the dictionary",
                    ));
                }
                if reset >= 2 {
                    let Some(&byte) = input.get(pos) else {
                        return Err(chunk_truncated());
                    };
                    pos += 1;
                    let props = LzmaProps::from_byte(byte)
                        .map_err(|_| malformed("lzma2: invalid properties byte"))?;
                    if props.lc + props.lp > LZMA2_MAX_LC_LP {
                        return Err(malformed("lzma2: lc + lp exceeds 4"));
                    }
                    decoder.configure(props)?;
                    need_props = false;
                } else if need_props {
                    return Err(malformed("lzma2: chunk needs new properties"));
                } else if reset == 1 {
                    decoder.reset_state();
                }

                let chunk = take(input, pos, packed)?;
                if unpacked > limit.saturating_sub(out.len()) {
                    return Err(malformed("lzma2: output exceeds the size limit"));
                }
                let bounds = Bounds {
                    dict_start,
                    dict_size,
                    stop: out.len() + unpacked,
                    sized: true,
                    marker: Marker::Forbidden,
                };
                let mut rc = RangeDecoder::new(chunk)?;
                decoder.run(&mut rc, out, bounds)?;
                if rc.pos != chunk.len() || rc.code != 0 {
                    return Err(malformed(
                        "lzma2: chunk does not end at its declared compressed size",
                    ));
                }
                pos += packed;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compat::vec;

    const LIMIT: u64 = 1 << 26;
    const DICT: u32 = 1 << 23;
    const TEXT_LEN: usize = 1200;

    fn hex(text: &str) -> Vec<u8> {
        let digits: Vec<u8> = text
            .bytes()
            .filter(|b| b.is_ascii_hexdigit())
            .map(|b| (b as char).to_digit(16).unwrap() as u8)
            .collect();
        assert_eq!(digits.len() % 2, 0);
        digits
            .chunks(2)
            .map(|pair| pair[0] << 4 | pair[1])
            .collect()
    }

    fn fnv(data: &[u8]) -> u64 {
        data.iter().fold(0xcbf2_9ce4_8422_2325, |hash, &byte| {
            (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3)
        })
    }

    fn step(x: &mut u64) -> u64 {
        *x ^= *x << 13;
        *x ^= *x >> 7;
        *x ^= *x << 17;
        *x
    }

    // The generators below mirror the script that produced the vectors.
    fn random(len: usize, seed: u64) -> Vec<u8> {
        let mut x = seed;
        (0..len).map(|_| (step(&mut x) >> 32) as u8).collect()
    }

    fn text(len: usize, seed: u64) -> Vec<u8> {
        const WORDS: [&str; 16] = [
            "archive", "resource", "fork", "finder", "volume", "sector", "catalog", "extent",
            "bitmap", "block", "track", "header", "stream", "packet", "window", "range",
        ];
        let mut x = seed;
        let mut out = Vec::new();
        while out.len() < len {
            let value = step(&mut x);
            out.extend_from_slice(WORDS[(value >> 40) as usize & 15].as_bytes());
            out.push(if (value >> 50) & 7 != 0 { b' ' } else { b'\n' });
        }
        out.truncate(len);
        out
    }

    fn quad(len: u64) -> Vec<u8> {
        (0..len).map(|i| ((i * i) >> 3) as u8).collect()
    }

    fn runs() -> Vec<u8> {
        let mut out = vec![0u8; 5000];
        out.extend_from_slice(&b"ab".repeat(3000));
        out.push(0xFF);
        out.extend_from_slice(&b"xyz".repeat(700));
        out
    }

    fn sample() -> Vec<u8> {
        text(TEXT_LEN, 7)
    }

    fn alone(vector: &str) -> Vec<u8> {
        let input = hex(vector);
        let decoded = lzma_alone_decode(&input, LIMIT).unwrap();
        assert_eq!(decoded.consumed, input.len());
        decoded.data
    }

    fn alone_error(input: &[u8]) -> String {
        match lzma_alone_decode(input, LIMIT) {
            Ok(_) => panic!("decoding should fail"),
            Err(err) => err.to_string(),
        }
    }

    fn raw2_with(input: &[u8], dict_size: u32) -> Result<Vec<u8>> {
        let mut out = Vec::new();
        let consumed = lzma2_decode(input, dict_size, &mut out, LIMIT)?;
        assert_eq!(consumed, input.len());
        Ok(out)
    }

    fn raw2(vector: &str) -> Vec<u8> {
        raw2_with(&hex(vector), DICT).unwrap()
    }

    fn raw2_error(input: &[u8]) -> String {
        let mut out = Vec::new();
        match lzma2_decode(input, DICT, &mut out, LIMIT) {
            Ok(_) => panic!("decoding should fail"),
            Err(err) => err.to_string(),
        }
    }

    fn uncompressed_chunk(control: u8, data: &[u8]) -> Vec<u8> {
        let mut chunk = vec![control];
        chunk.extend_from_slice(&(data.len() as u16 - 1).to_be_bytes());
        chunk.extend_from_slice(data);
        chunk
    }

    // An LZMA2 vector's single LZMA chunk with its control bits replaced and,
    // below a properties reset, its properties byte dropped.
    fn rechunk(vector: &[u8], reset: u8) -> Vec<u8> {
        assert_eq!(vector[0] & 0xE0, 0xE0);
        let mut chunk = vector[..vector.len() - 1].to_vec();
        chunk[0] = 0x80 | (reset << 5) | (chunk[0] & 0x1F);
        if reset < 2 {
            chunk.remove(5);
        }
        chunk
    }

    #[test]
    fn properties_parsing() {
        let props = LzmaProps::from_byte(0x5D).unwrap();
        assert_eq!((props.lc, props.lp, props.pb), (3, 0, 2));
        let props = LzmaProps::from_byte(224).unwrap();
        assert_eq!((props.lc, props.lp, props.pb), (8, 4, 4));
        assert!(LzmaProps::from_byte(225).is_err());

        let (props, dict) = lzma_parse_props(&[0x5D, 0, 0, 0x80, 0, 0xAA]).unwrap();
        assert_eq!((props.lc, props.lp, props.pb, dict), (3, 0, 2, 1 << 23));
        assert!(lzma_parse_props(&[0x5D, 0, 0, 0x80]).is_err());

        assert_eq!(lzma2_dict_size(0).unwrap(), 4096);
        assert_eq!(lzma2_dict_size(1).unwrap(), 6144);
        assert_eq!(lzma2_dict_size(22).unwrap(), 1 << 23);
        assert_eq!(lzma2_dict_size(39).unwrap(), 3 << 30);
        assert_eq!(lzma2_dict_size(40).unwrap(), u32::MAX);
        assert!(lzma2_dict_size(41).is_err());
        assert!(lzma2_dict_size(0x40).is_err());
    }

    #[test]
    fn alone_small_inputs() {
        assert!(alone(ALONE_EMPTY).is_empty());
        assert_eq!(alone(ALONE_ONE), b"Z");
        assert_eq!(alone(ALONE_RUNS), runs());
    }

    #[test]
    fn alone_presets() {
        let expected = sample();
        assert_eq!(fnv(&expected), 0x51ba_f3e1_ca05_7aa2);
        for vector in [
            ALONE_TEXT_P0,
            ALONE_TEXT_P1,
            ALONE_TEXT_P2,
            ALONE_TEXT_P3,
            ALONE_TEXT_P4,
            ALONE_TEXT_P5,
            ALONE_TEXT_P6,
            ALONE_TEXT_P7,
            ALONE_TEXT_P8,
            ALONE_TEXT_P9,
        ] {
            assert_eq!(alone(vector), expected);
        }
    }

    #[test]
    fn alone_property_combinations() {
        let expected = sample();
        for vector in [
            ALONE_TEXT_LC0_LP0_PB0,
            ALONE_TEXT_LC3_LP0_PB2,
            ALONE_TEXT_LC1_LP3_PB1,
            ALONE_TEXT_LC4_LP0_PB0,
            ALONE_TEXT_LC0_LP4_PB3,
        ] {
            assert_eq!(alone(vector), expected);
        }
    }

    #[test]
    fn alone_known_size_with_marker() {
        let mut input = hex(ALONE_TEXT_P6);
        assert_eq!(input[5..13], [0xFF; 8]);
        input[5..13].copy_from_slice(&(TEXT_LEN as u64).to_le_bytes());
        let decoded = lzma_alone_decode(&input, LIMIT).unwrap();
        assert_eq!(decoded.data, sample());
        assert_eq!(decoded.consumed, input.len());

        input[5..13].copy_from_slice(&(TEXT_LEN as u64 - 1).to_le_bytes());
        assert!(alone_error(&input).contains("past the declared size"));
        input[5..13].copy_from_slice(&(TEXT_LEN as u64 + 1).to_le_bytes());
        assert!(alone_error(&input).contains("before the declared size"));
    }

    #[test]
    fn raw_lzma_without_header() {
        let input = hex(RAW1_TEXT_LC2_LP1_PB3);
        let props = LzmaProps {
            lc: 2,
            lp: 1,
            pb: 3,
        };
        let size = Some(TEXT_LEN as u64);
        let decoded =
            lzma_decode(&input, props, 1 << 16, size, EndMarker::Optional, 0, LIMIT).unwrap();
        assert_eq!(decoded.data, sample());
        assert_eq!(decoded.consumed, input.len());

        let wrong = LzmaProps {
            lc: 2,
            lp: 1,
            pb: 9,
        };
        assert!(lzma_decode(&input, wrong, 1 << 16, size, EndMarker::Optional, 0, LIMIT).is_err());
    }

    // ZIP method 14 data segment: version, properties size, properties, stream.
    fn zip_segment(vector: &str) -> (Vec<u8>, LzmaProps, u32) {
        let segment = hex(vector);
        assert_eq!(segment[2..4], [5, 0]);
        let (props, dict_size) = lzma_parse_props(&segment[4..]).unwrap();
        (segment[9..].to_vec(), props, dict_size)
    }

    #[test]
    fn zip_streams_with_end_marker() {
        let expected = sample();
        for (vector, lc, lp, pb) in [
            (ZIP14_844, 8, 4, 4),
            (ZIP14_800, 8, 0, 0),
            (ZIP14_531, 5, 3, 1),
        ] {
            let (stream, props, dict) = zip_segment(vector);
            assert_eq!((props.lc, props.lp, props.pb), (lc, lp, pb));
            for size in [None, Some(TEXT_LEN as u64)] {
                let decoded =
                    lzma_decode(&stream, props, dict, size, EndMarker::Required, 0, LIMIT).unwrap();
                assert_eq!(decoded.data, expected);
                assert_eq!(decoded.consumed, stream.len());
            }
        }
    }

    #[test]
    fn zip_stream_without_end_marker() {
        let (mut stream, props, dict) = zip_segment(ZIP14_NOEOS);
        let size = Some(TEXT_LEN as u64);
        let len = stream.len();
        stream.extend_from_slice(b"trailing bytes");
        let decoded =
            lzma_decode(&stream, props, dict, size, EndMarker::Optional, 0, LIMIT).unwrap();
        assert_eq!(decoded.data, sample());
        assert_eq!(decoded.consumed, len);

        let stream = &stream[..len];
        let err = lzma_decode(stream, props, dict, size, EndMarker::Required, 0, LIMIT)
            .map(|_| ())
            .unwrap_err();
        assert!(err.to_string().contains("no end marker"), "{err}");
        assert!(lzma_decode(stream, props, dict, None, EndMarker::Optional, 0, LIMIT).is_err());
        let short = Some(TEXT_LEN as u64 - 1);
        assert!(lzma_decode(stream, props, dict, short, EndMarker::Optional, 0, LIMIT).is_err());
    }

    fn naive_match(buf: &mut [u8], end: usize, distance: usize, len: usize) {
        for index in end..end + len {
            buf[index] = buf[index - distance];
        }
    }

    #[test]
    fn match_copies_agree_with_bytewise_copy() {
        let history = random(64, 23);
        for distance in 1..=64 {
            for len in [2, 3, 5, 15, 16, 17, 31, 40, 100, 273] {
                for slack in [0, 1, 15, 16, 40] {
                    let mut expected = history.clone();
                    expected.resize(64 + len, 0);
                    naive_match(&mut expected, 64, distance, len);

                    let mut buf = history.clone();
                    buf.resize(64 + len + slack, 0);
                    copy_match(&mut buf, 64, distance, len);
                    assert_eq!(buf[..64 + len], expected, "{distance} {len} {slack}");
                }
            }
        }
    }

    #[test]
    fn failed_runs_leave_no_scratch_behind() {
        let good = hex(RAW2_TEXT_P6);
        let sample = sample();
        for index in 6..good.len() - 1 {
            let mut input = good.clone();
            input[index] ^= 0x10;
            let mut out = b"prefix".to_vec();
            if lzma2_decode(&input, DICT, &mut out, LIMIT).is_err() {
                assert!(out.len() <= 6 + TEXT_LEN, "{index}");
                assert_eq!(&out[..6], b"prefix");
                // Whatever came before the damaged byte took effect is intact.
                let intact = out[6..]
                    .iter()
                    .zip(&sample)
                    .take_while(|(a, b)| a == b)
                    .count();
                assert!(intact > 0 || index < 16, "{index}");
            }
        }

        let mut input = hex(ALONE_TEXT_P6);
        input.truncate(input.len() / 2);
        assert!(alone_error(&input).contains("unexpected end"));
    }

    #[test]
    fn alone_multi_megabyte() {
        let data = alone(ALONE_QUAD);
        assert_eq!(data.len(), 3 << 20);
        assert_eq!(fnv(&data), 0xb0e4_8956_76fa_2325);
        assert_eq!(data, quad(3 << 20));
    }

    #[test]
    fn distance_beyond_dictionary_size() {
        let mut expected = random(300, 17);
        expected.extend_from_slice(&[0; 5000]);
        expected.extend_from_slice(&random(300, 17));

        assert_eq!(alone(ALONE_FAR), expected);
        let mut input = hex(ALONE_FAR);
        input[1..5].copy_from_slice(&5299u32.to_le_bytes());
        assert!(alone_error(&input).contains("outside the dictionary"));
        input[1..5].copy_from_slice(&5300u32.to_le_bytes());
        assert_eq!(lzma_alone_decode(&input, LIMIT).unwrap().data, expected);
        // Sizes below 4 KiB count as 4 KiB.
        input[1..5].copy_from_slice(&0u32.to_le_bytes());
        assert!(alone_error(&input).contains("outside the dictionary"));

        let input = hex(RAW2_FAR);
        assert_eq!(raw2_with(&input, 5300).unwrap(), expected);
        let err = raw2_with(&input, 4096).unwrap_err().to_string();
        assert!(err.contains("outside the dictionary"), "{err}");
    }

    #[test]
    fn alone_corrupt_inputs() {
        let good = hex(ALONE_TEXT_P6);
        for len in 0..good.len() {
            assert!(lzma_alone_decode(&good[..len], LIMIT).is_err(), "{len}");
        }

        let mut input = good.clone();
        input[13] = 1;
        assert!(alone_error(&input).contains("does not start with zero"));
        let mut input = good.clone();
        input[0] = 225;
        assert!(alone_error(&input).contains("invalid properties byte"));

        // Any single-bit error either fails or stays within the limit.
        for index in 0..good.len() {
            for bit in 0..8 {
                let mut input = good.clone();
                input[index] ^= 1 << bit;
                if let Ok(decoded) = lzma_alone_decode(&input, 1 << 16) {
                    assert!(decoded.data.len() <= 1 << 16);
                }
            }
        }

        // A zero code decodes as zero literals until the input runs out.
        let mut header = vec![0x5D, 0, 0, 1, 0];
        header.extend_from_slice(&[0xFF; 8]);
        let zeros = [&header[..], &[0; 64]].concat();
        assert!(alone_error(&zeros).contains("unexpected end"));
        let garbage = [&header[..], &[0], &random(4096, 3)].concat();
        assert!(lzma_alone_decode(&garbage, 1 << 16).is_err());
    }

    #[test]
    fn output_limits() {
        let input = hex(ALONE_TEXT_P6);
        assert!(lzma_alone_decode(&input, TEXT_LEN as u64).is_ok());
        let err = lzma_alone_decode(&input, TEXT_LEN as u64 - 1)
            .map(|_| ())
            .unwrap_err();
        assert!(err.to_string().contains("exceeds the size limit"), "{err}");

        // A declared size above the limit fails before any decoding.
        let mut sized = input.clone();
        sized[5..13].copy_from_slice(&(1u64 << 40).to_le_bytes());
        assert!(alone_error(&sized).contains("exceeds the size limit"));

        let input = hex(RAW2_TEXT_P6);
        let mut out = b"prefix".to_vec();
        let consumed = lzma2_decode(&input, DICT, &mut out, TEXT_LEN as u64 + 6).unwrap();
        assert_eq!(consumed, input.len());
        assert_eq!(&out[..6], b"prefix");
        assert_eq!(&out[6..], sample());

        let mut out = b"prefix".to_vec();
        let err = lzma2_decode(&input, DICT, &mut out, TEXT_LEN as u64 + 5).unwrap_err();
        assert!(err.to_string().contains("exceeds the size limit"), "{err}");

        let input = hex(RAW2_RANDOM);
        let mut out = Vec::new();
        assert!(lzma2_decode(&input, DICT, &mut out, 63).is_err());
        assert!(lzma2_decode(&input, DICT, &mut out, 64).is_ok());
    }

    #[test]
    fn lzma2_small_inputs() {
        assert!(raw2(RAW2_EMPTY).is_empty());
        assert_eq!(raw2(RAW2_ONE), b"Z");
        assert_eq!(raw2(RAW2_RUNS), runs());
    }

    #[test]
    fn lzma2_presets() {
        let expected = sample();
        for vector in [
            RAW2_TEXT_P0,
            RAW2_TEXT_P1,
            RAW2_TEXT_P2,
            RAW2_TEXT_P3,
            RAW2_TEXT_P4,
            RAW2_TEXT_P5,
            RAW2_TEXT_P6,
            RAW2_TEXT_P7,
            RAW2_TEXT_P8,
            RAW2_TEXT_P9,
        ] {
            assert_eq!(raw2(vector), expected);
        }
    }

    #[test]
    fn lzma2_property_combinations() {
        let expected = sample();
        for vector in [
            RAW2_TEXT_LC0_LP0_PB0,
            RAW2_TEXT_LC4_LP0_PB4,
            RAW2_TEXT_LC0_LP4_PB2,
            RAW2_TEXT_LC2_LP2_PB0,
            RAW2_TEXT_LC1_LP3_PB3,
        ] {
            assert_eq!(raw2(vector), expected);
        }
    }

    #[test]
    fn lzma2_uncompressed_chunk() {
        let input = hex(RAW2_RANDOM);
        assert_eq!(input[0], 1);
        assert_eq!(raw2_with(&input, DICT).unwrap(), random(64, 9));
    }

    #[test]
    fn lzma2_multiple_chunks() {
        let input = hex(RAW2_QUAD);
        // Full reset, then a chunk that resets nothing.
        assert_eq!(input[0], 0xFF);
        let data = raw2_with(&input, DICT).unwrap();
        assert_eq!(data.len(), 3 << 20);
        assert_eq!(fnv(&data), 0xb0e4_8956_76fa_2325);
        assert_eq!(data, quad(3 << 20));
    }

    #[test]
    fn lzma2_stops_at_end_marker() {
        let mut input = hex(RAW2_TEXT_P6);
        let len = input.len();
        input.extend_from_slice(&[0xE0, 1, 2, 3]);
        let mut out = Vec::new();
        assert_eq!(lzma2_decode(&input, DICT, &mut out, LIMIT).unwrap(), len);
        assert_eq!(out, sample());
    }

    // The streams below are spliced from encoder output; the reference
    // decoder accepts each of them with the same result.
    #[test]
    fn lzma2_state_survives_uncompressed_chunk() {
        let period = random(256, 11);
        let mut input = hex(RAW2_PERIODIC);
        // Second LZMA chunk, which resets nothing.
        assert_eq!(input[640] & 0xE0, 0x80);
        input.splice(640..640, uncompressed_chunk(2, &period));
        let data = raw2_with(&input, DICT).unwrap();
        assert_eq!(data.len(), 256 * 9001);
        assert_eq!(fnv(&data), 0x1e42_a892_1b2e_c35e);
        assert!(data.chunks(256).all(|chunk| chunk == period));
    }

    #[test]
    fn lzma2_reset_levels() {
        let chunked = hex(RAW2_TEXT_LC0_LP0_PB0);
        let sample = sample();
        let filler = random(1600, 13);

        // Uncompressed dictionary reset, then new properties.
        let mut input = uncompressed_chunk(1, &filler);
        input.extend_from_slice(&rechunk(&chunked, 2));
        input.push(0);
        assert_eq!(
            raw2_with(&input, DICT).unwrap(),
            [&filler[..], &sample].concat()
        );

        // The same without new properties.
        let mut input = uncompressed_chunk(1, &filler);
        input.extend_from_slice(&rechunk(&chunked, 1));
        input.push(0);
        assert!(raw2_error(&input).contains("needs new properties"));

        // State reset that keeps the properties of the first chunk.
        let mut input = rechunk(&chunked, 3);
        input.extend_from_slice(&uncompressed_chunk(2, &filler));
        input.extend_from_slice(&rechunk(&chunked, 1));
        input.push(0);
        assert_eq!(
            raw2_with(&input, DICT).unwrap(),
            [&sample[..], &filler, &sample].concat()
        );

        // Full reset in the middle of a stream.
        let mut input = rechunk(&chunked, 3);
        input.extend_from_slice(&chunked);
        assert_eq!(
            raw2_with(&input, DICT).unwrap(),
            [&sample[..], &sample].concat()
        );
        let mut input = hex(RAW2_RANDOM);
        input.pop();
        input.extend_from_slice(&hex(RAW2_TEXT_P6));
        assert_eq!(
            raw2_with(&input, DICT).unwrap(),
            [&random(64, 9)[..], &sample].concat()
        );

        // Without any reset the second chunk continues the adapted model and
        // no longer decodes.
        let mut input = rechunk(&chunked, 3);
        input.extend_from_slice(&rechunk(&chunked, 0));
        input.push(0);
        assert!(raw2_with(&input, DICT).is_err());
    }

    #[test]
    fn lzma2_first_chunk_rules() {
        let chunked = hex(RAW2_TEXT_P6);
        for reset in 0..3 {
            let mut input = rechunk(&chunked, reset);
            input.push(0);
            assert!(raw2_error(&input).contains("does not reset the dictionary"));
        }
        let mut input = hex(RAW2_RANDOM);
        input[0] = 2;
        assert!(raw2_error(&input).contains("does not reset the dictionary"));
    }

    #[test]
    fn lzma2_corrupt_inputs() {
        let good = hex(RAW2_TEXT_P6);
        for len in 0..good.len() {
            assert!(raw2_with(&good[..len], DICT).is_err(), "{len}");
        }
        let random_chunk = hex(RAW2_RANDOM);
        for len in 0..random_chunk.len() {
            assert!(raw2_with(&random_chunk[..len], DICT).is_err(), "{len}");
        }

        for control in [3, 0x7F] {
            let mut input = good.clone();
            input[0] = control;
            assert!(raw2_error(&input).contains("invalid control byte"));
        }

        let mut input = good.clone();
        input[5] = (2 * 5 + 2) * 9 + 3;
        assert!(raw2_error(&input).contains("lc + lp exceeds 4"));
        input[5] = 225;
        assert!(raw2_error(&input).contains("invalid properties byte"));

        let mut input = good.clone();
        input[6] = 1;
        assert!(raw2_error(&input).contains("does not start with zero"));

        // Declared uncompressed size one too large and one too small.
        let mut input = good.clone();
        input[2] += 1;
        assert!(raw2_with(&input, DICT).is_err());
        input[2] -= 2;
        assert!(raw2_with(&input, DICT).is_err());

        // Declared compressed size one too large.
        let mut input = good.clone();
        input[4] += 1;
        input.insert(good.len() - 1, 0);
        assert!(raw2_error(&input).contains("declared compressed size"));

        // An end marker inside a chunk.
        let alone = hex(ALONE_TEXT_P6);
        let stream = &alone[13..];
        let mut input = vec![0xE0];
        input.extend_from_slice(&(TEXT_LEN as u16 - 1).to_be_bytes());
        input.extend_from_slice(&(stream.len() as u16 - 1).to_be_bytes());
        input.push(alone[0]);
        input.extend_from_slice(stream);
        input.push(0);
        assert!(raw2_with(&input, DICT).is_err());

        for index in 0..good.len() {
            for bit in 0..8 {
                let mut input = good.clone();
                input[index] ^= 1 << bit;
                let mut out = Vec::new();
                if lzma2_decode(&input, DICT, &mut out, 1 << 16).is_ok() {
                    assert!(out.len() <= 1 << 16);
                }
            }
        }
    }

    const ALONE_EMPTY: &str = "\
        5d00008000ffffffffffffffff0083fffbffffc0000000\
    ";

    const ALONE_ONE: &str = "\
        5d00008000ffffffffffffffff002d41fbffffffe0000000\
    ";

    const ALONE_RUNS: &str = "\
        5d00008000ffffffffffffffff00006ffdffffa3b7ff473e481572396151b89228e6a38607f9eecdd08c49dcbc5af8a6\
        ef8709ef4223a9dd4e916552306e190b8034d6dbf6a9b00b04e573fffffd083000\
    ";

    const ALONE_TEXT_P0: &str = "\
        5d00000400ffffffffffffffff00309c888a1c4180a92a80232dcc873c3de5929256571632f53e2c52993b5ad9792174\
        2d5e4e0ed087756af9738b45569c632d2e0de8e3eb6f455c32a6ce44042573c72a5f2b608c3bf9a31b67f58ef839e962\
        b9c5ce0aabe2ed39a7a07a39314db706a1c87853c021d34543b33c9fe7ad2ff6eb89d840b379e57017c4da3ae62ba04c\
        af0eaf9b03759d23bf7d3dfe7c6f6bb7f393721f828e0bcec2a519420686e37c16c6c62b9f9d5cd8e1c9a73b9b517c5b\
        197c67b4731c5c326bfc3d40928b0a4e17d45481edddd6b3092c95b3b6a2bed1bc3f991ce0b38f74408599a09d208d91\
        25df8d2760fb14313e0ed641330876c8fde44a86c4f10bd7f3a985f13c38d793520501acd3023ae9f334db892c22a2f0\
        c0a47e9f732dacf478adb239e86b853dcccce1dc2bd37306d1c740c8966f455a45fac58a5592c5a84ea3dfa359ad368b\
        b6b171bbfe1b4ed5ac948991a1e7383b24c5b8ffe6687054\
    ";

    const ALONE_TEXT_P1: &str = "\
        5d00001000ffffffffffffffff00309c888a1c4180a92a80232dcc873c3de5929256571632f53e2c52993b5ad9792174\
        2d5e4e0ed087756af9738b45569c632d2e0de8e3eb6f455c32a6ce44042573c72a5f2b608c3bf9a31b67f58ef839e962\
        b9c5ce0aabe2ed39a7a07a39314db706a1c87853c021d34543b33c9fe7ad2ff6eb89d840b379e57017c4da3ae62ba04c\
        af0eaf9b03759d23bf7d3dfe7c6f6bb7f393721f828e0bcec2a519420686e37c16c6c62b9f9d5cd8e1c9a73b9b517c5b\
        197c67b4731c5c326bfc3d40928b0a4e17d45481edddd6b3092c95b3b6a2bed1bc3f991ce0b38f74408599c8b5ce092d\
        b0ba5b4806dfa069c4d1ed135dacea91f5135d604c47ca72fd25f288c1c497f35138a1fad05b9c3fa220dbbeb85b4ece\
        cf5fa82bde5095eb02a45f629a30c57ce7e34842b4eceb89afa65a61c8a18baa04782df10e583b9c0953590022f6209c\
        ac045b2958ba45681177cdcda9b7d9199fffab04b00c\
    ";

    const ALONE_TEXT_P2: &str = "\
        5d00002000ffffffffffffffff00309c888a1c4180a92a80232dcc873c3de5929256571632f53e2c52993b5ad9792174\
        2d5e4e0ed087756af9738b45569c632d2e0de8e3eb6f455c32a6ce44042573c72a5f2b608c3bf9a31b67f58ef839e962\
        b9c5ce0aabe2ed39a7a07a39314db706a1c87853c021d34543b33c9fe7ad2ff6eb89d840b379e57017c4da3ae62ba04c\
        af0eaf9b03759d23bf7d3dfe7c6f6bb7f393721f828e0bcec2a519420686e37c16c6c62b9f9d5cd8e1c9a73b9b517c5b\
        197c67b4731c5c326bfc3d40928b0a4e17d45481edddd6b3092c95b3b6a2bed1bc3f991ce0b38f74408599c8b5ce092d\
        b0ba5b4806dfa069c4d1ed135dacea91f5135d604c47ca72fd25f288c1c497f35138a1fad05b9c3fa220dbbeb85b4ece\
        cf5fd87b96901a5a5c748dcc0e98f2c0d809c39a4337c8b04251009ad43ec254591d30f2cef7c92436872f38632b54d3\
        7f6ae19b8d5ff242f1c560719a58eb09ffff98f99422\
    ";

    const ALONE_TEXT_P3: &str = "\
        5d00004000ffffffffffffffff00309c888a1c4180a92a80232dcc873c3de5929256571632f53e2c52993b5ad9792174\
        2d5e4e0ed087756af9738b45569c632d2e0de8e3eb6f455c32a6ce44042573c72a5f2b608c3bf9a31b67f58ef839e962\
        b9c5ce0aabe2ed39a7a07a39314db706a1c87853c021d34543b33c9fe7ad2ff6eb89d840b379e57017c4da3ae62ba04c\
        af0eaf9b03759d23bf7d3dfe7c6f6bb7f393721f828e0bcec2a519420686e37c16c6c62b9f9d5cd8e1c9a73b9b517c5b\
        197c67b4731c5c326bfc3d40928b0a4e17d45481edddd6b3092c95b3b6a2bed1bc3f991ce0b38f74408599c8b5ce092d\
        b0ba5b4806dfa069c4d1ed135dacea91f5135d604c47ca72fd25f288c1c497f35138a1fad05b9c3fa220dbbeb85b4ece\
        cf5fd87b96901a5a5c748dcc0e98f2c0d809c39a4337c8b04251009ad43ec254591d30f2cef7c92436872f38632b54d3\
        7f6ae19b8d5ff242f1c560719a58eb09ffff98f99422\
    ";

    const ALONE_TEXT_P4: &str = "\
        5d00004000ffffffffffffffff00309c888a1c4180a92a80232dcc873c3de5929256571632f53e2c52993b5ad9792174\
        2d5e76e1f74ff984449cef78432628d789da170f07b5e8f622c9c3f9c8802402d9c9e2138b4e8ddd9267bc1c30ba5b84\
        1a62c0a4b4d59096698fa3039f49383985651aba13ef994ec5520901a16ef49b58d799d2c99b2a0d71d2200dddab4e57\
        6cab5412dc1096c9b90779199bf829045429e035f03aee696a020fb303bcba99ba452584937adca6beccc72481881fce\
        d565eead30e54c7ce38d089fc9fd7ef6c9e7b8ade9484890b2dd8bd70a5d1fa03a93e8092e7e399aa08eb4b59e9b01f8\
        201ddeed31cba6c1065873911e87e2dee535d1a19c217d1173497998718373e2640f498911e4ba483132a3c184add858\
        3a6d85a296468882ba2d0b80119c9954667107bc6bda4040d4521c47bdeed19f293b7c43605e4b3d2033b1655853a920\
        90727a719d2fffb8f92500\
    ";

    const ALONE_TEXT_P5: &str = "\
        5d00008000ffffffffffffffff00309c888a1c4180a92a80232dcc873c3de5929256571632f53e2c52993b5ad9792174\
        2d5e76e1f74ff984449cef78432628d789da170f07b5e8f622c9c3f9c8802402d9c9e2138b4e8ddd9267bc1c30ba5b84\
        1a62c0a4b4d59096698fa3039f49383985651aba13ef994ec5520901a16ef49b58d799d2c99b2a0d71d2200dddab4e57\
        6cab5412dc1096c9b90779199bf829045437eb8ca44b5f3d5bdc64f5713953b159db98a52d1aea16de9612d1e4573622\
        56c8592e7d43ae1c40670bfae606423ada5bad464d00db3a358a4ac47b5d2e182b1f3e4884f2895fb8fc808434b43e09\
        80f268606449d394dfff88653474fe777f7a9a99a13ec0d748e6c1d0267ef1b2b133d300248e5703ed8fa5b2d2939675\
        1941294d02fa1f19a858a758604c8f2ec8a00dc1279f6b866c4967e11388572332446a4c8d30c230e52bcfc948beab5d\
        5cb9e07a045317ff8f62dc00\
    ";

    const ALONE_TEXT_P6: &str = "\
        5d00008000ffffffffffffffff00309c888a1c4180a92a80232dcc873c3de5929256571632f53e2c52993b5ad9792174\
        2d5e76e1f74ff984449cef78432628d789da170f07b5e8f622c9c3f9c8802402d9c9e2138b4e8ddd9267bc1c30ba5b84\
        1a62c0a4b4d59096698fa3039f49383985651aba13ef994ec5520901a16ef49b58d799d2c99b2a0d71d2200dddab4e57\
        6cab5412dc1096c9b90779199bf829045437eb8ca44b5f3d5bdc64f5713953b159db98a52d1aea16de9612d1e4573622\
        56c8592e7d43ae1c40670bfae606423ada5bad464d00db3a358a4ac47b5d2e182b1f3e4884f2895fb8fc808434b43e09\
        80f268606449d394dfff88653474fe777f7a9a99a13ec0d748e6c1d0267ef1b2b133d300248e5703ed8fa5b2d2938913\
        b6a614ab60e7d97856a1a2facd695cabb52c1c9f6c41d66639d036f8a658dd9cac7b0cc0233cba1140a1a283242fb094\
        d7eb87eec3c33fffbd80eb04\
    ";

    const ALONE_TEXT_P7: &str = "\
        5d00000001ffffffffffffffff00309c888a1c4180a92a80232dcc873c3de5929256571632f53e2c52993b5ad9792174\
        2d5e76e1f74ff984449cef78432628d789da170f07b5e8f622c9c3f9c8802402d9c9e2138b4e8ddd9267bc1c30ba5b84\
        1a62c0a4b4d59096698fa3039f49383985651aba13ef994ec5520901a16ef49b58d799d2c99b2a0d71d2200dddab4e57\
        6cab5412dc1096c9b90779199bf829045437eb8ca44b5f3d5bdc64f5713953b159db98a52d1aea16de9612d1e4573622\
        56c8592e7d43ae1c40670bfae606423ada5bad464d00db3a358a4ac47b5d2e182b1f3e4884f2895fb8fc808434b43e09\
        80f268606449d394dfff88653474fe777f7a9a99a13ec0d748e6c1d0267ef1b2b133d300248e5703ed8fa5b2d2938913\
        b6a614ab60e7d97856a1a2facd695cabb52c1c9f6c41d66639d036f8a658dd9cac7b0cc0233cba1140a1a283242fb094\
        d7eb87eec3c33fffbd80eb04\
    ";

    const ALONE_TEXT_P8: &str = "\
        5d00000002ffffffffffffffff00309c888a1c4180a92a80232dcc873c3de5929256571632f53e2c52993b5ad9792174\
        2d5e76e1f74ff984449cef78432628d789da170f07b5e8f622c9c3f9c8802402d9c9e2138b4e8ddd9267bc1c30ba5b84\
        1a62c0a4b4d59096698fa3039f49383985651aba13ef994ec5520901a16ef49b58d799d2c99b2a0d71d2200dddab4e57\
        6cab5412dc1096c9b90779199bf829045437eb8ca44b5f3d5bdc64f5713953b159db98a52d1aea16de9612d1e4573622\
        56c8592e7d43ae1c40670bfae606423ada5bad464d00db3a358a4ac47b5d2e182b1f3e4884f2895fb8fc808434b43e09\
        80f268606449d394dfff88653474fe777f7a9a99a13ec0d748e6c1d0267ef1b2b133d300248e5703ed8fa5b2d2938913\
        b6a614ab60e7d97856a1a2facd695cabb52c1c9f6c41d66639d036f8a658dd9cac7b0cc0233cba1140a1a283242fb094\
        d7eb87eec3c33fffbd80eb04\
    ";

    const ALONE_TEXT_P9: &str = "\
        5d00000004ffffffffffffffff00309c888a1c4180a92a80232dcc873c3de5929256571632f53e2c52993b5ad9792174\
        2d5e76e1f74ff984449cef78432628d789da170f07b5e8f622c9c3f9c8802402d9c9e2138b4e8ddd9267bc1c30ba5b84\
        1a62c0a4b4d59096698fa3039f49383985651aba13ef994ec5520901a16ef49b58d799d2c99b2a0d71d2200dddab4e57\
        6cab5412dc1096c9b90779199bf829045437eb8ca44b5f3d5bdc64f5713953b159db98a52d1aea16de9612d1e4573622\
        56c8592e7d43ae1c40670bfae606423ada5bad464d00db3a358a4ac47b5d2e182b1f3e4884f2895fb8fc808434b43e09\
        80f268606449d394dfff88653474fe777f7a9a99a13ec0d748e6c1d0267ef1b2b133d300248e5703ed8fa5b2d2938913\
        b6a614ab60e7d97856a1a2facd695cabb52c1c9f6c41d66639d036f8a658dd9cac7b0cc0233cba1140a1a283242fb094\
        d7eb87eec3c33fffbd80eb04\
    ";

    const ALONE_TEXT_LC0_LP0_PB0: &str = "\
        0000100000ffffffffffffffff00309e3d16b50a074e985ba5bb66c4e0fd6a94bc899ae1309d2866939701487579573a\
        4439a666ece05425839ab1c7fedb6a00e79a840f8da8565d0eebfef05fe5c003e2b76bb54b5268e18a0474133ee5e0b0\
        2d04e4a71ded95ffc9e8162c3136877f16e099ea54af8ef2eb90d137ccde57a5a7b7ee8e79c8faa474664555dd64b4fb\
        87d61882bc92248f946f50713521cc28a13e40fec4e97671f4d2aac28feb1bd28f2182176ff503a63d2928c88e7dc445\
        7051136c2c2d40b3aa4e4d104644f71461138c2369bf1e06bd238b676dca6cc84f6e45e9126ef3134b9b70dc559b568a\
        c0ae05781e9d6902717f7ef084a888751421ccf73620f612924536b0dd97b42f1d3d7876abf9af629bf6c7e2617140a3\
        c5638b7dffaf7d0f8927f5aadffb8ff00691fe089da674e629bb4079a28ec83942e08dc3e1ffffcf593c3c\
    ";

    const ALONE_TEXT_LC3_LP0_PB2: &str = "\
        5d00001000ffffffffffffffff00309c888a1c4180a92a80232dcc873c3de5929256571632f53e2c52993b5ad9792174\
        2d5e76e1f74ff984449cef78432628d789da170f07b5e8f622c9c3f9c8802402d9c9e2138b4e8ddd9267bc1c30ba5b84\
        1a62c0a4b4d59096698fa3039f49383985651aba13ef994ec5520901a16ef49b58d799d2c99b2a0d71d2200dddab4e57\
        6cab5412dc1096c9b90779199bf829045437eb8ca44b5f3d5bdc64f5713953b159db98a52d1aea16de9612d1e4573622\
        56c8592e7d43ae1c40670bfae606423ada5bad464d00db3a358a4ac47b5d2e182b1f3e4884f2895fb8fc808434b43e09\
        80f268606449d394dfff88653474fe777f7a9a99a13ec0d748e6c1d0267ef1b2b133d300248e5703ed8fa5b2d2938913\
        b6a614ab60e7d97856a1a2facd695cabb52c1c9f6c41d66639d036f8a658dd9cac7b0cc0233cba1140a1a283242fb094\
        d7eb87eec3c33fffbd80eb04\
    ";

    const ALONE_TEXT_LC1_LP3_PB1: &str = "\
        4900100000ffffffffffffffff00309c88c9ed567aa4db249db9216b4a987c6f031bbf96ec0739138ea429731b93770a\
        b4ba8d2ce41272b15467eeb6d29a66870c560858c93d60568cc471155de520484d88f9ed35041a1ffdd31f6749fe305c\
        3faebf4e29dbc59ab2aec749dd45c150abb30ad647e863b199ea7f5c6fe1b2fb300cd625f81d7ee89aa7b7caaba9adeb\
        895ff6877973065aa344d30e356edb61579663488eb34bd50fd6dd1f5a64e3fd2122b570d0f8b2288c55892f3ad622ed\
        da303fbc64e9cf722729ef91335c02a2e6991b4582eae147183025b238212cb945ca2df38c7b01f2a33b6cd2181acf45\
        96ede5fdc20753f3179f12d5c1b0c365037fc303a19cb2735419977612f4c261968125b59e76771083ab255a787949b6\
        32a984e3c46a98135322c4869774c36039beb0980cf67538c6f67769700f0ff99acfc37c762ecd6e163e9adbcd50190a\
        8032065da4770b5af1dffc1819ad\
    ";

    const ALONE_TEXT_LC4_LP0_PB0: &str = "\
        0400000100ffffffffffffffff00309d6d93df7948139fb58874e29c1f50ac56191661a00ac2e0d49e97c1e55af99f0f\
        be3a6efad47a936ed0953f4e9244c8eaddfe01c5e0bcf8c173efa432d6db64dc842fb4ef4bdffc94d943c249d810193b\
        27a44e1820f3fb6dc536a13cf3fb341286117c19005094679249579189602ab362b17011e95343f9b91d06317ba91db4\
        8a37ba513e5ec40bcf39fd36419228f59206c5a80a4d492e8b556471cf6a2a28cb9fa5b23c75195afa573d40f50930cc\
        f36685183cd99b5185b8c9e3bb73b6a768f693977e528eace22f33fe02355c562b6b8beee44d3acee7c8579ae2ba8ca7\
        98dbce8565c9db5afb6b948ad7fc51060ff994e9668e888d8f97633be00c64e937a8b69093f20256555f72eaf05228f7\
        627221040594007371116969c0379bd3f48d383a4a78c8f5ee69fa93e5b786bf6dfce024c55e7019d15a81dbebcdd89f\
        ff5422af00\
    ";

    const ALONE_TEXT_LC0_LP4_PB3: &str = "\
        ab00000001ffffffffffffffff00309c88668349d8ca0a3433f76e23a06d975f6b8a740758c0d56cde3a33bfa3b296c5\
        f46a8098418319b67017497aef061362e2c3cabd5ff85fa7ee199e8e3632d4ce199af446f951a4431db9d1c67b6475f9\
        297d4bf94bcd0ceedcfbf489a680730c72538b704c13f3794598da20dfddf9c34c9e6c8d3a640e3cd106bc91fd2ed772\
        8dc11246287d7dd62cddf8f335e82394d276f7deb3263d09ace465cdbe980638c2631eb0c3fecee49e39b34d0f987eb0\
        75ca72ac95bb4532a342b5bfc756bef5c1c206f0cfbafc20d473241f61111a0dc36f2ebf3bad3fd8b9a98e83847e38f3\
        463bd692d8af2832cd048ab66db8a3a9972f1cc2055c5370448deb3c5273ea32f648ed8eff0e270af8b7d0ce18881ef2\
        7273f11e76be87289c940942a0902768adffde230344edcc152db890e88a933cc9c003096e16126b108b27ad56535379\
        7a61ee94aba5887c9f90121ae0c5aae91d35e4821a4cdff5fff16464e5\
    ";

    const ALONE_QUAD: &str = "\
        5d00008000ffffffffffffffff000067fea04eafef9d48aad50c105cf43e5d8f88a89ff22acfdb69c8c27fa9ae482b8c\
        c01265167cc5ea595329af4d93570c765fc9a103c295d21c21d3ac734f16c662b86b3c6cb013795393f711d25d348194\
        8a43c69b953dff4ec28be8c33f553ec8063b7fc5c50080dc83664a29f00868a569c31e6ab2a3aa862ea12936492005c1\
        fc0c8c5627f22738d5a2a6c0043204c0c4a336209e0d4f86eaee1438df53e93d2159b95218f21235e3b9abdeb6e1753b\
        d0833beb67072dfa873fa332c0eef3e082a5fcb0b4e9504eac27917baea29f9338ca7936aeac9da686c1b3859d563385\
        da790c3ba6ff64f22e31b6c3777106a16e332ec9211514f8998bd86503b64030ff17baaefde8074546685fb6a3fa85d1\
        979b3fdef98fa6fdd9eb765c05f0b3630faf48a31e8154da3a2b1f7307fe8d73b0b03c75c33a50cdbb652ba6c2a24a24\
        79c0746d699fb68d756e7a3bae02c3b23ba6f7468de26d4cd856ebd75abfd41ea9e95e6456eeed449fa2e9cbda8ddbc2\
        a091a4a666201c43043042c3559534462306ca2bdacb98535296f8e9a33692777751f4f6de1b57adf704658461208d73\
        9249a91ae67e5023ae47b226a42514de8aa78053a5eadd87dcef1d0250182f86b90597017910ab1cd8707add27dcc16c\
        7e5266dc1283c5416c6139dfbbc9be9b93cfcaec53628f8d6c33ff7cca2701050f01e59a5dcb1e010ef64df70e217907\
        1bf2e887ef87e1f6ae2cdfdba20115b7b8749017e8eea9e846d254a539d59513085646bd0b48b54f8216699ca487bb1b\
        070fe9d402b0e27c0b58552b7a4197668aec2c0a3b90d4b2813bf2bc4ea928b63c11066a62d2966f832e00448412438e\
        bb64f8f0e133a4dd9c2e9f869b5c639b13294526a56e6a447765abd47c19171f47d31f8f9651c0879f9780cf08595016\
        93ca304eae64ee5dc45349f3c7872361e04321a617dc57435cf1795b84f7c42ccf413d27c0ca32dea9cfefced0d93d42\
        2b202c064c1b07f195df44a387ddb642736f51d26276ebab183ae67bff3949a2b2016319b56bcd9007a54618bedab3fd\
        337d7d4ff3ca3cc9dec3fddc95bb377a2cdd79994726c3af3ad40b5927958ee5ff567218255f930286f96ad92a1265f5\
        e3356908ae206943b88476089686aca243f0011b892a1ce195773589b079d92ff3276a584223bd312a432f84fbda4060\
        b7effafd63033fd32c05c7e9c7db3b4a5bbbea12db1fab7908aaff43bb4e34f4cd4c4c58f83821b4eb3bb8d70a5c0d9a\
        47206ba78ba9f9c16f217af8545d3dd435f7e829d85f7289dcd02a05d751e2e8625d3accc30ab976ed37d95489789d99\
        5805768182dc436c30367446becf5f8ef268f31085a4ec7298deef7ebf1d2af07843d644e79b30e2d59c7868fda66262\
        5cfa1bce4c4d94cce18ba9e91d79e3190257d8884770b7836c979c1d6bae3994031536b2bab0b7cf409c1e8c35cba3ad\
        ea958c9e38bef0b3b88e23bdc2394c07cbe56ad12f15606058ba224c7bb9ae50baaad6aca0673f3828e943c863bd8043\
        97bc1c80a5c63e94286675ceb1e72623a8f8068bd9f57fd60d1b3f7d77ab8314216ed1d57af33e14536276ac59e4e55b\
        0508f9c7daadfcfb522b74cd1e5b2042f9dd533df82964093b80cb2a6cdfb53bf0c4bd2e5faa0f3e4b664290130eff10\
        93f8717859f80bcdff9528460fa9fc7cdefb9a302e56c08f85f38381c065c42553f8f591363105a5b0ee6fc1704d470c\
        d19111aaad601dbaceb127185c5986e9665258bee976ac59e4e55b0508f9c7daadfcfb522b74cd1e5b2042f9dd533df8\
        2964093b80cb2a6cdfb53bf0c4bd2e5faa0f3e4b664290130eff1093f8717859f80bcdff9528460fa9fc7cdefb9a302e\
        56c08f85f38381c065c42553f8f591363105a5b0ee6fc1704d470cd19111aaad601dbaceb127185c5986e9665258bee9\
        76ac59e4e55b0508f9c7daadfcfb522b74cd1e5b2042f9dd533df82964093b80cb2a6cdfb53bf0c4bd2e5faa0f3e4b66\
        4290130eff1093f8717859f80bcdff9528460fa9fc7cdefb9a302e56c08f85f38381c065c42553f8f591363105a5b0ee\
        6fc1704d470cd19111aaad601dbaceb127185c5986e9665258bee976ac59e4e55b036c88e7a8869ffc3b6ae0\
    ";

    const RAW1_TEXT_LC2_LP1_PB3: &str = "\
        00309c88669a0a8393536fb9243fd3791ca24f49bac8f829a70c02abe03763d2aeb7720501e77e50c7a233ed4fb88043\
        ad901057ee47855da8f0b9274ae85e3185a474738a725da9e4aadf84f222861aa3602476b019b5c0a397ea03a2e33c9c\
        a0f48118dc1e00a016a409bd9d514b907394290745f145520ad957ab7643a3d47d6611185e60a8279b0844201a871bc9\
        b7898003363d5654d99add3002f40fff7d24ef93023d83682bb7f8fa64c8999174f99fc635d365f99a2563b10f53294a\
        3a34c5c6ba028efbecbe7ef7bca9108f01f68b4fbd3cf0fb66fc490da9b13742b69c5299fd152407c794868c243c27ab\
        0c7c984e9c59d0efd3b15f63c41621bcef65f2db4a2b8d30055ffbc8c71aa7c75ac6fcf819ee5fa3bceaa296d5c30531\
        6692916b7b7e4418e2a9d276d033ba59a0e6930f4df08be6e7c7093d23ea7c89eefcd088bd98aac3e1c8a519bdf739f1\
        ffbbb8bbf1\
    ";

    const ZIP14_844: &str = "\
        1a010500e00010000000309c88668349d8ca0a329e0e865371d014723221c701352d46704ff075dec1ef20229851cb0c\
        043cdd7d58fa3b5e3fed3a51b61dbb5724b61315d607007a92e5fba13836f29eaa3023a84f33a0f8ec63fe0e0091e06b\
        204832361e9362e6b2a60e85e7d1fd61a7f14800853e463803efee71ab7e8c244bf700efb4b04bd189e5e1be18a46b7b\
        390218a4c43ac1e4a38109d5c986802e595c20bd86edda7e918ab0a338e9ba5d4600990d106f42820555e2babd516d58\
        2fb4f018e171566b1fdc5c227e75aeb0b8c73903750f678a5f2f168b2bba28da550f6d06e005b45ae7b8c8a3dacc0c3f\
        8316cf043bb031614c398f15be09134c44a6ef18887ce0c7e1401d32fe5a4c83ec7257b781648d5044e3a24210b4ec77\
        11639bc2f01aae62f6901e09295278aadb86acb0d08e78c953b4d09a169e9d481b602b758cc0d4f508c981c22f5005ce\
        88e4934667c4350dd6b798a7742745fa9933bcdb842f57ffdb10b12b\
    ";

    const ZIP14_800: &str = "\
        1a010500080010000000309d6d93c4395b4a4e0d349d220f6330cb1893e5a025bf6bdca384416b9ffc756d2027eab40e\
        aec3a867c836119120b31b7e1b7b20e0b9ae5353051e03f57f02364c6fe0b383700e2f4507db06ee6fe8047426c95d7b\
        aecd410f63f1162334f3641385b9edae315ab208acc950e4cafa670408aa3bc7120e81b556c9a06665be46cc286ca7be\
        6e741b48d9f64527fee80fe714c916b0f8d8d11bcac0113f64a2fb538cafe464fcbf5eee8519422e168a485f670e5744\
        e99e22a0ed270ec2675927a766ac05b9ff4b16c5185113bd6b5fe6fbb9f6964f721a285c670d7c751e4cda00e3624712\
        e051b49e43d42123038d2a8e7644dec3c02412879d055849ff761ed12713c89a15c19b6eba125c75a4aa357f7affe253\
        3d090503dd9d421348b9ad1172900518957728e650135366e54c6d6cb705c484b33622d06d0359cba74f6fc5c7c3f01f\
        7d0defae8fffc5d06640\
    ";

    const ZIP14_531: &str = "\
        1a0105004d0010000000309c88c9ed567aa4db231d726e9ac8245392eb1c62ca2454f5a1dbeb69be8249a66a0ed3909e\
        0cb833788d2ec0e669fce1faf2f5b53c789989e380b5e74be8093bfb6c05d33cb756da77b67a18e4c70ada77beaefb3a\
        b8e7e1c813ecb907437747befa0babe8fba8b8bbd1c7256ff12e9e3b3fffb3b2c5b85a0af91b60b75873b312c4a56c10\
        ac6a74e6adbd81391081e61def57459604e275c75937c3817a73de0ef84e3e5e632c9385e4c7323433f0047f22f18e44\
        8022f838ca506d4c007e30d8fdef9dccdd4e30318d12653f68c10080c7d25498c0c2c2e02cc53e511278a87e6c35ea79\
        13b58e1cfd1d9f6b67a90eae6fc2ee6214f027f950455e6cc0391b6c365cac30c5f0b96562ffd3199e713b26d4f6789c\
        c92325661229564fb64075c1c7080f274e6cc7102e7809dec8d07893b467cb62afbfc5a90604f7fb56025dadc11a3c18\
        11780525648c00fa37348d\
    ";

    const ZIP14_NOEOS: &str = "\
        1a0105005d0010000000309c888a1c4180a92a80232dcc873c3de5929256571632f53e2c52993b5ad97921742d5e76e1\
        f74ff984449cef78432628d789da170f07b5e8f622c9c3f9c8802402d9c9e2138b4e8ddd9267bc1c30ba5b841a62c0a4\
        b4d59096698fa3039f49383985651aba13ef994ec5520901a16ef49b58d799d2c99b2a0d71d2200dddab4e576cab5412\
        dc1096c9b90779199bf829045437eb8ca44b5f3d5bdc64f5713953b159db98a52d1aea16de9612d1e457362256c8592e\
        7d43ae1c40670bfae606423ada5bad464d00db3a358a4ac47b5d2e182b1f3e4884f2895fb8fc808434b43e0980f26860\
        6449d394dfff88653474fe777f7a9a99a13ec0d748e6c1d0267ef1b2b133d300248e5703ed8fa5b2d29396751941294d\
        02fa1f19a858a758604cd8f6ec4171a3d4b564695845c001d584dbcf5b02ae1fd71df43ab88d5520a6764d19086dd1da\
        98\
    ";

    const RAW2_EMPTY: &str = "\
        00\
    ";

    const RAW2_ONE: &str = "\
        0100005a00\
    ";

    const RAW2_RUNS: &str = "\
        e0332c003e5d00006ffdffffa3b7ff473e481572396151b89228e6a38607f9eecdd08c49dcbc5af8a6ef8709ef4223a9\
        dd4e916552306e190b8034d6dbf6a9b00af702410000\
    ";

    const RAW2_TEXT_P0: &str = "\
        e004af01555d00309c888a1c4180a92a80232dcc873c3de5929256571632f53e2c52993b5ad97921742d5e4e0ed08775\
        6af9738b45569c632d2e0de8e3eb6f455c32a6ce44042573c72a5f2b608c3bf9a31b67f58ef839e962b9c5ce0aabe2ed\
        39a7a07a39314db706a1c87853c021d34543b33c9fe7ad2ff6eb89d840b379e57017c4da3ae62ba04caf0eaf9b03759d\
        23bf7d3dfe7c6f6bb7f393721f828e0bcec2a519420686e37c16c6c62b9f9d5cd8e1c9a73b9b517c5b197c67b4731c5c\
        326bfc3d40928b0a4e17d45481edddd6b3092c95b3b6a2bed1bc3f991ce0b38f74408599a09d208d9125df8d2760fb14\
        313e0ed641330876c8fde44a86c4f10bd7f3a985f13c38d793520501acd3023ae9f334db892c22a2f0c0a47e9f732dac\
        f478adb239e86b853dcccce1dc2bd37306d1c740c8966f455a45fac58a5592c5a84ea3dfa359ad368bb6b171bbfe1b4e\
        d5ac948991a1e7382bfe5c0900\
    ";

    const RAW2_TEXT_P1: &str = "\
        e004af01535d00309c888a1c4180a92a80232dcc873c3de5929256571632f53e2c52993b5ad97921742d5e4e0ed08775\
        6af9738b45569c632d2e0de8e3eb6f455c32a6ce44042573c72a5f2b608c3bf9a31b67f58ef839e962b9c5ce0aabe2ed\
        39a7a07a39314db706a1c87853c021d34543b33c9fe7ad2ff6eb89d840b379e57017c4da3ae62ba04caf0eaf9b03759d\
        23bf7d3dfe7c6f6bb7f393721f828e0bcec2a519420686e37c16c6c62b9f9d5cd8e1c9a73b9b517c5b197c67b4731c5c\
        326bfc3d40928b0a4e17d45481edddd6b3092c95b3b6a2bed1bc3f991ce0b38f74408599c8b5ce092db0ba5b4806dfa0\
        69c4d1ed135dacea91f5135d604c47ca72fd25f288c1c497f35138a1fad05b9c3fa220dbbeb85b4ececf5fa82bde5095\
        eb02a45f629a30c57ce7e34842b4eceb89afa65a61c8a18baa04782df10e583b9c0953590022f6209cac045b2958ba45\
        681177cdcda97a0dd80000\
    ";

    const RAW2_TEXT_P2: &str = "\
        e004af01525d00309c888a1c4180a92a80232dcc873c3de5929256571632f53e2c52993b5ad97921742d5e4e0ed08775\
        6af9738b45569c632d2e0de8e3eb6f455c32a6ce44042573c72a5f2b608c3bf9a31b67f58ef839e962b9c5ce0aabe2ed\
        39a7a07a39314db706a1c87853c021d34543b33c9fe7ad2ff6eb89d840b379e57017c4da3ae62ba04caf0eaf9b03759d\
        23bf7d3dfe7c6f6bb7f393721f828e0bcec2a519420686e37c16c6c62b9f9d5cd8e1c9a73b9b517c5b197c67b4731c5c\
        326bfc3d40928b0a4e17d45481edddd6b3092c95b3b6a2bed1bc3f991ce0b38f74408599c8b5ce092db0ba5b4806dfa0\
        69c4d1ed135dacea91f5135d604c47ca72fd25f288c1c497f35138a1fad05b9c3fa220dbbeb85b4ececf5fd87b96901a\
        5a5c748dcc0e98f2c0d809c39a4337c8b04251009ad43ec254591d30f2cef7c92436872f38632b54d37f6ae19b8d5ff2\
        42f1c560719a102c6500\
    ";

    const RAW2_TEXT_P3: &str = "\
        e004af01525d00309c888a1c4180a92a80232dcc873c3de5929256571632f53e2c52993b5ad97921742d5e4e0ed08775\
        6af9738b45569c632d2e0de8e3eb6f455c32a6ce44042573c72a5f2b608c3bf9a31b67f58ef839e962b9c5ce0aabe2ed\
        39a7a07a39314db706a1c87853c021d34543b33c9fe7ad2ff6eb89d840b379e57017c4da3ae62ba04caf0eaf9b03759d\
        23bf7d3dfe7c6f6bb7f393721f828e0bcec2a519420686e37c16c6c62b9f9d5cd8e1c9a73b9b517c5b197c67b4731c5c\
        326bfc3d40928b0a4e17d45481edddd6b3092c95b3b6a2bed1bc3f991ce0b38f74408599c8b5ce092db0ba5b4806dfa0\
        69c4d1ed135dacea91f5135d604c47ca72fd25f288c1c497f35138a1fad05b9c3fa220dbbeb85b4ececf5fd87b96901a\
        5a5c748dcc0e98f2c0d809c39a4337c8b04251009ad43ec254591d30f2cef7c92436872f38632b54d37f6ae19b8d5ff2\
        42f1c560719a102c6500\
    ";

    const RAW2_TEXT_P4: &str = "\
        e004af01485d00309c888a1c4180a92a80232dcc873c3de5929256571632f53e2c52993b5ad97921742d5e76e1f74ff9\
        84449cef78432628d789da170f07b5e8f622c9c3f9c8802402d9c9e2138b4e8ddd9267bc1c30ba5b841a62c0a4b4d590\
        96698fa3039f49383985651aba13ef994ec5520901a16ef49b58d799d2c99b2a0d71d2200dddab4e576cab5412dc1096\
        c9b90779199bf829045429e035f03aee696a020fb303bcba99ba452584937adca6beccc72481881fced565eead30e54c\
        7ce38d089fc9fd7ef6c9e7b8ade9484890b2dd8bd70a5d1fa03a93e8092e7e399aa08eb4b59e9b01f8201ddeed31cba6\
        c1065873911e87e2dee535d1a19c217d1173497998718373e2640f498911e4ba483132a3c184add8583a6d85a2964688\
        82ba2d0b80119c9954667107bc6bda4040d4521c47bdeed19f293b7c43605e4b3d2033b1655853a920907244fcac0000\
    ";

    const RAW2_TEXT_P5: &str = "\
        e004af01495d00309c888a1c4180a92a80232dcc873c3de5929256571632f53e2c52993b5ad97921742d5e76e1f74ff9\
        84449cef78432628d789da170f07b5e8f622c9c3f9c8802402d9c9e2138b4e8ddd9267bc1c30ba5b841a62c0a4b4d590\
        96698fa3039f49383985651aba13ef994ec5520901a16ef49b58d799d2c99b2a0d71d2200dddab4e576cab5412dc1096\
        c9b90779199bf829045437eb8ca44b5f3d5bdc64f5713953b159db98a52d1aea16de9612d1e457362256c8592e7d43ae\
        1c40670bfae606423ada5bad464d00db3a358a4ac47b5d2e182b1f3e4884f2895fb8fc808434b43e0980f268606449d3\
        94dfff88653474fe777f7a9a99a13ec0d748e6c1d0267ef1b2b133d300248e5703ed8fa5b2d29396751941294d02fa1f\
        19a858a758604c8f2ec8a00dc1279f6b866c4967e11388572332446a4c8d30c230e52bcfc948beab5d5cb9e029c8fe00\
        00\
    ";

    const RAW2_TEXT_P6: &str = "\
        e004af01495d00309c888a1c4180a92a80232dcc873c3de5929256571632f53e2c52993b5ad97921742d5e76e1f74ff9\
        84449cef78432628d789da170f07b5e8f622c9c3f9c8802402d9c9e2138b4e8ddd9267bc1c30ba5b841a62c0a4b4d590\
        96698fa3039f49383985651aba13ef994ec5520901a16ef49b58d799d2c99b2a0d71d2200dddab4e576cab5412dc1096\
        c9b90779199bf829045437eb8ca44b5f3d5bdc64f5713953b159db98a52d1aea16de9612d1e457362256c8592e7d43ae\
        1c40670bfae606423ada5bad464d00db3a358a4ac47b5d2e182b1f3e4884f2895fb8fc808434b43e0980f268606449d3\
        94dfff88653474fe777f7a9a99a13ec0d748e6c1d0267ef1b2b133d300248e5703ed8fa5b2d2938913b6a614ab60e7d9\
        7856a1a2facd695cabb52c1c9f6c41d66639d036f8a658dd9cac7b0cc0233cba1140a1a283242fb094d7eb87c28a9f00\
        00\
    ";

    const RAW2_TEXT_P7: &str = "\
        e004af01495d00309c888a1c4180a92a80232dcc873c3de5929256571632f53e2c52993b5ad97921742d5e76e1f74ff9\
        84449cef78432628d789da170f07b5e8f622c9c3f9c8802402d9c9e2138b4e8ddd9267bc1c30ba5b841a62c0a4b4d590\
        96698fa3039f49383985651aba13ef994ec5520901a16ef49b58d799d2c99b2a0d71d2200dddab4e576cab5412dc1096\
        c9b90779199bf829045437eb8ca44b5f3d5bdc64f5713953b159db98a52d1aea16de9612d1e457362256c8592e7d43ae\
        1c40670bfae606423ada5bad464d00db3a358a4ac47b5d2e182b1f3e4884f2895fb8fc808434b43e0980f268606449d3\
        94dfff88653474fe777f7a9a99a13ec0d748e6c1d0267ef1b2b133d300248e5703ed8fa5b2d2938913b6a614ab60e7d9\
        7856a1a2facd695cabb52c1c9f6c41d66639d036f8a658dd9cac7b0cc0233cba1140a1a283242fb094d7eb87c28a9f00\
        00\
    ";

    const RAW2_TEXT_P8: &str = "\
        e004af01495d00309c888a1c4180a92a80232dcc873c3de5929256571632f53e2c52993b5ad97921742d5e76e1f74ff9\
        84449cef78432628d789da170f07b5e8f622c9c3f9c8802402d9c9e2138b4e8ddd9267bc1c30ba5b841a62c0a4b4d590\
        96698fa3039f49383985651aba13ef994ec5520901a16ef49b58d799d2c99b2a0d71d2200dddab4e576cab5412dc1096\
        c9b90779199bf829045437eb8ca44b5f3d5bdc64f5713953b159db98a52d1aea16de9612d1e457362256c8592e7d43ae\
        1c40670bfae606423ada5bad464d00db3a358a4ac47b5d2e182b1f3e4884f2895fb8fc808434b43e0980f268606449d3\
        94dfff88653474fe777f7a9a99a13ec0d748e6c1d0267ef1b2b133d300248e5703ed8fa5b2d2938913b6a614ab60e7d9\
        7856a1a2facd695cabb52c1c9f6c41d66639d036f8a658dd9cac7b0cc0233cba1140a1a283242fb094d7eb87c28a9f00\
        00\
    ";

    const RAW2_TEXT_P9: &str = "\
        e004af01495d00309c888a1c4180a92a80232dcc873c3de5929256571632f53e2c52993b5ad97921742d5e76e1f74ff9\
        84449cef78432628d789da170f07b5e8f622c9c3f9c8802402d9c9e2138b4e8ddd9267bc1c30ba5b841a62c0a4b4d590\
        96698fa3039f49383985651aba13ef994ec5520901a16ef49b58d799d2c99b2a0d71d2200dddab4e576cab5412dc1096\
        c9b90779199bf829045437eb8ca44b5f3d5bdc64f5713953b159db98a52d1aea16de9612d1e457362256c8592e7d43ae\
        1c40670bfae606423ada5bad464d00db3a358a4ac47b5d2e182b1f3e4884f2895fb8fc808434b43e0980f268606449d3\
        94dfff88653474fe777f7a9a99a13ec0d748e6c1d0267ef1b2b133d300248e5703ed8fa5b2d2938913b6a614ab60e7d9\
        7856a1a2facd695cabb52c1c9f6c41d66639d036f8a658dd9cac7b0cc0233cba1140a1a283242fb094d7eb87c28a9f00\
        00\
    ";

    const RAW2_TEXT_LC0_LP0_PB0: &str = "\
        e004af01370000309e3d16b50a074e985ba5bb66c4e0fd6a94bc899ae1309d2866939701487579573a4439a666ece054\
        25839ab1c7fedb6a00e79a840f8da8565d0eebfef05fe5c003e2b76bb54b5268e18a0474133ee5e0b02d04e4a71ded95\
        ffc9e8162c3136877f16e099ea54af8ef2eb90d137ccde57a5a7b7ee8e79c8faa474664555dd64b4fb87d61882bc9224\
        8f946f50713521cc28a13e40fec4e97671f4d2aac28feb1bd28f2182176ff503a63d2928c88e7dc4457051136c2c2d40\
        b3aa4e4d104644f71461138c2369bf1e06bd238b676dca6cc84f6e45e9126ef3134b9b70dc559b568ac0ae05781e9d69\
        02717f7ef084a888751421ccf73620f612924536b0dd97b42f1d3d7876abf9af629bf6c7e2617140a3c5638b7dffaf7d\
        0f8927f5aadffb8ff00691fe089da674e629bb4079a28ec83942e076f67a00\
    ";

    const RAW2_TEXT_LC4_LP0_PB4: &str = "\
        e004af0155b800309c88669a1299eee31a40f1b964fa8f3900a39b078f470dc41f804a450299360e102b258bef261628\
        47b56a6cea81a1977f2635d894a87169f7d17cf6e50afaee661b7e328f5d35e4bedbe6ce4c3ed2828e6e3ad05a44e72a\
        278865ba61361306a85d5012ce06420a918c7552cd5e478cd676cf70ef9d1102b35eb0023ea17b87f03f531d9627b7f5\
        a32a05444e30478017d498e7525e39836333df29762bd446128c488139d2548c4774435e7a01f07ceb04871832395133\
        c9ddcdaecaba14fb63ca4b5e52c511489b48507f1e4f7c9d7795d17d8919c65fdf34be1c40be5dfc4de81a852bc8654c\
        d27662425b553157783002311656cf492436387b47ae11ecb114e7e0a05349782556c77501caba8fb39da973a7c8c90f\
        d91e84f6f5c9ecadb9706a45111c0d785ef2ffb437545b3a0d8a46495da644738772c2389968fb2ce82dd724e8c5e997\
        ff15fd3040ee8c4d6fa4ee6200\
    ";

    const RAW2_TEXT_LC0_LP4_PB2: &str = "\
        e004af01577e00309c8866836436d3930bc57e3641c251d5ad6aaf99a23881c0ff435bc4aa0c133596472927e79aeaa2\
        93aeacd5c605a76c44f4f4a891aff8ccf83b10f9781e3b8efcdcba9a2a7646fd20d0b1e95033ae207af569f054f60bef\
        c0cce61494b5a92cde0962b5685b52a614025f64b3a0c98dc6f377cf88b83b8b11d7396552f1047a88502660ebd0bd58\
        6a741de4c8b14747d33493e33767353186ad2246e71a955a2213bf511da91d9968946ebb1ce47d4921d4e0c8f5e1060d\
        199f50518ebd8833806cae5330a1c3aef82727a80f7d307e8e6ced87031a36a2a56d5ee5e41f76661dd2b35e594c2517\
        a49fb444a54477560b1d2bc8505eec2103d97e411fae3a001a46d876617f1bdc6447db61af3f3e6a5221317461aeef68\
        d1b3693a3c72168ce3f9d81d86cf880a877e1a9f2daaeb55fadc84e36bd5ae87017113cd0a46d40ea5e363b90676a37a\
        dce80eab71783862cda2f89fcd5000\
    ";

    const RAW2_TEXT_LC2_LP2_PB0: &str = "\
        e004af01461400309d6d93c4396dbc4be9d4a3b9e56ca97145289abe79f6b7678c0a8c9a1c1c2f1f0cd3d275ac8d4788\
        dfb09d65c0d45e5b664794aab0e5048a8f1704c8f2176a7744c5951fadcc5fd6c1df20aaec91aebed61899e911195410\
        0f9488750ebe4d59b74a20a46be3eb3346a59025d5ac821ed1a7518a9f7082e2b135cfc9c0c5b0f16a20398f4e9a93f6\
        425ceac88d0e0d48bee2b8a5db28f0406736f2a2125a701374f31ed15c25c2b909dc5f8b019449316d271861388676b7\
        68c95831c59167b048c1a2ae3fc1ec84a8e1a75df22372cc4cf01ac3ed91cdfdea3259a7263927469bec7f22b6ce514b\
        fc1833b88f2e77f90164bbebaf81f8340c61848a79b885d2794795a2a77d57c385105981cb731ecc566782223f909f15\
        ca66914be0a63c2f0869fc4d983b9e5f39d8b0973932414a7a442d59f2e8173c5e05725a4ef3c2f05cf59f7ace00\
    ";

    const RAW2_TEXT_LC1_LP3_PB3: &str = "\
        e004af0158a300309c88668349d8ca0a352cca9950f5e22d9cce124c8ab04ee1f3f8fdb01cf0d920c8ba7c312c554148\
        f0c68c40e6ac8fa4388747a22b793caa73dc5405aab01a516a9ae4f04d828a4963bf4def38232508f8bd05d327fb0da5\
        dd4aac3b6c6ed4ef6670cb76f39b3baa44398d633070bffc8df74869bf0dd0cf85d92cbc17a80ebd6d438102aac471f2\
        b4aa9e4d7db256f6669f8e2ed249b19a72ac68de3c4e81aed5822ab2bd1eade0690ee01af610e918ea12f8c4e0440d80\
        cf45f196d02a85350af4497dcbcd196a26d684b334816952421ad9a32e4886a54a7c4a01c345efe8ed8949a1b4820b39\
        82f85d4849635ecfa7e915cb4d15f3771a9285046da3ed81b2d6158cfe27003cdb9a04bf4f3c0b047cba9c99879d37f0\
        adcea1c591aa2b1026478ba76277af037c9fd2a2c14aa473c42c31abc03256189d5ffff46368be6b8c7cb37cd3a08211\
        20aa995a787d9438c43e7a5607e92f00\
    ";

    const RAW2_RANDOM: &str = "\
        01003f02364491e914f028495d5f6f1dac6cc201e075a927159ae4500ec1c4cc586042dda574885fb08daf7d3051626c\
        61066ff992ab22d3c06a4ab66886079ed99c9400\
    ";

    const RAW2_QUAD: &str = "\
        ffffdd05535d000067fea04eafef9d48aad50c105cf43e5d8f88a89ff22acfdb69c8c27fa9ae482b8cc01265167cc5ea\
        595329af4d93570c765fc9a103c295d21c21d3ac734f16c662b86b3c6cb013795393f711d25d3481948a43c69b953dff\
        4ec28be8c33f553ec8063b7fc5c50080dc83664a29f00868a569c31e6ab2a3aa862ea12936492005c1fc0c8c5627f227\
        38d5a2a6c0043204c0c4a336209e0d4f86eaee1438df53e93d2159b95218f21235e3b9abdeb6e1753bd0833beb67072d\
        fa873fa332c0eef3e082a5fcb0b4e9504eac27917baea29f9338ca7936aeac9da686c1b3859d563385da790c3ba6ff64\
        f22e31b6c3777106a16e332ec9211514f8998bd86503b64030ff17baaefde8074546685fb6a3fa85d1979b3fdef98fa6\
        fdd9eb765c05f0b3630faf48a31e8154da3a2b1f7307fe8d73b0b03c75c33a50cdbb652ba6c2a24a2479c0746d699fb6\
        8d756e7a3bae02c3b23ba6f7468de26d4cd856ebd75abfd41ea9e95e6456eeed449fa2e9cbda8ddbc2a091a4a666201c\
        43043042c3559534462306ca2bdacb98535296f8e9a33692777751f4f6de1b57adf704658461208d739249a91ae67e50\
        23ae47b226a42514de8aa78053a5eadd87dcef1d0250182f86b90597017910ab1cd8707add27dcc16c7e5266dc1283c5\
        416c6139dfbbc9be9b93cfcaec53628f8d6c33ff7cca2701050f01e59a5dcb1e010ef64df70e2179071bf2e887ef87e1\
        f6ae2cdfdba20115b7b8749017e8eea9e846d254a539d59513085646bd0b48b54f8216699ca487bb1b070fe9d402b0e2\
        7c0b58552b7a4197668aec2c0a3b90d4b2813bf2bc4ea928b63c11066a62d2966f832e00448412438ebb64f8f0e133a4\
        dd9c2e9f869b5c639b13294526a56e6a447765abd47c19171f47d31f8f9651c0879f9780cf0859501693ca304eae64ee\
        5dc45349f3c7872361e04321a617dc57435cf1795b84f7c42ccf413d27c0ca32dea9cfefced0d93d422b202c064c1b07\
        f195df44a387ddb642736f51d26276ebab183ae67bff3949a2b2016319b56bcd9007a54618bedab3fd337d7d4ff3ca3c\
        c9dec3fddc95bb377a2cdd79994726c3af3ad40b5927958ee5ff567218255f930286f96ad92a1265f5e3356908ae2069\
        43b88476089686aca243f0011b892a1ce195773589b079d92ff3276a584223bd312a432f84fbda4060b7effafd63033f\
        d32c05c7e9c7db3b4a5bbbea12db1fab7908aaff43bb4e34f4cd4c4c58f83821b4eb3bb8d70a5c0d9a47206ba78ba9f9\
        c16f217af8545d3dd435f7e829d85f7289dcd02a05d751e2e8625d3accc30ab976ed37d95489789d995805768182dc43\
        6c30367446becf5f8ef268f31085a4ec7298deef7ebf1d2af07843d644e79b30e2d59c7868fda662625cfa1bce4c4d94\
        cce18ba9e91d79e3190257d8884770b7836c979c1d6bae3994031536b2bab0b7cf409c1e8c35cba3adea958c9e38bef0\
        b3b88e23bdc2394c07cbe56ad12f15606058ba224c7bb9ae50baaad6aca0673f3828e943c863bd804397bc1c80a5c63e\
        94286675ceb1e72623a8f8068bd9f57fd60d1b3f7d77ab8314216ed1d57af33e14536276ac59e4e55b0508f9c7daadfc\
        fb522b74cd1e5b2042f9dd533df82964093b80cb2a6cdfb53bf0c4bd2e5faa0f3e4b664290130eff1093f8717859f80b\
        cdff9528460fa9fc7cdefb9a302e56c08f85f38381c065c42553f8f591363105a5b0ee6fc1704d470cd19111aaad601d\
        baceb127185c5986e9665258bee976ac59e4e55b0508f9c7daadfcfb522b74cd1e5b2042f9dd533df82964093b80cb2a\
        6cdfb53bf0c4bd2e5faa0f3e4b664290130eff1093f8717859f80bcdff9528460fa9fc7cdefb9a302e56c08f85f38381\
        c065c42553f8f591363105a5b0ee6fc1704d470cd1911092990c900021009900ec7353a7fdbeae7c311a9fb78d316e70\
        9ea7235fec28cb85d195988a7e2a91f22775f719c006984d98fdd8afd5900fc42553f8f591363105a5b0ee6fc1704d47\
        0cd19111aaad601dbaceb127185c5986e9665258bee976ac59e4e55b0508f9c7daadfcfb522b74cd1e5b2042f9dd533d\
        f82964093b80cb2a6cdfb53bf0c4bd2e5faa0f3e4b664290130eff1093f8717859f80bcca3145e000000\
    ";

    const RAW2_FAR: &str = "\
        e015df015d5d00021a9cbaa795e4399da9ba460dc39eb992a6cc1c74c2bc08742537202d67b35285e658ba6fc3c6e924\
        bacdb3c0840d28513ad0bec094f9e9b1cbcbace93bb8043c4ded78fc8a0fc9f0224d5ad7d12befadff866eb50e1f221c\
        1103666a01d0b1e602d4fa34c127a5518c484d8439c6d02628839fe4b38b2400f3ffdc6ea442f42763d5c5fefbccc0b7\
        c094e0d7cdf0b6d073b515f04ca2ba31b2456f0511482d98118a75ea4ef87fe18957fab0dca2a5f032dd7a64b9e7699a\
        d7c4ab60e920681d6b045816259de38136e16697a2633928fc3335c8c2cec217abd23169a87cf8e115b5d7233c7f5b9e\
        bcde0b2f42adf793c2d900371b5e4966e53820fba0b1b305ba77136336ad2b260941400ad7379bbd2fbf33d265651b52\
        67d869ac96c6d291e39b153752129c3337cd00bec9d134558f744711586d608d22a2226a7b57b5e3e3172c26319cffa4\
        7833c479cf4bba48891201f0ec7bf29e452c6c0000\
    ";

    const ALONE_FAR: &str = "\
        5d00200000ffffffffffffffff00021a9cbaa795e4399da9ba460dc39eb992a6cc1c74c2bc08742537202d67b35285e6\
        58ba6fc3c6e924bacdb3c0840d28513ad0bec094f9e9b1cbcbace93bb8043c4ded78fc8a0fc9f0224d5ad7d12befadff\
        866eb50e1f221c1103666a01d0b1e602d4fa34c127a5518c484d8439c6d02628839fe4b38b2400f3ffdc6ea442f42763\
        d5c5fefbccc0b7c094e0d7cdf0b6d073b515f04ca2ba31b2456f0511482d98118a75ea4ef87fe18957fab0dca2a5f032\
        dd7a64b9e7699ad7c4ab60e920681d6b045816259de38136e16697a2633928fc3335c8c2cec217abd23169a87cf8e115\
        b5d7233c7f5b9ebcde0b2f42adf793c2d900371b5e4966e53820fba0b1b305ba77136336ad2b260941400ad7379bbd2f\
        bf33d265651b5267d869ac96c6d291e39b153752129c3337cd00bec9d134558f744711586d608d22a2226a7b57b5e3e3\
        172c26319cffa47833c479cf4bba48891201f0ec7bf29e45cbd9a7ffffbe469000\
    ";

    const RAW2_PERIODIC: &str = "\
        fffeff02795d00010f784905affa38b7cc85806318de7277b89965da36a488aedb4a2aa0e9d63660974d51ca75358481\
        49a9e73bc26dbcb8746702a99f2cca000d1285f030d01f87ffc80349ccc241d49af774905a8412702ad093379e2b51cf\
        d7d88c57a94e107aac746014f7793979a63ac2d0e6991610fe3dfe83258e0b3a1509f20b1f8637e9e4e038576928f06a\
        60b6efb1ab4f3458a1c49c09bbda14d7d4e4526c5d9a0365e32f00890d44f3763b7cadbf3937bddd92fa2f3b2eec4e3c\
        d8dbe9ab50198c46208b0288729b0cf4400a7ad704233ffc3fba94566b6e7d1c56719c8fc6131fa38b092dface1449e1\
        3b36f119aa017d606309eac530e5f4f58d1cc465fbff5051b8a42289bca6ec692e2bef452e4519856b15a546e1c4bb63\
        5d74074095dbfc6c7ca3997de6a862b492e71d5698bcb13568ffbbcc675415fe28e461820f26f2f2cd0f6031aac40902\
        0ba0a7306319295216a790801aeaa69be7591b79bb124f0799a2531fba372d60ddeb72b2e446f654f088752c30dbc250\
        5491229706035fc9703b14c0323798a51501a5e8a3092aecaaff5a26ac720c1692a7be398f24e1709ea7235fec28cb85\
        d195988a7e2a91f22775f719c006984d98fdd8afd5900fc42553f8f591363105a5b0ee6fc1704d470cd19111aaad601d\
        baceb127185c5986e9665258bee976ac59e4e55b0508f9c7daadfcfb522b74cd1e5b2042f9dd533df82964093b80cb2a\
        6cdfb53bf0c4bd2e5faa0f3e4b664290130eff1093f8717859f80bcdff9528460fa9fc7cdefb9a302e56c08f85f38381\
        c065c42553f8f591363105a5b0ee6fc1704d470cd19111aaad601dbaceb127185c5986e9665258bee976ac59e4e55b05\
        08f9c7daadfcfb522b74cd1e5987e5848328ff002200ec7353a7fdbeae7c311a9fb78d316e709ea7235fec28cb85d195\
        988a7de06cf3d20000\
    ";
}
