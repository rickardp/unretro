#!/usr/bin/env python3
"""Layout steps of the Toast fixtures that no packaged tool performs.

    make_layouts.py apm OUT [options] TYPE,NAME,START,SOURCE ...
    make_layouts.py carve IMAGE INDEX OUT
    make_layouts.py frame m1|m2|m2-2336|xa2056 COOKED OUT
    make_layouts.py cook  m1|m2|m2-2336|xa2056 FRAMED OUT

`apm` writes a driver descriptor record and an Apple Partition Map around
existing volume images, optionally on top of an existing image (an ISO 9660
volume, whose first 16 sectors are unused). `carve` copies one partition out
of an image. `frame` turns 2048-byte sectors into raw 2352-byte Mode 1 or
Mode 2 Form 1 sectors, or into the 2336- and 2056-byte parts of a Mode 2
Form 1 sector; `cook` is the inverse and takes nothing on trust from `frame`
but the offsets.

Written from Inside Macintosh: Devices (partition map entry and driver
descriptor record) and ECMA-130 (sector layout, clause 14 and annex A).
"""
import argparse
import struct
import sys

SECTOR = 2048
SYNC = b"\x00" + b"\xff" * 10 + b"\x00"


# --- Apple Partition Map -------------------------------------------------

def map_entry(unit, count, start, blocks, name, kind, status):
    name = name.encode("mac_roman")
    kind = kind.encode("ascii")
    assert len(name) <= 32 and len(kind) <= 32
    entry = struct.pack(">2sHIII32s32sIII", b"PM", 0, count, start, blocks,
                        name, kind, 0, blocks, status)
    return entry.ljust(unit, b"\0")


