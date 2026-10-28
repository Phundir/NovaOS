//! A TLS client: 1.3 (RFC 8446), and the modern subset of 1.2 (RFC 5246).
//!
//! ## Why 1.2 at all, and which part of it
//!
//! TLS 1.3 is the protocol every large site speaks, and the one this file
//! was first written for. But some sites still answer 1.2 only -- the
//! national press among them -- and a browser that cannot open them is
//! not much of one. So 1.2 is accepted, **restricted to what 1.3 kept**:
//! ephemeral elliptic-curve key exchange (ECDHE) and AEAD records
//! (AES-GCM, ChaCha20-Poly1305), with the extended master secret of
//! RFC 7627 when the server offers it. No RSA key transport, no CBC, no
//! renegotiation, no compression: the constructions 1.3 removed for cause
//! stay removed.
//!
//! A 1.3 server that is made to answer 1.2 marks its random with
//! `DOWNGRD` (RFC 8446, 4.1.3); that mark is checked, so an attacker in
//! the middle cannot force the older protocol on a server that speaks the
//! newer.
//!
//! ## What is offered
//!
//! * TLS 1.3 suites `TLS_AES_128_GCM_SHA256`,
//!   `TLS_CHACHA20_POLY1305_SHA256`, `TLS_AES_256_GCM_SHA384`;
//! * TLS 1.2 suites `ECDHE_{ECDSA,RSA}_WITH_AES_{128,256}_GCM` and
//!   `ECDHE_{ECDSA,RSA}_WITH_CHACHA20_POLY1305`;
//! * key exchange on X25519, or P-256 / P-384 when a server asks for it;
//! * signatures ECDSA P-256/P-384, RSA-PSS, and RSA PKCS #1 v1.5 where
//!   the protocol allows it (certificates, and the 1.2 key exchange);
//! * ALPN `http/1.1`, since the HTTP client speaks nothing else.
//!
//! No session resumption, no early data, no client certificate (an
//! empty one is sent if a server asks).
//!
//! ## The transport
//!
//! Anything that sends and receives bytes: the kernel's TCP connection,
//! or a socket of the development machine when this file is checked
//! there. `Transport` is the whole contract.

use alloc::collections::VecDeque;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use super::x509::{self, Anchor, Certificate, Key, Revocation, Trust};
use crate::crypto::aes::AesGcm;
use crate::crypto::chacha;
use crate::crypto::ecc::Curve;
use crate::crypto::hmac::{self, Hmac};
use crate::crypto::rsa::Digest;
use crate::crypto::sha2::{Hash, Sha256, Sha384};
use crate::crypto::x25519;

/// What carries the bytes.
pub trait Transport {
    fn send(&mut self, data: &[u8]) -> Result<(), &'static str>;
    /// Up to `max` bytes, waiting for at least one. Empty at end of stream.
    fn recv(&mut self, max: usize) -> Result<Vec<u8>, &'static str>;
}

const CONTENT_CHANGE_CIPHER_SPEC: u8 = 20;
const CONTENT_ALERT: u8 = 21;
const CONTENT_HANDSHAKE: u8 = 22;
const CONTENT_APPLICATION: u8 = 23;

const CLIENT_HELLO: u8 = 1;
const SERVER_HELLO: u8 = 2;
const NEW_SESSION_TICKET: u8 = 4;
const ENCRYPTED_EXTENSIONS: u8 = 8;
const CERTIFICATE: u8 = 11;
const SERVER_KEY_EXCHANGE: u8 = 12;
const CERTIFICATE_REQUEST: u8 = 13;
const SERVER_HELLO_DONE: u8 = 14;
const CERTIFICATE_VERIFY: u8 = 15;
const CERTIFICATE_STATUS: u8 = 22;
const CLIENT_KEY_EXCHANGE: u8 = 16;
const FINISHED: u8 = 20;
const KEY_UPDATE: u8 = 24;
const MESSAGE_HASH: u8 = 254;

const GROUP_X25519: u16 = 0x001d;
const GROUP_P256: u16 = 0x0017;
const GROUP_P384: u16 = 0x0018;

/// The fixed random of a HelloRetryRequest: SHA-256 of that name.
const HRR_RANDOM: [u8; 32] = [
    0xcf, 0x21, 0xad, 0x74, 0xe5, 0x9a, 0x61, 0x11, 0xbe, 0x1d, 0x8c, 0x02, 0x1e, 0x65, 0xb8, 0x91,
    0xc2, 0xa2, 0x11, 0x16, 0x7a, 0xbb, 0x8c, 0x5e, 0x07, 0x9e, 0x09, 0xe2, 0xc8, 0xa8, 0x33, 0x9c,
];

/// The end of a server random when a 1.3 server was pushed to 1.2.
const DOWNGRADE_12: [u8; 8] = *b"DOWNGRD\x01";

/// Largest plaintext of a record.
const MAX_FRAGMENT: usize = 16384;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HashAlg {
    Sha256,
    Sha384,
}

impl HashAlg {
    fn len(self) -> usize {
        match self {
            HashAlg::Sha256 => 32,
            HashAlg::Sha384 => 48,
        }
    }

    fn hash(self, data: &[u8]) -> Vec<u8> {
        match self {
            HashAlg::Sha256 => Sha256::digest(data)[..32].to_vec(),
            HashAlg::Sha384 => Sha384::digest(data)[..48].to_vec(),
        }
    }

    fn hmac(self, key: &[u8], data: &[u8]) -> Vec<u8> {
        match self {
            HashAlg::Sha256 => Hmac::<Sha256>::mac(key, data),
            HashAlg::Sha384 => Hmac::<Sha384>::mac(key, data),
        }
    }

    fn extract(self, salt: &[u8], ikm: &[u8]) -> Vec<u8> {
        match self {
            HashAlg::Sha256 => hmac::extract::<Sha256>(salt, ikm),
            HashAlg::Sha384 => hmac::extract::<Sha384>(salt, ikm),
        }
    }

    fn expand_label(self, secret: &[u8], label: &str, context: &[u8], length: usize) -> Vec<u8> {
        match self {
            HashAlg::Sha256 => hmac::expand_label::<Sha256>(secret, label, context, length),
            HashAlg::Sha384 => hmac::expand_label::<Sha384>(secret, label, context, length),
        }
    }

    fn derive_secret(self, secret: &[u8], label: &str, transcript: &[u8]) -> Vec<u8> {
        self.expand_label(secret, label, &self.hash(transcript), self.len())
    }

