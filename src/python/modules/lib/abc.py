"""abc for Nova's Python: abstract classes without metaclasses.

ABC computes its subclasses' abstract methods when they are created
(__init_subclass__) and refuses to instantiate one that still has some,
with CPython's message.
"""


def abstractmethod(funcobj):
    funcobj.__isabstractmethod__ = True
    return funcobj


class abstractproperty(property):
    __isabstractmethod__ = True


def _is_abstract(value):
    if getattr(value, '__isabstractmethod__', False):
        return True
    if isinstance(value, property):
        return getattr(value.fget, '__isabstractmethod__', False)
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


class ABC:
    __slots__ = ()

    def __init_subclass__(cls, **kwargs):
        super().__init_subclass__(**kwargs)
        _compute_abstracts(cls)

    def __new__(cls, *args, **kwargs):
        abstracts = getattr(cls, '__abstractmethods__', ())
        if abstracts:
            names = sorted(abstracts)
            s = 's' if len(names) > 1 else ''
            listed = ', '.join(f"'{n}'" for n in names)
            raise TypeError(f"Can't instantiate abstract class {cls.__name__} without an implementation for abstract method{s} {listed}")
        return object.__new__(cls)


ABCMeta = type


def update_abstractmethods(cls):
    _compute_abstracts(cls)
    return cls
