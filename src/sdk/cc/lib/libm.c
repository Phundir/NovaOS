/* libm.c -- <math.h> of Nova's C library.
 *
 * The transcendental functions are those of fdlibm (Sun, 1993), the
 * library glibc's own math descends from: exp, log, log10, pow, the
 * trigonometric functions and their inverses, cbrt -- same algorithms,
 * same constants, errors below one unit in the last place. Square root
 * is the processor's (`sqrtsd`, correctly rounded). Arguments of sin,
 * cos and tan beyond 2^19 * pi/2 are reduced exactly against 1280 bits
 * of 2/pi (the table the JavaScript engine of Nova already checks).
 *
 * Where fdlibm and today's glibc differ -- glibc's exp, log and pow are
 * now correctly rounded in nearly all cases -- a result may differ in
 * its last bit, which a 17-digit printf shows and a 6-digit one does
 * not. sinh, cosh, tanh, expm1, log1p, log2, exp2 and hypot are written
 * from the functions above, a little less accurate than glibc's. */

#include <math.h>
#include <stdint.h>
#include <string.h>

static unsigned long __bits(double x) {
    unsigned long u;
    memcpy(&u, &x, 8);
    return u;
}

static double __from(unsigned long u) {
    double x;
    memcpy(&x, &u, 8);
    return x;
}

static int __hi(double x) {
    return (int)(__bits(x) >> 32);
}

static unsigned __lo(double x) {
    return (unsigned)__bits(x);
}

static double __with_hi(double x, int hi) {
    return __from(((unsigned long)(unsigned)hi << 32) | __lo(x));
}

static double __with_lo(double x, unsigned lo) {
    return __from((__bits(x) & 0xffffffff00000000UL) | lo);
}

static double __words(int hi, unsigned lo) {
    return __from(((unsigned long)(unsigned)hi << 32) | lo);
}

/* ------------------------------------------------------------
 *  Classification, sign, rounding
 * ------------------------------------------------------------ */

int __isnan(double x) {
    return (__bits(x) & 0x7fffffffffffffffUL) > 0x7ff0000000000000UL;
}

int __isinf(double x) {
    return (__bits(x) & 0x7fffffffffffffffUL) == 0x7ff0000000000000UL;
}

int __isfinite(double x) {
    return (__bits(x) & 0x7ff0000000000000UL) != 0x7ff0000000000000UL;
}

int __signbit(double x) {
    return (int)(__bits(x) >> 63);
}

double sqrt(double x) {
    return __builtin_sqrt(x);
}

double fabs(double x) {
    return __from(__bits(x) & 0x7fffffffffffffffUL);
}

double copysign(double x, double y) {
    return __from((__bits(x) & 0x7fffffffffffffffUL) | (__bits(y) & 0x8000000000000000UL));
}

double trunc(double x) {
    unsigned long u = __bits(x);
    int e = (int)(u >> 52 & 0x7ff) - 0x3ff;
    if (e >= 52)
        return x;
    if (e < 0)
        return __from(u & 0x8000000000000000UL);
    return __from(u & ~(0x000fffffffffffffUL >> e));
}

double floor(double x) {
    double t = trunc(x);
    if (t != x && x < 0)
        return t - 1.0;
    return t;
}

double ceil(double x) {
    double t = trunc(x);
    if (t != x && x > 0)
        return t + 1.0;
    return t;
}

double round(double x) {
    double t = trunc(x);
    double d = x - t; /* exact */
    if (d >= 0.5)
        return t + 1.0;
    if (d <= -0.5)
        return t - 1.0;
    return t;
}

long lround(double x) {
    return (long)round(x);
}

/* To the nearest integer, ties to even: adding and removing 2^52 makes
 * the hardware round. */
double rint(double x) {
    unsigned long u = __bits(x);
    int e = (int)(u >> 52 & 0x7ff);
    double y;
    if (e >= 0x3ff + 52)
        return x;
    if (u >> 63) {
        y = x - 4503599627370496.0 + 4503599627370496.0;
        return y == 0 ? -0.0 : y;
    }
    y = x + 4503599627370496.0 - 4503599627370496.0;
    return y;
}

double nearbyint(double x) {
    return rint(x);
}

double fmax(double x, double y) {
    if (__isnan(x))
        return y;
    if (__isnan(y))
        return x;
    if (__signbit(x) != __signbit(y))
        return __signbit(x) ? y : x;
    return x < y ? y : x;
}

double fmin(double x, double y) {
    if (__isnan(x))
        return y;
    if (__isnan(y))
        return x;
    if (__signbit(x) != __signbit(y))
        return __signbit(x) ? x : y;
    return x < y ? x : y;
}

double fdim(double x, double y) {
    if (__isnan(x) || __isnan(y))
        return x + y;
    return x > y ? x - y : 0.0;
}

/* ------------------------------------------------------------
 *  Exponent and fraction
 * ------------------------------------------------------------ */

double frexp(double x, int *e) {
    unsigned long u = __bits(x);
    int ee = (int)(u >> 52 & 0x7ff);
    if (!ee) {
        if (x != 0) {
            x = frexp(x * 18446744073709551616.0, e); /* 2^64 */
            *e -= 64;
        } else {
            *e = 0;
        }
        return x;
    }
    if (ee == 0x7ff) {
        *e = 0;
        return x;
    }
    *e = ee - 0x3fe;
    u &= 0x800fffffffffffffUL;
    u |= 0x3fe0000000000000UL;
    return __from(u);
}

/* x * 2^n, rounded once even for a subnormal result (musl). */
double scalbn(double x, int n) {
    double y = x;
    if (n > 1023) {
        y *= 0x1p1023;
        n -= 1023;
        if (n > 1023) {
            y *= 0x1p1023;
            n -= 1023;
            if (n > 1023)
                n = 1023;
        }
    } else if (n < -1022) {
        /* Scale to just above the subnormal range first: the final
         * multiplication rounds only once. */
        y *= 0x1p-1022 * 0x1p53;
        n += 1022 - 53;
        if (n < -1022) {
            y *= 0x1p-1022 * 0x1p53;
            n += 1022 - 53;
            if (n < -1022)
                n = -1022;
        }
    }
    return y * __from((unsigned long)(0x3ff + n) << 52);
}

double ldexp(double x, int n) {
    return scalbn(x, n);
}

double modf(double x, double *ip) {
    double t = trunc(x);
    *ip = t;
    if (__isinf(x))
        return copysign(0.0, x);
    return copysign(x - t, x);
}

/* fmod: exact, by shifting and subtracting the integer significands
 * (musl's algorithm). */