    /// The PRF of TLS 1.2 (RFC 5246, 5): P_hash over `label || seed`.
    fn prf(self, secret: &[u8], label: &str, seed: &[u8], length: usize) -> Vec<u8> {
        let mut label_seed = label.as_bytes().to_vec();
        label_seed.extend_from_slice(seed);
        let mut out = Vec::with_capacity(length + 48);
        let mut a = self.hmac(secret, &label_seed);
        while out.len() < length {
            let mut input = a.clone();
            input.extend_from_slice(&label_seed);
            out.extend_from_slice(&self.hmac(secret, &input));
            a = self.hmac(secret, &a);
        }
        out.truncate(length);
        out
    }
}

/// The record protection of a suite.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Aead {
    Aes128,
    Aes256,
    Chacha,
}

impl Aead {
    fn key_len(self) -> usize {
        match self {
            Aead::Aes128 => 16,
            _ => 32,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Suite {
    code: u16,
    aead: Aead,
    hash: HashAlg,
    /// A TLS 1.2 suite (ECDHE key exchange), not a 1.3 one.
    legacy: bool,
}

/// Offered in this order: ChaCha20-Poly1305 first. In software it is
/// almost three times as fast as AES-GCM (680 against 240 MB/s measured on
/// the development machine), and the servers that share a client's
/// preference among suites of equal strength -- the large CDNs do --
/// then pick it.
const SUITES: [Suite; 9] = [
    Suite { code: 0x1303, aead: Aead::Chacha, hash: HashAlg::Sha256, legacy: false },
    Suite { code: 0x1301, aead: Aead::Aes128, hash: HashAlg::Sha256, legacy: false },
    Suite { code: 0x1302, aead: Aead::Aes256, hash: HashAlg::Sha384, legacy: false },
    Suite { code: 0xcca9, aead: Aead::Chacha, hash: HashAlg::Sha256, legacy: true },
    Suite { code: 0xcca8, aead: Aead::Chacha, hash: HashAlg::Sha256, legacy: true },
    Suite { code: 0xc02b, aead: Aead::Aes128, hash: HashAlg::Sha256, legacy: true },
    Suite { code: 0xc02f, aead: Aead::Aes128, hash: HashAlg::Sha256, legacy: true },
    Suite { code: 0xc02c, aead: Aead::Aes256, hash: HashAlg::Sha384, legacy: true },
    Suite { code: 0xc030, aead: Aead::Aes256, hash: HashAlg::Sha384, legacy: true },
];

impl Suite {
    fn from_code(code: u16) -> Option<Suite> {
        SUITES.iter().copied().find(|suite| suite.code == code)
    }

    fn name(self) -> &'static str {
        match self.code {
            0x1301 => "TLS 1.3 AES_128_GCM_SHA256",
            0x1302 => "TLS 1.3 AES_256_GCM_SHA384",
            0x1303 => "TLS 1.3 CHACHA20_POLY1305_SHA256",
            0xc02b => "TLS 1.2 ECDHE_ECDSA_AES_128_GCM_SHA256",
            0xc02f => "TLS 1.2 ECDHE_RSA_AES_128_GCM_SHA256",
            0xc02c => "TLS 1.2 ECDHE_ECDSA_AES_256_GCM_SHA384",
            0xc030 => "TLS 1.2 ECDHE_RSA_AES_256_GCM_SHA384",
            0xcca9 => "TLS 1.2 ECDHE_ECDSA_CHACHA20_POLY1305",
            _ => "TLS 1.2 ECDHE_RSA_CHACHA20_POLY1305",
        }
    }
}

enum Cipher {
    Aes(AesGcm),
    Chacha([u8; 32]),
}

impl Cipher {
    fn new(aead: Aead, key: &[u8]) -> Cipher {
        match aead {
            Aead::Chacha => {
                let mut bytes = [0u8; 32];
                bytes.copy_from_slice(key);
                Cipher::Chacha(bytes)
            }
            _ => Cipher::Aes(AesGcm::new(key).expect("key length of the suite")),
        }
    }

    fn seal(&self, nonce: &[u8; 12], aad: &[u8], data: &[u8]) -> Vec<u8> {
        match self {
            Cipher::Aes(gcm) => gcm.seal(nonce, aad, data),
            Cipher::Chacha(key) => chacha::seal(key, nonce, aad, data),
        }
    }

    fn open(&self, nonce: &[u8; 12], aad: &[u8], data: &[u8]) -> Option<Vec<u8>> {
        match self {
            Cipher::Aes(gcm) => gcm.open(nonce, aad, data),
            Cipher::Chacha(key) => chacha::open(key, nonce, aad, data),
        }
    }
}

/// How records are framed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Framing {
    /// Outer type 23, inner type after the content, IV xor sequence.
    Tls13,
    /// TLS 1.2 GCM: four-byte implicit salt, eight-byte explicit nonce.
    Tls12Gcm,
    /// TLS 1.2 ChaCha20 (RFC 7905): IV xor sequence, no explicit nonce.
    Tls12Chacha,
}

/// One direction's keys.
struct Direction {
    cipher: Cipher,
    iv: [u8; 12],
    sequence: u64,
    framing: Framing,
    /// The 1.3 traffic secret, for key updates.
    secret: Vec<u8>,
}

impl Direction {
    fn tls13(suite: Suite, secret: Vec<u8>) -> Direction {
        let hash = suite.hash;
        let key = hash.expand_label(&secret, "key", &[], suite.aead.key_len());
        let iv_bytes = hash.expand_label(&secret, "iv", &[], 12);
        let mut iv = [0u8; 12];
        iv.copy_from_slice(&iv_bytes);
        Direction {
            cipher: Cipher::new(suite.aead, &key),
            iv,
            sequence: 0,
            framing: Framing::Tls13,
            secret,
        }
    }

    fn tls12(suite: Suite, key: &[u8], iv_bytes: &[u8]) -> Direction {
        let mut iv = [0u8; 12];
        iv[..iv_bytes.len()].copy_from_slice(iv_bytes);
        Direction {
            cipher: Cipher::new(suite.aead, key),
            iv,
            sequence: 0,
            framing: if suite.aead == Aead::Chacha { Framing::Tls12Chacha } else { Framing::Tls12Gcm },
            secret: Vec::new(),
        }
    }

    fn xor_nonce(&self) -> [u8; 12] {
        let mut nonce = self.iv;
        for (slot, byte) in nonce[4..].iter_mut().zip(self.sequence.to_be_bytes()) {
            *slot ^= byte;
        }
        nonce
    }

    /// The additional data of a 1.2 record.
    fn aad12(&self, content_type: u8, length: usize) -> [u8; 13] {
        let mut aad = [0u8; 13];
        aad[..8].copy_from_slice(&self.sequence.to_be_bytes());
        aad[8] = content_type;
        aad[9] = 3;
        aad[10] = 3;
        aad[11] = (length >> 8) as u8;
        aad[12] = length as u8;
        aad
    }

