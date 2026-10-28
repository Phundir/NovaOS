"""The parts of Nova's os module written in Python, set onto it when it
is first imported: stat results, scandir, path helpers, process facts.
"""

import os
import time as _time

path = os.path


class stat_result(tuple):
    n_sequence_fields = 10

    def __new__(cls, values):
        return tuple.__new__(cls, values)

    st_mode = property(lambda self: self[0])
    st_ino = property(lambda self: self[1])
    st_dev = property(lambda self: self[2])
    st_nlink = property(lambda self: self[3])
    st_uid = property(lambda self: self[4])
    st_gid = property(lambda self: self[5])
    st_size = property(lambda self: self[6])
    st_atime = property(lambda self: self[7])
    st_mtime = property(lambda self: self[8])
    st_ctime = property(lambda self: self[9])

    def __repr__(self):
        names = ('st_mode', 'st_ino', 'st_dev', 'st_nlink', 'st_uid', 'st_gid',
                 'st_size', 'st_atime', 'st_mtime', 'st_ctime')
        return 'os.stat_result(' + ', '.join('%s=%r' % (n, v) for n, v in zip(names, self)) + ')'


def stat(p, *, follow_symlinks=True):
    p = os.fspath(p)
    if path.isdir(p):
        return stat_result((0o40755, 0, 0, 2, 0, 0, 4096, 0, 0, 0))
    if path.isfile(p):
        return stat_result((0o100644, 0, 0, 1, 0, 0, path.getsize(p), 0, 0, 0))
    raise FileNotFoundError(2, 'No such file or directory', p)


lstat = stat


class DirEntry:
    def __init__(self, directory, name):
        self.name = name
        self.path = path.join(directory, name)

    def is_dir(self, *, follow_symlinks=True):
        return path.isdir(self.path)

    def is_file(self, *, follow_symlinks=True):
        return path.isfile(self.path)

    def is_symlink(self):
        return False

    def stat(self, *, follow_symlinks=True):
        return stat(self.path)

    def __fspath__(self):
        return self.path

    def __repr__(self):
        return '<DirEntry %r>' % self.name


class _ScandirIterator:
    def __init__(self, directory):
        self._entries = iter([DirEntry(directory, n) for n in os.listdir(directory)])

    def __iter__(self):
        return self

    def __next__(self):
        return next(self._entries)

    def __enter__(self):
        return self

    def __exit__(self, *args):
        pass

    def close(self):
        pass


def scandir(p='.'):
    return _ScandirIterator(os.fspath(p))


_state = [0x2545F4914F6CDD1D ^ _time.time_ns()]


def urandom(n):
    """Bytes from a xorshift generator seeded by the clock: Nova's Python
    has no entropy source of its own, these are not for cryptography."""
    out = bytearray()
    x = _state[0]
    while len(out) < n:
        x ^= (x << 13) & 0xFFFFFFFFFFFFFFFF
        x ^= x >> 7
        x ^= (x << 17) & 0xFFFFFFFFFFFFFFFF
        out += x.to_bytes(8, 'little')
    _state[0] = x
    return bytes(out[:n])


def getpid():
    return 1


def getppid():
    return 0


def cpu_count():
    return 1


def get_terminal_size(fd=1):
    return terminal_size((80, 24))


class terminal_size(tuple):
    columns = property(lambda self: self[0])
    lines = property(lambda self: self[1])


def removedirs(name):
    os.rmdir(name)
    head, tail = path.split(name)
    while head and tail:
        try:
            os.rmdir(head)
        except OSError:
            break
        head, tail = path.split(head)


def putenv(key, value):
    os.environ[key] = value


def unsetenv(key):
    os.environ.pop(key, None)


def _commonprefix(m):
    if not m:
        return ''
    s1 = min(m)
    s2 = max(m)
    for i, c in enumerate(s1):
        if c != s2[i]:
            return s1[:i]
    return s1


def _relpath(p, start=None):
    if not p:
        raise ValueError("no path specified")
    p = os.fspath(p)
    start = os.curdir if start is None else os.fspath(start)
    start_list = [x for x in path.abspath(start).split('/') if x]
    path_list = [x for x in path.abspath(p).split('/') if x]
    i = len(_commonprefix([start_list, path_list]))
    rel_list = [os.pardir] * (len(start_list) - i) + path_list[i:]
    if not rel_list:
        return os.curdir
    return path.join(*rel_list)


def _commonpath(paths):
    paths = [os.fspath(p) for p in paths]
    if not paths:
        raise ValueError('commonpath() arg is an empty sequence')
    isabs = {p.startswith('/') for p in paths}
    if len(isabs) != 1:
        raise ValueError("Can't mix absolute and relative paths")
    split = [[c for c in p.split('/') if c and c != '.'] for p in paths]
    s1 = min(split)
    s2 = max(split)
    common = s1
    for i, c in enumerate(s1):
        if c != s2[i]:
            common = s1[:i]
            break
    prefix = '/' if paths[0].startswith('/') else ''
    return prefix + '/'.join(common)


def _splitdrive(p):
    return p[:0], p


def _getmtime(p):
    return float(stat(p).st_mtime)


def _expandvars(p):
    import re
    def repl(m):
        name = m.group(1) or m.group(2)
        return os.environ.get(name, m.group(0))
    return re.sub(r'\$(\w+)|\$\{([^}]*)\}', repl, p)


def _samefile(a, b):
    return path.abspath(a) == path.abspath(b)


def _lexists(p):
    return path.exists(p)


def _islink(p):
    return False


def _ismount(p):
    return path.abspath(p) == '/'


path.normcase = lambda s: os.fspath(s)
path.commonprefix = _commonprefix
path.relpath = _relpath
path.commonpath = _commonpath
path.splitdrive = _splitdrive
path.getmtime = _getmtime
path.getatime = _getmtime
path.getctime = _getmtime
path.expandvars = _expandvars
path.samefile = _samefile
path.lexists = _lexists
path.islink = _islink
path.ismount = _ismount
path.curdir = '.'
path.pardir = '..'
path.extsep = '.'
path.altsep = None
path.pathsep = ':'
path.defpath = '/bin'
path.devnull = '/dev/null'

for _name in ('stat_result', 'stat', 'lstat', 'DirEntry', 'scandir', 'urandom', 'getpid', 'getppid',
              'cpu_count', 'get_terminal_size', 'terminal_size', 'removedirs', 'putenv', 'unsetenv'):
    setattr(os, _name, globals()[_name])
os.extsep = '.'
os.pathsep = ':'
os.altsep = None
os.devnull = '/dev/null'
os.defpath = '/bin'
os.F_OK, os.R_OK, os.W_OK, os.X_OK = 0, 4, 2, 1
os.SEEK_SET, os.SEEK_CUR, os.SEEK_END = 0, 1, 2
os.PathLike = type('PathLike', (), {'__fspath__': lambda self: NotImplemented})
os.error = OSError
