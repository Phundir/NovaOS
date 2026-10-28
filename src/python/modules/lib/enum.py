"""enum for Nova's Python: Enum, IntEnum, StrEnum, Flag, auto, unique.

Without metaclasses, the members are made when the class is created
(__init_subclass__): each plain attribute of the body becomes an instance
of the class. Iterating the class, len() and `in` go through the hooks the
interpreter offers in place of a metaclass (__class_iter__...).
"""


class auto:
    _counter = 0

    def __init__(self):
        auto._counter += 1
        self._order = auto._counter
        self.value = None

    def __repr__(self):
        return 'auto()'


def _is_descriptor(obj):
    return (hasattr(obj, '__get__') or hasattr(obj, '__set__') or hasattr(obj, '__delete__')
            or isinstance(obj, (staticmethod, classmethod, property)))


def _is_member_name(name):
    return not (name.startswith('__') and name.endswith('__')) and not (name.startswith('_') and name.endswith('_') and len(name) > 2)


class Enum:
    def __init_subclass__(cls, **kwargs):
        super().__init_subclass__(**kwargs)
        # A data type's repr gives way to Enum's (EnumType does this).
        if '__repr__' not in cls.__dict__ and getattr(cls, '__repr__') in _data_reprs:
            cls.__repr__ = Enum.__repr__
        cls._member_names_ = []
        cls._member_map_ = {}
        cls._value2member_map_ = {}
        last_values = []
        candidates = []
        for name, value in list(cls.__dict__.items()):
            if not _is_member_name(name) or _is_descriptor(value) or callable(value) and not isinstance(value, auto):
                continue
            if name.startswith('_'):
                continue
            candidates.append((name, value))
        for name, value in candidates:
            if isinstance(value, auto):
                value = cls._generate_next_value_(name, 1, len(last_values), last_values[:])
            last_values.append(value)
            if value in cls._value2member_map_:
                member = cls._value2member_map_[value]
            else:
                member = cls._new_member_(value)
                member._name_ = name
                member._value_ = value
                cls._member_names_.append(name)
                try:
                    cls._value2member_map_[value] = member
                except TypeError:
                    pass
                init = cls.__dict__.get('__init__')
                if init is not None:
                    args = value if isinstance(value, tuple) else (value,)
                    init(member, *args)
            cls._member_map_[name] = member
            setattr(cls, name, member)
        cls.__members__ = dict(cls._member_map_)

    @classmethod
    def _new_member_(cls, value):
        for base in cls.__mro__:
            if base in (int, str, float):
                member = base.__new__(cls, value)
                return member
        return object.__new__(cls)

    @staticmethod
    def _generate_next_value_(name, start, count, last_values):
        for last_value in reversed(last_values):
            try:
                return last_value + 1
            except TypeError:
                pass
        return start

    def __new__(cls, value):
        try:
            return cls._value2member_map_[value]
        except (KeyError, TypeError, AttributeError):
            pass
        for member in cls._member_map_.values():
            if member._value_ == value:
                return member
        missing = cls._missing_(value)
        if isinstance(missing, cls):
            return missing
        raise ValueError(f"{value!r} is not a valid {cls.__qualname__}")

    def __init__(self, *args, **kwds):
        pass

    @classmethod
    def _missing_(cls, value):
        return None

    @classmethod
    def __class_iter__(cls):
        return iter([cls._member_map_[name] for name in cls._member_names_])

    @classmethod
    def __class_len__(cls):
        return len(cls._member_names_)

    @classmethod
    def __class_contains__(cls, value):
        if isinstance(value, cls):
            return True
        return value in cls._value2member_map_

    @classmethod
    def __class_getitem__(cls, name):
        return cls._member_map_[name]

    @classmethod
    def __class_repr__(cls):
        kind = 'flag' if issubclass(cls, Flag) else 'enum'
        return f"<{kind} {cls.__name__!r}>"

    @property
    def name(self):
        return self._name_

    @property
    def value(self):
        return self._value_

    def __repr__(self):
        v_repr = type(self)._value_repr_ if hasattr(type(self), '_value_repr_') else repr
        return f"<{type(self).__qualname__}.{self._name_}: {v_repr(self._value_)}>"

    def __str__(self):
        return f"{type(self).__qualname__}.{self._name_}"

    def __format__(self, spec):
        return format(str(self), spec)

    def __hash__(self):
        return hash(self._name_)

    def __reduce_ex__(self, proto):
        return self.__class__, (self._value_,)

    def __eq__(self, other):
        return self is other


_data_reprs = (int.__repr__, str.__repr__, float.__repr__, object.__repr__)


class ReprEnum(Enum):
    pass


class IntEnum(int, ReprEnum):
    def __str__(self):
        return int.__repr__(self)

    def __format__(self, spec):
        return int.__format__(self, spec)

    def __eq__(self, other):
        return int(self) == other

    def __hash__(self):
        return hash(int(self))

    def __lt__(self, other):
        return int(self) < other

    def __le__(self, other):
        return int(self) <= other

    def __gt__(self, other):
        return int(self) > other

    def __ge__(self, other):
        return int(self) >= other


class StrEnum(str, ReprEnum):
    def __str__(self):
        return str.__str__(self)

    def __format__(self, spec):
        return str.__format__(self, spec)

    def __eq__(self, other):
        return str(self) == other

    def __hash__(self):
        return hash(str(self))

    @staticmethod
    def _generate_next_value_(name, start, count, last_values):
        return name.lower()


class Flag(Enum):
    @staticmethod
    def _generate_next_value_(name, start, count, last_values):
        if not count:
            return 1
        high = max(last_values)
        return 2 ** high.bit_length()

    @classmethod
    def _missing_(cls, value):
        if not isinstance(value, int):
            return None
        member = object.__new__(cls)
        member._value_ = value
        names = [m._name_ for m in cls if m._value_ & value == m._value_ and m._value_]
        member._name_ = '|'.join(names) if names else None
        return member

    def __or__(self, other):
        return type(self)(self._value_ | other._value_)

    def __and__(self, other):
        return type(self)(self._value_ & other._value_)

    def __xor__(self, other):
        return type(self)(self._value_ ^ other._value_)

    def __contains__(self, other):
        return other._value_ & self._value_ == other._value_

    def __bool__(self):
        return bool(self._value_)

    def __eq__(self, other):
        return isinstance(other, Flag) and self._value_ == other._value_

    def __hash__(self):
        return hash(self._value_)


class IntFlag(int, Flag):
    def __or__(self, other):
        return type(self)(int(self) | int(other))

    def __and__(self, other):
        return type(self)(int(self) & int(other))

    def __str__(self):
        return int.__repr__(self)


def unique(enumeration):
    duplicates = []
    for name, member in enumeration._member_map_.items():
        if name != member._name_:
            duplicates.append((name, member._name_))
    if duplicates:
        alias_details = ', '.join(["%s -> %s" % (alias, name) for (alias, name) in duplicates])
        raise ValueError('duplicate values found in %r: %s' % (enumeration, alias_details))
    return enumeration
