# Nova OS — Architecture du code

État au terme de la phase 15 (mises à jour atomiques des applications),
le 28 octobre 2026. Ce document décrit **ce qui existe**,
pas ce qui est prévu — pour cela, voir `ROADMAP.md`.

À mettre à jour quand la structure change : nouveau module, nouvelle région
mémoire, nouveau vecteur d'interruption.

---

## 1. Arborescence du projet

```
nova-os/
├── CLAUDE.md                  Instructions de travail
├── Makefile                   Build, run, debug, checks
├── Cargo.toml                 crate-type = ["staticlib"]
├── rust-toolchain.toml        nightly épinglée + rust-src + llvm-tools-preview
├── x86_64-nova_os.json        Target custom
├── .cargo/config.toml         build-std, json-target-spec
│
├── boot/                      Assembleur NASM
│   ├── boot.asm               Multiboot2, transition long mode, higher-half
│   ├── linker.ld              Disposition mémoire
│   ├── switch.asm             Commutation de contexte
│   ├── trampoline.asm         Démarrage des cœurs secondaires (16→64 bits)
│   ├── syscall_entry.asm      Point d'entrée syscall/sysret
│   ├── usermode.asm           Programme de démonstration ring 3
│   ├── fault.asm              Faute de page, copies utilisateur, entrée ring 3
│   └── vmtest.asm             Programme d'essai des espaces privés (11.3)
│
├── iso/boot/grub/grub.cfg     Menu GRUB, 4 entrées
│
├── tools/
│   ├── ascii-check.py         Vérifie les chaînes affichées (make check)
│   ├── fs-inspect.py          Lit un volume NovaFS sans le noyau : table de
│   │                          nœuds plate d'un côté, arbre des liens de l'autre
│   ├── nova-fs.py             Écrit le même format depuis l'hôte : format,
│   │                          import d'une arborescence (mise à jour en place
│   │                          si elle y est déjà), put d'un fichier, ls, cat,
│   │                          reprise v1→v2 (volumes NovaFS d'avant la phase 14)
│   ├── boot-test.py           Démarrage sans fenêtre jugé (make test, CI)
│   ├── novasign.py            Ed25519 de l'hôte : clés d'autorité, manifestes
│   ├── lifs.py                LIFS depuis l'hôte : table GPT (create, table),
│   │                          formatage, import, put, ls, cat, check, reprise
│   │                          NovaFS → LIFS (from-novafs) ; délègue à nova-fs.py
│   │                          sur un disque NovaFS entier
│   ├── nova-wrap.py           ELF → image liée .cus (spec 3.5, phase 12)
│   ├── gen-venus.py           Produit src/gpu/venus/protocol.rs depuis le
│   │                          registre Vulkan (make venus, phase 13)
│   ├── venus-check/           Compare cet encodeur, octet par octet, à
│   │                          l'encodeur de référence (make venus-check)
│   ├── nova-kmd.py            ELF → module de pilote signé .kmd (spec 3.6)
│   ├── kmd/                   nova_kmd.h (ABI des pilotes GPU) + module
│   │                          d'essai test-gpu.c (make kmd-test)
│   ├── ports/                 Portage du Web Browser : racine Alpine, image
│   │                          racine, greffon ALSA, essais par QMP, pages
│   │                          d'essai servies par l'hôte (serve-test-pages.sh),
│   │                          micro-banc noyau (tests/sysbench.c)
│   │   ├── nova-libc/         libc Nova : musl d'Alpine + overlay (appels
│   │   │                      Nova), installée à /sys/runtimes/c/libc.so
│   │   ├── nova-gfx/          nova_gfx.h : l'API GPU des programmes (§6.8) ;
│   │   │                      rust/ : la même en crate no_std (13.6.3)
│   │   └── tests/gpu-rust/    Triangle en Rust par nova-gfx (13.6.3)
│   └── generer-police.py      Produit src/graphics/font.rs (make police)
│
├── src/
│   ├── lib.rs                 kernel_main, tâches, orchestration du boot
│   ├── version.rs             Version unique (Cargo.toml), commit du noyau
│   ├── json.rs                JSON borné, sans flottant, forme canonique (15.0.1)
│   │
│   ├── vga.rs                 Writer VGA 80x25 + SpinLock + aiguillage 7.1
│   ├── serial.rs              UART 16550 (COM1)
│   ├── port.rs                inb / outb
│   │
│   ├── gdt.rs                 GDT + TSS par cœur
│   ├── idt.rs                 IDT partagée, 32 exceptions + vecteurs matériels
│   │
│   ├── multiboot.rs           Parsing des tags Multiboot2
│   ├── memory.rs              Tables de pages, traduction, mapping
│   ├── frame.rs               Frame allocator bitmap
│   ├── heap.rs                Heap free-list + global_allocator
│   │
│   ├── pci.rs                 Enumeration du bus PCI (0xCF8/0xCFC)
│   ├── ahci.rs                Pilote AHCI minimal — un port, une commande
│   ├── pic.rs                 Masquage du 8259 legacy
│   ├── apic.rs                LAPIC, timer, IPI
│   ├── ioapic.rs              Routage des IRQ externes
│   ├── acpi.rs                RSDP → RSDT/XSDT → MADT
│   ├── smp.rs                 Démarrage des AP, identité des cœurs
│   │
│   ├── graphics/              Sortie graphique (phase 7.1)
│   │   ├── mod.rs             Init en deux étapes, palette, verrou unique
│   │   ├── framebuffer.rs     Pixels, rectangles, défilement, double tampon
│   │   ├── console.rs         Console texte 127x47 sur framebuffer
│   │   ├── font.rs            Police bitmap 8x16, 95 glyphes, générée
│   │   ├── selftest.rs        Autotest 7.1
│   │   └── virtio_gpu/        L'écran virtio-gpu (2D) et son transport 3D
│   │       ├── mod.rs         Mise en route unique (probe), changement de mode
│   │       ├── transport.rs   Capacités PCI, négociation, files 0 et 1
│   │       ├── queue.rs       Une file virtio : descripteurs indirects,
│   │       │                  requêtes en vol, réponses hors ordre, MSI-X
│   │       ├── exchange.rs    Requêtes attendues verrou relâché (exécution)
│   │       ├── proto.rs       Commandes 2D, 3D et curseur, capset Venus
│   │       ├── context.rs     Contextes, blobs, soumissions gardées (phase 13)
│   │       ├── scanout.rs     Sortie du GPU à l'écran, plan curseur (13.4.4)
│   │       ├── hostmem.rs     Région de mémoire partagée hôte-invité
│   │       └── selftest.rs
│   │
│   ├── gpu/                   Pile graphique Nova (phase 13, spec 8.5)
│   │   ├── mod.rs             Cœur : propriétaires, poignées, quotas, tâche GPU
│   │   ├── contexts.rs        Ouverture et fermeture des contextes, destructions
│   │   │                      différées
│   │   ├── objects.rs         Création, partage et destruction des objets
│   │   ├── work.rs            Soumissions, barrières, délai de garde, balayage
│   │   │                      des propriétaires disparus
│   │   ├── api.rs             SYS_GPU_* (domaine 0C), les deux portes
│   │   ├── backend.rs         Interface de backend (version 0x00010000)
│   │   ├── cmd.rs             Tampon de commandes Nova : format, validation
│   │   ├── spirv.rs           Validation du SPIR-V
│   │   ├── lock.rs            Verrou dormant (SleepLock)
│   │   ├── present.rs         SYS_GPU_PRESENT : relecture GPU, copie dans la
│   │   │                      fenêtre /dev/nova-gui
│   │   ├── screen.rs          L'écran et le curseur au cœur (13.4.4)
│   │   ├── selftest/          Autotest de la phase 13 (programs.rs : C, Rust,
│   │   │                      compositeur)
│   │   ├── shaders/           GLSL + SPIR-V versionné (make shaders)
│   │   ├── module/            Modules .kmd
│   │   │   ├── mod.rs         Conteneur, signature, chargement, démarrage
│   │   │   ├── elf.rs         Chargeur ELF (W^X, relocations relatives)
│   │   │   ├── abi.rs         KernelApi et miroirs repr(C) de nova_kmd.h
│   │   │   ├── context.rs     Adaptateur Context -> table du module
│   │   │   └── test-gpu.kmd   Module d'essai versionné (make kmd-test)
│   │   └── venus/             Backend intégré : Vulkan de l'hôte par Venus
│   │       ├── mod.rs         Session : instance, carte, device, file
│   │       ├── ring.rs        Anneau de commandes (13.5.2)
│   │       ├── backend/       Traduction des objets et commandes Nova
│   │       │   ├── mod.rs     Carte, contexte, mémoire, destruction, relève
│   │       │   ├── objects.rs Création des objets de l'hôte
│   │       │   ├── record.rs  Enregistrement et soumission d'un tampon
│   │       │   └── feedback.rs Mot de fin écrit par le GPU
│   │       ├── cs.rs          Écriture d'un flux, lecture d'une réponse
│   │       └── protocol.rs    Encodeur et décodeurs GÉNÉRÉS (gen-venus.py)
│   │
│   ├── gui/                   Système graphique (phase 7.2 → 7.7)
│   │   ├── mod.rs             Registre de modules, tâche compositeur
│   │   ├── protocol.rs        Protocole IPC GUI (7.4)
│   │   ├── client.rs          Côté appelant, file locale, contre-pression
│   │   ├── surface.rs         Surfaces, couches, z-order (7.2.3)
│   │   ├── painter.rs         Exécution d'une liste avec découpe, sur une
│   │   │                      cible : framebuffer, image, GPU
│   │   ├── gpu/               Le compositeur sur le GPU (13.8) : mod.rs
│   │   │                      moteur, frame.rs une image, record.rs
│   │   │                      instances, drive.rs bascule, suspension,
│   │   │                      plan curseur, selftest.rs égalité au pixel
│   │   ├── compositor.rs      État, requêtes, rendu, routage (7.2.1-7.2.2)
│   │   ├── module.rs          Format .mod, balayage, signatures (7.3)
│   │   ├── modules/           Les six modules natifs du §8.2 (7.3.5)
│   │   │                      cursor.rs : 3 formes en masques 16x16 + point chaud
│   │   │                      Hover / local / want_cursor : socle de survol
│   │   ├── theme.rs           theme.cfg et priorité utilisateur (7.5)
│   │   ├── notify.rs          Les cinq niveaux du §9 (7.6)
│   │   ├── splash.rs          Écran de démarrage modulaire (7.7.3)
│   │   ├── login.rs           Comptes, mots de passe, anti-bruteforce (7.7)
│   │   │                      + Layout : geometrie du panneau, clic, focus, survol
│   │   └── selftest.rs        Autotest phase 7
│   │
│   ├── textbuf.rs             Tampon de texte editable, eprouve (20 cas)
│   ├── apps/
│   │   ├── mod.rs             Registre des identites (12.1) + table NATIVE_APPS
│   │   │                      install_native efface les fichiers retires
│   │   ├── notepad.rs         notepad.cus : bloc-notes, fenetre .cus
│   │   ├── explorer/          explorer.ss : explorateur de fichiers
│   │   ├── parametres.rs      parametres.ss : reglages, onglets
│   │   ├── network_settings.rs  onglet Reseau
│   │   ├── clock_settings.rs  horloge.ss : onglet Date et heure (fuseau, NTP)
│   │   ├── sound_settings.rs  onglet Son
│   │   ├── player.rs          lecteur.cus : lecteur audio
│   │   ├── recorder.rs        magneto.cus : magnetophone
│   │   ├── playback.rs        Lecture WAV via le mixeur
│   │   ├── widgets.rs         Controles partages des applications audio
│   │   ├── window.rs          Boucle commune des fenetres a horloge
│   │   └── browser/           navigateur.cus : le navigateur web
│   │       ├── mod.rs         Fenetre, onglets, barre d'adresse, historique
│   │       ├── net.rs         Connexions, DNS en cache, tache de chargement
│   │       ├── certs.rs       Magasin de certificats par compte
│   │       └── pages.rs       Pages about: internes
│   │
│   ├── web/                   Client web, independant du noyau
│   │   ├── url.rs             URL (sous-ensemble WHATWG)
│   │   ├── http.rs            HTTP/1.1 : 7 methodes, redirections, cookies
│   │   ├── form.rs            Corps de requete : urlencoded, multipart, JSON
│   │   ├── cookies.rs         RFC 6265
│   │   ├── cache.rs           RFC 9111, cache mono-utilisateur
│   │   ├── inflate.rs         DEFLATE, gzip, zlib
│   │   ├── tls.rs             TLS 1.3 + TLS 1.2 ECDHE/AEAD
│   │   ├── der.rs / x509.rs   DER, certificats, verification de chaine
│   │   ├── roots.rs           Ancres de confiance (GENERE)
│   │   ├── text.rs            Jeux de caracteres, entites
│   │   ├── html.rs            Jetons puis arbre
│   │   ├── css.rs / style.rs  Analyse CSS, selecteurs, cascade
│   │   ├── layout.rs          Boites positionnees (CSS 2.1)
│   │   ├── paint.rs           Rendu dans un tampon de pixels
│   │   ├── page.rs            Chaine complete d'une page
│   │   ├── js/                Moteur JavaScript INACHEVE : seuls big.rs et
│   │   │                      num.rs sont compiles ; lexeur, parseur,
│   │   │                      interprete et builtins en attente, pas de DOM
│   │   └── selftest.rs        Autotest hors reseau
│   │
│   ├── vm/                    Espaces d'adressage prives (11.3)
│   │   ├── mod.rs             PML4 par processus, regions, pagination a la
│   │   │                      demande, copie sur ecriture, user_copy
│   │   └── selftest.rs        Programmes ring 3 de boot/vmtest.asm
│   │
│   ├── linux/                 Sous-systeme Linux, seconde version (11.5)
│   │   ├── mod.rs             Processus, threads, registre, fin, recolte
│   │   ├── syscall.rs         Table des appels par numero x86-64
│   │   ├── loader.rs          ELF, PT_INTERP (ld-musl), pile argv/auxv
│   │   ├── process.rs         fork, vfork, execve, wait4/waitid
│   │   ├── task.rs            clone (threads), futex, arch_prctl, rlimits
│   │   ├── mm.rs              mmap, mprotect, mremap, madvise, brk
│   │   ├── files.rs           Appels fichiers (*at, stat, getdents...)
│   │   ├── fd.rs              Descripteurs, descriptions, OpenFile, console
│   │   ├── pipe.rs            Tubes, eventfd
│   │   ├── poll.rs            poll, select, epoll (declenchement par front)
│   │   ├── net.rs             Sockets unix (SCM_RIGHTS) et IPv4 TCP/UDP
│   │   ├── signal.rs          Actions, masques, cadre rt_sigframe
│   │   ├── wait.rs            Files d'attente, echeances
│   │   ├── time.rs            Horloges au TSC etalonne
│   │   ├── audio.rs           /dev/nova-audio : son des programmes Linux vers le mixeur
│   │   ├── gui.rs             /dev/nova-gui : fenetre Nova d'un programme
│   │   ├── misc.rs            uname, getrandom, sysinfo
│   │   ├── selftest.rs        Programmes de /nova-tests
│   │   └── vfs/               Arborescence Linux
│   │       ├── mod.rs         Montages, resolution de chemins
│   │       ├── novafs/        Racine Linux et /home sur des dossiers du volume Nova
│   │       │   ├── mod.rs     Noeuds, lecture/ecriture par plages, cache de pages
│   │       │   └── shared.rs  Projections partagees inscriptibles (MAP_SHARED)
│   │       ├── tmpfs.rs       Fichiers en memoire, memfd
│   │       ├── devfs.rs       /dev
│   │       └── procfs.rs      /proc, /sys, /etc/resolv.conf genere
│   │
│   ├── fpu.rs                 SSE en ring 3, image FXSAVE initiale (11.2)
│   │
│   ├── crypto/                Primitives du client TLS et des signatures
│   │   ├── sha2.rs hmac.rs    SHA-256/384/512, HMAC, HKDF
│   │   ├── aes.rs chacha.rs   AES-GCM, ChaCha20-Poly1305
│   │   ├── x25519.rs ecc.rs   X25519, P-256/P-384 (ECDH, ECDSA)
│   │   ├── field25519.rs      GF(2^255-19), partagé par X25519 et Ed25519
│   │   ├── ed25519.rs         Signatures Ed25519 (RFC 8032) : ENTERPRISE,
│   │   │                      paquets, modules, manifestes (phase 15)
│   │   ├── rsa.rs bignum.rs   Verification RSA PKCS #1 v1.5 et PSS
│   │   └── selftest.rs        Vecteurs publies, au demarrage
│   ├── process.rs             Process, BinaryType, State, affinité
│   ├── scheduler.rs           Ordonnancement multi-cœurs
│   ├── ipc.rs                 Boîtes aux lettres, canaux nommés, §12.1
│   ├── rtc.rs                 Horloge temps réel CMOS (phase 5.6.3)
│   ├── fwcfg.rs               Fichiers fw_cfg de QEMU (opt/nova/*)
│   ├── user.rs                Bascule en ring 3
│   ├── alloc_test.rs          Vérification de la crate alloc
│   │
│   ├── apps.rs                Registre PID → AppIdentity      spec 12.1
│   ├── ports.rs               Ports internes applicatifs      spec 12.2
│   ├── prompt.rs              Invite d'autorisation           spec 12.5
│   ├── init.rs                init.dsa, services core, BOOT_ERR
│   │
│   ├── drivers/               Pilotes (phase 6.1)
│   │   ├── mod.rs             Registre, cycle de vie, vecteurs
│   │   ├── irq.rs             Démultiplexage IRQ → tâche pilote
│   │   ├── ps2.rs             Arbitrage du 8042 clavier/souris
│   │   ├── keyboard.rs        6.1.1 — canal dev.keyboard
│   │   ├── mouse.rs           6.1.2 — canal dev.mouse
│   │   ├── storage.rs         6.1.3 — canal dev.storage
│   │   ├── nic.rs             Carte en service : tâche drv-reseau, canal dev.net
│   │   ├── virtio_net.rs      virtio-net moderne, files scrutées, sondée en premier
│   │   ├── e1000.rs           6.1.4 — Intel 8254x, anneaux DMA (repli)
│   │   ├── audio.rs           6.1.5 — AC'97, liste de descripteurs
│   │   └── selftest.rs        Autotest de la phase 6
│   │
│   ├── net/                   Pile TCP/IP (phase 6.2)
│   │   ├── mod.rs             Interface, somme de contrôle, /net/
│   │   ├── eth.rs             Ethernet II, démultiplexage
│   │   ├── arp.rs             Cache, résolution, prochain saut
│   │   ├── ip.rs              IPv4, pseudo-en-tête, acheminement
│   │   ├── icmp.rs            Echo
│   │   ├── udp.rs             Sockets, ports éphémères
│   │   ├── tcp.rs             Client, machine à états, ACK groupés
│   │   ├── bench.rs           Mesure de débit, sur demande de l'hôte (fw_cfg)
│   │   ├── dhcp.rs            DISCOVER / OFFER / REQUEST / ACK
│   │   ├── dns.rs             Requête A, décompression de noms
│   │   └── sntp.rs            Heure réseau (RFC 4330), service horloge-ntp
│   │
│   ├── audio/mod.rs           Abstraction et mixage (phase 6.3)
│   │
│   ├── syscall/               API système (phase 5)
│   │   ├── mod.rs             MSR, trame, Call, aiguillage, pointeurs
│   │   ├── error.rs           SysResult, SysError structuré, traductions
│   │   ├── table.rs           Table par domaine, modes Sync/Async
│   │   ├── domain_core.rs     0x00 — trace, dernière erreur, description
│   │   ├── domain_fs/         0x01 — système de fichiers      spec 6.2
│   │   │   ├── mod.rs         Appels par chemin, disques, export
│   │   │   └── node.rs        Appels par identifiant de nœud (LIFS, 0x0130–0x0135)
│   │   ├── domain_proc.rs     0x02 — processus                spec 6.3
│   │   ├── domain_mem.rs      0x03 — mémoire                  spec 6.4
│   │   ├── domain_ipc.rs      0x04 — communication            spec 6.5
│   │   ├── domain_user.rs     0x05 — comptes et permissions   spec 6.6
│   │   ├── domain_hw.rs       0x06 — matériel                 spec 6.6
│   │   ├── domain_time.rs     0x07 — temps                    spec 6.6
│   │   ├── domain_net.rs      0x08 — réseau                   spec 6.6
│   │   ├── ported.rs          Programmes portés (libc Nova) : domaines 09,
│   │   │                      0A, 0B et extensions, spec 6.7 (phase 12)
│   │   └── selftest.rs        Autotest de la phase 5
│   │
│   └── binfmt/                Formats binaires (phase 3)
│       ├── mod.rs             parse, check_rules, inspect, autotest complet
│       ├── header.rs          Header 256 o, magic, sections, relocs, certificat
│       ├── identity.rs        AppIdentity, Permissions, Version
│       ├── sha256.rs          SHA-256 (FIPS 180-4), vecteurs de référence
│       ├── signature.rs       Vérification SYSTEM / ENTERPRISE / SELF
│       ├── loader.rs          Arène, mapping, relocations, transfert ring 3
│       └── sample.rs          Générateur de binaires de test
│
│   └── fs/                    Système de fichiers (phases 4 et 14)
│       ├── mod.rs             Table de montage, lettres, API de haut niveau, validation groupée
│       ├── filesystem.rs      Trait Filesystem : ce qu'offre un volume, LIFS ou NovaFS
│       ├── mount.rs           Découverte : tables, partitions, volumes, choix de la racine
│       ├── partition/
│       │   ├── mod.rs         Lecture GPT et MBR, PartitionDevice (fenêtre bornée), WholeDisk
│       │   └── write.rs       Écriture d'une GPT : création, ajout de partition (14.0.4)
│       ├── lifs/              LIFS, format natif (phase 14, spec §4.5)
│       │   ├── mod.rs         Volume : montage, formatage, blocs, comptes de références, nœuds, validation
│       │   ├── format.rs      Superbloc, enregistrement de nœud, entrée, empreinte des noms
│       │   ├── journal.rs     Transaction ouverte, validation en quatre temps, rejeu
│       │   ├── btree.rs       Arbres B+ à clés de 128 bits : la micro-base
│       │   ├── files.rs       Étendues, lecture, écriture avec copie sur écriture, troncature
│       │   ├── dirs.rs        Entrées de dossier (Tag), index inverse
│       │   ├── ops.rs         Opérations par chemin et par identifiant, droits, diagnostic
│       │   ├── check.rs       Vérification complète (fs::check_all)
│       │   └── selftest.rs    Clone de 1 Gio, CoW, Tag, coupures simulées, GPT
│       ├── block.rs           Trait BlockDevice, RamDisk, FrameDisk
│       ├── cache.rs           Cache de blocs, écriture différée, horloge
│       ├── format.rs          NovaFS : superbloc, nœud, entrée de lien
│       ├── volume.rs          NovaFS : opérations sur un volume monté
│       ├── perm.rs            13 permissions, 5 rôles, comptes
│       ├── audit.rs           Journal des accès (ADMIN seul)
│       ├── tree.rs            Arborescence système et utilisateur (§17)
│       ├── selftest.rs        Autotest de la phase 4
│       └── export/
│           ├── mod.rs         Formats de destination, avertissements
│           ├── fat32.rs       Écriture FAT32 avec noms longs
│           ├── ext4.rs        Écriture ext4 : extents, liens durs
│           └── ntfs.rs        Écriture NTFS : MFT, fixups, index B
│
│   ├── usb/                   Pile USB — pointage HID
│   │   ├── mod.rs             Orchestration, découverte PCI, scrutation
│   │   ├── xhci.rs            Contrôleur xHCI : anneaux, contextes, ports
│   │   ├── descriptor.rs      Descripteurs USB, décodés champ par champ
│   │   └── hid.rs             Rapports de pointage, relatif et absolu
│   │
│   ├── spm/                   SPM et mises à jour (phases 8 et 15)
│   │   ├── mod.rs             Arborescence, dépôt local, démarrage
│   │   ├── package.rs         Format .spm (v2), manifest, codes 10.2, niveaux 10.4
│   │   ├── script.rs          Scripts pre/post, confinés au préfixe (8.1.3)
│   │   ├── db.rs              Base des paquets installés, portées, fichiers
│   │   ├── manager.rs         Commandes du §10.3 ; accept / lay_down partagés
│   │   ├── repo.rs            Index de dépôt (phase 8), sceau (8.3)
│   │   ├── fetch.rs           file:, http://, https:// vers la mémoire ou un
│   │   │                      fichier, repris par Range (15.0.4)
│   │   ├── registry.rs        Manifestes JSON signés, canaux, téléchargement
│   │   │                      vérifié (15.2)
│   │   ├── versions.rs        versions/{v}, pointeur current, lancement,
│   │   │                      suivi des processus, nettoyage (15.3-15.5)
│   │   ├── service.rs         spm.dsa : canal spm.update, purge (15.6)
│   │   ├── update.rs          Types §11.1, service updater.dsa (8.4, 15.6.3)
│   │   ├── rollback.rs        Emplacements, validation 60 s, snapshots (8.5)
│   │   ├── selftest.rs        Autotest de la phase 8
│   │   └── selftest_atomic.rs Autotest de la phase 15
│   │
│   ├── sdk/                   SDK et langages (phase 9)
│   │   ├── mod.rs             Arborescence, purge des bacs à sable, démarrage
│   │   ├── runtime.rs         Tableau §13.2, résolution §13.3 (9.5)
│   │   ├── manifest.rs        dev.toml et sa validation croisée (9.2)
│   │   ├── asm.rs             Assembleur x86_64 deux passes (9.5.7)
│   │   ├── emit.rs            Édition de liens et émission Nova (9.1.2)
│   │   ├── script.rs          Interprète Nova Script, borné (9.1.4)
│   │   ├── template.rs        Les cinq gabarits du §15.5 (9.1.1)
│   │   ├── sandbox.rs         Arène, overlay, loopback, rejeu .ras (9.3)
│   │   ├── etr.rs             AST JavaScript, correspondances, seuil 70 % (9.6)
│   │   ├── libdev.rs          Catalogue des abstractions (9.4.3)
│   │   ├── cli.rs             Les six verbes du §15.2 (9.1)
│   │   ├── rustc/             Compilateur et interprète Rust (9.5.1)
│   │   │   ├── mod.rs         Sous-ensemble, appels integres, pipeline
│   │   │   ├── lex.rs         Jetons, lignes portees, refus a vue
│   │   │   ├── ast.rs         L'arbre, lu par les deux back-ends
│   │   │   ├── parse.rs       Descente recursive, escalade de priorite
│   │   │   ├── check.rs       Types, mutabilite, retour certain, emplacements
│   │   │   ├── codegen.rs     Machine a pile -> texte d'assemblage
│   │   │   └── interp.rs      Evaluateur d'arbre (colonne interprete §13.2)
│   │   └── selftest.rs        Autotest de la phase 9
│   │
│   ├── access/                Accessibilité (phase 10.1, spec §16)
│   │   ├── mod.rs             État, raccourcis §16.4, cinq tâches de module
│   │   ├── tree.rs            Arbre parallèle : rôles, focus, panneaux (10.1.1)
│   │   ├── speech.rs          Synthèse à formants en Q12, graphème→phonème (10.1.2)
│   │   ├── reader.rs          screenreader.mod : quoi annoncer, quand (10.1.2)
│   │   ├── zoom.rs            Global, loupe, zone — correspondance inverse (10.1.3)
│   │   ├── contrast.rs        Contraste élevé + les cinq modes du §16.3 (10.1.4)
│   │   ├── keynav.rs          Table du §16.4 en données, focus (10.1.5)
│   │   └── selftest.rs        Autotest de la phase 10.1
│   │
│   ├── i18n/                  Internationalisation (phase 10.2, spec §14)
│   │   ├── mod.rs             Priorité §14.1, rechargement à chaud, réglages
│   │   ├── units.rs           Dates, heures, nombres, monnaie, unités (10.2.3)
│   │   ├── lang.rs            Format .lang, substitution {nom} (10.2.6)
│   │   ├── keyboard.rs        Quatre agencements + touche de bascule (10.2.4)
│   │   ├── tz.rs              Fuseaux, décalage en minutes, UTC (10.2.5)
│   │   └── selftest.rs        Autotest de la phase 10.2
│   │
│   ├── security/              Sécurité avancée (phase 10.3, spec §12)
│   │   ├── mod.rs             Point d'entrée unique : authorize, check_path
│   │   ├── policy.rs          Liste noire / liste blanche, quatre étapes (10.3.3)
│   │   ├── sandbox.rs         Tableau §12.4 appliqué, racine de session (10.3.4)
│   │   ├── consent.rs         Invite graphique modale, case mémoriser (10.3.5)
│   │   └── selftest.rs        Autotest de la phase 10.3
│   │
│   └── compat/                Sous-systèmes de compatibilité (phase 10.4)
│       ├── mod.rs             Personnalité par tâche, conversion, aiguillage
│       ├── elf.rs             Parseur ELF64, champ par champ
│       ├── linux.rs           Traduction des appels Linux, chemins (10.4.1)
│       ├── pe.rs              Parseur PE32+, importations comprises
│       ├── windows.rs         Traduction NT, chemins, poignées (10.4.2)
│       └── selftest.rs        Autotest de la phase 10.4
│
├── SPECIFICATION.md           Spécification technique v1.0
├── ROADMAP.md                 Plan en 10 phases
├── DECISIONS.md               Décisions verrouillées + pièges connus
└── ARCHITECTURE.md            Ce fichier
```

