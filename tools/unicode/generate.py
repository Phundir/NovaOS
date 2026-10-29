#!/usr/bin/env python3
"""Writes src/python/modules/unidata.bin, the Unicode Character Database
tables of Nova's Python (unicodedata, the \\N{...} escape), from the host
CPython's unicodedata module (`make unicode`).

The file is a sequence of sections, each `tag (4 bytes) length (u32 LE)
payload`:

  PROP  runs of identical properties over all code points, fixed 8-byte
        records: start (u24), category, bidirectional, east asian width,
        combining class, mirrored (u8 each, the strings indexed in STRS)
  STRS  the category / bidi / width names, NUL-separated
  WORD  the words of character names, NUL-separated, most frequent first
  NAME  for each named code point (algorithmic names excluded), in order:
        varint(code point delta), varint(word count), varint word indices
  ALGO  ranges with algorithmic names: start (u24), end (u24), kind (u8)
        kind 0: CJK UNIFIED IDEOGRAPH-%X, 1: HANGUL SYLLABLE (computed),
        2: TANGUT IDEOGRAPH-%X, 3: KHITAN SMALL SCRIPT CHARACTER-%X,
        4: NUSHU CHARACTER-%X, 5: CJK COMPATIBILITY IDEOGRAPH-%X
  DIDX  decomposition index: cp (u24), offset into DECO (u24), sorted
  DECO  decompositions: tag (u8, 0 canonical, else STRS index of the
        <tag>), count (u8), code points (u24 each)
  COMP  primary composites: first (u24), second (u24), composite (u24),
        sorted by (first, second)
  NUMB  numeric values: cp (u24), decimal (i8, -1 none), digit (i8),
        numerator (i64), denominator (u32); sorted
"""

import os
import struct
import sys
import unicodedata as u

OUT = os.path.join(os.path.dirname(__file__), '..', '..', 'src', 'python', 'modules', 'unidata.bin')

ALGO_PREFIX = ['CJK UNIFIED IDEOGRAPH-', 'HANGUL SYLLABLE ', 'TANGUT IDEOGRAPH-',
               'KHITAN SMALL SCRIPT CHARACTER-', 'NUSHU CHARACTER-', 'CJK COMPATIBILITY IDEOGRAPH-']


def u24(n):
    return struct.pack('<I', n)[:3]


def varint(n):
    out = bytearray()
    while True:
        b = n & 0x7f
        n >>= 7
        if n:
            out.append(b | 0x80)
        else:
            out.append(b)
            return bytes(out)


def algo_kind(name, cp):
    for k, prefix in enumerate(ALGO_PREFIX):
        if name.startswith(prefix):
            if k == 1 or name == prefix + '%X' % cp or name == prefix + '%04X' % cp:
                return k
    return None


def main():
    strs = []

    def sid(s):
        if s not in strs:
            strs.append(s)
        return strs.index(s)

    # Properties.
    prop = bytearray()
    prev = None
    for cp in range(sys.maxunicode + 1):
        c = chr(cp)
        key = (sid(u.category(c)), sid(u.bidirectional(c)), sid(u.east_asian_width(c)), u.combining(c), u.mirrored(c))
        if key != prev:
            prop += u24(cp) + bytes(key)
            prev = key

    # Names.
    named = []
    algo = []
    words = {}
    for cp in range(sys.maxunicode + 1):
        name = u.name(chr(cp), '')
        if not name:
            continue
        k = algo_kind(name, cp)
        if k is not None:
            if algo and algo[-1][2] == k and algo[-1][1] == cp - 1:
                algo[-1][1] = cp
            else:
                algo.append([cp, cp, k])
            continue
        parts = name.split(' ')
        for w in parts:
            words[w] = words.get(w, 0) + 1
        named.append((cp, parts))
    order = sorted(words, key=lambda w: (-words[w], w))
    index = {w: i for i, w in enumerate(order)}
    names = bytearray()
    last = 0
    for cp, parts in named:
        names += varint(cp - last) + varint(len(parts))
        for w in parts:
            names += varint(index[w])
        last = cp
    algos = bytearray()
    for start, end, k in algo:
        algos += u24(start) + u24(end) + bytes([k])

    # Decompositions.
    didx = bytearray()
    deco = bytearray()
    for cp in range(sys.maxunicode + 1):
        d = u.decomposition(chr(cp))
        if not d:
            continue
        fields = d.split()
        tag = 0
        if fields[0].startswith('<'):
            tag = sid(fields[0])
            fields = fields[1:]
        didx += u24(cp) + u24(len(deco))
        deco += bytes([tag, len(fields)]) + b''.join(u24(int(f, 16)) for f in fields)

    # Primary composites: canonical pairs that NFC recomposes.
    comp = []
    for cp in range(sys.maxunicode + 1):
        d = u.decomposition(chr(cp))
        if not d or d.startswith('<'):
            continue
        fields = [int(f, 16) for f in d.split()]
        if len(fields) != 2:
            continue
        if u.normalize('NFC', chr(fields[0]) + chr(fields[1])) == chr(cp):
            comp.append((fields[0], fields[1], cp))
    comp.sort()
    comps = b''.join(u24(a) + u24(b) + u24(c) for a, b, c in comp)

    # Numeric values.
    from fractions import Fraction
    numb = bytearray()
    for cp in range(sys.maxunicode + 1):
        c = chr(cp)
        n = u.numeric(c, None)
        if n is None:
            continue
        f = Fraction(n).limit_denominator(1000)
        dec = u.decimal(c, -1)
        dig = u.digit(c, -1)
        numb += u24(cp) + struct.pack('<bbqI', dec, dig, f.numerator, f.denominator)

    sections = [
        (b'PROP', bytes(prop)),
        (b'STRS', b'\0'.join(s.encode('ascii') for s in strs)),
        (b'WORD', b'\0'.join(w.encode('ascii') for w in order)),
        (b'NAME', bytes(names)),
        (b'ALGO', bytes(algos)),
        (b'DIDX', bytes(didx)),
        (b'DECO', bytes(deco)),
        (b'COMP', comps),
        (b'NUMB', bytes(numb)),
    ]
    with open(OUT, 'wb') as f:
        f.write(b'UCD' + u.unidata_version.encode('ascii').ljust(13, b'\0'))
        for tag, payload in sections:
            f.write(tag + struct.pack('<I', len(payload)) + payload)
    total = sum(len(p) for _, p in sections)
    print('unidata.bin: Unicode %s, %d bytes (%d names, %d words, %d runs, %d decompositions, %d composites)'
          % (u.unidata_version, total, len(named), len(order), len(prop) // 8, len(didx) // 6, len(comp)))


main()
