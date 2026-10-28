def f(a, b=2, *args, c, d=4, **kw):
    return (a, b, args, c, d, kw)
print(f(1, c=3), f(1, 2, 3, 4, c=5, e=6), f(*[1, 2], **{"c": 9}))
def defaults(x, acc=[]):
    acc.append(x)
    return acc
print(defaults(1), defaults(2))
def counter():
    count = 0
    def inc():
        nonlocal count
        count += 1
        return count
    return inc
c = counter(); c(); c(); print(c())
g = 10
def read_g():
    return g
def write_g():
    global g
    g = 20
write_g(); print(read_g())
square = lambda x: x * x
print(square(7), (lambda *a, **k: (a, k))(1, z=2), sorted([3, 1, 2], key=lambda v: -v))
def deco(fn):
    def wrapper(*a, **k):
        print("avant", fn.__name__)
        r = fn(*a, **k)
        print("apres")
        return r
    return wrapper
@deco
def hello(name):
    print("bonjour", name)
    return 42
print(hello("Ada"))
def make_adders():
    return [lambda x, i=i: x + i for i in range(3)]
print([add(10) for add in make_adders()])
def fib(n):
    return n if n < 2 else fib(n - 1) + fib(n - 2)
print([fib(i) for i in range(15)])
def kwonly(*, a, b=1): return a + b
print(kwonly(a=5), kwonly(b=2, a=1))
def posonly(a, b, /, c): return a, b, c
print(posonly(1, 2, c=3))
for bad in (lambda: f(), lambda: f(1, 2), lambda: kwonly(1), lambda: posonly(a=1, b=2, c=3), lambda: hello(1, 2)):
    try:
        bad()
    except TypeError as e:
        print("TypeError:", e)
def doc():
    """Une docstring."""
print(doc.__doc__, doc.__name__, square.__name__, fib.__qualname__)
def outer():
    def inner():
        return "inner"
    return inner
print(outer()(), outer().__qualname__)
if (n := len("walrus")) > 3:
    print("walrus", n)
print([y for x in range(5) if (y := x * 2) > 4])
def gen_closure():
    xs = []
    for i in range(3):
        xs.append(lambda: i)
    return [x() for x in xs]
print(gen_closure())
def recursive(n):
    return 0 if n == 0 else 1 + recursive(n - 1)
print(recursive(900))
