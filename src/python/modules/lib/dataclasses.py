"""dataclasses for Nova's Python.

As in CPython, __init__ is generated as source and exec'd: its signature,
its defaults and its errors ("Point.__init__() missing 1 required
positional argument: 'y'") are those of a function written by hand.
"""

import copy as _copy


class FrozenInstanceError(AttributeError):
    pass


class _MissingType:
    def __repr__(self):
        return 'MISSING'


MISSING = _MissingType()


class _HasDefaultFactory:
    def __repr__(self):
        return '<factory>'


_HAS_DEFAULT_FACTORY = _HasDefaultFactory()


class _KwOnlyType:
    pass


KW_ONLY = _KwOnlyType()


class InitVar:
    def __init__(self, type):
        self.type = type

    def __class_getitem__(cls, type):
        return InitVar(type)


class Field:
    def __init__(self, default, default_factory, init, repr, hash, compare, metadata, kw_only):
        self.name = None
        self.type = None
        self.default = default
        self.default_factory = default_factory
        self.init = init
        self.repr = repr
        self.hash = hash
        self.compare = compare
        self.metadata = {} if metadata is None else metadata
        self.kw_only = kw_only
        self._field_type = '_FIELD'

    def __repr__(self):
        return ('Field('
                f'name={self.name!r},'
                f'type={self.type!r},'
                f'default={self.default!r},'
                f'default_factory={self.default_factory!r},'
                f'init={self.init!r},'
                f'repr={self.repr!r},'
                f'hash={self.hash!r},'
                f'compare={self.compare!r},'
                f'metadata={self.metadata!r},'
                f'kw_only={self.kw_only!r},'
                f'_field_type={self._field_type})')


def field(*, default=MISSING, default_factory=MISSING, init=True, repr=True,
          hash=None, compare=True, metadata=None, kw_only=MISSING):
    if default is not MISSING and default_factory is not MISSING:
        raise ValueError('cannot specify both default and default_factory')
    return Field(default, default_factory, init, repr, hash, compare, metadata, kw_only)


def _is_classvar(annotation):
    name = getattr(annotation, '_name', None)
    if name == 'ClassVar':
        return True
    return isinstance(annotation, str) and annotation.startswith(('ClassVar', 'typing.ClassVar'))


def _type_name(annotation):
    if isinstance(annotation, type):
        return annotation.__name__
    return repr(annotation)