double fmod(double x, double y) {
    unsigned long ux = __bits(x), uy = __bits(y);
    int ex = (int)(ux >> 52 & 0x7ff);
    int ey = (int)(uy >> 52 & 0x7ff);
    unsigned long sign = ux >> 63;
    unsigned long i;
    unsigned long uxi = ux;

    if ((uy << 1) == 0 || __isnan(y) || ex == 0x7ff)
        return (x * y) / (x * y);
    if ((ux << 1) <= (uy << 1)) {
        if ((ux << 1) == (uy << 1))
            return 0 * x;
        return x;
    }
    /* Normalize the significands. */
    if (!ex) {
        for (i = uxi << 12; i >> 63 == 0; ex--, i <<= 1)
            ;
        uxi <<= -ex + 1;
    } else {
        uxi &= (unsigned long)-1 >> 12;
        uxi |= 1UL << 52;
    }
    if (!ey) {
        for (i = uy << 12; i >> 63 == 0; ey--, i <<= 1)
            ;
        uy <<= -ey + 1;
    } else {
        uy &= (unsigned long)-1 >> 12;
        uy |= 1UL << 52;
    }
    /* x mod y */
    for (; ex > ey; ex--) {
        i = uxi - uy;
        if (i >> 63 == 0) {
            if (i == 0)
                return 0 * x;
            uxi = i;
        }
        uxi <<= 1;
    }
    i = uxi - uy;
    if (i >> 63 == 0) {
        if (i == 0)
            return 0 * x;
        uxi = i;
    }
    for (; uxi >> 52 == 0; uxi <<= 1, ex--)
        ;
    /* Scale back. */
    if (ex > 0) {
        uxi -= 1UL << 52;
        uxi |= (unsigned long)ex << 52;
    } else {
        uxi >>= -ex + 1;
    }
    uxi |= sign << 63;
    return __from(uxi);
}

double remainder(double x, double y) {
    double r, ay;
    if (__isnan(x) || __isnan(y) || __isinf(x) || y == 0)
        return (x * y) / (x * y);
    ay = fabs(y);
    r = fmod(x, 2 * ay); /* exact, |r| < 2|y| */
    if (fabs(r) > ay / 2 || (fabs(r) == ay / 2 && fabs(fmod(x, 2 * ay)) > ay)) {
        /* reduce to the nearest multiple of y, ties to even */
    }
    {
        double ar = fabs(r);
        double s = r < 0 ? -1.0 : 1.0;
        if (ar > 1.5 * ay || (ar == 1.5 * ay))
            ar -= 2 * ay;
        else if (ar > 0.5 * ay || (ar == 0.5 * ay && 0))
            ar -= ay;
        else if (ar == 0.5 * ay)
            ar = ar; /* tie with an even quotient: keep */
        r = s * ar;
    }
    return r == 0 ? copysign(0.0, x) : r;
}

/* ------------------------------------------------------------
 *  exp, log, log10, pow (fdlibm)
 * ------------------------------------------------------------ */

static const double __halF[2] = {0.5, -0.5};
static const double __ln2HI[2] = {6.93147180369123816490e-01, -6.93147180369123816490e-01};
static const double __ln2LO[2] = {1.90821492927058770002e-10, -1.90821492927058770002e-10};
static const double __huge = 1.0e+300;
static const double __tiny = 1.0e-300;
static const double __twom1000 = 9.33263618503218878990e-302;
static const double __P1 = 1.66666666666666019037e-01;
static const double __P2 = -2.77777777770155933842e-03;
static const double __P3 = 6.61375632143793436117e-05;
static const double __P4 = -1.65339022054652515390e-06;
static const double __P5 = 4.13813679705723846039e-08;

double exp(double x) {
    double y, hi = 0, lo = 0, c, t;
    int k = 0, xsb;
    unsigned hx;

    hx = (unsigned)__hi(x);
    xsb = (hx >> 31) & 1;
    hx &= 0x7fffffff;

    if (hx >= 0x40862E42) { /* |x| >= 709.78 */
        if (hx >= 0x7ff00000) {
            if (((hx & 0xfffff) | __lo(x)) != 0)
                return x + x; /* NaN */
            return xsb == 0 ? x : 0.0;
        }
        if (x > 7.09782712893383973096e+02)
            return __huge * __huge;
        if (x < -7.45133219101941108420e+02)
            return __twom1000 * __twom1000;
    }

    if (hx > 0x3fd62e42) { /* |x| > 0.5 ln2 */
        if (hx < 0x3FF0A2B2) { /* and |x| < 1.5 ln2 */
            hi = x - __ln2HI[xsb];
            lo = __ln2LO[xsb];
            k = 1 - xsb - xsb;
        } else {
            k = (int)(1.44269504088896338700e+00 * x + __halF[xsb]);
            t = k;
            hi = x - t * __ln2HI[0];
            lo = t * __ln2LO[0];
        }
        x = hi - lo;
    } else if (hx < 0x3e300000) { /* |x| < 2^-28 */
        if (__huge + x > 1.0)
            return 1.0 + x;
    } else {
        k = 0;
    }

    t = x * x;
    c = x - t * (__P1 + t * (__P2 + t * (__P3 + t * (__P4 + t * __P5))));
    if (k == 0)
        return 1.0 - ((x * c) / (c - 2.0) - x);
    y = 1.0 - ((lo - (x * c) / (2.0 - c)) - hi);
    if (k >= -1021)
        return __with_hi(y, __hi(y) + (k << 20));
    return __with_hi(y, __hi(y) + ((k + 1000) << 20)) * __twom1000;
}

static const double __ln2_hi = 6.93147180369123816490e-01;
static const double __ln2_lo = 1.90821492927058770002e-10;
static const double __two54 = 1.80143985094819840000e+16;
static const double __Lg1 = 6.666666666666735130e-01;
static const double __Lg2 = 3.999999999940941908e-01;
static const double __Lg3 = 2.857142874366239149e-01;
static const double __Lg4 = 2.222219843214978396e-01;
static const double __Lg5 = 1.818357216161805012e-01;
static const double __Lg6 = 1.531383769920937332e-01;
static const double __Lg7 = 1.479819860511658591e-01;

