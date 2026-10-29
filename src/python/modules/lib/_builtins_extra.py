"""Built-in names of Nova's Python written in Python, loaded the first
time a program uses one: exception groups (PEP 654) and the helpers of
except*, memoryview, and the runtime of the `type` statement.
"""


def _matcher(condition):
    if isinstance(condition, type) and issubclass(condition, BaseException):
        return lambda e: isinstance(e, condition)
    if isinstance(condition, tuple) and all(isinstance(c, type) and issubclass(c, BaseException) for c in condition):
        return lambda e: isinstance(e, condition)
    if callable(condition) and not isinstance(condition, type):
        return condition
    raise TypeError("expected an exception type, a tuple of exception types, or a callable (other than a class)")


class BaseExceptionGroup(BaseException):
    def __new__(cls, message, exceptions, /):
        if not isinstance(message, str):
            raise TypeError("argument 1 must be str, not %s" % type(message).__name__)
        arguments = exceptions
        try:
            exceptions = tuple(exceptions)
        except TypeError:
            raise TypeError("second argument (exceptions) must be a sequence")
        if not exceptions:
            raise ValueError("second argument (exceptions) must be a non-empty sequence")
        for i, e in enumerate(exceptions):
            if not isinstance(e, BaseException):
                raise ValueError("Item %d of second argument (exceptions) is not an exception" % i)
        if cls is BaseExceptionGroup:
            if all(isinstance(e, Exception) for e in exceptions):
                cls = ExceptionGroup
        elif issubclass(cls, Exception):
            for e in exceptions:
                if not isinstance(e, Exception):
                    if cls is ExceptionGroup:
                        raise TypeError("Cannot nest BaseExceptions in an ExceptionGroup")
                    raise TypeError("Cannot nest BaseExceptions in '%s'" % cls.__name__)
        self = BaseException.__new__(cls, message, exceptions)
        self._message = message
        self._exceptions = exceptions
        self._given = arguments
        return self

    def __init__(self, message, exceptions, /):
        BaseException.__init__(self, message, self._given)

    @property
    def message(self):
        return self._message

    @property
    def exceptions(self):
        return self._exceptions

    def __str__(self):
        n = len(self._exceptions)
        return '%s (%d sub-exception%s)' % (self._message, n, '' if n == 1 else 's')

    def __repr__(self):
        return '%s(%r, %r)' % (type(self).__name__, self._message, self._given)

    def derive(self, excs):
        return BaseExceptionGroup(self._message, excs)

    def _copy_meta(self, other):
        other.__traceback__ = self.__traceback__
        other.__cause__ = self.__cause__
        other.__context__ = self.__context__
        if hasattr(self, '__notes__'):
            other.__notes__ = list(self.__notes__)
        return other

    def split(self, condition):
        match = _matcher(condition)
        if match(self):
            return self, None
        matched = []
        rest = []
        for e in self._exceptions:
            if isinstance(e, BaseExceptionGroup):
                m, r = e.split(condition)
                if m is not None:
                    matched.append(m)
                if r is not None:
                    rest.append(r)
            elif match(e):
                matched.append(e)
            else:
                rest.append(e)
        if not rest and all(a is b for a, b in zip(matched, self._exceptions)) and len(matched) == len(self._exceptions):
            return self, None
        if not matched and all(a is b for a, b in zip(rest, self._exceptions)) and len(rest) == len(self._exceptions):
            return None, self
        m = self._copy_meta(self.derive(matched)) if matched else None
        r = self._copy_meta(self.derive(rest)) if rest else None
        return m, r

    def subgroup(self, condition):
        return self.split(condition)[0]

    def __class_getitem__(cls, item):
        return cls


class ExceptionGroup(BaseExceptionGroup, Exception):
    pass


def _leaves(e):
    if isinstance(e, BaseExceptionGroup):
        out = []
        for x in e.exceptions:
            out.extend(_leaves(x))
        return out
    return [e]


def _nova_excstar_prepare(e):
    if isinstance(e, BaseExceptionGroup):
        return e
    group = BaseExceptionGroup('', (e,))
    group.__traceback__ = e.__traceback__
    return group


def _nova_excstar_split(group, condition):
    if group is None:
        return None, None
    if isinstance(condition, type) and issubclass(condition, BaseExceptionGroup) or \
            isinstance(condition, tuple) and any(isinstance(c, type) and issubclass(c, BaseExceptionGroup) for c in condition):
        raise TypeError("catching ExceptionGroup with except* is not allowed. Use except instead.")
    return group.split(condition)


