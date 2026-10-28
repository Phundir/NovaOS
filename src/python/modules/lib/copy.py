"""copy for Nova's Python: shallow and deep copies, with memo."""


class Error(Exception):
    pass


error = Error


def copy(x):
    cls = type(x)
    if cls in (int, float, bool, str, bytes, tuple, frozenset, type(None), range, type) or callable(x) and not hasattr(x, '__dict__'):
        return x
    copier = getattr(cls, '__copy__', None)
    if copier is not None:
        return copier(x)
    if cls is list:
        return x[:]
    if cls is dict:
        return x.copy()
    if cls is set:
        return x.copy()
    if isinstance(x, list):
        y = cls.__new__(cls)
        y.extend(x)
        y.__dict__.update(x.__dict__)
        return y
    if isinstance(x, dict):
        y = cls.__new__(cls)
        y.update(x)
        y.__dict__.update(x.__dict__)
        return y
    if isinstance(x, (tuple, int, float, str, frozenset)):
        return x
    y = cls.__new__(cls)
    y.__dict__.update(x.__dict__)
    return y


def deepcopy(x, memo=None, _nil=[]):
    if memo is None:
        memo = {}
    d = id(x)
    y = memo.get(d, _nil)
    if y is not _nil:
        return y
    cls = type(x)
    if cls in (int, float, bool, str, bytes, type(None), range, type, frozenset) or (callable(x) and not hasattr(x, '__dict__')):
        return x
    copier = getattr(cls, '__deepcopy__', None)
    if copier is not None:
        y = copier(x, memo)
    elif cls is list:
        y = []
        memo[d] = y
        for a in x:
            y.append(deepcopy(a, memo))
    elif cls is tuple:
        items = [deepcopy(a, memo) for a in x]
        if d in memo:
            return memo[d]
        y = x if all(a is b for a, b in zip(items, x)) else tuple(items)
    elif cls is dict:
        y = {}
        memo[d] = y
        for key, value in x.items():
            y[deepcopy(key, memo)] = deepcopy(value, memo)
    elif cls is set:
        y = {deepcopy(a, memo) for a in x}
    else:
        y = cls.__new__(cls)
        memo[d] = y
        if isinstance(x, list):
            y.extend(deepcopy(a, memo) for a in x)
        elif isinstance(x, dict):
            for key, value in x.items():
                y[deepcopy(key, memo)] = deepcopy(value, memo)
        elif isinstance(x, tuple):
            y = cls(*(deepcopy(a, memo) for a in x)) if hasattr(cls, '_fields') else cls(deepcopy(a, memo) for a in x)
        state = getattr(x, '__dict__', None)
        if state:
            y.__dict__.update(deepcopy(state, memo))
    if x is not y:
        memo[d] = y
        _keep_alive(x, memo)
    return y


def _keep_alive(x, memo):
    try:
        memo[id(memo)].append(x)
    except KeyError:
        memo[id(memo)] = [x]


def replace(obj, /, **changes):
    func = getattr(type(obj), '__replace__', None)
    if func is None:
        raise TypeError(f"replace() does not support {type(obj).__name__} objects")
    return func(obj, **changes)
