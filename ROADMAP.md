# Nova OS — Roadmap de développement

Basée sur `SPECIFICATION.md` v1.0. Environnement : CachyOS, Rust nightly,
x86_64, GRUB/Multiboot2.

> **Ce fichier se met à jour à chaque fin d'étape**, après validation par la
> sortie série. Ne jamais cocher une case sur la foi d'une compilation réussie :
> seul un comportement observé à l'exécution vaut validation.

---

## État d'avancement

| Phase | État | Détail |
|---|---|---|
| Phase 1 — Environnement & Boot | ✅ terminée | 1.1 → 1.7 |
| Phase 2 — Microkernel | ✅ terminée | 2.1 → 2.9, SMP inclus |
| Phase 3 — Formats binaires | ✅ terminée | 3.1 → 3.4 |
| Phase 4 — Système de fichiers | ✅ terminée | 4.1 → 4.6, 128 vérifications ; l'export NTFS reste non relu par un outil hôte |
| Phase 5 — API système | ✅ terminée | 5.1 → 5.6, 262 vérifications (265 sous quatre cœurs) |
| Phase 6 — Couche OS | ✅ terminée | 6.1 → 6.5, 120 ou 121 vérifications selon le matériel présent |
| Phase 7 — Système graphique | ✅ terminée | 7.1 → 7.7, 13 + 140 vérifications ; le tableau visuel se confirme à l'écran, pas en série |
| Phase 8 — SPM & mises à jour | ✅ terminée | 8.1 → 8.5, 224 vérifications |
| Phase 9 — SDK & langages | 🔄 en cours | 21 cases sur 27 ; 433 vérifications au 5 septembre, **à relever** depuis le retrait de devstudio (ses quatre contrôles sont partis avec lui) ; les 6 restantes attendent une toolchain et un débogueur |
| Phase 10 — Finitions | ✅ terminée | 10.1 → 10.4, 340 vérifications |
| Phase 11 — Navigateur Web Browser (fork de Firefox) | ✅ close (24 septembre 2026) | Close sur décision de l'utilisateur ; 11.5 et 11.15 cochées sur les relevés série des 22-23 septembre ; quatre cases non faites, reportées : 11.10.1 (→ 12.8.2), retrait de `navigateur.cus` (11.10.2), GPU (11.12.6 → phase 13), rectangle de `PRESENT` (11.13.6) |
| Phase 12 — Navigateur natif (libc Nova) | 🔧 implémentée, à valider | Navigateur en `.cus` sur la libc Nova ; objets du noyau sortis de `src/linux/` vers `src/posix/` (23 septembre, soir) ; aucune case cochée |
| Phase 13 — Pile graphique Nova et accélération GPU | 🔧 faite hors Firefox, à valider | Spec acceptée (24 septembre) ; pilote virtio-gpu 3D, anneau Venus, encodeur généré et vérifié (68 commandes), cœur GPU, écran et plan curseur au cœur, compositeur sur le GPU (égal au pixel près au processeur), `SYS_GPU_*`, bibliothèques C et Rust, modules `.kmd` signés, délai de garde éprouvé ; 75 vérifications ; aucune case cochée (validation de l'utilisateur attendue) ; Firefox (13.0, 13.9-13.10, 13.11.1) reporté par l'utilisateur |
| Phase 14 — LIFS (nouveau type de partition, liens CoW et Tag) | 🔧 implémentée, à valider | Décisions 14.1 prises (24 septembre) ; GPT/MBR, partitions montées une lettre chacune, racine LIFS, plus aucun formatage implicite ; LIFS (arbres B+, étendues, clones CoW, Tag, index inverse, journal) ; `SYS_FS_NODE_*` ; `tools/lifs.py` et reprise NovaFS → LIFS ; 81 vérifications LIFS dont 32 coupures simulées ; aucune case cochée (validation de l'utilisateur attendue) |
| Phase 15 — Mises à jour atomiques des applications | ✅ faite (28 octobre 2026) | Proposition de l'utilisateur du 24 septembre 2026 ; points 15.1 tranchés sous mandat d'autonomie ; JSON, Ed25519 (ENTERPRISE enfin signé), HTTPS en flux, pointeur atomique, registre signé, versions à part, lancement par le pointeur, nettoyage différé, `spm.dsa` ; 70 vérifications ; reste 15.6.2 (canal par application dans l'interface) et 15.7.3 partiel |
| Phase 16 — Langages et outils (fin de la phase 9) | 🔄 en cours | 16.0 faite (28 octobre 2026) : shell, `terminal.ss`, shell série et mode recovery, 60 vérifications + `make test-recovery` ; restent le moteur JavaScript complet (navigateur intégré et ligne Node/Bun du §13.2), Python, C, Java, débogueur de `dev run --debug` |
| Phase 17 — Reliquats système de fichiers et POSIX | 📝 planifiée | Ce que les phases 12 et 14 ont laissé ouvert hors Firefox : page de temps, autotest appel par appel, `copy_file_range`/clonage, Tag dans l'explorateur, GPT de secours, partitionneur |
| Phase 18 — Firefox sur la pile Nova | ⏸ chez l'auteur | 12.5, 12.7, 12.8, 13.0, 13.9, 13.10, 13.11.1 : demandent la construction du fork et le GPU de la machine de l'auteur |

**La phase 10 est validée.** Les vingt cases du 10.1 au 10.4 sont
cochées sur la foi des sorties série du 5 septembre 2026 : `make run`,
`make run-smp`, un premier démarrage après `make disk-reset`, un
démarrage sans framebuffer (`-vga none`) et un démarrage sans carte son.
Les cinq relevés sont détaillés plus bas, dans « Comment valider la
phase 10 ». Les dix phases de la roadmap sont écrites ; il ne reste plus
de case à ouvrir, seulement des limites à lever — elles sont listées
phase par phase plus bas.

**La roadmap a été passée en revue le 5 septembre 2026 : 222 cases
cochées sur 229.** Les sept restantes sont toutes en phase 9, et elles
sont nommées juste en dessous. Il ne reste donc **plus de case à ouvrir**
— seulement des limites à lever, listées phase par phase plus bas.

**Prochaine étape : 16.1, le moteur JavaScript** ; la phase 15 et
l'étape 16.0 (shell, terminal, recovery) sont faites (28 octobre 2026).
(L'ancien choix « un espace d'adressage par processus » ou « élargir le
sous-ensemble Rust » est derrière nous : les espaces privés sont faits
depuis 11.3, et le reste de la phase 9 forme la phase 16.)

### Suite du projet — plan réorganisé (28 octobre 2026)

Demande de l'utilisateur du 28 octobre 2026 : continuer en autonomie,
optimiser, compléter, et **réorganiser les phases pour ne pas modifier
le même code plusieurs fois**. Le travail restant a donc été regroupé par
zone de code, chaque morceau partagé étant écrit une fois, au premier
endroit où il sert :

| Ordre | Phase | Zones de code touchées | Ce qui y est écrit une fois pour toutes |
|---|---|---|---|
| 1 | 15 — Mises à jour atomiques | `json`, `crypto`, `binfmt/signature`, `web/http`, `fs` (moteur), `spm`, `apps` | lecteur JSON (resservira à `JSON.parse`), **Ed25519** (ferme la limite ENTERPRISE de la phase 3 et des `.kmd`), téléchargement HTTPS en flux (resservira à `fetch`), remplacement atomique d'une entrée de dossier |
| 2 | 16 — Langages et outils | `shell`, `web/js`, `sdk/*`, `apps/browser` | le shell et le terminal qui manquaient (§1.1, §2.2), moteur JavaScript compilé et complet (DOM du navigateur intégré **et** `dev interpret` pour Node/Bun), interprète Python, compilateur C, JVM minimale, débogueur |
| 3 | 17 — Reliquats FS et POSIX | `fs/lifs`, `fs/partition`, `posix`, `apps/explorer` | ce que 12 et 14 laissent ouvert hors Firefox |
| 4 | 18 — Firefox sur la pile Nova | le fork, `nova-gfx` | rien ne se fait sans la machine de l'auteur (construction du fork, GPU) |

Pourquoi cet ordre :
- La phase 15 est la seule entièrement spécifiée par l'utilisateur et
  non commencée ; ses préalables (JSON, HTTPS en flux, Ed25519) sont
  exactement ce dont la phase 16 a besoin pour `JSON.parse` et `fetch`.
  Les écrire en 15 évite de rouvrir `web/http` et `crypto` en 16.
- Le moteur JavaScript sert deux lignes du §13.2 et le navigateur
  intégré : le compléter une fois couvre les trois.
- Les reliquats de 14 touchent le moteur LIFS ; ceux qui portent sur le
  remplacement atomique d'une entrée passent en 15 (il en a besoin), les
  autres restent groupés en 17.

Versions : `0.{phase}.{correctif}`. Chaque étape validée d'une phase en
cours peut donner une version corrective (`0.14.1`, `0.14.2`…) ; la fin de
la phase 15 donne `0.15.0`. Voir `CHANGELOG.md`.

Décision de conservation, en attendant l'avis de l'utilisateur :
**`navigateur.cus` (le navigateur intégré) reste.** Son retrait
(11.10.2) avait été reporté ; Firefox demande une racine de 833 Mio
construite sur l'hôte et n'est donc dans aucune release, où le
navigateur intégré est le seul. Le compléter (JavaScript, phase 16) ne
préjuge pas de son retrait le jour où Firefox sera livrable.

### Travaux hors phase, à la demande (état au 22 septembre 2026)

Ces ajouts ne correspondent à aucune case de la roadmap. Ils sont écrits
et compilent sans avertissement, mais **aucun n'a encore de relevé série
consigné ici** : ils ne sont pas validés au sens de ce fichier.

- **Navigateur `navigateur.cus`** (`src/apps/browser/`, `src/web/`,
  `src/crypto/`). HTML, cascade CSS et mise en page CSS 2.1, HTTP/1.1 avec les méthodes GET, HEAD, POST, PUT, PATCH,
  DELETE et OPTIONS, redirections, cookies, cache, gzip/deflate. TLS 1.3
  et le sous-ensemble ECDHE + AEAD de TLS 1.2, chaînes X.509 vérifiées
  contre des ancres intégrées, magasin de certificats par compte
  (autorités ajoutées, méfiances, exceptions). Autotest au démarrage, sans
  réseau, sur sa propre ligne :
  `[test] navigateur valide - N vecteurs cryptographiques, N verifications web`.
- **JavaScript : inachevé, et hors de la compilation.** `src/web/js/`
  contient un lexeur, un parseur ES2022, un interprète d'arbre, les
  expressions régulières et une partie de la bibliothèque standard, mais
  `js/mod.rs` ne déclare que `big` et `num` : le reste n'est pas compilé.
  Manquent `builtins/{collections,date,global,json,number,promise,regexp}`
  et `dom`, puis le branchement sur `web::page`. **Aucune page n'exécute
  de script** ; le navigateur le dit lui-même sur `about:accueil`.
- **`devstudio.ss` retiré** (IDE de l'étape 9.4), ainsi que le morceau de
  démonstration que le lecteur audio installait.
  `apps::install_native` supprime `/sysapp/devstudio.ss` et
  `/users/_shared/audio/demo-nova.wav` sur un volume existant. Le
  programme de démonstration ring 3 et les tâches de démonstration des
  phases 2.6 et 2.7 **restent** : ce sont des supports d'autotest, pas des
  applications.
- **Débit réseau : objectif de 200 Mo/s atteint lors des mesures.** Pilote
  virtio-net (`drivers::virtio_net`) sondé avant l'e1000, derrière une
  façade `drivers::nic` ; carte par défaut du Makefile :
  `virtio-net-pci`. Boucle d'attente TCP corrigée (voir `DECISIONS.md`,
  « Réseau à haut débit »). Mesure reproductible : `make bench-net`.
  Relevés de l'assistant, 22 septembre 2026, `-smp 4`, 512 Mio servis par
  l'hôte, mode `user` de QEMU, émulation TCG comme `make run` :

  ```
  [drv-reseau] virtio-net - MAC 52:54:00:12:34:56, files 256/256, BAR4 0xfe804000
  [debit] 536870912 octets en 1785 ms (1790 ms au compteur de tics) - 300.7 Mo/s (372835 trames recues, 0 perdues)
  [debit] 536870912 octets en 2089 ms (2090 ms au compteur de tics) - 256.9 Mo/s (372834 trames recues, 0 perdues)
  [debit] 536870912 octets en 1698 ms (1690 ms au compteur de tics) - 316.1 Mo/s (372837 trames recues, 0 perdues)
  ```

  Sous KVM : 1 160 à 1 620 Mo/s ; e1000 sous KVM : 127 à 133 Mo/s ;
  monocœur sous TCG : 330 Mo/s sur 128 Mio. Dans ces exécutions, les neuf
  verdicts d'autotest sont restés verts. Sans carte (`-nic none`) :
  `[drv-reseau] aucune carte virtio-net sur le bus PCI ; aucune carte Intel e1000 sur le bus PCI`.

  **Non couvert** : le débit mesuré est celui de TCP en clair, lu par
  `tcp::recv`. Le navigateur y ajoute TLS (AES-GCM ou ChaCha20 logiciels,
  sans SSE) et la limite `MAX_BODY` de 4 Mio par réponse : le débit d'un
  téléchargement HTTPS dans le navigateur **n'a pas été mesuré**. Aucun
  essai sur Internet réel, seulement contre l'hôte. `make disk-reset` et
  `-vga none` n'ont pas été relancés pour ce changement.

Une pile **USB** a été ajoutée hors phase, à la demande : contrôleur xHCI
(`src/usb/xhci.rs`), descripteurs, et pointage HID relatif comme absolu.
Elle coexiste avec le PS/2 et alimente le même état de pointeur. Détail
dans `ARCHITECTURE.md`, section « Pile USB et pointage ».

Une passe d'optimisation transverse a suivi la phase 8 : tâches de
démonstration bornées, table des tâches bornée et stable en mémoire,
publication du framebuffer par rectangle, `pick_next` en un seul parcours.
La dérive du tas laissée ouverte en fin de phase 8 **est résolue** — elle
venait des tâches de démonstration de la phase 2, restées en boucle
infinie. Le relevé du moniteur est désormais constant d'un rapport à
l'autre. Détail dans `DECISIONS.md`, section « Optimisation et bugs de
fond ».

Sortie série de référence, `make run` (les huit verdicts, aucun échec) :

```
[test] phase 7.1 validee - 13 verifications du framebuffer
[test] phases 3 et 4 validees - 128 verifications conformes aux specs 3 a 5
[test] phase 8 validee - 224 verifications de SPM et des mises a jour (spec 10 et 11)
[test] phase 6 validee - 121 verifications de la couche OS
[test] phase 7 validee - 185 verifications du systeme graphique
[test] phase 5 validee - 262 verifications de l'API systeme (spec 6)
[test] phase 9 validee - 433 verifications du SDK (spec 13 et 15)
[test] phase 10 validee - 340 verifications des finitions (spec 12, 14 et 16)
```

Sous quatre cœurs (`make run-smp`), les mêmes huit verdicts, avec
262 → 265 pour la phase 5 : cet autotest compte des cas qui dépendent du
nombre de cœurs. Celui de la phase 6 varie lui aussi, mais pour deux
autres raisons, détaillées à la fin de « Comment valider la phase 10 ».

Toutes les cases sont cochées **sauf six**, toutes en phase 9 :

| Case | Ce qui manque |
|---|---|
| 9.1.3 `dev run --debug` | le bac à sable existe et se conserve ; il n'y a pas de débogueur |
| 9.5.2 à 9.5.6 | C/C++, C#, Java, Python, Node/Bun : la table du §13.2 est en données et la résolution refuse nommément, mais aucune toolchain ne tourne |

**9.5.1 est fermée depuis le 5 septembre 2026** : `sdk::rustc` compile un
sous-ensemble défini de Rust en x86_64 qui s'exécute en ring 3, et
interprète le même arbre. Deux lignes du §13.2 sur huit sont désormais des
chaînes complètes.

Le reste a été confronté à son propre critère. Les phases 4 à 7 avaient
déjà leurs cases cochées avant cette session ; ce qui a été refait ici,
c'est le relevé de leurs verdicts et, pour la phase 6, sa liste
d'observation en sept points. Le tableau visuel de la phase 7 — ce qu'on
voit dans la fenêtre QEMU — ne se confirme pas depuis une console série,
et il n'est pas compté comme vérifié par les exécutions ci-dessus.

### Ce que la phase 9 a ajouté

`src/sdk/` — onze modules, décrits dans `ARCHITECTURE.md` :

| Module | Couvre | Ce qui s'exécute vraiment |
|---|---|---|
| `runtime` | 9.5 | table §13.2 en données, résolution `/users/{u}/runtimes` > `/sys/runtimes` |
| `manifest` | 9.2 | parseur TOML restreint, validation croisée §3.4 / §12.4 / §13.2 |
| `asm` | 9.5.7 | assembleur x86_64 deux passes, code position-indépendant |
| `emit` | 9.1.2 | édition de liens (déplacements RIP) et émission d'un binaire Nova |
| `script` | 9.1.4 | interprète complet, borné en pas et en profondeur |
| `template` | 9.1.1 | les cinq gabarits du §15.5, tous assemblables |
| `sandbox` | 9.3 | arène mémoire dédiée, overlay FS, loopback, rejeu `.ras`, purge |
| `etr` | 9.6 | lexeur + AST JavaScript, table de correspondance, seuil 70 % |
| `libdev` | 9.4.3 | catalogue des abstractions, vérifié contre la table d'appels §6 |
| `cli` | 9.1 | les six verbes du §15.2 |
| `rustc` | 9.5.1 | Rust : lexeur, parseur, types, x86_64, interprète — les deux colonnes du §13.2 |
| `devstudio` | 9.4 | l'IDE, application `.ss` de la couche `Window` — **retiré depuis, à la demande** |

Deux points où la spécification laissait le choix, tranchés explicitement :

- **Six des huit langages du §13.2 exigent une toolchain externe** (LLVM,
  Mono, une JVM, V8). Aucune ne peut vivre dans un noyau sans dépendance.
  Deux sont réalisables en place, et le sont pour de vrai :
  **ASM x86_64 — « assemblage direct »** dès l'origine, et **Rust —
  « natif direct »** depuis l'étape 9.5.1. Dans les deux cas `dev build`
  produit du code qui s'exécute en ring 3. Les six autres sont marqués
  `Delegated` — le SDK les résout dans `/sys/runtimes/` et **refuse
  nommément** quand elles manquent. Aucun `dev build` ne rend un binaire
  de `nop` en annonçant une compilation.
- **`dev interpret main.py` suppose un interprète.** Une neuvième ligne,
  hors §13.2, a été ajoutée : Nova Script (`.ns`), écrit en entier dans
  `sdk::script`. C'est la même démarche que le drapeau `NETWORK` ajouté au
  format binaire en 5.6.4 — une extension assumée et signalée, pas un trou
  masqué.

### Bugs corrigés pendant la phase 9

- **Un dossier partagé ne partageait rien.** `fs::perm::Mode` n'avait pas
  de `shared_file` : tout fichier créé dans `/cache/`, `/ras/` ou
  `/users/_shared/` naissait avec les droits d'un fichier privé. Le
  dossier acceptait qu'on y écrive, et le fichier écrit devenait aussitôt
  intouchable pour tout autre compte. `Mode::shared_file` comble le trou.
- **La limite « les tâches DEAD ne libèrent ni leur pile ni leur image »
  était périmée** : `scheduler::try_reap` appelle `release_stack` puis
  `loader::release_for` et `domain_mem::release_for` depuis l'étape 5.3.3.
  Elle a été retirée de la liste ci-dessous.

### Ce que la phase 10 a fermé

Deux limites portées depuis longtemps ne sont plus ouvertes, et elles ne
le sont plus parce que la phase 10 les a fait apparaître pour de vrai :

- **L'invalidation de TLB entre cœurs** (ouverte par la phase 3) : un
  binaire étranger chargé dans un créneau d'arène recyclé et démarré sur
  un autre cœur exécutait le code de l'occupant précédent. Vecteur 241,
  compteur de génération, trois points de synchronisation.
- **Le bac à sable par chemin du §12.4** (ouverte par la phase 4) :
  `tree::sandbox_roots` a enfin un lecteur, `syscall::domain_fs::guard`.

### Limites connues, non résolues

Ces points sont assumés à ce stade et documentés pour ne pas donner une fausse
impression de complétude :

- ~~**Le code ring 3 n'est préemptible qu'aux frontières d'appel système.**~~
  **Levée en 11.3** : le minuteur préempte le ring 3 (`preempt_user`,
  DECISIONS « La préemption du ring 3 imposait `swapgs` »). Texte
  d'origine conservé :
  `syscall_dispatch` consomme `NEED_RESCHEDULE` à sa sortie, ce qui suffit à
  répartir le CPU entre plusieurs tâches utilisateur actives. Une boucle ring 3
  qui n'appelle jamais le noyau garde en revanche son cœur jusqu'à l'échéance de
  sa tranche. Corriger cela suppose de commuter directement depuis le handler
  d'interruption, en reconstruisant la trame de retour.
- **Pas de vol de tâches entre cœurs.** L'affinité §7.2 est une partition
  stricte : un cœur inoccupé ne peut pas prendre le travail d'un cœur surchargé
  d'une autre classe. Conforme à la spec, mais à réexaminer si un déséquilibre
  gênant apparaît.

### Limites ouvertes par la phase 3

- ~~**Un seul espace d'adressage.**~~ **Levée en 11.3** (espaces
  d'adressage privés, `vm::Space`, 20 vérifications à chaque démarrage).
  Texte d'origine : Toutes les tâches partagent la même hiérarchie
  de pages : l'arène du chargeur place les images dans des créneaux disjoints,
  mais rien n'empêche une tâche ring 3 de lire les pages d'une autre. L'isolation
  §12.4 reste donc *conventionnelle*. La séparation réelle suppose un CR3 par
  processus, avec le coût de commutation et le partage du noyau que cela
  implique — à traiter avant que du code tiers ne s'exécute pour de bon.
- **ENTERPRISE n'est pas une preuve d'authenticité.** La chaîne autorité →
  binaire est établie par un hachage dont l'entrée est publique : n'importe qui
  sachant lire le format peut fabriquer un certificat valide. Structure, magasin
  de racines et parcours de vérification sont en place ; il manque une signature
  asymétrique (Ed25519). SYSTEM et SELF, eux, tiennent leurs promesses.
  **Ne rien afficher à l'utilisateur qui présenterait ENTERPRISE comme vérifié**
  tant que ce point n'est pas traité.
- **La table d'empreintes SYSTEM est vide à la compilation.** C'est l'état
  correct tant qu'aucun binaire officiel n'est livré : une table pré-remplie
  donnerait l'illusion d'une vérification. Les entrées de l'autotest sont
  enregistrées à l'exécution. La génération réelle viendra avec `dev package`
  (étape 9.1.5).
- **La validité temporelle des certificats n'est pas contrôlée.** Les champs
  `valid_from` / `valid_until` sont décodés et affichés, jamais comparés :
  il n'y a pas encore d'horloge temps réel (`SYS_TIME_*`, étape 5.6.3).
- **La bascule avant/arrière-plan des `.gs` est un mécanisme sans politique.**
  `scheduler::set_foreground` fait chuter la tranche de 20 à 2 tics conformément
  à la spec §3.4, mais rien ne décide *quand* l'appeler : c'est le compositeur
  qui le fera, en phase 7.2.
- **Un programme ring 3 ne sait pas résoudre un service par son nom.** Il n'a
  que des PID, dont l'ordre dépend du nombre de cœurs et du nombre de binaires
  chargés. `SYS_IPC_OPEN` / `SYS_IPC_PUBLISH` (étape 5.5.2) lèveront la limite.
- **Le chargeur sérialise tout.** Un verrou unique protège l'arène *et* les
  créations de tables de pages, faute de verrou dans `memory::map_page`. La
  fréquence des chargements ne justifie rien de plus fin pour l'instant.

### Limites ouvertes par la phase 4

- **Pas de journal.** Le cache écrit en différé et se vide après chaque
  opération structurelle, ce qui réduit la fenêtre de perte à une seule
  opération — mais une coupure au mauvais moment laisse un volume dont le
  bitmap et la table des nœuds divergent. `fs::check_all` détecte l'écart,
  rien ne le répare. Un journal d'intentions est le remède, et il change
  la disposition sur disque : mieux vaut le poser avant que des volumes
  existent en dehors des tests.
- **Un fichier plafonne à 2068 Kio.** Cinq pointeurs directs plus un bloc
  d'indirection de 512 entrées, dans un nœud de 128 octets. Un second
  niveau d'indirection porterait la limite à 1 Gio ; il faudra le faire
  avant de stocker autre chose que de la configuration et des binaires.
- **Le chemin de copie inter-volumes passe par la mémoire.** Il n'y a
  qu'un verrou pour toute la table de montage, les deux volumes ne peuvent
  donc pas être ouverts en même temps. La taille copiable est plafonnée
  par celle d'un nœud, ce qui suffit tant que la limite ci-dessus tient.
- **Les trois formats étrangers sont écrits**, chacun sur son propre
  support amovible (`make check-export`). Ce qu'ils ne font pas :
  - **ext4 n'a ni journal ni sommes de contrôle.** Un volume sans journal
    est valide — c'est `mke2fs -O ^has_journal` — mais ne rejoue rien
    après une coupure. `metadata_csum` obligerait à un CRC32c sur chaque
    structure.
  - **Un inœud ext4 ne porte que quatre extents en ligne.** Un fichier
    qui demanderait cinq plages disjointes est refusé plutôt que tronqué.
    Sans conséquence tant qu'un fichier plafonne à 517 blocs.
  - **NTFS transforme chaque lien multiple en copie.** Le format sait les
    exprimer — plusieurs `$FILE_NAME` dans un enregistrement — mais il
    faudrait réécrire un enregistrement déjà émis. ext4, lui, les rend par
    de vrais liens durs.
  - **NTFS n'accorde qu'un descripteur de sécurité, ouvert à tous.**
    `$Secure` existe sans index `$SDH`/`$SII`. Traduire les permissions du
    §5 en ACL demanderait des identifiants de compte Windows.
  - **NTFS borne un dossier par ce que sa racine d'index peut décrire** :
    environ 440 octets d'entrées dans l'enregistrement MFT, soit quelques
    milliers d'entrées par dossier une fois l'arbre monté en profondeur.
    Au-delà, l'export refuse au lieu de tronquer.
  - **Deux noms qui ne diffèrent que par la casse** sont distincts pour
    Nova OS ; NTFS les confond à l'ordre de tri. Les deux sont exportés,
    un hôte Windows n'en verra qu'un.
  - **Aucun des trois n'est vérifié contre un pilote réel sous QEMU.** La
    preuve est celle des outils de l'hôte, ce qui couvre la structure mais
    pas le montage.
- **Le montage à chaud n'existe pas.** `SYS_FS_DISK_MOUNT` confirme qu'une
  lettre répond ; les supports sont détectés une fois, au démarrage.
  Détecter une insertion suppose les interruptions AHCI, donc la couche
  asynchrone des pilotes en espace utilisateur (phase 6.1).
- **Le pilote AHCI ne traite qu'une commande à la fois**, en scrutation,
  avec un tampon de rebond. AHCI en accepte trente-deux simultanées : s'en
  servir demande une file de requêtes et des interruptions.
- **Le journal d'audit ne survit pas au redémarrage.** Tampon circulaire de
  128 entrées en mémoire. Le persister vers `/logs/kernel/` suppose une
  file différée, le chemin étant réentrant (étape 5.2.3).
- **`Credentials` est fourni par l'appelant**, et non déduit du processus
  courant : le noyau n'associe pas encore un compte à un processus. C'est
  `SYS_USER_*` (étape 5.6.1) qui fermera ce point — d'ici là, un appelant
  malveillant *dans le noyau* pourrait se présenter comme ROOT. Sans
  conséquence tant que rien n'appelle le système de fichiers depuis le
  ring 3, ce qui reste le cas.
- **La validité temporelle n'est vérifiée nulle part** — dates de nœuds
  comme certificats — faute d'horloge temps réel (étape 5.6.3). Les
  horodatages valent le compteur de tics, et les dates FAT exportées sont
  une constante assumée.
- **Un lien de dossier ne se transporte pas.** Le §5.1 autorise `link`
  sur un dossier ; aucun des trois formats de destination n'a d'équivalent
  du lien dur de dossier. Les exporteurs écartent la seconde occurrence
  en la signalant, ce qui coupe aussi les cycles que ces liens rendent
  possibles.

---

## Phase 1 — Environnement & Boot ✅

### 1.1 Mise en place de la toolchain
- [x] 1.1.1 Installer Rust nightly via `rustup`
- [x] 1.1.2 Ajouter les composants `rust-src` et `llvm-tools-preview`
- [x] 1.1.3 Installer le target `x86_64-unknown-none`
- [x] 1.1.4 Installer `qemu-system-x86_64`, `grub`, `libisoburn`, `nasm`, `mtools`
- [x] 1.1.5 Vérifier chaque outil (`--version`) et corriger les manques

### 1.2 Structure du projet Rust no_std
- [x] 1.2.1 `cargo init --lib`
- [x] 1.2.2 Créer `rust-toolchain.toml`
- [x] 1.2.3 Créer `.cargo/config.toml`
  - [x] 1.2.3.1 Target par défaut `x86_64-nova_os.json`
  - [x] 1.2.3.2 `build-std = ["core", "compiler_builtins", "alloc"]`
  - [x] 1.2.3.3 `build-std-features = ["compiler-builtins-mem"]`
- [x] 1.2.4 Configurer `Cargo.toml`
  - [x] 1.2.4.1 `panic = "abort"` en profil dev et release
  - [x] 1.2.4.2 `#![no_std]` dans `lib.rs`, crate-type `staticlib`

### 1.3 Target custom x86_64
- [x] 1.3.1 Fichier de spécification JSON (`x86_64-nova_os.json`)
- [x] 1.3.2 Désactiver mmx et sse, activer soft-float
- [x] 1.3.3 Définir `linker-flavor` (gnu-lld) et `linker` (rust-lld)

### 1.4 Bootloader GRUB / Multiboot2
- [x] 1.4.1 Header Multiboot2 en NASM
- [x] 1.4.2 Script linker (`boot/linker.ld`)
- [x] 1.4.3 Point d'entrée ASM (`_start`)
  - [x] 1.4.3.1 Vérification du magic number Multiboot2
  - [x] 1.4.3.2 Pile temporaire
  - [x] 1.4.3.3 Saut vers `kernel_main`
- [x] 1.4.4 Arborescence ISO GRUB (`iso/boot/grub/grub.cfg`)
- [x] 1.4.5 Génération de l'ISO avec `grub-mkrescue`

### 1.5 Affichage minimal (VGA text mode)
- [x] 1.5.1 Writer VGA (buffer à `0xb8000`)
- [x] 1.5.2 `core::fmt::Write` + macros `print!`/`println!`
- [x] 1.5.3 Message de test « Nova OS booted »

### 1.6 Boot sous QEMU
- [x] 1.6.1 Lancement de l'ISO
- [x] 1.6.2 Makefile de build+run
- [x] 1.6.3 Support `-serial stdio` (UART 16550, COM1)
- [x] 1.6.4 Vérification de l'affichage VGA

### 1.7 Menu GRUB (spec §2.2)
- [x] 1.7.1 Entrée « démarrage normal »
- [x] 1.7.2 Entrée « Mode recovery »
- [x] 1.7.3 Entrée « Mode debug »
- [x] 1.7.4 Entrée « Version précédente » (placeholder)
- [x] 1.7.5 Parsing de la cmdline Multiboot2 (`mode=`, `verbose=`, `rollback=`)

---

## Phase 2 — Microkernel ✅

### 2.1 Structures de bas niveau CPU
- [x] 2.1.1 GDT
  - [x] 2.1.1.1 Segments code/data ring 0
  - [x] 2.1.1.2 Chargement (`lgdt`)
- [x] 2.1.2 IDT
  - [x] 2.1.2.1 32 exceptions CPU
  - [x] 2.1.2.2 Handler générique avec dump de contexte
  - [x] 2.1.2.3 Chargement (`lidt`)
- [x] 2.1.3 Double fault handler avec stack séparée (IST)

### 2.2 Pagination
- [x] 2.2.1 Memory map Multiboot2 (tag 6)
- [x] 2.2.2 Tables de pages 4 niveaux (PML4 → PDPT → PD → PT)
- [x] 2.2.3 Kernel en higher-half à `0xFFFFFFFF80000000`
- [x] 2.2.4 PAE + long mode + pagination
- [x] 2.2.5 Fenêtre mémoire physique à `0xFFFF800000000000` (4 GiB)

### 2.3 Allocateur mémoire
- [x] 2.3.1 Frame allocator bitmap
- [x] 2.3.2 Heap kernel (`#[global_allocator]`)
  - [x] 2.3.2.1 Stratégie free-list avec fusion des blocs adjacents
  - [x] 2.3.2.2 Région virtuelle dédiée à `0xFFFF808000000000` (4 MiB)
- [x] 2.3.3 Support `alloc` (Box, Vec, String, BTreeMap)

### 2.4 APIC / interruptions matérielles
- [x] 2.4.1 PIC 8259 remappé puis masqué
- [x] 2.4.2 Local APIC
- [x] 2.4.3 I/O APIC
- [x] 2.4.4 Timer APIC à 100 Hz, calibré contre le PIT
- [x] 2.4.5 IRQ clavier PS/2

### 2.5 Scheduler
- [x] 2.5.1 Structure `Process` (contexte, état, priorité, affinité)
- [x] 2.5.2 États READY / RUNNING / SLEEPING / SUSPENDED / DEAD (spec §7.4)
- [x] 2.5.3 Round-robin à l'intérieur d'un niveau de priorité
- [x] 2.5.4 Context switch en ASM (`boot/switch.asm`)
- [x] 2.5.5 Priorités fixes par type de binaire (spec §7.3)
- [x] 2.5.6 Anti-famine : seuil 500 ms (spec §7.5)
- [x] 2.5.7 Affinité CPU multi-cœurs (spec §7.2)

### 2.6 IPC
- [x] 2.6.1 Message passing, réception bloquante
- [x] 2.6.2 Canaux nommés (SYS_IPC_OPEN/CLOSE/PUBLISH/SUBSCRIBE)
- [x] 2.6.3 File de messages par processus

### 2.7 Support multi-cœur (SMP)
- [x] 2.7.1 Parsing ACPI/MADT (RSDP → RSDT/XSDT → MADT)
- [x] 2.7.2 Démarrage des AP via INIT-SIPI-SIPI + trampoline 16 bits
- [x] 2.7.3 Pile, GDT, TSS et LAPIC par cœur

### 2.8 Syscall gate
- [x] 2.8.1 `syscall`/`sysret` (EFER.SCE, STAR, LSTAR, FMASK, KERNEL_GS_BASE)
- [x] 2.8.2 Table de dispatch par numéro (spec §6.1)
- [x] 2.8.3 Séparation ring 3 / ring 0, validation des pointeurs utilisateur

### 2.9 Ordonnancement multi-cœurs
- [x] 2.9.1 `current` par cœur, table de tâches partagée
- [x] 2.9.2 Filtrage par affinité à l'élection (spec §7.2)
- [x] 2.9.3 Timer LAPIC armé par cœur, calibration partagée
- [x] 2.9.4 IPI de réordonnancement (vecteur 240)
- [x] 2.9.5 Tâche de veille épinglée par cœur
- [x] 2.9.6 Identification du cœur courant via table LAPIC → index

---

## Phase 3 — Formats binaires ✅

### 3.1 Parsing des headers (spec §3) ✅
- [x] 3.1.1 Struct de header commun (256 octets, lecture champ par champ)
- [x] 3.1.2 Reconnaissance des 5 magic numbers (spec §3.3)
- [x] 3.1.3 Parsing `AppIdentity` (app_id, author, group, version, binary_type)
- [x] 3.1.4 Validation des règles §3.4 et §12.3/§12.4 à l'analyse
- [x] 3.1.5 Générateur de binaires de test + autotest (15 cas)

### 3.2 Loader de binaires ✅
- [x] 3.2.1 Chargement en mémoire (mapping des sections)
- [x] 3.2.2 Relocation si nécessaire
- [x] 3.2.3 Transfert du contrôle au point d'entrée de l'app
- [x] 3.2.4 Application des règles par type (spec §3.4)
  - [x] 3.2.4.1 `.dsa` : signature SYSTEM obligatoire, accès kernel space
  - [x] 3.2.4.2 `.ss` : signature SYSTEM ou ENTERPRISE
  - [x] 3.2.4.3 `.gs` : bascule avant/arrière-plan (CPU réduit à 2 %)
  - [x] 3.2.4.4 `.ras` : avertissement obligatoire, jamais app par défaut

### 3.3 Système de signatures (spec §3.2)
- [x] 3.3.1 Implémentation SHA-256
- [x] 3.3.2 Vérification SYSTEM (hash hardcodé, vérifié kernel)
- [x] 3.3.3 Vérification ENTERPRISE (certificat tiers)
- [x] 3.3.4 Vérification SELF (auto-signé + avertissement install)
- [x] 3.3.5 Rejet des combinaisons interdites (déjà couvert par 3.1.4, à relier)

### 3.4 Intégration scheduler
- [x] 3.4.1 Attribution de la priorité fixe selon le type au chargement
- [x] 3.4.2 Attribution du cœur CPU selon affinité (spec §7.2)

---

## Phase 4 — Système de fichiers 🔄

### 4.1 Node table & liens virtuels (spec §4.1)
- [x] 4.1.1 Structure de nœud fichier (inode-like) indépendante du chemin
- [x] 4.1.2 Table de liens dossier → nœud
- [x] 4.1.3 Déplacement = modification de lien uniquement (pas de copie)
- [x] 4.1.4 Copie réelle seulement inter-disques

### 4.2 Gestion des disques (spec §4.2)
- [x] 4.2.1 Détection des disques au boot
- [x] 4.2.2 Attribution des lettres (`/` par défaut, `A:/`, `B:/`...)
- [x] 4.2.3 SYS_FS_DISK_LIST / DISK_MOUNT

### 4.3 VFS (Virtual File System)
- [x] 4.3.1 Couche d'abstraction driver de stockage (AHCI/NVMe minimal)
- [x] 4.3.2 Format disque natif Nova OS (structure sur disque pour la node table)
- [x] 4.3.3 Cache lecture/écriture

### 4.4 Arborescence complète (spec §4.3 / §17)
- [x] 4.4.1 Création automatique de l'arborescence système au premier boot
- [x] 4.4.2 Création de l'arborescence utilisateur (`/users/{user}/...`)

### 4.5 Permissions (spec §5)
- [x] 4.5.1 Implémenter les 13 permissions
- [x] 4.5.2 Règle double-niveau execute (fichier ET dossier)
- [x] 4.5.3 Rôles utilisateurs (ROOT/ADMIN/USER/GUEST/SERVICE — spec §5.2)
- [x] 4.5.4 Logs d'audit (accessible ADMIN uniquement)

### 4.6 Multi-compatibilité (spec §4.4)
- [x] 4.6.1 Export FAT32
- [x] 4.6.2 Export ext4 — *écrit et vérifié par `fsck.ext4 -fn` ; à confirmer*
- [x] 4.6.3 Export NTFS — *écrit et relu par un lecteur indépendant ; à confirmer*
- [x] 4.6.4 Option d'activation à l'installation

---

## Phase 5 — API système 🔄

### 5.1 Infrastructure syscall générique ⬅️ **ÉTAPE COURANTE**
- [x] 5.1.1 Type `SysResult<T>` (success, data, SysError) — *écrit*
- [x] 5.1.2 Type `SysError` structuré (code, name, message, context) — *écrit*
- [x] 5.1.3 Table de dispatch par domaine (0x01xx à 0x08xx) — *écrit*
- [x] 5.1.4 Distinction Sync/Async dans le traitement — *écrit*

### 5.2 Domaine SYS_FS_* (0x01xx — spec §6.2)
- [x] 5.2.1 READ/WRITE/OVERWRITE/DELETE/RENAME — *écrit*
- [x] 5.2.2 MOVE/COPY/LINK — *écrit*
- [x] 5.2.3 STAT/CHMOD/CHOWN/AUDIT — *écrit*
- [x] 5.2.4 DIR_ACCESS/DIR_CREATE — *écrit*
- [x] 5.2.5 DISK_LIST/DISK_MOUNT/DISK_EXPORT — *écrit*

### 5.3 Domaine SYS_PROC_* (0x02xx)
- [x] 5.3.1 SPAWN/KILL/SUSPEND/RESUME — *écrit*
- [x] 5.3.2 STAT/LIST/SETPRIO — *écrit ; `SETPRIO` applique la table du §7.3*
- [x] 5.3.3 WAIT/SELF — *écrit ; ferme la limite « les tâches DEAD ne libèrent rien »*

### 5.4 Domaine SYS_MEM_* (0x03xx)
- [x] 5.4.1 ALLOC/FREE/RESIZE — *écrit*
- [x] 5.4.2 SHARE (mémoire partagée via IPC) — *écrit, portée limitée par l'espace d'adressage unique*
- [x] 5.4.3 PROTECT (droits R/W/X) — *écrit, W^X appliqué*
- [x] 5.4.4 LOCK (anti-swap) — *écrit, sans effet faute de swap*

### 5.5 Domaine SYS_IPC_* (0x04xx)
- [x] 5.5.1 SEND/RECV — *écrit, réception bloquante comprise*
- [x] 5.5.2 OPEN/CLOSE (canaux nommés) — *écrit ; ferme la limite « pas de résolution par nom »*
- [x] 5.5.3 SUBSCRIBE/PUBLISH (event bus système) — *écrit*
- [x] 5.5.4 QUERY/RESPOND (API exposées par les .dsa) — *écrit*

### 5.6 Domaines complémentaires (spec §6.6)
- [x] 5.6.1 SYS_USER_* (0x05xx) — *écrit ; ferme la limite « Credentials fourni par l'appelant »*
- [x] 5.6.2 SYS_HW_* (0x06xx) — *écrit, accès réservé aux .dsa/.ss/.ras*
- [x] 5.6.3 SYS_TIME_* (0x07xx) — *écrit ; horloge CMOS ajoutée, ferme la limite « pas d'horloge temps réel »*
- [x] 5.6.4 SYS_NET_* (0x08xx) — *écrit ; la déclaration d'en-tête est appliquée, la pile réseau reste à faire*

### Ce que `SETPRIO` recouvre (§6.3, §7.1, §7.3)

Le §6.3 liste « Modifier la priorité », le §7.1 déclare la priorité fixe.
La contradiction n'est qu'apparente : le §7.1 pose lui-même son exception
— « sauf pour les `.gs` qui basculent automatiquement entre avant et
arrière-plan » — et le §7.3 donne la tranche en **deux colonnes** par
format. Il y a donc deux grandeurs, et `SETPRIO` agit sur la seconde :

| Type | Tranche arr./av. | Note du §7.3 | `SETPRIO` |
|---|---|---|---|
| `.dsa` | 40 / 40 | « Jamais réduit » | refusé |
| `.ss` | 15 / 25 | — | accepté |
| `.gs` | 2 / 20 | « Bascule automatiquement » | accepté |
| `.cus` | 4 / 10 | — | accepté |
| `.ras` | 1 / 5 | « Jamais modifiable » | refusé |

Les deux refus sont écrits dans la table du §7.3, pas déduits. La classe
de priorité, elle, n'est jamais modifiable : toute valeur autre que
« avant-plan » ou « arrière-plan » est rejetée en nommant la classe fixe
de la tâche visée.

**Descendre est permis à tous, remonter est réservé au système** — `.dsa`,
`.ss` (donc le compositeur du §8.1), ROOT et ADMIN. Sans cette
dissymétrie, le « CPU réduit à 2 % » du §3.4 ne serait qu'une suggestion
qu'une application refuserait d'un appel. C'est par cet appel que le
compositeur exercera la bascule en phase 7.2 : le mécanisme existait, la
politique manquait.

### Limites ouvertes par la phase 5

- **Le drapeau `NETWORK` est une extension du format.** Le §6.6 conditionne
  l'accès réseau à une « déclaration header » sans dire laquelle ; ce bit
  a été ajouté aux drapeaux du §3.4 pour rendre la règle applicable. Un
  binaire antérieur ne le pose pas et se voit refuser le domaine, ce qui
  est le comportement attendu.
- **`SYS_MEM_SHARE` n'ouvre aucun accès.** Toutes les tâches partagent une
  seule hiérarchie de pages : la région est déjà lisible par les autres.
  L'appel enregistre l'autorisation et notifie le destinataire — le
  protocole que les deux parties suivront quand un CR3 par processus
  rendra le partage réel.
- **`SYS_MEM_LOCK` n'empêche rien**, faute de swap. L'intention est
  enregistrée sur la région pour que le futur mécanisme de décharge la
  respecte.
- **Le mode `Async` du domaine 0x01 est une promesse d'interface.** Le
  pilote AHCI travaille en scrutation : `READ`, `WRITE` et `COPY` ne
  rendent la main qu'une fois l'entrée-sortie finie. La promesse sera
  tenue quand le stockage passera en espace utilisateur (phase 6.1).
  Seuls `SYS_PROC_WAIT`, `SYS_IPC_RECV` bloquant et `SYS_TIME_SLEEP_MS`
  endorment réellement leur appelant.
- **Aucune pile réseau.** Les appels de transport du domaine 0x08 existent
  et refusent en `UNSUPPORTED` ; `STATUS` et `IFACE_LIST` énumèrent
  réellement les cartes vues par le parcours PCI. La pile est la phase 6.2.
- **Les comptes n'ont pas de mot de passe.** `SYS_USER_SETID` ne sait que
  descendre en privilège, ce qui est le seul changement d'identité
  légitime sans preuve. Une ouverture de session suppose un secret stocké
  et un écran de connexion, donc la couche OS.
- **Aucune tâche n'est réclamée automatiquement.** `SYS_PROC_WAIT` rend
  pile et image, mais une tâche que personne n'attend reste un tombeau.
  Un `init.dsa` qui adopte les orphelins (phase 6.5.1) fermera ce point.
- **Un tombeau n'est jamais retiré de la table.** L'en retirer décalerait
  les indices que chaque cœur détient sur sa tâche courante. Le coût est
  d'une structure `Process` par tâche morte, pile et image exclues.
- **L'horloge murale ne se resynchronise pas.** Elle est lue une fois au
  démarrage, et le temps courant s'en déduit par le compteur de tics :
  la dérive est celle du timer APIC, quelques secondes par jour.
- **Le §12.2 n'est pas appliqué.** Une communication entre applications de
  groupes différents devrait demander l'accord de l'utilisateur ; il n'y a
  ni interface pour poser la question ni compositeur pour l'afficher
  (étape 6.4.2).

---

## Phase 6 — Couche OS 🔄

### 6.1 Drivers user space
- [x] 6.1.1 Driver clavier (migration depuis le kernel) — *écrit : `drivers/keyboard.rs`, canal `dev.keyboard`, le handler d'IRQ ne décode plus rien*
- [x] 6.1.2 Driver souris — *écrit : `drivers/mouse.rs`, PS/2 trois boutons, IRQ 12, canal `dev.mouse`*
- [x] 6.1.3 Driver stockage — *écrit : service `dev.storage` (énumération, lecture brute, vidage) ; AHCI reste en scrutation, NVMe détecté non piloté*
- [x] 6.1.4 Driver réseau (e1000 sous QEMU) — *écrit : `drivers/e1000.rs`, anneaux DMA RX/TX, scrutation*
- [x] 6.1.5 Driver audio (base) — *écrit : `drivers/audio.rs`, AC'97, liste de descripteurs ; **aucune sortie sonore vérifiée***
- [x] 6.1.6 Framework driver générique — *écrit : `drivers/mod.rs` (registre, cycle de vie) et `drivers/irq.rs` (démultiplexage IRQ → tâche)*

### 6.2 Réseau
- [x] 6.2.1 Pile TCP/IP — *écrit : Ethernet, ARP, IPv4, ICMP echo, UDP, TCP client (`src/net/`)*
- [x] 6.2.2 Configuration réseau (`/net/`) — *écrit : `/net/interface.cfg`, statique prioritaire sur DHCP*
- [x] 6.2.3 DNS, DHCP basique — *écrit : DISCOVER/REQUEST complet, requête A avec décompression de noms*

### 6.3 Audio
- [x] 6.3.1 Abstraction périphérique audio — *écrit : `src/audio/`, 48 kHz stéréo 16 bits, arithmétique entière*
- [x] 6.3.2 Mixage simple multi-app — *écrit : 8 flux, volume par flux et général, saturation contrôlée*

### 6.4 IPC applicatif étendu
- [x] 6.4.1 Ports internes applicatifs (spec §12.2) — *écrit : `src/ports.rs` + `src/apps.rs` ; le §12.1 est **appliqué** dans `ipc::send`*
- [x] 6.4.2 Prompt utilisateur accepter/refuser — *écrit : `src/prompt.rs`, choix mémorisés dans `settings/permissions/`, refus par défaut à l'expiration*

### 6.5 Services de boot (spec §2.1)
- [x] 6.5.1 `init.dsa` — *écrit : orchestrateur + adoption des orphelines*
- [x] 6.5.2 `fs.dsa`, `ipc.dsa`, `hw.dsa` — *écrit : trois services sur `sys.fs`, `sys.ipc`, `sys.hw`*
- [x] 6.5.3 Timeouts et codes d'erreur boot (spec §2.5) — *écrit : les sept `BOOT_ERR_*`, échéance de 30 s sur les services core*
- [x] 6.5.4 Lancement des services secondaires — *écrit : pilotes puis configuration réseau, échec non bloquant*

### Comment valider la phase 6

```bash
make run-nogui      # monoprocesseur
make run-smp        # quatre cœurs — obligatoire
make run-netdump    # enregistre les trames dans build/net.pcap
make run-nonet      # sans carte réseau ni audio
```

Ce qu'il faut voir dans la sortie série :

1. `[drv] 5 pilotes declares` puis, pour chacun, une ligne `pret` ou
   `absent` — **aucun ne doit rester en `demarrage`** ;
2. `[drv-clavier] pret`, `[drv-souris] prete`, `[drv-reseau] Intel
   82540EM — MAC ...`, `[drv-audio] AC'97 — mixeur 0x...` ;
3. `[init] 3 services core en ligne`, **sans** `BOOT_ERR_` ;
4. `[net] 10.0.2.15 par DHCP — passerelle 10.0.2.2, DNS 10.0.2.3` — c'est
   la preuve que les anneaux DMA, ARP, IPv4 et UDP fonctionnent
   ensemble ;
5. `[net] configuration ecrite dans /net/interface.cfg` ;
6. `[test] phase 6 validee — N verifications de la couche OS` ;
7. les invariants permanents : compteurs de tics par cœur qui progressent,
   `{octets alloués, octets libres}` identiques d'un rapport du moniteur
   au suivant, frames libres stables.

Au **second** démarrage, `/net/interface.cfg` existe déjà mais porte
`source=dhcp` : la configuration doit être renégociée, pas relue. Une
adresse relue serait un bogue — le bail a pu expirer.

Sous `make run-nonet`, les pilotes réseau et audio doivent se déclarer
`absent` et le démarrage se poursuivre jusqu'à l'autotest.

### Limites ouvertes par la phase 6

- **Les tâches pilotes s'exécutent en ring 0.** Fabriquer de vrais
  binaires ring 3 suppose le SDK (9.1) ; rien aujourd'hui ne compile un
  `.dsa` autonome. Ce qui est réel, c'est la **discipline d'interface** :
  un pilote est une tâche ordonnancée, reçoit ses IRQ par un
  démultiplexeur qui ne fait que compter et réveiller, ne s'expose que par
  un canal IPC nommé, et dort par `SYS_TIME_SLEEP_MS`. La bascule en
  ring 3 ne changera aucune de ces interfaces. Un pilote fautif peut en
  revanche encore corrompre le noyau.
- **Aucune ligne PCI n'est routée vers un vecteur.** Elles sont à
  déclenchement par niveau et actives à l'état bas : le relais générique
  du noyau ne lit aucun registre de périphérique — c'est ce qui lui permet
  d'ignorer ce qu'il relaie — et laisserait la ligne assertée, donc une
  tempête d'interruptions. Le remède est de masquer la ligne dans l'I/O
  APIC depuis le relais et de la rouvrir depuis la tâche. En attendant,
  la carte réseau et l'audio **scrutent** toutes les 10 ms. Le clavier
  (IRQ 1) et la souris (IRQ 12) sont ISA, donc à front, et sont bien
  pilotés par interruption.
- **Le chemin de données du système de fichiers ne passe pas par le
  service de stockage.** Un bloc fait 4 Kio, une charge utile IPC 256
  octets, et la mémoire partagée n'ouvre encore aucun accès faute d'un
  CR3 par processus. Le service rend l'énumération, l'état et une lecture
  brute bornée ; le cache continue d'appeler AHCI directement.
- **AHCI reste en scrutation, une commande à la fois**, et **NVMe est
  détecté sans être piloté**. Les deux demandent une file de requêtes et
  des interruptions, donc le point précédent.
- **TCP émet un segment à la fois.** Pas de fenêtre glissante, pas de
  contrôle de congestion, pas d'estimation du temps d'aller-retour. Les
  segments hors séquence sont écartés plutôt que réassemblés — l'autre
  extrémité retransmet, c'est le contrat. Correct mais lent.
- **Pas d'ouverture passive TCP** : `SYS_NET_CONNECT` est un appel de
  client. Écouter demanderait un carnet de connexions à demi ouvertes.
- **Pas de réassemblage de fragments IP**, pas d'IPv6, une seule
  interface configurée.
- **Le numéro de séquence initial TCP n'est pas imprévisible.** Il mélange
  le compteur de tics et les ports ; la RFC 6528 veut une source
  cryptographique, qui n'existe pas encore. Ne pas présenter les
  connexions comme protégées contre l'injection par un tiers sur le même
  lien.
- **Le bail DHCP n'est pas renouvelé.** Sa durée est enregistrée et
  affichée ; atteindre la moitié du bail devrait déclencher un REQUEST.
- **Aucune sortie sonore n'a été confirmée.** La séquence AC'97 est écrite
  d'après la spécification et ses registres se relisent — l'index de
  tampon courant avance —, mais personne n'a entendu ce pilote.
- **Pas de rééchantillonnage audio** : un flux doit être produit à 48 kHz.
- **Aucun redémarrage automatique d'un service mort.** Les `BOOT_ERR_*`
  du §2.5 sont émis, rien ne relance : distinguer une panne transitoire
  d'une panne franche demande une politique.
- **Le cache ARP n'expire jamais.** La table est bornée et remplace la
  plus ancienne entrée ; un changement de carte sur la même adresse IP
  resterait invisible jusqu'au débordement.
- **L'invite d'autorisation s'affiche sur l'écran texte**, faute de
  compositeur (§8.1, phase 7.2). Le mécanisme, les choix mémorisés et la
  révocation sont en place ; c'est la fenêtre qui manque.
- **Aucune limite de débit par lien de port interne.** Un client accepté
  peut saturer la boîte du serveur ; la file d'IPC le protège en
  refusant, mais rien ne pénalise l'émetteur.

---

## Phase 7 — Système graphique

### 7.1 Framebuffer
- [x] 7.1.1 Passage du VGA text mode au mode graphique (GOP via Multiboot2 ou VBE)
- [x] 7.1.2 Driver framebuffer bas niveau (pixels, double buffer)

> Écrit et éprouvé sous QEMU, **cases laissées décochées** en attente de la
> confirmation de l'utilisateur. Voir « Comment valider la phase 7.1 »
> ci-dessous.

### Comment valider la phase 7.1

`make run` — puis, sur la console série, quatre lignes doivent apparaître
**avant** `[pic]` :

```
[gfx] mode 1024x768 en 32 bits (RGB direct) - pas 4096 o, 3072 Kio a 0xfd000000
[gfx] composantes : R 16+8  V 8+8  B 0+8
[gfx] console 127x47 cellules de 8x16 - double tampon : oui
[gfx] PAT 0x0007040100070406 (defaut 0x0007040600070406) - write-combining : oui
[gfx] --- phase 7.1 : 13 verifications, 0 echecs ---
[test] phase 7.1 validee - 13 verifications du framebuffer
```

Le PAT doit différer du défaut : c'est la preuve que le write-combining est
armé. `double tampon : oui` est la preuve que les cadres ont été alloués.

**Dans la fenêtre QEMU** : le texte occupe tout l'écran en 1024×768, pas les
80×25 du mode texte, et les couleurs suivent la palette habituelle — vert pour
les succès, cyan pour l'APIC, magenta pour le ring 3, rouge sur blanc pour
l'avertissement `.ras`. **Aucun `■` ne doit apparaître** : un carré plein
signale un caractère non-ASCII dans une chaîne affichée.

Trois autres configurations doivent démarrer sans échec :

| Commande | Ce qu'elle éprouve |
|---|---|
| `make run-smp` | le PAT programmé sur les quatre cœurs |
| `qemu-system-x86_64 -cdrom build/nova-os.iso -m 512M -vga cirrus …` | le chemin 24 bits, trois octets par pixel |
| `… -vga none` | le repli en mode texte VGA, sans plantage |

### Limites ouvertes par la phase 7.1

- **La console graphique n'est pas le rendu de texte du système.** Le §8.1
  réserve l'écriture sur le framebuffer au compositeur. Celle-ci est la
  console de diagnostic du noyau, avec la même palette de seize couleurs
  et les mêmes limites que le writer VGA — pas d'antialiasing, pas de
  police proportionnelle, pas de curseur affiché.
- **Chaque ligne qui fait défiler l'écran republie l'image entière.**
  Trois mébioctets, soit 45 Mo/s au rythme du journal de démarrage.
  Négligeable sur du vrai matériel, mais 2× le coût du reste sous QEMU.
  La publication par rectangles sales viendra avec le compositeur (7.2).
- **Le verrou de l'écran masque les interruptions pendant un `print!`.**
  Un défilement tient donc le cœur environ deux millisecondes, exactement
  comme le faisait le writer VGA — mais sur une surface cent fois plus
  grande. À reprendre si une échéance temps réel apparaît.
- **Le mode indexé et le mode texte EGA ne sont pas exploités.** Le premier
  demanderait de programmer une palette DAC, le second n'est pas un
  framebuffer. Les deux retombent proprement sur le writer VGA.
- **Aucun changement de mode à chaud.** La résolution est celle que GRUB a
  programmée au démarrage. En changer suppose de repasser par le BIOS en
  mode réel, ou un pilote de carte graphique — ni l'un ni l'autre n'est au
  programme de la phase 7.
- **Le framebuffer au-delà de 4 Gio n'allume l'écran qu'à la seconde
  étape.** Les lignes de démarrage qui précèdent l'allocateur ne sortent
  alors que sur la console série. Jamais rencontré sous QEMU ; la
  situation est traitée, pas éprouvée.
- **La police ne couvre que 0x20–0x7E.** Tout le reste devient un carré
  plein. L'i18n du §14 demandera bien davantage, et une police bitmap 8×16
  n'est pas le bon véhicule pour ça.

### 7.2 Compositeur (.ss — spec §8.1)
- [x] 7.2.1 `compositor_core.mod` (non remplaçable)
- [x] 7.2.2 Boucle de rendu + réception des instructions IPC
- [x] 7.2.3 Gestion du z-order et des surfaces

### 7.3 Système de modules .mod (spec §8.2-8.3)
- [x] 7.3.1 Module Loader (détection automatique dans `/sys/gui/modules/`)
- [x] 7.3.2 Validation de signature des `.mod`
- [x] 7.3.3 Isolation mémoire par module
- [x] 7.3.4 Mécanisme pause (`.mod` → `.pause`)
- [x] 7.3.5 Modules natifs : taskbar, winmanager, notifs, wallpaper, launcher, cursor

### 7.4 Protocole IPC GUI
- [x] 7.4.1 Messages de dessin (rect, texte, image...)
- [x] 7.4.2 Événements input vers les apps focalisées

### 7.5 Customisation (spec §8.4)
- [x] 7.5.1 Priorité modules utilisateur > modules système
- [x] 7.5.2 `theme.cfg` (couleurs, polices, coins arrondis, dimensions)

### 7.6 Notifications (spec §9)
- [x] 7.6.1 5 niveaux de priorité (PRIORITAIRE à BASSE)
- [x] 7.6.2 Conditions de disparition (timeout, action, IPC, `Any[]`)
- [x] 7.6.3 Permissions par type de binaire (tableau §9.3)

### 7.7 Écran de connexion & splash (spec §2.3-2.4)
- [x] 7.7.1 `login.ss` — sélection utilisateur + mot de passe
- [x] 7.7.2 Anti-bruteforce (3→30 s, 5→5 min, 10→verrouillage)
- [x] 7.7.3 Splash screen modulaire (`.mod`, overridable par user)
- [x] 7.7.4 Sélection du compte et focus du champ **au clic** — *`login::Layout`
  est la source unique de la géométrie, lue par le dessin et par le test de
  clic ; 12 vérifications de plus dans l'autotest (140 → 152)*
- [x] 7.7.5 Flèche de validation à droite du champ — *même `submit()` que la
  touche Entrée, donc même barème 7.7.2 ; dessinée au `rect` faute de glyphe
  de flèche dans la police ASCII ; 5 vérifications de plus (152 → 157)*
- [x] 7.7.6 Curseur par forme et effet de survol — *`OP_CURSOR_SHAPE` + trois
  formes dans `cursor.mod`, teinte de survol sur les quatre sortes d'éléments
  du panneau ; 11 vérifications de plus (157 → 168)*

### 7.8 Survol et menu sur le bureau

- [x] 7.8.1 Socle de survol commun aux modules — *`modules::Hover`,
  `modules::local`, `modules::want_cursor`, et `Canvas` porte enfin l'origine
  de sa surface ; 8 vérifications de plus (168 → 176)*
- [x] 7.8.2 `taskbar.mod` : survol du bouton Nova OS et des boutons de fenêtre,
  curseur **main**, clic qui bascule le menu et clic qui ramène une fenêtre au
  premier plan
- [x] 7.8.3 `launcher.mod` : survol des lignes, curseur **flèche** parce que
  rien n'y répond au clic, et réponse à `EV_MENU_TOGGLE`
- [x] 7.8.4 La forme du curseur est **poussée**, plus en attente du mouvement
  suivant — *le retard d'un événement documenté par la 7.7.6 est levé*
- [x] 7.8.5 Latence du clic sur les modules — *un module traitait **un** événement
  par réveil, donc un par 100 ms. Mesuré : `1336 ms` entre l'appui sur « Nova OS »
  et l'ouverture du menu, **41 ms** après correction*
- [x] 7.8.6 La croix de fermeture d'une fenêtre agit — *dessinée depuis la phase 7
  et testée nulle part ; `EV_CLOSE` n'était émis par aucune ligne du système*

### 7.9 `notepad.cus` — bloc-notes (spec §3.1)

- [x] 7.9.1 `src/textbuf.rs` — tampon de texte éprouvé, 20 vérifications
- [x] 7.9.2 `apps::notepad` — fenêtre `.cus`, barre d'outils, zone de texte, barre d'état
- [x] 7.9.3 Écrire, Entrée, Retour arrière, Suppr, flèches, Début, Fin
- [x] 7.9.4 **Caret placé au clic**, arrondi au plus proche intervalle entre glyphes
- [x] 7.9.5 Ctrl+S enregistrer, Ctrl+O ouvrir, Ctrl+N nouveau, Ctrl+W fermer
- [x] 7.9.6 Demande de chemin avec **liste du dossier**, survol et clic pour compléter
- [x] 7.9.7 Déplacement par la barre de titre, fermeture par la croix
- [x] 7.9.8 `apps::NATIVE_APPS` + lancement depuis une ligne du menu

> **Le lanceur lance enfin.** Son commentaire annonçait depuis le premier jour
> que « le lancement se fait avec `SYS_PROC_SPAWN` » et aucune ligne du fichier
> ne l'appelait. Ses lignes gagnent donc la main, ce qui lève la réserve posée
> par la 7.8.3.
>
> **Ce qui démarre est le code natif de `apps::NATIVE_APPS`, pas les octets du
> fichier de `/apps`.** Le binaire est réel — en-tête, signature et permissions
> vérifiés par le §3.3 — mais l'exécuter dans son propre espace d'adressage
> demande un CR3 par processus, limite ouverte par la phase 3. Une ligne dont le
> nom n'est pas au catalogue l'affiche sur sa ligne, en rouge, plutôt que de ne
> rien faire.

### Comment valider le bloc-notes

`make run`, se connecter, cliquer « Nova OS », puis la ligne `notepad.cus`.
Relevé par injection QMP, du lancement à la fermeture :

```
[gui] applications installees : 1 ecrite(s), 1 au catalogue
[apps] lancement de "notepad.cus" (os.nova.notepad) - PID 60
[notepad] surface 13 - 620x420 en (202, 174) - dossier par defaut /users/admin/home/documents
[notepad] enregistre "/users/admin/home/documents/notes" - 2 ligne(s), 12 caractere(s)
[notepad] ouvert "/users/admin/home/documents/notes"
[notepad] ferme - surface rendue
```

Les **12 caractères** sont la preuve du 7.9.4 : `bonjour` puis `deux` font onze
caractères, et le `x` inséré après un clic en colonne 3 donne `bonxjour` — le
caret était exactement où le clic l'avait mis, pas en fin de ligne.

Au second démarrage, `0 ecrite(s), 1 au catalogue` : l'installation **retrouve**
au lieu de recréer, ce que le §17 demande.

> Le clic sur « Nova OS » **corrige une promesse non tenue** : le commentaire de
> `launcher.mod` annonçait depuis le début qu'il « s'ouvre sur un clic sur le
> bouton de la barre des tâches ou sur la touche Super », et le clic n'a jamais
> existé. Relevé sur la série : `[mod-taskbar] clic : bascule du menu demandee`
> puis `[mod-launcher] menu ouvert`, et la touche Super passe désormais par le
> **même** chemin, donc le bouton affiche toujours le bon état.

### Où le survol existe, et où il n'existe pas

| Composant | Survol | Curseur | Pourquoi |
|---|---|---|---|
| `login.ss` | oui, 4 sortes d'éléments | main / barre en I / flèche | 7.7.6 |
| `taskbar.mod` — bouton Nova OS | oui | main | il bascule le menu |
| `taskbar.mod` — boutons de fenêtre | oui | main | ils ramènent la fenêtre au premier plan |
| `taskbar.mod` — horloge, badge | **non** | flèche | ils ne répondent à aucun clic |
| `launcher.mod` — lignes | oui | **main** | une ligne lance l'application (7.9.8) |
| `notepad.cus` — zone de texte | — | **barre en I** | on y place le caret au clic |
| `notepad.cus` — liste de la demande de chemin | oui | main | un clic complète la saisie |
| `notifs.mod` — bandeaux | **non** | flèche | rien n'y répond ; un surlignage dirait le contraire |
| `winmanager.mod` | sans objet | — | surface de 1×1 invisible |
| `wallpaper.mod` | sans objet | — | rien d'interactif |
| `consent.ss` (§12.5) | **non** | flèche | dialogue au clavier seul (`O`/`N`/espace), aucun événement pointeur |
| `devstudio.ss` | **non** | flèche | éditeur au clavier |

Un surlignage sur un élément inerte est le mensonge de l'entrée 37 : il dit
« ceci répond », le clic dit le contraire. C'est la règle qui a décidé chaque
ligne de ce tableau — et c'est pourquoi les boutons de fenêtre de la barre ont
**gagné une action** plutôt que perdu leur surlignage.

> **7.7.6 revient sur une limite énoncée par la phase 7** (« le survol seul ne
> redessine rien »), à la demande explicite de l'utilisateur. Le coût qui
> l'avait fait refuser est traité, pas ignoré : le repeint est conditionné au
> **franchissement de frontière**, jamais au mouvement. Mesuré sur la série :
> `[login] pointeur : 99 evenement(s), 17 repeint(s) de survol`, et 60
> mouvements consécutifs à l'intérieur d'une même ligne de compte n'en coûtent
> **qu'un**. Le rapport n'est pas de un pour un, et c'est tout ce que la
> décision demandait.

> La 7.7.4 a été éprouvée par injection QMP `input-send-event` sur la
> tablette USB absolue, pas à la main : un clic reproductible demande une
> position, et `DECISIONS.md` a déjà payé une fois le fait qu'une souris
> PS/2 relative ne transmet rien sans capture du pointeur hôte. Les sept
> lignes `[login] clic : ...` relevées sont dans le tableau « Se connecter
> à la souris » ci-dessous.

> Écrit et éprouvé sous QEMU, **cases laissées décochées** en attente de la
> confirmation de l'utilisateur.

### Comment valider la phase 7

`make run` — sur la console série, quatre verdicts doivent apparaître :

```
[test] phase 7.1 validee - 13 verifications du framebuffer
[test] phase 7 validee - 185 verifications du systeme graphique
[test] phase 6 validee - 121 verifications de la couche OS
[test] phase 5 validee - 262 verifications de l'API systeme (spec 6)
```

Puis ces lignes. Les compteurs de modules ont monté avec les phases
suivantes — sept à l'origine, douze depuis que la phase 10 a ajouté ses
cinq modules d'accessibilité :

```
[gui] modules installes : 0 ecrits, 12 deja presents - theme systeme deja present
[gui] compositeur en ligne - canal "sys.gui", 1024x768, theme 19 cles systeme
[gui] 12 module(s) actif(s) apres balayage
[mod-splash] affiche - surface 8
[mod-splash] demarrage termine - surface rendue
[login] ecran affiche - 3 compte(s), surface 4
```

L'ordre entre `[login]` et `[mod-splash]` n'est pas garanti : les modules
créent leurs surfaces sur des tâches distinctes, et l'ordonnanceur les
entrelace. Ce qui compte est que les six lignes soient là.

**Ce qu'une console série ne prouve pas.** Le tableau ci-dessous se
regarde dans la fenêtre QEMU, et rien dans la sortie série n'en tient
lieu. En particulier `[gui] emises 0 - refusees 0` est le relevé
**normal** d'une exécution où personne ne s'est connecté : le toast du
§9.1 est émis par `login::open_session`, donc par une frappe au clavier.

**Dans la fenêtre QEMU**, la séquence complète du §2.1 :

| Ce qu'on voit | Ce que ça prouve |
|---|---|
| Barre de progression « Nova OS », étapes qui défilent | 7.7.3 — splash chargé comme un `.mod` de `boot/` |
| Panneau de connexion, 3 comptes, curseur en flèche | 7.7.1, 7.3.5 — `login.ss` et `cursor.mod` |
| Dégradé de fond, barre des tâches, horloge à droite | 7.3.5 — `wallpaper.mod` et `taskbar.mod` |
| Toast « NORMALE (1) » en haut à droite, `[1]` dans la barre | 7.6 — émission, affichage, badge |
| Le toast disparaît au bout de 5 s, le badge repasse à rien | 7.6.2 — échéance automatique du §9.1 |
| Le curseur suit la souris, entier, sans traînée | 7.2.2, 7.4.2 — routage et confinement du rectangle endommagé |
| La touche Super ouvre et referme le menu, quelle que soit la fenêtre active | 7.4.2, §16.4 — raccourcis globaux |

**Se connecter** : le compte `admin` est présélectionné, le mot de passe est
égal au nom du compte. Sur un clavier AZERTY émulé, `admin` se tape
`q d ; i n` en scancodes QWERTY. Trois mauvais mots de passe doivent afficher
« reessayez dans 30 s » sur la ligne du compte.

**Se connecter à la souris.** Depuis l'ajout du pointeur à `login.ss`, Tab
n'est plus la seule voie. Ce qui doit se voir et s'entendre sur la série :

| Geste | À l'écran | Sur la série |
|---|---|---|
| Clic sur une ligne de compte | la ligne se surligne, le champ prend le cadre accentué et le caret | `[login] clic : compte "nova" selectionne` |
| Clic dans le champ de mot de passe | cadre accentué de deux pixels, caret après les étoiles | `[login] clic : focus au champ de mot de passe` |
| Clic sur la flèche, à droite du champ | rien de plus, la tentative part | `[login] clic : validation par la fleche` puis le verdict |
| **Survol** d'une ligne de compte | la ligne s'éclaircit d'un tiers vers la sélection, un liseré d'un pixel, le libellé passe en clair ; le curseur devient une **main** | `[mod-cursor] forme main` |
| **Survol** du champ de mot de passe | le fond s'éclaircit, la bordure passe en `text_dim` — jamais en accent, réservé au focus ; le curseur devient une **barre en I** | `[mod-cursor] forme texte` |
| **Survol** de la flèche ou du bouton Accessibilité | le bouton s'éclaircit vers le blanc ; curseur **main** | `[mod-cursor] forme main` |
| **Survol** hors de tout élément | rien ne s'éclaircit ; curseur **flèche** | `[mod-cursor] forme fleche` |
| Clic sur `[F1] Accessibilité` | le bouton passe en couleur d'accent | `[login] clic : accessibilite activee` |
| Clic à côté du panneau | le cadre du champ redevient sobre, le caret disparaît | rien — un défocus n'est pas un événement |
| Frappe après un clic à côté | « cliquez dans le champ - ou Entree - pour saisir » | rien |

Le focus n'est pas décoratif : champ défocalisé, les caractères
imprimables **n'entrent pas**, et Entrée ramène le focus au champ au lieu
de soumettre. C'est ce qui empêche le cadre accentué d'être le bouton F1
d'avant la 10.1 — un état affiché que rien ne commande.

**Survol et focus ne se dessinent pas pareil**, et c'est voulu : le survol dit
où est la souris, le focus dit où va le clavier. Un champ survolé mais non
focalisé prend donc une bordure plus claire, pas la bordure d'accent — celle-là
prétendrait que le clavier est là alors qu'il n'y est pas. Trois états de ligne
de compte et trois nuances, la nuance de survol étant un **mélange** entre le
fond et la couleur de sélection : une ligne survolée peinte en `surface_alt` se
lirait comme sélectionnée, et le pointeur aurait l'air de choisir tout ce qu'il
traverse.

La flèche appelle le **même** `submit()` que la touche Entrée, y compris
sur un champ vide : la tentative compte alors un échec au barème 7.7.2,
exactement comme Entrée. Elle n'est jamais grisée, parce qu'elle n'est
jamais inactive. Une connexion entière à la souris seule — clic sur le
compte, clic dans le champ, saisie, clic sur la flèche — a été relevée :
`[login] "admin" : accepte` puis `[login] session ouverte pour "admin"`.

Trois autres configurations doivent démarrer sans échec :

| Commande | Ce qu'elle éprouve |
|---|---|
| `make run-smp` | le compositeur et ses modules sur quatre cœurs |
| `… -vga none` | pas de framebuffer : aucun module lancé, autotests atteints |
| deuxième `make run` sans `disk-reset` | idempotence — modules et thème déjà présents |

### Limites ouvertes par le bloc-notes

- **Pas de sélection, pas de copier-coller, pas d'annulation.** Nommées pour
  que personne ne les cherche.
- **Pas de retour à la ligne automatique ni d'ascenseur.** Une ligne plus large
  que la fenêtre est coupée au bord et la vue suit le caret horizontalement.
- **Le travail non enregistré est signalé, pas protégé.** Une fermeture avec des
  caractères en attente écrit
  `[notepad] fermeture avec N caractere(s) non enregistre(s)` sur la série et
  ferme quand même. Poser la question demande une boîte de dialogue modale, qui
  est une fonctionnalité à part entière.
- **Les droits sont ceux du noyau.** `Credentials::kernel()` : le noyau
  n'associe pas encore un compte à un processus, limite ouverte par la phase 6.
  Le bloc-notes peut donc écrire hors de son propre dossier, alors que son
  en-tête ne déclare que `FS_USER_HOME`. L'en-tête dit juste ; c'est
  l'application des droits qui manque.
- **Le code exécuté n'est pas celui du fichier.** Voir la 7.9.8.
- **Rien n'empêche deux instances.** Deux clics sur la ligne du menu ouvrent
  deux fenêtres, chacune avec son tampon.
- **Pas de glisser pour sélectionner** : le clic place le caret, le maintien ne
  fait rien de plus.
- **Un nom de fichier tapé sur un clavier AZERTY émulé n'a pas de point.** Le
  scancode du `.` QWERTY donne `:` en AZERTY. Ce n'est pas un défaut du
  bloc-notes — c'est la disposition — mais cela surprend au premier essai.

### Limites ouvertes par la phase 7

- **Le compositeur et les six modules sont des tâches noyau.** Le §8.1 en
  fait des applications `.ss` et `.mod`. Le protocole, les emplacements,
  le z-order et les règles de propriété sont réels ; seule la livraison du
  code change, et elle changera avec le SDK (9.1). `gui::NATIVE_MODULES`
  est la table qui disparaîtra — ni le format ni le chargeur ne bougeront.
- **L'isolation mémoire des modules reste conventionnelle.** Un module qui
  plante ne bloque pas l'OS — sa tâche meurt, le compositeur libère son
  emplacement — mais rien ne l'empêche de lire la mémoire d'un autre. Un
  CR3 par processus reste la limite ouverte par la phase 3.
- **Aucune image.** `DRAW_ICON` transporte 16×16 monochrome, soit 32
  octets. Une charge utile IPC plafonne à 256 octets, ce qui est en dessous
  d'une icône en couleurs. Les vraies images demandent de la mémoire
  partagée, donc là encore un CR3 par processus.
- **Les coins arrondis sont crénelés.** L'antialiasing suppose de relire le
  framebuffer pour mélanger avec ce qui est dessous, ce qu'un mapping
  write-combining rend lent.
- **Le mot de passe n'est pas protégé, seulement haché.** SHA-256 est
  rapide par construction ; une attaque hors ligne essaierait des millions
  de candidats par seconde. Argon2 ou scrypt sont la vraie réponse. Le sel
  n'est pas imprévisible non plus, faute de source cryptographique.
- **Les mots de passe par défaut sont égaux au nom du compte.** Sans SDK ni
  installeur, il n'y a personne pour en demander un. C'est mauvais, c'est
  dit, et c'est mieux qu'aucun mot de passe du tout.
- **Les temporisations anti-bruteforce ne survivent pas au redémarrage.**
  Seul le verrouillage à dix échecs est persisté.
- **Le survol redessine, et seulement au franchissement.** La 7.7.6 puis la
  7.8 ont levé la limite pour `login.ss`, `taskbar.mod` et `launcher.mod`. Là
  où il n'y a pas d'effet de survol, c'est que rien ne répond au clic — voir le
  tableau de la 7.8. Le repeint reste conditionné au changement de cible ; sur
  un simple déplacement à l'intérieur d'un élément, le contenu de la barre des
  tâches ne change pas et le
  repeint coûterait une vingtaine de messages IPC par millimètre de souris.
- **Aucune application n'ouvre de fenêtre.** La couche `Window`, ses
  décorations, son glisser-déposer et son focus sont écrits et exercés par
  l'autotest, mais rien dans le système ne crée encore de fenêtre : il
  faudrait une application, donc le SDK.
- **Le §16 n'est qu'amorcé.** L'emplacement `Accessibility` existe et le
  bouton du §2.4 est affiché et navigable au clavier ; les cinq modules du
  §16.2, l'arbre d'accessibilité et les modes daltonisme sont l'étape 10.1.
- **Le mode « ne pas déranger » n'a pas d'interrupteur.** La politique du
  §9.1 est appliquée, mais rien ne permet encore à l'utilisateur de
  l'activer — il faudrait un panneau de réglages.
- **`.gs` et la bascule avant/arrière-plan.** `scheduler::set_foreground`
  attend toujours son appelant : le compositeur sait qui a le focus, mais
  ne le relie pas encore à la règle du §3.4.

---

## Phase 8 — SPM & Mises à jour

### 8.1 Format .spm (spec §10.1)
- [x] 8.1.1 Header SPM (magic, version, code MAJ, hash SHA-256, signature)
- [x] 8.1.2 Manifest (métadonnées)
- [x] 8.1.3 Scripts pre/post installation

### 8.2 Gestionnaire de paquets
- [x] 8.2.1 `spm install` (repo, `--user`, fichier local)
- [x] 8.2.2 `spm remove`
- [x] 8.2.3 `spm update` / `spm search`
- [x] 8.2.4 `spm repo add`
- [x] 8.2.5 Codes de mise à jour -1/0/1/2 (spec §10.2)
- [x] 8.2.6 Niveaux d'avertissement à l'installation (spec §10.4)

### 8.3 Dépôts
- [x] 8.3.1 Format de dépôt distant
- [x] 8.3.2 Vérification signature à la récupération

### 8.4 Mises à jour OS (spec §11)
- [x] 8.4.1 Types PATCH/UPDATE/MAJOR/CRITICAL
- [x] 8.4.2 Service `updater.dsa` (vérification 24 h configurable)
- [x] 8.4.3 Téléchargement silencieux en arrière-plan

### 8.5 Rollback automatique (spec §11.3)
- [x] 8.5.1 Stratégie double-partition
- [x] 8.5.2 Validation des services core au boot suivant (timeout 60 s)
- [x] 8.5.3 Rollback automatique si échec + notification
- [x] 8.5.4 Snapshots incrémentaux (30 jours, `/sys/updates/rollback/`)


> Écrit et éprouvé sous QEMU : **224 vérifications passent**, aucun échec.
> Les cases sont cochées sur la foi de la sortie série reproduite ci-dessous.

### Comment valider la phase 8

`make run` — sur la console série, la ligne de verdict :

```
[test] phase 8 validee - 224 verifications de SPM et des mises a jour (spec 10 et 11)
```

Puis, dans l'ordre, ces lignes de mise en place :

```
[spm] arborescence prete - N dossier(s) cree(s), depot local declare
[updater] service demarre (spec 11.2)
[spm] depot "systeme" -> file:/sys/tools/spm/depot (ancre)
[updater] version 0.8.0.1 - intervalle 24 h, automatique oui
[rollback] version 0.8.0.1 - emplacement A (volume distinct), 0 snapshot(s)
```

Et, pendant l'autotest, la trace d'une installation complète :

```
[spm]   paquet       : test.spm.exemple "Exemple SPM" v1.0.0.0
[spm]   code MAJ     : 2 (silencieuse) - aucune notification, appliquee en arriere-plan
[spm]   spec 10.4    : non verifie - avertissement jaune
[spm] test.spm.exemple v1.0.0.0 installe dans /apps/test.spm.exemple - portee global, 5 fichier(s), installe
[spm] test.spm.exemple desinstalle - 5 fichier(s) supprime(s), 0 conserve(s)
```

Ce qu'il faut vérifier point par point :

| Observation | Ce que ça prouve |
|---|---|
| `phase 8 validee - 224` et **aucun** `[spm] ECHEC` | les tables §10.2, §10.4 et §11.1 sont transcrites ligne à ligne |
| `depot local declare` puis, au second démarrage, `deja declare` | idempotence — `repo add` ne réécrit pas ce qui existe |
| L'installation apparaît, puis la désinstallation avec `0 conserve(s)` | 8.2.1 et 8.2.2 — le paquet ne laisse rien derrière lui |
| Aucun fichier `test.spm.*` dans `/apps/` au démarrage suivant | l'autotest purge en entrant **et** en sortant |
| `emplacement A (volume distinct)` | 8.5.1 — le second volume QEMU sert bien d'emplacement B |

Deux configurations supplémentaires doivent passer :

| Commande | Ce qu'elle éprouve |
|---|---|
| `make run-smp` | l'installation et la base sous quatre cœurs |
| deuxième `make run` sans `disk-reset` | idempotence de `install_layout` et de `repo add` |

### Limites ouvertes par la phase 8

- **`spm` n'a pas d'interface.** Le §10.3 décrit une ligne de commande, et
  `manager::execute` analyse littéralement cette chaîne — mais rien ne
  permet encore de la taper. Le terminal vient avec le SDK (9.4) ;
  `execute` attend déjà une chaîne et ne changera pas.
- **Les scripts pre/post ne sont pas du code.** Six verbes déclaratifs,
  pas de branchement ni de boucle. C'est un choix de sécurité assumé —
  l'installeur tourne sous les droits du noyau — et non une étape
  intermédiaire vers un interpréteur complet.
- **La double partition est consignée, pas amorcée par GRUB.** L'état
  bascule d'emplacement, les fichiers vont bien dans le volume B, le
  rollback restaure — mais l'image effectivement démarrée reste celle que
  GRUB connaît. Fermer cette limite demande une entrée de menu par
  emplacement et un noyau qui sait où il tourne.
- **`http://` est écrit, pas éprouvé.** Le client HTTP/1.0 du transport de
  dépôt n'a pas de serveur en face sous QEMU. Seul le schéma `file:` est
  exercé par l'autotest.
- **L'ancre d'un dépôt n'authentifie personne.** Comme en phase 3, c'est un
  hachage dont l'entrée est connue : il détecte une altération, il
  n'établit pas une provenance. Le jour où Ed25519 existera, l'ancre
  deviendra une clé publique et `repo::seal_of` sera la seule fonction à
  changer.
- **Le code -1 du §10.2 n'obtient pas son bouton non ignorable.** Une
  notification PRIORITAIRE exige au moins un bouton (§9.1), et
  `post_notification` n'en pose pas : le niveau est rabaissé à HAUTE. Le
  chemin applicatif `OP_NOTIFY_POST` sait construire les boutons ; il
  faudrait que le gestionnaire passe par lui, donc qu'il soit une
  application.
- **Aucune mise à jour de l'OS n'est réellement appliquée.** `rollback::apply`
  bascule l'état et prend le snapshot ; ce qui manque est le remplacement
  effectif des fichiers du système, qui suppose de savoir lesquels
  constituent l'OS — c'est le manifeste que produira le SDK.
- **Les dépendances ne sont pas résolues, seulement vérifiées.** Un paquet
  dont une dépendance manque est refusé ; rien ne va la chercher au dépôt.
  La résolution transitive suppose un graphe et une politique de conflit
  qu'aucune section de la spec ne décrit.
- **Le nettoyage à la désinstallation s'arrête au premier obstacle.** Un
  dossier où l'utilisateur a ajouté un fichier n'est pas supprimé, et
  c'est voulu : mieux vaut laisser un dossier que d'emporter des données.
  Le compteur `conserve(s)` le dit.

---

## Phase 9 — SDK & Langages

### 9.1 CLI `dev`
- [x] 9.1.1 `dev new` (templates : blank, gui-app, service, game, cli-tool) — *les cinq gabarits sont créés, construits, et le binaire produit est relu par `binfmt::parse`*
- [x] 9.1.2 `dev build --release` — *incrémente le numéro de construction et le réécrit dans le manifest*
- [ ] 9.1.3 `dev run --debug` (sandbox + débogueur) — *le bac à sable est fait et conservé pour inspection ; **le débogueur n'existe pas** : pas de point d'arrêt, pas de lecture de registre*
- [x] 9.1.4 `dev interpret` — *le verbe fonctionne sur Nova Script (`.ns`) ; les langages du §13.2 relèvent du 9.5*
- [x] 9.1.5 `dev package --sign` — *le `.spm` produit est relu du volume et accepté par la vérification de la phase 8*
- [x] 9.1.6 `dev convert --from electron` (appel à ETR)

### 9.2 dev.toml (spec §15.3)
- [x] 9.2.1 Parsing manifest complet — *les dix champs du §15.3, aller-retour écriture/relecture vérifié*

### 9.3 Sandbox de test (spec §15.4)
- [x] 9.3.1 Espace mémoire dédié — *arène propre, pages remises à zéro*
- [x] 9.3.2 FS virtualisé (overlay lecture seule) — *l'écriture ne traverse pas jusqu'à l'hôte*
- [x] 9.3.3 Réseau loopback uniquement — *une adresse hors 127.0.0.0/8 est refusée*
- [x] 9.3.4 Exécution automatique en `.ras`
- [x] 9.3.5 Nettoyage complet à l'arrêt — *`5 creee(s), 5 detruite(s), 0 vivante(s)`, et les pages sont rendues au compte exact*

### 9.4 IDE devstudio.ss

> **Retiré à la demande en septembre 2026.** Les trois cases ont été
> validées le 5 septembre 2026 et restent cochées comme trace historique ;
> le code (`sdk::devstudio`) n'existe plus, et `apps::install_native` efface
> `/sysapp/devstudio.ss` d'un volume qui le portait encore. Le catalogue
> `libdev` du 9.4.3 reste en place, sans IDE pour l'afficher.
- [x] 9.4.1 Éditeur de base — *tampon de lignes, curseur, insertion, suppression, Ctrl+S ; pas de sélection, pas de copier-coller, pas de coloration*
- [x] 9.4.2 Intégration build/run/debug — *F5 `dev build`, F9 `dev run`, F7 `dev run --debug`, F8 `dev package` ; le « debug » vaut ce que vaut le 9.1.3*
- [x] 9.4.3 Intégration SDK/libdev — *le panneau latéral bascule sur le catalogue `libdev`*

### 9.5 Runtimes multi-langages (spec §13.2)
- [x] 9.5.1 Rust (natif direct) — *`sdk::rustc` : lexeur, parseur, vérificateur de types, générateur de code x86_64 et interprète, écrits ici. Un **sous-ensemble** défini du langage, tout ce qui en sort étant refusé nommément. Les deux colonnes du §13.2 sont répondues*
- [ ] 9.5.2 C/C++ (LLVM JIT + natif) — *déclaré au tableau, résolu, refusé nommément ; aucune toolchain*
- [ ] 9.5.3 C# (Mono allégé + transpilation) — *idem*
- [ ] 9.5.4 Java (JVM maison minimale) — *idem*
- [ ] 9.5.5 Python (rustpython) — *idem*
- [ ] 9.5.6 Node.js / Bun (moteurs JS/TS → Rust) — *idem*
- [x] 9.5.7 ASM x86_64 (émulateur sandboxé) — *chaîne complète : `dev build` assemble, édite les liens et produit un binaire qui **s'exécute en ring 3**. Ce que le §13.2 appelle « émulateur sandboxé » est ici le bac à sable du 9.3, qui fait tourner le vrai code au lieu de l'émuler*
- [x] 9.5.8 Emplacements runtimes (`/sys/runtimes/` vs `/users/{user}/runtimes/`) — *la priorité du §13.3 est éprouvée : un descripteur posé chez un compte masque celui du système, un autre compte ne le voit pas, son retrait rend la main*

### 9.6 Outil ETR (spec §13.1)
- [x] 9.6.1 Analyse AST du projet Electron — *lexeur et AST JavaScript écrits ici*
- [x] 9.6.2 Mapping des équivalents système
- [x] 9.6.3 Rapport de conversion (taux de réussite, avertissements) — *les manques sont nommés un par un*
- [x] 9.6.4 Seuils : ≥ 70 % → `.cus` natif ; < 70 % ou `eval()` → `.ras`

### Comment valider la phase 9

`make run` puis `make run-smp`. Le verdict :

```
[test] phase 9 validee - N verifications du SDK (spec 13 et 15)
```

Il part **après** celui de la phase 8, et c'est voulu : les deux écrivent
sur le volume, et celui de la phase 9 mesure les cadres libres autour de
ses allocations. Les faire tourner en parallèle rendrait ce compte
dépendant de ce que l'autre est en train de faire.

Ce qu'il faut observer, dans l'ordre où cela apparaît :

1. `[sdk] arborescence prete - N entree(s) creee(s)` — N non nul au
   premier démarrage, `0` au second : l'installation est idempotente.
   (Jusqu'au retrait de l'IDE, la ligne se terminait par
   `devstudio.ss installe` ou `deja present`, et deux lignes
   `[devstudio] surface ...` / `[devstudio] arret ...` suivaient.)
2. `[test] phase 9 validee` **sans aucune ligne `[sdk] ECHEC`**. Chaque
   échec nomme le contrôle exact qui a cédé.
3. `[dev] N construction(s), N execution(s), N paquet(s), N conversion(s)`
   — non nuls. Un zéro signifierait que l'autotest a été ignoré, pas
   qu'il a réussi.
4. `[sandbox] 0 vivante(s) - N creee(s), N detruite(s)`, avec les deux
   compteurs **égaux**. Un écart est un bac à sable qui a survécu à son
   test, donc des cadres et un dossier laissés derrière.
5. Les invariants permanents de `DECISIONS.md`, en particulier **le nombre
   de cadres libres identique d'un rapport du moniteur au suivant** :
   c'est ce qui prouve que `dev run` rend ce qu'il prend.

Contrôles à faire à la main, une fois la sortie verte :

- **Second démarrage.** `/sys/tools/sdk` et
  `/sys/runtimes/asm/runtime.cfg` doivent être retrouvés, pas recréés.
- **Démarrage sans framebuffer** (`-vga none`, pas `make run-nogui` :
  `-display none` supprime la fenêtre mais laisse le matériel VGA).
  Depuis le retrait de l'IDE, la phase 9 ne dépend plus du compositeur :
  le compte de vérifications doit être **le même** qu'avec `make run`.
- **`make run-smp`.** Les défauts de synchronisation du registre des bacs
  à sable ne se manifestent que sous quatre cœurs.

### Ce qui a été observé, le 5 septembre 2026

Quatre exécutions, aucune ligne `[sdk] ECHEC`.

| Exécution | Relevé |
|---|---|
| `make run` | `phase 9 validee - 433 verifications` |
| `make run-smp` | `433` également |
| `-vga none` | `429` — les quatre vérifications de devstudio tombent avec lui, et `[sdk] pas de compositeur - devstudio.ss n'est pas lance` apparaît |
| premier démarrage, après `make disk-reset` | `[sdk] arborescence prete - 4 entree(s) creee(s), devstudio.ss installe`, puis `0 entree(s) creee(s), devstudio.ss deja present` au suivant |

Le nombre est passé de 284 à 433 en deux temps : cinq vérifications pour
le contrôle du 9.5.8 qui ne contrôlait rien, puis cent trente-neuf pour
l'étape 9.5.1.

### Ce que l'étape 9.5.1 a ajouté

`src/sdk/rustc/` — six modules :

| Module | Ce qu'il fait |
|---|---|
| `lex` | jetons Rust, lignes portées, refus des durées de vie et des littéraux de caractère |
| `ast` | l'arbre, lu **sans changement** par les deux back-ends |
| `parse` | descente récursive, escalade de priorité, refus nommé de tout ce qui sort du sous-ensemble |
| `check` | types, mutabilité, arité, retour certain, attribution des emplacements de pile |
| `codegen` | machine à pile, cadre `[rbp - 8n]`, sortie en **texte d'assemblage** |
| `interp` | évaluateur d'arbre : la colonne « interprète » du §13.2 |

Trois décisions non triviales, détaillées dans `DECISIONS.md` :

- **Le générateur émet du texte, pas des octets**, et le passe à
  `sdk::asm`. Deux encodeurs d'instructions dans le dépôt divergeraient,
  et la divergence se verrait comme un programme qui tourne et fait autre
  chose — pas comme une compilation qui échoue.
- **Machine à pile plutôt qu'allocateur de registres.** Trois fois plus
  lent et manifestement correct ; surtout, `syscall` détruit `rcx` et
  `r11`, et avec toute valeur vivante sur la pile un `println!` au milieu
  d'une expression ne peut rien corrompre.
- **Une valeur = un mot.** Un `&str` est un pointeur vers un bloc
  `{ longueur, octets }` de `.data`, ce qui lui permet de traverser un
  paramètre, une constante et un trou de `println!` sans cas particulier.

L'assembleur du 9.5.7 a reçu quatre ajouts, tous des instructions que le
manuel décrit : les opérandes mémoire `[reg + dep]` — sans quoi les
variables locales n'ont que les seize registres pour vivre —, le `mov`
d'un octet vers la mémoire, `imul` / `idiv` signés, et les décalages.

Les six points de la liste ci-dessus sont sortis conformes :

```
[devstudio] surface 12 - 768x576 en (128, 96)
[devstudio] arret - surface rendue
[dev] 7 construction(s), 3 execution(s), 2 paquet(s), 1 conversion(s)
[sandbox] 0 vivante(s) - 5 creee(s), 5 detruite(s)
```

`[moniteur] frames libres : 123955` sur les vingt-trois rapports qui
suivent la fin du boot : `dev run` rend ce qu'il prend.

Le compte est passé de 284 à 289 pendant cette validation — voir les deux
bugs ci-dessous.

### Bugs corrigés pendant l'étape 9.5.1

Deux, et tous deux trouvés en **exécutant** le code produit, pas en le
lisant. C'est le propre d'un compilateur : ses erreurs ne font pas
échouer la compilation, elles font répondre autre chose.

- **Une fonction dont le corps se termine par une expression rendait
  zéro.** Le générateur émettait `mov rax, 0` juste avant l'épilogue, au
  nom de « tomber en fin de fonction est un fait, pas une hypothèse ».
  Sauf qu'un corps à expression finale vient de laisser sa valeur dans
  `rax` : la voici écrasée. `fn somme(n: i64) -> i64 { if n == 0 { 0 }
  else { n + somme(n - 1) } }` est exactement cette forme, et le
  programme s'assemblait, se chargeait, s'exécutait en ring 3, imprimait
  — et imprimait `0`. Le repli n'est plus émis que lorsqu'il n'y a rien
  à préserver.
- **Un `break` dans un `if` final était avalé par l'interprète.** Un `if`
  en dernière position d'un bloc est la valeur de ce bloc, donc une
  **expression** — et une expression rend une valeur, pas un flux.
  `loop { n += 1; if n == 7 { break; } }` tournait donc jusqu'à
  l'épuisement du budget de pas. Le compilé n'avait pas le problème : un
  `break` y est un `jmp` vers une étiquette qui l'englobe lexicalement,
  quelle que soit l'expression autour. C'est l'interprète qui a été mis
  d'accord avec lui, par un champ `escaped` que les blocs en position de
  valeur reversent à leur appelant.

### Bugs corrigés pendant la validation de la phase 9

- **Le contrôle du 9.5.8 ne contrôlait pas le 9.5.8.** Son commentaire
  annonçait « la résolution doit favoriser le runtime de l'utilisateur »,
  et l'assertion en dessous vérifiait que l'assembleur se résout côté
  **système** — c'est-à-dire ce que `install_layout` vient de poser, donc
  ce qu'on aurait trouvé que la priorité marche ou non. La règle du §13.3
  n'avait aucun contrôle. `check_runtime_priority` en pose cinq : un
  descripteur installé sous `/users/nova/runtimes/asm/`, l'origine qui
  bascule sur `User`, la version relue depuis ce fichier-là, un autre
  compte qui continue de voir le système, et le retour au système après
  retrait. Les cinq passent — la priorité marchait, elle n'était
  simplement pas éprouvée.
- **F6 ne faisait rien dans devstudio.** Le §16.4 a donné F6 au changement
  de panneau, et le compositeur le classe donc parmi les raccourcis
  système : `is_global_shortcut` le route vers la couche `Shell`, et
  `devstudio.ss` vit dans la couche `Window`. La touche n'atteignait plus
  personne — aucun module de la couche `Shell` ne la traite non plus.
  `dev run` passe sur **F9**, libre des deux côtés. C'est la phase 10 qui
  a pris la touche à la phase 9, et rien ne l'avait signalé : une touche
  qui ne fait rien ne fait pas échouer d'autotest.

### Limites ouvertes par la phase 9

- **Le compilateur Rust ne prend qu'un sous-ensemble du langage.** C'est
  la limite que l'étape 9.5.1 laisse ouverte en refermant la précédente
  (« il n'y a de compilateur que pour l'assembleur »). Ce qui compile :
  `fn`, `const`, `i64`, `u64`, `bool`, `&str`, `()`, les dix-huit
  opérateurs binaires, `if` / `while` / `loop` / `break` / `continue` /
  `return`, les blocs valeur, `print!` et `println!`. Ce qui ne compile
  pas et **le dit avec le nom de la construction** : `struct`, `enum`,
  `trait`, `impl`, `match`, `for`, les fermetures, les génériques, les
  méthodes, l'indexation, les tuples, `String`, `Vec`, `Option`,
  `Result`, `?`, `unsafe`, `async`, `use`. Élargir ce périmètre est un
  travail incrémental : chaque construction ajoutée est une ligne de
  moins dans la liste des refus.
- **Deux comportements diffèrent de `rustc`, et les deux sont dits.**
  L'arithmétique **boucle** au lieu de paniquer au débordement — c'est le
  comportement *release* de Rust, et x86 ne piège pas de lui-même ; un
  contrôle après chaque addition doublerait le code émis. Un décalage est
  **masqué à six bits**, parce que c'est ce que fait le processeur. Le
  compilé et l'interprété s'accordent sur les deux, ce qui est la seule
  chose qui compte : c'est l'accord qui permet de se servir de l'un pour
  éprouver l'autre.
- **L'interprète plafonne à dix appels imbriqués.** Un niveau d'appel
  interprété coûte les cadres de `call`, `call_inner`, `block`,
  `statement`, `expr` et `apply`, et en build de débogage cela se compte
  en kilooctets sur une pile de tâche de 128 Kio déjà entamée par
  l'appelant. Douze niveaux ont été mesurés depuis l'autotest ; dix est le
  chiffre retenu, avec la marge. **Le compilé n'a pas cette limite** — il
  récurse sur sa propre pile ring 3, et l'autotest le prouve en faisant
  tourner en bac à sable une récursion de vingt-quatre niveaux que
  l'interprète refuserait.
- **Les six autres lignes du §13.2 exigent toujours une toolchain
  externe.** Elles sont déclarées, résolues et refusées nommément.
- **`libdev` n'est pas livrée en binaire.** Le §15.1 la décrit comme une
  bibliothèque ; ce qui existe est son **contrat** — la table des
  abstractions et de leur traduction, vérifiée contre la table d'appels de
  la phase 5. Le code, lui, demanderait un compilateur.
- **Il n'y a pas de débogueur.** `dev run --debug` trace le cycle de vie
  et conserve le bac à sable pour inspection. Pas de point d'arrêt, pas de
  lecture de registre. Un vrai débogueur suppose `int3` géré comme un
  événement transmis à un processus superviseur, et donc un protocole que
  la phase 5 n'a pas.
- **L'assembleur n'émet que du code position-indépendant.** Pas de
  relocation absolue, pas d'adressage indexé (`[rax + 8]`), pas de
  registres 8/16/32 bits, pas de flottant. C'est délibéré pour les trois
  premiers points ; le quatrième découle du soft-float du noyau.
- **Le bac à sable ne libère pas ses tables de pages intermédiaires.**
  `memory::unmap_page` rend la page, pas la table qui la portait — c'est
  vrai de l'arène du §6.4 comme de celle du §15.4. Le coût est d'un cadre
  par tranche de 2 Mio et par arène, payé une seule fois. L'autotest le
  neutralise par un cycle de chauffe avant sa mesure, et le dit.
- **L'isolation mémoire du bac à sable reste conventionnelle.** Toutes les
  tâches partagent une hiérarchie de pages — limite ouverte par la phase 3
  et toujours ouverte. L'arène `0x3000_0000` est disjointe des trois
  autres, ce qui garantit qu'aucune allocation ne se marche dessus, mais
  pas qu'une tâche ne puisse lire celle d'une autre.
- **Le loopback du §15.4 est une pile séparée, pas une interface.** Il n'y
  a pas de `lo` dans `net` : le bac à sable fournit des ports et des files
  en mémoire, et refuse toute adresse hors `127.0.0.0/8`. Un programme qui
  appellerait `SYS_NET_*` au lieu de passer par le bac à sable atteindrait
  la vraie carte — la substitution des appels systèmes par tâche
  supposerait une table de dispatch par processus.
- **L'overlay ne virtualise pas `list`.** `read`, `write`, `delete` et
  `exists` traversent les deux couches ; l'énumération d'un dossier, non.
  Un programme qui listerait `/apps/{id}` verrait la couche basse seule.
  La fusion suppose de dédoublonner deux listings et d'y appliquer les
  pierres tombales.
- **ETR mesure le taux sur les sites d'appel.** Le §13.1 dit « taux de
  réussite » sans dire de quoi. Le choix est documenté dans `sdk::etr` :
  c'est la seule mesure qui reflète ce qui manquera à l'exécution. Un
  projet dont l'essentiel est du DOM tombe à 15 %, ce qui est juste ; un
  projet sans aucun appel est déclaré converti à 100 %, ce qui est vrai et
  inutile.
- **ETR ne suit pas les affectations.** La correspondance se fait par
  suffixe pointé : `fs.readFileSync` couvre `nodeFs.readFileSync`. Une
  vraie analyse de flot de données lèverait l'approximation ; elle est
  hors de proportion pour un outil que le §13.1 décrit comme manuel et
  dont le rapport est relu par un humain.
- **`node_modules` n'est pas analysé**, ni au-delà de 64 fichiers ou
  256 Kio par fichier. Le rapport dit combien il a écarté ; le taux ne
  porte que sur ce qu'il a lu.
- **`dev` n'a plus d'interface.** `execute` attend une chaîne, et depuis
  le retrait de `devstudio.ss`, seul l'autotest la lui donne : il n'y a
  ni terminal ni IDE pour lancer `dev build` à la main. C'est exactement
  la situation de `spm` en fin de phase 8, et elle se résoudra de la même
  façon.

---

## Phase 10 — Finitions ✅

### 10.1 Accessibilité (spec §16)
- [x] 10.1.1 `accessibility.mod` (arbre d'accessibilité parallèle)
- [x] 10.1.2 `screenreader.mod` (synthèse vocale)
- [x] 10.1.3 `zoom.mod`
- [x] 10.1.4 `contrast.mod` (+ modes daltonisme)
- [x] 10.1.5 `keyboard-nav.mod`
- [x] 10.1.6 Raccourcis globaux (Alt+Super+S/Z/C/K, Tab, Entrée, Super+Tab, F6)
- [x] 10.1.7 Fonctionnalité dès l'écran de connexion

### 10.2 Internationalisation (spec §14)
- [x] 10.2.1 Priorité Application > Utilisateur > Système
- [x] 10.2.2 Rechargement à chaud via `SYS_IPC_PUBLISH("i18n.changed")`
- [x] 10.2.3 Formats date/heure/nombres/monnaie/distance/poids/température/vitesse
- [x] 10.2.4 Clavier (layout principal/secondaire/switch)
- [x] 10.2.5 Fuseaux horaires (stockage UTC, conversion affichage)
- [x] 10.2.6 Format `.lang` (clé-valeur, variables `{nom}`)

### 10.3 Sécurité avancée (spec §12)
- [x] 10.3.1 `AppIdentity` immuable à la compilation *(structure faite en 3.1.3)*
- [x] 10.3.2 Règles de communication par groupe/type (§12.1) *(logique faite en 3.1)*
- [x] 10.3.3 Modèle hybride liste blanche/liste noire (§12.3)
- [x] 10.3.4 Sandbox par type détaillée (§12.4)
- [x] 10.3.5 Prompt d'autorisation + mémorisation du choix (§12.5)

### 10.4 Sous-systèmes de compatibilité (long terme)
- [x] 10.4.1 Sous-système Linux
- [x] 10.4.2 Sous-système Windows

### Ce que la phase 10 a ajouté

Quatre sous-systèmes, décrits dans `ARCHITECTURE.md` :

| Module | Couvre | Ce qui s'exécute vraiment |
|---|---|---|
| `access/tree` | 10.1.1 | arbre parallèle : rôle, label, indice, valeur, focalisable, raccourcis |
| `access/speech` | 10.1.2 | synthèse à formants en virgule fixe Q12, règles graphème-phonème françaises |
| `access/reader` | 10.1.2 | `screenreader.mod` : quoi annoncer, quand, et quoi interrompre |
| `access/zoom` | 10.1.3 | zoom global, loupe, zone — correspondance inverse, plus proche voisin |
| `access/contrast` | 10.1.4 | contraste élevé et les cinq modes du §16.3, en **correction** et non en simulation |
| `access/keynav` | 10.1.5 | table du §16.4 en données, déplacement du focus, panneaux |
| `i18n/units` | 10.2.3 | dates, heures, nombres, monnaie, quatre familles d'unités |
| `i18n/lang` | 10.2.6 | format `.lang`, substitution `{nom}` bornée |
| `i18n/keyboard` | 10.2.4 | AZERTY, QWERTY, QWERTZ, Dvorak + touche de bascule |
| `i18n/tz` | 10.2.5 | vingt fuseaux, décalage en minutes, stockage UTC |
| `security/policy` | 10.3.3 | moteur de décision en quatre étapes, ordre vérifié |
| `security/sandbox` | 10.3.4 | tableau §12.4 appliqué, racines de session comprises |
| `security/consent` | 10.3.5 | invite graphique modale avec case « se souvenir » |
| `compat/elf` + `linux` | 10.4.1 | parseur ELF64, conversion d'image, 24 appels traduits sur 32 |
| `compat/pe` + `windows` | 10.4.2 | parseur PE32+, conversion d'image, 11 appels NT sur 14 |

Points où la spécification laissait le choix, tranchés explicitement :

- **Le séparateur décimal n'est pas dans le §14.2.** Il n'y donne que le
  séparateur des milliers. Laisser le décimal fixe imprimerait `1,234,56`
  en anglais. Règle retenue : une virgule aux milliers impose le point aux
  décimales, tout autre séparateur impose la virgule.
- **Les modes daltonisme du §16.3 corrigent, ils ne simulent pas.**
  Simuler montre à un voyant ce que voit un daltonien ; corriger déplace
  l'information hors du canal que l'œil ne sépare pas. Le §16.3 les range
  sous « Accessibilité », donc c'est la seconde lecture.
- **Cinq emplacements de modules ont été ajoutés**, un par ligne du §16.2,
  plus un emplacement `Diagnostic` que l'autotest de la phase 7.3.4
  occupait jusqu'ici dans `Accessibility`. Le §8.2 fige sa propre colonne,
  il n'interdit pas les emplacements que le reste de la spec réclame —
  précédent posé par `Splash` en 7.7.
- **Les numéros d'appel NT sont ceux de ce sous-système, pas ceux de
  Windows.** Les vrais changent d'une version à l'autre : en recopier une
  serait recopier une version, pas une interface.

### Bugs corrigés pendant la phase 10

- **L'identité d'une tâche vivante pouvait être réécrite.** `apps::register`
  écrasait silencieusement l'entrée d'un PID déjà enregistré. Le §12.1 veut
  `AppIdentity` « immuable à l'exécution » et `DECISIONS.md` la liste parmi
  les décisions non négociables : c'était exactement la mutation interdite.
  Un réenregistrement différent est maintenant refusé et journalisé.
- **Le pilote audio saturait la machine dès qu'un son existait.** Il
  remplissait tous les tampons dépassés par le contrôleur à chaque réveil,
  avec une allocation de 4 Kio par tampon, cent fois par seconde. Rien ne
  l'avait montré tant que le seul son du système était le bip d'une seconde
  de l'autotest de la phase 6 ; le lecteur d'écran joue en continu, et le
  coût est devenu la machine entière. Un tampon par réveil suffit — un
  tampon vaut 21 ms de son, la boucle tourne toutes les 10 ms — et le
  tampon de travail est alloué une fois.
- **Le lecteur d'écran gardait son flux audio ouvert en permanence.** Un
  flux qui existe rend `audio::has_audio` vrai, donc fait tourner le pilote
  pour du silence entre deux annonces. Il s'ouvre désormais par énoncé.
- **Pas d'invalidation de TLB entre cœurs — limite ouverte depuis la
  phase 3, désormais fermée.** Elle est devenue visible de la façon la plus
  directe qui soit : un binaire étranger chargé dans un créneau d'arène
  recyclé et démarré sur un autre cœur exécutait le code de l'occupant
  précédent, et fautait au cinquième octet. Un compteur de génération, une
  IPI de diffusion (vecteur 241) et trois points de synchronisation — la
  IPI, le tic d'horloge, et l'entrée en ring 3 — la lèvent. Aucune attente
  d'acquittement : le premier essai en faisait une par page libérée, ce qui
  bloquait quatre cœurs pendant la libération d'une image.
- **Une touche frappée avant une invite y répondait.** L'invite lit la file
  du clavier, et ce qui y traînait déjà l'acceptait à l'instant où elle
  s'affichait. L'autotest du 10.3 l'a subi pour de vrai. `keyboard::drain`
  vide la file avant toute lecture de réponse, dans l'invite graphique
  comme dans celle du 6.4.2.
- **Le bac à sable du §12.4 était décrit, pas appliqué** — limite ouverte
  depuis la phase 4. `syscall::domain_fs::guard` l'applique maintenant dans
  `path_at`, donc pour les dix-huit appels qui nomment un chemin.
- **L'agencement du clavier était figé dans une fonction.** Le §14.2 veut
  un agencement par utilisateur ; l'en-tête de `drivers::keyboard` le
  listait comme son défaut numéro trois depuis la phase 6.1.1. Les quatre
  tables sont dans `i18n::keyboard`, en données.

### Bugs corrigés pendant la validation de la phase 10

Trois défauts que seule l'exécution pouvait montrer, et qu'aucune des
340 vérifications ne voyait : toutes trois portent sur ce que l'autotest
laisse derrière lui ou sur ce qu'il ne peut pas observer de lui-même.

- **L'autotest du 16.1 vidait l'arbre d'accessibilité réel.**
  `access::selftest::run` construisait chacun de ses arbres en local —
  son propre commentaire disait pourquoi : « un test qui viderait l'arbre
  global laisserait la machine sans arbre d'accessibilité » — puis
  appelait `with_tree(|tree| tree.clear())` en sortie, ce qui faisait
  exactement cela. Les trois nœuds que le coordinateur déclare pour le
  shell disparaissaient, et Tab ne parcourait plus rien pour le reste de
  la session. La ligne d'état le disait sans qu'on l'entende :
  `[access] arbre : 0 element(s), 0 focalisable(s)`. Elle dit maintenant
  `3 element(s), 2 focalisable(s) - focus 2`.
- **Le même autotest éteignait le mode couleur configuré.** Il relevait
  l'état d'entrée pour le restaurer, mais lisait le mode par
  `contrast::Mode::from_code(0)` — c'est-à-dire `None`, quel que soit le
  mode réel. Une machine réglée sur la deutéranopie sortait de son propre
  autotest avec la correction éteinte. Le relevé se fait désormais par
  `contrast_mode()`.
- **Le lecteur d'écran se déclarait sonore sur une machine muette.** Il
  demandait un flux au **mixeur** et prenait le succès pour une réponse.
  Le mixeur existe qu'un contrôleur le joue ou non : `audio::open`
  réussit donc sur une machine sans carte son, et le lecteur annonçait
  `sortie audio disponible` en même temps que le pilote annonçait
  `aucun controleur AC'97 sur le bus PCI`. Il poussait en prime ses
  échantillons dans une file que rien ne vidait. La question se pose
  maintenant au registre des pilotes, et elle attend que celui-ci ait
  répondu : le lecteur est un module du compositeur, donc il démarre
  **avant** `drivers::start_all`, et interroger le registre sans attendre
  aurait répondu « absent » sur toutes les machines. L'attente est bornée
  à cinq secondes, et le lecteur reprend le diagnostic du pilote mot pour
  mot plutôt que d'en inventer un.

### Comment valider la phase 10

`make run` puis `make run-smp`. Le verdict :

```
[test] phase 10 validee - 340 verifications des finitions (spec 12, 14 et 16)
```

Il part **après** celui de la phase 9, et pour la même raison qui a mis la
9 après la 8 : les trois écrivent sur le volume.

Ce qu'il faut observer, dans l'ordre :

1. `[i18n] N entree(s) creee(s) - langue fr-FR (repli en-US), 21 + 21 cle(s)`
   — au premier démarrage `N` vaut 5, au second 0 : l'installation est
   idempotente, et une langue choisie par l'utilisateur ne doit pas
   revenir au français à chaque redémarrage.
2. `[access] N entree(s) creee(s), 11 reglage(s) charge(s) - raccourcis 16.4 actifs`,
   suivi de l'état des quatre fonctions. Les onze réglages doivent être
   lus : un zéro signifierait un fichier de configuration illisible.
3. `[linux] bonjour depuis un ELF` **et** `[windows] bonjour depuis un PE`.
   C'est le seul contrôle qui prouve que les deux sous-systèmes exécutent
   quelque chose : un tableau de traduction se relit, il ne s'éprouve pas.
   Les deux lignes sont écrites par du code ring 3 assemblé par le SDK de
   la phase 9, empaqueté en ELF64 et en PE32+ par l'autotest lui-même.
4. `[linux] PID N sort avec le code 0 apres 3 appel(s)` et son équivalent
   Windows : les programmes se terminent d'eux-mêmes. Un étranger qui ne
   se termine pas garde un créneau d'arène et continue de traduire.
5. `[compat] N image(s) chargee(s), 0 refusee(s), 6 appel(s) traduit(s) - 0 vivante(s)`.
   Le zéro final est l'invariant : aucune tâche étrangère ne survit à
   l'autotest.
6. `[smp] 280 invalidation(s) de TLB diffusee(s) - 0 coeur(s) en retard`,
   sous `make run-smp` uniquement. Le second nombre doit être **zéro** :
   un cœur en retard durablement ne prend plus ses interruptions.
7. Les invariants permanents de `DECISIONS.md`, en particulier le nombre
   de cadres libres identique d'un rapport du moniteur au suivant.

Contrôles à faire à la main, une fois la sortie verte :

- **Second démarrage.** `/sys/config/i18n.cfg`,
  `/sys/config/accessibilite.cfg` et `/sys/i18n/fr-FR/systeme.lang`
  doivent être retrouvés, pas recréés.
- **Démarrage sans framebuffer** (`-vga none`). Les 340 vérifications
  doivent passer quand même : les raccourcis du §16.4 ne dépendent ni du
  compositeur ni d'une session, et c'est ce qui rend vraie la dernière
  phrase du §16.4. La ligne
  `[lecteur] module non demarre - pas de compositeur, donc pas de modules`
  doit apparaître — c'est un diagnostic différent de « pas de sortie
  audio ».
- **Démarrage sans carte son** (retirer `-device AC97`). Le lecteur doit
  se déclarer muet et transcrire quand même sur la console série.
- **`make run-smp`.** L'invalidation de TLB entre cœurs ne s'exerce que
  là, et c'est elle qui a fait fauter le premier binaire étranger.

### Ce qui a été observé, le 5 septembre 2026

Cinq exécutions, toutes menées jusqu'au bout des huit verdicts.

| Exécution | Ce qui a été relevé |
|---|---|
| `make run` | les huit verdicts, `phase 10 validee - 340 verifications` |
| `make run-smp` (4 cœurs) | les huit verdicts, `[smp] 280 invalidation(s) de TLB diffusee(s) - 0 coeur(s) en retard`, relevé identique sur les quatre cœurs |
| premier démarrage, après `make disk-reset` | `[i18n] 5 entree(s) creee(s)`, `[access] 1 entree(s) creee(s), 11 reglage(s) charge(s)` |
| second démarrage | `[i18n] 0 entree(s) creee(s)`, `[access] 0 entree(s) creee(s), 11 reglage(s) charge(s)` — installation idempotente |
| `-vga none` | les 340 vérifications passent quand même, et `[lecteur] module non demarre - pas de compositeur, donc pas de modules` |
| sans `-device AC97` | `[lecteur] pas de sortie audio (aucun controleur AC'97 sur le bus PCI) - annonces sur la console serie seule`, et la transcription série continue |

Les six points de la liste ci-dessus sont sortis conformes :

```
[linux] bonjour depuis un ELF
[linux] PID 57 sort avec le code 0 apres 3 appel(s)
[windows] bonjour depuis un PE
[windows] PID 58 se termine avec le statut 0x0 apres 3 appel(s)
[compat] 2 image(s) chargee(s), 0 refusee(s), 6 appel(s) traduit(s) - 0 vivante(s)
[access] arbre : 3 element(s), 2 focalisable(s) - focus 2
[access] 18 bascule(s), 1 annonce(s)
[secu] 49 acces autorises, 3 refuses, 0 soumis a l'utilisateur
```

Et l'invariant permanent : `[moniteur] frames libres : 123955` sur les
vingt et un rapports qui suivent la fin du boot, `heap : 3949704 o
alloues / 12827512 libres` sur tous. Aucune dérive.

Deux écarts au relevé attendu, tous deux sans conséquence et notés pour
qu'on ne les prenne pas pour des régressions :

- l'autotest de la phase 6 rend **120 ou 121** vérifications, et le
  nombre de cœurs n'y est pour rien. Deux vérifications sont
  conditionnelles :
  - `un pilote pret a ouvert son canal nomme` est comptée une fois par
    pilote **en service**. Sans `-device AC97`, le pilote audio se
    déclare `absent` et la vérification disparaît avec lui — c'est le
    dessin voulu ;
  - `l'interface se reconnait sur son propre lien` n'existe que si la
    négociation DHCP a abouti **avant** que l'autotest n'y arrive. C'est
    une course, pas une régression, et c'est elle qui a fait tomber une
    exécution sous quatre cœurs à 120 alors que la carte son était
    présente. Un compte qui bouge est un mauvais critère de validation :
    à corriger du côté de la phase 6, en faisant attendre l'autotest ou
    en vérifiant la reconnaissance de lien sur une interface construite
    pour le test ;
- au **premier** démarrage, les phases 3-4 et 5 rendent une vérification
  de moins qu'aux suivants (127 et 261 contre 128 et 262) : elles
  comptent un cas qui suppose un volume déjà peuplé.

### Limites ouvertes par la phase 10

- **Les sous-systèmes ne lancent pas les binaires qu'on trouve vraiment
  sur les deux systèmes.** Un binaire de distribution Linux est lié
  dynamiquement ; un programme Windows importe `kernel32.dll`. Les deux
  sont refusés **nommément**, avec la pièce manquante écrite en clair.
  Ce qui s'exécute est la classe des exécutables **autonomes** : un ELF64
  statique et position-indépendant, un PE32+ sans importation système.
  C'est étroit et c'est réel. Lever la limite du côté Linux suppose un
  éditeur de liens dynamique et une libc ; du côté Windows, une
  réimplémentation de `kernel32` et de `ntdll`.
- **Aucun des deux ne reloge.** Un binaire portant des relocations est
  refusé plutôt que corrigé : le format Nova n'a pas de table de
  relocation à lui offrir, et deviner quels octets d'une image étrangère
  sont des adresses n'est pas devinable.
- **`fork` et les threads NT sont déclarés absents**, et pour la même
  raison : les deux supposent un second espace d'adressage, ou un partage
  contrôlé du premier. Toutes les tâches partagent une hiérarchie de pages
  — limite ouverte par la phase 3, toujours ouverte.
- **`mmap` d'un fichier est refusé.** Une projection de fichier promet
  qu'une écriture en mémoire atteint le fichier, et il n'y a pas de cache
  de pages derrière. Répondre avec de la mémoire anonyme serait une
  projection qui perd silencieusement tout ce qu'on y écrit.
- **La synthèse vocale n'a pas de prosodie.** Une descente lente de la
  hauteur, rien d'autre : pas d'intonation de question, pas d'accent
  tonique, pas de liaison. Trois résonateurs, pas cinq — chaque pôle
  supplémentaire est un produit en Q12 par échantillon et par formant, et
  à 48 kHz ce budget est réel.
- **Le lecteur ne lit que de l'ASCII imprimable.** La règle du dépôt
  interdit les accents dans une chaîne affichée, donc le lecteur reçoit
  `fenetre` et prononce la voyelle nue. C'est une perte de qualité réelle,
  et c'est le même compromis que fait la console.
- **L'arbre d'accessibilité n'est peuplé que par le coordinateur.** Les
  applications ne le déclarent pas encore : il manque les opérations du
  protocole graphique qui laisseraient une fenêtre décrire ses éléments.
  Tant qu'elles n'existent pas, Tab ne parcourt que ce que le shell a
  déclaré lui-même.
- **Le zoom republie tout l'écran à chaque trame.** Sous grossissement, un
  rectangle du tampon arrière atterrit sur un rectangle **différent** de
  l'écran, et la correspondance bouge avec le pointeur : publier une zone
  partielle laisserait le reste sur le grossissement précédent, déchiré le
  long de la frontière. Le coût est réel et il n'est payé que par qui
  l'a demandé.
- **L'invite du §12.5 est dessinée par le demandeur.** Un courtier serait
  la forme habituelle et serait la mauvaise ici : l'invite est
  **synchrone**, l'appel système qui l'a déclenchée attend. La
  conséquence est qu'une application qui meurt pendant son invite laisse
  sa surface au ramassage des propriétaires morts du compositeur, pas
  plus.
- **`SYS_MEM_SHARE` échappe encore à la liste noire du §12.3.** La règle
  « pas d'accès mémoire inter-processus » est appliquée sur
  `Resource::ForeignMemory`, que rien n'émet aujourd'hui : le partage
  explicite du 5.4 est un mécanisme consenti, pas une lecture furtive, et
  les distinguer demande de savoir qui a consenti — ce qui suppose la
  table de descripteurs par processus que le noyau n'a pas.
- **Le fuseau horaire n'a pas d'heure d'été.** Une règle de changement
  d'heure est une loi qui change, portée par une base de données mise à
  jour plusieurs fois par an. En figer une copie dans un noyau donnerait
  une réponse fausse la moitié de l'année ; les fuseaux portent leur
  décalage **standard**, et les variantes d'été sont des entrées
  distinctes que l'utilisateur choisit.
- **Nommer une touche de bascule lui prend son autre sens.** Le défaut est
  l'Alt **droit**, qui perd son rôle d'AltGr. C'est un coût réel, et le
  choix inverse — une touche sans glyphe et sans fonction — n'existe pas
  sur un clavier ordinaire.

## Phase 11 — Navigateur « Web Browser » (fork de Firefox) ✅

Décision du 22 septembre 2026 (voir `DECISIONS.md`, « Aucune dépendance
externe ») : le navigateur est un fork de Firefox — Gecko, SpiderMonkey,
WebRender — adapté au format `.cus`, avec une interface propre à Nova. Nom
provisoire du fork : **Web Browser** (la marque « Firefox » est déposée).

Voie retenue : Firefox est compilé **sur l'hôte** pour `x86_64-linux-musl`,
lié statiquement, et exécuté sous la personnalité Linux de la phase 10.4.1,
étendue aux appels qu'il utilise. L'affichage passe par un backend
`widget/nova` dérivé du backend headless de Gecko, le rendu par WebRender
logiciel. Les étapes 11.1 à 11.5 sont le socle du noyau ; aucune ne touche
encore au code de Mozilla.

### 11.1 Mémoire de l'invite
- [x] 11.1.1 QEMU à 4 Gio par défaut (`QEMU_MEM`, `make run QEMU_MEM=512M` pour l'ancienne configuration)
- [x] 11.1.2 Fenêtre physique étendue à toute la RAM détectée (`memory::extend_window`, 1 Gio par page, plafond 64 Gio)

### 11.2 SSE en espace utilisateur
- [x] 11.2.1 SSE armé dans CR0/CR4 sur chaque cœur ; le noyau reste soft-float
- [x] 11.2.2 État FPU/SSE sauvegardé et restauré par tâche (FXSAVE dans `switch_context`)

Relevé préalable (22 septembre 2026) : `firefox-esr` 140.12 d'Alpine,
exécuté sans écran sur l'hôte dans une racine Alpine (`tools/ports/alpine.sh`),
rend une page de test. Sous `strace -f` : **106 appels système distincts**,
123 créations de threads, et des processus de contenu lancés par `fork` +
`execve`. Une construction non « officielle » accepte
`browser.tabs.remote.autostart=false` : le fork visera **un seul
processus**, ce qui retire `fork`, `execve` et le passage de descripteurs
entre processus du périmètre du noyau. Les étapes qui suivent en découlent.

### 11.3 Espaces d'adressage, fautes de page, préemption
- [x] 11.3.1 PML4 par processus (moitié haute partagée), CR3 commuté par l'ordonnanceur
- [x] 11.3.2 Régions et pagination à la demande ; une faute ring 3 termine le processus, pas le cœur
- [x] 11.3.3 Préemption réelle du ring 3 par le minuteur (elle n'avait lieu qu'aux appels système)

### 11.4 Threads
- [x] 11.4.1 Plusieurs tâches dans un même espace d'adressage (`clone` avec `CLONE_VM | CLONE_THREAD`)
- [x] 11.4.2 Base FS par tâche (TLS), futex, `set_tid_address` et `CLONE_CHILD_CLEARTID`

### 11.5 Sous-système Linux, seconde version
- [x] 11.5.1 Chargeur ELF dynamique (`PT_INTERP`, `ld-musl`), pile initiale argv/envp/auxv
- [x] 11.5.2 Mémoire : `mmap`/`munmap`/`mprotect`/`mremap`/`madvise`/`brk`, fichiers projetés
- [x] 11.5.3 Descripteurs : fichiers, tubes, `eventfd`, `epoll`/`poll`, `memfd`, `socketpair`
- [x] 11.5.4 Sockets inet sur la pile réseau de Nova
- [x] 11.5.5 Signaux minimaux, horloges, `getrandom`, `/proc/self`, `uname` — *signaux, horloges et `/proc/self` éprouvés par les programmes de test ; la livraison n'a lieu qu'au retour d'un appel système*
- [x] 11.5.6 `fork` (copie sur écriture), `vfork`, `execve` (y compris `#!`), `wait4`/`waitid`, `SIGCHLD`, `kill` — *ajoutée : Firefox 140 lance ses processus de contenu même en mode mono-processus*

### 11.6 Système de fichiers du navigateur
- [x] 11.6.1 Racine en lecture seule (paquets Alpine) sur son propre disque, montée comme racine Linux — le système de fichiers Nova plafonne un fichier à 2 Mio, `libxul.so` en pèse 150
- [x] 11.6.2 tmpfs pour `/tmp` et le profil

### 11.7 Le fork Web Browser
- [x] 11.7.1 Construction sur l'hôte depuis `ports/web-browser/` (recette Alpine, musl), sans D-Bus, rapport de plantage ni bac à sable, marque « Web Browser » — *`ports/web-browser/build.sh`, `web-browser-140.12.0.en-US.linux-musl-x86_64.tar.xz`, 68 Mio ; le mode mono-processus est abandonné, voir 11.5.6*
- [x] 11.7.2 Démarrage sans écran sous Nova : une page rendue en mémoire

### 11.8 `widget/nova`
- [x] 11.8.1 Surface de fenêtre recopiée vers le compositeur — *`/dev/nova-gui` et `NovaDisplay` (patch `ports/web-browser/nova/`), popups composés*
- [x] 11.8.2 Clavier, souris — *le redimensionnement n'existe pas : la fenêtre garde la taille demandée à l'ouverture*

### 11.9 Interface Nova
- [x] 11.9.1 Interface Nova : thème (`userChrome.css`, palette de `gui::theme`), page d'accueil, DuckDuckGo, français — *couche sur l'interface de Firefox, voir `DECISIONS.md`*

### 11.10 Paquet et mise en service
- [ ] 11.10.1 Paquet `.spm`, installation, signature — **reportée en 12.8.2** : le navigateur est devenu natif, le paquet se fait pour lui
- [ ] 11.10.2 Mesures de performance ; retrait de l'ancien `navigateur.cus` — *mesures faites en 11.11 et 11.15 ; **retrait reporté** : `src/apps/browser/` (`/apps/navigateur.cus`) est toujours compilé et installé*

### 11.11 Vitesse du navigateur, heure du système (étape ajoutée)
Demande du 22 septembre 2026 : le navigateur était « ultra lent » à
démarrer, à chercher et à ouvrir un onglet ; l'heure devait suivre un
fuseau choisi dans les paramètres et se régler par Internet.
- [x] 11.11.1 QEMU sous KVM par défaut quand `/dev/kvm` est là (`QEMU_ACCEL`) ; les trois bogues que KVM a révélés, corrigés (`STAR` sans RPL 3 sur AMD, pile désalignée des exceptions à code d'erreur, invalidations de TLB en vol)
- [x] 11.11.2 Cache de pages de l'image racine, partagé en lecture seule entre processus (`tarfs`, bit `CACHED` de `vm`) ; lectures AHCI de 128 Kio en une commande (`read_to_frames`)
- [x] 11.11.3 Préférences du navigateur : sans Fission, quatre processus de contenu dont un préparé d'avance, services de fond de Mozilla coupés
- [x] 11.11.4 Fuseau horaire par compte, onglet « Date et heure » (`horloge.ss`) ; horloge de la barre des tâches et `/etc/localtime` des programmes Linux à l'heure du fuseau
- [x] 11.11.5 Heure réglée par NTP au démarrage puis toutes les six heures (`net::sntp`), désactivable

### 11.12 Son, vidéo, marque, démarrage (étape ajoutée)
Demande du 22 septembre 2026 : pas de son dans le navigateur, encore un
peu lent, « Mozilla Firefox » qui reparaît, vidéos très buguées,
vérifications système trop longues ; 8 Gio pour l'invité, GPU de l'hôte
autorisé.
- [x] 11.12.1 Son : périphérique `/dev/nova-audio` vers le mixeur, greffon ALSA `nova` (conversions de format et de fréquence), `default` d'ALSA ; cubeb sur ALSA, sans relais AudioIPC
- [x] 11.12.2 Marque : le paquet de langue français réécrit à « Web Browser »
- [x] 11.12.3 Ordonnanceur : la tâche inactive ne prend plus le cœur d'une tâche qui cède ou finit sa tranche ; jamais « affamée »
- [x] 11.12.4 Démarrage : menu GRUB d'une seconde ; profil du navigateur préparé à la construction de l'image
- [x] 11.12.5 QEMU à 8 Gio et 8 cœurs par défaut (`QEMU_MEM`, `QEMU_SMP`) ; vidéo 720p mesurée
- [ ] 11.12.6 GPU de l'hôte — **reportée en phase 13** (pile graphique Nova, pilote dans le noyau)

### 11.13 Pavé numérique, fluidité, vérification du système de fichiers (étape ajoutée)
Demande du 22 septembre 2026 : pavé numérique activable, navigateur encore
lent et vidéos saccadées, dossier `/apps/demo` à supprimer, vérifier que
le système de fichiers est bien fait de liens et non de dossiers réels.
- [x] 11.13.1 Pavé numérique : Verr Num bascule (voyant du clavier), chiffres publiés sous le code de la rangée principale, navigation sans Verr Num, opérateurs toujours
- [x] 11.13.2 `/apps/demo` : l'autotest du système de fichiers retire son échafaudage
- [x] 11.13.3 Échéances des programmes Linux à la milliseconde (cœur 0 à 1 kHz, `idt::fine_ticks`), le tic système restant à 100 Hz
- [x] 11.13.4 Pont de fenêtre réveillé par `PRESENT`, copie de la seule zone modifiée quand le programme la donne
- [x] 11.13.5 `tools/fs-inspect.py` : le disque réel (table de nœuds plate) face à l'arbre vu par les programmes
- [ ] 11.13.6 Firefox ne transmet pas encore sa zone modifiée (`PRESENT` sans rectangle) — **reportée** : reconstruction du fork à faire, à rattacher à la phase 12 (le fork se reconstruit en 12.8.1)

### 11.14 Les fichiers du navigateur dans le système de fichiers Nova (étape ajoutée)
Décision de l'utilisateur du 22 septembre 2026 : « tout dans le FS Nova ».
La racine Linux du navigateur quitte son image tar et son disque séparé
pour devenir des nœuds du volume Nova, à la même racine et avec les mêmes
liens que le reste.
- [x] 11.14.1 Format NovaFS v2 : liens de 128 octets (noms de 120 caractères), deux niveaux d'indirection, fichier jusqu'à 1 Gio
- [x] 11.14.2 `tools/nova-fs.py` : formate, importe une arborescence depuis l'hôte (liens durs et symboliques conservés), liste, extrait, reprend un volume v1
- [x] 11.14.3 `src/linux/vfs/novafs.rs` : la racine Linux est un dossier Nova (`/apps/web-browser/root`), lecture par pages partagées, lecture anticipée ; `tarfs` supprimé
- [x] 11.14.4 Le profil du navigateur vit dans `/users/{compte}/apps/web-browser` et survit à l'extinction
- [x] 11.14.5 L'arborescence de la spec 17 est imposée à chaque démarrage, droits compris, sur un volume rempli par l'hôte
- [x] 11.14.6 Un dossier rend ses blocs de liens vidés, un fichier tronqué rend ses tables d'indirection
- [x] 11.14.7 Un volume au format v1 est laissé intact (jamais formaté) et le système se replie sur un volume en mémoire
- [x] 11.14.8 Une tâche est renommée à l'`execve`, sans quoi le profil crédite le navigateur à `sh`

### 11.15 Latence du navigateur, fluidité vidéo, son (étape ajoutée)
Demande du 23 septembre 2026 : pages chargées en moins de 50 ms, lecture
des vidéos et des fichiers parfaitement fluide, son sans défaut. Mesuré
d'abord contre le même Firefox sur l'hôte (voir `DECISIONS.md`, « Mesurer
le même binaire sur l'hôte et sous Nova »).
- [x] 11.15.1 Identité du cœur par `RDPID`/`TSC_AUX` au lieu du LAPIC en MMIO (appel système nu : 6 800 → 60 ns)
- [x] 11.15.2 LAPIC en x2APIC quand le processeur l'offre ; `kvm-pit.lost_tick_policy=discard` sous KVM (AVIC actif)
- [x] 11.15.3 Invalidations de TLB ciblées sur les cœurs de l'espace (`shootdown_space_and_wait`)
- [x] 11.15.4 Ordonnanceur : réveil sur le cœur même, `current_pid` sans verrou, scrutation avant `hlt`, vidages hors verrou
- [x] 11.15.5 Réception réseau par MSI-X (virtio-net, vecteur 64)
- [x] 11.15.6 Son : seuil de réveil du greffon (fin de la boucle `poll` à vide), délai lu dans le DMA et lissé, tampons courts et amorçage sans trou
- [x] 11.15.7 Pages `file://` dans les processus web (`nova.cfg`)
- [x] 11.15.8 Mesures : `[nova-gui] ecran`, `[profil] N ms apres l'entree`, `[moniteur] reveils Linux`, `/nova-tests/sysbench`, `nova/perf.html`, `nova/bench.html`, `nova/video.html?1080`, `tools/ports/serve-test-pages.sh`
- [x] 11.15.9 `tools/nova-fs.py import` met à jour un volume déjà rempli ; `put`
- [x] 11.15.10 Aucune taille demandée par un programme n'entre entière dans le tas noyau (`sendmsg`, `recvmsg`, `writev`, file de réception TCP) ; un refus est `ENOMEM`, plus l'arrêt du noyau (« ALLOCATION ECHOUEE : 5898240 octets », la coupure réseau signalée le 23 septembre) ; un fichier ordinaire se lit en entier comme sous Linux ; `/nova-tests/events` fait passer 8 Mio par un `socketpair`

### Comment valider l'étape 11.15

**Ce que « moins de 50 ms » peut vouloir dire.** Une page venue
d'Internet ne peut pas arriver plus vite que ses allers-retours réseau :
depuis l'hôte, `curl` met 60 ms pour le premier octet d'`example.com`
(11 ms d'aller-retour, DNS + TCP + TLS + HTTP) et 85 ms pour une page de
Wikipédia (29 ms d'aller-retour). L'objectif de 50 ms est tenu pour ce
que Nova maîtrise : une page locale, une page servie sur le réseau local,
une page déjà connectée ; une page Internet froide reste bornée par son
réseau.

`make` puis `make webroot` (met à jour le navigateur dans le volume ;
l'ancien disque est gardé par l'assistant en
`disks/nova-disk.img.avant-11.15`), puis `make run`. Sur la série :

```
[apic] LAPIC id 0 actif - mode x2APIC
[drv-reseau] virtio-net - ... - reception par interruption MSI-X, vecteur 64
[moniteur] reveils Linux : ... - attente moyenne N us ...        (toutes les 5 s, N de l'ordre de 10 a 40)
```

Puis, dans le navigateur, avec `tools/ports/serve-test-pages.sh` lancé
sur l'hôte :

| Adresse tapée | Ligne à observer | Attendu |
|---|---|---|
| `file:///usr/lib/web-browser/nova/perf.html` (2e fois) | `[nova-gui] titre "perf ..." N ms apres l'entree` | N < 50 |
| `http://10.0.2.2:8123/perf.html` (2e fois) | idem | N < 50 |
| `https://example.com` (2e fois) | `titre "Example Domain ..."` | ~50-60 ms (réseau : 11 ms d'aller-retour) |
| `file:///usr/lib/web-browser/nova/video.html?720`, un clic | `titre "vp9-720p t=10.0 images 301 perdues N fin"`, `[nova-audio] flux 0 ferme - ... 0 trou(s) ... 0 arret(s) du DMA`, `[nova-gui] ecran : ... ecarts ... <38 M` | N ≤ 3 ; aucun trou ; M proche du nombre d'images |
| `...video.html?1080`, `...video.html?mp4` | idem | idem |
| `.../son.html`, un clic | la de 3 s audible, `titre "Son joue a 44100 Hz"` | son net, même après une vidéo |

Au démarrage, dans l'autotest 11.5 :
`events: pipe, eventfd+epoll (.. ms), poll, SCM_RIGHTS, message de 8 Mio conformes`.
Plus aucun `ALLOCATION ECHOUEE` en navigation prolongée (vidéos, gros
téléchargements) ; s'il reparaissait, la ligne donne maintenant la tâche
et le cœur.

Chaque titre est suivi d'une ligne `[profil]` qui dit où est passé le
temps (calcul par thread, appels système, fautes, invalidations). Le
micro-banc : `/nova-tests/sysbench` depuis une image de test, à comparer
à `build/perf/sysbench` lancé sur l'hôte.

À éprouver aussi : `make run QEMU_SMP=1`, `make run-smp`,
`make run QEMU_ACCEL="-accel tcg"` (replis sans x2APIC ni `RDPID`),
`-vga none`, sans AC97.

**Relevés de l'assistant (23 septembre 2026, confirmés par
l'utilisateur le 24 septembre).** Sur une copie du disque mis à jour, KVM, 8 cœurs, son
capté par `-audiodev wav` : première image du navigateur 1,07 à 1,45 s
(2,4 s avant) ; page locale 24-35 ms (58-66 avant) ; même page servie par
l'hôte 33-35 ms (81) ; `example.com` 55 ms à chaud (113) ; Wikipédia
« Paris » 99-203 ms. Vidéo : VP9 720p 1 à 2 images perdues sur 301, VP9
1080p 1 sur 301, H.264 0 à 1 sur 300, aucun trou de son, aucun arrêt du
DMA, 2 100 `poll` au lieu de 140 000 ; la plupart des images à 32-34 ms
d'intervalle. Micro-banc, Nova puis hôte (ns) : appel système nu 43-54 /
49, `futex` sans attente 57-66 / 109, faute + `munmap` 1 155-1 379 /
1 340, `mprotect` aller-retour 1 857-1 912 / 946, aller-retour par tube
entre deux threads 2 700-3 400 / 3 000 (6 800, 31 000, 43 000 et 95 000
avant). Autotests verts sous 1, 4 et 8 cœurs, TCG, `-vga none`, sans
AC97, x2APIC forcé ou non, hors l'export ci-dessous.

**Non couvert.**
- Page Internet froide : bornée par ses allers-retours (voir plus haut).
- Une navigation reste 3 à 5 fois plus longue que le même Firefox sans
  écran sur l'hôte (35-50 ms contre 6-15 au banc `bench.html`) ; la
  comparaison n'est pas à armes égales (l'hôte ne peint rien), mais
  l'écart restant n'est pas localisé.
- Régularité vidéo : selon la phase entre la grille des images et le
  vsync logiciel de 60 Hz de Firefox (délais en millisecondes), 2 à 30 %
  des images d'une fenêtre de 5 s arrivent un rafraîchissement trop tard
  puis trop tôt (paires 50/17 ms). Une minuterie à la microseconde
  (TSC-deadline) pour les attentes Linux est la piste, non faite.
- Pas de vDSO : `clock_gettime` reste un appel système (85 ns, 20 sur
  l'hôte) — 60 % des appels d'une navigation.
- Pas de PCID : un changement d'espace vide le TLB ; les tâches noyau
  chargent la PML4 d'amorçage (pas de « lazy TLB »).
- `hlt` scruté 50 µs : coût nul à vide, mesurable sous charge légère.
- Toujours pas d'AVX (`CR4.OSXSAVE` à 0) : les décodeurs vidéo tournent
  en SSE.
- **Préexistant, non traité ici** : l'autotest d'export de la phase 4
  échoue (« plus de bloc libre ») sur un disque où l'on a navigué — il
  exporte `/users`, qui contient depuis 11.14 le profil du navigateur et
  son cache HTTP, sur des supports de 96 Mio.
- Le double démarrage d'un disque neuf n'a pas été rejoué : aucun code
  d'installation ni de format n'a changé ; le volume mis à jour par
  `make webroot` a été vérifié par l'autotest de cohérence (83 réussites,
  les 3 échecs étant l'export ci-dessus).

### Ce qui a été observé pour 11.1, le 22 septembre 2026

Relevés de l'assistant, QEMU sans fenêtre sous `timeout`, `-snapshot`,
carte son muette. Les dix verdicts d'autotest sont verts dans les trois
configurations, et le premier `make run-smp` de l'utilisateur (4 Gio) les
donne verts aussi.

```
-m 4G, -smp 4 : [frame] 1046599 frames libres (4088 Mio) - fenetre physique de 5 Gio
-m 512M       : [frame] 129095 frames libres (504 Mio) - fenetre physique de 4 Gio
-m 8G, -smp 4 : [frame] 2095175 frames libres (8184 Mio) - fenetre physique de 9 Gio
```

`make disk-reset`, `-vga none` et l'absence de carte son n'ont pas été
relancés pour ce changement : il ne touche ni au disque, ni à l'affichage,
ni aux pilotes.

### Ce qui a été observé pour 11.2, le 22 septembre 2026

Relevés de l'assistant, `-m 4G`, monocœur puis `-smp 4`, les dix autres
verdicts verts dans les deux cas :

```
[fpu] SSE arme - etat x87/SSE sauve a chaque commutation (FXSAVE)
[fpu] 3 taches x 2000 commutations - 0 etat(s) corrompu(s)
[test] phase 11.2 validee - 5 verifications de l'etat SSE par tache
```

Contre-épreuve : le même noyau avec `fxsave`/`fxrstor` remplacés par des
`nop` donne `4000 etat(s) corrompu(s)` et `[test] phase 11.2 ECHEC` —
le test voit bien ce qu'il prétend voir.

### Ce qui a été observé pour 11.3, le 22 septembre 2026

Relevés de l'assistant, `-m 4G`, monocœur puis `-smp 4` (le cœur de la
boucle diffère, le reste est identique) ; tous les autres verdicts verts :

```
[vm] moitie haute figee - 253 entree(s) PML4 creees, espaces prives prets
[vm] 256 pages touchees : 257 cadres residents, 257 fautes resolues
[idt] tache 69 : exception #14 (Faute de page) en ring 3 a RIP 0x4000e4 - 0x0 - tache terminee
[idt] tache 70 : exception #14 (Faute de page) en ring 3 a RIP 0x4000f4 - 0x30000000 - tache terminee
[vm] boucle ring 3 sans appel systeme sur le coeur 2 - voisin execute
[vm] fautes : 271 resolues, 1 rattrapees, 2 fatales - espaces vivants : 0 - cadres libres 1026725 -> 1026724
[test] phase 11.3 validee - 20 verifications des espaces d'adressage prives
```

Contre-épreuve : sans la préemption du ring 3 dans le gestionnaire du
minuteur, la même boucle donne `voisin affame`, et le test reste bloqué.

**Non couvert par 11.3** : seules les tâches créées par `vm::spawn_user`
ont un espace privé. Les images Nova de `binfmt::loader` partagent
toujours la PML4 d'amorçage et ses arènes. Le ring 0 reste coopératif.
Les appels système Nova d'une tâche à espace privé valident puis copient
sans passer par `user_copy` : un autre thread qui démapperait la page
entre les deux ferait fauter le noyau (le sous-système Linux, lui, passe
par `user_copy`).

### Ce qui a été observé pour 11.4 à 11.6, le 22 septembre 2026

Huit programmes Linux compilés contre musl dans la racine Alpine
(`tools/ports/tests/*.c`), placés dans l'image racine
(`make webroot`, `tools/ports/webroot.sh`) et lancés par l'autotest à
chaque démarrage. Relevé de l'assistant, `-m 4G -smp 4` :

```
[fs] "QEMU HARDDISK" (1447 blocs) - image racine du sous-systeme Linux, reservee
[tarfs] image racine : 192 noeuds, 72 fichiers, 5 Mio, indexee en 0 tics (48 blocs lus sur 1447)
threads: 8 threads, 160000 increments sous verrou, TLS et conditions conformes
memory: 1 Gio reserve, mprotect, mremap, 1000 malloc, brk conformes
files: tmpfs, image racine (35 entrees dans /etc), liens, /proc, memfd conformes
events: pipe, eventfd+epoll (49 ms), poll, SCM_RIGHTS conformes
signals: gestionnaire, pile alternative, masque, EINTR, EPIPE conformes
[linux] /nova-tests/dynamic charge - base 0x555555554000, entree 0x7efffffc41ae, interprete a 0x7efffff58000
bonjour depuis musl, lie dynamiquement
spawn: fork COW, memfd partage, execve, posix_spawn, kill, waitpid, 4 SIGCHLD conformes
[test] phase 11.5 validee - 8 programme(s) Linux conformes, 0 absent(s)
```

Le même relevé en monocœur donne le même verdict (hors `spawn`, ajouté
après). **Non couvert** : les sockets IPv4 (11.5.4) n'ont pas encore de
programme de test ; les signaux ne sont livrés qu'au retour d'un appel
système ; une faute synchrone (SIGSEGV) tue le processus sans appeler de
gestionnaire.

### Ce qui a été observé pour 11.7 à 11.9, le 22 septembre 2026

**Sans écran (11.7.2).** Le Firefox 140.12 d'Alpine, lancé par
`/nova/autostart.sh`, rend une page de test et l'imprime sur la série en
base64 (`tools/ports/extract-shot.sh`) : texte, JavaScript exécuté
(« JavaScript actif : 42 »), processus de contenu lancés par
`fork`/`execve` et terminés proprement :

```
[linux] processus 183 (firefox-esr) termine - code 0
[linux] processus 220 (firefox-esr) termine - code 0
[linux] processus 103 (firefox-esr) termine - code 0
autostart: navigateur termine, code 0
```

**En fenêtre (11.8, 11.9).** Le fork `web-browser`, lancé depuis l'entrée
« navigateur.cus » du menu (session `admin`, captures d'écran par QMP,
`tools/ports/test-window.sh` et `tools/ports/qmp.py`) :

```
[apps] lancement du Web Browser - PID 102
[nova-gui] surface 13 - 1229x691
```

Observé à l'écran : interface Nova en français, page d'accueil Nova ;
saisie au clavier AZERTY dans la barre d'adresse ; `example.com` en HTTPS
(cadenas) ; recherche DuckDuckGo complète ; menu du moteur de recherche
(popup) affiché et cliquable ; suggestions en direct. Trois démarrages en
parallèle après le correctif de la copie sur écriture : trois fenêtres,
aucune faute deux minutes plus tard.

**Régression.** Monocœur, `-smp 4`, `-vga none`, sans carte son : tous les
verdicts verts. Après `make disk-reset` : le second démarrage est vert ;
le premier (volume neuf) échoue une vérification de l'explorateur, « les
blocs libres reviennent a leur valeur », deux fois sur deux. Elle mesure
les blocs libres de tout le disque pendant que d'autres autotests y
écrivent ; l'ordre des tâches a changé avec la préemption réelle. **Non
corrigée** : à reprendre.

**Non couvert.** La fenêtre ne se redimensionne pas. Le profil vit en
tmpfs : marque-pages, historique et cookies disparaissent à l'extinction
(le système de fichiers Nova plafonne un fichier à 2 Mio). Seul le bouton
gauche de la souris est transmis (pas de menu contextuel). Pas de son,
pas de vidéo matérielle. Les performances n'ont pas été mesurées ; sous
TCG le démarrage prend environ une minute. L'ancien `navigateur.cus` reste
le repli sans image racine ; 11.10 (paquet `.spm`, retrait) est ouvert.

### Ce qui a été observé pour 11.11, le 22 septembre 2026

Relevés de l'assistant : QEMU sans fenêtre sous `timeout`, `-snapshot`,
série dans un fichier, image racine construite par
`NOVA_AUTOSTART_BROWSER=1 tools/ports/webroot.sh alpine-baselayout
alpine-release musl busybox firefox-esr font-dejavu ca-certificates tzdata`,
session `admin` ouverte par QMP, puis `ctrl-t`, une recherche tapée dans la
barre d'adresse, un second onglet, `fr.wikipedia.org`. Hôte : Ryzen 9 9900X.

**Où passait le temps.** Mesuré avant toute correction, sous KVM : première
image du navigateur à 9,1 s, dont 6,5 s de disque — 88 336 blocs de 4 Kio
lus (345 Mio), chaque processus relisant sa propre copie de `libxul.so` un
bloc par commande. Sous TCG (l'ancienne configuration de `make run`), 19 s ;
en monocœur TCG, bien davantage. Après le cache de pages :

```
[nova-gui] premiere image 3167 ms apres le lancement
[profil] image tar : 46255 blocs lus sur le disque en 307 ms - 45263 pages en cache, 79598 succes
```

Un second lancement dans la même session : `premiere image 1978 ms`.

**Pourquoi les recherches « ne répondaient pas ».** Le processus de contenu
chargé de DuckDuckGo mourait dans le `malloc` de musl
(`faute fatale 0x10 ... RIP ... "ld-musl-x86_64.so.1"`) : l'onglet restait
sur son sablier. Le programme de test `/nova-tests/mmstress` reproduit la
cause (trois fois sur trois avant correctif, zéro après) :

```
mmstress: 0x7efffffb7000 mot 0 : 0x0 au lieu de 0x20000000000085a
```

puis, corrigé :

```
mmstress: 4 thread(s), mode m, 6000 tours, pages et blocs passes de main en main intacts
[test] phase 11.5 validee - 12 programme(s) Linux conformes, 0 absent(s)
```

**Latences, trois démarrages après correctifs** (titre de la page reçu,
depuis la touche Entrée) :

```
[nova-gui] titre "frame nova at DuckDuckGo - Mozilla Firefox" 636 ms apres l'entree
[nova-gui] titre "Mozilla Firefox" 36 ms apres l'entree
[nova-gui] titre "Wikipedia, l'encyclopedie libre - Mozilla Firefox" 349 ms apres l'entree
```

Monocœur : 609 ms, 34 ms, 334 ms ; première image à 3,5 s.

**Heure.** À chaque démarrage avec réseau :

```
[ntp] horloge reglee sur pool.ntp.org (5.196.76.84) : 2026-09-22 15:20:18 UTC - correction 641 ms, aller-retour 329 ms
```

Fuseau « UTC+02:00 - Europe centrale (ete) » choisi dans l'onglet
« Date et heure » : `[parametres] horloge.ss : fuseau = Text("Europe/Paris-ete")`,
la barre des tâches passe de 15:17 à 17:17, et le navigateur relancé
affiche 17:19 sur sa page d'accueil (heure calculée par JavaScript, donc
par ICU via `/etc/localtime`). Côté Linux, au lancement :
`autostart: heure locale 2026-09-22 15:17:06 UTC - fuseau /usr/share/zoneinfo/UTC`.

**Régression.** Monocœur, `-smp 4`, `-vga none`, sans AC97 : tous les
verdicts verts. `make disk-reset` puis deux démarrages : le second retrouve
tout (`applications installees : 0 ecrite(s)`) ; le premier échoue encore
la vérification déjà connue de l'explorateur (« les blocs libres reviennent
a leur valeur »), sans lien avec cette étape.

**Non couvert.** Pas d'heure d'été automatique : les zones restent à décalage
fixe, avec leur variante « (ete) » à choisir (voir `i18n::tz`) ; le
navigateur reçoit le même décalage fixe (`Etc/GMT-2`) pour afficher la même
heure que la barre des tâches. Un changement de fuseau n'atteint un
programme Linux qu'à son prochain lancement. L'horloge CMOS n'est pas
réécrite. Le minuteur reste à 100 Hz : un délai de Firefox est arrondi à
10-20 ms (les tranches de temps de la spec 7 sont exprimées en tics ; en
changer la fréquence est une décision à prendre). Un `.cus` n'a que la
moitié haute des cœurs : deux sur quatre pour le navigateur. Sans Fission,
les sites ne sont plus isolés les uns des autres dans des processus
distincts. Le titre des fenêtres porte encore « Mozilla Firefox » (marque
du fork à reprendre dans sa construction). Les deux plantages de `-cpu host`
observés avant le correctif des invalidations n'ont plus reparu sur onze
démarrages, ce qui n'est pas une preuve.

### Ce qui a été observé pour 11.12, le 22 septembre 2026

Relevés de l'assistant, mêmes conditions qu'en 11.11 ; le son est capté
par QEMU dans un fichier (`-audiodev wav`) et analysé sur l'hôte.

**Son.** Page d'essai `nova/son.html` (un clic, un la de 3 s) :

```
[nova-audio] flux 0 ouvert (1 ouvert(s))
[nova-gui] titre "Son joue a 44100 Hz - Web Browser" 156 ms apres l'entree
```

Fichier capté : 3 s de signal à **440,0 Hz**, crête 7 832 (0,3 × 32 767 ×
80 % de volume de session), moins de 1 % de l'énergie hors du pic. Trois
obstacles levés en route : le greffon compilé sans `-DPIC`
(`snd_dlsym_start: symbol not found`) ; cubeb n'essayant que PulseAudio
(`media.cubeb.backend`) ; la couche `plug` d'ALSA refusant la latence de
2 ms de la sonde de Firefox (« Output number of channels is 0 ») — le
greffon convertit désormais lui-même. À chaque démarrage, en silence :

```
alsa: sonde de Firefox conforme - 2 canaux au plus
alsa: 22050 trames jouees par le peripherique "default"
```

Sans carte AC97 : `alsa: aucune carte son - peripherique declare absent`.

**Vérifications système** (horodatage côté hôte, 8 cœurs) : écran de
connexion à **10,1 s** au lieu de 37,9 s. Le test SSE de 11.2 prenait
20 s — 2 000 `yield` d'une tâche seule sur son cœur, chacun livré à la
tâche inactive jusqu'au tic suivant ; il prend 0,02 s. Le même défaut
coûtait jusqu'à un tic à chaque fin de tranche de tout programme seul sur
son cœur.

**Navigateur, 8 cœurs, profil préparé** : `premiere image 1759 ms` (2 871
avant le profil) ; nouvel onglet 110-137 ms, recherche DuckDuckGo 678 ms,
Wikipédia 305-377 ms. Plus aucun « Firefox » : titres
`"frame nova at DuckDuckGo - Web Browser"`.

**Vidéo** (`nova/video.html`, clip VP9 1280x720 30 i/s, titre = images
affichées et perdues) :

```
1 coeur  : vp9-720p t=1.0 images 260 perdues 242
4 coeurs : vp9-720p t=10.0 images 301 perdues 15
8 coeurs : vp9-720p t=10.0 images 301 perdues 7 fin
8 coeurs : h264 t=10.0 images 300 perdues 12 fin
```

Sur un seul cœur — l'ancien `make run` — la vidéo s'effondre : c'est ce
qui était vu. D'où 8 cœurs par défaut.

**Régression.** 1, 4 et 8 cœurs, `-vga none`, sans AC97 : tous verts.
`make disk-reset` et deux démarrages : le second retrouve tout ; le
premier échoue la vérification connue de l'explorateur.

**Non couvert.** Le GPU de l'hôte n'est pas utilisé : il faudrait soit
le confier entier à l'invité (VFIO) avec un pilote NVIDIA ou AMD écrit
pour Nova, soit virtio-gpu 3D (virgl/venus) avec une pile Mesa et DRM
portée — des chantiers d'une autre taille que cette étape ; le rendu reste
WebRender logiciel. Le son ne passe que dans un sens (pas de micro pour le
navigateur). La conversion de fréquence est cubique, pas un filtre
polyphasé : correcte à l'oreille, pas de qualité studio. YouTube n'a pas
été essayé (bandeau de consentement à cliquer) : la mesure porte sur des
clips locaux. Le profil préparé ajoute 33 Mio à l'image et se recopie en
mémoire à la première ouverture de chaque session.

### Ce qui a été observé pour 11.13, le 22 septembre 2026

Relevés de l'assistant, QEMU sans fenêtre, écran virtio-gpu forcé à
1920x1080, 8 cœurs, clip VP9 1280x720 30 i/s de `nova/video.html?720`.

**Vidéo, série A/B propre** (deux ISO, exécutions alternées, une seule
machine virtuelle à la fois) :

```
100 Hz : perdues 11 / 27 / 37 sur 301
1 kHz  : perdues  9 /  1 /  2 sur 301
[nova-gui] 30 images/s - copie moyenne 305 us
```

La copie du pont coûte 0,3 ms par image : ce n'était pas le goulot. Le
goulot était l'arrondi des minuteries : à 100 Hz, un délai de 33 ms
tombait à 40-50 ms.

**Deux pistes écartées, mesurées.** Répartir les fils réveillés sur les
cœurs libres de leur plage : aucun gain mesurable, retiré. Et un piège de
mesure : `tools/ports/test-window.sh` laisse sa machine virtuelle tourner ;
enchaînées, les mesures se disputaient l'hôte. Toutes les mesures de
cette section sont faites une machine à la fois.

**Pavé numérique** (touches QEMU `kp_*`) :

```
[nova-gui] titre "42+1 at DuckDuckGo - Web Browser" 721 ms apres l'entree
[clavier] pave numerique inactif
```

Verr Num coupé, `kp_8` et `kp_4` déplacent le curseur au lieu de taper.
Autotest : 5 vérifications de plus en phase 6 (128).

**Système de fichiers** (`tools/fs-inspect.py disks/nova-disk.img`) :
291 nœuds dans une seule table, 175 fichiers et 116 tables de liens ;
291/291 atteints par au moins un lien ; `/sysapp` est le nœud 2, une
table contenant `reseau.ss -> 271`, dont les octets sont aux blocs
428-430. Conforme à la spec 4.1.

**Régression.** 1, 4, 8 cœurs, `-vga none`, sans AC97 : tous verts.
`make disk-reset`, deux démarrages : `/apps` ne contient plus `demo` ; le
premier démarrage échoue toujours la vérification connue de l'explorateur.

**Non couvert.** Firefox recopie et présente encore toute sa fenêtre à
chaque image (le noyau sait désormais n'en copier qu'une partie, le fork
ne la donne pas) ; la fenêtre du navigateur garde sa taille d'ouverture ;
le rendu reste logiciel. Les fichiers du navigateur ne sont pas dans le
système de fichiers Nova mais sur une image tar à part (voir question
ouverte ci-dessous). Verr Num n'est pas mémorisé d'une session à l'autre.

### Ce qui a été observé pour 11.14, le 22 septembre 2026

Relevés de l'assistant, QEMU sans fenêtre sous `timeout`, 8 Gio, 8 cœurs
sauf mention, disque de 2 Gio formaté et rempli depuis l'hôte
(`tools/nova-fs.py format` puis `import`), supports amovibles neufs.

**L'écriture du navigateur dans le volume, depuis l'hôte** (0,6 s) :

```
[ok] 1941 fichiers, 339 dossiers, 542 liens supplementaires, 486 Mio ecrits dans /apps/web-browser/root
```

**Premier démarrage sur ce volume :**

```
[fs] "QEMU HARDDISK" (524288 blocs) monte en / - volume existant
[fs] arborescence systeme creee - 34 dossiers (spec 17)
[fs]   /users/admin/ - 18 dossiers, role ADMIN
[fs]   premier demarrage sur ce volume - le compteur est cree a 1
[fs] autotest termine : 91 reussites, 0 echecs
```

**Second démarrage du même disque** — rien n'est recréé, le contenu est
retrouvé :

```
[fs] 1 volume(s) monte(s) - 2048 Mio, arborescence spec 17 en place
[fs]   demarrage n 2 - le volume conserve son contenu depuis 1 extinction(s)
[fs] autotest termine : 92 reussites, 0 echecs
[spm] arborescence prete - 0 dossier(s) cree(s), depot local deja declare
```

**Deux fuites de blocs corrigées, vues par les autotests :**

```
[explorer]   blocs libres : 395263 avant, 395262 apres (1 de difference)
[fs] INCOHERENCE : noeud 2788 garde une table d'indirection inutile
```

La première venait d'un dossier qui ne rendait pas un bloc de liens vidé,
la seconde d'un fichier tronqué qui gardait sa table. Après correction,
les deux autotests passent aux deux démarrages.

**La racine Linux est écrivable** — `nova-tests/files` vérifie maintenant
qu'un fichier écrit dans `/etc` se relit par un autre descripteur puis
disparaît à l'`unlink` (il vérifiait l'inverse du temps de `tarfs`) :

```
[linux] /nova-tests/files : code 0 en 1 tics - conforme
```

**Navigateur.** Première image 2,6 s sur un disque neuf (profil à créer),
2,1 s au démarrage suivant, profil retrouvé sur le disque :

```
[linux] /home/nova sur /users/admin/apps/web-browser - le profil survit a l'extinction
[nova-gui] premiere image 2091 ms apres le lancement
[profil]     141 tics  coeur 4  web-browser
```

Les 141 tics sont le processus parent de Firefox : 1,4 s de calcul d'un
seul fil, le reste des cœurs est au repos. Le démarrage n'est donc plus
tenu par les entrées-sorties. Le profil ne montrait ces tics que depuis
le renommage des tâches à l'exec ; ils étaient crédités à `sh`.

**Vidéo**, clip VP9 1280x720 30 i/s, lecture en temps réel :

```
[nova-gui] titre "vp9-720p t=10.0 images 301 perdues 4 fin - Web Browser" 13623 ms apres l'entree
```

**Extinction propre puis relecture** : `[power] caches disque ecrits`,
puis au démarrage suivant `demarrage n 3 - le volume conserve son contenu
depuis 2 extinction(s)`.

**Disque au format v1** (copie du disque de l'utilisateur, 64 Mio) :

```
[fs] "QEMU HARDDISK" (16384 blocs) - volume Nova OS au format 1, ce noyau ecrit le format 2 - laisse intact, reprendre avec `make disk-migrate`
[fs] aucun volume de demarrage utilisable - repli sur un volume en memoire de 8 Mio
```

Empreinte du disque identique avant et après le démarrage
(`ff58b525a6a856fcfc009e32a5c3ad17`). Le système démarre, mais sur
8 Mio : l'installation d'un paquet et l'export FAT32 manquent de place,
et rien ne survit à l'extinction. C'est un mode dégradé, pas un mode de
travail.

**Régression.** 1, 4 et 8 cœurs, `-vga none`, sans AC97, et deux
démarrages du même disque : aucun `ECHEC` ni `INCOHERENCE` dans les six
sorties série. Sans carte son : `[nova-audio] ouverture refusee : aucune
carte son` — le pilote se déclare absent.

**Non couvert.** Le disque de l'utilisateur (`disks/nova-disk.img`) est
encore au format 1 et de 64 Mio : il faut `make disk-migrate` puis
`make webroot`, ou `make disk-reset` pour repartir d'un volume neuf de
2 Gio. Un volume écrit avant le correctif des tables d'indirection garde
ses tables inutiles jusqu'à réécriture. Firefox ne transmet toujours pas
sa zone modifiée à `PRESENT`. Le rendu reste logiciel, le GPU n'est pas
utilisé. Les supports amovibles d'export se remplissent d'un démarrage à
l'autre : au bout de quelques essais, les trois exports échouent faute de
place, et il faut les remettre à zéro.

### Comment valider l'étape 11.1

`make run` (4 Gio par défaut). Sur la série :

```
[frame] PHYS_OFFSET = 0xffff800000000000 - fenetre physique de 5 Gio
```

Avec `-m 4G`, le firmware laisse 3 Gio sous le trou PCI et place le
dernier gibioctet à partir de 4 Gio : la fenêtre doit donc annoncer 5 Gio,
et `[frame] ... libres` environ 4 000 Mio au lieu d'environ 500. Les
verdicts d'autotest doivent rester verts. À éprouver aussi :
`make run QEMU_MEM=512M` (fenêtre de 4 Gio, rien ne change), `make run-smp`
(les cœurs secondaires partagent la même PDPT), et `QEMU_MEM=8G`.

**Non couvert par 11.1** : un pilote dont le DMA est limité à 32 bits
(AC97) refuse déjà une trame au-dessus de 4 Gio ; les autres (AHCI,
e1000, virtio, xHCI) écrivent les 64 bits de l'adresse. Aucun allocateur
de trames « sous 4 Gio » n'existe : un pilote AC97 initialisé tard, sur
une mémoire déjà entamée, pourrait se déclarer indisponible.

---

## Phase 12 — Navigateur natif : libc Nova 🔧 (implémenté, en attente de validation)

Décision de l'utilisateur du 23 septembre 2026 : le navigateur doit être un
programme **natif** de Nova, sans passer par le sous-système Linux. Voie
choisie : la **libc Nova** (spec §6.7) — musl dont les fonctions appellent
l'API Nova. Firefox se compile pour `x86_64-linux-musl` comme aujourd'hui,
se lie à la libc Nova au lieu de celle d'Alpine, et devient une **image liée**
dans un `.cus` (spec §3.5). Aucun appel Linux ne franchit plus `syscall`
pour lui. Le sous-système Linux reste, pour les autres programmes (spec §1).
C'est la méthode de Redox (relibc), Haiku ou SerenityOS pour le même
logiciel.

Pourquoi l'ABI utilisateur de musl est gardée : types, constantes et
numéros d'erreur identiques, c'est ce qui permet de compiler Firefox, ses
bibliothèques et son code Rust (crate `libc`, cible `linux-musl`) **sans les
modifier**. Seule la frontière avec le noyau change.

### 12.1 Spécification
- [ ] 12.1.1 Relecture et accord : spec §3.5 (image liée), §6.7 (domaines 09 `SYS_OBJ_*`, 0A `SYS_THREAD_*`, 0B `SYS_SIG_*`, extensions de 00, 02, 03, 07, 08), §13.2
- [ ] 12.1.2 Accord sur le code tiers : la libc Nova reprend musl (licence MIT), en espace utilisateur, rattachée à l'exception du 22 septembre ; le noyau reste sans dépendance

### 12.2 Noyau : les nouveaux domaines
- [ ] 12.2.1 Descripteurs et objets (`SYS_OBJ_*`) sur les objets déjà écrits pour le sous-système Linux (`linux::fd`, `linux::vfs`, tubes, compteurs d'événements, mémoire partageable, attente multiple) — la personnalité Linux devient une traduction au-dessus des mêmes objets, pas une seconde copie
- [ ] 12.2.2 Threads et attente sur un mot (`SYS_THREAD_*`), en nanosecondes
- [ ] 12.2.3 Projection mémoire (`SYS_MEM_MAP` à `SYS_MEM_BARRIER`) sur `vm::Space`
- [ ] 12.2.4 Processus : `FORK`, `EXEC`, `WAIT_ANY`, `CWD`, `LIMITS`
- [ ] 12.2.5 Sockets BSD (`SYS_NET_SOCKET` à `SYS_NET_ADDRESS`), transmission de descripteurs
- [ ] 12.2.6 Signaux (`SYS_SIG_*`), y compris la remise d'une faute à son gestionnaire (limite ouverte de 11.5)
- [ ] 12.2.7 Temps : horloges en nanosecondes, **page de temps** (l'heure sans appel système : le vDSO qui manquait en 11.15)
- [ ] 12.2.8 Autotest de la phase 12, chaque appel par `syscall::invoke`

### 12.3 Chargeur d'image liée (spec §3.5)
- [ ] 12.3.1 En-tête `.cus` avec drapeau `IMAGE_LIEE`, ABI 2 ; projection de l'ELF, interpréteur
- [ ] 12.3.2 `tools/nova-wrap.py` : ELF → `.cus` (en-tête, empreinte, signature SELF ou ENTERPRISE)

### 12.4 libc Nova
- [ ] 12.4.1 musl de la version d'Alpine, architecture `x86_64-nova` : ses appels système deviennent des appels Nova ; `syscall()` brut traduit
- [ ] 12.4.2 Éditeur de liens dynamique (celui de musl) installé dans `/sys/runtimes/c/`
- [ ] 12.4.3 Racine de compilation (`build/nova-sysroot`) construite sur l'hôte, dans la racine Alpine
- [ ] 12.4.4 Les programmes de `tools/ports/tests/` recompilés contre elle tournent en `.cus` avec **zéro appel Linux** (compteur par processus)

### 12.5 Chaîne de compilation
- [ ] 12.5.1 Enveloppes `nova-cc`, `nova-c++` ; Rust `x86_64-unknown-linux-musl` lié à la libc Nova
- [ ] 12.5.2 Un programme Rust avec threads, fichiers et réseau, construit ainsi, conforme

### 12.6 Périphériques natifs
- [ ] 12.6.1 `/sys/dev/fenetre` et `/sys/dev/son`, mêmes ABI que `/dev/nova-gui` et `/dev/nova-audio`, ouverts par `SYS_OBJ_OPEN`
- [ ] 12.6.2 Son : greffon ALSA reconstruit contre la libc Nova, ou pilote `nova` dans cubeb (choix à faire à l'étape)

### 12.7 Dépendances du navigateur
- [ ] 12.7.1 Firefox en boîte à outils `cairo-headless` (le widget `NovaDisplay` est déjà bâti sur le widget sans écran) : plus de GTK ni de X11
- [ ] 12.7.2 Bibliothèques restantes (fontconfig, freetype, expat, zlib, alsa-lib si gardée…) reconstruites contre la libc Nova

### 12.8 Le navigateur natif
- [ ] 12.8.1 Construction du fork contre la racine de compilation Nova
- [ ] 12.8.2 Emballage `.cus` et paquet `.spm` signé (spec §10), installé dans `/apps/web-browser/`, lancé par le menu sans la racine Alpine
- [ ] 12.8.3 **Zéro appel Linux** de tous ses processus sur une session complète (pages, vidéo, son)
- [ ] 12.8.4 Mesures au moins égales à 11.15 (pages, vidéo, son) ; la racine Linux du navigateur retirée du volume

### État au 23 septembre 2026 — observé, non coché (validation de l'utilisateur attendue)

Le navigateur démarre et tourne en **programme natif** : `web-browser.cus`
(image liée, en-tête vérifié, empreinte SHA-256 recalculée à chaque
lancement), bibliothèque C `/sys/runtimes/c/libc.so` (musl 1.2.6 d'Alpine,
ses quatre correctifs et sa recette, plus la couche d'appels Nova). Aucun
processus du navigateur ne passe par le sous-système Linux.

**Ce qui a été fait, et comment cela s'écarte du plan.**
- 12.2 : les domaines 09, 0A, 0B et les extensions sont servis par
  `syscall/ported.rs`, sur les objets de `linux::*` (descripteurs, VFS,
  futex, sockets, `epoll`, espaces privés) : une traduction au-dessus des
  mêmes objets, comme prévu. Numéros ajoutés à la spec §6.7 (0x0215-0x0218,
  0x0315, 0x0316, 0x0914, 0x0A09, 0x0A0A) avec la convention d'appel
  (erreurs en `-errno`). **Non faits** : la page de temps (12.2.7, 0x0709
  réservé) et l'autotest appel par appel (12.2.8) ; les programmes natifs
  de `/nova-tests/natif` en tiennent lieu pour l'instant.
- 12.3 : chargeur d'image liée et `tools/nova-wrap.py`, faits.
- 12.4 : libc et éditeur de liens faits (12.4.1, 12.4.2). **12.4.3 (racine
  de compilation) n'a pas été nécessaire** : le chargeur de musl répond
  lui-même à tous les noms `libc.*`, donc les binaires dynamiques d'Alpine
  (Firefox, GTK, le greffon ALSA) tournent sur la libc Nova sans
  recompilation (DECISIONS, « La libc Nova est aussi l'éditeur de liens »).
  12.4.4 : 11 programmes conformes, dont `database` (SQLite en WAL).
- 12.5 (`nova-cc`, Rust) et 12.7 (retrait de GTK, bibliothèques
  reconstruites) : **non faits**, et plus indispensables pour être natif.
- 12.6.1 : `/sys/dev/fenetre` et `/sys/dev/son` existent ; Firefox ouvre
  toujours `/dev/nova-gui` et `/dev/nova-audio` (mêmes objets, par
  `SYS_OBJ_OPEN`). 12.6.2 : le greffon ALSA d'Alpine tourne tel quel.
- 12.8.2 : `.cus` fait, paquet `.spm` **non fait**. 12.8.4 : la racine
  Alpine reste sur le volume, pour ses fichiers (bibliothèques, polices,
  `omni.ja`) ; plus aucun appel ne passe par elle.

**Observé** (KVM, 8 cœurs, `build/perf/disk-final.img`, scénario QMP de la
11.15 : pages locales, HTTP de l'hôte, example.com, Wikipédia, vidéos VP9
720p et 1080p, H.264, son ; six sessions après correctifs) :
- `[test] phase 12 validee - 11 programme(s) natifs conformes, 0 absent(s), aucun appel Linux`
  sous 1, 4 et 8 cœurs, TCG, `-vga none`, sans AC97 ;
- `[natif] Web Browser lance - /usr/lib/web-browser/web-browser.cus`,
  `[nova-gui] premiere image 925 à 1271 ms apres le lancement` ;
- page locale `perf rep=0 dcl=9 à 32`, 17 à 49 ms de la touche au titre
  (premier chargement à froid jusqu'à 114 ms) ; VP9 720p, VP9 1080p, H.264 :
  `images 221 à 232 perdues 0 à 3` en 7,6 s ; son `Son joue a 44100 Hz` ;
- `[moniteur] syscalls : ... programmes portes 4767752, sous-systeme Linux 125259`
  (le compte Linux est celui des autotests du démarrage, il ne bouge plus
  pendant la session) ; aucune ligne `appel 0x.. inconnu - ENOSYS`.

**Défauts trouvés et corrigés en chemin** (détail dans DECISIONS) :
- le point d'entrée des fautes de page n'effaçait pas le drapeau de
  direction : une copie à rebours de musl faisait écrire au noyau 4 Kio
  dans la frame voisine (deux sessions natives sur six perdaient un
  processus, une fois le noyau lui-même) ;
- `munmap` libérait l'objet projeté avant l'invalidation des TLB ; tmpfs
  libérait au `truncate` des pages encore projetées ;
- `mmap(MAP_SHARED, PROT_WRITE)` d'un fichier du volume était refusé :
  toute base SQLite créée par le profil échouait (`SQLITE_IOERR_SHMMAP`),
  sous Linux comme en natif ;
- `unlink` sur novafs n'oubliait pas le cache du nœud supprimé.

**Encore ouvert.**
- Une session sur six, le processus lancé à la première vidéo meurt dans
  son initialisation (saut dans une page de `libva-drm.so.2`, registres
  identiques d'une fois sur l'autre) ; **plus aucune vidéo ne joue ensuite
  dans la session** (`t=0.0 images 0`), le son si. Pas vu par la
  personnalité Linux (0 sur 6). Cause non trouvée. Vu 2 fois sur 12
  sessions complètes avant la dernière série ; **0 sur les 6 sessions
  complètes et 22 passages ciblés de la dernière série** (noyau final).
  Une fois aussi, le processus parent s'est arrêté lui-même au démarrage
  (`RustMozCrash`, panique Rust de Firefox, message non imprimé). Le
  rapport de faute imprime désormais les chaînes pointées par rdi/rsi/rdx
  (le message de `RustMozCrash`), les adresses de retour de la pile avec
  leur bibliothèque et les 32 derniers appels du thread : la prochaine
  occurrence dira où.
- La page de temps, l'autotest appel par appel, `nova-cc`, le paquet `.spm`,
  le retrait de la racine Alpine : voir ci-dessus.
- Un programme **statique** ne peut pas être natif (il embarque sa musl).

### État au 23 septembre 2026, soir — observé par l'assistant, non coché

Demandes de l'utilisateur : certificats « tous invalides » dans le
navigateur ; « sortir Firefox de la dépendance Linux et mettre dans notre
noyau les dépendances nécessaires pour que ça fonctionne nativement sans
traduction » ; DNS Cloudflare ; « parfois la connexion coupe pour tout
l'OS ». Détail et raisons dans `DECISIONS.md` (quatre entrées du 23
septembre, soir).

**Fait.**
- **Noyau : `src/linux/` → `src/posix/`.** Les objets des programmes
  (processus, threads, descripteurs, fichiers, sockets, projections,
  signaux, horloges, chargeur, arbre de fichiers, fenêtre, son) sont le
  module `posix` ; `syscall::ported` (numéros Nova) et `linux::syscall`
  (numéros Linux) sont deux tables devant eux. `src/linux/` ne contient
  plus que la table Linux, `spawn` en `Abi::Linux`, le script de démarrage
  de l'image et l'autotest Linux. Aucun chemin d'un programme `Abi::Nova`
  ne traverse `crate::linux` ; `uname` lui répond « Nova ». Plus de repli
  du navigateur par `/bin/sh` et la personnalité Linux.
- **Réseau** : table TCP de 16 → 256 connexions pour tout le système
  (c'était la coupure : 14 prises par Firefox après quatre sites) ;
  références de connexion à génération (un RST ne fait plus fermer la
  connexion d'un autre) ; Firefox plafonné à 128 connexions (`nova.cfg`).
- **DNS** : Cloudflare `1.1.1.1`, puis `1.0.0.1` ; le DNS du bail DHCP
  n'est plus retenu ; un serveur saisi dans les paramètres gagne et
  survit au redémarrage.
- **AHCI** : une commande qui expire ou échoue est arrêtée (ST, CR) avant
  d'être abandonnée ; le DMA ne peut plus atterrir dans des frames rendues.
- **Diagnostic** : à chaque faute fatale d'un programme, ses pages du cache
  de pages sont comparées au disque (`[vm]   cache de pages : ...`).

**Observé** (KVM, disque de l'utilisateur copié, `-snapshot`) :
- `[net] 10.0.2.15 par DHCP - passerelle 10.0.2.2, DNS 1.1.1.1` et
  `[net] bail de 86400 s ... - DNS du bail 10.0.2.3 non retenu` ;
- neuf sites dans un onglet (DuckDuckGo, Google, GitHub, YouTube,
  Wikipédia, Le Monde, Hacker News, Reddit, example.com) : tous affichés,
  cadenas partout, jusqu'à `[tcp] 28 connexion(s)` ouvertes à la fois ;
  `[moniteur] syscalls : ... sous-systeme Linux 116869` identique du
  premier au dernier relevé de la session (ce sont les appels des
  autotests du démarrage) ;
- 18 démarrages avec ouverture du navigateur, 0 plantage (voir ci-dessous) ;
- autotests du démarrage sous 1 cœur, 4 cœurs, `-vga none`, sans AC97 et
  sous TCG : `phase 11.5 validee - 13 programme(s) Linux conformes` et
  `phase 12 validee - 11 programme(s) natifs conformes, 0 absent(s), aucun
  appel Linux` partout, `DNS 1.1.1.1` partout ; sans écran, 7.1 et 7 ne
  tournent pas (attendu). Le seul `ECHEC - 3 cas incorrects sur 131` est
  l'autotest d'export de la phase 4 sur un disque où le navigateur a
  servi (défaut connu) ; absent sur un disque neuf ;
- `make disk-reset` (disque à part) puis deux démarrages : `3 dossier(s)
  cree(s), depot local declare`, puis `0 dossier(s) cree(s), depot local
  deja declare` ; 11 verdicts valides, aucun échec, les deux fois.

**Certificats.** Non reproduit : sur une copie du disque de l'utilisateur,
les sites s'ouvrent avec un certificat valide. Une session sur quatre
avant les 18 démarrages ci-dessus, le processus parent de Firefox est
mort au démarrage en chargeant `libnssckbi.so` (le module des autorités
racines) : `.eh_frame` lu avec un contenu qui n'est pas celui du fichier.
Un échec de ce chargement qui ne tue pas le processus laisse NSS sans
autorités racines : tout certificat devient invalide. C'est l'explication
la plus probable du symptôme signalé, **non prouvée**. La cause de la
page fausse n'est pas trouvée ; la prochaine occurrence dira, par le
diagnostic ajouté, si une frame du cache a été écrasée.

**Speedtest (signalé ensuite : « ALLOCATION ECHOUEE : 1440 octets, alignement
1 - tache 42 sur le coeur 0 »).** Le tas du noyau (64 Mio) portait les piles
de 128 Kio des ~300 threads de Firefox : 43 Mio au lancement du navigateur.
Piles désormais en frames contiguës ; budget TCP compté en mémoire réservée,
files rendues une fois lues, copie hors ordre faillible (DECISIONS). Observé
(KVM, 8 cœurs, disque de l'utilisateur copié) : speedtest.net va au bout,
`689.86` Mbit/s en réception, `37.14` en émission ; `[moniteur] heap :` 18
Mio une page ouverte, 23 Mio au plus fort (contre 55 Mio avant). Autotests
sous 1 et 4 cœurs, `-vga none`, sans AC97, TCG (deux fois) : 11.5 et 12
validées, phase 9 validée après la correction de son contrôle de frames.

**Noms introuvables, speedtest « server fetch failed » (signalé ensuite, avec
captures).** Table UDP de 16 sockets pour tout le système : 15 prises
pendant un speedtest, 14 sur neuf sites ; la dix-septième privait le
résolveur de Firefox de socket. 256 sockets, références à génération,
files bornées en octets, copie faillible, table pleine signalée
(DECISIONS). Observé : deux speedtests d'affilée (894 et 754 Mbit/s en
réception) puis YouTube, Google, DuckDuckGo, Wikipédia, GitHub, tous
affichés ; au plus 18 sockets UDP et 59 connexions TCP ouvertes ; aucune
ligne `table pleine`, aucun datagramme perdu. Autotests sous TCG (deux
fois) et monocœur : phase 6 validée (128), 11.5 et 12 validées.

**Téléversement lent (signalé ensuite : 18 Mbit/s contre 278 sous le Linux de
l'hôte).** `write` sur une socket TCP attendait l'acquittement de tout ce
qu'il envoyait, 64 Kio en vol au plus. Émission désormais asynchrone : tampon
d'émission, segments libérés par les acquittements, fenêtre de congestion,
horloge de retransmission dans la tâche du pilote réseau, `close` qui livre
ce qui reste (DECISIONS). Observé (KVM, 8 cœurs) : speedtest.net `215.29` et
`182.96` Mbit/s en envoi ; page locale postant 200 Mio vers un serveur de
l'hôte : 390 à 695 Mbit/s reçus par le serveur selon l'exécution. Non égalé :
les 278 Mbit/s de l'hôte sur speedtest.net ; la pile n'en est plus la limite
(voir DECISIONS, « L'émission TCP est asynchrone »).

**Plantages au chargement d'une bibliothèque : cause trouvée.** Au deuxième
plantage de `libnssckbi.so`, le diagnostic du cache a vérifié 16 765 pages
contre le disque, 0 altérée ; la cause était `MAP_FIXED` fait en deux prises
du verrou (une autre projection pouvait s'insérer entre les deux). Corrigé ;
20 démarrages avec le navigateur, 0 plantage ; autotests sous TCG (deux fois)
et monocœur, 11.5 et 12 validées. Explication retenue des certificats
invalides, non prouvée (le symptôme n'a jamais été reproduit).

**Charge (demandée ensuite) : 5 vidéos YouTube, 3 sites, un speedtest, 16 Gio.**
`QEMU_MEM` passe à 16 Gio (`16375 Mio` libres, fenêtre physique de 17 Gio) ;
8 cœurs, comme avant. Observé (KVM, disque de l'utilisateur copié) : cinq
vidéos dans cinq onglets (dont Big Buck Bunny 4K 60 i/s et Costa Rica 4K 60
i/s), Wikipédia, GitHub, Le Monde, puis speedtest.net `496.22` / `75.97` Mbit/s
avec les vidéos en fond ; aucune faute, aucune `ALLOCATION ECHOUEE`, aucune
table pleine ; tas du noyau à 30 Mio sur 64 en fin de session ; autotests
verts (sauf l'export connu). Fluidité, onglet vidéo au premier plan, tout
chargé (`[nova-gui] ecran`, par 5 s) : Big Buck Bunny 60 i/s, 261 à 299
images, écarts sous 55 ms ; une vidéo 8K 60 i/s (publicité comprise) ~30 i/s
avec des écarts de 75 à 120 ms ; les vidéos à 30 i/s, 150 à 190 images, sans
trou. **Pas « ultra fluide » pour du 60 i/s en très haute définition** : le
décodage VP9/AV1 et WebRender sont logiciels, sur les quatre cœurs de la
moitié haute (spec 7.2).

**Encore ouvert.**
- GPU : pile propre à Nova, pilote dans le noyau (décisions de
  l'utilisateur) ; roadmap en phase 13, rien d'implémenté.
- Les certificats invalides : explication retenue ci-dessus, à confirmer
  par l'usage.
- Firefox reste une construction Linux (`x86_64-linux-musl`, GTK) qui
  tourne sur la libc Nova ; `cairo-headless` n'existe plus dans Firefox
  140 (12.7.1 impossible telle qu'écrite) ; retirer GTK est un portage de
  plateforme (`widget/nova` complet, cible Nova chez Mozilla et dans les
  crates Rust).
- Les fichiers du navigateur restent dans `/apps/web-browser/root`
  (paquets Alpine) ; Firefox ouvre toujours `/dev/nova-gui` et
  `/dev/nova-audio` (servis par `posix::vfs::devfs`, les mêmes objets que
  `/sys/dev/fenetre` et `/sys/dev/son`).

### Ce que la phase 12 ne couvre pas, dès le départ
- Le sous-système Linux n'est pas supprimé : il reste la couche de
  compatibilité de la spec §1 pour les autres programmes.
- Ce qui touche au code de Mozilla reste minimal par principe (boîte à
  outils sans écran, pilote son éventuel) ; un portage des couches
  internes de Firefox sur l'API Nova sans libc a été écarté (des milliers
  de sites dans le code, dont les crates Rust).
- Taille : plusieurs sessions de travail. Chaque étape se valide à part.

## Phase 13 — Pile graphique Nova et accélération GPU 🔧 (en cours)

Demande de l'utilisateur du 23 septembre 2026 : « exploiter la puissance de
calcul des cartes graphiques pour un affichage plus fluide, avec beaucoup
moins d'images manquantes » — pas un pilote par carte, un support qui marche
quelle que soit la carte.

**Décisions de l'utilisateur (23 septembre 2026, soir)** — voir `DECISIONS.md`,
« La pile graphique est à Nova » :
- **aucun pilote ni aucune pile graphique existants** : ni Mesa, ni libdrm,
  ni l'interface DRM de Linux, ni pilote tiers. Nova écrit **sa propre pile**,
  « dédiée, rapide et sûre » ;
- **le pilote est intégré au noyau par défaut** ;
- **les pilotes dédiés à une carte réelle** (NVIDIA, AMD, Intel) sont
  l'affaire des fabricants ou des utilisateurs : Nova leur fournit
  l'interface pour les écrire, pas les pilotes ;
- **c'est Firefox qui s'adapte à la pile de Nova**, pas l'inverse : c'est
  pour cela que le navigateur est un fork reconstruit et non un binaire
  Linux installé. Le fork reçoit un moteur de rendu Nova ; Nova n'imite pas
  OpenGL pour lui ;
- **le but d'ensemble** : un système entièrement à Nova, sans dépendre de
  Linux ni de pilotes propriétaires. La phase 13 y contribue pour le
  graphisme ; ce qui reste de Linux ailleurs est listé en fin de phase.

### Ce qui est imposé de l'extérieur

Sous QEMU, le seul chemin vers la carte de l'hôte est **virtio-gpu 3D** : un
périphérique virtuel dont l'hôte exécute les commandes sur sa carte (la RTX
5070 Ti ici, n'importe quelle autre ailleurs). Nova en écrit le pilote ; ce
qui ne se choisit pas, c'est **le format des commandes que l'hôte
comprend** — l'un des deux protocoles 3D de virtio-gpu :
- **Venus** : des commandes Vulkan sérialisées, shaders en SPIR-V ; exécutées
  par le pilote Vulkan de l'hôte. Explicite, moderne, rapide. Gros
  (plusieurs centaines de commandes), mais **généré** : le format est décrit
  dans le registre public de Vulkan (`vk.xml`), un outil Nova en tire
  l'encodeur Rust à la construction.
- **virgl** : des états Gallium et des shaders TGSI, exécutés par l'OpenGL de
  l'hôte. Plus ancien, plus lent, format peu documenté hors de
  virglrenderer.

**Retenu : Venus** (décision de l'utilisateur, 24 septembre 2026). Son modèle (tampons de commandes, pipelines, mémoire
explicite, barrières) est celui qu'une pile rapide et sûre veut exposer, et
ses shaders sont du SPIR-V, un format de fichier standard — pas un pilote.
Ce n'est pas utiliser la pile de quelqu'un : c'est parler le protocole du
périphérique, comme la pile réseau parle TCP. virgl est écarté.

### Ce qui a été vérifié (23 septembre 2026)

- Hôte : QEMU 11.1.1, virglrenderer 1.3.0 (Venus compris), pilote NVIDIA
  615.71, RTX 5070 Ti (16 Gio) ; Radeon intégrée aussi présente.
- Nova démarre avec `-device virtio-vga-gl,hostmem=8G,blob=true,venus=true
  -display egl-headless,rendernode=/dev/dri/renderD129` ; le pilote 2D
  actuel y fonctionne tel quel (`[virtio-gpu] mode 1280x800`). `renderD128`
  est la **Radeon**, `renderD129` la **RTX** : à désigner par chemin PCI.
- Les BAR 64 bits sont placés au-dessus de la RAM (`virtio-net ... BAR4
  0xe0200004000`) : la région de 8 Gio y a sa place — à confirmer (13.3.1).
- Le pilote Nova ne connaît aujourd'hui que 8 commandes 2D
  (`graphics/virtio_gpu/proto.rs`).

### Les « 8 Go de VRAM »

Sous virtio-gpu, l'invité n'a pas de VRAM propre. `hostmem=8G` est une
**fenêtre de mémoire partagée** (un BAR PCI de 8 Gio) où l'hôte projette les
ressources « blob » — c'est là que vivent les tampons et images GPU de Nova,
sans copie. Venus en a besoin. La mémoire de la carte reste gérée par l'hôte
(16 Gio sur la RTX).

### Architecture visée

```
programmes natifs (.ss/.cus/.gs)      Firefox (fork) : WebRender, backend
        |                             de peripherique Nova (13.10)
        |                                   |
        +------ bibliotheque nova-gfx (Rust, en-tete C) ------+
                           |                                  |
                  domaine SYS_GPU_* (0C)             compositeur (noyau)
                           |                              |
                 coeur GPU du noyau : memoire, files, barrieres,
                 isolation, reprise, ecran     <----------+
                           |
          interface de backend (pilote de peripherique)
             |                                   |
   backend virtio-gpu (integre)        backends de fabricants (tiers)
             |
   encodeur Venus (genere de vk.xml)
```

- **Cœur GPU** (indépendant du périphérique) : objets mémoire comptés par
  processus, files de commandes et leur ordonnancement, barrières, isolation,
  délai de garde et reprise, propriété de l'écran.
- **Backend** : ce qu'un pilote de carte doit fournir. Le backend virtio-gpu
  est intégré ; l'interface est documentée pour les fabricants.
- **API Nova** (`SYS_GPU_*`) : explicite, dans l'esprit de Vulkan mais
  réduite à ce que Nova sert ; shaders en SPIR-V validé.
- Le **compositeur**, dans le noyau, est un client direct du cœur : c'est lui
  qui profite le premier du GPU, pour toutes les applications.

### Les vrais problèmes, par ordre de risque

1. **Faire dessiner Firefox avec la pile Nova — la difficulté principale.**
   WebRender (le moteur de rendu de Firefox, en Rust) ne parle aujourd'hui
   qu'à OpenGL. Relevé dans le source du fork (140.12) : tout son accès au
   GPU passe par `gfx/wr/webrender/src/device/` (5 141 lignes, dont 215
   appels OpenGL dans `gl.rs`), et ses shaders sont **50 fichiers GLSL**
   connus d'avance (`webrender/res/`). Le travail, dans le fork :
   - un **backend de périphérique Nova** à côté de `device/gl.rs` : textures,
     cibles de rendu, tampons, programmes, dessins, lectures — traduits en
     appels `nova-gfx`. Ce n'est pas une émulation d'OpenGL : WebRender a
     déjà une notion de périphérique, on lui en donne un second ;
   - **les shaders compilés à la construction du fork** : les 50 GLSL
     traduits en SPIR-V par la chaîne de construction (outil hôte, comme
     clang), embarqués dans `libxul` ; aucun compilateur de shaders dans
     Nova ;
   - côté Gecko, un **`RenderCompositorNova`** (la famille existe :
     `RenderCompositorEGL`, `RenderCompositorSWGL`...) qui présente l'image
     GPU au compositeur de Nova sans copie ;
   - la **sélection du GPU** : Gecko décide du rendu matériel dans
     `gfxPlatform` (`IsHeadless`, lignes 2578 et 2620 : le mode sans écran
     le refuse), la liste de blocage et `glxtest` — tous à patcher pour
     Nova ;
   - **suivre les versions** de Firefox : le patch touche le cœur du rendu,
     chaque nouvelle ESR demandera de le reporter.

   Conséquence : **WebGL** (shaders arbitraires fournis par les pages) reste
   sans GPU tant que Nova n'a pas de compilateur de shaders à l'exécution.
   Sans GPU (vrai PC sans pilote, TCG), Firefox garde son rendu logiciel
   actuel (SWGL).
2. **Firefox en mode sans écran.** `NovaDisplay` est bâti sur le widget
   headless ; le rendu matériel doit y être permis sans réveiller le reste
   de ce qu'implique `MOZ_HEADLESS` (pas de GTK, pas de X11). À terme, le
   fork aura son propre widget (`widget/nova`, voir « Ce qui reste de Linux »
   plus bas).
3. **L'encodeur Venus.** Générer l'encodeur et le décodeur des réponses depuis
   `vk.xml`, gérer l'anneau de commandes partagé de Venus et ses objets ; le
   protocole est public mais se lit surtout dans virglrenderer (on le lit, on
   n'en reprend pas le code).
4. **Barrières et interruptions.** Rendu asynchrone : chaque soumission porte
   une barrière que l'hôte signale par interruption. Le pilote actuel scrute
   sa file : il faut MSI-X (comme virtio-net) et des barrières attendables,
   en descripteurs (`poll`, `epoll`).
5. **Sûreté dans le noyau.** Un pilote GPU dans le noyau agrandit sa surface :
   tout ce qu'un programme soumet (commandes, SPIR-V, tailles) est validé
   avant d'atteindre l'hôte ; aucune ressource d'un autre processus
   atteignable ; un programme qui boucle sur le GPU est coupé (délai de
   garde, destruction de son contexte) sans bloquer l'écran.
6. **Mémoire partagée hôte-invité.** Projeter la fenêtre de 8 Gio par morceaux
   dans les processus (write-combining), la rendre après invalidation des
   TLB, partager une image entre processus (Firefox passe ses tampons entre
   ses processus).
7. **Vidéo.** Le **décodage** VP9/AV1 reste logiciel (VA-API est une interface
   Linux, exclue ; Vulkan Video par Venus : plus tard). Le gain vient de ce
   que conversion YUV→RGB, mise à l'échelle et composition passent au GPU et
   rendent les cœurs au décodeur.
8. **Replis.** Sans GL sur l'hôte, sous TCG, `-vga none`, sur un vrai PC : le
   cœur GPU se déclare « absent » et tout reste sur le rendu actuel.
9. **La spec (§1.2) met les pilotes hors du noyau.** La décision de
   l'utilisateur (pilote intégré au noyau) est à écrire dans la spec en 13.1,
   avec la place des backends de fabricants (intégrés, ou modules signés).

### Étape préalable, sans GPU (gain rapide)

- [ ] 13.0.1 Mesure de référence : pour une vidéo 1080p60 au premier plan,
  le temps de calcul par fil (décodeur, `Renderer` WebRender, compositeur)
  et `[nova-gui] ecran` — ce qui dira quelle part le GPU peut reprendre
- [ ] 13.0.2 `gdt::MAX_CPUS` à 16 et l'invité à 16 cœurs : le navigateur
  passe de 4 à 8 cœurs (moitié haute, spec 7.2 inchangée) ; même scénario

### 13.1 Spécification et décisions

Spec écrite le 24 septembre 2026, **acceptée le même jour** (l'utilisateur
a demandé la mise en place complète après l'avoir reçue) : §1.2 (exception
du pilote GPU), §3.6 (module `.kmd` signé), §6.8 (domaine `SYS_GPU_*`),
§8.5 (pile graphique), §17 et §18. Les deux points « à confirmer » ont pris
la valeur recommandée — modules SELF autorisés un par un par un
administrateur, délai de garde de 2 s — **à confirmer explicitement**
(DECISIONS, « La spécification de la phase 13 est acceptée »). Ce que la
spec laissait ouvert (structures, format du tampon de commandes, limites,
conteneur `.kmd`, version de l'interface) y est maintenant écrit.

- [ ] 13.1.1 Spec §8.5 « Pile graphique » : cœur GPU, interface de backend,
  domaine `SYS_GPU_*` (0C), format des shaders (SPIR-V validé), barrières en
  descripteurs, partage d'images, isolation, délai de garde, repli
- [ ] 13.1.2 Spec §1.2 : le pilote GPU dans le noyau (décision de
  l'utilisateur) ; les backends de fabricants sont des **modules signés
  chargés au démarrage** (décision du 24 septembre 2026) : format du module,
  signature vérifiée (§3.2, niveaux de confiance), refus journalisé d'un
  module non signé ou altéré
- [ ] 13.1.3 Protocole hôte : **Venus** (décidé) ; outil de génération
  depuis `vk.xml` ; compilation des shaders de WebRender à la construction
  (outil hôte)

### 13.2 Configuration QEMU
- [ ] 13.2.1 `QEMU_GPU` dans le Makefile : `virtio-vga-gl,hostmem=8G,blob=true,venus=true`,
  `-display gtk,gl=on` pour `make run`, `egl-headless` + nœud de rendu de la
  RTX (par chemin PCI) pour les essais ; `QEMU_GPU=` redonne la 2D
- [ ] 13.2.2 Sans GL sur l'hôte, `make run` garde `-vga virtio` et le dit

### 13.3 Backend virtio-gpu 3D (noyau)
- [ ] 13.3.1 Négociation `RESOURCE_BLOB`, `CONTEXT_INIT` ; `GET_CAPSET_INFO`
  / `GET_CAPSET` (capset Venus) ; région de mémoire partagée (capacité PCI
  `SHARED_MEMORY_CFG`) trouvée et projetée
- [ ] 13.3.2 Contextes, ressources blob, `SUBMIT_3D`
- [ ] 13.3.3 Barrières : interruptions MSI-X de la file de contrôle
- [ ] 13.3.4 Autotest : contexte, ressource, soumission vide, barrière
  signalée, destruction ; sans 3D, « absent », pas échec

### 13.4 Cœur GPU (noyau)
- [ ] 13.4.1 Interface de backend (trait), documentée pour les fabricants
- [ ] 13.4.1b Chargement d'un backend en module signé au démarrage : signature
  vérifiée avant la moindre instruction ; le backend virtio-gpu reste
  intégré au noyau
- [ ] 13.4.2 Objets mémoire comptés par processus, projection, partage,
  libération après invalidation
- [ ] 13.4.3 Files de commandes par contexte, ordonnancement équitable,
  délai de garde, reprise d'un contexte fautif
- [ ] 13.4.4 Propriété de l'écran : sortie d'affichage et curseur

### 13.5 Encodeur Venus
- [ ] 13.5.1 Outil `tools/gen-venus` : encodeur et décodeur Rust depuis
  `vk.xml` ; seules les commandes que Nova expose sont générées
- [ ] 13.5.2 Anneau de commandes Venus, objets Vulkan de l'hôte suivis par
  le noyau (création, destruction, fuite impossible à la mort d'un
  processus)

### 13.6 API des programmes
- [ ] 13.6.1 Domaine `SYS_GPU_*` : contexte, mémoire, images, pipelines,
  tampons de commandes, soumission, barrières
- [ ] 13.6.2 Validation de tout ce qui est soumis (bornes, objets du
  processus, SPIR-V bien formé et limité aux capacités annoncées)
- [ ] 13.6.3 Bibliothèque `nova-gfx` (Rust, et un en-tête C)

### 13.7 Premier rendu
- [ ] 13.7.1 `/nova-tests/gpu` : programme natif qui dessine un triangle,
  relit l'image et la vérifie ; **aucune bibliothèque tierce, aucun appel
  Linux**
- [ ] 13.7.2 Autotest du démarrage : même vérification, verdict série

### 13.8 Compositeur sur le GPU
- [ ] 13.8.1 Surfaces des fenêtres en images GPU, composées sur le GPU,
  sortie d'affichage directe ; curseur sur la file curseur — écart : la
  sortie est une ressource 2D adossée à la mémoire GPU, l'affichage direct
  d'un blob sortant noir sur cet hôte (DECISIONS)
- [ ] 13.8.2 Loupe et filtres de couleur (§16) : sur le GPU ou sur le CPU,
  à trancher à l'étape — tranché : sur le processeur, lisant l'image que le
  GPU a composée (DECISIONS)
- [ ] 13.8.3 Mesure : `[nova-gui] ecran` et CPU du compositeur, avant et après

### 13.9 Firefox : présentation sans copie
- [ ] 13.9.1 `NovaDisplay` présente une image GPU partagée au compositeur
  (nouvelle commande de `/dev/nova-gui`), d'abord remplie par le rendu
  logiciel actuel : le chemin d'écran est prouvé avant le rendu GPU

### 13.10 Firefox : WebRender sur la pile Nova (dans le fork)
- [ ] 13.10.1 Inventaire : ce que `device/gl.rs` demande réellement au GPU
  (formats, types de textures, états, lectures), et les 50 shaders
- [ ] 13.10.2 Chaîne de construction du fork : GLSL de WebRender → SPIR-V,
  embarqué ; vérification que chaque shader passe la validation de Nova
- [ ] 13.10.3 Backend de périphérique Nova dans WebRender, sur `nova-gfx`
  (liaison Rust directe : WebRender et `nova-gfx` sont tous deux en Rust)
- [ ] 13.10.4 Gecko : `RenderCompositorNova`, sélection du rendu matériel
  sur Nova (`gfxPlatform`, liste de blocage, `glxtest`), repli SWGL sans GPU
- [ ] 13.10.5 Mesure sur le scénario de charge du 23 septembre ; WebGL
  toujours sans GPU (dit)

### 13.11 Validation de la phase
- [ ] 13.11.1 Scénario de charge (5 vidéos, onglets, speedtest) : aucune
  faute ; vidéo 1080p60 au premier plan à plus de 58 i/s, moins de 1 % des
  écarts au-delà de 22 ms
- [ ] 13.11.2 Replis : sans GL hôte, TCG, `-vga none`, `QEMU_SMP=1`, sans
  AC97, `disk-reset` et deux démarrages — rien ne casse
- [ ] 13.11.3 Sûreté : un programme qui soumet des commandes invalides, qui
  boucle sur le GPU, qui meurt avec des objets GPU : l'écran continue, rien
  ne fuit

### État au 24 septembre 2026 — observé par l'assistant, non coché

Relevés de l'assistant, KVM (sauf mention), disque de l'utilisateur copié
(`-snapshot`), `-device virtio-vga-gl,hostmem=8G,blob=true,venus=true
-display egl-headless` sur le nœud de rendu de la RTX 5070 Ti. **Aucune case
n'est cochée : la validation de l'utilisateur est attendue** (CLAUDE.md,
2.7). Firefox (13.0, 13.9, 13.10, 13.11.1) est reporté par l'utilisateur
(« ce n'est que plus tard qu'on adaptera firefox ») ; tout le reste de la
phase est fait.

```
[gpu] pret en 91 ms - NVIDIA GeForce RTX 5070 Ti (10de:2c05), 16303 Mio locaux, 62670 Mio visibles, images jusqu'a 16384, partage oui
[gui] compositeur sur le GPU - 1280x800, sortie en memoire hote, pret en 96 ms
[gpu]   triangle 64x64 : centre [255, 0, 0, 255], coin [0, 0, 255, 255] - contexte ouvert en 96 ms, rendu et relu en 0 ms
[gpu]   programme natif /nova-tests/natif/gpu : code 0 en 24 tics
gpu-rust: centre 0 255 0 255, coin 0 0 127 255
[natif] processus 161 (gpu-rust) termine - code 0 - 45 appels Nova, aucun appel Linux
[gpu]   composition 320x200 : 255 instances, 0 pixel(s) differents du processeur
[gpu]   ecran au coeur GPU : oui - plan curseur : oui
[test] phase 13 validee - 75 verifications de la pile graphique, sur le GPU
[gpu] anneaux Venus : 546 lot(s) ecrits, 12 reveil(s) du fil de l'hote, 0 session(s) sans anneau
```

`make venus-check` : `[ok] encodeur Venus identique a la reference - 68
commandes` (les 65 d'avant et les trois de l'anneau). `make` : aucun
avertissement du noyau ; `make check` : chaînes ASCII, aucun symbole non
résolu, SSE seulement dans l'autotest volontaire `fpu::worker`.

**Ce qui est fait, étape par étape.**
- 13.1, 13.2, 13.3 : voir la spec et les relevés précédents (transport 3D,
  capset Venus, région partagée, MSI-X, `QEMU_GPU`/`QEMU_RENDER`).
- 13.4.1, 13.4.1b, 13.4.2, 13.4.3 : interface de backend, modules `.kmd`
  signés, mémoire comptée et projetée, files par contexte, délai de garde.
- **13.4.4 — l'écran et le curseur au cœur** (`src/gpu/screen.rs`,
  `graphics/virtio_gpu/scanout.rs`) : seul un propriétaire noyau met une
  mémoire de son contexte à l'écran ; la sortie est une ressource 2D dont le
  support est cette mémoire (région partagée) — l'affichage direct d'un
  blob sort noir sur cet hôte (DECISIONS). Le pointeur passe par le **plan
  curseur** (file curseur virtio, deux images alternées) : bouger la souris
  ne redessine plus rien. Vérifié par VNC (pseudo-encodage curseur) :
  `[ok] curseur 64x64, point chaud (0, 0), 83 pixels opaques`.
- **13.5.1** : générateur, 68 commandes vérifiées octet par octet.
- **13.5.2 — l'anneau Venus** (`src/gpu/venus/ring.rs`) : chaque session
  écrit ses lots dans un anneau en mémoire partagée que lit un fil du
  moteur de rendu de l'hôte, sans requête virtio par lot ni passage par la
  boucle principale de QEMU ; `vkNotifyRingMESA` seulement quand ce fil
  s'est endormi (12 réveils pour 546 lots). La fin du travail GPU est lue
  dans un **mot de fin** que le GPU écrit en dernier (`backend/feedback.rs`)
  : la barrière virtio arrivait 10 ms trop tard (QEMU relève les barrières
  sur une minuterie) — le triangle passe de 10 ms à moins d'une.
- 13.6.1, 13.6.2 : les seize appels, validation. **13.6.3** : l'en-tête C
  `nova_gfx.h` et la **bibliothèque Rust** `tools/ports/nova-gfx/rust`
  (`no_std`, poignées typées libérées par `Drop`, erreurs typées), éprouvée
  par `/nova-tests/natif/gpu-rust` (10 vérifications, aucun appel Linux).
- 13.7.1, 13.7.2 : programme natif C et autotest (triangle sur la RTX).
- **13.8.1 — le compositeur sur le GPU** (`src/gui/gpu/`) : le même peintre
  (`painter::Target`) enregistre des instances au lieu de pixels ; un seul
  pipeline (rectangles, glyphes, icônes, toiles) compose dans une image, la
  zone endommagée est copiée par le GPU dans la sortie. **Égalité exacte**
  avec le processeur : `255 instances, 0 pixel(s) differents`, image entière
  et zone seule. Bascule automatique quand le cœur GPU est prêt ; retour au
  processeur si le GPU se perd.
- **13.8.2 — tranché : loupe et filtres restent sur le processeur**, lisant
  l'image que le GPU a composée (mêmes formules entières qu'en logiciel,
  éprouvées pixel à pixel par l'autotest : `loupe et filtre de couleur :
  chaque pixel egal au chemin processeur`).
- **13.8.3 — mesure** (autotest, même charge dans les deux sens) :

  | Charge | GPU | dont processeur | Processeur seul |
  |---|---|---|---|
  | Toile 1024×640 republiée, KVM | 565–955 µs/image | 134–176 µs | 441–643 µs/image |
  | Écran entier redessiné, KVM | 709–1112 µs/image | — | 490–636 µs/image |
  | Toile 1024×640, **TCG** | 1305 µs/image | 721 µs | **14 831 µs/image** |
  | Écran entier, **TCG** | 1497 µs/image | — | **17 982 µs/image** |

  Sous KVM, le compositeur consomme trois fois moins de processeur par
  image, mais attend l'aller-retour vers le GPU de l'hôte (0,4 à 0,8 ms) :
  la durée d'une image est du même ordre. Sous TCG (processeur émulé, comme
  une petite machine), le GPU compose onze fois plus vite. Le délai
  toile-écran (~10 ms) est celui du tour du compositeur, dans les deux cas.
- 13.11.2 — **replis** : matrice ci-dessous.
- **13.11.3 — sûreté** : commandes invalides refusées (autotest), processus
  mort avec des objets repris (autotest), et **boucle sur le GPU** : entrée
  GRUB « Nova OS — Essai du délai de garde GPU » (`essai-gpu=garde`), qui
  étalonne puis tient le GPU de l'hôte ~3 s :

  ```
  gpu: garde - 1024000 iterations en 0.111 s, charge longue de 27642260 iterations (environ 3 s)
  [gui] GPU occupe (une image en retard) - le processeur peint en attendant
  [gpu] contexte 86 "gpu" perdu - delai de garde depasse
  gpu: garde - contexte perdu au bout de 2.007 s
  gpu: garde - un nouveau contexte rend en 0.001 s
  [gui] compositeur sur le GPU - 1280x800, sortie en memoire hote, pret en 89 ms
  [test] phase 13 validee - 71 verifications de la pile graphique, sur le GPU
  ```

  Aucun message IPC perdu (15 872 perdus avant les corrections), le
  périphérique reste en service, le compositeur revient seul sur le GPU.

**Matrice, sur le code final** (un essai à la fois) :

| Variante | Phase 13 | Autres autotests |
|---|---|---|
| KVM, 8 cœurs | `75 verifications ... sur le GPU` | verts sauf l'export de la phase 4 (disque de l'utilisateur plein, défaut connu, antérieur) |
| `QEMU_SMP=1` | 75, sur le GPU | idem |
| `make run-smp` (4 cœurs) | 74, sur le GPU | idem |
| TCG | 74, sur le GPU | idem |
| `QEMU_GPU=` | `37 verifications ... sans GPU (absent)` (code final) | idem |
| `-vga none`, sans GPU | 37, sans GPU | idem ; compositeur inactif, dit |
| sans AC97 | 74, sur le GPU | idem |
| disque neuf, 1er puis 2e démarrage | 69 (programmes natifs absents : `make webroot` non passé) | **tous verts, export compris** ; le 2e retrouve ce que le 1er a installé |

**Défauts trouvés et corrigés en route** (DECISIONS) : `SleepLock::try_lock`
libérait le verrou d'un autre (course dans le cœur GPU depuis le début) ;
`vkCmdPushDescriptorSet` sans son extension (instance 1.3) ; attentes
synchrones de QEMU, verrou tenu et interruptions masquées, qui gelaient la
machine pendant un accaparement du GPU ; délai de garde compté en tics,
arrêtés dans ce cas ; plan curseur jamais montré (autotest renforcé).

**Non couvert, et limites.**
- Firefox : 13.0, 13.9 (présentation **sans copie** d'une image GPU d'une
  application — `SYS_GPU_PRESENT` passe encore par une relecture et une
  copie dans la fenêtre), 13.10, 13.11.1 — reportés par l'utilisateur.
- L'écran sur le GPU n'existe qu'avec le backend virtio : un module de
  fabricant n'a pas encore d'opération d'affichage dans l'interface
  `0x00010000` (il faudra une version suivante).
- Sur cet hôte, un programme qui accapare le GPU **gèle l'affichage de la
  VM** le temps que le délai de garde et le pilote de l'hôte le coupent :
  QEMU dessine son écran en OpenGL sur ce même GPU. Nova, lui, continue
  (aucune perte, reprise automatique). Un vrai shader infini dépend du
  pilote de l'hôte pour s'arrêter : Nova ne peut pas le préempter.
- La toile d'une fenêtre est copiée une fois par le processeur vers la
  mémoire du GPU (157 µs pour 1024×640) ; la mettre directement en mémoire
  GPU va avec 13.9.
- `gui/compositor.rs` (1538 lignes, 1512 avant la phase) reste à découper.

### Organisation et taille

Ordre : 13.0 (indépendant, gain immédiat) → 13.1 (accord) → 13.2 → 13.3 →
13.4 → 13.5 → 13.6 → 13.7 (premier triangle : toute la pile prouvée, sans
Firefox) → 13.8 (gain pour tout le bureau) → 13.9 → 13.10 → 13.11. Chaque
étape se valide à part, par la sortie série, sous les variantes de
CLAUDE.md.

Ordres de grandeur, en sessions comme celles du 23 septembre : 13.0 une ;
13.1-13.2 une ; 13.3 deux ; 13.4 deux à trois ; 13.5 trois à cinq (le
générateur et l'anneau Venus) ; 13.6-13.7 deux à trois ; 13.8 deux ; 13.9
une ; 13.10 cinq à dix (le backend WebRender et les patchs Gecko : la plus
grande inconnue) ; 13.11 une.
Au total, **de l'ordre de vingt à trente sessions**. Premier gain visible
pour le bureau à 13.8 ; pour Firefox à 13.10 ; 13.0.2 en donne un tout de
suite, sans GPU.

### Ce qui reste de Linux, et la suite vers un système entièrement à Nova

La phase 13 ne retire que la part graphique. Le reste, pour l'objectif
d'ensemble (état au 23 septembre 2026) :
- **Firefox est construit pour `x86_64-linux-musl`** avec GTK (le mode sans
  écran n'enlève pas la dépendance) ; ses bibliothèques viennent d'Alpine.
  Suite logique : une **cible Nova** dans le fork — `widget/nova` complet
  (fenêtres, clavier, presse-papiers, glisser-déposer, sélecteur de
  fichiers), cible Rust et C propre, bibliothèques reconstruites — qui fait
  disparaître GTK, X11 et la racine Alpine.
- **La libc Nova** garde l'ABI de musl et traduit en espace utilisateur les
  numéros Linux que musl et les appels bruts lui passent (spec §6.7).
- **QEMU tourne sur un Linux** : c'est l'environnement de développement,
  pas une dépendance du système. Sur un vrai PC, Nova n'utilise rien de
  Linux ; il lui manque alors des pilotes (GPU, et d'autres), que Nova
  n'écrira pas pour chaque carte (décision ci-dessus).

### Ce que la phase 13 ne couvre pas
- Les pilotes de cartes réelles : laissés aux fabricants et aux
  utilisateurs, sur l'interface de 13.4.1. Sur un vrai PC sans pilote, Nova
  reste sur le framebuffer GOP/VBE.
- WebGL, WebGPU, calcul général : demandent un compilateur de shaders à
  l'exécution ; après la phase.
- Le décodage vidéo matériel (Vulkan Video par Venus) : après la phase.

## Phase 14 — LIFS : liens CoW et Tag, dossiers en micro-bases 🔧 (implémentée, à valider)

Proposition de l'utilisateur du 24 septembre 2026, conservée dans
`propositions/lifs.md` : LIFS (Linked Index File System). Consigne de la
session : **lister ce qui doit être mis en place, sans code**.

### Ce que LIFS demande, en une phrase

Deux sortes de liens dans les dossiers : le **lien CoW** (une copie
instantanée, qui ne coûte rien tant qu'on n'écrit pas, et diverge à la
première écriture) et le **lien Tag** (le même fichier dans plusieurs
dossiers, toute modification visible partout) ; des dossiers stockés comme
de petites bases indexées ; un comptage de références qui libère un fichier
quand plus rien ne le désigne ; des appels système par identifiant de nœud.

### Ce qui existe déjà (NovaFS v2, état au 24 septembre 2026)

| LIFS | NovaFS v2 aujourd'hui | Écart |
|---|---|---|
| Topologie plate, nœuds sans nom | oui : table de nœuds de 128 octets, le nom vit dans le dossier (spec §4.1, `fs/format.rs`) | aucun |
| Déplacement O(1) | oui : `move_entry` réécrit une entrée | aucun |
| Lien Tag (lien dur) | oui en substance : `fs::link` + compteur `links` du nœud (`SYS_FS_LINK`) | le type de lien n'est pas enregistré dans l'entrée ; pas d'index inverse |
| Lien CoW, copie O(1) | **non** : `Volume::copy` relit tout le fichier et le réécrit | tout est à faire |
| Dossier en micro-base indexée | **non** : table plate d'entrées de 128 octets (nom de 120 octets + nœud), recherche linéaire | format à changer |
| 13 permissions | **oui, identiques** (spec §5.1, `fs/perm.rs`, mêmes bits 0 à 12) | LIFS en garde **un** masque, NovaFS **trois** (propriétaire, groupe, autres) |
| Journal d'accès (`audit`) | oui : `fs/audit.rs` | aucun |
| Fichiers de 100 Go | **non** : 1 Gio au plus (5 blocs directs + deux niveaux d'indirection) | format d'adressage à changer |
| API par identifiant de nœud | non : `SYS_FS_*` (domaine 01) par chemin | nouveaux appels |

**LIFS est un système de fichiers à part entière, avec son propre type de
partition** (précision de l'utilisateur, 24 septembre 2026). Il reprend la
philosophie de NovaFS (spec §4.1) et peut en reprendre du code, mais ce
n'est pas un « NovaFS v3 » : NovaFS reste le format des volumes existants,
LIFS devient un format nouveau, reconnu par son type de partition et son
superbloc.

### Préalable : Nova ne connaît pas les partitions

Relevé dans le code le 24 septembre 2026 :
- **aucune table de partitions n'est lue** (ni GPT, ni MBR) : un volume
  occupe un disque entier ; les images d'export (FAT32, ext4, NTFS) sont
  elles aussi des disques entiers, sans table ;
- **`Volume::mount_or_format` formate d'office un disque qui ne porte pas
  NovaFS** (`FormatError::NotNovaFs`). Sous QEMU, les disques sont les
  nôtres ; sur une machine réelle, un disque partitionné (Windows, Linux,
  données) serait **effacé au premier démarrage**. C'est un défaut de
  l'existant, à corriger avant LIFS quoi qu'il arrive : formater doit être
  un choix explicite, jamais une conséquence de ce qu'on ne reconnaît pas.

Ce que « un nouveau type de partition » demande donc :
- lire et écrire les tables **GPT** (d'abord) et **MBR** (disques anciens) ;
  chaque partition devient un périphérique de blocs à part (une fenêtre sur
  le disque), montable séparément ;
- un **type de partition LIFS** : un GUID GPT propre à Nova (à générer une
  fois, figé dans la spec), et pour MBR un octet de type (aucun n'est
  réservé officiellement : choisir un octet libre et le dire) ;
- la **détection** : type de partition puis superbloc (magic LIFS) ;
  NovaFS, FAT32, ext4, NTFS reconnus et montés, ou laissés tranquilles ;
- les **lettres de disque** (spec §4.2 : `A:/`, `B:/`) : par disque ou par
  partition — à décider ;
- le **volume racine** : désigné par le GUID de sa partition, et non plus
  « le premier disque vu » ;
- un **partitionneur** (installation, paramètres) et un formatage LIFS
  explicite ;
- côté hôte : images de disque partitionnées pour QEMU (`sgdisk`),
  `mkfs` LIFS dans `tools/`.

### Manques et contradictions du texte proposé — à trancher (14.1)

Relevés en confrontant la proposition au code et à la spec ; CLAUDE.md
demande de les signaler plutôt que de trancher seul.

1. **Le CoW au niveau du nœud ne suffit pas.** Le texte dit « copie les blocs
   modifiés vers un nouvel espace » : les blocs non modifiés restent donc
   partagés entre l'original et la copie. Un compteur par nœud
   (`cow_ref_count`) ne dit pas **quels blocs** sont partagés. Il faut un
   **compte de références par bloc** (ou par étendue de blocs, comme les
   « reflinks » de btrfs et XFS). Sans lui : soit copie du fichier entier à
   la première écriture (simple, mais une écriture de 1 octet dans 100 Go
   copie 100 Go), soit corruption.
2. **L'original aussi doit être protégé.** Après une copie CoW, une écriture
   par le chemin de **l'original** modifierait la copie (ils partagent les
   blocs). Le texte ne parle que des écritures par le lien CoW. Règle
   nécessaire : toute écriture dans un bloc partagé copie ce bloc, quel que
   soit le lien par lequel elle arrive.
3. **Métadonnées partagées.** Une copie CoW partage le nœud, donc les
   permissions et le propriétaire : un `chmod` par la copie changerait
   l'original. Il faut que le premier changement de métadonnées sépare aussi
   le nœud (sans copier les données).
4. **Le lien d'origine n'a pas de type.** À la création, le fichier a une
   entrée : est-elle un Tag, un CoW, une troisième sorte ? La règle de
   destruction (« les deux compteurs à zéro ») en dépend.
5. **Un seul masque de 16 bits, un seul propriétaire.** NovaFS a trois
   masques (propriétaire, groupe, autres), choix consigné dans `fs/perm.rs`
   pour que les rôles (spec §5.2) et `own` aient un effet. Garder les trois
   (proposé) ou passer à un masque unique avec des listes de droits par
   compte : décision de l'utilisateur.
6. **Douze pointeurs directs = 48 Kio.** `data_pointers: [u64; 12]` sans
   indirection limite un fichier à 12 blocs. « 100 Go en O(1) » exige un
   adressage par **étendues** (début, longueur) dans un arbre, qui sert aussi
   le partage par étendues du point 1.
7. **`query_tags` exige un index inverse** (nœud vers dossiers qui le
   désignent), absent des structures. Un index à tenir à jour à chaque lien
   créé ou retiré.
8. **Cohérence après une coupure.** Un CoW et des compteurs de références
   sans journal : une coupure de courant au milieu d'une copie ou d'un
   retrait laisse des blocs perdus ou, pire, un bloc libéré encore désigné.
   Il faut soit un **journal**, soit des métadonnées elles-mêmes en
   copie-sur-écriture (écriture ailleurs puis bascule atomique du
   superbloc). Le texte n'en dit rien ; c'est le plus gros morceau.
9. **Appels par identifiant.** `sys_lifs_move(node_id, ...)` : un programme
   peut nommer n'importe quel nœud par son numéro. Chaque appel doit vérifier
   que l'appelant atteint ce nœud par un chemin qu'il a le droit de voir, ou
   les identifiants deviennent des passe-partout. Et ils coexistent avec
   `SYS_FS_*` (chemins) : les deux API, une seule sémantique.
10. **Entre deux volumes, rien n'est O(1).** Déplacer ou copier vers un
    autre disque copie les octets (spec §4.1 le dit déjà) ; le texte promet
    O(1) « indépendamment de la taille » sans cette réserve.
11. **`bitflags`** : le texte l'utilise ; CLAUDE.md interdit toute dépendance
    externe. Masque écrit à la main, comme aujourd'hui.
12. **Espace et quotas.** Une copie CoW ne coûte rien, puis coûte à la
    première écriture : à qui compte-t-on les blocs partagés ? Et un disque
    plein pendant une écriture dans un bloc partagé fait échouer une
    écriture qui « n'agrandit » rien.
13. **Collisions de `name_hash`.** Deux noms, même empreinte : l'index doit
    comparer le nom, pas seulement l'empreinte.

### Conséquences hors du système de fichiers

- **Cache de pages et projections** (`posix::vfs::novafs`) : le cache est
  indexé par numéro de nœud. Si une écriture « crée un nouveau Node ID »
  (texte, 2.1), les pages projetées et le cache doivent suivre le nouveau
  nœud, ou une projection partagée lit l'ancien.
- **Programmes portés** : `link()` POSIX = lien Tag ; `rename` = move ; une
  copie par `cp --reflink` / `copy_file_range` = clone CoW. La libc Nova et
  `posix` doivent l'exposer.
- **Export** (spec §4.4, FAT32, ext4, NTFS) : un lien Tag devient un lien dur
  (ext4, NTFS) ou une copie (FAT32) ; un clone CoW devient une copie.
- **Outils hôte** : `tools/nova-fs.py`, `tools/fs-inspect.py` à mettre au
  format v3 ; migration d'un volume v2.
- **Interface graphique** : l'explorateur montre les tags d'un fichier et
  propose « copier » (CoW) et « étiqueter » (Tag).

### Étapes

#### 14.0 Partitions (préalable)
- [ ] 14.0.1 Plus aucun formatage implicite : un disque non reconnu est
  signalé et laissé intact ; formater est un appel explicite
- [ ] 14.0.2 Lecture des tables GPT et MBR ; une partition = un
  périphérique de blocs (décalage, taille), bornes vérifiées à chaque accès
- [ ] 14.0.3 Détection par type de partition puis superbloc ; montage de
  chaque partition reconnue ; lettres de disque selon la décision de 14.1
- [ ] 14.0.4 Écriture d'une table GPT (création, ajout de partition) ;
  images partitionnées pour QEMU dans le Makefile
- [ ] 14.0.5 Volume racine désigné par le GUID de sa partition

#### 14.1 Spécification et décisions
- [ ] 14.1.1 Réponses de l'utilisateur aux treize points ci-dessus, et aux
  questions des partitions : lettres par disque ou par partition ; racine
  en LIFS ou en NovaFS au départ ; octet MBR retenu
- [ ] 14.1.2 Spec §4 : partitions (GPT, MBR, types, GUID LIFS), LIFS comme
  format natif (format de partition, superbloc, magic), NovaFS maintenu
  pour les volumes existants ; §5 (permissions : un masque ou trois) ;
  domaine `SYS_FS_*` étendu (appels par identifiant, clone, tag, requête
  des tags) ; §4.4 (export des liens)

#### 14.2 Format LIFS (sur le papier puis dans un module `fs/lifs/`)
- [ ] 14.2.0 Superbloc LIFS (magic, version, taille de bloc, racines des
  arbres, compteur de génération pour la cohérence), dans la partition
- [ ] 14.2.1 Nœud : identifiant, type, taille, propriétaire, droits, dates,
  racine de l'arbre d'étendues
- [ ] 14.2.2 Arbre d'étendues par fichier ; comptes de références par
  étendue (arbre global des blocs partagés)
- [ ] 14.2.3 Dossier en arbre B (entrée : empreinte, nom, nœud, type de
  lien), recherche par empreinte puis nom
- [ ] 14.2.4 Index inverse nœud → dossiers (pour `query_tags` et pour savoir
  qui désigne un nœud)
- [ ] 14.2.5 Cohérence : journal ou métadonnées en copie-sur-écriture (choix
  en 14.1)

#### 14.3 Moteur
- [ ] 14.3.1 Création, lien Tag, retrait, destruction au dernier lien
- [ ] 14.3.2 Clone CoW : copie de l'arbre d'étendues, comptes +1 ; écriture
  dans une étendue partagée = copie de cette étendue seulement
- [ ] 14.3.3 Séparation des métadonnées au premier `chmod`/`own` d'un clone
- [ ] 14.3.4 Déplacement, renommage, dans un volume et entre volumes
- [ ] 14.3.5 Reprise après coupure : relecture du journal ou du dernier
  superbloc cohérent

#### 14.4 Interfaces
- [ ] 14.4.1 Appels système : ceux de la proposition, vérifiés contre les
  droits de l'appelant ; `SYS_FS_*` par chemin, mêmes règles
- [ ] 14.4.2 `posix` : `link`, `rename`, `copy_file_range` et le clonage
  ; cache de pages et projections qui suivent un nœud séparé
- [ ] 14.4.3 Export et import (FAT32, ext4, NTFS) avec les deux sortes de
  liens
- [ ] 14.4.4 Explorateur : tags d'un fichier, copier, étiqueter

#### 14.5 Outils et migration
- [ ] 14.5.1 Outils hôte : `mkfs` LIFS, lecture et import (comme
  `tools/nova-fs.py` pour NovaFS), inspection
- [ ] 14.5.2 Passage d'un volume NovaFS à LIFS : copie vers une nouvelle
  partition LIFS (outil hôte, puis dans Nova) ; un volume NovaFS n'est
  jamais reformaté sans demande

#### 14.6 Validation
- [ ] 14.6.1 Autotest : clone de 1 Gio en temps constant (compté en blocs
  écrits, pas en secondes) ; écriture d'un octet dans le clone = une
  étendue copiée ; l'original intact ; Tag visible partout ; destruction au
  dernier lien, blocs libres revenus à l'identique
- [ ] 14.6.2 Coupure simulée à chaque étape d'un clone et d'un retrait :
  volume cohérent au redémarrage (`fs::check_all`)
- [ ] 14.6.3 Le navigateur (profil SQLite, cache) sur une partition LIFS ;
  les variantes de CLAUDE.md ; `disk-reset` et deux démarrages
- [ ] 14.6.4 Disque partitionné avec d'autres systèmes (FAT32, ext4, NTFS,
  une partition inconnue) : Nova ne touche qu'à ses partitions, jamais aux
  autres

### Organisation et taille

Ordre : 14.1 (accord : rien ne se code avant) → 14.0 (partitions ; 14.0.1
peut passer tout de suite, c'est un défaut de l'existant) → 14.2 → 14.3 →
14.4 → 14.5 → 14.6. Le point dur est la cohérence après coupure (point 8) avec le partage
par étendues (point 1) : c'est ce qui sépare un CoW qui marche d'un CoW qui
perd des fichiers. Ordre de grandeur : **dix-huit à trente sessions**, dont
trois à cinq pour les partitions.

La phase 14 est indépendante de la phase 13 (GPU) : l'ordre entre elles est
à décider par l'utilisateur.

### État au 24 septembre 2026, soir — observé par l'assistant, non coché

Relevés de l'assistant, KVM sauf mention, `-boot d`, disques de
`/tmp/.../scratchpad` (jamais `disks/nova-disk.img`), supports d'export
copiés, sans AC97. **Aucune case n'est cochée : la validation de
l'utilisateur est attendue** (CLAUDE.md, 2.7).

**Ce qui est fait, par étape**

| Étape | Fait | Où |
|---|---|---|
| 14.1.1 / 14.1.2 | Réponses de l'utilisateur et recommandations appliquées (DECISIONS) ; spec §4.2, §4.5, §6.2 | `SPECIFICATION.md`, `DECISIONS.md` |
| 14.0.1 | Plus aucun formatage implicite : `mount_or_format` retiré ; seul le volume en mémoire (aucune racine) est formaté par le noyau | `fs/mount.rs`, `fs/mod.rs` |
| 14.0.2 | GPT (en-tête et entrées vérifiés par CRC32) et MBR (entrées primaires, validées une à une) ; `PartitionDevice`, bornes vérifiées à chaque accès | `fs/partition/mod.rs` |
| 14.0.3 | Type puis superbloc ; une lettre par partition ; partitions étrangères listées et laissées intactes | `fs/mount.rs` |
| 14.0.4 | Écriture GPT dans le noyau (création, ajout de partition) ; `make disk` : GPT + p1 LIFS système + p2 LIFS données | `fs/partition/write.rs`, `tools/lifs.py`, `Makefile` |
| 14.0.5 | `racine=<GUID>`, sinon LIFS « système », sinon premier LIFS, sinon NovaFS, sinon mémoire | `mount::choose_root`, `multiboot.rs` |
| 14.2.0 → 14.2.5 | Superbloc, nœud de 256 octets, étendues (7 en ligne, puis chaîne), comptes de références par étendue, dossiers en arbre B+ (empreinte puis nom), index inverse, journal des métadonnées | `fs/lifs/` |
| 14.3.1 → 14.3.5 | Création, Tag, retrait au dernier lien ; clone = nouveau nœud (métadonnées séparées d'emblée) ; CoW par bloc, original compris ; déplacement O(1), copie entre volumes ; rejeu au montage | `fs/lifs/ops.rs`, `files.rs`, `journal.rs` |
| 14.4.1 | `SYS_FS_NODE_STAT/MOVE/CLONE/TAG/UNLINK/TAGS` (0x0130–0x0135), droits du §5.1 et bac à sable §12.4 par chemin reconstruit ; `SYS_FS_COPY` = clone dans un volume LIFS | `syscall/domain_fs.rs` |
| 14.5.1 | `tools/lifs.py` : create, table, import, put, ls, cat, check ; délègue à `nova-fs.py` sur un disque NovaFS entier | `tools/lifs.py` |
| 14.5.2 | Reprise NovaFS → LIFS (hôte) : `make disk-lifs`, l'ancien gardé en `.novafs` | `tools/lifs.py from-novafs` |
| 14.6.1 / 14.6.2 | Autotest : clone de 1 Gio, CoW, Tag, restitution des blocs, coupure à chaque écriture | `fs/lifs/selftest.rs` |

**Relevés**

Disque neuf `tools/lifs.py create --size 2048` (validé par `sfdisk --verify`),
deux démarrages :

```
[fs] "QEMU HARDDISK" (2048 Mio) - table GPT, 2 partition(s)
[fs]   p1 LIFS "nova-systeme" (1790 Mio) - systeme
[fs]   p2 LIFS "nova-donnees" (256 Mio)
[fs] "QEMU HARDDISK" (96 Mio) - ni table de partitions ni volume Nova - laisse intact comme support amovible
[fs] "QEMU HARDDISK" p1 monte en / - LIFS "nova-systeme", 1790 Mio dont 1762 libres
[fs] "QEMU HARDDISK" p2 monte en A:/ - LIFS "nova-donnees", 256 Mio dont 251 libres
[fs] arborescence systeme creee - 37 dossiers (spec 17)
...second demarrage :
[fs]   demarrage n 2 - le volume conserve son contenu depuis 1 extinction(s)
[fs] autotest termine : 158 reussites, 0 echecs
```

Autotest LIFS (noyau final, identique en SMP1, SMP4, SMP8, TCG, `-vga none`) :

```
[lifs]   clone de 1 Gio : 5 bloc(s) de metadonnees, 14 ecriture(s) disque ; clone de 4 Kio : 4 bloc(s), 12 ecriture(s)
[lifs]   octet ecrit dans le clone : 1 bloc(s) pris, etendues du clone 3 (original 1)
[lifs]   blocs libres : 1032156 au depart, 1032156 apres destruction
[lifs]   blocs libres : 16123 avant, 15750 a 300 fichiers, 16123 apres
[lifs]   32 coupures : 13 avant le clone, 13 entre clone et retrait, 6 apres le retrait, 12 rejeu(x) du journal, 0 incoherence(s)
[lifs]   p1 secteurs 2048..133119, p2 secteurs 133120..524247 (190 Mio)
[lifs] autotest termine : 81 reussites, 0 echecs
[fs] autotest termine : 179 reussites, 0 echecs
```

Disque de l'utilisateur **copié** puis converti (`lifs.py from-novafs`,
5422 fichiers, 640 dossiers, 831 Tag, 2,5 s ; `lifs.py check` propre),
deux démarrages sur la racine LIFS :

```
[fs] "QEMU HARDDISK" p1 monte en / - LIFS "nova-systeme", 1790 Mio dont 1033 libres
[fs]   demarrage n 14 - le volume conserve son contenu depuis 13 extinction(s)
[test] phase 5 validee - 283 verifications de l'API systeme (spec 6)
[test] phase 11.5 validee - 13 programme(s) Linux conformes, 0 absent(s)
[test] phase 12 validee - 11 programme(s) natifs conformes, 0 absent(s), aucun appel Linux
[test] phase 13 validee - 73 verifications de la pile graphique, sur le GPU
```

Après ces démarrages, `tools/lifs.py check` sur l'image : `6063 noeuds,
186757 blocs en usage, 264556 libres`, aucun problème. Les trois échecs
`[export] ECHEC : export FAT32/ext4/NTFS - plus de bloc libre` sur cette
image existent à l'identique avec la racine NovaFS et le même noyau
(copie `-snapshot`) : c'est l'échec connu du disque plein de
l'utilisateur, pas LIFS.

**Matrice** — disque neuf sauf mention :

| Variante | Résultat |
|---|---|
| SMP4 (KVM) | tout validé ; LIFS 81/0, fs 179/0 |
| `QEMU_SMP=1` | tout validé |
| SMP8 | tout validé |
| TCG | tout validé |
| `-vga none` | tout validé (phase 13 : 37 vérifications, GPU absent) |
| sans AC97 | toutes les variantes ci-dessus |
| disque neuf, deux démarrages | le second retrouve (`demarrage n 2`) |
| disque vierge sans table | laissé intact, racine LIFS en mémoire |
| copie du disque NovaFS de l'utilisateur | montée en NovaFS, inchangée |
| même disque converti en LIFS | navigateur, Linux, natifs, GPU validés |

**Ce qui reste ouvert**

- 14.4.2 : aucun `copy_file_range` ni `FICLONE` dans la couche POSIX/Linux :
  un programme porté copie octet par octet ; seul `SYS_FS_COPY` clone. Le
  cache de pages n'a rien à suivre (un clone est un nouveau nœud, le CoW
  change les blocs, pas l'identifiant), mais ce n'est pas éprouvé par un
  programme porté.
- 14.4.3 : l'export traite un Tag comme les liens NovaFS d'avant (lien dur
  ext4/NTFS, copie FAT32) ; un clone s'exporte comme deux fichiers. Non
  éprouvé à part (l'export du disque plein échoue avant, voir plus haut).
- 14.4.4 : l'explorateur ne montre pas les Tag d'un fichier et n'offre pas
  « étiqueter » (le « copier » y est déjà un clone sur LIFS).
- 14.5.2 dans Nova : la reprise NovaFS → LIFS n'existe que côté hôte.
- 14.0.4 : l'écrivain GPT du noyau n'a ni appel système ni interface ; seul
  l'autotest s'en sert (sur un disque creux en mémoire).
- 14.6.4 : pas encore de disque partagé avec FAT32/ext4/NTFS réels (la
  lecture des partitions étrangères et leur mise à l'écart sont en place,
  la détection MBR durcie contre les secteurs d'amorçage).
- La table GPT de secours (fin du disque) est écrite mais **jamais lue** ;
  une GPT dont l'en-tête principal est abîmé est laissée intacte, non
  réparée.
- Une transaction plus grosse que le journal (`NoSpace` à la validation)
  reste ouverte et bloque les validations suivantes ; la validation au
  début de chaque opération à mi-journal l'évite pour les opérations
  courantes, pas pour une écriture unique de plusieurs gigaoctets très
  fragmentée.
- Une allocation qui échoue alors que des blocs n'attendent que la
  validation répond « plus de bloc libre » : pas de validation forcée au
  milieu d'une opération (voir DECISIONS).
- L'arbre B+ fusionne deux voisins mais n'emprunte pas : après des retraits
  dispersés, des feuilles à demi vides restent possibles.
- Les empreintes de performance (import de 500 Mio, profil du navigateur
  sur LIFS) ne sont pas mesurées.

## Phase 15 — Mises à jour atomiques des applications ✅ (faite le 28 octobre 2026)

Proposition de l'utilisateur du 24 septembre 2026, conservée dans
`propositions/mises-a-jour.md` : registre de manifestes JSON sur GitHub,
canaux release/beta/test, installation de chaque version dans son propre
dossier, bascule par un **pointeur actif**, ancienne version gardée tant
qu'elle tourne puis supprimée, seul le gestionnaire système écrit les
applications.

Le 28 octobre 2026, l'utilisateur a donné **mandat d'autonomie** (« tu es
en total autonomie pour concevoir le meilleur code possible ») : les dix
points de 15.1 sont tranchés ci-dessous par l'assistant, chacun sur la
recommandation déjà écrite ici le 24 septembre, et consignés dans
`DECISIONS.md`. Ils restent révisables par l'utilisateur.

### Ce qui existe déjà (SPM, phase 8 ; relevé le 24 septembre 2026)

| Proposition | SPM aujourd'hui | Écart |
|---|---|---|
| Registre de manifestes JSON, un par application | un **index texte** par dépôt (`# nova-spm-index v1`, lignes `pkg|...`, ancre d'intégrité), `src/spm/repo.rs` | nouveau format, lecture JSON à écrire |
| Canaux release / beta / test | aucun | nouveau |
| Téléchargement depuis GitHub (`raw.githubusercontent.com`, releases) | **HTTP en clair seulement** (`repo.rs` : `GET ... HTTP/1.0`, port 80) | HTTPS obligatoire chez GitHub : passer par le client TLS du noyau (`web::http`, `web::tls`) |
| Binaires de plusieurs dizaines de Mio | `web::http` plafonne une réponse à **4 Mio** (`MAX_BODY`), en mémoire | téléchargement en flux, écrit sur le disque au fil de l'eau |
| Checksum SHA-256 comparé | oui, et **en plus** l'ancre de l'index et la **signature du paquet** (spec §3.2, §10.4) | la proposition ne parle que du checksum : à garder en plus, pas à la place |
| Installation dans un dossier neuf, puis bascule | non : `manager::install_bytes` écrit directement dans `/apps/{app}/` | nouveau |
| Pointeur actif suivi par le menu, les raccourcis, les associations | non : chemins en dur (`/apps/navigateur.cus`, `/usr/lib/web-browser/web-browser.cus`) | nouveau, touche le lanceur, `apps::launch`, `posix` |
| Ancienne version gardée tant que son PID vit, supprimée à la fin ; purge au démarrage | non ; le rollback A/B existe **pour l'OS** (spec §11.3, `spm/rollback.rs`), pas pour les applications | nouveau |
| Seul le gestionnaire écrit les applications, l'application demande par IPC | en partie : le bac à sable (§12.4) interdit déjà `/apps/` aux applications ; pas de requête IPC « mets-moi à jour » | protocole IPC à définir |
| Paramètres séparés des binaires | oui (spec §17 : `/users/{user}/settings/`, profil du navigateur dans `/users/{user}/apps/web-browser`) | aucun |
| Codes de mise à jour obligatoire/standard/optionnelle/silencieuse (spec §10.2) | oui | le JSON proposé n'a pas ce champ : à ajouter |

### Points tranchés (15.1, 28 octobre 2026)

1. **Checksum et signature.** Le checksum du manifeste reste un contrôle de
   transfert ; l'authenticité vient de la **signature du paquet** (§3.2)
   **et** d'une **signature Ed25519 du manifeste** par la clé du registre.
   Ed25519 est écrit une fois (15.0.2) et sert aussi à faire enfin
   d'ENTERPRISE une vraie signature (limite de la phase 3).
2. **GitHub par défaut, pas imposé.** Un registre est une adresse
   (`https://`, `http://` ou `file:`) ; GitHub (`raw.githubusercontent.com`)
   en est un parmi d'autres. Le système livre un registre local `file:`
   (`/sys/tools/spm/registre`), qui sert aussi aux autotests.
3. **Le `download_url` désigne un `.spm`.** Le format de paquet ne change
   pas ; le manifeste dit où le prendre et quelle empreinte il doit avoir.
4. **Emplacement.** `{racine}/{app_id}/versions/{version}/`, où la racine
   est celle du §4.3 pour le type (`/sysapp`, `/apps`, `/games`, `/ras`) ;
   pendant l'installation, `versions/{version}.tmp/`.
5. **Le pointeur actif est un petit fichier** `{racine}/{app_id}/current`
   qui nomme la version active. Basculer, c'est écrire le nouveau à côté
   (`current.nouveau`) puis **remplacer l'entrée `current` en une
   opération** (`fs::retarget`, 15.0.5) : une transaction du journal sur
   LIFS, l'écriture d'une seule entrée sur NovaFS. L'ancien ou le nouveau,
   jamais aucun. Un lien Tag de dossier, envisagé d'abord, a été écarté :
   un processus de la version 1 qui ouvre encore ses fichiers par
   `current/...` après la bascule lirait ceux de la version 2 (Firefox
   charge ses bibliothèques au fil de l'eau) ; avec un pointeur lu au
   lancement, chaque processus reçoit le chemin absolu de **sa** version.
   Et LIFS ne nomme un dossier qu'une fois, ce qui l'exempte de cycles.
6. **Fichiers projetés.** Un processus garde ses fichiers par nœud, pas par
   chemin ; la suppression d'une version attend qu'**aucun processus**
   lancé depuis elle ne vive (tous ses processus, pas un PID).
7. **Canaux.** Par application, `release` par défaut, réglable dans les
   paramètres ; un canal global par défaut dans `/sys/config/spm.cfg`.
   Passer d'un canal à un canal plus ancien ne rétrograde pas : la
   version reste jusqu'à ce que le nouveau canal la dépasse, sauf demande
   explicite (`spm install {app} --canal release --retrograder`).
8. **Retour arrière.** La version **précédente** est gardée (une seule),
   pour `spm rollback {app}` ; les plus anciennes sont supprimées dès
   qu'aucun processus ne les exécute. Écart assumé au texte, qui supprime
   tout de suite : c'est ce que fait déjà le §11.3 pour l'OS.
9. **Frontière.** Ce mécanisme couvre les applications installées par SPM.
   Le noyau, ses applications intégrées (explorateur, bloc-notes…) et le
   navigateur intégré font partie de l'image de l'OS et suivent le §11
   (partition A/B, redémarrage).
10. **Le navigateur porté** (racine Alpine de 833 Mio) attend la phase 18 :
    rien n'est téléchargeable tant qu'il n'est pas un paquet.

### Étapes

#### 15.0 Préalables partagés (écrits une fois, resservent en 16 et 17)
- [x] 15.0.1 `src/json.rs` : lecteur et écrivain JSON (RFC 8259), bornés en
  profondeur et en taille, sans flottant (un nombre non entier garde son
  texte) ; autotest sur les cas de la RFC
- [x] 15.0.2 Ed25519 (RFC 8032) : SHA-512, vérification et signature ;
  vecteurs de la RFC à chaque démarrage
- [x] 15.0.3 ENTERPRISE signé pour de vrai : certificat v2 signé Ed25519 par
  l'autorité (binaires, `.kmd`, paquets), autorités = clés publiques,
  validité temporelle contrôlée par l'horloge ; l'ancien certificat refusé
  nommément
- [x] 15.0.4 Téléchargement HTTPS en flux (`web::http`) : redirections, corps
  écrit par morceaux dans un fichier, sans plafond de 4 Mio, reprise par
  `Range` après coupure
- [x] 15.0.5 `fs::retarget` : remplacement atomique du fichier désigné par
  une entrée (LIFS : une transaction ; NovaFS : une entrée) ; coupure
  simulée

#### 15.1 Spécification et décisions
- [x] 15.1.1 Les dix points ci-dessus (mandat d'autonomie du 28 octobre)
- [x] 15.1.2 Spec §10.5 (registre, manifeste, canaux, emplacement des
  versions, pointeur, rôle exclusif du gestionnaire, requête IPC) et §11
  (frontière applications / OS)

#### 15.2 Registre
- [x] 15.2.1 Manifeste JSON par application, canaux, codes §10.2, signature
  Ed25519 vérifiée ; `spm registry add <url>` ; registre local livré
- [x] 15.2.2 Résolution : version du canal choisi, comparée à la version
  installée ; refus nommé d'un manifeste altéré

#### 15.3 Installation atomique
- [x] 15.3.1 Téléchargement dans `versions/{version}.tmp`, empreinte,
  signature, niveau §10.4, puis renommage en `versions/{version}`
- [x] 15.3.2 Bascule de `current` par `fs::retarget`
- [x] 15.3.3 Coupure à chaque étape : au redémarrage, l'ancienne version
  active et le `.tmp` purgé, ou la nouvelle active — jamais d'application
  cassée

#### 15.4 Lancement par le pointeur
- [x] 15.4.1 Le lanceur, l'explorateur et `apps::launch` lancent
  `{racine}/{app}/current/{binaire}` dans un espace d'adressage à lui
- [x] 15.4.2 Chaque processus retient l'application et la version qu'il
  exécute

#### 15.5 Nettoyage
- [x] 15.5.1 Suppression différée d'une version quand plus aucun processus
  ne l'exécute (la précédente gardée pour le retour arrière)
- [x] 15.5.2 Purge au démarrage des `.tmp` et des versions ni actives ni
  précédentes ; `spm rollback {app}`

#### 15.6 Côté applications
- [x] 15.6.1 Requête IPC `spm.update` (« une mise à jour existe-t-elle »,
  « installe-la ») vers le gestionnaire ; refus pour une application qui
  demande pour une autre
- [ ] 15.6.2 Notifications selon les codes §10.2 ; canal choisi dans les
  paramètres
- [x] 15.6.3 `updater.dsa` vérifie aussi les manifestes des applications
  (24 h, §11.2)

#### 15.7 Validation
- [x] 15.7.1 Autotest sur le registre local : mise à jour d'une application
  pendant qu'elle tourne ; l'ancienne continue, la nouvelle au prochain
  lancement, l'ancienne supprimée à sa fin
- [x] 15.7.2 Manifeste altéré, checksum faux, paquet non signé : refus,
  rien sur le disque
- [ ] 15.7.3 Coupures simulées ; variantes de CLAUDE.md (`make test-all`)

### État au 28 octobre 2026 — observé par l'assistant (mandat d'autonomie)

Relevés de `make test` (TCG, sans KVM ni GPU, `tools/boot-test.py`,
disque neuf puis second démarrage sur le même disque), journaux dans
`build/test/`. Les cases ci-dessus sont cochées sur ces lignes, en vertu
du mandat d'autonomie de l'utilisateur (CLAUDE.md, 2.7 : validation par
l'assistant quand l'utilisateur la demande).

```
[sign] 0 autorite(s) ENTERPRISE de confiance (/sys/config/autorites.cfg)
[lifs]   17 coupures pendant la bascule : 11 sur l'ancienne version, 6 sur la nouvelle, 0 sans pointeur
[test] phases 3 et 4 validees - 250 verifications conformes aux specs 3 a 5
[spm.dsa] pret - canal spm.update ouvert, 0 application(s) versionnee(s), 0 version(s) purgee(s)
[test] navigateur valide - 30 vecteurs cryptographiques, 33 verifications web
[maj]   JSON : 27 verification(s), 0 echec(s)
[spm] os.nova.essai15 2.0.0 installe dans /apps/os.nova.essai15/versions/2.0.0 - version active, precedente 1.0.0
[spm] /apps/os.nova.essai15 1.0.0 encore en cours d'execution - suppression differee
[spm] /apps/os.nova.essai15 1.0.0 supprimee - plus active ni precedente, plus executee
[spm.dsa] processus 72 (os.nova.essai15) demande pour os.nova.explorateur - refuse
[test] phase 15 validee - 70 verifications des mises a jour atomiques (spec 10.5)
```

- 15.0.2 : les trois vecteurs de la RFC 8032 (clé, signature, vérification)
  et trois refus (message changé, R changé, S ≥ L) ; `tools/novasign.py`
  vérifié contre la bibliothèque `cryptography` de l'hôte (20 tirages).
- 15.0.3 : certificat v1, validité repoussée, certificat expiré, paquet
  ENTERPRISE v1 : refusés nommément (`phases 3 et 4`, `phase 8`).
- 15.0.4 : sur connexion scriptée : redirection, blocs coupés n'importe où,
  reprise par `Range`, plage ignorée, coupure avant la fin, taille
  au-delà de la limite.
- 15.7.1 : installation 1.0.0, processus lancé, 2.0.0 installée **pendant
  qu'il tourne**, un nouveau lancement prend 2.0.0, une 3.0.0 garde 1.0.0
  tant que son processus vit, puis la supprime à sa fin ; `spm rollback`.
- 15.7.2 : manifeste altéré, signé d'une autre clé, empreinte fausse,
  paquet refusé par sa signature : rien de plus sur le disque, aucun
  téléchargement laissé.

**Ce qui reste ouvert dans la phase 15**
- 15.6.2 : les notifications suivent les codes du §10.2 (`updater.dsa`) ;
  le choix du canal par défaut est dans l'onglet « Mises a jour » des
  paramètres (`mises-a-jour.ss`), le canal par application par commande
  seulement (`spm canal`).
- 15.7.3 : coupures simulées pendant la bascule (LIFS, ci-dessus) et
  `.tmp` d'une installation coupée purgé ; **pas** de coupure simulée au
  milieu de l'écriture d'une version (couverte par le journal LIFS de la
  phase 14, pas éprouvée ici).
- Aucun registre réel (GitHub) éprouvé : pas de serveur joignable depuis
  QEMU dans cet environnement.
- `installer` sur `spm.update` télécharge dans la tâche du service (une
  autre requête attend).

## Phase 16 — Langages et outils (fin de la phase 9) 📝 (planifiée)

Ferme ce que la phase 9 a laissé ouvert : 9.1.3 (`dev run --debug`) et
9.5.2 à 9.5.6 (les langages sans chaîne). Principe inchangé de la phase 9
(DECISIONS, « Sept lignes du §13.2 ne pouvaient pas être écrites ») :
**aucun chemin du SDK ne produit un binaire en prétendant l'avoir
compilé** ; ce qui n'est pas écrit est refusé nommément. Ce qui change :
au lieu d'attendre des chaînes externes (LLVM, Mono, une JVM, V8),
Nova écrit les siennes, chacune pour un sous-ensemble **défini et
documenté**, comme `sdk::rustc` pour Rust.

#### 16.0 Terminal et shell (spec §1.1 « Shell / CLI », §2.2 mode recovery)
Relevé le 28 octobre 2026 : **Nova n'a pas de shell**. `spm` (§10.3) et
`dev` (§15.2) sont des lignes de commande que `manager::execute` et
`sdk::cli::execute` analysent, mais personne ne peut les taper ; le mode
recovery promet « kernel + fs.dsa + shell CLI uniquement » et n'a pas de
shell. Préalable à tout ce qui suit : `dev interpret`, `dev run --debug`
ont besoin d'un endroit où taper.
- [x] 16.0.1 Interprète de commandes : `spm …`, `dev …`, et les commandes
  de base (`aide`, `ls`, `cd`, `pwd`, `cat`, `mkdir`, `rm`, `cp`, `mv`,
  `ps`, `kill`, `uname`, `date`, `df`, `lance`), droits de la session
- [x] 16.0.2 `terminal.ss` : fenêtre, historique, défilement
- [x] 16.0.3 Shell sur la console série en mode recovery (spec 2.2)

**Validée le 28 octobre 2026** par l'assistant, sous mandat d'autonomie
de l'utilisateur, sur les relevés suivants (QEMU TCG, sans KVM) :

`make test` (4 cœurs, deux démarrages sur le même disque), `make
test-smp1`, `make test-novga` :
```
[apps] lancement de "terminal.ss" (os.nova.terminal) - PID 87
[terminal] surface 12 - 760x460
[terminal] ferme - surface rendue
[test] phase 16.0 validee - 60 verifications du shell et du terminal (spec 1.1, 2.2)
```
(sans écran, les quatre contrôles de la fenêtre réelle sont omis :
56 vérifications.)

`make test-recovery` (ISO dont l'entrée par défaut est la recovery, deux
cœurs ; les commandes sont tapées sur la console série) :
```
[recovery] autotests omis - noyau, services core et shell serie (spec 2.2)
         tape : systeme
[init] mode recovery - pilotes, reseau, interface et session omis ; shell sur la console serie
Nova OS 0.15.0 (d80d8c5) - x86_64 - 2 coeur(s)
         tape : liste /
sysapp/
         tape : ecrire /cache/essai.txt bonjour du shell
/cache/essai.txt : 17 o ecrit(s)
         tape : lire /cache/essai.txt
bonjour du shell
         tape : aide
Commandes (aide <commande> pour le detail) :
```

Ce que couvrent les 60 vérifications : découpage en mots et guillemets,
chemins (relatifs, `..`, lettre de disque), les commandes de fichiers de
bout en bout (`dossier`, `va`, `ecrire`, `lire`, `cp`, `deplacer`,
`liste`, `supprimer` avec et sans `-r`), les refus (fichier absent,
`va` sur un fichier, commande inconnue), `aide` d'un alias, `systeme`,
`taches`, l'historique ; un compte ordinaire refusé sur `/sys` et sur le
fichier privé de ROOT ; le terminal joué au clavier synthétique (curseur,
historique, coupure à la largeur, page précédente, Ctrl+C, Ctrl+U,
Ctrl+L, erreurs en couleur, `effacer`, `quitter`) ; la vraie fenêtre
ouverte, titrée, fermée sur signal, surface rendue.

**Ce qui reste ouvert dans la 16.0**
- Pas de tubes, redirections, jokers, variables ni scripts.
- Une commande longue (`spm install` qui télécharge) fige la fenêtre.
- Console série : pas d'historique aux flèches ni de complétion.
- `spm …` et `dev …` passent par le shell, mais leurs effets sont
  éprouvés par les autotests des phases 8, 9 et 15, pas tapés ici.
- ROOT sur la console de recovery : choix à confirmer par l'auteur
  (DECISIONS, « Recovery : ROOT sur la console série »).
- La fenêtre du terminal n'a été vue que par la console série
  (surface créée, titrée, rendue) : son rendu à l'écran reste à regarder.

#### 16.1 Moteur JavaScript
- [x] 16.1.1 `src/web/js/` compilé en entier (il ne l'est pas : `mod.rs` ne
  déclare que `big` et `num`) ; bornes en pas et en pile (piège « un
  interprète de noyau a deux bornes »)
- [x] 16.1.2 Bibliothèque standard manquante : `Number`, `Math`, `JSON`
  (sur `src/json.rs`), `Date`, `RegExp`, `Map`/`Set`, `Promise` et file de
  microtâches, fonctions globales
- [x] 16.1.3 Autotest : un corpus de cas à la manière de test262, verdict
  série

**Validée le 28 octobre 2026** par l'assistant, sous mandat d'autonomie
de l'utilisateur. `make test` (4 cœurs, deux démarrages), `make
test-smp1`, `make test-novga`, QEMU TCG :
```
[test] phase 16.1 validee - 95 cas du moteur JavaScript (2270 ms)
```
Ce qu'il y a derrière :
- L'existant (lexeur, parseur ES2022, interprète, `Object`, `Function`,
  `Array`, `String`) compilait à cinq erreurs près : deux signatures, un
  double emprunt, et `Num::eq` qui masquait `PartialEq::eq` (renommé
  `equals` : l'égalité IEEE n'est pas l'égalité des bits).
- Écrits : `Number`, `Boolean`, `Symbol` (registre de `Symbol.for`),
  `Math` (35 fonctions ; `exp` et `ln` en virgule fixe 116 bits,
  correctement arrondis — sur 2 000 valeurs, deux écarts avec la libm de
  l'hôte, où c'est la libm qui se trompe), `JSON` (lecteur de
  `src/json.rs` en mode tolérant), `Date` (fuseau fixe de l'hôte),
  `RegExp` (protocole complet autour de `regex.rs`), `Map`/`Set`/`Weak*`
  et l'algèbre des ensembles ES2025, `Promise` (toutes les combinaisons,
  rejets non traités signalés), `console`, minuteries, `queueMicrotask`,
  `structuredClone`, URI, base64, `eval` indirect.
- Le corpus (94 cas + la boucle infinie arrêtée par le carburant) a été
  vérifié contre **Node 22** (fuseau UTC+1, horloge figée) : les attentes
  sont celles du langage, pas celles du moteur. Deux écarts, attendus :
  Node mesure son horloge réelle là où le moteur a une horloge virtuelle.
- Bug évité grâce à l'autotest : une chaîne doublée jusqu'à 32 Mio faisait
  échouer l'allocateur du noyau (`ALLOCATION ECHOUEE`, machine arrêtée).
  Désormais `ops::reserve` demande à l'hôte (`heap::room_for` : 16 Mio
  gardés pour le noyau, le plus grand bloc libre une fois et demie la
  demande) avant toute grosse chaîne ou tout tableau qui grandit, et le
  script reçoit une `RangeError`.

**Ce qui reste ouvert dans la 16.1**
- `BigInt` lu comme un nombre ; pas de `Proxy`, pas d'`Intl` (les
  `toLocaleString` suivent `en-US`), pas de tableaux creux (un trou vaut
  `undefined`), `eval` direct non pris en charge (indirect seulement).
- Générateurs exécutés d'un bloc (valeurs envoyées par `next(v)`
  ignorées) et `await` qui attend sur place : limites de l'interprète
  d'origine, documentées dans `interp.rs`.
- Les collections faibles ne sont pas faibles (libérées avec le royaume).
#### 16.2 Scripts dans le navigateur intégré
- [ ] 16.2.1 DOM : `document`, éléments, attributs, texte, événements,
  minuteries ; une modification du DOM refait la mise en page
- [ ] 16.2.2 `fetch` et `XMLHttpRequest` sur le client de 15.0.4
- [ ] 16.2.3 `about:accueil` ne dit plus « aucune page n'exécute de script »
#### 16.3 Node et Bun (§13.2)
- [ ] 16.3.1 `dev interpret main.js` : le moteur Nova, `console`, `require`
  de modules locaux, `fs` sur `SYS_FS_*` ; la colonne « compilateur »
  reste ETR
#### 16.4 Débogueur (9.1.3)
- [ ] 16.4.1 `dev run --debug` : points d'arrêt (`int3`), pas à pas (TF),
  registres, mémoire, pile d'appels, sur le programme du bac à sable
#### 16.5 Python (9.5.5)
- [ ] 16.5.1 Interprète d'un sous-ensemble défini de Python 3 (indentation,
  entiers illimités, `str`, `list`, `dict`, `tuple`, fonctions, classes,
  exceptions) : `dev interpret main.py`
#### 16.6 C (9.5.2)
- [ ] 16.6.1 Compilateur d'un sous-ensemble défini de C vers x86_64, binaire
  Nova qui s'exécute en ring 3 ; C++ refusé nommément
#### 16.7 Java (9.5.4)
- [ ] 16.7.1 JVM minimale : fichiers `.class`, interprète de bytecode,
  `java.lang` réduit
#### 16.8 C# (9.5.3)
- [ ] 16.8.1 Interprète CIL minimal, ou refus nommé maintenu (à trancher à
  l'étape selon ce que 16.7 aura coûté)

## Phase 17 — Reliquats système de fichiers et POSIX 📝 (planifiée)

Ce que les phases 12 et 14 ont laissé ouvert et qui ne dépend pas de
Firefox. Groupé pour ne rouvrir `fs/lifs`, `fs/partition` et `posix`
qu'une fois.

- [ ] 17.1 Page de temps (`SYS_TIME_PAGE`, 0x0709) lue par la libc Nova
  (12.2.7)
- [ ] 17.2 Autotest appel par appel des domaines 09 à 0B (12.2.8)
- [ ] 17.3 `copy_file_range`, `FICLONE`, `FICLONERANGE` : clone CoW sur LIFS
  (14.4.2)
- [ ] 17.4 Explorateur : les Tag d'un fichier, « étiqueter » (14.4.4)
- [ ] 17.5 GPT de secours lue et réparée ; partitionneur (appel système et
  onglet des paramètres) (14.0.4)
- [ ] 17.6 Arbre B+ : emprunt entre voisins ; transaction plus grosse que
  le journal découpée
- [ ] 17.7 Reprise NovaFS → LIFS dans Nova (14.5.2)
- [ ] 17.8 Export des clones et des Tag éprouvé sur un disque neuf (14.4.3)

## Phase 18 — Firefox sur la pile Nova ⏸ (chez l'auteur)

Regroupe les cases des phases 12 et 13 qui demandent la construction du
fork de Firefox et le GPU de la machine de l'auteur : 12.5 (`nova-cc`),
12.7 (bibliothèques reconstruites, GTK retiré), 12.8.1 à 12.8.4 (paquet
`.spm` du navigateur, zéro appel Linux, racine Alpine retirée), 13.0
(mesures), 13.9 et 13.10 (WebRender sur `nova-gfx`), 13.11.1 (scénario de
charge). Le paquet du navigateur deviendra alors une application de la
phase 15 comme une autre.

## Notes d'utilisation

- Chaque phase est la base de la suivante : ne pas paralléliser entre phases
  éloignées (pas de GUI avant que le scheduler multi-processus soit stable).
- Une case se coche **après** validation par la sortie série, jamais avant.
- Quand une étape révèle une limite qu'elle ne traite pas, l'ajouter à la section
  « Limites connues » en haut de ce fichier plutôt que de la laisser implicite.
- Quand une étape non prévue s'avère nécessaire (comme la 2.9), l'ajouter
  explicitement à la roadmap plutôt que de la fondre dans une étape existante.
