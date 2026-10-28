# Nova OS — Instructions de travail

Systeme d'exploitation ecrit integralement en Rust, cible x86_64, bootloader GRUB
via Multiboot2. Architecture microkernel : le noyau ne gere que scheduler, IPC,
memoire et IRQ ; tout le reste (drivers, systeme de fichiers, reseau, GUI) tourne
en espace utilisateur.

---

## 1. Documents de reference — a lire a chaque session

Trois fichiers gouvernent le travail. **Les consulter avant toute action**, sans
exception, y compris pour une correction qui parait triviale.

| Fichier | Role | Quand le lire |
|---|---|---|
| `SPECIFICATION.md` | Specification technique v1.0, 18 sections. Source de verite absolue sur le comportement attendu. | Avant d'implementer une etape : lire la ou les sections citees par la roadmap. |
| `ROADMAP.md` | Plan d'implementation en 10 phases, cases a cocher granulaires. | Au debut de chaque session, pour savoir ou on en est. A mettre a jour a la fin. |
| `DECISIONS.md` | Decisions d'architecture verrouillees + bugs deja resolus. | Avant d'ecrire du code, pour ne pas revenir sur un choix arrete ni reintroduire un bug connu. |

`ARCHITECTURE.md` decrit l'etat courant du code (carte des modules, disposition
memoire, invariants). Le consulter pour comprendre l'existant, le mettre a jour
quand la structure change.

**La specification prime sur toute autre consideration.** Si une implementation
plus elegante contredit la spec, c'est la spec qui gagne. Si la spec parait
incoherente, le signaler a l'utilisateur au lieu de trancher unilateralement.

---

## 2. Deroulement d'une etape

Le projet avance **une etape de roadmap a la fois**, dans l'ordre. Ne jamais
anticiper sur une phase ulterieure ni traiter plusieurs sous-etapes non
consecutives en parallele.

1. **Situer** — lire `ROADMAP.md`, identifier la premiere case non cochee.
2. **Documenter** — lire les sections de `SPECIFICATION.md` citees par
   l'etape, puis `DECISIONS.md`.
3. **Annoncer** — exposer en quelques lignes ce qui va etre fait, les decisions
   de conception non triviales et leurs raisons. Signaler explicitement tout
   point ou la spec laisse le choix.
4. **Implementer** — ecrire le code complet. Voir les conventions ci-dessous.
5. **Compiler** — `make` doit passer **sans erreur ni avertissement**. Un
   avertissement laisse aujourd'hui noiera un vrai probleme demain.
6. **Rendre la main** — indiquer la commande de test (`make run` ou
   `make run-smp`) et **ce qu'il faut observer precisement** dans la sortie
   serie pour considerer l'etape validee.
