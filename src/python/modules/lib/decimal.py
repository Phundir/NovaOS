"""decimal for Nova's Python: the General Decimal Arithmetic of CPython.

A Decimal is a sign, a coefficient (a string of digits) and an exponent,
or a special value (Infinity, NaN, sNaN). Every operation computes the
exact result, or enough of it, and rounds it once to the context's
precision with the context's rounding mode, raising the signals the
specification names (a trapped signal is an exception, its message the
list of conditions as CPython's C module prints it). sqrt, exp, ln,
log10 and non-integer powers are correctly rounded: they are evaluated
with guard digits until the rounding is certain.
"""

import math as _math
import numbers as _numbers
from collections import namedtuple as _namedtuple

__all__ = [
    'Decimal', 'Context', 'DecimalTuple',
    'DefaultContext', 'BasicContext', 'ExtendedContext',
    'DecimalException', 'Clamped', 'InvalidOperation', 'DivisionByZero',
    'Inexact', 'Rounded', 'Subnormal', 'Overflow', 'Underflow',
    'FloatOperation', 'DivisionImpossible', 'InvalidContext',
    'ConversionSyntax', 'DivisionUndefined',
    'ROUND_DOWN', 'ROUND_HALF_UP', 'ROUND_HALF_EVEN', 'ROUND_CEILING',
    'ROUND_FLOOR', 'ROUND_UP', 'ROUND_HALF_DOWN', 'ROUND_05UP',
    'setcontext', 'getcontext', 'localcontext',
    'MAX_PREC', 'MAX_EMAX', 'MIN_EMIN', 'MIN_ETINY', 'HAVE_THREADS',
    'HAVE_CONTEXTVAR',
]

__version__ = '1.70'
__libmpdec_version__ = '2.5.1'

DecimalTuple = _namedtuple('DecimalTuple', 'sign digits exponent')

ROUND_DOWN = 'ROUND_DOWN'
ROUND_HALF_UP = 'ROUND_HALF_UP'
ROUND_HALF_EVEN = 'ROUND_HALF_EVEN'
ROUND_CEILING = 'ROUND_CEILING'
ROUND_FLOOR = 'ROUND_FLOOR'
ROUND_UP = 'ROUND_UP'
ROUND_HALF_DOWN = 'ROUND_HALF_DOWN'
ROUND_05UP = 'ROUND_05UP'

HAVE_THREADS = True
HAVE_CONTEXTVAR = True
MAX_PREC = 999999999999999999
MAX_EMAX = 999999999999999999
MIN_EMIN = -999999999999999999
MIN_ETINY = MIN_EMIN - (MAX_PREC - 1)


class DecimalException(ArithmeticError):
    def handle(self, context, *args):
        pass


class Clamped(DecimalException):
    pass


class InvalidOperation(DecimalException):
    def handle(self, context, *args):
        if args:
            ans = _dec_from_triple(args[0]._sign, args[0]._int, 'n', True)
            return ans._fix_nan(context)
        return _NaN


class ConversionSyntax(InvalidOperation):
    def handle(self, context, *args):
        return _NaN


class DivisionByZero(DecimalException, ZeroDivisionError):
    def handle(self, context, sign, *args):
        return _SignedInfinity[sign]


class DivisionImpossible(InvalidOperation):
    def handle(self, context, *args):
        return _NaN


class DivisionUndefined(InvalidOperation, ZeroDivisionError):
    def handle(self, context, *args):
        return _NaN


class Inexact(DecimalException):
    pass


class InvalidContext(InvalidOperation):
    def handle(self, context, *args):
        return _NaN


class Rounded(DecimalException):
    pass


class Subnormal(DecimalException):
    pass


class Overflow(Inexact, Rounded):
    def handle(self, context, sign, *args):
        if context.rounding in (ROUND_HALF_UP, ROUND_HALF_EVEN,
                                ROUND_HALF_DOWN, ROUND_UP):
            return _SignedInfinity[sign]
        if sign == 0:
            if context.rounding == ROUND_CEILING:
                return _SignedInfinity[sign]
            return _dec_from_triple(sign, '9' * context.prec,
                                    context.Emax - context.prec + 1)
        if sign == 1:
            if context.rounding == ROUND_FLOOR:
                return _SignedInfinity[sign]
            return _dec_from_triple(sign, '9' * context.prec,
                                    context.Emax - context.prec + 1)


class Underflow(Inexact, Rounded, Subnormal):
    pass


class FloatOperation(DecimalException, TypeError):
    pass


_signals = [Clamped, DivisionByZero, Inexact, Overflow, Rounded,
            Underflow, InvalidOperation, Subnormal, FloatOperation]

# The order libmpdec prints the signals in (by their status bits).
_REPR_ORDER = [Clamped, InvalidOperation, DivisionByZero, Inexact, FloatOperation,
               Overflow, Rounded, Subnormal, Underflow]

_condition_map = {ConversionSyntax: InvalidOperation,
                  DivisionImpossible: InvalidOperation,
                  DivisionUndefined: InvalidOperation,
                  InvalidContext: InvalidOperation}

_rounding_modes = (ROUND_DOWN, ROUND_HALF_UP, ROUND_HALF_EVEN, ROUND_CEILING,
                   ROUND_FLOOR, ROUND_UP, ROUND_HALF_DOWN, ROUND_05UP)


_current = [None]


def getcontext():
    if _current[0] is None:
        _current[0] = Context()
    return _current[0]


def setcontext(context):
    if context in (DefaultContext, BasicContext, ExtendedContext):
        context = context.copy()
        context.clear_flags()
    _current[0] = context


class _ContextManager:
    def __init__(self, new_context):
        self.new_context = new_context.copy()

    def __enter__(self):
        self.saved_context = getcontext()
        setcontext(self.new_context)
        return self.new_context

    def __exit__(self, t, v, tb):
        setcontext(self.saved_context)


def localcontext(ctx=None, **kwargs):
    if ctx is None:
        ctx = getcontext()
    ctx_manager = _ContextManager(ctx)
    for key, value in kwargs.items():
        if key == 'prec':
            ctx_manager.new_context.prec = _check_int(value, 1, MAX_PREC, 'prec')
        elif key == 'rounding':
            ctx_manager.new_context.rounding = value
        elif key in ('Emin', 'Emax', 'capitals', 'clamp'):
            setattr(ctx_manager.new_context, key, value)
        elif key == 'flags':
            ctx_manager.new_context.flags = _signal_dict(value)
        elif key == 'traps':
            ctx_manager.new_context.traps = _signal_dict(value)
        else:
            raise TypeError(f"'{key}' is an invalid keyword argument for this function")
    return ctx_manager


def _check_int(value, low, high, name):
    if not isinstance(value, int):
        raise TypeError('%s must be an integer' % name)
    if not low <= value <= high:
        raise ValueError('valid range for %s is [%d, %d]' % (name, low, high))
    return value


def _signal_dict(value):
    d = {s: False for s in _signals}
    if isinstance(value, dict):
        for k, v in value.items():
            d[k] = bool(v)
        return d
    for s in value:
        d[s] = True
    return d


_PyHASH_MODULUS = 2305843009213693951
_PyHASH_INF = 314159
_PyHASH_NAN = 0
_PyHASH_10INV = pow(10, _PyHASH_MODULUS - 2, _PyHASH_MODULUS)


