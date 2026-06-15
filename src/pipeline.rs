//! Orchestration : pour chaque mise à jour AUR, applique la chaîne de décision
//! whitelist -> délai (hold ou lag) -> scan statique -> review IA.

use crate::ai;
use crate::aur::{self, LagTarget, PkgInfo, Update};
use crate::config::{Config, DelayMode};
use crate::scan::{self, ScanResult};
use anyhow::Result;
use std::collections::HashMap;
use std::process::Command;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// Mise à jour autorisée.
    Allow,
    /// Retardée car trop récente (âge en jours).
    Delayed(u64),
    /// Bloquée par le scan statique ou la review IA, avec la raison.
    Blocked(String),
}

#[derive(Debug, Clone)]
pub struct Outcome {
    pub update: Update,
    pub age_days: Option<u64>,
    pub whitelisted: bool,
    pub scan: ScanResult,
    pub decision: Decision,
    /// En mode lag : la révision décalée à installer (None = dernière version).
    pub lag: Option<LagTarget>,
}

/// Évalue toutes les mises à jour disponibles selon la config.
pub fn evaluate(cfg: &Config) -> Result<Vec<Outcome>> {
    let updates = aur::list_updates(&cfg.helper)?;
    if updates.is_empty() {
        return Ok(Vec::new());
    }

    let names: Vec<String> = updates.iter().map(|u| u.name.clone()).collect();
    let infos = aur::fetch_infos(&names).unwrap_or_default();
    let now = aur::now_secs();
    let threshold = cfg.delay_days * 86_400;

    let mut outcomes = Vec::new();
    for upd in updates {
        outcomes.push(evaluate_one(cfg, upd, &infos, now, threshold));
    }
    Ok(outcomes)
}

fn evaluate_one(
    cfg: &Config,
    upd: Update,
    infos: &HashMap<String, PkgInfo>,
    now: u64,
    threshold: u64,
) -> Outcome {
    let whitelisted = cfg.is_whitelisted(&upd.name);
    let info = infos.get(&upd.name);
    let age_days = info.map(|i| now.saturating_sub(i.last_modified) / 86_400);

    // Paquet de confiance : on vise la DERNIÈRE version, délai ignoré, mais
    // scan + review IA s'appliquent quand même.
    if whitelisted {
        return decide_latest(cfg, upd, age_days, true);
    }

    match cfg.delay_mode {
        DelayMode::Hold => {
            let fresh = info
                .map(|i| now.saturating_sub(i.last_modified) < threshold)
                .unwrap_or(false);
            if fresh {
                return delayed(upd, age_days);
            }
            decide_latest(cfg, upd, age_days, false)
        }
        DelayMode::Lag => evaluate_lag(cfg, upd, info, age_days, now, threshold),
    }
}

/// Mode lag : cible la révision qui était la HEAD il y a `threshold` secondes.
fn evaluate_lag(
    cfg: &Config,
    upd: Update,
    info: Option<&PkgInfo>,
    age_days: Option<u64>,
    now: u64,
    threshold: u64,
) -> Outcome {
    let pkgbase = info
        .map(|i| i.package_base.clone())
        .unwrap_or_else(|| upd.name.clone());
    let before = now.saturating_sub(threshold);

    let target = match aur::lagged_target(&pkgbase, before) {
        Ok(Some(t)) => t,
        Ok(None) => return delayed(upd, age_days), // paquet trop jeune
        Err(e) => {
            eprintln!("  (git indisponible pour {}: {e})", upd.name);
            return delayed(upd, age_days);
        }
    };

    // Version dynamique (VCS) : le lag par révision n'a pas de sens.
    if target.version == "?" {
        return Outcome {
            decision: Decision::Delayed(age_days.unwrap_or(0)),
            scan: ScanResult::Skipped,
            lag: None,
            update: upd,
            age_days,
            whitelisted: false,
        };
    }

    // Sommes-nous déjà à jour (ou en avance) par rapport à la cible J-N ?
    if vercmp(&target.version, &upd.old_ver) <= 0 {
        return delayed(upd, age_days);
    }

    // Scan statique + review IA sur LA RÉVISION qu'on installera.
    let scan = scan_lagged(&upd.name, &target.pkgbuild, cfg.use_aur_scan);
    if let ScanResult::Flagged(ref detail) = scan {
        return Outcome {
            decision: Decision::Blocked(format!("aur-scan: {detail}")),
            scan: scan.clone(),
            lag: Some(target),
            update: upd,
            age_days,
            whitelisted: false,
        };
    }

    if cfg.ai.enabled {
        let diff = aur::diff_against_installed(&upd.name, &target.pkgbuild);
        if !diff.trim().is_empty() {
            match ai::review_diff(&cfg.ai, &upd.name, &diff) {
                Ok(v) if !v.safe => {
                    return Outcome {
                        decision: Decision::Blocked(format!("IA [{}]: {}", v.severity, v.summary)),
                        scan,
                        lag: Some(target),
                        update: upd,
                        age_days,
                        whitelisted: false,
                    };
                }
                Ok(_) => {}
                Err(e) => eprintln!("  (review IA indisponible pour {}: {})", upd.name, e),
            }
        }
    }

    Outcome {
        decision: Decision::Allow,
        scan,
        lag: Some(target),
        update: upd,
        age_days,
        whitelisted: false,
    }
}

