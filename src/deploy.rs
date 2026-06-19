//! Intégration système : entrée de bureau, icône, catalogues de traduction et
//! le timer systemd `--user` qui déclenche les notifications de mise à jour.
//!
//! Tout l'I/O d'installation (écriture de fichiers dans `~/.local/share`,
//! `~/.config/systemd/user`, appels à `systemctl`/`msgfmt`/`notify-send`) est
//! centralisé ici : les frontends ne font qu'appeler ces fonctions et présenter
//! le résultat.

use crate::config::{Config, NotifyConfig};
use crate::{aur, t};
use anyhow::{Context, Result};
use std::path::PathBuf;
use std::process::{Command, Stdio};

/// Identifiant d'application (entrée de bureau, icône, métadonnées).
const APP_ID: &str = "fr.xhelliom.AurGuard";
/// Nom du binaire CLI.
const BIN_CLI: &str = "aur-guard";
/// Nom du binaire GUI (lancé par l'entrée de bureau).
const BIN_GUI: &str = "aur-guard-gui";
/// Permissions des binaires installés (rwxr-xr-x).
const BIN_MODE: u32 = 0o755;
/// Domaine gettext (doit correspondre à `i18n::init`).
const GETTEXT_DOMAIN: &str = "aur-guard";
/// Nom de base des unités systemd de notification (`.service` / `.timer`).
const NOTIFY_UNIT: &str = "aur-guard-notify";
/// Délai après le démarrage avant la première vérification.
const NOTIFY_BOOT_DELAY: &str = "2min";

/// Entrée de bureau et icône, embarquées dans le binaire pour que la commande
/// `install` soit autonome (pas besoin de l'arborescence source à l'exécution).
const DESKTOP_BYTES: &[u8] = include_bytes!("../data/fr.xhelliom.AurGuard.desktop");
const ICON_BYTES: &[u8] = include_bytes!("../data/fr.xhelliom.AurGuard.svg");

/// Catalogues de traduction source, compilés à l'installation via `msgfmt`.
/// `(code_langue, contenu_po)`.
const CATALOGS: &[(&str, &str)] = &[("fr", include_str!("../po/fr.po"))];

/// Racine des données utilisateur (`$XDG_DATA_HOME` ou `~/.local/share`).
fn data_home() -> Result<PathBuf> {
    dirs::data_dir().context("impossible de résoudre ~/.local/share")
}

/// Répertoire des unités systemd utilisateur (`~/.config/systemd/user`).
fn systemd_user_dir() -> Result<PathBuf> {
    let base = dirs::config_dir().context("impossible de résoudre ~/.config")?;
    Ok(base.join("systemd/user"))
}

/// Chemin absolu du binaire courant, pour le baker dans l'unité systemd.
fn current_exe() -> String {
    std::env::current_exe()
        .ok()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| GETTEXT_DOMAIN.to_string())
}

/// Copie les binaires (`aur-guard`, et `aur-guard-gui` s'il a été compilé) dans
/// le répertoire des exécutables utilisateur (`~/.local/bin`).
///
/// Renvoie `true` si le binaire GUI est disponible à l'arrivée (fraîchement
/// copié ou déjà présent) : l'appelant ne pose l'entrée de menu que dans ce cas,
/// pour ne pas créer un raccourci pointant vers un binaire absent.
pub fn install_binaries() -> Result<bool> {
    let dest_dir = dirs::executable_dir().context("impossible de résoudre ~/.local/bin")?;
    std::fs::create_dir_all(&dest_dir)
        .with_context(|| format!("création de {}", dest_dir.display()))?;
    let src_dir = std::env::current_exe()
        .context("résolution du binaire courant")?
        .parent()
        .map(|p| p.to_path_buf())
        .context("le binaire courant n'a pas de répertoire parent")?;

    install_one_binary(&src_dir, &dest_dir, BIN_CLI)?;
    let gui_dest = dest_dir.join(BIN_GUI);
    install_one_binary(&src_dir, &dest_dir, BIN_GUI)?;
    Ok(gui_dest.exists())
}