7. **Valider** — deux voies, et une seule a la fois :
   - **par l'utilisateur** : il execute et renvoie la sortie. C'est le
     defaut. Ne pas cocher la roadmap avant sa confirmation.
   - **par l'assistant, quand l'utilisateur le demande** ("teste bien le
     projet", "mets a jour la roadmap"). QEMU se lance alors sans
     fenetre, sous `timeout`, la serie redirigee vers un fichier :
     `timeout 120 qemu-system-x86_64 ... -display none -serial file:<log>`.
     Le noyau ne rend jamais la main, donc `timeout` est obligatoire et
     son code de sortie 124 est le cas normal. Cocher n'est alors permis
     que sur des lignes **citees** de ce fichier, jamais sur une
     compilation reussie.
8. **Cloturer** — cocher les cases dans `ROADMAP.md`, consigner dans la
   roadmap ce qui a ete observe et sous quelles conditions, mettre a jour
   `ARCHITECTURE.md` si la structure a change, ajouter toute lecon durable
   a `DECISIONS.md`, puis proposer un commit. **Proposer**, pas le faire :
   aucun `git commit` sans demande explicite.

### Ce qu'il faut eprouver, en plus de `make run`

Un verdict vert sous une seule configuration ne prouve qu'une
configuration. Les quatre variantes qui ont deja attrape des bugs :

| Variante | Ce qu'elle revele |
|---|---|
| `make run QEMU_SMP=1` (monocoeur) | ce qui suppose un second coeur sans le dire |
| `make run-smp` (4 coeurs) | courses entre coeurs, invalidation de TLB |
| `make disk-reset` puis deux demarrages | installation idempotente : le second doit **retrouver**, pas recreer |
| `-vga none` | ce qui depend du compositeur sans le dire |
| sans `-device AC97` | un pilote absent doit se declarer absent, pas echouer |

### Ce qui reste ouvert

Toujours nommer ce qui n'est **pas** couvert par l'etape qu'on vient de finir.
Une limite connue et enoncee vaut mieux qu'une fausse impression de completude.
Exemple : a la fin de la phase 2.8, le code ring 3 n'etait pas preemptible — ce
genre de reserve doit etre dit, pas passe sous silence.

---

## 3. Environnement

- **Machine de developpement** : CachyOS (base Arch). Les paquets passent par
  `pacman`, jamais `apt`. Noms specifiques : `libisoburn` fournit `xorriso`,
  le paquet `grub` contient les modules i386-pc (equivalent de `grub-pc-bin`),
  `qemu-desktop` fournit `qemu-system-x86_64`.
- **Toolchain** : Rust nightly via `rustup`, composants `rust-src` et
  `llvm-tools-preview`. NASM pour l'assembleur, `ld.lld` pour l'edition de liens.
- **Cible** : `x86_64-nova_os.json` (target custom a la racine du projet).
- **Emulateur** : QEMU. Le multi-coeur se teste avec `-smp 4`.

### Commandes

```bash
make            # build complet : ASM + Rust + edition de liens + ISO
make kernel     # binaire seul, sans ISO
make run        # QEMU 8 coeurs, 8 Gio, KVM, fenetre + serie (QEMU_SMP=1 : monocoeur)
make run-smp    # QEMU 4 coeurs — obligatoire pour tester le SMP
make run-nogui  # serie seule, sans fenetre
make log        # boot complet redirige vers build/boot.log
make debug      # QEMU en attente sur gdb :1234
make debug-int  # trace des interruptions et resets — diagnostic de triple faute
make check      # ASCII des chaines, symboles non resolus, SSE, segments
make check-ascii # caracteres non-ASCII dans les chaines affichees
make police     # regenere src/graphics/font.rs depuis tools/
make clean
```

Sortie de QEMU avec `-serial stdio` : `Ctrl+A` puis `X`. `Ctrl+C` est capture
par l'emulateur.

---

## 4. Conventions de code

### Langue

- **Code** : identifiants, types, fonctions en anglais (`frame_allocator`,
  `PageTable`, `send_ipi`).
- **Commentaires et documentation dans le code** (`src/`, `boot/`, `tools/`) :
  **en anglais**, ASCII. Cela vaut pour les commentaires de ligne, de bloc
  et la documentation `//!` / `///`.
- **Documents de reference a la racine** (`SPECIFICATION.md`, `ROADMAP.md`,
  `DECISIONS.md`, `ARCHITECTURE.md`) : en francais, accents autorises.
- **Messages de commit** : en francais, sans accents.
- **Chaines affichees a l'ecran ou sur la console serie** : **ASCII imprimable
  uniquement**, plage 0x20 a 0x7E. Le writer VGA remplace tout octet hors de
  cette plage par un carre plein. Pas de tirets cadratins, pas de guillemets
  typographiques, pas d'accents dans un `kprintln!`, `println!` ou
  `serial_println!`. Utiliser `-` et `"`.

### Structure

- **Aucune dependance externe.** Pas de `spin`, `lazy_static`, `x86_64`,
  `bitflags`. Tout est ecrit a la main. C'est un choix arrete : le noyau ne
  contient que du code auditable ligne a ligne. Seules `core`, `alloc` et
  `compiler_builtins` sont recompilees par `build-std`.
- Un module par responsabilite, un fichier par module. Les modules qui
  depassent ~600 lignes se scindent en sous-dossier avec `mod.rs`.
- Chaque module commence par un commentaire `//!` expliquant **ce qu'il fait
  et pourquoi il est concu ainsi**, pas seulement ce qu'il contient.

### Rust bare-metal

- `#![no_std]`, `panic = "abort"`, soft-float, pas de SSE. **Aucun `f32` ou
  `f64` dans le code noyau** tant que SSE n'est pas active dans `CR0`/`CR4`.
- Toute fonction `unsafe` porte une section `# Safety` decrivant les invariants
  que l'appelant doit garantir. Pas d'exception.
- Tout bloc `unsafe` a l'interieur d'une fonction est justifie par un
  commentaire quand la raison n'est pas evidente.
- Les statiques mutables s'adressent via `&raw mut` / `&raw const`, jamais par
  reference directe.
- Les pointeurs de fonction convertis en adresse passent par `as *const ()`
  avant `as u64` (lint `function_casts_as_integer`).
- Le MMIO se lit et s'ecrit avec `read_volatile` / `write_volatile`, et les
  pages correspondantes portent `NO_CACHE`.
- Les structures lues depuis un fichier ou une table firmware sont decodees
  **champ par champ** avec `from_le_bytes`, jamais par transmutation de
  pointeur : rien ne garantit leur alignement.

### Concurrence

- Le `SpinLock` maison masque les interruptions pendant la section critique et
  les restaure au `Drop`. C'est ce qui empeche l'interblocage entre un handler
  d'interruption et le code qu'il interrompt. Ne pas le contourner.
- **Ne jamais detenir un verrou pendant `switch_context`.** Extraire les
  pointeurs sous verrou, relacher, puis commuter.
- Tout handler d'IRQ se termine par `apic::end_of_interrupt()`. Exception :
  le vecteur spurious, qui ne doit surtout pas etre acquitte.
- L'ordre d'initialisation materiel n'est pas negociable :
  PIC masque -> LAPIC -> I/O APIC -> timer calibre -> `sti`.

---

## 5. Interdits

- Cocher une case de roadmap sans sortie serie a l'appui — celle de
  l'utilisateur, ou celle que l'assistant a captee lui-meme quand
  l'utilisateur lui a demande de tester (voir 2.7).
- Introduire une dependance externe.
- Laisser un avertissement de compilation.
- Modifier plusieurs phases a la fois.
- Utiliser des caracteres non-ASCII dans une chaine affichee.
- Revenir sur une decision listee dans `DECISIONS.md` sans en discuter
  d'abord avec l'utilisateur.
- Presenter comme fonctionnel ce qui n'a pas ete valide par une sortie serie.

---

## 6. Style de reponse attendu

- Direct, sans preambule ni recapitulatif de ce qui vient d'etre dit.
- **Fichiers complets** quand une modification touche plus de quelques lignes.
  Les fragments a substituer produisent des duplications (implementations de
  trait en double, signatures desynchronisees) qui coutent plus cher a
  diagnostiquer que le surcout de lecture.
- Expliquer les mecanismes materiels en jeu quand ils ne sont pas evidents :
  pourquoi `sysret` impose l'ordre des descripteurs GDT, pourquoi le trampoline
  SMP doit vivre sous 1 Mio, pourquoi la red zone doit etre desactivee.
- Signaler les erreurs de conception passees quand on en rencontre une, plutot
  que de les contourner silencieusement.
