"""glob for Nova's Python: pathname patterns over Nova's file system,
sorted like os.listdir returns them, hidden files skipped unless asked."""

import os
import re
import fnmatch

__all__ = ["glob", "iglob", "escape"]

magic_check = re.compile('([*?[])')


def has_magic(s):
    return magic_check.search(s) is not None


def escape(pathname):
    drive, pathname = '', pathname
    pathname = magic_check.sub(r'[\1]', pathname)
    return drive + pathname


def _listdir(dirname, dironly):
    try:
        names = os.listdir(dirname or os.curdir)
    except OSError:
        return []
    if dironly:
        names = [n for n in names if os.path.isdir(os.path.join(dirname, n))]
    return names


def _glob1(dirname, pattern, dironly, include_hidden):
    names = _listdir(dirname, dironly)
    if not (include_hidden or pattern.startswith('.')):
        names = [x for x in names if not x.startswith('.')]
    return fnmatch.filter(names, pattern)


def _glob0(dirname, basename, dironly):
    if basename:
        if os.path.exists(os.path.join(dirname, basename)):
            return [basename]
    else:
        if os.path.isdir(dirname):
            return [basename]
    return []


def _rlistdir(dirname, dironly, include_hidden):
    names = _listdir(dirname, dironly)
    for x in names:
        if include_hidden or not x.startswith('.'):
            yield x
            p = os.path.join(dirname, x) if dirname else x
            for y in _rlistdir(p, dironly, include_hidden):
                yield os.path.join(x, y)


def _glob2(dirname, pattern, dironly, include_hidden):
    yield pattern[:0]
    yield from _rlistdir(dirname, dironly, include_hidden)


def _iglob(pathname, root_dir, recursive, dironly, include_hidden):
    dirname, basename = os.path.split(pathname)
    if not has_magic(pathname):
        if basename:
            if os.path.exists(os.path.join(root_dir, pathname) if root_dir else pathname):
                yield pathname
        else:
            if os.path.isdir(os.path.join(root_dir, dirname) if root_dir else dirname):
                yield pathname
        return
    if not dirname:
        if recursive and basename == '**':
            yield from _glob2(root_dir or '', basename, dironly, include_hidden)
        else:
            yield from _glob1(root_dir or '', basename, dironly, include_hidden)
        return
    if dirname != pathname and has_magic(dirname):
        dirs = _iglob(dirname, root_dir, recursive, True, include_hidden)
    else:
        dirs = [dirname]
    if has_magic(basename):
        if recursive and basename == '**':
            glob_in_dir = _glob2
        else:
            glob_in_dir = _glob1
    else:
        glob_in_dir = lambda d, b, o, h: _glob0(d, b, o)
    for dirname in dirs:
        full = os.path.join(root_dir, dirname) if root_dir else dirname
        for name in glob_in_dir(full, basename, dironly, include_hidden):
            yield os.path.join(dirname, name)


def iglob(pathname, *, root_dir=None, dir_fd=None, recursive=False, include_hidden=False):
    pathname = os.fspath(pathname)
    it = _iglob(pathname, root_dir, recursive, False, include_hidden)
    if recursive and pathname.endswith('**') is False and False:
        pass
    return it


def glob(pathname, *, root_dir=None, dir_fd=None, recursive=False, include_hidden=False):
    return list(iglob(pathname, root_dir=root_dir, dir_fd=dir_fd,
                      recursive=recursive, include_hidden=include_hidden))
