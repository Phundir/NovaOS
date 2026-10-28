#!/usr/bin/env python3
"""Writes a Nova OS volume from the host: format, import a tree, list.

The kernel is the reference for the format (src/fs/format.rs); this tool
repeats it so that an image can be filled **without booting** -- the
browser's root weighs some 500 MiB, which no boot-time copy should have
to carry. What it writes is checked by the kernel itself: the self-test
of phase 4 walks every node and every block of the mounted volume.

Layout, identical to `SuperBlock::layout`:

    block 0        superblock
    ...            node bitmap        1 bit per node
    ...            block bitmap       1 bit per block
    ...            node table         128 bytes per node
    ...            data

A directory is a node whose blocks hold 64-byte entries, name then node
number (spec 4.1: folders are tables of links, nothing else).

    tools/nova-fs.py format disks/nova-disk.img --size 2048
    tools/nova-fs.py import disks/nova-disk.img build/ports/webroot /apps/web-browser
    tools/nova-fs.py ls disks/nova-disk.img /apps
    tools/nova-fs.py put disks/nova-disk.img build/x.so /apps/web-browser/root/usr/lib/x.so
"""
import argparse
import os
import struct
import sys

BLOCK = 4096
NODE_SIZE = 128
NODES_PER_BLOCK = BLOCK // NODE_SIZE
LINK_SIZE = 128
NAME_MAX = 120
LINKS_PER_BLOCK = BLOCK // LINK_SIZE
DIRECT_BLOCKS = 5
INDIRECT_ENTRIES = BLOCK // 8
SINGLE_LEVEL_BLOCKS = DIRECT_BLOCKS + INDIRECT_ENTRIES
MAGIC = b"NOVAFS\0\0"
VERSION = 2
ROOT_NODE = 1
KIND_FILE, KIND_DIR = 1, 2

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


FOLD = {"\u00e0": "a", "\u00e1": "a", "\u00e2": "a", "\u00e4": "a", "\u00e7": "c", "\u00e8": "e",
        "\u00e9": "e", "\u00ea": "e", "\u00eb": "e", "\u00ee": "i", "\u00ef": "i", "\u00f4": "o",
        "\u00f6": "o", "\u00f9": "u", "\u00fb": "u", "\u00fc": "u", "\u0151": "o", "\u00fa": "u",
        "\u00ed": "i", "\u00f3": "o", "\u00e5": "a", "\u00f1": "n", "\u00fd": "y"}


def ascii_name(name):
    """What `format::valid_name` accepts: printable ASCII, no slash.

    A name carrying accents (one certificate of Mozilla's store) keeps its
    letters without them rather than being dropped."""
    out = []
    for ch in name:
        if " " <= ch <= "~":
            out.append(ch)
        else:
            out.append(FOLD.get(ch.lower(), "_"))
    return "".join(out)


