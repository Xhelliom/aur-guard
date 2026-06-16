# aur-guard

Garde-fou de sécurité pour les mises à jour AUR. Né après la compromission de
masse de l'AUR de juin 2026 : plutôt que d'installer aveuglément la dernière
version d'un paquet AUR, `aur-guard` applique une chaîne de décision avant
chaque mise à jour.

> 🌐 **Première visite ? Commencez par la page de présentation :
> [xhelliom.github.io/aur-guard](https://xhelliom.github.io/aur-guard/)** — une
> vue d'ensemble illustrée du projet, de ses interfaces et de sa philosophie.
> La suite de ce README est la documentation technique.

## Chaîne de décision

Pour chaque paquet AUR avec une mise à jour disponible :

1. **Whitelist** — paquets de confiance (binaires signés d'éditeurs réputés) :
   le délai est ignoré, mais le scan et la review IA s'appliquent quand même.
2. **Délai** — deux sémantiques (`delay_mode`) :
   - **`lag`** (défaut) : installe la révision du PKGBUILD qui était la `HEAD`
     du dépôt git AUR il y a `delay_days` jours (cette révision a été exposée à
     la communauté tout ce temps). Les mises à jour arrivent toujours, avec un
     retard constant — pas de blocage permanent des paquets à maj fréquente.
     L'AUR ne stockant aucun binaire, la révision est **buildée localement**
     (`git checkout <commit>` + `makepkg -si`).
   - **`hold`** : bloque toute maj dont la dernière version a moins de
     `delay_days` jours ; reste sur la version installée (plus strict, mais un
     paquet mis à jour plus souvent que le délai n'est jamais installé).
3. **Garde anti-revert** (mode lag) — une version vérolée reste dans l'historique
   git même après correction en place. On refuse donc une révision cible si elle
   a été **annulée/nettoyée depuis** : soit un commit postérieur évoque une
   compromission, soit un motif d'exécution dangereux (`| bash`, `base64 -d`,
   `/dev/tcp/`…) présent dans la cible a disparu de la `HEAD` actuelle.
4. **Scan statique** — délègue à [`aur-scan`](https://github.com/KiefStudioMA/ks-aur-scanner)
   s'il est installé (70+ règles, base d'IOC). Une détection bloquante → refus.
5. **Review IA** — envoie le *diff* du PKGBUILD à un LLM (Groq / OpenAI /
   Anthropic, configurable) qui juge `safe / suspect` avec justification.

Seuls les paquets qui passent les quatre étapes sont proposés à l'installation.

## Multi-vote IA (économie de frais)

La review IA n'appelle le modèle **qu'une fois** quand le paquet est jugé sûr
(cas courant). Un blocage déclenche des votes supplémentaires (jusqu'à
`confirm_votes` au total) et n'est confirmé qu'à la **majorité stricte** — ce qui
neutralise les faux positifs dus au non-déterminisme du modèle.

## Interfaces

Trois frontends partagent le même cœur :

- **CLI** — `aur-guard <commande>`
- **TUI** (terminal, ratatui) — `aur-guard config-ui`
- **GUI** (GTK4 / libadwaita) — binaire `aur-guard-gui` : réglages éditables +
  rapport des mises à jour (✅ sûr / ⏳ retardé / ⛔ bloqué) + installation.

aur-guard **ne gère que les paquets AUR** (le contenu non vérifié). Les dépôts
officiels d'Arch sont signés et hors de son périmètre. Pour éviter qu'on les
mette à jour à part (et qu'on contourne la review AUR avec un `yay -Syu`), la
commande `upgrade` enchaîne les deux : `pacman -Syu` puis les AUR sûrs.

```bash
aur-guard            # rapport (alias de `check`), n'installe rien
aur-guard check      # idem (+ rappel du nombre de maj officielles)
aur-guard upgrade    # dépôts officiels (pacman -Syu) PUIS paquets AUR sûrs
aur-guard apply      # uniquement les paquets AUR jugés sûrs
aur-guard apply --dry-run
aur-guard status     # âge (dernière modif AUR) de tous les paquets AUR installés
aur-guard config     # chemin + résumé de la configuration
aur-guard config-ui  # interface de paramétrage en terminal (TUI)
aur-guard install   # entrée de bureau + icône + traductions + timer de notification
aur-guard review-file <PKGBUILD>  # (debug) review IA d'un fichier
```

## Configuration

`~/.config/aur-guard/config.toml` (créé au premier lancement) :

```toml
delay_days = 14
delay_mode = "lag"     # lag | hold
helper = "yay"
use_aur_scan = true
whitelist = ["google-chrome", "zen-browser-bin", "..."]

[ai]
enabled = true
provider = "groq"      # groq | anthropic | openai
model = ""              # vide => modèle par défaut du provider
api_key_env = ""        # vide => GROQ_API_KEY / ANTHROPIC_API_KEY / OPENAI_API_KEY

[notify]
enabled = false             # timer systemd --user de notification de bureau
interval_hours = 6          # périodicité de la vérification
silent_when_up_to_date = true
```

La clé API n'est **jamais** stockée dans `config.toml`. Elle est résolue depuis
la variable d'environnement du provider en priorité, sinon depuis un fichier
dédié `~/.config/aur-guard/secrets.toml` (permissions `0600`), que l'on peut
renseigner depuis les interfaces (GUI/TUI).

## Interfaces de paramétrage

La GUI met les **mises à jour en page d'accueil** et regroupe les réglages dans
une **page plein écran** séparée (bouton engrenage → navigation) : délai/mode/helper/scan, review IA
(provider, **modèle**, **clé API**, votes) et **whitelist** (édition + suggestions
des paquets AUR installés), et les **notifications** (activation, intervalle).
La TUI (`aur-guard config-ui`) offre les mêmes réglages au clavier.

## Intégration au bureau et notifications

`aur-guard install` installe l'entrée de menu (`.desktop`), l'icône et les
traductions, puis met en place un timer systemd `--user`
(`aur-guard-notify.timer`) qui exécute périodiquement `aur-guard notify` :
celui-ci **compte** les mises à jour officielles et AUR disponibles (sans scan
ni review IA, donc sans coût d'API) et envoie une notification via `notify-send`.
L'activation et l'intervalle se règlent depuis la GUI/TUI ou la section
`[notify]` du `config.toml` ; toute sauvegarde des réglages resynchronise le
timer.

## Langues

L'interface (CLI, TUI, GUI) est multilingue via gettext et suit la **locale
système**. Anglais par défaut, français fourni. Pour installer les traductions :

```bash
po/install.sh            # compile po/*.po → ~/.local/share/locale/<lang>/…
```

## Build

```bash
# CLI + TUI + GUI (défaut ; nécessite gtk4 et libadwaita ≥ 1.4)
cargo build --release

# Tout-en-un : copie les binaires (~/.local/bin), pose l'entrée de menu +
# l'icône (Exec en chemin absolu), installe les traductions et le timer de
# notification. À lancer depuis l'arborescence buildée :
./target/release/aur-guard install

# Variante sans GUI (machine headless / CLI seule) :
cargo build --release --no-default-features --features tui
```

> Sans la GUI (`--no-default-features`), seul le binaire CLI est copié et
> l'entrée de menu est ignorée (le raccourci pointerait dans le vide).

## Limites

- Le délai retarde aussi des correctifs de sécurité légitimes → d'où la
  whitelist pour les paquets de confiance.
- Une compromission non détectée plus longtemps que `delay_days` passe au
  travers du délai (mais pas forcément du scan / de la review IA).
- La review IA dépend de la qualité du modèle ; elle complète, ne remplace pas,
  la lecture humaine du diff.
