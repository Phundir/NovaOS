"""platform for Nova's Python: identify the system and the interpreter.

CPython's platform module probes the host by running `uname`, reading
/etc/os-release, parsing the C library or querying the registry. Nova
needs none of that: the interpreter runs inside the Nova kernel, on
x86_64, so the answers are known when the module is written. system()
is 'Nova', machine() and processor() are 'x86_64', node() is the host
name read from /etc/hostname ('nova' when the file is missing).
release() and version() are empty strings, CPython's convention for
"unknown": the kernel does not publish its version to Python.

The Python side comes from sys.version_info, so python_version() is
'3.13.0'. python_implementation() is 'Nova' (sys.implementation.name),
not 'CPython'. The return types, the uname_result tuple (five fields
shown by repr, processor as sixth item) and the platform() string
cleanup are CPython 3.13's (PSF License).
"""

import os
import re
import sys

__version__ = '1.0.9'

__all__ = ['architecture', 'machine', 'node', 'platform', 'processor',
           'python_branch', 'python_build', 'python_compiler',
           'python_implementation', 'python_revision', 'python_version',
           'python_version_tuple', 'release', 'system', 'system_alias',
           'uname', 'uname_result', 'version', 'libc_ver', 'win32_ver',
           'win32_edition', 'win32_is_iot', 'mac_ver', 'java_ver',
           'freedesktop_os_release']

_SYSTEM = 'Nova'
_MACHINE = 'x86_64'


def libc_ver(executable=None, lib='', version='', chunksize=16384):
    """Nova's Python is not linked against a C library: (lib, version)
    are returned unchanged."""
    return lib, version


def win32_ver(release='', version='', csd='', ptype=''):
    return release, version, csd, ptype


def win32_edition():
    return None


def win32_is_iot():
    return False


def mac_ver(release='', versioninfo=('', '', ''), machine=''):
    return release, versioninfo, machine


def java_ver(release='', vendor='', vminfo=('', '', ''), osinfo=('', '', '')):
    return release, vendor, vminfo, osinfo


def system_alias(system, release, version):
    """Return (system, release, version) aliased to common marketing
    names; Nova has no alias, so they come back unchanged."""
    return system, release, version


def _platform(*args):
    """Join the non-empty parts with '-' into a filename-safe string."""
    platform = '-'.join(x.strip() for x in filter(len, args))
    platform = platform.replace(' ', '_')
    for ch in '/\\:;"()':
        platform = platform.replace(ch, '-')
    platform = platform.replace('unknown', '')
    while True:
        cleaned = platform.replace('--', '-')
        if cleaned == platform:
            break
        platform = cleaned
    while platform and platform[-1] == '-':
        platform = platform[:-1]
    return platform


def _node(default=''):
    try:
        with open('/etc/hostname') as f:
            name = f.read().strip()
    except Exception:
        return default
    return name or default


def architecture(executable=sys.executable, bits='', linkage=''):
    """Return (bits, linkage) of the interpreter: ('64bit', 'ELF')."""
    return '64bit', 'ELF'


class uname_result(tuple):
    """(system, node, release, version, machine, processor), with the
    fields also reachable by name. repr shows the first five fields, as
    CPython's does."""

    _fields = ('system', 'node', 'release', 'version', 'machine', 'processor')

    def __new__(cls, system, node, release, version, machine, processor=None):
        if processor is None:
            processor = _MACHINE
        return tuple.__new__(cls, (system, node, release, version, machine,
                                   processor))

    system = property(lambda self: tuple.__getitem__(self, 0))
    node = property(lambda self: tuple.__getitem__(self, 1))
    release = property(lambda self: tuple.__getitem__(self, 2))
    version = property(lambda self: tuple.__getitem__(self, 3))
    machine = property(lambda self: tuple.__getitem__(self, 4))
    processor = property(lambda self: tuple.__getitem__(self, 5))

    def __repr__(self):
        return ('uname_result(system=%r, node=%r, release=%r, version=%r, '
                'machine=%r)' % tuple(self)[:5])

    def _asdict(self):
        return dict(zip(self._fields, self))

    def _replace(self, **kwargs):
        values = self._asdict()
        values.update(kwargs)
        return uname_result(*[values[f] for f in self._fields])


