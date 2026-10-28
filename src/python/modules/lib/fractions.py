"""fractions for Nova's Python: exact rationals, as CPython 3.13 behaves.

A Fraction is kept normalized (positive denominator, gcd 1). Mixed
arithmetic follows the numeric tower: with an int or a Fraction the result
is exact, with a float it is a float, with a complex a complex. The hash
equals that of an equal int or float, computed modulo 2**61 - 1 like
CPython's, so a Fraction and an equal number share a dict slot.
"""

import math
import numbers
import operator
import re

__all__ = ['Fraction']

_PyHASH_MODULUS = 2305843009213693951
_PyHASH_INF = 314159

_RATIONAL_FORMAT = re.compile(r"""
    \A\s*                                  # optional whitespace at the start,
    (?P<sign>[-+]?)                        # an optional sign, then
    (?=\d|\.\d)                            # lookahead for digit or .digit
    (?P<num>\d*|\d+(_\d+)*)                # numerator (possibly empty)
    (?:                                    # followed by
       (?:\s*/\s*(?P<denom>\d+(_\d+)*))?   # an optional denominator
    |                                      # or
       (?:\.(?P<decimal>\d*|\d+(_\d+)*))?  # an optional fractional part
       (?:E(?P<exp>[-+]?\d+(_\d+)*))?      # and optional exponent
    )
    \s*\Z                                  # and optional whitespace to finish
""", re.VERBOSE | re.IGNORECASE)

_FLOAT_FORMAT_SPEC = re.compile(r"""
    (?:
        (?P<fill>.)?
        (?P<align>[<>=^])
    )?
    (?P<sign>[-+ ]?)
    (?P<no_neg_zero>z)?
    (?P<alt>\#)?
    (?P<zeropad>0(?=[0-9]))?
    (?P<minimumwidth>[0-9]+)?
    (?P<thousands_sep>[,_])?
    (?:\.(?P<precision>[0-9]+))?
    (?P<presentation_type>[eEfFgG%])
""", re.DOTALL | re.VERBOSE)


def _hash_algorithm(numerator, denominator):
    try:
        dinv = pow(denominator, -1, _PyHASH_MODULUS)
    except ValueError:
        hash_ = _PyHASH_INF
    else:
        hash_ = hash(hash(abs(numerator)) * dinv)
    result = hash_ if numerator >= 0 else -hash_
    return -2 if result == -1 else result


def _round_to_exponent(n, d, exponent, no_neg_zero=False):
    if exponent >= 0:
        d *= 10 ** exponent
    else:
        n *= 10 ** -exponent
    q, r = divmod(n + (d >> 1), d)
    if r == 0 and d & 1 == 0:
        q &= -2
    sign = q < 0 if no_neg_zero else n < 0
    return sign, abs(q)


def _round_to_figures(n, d, figures):
    if n == 0:
        return False, 0, 1 - figures
    str_n, str_d = str(abs(n)), str(d)
    m = len(str_n) - len(str_d) + (str_d <= str_n)
    exponent = m - figures
    sign, significand = _round_to_exponent(n, d, exponent)
    if len(str(significand)) == figures + 1:
        significand //= 10
        exponent += 1
    return sign, significand, exponent


