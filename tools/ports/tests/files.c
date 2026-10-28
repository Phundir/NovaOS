/* Files: tmpfs, root image, directories, links, /proc, memfd (11.5). */
#define _GNU_SOURCE
#include <dirent.h>
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/stat.h>
#include <unistd.h>

int main(int argc, char **argv) {
    char buf[256];
    int fd = open("/tmp/essai.txt", O_CREAT | O_RDWR | O_TRUNC, 0644);
    if (fd < 0) return 1;
    if (write(fd, "bonjour nova\n", 13) != 13) return 2;
    if (lseek(fd, 8, SEEK_SET) != 8) return 3;
    if (read(fd, buf, 4) != 4 || memcmp(buf, "nova", 4) != 0) return 4;
    struct stat st;
    if (fstat(fd, &st) != 0 || st.st_size != 13 || !S_ISREG(st.st_mode)) return 5;
    close(fd);

    if (mkdir("/tmp/dossier", 0755) != 0) return 6;
    if (rename("/tmp/essai.txt", "/tmp/dossier/renomme.txt") != 0) return 7;
    if (symlink("/tmp/dossier/renomme.txt", "/tmp/lien") != 0) return 8;
    ssize_t n = readlink("/tmp/lien", buf, sizeof buf);
    if (n != 24 || memcmp(buf, "/tmp/dossier/renomme.txt", 24) != 0) return 9;
    if (stat("/tmp/lien", &st) != 0 || st.st_size != 13) return 10;

    int entries = 0;
    DIR *d = opendir("/etc");
    if (!d) return 11;
    struct dirent *e;
    while ((e = readdir(d))) entries++;
    closedir(d);
    if (entries < 5) return 12;

    n = readlink("/proc/self/exe", buf, sizeof buf - 1);
    if (n <= 0) return 13;
    buf[n] = 0;
    /* The program's own path: /nova-tests/files, or its native build
     * under /nova-tests/natif (phase 12). */
    const char *self = argc > 0 ? argv[0] : "/nova-tests/files";
    if (strcmp(buf, self) != 0) { printf("files: /proc/self/exe = %s\n", buf); return 14; }

    fd = open("/etc/alpine-release", O_RDONLY);
    if (fd < 0) return 15;
    n = read(fd, buf, sizeof buf - 1);
    close(fd);
    if (n < 3) return 16;

    /* memfd shared between a write and a mapping. */
    int m = memfd_create("essai", MFD_CLOEXEC);
    if (m < 0) return 17;
    if (ftruncate(m, 8192) != 0) return 18;
    char *map = mmap(0, 8192, PROT_READ | PROT_WRITE, MAP_SHARED, m, 0);
    if (map == MAP_FAILED) return 19;
    strcpy(map + 4096, "partage");
    if (pread(m, buf, 7, 4096) != 7 || memcmp(buf, "partage", 7) != 0) return 20;

    if (unlink("/tmp/lien") != 0 || unlink("/tmp/dossier/renomme.txt") != 0 || rmdir("/tmp/dossier") != 0) return 21;
    if (access("/tmp/dossier", F_OK) == 0) return 22;
    /* The root is no longer a read-only image unpacked into memory: it
     * is a directory of the Nova volume, seen through src/linux/vfs/novafs.rs.
     * What is written there reaches the disk, so the test writes, reads
     * back through a fresh descriptor -- a page cache that kept the old
     * content would show up here -- then removes its file. */
    fd = open("/etc/nova-essai", O_CREAT | O_WRONLY | O_TRUNC, 0644);
    if (fd < 0) return 23;
    if (write(fd, "racine ecrivable\n", 17) != 17) return 24;
    close(fd);
    fd = open("/etc/nova-essai", O_RDONLY);
    if (fd < 0) return 25;
    n = read(fd, buf, sizeof buf - 1);
    close(fd);
    if (n != 17 || memcmp(buf, "racine ecrivable\n", 17) != 0) return 26;
    if (unlink("/etc/nova-essai") != 0) return 27;
    if (access("/etc/nova-essai", F_OK) == 0) return 28;

    /* A writable shared mapping of a file of the volume (SQLite's -shm
     * index): what the mapping writes, read() sees at once, and what
     * write() writes, the mapping sees; msync puts it on the disk. */
    fd = open("/etc/nova-partage", O_CREAT | O_RDWR | O_TRUNC, 0644);
    if (fd < 0) return 29;
    if (ftruncate(fd, 8192) != 0) return 30;
    char *shared = mmap(0, 8192, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
    if (shared == MAP_FAILED) return 31;
    strcpy(shared + 4100, "par la projection");
    if (pread(fd, buf, 17, 4100) != 17 || memcmp(buf, "par la projection", 17) != 0) return 32;
    if (pwrite(fd, "par write", 9, 10) != 9 || memcmp(shared + 10, "par write", 9) != 0) return 33;
    if (msync(shared, 8192, MS_SYNC) != 0) return 34;
    munmap(shared, 8192);
    close(fd);
    fd = open("/etc/nova-partage", O_RDONLY);
    if (fd < 0) return 35;
    if (pread(fd, buf, 17, 4100) != 17 || memcmp(buf, "par la projection", 17) != 0) return 36;
    close(fd);
    if (unlink("/etc/nova-partage") != 0) return 37;

    printf("files: tmpfs, racine Nova ecrivable (%d entrees dans /etc), liens, /proc, memfd, projection partagee conformes\n", entries);
    return 0;
}
