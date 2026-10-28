# Nova OS — Spécification Technique

**Document de référence complet — Version 1.0**

Architecture : x86_64 • Langage : Rust • Bootloader : GRUB

> Ce document est la **source de vérité** du projet. Toute implémentation qui le
> contredit est un bug, quelle que soit son élégance. En cas d'ambiguïté, ne pas
> trancher seul : demander.

---

## 1. Introduction & Architecture Générale

Nova OS est un système d'exploitation écrit entièrement en Rust, ciblant
l'architecture x86_64, utilisant GRUB comme bootloader. Il repose sur une
architecture microkernel dans sa première version — le kernel gère le strict
minimum, le reste passe par la couche logicielle et utilisateur.

### 1.1 Architecture globale

```
┌─────────────────────────────────────────────────────┐
│           Applications utilisateur                  │
│   .dsa  .ss  .gs  .cus  .ras                        │
├─────────────────────────────────────────────────────┤
│   Sous-systèmes (Linux / Windows)   ← futur         │
├─────────────────────────────────────────────────────┤
│   GUI / Compositeur (.ss + modules .mod)            │
│   Shell / CLI                                       │
│   Drivers (user space)                              │
│   Système de fichiers                               │
│   Réseau / Audio / IPC                              │
├─────────────────────────────────────────────────────┤
│   API Système maison (syscalls)                     │
╠═════════════════════════════════════════════════════╣
│   MICROKERNEL                                       │
│   Scheduler / IPC / Mémoire / IRQ                   │
├─────────────────────────────────────────────────────┤
│   GRUB → Bootloader (Multiboot2)                    │
└─────────────────────────────────────────────────────┘
```

### 1.2 Principes fondamentaux

- Microkernel : le kernel ne gère que scheduler, IPC, mémoire et IRQ
- Tout le reste (drivers, FS, réseau, GUI) tourne en user space
- **Exception (24 septembre 2026)** : le pilote GPU est intégré au
  noyau par défaut, et les pilotes GPU de fabricants sont des modules noyau
  signés chargés au démarrage (§3.6, §8.5). Raison : la soumission de
  commandes au GPU est sur le chemin de chaque image ; un aller-retour IPC
  par soumission coûterait la fluidité que la pile existe pour donner
- Rust natif — sécurité mémoire garantie par le compilateur
- Évolution future : migration vers architecture monolithique hybride
- Sous-systèmes Linux et Windows prévus comme couche de compatibilité

---

## 2. Séquence de Boot

### 2.1 Phases de démarrage

| Phase | Composant | Description |
|---|---|---|
| 1 | BIOS/UEFI | Initialisation matérielle, détection des disques |
| 2 | GRUB | Chargement kernel via Multiboot2, passage infos hardware |
| 3 | Kernel init | GDT, IDT, paging, allocateur, APIC, scheduler |
| 4 | init.dsa | Premier processus, orchestre tous les services |
| 5 | Splash screen | Affichage visuel modulaire (.mod) |
| 6 | Services | Lancement core puis secondaires |
| 7 | Écran de connexion | login.ss — sélection utilisateur |
| 8 | Bureau | Chargement profil + restauration session |

### 2.2 Menu GRUB

- **Nova OS** — démarrage normal
- **Nova OS — Mode recovery** (kernel + fs.dsa + shell CLI uniquement)
- **Nova OS — Mode debug**
- **Nova OS — Version précédente**

### 2.3 Splash screen modulaire

Le splash screen et la barre de progression sont des modules `.mod` remplaçables.
Stockés dans `/sys/gui/modules/boot/`, ils peuvent être overridés par utilisateur
dans `/users/{user}/settings/gui/modules/boot/`.

### 2.4 Écran de connexion

Affichage de tous les comptes utilisateurs. Sélection puis saisie du mot de passe.
Bouton Accessibilité toujours visible.

**Anti-bruteforce** : 3 échecs → 30 s, 5 échecs → 5 min, 10 échecs → compte verrouillé.

### 2.5 Codes d'erreur boot

| Code | Description |
|---|---|
| `BOOT_ERR_0x0001` | Kernel panic — mémoire insuffisante |
| `BOOT_ERR_0x0010` | init.dsa introuvable ou corrompu |
| `BOOT_ERR_0x0020` | fs.dsa crash |
| `BOOT_ERR_0x0021` | ipc.dsa crash |
| `BOOT_ERR_0x0022` | hw.dsa crash |
| `BOOT_ERR_0x0030` | Compositeur crash avant splash |
| `BOOT_ERR_0x0040` | Timeout service core (> 30 s) |

---

## 3. Formats de Binaires

### 3.1 Hiérarchie des types

| Extension | Nom | Priorité | Description |
|---|---|---|---|
| `.dsa` | Direct System Access | 1 | Apps système officielles, communiquent avec l'API, peuvent servir d'API |
| `.ss` | System Software | 2 | Apps dédiées au système (explorateur, gestionnaire de tâches) |
| `.gs` | Game Software | 3 | Jeux — mode pause automatique en arrière-plan |
| `.cus` | Common Utility Software | 4 | Toutes autres apps (navigateur, logiciels...) |
| `.ras` | Random Access Software | 5 | Accès total, non recommandé — test uniquement |

> **Les priorités sont fixes et non ajustables par l'utilisateur.** C'est une
> décision de conception arrêtée, pas un réglage.

### 3.2 Système de signatures — 3 niveaux

| Type | Utilisateurs | Vérification |
|---|---|---|
| SYSTEM | Apps officielles uniquement | Hash hardcodé vérifié par le kernel |
| ENTERPRISE | Entreprises / studios | Certificat tiers avec infos légales |
| SELF | Devs indépendants | Auto-signé — avertissement à l'installation |

### 3.3 Magic numbers

```
DSA : 0x44 0x53 0x41 0x00   "DSA\0"
SS  : 0x53 0x53 0x00 0x00   "SS\0\0"
GS  : 0x47 0x53 0x00 0x00   "GS\0\0"
CUS : 0x43 0x55 0x53 0x00   "CUS\0"
RAS : 0x52 0x41 0x53 0x00   "RAS\0"
```

### 3.4 Règles par type

#### `.dsa` — Direct System Access
- Signature SYSTEM obligatoire (refus ENTERPRISE et SELF)
- Peut exposer une API publique via IPC
- Accès total kernel space déclaré

#### `.ss` — System Software
- Signature SYSTEM ou ENTERPRISE uniquement
- Peut remplacer un composant OS

#### `.gs` — Game Software
- Signatures ENTERPRISE ou SELF
- Mode arrière-plan : CPU réduit à 2 %, connexions réseau maintenues, état sauvegardé
- Retour au premier plan instantané sans interruption

#### `.ras` — Random Access Software
- Aucune signature requise
- Avertissement rouge obligatoire à l'installation **et** au lancement
- Ne peut pas être défini comme app par défaut

### 3.5 Image liée (extension du 23 septembre 2026)

Un binaire peut porter, comme charge utile, une **image ELF64 x86-64** plutôt
qu'une image Nova : drapeau `IMAGE_LIEE` de l'en-tête, version d'ABI 2. C'est
la forme des programmes portés (§6.7).

- L'en-tête, la signature et l'empreinte couvrent l'image comme pour tout
  binaire ; type, priorité et permissions restent ceux de l'en-tête.
- Le chargeur natif projette l'image. Si elle demande un interpréteur, c'est
  l'éditeur de liens de la libc Nova (`/sys/runtimes/c/`), qui charge les
  bibliothèques partagées du programme par `SYS_OBJ_OPEN` et `SYS_MEM_MAP`.
- Les bibliothèques partagées d'une application vivent dans son dossier
  (`/apps/{app}/`) ; elles ne sont pas des binaires au sens de ce chapitre et
  sont couvertes par la signature du paquet (§10).

### 3.6 Module de pilote signé (extension du 24 septembre 2026)

Un pilote de périphérique fourni par un tiers (d'abord : un backend GPU d'un
fabricant, §8.5) est un **module noyau** : extension `.kmd`, magic
`KMD\0` (`0x4B 0x4D 0x44 0x00`), rangé dans `/sys/kernel/modules/{classe}/`
(`gpu/` pour les pilotes graphiques).

- **Chargé au démarrage**, avant que le cœur GPU ne choisisse son
  périphérique, jamais après : pas de chargement à chaud.
- **La signature est vérifiée avant la moindre instruction** (§3.2) :
  SYSTEM ou ENTERPRISE acceptées ; SELF refusée par défaut, acceptée
  seulement si un administrateur a inscrit l'empreinte de ce module précis
  dans `/sys/config/modules-autorises` (une empreinte SHA-256 en hexadécimal
  par ligne), avec un avertissement à chaque démarrage. Un module non
  signé, altéré ou refusé est journalisé (`/logs/kernel/modules.txt`) et le
  système démarre sans lui.
