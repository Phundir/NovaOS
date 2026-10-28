import hashlib, hmac, zlib, binascii, base64, struct, io, textwrap
for name in ['md5', 'sha1', 'sha224', 'sha256', 'sha384', 'sha512']:
    h = hashlib.new(name)
    h.update(b'Nova ')
    h.update(b'OS' * 100)
    print(name, h.hexdigest(), h.digest_size, h.block_size, hashlib.new(name, b'').hexdigest()[:16])
print(hashlib.sha256(b'abc').hexdigest(), hashlib.md5(b'x' * 1000).hexdigest())
print(hmac.new(b'key', b'The quick brown fox jumps over the lazy dog', 'sha256').hexdigest(), hmac.compare_digest(b'a', b'a'))
print(hashlib.pbkdf2_hmac('sha256', b'password', b'salt', 100).hex())
data = b'hello hello hello hello world ' * 50
c = zlib.compress(data)
print(zlib.decompress(c) == data, len(c) < len(data), zlib.crc32(data), zlib.adler32(data), zlib.crc32(b'abc', 12345))
print(zlib.decompress(zlib.compress(b'', 0)), zlib.decompress(zlib.compress(data, 9, -15), -15) == data, zlib.decompress(zlib.compress(data, wbits=31), 31) == data)
co = zlib.compressobj(); x = co.compress(b'abc' * 100) + co.flush(); print(zlib.decompress(x)[:9])
print(zlib.decompress(bytes.fromhex('789ccb48cdc9c95728cf2fca4901001a0b045d')))
print(binascii.hexlify(b'\x00\xffab'), binascii.unhexlify('00ff6162'), binascii.b2a_base64(b'hello'), binascii.a2b_base64(b'aGVsbG8=\n'), binascii.crc32(b'hello'), binascii.hexlify(b'abcdef', '-', 2))
for s in [b'', b'f', b'fo', b'foo', b'foob', b'fooba', b'foobar']:
    print(base64.b64encode(s), base64.b32encode(s), base64.b16encode(s), base64.b85encode(s), base64.a85encode(s), base64.b32hexencode(s))
print(base64.b64decode('Zm9vYmFy'), base64.urlsafe_b64encode(b'\xfb\xff\xfe'), base64.urlsafe_b64decode(b'-__-'), base64.b32decode('MZXW6YQ='), base64.b85decode(b'W^Zp|VR8'), base64.a85decode(b'AoDTs@<)'))
print(base64.encodebytes(b'x' * 100))
try:
    base64.b64decode('abc')
except binascii.Error as e:
    print('Error', e)
print(struct.pack('<hHiIqQ', -2, 65535, -100000, 4000000000, -2**60, 2**64 - 1).hex(), struct.calcsize('@bhid'), struct.calcsize('=bhid'), struct.calcsize('<5s2xH'))
print(struct.pack('>d', 3.14159).hex(), struct.pack('<f', 0.1).hex(), struct.pack('<e', 1.5).hex(), struct.unpack('<f', struct.pack('<f', 0.1)), struct.unpack('>e', b'\x3c\x00'))
print(struct.unpack('<hHiI', struct.pack('<hHiI', -1, 2, -3, 4)), struct.pack('4s', b'ab'), struct.unpack('3p', b'\x02abX'[:3]), struct.pack('?c', True, b'z'))
print(list(struct.iter_unpack('<H', b'\x01\x00\x02\x00')), struct.Struct('<II').size, struct.Struct('<II').unpack(b'\x01\0\0\0\x02\0\0\0'), struct.pack('@bi', 1, 2))
for bad in [lambda: struct.pack('<h', 70000), lambda: struct.pack('b', 300), lambda: struct.unpack('<i', b'ab'), lambda: struct.pack('<ii', 1), lambda: struct.pack('i', 'x'), lambda: struct.pack('h', 99999)]:
    try:
        bad()
    except struct.error as e:
        print('struct.error', e)
try:
    struct.pack('<f', 1e300)
except OverflowError as e:
    print('OverflowError', e)
s = io.StringIO()
s.write('ligne 1\nligne 2\n')
print('x', file=s)
print(repr(s.getvalue()), s.tell())
s.seek(0)
print(s.readline(), s.read(3), list(s), s.read())
b = io.BytesIO(b'abc\ndef\n')
print(b.read(2), b.readline(), b.readlines(), b.tell(), b.seek(1), b.write(b'XY'), b.getvalue())
with io.StringIO('a\nb\nc') as f:
    print([l for l in f], f.closed)
print(f.closed)
try:
    f.read()
except ValueError as e:
    print(e)
txt = "The quick brown fox jumps over the lazy dog. Pack my box with five dozen liquor jugs! How vexingly quick daft zebras jump; a well-known sentence-ending example-with-hyphens appears here."
print(textwrap.fill(txt, 30))
print(textwrap.wrap(txt, 22, break_on_hyphens=False)[:4])
print(textwrap.shorten(txt, 50), '|', textwrap.shorten('Hello  world!', width=12))
print(textwrap.fill(txt, 25, initial_indent='* ', subsequent_indent='  ', max_lines=3))
print(repr(textwrap.dedent('''
    def f():
        return 1
    ''')), repr(textwrap.indent('a\n\nb\n', '> ')))
print(textwrap.wrap('Supercalifragilisticexpialidocious is long', 10))
