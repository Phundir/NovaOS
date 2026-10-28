/* libc.c -- the C library of Nova's C compiler (sdk::cc).
 *
 * Compiled with every program as a translation unit of its own; the
 * code generator keeps only the functions a program reaches. Written in
 * the C that sdk::cc compiles -- which makes it the compiler's first and
 * largest test.
 *
 * Layers, from the bottom:
 *   1. the system layer: the only part that differs between Nova
 *      (__nova__, Nova system calls) and the Linux build of the host
 *      test bench (__linux__);
 *   2. memory (malloc), streams (FILE), formatted output and input;
 *   3. strings, conversions, sorting, random numbers, time.
 *
 * The behaviour follows glibc's wherever a program can see it: printf's
 * digits are exact (the algorithm of musl's fmt_fp, which rounds the
 * exact binary value), rand() gives glibc's sequence, qsort is a stable
 * merge sort, strtod rounds correctly. */

#include <stddef.h>
#include <stdarg.h>
#include <stdint.h>
#include <limits.h>
#include <errno.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <ctype.h>
#include <time.h>
#include <math.h>

int errno;

/* ============================================================
 *  1. System layer
 * ============================================================ */

static void __flush_all(void);
static void __exit_now(int status);

#ifdef __nova__

#define NOVA_DEBUG_PRINT 0x0001
#define NOVA_FS_READ 0x0101
#define NOVA_FS_OVERWRITE 0x0103
#define NOVA_FS_DELETE 0x0104
#define NOVA_FS_RENAME 0x0105
#define NOVA_PROC_EXIT 0x020B
#define NOVA_MEM_ALLOC 0x0301
#define NOVA_TIME_UPTIME_MS 0x0702
#define NOVA_TIME_NOW 0x0703

/* The console takes whole lines (SYS_DEBUG_PRINT adds the newline): the
 * bytes are gathered here up to the end of a line. Characters outside
 * printable ASCII are shown as '?', as the console of a ported program
 * does (posix::console_write). */
static char __console_line[512];
static int __console_length;

static void __console_flush(void) {
    __builtin_syscall(NOVA_DEBUG_PRINT, __console_line, __console_length);
    __console_length = 0;
}

static long __sys_write(int fd, const char *buffer, unsigned long n) {
    unsigned long i;
    (void)fd;
    for (i = 0; i < n; i++) {
        char c = buffer[i];
        if (c == '\n') {
            __console_flush();
            continue;
        }
        if (__console_length == (int)sizeof(__console_line))
            __console_flush();
        __console_line[__console_length++] = (c >= 32 && c < 127) || c == '\t' ? c : '?';
    }
    return (long)n;
}

static long __sys_read(int fd, char *buffer, unsigned long n) {
    (void)fd;
    (void)buffer;
    (void)n;
    return 0; /* a Nova program has no standard input yet */
}

static void __sys_exit(int status) {
    if (__console_length)
        __console_flush();
    for (;;)
        __builtin_syscall(NOVA_PROC_EXIT, status);
}

static void *__sys_alloc(unsigned long size) {
    long address = __builtin_syscall(NOVA_MEM_ALLOC, size, 0);
    return address < 0 ? NULL : (void *)address;
}

static long __sys_time(void) {
    long seconds = __builtin_syscall(NOVA_TIME_NOW);
    return seconds < 0 ? 0 : seconds;
}

static long __sys_clock_us(void) {
    long ms = __builtin_syscall(NOVA_TIME_UPTIME_MS);
    return ms < 0 ? 0 : ms * 1000;
}

/* Whole-file access: an offset read, a replacement. */
static long __sys_read_file(const char *path, char *buffer, unsigned long size, unsigned long offset) {
    return __builtin_syscall(NOVA_FS_READ, path, strlen(path), buffer, size, offset);
}

static long __sys_write_file(const char *path, const char *buffer, unsigned long size) {
    return __builtin_syscall(NOVA_FS_OVERWRITE, path, strlen(path), buffer, size, 0);
}

static long __sys_remove(const char *path) {
    return __builtin_syscall(NOVA_FS_DELETE, path, strlen(path));
}

static long __sys_rename(const char *from, const char *to) {
    return __builtin_syscall(NOVA_FS_RENAME, from, strlen(from), to, strlen(to));
}

#endif

#ifdef __linux__

#define LINUX_READ 0
#define LINUX_WRITE 1
#define LINUX_OPEN 2
#define LINUX_CLOSE 3
#define LINUX_LSEEK 8
#define LINUX_MMAP 9
#define LINUX_GETPID 39
#define LINUX_KILL 62
#define LINUX_RENAME 82
#define LINUX_UNLINK 87
#define LINUX_CLOCK_GETTIME 228
#define LINUX_EXIT_GROUP 231

static long __sys_write(int fd, const char *buffer, unsigned long n) {
    unsigned long done = 0;
    while (done < n) {
        long written = __builtin_syscall(LINUX_WRITE, fd, buffer + done, n - done);
        if (written <= 0)
            return -1;
        done += written;
    }
    return (long)done;
}

static long __sys_read(int fd, char *buffer, unsigned long n) {
    return __builtin_syscall(LINUX_READ, fd, buffer, n);
}

static void __sys_exit(int status) {
    for (;;)
        __builtin_syscall(LINUX_EXIT_GROUP, status);
}

static void *__sys_alloc(unsigned long size) {
    /* PROT_READ|PROT_WRITE, MAP_PRIVATE|MAP_ANONYMOUS */
    long address = __builtin_syscall(LINUX_MMAP, 0, size, 3, 0x22, -1, 0);
    return address < 0 && address > -4096 ? NULL : (void *)address;
}

static long __sys_time(void) {
    long spec[2];
    __builtin_syscall(LINUX_CLOCK_GETTIME, 0, spec);
    return spec[0];
}

static long __sys_clock_us(void) {
    long spec[2];
    __builtin_syscall(LINUX_CLOCK_GETTIME, 2, spec); /* CLOCK_PROCESS_CPUTIME_ID */
    return spec[0] * 1000000 + spec[1] / 1000;
}

static long __sys_read_file(const char *path, char *buffer, unsigned long size, unsigned long offset) {
    long fd = __builtin_syscall(LINUX_OPEN, path, 0, 0);
    long total = 0;
    if (fd < 0)
        return fd;
    __builtin_syscall(LINUX_LSEEK, fd, offset, 0);
    while ((unsigned long)total < size) {
        long got = __builtin_syscall(LINUX_READ, fd, buffer + total, size - total);
        if (got <= 0)
            break;
        total += got;
    }
    __builtin_syscall(LINUX_CLOSE, fd);
    return total;
}

static long __sys_write_file(const char *path, const char *buffer, unsigned long size) {
    /* O_WRONLY|O_CREAT|O_TRUNC, 0644 */
    long fd = __builtin_syscall(LINUX_OPEN, path, 01 | 0100 | 01000, 0644);
    long done;
    if (fd < 0)
        return fd;
    done = __sys_write((int)fd, buffer, size);
    __builtin_syscall(LINUX_CLOSE, fd);
    return done;
}

static long __sys_remove(const char *path) {
    return __builtin_syscall(LINUX_UNLINK, path);
}

static long __sys_rename(const char *from, const char *to) {
    return __builtin_syscall(LINUX_RENAME, from, to);
}

#endif

/* ============================================================
 *  2. Program start and end
 * ============================================================ */

int main(int argc, char **argv);

static char *__argv[2] = {"programme", 0};
static void (*__atexit_functions[32])(void);
static int __atexit_count;

void __cc_start(void) {
    exit(main(1, __argv));
}

int atexit(void (*function)(void)) {
    if (__atexit_count == 32)
        return -1;
    __atexit_functions[__atexit_count++] = function;
    return 0;
}

void exit(int status) {
    while (__atexit_count > 0)
        __atexit_functions[--__atexit_count]();
    __flush_all();
    __exit_now(status);
}

void _Exit(int status) {
    __exit_now(status);
}

static void __exit_now(int status) {
    __sys_exit(status);
}

void abort(void) {
    __flush_all();
#ifdef __linux__
    /* SIGABRT, as glibc: the shell then sees 128 + 6. */
    __builtin_syscall(LINUX_KILL, __builtin_syscall(LINUX_GETPID), 6);
#endif
    __exit_now(134);
}

void __assert_fail(const char *expression, const char *file, int line, const char *function) {
    fprintf(stderr, "%s: %s:%d: %s: Assertion `%s' failed.\n", __argv[0], file, line, function, expression);
    abort();
}

char *getenv(const char *name) {
    (void)name;
    return NULL;
}

int system(const char *command) {
    (void)command;
    return -1;
}

/* ============================================================
 *  3. Memory
 *
 * Blocks carry a header of two words: their size and, while free, the
 * next free block. Small blocks (up to 512 bytes, by 16) are kept on a
 * list per size, as the kernel heap does; larger ones on one list sorted
 * by address, merged with their neighbours. Memory comes from the
 * system by 1 MiB at least and is never given back.
 * ============================================================ */