def build_apm(args):
    unit = args.unit
    image = bytearray(open(args.base, "rb").read()) if args.base else bytearray()
    parts = []
    for spec in args.part:
        kind, name, start, source = spec.split(",", 3)
        if source.startswith("@"):
            data = None
            length = int(source[1:]) * unit
        else:
            data = open(source, "rb").read()
            length = len(data)
        assert length % unit == 0, "%s is not a whole number of blocks" % source
        parts.append((kind, name, int(start), length // unit, data))
    count = 1 + len(parts)
    assert count <= args.map_blocks
    entries = [map_entry(unit, count, 1, args.map_blocks, args.map_name,
                         "Apple_partition_map", 0)]
    end = 1 + args.map_blocks
    for kind, name, start, blocks, data in parts:
        assert start >= end, "partition %r overlaps" % name
        status = 0 if kind == "Apple_Free" else 0x33
        entries.append(map_entry(unit, count, start, blocks, name, kind, status))
        end = start + blocks
        if len(image) < end * unit:
            image.extend(bytes(end * unit - len(image)))
        if data is not None:
            image[start * unit:end * unit] = data
    head = b"".join(entries)
    assert not any(image[:unit + len(head)]), "no room for the map"
    # Driver descriptor record: signature, device block size, device blocks.
    device_blocks = end * unit // args.ddm_block
    image[0:8] = struct.pack(">2sHI", b"ER", args.ddm_block, device_blocks)
    image[unit:unit + len(head)] = head
    image.extend(bytes(-len(image) % args.align))
    image.extend(bytes(args.tail))
    open(args.out, "wb").write(image)


def carve(args):
    image = open(args.image, "rb").read()
    assert image[:2] == b"ER"
    declared = struct.unpack(">H", image[2:4])[0]
    unit = 512 if image[512:514] == b"PM" else declared
    entry = image[unit * (1 + args.index):unit * (2 + args.index)]
    assert entry[:2] == b"PM"
    start, blocks = struct.unpack(">II", entry[8:16])
    open(args.out, "wb").write(image[start * unit:(start + blocks) * unit])


# --- CD sectors (ECMA-130) -----------------------------------------------

def _edc_table():
    table = []
    for value in range(256):
        for _ in range(8):
            value = (value >> 1) ^ (0xD8018001 if value & 1 else 0)
        table.append(value)
    return table


def _field():
    # GF(2^8), primitive polynomial x^8 + x^4 + x^3 + x^2 + 1.
    exp = [0] * 510
    log = [0] * 256
    value = 1
    for power in range(255):
        exp[power] = exp[power + 255] = value
        log[value] = power
        value <<= 1
        if value & 0x100:
            value ^= 0x11D
    return exp, log


EDC_TABLE = _edc_table()
EXP, LOG = _field()


def edc(data):
    value = 0
    for byte in data:
        value = (value >> 8) ^ EDC_TABLE[(value ^ byte) & 0xFF]
    return struct.pack("<I", value)


def _parity(data):
    """The two parity bytes that complete a Reed-Solomon codeword whose
    check matrix has the rows (1, ..., 1) and (a^(n-1), ..., a, 1)."""
    n = len(data) + 2
    s0 = 0
    s1 = 0
    for i, byte in enumerate(data):
        s0 ^= byte
        if byte:
            s1 ^= EXP[LOG[byte] + (n - 1 - i) % 255]
    # p0 + p1 = s0 and a*p0 + p1 = s1, so (a + 1)*p0 = s0 + s1.
    total = s0 ^ s1
    p0 = EXP[LOG[total] + 255 - LOG[3]] if total else 0
    return p0, s0 ^ p0


def ecc(sector):
    """P and Q parity over bytes 12..2075, filled in at 2076 and 2248."""
    for plane in (0, 1):
        at = lambda m: 12 + 2 * m + plane
        for j in range(43):
            p0, p1 = _parity([sector[at(j + 43 * i)] for i in range(24)])
            sector[at(1032 + j)] = p0
            sector[at(1075 + j)] = p1
        for k in range(26):
            q0, q1 = _parity([sector[at((44 * i + 43 * k) % 1118)] for i in range(43)])
            sector[at(1118 + k)] = q0
            sector[at(1144 + k)] = q1


def bcd(value):
    return (value // 10) << 4 | value % 10


def raw_sector(mode, number, user, last):
    address = number + 150
    sector = bytearray(2352)
    sector[0:12] = SYNC
    sector[12:16] = bytes([bcd(address // 4500), bcd(address // 75 % 60),
                           bcd(address % 75), mode])
    if mode == 1:
        sector[16:2064] = user
        sector[2064:2068] = edc(sector[:2064])
        ecc(sector)
    else:
        # Form 1 sub-header, twice: data sector, end of file on the last.
        sector[16:24] = (b"\x00\x00\x89\x00" if last else b"\x00\x00\x08\x00") * 2
        sector[24:2072] = user
        sector[2072:2076] = edc(sector[16:2072])
        # Mode 2 parity is computed with the address field taken as zero.
        header = bytes(sector[12:16])
        sector[12:16] = bytes(4)
        ecc(sector)
        sector[12:16] = header
    return bytes(sector)


# (bytes of the raw Mode 1 or Mode 2 Form 1 sector kept, offset of user data)
FRAMINGS = {
    "m1": (1, 0, 2352, 16),
    "m2": (2, 0, 2352, 24),
    "m2-2336": (2, 16, 2352, 8),
    "xa2056": (2, 16, 2072, 8),
}


def frame(args):
    mode, first, last, _ = FRAMINGS[args.framing]
    cooked = open(args.source, "rb").read()
    assert len(cooked) % SECTOR == 0
    count = len(cooked) // SECTOR
    with open(args.out, "wb") as out:
        for n in range(count):
            user = cooked[n * SECTOR:(n + 1) * SECTOR]
            out.write(raw_sector(mode, n, user, n == count - 1)[first:last])


def cook(args):
    _, first, last, offset = FRAMINGS[args.framing]
    size = last - first
    framed = open(args.source, "rb").read()
    assert len(framed) % size == 0
    with open(args.out, "wb") as out:
        for at in range(0, len(framed), size):
            out.write(framed[at + offset:at + offset + SECTOR])


def main():
    parser = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    sub = parser.add_subparsers(dest="command", required=True)

    p = sub.add_parser("apm")
    p.add_argument("out")
    p.add_argument("part", nargs="+", help="TYPE,NAME,START,FILE or TYPE,NAME,START,@BLOCKS")
    p.add_argument("--base", help="image to write the map into (default: empty)")
    p.add_argument("--ddm-block", type=int, default=512, help="block size the driver descriptor declares")
    p.add_argument("--unit", type=int, default=512, help="size of a map entry and unit of its block numbers")
    p.add_argument("--map-name", default="MRKS")
    p.add_argument("--map-blocks", type=int, default=2)
    p.add_argument("--align", type=int, default=SECTOR, help="pad the image to a multiple of this")
    p.add_argument("--tail", type=int, default=0, help="zero bytes appended after alignment")
    p.set_defaults(run=build_apm)

    p = sub.add_parser("carve")
    p.add_argument("image")
    p.add_argument("index", type=int, help="zero-based map entry")
    p.add_argument("out")
    p.set_defaults(run=carve)

    for name, run in (("frame", frame), ("cook", cook)):
        p = sub.add_parser(name)
        p.add_argument("framing", choices=sorted(FRAMINGS))
        p.add_argument("source")
        p.add_argument("out")
        p.set_defaults(run=run)

    args = parser.parse_args()
    args.run(args)


if __name__ == "__main__":
    sys.exit(main())
