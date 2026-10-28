#!/usr/bin/env python3
"""Generates src/web/roots.rs from the trust store of the build machine.

The browser needs trust anchors to verify HTTPS certificates. The project
takes no dependency, so the anchors are data, generated here -- like the
font -- from the CA bundle every Linux distribution ships (Mozilla's list,
through ca-certificates). Only what verification needs is kept: the
subject name, which certificates cite as their issuer, and the public key.

Usage: python3 tools/generer-racines.py [bundle.pem]
"""

import base64
import sys

BUNDLE = sys.argv[1] if len(sys.argv) > 1 else "/etc/ssl/certs/ca-certificates.crt"
OUTPUT = "src/web/roots.rs"

OID_RSA = bytes.fromhex("2a864886f70d010101")
OID_EC = bytes.fromhex("2a8648ce3d0201")
OID_P256 = bytes.fromhex("2a8648ce3d030107")
OID_P384 = bytes.fromhex("2b81040022")
OID_CN = bytes.fromhex("550403")


def element(data, offset):
    """(tag, content start, end) of the element at offset."""
    tag = data[offset]
    first = data[offset + 1]
    if first < 0x80:
        length, header = first, 2
    else:
        count = first & 0x7F
        length = int.from_bytes(data[offset + 2 : offset + 2 + count], "big")
        header = 2 + count
    return tag, offset + header, offset + header + length


def children(data, start, end):
    offset = start
    while offset < end:
        tag, content, stop = element(data, offset)
        yield tag, offset, content, stop
        offset = stop


def common_name(data, start, end):
    for _, _, set_content, set_end in children(data, start, end):
        for _, _, attr_content, attr_end in children(data, set_content, set_end):
            parts = list(children(data, attr_content, attr_end))
            if len(parts) >= 2 and data[parts[0][2] : parts[0][3]] == OID_CN:
                return data[parts[1][2] : parts[1][3]].decode("utf-8", "replace")
    return "?"


def anchor(der):
    _, cert_content, cert_end = element(der, 0)
    tbs = next(children(der, cert_content, cert_end))
    fields = list(children(der, tbs[2], tbs[3]))
    if fields[0][0] == 0xA0:
        fields = fields[1:]
    # serial, signature, issuer, validity, subject, spki
    subject = fields[4]
    spki = fields[5]
    subject_raw = der[subject[1] : subject[3]]
    spki_raw = der[spki[1] : spki[3]]
    algorithm = next(children(der, spki[2], spki[3]))
    algo_parts = list(children(der, algorithm[2], algorithm[3]))
    oid = der[algo_parts[0][2] : algo_parts[0][3]]
    if oid == OID_RSA:
        kind = "RSA"
    elif oid == OID_EC and len(algo_parts) > 1:
        curve = der[algo_parts[1][2] : algo_parts[1][3]]
        kind = {OID_P256: "P-256", OID_P384: "P-384"}.get(curve)
    else:
        kind = None
    name = common_name(der, subject[2], subject[3])
    return subject_raw, spki_raw, kind, name


def main():
    # The bundle carries comments with accented CA names: read as UTF-8.
    text = open(BUNDLE, encoding="utf-8", errors="replace").read()
    anchors = []
    skipped = 0
    for block in text.split("-----BEGIN CERTIFICATE-----")[1:]:
        body = block.split("-----END CERTIFICATE-----")[0]
        der = base64.b64decode("".join(body.split()))
        subject, spki, kind, name = anchor(der)
        if kind is None:
            skipped += 1
            continue
        anchors.append((subject, spki, kind, name))

    out = []
    out.append("//! Trust anchors of the web client -- GENERATED, do not edit.")
    out.append("//!")
    out.append("//! Produced by `tools/generer-racines.py` (`make racines`) from the CA")
    out.append("//! bundle of the build machine: %d anchors, %d skipped for a key type" % (len(anchors), skipped))
    out.append("//! the client cannot verify. Each record is `u16 length, subject name`")
    out.append("//! then `u16 length, SubjectPublicKeyInfo`, both DER.")
    out.append("")
    out.append("pub const COUNT: usize = %d;" % len(anchors))
    out.append("")
    out.append("pub static DATA: &[u8] = &[")
    for subject, spki, kind, name in anchors:
        safe = "".join(c if 32 <= ord(c) < 127 else "?" for c in name)
        out.append("    // %s (%s)" % (safe, kind))
        record = len(subject).to_bytes(2, "big") + subject + len(spki).to_bytes(2, "big") + spki
        for index in range(0, len(record), 16):
            out.append("    " + " ".join("0x%02x," % byte for byte in record[index : index + 16]))
    out.append("];")
    out.append("")
    open(OUTPUT, "w", encoding="ascii").write("\n".join(out))
    print("[ok] %d racines ecrites dans %s (%d ignorees)" % (len(anchors), OUTPUT, skipped))


main()