double log(double x) {
    double hfsq, f, s, z, R, w, t1, t2, dk;
    int k, hx, i, j;
    unsigned lx;

    hx = __hi(x);
    lx = __lo(x);
    k = 0;
    if (hx < 0x00100000) { /* x < 2^-1022 */
        if (((hx & 0x7fffffff) | lx) == 0)
            return -__two54 / 0.0; /* log(+-0) = -inf */
        if (hx < 0)
            return (x - x) / 0.0; /* log(-#) = NaN */
        k -= 54;
        x *= __two54; /* subnormal: scale up */
        hx = __hi(x);
    }
    if (hx >= 0x7ff00000)
        return x + x;
    k += (hx >> 20) - 1023;
    hx &= 0x000fffff;
    i = (hx + 0x95f64) & 0x100000;
    x = __with_hi(x, hx | (i ^ 0x3ff00000)); /* normalize x or x/2 */
    k += (i >> 20);
    f = x - 1.0;
    if ((0x000fffff & (2 + hx)) < 3) { /* |f| < 2^-20 */
        if (f == 0.0) {
            if (k == 0)
                return 0.0;
            dk = (double)k;
            return dk * __ln2_hi + dk * __ln2_lo;
        }
        R = f * f * (0.5 - 0.33333333333333333 * f);
        if (k == 0)
            return f - R;
        dk = (double)k;
        return dk * __ln2_hi - ((R - dk * __ln2_lo) - f);
    }
    s = f / (2.0 + f);
    dk = (double)k;
    z = s * s;
    i = hx - 0x6147a;
    w = z * z;
    j = 0x6b851 - hx;
    t1 = w * (__Lg2 + w * (__Lg4 + w * __Lg6));
    t2 = z * (__Lg1 + w * (__Lg3 + w * (__Lg5 + w * __Lg7)));
    i |= j;
    R = t2 + t1;
    if (i > 0) {
        hfsq = 0.5 * f * f;
        if (k == 0)
            return f - (hfsq - s * (hfsq + R));
        return dk * __ln2_hi - ((hfsq - (s * (hfsq + R) + dk * __ln2_lo)) - f);
    }
    if (k == 0)
        return f - s * (f - R);
    return dk * __ln2_hi - ((s * (f - R) - dk * __ln2_lo) - f);
}

double log10(double x) {
    double y, z;
    int i, k, hx;
    unsigned lx;

    hx = __hi(x);
    lx = __lo(x);
    k = 0;
    if (hx < 0x00100000) {
        if (((hx & 0x7fffffff) | lx) == 0)
            return -__two54 / 0.0;
        if (hx < 0)
            return (x - x) / 0.0;
        k -= 54;
        x *= __two54;
        hx = __hi(x);
    }
    if (hx >= 0x7ff00000)
        return x + x;
    k += (hx >> 20) - 1023;
    i = (int)(((unsigned)k & 0x80000000) >> 31);
    hx = (hx & 0x000fffff) | ((0x3ff - i) << 20);
    y = (double)(k + i);
    x = __with_hi(x, hx);
    z = y * 3.69423907715893078616e-13 + 4.34294481903251816668e-01 * log(x);
    return z + y * 3.01029995663611771306e-01;
}

double log2(double x) {
    int e;
    double m;
    if (__isnan(x) || x <= 0 || __isinf(x))
        return log(x) * 1.44269504088896338700e+00;
    m = frexp(x, &e);
    if (m == 0.5)
        return (double)(e - 1);
    if (m < 0.70710678118654752440) {
        m *= 2;
        e--;
    }
    return e + log(m) * 1.44269504088896338700e+00;
}

static const double __bp[2] = {1.0, 1.5};
static const double __dp_h[2] = {0.0, 5.84962487220764160156e-01};
static const double __dp_l[2] = {0.0, 1.35003920212974897128e-08};

