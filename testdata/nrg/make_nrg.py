#!/usr/bin/env python3
"""Synthetic Nero NRG images for the reader specified in docs/nrg/nrg.md.

Written from that specification alone, independently of src/formats/dos/nrg.rs.
Python 3 standard library only. See README.md.

usage: make_nrg.py trees DIR     source trees for make_volumes.sh
       make_nrg.py pack DIR      DIR/*.iso|*.bin|*.img|*.dmg -> volumes.tar.gz
       make_nrg.py build         images, broken.tar.gz and the three .tsv files
       make_nrg.py check         rebuild in memory and compare with the files here
       make_nrg.py spec [DIR]    rebuild images A to D of nrg.md section 12, compare digests
       make_nrg.py unpack DIR    plain images and the expected entries, for other tools
"""
import gzip, hashlib, io, math, os, struct, sys, tarfile

HERE = os.path.dirname(os.path.abspath(__file__))
MTIME = 1767225600  # 2026-01-01 00:00:00 UTC
NVOL = 12
SESSION_GAP = 11250  # lead-out and lead-in between two sessions, section 7.5
EXTRA_LBA = 11422  # index 1 of the data session of cd-extra.nrg


def fnv1a(data):
    h = 0xCBF29CE484222325
    for b in data:
        h = ((h ^ b) * 0x100000001B3) & 0xFFFFFFFFFFFFFFFF
    return h


# ---------------------------------------------------------------- content

WORDS = (
    "amber basalt cedar delta ember fjord garnet harbor indigo juniper kelp "
    "lichen marble nickel onyx pumice quartz russet slate tundra umber velvet "
    "willow xenon yarrow zinc"
).split()


def text(tag, lines):
    x = fnv1a(tag.encode()) & 0x7FFFFFFF
    out = []
    for n in range(lines):
        ws = []
        for _ in range(6):
            x = (x * 1103515245 + 12345) & 0x7FFFFFFF
            ws.append(WORDS[(x >> 16) % len(WORDS)])
        out.append("%s line %03d: %s\n" % (tag, n, " ".join(ws)))
    return "".join(out).encode()


def counter(tag, n):
    seed = fnv1a(tag.encode())
    step = (seed >> 8) % 250 | 1
    return bytes((seed + i * step + (i >> 8)) & 0xFF for i in range(n))


def pattern(tag, sectors):
    """Data sectors without a file system; every sector differs."""
    return b"".join(
        (("NRG fixture %s sector %06d|" % (tag, i)).encode() * 100)[:2048]
        for i in range(sectors)
    )


def tone(sectors, left, right, amp=12000):
    """Sine tones with periods of `left` and `right` samples (44100/period Hz)."""
    lt = [round(amp * math.sin(2 * math.pi * k / left)) for k in range(left)]
    rt = [round(amp * math.sin(2 * math.pi * k / right)) for k in range(right)]
    return b"".join(
        struct.pack("<hh", lt[k % left], rt[k % right]) for k in range(sectors * 588)
    )


def trees():
    t = {}
    for i in range(1, NVOL + 1):
        v = "vol%02d" % i
        t[v] = {
            "README.TXT": text("NRG_VOL%02d readme" % i, 14 + i),
            "DATA/COUNTER.BIN": counter(v, 3000),
            "DATA/DEEP/NESTED.TXT": text(v + " nested", 1),
        }
    t["extra"] = {
        "README.TXT": text("NRG_EXTRA readme", 18),
        "DATA/COUNTER.BIN": counter("extra", 3000),
        "DATA/DEEP/NESTED.TXT": text("extra nested", 1),
    }
    t["ms1"] = {
        "readme.txt": text("NRG_MS1 readme", 16),
        "keep/stay.bin": counter("ms1 stay", 2500),
        "keep/also.txt": text("ms1 also", 3),
        "docs/changed.txt": text("ms1 changed", 5),
        "docs/removed.txt": text("ms1 removed", 4),
    }
    t["ms2"] = {
        "changed.txt": text("ms2 changed", 12),
        "new.txt": text("ms2 new", 6),
        "second.bin": counter("ms2 second", 1800),
    }
    t["hfs"] = {
        "Read Me": text("NRG HFS read me", 15),
        "Data/counter.bin": counter("hfs", 3000),
    }
    t["hybrid"] = {
        "README.TXT": text("NRG_HYBRID readme", 15),
        "DATA/COUNTER.BIN": counter("hybrid", 3000),
    }
    return t


def volume_files(vol):
    """(path as a reader presents it, content, where the data lies)."""
    t = trees()
    if vol.startswith("vol") or vol == "extra":
        return [(p.lower(), c, "track") for p, c in t[vol].items()]
    if vol in ("hfs", "hybrid"):
        return [(p, c, "track") for p, c in t[vol].items()]
    if vol == "ms1":
        return [(p, c, "track") for p, c in t["ms1"].items()]
    if vol == "ms2":
        one, two = t["ms1"], t["ms2"]
        return [
            ("readme.txt", one["readme.txt"], "earlier"),
            ("keep/stay.bin", one["keep/stay.bin"], "earlier"),
            ("keep/also.txt", one["keep/also.txt"], "earlier"),
            ("docs/changed.txt", two["changed.txt"], "track"),
            ("added/new.txt", two["new.txt"], "track"),
            ("second.bin", two["second.bin"], "track"),
        ]
    raise KeyError(vol)


VOLUME_MEMBER = {
    "extra": "extra.iso", "ms1": "ms1.iso", "ms2": "ms2.bin",
    "hfs": "hfs.dmg", "hybrid": "hybrid.iso",
}
VOLUME_CONTENT = {
    "extra": "iso9660-later-session:NRG_EXTRA",
    "ms1": "iso9660:NRG_MS1",
    "ms2": "iso9660-later-session:NRG_MS2",
    "hfs": "apm+hfsplus:NRG HFS",
    "hybrid": "iso9660+apm+hfsplus:NRG_HYBRID",
}
_volumes = {}


def volume(vol):
    if not _volumes:
        with tarfile.open(os.path.join(HERE, "volumes.tar.gz")) as tar:
            for m in tar.getmembers():
                _volumes[m.name] = tar.extractfile(m).read()
    data = _volumes[VOLUME_MEMBER.get(vol, vol + ".iso")]
    assert len(data) % 2048 == 0, vol
    return data


def volume_content(vol):
    return VOLUME_CONTENT.get(vol, "iso9660:NRG_" + vol.upper())


# ---------------------------------------------------------------- sectors
# ECMA-130 section 14 and annex A; CD-ROM XA Form 1 as testdata/cue describes.

SYNC = b"\0" + b"\xff" * 10 + b"\0"
EDC = []
for _i in range(256):
    _e = _i
    for _ in range(8):
        _e = (_e >> 1) ^ (0xD8018001 if _e & 1 else 0)
    EDC.append(_e)
GF_F = [(i << 1) ^ (0x11D if i & 0x80 else 0) for i in range(256)]
GF_B = [0] * 256
for _i in range(256):
    GF_B[_i ^ GF_F[_i]] = _i


