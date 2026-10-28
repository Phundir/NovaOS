//! The commands of the shell.
//!
//! Each one is a function `(shell, arguments) -> Output`. They call what
//! the system already offers -- `fs`, `scheduler`, `spm::manager`,
//! `sdk::cli` -- and format the answer; none of them reaches around a
//! check of the layer it calls.

use alloc::string::{String, ToString};
use alloc::vec::Vec;

use super::{Output, Shell};
use crate::fs::format::NodeKind;
use crate::fs::perm::{Mode, Permissions};

/// (name, aliases, usage, description) -- what `aide` prints, and what
/// `run` dispatches on.
pub const COMMANDS: &[(&str, &[&str], &str, &str)] = &[
    ("aide", &["help", "?"], "aide [commande]", "liste les commandes, ou l'usage de l'une"),
    ("liste", &["ls", "dir"], "liste [-l] [chemin]", "contenu d'un dossier"),
    ("va", &["cd"], "va [chemin]", "change de dossier courant (sans argument : le dossier personnel)"),
    ("ici", &["pwd"], "ici", "dossier courant"),
    ("lire", &["cat"], "lire <fichier>", "affiche un fichier texte"),
    ("ecrire", &["write"], "ecrire <fichier> <texte...>", "cree ou remplace un fichier texte"),
    ("dossier", &["mkdir"], "dossier <chemin>", "cree un dossier (et ses parents)"),
    ("supprimer", &["rm"], "supprimer [-r] <chemin>", "supprime un fichier, ou un dossier entier avec -r"),
    ("copier", &["cp"], "copier <source> <destination>", "copie (un clone instantane sur LIFS)"),
    ("deplacer", &["mv"], "deplacer <source> <destination>", "deplace ou renomme (un lien reecrit)"),
    ("lier", &["ln"], "lier <source> <destination>", "lien Tag : le meme fichier a deux endroits"),
    ("info", &["stat"], "info <chemin>", "noeud, taille, proprietaire, droits"),
    ("arbre", &["tree"], "arbre [chemin] [profondeur]", "arborescence"),
    ("disques", &["df"], "disques", "volumes montes et espace libre"),
    ("taches", &["ps"], "taches", "processus et taches du noyau"),
    ("tuer", &["kill"], "tuer <pid>", "termine une tache"),
    ("lance", &["run", "open"], "lance <application|binaire>", "lance une application installee ou un binaire Nova"),
    ("memoire", &["free", "mem"], "memoire", "cadres et tas du noyau"),
    ("date", &[], "date", "date et heure locales"),
    ("depuis", &["uptime"], "depuis", "temps depuis le demarrage"),
    ("systeme", &["uname", "version"], "systeme", "version de Nova OS"),
    ("qui", &["whoami"], "qui", "compte et role de la session"),
    ("echo", &[], "echo <texte...>", "affiche le texte"),
    ("historique", &["history"], "historique", "commandes tapees"),
    ("effacer", &["clear", "cls"], "effacer", "efface l'ecran du terminal"),
    ("quitter", &["exit"], "quitter", "ferme le shell"),
    ("spm", &[], "spm <commande>", "paquets et mises a jour (spec 10.3, 10.5)"),
    ("dev", &[], "dev <commande>", "SDK developpeur (spec 15.2)"),
];

fn canonical(word: &str) -> Option<&'static str> {
    COMMANDS
        .iter()
        .find(|(name, aliases, _, _)| *name == word || aliases.contains(&word))
        .map(|(name, _, _, _)| *name)
}

