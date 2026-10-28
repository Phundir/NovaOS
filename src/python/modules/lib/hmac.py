"""hmac for Nova's Python (RFC 2104), over hashlib."""

import hashlib as _hashlib

digest_size = None


class HMAC:
    blocksize = 64

    def __init__(self, key, msg=None, digestmod=''):
        if not isinstance(key, (bytes, bytearray)):
            raise TypeError("key: expected bytes or bytearray, but got %r" % type(key).__name__)
        if not digestmod:
            raise TypeError("Missing required argument 'digestmod'.")
        if isinstance(digestmod, str):
            make = lambda d=b'': _hashlib.new(digestmod, d)
        elif callable(digestmod):
            make = digestmod
        else:
            make = lambda d=b'': digestmod.new(d)
        self._make = make
        self._outer = make()
        self._inner = make()
        self.digest_size = self._inner.digest_size
        blocksize = self._inner.block_size
        if len(key) > blocksize:
            key = make(key).digest()
        self.block_size = blocksize
        key = bytes(key) + bytes(blocksize - len(key))
        self._outer.update(bytes(x ^ 0x5C for x in key))
        self._inner.update(bytes(x ^ 0x36 for x in key))
        if msg is not None:
            self.update(msg)

    @property
    def name(self):
        return "hmac-" + self._inner.name

    def update(self, msg):
        self._inner.update(msg)

    def copy(self):
        other = self.__class__.__new__(self.__class__)
        other._make = self._make
        other.digest_size = self.digest_size
        other.block_size = self.block_size
        other._inner = self._inner.copy()
        other._outer = self._outer.copy()
        return other

    def _current(self):
        h = self._outer.copy()
        h.update(self._inner.digest())
        return h

    def digest(self):
        return self._current().digest()

    def hexdigest(self):
        return self._current().hexdigest()


def new(key, msg=None, digestmod=''):
    return HMAC(key, msg, digestmod)


def digest(key, msg, digest):
    return HMAC(key, msg, digest).digest()


def compare_digest(a, b):
    if isinstance(a, str) and isinstance(b, str):
        a, b = a.encode(), b.encode()
    if len(a) != len(b):
        return False
    result = 0
    for x, y in zip(a, b):
        result |= x ^ y
    return result == 0
