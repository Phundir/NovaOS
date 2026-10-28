#!/bin/sh
# Runs a command inside the Alpine root of build/ports/alpine, as a fake
# root, without a container engine: bubblewrap and a user namespace.
# The Web Browser port (phase 11) is built and inspected here, because
# Alpine is the distribution that builds Firefox against musl. The
# repository is visible inside as /nova.
REPO="$(cd "$(dirname "$0")/../.." && pwd)"
ROOT="$REPO/build/ports/alpine"
exec bwrap --bind "$ROOT" / --dev /dev --proc /proc --tmpfs /tmp \
    --ro-bind /etc/resolv.conf /etc/resolv.conf --bind "$REPO" /nova \
    --unshare-user --uid 0 --gid 0 --share-net \
    --setenv PATH /usr/sbin:/usr/bin:/sbin:/bin --setenv HOME /root \
    /bin/sh -c "$*"