/// Dispatches a line already cut into words.
pub fn run(shell: &mut Shell, line: &str, words: &[String]) -> Output {
    let Some(first) = words.first() else {
        return Output::default();
    };
    let args: Vec<&str> = words[1..].iter().map(String::as_str).collect();
    let now = crate::idt::ticks();
    match canonical(first) {
        Some("aide") => help(&args),
        Some("liste") => list(shell, &args),
        Some("va") => change_dir(shell, &args),
        Some("ici") => Output::line(shell.cwd.clone()),
        Some("lire") => read(shell, &args),
        Some("ecrire") => write(shell, &args, now),
        Some("dossier") => make_dir(shell, &args, now),
        Some("supprimer") => remove(shell, &args, now),
        Some("copier") => two_paths(shell, &args, "copie", |from, to, who| crate::fs::copy(from, to, who, now).map(|_| ())),
        Some("deplacer") => two_paths(shell, &args, "deplace", |from, to, who| crate::fs::move_entry(from, to, who, now).map(|_| ())),
        Some("lier") => two_paths(shell, &args, "lie", |from, to, who| crate::fs::link(from, to, who, now).map(|_| ())),
        Some("info") => info(shell, &args),
        Some("arbre") => tree(shell, &args),
        Some("disques") => disks(),
        Some("taches") => tasks(),
        Some("tuer") => kill(shell, &args),
        Some("lance") => launch(shell, &args),
        Some("memoire") => memory(),
        Some("date") => date(shell),
        Some("depuis") => uptime(),
        Some("systeme") => Output::line(alloc::format!(
            "Nova OS {} ({}) - x86_64 - {} coeur(s)",
            crate::version::VERSION,
            crate::version::COMMIT,
            crate::smp_core_count()
        )),
        Some("qui") => whoami(shell),
        Some("echo") => Output::line(args.join(" ")),
        Some("historique") => Output::lines(
            shell.history.iter().enumerate().map(|(i, l)| alloc::format!("{:4}  {}", i + 1, l)).collect(),
        ),
        Some("effacer") => Output { clear: true, ..Output::default() },
        Some("quitter") => Output { exit: true, ..Output::default() },
        Some("spm") => spm(line, now),
        Some("dev") => dev(shell, line, now),
        _ => Output::error(alloc::format!("{} : commande inconnue - \"aide\" les liste", first)),
    }
}

fn help(args: &[&str]) -> Output {
    if let Some(word) = args.first() {
        return match COMMANDS.iter().find(|(name, aliases, _, _)| name == word || aliases.contains(word)) {
            Some((_, aliases, usage, text)) => {
                let mut lines = alloc::vec![alloc::format!("{} - {}", usage, text)];
                if !aliases.is_empty() {
                    lines.push(alloc::format!("  aussi : {}", aliases.join(", ")));
                }
                Output::lines(lines)
            }
            None => Output::error(alloc::format!("{} : commande inconnue", word)),
        };
    }
    let mut lines = alloc::vec![String::from("Commandes (aide <commande> pour le detail) :")];
    for (name, aliases, _, text) in COMMANDS {
        let names = if aliases.is_empty() {
            String::from(*name)
        } else {
            alloc::format!("{} ({})", name, aliases.join(", "))
        };
        lines.push(alloc::format!("  {:<24} {}", names, text));
    }
    Output::lines(lines)
}

fn fs_error(path: &str, error: crate::fs::FsError) -> Output {
    Output::error(alloc::format!("{} : {}", path, error.message()))
}

/// The 13 rights of spec 5.1 as letters, one mask.
fn rights(mask: Permissions) -> String {
    const LETTERS: [(u64, char); 13] = [
        (Permissions::READ, 'r'),
        (Permissions::WRITE, 'w'),
        (Permissions::EXECUTE, 'x'),
        (Permissions::OVERWRITE, 'o'),
        (Permissions::DELETE, 'd'),
        (Permissions::RENAME, 'n'),
        (Permissions::ACCESS, 'a'),
        (Permissions::LINK, 'l'),
        (Permissions::MOVE, 'm'),
        (Permissions::COPY, 'c'),
        (Permissions::CHMOD, 'h'),
        (Permissions::OWN, 'p'),
        (Permissions::AUDIT, 'u'),
    ];
    LETTERS.iter().map(|(bit, letter)| if mask.contains(*bit) { *letter } else { '-' }).collect()
}

fn size_text(bytes: u64) -> String {
    match bytes {
        0..=9_999 => alloc::format!("{} o", bytes),
        10_000..=9_999_999 => alloc::format!("{} Kio", bytes / 1024),
        _ => alloc::format!("{} Mio", bytes / (1024 * 1024)),
    }
}