class Fraction(numbers.Rational):
    __slots__ = ('_numerator', '_denominator')

    def __new__(cls, numerator=0, denominator=None):
        self = object.__new__(cls)
        if denominator is None:
            if type(numerator) is int or type(numerator) is bool:
                self._numerator = int(numerator)
                self._denominator = 1
                return self
            elif isinstance(numerator, Fraction):
                self._numerator = numerator._numerator
                self._denominator = numerator._denominator
                return self
            elif isinstance(numerator, (float,)) or type(numerator).__name__ == 'Decimal':
                self._numerator, self._denominator = numerator.as_integer_ratio()
                return self
            elif isinstance(numerator, str):
                m = _RATIONAL_FORMAT.match(numerator)
                if m is None:
                    raise ValueError('Invalid literal for Fraction: %r' % numerator)
                numerator = int(m.group('num') or '0')
                denom = m.group('denom')
                if denom:
                    denominator = int(denom)
                else:
                    denominator = 1
                    decimal = m.group('decimal')
                    if decimal:
                        decimal = decimal.replace('_', '')
                        scale = 10 ** len(decimal)
                        numerator = numerator * scale + int(decimal)
                        denominator *= scale
                    exp = m.group('exp')
                    if exp:
                        exp = int(exp)
                        if exp >= 0:
                            numerator *= 10 ** exp
                        else:
                            denominator *= 10 ** -exp
                if m.group('sign') == '-':
                    numerator = -numerator
            elif isinstance(numerator, int):
                self._numerator = int(numerator)
                self._denominator = 1
                return self
            else:
                raise TypeError("argument should be a string or a Rational instance")
        elif type(numerator) is int is type(denominator):
            pass
        elif isinstance(numerator, (int, Fraction)) and isinstance(denominator, (int, Fraction)):
            numerator, denominator = (
                Fraction(numerator) / Fraction(denominator)).as_integer_ratio()
        else:
            raise TypeError("both arguments should be Rational instances")

        if denominator == 0:
            raise ZeroDivisionError('Fraction(%s, 0)' % numerator)
        g = math.gcd(numerator, denominator)
        if denominator < 0:
            g = -g
        numerator //= g
        denominator //= g
        self._numerator = numerator
        self._denominator = denominator
        return self

    @classmethod
    def from_float(cls, f):
        if isinstance(f, int):
            return cls(f)
        elif not isinstance(f, float):
            raise TypeError("%s.from_float() only takes floats, not %r (%s)" %
                            (cls.__name__, f, type(f).__name__))
        return cls._from_coprime_ints(*f.as_integer_ratio())

    @classmethod
    def from_decimal(cls, dec):
        if isinstance(dec, int):
            dec = __import__('decimal').Decimal(int(dec))
        elif type(dec).__name__ != 'Decimal':
            raise TypeError(
                "%s.from_decimal() only takes Decimals, not %r (%s)" %
                (cls.__name__, dec, type(dec).__name__))
        return cls._from_coprime_ints(*dec.as_integer_ratio())

    @classmethod
    def _from_coprime_ints(cls, numerator, denominator):
        obj = object.__new__(cls)
        obj._numerator = numerator
        obj._denominator = denominator
        return obj

    def is_integer(self):
        return self._denominator == 1

    def as_integer_ratio(self):
        return (self._numerator, self._denominator)

    def limit_denominator(self, max_denominator=1000000):
        if max_denominator < 1:
            raise ValueError("max_denominator should be at least 1")
        if self._denominator <= max_denominator:
            return Fraction(self)

        p0, q0, p1, q1 = 0, 1, 1, 0
        n, d = self._numerator, self._denominator
        while True:
            a = n // d
            q2 = q0 + a * q1
            if q2 > max_denominator:
                break
            p0, q0, p1, q1 = p1, q1, p0 + a * p1, q2
            n, d = d, n - a * d
        k = (max_denominator - q0) // q1

        if 2 * d * (q0 + k * q1) <= self._denominator:
            return Fraction._from_coprime_ints(p1, q1)
        else:
            return Fraction._from_coprime_ints(p0 + k * p1, q0 + k * q1)

    @property
    def numerator(a):
        return a._numerator

    @property
    def denominator(a):
        return a._denominator

    def __repr__(self):
        return '%s(%s, %s)' % (self.__class__.__name__,
                               self._numerator, self._denominator)

    def __str__(self):
        if self._denominator == 1:
            return str(self._numerator)
        else:
            return '%s/%s' % (self._numerator, self._denominator)

    def __format__(self, format_spec, /):
        if not format_spec:
            return str(self)
        match = _FLOAT_FORMAT_SPEC.fullmatch(format_spec)
        if match is None:
            if format_spec and format_spec[-1] not in 'eEfFgG%' and _general(format_spec):
                return format(str(self), format_spec)
            raise ValueError(
                f"Invalid format specifier {format_spec!r} "
                f"for object of type {type(self).__name__!r}"
            )
        elif match["align"] is not None and match["zeropad"] is not None:
            raise ValueError(
                f"Invalid format specifier {format_spec!r} "
                f"for object of type {type(self).__name__!r}; "
                "can't use explicit alignment when zero-padding"
            )
        fill = match["fill"] or " "
        align = match["align"] or ">"
        pos_sign = "" if match["sign"] == "-" else match["sign"]
        no_neg_zero = bool(match["no_neg_zero"])
        alternate_form = bool(match["alt"])
        zeropad = bool(match["zeropad"])
        minimumwidth = int(match["minimumwidth"] or "0")
        thousands_sep = match["thousands_sep"]
        precision = int(match["precision"] or "6")
        presentation_type = match["presentation_type"]
        trim_zeros = presentation_type in "gG" and not alternate_form
        trim_point = not alternate_form
        exponent_indicator = "E" if presentation_type in "EFG" else "e"

        if presentation_type in "fF%":
            exponent = -precision
            if presentation_type == "%":
                exponent -= 2
            negative, significand = _round_to_exponent(
                self._numerator, self._denominator, exponent, no_neg_zero)
            scientific = False
            point_pos = precision
        else:
            figures = (
                max(precision, 1)
                if presentation_type in "gG"
                else precision + 1
            )
            negative, significand, exponent = _round_to_figures(
                self._numerator, self._denominator, figures)
            scientific = (
                presentation_type in "eE"
                or exponent > 0
                or exponent + figures <= -4
            )
            point_pos = figures - 1 if scientific else -exponent

        if presentation_type == "%":
            suffix = "%"
        elif scientific:
            suffix = f"{exponent_indicator}{exponent + point_pos:+03d}"
        else:
            suffix = ""

        digits = f"{significand:0{point_pos + 1}d}"

        sign = "-" if negative else pos_sign
        leading = digits[: len(digits) - point_pos]
        frac_part = digits[len(digits) - point_pos:]
        if trim_zeros:
            frac_part = frac_part.rstrip("0")
        separator = "" if trim_point and not frac_part else "."
        trailing = separator + frac_part + suffix

        if zeropad:
            min_leading = minimumwidth - len(sign) - len(trailing)
            leading = leading.zfill(
                3 * min_leading // 4 + 1 if thousands_sep else min_leading
            )

        if thousands_sep:
            first_pos = 1 + (len(leading) - 1) % 3
            leading = leading[:first_pos] + "".join(
                thousands_sep + leading[pos: pos + 3]
                for pos in range(first_pos, len(leading), 3)
            )

        body = leading + trailing
        padding = fill * (minimumwidth - len(sign) - len(body))
        if align == ">":
            return padding + sign + body
        elif align == "<":
            return sign + body + padding
        elif align == "^":
            half = len(padding) // 2
            return padding[:half] + sign + body + padding[half:]
        else:  # align == "="
            return sign + padding + body

    def _operator_fallbacks(monomorphic_operator, fallback_operator):
        def forward(a, b):
            if isinstance(b, Fraction):
                return monomorphic_operator(a, b)
            elif isinstance(b, int):
                return monomorphic_operator(a, Fraction(b))
            elif isinstance(b, float):
                return fallback_operator(float(a), b)
            elif isinstance(b, complex):
                return fallback_operator(complex(a), b)
            else:
                return NotImplemented
        forward.__name__ = '__' + fallback_operator.__name__ + '__'
        forward.__doc__ = monomorphic_operator.__doc__

        def reverse(b, a):
            if isinstance(a, Fraction):
                return monomorphic_operator(Fraction(a), b)
            elif isinstance(a, int):
                return monomorphic_operator(Fraction(a), b)
            elif isinstance(a, float):
                return fallback_operator(a, float(b))
            elif isinstance(a, complex):
                return fallback_operator(complex(a), complex(b))
            else:
                return NotImplemented
        reverse.__name__ = '__r' + fallback_operator.__name__ + '__'
        reverse.__doc__ = monomorphic_operator.__doc__

        return forward, reverse

    def _add(a, b):
        na, da = a._numerator, a._denominator
        nb, db = b._numerator, b._denominator
        g = math.gcd(da, db)
        if g == 1:
            return Fraction._from_coprime_ints(na * db + da * nb, da * db)
        s = da // g
        t = na * (db // g) + nb * s
        g2 = math.gcd(t, g)
        if g2 == 1:
            return Fraction._from_coprime_ints(t, s * db)
        return Fraction._from_coprime_ints(t // g2, s * (db // g2))

    __add__, __radd__ = _operator_fallbacks(_add, operator.add)

    def _sub(a, b):
        na, da = a._numerator, a._denominator
        nb, db = b._numerator, b._denominator
        g = math.gcd(da, db)
        if g == 1:
            return Fraction._from_coprime_ints(na * db - da * nb, da * db)
        s = da // g
        t = na * (db // g) - nb * s
        g2 = math.gcd(t, g)
        if g2 == 1:
            return Fraction._from_coprime_ints(t, s * db)
        return Fraction._from_coprime_ints(t // g2, s * (db // g2))

    __sub__, __rsub__ = _operator_fallbacks(_sub, operator.sub)

    def _mul(a, b):
        na, da = a._numerator, a._denominator
        nb, db = b._numerator, b._denominator
        g1 = math.gcd(na, db)
        if g1 > 1:
            na //= g1
            db //= g1
        g2 = math.gcd(nb, da)
        if g2 > 1:
            nb //= g2
            da //= g2
        return Fraction._from_coprime_ints(na * nb, db * da)

    __mul__, __rmul__ = _operator_fallbacks(_mul, operator.mul)

    def _div(a, b):
        nb, db = b._numerator, b._denominator
        if nb == 0:
            raise ZeroDivisionError('Fraction(%s, 0)' % db)
        na, da = a._numerator, a._denominator
        g1 = math.gcd(na, nb)
        if g1 > 1:
            na //= g1
            nb //= g1
        g2 = math.gcd(db, da)
        if g2 > 1:
            da //= g2
            db //= g2
        n, d = na * db, nb * da
        if d < 0:
            n, d = -n, -d
        return Fraction._from_coprime_ints(n, d)

    __truediv__, __rtruediv__ = _operator_fallbacks(_div, operator.truediv)

    def _floordiv(a, b):
        return (a.numerator * b.denominator) // (a.denominator * b.numerator)

    __floordiv__, __rfloordiv__ = _operator_fallbacks(_floordiv, operator.floordiv)

    def _divmod(a, b):
        da, db = a.denominator, b.denominator
        div, n_mod = divmod(a.numerator * db, da * b.numerator)
        return div, Fraction(n_mod, da * db)

    __divmod__, __rdivmod__ = _operator_fallbacks(_divmod, divmod)

    def _mod(a, b):
        da, db = a.denominator, b.denominator
        return Fraction((a.numerator * db) % (b.numerator * da), da * db)

    __mod__, __rmod__ = _operator_fallbacks(_mod, operator.mod)

    def __pow__(a, b, modulo=None):
        if modulo is not None:
            return NotImplemented
        if isinstance(b, (int, Fraction)) and not isinstance(b, bool) or type(b) is bool:
            b = Fraction(b)
            if b.denominator == 1:
                power = b.numerator
                if power >= 0:
                    return Fraction._from_coprime_ints(
                        a._numerator ** power,
                        a._denominator ** power)
                elif a._numerator > 0:
                    return Fraction._from_coprime_ints(
                        a._denominator ** -power,
                        a._numerator ** -power)
                elif a._numerator == 0:
                    raise ZeroDivisionError('Fraction(%s, 0)' %
                                            a._denominator ** -power)
                else:
                    return Fraction._from_coprime_ints(
                        (-a._denominator) ** -power,
                        (-a._numerator) ** -power)
            else:
                return float(a) ** float(b)
        elif isinstance(b, (float, complex)):
            return float(a) ** b
        else:
            return NotImplemented

    def __rpow__(b, a, modulo=None):
        if modulo is not None:
            return NotImplemented
        if b._denominator == 1 and b._numerator >= 0:
            return a ** b._numerator
        if isinstance(a, int):
            return Fraction(a) ** b
        if isinstance(a, Fraction):
            return a ** b
        return a ** float(b)

    def __pos__(a):
        return Fraction._from_coprime_ints(a._numerator, a._denominator)

    def __neg__(a):
        return Fraction._from_coprime_ints(-a._numerator, a._denominator)

    def __abs__(a):
        return Fraction._from_coprime_ints(abs(a._numerator), a._denominator)

    def __int__(a):
        if a._numerator < 0:
            return -(-a._numerator // a._denominator)
        else:
            return a._numerator // a._denominator

    def __trunc__(a):
        return a.__int__()

    def __float__(a):
        return a._numerator / a._denominator

    def __complex__(a):
        return complex(float(a))

    def __floor__(a):
        return a._numerator // a._denominator

    def __ceil__(a):
        return -(-a._numerator // a._denominator)

    def __round__(self, ndigits=None):
        if ndigits is None:
            d = self._denominator
            floor, remainder = divmod(self._numerator, d)
            if remainder * 2 < d:
                return floor
            elif remainder * 2 > d:
                return floor + 1
            elif floor % 2 == 0:
                return floor
            else:
                return floor + 1
        shift = 10 ** abs(ndigits)
        if ndigits > 0:
            return Fraction(round(self * shift), shift)
        else:
            return Fraction(round(self / shift) * shift)

    def __hash__(self):
        return _hash_algorithm(self._numerator, self._denominator)

    def __eq__(a, b):
        if type(b) is int or type(b) is bool:
            return a._numerator == b and a._denominator == 1
        if isinstance(b, Fraction):
            return (a._numerator == b.numerator and
                    a._denominator == b.denominator)
        if isinstance(b, complex) and b.imag == 0:
            b = b.real
        if isinstance(b, float):
            if math.isnan(b) or math.isinf(b):
                return 0.0 == b
            else:
                return a == a.from_float(b)
        if isinstance(b, int):
            return a._numerator == b and a._denominator == 1
        return NotImplemented

    def _richcmp(self, other, op):
        if isinstance(other, (int, Fraction)):
            return op(self._numerator * other.denominator,
                      self._denominator * other.numerator)
        if isinstance(other, float):
            if math.isnan(other) or math.isinf(other):
                return op(0.0, other)
            else:
                return op(self, self.from_float(other))
        else:
            return NotImplemented

    def __lt__(a, b):
        return a._richcmp(b, operator.lt)

    def __gt__(a, b):
        return a._richcmp(b, operator.gt)

    def __le__(a, b):
        return a._richcmp(b, operator.le)

    def __ge__(a, b):
        return a._richcmp(b, operator.ge)

    def __bool__(a):
        return bool(a._numerator)

    def __reduce__(self):
        return (self.__class__, (self._numerator, self._denominator))

    def __copy__(self):
        if type(self) == Fraction:
            return self
        return self.__class__(self._numerator, self._denominator)

    def __deepcopy__(self, memo):
        if type(self) == Fraction:
            return self
        return self.__class__(self._numerator, self._denominator)


def _general(spec):
    return True
