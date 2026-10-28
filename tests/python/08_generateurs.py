def countdown(n):
    while n > 0:
        yield n
        n -= 1
    return "fini"
print(list(countdown(5)))
g = countdown(2)
print(next(g), next(g))
try:
    next(g)
except StopIteration as e:
    print("StopIteration", e.value)
def echo():
    received = yield "pret"
    while True:
        received = yield f"recu {received}"
e = echo(); print(next(e), e.send(1), e.send("deux"))
def inner():
    x = yield 1
    yield x * 10
    return "valeur"
def outer():
    r = yield from inner()
    yield r
o = outer(); print(next(o), o.send(5), next(o))
def gen_throw():
    try:
        yield 1
    except ValueError as err:
        yield f"attrape {err}"
gt = gen_throw(); print(next(gt), gt.throw(ValueError("x")))
def closing():
    try:
        yield 1
        yield 2
    finally:
        print("nettoyage")
c = closing(); next(c); c.close(); print("ferme")
squares = (x * x for x in range(6))
print(sum(squares), list(squares))
print(next(iter([]), "defaut"), list(zip(countdown(3), "abc")))
def fib():
    a, b = 0, 1
    while True:
        yield a
        a, b = b, a + b
f = fib(); print([next(f) for _ in range(12)])
def tree(n):
    if n:
        yield from tree(n - 1)
        yield n
        yield from tree(n - 1)
print(list(tree(3)))
it = iter([1, 2, 3]); print(next(it), list(it), list(it))
print(list(enumerate(x for x in "ab")), dict((k, len(k)) for k in ["aa", "b"]))
gen = (i for i in range(3)); print(type(gen).__name__, iter(gen) is gen)
def lazy():
    print("demarre")
    yield 1
l = lazy(); print("cree"); print(next(l))
print(sorted(x % 4 for x in range(10)), max(x * (5 - x) for x in range(6)))
