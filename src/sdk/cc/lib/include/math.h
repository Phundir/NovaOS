/* math.h -- Nova C library (double; the float forms convert) */
#ifndef _MATH_H
#define _MATH_H
#define HUGE_VAL (1.0 / 0.0)
#define INFINITY (1.0f / 0.0f)
#define NAN (0.0f / 0.0f)
#define M_E 2.7182818284590452354
#define M_LOG2E 1.4426950408889634074
#define M_LOG10E 0.43429448190325182765
#define M_LN2 0.69314718055994530942
#define M_LN10 2.30258509299404568402
#define M_PI 3.14159265358979323846
#define M_PI_2 1.57079632679489661923
#define M_PI_4 0.78539816339744830962
#define M_1_PI 0.31830988618379067154
#define M_2_PI 0.63661977236758134308
#define M_SQRT2 1.41421356237309504880
#define M_SQRT1_2 0.70710678118654752440
#define isnan(x) __isnan((double)(x))
#define isinf(x) __isinf((double)(x))
#define isfinite(x) __isfinite((double)(x))
#define signbit(x) __signbit((double)(x))
int __isnan(double x);
int __isinf(double x);
int __isfinite(double x);
int __signbit(double x);
double sqrt(double x);
double fabs(double x);
double floor(double x);
double ceil(double x);
double trunc(double x);
double round(double x);
long lround(double x);
double rint(double x);
double nearbyint(double x);
double fmod(double x, double y);
double remainder(double x, double y);
double frexp(double x, int *e);
double ldexp(double x, int e);
double scalbn(double x, int e);
double modf(double x, double *i);
double copysign(double x, double y);
double fmax(double x, double y);
double fmin(double x, double y);
double fdim(double x, double y);
double hypot(double x, double y);
double exp(double x);
double exp2(double x);
double expm1(double x);
double log(double x);
double log2(double x);
double log10(double x);
double log1p(double x);
double pow(double x, double y);
double cbrt(double x);
double sin(double x);
double cos(double x);
double tan(double x);
double asin(double x);
double acos(double x);
double atan(double x);
double atan2(double y, double x);
double sinh(double x);
double cosh(double x);
double tanh(double x);
float sqrtf(float x);
float fabsf(float x);
float floorf(float x);
float ceilf(float x);
float powf(float x, float y);
float sinf(float x);
float cosf(float x);
float expf(float x);
float logf(float x);
#endif
