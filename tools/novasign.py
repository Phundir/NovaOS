#!/usr/bin/env python3
"""Nova OS signing tool: Ed25519 keys, ENTERPRISE signatures, registry
manifests (phase 15).

    tools/novasign.py keygen <seed-file>            # new authority key
    tools/novasign.py pubkey <seed-file>            # its public key (hex)
    tools/novasign.py sign-manifest <seed-file> <manifest.json> [-o out.json]
    tools/novasign.py verify-manifest <manifest.json> [--key HEX]

A seed file holds the 32-byte secret of an authority, in hexadecimal. It
never goes on a Nova volume: the volume only learns the public key, one
line `name key-hex` in /sys/config/autorites.cfg (ENTERPRISE binaries,
packages and modules) or the `cle` of a registry (manifests).

The Ed25519 here is the reference algorithm of RFC 8032, in pure Python
(no dependency), checked at import against the RFC's first vector. The
messages signed are those of src/binfmt/signature.rs: a domain tag, then
the object -- "NOVA-REG\\0" followed by the canonical JSON of a manifest
without its "signature" member (compact, members sorted, the same bytes
as src/json.rs `canonical`).
"""
import argparse
import hashlib
import json
import os
import sys

# ------------------------------------------------------------------
#  Ed25519 (RFC 8032, section 5.1), reference arithmetic
# ------------------------------------------------------------------

