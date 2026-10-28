"""pathlib for Nova's Python: POSIX paths as objects.

PurePosixPath does the lexical work (parts, names, suffixes, joining,
relative_to, match), Path adds the file system through os. On Nova every
path is POSIX, so Path is PosixPath and the Windows flavours are absent.
"""

import os
import fnmatch
import re

__all__ = ["PurePath", "PurePosixPath", "Path", "PosixPath", "UnsupportedOperation"]


class UnsupportedOperation(NotImplementedError):
    pass


def _parse(path):
    if not path:
        return '', []
    root = ''
    if path.startswith('/'):
        root = '//' if path.startswith('//') and not path.startswith('///') else '/'
    parts = [p for p in path.split('/') if p and p != '.']
    return root, parts


class PurePath:
    __slots__ = ('_root', '_tail', '_str', '_hash')

    def __new__(cls, *args):
        if cls is PurePath:
            cls = PurePosixPath
        return object.__new__(cls)

    def __init__(self, *args):
        paths = []
        for a in args:
            if isinstance(a, PurePath):
                paths.append(str(a))
            else:
                try:
                    p = os.fspath(a)
                except TypeError:
                    raise TypeError("argument should be a str or an os.PathLike object "
                                    "where __fspath__ returns a str, not %r" % type(a).__name__)
                if not isinstance(p, str):
                    raise TypeError("argument should be a str or an os.PathLike object "
                                    "where __fspath__ returns a str, not %r" % type(p).__name__)
                paths.append(p)
        root = ''
        tail = []
        for p in paths:
            r, t = _parse(p)
            if r:
                root, tail = r, t
            else:
                tail = tail + t
        self._root = root
        self._tail = tail
        self._str = None
        self._hash = None

    def with_segments(self, *pathsegments):
        return type(self)(*pathsegments)

    def _from(self, root, tail):
        p = object.__new__(type(self))
        p._root = root
        p._tail = list(tail)
        p._str = None
        p._hash = None
        return p

    def __str__(self):
        if self._str is None:
            s = self._root + '/'.join(self._tail)
            self._str = s or '.'
        return self._str

    def __fspath__(self):
        return str(self)

    def as_posix(self):
        return str(self)

    def __bytes__(self):
        return os.fsencode(self) if hasattr(os, 'fsencode') else str(self).encode()

    def __repr__(self):
        return "{}({!r})".format(self.__class__.__name__, self.as_posix())

    def as_uri(self):
        if not self.is_absolute():
            raise ValueError("relative path can't be expressed as a file URI")
        return 'file://' + str(self)

    def _key(self):
        return str(self)

    def __hash__(self):
        if self._hash is None:
            self._hash = hash(self._key())
        return self._hash

    def __eq__(self, other):
        if not isinstance(other, PurePath):
            return NotImplemented
        return self._key() == other._key()

    def __lt__(self, other):
        if not isinstance(other, PurePath):
            return NotImplemented
        return self._tail_key() < other._tail_key()

    def __le__(self, other):
        if not isinstance(other, PurePath):
            return NotImplemented
        return self._tail_key() <= other._tail_key()

    def __gt__(self, other):
        if not isinstance(other, PurePath):
            return NotImplemented
        return self._tail_key() > other._tail_key()

    def __ge__(self, other):
        if not isinstance(other, PurePath):
            return NotImplemented
        return self._tail_key() >= other._tail_key()

    def _tail_key(self):
        return [self._root] + self._tail if self._root else self._tail

    @property
    def drive(self):
        return ''

    @property
    def root(self):
        return self._root

    @property
    def anchor(self):
        return self._root

    @property
    def parts(self):
        if self._root:
            return (self._root,) + tuple(self._tail)
        return tuple(self._tail)

    @property
    def name(self):
        return self._tail[-1] if self._tail else ''

    @property
    def suffix(self):
        name = self.name
        i = name.rfind('.')
        if 0 < i < len(name) - 1:
            return name[i:]
        return ''

    @property
    def suffixes(self):
        name = self.name
        if name.endswith('.'):
            return []
        name = name.lstrip('.')
        return ['.' + s for s in name.split('.')[1:]]

    @property
    def stem(self):
        name = self.name
        i = name.rfind('.')
        if 0 < i < len(name) - 1:
            return name[:i]
        return name

    @property
    def parent(self):
        if not self._tail:
            return self
        return self._from(self._root, self._tail[:-1])

    @property
    def parents(self):
        return _Parents(self)

    def with_name(self, name):
        if not self.name:
            raise ValueError("%r has an empty name" % (self,))
        if not name or '/' in name or name == '.':
            raise ValueError("Invalid name %r" % (name))
        return self._from(self._root, self._tail[:-1] + [name])

    def with_stem(self, stem):
        suffix = self.suffix
        if not suffix:
            return self.with_name(stem)
        elif not stem:
            raise ValueError(f"{self!r} has a non-empty suffix")
        return self.with_name(stem + suffix)

    def with_suffix(self, suffix):
        if suffix and not suffix.startswith('.') or suffix == '.':
            raise ValueError("Invalid suffix %r" % (suffix))
        if '/' in suffix:
            raise ValueError("Invalid suffix %r" % (suffix))
        name = self.name
        if not name:
            raise ValueError(f"{self!r} has an empty name")
        old = self.suffix
        if not old:
            return self.with_name(name + suffix)
        return self.with_name(name[:-len(old)] + suffix)

    def relative_to(self, other, /, *_deprecated, walk_up=False):
        other = self.with_segments(other, *_deprecated)
        if other._root != self._root:
            raise ValueError(f"{str(self)!r} is not in the subpath of {str(other)!r}")
        n = len(other._tail)
        if self._tail[:n] == other._tail:
            return self._from('', self._tail[n:])
        if not walk_up:
            raise ValueError(f"{str(self)!r} is not in the subpath of {str(other)!r}")
        common = 0
        for a, b in zip(self._tail, other._tail):
            if a != b:
                break
            common += 1
        if '..' in other._tail[common:]:
            raise ValueError(f"'..' segment in {str(other)!r} cannot be walked")
        return self._from('', ['..'] * (n - common) + self._tail[common:])

    def is_relative_to(self, other, /, *_deprecated):
        other = self.with_segments(other, *_deprecated)
        return other._root == self._root and self._tail[:len(other._tail)] == other._tail

    def is_absolute(self):
        return bool(self._root)

    def is_reserved(self):
        return False

    def joinpath(self, *pathsegments):
        return self.with_segments(self, *pathsegments)

    def __truediv__(self, key):
        try:
            return self.with_segments(self, key)
        except TypeError:
            return NotImplemented

    def __rtruediv__(self, key):
        try:
            return self.with_segments(key, self)
        except TypeError:
            return NotImplemented

    def full_match(self, pattern, *, case_sensitive=None):
        pat = self.with_segments(pattern)
        if pat.is_absolute() != self.is_absolute():
            return False
        return _match_parts(self._tail, pat._tail, True)

    def match(self, path_pattern, *, case_sensitive=None):
        pat = self.with_segments(path_pattern)
        if not pat._tail:
            raise ValueError("empty pattern")
        if pat._root:
            if not self._root or len(pat._tail) != len(self._tail):
                return False
        elif len(pat._tail) > len(self._tail):
            return False
        for part, p in zip(reversed(self._tail), reversed(pat._tail)):
            if not fnmatch.fnmatchcase(part, p):
                return False
        return True


