//! 3.3 -- Three-level signature system (spec 3.2).
//!
//! Three levels, three different roots of trust:
//!
//! | Level | Anchor | What verification proves |
//! |---|---|---|
//! | SYSTEM | Digest table frozen in the kernel | The binary is bit for bit the one shipped with the OS |
//! | ENTERPRISE | Ed25519 key of a known authority | The certificate was issued by that authority, for this very binary, with these legal fields and dates |
//! | SELF | The digest the binary carries itself | The binary has not been altered since it was produced |
//!
//! ## ENTERPRISE is a signature since phase 15
//!
//! Until then the authority -> binary chain was a SHA-256 of public data:
//! anyone able to read the format could forge a valid certificate, and
//! DECISIONS forbade presenting ENTERPRISE as verified. The certificate
//! (version 2) now carries the authority's **Ed25519 public key** and a
//! **signature** by it (`crypto::ed25519`) of the certificate fields and
//! the authenticity digest of the binary. The root store holds public
//! keys; a key is trusted because it is in the store, a certificate
//! because the key in the store signed it.
//!
//! The same primitive signs the other things an authority vouches for,
//! each under its own **domain tag** so that a signature made for one
//! cannot be replayed as another (`enterprise_message`): binaries
//! (`NOVA-BIN`), packages (`NOVA-SPM`), driver modules (`NOVA-KMD`) and
//! registry manifests (`NOVA-REG`, phase 15).
//!
//! What stays out of reach: revocation (no list is consulted) and a chain
//! longer than one level (the authority signs the binary directly). The
//! validity dates are compared with the hardware clock when it is known.
//!
//! ## The demonstration authority
//!
//! The self-tests need an authority whose secret they hold, to sign the
//! samples they check. Its seed is drawn from the kernel's generator at
//! first use, and lives in memory only: nothing outside this boot can sign
//! with it, which a seed written in the source would have allowed --
//! re-creating, with a real signature, exactly the forgeable authority
//! this version removed.

use alloc::string::String;

use super::BinaryHeader;
use super::header::{
    BinaryFormat, CERT_MAGIC, CERT_SIZE, CERT_VERSION, HEADER_SIZE, SignatureType, cert_offsets,
    offsets, read_u16, read_u64,
};
use super::sha256::{self, DIGEST_SIZE};

// ============================================================
//  3.3.2 -- SYSTEM root of trust
// ============================================================

/// Digest of an official binary, frozen at kernel compile time.
pub struct SystemEntry {
    pub app_id: &'static str,
    pub digest: [u8; DIGEST_SIZE],
}

/// Table of the recognised system applications.
///
/// Empty as long as no official binary is shipped: that is the correct
/// state. A table pre-filled with complacent values would give the
/// illusion of a check. The SDK (phase 9.1.5) will produce the real
/// digests and this table will be generated when the image is built.
///
/// The entries registered at run time by `register_system_digest` add to
/// this one, purely for the needs of the self-test.
static SYSTEM_DIGESTS: &[SystemEntry] = &[];

/// ENTERPRISE roots of trust: the Ed25519 public keys of the authorities.
///
/// Same remark as above -- empty by default; fed at boot by
/// `/sys/config/autorites.cfg` (`load_authorities`) and by the self-tests.
static ENTERPRISE_ROOTS: &[SystemEntry] = &[];

// ------------------------------------------------------------
//  Dynamic store
// ------------------------------------------------------------

/// Number of entries the dynamic store can hold.
///
/// Eight originally, when only the phase 3 self-test dropped into it.
/// Phase 7 puts its seven native modules there and phase 8 the binaries
/// of every SYSTEM package installed: the store was full before even
/// reaching the self-test, and `insert` returned `false` that nobody
/// read. The symptom misled -- a perfectly verified package laid down a
/// binary refused at load, with nothing linking the two.
const DYNAMIC_CAPACITY: usize = 32;

#[derive(Clone, Copy)]
struct DynamicEntry {
    digest: [u8; DIGEST_SIZE],
    used: bool,
}

