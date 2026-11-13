#!/usr/bin/env python3
"""Synthetic UDF volumes for the layouts no tool here can write without mounting.

Written from the descriptor layouts of ECMA-167 and the OSTA UDF specifications,
independently of the reader in src/formats/dos/udf.rs. See README.md.

usage: make_synthetic.py out.img set mode bs [variant] >> manifest rows
mode: phys | meta | vat15 | vat20 | spar
Prints one manifest.tsv row per expected file for `set`.
"""
import random, struct, sys

out, setname, mode, bs = sys.argv[1], sys.argv[2], sys.argv[3], int(sys.argv[4])
variant = sys.argv[5] if len(sys.argv) > 5 else ""
random.seed(42)

_serial = [0]

def filler(n):
    """Compressible bytes that differ from call to call and along their length."""
    _serial[0] += 1
    k = _serial[0]
    head = b"<%06d>" % k
    runs = b"".join(bytes([(k * 7 + j) % 251 + 1]) * 61 for j in range(n // 61 + 1))
    return (head + runs)[:n]

def fnv1a(data):
    h = 0xCBF29CE484222325
    for b in data:
        h = ((h ^ b) * 0x100000001B3) & 0xFFFFFFFFFFFFFFFF
    return h
NSR3 = mode in ("meta", "vat20") or "nsr3" in variant
VER = 3 if NSR3 else 2
EFE = NSR3

def crc16(b):
    c = 0
    for x in b:
        c ^= x << 8
        for _ in range(8):
            c = ((c << 1) ^ 0x1021) & 0xFFFF if c & 0x8000 else (c << 1) & 0xFFFF
    return c

def tagged(tid, body, loc):
    t = bytearray(struct.pack("<HHBBHHHI", tid, VER, 0, 0, 1, crc16(body), len(body), loc))
    t[4] = (sum(t[0:4]) + sum(t[5:16])) & 255
    return bytes(t) + body

def regid(s, suffix=b""):
    return b"\0" + s.encode().ljust(23, b"\0") + suffix.ljust(8, b"\0")

def dstring(s, n):
    b = b"\x08" + s.encode("latin1")
    return b.ljust(n - 1, b"\0") + bytes([len(b)])

def charspec():
    return b"\0" + b"OSTA Compressed Unicode".ljust(63, b"\0")

def osta(name):
    if all(ord(c) < 256 for c in name):
        return b"\x08" + name.encode("latin1")
    return b"\x10" + name.encode("utf-16-be")

TS = struct.pack("<HhBBBBBBBB", 0x1000, 2001, 2, 3, 4, 5, 6, 0, 0, 0)

NBLK = 4000
PS = 288  # partition start
PL = NBLK - PS - 300
img = bytearray(NBLK * bs)
# Non-zero fill, so that unrecorded extents read from the disc instead of as
# zeros would show.
img[PS * bs:(PS + PL) * bs] = b"\xA5" * (PL * bs)

def wr_abs(block, data):
    img[block * bs:block * bs + len(data)] = data

class Phys:
    ref = 0
    def __init__(self):
        self.next = 40
    def alloc(self, n, gap=None):
        l = self.next
        self.next += n + (random.randint(1, 3) if gap is None else gap)
        return l
    def write(self, lbn, data):
        pad = (-len(data)) % bs
        wr_abs(PS + lbn, data + bytes(pad))
    def tagloc(self, lbn):
        return lbn

phys = Phys()

class Meta:
    """metadata partition: meta lbn i lives at physical lbn self.map[i]"""
    ref = 1
    def __init__(self, n):
        # two non-contiguous physical extents back the metadata file
        a = phys.alloc(n // 2)
        phys.alloc(7)
        b = phys.alloc(n - n // 2)
        self.extents = [(a, n // 2), (b, n - n // 2)]
        self.map = [a + i for i in range(n // 2)] + [b + i for i in range(n - n // 2)]
        self.next = 0
        for p in self.map:
            phys.write(p, bytes(bs))
    def alloc(self, n, gap=None):
        l = self.next
        self.next += n + (1 if gap is None else gap)
        assert self.next <= len(self.map)
        return l
    def write(self, lbn, data):
        for i in range(0, len(data), bs):
            phys.write(self.map[lbn + i // bs], data[i:i + bs])
    def tagloc(self, lbn):
        return lbn

class Virt:
    ref = 1
    def __init__(self):
        self.vat = []
    def alloc(self, n, gap=None):
        assert n == 1
        p = phys.alloc(1)
        idx = len(self.vat)
        self.vat.append(p)
        if len(self.vat) % 3 == 0:
            self.vat.append(0xFFFFFFFF)  # unused virtual entry
        return idx
    def write(self, lbn, data):
        assert len(data) <= bs
        phys.write(self.vat[lbn], data)
    def tagloc(self, lbn):
        return lbn

if mode == "meta":
    ms = Meta(900)
elif mode.startswith("vat"):
    ms = Virt()
else:
    ms = phys

def short_ad(length, pos, typ=0):
    return struct.pack("<II", length | typ << 30, pos)

def long_ad(length, lbn, ref, typ=0):
    return struct.pack("<IIH6x", length | typ << 30, lbn, ref)

def ext_ad(length, lbn, ref, typ=0):
    return struct.pack("<IIIIH2x", length | typ << 30, length, length, lbn, ref)

def icbtag(ftype, adtype, extra_flags=0):
    return struct.pack("<IHHHBB6sH", 0, 4, 0, 1, 0, ftype, bytes(6), adtype | extra_flags)

uniq = [16]

def file_entry(lbn, ftype, adtype, info_len, ads, ea=b"", links=1, space=None):
    space = space or ms
    uniq[0] += 1
    if EFE:
        body = icbtag(ftype, adtype) + struct.pack("<IIIHBBI", 0, 0, 0x7FFF, links, 0, 0, 0)
        body += struct.pack("<QQQ", info_len, info_len, (info_len + bs - 1) // bs) + TS * 4
        body += struct.pack("<II", 1, 0) + bytes(16) + bytes(16) + regid("*unretro test") + struct.pack("<QII", uniq[0], len(ea), len(ads))
        tid = 266
    else:
        body = icbtag(ftype, adtype) + struct.pack("<IIIHBBI", 0, 0, 0x7FFF, links, 0, 0, 0)
        body += struct.pack("<QQ", info_len, (info_len + bs - 1) // bs) + TS * 3
        body += struct.pack("<I", 1) + bytes(16) + regid("*unretro test") + struct.pack("<QII", uniq[0], len(ea), len(ads))
        tid = 261
    body += ea + ads
    d = tagged(tid, body, space.tagloc(lbn))
    assert len(d) <= bs, (len(d), "FE too big")
    space.write(lbn, d)

def fid(name, icb_lbn, chars=0, iu=b""):
    nm = osta(name) if name is not None else b""
    body = struct.pack("<HBB", 1, chars, len(nm)) + long_ad(bs, icb_lbn, ms.ref) + struct.pack("<H", len(iu)) + iu + nm
    body += bytes((-(16 + len(body))) % 4)
    return body  # tag added when position known

expected = {}

def data_ad_kind():
    return 1 if ms is not phys else 0

def store_file(path, kind):
    """returns icb lbn"""
    icb = ms.alloc(1)
    force_long = ms is not phys
    if kind == "embedded":
        data = b"embedded in the ICB: " + path.encode() + b"\n"
        file_entry(icb, 5, 3, len(data), data)
    elif kind == "embedded_ea":
        data = b"embedded after extended attributes\n"
        ea = bytes(52)  # opaque EA space
        file_entry(icb, 5, 3, len(data), data, ea=ea)
    elif kind == "empty":
        data = b""
        file_entry(icb, 5, 0 if not force_long else 1, 0, b"")
    elif kind in ("short", "long", "ext", "cont"):
        n = {"short": 5, "long": 4, "ext": 3, "cont": 70}[kind]
        if bs >= 2048 and kind == "cont":
            n = 300
        data = b""
        ads = []
        for i in range(n):
            ln = bs * random.randint(1, 3) if i < n - 1 else random.randint(1, 2 * bs - 1)
            chunk = filler(ln)
            l = phys.alloc((ln + bs - 1) // bs)
            phys.write(l, chunk)
            data += chunk
            if kind == "ext":
                ads.append(ext_ad(ln, l, 0))
            elif kind == "long" or force_long:
                ads.append(long_ad(ln, l, 0))
            else:
                ads.append(short_ad(ln, l))
        adtype = 2 if kind == "ext" else (1 if kind == "long" or force_long else 0)
        adsz = len(ads[0])
        if kind == "cont":
            # first 3 ADs in the FE, then chained allocation extent descriptors (in the ICB's partition)
            first, rest = ads[:3], ads[3:]
            per = (bs - 24) // adsz - 1
            chunks = [rest[i:i + per] for i in range(0, len(rest), per)]
            lbns = [ms.alloc(1) for _ in chunks]
            def cont_ad(l):
                if adtype == 1:
                    return long_ad(bs, l, ms.ref, 3)
                return short_ad(bs, l, 3)
            for i, ch in enumerate(chunks):
                a = b"".join(ch) + (cont_ad(lbns[i + 1]) if i + 1 < len(chunks) else b"")
                ms.write(lbns[i], tagged(258, struct.pack("<II", 0, len(a)) + a, ms.tagloc(lbns[i])))
            file_entry(icb, 5, adtype, len(data), b"".join(first) + cont_ad(lbns[0]))
        else:
            file_entry(icb, 5, adtype, len(data), b"".join(ads))
    elif kind == "sparse":
        mk = (lambda ln, l, t: long_ad(ln, l, 0, t)) if force_long else (lambda ln, l, t: short_ad(ln, l, t))
        a = filler(bs * 2)
        la = phys.alloc(2); phys.write(la, a)
        lb = phys.alloc(3)  # allocated, not recorded: garbage on disc, must read as zeros
        c = filler(bs + 17)
        lc = phys.alloc(2); phys.write(lc, c)
        data = a + bytes(bs * 5) + bytes(bs * 3) + c
        ads = mk(bs * 2, la, 0) + mk(bs * 5, 0, 2) + mk(bs * 3, lb, 1) + mk(bs + 17, lc, 0)
        file_entry(icb, 5, 1 if force_long else 0, len(data), ads)
    elif kind == "sparse_tail":
        # file ends in an unallocated extent
        mk = (lambda ln, l, t: long_ad(ln, l, 0, t)) if force_long else (lambda ln, l, t: short_ad(ln, l, t))
        a = filler(bs)
        la = phys.alloc(1); phys.write(la, a)
        data = a + bytes(bs * 2 + 100)
        file_entry(icb, 5, 1 if force_long else 0, len(data), mk(bs, la, 0) + mk(bs * 2 + 100, 0, 2))
    expected[path] = data
    return icb

def build_dir(icb, parent_icb, entries, embed=False):
    bodies = [fid(None, parent_icb, chars=0x0A)]
    for name, cicb, chars, iu in entries:
        bodies.append(fid(name, cicb, chars, iu))
    total = sum(16 + len(b) for b in bodies)
    if embed:
        blob = b""
        for b in bodies:
            blob += tagged(257, b, ms.tagloc(icb))
        file_entry(icb, 4, 3, len(blob), blob)
        return
    nb = (total + bs - 1) // bs
    if mode.startswith("vat"):
        space, mk, adtype = phys, (lambda ln, l: long_ad(ln, l, 0)), 1
    else:
        space, mk, adtype = ms, (lambda ln, l: short_ad(ln, l)), 0
    # split the directory into two non-contiguous extents when it is large enough
    if nb >= 2:
        n1 = nb // 2
        l1 = space.alloc(n1, gap=2); l2 = space.alloc(nb - n1)
        exts = [(l1, n1), (l2, nb - n1)]
    else:
        exts = [(space.alloc(nb), nb)]
    blockmap = [l + i for l, n in exts for i in range(n)]
    blob = b""
    for b in bodies:
        blob += tagged(257, b, space.tagloc(blockmap[len(blob) // bs]))
    for i in range(nb):
        space.write(blockmap[i], blob[i * bs:(i + 1) * bs])
    ads = b""
    rem = total
    for l, n in exts:
        ln = min(rem, n * bs); rem -= ln
        ads += mk(ln, l)
    file_entry(icb, 4, adtype, total, ads)

root = ms.alloc(1)
fsd_lbn = None

def mkdir_tree():
    ents = []
    for nm, kind in [("embedded.txt", "embedded"), ("embedded_ea.txt", "embedded_ea"), ("empty", "empty"),
                     ("short.bin", "short"), ("long.bin", "long"), ("ext.bin", "ext"), ("cont.bin", "cont"),
                     ("sparse.bin", "sparse"), ("sparse_tail.bin", "sparse_tail")]:
        if mode.startswith("vat") and kind == "cont":
            continue
        ents.append((nm, store_file(nm, kind), 0, b""))
    # hidden file, impl-use in FID
    ents.append(("hidden.txt", store_file("hidden.txt", "embedded"), 1, b"IMPLUSE!"))
    # deleted entry: must not be listed
    ents.append(("deleted.txt", store_file("deleted.txt", "embedded"), 4, b""))
    del expected["deleted.txt"]
    # hard link: second name for short.bin
    ents.append(("hardlink.bin", ents[3][1], 0, b""))
    expected["hardlink.bin"] = expected["short.bin"]
    # 16-bit dir with nested content
    d16 = ms.alloc(1)
    dn = "Dir16 日本語 \U0001F600"
    sub = ms.alloc(1)
    e2 = [("café 8bit.txt", store_file(dn + "/café 8bit.txt", "embedded"), 0, b""),
          ("кириллица.bin", store_file(dn + "/кириллица.bin", "short"), 0, b""),
          ("embdir", sub, 2, b"")]
    build_dir(d16, root, e2)
    build_dir(sub, d16, [("inner.txt", store_file(dn + "/embdir/inner.txt", "embedded"), 0, b"")], embed=True)
    ents.append((dn, d16, 2, b""))
    # big directory: FIDs crossing block boundaries
    big = ms.alloc(1)
    e3 = []
    for i in range(120):
        nm = "file with a fairly long name number %03d.dat" % i
        e3.append((nm, store_file("Many/" + nm, "embedded" if i % 2 else "empty"), 0, b"x" * (i % 5)))
    build_dir(big, root, e3)
    ents.append(("Many", big, 2, b""))
    # empty directory (not listed as a leaf)
    ed = ms.alloc(1)
    build_dir(ed, root, [], embed=True)
    ents.append(("EmptyDir", ed, 2, b""))
    # deep chain
    prev_icb, prev_path = None, ""
    chain = []
    top = ms.alloc(1)
    cur, cur_parent, path = top, root, "deep"
    for i in range(12):
        nxt = ms.alloc(1)
        build_dir(cur, cur_parent, [("d%d" % i, nxt, 2, b"")], embed=(i % 2 == 0))
        cur_parent, cur = cur, nxt
        path += "/d%d" % i
    build_dir(cur, cur_parent, [("bottom.txt", store_file(path + "/bottom.txt", "embedded"), 0, b"")], embed=True)
    ents.append(("deep", top, 2, b""))
    build_dir(root, root, ents)

fsd = ms.alloc(1)
mkdir_tree()
body = TS + struct.pack("<HHIIII", 3, 3, 1, 1, 0, 0) + charspec() + dstring("SYNTH", 128) + charspec()
body += dstring("FileSet", 32) + bytes(32) + bytes(32) + long_ad(bs, root, ms.ref) + regid("*OSTA UDF Compliant", bytes([0x50 if mode == "meta" else 0x01, 2 if mode != "phys" else 1]))
body += bytes(16) + bytes(16) + bytes(32)
ms.write(fsd, tagged(256, body, ms.tagloc(fsd)))

# ---- partition maps
maps = struct.pack("<BBHH", 1, 6, 1, 0)
last_block = NBLK - 1
spare_note = ""
if mode == "meta":
    def meta_file(ftype, corrupt=False):
        l = phys.alloc(1)
        ads = b"".join(short_ad(n * bs, a) for a, n in ms.extents)
        file_entry(l, ftype, 0, len(ms.map) * bs, ads, space=phys)
        if corrupt:
            img[(PS + l) * bs + 40] ^= 0xFF
        return l
    mf = meta_file(250, corrupt="badmain" in variant)
    mm = meta_file(251)
    maps += struct.pack("<BB2x", 2, 64) + regid("*UDF Metadata Partition", b"\x50\x02") + struct.pack("<HHIIIIHB5x", 1, 0, mf, mm, 0xFFFFFFFF, 32, 1, 0)
elif mode.startswith("vat"):
    maps += struct.pack("<BB2x", 2, 64) + regid("*UDF Virtual Partition", b"\x50\x01") + struct.pack("<HH24x", 1, 0)
    ent = b"".join(struct.pack("<I", v) for v in ms.vat)
    if mode == "vat20":
        vat = struct.pack("<HH", 152, 0) + dstring("SYNTH", 128) + struct.pack("<IIIHHHH", 0xFFFFFFFF, 100, 20, 0x0200, 0x0200, 0x0200, 0) + ent
        ftype = 248
    else:
        vat = ent + regid("*UDF Virtual Alloc Tbl", b"\x50\x01") + struct.pack("<I", 0xFFFFFFFF)
        ftype = 0
    nb = (len(vat) + bs - 1) // bs
    vl = phys.alloc(nb, gap=0)
    phys.write(vl, vat)
    # VAT ICB is the last recorded block of the medium
    icb = phys.alloc(1, gap=0)
    file_entry(icb, ftype, 0, len(vat), short_ad(len(vat), vl), space=phys)
    last_block = PS + icb
elif mode == "spar":
    PKT = 32
    # remap two packets that hold live data into the spare area (outside the partition)
    used = phys.next
    victims = [(root // PKT) * PKT, ((used // 2) // PKT) * PKT]
    victims = sorted(set(victims))
    spare0 = PS + PL + 8
    entries = b""
    for i, v in enumerate(victims):
        dst = spare0 + i * PKT
        img[dst * bs:(dst + PKT) * bs] = img[(PS + v) * bs:(PS + v + PKT) * bs]
        img[(PS + v) * bs:(PS + v + PKT) * bs] = filler(PKT * bs)
        entries += struct.pack("<II", v, dst)
    # unused spare entries
    for i in range(len(victims), 6):
        entries += struct.pack("<II", 0xFFFFFFFF, spare0 + i * PKT)
    st = tagged(0, regid("*UDF Sparing Table", b"\x50\x01") + struct.pack("<H2xI", 6, 1) + entries, PS + PL + 2)
    wr_abs(PS + PL + 2, st)
    st2 = tagged(0, regid("*UDF Sparing Table", b"\x50\x01") + struct.pack("<H2xI", 6, 1) + entries, PS + PL + 4)
    wr_abs(PS + PL + 4, st2)
    stsize = (len(st) + bs - 1) // bs * bs
    maps = struct.pack("<BB2x", 2, 64) + regid("*UDF Sparable Partition", b"\x50\x01") + struct.pack("<HHHBxI4I", 1, 0, PKT, 2, len(st), PS + PL + 2, PS + PL + 4, 0, 0)
    spare_note = "remapped packets at lbn %s" % victims

nmaps = 1 if mode in ("phys", "spar") else 2

# ---- volume descriptor sequence
def vds(start):
    b = start
    pvd = struct.pack("<II", 1, 0) + dstring("SYNTH", 32) + struct.pack("<HHHHII", 1, 1, 2, 2, 1, 1) + dstring("0123456789ABCDEFsynth", 128)
    pvd += charspec() + charspec() + bytes(8) + bytes(8) + bytes(32) + TS + regid("*unretro test") + bytes(64) + struct.pack("<IH22x", 0, 0)
    wr_abs(b, tagged(1, pvd, b)); b += 1
    pd = struct.pack("<IHH", 2, 1, 0) + regid("+NSR03" if NSR3 else "+NSR02") + bytes(128) + struct.pack("<III", 1 if mode.startswith("vat") else 4, PS, PL) + regid("*unretro test") + bytes(128) + bytes(156)
    wr_abs(b, tagged(5, pd, b)); b += 1
    lvd = struct.pack("<I", 3) + charspec() + dstring("SYNTH", 128) + struct.pack("<I", bs) + regid("*OSTA UDF Compliant", b"\x50\x02") + long_ad(bs, fsd, ms.ref)
    lvd += struct.pack("<II", len(maps), nmaps) + regid("*unretro test") + bytes(128) + struct.pack("<II", 0, 0) + maps
    d = tagged(6, lvd, b)
    assert len(d) <= bs
    wr_abs(b, d); b += 1
    wr_abs(b, tagged(7, struct.pack("<II", 4, 0), b)); b += 1
    wr_abs(b, tagged(8, bytes(496), b)); b += 1

vds_blocks = max(16, (16 * 2048) // bs)
MV = 32 * 2048 // bs
RV = MV + vds_blocks
assert RV + vds_blocks <= 256
vds(MV); vds(RV)
def avdp(b):
    wr_abs(b, tagged(2, struct.pack("<IIII", vds_blocks * bs, MV, vds_blocks * bs, RV) + bytes(480), b))
avdp(256)
if mode.startswith("vat"):
    img = img[:(last_block + 1) * bs]
else:
    avdp(NBLK - 1)
    if "n257" in variant:
        avdp(NBLK - 257)
# VRS
for i, s in enumerate([b"BEA01", b"NSR03" if NSR3 else b"NSR02", b"TEA01"]):
    o = 32768 + i * max(2048, bs)
    img[o:o + 2048] = b"\0" + s + b"\x01" + bytes(2041)

assert phys.next < PL
open(out, "wb").write(img)
for p, d in expected.items():
    print("%s\t%s\t%s\t%d\t%016x" % (setname, "data" if d else "empty", p, len(d), fnv1a(d)))
print("%s\tdir\tEmptyDir\t0\t%016x" % (setname, fnv1a(b"")))
print(out, mode, bs, "files", len(expected), "blocks used", phys.next, "of", PL, spare_note, file=sys.stderr)
