#!/usr/bin/env python3
"""Writes a synthetic High Sierra CD-ROM image.

Every offset below is taken from the working paper of the CDROM Ad Hoc
Advisory Committee, "Working Paper for Information Processing - Volume and
File Structure of Compact Read Only Optical Discs for Information
Interchange", 28 May 1986. Section numbers in the comments are its sections;
BP is the paper's byte position, counted from 1.

    make_highsierra.py <image> [logical block size, default 2048]

The expected listing is printed as tab-separated `path size fnv64` rows, with
names the way this crate presents plain ISO 9660 names (lower case, no
version, no trailing full stop).
"""

import sys

SECTOR = 2048

# 13.1.6 File Flags
EXISTENCE = 0x01
DIRECTORY = 0x02
ASSOCIATED = 0x04
MULTI_EXTENT = 0x80

RECORDED = (87, 6, 15, 12, 30, 0)  # 13.1.5: years since 1900, month, day, h, m, s
VOLUME_DATE = b"1987061512300000"  # 11.4.30.1: 16 digits, no time zone


def both16(value):  # 10.2.3
    return value.to_bytes(2, "little") + value.to_bytes(2, "big")


def both32(value):  # 10.3.3
    return value.to_bytes(4, "little") + value.to_bytes(4, "big")


def fnv1a(data):
    value = 0xCBF29CE484222325
    for byte in data:
        value = ((value ^ byte) * 0x100000001B3) & 0xFFFFFFFFFFFFFFFF
    return value


def text(tag, lines):
    words = "alpine boulder cirque divide escarpment foothill granite".split()
    return "".join(
        f"{tag} {i:03d} {words[i % len(words)]} {words[(i * 3 + 1) % len(words)]}\r\n"
        for i in range(lines)
    ).encode("ascii")


def counter(length, seed):
    return bytes((seed + i * 5 + (i >> 8)) & 0xFF for i in range(length))


def directory_record(identifier, lbn, length, flags, ear_blocks=0):
    """13.1 Format of a Directory Record."""
    record = bytearray(33)
    record[1] = ear_blocks  # BP 2 Extended Attribute Record Length
    record[2:10] = both32(lbn)  # BP 3-10 Location of Extent
    record[10:18] = both32(length)  # BP 11-18 Data Length
    record[18:24] = bytes(RECORDED)  # BP 19-24 Recording Date and Time
    record[24] = flags  # BP 25 File Flags
    record[25] = 0  # BP 26 reserved
    record[26] = 0  # BP 27 Interleave Size
    record[27] = 0  # BP 28 Interleave Skip Factor
    record[28:32] = both16(1)  # BP 29-32 Volume Set Sequence Number
    record[32] = len(identifier)  # BP 33 LEN_FI
    record += identifier  # BP 34...
    if len(identifier) % 2 == 0:
        record.append(0)  # 13.1.13: present when LEN_FI is even
    record[0] = len(record)  # BP 1 LEN_DR
    return bytes(record)


def path_table_record(identifier, lbn, parent, order):
    """12.1.1 Format of a Path Table Record."""
    record = bytearray()
    record += lbn.to_bytes(4, order)  # BP 1-4 Location of Extent
    record.append(0)  # BP 5 Extended Attribute Record Length
    record.append(len(identifier))  # BP 6 LEN_DI
    record += parent.to_bytes(2, order)  # BP 7-8 Parent Directory Number
    record += identifier  # BP 9...
    if len(identifier) % 2 == 1:
        record.append(0)  # 12.1.1.6: present when LEN_DI is odd
    return bytes(record)


class Dir:
    def __init__(self, name, parent):
        self.name = name
        self.parent = parent or self
        self.dirs = []
        self.files = []  # (identifier, flags, [(data, ear_blocks)])
        self.sector = 0
        self.size = 0
        self.number = 0

    def path(self):
        if self.parent is self:
            return ""
        return self.parent.path() + self.name.decode().lower() + "/"


