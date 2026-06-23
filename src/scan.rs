//! Integration with `aur-scan` (ks-aur-scanner) for static analysis.
//! We delegate to the external tool when present; otherwise we return "skipped".

use std::path::Path;
use std::process::Command;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScanResult {
    /// aur-scan missing or disabled.
    Skipped,
    /// No critical alert.
    Clean,
    /// At least one blocking detection, with the raw detail.
    Flagged(String),
}

/// Is `aur-scan` available in the PATH?
pub fn available() -> bool {
    Command::new("aur-scan")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Scans an AUR package before installation: `aur-scan check <name>`.
/// Convention: a non-zero exit code => blocking detection.
pub fn scan_package(name: &str, enabled: bool) -> ScanResult {
    if !enabled || !available() {
        return ScanResult::Skipped;
    }
    interpret(Command::new("aur-scan").args(["check", name]).output())
}

/// Scans a local PKGBUILD file: `aur-scan scan <path>`.
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
