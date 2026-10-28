# Journal des versions

Chaque version publiée a sa section `## [X.Y.Z]` : c'est elle que la
publication automatique reprend comme notes de la release GitHub
(`.github/workflows/release.yml`). Le détail, étape par étape, avec ce qui
a été observé, reste dans `ROADMAP.md` ; les décisions dans `DECISIONS.md`.

Schéma de version : `0.{phase}.{correctif}` — la phase est la dernière de
la roadmap dont le code est complet.

## [0.15.6] — 2026-10-28

Phase 16.4 : un débogueur pour `dev run --debug`.

### Ajouté

- **Débogueur** : `dev run --debug` arrête le programme du bac à sable
  sur son entrée et ouvre une session dans le shell (invite `(ndb)`) :
  `continuer`, `pas [n]`, `suivant`, `arret <symbole|adresse>`,
  `effacer`, `points`, `registres`, `memoire <cible> [n]`,
  `ecrire <registre> <valeur>`, `pile`, `symboles`, `quitter` (alias gdb
  : c, s, n, b, d, r, x, set, bt, q).
- **Noyau** (`crate::debug`) : points d'arrêt `int3` et pas à pas (TF)
  transmis au débogueur, la tâche tracée endormie dans son exception ;
  registres lus et **écrits** ; mémoire du programme lue et écrite (code
  en lecture seule compris) ; passage d'un point d'arrêt invisible.
- Autotest 16.4 : une session tapée dans un shell, jusqu'à changer un
  registre et constater le code de sortie qu'il explique
  (`[test] phase 16.4 validee`).

### Modifié

- La porte du vecteur 3 est appelable du ring 3 : un `int3` d'un
  programme donne #3 (et non plus #GP).
- IA32_FMASK masque TF à l'entrée de `syscall`.

### Corrigé

- **Ordonnanceur** : une tâche tuée pendant qu'elle tournait pouvait
  revenir à la vie si elle s'endormait juste après (`arm_sleep` et
  `block_current` écrasaient l'état `Dead`).

## [0.15.5] — 2026-10-28

Phase 16.3.2 : modules ES et TypeScript, dans `dev interpret` comme dans
le navigateur.

### Ajouté

- **Modules ES** dans le moteur (`web::js::module`) : `import` et
  `export` sous toutes leurs formes (par défaut, nommés, renommés,
  espace de noms, réexports, `export *`), **liaisons vivantes** (un
  compteur exporté se lit à jour, les cycles d'imports marchent avec les
  fonctions hissées), `import()` dynamique, `import.meta`, `await` au
  premier niveau.
- **TypeScript à types effacés** (la ligne Bun du §13.2) : annotations,
  génériques, `as`, `satisfies`, `!`, `interface`, `type`, `declare`,
  surcharges, classes abstraites, modificateurs, signatures d'index,
  `enum` et propriétés de paramètres traduits comme `tsc`.
- **`dev interpret`** : `.mjs`, `.cjs`, `.ts`, `.mts`, `.cts` ; un module
  ES importe CommonJS, JSON (`with { type: "json" }`) et les modules
  intégrés (`node:fs`...) ; `require` d'un module ES ; un `.ts` reçoit
  `require` et `__dirname` comme sous Bun ; `./lib.js` trouve `lib.ts`.
- **Navigateur** : `<script type="module">` (différés, avant
  `DOMContentLoaded`), leurs imports récupérés à la demande.
- `make js-test` : 8 cas de modules et de TypeScript ; `jshost --module
  fichier` exécute un module du disque sur l'hôte.

### Modifié

- `web/js/parse.rs` (1 569 lignes) scindé en `parse/` : instructions,
  expressions, fonctions et classes, modules, TypeScript.

### Vérifié contre les références

- Modules : Node 22 ; TypeScript : Bun (et Node 22 avec
  `--experimental-transform-types`) ; module de page : Chromium.

### Limites connues

- `namespace`, décorateurs et JSX refusés nommément ; aucun type
  vérifié.
- Un `await` au premier niveau sans `import`/`export` ne fait pas d'un
  `.js` un module.

## [0.15.4] — 2026-10-28

Phase 16.3.1 : `dev interpret main.js`, des programmes Node sur le moteur
de Nova.

### Ajouté

- **`dev interpret` pour JavaScript** (`sdk::js`) : `require` de modules
  locaux (`.js`, `.json`, `index.js`, cache, cycles, nouvel essai après
  échec), `module`, `exports`, `require.main`, `__filename`,
  `__dirname` ; modules intégrés `fs` (synchrone, sur `SYS_FS_*`, avec
  les droits de la session), `path`, `os`, `util`, `events` ; `process`
  (`argv`, `env`, `exit`, `exitCode`, `cwd`, `nextTick`,
  `stdout.write`). Le script tourne dans sa propre tâche (pile de
  1 Mio) ; les minuteries s'exécutent sans attente réelle.
- La ligne Node.js du §13.2 passe à « interprété » ; la colonne
  compilateur reste ETR.
- Autotest 16.3 : trois programmes dont la sortie est comparée ligne à
  ligne à celle de Node 22, puis `dev interpret` par la ligne de commande
  (`[test] phase 16.3 validee`).

### Limites connues

- `await` attend sur place (16.1) : le code écrit après l'appel d'une
  fonction `async` qui attend s'exécute après l'attente.
- Ni modules ES (`import`/`export`), ni TypeScript : c'est 16.3.2.

## [0.15.3] — 2026-10-28

Phase 16.2 : le navigateur intégré exécute les scripts des pages.

### Ajouté