struct __block {
    unsigned long size; /* header included */
    struct __block *next;
};

#define __HEADER 16
#define __QUICK_MAX 512
static struct __block *__quick[__QUICK_MAX / 16 + 1];
static struct __block *__free_list;

static void __free_insert(struct __block *block) {
    struct __block **link = &__free_list;
    while (*link && *link < block)
        link = &(*link)->next;
    block->next = *link;
    *link = block;
    /* Merge with the next, then with the previous. */
    if (block->next && (char *)block + block->size == (char *)block->next) {
        block->size += block->next->size;
        block->next = block->next->next;
    }
    if (link != &__free_list) {
        struct __block *previous = (struct __block *)((char *)link - offsetof(struct __block, next));
        if ((char *)previous + previous->size == (char *)block) {
            previous->size += block->size;
            previous->next = block->next;
        }
    }
}

void *malloc(size_t size) {
    unsigned long need;
    struct __block **link;
    struct __block *block;
    if (size > (1UL << 40))
        return NULL;
    need = (size + __HEADER + 15) & ~15UL;
    if (need < 32)
        need = 32;
    if (need <= __QUICK_MAX && __quick[need / 16]) {
        block = __quick[need / 16];
        __quick[need / 16] = block->next;
        return (char *)block + __HEADER;
    }
    for (;;) {
        for (link = &__free_list; *link; link = &(*link)->next) {
            block = *link;
            if (block->size >= need) {
                if (block->size - need >= 32) {
                    struct __block *rest = (struct __block *)((char *)block + need);
                    rest->size = block->size - need;
                    rest->next = block->next;
                    *link = rest;
                    block->size = need;
                } else {
                    *link = block->next;
                }
                return (char *)block + __HEADER;
            }
        }
        {
            unsigned long chunk = need < (1UL << 20) ? (1UL << 20) : (need + 4095) & ~4095UL;
            struct __block *fresh = __sys_alloc(chunk);
            if (!fresh) {
                errno = ENOMEM;
                return NULL;
            }
            fresh->size = chunk;
            __free_insert(fresh);
        }
    }
}

void free(void *p) {
    struct __block *block;
    if (!p)
        return;
    block = (struct __block *)((char *)p - __HEADER);
    if (block->size <= __QUICK_MAX) {
        block->next = __quick[block->size / 16];
        __quick[block->size / 16] = block;
        return;
    }
    __free_insert(block);
}

void *calloc(size_t count, size_t size) {
    void *p;
    if (size && count > (size_t)-1 / size)
        return NULL;
    p = malloc(count * size);
    if (p)
        memset(p, 0, count * size);
    return p;
}

void *realloc(void *p, size_t size) {
    struct __block *block;
    void *fresh;
    unsigned long have;
    if (!p)
        return malloc(size);
    if (!size) {
        free(p);
        return NULL;
    }
    block = (struct __block *)((char *)p - __HEADER);
    have = block->size - __HEADER;
    if (size <= have)
        return p;
    fresh = malloc(size);
    if (!fresh)
        return NULL;
    memcpy(fresh, p, have);
    free(p);
    return fresh;
}

/* ============================================================
 *  4. Streams
 * ============================================================ */

struct __file {
    int fd;           /* 0, 1, 2 for the standard streams; -1 for a file */
    int mode;         /* 1 read, 2 write, 4 append, 8 update */
    char *data;       /* a file: its whole content */
    unsigned long length;
    unsigned long capacity;
    unsigned long position;
    char *path;
    char *out;        /* a standard stream: pending output */
    int out_length;
    int unget;        /* -1, or a byte pushed back */
    int eof;
    int error;
    int dirty;
    char *in;         /* standard input: buffered bytes */
    int in_length;
    int in_position;
};

static char __stdout_buffer[4096];
static FILE __stdin_file = {0, 1, 0, 0, 0, 0, 0, 0, 0, -1, 0, 0, 0, 0, 0, 0};
static FILE __stdout_file = {1, 2, 0, 0, 0, 0, 0, __stdout_buffer, 0, -1, 0, 0, 0, 0, 0, 0};
static FILE __stderr_file = {2, 2, 0, 0, 0, 0, 0, 0, 0, -1, 0, 0, 0, 0, 0, 0};
FILE *stdin = &__stdin_file;
FILE *stdout = &__stdout_file;
FILE *stderr = &__stderr_file;

static FILE *__open_files[FOPEN_MAX];

static int __stream_flush(FILE *f) {
    if (f->fd >= 0) {
        if (f->out && f->out_length) {
            __sys_write(f->fd, f->out, f->out_length);
            f->out_length = 0;
        }
        return 0;
    }
    if (f->dirty) {
        if (__sys_write_file(f->path, f->data ? f->data : "", f->length) < 0) {
            f->error = 1;
            return EOF;
        }
        f->dirty = 0;
    }
    return 0;
}

static void __flush_all(void) {
    int i;
    __stream_flush(stdout);
    __stream_flush(stderr);
    for (i = 0; i < FOPEN_MAX; i++)
        if (__open_files[i])
            __stream_flush(__open_files[i]);
}

int fflush(FILE *f) {
    if (!f) {
        __flush_all();
        return 0;
    }
    return __stream_flush(f);
}

/* Writes n bytes to a stream. */
static int __stream_write(FILE *f, const char *s, unsigned long n) {
    unsigned long i;
    if (!(f->mode & (2 | 4 | 8))) {
        f->error = 1;
        return EOF;
    }
    if (f->fd >= 0) {
        if (!f->out) {
            __sys_write(f->fd, s, n);
            return 0;
        }
        for (i = 0; i < n; i++) {
            f->out[f->out_length++] = s[i];
            if (s[i] == '\n' || f->out_length == (int)sizeof(__stdout_buffer))
                __stream_flush(f);
        }
        return 0;
    }
    if (f->mode & 4)
        f->position = f->length;
    if (f->position + n > f->capacity) {
        unsigned long capacity = f->capacity ? f->capacity : 256;
        char *grown;
        while (capacity < f->position + n)
            capacity *= 2;
        grown = realloc(f->data, capacity);
        if (!grown) {
            f->error = 1;
            return EOF;
        }
        f->data = grown;
        f->capacity = capacity;
    }
    if (f->position > f->length)
        memset(f->data + f->length, 0, f->position - f->length);
    memcpy(f->data + f->position, s, n);
    f->position += n;
    if (f->position > f->length)
        f->length = f->position;
    f->dirty = 1;
    return 0;
}

/* One byte from a stream, or EOF. */
static int __stream_getc(FILE *f) {
    int c;
    if (f->unget >= 0) {
        c = f->unget;
        f->unget = -1;
        return c;
    }
    if (f->eof)
        return EOF;
    if (f->fd >= 0) {
        if (f->fd == 0)
            __stream_flush(stdout);
        if (!f->in) {
            f->in = malloc(4096);
            if (!f->in)
                return EOF;
        }
        if (f->in_position >= f->in_length) {
            long got = __sys_read(f->fd, f->in, 4096);
            if (got <= 0) {
                f->eof = 1;
                return EOF;
            }
            f->in_length = (int)got;
            f->in_position = 0;
        }
        return (unsigned char)f->in[f->in_position++];
    }
    if (!(f->mode & (1 | 8)) || f->position >= f->length) {
        f->eof = 1;
        return EOF;
    }
    return (unsigned char)f->data[f->position++];
}

FILE *fopen(const char *path, const char *mode) {
    FILE *f;
    int slot;
    int flags;
    for (slot = 0; slot < FOPEN_MAX && __open_files[slot]; slot++)
        ;
    if (slot == FOPEN_MAX) {
        errno = ENOMEM;
        return NULL;
    }
    switch (mode[0]) {
    case 'r':
        flags = 1;
        break;
    case 'w':
        flags = 2;
        break;
    case 'a':
        flags = 4;
        break;
    default:
        errno = EINVAL;
        return NULL;
    }
    if (strchr(mode, '+'))
        flags |= 8;
    f = calloc(1, sizeof(FILE));
    if (!f)
        return NULL;
    f->fd = -1;
    f->mode = flags;
    f->unget = -1;
    f->path = strdup(path);
    if (flags & (1 | 4)) {
        /* Read the whole file, a block at a time. */
        unsigned long capacity = 4096;
        f->data = malloc(capacity);
        for (;;) {
            long got = __sys_read_file(path, f->data + f->length, capacity - f->length, f->length);
            if (got < 0) {
                if (flags & 1) {
                    free(f->data);
                    free(f->path);
                    free(f);
                    errno = ENOENT;
                    return NULL;
                }
                break;
            }
            f->length += got;
            if (f->length < capacity)
                break;
            capacity *= 2;
            f->data = realloc(f->data, capacity);
        }
        f->capacity = capacity;
    }
    if (flags & 2)
        f->dirty = 1; /* "w" empties the file even if nothing is written */
    if (flags & 4)
        f->position = f->length;
    __open_files[slot] = f;
    return f;
}

