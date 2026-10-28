//! `libdev` -- the SDK standard library (spec 15.1).
//!
//! ## What it is today: a catalogue, not code
//!
//! 15.1 describes it as the "standard library with high-level
//! abstractions". A library ships as compiled code, under `/libs/` -- and
//! compiling it would take a Rust or C toolchain, which does not exist
//! here (see `sdk::runtime`).
//!
//! What this module brings is the other half, and it is not the lesser
//! one: **the table of the abstractions and of their translation**. A
//! standard library is first of all a contract -- which operations exist,
//! what each one costs in system calls, which permission it demands. That
//! contract is here, and it is checkable: the self-test verifies that
//! every entry names a system call the phase 5 table really knows. An
//! abstraction pointing at a call that does not exist would be false
//! documentation, and a checked catalogue is exactly what avoids that.
//!
//! It is also what an IDE displays (9.4.3; `devstudio.ss` did, and is
//! retired): the developer sees what they may call and what it will
//! demand of their manifest.

use alloc::string::String;
use alloc::vec::Vec;

use crate::binfmt::identity::Permissions;

/// Family of abstractions, for on-screen grouping.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Family {
    Console,
    Files,
    Process,
    Memory,
    Messages,
    Time,
    Network,
    Interface,
}

impl Family {
    pub fn as_str(&self) -> &'static str {
        match self {
            Family::Console => "console",
            Family::Files => "fichiers",
            Family::Process => "processus",
            Family::Memory => "memoire",
            Family::Messages => "messages",
            Family::Time => "temps",
            Family::Network => "reseau",
            Family::Interface => "interface",
        }
    }
}

/// One abstraction offered to the developer.
pub struct Entry {
    pub family: Family,
    /// Name as a program would write it.
    pub name: &'static str,
    /// Number of the underlying system call.
    pub syscall: u16,
    /// Name of that call in the phase 5 table.
    pub syscall_name: &'static str,
    /// Permission from 12.3 that must have been declared. Zero: none.
    pub permission: u64,
    pub summary: &'static str,
}

use crate::syscall::{
    domain_core, domain_fs, domain_ipc, domain_mem, domain_net, domain_proc, domain_time,
};

