"""functools for Nova's Python: CPython's behaviour, written in Python.

The frames of this module do not count towards the recursion limit
(they stand for CPython's C code): an lru_cache'd recursive function goes
as deep as in CPython.
"""

WRAPPER_ASSIGNMENTS = ('__module__', '__name__', '__qualname__', '__doc__')
WRAPPER_UPDATES = ('__dict__',)


def update_wrapper(wrapper, wrapped, assigned=WRAPPER_ASSIGNMENTS, updated=WRAPPER_UPDATES):
    for attr in assigned:
        try:
            value = getattr(wrapped, attr)
        except AttributeError:
            pass
        else:
            try:
                setattr(wrapper, attr, value)
            except (AttributeError, TypeError):
                pass
    for attr in updated:
        try:
            getattr(wrapper, attr).update(getattr(wrapped, attr, {}))
        except (AttributeError, TypeError):
            pass
    try:
        wrapper.__wrapped__ = wrapped
    except (AttributeError, TypeError):
        pass
    return wrapper


def wraps(wrapped, assigned=WRAPPER_ASSIGNMENTS, updated=WRAPPER_UPDATES):
    return partial(update_wrapper, wrapped=wrapped, assigned=assigned, updated=updated)


class partial:
    def __new__(cls, func, /, *args, **keywords):
        if not callable(func):
            raise TypeError("the first argument must be callable")
        if isinstance(func, partial):
            args = func.args + args
            keywords = {**func.keywords, **keywords}
            func = func.func
        self = super().__new__(cls)
        self.func = func
        self.args = args
        self.keywords = keywords
        return self

    def __call__(self, /, *args, **keywords):
        keywords = {**self.keywords, **keywords}
        return self.func(*self.args, *args, **keywords)

    def __repr__(self):
        args = [repr(self.func)]
        args.extend(repr(x) for x in self.args)
        args.extend(f"{k}={v!r}" for (k, v) in self.keywords.items())
        return f"functools.partial({', '.join(args)})"

    def __get__(self, obj, objtype=None):
        return self


_initial_missing = object()


def reduce(function, sequence, initial=_initial_missing):
    it = iter(sequence)
    if initial is _initial_missing:
        try:
            value = next(it)
        except StopIteration:
            raise TypeError("reduce() of empty iterable with no initial value") from None
    else:
        value = initial
    for element in it:
        value = function(value, element)
    return value


def cmp_to_key(mycmp):
    class K(object):
        __slots__ = ['obj']

        def __init__(self, obj):
            self.obj = obj

        def __lt__(self, other):
            return mycmp(self.obj, other.obj) < 0

        def __gt__(self, other):
            return mycmp(self.obj, other.obj) > 0

        def __eq__(self, other):
            return mycmp(self.obj, other.obj) == 0

        def __le__(self, other):
            return mycmp(self.obj, other.obj) <= 0

        def __ge__(self, other):
            return mycmp(self.obj, other.obj) >= 0

        __hash__ = None
    return K


def total_ordering(cls):
    roots = {op for op in ('__lt__', '__le__', '__gt__', '__ge__')
             if getattr(cls, op, None) is not getattr(object, op, None) and op in _class_ops(cls)}
    if not roots:
        raise ValueError('must define at least one ordering operation: < > <= >=')
    root = max(roots)
    for opname, opfunc in _convert[root]:
        if opname not in roots:
            opfunc.__name__ = opname
            setattr(cls, opname, opfunc)
    return cls


def _class_ops(cls):
    names = set()
    for klass in cls.__mro__:
        if klass is object:
            break
        names.update(klass.__dict__)
    return names


def _gt_from_lt(self, other):
    op_result = type(self).__lt__(self, other)
    if op_result is NotImplemented:
        return op_result
    return not op_result and self != other


def _le_from_lt(self, other):
    op_result = type(self).__lt__(self, other)
    if op_result is NotImplemented:
        return op_result
    return op_result or self == other


def _ge_from_lt(self, other):
    op_result = type(self).__lt__(self, other)
    if op_result is NotImplemented:
        return op_result
    return not op_result


def _ge_from_le(self, other):
    op_result = type(self).__le__(self, other)
    if op_result is NotImplemented:
        return op_result
    return not op_result or self == other


def _lt_from_le(self, other):
    op_result = type(self).__le__(self, other)
    if op_result is NotImplemented:
        return op_result
    return op_result and self != other


def _gt_from_le(self, other):
    op_result = type(self).__le__(self, other)
    if op_result is NotImplemented:
        return op_result
    return not op_result


def _lt_from_gt(self, other):
    op_result = type(self).__gt__(self, other)
    if op_result is NotImplemented:
        return op_result
    return not op_result and self != other


def _ge_from_gt(self, other):
    op_result = type(self).__gt__(self, other)
    if op_result is NotImplemented:
        return op_result
    return op_result or self == other


def _le_from_gt(self, other):
    op_result = type(self).__gt__(self, other)
    if op_result is NotImplemented:
        return op_result
    return not op_result


def _le_from_ge(self, other):
    op_result = type(self).__ge__(self, other)
    if op_result is NotImplemented:
        return op_result
    return not op_result or self == other


def _gt_from_ge(self, other):
    op_result = type(self).__ge__(self, other)
    if op_result is NotImplemented:
        return op_result
    return op_result and self != other


