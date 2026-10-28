"""binascii for Nova's Python: hex, base64, uuencode-free subset, crc."""

import _nova


class Error(ValueError):
    pass


class Incomplete(Exception):
    pass


def _bytes(data):
    if isinstance(data, str):
        try:
            return data.encode('ascii')
        except UnicodeEncodeError:
            raise ValueError('string argument should contain only ASCII characters')
    return bytes(data)


def hexlify(data, sep=None, bytes_per_sep=1):
    if sep is None:
        return bytes(data).hex().encode('ascii')
    return bytes(data).hex(sep, bytes_per_sep).encode('ascii')


b2a_hex = hexlify


def unhexlify(hexstr):
    s = _bytes(hexstr)
    if len(s) % 2:
        raise Error('Odd-length string')
    try:
        return bytes.fromhex(s.decode('ascii'))
    except ValueError:
        raise Error('Non-hexadecimal digit found')


a2b_hex = unhexlify

_B64 = b'ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/'
_B64_INDEX = {c: i for i, c in enumerate(_B64)}


def b2a_base64(data, *, newline=True):
    data = bytes(data)
    out = bytearray()
    for i in range(0, len(data) - 2, 3):
        n = (data[i] << 16) | (data[i + 1] << 8) | data[i + 2]
        out += bytes((_B64[n >> 18], _B64[(n >> 12) & 63], _B64[(n >> 6) & 63], _B64[n & 63]))
    rest = len(data) % 3
    if rest == 1:
        n = data[-1] << 16
        out += bytes((_B64[n >> 18], _B64[(n >> 12) & 63])) + b'=='
    elif rest == 2:
        n = (data[-2] << 16) | (data[-1] << 8)
        out += bytes((_B64[n >> 18], _B64[(n >> 12) & 63], _B64[(n >> 6) & 63])) + b'='
    if newline:
        out += b'\n'
    return bytes(out)


def a2b_base64(data, /, *, strict_mode=False):
    s = _bytes(data)
    out = bytearray()
    acc = 0
    bits = 0
    pads = 0
    count = 0
    for c in s:
        if c == 61:
            pads += 1
            if count % 4 >= 2 and count % 4 + pads >= 4:
                break
            continue
        if c not in _B64_INDEX:
            if strict_mode:
                raise Error('Only base64 data is allowed')
            continue
        if pads and strict_mode:
            raise Error('Discontinuous padding not allowed')
        pads = 0
        acc = (acc << 6) | _B64_INDEX[c]
        bits += 6
        count += 1
        if bits >= 8:
            bits -= 8
            out.append((acc >> bits) & 0xff)
    if count % 4 == 1:
        raise Error('Invalid base64-encoded string: number of data characters (%d) cannot be 1 more than a multiple of 4' % count)
    if count % 4 and count % 4 + pads < 4:
        raise Error('Incorrect padding')
    return bytes(out)


def crc32(data, crc=0, /):
    return _nova.crc32(data, crc)


def crc_hqx(data, crc, /):
    crc &= 0xffff
    for b in bytes(data):
        crc ^= b << 8
        for _ in range(8):
            crc = ((crc << 1) ^ 0x1021) & 0xffff if crc & 0x8000 else (crc << 1) & 0xffff
    return crc
