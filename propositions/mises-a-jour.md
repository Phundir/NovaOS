# Proposition : architecture des mises à jour — texte de l'utilisateur

Reçu le 24 septembre 2026 (PDF de 3 pages, « Architecture du Système de Mise
à Jour »). Contenu reproduit fidèlement, mise en page retirée. C'est une
**proposition** : son intégration est la phase 15 de `ROADMAP.md`, qui la
confronte à l'existant (SPM, spec §10 et §11).

---

## Résumé exécutif

Ce document détaille le fonctionnement du système de gestion des applications
de base du système d'exploitation. L'architecture repose sur un registre
décentralisé (GitHub), des installations atomiques isolées, et une bascule
instantanée par pointeurs au niveau du système de fichiers pour garantir un
« zéro-downtime » (zéro temps d'arrêt) lors des mises à jour.

## 1. Principes fondamentaux

- **Registre orienté manifeste** : un dépôt GitHub public hébergeant des
  fichiers JSON, source de vérité pour les versions, les signatures
  (checksums) et les canaux de distribution (Release, Beta, Test).
- **Installations atomiques (A/B)** : chaque mise à jour est une installation
  vierge dans un nouvel emplacement du système de fichiers. L'ancienne
  version n'est jamais écrasée en cours d'exécution. Si une coupure
  d'alimentation survient, l'installation est abandonnée et recommencée,
  laissant le système intact.
- **Séparation stricte exécutable/données** : les paramètres utilisateurs et
  les données générées par l'application sont stockés à part des binaires.
  Une mise à jour ne modifie ni ne lit les configurations utilisateur.

## 2. Détection et résolution des conflits

Pour éviter qu'une application et le gestionnaire système mettent à jour le
même logiciel en même temps, séparation stricte des privilèges :

- **Rôle de l'application** : uniquement vérifier de manière asynchrone (en
  lisant le JSON) si une mise à jour existe, et en informer l'utilisateur.
- **IPC** : si l'utilisateur accepte, l'application n'effectue aucune
  opération sur le disque ; elle envoie un signal IPC au gestionnaire de
  paquets de l'OS.
- **Gestionnaire système** : seul processus disposant des droits d'écriture
  sur la partition des applications. Il centralise les requêtes, télécharge
  en arrière-plan, vérifie l'intégrité (checksum) et prépare l'environnement
  atomique.

## 3. Structure du registre GitHub

Un dépôt GitHub avec un manifeste global à la racine et des sous-répertoires
par branche de distribution.

### 3.1 Format du manifeste JSON

```json
{ "app_name": "WebBrowserCore", "app_id": "com.os.webbrowser",
  "current_stable_version": "2.4.1",
  "channels": {
    "release": { "version": "2.4.1",
      "checksum": "sha256-e3b0c442...b855",
      "download_url": "https://github.com/.../releases/download/v2.4.1/browser.bin" },
    "beta": { "version": "2.5.0-b2",
      "checksum": "sha256-1234c442...b855",
      "download_url": "https://github.com/.../releases/download/v2.5.0-b2/browser.bin" },
    "test": { "version": "2.5.0-nightly",
      "checksum": "sha256-abcd...",
      "download_url": "https://github.com/.../releases/download/nightly/browser.bin" } } }
```

**Contournement des limites d'API** : les JSON sont téléchargés depuis
`raw.githubusercontent.com` ou GitHub Pages (fichier statique), pas par l'API
GitHub et ses limites de requêtes.

### 3.2 Intégrité des paquets

Avant toute installation, le gestionnaire compare le checksum du fichier
téléchargé à celui du JSON. S'ils diffèrent, l'installation est avortée et le
fichier temporaire supprimé.

## 4. Mécanique du système de fichiers (bascule instantanée)

Les raccourcis, le menu des applications et les associations de fichiers ne
pointent jamais vers un binaire physique, mais vers un **pointeur actif**
(symlink ou équivalent natif).

| Étape | Action du FS et du gestionnaire | État de l'application en cours |
|---|---|---|
| 1. Téléchargement | Écriture dans un répertoire temporaire isolé (ex. `/System/Apps/Browser.v2.tmp`) | La V1 tourne normalement |
| 2. Vérification | Calcul du checksum sur `Browser.v2.tmp`, validation | La V1 continue |
| 3. Bascule atomique | Mise à jour du pointeur global `Browser_Current` vers le dossier V2 | Le processus V1 en mémoire reste actif |
| 4. Nouvelle exécution | Le prochain lancement suit le pointeur | Nouveau processus V2 |

## 5. Nettoyage des anciennes versions

L'ancien binaire (V1) reste sur le disque tant que l'application n'est pas
fermée.

- **Surveillance par PID** : à la bascule, le gestionnaire enregistre le PID
  de l'ancienne version encore en cours.
- **Suppression différée** : à la fin de ce PID, l'ancien répertoire est
  supprimé.
- **Nettoyage au démarrage** : un daemon au boot purge tout dossier binaire
  qui n'est pas la cible actuelle d'un pointeur `App_Current`.

**Migration des paramètres** : ils sont hors du dossier d'installation ; c'est
la nouvelle version, à son premier lancement, qui lit l'ancien format et le
migre. Le système de mise à jour ne touche jamais aux données utilisateur.