impl DynamicEntry {
    const fn empty() -> Self {
        DynamicEntry {
            digest: [0u8; DIGEST_SIZE],
            used: false,
        }
    }
}

struct Store {
    system: [DynamicEntry; DYNAMIC_CAPACITY],
    roots: [DynamicEntry; DYNAMIC_CAPACITY],
}

impl Store {
    const fn new() -> Self {
        Store {
            system: [DynamicEntry::empty(); DYNAMIC_CAPACITY],
            roots: [DynamicEntry::empty(); DYNAMIC_CAPACITY],
        }
    }

    fn insert(slots: &mut [DynamicEntry; DYNAMIC_CAPACITY], digest: [u8; DIGEST_SIZE]) -> bool {
        for slot in slots.iter_mut() {
            if slot.used && sha256::equal(&slot.digest, &digest) {
                return true;
            }
        }
        for slot in slots.iter_mut() {
            if !slot.used {
                slot.digest = digest;
                slot.used = true;
                return true;
            }
        }
        false
    }

    fn contains(slots: &[DynamicEntry; DYNAMIC_CAPACITY], digest: &[u8; DIGEST_SIZE]) -> bool {
        slots
            .iter()
            .any(|slot| slot.used && sha256::equal(&slot.digest, digest))
    }
}

static STORE: crate::vga::SpinLock<Store> = crate::vga::SpinLock::new(Store::new());

/// Declares a digest as that of a SYSTEM application.
///
/// Reserved to the self-test and, later, to the install service that will
/// register the binaries shipped by an OS update verified upstream. It is
/// **not** a path open to applications.
pub fn register_system_digest(digest: [u8; DIGEST_SIZE]) -> bool {
    let inserted = {
        let mut store = STORE.lock();
        let slots = &mut store.system;
        Store::insert(slots, digest)
    };
    // A full store is a silent failure: the binary concerned will be
    // refused at load, and nothing in that refusal will say the cause is
    // here. The lock is released before logging -- the serial console
    // takes its own.
    if !inserted {
        crate::serial_println!(
            "[sign] magasin SYSTEM plein ({} entrees) - empreinte non enregistree",
            DYNAMIC_CAPACITY
        );
    }
    inserted
}

/// Declares an Ed25519 public key as an ENTERPRISE root of trust.
pub fn register_enterprise_root(digest: [u8; DIGEST_SIZE]) -> bool {
    let inserted = {
        let mut store = STORE.lock();
        let slots = &mut store.roots;
        Store::insert(slots, digest)
    };
    if !inserted {
        crate::serial_println!(
            "[sign] magasin des racines plein ({} entrees) - racine non enregistree",
            DYNAMIC_CAPACITY
        );
    }
    inserted
}

/// Is the digest in the store of official binaries?
///
/// Public since phase 7.3.2: a `.mod` module declaring itself SYSTEM must
/// answer the same question as a `.dsa`, and there is no reason to have
/// two stores for a single notion of official.
pub fn is_official(digest: &[u8; DIGEST_SIZE]) -> bool {
    is_known_system(digest)
}

fn is_known_system(digest: &[u8; DIGEST_SIZE]) -> bool {
    if SYSTEM_DIGESTS
        .iter()
        .any(|entry| sha256::equal(&entry.digest, digest))
    {
        return true;
    }
    let store = STORE.lock();
    Store::contains(&store.system, digest)
}

/// Is the key in the store of ENTERPRISE roots?
///
/// Public since phase 8.3.2: a `.spm` package carries its certificate
/// binding in its header rather than in a separate block, but the
/// question put to the store is exactly the same as for a binary. Two
/// stores for a single notion of trusted authority would be two stores to
/// keep in agreement.
pub fn is_trusted_authority(digest: &[u8; DIGEST_SIZE]) -> bool {
    is_known_root(digest)
}

fn is_known_root(digest: &[u8; DIGEST_SIZE]) -> bool {
    if ENTERPRISE_ROOTS
        .iter()
        .any(|entry| sha256::equal(&entry.digest, digest))
    {
        return true;
    }
    let store = STORE.lock();
    Store::contains(&store.roots, digest)
}