P = 2 ** 255 - 19
L = 2 ** 252 + 27742317777372353535851937790883648493
D = -121665 * pow(121666, P - 2, P) % P
SQRT_M1 = pow(2, (P - 1) // 4, P)


def _add(a, b):
    x1, y1, z1, t1 = a
    x2, y2, z2, t2 = b
    A = (y1 - x1) * (y2 - x2) % P
    B = (y1 + x1) * (y2 + x2) % P
    C = t1 * 2 * D * t2 % P
    Dd = z1 * 2 * z2 % P
    E, F, G, H = B - A, Dd - C, Dd + C, B + A
    return (E * F % P, G * H % P, F * G % P, E * H % P)


def _mul(s, point):
    q = (0, 1, 1, 0)
    while s > 0:
        if s & 1:
            q = _add(q, point)
        point = _add(point, point)
        s >>= 1
    return q


def _encode(point):
    x, y, z, _ = point
    zi = pow(z, P - 2, P)
    x, y = x * zi % P, y * zi % P
    return int.to_bytes(y | ((x & 1) << 255), 32, "little")


def _recover_x(y, sign):
    if y >= P:
        return None
    x2 = (y * y - 1) * pow(D * y * y + 1, P - 2, P)
    if x2 == 0:
        return None if sign else 0
    x = pow(x2, (P + 3) // 8, P)
    if (x * x - x2) % P != 0:
        x = x * SQRT_M1 % P
    if (x * x - x2) % P != 0:
        return None
    if (x & 1) != sign:
        x = P - x
    return x


def _decode(data):
    y = int.from_bytes(data, "little")
    sign = y >> 255
    y &= (1 << 255) - 1
    x = _recover_x(y, sign)
    if x is None:
        return None
    return (x, y, 1, x * y % P)


_GY = 4 * pow(5, P - 2, P) % P
BASE = (_recover_x(_GY, 0), _GY, 1, _recover_x(_GY, 0) * _GY % P)


def _h(data):
    return int.from_bytes(hashlib.sha512(data).digest(), "little")


def _expand(seed):
    digest = hashlib.sha512(seed).digest()
    a = int.from_bytes(digest[:32], "little")
    a &= (1 << 254) - 8
    a |= 1 << 254
    return a, digest[32:]


def public_key(seed):
    a, _ = _expand(seed)
    return _encode(_mul(a, BASE))


def sign(seed, message):
    a, prefix = _expand(seed)
    key = _encode(_mul(a, BASE))
    r = _h(prefix + message) % L
    R = _encode(_mul(r, BASE))
    k = _h(R + key + message) % L
    s = (r + k * a) % L
    return R + int.to_bytes(s, 32, "little")


def verify(key, message, signature):
    if len(key) != 32 or len(signature) != 64:
        return False
    A = _decode(key)
    R = _decode(signature[:32])
    if A is None or R is None:
        return False
    s = int.from_bytes(signature[32:], "little")
    if s >= L:
        return False
    k = _h(signature[:32] + key + message) % L
    left = _encode(_mul(s, BASE))
    right = _encode(_add(R, _mul(k, A)))
    return left == right


_VECTOR_SEED = bytes.fromhex("9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60")
assert public_key(_VECTOR_SEED).hex() == "d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a"
assert sign(_VECTOR_SEED, b"").hex().startswith("e5564300c360ac729086e2cc806e828a")

# ------------------------------------------------------------------
#  Domains (src/binfmt/signature.rs, `domain`)
# ------------------------------------------------------------------

DOMAIN_BINARY = b"NOVA-BIN\0"
DOMAIN_PACKAGE = b"NOVA-SPM\0"
DOMAIN_MODULE = b"NOVA-KMD\0"
DOMAIN_REGISTRY = b"NOVA-REG\0"


def canonical(value):
    """The canonical JSON of src/json.rs: compact, members sorted by
    their UTF-8 bytes (the same order as code points), no float."""
    def check(item):
        if isinstance(item, float):
            sys.exit("manifeste : nombre a virgule refuse (ecrire la valeur en chaine)")
        if isinstance(item, dict):
            for child in item.values():
                check(child)
        if isinstance(item, list):
            for child in item:
                check(child)
    check(value)
    return json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=False).encode("utf-8")


def manifest_message(manifest):
    unsigned = {key: value for key, value in manifest.items() if key != "signature"}
    return DOMAIN_REGISTRY + canonical(unsigned)


def read_seed(path):
    text = open(path).read().strip()
    seed = bytes.fromhex(text)
    if len(seed) != 32:
        sys.exit("%s : 64 chiffres hexadecimaux attendus" % path)
    return seed


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    sub = parser.add_subparsers(dest="command", required=True)
    keygen = sub.add_parser("keygen")
    keygen.add_argument("seed")
    pubkey = sub.add_parser("pubkey")
    pubkey.add_argument("seed")
    signer = sub.add_parser("sign-manifest")
    signer.add_argument("seed")
    signer.add_argument("manifest")
    signer.add_argument("-o", "--output")
    checker = sub.add_parser("verify-manifest")
    checker.add_argument("manifest")
    checker.add_argument("--key")
    args = parser.parse_args()

    if args.command == "keygen":
        if os.path.exists(args.seed):
            sys.exit("%s existe deja : une cle ne s'ecrase pas" % args.seed)
        seed = os.urandom(32)
        with open(os.open(args.seed, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600), "w") as out:
            out.write(seed.hex() + "\n")
        print(public_key(seed).hex())
    elif args.command == "pubkey":
        print(public_key(read_seed(args.seed)).hex())
    elif args.command == "sign-manifest":
        seed = read_seed(args.seed)
        manifest = json.load(open(args.manifest, encoding="utf-8"))
        manifest["signature"] = {
            "cle": public_key(seed).hex(),
            "ed25519": sign(seed, manifest_message(manifest)).hex(),
        }
        text = json.dumps(manifest, indent=2, ensure_ascii=False) + "\n"
        open(args.output or args.manifest, "w", encoding="utf-8").write(text)
    elif args.command == "verify-manifest":
        manifest = json.load(open(args.manifest, encoding="utf-8"))
        signature = manifest.get("signature") or {}
        key = bytes.fromhex(args.key or signature.get("cle", ""))
        ok = verify(key, manifest_message(manifest), bytes.fromhex(signature.get("ed25519", "")))
        print("signature valide" if ok else "signature INVALIDE")
        sys.exit(0 if ok else 1)


if __name__ == "__main__":
    main()