- **DOM** (`web::js::dom`) : `Node`, `Element`, `Text`, `Document`,
  `querySelector(All)` sur les sélecteurs de la cascade, `classList`,
  `style`, `dataset`, `innerHTML`/`outerHTML`, contrôles de formulaire,
  `getBoundingClientRect` ; événements (capture, cible, bouillonnement,
  gestionnaires en ligne, `Event`/`CustomEvent`) ; `window`, `location`,
  `history`, `navigator`, `localStorage` (persistant) et
  `sessionStorage`, `matchMedia`, `requestAnimationFrame`, dialogues ;
  `fetch`, `Headers`, `Response`, `XMLHttpRequest`.
- **Navigateur** : scripts en ligne et externes exécutés, mise en page
  refaite après chaque changement du document (y compris les `<style>`
  ajoutés), clics donnés à l'élément sous le pointeur, `keydown`,
  `input`, `change`, `submit` annulables, minuteries, `about:console`
  (Ctrl+J), `document.cookie` sans les cookies `HttpOnly`.
- `web::tree` : l'arbre modifiable ; boîtes des éléments dans la mise en
  page (`Page::boxes`).
- Autotest 16.2 : 30 cas du DOM vérifiés contre jsdom, 12 vérifications du
  navigateur de bout en bout (`[test] phase 16.2 validee`).

### Corrigé

- **Noyau** : une tâche morte pouvait être récoltée pendant que son cœur
  la quittait encore — sa pile et sa case réutilisées, puis écrasées par
  le changement de contexte (saut en 0, cœur arrêté ; un démarrage sur
  une quarantaine). `scheduler::LEAVING`.
- La cascade CSS supposait l'ordre de l'arène égal à celui du document,
  ce que les scripts démentent : elle suit l'arbre.

### Limites connues

- `fetch`/`XMLHttpRequest` synchrones sous la promesse (la fenêtre attend
  le transfert) ; pas de `Proxy` (`dataset` sans nouvelles clés), de
  `MutationObserver`, de `FormData`, d'images ni de canvas.

## [0.15.2] — 2026-10-28

Phase 16.1 : le moteur JavaScript est compilé, complet et éprouvé.

### Ajouté

- `src/web/js/` compilé en entier : lexeur et parseur ES2022, interprète
  borné (pas, pile, mémoire), file de tâches et minuteries.
- Bibliothèque standard : `Number`, `Boolean`, `Symbol`, `Math` (`exp` et
  `ln` correctement arrondis, en virgule fixe 116 bits), `JSON`, `Date`
  (fuseau de Nova), `RegExp`, `Map`/`Set` (et l'algèbre des ensembles
  ES2025), `WeakMap`/`WeakSet`/`WeakRef`, `Promise` (`all`, `allSettled`,
  `any`, `race`, `withResolvers`, `try`), `console`, minuteries,
  `queueMicrotask`, `structuredClone`, fonctions URI, `atob`/`btoa`.
- Autotest 16.1 : 94 cas vérifiés contre Node 22, plus la boucle infinie
  arrêtée par le carburant (`[test] phase 16.1 validee`).
- `make js-test` : le moteur compilé pour l'hôte, son corpus en 0,2 s ;
  dans la CI.
- `json::Options::LENIENT` : lecture de `JSON.parse` (dernière clé en
  double gagnante, surrogats isolés), index au-delà de 16 membres.

### Corrigé

- Un script qui construisait une très grande chaîne ou un très grand
  tableau arrêtait la machine (allocateur du noyau à court) : il reçoit
  désormais une `RangeError` (`heap::room_for`, `ops::reserve`).
- `Number("1_0")` valait 10 ; `Math.exp(1)` ne valait pas `Math.E`.

### Limites connues

- Pas encore branché sur le navigateur intégré (16.2) ni sur
  `dev interpret` (16.3).
- Ni `BigInt`, ni `Proxy`, ni `Intl` ; tableaux sans trous ; `eval`
  indirect seulement.

## [0.15.1] — 2026-10-28

Première étape de la phase 16 : Nova a un shell.

### Ajouté

- **Shell** (spec §1.1 « Shell / CLI ») : `aide`, `liste`, `va`, `ici`,
  `lire`, `ecrire`, `dossier`, `supprimer [-r]`, `copier`, `deplacer`,
  `lier`, `info`, `arbre`, `disques`, `taches`, `tuer`, `lance`,
  `memoire`, `date`, `depuis`, `systeme`, `qui`, `historique`, `effacer`,
  `quitter`, et `spm …`, `dev …` enfin tapables. Noms français, alias
  usuels (`ls`, `cd`, `cat`, `rm`, `cp`, `mv`, `ps`, `kill`, `df`,
  `uname`…). Droits du compte de la session : le shell n'accorde rien.
- **Terminal** (`terminal.ss`) : fenêtre, historique aux flèches, édition
  de la ligne, défilement (Page préc./suiv., molette), Ctrl+C, Ctrl+L,
  Ctrl+U, lignes coupées à la largeur.
- **Mode recovery** (spec §2.2) : l'entrée « recovery » de GRUB démarre le
  noyau, les services core et un shell sur la console série, sans
  autotests, pilotes, réseau ni interface.
- Shell série aussi en démarrage normal : silencieux tant qu'on ne tape
  rien.
- `make test-recovery` : démarrage en recovery, commandes tapées sur la
  console série et réponses jugées ; dans `make test-all` et la CI.
- Autotest 16.0 : 60 vérifications (shell, droits, terminal au clavier
  synthétique, fenêtre réelle ouverte puis fermée).

### Limites connues

- Pas de tubes, redirections, jokers ni scripts.
- Une commande longue fige la fenêtre du terminal le temps qu'elle dure.
- La console de recovery tourne en ROOT (choix à confirmer).

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