int fclose(FILE *f) {
    int result;
    int i;
    if (f->fd >= 0)
        return __stream_flush(f);
    result = __stream_flush(f);
    for (i = 0; i < FOPEN_MAX; i++)
        if (__open_files[i] == f)
            __open_files[i] = NULL;
    free(f->data);
    free(f->path);
    free(f);
    return result;
}

size_t fwrite(const void *buffer, size_t size, size_t count, FILE *f) {
    if (!size || !count)
        return 0;
    if (__stream_write(f, buffer, size * count) == EOF)
        return 0;
    return count;
}

size_t fread(void *buffer, size_t size, size_t count, FILE *f) {
    unsigned char *out = buffer;
    size_t total = size * count;
    size_t i;
    if (!total)
        return 0;
    for (i = 0; i < total; i++) {
        int c = __stream_getc(f);
        if (c == EOF)
            break;
        out[i] = (unsigned char)c;
    }
    return i / size;
}

int fseek(FILE *f, long offset, int whence) {
    long base;
    if (f->fd >= 0)
        return -1;
    base = whence == SEEK_SET ? 0 : whence == SEEK_CUR ? (long)f->position : (long)f->length;
    if (base + offset < 0) {
        errno = EINVAL;
        return -1;
    }
    f->position = base + offset;
    f->eof = 0;
    f->unget = -1;
    return 0;
}

long ftell(FILE *f) {
    if (f->fd >= 0)
        return -1;
    return (long)f->position - (f->unget >= 0);
}

void rewind(FILE *f) {
    fseek(f, 0, SEEK_SET);
    f->error = 0;
}

int feof(FILE *f) {
    return f->eof;
}

int ferror(FILE *f) {
    return f->error;
}

void clearerr(FILE *f) {
    f->eof = 0;
    f->error = 0;
}

int setvbuf(FILE *f, char *buffer, int mode, size_t size) {
    (void)buffer;
    (void)size;
    if (f->fd >= 0 && mode == _IONBF) {
        __stream_flush(f);
        f->out = NULL;
    }
    return 0;
}

void setbuf(FILE *f, char *buffer) {
    if (!buffer)
        setvbuf(f, NULL, _IONBF, 0);
}

int remove(const char *path) {
    if (__sys_remove(path) < 0) {
        errno = ENOENT;
        return -1;
    }
    return 0;
}

int rename(const char *from, const char *to) {
    if (__sys_rename(from, to) < 0) {
        errno = ENOENT;
        return -1;
    }
    return 0;
}

int fputc(int c, FILE *f) {
    char byte = (char)c;
    if (__stream_write(f, &byte, 1) == EOF)
        return EOF;
    return (unsigned char)c;
}

int putc(int c, FILE *f) {
    return fputc(c, f);
}

int putchar(int c) {
    return fputc(c, stdout);
}

int fputs(const char *s, FILE *f) {
    if (__stream_write(f, s, strlen(s)) == EOF)
        return EOF;
    return 1;
}

int puts(const char *s) {
    if (fputs(s, stdout) == EOF)
        return EOF;
    return fputc('\n', stdout) == EOF ? EOF : 1;
}

int fgetc(FILE *f) {
    return __stream_getc(f);
}

int getc(FILE *f) {
    return __stream_getc(f);
}

int getchar(void) {
    return __stream_getc(stdin);
}

int ungetc(int c, FILE *f) {
    if (c == EOF)
        return EOF;
    f->unget = (unsigned char)c;
    f->eof = 0;
    return (unsigned char)c;
}

char *fgets(char *s, int n, FILE *f) {
    int i = 0;
    if (n <= 0)
        return NULL;
    while (i < n - 1) {
        int c = __stream_getc(f);
        if (c == EOF)
            break;
        s[i++] = (char)c;
        if (c == '\n')
            break;
    }
    if (i == 0)
        return NULL;
    s[i] = 0;
    return s;
}

void perror(const char *s) {
    if (s && *s)
        fprintf(stderr, "%s: %s\n", s, strerror(errno));
    else
        fprintf(stderr, "%s\n", strerror(errno));
}

char *strerror(int error) {
    switch (error) {
    case 0:
        return "Success";
    case EPERM:
        return "Operation not permitted";
    case ENOENT:
        return "No such file or directory";
    case EIO:
        return "Input/output error";
    case EBADF:
        return "Bad file descriptor";
    case ENOMEM:
        return "Cannot allocate memory";
    case EACCES:
        return "Permission denied";
    case EEXIST:
        return "File exists";
    case ENOTDIR:
        return "Not a directory";
    case EISDIR:
        return "Is a directory";
    case EINVAL:
        return "Invalid argument";
    case ENOSPC:
        return "No space left on device";
    case EDOM:
        return "Numerical argument out of domain";
    case ERANGE:
        return "Numerical result out of range";
    case ENOSYS:
        return "Function not implemented";
    }
    return "Unknown error";
}

/* ============================================================
 *  5. Formatted output
 * ============================================================ */

/* Where formatted output goes: a stream, or a buffer of known size. */
struct __sink {
    FILE *file;
    char *buffer;
    size_t capacity;
    size_t length; /* everything written, kept or not */
};

static void __emit(struct __sink *sink, const char *s, size_t n) {
    if (sink->file) {
        __stream_write(sink->file, s, n);
    } else {
        size_t i;
        for (i = 0; i < n; i++)
            if (sink->length + i + 1 < sink->capacity)
                sink->buffer[sink->length + i] = s[i];
    }
    sink->length += n;
}

#define F_LEFT 1
#define F_PLUS 2
#define F_SPACE 4
#define F_ALT 8
#define F_ZERO 16

static void __pad(struct __sink *sink, char c, int width, int length, int flags) {
    char block[64];
    int n;
    if ((flags & (F_LEFT | F_ZERO)) || length >= width)
        return;
    n = width - length;
    memset(block, c, n > 64 ? 64 : n);
    while (n >= 64) {
        __emit(sink, block, 64);
        n -= 64;
    }
    __emit(sink, block, n);
}

/* The decimal digits of x, written backwards ending at s. */
static char *__fmt_u(unsigned long x, char *s) {
    for (; x; x /= 10)
        *--s = '0' + (char)(x % 10);
    return s;
}

static unsigned long __double_bits(double x) {
    unsigned long bits;
    memcpy(&bits, &x, 8);
    return bits;
}

static double __bits_double(unsigned long bits) {
    double x;
    memcpy(&x, &bits, 8);
    return x;
}

/* %e, %f, %g of a double: musl's fmt_fp. The value is converted exactly
 * to base 10^9 digits, then rounded half to even on the exact value. */
