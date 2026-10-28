"""string for Nova's Python: the constants, capwords, Template."""

whitespace = ' \t\n\r\v\f'
ascii_lowercase = 'abcdefghijklmnopqrstuvwxyz'
ascii_uppercase = 'ABCDEFGHIJKLMNOPQRSTUVWXYZ'
ascii_letters = ascii_lowercase + ascii_uppercase
digits = '0123456789'
hexdigits = digits + 'abcdef' + 'ABCDEF'
octdigits = '01234567'
punctuation = r"""!"#$%&'()*+,-./:;<=>?@[\]^_`{|}~"""
printable = digits + ascii_letters + punctuation + whitespace


def capwords(s, sep=None):
    return (sep or ' ').join(map(str.capitalize, s.split(sep)))


class Template:
    delimiter = '$'

    def __init__(self, template):
        self.template = template

    def _substitute(self, mapping, safe):
        out = []
        t = self.template
        i = 0
        n = len(t)
        while i < n:
            c = t[i]
            if c != self.delimiter:
                out.append(c)
                i += 1
                continue
            if i + 1 < n and t[i + 1] == self.delimiter:
                out.append(self.delimiter)
                i += 2
                continue
            j = i + 1
            braced = j < n and t[j] == '{'
            if braced:
                j += 1
            start = j
            while j < n and (t[j].isalnum() or t[j] == '_') and (j > start or not t[j].isdigit()):
                j += 1
            name = t[start:j]
            if braced:
                if j >= n or t[j] != '}' or not name:
                    if safe:
                        out.append(t[i])
                        i += 1
                        continue
                    raise ValueError(f'Invalid placeholder in string: line 1, col {i + 1}')
                j += 1
            if not name:
                if safe:
                    out.append(t[i])
                    i += 1
                    continue
                raise ValueError(f'Invalid placeholder in string: line 1, col {i + 1}')
            if name in mapping:
                out.append(str(mapping[name]))
            elif safe:
                out.append(t[i:j])
            else:
                raise KeyError(name)
            i = j
        return ''.join(out)

    def substitute(self, mapping=None, /, **kws):
        if mapping is None:
            mapping = kws
        elif kws:
            mapping = {**mapping, **kws}
        return self._substitute(mapping, False)

    def safe_substitute(self, mapping=None, /, **kws):
        if mapping is None:
            mapping = kws
        elif kws:
            mapping = {**mapping, **kws}
        return self._substitute(mapping, True)