class Volume:
    def __init__(self, path, create_blocks=None, label="nova-systeme"):
        if create_blocks:
            with open(path, "wb") as f:
                f.truncate(create_blocks * BLOCK)
        self.f = open(path, "r+b")
        self.f.seek(0, os.SEEK_END)
        self.total = self.f.tell() // BLOCK
        # Metadata blocks (bitmaps, node table, link tables, indirection
        # tables) are kept in memory and written back at the end: read
        # and rewritten bit by bit, an import of 500 MiB would spend its
        # time in the same few blocks. File contents go straight to disk.
        self.cache = {}
        self.dirty = set()
        # Where the last allocation stopped: scanning the bitmap from the
        # start for every block is what makes a big import quadratic.
        self.next_block = 0
        self.next_node = 1
        # Version 1 wrote link entries of 64 bytes (56-character names),
        # version 2 writes 128 (120 characters). Reading both is what
        # lets `migrate` copy an old volume into a new one.
        self.version = VERSION
        self.link_size = LINK_SIZE
        self.name_max = NAME_MAX
        if create_blocks:
            self.layout(label)
        else:
            self.read_superblock()

    # --- blocks ---
    def read(self, n, cache=True):
        if n in self.cache:
            return self.cache[n]
        self.f.seek(n * BLOCK)
        data = self.f.read(BLOCK)
        data = data + bytes(BLOCK - len(data))
        if cache:
            self.cache[n] = data
        return data

    def write(self, n, data, cache=True):
        assert len(data) <= BLOCK
        data = data + bytes(BLOCK - len(data))
        if cache:
            self.cache[n] = data
            self.dirty.add(n)
            return
        self.cache.pop(n, None)
        self.f.seek(n * BLOCK)
        self.f.write(data)

    def flush(self):
        for n in sorted(self.dirty):
            self.f.seek(n * BLOCK)
            self.f.write(self.cache[n])
        self.dirty.clear()
        self.f.flush()

    # --- superblock ---
    def layout(self, label):
        self.node_count = max(self.total // 8, 64)
        self.node_bitmap_blocks = max(-(-self.node_count // (8 * BLOCK)), 1)
        self.node_table_blocks = max(-(-self.node_count // NODES_PER_BLOCK), 1)
        self.block_bitmap_blocks = max(-(-self.total // (8 * BLOCK)), 1)
        self.node_bitmap_start = 1
        self.block_bitmap_start = self.node_bitmap_start + self.node_bitmap_blocks
        self.node_table_start = self.block_bitmap_start + self.block_bitmap_blocks
        self.data_start = self.node_table_start + self.node_table_blocks
        if self.data_start >= self.total:
            sys.exit("image trop petite")
        self.label = label
        self.free_blocks = self.total - self.data_start
        self.free_nodes = self.node_count
        for n in range(1, self.data_start):
            self.write(n, bytes(BLOCK))
        for n in range(self.data_start):
            self.mark_block(n, True)
        root = self.allocate_node()
        assert root == ROOT_NODE
        self.write_node(self.new_node(root, KIND_DIR, SYSTEM_DIR, links=1))
        self.write_superblock()

    def read_superblock(self):
        b = self.read(0)
        if b[:8] != MAGIC:
            sys.exit("pas un volume Nova OS")
        self.version = struct.unpack_from("<I", b, 0x08)[0]
        self.link_size = 64 if self.version < 2 else LINK_SIZE
        self.name_max = self.link_size - 8
        (self.total, self.node_count, self.node_bitmap_start, self.node_bitmap_blocks,
         self.block_bitmap_start, self.block_bitmap_blocks, self.node_table_start,
         self.node_table_blocks, self.data_start, _root, self.free_blocks,
         self.free_nodes) = struct.unpack_from("<12Q", b, 0x10)
        self.label = b[0x80:0xA0].split(b"\0")[0].decode()

    def write_superblock(self):
        b = bytearray(BLOCK)
        b[0:8] = MAGIC
        struct.pack_into("<II", b, 0x08, self.version, BLOCK)
        struct.pack_into("<12Q", b, 0x10, self.total, self.node_count, self.node_bitmap_start,
                         self.node_bitmap_blocks, self.block_bitmap_start, self.block_bitmap_blocks,
                         self.node_table_start, self.node_table_blocks, self.data_start, ROOT_NODE,
                         self.free_blocks, self.free_nodes)
        struct.pack_into("<Q", b, 0x70, 0)   # created
        struct.pack_into("<Q", b, 0x78, 0)   # flags
        name = self.label.encode()[:31]
        b[0x80:0x80 + len(name)] = name
        # Checksum over 0x00..0x7B, FNV-1a, as `format::checksum`.
        h = 0x811C9DC5
        for byte in b[:0x7C]:
            h = ((h ^ byte) * 0x01000193) & 0xFFFFFFFF
        struct.pack_into("<I", b, 0x7C, h)
        self.write(0, bytes(b))

    # --- bitmaps ---
    def bit(self, start, index, value=None):
        block = start + index // (8 * BLOCK)
        offset = (index % (8 * BLOCK)) // 8
        mask = 1 << (index % 8)
        data = bytearray(self.read(block))
        if value is None:
            return bool(data[offset] & mask)
        if value:
            data[offset] |= mask
        else:
            data[offset] &= ~mask
        self.write(block, bytes(data))

    def mark_block(self, block, taken):
        self.bit(self.block_bitmap_start, block, taken)

    def allocate_block(self, zero=False):
        n = max(self.next_block, self.data_start)
        scanned = 0
        while scanned <= self.total - self.data_start:
            if n >= self.total:
                n = self.data_start
            if not self.bit(self.block_bitmap_start, n):
                self.next_block = n + 1
                self.mark_block(n, True)
                self.free_blocks -= 1
                if zero:
                    self.write(n, bytes(BLOCK))
                return n
            n += 1
            scanned += 1
        sys.exit("plus de bloc libre")

    def allocate_node(self):
        for n in range(self.next_node, self.node_count + 1):
            if not self.bit(self.node_bitmap_start, n - 1):
                self.next_node = n + 1
                self.bit(self.node_bitmap_start, n - 1, True)
                self.free_nodes -= 1
                return n
        sys.exit("plus de noeud libre")

    # --- nodes ---
    def new_node(self, node_id, kind, mode, links=0):
        applicable = DIR_APPLICABLE if kind == KIND_DIR else FILE_APPLICABLE
        return {"id": node_id, "kind": kind, "links": links, "owner": 0, "group": 0, "size": 0,
                "perm": [m & applicable for m in mode], "direct": [0] * DIRECT_BLOCKS,
                "indirect": 0, "levels": 1}

    def node_location(self, node_id):
        index = node_id - 1
        return self.node_table_start + index // NODES_PER_BLOCK, (index % NODES_PER_BLOCK) * NODE_SIZE

    def read_node(self, node_id):
        block, offset = self.node_location(node_id)
        slot = self.read(block)[offset:offset + NODE_SIZE]
        if slot[8] == 0:
            return None
        return {"id": struct.unpack_from("<Q", slot, 0)[0], "kind": slot[8],
                "levels": max(slot[9], 1), "links": struct.unpack_from("<H", slot, 0x0A)[0],
                "owner": struct.unpack_from("<I", slot, 0x0C)[0],
                "group": struct.unpack_from("<I", slot, 0x10)[0],
                "size": struct.unpack_from("<Q", slot, 0x18)[0],
                "perm": list(struct.unpack_from("<3Q", slot, 0x20)),
                "direct": list(struct.unpack_from("<5Q", slot, 0x50)),
                "indirect": struct.unpack_from("<Q", slot, 0x78)[0]}

    def write_node(self, node):
        block, offset = self.node_location(node["id"])
        data = bytearray(self.read(block))
        slot = bytearray(NODE_SIZE)
        struct.pack_into("<Q", slot, 0, node["id"])
        slot[8] = node["kind"]
        slot[9] = node["levels"]
        struct.pack_into("<H", slot, 0x0A, node["links"])
        struct.pack_into("<II", slot, 0x0C, node["owner"], node["group"])
        struct.pack_into("<Q", slot, 0x18, node["size"])
        struct.pack_into("<3Q", slot, 0x20, *node["perm"])
        struct.pack_into("<3Q", slot, 0x38, 0, 0, 0)
        struct.pack_into("<5Q", slot, 0x50, *node["direct"])
        struct.pack_into("<Q", slot, 0x78, node["indirect"])
        data[offset:offset + NODE_SIZE] = slot
        self.write(block, bytes(data))

    # --- blocks of a node ---
    def ensure_block(self, node, logical):
        if logical < DIRECT_BLOCKS:
            if node["direct"][logical] == 0:
                node["direct"][logical] = self.allocate_block()
            return node["direct"][logical]
        entry = logical - DIRECT_BLOCKS
        if entry >= INDIRECT_ENTRIES and node["levels"] < 2:
            top = self.allocate_block(zero=True)
            if node["indirect"]:
                data = bytearray(self.read(top))
                struct.pack_into("<Q", data, 0, node["indirect"])
                self.write(top, bytes(data))
            node["indirect"] = top
            node["levels"] = 2
        if node["indirect"] == 0:
            node["indirect"] = self.allocate_block(zero=True)
        if node["levels"] < 2:
            table, slot = node["indirect"], entry
        else:
            first, slot = divmod(entry, INDIRECT_ENTRIES)
            data = self.read(node["indirect"])
            table = struct.unpack_from("<Q", data, first * 8)[0]
            if table == 0:
                table = self.allocate_block(zero=True)
                top = bytearray(self.read(node["indirect"]))
                struct.pack_into("<Q", top, first * 8, table)
                self.write(node["indirect"], bytes(top))
        data = bytearray(self.read(table))
        block = struct.unpack_from("<Q", data, slot * 8)[0]
        if block == 0:
            block = self.allocate_block()
            struct.pack_into("<Q", data, slot * 8, block)
            self.write(table, bytes(data))
        return block

    def map_block(self, node, logical):
        if logical < DIRECT_BLOCKS:
            return node["direct"][logical] or None
        entry = logical - DIRECT_BLOCKS
        if not node["indirect"]:
            return None
        if node["levels"] < 2:
            return struct.unpack_from("<Q", self.read(node["indirect"]), entry * 8)[0] or None
        first, second = divmod(entry, INDIRECT_ENTRIES)
        table = struct.unpack_from("<Q", self.read(node["indirect"]), first * 8)[0]
        if not table:
            return None
        return struct.unpack_from("<Q", self.read(table), second * 8)[0] or None

    def release_block(self, block):
        self.mark_block(block, False)
        self.free_blocks += 1

    def free_content(self, node):
        """Gives back every data block and indirection table of a file and
        leaves it empty -- what the kernel's `truncate_node` does to zero."""
        for logical in range(-(-node["size"] // BLOCK)):
            block = self.map_block(node, logical)
            if block:
                self.release_block(block)
        if node["indirect"]:
            if node["levels"] >= 2:
                top = self.read(node["indirect"])
                for slot in range(INDIRECT_ENTRIES):
                    table = struct.unpack_from("<Q", top, slot * 8)[0]
                    if table:
                        self.release_block(table)
            self.release_block(node["indirect"])
        node["direct"] = [0] * DIRECT_BLOCKS
        node["indirect"] = 0
        node["levels"] = 1
        node["size"] = 0

    def replace_content(self, node, content):
        """Rewrites a file in place (same node, same links) when its content
        changed. Returns whether it did."""
        if node["size"] == len(content) and self.read_content(node) == content:
            return False
        self.free_content(node)
        self.write_content(node, content)
        return True

    def write_content(self, node, content):
        for logical in range(-(-len(content) // BLOCK)):
            block = self.ensure_block(node, logical)
            self.write(block, content[logical * BLOCK:(logical + 1) * BLOCK], cache=False)
        node["size"] = len(content)
        self.write_node(node)

    def read_content(self, node):
        out = bytearray()
        for logical in range(-(-node["size"] // BLOCK)):
            block = self.map_block(node, logical)
            out += self.read(block, cache=False) if block else bytes(BLOCK)
        return bytes(out[:node["size"]])

    # --- links ---
    def links(self, node):
        out = []
        for logical in range(-(-node["size"] // BLOCK)):
            block = self.map_block(node, logical)
            if not block:
                continue
            data = self.read(block)
            for slot in range(BLOCK // self.link_size):
                raw = data[slot * self.link_size:(slot + 1) * self.link_size]
                target = struct.unpack_from("<Q", raw, self.name_max)[0]
                if target:
                    out.append((raw[:self.name_max].split(b"\0")[0].decode("utf-8", "replace"), target))
        return out

    def find(self, node, name):
        for existing, target in self.links(node):
            if existing == name:
                return target
        return None

    def insert_link(self, parent, name, target):
        encoded = bytearray(self.link_size)
        raw = ascii_name(name).encode()
        if len(raw) >= self.name_max:
            raise ValueError(f"nom trop long ({len(raw)} caracteres) : {name}")
        encoded[:len(raw)] = raw
        struct.pack_into("<Q", encoded, self.name_max, target)
        blocks = -(-parent["size"] // BLOCK)
        for logical in range(blocks):
            block = self.map_block(parent, logical)
            data = bytearray(self.read(block))
            for slot in range(BLOCK // self.link_size):
                at = slot * self.link_size
                if struct.unpack_from("<Q", data, at + self.name_max)[0] == 0:
                    data[at:at + self.link_size] = encoded
                    self.write(block, bytes(data))
                    return
        block = self.ensure_block(parent, blocks)
        data = bytearray(BLOCK)
        data[:self.link_size] = encoded
        self.write(block, bytes(data))
        parent["size"] = (blocks + 1) * BLOCK
        self.write_node(parent)

    def resolve(self, path, create_dirs=False):
        node = self.read_node(ROOT_NODE)
        for part in [p for p in path.split("/") if p]:
            target = self.find(node, part)
            if target is None:
                if not create_dirs:
                    return None
                child = self.new_node(self.allocate_node(), KIND_DIR, SYSTEM_DIR, links=1)
                self.write_node(child)
                self.insert_link(node, part, child["id"])
                node = self.read_node(child["id"])
                continue
            node = self.read_node(target)
        return node


def resolve_link(source, path, depth=0):
    """Where a symbolic link of the tree points, inside the tree.

    An absolute target is read from the imported root, as the system that
    will read it does; a relative one from the link's directory. None when
    it leaves the tree or leads nowhere."""
    if depth > 8:
        return None
    root = os.path.realpath(source)
    # Everything absolute before comparing: a relative target joined to a
    # relative directory stays relative, and never starts with the root.
    here = os.path.realpath(os.path.dirname(path))
    target = os.readlink(path)
    target = os.path.join(root, target.lstrip("/")) if target.startswith("/") else os.path.join(here, target)
    target = os.path.normpath(target)
    if not target.startswith(root):
        return None
    if os.path.islink(target):
        return resolve_link(source, target, depth + 1)
    return target if os.path.exists(target) else None


def import_tree(v, source, destination):
    """Writes the host tree `source` at `destination`.

    Into a volume that already holds it -- the second `make webroot` --
    it is an update: a directory already there is entered, a file already
    there is rewritten in place when its content changed, a link already
    there is kept. The first version inserted a second link of the same
    name beside the old one: the old file stayed the one found, and the
    new one's blocks were lost."""
    files = dirs = skipped = links = updated = 0
    pending = []
    dangling = []
    total = 0
    parent = v.resolve(destination, create_dirs=True)
    stack = [(source, parent["id"])]
    while stack:
        directory, parent_id = stack.pop()
        for entry in sorted(os.listdir(directory)):
            path = os.path.join(directory, entry)
            parent = v.read_node(parent_id)
            if os.path.islink(path):
                # A symbolic link becomes a second link to the same node:
                # Nova's filesystem has one kind of link, the one a
                # directory holds (spec 4.1), and busybox's hundred
                # applets are exactly that -- a hundred names for one file.
                # An absolute target names the *image's* root, not the
                # host's: /bin/sh points at the busybox of the image.
                target = resolve_link(source, path)
                if target is None:
                    dangling.append(os.path.relpath(path, source))
                    continue
                relative = os.path.relpath(target, os.path.realpath(source))
                if v.find(parent, ascii_name(entry)) is not None:
                    continue
                node = v.resolve(f"{destination}/{relative}")
                if node is None:
                    # Its target has not been imported yet: seen again on
                    # a second pass, once the tree is complete.
                    pending.append((os.path.relpath(path, source), relative))
                    continue
                v.insert_link(parent, entry, node["id"])
                node["links"] += 1
                v.write_node(node)
                links += 1
                continue
            existing = v.find(parent, ascii_name(entry))
            if existing is not None:
                there = v.read_node(existing)
                if os.path.isdir(path) and there and there["kind"] == KIND_DIR:
                    stack.append((path, existing))
                elif not os.path.isdir(path) and there and there["kind"] == KIND_FILE:
                    with open(path, "rb") as f:
                        if v.replace_content(there, f.read()):
                            updated += 1
                else:
                    print(f"[!!] ignore : {entry} existe deja sous une autre forme")
                    skipped += 1
                continue
            if os.path.isdir(path):
                child = v.new_node(v.allocate_node(), KIND_DIR, SYSTEM_DIR, links=1)
                v.write_node(child)
                v.insert_link(parent, entry, child["id"])
                dirs += 1
                stack.append((path, child["id"]))
                continue
            with open(path, "rb") as f:
                content = f.read()
            executable = os.access(path, os.X_OK)
            node = v.new_node(v.allocate_node(), KIND_FILE,
                              SYSTEM_BINARY if executable else SYSTEM_FILE, links=1)
            v.write_node(node)
            v.write_content(node, content)
            try:
                v.insert_link(parent, entry, node["id"])
            except ValueError as error:
                print(f"[!!] ignore : {error}")
                skipped += 1
                continue
            files += 1
            total += len(content)
            if files % 200 == 0:
                print(f"    {files} fichiers, {total // (1024 * 1024)} Mio", end="\r", flush=True)
    # Second pass: the links whose target had not been imported yet.
    for link_path, relative in pending:
        parent_path = os.path.dirname(link_path)
        parent = v.resolve(f"{destination}/{parent_path}" if parent_path else destination)
        node = v.resolve(f"{destination}/{relative}")
        if parent is None or node is None:
            skipped += 1
            continue
        if v.find(parent, ascii_name(os.path.basename(link_path))) is not None:
            continue
        try:
            v.insert_link(parent, os.path.basename(link_path), node["id"])
        except ValueError as error:
            print(f"[!!] ignore : {error}")
            skipped += 1
            continue
        node["links"] += 1
        v.write_node(node)
        links += 1
    if dangling:
        print(f"[..] {len(dangling)} lien(s) hors de l'arbre ignore(s), par exemple {dangling[0]}")
    v.write_superblock()
    v.flush()
    print(f"[ok] {files} fichiers, {dirs} dossiers, {links} liens supplementaires, "
          f"{total // (1024 * 1024)} Mio ecrits dans {destination}"
          + (f" - {updated} fichier(s) existant(s) mis a jour" if updated else "")
          + (f" - {skipped} ignore(s)" if skipped else ""))


def migrate(source, target_path, size_mib):
    """Copies a volume into a new one: same tree, current format."""
    old = Volume(source)
    new = Volume(target_path, create_blocks=size_mib * 1024 * 1024 // BLOCK, label=old.label)
    files = dirs = 0

    def copy(old_node, new_parent_id, path):
        nonlocal files, dirs
        for name, target in old.links(old_node):
            child = old.read_node(target)
            if not child:
                continue
            parent = new.read_node(new_parent_id)
            if child["kind"] == KIND_DIR:
                created = new.new_node(new.allocate_node(), KIND_DIR, tuple(child["perm"]), links=1)
                # Owner and group travel with the node: a home directory
                # rewritten as root's would lock its user out.
                created["owner"], created["group"] = child["owner"], child["group"]
                new.write_node(created)
                new.insert_link(parent, name, created["id"])
                dirs += 1
                copy(child, created["id"], f"{path}/{name}")
            else:
                created = new.new_node(new.allocate_node(), KIND_FILE, tuple(child["perm"]), links=1)
                created["owner"], created["group"] = child["owner"], child["group"]
                new.write_node(created)
                new.write_content(created, old.read_content(child))
                new.insert_link(parent, name, created["id"])
                files += 1

    copy(old.read_node(ROOT_NODE), ROOT_NODE, "")
    new.write_superblock()
    new.flush()
    print(f"[ok] {files} fichiers, {dirs} dossiers repris dans {target_path} ({size_mib} Mio)")


def put_file(v, source, path):
    """Writes one host file at `path`, replacing the content of the file
    already there (same node, same links) or creating it. What a rebuilt
    library needs, without re-importing the whole browser root."""
    with open(source, "rb") as f:
        content = f.read()
    node = v.resolve(path)
    if node is None:
        parent = v.resolve(os.path.dirname(path), create_dirs=True)
        mode = SYSTEM_BINARY if os.access(source, os.X_OK) else SYSTEM_FILE
        node = v.new_node(v.allocate_node(), KIND_FILE, mode, links=1)
        v.write_node(node)
        v.insert_link(parent, os.path.basename(path), node["id"])
    elif node["kind"] != KIND_FILE:
        sys.exit(f"{path} : pas un fichier")
    v.replace_content(node, content)
    v.write_superblock()
    v.flush()
    print(f"[ok] {path} : {len(content)} octets (noeud {node['id']})")


def main():
    ap = argparse.ArgumentParser()
    sub = ap.add_subparsers(dest="command", required=True)
    f = sub.add_parser("format"); f.add_argument("image"); f.add_argument("--size", type=int, default=2048)
    f.add_argument("--label", default="nova-systeme")
    i = sub.add_parser("import"); i.add_argument("image"); i.add_argument("source"); i.add_argument("destination")
    l = sub.add_parser("ls"); l.add_argument("image"); l.add_argument("path", nargs="?", default="/")
    c = sub.add_parser("cat"); c.add_argument("image"); c.add_argument("path")
    p = sub.add_parser("put"); p.add_argument("image"); p.add_argument("source"); p.add_argument("path")
    m = sub.add_parser("migrate"); m.add_argument("source"); m.add_argument("target")
    m.add_argument("--size", type=int, default=2048)
    a = ap.parse_args()

    if a.command == "migrate":
        migrate(a.source, a.target, a.size)
        return
    if a.command == "format":
        v = Volume(a.image, create_blocks=a.size * 1024 * 1024 // BLOCK, label=a.label)
        v.flush()
        print(f"[ok] volume \"{v.label}\" : {v.total} blocs, {v.node_count} noeuds")
        return
    v = Volume(a.image)
    if a.command == "import":
        import_tree(v, a.source, a.destination)
    elif a.command == "ls":
        node = v.resolve(a.path)
        if not node:
            sys.exit(f"{a.path} : introuvable")
        for name, target in sorted(v.links(node)):
            child = v.read_node(target)
            kind = "/" if child and child["kind"] == KIND_DIR else ""
            print(f"{child['size'] if child else 0:>10}  {name}{kind}  -> noeud {target}")
    elif a.command == "put":
        put_file(v, a.source, a.path)
    elif a.command == "cat":
        node = v.resolve(a.path)
        if not node:
            sys.exit(f"{a.path} : introuvable")
        sys.stdout.buffer.write(v.read_content(node))


if __name__ == "__main__":
    main()