_uname_cache = None


def uname():
    """Return uname_result(system, node, release, version, machine) with
    processor as sixth item."""
    global _uname_cache
    if _uname_cache is None:
        _uname_cache = uname_result(_SYSTEM, _node('nova'), '', '', _MACHINE,
                                    _MACHINE)
    return _uname_cache


def system():
    """Return the system/OS name: 'Nova'."""
    return uname().system


def node():
    """Return the computer's network name."""
    return uname().node


def release():
    """Return the system's release ('' : not published by Nova)."""
    return uname().release


def version():
    """Return the system's release version ('' : not published by Nova)."""
    return uname().version


def machine():
    """Return the machine type: 'x86_64'."""
    return uname().machine


def processor():
    """Return the processor name: 'x86_64'."""
    return uname().processor


def python_implementation():
    """Return the name of the Python implementation: 'Nova'."""
    name = getattr(getattr(sys, 'implementation', None), 'name', 'nova')
    return {'cpython': 'CPython', 'pypy': 'PyPy', 'nova': 'Nova'}.get(name, name)


def python_version():
    """Return the Python version as 'major.minor.patchlevel'."""
    return '%d.%d.%d' % tuple(sys.version_info[:3])


def python_version_tuple():
    """Return the Python version as a tuple of three strings."""
    return tuple(python_version().split('.'))


def python_branch():
    """Return the source branch the interpreter was built from ('')."""
    return ''


def python_revision():
    """Return the source revision the interpreter was built from ('')."""
    return ''


def python_build():
    """Return (buildno, builddate) parsed from sys.version."""
    match = re.match(r'[^(]*\(([^,)]*)(?:, *([^)]*))?\)', sys.version)
    if match is None:
        return '', ''
    return match.group(1) or '', match.group(2) or ''


def python_compiler():
    """Return the compiler that built the interpreter: 'rustc'."""
    return 'rustc'


_platform_cache = {}


def platform(aliased=False, terse=False):
    """Return one string identifying the platform, e.g.
    'Nova-x86_64-64bit-ELF' (terse: 'Nova')."""
    result = _platform_cache.get((aliased, terse), None)
    if result is not None:
        return result
    system, node, release, version, machine, processor = uname()
    if machine == processor:
        processor = ''
    if aliased:
        system, release, version = system_alias(system, release, version)
    if terse:
        platform = _platform(system, release)
    else:
        bits, linkage = architecture(sys.executable)
        platform = _platform(system, release, machine, processor, bits, linkage)
    _platform_cache[(aliased, terse)] = platform
    return platform


_os_release_candidates = ("/etc/os-release", "/usr/lib/os-release")
_os_release_cache = None


def _parse_os_release(lines):
    info = {
        "NAME": "Linux",
        "ID": "linux",
        "PRETTY_NAME": "Linux",
    }
    os_release_line = re.compile(
        "^(?P<name>[a-zA-Z0-9_]+)=(?P<quote>[\"\']?)(?P<value>.*)(?P=quote)$"
    )
    os_release_unescape = re.compile(r"\\([\\\$\"\'`])")
    for line in lines:
        mo = os_release_line.match(line)
        if mo is not None:
            info[mo.group('name')] = os_release_unescape.sub(
                r"\1", mo.group('value'))
    return info


def freedesktop_os_release():
    """Return the fields of /etc/os-release as a dict; OSError when the
    file does not exist."""
    global _os_release_cache
    if _os_release_cache is None:
        errno = None
        for candidate in _os_release_candidates:
            try:
                with open(candidate, encoding="utf-8") as f:
                    _os_release_cache = _parse_os_release(f)
                break
            except OSError as e:
                errno = e.errno
        else:
            raise OSError(
                errno,
                f"Unable to read files {', '.join(_os_release_candidates)}"
            )
    return _os_release_cache.copy()


if __name__ == '__main__':
    terse = ('terse' in sys.argv or '--terse' in sys.argv)
    aliased = (not 'nonaliased' in sys.argv and not '--nonaliased' in sys.argv)
    print(platform(aliased, terse))
    sys.exit(0)