def _match_parts(parts, pattern, anchored):
    if not pattern:
        return not parts
    head = pattern[0]
    if head == '**':
        for i in range(len(parts) + 1):
            if _match_parts(parts[i:], pattern[1:], anchored):
                return True
        return False
    if not parts:
        return False
    return fnmatch.fnmatchcase(parts[0], head) and _match_parts(parts[1:], pattern[1:], anchored)


class _Parents:
    def __init__(self, path):
        self._path = path
        self._root = path._root
        self._tail = path._tail

    def __len__(self):
        return len(self._tail)

    def __getitem__(self, idx):
        if isinstance(idx, slice):
            return tuple(self[i] for i in range(*idx.indices(len(self))))
        if idx >= len(self) or idx < -len(self):
            raise IndexError(idx)
        if idx < 0:
            idx += len(self)
        return self._path._from(self._root, self._tail[:-idx - 1])

    def __iter__(self):
        for i in range(len(self)):
            yield self[i]

    def __repr__(self):
        return "<{}.parents>".format(type(self._path).__name__)


class PurePosixPath(PurePath):
    __slots__ = ()


class PureWindowsPath(PurePath):
    __slots__ = ()


class Path(PurePath):
    __slots__ = ()

    def __new__(cls, *args, **kwargs):
        if cls is Path:
            cls = PosixPath
        return object.__new__(cls)

    def stat(self, *, follow_symlinks=True):
        return os.stat(self)

    def lstat(self):
        return os.stat(self)

    def exists(self, *, follow_symlinks=True):
        return os.path.exists(str(self))

    def is_dir(self, *, follow_symlinks=True):
        return os.path.isdir(str(self))

    def is_file(self, *, follow_symlinks=True):
        return os.path.isfile(str(self))

    def is_symlink(self):
        return False

    def is_mount(self):
        return str(self.absolute()) == '/'

    def is_junction(self):
        return False

    def samefile(self, other_path):
        return self.resolve() == self.with_segments(other_path).resolve()

    def open(self, mode='r', buffering=-1, encoding=None, errors=None, newline=None):
        if 'b' in mode:
            return open(str(self), mode)
        return open(str(self), mode, encoding=encoding) if encoding else open(str(self), mode)

    def read_text(self, encoding=None, errors=None, newline=None):
        with self.open('r') as f:
            return f.read()

    def read_bytes(self):
        with self.open('rb') as f:
            return f.read()

    def write_text(self, data, encoding=None, errors=None, newline=None):
        if not isinstance(data, str):
            raise TypeError('data must be str, not %s' % data.__class__.__name__)
        with self.open('w') as f:
            return f.write(data)

    def write_bytes(self, data):
        view = bytes(data)
        with self.open('wb') as f:
            return f.write(view)

    def iterdir(self):
        root = str(self)
        for name in os.listdir(root):
            yield self._from(self._root, self._tail + [name])

    def _scan(self, parts, dirs_only=False):
        if not parts:
            yield self
            return
        head, rest = parts[0], parts[1:]
        if head == '**':
            yield from self._scan(rest, dirs_only)
            try:
                children = sorted(os.listdir(str(self)))
            except OSError:
                return
            for name in children:
                child = self._from(self._root, self._tail + [name])
                if child.is_dir():
                    yield from child._scan(parts, dirs_only)
            return
        if not fnmatch.translate(head).startswith('(?s:') or any(c in head for c in '*?['):
            try:
                names = os.listdir(str(self))
            except OSError:
                return
            for name in names:
                if fnmatch.fnmatchcase(name, head) and (not name.startswith('.') or head.startswith('.')):
                    child = self._from(self._root, self._tail + [name])
                    if rest and not child.is_dir():
                        continue
                    yield from child._scan(rest, dirs_only)
        else:
            child = self._from(self._root, self._tail + [head])
            if child.exists():
                yield from child._scan(rest, dirs_only)

    def glob(self, pattern, *, case_sensitive=None, recurse_symlinks=False):
        pat = self.with_segments(pattern)
        if pat._root:
            raise NotImplementedError("Non-relative patterns are unsupported")
        if not pat._tail:
            raise ValueError("Unacceptable pattern: {!r}".format(pattern))
        seen = set()
        for p in self._scan(pat._tail):
            key = str(p)
            if key not in seen:
                seen.add(key)
                yield p

    def rglob(self, pattern, *, case_sensitive=None, recurse_symlinks=False):
        return self.glob('**/' + str(pattern))

    def walk(self, top_down=True, on_error=None, follow_symlinks=False):
        stack = [self]
        while stack:
            path = stack.pop()
            if isinstance(path, tuple):
                yield path
                continue
            try:
                names = os.listdir(str(path))
            except OSError as error:
                if on_error is not None:
                    on_error(error)
                continue
            dirnames = []
            filenames = []
            for name in names:
                if os.path.isdir(os.path.join(str(path), name)):
                    dirnames.append(name)
                else:
                    filenames.append(name)
            if top_down:
                yield path, dirnames, filenames
            else:
                stack.append((path, dirnames, filenames))
            stack += [path._from(path._root, path._tail + [d]) for d in reversed(dirnames)]

    def absolute(self):
        if self.is_absolute():
            return self
        return self.with_segments(os.getcwd(), self)

    def resolve(self, strict=False):
        p = self.absolute()
        out = []
        for part in p._tail:
            if part == '..':
                if out:
                    out.pop()
            else:
                out.append(part)
        r = self._from('/', out)
        if strict and not r.exists():
            raise FileNotFoundError(2, 'No such file or directory', str(r))
        return r

    def owner(self):
        return 'nova'

    def group(self):
        return 'nova'

    def readlink(self):
        raise OSError(22, 'Invalid argument', str(self))

    def touch(self, mode=0o666, exist_ok=True):
        if self.exists():
            if not exist_ok:
                raise FileExistsError(17, 'File exists', str(self))
            return
        with self.open('w'):
            pass

    def mkdir(self, mode=0o777, parents=False, exist_ok=False):
        try:
            os.mkdir(str(self))
        except FileNotFoundError:
            if not parents or self.parent == self:
                raise
            self.parent.mkdir(parents=True, exist_ok=True)
            self.mkdir(mode, parents=False, exist_ok=exist_ok)
        except OSError:
            if not exist_ok or not self.is_dir():
                raise

    def chmod(self, mode, *, follow_symlinks=True):
        pass

    def unlink(self, missing_ok=False):
        try:
            os.unlink(str(self))
        except FileNotFoundError:
            if not missing_ok:
                raise

    def rmdir(self):
        os.rmdir(str(self))

    def rename(self, target):
        os.rename(str(self), os.fspath(target))
        return self.with_segments(target)

    def replace(self, target):
        os.replace(str(self), os.fspath(target))
        return self.with_segments(target)

    def expanduser(self):
        if self._tail and self._tail[0].startswith('~') and not self._root:
            home = os.path.expanduser('~')
            return self.with_segments(home, *self._tail[1:])
        return self

    @classmethod
    def cwd(cls):
        return cls(os.getcwd())

    @classmethod
    def home(cls):
        return cls(os.environ.get('HOME', '/'))


class PosixPath(Path, PurePosixPath):
    __slots__ = ()


class WindowsPath(Path, PureWindowsPath):
    __slots__ = ()

    def __new__(cls, *args, **kwargs):
        raise UnsupportedOperation(f"cannot instantiate {cls.__name__!r} on your system")
