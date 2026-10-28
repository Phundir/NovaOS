#!/bin/sh
# Extracts the screenshot printed by /nova/autostart.sh from a serial log.
# Kernel lines may be interleaved with the base64 text: only lines made of
# base64 characters alone are kept.
# Usage: tools/ports/extract-shot.sh <serial.log> <out.png>
sed -n '/=== NOVA-SHOT-BEGIN ===/,/=== NOVA-SHOT-END ===/p' "$1" | tr -d '\r' \
    | grep -E '^[A-Za-z0-9+/=]+$' | base64 -d > "$2" && ls -la "$2"