def _lt_from_ge(self, other):
    op_result = type(self).__ge__(self, other)
    if op_result is NotImplemented:
        return op_result
    return not op_result


_convert = {
    '__lt__': [('__gt__', _gt_from_lt), ('__le__', _le_from_lt), ('__ge__', _ge_from_lt)],
    '__le__': [('__ge__', _ge_from_le), ('__lt__', _lt_from_le), ('__gt__', _gt_from_le)],
    '__gt__': [('__lt__', _lt_from_gt), ('__ge__', _ge_from_gt), ('__le__', _le_from_gt)],
    '__ge__': [('__le__', _le_from_ge), ('__gt__', _gt_from_ge), ('__lt__', _lt_from_ge)],
}


class _CacheInfo(tuple):
    def __new__(cls, hits, misses, maxsize, currsize):
        return tuple.__new__(cls, (hits, misses, maxsize, currsize))

    hits = property(lambda self: self[0])
    misses = property(lambda self: self[1])
    maxsize = property(lambda self: self[2])
    currsize = property(lambda self: self[3])

    def __repr__(self):
        return "CacheInfo(hits=%r, misses=%r, maxsize=%r, currsize=%r)" % tuple(self)


_kwd_mark = (object(),)


def _make_key(args, kwds, typed):
    key = args
    if kwds:
        key += _kwd_mark
        for item in kwds.items():
            key += item
    if typed:
        key += tuple(type(v) for v in args)
        if kwds:
            key += tuple(type(v) for v in kwds.values())
    elif len(key) == 1 and type(key[0]) in {int, str}:
        return key[0]
    return key


def lru_cache(maxsize=128, typed=False):
    if isinstance(maxsize, int):
        if maxsize < 0:
            maxsize = 0
    elif callable(maxsize) and isinstance(typed, bool):
        user_function, maxsize = maxsize, 128
        return _lru_cache_wrapper(user_function, maxsize, typed)
    elif maxsize is not None:
        raise TypeError('Expected first argument to be an integer, a callable, or None')

    def decorating_function(user_function):
        return _lru_cache_wrapper(user_function, maxsize, typed)
    return decorating_function


def _lru_cache_wrapper(user_function, maxsize, typed):
    cache = {}
    stats = [0, 0]

    if maxsize == 0:
        def wrapper(*args, **kwds):
            stats[1] += 1
            return user_function(*args, **kwds)
    elif maxsize is None:
        def wrapper(*args, **kwds):
            key = _make_key(args, kwds, typed)
            if key in cache:
                stats[0] += 1
                return cache[key]
            stats[1] += 1
            result = user_function(*args, **kwds)
            cache[key] = result
            return result
    else:
        def wrapper(*args, **kwds):
            key = _make_key(args, kwds, typed)
            if key in cache:
                stats[0] += 1
                # Most recently used: to the end of the dict's order.
                result = cache.pop(key)
                cache[key] = result
                return result
            stats[1] += 1
            result = user_function(*args, **kwds)
            if key not in cache:
                if len(cache) >= maxsize:
                    del cache[next(iter(cache))]
                cache[key] = result
            return result

    def cache_info():
        return _CacheInfo(stats[0], stats[1], maxsize, len(cache))

    def cache_clear():
        cache.clear()
        stats[0] = stats[1] = 0

    wrapper.cache_info = cache_info
    wrapper.cache_clear = cache_clear
    wrapper.cache_parameters = lambda: {'maxsize': maxsize, 'typed': typed}
    return update_wrapper(wrapper, user_function)


def cache(user_function, /):
    return lru_cache(maxsize=None)(user_function)


class cached_property:
    def __init__(self, func):
        self.func = func
        self.attrname = None
        self.__doc__ = func.__doc__

    def __set_name__(self, owner, name):
        if self.attrname is None:
            self.attrname = name
        elif name != self.attrname:
            raise TypeError(
                "Cannot assign the same cached_property to two different names "
                f"({self.attrname!r} and {name!r})."
            )

    def __get__(self, instance, owner=None):
        if instance is None:
            return self
        if self.attrname is None:
            raise TypeError("Cannot use cached_property instance without calling __set_name__ on it.")
        cache = instance.__dict__
        if self.attrname in cache:
            return cache[self.attrname]
        val = self.func(instance)
        cache[self.attrname] = val
        return val


def singledispatch(func):
    registry = {}

    def dispatch(cls):
        for klass in cls.__mro__:
            if klass in registry:
                return registry[klass]
        return func

    def register(cls, method=None):
        if method is None:
            if isinstance(cls, type):
                return lambda f: register(cls, f)
            annotations = getattr(cls, '__annotations__', {})
            raise TypeError("Invalid first argument to `register()`: %r. Use either `@register(some_class)` or plain `@register` on an annotated function." % (cls,))
        registry[cls] = method
        return method

    def wrapper(*args, **kw):
        if not args:
            raise TypeError(f'{funcname} requires at least 1 positional argument')
        return dispatch(args[0].__class__)(*args, **kw)

    funcname = getattr(func, '__name__', 'singledispatch function')
    registry[object] = func
    wrapper.register = register
    wrapper.dispatch = dispatch
    wrapper.registry = registry
    update_wrapper(wrapper, func)
    return wrapper
