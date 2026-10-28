import os, glob, fnmatch
from pathlib import Path, PurePosixPath, PurePath
p = PurePosixPath('/usr/local/lib/python3.13/site.tar.gz')
print(p, repr(p), p.parts, p.name, p.suffix, p.suffixes, p.stem, p.parent, p.anchor, p.is_absolute())
print(list(p.parents)[:3], p.parents[0], len(p.parents), p.with_name('x.py'), p.with_suffix('.bz2'), p.with_stem('archive'))
print(p.relative_to('/usr'), p.is_relative_to('/etc'), PurePosixPath('a/b') / 'c' / PurePosixPath('d'), 'x' / PurePosixPath('y'), PurePath('a//b/./c'))
print(PurePosixPath('a/b.py').match('*.py'), PurePosixPath('/a/b/c.py').match('b/*.py'), PurePosixPath('/a/b/c.py').match('/*.py'), PurePosixPath('.hidden').suffix, PurePosixPath('x.').suffixes)
print(sorted([PurePosixPath('b'), PurePosixPath('a/c'), PurePosixPath('a')]), PurePosixPath('a') == PurePosixPath('a/'), hash(PurePosixPath('a')) == hash(PurePosixPath('a')), PurePosixPath('a/b').relative_to('c', walk_up=True))
try:
    p.relative_to('/etc')
except ValueError as e:
    print(e)
root = Path('_tmp_pathlib')
(root / 'sub' / 'deep').mkdir(parents=True, exist_ok=True)
(root / 'a.txt').write_text('alpha\nbeta\n')
(root / 'b.py').write_text('print(1)')
(root / 'sub' / 'c.py').write_bytes(b'\x00\x01')
(root / 'sub' / 'deep' / 'd.txt').touch()
print(root.exists(), root.is_dir(), (root / 'a.txt').is_file(), (root / 'a.txt').read_text().splitlines(), (root / 'sub/c.py').read_bytes(), (root / 'a.txt').stat().st_size)
print(sorted(x.name for x in root.iterdir()), sorted(str(x) for x in root.glob('*.py')), sorted(str(x) for x in root.rglob('*.py')), sorted(str(x) for x in root.glob('**/*.txt')))
print(sorted(glob.glob('_tmp_pathlib/*')), sorted(glob.glob('_tmp_pathlib/**/*.txt', recursive=True)), sorted(glob.glob('_tmp_pathlib/s*/*.py')), glob.escape('a[b]*'))
print(fnmatch.fnmatch('foo.py', '*.py'), fnmatch.filter(['a.c', 'b.h', 'c.c'], '*.c'), fnmatch.translate('*.[ch]'), fnmatch.fnmatchcase('X', '[!a-z]'))
print(sorted(e.name for e in os.scandir('_tmp_pathlib')), os.path.relpath('_tmp_pathlib/sub/c.py', '_tmp_pathlib/sub/deep'), os.path.commonpath(['/a/b/c', '/a/b/d']), os.path.commonprefix(['interstellar', 'internet']))
for dirpath, dirs, files in sorted(os.walk('_tmp_pathlib')):
    print(dirpath, sorted(dirs), sorted(files))
print([(str(a), sorted(b), sorted(c)) for a, b, c in sorted(root.walk(), key=lambda t: str(t[0]))])
q = (root / 'a.txt').rename(root / 'renamed.txt')
print(q, q.exists(), (root / 'a.txt').exists(), os.fspath(q), str(Path('x/../y').resolve()).endswith('/y'), Path('a') / 'b')
with q.open() as f:
    print(f.readline().strip())
for f in sorted(root.rglob('*'), key=lambda x: -len(str(x))):
    if f.is_file():
        f.unlink()
    else:
        f.rmdir()
root.rmdir()
print(root.exists(), Path('nope.txt').exists())
(root / 'missing').unlink(missing_ok=True) if False else None
try:
    Path('_nope/x').read_text()
except FileNotFoundError as e:
    print('FileNotFoundError', e.errno)
import re
print(re.findall(r'\w+', 'Héllo wörld, naïve café ٣٤ 東京'), re.findall(r'\d', 'a1٣²߂'), re.split(r'\s+', 'a　b\xa0c\td'))
print(re.findall(r'\b\w', 'élan vital über'), re.sub(r'(?i:straße)|x', '_', 'STRASSE straße X x'), re.findall(r'a(?s:.)b|c.d', 'a\nb c\nd'), re.findall(r'[\w-]+', 'été-2024 ok'))
print(re.findall(r'\W+', 'mot—mot·mot'), re.match(r'(?i:[a-c]+)z', 'ABcz').group(), re.findall(r'(?x: a b ) c', 'abc ab c'), re.findall(r'\w+', 'abc', re.ASCII), re.findall(r'\w', 'é', re.A))
