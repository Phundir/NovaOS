#!/bin/sh
# Boots Nova headless with a QMP socket, logs in as admin, waits for the
# Web Browser window and takes a screen capture.
# Usage: [SMP=n] tools/ports/test-window.sh <workdir> [seconds after the window]
set -e
REPO="$(cd "$(dirname "$0")/../.." && pwd)"
W="$1"; WAIT="${2:-90}"
mkdir -p "$W"; rm -f "$W/qmp.sock" "$W/serial.log"
# The 2D card (QEMU_GPU=): QMP's screendump reads the console's surface,
# and QEMU 11 has none while the screen is an OpenGL scanout (the 3D card,
# phase 13). tools/ports/vnc-shot.py captures that case instead.
FLAGS=$(make -s --no-print-directory -C "$REPO" QEMU_AUDIODEV=none QEMU_GPU= ${DISK:+DISK=$DISK} print-qemu-flags)
cd "$REPO"
timeout 1800 qemu-system-x86_64 $FLAGS -smp ${SMP:-8} -snapshot -display none \
    -serial file:"$W/serial.log" -qmp unix:"$W/qmp.sock",server,nowait &
until grep -a -q "ecran affiche (connexion)" "$W/serial.log" 2>/dev/null; do sleep 3; done
sleep 5
python3 tools/ports/qmp.py "$W/qmp.sock" type admin >/dev/null
python3 tools/ports/qmp.py "$W/qmp.sock" key ret >/dev/null
until grep -a -q "nova-gui\] surface" "$W/serial.log"; do sleep 3; done
sleep "$WAIT"
python3 tools/ports/qmp.py "$W/qmp.sock" screendump "$W/screen.ppm" >/dev/null
python3 -c "from PIL import Image; Image.open('$W/screen.ppm').save('$W/screen.png')"
echo "[ok] capture : $W/screen.png"
