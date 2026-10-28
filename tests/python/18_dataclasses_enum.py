from dataclasses import dataclass, field, asdict, astuple, fields, replace, FrozenInstanceError
from enum import Enum, IntEnum, auto, Flag, StrEnum, unique
@dataclass
class Point:
    x: int
    y: int = 0
@dataclass(order=True, frozen=True)
class Version:
    major: int
    minor: int = 0
    tags: tuple = ()
@dataclass
class Inventory:
    items: list = field(default_factory=list)
    count: int = field(default=0, repr=False)
    def add(self, item):
        self.items.append(item)
        self.count += 1
p = Point(1, 2)
print(p, Point(3), p == Point(1, 2), p != Point(2, 1), asdict(p), astuple(p), [f.name for f in fields(p)])
print(replace(p, y=9), Point.__init__.__qualname__, hash(Version(1)) == hash(Version(1)))
inv = Inventory(); inv.add("pomme"); inv.add("poire"); print(inv, inv.count, Inventory())
print(sorted([Version(2), Version(1, 5), Version(1, 2)]), Version(1) < Version(1, 1))
try:
    Version(1).major = 2
except FrozenInstanceError as e:
    print("FrozenInstanceError:", e)
try:
    Point()
except TypeError as e:
    print("TypeError:", e)
try:
    hash(p)
except TypeError as e:
    print("TypeError:", e)
@dataclass
class Post:
    a: int
    b: int = 2
    def __post_init__(self):
        self.total = self.a + self.b
print(Post(1).total, Post(1, 5))
class Color(Enum):
    RED = 1
    GREEN = 2
    BLUE = 3
print(Color.RED, repr(Color.GREEN), Color(3), Color["RED"], Color.RED.name, Color.RED.value, list(Color), len(Color))
print(Color.RED in Color, Color.RED == Color.RED, Color.RED is Color(1), isinstance(Color.BLUE, Color), [c.name for c in Color])
try:
    Color(7)
except ValueError as e:
    print("ValueError:", e)
class Prio(IntEnum):
    LOW = 1
    HIGH = 10
print(Prio.LOW < Prio.HIGH, Prio.HIGH + 1, sorted([Prio.HIGH, Prio.LOW]), int(Prio.HIGH), Prio.LOW == 1, str(Prio.LOW), f"{Prio.HIGH}")
class Auto(Enum):
    A = auto()
    B = auto()
    C = auto()
print([(m.name, m.value) for m in Auto])
class Perm(Flag):
    R = 4
    W = 2
    X = 1
rw = Perm.R | Perm.W
print(Perm.R in rw, Perm.X in rw, bool(rw & Perm.X), rw.value)
class Coul(StrEnum):
    ROUGE = auto()
    VERT = "vert"
print(Coul.ROUGE, Coul.VERT == "vert", list(Coul))
class Alias(Enum):
    A = 1
    B = 1
print(list(Alias), Alias.B is Alias.A)
try:
    unique(Alias)
except ValueError as e:
    print("ValueError:", e)
