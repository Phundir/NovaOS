//! The browser's certificate management: the account's trust store, the
//! certificate viewer, the error page, and the pages that change trust.
//!
//! ## What the user can do
//!
//! * **look** at the certificate of the page shown (`about:certificat`):
//!   the chain, each certificate's subject, issuer, validity, key,
//!   signature and SHA-256 fingerprint, the anchor it ends on, the stapled
//!   OCSP status;
//! * **distrust** an authority compiled into the system, and trust it
//!   again (`about:certificats`);
//! * **add** an authority from a PEM or DER file of their home -- a
//!   company's or a home server's;
//! * **accept** a certificate the checks refused, for one site, knowingly:
//!   the error page shows what was presented and why it was refused; the
//!   exception pins that certificate's fingerprint, so another certificate
//!   on the same site is refused again.
//!
//! ## Where it lives
//!
//! `/users/{user}/settings/navigateur/certificats.txt`, one line each:
//! `mefiance <sha256 of the key>`, `racine <certificate in hex>`,
//! `exception <host> <sha256 of the certificate>`. Read into `TRUST` when
//! the browser starts or the file changes; every connection of the
//! account's jobs uses that store (`net::KernelNet::trust`).

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use crate::vga::SpinLock;
use crate::web::page::escape;
use crate::web::tls::Session;
use crate::web::x509::{self, Certificate, Trust};

/// The account's store, built once per change.
static TRUST: SpinLock<Option<Trust>> = SpinLock::new(None);

#[derive(Debug, Clone, Default)]
pub struct Config {
    pub distrusted: Vec<[u8; 32]>,
    pub roots: Vec<Vec<u8>>,
    pub exceptions: Vec<(String, [u8; 32])>,
}

fn config_file(user: &str) -> String {
    format!("{}/certificats.txt", super::net::settings_directory(user))
}

fn digest(text: &str) -> Option<[u8; 32]> {
    x509::unhex(text)?.try_into().ok()
}

pub fn load() -> Config {
    let mut config = Config::default();
    let Some((user, credentials)) = crate::apps::session_credentials() else {
        return config;
    };
    let Ok(bytes) = crate::fs::read_file(&config_file(&user), credentials) else {
        return config;
    };
    for line in String::from_utf8_lossy(&bytes).lines() {
        let mut words = line.split_whitespace();
        match (words.next(), words.next(), words.next()) {
            (Some("mefiance"), Some(hash), _) => config.distrusted.extend(digest(hash)),
            (Some("racine"), Some(der), _) => config.roots.extend(x509::unhex(der)),
            (Some("exception"), Some(host), Some(hash)) => {
                if let Some(hash) = digest(hash) {
                    config.exceptions.push((String::from(host), hash));
                }
            }
            _ => {}
        }
    }
    config
}

fn save(config: &Config) {
    let Some((user, credentials)) = crate::apps::session_credentials() else {
        return;
    };
    let mut text = String::from("# confiance du navigateur : mefiance <sha256 cle>, racine <der>, exception <hote> <sha256>\n");
    for hash in &config.distrusted {
        text.push_str(&format!("mefiance {}\n", x509::hex(hash)));
    }
    for root in &config.roots {
        text.push_str(&format!("racine {}\n", x509::hex(root)));
    }
    for (host, hash) in &config.exceptions {
        text.push_str(&format!("exception {} {}\n", host, x509::hex(hash)));
    }
    let path = config_file(&user);
    let now = crate::idt::ticks();
    let _ = crate::fs::delete(&path, credentials, now);
    let _ = crate::fs::write_file(&path, text.as_bytes(), credentials, crate::fs::perm::Mode::private_file(), now);
    *TRUST.lock() = None;
}