double pow(double x, double y) {
    double z, ax, z_h, z_l, p_h, p_l;
    double y1, t1, t2, r, s, t, u, v, w;
    int i, j, k, yisint, n;
    int hx, hy, ix, iy;
    unsigned lx, ly;

    hx = __hi(x);
    lx = __lo(x);
    hy = __hi(y);
    ly = __lo(y);
    ix = hx & 0x7fffffff;
    iy = hy & 0x7fffffff;

    /* y == 0: x**0 = 1, and 1**y = 1 (C99), even for a NaN. */
    if ((iy | ly) == 0 || x == 1.0)
        return 1.0;

    if (ix > 0x7ff00000 || ((ix == 0x7ff00000) && (lx != 0)) || iy > 0x7ff00000 || ((iy == 0x7ff00000) && (ly != 0)))
        return x + y;

    /* yisint: 0 not an integer, 1 odd, 2 even (when x < 0). */
    yisint = 0;
    if (hx < 0) {
        if (iy >= 0x43400000)
            yisint = 2;
        else if (iy >= 0x3ff00000) {
            k = (iy >> 20) - 0x3ff;
            if (k > 20) {
                j = (int)(ly >> (52 - k));
                if (((unsigned)j << (52 - k)) == ly)
                    yisint = 2 - (j & 1);
            } else if (ly == 0) {
                j = iy >> (20 - k);
                if ((j << (20 - k)) == iy)
                    yisint = 2 - (j & 1);
            }
        }
    }

    if (ly == 0) {
        if (iy == 0x7ff00000) { /* y is +-inf */
            if (((ix - 0x3ff00000) | lx) == 0)
                return 1.0; /* (-1)**+-inf is 1 (C99) */
            if (ix >= 0x3ff00000)
                return (hy >= 0) ? y : 0.0;
            return (hy < 0) ? -y : 0.0;
        }
        if (iy == 0x3ff00000) { /* y is +-1 */
            if (hy < 0)
                return 1.0 / x;
            return x;
        }
        if (hy == 0x40000000)
            return x * x; /* y is 2 */
        if (hy == 0x3fe00000) { /* y is 0.5 */
            if (hx >= 0)
                return sqrt(x);
        }
    }

    ax = fabs(x);
    if (lx == 0) {
        if (ix == 0x7ff00000 || ix == 0 || ix == 0x3ff00000) {
            z = ax; /* x is +-0, +-inf, +-1 */
            if (hy < 0)
                z = 1.0 / z;
            if (hx < 0) {
                if (((ix - 0x3ff00000) | yisint) == 0) {
                    z = (z - z) / (z - z); /* (-1)**non-int is NaN */
                } else if (yisint == 1)
                    z = -z;
            }
            return z;
        }
    }

    n = ((unsigned)hx >> 31) - 1;
    n = (hx >> 31) + 1;

    if ((n | yisint) == 0)
        return (x - x) / (x - x); /* (x<0)**(non-int) is NaN */

    s = 1.0;
    if ((n | (yisint - 1)) == 0)
        s = -1.0; /* (-ve)**(odd int) */

    if (iy > 0x41e00000) { /* |y| > 2^31 */
        if (iy > 0x43f00000) { /* |y| > 2^64: must over/underflow */
            if (ix <= 0x3fefffff)
                return (hy < 0) ? __huge * __huge : __tiny * __tiny;
            if (ix >= 0x3ff00000)
                return (hy > 0) ? __huge * __huge : __tiny * __tiny;
        }
        if (ix < 0x3fefffff)
            return (hy < 0) ? s * __huge * __huge : s * __tiny * __tiny;
        if (ix > 0x3ff00000)
            return (hy > 0) ? s * __huge * __huge : s * __tiny * __tiny;
        t = ax - 1.0;
        w = (t * t) * (0.5 - t * (0.3333333333333333333333 - t * 0.25));
        u = 1.44269502162933349609e+00 * t;
        v = t * 1.92596299112661746887e-08 - w * 1.44269504088896338700e+00;
        t1 = __with_lo(u + v, 0);
        t2 = v - (t1 - u);
    } else {
        double ss, s2, s_h, s_l, t_h, t_l;
        n = 0;
        if (ix < 0x00100000) {
            ax *= 9007199254740992.0;
            n -= 53;
            ix = __hi(ax);
        }
        n += ((ix) >> 20) - 0x3ff;
        j = ix & 0x000fffff;
        ix = j | 0x3ff00000;
        if (j <= 0x3988E)
            k = 0;
        else if (j < 0xBB67A)
            k = 1;
        else {
            k = 0;
            n += 1;
            ix -= 0x00100000;
        }
        ax = __with_hi(ax, ix);

        u = ax - __bp[k];
        v = 1.0 / (ax + __bp[k]);
        ss = u * v;
        s_h = __with_lo(ss, 0);
        t_h = __words(((ix >> 1) | 0x20000000) + 0x00080000 + (k << 18), 0);
        t_l = ax - (t_h - __bp[k]);
        s_l = v * ((u - s_h * t_h) - s_h * t_l);
        s2 = ss * ss;
        r = s2 * s2 *
            (5.99999999999994648725e-01 +
             s2 * (4.28571428578550184252e-01 +
                   s2 * (3.33333329818377432918e-01 +
                         s2 * (2.72728123808534006489e-01 + s2 * (2.30660745775561754067e-01 + s2 * 2.06975017800338417784e-01)))));
        r += s_l * (s_h + ss);
        s2 = s_h * s_h;
        t_h = __with_lo(3.0 + s2 + r, 0);
        t_l = r - ((t_h - 3.0) - s2);
        u = s_h * t_h;
        v = s_l * t_h + t_l * ss;
        p_h = __with_lo(u + v, 0);
        p_l = v - (p_h - u);
        z_h = 9.61796700954437255859e-01 * p_h;
        z_l = -7.02846165095275826516e-09 * p_h + p_l * 9.61796693925975554329e-01 + __dp_l[k];
        t = (double)n;
        t1 = __with_lo(((z_h + z_l) + __dp_h[k]) + t, 0);
        t2 = z_l - (((t1 - t) - __dp_h[k]) - z_h);
    }

    /* (y1 + y2) * (t1 + t2) */
    y1 = __with_lo(y, 0);
    p_l = (y - y1) * t1 + y * t2;
    p_h = y1 * t1;
    z = p_l + p_h;
    j = __hi(z);
    i = (int)__lo(z);
    if (j >= 0x40900000) { /* z >= 1024 */
        if (((j - 0x40900000) | i) != 0)
            return s * __huge * __huge;
        if (p_l + 8.0085662595372944372e-17 > z - p_h)
            return s * __huge * __huge;
    } else if ((j & 0x7fffffff) >= 0x4090cc00) { /* z <= -1075 */
        if (((j - (int)0xc090cc00) | i) != 0)
            return s * __tiny * __tiny;
        if (p_l <= z - p_h)
            return s * __tiny * __tiny;
    }

    /* 2**(p_h + p_l) */
    i = j & 0x7fffffff;
    k = (i >> 20) - 0x3ff;
    n = 0;
    if (i > 0x3fe00000) { /* |z| > 0.5: n = [z + 0.5] */
        n = j + (0x00100000 >> (k + 1));
        k = ((n & 0x7fffffff) >> 20) - 0x3ff;
        t = __words(n & ~(0x000fffff >> k), 0);
        n = ((n & 0x000fffff) | 0x00100000) >> (20 - k);
        if (j < 0)
            n = -n;
        p_h -= t;
    }
    t = __with_lo(p_l + p_h, 0);
    u = t * 6.93147182464599609375e-01;
    v = (p_l - (t - p_h)) * 6.93147180559945286227e-01 + t * -1.90465429995776804525e-09;
    z = u + v;
    w = v - (z - u);
    t = z * z;
    t1 = z - t * (__P1 + t * (__P2 + t * (__P3 + t * (__P4 + t * __P5))));
    r = (z * t1) / (t1 - 2.0) - (w + z * w);
    z = 1.0 - (r - z);
    j = __hi(z);
    j += (n << 20);
    if ((j >> 20) <= 0)
        z = scalbn(z, n); /* subnormal output */
    else
        z = __with_hi(z, __hi(z) + (n << 20));
    return s * z;
}

double exp2(double x) {
    double n, f;
    if (__isnan(x) || __isinf(x))
        return __isinf(x) && x < 0 ? 0.0 : x + x;
    if (x > 1024)
        return __huge * __huge;
    if (x < -1075)
        return __tiny * __tiny;
    n = rint(x);
    f = x - n;
    if (f == 0)
        return scalbn(1.0, (int)n);
    return scalbn(exp(f * 6.93147180559945309417e-01), (int)n);
}

/* expm1 and log1p by Kahan's corrections of exp and log. */
double expm1(double x) {
    double u;
    if (fabs(x) < 1e-5)
        return x + x * x / 2 + x * x * x / 6;
    u = exp(x);
    if (u == 1.0)
        return x;
    if (u - 1.0 == -1.0)
        return -1.0;
    if (__isinf(u))
        return u;
    return (u - 1.0) * x / log(u);
}

double log1p(double x) {
    double u = 1.0 + x;
    if (u == 1.0)
        return x;
    if (__isinf(u))
        return u;
    return log(u) * x / (u - 1.0);
}

/* ------------------------------------------------------------
 *  Trigonometry (fdlibm)
 * ------------------------------------------------------------ */