fn list(shell: &Shell, args: &[&str]) -> Output {
    let long = args.contains(&"-l");
    let target = args.iter().find(|a| !a.starts_with('-')).copied().unwrap_or(".");
    let path = shell.resolve(target);
    let entries = match crate::fs::list(&path, shell.who) {
        Ok(entries) => entries,
        Err(error) => return fs_error(&path, error),
    };
    let mut lines = Vec::new();
    for entry in entries {
        let child = alloc::format!("{}/{}", path.trim_end_matches('/'), entry.name);
        let node = crate::fs::stat(&child, shell.who).ok();
        let dir = node.as_ref().is_some_and(|n| n.kind == NodeKind::Directory);
        if long {
            let (size, owner) = node.as_ref().map(|n| (n.size, n.owner)).unwrap_or((0, 0));
            let mask = node.as_ref().map(|n| rights(n.perm_owner)).unwrap_or_default();
            lines.push(alloc::format!(
                "{} {} {:>6} {:>9}  {}{}",
                if dir { 'd' } else { '-' },
                mask,
                owner,
                if dir { String::new() } else { size_text(size) },
                entry.name,
                if dir { "/" } else { "" }
            ));
        } else {
            lines.push(alloc::format!("{}{}", entry.name, if dir { "/" } else { "" }));
        }
    }
    if lines.is_empty() {
        lines.push(String::from("(vide)"));
    }
    Output::lines(lines)
}

fn change_dir(shell: &mut Shell, args: &[&str]) -> Output {
    let target = match args.first() {
        Some(path) => shell.resolve(path),
        None => alloc::format!("/users/{}/home", shell.user),
    };
    match crate::fs::stat(&target, shell.who) {
        Ok(node) if node.kind == NodeKind::Directory => {
            // `access` on the directory, as for listing it.
            if let Err(error) = crate::fs::list(&target, shell.who) {
                return fs_error(&target, error);
            }
            shell.cwd = target;
            Output::default()
        }
        Ok(_) => Output::error(alloc::format!("{} : n'est pas un dossier", target)),
        Err(error) => fs_error(&target, error),
    }
}

fn read(shell: &Shell, args: &[&str]) -> Output {
    let Some(target) = args.first() else {
        return Output::error("usage : lire <fichier>");
    };
    let path = shell.resolve(target);
    let bytes = match crate::fs::read_file(&path, shell.who) {
        Ok(bytes) => bytes,
        Err(error) => return fs_error(&path, error),
    };
    const LIMIT: usize = 64 * 1024;
    let shown = &bytes[..bytes.len().min(LIMIT)];
    let binary = shown.iter().filter(|b| **b == 0 || (**b < 0x20 && !b" \t\r\n".contains(b))).count() > shown.len() / 50;
    if binary {
        return Output::line(alloc::format!("{} : fichier binaire ({})", path, size_text(bytes.len() as u64)));
    }
    let text = String::from_utf8_lossy(shown);
    let mut lines: Vec<String> = text.lines().map(ToString::to_string).collect();
    if bytes.len() > LIMIT {
        lines.push(alloc::format!("... ({} de plus)", size_text((bytes.len() - LIMIT) as u64)));
    }
    Output::lines(lines)
}

fn write(shell: &Shell, args: &[&str], now: u64) -> Output {
    let Some(target) = args.first() else {
        return Output::error("usage : ecrire <fichier> <texte...>");
    };
    let path = shell.resolve(target);
    let mut text = args[1..].join(" ");
    text.push('\n');
    match crate::fs::write_file(&path, text.as_bytes(), shell.who, Mode::private_file(), now) {
        Ok(_) => Output::line(alloc::format!("{} : {} ecrit(s)", path, size_text(text.len() as u64))),
        Err(error) => fs_error(&path, error),
    }
}

fn make_dir(shell: &Shell, args: &[&str], now: u64) -> Output {
    let Some(target) = args.first() else {
        return Output::error("usage : dossier <chemin>");
    };
    let path = shell.resolve(target);
    match crate::fs::create_directory(&path, shell.who, Mode::private_dir(), now) {
        Ok(_) => Output::default(),
        Err(error) => fs_error(&path, error),
    }
}