/// The store every connection of the account uses.
pub fn trust() -> Trust {
    if let Some(trust) = TRUST.lock().as_ref() {
        return trust.clone();
    }
    let config = load();
    let mut trust = Trust::builtin(&config.distrusted);
    for der in &config.roots {
        if let Some(certificate) = Certificate::parse(der) {
            trust.add_root(&certificate);
        }
    }
    trust.exceptions = config.exceptions.clone();
    *TRUST.lock() = Some(trust.clone());
    trust
}

/// Forgets the store: the account changed, or the browser closed.
pub fn reset() {
    *TRUST.lock() = None;
}

// ============================================================
//  Actions
// ============================================================

/// Decodes base64 (RFC 4648), skipping white space.
fn base64(text: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(text.len() * 3 / 4);
    let mut buffer = 0u32;
    let mut bits = 0;
    for byte in text.bytes() {
        let value = match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            b'=' => break,
            b' ' | b'\n' | b'\r' | b'\t' => continue,
            _ => return None,
        };
        buffer = (buffer << 6) | value as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buffer >> bits) as u8);
        }
    }
    Some(out)
}

/// Reads a certificate from a file: PEM (the first one) or DER.
fn read_certificate(bytes: &[u8]) -> Option<Certificate> {
    if let Ok(text) = core::str::from_utf8(bytes)
        && let Some(start) = text.find("-----BEGIN CERTIFICATE-----")
    {
        let body = &text[start + 27..];
        let end = body.find("-----END CERTIFICATE-----")?;
        return Certificate::parse(&base64(&body[..end])?);
    }
    Certificate::parse(bytes)
}

/// Adds an authority from a file of the account's home. Returns a line
/// for the user.
pub fn add_root(relative: &str) -> String {
    let Some((user, credentials)) = crate::apps::session_credentials() else {
        return String::from("aucune session ouverte");
    };
    let path = format!("/users/{}/home/{}", user, relative.trim().trim_start_matches('/'));
    let bytes = match crate::fs::read_file(&path, credentials) {
        Ok(bytes) => bytes,
        Err(error) => return format!("{} : {}", path, error.message()),
    };
    let Some(certificate) = read_certificate(&bytes) else {
        return format!("{} n'est pas un certificat PEM ou DER lisible", path);
    };
    let mut config = load();
    if config.roots.iter().any(|existing| *existing == certificate.der) {
        return String::from("cette autorite est deja dans la liste");
    }
    let name = certificate.subject_text();
    let ca = certificate.is_ca;
    config.roots.push(certificate.der);
    save(&config);
    crate::serial_println!("[navigateur] autorite ajoutee : {}", name);
    if ca {
        format!("Autorite ajoutee : {}", name)
    } else {
        format!("Ajoute, mais ce certificat ne se declare pas autorite (CA) : {}", name)
    }
}

pub fn remove_root(fingerprint: &str) -> String {
    let mut config = load();
    let before = config.roots.len();
    config
        .roots
        .retain(|der| Certificate::parse(der).is_none_or(|certificate| x509::hex(&certificate.fingerprint()) != fingerprint));
    save(&config);
    if config.roots.len() < before { String::from("Autorite retiree.") } else { String::from("Autorite introuvable.") }
}

pub fn distrust(key_hash: &str, trusted: bool) -> String {
    let Some(hash) = digest(key_hash) else {
        return String::from("empreinte invalide");
    };
    let mut config = load();
    config.distrusted.retain(|existing| *existing != hash);
    if !trusted {
        config.distrusted.push(hash);
    }
    save(&config);
    String::from(if trusted { "Confiance retablie." } else { "Cette autorite n'est plus approuvee." })
}

pub fn add_exception(host: &str, fingerprint: &str) -> Result<(), String> {
    let hash = digest(fingerprint).ok_or("empreinte invalide")?;
    let mut config = load();
    config.exceptions.retain(|(existing, _)| !existing.eq_ignore_ascii_case(host));
    config.exceptions.push((host.to_ascii_lowercase(), hash));
    save(&config);
    crate::serial_println!("[navigateur] exception de certificat pour {}", host);
    Ok(())
}

