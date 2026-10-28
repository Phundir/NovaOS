//! Published test vectors, run at boot.
//!
//! Each algorithm is checked against the vectors of its own standard: a
//! cipher that is wrong in one bit encrypts and decrypts itself perfectly
//! and talks to nobody else. These are the vectors that caught nothing on
//! the development machine -- where the same files were also checked
//! against OpenSSL on generated keys -- and they are kept here so that a
//! regression of the kernel build (an optimisation, a target change) is
//! seen at boot rather than as a TLS handshake that fails for no reason.

use alloc::vec::Vec;

use super::sha2::{Hash, Sha256, Sha384, Sha512};
use super::{aes, chacha, ecc, hmac, x25519};

fn hex(text: &str) -> Vec<u8> {
    let digits: Vec<u8> = text
        .bytes()
        .filter(|b| b.is_ascii_hexdigit())
        .map(|b| (b as char).to_digit(16).unwrap_or(0) as u8)
        .collect();
    digits.chunks(2).map(|pair| (pair[0] << 4) | pair[1]).collect()
}

struct Report {
    passed: usize,
    failed: usize,
}

impl Report {
    fn check(&mut self, label: &str, condition: bool) {
        if condition {
            self.passed += 1;
        } else {
            self.failed += 1;
            crate::serial_println!("[crypto] ECHEC : {}", label);
        }
    }
}