- Il s'exécute en ring 0 : **la signature est sa seule barrière**. Il ne
  voit du noyau que l'interface de sa classe, une table de fonctions
  versionnée (`tools/kmd/nova_kmd.h`) : aucun symbole du noyau n'est résolu
  en lui. Un module d'une autre version d'interface est refusé.
- Un module ne remplace jamais un pilote intégré : il s'ajoute (une carte
  que le pilote intégré ne connaît pas). Le cœur GPU ne pilote qu'un
  périphérique à la fois : un module n'est utilisé que si le pilote intégré
  n'a pas trouvé le sien.

**Conteneur** (lu champ par champ, petit-boutiste) :

| Octets | Champ |
|---|---|
| 0-3 | `KMD\0` |
| 4-5 | version du conteneur (1) |
| 6-7 | classe (1 : GPU) |
| 8-11 | version de l'interface de backend visée (`0x00010000`) |
| 12-15 | niveau de signature : 1 SELF, 2 ENTERPRISE, 3 SYSTEM |
| 16-31 | décalage et taille de la charge utile (u64, u64) |
| 32-63 | SHA-256 de la charge utile |
| 64-111 | nom (ASCII) |
| 112-143 | éditeur (ASCII) |
| 144-175 | ENTERPRISE : clé de l'autorité |
| 176-207 | ENTERPRISE : liaison `SHA-256(clé ‖ empreinte)` |
| 208-255 | réservé ; la charge utile suit |

L'**empreinte d'authenticité** — celle que SYSTEM cherche dans la table du
noyau et qu'ENTERPRISE lie à son autorité — est le SHA-256 des octets 0 à
143 de l'en-tête suivis de la charge utile. Même réserve qu'au §3.2 : pas
encore de signature asymétrique.