/// Number of anchors registered: (SYSTEM, ENTERPRISE roots).
pub fn store_size() -> (usize, usize) {
    let store = STORE.lock();
    (
        SYSTEM_DIGESTS.len() + store.system.iter().filter(|e| e.used).count(),
        ENTERPRISE_ROOTS.len() + store.roots.iter().filter(|e| e.used).count(),
    )
}

// ============================================================
//  Digests
// ============================================================

/// Digest of the payload -- the content really loaded into memory.
///
/// It is the one the `payload_hash` field of the header carries.
pub fn payload_digest(header: &BinaryHeader, data: &[u8]) -> [u8; DIGEST_SIZE] {
    let start = header.payload_offset as usize;
    let end = start + header.payload_size as usize;
    if header.payload_size == 0 || end > data.len() {
        return sha256::digest(&[]);
    }
    sha256::digest(&data[start..end])
}

/// Authenticity digest: identity + declarations + code.
///
/// Covers the header **with the digest field neutralised** -- otherwise it
/// would refer to itself -- followed by the payload. A binary whose
/// app_id, a permission or one byte of code is changed produces a
/// different digest.
pub fn authenticity_digest(header: &BinaryHeader, data: &[u8]) -> [u8; DIGEST_SIZE] {
    let mut hasher = sha256::Sha256::new();

    hasher.update(&data[..offsets::PAYLOAD_HASH]);
    hasher.update_zeros(DIGEST_SIZE);
    hasher.update(&data[offsets::PAYLOAD_HASH + DIGEST_SIZE..HEADER_SIZE]);

    let start = header.payload_offset as usize;
    let end = start + header.payload_size as usize;
    if header.payload_size > 0 && end <= data.len() {
        hasher.update(&data[start..end]);
    }

    hasher.finish()
}

// ============================================================
//  3.3.3 -- ENTERPRISE certificate
// ============================================================

/// Readable content of a certificate block.
#[derive(Debug, Clone)]
pub struct Certificate {
    pub version: u16,
    pub authority: String,
    pub subject: String,
    pub legal_id: String,
    pub valid_from: u64,
    pub valid_until: u64,
    /// Ed25519 public key of the authority.
    pub authority_key: [u8; 32],
    pub signature: [u8; 64],
}

fn read_cert_name(block: &[u8], offset: usize) -> String {
    let raw = &block[offset..offset + 32];
    let len = raw.iter().position(|&b| b == 0).unwrap_or(32);
    String::from(core::str::from_utf8(&raw[..len]).unwrap_or("?"))
}

/// Decodes the certificate block of a binary, if it carries one (any
/// version: `verify` refuses the obsolete ones by name).
pub fn read_certificate(header: &BinaryHeader, data: &[u8]) -> Option<Certificate> {
    if !header.has_certificate() {
        return None;
    }
    let base = header.cert_offset as usize;
    if base + CERT_SIZE > data.len() {
        return None;
    }
    let block = &data[base..base + CERT_SIZE];
    let version = read_u16(block, cert_offsets::VERSION);
    if block[..4] != CERT_MAGIC || version == 0 || version > CERT_VERSION {
        return None;
    }

    let mut authority_key = [0u8; 32];
    authority_key.copy_from_slice(&block[cert_offsets::AUTHORITY_KEY..cert_offsets::AUTHORITY_KEY + 32]);
    let mut signature = [0u8; 64];
    signature.copy_from_slice(&block[cert_offsets::SIGNATURE..cert_offsets::SIGNATURE + 64]);

    Some(Certificate {
        version,
        authority: read_cert_name(block, cert_offsets::AUTHORITY),
        subject: read_cert_name(block, cert_offsets::SUBJECT),
        legal_id: read_cert_name(block, cert_offsets::LEGAL_ID),
        valid_from: read_u64(block, cert_offsets::VALID_FROM),
        valid_until: read_u64(block, cert_offsets::VALID_UNTIL),
        authority_key,
        signature,
    })
}

// ============================================================
//  15.0.3 -- ENTERPRISE signatures (Ed25519)
// ============================================================

