x = "global"
def f():
    print(x)
f()
def g():
    x = "local"
    def h():
        return x
    return h()
print(g())
def counter_class():
    class C:
        n = 0
        def inc(self):
            C.n += 1
            return C.n
    return C
C = counter_class(); c = C(); c.inc(); print(c.inc())
class K:
    a = 1
    b = [a * i for i in range(3)] if False else None
    def m(self):
        return a if False else K.a
print(K().m(), K.b)
try:
    def bad():
        print(y)
        y = 1
    bad()
except UnboundLocalError as e:
    print("UnboundLocalError:", e)
try:
    print(not_defined)
except NameError as e:
    print("NameError:", e)
def outer():
    v = 1
    def middle():
        def inner():
            nonlocal v
            v += 10
            return v
        return inner()
    middle()
    return v
print(outer())
funcs = [lambda: i for i in range(3)]
print([fn() for fn in funcs])
i = "fuite?"
[i for i in range(3)]
print(i)
print([(a, b) for a in range(3) for b in range(a)])
print({k: [v for v in range(k)] for k in range(3)})
total = 0
for i in range(5):
    for j in range(5):
        if j > i:
            break
        total += j
    else:
        total += 100
print(total)
n = 0
while n < 5:
    n += 1
    if n == 10:
        break
else:
    print("while sans break", n)
for i in []:
    pass
else:
    print("for vide: else")
del total
try:
    total
except NameError as e:
    print(e)
a = b = c = []
a.append(1); print(b, c, a is c)
x, y = 1, 2; x, y = y, x; print(x, y)
lst = [1, 2, 3]; i = 0
i, lst[i] = 1, 9; print(lst)
class Attr:
    pass
o = Attr(); o.x = 1; o.y = o; print(o.y.y.x, hasattr(o, "z"))
setattr(o, "z", 3); print(o.z); delattr(o, "z"); print(hasattr(o, "z"))
print(globals()["x"], "f" in globals(), sorted(k for k in locals() if len(k) == 1))
def uses_locals(p, q=2):
    r = p + q
    return sorted(locals().items())
print(uses_locals(1))
print(eval("1 + 2 * 3"), eval("x + y", {"x": 1, "y": 2}), eval("[i * i for i in range(4)]"))
ns = {}
exec("def sq(n):\n    return n * n\nres = sq(7)", ns)
print(ns["res"], ns["sq"](3))
exec("zz = 5")
print(zz)
