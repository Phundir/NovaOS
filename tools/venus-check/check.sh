#!/bin/sh
# Compares the Venus encoder Nova generated (src/gpu/venus/protocol.rs) with the
# reference encoder of venus-protocol, command by command, byte by byte.
#
#   tools/venus-check/check.sh      (after `make venus`)
#
# The reference headers are produced by venus-protocol's own generator at
# the commit tools/gen-venus.py pins, in build/venus/; they are only read
# here, never part of Nova.
set -e
REPO="$(cd "$(dirname "$0")/../.." && pwd)"
WORK="$REPO/build/venus"
COMMIT=$(sed -n "s/^VENUS_PROTOCOL_COMMIT = '\(.*\)'/\1/p" "$REPO/tools/gen-venus.py")
mkdir -p "$WORK"
cd "$WORK"
if [ ! -d venus-protocol ]; then
    git clone -q https://gitlab.freedesktop.org/virgl/venus-protocol.git
fi
git -C venus-protocol checkout -q "$COMMIT"
if [ ! -x venv/bin/python ]; then
    python3 -m venv venv
    venv/bin/pip -q install mako
fi
mkdir -p ref
echo "/* reference */" > ref/banner
(cd venus-protocol && "$WORK/venv/bin/python" vn_protocol.py --srcdir . --outdir "$WORK/ref" --banner "$WORK/ref/banner" 2>/dev/null)
python3 "$REPO/tools/venus-check/gen-check.py" --xml "$WORK/xml" --out "$WORK/check"
cp "$REPO/tools/venus-check/check_prelude.h" "$WORK/check/"
sed "s#@REPO@#$REPO#g" "$REPO/tools/venus-check/check_prelude.rs" > "$WORK/check/check_prelude.rs"
cc -std=c11 -O0 -w -I"$REPO/tools/venus-check" -I"$WORK/ref" -I"$WORK/venus-protocol/include" \
    -o "$WORK/check/check-c" "$WORK/check/check.c"
rustc --edition 2024 -O -o "$WORK/check/check-rs" "$WORK/check/check.rs" 2>&1 | grep -v "^warning\|^ *|\|^ *=\|^$" || true
"$WORK/check/check-c" > "$WORK/check/c.txt"
"$WORK/check/check-rs" > "$WORK/check/rs.txt"
total=$(wc -l < "$WORK/check/c.txt")
if cmp -s "$WORK/check/c.txt" "$WORK/check/rs.txt"; then
    echo "[ok] encodeur Venus identique a la reference - $total commandes"
else
    diff "$WORK/check/c.txt" "$WORK/check/rs.txt" | head -20
    bad=$(diff "$WORK/check/c.txt" "$WORK/check/rs.txt" | grep -c '^<' || true)
    echo "[!!] encodeur Venus : $bad commande(s) sur $total differente(s)"
    exit 1
fi
