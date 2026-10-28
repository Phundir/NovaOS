b = b"Hello World"
print(b.upper(), b.split(), b.replace(b"o", b"0"), b.find(b"W"), b.startswith(b"He"), b[1], b[1:4], list(b[:3]), b.strip(b"Hd"), b"-".join([b"a", b"b"]), b.count(b"l"), b.hex(), bytes.fromhex("4142"), b.decode())
print(b.lower(), b.title(), b.swapcase(), b.isalpha(), b"123".isdigit(), b"  x y  ".split(), b"  x  ".strip(), b"a,b,,c".split(b","), b.partition(b" "), b.center(15, b"*"), b.find(111), b.index(b"l"), b.hex(":"))
ba = bytearray(b"abc")
ba.append(100)
ba.extend(b"ef")
ba.extend([103, 104])
ba[0] = 65
ba[1:3] = b"XYZ"
print(ba, len(ba), ba[0], ba[1:4], bytes(ba), ba.upper(), ba.decode(), list(ba)[:3])
del ba[0]
del ba[-2:]
print(ba, ba.pop(), ba, b"X" in ba, 89 in ba, ba == bytearray(b"XYZde"), ba == b"XYZd")
ba += b"!!"
ba.insert(0, 33)
ba.reverse()
print(ba, ba * 2, bytearray(3), bytearray("hé", "utf-8"), bytearray([1, 2, 255]), ba.copy() is not ba, ba.split(b"!"))
try:
    hash(ba)
except TypeError as e:
    print(e)
try:
    ba.append(256)
except ValueError as e:
    print(e)
print(bytes("é", "latin-1"), "é".encode(), b"\xc3\xa9".decode(), b"\xff".decode("latin-1"), b"ab\xffc".decode("utf-8", "replace"), b"ab\xffc".decode("ascii", "ignore"))
try:
    b"\xff".decode()
except UnicodeDecodeError as e:
    print(type(e).__name__, e)
t = bytes.maketrans(b"abc", b"xyz")
print(b"aabbcc".translate(t), b"hello".translate(None, b"l"), sorted([b"b", b"a"]), b"ab" < b"b", bytearray(b"z") > b"a")
print(repr(bytearray()), str(bytearray(b"x")), isinstance(ba, bytearray), type(ba).__name__)
