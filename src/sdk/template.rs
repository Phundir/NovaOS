//! 9.1.1 -- The five project templates (spec 15.5).
//!
//! ## A template that does not compile is worth nothing
//!
//! 15.5 gives five lines: blank, gui-app, service, game, cli-tool. The
//! obvious trap would be to write five decorative skeletons -- a
//! `dev.toml` and an empty source file. They would pass `dev new` and
//! fail on the first `dev build`.
//!
//! Each of the five therefore produces a project that **assembles, loads
//! and runs**. That is what the self-test checks: `dev new` then
//! `dev build` on all five, and five binaries `binfmt::parse` accepts.
//!
//! ## Why all of them in assembly
//!
//! Because it is the only complete toolchain the SDK owns (see
//! `sdk::runtime`). A `--lang rust` template would write a `main.rs`
//! nothing could compile, and would hand `dev build` a failure on the
//! very first try. Once a Rust runtime is installed under
//! `/sys/runtimes/rust/`, the same templates will follow; until then,
//! promising less and keeping the promise is worth more.
//!
//! ## What each template exercises
//!
//! | Template | Format | What its code does |
//! |---|---|---|
//! | blank | `.cus` | returns code 0 and terminates |
//! | cli-tool | `.cus` | writes to the console, terminates |
//! | gui-app | `.cus` | reads its PID, announces itself, waits, terminates |
//! | service | `.dsa` | message-wait loop, no interface |
//! | game | `.gs` | bounded game loop, yields the CPU every turn |
//!
//! The formats come from 15.5: "service `.dsa` without GUI", "game `.gs`
//! with a game loop". The other two are ordinary applications, hence
//! `.cus`.
//!
//! ## The `service` template produces a binary that will not load
//!
//! And that is correct. 13.2 demands a SYSTEM signature for an assembled
//! `.dsa`, and a SYSTEM signature is only recognised if its digest is in
//! the kernel table -- which only the system image build can do.
//! `dev build` produces the file, `dev run` replays it as a `.ras`
//! (15.4) and **that is how it is tested**. Letting the SDK register its
//! own binary as official would empty the SYSTEM signature of all
//! meaning.

use alloc::string::String;
use alloc::vec::Vec;

use crate::binfmt::header::{BinaryFormat, SignatureType, flags};
use crate::binfmt::identity::{Permissions, Version};

use super::manifest::DevManifest;
use super::runtime::Language;

/// One template from 15.5.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Template {
    Blank,
    GuiApp,
    Service,
    Game,
    CliTool,
}

impl Template {
    /// Name as `dev new --template` accepts it.
    pub fn key(&self) -> &'static str {
        match self {
            Template::Blank => "blank",
            Template::GuiApp => "gui-app",
            Template::Service => "service",
            Template::Game => "game",
            Template::CliTool => "cli-tool",
        }
    }

    /// "Description" column of the 15.5 table.
    pub fn description(&self) -> &'static str {
        match self {
            Template::Blank => "Projet vide",
            Template::GuiApp => "App avec fenetre de base",
            Template::Service => "Service .dsa sans GUI",
            Template::Game => "Jeu .gs avec boucle de jeu",
            Template::CliTool => "Outil CLI sans GUI",
        }
    }

    pub fn from_key(key: &str) -> Option<Template> {
        Some(match key {
            "blank" => Template::Blank,
            "gui-app" | "gui" => Template::GuiApp,
            "service" => Template::Service,
            "game" | "jeu" => Template::Game,
            "cli-tool" | "cli" => Template::CliTool,
            _ => return None,
        })
    }

    /// Binary format imposed by 15.5.
    pub fn format(&self) -> BinaryFormat {
        match self {
            Template::Service => BinaryFormat::Dsa,
            Template::Game => BinaryFormat::Gs,
            Template::Blank | Template::GuiApp | Template::CliTool => BinaryFormat::Cus,
        }
    }

    /// Signature chosen, derived from 3.4.
    ///
    /// A `.dsa` exists only as SYSTEM, a `.gs` as ENTERPRISE or
    /// self-signed. The template takes the least demanding level the
    /// table allows: a brand new project has no certificate authority.
    pub fn signature(&self) -> SignatureType {
        match self {
            Template::Service => SignatureType::System,
            Template::Blank | Template::GuiApp | Template::CliTool | Template::Game => {
                SignatureType::SelfSigned
            }
        }
    }

    fn permissions(&self) -> u64 {
        match self {
            Template::Blank => 0,
            Template::CliTool => 0,
            Template::GuiApp => Permissions::NOTIFICATIONS | Permissions::CLIPBOARD,
            Template::Game => Permissions::AUDIO_OUTPUT | Permissions::FULLSCREEN,
            Template::Service => {
                Permissions::IPC_PUBLIC | Permissions::IPC_INTERNAL_PORTS | Permissions::SYS_DIRECTORY
            }
        }
    }

    fn header_flags(&self) -> u32 {
        match self {
            Template::Blank | Template::CliTool => 0,
            Template::GuiApp => flags::GUI,
            Template::Game => flags::GUI | flags::AUTO_PAUSE | flags::BACKGROUND,
            Template::Service => flags::SERVICE | flags::EXPOSES_API | flags::BACKGROUND,
        }
    }

    fn ports(&self) -> Vec<u16> {
        match self {
            // A service exposes an API: 12.2 wants a declared internal
            // port, without which nobody knows where to reach it.
            Template::Service => alloc::vec![7000],
            _ => Vec::new(),
        }
    }
}

