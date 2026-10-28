"""fnmatch for Nova's Python: shell patterns translated to regular
expressions, as CPython's translate() writes them."""

import os
import re
import functools

__all__ = ["filter", "fnmatch", "fnmatchcase", "translate"]


def fnmatch(name, pat):
    name = os.path.normcase(name) if hasattr(os.path, 'normcase') else name
    return fnmatchcase(name, pat)


@functools.lru_cache(maxsize=32768)
def _compile_pattern(pat):
    return re.compile(translate(pat)).match


def filter(names, pat):
    match = _compile_pattern(pat)
    return [name for name in names if match(name)]


def fnmatchcase(name, pat):
    match = _compile_pattern(pat)
    return match(name) is not None


def translate(pat):
    STAR = object()
    parts = _translate(pat, STAR, '.')
    return _join_translated_parts(parts, STAR)


def _translate(pat, STAR, QUESTION_MARK):
    res = []
    add = res.append
    i, n = 0, len(pat)
    while i < n:
        c = pat[i]
        i = i + 1
        if c == '*':
            if (not res) or res[-1] is not STAR:
                add(STAR)
        elif c == '?':
            add(QUESTION_MARK)
        elif c == '[':
            j = i
            if j < n and pat[j] == '!':
                j = j + 1
            if j < n and pat[j] == ']':
                j = j + 1
            while j < n and pat[j] != ']':
                j = j + 1
            if j >= n:
                add('\\[')
            else:
                stuff = pat[i:j]
                if '-' not in stuff:
                    stuff = stuff.replace('\\', r'\\')
                else:
                    chunks = []
                    k = i + 2 if pat[i] == '!' else i + 1
                    while True:
                        k = pat.find('-', k, j)
                        if k < 0:
                            break
                        chunks.append(pat[i:k])
                        i = k + 1
                        k = k + 3
                    chunk = pat[i:j]
                    if chunk:
                        chunks.append(chunk)
                    else:
                        chunks[-1] += '-'
                    for k in range(len(chunks) - 1, 0, -1):
                        if chunks[k - 1][-1] > chunks[k][0]:
                            chunks[k - 1] = chunks[k - 1][:-1] + chunks[k][1:]
                            del chunks[k]
                    stuff = '-'.join(s.replace('\\', r'\\').replace('-', r'\-')
                                     for s in chunks)
                stuff = re.sub(r'([&~|])', r'\\\1', stuff)
                i = j + 1
                if not stuff:
                    add('(?!)')
                elif stuff == '!':
                    add('.')
                else:
                    if stuff[0] == '!':
                        stuff = '^' + stuff[1:]
                    elif stuff[0] in ('^', '['):
                        stuff = '\\' + stuff
                    add(f'[{stuff}]')
        else:
            add(re.escape(c))
    assert i == n
    return res


def _join_translated_parts(inp, STAR):
    res = []
    add = res.append
    i, n = 0, len(inp)
    while i < n and inp[i] is not STAR:
        add(inp[i])
        i += 1
    while i < n:
        assert inp[i] is STAR
        i += 1
        if i == n:
            add(".*")
            break
        assert inp[i] is not STAR
        fixed = []
        while i < n and inp[i] is not STAR:
            fixed.append(inp[i])
            i += 1
        fixed = "".join(fixed)
        if i == n:
            add(".*")
            add(fixed)
        else:
            add(f"(?:.*?{fixed})")
    res = "".join(res)
    return fr'(?s:{res})\Z'