/// Depth-first removal, bounded by the stack like every recursion of the
/// kernel on data from outside.
fn remove_tree(path: &str, who: crate::fs::perm::Credentials, now: u64, count: &mut usize) -> Result<(), crate::fs::FsError> {
    if crate::scheduler::stack_headroom() < 16 * 1024 {
        return Err(crate::fs::FsError::TooLarge);
    }
    if let Ok(node) = crate::fs::stat(path, who)
        && node.kind == NodeKind::Directory
    {
        for entry in crate::fs::list(path, who)? {
            remove_tree(&alloc::format!("{}/{}", path, entry.name), who, now, count)?;
        }
    }
    crate::fs::delete(path, who, now)?;
    *count += 1;
    Ok(())
}

fn remove(shell: &Shell, args: &[&str], now: u64) -> Output {
    let recursive = args.contains(&"-r");
    let Some(target) = args.iter().find(|a| !a.starts_with('-')) else {
        return Output::error("usage : supprimer [-r] <chemin>");
    };
    let path = shell.resolve(target);
    if path == "/" {
        return Output::error("la racine ne se supprime pas");
    }
    if recursive {
        let mut count = 0;
        return match remove_tree(&path, shell.who, now, &mut count) {
            Ok(()) => Output::line(alloc::format!("{} entree(s) supprimee(s)", count)),
            Err(error) => fs_error(&path, error),
        };
    }
    match crate::fs::delete(&path, shell.who, now) {
        Ok(()) => Output::default(),
        Err(error) => fs_error(&path, error),
    }
}

fn two_paths(
    shell: &Shell,
    args: &[&str],
    verb: &str,
    action: impl Fn(&str, &str, crate::fs::perm::Credentials) -> Result<(), crate::fs::FsError>,
) -> Output {
    let (Some(from), Some(to)) = (args.first(), args.get(1)) else {
        return Output::error("deux chemins attendus : source, destination");
    };
    let from = shell.resolve(from);
    let mut to = shell.resolve(to);
    // Into an existing directory: under the same name, as `cp a dir/`.
    if crate::fs::stat(&to, shell.who).is_ok_and(|n| n.kind == NodeKind::Directory) {
        let name = from.rsplit('/').next().unwrap_or("");
        to = alloc::format!("{}/{}", to.trim_end_matches('/'), name);
    }
    match action(&from, &to, shell.who) {
        Ok(()) => Output::line(alloc::format!("{} -> {} ({})", from, to, verb)),
        Err(error) => fs_error(&from, error),
    }
}

fn info(shell: &Shell, args: &[&str]) -> Output {
    let Some(target) = args.first() else {
        return Output::error("usage : info <chemin>");
    };
    let path = shell.resolve(target);
    let node = match crate::fs::stat(&path, shell.who) {
        Ok(node) => node,
        Err(error) => return fs_error(&path, error),
    };
    let owner = crate::fs::account_of(node.owner).map(|(name, _, _)| name).unwrap_or_else(|| alloc::format!("{}", node.owner));
    Output::lines(alloc::vec![
        alloc::format!("{}", path),
        alloc::format!(
            "  noeud {} - {} - {} - {} lien(s)",
            node.id,
            if node.kind == NodeKind::Directory { "dossier" } else { "fichier" },
            size_text(node.size),
            node.links
        ),
        alloc::format!("  proprietaire {} (uid {}), groupe {}", owner, node.owner, node.group),
        alloc::format!(
            "  droits  proprietaire {}  groupe {}  autres {}",
            rights(node.perm_owner),
            rights(node.perm_group),
            rights(node.perm_other)
        ),
        String::from("          r lire w ecrire x executer o ecraser d supprimer n renommer"),
        String::from("          a acceder l lier m deplacer c copier h droits p propriete u audit"),
    ])
}

fn tree(shell: &Shell, args: &[&str]) -> Output {
    let path = shell.resolve(args.first().copied().unwrap_or("."));
    let depth = args.get(1).and_then(|d| d.parse::<usize>().ok()).unwrap_or(3).min(8);
    let mut lines = alloc::vec![path.clone()];
    fn walk(path: &str, who: crate::fs::perm::Credentials, depth: usize, indent: usize, lines: &mut Vec<String>) {
        if depth == 0 || lines.len() > 400 || crate::scheduler::stack_headroom() < 16 * 1024 {
            return;
        }
        let Ok(entries) = crate::fs::list(path, who) else {
            return;
        };
        for entry in entries {
            let child = alloc::format!("{}/{}", path.trim_end_matches('/'), entry.name);
            let dir = crate::fs::stat(&child, who).is_ok_and(|n| n.kind == NodeKind::Directory);
            lines.push(alloc::format!("{}{}{}", " ".repeat(indent * 2 + 2), entry.name, if dir { "/" } else { "" }));
            if dir {
                walk(&child, who, depth - 1, indent + 1, lines);
            }
        }
    }
    walk(&path, shell.who, depth, 0, &mut lines);
    Output::lines(lines)
}

