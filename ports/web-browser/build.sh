#!/bin/sh
# Builds the Web Browser fork of Firefox ESR for Nova OS (phase 11.7).
#
# Runs inside the Alpine root (tools/ports/alpine.sh), from the recipe of
# Alpine's firefox-esr package, which already builds Firefox against musl:
# same patches, same system libraries, same toolchain. What differs is the
# fork itself:
#
#   * branding "Web Browser" (browser/branding/nova, derived from the
#     unofficial branding: the Firefox name and logo are trademarks);
#   * no D-Bus, no Wi-Fi geolocation, no crash reporter, no sandbox --
#     Nova provides none of what they talk to, and the sandbox relies on
#     seccomp and user namespaces;
#   * no profile-guided optimisation: the Alpine recipe builds twice and
#     runs the browser under Xvfb in between, hours for a gain that does
#     not matter before the port works;
#   * the patches of ports/web-browser/nova/, applied after Alpine's.
#
# Result: /home/build/firefox-$VERSION/obj/dist/web-browser-*.tar.xz
set -e
VERSION=140.12.0
SRC=/home/build/firefox-$VERSION
RECIPE=/nova/ports/web-browser
cd /home/build

if [ ! -d "$SRC" ]; then
    tar -xf firefox-${VERSION}esr.source.tar.xz
fi
cd "$SRC"

if [ ! -e .nova-patched ]; then
    for p in abseil-cpp bmo-1952657-no-execinfo fix-fortify-system-wrappers \
             fix-rust-target glean-stub lfs64 rust-lto-thin \
             sandbox-sched_setscheduler sqlite-ppc rust1.90-ppc wasip1 \
             esr-metainfo fix-webrtc-glibcisms icu78 llvm-22 python-3.14-fixes; do
        patch -p1 -s < "$RECIPE/$p.patch"
    done
    for p in "$RECIPE"/nova/*.patch; do
        [ -e "$p" ] && patch -p1 -s < "$p"
    done
    cp "$RECIPE/stab.h" toolkit/crashreporter/google-breakpad/src/
    sed -i 's/\("files":{\)[^}]*/\1/' third_party/rust/audio_thread_priority/.cargo-checksum.json
    sed -i 's/\("files":{\)[^}]*/\1/' third_party/rust/cc/.cargo-checksum.json
    # Branding: the unofficial one, renamed.
    rm -rf browser/branding/nova
    cp -r browser/branding/unofficial browser/branding/nova
    sed -i 's/^MOZ_APP_DISPLAYNAME=.*/MOZ_APP_DISPLAYNAME="Web Browser"/' browser/branding/nova/configure.sh
    for f in browser/branding/nova/locales/en-US/brand.ftl browser/branding/nova/locales/en-US/brand.properties; do
        [ -e "$f" ] && sed -i -e 's/Nightly/Web Browser/g' -e 's/Firefox/Web Browser/g' "$f"
    done
    touch .nova-patched
fi

export CFLAGS="-O2 -g0"
export CXXFLAGS="-O2 -g0 -Wno-deprecated-builtins -Wno-deprecated-declarations"
export LDFLAGS="-Wl,-rpath,/usr/lib/web-browser -Wl,--thinlto-jobs=24"
export SHELL=/bin/sh
export USE_SHORT_LIBNAME=1
export MACH_BUILD_PYTHON_NATIVE_PACKAGE_SOURCE=system
export MOZ_APP_REMOTINGNAME=web-browser
export MOZBUILD_STATE_PATH=/home/build/mozbuild
export MOZ_NOSPAM=1
export RUST_TARGET=x86_64-alpine-linux-musl
export CC=clang-22
export CXX=clang++-22
export LD_PRELOAD=/usr/lib/libmimalloc-insecure.so.2
unset RUSTFLAGS CARGO_PROFILE_RELEASE_OPT_LEVEL CARGO_PROFILE_RELEASE_LTO

cat > nova-mozconfig <<CONF
ac_add_options --disable-bootstrap
ac_add_options --disable-cargo-incremental
ac_add_options --disable-debug
ac_add_options --disable-debug-symbols
ac_add_options --disable-install-strip
ac_add_options --disable-jemalloc
ac_add_options --disable-strip
ac_add_options --disable-tests
ac_add_options --disable-updater
ac_add_options --disable-crashreporter
ac_add_options --disable-sandbox
ac_add_options --disable-dbus
ac_add_options --disable-necko-wifi
ac_add_options --disable-profiling

ac_add_options --enable-alsa
ac_add_options --enable-default-toolkit=cairo-gtk3-wayland
ac_add_options --enable-ffmpeg
ac_add_options --enable-hardening
ac_add_options --enable-linker=lld
ac_add_options --enable-optimize="-O2"
ac_add_options --enable-packed-relative-relocs
ac_add_options --enable-pulseaudio
ac_add_options --enable-release

ac_add_options --enable-system-pixman
ac_add_options --with-system-ffi
ac_add_options --with-system-icu
ac_add_options --with-system-jpeg
ac_add_options --with-system-libevent
ac_add_options --with-system-libvpx
ac_add_options --with-system-nspr
ac_add_options --with-system-nss
ac_add_options --with-system-png
ac_add_options --with-system-webp
ac_add_options --with-system-zlib

ac_add_options --with-branding=browser/branding/nova
ac_add_options --prefix=/usr
ac_add_options --with-app-name=web-browser
ac_add_options --with-distribution-id=os.nova
ac_add_options --with-libclang-path=/usr/lib
ac_add_options --with-wasi-sysroot=/usr/share/wasi-sysroot
ac_add_options --host=x86_64-alpine-linux-musl
ac_add_options --target=x86_64-alpine-linux-musl

mk_add_options MOZ_OBJDIR=$SRC/obj
mk_add_options MOZ_PARALLEL_BUILD=24
CONF
export MOZCONFIG=$SRC/nova-mozconfig

./mach build
./mach package
ls -la obj/dist/*.tar.* 2>/dev/null || ls obj/dist
