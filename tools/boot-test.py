#!/usr/bin/env python3
"""Boots Nova OS under QEMU without a window and judges the serial output.

The kernel never returns: this script reads the serial line as it comes,
stops QEMU as soon as every expected self-test verdict has been printed,
and fails on the first sign of trouble -- an "ECHEC" verdict, a kernel
panic, a BOOT_ERR_ code, a failed allocation -- or when the time is up
with verdicts still missing.

It is what `make test` and the CI run; a developer reads the same lines by
eye after `make run` (CLAUDE.md, 2.7). The whole serial output is kept in
the log file, so a failure can be read afterwards.

    tools/boot-test.py --timeout 600 --log build/test.log -- \\
        qemu-system-x86_64 -cdrom build/nova-os.iso ... -display none

Exit code: 0 all verdicts green, 1 a failure seen, 2 timeout.
"""

import argparse
import os
import re
import selectors
import signal
import subprocess
import sys
import time

# Verdicts a boot without GPU, Linux root image or native programs must
# print. Each is a regular expression on the line, matched from the start.
# "non eprouvee" is accepted where the missing piece is an optional image
# (11.5: the Linux root of the browser, 12: the native test programs).
DEFAULT_VERDICTS = [
    (r"\[test\] phase 7\.1 validee", "7.1 framebuffer"),
    (r"\[test\] phases 3 et 4 validees", "3-4 binaires et fichiers"),
    (r"\[test\] phase 5 validee", "5 API systeme"),
    (r"\[test\] phase 6 validee", "6 couche OS"),
    (r"\[test\] phase 7 validee", "7 systeme graphique"),
    (r"\[test\] phase 8 validee", "8 SPM"),
    (r"\[test\] phase 9 validee", "9 SDK"),
    (r"\[test\] phase 10 validee", "10 finitions"),
    (r"\[test\] navigateur valide", "navigateur integre"),
    (r"\[test\] phase 11\.2 validee", "11.2 SSE"),
    (r"\[test\] phase 11\.3 validee", "11.3 espaces prives"),
    (r"\[test\] phase 11\.5 (validee|non eprouvee)", "11.5 Linux"),
    (r"\[test\] phase 12 (validee|non eprouvee)", "12 programmes natifs"),
    (r"\[test\] phase 13 validee", "13 pile graphique"),
    (r"\[test\] phase 15 validee", "15 mises a jour atomiques"),
    (r"\[test\] phase 16\.0 validee", "16.0 shell et terminal"),
    (r"\[test\] phase 16\.1 validee", "16.1 moteur JavaScript"),
    (r"\[test\] phase 16\.2 validee", "16.2 DOM et navigateur"),
    (r"\[test\] phase 16\.3 validee", "16.3 dev interpret (Node)"),
    (r"\[test\] phase 16\.4 validee", "16.4 debogueur"),
    (r"\[test\] phase 16\.5 validee", "16.5 dev interpret (Python)"),
]

# Verdicts that only exist when a framebuffer does (-vga none drops them).
SCREEN_VERDICTS = {"7.1 framebuffer", "7 systeme graphique"}

