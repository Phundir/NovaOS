# Nova OS — Décisions d'architecture & pièges connus

Ce fichier a deux fonctions : empêcher de revenir sur des choix déjà arbitrés,
et éviter de réintroduire un bug déjà payé une fois.

**À lire avant d'écrire du code.** Une décision listée ici ne se remet pas en
question sans en discuter d'abord avec l'utilisateur.

---

## 1. Décisions issues de la spécification — non négociables

Ces points viennent de `SPECIFICATION.md` et ont été confirmés lors de la
phase de conception. Ce ne sont pas des réglages.

| Décision | Référence |
|---|---|
| Les priorités des 5 formats binaires sont **fixes**, jamais ajustables par l'utilisateur | §3.1, §7.1 |
| Les `.ras` sont **totalement isolés** de l'IPC — pas d'exception, pas de contournement | §12.1, §12.3 |
| Le GUI est une application `.ss` remplaçable, avec des modules `.mod` chargés dynamiquement | §8.1, §8.2 |
| Le compositeur est le **seul** composant autorisé à écrire sur le framebuffer | §8.1 |
| Le cœur 0 est réservé aux `.dsa` — jamais partagé | §7.2 |
| Un dossier n'est pas une entité réelle mais une table de liens vers des nœuds | §4.1 |
| `AppIdentity` est fixée à la compilation et immuable à l'exécution | §12.1 |
| Modèle de permissions hybride : liste blanche déclarée dans le header + liste noire absolue | §12.3 |

---

## 2. Décisions techniques prises pendant l'implémentation

### Toolchain et build

- **`staticlib` plutôt que crate binaire.** Le crate Rust produit
  `libnova_os.a`, que le Makefile lie manuellement avec les objets NASM et
  `boot/linker.ld` via `ld.lld`. C'est ce qui permet de contrôler exactement la
  disposition mémoire et de mêler code 16/32/64 bits.
- **Aucune dépendance externe.** Pas de `spin`, `lazy_static`, `x86_64`,
  `bitflags`. Le `SpinLock`, la GDT, l'IDT, le parsing ACPI, l'allocateur : tout
  est écrit à la main. Choix assumé — le noyau ne contient que du code auditable.
  **Exception décidée par l'utilisateur le 22 septembre 2026** : une
  application complexe comme le navigateur peut reprendre du code tiers.
  Le navigateur sera donc un fork de Firefox (Gecko, SpiderMonkey,
  WebRender) adapté au format `.cus`, avec une interface propre à Nova.
  L'exception ne couvre **que** ces applications : le noyau, les pilotes et
  les services restent sans dépendance. Le fork ne porte pas la marque
  « Firefox », qui est déposée ; la licence MPL 2.0 impose de publier les
  fichiers de Mozilla qu'on modifie.
- **`build-std` recompile `core`, `alloc` et `compiler_builtins`** pour la target
  custom. `compiler-builtins-mem` est indispensable, sinon `memcpy`/`memset`
  restent non résolus à l'édition de liens.
- **`json-target-spec`** doit figurer dans `[unstable]` de `.cargo/config.toml` :
  les nightly récentes refusent les target JSON sans ce flag.
- **`code-model: kernel`** depuis le passage en higher-half. `small` supposait
  que tout tenait dans les 2 premiers GiB.

### Mémoire

- **Frame allocator bitmap**, pas free-list. Le bitmap permet de *marquer* des
  régions entières comme réservées (kernel, mémoire basse, structure Multiboot2),
  ce qu'une free-list ne fait pas naturellement.
- **Le bitmap vit dans un statique `.bss`.** Allouer le bitmap de l'allocateur
  demanderait un allocateur : problème de l'œuf et de la poule résolu par une
  réservation statique de 2 MiB.
- **Heap free-list**, pas buddy. Le kernel alloue peu et par gros blocs ; la
  fragmentation n'est pas le problème dominant. La fusion des blocs adjacents se
  fait à l'insertion, pas par un balayage postérieur.
- **Tout le premier MiB est réservé** au frame allocator : IVT, BDA, EBDA, zone
  VGA, ROM BIOS, et le trampoline SMP à `0x8000`.

### Disposition virtuelle

| Région | Base | Contenu |
|---|---|---|
| Fenêtre physique | `0xFFFF800000000000` | 4 GiB en pages de 1 GiB — couvre le MMIO APIC |
| Heap kernel | `0xFFFF808000000000` | 4 MiB en pages de 4 KiB |
| Kernel | `0xFFFFFFFF80000000` | Code, données, pile — pages de 2 MiB |
| Code utilisateur | `0x400000` | Pages 4 KiB, bit USER, non modifiable |
| Pile utilisateur | `0x800000` | Pages 4 KiB, bit USER, modifiable, NX |
| Arène des images | `0x1000000` | 32 créneaux de 2 MiB — chargeur 3.2 |
| Arène des piles | `0x10000000` | 32 créneaux de 1 MiB, page de garde en bas |
| Trampoline SMP | `0x8000` | Physique — contrainte du mode réel |

- **L'identity mapping est supprimé** après la transition higher-half, et
  réinstallé temporairement le temps de démarrer les cœurs secondaires.

### Ordonnancement

- **Commutation coopérative aux points `preempt_if_needed()`**, pas depuis le
  handler d'interruption. Le timer lève un drapeau, la commutation a lieu au
  retour dans un contexte maîtrisé. Éviter de reconstruire la trame de retour à
  la main.
- **`syscall_dispatch` consomme le drapeau à sa sortie.** C'est ce qui rend le
  code ring 3 préemptible *aux frontières d'appel système* — sans cela rien ne
  consommait `NEED_RESCHEDULE` pour une tâche utilisateur, puisqu'un programme
  ring 3 ne peut pas appeler `preempt_if_needed`. La commutation n'y est sûre
  que parce que chaque tâche possède sa propre pile noyau (piège n° 23).
  *Conséquence assumée : une boucle ring 3 qui n'appelle jamais le noyau garde
  son cœur jusqu'à l'échéance de sa tranche.*
- **Table de tâches unique, `current` par cœur.** Plutôt que N files physiques
  avec leurs verrous et le risque d'interblocage au vol de tâches. Une tâche
  passée `RUNNING` sous verrou n'est plus éligible : deux cœurs ne peuvent pas
  se disputer la même.
- **Blocage IPC via `State::Sleeping`**, avec vérification et endormissement
  sous interruptions masquées — voir piège n° 7 ci-dessous.

### Formats binaires

- **Header de 256 octets, lu champ par champ** avec `from_le_bytes`. Jamais de
  `&*(ptr as *const Header)` : un fichier n'offre aucune garantie d'alignement.
- **Le type est déclaré deux fois** — dans le magic number et dans un champ
  dédié. Toute divergence fait rejeter le binaire : protection contre un fichier
  dont on aurait changé l'extension.
- **Un bit de permission inconnu fait rejeter le binaire**, il n'est pas ignoré.
  Accorder silencieusement une permission incomprise serait une faille.
- **Distinction rejet / refus** : *rejeté* = malformé (magic inconnu, tronqué),
  *refusé* = bien formé mais viole une règle de la spec. SPM doit pouvoir
  expliquer la différence à l'utilisateur.

### Chargeur et signatures (phase 3.2 à 3.4)

- **La table de relocations est une section, pas un champ de header.** Le header
  fait 256 octets et ils sont pleins. Une section de nature `Reloc` — présente
  dans le fichier, absente de la mémoire — laisse le format s'étendre sans
  toucher à une disposition figée. Même mécanisme pour le certificat.
- **Une section par page.** La pagination protège à la granularité de la page :
  un `.text` et un `.data` voisins dans une même page obligeraient à choisir
  entre une page RWX et une section cassée. Le format impose donc au producteur
  d'aligner ce qui doit être protégé séparément, et le chargeur refuse le
  contraire. C'est une contrainte de format, pas une limite d'implémentation.
- **Relocation exercée, pas théorique.** L'arène du chargeur commence à
  `0x1000000` alors que le SDK lie à `0x400000` : *toute* image est relocalisée.
  Un chemin de code emprunté à chaque chargement ne peut pas pourrir en silence.
- **Toutes les frames sont prises d'un bloc avant le premier mapping.** Échouer
  sur `OutOfFrames` à mi-parcours obligerait à démonter une image à demi
  construite ; il vaut mieux échouer avant d'avoir rien installé.
- **La charge utile hachée couvre les tables, pas seulement le code.** Elle va
  de la fin du header au début du certificat : sans cela, une table de
  relocations falsifiée passerait sous le radar de l'empreinte.
- **Le certificat est en fin de fichier, hors de la charge utile.** Il contient
  une liaison vers l'empreinte du binaire ; s'il était couvert par cette
  empreinte, elle se référerait à elle-même.
- **SYSTEM est solide, ENTERPRISE ne l'est pas encore.** L'ancrage de SYSTEM est
  l'image du noyau — modifier un binaire officiel change son empreinte, qui ne
  figure plus dans la table. ENTERPRISE repose sur un hachage dont l'entrée est
  publique : la structure et le magasin de racines sont en place, la primitive
  Ed25519 qui rendrait la liaison infalsifiable manque. **Ne jamais présenter
  ENTERPRISE comme une preuve d'authenticité** tant que c'est le cas.
- **Un `.ras` ne peut revendiquer aucune signature.** La spec n'en exige aucune ;
  lui laisser en déclarer une ouvrirait un blanchiment de privilèges, le `.ras`
  étant à la fois le format que n'importe qui produit et celui qui réclame le
  plus d'accès matériel.
- **Une pile noyau par tâche, pas par cœur.** Voir piège n° 23.
- **W^X vérifié après coup.** `loader::verify_protection` relit la hiérarchie de
  pages plutôt que les intentions du chargeur : c'est la seule façon de prouver
  que la protection est en place et non simplement demandée.

---

## 3. Pièges déjà rencontrés — ne pas les repayer

### 1. `lretq` n'existe pas en syntaxe Intel
L'assembleur intégré de LLVM attend **`retfq`**. `lretq` est la forme AT&T,
acceptée par GAS mais pas par `asm!`.

### 2. `function_casts_as_integer`
Convertir directement un pointeur de fonction en `u64` déclenche ce lint.
Passer par `as *const ()` d'abord. La bonne correction est de changer la
signature de la fonction réceptrice pour qu'elle prenne un `*const ()`, pas
d'ajouter le cast à trente endroits.

### 3. Fenêtre physique trop petite
La version initiale ne mappait que 1 GiB. Le LAPIC est à `0xFEE00000`, soit
presque 4 GiB : faute de page immédiate. **La fenêtre doit couvrir 4 GiB**,
mappés en 4 pages de 1 GiB dans la PDPT.

### 4. MMIO caché
Sans les bits `PCD` et `PWT` sur les pages APIC, les écritures sur les registres
peuvent rester en cache et n'atteindre jamais le contrôleur. `set_mmio_uncached`
doit être appelée avant la première écriture.

### 5. Interblocage SpinLock ↔ handler d'interruption
Un handler qui appelle `print!` prend le verrou VGA. Si le code interrompu le
détenait déjà, attente infinie. **Le `SpinLock` masque les interruptions à
l'acquisition et les restaure au `Drop`.** Le garde porte un champ
`restore_interrupts` pour savoir s'il faut réarmer.

### 6. Verrou détenu pendant `switch_context`
Bloque tous les autres cœurs jusqu'au retour de la tâche sortante. Extraire les
pointeurs sous verrou, relâcher, **puis** commuter.

### 7. Course entre vérification et endormissement (IPC)
Si une préemption survient entre « pas de message » et « je m'endors », un
expéditeur peut livrer et réveiller une tâche encore `RUNNING`, qui s'endormira
ensuite définitivement. **La séquence test-puis-dors doit être atomique** :
interruptions masquées autour.

### 8. EOI avant l'appel au scheduler
Si la commutation intervient avant l'acquittement, le LAPIC considère
l'interruption toujours en service et n'en délivre plus aucune. Ordre correct :
`end_of_interrupt()` **puis** `scheduler::on_timer_tick()`.

### 9. Le vecteur spurious ne doit jamais être acquitté
Contrairement à tous les autres handlers.

### 10. Ordre des descripteurs GDT imposé par `sysret`
`sysret` calcule ses sélecteurs par arithmétique sur `STAR[63:48]` : code
utilisateur à base+16, données à base+8. D'où un descripteur de code 32 bits
ring 3 inutilisé mais **obligatoire** en 0x18. Le TSS passe en 0x30.

### 11. Permissions de page conjonctives
Un accès ring 3 exige le bit `USER` sur **les quatre niveaux** de la hiérarchie,
pas seulement sur la page finale. `next_table_or_create` doit propager le bit
aux tables intermédiaires.

### 12. `syscall` ne change pas de pile
Contrairement à une interruption. Le basculement se fait à la main via `swapgs`
et la structure `PerCpu`. `IA32_FMASK` doit masquer `IF` pour qu'aucune
interruption n'arrive pendant la fenêtre où le noyau n'a pas de pile valide.

### 13. TSS.RSP0 obligatoire dès qu'un code ring 3 tourne
C'est la pile sur laquelle le CPU bascule quand une interruption survient en
ring 3. Une par cœur, distincte de la pile `syscall`.

### 14. Timer LAPIC par cœur
Ce n'est pas une horloge partagée. Chaque cœur arme le sien. En revanche la
fréquence du bus est commune : seul le BSP calibre contre le PIT, les AP
réutilisent la mesure — le PIT est unique sur la machine et ne supporte pas deux
calibrations simultanées.

### 15. `busy_wait_micros` réquisitionne le timer
Sauvegarder `LVT_TIMER` et le compteur de rechargement, puis les restaurer.
Sans quoi la préemption s'arrête après le premier appel.

### 16. Un AP démarre en mode réel 16 bits
Le trampoline doit vivre sous 1 MiB, aligné sur 4 KiB, et refaire tout le chemin
réel → protégé → long mode. L'identity mapping doit être réinstallé le temps du
démarrage.

### 17. Section `.boot` liée en dessous de 4 GiB
Le code 32 bits ne peut pas adresser plus haut. Seul `.text` et les suivantes
basculent en higher-half via `AT()` dans le script de liaison.

### 18. `-n` (nmagic) obligatoire pour `ld.lld`
Sans lui, le linker aligne les sections sur des frontières de page dans le
fichier de sortie, et le header Multiboot2 risque de sortir des 32 premiers Kio.

### 19. Caractères non-ASCII dans les chaînes affichées
Le writer VGA remplace tout octet hors de la plage 0x20–0x7E par un carré plein,
et la console graphique de la phase 7.1 fait de même. Les tirets cadratins et
les accents apparaissent comme `■`. **ASCII uniquement** dans `kprintln!`,
`println!` et `serial_println!`.

La règle était écrite depuis le début et **233 tirets cadratins l'enfreignaient
quand même**, répartis dans 55 fichiers. Personne ne l'avait vu parce que la
console série, elle, affiche l'UTF-8 correctement : la faute ne se voyait que
sur l'écran, où le mode texte 80×25 la noyait. Passer en mode graphique l'a
rendue criante, un `■■■` par ligne.

La leçon n'est pas « respecter la règle » mais **« une règle que rien ne
vérifie ne tient pas »**. Le remède appliqué est un script qui analyse les
littéraux de chaîne Rust — en distinguant vraiment les chaînes des
commentaires, où le français accentué reste autorisé — et remplace les
caractères typographiques. Le refaire tourner avant une revue coûte une
seconde.

### 20. Cible `.PHONY` en collision avec un dossier
Une cible nommée `build` alors qu'un dossier `build/` existe produit
`Circular build dependency dropped`. Renommer la cible (`kernel`).

### 21. Récursion de fusion et emprunt mutable
La première version de `push_free` prenait deux emprunts mutables simultanés
(E0499). La solution n'est pas de contourner le borrow checker mais de repenser
l'algorithme : fusionner **à l'insertion**, en traitant les deux seuls cas
possibles (le bloc prolonge son prédécesseur, ou précède son successeur).

---

### 22. `swapgs` ne sait rien du niveau de privilège
Sa correction tient à une convention qui doit valoir **à tout instant** : en
ring 0 `GS.base` porte la structure du cœur et `KERNEL_GS_BASE` vaut 0, en
ring 3 l'inverse. La version initiale ne posait que `KERNEL_GS_BASE`, en
s'appuyant sur le fait qu'un cœur démarre en ring 0 avec `GS.base` à zéro.

Cela tenait tant qu'une **seule** tâche passait en ring 3. Dès que deux le font
sur le même cœur, une tâche préemptée au milieu d'un appel système laisse
`GS.base` à la valeur noyau ; la tâche suivante franchit `iretq` sans le
corriger, et son premier `syscall` exécute un `swapgs` qui remet `GS.base` à
zéro. Faute de page sur `gs:0x8` dès la première instruction du gestionnaire.

Correction : `syscall::init` pose **les deux** MSR, et `user::enter_ring3`
exécute un `swapgs` avant son `iretq` — `iretq` ne le fait pas tout seul,
contrairement à `syscall`/`sysret`.

### 23. Une pile noyau par cœur ne suffit pas
`PerCpu.kernel_rsp` et `TSS.RSP0` désignaient une pile unique par processeur.
Une tâche ring 3 qui cède le CPU au milieu d'un appel système laisse sa trame
sur cette pile ; la tâche ring 3 suivante, sur le même cœur, repart du même
sommet et l'écrase. Le bogue reste invisible avec une seule tâche utilisateur.

La pile noyau doit être **celle de la tâche**, réinstallée à chaque
commutation. C'est correct parce qu'une tâche en ring 3 n'a rien d'utile sur sa
pile noyau : le gestionnaire d'appel système la vide avant de rendre la main.

### 24. `unmap_page` laissait fuir les tables intermédiaires
Chaque application chargée puis fermée abandonnait les trois ou quatre cadres de
sa hiérarchie de pages. Invisible dans les compteurs de pages utilisateur —
aucune page utilisateur n'est concernée — mais bien réel. L'élagage des tables
devenues vides est limité à la **moitié basse** : celles de la moitié haute
décrivent le noyau et sont partagées par tous les cœurs.

### 25. Le reliquat de tête d'une allocation alignée était perdu
`Heap::alloc` réinsérait le reliquat de queue d'un bloc mais pas celui de tête,
laissé par l'arrondi d'alignement. Fuite lente, proportionnelle au nombre
d'allocations fortement alignées. Le contrôle est le couple
`{octets alloués, octets libres}` du moniteur : il doit rester **strictement
stable** sur une charge cyclique, pas seulement « à peu près ».

### 26. `make re` échouait sur des objets absents
`re: clean all` ne fonctionne pas : make résout tout le graphe de dépendances
avant d'exécuter la moindre recette, il considère donc les `.o` comme à jour au
moment où `clean` les supprime. Deux sous-`make` successifs.

### 27. Adressage ambigu en 64 bits sous NASM
`mov [label], reg` dans une section `bits 64` laisse NASM choisir entre absolu
et RIP-relatif ; il avertit tant que `default abs` (ou `default rel`) n'est pas
déclaré. Le trampoline s'exécute à l'adresse pour laquelle il est lié : c'est
bien `abs` qu'on veut. L'avertissement ne se voit qu'à la **reconstruction
complète**, les `.o` étant mis en cache — d'où l'intérêt d'un `make re` qui
marche.

### 28. Déclarer un canal IPC n'est pas l'ouvrir
`drivers::register` prend un nom de canal et le range dans le registre des
pilotes ; c'est une **déclaration d'intention**. Le canal n'existe qu'après
`ipc::open_channel`. Les pilotes réseau et audio faisaient le premier appel,
pas le second, et journalisaient pourtant `pret - canal "dev.net"`. Le nom
était donc introuvable pour un client, alors que le journal affirmait le
contraire — la pire combinaison, celle où la trace ment.

Deux causes possibles à cette asymétrie, et les deux étaient présentes : ces
deux pilotes **scrutent** au lieu d'attendre sur `recv`, et n'avaient donc
aucune boucle de service à laquelle rattacher le canal. La correction ouvre le
canal **avant le sondage du matériel** — pour qu'un client puisse distinguer
« pas de pilote » de « pas de matériel » — et draine les requêtes par
`try_recv` en tête de chaque tour de scrutation.

Invariant tiré de là, ajouté en fin de document : *un pilote `pret` a ouvert
son canal nommé.*

### 29. L'étiquette d'un message IPC est le numéro d'opération
`init::service_hw` branche sur `request.tag`. L'autotest de la phase 6 lui
envoyait `0x6501`, une valeur arbitraire choisie comme identifiant de
corrélation. Le service tombait dans sa branche par défaut, répondait une
charge utile vide, et l'autotest concluait « le service matériel ne répond
pas ».

Le service allait parfaitement bien. **Une valeur qui sert à la fois de numéro
d'opération et de clé de corrélation ne peut pas être choisie librement par
l'appelant** ; c'était au test de s'aligner sur le protocole, pas l'inverse.
Quand un autotest échoue sur un composant qui semble sain, suspecter d'abord
le test.

---

## Export multi-format (4.6) — pièges rencontrés

**Un index de support est une identité, pas une position dans une file.**
`export_to_medium` retirait le support du vivier le temps de l'écriture
puis le remettait **en fin de liste**. Avec un seul support, invisible ;
avec trois, exporter vers l'index 0 décale tout, et la vérification qui
suit relit un autre support que celui qu'on vient d'écrire. Le symptôme
— « image FAT32 illisible » sur une image que `fsck.vfat` valide depuis
l'hôte — désigne le lecteur, pas l'écrivain. `insert(index, device)`.

**`link` est autorisé sur un dossier (§5.1), donc l'arborescence est un
graphe.** Rien n'interdit qu'un dossier se contienne lui-même par un
lien. Tout parcours en profondeur doit retenir les identifiants de nœud
déjà visités, faute de quoi il ne termine pas. `check` et `dump_tree`
étaient déjà à l'abri — l'une balaye la table des nœuds linéairement,
l'autre est bornée en profondeur — mais pas l'exporteur FAT32.

**NTFS impose la substitution de fin de secteur.** Les deux derniers
octets de chaque secteur d'un enregistrement MFT ou d'un bloc d'index ne
sont pas des données : ils portent un numéro de séquence, et les octets
réels vivent dans un petit tableau en tête. Une structure écrite sans
cette substitution est refusée par le pilote, sans message utile.

**Le journal NTFS vide vaut 0xFF, pas zéro.** `$LogFile` rempli de zéros
est lu comme une page de redémarrage corrompue ; rempli de `0xFF`, il est
reconnu comme « rien à rejouer ». C'est ce qu'écrit `mkntfs`.

**Un volume ext4 sans `lost+found` est signalé par `e2fsck`.** Le créer
au moment de l'export coûte un bloc et évite qu'une vérification de
routine ressemble à une anomalie.

**Le bourrage des bitmaps ext4 doit valoir 1.** Les bits au-delà du
dernier bloc du groupe, et au-delà du dernier inœud, décrivent des objets
qui n'existent pas : les laisser à zéro fait croire à de la place libre
hors du volume, et `e2fsck` le corrige.

**`u8 * 31` déborde en profil debug.** Le générateur d'identifiant de
volume multipliait un index de 0 à 15 par 31 : au-delà de 8, panique.
Le noyau tourne en debug, où toutes les opérations entières sont
vérifiées — un dépassement silencieux en release y devient une panique.

**Une table d'indirection n'était jamais rendue.** Un fichier passé de
plusieurs mébioctets à quelques octets libérait ses blocs de données mais
gardait le bloc de la table, marqué occupé et pointé par un nœud qui ne
s'en servait plus. `check` ne le voyait pas : il vérifiait que tout bloc
*référencé* est marqué, jamais l'inverse. Deux corrections : `release_from`
rend la table dès que plus rien au-delà des pointeurs directs n'est
décrit, et `check` compte désormais la table d'indirection et signale
celle qui ne décrit plus rien. La leçon est générale — **un invariant qui
ne vérifie qu'un sens ne détecte pas les fuites.**

**Une écriture qui échoue faute de place doit rendre ce qu'elle a pris.**
`write_all` allouait bloc par bloc ; à la première erreur, `node.size`
n'ayant pas été mis à jour, les blocs déjà pris devenaient inatteignables
tout en restant marqués occupés.

---

## API système (phase 5) — arbitrages et pièges

**`SysResult<T>` de la spec §6.1, c'est un `Result` Rust.** La spec veut
`success`, `data` et `error` dans un même objet. Un `Result` porte
exactement cela, avec en plus l'impossibilité de lire `data` sans avoir
constaté `success`. Inventer une structure à trois champs aurait donné le
même contenu et retiré cette garantie.

**Le chemin d'erreur ne doit rien allouer.** Une erreur peut survenir
*parce que* le tas est plein ; construire une `String` pour l'expliquer
échouerait à son tour. D'où le contexte de taille fixe, tronqué plutôt
qu'alloué.

**`SETPRIO` ne contredit pas le §7.1 : il agit sur l'autre grandeur.**
Le §7.1 pose lui-même son exception — « sauf pour les `.gs` qui basculent
automatiquement entre avant et arrière-plan » — et le §7.3 donne la
tranche en deux colonnes par format. La classe de priorité (fixée par le
format, gouvernant l'élection et l'affinité) et la tranche CPU (variable
selon l'état d'affichage) sont deux choses distinctes que la spec ne
confond jamais. `SETPRIO` porte sur la seconde, et la table du §7.3
**énumère elle-même** les formats concernés : `.dsa` « jamais réduit » et
`.ras` « jamais modifiable » sont refusés, `.ss`, `.gs` et `.cus`
acceptés. Le refus nomme la classe fixe de la tâche visée, pour que
l'appelant constate qu'elle n'est pas négociable.

**Descendre en arrière-plan est permis à tous, remonter est réservé.**
Si une application pouvait se déclarer au premier plan, le « CPU réduit à
2 % » du §3.4 ne vaudrait rien : il suffirait d'un appel pour refuser
d'être mis en pause. Seuls `.dsa`, `.ss` — donc le compositeur du §8.1 —
ROOT et ADMIN promeuvent. Le mécanisme (`set_foreground`) existait depuis
la phase 3 ; ce qui manquait, c'était la politique.

**Le réveil perdu ne se voit pas en monoprocesseur.** Endormir une tâche
suppose de poser l'état SLEEPING **avant** de tester la condition. Poser
l'état après laisse un autre cœur satisfaire la condition, appeler `wake`
sur une tâche encore RUNNING — sans aucun effet — puis la voir s'endormir
définitivement. Masquer les interruptions ne ferme pas cette fenêtre :
elle est ouverte par le parallélisme, pas par la préemption. `ipc::recv`
portait ce défaut depuis la phase 2 et passait tous les tests, parce
qu'ils tournaient sur un seul cœur. D'où `arm_sleep` / `cancel_sleep`.

**Une tâche marquée morte peut encore s'exécuter.** `kill` pose l'état
depuis un autre cœur ; le cœur qui la fait tourner ne l'abandonne qu'à sa
prochaine élection. Lui reprendre sa pile à ce moment revient à la retirer
sous ses pieds. `try_reap` refuse donc de récupérer une tâche qu'un cœur
désigne encore comme courante, et `wait_for` cède le CPU sans dormir tant
que c'est le cas — personne ne l'annoncera une seconde fois.

**`Frame(u64)` porte un numéro de cadre, pas une adresse.** Le confondre
avec une adresse physique mappe tout un espace virtuel sur la page
physique 0 : l'écriture et la lecture réussissent, la mémoire semble
fonctionner, et deux régions distinctes se recouvrent silencieusement.
Seul un contrôle de remise à zéro l'a révélé. Toujours passer par
`start_addr()`.

**L'origine d'un appel ne dit rien de l'identité de l'appelant.**
`Call::who` renvoyait les droits du noyau dès que l'appel venait du noyau,
ce qui rendait `SYS_USER_SETID` sans effet : une tâche qui abandonnait ses
droits les retrouvait au premier appel interne. L'identité se lit toujours
dans l'ordonnanceur ; seul le code de démarrage, qui n'est pas une tâche,
agit au nom du noyau.

**`is_rejection()` n'est pas une classification d'erreur ABI.** Le
chargeur distingue *rejeté* (malformé) de *refusé* (interdit) ; s'en
servir tel quel pour choisir un code d'erreur système faisait remonter
`PERMISSION_DENIED` sur un fichier qui n'était simplement pas un binaire.
Les deux diagnostics n'appellent pas la même correction : reconstruire le
binaire, ou changer sa signature. Chaque variante de `LoadError` est donc
traduite explicitement.

**Un cycle répété, pas un cycle unique.** Le premier chargement dans une
arène retient légitimement quelques cadres : une table de pages créée pour
un créneau reste en place tant qu'un créneau voisin la partage. Exiger
qu'un premier cycle ne coûte rien serait faux ; exiger qu'un **second**
cycle identique ne coûte rien est exactement la frontière entre
infrastructure partagée et fuite. Mesuré : 7 cadres au premier passage,
zéro au second.

**Deux origines, une implémentation.** Faire tester l'API depuis le noyau
n'est légitime que si le chemin est le même : même table, mêmes droits,
mêmes modes, la provenance des pointeurs seule différant. Cette différence
est elle-même testée, dans les deux sens.

**Un autotest d'API doit s'exécuter dans une tâche.** Lancé depuis
`kernel_main`, `current_pid` vaut zéro, aucune boîte aux lettres n'existe
et aucun compte n'est attaché : la moitié des domaines n'aurait rien à
décrire. Le symptôme — `NOT_FOUND` sur son propre PID — désigne le
contexte d'exécution, pas l'appel.

**Le mode `Async` est un contrat vérifié, pas une étiquette.**
`Call::require_async` refuse un blocage demandé depuis un appel
synchrone. Sans cela, la colonne « Mode » de la spec serait de la
documentation, et un domaine pourrait endormir une tâche que son auteur
croyait vivante.

---

## Couche OS (phase 6) — arbitrages et pièges

**« Pilote en espace utilisateur » se décompose en deux choses, et une
seule était atteignable.** Le SDK qui produirait un binaire de pilote
autonome est la phase 9.1 ; rien avant elle ne compile un `.dsa` séparé.
Ce qui l'était : la **discipline d'interface** — une tâche ordonnancée,
des IRQ reçues par un démultiplexeur, un service exposé par canal nommé,
un sommeil par appel système. Aucune de ces quatre interfaces ne changera
au passage en ring 3. Les écrire maintenant, c'est écrire la partie qui
survivra ; les repousser aurait laissé la logique matérielle s'installer
dans des handlers d'interruption, d'où elle ne se déloge plus.
**Ne jamais présenter les pilotes comme isolés du noyau** tant qu'ils
tournent en ring 0.

**Un compteur d'IRQ, jamais un drapeau.** Un booléen perd les
interruptions qui arrivent pendant que la tâche traite la précédente :
deux IRQ rapprochées deviennent une seule passe de traitement, et le
périphérique reste avec une trame non lue. Un compteur monotone, comparé
à la dernière valeur vue par la tâche, dit *combien* d'événements se sont
produits — une seule passe peut tous les vider.

**Une ligne PCI ne se route pas comme une ligne ISA.** Elle est à
déclenchement **par niveau** et **active à l'état bas** : le périphérique
maintient la ligne assertée tant que son registre de cause n'est pas lu.
Le relais générique du noyau ne lit aucun registre de périphérique — c'est
précisément ce qui lui permet d'ignorer ce qu'il relaie — et laisserait
donc la ligne assertée : une tempête d'interruptions qui fige le cœur
avant même que la tâche pilote ne soit élue. D'où la **scrutation** pour
la carte réseau et l'audio. Le remède est de masquer la ligne dans l'I/O
APIC depuis le relais et de la rouvrir depuis la tâche une fois la cause
lue ; tant qu'il n'est pas écrit, **ne pas armer d'interruption sur un
périphérique PCI** — ne pas en demander est le seul moyen sûr de ne pas
en subir.

**Le clavier et la souris partagent le port 0x60.** Le bit 5 du registre
d'état dit lequel a produit l'octet. Deux pilotes qui liraient 0x60 chacun
de leur côté se voleraient des octets, et comme un paquet souris ne porte
aucune marque de début fiable, le flux se désynchroniserait
définitivement — la souris partirait en diagonale pour toujours. Un seul
lecteur, sous verrou, qui range chaque octet dans la bonne file.

**Le déplacement d'une souris tient sur neuf bits, pas huit.** Le
neuvième vit dans le premier octet du paquet, un bit par axe. Reconstituer
la valeur par extension de signe sur l'octet seul est faux pour tout
déplacement de plus de 127 unités, et transforme un −1 en +255 : la souris
ne sait plus aller que vers la droite et le bas.

**La tâche pilote réseau ne doit jamais dormir dans son propre chemin de
réception.** Émettre un acquittement TCP peut demander une résolution ARP,
laquelle attend une réponse — que seule la tâche pilote peut recevoir, et
elle est occupée à attendre. Le blocage est borné, donc ce n'est pas un
interblocage, mais la réception s'arrête pendant toute la résolution et
l'attente est perdue d'avance. D'où `ip::send_cached`, qui n'utilise que
le cache et abandonne sinon : l'autre extrémité retransmet, c'est le
contrat de TCP. **Toute réponse émise depuis le traitement d'un paquet
reçu passe par ce chemin.**

**La somme de contrôle d'Internet, sur une longueur impaire.** Le dernier
octet se complète par un zéro **à droite**. Le compléter à gauche donne
une somme fausse d'un facteur 256 — et ce cas n'apparaît jamais sur une
longueur paire, c'est-à-dire pendant tous les tests faciles. Le vecteur de
la RFC 1071 (`00 01 f2 03 f4 f5 f6 f7` → `0x220D`) est rejoué à chaque
démarrage : une implémentation fausse produit des sommes stables et
d'apparence normale, rien ne la trahit sans référence publiée.

**Une somme UDP calculée qui vaut zéro s'émet comme 0xFFFF.** Les deux
valent la même chose en complément à un, mais le zéro littéral signifie
« pas de somme » en IPv4 : l'émettre désactiverait le contrôle exactement
dans le cas où le calcul avait abouti à cette valeur.

