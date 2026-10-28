#!/usr/bin/env python3
"""LIFS from the host: partition table, format, import, read, check.

The kernel is the reference for the format (src/fs/lifs/format.rs, the
B+ trees of src/fs/lifs/btree.rs); this tool writes the same bytes so a
disk can be partitioned, formatted and filled **without booting** -- the
browser's root weighs some 500 MiB. What it writes is checked by the
kernel at boot (`fs::check_all`) and by `check` here.

The host works offline: it writes metadata straight to its homes, with no
journal transaction, and leaves the journal holding nothing the kernel
would replay (the kernel replays sequence `journal_seq + 1` only).

    tools/lifs.py create disks/nova-disk.img --size 2048     # GPT + LIFS
    tools/lifs.py import disks/nova-disk.img build/ports/webroot /apps/web-browser/root
    tools/lifs.py put disks/nova-disk.img build/x.so /sys/runtimes/c/libc.so
    tools/lifs.py ls disks/nova-disk.img /apps
    tools/lifs.py cat disks/nova-disk.img /logs/kernel/demarrages.txt
    tools/lifs.py check disks/nova-disk.img
    tools/lifs.py table disks/nova-disk.img
    tools/lifs.py from-novafs old-novafs.img disks/nova-disk.img --size 2048

A volume is chosen on the image as the kernel chooses its root: the LIFS
partition flagged "system", else the first LIFS partition, else a whole
disk carrying LIFS at block 0 (`--partition N` picks another).
"""
import argparse
import os
import struct
import sys
import uuid
import zlib

BLOCK = 4096
SECTOR = 512
MAGIC = b"LIFS\0\0\0\x01"
VERSION = 1
ROOT = 1
FLAG_SYSTEM = 1
NAME_MAX = 254
INLINE_EXTENTS = 7
NODE_SIZE, ENTRY_SIZE, REFCOUNT_SIZE, REVERSE_SIZE = 256, 264, 16, 8
CHAINED = 0xFF
KIND_FILE, KIND_DIR = 1, 2
CHAIN_PER_BLOCK = (BLOCK - 16) // 24
HEADER, CHILD = 16, 8

LIFS_TYPE = uuid.UUID("BD095063-2621-48A5-89A3-0310280C1AF0")
NOVAFS_TYPE = uuid.UUID("A395A30A-8F34-4F37-B123-AE9B7E682E3A")

# Permissions (src/fs/perm.rs)
READ, WRITE, EXECUTE, OVERWRITE, DELETE = 1 << 0, 1 << 1, 1 << 2, 1 << 3, 1 << 4
RENAME, ACCESS, LINK, MOVE, COPY, CHMOD, OWN, AUDIT = (1 << i for i in range(5, 13))
FILE_APPLICABLE = READ | WRITE | EXECUTE | OVERWRITE | DELETE | RENAME | LINK | MOVE | COPY | CHMOD | OWN | AUDIT
DIR_APPLICABLE = WRITE | EXECUTE | DELETE | RENAME | ACCESS | LINK | MOVE | COPY | CHMOD | OWN | AUDIT
SYSTEM_DIR = (WRITE | EXECUTE | DELETE | RENAME | ACCESS | LINK | MOVE | COPY | CHMOD | OWN,
              ACCESS | EXECUTE, ACCESS | EXECUTE)
SYSTEM_FILE = (READ | WRITE | OVERWRITE | DELETE | RENAME | CHMOD | OWN, READ, READ)
SYSTEM_BINARY = (READ | EXECUTE | OVERWRITE | DELETE | RENAME | CHMOD | OWN,
                 READ | EXECUTE, READ | EXECUTE)

KEY_MIN, KEY_MAX = (0, 0), (2**64 - 1, 2**64 - 1)


def crc32(data):
    return zlib.crc32(data) & 0xFFFFFFFF


def fnv1a(data):
    h = 0xcbf29ce484222325
    for b in data:
        h ^= b
        h = (h * 0x100000001b3) & 0xFFFFFFFFFFFFFFFF
    return h


def ascii_name(name):
    """Printable ASCII, as the kernel's names; other letters become '_'."""
    return "".join(ch if " " <= ch <= "~" else "_" for ch in name)


def valid_name(name):
    return 0 < len(name.encode()) <= NAME_MAX and name not in (".", "..") and "/" not in name and "\0" not in name


# ============================================================
#  Partition table (GPT, and reading MBR)
# ============================================================

def gpt_entry(type_guid, unique, first, last, name):
    e = bytearray(128)
    e[0:16] = type_guid.bytes_le
    e[16:32] = unique.bytes_le
    struct.pack_into("<QQQ", e, 32, first, last, 0)
    raw = name.encode("utf-16-le")[:72]
    e[56:56 + len(raw)] = raw
    return bytes(e)


def write_gpt(f, sectors, partitions):
    """partitions: [(type UUID, unique UUID, first LBA, last LBA, name)]."""
    entries = bytearray(128 * 128)
    for i, p in enumerate(partitions):
        entries[i * 128:(i + 1) * 128] = gpt_entry(*p)
    entries_crc = crc32(bytes(entries))
    disk_guid = uuid.uuid4()
    last = sectors - 1

    def header(current, backup, entries_lba):
        h = bytearray(92)
        h[0:8] = b"EFI PART"
        struct.pack_into("<IIII", h, 8, 0x00010000, 92, 0, 0)
        struct.pack_into("<QQQQ", h, 24, current, backup, 34, last - 33)
        h[56:72] = disk_guid.bytes_le
        struct.pack_into("<QIII", h, 72, entries_lba, 128, 128, entries_crc)
        struct.pack_into("<I", h, 16, crc32(bytes(h)))
        return bytes(h) + bytes(SECTOR - 92)

    mbr = bytearray(SECTOR)
    mbr[446 + 4] = 0xEE
    struct.pack_into("<II", mbr, 446 + 8, 1, min(sectors - 1, 0xFFFFFFFF))
    mbr[446 + 1:446 + 4] = b"\x00\x02\x00"
    mbr[446 + 5:446 + 8] = b"\xff\xff\xff"
    mbr[510:512] = b"\x55\xaa"
    f.seek(0)
    f.write(bytes(mbr))
    f.write(header(1, last, 2))
    f.write(bytes(entries))
    f.seek((last - 32) * SECTOR)
    f.write(bytes(entries))
    f.write(header(last, 1, last - 32))


