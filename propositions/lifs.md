# Proposition LIFS (Linked Index File System) — texte de l'utilisateur

Reçu le 24 septembre 2026, sous forme d'une page HTML. Conservé ici tel quel
dans son contenu (mise en forme HTML retirée), comme **proposition** : ce
n'est pas encore la spécification. Son intégration est l'objet de la phase 14
de `ROADMAP.md`, qui relève aussi ses manques et ses contradictions avec
l'existant.

Précision de l'utilisateur (24 septembre 2026) : **LIFS est un système de
fichiers, donc un nouveau type de partition** — pas une révision de NovaFS.

---

## 1. Concept et philosophie d'architecture

LIFS est un système de fichiers de nouvelle génération conçu pour une
intégration native et hautement performante avec un système d'exploitation
moderne. Il abandonne la hiérarchie physique traditionnelle au profit d'une
**topologie de stockage plate** (Flat Storage Topology) pilotée par des
**micro-bases de données**.

- **Dossiers virtuels (DirNodes)** : un dossier n'est pas un conteneur
  physique, mais un fichier spécial (Micro-DB / « lien invisible ») contenant
  une liste de pointeurs vers des identifiants uniques (Node IDs).
- **Performance O(1)** : le déplacement ou la copie de données,
  indépendamment de leur taille (1 Ko ou 100 Go), s'effectue en temps
  constant O(1) par la simple modification d'un pointeur dans la Micro-DB.
- **Sécurité et flexibilité** : intégration native du Copy-on-Write (CoW) et
  d'un système de multi-tagging gérable depuis les interfaces graphiques.

## 2. Le moteur de liens (Routing Engine)

LIFS repose sur la distinction entre deux types de liens (DirEntries) stockés
dans les Micro-DBs.

### 2.1 Le lien CoW (Copy-on-Write) — copie par défaut

Pointe vers le fichier original. Ne consomme aucun espace disque
supplémentaire.

**Déclencheur** : dès qu'une écriture (`write` ou `overwrite`) est requise,
l'OS intercepte l'appel, copie les blocs modifiés vers un nouvel espace libre,
crée un nouveau Node ID, et met à jour le lien silencieusement.

### 2.2 Le lien Tag (multi-tagging) — partage d'état

Pointe vers le fichier original de manière forte (hard link intelligent).

**Déclencheur** : toute modification du fichier est instantanément répercutée
et visible dans tous les dossiers contenant ce Tag Link. L'OS écrit
directement dans les blocs originaux.

## 3. Matrice de permissions

Masque binaire sur 16 bits, 13 niveaux de contrôle.

| Bit | Permission | Fichier | Dossier | Description |
|---|---|---|---|---|
| 0 | read | ✓ | — | Lire le contenu binaire d'un fichier |
| 1 | write | ✓ | ✓ | Fichier : ajouter. Dossier : ajouter une entrée (lien) |
| 2 | execute | ✓ | ✓ | Exécuter (droits sur le fichier ET le dossier) |
| 3 | overwrite | ✓ | — | Remplacer ou tronquer un fichier existant |
| 4 | delete | ✓ | ✓ | Dossier : retirer une entrée. Fichier : forcer la destruction |
| 5 | rename | ✓ | ✓ | Renommer le pointeur de nom dans la Micro-DB |
| 6 | access | — | ✓ | Lire la liste des entrées |
| 7 | link | ✓ | ✓ | Créer un Tag Link (partage) vers ce nœud |
| 8 | move | ✓ | ✓ | Déplacer le nœud (requiert `write` sur la destination) |
| 9 | copy | ✓ | ✓ | Cloner le nœud via CoW (requiert `write` sur la destination) |
| 10 | chmod | ✓ | ✓ | Modifier les permissions du nœud |
| 11 | own | ✓ | ✓ | Transférer la propriété du nœud |
| 12 | audit | ✓ | ✓ | (ADMIN) Consulter les logs d'accès du nœud |

## 4. Structures de données (proposées)

```rust
use bitflags::bitflags;

bitflags! {
    pub struct LifsPermissions: u16 {
        const READ      = 1 << 0;
        const WRITE     = 1 << 1;
        const EXECUTE   = 1 << 2;
        const OVERWRITE = 1 << 3;
        const DELETE    = 1 << 4;
        const RENAME    = 1 << 5;
        const ACCESS    = 1 << 6;
        const LINK      = 1 << 7;
        const MOVE      = 1 << 8;
        const COPY      = 1 << 9;
        const CHMOD     = 1 << 10;
        const OWN       = 1 << 11;
        const AUDIT     = 1 << 12;
    }
}

pub enum LinkType {
    CoW, // Default clone (Copy-on-Write)
    Tag, // Shared state (Multi-tagging)
}

pub struct LifsNode {
    pub node_id: u64,
    pub is_dir: bool,
    pub size: u64,
    pub tag_ref_count: u32,
    pub cow_ref_count: u32,
    pub owner_id: u32,
    pub permissions: u16,
    pub data_pointers: [u64; 12],
}

pub struct DirEntry {
    pub name_hash: u64,
    pub name: String,
    pub target_node_id: u64,
    pub link_type: LinkType,
}

pub struct DirNodeData {
    pub entries: Vec<DirEntry>, // In production, serialize as a B-Tree
}
```

## 5. Cycle de vie des nœuds et garbage collection

Comptage de références.

**A. Déplacement (move)** : suppression de la `DirEntry` dans la Micro-DB
source ; création de la `DirEntry` dans la Micro-DB destination ; les
compteurs restent inchangés.

**B. Suppression physique** : la `DirEntry` est retirée ; le noyau décrémente
`cow_ref_count` ou `tag_ref_count` selon le type de lien ; si
`cow_ref_count == 0` ET `tag_ref_count == 0`, le noyau désalloue les blocs et
détruit le `LifsNode`.

## 6. API noyau (syscalls LIFS)

| Syscall | Arguments | Description |
|---|---|---|
| `sys_lifs_create` | `(dir_id, name, is_dir, perms)` | Alloue un nouveau `LifsNode` et insère une `DirEntry` |
| `sys_lifs_move` | `(node_id, src_dir_id, dest_dir_id)` | Déplacement O(1) : supprime l'entrée source, crée l'entrée destination |
| `sys_lifs_clone` | `(node_id, dest_dir_id, new_name)` | Copier-coller (CoW). Instantané, +1 au `cow_ref_count` |
| `sys_lifs_tag` | `(node_id, dest_dir_id, new_name)` | Partage de fichier. Crée une entrée `LinkType::Tag` |
| `sys_lifs_unlink` | `(node_id, dir_id)` | Supprime le lien. Déclenche le GC si références = 0 |
| `sys_lifs_query_tags` | `(node_id)` | Retourne tous les dossiers contenant un Tag Link vers ce nœud |
