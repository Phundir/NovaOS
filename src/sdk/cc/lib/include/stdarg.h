/* stdarg.h -- Nova C library
 *
 * Every argument is passed on the stack, one 8-byte word per scalar
 * (sdk::cc::gen): a va_list is a pointer that walks those words. */
#ifndef _STDARG_H
#define _STDARG_H
typedef char *va_list;
typedef char *__gnuc_va_list;
#define __va_words(type) ((sizeof(type) + 7) & ~7UL)
#define va_start(ap, last) ((ap) = __builtin_va_start())
#define va_arg(ap, type) (*(type *)(((ap) += __va_words(type)) - __va_words(type)))
#define va_end(ap) ((void)(ap))
#define va_copy(dest, src) ((dest) = (src))
#endif