/// Domain tags: what kind of object a signature vouches for.
pub mod domain {
    pub const BINARY: &[u8] = b"NOVA-BIN\0";
    pub const PACKAGE: &[u8] = b"NOVA-SPM\0";
    pub const MODULE: &[u8] = b"NOVA-KMD\0";
    pub const REGISTRY: &[u8] = b"NOVA-REG\0";
}

/// The message an authority signs: a domain tag, then the parts.
pub fn enterprise_message(tag: &[u8], parts: &[&[u8]]) -> alloc::vec::Vec<u8> {
    let mut message = alloc::vec::Vec::from(tag);
    for part in parts {
        message.extend_from_slice(part);
    }
    message
}

/// Checks an ENTERPRISE signature: the key must be a root of the store,
/// and the signature its own on `message`.
pub fn check_enterprise(key: &[u8; 32], message: &[u8], signature: &[u8; 64]) -> Result<(), Reason> {
    if !is_known_root(key) {
        return Err(Reason::UntrustedAuthority);
    }
    if !crate::crypto::ed25519::verify(key, message, signature) {
        return Err(Reason::BindingMismatch);
    }
    Ok(())
}

/// Days since 2000-01-01 by the hardware clock, or `None` when unknown --
/// the unit of the validity fields.
pub fn today() -> Option<u64> {
    const EPOCH_2000: u64 = 946_684_800;
    crate::rtc::now().filter(|now| *now > EPOCH_2000).map(|now| (now - EPOCH_2000) / 86_400)
}

/// The message of a binary's certificate: tag, signed fields, digest.
pub fn certificate_message(block: &[u8], digest: &[u8; DIGEST_SIZE]) -> alloc::vec::Vec<u8> {
    enterprise_message(domain::BINARY, &[&block[..cert_offsets::SIGNED_END], digest])
}

/// Signs the certificate block of a binary in place (version 2): sets the
/// key and the signature from the authority's seed. The other fields must
/// be written already, `digest` be the binary's authenticity digest.
pub fn sign_certificate(block: &mut [u8], digest: &[u8; DIGEST_SIZE], seed: &[u8; 32]) {
    let key = crate::crypto::ed25519::public_key(seed);
    block[cert_offsets::VERSION..cert_offsets::VERSION + 2].copy_from_slice(&CERT_VERSION.to_le_bytes());
    block[cert_offsets::AUTHORITY_KEY..cert_offsets::AUTHORITY_KEY + 32].copy_from_slice(&key);
    let message = certificate_message(block, digest);
    let signature = crate::crypto::ed25519::sign(seed, &message);
    block[cert_offsets::SIGNATURE..cert_offsets::SIGNATURE + 64].copy_from_slice(&signature);
}

/// The demonstration authority's seed: drawn once per boot from the
/// kernel's generator, kept in memory only (see the module comment).
pub fn demo_authority_seed() -> [u8; 32] {
    static SEED: crate::vga::SpinLock<Option<[u8; 32]>> = crate::vga::SpinLock::new(None);
    let mut seed = SEED.lock();
    if let Some(seed) = *seed {
        return seed;
    }
    let mut fresh = [0u8; 32];
    crate::random::fill(&mut fresh);
    *seed = Some(fresh);
    fresh
}

/// The demonstration authority's public key.
pub fn demo_authority_key() -> [u8; 32] {
    static KEY: crate::vga::SpinLock<Option<[u8; 32]>> = crate::vga::SpinLock::new(None);
    if let Some(key) = *KEY.lock() {
        return key;
    }
    let key = crate::crypto::ed25519::public_key(&demo_authority_seed());
    *KEY.lock() = Some(key);
    key
}

/// File of the authorities an administrator trusts: `name key-hex` per
/// line, `#` for comments (spec 17: `/sys/config/`, writable by admins).
pub const AUTHORITIES_PATH: &str = "/sys/config/autorites.cfg";

