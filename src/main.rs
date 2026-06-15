//! aur-guard — garde-fou de sécurité pour les mises à jour AUR.
//!
//! Chaîne de décision par paquet : whitelist -> délai (LastModified) ->
//! scan statique (aur-scan) -> review IA du diff PKGBUILD.

use anyhow::Result;
use aur_guard::pipeline::{Decision, Outcome};
use aur_guard::{ai, aur, config, pipeline, scan};
use clap::{Parser, Subcommand};
use std::process::Command;

#[derive(Parser)]
#[command(
    name = "aur-guard",
    version,
    about = "Mises à jour AUR sécurisées : délai, whitelist, scan statique et review IA"
)]
struct Cli {
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Rapport : évalue les maj disponibles sans rien installer (défaut).
    Check,
    /// Installe les paquets jugés sûrs via le helper AUR.
    Apply {
        /// N'installe pas, montre seulement la commande qui serait lancée.
        #[arg(long)]
        dry_run: bool,
    },
    /// Affiche l'âge (dernière modif AUR) de tous les paquets AUR installés.
    Status,
    /// Affiche le chemin du fichier de config (et le crée si absent).
    Config,
    /// Intègre aur-guard au service systemd checkupdates-notify.
    InstallHook,
    /// (debug) Lance la review IA sur un fichier PKGBUILD local.
    ReviewFile {
        /// Chemin du PKGBUILD (ou diff) à analyser.
        path: String,
    },
    /// Ouvre l'interface de paramétrage en terminal (TUI).
    #[cfg(feature = "tui")]
    ConfigUi,
}

