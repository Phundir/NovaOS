#!/usr/bin/env python3
"""Captures the screen of a QEMU started with `-vnc unix:<socket>`.

    vnc-shot.py <socket> <out.png> [cursor.png]

Why not QMP's screendump: it reads the console's 2D surface, and QEMU 11
has none while the screen is an OpenGL scanout -- the 3D card of phase 13
(`virtio-vga-gl`), whether the scanout is a texture or a dma-buf blob. With
`-display egl-headless`, QEMU still reads the GL scanout back for VNC: this
takes one frame of it, over the RFB protocol (3.8, no authentication, raw
encoding).

With a third argument, the client also announces the RFB "Cursor"
pseudo-encoding (-239): QEMU then sends the image of the device's cursor
plane (phase 13.4.4), which is saved there with its mask as alpha -- the
proof that the host received the pointer the guest put on the plane.
"""
import socket
import struct
import sys

from PIL import Image


def read(sock, count):
    data = b''
    while len(data) < count:
        chunk = sock.recv(count - len(data))
        if not chunk:
            raise EOFError('connexion VNC fermee')
        data += chunk
    return data


def main():
    path, out = sys.argv[1], sys.argv[2]
    cursor_out = sys.argv[3] if len(sys.argv) > 3 else None
    sock = socket.socket(socket.AF_UNIX)
    sock.connect(path)
    read(sock, 12)
    sock.sendall(b'RFB 003.008\n')
    count = read(sock, 1)[0]
    types = read(sock, count)
    if 1 not in types:
        sys.exit('pas de securite "None" proposee')
    sock.sendall(b'\x01')
    if struct.unpack('>I', read(sock, 4))[0] != 0:
        sys.exit('poignee de main refusee')
    sock.sendall(b'\x01')
    width, height = struct.unpack('>HH', read(sock, 4))
    read(sock, 16)
    read(sock, struct.unpack('>I', read(sock, 4))[0])
    # 32 bits per pixel, little-endian, red at bit 16: BGRX in memory.
    pixel = struct.pack('>BBBBHHHBBB3x', 32, 24, 0, 1, 255, 255, 255, 16, 8, 0)
    sock.sendall(b'\x00\x00\x00\x00' + pixel)
    encodings = [0, -239] if cursor_out else [0]
    sock.sendall(struct.pack('>BBH', 2, 0, len(encodings)) + b''.join(struct.pack('>i', e) for e in encodings))
    sock.sendall(struct.pack('>BBHHHH', 3, 0, 0, 0, width, height))
    image = Image.new('RGB', (width, height))
    cursor = None
    covered = 0
    while covered < width * height or (cursor_out and cursor is None):
        if covered >= width * height:
            # The cursor may come in a later update: ask again.
            sock.sendall(struct.pack('>BBHHHH', 3, 1, 0, 0, width, height))
        kind = read(sock, 1)[0]
        if kind != 0:
            sys.exit('message VNC inattendu %d' % kind)
        read(sock, 1)
        (rects,) = struct.unpack('>H', read(sock, 2))
        for _ in range(rects):
            x, y, w, h, encoding = struct.unpack('>HHHHi', read(sock, 12))
            if encoding == -239:
                data = read(sock, w * h * 4)
                mask = read(sock, (w + 7) // 8 * h)
                cursor = Image.frombuffer('RGB', (w, h), data, 'raw', 'BGRX', 0, 1).convert('RGBA')
                for row in range(h):
                    for column in range(w):
                        bit = mask[row * ((w + 7) // 8) + column // 8] & (0x80 >> (column % 8))
                        r, g, b, _ = cursor.getpixel((column, row))
                        cursor.putpixel((column, row), (r, g, b, 255 if bit else 0))
                print('[ok] curseur %dx%d, point chaud (%d, %d), %d pixels opaques' % (
                    w, h, x, y, sum(bin(byte).count('1') for byte in mask)))
                continue
            if encoding != 0:
                sys.exit('encodage VNC inattendu %d' % encoding)
            data = read(sock, w * h * 4)
            image.paste(Image.frombuffer('RGB', (w, h), data, 'raw', 'BGRX', 0, 1), (x, y))
            covered += w * h
    image.save(out)
    print('[ok] %s : %dx%d' % (out, width, height))
    if cursor_out and cursor is not None:
        cursor.save(cursor_out)


if __name__ == '__main__':
    main()