fn disks() -> Output {
    let mut lines = alloc::vec![String::from("lettre  format  libre / total     volume")];
    for disk in crate::fs::disk_list() {
        lines.push(alloc::format!(
            "  {}:     {:<6}  {} / {} Mio   {} ({}, {})",
            disk.letter,
            disk.format,
            disk.free_blocks * 4 / 1024,
            disk.total_blocks * 4 / 1024,
            disk.label,
            disk.device,
            disk.medium
        ));
    }
    Output::lines(lines)
}

fn tasks() -> Output {
    let mut lines = alloc::vec![String::from("  PID  PARENT  TYPE  COEUR  ETAT        NOM")];
    for task in crate::scheduler::task_list() {
        lines.push(alloc::format!(
            "{:>5}  {:>6}  {:<4}  {:>5}  {:<10}  {}",
            task.pid,
            task.parent,
            task.binary_type.as_str(),
            task.cpu,
            alloc::format!("{:?}", task.state),
            task.name
        ));
    }
    Output::lines(lines)
}

fn kill(shell: &Shell, args: &[&str]) -> Output {
    let Some(pid) = args.first().and_then(|p| p.parse::<u64>().ok()) else {
        return Output::error("usage : tuer <pid>");
    };
    let Some(task) = crate::scheduler::task_info(pid) else {
        return Output::error(alloc::format!("{} : aucune tache", pid));
    };
    // Same rule as SYS_PROC_KILL: ROOT and ADMIN act on everything, the
    // others on their own account's tasks.
    let allowed = matches!(shell.who.role, crate::fs::perm::Role::Root | crate::fs::perm::Role::Admin)
        || task.uid == shell.who.uid;
    if !allowed {
        return Output::error(alloc::format!("{} : tache d'un autre compte", pid));
    }
    if crate::scheduler::kill(pid, -1) {
        Output::line(alloc::format!("{} ({}) terminee", pid, task.name))
    } else {
        Output::error(alloc::format!("{} : refus", pid))
    }
}

fn launch(shell: &Shell, args: &[&str]) -> Output {
    let Some(target) = args.first() else {
        return Output::error("usage : lance <application|binaire>");
    };
    let is_binary = ["dsa", "ss", "gs", "cus", "ras"].iter().any(|ext| target.ends_with(&alloc::format!(".{}", ext)));
    if is_binary && target.contains('/') {
        let path = shell.resolve(target);
        return match crate::fs::is_executable(&path, shell.who) {
            Ok(true) => match crate::spm::versions::launch_binary(&path) {
                Ok(pid) => Output::line(alloc::format!("{} lance - processus {}", path, pid)),
                Err(reason) => Output::error(reason),
            },
            Ok(false) => Output::error(alloc::format!("{} : droit d'execution refuse", path)),
            Err(error) => fs_error(&path, error),
        };
    }
    match crate::apps::launch(target) {
        Some(pid) => Output::line(alloc::format!("{} lance - processus {}", target, pid)),
        None => Output::error(alloc::format!("{} : application inconnue (voir \"liste /apps\")", target)),
    }
}

fn memory() -> Output {
    let (total, used, free) = crate::frame::stats();
    let (allocated, heap_free, _, _) = crate::heap::stats();
    Output::lines(alloc::vec![
        alloc::format!(
            "cadres : {} Mio utilises, {} Mio libres sur {} Mio",
            used * 4 / 1024,
            free * 4 / 1024,
            total * 4 / 1024
        ),
        alloc::format!("tas du noyau : {} Kio alloues, {} Kio libres", allocated / 1024, heap_free / 1024),
    ])
}