static int __fmt_fp(struct __sink *sink, double y, int w, int p, int fl, int t) {
    uint32_t big[(53 + 28) / 29 + 1 + (1024 + 53 + 28 + 8) / 9];
    uint32_t *a, *d, *r, *z;
    int e2 = 0, e, i, j, l;
    char buf[9 + 53 / 4];
    const char *prefix = "-0X+0X 0X-0x+0x 0x";
    int pl;
    char ebuf0[3 * sizeof(int)], *ebuf = &ebuf0[3 * sizeof(int)], *estr = 0;
    unsigned long bits = __double_bits(y);

    pl = 1;
    if (bits >> 63) {
        y = -y;
    } else if (fl & F_PLUS) {
        prefix += 3;
    } else if (fl & F_SPACE) {
        prefix += 6;
    } else {
        prefix++;
        pl = 0;
    }

    if (((bits >> 52) & 0x7ff) == 0x7ff) {
        const char *s = (t & 32) ? ((bits << 12) ? "nan" : "inf") : ((bits << 12) ? "NAN" : "INF");
        __pad(sink, ' ', w, 3 + pl, fl & ~F_ZERO);
        __emit(sink, prefix, pl);
        __emit(sink, s, 3);
        __pad(sink, ' ', w, 3 + pl, fl ^ F_LEFT);
        return w > 3 + pl ? w : 3 + pl;
    }

    y = frexp(y, &e2) * 2;
    if (y != 0)
        e2--;

    if (p < 0)
        p = 6;

    if (y != 0) {
        y *= 268435456.0; /* 0x1p28 */
        e2 -= 28;
    }

    if (e2 < 0)
        a = r = z = big;
    else
        a = r = z = big + sizeof(big) / sizeof(*big) - 53 - 1;

    do {
        *z = (uint32_t)y;
        y = 1000000000 * (y - *z++);
    } while (y != 0);

    while (e2 > 0) {
        uint32_t carry = 0;
        int sh = e2 < 29 ? e2 : 29;
        for (d = z - 1; d >= a; d--) {
            uint64_t x = ((uint64_t)*d << sh) + carry;
            *d = (uint32_t)(x % 1000000000);
            carry = (uint32_t)(x / 1000000000);
        }
        if (carry)
            *--a = carry;
        while (z > a && !z[-1])
            z--;
        e2 -= sh;
    }
    while (e2 < 0) {
        uint32_t carry = 0, *b;
        int sh = -e2 < 9 ? -e2 : 9, need = 1 + (p + 53 / 3 + 8) / 9;
        for (d = a; d < z; d++) {
            uint32_t rm = *d & ((1 << sh) - 1);
            *d = (*d >> sh) + carry;
            carry = (1000000000 >> sh) * rm;
        }
        if (!*a)
            a++;
        if (carry)
            *z++ = carry;
        /* Avoid computation past the requested precision. */
        b = (t | 32) == 'f' ? r : a;
        if (z - b > need)
            z = b + need;
        e2 += sh;
    }

    if (a < z)
        for (i = 10, e = 9 * (int)(r - a); *a >= (uint32_t)i; i *= 10, e++)
            ;
    else
        e = 0;

    /* Rounding: j is the precision after the radix point. */
    j = p - ((t | 32) != 'f') * e - ((t | 32) == 'g' && p);
    if (j < 9 * (z - r - 1)) {
        uint32_t x;
        d = r + 1 + ((j + 9 * 1024) / 9 - 1024);
        j += 9 * 1024;
        j %= 9;
        for (i = 10, j++; j < 9; i *= 10, j++)
            ;
        x = *d % i;
        /* Any significant digit past j? */
        if (x || d + 1 != z) {
            double round = 2 / 2.22044604925031308085e-16;
            double small;
            if ((*d / i & 1) || (i == 1000000000 && d > a && (d[-1] & 1)))
                round += 2;
            if (x < (uint32_t)(i / 2))
                small = 0.5;
            else if (x == (uint32_t)(i / 2) && d + 1 == z)
                small = 1.0;
            else
                small = 1.5;
            if (pl && *prefix == '-') {
                round *= -1;
                small *= -1;
            }
            *d -= x;
            /* Decide whether to round up by probing round + small. */
            if (round + small != round) {
                *d = *d + i;
                while (*d > 999999999) {
                    *d-- = 0;
                    if (d < a)
                        *--a = 0;
                    (*d)++;
                }
                for (i = 10, e = 9 * (int)(r - a); *a >= (uint32_t)i; i *= 10, e++)
                    ;
            }
        }
        if (z > d + 1)
            z = d + 1;
    }
    for (; z > a && !z[-1]; z--)
        ;

    if ((t | 32) == 'g') {
        if (!p)
            p++;
        if (p > e && e >= -4) {
            t--;
            p -= e + 1;
        } else {
            t -= 2;
            p--;
        }
        if (!(fl & F_ALT)) {
            /* Count the trailing zeros of the last digit group. */
            if (z > a && z[-1])
                for (i = 10, j = 0; z[-1] % i == 0; i *= 10, j++)
                    ;
            else
                j = 9;
            if ((t | 32) == 'f') {
                int keep = 9 * (int)(z - r - 1) - j;
                p = p < (keep > 0 ? keep : 0) ? p : (keep > 0 ? keep : 0);
            } else {
                int keep = 9 * (int)(z - r - 1) + e - j;
                p = p < (keep > 0 ? keep : 0) ? p : (keep > 0 ? keep : 0);
            }
        }
    }
    l = 1 + p + (p || (fl & F_ALT));
    if ((t | 32) == 'f') {
        if (e > 0)
            l += e;
    } else {
        estr = __fmt_u(e < 0 ? -e : e, ebuf);
        while (ebuf - estr < 2)
            *--estr = '0';
        *--estr = (e < 0 ? '-' : '+');
        *--estr = (char)t;
        l += (int)(ebuf - estr);
    }

    __pad(sink, ' ', w, pl + l, fl);
    __emit(sink, prefix, pl);
    __pad(sink, '0', w, pl + l, fl ^ F_ZERO);

    if ((t | 32) == 'f') {
        if (a > r)
            a = r;
        for (d = a; d <= r; d++) {
            char *s = __fmt_u(*d, buf + 9);
            if (d != a)
                while (s > buf)
                    *--s = '0';
            else if (s == buf + 9)
                *--s = '0';
            __emit(sink, s, buf + 9 - s);
        }
        if (p || (fl & F_ALT))
            __emit(sink, ".", 1);
        for (; d < z && p > 0; d++, p -= 9) {
            char *s = __fmt_u(*d, buf + 9);
            while (s > buf)
                *--s = '0';
            __emit(sink, s, 9 < p ? 9 : p);
        }
        __pad(sink, '0', p + 9, 9, 0);
    } else {
        if (z <= a)
            z = a + 1;
        for (d = a; d < z && p >= 0; d++) {
            char *s = __fmt_u(*d, buf + 9);
            if (s == buf + 9)
                *--s = '0';
            if (d != a)
                while (s > buf)
                    *--s = '0';
            else {
                __emit(sink, s++, 1);
                if (p > 0 || (fl & F_ALT))
                    __emit(sink, ".", 1);
            }
            __emit(sink, s, (buf + 9 - s) < p ? (buf + 9 - s) : p);
            p -= (int)(buf + 9 - s);
        }
        __pad(sink, '0', p + 18, 18, 0);
        __emit(sink, estr, ebuf - estr);
    }

    __pad(sink, ' ', w, pl + l, fl ^ F_LEFT);

    return w > pl + l ? w : pl + l;
}

/* The engine of the printf family. */
static int __format(struct __sink *sink, const char *format, va_list ap) {
    const char *f = format;
    while (*f) {
        const char *start = f;
        int flags = 0, width = 0, precision = -1, size = 0;
        char c;
        char digits[72];
        char *s;
        const char *prefix = "";
        unsigned long value;
        int negative;
        int length;
        int zeros;
        int prefix_length;

        while (*f && *f != '%')
            f++;
        if (f > start)
            __emit(sink, start, f - start);
        if (!*f)
            break;
        f++;

        for (;; f++) {
            if (*f == '-')
                flags |= F_LEFT;
            else if (*f == '+')
                flags |= F_PLUS;
            else if (*f == ' ')
                flags |= F_SPACE;
            else if (*f == '#')
                flags |= F_ALT;
            else if (*f == '0')
                flags |= F_ZERO;
            else
                break;
        }
        if (*f == '*') {
            width = va_arg(ap, int);
            if (width < 0) {
                flags |= F_LEFT;
                width = -width;
            }
            f++;
        } else {
            while (*f >= '0' && *f <= '9')
                width = width * 10 + (*f++ - '0');
        }
        if (*f == '.') {
            f++;
            precision = 0;
            if (*f == '*') {
                precision = va_arg(ap, int);
                f++;
            } else {
                while (*f >= '0' && *f <= '9')
                    precision = precision * 10 + (*f++ - '0');
            }
        }
        /* Length: 0 int, -1 short, -2 char, 1 long (and ll, z, j, t). */
        for (;; f++) {
            if (*f == 'l' || *f == 'z' || *f == 'j' || *f == 't' || *f == 'q')
                size = 1;
            else if (*f == 'h')
                size = size == -1 ? -2 : -1;
            else if (*f == 'L')
                size = 1;
            else
                break;
        }
        c = *f++;
        if (flags & F_LEFT)
            flags &= ~F_ZERO;

        switch (c) {
        case 'd':
        case 'i': {
            long n;
            if (size == 1)
                n = va_arg(ap, long);
            else
                n = va_arg(ap, int);
            if (size == -1)
                n = (short)n;
            else if (size == -2)
                n = (signed char)n;
            negative = n < 0;
            value = negative ? -(unsigned long)n : (unsigned long)n;
            prefix = negative ? "-" : (flags & F_PLUS) ? "+" : (flags & F_SPACE) ? " " : "";
            s = __fmt_u(value, digits + sizeof(digits));
            goto integer;
        }
        case 'u':
        case 'x':
        case 'X':
        case 'o':
        case 'p': {
            int base = c == 'u' ? 10 : c == 'o' ? 8 : 16;
            const char *alphabet = c == 'X' ? "0123456789ABCDEF" : "0123456789abcdef";
            if (c == 'p') {
                value = (unsigned long)va_arg(ap, void *);
                if (!value) {
                    s = "(nil)";
                    length = 5;
                    __pad(sink, ' ', width, length, flags & ~F_ZERO);
                    __emit(sink, s, length);
                    __pad(sink, ' ', width, length, flags ^ F_LEFT);
                    continue;
                }
                prefix = "0x";
            } else if (size == 1) {
                value = va_arg(ap, unsigned long);
            } else {
                value = va_arg(ap, unsigned int);
            }
            if (size == -1)
                value = (unsigned short)value;
            else if (size == -2)
                value = (unsigned char)value;
            s = digits + sizeof(digits);
            {
                unsigned long v = value;
                for (; v; v /= base)
                    *--s = alphabet[v % base];
            }
            if ((flags & F_ALT) && value) {
                if (c == 'x')
                    prefix = "0x";
                else if (c == 'X')
                    prefix = "0X";
            }
            if ((flags & F_ALT) && c == 'o') {
                /* The precision grows so that the first digit is 0. */
                int count = (int)(digits + sizeof(digits) - s);
                if (precision <= count)
                    precision = count + 1;
            }
            goto integer;
        }
        integer:
            length = (int)(digits + sizeof(digits) - s);
            if (precision >= 0)
                flags &= ~F_ZERO;
            /* Zero has one digit, none at precision 0 (C11 7.21.6.1). */
            if (!length && precision < 0)
                precision = 1;
            zeros = precision > length ? precision - length : 0;
            prefix_length = (int)strlen(prefix);
            __pad(sink, ' ', width, prefix_length + zeros + length, flags);
            __emit(sink, prefix, prefix_length);
            __pad(sink, '0', width, prefix_length + zeros + length, flags ^ F_ZERO);
            while (zeros-- > 0)
                __emit(sink, "0", 1);
            __emit(sink, s, length);
            __pad(sink, ' ', width, prefix_length + (precision > length ? precision : length), flags ^ F_LEFT);
            break;
        case 'c': {
            char byte = (char)va_arg(ap, int);
            __pad(sink, ' ', width, 1, flags & ~F_ZERO);
            __emit(sink, &byte, 1);
            __pad(sink, ' ', width, 1, flags ^ F_LEFT);
            break;
        }
        case 's': {
            const char *text = va_arg(ap, const char *);
            if (!text)
                text = "(null)";
            length = (int)(precision >= 0 ? strnlen(text, precision) : strlen(text));
            __pad(sink, ' ', width, length, flags & ~F_ZERO);
            __emit(sink, text, length);
            __pad(sink, ' ', width, length, flags ^ F_LEFT);
            break;
        }
        case 'e':
        case 'E':
        case 'f':
        case 'F':
        case 'g':
        case 'G':
            __fmt_fp(sink, va_arg(ap, double), width, precision, flags, c);
            break;
        case 'n':
            *va_arg(ap, int *) = (int)sink->length;
            break;
        case '%':
            __emit(sink, "%", 1);
            break;
        case 0:
            /* A lone % at the end of the format. */
            f--;
            break;
        default:
            /* An unknown conversion is written as it was. */
            __emit(sink, "%", 1);
            __emit(sink, &c, 1);
            break;
        }
    }
    return (int)sink->length;
}

