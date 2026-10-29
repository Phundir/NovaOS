"""typing for Nova's Python: what annotations name, at run time.

Annotations are evaluated in modules and class bodies (CPython 3.13), so
every name here exists and can be subscripted; type checking itself is
not done. NamedTuple and TypedDict work as classes.
"""

TYPE_CHECKING = False


class _Special:
    def __init__(self, name, args=()):
        self._name = name
        self.__args__ = args

    def __getitem__(self, params):
        if not isinstance(params, tuple):
            params = (params,)
        return _Special(self._name, params)

    def __call__(self, *args, **kwargs):
        raise TypeError(f"Cannot instantiate typing.{self._name}")

    def __or__(self, other):
        return Union[self, other]

    def __ror__(self, other):
        return Union[other, self]

    def __repr__(self):
        if not self.__args__:
            return f'typing.{self._name}'
        return f'typing.{self._name}[{", ".join(_type_repr(a) for a in self.__args__)}]'

    def __eq__(self, other):
        return isinstance(other, _Special) and self._name == other._name and self.__args__ == other.__args__

    def __hash__(self):
        return hash((self._name, self.__args__))

    def __mro_entries__(self, bases):
        return (Generic,)


def _type_repr(obj):
    if isinstance(obj, type):
        if obj.__module__ == 'builtins':
            return obj.__qualname__
        return f'{obj.__module__}.{obj.__qualname__}'
    if obj is ...:
        return '...'
    return repr(obj)


Any = _Special('Any')
Union = _Special('Union')
Optional = _Special('Optional')
List = _Special('List')
Dict = _Special('Dict')
Set = _Special('Set')
FrozenSet = _Special('FrozenSet')
Tuple = _Special('Tuple')
Type = _Special('Type')
Callable = _Special('Callable')
Iterable = _Special('Iterable')
Iterator = _Special('Iterator')
Generator = _Special('Generator')
Sequence = _Special('Sequence')
MutableSequence = _Special('MutableSequence')
Mapping = _Special('Mapping')
MutableMapping = _Special('MutableMapping')
Collection = _Special('Collection')
Container = _Special('Container')
Hashable = _Special('Hashable')
Sized = _Special('Sized')
Reversible = _Special('Reversible')
AbstractSet = _Special('AbstractSet')
MutableSet = _Special('MutableSet')
Deque = _Special('Deque')
DefaultDict = _Special('DefaultDict')
OrderedDict = _Special('OrderedDict')
Counter = _Special('Counter')
ChainMap = _Special('ChainMap')
Awaitable = _Special('Awaitable')
Coroutine = _Special('Coroutine')
AsyncIterator = _Special('AsyncIterator')
AsyncIterable = _Special('AsyncIterable')
AsyncGenerator = _Special('AsyncGenerator')
ContextManager = _Special('ContextManager')
Pattern = _Special('Pattern')
Match = _Special('Match')
Literal = _Special('Literal')
Final = _Special('Final')
ClassVar = _Special('ClassVar')
Annotated = _Special('Annotated')
NoReturn = _Special('NoReturn')
Never = _Special('Never')
Self = _Special('Self')
LiteralString = _Special('LiteralString')
TypeAlias = _Special('TypeAlias')
TypeGuard = _Special('TypeGuard')
Required = _Special('Required')
NotRequired = _Special('NotRequired')
Unpack = _Special('Unpack')
Concatenate = _Special('Concatenate')
SupportsInt = _Special('SupportsInt')
SupportsFloat = _Special('SupportsFloat')
SupportsAbs = _Special('SupportsAbs')
SupportsIndex = _Special('SupportsIndex')
Text = str
AnyStr = _Special('AnyStr')


class TypeVar:
    def __init__(self, name, *constraints, bound=None, covariant=False, contravariant=False, infer_variance=False, default=None):
        self.__name__ = name
        self.__constraints__ = constraints
        self.__bound__ = bound
        self.__covariant__ = covariant
        self.__contravariant__ = contravariant

    def __repr__(self):
        prefix = '+' if self.__covariant__ else '-' if self.__contravariant__ else '~'
        return prefix + self.__name__

    def __or__(self, other):
        return Union[self, other]


class ParamSpec(TypeVar):
    args = None
    kwargs = None


class TypeVarTuple(TypeVar):
    pass


class Generic:
    def __class_getitem__(cls, params):
        return cls


class Protocol(Generic):
    pass


def runtime_checkable(cls):
    return cls


def cast(typ, val):
    return val


def overload(func):
    return func


def final(f):
    return f


def override(method):
    return method


def no_type_check(arg):
    return arg


def get_type_hints(obj, globalns=None, localns=None, include_extras=False):
    hints = {}
    if isinstance(obj, type):
        for base in reversed(obj.__mro__):
            hints.update(getattr(base, '__annotations__', {}))
        return hints
    return dict(getattr(obj, '__annotations__', {}))


def get_origin(tp):
    return getattr(tp, '__origin__', None)


def get_args(tp):
    return getattr(tp, '__args__', ())


def reveal_type(obj):
    print(f'Runtime type is {type(obj).__name__!r}')
    return obj


def assert_never(arg):
    raise AssertionError(f'Expected code to be unreachable, but got: {arg!r}')


def NewType(name, tp):
    def new_type(x):
        return x
    new_type.__name__ = name
    new_type.__qualname__ = name
    new_type.__supertype__ = tp
    return new_type


class NamedTuple(tuple):
    """class P(NamedTuple): x: int; y: int = 0 -- a namedtuple."""

    def __init_subclass__(cls, **kwargs):
        from collections import namedtuple
        annotations = cls.__dict__.get('__annotations__', {})
        fields = list(annotations)
        defaults = []
        for name in fields:
            if name in cls.__dict__:
                defaults.append(cls.__dict__[name])
            elif defaults:
                raise TypeError(f"Non-default namedtuple field {name} cannot follow default field{'s' if len(defaults) > 1 else ''} {', '.join(fields[:len(defaults)])}")
        base = namedtuple(cls.__name__, fields, defaults=defaults or None, module=cls.__module__)
        for name in ('__new__', '_make', '_replace', '__repr__', '_asdict', '__getnewargs__', '_fields', '_field_defaults', '__match_args__'):
            setattr(cls, name, base.__dict__[name])
        for index, name in enumerate(fields):
            setattr(cls, name, base.__dict__[name])
        cls.__annotations__ = annotations


class TypedDict(dict):
    """class Movie(TypedDict): ... -- instances are plain dicts."""

    def __new__(cls, *args, **kwargs):
        return dict(*args, **kwargs)


from _builtins_extra import TypeAliasType