/// Décision visant la dernière version (whitelist, ou hold après maturation).
fn decide_latest(cfg: &Config, upd: Update, age_days: Option<u64>, whitelisted: bool) -> Outcome {
    let scan = scan::scan_package(&upd.name, cfg.use_aur_scan);
    if let ScanResult::Flagged(ref detail) = scan {
        return Outcome {
            decision: Decision::Blocked(format!("aur-scan: {detail}")),
            scan: scan.clone(),
            lag: None,
            update: upd,
            age_days,
            whitelisted,
        };
    }
    if cfg.ai.enabled {
        if let Ok(diff) = aur::pkgbuild_diff(&upd.name) {
            if !diff.trim().is_empty() {
                match ai::review_diff(&cfg.ai, &upd.name, &diff) {
                    Ok(v) if !v.safe => {
                        return Outcome {
                            decision: Decision::Blocked(format!("IA [{}]: {}", v.severity, v.summary)),
                            scan,
                            lag: None,
                            update: upd,
                            age_days,
                            whitelisted,
                        };
                    }
                    Ok(_) => {}
                    Err(e) => eprintln!("  (review IA indisponible pour {}: {})", upd.name, e),
                }
            }
        }
    }
    Outcome {
        decision: Decision::Allow,
        scan,
        lag: None,
        update: upd,
        age_days,
        whitelisted,
    }
}

fn delayed(upd: Update, age_days: Option<u64>) -> Outcome {
    Outcome {
        decision: Decision::Delayed(age_days.unwrap_or(0)),
        scan: ScanResult::Skipped,
        lag: None,
        update: upd,
        age_days,
        whitelisted: false,
    }
}

/// Scan statique d'une révision lag : on écrit le PKGBUILD dans un fichier
/// temporaire et on le passe à `aur-scan scan`.
fn scan_lagged(name: &str, pkgbuild: &str, enabled: bool) -> ScanResult {
    if !enabled {
        return ScanResult::Skipped;
    }
    let path = std::env::temp_dir().join(format!("aur-guard-{name}.PKGBUILD"));
    if std::fs::write(&path, pkgbuild).is_err() {
        return ScanResult::Skipped;
    }
    let res = scan::scan_pkgbuild_file(&path, enabled);
    let _ = std::fs::remove_file(&path);
    res
}

/// Compare deux versions via l'outil `vercmp` de pacman.
/// Renvoie >0 si a est plus récent que b, 0 si égal, <0 sinon.
fn vercmp(a: &str, b: &str) -> i32 {
    match Command::new("vercmp").args([a, b]).output() {
        Ok(out) => String::from_utf8_lossy(&out.stdout)
            .trim()
            .parse()
            .unwrap_or(0),
        Err(_) => {
            // Pas de vercmp : on retombe sur une égalité stricte de chaînes.
            if a == b {
                0
            } else {
                1
            }
        }
    }
}

/// Liste des noms autorisés (toutes décisions Allow confondues).
pub fn allowed_names(outcomes: &[Outcome]) -> Vec<String> {
    outcomes
        .iter()
        .filter(|o| o.decision == Decision::Allow)
        .map(|o| o.update.name.clone())
        .collect()
}