def read_table(f):
    """[(index, kind, first LBA, sectors, unique UUID or None, name)]; None
    when the disk has no table."""
    f.seek(0)
    mbr = f.read(SECTOR)
    if len(mbr) < SECTOR or mbr[510:512] != b"\x55\xaa":
        return None
    types = [mbr[446 + i * 16 + 4] for i in range(4)]
    if 0xEE in types:
        f.seek(SECTOR)
        h = f.read(SECTOR)
        if h[0:8] != b"EFI PART":
            sys.exit("table GPT sans en-tete valide")
        size = struct.unpack_from("<I", h, 12)[0]
        check = bytearray(h[:size])
        struct.pack_into("<I", check, 16, 0)
        if crc32(bytes(check)) != struct.unpack_from("<I", h, 16)[0]:
            sys.exit("en-tete GPT : somme de controle fausse")
        lba, count, esize, ecrc = struct.unpack_from("<QIII", h, 72)
        f.seek(lba * SECTOR)
        raw = f.read(count * esize)
        if crc32(raw) != ecrc:
            sys.exit("entrees GPT : somme de controle fausse")
        out = []
        for i in range(count):
            e = raw[i * esize:(i + 1) * esize]
            t = uuid.UUID(bytes_le=bytes(e[0:16]))
            if t.int == 0:
                continue
            first, last = struct.unpack_from("<QQ", e, 32)
            name = e[56:128].decode("utf-16-le", "replace").split("\0")[0]
            kind = "LIFS" if t == LIFS_TYPE else "NovaFS" if t == NOVAFS_TYPE else f"type {t}"
            out.append((i + 1, kind, first, last - first + 1, uuid.UUID(bytes_le=bytes(e[16:32])), name))
        return out
    out = []
    for i in range(4):
        e = mbr[446 + i * 16:446 + (i + 1) * 16]
        if e[4] == 0:
            continue
        first, count = struct.unpack_from("<II", e, 8)
        out.append((i + 1, "LIFS" if e[4] == 0x4C else f"type MBR 0x{e[4]:02X}", first, count, None, ""))
    return out


# ============================================================
#  Records
# ============================================================

def decode_record(b):
    inline = [struct.unpack_from("<QQQ", b, 80 + i * 24) for i in range(INLINE_EXTENTS)]
    return {
        "id": struct.unpack_from("<Q", b, 0)[0], "kind": b[8], "extent_count": b[9],
        "links": struct.unpack_from("<I", b, 12)[0], "size": struct.unpack_from("<Q", b, 16)[0],
        "owner": struct.unpack_from("<I", b, 24)[0], "group": struct.unpack_from("<I", b, 28)[0],
        "perm": list(struct.unpack_from("<HHH", b, 32)),
        "created": struct.unpack_from("<Q", b, 40)[0], "modified": struct.unpack_from("<Q", b, 48)[0],
        "accessed": struct.unpack_from("<Q", b, 56)[0], "tree": struct.unpack_from("<Q", b, 64)[0],
        "entries": struct.unpack_from("<Q", b, 72)[0], "inline": [list(e) for e in inline],
    }


def encode_record(r):
    b = bytearray(NODE_SIZE)
    struct.pack_into("<QBB", b, 0, r["id"], r["kind"], r["extent_count"])
    struct.pack_into("<IQII", b, 12, r["links"], r["size"], r["owner"], r["group"])
    struct.pack_into("<HHH", b, 32, *r["perm"])
    struct.pack_into("<QQQQQ", b, 40, r["created"], r["modified"], r["accessed"], r["tree"], r["entries"])
    for i, e in enumerate(r["inline"]):
        struct.pack_into("<QQQ", b, 80 + i * 24, *e)
    return bytes(b)


def new_record(node_id, kind, mode, owner=0, group=0):
    applicable = DIR_APPLICABLE if kind == KIND_DIR else FILE_APPLICABLE
    return {"id": node_id, "kind": kind, "extent_count": 0, "links": 0, "size": 0, "owner": owner,
            "group": group, "perm": [m & applicable for m in mode], "created": 0, "modified": 0,
            "accessed": 0, "tree": 0, "entries": 0, "inline": [[0, 0, 0] for _ in range(INLINE_EXTENTS)]}


def encode_entry(target, name):
    raw = name.encode()[:NAME_MAX]
    b = bytearray(ENTRY_SIZE)
    struct.pack_into("<Q", b, 0, target)
    b[9] = len(raw)
    b[10:10 + len(raw)] = raw
    return bytes(b)


def decode_entry(b):
    n = min(b[9], NAME_MAX)
    return struct.unpack_from("<Q", b, 0)[0], b[10:10 + n].decode("utf-8", "replace")


# ============================================================
#  The volume
# ============================================================