FAILURES = [
    re.compile(r"ECHEC"),
    re.compile(r"KERNEL PANIC"),
    re.compile(r"BOOT_ERR_0x"),
    re.compile(r"ALLOCATION ECHOUEE"),
    re.compile(r"triple faute|TRIPLE FAULT", re.IGNORECASE),
]


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--timeout", type=float, default=600,
                        help="seconds before giving up (default 600)")
    parser.add_argument("--log", default="build/test.log",
                        help="file that receives the serial output")
    parser.add_argument("--expect", action="append", default=[],
                        help="extra regular expression a line must match")
    parser.add_argument("--no-screen", action="store_true",
                        help="boot without framebuffer: skip 7.1 and 7")
    parser.add_argument("--no-default", action="store_true",
                        help="expect only the --expect lines (recovery boot)")
    parser.add_argument("--type", action="append", default=[], metavar="REGEX=>TEXT",
                        help="when a line matches REGEX, type TEXT and Enter on the serial console")
    parser.add_argument("--linger", type=float, default=3,
                        help="seconds to keep reading after a failure")
    parser.add_argument("qemu", nargs=argparse.REMAINDER,
                        help="QEMU command line, after --")
    args = parser.parse_args()

    command = args.qemu[1:] if args.qemu[:1] == ["--"] else args.qemu
    if not command:
        parser.error("no QEMU command given (after --)")

    expected = [] if args.no_default else [
        (re.compile(p), name) for p, name in DEFAULT_VERDICTS
        if not (args.no_screen and name in SCREEN_VERDICTS)]
    expected += [(re.compile(p), p) for p in args.expect]
    pending = list(expected)
    typing = []
    for item in args.type:
        pattern, _, text = item.partition("=>")
        typing.append([re.compile(pattern), text, False])

    os.makedirs(os.path.dirname(os.path.abspath(args.log)), exist_ok=True)
    log = open(args.log, "w", encoding="utf-8", errors="replace")

    print("[boot-test] " + " ".join(command), flush=True)
    start = time.monotonic()
    qemu = subprocess.Popen(command + ["-serial", "stdio"],
                            stdin=subprocess.PIPE if typing else subprocess.DEVNULL,
                            stdout=subprocess.PIPE,
                            stderr=subprocess.STDOUT)
    selector = selectors.DefaultSelector()
    selector.register(qemu.stdout, selectors.EVENT_READ)

    failures = []
    fail_deadline = None
    buffer = b""
    status = None

    def handle(line):
        nonlocal fail_deadline
        log.write(line + "\n")
        for pattern in FAILURES:
            if pattern.search(line):
                failures.append(line)
                if fail_deadline is None:
                    fail_deadline = time.monotonic() + args.linger
        for item in list(pending):
            if item[0].search(line):
                pending.remove(item)
                elapsed = time.monotonic() - start
                print("[boot-test] %6.1f s  %s" % (elapsed, line), flush=True)
        for item in typing:
            if not item[2] and item[0].search(line):
                item[2] = True
                print("[boot-test]          tape : " + item[1], flush=True)
                # Slowly, as a person would: the shell polls the UART.
                for char in (item[1] + "\r").encode():
                    qemu.stdin.write(bytes([char]))
                    qemu.stdin.flush()
                    time.sleep(0.01)

    while True:
        now = time.monotonic()
        if fail_deadline is not None and now >= fail_deadline:
            status = 1
            break
        if not pending and not failures:
            status = 0
            break
        if now - start > args.timeout:
            status = 2
            break
        for _key, _mask in selector.select(timeout=0.5):
            chunk = os.read(qemu.stdout.fileno(), 65536)
            if not chunk:
                status = 1 if failures or pending else 0
                if status:
                    failures.append("QEMU s'est arrete (code %s)" % qemu.poll())
                break
            buffer += chunk
            while b"\n" in buffer:
                raw, buffer = buffer.split(b"\n", 1)
                handle(raw.decode("utf-8", "replace").rstrip("\r"))
        if status is not None:
            break

    if buffer:
        handle(buffer.decode("utf-8", "replace"))
    if qemu.poll() is None:
        qemu.send_signal(signal.SIGTERM)
        try:
            qemu.wait(timeout=10)
        except subprocess.TimeoutExpired:
            qemu.kill()
    log.close()

    elapsed = time.monotonic() - start
    if status == 0:
        print("[boot-test] OK - %d verdict(s) en %.0f s - journal : %s"
              % (len(expected), elapsed, args.log))
    else:
        for line in failures:
            print("[boot-test] ECHEC : " + line)
        for _pattern, name in pending:
            print("[boot-test] manquant : " + name)
        reason = "delai depasse" if status == 2 else "echec"
        print("[boot-test] %s apres %.0f s - journal : %s"
              % (reason, elapsed, args.log))
    return status


if __name__ == "__main__":
    sys.exit(main())
