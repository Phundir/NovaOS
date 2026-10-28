//! 3.3 -- Three-level signature system (spec 3.2).
//!
//! Three levels, three different roots of trust:
//!
//! | Level | Anchor | What verification proves |
//! |---|---|---|
//! | SYSTEM | Digest table frozen in the kernel | The binary is bit for bit the one shipped with the OS |
//! | ENTERPRISE | Certificate attached to a known authority | The publisher is identified and its legal information is readable |
//! | SELF | The digest the binary carries itself | The binary has not been altered since it was produced |
//!
//! ## What this implementation does not do
//!
//! There is **no asymmetric cryptography** here, and therefore no
//! signature in the strict sense. SHA-256 is a hash function: it detects
//! an alteration, it authenticates nobody.
//!
//! Concretely:
//!
//!   * **SYSTEM is solid.** The anchor is the kernel image itself.
//!     Modifying a `.dsa` shipped with the OS changes its digest, which
//!     is no longer in the table: the binary is refused. Forging the
//!     table assumes modifying the kernel, which is another problem
//!     (verified boot, a later phase).
//!   * **SELF is honest.** The spec expects nothing more of it than an
//!     integrity check doubled with a warning at install time.
//!   * **ENTERPRISE is incomplete.** The authority -> binary chain is
//!     established by a hash whose input is public: anyone able to read
//!     the format can forge a valid certificate. The structure, the root
//!     store and the verification walk are in place; what is missing is
//!     the Ed25519 primitive that would make the binding unforgeable.
//!     Reservation recorded in the roadmap.
//!
//! Nothing here may be presented to the user as proof of authenticity
//! until that primitive exists.

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

/// ENTERPRISE roots of trust: digests of the authority keys.
///
/// Same remark as above -- empty by default, fed by the dynamic store for
/// the self-test.
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

/// Declares a key digest as an ENTERPRISE root of trust.
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
    pub authority: String,
    pub subject: String,
    pub legal_id: String,
    pub valid_from: u64,
    pub valid_until: u64,
    pub authority_key: [u8; DIGEST_SIZE],
    pub binding: [u8; DIGEST_SIZE],
}

fn read_cert_name(block: &[u8], offset: usize) -> String {
    let raw = &block[offset..offset + 32];
    let len = raw.iter().position(|&b| b == 0).unwrap_or(32);
    String::from(core::str::from_utf8(&raw[..len]).unwrap_or("?"))
}

/// Decodes the certificate block of a binary, if it carries one.
pub fn read_certificate(header: &BinaryHeader, data: &[u8]) -> Option<Certificate> {
    if !header.has_certificate() {
        return None;
    }
    let base = header.cert_offset as usize;
    if base + CERT_SIZE > data.len() {
        return None;
    }
    let block = &data[base..base + CERT_SIZE];

    if block[..4] != CERT_MAGIC || read_u16(block, cert_offsets::VERSION) != CERT_VERSION {
        return None;
    }

    let mut authority_key = [0u8; DIGEST_SIZE];
    authority_key.copy_from_slice(
        &block[cert_offsets::AUTHORITY_KEY..cert_offsets::AUTHORITY_KEY + DIGEST_SIZE],
    );
    let mut binding = [0u8; DIGEST_SIZE];
    binding.copy_from_slice(&block[cert_offsets::BINDING..cert_offsets::BINDING + DIGEST_SIZE]);

    Some(Certificate {
        authority: read_cert_name(block, cert_offsets::AUTHORITY),
        subject: read_cert_name(block, cert_offsets::SUBJECT),
        legal_id: read_cert_name(block, cert_offsets::LEGAL_ID),
        valid_from: read_u64(block, cert_offsets::VALID_FROM),
        valid_until: read_u64(block, cert_offsets::VALID_UNTIL),
        authority_key,
        binding,
    })
}

/// Expected binding between an authority and a binary.
///
/// `SHA-256(authority key || authenticity digest)`. A certificate is
/// therefore attached to one precise binary: copying it onto another
/// application does not produce a valid binding.
pub fn expected_binding(
    authority_key: &[u8; DIGEST_SIZE],
    digest: &[u8; DIGEST_SIZE],
) -> [u8; DIGEST_SIZE] {
    let mut hasher = sha256::Sha256::new();
    hasher.update(authority_key);
    hasher.update(digest);
    hasher.finish()
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
    /// The certificate does not attach to this binary.
    BindingMismatch,
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
            Reason::BindingMismatch => "le certificat ne se rattache pas a ce binaire",
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

            let block = &data[header.cert_offset as usize..];
            if read_u16(block, cert_offsets::SIGNATURE_TYPE) != SignatureType::Enterprise as u16 {
                return Verdict::Rejected(Reason::CertificateTypeMismatch);
            }
            if !is_known_root(&cert.authority_key) {
                return Verdict::Rejected(Reason::UntrustedAuthority);
            }
            if !sha256::equal(
                &cert.binding,
                &expected_binding(&cert.authority_key, &digest),
            ) {
                return Verdict::Rejected(Reason::BindingMismatch);
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