/// Runs the vectors. Returns (passed, failed).
pub fn run() -> (usize, usize) {
    let mut report = Report { passed: 0, failed: 0 };

    report.check(
        "SHA-256 de \"abc\" (FIPS 180-4)",
        Sha256::digest(b"abc")[..32] == hex("ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad")[..],
    );
    report.check(
        "SHA-384 de \"abc\"",
        Sha384::digest(b"abc")[..48]
            == hex("cb00753f45a35e8bb5a03d699ac65007272c32ab0eded1631a8b605a43ff5bed8086072ba1e7cc2358baeca134c825a7")[..],
    );
    report.check(
        "SHA-512 de \"abc\"",
        Sha512::digest(b"abc")[..64]
            == hex("ddaf35a193617abacc417349ae20413112e6fa4e89a97ea20a9eeee64b55d39a2192992a274fc1a836ba3c23a3feebbd454d4423643ce80e2a9ac94fa54ca49f")[..],
    );
    let mut incremental = Sha256::new();
    for chunk in [&b"ab"[..], &b"c"[..]] {
        incremental.update(chunk);
    }
    report.check("SHA-256 incremental = SHA-256 d'un bloc", incremental.finish()[..32] == Sha256::digest(b"abc")[..32]);

    report.check(
        "HMAC-SHA-256, RFC 4231 cas 2",
        hmac::Hmac::<Sha256>::mac(b"Jefe", b"what do ya want for nothing?")
            == hex("5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"),
    );
    let prk = hmac::extract::<Sha256>(&hex("000102030405060708090a0b0c"), &[0x0b; 22]);
    report.check(
        "HKDF-Extract, RFC 5869 cas 1",
        prk == hex("077709362c2e32df0ddc3f0dc47bba6390b6c73bb50f9c3122ec844ad7c2b3e5"),
    );
    report.check(
        "HKDF-Expand, RFC 5869 cas 1",
        hmac::expand::<Sha256>(&prk, &hex("f0f1f2f3f4f5f6f7f8f9"), 42)
            == hex("3cb25f25faacd57a90434f64d0362f2a2d2d0a90cf1a5a4c5db02d56ecc4c5bf34007208d5b887185865"),
    );

    let mut block: [u8; 16] = [0; 16];
    block.copy_from_slice(&hex("00112233445566778899aabbccddeeff"));
    if let Some(cipher) = aes::Aes::new(&hex("000102030405060708090a0b0c0d0e0f")) {
        cipher.encrypt_block(&mut block);
    }
    report.check("AES-128, FIPS 197 annexe C.1", block[..] == hex("69c4e0d86a7b0430d8cdb78070b4c55a")[..]);
    block.copy_from_slice(&hex("00112233445566778899aabbccddeeff"));
    if let Some(cipher) = aes::Aes::new(&hex("000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f")) {
        cipher.encrypt_block(&mut block);
    }
    report.check("AES-256, FIPS 197 annexe C.3", block[..] == hex("8ea2b7ca516745bfeafc49904b496089")[..]);

    let gcm = aes::AesGcm::new(&hex("feffe9928665731c6d6a8f9467308308"));
    let mut nonce = [0u8; 12];
    nonce.copy_from_slice(&hex("cafebabefacedbaddecaf888"));
    let plaintext = hex("d9313225f88406e5a55909c5aff5269a86a7a9531534f7da2e4c303d8a318a721c3c0c95956809532fcf0e2449a6b525b16aedf5aa0de657ba637b39");
    let aad = hex("feedfacedeadbeeffeedfacedeadbeefabaddad2");
    if let Some(gcm) = gcm {
        let sealed = gcm.seal(&nonce, &aad, &plaintext);
        report.check("AES-GCM, cas 4 de McGrew et Viega : etiquette", sealed[sealed.len() - 16..] == hex("5bc94fbc3221a5db94fae95ae7121a47")[..]);
        report.check("AES-GCM : ouverture", gcm.open(&nonce, &aad, &sealed).as_deref() == Some(&plaintext[..]));
        let mut forged = sealed.clone();
        forged[0] ^= 1;
        report.check("AES-GCM : un octet change est refuse", gcm.open(&nonce, &aad, &forged).is_none());
    } else {
        report.check("AES-GCM : cle acceptee", false);
    }

    let mut key = [0u8; 32];
    key.copy_from_slice(&hex("85d6be7857556d337f4452fe42d506a80103808afb0db2fd4abff6af4149f51b"));
    report.check(
        "Poly1305, RFC 8439 2.5.2",
        chacha::poly1305(&key, b"Cryptographic Forum Research Group")[..] == hex("a8061dc1305136c6c22b8baf0c0127a9")[..],
    );
    key.copy_from_slice(&hex("808182838485868788898a8b8c8d8e8f909192939495969798999a9b9c9d9e9f"));
    nonce.copy_from_slice(&hex("070000004041424344454647"));
    let sealed = chacha::seal(
        &key,
        &nonce,
        &hex("50515253c0c1c2c3c4c5c6c7"),
        b"Ladies and Gentlemen of the class of '99: If I could offer you only one tip for the future, sunscreen would be it.",
    );
    report.check(
        "ChaCha20-Poly1305, RFC 8439 2.8.2",
        sealed[sealed.len() - 16..] == hex("1ae10b594f09e26a7e902ecbd0600691")[..],
    );

    let mut alice = [0u8; 32];
    alice.copy_from_slice(&hex("77076d0a7318a57d3c16c17251b26645df4c2f87ebc0992ab177fba51db92c2a"));
    let mut bob = [0u8; 32];
    bob.copy_from_slice(&hex("5dab087e624a8a4b79e17f8b83800ee66f3bb1292618b6fd1c2f8b27ff88e0eb"));
    report.check(
        "X25519, RFC 7748 6.1 : cle publique",
        x25519::public_key(&alice)[..] == hex("8520f0098930a754748b7ddcb43ef75a0dbf3a0d26381af4eba4a98eaa9b4e6a")[..],
    );
    report.check(
        "X25519 : secret partage",
        x25519::scalarmult(&alice, &x25519::public_key(&bob))[..]
            == hex("4a5d9d5ba4ce2de1728e3bf480350f25e07e21c947d19e3376f09b3c1e161742")[..],
    );

    // P-256: 1 x G is G, and ECDH is symmetric.
    let curve = ecc::Curve::p256();
    let mut one = [0u8; 32];
    one[31] = 1;
    report.check(
        "P-256 : 1 x G = G",
        curve.public_key(&one).is_some_and(|point| {
            point[1..33] == hex("6b17d1f2e12c4247f8bce6e563a440f277037d812deb33a0f4a13945d898c296")[..]
        }),
    );
    let first = curve.private_from(&[0x11; 48]);
    let second = curve.private_from(&[0x5a; 48]);
    let shared = (
        curve.public_key(&second).and_then(|public| curve.shared_secret(&first, &public)),
        curve.public_key(&first).and_then(|public| curve.shared_secret(&second, &public)),
    );
    report.check(
        "P-256 : l'echange de cles est symetrique",
        matches!(&shared, (Some(a), Some(b)) if a == b),
    );

    (report.passed, report.failed)
}
