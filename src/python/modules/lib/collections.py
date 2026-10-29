"""collections for Nova's Python: namedtuple, deque, Counter,
defaultdict, OrderedDict, ChainMap -- with CPython's reprs and errors.

namedtuple builds its __new__ with exec, as CPython does, so that a
missing field gives CPython's message ("Point.__new__() missing 1
required positional argument: 'y'").
"""

import sys as _sys
from operator import itemgetter as _itemgetter


_KEYWORDS = frozenset((
    'False', 'None', 'True', 'and', 'as', 'assert', 'async', 'await', 'break', 'class', 'continue',
    'def', 'del', 'elif', 'else', 'except', 'finally', 'for', 'from', 'global', 'if', 'import',
    'in', 'is', 'lambda', 'nonlocal', 'not', 'or', 'pass', 'raise', 'return', 'try', 'while',
    'with', 'yield',
))


def namedtuple(typename, field_names, *, rename=False, defaults=None, module=None):
    if isinstance(field_names, str):
        field_names = field_names.replace(',', ' ').split()
    field_names = list(map(str, field_names))
    typename = str(typename)
    if rename:
        seen = set()
        for index, name in enumerate(field_names):
            if (not name.isidentifier() or name in _KEYWORDS
                    or name.startswith('_') or name in seen):
                field_names[index] = f'_{index}'
            seen.add(name)
    for name in [typename] + field_names:
        if type(name) is not str:
            raise TypeError('Type names and field names must be strings')
        if not name.isidentifier():
            raise ValueError('Type names and field names must be valid '
                             f'identifiers: {name!r}')
        if name in _KEYWORDS:
            raise ValueError('Type names and field names cannot be '
                             f'a keyword: {name!r}')
    seen = set()
    for name in field_names:
        if name.startswith('_') and not rename:
            raise ValueError('Field names cannot start with an underscore: '
                             f'{name!r}')
        if name in seen:
            raise ValueError(f'Encountered duplicate field name: {name!r}')
        seen.add(name)

    field_defaults = {}
    if defaults is not None:
        defaults = tuple(defaults)
        if len(defaults) > len(field_names):
            raise TypeError('Got more default values than field names')
        field_defaults = dict(reversed(list(zip(reversed(field_names),
                                                reversed(defaults)))))

    field_names = tuple(field_names)
    num_fields = len(field_names)
    arg_list = ', '.join(field_names)
    if num_fields == 1:
        arg_list += ','
    repr_fmt = '(' + ', '.join(f'{name}=%r' for name in field_names) + ')'
    tuple_new = tuple.__new__

    namespace = {
        '_tuple_new': tuple_new,
        '__builtins__': {},
        '__name__': f'namedtuple_{typename}',
    }
    params = []
    first_default = num_fields - len(defaults or ())
    for i, name in enumerate(field_names):
        if i >= first_default:
            params.append(f'{name}=_d{i}')
            namespace[f'_d{i}'] = defaults[i - first_default]
        else:
            params.append(name)
    code = f'def __new__(_cls, {", ".join(params)}): return _tuple_new(_cls, ({arg_list}))'
    exec(code, namespace)
    __new__ = namespace['__new__']
    __new__.__qualname__ = f'{typename}.__new__'
    __new__.__doc__ = f'Create new instance of {typename}({arg_list})'

    @classmethod
    def _make(cls, iterable):
        result = tuple_new(cls, iterable)
        if len(result) != num_fields:
            raise TypeError(f'Expected {num_fields} arguments, got {len(result)}')
        return result

    def _replace(self, /, **kwds):
        result = self._make(map(kwds.pop, field_names, self))
        if kwds:
            raise TypeError(f'Got unexpected field names: {list(kwds)!r}')
        return result

    def __repr__(self):
        return self.__class__.__name__ + repr_fmt % tuple(self)

    def _asdict(self):
        return dict(zip(self._fields, self))

    def __getnewargs__(self):
        return tuple(self)

    class_namespace = {
        '__doc__': f'{typename}({arg_list})',
        '__slots__': (),
        '_fields': field_names,
        '_field_defaults': field_defaults,
        '__new__': staticmethod(__new__),
        '_make': _make,
        '_replace': _replace,
        '__repr__': __repr__,
        '_asdict': _asdict,
        '__getnewargs__': __getnewargs__,
        '__match_args__': field_names,
    }
    for index, name in enumerate(field_names):
        class_namespace[name] = property(_itemgetter(index))

    if module is None:
        try:
            module = _sys._getframe(1).f_globals.get('__name__', '__main__')
        except (AttributeError, ValueError):
            module = '__main__'
    class_namespace['__module__'] = module
    class_namespace['__qualname__'] = typename
    result = type(typename, (tuple,), class_namespace)
    return result