class Volume:
    """A LIFS volume in an image, at byte `offset`, `blocks` blocks."""

    def __init__(self, f, offset, blocks):
        self.f, self.offset, self.blocks = f, offset, blocks
        self.meta = {}      # block -> bytearray, every metadata block read or written
        self.dirty = set()
        raw = self.read_raw(0)
        if raw[0:8] != MAGIC:
            raise ValueError("pas de superbloc LIFS")
        if crc32(raw[:BLOCK - 4]) != struct.unpack_from("<I", raw, BLOCK - 4)[0]:
            raise ValueError("superbloc LIFS : somme de controle fausse")
        (self.total, ) = struct.unpack_from("<Q", raw, 16)
        self.uuid = raw[24:40]
        self.label = raw[40:72].split(b"\0")[0].decode("ascii", "replace")
        (self.flags, ) = struct.unpack_from("<I", raw, 72)
        (self.bitmap_start, self.bitmap_blocks, self.journal_start, self.journal_blocks,
         self.node_root, self.refcount_root, self.reverse_root, _root, self.next_node,
         self.free_blocks, self.node_count, self.journal_seq, self.created, self.modified,
         self.mount_count) = struct.unpack_from("<15Q", raw, 80)
        self.data_start = self.journal_start + self.journal_blocks
        self.bitmap = bytearray()
        for i in range(self.bitmap_blocks):
            self.bitmap += self.read_raw(self.bitmap_start + i)
        self.hint = self.data_start

    # --- raw access ---
    def read_raw(self, block):
        self.f.seek(self.offset + block * BLOCK)
        data = self.f.read(BLOCK)
        return data + bytes(BLOCK - len(data))

    def write_raw(self, block, data):
        assert len(data) == BLOCK and block < self.total
        self.f.seek(self.offset + block * BLOCK)
        self.f.write(data)

    def get(self, block):
        if block not in self.meta:
            self.meta[block] = bytearray(self.read_raw(block))
        return self.meta[block]

    def put(self, block, data):
        self.meta[block] = bytearray(data)
        self.dirty.add(block)

    # --- blocks ---
    def used(self, block):
        return self.bitmap[block // 8] & (1 << (block % 8)) != 0

    def set_bits(self, start, length, value):
        for b in range(start, start + length):
            if value:
                self.bitmap[b // 8] |= 1 << (b % 8)
            else:
                self.bitmap[b // 8] &= ~(1 << (b % 8)) & 0xFF

    def alloc_run(self, want):
        """First free run from the hint on (then from the start): (start, len)."""
        if self.free_blocks == 0:
            raise OSError("volume LIFS plein")
        for lo, hi in ((max(self.hint, self.data_start), self.total), (self.data_start, max(self.hint, self.data_start))):
            b = lo
            while b < hi:
                if b % 8 == 0 and self.bitmap[b // 8] == 0xFF and b + 8 <= hi:
                    b += 8
                    continue
                if not self.used(b):
                    start = b
                    while b < hi and b - start < want and not self.used(b):
                        b += 1
                    length = b - start
                    self.set_bits(start, length, True)
                    self.free_blocks -= length
                    self.hint = start + length
                    return start, length
                b += 1
        raise OSError("volume LIFS plein")

    def free_run(self, start, length):
        self.set_bits(start, length, False)
        self.free_blocks += length
        for b in range(start, start + length):
            self.meta.pop(b, None)
            self.dirty.discard(b)

    def alloc_meta(self):
        block, _ = self.alloc_run(1)
        self.put(block, bytes(BLOCK))
        return block

    # --- B+ tree (src/fs/lifs/btree.rs) ---
    @staticmethod
    def capacity(size):
        return (BLOCK - HEADER) // (16 + size)

    def load(self, block):
        img = self.get(block)
        if img[0:2] != b"LB":
            raise ValueError(f"bloc d'arbre {block} sans signature")
        level, count, size = img[2], struct.unpack_from("<H", img, 4)[0], struct.unpack_from("<H", img, 6)[0]
        entries = []
        for i in range(count):
            at = HEADER + i * (16 + size)
            hi, lo = struct.unpack_from("<QQ", img, at)
            entries.append(((hi, lo), bytes(img[at + 16:at + 16 + size])))
        return level, size, entries

    def save(self, block, level, size, entries):
        img = bytearray(BLOCK)
        img[0:2] = b"LB"
        img[2] = level
        struct.pack_into("<HH", img, 4, len(entries), size)
        for i, (key, value) in enumerate(entries):
            at = HEADER + i * (16 + size)
            struct.pack_into("<QQ", img, at, *key)
            img[at + 16:at + 16 + size] = value
        self.put(block, img)

    @staticmethod
    def route(entries, key):
        index = 0
        for i, (k, _) in enumerate(entries):
            if k <= key:
                index = i
        return index

    @staticmethod
    def child(entries, i):
        return struct.unpack_from("<Q", entries[i][1], 0)[0]

    def tget(self, root, key):
        block = root
        while block:
            level, _, entries = self.load(block)
            if level == 0:
                for k, v in entries:
                    if k == key:
                        return v
                return None
            block = self.child(entries, self.route(entries, key))
        return None

    def tinsert(self, root, size, key, value):
        """Returns the new root."""
        assert len(value) == size
        if root == 0:
            block = self.alloc_meta()
            self.save(block, 0, size, [(key, value)])
            return block
        split = self.insert_in(root, size, key, value)
        if split:
            level, _, old = self.load(root)
            first = min(old[0][0] if old else KEY_MIN, key)
            block = self.alloc_meta()
            self.save(block, level + 1, CHILD, [(first, struct.pack("<Q", root)), (split[0], struct.pack("<Q", split[1]))])
            return block
        return root

    def insert_in(self, block, size, key, value):
        level, stored, entries = self.load(block)
        if level == 0:
            keys = [k for k, _ in entries]
            import bisect
            i = bisect.bisect_left(keys, key)
            if i < len(entries) and entries[i][0] == key:
                entries[i] = (key, value)
            else:
                entries.insert(i, (key, value))
        else:
            i = self.route(entries, key)
            if key < entries[i][0]:
                entries[i] = (key, entries[i][1])
            split = self.insert_in(self.child(entries, i), size, key, value)
            if split:
                entries.insert(i + 1, (split[0], struct.pack("<Q", split[1])))
        vsize = size if level == 0 else CHILD
        if len(entries) <= self.capacity(vsize):
            self.save(block, level, vsize, entries)
            return None
        half = len(entries) // 2
        left, right = entries[:half], entries[half:]
        right_block = self.alloc_meta()
        self.save(block, level, vsize, left)
        self.save(right_block, level, vsize, right)
        return right[0][0], right_block

    def tremove(self, root, key):
        """Returns (new root, removed value or None)."""
        if root == 0:
            return 0, None
        removed, empty = self.remove_in(root, key)
        if empty:
            self.free_run(root, 1)
            return 0, removed
        while True:
            level, _, entries = self.load(root)
            if level == 0 or len(entries) != 1:
                break
            only = self.child(entries, 0)
            self.free_run(root, 1)
            root = only
        return root, removed

    def remove_in(self, block, key):
        level, size, entries = self.load(block)
        if level == 0:
            for i, (k, v) in enumerate(entries):
                if k == key:
                    del entries[i]
                    removed = v
                    break
            else:
                return None, False
        else:
            i = self.route(entries, key)
            target = self.child(entries, i)
            removed, empty = self.remove_in(target, key)
            if removed is None:
                return None, False
            if empty:
                self.free_run(target, 1)
                del entries[i]
            elif len(entries) >= 2:
                # Merge with a neighbour when both fit (btree.rs,
                # merge_neighbours).
                li = i if i + 1 < len(entries) else i - 1
                lb, rb = self.child(entries, li), self.child(entries, li + 1)
                ll, ls, left = self.load(lb)
                rl, rs, right = self.load(rb)
                if ll == rl and ls == rs and len(left) + len(right) <= self.capacity(ls):
                    self.save(lb, ll, ls, left + right)
                    self.free_run(rb, 1)
                    del entries[li + 1]
        if not entries:
            return removed, True
        self.save(block, level, size, entries)
        return removed, False

    def trange(self, root, lo, hi):
        out = []
        if root:
            self.range_in(root, lo, hi, out)
        return out

    def range_in(self, block, lo, hi, out):
        level, _, entries = self.load(block)
        if level == 0:
            out.extend((k, v) for k, v in entries if lo <= k <= hi)
            return
        for i in range(len(entries)):
            low_ok = i == 0 or entries[i][0] <= hi
            high_ok = i + 1 >= len(entries) or entries[i + 1][0] > lo
            if low_ok and high_ok:
                self.range_in(self.child(entries, i), lo, hi, out)

    def tblocks(self, root, out):
        if root == 0:
            return
        out.append(root)
        level, _, entries = self.load(root)
        if level:
            for i in range(len(entries)):
                self.tblocks(self.child(entries, i), out)

    def tfree_all(self, root):
        blocks = []
        self.tblocks(root, blocks)
        for b in blocks:
            self.free_run(b, 1)

    # --- records ---
    def load_record(self, node_id):
        v = self.tget(self.node_root, (node_id, 0))
        return decode_record(v) if v else None

    def save_record(self, r):
        self.node_root = self.tinsert(self.node_root, NODE_SIZE, (r["id"], 0), encode_record(r))

    def make_record(self, kind, mode, owner=0, group=0):
        r = new_record(self.next_node, kind, mode, owner, group)
        self.next_node += 1
        self.node_count += 1
        return r

    def drop_record(self, node_id):
        self.node_root, _ = self.tremove(self.node_root, (node_id, 0))
        self.node_count -= 1

    # --- extents and refcounts ---
    def extents(self, r):
        if r["extent_count"] != CHAINED:
            return [tuple(e) for e in r["inline"][:r["extent_count"]]]
        out, block = [], r["tree"]
        while block:
            img = self.get(block)
            if img[0:2] != b"LE":
                raise ValueError("chaine d'etendues sans signature")
            count = struct.unpack_from("<I", img, 4)[0]
            out += [struct.unpack_from("<QQQ", img, 16 + i * 24) for i in range(count)]
            block = struct.unpack_from("<Q", img, 8)[0]
        return out

    def chain_blocks(self, r):
        out, block = [], r["tree"] if r["extent_count"] == CHAINED else 0
        while block:
            out.append(block)
            block = struct.unpack_from("<Q", self.get(block), 8)[0]
        return out

    def set_extents(self, r, extents):
        for b in self.chain_blocks(r):
            self.free_run(b, 1)
        if len(extents) <= INLINE_EXTENTS:
            r["extent_count"], r["tree"] = len(extents), 0
            r["inline"] = [list(e) for e in extents] + [[0, 0, 0]] * (INLINE_EXTENTS - len(extents))
            return
        chunks = [extents[i:i + CHAIN_PER_BLOCK] for i in range(0, len(extents), CHAIN_PER_BLOCK)]
        blocks = [self.alloc_meta() for _ in chunks]
        for i, chunk in enumerate(chunks):
            img = bytearray(BLOCK)
            img[0:2] = b"LE"
            struct.pack_into("<IQ", img, 4, len(chunk), blocks[i + 1] if i + 1 < len(blocks) else 0)
            for j, e in enumerate(chunk):
                struct.pack_into("<QQQ", img, 16 + j * 24, *e)
            self.put(blocks[i], img)
        r["extent_count"], r["tree"] = CHAINED, blocks[0]
        r["inline"] = [[0, 0, 0] for _ in range(INLINE_EXTENTS)]

    def shared_runs(self):
        return [(k[0], struct.unpack_from("<Q", v, 0)[0], struct.unpack_from("<I", v, 8)[0])
                for k, v in self.trange(self.refcount_root, KEY_MIN, KEY_MAX)]

    def release_run(self, start, length):
        """One designation of `start .. start+length` less (Volume::adjust_refs, -1)."""
        end = start + length
        runs = [(s, l, c) for s, l, c in self.shared_runs() if s < end and s + l > start]
        for s, l, c in runs:
            self.refcount_root, _ = self.tremove(self.refcount_root, (s, 0))
            if s < start:
                self.put_shared(s, start - s, c)
            if s + l > end:
                self.put_shared(end, s + l - end, c)
        at = start
        for s, l, c in sorted(runs):
            lo, hi = max(s, start), min(s + l, end)
            if lo > at:
                self.free_run(at, lo - at)
            if c - 1 >= 2:
                self.put_shared(lo, hi - lo, c - 1)
            at = hi
        if at < end:
            self.free_run(at, end - at)

    def put_shared(self, start, length, refs):
        self.refcount_root = self.tinsert(self.refcount_root, REFCOUNT_SIZE, (start, 0), struct.pack("<QI4x", length, refs))

    def release_content(self, r):
        for _, phys, length in self.extents(r):
            self.release_run(phys, length)
        for b in self.chain_blocks(r):
            self.free_run(b, 1)
        r["extent_count"], r["tree"], r["size"] = 0, 0, 0
        r["inline"] = [[0, 0, 0] for _ in range(INLINE_EXTENTS)]

    def write_content(self, r, content):
        """A file's whole content, in as few runs as the free space allows."""
        blocks = -(-len(content) // BLOCK)
        extents, logical = [], 0
        while logical < blocks:
            start, length = self.alloc_run(blocks - logical)
            chunk = content[logical * BLOCK:(logical + length) * BLOCK]
            chunk += bytes(length * BLOCK - len(chunk))
            self.f.seek(self.offset + start * BLOCK)
            self.f.write(chunk)
            extents.append((logical, start, length))
            logical += length
        self.set_extents(r, extents)
        r["size"] = len(content)
        self.save_record(r)

    def read_content(self, r):
        out = bytearray(r["size"])
        for logical, phys, length in self.extents(r):
            at = logical * BLOCK
            if at >= r["size"]:
                continue
            self.f.seek(self.offset + phys * BLOCK)
            data = self.f.read(min(length * BLOCK, r["size"] - at))
            out[at:at + len(data)] = data
        return bytes(out)

    # --- directories and the reverse index ---
    def lookup(self, d, name):
        raw = name.encode()
        h = fnv1a(raw)
        for k, v in self.trange(d["tree"], (h, 0), (h, 2**64 - 1)):
            if v[9] == len(raw) and v[10:10 + len(raw)] == raw:
                return k, struct.unpack_from("<Q", v, 0)[0]
        return None

    def entries(self, d):
        return [decode_entry(v) for _, v in self.trange(d["tree"], KEY_MIN, KEY_MAX)]

    def reverse_adjust(self, node, directory, delta):
        key = (node, directory)
        v = self.tget(self.reverse_root, key)
        count = (struct.unpack_from("<Q", v, 0)[0] if v else 0) + delta
        if count <= 0:
            self.reverse_root, _ = self.tremove(self.reverse_root, key)
        else:
            self.reverse_root = self.tinsert(self.reverse_root, REVERSE_SIZE, key, struct.pack("<Q", count))

    def add_entry(self, dir_id, name, target):
        if not valid_name(name):
            raise ValueError(f"nom refuse : {name!r}")
        d = self.load_record(dir_id)
        raw = name.encode()
        h = fnv1a(raw)
        same = self.trange(d["tree"], (h, 0), (h, 2**64 - 1))
        if any(v[9] == len(raw) and v[10:10 + len(raw)] == raw for _, v in same):
            raise ValueError(f"{name} existe deja")
        lo = max((k[1] + 1 for k, _ in same), default=0)
        d["tree"] = self.tinsert(d["tree"], ENTRY_SIZE, (h, lo), encode_entry(target, name))
        d["entries"] += 1
        self.save_record(d)
        self.reverse_adjust(target, dir_id, 1)

    def remove_entry(self, dir_id, name):
        d = self.load_record(dir_id)
        found = self.lookup(d, name)
        if not found:
            raise KeyError(name)
        d["tree"], _ = self.tremove(d["tree"], found[0])
        d["entries"] -= 1
        self.save_record(d)
        self.reverse_adjust(found[1], dir_id, -1)
        return found[1]

    def resolve(self, path, create_dirs=False, mode=SYSTEM_DIR):
        r = self.load_record(ROOT)
        for part in [p for p in path.split("/") if p]:
            found = self.lookup(r, part)
            if found is None:
                if not create_dirs:
                    return None
                child = self.make_record(KIND_DIR, mode)
                child["links"] = 1
                self.save_record(child)
                self.add_entry(r["id"], part, child["id"])
                r = self.load_record(child["id"])
                continue
            r = self.load_record(found[1])
            if r is None:
                raise ValueError(f"entree {part} vers un noeud absent")
        return r

    def unlink(self, dir_id, name):
        target = self.remove_entry(dir_id, name)
        r = self.load_record(target)
        r["links"] -= 1
        if r["links"] > 0:
            self.save_record(r)
            return
        if r["kind"] == KIND_DIR:
            self.tfree_all(r["tree"])
        else:
            self.release_content(r)
        self.drop_record(target)

    # --- writing back ---
    def superblock(self):
        b = bytearray(BLOCK)
        b[0:8] = MAGIC
        struct.pack_into("<IIQ", b, 8, VERSION, BLOCK, self.total)
        b[24:40] = self.uuid
        label = self.label.encode()[:31]
        b[40:40 + len(label)] = label
        struct.pack_into("<I", b, 72, self.flags)
        struct.pack_into("<15Q", b, 80, self.bitmap_start, self.bitmap_blocks, self.journal_start,
                         self.journal_blocks, self.node_root, self.refcount_root, self.reverse_root, ROOT,
                         self.next_node, self.free_blocks, self.node_count, self.journal_seq, self.created,
                         self.modified, self.mount_count)
        struct.pack_into("<I", b, BLOCK - 4, crc32(bytes(b[:BLOCK - 4])))
        return bytes(b)

    def close(self):
        for block in sorted(self.dirty):
            self.write_raw(block, bytes(self.meta[block]))
        self.dirty.clear()
        for i in range(self.bitmap_blocks):
            self.write_raw(self.bitmap_start + i, bytes(self.bitmap[i * BLOCK:(i + 1) * BLOCK]))
        # Nothing for the kernel to replay: the journal's first block blank.
        self.write_raw(self.journal_start, bytes(BLOCK))
        self.write_raw(0, self.superblock())
        self.f.flush()

    # --- check (src/fs/lifs/check.rs, the same rules) ---
    def check(self):
        records = {k[0]: decode_record(v) for k, v in self.trange(self.node_root, KEY_MIN, KEY_MAX)}
        problems = []
        if len(records) != self.node_count:
            problems.append(f"superbloc : {self.node_count} noeuds, arbre : {len(records)}")
        named, reverse, meta, data = {ROOT: 1}, {}, [], []
        queue, seen = [ROOT], {ROOT}
        while queue:
            d = records[queue.pop()]
            es = self.entries(d)
            if len(es) != d["entries"]:
                problems.append(f"dossier {d['id']} : {d['entries']} entrees annoncees, {len(es)} trouvees")
            self.tblocks(d["tree"], meta)
            for target, name in es:
                if target not in records:
                    problems.append(f"{name} -> noeud absent {target}")
                    continue
                named[target] = named.get(target, 0) + 1
                reverse[(target, d["id"])] = reverse.get((target, d["id"]), 0) + 1
                if records[target]["kind"] == KIND_DIR:
                    if target in seen:
                        problems.append(f"dossier {target} nomme deux fois")
                    else:
                        seen.add(target)
                        queue.append(target)
        for r in records.values():
            if named.get(r["id"], 0) != r["links"]:
                problems.append(f"noeud {r['id']} : {r['links']} liens, {named.get(r['id'], 0)} entrees")
        stored = {k: struct.unpack_from("<Q", v, 0)[0] for k, v in self.trange(self.reverse_root, KEY_MIN, KEY_MAX)}
        if stored != reverse:
            problems.append("index inverse different des entrees")
        for root in (self.node_root, self.refcount_root, self.reverse_root):
            self.tblocks(root, meta)
        counts = {}
        for r in records.values():
            if r["kind"] == KIND_FILE:
                meta += self.chain_blocks(r)
                for _, phys, length in self.extents(r):
                    data.append((phys, length))
                    for b in range(phys, phys + length):
                        counts[b] = counts.get(b, 0) + 1
        if len(set(meta)) != len(meta):
            problems.append("bloc d'arbre designe deux fois")
        if set(meta) & set(counts):
            problems.append("bloc a la fois arbre et donnees")
        shared = {}
        for s, l, c in self.shared_runs():
            for b in range(s, s + l):
                shared[b] = c
        if {b: c for b, c in counts.items() if c >= 2} != shared:
            problems.append("comptes de references differents des partages")
        in_use = set(meta) | set(counts)
        wrong = [b for b in range(self.data_start, self.total) if self.used(b) != (b in in_use)]
        if wrong:
            problems.append(f"bitmap : {len(wrong)} bloc(s) faux, par exemple {wrong[0]}")
        if self.free_blocks != (self.total - self.data_start) - len(in_use):
            problems.append(f"blocs libres : superbloc {self.free_blocks}, bitmap {(self.total - self.data_start) - len(in_use)}")
        return len(records), len(in_use), problems


def format_volume(f, offset, blocks, label, flags):
    """Writes an empty LIFS volume (Volume::format of the kernel)."""
    bitmap_blocks = -(-blocks // (BLOCK * 8))
    journal_blocks = min(max(blocks // 64, 64), 16384)
    journal_start = 1 + bitmap_blocks
    data_start = journal_start + journal_blocks
    bitmap = bytearray(bitmap_blocks * BLOCK)
    for b in list(range(data_start)) + list(range(blocks, bitmap_blocks * BLOCK * 8)):
        bitmap[b // 8] |= 1 << (b % 8)
    for i in range(bitmap_blocks):
        f.seek(offset + (1 + i) * BLOCK)
        f.write(bytes(bitmap[i * BLOCK:(i + 1) * BLOCK]))
    f.seek(offset + journal_start * BLOCK)
    f.write(bytes(BLOCK))
    sb = bytearray(BLOCK)
    sb[0:8] = MAGIC
    struct.pack_into("<IIQ", sb, 8, VERSION, BLOCK, blocks)
    sb[24:40] = uuid.uuid4().bytes
    raw = label.encode()[:31]
    sb[40:40 + len(raw)] = raw
    struct.pack_into("<I", sb, 72, flags)
    struct.pack_into("<15Q", sb, 80, 1, bitmap_blocks, journal_start, journal_blocks, 0, 0, 0, ROOT, ROOT,
                     blocks - data_start, 0, 0, 0, 0, 0)
    struct.pack_into("<I", sb, BLOCK - 4, crc32(bytes(sb[:BLOCK - 4])))
    f.seek(offset)
    f.write(bytes(sb))
    f.flush()
    v = Volume(f, offset, blocks)
    root = v.make_record(KIND_DIR, SYSTEM_DIR)
    root["links"] = 1
    v.save_record(root)
    v.close()
    return v


# ============================================================
#  Opening an image
# ============================================================

def locate(f, partition=None):
    """(offset, blocks, description) of the volume to work on."""
    table = read_table(f)
    size = os.fstat(f.fileno()).st_size
    if table is None:
        return 0, size // BLOCK, "disque entier"
    candidates = []
    for index, kind, first, sectors, unique, name in table:
        if kind != "LIFS" or first % 8:
            continue
        f.seek(first * SECTOR)
        head = f.read(BLOCK)
        if head[0:8] != MAGIC:
            continue
        system = struct.unpack_from("<I", head, 72)[0] & FLAG_SYSTEM
        candidates.append((index, first, sectors, system, name))
    if partition is not None:
        candidates = [c for c in candidates if c[0] == partition]
    else:
        candidates.sort(key=lambda c: (not c[3], c[0]))
    if not candidates:
        sys.exit("aucune partition LIFS formatee sur cette image")
    index, first, sectors, _, name = candidates[0]
    return first * SECTOR, sectors // 8, f"partition {index} \"{name}\""


def novafs_disk(image):
    with open(image, "rb") as f:
        return read_table(f) is None and f.seek(0) == 0 and f.read(8) == b"NOVAFS\0\0"


def open_volume(image, partition=None):
    f = open(image, "r+b")
    offset, blocks, where = locate(f, partition)
    try:
        return Volume(f, offset, blocks), where
    except ValueError as error:
        sys.exit(f"{image} ({where}) : {error}")


# ============================================================
#  Commands
# ============================================================

def create(image, size_mib, data_mib, label):
    """A GPT disk: p1 LIFS "system" (the root), p2 LIFS for data when
    `data_mib` is not zero. Never on an existing file: that is what
    `make disk-reset` removes first."""
    if os.path.exists(image):
        sys.exit(f"{image} existe deja - `make disk-reset` pour repartir de zero")
    sectors = size_mib * 1024 * 1024 // SECTOR
    first = 2048
    last_usable = sectors - 34
    data_sectors = data_mib * 1024 * 1024 // SECTOR
    system_last = (last_usable - data_sectors) // 8 * 8 - 1 if data_mib else last_usable // 8 * 8 - 1
    parts = [(LIFS_TYPE, uuid.uuid4(), first, system_last, label)]
    if data_mib:
        data_first = system_last + 1
        data_last = (last_usable + 1) // 8 * 8 - 1
        parts.append((LIFS_TYPE, uuid.uuid4(), data_first, data_last, "nova-donnees"))
    with open(image, "w+b") as f:
        f.truncate(sectors * SECTOR)
        write_gpt(f, sectors, parts)
        for i, (_, unique, lo, hi, name) in enumerate(parts):
            blocks = (hi - lo + 1) // 8
            format_volume(f, lo * SECTOR, blocks, name, FLAG_SYSTEM if i == 0 else 0)
            print(f"[ok] p{i + 1} LIFS \"{name}\" : {blocks} blocs ({blocks * BLOCK // (1024 * 1024)} Mio), "
                  f"GUID {str(unique).upper()}{' - systeme' if i == 0 else ''}")


def import_tree(v, source, destination):
    """The host tree `source` at `destination` (as tools/nova-fs.py does):
    a directory already there is entered, a file already there rewritten
    when its content changed; a symbolic link inside the tree becomes a
    Tag of its target -- LIFS has one kind of name, the entry."""
    sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
    from importlib import import_module
    resolve_link = import_module("nova-fs").resolve_link
    files = dirs = links = skipped = updated = total = 0
    pending, dangling = [], []
    parent = v.resolve(destination, create_dirs=True)
    stack = [(source, parent["id"])]
    while stack:
        directory, parent_id = stack.pop()
        for entry in sorted(os.listdir(directory)):
            path = os.path.join(directory, entry)
            name = ascii_name(entry)
            parent = v.load_record(parent_id)
            if os.path.islink(path):
                target = resolve_link(source, path)
                if target is None:
                    dangling.append(os.path.relpath(path, source))
                    continue
                pending.append((parent_id, name, os.path.relpath(target, os.path.realpath(source))))
                continue
            existing = v.lookup(parent, name)
            if existing is not None:
                there = v.load_record(existing[1])
                if os.path.isdir(path) and there["kind"] == KIND_DIR:
                    stack.append((path, there["id"]))
                elif not os.path.isdir(path) and there["kind"] == KIND_FILE:
                    with open(path, "rb") as fh:
                        content = fh.read()
                    if there["size"] != len(content) or v.read_content(there) != content:
                        v.release_content(there)
                        v.write_content(there, content)
                        updated += 1
                else:
                    print(f"[!!] ignore : {entry} existe deja sous une autre forme")
                    skipped += 1
                continue
            if os.path.isdir(path):
                child = v.make_record(KIND_DIR, SYSTEM_DIR)
                child["links"] = 1
                v.save_record(child)
                v.add_entry(parent_id, name, child["id"])
                dirs += 1
                stack.append((path, child["id"]))
                continue
            with open(path, "rb") as fh:
                content = fh.read()
            r = v.make_record(KIND_FILE, SYSTEM_BINARY if os.access(path, os.X_OK) else SYSTEM_FILE)
            r["links"] = 1
            v.write_content(r, content)
            try:
                v.add_entry(parent_id, name, r["id"])
            except ValueError as error:
                print(f"[!!] ignore : {error}")
                v.release_content(r)
                v.drop_record(r["id"])
                skipped += 1
                continue
            files += 1
            total += len(content)
            if files % 500 == 0:
                print(f"    {files} fichiers, {total // (1024 * 1024)} Mio", end="\r", flush=True)
    # The links, once every target is there.
    for parent_id, name, relative in pending:
        target = v.resolve(f"{destination}/{relative}")
        parent = v.load_record(parent_id)
        if target is None:
            skipped += 1
            continue
        if v.lookup(parent, name) is not None:
            continue
        if target["kind"] == KIND_DIR:
            # A directory under two names would make the tree a graph.
            skipped += 1
            continue
        v.add_entry(parent_id, name, target["id"])
        target = v.load_record(target["id"])
        target["links"] += 1
        v.save_record(target)
        links += 1
    if dangling:
        print(f"[..] {len(dangling)} lien(s) hors de l'arbre ignore(s), par exemple {dangling[0]}")
    v.close()
    print(f"[ok] {files} fichiers, {dirs} dossiers, {links} Tag(s) supplementaires, "
          f"{total // (1024 * 1024)} Mio ecrits dans {destination}"
          + (f" - {updated} fichier(s) existant(s) mis a jour" if updated else "")
          + (f" - {skipped} ignore(s)" if skipped else ""))


def put_file(v, source, path):
    with open(source, "rb") as fh:
        content = fh.read()
    r = v.resolve(path)
    if r is None:
        parent = v.resolve(os.path.dirname(path), create_dirs=True)
        r = v.make_record(KIND_FILE, SYSTEM_BINARY if os.access(source, os.X_OK) else SYSTEM_FILE)
        r["links"] = 1
        v.save_record(r)
        v.add_entry(parent["id"], os.path.basename(path), r["id"])
        r = v.load_record(r["id"])
    elif r["kind"] != KIND_FILE:
        sys.exit(f"{path} : pas un fichier")
    v.release_content(r)
    v.write_content(r, content)
    v.close()
    print(f"[ok] {path} : {len(content)} octets (noeud {r['id']})")


def from_novafs(source, target, size_mib, data_mib):
    """14.5.2 -- A NovaFS volume copied into a new GPT + LIFS disk: same
    tree, same owners and rights; the NovaFS image is only read."""
    sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
    from importlib import import_module
    novafs = import_module("nova-fs")
    old = novafs.Volume(source)
    create(target, size_mib, data_mib, old.label or "nova-systeme")
    v, _ = open_volume(target)
    seen = {}
    counts = {"files": 0, "dirs": 0, "tags": 0}

    def copy(old_node, new_dir):
        for name, node_id in old.links(old_node):
            child = old.read_node(node_id)
            if not child:
                continue
            if node_id in seen:
                # A second NovaFS link: a second Tag of the same LIFS node.
                v.add_entry(new_dir, name, seen[node_id])
                r = v.load_record(seen[node_id])
                r["links"] += 1
                v.save_record(r)
                counts["tags"] += 1
                continue
            kind = KIND_DIR if child["kind"] == novafs.KIND_DIR else KIND_FILE
            r = v.make_record(kind, tuple(child["perm"]), child["owner"], child["group"])
            r["links"] = 1
            if kind == KIND_DIR:
                v.save_record(r)
                v.add_entry(new_dir, name, r["id"])
                counts["dirs"] += 1
                copy(child, r["id"])
            else:
                v.write_content(r, old.read_content(child))
                v.add_entry(new_dir, name, r["id"])
                seen[node_id] = r["id"]
                counts["files"] += 1

    copy(old.read_node(novafs.ROOT_NODE), ROOT)
    v.close()
    print(f"[ok] {counts['files']} fichiers, {counts['dirs']} dossiers, {counts['tags']} Tag(s) repris de {source}")


def main():
    ap = argparse.ArgumentParser(description="LIFS depuis l'hote")
    ap.add_argument("--partition", type=int, help="numero de la partition LIFS (defaut : la partition systeme)")
    sub = ap.add_subparsers(dest="command", required=True)
    c = sub.add_parser("create"); c.add_argument("image"); c.add_argument("--size", type=int, default=2048)
    c.add_argument("--data", type=int, default=256, help="Mio de la partition de donnees (0 : aucune)")
    c.add_argument("--label", default="nova-systeme")
    i = sub.add_parser("import"); i.add_argument("image"); i.add_argument("source"); i.add_argument("destination")
    p = sub.add_parser("put"); p.add_argument("image"); p.add_argument("source"); p.add_argument("path")
    l = sub.add_parser("ls"); l.add_argument("image"); l.add_argument("path", nargs="?", default="/")
    k = sub.add_parser("cat"); k.add_argument("image"); k.add_argument("path")
    h = sub.add_parser("check"); h.add_argument("image")
    t = sub.add_parser("table"); t.add_argument("image")
    m = sub.add_parser("from-novafs"); m.add_argument("source"); m.add_argument("target")
    m.add_argument("--size", type=int, default=2048); m.add_argument("--data", type=int, default=256)
    a = ap.parse_args()

    if a.command == "create":
        create(a.image, a.size, a.data, a.label)
        return
    if a.command == "from-novafs":
        from_novafs(a.source, a.target, a.size, a.data)
        return
    if a.command == "table":
        with open(a.image, "rb") as f:
            table = read_table(f)
        if table is None:
            print("aucune table de partitions")
        for index, kind, first, sectors, unique, name in table or []:
            print(f"p{index}  {kind:<10} secteurs {first}..{first + sectors - 1}  {sectors * SECTOR // (1024 * 1024)} Mio"
                  f"  {str(unique).upper() if unique else ''}  {name}")
        return
    if a.command in ("import", "put", "ls", "cat") and a.partition is None and novafs_disk(a.image):
        # A disk from before phase 14: NovaFS on the whole disk. Its tool
        # handles it, so `make webroot` works on both.
        tool = os.path.join(os.path.dirname(os.path.abspath(__file__)), "nova-fs.py")
        args = [a.image] + [x for x in (getattr(a, "source", None), getattr(a, "destination", None), getattr(a, "path", None)) if x]
        os.execv(sys.executable, [sys.executable, tool, a.command] + args)
    v, where = open_volume(a.image, a.partition)
    if a.command == "import":
        import_tree(v, a.source, a.destination)
    elif a.command == "put":
        put_file(v, a.source, a.path)
    elif a.command == "ls":
        r = v.resolve(a.path)
        if r is None:
            sys.exit(f"{a.path} : introuvable")
        for target, name in sorted(v.entries(r), key=lambda e: e[1]):
            child = v.load_record(target)
            print(f"{child['size'] if child else 0:>10}  {name}{'/' if child and child['kind'] == KIND_DIR else ''}  -> noeud {target}")
    elif a.command == "cat":
        r = v.resolve(a.path)
        if r is None:
            sys.exit(f"{a.path} : introuvable")
        sys.stdout.buffer.write(v.read_content(r))
    elif a.command == "check":
        nodes, blocks, problems = v.check()
        for problem in problems:
            print(f"[!!] {problem}")
        print(f"[{'ok' if not problems else '!!'}] {where} \"{v.label}\" : {nodes} noeuds, {blocks} blocs en usage, "
              f"{v.free_blocks} libres{'' if not problems else f' - {len(problems)} probleme(s)'}")
        sys.exit(1 if problems else 0)


if __name__ == "__main__":
    main()