/// The catalogue.
///
/// The numbers are not hard-coded: they are the constants of the phase 5
/// modules. Renumbering a call drags the catalogue along with it, instead
/// of letting it lie in silence.
pub static CATALOG: &[Entry] = &[
    Entry {
        family: Family::Console,
        name: "trace(texte)",
        syscall: domain_core::SYS_DEBUG_PRINT,
        syscall_name: "SYS_DEBUG_PRINT",
        permission: 0,
        summary: "ecrit une ligne sur la console de diagnostic",
    },
    Entry {
        family: Family::Console,
        name: "derniere_erreur()",
        syscall: domain_core::SYS_ERROR_LAST,
        syscall_name: "SYS_ERROR_LAST",
        permission: 0,
        summary: "rend l'erreur structuree du dernier appel",
    },
    Entry {
        family: Family::Files,
        name: "lire(chemin)",
        syscall: domain_fs::SYS_FS_READ,
        syscall_name: "SYS_FS_READ",
        permission: Permissions::FS_USER_HOME,
        summary: "lit un fichier entier",
    },
    Entry {
        family: Family::Files,
        name: "ecrire(chemin, octets)",
        syscall: domain_fs::SYS_FS_WRITE,
        syscall_name: "SYS_FS_WRITE",
        permission: Permissions::FS_USER_HOME,
        summary: "cree ou remplace un fichier",
    },
    Entry {
        family: Family::Files,
        name: "lister(dossier)",
        syscall: domain_fs::SYS_FS_DIR_ACCESS,
        syscall_name: "SYS_FS_DIR_ACCESS",
        permission: Permissions::FS_USER_HOME,
        summary: "enumere le contenu d'un dossier",
    },
    Entry {
        family: Family::Process,
        name: "lancer(chemin)",
        syscall: domain_proc::SYS_PROC_SPAWN,
        syscall_name: "SYS_PROC_SPAWN",
        permission: 0,
        summary: "charge un binaire et le demarre",
    },
    Entry {
        family: Family::Process,
        name: "attendre(pid)",
        syscall: domain_proc::SYS_PROC_WAIT,
        syscall_name: "SYS_PROC_WAIT",
        permission: 0,
        summary: "bloque jusqu'a la fin d'une tache et rend son code",
    },
    Entry {
        family: Family::Process,
        name: "terminer(code)",
        syscall: domain_proc::SYS_PROC_EXIT,
        syscall_name: "SYS_PROC_EXIT",
        permission: 0,
        summary: "termine la tache courante avec un code",
    },
    Entry {
        family: Family::Memory,
        name: "allouer(octets)",
        syscall: domain_mem::SYS_MEM_ALLOC,
        syscall_name: "SYS_MEM_ALLOC",
        permission: 0,
        summary: "reserve des pages remises a zero",
    },
    Entry {
        family: Family::Memory,
        name: "liberer(base)",
        syscall: domain_mem::SYS_MEM_FREE,
        syscall_name: "SYS_MEM_FREE",
        permission: 0,
        summary: "rend une region allouee",
    },
    Entry {
        family: Family::Messages,
        name: "envoyer(pid, octets)",
        syscall: domain_ipc::SYS_IPC_SEND,
        syscall_name: "SYS_IPC_SEND",
        permission: Permissions::IPC_INTERNAL_PORTS,
        summary: "depose un message dans la boite d'une tache",
    },
    Entry {
        family: Family::Messages,
        name: "recevoir()",
        syscall: domain_ipc::SYS_IPC_RECV,
        syscall_name: "SYS_IPC_RECV",
        permission: 0,
        summary: "retire le prochain message, en bloquant",
    },
    Entry {
        family: Family::Messages,
        name: "publier(canal, octets)",
        syscall: domain_ipc::SYS_IPC_PUBLISH,
        syscall_name: "SYS_IPC_PUBLISH",
        permission: Permissions::IPC_PUBLIC,
        summary: "diffuse a tous les abonnes d'un canal",
    },
    Entry {
        family: Family::Time,
        name: "maintenant_ms()",
        syscall: domain_time::SYS_TIME_NOW_MS,
        syscall_name: "SYS_TIME_NOW_MS",
        permission: 0,
        summary: "horodatage en millisecondes",
    },
    Entry {
        family: Family::Time,
        name: "dormir(ms)",
        syscall: domain_time::SYS_TIME_SLEEP_MS,
        syscall_name: "SYS_TIME_SLEEP_MS",
        permission: 0,
        summary: "suspend la tache pour une duree",
    },
    Entry {
        family: Family::Network,
        name: "connecter(adresse, port)",
        syscall: domain_net::SYS_NET_CONNECT,
        syscall_name: "SYS_NET_CONNECT",
        permission: Permissions::NETWORK_INTERNET,
        summary: "ouvre une connexion TCP",
    },
    Entry {
        family: Family::Network,
        name: "resoudre(nom)",
        syscall: domain_net::SYS_NET_RESOLVE,
        syscall_name: "SYS_NET_RESOLVE",
        permission: Permissions::NETWORK_INTERNET,
        summary: "traduit un nom en adresse",
    },
    Entry {
        family: Family::Interface,
        name: "fenetre(largeur, hauteur)",
        // The graphics protocol is not a system API (7.4): creating a
        // surface goes through a message to the compositor. So we name
        // the call that carries that message, rather than a graphics
        // call that does not exist.
        syscall: domain_ipc::SYS_IPC_SEND,
        syscall_name: "SYS_IPC_SEND vers compositor.ss",
        permission: 0,
        summary: "cree une surface aupres du compositeur (spec 7.4)",
    },
];

/// Entries of one family.
pub fn family(family: Family) -> Vec<&'static Entry> {
    CATALOG
        .iter()
        .filter(|entry| entry.family == family)
        .collect()
}

/// The permissions a set of abstractions demands.
///
/// What a developer's manifest will have to declare before the loader
/// refuses it.
pub fn required_permissions(names: &[&str]) -> u64 {
    CATALOG
        .iter()
        .filter(|entry| names.contains(&entry.name))
        .fold(0, |mask, entry| mask | entry.permission)
}

/// Renders the catalogue as displayable lines.
pub fn lines() -> Vec<String> {
    let mut lines = Vec::new();
    let mut current: Option<Family> = None;

    for entry in CATALOG.iter() {
        if current != Some(entry.family) {
            lines.push(alloc::format!("[{}]", entry.family.as_str()));
            current = Some(entry.family);
        }
        lines.push(alloc::format!(
            "  {:<26} 0x{:04X} {}",
            entry.name,
            entry.syscall,
            entry.syscall_name
        ));
    }
    lines
}
