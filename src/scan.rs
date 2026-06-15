//! Intégration de `aur-scan` (ks-aur-scanner) pour l'analyse statique.
//! On délègue à l'outil externe s'il est présent ; sinon on renvoie « ignoré ».

use std::path::Path;
use std::process::Command;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScanResult {
    /// aur-scan absent ou désactivé.
    Skipped,
    /// Aucune alerte critique.
    Clean,
    /// Au moins une détection bloquante, avec le détail brut.
    Flagged(String),
}

/// `aur-scan` est-il disponible dans le PATH ?
pub fn available() -> bool {
    Command::new("aur-scan")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Scanne un paquet AUR avant installation : `aur-scan check <name>`.
/// Convention : code de sortie non-zéro => détection bloquante.
pub fn scan_package(name: &str, enabled: bool) -> ScanResult {
    if !enabled || !available() {
        return ScanResult::Skipped;
    }
    interpret(Command::new("aur-scan").args(["check", name]).output())
}

/// Scanne un fichier PKGBUILD local : `aur-scan scan <path>`.
pub fn scan_pkgbuild_file(path: &Path, enabled: bool) -> ScanResult {
    if !enabled || !available() {
        return ScanResult::Skipped;
    }
    let path = match path.to_str() {
        Some(p) => p,
        None => return ScanResult::Skipped,
    };
    interpret(Command::new("aur-scan").args(["scan", path]).output())
}

fn interpret(output: std::io::Result<std::process::Output>) -> ScanResult {
    match output {
        Ok(out) => {
            if out.status.success() {
                ScanResult::Clean
            } else {
                let mut detail = String::from_utf8_lossy(&out.stdout).to_string();
                detail.push_str(&String::from_utf8_lossy(&out.stderr));
                ScanResult::Flagged(detail.trim().to_string())
            }
        }
        Err(_) => ScanResult::Skipped,
    }
}
