#!/usr/bin/env python3
"""Adds every src/python/modules/lib/*.py to the SOURCES table of
src/python/modules/mod.rs (sorted), so the interpreter ships it. A file
named a_b.py for a dotted module is listed in DOTTED below."""

import os
import re

ROOT = os.path.join(os.path.dirname(__file__), '..', '..', 'src', 'python', 'modules')
# Files loaded another way (os and sys extensions).
SKIP = {'_osextra', '_sysextra'}
# Flat file name -> dotted module name.
DOTTED = {
    'collections_abc': 'collections.abc',
    'html_entities': 'html.entities',
    'html_parser': 'html.parser',
    'unittest_mock': 'unittest.mock',
    'urllib_parse': 'urllib.parse',
}

names = sorted(f[:-3] for f in os.listdir(os.path.join(ROOT, 'lib')) if f.endswith('.py') and f[:-3] not in SKIP)
entries = []
for n in names:
    entries.append((DOTTED.get(n, n), n))
# Parents of dotted modules without a file of their own: empty packages.
for dotted, _ in list(entries):
    if '.' in dotted:
        parent = dotted.rsplit('.', 1)[0]
        if not any(d == parent for d, _ in entries):
            entries.append((parent, None))
entries.sort()
lines = []
for dotted, n in entries:
    if n is None:
        lines.append('    ("%s", "__path__ = []\\n"),' % dotted)
    else:
        lines.append('    ("%s", include_str!("lib/%s.py")),' % (dotted, n))
path = os.path.join(ROOT, 'mod.rs')
s = open(path).read()
s = re.sub(r'(const SOURCES: &\[\(&str, &str\)\] = &\[\n)(.*?)(\n\];)', lambda m: m.group(1) + '\n'.join(lines) + m.group(3), s, count=1, flags=re.S)
open(path, 'w').write(s)
print('%d modules' % len(lines))
