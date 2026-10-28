"""itertools for Nova's Python: the documented equivalents, lazily.

Each function gives what CPython's gives, in the same order (product and
permutations in lexicographic order of positions, groupby sharing its
underlying iterator, tee buffering what one copy read ahead).
"""


class count:
    def __init__(self, start=0, step=1):
        for v in (start, step):
            if not isinstance(v, (int, float)):
                raise TypeError('a number is required')
        self._n = start
        self._step = step

    def __iter__(self):
        return self

    def __next__(self):
        n = self._n
        self._n += self._step
        return n

    def __repr__(self):
        if self._step == 1:
            return f'count({self._n!r})'
        return f'count({self._n!r}, {self._step!r})'


class repeat:
    def __init__(self, object, times=None):
        self._object = object
        self._times = times

    def __iter__(self):
        return self

    def __next__(self):
        if self._times is not None:
            if self._times <= 0:
                raise StopIteration
            self._times -= 1
        return self._object

    def __length_hint__(self):
        return self._times if self._times is not None else 0

    def __repr__(self):
        if self._times is None:
            return f'repeat({self._object!r})'
        return f'repeat({self._object!r}, {self._times})'


def cycle(iterable):
    saved = []
    for element in iterable:
        yield element
        saved.append(element)
    while saved:
        for element in saved:
            yield element


def accumulate(iterable, func=None, *, initial=None):
    it = iter(iterable)
    total = initial
    if initial is None:
        try:
            total = next(it)
        except StopIteration:
            return
    yield total
    for element in it:
        total = total + element if func is None else func(total, element)
        yield total


def batched(iterable, n, *, strict=False):
    if n < 1:
        raise ValueError('n must be at least one')
    it = iter(iterable)
    while True:
        batch = tuple(islice(it, n))
        if not batch:
            return
        if strict and len(batch) != n:
            raise ValueError('batched(): incomplete batch')
        yield batch


def chain(*iterables):
    for it in iterables:
        for element in it:
            yield element


def _from_iterable(iterables):
    for it in iterables:
        for element in it:
            yield element


chain.from_iterable = _from_iterable


def compress(data, selectors):
    return (d for d, s in zip(data, selectors) if s)


def dropwhile(predicate, iterable):
    iterable = iter(iterable)
    for x in iterable:
        if not predicate(x):
            yield x
            break
    for x in iterable:
        yield x


def filterfalse(predicate, iterable):
    if predicate is None:
        predicate = bool
    for x in iterable:
        if not predicate(x):
            yield x


class _Grouper:
    def __init__(self, parent, tgtkey):
        self._parent = parent
        self._tgtkey = tgtkey

    def __iter__(self):
        return self

    def __next__(self):
        parent = self._parent
        if parent._currkey is _marker:
            parent._step()
        if parent._exhausted or parent._currgrouper is not self:
            raise StopIteration
        if parent._currkey != self._tgtkey:
            raise StopIteration
        value = parent._currvalue
        parent._currkey = _marker
        return value


_marker = object()


class groupby:
    def __init__(self, iterable, key=None):
        self._keyfunc = (lambda x: x) if key is None else key
        self._it = iter(iterable)
        self._tgtkey = self._currkey = self._currvalue = _marker
        self._currgrouper = None
        self._exhausted = False

    def _step(self):
        try:
            self._currvalue = next(self._it)
        except StopIteration:
            self._exhausted = True
            self._currkey = _marker
            return
        self._currkey = self._keyfunc(self._currvalue)

    def __iter__(self):
        return self

    def __next__(self):
        self._currgrouper = None
        while True:
            if self._currkey is _marker:
                self._step()
                if self._exhausted:
                    raise StopIteration
            if self._tgtkey is _marker or self._currkey != self._tgtkey:
                break
            self._currkey = _marker
        self._tgtkey = self._currkey
        grouper = _Grouper(self, self._tgtkey)
        self._currgrouper = grouper
        return (self._currkey, grouper)


