#!/usr/bin/env python3
"""Wraps an ELF64 x86-64 program into a Nova binary: a linked image
(spec 3.5).

    tools/nova-wrap.py <elf> <out.cus> --app-id os.nova.web-browser \\
        --name "Web Browser" [--author ...] [--group ...] [--version 1.0.0.0] \\
        [--permissions NETWORK_INTERNET,AUDIO_OUTPUT,...] [--flags GUI,NETWORK]

The result is the 256-byte header the kernel's `binfmt` reads (the same
layout as src/binfmt/header.rs, field by field), padding to 4096 bytes,
then the ELF unchanged as the payload: on a page boundary, so that the
kernel maps its segments straight from the file's pages. The header
declares ABI version 2 and the IMAGE_LIEE flag; its digest is the
SHA-256 of the payload, recomputed by the kernel at every start. The
signature is SELF (spec 3.2: an unverified developer, a yellow warning at
installation), with no certificate block.

The ELF keeps whatever interpreter it names: the kernel gives every
program of the Nova interface the Nova C library's linker instead.
"""
import argparse, hashlib, struct, sys

HEADER_SIZE = 256
PAYLOAD_OFFSET = 4096
NAME_LEN = 32
FORMATS = {"dsa": (b"DSA\0", 1), "ss": (b"SS\0\0", 2), "gs": (b"GS\0\0", 3), "cus": (b"CUS\0", 4), "ras": (b"RAS\0", 5)}
SIGNATURE_SELF = 1
FLAGS = {"GUI": 1 << 0, "BACKGROUND": 1 << 1, "AUTO_PAUSE": 1 << 2, "EXPOSES_API": 1 << 3,
         "REPLACES_COMPONENT": 1 << 4, "DEFAULT_CAPABLE": 1 << 5, "REQUIRES_ADMIN": 1 << 6,
         "SERVICE": 1 << 7, "NETWORK": 1 << 8, "IMAGE_LIEE": 1 << 9}
PERMISSIONS = {"NETWORK_INTERNET": 1 << 0, "NETWORK_LOCAL": 1 << 1, "MICROPHONE": 1 << 2, "WEBCAM": 1 << 3,
               "AUDIO_OUTPUT": 1 << 4, "FS_USER_HOME": 1 << 9, "IPC_INTERNAL_PORTS": 1 << 12,
               "NOTIFICATIONS": 1 << 13, "FULLSCREEN": 1 << 14, "CLIPBOARD": 1 << 15}


def elf_layout(elf):
    """(entry, lowest page of the loaded segments, span) of an ELF64 x86-64."""
    if elf[:4] != b"\x7fELF" or elf[4] != 2 or elf[5] != 1 or struct.unpack_from("<H", elf, 18)[0] != 62:
        sys.exit("pas un ELF64 x86-64")
    entry, phoff = struct.unpack_from("<QQ", elf, 24)
    phentsize, phnum = struct.unpack_from("<HH", elf, 54)
    loads = []
    for i in range(phnum):
        p = phoff + i * phentsize
        kind = struct.unpack_from("<I", elf, p)[0]
        if kind == 1:
            vaddr, _, _, memsz = struct.unpack_from("<QQQQ", elf, p + 16)
            loads.append((vaddr, vaddr + memsz))
    if not loads:
        sys.exit("aucun segment chargeable")
    low = min(v for v, _ in loads) & ~0xFFF
    high = max(e for _, e in loads)
    return entry, low, high - low


def name(text):
    raw = text.encode("ascii")[:NAME_LEN]
    return raw + b"\0" * (NAME_LEN - len(raw))


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("elf")
    ap.add_argument("out")
    ap.add_argument("--app-id", required=True)
    ap.add_argument("--name", required=True)
    ap.add_argument("--author", default="Nova OS")
    ap.add_argument("--group", default="apps")
    ap.add_argument("--version", default="1.0.0.0")
    ap.add_argument("--type", default="cus", choices=FORMATS)
    ap.add_argument("--permissions", default="")
    ap.add_argument("--flags", default="GUI")
    ap.add_argument("--stack", type=int, default=8 << 20)
    a = ap.parse_args()

    elf = open(a.elf, "rb").read()
    entry, low, span = elf_layout(elf)
    magic, code = FORMATS[a.type]
    flags = FLAGS["IMAGE_LIEE"]
    for f in filter(None, a.flags.split(",")):
        flags |= FLAGS[f]
    permissions = 0
    for p in filter(None, a.permissions.split(",")):
        permissions |= PERMISSIONS[p]
    version = [int(x) for x in a.version.split(".")] + [0, 0, 0, 0]

    h = bytearray(HEADER_SIZE)
    h[0x00:0x04] = magic
    struct.pack_into("<HHH", h, 0x04, 1, HEADER_SIZE, 2)   # header version, size, ABI 2
    h[0x0A] = code
    h[0x0B] = SIGNATURE_SELF
    struct.pack_into("<I", h, 0x0C, flags)
    struct.pack_into("<QQQQQ", h, 0x10, entry, low, span, 0, a.stack)
    struct.pack_into("<II", h, 0x38, 0, 0)                  # no section of its own
    h[0x40:0x60] = name(a.app_id)
    h[0x60:0x80] = name(a.author)
    h[0x80:0xA0] = name(a.group)
    h[0xA0:0xC0] = name(a.name)
    struct.pack_into("<HHHH", h, 0xC0, *version[:4])
    struct.pack_into("<Q", h, 0xC8, permissions)
    struct.pack_into("<HH", h, 0xD0, 0, 0)                  # no internal port
    struct.pack_into("<III", h, 0xD4, PAYLOAD_OFFSET, len(elf), 0)
    h[0xE0:0x100] = hashlib.sha256(elf).digest()

    with open(a.out, "wb") as out:
        out.write(bytes(h))
        out.write(b"\0" * (PAYLOAD_OFFSET - HEADER_SIZE))
        out.write(elf)
    print(f"[ok] {a.out} : image liee .{a.type} de {len(elf)} octets, entree 0x{entry:x}, "
          f"empreinte {hashlib.sha256(elf).hexdigest()[:16]}")


if __name__ == "__main__":
    main()
