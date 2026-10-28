#!/bin/sh
# Builds the Nova C library (spec 6.7): musl 1.2.6, the version of the
# Alpine root, with Alpine's patches, whose system calls are the Nova
# calls of the ported programs' domains instead of Linux's.
#
#   tools/ports/nova-libc/build.sh      -> build/ports/nova-libc/out/
#
# out/libc.so is the library and its own dynamic linker, as in musl; it
# is installed in the Nova volume at /sys/runtimes/c/libc.so (`make
# nova-libc`), where the kernel's loader takes it for every program of
# the Nova interface. out/COPYRIGHT is musl's licence (MIT), which goes
# with it.
#
# The Nova part is tools/ports/nova-libc/overlay/, copied over musl's
# tree: arch/x86_64/syscall_arch.h, six assembly files, and
# src/nova/nova_syscall.c, the translation of musl's requests into Nova
# calls. Nothing else of musl changes -- its user ABI stays the one the
# programs and libraries of the Alpine root were compiled against.
set -e
REPO="$(cd "$(dirname "$0")/../../.." && pwd)"
WORK="$REPO/build/ports/nova-libc"
VERSION=1.2.6
BRANCH=3.24-stable
mkdir -p "$WORK" "$WORK/out"
cd "$WORK"

fetch() {
    [ -f "$2" ] || curl -sfL -o "$2" "$1"
}
fetch "https://musl.libc.org/releases/musl-$VERSION.tar.gz" "musl-$VERSION.tar.gz"
fetch "https://gitlab.alpinelinux.org/alpine/aports/-/raw/$BRANCH/main/musl/APKBUILD" APKBUILD
PATCHES="handle-aux-at_base.patch 0001-add-stub-for-pthread_mutexattr_setprioceiling.patch CVE-2026-6042.patch CVE-2026-40200.patch"
for p in $PATCHES; do
    fetch "https://gitlab.alpinelinux.org/alpine/aports/-/raw/$BRANCH/main/musl/$p" "$p"
done
# The sums Alpine's recipe carries for the same files.
for f in "musl-$VERSION.tar.gz" $PATCHES; do
    want=$(grep "  $f\$" APKBUILD | cut -d' ' -f1)
    have=$(sha512sum "$f" | cut -d' ' -f1)
    [ -n "$want" ] && [ "$want" = "$have" ] || { echo "[!!] somme de controle fausse : $f"; exit 1; }
done

rm -rf "musl-$VERSION"
tar xzf "musl-$VERSION.tar.gz"
cd "musl-$VERSION"
for p in $PATCHES; do
    patch -p1 -s < "../$p"
done
# What Alpine's recipe does besides its patches (APKBUILD, prepare()):
# the hand-written x86-64 memcpy and memmove go, for the C versions. The
# programs of the Alpine root were tested against that library; this one
# must behave the same. (memmove.s copied backwards with the direction
# flag set, and the kernel's page fault entry did not clear it: DECISIONS,
# "Le drapeau de direction appartient au programme".)
rm src/string/x86_64/memcpy.s src/string/x86_64/memmove.s
cp -r "$REPO/tools/ports/nova-libc/overlay/." .
echo "$VERSION-nova" > VERSION

"$REPO/tools/ports/alpine.sh" "
set -e
cd /nova/build/ports/nova-libc/musl-$VERSION
# musl getcwd.c returns a strdup of its local buffer, never the buffer:
# a false positive of -Wreturn-local-addr, silenced by name.
CFLAGS=-Wno-return-local-addr ./configure --prefix=/usr --syslibdir=/lib --enable-debug >/dev/null
make -j\$(nproc) >/dev/null
"
# Not one Linux call left: every syscall instruction of the library must
# sit in the Nova translation or in the assembly files rewritten for it.
strays=$(objdump -d --no-show-raw-insn lib/libc.so \
    | awk '/^[0-9a-f]+ </ { fn = $2 } /\tsyscall/ { print fn }' \
    | grep -v -x -e '<__nova_syscall>:' -e '<__clone>:' -e '<vfork>:' -e '<__restore_rt>:' \
        -e '<__set_thread_area>:' -e '<__unmapself>:' | sort -u)
if [ -n "$strays" ]; then
    echo "[!!] instruction syscall hors de la traduction Nova : $strays"
    exit 1
fi
# Debug information out, symbols kept (the linker and backtraces use them).
strip --strip-debug -o "$WORK/out/libc.so" lib/libc.so
cp COPYRIGHT "$WORK/out/COPYRIGHT"
echo "[ok] bibliotheque C Nova : $WORK/out/libc.so ($(stat -c %s "$WORK/out/libc.so") octets)"