static double __kernel_sin(double x, double y, int iy) {
    double z, r, v;
    int ix = __hi(x) & 0x7fffffff;
    if (ix < 0x3e400000) { /* |x| < 2^-27 */
        if ((int)x == 0)
            return x;
    }
    z = x * x;
    v = z * x;
    r = 8.33333333332248946124e-03 +
        z * (-1.98412698298579493134e-04 +
             z * (2.75573137070700676789e-06 + z * (-2.50507602534068634195e-08 + z * 1.58969099521155010221e-10)));
    if (iy == 0)
        return x + v * (-1.66666666666666324348e-01 + z * r);
    return x - ((z * (0.5 * y - v * r) - y) - v * -1.66666666666666324348e-01);
}

static double __kernel_cos(double x, double y) {
    double a, hz, z, r, qx;
    int ix = __hi(x) & 0x7fffffff;
    if (ix < 0x3e400000) {
        if ((int)x == 0)
            return 1.0;
    }
    z = x * x;
    r = z * (4.16666666666666019037e-02 +
             z * (-1.38888888888741095749e-03 +
                  z * (2.48015872894767294178e-05 +
                       z * (-2.75573143513906633035e-07 + z * (2.08757232129817482790e-09 + z * -1.13596475577881948265e-11)))));
    if (ix < 0x3FD33333) /* |x| < 0.3 */
        return 1.0 - (0.5 * z - (z * r - x * y));
    if (ix > 0x3fe90000) /* x > 0.78125 */
        qx = 0.28125;
    else
        qx = __words(ix - 0x00200000, 0); /* x/4 */
    hz = 0.5 * z - qx;
    a = 1.0 - qx;
    return a - (hz - (z * r - x * y));
}

static const double __T[13] = {
    3.33333333333334091986e-01, 1.33333333333201242699e-01, 5.39682539762260521377e-02, 2.18694882948595424599e-02,
    8.86323982359930005737e-03, 3.59207910759131235356e-03, 1.45620945432529025516e-03, 5.88041240820264096874e-04,
    2.46463134818469906812e-04, 7.81794442939557092300e-05, 7.14072491382608190305e-05, -1.85586374855275456654e-05,
    2.59073051863633712884e-05,
};

static double __kernel_tan(double x, double y, int iy) {
    double z, r, v, w, s;
    int ix, hx;
    hx = __hi(x);
    ix = hx & 0x7fffffff;
    if (ix < 0x3e300000) { /* |x| < 2^-28 */
        if ((int)x == 0) {
            if (((ix | __lo(x)) | (iy + 1)) == 0)
                return 1.0 / fabs(x);
            if (iy == 1)
                return x;
            {
                double a, t;
                z = w = x + y;
                z = __with_lo(z, 0);
                v = y - (z - x);
                t = a = -1.0 / w;
                t = __with_lo(t, 0);
                s = 1.0 + t * z;
                return t + a * (s + t * v);
            }
        }
    }
    if (ix >= 0x3FE59428) { /* |x| >= 0.6744 */
        if (hx < 0) {
            x = -x;
            y = -y;
        }
        z = 7.85398163397448278999e-01 - x;
        w = 3.06161699786838301793e-17 - y;
        x = z + w;
        y = 0.0;
    }
    z = x * x;
    w = z * z;
    r = __T[1] + w * (__T[3] + w * (__T[5] + w * (__T[7] + w * (__T[9] + w * __T[11]))));
    v = z * (__T[2] + w * (__T[4] + w * (__T[6] + w * (__T[8] + w * (__T[10] + w * __T[12])))));
    s = z * x;
    r = y + z * (s * (r + v) + y);
    r += __T[0] * s;
    w = x + r;
    if (ix >= 0x3FE59428) {
        v = (double)iy;
        return (double)(1 - ((hx >> 30) & 2)) * (v - 2.0 * (x - (w * w / (w + v) - r)));
    }
    if (iy == 1)
        return w;
    {
        double a, t;
        z = __with_lo(w, 0);
        v = r - (z - x);
        t = a = -1.0 / w;
        t = __with_lo(t, 0);
        s = 1.0 + t * z;
        return t + a * (s + t * v);
    }
}

/* 2/pi, 1280 bits (the table of src/web/js/num.rs). */
static const unsigned long __two_over_pi[20] = {
    0xa2f9836e4e441529UL, 0xfc2757d1f534ddc0UL, 0xdb6295993c439041UL, 0xfe5163abdebbc561UL, 0xb7246e3a424dd2e0UL,
    0x06492eea09d1921cUL, 0xfe1deb1cb129a73eUL, 0xe88235f52ebb4484UL, 0xe99c7026b45f7e41UL, 0x3991d639835339f4UL,
    0x9c845f8bbdf9283bUL, 0x1ff897ffde05980fUL, 0xef2f118b5a0a6d1fUL, 0x6d367ecf27cb09b7UL, 0x4f463f669e5fea2dUL,
    0x7527bac7ebe5f17bUL, 0x3d0739f78a5292eaUL, 0x6bfb5fb11f8d5d08UL, 0x56033046fc7b6babUL, 0xf0cfbc209af4361dUL,
};

/* r = a * b, little-endian 32-bit limbs. */
static void __limb_mul(const uint32_t *a, int na, const uint32_t *b, int nb, uint32_t *r) {
    int i, j;
    for (i = 0; i < na + nb; i++)
        r[i] = 0;
    for (i = 0; i < na; i++) {
        uint64_t carry = 0;
        for (j = 0; j < nb; j++) {
            uint64_t t = (uint64_t)a[i] * b[j] + r[i + j] + carry;
            r[i + j] = (uint32_t)t;
            carry = t >> 32;
        }
        r[i + nb] = (uint32_t)carry;
    }
}

static int __limb_bit(const uint32_t *a, int bit) {
    return (int)((a[bit / 32] >> (bit % 32)) & 1);
}

/* Reduction of a huge argument: x = n * pi/2 + y[0] + y[1], computed
 * exactly from the bits of 2/pi (Payne and Hanek). */
