//! Orchestration : pour chaque mise à jour AUR, applique la chaîne de décision
//! whitelist -> délai -> scan statique -> review IA et produit un verdict.

use crate::aur::{self, Update};
use crate::config::Config;
use crate::scan::{self, ScanResult};
use crate::ai;
use anyhow::Result;
use std::collections::HashMap;

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
}

/// Évalue toutes les mises à jour disponibles selon la config.
pub fn evaluate(cfg: &Config) -> Result<Vec<Outcome>> {
    let updates = aur::list_updates(&cfg.helper)?;
    if updates.is_empty() {
        return Ok(Vec::new());
    }

    let names: Vec<String> = updates.iter().map(|u| u.name.clone()).collect();
    let last_mod = aur::last_modified(&names).unwrap_or_default();
    let now = aur::now_secs();
    let threshold = cfg.delay_days * 86_400;

    let mut outcomes = Vec::new();
    for upd in updates {
        outcomes.push(evaluate_one(cfg, upd, &last_mod, now, threshold));
    }
    Ok(outcomes)
}

fn evaluate_one(
    cfg: &Config,
    upd: Update,
    last_mod: &HashMap<String, u64>,
    now: u64,
    threshold: u64,
) -> Outcome {
    let whitelisted = cfg.is_whitelisted(&upd.name);
    let age_days = last_mod
        .get(&upd.name)
        .map(|lm| now.saturating_sub(*lm) / 86_400);

    // 1) Délai (ignoré pour les paquets whitelistés).
    let fresh = match last_mod.get(&upd.name) {
        Some(lm) => now.saturating_sub(*lm) < threshold,
        None => false, // pas d'info => on ne retarde pas, mais scan/IA s'appliquent
    };
    if fresh && !whitelisted {
        return Outcome {
            update: upd,
            age_days,
            whitelisted,
            scan: ScanResult::Skipped,
            decision: Decision::Delayed(age_days.unwrap_or(0)),
        };
    }

    // 2) Scan statique (aur-scan) — s'applique même aux paquets whitelistés.
    let scan = scan::scan_package(&upd.name, cfg.use_aur_scan);
    if let ScanResult::Flagged(ref detail) = scan {
        return Outcome {
            update: upd,
            age_days,
            whitelisted,
            scan: scan.clone(),
            decision: Decision::Blocked(format!("aur-scan: {detail}")),
        };
    }

    // 3) Review IA du diff PKGBUILD.
    if cfg.ai.enabled {
        match aur::pkgbuild_diff(&upd.name) {
            Ok(diff) if diff.trim().is_empty() => { /* identique, rien à juger */ }
            Ok(diff) => match ai::review_diff(&cfg.ai, &upd.name, &diff) {
                Ok(v) if !v.safe => {
                    return Outcome {
                        update: upd,
                        age_days,
                        whitelisted,
                        scan,
                        decision: Decision::Blocked(format!(
                            "IA [{}]: {}",
                            v.severity, v.summary
                        )),
                    };
                }
                Ok(_) => { /* jugé sûr */ }
                Err(e) => {
                    // Une review qui échoue ne bloque pas, mais on le signale.
                    eprintln!("  (review IA indisponible pour {}: {})", upd.name, e);
                }
            },
            Err(e) => eprintln!("  (diff PKGBUILD indisponible pour {}: {})", upd.name, e),
        }
    }

    Outcome {
        update: upd,
        age_days,
        whitelisted,
        scan,
        decision: Decision::Allow,
    }
}

/// Liste des noms autorisés à être installés.
pub fn allowed_names(outcomes: &[Outcome]) -> Vec<String> {
    outcomes
        .iter()
        .filter(|o| o.decision == Decision::Allow)
        .map(|o| o.update.name.clone())
        .collect()
}

