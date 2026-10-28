# Journal des versions

Chaque version publiée a sa section `## [X.Y.Z]` : c'est elle que la
publication automatique reprend comme notes de la release GitHub
(`.github/workflows/release.yml`). Le détail, étape par étape, avec ce qui
a été observé, reste dans `ROADMAP.md` ; les décisions dans `DECISIONS.md`.

Schéma de version : `0.{phase}.{correctif}` — la phase est la dernière de
la roadmap dont le code est complet.

## [0.15.0] — 2026-10-28

Phase 15 : mises à jour atomiques des applications (proposition de
l'auteur du 24 septembre 2026), et les fondations qu'elle partage avec la
suite.

### Ajouté

- **Registre de manifestes** (spec §10.5) : un manifeste JSON par
  application, sur GitHub, un serveur ou un dossier ; canaux `release`,
  `beta`, `test` ; manifeste **signé Ed25519** par la clé du registre ;
  paquet téléchargé vérifié (taille, SHA-256) puis contrôlé comme tout
  paquet. `spm registry add`, `spm install <app> [--canal beta]`,
  `spm update <app>`, `spm rollback <app>`, `spm canal`.
- **Installation atomique** : chaque version dans
  `{racine}/{app}/versions/{v}/`, écrite en `.tmp`, scellée, puis rendue
  active par le pointeur `current` remplacé en une opération du système
  de fichiers. Une coupure laisse l'ancienne version ou la nouvelle.
- **Lancement par le pointeur** : le menu et l'explorateur lancent les
  applications installées par SPM (binaires Nova et images liées) ; un
  processus garde sa version après une mise à jour.
- **Nettoyage** : une version ni active, ni précédente, est supprimée à la
  fin de son dernier processus ; purge au démarrage.
- **`spm.dsa`** : seul écrivain des applications ; une application demande
  `verifier` ou `installer` sur le canal `spm.update`, pour elle-même
  seulement. `updater.dsa` vérifie aussi les applications (code 2 en
  silence, les autres notifiés).
- Onglet **« Mises a jour »** des paramètres : canal par défaut,
  vérification automatique, applications installées, registres.
- **Ed25519** (RFC 8032) ; **ENTERPRISE est enfin une vraie signature** :
  certificats de binaires v2, paquets v2, modules `.kmd` v2, autorités de
  confiance dans `/sys/config/autorites.cfg`, dates de validité contrôlées.
  `tools/novasign.py` (clés, signature des manifestes).
- **JSON** (`src/json.rs`), **téléchargement HTTPS en flux** (redirections,
  reprise par `Range`, sans limite de 4 Mio) ; les dépôts d'index de la
  phase 8 passent aussi en HTTPS.
- `fs::retarget` : remplacement atomique d'une entrée (LIFS et NovaFS).

### Corrigé

- Autotest de la phase 7 : la zone endommagée lue sous le verrou du commit
  (faux échec un démarrage sur dix).

### Limites connues

- Aucun registre réel éprouvé sous QEMU (cycle complet sur un registre
  `file:`, transfert HTTPS sur connexion scriptée).
- Canal par application : par commande seulement.

## [0.14.0] — 2026-10-28

Première version publiée : l'état du système au terme des phases 1 à 14.

### Contenu

- **Microkernel x86_64** : amorçage GRUB/Multiboot2, moitié haute, SMP
  jusqu'à 8 cœurs, LAPIC/x2APIC, I/O APIC, MSI-X, ordonnanceur préemptif à
  priorités fixes par type de binaire, IPC, espaces d'adressage par
  processus, copie sur écriture.
- **Formats binaires** `.dsa`, `.ss`, `.gs`, `.cus`, `.ras`, signatures
  SYSTEM/ENTERPRISE/SELF, images liées ELF, modules de pilote `.kmd`.
- **Stockage** : AHCI, GPT/MBR, **LIFS** (arbres B+, clones CoW, liens Tag,
  index inverse, journal), NovaFS, export FAT32/ext4/NTFS.
- **API système** complète (domaines 01 à 0C), couche POSIX, libc Nova,
  sous-système Linux.
- **Réseau** (virtio-net, e1000, TCP/IP, DHCP, DNS, TLS 1.3), **son**
  (AC'97), **USB** (xHCI, HID).
- **Interface graphique** : compositeur, modules `.mod`, applications
  (explorateur, bloc-notes, lecteur audio, navigateur, paramètres),
  accessibilité, internationalisation.
- **Pile graphique GPU** (virtio-gpu 3D, Venus) avec repli logiciel.
- **SPM** et mises à jour de l'OS avec retour arrière ; **SDK** `dev`
  (assembleur, sous-ensemble de Rust, Nova Script, ETR).

### Ajouté dans cette version

- Version unique dans `Cargo.toml`, imprimée au démarrage avec le commit
  (`src/version.rs`), donnée à `uname` et à SPM.
- `make test`, `make test-smp1`, `make test-novga` : démarrage sans fenêtre
  jugé automatiquement (`tools/boot-test.py`), deux démarrages sur le même
  disque.
- `make dist` : ISO, noyau sans informations de débogage (92 → 8 Mio),
  disque système vierge compressé, empreintes.
- Intégration continue et publication automatique des releases
  (`.github/workflows/`).
- Toolchain nightly épinglée (`rust-toolchain.toml`).
- `README.md`, `CHANGELOG.md`.

### Corrigé

- Deux autotests qui mesuraient des compteurs globaux (blocs libres,
  cadres libres) pendant que d'autres autotests écrivaient : faux échecs
  au second démarrage d'un disque neuf et sous émulation. Mesure isolée de
  la croissance des arbres LIFS, cycle rejoué quand la machine a bougé
  (`frame::neutral_cycle`).

### Limites connues

- Phases 12, 13 et 14 implémentées mais non cochées : validation de
  l'auteur attendue.
- Phase 9 : six cases ouvertes (débogueur, chaînes C/C++, C#, Java,
  Python, Node/Bun).
- ENTERPRISE n'est pas encore une signature asymétrique.
