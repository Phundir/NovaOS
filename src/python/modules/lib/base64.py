"""base64 for Nova's Python: base16, base32, base64 and their variants,
base85 and ascii85, as RFC 4648 and CPython define them."""

import binascii

__all__ = ['encode', 'decode', 'encodebytes', 'decodebytes',
           'b64encode', 'b64decode', 'b32encode', 'b32decode',
           'b32hexencode', 'b32hexdecode', 'b16encode', 'b16decode',
           'b85encode', 'b85decode', 'a85encode', 'a85decode',
           'standard_b64encode', 'standard_b64decode',
           'urlsafe_b64encode', 'urlsafe_b64decode']

bytes_types = (bytes, bytearray)


def _bytes_from_decode_data(s):
    if isinstance(s, str):
        try:
            return s.encode('ascii')
        except UnicodeEncodeError:
            raise ValueError('string argument should contain only ASCII characters')
    if isinstance(s, bytes_types):
        return bytes(s)
    raise TypeError("argument should be a bytes-like object or ASCII "
                    "string, not %r" % s.__class__.__name__)


def b64encode(s, altchars=None):
    encoded = binascii.b2a_base64(s, newline=False)
    if altchars is not None:
        assert len(altchars) == 2, repr(altchars)
        return encoded.translate(bytes.maketrans(b'+/', altchars))
    return encoded


def b64decode(s, altchars=None, validate=False):
    s = _bytes_from_decode_data(s)
    if altchars is not None:
        altchars = _bytes_from_decode_data(altchars)
        assert len(altchars) == 2, repr(altchars)
        s = s.translate(bytes.maketrans(altchars, b'+/'))
    return binascii.a2b_base64(s, strict_mode=validate)


def standard_b64encode(s):
    return b64encode(s)


def standard_b64decode(s):
    return b64decode(s)


def urlsafe_b64encode(s):
    return b64encode(s).translate(bytes.maketrans(b'+/', b'-_'))


def urlsafe_b64decode(s):
    s = _bytes_from_decode_data(s)
    s = s.translate(bytes.maketrans(b'-_', b'+/'))
    return b64decode(s)


_B32 = b'ABCDEFGHIJKLMNOPQRSTUVWXYZ234567'
_B32HEX = b'0123456789ABCDEFGHIJKLMNOPQRSTUV'


def _b32encode(alphabet, s):
    s = bytes(s)
    leftover = len(s) % 5
    if leftover:
        s = s + b'\0' * (5 - leftover)
    encoded = bytearray()
    for i in range(0, len(s), 5):
        c = int.from_bytes(s[i: i + 5], 'big')
        for shift in range(35, -1, -5):
            encoded.append(alphabet[(c >> shift) & 31])
    if leftover == 1:
        encoded[-6:] = b'======'
    elif leftover == 2:
        encoded[-4:] = b'===='
    elif leftover == 3:
        encoded[-3:] = b'==='
    elif leftover == 4:
        encoded[-1:] = b'='
    return bytes(encoded)


def _b32decode(alphabet, s, casefold=False, map01=None):
    s = _bytes_from_decode_data(s)
    if len(s) % 8:
        raise binascii.Error('Incorrect padding')
    if map01 is not None:
        map01 = _bytes_from_decode_data(map01)
        assert len(map01) == 1, repr(map01)
        s = s.translate(bytes.maketrans(b'01', b'O' + map01))
    if casefold:
        s = s.upper()
    l = len(s)
    s = s.rstrip(b'=')
    padchars = l - len(s)
    rev = {c: i for i, c in enumerate(alphabet)}
    decoded = bytearray()
    for i in range(0, len(s), 8):
        quanta = s[i: i + 8]
        acc = 0
        try:
            for c in quanta:
                acc = (acc << 5) + rev[c]
        except KeyError:
            raise binascii.Error('Non-base32 digit found') from None
        acc <<= 5 * (8 - len(quanta))
        decoded += acc.to_bytes(5, 'big')
    if l % 8 or padchars not in {0, 1, 3, 4, 6}:
        raise binascii.Error('Incorrect padding')
    if padchars and decoded:
        leftover = (43 - 5 * padchars) // 8
        decoded[leftover - 5:] = b''
    return bytes(decoded)


def b32encode(s):
    return _b32encode(_B32, s)


def b32decode(s, casefold=False, map01=None):
    return _b32decode(_B32, s, casefold, map01)


def b32hexencode(s):
    return _b32encode(_B32HEX, s)


def b32hexdecode(s, casefold=False):
    return _b32decode(_B32HEX, s, casefold)


def b16encode(s):
    return binascii.hexlify(s).upper()


def b16decode(s, casefold=False):
    s = _bytes_from_decode_data(s)
    if casefold:
        s = s.upper()
    for c in s:
        if c not in b'0123456789ABCDEF':
            raise binascii.Error('Non-base16 digit found')
    return binascii.unhexlify(s)


