//! X.509 certificates (RFC 5280): parsing, chain verification, the trust
//! store, and OCSP stapling.
//!
//! ## What is checked
//!
//! * every signature along the chain, up to a trust anchor;
//! * every validity period, against the wall clock;
//! * that each issuer below the anchor is a CA (`basicConstraints`);
//! * that the leaf names the host (`subjectAltName`, with the single-label
//!   wildcard of RFC 6125);
//! * revocation, **when the server staples an OCSP response** (RFC 6066,
//!   RFC 6960): a stapled "revoked" refuses the connection; a response
//!   that does not verify is ignored, as browsers do.
//!
//! ## The trust store
//!
//! The anchors compiled into the kernel (`roots`), minus those the user
//! distrusted, plus those the user added from a file; and per-site
//! exceptions: a certificate the user chose to accept for one host,
//! pinned by its SHA-256 fingerprint -- a different certificate on the
//! same host is refused again. The browser keeps that store per account.
//!
//! ## What is not
//!
//! Revocation lists (CRL) and OCSP queries of our own: a certificate
//! revoked but not stapled is accepted until it expires. Name
//! constraints, policies and path lengths are not enforced. Named so that
//! nobody believes otherwise.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use super::der::{self, Element, Reader};
use crate::crypto::ecc::Curve;
use crate::crypto::rsa::{Digest, PublicKey as RsaKey};

const OID_RSA: &str = "1.2.840.113549.1.1.1";
const OID_RSA_PSS: &str = "1.2.840.113549.1.1.10";
const OID_RSA_SHA256: &str = "1.2.840.113549.1.1.11";
const OID_RSA_SHA384: &str = "1.2.840.113549.1.1.12";
const OID_RSA_SHA512: &str = "1.2.840.113549.1.1.13";
const OID_EC: &str = "1.2.840.10045.2.1";
const OID_P256: &str = "1.2.840.10045.3.1.7";
const OID_P384: &str = "1.3.132.0.34";
const OID_ECDSA_SHA256: &str = "1.2.840.10045.4.3.2";
const OID_ECDSA_SHA384: &str = "1.2.840.10045.4.3.3";
const OID_ECDSA_SHA512: &str = "1.2.840.10045.4.3.4";
const OID_SHA256: &str = "2.16.840.1.101.3.4.2.1";
const OID_SHA384: &str = "2.16.840.1.101.3.4.2.2";
const OID_SHA512: &str = "2.16.840.1.101.3.4.2.3";
const OID_BASIC_CONSTRAINTS: &str = "2.5.29.19";
const OID_SUBJECT_ALT_NAME: &str = "2.5.29.17";
const OID_COMMON_NAME: &str = "2.5.4.3";
const OID_OCSP_BASIC: &str = "1.3.6.1.5.5.7.48.1.1";

/// A subject public key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Key {
    Rsa { modulus: Vec<u8>, exponent: Vec<u8> },
    P256(Vec<u8>),
    P384(Vec<u8>),
}

/// How a certificate is signed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Signature {
    RsaPkcs1(Digest),
    RsaPss(Digest),
    Ecdsa(Digest),
}

/// Decodes a `SubjectPublicKeyInfo`.
pub fn parse_key(spki: &[u8]) -> Option<Key> {
    let mut outer = Reader::new(spki);
    let info = outer.expect(der::SEQUENCE)?;
    let mut info = info.reader();
    let algorithm = info.expect(der::SEQUENCE)?;
    let mut algorithm = algorithm.reader();
    let oid = der::oid_text(algorithm.expect(der::OID)?.content);
    let bits = info.expect(der::BIT_STRING)?.bits()?;
    match oid.as_str() {
        OID_RSA => {
            let mut key = Reader::new(bits);
            let mut key = key.expect(der::SEQUENCE)?.reader();
            let modulus = key.expect(der::INTEGER)?.content.to_vec();
            let exponent = key.expect(der::INTEGER)?.content.to_vec();
            Some(Key::Rsa { modulus, exponent })
        }
        OID_EC => {
            let curve = der::oid_text(algorithm.expect(der::OID)?.content);
            match curve.as_str() {
                OID_P256 => Some(Key::P256(bits.to_vec())),
                OID_P384 => Some(Key::P384(bits.to_vec())),
                _ => None,
            }
        }
        _ => None,
    }
}