---

## 2. Séquence de démarrage

```
GRUB
 └─ _start                     (boot.asm, 32 bits, section .boot à 1 MiB)
     ├─ vérification du magic Multiboot2 (EAX)
     ├─ EBX → EDI (pointeur d'info, deviendra RDI)
     ├─ checks CPUID / long mode / pages 1 GiB
     ├─ construction des tables de pages
     │   ├─ PML4[0]   identity temporaire
     │   ├─ PML4[256] fenêtre physique, 4 × 1 GiB
     │   └─ PML4[511] kernel higher-half, 512 × 2 MiB
     ├─ PAE + EFER.LME + CR0.PG
     └─ long_mode_low → higher_half_entry
         ├─ pile kernel définitive
         ├─ suppression de PML4[0]
         └─ kernel_main(rdi)   (lib.rs)
             ├─ serial::init
             ├─ multiboot::parse
             ├─ gdt::init / idt::init
             ├─ frame::init / heap::init
             ├─ init_interrupts : pic → apic → ioapic → timer → sti
             ├─ syscall::init(0)
             ├─ acpi::parse → smp::start_all
             │   └─ pour chaque AP : trampoline → ap_entry
             │       ├─ apic::init, LAPIC_TO_CPU
             │       ├─ gdt::init_cpu, idt::load
             │       ├─ syscall::init(cpu_id)
             │       └─ attente de release_scheduler
             ├─ binfmt::selftest
             ├─ spawn_tasks
             ├─ init::task (tache « init.dsa »)
             ├─ smp::release_scheduler
             └─ scheduler::start()      ← ne revient jamais
```

Une fois l'ordonnanceur lancé, la suite du §2.1 se déroule dans des
tâches, plus dans `kernel_main` :

```
init.dsa
 ├─ fs.dsa / ipc.dsa / hw.dsa          canaux sys.fs, sys.ipc, sys.hw
 ├─ attente <= 30 s                    sinon BOOT_ERR_0x0020..0x0040
 ├─ drivers::start_all                 clavier, souris, stockage, reseau, audio
 │   └─ chaque pilote : registre -> materiel -> canal -> pret | absent
 ├─ tache « reseau »                   /net/interface.cfg, sinon DHCP
 └─ boucle d'adoption                  recupere les taches mortes orphelines
```

---

## 3. Disposition mémoire

### Virtuelle

| Base | Taille | Contenu | Pages |
|---|---|---|---|
| `0x8000` | ~200 o | Trampoline SMP (physique) | — |
| `0x400000` | variable | Code du programme lié statiquement (USER, RX) | 4 KiB |
| `0x800000` | 16 KiB | Pile de ce programme (USER, RW, NX) | 4 KiB |
| `0x1000000` | 32 × 2 MiB | Arène des images chargées (3.2) | 4 KiB |
| `0x10000000` | 32 × 1 MiB | Arène des piles utilisateur (RW, NX, page de garde) | 4 KiB |
| `0x20000000` | 256 MiB | Arène de `SYS_MEM_ALLOC` (5.4) | 4 KiB |
| `0x30000000` | 128 MiB | Arène des bacs à sable du SDK (9.3.1) | 4 KiB |
| `0xFFFF800000000000` | 4 GiB, puis toute la RAM (≤ 64 GiB, `memory::extend_window`) | Fenêtre mémoire physique | 1 GiB |
| `0xFFFF808000000000` | 16 MiB | Heap kernel | 4 KiB |
| `0xFFFF810000000000` | taille du mode | Framebuffer matériel, write-combining | 4 KiB |
| `0xFFFF818000000000` | taille du mode | Tampon arrière, write-back, NX | 4 KiB |
| `0xFFFF820000000000` | quelques pages | Registres virtio-net, non cachés (BAR possiblement > 4 Gio) | 4 KiB |
| `0xFFFF830000000000` | taille de la région (8 Gio) | Vue noyau de la région partagée virtio-gpu : seuls les blobs que le noyau lit y sont présents (flux de réponse Venus, mémoire des contextes noyau) | 4 KiB |
| `0xFFFF840000000000` | ≤ 16 Mio par module | Modules de pilotes .kmd : code RX, données RW NX, relro en lecture seule | 4 KiB |
| `0xFFFF848000000000` | variable | Registres que les modules projettent, non cachés | 4 KiB |
| `0xFFFFFFFF80000000` | variable | Kernel (text, rodata, data, bss) | 2 MiB |

### Physique réservée

- `0x00000` – `0x100000` : mémoire basse entière (IVT, BDA, EBDA, VGA, ROM,
  trampoline SMP)
- `__boot_start` – `__kernel_end` : le kernel lui-même
- Structure d'information Multiboot2

---

## 4. GDT

Ordre imposé par `sysret` — voir `DECISIONS.md` § piège 10.

| Sélecteur | Contenu |
|---|---|
| `0x00` | Nul |
| `0x08` | Code noyau, ring 0, 64 bits |
| `0x10` | Données noyau, ring 0 |
| `0x18` | Code 32 bits ring 3 — inutilisé, requis par l'arithmétique de `sysret` |
| `0x20` | Données ring 3 |
| `0x28` | Code 64 bits ring 3 |
| `0x30` | TSS (deux emplacements) |