**Les numéros de séquence TCP se comparent en signé.** `a < b` est faux
dès que le compteur repasse par zéro, ce qui arrive après quatre
gibioctets échangés : une connexion de longue durée verrait soudain tous
ses acquittements refusés. La comparaison correcte est
`(a.wrapping_sub(b) as i32) > 0`. Impossible à découvrir autrement qu'en
le testant.

**L'en-tête IPv4 n'a pas une longueur fixe.** Le champ IHL la donne en
mots de quatre octets. Prendre vingt pour acquis fait lire la charge utile
décalée — les options IP interprétées comme le début de l'en-tête TCP.

**Aucun flottant dans le chemin audio.** La cible est en soft-float et SSE
n'est armé ni dans CR0 ni dans CR4 : un seul `f32` ferait échouer
l'édition de liens ou produirait une exception à la première note. Volume
en 256e, accumulation sur 32 bits, saturation explicite — un débordement
silencieux transforme un maximum positif en maximum négatif, soit un
claquement à chaque crête.

**Le mélangeur audio a un seul consommateur à la fois.** Deux appelants
qui mélangeraient tour à tour se voleraient des échantillons : le premier
viderait les files que le second attend. Le symptôme serait intermittent
et dépendrait de la présence d'une carte son — le pire des deux mondes.
D'où `audio::reserve` / `release`.

**Les volumes AC'97 sont des atténuations.** Zéro est le maximum, et le
bit 15 est la coupure. Écrire `0x8000` en croyant régler le volume au
maximum coupe la sortie : le pilote a l'air correct et ne fait aucun
bruit. C'est l'erreur classique de ce contrôleur.

**Une longueur de descripteur AC'97 est en échantillons, pas en octets.**
Confondre les deux fait jouer deux fois trop vite ou deux fois trop
lentement — un symptôme caractéristique qui ne ressemble pas à une panne.

**`ipc::send` est le point de passage unique du §12.1.** Tout message
nominatif y transite, donc c'est le seul endroit où la règle s'applique
sans trou. La seule dérogation est `send_unchecked`, réservée à la
*demande de connexion* d'un port interne : sans elle, aucune connexion
inter-groupes ne pourrait jamais être négociée. Elle ne transporte que la
demande — les messages d'un lien accepté repassent par `send`, où le lien
lui-même fait office d'autorisation.

**Un port interne n'est pas un port TCP.** Le §12.2 veut que deux
applications puissent porter le même numéro sans conflit : un port n'est
donc pas une clé globale, il n'a de sens que couplé à l'application qui le
détient. Une demande désigne *une application par son identifiant*, jamais
un numéro flottant dans un espace commun.

**Une invite d'autorisation sans réponse refuse.** Accorder par silence
ferait de toute machine sans surveillance une machine permissive, et un
programme n'aurait qu'à demander au bon moment. Le §12.5 décrit une
interface, pas une politique ; le modèle hybride du §12.3, où le non
déclaré est refusé, ne laisse pas d'autre lecture cohérente.

**Une révocation n'est pas un refus mémorisé.** Elle doit rendre le
système à l'état « jamais demandé », où l'invite réapparaîtra. Enregistrer
`deny` à la place rendrait la ressource définitivement inaccessible sans
que l'utilisateur puisse revenir dessus.

**`init.dsa` n'adopte que les orphelines.** Récupérer une tâche dont un
père attend le code de sortie le lui volerait : son `SYS_PROC_WAIT`
rendrait « tâche inconnue » là où il attendait un code. Le contrôle porte
sur deux conditions — personne ne l'attend, et son père n'existe plus.

**Un autotest de couche OS doit attendre que la couche existe.** Il
s'exécute sur le même cœur que les services qu'il vérifie, et rien ne
garantit qu'il soit élu après eux : sans attente, il constaterait l'ordre
de création des tâches et rien d'autre.

**Un autotest ne compare jamais à un instantané pris juste après.** L'état
réseau change dans une autre tâche — DHCP tourne en parallèle — et
comparer le résultat d'un appel à une lecture faite une instruction plus
tard est une course. Ce qui se vérifie, c'est une **propriété vraie à tout
instant** : « une pile déclarée opérationnelle porte une adresse ».

**Une attente bornée se paie interruptions masquées.** Les verrous du
projet les masquent : une borne de scrutation d'un million de tours sur du
MMIO ou un port d'entrée-sortie représente des centaines de millisecondes
sans horloge. Les bornes du 8042 et de l'anneau d'émission e1000 sont
dimensionnées sur le temps de réponse réel du matériel, pas sur « un grand
nombre ».

---

## Sortie graphique (phase 7.1) — arbitrages et pièges

### Le tag Multiboot2 est facultatif, et c'est délibéré

Le bit 0 des drapeaux du tag 5 vaut « facultatif ». Le mettre à zéro ferait
**échouer le démarrage** sur un firmware incapable de programmer le mode
demandé. Un noyau qui ne démarre pas parce qu'il n'a pas pu obtenir la
résolution qu'il voulait est un mauvais échange : le writer VGA existe, il
fonctionne, et une machine sans mode graphique reste une machine utilisable.
Vérifié avec `-vga none`, qui retombe proprement en texte 80×25.

Corollaire : **les dimensions du tag sont un souhait, pas un contrat.** GRUB
rend ce qu'il sait faire, dans le tag 8. Rien dans le noyau ne doit supposer
1024×768 — les positions et largeurs des composantes de couleur sont lues,
jamais devinées. C'est ce qui fait passer `-vga cirrus`, qui donne du 24 bits,
sur le même code que `-vga std`, qui donne du 32.

### Trois types de cache, un seul acceptable

La règle générale du projet — « le MMIO porte `NO_CACHE` » — ne s'applique
**pas** à un framebuffer. Un banc de registres a des effets de bord par
écriture ; un framebuffer n'en a aucun, seule compte l'image finale.

| Type | Ce que ça donne |
|---|---|
| UC | un aller-retour bus par pixel : trois millions pour une image, inutilisable |
| WB | correct, mais l'image sort du cache au gré des évictions |
| WC | rafales d'une ligne de cache, jamais relu — ce que le matériel prévoit pour ce cas |

D'où la reprogrammation du PAT. Le rituel complet du manuel Intel (CR0.CD,
`wbinvd`, invalidation du TLB, restauration) sert à **redéfinir une entrée déjà
utilisée** par des pages vivantes. Ce n'est pas le cas ici : les entrées 0 à 3
gardent leur sens et l'entrée 4 n'était atteignable par aucune page, faute de
bit PAT armé nulle part. `wbinvd` puis rechargement de CR3 suffisent.

### Le PAT est par processeur, les tables de pages sont partagées

Deux cœurs qui lisent la même page physique avec des types de cache différents
est un cas que l'architecture déclare **indéfini**. `smp::ap_entry` programme
donc le PAT en toute première instruction, avant même l'initialisation du
LAPIC — c'est-à-dire avant le premier accès mémoire que le cœur fait de son
propre chef.

### Pages de 4 Kio et non de 2 Mio pour le framebuffer

Rien ne garantit que l'adresse physique rendue par le firmware soit alignée sur
deux mébioctets, et une page géante mal alignée ne se mappe pas du tout.
Sept-cent-soixante-huit entrées pour un mode 1024×768×32, posées une fois au
démarrage, ne pèsent rien face au risque de tomber sur un firmware qui aligne
sur 64 Kio.

### Allumer l'écran avant l'allocateur

Le tampon arrière réclame des cadres, le mapping write-combining réclame des
tables de pages : ni l'un ni l'autre n'existe quand le noyau affiche sa
bannière. Attendre l'allocateur ferait disparaître de l'écran tout ce qui
précède — y compris l'état de l'allocateur lui-même, c'est-à-dire précisément
ce qu'on veut lire quand il ne démarre pas.

D'où deux étapes. La première ne demande **rien à personne** : elle passe par
la fenêtre physique de `boot.asm`. C'est ce qui la rend appelable en troisième
instruction de `kernel_main`.

### `present` est le coût, pas le dessin

Mesuré en neutralisant chaque étage à tour de rôle, sous QEMU en construction
debug :

| Étage neutralisé | Tours de la boucle de démonstration |
|---|---|
| référence | 15 850 |
| sans `present` | 30 550 |
| sans défilement | 14 600 |
| sans rendu de glyphes | 17 150 |

Le `present` pèse **2× à lui seul** ; le défilement et les glyphes moins de
10 % chacun. Optimiser le rendu de glyphes ou le remplissage de rectangles ne
change donc presque rien — la première tentative, une réplication de ligne dans
`fill_rect`, n'a rapporté que 2 %.

La conclusion n'est pas qu'il faut battre ce coût ici. Trois mébioctets par
ligne défilée font 45 Mo/s au rythme observé : négligeable sur du vrai
matériel, cher sous émulation où chaque écriture en VRAM déclenche un suivi de
pages sales. **C'est un artefact d'émulateur, et l'optimiser reviendrait à
optimiser QEMU.** La présentation par rectangles sales du compositeur (7.2)
traitera le problème là où il se pose vraiment, pour du contenu qui ne fait pas
défiler l'écran entier à chaque ligne.

### Un seul verrou pour la surface et la console

Deux verrous imbriqués poseraient une question d'ordre à chaque appel, pour
aucun parallélisme réel : il n'y a qu'un écran. `with_surface` prend et relâche
le verrou autour d'une fermeture, ce qui rend structurellement impossible de le
garder entre deux appels — et donc de commuter de tâche en le tenant.

---

## Système graphique (phase 7.2 → 7.7) — arbitrages et pièges

### Le protocole graphique n'est pas une API système

Le §8.1 est explicite : les applications « envoient des instructions IPC ». Un
domaine `SYS_GUI_*` aurait été plus simple à écrire — pas de corrélation de
réponses, pas de file locale, pas de contre-pression — et faux vis-à-vis de la
spec. Le compositeur est une application `.ss`, pas un sous-système du noyau ;
le mettre derrière un appel système aurait fait du noyau un serveur d'affichage.

### Une surface garde ses instructions, pas ses pixels

La solution évidente donne à chaque surface son tampon. Elle coûte 3 Mio par
fenêtre plein écran, **et elle est impossible ici** : il n'y a pas de mémoire
partagée pour que le client y écrive, donc chaque pixel devrait transiter par
un message plafonné à 256 octets.

Contrepartie assumée : le coût de rendu est proportionnel au nombre de
commandes, pas à la surface. D'où le plafond de 256 commandes par surface — une
fenêtre ne doit pas pouvoir rendre l'écran inutilisable en émettant sans fin, et
le refus est compté pour que la cause soit visible plutôt que devinée.

### La double liste d'affichage est le double tampon, un cran plus haut

Sans `pending` / `committed`, le rendu pourrait tomber au milieu d'une série de
commandes et afficher une fenêtre à moitié dessinée. C'est exactement le défaut
que le double tampon de la 7.1 existe pour éviter, transposé au niveau des
instructions.

### Le compositeur traite ce que la pause ne doit pas emporter

Le déplacement par la barre de titre, le passage au premier plan au clic et le
focus sont dans `compositor::route_pointer`, **pas** dans `winmanager.mod`. Ce
n'est pas un empiétement : le §8.3 dit qu'un module se met en pause sans casser
l'OS, et mettre le gestionnaire de fenêtres en pause rendrait le système
inutilisable si ces règles partaient avec lui.

Ce qui reste au module est exactement ce qui peut disparaître sans dommage : les
raccourcis de disposition, le cyclage au clavier, la fermeture au clavier.

Même raisonnement pour `notifs.mod` : la politique du §9 est côté compositeur,
le module ne fait que peindre. Un remplaçant peut changer l'apparence des
toasts ; il ne doit pas pouvoir laisser passer une PRIORITAIRE émise par un
`.gs`.

### Le curseur ne doit jamais attraper de clic

`Stack::hit` exclut la couche `Overlay`. Sans cela, le curseur — toujours dessiné
à l'endroit du pointeur, donc toujours sous lui-même — intercepterait chacun de
ses propres clics. Le symptôme serait « rien ne réagit », et la cause serait la
dernière qu'on irait chercher.

### `Any([...])` est le cas général, pas un cas particulier

`Dismissal` est un **vecteur** de conditions ; « une seule condition » est le
vecteur à un élément. Coder les deux séparément aurait donné deux chemins à
maintenir pour une seule règle : la première condition remplie l'emporte.

Point que la spec laisse ouvert : un vecteur vide. Choix retenu — elle ne
disparaît jamais d'elle-même, comme PRIORITAIRE. Ne rien déclarer, c'est ne
rien demander.

### La contre-pression IPC n'est pas une optimisation

Une boîte aux lettres plafonne à 64 messages. Le fond d'écran émet 36 commandes
par repeint, la barre des tâches une vingtaine. Deux modules qui se redessinent
en même temps saturent la file — et le symptôme observé a été **l'écran de
connexion qui n'obtient pas sa surface**, à l'autre bout du système.

Laisser tomber le message aurait été le pire choix : une commande perdue au
milieu d'une liste d'affichage ne se voit pas comme une erreur, elle se voit
comme une fenêtre mal dessinée. Le client cède la main et réessaie. Sans risque
de blocage circulaire, parce que le compositeur ne se bloque jamais sur un
client — c'est cette asymétrie qui rend l'attente sûre.

### Deux écrivains sur un framebuffer ne se partagent pas l'écran

Le compositeur et la console du noyau écrivaient tous deux dans le tampon
arrière. Le résultat n'est pas un écran partagé, c'est du scintillement et des
zones à moitié repeintes. `graphics::release_console` fait taire la console dès
que le compositeur démarre ; `kprintln!` continue sur la série, qui reçoit tout.

Exception unique : le gestionnaire de panique appelle `reclaim_console`. Un
noyau qui s'arrête doit pouvoir le dire à l'écran, et à ce stade il n'y a plus
de compositeur à ménager.

### Un mot de passe haché par SHA-256 n'est pas un mot de passe protégé

Le sel empêche une table précalculée et fait que deux comptes de même mot de
passe n'ont pas la même empreinte. Il n'empêche **rien** d'autre : SHA-256 est
conçu pour être rapide, et une attaque hors ligne sur le fichier essaierait des
millions de candidats par seconde. Il faut Argon2, scrypt ou à défaut PBKDF2 à
fort compte d'itérations.

Le sel lui-même n'est pas imprévisible : il vient du compteur de tics et de
l'horloge, faute de source cryptographique dans le noyau. **Cela doit rester
écrit tant que ce ne sera pas corrigé.**

### Les compteurs d'échec ne se remettent pas à zéro à l'expiration

Sinon attendre la fin d'une temporisation suffirait à repartir de zéro, et
l'attaquant y gagnerait un essai gratuit toutes les trente secondes. Ils ne
retombent qu'à une connexion réussie.

Le verrouillage à dix échecs est **persisté**, les temporisations non : une
temporisation perdue au redémarrage coûte trente secondes à l'attaquant, un
verrouillage perdu lui rendrait le compte.

### 33. Une couche exclue du ciblage a quand même besoin des événements

`Stack::hit` ignore la couche `Overlay` — le curseur est toujours dessiné sous
le pointeur, donc toujours sous lui-même, et il intercepterait chacun de ses
propres clics. Correct. Mais `route_pointer` ne servait **que** la surface
rendue par `hit` : le module du curseur ne recevait donc jamais rien, et la
souris bougeait dans l'état du compositeur en restant figée à l'écran.

Exclure une couche du *ciblage* et l'exclure de la *livraison* sont deux
décisions distinctes. Le routage a maintenant deux destinations : la
surimpression toujours, la surface désignée en plus.

### 34. Un commentaire qui promet ce que le code ne fait pas

`route_key` portait la phrase « Les raccourcis globaux du §16.4 sont
interceptés avant » — et n'interceptait rien. Les touches partaient à la seule
surface focalisée, si bien que `launcher.mod` et `winmanager.mod` ne recevaient
jamais leurs raccourcis : une seule surface a le focus, et ce n'était
pratiquement jamais la leur.

Deuxième moitié du même défaut : `KeyEvent::MOD_SUPER` était documenté comme
« ajouté par le compositeur », et personne ne l'ajoutait. Super n'est pas un
modificateur du jeu de scancodes 1, c'est une touche étendue ordinaire ; son
état se tient donc dans le compositeur, qui l'injecte dans ce qu'il transmet.

**Une promesse écrite que rien ne vérifie est pire qu'une fonctionnalité
absente** : elle se lit comme faite. D'où le test qui transcrit le tableau
§16.4 ligne à ligne.

### 35. Le rectangle endommagé doit borner la peinture, pas seulement la publication

C'est le défaut le plus coûteux de la phase 7, et le plus instructif.

Le rendu remplissait bien la zone endommagée avec le fond, puis rejouait chaque
surface qui la touchait — mais **avec le découpe de l'écran entier**. Le fond
d'écran, qui touche toujours la zone, repeignait donc les 3 Mio du tampon
arrière à chaque tic d'horloge de la barre des tâches, par-dessus les surfaces
plus hautes que le rendu venait d'ignorer faute d'intersection.

Le tampon arrière se désynchronisait du tampon avant. Symptôme observé : le
curseur apparaissait **amputé de quatre lignes** après un déplacement, et un
repeint complet ne le corrigeait pas — parce que la trame suivante l'effaçait
de nouveau. Rien dans le journal ne le disait.

Le remède tient en une ligne : toute la peinture d'une image a lieu **dans** la
zone, et le découpe de `Painter` se compose, donc une surface ne peut pas
peindre au-delà de ce qui sera publié. Bénéfice secondaire non négligeable : le
fond d'écran ne se repeint plus intégralement pour une bande de 28 pixels.

La leçon générale : dans un rendu par rectangles sales, **la zone est une
contrainte sur l'écriture, pas seulement sur la publication.** Publier moins
qu'on n'écrit laisse les deux tampons diverger, et la divergence ne se voit que
plusieurs trames plus tard, ailleurs.

### 36. Un autotest qui part trop tôt ne teste rien

La première version de `damage_containment` s'appuyait sur le fond d'écran réel
pour fournir la surface basse. Elle passait **avec le bug réintroduit** : à
l'instant où l'autotest s'exécute, aucun module n'a encore publié sa liste
d'affichage, et une liste vide ne peint rien.

Deux corrections, et les deux comptent :

* l'attente porte désormais sur des **listes d'affichage non vides**, pas sur
  l'existence des surfaces — un module crée la sienne bien avant d'y publier ;
* le test fabrique ses deux surfaces témoins plutôt que d'emprunter celles des
  modules, dont l'état dépend de l'ordonnancement.

Corollaire découvert au passage : les témoins étaient d'abord placés dans
`Notification`, sous le splash qui occupe `Modal` en plein écran. Le test ne
mesurait alors que la couleur du splash. **Vérifier qu'un test échoue quand on
réintroduit le défaut est le seul moyen de savoir qu'il teste quelque chose.**

### 37. Un focus qui ne commande rien est le bouton F1 d'avant la 10.1

Ajouter le clic à `login.ss` demandait une notion de focus : sans elle, un clic
dans le champ de mot de passe n'aurait pu que changer la couleur d'un cadre. Or
avant l'étape 10.1, le bouton Accessibilité basculait un drapeau et peignait une
autre couleur, sans rien allumer — et **un bouton qui annonce un état que rien
ne commande se lit comme fait**. C'est la même faute que le commentaire de
`route_key` promettant d'intercepter les raccourcis sans les intercepter
(entrée 34).

Donc `Focus` commande le clavier pour de vrai : champ défocalisé, les
caractères imprimables n'entrent pas dans le mot de passe, et Entrée ramène le
focus au lieu de soumettre — ce qui est l'ordre « sélection puis saisie » du
§2.4. Trois conséquences qu'il a fallu décider explicitement :

* **l'état initial reste `Password`**, pour que le chemin clavier documenté
  (`admin` présélectionné, on tape, Entrée) ne change pas d'un caractère ;
* **Tab ramène aussi le focus au champ**, sinon un clic à côté du panneau
  laisserait un utilisateur au clavier seul sans aucune touche pour revenir à
  la saisie — un pointeur qui dégrade le clavier ;
* **le mot de passe survit au défocus.** Perdre une saisie parce que le
  pointeur a glissé à côté serait une punition, pas un comportement.

Et une frappe imprimable sur un champ défocalisé écrit une ligne de conduite à
l'écran plutôt que d'être avalée : taper un mot de passe entier dans le vide
sans le moindre signe est le pire des diagnostics.

### 38. Deux endroits qui calculent le même rectangle, c'est le défaut « 14 par 20 »

`login::draw` calculait ses rectangles au fil de la descente du panneau. Un test
de clic écrit à côté aurait recalculé les mêmes nombres une seconde fois — et
c'est exactement la forme du rectangle endommagé du curseur, écrit « 14 par 20 »
pour une surface de 16 par 16, dont les trois colonnes de droite n'étaient
jamais salies (entrée 33 et suivantes). Le symptôme ici aurait été « le clic ne
sélectionne pas le compte qui est sous le pointeur », et rien n'aurait désigné
la cause.

D'où `login::Layout` : la géométrie est calculée une fois, le dessin la lit, le
test de clic lit les **mêmes** champs, et `Layout::row` rend le rectangle de
surlignage qui est aussi la zone cliquable. Le décalage vertical de trois pixels
du surlignage est dans cette fonction et nulle part ailleurs.

Corollaire mesuré au passage : la largeur du bouton Accessibilité était
dimensionnée sur le libellé **courant**, qui s'allonge quand la fonction
s'active. Au clavier cela ne se voyait pas. Au clic, le bouton rétrécit sous le
pointeur qui vient de l'enfoncer, et le deuxième clic tombe à côté. Le
rectangle est désormais dimensionné sur le plus long des deux libellés, quel
que soit l'état.

### 39. Le pointeur n'avait rien à intercepter, contrairement au clavier

`login::intercept` est appelé depuis `gui::route_input` **avant**
`compositor::route_key`, parce que l'écran de connexion est modal et qu'une
frappe qui fuirait vers une fenêtre d'arrière-plan pendant la saisie d'un mot de
passe serait précisément la fuite qu'on ne veut pas.

La symétrie était tentante et elle était fausse. La surface `login` est
`Layer::Modal` à la taille de l'écran : `Stack::hit` la désigne donc déjà pour
tout clic, et `route_pointer` poste `EV_POINTER` à sa tâche sans qu'on touche à
quoi que ce soit. Intercepter en amont aurait en plus affamé le curseur —
l'entrée 33 dit que la surimpression doit continuer à recevoir l'événement,
faute de quoi la souris bouge dans l'état du compositeur en restant figée à
l'écran.

Le clic est donc traité dans la boucle d'événements de `login.ss`, comme
n'importe quel module, et seulement sur le **front de pression** : agir sur
l'état du bouton plutôt que sur la transition repeindrait le panneau une
vingtaine de fois par millimètre de souris, pour un affichage qui ne dépend pas
de la position — la raison pour laquelle `taskbar.mod` ignore un survol.

### 40. Une flèche s'affiche en la dessinant, pas en l'écrivant

Le bouton de validation de `login.ss` porte une flèche vers la droite. Elle est
tracée au `rect` — une hampe et six segments verticaux de longueur croissante
pour la pointe — et non écrite.

Deux raisons, et la première est une règle du projet : un `"→"` est hors de la
plage 0x20–0x7E, donc refusé par `make check-ascii`, et le writer VGA le
remplacerait par un carré plein. La seconde est matérielle : la police embarquée
est un bitmap 8×16 indexé par octet ASCII, elle n'a aucun glyphe de flèche à
proposer. Un `"->"` en texte aurait été lisible, mais il se serait lu comme deux
caractères au bout d'un champ d'étoiles.

Le protocole n'a pas de primitive de triangle et n'en a pas besoin : six `rect`
coûtent moins cher que de lui en apprendre une.

### 41. Deux points d'entrée pour une action, un seul barème

La flèche appelle le même `submit()` que la touche Entrée. La tentation était de
la refuser sur un champ vide, ou de la griser — et les deux étaient des pièges :

* la griser tout en la laissant agir est le mensonge de l'entrée 37 ;
* la faire refuser là où Entrée accepte donnerait **deux règles pour un
  comportement**, et le barème anti-bruteforce du §2.4 se retrouverait appliqué
  différemment selon la main qui valide.

Donc un seul appel, un seul barème : un champ vide compte un échec, à la flèche
comme à la touche. C'est discutable comme comportement, mais c'est le
comportement qui existait déjà, et le changer serait une décision sur 7.7.2, pas
sur un bouton.

Corollaire de géométrie : la flèche a pris sa place **sur** la largeur du champ,
pas en élargissant le panneau. La largeur du panneau est ce qui le centre à
l'écran ; un panneau qui s'élargit pour un bouton déplace tout le reste.

### 42. Le survol est revenu, mais borne au franchissement

La phase 7 avait refusé l'effet de survol, et la raison était bonne : un pointeur
publie un evenement par mouvement, donc repeindre en fonction de la **position**
coute une vingtaine de messages IPC par millimètre de souris. C'est cette mesure
qui avait fait écrire « le survol seul ne redessine rien ».

L'utilisateur a demandé l'effet. Il est là, et le coût est traité au lieu d'être
ignoré : le repeint est conditionné au **changement de cible**, jamais au
mouvement. Traverser une ligne de compte coûte un repeint ; parcourir cette même
ligne de bout en bout n'en coûte aucun de plus.

Relevé, parce qu'une affirmation sur un coût qui ne se mesure pas est ce qui
avait fait refuser la fonctionnalité :
`[login] pointeur : 99 evenement(s), 17 repeint(s) de survol`, et soixante
mouvements consécutifs à l'intérieur d'une même ligne n'en coûtent qu'un. Les
deux compteurs sont dans `Session` pour que le chiffre reste citable, et non
pour le décor.

La même règle vaut pour la forme du curseur : `login.ss` n'envoie
`OP_CURSOR_SHAPE` que quand la forme change. Réémettre la même forme à chaque
mouvement aurait rendu au compositeur précisément le coût qu'on venait
d'éviter.

**La limite n'est levée que pour `login.ss`.** La barre des tâches, le lanceur
et le gestionnaire de fenêtres n'ont pas d'effet de survol. Le mécanisme est
dans le protocole, pas dans `login.rs` ; les y porter est une reprise module par
module.

### 43. Le survol et le focus ne se peignent pas de la même couleur

Deux choses différentes : le survol dit où est la **souris**, le focus dit où va
le **clavier**. Un champ survolé et non focalisé prend donc une bordure plus
claire et non la bordure d'accent — celle-là est réservée au focus, et
l'emprunter ferait dire au champ « le clavier est ici » alors qu'il n'y est pas.
C'est l'entrée 37 sous un autre angle : une couleur qui affirme un état que
rien ne commande.

Corollaire qui a demandé une fonction nouvelle : une ligne de compte a **trois**
états — ordinaire, survolée, sélectionnée — et un thème n'en déclare que deux
nuances, `surface` et `surface_alt`. Peindre une ligne survolée en `surface_alt`
la ferait lire comme sélectionnée, et le pointeur aurait l'air de choisir tout
ce qu'il traverse. D'où `framebuffer::mix`, qui interpole deux couleurs composante
par composante **en arithmétique entière** : pas un `f32` dans le noyau tant que
SSE dort dans `CR0`/`CR4`, et `(a * (255 - w) + b * w) / 255` tient dans un `u32`.

Les teintes de bouton vont vers le **blanc** et non vers une autre couleur du
thème. Un thème choisit sa palette : n'importe quelle autre couleur retenue
comme nuance de survol pourrait être déclarée égale à celle dont elle doit se
détacher. Éclaircir ce qui est là ne peut entrer en collision avec rien.

### 44. La forme du curseur voyage dans un bourrage que personne ne lisait

`cursor.mod` reçoit déjà un `EV_POINTER` à chaque mouvement — c'est ainsi qu'il
suit le pointeur. `PointerEvent::encode` y écrivait un `u16(0)` de bourrage que
`decode` ignorait. La forme y tient, et **ne coûte aucun message** : un
événement dédié en aurait ajouté un par frontière franchie.

Deux conséquences à énoncer :

* `decode` tolère l'absence de l'octet (`unwrap_or(0)`). Le rendre obligatoire
  ferait refuser toute charge utile qui s'arrête avant — un producteur non
  recompilé verrait ses événements rejetés, et le symptôme serait « la souris ne
  marche plus », pour un champ ajouté ailleurs ;
* la forme était d'abord vue au mouvement **suivant** celui qui l'avait
  changée, ce qui paraissait gratuit puisque ce mouvement est celui qui a causé
  le changement. C'était faux, et une souris continue le cachait : avec un
  pointeur qui avance par bonds — une tablette absolue, un saut, un seul rapport
  qui atterrit sur un bouton et s'arrête — **il n'y a pas de mouvement
  suivant**, et le curseur garde la flèche sur un bouton jusqu'à ce que la main
  tremble. Mesuré par injection QMP, où chaque déplacement est un événement
  unique : la forme arrivait systématiquement un geste trop tard.
  La forme est donc **poussée** depuis le gestionnaire d'`OP_CURSOR_SHAPE`, par
  la file de l'entrée 47. Le coût est un message par frontière franchie, soit
  quelques-uns par seconde ; ce qu'il fallait éviter était un message par
  *mouvement*, et cela reste évité.

Et une garantie qui n'est pas facultative : quitter une surface **rend la
flèche**. `route_pointer` remet la forme par défaut dès que la surface survolée
change. Sans cela, un client qui a demandé un curseur de texte puis a cessé de
répondre laisserait ce curseur sur tout le bureau, et rien ne le rendrait — le
§8.3 veut qu'un module en pause ne casse pas l'OS.

### 45. Une forme de curseur a un point chaud, donc la zone sale ne se déduit plus du pointeur

`route_pointer` calculait la zone à repeindre depuis la position du **pointeur**,
en supposant que le curseur y était. C'était vrai tant que toutes les formes
avaient leur point significatif au coin supérieur gauche, ce qui est le cas de
la flèche et d'aucune autre : la barre d'une saisie de texte désigne son
**milieu**.

`cursor.mod` décale donc sa surface de son point chaud, et une zone sale
calculée depuis le pointeur manquerait les lignes du dessus de ce décalage. C'est
l'entrée 35 pour la troisième fois, et son symptôme est une traînée derrière le
curseur.

Le compositeur lit maintenant le rectangle **réel** de la surimpression, position
comprise, et non plus seulement sa taille. C'est aussi plus robuste quand le
module prend du retard : on salit là où le curseur **est**, pas là où il devrait
être.

Point chaud de la main : le bout du doigt est dessiné en haut à gauche exprès.
Une main centrée dans sa boîte aurait mis le point désigné sept pixels sous le
doigt, ce qui se lit comme un pointeur qui rate ce qu'il survole.

### 46. Trois formes de curseur en bitmap coûtent moins que trois en commandes

La flèche était bâtie de quinze `rect` et `line`. Trois formes de cette façon en
auraient fait quarante-cinq, et une liste d'affichage reconstruite à chaque
changement. `DRAW_ICON` transporte un monochrome 16×16 en **un seul message** —
trente-deux octets de bits plus une couleur — ce qui est exactement la taille
d'un curseur. Une forme coûte donc deux messages : le contour, puis le corps.

Le contour est un second masque et non une astuce de couleur, parce qu'un
curseur d'une seule teinte disparaît dès qu'il passe sur un fond de la même
teinte. L'ordre compte : le corps **après** le contour, sinon le contour
mangerait la figure par ses bords, ce qui sur douze pixels de large en fait un
tiers.

Défaut découvert en transcrivant : l'ancienne flèche peignait son contour
**par-dessus** le corps avec trois `line` dont les extrémités le dépassaient,
laissant quelques pixels de couleur de fond à l'extérieur de la figure.
Invisible sur un fond plat, faiblement visible sur le dégradé de
`wallpaper.mod`. Le masque n'a pas ce défaut ; c'est la seule différence
d'aspect de la flèche.

### 47. Une requête qui doit atteindre un tiers ne peut pas l'atteindre depuis `handle`

`compositor::handle` tourne **sous le verrou du compositeur** et ne rend qu'une
réponse, pas une liste de destinataires. Émettre depuis son intérieur mettrait un
envoi IPC dans la section critique — ce que DECISIONS.md interdit partout pour la
même raison : une boîte pleine bloquerait alors le rendu.

D'où `Compositor::pending`, une file que `drain_mailbox` vide **hors du verrou**,
exactement là où il envoyait déjà la réponse. Deux usages, et il vaut la peine de
dire pourquoi l'un pousse et l'autre non :

* **la bascule du menu pousse.** Aucun événement ne vient derrière : la main qui
  a cliqué peut ensuite rester parfaitement immobile, et le menu resterait fermé
  jusqu'à ce que la souris bouge par hasard ;
* **la forme du curseur pousse aussi**, mais on a cru longtemps qu'elle n'en
  avait pas besoin. Voir l'entrée 44 : c'est faux dès que le pointeur avance par
  bonds.

### 48. La barre des tâches ne parle pas au lanceur, elle parle au compositeur

Le bouton « Nova OS » est sur la surface de `taskbar.mod`, le menu est celle de
`launcher.mod`, et le §8.2 en fait deux modules **indépendamment
remplaçables**. Un envoi direct supposerait que la barre connaisse le PID du
lanceur : un protocole privé entre deux composants dont chacun doit pouvoir être
échangé sans l'autre.

Le compositeur est donc le courtier : `OP_MENU_TOGGLE` monte, `EV_MENU_TOGGLE`
redescend **en diffusion** vers les modules du shell, exactement comme un
raccourci global du §16.4. Trois conséquences voulues :

* la touche Super passe maintenant par le **même** chemin. Elle basculait le
  menu sur place ; deux chemins auraient pu diverger, et le bouton de la barre
  aurait affiché le mauvais état pour l'un des deux ;
