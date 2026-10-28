"""io for Nova's Python: in-memory streams (StringIO, BytesIO) and the
names of the io module; open() is the built-in file of Nova's Python.
"""

__all__ = ['StringIO', 'BytesIO', 'open', 'SEEK_SET', 'SEEK_CUR', 'SEEK_END',
           'UnsupportedOperation', 'IOBase', 'TextIOBase', 'BufferedIOBase',
           'RawIOBase', 'DEFAULT_BUFFER_SIZE', 'text_encoding']

SEEK_SET = 0
SEEK_CUR = 1
SEEK_END = 2
DEFAULT_BUFFER_SIZE = 8192

open = open


class UnsupportedOperation(OSError, ValueError):
    pass


def text_encoding(encoding, stacklevel=2):
    return encoding if encoding is not None else 'locale'


class IOBase:
    def __enter__(self):
        self._check_closed()
        return self

    def __exit__(self, *args):
        self.close()

    def __iter__(self):
        self._check_closed()
        return self

    def __next__(self):
        line = self.readline()
        if not line:
            raise StopIteration
        return line

    _closed_message = 'I/O operation on closed file.'

    def _check_closed(self):
        if self.closed:
            raise ValueError(self._closed_message)

    @property
    def closed(self):
        return self._closed

    def close(self):
        self._closed = True

    def readable(self):
        self._check_closed()
        return True

    def writable(self):
        self._check_closed()
        return True

    def seekable(self):
        self._check_closed()
        return True

    def isatty(self):
        self._check_closed()
        return False

    def flush(self):
        self._check_closed()

    def fileno(self):
        raise UnsupportedOperation('fileno')

    def readlines(self, hint=-1):
        lines = []
        total = 0
        for line in self:
            lines.append(line)
            total += len(line)
            if 0 < hint <= total:
                break
        return lines

    def writelines(self, lines):
        for line in lines:
            self.write(line)


class RawIOBase(IOBase):
    pass


class BufferedIOBase(IOBase):
    pass


class TextIOBase(IOBase):
    pass


class _Memory(IOBase):
    """A buffer and a position, shared by StringIO and BytesIO."""

    def _init(self, initial, empty):
        self._buf = initial if initial is not None else empty
        self._pos = 0
        self._closed = False
        self._empty = empty

    def getvalue(self):
        self._check_closed()
        return self._buf

    def tell(self):
        self._check_closed()
        return self._pos

    def seek(self, pos, whence=0):
        self._check_closed()
        if not isinstance(pos, int):
            raise TypeError("'%s' object cannot be interpreted as an integer" % type(pos).__name__)
        if whence == 0:
            if pos < 0:
                raise ValueError('negative seek value %d' % pos)
            self._pos = pos
        elif whence == 1:
            if pos != 0 and isinstance(self, StringIO):
                raise OSError("Can't do nonzero cur-relative seeks")
            self._pos = max(0, self._pos + pos)
        elif whence == 2:
            if pos != 0 and isinstance(self, StringIO):
                raise OSError("Can't do nonzero end-relative seeks")
            self._pos = max(0, len(self._buf) + pos)
        else:
            raise ValueError('invalid whence (%d, should be 0, 1 or 2)' % whence)
        return self._pos

    def read(self, size=-1):
        self._check_closed()
        if size is None or size < 0:
            end = len(self._buf)
        else:
            end = min(len(self._buf), self._pos + size)
        data = self._buf[self._pos:end]
        self._pos = max(self._pos, end)
        return data

    def readline(self, size=-1):
        self._check_closed()
        nl = self._newline_char()
        i = self._buf.find(nl, self._pos)
        end = len(self._buf) if i < 0 else i + 1
        if size is not None and size >= 0:
            end = min(end, self._pos + size)
        data = self._buf[self._pos:end]
        self._pos = max(self._pos, end)
        return data

    def write(self, data):
        self._check_closed()
        data = self._coerce(data)
        n = len(data)
        if self._pos > len(self._buf):
            self._buf = self._buf + self._empty_padding(self._pos - len(self._buf))
        self._buf = self._buf[:self._pos] + data + self._buf[self._pos + n:]
        self._pos += n
        return n

    def truncate(self, size=None):
        self._check_closed()
        if size is None:
            size = self._pos
        if size < 0:
            raise ValueError('negative size value %d' % size)
        self._buf = self._buf[:size]
        return size


class StringIO(_Memory, TextIOBase):
    _closed_message = 'I/O operation on closed file'

    def __init__(self, initial_value='', newline='\n'):
        if initial_value is not None and not isinstance(initial_value, str):
            raise TypeError('initial_value must be str or None, not %s' % type(initial_value).__name__)
        self._newline = newline
        if newline is None and initial_value:
            initial_value = initial_value.replace('\r\n', '\n').replace('\r', '\n')
        self._init(initial_value or '', '')

    def _newline_char(self):
        return '\n'

    def _empty_padding(self, n):
        return '\0' * n

    def _coerce(self, data):
        if not isinstance(data, str):
            raise TypeError('string argument expected, got %r' % type(data).__name__)
        if self._newline is None:
            data = data.replace('\r\n', '\n').replace('\r', '\n')
        elif self._newline not in ('', '\n'):
            data = data.replace('\n', self._newline)
        return data

    @property
    def line_buffering(self):
        return False

    @property
    def newlines(self):
        return None

    def __repr__(self):
        return '<_io.StringIO object at %s>' % hex(id(self))


class BytesIO(_Memory, BufferedIOBase):
    def __init__(self, initial_bytes=b''):
        self._init(bytes(initial_bytes) if initial_bytes else b'', b'')

    def _newline_char(self):
        return b'\n'

    def _empty_padding(self, n):
        return b'\0' * n

    def _coerce(self, data):
        if isinstance(data, str):
            raise TypeError("a bytes-like object is required, not 'str'")
        return bytes(data)

    def getbuffer(self):
        return bytearray(self._buf)

    def read1(self, size=-1):
        return self.read(size)

    def readinto(self, b):
        data = self.read(len(b))
        b[:len(data)] = data
        return len(data)

    def __repr__(self):
        return '<_io.BytesIO object at %s>' % hex(id(self))
