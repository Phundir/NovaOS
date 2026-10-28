"""hashlib for Nova's Python: md5, sha1 and the SHA-2 family.

The digests are computed by Nova's own code (_nova.digest) over the data
gathered by update(); hmac and pbkdf2_hmac are built on them as CPython
specifies.
"""

import _nova

__all__ = ('md5', 'sha1', 'sha224', 'sha256', 'sha384', 'sha512', 'new',
           'algorithms_guaranteed', 'algorithms_available', 'pbkdf2_hmac',
           'file_digest')

_SIZES = {'md5': (16, 64), 'sha1': (20, 64), 'sha224': (28, 64),
          'sha256': (32, 64), 'sha384': (48, 128), 'sha512': (64, 128)}

algorithms_guaranteed = frozenset(_SIZES)
algorithms_available = frozenset(_SIZES)


class _Hash:
    __slots__ = ('name', '_data')

    def __init__(self, name, data=b''):
        self.name = name
        self._data = bytearray()
        if data:
            self.update(data)

    @property
    def digest_size(self):
        return _SIZES[self.name][0]

    @property
    def block_size(self):
        return _SIZES[self.name][1]

    def update(self, data):
        if isinstance(data, str):
            raise TypeError('Strings must be encoded before hashing')
        self._data += data

    def digest(self):
        return _nova.digest(self.name, bytes(self._data))

    def hexdigest(self):
        return self.digest().hex()

    def copy(self):
        other = _Hash(self.name)
        other._data = bytearray(self._data)
        return other

    def __repr__(self):
        return f'<{self.name} _hashlib.HASH object @ {hex(id(self))}>'


def new(name, data=b'', *, usedforsecurity=True):
    key = name.lower().replace('-', '')
    if key not in _SIZES:
        raise ValueError('unsupported hash type ' + name)
    return _Hash(key, data)


def md5(data=b'', *, usedforsecurity=True):
    return _Hash('md5', data)


def sha1(data=b'', *, usedforsecurity=True):
    return _Hash('sha1', data)


def sha224(data=b'', *, usedforsecurity=True):
    return _Hash('sha224', data)


def sha256(data=b'', *, usedforsecurity=True):
    return _Hash('sha256', data)


def sha384(data=b'', *, usedforsecurity=True):
    return _Hash('sha384', data)


def sha512(data=b'', *, usedforsecurity=True):
    return _Hash('sha512', data)


def _hmac(name, key, msg):
    block = _SIZES[name][1]
    if len(key) > block:
        key = _nova.digest(name, key)
    key = key + bytes(block - len(key))
    inner = _nova.digest(name, bytes(k ^ 0x36 for k in key) + msg)
    return _nova.digest(name, bytes(k ^ 0x5c for k in key) + inner)


def pbkdf2_hmac(hash_name, password, salt, iterations, dklen=None):
    name = hash_name.lower()
    if name not in _SIZES:
        raise ValueError('unsupported hash type ' + hash_name)
    if iterations < 1:
        raise ValueError('iteration value must be greater than 0.')
    size = _SIZES[name][0]
    if dklen is None:
        dklen = size
    if dklen < 1:
        raise ValueError('key length must be greater than 0.')
    out = bytearray()
    block = 1
    while len(out) < dklen:
        u = _hmac(name, password, salt + block.to_bytes(4, 'big'))
        acc = int.from_bytes(u, 'big')
        for _ in range(iterations - 1):
            u = _hmac(name, password, u)
            acc ^= int.from_bytes(u, 'big')
        out += acc.to_bytes(size, 'big')
        block += 1
    return bytes(out[:dklen])


def file_digest(fileobj, digest, /):
    h = new(digest) if isinstance(digest, str) else digest()
    h.update(fileobj.read())
    return h