class deque:
    __class_getitem__ = classmethod(type(list[int]))

    """A list with a moving head: append and popleft in constant time."""

    def __init__(self, iterable=(), maxlen=None):
        if maxlen is not None:
            if not isinstance(maxlen, int):
                raise TypeError('an integer is required')
            if maxlen < 0:
                raise ValueError('maxlen must be non-negative')
        self._items = []
        self._head = 0
        self._maxlen = maxlen
        self.extend(iterable)

    @property
    def maxlen(self):
        return self._maxlen

    def _compact(self):
        if self._head > 32 and self._head * 2 > len(self._items):
            del self._items[:self._head]
            self._head = 0

    def __len__(self):
        return len(self._items) - self._head

    def __bool__(self):
        return len(self._items) > self._head

    def append(self, x):
        self._items.append(x)
        if self._maxlen is not None and len(self) > self._maxlen:
            self.popleft()

    def appendleft(self, x):
        if self._head > 0:
            self._head -= 1
            self._items[self._head] = x
        else:
            self._items.insert(0, x)
        if self._maxlen is not None and len(self) > self._maxlen:
            self.pop()

    def pop(self):
        if not self:
            raise IndexError('pop from an empty deque')
        x = self._items.pop()
        if len(self._items) == self._head:
            self._items = []
            self._head = 0
        return x

    def popleft(self):
        if not self:
            raise IndexError('pop from an empty deque')
        x = self._items[self._head]
        self._items[self._head] = None
        self._head += 1
        if self._head == len(self._items):
            self._items = []
            self._head = 0
        else:
            self._compact()
        return x

    def extend(self, iterable):
        if iterable is self:
            iterable = list(iterable)
        for x in iterable:
            self.append(x)

    def extendleft(self, iterable):
        if iterable is self:
            iterable = list(iterable)
        for x in iterable:
            self.appendleft(x)

    def clear(self):
        self._items = []
        self._head = 0

    def copy(self):
        return deque(self, self._maxlen)

    __copy__ = copy

    def count(self, x):
        return sum(1 for v in self if v is x or v == x)

    def index(self, x, start=0, stop=None):
        items = list(self)
        if stop is None:
            stop = len(items)
        for i in range(start if start >= 0 else max(0, len(items) + start), min(stop, len(items))):
            if items[i] is x or items[i] == x:
                return i
        raise ValueError(f'{x!r} is not in deque')

    def insert(self, i, x):
        if self._maxlen is not None and len(self) >= self._maxlen:
            raise IndexError('deque already at its maximum size')
        items = list(self)
        items.insert(i, x)
        self._items = items
        self._head = 0

    def remove(self, value):
        items = list(self)
        for i, v in enumerate(items):
            if v is value or v == value:
                del items[i]
                self._items = items
                self._head = 0
                return
        raise ValueError(f'{value!r} is not in deque')

    def reverse(self):
        items = list(self)
        items.reverse()
        self._items = items
        self._head = 0

    def rotate(self, n=1):
        length = len(self)
        if length <= 1:
            return
        n %= length
        if n:
            items = list(self)
            self._items = items[-n:] + items[:-n]
            self._head = 0

    def __iter__(self):
        items = self._items
        i = self._head
        while i < len(self._items):
            yield self._items[i]
            i += 1

    def __reversed__(self):
        return iter(list(self)[::-1])

    def __getitem__(self, index):
        if isinstance(index, slice):
            raise TypeError('sequence index must be integer, not \'slice\'')
        length = len(self)
        if index < 0:
            index += length
        if not 0 <= index < length:
            raise IndexError('deque index out of range')
        return self._items[self._head + index]

    def __setitem__(self, index, value):
        length = len(self)
        if index < 0:
            index += length
        if not 0 <= index < length:
            raise IndexError('deque index out of range')
        self._items[self._head + index] = value

    def __delitem__(self, index):
        length = len(self)
        if index < 0:
            index += length
        if not 0 <= index < length:
            raise IndexError('deque index out of range')
        del self._items[self._head + index]

    def __contains__(self, x):
        for v in self:
            if v is x or v == x:
                return True
        return False

    def __eq__(self, other):
        if not isinstance(other, deque):
            return NotImplemented
        return list(self) == list(other)

    def __lt__(self, other):
        if not isinstance(other, deque):
            return NotImplemented
        return list(self) < list(other)

    def __le__(self, other):
        if not isinstance(other, deque):
            return NotImplemented
        return list(self) <= list(other)

    def __gt__(self, other):
        if not isinstance(other, deque):
            return NotImplemented
        return list(self) > list(other)

    def __ge__(self, other):
        if not isinstance(other, deque):
            return NotImplemented
        return list(self) >= list(other)

    __hash__ = None

    def __add__(self, other):
        if not isinstance(other, deque):
            raise TypeError(f'can only concatenate deque (not "{type(other).__name__}") to deque')
        result = self.copy()
        result.extend(other)
        return result

    def __iadd__(self, other):
        self.extend(other)
        return self

    def __mul__(self, n):
        result = deque((), self._maxlen)
        for _ in range(n):
            result.extend(self)
        return result

    def __repr__(self):
        if self._maxlen is None:
            return f'deque({list(self)!r})'
        return f'deque({list(self)!r}, maxlen={self._maxlen})'


