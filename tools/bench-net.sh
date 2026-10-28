#!/usr/bin/env bash
# Network throughput measurement (net::bench).
#
# Serves a file of $BENCH_MIB MiB on 127.0.0.1:$BENCH_PORT -- which the
# guest reaches as 10.0.2.2 through QEMU user networking -- and boots the
# kernel with the fw_cfg option that makes it download that file once its
# network is configured. QEMU is stopped as soon as the result line is in
# the log, or after $BENCH_TIMEOUT seconds.
#
# Usage: tools/bench-net.sh <qemu command and flags...>
set -u

BUILD_DIR=${BUILD_DIR:-build}
BENCH_MIB=${BENCH_MIB:-512}
BENCH_PORT=${BENCH_PORT:-8000}
BENCH_TIMEOUT=${BENCH_TIMEOUT:-240}
LOG="$BUILD_DIR/bench.log"
DATA="$BUILD_DIR/bench/bench.bin"

mkdir -p "$BUILD_DIR/bench"
size=$((BENCH_MIB * 1048576))
if [ ! -f "$DATA" ] || [ "$(stat -c %s "$DATA")" -ne "$size" ]; then
    # Random bytes: nothing on the path may compress them.
    head -c "$size" /dev/urandom > "$DATA"
fi

python3 -m http.server "$BENCH_PORT" --bind 127.0.0.1 --directory "$BUILD_DIR/bench" > /dev/null 2>&1 &
server=$!
trap 'kill $server $qemu 2>/dev/null' EXIT
sleep 1

rm -f "$LOG"
"$@" ${BENCH_DISPLAY:--display none} \
    -fw_cfg "name=opt/nova/netbench,string=10.0.2.2:$BENCH_PORT/bench.bin" \
    -serial "file:$LOG" &
qemu=$!

for _ in $(seq 1 "$BENCH_TIMEOUT"); do
    if grep -aq "\[debit\] .*Mo/s\|\[debit\] ECHEC" "$LOG" 2>/dev/null; then
        break
    fi
    if ! kill -0 "$qemu" 2>/dev/null; then
        break
    fi
    sleep 1
done
kill "$qemu" 2>/dev/null
wait "$qemu" 2>/dev/null

grep -a "\[debit\]\|\[drv-reseau\]" "$LOG" || echo "(no [debit] line: see $LOG)"