**Charge utile** : un objet partagé ELF 64 bits x86-64, indépendant de la
position, sans bibliothèque (`DT_NEEDED` refusé), dont les seules
relocations sont `R_X86_64_RELATIVE` ; aucune page à la fois modifiable et
exécutable. Son point d'entrée est `nova_kmd_init(const struct
nova_kmd_kernel *)`, qui rend la table du pilote ou NULL. Compilé sans SSE
ni zone rouge (`-mgeneral-regs-only -mno-red-zone`).

---

## 4. Système de Fichiers

### 4.1 Concept fondamental — Liens virtuels

Les dossiers ne sont pas réels mais des **tables de liens** entre nœuds de
fichiers. Déplacer un fichier = modifier son lien dans la node table →
instantané, aucune copie de données. La copie réelle n'a lieu qu'entre disques
différents.

### 4.2 Gestion des disques

```
/           ← volume système (la partition LIFS désignée comme racine)
A:/         ← première autre partition montée
B:/         ← deuxième
C:/         ← etc.
```

**Une lettre par partition montée** (décision du 24 septembre 2026), dans
l'ordre de détection : disques dans l'ordre des ports, partitions dans
l'ordre de leur table. Un disque sans table de partitions portant un volume
NovaFS entier (ancien format) compte pour une partition.

### 4.3 Arborescence complète

| Chemin | Description |
|---|---|
| `/sysapp/` | Applications système (.dsa, .ss) |
| `/sys/` | Fichiers noyau, drivers, config kernel |
| `/sys/kernel/` | Binaires kernel et modules |
| `/sys/drivers/` | Drivers user space |
| `/sys/config/` | Configuration système globale |
| `/games/` | Fichiers des jeux (.gs) |
| `/apps/` | Applications communes (.cus) |
| `/ras/` | Applications Random Access (.ras) |
| `/libs/` | Bibliothèques partagées (.xlib) |
| `/cache/` | Cache temporaire global système |
| `/logs/` | Logs système (kernel/, drivers/, apps/) |
| `/net/` | Ressources réseau et configs |
| `/users/_shared/` | Fichiers partagés entre utilisateurs |
| `/users/{user}/apps/` | Apps installées pour cet utilisateur |
| `/users/{user}/settings/` | Config et préférences utilisateur |
| `/users/{user}/cache/` | Cache temporaire utilisateur |
| `/users/{user}/logs/` | Logs des apps de l'utilisateur |
| `/users/{user}/home/desktop/` | Bureau |
| `/users/{user}/home/download/` | Téléchargements |
| `/users/{user}/home/archive/` | Archives |
| `/users/{user}/home/images/` | Images |
| `/users/{user}/home/videos/` | Vidéos |
| `/users/{user}/home/documents/` | Documents |
| `/users/{user}/home/executables/` | Exécutables utilisateur |

### 4.4 Multi-compatibilité

Option activable à l'installation. Lors d'un transfert vers un support externe,
l'utilisateur choisit le format de destination : format natif (liens virtuels),
FAT32 (compatibilité maximale), ext4 (Linux) ou NTFS (Windows).

À l'export, un lien Tag devient un lien dur (ext4, NTFS) ou une copie
(FAT32) ; un clone devient une copie.

### 4.5 Partitions et LIFS (extension du 24 septembre 2026)

**Partitions.** Nova lit les tables **GPT** (d'abord) et **MBR** (disques
anciens ; partitions primaires seulement). Une partition est un
périphérique de blocs à part — une fenêtre sur le disque, bornes vérifiées
à chaque accès — et doit commencer sur une frontière de 4 Kio. Nova
**ne formate jamais un disque ni une partition qu'il ne reconnaît pas** :
ce qui n'est ni LIFS ni NovaFS est signalé et laissé intact ; formater est
toujours un acte explicite (outil de l'hôte, ou appel dédié).

| Type | GPT (GUID de type) | MBR (octet) |
|---|---|---|
| LIFS | `BD095063-2621-48A5-89A3-0310280C1AF0` | `0x4C` |
| NovaFS (volume historique) | `A395A30A-8F34-4F37-B123-AE9B7E682E3A` | — |

Une partition est reconnue par son type **puis** par la signature de son
superbloc ; le type seul ne suffit pas. Le **volume racine** est la
partition dont le GUID unique est donné par `racine=<GUID>` sur la ligne de
commande du noyau ; à défaut, la première partition LIFS marquée
« système » dans son superbloc ; à défaut, la première partition LIFS
trouvée. Sans aucune, le premier volume NovaFS (disques d'avant la phase
14), et sans rien du tout, un volume en mémoire.

**LIFS** (Linked Index File System, proposition de l'utilisateur,
`propositions/lifs.md`) est le **format natif** de Nova, **y compris pour
la racine** (décision du 24 septembre 2026) ; NovaFS reste lu et écrit pour
les volumes existants. Ses principes :

- **Topologie plate.** Un nœud porte un identifiant de 64 bits jamais
  réutilisé ; un dossier est une **micro-base** — un arbre B+ d'entrées
  (nom → nœud), recherche par empreinte du nom puis comparaison du nom.
  Déplacer ou renommer dans un volume modifie deux entrées : O(1) quelle
  que soit la taille. Entre deux volumes, les octets sont copiés.
- **Deux sortes de liens.**
  - Le **lien Tag** est une entrée de dossier vers un nœud ; plusieurs Tag
    vers le même nœud sont le même fichier (lien dur) : une modification se
    voit partout. Le nœud disparaît avec son dernier Tag.
  - Le **clone CoW** (copie) crée un **nouveau nœud** qui partage les
    étendues de données de l'original, en temps indépendant de la taille.
    Toute écriture dans une étendue partagée — par la copie **ou** par
    l'original — n'en copie que la partie touchée. Les métadonnées
    (propriétaire, droits, dates) sont à chaque nœud : un `chmod` de la
    copie ne touche pas l'original.
- **Étendues et comptes de références.** Un fichier est une liste
  d'étendues (début, longueur) ; une étendue partagée par plusieurs nœuds
  porte un compte de références. Taille d'un fichier : 2^63 octets en
  principe, la partition en pratique.
- **Index inverse.** Pour chaque nœud, les dossiers qui le désignent :
  c'est ce qui répond à « où est étiqueté ce fichier ».
- **Cohérence par journal** (décision du 24 septembre 2026) : toute
  modification des métadonnées est d'abord écrite dans le journal, validée,
  puis recopiée à sa place ; au montage, une transaction validée et non
  recopiée est rejouée, une transaction non validée ignorée. Les données
  d'un fichier sont écrites avant la validation qui les désigne ; un bloc
  libéré ne resert qu'après la validation qui le libère. Une coupure perd
  au plus les dernières secondes, jamais la cohérence.
- **Permissions** : les treize du §5.1, **trois masques** (propriétaire,
  groupe, autres), comme NovaFS (décision du 24 septembre 2026).

Le format sur disque (superbloc, nœud, entrée, arbres, journal) est décrit
dans `ARCHITECTURE.md` et fixé par `src/fs/lifs/format.rs` ; l'outil de
l'hôte `tools/lifs.py` l'écrit et le lit de même (création de la table,
formatage, import, vérification).

---

## 5. Permissions

### 5.1 Table des permissions

| Permission | Fichier | Dossier | Description |
|---|---|---|---|
| read | ✓ | — | Lire le contenu d'un fichier |
| write | ✓ | ✓ | Créer un nouveau fichier ou dossier |
| execute | ✓ | ✓ | Exécuter — les deux niveaux requis (fichier ET dossier) |
| overwrite | ✓ | — | Remplacer un fichier existant (≠ sauvegarder une mise à jour) |
| delete | ✓ | ✓ | Supprimer |
| rename | ✓ | ✓ | Renommer |
| access | — | ✓ | Voir le contenu d'un dossier |
| link | ✓ | ✓ | Créer un lien virtuel vers ce nœud |
| move | ✓ | ✓ | Déplacer — requiert aussi write sur dossier destination |
| copy | ✓ | ✓ | Copier — requiert aussi write sur dossier destination |
| chmod | ✓ | ✓ | Modifier les permissions |
| own | ✓ | ✓ | Transférer la propriété |
| audit | ✓ | ✓ | Consulter les logs d'accès — ADMIN uniquement |

### 5.2 Rôles utilisateurs

| Rôle | Accès | Description |
|---|---|---|
| ROOT | Total | Compte système interne, jamais utilisé directement |
| ADMIN | Quasi-total | Installation d'apps, modification système |
| USER | Standard | Accès à son `/users/{user}/` et aux apps communes |
| GUEST | Restreint | Session temporaire, effacée à la déconnexion |
| SERVICE | Ciblé | Rôle interne pour les processus système (.dsa) |

---

## 6. API Système

### 6.1 Conventions générales

Chaque syscall possède un numéro unique et un nom symbolique. Convention :
`SYS_{DOMAINE}_{ACTION}`. Tout syscall retourne un `SysResult<T>` contenant
`success`, `data` et un `SysError` structuré (code, name, message, context).

### 6.2 Domaine 01 — Système de fichiers `SYS_FS_*`

| N° | Nom | Mode | Description |
|---|---|---|---|
| 0x0101 | SYS_FS_READ | Async | Lire le contenu d'un fichier |
| 0x0102 | SYS_FS_WRITE | Async | Créer et écrire un nouveau fichier |
| 0x0103 | SYS_FS_OVERWRITE | Async | Remplacer un fichier existant |
| 0x0104 | SYS_FS_DELETE | Sync | Supprimer un fichier |
| 0x0105 | SYS_FS_RENAME | Sync | Renommer un fichier |
| 0x0106 | SYS_FS_MOVE | Sync | Déplacer via modification de lien |
| 0x0107 | SYS_FS_COPY | Async | Copier un fichier |
| 0x0108 | SYS_FS_LINK | Sync | Créer un lien virtuel |
| 0x0109 | SYS_FS_STAT | Sync | Lire les métadonnées |
| 0x010A | SYS_FS_CHMOD | Sync | Modifier les permissions |
| 0x010B | SYS_FS_CHOWN | Sync | Transférer la propriété |
| 0x010C | SYS_FS_AUDIT | Sync | Consulter les logs d'accès |
| 0x0110 | SYS_FS_DIR_ACCESS | Sync | Lister le contenu d'un dossier |
| 0x0111 | SYS_FS_DIR_CREATE | Sync | Créer un nœud dossier |
| 0x0120 | SYS_FS_DISK_LIST | Sync | Lister les disques disponibles |
| 0x0121 | SYS_FS_DISK_MOUNT | Sync | Monter un disque |
| 0x0124 | SYS_FS_DISK_EXPORT | Async | Export multi-compatibilité |
| 0x0130 | SYS_FS_NODE_STAT | Sync | Métadonnées d'un nœud par identifiant (LIFS) |
| 0x0131 | SYS_FS_NODE_MOVE | Sync | Déplacer une entrée d'un dossier à un autre, par identifiants |
| 0x0132 | SYS_FS_NODE_CLONE | Sync | Clone CoW d'un nœud dans un dossier |
| 0x0133 | SYS_FS_NODE_TAG | Sync | Lien Tag d'un nœud dans un dossier |
| 0x0134 | SYS_FS_NODE_UNLINK | Sync | Retirer une entrée ; le nœud part avec son dernier Tag |
| 0x0135 | SYS_FS_NODE_TAGS | Sync | Dossiers qui désignent un nœud (index inverse) |

Les appels par identifiant (LIFS, §4.5) nomment un volume par sa lettre et
un nœud par son identifiant ; ils vérifient les mêmes droits que par chemin
— `access` sur le dossier source, `write` sur la destination, le droit du
nœud lui-même (`move`, `copy`, `link`, `delete`) — et `SYS_FS_NODE_TAGS` ne
rend que les dossiers que l'appelant peut lister : un identifiant n'est pas
un passe-partout. Sur un volume LIFS, `SYS_FS_COPY` dans un même volume est
un clone CoW, et `SYS_FS_LINK` un lien Tag.

### 6.3 Domaine 02 — Processus `SYS_PROC_*`

| N° | Nom | Mode | Description |
|---|---|---|---|
| 0x0201 | SYS_PROC_SPAWN | Async | Lancer un nouveau processus |
| 0x0202 | SYS_PROC_KILL | Sync | Terminer un processus |
| 0x0203 | SYS_PROC_SUSPEND | Sync | Suspendre un processus |
| 0x0204 | SYS_PROC_RESUME | Sync | Reprendre un processus suspendu |
| 0x0205 | SYS_PROC_STAT | Sync | Infos d'un processus (CPU, RAM) |
| 0x0206 | SYS_PROC_LIST | Sync | Lister tous les processus actifs |
| 0x0207 | SYS_PROC_SETPRIO | Sync | Modifier la priorité |
| 0x0209 | SYS_PROC_WAIT | Async | Attendre la fin d'un processus |
| 0x020A | SYS_PROC_SELF | Sync | Infos du processus appelant |

### 6.4 Domaine 03 — Mémoire `SYS_MEM_*`

| N° | Nom | Mode | Description |
|---|---|---|---|
| 0x0301 | SYS_MEM_ALLOC | Sync | Allouer un bloc mémoire |
| 0x0302 | SYS_MEM_FREE | Sync | Libérer un bloc mémoire |
| 0x0303 | SYS_MEM_RESIZE | Sync | Redimensionner une allocation |
| 0x0304 | SYS_MEM_SHARE | Sync | Partager une zone via IPC |
| 0x0305 | SYS_MEM_PROTECT | Sync | Définir les droits d'une zone (R/W/X) |
| 0x0307 | SYS_MEM_LOCK | Sync | Empêcher le swap d'une zone |

### 6.5 Domaine 04 — IPC `SYS_IPC_*`

| N° | Nom | Mode | Description |
|---|---|---|---|
| 0x0401 | SYS_IPC_SEND | Async | Envoyer un message |
| 0x0402 | SYS_IPC_RECV | Async | Recevoir un message |
| 0x0403 | SYS_IPC_OPEN | Sync | Ouvrir un canal IPC nommé |
| 0x0404 | SYS_IPC_CLOSE | Sync | Fermer un canal IPC |
| 0x0405 | SYS_IPC_SUBSCRIBE | Sync | S'abonner à un événement système |
| 0x0406 | SYS_IPC_PUBLISH | Async | Publier un événement |
| 0x0407 | SYS_IPC_QUERY | Sync | Demander une API exposée par un .dsa |
| 0x0408 | SYS_IPC_RESPOND | Async | Répondre à une requête IPC |

### 6.6 Domaines supplémentaires

| Domaine | Préfixe | Accès |
|---|---|---|
| Utilisateurs & Permissions | `SYS_USER_*` (0x05xx) | DSA, SS uniquement pour les ops admin |
| Matériel | `SYS_HW_*` (0x06xx) | DSA, SS — et RAS (accès total) |
| Temps | `SYS_TIME_*` (0x07xx) | Tous les types |
| Réseau | `SYS_NET_*` (0x08xx) | Tous selon déclaration header |

### 6.7 Programmes portés — libc Nova (extension du 23 septembre 2026)

Un programme écrit pour POSIX (C, C++, Rust de la cible `x86_64-linux-musl`)
devient un binaire Nova natif en étant lié à la **libc Nova** : musl, dont
les fonctions appellent l'API Nova au lieu du noyau Linux. Le programme garde
l'**ABI utilisateur de musl** (types, constantes, numéros d'erreur) : son code
source et ses bibliothèques se compilent sans modification. La frontière avec
le noyau est l'API Nova : **aucun appel Linux** ne franchit l'instruction
`syscall` d'un programme porté. La libc traduit les rares appels bruts
`syscall(SYS_...)` d'un programme en appels Nova. Premier programme porté :
le navigateur (fork de Firefox).

**Convention d'appel des domaines 09 à 0B et des extensions** : numéro dans
RAX, arguments dans RDI, RSI, RDX, R10, R8, R9 (R10 et non RCX, que
l'instruction `syscall` écrase). Un appel groupé (`SYS_OBJ_STAT`,
`SYS_PROC_IDENTITY`...) prend son opération en premier argument. Le
résultat revient dans RAX ; une erreur revient **négative, en numérotation
`errno` de musl** (`-ENOENT`), et non en code `SysError` (§6.1) : la libc
la pose telle quelle dans `errno` sans table de traduction, et un appel
brut de programme (Rust, par exemple) reçoit ce qu'il attend. Les
structures échangées (stat, timespec, sockaddr, sigaction, msghdr) sont
celles de musl x86_64.

Les domaines qui suivent donnent à ces programmes ce que les domaines 01 à 08
n'offrent pas : des **descripteurs** (objets ouverts, lus et écrits par
morceaux), des threads, la projection en mémoire, l'attente sur plusieurs
objets, des sockets au sens BSD et des signaux. Les appels déjà présents
restent inchangés.

#### Domaine 09 — Objets ouverts `SYS_OBJ_*`

Un objet ouvert est désigné par un **descripteur**, entier propre au
processus : fichier, dossier, tube, compteur d'événements, mémoire
partageable, socket, périphérique (`/sys/dev/fenetre`, `/sys/dev/son`).

| N° | Nom | Mode | Description |
|---|---|---|---|
| 0x0901 | SYS_OBJ_OPEN | Sync | Ouvrir un chemin (relatif à un dossier ouvert ou absolu) → descripteur |
| 0x0902 | SYS_OBJ_CLOSE | Sync | Fermer un descripteur |
| 0x0903 | SYS_OBJ_READ | Async | Lire (à la position courante ou à une position donnée, une ou plusieurs zones) |
| 0x0904 | SYS_OBJ_WRITE | Async | Écrire (idem) |
| 0x0905 | SYS_OBJ_SEEK | Sync | Déplacer la position courante |
| 0x0906 | SYS_OBJ_STAT | Sync | Métadonnées d'un descripteur, ou d'un chemin relatif à un dossier ouvert |
| 0x0907 | SYS_OBJ_DUP | Sync | Dupliquer un descripteur (au premier numéro libre ou à un numéro donné) |
| 0x0908 | SYS_OBJ_CONTROL | Sync | Drapeaux (non bloquant, fermeture à l'exécution) et commandes propres à l'objet |
| 0x0909 | SYS_OBJ_DIR_READ | Sync | Lire les entrées d'un dossier ouvert |
| 0x090A | SYS_OBJ_NAME_OP | Sync | Créer un dossier, un lien, un lien symbolique ; renommer, supprimer (relatif à un dossier ouvert) |
| 0x090B | SYS_OBJ_RESIZE | Sync | Fixer la taille d'un fichier ou d'une mémoire partageable |
| 0x090C | SYS_OBJ_SYNC | Async | Écrire sur le disque ce qui est en cache pour cet objet |
| 0x090D | SYS_OBJ_PIPE | Sync | Créer un tube → deux descripteurs |
| 0x090E | SYS_OBJ_EVENT | Sync | Créer un compteur d'événements |
| 0x090F | SYS_OBJ_MEMORY | Sync | Créer une mémoire anonyme partageable (projetable, transmissible) |
| 0x0910 | SYS_OBJ_WAIT | Async | Attendre qu'un des objets d'une liste soit prêt, avec échéance en nanosecondes |
| 0x0911 | SYS_OBJ_WATCH_CREATE | Sync | Créer un ensemble d'attente persistant |
| 0x0912 | SYS_OBJ_WATCH_CTL | Sync | Ajouter, modifier, retirer un objet de l'ensemble |
| 0x0913 | SYS_OBJ_WATCH_WAIT | Async | Attendre les objets prêts de l'ensemble |
| 0x0914 | SYS_OBJ_COPY | Async | Copier d'un descripteur vers un autre sans passer par le programme |

Les permissions (§5) sont vérifiées à l'ouverture ; les droits de l'objet
suivent le descripteur.

#### Domaine 0A — Threads et synchronisation `SYS_THREAD_*`

| N° | Nom | Mode | Description |
|---|---|---|---|
| 0x0A01 | SYS_THREAD_CREATE | Sync | Créer un thread du processus : drapeaux, pile, adresse où écrire son identifiant, adresse à effacer et réveiller à sa fin, base TLS. Revient deux fois, 0 dans le nouveau thread (la libc y appelle l'entrée) |
| 0x0A02 | SYS_THREAD_EXIT | Sync | Terminer le thread appelant |
| 0x0A03 | SYS_THREAD_SELF | Sync | Identifiant du thread appelant |
| 0x0A04 | SYS_THREAD_SET_TLS | Sync | Base TLS du thread appelant |
| 0x0A05 | SYS_THREAD_WAIT_ADDR | Async | Dormir tant qu'un mot vaut une valeur donnée, échéance en nanosecondes |
| 0x0A06 | SYS_THREAD_WAKE_ADDR | Sync | Réveiller jusqu'à N threads endormis sur un mot (ou les déplacer vers un autre) |
| 0x0A07 | SYS_THREAD_SET_NAME | Sync | Nommer le thread (diagnostic, profil) |
| 0x0A08 | SYS_THREAD_SLEEP_NS | Async | Dormir, en nanosecondes |
| 0x0A09 | SYS_THREAD_CLEAR_ADDR | Sync | Adresse à effacer et réveiller à la fin du thread appelant |
| 0x0A0A | SYS_THREAD_ROBUST | Sync | Liste des verrous à libérer si le thread meurt en les tenant |

Un thread est une tâche de l'ordonnanceur (§7) qui partage l'espace
d'adressage de son processus : même type de binaire, même priorité.

#### Extensions des domaines existants

| N° | Nom | Mode | Description |
|---|---|---|---|
| 0x0004 | SYS_RANDOM | Sync | Octets aléatoires du générateur du noyau |
| 0x0210 | SYS_PROC_FORK | Sync | Dupliquer le processus appelant (copie sur écriture) ; avec le drapeau de partage, l'enfant emprunte la mémoire du parent, suspendu, jusqu'à son `exec` (`vfork`, `posix_spawn`) |
| 0x0211 | SYS_PROC_EXEC | Sync | Remplacer l'image du processus (arguments, environnement, descripteurs gardés) |
| 0x0212 | SYS_PROC_WAIT_ANY | Async | Attendre la fin d'un enfant, avec son statut |
| 0x0213 | SYS_PROC_CWD | Sync | Lire ou changer le dossier courant |
| 0x0214 | SYS_PROC_LIMITS | Sync | Limites et consommation du processus |
| 0x0215 | SYS_PROC_EXIT_ALL | Sync | Terminer le processus, tous ses threads |
| 0x0216 | SYS_PROC_IDENTITY | Sync | Identifiants (processus, parent, groupe, session, utilisateur), masque de création, nom du système |
| 0x0217 | SYS_PROC_SCHED | Sync | Affinité, cœur courant, politique et priorité d'ordonnancement |
| 0x0218 | SYS_PROC_CONTROL | Sync | Réglages du processus (nom, signal à la mort du parent, droits) |
| 0x0310 | SYS_MEM_MAP | Sync | Projeter : anonyme ou objet ouvert, privé ou partagé, adresse souhaitée ou imposée, droits |
| 0x0311 | SYS_MEM_UNMAP | Sync | Retirer une projection |
| 0x0312 | SYS_MEM_REMAP | Sync | Agrandir, réduire ou déplacer une projection |
| 0x0313 | SYS_MEM_ADVISE | Sync | Conseil d'usage (pages à rendre, à précharger) |
| 0x0314 | SYS_MEM_BARRIER | Sync | Barrière mémoire sur les cœurs qui exécutent le processus |
| 0x0315 | SYS_MEM_PROTECT_MAP | Sync | Changer les droits d'une plage projetée |
| 0x0316 | SYS_MEM_BREAK | Sync | Fin du tas historique du processus (`brk`) |
| 0x0708 | SYS_TIME_CLOCK_NS | Sync | Horloge monotone, réelle, du processus ou du thread, en nanosecondes |
| 0x0709 | SYS_TIME_PAGE | Sync | Projeter la page de temps en lecture seule : l'heure se lit sans appel système (réservé, non implémenté : la libc passe par 0x0708) |
| 0x0810 | SYS_NET_SOCKET | Sync | Créer une socket (IPv4 ou locale ; flux, datagrammes ou paquets) → descripteur |
| 0x0811 | SYS_NET_PAIR | Sync | Créer une paire de sockets locales connectées |
| 0x0812 | SYS_NET_BIND | Sync | Lier une socket à une adresse |
| 0x0813 | SYS_NET_LISTEN | Sync | Accepter des connexions entrantes |
| 0x0814 | SYS_NET_ACCEPT | Async | Prendre une connexion entrante → descripteur |
| 0x0815 | SYS_NET_CONNECT_TO | Async | Connecter une socket (bloquante ou non) |
| 0x0816 | SYS_NET_SEND_MSG | Async | Envoyer : données, destinataire, descripteurs transmis |
| 0x0817 | SYS_NET_RECV_MSG | Async | Recevoir : données, expéditeur, descripteurs transmis |
| 0x0818 | SYS_NET_OPTION | Sync | Lire ou fixer une option de socket |
| 0x0819 | SYS_NET_SHUTDOWN | Sync | Fermer un sens d'une connexion |
| 0x081A | SYS_NET_ADDRESS | Sync | Adresse locale ou distante d'une socket |

#### Domaine 0B — Signaux `SYS_SIG_*`

| N° | Nom | Mode | Description |
|---|---|---|---|
| 0x0B01 | SYS_SIG_ACTION | Sync | Gestionnaire, masque et drapeaux d'un signal |
| 0x0B02 | SYS_SIG_MASK | Sync | Masque du thread appelant |
| 0x0B03 | SYS_SIG_SEND | Sync | Envoyer un signal à un processus ou à un thread |
| 0x0B04 | SYS_SIG_RETURN | Sync | Revenir d'un gestionnaire |
| 0x0B05 | SYS_SIG_ALTSTACK | Sync | Pile de secours des gestionnaires |
| 0x0B06 | SYS_SIG_WAIT | Async | Attendre un signal |

Une faute du programme (accès invalide, instruction illégale) est remise à
son gestionnaire s'il en a un, sinon elle termine le processus.

Ces domaines sont ouverts aux types `.cus`, `.gs` et `.ras`, dans les limites
des permissions déclarées (§12.3) : `SYS_NET_*` exige la déclaration réseau,
comme les appels 0x0801 à 0x0807.

---

### 6.8 Domaine 0C — GPU `SYS_GPU_*` (extension du 24 septembre 2026)

L'interface des programmes vers la pile graphique (§8.5). Explicite, dans
l'esprit de Vulkan, réduite à ce que Nova sert : pas d'état global caché,
les objets et leur mémoire sont créés et détruits par le programme, les
commandes sont enregistrées puis soumises, la synchronisation passe par des
barrières. La bibliothèque `nova-gfx` (Rust, et un en-tête C) l'enveloppe ;
c'est elle que les programmes utilisent.

**Convention d'appel** : celle du §6.7 (numéro dans RAX, arguments dans RDI,
RSI, RDX, R10, R8, R9, erreur négative), **pour tous les types de binaires** :
`nova-gfx` est la même bibliothèque pour un `.ss` natif et pour un programme
porté. Les objets sont désignés par des **poignées** propres au processus,
jamais par une adresse du noyau ni un identifiant de l'hôte.

| N° | Nom | Mode | Description |
|---|---|---|---|
| 0x0C01 | SYS_GPU_INFO | Sync | Présence d'un GPU, périphérique, capacités, limites (tailles, formats, mémoire) |
| 0x0C02 | SYS_GPU_CONTEXT_CREATE | Sync | Créer un contexte (tous les objets vivent dans un contexte) |
| 0x0C03 | SYS_GPU_CONTEXT_DESTROY | Sync | Détruire un contexte et tout ce qu'il contient |
| 0x0C04 | SYS_GPU_MEM_ALLOC | Sync | Allouer de la mémoire GPU : locale au GPU, visible des deux côtés, ou partageable |
| 0x0C05 | SYS_GPU_MEM_FREE | Sync | Rendre une allocation |
| 0x0C06 | SYS_GPU_MEM_MAP | Sync | Projeter une allocation visible dans l'espace du processus |
| 0x0C07 | SYS_GPU_MEM_UNMAP | Sync | Retirer la projection |
| 0x0C08 | SYS_GPU_OBJECT_CREATE | Sync | Créer un objet : tampon, image, échantillonneur, module de shader (SPIR-V), pipeline, disposition de ressources |
| 0x0C09 | SYS_GPU_OBJECT_DESTROY | Sync | Détruire un objet |
| 0x0C0A | SYS_GPU_SUBMIT | Async | Soumettre un tampon de commandes (format Nova), avec barrières attendues et signalées |
| 0x0C0B | SYS_GPU_FENCE_CREATE | Sync | Créer une barrière |
| 0x0C0C | SYS_GPU_FENCE_WAIT | Async | Attendre une ou plusieurs barrières, échéance en nanosecondes |
| 0x0C0D | SYS_GPU_FENCE_EXPORT | Sync | Barrière en descripteur (§6.7), attendable par `SYS_OBJ_WAIT` |
| 0x0C0E | SYS_GPU_SHARE | Sync | Rendre une image partageable avec un autre processus → jeton |
| 0x0C0F | SYS_GPU_IMPORT | Sync | Importer une image partagée par son jeton |
| 0x0C10 | SYS_GPU_PRESENT | Async | Confier une image au compositeur pour une surface (§8.5) |

**Tampon de commandes.** Un format propre à Nova, plat, en mémoire du
programme : début et fin de passe de rendu, liaison de pipeline et de
ressources, zone d'affichage et découpe, constantes, dessin (indexé ou non),
copie entre tampons et images, barrière de mémoire. Le noyau le **valide en
entier** (bornes, objets appartenant au contexte, états compatibles) avant
de le traduire pour le backend ; une commande invalide rejette la soumission
entière, sans effet partiel.

**Shaders.** Fournis en **SPIR-V** (format de fichier standard de Khronos),
validés à la création du module : structure bien formée, capacités limitées
à celles que `SYS_GPU_INFO` annonce, taille bornée. Nova n'a pas de
compilateur de shaders à l'exécution : un programme les compile à sa
construction.

**Limites et sûreté.** Mémoire GPU comptée par processus et plafonnée ; un
tampon de commandes qui ne finit pas dans le **délai de garde** (2 s par
défaut) fait perdre son contexte au programme (`SYS_ERR` de contexte perdu),
sans bloquer l'écran ni les autres ; la mort d'un processus rend tous ses
objets. Ouvert aux types `.dsa`, `.ss`, `.gs`, `.cus` et `.ras` ; aucune
permission supplémentaire.

Sans GPU (`SYS_GPU_INFO` : absent), tous les autres appels rendent une erreur
explicite ; les programmes gardent leur rendu logiciel.

**Erreurs.** Une valeur de retour négative de -1 à -4095 est un numéro
d'erreur (numérotation de musl) : `-ENODEV` pas de GPU, `-EBADF` poignée
inconnue ou d'une autre nature, `-EINVAL` arguments ou tampon de commandes
refusés, `-ENOMEM`, `-EDQUOT` quota du processus atteint, `-ENOTRECOVERABLE`
contexte perdu, `-ETIMEDOUT`, `-EINTR`. `SYS_GPU_MEM_MAP` rend une adresse :
seules les valeurs -1 à -4095 y sont des erreurs.

**Limites par processus.** 2 Gio de mémoire GPU (quota rapporté par
`SYS_GPU_INFO`), 16 contextes, 16 384 poignées ; 16 soumissions en vol par
contexte, au-delà `SYS_GPU_SUBMIT` attend. Un tampon de commandes fait au
plus 1 Mio, un module SPIR-V 1 Mio.

**Arguments.**

| Appel | Arguments |
|---|---|
| INFO | `(info*, taille)` : `struct nova_gpu_info` (128 octets) |
| CONTEXT_CREATE / DESTROY | `()` → contexte ; `(ctx)` |
| MEM_ALLOC | `(ctx, taille, genre)` : 0 locale, 1 visible (écrite par le processeur), 2 relecture (mise en cache) |
| MEM_FREE / MAP / UNMAP | `(ctx, mem)` ; une mémoire occupée par des objets ne se libère pas |
| OBJECT_CREATE | `(ctx, genre, description*, taille)` : 1 tampon, 2 image, 3 échantillonneur, 4 shader, 5 disposition, 6 pipeline |
| OBJECT_DESTROY | `(ctx, poignée)` : objets et barrières ; la destruction attend la fin des soumissions qui peuvent s'en servir |
| SUBMIT | `(ctx, commandes*, octets, barrière à lever, barrières attendues*, nombre)` |
| FENCE_CREATE | `(ctx)` → barrière déjà levée |
| FENCE_WAIT | `(barrières*, nombre, délai en ns, toutes)` ; délai `u64::MAX` : sans limite |
| FENCE_EXPORT | `(barrière)` → descripteur lisible une fois la barrière levée (programmes de la bibliothèque C Nova) |
| SHARE / IMPORT | `(ctx, image)` → jeton ; `(ctx, jeton)` → image |
| PRESENT | `(ctx, image, surface, barrière)` |

Les descriptions (`struct nova_*_desc`) et le détail des champs sont ceux de
`tools/ports/nova-gfx/nova_gfx.h` ; le noyau les lit champ par champ.

**Format du tampon de commandes.** Des mots de 32 bits petit-boutistes. Une
commande : un mot d'en-tête `opcode | (longueur en mots, en-tête compris) <<
16`, puis ses opérandes. Les décalages de 64 bits prennent deux mots (bas
d'abord), les couleurs, profondeurs et fenêtres d'affichage sont les motifs
binaires de flottants 32 bits.

| Opcode | Commande | Opérandes |
|---|---|---|
| 1 | BEGIN_RENDER | image couleur, chargement (0 effacer, 1 garder, 2 indifférent), couleur ×4, image de profondeur (0 : aucune), chargement, valeur, x, y, largeur, hauteur |
| 2 | END_RENDER | — |
| 3 | BIND_PIPELINE | pipeline |
| 4 | BIND_VERTEX | emplacement (0-3), tampon, décalage |
| 5 | BIND_INDEX | tampon, décalage, type (0 : 16 bits, 1 : 32 bits) |
| 6 | BIND_RESOURCE | liaison, objet, échantillonneur, décalage, étendue |
| 7 | VIEWPORT | x, y, largeur, hauteur, profondeur min, max |
| 8 | SCISSOR | x, y, largeur, hauteur |
| 9 | PUSH_CONSTANTS | décalage, puis les données |
| 10 | DRAW | sommets, instances, premier sommet, première instance |
| 11 | DRAW_INDEXED | indices, instances, premier indice, décalage de sommet, première instance |
| 12 | COPY_BUFFER | source, destination, décalage source, décalage destination, taille |
| 13 | COPY_BUFFER_TO_IMAGE | tampon, décalage, pixels par ligne (0 : serré), image, x, y, largeur, hauteur |
| 14 | COPY_IMAGE_TO_BUFFER | image, x, y, largeur, hauteur, tampon, décalage, pixels par ligne |
| 15 | COPY_IMAGE | source, x, y, destination, x, y, largeur, hauteur |
| 16 | BLIT_IMAGE | source, rectangle, destination, rectangle, filtre (0 plus proche, 1 linéaire) |
| 17 | CLEAR_IMAGE | image, couleur ×4 |
| 18 | FILL_BUFFER | tampon, décalage, taille, valeur |
| 19 | UPDATE_BUFFER | tampon, décalage, puis les données (64 Kio au plus) |

Ce que le programme n'écrit pas, le noyau le fournit : la zone de rendu est
la fenêtre d'affichage et la découpe par défaut ; les images vivent dans un
état unique dès leur premier usage ; une barrière mémoire complète sépare
les passes. Un programme ne peut pas écrire de conflit d'accès.

## 7. Scheduler

### 7.1 Modèle général

Scheduler préemptif à priorités **fixes** par format de binaire. La priorité est
déterminée au lancement et ne change pas en cours d'exécution, sauf pour les
`.gs` qui basculent automatiquement entre avant et arrière-plan.

### 7.2 Affinité CPU — Répartition sur les cœurs

| Cœurs | Types assignés | Note |
|---|---|---|
| Cœur 0 | `.dsa` exclusivement | Jamais partagé — toujours disponible pour le système |
| Cœurs 1..N/2 | `.ss` + compositeur GUI | Partagés entre eux uniquement |
| Cœurs N/2..N-1 | `.gs` + `.cus` + `.ras` | Partagés entre eux |
| Tous | Kernel | Peut s'exécuter sur n'importe quel cœur |

### 7.3 Tranches CPU par priorité

| Type | Priorité | Arrière-plan | Premier plan | Note |
|---|---|---|---|---|
| `.dsa` | 1 | 40 (fixe) | 40 (fixe) | Jamais réduit |
| `.ss` | 2 | 15 | 25 | — |
| `.gs` | 3 | 2 | 20 | Bascule automatiquement |
| `.cus` | 4 | 4 | 10 | — |
| `.ras` | 5 | 1 | 5 | Jamais modifiable |

### 7.4 États des processus

| État | Description | Transition |
|---|---|---|
| READY | Attend son tour CPU | → RUNNING quand scheduler le choisit |
| RUNNING | Utilise le CPU maintenant | → READY (time slice) ou SLEEPING (attente) |
| SLEEPING | Attend un événement | → READY quand événement reçu |
| SUSPENDED | Mis en pause manuellement | → READY via SYS_PROC_RESUME |
| DEAD | Terminé, en attente nettoyage | Mémoire libérée par kernel |

### 7.5 Anti-famine

Seuil : **500 ms** sans aucune tranche CPU. Le scheduler force alors 1 tranche,
quelle que soit la priorité. Évite le gel complet sans `SYS_PROC_SUSPEND` explicite.

---

## 8. Système Graphique

### 8.1 Architecture

Le compositeur est une application `.ss` — **le seul composant autorisé à écrire
sur le framebuffer**. Les applications ne dessinent jamais directement : elles
envoient des instructions IPC et le compositeur exécute le rendu.

### 8.2 Modules natifs

| Module | Slot | Remplaçable | Rôle |
|---|---|---|---|
| `compositor_core.mod` | — | Non | Noyau du compositeur — non remplaçable |
| `taskbar.mod` | Taskbar | Oui | Barre des tâches, apps ouvertes |
| `winmanager.mod` | WindowManager | Oui | Fenêtres, redimensionnement, z-order |
| `notifs.mod` | Notifications | Oui | Toasts et centre de notifications |
| `wallpaper.mod` | Wallpaper | Oui | Fond d'écran et bureau |
| `launcher.mod` | Launcher | Oui | Menu et lanceur d'apps |
| `cursor.mod` | Overlay | Oui | Rendu du curseur souris |

### 8.3 Cycle de vie d'un module

- Fichier déposé dans `/sys/gui/modules/`
- Détection automatique par le Module Loader (pas d'intervention utilisateur)
- Lecture header `.mod` — validation signature
- Chargement en mémoire isolée — crash d'un module ne bloque pas l'OS
- Mise en pause : renommer `.mod` en `.pause` → désenregistrement propre

### 8.4 Customisation

Modules avec `replaceable: true` peuvent être remplacés. Les modules dans
`/users/{user}/settings/gui/modules/` ont priorité sur les modules système. Un
fichier `theme.cfg` par utilisateur contrôle les couleurs, polices, coins
arrondis et dimensions.

### 8.5 Pile graphique Nova (extension du 24 septembre 2026)

But : exploiter la puissance de calcul du GPU pour un affichage fluide, avec
une pile **propre à Nova** — ni pilote, ni bibliothèque graphique existants
(pas de Mesa, pas d'interface DRM de Linux), et **sans pilote propriétaire**.

```
programmes (.ss/.cus/.gs...)  --nova-gfx-->  SYS_GPU_* (§6.8)
                                                   |
