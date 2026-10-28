#!/bin/sh
# Incremental rebuild of the Web Browser fork after editing its sources
# (same environment as build.sh), then packaging.
set -e
SRC=/home/build/firefox-140.12.0
cd "$SRC"
export CFLAGS="-O2 -g0"
export CXXFLAGS="-O2 -g0 -Wno-deprecated-builtins -Wno-deprecated-declarations"
export LDFLAGS="-Wl,-rpath,/usr/lib/web-browser -Wl,--thinlto-jobs=24"
export SHELL=/bin/sh USE_SHORT_LIBNAME=1 MACH_BUILD_PYTHON_NATIVE_PACKAGE_SOURCE=system
export MOZ_APP_REMOTINGNAME=web-browser MOZBUILD_STATE_PATH=/home/build/mozbuild MOZ_NOSPAM=1
export RUST_TARGET=x86_64-alpine-linux-musl CC=clang-22 CXX=clang++-22
export LD_PRELOAD=/usr/lib/libmimalloc-insecure.so.2
export MOZCONFIG=$SRC/nova-mozconfig
unset RUSTFLAGS CARGO_PROFILE_RELEASE_OPT_LEVEL CARGO_PROFILE_RELEASE_LTO
./mach build
./mach package