    fn seal(&mut self, content_type: u8, data: &[u8]) -> Vec<u8> {
        let record = match self.framing {
            Framing::Tls13 => {
                let mut inner = Vec::with_capacity(data.len() + 1);
                inner.extend_from_slice(data);
                inner.push(content_type);
                let length = inner.len() + 16;
                let header = [CONTENT_APPLICATION, 3, 3, (length >> 8) as u8, length as u8];
                let sealed = self.cipher.seal(&self.xor_nonce(), &header, &inner);
                let mut record = header.to_vec();
                record.extend_from_slice(&sealed);
                record
            }
            Framing::Tls12Gcm => {
                let explicit = self.sequence.to_be_bytes();
                let mut nonce = [0u8; 12];
                nonce[..4].copy_from_slice(&self.iv[..4]);
                nonce[4..].copy_from_slice(&explicit);
                let sealed = self.cipher.seal(&nonce, &self.aad12(content_type, data.len()), data);
                let length = 8 + sealed.len();
                let mut record = vec![content_type, 3, 3, (length >> 8) as u8, length as u8];
                record.extend_from_slice(&explicit);
                record.extend_from_slice(&sealed);
                record
            }
            Framing::Tls12Chacha => {
                let sealed = self.cipher.seal(&self.xor_nonce(), &self.aad12(content_type, data.len()), data);
                let length = sealed.len();
                let mut record = vec![content_type, 3, 3, (length >> 8) as u8, length as u8];
                record.extend_from_slice(&sealed);
                record
            }
        };
        self.sequence += 1;
        record
    }

    fn open(&mut self, header: &[u8], body: &[u8]) -> Option<(u8, Vec<u8>)> {
        let opened = match self.framing {
            Framing::Tls13 => {
                let mut inner = self.cipher.open(&self.xor_nonce(), header, body)?;
                // The content type is the last non-zero byte; zeros after
                // it are padding.
                while inner.last() == Some(&0) {
                    inner.pop();
                }
                let content_type = inner.pop()?;
                (content_type, inner)
            }
            Framing::Tls12Gcm => {
                if body.len() < 8 + 16 {
                    return None;
                }
                let mut nonce = [0u8; 12];
                nonce[..4].copy_from_slice(&self.iv[..4]);
                nonce[4..].copy_from_slice(&body[..8]);
                let aad = self.aad12(header[0], body.len() - 8 - 16);
                (header[0], self.cipher.open(&nonce, &aad, &body[8..])?)
            }
            Framing::Tls12Chacha => {
                if body.len() < 16 {
                    return None;
                }
                let aad = self.aad12(header[0], body.len() - 16);
                (header[0], self.cipher.open(&self.xor_nonce(), &aad, body)?)
            }
        };
        self.sequence += 1;
        Some(opened)
    }
}

/// What a finished handshake tells about the connection.
#[derive(Debug, Clone)]
pub struct Session {
    pub suite: &'static str,
    pub group: &'static str,
    pub subject: String,
    pub issuer_verified: bool,
    /// The verified chain, leaf first: what the certificate viewer shows.
    pub chain: Vec<Certificate>,
    /// The trust anchor the chain ends on.
    pub anchor: String,
    /// Accepted by an exception the user made for this site.
    pub exception: bool,
    pub revocation: Revocation,
}

/// Why a connection failed: the message for the user, and, when the
/// certificate was the reason, what the server presented -- so that the
/// user can look at it and, knowingly, accept it for this site.
#[derive(Debug, Clone, Default)]
pub struct Failure {
    pub message: String,
    pub certificates: Vec<Certificate>,
    pub host: String,
}

impl From<String> for Failure {
    fn from(message: String) -> Failure {
        Failure {
            message,
            ..Failure::default()
        }
    }
}

impl From<&str> for Failure {
    fn from(message: &str) -> Failure {
        Failure::from(String::from(message))
    }
}

impl core::fmt::Display for Failure {
    fn fmt(&self, formatter: &mut core::fmt::Formatter) -> core::fmt::Result {
        write!(formatter, "{}", self.message)
    }
}

fn rejected(error: x509::ChainError, host: &str) -> Failure {
    Failure {
        message: String::from(error.reason),
        certificates: error.certificates,
        host: String::from(host),
    }
}

/// An established connection.
pub struct Tls<T: Transport> {
    io: T,
    incoming: Vec<u8>,
    handshake: Vec<u8>,
    read: Option<Direction>,
    /// 1.2: the keys the server switches to at its ChangeCipherSpec.
    pending_read: Option<Direction>,
    write: Option<Direction>,
    suite: Suite,
    plaintext: VecDeque<u8>,
    closed: bool,
    pub session: Session,
}

/// Why a connection failed, for the user.
pub type Error = String;

fn fail(text: &str) -> Error {
    String::from(text)
}

fn alert_text(description: u8) -> &'static str {
    match description {
        40 => "le serveur refuse la negociation (handshake_failure)",
        42..=46 => "le serveur refuse le certificat",
        47 => "parametre refuse par le serveur",
        50 => "message mal forme selon le serveur",
        51 => "le serveur n'a pas pu dechiffrer",
        70 => "version de TLS refusee par le serveur",
        71 => "securite insuffisante selon le serveur",
        80 => "erreur interne du serveur",
        109 => "extension manquante selon le serveur",
        112 => "nom de site inconnu du serveur",
        120 => "aucun protocole applicatif commun",
        _ => "alerte TLS du serveur",
    }
}

/// Appends `value` with a big-endian length prefix of `width` bytes.
fn put_vector(out: &mut Vec<u8>, width: usize, value: &[u8]) {
    let length = value.len();
    for index in (0..width).rev() {
        out.push((length >> (index * 8)) as u8);
    }
    out.extend_from_slice(value);
}

fn put_extension(out: &mut Vec<u8>, kind: u16, body: &[u8]) {
    out.extend_from_slice(&kind.to_be_bytes());
    put_vector(out, 2, body);
}

/// A byte cursor over a received message.
struct Cursor<'a> {
    data: &'a [u8],
}

impl<'a> Cursor<'a> {
    fn u8(&mut self) -> Option<u8> {
        let (first, rest) = self.data.split_first()?;
        self.data = rest;
        Some(*first)
    }

    fn u16(&mut self) -> Option<u16> {
        Some(((self.u8()? as u16) << 8) | self.u8()? as u16)
    }

    fn bytes(&mut self, count: usize) -> Option<&'a [u8]> {
        if self.data.len() < count {
            return None;
        }
        let (head, rest) = self.data.split_at(count);
        self.data = rest;
        Some(head)
    }

    fn vector(&mut self, width: usize) -> Option<&'a [u8]> {
        let mut length = 0usize;
        for _ in 0..width {
            length = (length << 8) | self.u8()? as usize;
        }
        self.bytes(length)
    }
}

