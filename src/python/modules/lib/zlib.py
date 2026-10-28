"""zlib for Nova's Python: checksums, compression and decompression.

Decompression is Nova's inflater (raw, zlib and gzip streams, chosen by
wbits as zlib does). Compression writes valid streams with Nova's own
DEFLATE (LZ77 and fixed Huffman codes): they decompress anywhere, but
are not byte for byte the zlib library's output.
"""

import _nova

MAX_WBITS = 15
DEFLATED = 8
DEF_MEM_LEVEL = 8
DEF_BUF_SIZE = 16384
Z_NO_COMPRESSION = 0
Z_BEST_SPEED = 1
Z_BEST_COMPRESSION = 9
Z_DEFAULT_COMPRESSION = -1
Z_FILTERED = 1
Z_HUFFMAN_ONLY = 2
Z_RLE = 3
Z_FIXED = 4
Z_DEFAULT_STRATEGY = 0
Z_NO_FLUSH = 0
Z_PARTIAL_FLUSH = 1
Z_SYNC_FLUSH = 2
Z_FULL_FLUSH = 3
Z_FINISH = 4
Z_BLOCK = 5
ZLIB_VERSION = '1.3.1'
ZLIB_RUNTIME_VERSION = '1.3.1'


class error(Exception):
    pass


def crc32(data, value=0, /):
    return _nova.crc32(data, value)


def adler32(data, value=1, /):
    return _nova.adler32(data, value)


def _level(level):
    if level == -1:
        return 6
    if not 0 <= level <= 9:
        raise error('Bad compression level')
    return level


def _wrap(raw, data, wbits):
    if -15 <= wbits <= -9:
        return raw
    if 25 <= wbits <= 31:
        header = b'\x1f\x8b\x08\x00\x00\x00\x00\x00\x00\xff'
        return header + raw + crc32(data).to_bytes(4, 'little') + (len(data) & 0xffffffff).to_bytes(4, 'little')
    if 9 <= wbits <= 15:
        return b'\x78\x9c' + raw + adler32(data).to_bytes(4, 'big')
    raise ValueError('Invalid initialization option')


def compress(data, /, level=-1, wbits=MAX_WBITS):
    data = bytes(data)
    return _wrap(_nova.deflate(data, _level(level)), data, wbits)


def _mode(wbits):
    if -15 <= wbits <= -8:
        return 0
    if 25 <= wbits <= 31:
        return 2
    if 41 <= wbits <= 47 or wbits == 0 or 8 <= wbits <= 15:
        return 1
    raise ValueError('Invalid initialization option')


def decompress(data, /, wbits=MAX_WBITS, bufsize=DEF_BUF_SIZE):
    data = bytes(data)
    mode = _mode(wbits)
    if 41 <= wbits <= 47 and data[:2] == b'\x1f\x8b':
        mode = 2
    out = _nova.inflate(data, mode)
    if out is None:
        raise error('Error -3 while decompressing data: invalid stored block lengths'
                    if not data else 'Error -3 while decompressing data: incorrect header check')
    return out


class _Compress:
    def __init__(self, level, wbits):
        self._level = _level(level)
        self._wbits = wbits
        self._data = bytearray()
        self._done = False

    def compress(self, data):
        if self._done:
            raise error('Error -2 while compressing data: inconsistent stream state')
        self._data += data
        return b''

    def flush(self, mode=Z_FINISH):
        if mode != Z_FINISH:
            return b''
        self._done = True
        data = bytes(self._data)
        return _wrap(_nova.deflate(data, self._level), data, self._wbits)

    def copy(self):
        other = _Compress(self._level, self._wbits)
        other._data = bytearray(self._data)
        return other


class _Decompress:
    def __init__(self, wbits):
        self._wbits = wbits
        self._data = bytearray()
        self.unused_data = b''
        self.unconsumed_tail = b''
        self.eof = False

    def decompress(self, data, max_length=0):
        self._data += data
        try:
            out = decompress(bytes(self._data), self._wbits)
        except error:
            return b''
        self.eof = True
        self._data = bytearray()
        return out

    def flush(self, length=DEF_BUF_SIZE):
        return b''


def compressobj(level=-1, method=DEFLATED, wbits=MAX_WBITS, memLevel=DEF_MEM_LEVEL,
                strategy=Z_DEFAULT_STRATEGY, zdict=None):
    return _Compress(level, wbits)


def decompressobj(wbits=MAX_WBITS, zdict=b''):
    return _Decompress(wbits)