int vfprintf(FILE *f, const char *format, va_list ap) {
    struct __sink sink = {f, 0, 0, 0};
    return __format(&sink, format, ap);
}

int vprintf(const char *format, va_list ap) {
    return vfprintf(stdout, format, ap);
}

int vsnprintf(char *s, size_t n, const char *format, va_list ap) {
    struct __sink sink = {0, s, n, 0};
    int length = __format(&sink, format, ap);
    if (n)
        s[sink.length < n ? sink.length : n - 1] = 0;
    return length;
}

int vsprintf(char *s, const char *format, va_list ap) {
    return vsnprintf(s, (size_t)-1 / 2, format, ap);
}

int printf(const char *format, ...) {
    va_list ap;
    int n;
    va_start(ap, format);
    n = vfprintf(stdout, format, ap);
    va_end(ap);
    return n;
}

int fprintf(FILE *f, const char *format, ...) {
    va_list ap;
    int n;
    va_start(ap, format);
    n = vfprintf(f, format, ap);
    va_end(ap);
    return n;
}

int sprintf(char *s, const char *format, ...) {
    va_list ap;
    int n;
    va_start(ap, format);
    n = vsprintf(s, format, ap);
    va_end(ap);
    return n;
}

int snprintf(char *s, size_t size, const char *format, ...) {
    va_list ap;
    int n;
    va_start(ap, format);
    n = vsnprintf(s, size, format, ap);
    va_end(ap);
    return n;
}

/* ============================================================
 *  6. Formatted input
 * ============================================================ */

/* Where scanf reads: a stream or a string. */
struct __source {
    FILE *file;
    const char *text;
    int consumed;
};

static int __next(struct __source *in) {
    int c;
    if (in->file)
        c = __stream_getc(in->file);
    else
        c = in->text[in->consumed] ? (unsigned char)in->text[in->consumed] : EOF;
    if (c != EOF)
        in->consumed++;
    return c;
}

static void __back(struct __source *in, int c) {
    if (c == EOF)
        return;
    in->consumed--;
    if (in->file)
        ungetc(c, in->file);
}

static int __scan(struct __source *in, const char *format, va_list ap) {
    const char *f = format;
    int assigned = 0;
    int c;
    while (*f) {
        int suppress = 0, width = 0, size = 0;
        char conversion;
        if (isspace((unsigned char)*f)) {
            while (isspace((unsigned char)*f))
                f++;
            do
                c = __next(in);
            while (isspace(c));
            __back(in, c);
            continue;
        }
        if (*f != '%' || f[1] == '%') {
            if (*f == '%')
                f++;
            c = __next(in);
            if (c != (unsigned char)*f) {
                __back(in, c);
                return assigned ? assigned : (c == EOF ? EOF : 0);
            }
            f++;
            continue;
        }
        f++;
        if (*f == '*') {
            suppress = 1;
            f++;
        }
        while (*f >= '0' && *f <= '9')
            width = width * 10 + (*f++ - '0');
        for (;; f++) {
            if (*f == 'l' || *f == 'z' || *f == 'j' || *f == 't' || *f == 'L')
                size = size == 1 ? 2 : 1;
            else if (*f == 'h')
                size = size == -1 ? -2 : -1;
            else
                break;
        }
        conversion = *f++;
        if (!width)
            width = INT_MAX;
        if (conversion != 'c' && conversion != '[' && conversion != 'n') {
            do
                c = __next(in);
            while (isspace(c));
            __back(in, c);
        }
        switch (conversion) {
        case 'n':
            if (!suppress)
                *va_arg(ap, int *) = in->consumed;
            continue;
        case 'c': {
            char *out = suppress ? 0 : va_arg(ap, char *);
            int count = width == INT_MAX ? 1 : width;
            int i;
            for (i = 0; i < count; i++) {
                c = __next(in);
                if (c == EOF)
                    return assigned ? assigned : EOF;
                if (out)
                    out[i] = (char)c;
            }
            if (!suppress)
                assigned++;
            continue;
        }
        case 's': {
            char *out = suppress ? 0 : va_arg(ap, char *);
            int i = 0;
            c = __next(in);
            if (c == EOF)
                return assigned ? assigned : EOF;
            while (c != EOF && !isspace(c) && i < width) {
                if (out)
                    out[i] = (char)c;
                i++;
                c = __next(in);
            }
            __back(in, c);
            if (out)
                out[i] = 0;
            if (!suppress)
                assigned++;
            continue;
        }
        case '[': {
            char set[256];
            int invert = 0, i = 0;
            char *out;
            memset(set, 0, sizeof(set));
            if (*f == '^') {
                invert = 1;
                f++;
            }
            if (*f == ']')
                set[(unsigned char)*f++] = 1;
            while (*f && *f != ']') {
                if (f[1] == '-' && f[2] && f[2] != ']') {
                    int from = (unsigned char)f[0], to = (unsigned char)f[2];
                    for (; from <= to; from++)
                        set[from] = 1;
                    f += 3;
                } else {
                    set[(unsigned char)*f++] = 1;
                }
            }
            if (*f == ']')
                f++;
            out = suppress ? 0 : va_arg(ap, char *);
            c = __next(in);
            while (c != EOF && (set[c] != invert) && i < width) {
                if (out)
                    out[i] = (char)c;
                i++;
                c = __next(in);
            }
            __back(in, c);
            if (!i)
                return assigned ? assigned : (c == EOF ? EOF : 0);
            if (out)
                out[i] = 0;
            if (!suppress)
                assigned++;
            continue;
        }
        case 'd':
        case 'i':
        case 'u':
        case 'x':
        case 'X':
        case 'o':
        case 'p': {
            char text[80];
            int i = 0;
            int base = conversion == 'd' || conversion == 'u' ? 10 : conversion == 'o' ? 8 : conversion == 'i' ? 0 : 16;
            unsigned long value;
            char *end;
            c = __next(in);
            while (c != EOF && i < width && i < 79 &&
                   (isxdigit(c) || c == '-' || c == '+' || ((c == 'x' || c == 'X') && i > 0))) {
                if (base == 10 && !isdigit(c) && !((c == '-' || c == '+') && i == 0))
                    break;
                if (base == 8 && !((c >= '0' && c <= '7') || ((c == '-' || c == '+') && i == 0)))
                    break;
                if ((c == '-' || c == '+') && i > 0)
                    break;
                text[i++] = (char)c;
                c = __next(in);
            }
            __back(in, c);
            text[i] = 0;
            if (!i)
                return assigned ? assigned : (c == EOF ? EOF : 0);
            value = strtoul(text, &end, base);
            if (text[0] == '-')
                value = (unsigned long)strtol(text, &end, base);
            if (end == text)
                return assigned;
            if (!suppress) {
                if (conversion == 'p')
                    *va_arg(ap, void **) = (void *)value;
                else if (size >= 1)
                    *va_arg(ap, long *) = (long)value;
                else if (size == -1)
                    *va_arg(ap, short *) = (short)value;
                else if (size == -2)
                    *va_arg(ap, char *) = (char)value;
                else
                    *va_arg(ap, int *) = (int)value;
                assigned++;
            }
            continue;
        }
        case 'e':
        case 'E':
        case 'f':
        case 'F':
        case 'g':
        case 'G':
        case 'a': {
            char text[400];
            int i = 0;
            double value;
            char *end;
            c = __next(in);
            while (c != EOF && i < width && i < 399 &&
                   (isalnum(c) || c == '.' || c == '-' || c == '+')) {
                if ((c == '-' || c == '+') && i > 0 && text[i - 1] != 'e' && text[i - 1] != 'E')
                    break;
                text[i++] = (char)c;
                c = __next(in);
            }
            __back(in, c);
            text[i] = 0;
            if (!i)
                return assigned ? assigned : (c == EOF ? EOF : 0);
            value = strtod(text, &end);
            if (end == text)
                return assigned;
            if (!suppress) {
                if (size >= 1)
                    *va_arg(ap, double *) = value;
                else
                    *va_arg(ap, float *) = (float)value;
                assigned++;
            }
            continue;
        }
        default:
            return assigned;
        }
    }
    return assigned;
}