fn digest_of(oid: &str) -> Option<Digest> {
    match oid {
        OID_SHA256 => Some(Digest::Sha256),
        OID_SHA384 => Some(Digest::Sha384),
        OID_SHA512 => Some(Digest::Sha512),
        _ => None,
    }
}

fn parse_signature_algorithm(element: &Element) -> Option<Signature> {
    let mut reader = element.reader();
    let oid = der::oid_text(reader.expect(der::OID)?.content);
    match oid.as_str() {
        OID_RSA_SHA256 => Some(Signature::RsaPkcs1(Digest::Sha256)),
        OID_RSA_SHA384 => Some(Signature::RsaPkcs1(Digest::Sha384)),
        OID_RSA_SHA512 => Some(Signature::RsaPkcs1(Digest::Sha512)),
        OID_ECDSA_SHA256 => Some(Signature::Ecdsa(Digest::Sha256)),
        OID_ECDSA_SHA384 => Some(Signature::Ecdsa(Digest::Sha384)),
        OID_ECDSA_SHA512 => Some(Signature::Ecdsa(Digest::Sha512)),
        OID_RSA_PSS => {
            // RSASSA-PSS-params: [0] hashAlgorithm, default SHA-1 -- which
            // is refused, since nothing issues it any more.
            let params = reader.expect(der::SEQUENCE)?;
            let mut params = params.reader();
            let hash = params.expect(0xa0)?;
            let mut hash = hash.reader();
            let hash = hash.expect(der::SEQUENCE)?;
            let mut hash = hash.reader();
            let digest = digest_of(&der::oid_text(hash.expect(der::OID)?.content))?;
            Some(Signature::RsaPss(digest))
        }
        _ => None,
    }
}

/// Does `key` sign `message` into `signature` with `algorithm`?
pub fn verify(key: &Key, algorithm: Signature, message: &[u8], signature: &[u8]) -> bool {
    match (key, algorithm) {
        (Key::Rsa { modulus, exponent }, Signature::RsaPkcs1(digest)) => RsaKey {
            modulus: modulus.clone(),
            exponent: exponent.clone(),
        }
        .verify_pkcs1(digest, message, signature),
        (Key::Rsa { modulus, exponent }, Signature::RsaPss(digest)) => RsaKey {
            modulus: modulus.clone(),
            exponent: exponent.clone(),
        }
        .verify_pss(digest, message, signature),
        (Key::P256(point), Signature::Ecdsa(digest)) => {
            verify_ecdsa(&Curve::p256(), point, digest, message, signature)
        }
        (Key::P384(point), Signature::Ecdsa(digest)) => {
            verify_ecdsa(&Curve::p384(), point, digest, message, signature)
        }
        _ => false,
    }
}

fn verify_ecdsa(curve: &Curve, point: &[u8], digest: Digest, message: &[u8], signature: &[u8]) -> bool {
    let mut outer = Reader::new(signature);
    let Some(sequence) = outer.expect(der::SEQUENCE) else {
        return false;
    };
    let mut values = sequence.reader();
    let (Some(r), Some(s)) = (values.expect(der::INTEGER), values.expect(der::INTEGER)) else {
        return false;
    };
    curve.verify(point, &digest.hash(message), r.content, s.content)
}

/// The fields of a certificate this client uses.
#[derive(Debug, Clone)]
pub struct Certificate {
    /// The whole certificate, DER: what is fingerprinted and exported.
    pub der: Vec<u8>,
    pub serial: Vec<u8>,
    pub tbs: Vec<u8>,
    pub signature_algorithm: Option<Signature>,
    pub signature: Vec<u8>,
    pub issuer: Vec<u8>,
    pub subject: Vec<u8>,
    pub not_before: u64,
    pub not_after: u64,
    pub key: Option<Key>,
    pub spki: Vec<u8>,
    pub is_ca: bool,
    pub dns_names: Vec<String>,
    pub ip_addresses: Vec<Vec<u8>>,
    pub common_name: String,
}

