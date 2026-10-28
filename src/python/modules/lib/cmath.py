"""cmath for Nova's Python: complex functions, with CPython's algorithms
(cmathmodule.c) over math."""

import math as _m

pi = _m.pi
e = _m.e
tau = 2 * _m.pi
inf = float('inf')
nan = float('nan')
infj = complex(0.0, inf)
nanj = complex(0.0, nan)

_DBL_MIN = 2.2250738585072014e-308


def _c(z):
    if isinstance(z, complex):
        return z
    if isinstance(z, (int, float)):
        return complex(z, 0.0)
    if hasattr(z, '__complex__'):
        return z.__complex__()
    if hasattr(z, '__float__'):
        return complex(z.__float__(), 0.0)
    raise TypeError(f"must be real number, not {type(z).__name__}")


def phase(z):
    z = _c(z)
    return _m.atan2(z.imag, z.real)


def polar(z):
    z = _c(z)
    return (abs(z), _m.atan2(z.imag, z.real))


def rect(r, phi):
    if phi == 0:
        return complex(r, phi * r)
    return complex(r * _m.cos(phi), r * _m.sin(phi))


def sqrt(z):
    z = _c(z)
    if z.real == 0 and z.imag == 0:
        return complex(0.0, z.imag)
    ax = _m.fabs(z.real)
    ay = _m.fabs(z.imag)
    if ax < _DBL_MIN and ay < _DBL_MIN:
        ax = _m.ldexp(ax, 53)
        s = _m.ldexp(_m.sqrt(ax + _m.hypot(ax, _m.ldexp(ay, 53))), -(53 // 2) - 1)
    else:
        ax /= 8.0
        s = 2.0 * _m.sqrt(ax + _m.hypot(ax, ay / 8.0))
    d = ay / (2.0 * s)
    if z.real >= 0:
        return complex(s, _m.copysign(d, z.imag))
    return complex(d, _m.copysign(s, z.imag))


def exp(z):
    z = _c(z)
    if z.imag == 0:
        return complex(_m.exp(z.real), z.imag)
    l = _m.exp(z.real)
    return complex(l * _m.cos(z.imag), l * _m.sin(z.imag))


def _log(z):
    ax = _m.fabs(z.real)
    ay = _m.fabs(z.imag)
    if ax == 0 and ay == 0:
        raise ValueError('math domain error')
    h = _m.hypot(ax, ay)
    if 0.71 <= h <= 1.73:
        am = max(ax, ay)
        an = min(ax, ay)
        real = _m.log1p((am - 1) * (am + 1) + an * an) / 2.0
    else:
        real = _m.log(h)
    return complex(real, _m.atan2(z.imag, z.real))


def log(z, base=None):
    r = _log(_c(z))
    if base is not None:
        r = r / _log(_c(base))
    return r


def log10(z):
    r = _log(_c(z))
    return complex(r.real / _m.log(10), r.imag / _m.log(10))


def sin(z):
    z = _c(z)
    return complex(_m.sin(z.real) * _m.cosh(z.imag), _m.cos(z.real) * _m.sinh(z.imag))


def cos(z):
    z = _c(z)
    return complex(_m.cos(z.real) * _m.cosh(z.imag), -_m.sin(z.real) * _m.sinh(z.imag))


def tan(z):
    return sin(z) / cos(z)


def sinh(z):
    z = _c(z)
    return complex(_m.sinh(z.real) * _m.cos(z.imag), _m.cosh(z.real) * _m.sin(z.imag))


def cosh(z):
    z = _c(z)
    return complex(_m.cosh(z.real) * _m.cos(z.imag), _m.sinh(z.real) * _m.sin(z.imag))


def tanh(z):
    return sinh(z) / cosh(z)


def asin(z):
    z = _c(z)
    w = log(complex(-z.imag, z.real) + sqrt(1 - z * z))
    return complex(w.imag, -w.real)


def acos(z):
    return complex(pi / 2, 0) - asin(z)


def atan(z):
    z = _c(z)
    return complex(0, 0.5) * (log(1 - complex(0, 1) * z) - log(1 + complex(0, 1) * z))


def isfinite(z):
    z = _c(z)
    return _m.isfinite(z.real) and _m.isfinite(z.imag)


def isinf(z):
    z = _c(z)
    return _m.isinf(z.real) or _m.isinf(z.imag)


def isnan(z):
    z = _c(z)
    return _m.isnan(z.real) or _m.isnan(z.imag)


def isclose(a, b, *, rel_tol=1e-09, abs_tol=0.0):
    a = _c(a)
    b = _c(b)
    if a == b:
        return True
    if isinf(a) or isinf(b):
        return False
    diff = abs(b - a)
    return diff <= rel_tol * abs(b) or diff <= rel_tol * abs(a) or diff <= abs_tol