* la barre tient **sa propre** copie de « le menu est ouvert », alimentée par la
  diffusion, et ne lit pas `launcher::is_open()`. Lire le statique de l'autre
  module marcherait — un seul espace d'adressage — et déferait silencieusement la
  raison d'être du courtage : une barre de remplacement vivant hors du noyau
  n'aurait aucun statique à lire ;
* la barre ne bascule **pas** son propre état au clic. Seule la diffusion le
  change. Le faire localement en plus compterait deux fois une bascule que le
  compositeur aurait pu refuser, et le bouton affirmerait un état que le menu
  n'a pas.

**Promesse non tenue corrigée au passage** : le commentaire de `launcher.mod`
annonçait depuis le premier jour qu'il « s'ouvre sur un clic sur le bouton de la
barre des tâches ou sur la touche Super ». Le clic n'avait jamais existé. C'est
l'entrée 34, pour la troisième fois.

**Promesse non tenue laissée en place, mais écrite** : le même commentaire
affirmait que « le lancement se fait avec `SYS_PROC_SPAWN` ». Aucune ligne du
fichier ne l'appelle. `list_applications` alimente l'affichage et rien d'autre.
La phrase est remplacée par ce que le module fait vraiment, et la conséquence est
visible à l'écran : les lignes du menu reçoivent un **surlignage** — « le
pointeur est sur cette ligne » — et gardent la **flèche**, qui dit « rien à
cliquer ici ». La main prétendrait qu'un clic agit.

### 49. Un surlignage sur un élément inerte est un mensonge, donc l'élément gagne une action

Règle appliquée élément par élément pour décider où le survol existe.

Les boutons de fenêtre de la barre des tâches étaient inertes. Deux issues : pas
de surlignage, ou une action. Une barre des tâches dont les boutons ne font rien
n'est pas une barre des tâches, donc ils **ramènent la fenêtre au premier plan**
— `OP_SURFACE_RAISE` puis `OP_SURFACE_FOCUS`, ce que `winmanager.mod` fait déjà
au clavier.

Inversement, ce qui n'a **pas** reçu de survol, et pourquoi :

* l'horloge et le compteur de notifications de la barre : aucun clic n'y répond ;
* les bandeaux de `notifs.mod` : le §9 met la politique de disparition du côté
  du compositeur, et rendre un bandeau cliquable serait une décision sur le §9,
  pas un effet de survol ;
* le dialogue de consentement du §12.5 : il ne lit aucun événement de pointeur
  du tout, il répond à `O`, `N` et espace ;
* `devstudio.ss` : éditeur de texte au clavier.

### 50. La géométrie d'un survol ne se recalcule pas à chaque mouvement

Deux fois le même piège, évité de deux façons différentes, et les deux valent
d'être notées parce qu'un pointeur publie un événement par mouvement.

`taskbar.mod` : les boutons de fenêtre dépendent de ce que répond
`window_list()`, qui est un aller-retour IPC. Un test de survol qui l'appellerait
coûterait **un aller-retour par mouvement**. Le dessin publie donc les
rectangles qu'il a tracés dans `ZONES`, et le test de survol les lit. C'est la
règle de `login::Layout` — une seule arithmétique, lue par le dessin et par le
test — atteinte par l'autre bout, parce qu'ici la géométrie dépend d'un contenu
que le module ne possède pas. Le prix est que les zones sont celles du **dernier**
dessin : une fenêtre ouverte entre deux tics n'est pas cliquable dans la barre
pendant au plus une seconde.

`launcher.mod` : la première version de `row_at` appelait `list_applications()`,
qui liste **trois répertoires du système de fichiers**. Trois lectures de
répertoire par mouvement de souris, soit bien pire que l'aller-retour IPC qu'on
venait d'éviter à côté. Le nombre de lignes est désormais publié par le dessin
dans `ROWS`.

### 51. `Hover::set` doit répondre « a changé », pas « est mis »

Tout l'argument de coût repose sur cette valeur de retour : repeindre sur la
**position** du pointeur coûte une vingtaine de messages IPC par millimètre de
souris, repeindre sur la **frontière** en coûte un par élément franchi.

Un `set` qui répondrait `true` à chaque appel rétablirait silencieusement le coût
qui avait fait refuser le survol pendant deux phases, et **rien à l'écran ne
serait différent**. C'est exactement le genre de régression qu'aucun œil ne
rattrape, d'où le test qui vérifie qu'entrer est un changement, y rester non, en
sortir un seul.

Corollaire : `Hover::NONE` ne doit jamais passer pour un élément. Une barre sans
zone sous le pointeur surlignerait sinon la zone de rang -1.

### 52. Un module doit savoir où est sa surface pour lire un clic

`Canvas` donnait la taille de la surface et celle de l'écran, jamais l'origine.
Or un événement de pointeur porte des coordonnées **écran** et tout ce qu'un
module dessine est local à sa surface. Un module qui testerait sans soustraire
répondrait à des clics survenus ailleurs — et pour la barre des tâches, collée au
bord bas, « ailleurs » veut dire tout le bureau au-dessus d'elle.

`Canvas::x` et `Canvas::y` sont donc là, avec la réserve qui compte : c'est là où
`geometry` a placé la surface, pas nécessairement là où elle est maintenant. Un
module qui déplace la sienne, comme `cursor.mod`, ne doit pas les lire.

### 53. La forme voulue se compare à l'événement, pas à un souvenir

`want_cursor` compare la forme voulue à celle que **l'événement transporte**, et
non à la dernière forme que le module a demandée.

Un module qui se souviendrait se tromperait dès que le compositeur remet la forme
par défaut de son propre chef — ce qu'il fait chaque fois que le pointeur quitte
une surface, pour qu'aucun client ne puisse échouer son curseur sur le bureau. Le
module croirait la main encore en vigueur, resterait muet, et le pointeur
garderait la flèche sur un bouton.

La comparaison contre l'événement rend l'ensemble **auto-correcteur et sans
état** : quoi qu'il se soit passé entre-temps, la forme converge en un mouvement.

### 54. Un module traitait un événement par réveil, et cela se voyait à la main

Le clic sur « Nova OS » mettait **1336 ms** à ouvrir le menu. La cause n'était
pas dans le chemin du menu, qui est court, mais dans la boucle commune des
modules : elle prenait **un seul** événement par tour, puis dormait
`min(tick_ms, 100)`. Pour `taskbar.mod`, dont la période est d'une seconde, cela
faisait un événement par 100 ms.

Et un pointeur publie un événement par mouvement. Amener la souris sur le bouton
en empilait une dizaine ; le clic attendait derrière toute la file, à 100 ms
chacun. Une demi-seconde de mouvement coûtait une seconde de latence.

Deux corrections, et les deux comptaient :

* **vider la boite a chaque réveil**, borné par `ipc::MAX_QUEUE_LEN`, pour qu'un
  réveil puisse toujours épuiser une boite pleine et qu'un retard ne survive
  jamais a un réveil ;
* **dormir 20 ms et non 100.** La période d'un module n'est pas sa latence
  d'entrée : `taskbar.mod` rafraichit son horloge une fois par seconde et doit
  repondre a un clic bien plus vite. Cent millisecondes dépassent ce qu'une main
  lit comme immédiat.

Mesure après : **41 ms**. Un facteur trente.

Leçon générale : **une période de rafraichissement et une latence d'entrée sont
deux nombres différents**, et les confondre donne un systeme qui paraît lent
sans qu'aucun profil ne montre de calcul coûteux.

Conséquence de structure : le repeint se fait **une fois** après avoir vide la
file, pas une fois par événement, et `dispatch` est extrait pour que le chemin
bloquant et le chemin de vidage ne puissent pas diverger sur `TAG_MODULE_STOP`.
Avec un seul événement par tour il n'y avait qu'un appelant ; avec deux, celui
qui oublierait l'arrêt laisserait une surface a l'écran après la mort de son
module.

### 55. La croix de fermeture etait dessinée depuis la phase 7 et ne fermait rien

`paint_decoration` peignait une croix rouge a droite de chaque barre de titre.
`route_pointer` ne cherchait qu'un glissement de la barre. `EV_CLOSE` existait
dans le protocole, `devstudio.ss` le traitait, et **aucune ligne du systeme ne
l'émettait**. Un `grep` sur l'arbre entier le confirmait : trois occurrences, la
constante, le décodage et une liste de test.

C'est l'entrée 34 sous sa troisième forme, et la plus visible de toutes : ici ce
n'est pas un commentaire qui promet, c'est **un dessin**. Une croix peinte se lit
comme un bouton.

Trois décisions en la réparant :

* **le rectangle devient une fonction**, lue par le dessin et par le test de
  clic. Deux littéraux auraient donne une croix qui répond a côté d'elle-meme ;
* **la zone cliquable est agrandie de trois pixels sur chaque bord.** Neuf
  pixels sont en dessous de ce qu'une main atteint, et un clic raté ne tombe pas
  dans le vide : il tombe sur la barre de titre et **démarre un glissement**, si
  bien que la fenêtre saute au lieu de se fermer ;
* **la croix est testée avant le glissement.** Elle est a l'intérieur de la
  barre : tester le glissement d'abord attraperait la fenêtre a chaque fois et
  le bouton ne partirait jamais.

Et le compositeur **demandé**, il ne détruit pas. Une application avec du travail
non enregistré doit pouvoir dire quelque chose d'abord ; `winmanager.mod` prend
la meme précaution avec Alt+F4.

### 56. La file de poussee se vide pour tous les genres de message, pas seulement les requetes

Premiere version : `drain_mailbox` vidait `Compositor::pending` dans la branche
`Request`. La bascule du menu vient bien d'une requete, donc cela marchait.

La croix de fermeture, elle, vient d'un **événement de pointeur**. Son `EV_CLOSE`
serait resté dans la file jusqu'à ce qu'un module quelconque dessine -- ce qui
arrive constamment, donc la fermeture aurait fonctionné **par accident**, avec un
retard variable dont la cause aurait ete à trois fonctions de là.

Le vidage est donc sorti du `match`. Règle a retenir : une file de sortie se vide
là où la boucle se termine, pas dans la branche qui l'a remplie la première fois.

### 57. Deux tampons de texte coexistent, et c'est le choix le moins mauvais

`sdk::devstudio` porte son propre tampon depuis la phase 9. `src/textbuf.rs` en
est un second, et la duplication est délibérée.

Ce qui l'a décidée : **l'editeur de `devstudio` n'a aucun test**. Sa couverture
se limite à son en-tete de binaire, a l'obtention d'une surface et a son arrêt
propre ; pas une insertion, pas un déplacement. Le faire reposer sur `textbuf`
serait un remaniement que rien ne vérifierait, sur le composant le plus long de
la phase 9.

Les deux issues étaient donc :

* dupliquer la logique -- deux implementations des mêmes règles, dont une
  éprouvée ;
* remanier sans filet -- une seule implementation, et aucun moyen de savoir si
  les deux comportements concordaient encore.

La première laisse une dette **nommée**, la seconde laisse un risque muet. La
migration resté a faire, et elle commence par écrire les tests qui prouvent que
les deux tampons s'accordent : c'est une étape a elle seule, pas un détour.

### 58. Un caret place au clic s'arrondit, il ne se tronque pas

Convertir un pixel en colonne par `x / largeur_de_glyphe` met le caret **avant**
le caractère cliqué. Cliquer la moitié droite d'un `m` veut dire « après le
`m` », et la troncature répond « avant ». L'écart est d'un caractère partout, et
il se lit comme un clic qui raté.

D'où `(offset + largeur / 2) / largeur` : l'arrondi au plus proche **intervalle
entre deux caractères**, qui est ce qu'un caret désigne réellement. Un caret ne
se place pas sur un caractère, il se place entre deux.

Corollaire consigné dans `textbuf` : un clic et une touche ne bornent pas la
colonne de la meme façon. Une flèche verticale **garde** la colonne mémorisée,
pour qu'un caret en colonne 40 revienne en colonne 40 après avoir traversé une
ligne courte. Un clic, lui, borné immédiatement : la main qui a pointé au-delà
de la fin d'une ligne a demandé sa fin, elle n'a rien mémorisé.

### 59. Une application demandé ou est sa fenêtre, elle ne s'en souvient pas

`crate::gui::surface_origin` existe parce qu'une fenêtre est déplacée par **le
compositeur**, qui la traîne par sa barre de titre sans en avertir son
propriétaire.

Une application qui garderait son origine de création serait fausse dès le
premier glissement, et un clic destiné a une lettre de texte tomberait plusieurs
caractères plus loin -- d'autant plus loin que la fenêtre aurait ete déplacée.
Le defaut serait invisible tant que personne ne deplace la fenêtre, ce qui est
exactement la forme qu'il faut se méfier.

Meme famille que l'entrée 52 pour les modules : la position d'une surface se
demandé, elle ne se déduit pas.

### 60. Un binaire reel dont le code n'est pas exécuté doit le dire

`/apps/notepad.cus` est un vrai fichier : assemblé, en-tete du §3.3, signature
enregistrée, permissions déclarées (`FS_USER_HOME` seule, parce qu'un bloc-notes
n'a besoin de rien d'autre). Ce qui **tourne** est le code natif de
`apps::NATIVE_APPS`, pas les octets du fichier.

La raison est connue : exécuter un binaire étranger dans son propre espace
d'adressage demandé un CR3 par processus, limite ouverte par la phase 3. La table
est le meme arrangement que `gui::NATIVE_MODULES` et aura le meme sort.

Ce qu'il faut écrire, c'est **pourquoi le taire serait pire** : un `.cus` dans
`/apps` qui se lance ressemble exactement a un `.cus` dans `/apps` qui
s'exécuté. Rien a l'écran ne distingue les deux, donc rien ne rappellerait que
le chargeur resté a faire.

Corollaire de comportement : une ligne du menu dont le nom n'est pas au
catalogue **l'affiche sur sa ligne**, en rouge, au lieu de ne rien faire. Une
entrée muette sous le clic est indiscernable d'une entrée cassée.

### 30. Un test d'existence globale ne dit rien de ce qui manque

`install_default_credentials` s'arrêtait dès que le fichier des empreintes
contenait quelque chose. Il a suffi que l'autotest de la phase 7 y écrive en
premier pour qu'**aucun compte réel n'obtienne de mot de passe** — donc que
personne ne puisse ouvrir de session, sur un système où tout le reste
fonctionnait.

Le contrôle porte désormais sur chaque compte. La leçon vaut au-delà de ce cas :
une initialisation « si rien n'existe » est fausse dès qu'un second écrivain
partage la ressource.

### 31. `frame::stats` compte la machine entière

L'autotest de la phase 5 vérifiait qu'un cycle de vie complet ne fuit aucun
cadre, en comparant deux relevés de cadres libres. La vérification était juste
tant que rien d'autre n'allouait — puis la phase 7 a fait naître le compositeur
et ses sept modules précisément pendant cette fenêtre, et la mesure s'est mise à
constater l'apparition du système graphique.

La précondition est maintenant **explicite et vérifiée** : le nombre de tâches
est relevé en même temps que les cadres, et la comparaison n'a lieu que si rien
n'est né entre-temps. Quand elle ne tient pas, le dire vaut mieux que rendre un
verdict sur une mesure qui ne mesure pas ce qu'elle prétend.

### 32. La touche `m` n'existait pas sur un clavier français

La table AZERTY du pilote clavier s'arrêtait à `l` (0x26). La touche `m` d'un
clavier français est en 0x27, là où un clavier anglais a le point-virgule : elle
ne produisait aucun caractère. Personne ne l'avait vu tant que rien n'attendait
de texte — puis l'écran de connexion est arrivé, et le compte **`admin` s'est
trouvé intypable**.

Le remède n'est pas d'ajouter la touche mais de **compter les lettres** :
l'autotest vérifie que les vingt-six sont atteignables, ce qui ne dépend pas de
se souvenir de celle qui manque.

Piège dans le piège : la première version du balayage enfonçait 0x2A — la touche
majuscule — sans la relâcher, et toutes les lettres suivantes sortaient en
majuscules. Le test accusait alors la table d'un défaut qui était le sien. Un
balayage de scancodes doit émettre le relâchement **dans tous les cas**, y
compris quand l'enfoncement n'a rien produit.

---

## SPM et mises à jour (phase 8) — arbitrages et pièges

### Les scripts d'installation ne sont pas du code

Le §10.1 place « scripts pre/post installation » dans le paquet sans dire
ce qu'ils contiennent. La lecture usuelle — un binaire exécuté à
l'installation — est ici indéfendable : l'installeur tourne sous les droits
du noyau, parce qu'il écrit dans `/sys/` et `/apps/`. Faire exécuter du
code arbitraire par ce contexte offrirait à n'importe quel `.ras` non signé
le chemin le plus court vers l'anneau 0, et le §12.4 aurait été écrit pour
rien.

Retenu : six verbes déclaratifs, pas de branchement, pas de boucle. Ce
qu'un script d'installation fait réellement s'écrit avec ces six verbes.
**Décision verrouillée** : n'ajouter aucun verbe capable d'exécuter, de
charger, ou de créer un lien.

### §10.2 et §11.1 emploient les mêmes nombres pour des choses différentes

Le §10.2 numérote des comportements de notification : -1 forcée, 0
standard, 1 optionnelle, 2 silencieuse. Le §11.1 numérote des types de
mise à jour de l'OS : 0 PATCH, 1 UPDATE, 2 MAJOR, -1 CRITICAL.

Le piège est net : le code 2 vaut « aucune notification » d'un côté et
« nouvelle partition, redémarrage obligatoire » de l'autre. Les confondre
appliquerait en silence la mise à jour la plus lourde du système. Deux
types Rust distincts, une seule traduction écrite dans
`OsUpdateKind::notification_code`.

Corollaire consigné : **aucun type de mise à jour de l'OS n'est
silencieux.** Le §11.2 promet un *téléchargement* silencieux, jamais une
application silencieuse.

### Une empreinte qui se contient elle-même — deuxième occurrence

La liaison de certificat d'un `.spm` vaut `SHA-256(clé || empreinte
d'authenticité)`. L'empreinte couvre l'en-tête, l'en-tête contient la
liaison, la liaison dépend de l'empreinte : le constructeur écrivait la
liaison *après* avoir calculé l'empreinte, et le vérificateur recalculait
sur un en-tête où la liaison était désormais renseignée. Aucun paquet
ENTERPRISE n'était acceptable, et le symptôme — `BindingMismatch` —
désignait le certificat plutôt que le calcul.

C'est exactement la raison pour laquelle le champ d'empreinte est
neutralisé en phase 3 ; ce qui manquait, c'est de voir que la liaison est
**aussi** un champ dérivé. Règle générale : dans un en-tête,
`authenticity_digest` doit neutraliser *tout* champ calculé à partir
d'elle-même, et ne neutraliser aucune entrée du calcul — la clé de
l'autorité reste couverte, c'est ce qui empêche de substituer une autorité
en gardant la liaison.

### Un magasin de confiance plein échouait en silence

`DYNAMIC_CAPACITY` valait 8, dimensionné en phase 3 quand seul son autotest
y déposait. La phase 7 y met ses sept modules natifs et `binfmt::sample` un
binaire : le magasin était plein avant même d'atteindre la phase 8.
`Store::insert` rendait `false`, et **aucun des trois appelants ne lisait
la valeur de retour**.

Le symptôme trompait complètement : un paquet SYSTEM parfaitement vérifié
posait un binaire refusé au chargement, et rien ne reliait les deux
événements. Porté à 32, et le refus d'insertion est désormais journalisé —
une panne silencieuse dans un magasin de confiance est la pire espèce.

Leçon générale : une capacité fixe dimensionnée pour une phase doit être
réexaminée à chaque phase qui y écrit, et un `bool` de retour que personne
ne lit vaut mieux comme journal que comme valeur.

### `kprintln!` répercute déjà sur la console série

`kprintln!` développe en `println!` **plus** `serial_println!`. Y ajouter un
`serial_println!` pour « aussi » l'envoyer sur la série écrit la ligne deux
fois. Le doublon est passé inaperçu à la relecture et ne s'est vu que dans
la sortie.

### L'ordre des refus décide de ce qu'une installation laisse derrière elle

Forme, intégrité, signature, combinaison §3.4, avertissement §10.4,
dépendances : **tous ces refus ont lieu avant la première écriture**. C'est
l'invariant qui rend une installation refusée sans conséquence.

À partir de l'écriture du binaire, un échec laisse des fichiers — et c'est
pourquoi la liste des fichiers est tenue au fur et à mesure et non
constituée à la fin. Un script pre-installation qui échoue défait ce qu'il
a créé ; un script post-installation qui échoue ne défait rien, parce que
les fichiers sont posés et fonctionnels : le paquet est marqué « partiel »,
ce que `spm update` relit.

### Le compteur d'essais s'incrémente avant la validation, pas après

Un système qui se fige pendant la fenêtre de validation de soixante
secondes ne repasserait jamais par le code qui incrémente le compteur, et
redémarrerait indéfiniment sur la version cassée. Incrémenter d'abord,
valider ensuite : au deuxième essai infructueux, le rollback part sans
qu'on ait besoin d'atteindre quoi que ce soit.

### Un snapshot qu'on ne sait pas dater ne se purge pas

Les snapshots sont horodatés par le RTC, qui peut ne pas répondre. Zéro est
la valeur reconnaissable, et un snapshot daté à zéro est **conservé**.
Supprimer par défaut ce qu'on ne sait pas dater effacerait le seul recours
d'une machine sans pile CMOS — exactement celle où une mise à jour ratée
est le plus difficile à défaire.

### Trois vérifications distinctes sur le chemin d'un dépôt

Le sceau de l'index répond à « le dépôt a-t-il publié ceci ». L'empreinte
du fichier reçu répond à « est-ce ce que l'index annonce ». La signature du
paquet répond à « qui l'a produit ». Les trois sont nécessaires et aucune ne
remplace les autres ; les confondre laisserait croire qu'un téléchargement
réussi vaut confiance.

L'ancre d'un dépôt porte la même réserve que la chaîne ENTERPRISE de la
phase 3 : c'est un haché dont l'entrée est connue. Elle détecte une
altération, elle n'établit pas une provenance.

---

## Optimisation et bugs de fond (passe transverse)

### Un pointeur brut dans un `Vec` survivait à la libération du verrou

`switch` relevait `&raw mut sched.tasks[previous].rsp`, relâchait le verrou
de l'ordonnanceur — c'est la règle, on ne commute jamais verrou tenu — puis
appelait `switch_context` avec ce pointeur. Le pointeur désignait le
**tampon du vecteur**. Qu'un autre cœur crée une tâche pendant cette
fenêtre, que le `Vec` se réalloue, et `switch_context` écrivait le RSP
sortant dans de la mémoire libérée.

Le symptôme aurait été une tâche reprenant sur une pile fantôme, plusieurs
commutations plus tard, sans que rien ne désigne la cause. Jamais observé —
la fenêtre est étroite et les créations de tâches rares après le démarrage —
mais la règle « pas de verrou pendant `switch_context` » et « un `Vec` peut
déplacer ses éléments » sont incompatibles telles quelles.

Corrigé par `Vec<Box<Process>>` : réallouer le vecteur déplace des
pointeurs, pas les structures qu'ils désignent. Clippy signale `vec_box`
sur ce champ — c'est un faux positif ici, et la raison est écrite dans le
commentaire du champ. **Ne pas « corriger » ce lint.**

### La table des tâches ne rendait jamais un emplacement

`try_reap` libérait la pile et l'image, marquait `reaped`, et laissait
l'entrée dans le vecteur. `exists` et `task_list` filtraient déjà ces
entrées : le reste du système les considérait comme absentes, mais elles
occupaient une ligne, gardaient leur `String` de nom, et **l'ordonnanceur
les parcourait à chaque élection**. Le coût d'une commutation croissait
donc indéfiniment avec le nombre de tâches créées depuis le démarrage.

Corrigé par réemploi d'emplacement à la création plutôt que par
compactage : `current[cpu]` désigne des **positions**, et retirer une entrée
décalerait celles de tous les autres cœurs. Un emplacement réemployé garde
son index. La table est désormais bornée par le nombre de tâches
simultanément vivantes.

### Les tâches de démonstration de la phase 2 tournaient encore

`ping`/`pong` faisaient un aller-retour IPC en boucle infinie, avec un
`format!` par tour ; `calcul-a`/`calcul-b` étaient deux boucles vides ; le
moniteur scrutait `ticks()` au lieu de dormir. Cinq tâches à plein régime,
en permanence.

Le coût, mesuré : **73 % de la sortie série** (12 288 lignes de `[ping]`
sur 16 806), 200 000 messages IPC, autant d'allocations de tas — puisque
`Message::new` copie sa charge utile dans un `Vec` — et le CPU dont le
compositeur avait besoin pour suivre la souris.

C'est aussi l'explication de la dérive du tas restée ouverte à la fin de
la phase 8 : elle a disparu avec ces tâches. Le relevé du moniteur est
maintenant **strictement identique** d'un rapport à l'autre, ce que
l'invariant demandait depuis le début.

Bornées plutôt que supprimées : la démonstration garde sa valeur si elle
se termine et le dit. Un aller-retour qui fonctionne cinq cents fois
fonctionne ; s'il échoue, il échoue tôt.

### Publier des lignes entières coûtait cinquante-sept fois trop cher

`present_rows` recopie le pas complet de chaque ligne, donc toute la
largeur de l'écran. Déplacer un curseur de seize pixels salit dix-huit
lignes : la publication transférait 18 × 1024 × 4 octets, soit 74 Kio,
pour 1,3 Kio réellement modifiés — à chaque image, sur un mapping
write-combining où seul le nombre d'octets écrits compte.

`present_rect` découpe par colonnes ; `present_area` arbitre, et retombe
sur la recopie contiguë quand le rectangle occupe plus des trois quarts de
la largeur — au-delà, ce qu'on économise en colonnes ne compense plus la
perte de contiguïté.

### `pick_next` parcourait la table trois fois

Un passage pour l'anti-famine, un pour trouver la meilleure priorité, un
pour reprendre le tourniquet à cette priorité. C'est la fonction la plus
chaude du système. Ramenée à un seul passage qui retient la première
affamée **et** la première de plus faible priorité, l'anti-famine
l'emportant à la fin — l'ordre des trois boucles est reproduit, pas
approximé. Vérifié par l'égalité des compteurs de commutation et de
sauvetage avant et après.

### La souris fonctionnait ; c'est QEMU qui ne transmettait rien

Symptôme rapporté : le curseur ne bouge pas. Mesure : IRQ 12 reçue,
paquets décodés, publiés sur `dev.mouse`, routés par le compositeur,
**reçus par `cursor.mod`**, `move_surface` émis — et une capture d'écran
avant/après montrant le curseur disparaître de sa position d'origine et
réapparaître à la nouvelle. Toute la chaîne était correcte.

La cause est en amont : la souris émulée est une PS/2 **relative**, et le
display GTK ne transmet un déplacement que lorsque le pointeur de l'hôte
est **capturé** par la fenêtre. Sans clic préalable, l'invité ne reçoit
rien du tout. `-display gtk,grab-on-hover=on` capture au survol.

Un `-device usb-tablet` donnerait un pointeur absolu qui n'a pas besoin
d'être capturé, mais il faudrait alors une pile USB dans le noyau.

**Leçon** : avant de chercher un bug dans une chaîne à six maillons,
vérifier que quelque chose entre par le premier. Une capture d'écran
avant/après et un compteur d'IRQ ont tranché en deux mesures ce que la
relecture du code n'aurait pas trouvé — puisqu'il n'y avait rien à
trouver.

### Le rectangle endommagé du curseur ne correspondait pas à sa surface

`route_pointer` salissait « 14 par 20 » alors que `cursor.mod` crée une
surface de 16 par 16 : les trois colonnes de droite n'étaient jamais
salies. Invisible aujourd'hui — la flèche n'occupe que neuf pixels de
large — mais le premier curseur plus large aurait laissé une bande
verticale qui ne s'efface pas, et rien n'aurait désigné cette ligne. La
taille est désormais lue sur la surface, par `Stack::overlay_rect`.

Règle générale : une dimension qui appartient à un module ne se recopie
pas ailleurs. Deux vérités finissent toujours par diverger.

---

## Pile USB — arbitrages et pièges

### Le PS/2 ne suffit pas, et la raison n'est pas l'ancienneté

Une souris PS/2 ne rend que des déplacements. Un hôte qui l'émule doit
donc **confisquer** le pointeur physique avant de transmettre quoi que ce
soit, puisqu'il n'y a pas de position à communiquer. C'est cette
confiscation que l'utilisateur perçoit comme « il faut cliquer dans la
fenêtre pour que la souris marche ».

Un pointeur absolu transmet une position, qui a un sens sans capture.
C'est la vraie réponse au symptôme, pas un contournement.

### xHCI plutôt qu'UHCI — décision verrouillée

UHCI serait bien plus court, mais c'est de l'USB 1.1. Un pilote UHCI ne
fonctionnerait que sous émulation : ce serait écrire un pilote pour QEMU,
pas pour un ordinateur. Aucune machine construite depuis 2010 n'expose
autre chose que xHCI. **Ne pas ajouter UHCI ni OHCI** ; s'il faut couvrir
davantage, c'est EHCI pour les contrôleurs USB 2 séparés, pas les
générations antérieures.

### Le bit de cycle est ce qui remplace la tête et la queue

Un anneau xHCI n'a pas de pointeur de fin observable par les deux
parties. Seule l'alternance du bit de cycle distingue un enregistrement
neuf d'un vieux déjà consommé, et chaque lecteur garde son propre état.

Conséquence pratique, à ne jamais enfreindre : **le champ de contrôle
d'un TRB s'écrit en dernier**, avec une barrière avant. Il porte le
cycle, et le contrôleur peut lire l'anneau à tout instant : un TRB dont
le cycle serait valide avant que ses autres champs le soient serait
exécuté à moitié écrit.

### Les bits de changement d'un port s'effacent en écrivant un

`PORTSC` mélange des bits d'état ordinaires et des bits « écrire un pour
effacer ». Les recopier tels quels les **acquitte** ; les laisser à zéro
les **préserve**. La faute se manifeste par un port qui signale un
changement indéfiniment, et rien dans le symptôme ne désigne l'écriture
fautive. D'où `PRESERVE`, un masque explicite, et `acknowledge_port`, un
seul endroit qui sait le faire.

### Un endpoint d'interruption sans TRB en attente ne produit plus rien

C'est le piège qui fige une souris après son premier mouvement : le
transfert doit être **réarmé** à chaque rapport reçu. Le réarmement a
lieu après la boucle de consommation des événements, jamais pendant —
déposer sur l'anneau demande le périphérique en mutable et sonner demande
le contrôleur, deux emprunts qui se recouvrent.

### `busy_wait_micros` ne convient pas à une attente longue

Il masque le timer local (piège 15) : la préemption s'arrête sur ce cœur.
Acceptable pour les quelques microsecondes d'un transfert de contrôle,
inacceptable pour la seconde entière d'une échéance manquée. D'où
l'attente en deux régimes de `wait_event` : scrutation serrée pendant
cinq millisecondes, puis sommeil ordinaire.

### La forme d'un rapport vient de ce que l'interface déclare

Et non de sa longueur. Une souris d'amorçage dont l'endpoint annoncerait
huit octets serait sinon prise pour une tablette, et son deuxième octet —
un déplacement signé — lu comme la moitié basse d'une position absolue.
La longueur ne sert que pour les périphériques qui ne déclarent rien.

### Diagnostiquer une souris demande de savoir ce qui entre

`mouse_move` du moniteur QEMU envoie des événements **relatifs**. Une
tablette absolue ne les accepte pas : QEMU les route vers un autre
périphérique, et la tablette ne produit aucun rapport. Une pile USB
parfaitement fonctionnelle paraît alors morte.

Éprouver un pointeur absolu demande QMP et `input-send-event` avec des
axes `abs`. C'est la seconde fois dans ce projet qu'un problème de souris
n'était pas dans le système : la première fois c'était la capture du
pointeur, celle-ci le type d'événement injecté. **Vérifier d'abord que
quelque chose entre par le premier maillon.**

### Un curseur au dernier pixel est invisible, et c'est correct

Le point chaud du curseur est son coin supérieur gauche. Borné à
`largeur - 1`, il ne laisse qu'une colonne visible — et cette colonne est
son **contour**, dessiné en couleur de fond. Le curseur paraît disparaître
dans le coin bas-droit. Ce n'est pas un défaut de rendu : c'est ce que
fait tout système dont le point chaud est en haut à gauche.

---

## SDK et langages (phase 9) — arbitrages et pièges

### Sept lignes du §13.2 ne pouvaient pas être écrites, et le dire vaut mieux que le contourner

Le tableau §13.2 aligne huit langages avec, pour chacun, un interpréteur et
un compilateur. Sept supposent LLVM, Mono, une JVM ou V8 : plusieurs
centaines de mégaoctets de dépendance externe, exclues par la règle
« aucune dépendance ».

Le piège tentant était d'écrire un `dev build --lang rust` qui rende un
binaire de `nop` en annonçant une compilation. Il aurait passé tous les
tests structurels — le fichier est bien formé, il se charge, il s'exécute —
et menti sur le seul point qui compte.

La table de `sdk::runtime` transcrit donc le §13.2 **ligne à ligne**, et
chaque ligne porte en plus un `Support` qui dit ce que le SDK sait faire :
`InTree`, `Interpreted`, ou `Delegated`. Une ligne `Delegated` est résolue
dans `/sys/runtimes/` et **refusée nommément** quand elle manque —
`DevError::RuntimeMissing(Language::Python)`, pas un échec obscur.

**Décision verrouillée : aucun chemin du SDK ne doit produire un binaire en
prétendant l'avoir compilé.** Un refus nommé est un résultat ; un binaire
vide est un mensonge.

### Une neuvième ligne, assumée et signalée