fn main() {
    if let Err(e) = run() {
        eprintln!("erreur: {e:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let cli = Cli::parse();
    match cli.cmd.unwrap_or(Cmd::Check) {
        Cmd::Check => cmd_check(),
        Cmd::Apply { dry_run } => cmd_apply(dry_run),
        Cmd::Status => cmd_status(),
        Cmd::Config => cmd_config(),
        Cmd::InstallHook => cmd_install_hook(),
        Cmd::ReviewFile { path } => cmd_review_file(&path),
        #[cfg(feature = "tui")]
        Cmd::ConfigUi => {
            let cfg = config::Config::load_or_init()?;
            aur_guard::tui::run(cfg)
        }
    }
}

fn cmd_review_file(path: &str) -> Result<()> {
    let cfg = config::Config::load_or_init()?;
    let content = std::fs::read_to_string(path)
        .map_err(|e| anyhow::anyhow!("lecture de {path}: {e}"))?;
    println!(
        "Review IA de {path} (provider {:?}, jusqu'à {} votes de confirmation)\n",
        cfg.ai.provider, cfg.ai.confirm_votes
    );
    let v = ai::review_diff(&cfg.ai, path, &content)?;
    println!("  safe     : {}", v.safe);
    println!("  severity : {}", v.severity);
    println!("  résumé   : {}", v.summary);
    Ok(())
}

fn cmd_check() -> Result<()> {
    let cfg = config::Config::load_or_init()?;
    let outcomes = pipeline::evaluate(&cfg)?;
    print_report(&cfg, &outcomes);
    Ok(())
}

fn cmd_apply(dry_run: bool) -> Result<()> {
    let cfg = config::Config::load_or_init()?;
    let outcomes = pipeline::evaluate(&cfg)?;
    print_report(&cfg, &outcomes);

    let allowed = pipeline::allowed_names(&outcomes);
    if allowed.is_empty() {
        println!("\nRien à installer.");
        return Ok(());
    }

    println!("\nÀ installer : {}", allowed.join(", "));
    if dry_run {
        println!("(dry-run) commande : {} -S {}", cfg.helper, allowed.join(" "));
        return Ok(());
    }

    let status = Command::new(&cfg.helper)
        .arg("-S")
        .args(&allowed)
        .status()?;
    if !status.success() {
        anyhow::bail!("le helper a renvoyé une erreur");
    }
    Ok(())
}

fn cmd_status() -> Result<()> {
    let cfg = config::Config::load_or_init()?;
    let out = Command::new(&cfg.helper).args(["-Qmq"]).output();
    // -Qmq via pacman serait plus fiable ; on passe par pacman directement.
    let pkgs_raw = match out {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).to_string(),
        _ => {
            let p = Command::new("pacman").args(["-Qmq"]).output()?;
            String::from_utf8_lossy(&p.stdout).to_string()
        }
    };
    let names: Vec<String> = pkgs_raw.lines().map(|l| l.trim().to_string()).collect();
    let last_mod = aur::last_modified(&names)?;
    let now = aur::now_secs();
    let threshold = cfg.delay_days * 86_400;

    println!("Âge des {} paquets AUR installés (délai = {}j) :", names.len(), cfg.delay_days);
    let mut rows: Vec<(String, Option<u64>)> = names
        .iter()
        .map(|n| (n.clone(), last_mod.get(n).map(|lm| now.saturating_sub(*lm) / 86_400)))
        .collect();
    rows.sort_by(|a, b| a.0.cmp(&b.0));
    for (name, age) in rows {
        match age {
            Some(d) => {
                let flag = if d * 86_400 < threshold { "⏳" } else { "  " };
                println!("  {flag} {name:<34} {d:>4}j");
            }
            None => println!("     {name:<34}    ?"),
        }
    }
    Ok(())
}

fn cmd_config() -> Result<()> {
    let cfg = config::Config::load_or_init()?;
    println!("Config : {}", config::Config::path()?.display());
    println!("  délai           : {} jours", cfg.delay_days);
    println!("  helper          : {}", cfg.helper);
    println!("  aur-scan         : {}", cfg.use_aur_scan);
    println!("  review IA        : {} (provider: {:?}, modèle: {})",
        cfg.ai.enabled, cfg.ai.provider, cfg.ai.model_or_default());
    println!("  votes confirm.   : {} (déclenchés seulement sur blocage)", cfg.ai.confirm_votes);
    println!("  whitelist        : {} paquets", cfg.whitelist.len());
    Ok(())
}

fn cmd_install_hook() -> Result<()> {
    let base = dirs::config_dir().ok_or_else(|| anyhow::anyhow!("~/.config introuvable"))?;
    let svc = base.join("systemd/user/checkupdates-notify.service");
    let exe = std::env::current_exe()
        .ok()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "aur-guard".to_string());

    let content = format!(
        "[Unit]\n\
         Description=Notify available system updates (aur-guard)\n\n\
         [Service]\n\
         Type=oneshot\n\
         ExecStart=/bin/bash -c 'updates=$(checkupdates 2>/dev/null | wc -l); \
         aur=$({exe} check 2>/dev/null | grep -c \"✅\"); \
         if [ \"$updates\" -gt 0 ] || [ \"$aur\" -gt 0 ]; then \
         notify-send -u normal \"Mises à jour disponibles\" \
         \"$updates paquets dépôts + $aur maj AUR sûres (aur-guard apply)\"; \
         else notify-send -u low \"Système à jour\" \"Aucune mise à jour\"; fi'\n"
    );

    if svc.exists() {
        let backup = svc.with_extension("service.bak");
        std::fs::copy(&svc, &backup)?;
        println!("Sauvegarde : {}", backup.display());
    }
    std::fs::write(&svc, content)?;
    println!("Service mis à jour : {}", svc.display());
    println!("Recharge avec : systemctl --user daemon-reload");
    Ok(())
}

fn print_report(cfg: &config::Config, outcomes: &[Outcome]) {
    if outcomes.is_empty() {
        println!("Aucune mise à jour AUR disponible.");
        return;
    }
    println!(
        "Mises à jour AUR : {} (délai {}j, review IA {})\n",
        outcomes.len(),
        cfg.delay_days,
        if cfg.ai.enabled { "activée" } else { "désactivée" }
    );
    let (mut allow, mut delay, mut block) = (0, 0, 0);
    for o in outcomes {
        let tag = match &o.decision {
            Decision::Allow => {
                allow += 1;
                let scanned = matches!(o.scan, scan::ScanResult::Clean);
                let base = if o.whitelisted { "✅ (whitelist)" } else { "✅" };
                if scanned { format!("{base} [scan ok]") } else { base.to_string() }
            }
            Decision::Delayed(d) => {
                delay += 1;
                format!("⏳ retardé ({d}j)")
            }
            Decision::Blocked(reason) => {
                block += 1;
                format!("⛔ BLOQUÉ — {reason}")
            }
        };
        let ver = match (o.update.old_ver.is_empty(), o.update.new_ver.is_empty()) {
            (false, false) => format!("{} → {}", o.update.old_ver, o.update.new_ver),
            _ => o.age_days.map(|d| format!("modifié il y a {d}j")).unwrap_or_default(),
        };
        println!("  {:<30} {:<22} {tag}", o.update.name, ver);
    }
    println!("\nSûrs : {allow} | retardés : {delay} | bloqués : {block}");
}