/// Loads `AUTHORITIES_PATH` into the root store. Returns the number of
/// keys registered. A malformed line is reported and skipped.
pub fn load_authorities() -> usize {
    let Ok(bytes) = crate::fs::read_file(AUTHORITIES_PATH, crate::fs::perm::Credentials::kernel()) else {
        return 0;
    };
    let text = String::from_utf8_lossy(&bytes);
    let mut loaded = 0;
    for line in text.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')) {
        let hex = line.rsplit(char::is_whitespace).next().unwrap_or("");
        match parse_key(hex) {
            Some(key) => {
                if register_enterprise_root(key) {
                    loaded += 1;
                }
            }
            None => crate::serial_println!("[sign] {} : ligne ignoree (cle de 64 chiffres attendue)", AUTHORITIES_PATH),
        }
    }
    loaded
}

/// A 32-byte key written as 64 hexadecimal digits.
pub fn parse_key(hex: &str) -> Option<[u8; 32]> {
    if hex.len() != 64 || !hex.is_ascii() {
        return None;
    }
    let mut key = [0u8; 32];
    for (index, byte) in key.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&hex[index * 2..index * 2 + 2], 16).ok()?;
    }
    Some(key)
}

// ============================================================
//  Verdict
// ============================================================

/// Outcome of the verification of a binary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// Official application: digest present in the kernel table.
    System,
    /// Publisher identified by a certificate attached to a known authority.
    Enterprise,
    /// Self-signed: integrity checked, origin not established.
    SelfSigned,
    /// No signature -- the normal case of a `.ras`.
    Unsigned,
    /// Verification failed: the binary must not be loaded.
    Rejected(Reason),
}

/// Precise cause of a rejection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reason {
    /// The recomputed digest does not match the one in the header.
    PayloadTampered,
    /// SYSTEM signature claimed but digest unknown to the kernel.
    UnknownSystemBinary,
    /// Certificate block absent, truncated or of unknown version.
    MalformedCertificate,
    /// The issuing authority is not in the root store.
    UntrustedAuthority,
    /// The signature is not the authority's on this binary.
    BindingMismatch,
    /// A version 1 certificate: a hash of public data, not a signature.
    ObsoleteCertificate,
    /// Outside the validity dates of the certificate.
    CertificateExpired,
    /// The type declared by the certificate contradicts that of the header.
    CertificateTypeMismatch,
    /// Format/signature combination forbidden by spec 3.4.
    ForbiddenCombination,
}

impl Reason {
    pub fn message(&self) -> &'static str {
        match self {
            Reason::PayloadTampered => "l'empreinte de la charge utile ne correspond pas",
            Reason::UnknownSystemBinary => "signature SYSTEM revendiquee, empreinte inconnue",
            Reason::MalformedCertificate => "bloc de certificat absent ou mal forme",
            Reason::UntrustedAuthority => "autorite emettrice inconnue du magasin",
            Reason::BindingMismatch => "la signature de l'autorite ne couvre pas ce binaire",
            Reason::ObsoleteCertificate => "certificat v1 sans signature asymetrique - refuse",
            Reason::CertificateExpired => "certificat hors de sa periode de validite",
            Reason::CertificateTypeMismatch => "le certificat contredit le type du header",
            Reason::ForbiddenCombination => "combinaison format/signature interdite (spec 3.4)",
        }
    }
}

impl Verdict {
    pub fn accepted(&self) -> bool {
        !matches!(self, Verdict::Rejected(_))
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Verdict::System => "SYSTEM verifie",
            Verdict::Enterprise => "ENTERPRISE verifie",
            Verdict::SelfSigned => "SELF - integrite seule",
            Verdict::Unsigned => "non signe",
            Verdict::Rejected(_) => "REJETE",
        }
    }

    /// 3.2.4.4 and 10.4 -- Must the user be warned at launch?
    ///
    /// An unsigned or self-signed binary runs, but never in silence: the
    /// user must know they are trusting a stranger.
    pub fn requires_warning(&self) -> bool {
        matches!(self, Verdict::SelfSigned | Verdict::Unsigned)
    }
}

// ============================================================
//  3.3.5 -- Forbidden combinations
// ============================================================

