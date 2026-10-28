/* stdlib.h -- Nova C library */
#ifndef _STDLIB_H
#define _STDLIB_H
#include <stddef.h>
#define EXIT_SUCCESS 0
#define EXIT_FAILURE 1
#define RAND_MAX 2147483647
typedef struct { int quot; int rem; } div_t;
typedef struct { long quot; long rem; } ldiv_t;
typedef struct { long long quot; long long rem; } lldiv_t;
void *malloc(size_t size);
void *calloc(size_t count, size_t size);
void *realloc(void *p, size_t size);
void free(void *p);
void exit(int status);
void _Exit(int status);
void abort(void);
int atexit(void (*function)(void));
int abs(int n);
long labs(long n);
long long llabs(long long n);
div_t div(int a, int b);
ldiv_t ldiv(long a, long b);
lldiv_t lldiv(long long a, long long b);
int atoi(const char *s);
long atol(const char *s);
long long atoll(const char *s);
double atof(const char *s);
long strtol(const char *s, char **end, int base);
unsigned long strtoul(const char *s, char **end, int base);
long long strtoll(const char *s, char **end, int base);
unsigned long long strtoull(const char *s, char **end, int base);
double strtod(const char *s, char **end);
float strtof(const char *s, char **end);
void qsort(void *base, size_t count, size_t size, int (*compare)(const void *, const void *));
void *bsearch(const void *key, const void *base, size_t count, size_t size, int (*compare)(const void *, const void *));
int rand(void);
void srand(unsigned seed);
char *getenv(const char *name);
int system(const char *command);
#endif