class defaultdict(dict):
    def __init__(self, default_factory=None, *args, **kwargs):
        if default_factory is not None and not callable(default_factory):
            raise TypeError('first argument must be callable or None')
        super().__init__(*args, **kwargs)
        self.default_factory = default_factory

    def __missing__(self, key):
        if self.default_factory is None:
            raise KeyError(key)
        value = self.default_factory()
        self[key] = value
        return value

    def __repr__(self):
        factory = repr(self.default_factory) if self.default_factory is not None else 'None'
        return f'defaultdict({factory}, {dict.__repr__(self)})'

    def copy(self):
        return defaultdict(self.default_factory, self)

    __copy__ = copy


class OrderedDict(dict):
    def move_to_end(self, key, last=True):
        value = self.pop(key)
        if last:
            self[key] = value
        else:
            items = list(self.items())
            self.clear()
            self[key] = value
            for k, v in items:
                self[k] = v

    def popitem(self, last=True):
        if not self:
            raise KeyError('dictionary is empty')
        key = next(reversed(self)) if last else next(iter(self))
        value = self.pop(key)
        return key, value

    def __repr__(self):
        if not self:
            return f'{self.__class__.__name__}()'
        return f'{self.__class__.__name__}({dict.__repr__(self)})'

    def __eq__(self, other):
        if isinstance(other, OrderedDict):
            return dict.__eq__(self, other) and list(self) == list(other)
        return dict.__eq__(self, other)

    def copy(self):
        return self.__class__(self)