Le §15.2 écrit `dev interpret main.py`. Le SDK doit donc avoir un
interprète, et écrire CPython n'est pas une option. Nova Script (`.ns`) a
été ajouté — un langage complet mais minuscule, écrit en entier dans
`sdk::script`.

Il est déclaré **hors §13.2** dans la table, exactement comme le drapeau
`NETWORK` ajouté au format binaire en 5.6.4 : une extension assumée, qu'on
signale pour que personne ne la prenne pour une ligne de la spécification.

### Un interprète de noyau a deux bornes qu'un interprète ordinaire ignore

Il tourne sur la pile d'une tâche (`process::STACK_SIZE`) et dans une tâche
ordonnancée. Deux conséquences, et aucune n'est facultative :

- **Un budget de pas.** Une boucle `while true` rendrait son cœur
  indisponible jusqu'au redémarrage. `Interpreter::steps` compte et rend
  `ScriptError::Budget` plutôt que de ne jamais rendre.
- **Une profondeur bornée**, à l'analyse comme à l'évaluation. Une
  expression parenthésée mille fois déborderait la pile pendant le
  *parsing* ; une fonction récursive ferait de même à l'exécution.

### Compter des niveaux ne borne pas une pile — c'est compter des octets qui la borne

**Le bug le plus coûteux de la phase 9.** Symptôme : après un démarrage
apparemment normal, `EXCEPTION CPU #14`, `RIP = 0xffff_8080_0009_0488`,
`CR2 = 0x1c`, ring 0. Écran figé sur le fond du compositeur.

`RIP` était dans le **heap** (`HEAP_START = 0xFFFF_8080_0000_0000`), pas
dans le texte noyau (`0xffff_ffff_8000_0000`). Le noyau exécutait des
données — en l'occurrence un tampon de `Vec<TaskInfo>`. Aucune conversion
vers un pointeur de fonction n'existe dans l'arbre : le saut ne pouvait
venir que d'une adresse de retour écrasée.

Cause réelle : `MAX_DEPTH = 24` bornait la récursion de
`sdk::script::Interpreter::call` à vingt-quatre niveaux. Mais un niveau,
dans une compilation de débogage, c'est la somme des cadres de `call`
(1784 o), `statement` (2664 o), `binary` (2520 o), `eval` (1352 o) et
`block` (360 o) — près de 9 Kio. Vingt-quatre niveaux réclamaient plus de
200 Kio sur une pile qui en faisait 32. L'autotest
`fn f(n) { return f(n) }` débordait donc de sa pile au cinquième niveau et
continuait à descendre.

Ce qui rend le défaut coûteux à diagnostiquer : **les piles de tâches
n'ont pas de page de garde.** Elles sont allouées dans le heap par
`Process::new`, et le heap est mappé de bout en bout. Déborder n'écrit pas
dans le vide et ne produit aucune faute : cela écrase la pile de la tâche
voisine, puis celle d'après. Ici, quatre-vingt-seize kilo-octets plus bas
— trois piles entières, dont celle du compositeur. Le système ne s'est
arrêté que bien plus tard, sur une adresse de retour corrompue, à des
dizaines de milliers d'instructions de la cause, et dans une tâche qui
n'était pas la coupable.

Trois corrections, complémentaires :

1. `process::STACK_SIZE` passe de 32 à 128 Kio, et `heap::HEAP_SIZE` de 4
   à 16 Mio pour les loger.
2. `scheduler::current_stack_bottom` et `stack_headroom` exposent la
   borne réelle de la pile courante, sans verrou, depuis
   `PerCpu.kernel_rsp`.
3. `Interpreter::tick` compare son `rsp` à un plancher relevé une fois à
   la construction (`stack_floor`, base + 32 Kio de marge) et rend
   `ScriptError::TooDeep`. `MAX_DEPTH` reste la borne de **politique** ;
   celle-ci est la borne **physique**, et c'est la seule que le
   compilateur ne peut pas démentir.

**Règle générale.** Tout code noyau dont la profondeur de récursion dépend
d'une donnée extérieure — un script, un fichier, une trame réseau —
interroge `scheduler::stack_headroom` avant de descendre d'un cran. Un
compteur de niveaux ne dit rien de la taille d'un niveau, et c'est le
compilateur qui la décide.

### Une tranche vide reste bornée

`file[data_offset..data_offset]` panique quand `data_offset` dépasse la
longueur : Rust vérifie l'indice de départ même lorsque la longueur est
nulle. `sdk::emit::emit` arrête volontairement le fichier à la fin de
`.text` quand il n'y a pas de données — `data_offset` désigne alors une
position au-delà de la dernière case. Tout binaire sans `.data`, c'est-à-dire
les cinq gabarits du §15.5, faisait paniquer le noyau. La copie est
maintenant sous `if has_data`.

### La clé d'un manifest n'est pas le suffixe d'un fichier

`BinaryFormat::extension()` rend `".cus"`. `manifest::to_toml` s'en servait
pour écrire `type`, alors que `format_from_key` relit `"cus"`. Aucun
`dev.toml` produit par le SDK ne repassait sa propre analyse : l'aller-retour
du §15.3 échouait, et avec lui tout ce qui commence par relire un manifest
généré — `dev build`, `dev run`, `dev package`. Un `format_key` distinct
sépare les deux usages.

### Une table en mémoire ne survit pas à un fichier qui, lui, persiste

`devstudio::install` était idempotente au sens du §17 : si
`/sysapp/devstudio.ss` existe, elle ne recrée rien. Mais elle sautait du
même coup `register_as_official`, alors que la table des empreintes
officielles vit en mémoire et repart vide à chaque démarrage. Résultat :
`signature::verify` rejetait l'IDE en `UnknownSystemBinary` (§3.3.2) sur
toute machine ayant déjà démarré une fois. Le second démarrage ne doit
rien recréer — il doit quand même **reconnaître**.

### Mesurer des cadres libres, c'est parfois mesurer l'ordonnanceur

Le contrôle « les exécutions rendent leurs cadres » tombait juste en
monoprocesseur et manquait treize cadres, exactement, à chaque démarrage
sous quatre cœurs. Ce n'était pas du bruit : `try_reap` refuse tant qu'un
cœur désigne encore la tâche morte comme courante, et ce cœur ne
l'abandonne qu'à sa prochaine élection. Les cadres revenaient, plus tard.
Le contrôle attend maintenant leur retour avec une échéance, au lieu
d'exiger le compte à un instant arbitraire.

### Un analyseur qui n'avance pas fait tourner un cœur pour toujours

`const { app, BrowserWindow } = require('electron')` a fait boucler
indéfiniment le parseur ETR. La déconstruction n'est pas reconnue, le
parseur retombe sur `expression()`, qui rencontre une virgule — un jeton
sur lequel elle s'arrête sans le consommer — et rend un nœud vide. La
boucle de `block()` rappelle `statement()`, qui refait exactement la même
chose.

En espace utilisateur, c'est un programme qui se fige. Dans un noyau,
c'est un cœur perdu, et le symptôme — la machine qui ralentit sans rien
dire — ne désigne pas le coupable.

**Règle : toute boucle d'analyse doit garantir sa progression.** `block()`
et `statement()` relèvent `self.position` avant l'appel et consomment un
jeton si rien n'a bougé. Le nœud produit est `Opaque`, ce qui est le
verdict prudent : ce qu'on n'a pas compris ne compte pas comme converti.

### Un dossier partagé dont le contenu ne l'est pas ne partage rien

`fs::perm::Mode` avait `shared_dir` — pour `/cache/`, `/ras/` et
`/users/_shared/` — et rien au niveau du fichier. Tout fichier créé dans
ces dossiers naissait avec `private_file` : le dossier acceptait qu'on y
écrive, et le fichier écrit devenait aussitôt intouchable pour tout autre
compte que son créateur.

Le défaut était invisible tant que rien n'écrivait dans ces dossiers
depuis deux comptes différents. `Mode::shared_file` le comble, sans
`EXECUTE` : un dossier ouvert en écriture au groupe est exactement
l'endroit où un binaire déposé par n'importe qui ne doit pas devenir
exécutable pour tout le monde.

### Une image lancée mais pas confiée fuit sans le dire

`loader::load_and_spawn` rend `(LoadedImage, Pid)`. `LoadedImage` n'a
**pas** de `Drop` : l'abandonner laisse ses cadres pris, ses pages mappées
et son créneau d'arène occupé, sans que rien ne le signale. Le premier
`dev run` écrit le faisait, et la fuite ne se voyait que dans le compte de
cadres libres, plusieurs relevés plus tard.

**Tout appelant de `load_and_spawn` doit appeler `loader::keep(pid, image)`.**
C'est ce qui range l'image sous son PID, et c'est là que
`scheduler::try_reap` ira la chercher.

### Le rejeu en `.ras` du §15.4 est une reconstruction, pas un changement d'étiquette

Trois règles se conjuguent : `(Ras, None)` est la seule combinaison admise
(§3.4), le plafond de permissions tient en trois bits (§12.4), et un `.ras`
dont la table de ports n'est pas vide est refusé par `check_rules` (§12.3).

Le premier jet gardait les ports du manifest. Le gabarit `service` en
déclare un, et son rejeu produisait un binaire que le chargeur refusait —
pour le format que le §15.4 impose justement d'utiliser. `plan_ras` porte
maintenant la décision au complet, `ports_dropped` compris.

Contrepartie heureuse : un `.dsa` SYSTEM, dont le chargeur refuserait
l'empreinte inconnue, devient testable **grâce à** cette transformation.
C'est ce qui prouve que le rejeu n'est pas cosmétique.

### Les tables de pages intermédiaires ne se rendent pas

`memory::unmap_page` rend la page, pas la table qui la portait. C'est vrai
de l'arène du §6.4 comme de celle du §15.4 : la première allocation dans
une tranche de 2 Mio coûte un cadre de plus, définitivement.

Ce n'est pas une fuite — c'est un coût d'installation, payé une fois par
tranche et par arène. Mais un autotest qui mesure les cadres avant et
après **la première** allocation le compte comme une fuite et échoue à
raison sur une prémisse fausse. L'autotest de la phase 9 fait donc un
**cycle de chauffe** avant sa mesure, et le dit dans son commentaire.

### Le taux de conversion se mesure sur les appels, et un appel interne compte

Le §13.1 dit « taux de réussite » sans dire de quoi. Trois lectures
étaient possibles — lignes, fonctions, sites d'appel. C'est la troisième,
parce que c'est la seule qui mesure ce qui va manquer à l'exécution.

Le premier jet comptait tout appel non répertorié comme un manque, y
compris les appels aux fonctions du projet lui-même. Conséquence perverse :
**plus une application était découpée en fonctions, plus elle paraissait
intraduisible.** ETR fait maintenant deux parcours — recenser les
déclarations, puis compter — et l'ordre compte : toutes les déclarations
de tous les fichiers doivent être connues avant le premier comptage.

### Un `/` en JavaScript se lit dans son contexte

Une expression régulière ne peut apparaître que là où une **valeur** est
attendue : après une ponctuation ou un mot-clé, jamais après un
identifiant, un nombre, une chaîne ou une parenthèse fermante. Sans cette
règle, `/a"b/g` ouvre une chaîne qui avale le reste du fichier — et le
taux de conversion devient une fiction.

### Les sauts sont toujours en `rel32`

Un assembleur qui optimiserait la taille des sauts verrait ses étiquettes
se déplacer, donc devrait recommencer jusqu'à point fixe. L'algorithme est
correct mais peut ne pas converger si on le programme mal. Cinq octets de
plus par saut valent mieux qu'une boucle de relaxation dans un noyau.

Même raison pour `mov r64, imm64` : toujours la forme de huit octets, même
pour charger 0. La taille d'une instruction ne dépend pas de son opérande,
et un désassemblage se relit.

### Les encodages se vérifient contre une table écrite à la main

Un assembleur faux produit du code parfaitement bien formé qui fait autre
chose. Rien ne le trahit — pas même une exécution, tant que l'instruction
erronée ne tombe pas sur un chemin emprunté.

Un test qui recalculerait l'encodage attendu avec la même fonction que le
code testé ne vérifierait que la reproductibilité de son propre bug. La
table de `check_encodings` est donc écrite à la main, depuis le manuel
Intel, et confrontée octet par octet.

### Une purge « au démarrage » n'a de sens que si rien n'a démarré avant

`sdk::start` efface les dossiers laissés sous `/cache/sandbox/` par une
session interrompue. Le raisonnement écrit dans son commentaire était
juste — le registre des bacs vivants est en mémoire, donc vide au
démarrage — mais **« au démarrage » n'est pas « avant tout le reste »** :
l'ordonnanceur tourne déjà quand `init.dsa` appelle `sdk::start`, et
l'autotest de la phase 9 est une tâche comme une autre.

Cette tâche se synchronisait sur `runtime::resolve(Asm)`, c'est-à-dire sur
la présence de `/sys/runtimes/asm/runtime.cfg`. Ce fichier **persiste** :
au premier démarrage la condition était fausse et l'autotest attendait,
à tous les suivants elle était vraie d'emblée et l'autotest doublait
`sdk::start`. Sa sandbox n° 3, vivante et en cours d'usage, était alors
purgée comme un résidu de session précédente.

Deux corrections, et les deux comptent :

- `purge_stale_sandboxes` interroge `sandbox::live_ids()` et saute tout
  dossier portant l'identifiant d'un bac vivant. C'est le filet de
  sécurité : il vaut quel que soit l'ordre des tâches.
- La barrière de `task_phase9_selftest` est devenue `sdk::is_started()`,
  un drapeau posé en dernier par `start`. Une barrière doit tester ce
  qu'elle attend, pas un effet de bord qui se trouve vrai la plupart du
  temps.

**Leçon générale** : une condition de synchronisation qui porte sur un
état *persistant* ne synchronise rien au deuxième démarrage. Le symptôme
était invisible — le `.ras` était déjà chargé en mémoire quand son
overlay disparaissait — et ne se voyait qu'à la ligne
`[sdk] 1 bac(s) a sable laisse(s) par une session precedente` d'un boot
sur un disque déjà formaté.

### Un compilateur pour un sous-ensemble, et qui le dit

`rustc` est un vérificateur d'emprunts, un solveur de traits, un
monomorphiseur et LLVM. La règle du dépôt interdit la dépendance et la
taille interdit la copie. Ce qui était possible, c'était de prendre la
part du langage qu'un programme système utilise vraiment — fonctions,
entiers, booléens, littéraux de chaîne, contrôle de flux — et de la
compiler pour de bon.

Le point qui rend l'affaire honnête n'est pas le périmètre retenu, c'est
la façon dont on sort du périmètre. `struct` ne produit pas « jeton
inattendu » ; il produit « struct n'est pas dans le sous-ensemble Rust de
Nova OS - les donnees se passent en parametres, un scalaire a la fois ».
Vingt-deux constructions ont ainsi leur propre refus, et l'autotest
vérifie que le message **nomme** la construction. C'est la même
discipline que le 9.5 appliquait déjà à un runtime manquant : dire quelle
pièce manque plutôt que rendre un binaire de `nop`.

Deux écarts avec `rustc` sont assumés et écrits : l'arithmétique boucle
au lieu de paniquer, et un décalage est masqué à six bits. Les deux sont
le comportement **release** de Rust, pas une invention ; et surtout le
compilé et l'interprété s'accordent dessus, ce qui est la seule propriété
qui compte — c'est l'accord qui permet de se servir de l'un pour éprouver
l'autre.

### Le générateur de code écrit du texte, pas des octets

`sdk::asm` sait déjà encoder x86_64, résoudre des étiquettes et laisser
les trous RIP que l'éditeur de liens comble. Émettre des octets depuis
`sdk::rustc` aurait mis un **second encodeur** dans le dépôt.

Deux encodeurs dérivent. Et la dérive ne se voit pas : elle ne fait pas
échouer une compilation, elle fait tourner un programme qui répond autre
chose. C'est exactement le mode de défaillance contre lequel l'en-tête de
`asm` mettait déjà en garde à propos des encodages vérifiés à la main.

Le texte intermédiaire a un second mérite : il se lit. Une instruction
fausse se **voit**, au lieu de se déduire d'un comportement.

Quatre instructions ont été ajoutées à l'assembleur pour que cela suffise,
et pas une de plus : les opérandes mémoire `[reg + dep]` — sans quoi les
variables locales n'ont que les seize registres pour vivre, ce qui plafonne
une fonction et interdit la récursion —, le `mov` d'un octet vers la
mémoire — la conversion décimale écrit chiffre par chiffre —, `imul` et
`idiv` signés — `mul` et `div` sont non signés, et s'en servir pour un
`i64` donne une réponse plausible et fausse pour tout opérande négatif —,
et les décalages.

### Une machine à pile, contre un allocateur de registres

Trois fois plus lent, et retenu quand même. Trois raisons, dont la
deuxième est la vraie :

1. **C'est manifestement correct.** Un allocateur qui déverse mal produit
   un programme qui marche jusqu'au jour où il ne marche plus.
2. **`syscall` détruit `rcx` et `r11`.** Avec toute valeur vivante sur la
   pile machine, un `println!` au milieu d'une expression ne peut rien
   corrompre. Un allocateur devrait connaître cette contrainte et la
   respecter à chaque site d'appel.
3. **Les appels imbriqués n'ont pas de liste de sauvegarde.** Le prologue
   et l'épilogue de l'appelé restaurent `rsp` ; ce que l'appelant avait
   empilé est toujours là.

### Deux bugs qu'aucune lecture n'aurait trouvés

Ils sont du genre propre aux compilateurs : ils ne font pas échouer la
compilation, ils font répondre autre chose.

- **Le repli de fin de fonction écrasait la valeur de retour.** Le
  générateur émettait `mov rax, 0` avant l'épilogue, au nom de « tomber
  en fin de fonction est un fait, pas une hypothèse ». Sauf qu'un corps à
  expression finale vient de laisser sa valeur dans `rax`.
  `fn somme(n: i64) -> i64 { if n == 0 { 0 } else { n + somme(n - 1) } }`
  s'assemblait, se chargeait, s'exécutait en ring 3, imprimait — et
  imprimait `0`. La leçon : **un repli ne se pose qu'après avoir vérifié
  qu'il n'y a rien à préserver.**
- **Un `break` dans un `if` final était avalé par l'interprète.** Un `if`
  en dernière position d'un bloc est la valeur de ce bloc, donc une
  expression ; et une expression rend une valeur, pas un flux.
  `loop { n += 1; if n == 7 { break; } }` tournait jusqu'à l'épuisement du
  budget de pas. Le compilé n'avait pas le problème — un `break` y est un
  `jmp` vers une étiquette qui l'englobe lexicalement — et c'est
  l'interprète qui a été mis d'accord avec lui.

  La leçon générale : **quand deux back-ends lisent le même arbre, celui
  qui a tort est celui qui a besoin d'un mécanisme en plus.** Le compilé
  n'avait rien à ajouter ; l'interprète avait besoin d'un champ pour
  reverser le flux qui s'échappe d'une expression.

### Une profondeur d'interprétation qui dépend de son appelant

L'interprète Rust plafonne à dix appels imbriqués, et le chiffre n'est
pas un choix de goût : un niveau coûte les cadres de `call`,
`call_inner`, `block`, `statement`, `expr` et `apply`, ce qui en build de
débogage se compte en kilooctets sur une pile de tâche de 128 Kio déjà
entamée par l'appelant.

Ce qui rend le cas instructif, c'est que **le nombre bouge avec la chaîne
d'appel qui a mené à l'interprète** : douze niveaux mesurés depuis un
appelant peu profond, huit depuis l'autotest, dont le cadre est énorme.
L'autotest imprime donc la profondeur atteinte au lieu de l'affirmer, et
n'exige qu'une récursion ordinaire. Affirmer un nombre aurait été
affirmer une propriété de l'autotest lui-même.

La limite comptée est réglée **sous** ce que le plancher de pile
attraperait, pour que le message soit « recursion trop profonde » avec un
nombre derrière, et non un garde-fou déclenché sur un compte d'octets.

Le compilé, lui, n'a pas cette limite : il récurse sur sa propre pile
ring 3. L'autotest le prouve en faisant tourner une récursion de
vingt-quatre niveaux que l'interprète refuserait.

### Un contrôle dont le commentaire promet plus que l'assertion

Le contrôle du 9.5.8 s'annonçait ainsi : « la résolution doit favoriser
le runtime de l'utilisateur ». L'assertion en dessous vérifiait que
l'assembleur se résout côté **système** — c'est-à-dire exactement ce que
`install_layout` vient de poser, donc ce qu'on aurait trouvé que la
priorité du §13.3 marche ou non. La règle n'avait aucun contrôle, et le
commentaire faisait croire le contraire pendant toute la phase.

Le test qui la vérifie vraiment doit **changer l'état** puis observer la
différence : poser un descripteur sous `/users/nova/runtimes/asm/`, voir
l'origine basculer sur `User`, relire la version depuis ce fichier-là
plutôt que de croire l'étiquette, vérifier qu'un autre compte continue de
voir le système, et le retirer. Cinq vérifications au lieu d'une, et la
règle est éprouvée.

La leçon : **un contrôle qui ne peut pas échouer ne contrôle rien.** Se
demander, pour chaque assertion, quelle panne la ferait tomber. Si la
réponse est « aucune », le contrôle est un commentaire.

### Une touche prise à une phase par une autre ne fait pas échouer d'autotest

`devstudio.ss` lançait `dev run` sur F6. Le §16.4 a ensuite donné F6 au
changement de panneau, et le compositeur classe donc F6 parmi les
raccourcis système : `is_global_shortcut` le route vers la couche
`Shell`, et `devstudio.ss` vit dans la couche `Window`. La touche
n'atteignait plus personne — aucun module de la couche `Shell` ne la
traite non plus. `dev run` est passé sur F9.

Ce qui rend le cas instructif, c'est qu'il ne **peut pas** apparaître
dans un autotest : une touche qui ne fait rien n'écrit rien, ne compte
rien et ne casse rien. Le seul moyen de le voir est de tenir la liste des
touches revendiquées par la spécification et de la comparer à celles que
les applications s'attribuent. Quand une phase ultérieure réserve une
touche, il faut relire ce qui la prenait déjà.

---

## Finitions (phase 10) — arbitrages et pièges

### Un pilote audio qui ne coûtait rien tant que rien ne jouait

`Ac97::fill` allouait 4 Kio de tampon de travail par appel, et la boucle
du pilote remplissait **tous** les tampons dépassés par le contrôleur à
chaque réveil, cent fois par seconde. Sur le papier c'est de l'avance
prise ; en pratique un contrôleur émulé dont le backend consomme
instantanément déclare tous ses tampons dépassés, et le pilote faisait
trois fois le travail d'un vrai, cent fois par seconde.

Rien ne l'avait montré parce que rien ne jouait jamais longtemps : le seul
son du système était le bip d'une seconde de l'autotest de la phase 6. Le
lecteur d'écran du 10.1.2 joue en continu, et le coût est devenu la
machine entière — les autres tâches ne reprenaient plus la main, et le
symptôme se décrivait comme « le système s'arrête peu après l'activation
du lecteur d'écran ». Il ne nommait ni l'audio, ni le pilote.

Deux corrections, et la seconde est la vraie :

1. le tampon de travail est alloué une fois, et l'accumulateur de
   `audio::mix_into` est conservé d'un appel au suivant ;
2. **un tampon par réveil**, pas tous. Un tampon vaut 2048 échantillons,
   soit 1024 trames stéréo, soit 21 ms de son ; la boucle tourne toutes
   les 10 ms. Un remplissage par tour produit deux fois plus vite que le
   matériel ne consomme, ce qui est toute la marge dont une file de
   quatre tampons a besoin.

**Leçon** : un chemin dont le coût est proportionnel à « ce que le
matériel a consommé » doit être borné par le temps, pas par le matériel.
Un périphérique émulé n'a pas les vitesses d'un vrai.

### Un flux audio ouvert en permanence fait tourner le pilote pour du silence

Le lecteur d'écran ouvrait son flux au démarrage et le gardait. Un flux
qui existe rend `audio::has_audio` vrai, donc fait remplir et publier des
tampons **pour toujours**, entre deux annonces distantes d'une minute.

Il s'ouvre maintenant par énoncé et se ferme après — y compris quand
l'énoncé est interrompu par le suivant. Le périphérique est au repos dès
que personne ne se fait parler, c'est-à-dire presque toujours.

### La TLB d'un autre cœur gardait l'occupant précédent d'un créneau

Limite ouverte par la phase 3 : « `unmap_page` n'invalide que le TLB
local ». Elle était accompagnée de « sans conséquence aujourd'hui — les
déchargements n'ont lieu qu'au démarrage ». Ce n'était plus vrai.

La phase 10.4 charge un binaire étranger dans un créneau d'arène que
d'autres images ont déjà occupé, pendant que l'ordonnanceur tourne. Sous
quatre cœurs, la tâche est partie sur le cœur 2, qui tenait encore la
traduction de l'occupant précédent : elle a exécuté **le code de
l'occupant précédent** et fauté au cinquième octet de son point d'entrée,
en lisant une adresse qui ne lui appartenait pas.

Le diagnostic était trompeur au premier degré : `RIP = 0x1000005` désigne
le début de l'image, la faute ressemble à un point d'entrée mal calculé,
et le même binaire fonctionne parfaitement sur un seul cœur.

La correction est un compteur de génération plutôt qu'une liste
d'adresses : une liste devrait être bornée, vidée et synchronisée, pour un
événement qui a lieu quelques dizaines de fois par démarrage.

**Ce qu'il ne faut pas refaire** : attendre l'acquittement. Le premier
essai attendait que tous les cœurs répondent, borné à un million de tours
d'attente active, **par page libérée**. Libérer une image de dix-huit
pages bloquait donc quatre cœurs dix-huit fois. Et l'attente n'achète
rien : l'entrée périmée nomme une page utilisateur, qu'aucun code noyau
n'adresse, et tout chemin vers le ring 3 se synchronise pour son propre
compte.

### Une touche frappée avant une invite y répondait

L'invite d'autorisation du §12.5 lit la file du clavier. Ce qui s'y
trouvait **avant** son affichage y répond : appuyer sur `o` pour autre
chose, et le dialogue d'autorisation qui s'ouvre un instant plus tard est
déjà accepté, sans que rien ait été affiché assez longtemps pour être lu.

Ce n'est pas une hypothèse : l'autotest de la phase 10.3 a levé une vraie
invite et l'a vue répondue instantanément par une touche laissée par
l'écran de connexion. Le cas où la touche qui traîne est justement `o` est
le cas où un programme obtient ce qu'il demandait.

`keyboard::drain` vide la file, et **tout** lecteur de réponse l'appelle
d'abord — l'invite graphique du 10.3.5 comme celle du 6.4.2.

**Leçon générale** : une file d'entrée qui survit à ce qui la lisait est
une file dont le contenu répond à la mauvaise question.

### Le plafond du format répond avant la déclaration, et c'est voulu

Un `.cus` qui lit `/sys/` est refusé pour **dépassement de plafond**, pas
pour absence de déclaration. La distinction n'est pas cosmétique : un
`.cus` ne peut pas déclarer `SYS_DIRECTORY` du tout, donc « vous ne l'avez
pas demandé » serait une réponse trompeuse à un programme qui n'aurait
jamais pu le demander. Un `.ss`, lui, le peut, et reçoit alors bien
« non déclaré ».

L'autotest garde les deux cas côte à côte, parce que c'est la paire qui
prouve que la règle porte sur la déclaration et non sur le chemin.

### Un émetteur ne reçoit pas sa propre publication

`ipc::publish` saute délibérément l'émetteur. Le premier autotest du
§14.1 s'abonnait puis publiait depuis la même tâche, et mesurait donc
**zéro abonné atteint** — quelle que soit la correction du code. Il
signalait une panne qui était la sienne.

La seule forme qui éprouve le vrai chemin est une seconde tâche : elle
s'abonne, se bloque, et enregistre ce qui arrive. L'autotest vérifie
maintenant aussi la propriété elle-même, parce qu'un changement de cette
règle rendrait le test faux à nouveau.

### Le séparateur décimal n'est pas dans la spécification

Le §14.2 donne le séparateur des **milliers** — espace, virgule, point —
et se tait sur le décimal. Le laisser fixe imprime `1,234,56` en anglais.

Règle retenue, et c'est un choix : une virgule aux milliers impose le
point aux décimales, tout autre séparateur impose la virgule. C'est la
convention des deux familles de locales, et la seule qui ne produise
jamais un nombre ambigu. L'autotest la vérifie dans les deux sens, pour
qu'elle ne change pas par accident.

### Les modes daltonisme corrigent, ils ne simulent pas

Deux choses opposées se font avec une déficience de vision des couleurs.
**Simuler** montre à un voyant ce que voit un daltonien : c'est un outil
de conception. **Corriger** déplace l'information hors du canal que l'œil
ne sépare pas : c'est une fonction d'accessibilité. Le §16.3 les range
sous « Accessibilité », donc ce module corrige.

D'où trois matrices de redistribution **asymétriques** : un protanope voit
le bleu, donc l'erreur rouge-vert y va ; un tritanope voit le rouge et le
vert, donc l'erreur bleue y va. Une seule matrice pour les trois est
l'erreur qui fait ressembler la daltonisation à une teinte.

### Le contraste élevé étire les canaux, pas la luminance

La forme élégante — étirer la **luminance** et redimensionner les trois
canaux dans le rapport obtenu — conserve exactement la teinte et
s'effondre : une couleur sombre et saturée a une luminance basse,
l'étirement la pousse sous zéro, et le rapport ramène alors les trois
canaux à zéro. Un bleu sombre ressort **noir** — et « c'était bleu » est
précisément l'information que la personne cherche à voir.

L'étirement par canal est une application monotone : il conserve l'ordre
des trois canaux, donc la teinte reconnaissable. Ce qu'il change est la
saturation, ce qui pour un mode « contraste élevé » est la direction
voulue de toute façon.

### Une nasale se casse aussi sur une seconde consonne nasale

`bon` est nasal, `bonne` ne l'est pas. La règle évidente — voyelle plus
`n` ou `m` **non suivi d'une voyelle** — donne `b-on-n` pour `bonne`,
parce que la lettre regardée après `on` est un `n`, qui n'est pas une
voyelle. Il faut les deux conditions : une voyelle **ou** une seconde
nasale casse la nasalisation.

Et une consonne doublée se prononce une fois. Elle est consommée en
paire, en court-circuitant les règles de contexte : `ss` entre deux
voyelles reste /s/ (`poisson`) là où un `s` seul au même endroit vaut /z/
(`usine`). Laisser la paire tomber dans la règle de la lettre seule
transformerait tous les `ss` en `z`.

### Le répertoire d'importation d'un PE est l'entrée 1, pas l'entrée 0

L'entrée 0 des répertoires de données est celle des **exports**. Un
autotest qui écrit sa table d'importation dans l'entrée 0 produit un
fichier que le parseur lit comme n'ayant aucune importation — exactement
la mauvaise réponse pour un test dont tout l'objet est qu'elles soient
lues, et le genre d'erreur qui fait accuser le parseur.

### Un emplacement de module réservé au diagnostic

L'autotest du 7.3.4 dépose un vrai `.mod` sur le disque et regarde le
chargeur le prendre, le mettre en pause et le rendre. Il occupait
l'emplacement `Accessibility`, choisi parce qu'« aucun natif ne le
réclame ». La phase 10.1 lui a donné un occupant réel, et les deux se
seraient disputé un emplacement — l'échec aurait ressemblé à un bug du
chargeur.

L'emplacement `Diagnostic` existe donc pour un test, et c'est un coût
assumé : l'autre solution était un test qui met le lecteur d'écran en
pause pour vérifier que la mise en pause fonctionne.

### Un autotest ne restaure pas ce qu'il n'a pas relevé

Trois défauts trouvés en validant la phase 10, tous du même genre : un
autotest qui abîme l'état de la machine qu'il vient de vérifier. Ils ne
faisaient échouer aucune des 340 vérifications — ils s'exercent **après**
elles, ou sur ce que l'autotest ne regarde pas.

1. `access::selftest::run` finissait par `with_tree(|tree| tree.clear())`
   sur l'arbre **global**. Chacun de ses tests construit pourtant son
   propre arbre local, et le commentaire de `sample_tree` dit exactement
   pourquoi : « un test qui viderait l'arbre global laisserait la machine
   sans arbre d'accessibilité ». La ligne de sortie faisait la même
   promesse à l'envers, sans que personne l'entende : `[access] arbre :
   0 element(s), 0 focalisable(s)`. Les trois nœuds que le coordinateur
   déclare pour le shell partaient avec, et Tab ne parcourait plus rien.
2. Le même autotest relevait le mode couleur d'entrée par
   `contrast::Mode::from_code(0)` — c'est-à-dire `None`, toujours — pour
   le « restaurer » en sortie. Une machine réglée sur la deutéranopie
   sortait de son autotest avec la correction éteinte.

La leçon générale : **relever l'état avec le même accesseur que le reste
du système**, jamais en le reconstruisant depuis une constante. Un relevé
qui n'interroge rien ne relève rien, et la restauration devient une
remise à zéro qui se fait passer pour une restauration.

Et son corollaire : une ligne d'état qui affiche zéro là où elle devrait
compter est un symptôme, pas un décor. Celle-ci a été imprimée à chaque
démarrage pendant toute la phase 10.

### Demander au mixeur si le matériel existe

Le lecteur d'écran ouvrait un flux avec `audio::open` et prenait le
succès pour « il y a une sortie audio ». Le mixeur existe qu'un
contrôleur le joue ou non : sur une machine sans carte son, l'appel
réussit. Le lecteur annonçait donc `sortie audio disponible` à l'instant
même où le pilote annonçait `aucun controleur AC'97 sur le bus PCI`, et
il poussait ses échantillons dans une file que rien ne vidait.