impl Certificate {
    pub fn parse(data: &[u8]) -> Option<Certificate> {
        let mut outer = Reader::new(data);
        let certificate = outer.expect(der::SEQUENCE)?;
        let mut certificate = certificate.reader();
        let tbs = certificate.expect(der::SEQUENCE)?;
        let algorithm = certificate.expect(der::SEQUENCE)?;
        let signature = certificate.expect(der::BIT_STRING)?.bits()?.to_vec();

        let mut fields = tbs.reader();
        // [0] version, optional (v1 certificates have none).
        fields.optional(0xa0);
        let serial = fields.expect(der::INTEGER)?.content.to_vec();
        fields.expect(der::SEQUENCE)?; // signature algorithm, again
        let issuer = fields.expect(der::SEQUENCE)?.raw.to_vec();
        let validity = fields.expect(der::SEQUENCE)?;
        let mut validity = validity.reader();
        let not_before = der::time(&validity.next()?)?;
        let not_after = der::time(&validity.next()?)?;
        let subject_element = fields.expect(der::SEQUENCE)?;
        let subject = subject_element.raw.to_vec();
        let spki = fields.expect(der::SEQUENCE)?.raw.to_vec();

        let mut is_ca = false;
        let mut dns_names = Vec::new();
        let mut ip_addresses = Vec::new();
        while let Some(element) = fields.next() {
            // [1] issuerUniqueID, [2] subjectUniqueID, [3] extensions.
            if element.tag != 0xa3 {
                continue;
            }
            let mut wrapper = element.reader();
            let extensions = wrapper.expect(der::SEQUENCE)?;
            let mut extensions = extensions.reader();
            while let Some(extension) = extensions.next() {
                let mut extension = extension.reader();
                let oid = der::oid_text(extension.expect(der::OID)?.content);
                extension.optional(der::BOOLEAN);
                let value = extension.expect(der::OCTET_STRING)?.content;
                match oid.as_str() {
                    OID_BASIC_CONSTRAINTS => {
                        let mut value = Reader::new(value);
                        if let Some(sequence) = value.expect(der::SEQUENCE) {
                            let mut sequence = sequence.reader();
                            if let Some(flag) = sequence.optional(der::BOOLEAN) {
                                is_ca = flag.content.first().is_some_and(|byte| *byte != 0);
                            }
                        }
                    }
                    OID_SUBJECT_ALT_NAME => {
                        let mut value = Reader::new(value);
                        if let Some(names) = value.expect(der::SEQUENCE) {
                            let mut names = names.reader();
                            while let Some(name) = names.next() {
                                match name.tag {
                                    // [2] dNSName, IA5String.
                                    0x82 => {
                                        if let Ok(text) = core::str::from_utf8(name.content) {
                                            dns_names.push(text.to_ascii_lowercase());
                                        }
                                    }
                                    // [7] iPAddress.
                                    0x87 => ip_addresses.push(name.content.to_vec()),
                                    _ => {}
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
        }

        Some(Certificate {
            der: outer_raw(data)?,
            serial,
            tbs: tbs.raw.to_vec(),
            signature_algorithm: parse_signature_algorithm(&algorithm),
            signature,
            issuer,
            common_name: common_name(&subject_element),
            subject,
            not_before,
            not_after,
            key: parse_key(&spki),
            spki,
            is_ca,
            dns_names,
            ip_addresses,
        })
    }

    /// Does this certificate name `host`?
    pub fn names(&self, host: &str) -> bool {
        let host = host.to_ascii_lowercase();
        if let Some(address) = parse_ipv4(&host) {
            return self.ip_addresses.iter().any(|ip| ip.as_slice() == address);
        }
        self.dns_names.iter().any(|pattern| match_name(pattern, &host))
    }

    /// Is `self` signed by the key `key`?
    pub fn signed_by(&self, key: &Key) -> bool {
        match self.signature_algorithm {
            Some(algorithm) => verify(key, algorithm, &self.tbs, &self.signature),
            None => false,
        }
    }

    /// SHA-256 of the whole certificate: what a person compares.
    pub fn fingerprint(&self) -> [u8; 32] {
        crate::crypto::sha2::sha256(&self.der)
    }

    pub fn subject_text(&self) -> String {
        name_text(&self.subject)
    }

    pub fn issuer_text(&self) -> String {
        name_text(&self.issuer)
    }

    pub fn is_self_issued(&self) -> bool {
        self.subject == self.issuer
    }

    /// "RSA 2048 bits", "ECDSA P-256".
    pub fn key_text(&self) -> String {
        match &self.key {
            Some(Key::Rsa { modulus, .. }) => {
                let start = modulus.iter().position(|byte| *byte != 0).unwrap_or(modulus.len());
                format!("RSA {} bits", (modulus.len() - start) * 8)
            }
            Some(Key::P256(_)) => String::from("ECDSA P-256"),
            Some(Key::P384(_)) => String::from("ECDSA P-384"),
            None => String::from("cle non prise en charge"),
        }
    }

    pub fn signature_text(&self) -> &'static str {
        match self.signature_algorithm {
            Some(Signature::RsaPkcs1(Digest::Sha256)) => "RSA PKCS#1 v1.5 avec SHA-256",
            Some(Signature::RsaPkcs1(Digest::Sha384)) => "RSA PKCS#1 v1.5 avec SHA-384",
            Some(Signature::RsaPkcs1(Digest::Sha512)) => "RSA PKCS#1 v1.5 avec SHA-512",
            Some(Signature::RsaPss(_)) => "RSA-PSS",
            Some(Signature::Ecdsa(Digest::Sha256)) => "ECDSA avec SHA-256",
            Some(Signature::Ecdsa(Digest::Sha384)) => "ECDSA avec SHA-384",
            Some(Signature::Ecdsa(Digest::Sha512)) => "ECDSA avec SHA-512",
            None => "algorithme non pris en charge",
        }
    }
}

/// The encoding of the outer SEQUENCE: `data` may carry trailing bytes.
fn outer_raw(data: &[u8]) -> Option<Vec<u8>> {
    Reader::new(data).expect(der::SEQUENCE).map(|element| element.raw.to_vec())
}

/// Hexadecimal, pairs separated by `:`, as certificate viewers show it.
pub fn hex_colons(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 3);
    for (index, byte) in bytes.iter().enumerate() {
        if index > 0 {
            out.push(':');
        }
        out.push_str(&format!("{:02X}", byte));
    }
    out
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{:02x}", byte)).collect()
}

pub fn unhex(text: &str) -> Option<Vec<u8>> {
    let text = text.trim();
    if text.len() % 2 != 0 {
        return None;
    }
    (0..text.len() / 2)
        .map(|index| u8::from_str_radix(text.get(index * 2..index * 2 + 2)?, 16).ok())
        .collect()
}

/// A distinguished name as text: `CN=exemple.org, O=Exemple, C=FR`.
pub fn name_text(name: &[u8]) -> String {
    let mut parts: Vec<String> = Vec::new();
    let mut outer = Reader::new(name);
    let Some(sequence) = outer.expect(der::SEQUENCE) else {
        return String::new();
    };
    let mut sets = sequence.reader();
    while let Some(set) = sets.next() {
        let mut attributes = set.reader();
        while let Some(attribute) = attributes.next() {
            let mut attribute = attribute.reader();
            let (Some(oid), Some(value)) = (attribute.expect(der::OID), attribute.next()) else {
                continue;
            };
            let label = match der::oid_text(oid.content).as_str() {
                "2.5.4.3" => String::from("CN"),
                "2.5.4.6" => String::from("C"),
                "2.5.4.7" => String::from("L"),
                "2.5.4.8" => String::from("ST"),
                "2.5.4.10" => String::from("O"),
                "2.5.4.11" => String::from("OU"),
                "2.5.4.5" => String::from("serialNumber"),
                "1.2.840.113549.1.9.1" => String::from("E"),
                other => String::from(other),
            };
            parts.push(format!("{}={}", label, String::from_utf8_lossy(value.content)));
        }
    }
    parts.join(", ")
}

fn common_name(subject: &Element) -> String {
    let mut sets = subject.reader();
    while let Some(set) = sets.next() {
        let mut attributes = set.reader();
        while let Some(attribute) = attributes.next() {
            let mut attribute = attribute.reader();
            let Some(oid) = attribute.expect(der::OID) else {
                continue;
            };
            if der::oid_text(oid.content) == OID_COMMON_NAME
                && let Some(value) = attribute.next()
            {
                return String::from_utf8_lossy(value.content).into_owned();
            }
        }
    }
    String::new()
}

fn parse_ipv4(host: &str) -> Option<[u8; 4]> {
    let mut out = [0u8; 4];
    let mut parts = host.split('.');
    for slot in out.iter_mut() {
        *slot = parts.next()?.parse().ok()?;
    }
    parts.next().is_none().then_some(out)
}

/// RFC 6125: exact match, or `*.` matching exactly one leftmost label.
fn match_name(pattern: &str, host: &str) -> bool {
    if let Some(suffix) = pattern.strip_prefix("*.") {
        match host.split_once('.') {
            Some((label, rest)) => !label.is_empty() && rest == suffix,
            None => false,
        }
    } else {
        pattern == host
    }
}

/// A trust anchor: the name certificates cite as issuer, and its key.
pub struct Anchor<'a> {
    pub subject: &'a [u8],
    pub spki: &'a [u8],
}

/// What a connection trusts.
#[derive(Debug, Clone, Default)]
pub struct Trust {
    /// (subject DER, SubjectPublicKeyInfo DER) of each anchor.
    pub anchors: Vec<(Vec<u8>, Vec<u8>)>,
    /// (host, SHA-256 of the leaf) the user accepted despite an error.
    pub exceptions: Vec<(String, [u8; 32])>,
}

impl Trust {
    /// The anchors compiled into the kernel, minus the SubjectPublicKeyInfo
    /// digests in `distrusted`.
    pub fn builtin(distrusted: &[[u8; 32]]) -> Trust {
        let mut trust = Trust::default();
        for anchor in super::tls::anchors() {
            let digest = crate::crypto::sha2::sha256(anchor.spki);
            if !distrusted.contains(&digest) {
                trust.anchors.push((anchor.subject.to_vec(), anchor.spki.to_vec()));
            }
        }
        trust
    }

    /// Adds a certificate the user trusts as an authority.
    pub fn add_root(&mut self, certificate: &Certificate) {
        self.anchors.push((certificate.subject.clone(), certificate.spki.clone()));
    }
}

/// A verified chain.
#[derive(Debug, Clone)]
pub struct Verified {
    /// The leaf, then its issuers as far as the one the anchor signed.
    pub chain: Vec<Certificate>,
    /// The name of the anchor the chain ends on, or empty when the leaf was
    /// accepted by an exception.
    pub anchor: String,
    /// Accepted by a user's exception, not by the chain.
    pub exception: bool,
}

/// Why a chain was refused, with what the server sent, so that the user
/// can look at it -- and accept it for this site if they choose to.
#[derive(Debug, Clone)]
pub struct ChainError {
    pub reason: &'static str,
    pub certificates: Vec<Certificate>,
}

impl ChainError {
    fn new(reason: &'static str, certificates: &[Certificate]) -> ChainError {
        ChainError {
            reason,
            certificates: certificates.to_vec(),
        }
    }
}

/// Verifies a chain as a TLS server sends it -- leaf first -- for `host`
/// at `now` (seconds since 1970; zero skips the dates, for a machine with
/// no clock).
pub fn verify_chain(chain: &[Vec<u8>], host: &str, now: u64, trust: &Trust) -> Result<Verified, ChainError> {
    let certificates: Vec<Certificate> = chain.iter().filter_map(|der| Certificate::parse(der)).collect();
    let Some(leaf) = certificates.first() else {
        return Err(ChainError::new("certificat illisible", &certificates));
    };
    match walk(&certificates, host, now, trust) {
        Ok(verified) => Ok(verified),
        Err(reason) => {
            // An exception accepts this very certificate on this very host.
            let fingerprint = leaf.fingerprint();
            if trust
                .exceptions
                .iter()
                .any(|(excepted, pinned)| excepted.eq_ignore_ascii_case(host) && *pinned == fingerprint)
            {
                return Ok(Verified {
                    chain: certificates.clone(),
                    anchor: String::new(),
                    exception: true,
                });
            }
            Err(ChainError::new(reason, &certificates))
        }
    }
}

fn walk(certificates: &[Certificate], host: &str, now: u64, trust: &Trust) -> Result<Verified, &'static str> {
    let leaf = &certificates[0];
    if !leaf.names(host) {
        return Err("le certificat ne correspond pas au site");
    }
    let in_time = |certificate: &Certificate| now == 0 || (certificate.not_before <= now && now <= certificate.not_after);
    if !in_time(leaf) {
        return Err("certificat expire ou pas encore valide");
    }

    let mut current = leaf.clone();
    let mut chain = alloc::vec![leaf.clone()];
    // Each certificate of the chain serves once: a self-signed root sent
    // by the server would otherwise issue itself round after round.
    let mut used = alloc::vec![false; certificates.len()];
    used[0] = true;
    for _ in 0..8 {
        // An anchor that issued the current certificate ends the walk. It
        // is looked for first: a server may send a cross-signed copy of a
        // root the store already trusts, and the stored one is the one
        // that counts.
        for (subject, spki) in &trust.anchors {
            if *subject == current.issuer
                && let Some(key) = parse_key(spki)
                && current.signed_by(&key)
            {
                return Ok(Verified {
                    chain,
                    anchor: name_text(subject),
                    exception: false,
                });
            }
        }
        let issuer = certificates.iter().enumerate().find(|(index, candidate)| {
            !used[*index]
                && candidate.subject == current.issuer
                && candidate.is_ca
                && in_time(candidate)
                && candidate.key.as_ref().is_some_and(|key| current.signed_by(key))
        });
        match issuer {
            Some((index, issuer)) => {
                used[index] = true;
                current = issuer.clone();
                chain.push(issuer.clone());
            }
            None => {
                return Err(if leaf.is_self_issued() {
                    "certificat auto-signe : aucune autorite ne le garantit"
                } else {
                    "autorite de certification inconnue ou signature invalide"
                });
            }
        }
    }
    Err("chaine de certificats trop longue")
}

// ============================================================
//  OCSP stapling (RFC 6960)
// ============================================================

/// What a stapled OCSP response says about the leaf.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Revocation {
    /// No response stapled.
    NotStapled,
    Good,
    Revoked,
    /// The responder does not know the certificate.
    Unknown,
    /// Stapled, but not verifiable -- ignored, as browsers do.
    Unverifiable(&'static str),
}

impl Revocation {
    pub fn text(&self) -> String {
        match self {
            Revocation::NotStapled => String::from("revocation non verifiee (pas de reponse OCSP agrafee)"),
            Revocation::Good => String::from("non revoque (reponse OCSP agrafee et verifiee)"),
            Revocation::Revoked => String::from("REVOQUE selon l'autorite"),
            Revocation::Unknown => String::from("inconnu du repondeur OCSP"),
            Revocation::Unverifiable(reason) => format!("reponse OCSP ignoree : {}", reason),
        }
    }
}

/// Checks a stapled OCSP response for `leaf`, issued by `issuer`.
pub fn check_ocsp(response: &[u8], leaf: &Certificate, issuer: &Certificate, now: u64) -> Revocation {
    match parse_ocsp(response, leaf, issuer, now) {
        Ok(status) => status,
        Err(reason) => Revocation::Unverifiable(reason),
    }
}

fn parse_ocsp(response: &[u8], leaf: &Certificate, issuer: &Certificate, now: u64) -> Result<Revocation, &'static str> {
    let bad = "reponse OCSP mal formee";
    let mut outer = Reader::new(response);
    let mut response = outer.expect(der::SEQUENCE).ok_or(bad)?.reader();
    // responseStatus ENUMERATED: 0 is "successful".
    let status = response.expect(0x0a).ok_or(bad)?;
    if status.content != [0] {
        return Err("le repondeur OCSP a signale une erreur");
    }
    let mut wrapper = response.expect(0xa0).ok_or(bad)?.reader();
    let mut bytes = wrapper.expect(der::SEQUENCE).ok_or(bad)?.reader();
    if der::oid_text(bytes.expect(der::OID).ok_or(bad)?.content) != OID_OCSP_BASIC {
        return Err("type de reponse OCSP inconnu");
    }
    let basic_der = bytes.expect(der::OCTET_STRING).ok_or(bad)?.content;
    let mut basic_outer = Reader::new(basic_der);
    let mut basic = basic_outer.expect(der::SEQUENCE).ok_or(bad)?.reader();
    let tbs = basic.expect(der::SEQUENCE).ok_or(bad)?;
    let algorithm = parse_signature_algorithm(&basic.expect(der::SEQUENCE).ok_or(bad)?).ok_or("signature OCSP non prise en charge")?;
    let signature = basic.expect(der::BIT_STRING).ok_or(bad)?.bits().ok_or(bad)?;

    // The signer: the issuer itself, or a responder certificate the
    // issuer signed.
    let issuer_key = issuer.key.as_ref().ok_or("cle de l'emetteur illisible")?;
    let mut signed = verify(issuer_key, algorithm, tbs.raw, signature);
    if !signed && let Some(certs) = basic.optional(0xa0) {
        let mut certs = certs.reader();
        if let Some(list) = certs.expect(der::SEQUENCE) {
            let mut list = list.reader();
            while let Some(candidate) = list.next() {
                if let Some(responder) = Certificate::parse(candidate.raw)
                    && responder.issuer == issuer.subject
                    && responder.signed_by(issuer_key)
                    && let Some(key) = &responder.key
                    && verify(key, algorithm, tbs.raw, signature)
                {
                    signed = true;
                    break;
                }
            }
        }
    }
    if !signed {
        return Err("signature OCSP invalide");
    }

    let mut data = tbs.reader();
    data.optional(0xa0); // version
    data.next().ok_or(bad)?; // responderID
    data.next().ok_or(bad)?; // producedAt
    let mut responses = data.expect(der::SEQUENCE).ok_or(bad)?.reader();
    while let Some(single) = responses.next() {
        let mut single = single.reader();
        let mut cert_id = single.expect(der::SEQUENCE).ok_or(bad)?.reader();
        cert_id.next().ok_or(bad)?; // hashAlgorithm
        cert_id.next().ok_or(bad)?; // issuerNameHash
        cert_id.next().ok_or(bad)?; // issuerKeyHash
        let serial = cert_id.expect(der::INTEGER).ok_or(bad)?.content;
        if serial != leaf.serial.as_slice() {
            continue;
        }
        let status = single.next().ok_or(bad)?;
        let this_update = single.next().and_then(|element| der::time(&element)).ok_or(bad)?;
        let next_update = single
            .optional(0xa0)
            .and_then(|wrapper| wrapper.reader().next().and_then(|element| der::time(&element)));
        if now != 0 {
            if this_update > now + 3600 {
                return Err("reponse OCSP datee du futur");
            }
            if next_update.is_some_and(|until| until + 3600 < now) {
                return Err("reponse OCSP perimee");
            }
        }
        return Ok(match status.tag {
            0x80 => Revocation::Good,
            0xa1 => Revocation::Revoked,
            _ => Revocation::Unknown,
        });
    }
    Err("la reponse OCSP ne parle pas de ce certificat")
}
