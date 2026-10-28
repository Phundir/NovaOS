values = [0, 1, -1, 3.14159, -0.0, 1e-7, 123456.789, 1e22, float("inf"), float("nan"), 2 ** 40, True]
for spec in ["", ">10", "<8", "^9", "+", " ", ".2f", "08.3f", ",", "_", ".3e", ".4g", "g", "%", ".1%", "e"]:
    row = []
    for v in values:
        try:
            row.append(format(v, spec))
        except (ValueError, TypeError) as err:
            row.append("!" + type(err).__name__)
    print(repr(spec), row)
for v in [255, -255, 0]:
    print([format(v, s) for s in ["x", "X", "#x", "o", "#o", "b", "#b", "08b", "#010x", "c" if v > 0 else "d", "n"]])
print("%d %i %5d %-5d| %05d %+d % d" % (42, -42, 42, 42, 42, 42, 42))
print("%f %.3f %10.2f %-10.2f| %e %.2E %g %G" % (1.5, 2.0 / 3, 3.14159, 3.14159, 12345.678, 0.000123, 0.00001234, 1e20))
print("%s %r %a %c %%" % ("str", "rep", "é", 65), "%(a)s-%(b)03d" % {"a": "x", "b": 7})
print("%x %X %o %#x %#o" % (3054, 3054, 8, 255, 8), "%*d|%-*d|" % (5, 1, 4, 2), "%.*f" % (2, 3.14159))
print("{0:{1}}".format("x", 5) + "|", "{:{}{}}".format(3.14159, ".", "3"), "{!r:>8}".format("a"), "{0[1]} {0[0]}".format(["a", "b"]))
class P:
    x = 3
    def __format__(self, spec):
        return "P<" + spec + ">"
print("{0.x} {1}".format(P, "y"), "{:abc}".format(P()), f"{P():zz}", format(P(), "q"))
name, n = "monde", 3
print(f"{name!r:>10}", f"{n:{'0'}{4}}", f"{'{'}{n}{'}'}", f"{n * 2 = }", f"{name=}", f"{3.0:.0f}", f"{1e6:,.0f}")
print("{:,}".format(1234567.891), "{:_d}".format(10 ** 9), "{:010,.2f}".format(-1234.5), "{:=+10d}".format(42))
for t in ["abc", "a\nb", "it's", 'say "hi"', "\x00\x7f", "tab\t", "unié€", "\U0001f600"]:
    print(repr(t), ascii(t), len(t))
print(repr(b"bytes\x00\xff"), str(b"x"), bytes([65, 66]), b"ab" + b"cd", b"abc"[1], b"abc"[1:], list(b"hi"), bytes(3))
