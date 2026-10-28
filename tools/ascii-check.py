#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""Checks that the kernel's displayable strings are printable ASCII.

The VGA writer and the graphical console alike replace any byte outside
the 0x20-0x7E range with a solid square: an em dash in a `kprintln!`
shows up as three squares on screen. The rule has existed since the
start of the project and 233 em dashes broke it anyway, across 55
files, because nothing checked it -- and because the serial console, for
its part, displays UTF-8 correctly.

This script analyses **Rust string literals**, really telling strings
apart from comments: only what is displayed is constrained. Comments and
documentation are English ASCII by convention, but that is not what this
script enforces.

    python3 tools/ascii-check.py           # reports, exit code 1 on a fault
    python3 tools/ascii-check.py --apply   # fixes the known characters

Called by `make check`.
"""

import os, sys, collections

def scan(src):
    """Returns the list of (start, end) ranges of the string literals."""
    spans = []
    i, n = 0, len(src)
    while i < n:
        c = src[i]
        if c == '/' and i + 1 < n and src[i+1] == '/':
            j = src.find('\n', i)
            i = n if j == -1 else j + 1
            continue
        if c == '/' and i + 1 < n and src[i+1] == '*':
            depth, i = 1, i + 2
            while i < n and depth:
                if src.startswith('/*', i): depth += 1; i += 2
                elif src.startswith('*/', i): depth -= 1; i += 2
                else: i += 1
            continue
        if c == 'r' and i + 1 < n and src[i+1] in '#"':
            j = i + 1
            hashes = 0
            while j < n and src[j] == '#': hashes += 1; j += 1
            if j < n and src[j] == '"':
                close = '"' + '#' * hashes
                k = src.find(close, j + 1)
                if k == -1: k = n
                spans.append((j + 1, k))
                i = k + len(close)
                continue
        if c == "'":
            # character literal or lifetime label
            j = i + 1
            if j < n and src[j] == '\\':
                j += 2
                while j < n and src[j] != "'": j += 1
                i = j + 1
                continue
            if j + 1 < n and src[j+1] == "'":
                i = j + 2
                continue
            i += 1
            continue
        if c == '"':
            j = i + 1
            while j < n:
                if src[j] == '\\': j += 2; continue
                if src[j] == '"': break
                j += 1
            spans.append((i + 1, j))
            i = j + 1
            continue
        i += 1
    return spans

REPLACEMENTS = {
    '—': '-',   # em dash
    '–': '-',   # en dash
    '’': "'",   # typographic apostrophe
    '«': '"',
    '»': '"',
    '“': '"',
    '”': '"',
    '…': '...',
}

dry = '--apply' not in sys.argv
found = collections.Counter()
touched = 0

for root, dirs, names in os.walk('src'):
    for name in sorted(names):
        if not name.endswith('.rs'): continue
        path = os.path.join(root, name)
        src = open(path, encoding='utf-8').read()
        spans = scan(src)
        out, last, changed = [], 0, 0
        leftovers = set()
        for a, b in spans:
            out.append(src[last:a])
            lit = src[a:b]
            for ch, rep in REPLACEMENTS.items():
                if ch in lit:
                    changed += lit.count(ch)
                    found[ch] += lit.count(ch)
                    lit = lit.replace(ch, rep)
            for ch in lit:
                if (ord(ch) < 0x20 or ord(ch) > 0x7e) and ch != '\n':
                    leftovers.add(ch)
            out.append(lit)
            last = b
        out.append(src[last:])
        if leftovers:
            print('%-40s reste : %s' % (path, ' '.join('U+%04X %s' % (ord(c), c) for c in sorted(leftovers))))
        if changed:
            touched += 1
            if not dry:
                open(path, 'w', encoding='utf-8').write(''.join(out))

print('---')
for ch, count in found.most_common():
    print('U+%04X %r  %d' % (ord(ch), ch, count))
print('%d fichier(s) %s' % (touched, 'a corriger' if dry else 'corrige(s)'))
if dry and touched:
    print('lancer : python3 tools/ascii-check.py --apply')
sys.exit(1 if (dry and touched) else 0)