Une GDT et un TSS **par cœur**, indexés par `cpu_id`. `IST[0]` porte la pile de
double faute, `RSP0` la pile des transitions ring 3 → ring 0.

### Pile noyau : une par tâche, pas une par cœur

`TSS.RSP0` et `PerCpu.kernel_rsp` sont réécrits **à chaque commutation** par
`scheduler::install_kernel_stack`, avec le sommet de la pile de la tâche
entrante. Deux tâches ring 3 sur le même cœur ne peuvent donc pas s'écraser
mutuellement leur trame d'appel système quand l'une cède le CPU au milieu d'un
`syscall`.

Une tâche en ring 3 n'a rien d'utile sur sa pile noyau : le gestionnaire la
vide entièrement avant de rendre la main. Repartir du sommet à chaque entrée est
donc correct.

**Ces piles font 128 Kio (`process::STACK_SIZE`) et n'ont pas de page de
garde.** Ce sont 32 frames physiquement contiguës
(`frame::allocate_contiguous`, `process::KernelStack`), atteintes par la
fenêtre physique, qui est mappée de bout en bout : déborder n'écrit pas
dans le vide et ne lève aucune faute, cela écrase la frame voisine --
la pile d'une autre tâche, une page d'un programme, une table. Elles
venaient du tas jusqu'au 23 septembre 2026 ; les ~300 threads du
navigateur y prenaient 37 Mio sur 64. Le système ne s'arrête que bien plus tard, sur
une adresse de retour corrompue, loin de la cause et souvent dans une autre
tâche. Le récit complet est dans `DECISIONS.md`, « Compter des niveaux ne
borne pas une pile ».

Tant qu'il n'y a pas de page de garde, la mesure remplace la protection :

| Fonction | Rend |
|---|---|
| `scheduler::current_stack_bottom()` | adresse la plus basse de la pile courante, 0 hors tâche |
| `scheduler::stack_pointer()` | `rsp`, lecture pure d'un registre |
| `scheduler::stack_headroom()` | octets restants, `usize::MAX` hors tâche |

Aucune ne prend de verrou : la borne vient de `PerCpu.kernel_rsp`, que
`install_kernel_stack` publie à chaque commutation. **Tout code noyau dont
la profondeur de récursion dépend d'une donnée extérieure — un script, un
fichier, une trame réseau — les interroge avant de descendre d'un cran.**
`sdk::script::Interpreter` en est le premier client : il relève son
plancher une fois à la construction et le compare à chaque pas.

### Convention de GS

`swapgs` ne sait rien du niveau de privilège : la correction du mécanisme tient
entièrement à une convention, vraie à **tout** instant.

| Niveau | `GS.base` | `IA32_KERNEL_GS_BASE` |
|---|---|---|
| ring 0 | structure du cœur (`PerCpu`) | 0 |
| ring 3 | 0 | structure du cœur |

Les deux MSR sont posés par `syscall::init`. Chaque franchissement de privilège
doit exécuter un `swapgs` : `syscall`/`sysret` le font dans
`boot/syscall_entry.asm`, et `user::enter_ring3` le fait avant son `iretq` —
`iretq` ne s'en charge pas tout seul.

Les handlers d'interruption ne lisent jamais `gs:` : une interruption prise en
ring 3 s'exécute donc avec le GS utilisateur, sans conséquence.

---

## 4 bis. API système (phase 5, spec §6)

Soixante et onze appels répartis sur neuf domaines, décrits par une table
de données plutôt que par un `match` : c'est ce qui rend l'API
interrogeable par `SYS_API_INFO` et vérifiable au démarrage.

| Domaine | Préfixe | Appels | Ce qu'il couvre |
|---|---|---|---|
| 0x00 | `SYS_CORE` | 3 | trace, dernière erreur, description de l'API |
| 0x01 | `SYS_FS` | 17 | §6.2, système de fichiers complet |
| 0x02 | `SYS_PROC` | 11 | §6.3, plus `YIELD` et `EXIT` |
| 0x03 | `SYS_MEM` | 7 | §6.4, plus `STAT` |
| 0x04 | `SYS_IPC` | 8 | §6.5 |
| 0x05 | `SYS_USER` | 7 | §6.6, comptes et permissions |
| 0x06 | `SYS_HW` | 5 | §6.6, description du matériel |
| 0x07 | `SYS_TIME` | 7 | §6.6, tics, horloge murale, sommeil |
| 0x08 | `SYS_NET` | 6 | §6.6, déclaration appliquée, pile absente |
| 0x09–0x0B | `SYS_OBJ`, `SYS_THREAD`, `SYS_SIG` | — | §6.7, servis par `syscall::ported` seul (tables vides ici) |
| 0x0C | `SYS_GPU` | 16 | §6.8, la pile graphique ; les deux portes mènent à `gpu::api::dispatch`, erreurs en `-errno` |

**Convention d'appel** : `RAX` porte le numéro, les arguments passent par
`RDI RSI RDX R10 R8 R9` — `R10` et non `RCX`, que l'instruction `syscall`
écrase avec l'adresse de retour. Le retour est dans `RAX` : positif ou nul
en cas de succès, opposé du code d'erreur sinon.

**Erreur structurée** (§6.1) : `RAX` ne porte que le code. `SYS_ERROR_LAST`
rend l'enregistrement complet — nom symbolique, message, et un contexte de
taille fixe qui nomme ce qui a échoué. Le contexte est tronqué plutôt
qu'alloué : construire une erreur ne doit jamais dépendre de l'état du tas.

**Sync et Async** (§6.1) : un appel `Sync` ne doit jamais endormir son
appelant ; un appel `Async` en a le droit. Ce n'est pas une convention de
nommage mais un invariant vérifié — `Call::require_async` refuse un
blocage demandé depuis un appel synchrone. Trois appels endorment
réellement : `SYS_PROC_WAIT`, `SYS_IPC_RECV` avec attente, et
`SYS_TIME_SLEEP_MS`.

**Deux origines, une implémentation** : `syscall_dispatch` sert le ring 3,
`syscall::invoke` sert le noyau. Même table, mêmes droits, mêmes modes ;
seule la provenance des pointeurs diffère, et cette différence est
elle-même testée.

**`SETPRIO` et la table du §7.3** : la classe de priorité est fixée par le
format (§7.1) et jamais modifiable ; la tranche CPU l'est, pour les
formats que le §7.3 ne marque pas autrement. `.dsa` (« jamais réduit ») et
`.ras` (« jamais modifiable ») sont refusés, `.ss`, `.gs` et `.cus`
acceptés. Descendre en arrière-plan est permis à tous ; remonter est
réservé à `.dsa`, `.ss`, ROOT et ADMIN — sans quoi le « CPU réduit à 2 % »
du §3.4 ne serait qu'une suggestion. L'appel renvoie la tranche effective
en tics.

**Identité** : `Call::who` interroge l'ordonnanceur. Une tâche hérite du
compte de son créateur au lancement et ne peut que descendre en privilège
(`SYS_USER_SETID`). C'est ce qui ferme la limite « `Credentials` est
fourni par l'appelant » : un programme ring 3 n'a aucun moyen de se
déclarer ROOT.

**Arènes virtuelles** — trois plages disjointes, chacune avec sa durée de
vie et ses droits :

```text
  0x0100_0000   images du chargeur, creneaux de 2 Mio
  0x1000_0000   piles utilisateur, creneaux de 1 Mio
  0x2000_0000   allocations SYS_MEM_ALLOC, 256 Mio, granularite 4 Kio
```

---

## 5. Vecteurs d'interruption

| Vecteur | Usage |
|---|---|
| 0–31 | Exceptions CPU. Le 8 (double faute) bascule sur `IST[0]` |
| 32 | Timer LAPIC — un par cœur, 100 Hz |
| 33 | IRQ clavier PS/2, routée vers le cœur 0 |
| 48–63 | Lignes de périphérique, vecteur = 48 + numéro d'IRQ (6.1.6) |
| 64–67 | Messages MSI-X de fonctions PCI (`drivers::VECTOR_MSI_BASE`) ; 64 = réception de virtio-net, vers le cœur 0 (11.15) ; 65 = file de contrôle de virtio-gpu (`VECTOR_GPU`, phase 13), relayée par la tâche `drv-gpu` |
| 240 | IPI de réordonnancement |
| 241 | IPI d'invalidation de TLB (diffusée, ou ciblée sur un espace) |
| 254 | Erreur interne LAPIC |
| 255 | Interruption parasite — **jamais acquittée** |

Depuis la phase 6.1.6, **aucun handler ne pilote de périphérique**. Les
vecteurs 33 et 48–63 exécutent le même corps : incrémenter le compteur du
vecteur, réveiller les tâches inscrites, acquitter le LAPIC. Toute la
logique vit dans la tâche pilote, qui a une pile et le droit de dormir.

Le compteur, et non un drapeau : deux IRQ rapprochées pendant que la tâche
travaille ne doivent pas se réduire à une seule passe de traitement. La
tâche compare le compteur à la dernière valeur qu'elle a vue et sait
*combien* d'événements l'attendent.

Seules les lignes **ISA** sont routées — IRQ 1 (clavier) et 12 (souris),
à front, actives hautes. Une ligne PCI est à niveau et active basse : le
relais générique, qui ne lit aucun registre de périphérique, la laisserait
assertée. Voir les limites de la phase 6 dans `ROADMAP.md`. Les
fonctions PCI qui offrent **MSI-X** passent par les vecteurs 64–67 : un
message est un front, le même relais sert (`pci::Msix::route`,
`idt::msi_irq_*`). Seule la file de réception de virtio-net l'utilise.

Le LAPIC est piloté en **x2APIC** (registres en MSR) quand le processeur
l'offre, en xAPIC (MMIO) sinon ; `apic::is_x2apic`. L'identité du cœur
courant se lit par `RDPID` (ou `RDTSCP`) dans `TSC_AUX`, que chaque cœur
écrit à son démarrage (`smp::publish_cpu_id`) ; le registre d'identité
du LAPIC n'est plus lu qu'avant cela.

---

## 6. Ordonnancement

- Table de tâches unique sous `SpinLock`, `current` et `started` indexés par cœur.
- Élection : anti-famine (500 ms) d'abord, puis priorité minimale, puis
  round-robin circulaire — le tout filtré par affinité.
- Affinité selon spec §7.2 : cœur 0 aux `.dsa`, moitié basse aux `.ss`, moitié
  haute au reste, `Kernel` et `Idle` partout.
- Une tâche `Idle` épinglée par cœur, de priorité 6 (inférieure à `.ras`).
- Commutation aux points `preempt_if_needed()`, jamais depuis un handler.
- **Endormissement en deux temps** : `arm_sleep()` pose l'état SLEEPING
  *avant* que la condition ne soit testée, `yield_now()` cède ensuite.
  Poser l'état après le test laisserait un autre cœur satisfaire la
  condition et appeler `wake` sur une tâche encore RUNNING — un réveil
  sans effet, suivi d'un sommeil définitif. Masquer les interruptions ne
  ferme pas cette fenêtre : elle est ouverte par le parallélisme.
- **Une tâche morte encore élue n'est pas récupérable.** `try_reap` refuse
  de rendre la pile d'une tâche qu'un cœur désigne encore comme courante :
  `kill` pose l'état depuis l'extérieur, mais le cœur concerné ne
  l'abandonne qu'à sa prochaine élection.
- **Un réveil sur le cœur même lève son drapeau de réordonnancement**
  (`wake`) : le prochain point de préemption élit à nouveau, comme l'IPI
  le fait pour un autre cœur.
- `current_pid()` se lit sans verrou (`CURRENT_PID`, tenu à jour par la
  commutation) ; le tableau des tâches du moniteur est copié sous le
  verrou et imprimé après.
- La tâche inactive guette son drapeau 50 µs avant `hlt` quand elle
  revient d'un vrai travail (`IDLE_POLL_US`), pas après un tic à vide.
- Mesures par tâche : temps processeur en cycles TSC (`run_cycles`),
  attente entre `wake` et l'élection d'un thread Linux
  (`wake_latency`, ligne `[moniteur] reveils Linux` toutes les 5 s).

### Priorités et tranches (spec §7.3)

| Type | Priorité | Tranche 1er plan | Tranche arrière-plan |
|---|---|---|---|
| Kernel | 0 | 40 | 40 |
| `.dsa` | 1 | 40 | 40 |
| `.ss` | 2 | 25 | 15 |
| `.gs` | 3 | 20 | 2 |
| `.cus` | 4 | 10 | 4 |
| `.ras` | 5 | 5 | 1 |
| Idle | 6 | 1 | 1 |

---

## 7. Appels système implémentés

