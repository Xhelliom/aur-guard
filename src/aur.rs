//! Interactions avec l'AUR : liste des mises à jour, date de dernière
//! modification (API RPC) et récupération du diff de PKGBUILD.

use anyhow::{Context, Result};
use serde::Deserialize;
use std::collections::HashMap;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

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

#[derive(Deserialize)]
struct RpcInfo {
    #[serde(rename = "Name")]
    name: String,
    #[serde(rename = "LastModified")]
    last_modified: u64,
}

#[derive(Deserialize)]
struct RpcResponse {
    results: Vec<RpcInfo>,
}

/// Récupère le timestamp `LastModified` de chaque paquet via l'API RPC v5.
/// Renvoie une map nom -> epoch (secondes).
pub fn last_modified(names: &[String]) -> Result<HashMap<String, u64>> {
    let mut map = HashMap::new();
    if names.is_empty() {
        return Ok(map);
    }
    // L'API accepte plusieurs arg[]= ; on découpe par lots pour éviter une URL
    // trop longue.
    for chunk in names.chunks(50) {
        let query: String = chunk
            .iter()
            .map(|n| format!("arg[]={}", urlencode(n)))
            .collect::<Vec<_>>()
            .join("&");
        let url = format!("https://aur.archlinux.org/rpc/v5/info?{query}");
        let resp: RpcResponse = ureq::get(&url)
            .set("User-Agent", "aur-guard")
            .call()
            .with_context(|| "appel API AUR RPC")?
            .into_json()
            .context("parsing JSON RPC")?;
        for info in resp.results {
            map.insert(info.name, info.last_modified);
        }
    }
    Ok(map)
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
        "https://aur.archlinux.org/cgit/aur.git/plain/PKGBUILD?h={}",
        urlencode(name)
    );
    let body = ureq::get(&url)
        .set("User-Agent", "aur-guard")
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
