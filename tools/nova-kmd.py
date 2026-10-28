#!/usr/bin/env python3
"""Wraps a GPU driver module into a .kmd container (spec 3.6).

    tools/nova-kmd.py module.so out.kmd --name NAME [--vendor TEXT]
                      [--signature system|enterprise|self]
                      [--seed SEED-FILE] [--interface 0x00010000]

The container, read by src/gpu/module.rs field by field:

    0    "KMD\\0"
    4    u16  container version (1; 2 for ENTERPRISE)
    6    u16  class (1: GPU)
    8    u32  backend interface version the module was built for
    12   u32  signature level: 1 SELF, 2 ENTERPRISE, 3 SYSTEM
    16   u64  payload offset         24  u64  payload size
    32   [32] SHA-256 of the payload (the ELF)
    64   [48] name, ASCII            112 [32] vendor, ASCII
    144  [32] ENTERPRISE: Ed25519 public key of the authority
    176  [64] ENTERPRISE: Ed25519 signature (container version 2)
    240  reserved to 256; the payload follows

The authenticity digest -- what SYSTEM looks up in the kernel's table and
ENTERPRISE signs -- is SHA-256 of bytes 0..144 of the header followed by
the payload. The authority signs "NOVA-KMD\0" || key || digest with the
seed given by --seed (tools/novasign.py keygen), as the binaries and the
packages of spec 3.2 (src/binfmt/signature.rs). Version 1's binding, a
SHA-256 of public data, is refused by the kernel since phase 15.
"""
import argparse
import hashlib
import importlib.util
import struct
from pathlib import Path


def novasign():
    path = Path(__file__).with_name('novasign.py')
    spec = importlib.util.spec_from_file_location('novasign', path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module

HEADER = 256
LEVELS = {'self': 1, 'enterprise': 2, 'system': 3}


def build(payload, name, vendor, level, interface, seed=None):
    header = bytearray(HEADER)
    header[0:4] = b'KMD\0'
    version = 2 if level == 'enterprise' else 1
    struct.pack_into('<HHII', header, 4, version, 1, interface, LEVELS[level])
    struct.pack_into('<QQ', header, 16, HEADER, len(payload))
    header[32:64] = hashlib.sha256(payload).digest()
    header[64:64 + len(name)] = name.encode('ascii')[:47]
    header[112:112 + len(vendor)] = vendor.encode('ascii')[:31]
    digest = hashlib.sha256(bytes(header[:144]) + payload).digest()
    if level == 'enterprise':
        if seed is None:
            raise SystemExit('ENTERPRISE : --seed requis (tools/novasign.py keygen)')
        sign = novasign()
        key = sign.public_key(seed)
        header[144:176] = key
        header[176:240] = sign.sign(seed, sign.DOMAIN_MODULE + key + digest)
    return bytes(header) + payload, digest


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('module', type=Path)
    parser.add_argument('out', type=Path)
    parser.add_argument('--name', required=True)
    parser.add_argument('--vendor', default='')
    parser.add_argument('--signature', choices=LEVELS, default='system')
    parser.add_argument('--seed', type=Path, help='graine de l\'autorite (ENTERPRISE)')
    parser.add_argument('--interface', default='0x00010000')
    args = parser.parse_args()
    payload = args.module.read_bytes()
    seed = bytes.fromhex(args.seed.read_text().strip()) if args.seed else None
    data, digest = build(payload, args.name, args.vendor, args.signature, int(args.interface, 0), seed)
    args.out.write_bytes(data)
    print('[ok] %s : %d octets, empreinte %s (%s)' % (args.out, len(data), digest.hex(), args.signature))


if __name__ == '__main__':
    main()