def _process_class(cls, init, repr, eq, order, unsafe_hash, frozen, match_args, kw_only, slots):
    fields = {}
    for base in cls.__mro__[-1:0:-1]:
        base_fields = getattr(base, '__dataclass_fields__', None)
        if base_fields is not None:
            for f in base_fields.values():
                fields[f.name] = f
    annotations = cls.__dict__.get('__annotations__', {})
    kw_only_mode = kw_only
    for name, annotation in annotations.items():
        if annotation is KW_ONLY or getattr(annotation, '__class__', None) is _KwOnlyType:
            kw_only_mode = True
            continue
        if _is_classvar(annotation):
            continue
        default = cls.__dict__.get(name, MISSING)
        if isinstance(default, Field):
            f = default
        else:
            f = field(default=default)
        f.name = name
        f.type = annotation
        if f.kw_only is MISSING:
            f.kw_only = kw_only_mode
        if isinstance(annotation, InitVar):
            f._field_type = '_FIELD_INITVAR'
        if f.default is not MISSING and f.default.__class__.__hash__ is None and isinstance(f.default, (list, dict, set)):
            raise ValueError(f'mutable default {type(f.default)} for field {name} is not allowed: use default_factory')
        fields[name] = f
        if isinstance(default, Field):
            if f.default is MISSING:
                try:
                    delattr(cls, name)
                except AttributeError:
                    pass
            else:
                setattr(cls, name, f.default)
    for name, value in cls.__dict__.items():
        if isinstance(value, Field) and name not in annotations:
            raise TypeError(f'{name!r} is a field but has no type annotation')
    cls.__dataclass_fields__ = fields
    cls.__dataclass_params__ = (init, repr, eq, order, unsafe_hash, frozen)
    all_fields = [f for f in fields.values() if f._field_type == '_FIELD']
    init_fields = [f for f in fields.values() if f.init]

    if init:
        _set_init(cls, init_fields, frozen)
    if repr:
        repr_fields = [f for f in all_fields if f.repr]

        def __repr__(self):
            return self.__class__.__qualname__ + '(' + ', '.join(f'{f.name}={getattr(self, f.name)!r}' for f in repr_fields) + ')'
        __repr__.__qualname__ = f'{cls.__qualname__}.__repr__'
        if '__repr__' not in cls.__dict__:
            cls.__repr__ = __repr__
    compare_fields = [f for f in all_fields if f.compare]

    def astuple_compare(self):
        return tuple(getattr(self, f.name) for f in compare_fields)
    if eq and '__eq__' not in cls.__dict__:
        def __eq__(self, other):
            if self is other:
                return True
            if other.__class__ is self.__class__:
                return astuple_compare(self) == astuple_compare(other)
            return NotImplemented
        cls.__eq__ = __eq__
    if order:
        for name, op in (('__lt__', lambda a, b: a < b), ('__le__', lambda a, b: a <= b),
                         ('__gt__', lambda a, b: a > b), ('__ge__', lambda a, b: a >= b)):
            if name in cls.__dict__:
                raise TypeError(f'Cannot overwrite attribute {name} in class {cls.__name__}. Consider using functools.total_ordering')

            def method(self, other, op=op):
                if other.__class__ is self.__class__:
                    return op(astuple_compare(self), astuple_compare(other))
                return NotImplemented
            method.__name__ = name
            setattr(cls, name, method)
    if frozen:
        def __setattr__(self, name, value):
            if type(self) is cls or name in fields:
                raise FrozenInstanceError(f'cannot assign to field {name!r}')
            object.__setattr__(self, name, value)

        def __delattr__(self, name):
            if type(self) is cls or name in fields:
                raise FrozenInstanceError(f'cannot delete field {name!r}')
            object.__delattr__(self, name)
        cls.__setattr__ = __setattr__
        cls.__delattr__ = __delattr__
    # CPython's table of __hash__ actions.
    has_hash = '__hash__' in cls.__dict__ and cls.__dict__['__hash__'] is not None
    hash_fields = [f for f in all_fields if (f.compare if f.hash is None else f.hash)]

    def __hash__(self):
        return hash(tuple(getattr(self, f.name) for f in hash_fields))
    if unsafe_hash:
        if has_hash:
            raise TypeError(f'Cannot overwrite attribute __hash__ in class {cls.__name__}')
        cls.__hash__ = __hash__
    elif eq and frozen:
        if not has_hash:
            cls.__hash__ = __hash__
    elif eq:
        if not has_hash:
            cls.__hash__ = None
    if match_args and '__match_args__' not in cls.__dict__:
        cls.__match_args__ = tuple(f.name for f in init_fields if not f.kw_only)
    return cls


def _set_init(cls, fields, frozen):
    seen_default = None
    for f in fields:
        if f.kw_only:
            continue
        if f.default is not MISSING or f.default_factory is not MISSING:
            seen_default = f
        elif seen_default is not None:
            raise TypeError(f'non-default argument {f.name!r} follows default argument {seen_default.name!r}')
    namespace = {'__dataclass_HAS_DEFAULT_FACTORY__': _HAS_DEFAULT_FACTORY, 'MISSING': MISSING,
                 '__dataclass_builtins_object__': object}
    params = []
    kw_params = []
    body = []
    for f in fields:
        if f.default is not MISSING:
            namespace[f'__dataclass_dflt_{f.name}__'] = f.default
            param = f'{f.name}=__dataclass_dflt_{f.name}__'
        elif f.default_factory is not MISSING:
            namespace[f'__dataclass_dflt_{f.name}__'] = f.default_factory
            param = f'{f.name}=__dataclass_HAS_DEFAULT_FACTORY__'
        else:
            param = f.name
        (kw_params if f.kw_only else params).append(param)
        if f._field_type == '_FIELD_INITVAR':
            continue
        if f.default_factory is not MISSING:
            value = f'__dataclass_dflt_{f.name}__() if {f.name} is __dataclass_HAS_DEFAULT_FACTORY__ else {f.name}'
        else:
            value = f.name
        if frozen:
            body.append(f'__dataclass_builtins_object__.__setattr__(self, {f.name!r}, {value})')
        else:
            body.append(f'self.{f.name} = {value}')
    for f in cls.__dataclass_fields__.values():
        if not f.init and f._field_type == '_FIELD':
            if f.default_factory is not MISSING:
                namespace[f'__dataclass_dflt_{f.name}__'] = f.default_factory
                value = f'__dataclass_dflt_{f.name}__()'
            elif f.default is not MISSING:
                namespace[f'__dataclass_dflt_{f.name}__'] = f.default
                value = f'__dataclass_dflt_{f.name}__'
            else:
                continue
            if frozen:
                body.append(f'__dataclass_builtins_object__.__setattr__(self, {f.name!r}, {value})')
            else:
                body.append(f'self.{f.name} = {value}')
    if hasattr(cls, '__post_init__'):
        initvars = [f.name for f in fields if f._field_type == '_FIELD_INITVAR']
        body.append(f'self.__post_init__({", ".join(initvars)})')
    if not body:
        body = ['pass']
    signature = ', '.join(['self'] + params + (['*'] + kw_params if kw_params else []))
    source = f'def __init__({signature}):\n' + '\n'.join('    ' + line for line in body)
    exec(source, namespace)
    init = namespace['__init__']
    init.__qualname__ = f'{cls.__qualname__}.__init__'
    cls.__init__ = init