/// Copie un binaire depuis `src_dir` vers `dest_dir` s'il existe à la source et
/// que ce n'est pas déjà le même fichier (copier un binaire sur lui-même le
/// tronquerait). Absence à la source = rien à faire (pas une erreur).
fn install_one_binary(
    src_dir: &std::path::Path,
    dest_dir: &std::path::Path,
    name: &str,
) -> Result<()> {
    let src = src_dir.join(name);
    if !src.exists() {
        return Ok(());
    }
    let dest = dest_dir.join(name);
    // Même chemin (on tourne déjà depuis ~/.local/bin) : ne rien copier.
    if std::fs::canonicalize(&src).ok() == std::fs::canonicalize(&dest).ok() && dest.exists() {
        return Ok(());
    }
    // Écriture temporaire puis rename atomique : remplacer directement un binaire
    // en cours d'exécution échouerait (ETXTBSY) ; le rename ne touche que l'entrée
    // de répertoire, le process en cours garde son ancien inode.
    let tmp = dest_dir.join(format!(".{name}.new"));
    std::fs::copy(&src, &tmp)
        .with_context(|| format!("copie de {} vers {}", src.display(), tmp.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(BIN_MODE))?;
    }
    if let Err(e) = std::fs::rename(&tmp, &dest) {
        let _ = std::fs::remove_file(&tmp); // nettoyage best-effort
        return Err(e).with_context(|| format!("installation de {}", dest.display()));
    }
    Ok(())
}

/// Chemin absolu d'un binaire installé dans `~/.local/bin`, ou son nom nu en
/// dernier recours (binaire absent / `~/.local/bin` non résolu). Les lanceurs
/// graphiques — et `bash -c` non interactif — n'ont souvent pas `~/.local/bin`
/// dans leur PATH : un nom nu ne s'y résoudrait pas, d'où la préférence absolue.
fn installed_binary(name: &str) -> String {
    dirs::executable_dir()
        .map(|d| d.join(name))
        .filter(|p| p.exists())
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| name.to_string())
}

/// Commande à utiliser pour lancer le binaire CLI `aur-guard`, sous forme de
/// chemin absolu quand il est installé. Destinée aux frontends qui démarrent le
/// CLI dans un terminal externe, dont le PATH n'inclut pas `~/.local/bin`.
pub fn cli_command() -> String {
    installed_binary(BIN_CLI)
}

/// Installe l'entrée de bureau et l'icône dans `~/.local/share`.
///
/// Rend l'application visible dans le menu et associe son icône. La ligne `Exec`
/// embarquée (`aur-guard-gui` nu) est réécrite avec le **chemin absolu** du
/// binaire installé : les lanceurs graphiques n'ont souvent pas `~/.local/bin`
/// dans leur PATH, une commande nue ne s'y résoudrait pas.
pub fn install_desktop_entry() -> Result<()> {
    let share = data_home()?;
    let desktop = share.join("applications").join(format!("{APP_ID}.desktop"));

    let gui_path = installed_binary(BIN_GUI);
    let content = String::from_utf8_lossy(DESKTOP_BYTES)
        .replace(&format!("Exec={BIN_GUI}"), &format!("Exec={gui_path}"));
    write_file(&desktop, content.as_bytes())?;

    let icon = share
        .join("icons/hicolor/scalable/apps")
        .join(format!("{APP_ID}.svg"));
    write_file(&icon, ICON_BYTES)?;
    Ok(())
}

/// Compile et installe les catalogues de traduction (`msgfmt`).
///
/// L'absence de `msgfmt` n'est pas fatale : on avertit et on continue (les
/// chaînes retombent alors sur l'anglais source).
pub fn install_locales() -> Result<()> {
    let share = data_home()?;
    for (lang, po) in CATALOGS {
        let dest = share
            .join("locale")
            .join(lang)
            .join("LC_MESSAGES")
            .join(format!("{GETTEXT_DOMAIN}.mo"));
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("création de {}", parent.display()))?;
        }
        // `msgfmt - -o <dest>` lit le .po sur stdin : pas de fichier temporaire.
        let res = Command::new("msgfmt")
            .arg("-")
            .arg("-o")
            .arg(&dest)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .spawn()
            .and_then(|mut child| {
                use std::io::Write;
                if let Some(stdin) = child.stdin.take() {
                    let mut stdin = stdin;
                    stdin.write_all(po.as_bytes())?;
                }
                child.wait()
            });
        match res {
            Ok(status) if status.success() => {}
            Ok(_) => eprintln!("{}", t!("msgfmt failed for locale {}", lang)),
            Err(_) => {
                eprintln!("{}", t!("msgfmt not found — skipping translations"));
                break;
            }
        }
    }
    Ok(())
}