pub fn remove_exception(host: &str) -> String {
    let mut config = load();
    config.exceptions.retain(|(existing, _)| !existing.eq_ignore_ascii_case(host));
    save(&config);
    format!("Exception retiree pour {}.", host)
}

// ============================================================
//  Pages
// ============================================================

fn date(seconds: u64) -> String {
    let datetime = crate::rtc::DateTime::from_epoch(seconds);
    String::from_utf8_lossy(&datetime.format()).into_owned()
}

fn certificate_box(certificate: &Certificate, index: usize, now: u64) -> String {
    let valid = now == 0 || (certificate.not_before <= now && now <= certificate.not_after);
    let role = if index == 0 {
        "Certificat du site"
    } else if certificate.is_self_issued() {
        "Autorite racine"
    } else {
        "Autorite intermediaire"
    };
    let names = if certificate.dns_names.is_empty() {
        String::new()
    } else {
        format!(
            "<tr><td><b>Noms</b></td><td>{}</td></tr>",
            escape(&certificate.dns_names.iter().take(20).cloned().collect::<Vec<String>>().join(", "))
        )
    };
    format!(
        "<div class=\"box\"><h3>{}</h3><table>\
         <tr><td><b>Sujet</b></td><td>{}</td></tr>\
         <tr><td><b>Emetteur</b></td><td>{}</td></tr>{}\
         <tr><td><b>Valide du</b></td><td>{} au {} {}</td></tr>\
         <tr><td><b>Cle</b></td><td>{}</td></tr>\
         <tr><td><b>Signature</b></td><td>{}</td></tr>\
         <tr><td><b>Numero de serie</b></td><td>{}</td></tr>\
         <tr><td><b>Empreinte SHA-256</b></td><td>{}</td></tr>\
         <tr><td><b>Autorite (CA)</b></td><td>{}</td></tr></table></div>",
        role,
        escape(&certificate.subject_text()),
        escape(&certificate.issuer_text()),
        names,
        date(certificate.not_before),
        date(certificate.not_after),
        if valid { "" } else { "<span class=\"error\">(hors de sa periode de validite)</span>" },
        escape(&certificate.key_text()),
        certificate.signature_text(),
        x509::hex_colons(&certificate.serial),
        x509::hex_colons(&certificate.fingerprint()),
        if certificate.is_ca { "oui" } else { "non" },
    )
}

/// `about:certificat`: the certificate of the page last shown in the tab.
pub fn viewer(address: &str, session: Option<&Session>) -> String {
    let now = crate::rtc::now().unwrap_or(0);
    let body = match session {
        None => format!(
            "<h1>Certificat</h1><p>La page {} n'a pas de certificat : elle n'a pas ete chargee en HTTPS.</p>",
            escape(address)
        ),
        Some(session) => {
            let mut body = format!(
                "<h1>Certificat de {}</h1><div class=\"box\"><p><b>Connexion</b> : {} - echange de cles {}</p>\
                 <p><b>Confiance</b> : {}</p><p><b>Revocation</b> : {}</p></div>",
                escape(address),
                session.suite,
                session.group,
                if session.exception {
                    String::from("<span class=\"error\">accepte par une exception que vous avez creee pour ce site</span>")
                } else {
                    format!("chaine verifiee jusqu'a l'autorite {}", escape(&session.anchor))
                },
                escape(&session.revocation.text())
            );
            for (index, certificate) in session.chain.iter().enumerate() {
                body.push_str(&certificate_box(certificate, index, now));
            }
            body
        }
    };
    super::pages::wrap("Certificat", &format!("{}<p><a href=\"about:certificats\">Gerer les autorites et les exceptions</a></p>", body))
}

