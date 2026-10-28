class Animal:
    count = 0
    def __init__(self, name):
        self.name = name
        Animal.count += 1
    def speak(self):
        return f"{self.name} fait {self.sound()}"
    def sound(self):
        return "..."
    def __repr__(self):
        return f"{type(self).__name__}({self.name!r})"
class Dog(Animal):
    def sound(self):
        return "ouaf"
class Puppy(Dog):
    def __init__(self, name, age):
        super().__init__(name)
        self.age = age
    def speak(self):
        return super().speak() + " (doucement)"
p = Puppy("Rex", 1)
print(p.speak(), p, Animal.count, isinstance(p, Animal), issubclass(Puppy, Dog), Puppy.__mro__)
print(type(p).__name__, p.__class__.__name__, p.__dict__, hasattr(p, "age"), getattr(p, "zz", "def"))
class Vec:
    def __init__(self, x, y): self.x, self.y = x, y
    def __add__(self, o): return Vec(self.x + o.x, self.y + o.y)
    def __mul__(self, k): return Vec(self.x * k, self.y * k)
    __rmul__ = __mul__
    def __eq__(self, o): return isinstance(o, Vec) and (self.x, self.y) == (o.x, o.y)
    def __hash__(self): return hash((self.x, self.y))
    def __lt__(self, o): return (self.x, self.y) < (o.x, o.y)
    def __len__(self): return 2
    def __getitem__(self, i): return (self.x, self.y)[i]
    def __iter__(self): yield self.x; yield self.y
    def __repr__(self): return f"Vec({self.x}, {self.y})"
    def __bool__(self): return bool(self.x or self.y)
    def __neg__(self): return Vec(-self.x, -self.y)
    def __contains__(self, v): return v in (self.x, self.y)
    def __call__(self, z): return self.x * z
v = Vec(1, 2)
print(v + Vec(3, 4), v * 3, 2 * v, v == Vec(1, 2), v != Vec(0, 0), len(v), v[1], list(v), -v, bool(Vec(0, 0)), 2 in v, v(10))
print(sorted([Vec(2, 1), Vec(1, 5), Vec(1, 2)]), {Vec(1, 2), Vec(1, 2)}, max([Vec(0, 9), Vec(1, 0)]), x if (x := v) else None)
class Temp:
    def __init__(self): self._c = 0
    @property
    def celsius(self): return self._c
    @celsius.setter
    def celsius(self, v):
        if v < -273.15: raise ValueError("trop froid")
        self._c = v
    @property
    def fahrenheit(self): return self._c * 9 / 5 + 32
    @staticmethod
    def info(): return "statique"
    @classmethod
    def make(cls, c):
        t = cls(); t.celsius = c; return t
t = Temp.make(100)
print(t.celsius, t.fahrenheit, Temp.info(), t.info())
try:
    t.celsius = -300
except ValueError as e:
    print("ValueError:", e)
try:
    t.fahrenheit = 3
except AttributeError as e:
    print("AttributeError:", e)
class Base:
    def __init_subclass__(cls, **kw):
        super().__init_subclass__()
        cls.registered = True
class Child(Base): pass
print(Child.registered)
class A:
    def who(self): return "A"
class B(A):
    def who(self): return "B" + super().who()
class C(A):
    def who(self): return "C" + super().who()
class D(B, C):
    def who(self): return "D" + super().who()
print(D().who(), [k.__name__ for k in D.__mro__])
class NoEq: pass
print(NoEq() == NoEq(), len({NoEq(), NoEq()}))
class Eq:
    def __eq__(self, o): return True
try:
    hash(Eq())
except TypeError as e:
    print("TypeError:", e)
class Ctx:
    def __enter__(self): print("entree"); return self
    def __exit__(self, t, v, tb): print("sortie", t.__name__ if t else None); return True
with Ctx() as c:
    raise ValueError("avale")
print("apres with")
class Counter:
    def __init__(self, n): self.i, self.n = 0, n
    def __iter__(self): return self
    def __next__(self):
        if self.i >= self.n: raise StopIteration
        self.i += 1
        return self.i
print(list(Counter(4)), sum(Counter(10)))
class Desc:
    def __set_name__(self, owner, name): self.name = "_" + name
    def __get__(self, obj, objtype=None): return "desc:" + str(getattr(obj, self.name, None)) if obj is not None else self
    def __set__(self, obj, v): setattr(obj, self.name, v * 2)
class UsesDesc:
    attr = Desc()
u = UsesDesc(); u.attr = 21; print(u.attr, UsesDesc.attr.name)
class Slotted:
    def __init__(self, **kw): self.__dict__.update(kw)
s = Slotted(a=1, b=2); print(s.a, s.b, vars(s))
class MyList(list):
    def total(self): return sum(self)
ml = MyList([1, 2, 3]); ml.append(4)
print(ml, ml.total(), len(ml), ml[0], isinstance(ml, list), type(ml).__name__)
class MyDict(dict):
    def __missing__(self, key): return key.upper()
md = MyDict(a=1); print(md["a"], md["zz"], md)
class MyInt(int):
    def __repr__(self): return f"MyInt({int(self)})"
mi = MyInt(5); print(mi, mi + 1, mi * 2, mi == 5, str(mi))
print(type(5), type("s"), type(None), type(len), type(Vec), Vec, Vec.__name__, Vec.__qualname__)
