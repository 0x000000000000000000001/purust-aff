# Parallélisme CPU natif

## Démarrer les branches parallèles sans attendre leur première suspension

- [x] **Adopté pour le backend Rust `--threaded` : la même liberté d’ordre
  d’exécution entre branches parallèles que gopurs-aff.** Les feuilles de
  `ParAff` / `parTraverse`, `forkAff`, `launchAff`/`Fiber.run`, `joinFiber` et
  l’`acquire` de `supervise` soumettent leur démarrage à un pool CPU borné
  (`PURUST_AFF_WORKERS`, défaut `max(2, available_parallelism())`) au lieu de
  courir sur la pile de l’appelant. `suspendAff` reste suspendu jusqu’à
  observation.

Constat du 21 septembre 2026 (traité) : `aff_run_parallel` appelait
successivement `AffFiber::start`, qui exécutait chaque branche jusqu’à sa
première suspension ou sa terminaison. Deux calculs entièrement synchrones
restaient donc séquentiels. Gopurs-aff lance directement les branches dans des
goroutines ; il ne garantit pas leur ordre d’exécution relatif.

### Implémentation

- `src/Effect/Aff.rs` : `AffPool` borné (`Mutex<VecDeque>` + `Condvar`, threads
  `std::thread`, aucune dépendance nouvelle), créé dans `purust_aff_run_main` et
  possédé par `AffRuntime`. Chaque job entre le handle Tokio (pour que
  `tokio::spawn` fonctionne depuis la FFI) et `block_in_place` reste un blocage
  simple hors runtime. Arrêt propre : les jobs en file sont libérés au `Drop`.
- `AffFiber::begin` revendique la fibre une seule fois et incrémente `active`
  **au moment de la soumission** ; `start` soumet `drive` au pool. Une panique
  dans un job est enregistrée comme les autres et rompt l’attente du point
  d’entrée.
- `AffFiber::event(Kill)` garde son `drive` inline : une fibre non démarrée est
  annulée sans exécuter son corps, et une fibre déjà terminée ignore le kill.
- Les réveils, timers et reprises restent sur Tokio : les attentes asynchrones
  restent disponibles pendant les calculs CPU.
- `test/Test/Main.purs`, `Test/Concurrency.purs`, `Test/Lifetime.purs` : les
  assertions d’ordre entre fibres indépendantes sont remplacées par des
  invariants (multiset + sous-séquences par branche), les courses qui
  supposaient un démarrage synchrone sont synchronisées par gates `AVar`, et
  deux tests sont ajoutés (`par/traverse-order`, `scheduler`). Le test de
  rendez-vous de `Test.Concurrency` prouve que deux branches d’un même
  `ParAff` se chevauchent réellement, sans suspension.
- `bin/test` régénère tout le backend depuis les sources ; la suite complète
  (47 tests Aff, unitaires Rust, Concurrency, Lifetime + scénarios d’échec,
  9 scénarios d’error-reporting) passe.
- Robustesse : `Test.Main` et `Test.Concurrency` rejoués 5 fois chacun sans
  écart ; les scénarios d’erreur 4 et 6 rejoués 20 et 10 fois sans échec.
- Consommateurs externes vérifiés : `purust-js-promise-aff` (`runAff_` pour
  résoudre une Promise) et `purust-spec` (`forkAff` + handshake `AVar`) ne
  dépendent que de la terminaison d’une fibre, pas d’un démarrage synchrone.

### Contrat retenu

- Ordre de démarrage, d’exécution et de terminaison indéterminé entre branches
  explicitement parallèles et entre fibres indépendantes. Deux branches
  entièrement synchrones peuvent se chevaucher.
- Séquencement des actions à l’intérieur de chaque branche conservé.
- Association et ordre des résultats de `parTraverse` conservés, indépendamment
  de l’ordre de terminaison.
- Garanties d’erreur, d’annulation, de supervision et de nettoyage
  (`bracket` / finalizers) et gestion des paniques conservées. Une fibre
  annulée avant son démarrage n’exécute jamais son corps : un `bracket` ne
  lance son finalizer que pour une ressource acquise. Un calcul synchrone déjà
  commencé n’est pas interrompu de l’extérieur : la demande d’annulation est
  observée à la prochaine suspension.
- Pour les courses `Alt`, une branche perdante peut avoir déjà commencé (et
  même déjà produit son effet) ; aucune priorité de démarrage à gauche.

### Mesures

Machine : 14 cœurs, macOS arm64. Charge : `Test.ParBench`, 8 tâches
`parTraverse` CPU-bound sans suspension (500 000 pas `tailRecM` chacune),
médiane de 3 exécutions, `/usr/bin/time -l`, build debug.

| `PURUST_AFF_WORKERS` | temps mural | CPU utilisateur | RSS max |
| ---: | ---: | ---: | ---: |
| 1 | 5,86 s | 5,84 s | 5,5 Mo |
| 2 | 3,11 s | 6,20 s | 5,6 Mo |
| 4 | 1,74 s | 6,77 s | 6,0 Mo |
| 8 | 0,99 s | 7,09 s | 6,6 Mo |
| 14 | 1,01 s | 7,34 s | 6,9 Mo |

Accélération quasi linéaire jusqu’au nombre de tâches (8), puis plateau ; la
surconsommation CPU au-delà vient de la contention. Reproduire avec
`--main Test.ParBench` puis `PURUST_AFF_WORKERS=n /usr/bin/time -l`.

### Limites et suites possibles

- Le pool borne les démarrages, pas les reprises : une continuation après
  suspension reste sur le pool bloquant Tokio (jusqu’à 512 threads). Borner
  aussi les reprises demanderait un sémaphore supplémentaire.
- Un FFI qui bloque réellement (hors `makeAff`) immobilise un worker du pool ;
  c’est la contre-pression voulue, à documenter côté utilisateur.
- Répartition FIFO sans vol de travail : une très longue section synchrone peut
  retarder les branches suivantes.
- Avec `PURUST_AFF_WORKERS=1`, aucun chevauchement n’est possible (les tests de
  concurrence exigent ≥ 2 workers, comme le reste du runtime).
- `test/Test/Bench.purs` (minibench) ne mesure plus que le coût de lancement de
  `launchAff` ; à réécrire si on veut re-benchmarker les mêmes boucles.

Sources : [pool et démarrage](src/Effect/Aff.rs), [contrat](README.md#rust-backend),
[preuve de chevauchement](test/Test/Concurrency.purs).
