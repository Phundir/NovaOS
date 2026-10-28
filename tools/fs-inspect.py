#!/usr/bin/env python3
"""Shows what a Nova OS volume really is on disk, next to what programs see.

Spec 4.1: folders are not real. The disk holds one flat table of numbered
nodes; a "folder" is a node whose content is a table of links (name ->
node number), and a name exists nowhere else. The tree a program sees is
rebuilt by the kernel's path resolution (SYS_FS_*), link table by link
table. This tool reads the image directly, without the kernel, and puts
the two side by side:

  * the reality: the node table, in the order of the disk, and the
    blocks where each node's bytes lie -- wherever the allocator found
    room, with no relation to the folders;
  * the view: the tree obtained by following the links from node 1.

It checks as it goes that every node in use is reached by at least one
link, and lists the nodes reached by several (the same file under two
names, spec 4.1.4).

Usage: tools/fs-inspect.py [disks/nova-disk.img] [--nodes N] [--depth D]
"""
import argparse
import struct
import sys

BLOCK = 4096
NODE_SIZE = 128
LINK_SIZE = 64
NAME_MAX = 56
DIRECT = 5
KINDS = {0: "libre", 1: "fichier", 2: "table de liens"}


def u16(b, o): return struct.unpack_from("<H", b, o)[0]
def u32(b, o): return struct.unpack_from("<I", b, o)[0]
def u64(b, o): return struct.unpack_from("<Q", b, o)[0]