Le seul qui sache est le **registre des pilotes**, et il ne le sait
qu'une fois la sonde passée. D'où la difficulté réelle : le lecteur est
un module du compositeur, donc `gui` le démarre **avant**
`drivers::start_all`. L'interroger sans attendre aurait répondu
« absent » sur toutes les machines — la même erreur dans l'autre sens.
`reader::wait_for_audio` attend donc que le pilote `audio` ait quitté
l'état `Starting`, avec une échéance de cinq secondes, puis lit
`drivers::audio::is_present()`.

La règle qui en sort : **l'ordre de démarrage fait partie de
l'interface**. Un composant qui interroge un autre doit savoir s'il est
déjà là, et une réponse obtenue trop tôt ne se distingue pas d'une
réponse négative.

Le diagnostic affiché est celui du pilote, mot pour mot : « aucun
contrôleur AC'97 sur le bus PCI » et « le codec ne répond pas » sont deux
machines différentes, et un lecteur qui dirait seulement « pas d'audio »
cacherait laquelle.

### Les numéros d'appel NT sont les nôtres, et il faut le dire

Les vrais numéros de `ntdll` ne sont pas documentés et changent d'une
version de Windows à l'autre. En recopier une serait recopier une version,
pas une interface. Le sous-système traduit donc au niveau de l'**API NT**,
qui est documentée et stable, et un programme compilé pour Windows est
refusé nommément au chargement — sur ses importations — plutôt que
mal aiguillé à l'exécution.

---

### Les commentaires du dépôt sont en anglais

Décision du 1er septembre 2026, à la demande de l'utilisateur. Tout
`src/` et tout `boot/` ont été traduits : commentaires de ligne,
commentaires de bloc et documentation `//!` / `///`.

Ce qui **reste en français**, et doit le rester :

- les **chaînes affichées** — console série, écran, messages d'erreur
  rendus à l'utilisateur. La sortie de référence de la roadmap les cite
  mot pour mot ;
- les **identifiants de la spécification** exposés à l'exécution : noms
  de canaux, clés de fichiers de configuration, noms de rôles ;
- les **messages de commit** et les documents de référence à la racine.

`tools/generer-police.py` a été traduit en même temps que `font.rs` qu'il
engendre : traduire le fichier produit sans son générateur aurait été
défait au premier `make police`.

---

## Réseau à haut débit (hors phase, septembre 2026)

### virtio-net sondé en premier, l'e1000 gardé en repli

Demande : un navigateur capable de monter à 200 Mo/s. L'e1000 émulée est
une carte à 1 Gbit/s, soit 125 Mo/s au mieux, et chaque accès à un de ses
registres est un piège dans l'émulateur. `drivers::virtio_net` pilote la
carte paravirtuelle ; `drivers::nic` sonde virtio-net puis l'e1000, et la
pile ne connaît que `nic`. **Le défaut du Makefile est passé à
`virtio-net-pci`** (`QEMU_NIC`), `make run QEMU_NIC=e1000` éprouve le
repli. Aucune délégation de somme de contrôle ni segmentation négociée :
la mesure n'en a pas eu besoin.

### La carte n'était pas le goulot, la boucle d'attente l'était

Premier relevé : 28,6 Mo/s en virtio-net, 26,7 en e1000. Deux cartes
différentes au même débit : la limite était ailleurs. `tcp::wait_step`
dormait un tic après 2 000 tours vides, et pendant ce sommeil seule la
tâche pilote vidait l'anneau, une fois toutes les 10 ms : 256 trames par
tic, 37 Mo/s au plafond. **Règle : une tâche qui attend des données
pendant qu'elles arrivent ne dort pas** (`nic::recently_active`, deux
tics). Une connexion inactive dort toujours.

### `yield_now` dans une boucle d'attente donne le cœur pour une tranche entière

Après le correctif précédent, les relevés allaient de 26 à 838 Mo/s d'un
démarrage à l'autre. `yield_now` à chaque tour cédait le cœur à toute
tâche prête du même cœur, pour sa tranche complète. **Pendant un transfert
actif, `preempt_if_needed`** : le cœur n'est rendu qu'à l'échéance.

### Une boucle d'attente qui prend des verrous fait perdre des tics au cœur 0

Sans `yield_now`, la mesure annonçait 5 Go/s. Le lecteur prenait à chaque
tour vide le verrou de la carte, celui de la table TCP et celui du tas.
Tous masquent les interruptions, et un `SpinLock` n'est pas équitable.
Le cœur 0 attendait ces verrous interruptions masquées, perdait des
interruptions de minuteur, et `idt::ticks` (qui n'avance qu'avec le cœur
0) sous-comptait le temps. **Règles : un tour vide ne prend aucun verrou**
(`virtio_net::poll` compare l'index de l'anneau sans verrou) **et marque
une pause** ; **une mesure de durée ne se fie pas aux seuls tics** —
`net::bench` chronomètre au TSC étalonné et imprime les deux, un écart
entre eux est un défaut à chercher.

### Un ACK pour deux segments, vidés à la fin de chaque lot

Chaque ACK est une trame, donc une notification de la carte : 370 000
pour 512 Mio. Les segments reçus dans l'ordre sont acquittés par deux
(`ACK_EVERY`, le rythme du RFC 1122), et `nic::poll` appelle
`tcp::flush_acks` à la fin de chaque lot : un ACK ne survit pas à la
scrutation qui a reçu son segment, aucun minuteur n'est nécessaire. Un
segment hors ordre ou dupliqué est acquitté immédiatement : ces ACK
dupliqués alimentent la retransmission rapide de l'émetteur. `ACK_EVERY`
à 4 a été essayé, mais avant les correctifs d'ordonnancement ci-dessus :
les relevés d'alors ne départagent rien, et ce réglage n'a pas été
réévalué depuis.

### `run-nonet` démarrait avec une carte réseau

Sans `-netdev`, QEMU ajoute d'office une e1000 : la cible censée prouver
qu'un pilote absent se déclare absent pilotait en réalité une carte. Elle
passe désormais `-nic none`. Relevé :
`[drv-reseau] aucune carte virtio-net sur le bus PCI ; aucune carte Intel e1000 sur le bus PCI`.

---

## Navigateur Web Browser (phase 11) — arbitrages et pièges

### La fenêtre physique suit la RAM, pas un plafond fixe

Au-delà de 3 Gio d'invité, le firmware place le reste de la RAM après
4 Gio. La fenêtre de `boot.asm` ne couvrait que 4 Gio : l'allocateur
aurait distribué des cadres injoignables. `memory::extend_window` étend la
fenêtre, par pages de 1 Gio, **jusqu'à la fin de la RAM et pas plus** :
un BAR PCI 64 bits est placé au-dessus, et le projeter en mémoire cache
inviterait des lectures spéculatives sur un périphérique.

### Toute tâche démarrait avec une pile désalignée de 8 octets

Le cadre initial d'une tâche faisait « revenir » `switch_context`
directement dans la fonction d'entrée, avec RSP aligné sur 16. L'ABI
System V promet RSP ≡ 8 (mod 16) à la première instruction d'une
fonction. Toutes les tâches tournaient donc désalignées depuis la phase
2 ; sans SSE, rien ne le montrait. Le premier `fxsave` (11.2) a fauté.
Correctif : un trampoline assembleur (`task_trampoline`) appelle l'entrée
passée dans R12. **Règle : un cadre initial se termine par un `call`,
jamais par un `ret` vers une fonction.**

### L'état SSE se sauve à chaque commutation, sur la pile de la tâche

Sauvegarde immédiate (FXSAVE/FXRSTOR dans `switch_context`) plutôt que
paresseuse (`CR0.TS` et `#NM`) : la seconde économise 512 octets de copie
au prix d'un propriétaire par cœur à suivre à travers les morts de tâches.
L'image est posée sous les registres généraux, sur la pile noyau de la
tâche : aucun champ à ajouter à `Process`. AVX n'est pas armé
(`CR4.OSXSAVE` reste à 0), Firefox se replie alors sur SSE2.

### Firefox mono-processus

Relevé sous `strace` : Firefox lance ses processus de contenu par `fork`
puis `execve`. Une construction officielle ignore
`browser.tabs.remote.autostart=false` ; la nôtre ne l'est pas. Le fork
vise un seul processus, et `fork` reste hors du noyau tant qu'aucun autre
programme ne l'exige.

### La préemption du ring 3 imposait `swapgs` dans les interruptions

Avant 11.3, une commutation n'avait lieu que dans un appel système, où
`syscall_entry` a déjà échangé GS. Dès que le minuteur a préempté du code
ring 3, la tâche suivante a hérité de la base GS **utilisateur** ; revenue
d'un appel système par `swapgs; sysret`, elle repartait bases inversées et
son appel suivant fautait sur `gs:0x8`. **Règle : toute commutation se
fait avec la base GS du noyau chargée** — `preempt_user` et
`page_fault_entry` échangent GS quand l'interruption vient du ring 3, et
`user_exception` aussi avant de terminer la tâche.

### Les processus Linux sont de vrais processus

Le mode mono-processus de Firefox (`MOZ_FORCE_DISABLE_E10S`) est un
vestige : la version 140 lance encore des processus de contenu pour la
capture d'écran sans écran. Nova fournit donc `fork` (copie sur écriture,
compteurs de références par cadre), `vfork`, `execve` et `wait`, et
Firefox tourne dans son architecture d'origine.

### Un disque racine en tar, pas le système de fichiers Nova

Le système de fichiers Nova plafonne un nœud à 2 Mio ; `libxul.so` pèse
150 Mo. La racine Linux est une archive tar sur son propre disque,
reconnue à son premier en-tête (`.nova-webroot`) et **retirée du lot des
supports amovibles** avant l'autotest d'export de la phase 4, qui écrit
sur ceux-ci. Ce qu'un programme écrit va dans des tmpfs.

### Une attente ne lit jamais la mémoire utilisateur une fois armée

`wait::wait_until` arme le sommeil avant de tester sa condition. Une faute
de page dans la condition pourrait dormir à son tour, sommeil armé : la
tâche ne se réveillerait plus. Les futex lisent donc le mot **avant**
d'attendre, en s'inscrivant d'abord comme attente.

### Le noyau respecte les pages en lecture seule (`CR0.WP`)

Sans `CR0.WP`, une écriture du ring 0 traverse une entrée en lecture
seule. Avec la copie sur écriture, un enfant fraîchement forké qui
appelait `sigprocmask` voyait l'ancien masque écrit par le noyau **dans le
cadre encore partagé avec son parent** ; le tas du parent affichait plus
tard « Scudo ERROR: corrupted chunk header ». **Règle : `CR0.WP` est armé
sur chaque cœur** ; le noyau n'écrit en mémoire utilisateur que par
`user_copy` (une faute y est résolue comme une faute ring 3) ou par la
fenêtre physique.

### Une faute concurrente d'un remaniement de régions se rejoue

`vm::Space::resolve` remplit la page verrou relâché. Si un autre thread
coupe ou fusionne la région entre-temps (`mprotect` voisin), le début de
la région change ; le traiter comme une faute tuait Firefox au hasard. La
faute est rejouée tant que la **source** de la page (anonyme, ou même
fichier au même décalage) n'a pas changé.

### Le Web Browser s'affiche par le widget headless de Gecko

Plutôt qu'un backend `widget/nova` complet (des milliers de lignes par
plateforme), le fork garde le widget headless, qui gère déjà fenêtres,
tailles et évènements, et lui ajoute `NovaDisplay` : une fenêtre
`/dev/nova-gui` par fenêtre de premier niveau, WebRender logiciel dessine
directement dans le tampon partagé (`StartRemoteDrawingInRegion`), et les
popups (menus, listes) sont composés par-dessus au moment de présenter.
Le patch tient dans `ports/web-browser/nova/0001-widget-nova-display.patch`.

### L'interface Nova est une couche sur l'interface de Firefox

La demande était une interface propre à Nova. Réécrire le chrome de
Firefox aurait coûté ses fonctions (onglets, historique, extensions,
réglages). L'interface Nova est donc un style (`userChrome.css`, palette
de `gui::theme`), une page d'accueil Nova, un moteur par défaut
(DuckDuckGo) et la langue française, posés par `nova.cfg` et
`policies.json` dans l'image racine.

### QEMU tourne sous KVM par défaut — et TCG cachait trois bogues

`QEMU_ACCEL` vaut `-accel kvm -cpu host` quand `/dev/kvm` existe, sinon
`-accel tcg` ; `make run QEMU_ACCEL="-accel tcg"` rend l'ancien
comportement. Sous émulation, le navigateur mettait des minutes à
démarrer. Les trois bogues que le premier démarrage sous KVM a montrés
étaient là depuis longtemps : **tester sous les deux accélérations**.

1. **`STAR[63:48]` doit porter RPL 3 (0x1B), pas 0x18.** Intel force RPL 3
   sur les sélecteurs que `sysret` charge ; AMD copie `STAR[63:48] + 8`
   tel quel dans SS. Sur un Ryzen, chaque `sysret` laissait SS = 0x20
   (RPL 0) en ring 3, et la première exception suivante construisait un
   cadre `iretq` (CS 0x2B, SS 0x20) que le processeur refuse : `#GP(0x20)`
   sur l'`iretq` de `page_fault_entry`. Linux met `__USER32_CS` (0x23).
2. **Un handler `x86-interrupt` à code d'erreur tourne sur une pile
   désalignée.** Le processeur empile 48 octets et laisse RSP aligné sur
   16 ; le prologue de LLVM compte comme s'il valait 8 mod 16 (comme après
   un `call`). Tout ce que le handler appelle est décalé de 8 — jusqu'au
   `fxsave` de `switch_context`, qui exige l'alignement : un `#GP` en ring 3
   d'un programme Linux devenait un `#GP` du noyau. `user_exception`
   réaligne la pile à la main (`and rsp, -16`) avant de continuer ; la
   tâche ne revient jamais.
3. **Les invalidations de TLB en vol.** Voir les deux entrées suivantes.

### Aucune adresse n'est rendue avant que chaque cœur l'ait oubliée

`munmap` retire la région, relâche le verrou, **puis** attend que chaque
cœur ait vidé son TLB (`shootdown_and_wait` exige les interruptions, donc
pas de verrou). Dans cet intervalle, un autre thread pouvait obtenir les
mêmes adresses par `mmap` et y écrire depuis un cœur qui en gardait
l'ancienne traduction : **sans faute**, directement dans l'ancien cadre,
en route vers l'allocateur. Le lecteur suivant trouvait la page vide —
un en-tête de groupe du `malloc` de musl à zéro, et le processus de
contenu mourait en lisant `meta->mem` à 0x10 : les recherches restaient
en chargement. Aucune vérification dans `resolve` ne peut l'attraper,
puisqu'il n'y a pas de faute. **Règle : `vm::Space` compte ses
invalidations en vol (`invalidating`, augmenté sous le verrou) ; `map`,
`map_exact` et `remap` ne distribuent d'adresses qu'à compteur nul
(`lock_settled`), et une faute n'installe pas de cadre non plus.** C'est
ce que Linux obtient en vidant avant de relâcher `mmap_lock`. Le
programme `/nova-tests/mmstress` le vérifie à chaque démarrage.

### Un cadre qui change sous une page s'invalide sur tous les cœurs

`break_cow`, quand le cadre reste partagé, pose une copie dans l'entrée
et ne vidait que le TLB local : les autres threads lisaient encore
l'ancien cadre pendant que celui-ci écrivait la copie. Même règle que
Linux (`ptep_clear_flush`) : **toute entrée dont le cadre change est
invalidée partout avant que l'écriture fautive reprenne**, et l'ancien
cadre n'est rendu qu'ensuite. Idem pour la copie d'une page du cache.

### L'image racine a un cache de pages, jamais vidé

Mesure avant : 6,5 s de disque sur 9,1 s de démarrage, 345 Mio lus pour
une image de 450 — chaque processus relisait sa propre copie de
`libxul.so`, une commande AHCI par bloc. Chaque page de fichier de
`tarfs` est désormais lue une fois (avec 31 suivantes, en une commande
dont la PRDT vise directement les cadres), gardée, et projetée **en
lecture seule dans tous les processus** (bit logiciel `CACHED` = bit 52,
avec `BORROWED`) ; la première écriture la copie. Le cache ne rend
jamais ses cadres : le faire exigerait de savoir quels espaces les
projettent (une carte inverse que `vm` n'a pas), et un plafond d'un
tiers de la mémoire libre au montage tient toute l'image. `mprotect` ne
doit jamais rendre une page `CACHED` inscriptible (`reprotect_range`).

### Le navigateur tourne sans Fission

Un processus coûte environ une seconde à démarrer sous Nova, et un `.cus`
n'a que deux cœurs sur quatre. L'isolation par site (Fission) en démarre
un par site visité. Sans elle, les pages partagent quatre processus de
contenu, dont un préparé d'avance. Contrepartie assumée : deux sites
d'onglets différents peuvent partager un processus. Les services de fond
de Mozilla (expériences, réglages distants, sondes de connectivité) sont
coupés dans `nova.cfg` : ils prenaient les cœurs de la première page.

### Le fuseau est par compte, l'horloge par machine

Le fuseau est une préférence d'affichage (14.2 : stocké en UTC, converti
à l'affichage) : ligne `fuseau` du `i18n.cfg` du compte, seule ligne
réécrite. La synchronisation NTP règle l'unique horloge de la machine
(`rtc` et `CLOCK_REALTIME` Linux), son interrupteur est dans
`/sys/config/horloge.cfg`. L'heure de `rtc` est désormais un seul mot
(`EPOCH_AT_TICK_ZERO_MS`) : une correction ne peut plus être lue à moitié
écrite. Les programmes Linux reçoivent le fuseau par `/etc/localtime`, un
lien vers `/usr/share/zoneinfo` (paquet `tzdata` de l'image) : ICU lit le
**nom** de la zone dans la cible du lien. Une zone Nova qui a une heure
d'été ailleurs devient `Etc/GMT-N` du décalage choisi, pour que le
navigateur affiche la même heure que la barre des tâches.

### La tâche inactive ne court que quand rien d'autre ne peut

`pick_next` ne considère que les tâches `READY` ; la tâche qui cède
(`yield_now`, ou la fin de sa tranche) est encore `RUNNING`, donc
invisible. Seule sur son cœur, elle était remplacée par la tâche inactive,
qui dormait (`hlt`) jusqu'au tic suivant : jusqu'à 10 ms perdues à chaque
`yield` et à chaque fin de tranche — un cinquième d'un cœur pour un `.cus`
d'arrière-plan, 20 s pour les 2 000 `yield` de l'autotest SSE. Et la tâche
inactive, jamais élue, passait pour « affamée » (7.5) et prenait un tic
toutes les demi-secondes. **Règle : `yield_now` garde la tâche courante
plutôt que d'élire la tâche inactive ; la tâche inactive n'est jamais
secourue par l'anti-famine.**

### Le son d'un programme Linux passe par un greffon ALSA

Le navigateur joue par ALSA (cubeb). Plutôt que les ioctls de
`/dev/snd/*`, un périphérique minimal, `/dev/nova-audio` (48 kHz,
stéréo, s16 : le format du mixeur ; `PENDING`, `FLUSH`), et un greffon
`ioplug` en espace utilisateur (`tools/ports/alsa-nova/pcm_nova.c`) qui
tient la comptabilité d'ALSA et **convertit lui-même** (flottants, mono,
toute fréquence, interpolation cubique). La couche `plug` d'ALSA le
faisait, mais refusait une latence sur deux, dont les 2 ms de la sonde de
Firefox. Pièges : un greffon se compile avec `-DPIC` ; le pointeur
matériel ne doit jamais avancer d'un tampon entier entre deux lectures
(sous-alimentation annoncée par `-EPIPE`) ; cubeb doit être forcé sur
ALSA et sans relais AudioIPC (`nova.cfg`).

### Le paquet de langue porte sa propre marque

Le fork s'appelle « Web Browser », mais le paquet de langue français de
Mozilla apporte `brand.ftl` et `brand.properties` (« Mozilla Firefox ») et
l'emporte en français. `tools/ports/rebrand-langpack.py` les réécrit à la
construction de l'image, et remplace « Firefox » dans les textes (hors
noms de services : Firefox View, Relay...).

### Le profil du navigateur est préparé avec l'image

Le profil vit en mémoire (tmpfs) : chaque session refaisait le premier
démarrage de Firefox (bases, paquet de langue, cache de démarrage), une
seconde de plus. `webroot.sh` lance le navigateur une fois sur l'hôte,
dans la racine même de l'image (bubblewrap), et `nova.cfg` le fait quitter
proprement (`NOVA_PREPARE_PROFILE`) : le cache de démarrage n'est écrit
qu'à la sortie. Le lanceur recopie ce profil à la première ouverture. Le
répertoire du profil doit exister avant le lancement : absent, Firefox
attend un dialogue qu'un mode sans écran ne montre pas.

### QEMU : 8 Gio, 8 cœurs, menu GRUB d'une seconde

Un `.cus` n'a que la moitié haute des cœurs (spec 7.2) : sur l'ancien
`make run` monocœur, le navigateur partageait un cœur avec tout le système
et une vidéo 720p perdait neuf images sur dix. `QEMU_SMP` vaut 8 (le
maximum du noyau) pour `run`, `run-full`, `run-nogui` et `log` ;
`QEMU_SMP=1` reste la variante monocœur. Le délai du menu GRUB passe de
5 s à 1 s : la spec 2.2 fixe ses entrées, pas son attente.

### Les échéances Linux comptent en millisecondes, le système en tics

Firefox rythme ses images par des minuteries (16,7 ms pour le
rafraîchissement, 33 ms pour une vidéo à 30 i/s). Arrondies au tic de
10 ms suivant, plus un pour ne jamais expirer tôt, elles tombaient à 20-30
et 40-50 ms : images irrégulières et perdues, quelle que soit la
puissance. Passer tout le système à 1 kHz aurait changé la durée de
chaque tranche de la spec 7.3, de chaque délai en tics et de chaque
autotest. Seul le **cœur 0** bat à 1 kHz (`FINE_HZ`) : une interruption
sur dix est le tic système (`idt::ticks`, inchangé), les dix servent les
échéances des attentes Linux (`idt::fine_ticks`, `futex`, `poll`, `epoll`,
`nanosleep`). Les autres cœurs restent à 100 Hz. `busy_wait_micros` relit
le compte du minuteur dans son registre : les cœurs n'ont plus tous le même.

### Le pont de fenêtre se réveille sur PRESENT

Il attendait le tour suivant de sa boucle de 10 ms. `PRESENT` le réveille
désormais, et accepte un rectangle : seule la zone changée (unie à celle
de l'image précédente, le canevas étant à double tampon) est recopiée.
Mesuré, la copie ne coûtait que 0,3 ms ; le réveil compte plus que la copie.

### Répartir les fils réveillés : essayé, mesuré, retiré

Les deux fils les plus actifs du navigateur partageaient un cœur. Placer
un fil réveillé sur un cœur libre de sa plage (spec 7.2 le permet) exige
de savoir qu'il a quitté son ancien cœur (sinon un second cœur le reprend
sur une pile non encore sauvée) ; mesuré en conditions propres, aucun
gain. `cpu_affinity` reste fixé à la création. **Règle de mesure :** une
machine virtuelle à la fois — `test-window.sh` laisse la sienne tourner,
et des mesures enchaînées se disputaient l'hôte.

### Pavé numérique

Verr Num actif au démarrage, basculé par sa touche, montré par le voyant
(commande 0xED ; les accusés 0xFA du clavier sont ignorés par le
décodeur). Un chiffre du pavé est publié avec le code de la touche de la
rangée principale qui tape le même chiffre : les codes du pavé sont ceux
des flèches, d'Origine, de Fin..., et une application lisant le code
aurait déplacé son curseur. Maj inverse Verr Num, comme sur tout PC.

### Les fichiers du navigateur sont des nœuds du système de fichiers Nova

La racine Linux du navigateur vivait sur une image tar dépliée en mémoire
(`tarfs`), sur un second disque. Elle est maintenant écrite dans le volume
Nova, sous `/apps/web-browser/root` : mêmes nœuds, mêmes tables de liens,
mêmes droits que le reste. `tarfs` est supprimé, le second disque aussi.
Conséquences assumées :

* le format passe en **v2** — liens de 128 octets (noms de 120 caractères,
  contre 56 : les fichiers de Firefox dépassent) et **deux niveaux
  d'indirection**, soit 1 Gio par fichier au lieu de 2 Mio ;
* l'hôte remplit le volume avec `tools/nova-fs.py`, qui écrit le même
  format que le noyau, liens durs et symboliques compris ;
* `src/linux/vfs/novafs.rs` présente un dossier Nova comme racine Linux,
  avec un cache de pages partagées et une lecture anticipée ;
* le profil du navigateur est dans `/users/{compte}/apps/web-browser` et
  survit donc à l'extinction.

Un volume v1 se reprend avec `make disk-migrate` (l'ancien est gardé en
`.v1`), puis `make webroot` y écrit le navigateur.

### L'arborescence de la spec 17 est imposée, pas seulement créée

Elle était construite une fois, sur un volume que le noyau venait de
formater. Un volume rempli par l'hôte arrive avec des dossiers créés en
passant, par exemple un `/users/` né comme simple parent de
`/users/admin/apps/web-browser` : droits de dossier privé, propriétaire 0,
et plus aucun compte n'y entre. Neuf autotests échouaient ainsi. La passe
tourne donc à **chaque démarrage** et, pour chaque dossier de la spec 17,
remet les droits et le propriétaire à ce que la spec dit — elle ne se
contente pas de créer ce qui manque. Elle est silencieuse quand elle n'a
rien créé.

### Tout bloc revient, y compris les tables

Deux fuites, trouvées par des autotests qui en cherchaient une :

* un dossier ne rendait jamais un bloc de liens vidé — un bloc perdu par
  arbre supprimé (`[explorer] blocs libres : 395263 avant, 395262 apres`).
  `remove_link` rend maintenant les blocs de queue entièrement vides ; un
  trou au milieu reste, le combler exigerait de renuméroter la suite ;
* un fichier tronqué gardait sa table d'indirection. Les bases du profil
  du navigateur grandissent puis se tronquent à chaque démarrage : au
  second, `[fs] INCOHERENCE : noeud N garde une table d'indirection
  inutile`. `truncate_node` rend la table dès qu'elle ne décrit plus rien.

Un volume écrit par un noyau antérieur à ce correctif garde ses tables
inutiles jusqu'à réécriture des fichiers concernés.

### Un volume d'une autre version n'est jamais formaté

Formater le disque de démarrage, c'est installer le système : le noyau le
fait sur un disque vierge. Mais le passage au format v2 a créé un cas
nouveau — un disque qui porte bien la signature `NOVAFS`, avec des
données, dans une version que ce noyau ne lit pas. Le formater serait
effacer le travail de l'utilisateur pour un numéro de version. Le volume
est donc laissé intact, la série dit quoi faire
(`make disk-migrate`), et le système se replie sur un volume en mémoire
de 8 Mio pour rester utilisable. Vérifié : le disque ressort avec la même
empreinte qu'avant le démarrage.

### Une tâche est renommée à l'exec

Le profil créditait 1,4 s de démarrage du navigateur à une tâche nommée
`sh` : le nom de la tâche restait celui du programme lancé, alors que
`execve` avait remplacé l'image. `linux::process::execve` appelle
maintenant `scheduler::set_name`. Sans quoi toute mesure par tâche ment
dès qu'un script lance un programme.

### Mesurer le même binaire sur l'hôte et sous Nova

Demande du 23 septembre 2026 : pages en moins de 50 ms, vidéo et son
parfaits. Avant toute correction, le même Firefox, la même page et le
même profil ont tourné sur l'hôte (bubblewrap, 4 cœurs) et sous Nova :
6 à 15 ms par navigation d'un côté, 65 à 100 de l'autre. Puis un
micro-banc statique (`tools/ports/tests/sysbench.c`, `/nova-tests/sysbench`)
a mesuré chaque chemin du noyau des deux côtés. C'est ce tableau, pas une
lecture du code, qui a trouvé les défauts qui suivent. **Règle : un écart
de performance se localise en comparant au natif, primitive par
primitive.**

### L'identité du cœur ne se lit plus dans le LAPIC

`smp::current_cpu_id` lisait le registre d'identité du LAPIC par MMIO.
Sous KVM, un accès MMIO au LAPIC est une sortie de VM et une instruction
émulée, et la fonction servait plusieurs fois par appel système : un
appel système nu coûtait **6 800 ns contre 49 sur l'hôte**. Chaque cœur
écrit désormais son numéro dans `TSC_AUX` (disposition de Linux : cœur
dans les 12 bits bas) et le relit par `RDPID`, à défaut `RDTSCP`, sans
sortie : 60 ns. Le LAPIC reste le repli (TCG, vieux processeurs).
**Règle : rien de chaud ne touche le LAPIC en MMIO.**

### x2APIC quand le processeur l'offre

Un IPI en xAPIC coûtait trois accès MMIO émulés (commande haute, basse,
relecture de l'état de livraison), plus l'EOI du côté receveur. En
x2APIC, un IPI est un `wrmsr` de la commande 64 bits, que KVM traite sur
son chemin rapide, et l'état de livraison n'existe plus. Le mode est
choisi par le premier cœur (CPUID.1:ECX bit 21) et appliqué par chacun à
son LAPIC ; l'identité x2APIC est le registre entier, pas son octet haut.

### Sous KVM sur AMD, le PIT ne réinjecte pas ses tics

`QEMU_ACCEL` porte `-global kvm-pit.lost_tick_policy=discard`. Avec la
politique par défaut, KVM réinjecte les tics perdus du PIT, et cette
réinjection **interdit l'AVIC** (livraison matérielle des interruptions
entre cœurs) : chaque IPI passait par l'hyperviseur. Aller-retour entre
deux threads par un tube : 17 µs avant, 6 après (3 sur l'hôte). Nova ne
se sert du PIT que pour étalonner son minuteur, sans interruption. Hors
KVM, le périphérique `kvm-pit` n'existe pas : le drapeau n'est posé que
dans la branche KVM.

### Une invalidation de TLB ne vise que les cœurs de l'espace

`shootdown_and_wait` interrompait et attendait **les huit cœurs** à chaque
`munmap`, `mprotect`, `madvise` de n'importe quel processus, et vidait
huit TLB. Les espaces privés invalident désormais par
`shootdown_space_and_wait(cr3)`, qui ne vise que les cœurs dont le CR3
est, ou va devenir, celui de l'espace (`vm::may_hold`). C'est suffisant
parce que les pages utilisateur ne sont pas globales et que CR3 est
rechargé à chaque changement d'espace (pas de PCID) ; `vm::activate`
publie le nouveau CR3 **avant** de le charger, clôture comprise, pour
qu'une invalidation qui ne le voit pas ait changé ses entrées avant que ce
cœur les parcoure. Chaque cœur a sa génération demandée
(`CPU_TLB_WANTED`) : un cœur non visé ne vide plus rien. `mprotect` :
17 → 2,2 µs ; `mmap`+faute+`munmap` : 9,2 → 1,3 µs (hôte 1,34).
`/nova-tests/mmstress`, qui reproduit la course de 11.11, reste conforme
dans ses quatre modes.

### Un réveil sur le cœur même lève le drapeau de réordonnancement

`wake` n'envoyait d'IPI qu'à un **autre** cœur. Un thread réveillé par un
voisin de son propre cœur (futex, tube, socket entre deux threads du
navigateur) attendait la fin de la tranche du réveilleur, jusqu'à 100 ms
pour un `.cus` ; un pilote réveillé par une interruption sur un cœur 0
inactif attendait le tic suivant. Le drapeau du cœur est désormais levé,
et le prochain point de préemption élit à nouveau — ce que l'IPI faisait
déjà ailleurs.

### Aucun vidage n'imprime en tenant un verrou pris à chaud

`scheduler::dump_tasks` imprimait ses ~270 lignes **verrou de
l'ordonnanceur tenu**, interruptions masquées ; chaque octet de la série
est une sortie de VM. Toutes les 30 secondes, aucun cœur ne pouvait élire
ni réveiller pendant des dizaines de millisecondes : une vidéo 1080p y
perdait 8 images d'un coup, la carte son se vidait, un thread attendait
300 ms. Même défaut, plus petit, dans `smp::dump_state` (`CPUS`, que
chaque IPI lisait), `tcp` et `udp` (tables prises à chaque paquet),
`ports`. **Règle : copier sous le verrou, imprimer après.** Et le moniteur,
tâche noyau que le minuteur ne préempte pas, cède son cœur entre les
lignes (`preempt_if_needed`). `smp::lapic_of` se lit sans verrou.

### Le périphérique son dit quand réveiller son écrivain

`/dev/nova-audio` annonçait `POLLOUT` dès qu'un quart de sa file (une
demi-seconde) était libre, donc **toujours** : la boucle `poll` de cubeb
tournait à vide, 14 000 appels par seconde, un cœur entier pris au
décodeur vidéo tant que le son jouait — et le verrou du contexte cubeb si
souvent tenu qu'un `AudioContext` ouvert après une vidéo ne démarrait pas.
Le greffon déclare maintenant son seuil (`THRESHOLD` : tampon moins une
période, en trames du périphérique), comme l'interruption par période
d'une vraie carte : 2 100 `poll` pour 10 s de vidéo. Le pilote réveille
les écrivains dès qu'il a pris un tampon.

### Un tampon n'est mélangé court qu'en dernier recours

Un flux mélangé avec moins d'un tampon (1 024 trames) est complété de
silence **au milieu de son son** : un clic. Le pilote attend donc un tour
tant que l'anneau garde le tampon en cours et deux d'avance (43 ms), et
ne prend un tampon court que si l'anneau va se vider. Même règle à
l'amorçage : il amorçait les quatre tampons, alors qu'un lecteur démarre
avec ce que sa latence permet (40 ms pour le Web Audio de Firefox) —
64 ms de silence juste après la première note de chaque son. Le compteur
de trous de `[nova-audio] flux N ferme` compte aussi le dernier tampon
d'un son, légitimement court.

### L'horloge du son se lit dans le DMA, lissée

