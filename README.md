<div align="center">

# Nova OS

**Un système d'exploitation complet, écrit en Rust, pour x86_64.**

Microkernel · GRUB / Multiboot2 · SMP · aucune dépendance externe dans le noyau

[![CI](https://github.com/Phundir/NovaOS/actions/workflows/ci.yml/badge.svg)](https://github.com/Phundir/NovaOS/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/Phundir/NovaOS?include_prereleases&label=release)](https://github.com/Phundir/NovaOS/releases)
![Rust nightly](https://img.shields.io/badge/rust-nightly-orange)
![x86_64](https://img.shields.io/badge/arch-x86__64-blue)

</div>

---

## Sommaire

1. [En bref](#en-bref)
2. [Essayer une release](#essayer-une-release)
3. [Construire depuis les sources](#construire-depuis-les-sources)
4. [Commandes](#commandes)
5. [Tester](#tester)
6. [Ce que contient le système](#ce-que-contient-le-système)
7. [Publier une application](#publier-une-application)
8. [Architecture](#architecture)
9. [État d'avancement](#état-davancement)
10. [Versions et publication](#versions-et-publication)
11. [Documents de référence](#documents-de-référence)
12. [Contribuer](#contribuer)
13. [Organisation du dépôt](#organisation-du-dépôt)

---

## En bref

Nova OS est un système d'exploitation écrit **intégralement en Rust**
(`#![no_std]`) et en assembleur NASM pour l'amorçage. Il démarre par GRUB
(Multiboot2), passe en mode long, monte en moitié haute et lance un
**microkernel** qui ne gère que l'ordonnancement, l'IPC, la mémoire et les
interruptions — le reste (pilotes, systèmes de fichiers, réseau, interface
graphique) est organisé en services.

Quelques chiffres :

| | |
|---|---|
| Code Rust | ~173 000 lignes, **zéro dépendance** (seuls `core`, `alloc`, `compiler_builtins`) |
| Autotests au démarrage | ~2 100 vérifications, jugées automatiquement à chaque commit |
| Cœurs | 1 à 8 (SMP, affinité par type de binaire) |
| Démarrage complet sous émulation | ~20 s jusqu'au dernier verdict d'autotest |

Ce qui le distingue :

- **Cinq formats de binaires signés** (`.dsa`, `.ss`, `.gs`, `.cus`, `.ras`)
  avec priorités fixes, bac à sable par type et trois niveaux de signature.
- **LIFS**, son propre système de fichiers : dossiers en arbres B+, clones
  copie-sur-écriture instantanés, liens « Tag », index inverse, journal.
- **Une API système maison** (`SYS_*`, résultats structurés), plus une
  couche POSIX et une **libc Nova** (musl) pour les programmes portés.
- **Une interface graphique complète** : compositeur, modules `.mod`
  remplaçables, fenêtres, barre des tâches, notifications, accessibilité.
- **Une pile graphique GPU propre à Nova** (virtio-gpu 3D, protocole Venus,
  encodeur généré depuis le registre Vulkan) — sans Mesa ni pilote tiers.

---

## Essayer une release

Chaque version publiée sur la page
[Releases](https://github.com/Phundir/NovaOS/releases) contient :

| Fichier | Contenu |
|---|---|
| `nova-os-X.Y.Z.iso` | ISO amorçable (GRUB) |
| `nova-os-X.Y.Z-kernel.bin` | Le noyau seul |
| `nova-os-X.Y.Z-disk.img.xz` | Disque système vierge (GPT + LIFS, 2 Gio) |
| `SHA256SUMS` | Empreintes |

Démarrage le plus simple, sans disque (le système travaille alors sur un
volume en mémoire) :

```bash
qemu-system-x86_64 -cdrom nova-os-X.Y.Z.iso -m 2G -smp 4 -vga virtio -serial stdio
```

Avec le disque système (les fichiers persistent d'un démarrage à l'autre),
le réseau, le son et une tablette USB pour la souris :

```bash
xz -d nova-os-X.Y.Z-disk.img.xz
```

```bash
qemu-system-x86_64 -cdrom nova-os-X.Y.Z.iso -boot d -m 4G -smp 4 \
  -accel kvm -cpu host -vga virtio \
  -drive id=disk,file=nova-os-X.Y.Z-disk.img,format=raw,if=none \
  -device ich9-ahci,id=ahci -device ide-hd,drive=disk,bus=ahci.0 \
  -netdev user,id=net0 -device virtio-net-pci,netdev=net0 \
  -audiodev pa,id=snd0 -device AC97,audiodev=snd0 \
  -device qemu-xhci -device usb-tablet \
  -serial stdio
```

Retirer `-accel kvm -cpu host` sur une machine sans KVM (macOS, Windows,
machine virtuelle) : QEMU émule alors le processeur, plus lentement.
Comptes créés au premier démarrage : `admin`, `nova` et `invite`, dont le
mot de passe initial est le nom du compte.

---

## Construire depuis les sources

### Prérequis

| Outil | Rôle | Arch / CachyOS | Debian / Ubuntu |
|---|---|---|---|
| `rustup` | toolchain nightly épinglée (`rust-toolchain.toml`) | `rustup` | [rustup.rs](https://rustup.rs) |
| NASM | assembleur d'amorçage | `nasm` | `nasm` |
| `ld.lld` | édition de liens | `lld` | `lld` |
| GRUB (i386-pc) | `grub-mkrescue`, `grub-file` | `grub` | `grub-pc-bin grub-common` |
| xorriso, mtools | image ISO | `libisoburn mtools` | `xorriso mtools` |
| QEMU | exécution | `qemu-desktop` | `qemu-system-x86` |
| Python 3 | outils (`tools/`) | `python` | `python3` |

```bash
sudo pacman -S rustup nasm lld grub libisoburn mtools qemu-desktop python
```

```bash
sudo apt install nasm lld grub-pc-bin grub-common xorriso mtools qemu-system-x86 python3
```

La toolchain Rust (nightly datée, `rust-src`, `llvm-tools-preview`) est
installée d'elle-même par `rustup` au premier `make`, d'après
`rust-toolchain.toml`.

### Compiler et lancer

```bash
make            # noyau + ISO dans build/
```

```bash
make run        # QEMU, fenêtre + console série dans le terminal
```

Le premier `make run` crée aussi le disque système (`disks/nova-disk.img`,
GPT + LIFS) et trois supports amovibles vierges pour l'export FAT32, ext4
et NTFS. Ils persistent entre deux lancements ; `make disk-reset` repart
d'un disque neuf.

Sortir de QEMU lancé avec `-serial stdio` : **Ctrl+A** puis **X**.

### Le shell

Nova a un shell (phase 16.0), à trois endroits : l'application
**Terminal** du menu, la **console série** (taper une ligne dans le
terminal de `make run` ou `make run-nogui` suffit à le réveiller), et
l'entrée **recovery** du menu GRUB, qui ne démarre que le noyau, les
services core et ce shell — de quoi réparer une machine dont la session
graphique ne démarre plus.

```text
root:/$ aide
root:/$ liste -l /sys/config
root:/$ ecrire /cache/note.txt "bonjour"
root:/$ lire /cache/note.txt
root:/$ taches
root:/$ spm update
root:/$ dev new org.exemple.essai --type cus --lang rust
```

Les commandes sont en français, avec les noms usuels en alias (`ls`,
`cd`, `cat`, `rm`, `cp`, `mv`, `ps`, `kill`, `df`, `uname`…). Chaque
commande s'exécute avec les droits du compte de la session (ROOT sur la
console de recovery) : le shell n'accorde rien que le système de fichiers
refuserait.

---

## Commandes

| Commande | Effet |
|---|---|
| `make` | Construction complète : assembleur, Rust, édition de liens, ISO |
| `make kernel` | Le noyau seul (`build/nova-os.bin`) |
| `make run` | QEMU, 8 cœurs, KVM si disponible, fenêtre + série |
| `make run QEMU_SMP=1` | Variante monocœur |
| `make run-smp` | QEMU 4 cœurs — la configuration SMP de référence |
| `make run-nogui` | Série seule, sans fenêtre |
| `make run-nonet` | Sans carte réseau ni son : un pilote absent doit se déclarer absent |
| `make log` | Démarrage complet redirigé vers `build/boot.log` |
| `make test` | Démarrage sans fenêtre, verdicts d'autotest jugés automatiquement |
| `make test-recovery` | Démarrage en mode recovery, commandes tapées sur la console série |
| `make test-all` | `test` + monocœur + sans écran + recovery (ce que fait la CI) |
| `make dist` | Ce que publie une release (`build/dist/`) |
| `make check` | Chaînes ASCII, symboles non résolus, instructions SSE, segments |
| `make debug` | QEMU en attente de GDB sur `:1234` |
| `make debug-int` | Trace des interruptions et des resets (triple faute) |
| `make PILE=1` | Noyau qui imprime sa pile d'appels à la panique |
| `make disk-reset` | Disque système neuf (le prochain démarrage recrée l'arborescence) |
| `make check-export` | Vérifie les images exportées avec les outils de l'hôte |
| `make bench-net` | Mesure du débit réseau (fichier servi par l'hôte) |
| `make version` | Version courante (celle de `Cargo.toml`) |
| `make clean` | Efface `build/` et `target/` (jamais les disques) |

Variables utiles : `QEMU_MEM=4G`, `QEMU_SMP=4`, `QEMU_GPU=` (rendu
logiciel), `QEMU_AUDIODEV=none` (sans son), `QEMU_NIC=e1000`.

---

## Tester

Le système s'éprouve lui-même : chaque sous-système lance au démarrage un
autotest qui imprime un **verdict** sur la console série.

```
[test] phase 7.1 validee - 13 verifications du framebuffer
[test] phases 3 et 4 validees - 227 verifications conformes aux specs 3 a 5
[test] phase 5 validee - 282 verifications de l'API systeme (spec 6)
[test] phase 6 validee - 128 verifications de la couche OS
[test] phase 7 validee - 214 verifications du systeme graphique
[test] phase 8 validee - 224 verifications de SPM et des mises a jour (spec 10 et 11)
[test] phase 9 validee - 425 verifications du SDK (spec 13 et 15)
[test] phase 10 validee - 340 verifications des finitions (spec 12, 14 et 16)
[test] phase 13 validee - 37 verifications de la pile graphique, sans GPU (absent)
[test] phase 15 validee - 70 verifications des mises a jour atomiques (spec 10.5)
[test] phase 16.0 validee - 60 verifications du shell et du terminal (spec 1.1, 2.2)
```

`make test` démarre l'ISO sans fenêtre (`tools/boot-test.py`), arrête
QEMU dès que le dernier verdict est imprimé et échoue à la première
panique, au premier `ECHEC` ou `BOOT_ERR_`. Il démarre **deux fois** sur
le même disque : le second démarrage doit retrouver le volume, pas le
recréer. La CI le fait à chaque push, sur quatre cœurs, sur un cœur et
sans écran ; puis elle démarre en mode recovery et tape des commandes sur
la console série (`make test-recovery`), chaque réponse attendue avant la
commande suivante.

Les variantes qui ont déjà attrapé des bugs, et qu'il vaut mieux éprouver
avant de considérer un changement comme fini :

| Variante | Ce qu'elle révèle |
|---|---|
| `make test-smp1` | ce qui suppose un second cœur sans le dire |
| `make test` (4 cœurs) | courses entre cœurs, invalidation de TLB |
| `make test` (deux démarrages) | installation idempotente |
| `make test-novga` | ce qui dépend du compositeur sans le dire |
| `make test-recovery` | ce qui suppose l'interface ou les pilotes sans le dire |
| `make run-nonet` | un pilote absent doit se déclarer absent, pas échouer |

---

## Ce que contient le système

### Noyau

| Domaine | Contenu |
|---|---|
| Amorçage | Multiboot2, passage en mode long, noyau en moitié haute, menu GRUB (normal, recovery, debug, version précédente) |
| Processeur | GDT/TSS par cœur, IDT, `syscall`/`sysret`, SSE en espace utilisateur (FXSAVE par tâche) |
| Interruptions | LAPIC/x2APIC, I/O APIC, MSI-X, minuteur calibré, IPI, invalidation de TLB entre cœurs |
| SMP | jusqu'à 8 cœurs ; cœur 0 réservé aux `.dsa`, moitiés basse et haute selon le type (spec §7.2) |
| Mémoire | allocateur de cadres (bitmap), tas noyau, pagination 4 niveaux, W^X, NX, espaces d'adressage par processus, fautes de page, copie sur écriture |
| Ordonnanceur | préemptif, priorités fixes par type de binaire, tranches §7.3, anti-famine 500 ms |
| IPC | canaux nommés, messages, publication/abonnement, requêtes vers les `.dsa`, ports internes par application |

### Formats et sécurité

| Format | Usage | Priorité |
|---|---|---|
| `.dsa` | Applications système (services) | 1 |
| `.ss` | Logiciels système (explorateur, paramètres) | 2 |
| `.gs` | Jeux (mise en arrière-plan automatique) | 3 |
| `.cus` | Applications courantes | 4 |
| `.ras` | Accès total, isolé de tout, pour les tests | 5 |

Signatures à trois niveaux (SYSTEM, ENTERPRISE en Ed25519, SELF), `AppIdentity`
immuable, permissions déclarées dans l'en-tête, bac à sable par type
(§12.4), invite d'autorisation mémorisable, images liées ELF (§3.5) et
modules de pilote signés `.kmd` (§3.6).

### Stockage

- **AHCI** (SATA), tables **GPT** et **MBR**, une lettre par partition
  (`/`, `A:/`, `B:/`…) ; Nova ne formate jamais ce qu'il ne reconnaît pas.
- **LIFS** (format natif) : nœuds de 64 bits jamais réutilisés, dossiers en
  arbres B+, étendues avec comptes de références, **clone CoW** en temps
  constant, **lien Tag**, index inverse, journal des métadonnées rejoué au
  montage.
- **NovaFS** (volumes historiques), **export** vers FAT32, ext4 et NTFS
  vérifiable par les outils de l'hôte.
- Permissions à treize droits (§5.1) et trois masques, rôles ROOT, ADMIN,
  USER, GUEST, SERVICE, journal d'audit.

### Réseau, audio, périphériques

- Pilotes **virtio-net** (jusqu'à ~1,6 Go/s sous KVM) et **e1000** ;
  Ethernet, ARP, IPv4, ICMP, UDP, **TCP** (émission asynchrone, fenêtre de
  congestion), DHCP, DNS, SNTP.
- **TLS 1.3** (et TLS 1.2 ECDHE + AEAD), X.509, AES-GCM, ChaCha20-Poly1305,
  SHA-2, HMAC, RSA, ECDSA, X25519 — écrits à la main.
- Son **AC'97** : mixeur, WAV, synthèse, rééchantillonnage, capture.
- Clavier et souris **PS/2**, pile **USB xHCI** (souris et tablette HID).

### Interface graphique

- Framebuffer (GOP/VBE) avec write-combining et double tampon, écran
  **virtio-gpu** qui suit la taille de la fenêtre de l'hôte.
- **Compositeur** et modules `.mod` remplaçables : barre des tâches,
  gestionnaire de fenêtres, notifications (cinq niveaux de priorité),
  fond d'écran, lanceur, curseur.
- Écran de connexion avec anti-bruteforce, splash modulaire, thèmes.
- Applications : explorateur de fichiers, bloc-notes, lecteur et
  enregistreur audio, navigateur intégré (HTML/CSS, HTTP, TLS),
  paramètres (son, réseau, horloge, affichage).
- **Accessibilité** : lecteur d'écran avec synthèse vocale, loupe,
  contraste élevé, modes daltonisme, navigation clavier complète — dès
  l'écran de connexion.
- **Internationalisation** : langues, formats de date/heure/nombres, unités,
  fuseaux horaires, dispositions clavier.

### Pile graphique GPU

Cœur GPU dans le noyau (objets par processus, quotas, files, barrières,
délai de garde de 2 s), backend **virtio-gpu 3D / Venus** (Vulkan de
l'hôte), encodeur généré depuis `vk.xml` et vérifié octet par octet,
domaine `SYS_GPU_*`, bibliothèque `nova-gfx` (Rust et C), compositeur sur
le GPU identique au pixel près au rendu processeur. Sans GPU, tout reste
en rendu logiciel.

### Programmes et développement

- **API système** : domaines `SYS_FS_*`, `SYS_PROC_*`, `SYS_MEM_*`,
  `SYS_IPC_*`, `SYS_USER_*`, `SYS_HW_*`, `SYS_TIME_*`, `SYS_NET_*`, plus
  objets ouverts, threads, signaux et GPU pour les programmes portés.
- **POSIX et libc Nova** : musl dont les appels deviennent des appels Nova ;
  un programme C, C++ ou Rust `x86_64-linux-musl` tourne sans un seul appel
  Linux. Sous-système Linux et chargeur PE pour la compatibilité.
- **SPM** : paquets `.spm` signés, dépôts, quatre codes de mise à jour,
  mises à jour de l'OS avec retour arrière automatique.
- **Mises à jour atomiques des applications** : registres de manifestes
  JSON signés (GitHub, serveur ou dossier), canaux `release`/`beta`/`test`,
  chaque version dans son dossier, bascule par un pointeur remplacé en une
  opération, ancienne version gardée tant qu'elle tourne, retour arrière.
- **Shell** : interprète de commandes (fichiers, tâches, mémoire, disques,
  `spm`, `dev`), fenêtre **Terminal** avec historique et défilement, shell
  sur la console série et mode recovery.
- **SDK `dev`** : gabarits, `dev.toml`, bac à sable, assembleur x86_64,
  compilateur d'un sous-ensemble de **Rust** vers x86_64, interprète Nova
  Script, convertisseur Electron → natif (ETR).

---

## Publier une application

Une application se publie dans un **registre** : un dossier servi tel quel
(dépôt GitHub lu par `raw.githubusercontent.com`, GitHub Pages, tout
serveur, ou un dossier local), qui contient un manifeste JSON signé par
application et ses paquets `.spm`.

```bash
tools/novasign.py keygen registre.seed          # une fois : la clé du registre
```

```json
{
  "app_name": "Mon application",
  "app_id": "org.exemple.monapp",
  "current_stable_version": "1.2.0",
  "channels": {
    "release": { "version": "1.2.0", "code": 0, "size": 48213,
                 "checksum": "sha256-<empreinte du .spm>",
                 "download_url": "monapp-1.2.0.spm" }
  }
}
```

```bash
tools/novasign.py sign-manifest registre.seed org.exemple.monapp.json
```

Côté Nova, une fois, dans le Terminal (ou sur la console série) :

```text
spm registry add exemple https://raw.githubusercontent.com/<compte>/<depot>/main <cle publique>
spm install org.exemple.monapp
```

Ensuite `spm update org.exemple.monapp` (ou `updater.dsa`, toutes les
24 h) installe chaque nouvelle version à côté de l'ancienne et bascule
d'un coup ; `spm rollback` revient à la précédente. Détail : spec §10.5.

---

## Architecture

```
┌─────────────────────────────────────────────────────┐
│   Applications   .dsa  .ss  .gs  .cus  .ras         │
├─────────────────────────────────────────────────────┤
│   Sous-systèmes (POSIX / libc Nova, Linux, Windows) │
├─────────────────────────────────────────────────────┤
│   Compositeur + modules .mod      Shell / SDK       │
│   Pilotes        Système de fichiers (LIFS, NovaFS) │
│   Réseau / Audio / IPC applicatif    SPM            │
├─────────────────────────────────────────────────────┤
│   API système (SYS_*)                               │
╞═════════════════════════════════════════════════════╡
│   MICROKERNEL : ordonnanceur, IPC, mémoire, IRQ     │
│   (+ cœur GPU, exception assumée de la spec §1.2)   │
├─────────────────────────────────────────────────────┤
│   GRUB → Multiboot2                                 │
└─────────────────────────────────────────────────────┘
```

Séquence de démarrage : GRUB → `boot.asm` (mode long, moitié haute) →
`kernel_main` (GDT, IDT, pagination, tas, APIC, SMP) → `init.dsa`
(services core puis secondaires) → splash → écran de connexion → bureau.

La carte complète du code — modules, disposition mémoire, vecteurs
d'interruption, appels système, invariants — est dans
[`ARCHITECTURE.md`](ARCHITECTURE.md).

---

## État d'avancement

| Phase | Objet | État |
|---|---|---|
| 1 | Environnement et amorçage | ✅ |
| 2 | Microkernel (pagination, APIC, ordonnanceur, IPC, SMP, syscalls) | ✅ |
| 3 | Formats binaires et signatures | ✅ |
| 4 | Système de fichiers, permissions, export | ✅ |
| 5 | API système | ✅ |
| 6 | Couche OS : pilotes, réseau, audio, services | ✅ |
| 7 | Système graphique | ✅ |
| 8 | SPM et mises à jour de l'OS | ✅ |
| 9 | SDK et langages | 🔄 21 cases sur 27 |
| 10 | Accessibilité, i18n, sécurité, compatibilité | ✅ |
| 11 | Navigateur (fork de Firefox) | ✅ close |
| 12 | Navigateur natif (libc Nova) | 🔧 à valider |
| 13 | Pile graphique Nova et GPU | 🔧 à valider |
| 14 | LIFS et partitions | 🔧 à valider |
| 15 | Mises à jour atomiques des applications, Ed25519 | ✅ |
| 16 | Langages et outils : shell et terminal ✅ ; JavaScript, Python, C, Java, débogueur | 🔄 |
| 17 | Reliquats système de fichiers et POSIX | 📝 |
| 18 | Firefox sur la pile Nova (machine de l'auteur) | ⏸ |

Le détail, case par case, avec ce qui a été observé et sous quelles
conditions, est dans [`ROADMAP.md`](ROADMAP.md) ; les limites connues y
sont nommées phase par phase.

---

## Versions et publication

La version vit à **un seul endroit** : le champ `version` de `Cargo.toml`.
Le noyau l'imprime au démarrage (`Nova OS 0.14.0 (a1b2c3d) booted`), la
donne à `uname` et la compare aux mises à jour proposées par SPM.

Schéma : `0.{phase}.{correctif}` — le chiffre du milieu est la dernière
phase de la roadmap dont le code est complet.

**Publication automatique** (`.github/workflows/release.yml`) : quand un
push change `Cargo.toml` et que le tag `v{version}` n'existe pas encore, la
CI construit la distribution (`make dist`), **démarre l'ISO même qu'elle va
publier** sous QEMU, puis crée le tag et la release GitHub avec les notes
de cette version tirées de [`CHANGELOG.md`](CHANGELOG.md). Une version
suffixée (`0.16.0-rc1`) part en pré-release.

Publier une version :

1. décrire les changements sous `## [X.Y.Z]` dans `CHANGELOG.md` ;
2. changer `version = "X.Y.Z"` dans `Cargo.toml` ;
3. pousser.

---

## Documents de référence

| Fichier | Rôle |
|---|---|
| [`SPECIFICATION.md`](SPECIFICATION.md) | Spécification technique — **la source de vérité** |
| [`ROADMAP.md`](ROADMAP.md) | Plan en phases, cases à cocher, observations et limites |
| [`DECISIONS.md`](DECISIONS.md) | Décisions verrouillées et pièges déjà payés |
| [`ARCHITECTURE.md`](ARCHITECTURE.md) | État courant du code : modules, mémoire, invariants |
| [`CHANGELOG.md`](CHANGELOG.md) | Ce que contient chaque version publiée |
| [`CLAUDE.md`](CLAUDE.md) | Méthode de travail et conventions |
| [`propositions/`](propositions/) | Propositions de l'auteur en cours d'intégration |

---

## Contribuer

Le projet avance **une étape de roadmap à la fois** ; la méthode complète
est dans [`CLAUDE.md`](CLAUDE.md). L'essentiel :

- **La spécification prime.** Une implémentation qui la contredit est un
  bug ; une spécification incohérente se signale, elle ne se tranche pas
  seul.
- **Aucune dépendance externe** dans le noyau, les pilotes et les services.
- **Zéro avertissement** de compilation.
- Code et commentaires **en anglais** ; documents de référence et messages
  de commit **en français** (commits sans accents).
- Chaînes affichées à l'écran ou sur la série : **ASCII imprimable
  uniquement** (`make check-ascii`).
- Toute fonction `unsafe` documente ses invariants (`# Safety`) ; les
  structures lues sur disque se décodent champ par champ.
- Une case de roadmap ne se coche que sur une **sortie série observée**.

---

## Organisation du dépôt

```
NovaOS/
├── src/                 Le noyau et ses services (Rust)
│   ├── lib.rs           kernel_main, orchestration du démarrage
│   ├── scheduler.rs …   microkernel : mémoire, IRQ, SMP, ordonnanceur, IPC
│   ├── syscall/         API système (domaines SYS_*)
│   ├── fs/              stockage : LIFS, NovaFS, partitions, export
│   ├── posix/ linux/    objets POSIX, personnalité Linux
│   ├── drivers/ usb/    pilotes (clavier, souris, réseau, son, xHCI)
│   ├── net/ web/        pile réseau, HTTP, TLS, HTML/CSS
│   ├── gui/ apps/       compositeur, modules, applications
│   ├── graphics/ gpu/   framebuffer, virtio-gpu, pile GPU (Venus)
│   ├── spm/ sdk/        paquets, mises à jour, SDK et compilateurs
│   └── access/ i18n/    accessibilité, internationalisation
├── boot/                Assembleur NASM et script d'édition de liens
├── iso/                 Configuration GRUB
├── tools/               Outils de l'hôte (LIFS, polices, Venus, tests)
├── ports/               Recette du navigateur porté
├── propositions/        Propositions de conception
└── .github/workflows/   CI et publication
```