enum KeyShare {
    X25519([u8; 32]),
    P256(Vec<u8>),
    P384(Vec<u8>),
}

impl KeyShare {
    fn group(&self) -> u16 {
        match self {
            KeyShare::X25519(_) => GROUP_X25519,
            KeyShare::P256(_) => GROUP_P256,
            KeyShare::P384(_) => GROUP_P384,
        }
    }

    fn public(&self) -> Vec<u8> {
        match self {
            KeyShare::X25519(private) => x25519::public_key(private).to_vec(),
            KeyShare::P256(private) => Curve::p256().public_key(private).unwrap_or_default(),
            KeyShare::P384(private) => Curve::p384().public_key(private).unwrap_or_default(),
        }
    }

    fn agree(&self, peer: &[u8]) -> Option<Vec<u8>> {
        match self {
            KeyShare::X25519(private) => {
                let peer: [u8; 32] = peer.try_into().ok()?;
                let shared = x25519::scalarmult(private, &peer);
                // An all-zero result means a small-order peer point.
                (shared != [0u8; 32]).then(|| shared.to_vec())
            }
            KeyShare::P256(private) => Curve::p256().shared_secret(private, peer),
            KeyShare::P384(private) => Curve::p384().shared_secret(private, peer),
        }
    }

    fn for_group(group: u16, random: &mut dyn FnMut(&mut [u8])) -> Option<KeyShare> {
        match group {
            GROUP_X25519 => {
                let mut secret = [0u8; 32];
                random(&mut secret);
                Some(KeyShare::X25519(secret))
            }
            GROUP_P256 => {
                let mut bytes = [0u8; 48];
                random(&mut bytes);
                Some(KeyShare::P256(Curve::p256().private_from(&bytes)))
            }
            GROUP_P384 => {
                let mut bytes = [0u8; 64];
                random(&mut bytes);
                Some(KeyShare::P384(Curve::p384().private_from(&bytes)))
            }
            _ => None,
        }
    }
}

fn client_hello(host: &str, random: &[u8; 32], session_id: &[u8; 32], share: &KeyShare, cookie: Option<&[u8]>) -> Vec<u8> {
    let mut body = Vec::new();
    body.extend_from_slice(&[3, 3]);
    body.extend_from_slice(random);
    put_vector(&mut body, 1, session_id);
    let mut suites = Vec::new();
    for suite in SUITES.iter() {
        suites.extend_from_slice(&suite.code.to_be_bytes());
    }
    put_vector(&mut body, 2, &suites);
    put_vector(&mut body, 1, &[0]);

    let mut extensions = Vec::new();
    // server_name
    let mut name = vec![0u8];
    put_vector(&mut name, 2, host.as_bytes());
    let mut list = Vec::new();
    put_vector(&mut list, 2, &name);
    put_extension(&mut extensions, 0x0000, &list);
    // supported_groups
    let mut groups = Vec::new();
    put_vector(&mut groups, 2, &[0x00, 0x1d, 0x00, 0x17, 0x00, 0x18]);
    put_extension(&mut extensions, 0x000a, &groups);
    // status_request: an OCSP response stapled by the server (RFC 6066).
    put_extension(&mut extensions, 0x0005, &[1, 0, 0, 0, 0]);
    // ec_point_formats: uncompressed only (1.2)
    put_extension(&mut extensions, 0x000b, &[1, 0]);
    // signature_algorithms
    let mut algorithms = Vec::new();
    put_vector(
        &mut algorithms,
        2,
        &[
            0x04, 0x03, 0x05, 0x03, 0x08, 0x04, 0x08, 0x05, 0x08, 0x06, 0x04, 0x01, 0x05, 0x01, 0x06, 0x01,
        ],
    );
    put_extension(&mut extensions, 0x000d, &algorithms);
    // application_layer_protocol_negotiation
    let mut protocols = Vec::new();
    put_vector(&mut protocols, 1, b"http/1.1");
    let mut alpn = Vec::new();
    put_vector(&mut alpn, 2, &protocols);
    put_extension(&mut extensions, 0x0010, &alpn);
    // extended_master_secret (1.2)
    put_extension(&mut extensions, 0x0017, &[]);
    // supported_versions: 1.3, then 1.2
    put_extension(&mut extensions, 0x002b, &[4, 3, 4, 3, 3]);
    // psk_key_exchange_modes: some servers want it even without a PSK.
    put_extension(&mut extensions, 0x002d, &[1, 1]);
    // cookie, echoed from a HelloRetryRequest
    if let Some(cookie) = cookie {
        let mut value = Vec::new();
        put_vector(&mut value, 2, cookie);
        put_extension(&mut extensions, 0x002c, &value);
    }
    // key_share
    let mut entry = share.group().to_be_bytes().to_vec();
    put_vector(&mut entry, 2, &share.public());
    let mut shares = Vec::new();
    put_vector(&mut shares, 2, &entry);
    put_extension(&mut extensions, 0x0033, &shares);
    // renegotiation_info, empty: "I do not renegotiate" (1.2)
    put_extension(&mut extensions, 0xff01, &[0]);

    put_vector(&mut body, 2, &extensions);
    let mut message = vec![CLIENT_HELLO];
    put_vector(&mut message, 3, &body);
    message
}

/// What a ServerHello (or HelloRetryRequest) says.
struct ServerHello {
    retry: bool,
    tls13: bool,
    random: [u8; 32],
    suite: Suite,
    group: u16,
    key: Vec<u8>,
    cookie: Option<Vec<u8>>,
    extended_master_secret: bool,
}