Firefox cale l'image sur la position du son (`snd_pcm_delay`). Le délai
était la file du mixeur plus une constante : il avançait par sauts de
1 024 trames (21 ms). `DELAY` ajoute ce que l'anneau AC'97 garde (`CIV`,
`LVI`, `PICB`), et `PlaybackClock` extrapole entre deux sauts de
l'émulateur (le minuteur audio de QEMU consomme 10 ms d'un coup), d'un
saut au plus, sans jamais reculer.

### La réception réseau interrompt (MSI-X)

La tâche pilote réseau scrutait l'anneau toutes les 10 ms (20 pour un
sommeil arrondi) : chaque aller-retour d'un chargement de page (DNS,
TCP, TLS, HTTP) pouvait l'attendre. La file de réception de virtio-net
envoie un message MSI-X (vecteur 64, cœur 0) : un front, que le relais
générique sert sans tempête, contrairement à une ligne PCI. La tâche
coupe l'interruption pendant qu'elle vide l'anneau et la réarme avant de
dormir, puis regarde une dernière fois (NAPI de Linux). L'e1000 et une
carte sans MSI-X gardent la scrutation.

### Les pages `file://` vont dans les processus web

`browser.tabs.remote.separateFileUriProcess=false` (`nova.cfg`) : passer
d'une page web à une page locale, ou l'inverse, attendait le démarrage
d'un processus (600 à 800 ms). La séparation protège ce que le bac à
sable de contenu isole, et ce bac à sable est coupé sous Nova.

### La tâche inactive guette avant de s'arrêter

`hlt` rend le processeur virtuel à l'hôte, et le réveil suivant doit le
faire reprogrammer. La tâche inactive guette son drapeau 50 µs avant
`hlt` (`IDLE_POLL_US`, le `cpuidle-haltpoll` des invités Linux), mais
seulement quand elle revient d'un vrai travail : un cœur réveillé par son
tic à vide s'arrête aussitôt. Aller-retour par tube : 7,8 → 6,0 µs ; à
vide, QEMU prend 8 % d'un cœur de l'hôte avec ou sans.

### Placer une tâche selon la charge à sa naissance : essayé, mesuré, retiré

Le décodeur vidéo naissait sur le cœur des deux fils principaux (le
moins de tâches, mais les plus occupées). Choisir à la naissance le cœur
le moins occupé (moyenne glissante par tic) n'a rien changé de mesurable
à la régularité des images (107 à 147 intervalles réguliers sur 155
avant comme après). Retiré : pas de gain, pas de complexité.

### `nova-fs.py import` met à jour

`insert_link` ne cherchait pas de doublon : un second `make webroot` sur
un volume rempli ajoutait un lien homonyme, l'ancien fichier restait
celui qu'on trouvait et les blocs du nouveau étaient perdus. L'import
entre maintenant dans un dossier existant et réécrit en place un fichier
dont le contenu a changé (même nœud, mêmes liens, blocs et tables
rendus) ; `put` remplace un seul fichier.

### Une taille venue d'un programme n'entre jamais entière dans le tas noyau

« ALLOCATION ECHOUEE : 5898240 octets, alignement 1 » et la connexion
coupée : `recvmsg` allouait dans le tas noyau (64 Mio, celui de tout le
système) un tampon de la taille demandée par le programme, jusqu'à
16 Mio ; `sendmsg` rassemblait le message entier, `writev` jusqu'à
64 Mio. Firefox fait passer par ses sockets IPC des messages de plusieurs
mégaoctets (5 898 240 = une image 1920 × 768 × 4) : le tas se fragmente,
une allocation échoue, et `alloc_error` arrête le noyau. Désormais les
données passent par morceaux (`files::gather_range`, 256 Kio ; ce que la
socket peut contenir pour une réception), les tampons s'obtiennent par
`files::user_buffer` (`try_reserve` : `ENOMEM` pour l'appel, pas l'arrêt
du système), et la file de réception TCP refuse un segment qu'elle ne
peut loger plutôt que de faire tomber le noyau (l'émetteur le renvoie).
**Règle : toute taille fixée par un programme est bornée et faillible.**

Piège rencontré en route : réduire le morceau de lecture à 256 Kio a
rendu `read` court sur un fichier ordinaire. POSIX le permet, Linux ne le
fait jamais avant la fin du fichier, et le lecteur de médias de Firefox
n'a plus montré d'image pendant les 4 premières secondes d'une vidéo.
**Un fichier ordinaire se lit en entier** (`read_to_user` boucle sur un
seul tampon) ; seuls les tubes, sockets et périphériques rendent une
lecture courte.

### Le navigateur devient natif, par une libc Nova (23 septembre 2026)

**Décision de l'utilisateur, discutée avant tout code.** Elle remplace la
« voie retenue » de la phase 11 (Firefox exécuté par la personnalité
Linux) : le navigateur doit être un programme natif de Nova. Trois voies ont
été présentées ; retenue : la libc Nova (spec §6.7), plan en phase 12.

- **Écarté : emballer le binaire actuel en `.cus`.** Il aurait gardé les
  appels Linux : ce n'est pas natif.
- **Écarté : porter les couches internes de Firefox sur l'API Nova sans
  libc.** NSPR, IPC, widget, réseau, audio, et les crates Rust qui appellent
  `libc` : des milliers de sites, et au bout une couche POSIX de fait.
- **Retenu : musl, dont les fonctions appellent l'API Nova.** Le programme
  garde l'ABI utilisateur de musl (types, constantes, `errno`) : Firefox, ses
  bibliothèques et son code Rust se compilent sans modification pour
  `x86_64-linux-musl`. Seule la frontière avec le noyau change. C'est la voie
  de Redox (relibc), Haiku, SerenityOS.
- **Conséquences écrites dans la spec** : domaines 09 (`SYS_OBJ_*`,
  descripteurs), 0A (`SYS_THREAD_*`), 0B (`SYS_SIG_*`) et extensions (§6.7) ;
  image liée dans un `.cus` (§3.5) ; C/C++ et Rust par la libc Nova (§13.2).
- **Le sous-système Linux reste** pour les autres programmes (spec §1). Les
  objets qu'il a fait écrire (descripteurs, VFS, futex, sockets, `epoll`,
  espaces privés) sont réemployés par les domaines natifs : la personnalité
  Linux devient une traduction au-dessus d'eux, pas un second exemplaire.
- **Code tiers** : musl (MIT) en espace utilisateur, rattachée à
  l'exception du 22 septembre. Confirmé par l'utilisateur le 23 septembre
  (« pas d'autorisation spéciale ? alors fais-le ») : la licence MIT demande
  seulement que l'avis de copyright accompagne la bibliothèque, d'où
  `/sys/runtimes/c/COPYRIGHT` installé à côté de `libc.so`.

### La libc Nova est aussi l'éditeur de liens : les binaires musl tournent sans être recompilés

Le plan de la phase 12 prévoyait de recompiler Firefox et ses bibliothèques
contre une racine Nova (12.4.3, 12.5, 12.7.2, 12.8.1). Ce n'est pas
nécessaire, et ce n'est pas ce qui a été fait. Le chargeur de musl
(`ldso/dynlink.c`) reconnaît lui-même les noms de sa bibliothèque C
(`libc.so`, `ld-musl-x86_64.so.1`, `libpthread.so.0`, `libm.so.6`...) et y
répond par sa propre image, sans chercher de fichier. Le noyau impose à tout
programme de l'interface Nova l'interpréteur `/sys/runtimes/c/libc.so`, qui
est la libc Nova et son éditeur de liens à la fois : un binaire dynamique
d'Alpine, lié contre la musl d'Alpine, s'exécute sur la libc Nova sans
qu'un octet en soit changé. Firefox, GTK, fontconfig, le greffon ALSA :
tous leurs appels passent par les fonctions de la libc Nova, donc par les
domaines de la spec §6.7.

- **Pourquoi c'est sûr** : l'ABI utilisateur est celle de musl, identique
  (spec §6.7). Les deux musl doivent rester de la même série (1.2.x) :
  un symbole ajouté par une version plus récente manquerait à l'autre.
  Celle de la libc Nova suit celle d'Alpine (`build.sh` vérifie l'empreinte
  contre l'APKBUILD).
- **Limite** : un binaire **statique** embarque sa propre musl et fait des
  appels Linux ; emballé en `.cus`, il reçoit `ENOSYS` à son premier appel
  (`[natif] ... appel 0x.. inconnu - ENOSYS`). Il reste un programme de la
  personnalité Linux.
- **Ce qui reste de la racine Alpine** : des fichiers (bibliothèques
  partagées, polices, `omni.ja`), plus aucun appel. La retirer (12.8.4)
  demande de loger ces fichiers ailleurs, pas de recompiler.

### Une erreur de la libc Nova revient en `-errno`

Les domaines de la spec §6.7 rendent leurs erreurs négatives, dans la
numérotation `errno` de musl, et non en code `SysError` (§6.1). La libc les
pose telles quelles dans `errno`, sans table ; un appel brut d'un programme
(la bibliothèque standard de Rust appelle `syscall(SYS_futex)` elle-même)
reçoit ce qu'il attend. Écrit dans la spec §6.7, « Convention d'appel ».

### Le drapeau de direction appartient au programme

Défaut trouvé le 23 septembre, sous le navigateur natif : deux sessions
complètes sur six perdaient un processus de Firefox (adresses aberrantes
dans libxul, dans `nsTextFrame`, un saut dans une page de `libva-drm`), et
une fois le noyau lui-même (faute dans l'arbre `COW_REFS`). Par la
personnalité Linux, sur le même noyau et le même scénario : six sur six sans
faute.

La cause : `page_fault_entry` (`boot/fault.asm`), point d'entrée écrit à la
main, n'effaçait pas le drapeau de direction. musl copie à rebours dans son
`memmove.s` (`std; rep movsb; cld`) ; une faute prise dans cette fenêtre (la
première écriture d'une page à la demande ou copiée à l'écriture) faisait
tourner le gestionnaire avec DF=1. Ses `memset` et `memcpy` (`rep stos`,
`rep movs`) remontaient alors les adresses : 4 Kio écrits dans la frame
physique **au-dessous** de celle qu'il remplissait -- le tas du noyau, une
table de pages, la page d'un autre processus. Alpine retire `memmove.s` de sa
musl (son APKBUILD) : la personnalité Linux n'exécutait jamais `std`, la libc
Nova, construite sans ce retrait, si.

- **Règle** : tout point d'entrée du noyau écrit en assembleur commence par
  `cld`. Les gestionnaires Rust `extern "x86-interrupt"` le reçoivent du
  compilateur (vérifié dans le binaire), l'entrée `syscall` d'`IA32_FMASK`.
  Le DF du programme revient avec ses RFLAGS à `iretq`.
- **C'était aussi une faille** : n'importe quel programme pouvait écrire une
  page dans la mémoire du noyau par `std` puis un accès à une page absente.
- **Garde** : `tools/ports/tests/memory.c` écrit une page copiée à
  l'écriture et lit des pages fraîches avec DF levé, puis les vérifie. Sur
  l'ancien noyau le test ne se termine pas et entraîne `spawn` et
  `mmstress` dans sa chute ; sur le nouveau : « drapeau de direction
  conformes », en Linux statique comme en natif.
- **La libc Nova suit la recette d'Alpine en entier**, pas seulement ses
  correctifs : `build.sh` retire `memcpy.s` et `memmove.s` comme elle. Les
  programmes de la racine Alpine ont été éprouvés contre cette
  bibliothèque-là.

### Une région retirée ne meurt qu'après l'invalidation des TLB

Défaut trouvé le 23 septembre en cherchant le précédent (même symptôme,
cause distincte, fenêtre bien plus étroite). `Space::unmap` retirait les
régions (`carve`) et les laissait tomber aussitôt, **avant** `clear_range`
et avant `finish_invalidation`. Une région peut tenir la dernière référence
de l'objet qu'elle projette : un memfd dont le descripteur a été fermé après
`mmap`, la mémoire partagée de Firefox. L'objet libérait alors ses frames
pendant que les autres cœurs les atteignaient encore par leur TLB ; une
frame rendue à l'allocateur puis donnée comme table de pages recevait les
écritures d'un fil resté sur l'ancienne traduction. Les régions retirées
vivent désormais jusqu'après l'invalidation.

Même famille : `truncate` de tmpfs libérait les pages coupées d'un fichier
encore projeté (le commentaire parlait de SIGBUS « comme sous Linux » ;
Linux, lui, retire d'abord les pages de toutes les projections). Un fichier
qui a été projeté partagé **retire** ses pages coupées ; elles sont libérées
avec le fichier.

- **Règle** : une frame ne retourne à l'allocateur qu'une fois que plus
  aucun espace ne peut la traduire, TLB compris (invariant ajouté en §4).
- **Diagnostic** : une faute noyau imprime désormais sur la série la tâche,
  les registres et les adresses de retour trouvées sur la pile
  (`[vm] faute noyau`, `[vm]   pile +0x..`), à passer à `addr2line -e
  build/nova-os.bin`. La précédente ne donnait que RIP et CR2.

### Les fichiers du volume se projettent en partage pour de vrai

`mmap(MAP_SHARED, PROT_WRITE)` d'un fichier du volume Nova était refusé
(`EACCES`) : la règle datait de la racine en lecture seule. Or le profil du
navigateur vit dans `/home`, sur le volume, et SQLite en mode WAL projette
son index `-shm` ainsi : toute base **créée** par le profil échouait
(`SQLITE_IOERR_SHMMAP`, code étendu 5386 ; `ExperimentStore` et la base
`54d8354a.sqlite` au démarrage, sous Linux comme en natif). Désormais
(`linux/vfs/novafs/shared.rs`) chaque page projetée partagée et inscriptible
a une frame, remplie depuis le disque ; `write` y recopie, `read` la lit,
`fsync` et `msync` l'écrivent sur le disque.

- **Une vue partagée en lecture seule d'un fichier que personne ne projette
  en écriture reste sur le cache de pages** : sinon chaque archive projetée
  (`omni.ja`, 40 Mio) épinglerait une frame par page en double du cache.
- **Limite** : rien n'écrit ces frames sur le disque de lui-même ; ce qui
  n'est ni `fsync` ni `msync` vit jusqu'à l'arrêt. Les frames ne sont jamais
  rendues (comme celles du cache : le `vm` n'a pas de table inverse).
- **Au passage** : `unlink` oubliait le cache *après* la suppression, par le
  chemin, qui ne nommait plus rien ; le cache d'un nœud supprimé survivait
  et pouvait servir au fichier suivant qui reprenait son numéro. L'oubli se
  fait maintenant par numéro, pris avant la suppression.

### Les objets POSIX sont au noyau, la personnalité Linux ne fait que traduire (23 septembre 2026)

Demande de l'utilisateur : « sortir Firefox de la dépendance Linux et mettre
dans notre noyau les dépendances nécessaires pour que ça fonctionne
nativement sans traduction ». Constat : depuis la phase 12, le navigateur
ne faisait plus aucun appel Linux, mais `syscall::ported` servait chacun de
ses appels par des fonctions de `src/linux/` (`linux::files`,
`linux::syscall::clock_gettime`, `linux::syscall::prctl`...). La décision
précédente disait l'inverse (« la personnalité Linux devient une traduction
au-dessus » des objets) ; le code l'avait rangé à l'envers.

- **Fait** : `src/linux/` devient `src/posix/` (processus, threads,
  descripteurs, fichiers, tubes, sockets, projections, signaux, horloges,
  chargeur, arbre de fichiers, fenêtre, son). `src/linux/` ne garde que
  la table des numéros Linux (`syscall.rs`), `spawn` en `Abi::Linux`, le
  script de démarrage de l'image et l'autotest des programmes Linux
  statiques. Les appels génériques qui vivaient dans `linux::syscall`
  (horloges, sommeil, `prctl`, `membarrier`) sont dans `posix::time` et
  `posix::task`. `TRACE` est à `posix`.
- **Règle** : aucun chemin d'un programme `Abi::Nova` ne traverse
  `crate::linux`. `posix` ne lit `linux` que pour ses compteurs et ses
  noms d'appels (statistiques, rapport de faute d'un programme Linux).
- **`uname`** répond « Nova 12.0 » à un programme porté, « Linux
  6.6.0-nova » à un programme Linux (qui y lit quels appels il peut faire).
- **Plus de repli Linux pour le navigateur** : sans image liée,
  `launch_web_browser` rend `None` et le menu ouvre `navigateur.cus`. Il
  n'y a plus de chemin où le Web Browser passe par `/bin/sh` et la
  personnalité Linux.
- **Ce qui ne relève pas du noyau et reste** : Firefox est une
  construction Linux (`x86_64-linux-musl`, GTK) qui tourne sur la libc
  Nova. `--enable-default-toolkit=cairo-headless` n'existe plus dans
  Firefox 140 (`toolkit/moz.configure` n'offre que les variantes
  `cairo-gtk3*` sur ce système) : l'étape 12.7.1 est impossible telle
  qu'écrite. Retirer GTK demande une boîte à outils `widget/nova`
  complète (fenêtres, presse-papiers, glisser-déposer, apparence,
  sélecteur de fichiers) et une cible Nova dans le système de
  construction de Mozilla et dans les crates Rust : un portage de
  plateforme, pas une étape. Les numéros Linux que la libc Nova reçoit de
  musl et des appels bruts (`syscall(SYS_futex)` de Rust) sont traduits
  **en espace utilisateur** par `nova_syscall.c`, comme la spec §6.7 le
  prévoit ; le noyau n'en voit aucun.

### Une table TCP de seize entrées coupait le réseau de tout le système

Symptôme signalé : « parfois la connexion coupe pour tout l'OS ».
`net::tcp` n'avait que 16 connexions pour tout le système. Firefox garde
jusqu'à six connexions inactives par serveur pendant deux minutes : quatre
sites dans un seul onglet en tenaient 14 (relevé `[tcp] 14 connexion(s)`).
Au-delà, chaque `connect` de chaque programme était refusé (« plus de
connexion disponible ») jusqu'à l'expiration d'une connexion inactive.

- **256 entrées.** Le tampon de réception reste plafonné par
  `TOTAL_BUFFER` (12 Mio pour toute la table) : une connexion inactive ne
  coûte que son entrée.
- **Firefox plafonné à 128 connexions** (`network.http.max-connections`,
  `nova.cfg`) : un programme ne peut plus prendre toute la table.
- **Une référence de connexion porte une génération.** Une connexion
  réinitialisée par l'autre bout (RST) libérait son entrée pendant que le
  programme gardait la référence ; un `connect` suivant reprenait l'entrée,
  et le `close` de l'ancienne socket fermait la connexion de quelqu'un
  d'autre. `Handle` = (entrée, génération) ; une génération périmée se lit
  comme une connexion fermée. L'identifiant rendu au ring 3 (`SYS_NET_*`)
  porte la génération dans ses bits hauts.
- `flush_acks` n'alloue plus un tableau de la taille de la table sur la
  pile noyau (8 Kio à 256 entrées, piles sans page de garde) : groupes de
  32, jusqu'à ce qu'aucun ACK n'attende.

### Le tas du noyau ne porte plus les piles des tâches

Symptôme signalé : un speedtest (speedtest.net) arrêtait le système sur
« ALLOCATION ECHOUEE : 1440 octets, alignement 1 - tache 42 sur le coeur
0 ». Relevé du moniteur : le tas de 64 Mio passait de 7 à 43 Mio **au
lancement du navigateur**, 54 Mio une page ouverte ; le speedtest ne
faisait que franchir la limite. La cause : chaque tâche recevait une pile
de 128 Kio prise dans le tas (`vec![0u8; stack_size]`), et les quelque
trois cents threads des processus de Firefox en prenaient 37 Mio.

- **Les piles sont des frames contiguës** (`frame::allocate_contiguous`,
  `process::KernelStack`), atteintes par la fenêtre physique et rendues
  avec la tâche. Même taille, même absence de page de garde. Mesuré : tas
  à 18 Mio une page ouverte, 23 Mio au plus fort du speedtest, qui va au
  bout (689 Mbit/s en réception, 37 en émission, sous KVM).
- **Les contrôles de fuite de frames** qui tolèrent l'activité des tâches
  système comptent les piles à part (`process::kernel_stack_frames`) : une
  tâche lancée par un autre autotest pendant la mesure prend 32 frames,
  ce qui dépassait la tolérance de 8 de l'autotest de la phase 9 sous TCG.

### Le budget de réception TCP compte la mémoire réservée, pas les octets

Deuxième défaut sur le même chemin. `TOTAL_BUFFER` (12 Mio) comptait les
octets en attente ; la file de réception de chaque connexion doublait sa
capacité en grandissant et la gardait une fois lue. Une dizaine de
connexions à pleine vitesse gardaient chacune 4 à 8 Mio pour quelques
kilo-octets « tenus ». Et le segment hors ordre était copié par une
allocation infaillible (`payload.to_vec()`) : sur un tas plein, c'est elle
qui arrêtait le noyau (1440 octets, la taille d'un segment).

- Le budget compte les **capacités** (plus les segments hors ordre et un
  surcoût par segment) ; la croissance d'une file est plafonnée par ce
  qui reste du budget et par `RECV_BUFFER` ; la fenêtre annoncée compte
  la place déjà réservée dans la file.
- Une file lue rend sa mémoire : entièrement quand elle est vide (au-delà
  de 16 Kio), jusqu'au double de son contenu quand elle est vide aux trois
  quarts. La réduction alloue la petite file avant de rendre la grande, et
  seulement si le tas a la place.
- La copie d'un segment hors ordre est faillible : sans place, le segment
  est abandonné et renvoyé par l'émetteur.

### La table UDP avait le même défaut que la table TCP

Symptôme signalé avec captures : « Hmm. We're having trouble finding that
site » (échec de résolution de nom) et, sur speedtest.net, « every server
in the list failed the latency test », réseau de l'hôte en parfait état.
`net::udp` n'avait que 16 sockets pour tout le système. Chaque résolution
de Firefox en ouvre une (musl interroge par UDP), chaque connexion HTTP/3
(QUIC : Google, YouTube, speedtest.net) en garde une toute sa vie. Relevé
avant correction : 15 sockets ouvertes pendant un speedtest, 14 sur neuf
sites. La dix-septième `bind` échouait, le résolveur recevait
`EADDRINUSE` : plus aucun nom ne se résolvait.

- **256 sockets**, références à génération (comme `tcp::Handle`).
- **Files bornées en octets**, pas en nombre : 256 Kio par socket, 4 Mio
  pour toute la table. Huit datagrammes par socket perdaient une rafale
  QUIC (une socket du speedtest : 69 perdus).
- Copie d'un datagramme faillible ; sur un tas plein il est perdu, comme
  UDP le permet.
- **Une table pleine se dit sur la série** (`[tcp] table pleine`,
  `[udp] table pleine`), après avoir rendu le verrou : vue d'un programme,
  elle ressemble à une coupure du réseau.
- Observé après correction : 18 sockets UDP et 59 connexions TCP ouvertes
  à la fois (deux speedtests, 894 et 754 Mbit/s, puis cinq sites), aucune
  table pleine, aucun datagramme perdu.

### L'émission TCP est asynchrone

Symptôme signalé : téléversement à 18 Mbit/s dans le navigateur (speedtest)
contre 278 sous le Linux de l'hôte, réception identique. `tcp::send`, appelé
par chaque `write` d'une socket, **attendait l'acquittement de tous les
octets** avant de rendre la main, avec 64 Kio en vol au plus : un aller-retour
par écriture, et le fil réseau de Firefox -- qui sert toutes ses connexions
-- bloqué pendant ce temps.

- **Tampon d'émission par connexion** (`SEND_BUFFER`, 1 Mio ; 8 Mio pour la
  table, capacités comptées, croissance faillible). `tcp::write` copie ce
  qui tient et rend la main ; `send_room` dit la place ; `POLLOUT` ne se
  lève que s'il y en a ; un `write` bloquant attend de la place, un non
  bloquant rend `EAGAIN`. `tcp::send` (attente de l'acquittement complet)
  reste pour les clients du noyau.
- **Les acquittements font partir la suite** (`input`, depuis la tâche qui
  a relevé la carte, sans résolution ARP). Fenêtre de congestion : dix
  segments au départ, démarrage lent puis croissance additive, moitié à la
  perte (RFC 5681).
- **Horloge** : `tcp::tick`, à chaque tour de la tâche du pilote réseau (au
  moins chaque tic) : retransmission depuis `snd_una` après `rto` (400 ms,
  doublé à chaque échec, sept échecs = connexion morte), sondage d'une
  fenêtre nulle. Trois acquittements en double renvoient tout de suite.
  L'horloge ne touche qu'aux états qui portent des données : appliquée à
  un `SYN_SENT`, elle ramenait `snd_nxt` au numéro initial et le SYN-ACK du
  serveur, qui acquitte ce numéro + 1, était ignoré (toutes les pages en
  « délai dépassé » au premier essai).
- **`close` n'abandonne plus ce qui reste à envoyer** : l'entrée, orpheline,
  envoie son tampon puis son FIN, et se libère à l'acquittement du FIN ou
  après `LINGER_MS` (10 s). L'ancien `close` envoyait le FIN tout de suite
  et libérait l'entrée.
- **Essayé et retiré** : ne laisser partir les segments que sur
  acquittement (le `write` n'émettant que dans un vol vide), pour décharger
  le fil réseau de Firefox. Mesuré pire sur speedtest.net (131 Mbit/s),
  qui écrit par petits morceaux sur plusieurs connexions.
- **Mesuré** : speedtest.net 215 et 183 Mbit/s en envoi (18 avant) ; vers
  un serveur de l'hôte (page locale qui poste 200 Mio par `fetch`) : 390
  à 695 Mbit/s reçus par le serveur, d'une exécution à l'autre. La pile
  n'est plus la limite ; le reste se partage entre le fil réseau de
  Firefox (trois quarts d'un cœur pendant l'envoi : le corps passe du
  processus de contenu au processus parent par IPC) et le relais réseau
  de QEMU. La comparaison juste serait un Linux invité sous le même QEMU
  (mode `user`), pas le Linux de l'hôte.

### `MAP_FIXED` remplaçait en deux temps : la cause des plantages au chargement

Trouvé le 23 septembre grâce au diagnostic du cache de pages : deuxième
plantage du processus parent de Firefox au démarrage, encore dans
`libnssckbi.so` (pointeur nul lu à `0x7ef8`), et **16 765 pages du cache
vérifiées contre le disque, 0 altérée**. Le contenu des fichiers était
juste ; c'est la projection qui ne l'était pas.

`Space::map(fixed)` appelait `unmap`, rendait le verrou, puis le reprenait
pour insérer la nouvelle région. Entre les deux, la plage était libre : un
autre thread (Firefox en a des dizaines qui projettent au démarrage) qui
faisait `mmap(NULL)` pouvait s'en voir attribuer une partie. Les deux
régions se chevauchaient, et la bibliothèque que le chargeur de musl
plaçait segment par segment (réservation de toute l'étendue, puis
`MAP_FIXED` par segment) lisait la mémoire de l'autre : un `.eh_frame`
absurde la première fois, un pointeur nul la seconde. Linux fait le
remplacement sous un seul `mmap_lock`.

- **Correctif** : retrait et insertion sous une seule prise du verrou ;
  aucune page de la nouvelle région n'est installée avant la fin de
  l'invalidation des TLB (`invalidating`, que `resolve` attend) ; les
  régions retirées tombent après l'invalidation, comme dans `unmap`.
- **C'est l'explication retenue des « certificats tous invalides »** :
  `libnssckbi.so` est le module des autorités racines de NSS, chargé par
  `dlopen` pendant que d'autres threads projettent. Un chevauchement qui ne
  tue pas le processus fait échouer le module, et NSS démarre sans
  autorité racine. Probable, non prouvé : le symptôme lui-même n'a pas été
  reproduit.
- **Même famille probable** : la faute « dans une page de `libva-drm` » de
  la phase 12 (bibliothèque chargée par `dlopen` au premier lecture vidéo).
- Reste ouvert, sans lien : `mremap` avec `MREMAP_FIXED` ne va pas à
  l'adresse demandée (`posix::mm::mremap` rend l'adresse choisie par
  `remap`).

### Le DNS du système est Cloudflare

Décision de l'utilisateur du 23 septembre 2026. `1.1.1.1`, puis `1.0.0.1`
quand le premier ne répond pas (`net::dns_servers`, `dns::resolve`,
`/etc/resolv.conf` des programmes). Le serveur annoncé par DHCP n'est plus
retenu (journalisé : « DNS du bail 10.0.2.3 non retenu »). Un serveur saisi
dans les paramètres réseau gagne sur tout, DHCP compris, et survit au
redémarrage (`dns-choisi=oui` dans `/net/interface.cfg`) ; une ligne `dns=`
sans cette marque est celle d'un ancien bail et reste ignorée.

### Une commande AHCI qui expire est arrêtée avant d'être abandonnée

`ahci::issue` rendait `Timeout` alors que la commande était encore entre
les mains du contrôleur. L'appelant (le cache de pages de `novafs`) rendait
alors ses frames à l'allocateur, et le DMA en retard pouvait écrire un bloc
du disque dans la mémoire de quelqu'un d'autre. Désormais, une expiration
ou une erreur arrête le port (ST, puis attente de CR : plus aucun DMA en
vol), le relance, et le journalise (`[ahci] port N : commande ... 
abandonnee`). Un refus de lecture du cache est lui aussi journalisé
(`[novafs] lecture de N page(s) ... refusee`). Aucune expiration n'a été
observée : c'est une faille fermée, pas la cause établie d'un symptôme.

### Diagnostic : les pages du cache sont vérifiées à chaque faute fatale

Une session native sur quatre, le 23 septembre, a perdu le processus
parent de Firefox au démarrage : `libgcc_s` (`__register_frame_info`,
appelé par l'initialisation de `libnssckbi.so`, le module des autorités
racines de NSS) lisait dans `.eh_frame` un décalage de CIE absurde, alors
que ces octets viennent du fichier tel quel et que le fichier est intact
sur le disque. Même famille que la faute « dans une page de `libva-drm` »
de la phase 12. **C'est aussi la cause la plus probable des « certificats
tous invalides »** signalés : NSS qui échoue à charger ce module sans
tuer le processus démarre sans autorités racines, et tout certificat est
alors d'un émetteur inconnu.

`Space::verify_cached_pages` compare, à chaque faute fatale d'un programme
(faute de page ou autre exception), chaque frame `CACHED` qu'il projette
avec le fichier relu sur le disque, sans passer par le cache
(`Pager::fill_uncached`) : `[vm]   page du cache ALTEREE : "lib" +off ...`
puis un total. Une page altérée prouve qu'une frame du cache a été écrite
ou donnée à quelqu'un d'autre ; aucune, que la cause est ailleurs
(chargeur, relocations, copie sur écriture).

## Pile graphique et GPU (phase 13) — décisions

### La pile graphique est à Nova (23 septembre 2026)

**Décisions de l'utilisateur**, en réponse à la première roadmap GPU (qui
proposait Mesa et l'interface DRM de Linux) :
- « on va éviter d'utiliser des drivers Linux ou autre, il faut faire notre
  propre driver » : **ni Mesa, ni libdrm, ni l'interface DRM**, ni pilote
  tiers ;
- « le pilote sera par défaut intégré au noyau » ;
- « pour les drivers dédiés, les entreprises se débrouilleront ou les
  utilisateurs » : Nova fournit l'interface de backend, pas les pilotes de
  cartes réelles ;
- « une pile interne dédiée, rapide et sûre, pas un truc qui existe déjà ».

Ce qui en découle (roadmap, phase 13) :
- **Le protocole du périphérique n'est pas une pile.** Sous QEMU, parler à
  la carte de l'hôte passe par virtio-gpu 3D, dont l'hôte impose le format
  des commandes (Venus ou virgl). Nova écrit l'encodeur ; le parler est
  aussi légitime que de parler TCP. Proposé : Venus (Vulkan sérialisé,
  shaders SPIR-V), encodeur généré depuis le registre public `vk.xml` par
  un outil Nova. À confirmer en 13.1.
- **Firefox devient le point dur** *(voie remplacée par la précision
  ci-dessous : backend Nova dans le fork)* : il ne dessine sur GPU qu'en OpenGL ES
  par EGL. Nova fournira ses propres `libEGL.so` / `libGLESv2.so`, limitées
  à ce que WebRender appelle, avec les shaders de WebRender compilés à la
  construction de l'image (outil hôte). Pas de compilateur GLSL dans Nova :
  WebGL reste sans GPU tant qu'il n'y en a pas.
- **VA-API est exclu** (interface Linux) : le décodage vidéo reste logiciel
  dans cette phase.
- **Spec §1.2** (pilotes hors du noyau) : à amender en 13.1 pour le pilote
  GPU, avec le statut des backends de fabricants.
- **Précision de l'utilisateur** (même soir) : « la difficulté principale va
  être de modifier Firefox pour qu'il prenne notre pile graphique, c'est
  aussi pour ce genre de cas qu'on a forké Firefox » ; « le but est de
  faire un OS entièrement à nous sans dépendre de Linux ou de drivers
  propriétaires ». **Remplace l'idée d'une `libEGL`/`libGLESv2` Nova** : le
  fork reçoit un backend de périphérique Nova dans WebRender (à côté de
  `device/gl.rs`), ses 50 shaders compilés en SPIR-V à la construction, et
  un `RenderCompositorNova` côté Gecko. Nova n'imite pas OpenGL.
- **Protocole : Venus** (décision de l'utilisateur, 24 septembre 2026,
  après explication des deux protocoles). virgl écarté : format OpenGL plus
  ancien et plus lent, shaders TGSI peu documentés.
- **Pilotes de fabricants : modules signés chargés au démarrage**
  (décision de l'utilisateur, 24 septembre 2026). Le backend virtio-gpu
  reste intégré au noyau ; un backend tiers est un module dont la signature
  est vérifiée avant exécution (spec §3.2), refusé et journalisé sinon.
- La première version de la roadmap (Mesa, DRM, VA-API) est remplacée ; ce
  qu'elle avait vérifié sur l'hôte (QEMU 11.1.1, virglrenderer 1.3.0 avec
  Venus, `virtio-vga-gl,hostmem=8G,blob=true,venus=true` accepté, pilote 2D
  de Nova fonctionnel dessus, `renderD129` = RTX) reste valable.

### La spécification de la phase 13 est acceptée (24 septembre 2026)

L'utilisateur a demandé la mise en place complète des pilotes graphiques
(« déjà on va faire notre propre driver graphique comme tu l'as noté hier,
met en place complètement ces drivers graphiques, ce n'est que plus tard
qu'on adaptera firefox ») après avoir reçu la spec §1.2, §3.6, §6.8, §8.5.
Elle est retirée de l'état « à relire ». Les deux points qu'elle laissait
« à confirmer » ont reçu la valeur recommandée par l'assistant, que
l'utilisateur n'a pas contredite — **à confirmer explicitement** :

- **modules SELF** : acceptés si un administrateur a inscrit l'empreinte
  exacte du module dans `/sys/config/modules-autorises`, avec un
  avertissement à chaque démarrage ;
- **délai de garde** : 2 secondes par soumission (la valeur du TDR de
  Windows).

Ce que la spec laissait ouvert a été fixé et écrit dans le §6.8 : les
structures, le format du tampon de commandes (19 opcodes), les erreurs en
`-errno`, les limites par processus (2 Gio, 16 contextes, 16 384 poignées,
16 soumissions en vol) ; dans le §3.6 : le conteneur `.kmd` et la charge
utile ELF ; dans le §8.5 : la version de l'interface de backend.

### Un contexte GPU, un périphérique Vulkan sur l'hôte

Chaque contexte d'un programme ouvre son propre contexte virtio-gpu, donc
sa propre instance et son propre `VkDevice` sur l'hôte (Venus lance un
processus de rendu par contexte). Coût : environ 80 ms à l'ouverture, et
la mémoire d'un device par contexte. Gain : un shader qui boucle ou un
flux que l'hôte ne décode pas perd **ce** contexte, et pas l'écran ni les
autres programmes — ce que le §6.8 promet. Le partage d'images entre
contextes passe alors par la mémoire exportée en dma-buf (`SYS_GPU_SHARE`).

### Les réponses Venus passent par la mémoire partagée, le travail par la timeline

Deux chemins, pour deux besoins :

- **ce qui répond** (un `VkResult`, des besoins mémoire) : le flux de
  réponse est un blob de mémoire partagée (`blob_id` 0, un memfd sur
  l'hôte), projeté dans la vue noyau ; le lot est soumis avec une barrière
  sur l'anneau 0 (le processeur de l'hôte), levée quand le flux est
  décodé ;
- **le travail** : la file Vulkan du contexte est liée à l'anneau 1
  (`VkDeviceQueueTimelineInfoMESA`, que le rendu Venus exige de toute
  façon) ; la soumission porte une barrière sur cet anneau, et QEMU ne
  rend la requête que lorsque le GPU a fini. Pas de `VkFence`, pas de
  scrutation de l'hôte : la fin du travail arrive comme une interruption.

### L'encodeur Venus est généré, et comparé à la référence octet par octet

`tools/gen-venus.py` lit le registre Vulkan (`vk.xml` et les deux XML de
venus-protocol, au commit épinglé, sommes vérifiées) et écrit l'encodeur,
les décodeurs de réponse et les constantes des énumérations, pour les 65
commandes que Nova utilise et rien d'autre. `tools/venus-check/` construit
les mêmes arguments en C pour l'encodeur de référence de venus-protocol et
en Rust pour celui de Nova, et compare les octets : identiques sur les 65
commandes, chaînes pNext complètes comprises. Deux règles du protocole que
la spécification de VK_EXT_command_serialization ne dit pas et que seule
la comparaison a montrées : une chaîne pNext s'encode en tête d'abord
(marqueur, sType, reste de la chaîne, puis le corps), et un scalaire de
sortie n'est pas envoyé du tout — seulement son pointeur.

### Le noyau choisit pour le programme ce que Vulkan lui laisserait

La traduction fixe une fois pour toutes : toutes les images en
`VK_IMAGE_LAYOUT_GENERAL` dès leur premier usage, une barrière mémoire
complète avant chaque passe et chaque copie (et vers l'hôte à la fin),
les ressources poussées (`vkCmdPushDescriptorSet`), le rendu dynamique,
`robustBufferAccess` et `robustImageAccess` activés. Un programme ne nomme
ni état d'image ni barrière : il ne peut donc pas en écrire une fausse. Le
coût, sur un GPU actuel, est faible devant celui d'une erreur de
synchronisation (un résultat indéfini, pas une erreur signalée).

### Un blob de mémoire locale au GPU s'exporte « partageable », jamais « projetable »

Relevé à l'essai du partage d'images : le rendu Venus refuse un blob
`MAPPABLE` sur une mémoire qui n'est pas visible de l'hôte (« mem cannot
support mappable blob »). L'export pour le partage (`CTX_ATTACH_RESOURCE`
d'un autre contexte) se fait avec `SHAREABLE` seul ; la projection dans la
région partagée, avec `MAPPABLE` seul, sur une mémoire visible.

### Une réponse ne se recopie que sur la taille qu'elle peut avoir

La première version recopiait les 256 Kio du flux de réponse dans le tas
noyau à chaque appel. Le tas grossissait de 64 pages à la première session,
ce que l'autotest de la phase 5 a vu (« un second cycle de lancement ne
coûte aucun cadre »). `Session::call` recopie 4 Kio ; les deux énumérations
longues (cartes, extensions) demandent la taille qu'il leur faut.

### Le GPU se met en route après les autotests du démarrage

`gpu::start` lance la tâche `drv-gpu` avec les pilotes ; la tâche met le
backend en route une fois les autotests terminés. Raison : plusieurs
autotests comptent les cadres libres de toute la machine autour de ce
qu'ils font. La sonde du GPU attend l'hôte une centaine de millisecondes ;
faite dans `init.dsa`, elle décalait les allocations des services lancés
après elle dans la fenêtre de mesure de la phase 5, qui échouait en
monocœur. Rien n'a besoin du GPU avant l'ouverture de session, et l'écran
de démarrage attend déjà la fin des autotests.

### Une adresse noyau se lit négative : une erreur est -1 à -4095

`SYS_GPU_MEM_MAP` rend une adresse, et une adresse de la moitié haute
(la vue noyau, pour un appel du noyau) est négative en `i64`. Comme sous
Linux, seules les valeurs -1 à -4095 sont des erreurs ; l'autotest l'a
appris en prenant une projection réussie pour un échec.

### Les pilotes de fabricants ne voient qu'une table

Un module `.kmd` est un ELF indépendant de la position ; aucun symbole du
noyau n'est résolu en lui. Il reçoit `struct nova_kmd_kernel` (journal,
mémoire, pages contiguës, registres, PCI, temps) et rend `struct
nova_kmd_gpu`, en miroir exact de `backend::Context`. Le chargeur refuse,
avant toute instruction du module : une autre version d'interface, une
empreinte fausse, une signature non reconnue, une bibliothèque demandée,
une relocation autre que `R_X86_64_RELATIVE`, une page à la fois
modifiable et exécutable. Le module d'essai (`tools/kmd/test-gpu.c`) ne
trouve pas de carte ; derrière, un « GPU » fait de processeur et de
mémoire permet à l'autotest de traverser l'interface pour de vrai
(mémoire, tampons, remplissage, mise à jour, copie, relus). Les tailles et
les décalages des structures C sont comparés à ceux du Rust.

**Limite** : le cœur ne pilote qu'un périphérique. Un module n'est utilisé
que si le pilote intégré n'a pas trouvé le sien.

### QEMU : `virtio-vga-gl` avec Venus par défaut

`QEMU_GPU` vaut `-device virtio-vga-gl,hostmem=8G,blob=true,venus=true`
quand l'hôte a un nœud de rendu ; `QEMU_GPU=` redonne la carte 2D. Deux
conséquences : l'affichage doit être OpenGL (`gtk,gl=on` avec fenêtre,
`egl-headless` sans : `-display none` n'est plus possible avec cette
carte, d'où `QEMU_HEADLESS` et `make print-qemu-headless` pour les
scripts) ; et le nœud de rendu d'`egl-headless` est choisi par chemin PCI,
la carte NVIDIA d'abord (`renderD128` est la Radeon intégrée sur cet
hôte). Le GPU que Nova utilise, il le choisit lui-même parmi ceux que
l'hôte expose : le dédié d'abord.

### Les artefacts générés sont versionnés

`src/gpu/venus/protocol.rs` (`make venus`), `src/gpu/shaders/*.spv`
(`make shaders`), `src/gpu/module/test-gpu.kmd` (`make kmd-test`) : comme
`src/graphics/font.rs`, ils sont dans le dépôt, et la construction du
noyau n'a besoin ni du réseau, ni de Python, ni de glslang, ni de gcc.

### La sortie d'affichage directe par dma-buf est retirée : noir sur l'hôte NVIDIA (24 septembre 2026)

Essai de 13.8 : l'écran pris directement dans un blob Venus
(`SET_SCANOUT_BLOB`). QEMU accepte les commandes, mais l'hôte affiche du
**noir**. La capture VNC le montre ; les pixels côté invité sont pourtant
justes (00101218 écrit par le CPU, puis 00ff0000 rempli par le GPU). Le
résultat est le même en mémoire système, en VRAM par ReBAR, et avec
l'ancienne texture 2D gardée en vie.
Cause : QEMU 11 importe le dma-buf avec `DRM_FORMAT_MOD_INVALID`, et le
pilote EGL de NVIDIA affiche alors du noir. Il n'y a rien à corriger côté
Nova. Le code d'affichage (`show_blob`, `flush_blob`, `gpu/display.rs`)
a été **retiré** plutôt que laissé mort ; l'écran reste sur le chemin 2D
(`RESOURCE_FLUSH`).
À réessayer sur un hôte Mesa (AMD, Intel), ou quand QEMU passera le
modificateur.
Ce qui a été gardé : la mémoire « visible » d'un contexte prend d'abord
un type `DEVICE_LOCAL | HOST_VISIBLE` (ReBAR) quand l'hôte en a un.
L'écriture par le CPU coûte le même prix, et la lecture par le GPU est
locale.

### `PRESENT` passe par une relecture, et lit la mémoire du programme comme un appel système

`SYS_GPU_PRESENT` fait copier l'image par le GPU dans une zone de relecture
du contexte (`present::Staging` : mémoire, tampon, barrière, faits au
premier appel, gardés, refaits si la fenêtre grandit, comptés au quota du
programme), attend cette copie, puis recopie les lignes dans la fenêtre. C'est une copie de trop par
rapport à 13.8, mais c'est le seul chemin qui marche partout.
Leçon : la première version lisait la mémoire du programme par un
pointeur direct. Une page projetée mais jamais touchée n'a pas de cadre,
et le noyau a fauté (`EXCEPTION CPU #14` à 0x7efffff1f000). Toute lecture
de mémoire d'un programme passe par `vm::copy_from_user`, qui résout la
faute comme le ferait le programme, **même quand l'adresse vient d'un
objet que le noyau a lui-même projeté**.

### Capture d'écran : pas de `screendump` avec le GPU 3D

Avec un affichage GL, `screendump` (QMP) répond « no surface ».
`tools/ports/test-window.sh` lance donc QEMU sans GPU 3D (`QEMU_GPU=`,
`-display none`), et `tools/ports/vnc-shot.py` capture par VNC (RFB)
quand le GPU doit être là.

### Le compositeur compose sur le GPU, avec le même peintre (13.8, 24 septembre 2026)

Le peintre (`gui::painter`) écrit sur une **cible** (`Target`) : le
framebuffer, une image en mémoire, ou l'enregistreur du GPU
(`gui::gpu::record`), qui produit une instance par primitive au lieu de
pixels. Découpe, translation et ordre restent dans le peintre, écrits une
fois : c'est ce qui rend la composition du GPU **égale au pixel près** à
celle du processeur, et l'autotest le vérifie (scène complète, zone seule,
transformations). Un seul pipeline : rectangles, glyphes (atlas R8), icônes
(atlas R8, cache par leurs bits), toiles (une image par toile) ; coordonnées
entières, `texelFetch`, couleurs `c / 255` écrites en 8 bits — aucune
interpolation, aucun arrondi.