/// Écrit (ou rafraîchit) les unités systemd de notification puis active ou
/// désactive le timer selon `cfg.enabled`.
///
/// Le service exécute `<exe> notify` : toute la logique de notification est en
/// Rust, rien n'est baké en dur dans une chaîne shell.
pub fn apply_notify(cfg: &NotifyConfig) -> Result<()> {
    let dir = systemd_user_dir()?;
    std::fs::create_dir_all(&dir).with_context(|| format!("création de {}", dir.display()))?;
    let exe = current_exe();

    let service = format!(
        "[Unit]\n\
         Description=aur-guard update notification\n\n\
         [Service]\n\
         Type=oneshot\n\
         ExecStart={exe} notify\n"
    );
    let interval = cfg.interval_hours.max(1);
    let timer = format!(
        "[Unit]\n\
         Description=aur-guard periodic update check\n\n\
         [Timer]\n\
         OnBootSec={NOTIFY_BOOT_DELAY}\n\
         OnUnitActiveSec={interval}h\n\
         Persistent=true\n\n\
         [Install]\n\
         WantedBy=timers.target\n"
    );
    write_file(
        &dir.join(format!("{NOTIFY_UNIT}.service")),
        service.as_bytes(),
    )?;
    write_file(&dir.join(format!("{NOTIFY_UNIT}.timer")), timer.as_bytes())?;

    run_systemctl(&["daemon-reload"]);
    let timer_unit = format!("{NOTIFY_UNIT}.timer");
    if cfg.enabled {
        run_systemctl(&["enable", "--now", &timer_unit]);
    } else {
        run_systemctl(&["disable", "--now", &timer_unit]);
    }
    Ok(())
}

/// Calcule les compteurs de mises à jour et envoie une notification de bureau.
///
/// Léger par construction : compte les maj officielles et AUR *disponibles*
/// sans déclencher de scan ni de review IA (donc aucun coût d'API sur le timer).
pub fn send_notification(cfg: &Config) -> Result<()> {
    let official = aur::official_updates().len();
    let aur = aur::list_updates(&cfg.helper).map(|u| u.len()).unwrap_or(0);

    if official > 0 || aur > 0 {
        notify_send(
            "normal",
            &t!("Updates available"),
            &t!("{} repo + {} AUR (aur-guard upgrade)", official, aur),
        );
    } else if !cfg.notify.silent_when_up_to_date {
        notify_send("low", &t!("System up to date"), &t!("No updates"));
    }
    Ok(())
}

/// Envoie immédiatement une notification de test, **toujours visible** (quel que
/// soit l'état des mises à jour), pour vérifier que `notify-send` et le démon de
/// notifications du bureau fonctionnent.
pub fn send_test_notification() {
    notify_send(
        "normal",
        "aur-guard",
        &t!("Test notification — if you see this, notifications work."),
    );
}

/// Écrit un fichier en créant ses répertoires parents au besoin.
fn write_file(path: &std::path::Path, bytes: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("création de {}", parent.display()))?;
    }
    std::fs::write(path, bytes).with_context(|| format!("écriture de {}", path.display()))
}

/// Appelle `systemctl --user <args>` ; un échec est journalisé, jamais fatal.
fn run_systemctl(args: &[&str]) {
    let res = Command::new("systemctl").arg("--user").args(args).status();
    if let Ok(status) = res {
        if !status.success() {
            eprintln!("{}", t!("systemctl --user {} failed", args.join(" ")));
        }
    } else {
        eprintln!(
            "{}",
            t!("systemctl not found — notification timer not applied")
        );
    }
}

/// Appelle `notify-send` ; l'absence de l'outil n'est pas fatale.
fn notify_send(urgency: &str, title: &str, body: &str) {
    let _ = Command::new("notify-send")
        .args(["-u", urgency, title, body])
        .status();
}