def dataclass(cls=None, /, *, init=True, repr=True, eq=True, order=False,
              unsafe_hash=False, frozen=False, match_args=True,
              kw_only=False, slots=False, weakref_slot=False):
    def wrap(cls):
        return _process_class(cls, init, repr, eq, order, unsafe_hash, frozen, match_args, kw_only, slots)
    if cls is None:
        return wrap
    return wrap(cls)


def fields(class_or_instance):
    try:
        fields = getattr(class_or_instance, '__dataclass_fields__')
    except AttributeError:
        raise TypeError('must be called with a dataclass type or instance') from None
    return tuple(f for f in fields.values() if f._field_type == '_FIELD')


def is_dataclass(obj):
    cls = obj if isinstance(obj, type) else type(obj)
    return hasattr(cls, '__dataclass_fields__')


def _is_dataclass_instance(obj):
    return hasattr(type(obj), '__dataclass_fields__')


def asdict(obj, *, dict_factory=dict):
    if not _is_dataclass_instance(obj):
        raise TypeError('asdict() should be called on dataclass instances')
    return _asdict_inner(obj, dict_factory)


def _asdict_inner(obj, dict_factory):
    if _is_dataclass_instance(obj):
        return dict_factory([(f.name, _asdict_inner(getattr(obj, f.name), dict_factory)) for f in fields(obj)])
    if isinstance(obj, tuple) and hasattr(obj, '_fields'):
        return type(obj)(*[_asdict_inner(v, dict_factory) for v in obj])
    if isinstance(obj, (list, tuple)):
        return type(obj)(_asdict_inner(v, dict_factory) for v in obj)
    if isinstance(obj, dict):
        return type(obj)((_asdict_inner(k, dict_factory), _asdict_inner(v, dict_factory)) for k, v in obj.items())
    return _copy.deepcopy(obj)


def astuple(obj, *, tuple_factory=tuple):
    if not _is_dataclass_instance(obj):
        raise TypeError('astuple() should be called on dataclass instances')
    return _astuple_inner(obj, tuple_factory)


def _astuple_inner(obj, tuple_factory):
    if _is_dataclass_instance(obj):
        return tuple_factory([_astuple_inner(getattr(obj, f.name), tuple_factory) for f in fields(obj)])
    if isinstance(obj, tuple) and hasattr(obj, '_fields'):
        return type(obj)(*[_astuple_inner(v, tuple_factory) for v in obj])
    if isinstance(obj, (list, tuple)):
        return type(obj)(_astuple_inner(v, tuple_factory) for v in obj)
    if isinstance(obj, dict):
        return type(obj)((_astuple_inner(k, tuple_factory), _astuple_inner(v, tuple_factory)) for k, v in obj.items())
    return _copy.deepcopy(obj)


def replace(obj, /, **changes):
    if not _is_dataclass_instance(obj):
        raise TypeError('replace() should be called on dataclass instances')
    for f in getattr(obj, '__dataclass_fields__').values():
        if not f.init:
            if f.name in changes:
                raise ValueError(f'field {f.name} is declared with init=False, it cannot be specified with replace()')
            continue
        if f.name not in changes:
            changes[f.name] = getattr(obj, f.name)
    return obj.__class__(**changes)


def make_dataclass(cls_name, fields, *, bases=(), namespace=None, **kwargs):
    namespace = dict(namespace or {})
    annotations = {}
    for item in fields:
        if isinstance(item, str):
            annotations[item] = 'typing.Any'
        elif len(item) == 2:
            annotations[item[0]] = item[1]
        else:
            annotations[item[0]] = item[1]
            namespace[item[0]] = item[2]
    namespace['__annotations__'] = annotations
    cls = type(cls_name, bases, namespace)
    return dataclass(cls, **kwargs)