/// The five templates, in the order of the 15.5 table.
pub static ALL: &[Template] = &[
    Template::Blank,
    Template::GuiApp,
    Template::Service,
    Template::Game,
    Template::CliTool,
];

/// One file of the generated project.
pub struct File {
    /// Path relative to the project root.
    pub path: String,
    pub content: String,
}

/// Name of the source file, per template and per language.
///
/// The extension is what `dev interpret` and the compiler dispatch both
/// read, so it is not decoration: a Rust project whose entry point ended
/// in `.asm` would be handed to the assembler.
pub fn entry_name(_template: Template, language: Language) -> &'static str {
    match language {
        Language::Rust => "src/main.rs",
        _ => "src/main.asm",
    }
}

// ============================================================
//  Sources
// ============================================================

/// Common prologue: three comment lines and the entry point.
///
/// The name `_start` is not a decorative convention -- `sdk::asm` uses it
/// to fill the "entry point" field of the header, and refuses to
/// assemble a source that has none.
fn source_blank(app_id: &str) -> String {
    alloc::format!(
        "; {} - gabarit blank\n\
         ; Rend le code 0 et se termine. C'est le programme le plus court\n\
         ; qui soit correct : un binaire qui ne se termine pas laisse une\n\
         ; tache vivante que rien ne recuperera.\n\
         \n\
         .text\n\
         _start:\n\
         \x20   mov rax, 0x020B          ; SYS_PROC_EXIT\n\
         \x20   mov rdi, 0               ; code de sortie\n\
         \x20   syscall\n\
         \x20   hlt                      ; jamais atteint\n",
        app_id
    )
}

fn source_cli(app_id: &str) -> String {
    alloc::format!(
        "; {} - gabarit cli-tool\n\
         ; Ecrit une ligne sur la console puis se termine.\n\
         ;\n\
         ; La longueur du message se calcule par difference d'etiquettes :\n\
         ; la recompter a la main a chaque retouche du texte est\n\
         ; exactement le genre d'erreur qu'un assembleur doit eviter.\n\
         \n\
         .text\n\
         _start:\n\
         \x20   mov rax, 0x0001          ; SYS_DEBUG_PRINT\n\
         \x20   lea rdi, [rip + message]\n\
         \x20   mov rsi, message_fin - message\n\
         \x20   syscall\n\
         \n\
         \x20   mov rax, 0x020B          ; SYS_PROC_EXIT\n\
         \x20   mov rdi, 0\n\
         \x20   syscall\n\
         \x20   hlt\n\
         \n\
         .data\n\
         message:\n\
         \x20   db \"outil en ligne de commande - Nova OS\"\n\
         message_fin:\n",
        app_id
    )
}

