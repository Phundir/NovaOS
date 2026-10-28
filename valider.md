---
description: Cloturer l'etape courante apres validation par la sortie serie
---

L'utilisateur a execute le test et renvoie la sortie. Sortie fournie :

$ARGUMENTS

Deroulement :

1. **Analyser la sortie** avant toute autre chose. Verifier point par point ce
   qui avait ete annonce comme critere de validation. Si un element manque ou
   diverge, le dire clairement et ne rien cocher.
2. Verifier les invariants permanents listes en fin de `docs/DECISIONS.md` :
   protection du kernel, une seule tache RUNNING par coeur, compteurs de tics
   qui progressent, allocations et liberations qui se rejoignent.
3. Si tout est conforme :
   - cocher les cases correspondantes dans `docs/ROADMAP.md`
   - mettre a jour le tableau d'avancement en tete de fichier
   - ajouter a la section « Limites connues » tout point que l'etape n'a pas
     traite et qu'on laisse ouvert
   - mettre a jour `docs/ARCHITECTURE.md` si un module, une region memoire, un
     vecteur d'interruption ou un appel systeme a ete ajoute
   - ajouter a `docs/DECISIONS.md` toute lecon durable : decision d'architecture
     prise pendant l'etape, ou piege rencontre et resolu
4. Proposer un message de commit en francais, sans accents, au format
   `N.N - description courte`.
5. Indiquer quelle est l'etape suivante.

Si la sortie revele un probleme, ne pas cocher : diagnostiquer et corriger
d'abord.