static int __rem_pio2_large(double x, double *y) {
    unsigned long u = __bits(x) & 0x7fffffffffffffffUL;
    int e = (int)(u >> 52) - 1075; /* x = m * 2^e */
    unsigned long m = (u & 0x000fffffffffffffUL) | (1UL << 52);
    uint32_t mm[2], window[6], product[8], fraction[4], scaled[8];
    static const uint32_t pio2[4] = {0x701B839A, 0x9898CC51, 0x442D1846, 0x1921FB54}; /* 124 fraction bits */
    int i0 = e - 1 > 1 ? e - 1 : 1;
    int point, j, n, negative = 0, top;
    double head, tail;

    mm[0] = (uint32_t)m;
    mm[1] = (uint32_t)(m >> 32);
    for (j = 0; j < 6; j++)
        window[j] = 0;
    for (j = 0; j < 192; j++) {
        int index = i0 + j - 1; /* bit i0 + j of 2/pi, from its MSB */
        int bit = (int)((__two_over_pi[index / 64] >> (63 - index % 64)) & 1);
        if (bit)
            window[(191 - j) / 32] |= 1u << ((191 - j) % 32);
    }
    __limb_mul(mm, 2, window, 6, product);
    point = i0 + 191 - e;
    n = __limb_bit(product, point) + 2 * __limb_bit(product, point + 1);
    for (j = 0; j < 4; j++)
        fraction[j] = 0;
    for (j = 0; j < 128; j++)
        if (__limb_bit(product, point - 128 + j))
            fraction[j / 32] |= 1u << (j % 32);
    if (fraction[3] >> 31) {
        /* Past one half: the nearest multiple is the next one. */
        uint64_t borrow = 0;
        n++;
        negative = 1;
        for (j = 0; j < 4; j++) {
            uint64_t d = (uint64_t)0 - fraction[j] - borrow;
            fraction[j] = (uint32_t)d;
            borrow = (d >> 32) & 1;
        }
    }
    __limb_mul(fraction, 4, pio2, 4, scaled); /* value * 2^-252 */
    for (top = 255; top >= 0 && !__limb_bit(scaled, top); top--)
        ;
    if (top < 0) {
        y[0] = y[1] = 0.0;
    } else {
        unsigned long h = 0, t = 0;
        for (j = 0; j < 53; j++) {
            int bit = top - j;
            h = (h << 1) | (unsigned long)(bit >= 0 ? __limb_bit(scaled, bit) : 0);
        }
        for (j = 53; j < 106; j++) {
            int bit = top - j;
            t = (t << 1) | (unsigned long)(bit >= 0 ? __limb_bit(scaled, bit) : 0);
        }
        head = scalbn((double)h, top - 52 - 252);
        tail = scalbn((double)t, top - 105 - 252);
        y[0] = head + tail;
        y[1] = tail - (y[0] - head);
    }
    if (negative) {
        y[0] = -y[0];
        y[1] = -y[1];
    }
    if (__bits(x) >> 63) {
        y[0] = -y[0];
        y[1] = -y[1];
        return -n;
    }
    return n;
}

static const double __invpio2 = 6.36619772367581382433e-01;
static const double __pio2_1 = 1.57079632673412561417e+00;
static const double __pio2_1t = 6.07710050650619224932e-11;
static const double __pio2_2 = 6.07710050630396597660e-11;
static const double __pio2_2t = 2.02226624879595063154e-21;
static const double __pio2_3 = 2.02226624871116645580e-21;
static const double __pio2_3t = 8.47842766036889956997e-32;

static int __rem_pio2(double x, double *y) {
    double z, w, t, r, fn;
    int i, j, n, ix, hx;

    hx = __hi(x);
    ix = hx & 0x7fffffff;
    if (ix <= 0x3fe921fb) { /* |x| <= pi/4 */
        y[0] = x;
        y[1] = 0;
        return 0;
    }
    if (ix < 0x4002d97c) { /* |x| < 3pi/4: n = +-1 */
        if (hx > 0) {
            z = x - __pio2_1;
            if (ix != 0x3ff921fb) {
                y[0] = z - __pio2_1t;
                y[1] = (z - y[0]) - __pio2_1t;
            } else {
                z -= __pio2_2;
                y[0] = z - __pio2_2t;
                y[1] = (z - y[0]) - __pio2_2t;
            }
            return 1;
        }
        z = x + __pio2_1;
        if (ix != 0x3ff921fb) {
            y[0] = z + __pio2_1t;
            y[1] = (z - y[0]) + __pio2_1t;
        } else {
            z += __pio2_2;
            y[0] = z + __pio2_2t;
            y[1] = (z - y[0]) + __pio2_2t;
        }
        return -1;
    }
    if (ix <= 0x413921fb) { /* |x| <= 2^19 * pi/2 */
        t = fabs(x);
        n = (int)(t * __invpio2 + 0.5);
        fn = (double)n;
        r = t - fn * __pio2_1;
        w = fn * __pio2_1t;
        j = ix >> 20;
        y[0] = r - w;
        i = j - ((__hi(y[0]) >> 20) & 0x7ff);
        if (i > 16) { /* second iteration, good to 118 bits */
            t = r;
            w = fn * __pio2_2;
            r = t - w;
            w = fn * __pio2_2t - ((t - r) - w);
            y[0] = r - w;
            i = j - ((__hi(y[0]) >> 20) & 0x7ff);
            if (i > 49) { /* third, 151 bits */
                t = r;
                w = fn * __pio2_3;
                r = t - w;
                w = fn * __pio2_3t - ((t - r) - w);
                y[0] = r - w;
            }
        }
        y[1] = (r - y[0]) - w;
        if (hx < 0) {
            y[0] = -y[0];
            y[1] = -y[1];
            return -n;
        }
        return n;
    }
    if (ix >= 0x7ff00000) {
        y[0] = y[1] = x - x;
        return 0;
    }
    return __rem_pio2_large(x, y);
}

double sin(double x) {
    double y[2];
    int n, ix = __hi(x) & 0x7fffffff;
    if (ix <= 0x3fe921fb)
        return __kernel_sin(x, 0.0, 0);
    if (ix >= 0x7ff00000)
        return x - x;
    n = __rem_pio2(x, y);
    switch (n & 3) {
    case 0:
        return __kernel_sin(y[0], y[1], 1);
    case 1:
        return __kernel_cos(y[0], y[1]);
    case 2:
        return -__kernel_sin(y[0], y[1], 1);
    default:
        return -__kernel_cos(y[0], y[1]);
    }
}

double cos(double x) {
    double y[2];
    int n, ix = __hi(x) & 0x7fffffff;
    if (ix <= 0x3fe921fb)
        return __kernel_cos(x, 0.0);
    if (ix >= 0x7ff00000)
        return x - x;
    n = __rem_pio2(x, y);
    switch (n & 3) {
    case 0:
        return __kernel_cos(y[0], y[1]);
    case 1:
        return -__kernel_sin(y[0], y[1], 1);
    case 2:
        return -__kernel_cos(y[0], y[1]);
    default:
        return __kernel_sin(y[0], y[1], 1);
    }
}

