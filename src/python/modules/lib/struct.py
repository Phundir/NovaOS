"""struct for Nova's Python: packing values into bytes, C layouts.

The formats, sizes, alignment ('@', native x86_64) and error messages of
CPython; floats go through their IEEE 754 bits (_nova.pack_float).
"""

import _nova

__all__ = ['calcsize', 'pack', 'pack_into', 'unpack', 'unpack_from',
           'iter_unpack', 'Struct', 'error']


class error(Exception):
    pass


_NATIVE = {'x': 1, 'c': 1, 'b': 1, 'B': 1, '?': 1, 'h': 2, 'H': 2, 'i': 4,
           'I': 4, 'l': 8, 'L': 8, 'q': 8, 'Q': 8, 'n': 8, 'N': 8, 'e': 2,
           'f': 4, 'd': 8, 's': 1, 'p': 1, 'P': 8}
_STANDARD = {'x': 1, 'c': 1, 'b': 1, 'B': 1, '?': 1, 'h': 2, 'H': 2, 'i': 4,
             'I': 4, 'l': 4, 'L': 4, 'q': 8, 'Q': 8, 'e': 2, 'f': 4, 'd': 8,
             's': 1, 'p': 1}
_SIGNED = set('bhilqn')


def _parse(fmt):
    if isinstance(fmt, bytes):
        fmt = fmt.decode('ascii')
    if not isinstance(fmt, str):
        raise TypeError("Struct() argument 1 must be a str or bytes object, not %s" % type(fmt).__name__)
    order = '@'
    i = 0
    if fmt and fmt[0] in '@=<>!':
        order = fmt[0]
        i = 1
    native = order == '@'
    little = order in '@=<'
    sizes = _NATIVE if native else _STANDARD
    items = []
    offset = 0
    n = len(fmt)
    while i < n:
        ch = fmt[i]
        if ch.isspace():
            i += 1
            continue
        count = None
        while i < n and fmt[i].isdigit():
            count = (count or 0) * 10 + int(fmt[i])
            i += 1
        if i >= n:
            raise error('repeat count given without format specifier')
        ch = fmt[i]
        i += 1
        if ch not in sizes:
            raise error('bad char in struct format')
        size = sizes[ch]
        if count is None:
            count = 1
        if native and ch not in 'xcbB?sp' and offset % size:
            offset += size - offset % size
        if ch in 'sp':
            items.append((ch, offset, count))
            offset += count
        elif ch == 'x':
            offset += count
        else:
            for _ in range(count):
                items.append((ch, offset, size))
                offset += size
    return items, offset, little


def calcsize(fmt):
    return _parse(fmt)[1]


def _int_value(v, code):
    if not isinstance(v, int):
        if hasattr(v, '__index__'):
            return v.__index__()
        raise error('required argument is not an integer')
    return v


def _pack(fmt, args):
    items, size, little = _parse(fmt)
    if len(items) != len(args):
        raise error('pack expected %d items for packing (got %d)' % (len(items), len(args)))
    out = bytearray(size)
    order = 'little' if little else 'big'
    for (code, offset, width), v in zip(items, args):
        if code in 'sp':
            if not isinstance(v, (bytes, bytearray)):
                raise error("argument for '%s' must be a bytes object" % code)
            if code == 'p':
                data = bytes(v[:max(width - 1, 0)][:255])
                if width:
                    out[offset] = len(data)
                    out[offset + 1:offset + 1 + len(data)] = data
            else:
                data = bytes(v[:width])
                out[offset:offset + len(data)] = data
        elif code == 'c':
            if not isinstance(v, (bytes, bytearray)) or len(v) != 1:
                raise error("char format requires a bytes object of length 1")
            out[offset] = v[0]
        elif code == '?':
            out[offset] = 1 if v else 0
        elif code in 'efd':
            if not isinstance(v, (int, float)) and not hasattr(v, '__float__'):
                raise error('required argument is not a float')
            v = float(v)
            if code == 'd':
                bits = _nova.pack_float(v, 8)
            else:
                try:
                    bits = _nova.pack_float(v, width)
                except OverflowError:
                    raise OverflowError('float too large to pack with %s format' % code)
            out[offset:offset + width] = bits.to_bytes(width, order)
        else:
            v = _int_value(v, code)
            bits = width * 8
            if code in _SIGNED:
                lo, hi = -(1 << (bits - 1)), (1 << (bits - 1)) - 1
            else:
                lo, hi = 0, (1 << bits) - 1
            if not lo <= v <= hi:
                if code in _SIGNED:
                    raise error("'%s' format requires %d <= number <= %d" % (code, lo, hi))
                raise error("'%s' format requires 0 <= number <= %d" % (code, hi))
            out[offset:offset + width] = v.to_bytes(width, order, signed=code in _SIGNED)
    return bytes(out)


