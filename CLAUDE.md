# CLAUDE.md — guide de développement de aur-guard

Instructions pour toute personne (ou agent) qui modifie ce dépôt. À lire avant
de coder. Les règles ci-dessous **prévalent** sur les habitudes par défaut.

## Le projet en une phrase

Garde-fou de sécurité pour les mises à jour AUR : pour chaque paquet, une chaîne
de décision **whitelist → délai (lag/hold) → garde anti-revert → scan statique →
review IA** décide d'installer, de retarder ou de bloquer.

## Architecture

Cœur en bibliothèque (`src/lib.rs`, crate `aur_guard`), partagé par trois
frontends. **Toute la logique métier vit dans la lib ; les frontends ne font que
présenter et déléguer.**

| Module            | Responsabilité unique |
|-------------------|-----------------------|
| `config`          | Schéma de config, valeurs par défaut, (dé)sérialisation TOML |
| `aur`             | Tout l'I/O AUR : RPC, git, PKGBUILD, vercmp. **Aucun autre module ne parle HTTP/git.** |
| `scan`            | Délégation à `aur-scan` (analyse statique) |
| `ai`              | Review IA multi-provider + multi-vote |
| `pipeline`        | Orchestration de la chaîne de décision (peu d'I/O direct, appelle les autres) |
| `main` (bin)      | CLI |
| `tui` (feat `tui`)| Interface terminal (ratatui) |
| `bin/gui` (feat `gui`) | Interface GTK4/libadwaita |

Frontières : un détail d'implémentation (URL AUR, commande git, schéma JSON d'un
provider) ne doit **pas** fuiter hors de son module. Si `pipeline` a besoin d'une
donnée AUR, il appelle une fonction de `aur`, il ne construit pas d'URL.

## Commandes

```bash
cargo build                                       # CLI + TUI + GUI (défaut, gtk4 + libadwaita ≥ 1.4)
cargo build --no-default-features --features tui  # CLI + TUI, sans GUI
cargo build --no-default-features                 # cœur + CLI seuls (doit compiler)
cargo fmt
cargo clippy --all-targets
```

## Règles de code (obligatoires)

### Qualité — porte d'entrée avant tout commit
- `cargo fmt` appliqué, `cargo clippy --all-targets` **sans aucun warning**, et
  les trois combinaisons de features ci-dessus compilent. Un commit qui n'est pas
  fmt+clippy clean n'est pas prêt.

### DRY — n'écris pas deux fois la même logique
- Factorise dès la **règle de trois** (3ᵉ duplication). Exemples en place à
  réutiliser/imiter : `pipeline::vet()` (étape scan+IA commune aux deux chemins),
  `pipeline::outcome()` (construction d'`Outcome`), `aur::run_git()`.
- Une duplication de *structure* (même littéral de struct répété, même
  enchaînement scan→bloque→IA) est un signal de refactor, pas un style.

### Zéro magic number / magic string
- Toute valeur littérale porteuse de sens devient une **constante nommée**,
  proche de son usage, documentée. Modèles en place : `aur::SECS_PER_DAY`,
  `aur::RPC_BATCH`, `aur::AUR_HOST`, `aur::USER_AGENT`, `aur::DYNAMIC_VERSION`,
  `ai::MAX_TOKENS`, `ai::TEMPERATURE`, `main::SHORT_HASH_LEN`.
- Interdit : `86_400`, `"https://aur.archlinux.org/..."`, `512`, `7` écrits en
  dur dans la logique. Les URLs, en-têtes, tailles de lot, seuils → constantes.
- Une chaîne sentinelle (ex. `"?"` pour une version dynamique) est une constante
  partagée, jamais comparée à un littéral à distance de sa définition.

### Gestion d'erreur
- `anyhow::Result` + `.context(...)` pour situer l'échec. Propage avec `?`.
- **Aucun `unwrap()`/`expect()`/`panic!` dans la lib.** Tolérés uniquement dans
  les frontends pour un invariant réellement impossible, et alors commentés.
- Une opération réseau/git/process qui échoue ne doit jamais faire planter une
  évaluation : on logge (`eprintln!`) et on retombe sur une décision sûre.

### Sécurité — invariants non négociables (c'est un outil de sécurité)
- **Fail-closed** : tout doute (erreur, info manquante, version inconnue,
  ambiguïté) penche vers *retarder/bloquer*, jamais vers *installer*.
- Le scan et la review IA portent sur **exactement la révision qui sera
  installée** (en mode lag : la révision décalée, pas la dernière).
- Les secrets (clés API) ne vont **jamais** dans `config.toml`, les logs ou un
  message d'erreur. Résolution : variable d'environnement d'abord, sinon le
  fichier dédié `secrets.toml` écrit en `0600` (`config::Secrets` /
  `config::resolve_api_key`). Une UI ne pré-remplit jamais une clé existante.
- N'élargis pas une décision « autorisé » sans que les étapes en amont l'aient
  validée.

### Fonctions et lisibilité
- Une fonction = une responsabilité. Retours anticipés plutôt que nidification
  profonde. Si une fonction dépasse ~50 lignes ou imbrique > 3 niveaux, découpe.
- Noms explicites (le domaine en français est admis : `lagged_target`,
  `reverted_since`), `snake_case`, pas d'abréviations obscures.

### Commentaires
- Doc-comments `///` sur **tout élément public**, décrivant le contrat.
- Les commentaires expliquent le **pourquoi / l'intention / une contrainte**,
  pas la paraphrase du code. Pas de commentaire qui répète la ligne suivante.

### Frontends
- `main`, `tui`, `gui` ne contiennent **aucune** logique métier : ils lisent la
  config, appellent `pipeline`/`aur`, et affichent. Toute règle de décision se
  code (et se teste) dans la lib.
- Les features `tui` et `gui` restent optionnelles ; le cœur compile sans elles.

### Tests
- Les fonctions pures (parsing, comparaisons, heuristiques comme
  `danger_signatures`) méritent des tests `#[cfg(test)]`.
- Les flux à effet de bord (git, IA) s'éprouvent via les sous-commandes de debug
  (`review-file`, `revert-check`) sur des fixtures contrôlées.

### Dépendances
- Minimales et justifiées. Épingle les versions majeures. Préfère une fonction
  maison courte (cf. `unified_diff`, `urlencode`) à une dépendance lourde pour un
  besoin trivial.

### Commits
- Messages impératifs, préfixés `feat:`/`fix:`/`refactor:`/`docs:`/`chore:`,
  expliquant le **pourquoi**. Un commit = un changement cohérent.

## Internationalisation (i18n)
- Toute chaîne **visible par l'utilisateur** (CLI, TUI, GUI) passe par la macro
  `t!(...)` (gettext). Source en **anglais** (msgid) ; traductions dans
  `po/<lang>.po`. Interpolation positionnelle avec `{}` : `t!("Found {}", n)`.
- Après ajout/modif d'une chaîne `t!`, mettre à jour `po/fr.po` puis recompiler
  les `.mo` via `po/install.sh`.
- Les `.context()`/`bail!` internes et les commentaires de code restent en
  anglais (langue source du dépôt), non traduits.