def islice(iterable, *args):
    s = slice(*args)
    start, stop, step = s.start or 0, s.stop, 1 if s.step is None else s.step
    for name, v in (('Indices', start), ('Stop argument', stop)):
        if v is not None and (not isinstance(v, int) or v < 0):
            raise ValueError(f'{name} for islice() must be None or an integer: 0 <= x <= sys.maxsize.')
    if not isinstance(step, int) or step < 1:
        raise ValueError('Step for islice() must be a positive integer or None.')
    it = iter(iterable)
    i = 0
    nexti = start
    if stop is not None and stop <= start:
        for _ in zip(range(stop), it):
            pass
        return
    for element in it:
        if i == nexti:
            yield element
            nexti += step
        i += 1
        if stop is not None and i >= stop:
            return


def pairwise(iterable):
    iterator = iter(iterable)
    a = next(iterator, _marker)
    if a is _marker:
        return
    for b in iterator:
        yield a, b
        a = b


def starmap(function, iterable):
    for args in iterable:
        yield function(*args)


def takewhile(predicate, iterable):
    for x in iterable:
        if predicate(x):
            yield x
        else:
            break


def tee(iterable, n=2):
    if n < 0:
        raise ValueError('n must be >= 0')
    it = iter(iterable)
    buffers = [[] for _ in range(n)]

    def gen(mine):
        while True:
            if not mine:
                try:
                    value = next(it)
                except StopIteration:
                    return
                for b in buffers:
                    b.append(value)
            yield mine.pop(0)
    return tuple(gen(b) for b in buffers)


def zip_longest(*iterables, fillvalue=None):
    iterators = [iter(it) for it in iterables]
    num_active = len(iterators)
    if not num_active:
        return
    while True:
        values = []
        for i, it in enumerate(iterators):
            try:
                value = next(it)
            except StopIteration:
                num_active -= 1
                if not num_active:
                    return
                iterators[i] = repeat(fillvalue)
                value = fillvalue
            values.append(value)
        yield tuple(values)


def product(*iterables, repeat=1):
    if repeat < 0:
        raise ValueError('repeat argument cannot be negative')
    pools = [tuple(pool) for pool in iterables] * repeat
    result = [[]]
    for pool in pools:
        result = [x + [y] for x in result for y in pool]
    for prod in result:
        yield tuple(prod)


def permutations(iterable, r=None):
    pool = tuple(iterable)
    n = len(pool)
    r = n if r is None else r
    if r > n:
        return
    indices = list(range(n))
    cycles = list(range(n, n - r, -1))
    yield tuple(pool[i] for i in indices[:r])
    while n:
        for i in reversed(range(r)):
            cycles[i] -= 1
            if cycles[i] == 0:
                indices[i:] = indices[i + 1:] + indices[i:i + 1]
                cycles[i] = n - i
            else:
                j = cycles[i]
                indices[i], indices[-j] = indices[-j], indices[i]
                yield tuple(pool[i] for i in indices[:r])
                break
        else:
            return


def combinations(iterable, r):
    pool = tuple(iterable)
    n = len(pool)
    if r > n:
        return
    indices = list(range(r))
    yield tuple(pool[i] for i in indices)
    while True:
        for i in reversed(range(r)):
            if indices[i] != i + n - r:
                break
        else:
            return
        indices[i] += 1
        for j in range(i + 1, r):
            indices[j] = indices[j - 1] + 1
        yield tuple(pool[i] for i in indices)


def combinations_with_replacement(iterable, r):
    pool = tuple(iterable)
    n = len(pool)
    if not n and r:
        return
    indices = [0] * r
    yield tuple(pool[i] for i in indices)
    while True:
        for i in reversed(range(r)):
            if indices[i] != n - 1:
                break
        else:
            return
        indices[i:] = [indices[i] + 1] * (r - i)
        yield tuple(pool[i] for i in indices)