Convention : `RAX` = numéro, arguments dans `RDI, RSI, RDX, R10, R8, R9`,
retour dans `RAX` (négatif = code d'erreur opposé). La table complète est
lisible à l'exécution par `SYS_API_INFO`, et journalisée au démarrage.

| Numéro | Nom | Mode | Note |
|---|---|---|---|
| `0x0001` | DEBUG_PRINT | Sync | Provisoire, disparaîtra avec `/dev/console` |
| `0x0002` | ERROR_LAST | Sync | Erreur structurée complète du §6.1 |
| `0x0003` | API_INFO | Sync | Décrit la table elle-même |
| `0x0101`–`0x0105` | FS READ/WRITE/OVERWRITE/DELETE/RENAME | Async ×3 | `READ` accepte un décalage |
| `0x0106`–`0x0108` | FS MOVE/COPY/LINK | Sync, Async, Sync | `MOVE` dit s'il a recopié |
| `0x0109`–`0x010C` | FS STAT/CHMOD/CHOWN/AUDIT | Sync | `AUDIT` réservé à ADMIN |
| `0x0110`–`0x0111` | FS DIR_ACCESS/DIR_CREATE | Sync | |
| `0x0120`–`0x0124` | FS DISK_LIST/MOUNT/EXPORT | Sync ×2, Async | `EXPORT` réservé à ADMIN |
| `0x0130`–`0x0135` | FS NODE_STAT/MOVE/CLONE/TAG/UNLINK/TAGS | Sync | LIFS seulement (NovaFS : `SYS_ERR_UNSUPPORTED`) ; chemin du nœud reconstruit par l'index inverse et passé au bac à sable §12.4 |
| `0x0201` | PROC_SPAWN | Async | Charge depuis le système de fichiers |
| `0x0202`–`0x0204` | PROC KILL/SUSPEND/RESUME | Sync | |
| `0x0205`–`0x0207` | PROC STAT/LIST/SETPRIO | Sync | `SETPRIO` : tranche §7.3, pas la classe §7.1 |
| `0x0208` | PROC_YIELD | Sync | Hors spec, nécessaire |
| `0x0209`–`0x020A` | PROC WAIT/SELF | Async, Sync | `WAIT` rend pile et image |
| `0x020B` | PROC_EXIT | Sync | Hors spec : terminaison avec code |
| `0x0301`–`0x0303` | MEM ALLOC/FREE/RESIZE | Sync | Par pages, W^X appliqué |
| `0x0304`–`0x0307` | MEM SHARE/PROTECT/LOCK | Sync | Voir les limites de la roadmap |
| `0x0308` | MEM_STAT | Sync | Hors spec : état de l'arène |
| `0x0401`–`0x0402` | IPC SEND/RECV | Async | En-tête de 16 octets devant la charge |
| `0x0403`–`0x0406` | IPC OPEN/CLOSE/SUBSCRIBE/PUBLISH | Sync ×3, Async | Abonnement **par nom** |
| `0x0407`–`0x0408` | IPC QUERY/RESPOND | Sync, Async | Identifiant de requête fabriqué par le noyau |
| `0x0501`–`0x0507` | USER SELF/LIST/LOOKUP/SETID/CAN/CREATE/DELETE | Sync | `SETID` ne sait que descendre |
| `0x0409`–`0x040C` | IPC PORT_BIND/CONNECT/ACCEPT/CLOSE | Sync ×3, Async | Hors spec : mécanisme du §12.2 |
| `0x0601`–`0x0605` | HW INFO/CPU_LIST/MEMORY/PCI_LIST/DISK_INFO | Sync | `.dsa`, `.ss`, `.ras` uniquement |
| `0x0606` | HW_DRIVER_LIST | Sync | Nomme le canal de chaque pilote (6.1.6) |
| `0x0701`–`0x0705` | TIME TICKS/UPTIME/NOW/NOW_MS/DATE | Sync | Deux échelles jamais confondues |
| `0x0706`–`0x0707` | TIME SLEEP_MS/RESOLUTION | Async, Sync | |
| `0x0801`–`0x0802` | NET STATUS/IFACE_LIST | Sync | Adresse, masque, passerelle, DNS |
| `0x0803`–`0x0806` | NET CONNECT/SEND/RECV/RESOLVE | Async | Câblés sur la pile de la phase 6.2 |
| `0x0807` | NET_CLOSE | Sync | Hors spec : sans elle, huit connexions et plus rien |

---

## 8. Tâches créées au démarrage

Jeu de démonstration, destiné à être remplacé par `init.dsa` en phase 6.5.

| Nom | Type | Rôle |
|---|---|---|
| `idle-N` | Idle | Une par cœur, `hlt` en boucle |
| `journal` | `.dsa` | Ouvre le canal `sys.log`, affiche ce qui y transite |
| `ping` / `pong` | `.ss` | Va-et-vient IPC bloquant |
| `moniteur` | `.cus` | Rapport périodique toutes les 5 s |
| `calcul-a` / `calcul-b` | `.cus` | Calcul pur, vérifie le parallélisme réel |
| `userland` | `.cus` | Bascule en ring 3, exerce les appels système |
| `demo3` | `.cus` | Image chargée par le loader, relocalisée, auto-signée |
| `demo` | `.gs` | Image ENTERPRISE certifiée — bascule avant/arrière-plan |
| `demo` | `.ras` | Image non signée — avertissement rouge au lancement |
| `init.dsa` | `.dsa` | Orchestrateur du §2.1, adopte les orphelines |
| `fs.dsa` / `ipc.dsa` / `hw.dsa` | `.dsa` | Services core, un canal nommé chacun |
| `drv-clavier` … `drv-audio` | `.dsa` | Cinq pilotes (6.1) |
| `reseau` | `.dsa` | Configuration statique ou DHCP, puis se termine |
| `phase6-autotest` | `.dsa` | Autotest de la couche OS |
| `drv-gpu` | `.dsa` | Pile graphique (phase 13) : relaie l'interruption de virtio-gpu, met le backend en route après les autotests, suit les soumissions, délai de garde, reprend les contextes des processus morts |
| `phase13-autotest` | `.dsa` | Autotest de la pile graphique, lancé en dernier |

Les trois dernières sont produites par `binfmt::loader` à partir de binaires
construits en mémoire par `binfmt::sample`, et portent toutes le même
programme ring 3 (`boot/usermode.asm`), recopié depuis la section `.user`.

---

## 9. Chaîne de chargement d'un binaire (phase 3)

```
binfmt::loader::load(data)
 ├─ binfmt::inspect          analyse structurelle (3.1) + règles spec §3.4
 ├─ binfmt::signature::verify   intégrité SHA-256 puis SYSTEM / ENTERPRISE / SELF
 ├─ réservation d'un créneau d'arène (image + pile)
 ├─ allocation de tous les cadres d'un bloc — échec avant tout mapping
 ├─ recopie des sections via la fenêtre physique
 ├─ relocations ABS64 si la base effective diffère de la base de liaison
 └─ mapping définitif, une permission par page (W^X)

binfmt::loader::spawn(&image)
 ├─ apply_type_rules            journalise ce que le format impose (3.2.4)
 ├─ scheduler::spawn_dormant    priorité et cœur déduits du seul format (3.4)
 ├─ dépôt du point d'entrée dans LAUNCHES
 └─ scheduler::resume           la tâche devient éligible
     └─ user_task_entry → avertissement éventuel → user::enter_ring3
```

### W^X

Chaque page reçoit soit `WRITABLE`, soit l'absence de `NO_EXECUTE`, jamais les
deux. Trois barrières successives :

1. `Section::violates_wx` — une section ne peut pas déclarer W et X ;
2. `sections_share_page` — deux sections aux droits distincts ne peuvent pas
   partager une page de 4 KiB ;
3. `loader::verify_protection` — relit la hiérarchie de pages après coup, plutôt
   que de faire confiance aux intentions du chargeur.

Le bit `NO_EXECUTE` n'est effectif que si `EFER.NXE` est armé, sur **chaque**
cœur (`memory::enable_nx`, appelé par `kernel_main` et par `smp::ap_entry`).

---

## 10. Système de fichiers

### Chaîne de démarrage du stockage

```
fs::init
 ├─ pci::enumerate            classe 01.06 → contrôleur AHCI
 ├─ ahci::detect              un AhciDisk par port dont le lien est établi
 │   └─ ports sans disque ou ATAPI : ignorés
 ├─ mount::discover           rien n'est écrit, rien n'est formaté (14.0.1)
 │   ├─ table GPT ou MBR : chaque partition LIFS ou NovaFS dont le superbloc
 │   │   confirme le type → une fenêtre PartitionDevice ; les autres
 │   │   partitions sont listées et laissées intactes
 │   ├─ pas de table : LIFS ou NovaFS au bloc 0 → un volume (disque entier)
 │   ├─ table endommagée, NovaFS d'une autre version : signalés, intacts
 │   └─ sinon : vivier des supports amovibles (cibles de l'export)
 ├─ mount::choose_root        racine=<GUID>, sinon LIFS « système », sinon
 │                            premier LIFS, sinon premier NovaFS
 ├─ montage : la racine en /, les autres volumes A:/, B:/… (une lettre par
 │   partition) ; LIFS rejoue au besoin la transaction validée du journal
 ├─ aucune racine : volume LIFS en mémoire de 8 Mio, formaté par le noyau
 └─ a chaque demarrage : tree::create_system_tree + create_user_tree
     (cree ce qui manque, et remet droits et proprietaire de la spec 17)
```

La tâche `fs-validation` appelle `fs::tick_all` toutes les 500 ms : chaque
volume LIFS valide sa transaction toutes les deux secondes (validation
groupée), et aussi sur `flush` (`fsync`, `sync`, arrêt).

QEMU démarre avec `-boot d` : le MBR protecteur d'une GPT porte `0x55AA`, et
sans cela le BIOS tente d'amorcer le disque (qui n'a pas de code d'amorçage)
au lieu du CD.

### Disque système (phase 14)

`make disk` (`tools/lifs.py create`) écrit une GPT : p1 LIFS
« nova-systeme », drapeau système, la racine ; p2 LIFS « nova-donnees »
(`DATA_SIZE`, 256 Mio), `A:/`. Partitions alignées sur 1 Mio, longueurs en
blocs entiers de 4 Kio. Type LIFS `BD095063-2621-48A5-89A3-0310280C1AF0`,
octet MBR `0x4C`.

### Disposition d'un volume LIFS

| Région | Contenu |
|---|---|
| bloc 0 | superbloc — `LIFS\0\0\0\x01`, version, géométrie, racines des trois arbres globaux, prochain nœud, compteurs, séquence du journal, CRC32 à l'octet 4092 |
| blocs 1… | bitmap des blocs, un bit par bloc |
| … | journal : `total/64` blocs, entre 64 et 16 384 |
| le reste | blocs d'arbres (`"LB"`), chaînes d'étendues (`"LE"`), données |

Quatre sortes d'arbres B+, un seul code (`btree.rs`), clés de 128 bits :

| Arbre | Clé | Valeur |
|---|---|---|
| nœuds | (identifiant, 0) | enregistrement de 256 octets : type, liens, taille, propriétaire, groupe, trois masques, dates, racine (dossier) ou chaîne d'étendues, sept étendues en ligne |
| un par dossier | (FNV-1a du nom, discriminant) | entrée de 264 octets : nœud visé, longueur, nom (254 octets au plus) |
| comptes de références | (premier bloc, 0) | longueur, références (≥ 2 ; absent = 1) |
| index inverse | (nœud, dossier) | nombre d'entrées de ce dossier qui désignent ce nœud |

Journal : pour une transaction, des blocs descripteurs `LJDESC` (numéros
des blocs d'origine), les images, puis un bloc `LJCOMMIT` (séquence,
nombre, CRC32 des images). Validation : données du cache d'abord, journal
puis flush, blocs d'origine puis flush, **superbloc en dernier** puis
flush. Au montage, la transaction `journal_seq + 1` entière est rejouée.

Sans contrôleur AHCI, un volume LIFS en mémoire de 8 Mio prend le relais :
le reste du système ne doit pas dépendre de la présence d'un disque.

### Disposition d'un volume NovaFS (volumes d'avant la phase 14)

| Région | Contenu |
|---|---|
| bloc 0 | superbloc — signature `NOVAFS`, géométrie, somme de contrôle |
| bloc 1… | bitmap des nœuds, un bit par nœud |
| … | bitmap des blocs, un bit par bloc |
| … | table des nœuds, 128 octets par nœud, 32 par bloc |
| … | zone de données |

Bloc logique de 4 Kio, quelle que soit la taille de secteur du support.
Un nœud pour 32 Kio de volume.

### Nœud et lien (spec §4.1)

Un **nœud** porte un contenu, une taille, des droits — jamais un nom. Un
**dossier** est un nœud dont le contenu est une table d'entrées de
128 octets `nom (120) → identifiant de nœud (8)` (format v2 ; le format v1
tenait en 64 octets, dont 56 de nom). Il en découle :

| Opération | Ce qui bouge réellement |
|---|---|
| `move` intra-volume | deux entrées de 128 octets |
| `rename` | une entrée de 128 octets |
| `link` | une entrée ajoutée, compteur de liens incrémenté |
| `copy` intra-volume | un nœud neuf, tous les blocs recopiés |
| `move` inter-volumes | copie réelle puis suppression — l'identifiant de nœud n'a de sens que dans sa table |

Adressage : 5 pointeurs directs plus un bloc d'indirection. Celui-ci tient
512 entrées, ou 512 tables de 512 entrées quand le fichier dépasse
2068 Kio (`indirect_levels`) — 1 Gio au plus par nœud. Un fichier qui
rétrécit rend ses blocs **et** les tables qui ne décrivent plus rien ; un
dossier rend de même les blocs de liens vidés en fin de table.

### Permissions (spec §5)

Treize permissions, trois masques par nœud — propriétaire, groupe, autres.
La première classe qui s'applique décide ; il n'y a pas de repli sur la
suivante.

| Rôle | Effet |
|---|---|
| ROOT | passe outre tous les masques |
| ADMIN | passe outre les masques, **sauf** la règle du double niveau |
| USER / GUEST / SERVICE | soumis aux masques |

`execute` est la seule permission à double niveau : exigée sur le fichier
**et** sur le dossier qui le contient. `audit` est réservée à ADMIN et ROOT
quels que soient les masques.

Chaque vérification alimente `fs::audit`, tampon circulaire de 128 entrées
lisible par ADMIN seulement.

### Export (spec §4.4)

`fs::export_to_medium` réécrit une arborescence sur un support **non
monté**, puis `fs::verify_medium` relit l'image par le chemin
qu'emprunterait un hôte. Les trois formats étrangers sont écrits :

| Format | Unité | Ce qui survit | Vérification hôte |
|---|---|---|---|
| FAT32 | cluster 1 Kio | nom long, lecture seule | `fsck.vfat -n` |
| ext4 | bloc 4 Kio | uid/gid, bits POSIX, **liens durs** | `fsck.ext4 -fn` |
| NTFS | cluster 4 Kio | nom UTF-16, lecture seule | `ntfsfix -n` |

`make check-export` passe les trois images aux outils du système. Chaque
exporteur retient les identifiants de nœud déjà visités : `link` étant
autorisé sur un dossier (§5.1), l'arborescence est un graphe et un
parcours naïf ne terminerait pas.

L'ordonnancement des supports est significatif : le port AHCI *n* reçoit
le format *n* de la liste `[FAT32, ext4, NTFS]`, ce que le `Makefile`
fixe par l'ordre des `-device ide-hd`.

---

## 11. Couche OS (phase 6)

### Ce que « pilote en espace utilisateur » recouvre aujourd'hui

Un pilote est une **tâche ordonnancée**, pas une fonction appelée depuis
une IRQ. Quatre règles, et elles sont l'essentiel de la valeur :

| Règle | Ce qu'elle interdit |
|---|---|
| Une tâche, avec sa pile et sa priorité | Faire du travail dans un handler |
| IRQ reçues par `drivers::irq` | Installer un handler propre au pilote |
| Service exposé par un canal IPC nommé | Être appelé par pointeur de fonction |
| Sommeil par `SYS_TIME_SLEEP_MS` | Contourner l'ordonnanceur |

Ce qui **n'est pas** vrai : les tâches pilotes tournent en ring 0 et
partagent l'espace d'adressage du noyau, faute de SDK (phase 9.1) capable
de produire un binaire autonome. Aucune des quatre interfaces ci-dessus ne
changera le jour où elles passeront en ring 3.

### Les cinq pilotes et leurs canaux

| Tâche | Canal | Matériel | Réveil |
|---|---|---|---|
| `drv-clavier` | `dev.keyboard` | 8042, scancodes 1 | IRQ 1 (vecteur 33) |
| `drv-souris` | `dev.mouse` | PS/2 3 boutons | IRQ 12 (vecteur 60) |
| `drv-stockage` | `dev.storage` | Volumes montés | requête IPC |
| `drv-reseau` | `dev.net` | virtio-net, sinon Intel 8254x | scrutation 10 ms + tâches en attente |
| `drv-audio` | `dev.audio` | AC'97 | scrutation 10 ms |

Le clavier et la souris partagent **un seul port de données**, 0x60 :
`drivers::ps2` est l'unique lecteur, sous verrou, et range chaque octet
dans la file du périphérique qui l'a émis (bit 5 du registre d'état). Deux
pilotes lisant 0x60 chacun de leur côté se voleraient des octets, et un
paquet souris ne porte aucune marque de début fiable — le flux se
désynchroniserait définitivement.

### Pile réseau (6.2)

```
drivers::nic ─┬─ virtio_net
              └─ e1000       ──trame──▶  net::input
                               └─ eth  ─┬─ arp   cache, résolution, prochain saut
                                        └─ ip   ─┬─ icmp  echo
                                                 ├─ udp  ─┬─ dhcp
                                                 │        └─ dns
                                                 └─ tcp   client, machine à états
```

Deux chemins d'émission, et les confondre est un piège réel :

| Fonction | Résolution ARP | Appelée depuis |
|---|---|---|
| `ip::send` | attend, jusqu'à 900 ms | une tâche cliente |
| `ip::send_cached` | cache seul, abandonne sinon | le traitement d'un paquet reçu |

Les trames ne montent que par une scrutation (`nic::poll`) : celle de la
tâche pilote, ou celle d'une tâche qui attend sur TCP. Si la tâche qui
scrute attendait une réponse ARP, elle attendrait une trame qu'elle est
elle-même chargée de lire : l'attente est perdue d'avance et la réception s'arrête pendant ce
temps. Les acquittements TCP et les réponses d'écho ICMP passent donc par
`send_cached` — un paquet abandonné faute d'adresse matérielle est
retransmis par l'autre extrémité, c'est le contrat.

Configuration dans `/net/interface.cfg`, une ligne `clé=valeur` par
champ. Une configuration **statique l'emporte sur DHCP** : c'est un choix
explicite de l'administrateur, qu'un bail obtenu ensuite écraserait sans
qu'il l'ait demandé.

### Débit réseau

Mesuré par `make bench-net` (`net::bench`) : l'hôte sert un fichier de
512 Mio, l'invité le télécharge une fois ses autotests finis, dans une
tâche `.cus` comme celles du navigateur, et chronomètre au TSC étalonné.
Relevés du 22 septembre 2026, `-smp 4`, mode `user` de QEMU :

| Carte | Émulation | Débit |
|---|---|---|
| virtio-net | TCG (`make run`) | 257 à 316 Mo/s |
| virtio-net | KVM | 1 160 à 1 620 Mo/s |
| e1000 | KVM | 127 à 133 Mo/s |
| virtio-net, monocœur | TCG | 330 Mo/s (128 Mio) |

Ce qui fait le débit, dans l'ordre où cela a compté :

1. **Le lecteur ne dort pas au milieu d'un transfert.** `tcp::wait_step`
   dormait un tic entier (10 ms) après 2 000 tours vides ; pendant ce
   temps seule la tâche pilote vide l'anneau, une fois par tic : 256
   trames par 10 ms, 37 Mo/s. Tant que des trames sont arrivées dans les
   deux derniers tics (`nic::recently_active`), le lecteur continue de
   scruter.
2. **Il ne cède pas le cœur à chaque tour.** `yield_now` donnait le cœur,
   pour toute sa tranche, à n'importe quelle tâche prête du même cœur.
   Pendant un transfert actif, c'est `preempt_if_needed` : le cœur n'est
   rendu qu'à l'échéance de la tranche.
3. **Il ne martèle pas les verrous.** Un tour vide ne prend aucun verrou
   (`virtio_net::poll` compare l'index de l'anneau sans verrou) et marque
   une pause de 64 `pause`. Sans cela, le cœur 0 attendait ces verrous
   interruptions masquées et perdait des tics : la mesure au compteur de
   tics annonçait 5 Go/s.
4. **Pas de copie ni d'allocation par trame** sur virtio-net : les
   tampons sont prêtés à la pile puis rendus. Octets copiés par tranches
   dans `tcp::input` et `tcp::recv`.
5. **Un ACK pour deux segments** (`ACK_EVERY`), et les ACK en attente
   partent à la fin de chaque lot scruté (`tcp::flush_acks`) : aucun
   minuteur d'ACK retardé n'est nécessaire.

Le mode `user` de QEMU (slirp) est le goulot restant sous TCG. Rien
n'est plafonné volontairement à 200 Mo/s.

### Isolation applicative (§12.1, §12.2, §12.5)

```
ipc::send
 └─ allowed_by_isolation(expéditeur, destinataire)
     ├─ apps::communication  → identités, table du §12.1
     │    DirectIpc     : passe
     │    Blocked       : refusé (.ras, sans exception)
     │    InternalPort  : consulte ports::authorized
     └─ ports::authorized   → un lien accepté existe-t-il ?
```

`ipc::send` est le **point de passage unique** de tout message nominatif,
donc le seul endroit où la règle puisse être appliquée sans trou. Une
tâche dont l'identité n'est pas enregistrée — pilote, service, noyau —
n'est pas concernée : ces règles régissent les applications entre elles.