double tan(double x) {
    double y[2];
    int n, ix = __hi(x) & 0x7fffffff;
    if (ix <= 0x3fe921fb)
        return __kernel_tan(x, 0.0, 1);
    if (ix >= 0x7ff00000)
        return x - x;
    n = __rem_pio2(x, y);
    return __kernel_tan(y[0], y[1], 1 - ((n & 1) << 1));
}

static const double __atanhi[4] = {
    4.63647609000806093515e-01, 7.85398163397448278999e-01, 9.82793723247329054082e-01, 1.57079632679489655800e+00};
static const double __atanlo[4] = {
    2.26987774529616870924e-17, 3.06161699786838301793e-17, 1.39033110312309984516e-17, 6.12323399573676603587e-17};
static const double __aT[11] = {
    3.33333333333329318027e-01,  -1.99999999998764832476e-01, 1.42857142725034663711e-01, -1.11111104054623557880e-01,
    9.09088713343650656196e-02,  -7.69187620504482999495e-02, 6.66107313738753120669e-02, -5.83357013379057348645e-02,
    4.97687799461593236017e-02,  -3.65315727442169155270e-02, 1.62858201153657823623e-02,
};

double atan(double x) {
    double w, s1, s2, z;
    int ix, hx, id;
    hx = __hi(x);
    ix = hx & 0x7fffffff;
    if (ix >= 0x44100000) { /* |x| >= 2^66 */
        if (ix > 0x7ff00000 || (ix == 0x7ff00000 && (__lo(x) != 0)))
            return x + x;
        if (hx > 0)
            return __atanhi[3] + __atanlo[3];
        return -__atanhi[3] - __atanlo[3];
    }
    if (ix < 0x3fdc0000) { /* |x| < 0.4375 */
        if (ix < 0x3e200000) { /* |x| < 2^-29 */
            if (__huge + x > 1.0)
                return x;
        }
        id = -1;
    } else {
        x = fabs(x);
        if (ix < 0x3ff30000) { /* |x| < 1.1875 */
            if (ix < 0x3fe60000) {
                id = 0;
                x = (2.0 * x - 1.0) / (2.0 + x);
            } else {
                id = 1;
                x = (x - 1.0) / (x + 1.0);
            }
        } else {
            if (ix < 0x40038000) { /* |x| < 2.4375 */
                id = 2;
                x = (x - 1.5) / (1.0 + 1.5 * x);
            } else {
                id = 3;
                x = -1.0 / x;
            }
        }
    }
    z = x * x;
    w = z * z;
    s1 = z * (__aT[0] + w * (__aT[2] + w * (__aT[4] + w * (__aT[6] + w * (__aT[8] + w * __aT[10])))));
    s2 = w * (__aT[1] + w * (__aT[3] + w * (__aT[5] + w * (__aT[7] + w * __aT[9]))));
    if (id < 0)
        return x - x * (s1 + s2);
    z = __atanhi[id] - ((x * (s1 + s2) - __atanlo[id]) - x);
    return (hx < 0) ? -z : z;
}

double atan2(double y, double x) {
    double z;
    int k, m, hx, hy, ix, iy;
    unsigned lx, ly;
    static const double pi_o_4 = 7.8539816339744827900E-01;
    static const double pi_o_2 = 1.5707963267948965580E+00;
    static const double pi = 3.1415926535897931160E+00;
    static const double pi_lo = 1.2246467991473531772E-16;

    hx = __hi(x);
    ix = hx & 0x7fffffff;
    lx = __lo(x);
    hy = __hi(y);
    iy = hy & 0x7fffffff;
    ly = __lo(y);
    if (__isnan(x) || __isnan(y))
        return x + y;
    if (((hx - 0x3ff00000) | lx) == 0)
        return atan(y); /* x = 1.0 */
    m = ((hy >> 31) & 1) | ((hx >> 30) & 2); /* 2*sign(x) + sign(y) */

    if ((iy | ly) == 0) { /* y = 0 */
        switch (m) {
        case 0:
        case 1:
            return y;
        case 2:
            return pi + __tiny;
        default:
            return -pi - __tiny;
        }
    }
    if ((ix | lx) == 0)
        return (hy < 0) ? -pi_o_2 - __tiny : pi_o_2 + __tiny;

    if (ix == 0x7ff00000) {
        if (iy == 0x7ff00000) {
            switch (m) {
            case 0:
                return pi_o_4 + __tiny;
            case 1:
                return -pi_o_4 - __tiny;
            case 2:
                return 3.0 * pi_o_4 + __tiny;
            default:
                return -3.0 * pi_o_4 - __tiny;
            }
        }
        switch (m) {
        case 0:
            return 0.0;
        case 1:
            return -0.0;
        case 2:
            return pi + __tiny;
        default:
            return -pi - __tiny;
        }
    }
    if (iy == 0x7ff00000)
        return (hy < 0) ? -pi_o_2 - __tiny : pi_o_2 + __tiny;

    k = (iy - ix) >> 20;
    if (k > 60)
        z = pi_o_2 + 0.5 * pi_lo; /* |y/x| > 2^60 */
    else if (hx < 0 && k < -60)
        z = 0.0; /* |y|/x < -2^60 */
    else
        z = atan(fabs(y / x));
    switch (m) {
    case 0:
        return z;
    case 1:
        return -z;
    case 2:
        return pi - (z - pi_lo);
    default:
        return (z - pi_lo) - pi;
    }
}

static const double __pio2_hi = 1.57079632679489655800e+00;
static const double __pio2_lo = 6.12323399573676603587e-17;
static const double __pio4_hi = 7.85398163397448278999e-01;

static double __asin_r(double t) {
    double p = t * (1.66666666666666657415e-01 +
                    t * (-3.25565818622400915405e-01 +
                         t * (2.01212532134862925881e-01 +
                              t * (-4.00555345006794114027e-02 + t * (7.91534994289814532176e-04 + t * 3.47933107596021167570e-05)))));
    double q = 1.0 + t * (-2.40339491173441421878e+00 +
                          t * (2.02094576023350569471e+00 + t * (-6.88283971605453293030e-01 + t * 7.70381505559019352791e-02)));
    return p / q;
}