/// The page shown in place of a site whose certificate was refused.
pub fn error_page(address: &str, host: &str, reason: &str, certificates: &[Certificate]) -> String {
    let now = crate::rtc::now().unwrap_or(0);
    let mut body = format!(
        "<h1>Connexion non securisee</h1><div class=\"box\"><p class=\"error\">{}</p>\
         <p>Le site <b>{}</b> a presente un certificat que le navigateur ne peut pas garantir. \
         Quelqu'un pourrait se faire passer pour lui ou lire ce que vous lui envoyez.</p></div>",
        escape(reason),
        escape(host)
    );
    body.push_str("<p><a href=\"about:accueil\">Revenir en lieu sur</a></p>");
    if let Some(leaf) = certificates.first() {
        body.push_str(&format!(
            "<h2>Continuer malgre tout</h2><p>Seulement si vous savez pourquoi ce certificat est refuse -- votre propre serveur, \
             un reseau d'entreprise. L'exception vaut pour <b>ce</b> certificat sur <b>ce</b> site : \
             s'il change, il sera verifie a nouveau.</p>\
             <p><a href=\"about:action/exception?hote={}&empreinte={}&retour={}\">Accepter ce certificat pour {} et continuer</a></p>",
            crate::web::url::form_encode(host),
            x509::hex(&leaf.fingerprint()),
            crate::web::url::form_encode(address),
            escape(host)
        ));
    }
    body.push_str("<h2>Ce que le serveur a presente</h2>");
    for (index, certificate) in certificates.iter().enumerate() {
        body.push_str(&certificate_box(certificate, index, now));
    }
    super::pages::wrap("Connexion non securisee", &body)
}

/// `about:certificats`: authorities and exceptions.
pub fn management(message: &str) -> String {
    let config = load();
    let mut body = String::from("<h1>Certificats</h1>");
    if !message.is_empty() {
        body.push_str(&format!("<div class=\"box\">{}</div>", escape(message)));
    }
    body.push_str(
        "<h2>Ajouter une autorite</h2><div class=\"box\"><form action=\"about:action/ajouter-racine\">\
         Fichier PEM ou DER dans votre dossier : <input name=\"chemin\" size=\"40\" placeholder=\"documents/mon-autorite.pem\"> \
         <input type=\"submit\" value=\"Ajouter\"></form></div>",
    );
    body.push_str("<h2>Autorites ajoutees</h2>");
    if config.roots.is_empty() {
        body.push_str("<p class=\"dim\">Aucune.</p>");
    }
    for der in &config.roots {
        if let Some(certificate) = Certificate::parse(der) {
            body.push_str(&format!(
                "<p>{} <a href=\"about:action/retirer-racine?empreinte={}\">retirer</a></p>",
                escape(&certificate.subject_text()),
                x509::hex(&certificate.fingerprint())
            ));
        }
    }
    body.push_str("<h2>Exceptions par site</h2>");
    if config.exceptions.is_empty() {
        body.push_str("<p class=\"dim\">Aucune.</p>");
    }
    for (host, hash) in &config.exceptions {
        body.push_str(&format!(
            "<p>{} - certificat {} <a href=\"about:action/retirer-exception?hote={}\">retirer</a></p>",
            escape(host),
            x509::hex_colons(&hash[..8]),
            crate::web::url::form_encode(host)
        ));
    }
    body.push_str("<h2>Autorites du systeme</h2><p class=\"dim\">Les autorites de confiance livrees avec le systeme (magasin de Mozilla).</p><ul>");
    for anchor in crate::web::tls::anchors() {
        let hash = crate::crypto::sha2::sha256(anchor.spki);
        let distrusted = config.distrusted.contains(&hash);
        let name = x509::name_text(anchor.subject);
        let label = if distrusted { format!("<s>{}</s>", escape(&name)) } else { escape(&name) };
        body.push_str(&format!(
            "<li>{} <a href=\"about:action/{}?cle={}\">{}</a></li>",
            label,
            if distrusted { "approuver" } else { "desapprouver" },
            x509::hex(&hash),
            if distrusted { "approuver de nouveau" } else { "ne plus approuver" }
        ));
    }
    body.push_str("</ul>");
    super::pages::wrap("Certificats", &body)
}