int vfscanf(FILE *f, const char *format, va_list ap) {
    struct __source in = {f, 0, 0};
    return __scan(&in, format, ap);
}

int vsscanf(const char *s, const char *format, va_list ap) {
    struct __source in = {0, s, 0};
    return __scan(&in, format, ap);
}

int scanf(const char *format, ...) {
    va_list ap;
    int n;
    va_start(ap, format);
    n = vfscanf(stdin, format, ap);
    va_end(ap);
    return n;
}

int fscanf(FILE *f, const char *format, ...) {
    va_list ap;
    int n;
    va_start(ap, format);
    n = vfscanf(f, format, ap);
    va_end(ap);
    return n;
}

int sscanf(const char *s, const char *format, ...) {
    va_list ap;
    int n;
    va_start(ap, format);
    n = vsscanf(s, format, ap);
    va_end(ap);
    return n;
}

/* ============================================================
 *  7. Characters and strings
 * ============================================================ */

int isdigit(int c) { return c >= '0' && c <= '9'; }
int islower(int c) { return c >= 'a' && c <= 'z'; }
int isupper(int c) { return c >= 'A' && c <= 'Z'; }
int isalpha(int c) { return islower(c) || isupper(c); }
int isalnum(int c) { return isalpha(c) || isdigit(c); }
int isxdigit(int c) { return isdigit(c) || (c >= 'a' && c <= 'f') || (c >= 'A' && c <= 'F'); }
int isspace(int c) { return c == ' ' || (c >= '\t' && c <= '\r'); }
int isblank(int c) { return c == ' ' || c == '\t'; }
int iscntrl(int c) { return (c >= 0 && c < 32) || c == 127; }
int isprint(int c) { return c >= 32 && c < 127; }
int isgraph(int c) { return c > 32 && c < 127; }
int ispunct(int c) { return isgraph(c) && !isalnum(c); }
int tolower(int c) { return isupper(c) ? c + 32 : c; }
int toupper(int c) { return islower(c) ? c - 32 : c; }

void *memcpy(void *dest, const void *src, size_t n) {
    char *d = dest;
    const char *s = src;
    while (n >= 8) {
        *(unsigned long *)d = *(const unsigned long *)s;
        d += 8;
        s += 8;
        n -= 8;
    }
    while (n--)
        *d++ = *s++;
    return dest;
}

void *memmove(void *dest, const void *src, size_t n) {
    char *d = dest;
    const char *s = src;
    if (d == s || !n)
        return dest;
    if (d < s || d >= s + n)
        return memcpy(dest, src, n);
    d += n;
    s += n;
    while (n--)
        *--d = *--s;
    return dest;
}

void *memset(void *dest, int c, size_t n) {
    unsigned char *d = dest;
    unsigned long word = (unsigned char)c * 0x0101010101010101UL;
    while (n >= 8) {
        *(unsigned long *)d = word;
        d += 8;
        n -= 8;
    }
    while (n--)
        *d++ = (unsigned char)c;
    return dest;
}

int memcmp(const void *a, const void *b, size_t n) {
    const unsigned char *x = a, *y = b;
    for (; n; n--, x++, y++)
        if (*x != *y)
            return *x - *y;
    return 0;
}

void *memchr(const void *s, int c, size_t n) {
    const unsigned char *p = s;
    for (; n; n--, p++)
        if (*p == (unsigned char)c)
            return (void *)p;
    return NULL;
}

size_t strlen(const char *s) {
    const char *p = s;
    while (*p)
        p++;
    return p - s;
}

size_t strnlen(const char *s, size_t n) {
    size_t i = 0;
    while (i < n && s[i])
        i++;
    return i;
}

char *strcpy(char *dest, const char *src) {
    char *d = dest;
    while ((*d++ = *src++))
        ;
    return dest;
}

char *strncpy(char *dest, const char *src, size_t n) {
    size_t i = 0;
    for (; i < n && src[i]; i++)
        dest[i] = src[i];
    for (; i < n; i++)
        dest[i] = 0;
    return dest;
}

char *strcat(char *dest, const char *src) {
    strcpy(dest + strlen(dest), src);
    return dest;
}

char *strncat(char *dest, const char *src, size_t n) {
    char *d = dest + strlen(dest);
    while (n-- && *src)
        *d++ = *src++;
    *d = 0;
    return dest;
}

int strcmp(const char *a, const char *b) {
    while (*a && *a == *b) {
        a++;
        b++;
    }
    return (unsigned char)*a - (unsigned char)*b;
}

int strncmp(const char *a, const char *b, size_t n) {
    for (; n; n--, a++, b++) {
        if (*a != *b)
            return (unsigned char)*a - (unsigned char)*b;
        if (!*a)
            return 0;
    }
    return 0;
}

int strcoll(const char *a, const char *b) {
    return strcmp(a, b);
}

char *strchr(const char *s, int c) {
    for (;; s++) {
        if (*s == (char)c)
            return (char *)s;
        if (!*s)
            return NULL;
    }
}

char *strrchr(const char *s, int c) {
    const char *found = NULL;
    for (;; s++) {
        if (*s == (char)c)
            found = s;
        if (!*s)
            return (char *)found;
    }
}

char *strstr(const char *haystack, const char *needle) {
    size_t n = strlen(needle);
    if (!n)
        return (char *)haystack;
    for (; *haystack; haystack++)
        if (*haystack == *needle && !strncmp(haystack, needle, n))
            return (char *)haystack;
    return NULL;
}

size_t strspn(const char *s, const char *accept) {
    size_t i = 0;
    while (s[i] && strchr(accept, s[i]))
        i++;
    return i;
}

size_t strcspn(const char *s, const char *reject) {
    size_t i = 0;
    while (s[i] && !strchr(reject, s[i]))
        i++;
    return i;
}

char *strpbrk(const char *s, const char *accept) {
    s += strcspn(s, accept);
    return *s ? (char *)s : NULL;
}

char *strtok(char *s, const char *delim) {
    static char *saved;
    char *start;
    if (!s)
        s = saved;
    if (!s)
        return NULL;
    s += strspn(s, delim);
    if (!*s) {
        saved = NULL;
        return NULL;
    }
    start = s;
    s += strcspn(s, delim);
    if (*s)
        *s++ = 0;
    else
        s = NULL;
    saved = s;
    return start;
}

char *strdup(const char *s) {
    size_t n = strlen(s) + 1;
    char *copy = malloc(n);
    if (copy)
        memcpy(copy, s, n);
    return copy;
}

char *strndup(const char *s, size_t n) {
    size_t length = strnlen(s, n);
    char *copy = malloc(length + 1);
    if (copy) {
        memcpy(copy, s, length);
        copy[length] = 0;
    }
    return copy;
}

/* ============================================================
 *  8. Numbers
 * ============================================================ */

int abs(int n) { return n < 0 ? -n : n; }
long labs(long n) { return n < 0 ? -n : n; }
long long llabs(long long n) { return n < 0 ? -n : n; }

div_t div(int a, int b) {
    div_t r;
    r.quot = a / b;
    r.rem = a % b;
    return r;
}

ldiv_t ldiv(long a, long b) {
    ldiv_t r;
    r.quot = a / b;
    r.rem = a % b;
    return r;
}

lldiv_t lldiv(long long a, long long b) {
    lldiv_t r;
    r.quot = a / b;
    r.rem = a % b;
    return r;
}

/* The digits of an integer in base 2 to 36: the common part of strtol
 * and strtoul. *overflow is set when the magnitude passes `limit`. */