class Image:
    def __init__(self, block_size):
        self.block_size = block_size
        self.per_sector = SECTOR // block_size
        self.sectors = {}
        self.next_sector = 20  # 16 and 17 descriptors, 18 and 19 path tables
        self.expected = {}

    def lbn(self, sector):
        return sector * self.per_sector

    def put(self, sector, data):
        for i in range(0, len(data), SECTOR):
            self.sectors[sector + i // SECTOR] = data[i : i + SECTOR].ljust(SECTOR, b"\0")

    def allocate(self, sectors):
        first = self.next_sector
        self.next_sector += sectors
        return first

    def store(self, data, ear_blocks=0):
        """Returns the sector of an extent: `ear_blocks` logical blocks of
        Extended Attribute Record (13.2), then the data."""
        lead = bytes([0xEA]) * (ear_blocks * self.block_size)
        whole = lead + data
        first = self.allocate(max(1, -(-len(whole) // SECTOR)))
        self.put(first, whole)
        return first


def build(block_size):
    image = Image(block_size)
    root = Dir(b"", None)
    docs = Dir(b"DOCS", root)
    deep = Dir(b"DEEP", docs)
    many = Dir(b"MANY", root)
    root.dirs = [docs, many]
    docs.dirs = [deep]

    def add(directory, identifier, pieces, flags=0, listed=True):
        directory.files.append((identifier, flags, pieces))
        if listed:
            name = identifier.decode().split(";")[0].rstrip(".").lower()
            data = b"".join(piece for piece, _ in pieces)
            image.expected[directory.path() + name] = data

    add(root, b"AUTOEXEC.BAT;1", [(text("rem high sierra", 4), 0)])
    add(root, b"BIG.BIN;1", [(counter(5000, 11), 0)])
    add(root, b"EMPTY.DAT;1", [(b"", 0)])
    # 13.1.6 bit 0: need not be made known to the user. Iso9660Container
    # lists such files on ISO 9660 volumes.
    add(root, b"HIDDEN.TXT;1", [(text("existence bit", 2), 0)], flags=EXISTENCE)
    # 10.5.2.5: the full stop is optional without an extension; here it is
    # recorded.
    add(root, b"NOEXT.;1", [(text("no extension", 3), 0)])
    add(root, b"README.TXT;1", [(text("high sierra readme", 30), 0)])
    # 8.3.2: the Associated File record sorts after the file of the same name.
    add(root, b"RESOURCE.DAT;1", [(counter(700, 40), 0)])
    add(root, b"RESOURCE.DAT;1", [(b"associated file, not listed\r\n", 0)],
        flags=ASSOCIATED, listed=False)
    # 13.1.6 bit 7: two extents, the first in a record with Multi-Extent set.
    add(root, b"SPLIT.BIN;1", [(counter(4096, 70), 0), (counter(1500, 71), 0)])
    # 13.1.2: one logical block of Extended Attribute Record before the data.
    add(root, b"XATTR.TXT;1", [(text("after an extended attribute record", 5), 1)])
    add(docs, b"GUIDE.TXT;1", [(text("guide", 12), 0)])
    add(deep, b"NESTED.TXT;1", [(text("nested", 2), 0)])
    for i in range(60):
        add(many, b"F%02d.TXT;1" % i, [(b"many %02d\r\n" % i, 0)])

    # Path table order (8.2.1): level, then parent number, then identifier.
    ordered = [root]
    for directory in ordered:
        ordered.extend(sorted(directory.dirs, key=lambda d: d.name))
    for number, directory in enumerate(ordered, 1):
        directory.number = number

    # Directory sizes first: records never cross a sector (8.3.1).
    def records(directory, resolve):
        out = [
            directory_record(b"\x00", *resolve(directory), DIRECTORY),  # 10.6.3.2
            directory_record(b"\x01", *resolve(directory.parent), DIRECTORY),  # 10.6.3.3
        ]
        entries = [(d.name, d) for d in directory.dirs]
        entries += [(f[0], f) for f in directory.files]
        # Stable sort: the associated record stays behind its file.
        for name, entry in sorted(entries, key=lambda e: e[0]):
            if isinstance(entry, Dir):
                out.append(directory_record(name, *resolve(entry), DIRECTORY))
                continue
            _, flags, pieces = entry
            for index, (data, ear_blocks) in enumerate(pieces):
                last = index == len(pieces) - 1
                sector = resolve((id(entry), index))
                out.append(
                    directory_record(
                        name,
                        image.lbn(sector),
                        len(data),
                        flags | (0 if last else MULTI_EXTENT),
                        ear_blocks,
                    )
                )
        return out

    def lay_out(recs):
        data = bytearray()
        for record in recs:
            if len(data) % SECTOR + len(record) > SECTOR:
                data += bytes(SECTOR - len(data) % SECTOR)
            data += record
        data += bytes(-len(data) % SECTOR)  # 8.3.3
        return bytes(data)

    placeholder = lambda _: (0, 0) if isinstance(_, Dir) else 0
    for directory in ordered:
        directory.size = len(lay_out(records(directory, placeholder)))
        directory.sector = image.allocate(directory.size // SECTOR)

    # File extents. The two extents of SPLIT.BIN are kept apart by storing
    # every first piece before any second piece.
    extent_sector = {}
    for index in (0, 1):
        for directory in ordered:
            for entry in directory.files:
                pieces = entry[2]
                if index < len(pieces):
                    data, ear_blocks = pieces[index]
                    extent_sector[(id(entry), index)] = image.store(data, ear_blocks)

    def resolve(target):
        if isinstance(target, Dir):
            return image.lbn(target.sector), target.size
        return extent_sector[target]

    for directory in ordered:
        image.put(directory.sector, lay_out(records(directory, resolve)))

    tables = {}
    for order in ("little", "big"):
        table = b"".join(
            path_table_record(d.name or b"\x00", image.lbn(d.sector), d.parent.number, order)
            for d in ordered
        )
        tables[order] = table
    image.put(18, tables["little"])
    image.put(19, tables["big"])

    total_sectors = image.next_sector

    # 11.4 Standard File Structure Volume Descriptor
    vd = bytearray(SECTOR)
    vd[0:8] = both32(image.lbn(16))  # BP 1-8 Volume Descriptor LBN
    vd[8] = 1  # BP 9 Volume Descriptor Type
    vd[9:14] = b"CDROM"  # BP 10-14 Volume Structure Standard Identifier
    vd[14] = 1  # BP 15 Volume Structure Standard Version
    vd[16:48] = b"UNRETRO TEST".ljust(32)  # BP 17-48 System Identifier
    vd[48:80] = b"HIGH_SIERRA".ljust(32)  # BP 49-80 Volume Identifier
    vd[88:96] = both32(image.lbn(total_sectors))  # BP 89-96 Volume Space Size
    vd[128:132] = both16(1)  # BP 129-132 Volume Set Size
    vd[132:136] = both16(1)  # BP 133-136 Volume Set Sequence Number
    vd[136:140] = both16(block_size)  # BP 137-140 Logical Block Size
    vd[140:148] = both32(len(tables["little"]))  # BP 141-148 Path Table Size
    vd[148:152] = image.lbn(18).to_bytes(4, "little")  # BP 149-152, 10.3.1
    vd[164:168] = image.lbn(19).to_bytes(4, "big")  # BP 165-168, 10.3.2
    root_record = directory_record(b"\x00", image.lbn(root.sector), root.size, DIRECTORY)
    assert len(root_record) == 34
    vd[180:214] = root_record  # BP 181-214 Directory Record for Root Directory
    vd[214:342] = b"UNRETRO_FIXTURES".ljust(128)  # BP 215-342 Volume Set Identifier
    vd[342:470] = b" " * 128  # BP 343-470 Publisher Identifier
    vd[470:598] = b"MAKE_HIGHSIERRA.PY".ljust(128)  # BP 471-598 Data Preparer Identifier
    vd[598:726] = b" " * 128  # BP 599-726 Application Identifier
    vd[726:758] = b" " * 32  # BP 727-758 Copyright File Identifier
    vd[758:790] = b" " * 32  # BP 759-790 Abstract File Identifier
    vd[790:806] = VOLUME_DATE  # BP 791-806 Volume Creation Date and Time
    vd[806:822] = VOLUME_DATE  # BP 807-822 Volume Modification Date and Time
    vd[822:838] = b"0" * 16  # BP 823-838 Volume Expiration Date and Time
    vd[838:854] = b"0" * 16  # BP 839-854 Volume Effective Date and Time
    vd[854] = 1  # BP 855 File Structure Standard Version
    image.put(16, bytes(vd))

    # 11.3 Volume Descriptor Sequence Terminator
    end = bytearray(SECTOR)
    end[0:8] = both32(image.lbn(17))
    end[8] = 255
    end[9:14] = b"CDROM"
    end[14] = 1
    image.put(17, bytes(end))

    out = bytearray(total_sectors * SECTOR)
    for sector, data in image.sectors.items():
        out[sector * SECTOR : (sector + 1) * SECTOR] = data
    return bytes(out), image.expected


def main():
    path = sys.argv[1]
    block_size = int(sys.argv[2]) if len(sys.argv) > 2 else 2048
    assert block_size in (512, 1024, 2048)
    data, expected = build(block_size)
    with open(path, "wb") as out:
        out.write(data)
    for name in sorted(expected):
        print(f"{name}\t{len(expected[name])}\t{fnv1a(expected[name]):016x}")


if __name__ == "__main__":
    main()