fn source_gui(app_id: &str) -> String {
    alloc::format!(
        "; {} - gabarit gui-app\n\
         ; Releve son PID, se declare sur la console, cede la main quelques\n\
         ; tours, puis se termine.\n\
         ;\n\
         ; La creation de fenetre ne passe pas par un appel systeme : le\n\
         ; spec 7.4 la fait transiter par le protocole IPC du compositeur.\n\
         ; C'est libdev qui l'enveloppera ; le drapeau GUI du manifest\n\
         ; declare deja l'intention, et c'est lui que le compositeur lit.\n\
         \n\
         .text\n\
         _start:\n\
         \x20   mov rax, 0x020A          ; SYS_PROC_SELF\n\
         \x20   syscall\n\
         \x20   mov [rip + pid], rax\n\
         \n\
         \x20   mov rax, 0x0001          ; SYS_DEBUG_PRINT\n\
         \x20   lea rdi, [rip + titre]\n\
         \x20   mov rsi, titre_fin - titre\n\
         \x20   syscall\n\
         \n\
         \x20   mov r14, 0\n\
         boucle:\n\
         \x20   mov rax, 0x0208          ; SYS_PROC_YIELD\n\
         \x20   syscall\n\
         \x20   inc r14\n\
         \x20   cmp r14, 8\n\
         \x20   jb boucle\n\
         \n\
         \x20   mov rax, 0x020B          ; SYS_PROC_EXIT\n\
         \x20   mov rdi, 0\n\
         \x20   syscall\n\
         \x20   hlt\n\
         \n\
         .data\n\
         titre:\n\
         \x20   db \"fenetre de base - Nova OS\"\n\
         titre_fin:\n\
         \n\
         .bss\n\
         pid:\n\
         \x20   resb 8\n",
        app_id
    )
}

fn source_service(app_id: &str) -> String {
    alloc::format!(
        "; {} - gabarit service\n\
         ; Service .dsa sans interface : il ouvre son canal, puis boucle\n\
         ; sur l'attente d'un message.\n\
         ;\n\
         ; La boucle est **bornee**. Un service reel tourne indefiniment,\n\
         ; mais un gabarit qui ne se termine jamais laisse une tache\n\
         ; vivante dans chaque test - la lecon des taches de demonstration\n\
         ; de la phase 2, restees en boucle et responsables d'une derive du\n\
         ; tas qu'il a fallu des semaines pour attribuer.\n\
         \n\
         .text\n\
         _start:\n\
         \x20   mov rax, 0x0001          ; SYS_DEBUG_PRINT\n\
         \x20   lea rdi, [rip + banniere]\n\
         \x20   mov rsi, banniere_fin - banniere\n\
         \x20   syscall\n\
         \n\
         \x20   mov r14, 0\n\
         service:\n\
         \x20   mov rax, 0x0706          ; SYS_TIME_SLEEP_MS\n\
         \x20   mov rdi, 20\n\
         \x20   syscall\n\
         \x20   inc r14\n\
         \x20   cmp r14, 16\n\
         \x20   jb service\n\
         \n\
         \x20   mov rax, 0x020B          ; SYS_PROC_EXIT\n\
         \x20   mov rdi, 0\n\
         \x20   syscall\n\
         \x20   hlt\n\
         \n\
         .data\n\
         banniere:\n\
         \x20   db \"service en ligne - Nova OS\"\n\
         banniere_fin:\n",
        app_id
    )
}

fn source_game(app_id: &str) -> String {
    alloc::format!(
        "; {} - gabarit game\n\
         ; Boucle de jeu : etat, mise a jour, cession du CPU.\n\
         ;\n\
         ; La cession a chaque tour n'est pas de la politesse. Un .gs a la\n\
         ; priorite 3 et une tranche de 20 tics en avant-plan (spec 7.3) : une\n\
         ; boucle qui ne cede jamais garde son coeur jusqu'a l'echeance,\n\
         ; parce que le code ring 3 n'est preemptible qu'aux frontieres\n\
         ; d'appel systeme.\n\
         \n\
         .text\n\
         _start:\n\
         \x20   mov rax, 0x0001          ; SYS_DEBUG_PRINT\n\
         \x20   lea rdi, [rip + titre]\n\
         \x20   mov rsi, titre_fin - titre\n\
         \x20   syscall\n\
         \n\
         \x20   mov r15, 0               ; numero d'image\n\
         image:\n\
         \x20   mov rax, 0x0701          ; SYS_TIME_TICKS\n\
         \x20   syscall\n\
         \x20   mov [rip + horloge], rax\n\
         \n\
         \x20   inc r15\n\
         \x20   mov [rip + compteur], r15\n\
         \n\
         \x20   mov rax, 0x0208          ; SYS_PROC_YIELD\n\
         \x20   syscall\n\
         \n\
         \x20   cmp r15, 24\n\
         \x20   jb image\n\
         \n\
         \x20   mov rax, 0x020B          ; SYS_PROC_EXIT\n\
         \x20   mov rdi, 0\n\
         \x20   syscall\n\
         \x20   hlt\n\
         \n\
         .data\n\
         titre:\n\
         \x20   db \"boucle de jeu - Nova OS\"\n\
         titre_fin:\n\
         \n\
         .bss\n\
         horloge:\n\
         \x20   resb 8\n\
         compteur:\n\
         \x20   resb 8\n",
        app_id
    )
}