L'ouverture d'un lien inter-groupes demande l'accord de l'utilisateur
(`prompt::ask`) **puis** celui de l'application réceptrice. L'invite passe
en premier : réveiller une application pour une demande que l'utilisateur
va rejeter lui apprendrait qui cherche à lui parler, ce qui est déjà une
fuite. Un choix mémorisé rend l'invite gratuite et retire tout coût à cet
ordre.

Les choix vivent dans `/users/{user}/settings/permissions/{app_id}.cfg`,
une ligne par ressource. Une invite sans réponse **expire et refuse** :
accorder par silence ferait de toute machine sans surveillance une machine
permissive.

### Audio (6.3)

48 kHz, stéréo, entiers signés de 16 bits. **Aucun flottant** : la cible
est en soft-float, SSE n'est armé ni dans CR0 ni dans CR4, et un seul
`f32` produirait une exception à la première note. Le volume est une
fraction sur 256, l'accumulation se fait sur 32 bits et **sature** — un
débordement silencieux transformerait un maximum positif en maximum
négatif, soit un claquement à chaque crête.

La sortie du mélangeur a **un seul consommateur à la fois** : le pilote
normalement, un diagnostic quand il la réserve (`audio::reserve`). Deux
consommateurs se voleraient des échantillons.

---

## 11 bis. Sortie graphique (phase 7.1)

### Du header Multiboot2 au pixel

| Étape | Où | Ce qui se passe |
|---|---|---|
| 1 | `boot/boot.asm` | Tag 5 du header Multiboot2 : demande 1024×768×32, drapeau « facultatif » |
| 2 | GRUB | Programme le mode par VBE, décrit le résultat dans un tag 8 |
| 3 | `multiboot::parse` | Décode le tag 8 champ par champ en `FramebufferInfo` |
| 4 | `graphics::init` | Accroche le framebuffer par la fenêtre physique, allume la console |
| 5 | `graphics::enable_double_buffer` | Remappe en write-combining, alloue le tampon arrière |

Les dimensions du tag 5 sont un **souhait** : GRUB rend ce qu'il sait faire, et
rien dans le noyau ne suppose 1024×768. Vérifié sous QEMU avec `-vga std`
(32 bits), `-vga cirrus` (24 bits), `-vga vmware` et `-vga qxl`, ainsi qu'avec
`-vga none`, qui retombe proprement sur le writer VGA.

### Pourquoi deux étapes

La première ne demande **ni cadre ni table de pages** : elle passe par la
fenêtre physique déjà installée par `boot.asm`. C'est ce qui lui permet d'être
appelée avant `frame::init`, et donc d'afficher les toutes premières lignes du
noyau. La seconde vient après l'allocateur, parce qu'un tampon arrière coûte
des cadres et qu'un mapping write-combining coûte des tables de pages.

Contrepartie de la première étape : elle ne marche que si le framebuffer tient
sous 4 GiB, limite de la fenêtre physique. Au-delà, l'écran ne s'allume qu'à la
seconde étape et les lignes de démarrage ne sortent que sur la série.

### Types de cache

Un framebuffer n'est pas un banc de registres : aucune écriture n'a d'effet de
bord, seule compte l'image finale.

| Type | Coût | Verdict |
|---|---|---|
| UC (`NO_CACHE`, comme le MMIO classique) | un accès bus par pixel | inutilisable |
| WB (fenêtre physique par défaut) | correct, mais l'image sort au gré des évictions | replis |
| WC (PAT entrée 4) | rafales d'une ligne de cache, jamais relu | retenu |

`memory::init_pat` reprogramme l'entrée 4 du MSR `IA32_PAT` (0x277) de WB
(0x06) à WC (0x01), soit `0x0007040100070406`. Les entrées 0 à 3 ne bougent
pas : ce sont elles que sélectionnent les combinaisons PCD/PWT des pages déjà
mappées. L'entrée 4 se sélectionne par le bit PAT (bit 7 d'une entrée de 4 KiB)
avec PCD = 0 et PWT = 0.

Le PAT est un registre **par processeur** : `smp::ap_entry` le programme en
toute première instruction, avant le moindre accès mémoire discrétionnaire.
Deux cœurs avec des types de cache différents sur la même page physique est un
cas que l'architecture déclare indéfini.

### Découpage des responsabilités

| Module | Sait | Ignore |
|---|---|---|
| `framebuffer.rs` | pixels, pas de ligne, format de couleur, tampons | caractères, fenêtres |
| `font.rs` | 95 glyphes 8×16 | où ils sont dessinés |
| `console.rs` | cellules, curseur, défilement, bande sale | format des pixels |
| `mod.rs` | initialisation, verrou, palette, aiguillage | le reste |

Le compositeur du §8.1 s'empilera sur `framebuffer.rs` sans passer par la
console : le §8.1 lui réserve l'écriture sur le framebuffer, et la console
n'est que la sortie de diagnostic du noyau.

### Police

95 glyphes, 0x20 à 0x7E, dans une cellule de 8×16. Le dessin occupe une boîte
de 5×9 posée au décalage (1, 4) : une colonne vide à gauche, deux à droite et
quatre lignes au-dessus donnent l'interlettrage et l'interligne sans que le
rendu ait à les ajouter. Les lignes 0 à 6 portent le corps, la ligne 6 est la
ligne de base, les lignes 7 et 8 les jambages de `g j p q y` et de la virgule.

Écrite pour Nova OS, pas empruntée : le noyau n'admet aucune dépendance
externe, et une police de console prise à l'hôte poserait en plus une question
de licence dans un binaire redistribué. Un octet par ligne de cellule, bit 7 à
gauche.

### Publication

La console note la bande de lignes qu'elle a salie et ne la publie qu'à la fin
d'un `print!`. Un caractère ne coûte donc qu'une bande de 16 pixels — sauf
quand la ligne fait défiler l'écran, auquel cas tout a changé et l'image
entière repart.

Mesuré sous QEMU, en construction debug, sur la boucle de démonstration :
2,7× plus lent qu'en mode texte, dont **2× imputables au seul `present`**
(constaté en le neutralisant). Le défilement et le rendu des glyphes pèsent
chacun moins de 10 %. Trois mébioctets par ligne défilée sont négligeables sur
du vrai matériel — 45 Mo/s au rythme observé — mais coûtent cher sous
émulation, où chaque écriture en VRAM déclenche un suivi de pages sales. C'est
la présentation par rectangles sales du compositeur (7.2) qui traitera le
problème pour du vrai contenu ; l'attaquer ici reviendrait à optimiser un
artefact d'émulateur.

---

## 11 ter. Système graphique (phase 7.2 → 7.7)

### Aucun appel système nouveau

Le §8.1 dit que les applications « envoient des instructions IPC et le
compositeur exécute le rendu ». Le protocole vit donc **entièrement au-dessus
d'`ipc`**, sur le canal nommé `sys.gui`. Un domaine `SYS_GUI_*` aurait mis le
noyau sur le chemin du dessin, ce que la spec refuse — et le compositeur est
une application `.ss`, pas un sous-système du noyau.

### Le compositeur est une tâche noyau, comme `init.dsa`

Même précédent que la phase 6 : sans SDK (étape 9.1), un composant spécifié
comme application est écrit en tâche noyau portant le même contrat. Le
protocole, les emplacements de modules, le z-order et les règles de propriété
sont réels et vérifiables ; seule la livraison du code change.

Ce qui est déjà vrai du §8.1 : **rien d'autre que le compositeur n'écrit sur le
framebuffer.** Dès qu'il démarre, `graphics::release_console` fait taire la
console du noyau à l'écran — elle continue sur la série, qui reçoit tout. Le
gestionnaire de panique reprend l'écran par `reclaim_console`, seule exception,
et elle se justifie d'elle-même.

### Une surface garde ses instructions, pas ses pixels

Un tampon par surface coûterait 3 Mio par fenêtre plein écran, et il n'y a pas
de mémoire partagée pour que le client y écrive — il faudrait faire transiter
chaque pixel par un message plafonné à 256 octets. Une surface retient donc sa
**liste d'affichage**, que le compositeur rejoue en ordre de z à chaque image.

| Conséquence | Détail |
|---|---|
| Coût mémoire | quelques dizaines d'octets par commande, au lieu de 3 Mio |
| Coût de rendu | proportionnel au nombre de commandes, pas à la surface |
| Protection | plafond de 256 commandes par surface, refus compté |
| Atomicité | double liste (`pending` / `committed`), publiée au `COMMIT` |

### Couches et z-order

`Layer` est figée par construction ; le `z` demandé par un client ne joue qu'à
l'intérieur de sa couche.

| Couche | Contenu | Focusable | Cliquable |
|---|---|---|---|
| `Wallpaper` | fond d'écran | non | oui |
| `Window` | fenêtres d'applications | oui | oui |
| `Shell` | barre des tâches, lanceur | oui | oui |
| `Notification` | toasts | non | oui |
| `Modal` | plein écran, connexion | oui | oui |
| `Overlay` | curseur | non | **non** |

`Overlay` est exclue de `Stack::hit` : le curseur est toujours sous lui-même et
intercepterait chacun de ses propres clics.

### Étanchéité

`Painter` porte un rectangle de découpe et une origine. Une application dessine
dans son propre repère, part de (0, 0), et **n'a aucun moyen d'atteindre un
pixel qui ne lui appartient pas** — non par convention, mais parce que chaque
coordonnée passe par `Painter`, qui la ramène dans la zone. Le découpe se
compose : une sous-zone ne peut pas s'élargir. Vérifié par l'autotest, qui
dessine avec des coordonnées délirantes dans les quatre directions et relit les
pixels alentour.

### Publication par rectangle endommagé

La limite ouverte par la 7.1 — chaque ligne républiait l'écran entier — se
ferme ici. Le compositeur accumule un rectangle endommagé et ne publie que les
lignes concernées. Déplacer une fenêtre coûte la surface qu'elle occupait plus
celle qu'elle occupe.

L'union plutôt qu'une liste de rectangles : deux zones éloignées donnent un
englobant plus large que nécessaire, mais une liste demanderait de fusionner
les recouvrements à chaque ajout — et peut oublier une zone, ce que l'union ne
peut pas.

**Toute la peinture d'une image a lieu à l'intérieur de la zone**, pas
seulement sa publication. `render` ouvre une `Painter::region(damage)` et
chaque surface est rejouée dedans, en coordonnées relatives. Le découpe de
`Painter` se composant, une surface ne peut pas peindre au-delà de ce qui sera
publié. Écrire plus large que ce qu'on publie ferait diverger le tampon arrière
du tampon avant, et la divergence ne se verrait que plusieurs trames plus tard,
ailleurs (voir `DECISIONS.md`, piège 35).

### Routage des événements

| Événement | Destinations |
|---|---|
| Pointeur | la surface `Overlay` (le curseur) **et** la surface désignée par `hit` |
| Touche ordinaire | la surface focalisée |
| Raccourci §16.4 | tous les propriétaires de surfaces `Shell` |

`Stack::hit` ignore `Overlay` — le curseur intercepterait chacun de ses propres
clics — mais l'exclure du ciblage n'est pas l'exclure de la livraison : c'est ce
module qui déplace la surface du curseur.

`MOD_SUPER` est posé par le compositeur, qui tient l'état de la touche : Super
n'est pas un modificateur du jeu de scancodes 1, le pilote ne la suit pas.

### Format `.mod` (7.3)

En-tête de 128 octets, magic `MOD\0`, décodé champ par champ.

```text
  0x00  4  magic          0x10 24  app_id
  0x04  2  version        0x28 24  nom affiché
  0x06  1  emplacement    0x40 24  auteur
  0x07  1  signature      0x58  8  version du module
  0x08  4  drapeaux       0x60 32  SHA-256 de la charge utile
  0x0C  4  taille charge  0x80 --  charge utile
```

Un format à part, et non un sixième `BinaryFormat` : un module GUI ne se lance
pas, il **occupe un emplacement**, et il porte deux informations qu'aucune
application n'a — lequel, et s'il est remplaçable. La spec déclare la hiérarchie
du §3.1 close.

| Signature | Règle |
|---|---|
| SYSTEM | l'empreinte doit figurer au magasin du noyau (`signature::is_official`) |
| ENTERPRISE | accepté, même réserve qu'en phase 3 (pas de signature asymétrique) |
| SELF / aucune | accepté dans un emplacement **remplaçable** seulement |

Le drapeau `REPLACEABLE` et le tableau §8.2 doivent dire la même chose : un
fichier qui se déclarerait remplaçable dans `Core` est refusé, pas corrigé.

### Cycle de vie et priorité

| Étape | Mécanisme |
|---|---|
| Détection (7.3.1) | `module::scan` sur `/sys/gui/modules/`, `boot/`, `accessibility/` |
| Signature (7.3.2) | empreinte recalculée, puis `authorize` |
| Isolation (7.3.3) | une tâche par module ; mémoire conventionnelle (un seul CR3) |
| Pause (7.3.4) | `nom.mod` → `nom.pause`, signal `TAG_MODULE_STOP`, surface rendue |
| Priorité (7.5.1) | tri par emplacement puis provenance décroissante, premier gagne |

Un module système remplacé passe à `Shadowed` et **reste visible au
diagnostic** : le faire disparaître rendrait la priorité invisible à qui cherche
pourquoi son module ne s'applique plus.

### Contre-pression IPC

Une boîte aux lettres plafonne à 64 messages. Le fond d'écran émet 36 commandes
pour un seul repeint, la barre des tâches une vingtaine : deux modules qui se
redessinent en même temps saturaient la file, et l'écran de connexion se voyait
refuser sa surface. `Client::deliver` cède la main et réessaie plutôt que de
laisser tomber le message — une commande perdue au milieu d'une liste ne se voit
pas comme une erreur, elle se voit comme une fenêtre mal dessinée.

Sans risque de blocage circulaire : le compositeur ne se bloque jamais sur un
client.

### Notifications (7.6)

Politique dans `gui::notify`, côté compositeur ; rendu dans `notifs.mod`,
remplaçable. La séparation n'est pas cosmétique : un remplaçant peut changer
l'apparence des toasts, il ne doit pas pouvoir laisser passer une PRIORITAIRE
émise par un `.gs` ni raccourcir l'échéance d'une URGENT.

`Any([...])` du §9.2 est le cas général : `Dismissal` est un vecteur de
conditions, et « une seule condition » est le vecteur à un élément. Un vecteur
vide ne fait jamais disparaître — ne rien déclarer, c'est ne rien demander.

### Connexion (7.7)

SHA-256 sur `sel || mot de passe`, sel par compte, dans `/sys/config/accounts.cfg`
en `Mode::private_file()`. **Ce n'est pas un stockage sûr** : SHA-256 est conçu
pour être rapide, et une attaque hors ligne essaierait des millions de candidats
par seconde. Il faudrait Argon2 ou scrypt. Le sel n'est pas non plus
imprévisible — il n'y a aucune source cryptographique dans le noyau.

Anti-bruteforce en mémoire pour les temporisations, **persisté pour le
verrouillage** : une temporisation perdue au redémarrage coûte trente secondes à
l'attaquant, un verrouillage perdu lui rendrait le compte.

Les compteurs ne se remettent à zéro qu'à une connexion réussie : sinon attendre
la fin d'une temporisation donnerait un essai gratuit toutes les trente
secondes.

---

## 11 quater. Gestionnaire de paquets et mises à jour (phase 8)

### Trois barrières avant la première écriture

C'est la première voie par laquelle du contenu extérieur entre dans le
système, et l'ordre des contrôles est l'invariant qui compte :

1. **La forme** — `package::parse` refuse un fichier dont une section
   déborde ou recouvre une autre, avant qu'aucune ne soit lue. Deux
   sections superposées rendraient le contenu dépendant de l'ordre de
   lecture, et un paquet pourrait présenter un manifest anodin à
   l'inspection tout en installant autre chose.
2. **L'intégrité et l'origine** — `package::verify` reprend les verdicts
   et les motifs de la phase 3, table §3.4 comprise : un paquet ne peut
   pas livrer un `.dsa` auto-signé sous prétexte que son emballage serait
   en règle.
3. **Le consentement** — le tableau §10.4, dont deux lignes sur cinq
   exigent une réponse humaine, obtenue par `prompt::ask` dont
   l'expiration refuse.

Une installation refusée ne laisse rien derrière elle. L'autotest le
vérifie en comptant les entrées de `/apps/` avant et après.

### Format `.spm` (8.1)

En-tête de 256 octets, décodé champ par champ, puis cinq sections
positionnées par offset : manifest, binaire principal, ressources, script
pre-installation, script post-installation.

**Deux empreintes, deux questions.** Celle de charge utile couvre ce qui
suit l'en-tête et répond à « le fichier est-il arrivé entier ». Celle
d'authenticité couvre l'en-tête — champs dérivés neutralisés — puis la
charge utile, et répond à « est-ce exactement ce qu'un éditeur a produit ».

**Deux champs sont neutralisés, pas un.** L'empreinte de charge utile
(0xA0) parce qu'une empreinte ne peut pas se contenir elle-même ; la
liaison de certificat (0xC0) pour la même raison, par un détour : la
liaison vaut `SHA-256(clé || empreinte d'authenticité)`, elle dépend donc
de l'empreinte, qui couvre l'en-tête, qui contient la liaison. La clé de
l'autorité (0xE0) reste couverte — c'est une entrée du calcul, et la
couvrir est ce qui empêche de substituer une autorité à une autre.

### Scripts déclaratifs (8.1.3)