La sortie n'est pas un affichage direct (noir sur cet hôte, entrée
précédente) mais une **ressource 2D dont le support est une mémoire Venus**
de la région partagée : le GPU y copie la zone endommagée, `TRANSFER_TO_
HOST_2D` et `RESOURCE_FLUSH` la montrent. Aucun pixel ne passe par le
processeur de l'invité. Mémoire « relecture » (système, cachée) : l'hôte la
lit à chaque transfert, et la VRAM par le BAR se lit très lentement.

**13.8.2 tranché** : la loupe et les filtres de couleur restent sur le
processeur, lisant l'image composée par le GPU (`back`). Leurs formules sont
entières et éprouvées ; un shader les referait en flottants, avec ses
propres arrondis. Le coût ne touche que qui active une transformation.

Le compositeur est un client comme un autre du cœur GPU (propriétaire
noyau, mêmes validations) ; seul un propriétaire noyau peut mettre une
mémoire à l'écran (`gpu::screen`).

### Le pointeur sur le plan curseur (13.4.4)

La surface de la couche `Overlay` (le pointeur de `cursor.mod`) n'est plus
composée quand le périphérique a une file curseur et qu'aucune
transformation n'est active (la loupe doit agrandir le pointeur) : elle est
peinte dans une image 64×64 avec alpha à chaque changement de forme,
montrée par `UPDATE_CURSOR`, déplacée par `MOVE_CURSOR`. Bouger la souris ne
salit plus l'écran. **Deux ressources alternées** : l'image suivante est
transférée pendant que l'autre est montrée, et ne passe sur le plan qu'une
fois la réponse du transfert reçue (`cursor_poll`) — sans attente, la file
curseur étant servie à part de la file de contrôle.
Leçon : l'autotest vérifiait l'intention du compositeur (`cursor_plane`),
et l'image n'était jamais montrée (`cursor_poll` jamais appelé) ; il
vérifie maintenant ce que l'hôte a accepté (`cursor_shown`), et la capture
VNC demande le pseudo-encodage curseur.

### La fin du travail GPU se lit dans un mot que le GPU écrit en dernier

