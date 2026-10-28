/* stdio.h -- Nova C library
 *
 * A file is read whole when it is opened and written whole when it is
 * closed or flushed: Nova's filesystem calls work on whole files
 * (SYS_FS_READ, SYS_FS_OVERWRITE). The standard streams are streams. */
#ifndef _STDIO_H
#define _STDIO_H
#include <stddef.h>
#include <stdarg.h>
#define EOF (-1)
#define BUFSIZ 1024
#define FILENAME_MAX 4096
#define FOPEN_MAX 64
#define SEEK_SET 0
#define SEEK_CUR 1
#define SEEK_END 2
#define _IOFBF 0
#define _IOLBF 1
#define _IONBF 2
typedef struct __file FILE;
typedef long fpos_t;
extern FILE *stdin;
extern FILE *stdout;
extern FILE *stderr;
int printf(const char *format, ...);
int fprintf(FILE *stream, const char *format, ...);
int sprintf(char *s, const char *format, ...);
int snprintf(char *s, size_t n, const char *format, ...);
int vprintf(const char *format, va_list ap);
int vfprintf(FILE *stream, const char *format, va_list ap);
int vsprintf(char *s, const char *format, va_list ap);
int vsnprintf(char *s, size_t n, const char *format, va_list ap);
int scanf(const char *format, ...);
int fscanf(FILE *stream, const char *format, ...);
int sscanf(const char *s, const char *format, ...);
int vfscanf(FILE *stream, const char *format, va_list ap);
int vsscanf(const char *s, const char *format, va_list ap);
int putchar(int c);
int puts(const char *s);
int fputs(const char *s, FILE *stream);
int fputc(int c, FILE *stream);
int putc(int c, FILE *stream);
int getchar(void);
int fgetc(FILE *stream);
int getc(FILE *stream);
int ungetc(int c, FILE *stream);
char *fgets(char *s, int n, FILE *stream);
FILE *fopen(const char *path, const char *mode);
int fclose(FILE *stream);
int fflush(FILE *stream);
size_t fread(void *buffer, size_t size, size_t count, FILE *stream);
size_t fwrite(const void *buffer, size_t size, size_t count, FILE *stream);
int fseek(FILE *stream, long offset, int whence);
long ftell(FILE *stream);
void rewind(FILE *stream);
int feof(FILE *stream);
int ferror(FILE *stream);
void clearerr(FILE *stream);
int setvbuf(FILE *stream, char *buffer, int mode, size_t size);
void setbuf(FILE *stream, char *buffer);
int remove(const char *path);
int rename(const char *from, const char *to);
void perror(const char *s);
#endif