double asin(double x) {
    double t, w, p, q, c, r, s;
    int hx, ix;
    hx = __hi(x);
    ix = hx & 0x7fffffff;
    if (ix >= 0x3ff00000) { /* |x| >= 1 */
        if (((ix - 0x3ff00000) | __lo(x)) == 0)
            return x * __pio2_hi + x * __pio2_lo;
        return (x - x) / (x - x);
    }
    if (ix < 0x3fe00000) { /* |x| < 0.5 */
        if (ix < 0x3e400000) {
            if (__huge + x > 1.0)
                return x;
        }
        t = x * x;
        return x + x * __asin_r(t);
    }
    w = 1.0 - fabs(x);
    t = w * 0.5;
    s = sqrt(t);
    if (ix >= 0x3FEF3333) { /* |x| > 0.975 */
        w = __asin_r(t);
        t = __pio2_hi - (2.0 * (s + s * w) - __pio2_lo);
    } else {
        w = __with_lo(s, 0);
        c = (t - w * w) / (s + w);
        r = __asin_r(t);
        p = 2.0 * s * r - (__pio2_lo - 2.0 * c);
        q = __pio4_hi - 2.0 * w;
        t = __pio4_hi - (p - q);
    }
    return hx > 0 ? t : -t;
}

double acos(double x) {
    double z, r, w, s, c, df;
    int hx, ix;
    hx = __hi(x);
    ix = hx & 0x7fffffff;
    if (ix >= 0x3ff00000) { /* |x| >= 1 */
        if (((ix - 0x3ff00000) | __lo(x)) == 0) {
            if (hx > 0)
                return 0.0;
            return 3.14159265358979311600e+00 + 2.0 * __pio2_lo;
        }
        return (x - x) / (x - x);
    }
    if (ix < 0x3fe00000) { /* |x| < 0.5 */
        if (ix <= 0x3c600000)
            return __pio2_hi + __pio2_lo;
        z = x * x;
        r = __asin_r(z);
        return __pio2_hi - (x - (__pio2_lo - x * r));
    }
    if (hx < 0) { /* x < -0.5 */
        z = (1.0 + x) * 0.5;
        s = sqrt(z);
        r = __asin_r(z);
        w = r * s - __pio2_lo;
        return 3.14159265358979311600e+00 - 2.0 * (s + w);
    }
    z = (1.0 - x) * 0.5; /* x > 0.5 */
    s = sqrt(z);
    df = __with_lo(s, 0);
    c = (z - df * df) / (s + df);
    r = __asin_r(z);
    w = r * s + c;
    return 2.0 * (df + w);
}

/* ------------------------------------------------------------
 *  Hyperbolic functions, cbrt, hypot
 * ------------------------------------------------------------ */

double sinh(double x) {
    double t, h = x < 0 ? -0.5 : 0.5;
    double ax = fabs(x);
    if (__isnan(x) || __isinf(x))
        return x + x;
    if (ax < 22) {
        if (ax < 0x1p-28)
            return x;
        t = expm1(ax);
        if (ax < 1)
            return h * (2.0 * t - t * t / (t + 1.0));
        return h * (t + t / (t + 1.0));
    }
    if (ax < 709.78)
        return h * exp(ax);
    t = exp(0.5 * ax);
    return (h * t) * t;
}

double cosh(double x) {
    double t, ax = fabs(x);
    if (__isnan(x))
        return x + x;
    if (ax < 0.3465735902799726547) { /* ln2/2 */
        t = expm1(ax);
        if (ax < 0x1p-55)
            return 1.0;
        return 1.0 + (t * t) / (2.0 * (1.0 + t));
    }
    if (ax < 22) {
        t = exp(ax);
        return 0.5 * t + 0.5 / t;
    }
    if (ax < 709.78)
        return 0.5 * exp(ax);
    t = exp(0.5 * ax);
    return (0.5 * t) * t;
}

double tanh(double x) {
    double t, z, ax = fabs(x);
    if (__isnan(x))
        return x + x;
    if (ax < 22) {
        if (ax < 0x1p-55)
            return x;
        if (ax >= 1) {
            t = expm1(2.0 * ax);
            z = 1.0 - 2.0 / (t + 2.0);
        } else {
            t = expm1(-2.0 * ax);
            z = -t / (t + 2.0);
        }
    } else {
        z = 1.0;
    }
    return x < 0 ? -z : z;
}

double cbrt(double x) {
    int hx;
    double r, s, t = 0.0, w;
    unsigned sign;
    static const unsigned B1 = 715094163;
    static const unsigned B2 = 696219795;

    hx = __hi(x);
    sign = (unsigned)hx & 0x80000000;
    hx ^= (int)sign;
    if (hx >= 0x7ff00000)
        return x + x;
    if ((hx | (int)__lo(x)) == 0)
        return x;
    x = __with_hi(x, hx); /* |x| */
    if (hx < 0x00100000) { /* subnormal */
        t = __words(0x43500000, 0); /* 2^54 */
        t *= x;
        t = __with_hi(t, (int)((unsigned)__hi(t) / 3 + B2));
    } else {
        t = __words((int)((unsigned)hx / 3 + B1), 0);
    }
    /* to 23 bits */
    r = t * t / x;
    s = 5.42857142857142815906e-01 + r * t;
    t *= 3.57142857142857150787e-01 + 1.60714285714285720630e+00 / (s + 1.41428571428571436819e+00 + -7.05306122448979611050e-01 / s);
    /* chopped to 20 bits and made larger than cbrt(x) */
    t = __words(__hi(t) + 1, 0);
    /* one Newton step to 53 bits */
    s = t * t;
    r = x / s;
    w = t + t;
    r = (r - t) / (w + r);
    t = t + t * r;
    return __with_hi(t, (int)((unsigned)__hi(t) | sign));
}

double hypot(double x, double y) {
    double a = fabs(x), b = fabs(y), t;
    int scale = 0;
    if (__isinf(a) || __isinf(b))
        return __from(0x7ff0000000000000UL);
    if (__isnan(a) || __isnan(b))
        return a + b;
    if (a < b) {
        t = a;
        a = b;
        b = t;
    }
    if (b == 0)
        return a;
    /* Keep the squares away from overflow and underflow. */
    if (a > 0x1p500) {
        a *= 0x1p-600;
        b *= 0x1p-600;
        scale = 600;
    } else if (b < 0x1p-500) {
        a *= 0x1p600;
        b *= 0x1p600;
        scale = -600;
    }
    return scalbn(sqrt(a * a + b * b), scale);
}

/* ------------------------------------------------------------
 *  float forms
 * ------------------------------------------------------------ */

float sqrtf(float x) { return (float)sqrt(x); }
float fabsf(float x) { return (float)fabs(x); }
float floorf(float x) { return (float)floor(x); }
float ceilf(float x) { return (float)ceil(x); }
float powf(float x, float y) { return (float)pow(x, y); }
float sinf(float x) { return (float)sin(x); }
float cosf(float x) { return (float)cos(x); }
float expf(float x) { return (float)exp(x); }
float logf(float x) { return (float)log(x); }
