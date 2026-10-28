#!/usr/bin/env python3
"""Writes the two halves of the Venus encoder check.

    tools/venus-check/gen-check.py --xml build/venus/xml --out build/venus/check

For every command of tools/gen-venus.py, the same arguments are built
twice -- in C for the reference encoder of venus-protocol, in Rust for the
encoder Nova generated -- with every scalar taking the next value of one
counter, in the same order on both sides. Each side prints one line per
command: its name and the bytes of the stream in hexadecimal. The two
outputs must be identical (tools/venus-check/check.sh diffs them).

What the values exercise: every array holds two elements, every string and
blob an odd length (padding), every pNext chain every structure the kernel
may chain on that base, every conditional array its sending condition.
"""

import argparse
import importlib.util
import re
from pathlib import Path

HERE = Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location('gen_venus', HERE.parent / 'gen-venus.py')
gv = importlib.util.module_from_spec(spec)
spec.loader.exec_module(gv)

# Scalars whose value other fields depend on.
SPECIAL = {
    'codeSize': 8,
    'rasterizationSamples': 1,
    'sharingMode': 1,
    'commandBufferCount': 2,
    'dataSize': 5,
}
ARRAY = 2


class Check:
    def __init__(self, reg):
        self.reg = reg
        self.gen = gv.Gen(reg)
        self.gen.collect()
        self.c = []
        self.rs = []
        self.tmp = 0

    def name(self, stem):
        self.tmp += 1
        return '%s%d' % (stem, self.tmp)

    # ---- values ----------------------------------------------------------

    def scalar_value(self, var):
        if var.name in SPECIAL:
            return str(SPECIAL[var.name]), False
        return 'next_value()', True

    # ---- C side ------------------------------------------------------------

    def c_scalar(self, lvalue, var, ctype):
        value, _ = self.scalar_value(var)
        t = self.reg.resolve(ctype)
        if t == 'float':
            return ['{ uint32_t t_ = (uint32_t)(%s); memcpy(&%s, &t_, 4); }' % (value, lvalue)]
        if t == 'double':
            return ['{ uint64_t t_ = (uint64_t)(%s); memcpy(&%s, &t_, 8); }' % (value, lvalue)]
        cat = self.reg.category(t)
        if cat == 'handle':
            return ['%s = (%s)(uintptr_t)(%s);' % (lvalue, t, value)]
        return ['%s = (%s)(%s);' % (lvalue, t, value)]

    def c_struct(self, lvalue, struct, chain=True):
        """Statements filling the C struct at lvalue (an lvalue expression)."""
        struct = self.reg.resolve(struct)
        out = []
        members = self.reg.members(struct)
        derived = self.gen.derived_counts(members)
        counts = set()
        for m in members:
            if m.len and self.gen.kind(m) in ('array', 'blob', 'strings'):
                counts.add(m.len_parts()[0])
        for m in members:
            k = self.gen.kind(m)
            lv = '%s.%s' % (lvalue, m.name)
            if k == 'stype':
                out.append('%s = (VkStructureType)%d;' % (lv, self.reg.stype(struct)))
            elif k == 'next':
                if chain and struct not in self.gen.chain:
                    out += self.c_chain(lv, struct)
                else:
                    out.append('%s = NULL;' % lv)
            elif m.name in counts and m.name not in SPECIAL:
                out.append('%s = %d;' % (lv, ARRAY if m.type != 'size_t' else 5))
            elif k in ('scalar', 'enum', 'handle'):
                out += self.c_scalar(lv, m, m.type)
            elif k == 'union':
                out.append('for (int u_ = 0; u_ < 4; u_++) %s.uint32[u_] = next_value();' % (lv + ('.color' if self.reg.resolve(m.type) == 'VkClearValue' else '')))
            elif k == 'struct':
                out += self.c_struct(lv, m.type)
            elif k == 'fixed':
                n = self.reg.constant(m.fixed)
                if self.reg.category(m.type) == 'struct':
                    for i in range(n):
                        out += self.c_struct('%s[%d]' % (lv, i), m.type)
                else:
                    for i in range(n):
                        out += self.c_scalar('%s[%d]' % (lv, i), m, m.type)
            elif k == 'string':
                out.append('%s = next_string();' % lv)
            elif k == 'strings':
                arr = self.name('strs')
                out.append('static const char *%s[%d]; for (int s_ = 0; s_ < %d; s_++) %s[s_] = next_string();' % (arr, ARRAY, ARRAY, arr))
                out.append('%s = %s;' % (lv, arr))
            elif k == 'blob':
                arr = self.name('blob')
                out.append('static uint8_t %s[5]; for (int b_ = 0; b_ < 5; b_++) %s[b_] = (uint8_t)next_value();' % (arr, arr))
                out.append('%s = %s;' % (lv, arr))
            elif k == 'array':
                t = self.reg.resolve(m.type)
                n = self.array_len(m)
                arr = self.name('arr')
                ctype = t
                out.append('static %s %s[%d];' % (ctype, arr, n))
                for i in range(n):
                    if self.reg.category(t) == 'struct':
                        out += self.c_struct('%s[%d]' % (arr, i), t)
                    else:
                        out += self.c_scalar('%s[%d]' % (arr, i), m, t)
                out.append('%s = %s;' % (lv, arr))
            elif k == 'pointer':
                t = self.reg.resolve(m.type)
                arr = self.name('one')
                out.append('static %s %s;' % (t, arr))
                out += self.c_struct(arr, t)
                out.append('%s = &%s;' % (lv, arr))
            elif k == 'chars':
                n = self.reg.constant(m.fixed)
                out.append('for (int c_ = 0; c_ < %d - 1; c_++) %s[c_] = (char)(1 + next_value() %% 100); %s[%d - 1] = 0;' % (n, lv, lv, n))
            else:
                gv.fail('C : %s.%s' % (struct, m.name))
        return out

    def array_len(self, m):
        if m.altlen and 'codeSize' in m.altlen:
            return SPECIAL['codeSize'] // 4
        if m.altlen and 'rasterizationSamples' in m.altlen:
            return 1
        return ARRAY

    def allowed_chain(self, base):
        return [c for c in self.gen.chain if base in self.reg.extends(c)]

    def c_chain(self, lvalue, base):
        out = []
        items = self.allowed_chain(base)
        names = []
        for c in items:
            n = self.name('chain')
            out.append('static %s %s;' % (c, n))
            out += self.c_struct(n, c, chain=False)
            names.append(n)
        for i, n in enumerate(names):
            out.append('%s.pNext = %s;' % (n, '&' + names[i + 1] if i + 1 < len(names) else 'NULL'))
        out.append('%s = %s;' % (lvalue, '&' + names[0] if names else 'NULL'))
        return out

    # ---- Rust side -----------------------------------------------------------

    def rs_scalar(self, var, ctype):
        value, _ = self.scalar_value(var)
        rust = self.gen.scalar(ctype)[0] if self.reg.resolve(ctype) not in gv.UNIONS else 'u32'
        return '(%s) as %s' % (value, rust)

    def rs_struct(self, struct, chain=True):
        """A Rust expression building the struct."""
        struct = self.reg.resolve(struct)
        members = self.reg.members(struct)
        derived = self.gen.derived_counts(members)
        counts = set()
        for m in members:
            if m.len and self.gen.kind(m) in ('array', 'blob', 'strings'):
                counts.add(m.len_parts()[0])
        fields = []
        lets = []
        for m in members:
            k = self.gen.kind(m)
            f = m.field
            if k == 'stype':
                continue
            if k == 'next':
                if struct in self.gen.chain:
                    continue
                if chain:
                    items = []
                    for c in self.allowed_chain(struct):
                        items.append('VkNext::%s(%s)' % (c[2:], self.rs_struct(c, chain=False)))
                    fields.append('next: leak(vec![%s])' % ', '.join(items))
                else:
                    fields.append('next: &[]')
                continue
            if m.name in derived:
                continue
            if m.name in counts and m.name not in SPECIAL:
                fields.append('%s: %d' % (f, ARRAY if m.type != 'size_t' else 5))
            elif k in ('scalar', 'enum', 'handle'):
                fields.append('%s: %s' % (f, self.rs_scalar(m, m.type)))
            elif k == 'union':
                fields.append('%s: [next_value() as u32, next_value() as u32, next_value() as u32, next_value() as u32]' % f)
            elif k == 'struct':
                fields.append('%s: %s' % (f, self.rs_struct(m.type)))
            elif k == 'fixed':
                n = self.reg.constant(m.fixed)
                if self.reg.category(m.type) == 'struct':
                    fields.append('%s: [%s]' % (f, ', '.join(self.rs_struct(m.type) for _ in range(n))))
                else:
                    fields.append('%s: [%s]' % (f, ', '.join(self.rs_scalar(m, m.type) for _ in range(n))))
            elif k == 'string':
                fields.append('%s: Some(next_string())' % f)
            elif k == 'strings':
                fields.append('%s: leak(vec![next_string(), next_string()])' % f)
            elif k == 'blob':
                opt = m.optional.startswith('true') or m.noauto
                expr = 'leak((0..5).map(|_| next_value() as u8).collect::<Vec<u8>>())'
                fields.append('%s: %s' % (f, 'Some(%s)' % expr if opt else expr))
            elif k == 'array':
                t = self.reg.resolve(m.type)
                n = self.array_len(m)
                if self.reg.category(t) == 'struct':
                    items = ', '.join(self.rs_struct(t) for _ in range(n))
                else:
                    items = ', '.join(self.rs_scalar(m, t) for _ in range(n))
                expr = 'leak(vec![%s])' % items
                plain = m.len_parts()[0] in derived
                fields.append('%s: %s' % (f, expr if plain else 'Some(%s)' % expr))
            elif k == 'pointer':
                fields.append('%s: Some(leak_one(%s))' % (f, self.rs_struct(m.type)))
            elif k == 'chars':
                n = self.reg.constant(m.fixed)
                fields.append('%s: { let mut c = [0u8; %d]; for x in c.iter_mut().take(%d - 1) { *x = 1 + (next_value() %% 100) as u8; } c }' % (f, n, n))
            else:
                gv.fail('Rust : %s.%s' % (struct, m.name))
        # Rust evaluates struct fields in the order written, which is the
        # member order -- the order the C side consumed the counter in.
        return '%s { %s }' % (struct, ', '.join(fields)) if fields else '%s::default()' % struct

    # ---- commands --------------------------------------------------------------

    def command(self, command):
        params = self.gen.params(command)
        derived = self.gen.derived_counts(params)
        names = {p.name: p for p in params}
        inout = set()
        for p in params:
            if p.len and p.pointers == 1 and not p.const:
                c = p.len_parts()[0]
                if c in names and names[c].pointers == 1:
                    inout.add(c)
        counts = set()
        for p in params:
            if p.len and self.gen.kind(p) in ('array', 'blob'):
                counts.add(p.len_parts()[0])
        c_args, c_pre = [], []
        rs_args, rs_pre = [], []
        for p in params:
            k = self.gen.kind(p)
            t = self.reg.resolve(p.type)
            cat = self.reg.category(t)
            if p.name == 'pAllocator':
                c_args.append('NULL')
                continue
            is_out = p.pointers >= 1 and not p.const
            if p.name in inout:
                v = self.name('count')
                c_pre.append('uint32_t %s = %d;' % (v, ARRAY))
                c_args.append('&' + v)
                rs_args.append('Some(%d)' % ARRAY)
                continue
            if is_out and k == 'pointer':
                v = self.name('out')
                if cat == 'handle':
                    c_pre.append('%s %s = (%s)(uintptr_t)next_value();' % (t, v, t))
                    rs_pre.append('let %s = next_value();' % v)
                    rs_args.append(v)
                elif cat == 'struct':
                    c_pre.append('%s %s; memset(&%s, 0, sizeof(%s));' % (t, v, v, v))
                else:
                    c_pre.append('%s %s = 0;' % (t, v))
                c_args.append('&' + v)
                continue
            if is_out and k == 'array':
                v = self.name('outs')
                if cat == 'handle':
                    c_pre.append('%s %s[%d]; for (int h_ = 0; h_ < %d; h_++) %s[h_] = (%s)(uintptr_t)next_value();' % (t, v, ARRAY, ARRAY, v, t))
                    rs_pre.append('let %s: Vec<u64> = (0..%d).map(|_| next_value()).collect();' % (v, ARRAY))
                    rs_args.append('Some(&%s)' % v)
                else:
                    c_pre.append('%s %s[%d]; memset(%s, 0, sizeof(%s));' % (t, v, ARRAY, v, v))
                    rs_args.append('true')
                c_args.append(v)
                continue
            if p.name in derived:
                c_args.append('5' if self.gen.kind(derived[p.name]) == 'blob' else str(ARRAY))
                continue
            if p.name in counts:
                c_args.append(str(ARRAY))
                rs_args.append(str(ARRAY))
                continue
            if k in ('scalar', 'enum', 'handle'):
                v = self.name('s')
                if self.reg.resolve(p.type) == 'float':
                    c_pre.append('float %s; { uint32_t t_ = (uint32_t)next_value(); memcpy(&%s, &t_, 4); }' % (v, v))
                else:
                    c_pre.append('%s %s = (%s)(uintptr_t)next_value();' % (t, v, t))
                rs_pre.append('let %s = next_value() as %s;' % (v, self.gen.scalar(p.type)[0]))
                c_args.append(v)
                rs_args.append(v)
            elif k == 'pointer' and t in gv.UNIONS:
                v = self.name('u')
                c_pre.append('%s %s; for (int u_ = 0; u_ < 4; u_++) %s.uint32[u_] = next_value();' % (t, v, v))
                rs_pre.append('let %s = [next_value() as u32, next_value() as u32, next_value() as u32, next_value() as u32];' % v)
                c_args.append('&' + v)
                rs_args.append('&' + v)
            elif k == 'pointer':
                v = self.name('p')
                c_pre.append('static %s %s;' % (t, v))
                c_pre += self.c_struct(v, t)
                rs_pre.append('let %s = %s;' % (v, self.rs_struct(t)))
                c_args.append('&' + v)
                rs_args.append('Some(&%s)' % v if p.optional.startswith('true') else '&' + v)
            elif k == 'array':
                v = self.name('a')
                n = ARRAY
                c_pre.append('static %s %s[%d];' % (t, v, n))
                items = []
                for i in range(n):
                    if cat == 'struct':
                        c_pre += self.c_struct('%s[%d]' % (v, i), t)
                        items.append(self.rs_struct(t))
                    else:
                        c_pre += self.c_scalar('%s[%d]' % (v, i), p, t)
                        items.append(self.rs_scalar(p, t))
                rs_pre.append('let %s = vec![%s];' % (v, ', '.join(items)))
                c_args.append(v)
                rs_args.append('&%s' % v if p.len_parts()[0] in derived else 'Some(&%s)' % v)
            elif k == 'blob':
                v = self.name('b')
                c_pre.append('static uint8_t %s[5]; for (int b_ = 0; b_ < 5; b_++) %s[b_] = (uint8_t)next_value();' % (v, v))
                rs_pre.append('let %s: Vec<u8> = (0..5).map(|_| next_value() as u8).collect();' % v)
                c_args.append(v)
                rs_args.append('&%s' % v)
            elif k == 'string':
                v = self.name('str')
                c_pre.append('const char *%s = next_string();' % v)
                rs_pre.append('let %s = next_string();' % v)
                c_args.append(v)
                rs_args.append('Some(%s)' % v)
            else:
                gv.fail('verification : %s.%s' % (command, p.name))
        fname = gv.snake(command[2:])
        self.c.append('static void check_%s(void) {' % command)
        self.c.append('    struct vn_cs_encoder enc = {0};')
        for line in c_pre:
            self.c.append('    ' + line)
        self.c.append('    vn_encode_%s(&enc, 1%s);' % (command, ''.join(', ' + a for a in c_args)))
        self.c.append('    dump("%s", &enc);' % command)
        self.c.append('}')
        self.c.append('')
        self.rs.append('fn check_%s() {' % command)
        for line in rs_pre:
            self.rs.append('    ' + line)
        self.rs.append('    let mut e = Enc::new();')
        self.rs.append('    %s(&mut e, 1%s);' % (fname, ''.join(', ' + a for a in rs_args)))
        self.rs.append('    dump("%s", &e);' % command)
        self.rs.append('}')
        self.rs.append('')

    def run(self, out):
        for command in gv.COMMANDS:
            self.command(command)
        c = ['#include "check_prelude.h"', ''] + self.c
        c.append('int main(void) {')
        for command in gv.COMMANDS:
            c.append('    check_%s();' % command)
        c.append('    return 0;')
        c.append('}')
        rs = ['#![allow(unused, clippy::all)]', 'include!("check_prelude.rs");', ''] + self.rs
        rs.append('fn main() {')
        for command in gv.COMMANDS:
            rs.append('    check_%s();' % command)
        rs.append('}')
        out.mkdir(parents=True, exist_ok=True)
        (out / 'check.c').write_text('\n'.join(c) + '\n')
        (out / 'check.rs').write_text('\n'.join(rs) + '\n')


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--xml', type=Path, default=Path('build/venus/xml'))
    parser.add_argument('--out', type=Path, default=Path('build/venus/check'))
    args = parser.parse_args()
    reg = gv.Registry(args.xml)
    Check(reg).run(args.out)


if __name__ == '__main__':
    main()