def _nova_excstar_reraise(original, rest, raised, matches):
    reraised = []
    new = []
    for r in raised:
        if any(r is m for m in matches):
            reraised.append(r)
        else:
            new.append(r)
    if not isinstance(original, BaseExceptionGroup):
        # A naked exception: one clause at most ran.
        if new:
            raise new[0]
        if reraised or rest is not None:
            raise original
        return None
    keep = set()
    if rest is not None:
        keep.update(id(x) for x in _leaves(rest))
    for r in reraised:
        keep.update(id(x) for x in _leaves(r))
    unhandled = None
    if keep:
        unhandled = original.subgroup(lambda e: not isinstance(e, BaseExceptionGroup) and id(e) in keep)
    if not new:
        if unhandled is not None:
            raise unhandled
        return None
    if unhandled is not None:
        new.append(unhandled)
    raise BaseExceptionGroup('', tuple(new))


class memoryview:
    """A view of the bytes of a bytes or bytearray object (format 'B')."""

    __nova_type_name__ = 'memoryview'

    def __init__(self, obj):
        if isinstance(obj, memoryview):
            self._obj, self._start, self._stop, self._step = obj._obj, obj._start, obj._stop, obj._step
            self._readonly = obj._readonly
        elif isinstance(obj, (bytes, bytearray)):
            self._obj = obj
            self._start, self._stop, self._step = 0, len(obj), 1
            self._readonly = isinstance(obj, bytes)
        else:
            raise TypeError("memoryview: a bytes-like object is required, not '%s'" % type(obj).__name__)
        self._released = False

    def _check(self):
        if self._released:
            raise ValueError('operation forbidden on released memoryview object')

    def _range(self):
        return range(self._start, self._stop, self._step)

    @property
    def obj(self):
        return self._obj

    @property
    def readonly(self):
        return self._readonly

    @property
    def nbytes(self):
        return len(self)

    format = 'B'
    itemsize = 1
    ndim = 1

    @property
    def shape(self):
        return (len(self),)

    @property
    def strides(self):
        return (self._step,)

    @property
    def contiguous(self):
        return self._step == 1

    c_contiguous = contiguous
    f_contiguous = contiguous

    def __len__(self):
        self._check()
        return len(self._range())

    def __getitem__(self, key):
        self._check()
        r = self._range()
        if isinstance(key, slice):
            sub = r[key]
            view = memoryview(self)
            view._start, view._stop, view._step = sub.start, sub.stop, sub.step
            return view
        if not isinstance(key, int):
            raise TypeError('memoryview: invalid slice key')
        try:
            return self._obj[r[key]]
        except IndexError:
            raise IndexError('index out of bounds on dimension 1')

    def __setitem__(self, key, value):
        self._check()
        if self._readonly:
            raise TypeError('cannot modify read-only memory')
        r = self._range()
        if isinstance(key, slice):
            sub = r[key]
            value = bytes(value)
            if len(value) != len(sub):
                raise ValueError('memoryview assignment: lvalue and rvalue have different structures')
            for i, b in zip(sub, value):
                self._obj[i] = b
            return
        self._obj[r[key]] = value

    def __iter__(self):
        self._check()
        for i in self._range():
            yield self._obj[i]

    def __eq__(self, other):
        if isinstance(other, memoryview):
            return self.tobytes() == other.tobytes()
        if isinstance(other, (bytes, bytearray)):
            return self.tobytes() == bytes(other)
        return NotImplemented

    __hash__ = None

    def __bytes__(self):
        return self.tobytes()

    def tobytes(self, order='C'):
        self._check()
        return bytes(self._obj[i] for i in self._range())

    def tolist(self):
        self._check()
        return [self._obj[i] for i in self._range()]

    def hex(self, *args):
        return self.tobytes().hex(*args)

    def toreadonly(self):
        view = memoryview(self)
        view._readonly = True
        return view

    def cast(self, format, shape=None):
        if format not in ('B', 'b', 'c'):
            raise ValueError("memoryview: destination format must be a native single character format prefixed with an optional '@'")
        return memoryview(self)

    def release(self):
        self._released = True

    def __enter__(self):
        return self

    def __exit__(self, *args):
        self.release()

    def __repr__(self):
        return '<memory at %s>' % hex(id(self))


