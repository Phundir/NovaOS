from collections import namedtuple, deque, Counter, defaultdict, OrderedDict, ChainMap
Point = namedtuple("Point", "x y")
p = Point(1, y=2)
print(p, p.x, p[1], p._asdict(), p._replace(x=9), Point._fields, Point._make([3, 4]), tuple(p), p == (1, 2))
x, y = p; print(x + y, len(p), isinstance(p, tuple), hash(p) == hash((1, 2)), sorted([Point(2, 1), Point(1, 5)]))
P3 = namedtuple("P3", ["a", "b", "c"], defaults=[0, 1])
print(P3(5), P3(1, 2, 3), P3.__name__, P3._field_defaults)
try:
    Point(1)
except TypeError as e:
    print("TypeError:", e)
try:
    namedtuple("Bad", "a a")
except ValueError as e:
    print("ValueError:", e)
d = deque([1, 2, 3])
d.append(4); d.appendleft(0); d.extend([5, 6]); d.extendleft([-1, -2])
print(d, len(d), d[0], d[-1], d.pop(), d.popleft(), d)
d.rotate(2); print(d); d.rotate(-3); print(d)
print(list(reversed(d)), 3 in d, d.count(3), d.index(4))
dm = deque(range(10), maxlen=3); print(dm, dm.maxlen); dm.append(10); print(dm)
q = deque()
for i in range(1000):
    q.append(i)
s = 0
while q:
    s += q.popleft()
print(s, q)
try:
    q.pop()
except IndexError as e:
    print("IndexError:", e)
c = Counter("abracadabra")
print(c, c["a"], c["z"], c.most_common(2), sorted(c.elements())[:6], c.total())
c.update("aaz"); c.subtract({"b": 1}); print(c)
c2 = Counter(a=3, b=1)
print(c2 + Counter(a=1, c=2), c2 - Counter(a=5), c2 | Counter(b=4), c2 & Counter(a=2), Counter(["x", "y", "x"]))
print(Counter("hello world").most_common(3), Counter() == Counter(), list(Counter("cab")))
dd = defaultdict(list)
for k, v in [("a", 1), ("b", 2), ("a", 3)]:
    dd[k].append(v)
print(dd, dd["a"], dd["zz"], len(dd), dict(dd))
di = defaultdict(int)
for ch in "mississippi":
    di[ch] += 1
print(sorted(di.items()), di)
od = OrderedDict()
od["b"] = 1; od["a"] = 2; od["c"] = 3
od.move_to_end("b"); print(od, list(od.keys()))
od.move_to_end("c", last=False); print(od, od.popitem(), od.popitem(last=False), od)
cm = ChainMap({"a": 1}, {"a": 2, "b": 3})
print(cm["a"], cm["b"], len(cm), sorted(cm), "b" in cm, cm.new_child({"c": 4})["c"])
