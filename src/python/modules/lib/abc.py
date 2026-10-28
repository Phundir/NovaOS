"""abc for Nova's Python: abstract base classes, with a real ABCMeta.

ABCMeta computes a class's abstract methods when it is created and
keeps a registry of virtual subclasses (`register`). isinstance and
issubclass reach it through the metaclass's `__instancecheck__` and
`__subclasscheck__`, as in CPython; `__subclasshook__` lets an ABC
recognize a class by its structure (collections.abc uses it). Refusing to
instantiate a class whose `__abstractmethods__` is not empty is done by
object.__new__ itself, so it holds for every way of creating an instance.
"""


def abstractmethod(funcobj):
    funcobj.__isabstractmethod__ = True
    return funcobj


class abstractclassmethod(classmethod):
    __isabstractmethod__ = True

    def __init__(self, callable):
        callable.__isabstractmethod__ = True
        super().__init__(callable)


class abstractstaticmethod(staticmethod):
    __isabstractmethod__ = True

    def __init__(self, callable):
        callable.__isabstractmethod__ = True
        super().__init__(callable)


class abstractproperty(property):
    __isabstractmethod__ = True


def _is_abstract(value):
    if getattr(value, '__isabstractmethod__', False):
        return True
    if isinstance(value, property):
        return (getattr(value.fget, '__isabstractmethod__', False)
                or getattr(value.fset, '__isabstractmethod__', False)
                or getattr(value.fdel, '__isabstractmethod__', False))
    if isinstance(value, (staticmethod, classmethod)):
        return getattr(value.__func__, '__isabstractmethod__', False)
    return False


def _compute_abstracts(cls):
    abstracts = set()
    for name, value in cls.__dict__.items():
        if _is_abstract(value):
            abstracts.add(name)
    for base in cls.__bases__:
        for name in getattr(base, '__abstractmethods__', ()):
            value = getattr(cls, name, None)
            if _is_abstract(value):
                abstracts.add(name)
    cls.__abstractmethods__ = frozenset(abstracts)


class ABCMeta(type):
    def __new__(mcls, name, bases, namespace, /, **kwargs):
        cls = super().__new__(mcls, name, bases, namespace, **kwargs)
        _compute_abstracts(cls)
        cls._abc_registry = []
        cls._abc_cache = []
        cls._abc_negative_cache = []
        return cls

    def register(cls, subclass):
        if not isinstance(subclass, type):
            raise TypeError("Can only register classes")
        if issubclass(subclass, cls):
            return subclass
        if issubclass(cls, subclass):
            raise RuntimeError("Refusing to create an inheritance cycle")
        cls._abc_registry.append(subclass)
        cls._abc_negative_cache.clear()
        return subclass

    def __instancecheck__(cls, instance):
        subclass = instance.__class__
        if subclass in cls._abc_cache:
            return True
        subtype = type(instance)
        if subtype is subclass:
            return cls.__subclasscheck__(subclass)
        return any(cls.__subclasscheck__(c) for c in (subclass, subtype))

    def __subclasscheck__(cls, subclass):
        if not isinstance(subclass, type):
            raise TypeError('issubclass() arg 1 must be a class')
        if subclass in cls._abc_cache:
            return True
        if subclass in cls._abc_negative_cache:
            return False
        ok = cls.__subclasshook__(subclass)
        if ok is not NotImplemented:
            if ok:
                cls._abc_cache.append(subclass)
            else:
                cls._abc_negative_cache.append(subclass)
            return ok
        if cls in getattr(subclass, '__mro__', ()):
            cls._abc_cache.append(subclass)
            return True
        for rcls in cls._abc_registry:
            if issubclass(subclass, rcls):
                cls._abc_cache.append(subclass)
                return True
        for scls in cls.__subclasses__():
            if issubclass(subclass, scls):
                cls._abc_cache.append(subclass)
                return True
        cls._abc_negative_cache.append(subclass)
        return False

    def _abc_registry_clear(cls):
        cls._abc_registry.clear()

    def _abc_caches_clear(cls):
        cls._abc_cache.clear()
        cls._abc_negative_cache.clear()


class ABC(metaclass=ABCMeta):
    __slots__ = ()

    @classmethod
    def __subclasshook__(cls, subclass):
        return NotImplemented


def get_cache_token():
    return 0


def update_abstractmethods(cls):
    if not hasattr(cls, '__abstractmethods__'):
        return cls
    _compute_abstracts(cls)
    return cls