static unsigned long __strtox(const char *s, char **end, int base, int *negative, unsigned long limit, int *overflow) {
    const char *p = s;
    unsigned long value = 0;
    int any = 0;
    *negative = 0;
    *overflow = 0;
    while (isspace((unsigned char)*p))
        p++;
    if (*p == '-' || *p == '+')
        *negative = *p++ == '-';
    if ((base == 0 || base == 16) && p[0] == '0' && (p[1] == 'x' || p[1] == 'X') && isxdigit((unsigned char)p[2])) {
        p += 2;
        base = 16;
    } else if (base == 0) {
        base = p[0] == '0' ? 8 : 10;
    }
    for (;; p++) {
        int digit;
        if (isdigit((unsigned char)*p))
            digit = *p - '0';
        else if (isalpha((unsigned char)*p))
            digit = tolower((unsigned char)*p) - 'a' + 10;
        else
            break;
        if (digit >= base)
            break;
        any = 1;
        if (value > (limit - digit) / base)
            *overflow = 1;
        else
            value = value * base + digit;
    }
    if (end)
        *end = (char *)(any ? p : s);
    return value;
}

long strtol(const char *s, char **end, int base) {
    int negative, overflow;
    unsigned long value = __strtox(s, end, base, &negative, (unsigned long)LONG_MAX + 1, &overflow);
    if (overflow || (!negative && value > LONG_MAX)) {
        errno = ERANGE;
        return negative ? LONG_MIN : LONG_MAX;
    }
    return negative ? -(long)value : (long)value;
}

unsigned long strtoul(const char *s, char **end, int base) {
    int negative, overflow;
    unsigned long value = __strtox(s, end, base, &negative, ULONG_MAX, &overflow);
    if (overflow) {
        errno = ERANGE;
        return ULONG_MAX;
    }
    return negative ? -value : value;
}

long long strtoll(const char *s, char **end, int base) {
    return strtol(s, end, base);
}

unsigned long long strtoull(const char *s, char **end, int base) {
    return strtoul(s, end, base);
}

int atoi(const char *s) {
    return (int)strtol(s, NULL, 10);
}

long atol(const char *s) {
    return strtol(s, NULL, 10);
}

long long atoll(const char *s) {
    return strtol(s, NULL, 10);
}

/* --- strtod: correctly rounded, by big integers ---
 *
 * The decimal value D * 10^E is compared exactly with binary values: a
 * big integer is a little-endian array of 32-bit words. */

#define __BIG 140 /* words: 4480 bits, past 10^1100 */

struct __bignum {
    int length;
    uint32_t word[__BIG];
};

static void __big_mul_add(struct __bignum *n, uint32_t factor, uint32_t add) {
    uint64_t carry = add;
    int i;
    for (i = 0; i < n->length; i++) {
        uint64_t x = (uint64_t)n->word[i] * factor + carry;
        n->word[i] = (uint32_t)x;
        carry = x >> 32;
    }
    if (carry && n->length < __BIG)
        n->word[n->length++] = (uint32_t)carry;
}

static int __big_bits(const struct __bignum *n) {
    int bits;
    uint32_t top;
    if (!n->length)
        return 0;
    top = n->word[n->length - 1];
    for (bits = 0; top; bits++)
        top >>= 1;
    return (n->length - 1) * 32 + bits;
}

static void __big_shift_left(struct __bignum *n, int shift) {
    int words = shift / 32, bits = shift % 32, i;
    if (!n->length)
        return;
    if (n->length + words + 1 > __BIG)
        return;
    n->word[n->length + words] = 0;
    for (i = n->length - 1; i >= 0; i--) {
        uint64_t x = (uint64_t)n->word[i] << bits;
        n->word[i + words + 1] |= (uint32_t)(x >> 32);
        n->word[i + words] = (uint32_t)x;
    }
    for (i = 0; i < words; i++)
        n->word[i] = 0;
    n->length += words + 1;
    while (n->length && !n->word[n->length - 1])
        n->length--;
}

static int __big_compare(const struct __bignum *a, const struct __bignum *b) {
    int i;
    if (a->length != b->length)
        return a->length < b->length ? -1 : 1;
    for (i = a->length - 1; i >= 0; i--)
        if (a->word[i] != b->word[i])
            return a->word[i] < b->word[i] ? -1 : 1;
    return 0;
}

static void __big_subtract(struct __bignum *a, const struct __bignum *b) {
    int64_t borrow = 0;
    int i;
    for (i = 0; i < a->length; i++) {
        int64_t x = (int64_t)a->word[i] - (i < b->length ? b->word[i] : 0) - borrow;
        borrow = x < 0;
        a->word[i] = (uint32_t)(x + (borrow ? (int64_t)1 << 32 : 0));
    }
    while (a->length && !a->word[a->length - 1])
        a->length--;
}

/* The double nearest to mantissa * 2^exponent, where `sticky` says
 * whether bits below the mantissa were lost. */
static double __make_double(unsigned long mantissa, int exponent, int sticky) {
    int shift = 0;
    unsigned long bits;
    if (!mantissa)
        return 0.0;
    while (!(mantissa >> 63)) {
        mantissa <<= 1;
        exponent--;
    }
    /* value = mantissa * 2^exponent with the top bit set: keep 53 bits,
     * fewer for a subnormal. */
    {
        int top = exponent + 63; /* exponent of the leading bit */
        if (top > 1023)
            return __bits_double(0x7ff0000000000000UL);
        shift = 11;
        if (top < -1022)
            shift += -1022 - top;
        if (shift > 64) {
            return 0.0;
        }
    }
    {
        unsigned long kept = shift == 64 ? 0 : mantissa >> shift;
        unsigned long rest = shift == 64 ? mantissa : mantissa & ((1UL << shift) - 1);
        unsigned long half = 1UL << (shift - 1);
        if (rest > half || (rest == half && (sticky || (kept & 1))))
            kept++;
        if (shift > 11) {
            /* Subnormal (or rounded up to the smallest normal). */
            bits = kept;
        } else {
            int biased = exponent + 63 + 1023;
            if (kept >> 53) {
                kept >>= 1;
                biased++;
            }
            if (biased >= 2047)
                return __bits_double(0x7ff0000000000000UL);
            bits = ((unsigned long)biased << 52) | (kept & ((1UL << 52) - 1));
        }
    }
    return __bits_double(bits);
}

double strtod(const char *s, char **end) {
    const char *p = s;
    int negative = 0;
    struct __bignum digits;
    int exponent = 0;
    int any = 0;
    int significant = 0;
    int dropped = 0; /* nonzero digits past the 780 kept */
    double value;

    while (isspace((unsigned char)*p))
        p++;
    if (*p == '-' || *p == '+')
        negative = *p++ == '-';

    if (tolower(p[0]) == 'i' && tolower(p[1]) == 'n' && tolower(p[2]) == 'f') {
        p += 3;
        if (tolower(p[0]) == 'i' && tolower(p[1]) == 'n' && tolower(p[2]) == 'i' && tolower(p[3]) == 't' &&
            tolower(p[4]) == 'y')
            p += 5;
        if (end)
            *end = (char *)p;
        value = __bits_double(0x7ff0000000000000UL);
        return negative ? -value : value;
    }
    if (tolower(p[0]) == 'n' && tolower(p[1]) == 'a' && tolower(p[2]) == 'n') {
        if (end)
            *end = (char *)p + 3;
        return __bits_double(0x7ff8000000000000UL);
    }

    digits.length = 0;
    for (; isdigit((unsigned char)*p); p++) {
        any = 1;
        if (*p == '0' && !significant)
            continue;
        if (significant < 780) {
            if (!digits.length)
                digits.length = 1, digits.word[0] = 0;
            __big_mul_add(&digits, 10, *p - '0');
            significant++;
        } else {
            exponent++;
            dropped |= *p != '0';
        }
    }
    if (*p == '.') {
        p++;
        for (; isdigit((unsigned char)*p); p++) {
            any = 1;
            if (*p == '0' && !significant) {
                exponent--;
                continue;
            }
            if (significant < 780) {
                if (!digits.length)
                    digits.length = 1, digits.word[0] = 0;
                __big_mul_add(&digits, 10, *p - '0');
                significant++;
                exponent--;
            } else {
                dropped |= *p != '0';
            }
        }
    }
    if (!any) {
        if (end)
            *end = (char *)s;
        return 0.0;
    }
    if (*p == 'e' || *p == 'E') {
        const char *q = p + 1;
        int sign = 1, e = 0;
        if (*q == '-' || *q == '+')
            sign = *q++ == '-' ? -1 : 1;
        if (isdigit((unsigned char)*q)) {
            while (isdigit((unsigned char)*q)) {
                if (e < 100000)
                    e = e * 10 + (*q - '0');
                q++;
            }
            exponent += sign * e;
            p = q;
        }
    }
    if (end)
        *end = (char *)p;

    while (digits.length && !digits.word[digits.length - 1])
        digits.length--;
    if (!digits.length)
        return negative ? -0.0 : 0.0;
    if (exponent + significant > 310) {
        errno = ERANGE;
        value = __bits_double(0x7ff0000000000000UL);
        return negative ? -value : value;
    }
    if (exponent + significant < -330) {
        errno = ERANGE;
        return negative ? -0.0 : 0.0;
    }

    if (exponent >= 0) {
        /* An integer: D * 10^E, then its top 64 bits. */
        int i, bits, shift;
        unsigned long mantissa = 0;
        int sticky = dropped;
        for (i = 0; i < exponent; i++)
            __big_mul_add(&digits, 10, 0);
        bits = __big_bits(&digits);
        shift = bits > 64 ? bits - 64 : 0;
        for (i = 0; i < 64 && shift + i < bits; i++) {
            int bit = shift + i;
            if ((digits.word[bit / 32] >> (bit % 32)) & 1)
                mantissa |= 1UL << i;
        }
        for (i = 0; i < shift; i++)
            if ((digits.word[i / 32] >> (i % 32)) & 1) {
                sticky = 1;
                break;
            }
        value = __make_double(mantissa, shift, sticky);
    } else {
        /* A fraction: D / 10^-E, scaled by 2^s so that the quotient has
         * 63 or 64 bits, computed bit by bit; the remainder is sticky. */
        struct __bignum divisor;
        struct __bignum remainder;
        int i, s2, bits, position;
        unsigned long mantissa = 0;
        divisor.length = 1;
        divisor.word[0] = 1;
        for (i = 0; i < -exponent; i++)
            __big_mul_add(&divisor, 10, 0);
        s2 = __big_bits(&digits) - __big_bits(&divisor) - 63;
        remainder = digits;
        if (s2 < 0)
            __big_shift_left(&remainder, -s2);
        else
            __big_shift_left(&divisor, s2);
        bits = __big_bits(&remainder) - __big_bits(&divisor);
        __big_shift_left(&divisor, bits > 0 ? bits : 0);
        for (position = bits; position >= 0; position--) {
            int w;
            mantissa <<= 1;
            if (__big_compare(&remainder, &divisor) >= 0) {
                __big_subtract(&remainder, &divisor);
                mantissa |= 1;
            }
            /* divisor >>= 1 */
            for (w = 0; w < divisor.length; w++) {
                divisor.word[w] >>= 1;
                if (w + 1 < divisor.length)
                    divisor.word[w] |= divisor.word[w + 1] << 31;
            }
            while (divisor.length && !divisor.word[divisor.length - 1])
                divisor.length--;
        }
        value = __make_double(mantissa, s2, remainder.length != 0 || dropped);
    }
    return negative ? -value : value;
}