def edc_of(data):
    e = 0
    for b in data:
        e = (e >> 8) ^ EDC[(e ^ b) & 0xFF]
    return e


def _parity(sec, major_count, minor_count, major_mult, minor_inc, dest):
    size = major_count * minor_count
    for major in range(major_count):
        index = (major >> 1) * major_mult + (major & 1)
        a = b = 0
        for _ in range(minor_count):
            v = sec[12 + index]
            index += minor_inc
            if index >= size:
                index -= size
            a ^= v
            b ^= v
            a = GF_F[a]
        a = GF_B[GF_F[a] ^ b]
        sec[dest + major] = a
        sec[dest + major + major_count] = a ^ b


def ecc(sec):
    _parity(sec, 86, 24, 2, 86, 2076)
    _parity(sec, 52, 43, 86, 88, 2248)


def bcd(n):
    return (n // 10) << 4 | n % 10


def msf(lba):
    a = lba + 150
    return bytes([bcd(a // 4500), bcd(a // 75 % 60), bcd(a % 75)])


def raw_mode1(lba, user):
    s = bytearray(SYNC + msf(lba) + b"\x01" + user)
    s += struct.pack("<I", edc_of(s)) + bytes(8 + 276)
    ecc(s)
    return bytes(s)


_form1 = {}


def form1_body(user, submode=0x08):
    """The 2336 bytes after the header of a Mode 2 Form 1 sector."""
    key = (user, submode)
    if key not in _form1:
        s = bytearray(SYNC + bytes(4) + bytes([0, 0, submode, 0]) * 2 + user)
        s += struct.pack("<I", edc_of(s[16:])) + bytes(276)
        ecc(s)  # over a zeroed address
        _form1[key] = bytes(s[16:])
    return _form1[key]


def raw_mode2(lba, body):
    return SYNC + msf(lba) + b"\x02" + body


def subchannel(i):
    """96 bytes P-W, only P and Q bits set, never all zero."""
    return bytes(0x40 if (j * 5 + i) % 3 == 0 else (0x80 if j % 16 == 0 else 0) for j in range(96))


# ---------------------------------------------------------------- chunks

def chunk(ident, content=b""):
    return ident + struct.pack(">I", len(content)) + content


END = chunk(b"END!")


def sinf(n):
    return chunk(b"SINF", struct.pack(">I", n))


def mtyp(n=1):
    return chunk(b"MTYP", struct.pack(">I", n))


def cue_chunk(v2, entries):
    """entries: (control, track byte, index, LBA)."""
    out = b""
    for ctl, trk, idx, lba in entries:
        if v2:
            out += struct.pack(">BBBBi", ctl, trk, idx, 0, lba)
        else:
            out += bytes([ctl, trk, idx, 0, 0]) + msf(lba)
    return chunk(b"CUEX" if v2 else b"CUES", out)


def dao_chunk(v2, entries, stype=0, last=1, first=1, lastno=None, mcn=bytes(13),
              again=None, b17=0, tail=b""):
    """entries: (isrc, S, M, byte 15, bytes 16-17, index 0, index 1, end)."""
    length = 22 + (42 if v2 else 30) * len(entries) + len(tail)
    if lastno is None:
        lastno = first + len(entries) - 1
    body = struct.pack(">I", length if again is None else again) + mcn
    body += bytes([b17, stype, last, first & 0xFF, lastno & 0xFF])
    for isrc, s, m, b15, b1617, i0, i1, end in entries:
        body += isrc + struct.pack(">HBB", s, m, b15) + b1617
        body += struct.pack(">QQQ" if v2 else ">III", i0, i1, end)
    return chunk(b"DAOX" if v2 else b"DAOI", body + tail)


def etn2_chunk(entries, tail=b""):
    """entries: (offset, length, mode, LBA, zero, sectors)."""
    return chunk(b"ETN2", b"".join(struct.pack(">QQIIII", *e) for e in entries) + tail)


def etnf_chunk(entries):
    """entries: (offset, length, mode, sector number, zero)."""
    return chunk(b"ETNF", b"".join(struct.pack(">IIIII", *e) for e in entries))


def tinf_chunk(entries):
    return chunk(b"TINF", b"".join(struct.pack(">III", *e) for e in entries))


def footer(v2, offset):
    return b"NER5" + struct.pack(">Q", offset) if v2 else b"NERO" + struct.pack(">I", offset)


# ---------------------------------------------------------------- discs

FORM_SIZE = {"2048": 2048, "2336": 2336, "raw1": 2352, "raw2": 2352, "audio": 2352}
AUDIO_MODES = (0x07, 0x08, 0x10)


class Disc:
    """Data area under construction, with what each track has to yield."""

    def __init__(self, lba=-150, tao=False):
        self.data = bytearray()
        self.tracks = []
        self.lba = lba
        self.tao = tao
        self.session = 0

    def gap(self, nbytes, sectors=0, fill=0):
        """Bytes in no track."""
        self.data += bytes([fill]) * nbytes
        self.lba += sectors

    def next_session(self, tao=False):
        self.session += 1
        self.lba += SESSION_GAP
        self.tao = tao

    def _sector(self, form, lba, user, eof, sub, nosync, index):
        if form == "2336":
            s = form1_body(user, 0x89 if eof else 0x08)
        elif form == "raw1":
            s = raw_mode1(lba, user)
        elif form == "raw2":
            s = raw_mode2(lba, form1_body(user, 0x89 if eof else 0x08))
        else:
            s = user
        if nosync:
            s = b"\x01" + s[1:]
        return s + (subchannel(index) if sub else b"")

    def track(self, mode, form, cooked, pregap=0, sub=False, content=None, vol=None,
              files=None, pregap_audio=None, nosync=False, header_mode=None, isrc=bytes(12)):
        """`mode` is the code written to the chain, `form` what the sectors are."""
        if vol is not None:
            cooked, content = volume(vol), volume_content(vol)
            files = volume_files(vol)
        audio = form == "audio"
        unit = 2352 if audio else 2048
        n, rest = divmod(len(cooked), unit)
        assert rest == 0
        if self.tao:
            assert pregap == 0
            self.lba += 150  # on the disc, not in the image
        t = {
            "no": len(self.tracks) + 1, "session": self.session, "mode": mode,
            "ssize": FORM_SIZE[form] + (96 if sub else 0), "audio": audio,
            "lba0": self.lba, "index0": len(self.data), "isrc": isrc,
            "cooked": bytes(cooked), "content": content or ("pcm" if audio else "data"),
            "files": files or [], "chunk": None, "why": None,
        }
        for i in range(pregap):
            user = pregap_audio[i * 2352:(i + 1) * 2352] if pregap_audio else bytes(unit)
            self.data += self._sector(form, self.lba + i, user, False, sub, nosync, i)
        self.lba += pregap
        t["lba1"], t["index1"] = self.lba, len(self.data)
        for i in range(n):
            user = cooked[i * unit:(i + 1) * unit]
            s = self._sector(form, self.lba + i, user, i == n - 1, sub, nosync, i)
            if header_mode is not None:
                s = s[:15] + bytes([header_mode]) + s[16:]
            self.data += s
        self.lba += n
        t["lba_end"], t["end"] = self.lba, len(self.data)
        self.tracks.append(t)
        return t

    def of_session(self, session):
        return [t for t in self.tracks if t["session"] == (self.session if session is None else session)]

    def dao(self, session=None, v2=True, cue=True, **header):
        """The cue chunk and the DAO chunk of one session."""
        ts = self.of_session(session)
        ctl = lambda t: 0x01 if t["audio"] else 0x41
        cues = [(ctl(ts[0]), 0, 0, ts[0]["lba0"])]
        entries = []
        for t in ts:
            cues.append((ctl(t), bcd(t["no"]), 0, t["lba0"]))
            cues.append((ctl(t), bcd(t["no"]), 1, t["lba1"]))
            entries.append((t["isrc"], t["ssize"], t["mode"], 0, b"\0\1",
                            t["index0"], t["index1"], t["end"]))
            t["chunk"] = "DAOX" if v2 else "DAOI"
        cues.append((ctl(ts[-1]), 0xAA, 1, ts[-1]["lba_end"]))
        header.setdefault("first", ts[0]["no"])
        header.setdefault("last", 1 if ts[-1] is self.tracks[-1] else 0)
        out = [cue_chunk(v2, cues)] if cue else []
        return out + [dao_chunk(v2, entries, **header)]

    def tao_chunk(self, kind=b"ETN2", session=None, counts=True):
        ts = self.of_session(session)
        entries, stored = [], 0
        for t in ts:
            off, length = t["index1"], t["end"] - t["index1"]
            sectors = length // t["ssize"] if t["ssize"] else 0
            if kind == b"ETN2":
                entries.append((off, length, t["mode"], t["lba1"] & 0xFFFFFFFF, 0,
                                sectors if counts else 0))
            elif kind == b"ETNF":
                entries.append((off, length, t["mode"], stored, 0))
            else:
                entries.append((off, length, t["mode"]))
            stored += sectors
            t["chunk"] = kind.decode()
        make = {b"ETN2": etn2_chunk, b"ETNF": etnf_chunk, b"TINF": tinf_chunk}[kind]
        return make(entries)

    def finish(self, chunks, v2=True, slack=b""):
        return bytes(self.data) + b"".join(chunks) + slack + footer(v2, len(self.data))

    def lose(self, number, why, **fields):
        """Marks a track as one the reader has to report, after editing its chain fields."""
        t = self.tracks[number - 1]
        t.update(fields)
        t["cooked"], t["why"] = None, why

    def lose_all(self, why):
        for t in self.tracks:
            t["cooked"], t["why"] = None, why


# ---------------------------------------------------------------- good images

def g_tao_v2():
    d = Disc(tao=True)
    d.track(0x00, "2048", None, vol="vol01")
    return d.finish([d.tao_chunk(), END]), d


def g_tao_v2_mixed():
    d = Disc(tao=True)
    d.track(0x00, "2048", None, vol="vol02")
    d.track(0x07, "audio", tone(12, 100, 50))
    d.track(0x07, "audio", tone(10, 30, 20))
    return d.finish([d.tao_chunk(counts=False), mtyp(), END]), d


def g_tao_v1():
    d = Disc(tao=True)
    d.track(0x00, "2048", None, vol="vol03")
    d.track(0x07, "audio", tone(12, 70, 35))
    return d.finish([d.tao_chunk(b"ETNF"), sinf(2), END], v2=False), d


def g_tao_modes():
    d = Disc(tao=True)
    d.track(0x02, "2048", pattern("tao-modes 02", 4))
    d.track(0x03, "2336", pattern("tao-modes 03", 4))
    d.track(0x05, "raw1", pattern("tao-modes 05", 4))
    d.track(0x06, "raw2", pattern("tao-modes 06", 4))
    d.track(0x0F, "raw1", pattern("tao-modes 0F", 4), sub=True)
    d.track(0x10, "audio", tone(4, 60, 30), sub=True)
    d.track(0x11, "raw2", pattern("tao-modes 11", 4), sub=True)
    d.track(0x08, "audio", tone(4, 45, 15))
    chain = [d.tao_chunk(), chunk(b"RELO", bytes(4)), chunk(b"TOCT", b"\0\x20"),
             sinf(8), chunk(b"DINF", bytes(4)), mtyp(), END]
    return d.finish(chain), d


def g_tinf():
    d = Disc(tao=True)
    d.track(0x00, "2048", pattern("tinf", 5))
    d.track(0x07, "audio", tone(5, 36, 12))
    return d.finish([d.tao_chunk(b"TINF"), END], v2=False), d


def g_tao_clamp():
    d = Disc(tao=True)
    d.track(0x07, "audio", tone(6, 50, 20))
    d.gap(700, fill=0x11)  # a partial seventh sector
    t = d.tracks[0]
    entry = (0, 10 * len(d.data), 7, 0, 0, 0)
    t["end"], t["chunk"] = entry[1], "ETN2"
    return d.finish([etn2_chunk([entry]), END]), d


def g_dao_v2():
    d = Disc()
    d.track(0x00, "2048", None, vol="vol04", pregap=150)
    return d.finish(d.dao() + [sinf(1), mtyp(), END]), d


def g_dao_v1():
    d = Disc()
    d.track(0x00, "2048", None, vol="vol05", pregap=150)
    d.track(0x07, "audio", tone(12, 100, 20))
    return d.finish(d.dao(v2=False) + [sinf(2), END], v2=False), d


def g_dao_raw_mode1():
    d = Disc()
    d.track(0x05, "raw1", None, vol="vol06", pregap=150)
    return d.finish(d.dao() + [sinf(1), mtyp(), END]), d


def g_dao_xa():
    d = Disc()
    d.track(0x03, "2336", None, vol="vol07", pregap=150)
    d.track(0x02, "2048", None, vol="vol08", pregap=150)
    d.track(0x06, "raw2", None, vol="vol09", pregap=150)
    return d.finish(d.dao(stype=0x20) + [sinf(3), mtyp(), END]), d


def cdtext():
    packs = b""
    for seq, (kind, trk, txt) in enumerate([(0x80, 0, b"NRG FIXTURE\0"), (0x80, 1, b"DATA\0AUDIO\0\0"),
                                            (0x81, 0, b"NOBODY\0\0\0\0\0\0")]):
        pack = bytes([kind, trk, seq, 0]) + txt
        crc = 0
        for b in pack:
            crc ^= b << 8
            for _ in range(8):
                crc = (crc << 1) ^ (0x1021 if crc & 0x8000 else 0)
        packs += pack + struct.pack(">H", ~crc & 0xFFFF)
    return chunk(b"CDTX", packs)


def g_mixed():
    d = Disc()
    d.track(0x00, "2048", None, vol="vol10", pregap=150)
    d.track(0x07, "audio", tone(20, 100, 50), pregap=150, isrc=b"ZZNRG2600002")
    d.track(0x07, "audio", tone(20, 30, 20), isrc=b"ZZNRG2600003")
    d.track(0x07, "audio", tone(16, 70, 35), pregap=75, pregap_audio=tone(75, 60, 12))
    return d.finish(d.dao(mcn=b"0123456789012") + [cdtext(), sinf(4), mtyp(), END]), d


def g_audio():
    d = Disc()
    d.track(0x07, "audio", tone(15, 100, 25), pregap=150)
    d.track(0x07, "audio", tone(12, 45, 36))
    afnm = chunk(b"AFNM", b"01 first tone.wav\0" b"02 second tone.wav\0")
    volm = chunk(b"VOLM", b"Audio CD\0")
    return d.finish(d.dao() + [sinf(2), mtyp(), afnm, volm, END]), d


def g_multisession():
    d = Disc()
    d.track(0x00, "2048", None, vol="ms1", pregap=150)
    d.next_session()
    t = d.track(0x00, "2048", None, vol="ms2", pregap=150)
    assert t["lba1"] == len(volume("ms1")) // 2048 + 11400
    chain = d.dao(0, stype=0x20) + d.dao(1, stype=0x20) + [sinf(1), sinf(1), mtyp(), END]
    return d.finish(chain), d


def g_cd_extra():
    d = Disc()
    d.track(0x07, "audio", tone(12, 100, 50), pregap=150)
    d.track(0x07, "audio", tone(10, 20, 15))
    d.next_session()
    t = d.track(0x02, "2048", None, vol="extra", pregap=150)
    assert t["lba1"] == EXTRA_LBA
    chain = d.dao(0) + d.dao(1, stype=0x20) + [sinf(2), sinf(1), mtyp(), END]
    return d.finish(chain), d


def g_hfs():
    d = Disc(tao=True)
    d.track(0x00, "2048", None, vol="hfs")
    return d.finish([d.tao_chunk(), END]), d


def g_hybrid():
    d = Disc()
    d.track(0x00, "2048", None, vol="hybrid", pregap=150)
    return d.finish(d.dao() + [sinf(1), mtyp(), END]), d


def g_dvd():
    d = Disc(lba=0)
    d.track(0x00, "2048", None, vol="vol11")
    return d.finish(d.dao(stype=0x40) + [sinf(1), mtyp(0x1C), END]), d


def g_gaps():
    d = Disc()
    d.track(0x02, "2048", None, vol="vol12", pregap=150)
    d.gap(150 * 2048 + 150 * 2352, sectors=300)
    d.tracks[-1]["lba_end"] = d.lba
    d.track(0x07, "audio", tone(14, 100, 50))
    d.gap(150 * 2352, sectors=150)
    d.tracks[-1]["lba_end"] = d.lba
    d.track(0x07, "audio", tone(14, 35, 20))
    return d.finish(d.dao(stype=0x20) + [sinf(3), mtyp(), END]), d


def g_subchannel():
    d = Disc()
    d.track(0x0F, "raw1", pattern("subchannel 0F", 6), pregap=150, sub=True)
    d.track(0x10, "audio", tone(8, 50, 25), sub=True)
    d.track(0x11, "raw2", pattern("subchannel 11", 6), pregap=150, sub=True)
    return d.finish(d.dao() + [sinf(3), mtyp(), END]), d


def g_clamp():
    d = Disc(lba=0)
    d.track(0x00, "2048", pattern("clamp", 5))
    d.gap(1000, fill=0x22)  # partial sixth sector, inside the track's range
    d.tracks[0]["end"] = len(d.data)
    d.track(0x07, "audio", tone(4, 30, 15))
    d.gap(700, fill=0x33)  # partial fifth sector before the chain
    d.tracks[1]["end"] = 0x7FFFFFFFFFFFFFFF
    return d.finish(d.dao() + [sinf(2), mtyp(), END]), d


def g_mismatch():
    d = Disc(lba=0)
    p = lambda n: pattern("mismatch %02d" % n, 3)
    d.track(0x00, "raw1", p(1))                     # code says cooked, sectors are raw
    d.track(0x03, "raw2", p(2))
    d.track(0x05, "raw2", p(3))                     # the sector's mode byte decides
    d.track(0x06, "raw1", p(4))
    d.track(0x05, "raw1", p(5), nosync=True)        # no sync pattern: the code decides
    d.track(0x06, "raw2", p(6), nosync=True)
    d.track(0x02, "2336", p(7))
    d.track(0x00, "2336", p(8))
    d.track(0x03, "2048", p(9))
    d.track(0x07, "audio", tone(3, 50, 30), sub=True)
    d.track(0x10, "audio", tone(3, 20, 12))
    d.track(0x00, "raw1", p(12), nosync=True)
    d.track(0x02, "raw2", p(13), nosync=True)
    d.track(0x05, "raw1", p(14), header_mode=0x00)  # sync pattern, mode byte neither 1 nor 2
    d.track(0x11, "raw2", p(15), header_mode=0x03, sub=True)
    return d.finish(d.dao() + [sinf(15), mtyp(), END]), d


def g_families():
    d = Disc(lba=-2)
    d.track(0x08, "audio", tone(3, 100, 20), pregap=2)
    d.next_session(tao=True)
    d.track(0x00, "2048", pattern("families etn2", 3))
    d.next_session(tao=True)
    d.track(0x07, "audio", tone(3, 60, 15))
    d.next_session(tao=True)
    d.track(0x00, "2048", pattern("families tinf", 3))
    d.next_session()
    d.track(0x03, "2336", pattern("families daoi", 3), pregap=2)
    chain = d.dao(0, last=0) + [d.tao_chunk(b"ETN2", 1), d.tao_chunk(b"ETNF", 2),
                                d.tao_chunk(b"TINF", 3)] + d.dao(4, v2=False)
    return d.finish(chain + [sinf(1)] * 5 + [END], v2=False), d


def g_v2_then_v1():
    d = Disc(tao=True)
    d.track(0x00, "2048", pattern("v2-then-v1", 3))
    d.track(0x07, "audio", tone(3, 70, 20))
    return d.finish([d.tao_chunk(b"ETNF"), END], v2=False, slack=b"NER5"), d


def base(pregap=2):
    """The three-track image the variants below and the broken images change."""
    d = Disc(lba=-pregap)
    d.track(0x00, "2048", pattern("base 1", 2), pregap=pregap)
    d.track(0x07, "audio", tone(2, 20, 10), pregap=1)
    d.track(0x03, "2336", pattern("base 3", 2))
    return d


def base_chain(d, extra=()):
    return d.dao(stype=0x20) + [sinf(3), mtyp()] + list(extra) + [END]


def tao_base():
    d = Disc(tao=True)
    d.track(0x00, "2048", pattern("tao base 1", 2))
    d.track(0x07, "audio", tone(2, 30, 15))
    return d


def g_unknown_chunks():
    d = base(150)
    cue, dao = d.dao(stype=0x20)
    decoy = END + footer(True, 0) + dao[8:] + b"NERO\0\0\0\0"
    chain = [chunk(b"ZZZZ", b"unknown chunk"), chunk(b"    "), dao, chunk(b"DECO", decoy),
             chunk(b"~~~~", b"\xff" * 7), chunk(b"nero", b"abc"), chunk(b"end!"), chunk(b"END?"),
             chunk(b"VOLM", b"Audio CD\0"), sinf(3), chunk(b"AFNM", b"x.wav\0"), mtyp(), cue,
             chunk(b"DAOY", dao[8:]), END]
    return d.finish(chain), d


def g_chunks_256():
    d = base(150)
    return d.finish(base_chain(d, [chunk(b"JUNK")] * 251)), d


def g_chain_1mib():
    d = tao_base()
    track = d.tao_chunk()
    pad = chunk(b"PADD", bytes(1048576 - len(track) - 16))
    return d.finish([track, pad, END]), d


def g_lying_fields():
    d = Disc(lba=-2)
    d.track(0x00, "2048", pattern("lying 1", 4), pregap=2, isrc=b"/etc/passwd\0")
    d.track(0x07, "audio", tone(3, 50, 12), isrc=b"..\\..\\x.pcm\0")
    d.next_session(tao=True)
    d.track(0x03, "2336", pattern("lying 3", 3))
    one, two, three = d.tracks
    cue = chunk(b"CUEX", bytes(range(1, 14)) + struct.pack(">BBBBi", 0x41, 0x99, 9, 0xFF, -(1 << 31)))
    entries = [(t["isrc"], t["ssize"], t["mode"], 0xFF, b"\xff\xff", t["index0"], t["index1"], t["end"])
               for t in (one, two)]
    dao = dao_chunk(True, entries, stype=0x10, last=0x77, first=9, lastno=3, mcn=b"../../etc/pwd",
                    again=0xFFFFFFFF, b17=0xFF, tail=b"\xff" * 41)
    one["chunk"] = two["chunk"] = "DAOX"
    three["chunk"] = "ETN2"
    tao = etn2_chunk([(three["index1"], three["end"] - three["index1"], 3, 0xFFFFFFFF, 0xFFFFFFFF,
                       0xFFFFFFFF)], tail=b"\xff" * 31)
    chain = [cue, dao, tao, sinf(99), chunk(b"SINF"), mtyp(0xFFFFFFFF), chunk(b"END!", b"more")]
    return d.finish(chain, slack=b"slack"), d


def many_tracks(n, tones=True):
    d = Disc(tao=True)
    for i in range(1, n + 1):
        if i % 2 or not tones:
            d.track(0x00, "2048", pattern("many %03d" % i, 1))
        else:
            d.track(0x07, "audio", tone(1, 6 + i // 2, 6 + i // 2, amp=3000 + 100 * i))
    return d


def g_tracks_99():
    d = many_tracks(99)
    return d.finish([d.tao_chunk(), sinf(99), END]), d


def g_nested():
    inner = tao_base()
    track = inner.tao_chunk()
    size = len(inner.data) + len(track) + 8 + 8 + 12
    pad = chunk(b"PADD", bytes(-size % 2048))
    image = inner.finish([track, pad, END])
    d = Disc()
    files = [("track%02d.%s" % (t["no"], "pcm" if t["audio"] else "img"), t["cooked"], "track")
             for t in inner.tracks]
    d.track(0x00, "2048", image, pregap=150, content="nrg", files=files)
    return d.finish(d.dao() + [sinf(1), mtyp(), END]), d


GOOD = [
    ("tao-v2", g_tao_v2), ("tao-v2-mixed", g_tao_v2_mixed), ("tao-v1", g_tao_v1),
    ("tao-modes", g_tao_modes), ("tinf", g_tinf), ("tao-clamp", g_tao_clamp),
    ("dao-v2", g_dao_v2), ("dao-v1", g_dao_v1), ("dao-raw-mode1", g_dao_raw_mode1),
    ("dao-xa", g_dao_xa), ("mixed", g_mixed), ("audio", g_audio),
    ("multisession", g_multisession), ("cd-extra", g_cd_extra), ("hfs", g_hfs),
    ("hybrid", g_hybrid), ("dvd", g_dvd), ("gaps", g_gaps), ("subchannel", g_subchannel),
    ("clamp", g_clamp), ("mismatch", g_mismatch), ("families", g_families),
    ("v2-then-v1", g_v2_then_v1), ("unknown-chunks", g_unknown_chunks),
    ("chunks-256", g_chunks_256), ("chain-1mib", g_chain_1mib),
    ("lying-fields", g_lying_fields), ("tracks-99", g_tracks_99), ("nested", g_nested),
]

# ---------------------------------------------------------------- broken images

def base_image():
    d = base()
    return d.finish(base_chain(d)), d


def with_offset(image, value, v2=True):
    return image[:-8] + struct.pack(">Q", value) if v2 else image[:-4] + struct.pack(">I", value)


def not_nrg():
    """(name, bytes, rule, defect)"""
    img, d = base_image()
    size, D, E = len(img), len(d.data), len(img) - 12
    v1d = tao_base()
    v1 = v1d.finish([v1d.tao_chunk(b"ETNF"), sinf(2), END], v2=False)
    head = d.dao(stype=0x20)
    out = [
        ("bad-truncated", img[:-1], "3.2(1) 16.1", "last byte removed"),
        ("bad-truncated-footer", img[:-12], "3.2(1) 16.1", "the 12 footer bytes removed; the file ends with the END! chunk"),
        ("bad-truncated-data", img[:D // 2], "3.2(1) 16.1", "cut in the middle of the data area"),
        ("bad-empty", b"", "3.2(1)", "empty file"),
        ("bad-tiny-v2", footer(True, 0), "3.2(1)", "12 bytes: a version 2 footer and nothing else"),
        ("bad-tiny-v1", footer(False, 0), "3.2(1)", "8 bytes: a version 1 footer and nothing else"),
        ("bad-minimal-no-tracks", END + footer(True, 0), "3.2(4)", "20 bytes: END! and a version 2 footer; a valid chain without tracks"),
        ("bad-trailing-byte", img + b"\0", "3.2(1)", "one zero byte after the footer"),
        ("bad-trailing-sector", img + bytes(2048), "3.2(1)", "2048 zero bytes after the footer"),
        ("bad-offset-zero", with_offset(img, 0), "3.2(3) 16.1", "chain offset 0: the first identifier would be four zero bytes of the pregap"),
        ("bad-offset-plus1", with_offset(img, D + 1), "3.2(3)", "chain offset one too high: the first identifier is UEX and a zero byte"),
        ("bad-offset-no-room", with_offset(img, E - 7), "3.2(2) 16.1", "chain offset 7 bytes before the footer: no room for a chunk header"),
        ("bad-offset-at-footer", with_offset(img, E), "3.2(2) 16.1", "chain offset is the offset of the footer"),
        ("bad-offset-filesize", with_offset(img, size), "3.2(2) 16.1", "chain offset is the file size"),
        ("bad-offset-beyond", with_offset(img, size + 4096), "3.2(2) 16.1", "chain offset beyond the end of the file"),
        ("bad-offset-max", with_offset(img, 0xFFFFFFFFFFFFFFFF), "3.2(2) 16.1", "chain offset FFFFFFFFFFFFFFFF"),
        ("bad-v1-offset-max", with_offset(v1, 0xFFFFFFFF, False), "3.2(2) 16.1", "version 1 footer with chain offset FFFFFFFF"),
        ("bad-v1-offset-plus1", with_offset(v1, len(v1d.data) + 1, False), "3.2(3)", "version 1 footer with a chain offset one too high"),
        ("bad-signature", img[:-12] + b"NER6" + img[-8:], "3.2(1)", "signature NER6"),
        ("bad-signature-swapped", img[:-12] + b"NERO" + img[-8:], "3.2(1)", "NERO in place of NER5 before a 64-bit offset: the last 8 bytes do not start with NERO"),
    ]

    def chained(chunks, dd=d):
        return bytes(dd.data) + b"".join(chunks) + footer(True, len(dd.data))

    tail = [sinf(3), mtyp()]
    out += [
        ("bad-no-end", chained(head + tail + [chunk(b"XXXX")]), "3.2(3) 16.2", "XXXX in place of END!: the chain runs into the footer"),
        ("bad-end-overrun", chained(head + tail + [b"END!\xff\xff\xff\xff"]), "3.2(3) 5.9 16.2", "END! with length FFFFFFFF, which is bounds-checked like any other"),
        ("bad-chunk-overrun", chained(head + [b"SINF\xff\xff\xff\xff" + struct.pack(">I", 3), mtyp(), END]), "3.2(3) 16.2", "SINF with length FFFFFFFF"),
        ("bad-chunk-overrun-1", chained(head + tail + [b"JUNK" + struct.pack(">I", 9) + END]), "3.2(3) 16.2", "last chunk reaches one byte into the footer"),
        ("bad-chunk-swallows-end", chained(head + tail + [chunk(b"JUNK", END)]), "3.2(3) 16.2", "last chunk ends at the footer and contains the only END!"),
        ("bad-chunk-id-nul", chained(head + [chunk(b"SIN\0", struct.pack(">I", 3)), mtyp(), END]), "3.2(3)", "identifier with a zero byte"),
        ("bad-chunk-id-high", chained(head + [chunk(b"SIN\xc4", struct.pack(">I", 3)), chunk(b"\x7fTYP"), END]), "3.2(3)", "identifiers with the bytes C4 and 7F"),
        ("bad-chunks-257", chained(head + tail + [chunk(b"JUNK")] * 252 + [END]), "3.2(3) 16.2", "257 chunks, END! included"),
        ("bad-no-track-chunk", chained([head[0]] + tail + [END]), "3.2(4)", "CUEX, SINF, MTYP, END!: no track chunk"),
        ("bad-empty-track-chunks", chained([head[0], chunk(b"DAOX", head[1][8:30]), chunk(b"DAOX", bytes(21)), chunk(b"ETN2", bytes(31)), chunk(b"ETNF", bytes(19)), chunk(b"TINF", bytes(11)), chunk(b"DAOI", bytes(51))] + tail + [END]), "3.2(4) 5.1 5.2", "track chunks too short for one whole entry"),
    ]
    t = tao_base()
    track = t.tao_chunk()
    out.append(("bad-chain-1mib-plus1", t.finish([track, chunk(b"PADD", bytes(1048577 - len(track) - 16)), END]), "3.2(2) 16.2", "chain of 1,048,577 bytes, otherwise as chain-1mib.nrg"))
    m = many_tracks(100, tones=False)
    out.append(("bad-tracks-100", m.finish([m.tao_chunk(), sinf(100), END]), "3.2(4) 8.1 16.3", "100 tracks in one ETN2 chunk"))
    m = many_tracks(100, tones=False)
    for t in m.tracks[60:]:
        t["session"] = 1
    m.session = 1
    out.append(("bad-tracks-100-split", m.finish([m.tao_chunk(session=0), m.tao_chunk(session=1), END]), "3.2(4) 8.1 16.3", "100 tracks in two ETN2 chunks of 60 and 40"))
    return out


def opened():
    """(name, bytes, disc, rule, defect): recognised, with tracks to report."""
    out = []

    def dao_case(name, rule, defect, edit, chain=None):
        d = base()
        edit(d)
        out.append((name, d.finish(chain(d) if chain else base_chain(d)), d, rule, defect))

    def tao_case(name, rule, defect, edit):
        d = tao_base()
        entries = edit(d)
        for t in d.tracks:
            t["chunk"] = "ETN2"
        out.append((name, d.finish([etn2_chunk(entries), sinf(2), END]), d, rule, defect))

    D = len(base().data)
    dao_case("bad-track-beyond", "8.3(2) 16.6", "track 3 has index 1 and end 4096 bytes beyond the chain offset",
             lambda d: d.lose(3, "beyond", index0=D + 4096, index1=D + 4096, end=D + 8192))
    dao_case("bad-track-at-end", "8.3(2) 16.6", "track 3 has index 1 at the chain offset",
             lambda d: d.lose(3, "beyond", index1=D, end=D + 2336))
    dao_case("bad-sector-size-zero", "8.3(1) 16.5", "track 3 has sector size 0",
             lambda d: d.lose(3, "sector-size", ssize=0))

    def absurd(d):
        d.lose(1, "sector-size", ssize=1)
        d.lose(3, "sector-size", ssize=0xFFFF)
    dao_case("bad-sector-size-absurd", "8.3(1) 16.5", "track 1 has sector size 1, track 3 has 65535", absurd)

    def near(d):
        d.lose(1, "sector-size", ssize=2324)
        d.lose(3, "sector-size", ssize=2340)
    dao_case("bad-sector-size-near", "8.3(1) 16.5", "track 1 has sector size 2324, track 3 has 2340", near)
    dao_case("bad-audio-2048", "6.1 8.3(1)", "track 1 has the audio mode 07 with sector size 2048",
             lambda d: d.lose(1, "sector-size", mode=0x07))
    dao_case("bad-mode-unknown", "6.1 8.3(1) 16.13", "track 2 has mode 09",
             lambda d: d.lose(2, "mode", mode=0x09))

    def all_modes(d):
        d.lose(1, "mode", mode=0x01)
        d.lose(2, "mode", mode=0x20)
        d.lose(3, "mode", mode=0xFF)
    dao_case("bad-mode-unknown-all", "6.1 10.4", "modes 01, 20 and FF: the image opens and lists nothing", all_modes)
    dao_case("bad-empty-track", "8.3(4)", "track 2 ends at its index 1",
             lambda d: d.lose(2, "empty", end=d.tracks[1]["index1"]))
    dao_case("bad-short-track", "8.3(4)", "track 3 is one byte short of a sector",
             lambda d: d.lose(3, "empty", end=d.tracks[2]["index1"] + 2335))

    def tao_mode(d):
        one, two = d.tracks
        d.lose(2, "mode")
        return [(0, one["end"], 0, 0, 0, 2),
                (two["index1"], two["end"] - two["index1"], 0x01000007, 152, 0, 2)]
    tao_case("bad-tao-mode-high", "8.1 8.3(1)", "ETN2 track 2 has the 32-bit mode 01000007", tao_mode)

    def layout(name, rule, defect, edit, chain=None):
        def run(d):
            edit(d)
            d.lose_all("layout")
        dao_case(name, rule, defect, run, chain)

    layout("bad-overlap", "8.2 16.7", "track 2 has index 0 one sector before the end of track 1",
           lambda d: d.tracks[1].update(index0=d.tracks[0]["end"] - 2048))
    layout("bad-end-before-start", "8.2", "track 1 ends one byte before its index 1",
           lambda d: d.tracks[0].update(end=d.tracks[0]["index1"] - 1))
    layout("bad-index0-after-index1", "8.2", "track 2 has index 0 one byte after its index 1",
           lambda d: d.tracks[1].update(index0=d.tracks[1]["index1"] + 1))

    def swap(d):
        a, b = d.tracks[0], d.tracks[1]
        for key in ("mode", "ssize", "index0", "index1", "end", "audio"):
            a[key], b[key] = b[key], a[key]
    layout("bad-order", "8.2", "the entries of tracks 1 and 2 are exchanged", swap)

    def twice(d):
        cue, dao = d.dao(stype=0x20)
        chain = [cue, dao, dao, sinf(3), mtyp(), END]
        d.tracks += [dict(t, no=t["no"] + 3) for t in d.tracks]
        return chain
    d = base()
    chain = twice(d)
    d.lose_all("layout")
    out.append(("bad-duplicate-session", d.finish(chain), d, "8.2 16.7", "the DAOX chunk is there twice: six tracks, the fourth starts before the third ends"))

    d = Disc(lba=0)
    d.track(0x07, "audio", tone(4, 25, 10))
    whole = (bytes(12), 2352, 7, 0, b"\0\1", 0, 0, len(d.data))
    d.tracks = [dict(d.tracks[0], no=i + 1, chunk="DAOX") for i in range(99)]
    d.lose_all("layout")
    out.append(("bad-amplify", d.finish([dao_chunk(True, [whole] * 99), sinf(99), mtyp(), END]), d, "8.2 16.7", "99 tracks that all cover the same four sectors"))

    def overflow(d):
        one = d.tracks[0]
        d.lose_all("layout")
        return [(0, one["end"], 0, 0, 0, 2), (0xFFFFFFFFFFFFF000, 0x2000, 7, 152, 0, 2)]
    tao_case("bad-tao-overflow", "8.2 16.4", "ETN2 track 2: offset FFFFFFFFFFFFF000 plus length 2000 overflows 64 bits", overflow)

    def tao_overlap(d):
        one, two = d.tracks
        d.lose_all("layout")
        return [(0, one["end"], 0, 0, 0, 2), (one["end"] - 2048, two["end"] - two["index1"], 7, 152, 0, 2)]
    tao_case("bad-tao-overlap", "8.2 16.7", "ETN2 track 2 starts one sector before the end of track 1", tao_overlap)
    return out


# ---------------------------------------------------------------- spec vectors

def spec_images():
    """Images A to D of nrg.md section 12 with the digests stated there."""
    dsec = lambda i: (b"T1-%06d|" % i * 205)[:2048]
    asec = lambda i: b"".join(
        struct.pack("<hh", v, -v) for v in ((i * 588 + k) % 30000 for k in range(588)))

    def xsec(i):
        if i == 7:
            return bytes([0, 0, 0x20, 0] * 2) + (b"F2-000007|" * 233)[:2324] + bytes(4)
        return bytes([0, 0, 8, 0] * 2) + (b"T3-%06d|" % i * 205)[:2048] + bytes(280)

    m1 = lambda lba, u: SYNC + msf(lba) + b"\x01" + u + bytes(288)
    m2 = lambda lba, x: SYNC + msf(lba) + b"\x02" + x
    sub = b"S" * 96
    t1 = b"".join(dsec(i) for i in range(20))
    au = b"".join(asec(i) for i in range(30))
    xs = b"".join(xsec(i) for i in range(12))
    z = bytes(12)
    e = lambda s, m, i0, i1, end: (z, s, m, 0, b"\0\1", i0, i1, end)

    a = bytes(307200) + t1 + bytes(23520) + au + (bytes([0, 0, 8, 0] * 2) + bytes(2328)) * 5 + xs
    a += cue_chunk(True, [(0x41, 0, 0, -150), (0x41, 1, 0, -150), (0x41, 1, 1, 0), (1, 2, 0, 20),
                          (1, 2, 1, 30), (0x41, 3, 0, 60), (0x41, 3, 1, 65), (0x41, 0xAA, 1, 77)])
    a += dao_chunk(True, [e(2048, 0, 0, 307200, 348160), e(2352, 7, 348160, 371680, 442240),
                          e(2336, 3, 442240, 453920, 481952)], stype=0x20)
    a += sinf(3) + mtyp() + END + footer(True, 481952)

    b = t1 + xs + au
    b += etnf_chunk([(0, 40960, 0, 0, 0), (40960, 28032, 3, 20, 0), (68992, 70560, 7, 32, 0)])
    b += sinf(3) + END + footer(False, 139552)

    c = m1(-2, bytes(2048)) + m1(-1, bytes(2048))
    c += b"".join(m1(i, dsec(i)) for i in range(20)) + b"\xee" * 1000
    c += b"".join(asec(i) + sub for i in range(30))
    c += b"".join(m2(11300 + i, xsec(i)) + sub for i in range(12))
    c += b"".join(m1(11312 + i, dsec(i)) + sub for i in range(20))
    c += b"".join(m2(11332 + i, xsec(i)) for i in range(12))
    c += cue_chunk(True, [(0x41, 0, 0, -2), (0x41, 1, 0, -2), (0x41, 1, 1, 0), (1, 2, 0, 20),
                          (1, 2, 1, 20), (1, 0xAA, 1, 50)])
    c += dao_chunk(True, [e(2352, 5, 0, 4704, 51744), e(2448, 0x10, 52744, 52744, 126184)], last=0)
    c += chunk(b"ZZZZ", b"unknown chunk")
    c += cue_chunk(True, [(0x41, 0, 0, 11300), (0x41, 3, 0, 11300), (0x41, 3, 1, 11300),
                          (0x41, 4, 0, 11312), (0x41, 4, 1, 11312), (0x41, 5, 0, 11332),
                          (0x41, 5, 1, 11332), (0x41, 0xAA, 1, 11344)])
    c += dao_chunk(True, [e(2448, 0x11, 126184, 126184, 155560), e(2448, 0x0F, 155560, 155560, 204520),
                          e(2352, 6, 204520, 204520, 232744)], stype=0x20, first=3)
    c += sinf(2) + sinf(3) + mtyp() + END + footer(True, 232744)

    dd = bytes(307200) + t1 + au
    dd += cue_chunk(False, [(0x41, 0, 0, -150), (0x41, 1, 0, -150), (0x41, 1, 1, 0), (1, 2, 0, 20),
                            (1, 2, 1, 20), (1, 0xAA, 1, 50)])
    dd += dao_chunk(False, [e(2048, 0, 0, 307200, 348160), e(2352, 7, 348160, 348160, 418720)])
    dd += sinf(2) + END + footer(False, 418720)
    return [
        ("spec-a", a, "bb401e8fec56a9a4556a5bf165d7f75c70fe177ffb358ad71add20b73e12db5f"),
        ("spec-b", b, "d45ed70fb4bcd3d7c8cc5bc82cc7168fc9422e06d4a77e00922283cfde5fa7cc"),
        ("spec-c", c, "ac97914aca3f08f591301e5c34ead7a709f30029f340255984dc049a54765b94"),
        ("spec-d", dd, "06fdc93f479547a17089f247c8144e11b67fee343fefe1fae6dc22369ecc46c7"),
    ]


# ---------------------------------------------------------------- output

MANIFEST = ("archive image track entry chunk mode sector_size index0 index1 end lba "
            "sectors size fnv64 content").split()
FILES = "archive image track path size fnv64 where".split()
BROKEN = "archive image outcome entries reported reasons rule defect".split()


def entry_name(t):
    return "track%02d.%s" % (t["no"], "pcm" if t["audio"] else "img")


def rows_of(archive, image, d):
    manifest, files = [], []
    for t in d.tracks:
        c = t["cooked"]
        if c is None:
            continue
        unit = 2352 if t["audio"] else 2048
        manifest.append([archive, image, t["no"], entry_name(t), t["chunk"], "%02x" % t["mode"],
                         t["ssize"], t["index0"], t["index1"], t["end"], t["lba1"], len(c) // unit,
                         len(c), "%016x" % fnv1a(c), t["content"]])
        for path, content, where in t["files"]:
            files.append([archive, image, t["no"], path, len(content), "%016x" % fnv1a(content), where])
    return manifest, files


def tsv(header, rows):
    return "".join("\t".join(str(c) for c in row) + "\n" for row in [header] + rows).encode()


def tar_of(members):
    buf = io.BytesIO()
    with tarfile.open(fileobj=buf, mode="w", format=tarfile.USTAR_FORMAT) as tar:
        for name, data in sorted(members):
            info = tarfile.TarInfo(name)
            info.size, info.mtime, info.mode = len(data), MTIME, 0o644
            tar.addfile(info, io.BytesIO(data))
    return buf.getvalue()


def build():
    """{file name: (plain bytes, gzip?)} and the plain images by name."""
    out, plain, manifest, files, broken = {}, {}, [], [], []
    for name, make in GOOD:
        image, d = make()
        assert all(t["chunk"] for t in d.tracks), name
        plain[name + ".nrg"] = (image, d)
        out[name + ".nrg.gz"] = (image, True)
        m, f = rows_of("-", name + ".nrg", d)
        manifest += m
        files += f
    members = []
    for name, image, rule, defect in not_nrg():
        members.append((name + ".nrg", image))
        plain[name + ".nrg"] = (image, None)
        broken.append(["broken.tar.gz", name + ".nrg", "not-nrg", "-", "-", "-", rule, defect])
    for name, image, d, rule, defect in opened():
        members.append((name + ".nrg", image))
        plain[name + ".nrg"] = (image, d)
        m, f = rows_of("broken.tar.gz", name + ".nrg", d)
        manifest += m
        lost = [t for t in d.tracks if t["cooked"] is None]
        reasons = ",".join(sorted({t["why"] for t in lost}))
        broken.append(["broken.tar.gz", name + ".nrg", "partial" if m else "no-entries",
                       ",".join(row[3] for row in m) or "-", len(lost), reasons, rule, defect])
    assert len({m[0] for m in members}) == len(members)
    out["broken.tar.gz"] = (tar_of(members), True)
    out["manifest.tsv"] = (tsv(MANIFEST, manifest), False)
    out["files.tsv"] = (tsv(FILES, files), False)
    out["broken.tsv"] = (tsv(BROKEN, broken), False)
    return out, plain


def main(argv):
    cmd = argv[1] if len(argv) > 1 else ""
    if cmd == "trees":
        for vol, tree in trees().items():
            for path, content in tree.items():
                full = os.path.join(argv[2], vol, path)
                os.makedirs(os.path.dirname(full), exist_ok=True)
                with open(full, "wb") as f:
                    f.write(content)
                os.utime(full, (MTIME, MTIME))
    elif cmd == "pack":
        names = sorted(n for n in os.listdir(argv[2]) if n.endswith((".iso", ".bin", ".dmg")))
        tar = tar_of([(n, open(os.path.join(argv[2], n), "rb").read()) for n in names])
        with open(os.path.join(HERE, "volumes.tar.gz"), "wb") as f:
            f.write(gzip.compress(tar, 9, mtime=0))
    elif cmd in ("build", "check"):
        out, _ = build()
        bad = 0
        for name, (data, packed) in sorted(out.items()):
            path = os.path.join(HERE, name)
            if cmd == "build":
                with open(path, "wb") as f:
                    f.write(gzip.compress(data, 9, mtime=0) if packed else data)
                continue
            # Compared unpacked: the deflate stream depends on the zlib at hand.
            have = open(path, "rb").read() if os.path.exists(path) else None
            if have is not None and packed:
                have = gzip.decompress(have)
            if have != data:
                bad += 1
                print("differs:", name)
        if cmd == "check":
            print("%d files, %d differ" % (len(out), bad))
            return 1 if bad else 0
    elif cmd == "spec":
        bad = 0
        for name, image, digest in spec_images():
            ok = hashlib.sha256(image).hexdigest() == digest
            bad += not ok
            print("%s %7d bytes %s" % (name, len(image), "matches nrg.md" if ok else "DIFFERS"))
            if len(argv) > 2:
                with open(os.path.join(argv[2], name + ".nrg"), "wb") as f:
                    f.write(image)
        return 1 if bad else 0
    elif cmd == "unpack":
        _, plain = build()
        for name, (image, d) in plain.items():
            with open(os.path.join(argv[2], name), "wb") as f:
                f.write(image)
            for t in d.tracks if d else []:
                if t["cooked"] is not None:
                    os.makedirs(os.path.join(argv[2], name + ".tracks"), exist_ok=True)
                    with open(os.path.join(argv[2], name + ".tracks", entry_name(t)), "wb") as f:
                        f.write(t["cooked"])
    else:
        print(__doc__)
        return 2
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