/// A sample script, dropped by every template.
///
/// It gives `dev interpret` something to run from the very first project;
/// without it the command would only make sense once a developer had
/// written a script themselves.
fn source_script() -> String {
    String::from(
        "# exemple pour dev interpret - Nova Script\n\
         \n\
         fn somme(limite) {\n\
         \x20   let total = 0\n\
         \x20   let index = 1\n\
         \x20   while index <= limite {\n\
         \x20       total = total + index\n\
         \x20       index = index + 1\n\
         \x20   }\n\
         \x20   return total\n\
         }\n\
         \n\
         let resultat = somme(10)\n\
         print(\"somme de 1 a 10 :\", resultat)\n\
         if resultat == 55 {\n\
         \x20   print(\"conforme\")\n\
         } else {\n\
         \x20   print(\"incorrect\")\n\
         }\n",
    )
}

fn source_of(template: Template, app_id: &str, language: Language) -> String {
    if language == Language::Rust {
        return rust_source_of(template, app_id);
    }
    match template {
        Template::Blank => source_blank(app_id),
        Template::CliTool => source_cli(app_id),
        Template::GuiApp => source_gui(app_id),
        Template::Service => source_service(app_id),
        Template::Game => source_game(app_id),
    }
}

// ============================================================
//  9.5.1 -- The same five templates, in Nova Rust
// ============================================================

/// The five sources of 15.5 written in the Rust subset.
///
/// They are not translations of the assembly ones: a template exists to
/// show how the language is written here, and an assembly program
/// transcribed statement by statement would show the assembly. Each one
/// uses what its format actually needs -- the service loops and sleeps,
/// the game counts frames, the CLI tool prints and stops.
///
/// All five **assemble**, and the self-test of 9.1.1 builds every one of
/// them: a template that does not compile is worse than no template.
fn rust_blank(app_id: &str) -> String {
    alloc::format!(
        "// {} - gabarit blank, en Nova Rust\n         //\n         // Le programme le plus court qui soit correct : il se termine.\n         // Un binaire qui ne se termine pas laisse une tache vivante que\n         // rien ne recuperera.\n         \n         fn main() {{\n             println!(\"bonjour depuis {}\");\n         }}\n",
        app_id, app_id
    )
}

fn rust_cli(app_id: &str) -> String {
    alloc::format!(
        "// {} - gabarit cli-tool, en Nova Rust\n         //\n         // Montre ce qu'un outil en ligne de commande fait vraiment :\n         // calculer quelque chose et le dire. La somme est verifiee par\n         // une formule fermee, donc le programme se contredit s'il se\n         // trompe.\n         \n         const LIMITE: i64 = 10;\n         \n         fn somme(jusqu_a: i64) -> i64 {{\n             let mut total = 0;\n             let mut n = 1;\n             while n <= jusqu_a {{\n                 total += n;\n                 n += 1;\n             }}\n             total\n         }}\n         \n         fn main() {{\n             let calcule = somme(LIMITE);\n             let attendu = LIMITE * (LIMITE + 1) / 2;\n             println!(\"somme(1..{{}}) = {{}}\", LIMITE, calcule);\n             if calcule == attendu {{\n                 println!(\"conforme a la formule fermee\");\n             }} else {{\n                 println!(\"ecart detecte - attendu {{}}\", attendu);\n                 exit(1);\n             }}\n         }}\n",
        app_id
    )
}

fn rust_gui(app_id: &str) -> String {
    alloc::format!(
        "// {} - gabarit gui-app, en Nova Rust\n         //\n         // La creation de fenetre ne passe pas par un appel systeme : le\n         // spec 7.4 la fait transiter par le protocole IPC du\n         // compositeur. Le drapeau GUI du manifest declare deja\n         // l'intention, et c'est lui que le compositeur lit.\n         \n         fn main() {{\n             println!(\"fenetre de base - PID {{}}\", pid());\n             let mut tours = 0;\n             while tours < 8 {{\n                 yield_now();\n                 tours += 1;\n             }}\n             println!(\"{{}} tours cedes avant de rendre la main\", tours);\n         }}\n",
        app_id
    )
}

