/* Same as hello.c, dynamically linked: loads ld-musl and libc.so
 * through PT_INTERP (phase 11.5). */
#include <unistd.h>
#include <string.h>
int main(int argc, char **argv) {
    const char *msg = "bonjour depuis musl, lie dynamiquement\n";
    write(1, msg, strlen(msg));
    return argc == 1 ? 0 : 8;
}
