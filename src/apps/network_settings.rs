//! `reseau.ss` -- the "Reseau" tab of `parametres.ss`.
//!
//! What the network layer of 6.2 exposes, in one place: the card and its
//! link, where the address comes from (DHCP or fixed), the address, mask,
//! gateway and DNS server, a renewal of the lease, and a test that goes as
//! far as a TLS server on the internet.
//!
//! ## Wi-Fi
//!
//! The tab has a Wi-Fi line, and it says what there is: no adapter. QEMU
//! emulates no wireless card -- the guest sees an Ethernet card, and the
//! host puts it on whatever **it** uses, Wi-Fi or cable. A list of
//! networks with a password field would be a mock-up: there is no radio
//! to scan with. On real hardware a Wi-Fi driver would be a driver of
//! its own (firmware, 802.11, WPA2), which does not exist; the line would
//! show it the day it does.
//!
//! ## Who may change it
//!
//! The configuration belongs to the machine, not to an account: an
//! administrator changes it, anyone reads it (5.2: modifying the system
//! is the admin role's).

use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use crate::binfmt::identity::Permissions;
use crate::config::{self, Choice, Kind, Provider, Setting, Value};
use crate::net::{self, Ipv4};

pub const APP_ID: &str = "os.nova.reseau";
pub const FILE_NAME: &str = "reseau.ss";
pub const BINARY: &str = "/sysapp/reseau.ss";

pub struct NetworkTab;

pub static TAB: NetworkTab = NetworkTab;

/// Is `user` an administrator of the machine?
fn is_admin(user: &str) -> bool {
    crate::fs::account_names()
        .iter()
        .any(|(name, role, _)| name == user && matches!(role, crate::fs::perm::Role::Admin | crate::fs::perm::Role::Root))
}

fn apply_and_save(ip: Ipv4, netmask: Ipv4, gateway: Ipv4, dns: Ipv4, dynamic: bool) -> Result<(), String> {
    net::configure(ip, netmask, gateway, dns, dynamic, 0);
    net::save_config().map_err(|error| format!("configuration non enregistree : {}", error.message()))
}

/// Renews the lease in a task of its own: DHCP waits for answers, and the
/// settings window must not freeze meanwhile.
fn renew_task() -> ! {
    match net::dhcp::acquire() {
        Ok(lease) => {
            crate::serial_println!("[reseau] bail renouvele : {} par DHCP", lease.ip.text());
            let _ = net::save_config();
        }
        Err(reason) => crate::serial_println!("[reseau] renouvellement impossible : {}", reason),
    }
    crate::scheduler::exit_current_with(0)
}

/// Resolves a name and opens a TLS session to it: the whole chain a page
/// takes, in one button.
fn connectivity_test() -> Result<String, String> {
    let interface = net::interface();
    if !interface.configured() {
        return Err(String::from("pas d'adresse IP : aucune connexion"));
    }
    let started = crate::idt::ticks();
    let host = "duckduckgo.com";
    let address = crate::apps::browser::net::resolve(host)?;
    let handle = net::tcp::connect(address, 443, 5000).map_err(|error| format!("{} ({}) injoignable : {}", host, address.text(), error))?;
    let connection = crate::apps::browser::net::Connection::from_handle(handle);
    let trust = crate::web::x509::Trust::builtin(&[]);
    let mut random = |buffer: &mut [u8]| crate::random::fill(buffer);
    let now = crate::rtc::now().unwrap_or(0);
    let tls = crate::web::tls::Tls::connect(connection, host, now, &mut random, &trust).map_err(|failure| failure.message)?;
    let elapsed = (crate::idt::ticks() - started) * (1000 / crate::TIMER_HZ as u64);
    Ok(format!(
        "internet joignable : {} ({}) en {} ms - {}, certificat verifie",
        host,
        address.text(),
        elapsed,
        tls.session.suite
    ))
}

fn parse_address(value: &Value) -> Result<Ipv4, String> {
    Ipv4::parse(value.as_text()).ok_or_else(|| format!("\"{}\" n'est pas une adresse IPv4", value.as_text()))
}

