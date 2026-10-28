/* assert.h -- Nova C library */
#undef assert
#ifdef NDEBUG
#define assert(e) ((void)0)
#else
void __assert_fail(const char *expression, const char *file, int line, const char *function);
#define assert(e) ((e) ? (void)0 : __assert_fail(#e, __FILE__, __LINE__, __func__))
#endif
#ifndef static_assert
#define static_assert _Static_assert
#endif