def pack(fmt, /, *args):
    return _pack(fmt, args)


def pack_into(fmt, buffer, offset, /, *args):
    data = _pack(fmt, args)
    if offset < 0:
        offset += len(buffer)
    if offset + len(data) > len(buffer):
        raise error('pack_into requires a buffer of at least %d bytes for packing %d bytes at offset %d (actual buffer size is %d)'
                    % (offset + len(data), len(data), offset, len(buffer)))
    buffer[offset:offset + len(data)] = data


def _unpack(fmt, data, base=0):
    items, size, little = _parse(fmt)
    order = 'little' if little else 'big'
    result = []
    for code, offset, width in items:
        offset += base
        if code == 's':
            result.append(bytes(data[offset:offset + width]))
        elif code == 'p':
            n = min(data[offset], width - 1) if width else 0
            result.append(bytes(data[offset + 1:offset + 1 + n]))
        elif code == 'c':
            result.append(bytes(data[offset:offset + 1]))
        elif code == '?':
            result.append(data[offset] != 0)
        elif code in 'efd':
            bits = int.from_bytes(data[offset:offset + width], order)
            result.append(_nova.unpack_float(bits, width))
        else:
            result.append(int.from_bytes(data[offset:offset + width], order, signed=code in _SIGNED))
    return tuple(result)


def unpack(fmt, buffer, /):
    size = calcsize(fmt)
    if len(buffer) != size:
        raise error('unpack requires a buffer of %d bytes' % size)
    return _unpack(fmt, buffer)


def unpack_from(fmt, /, buffer, offset=0):
    size = calcsize(fmt)
    if offset < 0:
        offset += len(buffer)
    if len(buffer) - offset < size:
        raise error('unpack_from requires a buffer of at least %d bytes for unpacking %d bytes at offset %d (actual buffer size is %d)'
                    % (size + offset, size, offset, len(buffer)))
    return _unpack(fmt, buffer, offset)


def iter_unpack(fmt, buffer, /):
    size = calcsize(fmt)
    if size == 0:
        raise error('cannot iteratively unpack with a struct of length 0')
    if len(buffer) % size:
        raise error('iterative unpacking requires a buffer of a multiple of %d bytes' % size)
    for i in range(0, len(buffer), size):
        yield _unpack(fmt, buffer, i)


class Struct:
    def __init__(self, format):
        self.format = format.decode('ascii') if isinstance(format, bytes) else format
        self.size = calcsize(format)

    def pack(self, *args):
        return _pack(self.format, args)

    def pack_into(self, buffer, offset, *args):
        pack_into(self.format, buffer, offset, *args)

    def unpack(self, buffer):
        return unpack(self.format, buffer)

    def unpack_from(self, buffer, offset=0):
        return unpack_from(self.format, buffer, offset)

    def iter_unpack(self, buffer):
        return iter_unpack(self.format, buffer)

    def __repr__(self):
        return 'Struct(%r)' % self.format