compositeur (seul maitre de l'ecran) ------> coeur GPU (noyau)
                                                   |
                                         interface de backend
                                           |               |
                            backend virtio-gpu      modules .kmd de
                            (integre au noyau)      fabricants (§3.6)
```

**Cœur GPU** (noyau, indépendant du périphérique) : poignées et objets par
processus, mémoire comptée, files de commandes et ordonnancement équitable,
barrières, validation de tout ce qui est soumis, délai de garde et reprise,
propriété de l'écran et du curseur.

**Interface de backend** : ce qu'un pilote de carte fournit au cœur —
capacités, allocation et projection de mémoire, création d'objets,
exécution d'une liste de commandes déjà validée, suivi de ses travaux,
export et import d'images. Version `0x00010000` (`INTERFACE_VERSION`) ;
publiée pour les fabricants et les utilisateurs qui écrivent des pilotes
(`tools/kmd/nova_kmd.h`, module signé du §3.6). Nova n'écrit pas de pilote
pour chaque carte réelle.

**Un contexte, un périphérique.** Chaque contexte d'un programme ouvre sa
propre session sur l'hôte — une instance et un périphérique Vulkan à lui :
une faute de ses shaders ou un flux illisible perd ce contexte seul. Le
délai de garde est de **2 secondes** par soumission.

**Backend intégré : virtio-gpu 3D, protocole Venus.** Sous QEMU, le GPU de
l'hôte (quel qu'il soit) est atteint par le périphérique virtio-gpu. Nova
lui parle **Venus** : des commandes Vulkan sérialisées, shaders SPIR-V,
exécutées par le pilote Vulkan de l'hôte. L'encodeur est **généré** par un
outil de Nova depuis le registre public de Vulkan (`vk.xml`) ; aucun code
de Mesa ni de virglrenderer n'est repris. La mémoire partagée entre hôte et
invité (`hostmem`, 8 Gio par défaut) porte les ressources « blob » : les
images passent sans copie.

**Écran.** Le compositeur (§8.1) reste le seul à décider de ce qui s'affiche.
Avec un GPU, les surfaces des fenêtres sont des images GPU qu'il compose sur
le GPU ; une application qui dessine sur le GPU lui confie son image par
`SYS_GPU_PRESENT`, sans copie. Le curseur passe par le plan curseur du
périphérique quand il en a un.

**Repli.** Sans backend (pas de virtio-gpu 3D, carte sans module, `-vga
none`, émulation logicielle), le cœur se déclare absent : le compositeur et
les applications gardent le rendu logiciel, sans erreur. Sur une machine
réelle sans module de fabricant, Nova affiche par le framebuffer GOP/VBE.

**Hors de cette extension** : calcul général (compute), compilateur de
shaders à l'exécution (donc WebGL et WebGPU dans le navigateur), décodage
vidéo matériel.

---

## 9. Système de Notifications

### 9.1 Niveaux de priorité

| Niveau | Valeur | Toast | Son | Plein écran | Mode NDD | Disparition |
|---|---|---|---|---|---|---|
| PRIORITAIRE | 0 | Forcé | Oui | Oui | Ignoré | Bouton obligatoire |
| URGENT | 1 | Oui | Oui | Oui | Ignoré | 15 secondes |
| HAUTE | 2 | Oui | Oui | Non | Bloqué | 8 secondes |
| NORMALE | 3 | Si actif | Opt. | Non | Bloqué | 5 secondes |
| BASSE | 4 | Non | Non | Non | Bloqué | Centre uniquement |

### 9.2 Conditions de disparition

- Timeout fixe en millisecondes
- Durée automatique selon priorité (voir tableau)
- Fermeture manuelle par l'utilisateur
- Clic sur un bouton d'action spécifique
- Événement IPC envoyé par l'application (ex : transfert terminé)
- Combinaison : `Any([condition1, condition2])` — la première remplie

### 9.3 Permissions par type de binaire

| Niveau | DSA | SS | GS | CUS | RAS |
|---|---|---|---|---|---|
| PRIORITAIRE | Oui | Oui | Non | Non | Non |
| URGENT | Oui | Oui | Oui | Non | Non |
| HAUTE | Oui | Oui | Oui | Oui | Non |
| NORMALE | Oui | Oui | Oui | Oui | Oui |
| BASSE | Oui | Oui | Oui | Oui | Oui |

---

## 10. Gestionnaire de Paquets — SPM

### 10.1 Format de paquet `.spm`

Un paquet est un fichier binaire unique embarquant : header SPM (magic, version,
code MAJ, hash SHA-256, signature), manifest (métadonnées), binaire principal,
ressources et scripts pre/post installation.

### 10.2 Codes de mise à jour

| Code | Type | Comportement | Redémarrage |
|---|---|---|---|
| -1 | Obligatoire | Notification forcée + explication — bouton non ignorable | Si nécessaire |
| 0 | Standard | Notification + confirmation utilisateur | Non |
| 1 | Optionnelle | Notification discrète, ignorable | Non |
| 2 | Silencieuse | Aucune notification, appliquée en arrière-plan | Non |

### 10.3 Commandes SPM

```
spm install firefox              # depuis repo, global
spm install firefox --user       # scope personnel
spm install ./monApp.spm         # depuis fichier local
spm remove firefox               # désinstalle + nettoie
spm update                       # vérifie toutes les MAJ
spm search firefox               # cherche dans tous les repos
spm repo add <url>               # ajoute un dépôt tiers
```

### 10.4 Niveaux d'avertissement à l'installation

| Combinaison | Niveau | Action |
|---|---|---|
| SYSTEM + tout type | Badge officiel | Aucun avertissement |
| ENTERPRISE + `.cus`/`.gs` | Badge vérifié | Aucun avertissement |
| ENTERPRISE + `.ss`/`.dsa` | Avertissement | Prompt admin obligatoire |
| SELF + tout type | Non vérifié | Avertissement jaune |
| Aucune + `.ras` | Dangereux | Avertissement rouge bloquant |

---

## 11. Mises à Jour de l'OS

### 11.1 Types de mises à jour

| Type | Code | Format | Redémarrage |
|---|---|---|---|
| PATCH | 0 | Incrémentale — fichiers modifiés uniquement | Non |
| UPDATE | 1 | Incrémentale — plusieurs composants | Non |
| MAJOR | 2 | Complète — nouvelle partition | Oui |
| CRITICAL | -1 | Incrémentale ou complète selon impact | Selon impact |

### 11.2 Détection

- Automatique : service `updater.dsa` — vérification toutes les 24 h (configurable)
- Manuelle : `spm update --os`
- Téléchargement silencieux en arrière-plan avant notification

### 11.3 Système de rollback automatique

Après une mise à jour MAJOR, le boot suivant valide tous les services core
(timeout 60 s). En cas d'échec, rollback automatique vers la version précédente.
La notification explique la raison. Pour les mises à jour incrémentales, un
snapshot est conservé 30 jours dans `/sys/updates/rollback/`.

---

## 12. Sécurité & Sandbox

### 12.1 Isolation par type et groupe

Chaque application possède une `AppIdentity` **immuable définie à la compilation**
(app_id, author, group, version, binary_type, signature). La communication entre
apps suit des règles strictes selon le groupe et le type.

| Situation | Communication | Condition |
|---|---|---|
| Même groupe + même type | IPC directe | Autorisée sans prompt |
| Même groupe + types compatibles | IPC directe | DSA↔SS, SS↔CUS, CUS↔GS |
| Groupes différents | Port interne dédié | App réceptrice accepte/refuse + prompt utilisateur |
| RAS ↔ tout | Bloquée | Jamais — isolation totale |

### 12.2 Ports internes applicatifs

Les ports internes sont des canaux isolés du réseau OS, dédiés à une app. Deux
apps peuvent utiliser le même numéro sans conflit — jamais mappés vers
l'extérieur. L'app réceptrice reçoit la requête + `AppIdentity` complète et
décide d'accepter ou refuser.

### 12.3 Modèle de permissions hybride

- **Liste blanche** (accès sensibles) : réseau internet, micro, webcam, `/sys/`,
  `SYS_HW_*`, gestion utilisateurs — déclarés dans le header, refusés sinon
- **Liste noire** (toujours bloqué) : `.ras` ne peut pas communiquer avec
  d'autres apps, accès mémoire inter-processus, modification des binaires d'une
  autre app

### 12.4 Sandbox par type

| Type | Mémoire | Fichiers | IPC | Réseau |
|---|---|---|---|---|
| `.dsa` | Confiance totale | Accès complet | Libre | Complet |
| `.ss` | Isolée des apps user | Étendu selon déclaration | Libre avec `.ss` et `.dsa` | Selon déclaration |
| `.gs` | Isolée | `/games/{app}/` + `/users/{user}/home/` | Groupe + port interne | Selon déclaration |
| `.cus` | Isolée | `/apps/{app}/` + `/users/{user}/home/` | Groupe + port + prompt | Selon déclaration |
| `.ras` | Isolée | `/ras/{app}/` uniquement | Aucune | Bloqué par défaut |

### 12.5 Prompt d'autorisation

Tout accès hors sandbox déclenche un prompt en temps réel affichant l'identité de
l'app, la ressource demandée et une case « Se souvenir de ce choix ». Le choix
mémorisé est stocké dans `/users/{user}/settings/permissions/{app_id}.cfg` et
révocable depuis les paramètres.

---

## 13. Compatibilité Langages & Anti-Electron

### 13.1 Outil ETR — Electron-To-Rust

Outil manuel lancé par le développeur. Convertit une app Electron
(JS/HTML/CSS + Node.js) en Rust natif via analyse AST et mapping des équivalents
système. Génère un rapport de conversion avec taux de réussite et avertissements.

- Conversion ≥ 70 % → binaire `.cus` natif produit
- Conversion < 70 % ou `eval()` détecté → classement automatique en `.ras` avec avertissement
- Réduction typique : 180 Mo → 4 Mo

### 13.2 Langages supportés

| Langage | Interpréteur | Compilateur | Formats cibles |
|---|---|---|---|
| Rust | REPL interactif | Natif direct ; cible `x86_64-linux-musl` liée à la libc Nova pour un programme porté | Tous |
| C / C++ | LLVM JIT | LLVM → natif (image liée à la libc Nova, §3.5, §6.7) | `.dsa`, `.ss`, `.gs`, `.cus`, `.ras` |
| C# | Runtime Mono allégé | Transpilation → Rust | `.ss`, `.gs`, `.cus`, `.ras` |
| Java | JVM maison minimale | Bytecode → Rust | `.gs`, `.cus`, `.ras` |
| Python | CPython adapté | rustpython → natif | `.cus`, `.ras` |
| Node.js | V8 allégé | JS → Rust | `.cus`, `.ras` |
| Bun | Runtime Bun porté | TS/JS → Rust | `.cus`, `.ras` |
| ASM x86_64 | Émulateur sandboxé | Assemblage direct | Tous (SYSTEM requis pour `.dsa`) |

### 13.3 Runtimes — Emplacement

- `/sys/runtimes/` — runtimes système globaux (tous les utilisateurs)
- `/users/{user}/runtimes/` — runtimes personnels (versions alternatives)
- Priorité : runtime utilisateur > runtime système pour ce user

---

## 14. Internationalisation (i18n)

### 14.1 Priorité des paramètres

**Application > Utilisateur > Système.** Tout changement est immédiat — le
compositeur recharge les traductions via `SYS_IPC_PUBLISH("i18n.changed")` sans
redémarrage.

### 14.2 Ce que l'i18n couvre

| Domaine | Options |
|---|---|
| Langue | Langue principale + fallback si traduction manquante |
| Formats date | DD/MM/YYYY, MM/DD/YYYY, YYYY-MM-DD |
| Formats heure | 24 h / 12 h (AM/PM) |
| Nombres | Séparateur milliers : espace / virgule / point |
| Monnaie | Code devise + position avant/après le montant |
| Distance | km / miles / nautical_miles |
| Poids | kg / lbs |
| Température | Celsius / Fahrenheit / Kelvin |
| Vitesse | km/h / mph / knots |
| Clavier | Layout principal + secondaire + touche de switch |
| Fuseau horaire | Toutes dates stockées en UTC, converties à l'affichage |

### 14.3 Format `.lang`

Fichier clé-valeur. Variables dynamiques avec syntaxe `{nom}`. Stockés dans
`/sys/i18n/{locale}/` pour le système et `/apps/{nom}/i18n/` pour les apps.

---

## 15. SDK Développeur

### 15.1 Vue d'ensemble

- **CLI** — commande `dev` (outil principal)
- **IDE** — `devstudio.ss` (environnement graphique intégré)
- **libdev** — bibliothèque standard avec abstractions haut niveau
- **Sandbox** — test directement sur l'OS hôte dans un contexte isolé

### 15.2 Commandes CLI principales

```
dev new monapp --type cus --lang rust    # nouveau projet
dev build --release                      # build optimisé
dev run --debug                          # sandbox + débogueur
dev interpret main.py                    # test sans compilation
dev package --sign enterprise            # génère le .spm signé
dev convert --from electron ./monapp     # ETR conversion
```

### 15.3 Fichier `dev.toml`

Manifest du projet : `app_id`, `version`, `author`, `binary_type`, `lang`,
`entry`, `signature`, `permissions` (liste blanche), ports internes, dépendances.

### 15.4 Sandbox de test

Chaque run crée un contexte isolé : espace mémoire dédié, FS virtualisé (overlay
lecture seule sur le vrai FS), réseau loopback uniquement. L'app tourne en `.ras`
automatiquement. À l'arrêt : sandbox détruite, aucune trace sur l'OS hôte.

### 15.5 Templates de projet

| Template | Description |
|---|---|
| blank | Projet vide |
| gui-app | App avec fenêtre de base |
| service | Service `.dsa` sans GUI |
| game | Jeu `.gs` avec boucle de jeu |
| cli-tool | Outil CLI sans GUI |

---

## 16. Accessibilité

### 16.1 Architecture

Tous les composants d'accessibilité sont des modules `.mod` remplaçables. Le
module coordinateur `accessibility.mod` maintient un arbre d'accessibilité
parallèle à l'arbre de rendu. Chaque élément UI expose des métadonnées (rôle,
label, hint, valeur, focusable, raccourcis).

### 16.2 Modules

| Module | Fonction | Remplaçable |
|---|---|---|
| `accessibility.mod` | Coordinateur — arbre d'accessibilité | Oui |
| `screenreader.mod` | Lecteur d'écran + synthèse vocale | Oui |
| `zoom.mod` | Zoom global / loupe / zone | Oui |
| `contrast.mod` | Contrastes élevés + modes daltonisme | Oui |
| `keyboard-nav.mod` | Navigation clavier complète sans souris | Oui |

### 16.3 Modes daltonisme supportés

- Protanopie (rouge difficile)
- Deutéranopie (vert difficile)
- Tritanopie (bleu difficile)
- Achromatopsie (absence totale de couleur)
- Custom (matrice de correction personnalisée)

### 16.4 Raccourcis d'activation globaux

| Raccourci | Action |
|---|---|
| Alt+Super+S | Toggle lecteur d'écran |
| Alt+Super+Z | Toggle zoom |
| Alt+Super+C | Toggle contraste élevé |
| Alt+Super+K | Toggle navigation clavier |
| Tab / Shift+Tab | Focus élément suivant / précédent |
| Entrée / Espace | Activer l'élément focusé |
| Super+Tab | Switcher entre fenêtres |
| F6 | Switcher entre panneaux d'une fenêtre |

Tous ces raccourcis sont fonctionnels **dès l'écran de connexion**. Le bouton
Accessibilité est toujours visible et navigable au clavier.

---

## 17. Arborescence Système Complète

```
/                           ← Disque par défaut
├── sysapp/                 ← Applications système (.dsa, .ss)
├── sys/
│   ├── kernel/             ← Binaires kernel et modules
│   │   └── modules/gpu/    ← Pilotes GPU de fabricants (.kmd signés, §3.6)
│   ├── drivers/            ← Drivers user space
│   ├── config/             ← Configuration système globale
│   ├── gui/
│   │   ├── modules/        ← Modules GUI actifs (.mod)
│   │   ├── modules/boot/   ← Modules splash screen
│   │   ├── modules/accessibility/
│   │   ├── fonts/          ← Polices système
│   │   ├── themes/         ← Thèmes globaux
│   │   └── compositor.ss   ← Binaire compositeur
│   ├── i18n/               ← Traductions système + voix TTS
│   ├── runtimes/           ← Runtimes langages (rust, c, java...)
│   ├── tools/
│   │   ├── spm/            ← Gestionnaire de paquets
│   │   ├── sdk/            ← SDK développeur
│   │   ├── etr/            ← Convertisseur Electron→Rust
│   │   ├── interpret/      ← Interpréteurs
│   │   └── compile/        ← Compilateurs
│   └── updates/            ← Paquets MAJ en attente + rollback
├── games/                  ← Jeux (.gs)
├── apps/                   ← Applications communes (.cus)
├── ras/                    ← Applications RAS (.ras)
├── libs/
│   ├── sys/                ← Libs système officielles
│   └── shared/             ← Libs partagées
├── cache/                  ← Cache temporaire global
├── logs/
│   ├── kernel/             ← Logs kernel (boot, security, update)
│   ├── drivers/
│   └── apps/
├── net/                    ← Config réseau, interfaces, Wi-Fi
└── users/
    ├── _shared/            ← Fichiers partagés entre users
    └── {user}/
        ├── apps/           ← Apps personnelles
        ├── settings/       ← Config, thèmes, permissions, i18n
        ├── cache/          ← Cache utilisateur
        ├── runtimes/       ← Runtimes alternatifs
        ├── logs/           ← Logs apps utilisateur
        └── home/
            ├── desktop/    downloads/  archive/
            ├── images/     videos/     documents/
            └── executables/
```

---

## 18. Roadmap d'Implémentation

| Phase | Objectif | Composants clés |
|---|---|---|
| Phase 1 | Environnement & Boot | Toolchain Rust no_std, GRUB Multiboot2, point d'entrée ASM, affichage VGA, QEMU |
| Phase 2 | Microkernel | Paging x86_64, IDT, APIC, scheduler Round-Robin, IPC, syscall gate |
| Phase 3 | Formats binaires | Parser des headers, loader de binaires, système de signatures |
| Phase 4 | Système de fichiers | Node table, liens virtuels, VFS, permissions |
| Phase 5 | API système | Tous les domaines `SYS_*`, SysResult, SysError |
| Phase 6 | Couche OS | Drivers user space, FS, réseau, audio |
| Phase 7 | Système graphique | Framebuffer GOP, compositeur, modules .mod, protocole IPC GUI |
| Phase 8 | SPM & Mises à jour | Gestionnaire de paquets, dépôts, rollback |
| Phase 9 | SDK & Langages | CLI dev, IDE devstudio, runtimes, ETR |
| Phase 10 | Finitions | Accessibilité, i18n, sécurité avancée, sous-systèmes Linux/Windows |
| Phase 11 | Navigateur | Fork de Firefox, sous-système Linux étendu |
| Phase 12 | Navigateur natif | libc Nova, image liée, domaines 09 à 0B |
| Phase 13 | Pile graphique | Cœur GPU, backend virtio-gpu/Venus, `SYS_GPU_*`, modules `.kmd`, Firefox sur la pile Nova |

Le détail granulaire de ces phases se trouve dans `docs/ROADMAP.md`.

### Ressources recommandées

- `blog.phil-opp.com` — « Writing an OS in Rust » (référence pour les phases 1 et 2)
- `redox-os.org` — OS microkernel en Rust, architecture proche du projet
- `wiki.osdev.org` — référence technique exhaustive sur le développement OS x86
- Outils : rustup, cargo, qemu-system-x86_64, xorriso (création ISOs GRUB)