fn date(shell: &Shell) -> Output {
    let Some(epoch) = crate::rtc::now() else {
        return Output::error("horloge indisponible");
    };
    let settings = crate::i18n::resolve(Some(&shell.user), None);
    let zone = crate::i18n::tz::find(&settings.timezone).unwrap_or_else(crate::i18n::tz::default_zone);
    let local = crate::rtc::DateTime::from_epoch(zone.to_local(epoch)).format();
    Output::line(alloc::format!(
        "{} ({} {})",
        core::str::from_utf8(&local).unwrap_or("?"),
        zone.label,
        zone.offset_text()
    ))
}

fn uptime() -> Output {
    let seconds = crate::idt::ticks() / crate::TIMER_HZ as u64;
    Output::line(alloc::format!(
        "demarre depuis {} h {:02} min {:02} s",
        seconds / 3600,
        (seconds / 60) % 60,
        seconds % 60
    ))
}

fn whoami(shell: &Shell) -> Output {
    Output::line(alloc::format!(
        "{} - uid {} - role {}",
        shell.user,
        shell.who.uid,
        shell.who.role.as_str()
    ))
}

fn spm(line: &str, now: u64) -> Output {
    use crate::spm::manager::Output as Spm;
    match crate::spm::manager::execute(line, now) {
        Ok(Spm::Installed(done)) => Output::line(alloc::format!(
            "{} {} installe dans {} ({})",
            done.app_id,
            crate::spm::db::version_string(&done.version),
            done.prefix,
            done.status.as_str()
        )),
        Ok(Spm::Versioned(done)) => Output::line(alloc::format!(
            "{} {} installe et actif ({}) - precedente {}",
            done.app_id,
            done.version,
            done.root,
            done.replaced.as_deref().unwrap_or("aucune")
        )),
        Ok(Spm::Removed(done)) => Output::line(alloc::format!(
            "{} desinstalle - {} entree(s) supprimee(s), {} conservee(s)",
            done.app_id,
            done.removed_files,
            done.kept_files
        )),
        Ok(Spm::Updates(list)) => {
            if list.is_empty() {
                return Output::line("tout est a jour");
            }
            Output::lines(
                list.iter()
                    .map(|u| {
                        alloc::format!(
                            "{} {} -> {} (depot {}, code {})",
                            u.app_id,
                            crate::spm::db::version_string(&u.installed),
                            crate::spm::db::version_string(&u.available),
                            u.repo,
                            u.code.as_str()
                        )
                    })
                    .collect(),
            )
        }
        Ok(Spm::OsUpdates(count)) => Output::line(alloc::format!("{} mise(s) a jour du systeme deposee(s)", count)),
        Ok(Spm::Results(found)) => {
            if found.is_empty() {
                return Output::line("aucun resultat");
            }
            Output::lines(
                found
                    .iter()
                    .map(|f| {
                        alloc::format!(
                            "{} {} ({}) - depot {}{}",
                            f.app_id,
                            crate::spm::db::version_string(&f.version),
                            f.format.extension(),
                            f.repo,
                            if f.installed { " - installe" } else { "" }
                        )
                    })
                    .collect(),
            )
        }
        Ok(Spm::RepoAdded(name)) => Output::line(alloc::format!("depot {} ajoute", name)),
        Ok(Spm::RepoExists(name)) => Output::line(alloc::format!("{} existe deja", name)),
        Ok(Spm::UpToDate(version)) => Output::line(alloc::format!("deja a jour ({})", version)),
        Ok(Spm::RolledBack(version)) => Output::line(alloc::format!("revenu a la version {}", version)),
        Ok(Spm::RegistryAdded(name)) => Output::line(alloc::format!("registre {} ajoute", name)),
        Ok(Spm::ChannelSet(text)) => Output::line(text),
        Err(error) => Output::error(alloc::format!("spm : {}", error.message())),
    }
}

fn dev(shell: &mut Shell, line: &str, now: u64) -> Output {
    match crate::sdk::cli::execute(line, now) {
        Ok(crate::sdk::cli::Output::Debugging(started)) => {
            // The program waits on its entry: the next lines are the
            // debugger's (16.4).
            if crate::sdk::debug::is_open(started.pid) {
                shell.debugging = Some(started.pid);
            }
            Output::lines(started.lines)
        }
        Ok(output) => Output::lines(output.lines()),
        Err(error) => Output::error(alloc::format!("dev : {}", error.message())),
    }
}