impl Provider for NetworkTab {
    fn file(&self) -> &'static str {
        FILE_NAME
    }

    fn tab(&self) -> &'static str {
        "Reseau"
    }

    fn settings(&self, _user: &str) -> Vec<Setting> {
        let mut settings = vec![
            Setting::new("carte", "Ethernet", "Carte reseau filaire vue par le systeme", Kind::Info),
            Setting::new(
                "mode",
                "Adresse",
                "Automatique (DHCP) ou fixee a la main ; reserve aux administrateurs",
                Kind::Select(vec![
                    Choice::new("dhcp", "Automatique (DHCP)"),
                    Choice::new("statique", "Fixe (manuelle)"),
                ]),
            ),
        ];
        for (key, label) in [
            ("ip", "Adresse IP"),
            ("masque", "Masque"),
            ("passerelle", "Passerelle"),
            ("dns", "Serveur DNS"),
        ] {
            settings.push(Setting::new(
                key,
                label,
                "En mode fixe : quatre nombres de 0 a 255, par exemple 10.0.2.15",
                Kind::Field { max: 15 },
            ));
        }
        settings.push(Setting::new(
            "renouveler",
            "Bail DHCP",
            "Redemande une adresse au serveur DHCP",
            Kind::Action("Renouveler"),
        ));
        settings.push(Setting::new(
            "tester",
            "Connexion",
            "Resout duckduckgo.com et ouvre une session TLS verifiee",
            Kind::Action("Tester"),
        ));
        settings.push(Setting::new("trafic", "Trafic", "", Kind::Info));
        settings.push(Setting::new("wifi", "Wi-Fi", "", Kind::Info));
        settings
    }

    fn get(&self, _user: &str, key: &str) -> Value {
        let interface = net::interface();
        match key {
            "carte" => Value::Text(if !interface.attached {
                String::from("aucune carte reseau detectee")
            } else {
                format!(
                    "Intel 82540EM (e1000) - {} - cable {} - {}",
                    net::mac_text(&interface.mac),
                    if crate::drivers::nic::link_up() { "branche" } else { "debranche" },
                    if interface.configured() { "connecte" } else { "sans adresse" }
                )
            }),
            "mode" => Value::Text(String::from(if interface.dynamic || interface.ip.is_unspecified() { "dhcp" } else { "statique" })),
            "ip" => Value::Text(interface.ip.text()),
            "masque" => Value::Text(interface.netmask.text()),
            "passerelle" => Value::Text(interface.gateway.text()),
            "dns" => Value::Text(interface.dns.text()),
            "trafic" => {
                let (frames_in, frames_out, bytes_in, bytes_out, dropped) = net::stats();
                Value::Text(format!(
                    "recu {} Kio ({} trames), envoye {} Kio ({} trames), {} ecartees",
                    bytes_in / 1024,
                    frames_in,
                    bytes_out / 1024,
                    frames_out,
                    dropped
                ))
            }
            "wifi" => Value::Text(String::from(
                "aucun adaptateur Wi-Fi : sous QEMU, la connexion de l'hote (Wi-Fi ou cable) passe par la carte Ethernet",
            )),
            _ => Value::None,
        }
    }

    fn set(&self, user: &str, key: &str, value: &Value) -> Result<(), String> {
        if !is_admin(user) {
            return Err(String::from("reserve aux administrateurs"));
        }
        let interface = net::interface();
        match (key, value) {
            ("mode", Value::Text(mode)) if mode == "dhcp" => {
                crate::scheduler::spawn("reseau-dhcp", crate::process::BinaryType::Ss, renew_task);
                Ok(())
            }
            ("mode", Value::Text(_)) => {
                // Fixed, from the values in force: the fields below then
                // change them one by one.
                let netmask = if interface.netmask.is_unspecified() { Ipv4([255, 255, 255, 0]) } else { interface.netmask };
                apply_and_save(interface.ip, netmask, interface.gateway, interface.dns, false)
            }
            ("ip", value) => {
                let ip = parse_address(value)?;
                apply_and_save(ip, interface.netmask, interface.gateway, interface.dns, false)
            }
            ("masque", value) => {
                let mask = parse_address(value)?;
                // A mask is ones then zeros.
                let bits = mask.to_u32();
                if bits.leading_ones() + bits.trailing_zeros() != 32 {
                    return Err(String::from("masque invalide : des 1 puis des 0"));
                }
                apply_and_save(interface.ip, mask, interface.gateway, interface.dns, false)
            }
            ("passerelle", value) => {
                let gateway = parse_address(value)?;
                apply_and_save(interface.ip, interface.netmask, gateway, interface.dns, false)
            }
            ("dns", value) => {
                let dns = parse_address(value)?;
                net::choose_dns(dns);
                net::save_config().map_err(|error| format!("configuration non enregistree : {}", error.message()))
            }
            _ => Err(String::from("valeur refusee")),
        }
    }

    fn act(&self, user: &str, key: &str) -> Result<String, String> {
        match key {
            "renouveler" => {
                if !is_admin(user) {
                    return Err(String::from("reserve aux administrateurs"));
                }
                if !net::interface().attached {
                    return Err(String::from("aucune carte reseau"));
                }
                crate::scheduler::spawn("reseau-dhcp", crate::process::BinaryType::Ss, renew_task);
                Ok(String::from("demande DHCP envoyee - l'adresse se met a jour dans quelques secondes"))
            }
            "tester" => connectivity_test(),
            _ => Err(String::from("aucune action")),
        }
    }
}

/// Registers the tab. Called at boot.
pub fn register() {
    config::register(&TAB);
}

/// `reseau.ss` from the menu: the settings, opened on this tab.
pub fn task() -> ! {
    super::parametres::run_on(Some(FILE_NAME))
}

pub fn install(now: u64) -> bool {
    super::install_binary(
        &super::BinarySpec {
            path: BINARY,
            title: FILE_NAME,
            app_id: APP_ID,
            display_name: "Reseau",
            format: crate::binfmt::header::BinaryFormat::Ss,
            permissions: Permissions::NETWORK_INTERNET | Permissions::NETWORK_LOCAL | Permissions::HARDWARE,
        },
        now,
    )
}