fn parse_server_hello(message: &[u8], session_id: &[u8; 32]) -> Result<ServerHello, Error> {
    let bad = || fail("ServerHello mal forme");
    let mut cursor = Cursor { data: &message[4..] };
    let legacy_version = cursor.u16().ok_or_else(bad)?;
    let random: [u8; 32] = cursor.bytes(32).ok_or_else(bad)?.try_into().map_err(|_| bad())?;
    let echoed = cursor.vector(1).ok_or_else(bad)?;
    let suite = Suite::from_code(cursor.u16().ok_or_else(bad)?).ok_or_else(|| fail("suite de chiffrement non proposee"))?;
    cursor.u8().ok_or_else(bad)?;
    let mut version = legacy_version;
    let mut group = 0u16;
    let mut key = Vec::new();
    let mut cookie = None;
    let mut extended_master_secret = false;
    let retry = random == HRR_RANDOM;
    if !cursor.data.is_empty() {
        let mut extensions = Cursor {
            data: cursor.vector(2).ok_or_else(bad)?,
        };
        while !extensions.data.is_empty() {
            let kind = extensions.u16().ok_or_else(bad)?;
            let mut body = Cursor {
                data: extensions.vector(2).ok_or_else(bad)?,
            };
            match kind {
                0x002b => version = body.u16().ok_or_else(bad)?,
                0x0033 => {
                    group = body.u16().ok_or_else(bad)?;
                    if !retry {
                        key = body.vector(2).ok_or_else(bad)?.to_vec();
                    }
                }
                0x002c => cookie = Some(body.vector(2).ok_or_else(bad)?.to_vec()),
                0x0017 => extended_master_secret = true,
                _ => {}
            }
        }
    }
    let tls13 = match version {
        0x0304 => true,
        0x0303 => false,
        _ => return Err(fail("version de TLS trop ancienne (seuls 1.2 et 1.3 sont acceptes)")),
    };
    if tls13 != !suite.legacy {
        return Err(fail("suite incoherente avec la version negociee"));
    }
    if tls13 && echoed != session_id {
        return Err(fail("le serveur n'a pas renvoye l'identifiant de session"));
    }
    if !tls13 && random[24..] == DOWNGRADE_12 {
        return Err(fail("tentative de retour force a TLS 1.2 detectee"));
    }
    Ok(ServerHello {
        retry,
        tls13,
        random,
        suite,
        group,
        key,
        cookie,
        extended_master_secret,
    })
}

fn group_name(group: u16) -> &'static str {
    match group {
        GROUP_X25519 => "X25519",
        GROUP_P256 => "P-256",
        _ => "P-384",
    }
}

/// The signature schemes of a CertificateVerify or ServerKeyExchange.
fn signature_scheme(algorithm: u16, allow_pkcs1: bool) -> Result<x509::Signature, Error> {
    Ok(match algorithm {
        0x0403 => x509::Signature::Ecdsa(Digest::Sha256),
        0x0503 => x509::Signature::Ecdsa(Digest::Sha384),
        0x0603 => x509::Signature::Ecdsa(Digest::Sha512),
        0x0804 | 0x0809 => x509::Signature::RsaPss(Digest::Sha256),
        0x0805 | 0x080a => x509::Signature::RsaPss(Digest::Sha384),
        0x0806 | 0x080b => x509::Signature::RsaPss(Digest::Sha512),
        0x0401 if allow_pkcs1 => x509::Signature::RsaPkcs1(Digest::Sha256),
        0x0501 if allow_pkcs1 => x509::Signature::RsaPkcs1(Digest::Sha384),
        0x0601 if allow_pkcs1 => x509::Signature::RsaPkcs1(Digest::Sha512),
        _ => return Err(fail("algorithme de signature non pris en charge")),
    })
}

impl<T: Transport> Tls<T> {
    /// Opens a TLS session to `host` over `io`.
    ///
    /// `now` is the time in seconds since 1970 for the certificate dates;
    /// `random` fills a buffer with unpredictable bytes.
    pub fn connect(
        io: T,
        host: &str,
        now: u64,
        random: &mut dyn FnMut(&mut [u8]),
        trust: &Trust,
    ) -> Result<Tls<T>, Failure> {
        let mut tls = Tls {
            io,
            incoming: Vec::new(),
            handshake: Vec::new(),
            read: None,
            pending_read: None,
            write: None,
            suite: SUITES[0],
            plaintext: VecDeque::new(),
            closed: false,
            session: Session {
                suite: "",
                group: "",
                subject: String::new(),
                issuer_verified: false,
                chain: Vec::new(),
                anchor: String::new(),
                exception: false,
                revocation: Revocation::NotStapled,
            },
        };

        let mut client_random = [0u8; 32];
        random(&mut client_random);
        let mut session_id = [0u8; 32];
        random(&mut session_id);
        let mut share = KeyShare::for_group(GROUP_X25519, random).expect("X25519 is known");

        let first_hello = client_hello(host, &client_random, &session_id, &share, None);
        tls.send_plain(CONTENT_HANDSHAKE, &first_hello, true)?;
        let mut transcript = first_hello;

        let mut server_message = tls.next_handshake()?;
        if server_message[0] != SERVER_HELLO {
            return Err(fail("reponse inattendue au ClientHello").into());
        }
        let mut hello = parse_server_hello(&server_message, &session_id)?;

        if hello.retry {
            if !hello.tls13 {
                return Err(fail("demande de nouvel essai hors TLS 1.3").into());
            }
            // RFC 8446, 4.4.1: the first ClientHello is replaced in the
            // transcript by a synthetic message carrying its hash.
            let hash = hello.suite.hash;
            let digest = hash.hash(&transcript);
            let mut synthetic = vec![MESSAGE_HASH, 0, 0, hash.len() as u8];
            synthetic.extend_from_slice(&digest);
            transcript = synthetic;
            transcript.extend_from_slice(&server_message);

            share = match hello.group {
                GROUP_P256 | GROUP_P384 => KeyShare::for_group(hello.group, random).expect("known group"),
                _ => return Err(fail("groupe d'echange de cles non pris en charge").into()),
            };
            let second_hello = client_hello(host, &client_random, &session_id, &share, hello.cookie.as_deref());
            tls.send_plain(CONTENT_HANDSHAKE, &second_hello, false)?;
            transcript.extend_from_slice(&second_hello);
            server_message = tls.next_handshake()?;
            if server_message[0] != SERVER_HELLO {
                return Err(fail("reponse inattendue au second ClientHello").into());
            }
            let suite = hello.suite;
            hello = parse_server_hello(&server_message, &session_id)?;
            if hello.retry || hello.suite != suite {
                return Err(fail("seconde demande de nouvel essai").into());
            }
        }
        transcript.extend_from_slice(&server_message);
        tls.suite = hello.suite;

        if hello.tls13 {
            tls.handshake13(host, now, trust, &hello, &share, transcript)?;
        } else {
            tls.handshake12(host, now, trust, &hello, &client_random, random, transcript)?;
        }
        tls.session.suite = hello.suite.name();
        Ok(tls)
    }