/// Format/signature combinations allowed by spec 3.4.
///
/// Restates what `check_rules` already checks at parse time. The
/// redundancy is intended: the loader must not depend on a caller having
/// remembered to consult the violations before handing it an image.
pub fn combination_allowed(format: BinaryFormat, signature: SignatureType) -> bool {
    use BinaryFormat::*;
    use SignatureType::*;

    matches!(
        (format, signature),
        (Dsa, System)
            | (Ss, System)
            | (Ss, Enterprise)
            | (Gs, Enterprise)
            | (Gs, SelfSigned)
            | (Cus, _)
            | (Ras, None)
    )
}

// ============================================================
//  Verification
// ============================================================

/// Verifies the signature of an already structurally parsed binary.
///
/// Integrity is checked in every case, including for an unsigned `.ras`:
/// a truncated or corrupt file must not be loaded, whatever trust is
/// otherwise placed in it.
pub fn verify(header: &BinaryHeader, data: &[u8]) -> Verdict {
    if !combination_allowed(header.format(), header.signature()) {
        return Verdict::Rejected(Reason::ForbiddenCombination);
    }

    // --- integrity, common to the four levels ---
    let payload = payload_digest(header, data);
    if !sha256::equal(&payload, &header.payload_hash) {
        return Verdict::Rejected(Reason::PayloadTampered);
    }

    let digest = authenticity_digest(header, data);

    match header.signature() {
        // --- 3.3.2: digest frozen in the kernel ---
        SignatureType::System => {
            if is_known_system(&digest) {
                Verdict::System
            } else {
                Verdict::Rejected(Reason::UnknownSystemBinary)
            }
        }

        // --- 3.3.3: third-party certificate ---
        SignatureType::Enterprise => {
            let Some(cert) = read_certificate(header, data) else {
                return Verdict::Rejected(Reason::MalformedCertificate);
            };
            if cert.version < CERT_VERSION {
                return Verdict::Rejected(Reason::ObsoleteCertificate);
            }

            let base = header.cert_offset as usize;
            let block = &data[base..base + CERT_SIZE];
            if read_u16(block, cert_offsets::SIGNATURE_TYPE) != SignatureType::Enterprise as u16 {
                return Verdict::Rejected(Reason::CertificateTypeMismatch);
            }
            if let Err(reason) = check_enterprise(&cert.authority_key, &certificate_message(block, &digest), &cert.signature) {
                return Verdict::Rejected(reason);
            }
            if let Some(today) = today()
                && (today < cert.valid_from || today > cert.valid_until)
            {
                return Verdict::Rejected(Reason::CertificateExpired);
            }
            Verdict::Enterprise
        }

        // --- 3.3.4: self-signed ---
        // Integrity has already been checked above: that is all this level
        // can prove. The install warning is carried by
        // `requires_warning`.
        SignatureType::SelfSigned => Verdict::SelfSigned,

        SignatureType::None => Verdict::Unsigned,
    }
}

/// Logs the detail of a verification on the serial console.
pub fn dump(header: &BinaryHeader, data: &[u8], verdict: Verdict) {
    let digest = authenticity_digest(header, data);
    let hex = sha256::short_hex(&digest);

    crate::serial_println!(
        "[sign]     niveau       : {} - verdict {}",
        header.signature().as_str(),
        verdict.as_str()
    );
    crate::serial_println!(
        "[sign]     empreinte    : {}...",
        core::str::from_utf8(&hex).unwrap_or("?")
    );

    if let Some(cert) = read_certificate(header, data) {
        crate::serial_println!(
            "[sign]     certificat   : autorite \"{}\" - sujet \"{}\"",
            cert.authority,
            cert.subject
        );
        crate::serial_println!(
            "[sign]     legal        : {} - validite {} a {}",
            cert.legal_id,
            cert.valid_from,
            cert.valid_until
        );
    }

    if let Verdict::Rejected(reason) = verdict {
        crate::serial_println!("[sign]     cause        : {}", reason.message());
    } else if verdict.requires_warning() {
        crate::serial_println!(
            "[sign]     AVERTISSEMENT: {}",
            header.signature().warning_level(header.format())
        );
    }
}
