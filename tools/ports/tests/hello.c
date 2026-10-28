/* Smallest Linux program for Nova's Linux subsystem (phase 11.5):
 * write(2) to stdout, then exit with a known code. */
#include <unistd.h>
#include <string.h>
int main(int argc, char **argv) {
    const char *msg = "bonjour depuis musl\n";
    write(1, msg, strlen(msg));
    return argc == 1 ? 7 : 8;
}