Six verbes — `mkdir`, `write`, `copy`, `delete`, `require`, `echo` — sans
branchement ni boucle. L'installeur tourne sous les droits du noyau parce
qu'il écrit dans `/sys/` et `/apps/` ; y faire exécuter du code arbitraire
offrirait à n'importe quel `.ras` non signé le chemin le plus court vers
l'anneau 0.

Chaque chemin est normalisé puis comparé aux racines autorisées, qui sont
le préfixe d'installation et rien d'autre. Le contrôle porte sur le
**programme entier avant toute exécution** : un script dont la dernière
ligne déborde ne doit pas avoir exécuté les précédentes. `link` n'existe
pas comme verbe, précisément parce qu'un lien virtuel du §4.1 posé dans le
préfixe et pointant au-dehors ferait sortir une écriture du périmètre.

### Base des paquets

Une ligne par paquet dans `installed.db`, une liste de fichiers par paquet
dans `files/{app_id}.list`. La liste est tenue **au fur et à mesure** de
l'installation et relue **à l'envers** à la désinstallation : un dossier ne
se supprime qu'une fois vide, et un fichier ajouté après coup par
l'utilisateur fait donc échouer la suppression de son dossier — c'est
voulu, et le compteur `conserve(s)` le dit.

### Dépôts (8.3)

Un index texte et des `.spm` rangés à côté. `file:` lit le système de
fichiers, `http://` passe par le client TCP de la phase 6.2 en HTTP/1.0 —
la version 1.0 ferme la connexion en fin de réponse, ce qui rend la fin du
corps observable sans décoder `Content-Length` ni le découpage en morceaux.

**Trois vérifications distinctes**, qu'il ne faut pas confondre :
le sceau de l'index (le dépôt a-t-il publié ceci), l'empreinte du fichier
reçu (est-ce ce que l'index annonce), la signature du paquet (qui l'a
produit). Un téléchargement réussi ne vaut pas confiance.

### Mises à jour de l'OS (8.4)

**Les tables §10.2 et §11.1 emploient les mêmes nombres pour des choses
différentes** : le code 2 vaut « aucune notification » au §10.2 et
« nouvelle partition, redémarrage obligatoire » au §11.1. Ce sont deux
types Rust distincts, et la traduction de l'un vers l'autre est écrite une
seule fois, dans `OsUpdateKind::notification_code`. Aucun type de mise à
jour de l'OS n'est silencieux : le §11.2 promet un *téléchargement*
silencieux, jamais une application silencieuse.

L'intervalle est relu à chaque tour de boucle et non une fois au
démarrage : un service de mise à jour est précisément celui qu'on ne veut
pas redémarrer pour changer un réglage. Le sommeil de vingt-quatre heures
est découpé en tranches d'une minute pour cette raison.

### Rollback (8.5)

Deux emplacements, `A` et `B` ; lequel est actif est un choix consigné
dans `/sys/updates/state.cfg`, pas une propriété du disque. `B` prend le
second volume monté s'il en existe un, et retombe sinon sur un dossier du
volume de démarrage — le repli protège contre le même accident, pas contre
la même panne.

**Le compteur d'essais est incrémenté avant la validation, pas après.** Un
système qui se fige pendant la fenêtre de soixante secondes ne repasserait
jamais par le code qui incrémente, et redémarrerait indéfiniment sur la
version cassée. Au deuxième essai infructueux, le rollback part sans qu'on
ait eu besoin d'atteindre quoi que ce soit.

Les snapshots sont horodatés par le RTC. **Un snapshot daté à zéro n'est
jamais purgé** : on ne sait pas quel âge il a, et supprimer par défaut ce
qu'on ne sait pas dater effacerait le seul recours d'une machine sans pile
CMOS.

---

## 11 quinquies. Passe d'optimisation

### Ce qui coûtait, et ce que ça coûtait

| Point | Avant | Après |
|---|---|---|
| Sortie série d'un démarrage | 16 806 lignes, dont 12 288 de `[ping]` | ~1 300 lignes |
| Tâches en boucle serrée permanente | 5 | 0 |
| Table des tâches | croît sans fin | bornée aux tâches vivantes |
| `pick_next` | 3 parcours par élection | 1 |
| Publication d'un curseur déplacé | ~74 Kio | ~1,3 Kio |
| Octets alloués au repos | dérive de ~160 o / 5 s | strictement constant |

### Les tâches de démonstration sont bornées

`ping`/`pong` (2.6) et `calcul-a`/`calcul-b` (2.7) démontrent le rendez-vous
IPC et la répartition sur les cœurs. Elles le démontraient en boucle
infinie. Elles s'exécutent maintenant un nombre fixe de fois, annoncent
leur résultat et sortent — `DEMO_ROUNDS` et `DEMO_ITERATIONS` dans
`lib.rs`. Le moniteur dort entre deux relevés au lieu de scruter l'horloge.

Les compteurs du moniteur partent toujours toutes les cinq secondes : ce
sont eux qui portent les invariants. Les **tableaux** (`dump_tasks` et les
états des sous-systèmes) s'espacent à trente secondes.

### La table des tâches ne bouge plus en mémoire

`tasks: Vec<Box<Process>>`. La `Box` n'est pas décorative : `switch` relève
`&raw mut sched.tasks[previous].rsp`, relâche le verrou, puis commute. Avec
un `Vec<Process>`, une réallocation concurrente ferait écrire le RSP
sortant dans de la mémoire libérée. Une `Box` ne bouge jamais.

Un emplacement mort et réclamé est **réemployé** à la création suivante
plutôt que retiré : `current[cpu]` désigne des positions, et un retrait
décalerait celles des autres cœurs.

### Publication par rectangle

`Framebuffer::present_area` arbitre entre `present_rows` — une recopie
contiguë de lignes entières — et `present_rect`, qui découpe par colonnes.
Le seuil est aux trois quarts de la largeur de l'écran : au-delà, la
contiguïté reprend l'avantage sur les colonnes économisées.

### Souris : où le mouvement se perdait

Il ne se perdait nulle part dans le système. La chaîne IRQ 12 → `ps2` →
`mouse.rs` → `dev.mouse` → compositeur → `cursor.mod` → `move_surface` est
correcte de bout en bout, vérifiée par injection de mouvement au moniteur
QEMU et par capture d'écran avant/après.

Ce qui manquait est en amont du système : la souris émulée est une PS/2
**relative**, et le display GTK ne transmet un déplacement qu'une fois le
pointeur de l'hôte **capturé**. `make run` passe désormais
`-display gtk,grab-on-hover=on`, qui capture au survol ; la capture se
relâche avec Ctrl+Alt+G.

---

## 11 sexies. Pile USB et pointage

### Pourquoi l'USB alors que le PS/2 fonctionne

Le PS/2 fonctionne — IRQ 12 comprise, mesurée. Mais une souris PS/2 ne
rend que des **déplacements**. Elle n'a pas de position à communiquer, et
un hôte qui l'émule doit donc confisquer le pointeur physique avant de
transmettre quoi que ce soit. C'est ce que fait la capture d'une fenêtre
d'émulateur, et c'est ce qui donne l'impression que la souris ne marche
pas tant qu'on n'a pas cliqué dedans.

Un pointeur **absolu** transmet une position, qui a un sens sans capture.
Sur du matériel réel la question ne se pose pas autrement : les machines
construites depuis quinze ans n'ont plus de port PS/2, et le 8042
qu'elles exposent encore est lui-même émulé par le firmware.

### xHCI et pas UHCI

UHCI serait cinq fois plus court, mais c'est de l'USB 1.1 : un pilote
UHCI ne fonctionnerait que sous émulation, ce qui revient à écrire un
pilote pour QEMU plutôt que pour un ordinateur. xHCI pilote toutes les
vitesses sur le même jeu de registres.

### Les quatre bancs de registres

Trois des quatre sont trouvés par un offset lu dans le premier ; aucune
adresse n'est fixe.

```
  BAR0                capacités  (CAPLENGTH, HCSPARAMS, HCCPARAMS, DBOFF, RTSOFF)
  BAR0 + CAPLENGTH    opération  (USBCMD, USBSTS, CRCR, DCBAAP, CONFIG, ports)
  BAR0 + RTSOFF       exécution  (interrupteurs, anneau d'événements)
  BAR0 + DBOFF        sonnettes  (une par slot)
```

### Anneaux et bit de cycle

Trois familles d'anneaux, dont le sens de circulation s'inverse :
l'anneau de **commandes** et les anneaux de **transfert** vont du pilote
vers le contrôleur, l'anneau d'**événements** dans l'autre sens.

C'est ce qui rend le **bit de cycle** indispensable : un anneau n'a ni
tête ni queue observable, seule l'alternance de ce bit distingue un
enregistrement neuf d'un vieux déjà consommé. Chaque lecteur garde son
propre état de cycle et l'inverse à chaque tour ; un TRB de liaison au
bout de l'anneau porte le bit de bascule qui fait faire de même au
contrôleur.

Un TRB s'écrit **champ de contrôle en dernier** : c'est lui qui porte le
cycle, et le contrôleur peut lire l'anneau à tout instant.

### Séquence d'adoption d'un périphérique

```
  1  réinitialiser le port, attendre que le lien soit établi
  2  Enable Slot            → identifiant de slot
  3  contextes d'entrée et de sortie, anneau de contrôle
  4  Address Device         → le périphérique répond à son adresse
  5  GET_DESCRIPTOR(device)
  6  GET_DESCRIPTOR(config) — en-tête, puis la longueur qu'il annonce
  7  Configure Endpoint     → l'endpoint d'interruption devient actif
  8  SET_CONFIGURATION
  9  SET_PROTOCOL(boot)     — seulement si l'interface s'en réclame
 10  armer un transfert entrant, et le réarmer à chaque rapport
```

Le point 10 n'est pas facultatif : un endpoint d'interruption sans TRB en
attente ne produit plus rien, et la souris se figerait après son premier
mouvement.

### Deux formes de rapport

| Forme | Longueur | Contenu |
|---|---|---|
| Souris d'amorçage | 3–4 o | boutons, dx, dy signés 8 bits, molette |
| Tablette absolue | 6–8 o | boutons, X et Y sur 16 bits (0..32767), molette |

La forme est décidée par ce que **l'interface déclare** (`3.1.2` = HID,
boot, souris) et, à défaut de déclaration, par la longueur du rapport.
L'inverse — deviner d'après la longueur seule — prendrait une souris
d'amorçage à endpoint de huit octets pour une tablette, et lirait son
deuxième octet, un déplacement, comme la moitié basse d'une position.

Analyser réellement le descripteur de rapport demanderait un interprète
d'éléments HID : plusieurs centaines de lignes pour un résultat que ces
deux informations donnent déjà. Réserve consignée.

### Les deux bus alimentent le même pointeur

`drivers::mouse` tient la position, les boutons et la surface, et publie
sur `dev.mouse`. Rien en aval ne sait laquelle des deux sources a produit
un événement — le compositeur n'a pas à connaître la topologie du bus.

Trois entrées, un seul état :

| Entrée | Source | Axe vertical |
|---|---|---|
| `decode_byte` | paquet PS/2 | inversé (la souris pointe vers le haut) |
| `apply_relative` | souris USB | direct (le HID pointe vers le bas) |
| `set_absolute` | tablette USB | direct, converti depuis l'échelle 0..32767 |

### Scrutation plutôt qu'interruption

Le contrôleur sait émettre une interruption MSI-X ; la consommer
demanderait de programmer une table MSI-X, ce que rien d'autre dans le
système ne fait. La tâche pilote scrute donc l'anneau d'événements —
toutes les 8 ms quand un pointage USB répond, 200 ms sinon, l'échéance
étant celle sur laquelle elle attend déjà l'IRQ 12 du PS/2.

---

## 11 septies. SDK et langages (phase 9)

### La chaîne complète, en cinq étapes

C'est la première fois que le système transforme **du texte écrit par un
humain** en un binaire qui s'exécute en ring 3. Jusqu'ici, tout ce qu'il
exécutait avait été fabriqué par le noyau lui-même.

```text
  dev.toml  --> manifest::parse   --> DevManifest validé (§3.4, §12.4, §13.2)
  main.asm  --> asm::assemble     --> Object : octets, étiquettes, trous
            --> emit::link        --> déplacements RIP résolus
            --> emit::emit        --> fichier .cus/.gs/.dsa complet
            --> loader::load      --> tâche ring 3
```

### Carte des modules

| Module | Couvre | Lignes de force |
|---|---|---|
| `sdk::runtime` | 9.5 | table §13.2 en données ; résolution `/users/{u}/runtimes` > `/sys/runtimes` |
| `sdk::manifest` | 9.2 | TOML restreint ; refuse une clé inconnue au lieu de l'ignorer |
| `sdk::asm` | 9.5.7 | deux passes, sauts toujours `rel32`, aucun code non position-indépendant |
| `sdk::emit` | 9.1.2 | pas de section `.reloc` ; `.text` et `.data` sur des pages distinctes |
| `sdk::script` | 9.1.4 | budget de pas, profondeur bornée à 24 |
| `sdk::template` | 9.1.1 | cinq gabarits, tous assemblables |
| `sdk::sandbox` | 9.3 | arène `0x3000_0000`, overlay, loopback, rejeu `.ras` |
| `sdk::etr` | 9.6 | lexeur JS (le piège du `/`), AST, correspondance, seuil 70 % |
| `sdk::libdev` | 9.4.3 | catalogue vérifié contre `syscall::table::lookup` |
| `sdk::cli` | 9.1 | six verbes ; `execute` attend une chaîne, comme `spm` |
| ~~`sdk::devstudio`~~ | 9.4 | retiré en septembre 2026 ; `apps::install_native` efface `/sysapp/devstudio.ss` |
| `sdk::rustc` | 9.5.1 | sous-ensemble Rust : compilé en x86_64 **et** interprété, un seul arbre |

### Le chemin d'un programme Rust

```text
  main.rs
    lex     -> jetons + lignes
    parse   -> ast::Program            (hors sous-ensemble : refus nomme)
    check   -> types, mutabilite, arite, retour certain, emplacements de pile
      |
      +-- codegen -> TEXTE d'assemblage -- sdk::asm --> Object
      |                                    sdk::emit -> .cus, ring 3
      +-- interp  -> valeurs et lignes de sortie   (dev interpret)
```

Le générateur émet du **texte** et le confie à `sdk::asm`, comme `rustc`
confie sa sortie à un assembleur. Un second encodeur d'instructions dans
le dépôt dériverait du premier, et la dérive se verrait comme un
programme qui tourne et fait autre chose — jamais comme une compilation
qui échoue.

Disposition retenue par `codegen` :

```text
  [rbp + 8]   adresse de retour
  [rbp]       rbp sauve
  [rbp - 8]   emplacement 0   <- parametres d'abord, dans l'ordre declare,
  [rbp - 16]  emplacement 1      puis les locales dans l'ordre des `let`
```

Toute expression laisse sa valeur dans `rax` ; une opération binaire
empile la gauche, évalue la droite, dépile. C'est une **machine à pile**,
et c'est un choix : `syscall` détruit `rcx` et `r11`, donc avec toute
valeur vivante sur la pile un `println!` au milieu d'une expression ne
peut rien corrompre. Le code utilisateur engendré ne touche que `rax`,
`rcx`, `rdx` et les six registres d'argument — ce qui laisse `r12` à
`r15` aux routines d'exécution, qui s'en servent sans les sauver.

Une valeur vaut **un mot**, y compris un `&str` : c'est un pointeur vers
un bloc de `.data` qui porte sa longueur puis ses octets.

```text
  __nova_str_0:  dq 12
                 db "bonjour Nova"
```

Les routines d'exécution (`__nova_buf_*`, `__nova_str_eq`,
`__nova_panic_div0`) ne sont émises **que si le programme les appelle** :
un programme qui n'imprime rien ne porte pas la conversion décimale.

### La barrière de démarrage du SDK

`sdk::start` construit l'arborescence `/sys/runtimes`, puis **purge** ce que `/cache/sandbox/` contient encore.
Cette purge n'est correcte que si aucun bac vivant ne s'y trouve, et rien
dans l'ordonnanceur ne garantit qu'une tâche créée avant `start` ne le
double pas. Deux garde-fous, tous deux nécessaires :

| Élément | Rôle |
|---|---|
| `sdk::is_started()` | Drapeau atomique posé **en dernier** par `start`. C'est la barrière que doit attendre toute tâche qui crée un bac à sable. |
| `sdk::sandbox::live_ids()` | Identifiants des bacs vivants. La purge saute tout dossier qui en porte un : un résidu et un bac en cours d'usage sont indiscernables sur le disque. |

L'autotest de la phase 9 attendait auparavant `runtime::resolve(Asm)`.
Cette condition porte sur un fichier **persistant** : vraie d'emblée dès
le deuxième démarrage, elle ne synchronisait plus rien. Voir
`DECISIONS.md`, « Une purge au démarrage n'a de sens que si rien n'a
démarré avant ».

### Ce qui a été ajouté hors de `sdk/`

- `fs::perm::Mode::shared_file` — le pendant de `shared_dir` au niveau du
  fichier. Sans lui, un fichier créé dans `/cache/`, `/ras/` ou
  `/users/_shared/` naissait privé : le dossier acceptait qu'on y écrive,
  et le fichier écrit devenait aussitôt intouchable pour tout autre
  compte. Un dossier partagé dont le contenu ne l'est pas ne partage rien.
