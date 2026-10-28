import heapq, bisect, string, copy, math, json, re, random, sys, time
from contextlib import contextmanager, suppress
from abc import ABC, abstractmethod
from typing import List, Dict, Optional, NamedTuple
h = [5, 1, 8, 3, 9, 2]
heapq.heapify(h); print(h)
heapq.heappush(h, 0); print([heapq.heappop(h) for _ in range(3)], h)
print(heapq.nlargest(3, [1, 8, 3, 9, 2]), heapq.nsmallest(2, [5, 1, 8]), list(heapq.merge([1, 4, 7], [2, 5, 8])))
print(heapq.nlargest(2, ["aa", "b", "ccc"], key=len))
a = [1, 3, 3, 5]
print(bisect.bisect_left(a, 3), bisect.bisect_right(a, 3), bisect.bisect(a, 4)); bisect.insort(a, 4); print(a)
print(string.ascii_lowercase, string.digits, string.capwords("hello wide world"), string.Template("$x vaut ${y}").substitute(x="a", y=1))
orig = {"l": [1, [2, 3]], "d": {"k": "v"}}
sh = copy.copy(orig); dp = copy.deepcopy(orig)
orig["l"][1].append(4)
print(sh["l"], dp["l"], sh is orig, sh["l"] is orig["l"], dp["d"] == orig["d"])
@contextmanager
def tag(name):
    print(f"<{name}>")
    yield name.upper()
    print(f"</{name}>")
with tag("p") as t:
    print("dedans", t)
with suppress(ZeroDivisionError):
    1 / 0
print("supprime")
class Shape(ABC):
    @abstractmethod
    def area(self): ...
class Square(Shape):
    def __init__(self, s): self.s = s
    def area(self): return self.s ** 2
print(Square(3).area())
try:
    Shape()
except TypeError as e:
    print("TypeError:", e)
class Emp(NamedTuple):
    name: str
    salary: int = 1000
e = Emp("Ada"); print(e, e.salary, e._replace(salary=5), isinstance(e, tuple))
def total(xs: List[int], d: Optional[Dict[str, int]] = None) -> int:
    return sum(xs)
print(total([1, 2]), Optional[int], int | None, isinstance(3, int | str), isinstance(3.0, int | str))
print(json.dumps({"a": [1, 2.5, None, True], "b": "é"}), json.dumps([1, {"x": "y"}], indent=2), json.dumps({"b": 1, "a": 2}, sort_keys=True))
print(json.loads('{"x": [1, 2.0, "s", null, false], "y": {"z": -1e3}}'), json.dumps("tèst\n", ensure_ascii=False))
try:
    json.loads("{bad")
except json.JSONDecodeError as err:
    print("JSONDecodeError:", err)
print(re.findall(r"\d+", "a1b22c333"), re.sub(r"(\w+)@(\w+)", r"\2 chez \1", "ada@lovelace"), re.split(r"[,;]\s*", "a, b;c"))
m = re.match(r"(?P<key>\w+)=(?P<val>\d+)", "age=42 ans")
print(m.group(0), m.group("key"), m.groups(), m.groupdict(), m.span(2), m.start(), m.end(), bool(re.search("z", "abc")))
print(re.sub("x*", "-", "abxd"), re.split(r"\b", "a b"), re.fullmatch(r"\d{3}", "123") is not None, re.compile(r"[aeiou]", re.I).findall("AbcE"))
print([m.group() for m in re.finditer(r"\w+", "un deux trois")], re.subn("a", "b", "banana"), re.escape("a.b*c"))
random.seed(42)
print(random.random(), random.randint(1, 100), random.choice("abcdef"), random.sample(range(100), 5), random.uniform(1, 2))
l = list(range(10)); random.shuffle(l); print(l, random.randrange(0, 100, 7), random.getrandbits(40))
random.seed(2024); print([random.randint(1, 6) for _ in range(10)], random.choices("abc", weights=[5, 1, 1], k=5))
r = random.Random(7); print(r.random(), r.randint(1, 10))
print(math.pi, math.e, math.sqrt(2), math.floor(-2.5), math.ceil(2.1), math.fsum([0.1] * 10), math.hypot(3, 4), math.log(100, 10), math.log10(1000), math.log2(1024))
print(math.sin(math.pi / 6), math.cos(0), math.atan2(1, 1), math.degrees(math.pi), math.isclose(0.1 + 0.2, 0.3), math.inf > 1e308, math.isnan(math.nan), math.trunc(-3.7), math.copysign(1, -0.0))
print(sys.argv[0].endswith("19_modules_divers.py"), sys.version_info[0], sys.maxsize, type(time.time()).__name__, sys.getrecursionlimit())
