import itertools as it
import functools as ft
import operator as op
print(list(it.islice(it.count(5, 2), 4)), list(it.islice(it.cycle("ab"), 5)), list(it.repeat("x", 3)), repr(it.count(3)))
print(list(it.chain([1, 2], (3,), "ab")), list(it.chain.from_iterable([[1], [2, 3]])), list(it.accumulate([1, 2, 3, 4])), list(it.accumulate([3, 1, 4], max)))
print(list(it.product("ab", [1, 2])), len(list(it.product(range(3), repeat=3))))
print(list(it.permutations("abc", 2)), list(it.combinations(range(4), 2)), list(it.combinations_with_replacement("ab", 2)))
print([(k, list(g)) for k, g in it.groupby("aaabbcaa")], [(k, len(list(g))) for k, g in it.groupby(sorted([3, 1, 3, 2, 1]))])
print(list(it.takewhile(lambda x: x < 3, [1, 2, 3, 1])), list(it.dropwhile(lambda x: x < 3, [1, 2, 3, 1])), list(it.filterfalse(None, [0, 1, 0, 2])))
print(list(it.zip_longest("abc", [1], fillvalue="-")), list(it.starmap(pow, [(2, 3), (3, 2)])), list(it.compress("abcd", [1, 0, 1, 0])))
print(list(it.pairwise([1, 2, 3, 4])), list(it.batched(range(7), 3)), list(it.islice(range(10), 2, 8, 2)))
a, b = it.tee([1, 2, 3]); print(list(a), list(b))
print(ft.reduce(op.add, [1, 2, 3, 4]), ft.reduce(lambda x, y: x * y, range(1, 6), 10))
@ft.lru_cache(maxsize=None)
def fib(n):
    return n if n < 2 else fib(n - 1) + fib(n - 2)
print(fib(200), fib.cache_info())
@ft.lru_cache(maxsize=2)
def sq(n):
    return n * n
for v in [1, 2, 1, 3, 2]:
    sq(v)
print(sq.cache_info())
@ft.cache
def deep(n):
    return 0 if n == 0 else 1 + deep(n - 1)
print(deep(900))
p = ft.partial(int, base=2); print(p("101"), p.func, p.keywords)
def greet(greeting, name, punct="!"):
    return f"{greeting} {name}{punct}"
hello = ft.partial(greet, "Bonjour"); print(hello("Ada"), hello("Bob", punct="?"))
@ft.total_ordering
class V:
    def __init__(self, v): self.v = v
    def __eq__(self, o): return self.v == o.v
    def __lt__(self, o): return self.v < o.v
print(V(1) <= V(2), V(3) >= V(2), V(2) > V(1), sorted([V(3), V(1)], key=lambda x: x.v)[0].v)
print(sorted([3, 1, 2], key=ft.cmp_to_key(lambda a, b: b - a)))
def deco(f):
    @ft.wraps(f)
    def wrapper(*a):
        return f(*a)
    return wrapper
@deco
def documented():
    """doc"""
print(documented.__name__, documented.__doc__, documented.__wrapped__.__name__)
class Circle:
    def __init__(self, r): self.r = r
    @ft.cached_property
    def area(self):
        print("calcul")
        return 3 * self.r ** 2
c = Circle(2); print(c.area, c.area)
print(op.itemgetter(1)("abc"), op.itemgetter(0, 2)("abc"), op.attrgetter("r")(c), op.methodcaller("upper")("x"), op.add(2, 3), op.mul("a", 3))
print(sorted([("b", 2), ("a", 2), ("c", 1)], key=op.itemgetter(1, 0)), list(map(op.neg, [1, -2])))
@ft.singledispatch
def show(x): return "objet"
@show.register(int)
def _(x): return "entier"
@show.register(list)
def _(x): return "liste"
print(show(1), show([]), show("s"), show(True))
