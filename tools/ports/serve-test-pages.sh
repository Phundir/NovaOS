#!/bin/sh
# Serves the Web Browser's test pages (perf.html, son.html, video.html)
# from the host over HTTP, for load-time measurements that cross Nova's
# network stack without the Internet's round trips: QEMU's user network
# shows the host as 10.0.2.2, so the guest opens
# http://10.0.2.2:8123/perf.html. Stop it with Ctrl+C.
# Usage: tools/ports/serve-test-pages.sh [port]
REPO="$(cd "$(dirname "$0")/../.." && pwd)"
exec python3 -m http.server "${1:-8123}" --bind 127.0.0.1 \
    --directory "$REPO/tools/ports/overlay/usr/lib/web-browser/nova"