float strtof(const char *s, char **end) {
    return (float)strtod(s, end);
}

double atof(const char *s) {
    return strtod(s, NULL);
}

/* ============================================================
 *  9. Sorting and searching
 * ============================================================ */

static void __swap_bytes(char *a, char *b, size_t n) {
    while (n--) {
        char t = *a;
        *a++ = *b;
        *b++ = t;
    }
}

/* A stable merge sort, as glibc's qsort does when it can allocate. */
static void __merge_sort(char *base, char *tmp, size_t count, size_t size, int (*compare)(const void *, const void *)) {
    size_t left, right, i, j, k;
    if (count <= 1)
        return;
    left = count / 2;
    right = count - left;
    __merge_sort(base, tmp, left, size, compare);
    __merge_sort(base + left * size, tmp, right, size, compare);
    i = 0;
    j = 0;
    k = 0;
    while (i < left && j < right) {
        char *a = base + i * size;
        char *b = base + (left + j) * size;
        if (compare(a, b) <= 0) {
            memcpy(tmp + k * size, a, size);
            i++;
        } else {
            memcpy(tmp + k * size, b, size);
            j++;
        }
        k++;
    }
    while (i < left) {
        memcpy(tmp + k * size, base + i * size, size);
        i++;
        k++;
    }
    memcpy(base, tmp, k * size);
}

void qsort(void *base, size_t count, size_t size, int (*compare)(const void *, const void *)) {
    char *tmp;
    if (count <= 1 || !size)
        return;
    tmp = malloc(count * size);
    if (!tmp) {
        /* No memory: an insertion sort, in place, still stable. */
        size_t i, j;
        for (i = 1; i < count; i++)
            for (j = i; j > 0 && compare((char *)base + (j - 1) * size, (char *)base + j * size) > 0; j--)
                __swap_bytes((char *)base + (j - 1) * size, (char *)base + j * size, size);
        return;
    }
    __merge_sort(base, tmp, count, size, compare);
    free(tmp);
}

void *bsearch(const void *key, const void *base, size_t count, size_t size, int (*compare)(const void *, const void *)) {
    size_t low = 0, high = count;
    while (low < high) {
        size_t middle = (low + high) / 2;
        const char *p = (const char *)base + middle * size;
        int c = compare(key, p);
        if (c == 0)
            return (void *)p;
        if (c < 0)
            high = middle;
        else
            low = middle + 1;
    }
    return NULL;
}

/* ============================================================
 *  10. Random numbers: glibc's random(), TYPE_3
 *
 * An additive feedback generator r[i] = r[i-3] + r[i-31], seeded by a
 * Lehmer generator, the first 310 outputs discarded: rand() after
 * srand(s) gives the numbers glibc gives.
 * ============================================================ */

static int32_t __rand_state[34];
static int __rand_index;
static int __rand_seeded;

void srand(unsigned seed) {
    int i;
    if (seed == 0)
        seed = 1;
    __rand_state[0] = (int32_t)seed;
    for (i = 1; i < 31; i++) {
        /* 16807 * r % 2147483647, by Schrage's method as glibc does. */
        long hi = __rand_state[i - 1] / 127773;
        long lo = __rand_state[i - 1] % 127773;
        long word = 16807 * lo - 2836 * hi;
        if (word < 0)
            word += 2147483647;
        __rand_state[i] = (int32_t)word;
    }
    for (i = 31; i < 34; i++)
        __rand_state[i] = __rand_state[i - 31];
    __rand_index = 0;
    __rand_seeded = 1;
    for (i = 34; i < 344; i++)
        rand();
}

int rand(void) {
    uint32_t value;
    int i;
    if (!__rand_seeded)
        srand(1);
    i = __rand_index;
    /* The ring holds the last 34 values: r[i-31] is 3 ahead, r[i-3] 31. */
    value = (uint32_t)__rand_state[(i + 3) % 34] + (uint32_t)__rand_state[(i + 31) % 34];
    __rand_state[i] = (int32_t)value;
    __rand_index = (i + 1) % 34;
    return (int)(value >> 1);
}

/* ============================================================
 *  11. Time
 * ============================================================ */

time_t time(time_t *t) {
    time_t now = __sys_time();
    if (t)
        *t = now;
    return now;
}

clock_t clock(void) {
    return __sys_clock_us();
}

double difftime(time_t a, time_t b) {
    return (double)(a - b);
}

static struct tm __tm;

struct tm *gmtime(const time_t *t) {
    long days = *t / 86400;
    long seconds = *t % 86400;
    long era, doe, yoe, doy, mp, year;
    if (seconds < 0) {
        seconds += 86400;
        days--;
    }
    __tm.tm_hour = (int)(seconds / 3600);
    __tm.tm_min = (int)(seconds / 60 % 60);
    __tm.tm_sec = (int)(seconds % 60);
    __tm.tm_wday = (int)((days + 4) % 7 + 7) % 7;
    /* Civil date from days since 1970 (Howard Hinnant's algorithm). */
    days += 719468;
    era = (days >= 0 ? days : days - 146096) / 146097;
    doe = days - era * 146097;
    yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    year = yoe + era * 400;
    doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    mp = (5 * doy + 2) / 153;
    __tm.tm_mday = (int)(doy - (153 * mp + 2) / 5 + 1);
    __tm.tm_mon = (int)(mp < 10 ? mp + 2 : mp - 10);
    if (__tm.tm_mon <= 1)
        year++;
    __tm.tm_year = (int)(year - 1900);
    {
        static const int before[12] = {0, 31, 59, 90, 120, 151, 181, 212, 243, 273, 304, 334};
        int leap = (year % 4 == 0 && year % 100 != 0) || year % 400 == 0;
        __tm.tm_yday = before[__tm.tm_mon] + __tm.tm_mday - 1 + (leap && __tm.tm_mon > 1);
    }
    __tm.tm_isdst = 0;
    return &__tm;
}

struct tm *localtime(const time_t *t) {
    return gmtime(t);
}

time_t mktime(struct tm *tm) {
    long year = tm->tm_year + 1900L, month = tm->tm_mon + 1L;
    long era, yoe, doy, doe, days;
    if (month <= 2)
        year--;
    era = (year >= 0 ? year : year - 399) / 400;
    yoe = year - era * 400;
    doy = (153 * (month > 2 ? month - 3 : month + 9) + 2) / 5 + tm->tm_mday - 1;
    doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    days = era * 146097 + doe - 719468;
    return days * 86400 + tm->tm_hour * 3600L + tm->tm_min * 60L + tm->tm_sec;
}
