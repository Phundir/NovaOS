#!/usr/bin/env python3
"""Wraps a GPU driver module into a .kmd container (spec 3.6).

    tools/nova-kmd.py module.so out.kmd --name NAME [--vendor TEXT]
                      [--signature system|enterprise|self]
                      [--authority HEX32] [--interface 0x00010000]

The container, read by src/gpu/module.rs field by field:

    0    "KMD\\0"
    4    u16  container version (1)
    6    u16  class (1: GPU)
    8    u32  backend interface version the module was built for
    12   u32  signature level: 1 SELF, 2 ENTERPRISE, 3 SYSTEM
    16   u64  payload offset         24  u64  payload size
    32   [32] SHA-256 of the payload (the ELF)
    64   [48] name, ASCII            112 [32] vendor, ASCII
    144  [32] ENTERPRISE: authority key   176 [32] ENTERPRISE: binding
    208  reserved to 256; the payload follows

The authenticity digest -- what SYSTEM looks up in the kernel's table and
ENTERPRISE binds to its authority -- is SHA-256 of bytes 0..144 of the
header followed by the payload. The binding is SHA-256(key || digest), as
for the binaries of spec 3.2 (and with the same reservation: no asymmetric
signature yet, see src/binfmt/signature.rs).
"""
import argparse
import hashlib
import struct
from pathlib import Path

HEADER = 256
LEVELS = {'self': 1, 'enterprise': 2, 'system': 3}


def build(payload, name, vendor, level, interface, authority=b''):
    header = bytearray(HEADER)
    header[0:4] = b'KMD\0'
    struct.pack_into('<HHII', header, 4, 1, 1, interface, LEVELS[level])
    struct.pack_into('<QQ', header, 16, HEADER, len(payload))
    header[32:64] = hashlib.sha256(payload).digest()
    header[64:64 + len(name)] = name.encode('ascii')[:47]
    header[112:112 + len(vendor)] = vendor.encode('ascii')[:31]
    digest = hashlib.sha256(bytes(header[:144]) + payload).digest()
    if level == 'enterprise':
        header[144:176] = authority
        header[176:208] = hashlib.sha256(authority + digest).digest()
    return bytes(header) + payload, digest


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('module', type=Path)
    parser.add_argument('out', type=Path)
    parser.add_argument('--name', required=True)
    parser.add_argument('--vendor', default='')
    parser.add_argument('--signature', choices=LEVELS, default='system')
    parser.add_argument('--authority', default='')
    parser.add_argument('--interface', default='0x00010000')
    args = parser.parse_args()
    payload = args.module.read_bytes()
    authority = bytes.fromhex(args.authority) if args.authority else b'\0' * 32
    data, digest = build(payload, args.name, args.vendor, args.signature, int(args.interface, 0), authority)
    args.out.write_bytes(data)
    print('[ok] %s : %d octets, empreinte %s (%s)' % (args.out, len(data), digest.hex(), args.signature))


if __name__ == '__main__':
    main()