_A85START = b"<~"
_A85END = b"~>"


def _85encode(b, chars, chars2, pad=False, foldnuls=False, foldspaces=False):
    b = bytes(b)
    padding = (-len(b)) % 4
    if padding:
        b = b + b'\0' * padding
    words = [int.from_bytes(b[i:i + 4], 'big') for i in range(0, len(b), 4)]
    chunks = []
    for word in words:
        if foldnuls and not word:
            chunks.append(b'z')
        elif foldspaces and word == 0x20202020:
            chunks.append(b'y')
        else:
            chunks.append(bytes((chars[word // 52200625],
                                 chars[word // 614125 % 85],
                                 chars[word // 7225 % 85],
                                 chars[word // 85 % 85],
                                 chars[word % 85])))
    if padding and not pad:
        if chunks[-1] == b'z':
            chunks[-1] = chars[0:1] * 5
        chunks[-1] = chunks[-1][:-padding]
    return b''.join(chunks)


_A85CHARS = bytes(range(33, 118))
_B85ALPHABET = (b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ"
                b"abcdefghijklmnopqrstuvwxyz!#$%&()*+-;<=>?@^_`{|}~")


def a85encode(b, *, foldspaces=False, wrapcol=0, pad=False, adobe=False):
    result = _85encode(b, _A85CHARS, None, pad, True, foldspaces)
    if adobe:
        result = _A85START + result
    if wrapcol:
        wrapcol = max(2 if adobe else 1, wrapcol)
        chunks = [result[i: i + wrapcol] for i in range(0, len(result), wrapcol)]
        if adobe:
            if len(chunks[-1]) + 2 > wrapcol:
                chunks.append(b'')
        result = b'\n'.join(chunks)
    if adobe:
        result += _A85END
    return result


def a85decode(b, *, foldspaces=False, adobe=False, ignorechars=b' \t\n\r\v'):
    b = _bytes_from_decode_data(b)
    if adobe:
        if not b.endswith(_A85END):
            raise ValueError("Ascii85 encoded byte sequences must end with {!r}".format(_A85END))
        if b.startswith(_A85START):
            b = b[2:-2]
        else:
            b = b[:-2]
    decoded = []
    curr = []
    for x in b + b'u' * 4:
        if 33 <= x <= 117:
            curr.append(x)
            if len(curr) == 5:
                acc = 0
                for x in curr:
                    acc = 85 * acc + (x - 33)
                try:
                    decoded.append(acc.to_bytes(4, 'big'))
                except OverflowError:
                    raise ValueError('Ascii85 overflow') from None
                curr = []
        elif x == 122:
            if curr:
                raise ValueError('z inside Ascii85 5-tuple')
            decoded.append(b'\0\0\0\0')
        elif foldspaces and x == 121:
            if curr:
                raise ValueError('y inside Ascii85 5-tuple')
            decoded.append(b'\x20\x20\x20\x20')
        elif x in ignorechars:
            continue
        else:
            raise ValueError('Non-Ascii85 digit found: %c' % x)
    result = b''.join(decoded)
    padding = 4 - len(curr)
    if padding:
        result = result[:-padding]
    return result


def b85encode(b, pad=False):
    return _85encode(b, _B85ALPHABET, None, pad)


def b85decode(b):
    b = _bytes_from_decode_data(b)
    rev = {c: i for i, c in enumerate(_B85ALPHABET)}
    padding = (-len(b)) % 5
    b = b + b'~' * padding
    out = bytearray()
    for i in range(0, len(b), 5):
        chunk = b[i:i + 5]
        acc = 0
        try:
            for c in chunk:
                acc = acc * 85 + rev[c]
        except KeyError:
            for j, c in enumerate(chunk):
                if c not in rev:
                    raise ValueError('bad base85 character at position %d' % (i + j)) from None
            raise
        try:
            out += acc.to_bytes(4, 'big')
        except OverflowError:
            raise ValueError('base85 overflow in hunk starting at byte %d' % i) from None
    if padding:
        out = out[:-padding]
    return bytes(out)


MAXLINESIZE = 76
MAXBINSIZE = (MAXLINESIZE // 4) * 3


def encodebytes(s):
    pieces = []
    for i in range(0, len(s), MAXBINSIZE):
        chunk = s[i: i + MAXBINSIZE]
        pieces.append(binascii.b2a_base64(chunk))
    return b"".join(pieces)


def decodebytes(s):
    return binascii.a2b_base64(s)


def encode(input, output):
    while s := input.read(MAXBINSIZE):
        output.write(binascii.b2a_base64(s))


def decode(input, output):
    output.write(binascii.a2b_base64(input.read()))