class TypeAliasType:
    __nova_type_name__ = 'typing.TypeAliasType'

    def __init__(self, name, value, *, type_params=()):
        self.__name__ = name
        self.__qualname__ = name
        self.__type_params__ = tuple(type_params)
        self.__module__ = '__main__'
        self._evaluate = value if callable(value) else (lambda: value)

    @property
    def __value__(self):
        return self._evaluate()

    def __repr__(self):
        return self.__name__

    def __getitem__(self, params):
        return self


def _nova_type_alias(name, thunk):
    return TypeAliasType(name, thunk)


def _nova_typevar(name, kind):
    """The object a type parameter `T` (kind 0), `*Ts` (1) or `**P` (2)
    of PEP 695 stands for."""
    import typing
    if kind == 1:
        return typing.TypeVarTuple(name)
    if kind == 2:
        return typing.ParamSpec(name)
    return typing.TypeVar(name)


def _nova_generic():
    import typing
    return typing.Generic


def _type_repr(a):
    if a is Ellipsis:
        return '...'
    if isinstance(a, type):
        module = getattr(a, '__module__', 'builtins')
        if module == 'builtins':
            return a.__qualname__
        return '%s.%s' % (module, a.__qualname__)
    return repr(a)


class GenericAlias:
    """`list[int]`: the class it parameterizes, and its arguments."""

    __nova_type_name__ = 'types.GenericAlias'

    def __init__(self, origin, args):
        self.__origin__ = origin
        self.__args__ = args if isinstance(args, tuple) else (args,)
        self.__parameters__ = tuple(a for a in self.__args__
                                    if type(a).__name__ in ('TypeVar', 'ParamSpec', 'TypeVarTuple'))
        self.__unpacked__ = False

    def __call__(self, *args, **kwargs):
        return self.__origin__(*args, **kwargs)

    def __getattr__(self, name):
        if name.startswith('__') and name.endswith('__'):
            raise AttributeError(name)
        return getattr(self.__origin__, name)

    def __repr__(self):
        if not self.__args__:
            return '%s[()]' % _type_repr(self.__origin__)
        return '%s[%s]' % (_type_repr(self.__origin__), ', '.join(_type_repr(a) for a in self.__args__))

    def __eq__(self, other):
        if not isinstance(other, GenericAlias):
            return NotImplemented
        return self.__origin__ is other.__origin__ and self.__args__ == other.__args__

    def __hash__(self):
        return hash(self.__origin__) ^ hash(self.__args__)

    def __mro_entries__(self, bases):
        return (self.__origin__,)

    def __getitem__(self, params):
        return GenericAlias(self.__origin__, params)


def _nova_generic_alias(origin, args):
    return GenericAlias(origin, args)


class _Code:
    __nova_type_name__ = 'code'

    def __init__(self, filename, name):
        self.co_filename = filename
        self.co_name = name
        self.co_qualname = name
        self.co_firstlineno = 0

    def __repr__(self):
        return '<code object %s, file "%s">' % (self.co_name, self.co_filename)


class _Frame:
    __nova_type_name__ = 'frame'

    def __init__(self, filename, name, lineno):
        self.f_code = _Code(filename, name)
        self.f_lineno = lineno
        self.f_globals = {}
        self.f_locals = {}
        self.f_back = None
        self.f_lasti = 0

    def clear(self):
        pass

    def __repr__(self):
        return '<frame, file %r, line %d, code %s>' % (self.f_code.co_filename, self.f_lineno, self.f_code.co_name)


class traceback:
    """A link of an exception's traceback (outermost first), as CPython's
    tb_next chain; Nova keeps the entries, these objects are views."""

    __nova_type_name__ = 'traceback'

    def __init__(self, tb_next, tb_frame, tb_lasti, tb_lineno):
        self.tb_next = tb_next
        self.tb_frame = tb_frame
        self.tb_lasti = tb_lasti
        self.tb_lineno = tb_lineno
        self._entry = None

    def __repr__(self):
        return '<traceback object at %s>' % hex(id(self))


def _nova_traceback(entries):
    tb = None
    for entry in reversed(entries):
        filename, lineno, name = entry[0], entry[1], entry[2]
        tb = traceback(tb, _Frame(filename, name, lineno), 0, lineno)
        tb._entry = entry
    return tb