- `gui::surface_named` — un autotest qui lance une application a besoin de
  savoir qu'elle a **obtenu sa fenêtre**, pas seulement qu'elle est
  vivante. La question porte sur le titre : c'est ce que le compositeur
  connaît de l'application, et c'est aussi ce qui reste vérifiable après
  sa mort.

### Disposition mémoire ajoutée

| Base | Taille | Contenu |
|---|---|---|
| `0x30000000` | 128 MiB | Arène des bacs à sable (9.3.1), plafond de 256 pages par bac |

Quatrième arène disjointe, après les images (`0x1000000`), les piles
(`0x10000000`) et le tas de `SYS_MEM_ALLOC` (`0x20000000`). Même raison
qu'en 5.4 : une fuite dans l'une ne doit pas masquer l'épuisement d'une
autre.

### Pourquoi l'assembleur n'émet aucune relocation

`sdk::asm` ne produit que du code position-indépendant : toute référence à
une donnée passe par `lea r64, [rip + étiquette]`, tout saut par un
déplacement relatif. Ce n'est pas une simplification — c'est le mode le
plus sûr, puisqu'une table de relocations est précisément ce qu'un
attaquant falsifierait pour faire écrire le chargeur n'importe où.

La contrepartie est que le déplacement RIP ne peut pas être calculé par
l'assembleur : `.data` n'a pas encore d'adresse quand `.text` s'assemble.
L'assembleur laisse un trou de quatre octets et enregistre un `Fixup` ;
`emit::link` les résout une fois la disposition arrêtée. C'est exactement
la frontière assembleur / éditeur de liens, et la respecter évite d'avoir
à connaître la disposition en deux endroits.

Le calcul ne fait intervenir que des **écarts internes à l'image** :
l'adresse de chargement s'annule des deux côtés de la soustraction, et
`LOAD_BASE` n'apparaît pas dans la formule. C'est ce qui rend le résultat
valable à n'importe quelle base — et c'est vérifiable en relisant `link`.

### Le bac à sable, couche par couche

```text
  écriture --------> couche haute : /cache/sandbox/{id}/...   (toujours)
  lecture  --+-----> couche haute si présente
             +-----> pierre tombale ? -> introuvable
             +-----> couche basse, bornée par tree::sandbox_roots (§12.4)
```

Un fichier effacé dans le bac ne peut pas l'être en dessous : on pose une
**pierre tombale**, un marqueur `{chemin}.tombe` qui masque l'entrée de la
couche basse sans y toucher. C'est la seule façon de rendre `delete`
observable sans écrire sur l'hôte.

Le §12.4 sert ici pour la première fois à quelque chose : la limite « le
sandbox par chemin est décrit, pas appliqué », ouverte par la phase 4, se
ferme pour le SDK — et pour lui seul.

### Le rejeu en `.ras` n'est pas cosmétique