class Volume:
    def __init__(self, path):
        self.f = open(path, "rb")
        sb = self.block(0)
        if sb[:8] != b"NOVAFS\0\0":
            sys.exit(f"{path} : pas un volume Nova OS")
        self.version = u32(sb, 0x08)
        # Version 1 wrote 64-byte link entries, version 2 writes 128.
        self.link_size = 64 if self.version < 2 else 128
        self.name_max = self.link_size - 8
        self.total = u64(sb, 0x10)
        self.node_count = u64(sb, 0x18)
        self.table = u64(sb, 0x40)
        self.data_start = u64(sb, 0x50)
        self.root = u64(sb, 0x58)
        self.free_blocks = u64(sb, 0x60)
        self.free_nodes = u64(sb, 0x68)
        self.label = sb[0x80:0xA0].split(b"\0")[0].decode("ascii", "replace")

    def block(self, n):
        self.f.seek(n * BLOCK)
        return self.f.read(BLOCK)

    def node(self, node_id):
        index = node_id - 1
        raw = self.block(self.table + index // (BLOCK // NODE_SIZE))
        o = (index % (BLOCK // NODE_SIZE)) * NODE_SIZE
        slot = raw[o:o + NODE_SIZE]
        kind = slot[8]
        if kind == 0:
            return None
        return {
            "id": u64(slot, 0), "kind": kind, "links": u16(slot, 0x0A),
            "levels": max(slot[9], 1), "size": u64(slot, 0x18),
            "direct": [u64(slot, 0x50 + 8 * i) for i in range(DIRECT)],
            "indirect": u64(slot, 0x78),
        }

    def blocks(self, node):
        """Data blocks of a node, through one or two levels of indirection."""
        found = [b for b in node["direct"] if b]
        if node["indirect"]:
            tables = [self.block(node["indirect"])]
            if node["levels"] >= 2:
                tables = [self.block(t) for t in struct.unpack_from("<512Q", tables[0]) if t]
            for raw in tables:
                found += [b for b in struct.unpack_from("<512Q", raw) if b]
        return found

    def links(self, node):
        out = []
        for b in self.blocks(node):
            raw = self.block(b)
            for s in range(BLOCK // self.link_size):
                slot = raw[s * self.link_size:(s + 1) * self.link_size]
                target = u64(slot, self.name_max)
                if target:
                    name = slot[:self.name_max].split(b"\0")[0].decode("utf-8", "replace")
                    out.append((name, target))
        return out


def ranges(blocks):
    """[10, 11, 12, 40] -> '10-12, 40'"""
    if not blocks:
        return "-"
    out, start, prev = [], blocks[0], blocks[0]
    for b in blocks[1:] + [None]:
        if b is not None and b == prev + 1:
            prev = b
            continue
        out.append(f"{start}" if start == prev else f"{start}-{prev}")
        if b is not None:
            start = prev = b
    return ", ".join(out)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("image", nargs="?", default="disks/nova-disk.img")
    ap.add_argument("--nodes", type=int, default=25, help="noeuds montres dans la table")
    ap.add_argument("--depth", type=int, default=3, help="profondeur de la vue")
    a = ap.parse_args()
    v = Volume(a.image)

    used = {}
    for node_id in range(1, v.node_count + 1):
        n = v.node(node_id)
        if n:
            used[node_id] = n
    tables = sum(1 for n in used.values() if n["kind"] == 2)
    print(f"Volume \"{v.label}\" (format v{v.version}) : {v.total} blocs de 4 Kio, table de {v.node_count} noeuds")
    print(f"  {len(used)} noeuds occupes : {len(used) - tables} fichiers, {tables} tables de liens")
    print(f"  {v.free_blocks} blocs libres, {v.free_nodes} noeuds libres")

    # --- The reality: the flat table ---
    print("\n=== Sur le disque : une seule table de noeuds, sans nom ni dossier ===")
    print(f"{'noeud':>6}  {'nature':<15} {'liens':>5} {'taille':>9}  blocs de donnees")
    for node_id in sorted(used)[: a.nodes]:
        n = used[node_id]
        print(f"{node_id:>6}  {KINDS[n['kind']]:<15} {n['links']:>5} {n['size']:>9}  {ranges(v.blocks(n))}")
    if len(used) > a.nodes:
        print(f"   ... {len(used) - a.nodes} autres")

    # --- Walk the links from the root, as the kernel's resolution does ---
    reached = {}  # node -> list of paths naming it
    order = []

    def walk(node_id, path, depth, seen):
        reached.setdefault(node_id, []).append(path or "/")
        n = used.get(node_id)
        if not n or n["kind"] != 2 or node_id in seen:
            return
        seen = seen | {node_id}
        for name, target in v.links(n):
            child = f"{path}/{name}"
            order.append((depth, name, target, child))
            walk(target, child, depth + 1, seen)

    walk(v.root, "", 0, frozenset())

    print("\n=== Ce que voient les programmes : l'arbre reconstruit en suivant les liens ===")
    print("(nom -> noeud : chaque nom n'existe que dans la table de liens du parent)")
    for depth, name, target, _ in order:
        if depth < a.depth:
            kind = KINDS[used[target]["kind"]] if target in used else "??"
            print(f"{'  ' * depth}{name}{'/' if kind == 'table de liens' else ''} -> noeud {target}")

    # --- Checks ---
    print("\n=== Verifications ===")
    orphans = [i for i in used if i not in reached]
    print(f"  noeuds occupes atteints par au moins un lien : {len(used) - len(orphans)}/{len(used)}")
    if orphans:
        print(f"  ORPHELINS (occupes, sans aucun lien) : {orphans[:20]}")
    multi = {i: p for i, p in reached.items() if len(p) > 1}
    print(f"  noeuds nommes par plusieurs liens : {len(multi)}")
    for i, paths in list(multi.items())[:10]:
        print(f"    noeud {i} : {' | '.join(paths)}")
    # Where a folder's files lie relative to the folder itself.
    for depth, name, target, path in order:
        n = used.get(target)
        if n and n["kind"] == 1 and n["size"] > BLOCK:
            parent = "/".join(path.split("/")[:-1]) or "/"
            print(f"\n  Exemple : {path}")
            print(f"    noeud {target}, {n['size']} octets, blocs {ranges(v.blocks(n))}")
            print(f"    le dossier {parent} n'est qu'une table de liens : il ne contient pas ces blocs,")
            print(f"    il contient la ligne \"{name} -> {target}\".")
            break


if __name__ == "__main__":
    main()
