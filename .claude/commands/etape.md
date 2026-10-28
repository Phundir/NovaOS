---
description: Demarrer la prochaine etape de la roadmap Nova OS
argument-hint: "[numero d'etape optionnel, ex: 3.2]"
---

Etape demandee : $ARGUMENTS (si vide, prendre la premiere case non cochee).

Deroulement impose :

1. Lire `ROADMAP.md` et identifier precisement l'etape a traiter.
2. Lire les sections de `SPECIFICATION.md` que cette etape reference.
   Citer les numeros de section consultes.
3. Lire `DECISIONS.md` en entier — verifier qu'aucune decision verrouillee
   ni piege connu ne concerne ce qui va etre ecrit.
4. Lire `ARCHITECTURE.md` pour situer le code existant.
5. **Avant d'ecrire du code** : exposer en quelques lignes ce qui va etre fait,
   les decisions de conception non triviales et leurs raisons, et tout point ou
   la specification laisse un choix ouvert.
6. Implementer. Fichiers complets, pas de fragments a substituer.
7. Lancer `make`. Corriger jusqu'a zero erreur **et zero avertissement**.
8. Rendre la main : indiquer la commande de test exacte et **ce qu'il faut
   observer precisement** dans la sortie serie pour valider.
9. S'arreter la. Ne pas cocher la roadmap, ne pas commiter.

Ne jamais lancer QEMU : c'est l'utilisateur qui execute et renvoie la sortie.