    fn handshake13(
        &mut self,
        host: &str,
        now: u64,
        trust: &Trust,
        hello: &ServerHello,
        share: &KeyShare,
        mut transcript: Vec<u8>,
    ) -> Result<(), Failure> {
        if hello.group != share.group() {
            return Err(fail("groupe d'echange de cles inattendu").into());
        }
        let suite = hello.suite;
        let hash = suite.hash;
        let shared = share.agree(&hello.key).ok_or_else(|| fail("cle d'echange du serveur invalide"))?;

        // --- key schedule ---
        let zeros = vec![0u8; hash.len()];
        let early = hash.extract(&[], &zeros);
        let derived = hash.derive_secret(&early, "derived", &[]);
        let handshake_secret = hash.extract(&derived, &shared);
        let client_hs = hash.derive_secret(&handshake_secret, "c hs traffic", &transcript);
        let server_hs = hash.derive_secret(&handshake_secret, "s hs traffic", &transcript);
        self.read = Some(Direction::tls13(suite, server_hs.clone()));

        // --- the server's encrypted flight ---
        let mut chain: Vec<Vec<u8>> = Vec::new();
        let mut certificate_requested: Option<Vec<u8>> = None;
        let mut verified_key: Option<Key> = None;
        loop {
            let message = self.next_handshake()?;
            let mut cursor = Cursor { data: &message[4..] };
            match message[0] {
                ENCRYPTED_EXTENSIONS => transcript.extend_from_slice(&message),
                CERTIFICATE_REQUEST => {
                    let context = cursor.vector(1).unwrap_or(&[]).to_vec();
                    certificate_requested = Some(context);
                    transcript.extend_from_slice(&message);
                }
                CERTIFICATE => {
                    cursor.vector(1).ok_or_else(|| fail("Certificate mal forme"))?;
                    let mut list = Cursor {
                        data: cursor.vector(3).ok_or_else(|| fail("Certificate mal forme"))?,
                    };
                    let mut stapled: Option<Vec<u8>> = None;
                    while !list.data.is_empty() {
                        let der = list.vector(3).ok_or_else(|| fail("Certificate mal forme"))?;
                        let mut extensions = Cursor {
                            data: list.vector(2).ok_or_else(|| fail("Certificate mal forme"))?,
                        };
                        // The leaf's `status_request` carries the stapled
                        // OCSP response (RFC 8446, 4.4.2.1).
                        while chain.is_empty() && !extensions.data.is_empty() {
                            let kind = extensions.u16().ok_or_else(|| fail("Certificate mal forme"))?;
                            let mut body = Cursor {
                                data: extensions.vector(2).ok_or_else(|| fail("Certificate mal forme"))?,
                            };
                            if kind == 0x0005 && body.u8() == Some(1) {
                                stapled = body.vector(3).map(|response| response.to_vec());
                            }
                        }
                        chain.push(der.to_vec());
                    }
                    let verified = x509::verify_chain(&chain, host, now, trust).map_err(|error| rejected(error, host))?;
                    self.accept(verified, stapled.as_deref(), now)?;
                    verified_key = self.session.chain.first().and_then(|leaf| leaf.key.clone());
                    transcript.extend_from_slice(&message);
                }
                CERTIFICATE_VERIFY => {
                    let key = verified_key.as_ref().ok_or_else(|| fail("CertificateVerify sans certificat"))?;
                    let algorithm = cursor.u16().ok_or_else(|| fail("CertificateVerify mal forme"))?;
                    let signature = cursor.vector(2).ok_or_else(|| fail("CertificateVerify mal forme"))?;
                    let mut content = vec![0x20u8; 64];
                    content.extend_from_slice(b"TLS 1.3, server CertificateVerify");
                    content.push(0);
                    content.extend_from_slice(&hash.hash(&transcript));
                    // PKCS #1 v1.5 is forbidden here by RFC 8446, 4.4.3.
                    let scheme = signature_scheme(algorithm, false)?;
                    if !x509::verify(key, scheme, &content, signature) {
                        return Err(fail("signature du serveur invalide").into());
                    }
                    transcript.extend_from_slice(&message);
                }
                FINISHED => {
                    if verified_key.is_none() {
                        return Err(fail("le serveur n'a presente aucun certificat").into());
                    }
                    let finished_key = hash.expand_label(&server_hs, "finished", &[], hash.len());
                    let expected = hash.hmac(&finished_key, &hash.hash(&transcript));
                    if !crate::crypto::equal(&expected, &message[4..]) {
                        return Err(fail("message Finished du serveur invalide").into());
                    }
                    transcript.extend_from_slice(&message);
                    break;
                }
                _ => return Err(fail("message de negociation inattendu").into()),
            }
        }

        // --- application secrets: over ClientHello..server Finished ---
        let derived = hash.derive_secret(&handshake_secret, "derived", &[]);
        let master = hash.extract(&derived, &zeros);
        let client_ap = hash.derive_secret(&master, "c ap traffic", &transcript);
        let server_ap = hash.derive_secret(&master, "s ap traffic", &transcript);

        // --- the client's flight ---
        // A dummy ChangeCipherSpec first: middleboxes that do not know
        // TLS 1.3 let a session through only if it looks like 1.2.
        self.send_plain(CONTENT_CHANGE_CIPHER_SPEC, &[1], false)?;
        self.write = Some(Direction::tls13(suite, client_hs.clone()));
        if let Some(context) = certificate_requested {
            let mut body = Vec::new();
            put_vector(&mut body, 1, &context);
            put_vector(&mut body, 3, &[]);
            let mut message = vec![CERTIFICATE];
            put_vector(&mut message, 3, &body);
            self.send_encrypted(CONTENT_HANDSHAKE, &message)?;
            transcript.extend_from_slice(&message);
        }
        let finished_key = hash.expand_label(&client_hs, "finished", &[], hash.len());
        let verify_data = hash.hmac(&finished_key, &hash.hash(&transcript));
        let mut finished = vec![FINISHED];
        put_vector(&mut finished, 3, &verify_data);
        self.send_encrypted(CONTENT_HANDSHAKE, &finished)?;

        self.read = Some(Direction::tls13(suite, server_ap));
        self.write = Some(Direction::tls13(suite, client_ap));
        self.session.group = group_name(share.group());
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn handshake12(
        &mut self,
        host: &str,
        now: u64,
        trust: &Trust,
        hello: &ServerHello,
        client_random: &[u8; 32],
        random: &mut dyn FnMut(&mut [u8]),
        mut transcript: Vec<u8>,
    ) -> Result<(), Failure> {
        let suite = hello.suite;
        let hash = suite.hash;

        let mut leaf_key: Option<Key> = None;
        let mut server_share: Option<(u16, Vec<u8>)> = None;
        let mut certificate_requested = false;
        loop {
            let message = self.next_handshake()?;
            let mut cursor = Cursor { data: &message[4..] };
            match message[0] {
                CERTIFICATE => {
                    let mut list = Cursor {
                        data: cursor.vector(3).ok_or_else(|| fail("Certificate mal forme"))?,
                    };
                    let mut chain = Vec::new();
                    while !list.data.is_empty() {
                        chain.push(list.vector(3).ok_or_else(|| fail("Certificate mal forme"))?.to_vec());
                    }
                    let verified = x509::verify_chain(&chain, host, now, trust).map_err(|error| rejected(error, host))?;
                    self.accept(verified, None, now)?;
                    leaf_key = self.session.chain.first().and_then(|leaf| leaf.key.clone());
                }
                CERTIFICATE_STATUS => {
                    // 1.2 staples in a message of its own (RFC 6066, 8).
                    if cursor.u8() == Some(1)
                        && let Some(response) = cursor.vector(3)
                    {
                        let response = response.to_vec();
                        self.staple(&response, now)?;
                    }
                }
                SERVER_KEY_EXCHANGE => {
                    let key = leaf_key.as_ref().ok_or_else(|| fail("echange de cles sans certificat"))?;
                    let params_start = cursor.data;
                    if cursor.u8() != Some(3) {
                        return Err(fail("courbe non nommee refusee").into());
                    }
                    let group = cursor.u16().ok_or_else(|| fail("ServerKeyExchange mal forme"))?;
                    let public = cursor.vector(1).ok_or_else(|| fail("ServerKeyExchange mal forme"))?.to_vec();
                    let params = &params_start[..params_start.len() - cursor.data.len()];
                    let algorithm = cursor.u16().ok_or_else(|| fail("ServerKeyExchange mal forme"))?;
                    let signature = cursor.vector(2).ok_or_else(|| fail("ServerKeyExchange mal forme"))?;
                    let mut signed = client_random.to_vec();
                    signed.extend_from_slice(&hello.random);
                    signed.extend_from_slice(params);
                    let scheme = signature_scheme(algorithm, true)?;
                    if !x509::verify(key, scheme, &signed, signature) {
                        return Err(fail("signature de l'echange de cles invalide").into());
                    }
                    server_share = Some((group, public));
                }
                CERTIFICATE_REQUEST => certificate_requested = true,
                SERVER_HELLO_DONE => {
                    transcript.extend_from_slice(&message);
                    break;
                }
                _ => return Err(fail("message de negociation TLS 1.2 inattendu").into()),
            }
            transcript.extend_from_slice(&message);
        }

        let (group, public) = server_share.ok_or_else(|| fail("le serveur n'a pas envoye d'echange de cles"))?;
        let share = KeyShare::for_group(group, random).ok_or_else(|| fail("courbe de l'echange non prise en charge"))?;
        let premaster = share.agree(&public).ok_or_else(|| fail("cle d'echange du serveur invalide"))?;

        // --- the client's flight ---
        if certificate_requested {
            let message = [CERTIFICATE, 0, 0, 3, 0, 0, 0];
            self.send_plain(CONTENT_HANDSHAKE, &message, false)?;
            transcript.extend_from_slice(&message);
        }
        let mut body = Vec::new();
        put_vector(&mut body, 1, &share.public());
        let mut exchange = vec![CLIENT_KEY_EXCHANGE];
        put_vector(&mut exchange, 3, &body);
        self.send_plain(CONTENT_HANDSHAKE, &exchange, false)?;
        transcript.extend_from_slice(&exchange);

        // RFC 7627: the master secret covers the whole handshake so far.
        // Without it, the classic derivation over the two randoms: the
        // triple handshake attack it opens needs resumption, renegotiation
        // or a client certificate, and this client does none of the three.
        let master = if hello.extended_master_secret {
            hash.prf(&premaster, "extended master secret", &hash.hash(&transcript), 48)
        } else {
            let mut randoms = client_random.to_vec();
            randoms.extend_from_slice(&hello.random);
            hash.prf(&premaster, "master secret", &randoms, 48)
        };
        let mut seed = hello.random.to_vec();
        seed.extend_from_slice(client_random);
        let key_len = suite.aead.key_len();
        let iv_len = if suite.aead == Aead::Chacha { 12 } else { 4 };
        let block = hash.prf(&master, "key expansion", &seed, 2 * key_len + 2 * iv_len);
        let (client_key, rest) = block.split_at(key_len);
        let (server_key, rest) = rest.split_at(key_len);
        let (client_iv, server_iv) = rest.split_at(iv_len);

        self.send_plain(CONTENT_CHANGE_CIPHER_SPEC, &[1], false)?;
        self.write = Some(Direction::tls12(suite, client_key, client_iv));
        self.pending_read = Some(Direction::tls12(suite, server_key, server_iv));
        let verify_data = hash.prf(&master, "client finished", &hash.hash(&transcript), 12);
        let mut finished = vec![FINISHED];
        put_vector(&mut finished, 3, &verify_data);
        self.send_encrypted(CONTENT_HANDSHAKE, &finished)?;
        transcript.extend_from_slice(&finished);

        // The server's ChangeCipherSpec switches the read keys (see
        // `next_handshake`), then its Finished arrives encrypted.
        loop {
            let message = self.next_handshake()?;
            match message[0] {
                NEW_SESSION_TICKET => transcript.extend_from_slice(&message),
                FINISHED => {
                    if self.pending_read.is_some() {
                        return Err(fail("Finished recu en clair").into());
                    }
                    let expected = hash.prf(&master, "server finished", &hash.hash(&transcript), 12);
                    if !crate::crypto::equal(&expected, &message[4..]) {
                        return Err(fail("message Finished du serveur invalide").into());
                    }
                    break;
                }
                _ => return Err(fail("message inattendu en fin de negociation").into()),
            }
        }
        self.session.group = group_name(group);
        Ok(())
    }

    /// Records a verified chain, and checks a stapled OCSP response.
    fn accept(&mut self, verified: x509::Verified, stapled: Option<&[u8]>, now: u64) -> Result<(), Failure> {
        self.session.subject = verified.chain.first().map(|leaf| leaf.common_name.clone()).unwrap_or_default();
        self.session.issuer_verified = !verified.exception;
        self.session.anchor = verified.anchor;
        self.session.exception = verified.exception;
        self.session.chain = verified.chain;
        if let Some(response) = stapled {
            self.staple(response, now)?;
        }
        Ok(())
    }

    /// A stapled OCSP response: a verified "revoked" ends the connection.
    fn staple(&mut self, response: &[u8], now: u64) -> Result<(), Failure> {
        let revocation = match (self.session.chain.first(), self.session.chain.get(1)) {
            (Some(leaf), Some(issuer)) => x509::check_ocsp(response, leaf, issuer, now),
            _ => Revocation::Unverifiable("emetteur absent de la chaine"),
        };
        if revocation == Revocation::Revoked {
            return Err(Failure {
                message: String::from("certificat revoque par son autorite (OCSP)"),
                certificates: self.session.chain.clone(),
                host: String::new(),
            });
        }
        self.session.revocation = revocation;
        Ok(())
    }

    fn send_plain(&mut self, content_type: u8, data: &[u8], first: bool) -> Result<(), Error> {
        let mut record = vec![content_type, 3, if first { 1 } else { 3 }];
        put_vector(&mut record, 2, data);
        self.io.send(&record).map_err(fail)
    }

    fn send_encrypted(&mut self, content_type: u8, data: &[u8]) -> Result<(), Error> {
        let direction = self.write.as_mut().ok_or_else(|| fail("pas de cle d'emission"))?;
        let record = direction.seal(content_type, data);
        self.io.send(&record).map_err(fail)
    }

    /// Reads one record, decrypting it when keys are in place.
    fn read_record(&mut self) -> Result<Option<(u8, Vec<u8>)>, Error> {
        loop {
            if self.incoming.len() >= 5 {
                let length = ((self.incoming[3] as usize) << 8) | self.incoming[4] as usize;
                if length > MAX_FRAGMENT + 256 {
                    return Err(fail("enregistrement TLS trop long"));
                }
                if self.incoming.len() >= 5 + length {
                    let record: Vec<u8> = self.incoming.drain(..5 + length).collect();
                    let content_type = record[0];
                    let (header, body) = record.split_at(5);
                    // In 1.3 only type 23 is encrypted (a ChangeCipherSpec
                    // may still come in clear); in 1.2 everything after the
                    // ChangeCipherSpec is.
                    let encrypted = match self.read.as_ref().map(|direction| direction.framing) {
                        Some(Framing::Tls13) => content_type == CONTENT_APPLICATION,
                        Some(_) => content_type != CONTENT_CHANGE_CIPHER_SPEC,
                        None => false,
                    };
                    if encrypted && let Some(direction) = self.read.as_mut() {
                        let opened = direction
                            .open(header, body)
                            .ok_or_else(|| fail("enregistrement TLS non authentifie"))?;
                        return Ok(Some(opened));
                    }
                    return Ok(Some((content_type, body.to_vec())));
                }
            }
            let data = self.io.recv(32 * 1024).map_err(fail)?;
            if data.is_empty() {
                return Ok(None);
            }
            self.incoming.extend_from_slice(&data);
        }
    }

    /// The next handshake message, header included.
    fn next_handshake(&mut self) -> Result<Vec<u8>, Error> {
        loop {
            if self.handshake.len() >= 4 {
                let length = ((self.handshake[1] as usize) << 16)
                    | ((self.handshake[2] as usize) << 8)
                    | self.handshake[3] as usize;
                if length > 256 * 1024 {
                    return Err(fail("message de negociation trop long"));
                }
                if self.handshake.len() >= 4 + length {
                    return Ok(self.handshake.drain(..4 + length).collect());
                }
            }
            match self.read_record()? {
                None => return Err(fail("connexion fermee pendant la negociation")),
                Some((CONTENT_HANDSHAKE, data)) => self.handshake.extend_from_slice(&data),
                Some((CONTENT_CHANGE_CIPHER_SPEC, _)) => {
                    // 1.2: from here on the server encrypts. In 1.3 the
                    // record is a compatibility dummy with no effect.
                    if let Some(direction) = self.pending_read.take() {
                        self.read = Some(direction);
                    }
                }
                Some((CONTENT_ALERT, data)) => {
                    return Err(fail(alert_text(data.get(1).copied().unwrap_or(0))));
                }
                Some(_) => return Err(fail("enregistrement inattendu pendant la negociation")),
            }
        }
    }

    /// Sends application data.
    pub fn write(&mut self, data: &[u8]) -> Result<(), Error> {
        for chunk in data.chunks(MAX_FRAGMENT) {
            self.send_encrypted(CONTENT_APPLICATION, chunk)?;
        }
        Ok(())
    }

    /// Reads application data: at most `max` bytes, empty at the end of
    /// the stream.
    pub fn read(&mut self, max: usize) -> Result<Vec<u8>, Error> {
        while self.plaintext.is_empty() && !self.closed {
            match self.read_record()? {
                None => self.closed = true,
                Some((CONTENT_APPLICATION, data)) => self.plaintext.extend(data),
                Some((CONTENT_HANDSHAKE, data)) => self.post_handshake(&data)?,
                Some((CONTENT_ALERT, data)) => {
                    // close_notify ends the stream; anything else is an
                    // error the server raised.
                    if data.get(1) == Some(&0) {
                        self.closed = true;
                    } else {
                        return Err(fail(alert_text(data.get(1).copied().unwrap_or(0))));
                    }
                }
                Some(_) => {}
            }
        }
        let count = max.min(self.plaintext.len());
        Ok(self.plaintext.drain(..count).collect())
    }

    /// Messages after the handshake: tickets, which this client does not
    /// use, and 1.3 key updates, which it must follow. A 1.2 HelloRequest
    /// asks for a renegotiation, which is ignored -- the RFC allows it.
    fn post_handshake(&mut self, data: &[u8]) -> Result<(), Error> {
        self.handshake.extend_from_slice(data);
        while self.handshake.len() >= 4 {
            let length = ((self.handshake[1] as usize) << 16)
                | ((self.handshake[2] as usize) << 8)
                | self.handshake[3] as usize;
            if self.handshake.len() < 4 + length {
                break;
            }
            let message: Vec<u8> = self.handshake.drain(..4 + length).collect();
            if message[0] == KEY_UPDATE && !self.suite.legacy {
                let suite = self.suite;
                let hash = suite.hash;
                if let Some(read) = self.read.as_ref() {
                    let next = hash.expand_label(&read.secret, "traffic upd", &[], hash.len());
                    self.read = Some(Direction::tls13(suite, next));
                }
                if message.get(4) == Some(&1) {
                    self.send_encrypted(CONTENT_HANDSHAKE, &[KEY_UPDATE, 0, 0, 1, 0])?;
                    if let Some(write) = self.write.as_ref() {
                        let next = hash.expand_label(&write.secret, "traffic upd", &[], hash.len());
                        self.write = Some(Direction::tls13(suite, next));
                    }
                }
            }
        }
        Ok(())
    }

    /// Says goodbye, politely.
    pub fn close(&mut self) {
        let _ = self.send_encrypted(CONTENT_ALERT, &[1, 0]);
    }
}

/// The trust anchors compiled into the kernel.
pub fn anchors() -> Vec<Anchor<'static>> {
    let data = super::roots::DATA;
    let mut anchors = Vec::with_capacity(super::roots::COUNT);
    let mut offset = 0;
    while offset + 2 <= data.len() {
        let subject_len = ((data[offset] as usize) << 8) | data[offset + 1] as usize;
        let subject = &data[offset + 2..offset + 2 + subject_len];
        offset += 2 + subject_len;
        let spki_len = ((data[offset] as usize) << 8) | data[offset + 1] as usize;
        let spki = &data[offset + 2..offset + 2 + spki_len];
        offset += 2 + spki_len;
        anchors.push(Anchor { subject, spki });
    }
    anchors
}
