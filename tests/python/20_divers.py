print("a,b,c,d".split(",", 2), "a,b,c,d".rsplit(",", 2), " a  b ".split(" "), "".split(), "".split(","), "abc".split("b"))
print("hello".center(11, "-"), "x".zfill(3), "tEsT".swapcase(), "hello world".title(), "they're bill's".title())
print("abc".find(""), "abc".count(""), "aaa".count("aa"), "abcabc".rfind("b", 0, 4), "abc".index("c", -1), "abc".startswith("bc", 1))
print([1, 2, 3, 4, 5][::-2], [1, 2, 3, 4, 5][-2::-1], [1, 2, 3, 4, 5][1:-1:2], "python"[::-1], (1, 2, 3)[::-1], list(range(10))[8:2:-3])
print(list(range(5, -5, -3)), list(reversed(range(1, 10, 2))), range(10)[::-1], range(1, 10)[2:5], list(range(0)))
print(sorted([("b", 1), ("a", 1), ("c", 0)], key=lambda t: t[1]), sorted("BaAb", key=str.lower), sorted([3, 1, 2], reverse=True))
print(max([], default="vide"), min("bca"), max({"a": 3, "b": 1}, key=lambda k: -ord(k)), min(3, 1, 2, key=lambda v: (v - 2) ** 2))
print(sum([[1], [2]], []), sum((0.5, 0.25)), sum(range(5), 10), any(()), all((1, 1)), list(map(lambda a, b: a * b, [1, 2], [3, 4, 5])))
print(list(zip()), list(zip([1])), dict(enumerate("ab", start=5)), list(enumerate([])), [i for i, _ in zip(range(10), "abc")])
try:
    list(zip([1, 2], [1], strict=True))
except ValueError as e:
    print("ValueError:", e)
print(divmod(-7, 2), divmod(7, -2), -7 // 2, 7 // -2, -7 % 2, 7 % -2, divmod(-7.5, 2), round(-1.5), round(2.5), round(-3.5))
print(pow(-2, 3), pow(2, -2), pow(-8, 1/3) if False else "-", 0 ** 0, 0.0 ** 0, (-1) ** 0.5 if False else "-", 10 ** -3)
print(str(1e100), str(1e-100), str(123.0), str(1e16), str(-1e-5), str(2 ** 0.5), 1e300 * 10, -1e300 * 1e10, 1e-320, 1e-330)
print(float("1e5"), float("-0"), float(" 12 "), float("1_0"), int("-0"), int("+7"), int("1_000"), int("0b101", 0), int("0O17", 0))
for bad in ["1e5", "", " ", "1.5", "0x", "abc", "1__0"]:
    try:
        int(bad)
    except ValueError as e:
        print("ValueError:", e)
for bad in ["", "abc", "1e", "--1", "1.2.3"]:
    try:
        float(bad)
    except ValueError as e:
        print("ValueError:", e)
print(isinstance(1, (str, int)), isinstance(True, bool), issubclass(bool, int), type(1) is int, type(True).__name__, callable(print), callable(1))
print(id(None) == id(None), (lambda: 0).__name__, len.__name__, print.__name__ if hasattr(print, "__name__") else "-")
print(repr(None), repr(...), repr(NotImplemented), str(True), repr(int), repr(len), type(...).__name__)
print(chr(0x1F600), ord("é"), "\x41B\U00000043", "a\\b", r"a\nb", len("\t"))
print(bytes("abc", "utf-8"), "abc".encode("ascii"), b"%d" if False else b"x", bytes.fromhex("4142") if False else "-", list(bytes(range(3))))
a = [3, 1, 2]
b = a
a += [4]
print(a, b, a is b)
t = (1, 2)
u = t
t += (3,)
print(t, u, t is u)
s = "ab"
s *= 3
print(s)
nums = [1, 2, 3]
nums[1:2] = []
print(nums)
nums[:] = range(5)
print(nums)
nums[::2] = ["x", "y", "z"]
print(nums)
try:
    nums[::2] = [1]
except ValueError as e:
    print("ValueError:", e)
d = {1: "a"}
d[True] = "b"
print(d, {1.0: "x", 1: "y"}, len({0, 0.0, False, -0.0}))
matrix = [[1, 2], [3, 4]]
copy_rows = [row[:] for row in matrix]
copy_rows[0][0] = 99
print(matrix, copy_rows, list(map(list, zip(*matrix))))
print("%s" % [1, 2], "%s" % (1,), "%s %s" % ("a", "b"), "%%", "%.2s" % "abc", "%5s|%-5s|" % ("r", "l"))
x = 10
def change():
    x = 20
    return x
print(change(), x)
print(f"{'nested ' + f'{x * 2}'}", f"{x!s:>5}", f"{x:#b}", f"{3.14159:.3}", f"{1234.5:,.1f}", f"{-0.0:.1f}", f"{float('nan'):f}")
class Base:
    def __repr__(self): return f"{self.__class__.__name__}()"
class Derived(Base): pass
print([Base(), Derived()], str(Derived()), f"{Derived()}", "%s %r" % (Base(), Derived()))
print(int.__name__, list.__qualname__, Base.__module__, Derived.__bases__, Derived.__mro__[-1])
print(sorted({3: 1, 1: 2}), sorted({3, 1, 2}), sorted("cba"), sorted((3, 2)), sorted([], reverse=True))
print(list(filter(lambda c: c.isupper(), "aBcD")), list(map(int, "123")), "".join(map(str.upper, "ab")))
g = (x for x in range(3))
print(list(g), list(g), sum(x for x in []), min((x for x in [3, 1]), default=0))