class Counter(dict):
    def __init__(self, iterable=None, /, **kwds):
        super().__init__()
        self.update(iterable, **kwds)

    def __missing__(self, key):
        return 0

    def total(self):
        return sum(self.values())

    def most_common(self, n=None):
        if n is None:
            return sorted(self.items(), key=_itemgetter(1), reverse=True)
        import heapq
        return heapq.nlargest(n, self.items(), key=_itemgetter(1))

    def elements(self):
        for elem, count in self.items():
            for _ in range(count):
                yield elem

    @classmethod
    def fromkeys(cls, iterable, v=None):
        raise NotImplementedError(
            'Counter.fromkeys() is undefined.  Use Counter(iterable) instead.')

    def update(self, iterable=None, /, **kwds):
        if iterable is not None:
            if isinstance(iterable, dict):
                if self:
                    self_get = self.get
                    for elem, count in iterable.items():
                        self[elem] = count + self_get(elem, 0)
                else:
                    dict.update(self, iterable)
            else:
                self_get = self.get
                for elem in iterable:
                    self[elem] = self_get(elem, 0) + 1
        if kwds:
            self.update(kwds)

    def subtract(self, iterable=None, /, **kwds):
        if iterable is not None:
            self_get = self.get
            if isinstance(iterable, dict):
                for elem, count in iterable.items():
                    self[elem] = self_get(elem, 0) - count
            else:
                for elem in iterable:
                    self[elem] = self_get(elem, 0) - 1
        if kwds:
            self.subtract(kwds)

    def copy(self):
        return self.__class__(self)

    def __delitem__(self, elem):
        if elem in self:
            super().__delitem__(elem)

    def __repr__(self):
        if not self:
            return f'{self.__class__.__name__}()'
        try:
            d = dict(self.most_common())
        except TypeError:
            d = dict(self)
        return f'{self.__class__.__name__}({d!r})'

    def __eq__(self, other):
        if not isinstance(other, Counter):
            return NotImplemented
        return all(self[e] == other[e] for c in (self, other) for e in c)

    def __ne__(self, other):
        if not isinstance(other, Counter):
            return NotImplemented
        return not self == other

    def __le__(self, other):
        if not isinstance(other, Counter):
            return NotImplemented
        return all(self[e] <= other[e] for c in (self, other) for e in c)

    def __lt__(self, other):
        if not isinstance(other, Counter):
            return NotImplemented
        return self <= other and self != other

    def __ge__(self, other):
        if not isinstance(other, Counter):
            return NotImplemented
        return all(self[e] >= other[e] for c in (self, other) for e in c)

    def __gt__(self, other):
        if not isinstance(other, Counter):
            return NotImplemented
        return self >= other and self != other

    def __add__(self, other):
        if not isinstance(other, Counter):
            return NotImplemented
        result = Counter()
        for elem, count in self.items():
            newcount = count + other[elem]
            if newcount > 0:
                result[elem] = newcount
        for elem, count in other.items():
            if elem not in self and count > 0:
                result[elem] = count
        return result

    def __sub__(self, other):
        if not isinstance(other, Counter):
            return NotImplemented
        result = Counter()
        for elem, count in self.items():
            newcount = count - other[elem]
            if newcount > 0:
                result[elem] = newcount
        for elem, count in other.items():
            if elem not in self and count < 0:
                result[elem] = 0 - count
        return result

    def __or__(self, other):
        if not isinstance(other, Counter):
            return NotImplemented
        result = Counter()
        for elem, count in self.items():
            other_count = other[elem]
            newcount = other_count if count < other_count else count
            if newcount > 0:
                result[elem] = newcount
        for elem, count in other.items():
            if elem not in self and count > 0:
                result[elem] = count
        return result

    def __and__(self, other):
        if not isinstance(other, Counter):
            return NotImplemented
        result = Counter()
        for elem, count in self.items():
            other_count = other[elem]
            newcount = count if count < other_count else other_count
            if newcount > 0:
                result[elem] = newcount
        return result

    def __pos__(self):
        result = Counter()
        for elem, count in self.items():
            if count > 0:
                result[elem] = count
        return result

    def __neg__(self):
        result = Counter()
        for elem, count in self.items():
            if count < 0:
                result[elem] = 0 - count
        return result

    def _keep_positive(self):
        nonpositive = [elem for elem, count in self.items() if not count > 0]
        for elem in nonpositive:
            del self[elem]
        return self

    def __iadd__(self, other):
        for elem, count in other.items():
            self[elem] += count
        return self._keep_positive()

    def __isub__(self, other):
        for elem, count in other.items():
            self[elem] -= count
        return self._keep_positive()

    def __ior__(self, other):
        for elem, other_count in other.items():
            count = self[elem]
            if other_count > count:
                self[elem] = other_count
        return self._keep_positive()

    def __iand__(self, other):
        for elem, count in self.items():
            other_count = other[elem]
            if other_count < count:
                self[elem] = other_count
        return self._keep_positive()


class ChainMap:
    __class_getitem__ = classmethod(type(list[int]))

    def __init__(self, *maps):
        self.maps = list(maps) or [{}]

    def __missing__(self, key):
        raise KeyError(key)

    def __getitem__(self, key):
        for mapping in self.maps:
            if key in mapping:
                return mapping[key]
        return self.__missing__(key)

    def get(self, key, default=None):
        return self[key] if key in self else default

    def __len__(self):
        return len(set().union(*self.maps))

    def __iter__(self):
        d = {}
        for mapping in reversed(self.maps):
            d.update(dict.fromkeys(mapping))
        return iter(d)

    def __contains__(self, key):
        return any(key in m for m in self.maps)

    def __bool__(self):
        return any(self.maps)

    def __repr__(self):
        return f'{self.__class__.__name__}({", ".join(map(repr, self.maps))})'

    def keys(self):
        return list(self)

    def items(self):
        return [(k, self[k]) for k in self]

    def values(self):
        return [self[k] for k in self]

    def new_child(self, m=None):
        if m is None:
            m = {}
        return self.__class__(m, *self.maps)

    @property
    def parents(self):
        return self.__class__(*self.maps[1:])

    def __setitem__(self, key, value):
        self.maps[0][key] = value

    def __delitem__(self, key):
        try:
            del self.maps[0][key]
        except KeyError:
            raise KeyError(f'Key not found in the first mapping: {key!r}')


class UserDict(dict):
    pass


class UserList(list):
    pass


class UserString(str):
    pass