class Decimal(object):
    """Floating point class for decimal arithmetic."""

    __slots__ = ('_exp', '_int', '_sign', '_is_special')
    __nova_type_name__ = 'decimal.Decimal'

    def __new__(cls, value="0", context=None):
        self = object.__new__(cls)

        if isinstance(value, str):
            m = _parser(value.strip().replace("_", ""))
            if m is None:
                if context is None:
                    context = getcontext()
                return context._raise_error(ConversionSyntax,
                                            "Invalid literal for Decimal: %r" % value)

            if m.group('sign') == "-":
                self._sign = 1
            else:
                self._sign = 0
            intpart = m.group('int')
            if intpart is not None:
                fracpart = m.group('frac') or ''
                exp = int(m.group('exp') or '0')
                self._int = str(int(intpart + fracpart))
                self._exp = exp - len(fracpart)
                self._is_special = False
            else:
                diag = m.group('diag')
                if diag is not None:
                    self._int = str(int(diag or '0')).lstrip('0')
                    if m.group('signal'):
                        self._exp = 'N'
                    else:
                        self._exp = 'n'
                else:
                    self._int = '0'
                    self._exp = 'F'
                self._is_special = True
            return self

        if isinstance(value, int):
            if value >= 0:
                self._sign = 0
            else:
                self._sign = 1
            self._exp = 0
            self._int = _digits(abs(value))
            self._is_special = False
            return self

        if isinstance(value, Decimal):
            self._exp = value._exp
            self._sign = value._sign
            self._int = value._int
            self._is_special = value._is_special
            return self

        if isinstance(value, (list, tuple)):
            if len(value) != 3:
                raise ValueError('Invalid tuple size in creation of Decimal '
                                 'from list or tuple.  The list or tuple '
                                 'should have exactly three elements.')
            if not (isinstance(value[0], int) and value[0] in (0, 1)):
                raise ValueError("Invalid sign.  The first value in the tuple "
                                 "should be an integer; either 0 for a "
                                 "positive number or 1 for a negative number.")
            self._sign = value[0]
            if value[2] == 'F':
                self._int = '0'
                self._exp = value[2]
                self._is_special = True
            else:
                digits = []
                for digit in value[1]:
                    if isinstance(digit, int) and 0 <= digit <= 9:
                        if digits or digit != 0:
                            digits.append(digit)
                    else:
                        raise ValueError("The second value in the tuple must "
                                         "be composed of integers in the range "
                                         "0 through 9.")
                if value[2] in ('n', 'N'):
                    self._int = ''.join(map(str, digits))
                    self._exp = value[2]
                    self._is_special = True
                elif isinstance(value[2], int):
                    self._int = ''.join(map(str, digits or [0]))
                    self._exp = value[2]
                    self._is_special = False
                else:
                    raise ValueError("The third value in the tuple must "
                                     "be an integer, or one of the "
                                     "strings 'F', 'n', 'N'.")
            return self

        if isinstance(value, float):
            if context is None:
                context = getcontext()
            context._raise_error(FloatOperation,
                                 "strict semantics for mixing floats and Decimals are "
                                 "enabled")
            value = Decimal.from_float(value)
            self._exp = value._exp
            self._sign = value._sign
            self._int = value._int
            self._is_special = value._is_special
            return self

        raise TypeError("conversion from %s to Decimal is not supported" % type(value).__name__)

    @classmethod
    def from_float(cls, f):
        if isinstance(f, int):
            sign = 0 if f >= 0 else 1
            k = 0
            coeff = str(abs(f))
        elif isinstance(f, float):
            if _math.isinf(f) or _math.isnan(f):
                return cls(repr(f))
            if _math.copysign(1.0, f) == 1.0:
                sign = 0
            else:
                sign = 1
            n, d = abs(f).as_integer_ratio()
            k = d.bit_length() - 1
            coeff = str(n * 5 ** k)
        else:
            raise TypeError("argument must be int or float.")

        result = _dec_from_triple(sign, coeff, -k)
        if cls is Decimal:
            return result
        else:
            return cls(result)

    @classmethod
    def from_number(cls, number):
        if isinstance(number, (int, Decimal, float)):
            return cls.from_float(number) if isinstance(number, float) else cls(number)
        raise TypeError("conversion from %s to Decimal is not supported" % type(number).__name__)

    def _isnan(self):
        if self._is_special:
            exp = self._exp
            if exp == 'n':
                return 1
            elif exp == 'N':
                return 2
        return 0

    def _isinfinity(self):
        if self._exp == 'F':
            if self._sign:
                return -1
            return 1
        return 0

    def _check_nans(self, other=None, context=None):
        self_is_nan = self._isnan()
        if other is None:
            other_is_nan = False
        else:
            other_is_nan = other._isnan()

        if self_is_nan or other_is_nan:
            if context is None:
                context = getcontext()

            if self_is_nan == 2:
                return context._raise_error(InvalidOperation, 'sNaN', self)
            if other_is_nan == 2:
                return context._raise_error(InvalidOperation, 'sNaN', other)
            if self_is_nan:
                return self._fix_nan(context)

            return other._fix_nan(context)
        return 0

    def _compare_check_nans(self, other, context):
        if context is None:
            context = getcontext()

        if self._is_special or other._is_special:
            if self.is_snan():
                return context._raise_error(InvalidOperation,
                                            'comparison involving sNaN', self)
            elif other.is_snan():
                return context._raise_error(InvalidOperation,
                                            'comparison involving sNaN', other)
            elif self.is_qnan():
                return context._raise_error(InvalidOperation,
                                            'comparison involving NaN', self)
            elif other.is_qnan():
                return context._raise_error(InvalidOperation,
                                            'comparison involving NaN', other)
        return 0

    def __bool__(self):
        return self._is_special or self._int != '0'

    def _cmp(self, other):
        if self._is_special or other._is_special:
            self_inf = self._isinfinity()
            other_inf = other._isinfinity()
            if self_inf == other_inf:
                return 0
            elif self_inf < other_inf:
                return -1
            else:
                return 1

        if not self:
            if not other:
                return 0
            else:
                return -((-1) ** other._sign)
        if not other:
            return (-1) ** self._sign

        if other._sign < self._sign:
            return -1
        if self._sign < other._sign:
            return 1

        self_adjusted = self.adjusted()
        other_adjusted = other.adjusted()
        if self_adjusted == other_adjusted:
            self_padded = self._int + '0' * (self._exp - other._exp)
            other_padded = other._int + '0' * (other._exp - self._exp)
            if self_padded == other_padded:
                return 0
            elif self_padded < other_padded:
                return -(-1) ** self._sign
            else:
                return (-1) ** self._sign
        elif self_adjusted > other_adjusted:
            return (-1) ** self._sign
        else:
            return -((-1) ** self._sign)

    def __eq__(self, other, context=None):
        self, other = _convert_for_comparison(self, other, equality_op=True)
        if other is NotImplemented:
            return other
        if self._check_nans(other, context):
            return False
        return self._cmp(other) == 0

    def __lt__(self, other, context=None):
        self, other = _convert_for_comparison(self, other)
        if other is NotImplemented:
            return other
        ans = self._compare_check_nans(other, context)
        if ans:
            return False
        return self._cmp(other) < 0

    def __le__(self, other, context=None):
        self, other = _convert_for_comparison(self, other)
        if other is NotImplemented:
            return other
        ans = self._compare_check_nans(other, context)
        if ans:
            return False
        return self._cmp(other) <= 0

    def __gt__(self, other, context=None):
        self, other = _convert_for_comparison(self, other)
        if other is NotImplemented:
            return other
        ans = self._compare_check_nans(other, context)
        if ans:
            return False
        return self._cmp(other) > 0

    def __ge__(self, other, context=None):
        self, other = _convert_for_comparison(self, other)
        if other is NotImplemented:
            return other
        ans = self._compare_check_nans(other, context)
        if ans:
            return False
        return self._cmp(other) >= 0

    def compare(self, other, context=None):
        other = _convert_other(other, raiseit=True)
        if (self._is_special or other and other._is_special):
            ans = self._check_nans(other, context)
            if ans:
                return ans
        return Decimal(self._cmp(other))

    def __hash__(self):
        if self._is_special:
            if self.is_snan():
                raise TypeError('Cannot hash a signaling NaN value.')
            elif self.is_nan():
                return object.__hash__(self)
            else:
                if self._sign:
                    return -_PyHASH_INF
                else:
                    return _PyHASH_INF

        if self._exp >= 0:
            exp_hash = pow(10, self._exp, _PyHASH_MODULUS)
        else:
            exp_hash = pow(_PyHASH_10INV, -self._exp, _PyHASH_MODULUS)
        hash_ = int(self._int) * exp_hash % _PyHASH_MODULUS
        ans = hash_ if self >= 0 else -hash_
        return -2 if ans == -1 else ans

    def as_tuple(self):
        return DecimalTuple(self._sign, tuple(map(int, self._int)), self._exp)

    def as_integer_ratio(self):
        if self._is_special:
            if self.is_nan():
                raise ValueError("cannot convert NaN to integer ratio")
            else:
                raise OverflowError("cannot convert Infinity to integer ratio")

        if not self:
            return 0, 1

        n = int(self._int)
        if self._exp >= 0:
            n, d = n * 10 ** self._exp, 1
        else:
            d5 = -self._exp
            while d5 > 0 and n % 5 == 0:
                n //= 5
                d5 -= 1

            d2 = -self._exp
            shift2 = min((n & -n).bit_length() - 1, d2)
            if shift2:
                n >>= shift2
                d2 -= shift2

            d = 5 ** d5 << d2

        if self._sign:
            n = -n
        return n, d

    def __repr__(self):
        return "Decimal('%s')" % str(self)

    def __str__(self, eng=False, context=None):
        sign = ['', '-'][self._sign]
        if self._is_special:
            if self._exp == 'F':
                return sign + 'Infinity'
            elif self._exp == 'n':
                return sign + 'NaN' + self._int
            else:
                return sign + 'sNaN' + self._int

        leftdigits = self._exp + len(self._int)

        if self._exp <= 0 and leftdigits > -6:
            dotplace = leftdigits
        elif not eng:
            dotplace = 1
        elif self._int == '0':
            dotplace = (leftdigits + 1) % 3 - 1
        else:
            dotplace = (leftdigits - 1) % 3 + 1

        if dotplace <= 0:
            intpart = '0'
            fracpart = '.' + '0' * (-dotplace) + self._int
        elif dotplace >= len(self._int):
            intpart = self._int + '0' * (dotplace - len(self._int))
            fracpart = ''
        else:
            intpart = self._int[:dotplace]
            fracpart = '.' + self._int[dotplace:]
        if leftdigits == dotplace:
            exp = ''
        else:
            if context is None:
                context = getcontext()
            exp = ['e', 'E'][context.capitals] + "%+d" % (leftdigits - dotplace)

        return sign + intpart + fracpart + exp

    def to_eng_string(self, context=None):
        return self.__str__(eng=True, context=context)

    def __neg__(self, context=None):
        if self._is_special:
            ans = self._check_nans(context=context)
            if ans:
                return ans

        if context is None:
            context = getcontext()

        if not self and context.rounding != ROUND_FLOOR:
            ans = self.copy_abs()
        else:
            ans = self.copy_negate()

        return ans._fix(context)

    def __pos__(self, context=None):
        if self._is_special:
            ans = self._check_nans(context=context)
            if ans:
                return ans

        if context is None:
            context = getcontext()

        if not self and context.rounding != ROUND_FLOOR:
            ans = self.copy_abs()
        else:
            ans = Decimal(self)

        return ans._fix(context)

    def __abs__(self, round=True, context=None):
        if not round:
            return self.copy_abs()

        if self._is_special:
            ans = self._check_nans(context=context)
            if ans:
                return ans

        if self._sign:
            ans = self.__neg__(context=context)
        else:
            ans = self.__pos__(context=context)

        return ans

    def __add__(self, other, context=None):
        other = _convert_other(other)
        if other is NotImplemented:
            return other

        if context is None:
            context = getcontext()

        if self._is_special or other._is_special:
            ans = self._check_nans(other, context)
            if ans:
                return ans

            if self._isinfinity():
                if self._sign != other._sign and other._isinfinity():
                    return context._raise_error(InvalidOperation, '-INF + INF')
                return Decimal(self)
            if other._isinfinity():
                return Decimal(other)

        exp = min(self._exp, other._exp)
        negativezero = 0
        if context.rounding == ROUND_FLOOR and self._sign != other._sign:
            negativezero = 1

        if not self and not other:
            sign = min(self._sign, other._sign)
            if negativezero:
                sign = 1
            ans = _dec_from_triple(sign, '0', exp)
            ans = ans._fix(context)
            return ans
        if not self:
            exp = max(exp, other._exp - context.prec - 1)
            ans = other._rescale(exp, context.rounding)
            ans = ans._fix(context)
            return ans
        if not other:
            exp = max(exp, self._exp - context.prec - 1)
            ans = self._rescale(exp, context.rounding)
            ans = ans._fix(context)
            return ans

        op1 = _WorkRep(self)
        op2 = _WorkRep(other)
        op1, op2 = _normalize(op1, op2, context.prec)

        result = _WorkRep()
        if op1.sign != op2.sign:
            if op1.int == op2.int:
                ans = _dec_from_triple(negativezero, '0', exp)
                ans = ans._fix(context)
                return ans
            if op1.int < op2.int:
                op1, op2 = op2, op1
            if op1.sign == 1:
                result.sign = 1
                op1.sign, op2.sign = op2.sign, op1.sign
            else:
                result.sign = 0
        elif op1.sign == 1:
            result.sign = 1
            op1.sign, op2.sign = (0, 0)
        else:
            result.sign = 0

        if op2.sign == 0:
            result.int = op1.int + op2.int
        else:
            result.int = op1.int - op2.int

        result.exp = op1.exp
        ans = Decimal(result)
        ans = ans._fix(context)
        return ans

    __radd__ = __add__

    def __sub__(self, other, context=None):
        other = _convert_other(other)
        if other is NotImplemented:
            return other

        if self._is_special or other._is_special:
            ans = self._check_nans(other, context=context)
            if ans:
                return ans

        return self.__add__(other.copy_negate(), context=context)

    def __rsub__(self, other, context=None):
        other = _convert_other(other)
        if other is NotImplemented:
            return other

        return other.__sub__(self, context=context)

    def __mul__(self, other, context=None):
        other = _convert_other(other)
        if other is NotImplemented:
            return other

        if context is None:
            context = getcontext()

        resultsign = self._sign ^ other._sign

        if self._is_special or other._is_special:
            ans = self._check_nans(other, context)
            if ans:
                return ans

            if self._isinfinity():
                if not other:
                    return context._raise_error(InvalidOperation, '(+-)INF * 0')
                return _SignedInfinity[resultsign]

            if other._isinfinity():
                if not self:
                    return context._raise_error(InvalidOperation, '0 * (+-)INF')
                return _SignedInfinity[resultsign]

        resultexp = self._exp + other._exp

        if not self or not other:
            ans = _dec_from_triple(resultsign, '0', resultexp)
            ans = ans._fix(context)
            return ans

        ans = _dec_from_triple(resultsign, _digits(_intof(self._int) * _intof(other._int)), resultexp)
        ans = ans._fix(context)

        return ans

    __rmul__ = __mul__

    def __truediv__(self, other, context=None):
        other = _convert_other(other)
        if other is NotImplemented:
            return NotImplemented

        if context is None:
            context = getcontext()

        sign = self._sign ^ other._sign

        if self._is_special or other._is_special:
            ans = self._check_nans(other, context)
            if ans:
                return ans

            if self._isinfinity() and other._isinfinity():
                return context._raise_error(InvalidOperation, '(+-)INF/(+-)INF')

            if self._isinfinity():
                return _SignedInfinity[sign]

            if other._isinfinity():
                context._raise_error(Clamped, 'Division by infinity')
                return _dec_from_triple(sign, '0', context.Etiny())

        if not other:
            if not self:
                return context._raise_error(DivisionUndefined, '0 / 0')
            return context._raise_error(DivisionByZero, 'x / 0', sign)

        if not self:
            exp = self._exp - other._exp
            coeff = 0
        else:
            shift = len(other._int) - len(self._int) + context.prec + 1
            exp = self._exp - other._exp - shift
            op1 = _WorkRep(self)
            op2 = _WorkRep(other)
            if shift >= 0:
                coeff, remainder = divmod(op1.int * 10 ** shift, op2.int)
            else:
                coeff, remainder = divmod(op1.int, op2.int * 10 ** -shift)
            if remainder:
                if coeff % 5 == 0:
                    coeff += 1
            else:
                ideal_exp = self._exp - other._exp
                while exp < ideal_exp and coeff % 10 == 0:
                    coeff //= 10
                    exp += 1

        ans = _dec_from_triple(sign, str(coeff), exp)
        return ans._fix(context)

    def _divide(self, other, context):
        sign = self._sign ^ other._sign
        if other._isinfinity():
            ideal_exp = self._exp
        else:
            ideal_exp = min(self._exp, other._exp)

        expdiff = self.adjusted() - other.adjusted()
        if not self or other._isinfinity() or expdiff <= -2:
            return (_dec_from_triple(sign, '0', 0),
                    self._rescale(ideal_exp, context.rounding))
        if expdiff <= context.prec:
            op1 = _WorkRep(self)
            op2 = _WorkRep(other)
            if op1.exp >= op2.exp:
                op1.int *= 10 ** (op1.exp - op2.exp)
            else:
                op2.int *= 10 ** (op2.exp - op1.exp)
            q, r = divmod(op1.int, op2.int)
            if q < 10 ** context.prec:
                return (_dec_from_triple(sign, str(q), 0),
                        _dec_from_triple(self._sign, str(r), ideal_exp))

        ans = context._raise_error(DivisionImpossible,
                                   'quotient too large in //, % or divmod')
        return ans, ans

    def __rtruediv__(self, other, context=None):
        other = _convert_other(other)
        if other is NotImplemented:
            return other
        return other.__truediv__(self, context=context)

    def __divmod__(self, other, context=None):
        other = _convert_other(other)
        if other is NotImplemented:
            return other

        if context is None:
            context = getcontext()

        ans = self._check_nans(other, context)
        if ans:
            return (ans, ans)

        sign = self._sign ^ other._sign
        if self._isinfinity():
            if other._isinfinity():
                ans = context._raise_error(InvalidOperation, 'divmod(INF, INF)')
                return ans, ans
            else:
                return (_SignedInfinity[sign],
                        context._raise_error(InvalidOperation, 'INF % x'))

        if not other:
            if not self:
                ans = context._raise_error(DivisionUndefined, 'divmod(0, 0)')
                return ans, ans
            else:
                return (context._raise_error(DivisionByZero, 'x // 0', sign),
                        context._raise_error(InvalidOperation, 'x % 0'))

        quotient, remainder = self._divide(other, context)
        remainder = remainder._fix(context)
        return quotient, remainder

    def __rdivmod__(self, other, context=None):
        other = _convert_other(other)
        if other is NotImplemented:
            return other
        return other.__divmod__(self, context=context)

    def __mod__(self, other, context=None):
        other = _convert_other(other)
        if other is NotImplemented:
            return other

        if context is None:
            context = getcontext()

        ans = self._check_nans(other, context)
        if ans:
            return ans

        if self._isinfinity():
            return context._raise_error(InvalidOperation, 'INF % x')
        elif not other:
            if self:
                return context._raise_error(InvalidOperation, 'x % 0')
            else:
                return context._raise_error(DivisionUndefined, '0 % 0')

        remainder = self._divide(other, context)[1]
        remainder = remainder._fix(context)
        return remainder

    def __rmod__(self, other, context=None):
        other = _convert_other(other)
        if other is NotImplemented:
            return other
        return other.__mod__(self, context=context)

    def remainder_near(self, other, context=None):
        if context is None:
            context = getcontext()

        other = _convert_other(other, raiseit=True)

        ans = self._check_nans(other, context)
        if ans:
            return ans

        if self._isinfinity():
            return context._raise_error(InvalidOperation, 'remainder_near(infinity, x)')

        if not other:
            if self:
                return context._raise_error(InvalidOperation, 'remainder_near(x, 0)')
            else:
                return context._raise_error(DivisionUndefined, 'remainder_near(0, 0)')

        if other._isinfinity():
            ans = Decimal(self)
            return ans._fix(context)

        ideal_exponent = min(self._exp, other._exp)
        if not self:
            ans = _dec_from_triple(self._sign, '0', ideal_exponent)
            return ans._fix(context)

        expdiff = self.adjusted() - other.adjusted()
        if expdiff >= context.prec + 1:
            return context._raise_error(DivisionImpossible)
        if expdiff <= -2:
            ans = self._rescale(ideal_exponent, context.rounding)
            return ans._fix(context)

        op1 = _WorkRep(self)
        op2 = _WorkRep(other)
        if op1.exp >= op2.exp:
            op1.int *= 10 ** (op1.exp - op2.exp)
        else:
            op2.int *= 10 ** (op2.exp - op1.exp)
        q, r = divmod(op1.int, op2.int)
        if 2 * r + (q & 1) > op2.int:
            r -= op2.int
            q += 1

        if q >= 10 ** context.prec:
            return context._raise_error(DivisionImpossible)

        sign = self._sign
        if r < 0:
            sign = 1 - sign
            r = -r

        ans = _dec_from_triple(sign, str(r), ideal_exponent)
        return ans._fix(context)

    def __floordiv__(self, other, context=None):
        other = _convert_other(other)
        if other is NotImplemented:
            return other

        if context is None:
            context = getcontext()

        ans = self._check_nans(other, context)
        if ans:
            return ans

        if self._isinfinity():
            if other._isinfinity():
                return context._raise_error(InvalidOperation, 'INF // INF')
            else:
                return _SignedInfinity[self._sign ^ other._sign]

        if not other:
            if self:
                return context._raise_error(DivisionByZero, 'x // 0',
                                            self._sign ^ other._sign)
            else:
                return context._raise_error(DivisionUndefined, '0 // 0')

        return self._divide(other, context)[0]

    def __rfloordiv__(self, other, context=None):
        other = _convert_other(other)
        if other is NotImplemented:
            return other
        return other.__floordiv__(self, context=context)

    def __float__(self):
        if self._isnan():
            if self.is_snan():
                raise ValueError("Cannot convert signaling NaN to float")
            s = "-nan" if self._sign else "nan"
        else:
            s = str(self)
        return float(s)

    def __int__(self):
        if self._is_special:
            if self._isnan():
                raise ValueError("Cannot convert NaN to integer")
            elif self._isinfinity():
                raise OverflowError("Cannot convert infinity to integer")
        s = (-1) ** self._sign
        if self._exp >= 0:
            return s * int(self._int) * 10 ** self._exp
        else:
            return s * int(self._int[:self._exp] or '0')

    __trunc__ = __int__

    @property
    def real(self):
        return self

    @property
    def imag(self):
        return Decimal(0)

    def conjugate(self):
        return self

    def __complex__(self):
        return complex(float(self))

    def _fix_nan(self, context):
        payload = self._int
        max_payload_len = context.prec - context.clamp
        if len(payload) > max_payload_len:
            payload = payload[len(payload) - max_payload_len:].lstrip('0')
            return _dec_from_triple(self._sign, payload, self._exp, True)
        return Decimal(self)

    def _fix(self, context):
        if self._is_special:
            if self._isnan():
                return self._fix_nan(context)
            else:
                return Decimal(self)

        Etiny = context.Etiny()
        Etop = context.Etop()
        if not self:
            exp_max = [context.Emax, Etop][context.clamp]
            new_exp = min(max(self._exp, Etiny), exp_max)
            if new_exp != self._exp:
                context._raise_error(Clamped)
                return _dec_from_triple(self._sign, '0', new_exp)
            else:
                return Decimal(self)

        exp_min = len(self._int) + self._exp - context.prec
        if exp_min > Etop:
            ans = context._raise_error(Overflow, 'above Emax', self._sign)
            context._raise_error(Inexact)
            context._raise_error(Rounded)
            return ans

        self_is_subnormal = exp_min < Etiny
        if self_is_subnormal:
            exp_min = Etiny

        if self._exp < exp_min:
            digits = len(self._int) + self._exp - exp_min
            if digits < 0:
                self = _dec_from_triple(self._sign, '1', exp_min - 1)
                digits = 0
            rounding_method = _ROUNDERS[context.rounding]
            changed = rounding_method(self, digits)
            coeff = self._int[:digits] or '0'
            if changed > 0:
                coeff = str(int(coeff) + 1)
                if len(coeff) > context.prec:
                    coeff = coeff[:-1]
                    exp_min += 1

            if exp_min > Etop:
                ans = context._raise_error(Overflow, 'above Emax', self._sign)
            else:
                ans = _dec_from_triple(self._sign, coeff, exp_min)

            if changed and self_is_subnormal:
                context._raise_error(Underflow)
            if self_is_subnormal:
                context._raise_error(Subnormal)
            if changed:
                context._raise_error(Inexact)
            context._raise_error(Rounded)
            if not ans:
                context._raise_error(Clamped)
            return ans

        if self_is_subnormal:
            context._raise_error(Subnormal)

        if context.clamp == 1 and self._exp > Etop:
            context._raise_error(Clamped)
            self_padded = self._int + '0' * (self._exp - Etop)
            return _dec_from_triple(self._sign, self_padded, Etop)

        return Decimal(self)

    def _round_down(self, prec):
        if _all_zeros(self._int, prec):
            return 0
        else:
            return -1

    def _round_up(self, prec):
        return -self._round_down(prec)

    def _round_half_up(self, prec):
        if self._int[prec] in '56789':
            return 1
        elif _all_zeros(self._int, prec):
            return 0
        else:
            return -1

    def _round_half_down(self, prec):
        if _exact_half(self._int, prec):
            return -1
        else:
            return self._round_half_up(prec)

    def _round_half_even(self, prec):
        if _exact_half(self._int, prec) and \
                (prec == 0 or self._int[prec - 1] in '02468'):
            return -1
        else:
            return self._round_half_up(prec)

    def _round_ceiling(self, prec):
        if self._sign:
            return self._round_down(prec)
        else:
            return -self._round_down(prec)

    def _round_floor(self, prec):
        if not self._sign:
            return self._round_down(prec)
        else:
            return -self._round_down(prec)

    def _round_05up(self, prec):
        if prec and self._int[prec - 1] not in '05':
            return self._round_down(prec)
        else:
            return -self._round_down(prec)

    def __round__(self, n=None):
        if n is not None:
            if not isinstance(n, int):
                raise TypeError('Second argument to round should be integral')
            exp = _dec_from_triple(0, '1', -n)
            return self.quantize(exp)

        if self._is_special:
            if self.is_nan():
                raise ValueError("cannot round a NaN")
            else:
                raise OverflowError("cannot round an infinity")
        return int(self._rescale(0, ROUND_HALF_EVEN))

    def __floor__(self):
        if self._is_special:
            if self.is_nan():
                raise ValueError("cannot round a NaN")
            else:
                raise OverflowError("cannot round an infinity")
        return int(self._rescale(0, ROUND_FLOOR))

    def __ceil__(self):
        if self._is_special:
            if self.is_nan():
                raise ValueError("cannot round a NaN")
            else:
                raise OverflowError("cannot round an infinity")
        return int(self._rescale(0, ROUND_CEILING))

    def fma(self, other, third, context=None):
        other = _convert_other(other, raiseit=True)
        third = _convert_other(third, raiseit=True)
        if context is None:
            context = getcontext()
        if self._is_special or other._is_special:
            if self.is_snan():
                return context._raise_error(InvalidOperation, 'sNaN', self)
            if other.is_snan():
                return context._raise_error(InvalidOperation, 'sNaN', other)
            if self._isnan():
                product = self
            elif other._isnan():
                product = other
            elif self._exp == 'F':
                if not other:
                    return context._raise_error(InvalidOperation, 'INF * 0 in fma')
                product = _SignedInfinity[self._sign ^ other._sign]
            elif other._exp == 'F':
                if not self:
                    return context._raise_error(InvalidOperation, '0 * INF in fma')
                product = _SignedInfinity[self._sign ^ other._sign]
        else:
            product = _dec_from_triple(self._sign ^ other._sign,
                                       _digits(_intof(self._int) * _intof(other._int)),
                                       self._exp + other._exp)

        return product.__add__(third, context)

    def __pow__(self, other, modulo=None, context=None):
        if modulo is not None:
            return self._power_modulo(other, modulo, context)

        other = _convert_other(other)
        if other is NotImplemented:
            return other

        if context is None:
            context = getcontext()

        ans = self._check_nans(other, context)
        if ans:
            return ans

        if not other:
            if not self:
                return context._raise_error(InvalidOperation, '0 ** 0')
            else:
                return _One

        result_sign = 0
        if self._sign == 1:
            if other._isinteger():
                if not other._iseven():
                    result_sign = 1
            else:
                if self:
                    return context._raise_error(InvalidOperation,
                                                'x ** y with x negative and y not an integer')
            self = self.copy_negate()

        if not self:
            if other._sign == 0:
                return _dec_from_triple(result_sign, '0', 0)
            else:
                return _SignedInfinity[result_sign]

        if self._isinfinity():
            if other._sign == 0:
                return _SignedInfinity[result_sign]
            else:
                return _dec_from_triple(result_sign, '0', 0)

        if self == _One:
            if other._isinteger():
                if other._sign == 1:
                    multiplier = 0
                elif other > context.prec:
                    multiplier = context.prec
                else:
                    multiplier = int(other)

                exp = self._exp * multiplier
                if exp < 1 - context.prec:
                    exp = 1 - context.prec
                    context._raise_error(Rounded)
            else:
                context._raise_error(Inexact)
                context._raise_error(Rounded)
                exp = 1 - context.prec

            return _dec_from_triple(result_sign, '1' + '0' * -exp, exp)

        if other._isinfinity():
            if (other._sign == 0) == (self.adjusted() < 0):
                return _dec_from_triple(result_sign, '0', 0)
            else:
                return _SignedInfinity[result_sign]

        if other._isinteger():
            n = int(other)
            # A result far out of range: no need for its digits.
            lead = float(_dec_from_triple(0, self._int[:17], 1 - min(len(self._int), 17)))
            estimate = n * (_math.log10(lead) + self.adjusted())
            if estimate > context.Emax + context.prec + 2:
                ans = _dec_from_triple(result_sign, '1', context.Emax + 1)
                return ans._fix(context)
            if estimate < context.Etiny() - context.prec - 2:
                ans = _dec_from_triple(result_sign, '1', context.Etiny() - 2)
                return ans._fix(context)
            if n >= 0:
                coeff = _intof(self._int) ** n
                ans = _dec_from_triple(result_sign, _digits(coeff), self._exp * n)
                return ans._fix(context)
            m = -n
            c = _intof(self._int) ** m
            return _rational(result_sign, 1, c, -self._exp * m, context, ideal_exp=self._exp * n)

        # Non-integer power: exp(y * ln(x)), rounded once; exact when x
        # is a perfect power for y's denominator.
        x = _to_fraction(self)
        y = _to_fraction(other)
        exact = _exact_power(x, y, context)
        if exact is not None:
            # libmpdec gives an exact non-integer power all its digits.
            pad = context.prec - len(exact._int)
            if pad > 0 and exact:
                exact = _dec_from_triple(exact._sign, exact._int + '0' * pad, exact._exp - pad)
            if result_sign:
                exact = exact.copy_negate()
            return exact._fix(context)

        def compute(p):
            return _exp_fixed(_mul_fixed(_ln_fixed(x, p + 5), y, p + 5) // 10 ** 5, p)
        return _ziv(result_sign, compute, context)

    def __rpow__(self, other, context=None):
        other = _convert_other(other)
        if other is NotImplemented:
            return other
        return other.__pow__(self, context=context)

    def _power_modulo(self, other, modulo, context=None):
        if context is None:
            context = getcontext()

        other = _convert_other(other, raiseit=True)
        modulo = _convert_other(modulo, raiseit=True)

        if not (self._isinteger() and other._isinteger() and modulo._isinteger()):
            return context._raise_error(InvalidOperation,
                                        'pow() 3rd argument not allowed '
                                        'unless all arguments are integers')
        if other < 0:
            return context._raise_error(InvalidOperation,
                                        'pow() 2nd argument cannot be '
                                        'negative when 3rd argument specified')
        if not modulo:
            return context._raise_error(InvalidOperation,
                                        'pow() 3rd argument cannot be 0')
        if not other and not self:
            return context._raise_error(InvalidOperation,
                                        'at least one of pow() 1st argument '
                                        'and 2nd argument must be nonzero; '
                                        '0**0 is not defined')
        if other._iseven():
            sign = 0
        else:
            sign = self._sign

        modulo = abs(int(modulo))
        base = _WorkRep(self.to_integral_value())
        exponent = _WorkRep(other.to_integral_value())

        base = (base.int % modulo * pow(10, base.exp, modulo)) % modulo
        for i in range(exponent.exp):
            base = pow(base, 10, modulo)
        base = pow(base, exponent.int, modulo)

        return _dec_from_triple(sign, str(base), 0)

    def normalize(self, context=None):
        if context is None:
            context = getcontext()

        if self._is_special:
            ans = self._check_nans(context=context)
            if ans:
                return ans

        dup = self._fix(context)
        if dup._isinfinity():
            return dup

        if not dup:
            return _dec_from_triple(dup._sign, '0', 0)
        exp_max = [context.Emax, context.Etop()][context.clamp]
        end = len(dup._int)
        exp = dup._exp
        while dup._int[end - 1] == '0' and exp < exp_max:
            exp += 1
            end -= 1
        return _dec_from_triple(dup._sign, dup._int[:end], exp)

    def quantize(self, exp, rounding=None, context=None):
        exp = _convert_other(exp, raiseit=True)

        if context is None:
            context = getcontext()
        if rounding is None:
            rounding = context.rounding

        if self._is_special or exp._is_special:
            ans = self._check_nans(exp, context)
            if ans:
                return ans

            if exp._isinfinity() or self._isinfinity():
                if exp._isinfinity() and self._isinfinity():
                    return Decimal(self)
                return context._raise_error(InvalidOperation,
                                            'quantize with one INF')

        if not (context.Etiny() <= exp._exp <= context.Emax):
            return context._raise_error(InvalidOperation,
                                        'target exponent out of bounds in quantize')

        if not self:
            ans = _dec_from_triple(self._sign, '0', exp._exp)
            return ans._fix(context)

        self_adjusted = self.adjusted()
        if self_adjusted > context.Emax:
            return context._raise_error(InvalidOperation,
                                        'exponent of quantize result too large for current context')
        if self_adjusted - exp._exp + 1 > context.prec:
            return context._raise_error(InvalidOperation,
                                        'quantize result has too many digits for current context')

        ans = self._rescale(exp._exp, rounding)
        if ans.adjusted() > context.Emax:
            return context._raise_error(InvalidOperation,
                                        'exponent of quantize result too large for current context')
        if len(ans._int) > context.prec:
            return context._raise_error(InvalidOperation,
                                        'quantize result has too many digits for current context')

        if ans and ans.adjusted() < context.Emin:
            context._raise_error(Subnormal)
        if ans._exp > self._exp:
            if ans != self:
                context._raise_error(Inexact)
            context._raise_error(Rounded)

        ans = ans._fix(context)
        return ans

    def same_quantum(self, other, context=None):
        other = _convert_other(other, raiseit=True)
        if self._is_special or other._is_special:
            return (self.is_nan() and other.is_nan() or
                    self.is_infinite() and other.is_infinite())
        return self._exp == other._exp

    def _rescale(self, exp, rounding):
        if self._is_special:
            return Decimal(self)
        if not self:
            return _dec_from_triple(self._sign, '0', exp)

        if self._exp >= exp:
            return _dec_from_triple(self._sign,
                                    self._int + '0' * (self._exp - exp), exp)

        digits = len(self._int) + self._exp - exp
        if digits < 0:
            self = _dec_from_triple(self._sign, '1', exp - 1)
            digits = 0
        this_function = _ROUNDERS[rounding]
        changed = this_function(self, digits)
        coeff = self._int[:digits] or '0'
        if changed == 1:
            coeff = str(int(coeff) + 1)
        return _dec_from_triple(self._sign, coeff, exp)

    def _round(self, places, rounding):
        if places <= 0:
            raise ValueError("argument should be at least 1 in _round")
        if self._is_special or not self:
            return Decimal(self)
        ans = self._rescale(self.adjusted() + 1 - places, rounding)
        if ans.adjusted() != self.adjusted():
            ans = ans._rescale(ans.adjusted() + 1 - places, rounding)
        return ans

    def to_integral_exact(self, rounding=None, context=None):
        if self._is_special:
            ans = self._check_nans(context=context)
            if ans:
                return ans
            return Decimal(self)
        if self._exp >= 0:
            return Decimal(self)
        if not self:
            return _dec_from_triple(self._sign, '0', 0)
        if context is None:
            context = getcontext()
        if rounding is None:
            rounding = context.rounding
        ans = self._rescale(0, rounding)
        if ans != self:
            context._raise_error(Inexact)
        context._raise_error(Rounded)
        return ans

    def to_integral_value(self, rounding=None, context=None):
        if context is None:
            context = getcontext()
        if rounding is None:
            rounding = context.rounding
        if self._is_special:
            ans = self._check_nans(context=context)
            if ans:
                return ans
            return Decimal(self)
        if self._exp >= 0:
            return Decimal(self)
        else:
            return self._rescale(0, rounding)

    to_integral = to_integral_value

    def sqrt(self, context=None):
        if context is None:
            context = getcontext()

        if self._is_special:
            ans = self._check_nans(context=context)
            if ans:
                return ans

            if self._isinfinity() and self._sign == 0:
                return Decimal(self)

        if not self:
            ans = _dec_from_triple(self._sign, '0', self._exp // 2)
            return ans._fix(context)

        if self._sign == 1:
            return context._raise_error(InvalidOperation, 'sqrt(-x), x > 0')

        prec = context.prec + 1

        e = self._exp >> 1
        if self._exp & 1:
            c = int(self._int) * 10
            l = (len(self._int) >> 1) + 1
        else:
            c = int(self._int)
            l = len(self._int) + 1 >> 1

        shift = prec - l
        if shift >= 0:
            c *= 100 ** shift
            exact = True
        else:
            c, remainder = divmod(c, 100 ** -shift)
            exact = not remainder
        e -= shift

        n = _math.isqrt(c)
        exact = exact and n * n == c

        if exact:
            if shift >= 0:
                n //= 10 ** shift
            else:
                n *= 10 ** -shift
            e += shift
        else:
            if n % 5 == 0:
                n += 1

        ans = _dec_from_triple(0, str(n), e)

        context = context._shallow_copy()
        rounding = context._set_rounding(ROUND_HALF_EVEN)
        ans = ans._fix(context)
        context.rounding = rounding

        return ans

    def max(self, other, context=None):
        other = _convert_other(other, raiseit=True)
        if context is None:
            context = getcontext()
        if self._is_special or other._is_special:
            sn = self._isnan()
            on = other._isnan()
            if sn or on:
                if on == 1 and sn == 0:
                    return self._fix(context)
                if sn == 1 and on == 0:
                    return other._fix(context)
                return self._check_nans(other, context)
        c = self._cmp(other)
        if c == 0:
            c = self.compare_total(other)
        if c == -1:
            ans = other
        else:
            ans = self
        return ans._fix(context)

    def min(self, other, context=None):
        other = _convert_other(other, raiseit=True)
        if context is None:
            context = getcontext()
        if self._is_special or other._is_special:
            sn = self._isnan()
            on = other._isnan()
            if sn or on:
                if on == 1 and sn == 0:
                    return self._fix(context)
                if sn == 1 and on == 0:
                    return other._fix(context)
                return self._check_nans(other, context)
        c = self._cmp(other)
        if c == 0:
            c = self.compare_total(other)
        if c == -1:
            ans = self
        else:
            ans = other
        return ans._fix(context)

    def _isinteger(self):
        if self._is_special:
            return False
        if self._exp >= 0:
            return True
        rest = self._int[self._exp:]
        return rest == '0' * len(rest)

    def _iseven(self):
        if not self or self._exp > 0:
            return True
        return self._int[-1 + self._exp] in '02468'

    def adjusted(self):
        try:
            return self._exp + len(self._int) - 1
        except TypeError:
            return 0

    def canonical(self):
        return self

    def compare_signal(self, other, context=None):
        other = _convert_other(other, raiseit=True)
        ans = self._compare_check_nans(other, context)
        if ans:
            return ans
        return self.compare(other, context=context)

    def compare_total(self, other, context=None):
        other = _convert_other(other, raiseit=True)

        if self._sign and not other._sign:
            return _NegativeOne
        if not self._sign and other._sign:
            return _One
        sign = self._sign

        self_nan = self._isnan()
        other_nan = other._isnan()
        if self_nan or other_nan:
            if self_nan == other_nan:
                self_key = len(self._int), self._int
                other_key = len(other._int), other._int
                if self_key < other_key:
                    if sign:
                        return _One
                    else:
                        return _NegativeOne
                if self_key > other_key:
                    if sign:
                        return _NegativeOne
                    else:
                        return _One
                return _Zero

            if sign:
                if self_nan == 1:
                    return _NegativeOne
                if other_nan == 1:
                    return _One
                if self_nan == 2:
                    return _NegativeOne
                if other_nan == 2:
                    return _One
            else:
                if self_nan == 1:
                    return _One
                if other_nan == 1:
                    return _NegativeOne
                if self_nan == 2:
                    return _One
                if other_nan == 2:
                    return _NegativeOne

        if self < other:
            return _NegativeOne
        if self > other:
            return _One

        if self._exp < other._exp:
            if sign:
                return _One
            else:
                return _NegativeOne
        if self._exp > other._exp:
            if sign:
                return _NegativeOne
            else:
                return _One
        return _Zero

    def compare_total_mag(self, other, context=None):
        other = _convert_other(other, raiseit=True)
        s = self.copy_abs()
        o = other.copy_abs()
        return s.compare_total(o)

    def copy_abs(self):
        return _dec_from_triple(0, self._int, self._exp, self._is_special)

    def copy_negate(self):
        if self._sign:
            return _dec_from_triple(0, self._int, self._exp, self._is_special)
        else:
            return _dec_from_triple(1, self._int, self._exp, self._is_special)

    def copy_sign(self, other, context=None):
        other = _convert_other(other, raiseit=True)
        return _dec_from_triple(other._sign, self._int,
                                self._exp, self._is_special)

    def exp(self, context=None):
        if context is None:
            context = getcontext()
        ans = self._check_nans(context=context)
        if ans:
            return ans
        if self._isinfinity() == -1:
            return _Zero
        if not self:
            return _One
        if self._isinfinity() == 1:
            return Decimal(self)
        adj = self.adjusted()
        if self._sign == 0 and adj > len(str((context.Emax + 1) * 3)):
            ans = _dec_from_triple(0, '1', context.Emax + 1)
            return ans._fix(context)
        if self._sign == 1 and adj > len(str((-context.Etiny() + 1) * 3)):
            ans = _dec_from_triple(0, '1', context.Etiny() - 1)
            return ans._fix(context)
        x = _to_fraction(self)
        return _ziv(0, lambda p: _exp_fixed(_frac_fixed(x, p + 5), p + 5) // 10 ** 5, context, ROUND_HALF_EVEN)

    def ln(self, context=None):
        if context is None:
            context = getcontext()
        ans = self._check_nans(context=context)
        if ans:
            return ans
        if not self:
            return _NegativeInfinity
        if self._isinfinity() == 1:
            return _Infinity
        if self == _One:
            return _Zero
        if self._sign == 1:
            return context._raise_error(InvalidOperation, 'ln of a negative value')
        x = _to_fraction(self)
        return _ziv(0, lambda p: _ln_fixed(x, p), context, ROUND_HALF_EVEN)

    def log10(self, context=None):
        if context is None:
            context = getcontext()
        ans = self._check_nans(context=context)
        if ans:
            return ans
        if not self:
            return _NegativeInfinity
        if self._isinfinity() == 1:
            return _Infinity
        if self._sign == 1:
            return context._raise_error(InvalidOperation, 'log10 of a negative value')
        if self._int[0] == '1' and self._int[1:] == '0' * (len(self._int) - 1):
            ans = Decimal(self._exp + len(self._int) - 1)
        else:
            x = _to_fraction(self)
            return _ziv(0, lambda p: _ln_fixed(x, p + 5) * 10 ** (p + 5) // _ln10_fixed(p + 5) // 10 ** 5, context, ROUND_HALF_EVEN)
        context = context._shallow_copy()
        rounding = context._set_rounding(ROUND_HALF_EVEN)
        ans = ans._fix(context)
        context.rounding = rounding
        return ans

    def logb(self, context=None):
        ans = self._check_nans(context=context)
        if ans:
            return ans
        if context is None:
            context = getcontext()
        if self._isinfinity():
            return _Infinity
        if not self:
            return context._raise_error(DivisionByZero, 'logb(0)', 1)
        ans = Decimal(self.adjusted())
        return ans._fix(context)

    def scaleb(self, other, context=None):
        if context is None:
            context = getcontext()
        other = _convert_other(other, raiseit=True)
        ans = self._check_nans(other, context)
        if ans:
            return ans
        if other._exp != 0:
            return context._raise_error(InvalidOperation)
        liminf = -2 * (context.Emax + context.prec)
        limsup = 2 * (context.Emax + context.prec)
        if not (liminf <= int(other) <= limsup):
            return context._raise_error(InvalidOperation)
        if self._isinfinity():
            return Decimal(self)
        d = _dec_from_triple(self._sign, self._int, self._exp + int(other))
        d = d._fix(context)
        return d

    def next_plus(self, context=None):
        if context is None:
            context = getcontext()
        ans = self._check_nans(context=context)
        if ans:
            return ans
        if self._isinfinity() == 1:
            return _Infinity
        if self._isinfinity() == -1:
            return _dec_from_triple(1, '9' * context.prec, context.Etop())
        context = context.copy()
        context._set_rounding(ROUND_CEILING)
        context._ignore_all_flags()
        new_self = self._fix(context)
        if new_self != self:
            return new_self
        return self.__add__(_dec_from_triple(0, '1', context.Etiny() - 1), context)

    def next_minus(self, context=None):
        if context is None:
            context = getcontext()
        ans = self._check_nans(context=context)
        if ans:
            return ans
        if self._isinfinity() == -1:
            return _NegativeInfinity
        if self._isinfinity() == 1:
            return _dec_from_triple(0, '9' * context.prec, context.Etop())
        context = context.copy()
        context._set_rounding(ROUND_FLOOR)
        context._ignore_all_flags()
        new_self = self._fix(context)
        if new_self != self:
            return new_self
        return self.__sub__(_dec_from_triple(0, '1', context.Etiny() - 1), context)

    def next_toward(self, other, context=None):
        other = _convert_other(other, raiseit=True)
        if context is None:
            context = getcontext()
        ans = self._check_nans(other, context)
        if ans:
            return ans
        comparison = self._cmp(other)
        if comparison == 0:
            return self.copy_sign(other)
        if comparison == -1:
            ans = self.next_plus(context)
        else:
            ans = self.next_minus(context)
        return ans

    def number_class(self, context=None):
        if self.is_snan():
            return "sNaN"
        if self.is_qnan():
            return "NaN"
        inf = self._isinfinity()
        if inf == 1:
            return "+Infinity"
        if inf == -1:
            return "-Infinity"
        if self.is_zero():
            if self._sign:
                return "-Zero"
            else:
                return "+Zero"
        if context is None:
            context = getcontext()
        if self.is_subnormal(context=context):
            if self._sign:
                return "-Subnormal"
            else:
                return "+Subnormal"
        if self._sign:
            return "-Normal"
        else:
            return "+Normal"

    def radix(self):
        return Decimal(10)

    def is_canonical(self):
        return True

    def is_finite(self):
        return not self._is_special

    def is_infinite(self):
        return self._exp == 'F'

    def is_nan(self):
        return self._exp in ('n', 'N')

    def is_normal(self, context=None):
        if self._is_special or not self:
            return False
        if context is None:
            context = getcontext()
        return context.Emin <= self.adjusted()

    def is_qnan(self):
        return self._exp == 'n'

    def is_signed(self):
        return self._sign == 1

    def is_snan(self):
        return self._exp == 'N'

    def is_subnormal(self, context=None):
        if self._is_special or not self:
            return False
        if context is None:
            context = getcontext()
        return self.adjusted() < context.Emin

    def is_zero(self):
        return not self._is_special and self._int == '0'

    def __reduce__(self):
        return (self.__class__, (str(self),))

    def __copy__(self):
        if type(self) is Decimal:
            return self
        return self.__class__(str(self))

    def __deepcopy__(self, memo):
        if type(self) is Decimal:
            return self
        return self.__class__(str(self))

    def __format__(self, specifier, context=None, _localeconv=None):
        if context is None:
            context = getcontext()

        spec = _parse_format_specifier(specifier, _localeconv=_localeconv)

        if self._is_special:
            sign = _format_sign(self._sign, spec)
            body = str(self.copy_abs())
            if spec['type'] == '%':
                body += '%'
            return _format_align(sign, body, spec)

        if spec['type'] is None:
            spec['type'] = ['g', 'G'][context.capitals]

        if spec['type'] == '%':
            self = _dec_from_triple(self._sign, self._int, self._exp + 2)

        rounding = context.rounding
        precision = spec['precision']
        if precision is not None:
            if spec['type'] in 'eE':
                self = self._round(precision + 1, rounding)
            elif spec['type'] in 'fF%':
                self = self._rescale(-precision, rounding)
            elif spec['type'] in 'gG' and len(self._int) > precision:
                self = self._round(precision, rounding)
        if not self and self._exp > 0 and spec['type'] in 'fF%':
            self = self._rescale(0, rounding)
        if not self and spec['no_neg_0'] and self._sign:
            adjusted_sign = 0
        else:
            adjusted_sign = self._sign

        leftdigits = self._exp + len(self._int)
        if spec['type'] in 'eE':
            if not self and precision is not None:
                dotplace = 1 - precision
            else:
                dotplace = 1
        elif spec['type'] in 'fF%':
            dotplace = leftdigits
        elif spec['type'] in 'gG':
            if self._exp <= 0 and leftdigits > -6:
                dotplace = leftdigits
            else:
                dotplace = 1

        if dotplace < 0:
            intpart = '0'
            fracpart = '0' * (-dotplace) + self._int
        elif dotplace > len(self._int):
            intpart = self._int + '0' * (dotplace - len(self._int))
            fracpart = ''
        else:
            intpart = self._int[:dotplace] or '0'
            fracpart = self._int[dotplace:]
        exp = leftdigits - dotplace

        return _format_number(adjusted_sign, intpart, fracpart, exp, spec)


def _digits(n):
    """str(n) for an int of any size (str() refuses past 4300 digits)."""
    if n < 10 ** 4000:
        return str(n)
    k = (n.bit_length() * 1233 >> 12) // 2
    high, low = divmod(n, 10 ** k)
    return _digits(high) + _digits(low).rjust(k, '0')


def _intof(digits):
    """int(digits) for a string of any length."""
    if len(digits) <= 4000:
        return int(digits)
    k = len(digits) // 2
    return _intof(digits[:-k]) * 10 ** k + _intof(digits[-k:])


def _dec_from_triple(sign, coefficient, exponent, special=False):
    self = object.__new__(Decimal)
    self._sign = sign
    self._int = coefficient
    self._exp = exponent
    self._is_special = special
    return self


_numbers.Number.register(Decimal)


class _WorkRep(object):
    __slots__ = ('sign', 'int', 'exp')

    def __init__(self, value=None):
        if value is None:
            self.sign = None
            self.int = 0
            self.exp = None
        elif isinstance(value, Decimal):
            self.sign = value._sign
            self.int = int(value._int)
            self.exp = value._exp
        else:
            self.sign = value[0]
            self.int = value[1]
            self.exp = value[2]

    def __repr__(self):
        return "(%r, %r, %r)" % (self.sign, self.int, self.exp)


def _decimal_from_workrep(w):
    return _dec_from_triple(w.sign, str(w.int), w.exp)


_old_new = Decimal.__new__


def _new_with_workrep(cls, value="0", context=None):
    if isinstance(value, _WorkRep):
        self = object.__new__(cls)
        self._sign = value.sign
        self._int = str(value.int)
        self._exp = int(value.exp)
        self._is_special = False
        return self
    return _old_new(cls, value, context)


Decimal.__new__ = _new_with_workrep


def _normalize(op1, op2, prec=0):
    if op1.exp < op2.exp:
        tmp = op2
        other = op1
    else:
        tmp = op1
        other = op2

    tmp_len = len(str(tmp.int))
    other_len = len(str(other.int))
    exp = tmp.exp + min(-1, tmp_len - prec - 2)
    if other_len + other.exp - 1 < exp:
        other.int = 1
        other.exp = exp

    tmp.int *= 10 ** (tmp.exp - other.exp)
    tmp.exp = other.exp
    return op1, op2


def _all_zeros(digits, prec):
    rest = digits[prec:]
    return rest == '0' * len(rest)


def _exact_half(digits, prec):
    if len(digits) <= prec or digits[prec] != '5':
        return False
    return _all_zeros(digits, prec + 1)


_ROUNDERS = {
    ROUND_DOWN: Decimal._round_down,
    ROUND_UP: Decimal._round_up,
    ROUND_HALF_UP: Decimal._round_half_up,
    ROUND_HALF_DOWN: Decimal._round_half_down,
    ROUND_HALF_EVEN: Decimal._round_half_even,
    ROUND_CEILING: Decimal._round_ceiling,
    ROUND_FLOOR: Decimal._round_floor,
    ROUND_05UP: Decimal._round_05up,
}


def _convert_other(other, raiseit=False, allow_float=False):
    if isinstance(other, Decimal):
        return other
    if isinstance(other, int):
        return Decimal(other)
    if allow_float and isinstance(other, float):
        return Decimal.from_float(other)

    if raiseit:
        raise TypeError("conversion from %s to Decimal is not supported" % type(other).__name__)
    return NotImplemented


def _convert_for_comparison(self, other, equality_op=False):
    if isinstance(other, Decimal):
        return self, other

    if isinstance(other, _numbers.Rational) and not isinstance(other, int):
        if not self._is_special:
            self = _dec_from_triple(self._sign,
                                    str(int(self._int) * other.denominator),
                                    self._exp)
        return self, Decimal(other.numerator)

    if equality_op and isinstance(other, complex) and other.imag == 0:
        other = other.real
    if isinstance(other, float):
        context = getcontext()
        if equality_op:
            context.flags[FloatOperation] = True
        else:
            context._raise_error(FloatOperation,
                                 "strict semantics for mixing floats and Decimals are enabled")
        return self, Decimal.from_float(other)
    if isinstance(other, int):
        return self, Decimal(other)
    return NotImplemented, NotImplemented


# ------------------------------------------------------------
#  Transcendental functions: fixed point at 10**p, then Ziv's loop.
# ------------------------------------------------------------

def _to_fraction(d):
    from fractions import Fraction
    n, den = d.as_integer_ratio()
    return Fraction(n, den)


def _frac_fixed(x, p):
    """x (a Fraction) as an integer scaled by 10**p, truncated."""
    n, d = x.as_integer_ratio()
    return n * 10 ** p // d


def _mul_fixed(a, x, p):
    """a (scaled by 10**p) times the Fraction x, scaled by 10**p."""
    n, d = x.as_integer_ratio()
    return a * n // d


def _exp_fixed(x, p):
    """e**(x / 10**p), scaled by 10**p (x may be large)."""
    one = 10 ** p
    # Split off whole multiples of ln 2 so the series argument is small.
    ln2 = _ln2_fixed(p + 10)
    k = (x * 10 ** 10) // ln2 if x >= 0 else -((-x * 10 ** 10) // ln2) - 1
    r = x * 10 ** 10 - k * ln2
    q = p + 10
    # r in [0, ln2), scaled by 10**q; halve it s times.
    s = 8
    r >>= 0
    r = r // (1 << s)
    term = 10 ** q
    total = term
    n = 1
    while term:
        term = term * r // (10 ** q * n)
        total += term
        n += 1
    for _ in range(s):
        total = total * total // 10 ** q
    if k >= 0:
        total <<= k
    else:
        total >>= -k
    return total // 10 ** 10


_LN2_CACHE = {}


def _atanh_inv(m, q):
    """atanh(1/m) scaled by 10**q."""
    x = 10 ** q // m
    m2 = m * m
    total = x
    n = 3
    while x:
        x //= m2
        total += x // n
        n += 2
    return total


def _ln2_fixed(q):
    if q not in _LN2_CACHE:
        _LN2_CACHE[q] = (2 * _atanh_inv(3, q + 5)) // 10 ** 5
    return _LN2_CACHE[q]


def _ln10_fixed(p):
    q = p + 10
    ln2 = _ln2_fixed(q)
    # ln 10 = 3 ln 2 + ln(5/4) = 3 ln 2 + 2 atanh(1/9)
    ln10 = 3 * ln2 + 2 * _atanh_inv(9, q)
    return ln10 // 10 ** 10


def _ln_fixed(x, p):
    """ln(x) for a positive Fraction x, scaled by 10**p."""
    n, d = x.as_integer_ratio()
    q = p + 10
    # x = 2**k * y with y in [1, 2)
    k = n.bit_length() - d.bit_length()
    if k >= 0:
        dd = d << k
        nn = n
    else:
        nn = n << -k
        dd = d
    if nn < dd:
        nn <<= 1
        k -= 1
    # y = nn/dd in [1, 2): ln y = 2 atanh((y - 1)/(y + 1))
    s_num = nn - dd
    s_den = nn + dd
    z = s_num * 10 ** q // s_den
    z2 = z * z // 10 ** q
    term = z
    total = z
    i = 3
    while term:
        term = term * z2 // 10 ** q
        total += term // i
        i += 2
    result = 2 * total + k * _ln2_fixed(q)
    return result // 10 ** 10


def _ziv(sign, compute, context, rounding=None):
    """Rounds compute(p) / 10**p to the context, raising p until the
    digits beyond the precision decide the rounding."""
    prec = context.prec
    p = prec + 10
    while True:
        v = compute(p)
        negative = 1 if v < 0 else 0
        a = abs(v)
        # The approximation is within a few units of 10**-p: the result
        # is certain when a - 10 and a + 10 round alike.
        c = context._shallow_copy()
        c.traps = {s: 0 for s in _signals}
        c.flags = {s: 0 for s in _signals}
        c._ignored_flags = []
        if rounding is not None:
            c.rounding = rounding
        lo = _dec_from_triple(negative, _digits(max(a - 10, 0)), -p)._fix(c)
        hi = _dec_from_triple(negative, _digits(a + 10), -p)._fix(c)
        if a > 10 ** 6 and str(lo) == str(hi):
            ans = _dec_from_triple(negative ^ (sign or 0), _digits(a), -p)
            saved = context.rounding
            if rounding is not None:
                context.rounding = rounding
            try:
                result = ans._fix(context)
            finally:
                context.rounding = saved
            return result
        p += prec + 10


def _exact_power(x, y, context):
    """x ** y when it is exact: y = n/d and x a perfect d-th power."""
    n, d = y.numerator, y.denominator
    if d > 64:
        return None
    num, den = x.numerator, x.denominator
    rn = _iroot(num, d)
    rd = _iroot(den, d)
    if rn is None or rd is None:
        return None
    if n >= 0:
        pn, pd = rn ** n, rd ** n
    else:
        pn, pd = rd ** -n, rn ** -n
    return _rational(0, pn, pd, 0, context, exact_only=True)


def _iroot(n, k):
    """The integer k-th root of n if n is a perfect k-th power, else None."""
    if n < 0:
        return None
    if n in (0, 1):
        return n
    r = int(round(n ** (1.0 / k))) if n.bit_length() < 1000 else None
    if r is None:
        lo, hi = 0, 1 << (n.bit_length() // k + 1)
        while lo < hi:
            mid = (lo + hi) // 2
            if mid ** k < n:
                lo = mid + 1
            else:
                hi = mid
        r = lo
    for c in (r - 1, r, r + 1):
        if c >= 0 and c ** k == n:
            return c
    return None


def _rational(sign, num, den, exp10, context, ideal_exp=None, exact_only=False):
    """The Decimal nearest (num/den) * 10**exp10, rounded once."""
    if exact_only:
        # Exact only when den has no prime factor but 2 and 5.
        d = den
        twos = fives = 0
        while d % 2 == 0:
            d //= 2
            twos += 1
        while d % 5 == 0:
            d //= 5
            fives += 1
        if d != 1:
            return None
        k = max(twos, fives)
        coeff = num * 10 ** k // den
        e = exp10 - k
        while coeff % 10 == 0 and coeff:
            coeff //= 10
            e += 1
        return _dec_from_triple(sign, str(coeff), e)
    prec = context.prec
    shift = prec + 2 + len(str(den)) - len(str(num))
    if shift >= 0:
        coeff, remainder = divmod(num * 10 ** shift, den)
    else:
        coeff, remainder = divmod(num, den * 10 ** -shift)
    e = exp10 - shift
    if remainder:
        if coeff % 5 == 0:
            coeff += 1
    elif ideal_exp is not None:
        while e < ideal_exp and coeff % 10 == 0:
            coeff //= 10
            e += 1
    ans = _dec_from_triple(sign, str(coeff), e)
    return ans._fix(context)


# ------------------------------------------------------------
#  Context
# ------------------------------------------------------------

class Context(object):
    __nova_type_name__ = 'decimal.Context'
    def __init__(self, prec=None, rounding=None, Emin=None, Emax=None,
                 capitals=None, clamp=None, flags=None, traps=None,
                 _ignored_flags=None):
        try:
            dc = DefaultContext
        except NameError:
            pass

        self.prec = prec if prec is not None else dc.prec
        self.rounding = rounding if rounding is not None else dc.rounding
        self.Emin = Emin if Emin is not None else dc.Emin
        self.Emax = Emax if Emax is not None else dc.Emax
        self.capitals = capitals if capitals is not None else dc.capitals
        self.clamp = clamp if clamp is not None else dc.clamp

        if _ignored_flags is None:
            self._ignored_flags = []
        else:
            self._ignored_flags = _ignored_flags

        if traps is None:
            self.traps = dc.traps.copy()
        elif not isinstance(traps, dict):
            self.traps = dict((s, s in traps) for s in _signals + traps)
        else:
            self.traps = traps

        if flags is None:
            self.flags = dict.fromkeys(_signals, False)
        elif not isinstance(flags, dict):
            self.flags = dict((s, s in flags) for s in _signals + flags)
        else:
            self.flags = flags

    def __setattr__(self, name, value):
        if name == 'prec':
            _check_int(value, 1, MAX_PREC, 'prec')
        elif name == 'Emin':
            _check_int(value, MIN_EMIN, 0, 'Emin')
        elif name == 'Emax':
            _check_int(value, 0, MAX_EMAX, 'Emax')
        elif name == 'capitals':
            _check_int(value, 0, 1, 'capitals')
        elif name == 'clamp':
            _check_int(value, 0, 1, 'clamp')
        elif name == 'rounding':
            if value not in _rounding_modes:
                raise TypeError("valid values for rounding are:\n"
                                "  [ROUND_CEILING, ROUND_FLOOR, ROUND_UP, ROUND_DOWN,\n"
                                "   ROUND_HALF_UP, ROUND_HALF_DOWN, ROUND_HALF_EVEN,\n"
                                "   ROUND_05UP]")
        object.__setattr__(self, name, value)

    def __repr__(self):
        s = []
        s.append('Context(prec=%(prec)d, rounding=%(rounding)s, '
                 'Emin=%(Emin)d, Emax=%(Emax)d, capitals=%(capitals)d, '
                 'clamp=%(clamp)d'
                 % vars(self))
        names = [f.__name__ for f in _REPR_ORDER if self.flags.get(f)]
        s.append('flags=[' + ', '.join(names) + ']')
        names = [t.__name__ for t in _REPR_ORDER if self.traps.get(t)]
        s.append('traps=[' + ', '.join(names) + ']')
        return ', '.join(s) + ')'

    def clear_flags(self):
        for flag in self.flags:
            self.flags[flag] = False

    def clear_traps(self):
        for flag in self.traps:
            self.traps[flag] = False

    def _shallow_copy(self):
        nc = Context(self.prec, self.rounding, self.Emin, self.Emax,
                     self.capitals, self.clamp, self.flags, self.traps,
                     self._ignored_flags)
        return nc

    def copy(self):
        nc = Context(self.prec, self.rounding, self.Emin, self.Emax,
                     self.capitals, self.clamp,
                     self.flags.copy(), self.traps.copy(),
                     self._ignored_flags)
        return nc

    __copy__ = copy

    def _raise_error(self, condition, explanation=None, *args):
        error = _condition_map.get(condition, condition)
        if error in self._ignored_flags:
            return error().handle(self, *args)

        self.flags[error] = True
        if not self.traps[error]:
            return condition().handle(self, *args)

        raise error([condition])

    def _ignore_all_flags(self):
        return self._ignore_flags(*_signals)

    def _ignore_flags(self, *flags):
        self._ignored_flags = (self._ignored_flags + list(flags))
        return list(flags)

    def _regard_flags(self, *flags):
        if flags and isinstance(flags[0], (tuple, list)):
            flags = flags[0]
        for flag in flags:
            self._ignored_flags.remove(flag)

    __hash__ = None

    def Etiny(self):
        return int(self.Emin - self.prec + 1)

    def Etop(self):
        return int(self.Emax - self.prec + 1)

    def _set_rounding(self, type):
        rounding = self.rounding
        self.rounding = type
        return rounding

    def create_decimal(self, num='0'):
        if isinstance(num, str) and (num != num.strip() or '_' in num):
            return self._raise_error(ConversionSyntax,
                                     "trailing or leading whitespace and "
                                     "underscores are not permitted.")

        d = Decimal(num, context=self)
        if d._isnan() and len(d._int) > self.prec - self.clamp:
            return self._raise_error(ConversionSyntax,
                                     "diagnostic info too long in NaN")
        return d._fix(self)

    def create_decimal_from_float(self, f):
        d = Decimal.from_float(f)
        return d._fix(self)

    def abs(self, a):
        a = _convert_other(a, raiseit=True)
        return a.__abs__(context=self)

    def add(self, a, b):
        a = _convert_other(a, raiseit=True)
        r = a.__add__(b, context=self)
        if r is NotImplemented:
            raise TypeError("Unable to convert %s to Decimal" % b)
        return r

    def subtract(self, a, b):
        a = _convert_other(a, raiseit=True)
        r = a.__sub__(b, context=self)
        if r is NotImplemented:
            raise TypeError("Unable to convert %s to Decimal" % b)
        return r

    def multiply(self, a, b):
        a = _convert_other(a, raiseit=True)
        r = a.__mul__(b, context=self)
        if r is NotImplemented:
            raise TypeError("Unable to convert %s to Decimal" % b)
        return r

    def divide(self, a, b):
        a = _convert_other(a, raiseit=True)
        r = a.__truediv__(b, context=self)
        if r is NotImplemented:
            raise TypeError("Unable to convert %s to Decimal" % b)
        return r

    def divide_int(self, a, b):
        a = _convert_other(a, raiseit=True)
        return a.__floordiv__(b, context=self)

    def divmod(self, a, b):
        a = _convert_other(a, raiseit=True)
        return a.__divmod__(b, context=self)

    def remainder(self, a, b):
        a = _convert_other(a, raiseit=True)
        return a.__mod__(b, context=self)

    def remainder_near(self, a, b):
        a = _convert_other(a, raiseit=True)
        return a.remainder_near(b, context=self)

    def power(self, a, b, modulo=None):
        a = _convert_other(a, raiseit=True)
        return a.__pow__(b, modulo, context=self)

    def sqrt(self, a):
        a = _convert_other(a, raiseit=True)
        return a.sqrt(context=self)

    def exp(self, a):
        a = _convert_other(a, raiseit=True)
        return a.exp(context=self)

    def ln(self, a):
        a = _convert_other(a, raiseit=True)
        return a.ln(context=self)

    def log10(self, a):
        a = _convert_other(a, raiseit=True)
        return a.log10(context=self)

    def quantize(self, a, b):
        a = _convert_other(a, raiseit=True)
        return a.quantize(b, context=self)

    def compare(self, a, b):
        a = _convert_other(a, raiseit=True)
        return a.compare(b, context=self)

    def max(self, a, b):
        a = _convert_other(a, raiseit=True)
        return a.max(b, context=self)

    def min(self, a, b):
        a = _convert_other(a, raiseit=True)
        return a.min(b, context=self)

    def minus(self, a):
        a = _convert_other(a, raiseit=True)
        return a.__neg__(context=self)

    def plus(self, a):
        a = _convert_other(a, raiseit=True)
        return a.__pos__(context=self)

    def normalize(self, a):
        a = _convert_other(a, raiseit=True)
        return a.normalize(context=self)

    def to_integral_value(self, a):
        a = _convert_other(a, raiseit=True)
        return a.to_integral_value(context=self)

    to_integral = to_integral_value

    def to_sci_string(self, a):
        a = _convert_other(a, raiseit=True)
        return a.__str__(context=self)

    def to_eng_string(self, a):
        a = _convert_other(a, raiseit=True)
        return a.to_eng_string(context=self)

    def fma(self, a, b, c):
        a = _convert_other(a, raiseit=True)
        return a.fma(b, c, context=self)

    def is_signed(self, a):
        a = _convert_other(a, raiseit=True)
        return a.is_signed()

    def is_zero(self, a):
        a = _convert_other(a, raiseit=True)
        return a.is_zero()

    def number_class(self, a):
        a = _convert_other(a, raiseit=True)
        return a.number_class(context=self)

    def copy_sign(self, a, b):
        a = _convert_other(a, raiseit=True)
        return a.copy_sign(b)

    def radix(self):
        return Decimal(10)


# ------------------------------------------------------------
#  Parsing and formatting
# ------------------------------------------------------------

import re as _re

_parser = _re.compile(r"""        # A numeric string consists of:
    (?P<sign>[-+])?              # an optional sign, followed by either...
    (
        (?=\d|\.\d)              # ...a number (with at least one digit)
        (?P<int>\d*)             # having a (possibly empty) integer part
        (\.(?P<frac>\d*))?       # followed by an optional fractional part
        (E(?P<exp>[-+]?\d+))?    # followed by an optional exponent, or...
    |
        Inf(inity)?              # ...an infinity, or...
    |
        (?P<signal>s)?           # ...an (optionally signaling)
        NaN                      # NaN
        (?P<diag>\d*)            # with (possibly empty) diagnostic info.
    )
    \Z
""", _re.VERBOSE | _re.IGNORECASE).match

_parse_format_specifier_regex = _re.compile(r"""\A
(?:
   (?P<fill>.)?
   (?P<align>[<>=^])
)?
(?P<sign>[-+ ])?
(?P<no_neg_0>z)?
(?P<alt>\#)?
(?P<zeropad>0)?
(?P<minimumwidth>\d+)?
(?P<thousands_sep>[,_])?
(?:\.(?P<precision>\d+))?
(?P<type>[eEfFgGn%])?
\Z
""", _re.VERBOSE | _re.DOTALL)


def _parse_format_specifier(format_spec, _localeconv=None):
    m = _parse_format_specifier_regex.match(format_spec)
    if m is None:
        raise ValueError("Invalid format specifier: " + format_spec)

    format_dict = m.groupdict()

    fill = format_dict['fill']
    align = format_dict['align']
    format_dict['zeropad'] = (format_dict['zeropad'] is not None)
    if format_dict['zeropad']:
        if fill is not None:
            raise ValueError("Fill character conflicts with '0'"
                             " in format specifier: " + format_spec)
        if align is not None:
            raise ValueError("Alignment conflicts with '0' in "
                             "format specifier: " + format_spec)
    format_dict['fill'] = fill or ' '
    format_dict['align'] = align or '>'

    if format_dict['sign'] is None:
        format_dict['sign'] = '-'

    format_dict['minimumwidth'] = int(format_dict['minimumwidth'] or '0')
    if format_dict['precision'] is not None:
        format_dict['precision'] = int(format_dict['precision'])

    if format_dict['precision'] == 0:
        if format_dict['type'] is None or format_dict['type'] in 'gGn':
            format_dict['precision'] = 1

    if format_dict['type'] == 'n':
        format_dict['type'] = 'g'
        if format_dict['thousands_sep'] is None:
            format_dict['thousands_sep'] = ''
        format_dict['grouping'] = [3, 0]
        format_dict['decimal_point'] = '.'
    else:
        if format_dict['thousands_sep'] is None:
            format_dict['thousands_sep'] = ''
        format_dict['grouping'] = [3, 0]
        format_dict['decimal_point'] = '.'

    format_dict['no_neg_0'] = format_dict['no_neg_0'] is not None
    format_dict['alt'] = format_dict['alt'] is not None
    return format_dict


def _format_align(sign, body, spec):
    minimumwidth = spec['minimumwidth']
    fill = spec['fill']
    padding = fill * (minimumwidth - len(sign) - len(body))

    align = spec['align']
    if align == '<':
        result = sign + body + padding
    elif align == '>':
        result = padding + sign + body
    elif align == '=':
        result = sign + padding + body
    elif align == '^':
        half = len(padding) // 2
        result = padding[:half] + sign + body + padding[half:]
    else:
        raise ValueError('Unrecognised alignment field')

    return result


def _group_lengths(grouping):
    from itertools import chain, repeat
    if not grouping:
        return []
    elif grouping[-1] == 0 and len(grouping) >= 2:
        return chain(grouping[:-1], repeat(grouping[-2]))
    else:
        raise ValueError('unrecognised format for grouping')


def _insert_thousands_sep(digits, spec, min_width=1):
    sep = spec['thousands_sep']
    grouping = spec['grouping']

    groups = []
    for l in _group_lengths(grouping):
        if l <= 0:
            raise ValueError("group length should be positive")
        l = min(max(len(digits), min_width, 1), l)
        groups.append('0' * (l - len(digits)) + digits[-l:])
        digits = digits[:-l]
        min_width -= l
        if not digits and min_width <= 0:
            break
        min_width -= len(sep)
    else:
        l = max(len(digits), min_width, 1)
        groups.append('0' * (l - len(digits)) + digits[-l:])
    return sep.join(reversed(groups))


def _format_sign(is_negative, spec):
    if is_negative:
        return '-'
    elif spec['sign'] in ' +':
        return spec['sign']
    else:
        return ''


def _format_number(is_negative, intpart, fracpart, exp, spec):
    sign = _format_sign(is_negative, spec)

    if fracpart or spec['alt']:
        fracpart = spec['decimal_point'] + fracpart

    if exp != 0 or spec['type'] in 'eE':
        echar = {'E': 'E', 'e': 'e', 'G': 'E', 'g': 'e'}[spec['type']]
        fracpart += "{0}{1:+}".format(echar, exp)
    if spec['type'] == '%':
        fracpart += '%'

    if spec['zeropad']:
        min_width = spec['minimumwidth'] - len(fracpart) - len(sign)
    else:
        min_width = 0
    intpart = _insert_thousands_sep(intpart, spec, min_width)

    return _format_align(sign, intpart + fracpart, spec)


DefaultContext = Context(
    prec=28, rounding=ROUND_HALF_EVEN,
    traps=[DivisionByZero, Overflow, InvalidOperation],
    flags=[],
    Emax=999999,
    Emin=-999999,
    capitals=1,
    clamp=0
)

BasicContext = Context(
    prec=9, rounding=ROUND_HALF_UP,
    traps=[DivisionByZero, Overflow, InvalidOperation, Clamped, Underflow],
    flags=[],
)

ExtendedContext = Context(
    prec=9, rounding=ROUND_HALF_EVEN,
    traps=[],
    flags=[],
)

_Infinity = Decimal('Inf')
_NegativeInfinity = Decimal('-Inf')
_NaN = Decimal('NaN')
_Zero = Decimal(0)
_One = Decimal(1)
_NegativeOne = Decimal(-1)

_SignedInfinity = (_Infinity, _NegativeInfinity)