Le §15.4 impose que l'application testée tourne en `.ras`. Le §3.4 rend
l'exigence contraignante : un `.ras` est **non signé**
(`combination_allowed` n'admet que `(Ras, None)`), plafonné à trois
permissions (§12.4), et ne peut porter **aucun port interne** (§12.3 —
`check_rules` refuse un `.ras` dont la table de ports n'est pas vide).

Rejouer le binaire du projet en `.ras`, c'est donc le **reconstruire** :
signature retirée, permissions rabotées, ports supprimés, drapeaux
`EXPOSES_API` / `REPLACES_COMPONENT` / `AUTO_PAUSE` / `DEFAULT_CAPABLE`
retirés. Un `.dsa` SYSTEM devient testable **grâce à** cette
transformation : le chargeur refuserait son empreinte inconnue, il accepte
le `.ras` qui en sort.

### Le loopback est une pile séparée, pas une restriction

Il n'y a pas d'interface de bouclage dans `net` : la pile de la phase 6
parle à une carte e1000, et rien d'autre. Le bac à sable en fournit une,
entièrement en mémoire — des ports, des files, et une règle : toute
adresse hors `127.0.0.0/8` est refusée. Le trafic d'un bac ne peut pas
atteindre le pilote parce qu'**il n'y a pas de chemin qui l'y mène**.

### Le lexeur JavaScript et le piège du `/`

Un `/` est une division ou le début d'une expression régulière selon ce
qui précède : une expression régulière ne peut apparaître que là où une
**valeur** est attendue — après une ponctuation ou un mot-clé, jamais
après un identifiant, un nombre, une chaîne ou une parenthèse fermante.

Se tromper là-dessus fait avaler la moitié du fichier dans une chaîne, et
le taux de conversion s'effondre sans raison — ou grimpe, ce qui est pire.

### Comment le taux ETR se calcule

Sur les **sites d'appel**, pas sur les lignes. Le §13.1 dit « taux de
réussite » sans dire de quoi ; c'est la seule des trois lectures possibles
qui mesure ce qui va réellement manquer à l'exécution. Un fichier de mille
lignes d'arithmétique se convertit entièrement ; un fichier de dix lignes
qui appelle `document.querySelector` ne se convertit pas du tout.

Deux parcours de l'arbre, et l'ordre compte : le premier recense les
fonctions que le projet déclare, le second compte. Un appel à une fonction
du projet est **converti** — elle sera traduite avec le reste. Les compter
comme des manques faisait chuter le taux d'un projet bien structuré, ce
qui est l'inverse de la vérité.

Le taux est un **entier** de 0 à 100. Le noyau tourne en soft-float sans
SSE : un `f32` ici serait un `f32` dans le noyau.

---

## 11 octies. Finitions (phase 10)

### Quatre sous-systèmes, un point d'entrée chacun

| Sous-système | Point d'entrée | Ce qu'il décide |
|---|---|---|
| `access` | `handle_key`, `map_pixel` | ce que fait une touche du §16.4, à quoi ressemble un pixel |
| `i18n` | `translate`, `settings` | dans quelle langue et sous quelle forme un nombre s'écrit |
| `security` | `authorize`, `check_path` | si un accès a lieu |
| `compat` | `personality_of`, `dispatch` | ce que veut dire `syscall` pour cette tâche |

Chacun a **un** point d'entrée, et ce n'est pas de la propreté : une règle
appliquée à deux endroits n'est appliquée nulle part, parce que les deux
divergent. `security::policy::evaluate` est le seul endroit du noyau où
l'ordre des quatre étapes du §12.3 est écrit ; l'autotest vérifie
**l'ordre**, pas seulement les branches.

### Le partage module / compositeur pour le zoom et le contraste

Le §8.1 dit que seul le compositeur écrit sur le framebuffer. Le §16.2 dit
que `zoom.mod` et `contrast.mod` sont remplaçables. Si le module peignait,
le §8.1 tomberait ; si le compositeur décidait, le §16.2 tomberait.

Le partage qui satisfait les deux : **le module possède la politique, le
compositeur possède les pixels.**

```
zoom.mod / contrast.mod          access::* (atomiques)        compositor::render
  decide la forme, le facteur  →   lus une fois par trame   →  present_area_mapped
  et le mode couleur                                           applique zoom puis couleur
```

L'état est en atomiques et non sous verrou : il est lu sur le chemin de
rendu, par une tâche qui tient déjà le verrou du compositeur. Un second
verrou là créerait un ordre — COMPOSITOR puis ACCESS — qu'un module devrait
respecter à l'envers dès qu'il voudrait lire le compositeur.

La transformation s'applique **entre le tampon arrière et l'écran**
(`framebuffer::present_area_mapped`), jamais sur les instructions de
dessin : une application dessine ce qu'elle dessine, et rien au-dessus de
l'étage de publication ne sait que l'écran est grossi.

### Pourquoi l'ordre zoom puis couleur

Les deux transformations commutent tant que la couleur est calculée pixel
par pixel. Elles cessent de commuter dès que la transformation de couleur
regarde ses voisins — ce dont l'étirement de contraste n'est qu'à un pas.
Fixer l'ordre maintenant ne coûte rien et supprime une surprise.

### La synthèse vocale, en entiers

Le noyau est en soft-float : un `f32` ici serait un `f32` dans le noyau.
Un résonateur à deux pôles s'écrit normalement avec un cosinus et une
exponentielle ; ici les coefficients sont calculés une fois, en virgule
fixe Q12, à partir d'une série de Taylor repliée sur le premier quadrant.

```
texte → graphemes()  → phonèmes → render() → PCM 48 kHz mono
        règles FR       29 sons     3 résonateurs parallèles
                                    + source glottale ou bruit
```

Trois résonateurs et non cinq : chaque pôle supplémentaire est un produit
Q12 par échantillon et par formant, et à 48 kHz ce budget est réel. Trois
formants suffisent à distinguer les voyelles françaises, ce qui est ce
qu'un lecteur d'écran demande.

Les résonateurs sont en **parallèle** et non en cascade : une cascade
multiplie les pics et enterre le troisième formant.

### Le lecteur demande au pilote, pas au mixeur

`audio::open` réussit sur une machine sans carte son : le mixeur existe
qu'un contrôleur le joue ou non. La seule source qui sache est le
registre des pilotes, et il ne le sait qu'une fois la sonde passée.

```
gui::start_modules()  →  reader::task()  ─┐
                                          │  wait_for_audio() : attend que
drivers::start_all()  →  probe AC'97   ───┘  le pilote "audio" quitte
                         registre: Ready       l'etat Starting (5 s max)
                                 ou Absent   → drivers::audio::is_present()
```

L'ordre compte : le lecteur est un module du compositeur, il démarre donc
**avant** les pilotes. Interroger le registre sans attendre aurait
répondu « absent » sur toutes les machines. Le flux, lui, reste ouvert
par énoncé et non pour la vie du lecteur — un flux qui existe fait
tourner le pilote pour du silence.

### L'invalidation de TLB entre cœurs

Limite ouverte par la phase 3, fermée ici. Le mécanisme tient en un
compteur :

```
unmap_page → TLB_GENERATION++ → IPI vecteur 241 vers tous les autres cœurs
                                        │
     ┌──────────────────────────────────┼──────────────────────────┐
     ▼                                  ▼                          ▼
 handler d'IPI                    tic d'horloge            entrée en ring 3
 (immédiat)                       (10 ms au pire)          (là où ça compte)
     └────────────── memory::sync_tlb() : CR3 rechargé si en retard ─────┘
```

**Aucune attente d'acquittement.** Le premier essai en faisait une par
page libérée, chacune bornée à un million de tours avec les interruptions
masquées : libérer une image de dix-huit pages bloquait quatre cœurs.
L'attente n'achète d'ailleurs rien, à cause de ce que l'entrée périmée
peut atteindre — elle nomme une page **utilisateur**, qu'aucun code noyau
n'adresse. Le seul code qui peut s'en servir est une tâche ring 3, et tout
chemin vers le ring 3 appelle `sync_tlb` pour son propre compte.

Depuis 11.15, chaque cœur a sa propre génération demandée
(`CPU_TLB_WANTED`). La diffusion ci-dessus la lève pour tous ; les
espaces privés (`vm::Space`) invalident par
`shootdown_space_and_wait(cr3)`, qui ne vise et n'attend que les cœurs
pouvant tenir ce CR3 (`vm::may_hold` : `LOADING_CR3`, publié avant le
chargement, et `ACTIVE_CR3`, après).

### Un binaire étranger est un binaire Nova venu d'ailleurs

Il n'y a pas de second chargeur, pas de seconde arène, pas de second
ordonnanceur.

```
ELF64 / PE32+ → compat::convert → objet (.text/.data/.bss)
              → sdk::emit::emit → binaire .cus Nova complet
              → binfmt::loader  → même en-tête, même signature, même arène,
                                  même verify_protection, même bac à sable §12.4
```

Ce qui distingue la tâche, c'est sa **personnalité** :
`syscall_dispatch` la consulte avant de consulter la table Nova, parce
qu'un `write` Linux vaut 1 et que 1 vaut `SYS_DEBUG_PRINT` ici. La
recherche est une sonde sur un vecteur vide dans le cas courant, donc
gratuite pour toute tâche que les sous-systèmes n'ont pas chargée.

Les conventions d'arguments tombent juste pour Linux — RDI, RSI, RDX, R10,
R8, R9 des deux côtés — et pas pour Windows, dont `ntdll` déplace RCX dans
R10 avant le `syscall`. C'est le seul endroit où la traduction déplace un
registre au lieu de renuméroter un appel.

### Ce qui a été ajouté hors des quatre dossiers

| Fichier | Ajout |
|---|---|
| `apic.rs` | `VECTOR_TLB` (241), `broadcast_tlb_shootdown` |
| `idt.rs` | handler du vecteur 241, `sync_tlb` au tic d'horloge |
| `memory.rs` | `TLB_GENERATION`, `sync_tlb`, `shootdown`, `tlb_stats` |
| `graphics/framebuffer.rs` | `present_area_mapped` : publication transformée |
| `gui/compositor.rs` | premier refus des touches à `access`, centre du zoom, chemin transformé |
| `sdk/devstudio.rs` | `dev run` déplacé de F6 vers F9 : le §16.4 a pris F6 (fichier retiré depuis) |
| `sdk/asm.rs` | opérandes `[reg + dep]`, `mov` d'un octet, `imul` / `idiv`, décalages — ce dont un cadre de pile a besoin (9.5.1) |
| `sdk/template.rs` | les cinq gabarits du §15.5 existent aussi en Rust ; `entry_name` dépend du langage |
| `sdk/cli.rs` | `compile` aiguille sur le langage ; `dev convert` produit désormais un projet **Rust**, ce que son nom promettait |
| `gui/module.rs` | cinq emplacements du §16.2 + `Diagnostic` |
| `gui/mod.rs` | `session_user`, cinq modules natifs d'accessibilité |
| `gui/login.rs` | F1 allume vraiment le lecteur et la navigation |
| `apps.rs` | `permissions_of`, refus de réécrire une identité vivante |
| `drivers/keyboard.rs` | agencement consulté dans `i18n`, touche de bascule, `drain` |
| `drivers/audio.rs` | un tampon par réveil, tampon de travail alloué une fois |
| `access/reader.rs` | disponibilité audio lue au registre des pilotes, après attente |
| `audio/mod.rs` | accumulateur de mixage conservé d'un appel au suivant |
| `syscall/mod.rs` | `invoke_user`, `read_user_bytes`, `read_user_cstr`, `write_user_bytes`, sonde de personnalité |
| `syscall/domain_fs/mod.rs` | `guard` : le bac à sable du §12.4 appliqué dans `path_at` |
| `prompt.rs` | file du clavier vidée avant toute lecture de réponse |
| `init.rs` | `i18n::start` et `access::start` avant le compositeur |
| `lib.rs` | tâche `phase10-autotest` |

---

## 11 nonies. Navigateur Web Browser (phase 11)

Le navigateur est un fork de Firefox ESR 140 (`ports/web-browser/`)
construit sur l'hôte contre musl, dans une racine Alpine
(`tools/ports/alpine.sh`). Il a d'abord été exécuté par le sous-système
Linux (schéma ci-dessous) ; depuis la phase 12 il est un programme natif
(section suivante) et ce chemin n'est plus emprunté.

```
menu Nova "Navigateur"  -> apps::launch -> launch_web_browser (phase 11)
  /bin/sh /usr/bin/web-browser-nova          (racine = /apps/web-browser/root)
    web-browser (processus parent) --fork/execve--> processus de contenu
      widget headless + NovaDisplay            (patch ports/web-browser/nova/)
        open("/dev/nova-gui") ioctl(CREATE) mmap(pixels) -> posix::gui
          tache "nova-gui" : apps::window::run + canevas -> compositeur
        WebRender logiciel -> tampon arriere -> composition des popups
          -> pixels mappes -> ioctl(PRESENT)
        read(evenements 32 octets) -> fil NovaInput -> fil principal Gecko
```

| Élément | Où | Rôle |
|---|---|---|
| Espace privé | `vm::Space` | PML4 à moitié haute partagée, régions, fautes à la demande, copie sur écriture |
| Processus POSIX | `posix::Process` / `Thread` | un groupe de threads = des tâches Nova partageant l'espace |
| Racine | `posix::vfs::novafs` | un dossier du volume Nova, `/apps/web-browser/root`, écrit depuis l'hôte par `tools/nova-fs.py` |
| Fenêtre | `posix::gui` | `/dev/nova-gui`, pont vers `apps::window::run` |
| Interface Nova | `tools/ports/overlay/usr/lib/web-browser/` | `userChrome.css`, page d'accueil, `nova.cfg`, `policies.json` |
| Cache de pages | `novafs::cached_page` | page de fichier lue une fois (32 d'un coup, `fs::read_blocks_into_frames`), projetée en lecture seule partout (bit `CACHED`), copiée à la première écriture |
| Heure | `net::sntp`, `rtc::set_now_millis`, `posix::time::set_realtime_ns` | NTP au démarrage puis toutes les 6 h ; fuseau du compte vers `/etc/localtime` |
| Son | `posix::audio` (`/dev/nova-audio`), `tools/ports/alsa-nova/pcm_nova.c`, `/etc/asound.conf` | cubeb → ALSA `default` → greffon `nova` (conversion) → flux du mixeur → AC97 |
| Profil | `/users/{compte}/apps/web-browser`, monté sur `/home/nova` | recopié de `nova/profile` au premier lancement, puis conservé sur le disque |
| Échéances | `idt::fine_ticks` (cœur 0, 1 kHz), `posix::time::deadline_after_ns` | attentes Linux à la milliseconde ; `idt::ticks` reste le tic système de 10 ms |
| Son, cadence | `posix::audio` (`THRESHOLD`, `DELAY`), `drivers::audio::output_delay_frames` | le greffon dit sous quelle profondeur il veut être réveillé ; le délai lit la position du DMA, lissée entre les sauts de l'émulateur (`PlaybackClock`) |
| Réseau | `drivers::virtio_net` (MSI-X), `drivers::nic::task` | la tâche pilote dort jusqu'à la trame suivante ; interruption coupée pendant qu'elle vide l'anneau |
| Mesures | `posix::gui` (`[nova-gui] ecran`, `[profil] N ms apres l'entree`), `/nova-tests/sysbench`, `nova/perf.html`, `nova/bench.html`, `tools/ports/serve-test-pages.sh` | délai et régularité d'affichage ; calcul par thread, appels système, fautes et invalidations entre une touche et le titre d'une page ; coûts noyau comparés à l'hôte |

**Invariants.** Une commutation se fait toujours avec la base GS du noyau
chargée (`preempt_user`, `page_fault_entry`). `CR0.WP` est armé sur chaque
cœur : le noyau n'écrit jamais une page utilisateur en lecture seule.
Seul `user_copy` déréférence une adresse utilisateur depuis le ring 0.
`shootdown_and_wait` et `shootdown_space_and_wait` s'appellent sans
verrou et interruptions permises. Aucun vidage de diagnostic n'imprime sur
la série en tenant un verrou pris ailleurs à chaud (ordonnanceur, `CPUS`,
tables TCP et UDP).
Aucune adresse n'est distribuée ni aucun cadre installé tant qu'une
invalidation de l'espace est en vol (`Space::invalidating`,
`lock_settled`). Une entrée dont le cadre change est invalidée sur tous
les cœurs avant que l'écriture reprenne. Un cadre `CACHED` n'est jamais
inscriptible.

## 11 decies. Navigateur natif : libc Nova (phase 12)

```
    menu / connexion -> posix::launch_native_browser
      spawn_native(/usr/lib/web-browser/web-browser.cus, Abi::Nova)
        loader : en-tete .cus (IMAGE_LIEE, ABI 2), empreinte SHA-256,
                 ELF projete depuis l'offset 4096 du fichier
        interpreteur impose : /sys/runtimes/c/libc.so (musl + overlay)
          ldso de musl : repond lui-meme a libc.so, ld-musl-x86_64.so.1...
          -> libxul, GTK, ALSA d'Alpine lies a la libc Nova sans recompilation
    syscall (numero Nova dans RAX)
      syscall_dispatch : process.abi == Nova -> syscall::ported::dispatch
        -> objets de posix::* (fd, vfs, vm, net, signal, futex)
      process.abi == Linux -> linux::syscall::dispatch
        -> traduction des numeros Linux vers les memes objets posix::*
```

| Élément | Où | Rôle |
|---|---|---|
| Interface | `Abi` dans `posix::Process` | fixée à la création, héritée par `fork`, gardée par `execve` : un programme natif ne peut pas redevenir Linux |
| Numéros | `syscall::ported::nr`, `op` ; `nova-libc/overlay/src/nova/nova_abi.h` | les deux bouges ensemble ; erreurs en `-errno` (spec 6.7) |
| Traduction | `nova-libc/overlay/src/nova/nova_syscall.c` | les `__syscallN` de musl → appels Nova ; six fichiers assembleur réécrits (clone, vfork, retour de signal, TLS, annulation, `__unmapself`) |
| Image liée | `binfmt::header::flags::LINKED_IMAGE`, `posix::loader::linked_image`, `tools/nova-wrap.py` | en-tête vérifié, puis ELF projeté tel quel |
| Périphériques | `/sys/dev/fenetre`, `/sys/dev/son` (`vfs::devfs::nova_devices`) | mêmes objets que `/dev/nova-gui` et `/dev/nova-audio` |
| Autotest | `posix::selftest::run_native` | 11 programmes de `/nova-tests/natif` (dont `database` : SQLite en WAL sur le volume) |
| Comptage | `ported::counts` (par cœur), `[moniteur] syscalls : ... programmes portes N, sous-systeme Linux M` | une session native laisse M inchangé |

### `posix` et `linux` : les objets, puis la traduction

Jusqu'au 23 septembre 2026, les objets des programmes portés vivaient sous
`src/linux/`, écrits pour la personnalité Linux de la phase 11.5 : chaque
appel du navigateur natif passait par du code rangé comme Linux. Ils sont
désormais le module `posix` du noyau, et `linux` ne garde que ce qui est
propre à Linux :

| Module | Contenu |
|---|---|
| `posix/` | processus et threads (`Process`, `Thread`, `Abi`), descripteurs (`fd`, `files`, `pipe`, `poll`), sockets (`net`), projections (`mm`), signaux, horloges et attentes (`time`, `wait`), `fork`/`exec`/`wait` (`process`), chargeur ELF et image liée (`loader`), arbre de fichiers (`vfs` : `novafs`, `tmpfs`, `devfs`, `procfs`), fenêtre et son (`gui`, `audio`), lancement du navigateur, autotest natif |
| `linux/` | `syscall.rs` (numéros Linux → `posix`), `spawn` avec `Abi::Linux`, script de démarrage de l'image, autotest des programmes Linux statiques |
| `syscall/ported.rs` | numéros Nova (spec 6.7) → `posix` |

Règle : `posix` n'appelle `linux` que pour lire ses compteurs et ses noms
d'appels (statistiques, rapport de faute d'un programme Linux). Aucun
chemin d'un programme `Abi::Nova` ne traverse `linux`. `uname` répond
« Nova » à un programme porté, « Linux » à un programme Linux.

**Invariants.** Le noyau ne s'exécute jamais avec DF levé : chaque point
d'entrée en assembleur commence par `cld` (`page_fault_entry`), les
gestionnaires `x86-interrupt` le reçoivent du compilateur, `syscall`
d'`IA32_FMASK`. Une frame ne retourne à l'allocateur qu'une fois hors de
toute traduction : `Space::unmap` garde les régions retirées (et l'objet
dont elles tiennent peut-être la dernière référence) jusqu'après
`finish_invalidation` ; tmpfs retire, sans les libérer, les pages coupées
d'un fichier qui a été projeté partagé.

## 11 undecies. Pile graphique et GPU (phase 13)

### Les couches, de haut en bas

```text
  programme            nova_gfx.h -> syscall 0x0Cxx
  gpu::api             lit les structures champ par champ, -errno
  gpu (coeur)          poignees, quota, cmd::translate, SPIR-V, barrieres,
                       destructions differees, delai de garde, balayage
  backend::Context     le contrat des pilotes (version 0x00010000)
    venus::backend       objets et commandes Nova -> appels Vulkan
    venus::Session       instance, carte, device, file ; anneau de commandes
    venus::protocol      encodeur GENERE (68 commandes), verifie octet par octet
    module::ModuleDevice le meme contrat vers un pilote .kmd (ABI C)
  virtio_gpu::context  CTX_CREATE, blobs, SUBMIT_3D gardes (sans anneau)
  virtio_gpu::exchange attentes verrou relache, a l'execution
  virtio_gpu::queue    descripteurs indirects, reponses hors ordre, MSI-X

  gui (compositeur)    painter::Target -> gui::gpu::record (instances)
  gui::gpu             un pipeline, atlas, toiles ; image composee
  gpu::screen          seul un proprietaire noyau met une memoire a l'ecran
  virtio_gpu::scanout  ressource 2D adossee a la memoire GPU, plan curseur
```

### Une soumission, du programme au GPU de l'hôte

1. `SYS_GPU_SUBMIT` copie le tampon de commandes (≤ 1 Mio, allocation
   faillible) et attend, sans verrou, qu'il y ait moins de 16 soumissions
   en vol dans le contexte.
2. Sous le verrou dormant du contexte, `cmd::translate` valide tout et
   rend une liste de `backend::Command` ; une commande invalide refuse le
   tout.
3. Le backend Venus enregistre un tampon de commandes Vulkan (recyclé),
   ajoute les barrières et les transitions de première utilisation, et le
   termine par le **mot de fin** (`vkCmdFillBuffer` du numéro du travail).
   Le flux part par l'**anneau** de la session (mémoire partagée, lue par
   un fil du moteur de rendu de l'hôte) ; `vkNotifyRingMESA` sur la file
   de contrôle seulement si ce fil dort. Sans anneau : `SUBMIT_3D` gardé
   sur la timeline 1.
4. Le travail est fini quand le mot de fin l'atteint. `SYS_GPU_FENCE_WAIT`
   scrute ce mot 3 ms (entre deux cessions du cœur) puis dort ; la tâche
   `drv-gpu` fait de même à chaque tic ou interruption 65 : elle avance
   `completed`, exécute les destructions différées, réveille `gpu::EVENTS`
   et applique le délai de garde (au TSC).

### Mémoire partagée hôte-invité

La région de 8 Gio (`hostmem=8G`) est un espace d'adresses que le pilote
gère (`hostmem::Hostmem`, premier ajustement, pages). Un blob y est projeté
par `RESOURCE_MAP_BLOB` ; le noyau le voit à `0xFFFF83…` + décalage, un
programme par une région `[gpu]` de son espace (`GpuPager`, non héritée par
`fork`). Avant de rendre une plage, le cœur retire toutes les régions qui
la projettent (`Space::unmap_pager`), où que le programme les ait
déplacées.

### Contextes du noyau

L'autotest passe par `syscall::invoke`, le compositeur par les fonctions du
cœur (`gpu::object_create`, `gpu::submit`...) : même validation, seul le
propriétaire change (`Owner::Task`). Pour un tel propriétaire sans espace
d'adresses, `MEM_MAP` rend la vue noyau.

### Jusqu'à l'écran

Dès que le cœur est prêt, le compositeur ouvre son contexte et compose sur
le GPU (`gui::gpu`) : le peintre enregistre des instances, un pipeline les
dessine dans l'image composée sur la zone endommagée, le GPU copie cette
zone dans la **sortie** — une mémoire « relecture » que
`gpu::screen::show` a mise à l'écran comme support d'une ressource 2D ;
`TRANSFER_TO_HOST_2D` + `RESOURCE_FLUSH` (postés) la montrent. Loupe et
filtres : le GPU copie dans une seconde mémoire, le processeur applique
`access::map_pixel` vers la sortie. Une image que le GPU ne finit pas en
100 ms met le compositeur en **suspension** : le processeur peint dans la
sortie sans toucher au périphérique, jusqu'à la fin de l'image en retard.
Le pointeur passe par le plan curseur (deux ressources 64×64 alternées). Le
framebuffer de la console reste dessous : la panique le remet à l'écran
(`scanout::reclaim`), et le processeur le reprend si le GPU se perd trois
fois. L'affichage direct d'un blob GPU est retiré (DECISIONS : noir sur
l'hôte NVIDIA). `SYS_GPU_PRESENT` (`gpu::present`) fait copier l'image par
le GPU dans une zone de relecture du contexte, gardée d'un appel à l'autre
et comptée au quota. Il recopie ensuite les lignes, par
`vm::copy_from_user`, dans la fenêtre `/dev/nova-gui` désignée par le
descripteur (`posix::gui::window_of_descriptor`), puis présente la
fenêtre comme le ferait un programme.

### Démarrage

`gpu::start` (appelé par `init.dsa` avec les pilotes) ne fait que lancer
`drv-gpu`. La tâche relaie l'interruption du GPU tout de suite, attend la
fin des autotests du démarrage, charge les modules de
`/sys/kernel/modules/gpu/`, puis ouvre une session sonde (`nova-sonde`)
pour décrire la carte, et publie le périphérique.

## 11 duodecies. Mises à jour atomiques des applications (phase 15)

### D'un manifeste à un processus

```
registre (file:, https://)          spm.dsa / spm update / updater.dsa
  {app_id}.json  --fetch-->  registry::decode  (Ed25519 de la clé du registre,
                                               app_id, canal choisi)
  paquet.spm     --fetch::to_file--> {racine}/{app}/telechargement-{v}.spm
                                     (repris par Range, empreinte et taille)
                 --manager::accept--> analyse, signature §3.2, §10.4, dépendances
                 --manager::lay_down-> versions/{v}.tmp/   (scripts confinés)
                 --fs::rename-------> versions/{v}/
                 --versions::switch-> current.nouveau puis fs::retarget(current)
                                      previous <- l'ancienne
lanceur / explorateur --apps::launch--> versions::launch_by_name
                 lit current --> versions/{v}/{binaire} --> loader ou posix
                 note_launch(pid, racine, v)
spm.dsa, toutes les 10 s --> versions::tick : supprime ce qui n'est ni actif,
                 ni précédent, ni exécuté (processus et descendants)
```

### Disposition d'une application versionnée

| Chemin | Rôle |
|---|---|
| `{racine}/{app}/versions/{v}/` | une version complète : binaire, ressources, `manifeste.cfg` |
| `{racine}/{app}/versions/{v}.tmp/` | une version en cours d'écriture (purgée si elle traîne) |
| `{racine}/{app}/current` | le pointeur : la version active, en texte |
| `{racine}/{app}/previous` | la version d'un `spm rollback` |
| `/sys/tools/spm/registres.cfg` | `nom|url|clé` par registre |
| `/sys/config/spm-canaux.cfg` | `defaut=release`, `{app}=beta` |
| `/sys/config/autorites.cfg` | autorités ENTERPRISE de confiance (`nom clé`) |

### Signatures (15.0.3)

| Objet | Format | Message signé |
|---|---|---|
| Binaire | certificat v2 (0x78 clé, 0x98 signature) | `NOVA-BIN\0` ‖ champs 0x00..0x98 ‖ empreinte |
| Paquet | en-tête v2 de 320 octets (0xC0 signature, 0x100 clé) | `NOVA-SPM\0` ‖ empreinte |
| Module `.kmd` | conteneur v2 (144 clé, 176 signature) | `NOVA-KMD\0` ‖ clé ‖ empreinte |
| Manifeste | membre `signature` | `NOVA-REG\0` ‖ JSON canonique sans `signature` |

## 12. Points d'extension immédiats

- `loader::RESIDENT` → `SYS_PROC_WAIT` (5.3.3) y trouvera l'image à libérer
  quand une tâche meurt ; `loader::unload` fait déjà le travail.
- `signature::SYSTEM_DIGESTS` et `ENTERPRISE_ROOTS` → tables vides à la
  compilation, alimentées dynamiquement par l'autotest. La construction de
  l'image système (phase 9.1.5) les remplira réellement.
- ~~`signature::expected_binding`~~ → remplacé en phase 15 par
  `signature::check_enterprise` (Ed25519).
- `identity::Permissions::ceiling()` → 10.3.4 s'en servira pour le sandbox
  à l'exécution, pas seulement à l'analyse.
- `fs::read_file` / `write_file` / `list` / `stat` → base directe du domaine
  `SYS_FS_*` complet (étape 5.2) ; il ne manque que la validation des
  pointeurs et la traduction des erreurs.
- `fs::perm::Credentials` → aujourd'hui fourni par l'appelant ; il devra
  être déduit du processus courant (étape 5.6.1).
- `tree::sandbox_roots` → la table des chemins autorisés par type de
  binaire (§12.4) est écrite ; il reste à la faire respecter à l'exécution
  (étape 10.3.4).
- `ipc::send` / `ipc::recv` → base directe de `SYS_IPC_SEND` / `SYS_IPC_RECV`
  en phase 5.5. Il manque `SYS_IPC_OPEN` pour qu'un programme ring 3 résolve un
  service par son nom plutôt que par un PID.
- `acpi::AcpiInfo::resolve_irq()` → toujours pas utilisé : la souris route
  l'IRQ 12 par son numéro brut, sans consulter les redirections MADT, qui
  sont pourtant déjà parsées. Sans conséquence sous QEMU, faux dès qu'un
  firmware remappe une ligne ISA.
- `drivers::irq` → il manque le masquage de ligne depuis le relais et sa
  réouverture depuis la tâche, seul moyen de servir une ligne PCI à
  niveau. C'est ce qui fera passer la carte réseau et l'audio de la
  scrutation à l'interruption.
- `net::tcp` → la fenêtre d'émission vaut un segment. Une fenêtre
  glissante demande une file de retransmission, une estimation du temps
  d'aller-retour et un contrôle de congestion.
- `net::dhcp` → `Lease::seconds` est enregistré et jamais utilisé : le
  renouvellement à la moitié du bail est le prochain pas.
- `gui::painter::Painter::rounded` → les coins arrondis du §8.4 sont
  crénelés. L'antialiasing suppose de relire le framebuffer pour mélanger
  avec ce qui est dessous, ce qu'un mapping write-combining rend lent : il
  viendra avec une couche de composition qui garde ses pixels en RAM.
- `gui::NATIVE_MODULES` → la table qui associe un `app_id` au code natif
  qui l'exécute. C'est elle qui disparaîtra quand un `.mod` portera du
  vrai code ring 3 ; ni le format ni le chargeur ne bougeront.
- `gui::protocol::OP_DRAW_ICON` → 16x16 monochrome, faute de mémoire
  partagée. Une vraie image demande un CR3 par processus.
- `drivers::mouse::set_surface` → résolu en 7.1 : la tâche pilote pose la
  résolution réelle du framebuffer dès qu'un mode graphique est actif. Le
  compositeur s'en servira de la même façon s'il change de mode.
- `audio::mix_into` → base directe du chemin de rendu du compositeur ;
  le rééchantillonnage manque, et avec lui la possibilité de jouer autre
  chose que du 48 kHz.
