"""numbers for Nova's Python: the numeric tower of PEP 3141.

The abstract classes carry no behavior of their own beyond the derived
operations CPython defines on them; the built-in types are registered so
that isinstance(5, numbers.Integral) holds as in CPython.
"""

from abc import ABCMeta, abstractmethod

__all__ = ["Number", "Complex", "Real", "Rational", "Integral"]


class Number(metaclass=ABCMeta):
    __slots__ = ()
    __hash__ = None


class Complex(Number):
    __slots__ = ()

    def __bool__(self):
        return self != 0

    @property
    def imag(self):
        return 0

    def conjugate(self):
        return +self


class Real(Complex):
    __slots__ = ()

    def __complex__(self):
        return complex(float(self))

    @property
    def real(self):
        return +self

    @property
    def imag(self):
        return 0

    def conjugate(self):
        return +self

    def __divmod__(self, other):
        return (self // other, self % other)

    def __rdivmod__(self, other):
        return (other // self, other % self)


class Rational(Real):
    __slots__ = ()

    def __float__(self):
        return int(self.numerator) / int(self.denominator)


class Integral(Rational):
    __slots__ = ()

    def __float__(self):
        return float(int(self))

    def __index__(self):
        return int(self)

    @property
    def numerator(self):
        return +self

    @property
    def denominator(self):
        return 1


Complex.register(complex)
Real.register(float)
Integral.register(int)
