class Registry(type):
    classes = []
    def __new__(mcls, name, bases, ns, **kw):
        ns['tag'] = name.lower()
        cls = super().__new__(mcls, name, bases, ns)
        mcls.classes.append(name)
        return cls
    def __init__(cls, name, bases, ns, **kw):
        super().__init__(name, bases, ns)
        cls.initialized = True
    def describe(cls):
        return f"<{cls.__name__} tag={cls.tag}>"
    @property
    def size(cls):
        return len(cls.__dict__)

class Base(metaclass=Registry):
    pass

class Child(Base):
    x = 1

print(Registry.classes, Base.tag, Child.tag, Child.initialized, type(Child).__name__, isinstance(Child, Registry), Child.describe())
print(type(Base) is Registry, Child.__class__.__name__)

class Singleton(type):
    _instances = {}
    def __call__(cls, *args, **kwargs):
        if cls not in cls._instances:
            cls._instances[cls] = super().__call__(*args, **kwargs)
        return cls._instances[cls]

class Config(metaclass=Singleton):
    def __init__(self, value):
        print("init", value)
        self.value = value

a = Config(1)
b = Config(2)
print(a is b, a.value)

class Ordered(type):
    @classmethod
    def __prepare__(mcls, name, bases):
        print("prepare", name)
        return {}
    def __new__(mcls, name, bases, ns):
        ns["fields"] = [k for k in ns if not k.startswith("_")]
        return super().__new__(mcls, name, bases, ns)

class Point(metaclass=Ordered):
    x = 0
    y = 0
    def norm(self):
        return 0

print(Point.fields)

class Plugin:
    plugins = []
    def __init_subclass__(cls, name=None, **kwargs):
        super().__init_subclass__(**kwargs)
        cls.plugin_name = name or cls.__name__
        Plugin.plugins.append(cls.plugin_name)

class A(Plugin, name="alpha"):
    pass

class B(Plugin):
    pass

print(Plugin.plugins, A.plugin_name)

Dyn = Registry("Dyn", (), {"v": 3})
print(Dyn.v, Dyn.tag, type(Dyn).__name__, Registry.classes[-1])

class Meta(type):
    def __repr__(cls):
        return f"Meta<{cls.__name__}>"
class Q(metaclass=Meta):
    pass
print(repr(Q), Q)
try:
    class M1(type): pass
    class M2(type): pass
    class X(metaclass=M1): pass
    class Y(metaclass=M2): pass
    class Z(X, Y): pass
except TypeError as e:
    print("TypeError:", e)
