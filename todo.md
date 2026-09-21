# Parallélisme CPU natif

## Démarrer les branches parallèles sans attendre leur première suspension

- [ ] **Adopter, pour le backend Rust `--threaded`, la même liberté d’ordre d’exécution entre branches parallèles que gopurs-aff.** Les calculs CPU exécutés dans `ParAff` / `parTraverse` doivent pouvoir se chevaucher sans ajouter `delay 0`. Examiner également le démarrage par `forkAff`.

Constat du 21 septembre 2026 : `aff_run_parallel` appelle successivement `AffFiber::start`, qui exécute chaque branche jusqu’à sa première suspension ou sa terminaison. Deux calculs entièrement synchrones peuvent donc rester séquentiels. Les reprises après suspension peuvent déjà travailler simultanément. Gopurs-aff lance directement les branches dans des goroutines ; il ne garantit pas leur ordre d’exécution relatif.

Ce changement est prévu, **pas encore implémenté ni mesuré**. Aucun équivalent au mutex global de l’ancien `forceEscape` de gopurs n’a été trouvé dans le chemin de création des closures inspecté.

### Contrat visé

- Accepter un ordre de démarrage, d’exécution et de terminaison indéterminé entre branches explicitement parallèles.
- Conserver le séquencement des actions à l’intérieur de chaque branche.
- Conserver l’association et l’ordre des résultats de `parTraverse`, indépendamment de l’ordre de terminaison.
- Préserver les garanties d’erreur, d’annulation, de supervision et de nettoyage (`bracket` / finalizers), ainsi que la gestion des paniques.
- Pour les courses `Alt`, accepter qu’une branche perdante ait déjà commencé, même lorsqu’une autre réussit immédiatement. Ne pas conserver implicitement une priorité de démarrage à gauche.

### Étapes à traiter

1. **Preuve courte avant modification :** tracer deux calculs CPU synchrones indépendants dans `parTraverse`, sans suspension, puis comparer avec le chemin actuel après `delay 0`. Mesurer leur chevauchement effectif ; deux identifiants de threads différents ne suffisent pas à le prouver.
2. **Prototype de démarrage concurrent :** examiner `AffFiber::start` et `aff_run_parallel`. Utiliser des workers en nombre borné et une gestion des tâches adaptée aux calculs CPU ; éviter un thread système par branche et les blocages entre tâches parallèles imbriquées. Garder les attentes asynchrones disponibles pendant ces calculs.
3. **Validation du contrat :** couvrir résultats dans l’ordre malgré des terminaisons inversées, séquencement interne, concurrence imbriquée, erreurs, courses synchrones/asynchrones, annulation avant et après démarrage, supervision, finalizers et paniques. Distinguer demande d’annulation et interruption effective d’un calcul CPU synchrone.
4. **Mesures contrôlées :** comparer 1/2/4 workers sur les mêmes calculs, avec répétitions, temps mural, CPU et mémoire. Vérifier les résultats et les régressions Aff existantes. Aucun gain chiffré promis avant ces mesures.
5. **Documenter le comportement retenu :** mettre à jour le README et les tests qui supposent actuellement un démarrage synchrone jusqu’à la première suspension. Le périmètre est le backend Rust multithread ; les garanties des autres backends ne changent pas implicitement.

Sources du constat : [démarrage et ParAff](src/Effect/Aff.rs), [contrat actuel](README.md#rust-backend), [tests de reprises concurrentes](test/Test/Concurrency.purs).
