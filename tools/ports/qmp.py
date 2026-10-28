#!/usr/bin/env python3
"""Minimal QMP client for the tests of the Web Browser port.

    qmp.py <socket> screendump <out.ppm>
    qmp.py <socket> key <keys...>          e.g. key ctrl-l h e l l o ret
    qmp.py <socket> click <x> <y>          absolute, on the usb-tablet
    qmp.py <socket> type <text>

QEMU is started with -qmp unix:<socket>,server,nowait.
"""
import json, socket, sys, time

def connect(path):
    s = socket.socket(socket.AF_UNIX)
    s.connect(path)
    f = s.makefile('rw')
    json.loads(f.readline())
    cmd(f, {"execute": "qmp_capabilities"})
    return f

def cmd(f, obj):
    f.write(json.dumps(obj) + "\n"); f.flush()
    while True:
        r = json.loads(f.readline())
        if "return" in r or "error" in r:
            return r

def send_key(f, combo):
    keys = [{"type": "qcode", "data": k} for k in combo.split("-")]
    return cmd(f, {"execute": "send-key", "arguments": {"keys": keys}})

# Nova's default layout is AZERTY: QEMU sends key positions, so a letter
# is sent as the QWERTY key that sits where the AZERTY letter is.
AZERTY = {"a": "q", "q": "a", "z": "w", "w": "z", "m": "semicolon"}
QCODE = {"?": "shift-m", " ": "spc", ".": "shift-comma", "/": "shift-dot", ":": "dot", "-": "6", "\n": "ret"}

def qcode(ch):
    if ch in QCODE:
        return QCODE[ch]
    # AZERTY: the digits are the shifted characters of the top row.
    if ch.isdigit():
        return "shift-" + ch
    low = ch.lower()
    key = AZERTY.get(low, low)
    return ("shift-" + key) if ch.isupper() else key

def main():
    f = connect(sys.argv[1])
    op = sys.argv[2]
    if op == "screendump":
        print(cmd(f, {"execute": "screendump", "arguments": {"filename": sys.argv[3]}}))
    elif op == "key":
        for k in sys.argv[3:]:
            send_key(f, k); time.sleep(0.15)
    elif op == "type":
        for ch in " ".join(sys.argv[3:]):
            send_key(f, qcode(ch)); time.sleep(0.12)
    elif op == "click":
        # usb-tablet: absolute coordinates scaled to 0..32767.
        w, h = int(sys.argv[5]) if len(sys.argv) > 5 else 1280, int(sys.argv[6]) if len(sys.argv) > 6 else 800
        x = int(int(sys.argv[3]) * 32767 / w); y = int(int(sys.argv[4]) * 32767 / h)
        ev = lambda events: cmd(f, {"execute": "input-send-event", "arguments": {"events": events}})
        ev([{"type": "abs", "data": {"axis": "x", "value": x}}, {"type": "abs", "data": {"axis": "y", "value": y}}])
        time.sleep(0.1)
        ev([{"type": "btn", "data": {"down": True, "button": "left"}}]); time.sleep(0.1)
        ev([{"type": "btn", "data": {"down": False, "button": "left"}}])
    else:
        print("operation inconnue", op)

main()
