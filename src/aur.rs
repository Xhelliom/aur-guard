//! Interactions avec l'AUR : liste des mises à jour, date de dernière
//! modification (API RPC) et récupération du diff de PKGBUILD.

use anyhow::{bail, Context, Result};
use serde::Deserialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

/// Nombre de secondes dans une journée.
pub const SECS_PER_DAY: u64 = 86_400;
/// Marqueur de version non résolvable statiquement (paquets VCS, ex. `-git`).
pub const DYNAMIC_VERSION: &str = "?";

/// Hôte de l'AUR (API RPC, dépôts git, PKGBUILD bruts).
const AUR_HOST: &str = "https://aur.archlinux.org";
/// User-Agent envoyé aux requêtes HTTP vers l'AUR.
const USER_AGENT: &str = "aur-guard";
/// Nombre maximum de paquets par requête RPC (limite la longueur d'URL).
const RPC_BATCH: usize = 50;

/// Une mise à jour AUR disponible.
#[derive(Debug, Clone)]
pub struct Update {
    pub name: String,
    pub old_ver: String,
    pub new_ver: String,
}

/// Horodatage Unix courant (secondes).
pub fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Liste les mises à jour AUR via `<helper> -Qua`.
/// Sortie attendue : "nom ancienne -> nouvelle".
pub fn list_updates(helper: &str) -> Result<Vec<Update>> {
    let out = Command::new(helper)
        .args(["-Qua"])
        .output()
        .with_context(|| format!("exécution de `{helper} -Qua`"))?;
    // -Qua renvoie un code non-zéro quand il n'y a aucune maj : on ignore.
    let text = String::from_utf8_lossy(&out.stdout);
    let mut updates = Vec::new();
    for line in text.lines() {
        let parts: Vec<&str> = line.split_whitespace().collect();
        // format: name old -> new
        if parts.len() >= 4 && parts[2] == "->" {
            updates.push(Update {
                name: parts[0].to_string(),
                old_ver: parts[1].to_string(),
                new_ver: parts[3].to_string(),
            });
        } else if !parts.is_empty() {
            updates.push(Update {
                name: parts[0].to_string(),
                old_ver: String::new(),
                new_ver: String::new(),
            });
        }
    }
    Ok(updates)
}

/// Métadonnées AUR utiles d'un paquet.
#[derive(Debug, Clone)]
pub struct PkgInfo {
    pub package_base: String,
    pub last_modified: u64,
}

#[derive(Deserialize)]
struct RpcInfo {
    #[serde(rename = "Name")]
    name: String,
    #[serde(rename = "PackageBase")]
    package_base: String,
    #[serde(rename = "LastModified")]
    last_modified: u64,
}

#[derive(Deserialize)]
struct RpcResponse {
    results: Vec<RpcInfo>,
}

/// Récupère les métadonnées (PackageBase, LastModified) via l'API RPC v5.
pub fn fetch_infos(names: &[String]) -> Result<HashMap<String, PkgInfo>> {
    let mut map = HashMap::new();
    if names.is_empty() {
        return Ok(map);
    }
    // L'API accepte plusieurs arg[]= ; on découpe par lots pour éviter une URL
    // trop longue.
    for chunk in names.chunks(RPC_BATCH) {
        let query: String = chunk
            .iter()
            .map(|n| format!("arg[]={}", urlencode(n)))
            .collect::<Vec<_>>()
            .join("&");
        let url = format!("{AUR_HOST}/rpc/v5/info?{query}");
        let resp: RpcResponse = ureq::get(&url)
            .set("User-Agent", USER_AGENT)
            .call()
            .with_context(|| "appel API AUR RPC")?
            .into_json()
            .context("parsing JSON RPC")?;
        for info in resp.results {
            map.insert(
                info.name,
                PkgInfo {
                    package_base: info.package_base,
                    last_modified: info.last_modified,
                },
            );
        }
    }
    Ok(map)
}