fn rust_service(app_id: &str) -> String {
    alloc::format!(
        "// {} - gabarit service, en Nova Rust\n         //\n         // La boucle est **bornee**. Un service reel tourne indefiniment,\n         // mais un gabarit qui ne se termine jamais laisse une tache\n         // vivante dans chaque test.\n         \n         const TOURS: u64 = 4;\n         \n         fn main() {{\n             println!(\"service en ligne\");\n             let mut tour: u64 = 0;\n             while tour < TOURS {{\n                 sleep_ms(20);\n                 tour += 1;\n             }}\n             println!(\"service arrete apres {{}} tour(s)\", tour);\n         }}\n",
        app_id
    )
}

fn rust_game(app_id: &str) -> String {
    alloc::format!(
        "// {} - gabarit game, en Nova Rust\n         //\n         // Une boucle de jeu est un compteur de trames et une condition\n         // d'arret. Le reste - dessin, entrees - passe par le protocole\n         // graphique, que libdev enveloppera.\n         \n         fn main() {{\n             let mut trame = 0;\n             let mut score = 0;\n             loop {{\n                 trame += 1;\n                 if trame % 3 == 0 {{\n                     score += 10;\n                 }}\n                 if trame >= 12 {{\n                     break;\n                 }}\n                 yield_now();\n             }}\n             println!(\"{{}} trames, score {{}}\", trame, score);\n         }}\n",
        app_id
    )
}

/// The Rust source of one template, for the self-test.
///
/// Public so that 9.1.1 can compile all five without going through the
/// filesystem: a template that does not build must fail the build, not
/// the disk.
pub fn rust_source_for(template: Template, app_id: &str) -> String {
    rust_source_of(template, app_id)
}

fn rust_source_of(template: Template, app_id: &str) -> String {
    match template {
        Template::Blank => rust_blank(app_id),
        Template::CliTool => rust_cli(app_id),
        Template::GuiApp => rust_gui(app_id),
        Template::Service => rust_service(app_id),
        Template::Game => rust_game(app_id),
    }
}

// ============================================================
//  Generation
// ============================================================

/// Builds the manifest of a brand new project.
pub fn manifest_for(
    template: Template,
    app_id: &str,
    author: &str,
    language: Language,
) -> DevManifest {
    let display = app_id.rsplit('.').next().unwrap_or(app_id);
    let group = app_id
        .rsplit_once('.')
        .map(|(prefix, _)| String::from(prefix))
        .unwrap_or_default();

    DevManifest {
        app_id: String::from(app_id),
        display_name: String::from(display),
        author: String::from(author),
        group,
        version: Version::new(0, 1, 0, 1),
        format: template.format(),
        language,
        entry: String::from(entry_name(template, language)),
        signature: template.signature(),
        permissions: crate::binfmt::identity::Permissions(template.permissions()),
        ports: template.ports(),
        depends: Vec::new(),
        flags: template.header_flags(),
        stack_size: 64 * 1024,
        description: String::from(template.description()),
    }
}

/// Every file of a brand new project.
pub fn files(template: Template, app_id: &str, author: &str, language: Language) -> Vec<File> {
    let manifest = manifest_for(template, app_id, author, language);
    alloc::vec![
        File {
            path: String::from(super::manifest::FILE_NAME),
            content: super::manifest::to_toml(&manifest),
        },
        File {
            path: String::from(entry_name(template, language)),
            content: source_of(template, app_id, language),
        },
        File {
            path: String::from("scripts/exemple.ns"),
            content: source_script(),
        },
        File {
            path: String::from("LISEZMOI.txt"),
            content: readme(template, app_id),
        },
    ]
}

fn readme(template: Template, app_id: &str) -> String {
    alloc::format!(
        "{}\n\
         {}\n\
         \n\
         Gabarit : {}\n\
         Format  : .{}\n\
         \n\
         Commandes :\n\
         \x20 dev build             assemble le projet\n\
         \x20 dev build --release   assemble sans symboles de mise au point\n\
         \x20 dev run --debug       execute dans un bac a sable (spec 15.4)\n\
         \x20 dev interpret scripts/exemple.ns\n\
         \x20 dev package --sign self\n",
        app_id,
        "=".repeat(app_id.len().min(64)),
        template.key(),
        template.format().extension()
    )
}