QEMU relève les barrières de virglrenderer sur une minuterie de **10 ms** :
une barrière virtio annonce la fin d'un travail jusqu'à une image trop tard
(le triangle « rendu en 10 ms » n'en prenait pas une). Chaque tampon de
commandes finit donc par un `vkCmdFillBuffer` du numéro de son travail dans
un mot de mémoire hôte (relecture, cohérente) que le noyau lit
(`venus::backend::feedback`). `SYS_GPU_FENCE_WAIT` regarde d'abord 3 ms
en scrutant ce mot (entre deux cessions du cœur), puis dort. Le tampon de
commandes n'est réutilisé qu'une fois sûrement sorti de l'état « en
attente » : la réponse virtio arrivée, ou, par l'anneau, le travail suivant
terminé (la file s'exécute dans l'ordre).

### Les lots Venus passent par un anneau (13.5.2)

Chaque session ouvre un anneau (`vkCreateRingMESA`) : 4 Kio de contrôle
(tête, queue, état, extra, chacun sur sa ligne de cache) et 1 Mio de tampon,
dans un second blob de mémoire partagée. Le noyau écrit le lot et publie la
queue ; un fil du moteur de rendu de l'hôte le décode et avance la tête
après chaque commande ; un appel attend que la tête le dépasse, puis lit ses
réponses (l'anneau a son propre réglage du flux de réponses). Le fil
s'endort après **20 ms** sans travail (le compositeur soumet toutes les
10 ms : il reste éveillé) et le dit dans l'état ; le lot suivant le réveille
par `vkNotifyRingMESA` sur la file de contrôle. Un anneau refusé laisse la
session sur la file de contrôle. Un travail sans mot de fin (création
impossible) repasse par la file de contrôle avec sa barrière, une fois
l'anneau vidé — il désigne des objets que l'anneau a créés.
Intérêt : aucune requête virtio par lot, et surtout aucune dépendance à la
boucle principale de QEMU, qu'un programme tenant le GPU de l'hôte peut
bloquer des secondes (entrée suivante). Protocole lu dans virglrenderer
(`vkr_ring.c`, `vkr_transport.c`), pas repris.

### Un programme qui accapare le GPU de l'hôte : Nova ne doit rien attendre de QEMU (13.11.3)

Le GPU de l'hôte ne préempte pas un long dessin d'un autre contexte, et
QEMU dessine ses ressources 2D en OpenGL sur ce même GPU : pendant qu'un
programme le tient, le fil principal de QEMU reste coincé dans un appel
OpenGL, **avec son grand verrou**. Tout vCPU qui touche alors un
périphérique émulé par QEMU (configuration virtio, port série, disque)
attend avec lui. L'essai `essai-gpu=garde` l'a montré en quatre temps :
1. le compositeur attendait son image 2 s, puis abandonnait le GPU pour de
   bon — **attente d'une image bornée à 100 ms**, au-delà le processeur
   peint dans la sortie (qui reste à l'écran) et le GPU est repris seul ;
2. des commandes attendues **verrou tenu, interruptions masquées**
   déclaraient le périphérique mort — à l'exécution, les requêtes sont
   postées ou attendues **verrou relâché** (`virtio_gpu::exchange`, borne
   10 s) ; les destructions sont postées (la file de contrôle s'exécute
   dans l'ordre) ; seuls le démarrage et `show` gardent l'attente active ;
3. `poll_resize` lisait la configuration virtio **sous le verrou** :
   bloqué dans QEMU, ce vCPU gelait les autres qui attendaient le verrou,
   horloge comprise — la lecture se fait sans verrou, et pas du tout
   pendant une suspension ;
4. le délai de garde comptait en **tics**, arrêtés ; il compte au **TSC**.
Résultat : contexte fautif perdu à 2,007 s, aucun message IPC perdu (15 872
avant), reprise automatique. Ce que Nova ne peut pas faire : préempter le
GPU de l'hôte, ni garder l'affichage de la VM vivant pendant ce temps (QEMU
en a besoin). Règle pour la suite : **pendant l'exécution, aucun chemin
fréquent ne touche le périphérique en l'attendant**, et jamais sous un
verrou tournant.

### `SleepLock::try_lock` libérait le verrou d'un autre

`self.try_take().then_some(SleepGuard { lock: self })` : `then_some` évalue
son argument même quand la prise échoue, et la garde construite pour rien
était détruite aussitôt — son `Drop` **ouvrait le verrou tenu par une autre
tâche**. Deux tâches travaillaient alors dans le même contexte GPU (une
`BTreeMap` du backend corrompue, trouvée grâce à la pile d'appels). Le
défaut datait du début de la phase et ne se montrait pas tant que le
verrou était peu disputé : la scrutation du mot de fin l'a révélé. Règle :
**jamais de `then_some` avec une valeur à effet de bord** (garde, `Drop`) ;
`then(|| ...)` ou un `if`.

### `vkCmdPushDescriptorSet` demande l'extension sous une instance 1.3

La version d'un périphérique est la plus petite de la carte et de
l'instance ; l'instance demande 1.3, donc la commande 1.4 n'existe que par
`VK_KHR_push_descriptor` — même sur une carte 1.4. Sans elle, l'hôte n'a
aucune fonction derrière la commande et **abandonne le reste du flux** :
aucune erreur, la copie suivante n'est jamais faite. Aucun test n'avait
encore lié une image : le compositeur l'a trouvé.

### Les shaders se compilent en SPIR-V 1.5

En 1.6, glslang traduit `discard` par `OpDemoteToHelperInvocation`, qui
demande une capacité que la validation de Nova refuse (spec 6.8). En 1.5
(`--target-env vulkan1.2`), c'est `OpKill`, sans capacité.

### La pile d'appels à la panique : `make PILE=1`

Avec `PILE=1`, le noyau est construit avec les pointeurs de cadre et
`--cfg nova_pile`, et le gestionnaire de panique imprime les adresses de
retour (`llvm-addr2line -f -C -i -e build/nova-os.bin <adresse>`). La
panique donne aussi la tâche fautive, toujours. Sans l'option, rien ne
suit `rbp` : ce ne serait qu'un registre ordinaire.

### La bibliothèque Rust `nova-gfx` (13.6.3)

`tools/ports/nova-gfx/rust` : `core` seulement (`no_std`), un `syscall` par
appel comme l'en-tête C, poignées typées libérées par `Drop`, erreurs en
énumération, tampon de commandes écrit dans une tranche du programme. Les
descriptions sont posées mot par mot, comme le noyau les lit. Construite
par le Rust d'Alpine (cible musl, lien dynamique : la libc Nova à
l'exécution), dans une copie de son arbre hors du dépôt — sinon cargo
prendrait la configuration du noyau (`.cargo/config.toml`).

---

## Système de fichiers LIFS et partitions (phase 14) — décisions

### Réponses de l'utilisateur (24 septembre 2026)

- **La racine est en LIFS dès maintenant** (et non NovaFS d'abord) : le
  disque système est formaté en LIFS par `make disk-reset` ; NovaFS reste
  monté pour les volumes existants.
- **Une lettre par partition** montée (spec 4.2).
- **Cohérence par journal** (et non métadonnées en copie-sur-écriture).
- **Trois masques de permissions** (propriétaire, groupe, autres), les
  treize bits du §5.1.

Les autres points relevés en 14.1 prennent la recommandation de la
roadmap, faute d'avis contraire (l'utilisateur a demandé de les
appliquer) :
1-3. **Le clone CoW est un nouveau nœud** qui partage les **étendues** de
   l'original, avec un **compte de références par étendue** ; toute
   écriture dans une étendue partagée — par l'un ou l'autre — la copie.
   Chaque nœud a ses métadonnées : pas de séparation à faire au `chmod`.
   C'est un écart assumé au texte proposé (un `cow_ref_count` sur le même
   nœud), qui ne dirait pas quels blocs sont partagés.
4. **Toute entrée de dossier est un lien Tag** (le lien d'origine aussi) ;
   la copie n'est pas une sorte de lien mais une opération.
6. Adressage par **étendues**, non par pointeurs directs.
7. **Index inverse** nœud → dossiers, tenu à chaque lien créé ou retiré.
9. Les appels par identifiant vérifient les droits comme par chemin
   (spec 6.2).
10. Entre deux volumes, la copie copie les octets (dit dans la spec).
11. Pas de `bitflags` : masques écrits à la main.
12. Les blocs partagés sont comptés au propriétaire de chaque nœud qui
   les désigne (simple, surestime) ; un disque plein refuse une écriture
   dans un bloc partagé comme toute écriture.
13. Recherche par empreinte **puis comparaison du nom**.
- Octet MBR : `0x4C` (« L »), confirmé par la signature du superbloc.
- Racine désignée par `racine=<GUID>` sur la ligne de commande, sinon la
  partition LIFS marquée « système ».

### Nova ne formate plus rien de lui-même (14.0.1)

Jusqu'à la phase 14, `Volume::mount_or_format` formatait le disque de
démarrage s'il ne portait pas NovaFS : sur une machine réelle, un disque
Windows ou Linux branché le premier aurait été **effacé au premier
démarrage**. Désormais un support non reconnu est signalé et laissé
intact, quel que soit son rang ; formater est un acte explicite (l'outil
de l'hôte `tools/lifs.py`, `make disk-reset`). Seul un volume **en
mémoire** (aucun disque) est formaté par le noyau : il est le sien.

### Choix de mise en œuvre (24 septembre 2026)

- **Disque système par défaut** : GPT, p1 LIFS « nova-systeme » (drapeau
  système, la racine), p2 LIFS « nova-donnees » de 256 Mio (`A:/`), créés
  par `tools/lifs.py create` (`make disk`). La p2 n'est pas demandée par la
  spec : elle éprouve à chaque démarrage « une lettre par partition » et la
  copie entre volumes. `DATA_SIZE=0` la supprime.
- **Validation groupée** toutes les 2 s (tâche `fs-validation`), sur
  `flush`, et quand la transaction dépasse la moitié du journal — au début
  d'une opération seulement, jamais au milieu (voir plus bas).
- **Blocs libres annoncés** (`info`, `SYS_FS_DISK_LIST`) : les blocs libérés
  dans la transaction ouverte y sont comptés — ils le sont au plus tard dans
  deux secondes. Le superbloc, lui, ne les compte qu'après la validation.
- **Copie dans un volume LIFS = clone** (`SYS_FS_COPY`, `fs::copy`) ; entre
  deux volumes, copie des octets comme avant.
- **Appels par identifiant et bac à sable** : le §12.4 raisonne en chemins ;
  le chemin de chaque nœud nommé est reconstruit par l'index inverse (premier
  Tag à chaque niveau) et passé à la même `guard` que les chemins. Sans cela
  un identifiant sortirait du bac à sable.
- Le **journal ne couvre que les métadonnées** ; les données vont au cache
  directement (mode « ordonné » : écrites avant la validation qui les
  désigne). Un bloc libéré attend la validation avant de resservir.

### Bugs évités ou corrigés pendant la mise en œuvre — à ne pas réintroduire

- **Le superbloc s'écrit en dernier.** Les images d'une transaction étaient
  recopiées dans l'ordre des numéros de bloc, donc le superbloc (bloc 0) en
  premier : une coupure juste après lui laissait `journal_seq = n` avec des
  blocs de `n` absents, et le rejeu (qui cherche `n + 1`) ne se faisait pas.
  Désormais : les autres blocs, flush, puis le superbloc, flush — à la
  validation comme au rejeu (`journal::write_homes`).
- **Pas de validation au milieu d'une opération.** `release_content`
  validait quand le journal se remplissait : l'enregistrement sur disque
  désignait encore les étendues qu'on venait de rendre libres. Une coupure
  après cette validation aurait laissé un fichier pointant sur des blocs
  réalloués. `commit_if_full` n'est appelé qu'au début des opérations.
- **L'arbre B+ fusionne au retrait.** Sans fusion, un dossier (ou l'arbre
  des nœuds) qui grandit puis rétrécit garde les blocs de ses coupures : le
  compte de blocs libres ne revenait pas à sa valeur (autotest de
  l'explorateur, SMP1 et SMP8). `merge_neighbours` réunit un enfant et son
  voisin quand ils tiennent dans un bloc ; contrôlé par « 300 fichiers
  créés puis retirés ».
- **Un secteur d'amorçage n'est pas un MBR.** FAT et NTFS finissent aussi
  par `0x55AA` : un MBR n'est cru que si chaque entrée est vide ou saine
  (état 0x00/0x80, premier secteur non nul, étendue dans le disque). Sinon
  un support d'export devenait un « disque partitionné » laissé intact.
- **`-boot d` dans QEMU.** Le MBR protecteur d'une GPT porte `0x55AA` : sans
  ordre de démarrage, le BIOS tentait d'amorcer le disque système (aucun
  code) et rien ne s'affichait, pas même GRUB.


## Infrastructure : version, tests automatiques, publication (28 octobre 2026)

Demande de l'utilisateur : un README complet, une publication automatique
qui construit le projet à chaque changement de version et crée une
release, des commits et des releases réguliers, en autonomie.

### La version vit dans `Cargo.toml`, et nulle part ailleurs

`src/version.rs` lit `CARGO_PKG_VERSION` à la compilation : la bannière
(`Nova OS 0.14.0 (a1b2c3d) booted`), `uname` d'un programme porté et
`spm::update::OS_VERSION` en dérivent. SPM portait sa propre constante
(`0.8.0.1`) que rien ne tenait à jour ; or c'est elle qui décide si un
dépôt propose « mieux ». Le commit vient du Makefile (`NOVA_COMMIT`),
`local` hors d'un dépôt Git. Schéma : `0.{phase}.{correctif}`.

### La toolchain est épinglée

`rust-toolchain.toml` nomme une nightly datée (`nightly-2026-10-28`) et non
plus `nightly`. Le noyau dépend de fonctions instables (`build-std`, cible
JSON, `abi_x86_interrupt`) ; une release construite par la CI avec une
nightly différente de celle du développeur ne prouverait rien. Changer la
date est un changement comme un autre : il passe par `make test-all`.

La seule ligne `warning` tolérée par la CI est l'avis de la toolchain sur
`core` (« enabling the `sse` target feature ... » dans `stdarch`) : c'est
le code de la bibliothèque standard recompilée par `build-std` pour une
cible soft-float, pas le nôtre, et il n'a pas d'effet sur le binaire
(`make check` : aucune instruction SSE hors de `fpu::worker`).

### `make test` juge la sortie série, et s'arrête au dernier verdict

`tools/boot-test.py` lit la série au fil de l'eau, arrête QEMU dès que
tous les verdicts attendus sont imprimés, échoue à la première ligne
`ECHEC`, `KERNEL PANIC`, `BOOT_ERR_` ou `ALLOCATION ECHOUEE`. C'est la
validation « par l'assistant » de CLAUDE.md (2.7) rendue répétable : le
journal complet reste dans `build/test/`. Le disque est le sien
(`build/test/`), jamais `disks/` ; deux démarrages sur le même disque,
le second doit imprimer `demarrage n 2`. 11.5 et 12 acceptent « non
éprouvée » : leurs images (racine Linux, programmes natifs) ne sont pas
dans l'ISO.

### Un autotest qui lit un compteur global doit tolérer le bruit des autres

Le premier passage de `make test` en boucle a fait sortir trois faux
échecs, tous de la même famille : un autotest lit un compteur **de toute
la machine** (blocs libres du volume, cadres libres) avant et après une
opération, pendant que les autres autotests du démarrage allouent sur
les autres cœurs.
- « la table d'indirection est rendue avec les blocs » (phase 4) échouait
  au second démarrage d'un disque neuf : sur LIFS, créer le fichier
  insère un nœud et une entrée, ce qui peut couper une feuille d'arbre B+
  et prendre un bloc pour de bon — croissance du dossier, pas fuite du
  fichier. Le fichier est désormais créé court **avant** la première
  lecture du compteur.
- « un second cycle de lancement ne coûte aucun cadre » et « les cadres
  pris par l'arène sont tous rendus » (phase 5) : bruit des autres tâches.

Règle retenue : **une fuite coûte à chaque fois, le bruit non.** Le cycle
mesuré est rejoué jusqu'à trois fois (`frame::neutral_cycle`), et
l'invariant tient si un passage ne coûte rien. Les vérifications d'un
passage rejoué ne comptent que si elles échouent : le nombre de
vérifications d'un verdict ne dépend pas du bruit.

### La release démarre l'ISO qu'elle publie

`.github/workflows/release.yml` ne se déclenche que sur un push qui
touche `Cargo.toml`, et ne fait rien si le tag `v{version}` existe déjà.
Sinon : `make dist` (noyau dépouillé de ses informations de débogage,
92 → 8 Mio ; disque vierge GPT + LIFS compressé), puis `make test` **sur
l'ISO de `build/dist/`** (`TEST_ISO`), puis le tag et la release, notes
tirées de `CHANGELOG.md`. Une version suffixée part en pré-release. La
release n'est donc jamais une ISO que personne n'a vue démarrer.


## Mises à jour atomiques des applications (phase 15) — décisions

### Les dix points de 15.1 sont tranchés sous mandat d'autonomie (28 octobre 2026)

L'utilisateur a demandé de continuer « en total autonomie ». Chaque point
prend la recommandation écrite dans la roadmap le 24 septembre ; le
détail est dans ROADMAP, phase 15, « Points tranchés ». Révisables par
l'utilisateur. Écart assumé au texte proposé : la version **précédente**
est gardée pour le retour arrière (le texte supprimait l'ancienne dès la
fin de son processus), comme le §11.3 le fait pour l'OS.

### Le pointeur actif est un fichier, pas un lien de dossier

Première idée : `current` lien Tag vers le dossier de la version, pour
que `current/app.cus` se résolve sans rien lire. Écartée pour deux
raisons. **Un processus de la version 1 qui ouvre encore ses fichiers
par `current/...` après la bascule lirait ceux de la version 2** (Firefox
charge ses bibliothèques au fil de l'eau) : mélange de versions dans un
même processus. Et **LIFS ne nomme un dossier qu'une fois**
(`check_volume` : « dossier nommé deux fois ») : c'est ce qui garantit
l'absence de cycles, et le reverse index ne remonte que par le premier
parent. Le pointeur est donc un petit fichier lu au lancement ;
`fs::retarget` refuse les dossiers des deux côtés, sur LIFS comme sur
NovaFS.

### `fs::retarget` : une opération, donc une transaction

Sur LIFS, aucune validation n'a lieu au milieu d'une opération (phase
14) : retrait de l'ancienne entrée, ajout de la nouvelle, comptes de
liens et index inverse sont dans la même transaction. Sur NovaFS, sans
journal, le champ nœud de l'entrée est réécrit **en place** (une entrée
de 128 octets dans un bloc). Éprouvé : 17 coupures pendant la bascule,
11 laissent l'ancienne version, 6 la nouvelle, aucune sans pointeur.

### La clé de confiance d'un registre est celle de sa déclaration

Un manifeste porte la clé qui l'a signé ; elle n'est comparée qu'à celle
enregistrée par `spm registry add`, jamais crue pour elle-même —
n'importe qui peut signer un manifeste avec sa propre clé. Et un
manifeste valide d'une autre application est refusé : un registre ne
peut pas servir le manifeste de A à qui demande B.

### Un JSON aux clés en double est refusé

RFC 8259 laisse leur sens ouvert ; les implémentations divergent (la
première, la dernière). Pour un document signé, c'est une attaque : le
signataire lit une valeur, l'installateur une autre. `json::parse`
refuse. La forme canonique (compacte, membres triés par octets UTF-8)
est celle de `tools/novasign.py` (`json.dumps(sort_keys=True,
separators=(",", ":"), ensure_ascii=False)`) ; un flottant y est refusé,
le noyau gardant le texte des nombres et Python le réécrivant.

### Un registre qui ne connaît pas une application n'est pas un refus

Trouvé par `make test` au second démarrage : un registre resté déclaré
(voir ci-dessous) sans manifeste pour `test.spm.exemple` faisait échouer
`spm install test.spm.exemple` de la phase 8, au lieu de passer aux
dépôts d'index. `registry::resolve` distingue désormais : une lecture
impossible (absent, réseau) = « inconnu ici », on demande ailleurs ; un
manifeste présent qui échoue ses contrôles = refus, rapporté.

### Un autotest qui purge doit valider avant son verdict

LIFS valide par groupe toutes les 2 s. `make test` arrête QEMU dès le
dernier verdict : une purge écrite juste avant n'était pas validée, et le
démarrage suivant retrouvait le registre de l'autotest. Comportement
correct du système de fichiers (« une coupure perd au plus les dernières
secondes ») ; c'est l'autotest qui doit `flush_all` avant d'imprimer.

### ENTERPRISE, version 2 de trois formats

Certificat des binaires (v2, même bloc de 256 octets : clé publique en
0x78, signature de 64 octets en 0x98), en-tête des paquets (v2 : 320
octets, signature en 0xC0, clé en 0x100 ; la v1 reste lue pour SELF et
SYSTEM), conteneur `.kmd` (v2 pour ENTERPRISE seulement : signature en
176..240, `test-gpu.kmd` SYSTEM inchangé). Étiquettes de domaine
distinctes pour qu'une signature de binaire ne vaille pas pour un paquet.
**L'autorité de démonstration** des autotests a une graine tirée à chaque
démarrage : une graine écrite dans le source aurait recréé, avec une
vraie signature, l'autorité falsifiable que cette version supprime.

### Le téléchargement va là où il sera repris

Un paquet se télécharge dans la racine de l'application
(`telechargement-{v}.spm`, hors de `versions/`) quand elle est déjà
versionnée, dans `/sys/updates/` sinon ; il est **repris** par `Range`
après une coupure, et supprimé après l'installation comme après un refus.
Aucun codage de contenu n'est demandé (`identity`) : un paquet est déjà
compressé, et décoder en flux serait un analyseur de plus.

### Limites de la phase 15, connues

- `installer` sur `spm.update` télécharge **dans la tâche du service** :
  une autre requête attend la fin du transfert.
- Le choix du canal se fait par commande (`spm canal`, `--canal`) ; pas
  encore d'onglet dans les paramètres.
- Le suivi des processus d'une version remonte les parents (64 niveaux) :
  un processus qui changerait de parent (adoption par `init.dsa` après la
  mort du sien) sortirait du compte. Les tâches de Nova gardent le PID de
  leur parent même mort : le cas ne se produit pas aujourd'hui.
- Aucun registre réel (GitHub) n'est éprouvé sous QEMU : le transfert
  HTTPS l'est sur une connexion scriptée, le cycle complet sur un registre
  `file:`.

## Shell, terminal et mode recovery (phase 16.0) — décisions

### Le shell est une bibliothèque, pas un programme

`crate::shell::Shell` ne lit rien et n'écrit rien : `run(ligne)` rend des
lignes (`Output { lines, clear, exit, failed }`). La fenêtre
(`terminal.ss`), la console série (`shell-serie`) et l'autotest s'en
servent chacun à sa façon. Un shell qui écrirait lui-même aurait demandé
trois copies des commandes, ou une abstraction de terminal pour un seul
usage.

### Les commandes sont françaises, les noms usuels sont des alias

`liste`, `va`, `lire`, `ecrire`… parce que le système parle français
(14.1) ; `ls`, `cd`, `cat`, `rm`… parce que ses utilisateurs les tapent.
Une seule table (`commands::COMMANDS`) sert à la fois à `aide` et à
l'aiguillage : une commande ne peut pas exister sans son aide.

### Le shell n'accorde rien

Chaque commande appelle la couche existante (`fs::*`, `scheduler`,
`spm::manager::execute`, `sdk::cli::execute`) avec les droits du compte
du shell. `tuer` reprend la règle de `SYS_PROC_KILL` (ROOT et ADMIN
partout, les autres sur leurs tâches). L'autotest vérifie qu'un compte
ordinaire se voit refuser `/sys` et le fichier privé de ROOT.

### Recovery : ROOT sur la console série — choix pris sous mandat d'autonomie

Le §2.2 dit « kernel + fs.dsa + shell CLI uniquement » sans dire sous quel
compte. La console série d'un mode recovery n'a pas de session à ouvrir
(pas de compositeur, donc pas d'écran de connexion) et sert à réparer ce
qui empêche la session de démarrer : elle tourne en ROOT, comme le mode
mono-utilisateur d'Unix. Qui choisit l'entrée recovery de GRUB a la
machine sous la main. **À confirmer par l'auteur** : un mot de passe ROOT
demandé sur la console serait l'alternative.

### COM1 est scruté, pas servi par interruption

Le démarrage n'arme pas l'IRQ 4 et une personne tape lentement : 50 ms
entre deux lectures au repos, 5 ms quand des octets arrivent. Hors
recovery, la tâche ne dit rien tant qu'on ne tape rien — le journal de
démarrage reste le journal — et `make run-nogui` a un shell sans rien
changer.

### Le mode recovery omet les autotests et s'arrête aux services core

`kernel_main` lit le mode avant de lancer les autotests ; `init.dsa`
s'arrête après `fs.dsa`, `ipc.dsa`, `hw.dsa` et passe à l'adoption des
orphelines. Lancer les autotests en recovery écrirait sur le volume
qu'on cherche à réparer.

### Le terminal s'éprouve sans fenêtre, puis avec

L'édition (curseur, historique, coupure des lignes, défilement, Ctrl+C,
Ctrl+L, `quitter`) est jouée par des `KeyEvent` synthétiques sur un
`Terminal` sans fenêtre (`terminal::exercise`). Puis la vraie fenêtre est
lancée comme le menu la lance, doit obtenir sa surface et la renommer
(`Terminal - <dossier>`, au tour qui peint le premier), puis se fermer
sur le signal d'arrêt des modules et rendre sa surface. Sans compositeur
(`-vga none`), cette seconde partie est omise.

### `make test-recovery` tape sur la console série

`tools/boot-test.py --type 'REGEX=>TEXTE'` écrit une commande sur
l'entrée standard de QEMU (`-serial stdio`) quand une ligne attendue
apparaît : chaque réponse est attendue avant la commande suivante. L'ISO
de recovery est la même, `grub.cfg` réécrit avec `set default=1`.

### Limites de la 16.0, connues

- Pas de tubes, de redirections, de jokers (`*`), de variables ni de
  scripts : une ligne, une commande.
- Une commande tourne dans la tâche du terminal : un `spm install` qui
  télécharge fige la fenêtre le temps du transfert.
- Console série : pas d'historique aux flèches (les séquences
  d'échappement sont ignorées), pas de complétion.
- Terminal : pas de sélection ni de copier-coller ; un caractère hors
  ASCII s'affiche `?` (la police n'a que l'ASCII).

## Moteur JavaScript (phase 16.1) — décisions

### Le moteur se compile aussi pour l'hôte

`src/web/js/` n'utilise que `alloc` : `tools/jshost/` le compile avec
`std` (`rustc` directement, `.cargo/config.toml` orientant cargo vers la
cible du noyau) et fait tourner le corpus en 0,2 s, contre un démarrage
de QEMU. Trois cales seulement : la marge de pile (mesurée sur le fil),
`heap::room_for` et `serial_println!`. `make js-test`, dans la CI avant
les démarrages. Un moteur d'un peu moins de dix mille lignes ne se met pas
au point à vingt secondes l'essai.

### Les attentes du corpus sont celles de Node, pas celles du moteur

Chaque attente de `web::js::selftest` a été vérifiée contre Node 22
(fuseau UTC+1, horloge figée sur celle de l'hôte de test) avant d'être
confrontée au moteur. Un corpus écrit en regardant ce que le moteur
répond ne teste que sa reproductibilité ; c'est la règle des encodages
de l'assembleur (« se vérifient contre une table écrite à la main »)
appliquée au langage. Dix attentes fausses écrites de mémoire ont été
corrigées ainsi : l'auteur d'un test se trompe aussi.

### Un script n'épuise pas le tas du noyau

Le tas fait 64 Mio et l'allocateur arrête la machine quand il échoue
(`alloc_error`). Un `s += s` en boucle l'a fait dès le premier essai de
l'autotest. Désormais toute chaîne ou tout tableau qui va dépasser
256 Kio passe par `ops::reserve`, qui demande à l'hôte
(`Host::can_allocate` → `heap::room_for` : 16 Mio gardés pour le noyau,
le plus grand bloc libre une fois et demie la demande, puisqu'un vecteur
qui grandit déménage avant de libérer) ; le script reçoit une
`RangeError` qu'il peut rattraper. Les croissances qui n'allouent aucun
objet sont vues par `tick` toutes les 64 Ki étapes. `MAX_ARRAY` vaut
16 Mi éléments (l'ancien `new Array(1e7)` demandait 160 Mio d'un coup).

### `exp` et `ln` en virgule fixe 116 bits

Une série de Taylor en doubles arrondit à chaque terme : `Math.exp(1)`
ne valait pas `Math.E`. La réduction (x = k ln 2 + r) et la série se font
en virgule fixe sur 116 bits, avec un produit 256 bits, et un seul
arrondi à la fin (`Num::pack`). Sur 2 000 valeurs tirées au hasard, deux
écarts avec la libm de l'hôte : vérifiés en précision décimale, c'est la
libm qui avait tort.

### `Num::eq` masquait `PartialEq::eq`

`Num` dérive `PartialEq` (égalité des bits) et avait une méthode `eq`
(égalité IEEE : NaN différent de lui-même, -0 égal à +0). Sur un `&Num`,
la résolution de méthode trouvait d'abord le trait : `a.eq(*b)` ne
compilait pas, et ailleurs comparait les bits sans le dire. Renommée
`equals`.

### `JSON.parse` lit avec le lecteur des manifestes, en mode tolérant

`crate::json` refuse les clés en double (un manifeste signé où deux
lectures diffèrent est une attaque) ; le langage veut que la dernière
gagne, à la place de la première, et accepte un surrogat isolé.
`json::Options::LENIENT` le fait, `Options::STRICT` reste le défaut ; un
index se construit au-delà de 16 membres pour que les doublons ne coûtent
pas le carré de la taille. Les nombres gardent leur texte et
`Num::parse` les convertit exactement.

### `Date` : le fuseau fixe de l'hôte

Les fuseaux de Nova sont des décalages fixes (`i18n::tz`), les heures
d'été listées à part : un seul décalage vaut pour toutes les dates, donné
par `Host::tz_offset_minutes`. Une page voit l'heure de la barre des
tâches. `toString` ne nomme pas le fuseau entre parenthèses (le texte est
laissé à l'implémentation).

### Limites de la 16.1, connues

- `BigInt` lu comme un nombre ; ni `Proxy`, ni `Intl` (`toLocaleString`
  suit `en-US`) ; tableaux sans trous (un trou vaut `undefined`) ;
  `eval` indirect seulement.
- Générateurs exécutés d'un bloc, `await` qui attend sur place (choix de
  l'interprète d'origine, documentés dans `interp.rs`).
- `WeakMap`, `WeakSet`, `WeakRef` gardent leurs clés jusqu'à la fin du
  royaume.

## Scripts du navigateur intégré (phase 16.2) — décisions

### Un nœud ne meurt pas : l'arène garde sa place

Un objet de script nomme son nœud par indice ; libérer un nœud et
réutiliser sa place ferait parler un vieil objet d'un nœud neuf. Les
nœuds retirés restent donc, sans parent, et la création reste bornée par
`html::MAX_NODES`. Conséquence qu'il fallait tirer partout : **l'ordre du
document est celui de l'arbre, pas de l'arène.** `style::compute` le
supposait (« un parent précède toujours ses enfants, puisque l'analyseur
les ajoute dans cet ordre ») ; un script qui déplace un nœud sous un nœud
créé après lui le démentait. Tout parcours ordonné passe par
`Document::preorder`, qui écarte aussi les nœuds détachés.

### Le DOM s'éprouve contre jsdom, comme le langage contre Node

Même règle que pour 16.1 : les 30 cas du DOM ont été passés dans jsdom 24
avant d'être confrontés au moteur. Les écarts trouvés ont été corrigés du
côté qui avait tort (le moteur pour `cssText`, le test pour l'ordre des
cookies).

### Les écouteurs vivent sur l'objet cible

Dans une propriété qu'aucun script ne peut nommer : les nœuds, la fenêtre
(objet global) et un `XMLHttpRequest` partagent un seul mécanisme. Un
gestionnaire en ligne est compilé la première fois que son événement part,
puis gardé ; le réécrire (`setAttribute('onclick', ...)`) l'oublie.

### Un clic va à l'élément par les boîtes de la mise en page

La mise en page rend la boîte de chaque bloc, contrôle, image et passage
de texte (`Page::boxes`), dans l'ordre de peinture : la dernière qui
contient le point est la plus intérieure. Un passage de texte vaut son
élément parent. Les mêmes boîtes servent `getBoundingClientRect`.

### Les scripts d'abord, le comportement par défaut ensuite

Un clic, une touche, l'envoi d'un formulaire passent d'abord par les
écouteurs de la page ; `preventDefault` annule ce que le navigateur
aurait fait (suivre le lien, éditer le champ, envoyer). Un script ne
peut pas commander le navigateur pour autant : une navigation qu'il
demande passe par `may_open`, qui refuse `about:action/...` depuis une
page du web, et les pages internes ne portent aucun script.

### `fetch` synchrone sous la promesse — limite assumée

La requête se fait dans l'appel (`net::fetch_now`) : la promesse rendue
est déjà réglée, ses `then` passent après le script comme le langage
l'exige, mais la fenêtre attend le transfert. Un transfert dans une tâche
à part, la promesse réglée à son arrivée, est l'étape suivante.

### `document.cookie` ne voit pas `HttpOnly`

`Jar::for_script` les écarte, `Jar::store_from_script` refuse d'en créer
ou d'en remplacer : c'est la raison d'être de l'attribut.

### Bug du noyau trouvé en route : une tâche morte récoltée pendant qu'on la quittait

Un démarrage sur une quarantaine (sans écran, 4 cœurs), pendant
l'autotest 11.3 : « faute noyau 0x0 - tache 3 sur le coeur 2 - RIP 0x0 »,
juste après deux tâches ring 3 terminées par une faute de page sur ce
cœur ; le cœur arrêté, l'autotest suivant y affamait son voisin. Sans
rapport avec le JavaScript, qui ne tournait pas encore.

La course : `yield_now` met `current[cpu]` sur la tâche entrante **sous le
verrou**, rend le verrou, puis `switch_context` empile 576 octets sur la
pile de la tâche sortante et écrit son RSP dans sa case de la table.
`try_reap` jugeait une tâche morte récoltable dès qu'aucun cœur ne la
nommait courante : un autre cœur pouvait donc, dans cette fenêtre,
libérer sa pile et donner sa case à une tâche neuve — que le changement
de contexte écrasait ensuite. Le commentaire de `try_reap` (« tant qu'un
cœur la nomme courante, elle n'est pas récoltable ») était juste, mais
`current` change avant que le cœur ait fini de partir.

Correctif : `scheduler::LEAVING`, par cœur, la tâche qu'il quitte, posée
sous le verrou avec `current` et effacée par la tâche entrante
(`switched_in`, au retour de `switch_context` et dans `task_trampoline`
pour une première exécution) ; `try_reap` refuse une tâche encore
quittée. **À ne pas réintroduire** : toute libération d'une tâche doit
attendre que son cœur ait fini de la quitter, pas seulement qu'il en ait
élu une autre.

---

## 4. Invariants à ne jamais casser

- Le kernel doit rester **protégé du frame allocator** : `[frame] verification
  protection kernel : OK` doit apparaître à chaque boot.
- Il ne doit jamais y avoir **plus d'une tâche `RUNNING` par cœur**.
- Chaque cœur doit voir son compteur de tics **progresser** — un cœur à zéro
  signifie un timer non armé.
- `nm -u build/nova-os.bin` doit renvoyer **vide** (aucun symbole non résolu).
- `objdump -d` ne doit contenir **aucune instruction SSE** tant que la target
  est en soft-float.
- Le nombre d'allocations et de libérations du heap doit se rejoindre au repos ;
  un écart signale une fuite ou une fusion défaillante. Plus fiable encore sur
  une charge cyclique : `{octets alloués, octets libres}` doit être **identique**
  d'un rapport du moniteur au suivant.
- Le nombre de frames libres doit être **identique** d'un rapport à l'autre une
  fois le boot terminé.
- `loader::load` suivi de `loader::unload` doit rendre **exactement** les frames
  prises — tables de pages intermédiaires comprises.
- Aucune page utilisateur ne doit être à la fois modifiable et exécutable :
  `loader::verify_protection` le contrôle en relisant les tables.
- Aucune récursion noyau dont la profondeur dépend d'une donnée extérieure ne
  doit descendre sans avoir consulté `scheduler::stack_headroom` : les piles de
  tâches n'ont **pas** de page de garde, et les déborder n'est pas signalé.
- SHA-256 doit reproduire les vecteurs de FIPS 180-4 à chaque boot. Une
  implémentation de hachage fausse produit des empreintes stables et d'apparence
  normale : rien ne la trahit sans vecteurs de référence.
- `fs::check_all` doit passer : tout bloc référencé par un nœud est marqué
  occupé dans le bitmap, tout nœud occupé porte au moins un lien.
- Au **second** démarrage, le volume doit être remonté, jamais reformaté.
  Le compteur `/logs/kernel/demarrages.txt` doit s'incrémenter — c'est le
  seul contrôle qui distingue un système de fichiers correct en mémoire
  d'un système de fichiers qui écrit réellement.
- Les images produites par l'export doivent passer les vérificateurs de
  l'hôte **sans aucune correction** (`make check-export`) : `fsck.vfat -n`
  pour FAT32, `fsck.ext4 -fn` pour ext4, `ntfsfix -n` pour NTFS. Une image
  lisible par Nova OS seule ne prouve rien : c'est justement
  l'interopérabilité qu'on vise.
- Le nombre de blocs libres du volume doit revenir à sa valeur d'origine
  après qu'un fichier a grossi puis rétréci, **table d'indirection
  comprise**. Un écart d'un seul bloc est une fuite.
- `SYS_MEM_ALLOC` suivi de `SYS_MEM_FREE` doit rendre **exactement** les
  cadres pris, et l'arène revenir à zéro page allouée.
- Une région fraîchement allouée doit être **entièrement à zéro** : une
  page recyclée qui garderait le contenu d'une autre tâche serait une
  fuite d'information. C'est aussi ce contrôle qui détecte un mauvais
  calcul d'adresse physique.
- Le cycle complet naissance → communication → mort → `SYS_PROC_WAIT`
  d'une tâche ne doit fuir aucun cadre.
- L'autotest de la phase 5 doit passer **en monoprocesseur et en quatre
  cœurs**. Les défauts de synchronisation ne se manifestent que sous la
  seconde configuration : n'exécuter que la première revient à ne pas
  tester la concurrence.
- **Aucun pilote ne doit rester en état `demarrage`.** Un pilote se
  déclare `pret` ou `absent` ; rester entre les deux signifie qu'il est
  bloqué sur un matériel qui ne répond pas, et `init.dsa` n'a alors aucun
  moyen de savoir si le démarrage peut continuer.
- **Un pilote `pret` a ouvert son canal nommé.** C'est la seule chose que
  ses clients connaissent de lui ; un pilote prêt sans canal est
  injoignable.
- Le démarrage doit aboutir **sans aucun `BOOT_ERR_`** dans une
  configuration nominale, et se poursuivre jusqu'à l'autotest quand la
  carte réseau et l'audio sont absents (`make run-nonet`).
- Le vecteur de la RFC 1071 doit être reproduit à chaque démarrage, et la
  somme d'un tampon découpé en morceaux doit égaler celle du tampon
  entier — c'est ce second contrôle qui détecte une retenue perdue entre
  deux parties.
- Un `.ras` doit se voir refuser **tout** envoi IPC vers une autre
  application, et deux applications de groupes différents doivent être
  renvoyées vers le port interne. Vérifié sur la table de décision *et*
  sur le chemin réel d'`ipc::send` : une règle documentée qui n'est pas
  appliquée ne vaut rien.
- **Aucun `■` à l'écran.** Un carré plein signale un octet non-ASCII dans
  une chaîne affichée, jamais autre chose.
- **Un autotest qui écrit sur un volume persistant doit purger son
  arborescence en entrant**, pas seulement en sortant. Un passage
  interrompu au mauvais moment laisse des fichiers derrière lui, et tous
  les démarrages suivants échouent sur un dossier qu'ils croient vide.
  Le symptôme trompe : le code fautif n'est pas celui qui échoue.
- Le démarrage doit rester **sans échec avec `-vga std`, `-vga cirrus`
  (24 bits) et `-vga none`** (repli en mode texte). Les trois exercent des
  chemins différents du pilote framebuffer.
- Le PAT de **chaque** cœur doit valoir `0x0007040100070406` dès qu'une
  page write-combining existe. Un cœur resté au défaut lirait la même page
  physique en write-back.
- **Une commande de dessin ne sort jamais de sa surface.** Vérifié en
  dessinant avec des coordonnées délirantes dans les quatre directions
  puis en relisant les pixels alentour. Une fuite ici est une application
  qui peut écrire sur la fenêtre d'une autre.
- **Seul l'emplacement `Core` n'est pas remplaçable** (§8.2), et aucun
  fichier `.mod` ne peut prétendre le contraire : le drapeau et le tableau
  doivent dire la même chose, sinon le fichier est refusé.
- **Aucune valeur de `z` ne fait passer une fenêtre devant le curseur.**
  La couche prime, toujours.
- **Tout appelant de `loader::load_and_spawn` appelle `loader::keep`.**
  `LoadedImage` n'a pas de `Drop` : l'abandonner fuit ses cadres, ses
  mappings et son créneau d'arène en silence.
- **Aucun chemin du SDK ne produit un binaire en prétendant l'avoir
  compilé.** Un langage sans compilateur en place rend un refus nommé.
- **Toute boucle d'analyse garantit sa progression.** Un analyseur qui
  peut ne rien consommer fait tourner un cœur jusqu'au redémarrage.
- **Un bac à sable détruit rend exactement les cadres qu'il a pris**, et
  son dossier sous `/cache/sandbox/` disparaît. Les compteurs `creee(s)`
  et `detruite(s)` doivent se rejoindre à la fin de l'autotest.
- **Le taux de conversion ETR reste un entier de 0 à 100.** Le noyau est
  en soft-float : un flottant ici serait un flottant dans le noyau.
- **Le compositeur ne se bloque jamais sur un client.** C'est cette
  asymétrie qui rend sûre la contre-pression du côté client ; l'inverser
  ferait de la première application lente un gel de tout l'écran.
- **Une image n'écrit jamais hors de la zone qu'elle publie.** Sinon le
  tampon arrière diverge du tampon avant, et la divergence se manifeste
  plusieurs trames plus tard, ailleurs.
- **Le curseur reçoit les événements de pointeur bien qu'il soit exclu du
  ciblage.** Exclure une couche du ciblage n'est pas l'exclure de la
  livraison.
- **Les raccourcis du §16.4 atteignent les modules du shell quelle que
  soit la fenêtre focalisée.** Une application plein écran ne doit pas
  pouvoir rendre l'accessibilité inatteignable.
- **L'autotest de la phase 7 ne laisse ni module, ni compte, ni surface
  derrière lui.** Le volume et le fichier des empreintes survivent au
  redémarrage.
- Le démarrage doit rester **sans échec avec `-vga none`** : sans
  framebuffer, le compositeur ne démarre pas, aucun module n'est lancé, et
  le système atteint quand même ses autotests.
- **Une installation refusée ne laisse rien sur le disque.** Vérifié en
  comptant les entrées de `/apps/` avant et après un refus, pour chacune
  des causes : dépendance absente, script pre-installation débordant,
  signature rejetée.
- **Un script d'installation ne sort jamais du préfixe du paquet.**
  Vérifié sur un `write` hors périmètre, sur une remontée par `..`, et sur
  un préfixe textuel voisin (`/apps/mon` ne doit pas autoriser
  `/apps/monautre`). Le contrôle porte sur le programme entier avant
  exécution : la première instruction ne doit pas avoir eu lieu si la
  seconde déborde.
- **`allocated + free` doit valoir exactement la taille du tas** à chaque
  rapport du moniteur. C'est ce qui distingue une allocation vivante d'un
  fragment perdu par l'allocateur : les deux font monter `allocated`, seul
  le second casse l'égalité.
- **L'autotest de la phase 8 ne laisse ni paquet, ni entrée de base, ni
  dossier derrière lui**, et purge son périmètre **en entrant** autant
  qu'en sortant.
- **Le magasin de confiance ne doit jamais être plein.** Un refus
  d'insertion est journalisé ; le voir apparaître signifie qu'une empreinte
  manque et qu'un binaire sera refusé au chargement sans explication.
- Au **second** démarrage, `spm` doit afficher `0 dossier(s) cree(s),
  depot local deja declare` : l'arborescence et les dépôts sont
  idempotents, sans quoi un dépôt ajouté par l'utilisateur disparaîtrait à
  chaque redémarrage.
- **La table des tâches ne croît pas indéfiniment.** Le compteur du
  moniteur doit se stabiliser une fois le démarrage terminé ; s'il monte
  encore, un emplacement mort n'est pas réemployé.
- **Aucune tâche ne doit tourner en boucle serrée sans raison.** Une tâche
  qui attend doit dormir, pas scruter : un seul scrutateur suffit à priver
  le compositeur du temps dont il a besoin pour suivre la souris, et le
  symptôme se décrit alors comme « la souris ne bouge pas ».
- **Un pointeur relevé dans la table des tâches doit rester valide après
  la libération du verrou.** C'est ce que garantit la `Box` ; tout retour
  à un `Vec<Process>` rouvre la fenêtre d'écriture après libération.
- **`MAP_FIXED` retire et projette sous une seule prise du verrou des
  régions.** Entre deux prises, la plage est libre pour le `mmap` d'un
  autre thread.
- **Aucune pile de tâche dans le tas du noyau.** Le tas mesure 64 Mio ;
  trois cents threads à 128 Kio l'avaient rempli aux deux tiers.
- **Le démarrage doit aboutir sans contrôleur USB.** `[usb] aucun
  controleur USB sur le bus PCI` puis un pilote souris déclaré `pret` en
  PS/2 seul : une machine sans xHCI garde sa souris.
- **Le pilote souris n'est absent que si les deux bus sont muets.** Le
  déclarer absent parce que le PS/2 ne répond pas priverait une machine
  moderne — qui n'a plus de port PS/2 — d'une souris USB fonctionnelle.
- **Un transfert d'interruption USB est réarmé après chaque rapport.**
  Sans quoi la souris se fige après son premier mouvement, et le compteur
  d'événements du pilote reste à un.
- **Les deux bus produisent le même format d'événement sur `dev.mouse`.**
  Rien en aval ne doit pouvoir distinguer un mouvement PS/2 d'un
  mouvement USB.
- **L'identité d'une tâche vivante ne change pas.** `apps::register` doit
  **refuser** un second enregistrement portant une autre identité : le
  §12.1 la veut immuable à l'exécution, et un écrasement silencieux était
  exactement la mutation interdite.
- **Le bac à sable du §12.4 est appliqué, pas seulement décrit.** Tout
  appel système nommant un chemin passe par `domain_fs::guard`, donc par
  `security::check_path`. Une règle appliquée à dix-sept endroits sur
  dix-huit n'est pas appliquée.
- **L'ordre des quatre étapes du §12.3 est vérifié, pas seulement ses
  branches.** Les cas d'autotest sont construits pour être ambigus si
  l'ordre est faux : un `.ras` qui déclare `IPC_PUBLIC`, un `.cus` qui
  atteint `/sys/` par un chemin que le bac à sable aurait ouvert.
- **Aucune file d'entrée ne répond à une question posée après elle.**
  Toute lecture de réponse à une invite vide la file du clavier d'abord.
- **Le nombre de cœurs en retard sur l'invalidation de TLB doit être zéro
  au repos.** `[smp] N invalidation(s) de TLB diffusee(s) - 0 coeur(s) en
  retard` : un cœur durablement en retard ne prend plus ses interruptions,
  ce qui est un problème bien pire qu'une TLB périmée.
- **Une tâche qui entre en ring 3 synchronise sa TLB.** C'est le seul
  point où une traduction périmée coûte quelque chose, et c'est là qu'un
  binaire étranger a exécuté le code de l'occupant précédent de son
  créneau d'arène.
- **Le noyau ne s'exécute jamais avec le drapeau de direction levé.** Tout
  point d'entrée en assembleur commence par `cld` : un `rep movs` du noyau
  qui remonte les adresses écrit dans la frame d'à côté.
- **Une frame ne retourne à l'allocateur qu'une fois hors de toute
  traduction, TLB compris.** Une région retirée, et l'objet dont elle tient
  peut-être la dernière référence, ne tombent qu'après l'invalidation ; une
  page coupée d'un fichier projeté est retirée, pas libérée.
- **Aucune tâche étrangère ne survit à son programme.** Un PID mort qui
  garderait sa personnalité ferait traduire les appels Nova d'une tâche
  ultérieure comme des appels Linux.
- **Le pilote audio produit au plus un tampon par réveil.** Le coût d'un
  chemin appelé cent fois par seconde se borne par le temps, jamais par
  « ce que le matériel a consommé » : un périphérique émulé n'a pas les
  vitesses d'un vrai.
- **Le lecteur d'écran n'ouvre son flux audio que pour parler.** Un flux
  qui existe fait tourner le pilote, y compris pour du silence.
- **Un rectangle cliquable et le rectangle peint qui le montre viennent du
  même appel.** `login::Layout` est la seule arithmétique de géométrie du
  panneau de connexion ; l'autotest vérifie que chaque zone est atteignable,
  qu'aucune n'empiète sur une autre et que cela tient de 0 à 8 comptes.
- **Deux zones cliquables adjacentes se distinguent au pixel.** Le dernier
  pixel du champ de mot de passe répond `Password` et le premier de la flèche
  répond `Submit` : un intervalle qui se refermerait ferait de l'ordre des
  tests de `Layout::hit` la seule chose qui les sépare.
- **Le survol ne repeint qu'au franchissement d'une frontière.**
  `[login] pointeur : N evenement(s), M repeint(s) de survol` : un rapport qui
  s'approcherait de un pour un voudrait dire que la condition a sauté, et le
  coût qui avait fait refuser le survol serait revenu.
- **Quitter une surface rend la flèche.** Aucun client ne peut laisser sa forme
  de curseur sur le bureau en cessant de répondre.
- **Aucune composante de couleur ne déborde sur sa voisine.** `mix` est
  éprouvé sur les 256 poids : une composante qui déborderait donnerait une
  teinte de survol d'une autre couleur, pas d'une autre clarté.
- **Aucun code d'opération du protocole graphique n'est utilisé deux fois.**
  Une collision enverrait une bascule de menu là où un repeint était voulu.
  Les trente-six codes sont comparés entre eux, et non seulement les nouveaux :
  une collision ne s'introduit qu'en ajoutant.
- **Un élément survolé répond à un clic.** La règle qui décide où le survol
  existe. Un surlignage sur un élément inerte se lit comme une promesse.
- **Un module vide sa boîte à chaque réveil.** Un événement par réveil donne
  une latence de clic égale à la file accumulée : mesuré 1336 ms, corrigé à
  41 ms. La période de rafraîchissement d'un module n'est pas sa latence
  d'entrée.
- **Un rectangle cliquable dessiné est un rectangle cliquable testé.** La croix
  de fermeture a été peinte pendant trois phases sans rien fermer.
- **Une application demande où est sa fenêtre à chaque événement.** Le
  compositeur la déplace sans l'avertir.
- **L'encodeur Venus est identique à la référence.** `make venus-check`
  compare les 65 commandes octet par octet ; une commande ajoutée à
  `tools/gen-venus.py` entre dans la comparaison d'office.
- **Aucun tampon de commandes GPU n'atteint l'hôte sans avoir été validé
  entier.** Une commande refusée refuse la soumission : pas d'effet
  partiel, et le contexte sert encore après (vérifié par l'autotest).
- **Un objet GPU détruit pendant qu'une soumission peut s'en servir ne
  disparaît qu'après elle.** L'autotest détruit le tampon de sommets juste
  après avoir soumis le triangle, et le triangle est juste.
- **La mémoire GPU comptée revient à zéro quand tout est détruit**, et les
  contextes d'un processus mort sont fermés par le balayage de `drv-gpu`.
- **Aucune page de module .kmd n'est à la fois modifiable et exécutable**,
  et aucune de ses instructions ne s'exécute avant la vérification de sa
  signature.
- **Une coupure pendant la bascule d'un pointeur `current` laisse
  l'ancienne version ou la nouvelle, jamais aucune** (`[lifs] N coupures
  pendant la bascule : ..., 0 sans pointeur`).
- **Une mise à jour refusée ne laisse rien** : ni dossier de version, ni
  `.tmp`, ni téléchargement (autotest de la phase 15, quatre causes).
- **Une application ne demande que pour elle-même** sur `spm.update` ;
  son identité est la racine depuis laquelle SPM l'a lancée.
- **Le compositeur sur le GPU donne les mêmes pixels que sur le
  processeur** : `composition ... 0 pixel(s) differents du processeur` à
  chaque démarrage avec GPU.
- **Aucune attente de QEMU sous un verrou tournant pendant l'exécution** :
  les requêtes de la file de contrôle sont postées ou attendues verrou
  relâché ; le délai de garde compte au TSC.