/// Liste les mises à jour des dépôts officiels via `checkupdates`
/// (format « nom ancienne -> nouvelle »). Ces paquets sont signés et hors du
/// périmètre de review d'aur-guard.
pub fn official_updates() -> Vec<String> {
    Command::new("checkupdates")
        .output()
        .ok()
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .map(|l| l.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

/// Liste les paquets AUR (« foreign ») installés via `pacman -Qmq`.
pub fn installed_aur_packages() -> Vec<String> {
    Command::new("pacman")
        .args(["-Qmq"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .map(|l| l.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

/// Map nom -> timestamp `LastModified` (utilisé par la commande `status`).
pub fn last_modified(names: &[String]) -> Result<HashMap<String, u64>> {
    Ok(fetch_infos(names)?
        .into_iter()
        .map(|(k, v)| (k, v.last_modified))
        .collect())
}

/// Encodage URL minimal (les noms de paquets AUR contiennent rarement des
/// caractères spéciaux, mais on gère `+` et quelques autres).
fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{:02X}", b)),
        }
    }
    out
}

/// Télécharge le PKGBUILD courant d'un paquet depuis l'AUR.
pub fn fetch_remote_pkgbuild(name: &str) -> Result<String> {
    let url = format!(
        "{AUR_HOST}/cgit/aur.git/plain/PKGBUILD?h={}",
        urlencode(name)
    );
    let body = ureq::get(&url)
        .set("User-Agent", USER_AGENT)
        .call()
        .with_context(|| format!("téléchargement du PKGBUILD de {name}"))?
        .into_string()
        .context("lecture du corps PKGBUILD")?;
    Ok(body)
}

/// Tente de localiser le PKGBUILD local (version actuellement installée),
/// dans le cache du helper. Renvoie son contenu si trouvé.
pub fn local_pkgbuild(name: &str) -> Option<String> {
    let home = dirs::home_dir()?;
    let candidates = [
        home.join(".cache/yay").join(name).join("PKGBUILD"),
        home.join(".cache/paru/clone").join(name).join("PKGBUILD"),
    ];
    for path in candidates {
        if let Ok(text) = std::fs::read_to_string(&path) {
            return Some(text);
        }
    }
    None
}

/// Construit un diff unifié entre le PKGBUILD local (installé) et le distant.
/// Si le local est introuvable, renvoie le PKGBUILD distant entier annoté
/// comme « première inspection ».
pub fn pkgbuild_diff(name: &str) -> Result<String> {
    let remote = fetch_remote_pkgbuild(name)?;
    match local_pkgbuild(name) {
        Some(local) if local.trim() == remote.trim() => Ok(String::new()),
        Some(local) => Ok(unified_diff(&local, &remote, name)),
        None => Ok(format!(
            "# Pas de PKGBUILD local de référence — inspection complète du PKGBUILD distant :\n{remote}"
        )),
    }
}

// =====================================================================
// Mode LAG : installer la révision du PKGBUILD qui était la HEAD il y a N
// jours, via l'historique git du dépôt AUR.
// =====================================================================

/// Révision « décalée » ciblée par le mode lag.
#[derive(Debug, Clone)]
pub struct LagTarget {
    pub pkgbase: String,
    pub commit: String,
    /// Version (epoch:pkgver-pkgrel) à ce commit, ou "?" si dynamique (VCS).
    pub version: String,
    /// Horodatage Unix (secondes) du commit cible : permet d'afficher l'âge réel
    /// de la révision qui sera installée. 0 si la date n'a pas pu être lue.
    pub committed_at: u64,
    /// Contenu du PKGBUILD à ce commit (pour la review).
    pub pkgbuild: String,
}

fn aur_cache_dir() -> Result<PathBuf> {
    let base = dirs::cache_dir().context("cache dir introuvable")?;
    Ok(base.join("aur-guard").join("git"))
}

fn run_git(dir: &Path, args: &[&str]) -> Result<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .with_context(|| format!("git {args:?}"))?;
    if !out.status.success() {
        bail!(
            "git {:?} : {}",
            args,
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

/// Clone (ou met à jour) le dépôt git AUR du pkgbase et renvoie son chemin.
pub fn ensure_git_repo(pkgbase: &str) -> Result<PathBuf> {
    let dir = aur_cache_dir()?.join(pkgbase);
    if dir.join(".git").exists() {
        run_git(&dir, &["fetch", "--quiet", "origin"])?;
    } else {
        std::fs::create_dir_all(dir.parent().unwrap())?;
        let url = format!("{AUR_HOST}/{pkgbase}.git");
        let out = Command::new("git")
            .args(["clone", "--quiet", &url])
            .arg(&dir)
            .output()
            .context("git clone")?;
        if !out.status.success() {
            bail!(
                "clone de {url} : {}",
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
    }
    Ok(dir)
}

/// Détermine la révision qui était la HEAD avant `before_epoch`.
pub fn lagged_target(pkgbase: &str, before_epoch: u64) -> Result<Option<LagTarget>> {
    let dir = ensure_git_repo(pkgbase)?;
    let before = format!("--before=@{before_epoch}");
    // origin/HEAD pointe vers la branche par défaut (master sur l'AUR).
    let commit = run_git(&dir, &["rev-list", "-1", &before, "origin/HEAD"])
        .or_else(|_| run_git(&dir, &["rev-list", "-1", &before, "origin/master"]))?
        .trim()
        .to_string();
    if commit.is_empty() {
        return Ok(None); // le paquet n'existait pas encore il y a N jours
    }
    let pkgbuild = run_git(&dir, &["show", &format!("{commit}:PKGBUILD")]).unwrap_or_default();
    let version = parse_version(&pkgbuild);
    // Date du commit cible : `%ct` = horodatage Unix du committer.
    let committed_at = run_git(&dir, &["show", "-s", "--format=%ct", &commit])
        .ok()
        .and_then(|s| s.trim().parse::<u64>().ok())
        .unwrap_or(0);
    Ok(Some(LagTarget {
        pkgbase: pkgbase.to_string(),
        commit,
        version,
        committed_at,
        pkgbuild,
    }))
}

/// Extrait la version d'un PKGBUILD (statique uniquement).
fn parse_version(pkgbuild: &str) -> String {
    let pick = |key: &str| -> String {
        pkgbuild
            .lines()
            .find_map(|l| l.trim().strip_prefix(key))
            .map(|v| v.trim().trim_matches('\'').trim_matches('"').to_string())
            .unwrap_or_default()
    };
    let ver = pick("pkgver=");
    if ver.is_empty() || ver.contains('$') {
        return DYNAMIC_VERSION.to_string(); // version dynamique (VCS) : non gérée en lag
    }
    let rel = pick("pkgrel=");
    let epoch = pick("epoch=");
    let base = if rel.is_empty() {
        ver
    } else {
        format!("{ver}-{rel}")
    };
    if epoch.is_empty() {
        base
    } else {
        format!("{epoch}:{base}")
    }
}

/// Construit et installe la révision décalée (checkout + makepkg -si).
/// Retourne true si l'installation a réussi.
pub fn install_lagged(target: &LagTarget) -> Result<bool> {
    let dir = ensure_git_repo(&target.pkgbase)?;
    run_git(&dir, &["checkout", "--quiet", &target.commit])?;
    let status = Command::new("makepkg")
        .args(["-si", "--noconfirm"])
        .current_dir(&dir)
        .status()
        .context("lancement de makepkg")?;
    // Revient sur la branche par défaut pour les prochains fetch.
    let _ = run_git(&dir, &["checkout", "--quiet", "origin/HEAD"])
        .or_else(|_| run_git(&dir, &["checkout", "--quiet", "master"]));
    Ok(status.success())
}

/// Compare deux versions via l'outil `vercmp` de pacman.
/// Renvoie >0 si `a` est strictement plus récent que `b`, 0 si égal, <0 sinon.
///
/// Fail-closed : si `vercmp` est indisponible ou sa sortie illisible, renvoie une
/// valeur négative — jamais « plus récent ». Aucun appelant ne considère donc une
/// version comme installable faute de comparaison fiable.
pub fn vercmp(a: &str, b: &str) -> i32 {
    match Command::new("vercmp").args([a, b]).output() {
        Ok(out) if out.status.success() => String::from_utf8_lossy(&out.stdout)
            .trim()
            .parse()
            .unwrap_or(-1),
        _ => {
            eprintln!(
                "  (vercmp indisponible : comparaison de versions impossible, mise à jour ignorée)"
            );
            -1
        }
    }
}

/// Nombre maximum de commits inspectés en remontant l'historique pour dater la
/// prochaine révision installable (borne le coût sur un paquet à long historique).
const MAX_UPGRADE_SCAN: usize = 200;

/// Prochaine révision lag installable : la PLUS ANCIENNE plus récente que la
/// version installée. C'est elle qui mûrira en premier et sera réellement posée.
#[derive(Debug, Clone)]
pub struct NextUpgrade {
    /// Date de commit (Unix s) : l'échéance = `committed_at + délai`.
    pub committed_at: u64,
    /// Version de cette révision (ce qui sera installé à l'échéance).
    pub version: String,
}

/// Prochaine révision strictement plus récente que `installed` dans l'historique
/// git (la plus ancienne du lot), avec sa date et sa version. `None` si rien
/// n'est plus récent.
///
/// Contrairement à `last_modified` (qui suit la *dernière* publication et repart
/// à zéro à chaque nouveau commit), cette donnée est ancrée sur un commit précis :
/// une publication ultérieure ne repousse pas une échéance déjà acquise, et la
/// version annoncée est bien celle qui sera posée. Réutilise le dépôt git déjà
/// présent (pas de fetch) ; appelée après `lagged_target`.
pub fn next_upgrade(pkgbase: &str, installed: &str) -> Result<Option<NextUpgrade>> {
    let dir = aur_cache_dir()?.join(pkgbase);
    if !dir.join(".git").exists() {
        ensure_git_repo(pkgbase)?;
    }
    // Commits du plus récent au plus ancien, avec leur date de commit (%ct).
    let log = run_git(&dir, &["log", "--format=%H %ct", "origin/HEAD"])
        .or_else(|_| run_git(&dir, &["log", "--format=%H %ct", "master"]))?;

    let mut candidate = None;
    for line in log.lines().take(MAX_UPGRADE_SCAN) {
        let mut parts = line.split_whitespace();
        let (Some(commit), Some(ts)) = (parts.next(), parts.next()) else {
            continue;
        };
        let pkgbuild = run_git(&dir, &["show", &format!("{commit}:PKGBUILD")]).unwrap_or_default();
        let version = parse_version(&pkgbuild);
        if version == DYNAMIC_VERSION {
            continue; // révision VCS : pas de version comparable
        }
        if vercmp(&version, installed) > 0 {
            // On descend : on retient à chaque fois la révision la plus ancienne.
            candidate = ts.parse::<u64>().ok().map(|committed_at| NextUpgrade {
                committed_at,
                version,
            });
        } else {
            break; // on a rejoint la version installée (ou antérieure)
        }
    }
    Ok(candidate)
}

/// Motifs d'exécution de code distant / reverse shell dans un PKGBUILD.
/// Renvoie les libellés des motifs détectés.
fn danger_signatures(pkgbuild: &str) -> Vec<&'static str> {
    let low = pkgbuild.to_lowercase();
    let compact = low
        .replace(" | ", "|")
        .replace("| ", "|")
        .replace(" |", "|");
    let checks: [(&str, &str); 9] = [
        ("|bash", "pipe vers bash"),
        ("|sh", "pipe vers sh"),
        ("base64 -d", "décodage base64"),
        ("base64 --decode", "décodage base64"),
        ("/dev/tcp/", "reverse shell /dev/tcp"),
        ("eval \"$(", "eval de commande"),
        ("eval $(", "eval de commande"),
        ("nc -e", "reverse shell netcat"),
        ("curl -s http", "téléchargement opaque curl"),
    ];
    let mut hits = Vec::new();
    for (pat, label) in checks {
        if compact.contains(pat) && !hits.contains(&label) {
            hits.push(label);
        }
    }
    hits
}

/// Garde anti-« version vérolée restée dans l'historique » : la révision cible
/// a-t-elle été annulée/nettoyée depuis ? Renvoie Some(raison) si suspect.
///
/// Deux signaux :
///   A. un commit postérieur à `commit` mentionne une compromission ;
///   B. un motif d'exécution dangereux présent dans la révision cible a
///      disparu de la HEAD actuelle (signe d'un nettoyage post-incident).
pub fn reverted_since(pkgbase: &str, commit: &str) -> Result<Option<String>> {
    let dir = aur_cache_dir()?.join(pkgbase);
    if !dir.join(".git").exists() {
        ensure_git_repo(pkgbase)?;
    }
    let target = run_git(&dir, &["show", &format!("{commit}:PKGBUILD")]).unwrap_or_default();
    let head = run_git(&dir, &["show", "origin/HEAD:PKGBUILD"])
        .or_else(|_| run_git(&dir, &["show", "master:PKGBUILD"]))
        .unwrap_or_default();

    // B. motif dangereux retiré depuis (signal fort, peu de faux positifs).
    let target_sigs = danger_signatures(&target);
    if !target_sigs.is_empty() {
        let head_sigs = danger_signatures(&head);
        let removed: Vec<&str> = target_sigs
            .into_iter()
            .filter(|s| !head_sigs.contains(s))
            .collect();
        if !removed.is_empty() {
            return Ok(Some(format!(
                "motif dangereux présent dans la révision cible mais retiré depuis : {}",
                removed.join(", ")
            )));
        }
    }

    // A. message d'un commit postérieur évoquant une compromission.
    let log = run_git(
        &dir,
        &["log", "--format=%s %b", &format!("{commit}..origin/HEAD")],
    )
    .or_else(|_| {
        run_git(
            &dir,
            &["log", "--format=%s %b", &format!("{commit}..master")],
        )
    })
    .unwrap_or_default();
    let low = log.to_lowercase();
    const MAL: [&str; 7] = [
        "malicious",
        "malware",
        "backdoor",
        "compromise",
        "hijack",
        "trojan",
        "exfiltr",
    ];
    if let Some(kw) = MAL.iter().find(|k| low.contains(**k)) {
        return Ok(Some(format!("un commit postérieur mentionne « {kw} »")));
    }

    Ok(None)
}

/// Diff unifié entre le PKGBUILD installé et un nouveau contenu donné.
pub fn diff_against_installed(name: &str, new_pkgbuild: &str) -> String {
    match local_pkgbuild(name) {
        Some(local) if local.trim() == new_pkgbuild.trim() => String::new(),
        Some(local) => unified_diff(&local, new_pkgbuild, name),
        None => format!("# Pas de référence locale — inspection complète :\n{new_pkgbuild}"),
    }
}

/// Diff unifié maison (sans dépendance externe) : suffisant pour donner du
/// contexte à l'IA et à l'utilisateur.
fn unified_diff(old: &str, new: &str, name: &str) -> String {
    let old_lines: Vec<&str> = old.lines().collect();
    let new_lines: Vec<&str> = new.lines().collect();
    let mut out = format!("--- {name} (installé)\n+++ {name} (AUR)\n");
    let max = old_lines.len().max(new_lines.len());
    for i in 0..max {
        let o = old_lines.get(i);
        let n = new_lines.get(i);
        match (o, n) {
            (Some(a), Some(b)) if a == b => out.push_str(&format!("  {a}\n")),
            (Some(a), Some(b)) => {
                out.push_str(&format!("- {a}\n+ {b}\n"));
            }
            (Some(a), None) => out.push_str(&format!("- {a}\n")),
            (None, Some(b)) => out.push_str(&format!("+ {b}\n")),
            (None, None) => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_statique_est_extraite() {
        let pk = "pkgname=x\npkgver=1.2.3\npkgrel=2\n";
        assert_eq!(parse_version(pk), "1.2.3-2");
    }

    #[test]
    fn version_avec_epoch() {
        let pk = "pkgver=1.0\npkgrel=1\nepoch=2\n";
        assert_eq!(parse_version(pk), "2:1.0-1");
    }

    #[test]
    fn version_dynamique_vcs_non_geree() {
        let pk = "pkgver=r1234.$(git rev-parse)\npkgrel=1\n";
        assert_eq!(parse_version(pk), DYNAMIC_VERSION);
    }

    #[test]
    fn motif_pipe_bash_detecte() {
        let pk = "package(){ curl -fsSL https://x | bash; }";
        assert!(danger_signatures(pk).contains(&"pipe vers bash"));
    }

    #[test]
    fn pkgbuild_propre_sans_signature() {
        let pk = "package(){ install -Dm755 app \"$pkgdir/usr/bin/app\"; }";
        assert!(danger_signatures(pk).is_empty());
    }

    #[test]
    fn urlencode_gere_les_caracteres_speciaux() {
        assert_eq!(urlencode("c++-gtk"), "c%2B%2B-gtk");
        assert_eq!(urlencode("simple-bin"), "simple-bin");
    }
}
