//! Configuration de aur-guard : délai, whitelist et paramètres de review IA.
//! Le fichier vit dans ~/.config/aur-guard/config.toml et est créé avec des
//! valeurs par défaut au premier lancement.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Fournisseur d'IA pour la review du diff PKGBUILD.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Provider {
    Groq,
    Anthropic,
    Openai,
}

impl Provider {
    /// Endpoint HTTP de l'API chat du fournisseur.
    pub fn endpoint(&self) -> &'static str {
        match self {
            Provider::Groq => "https://api.groq.com/openai/v1/chat/completions",
            Provider::Anthropic => "https://api.anthropic.com/v1/messages",
            Provider::Openai => "https://api.openai.com/v1/chat/completions",
        }
    }

    /// Nom de la variable d'environnement contenant la clé API par défaut.
    pub fn default_key_env(&self) -> &'static str {
        match self {
            Provider::Groq => "GROQ_API_KEY",
            Provider::Anthropic => "ANTHROPIC_API_KEY",
            Provider::Openai => "OPENAI_API_KEY",
        }
    }

    /// Modèle par défaut raisonnable pour de l'analyse de sécurité.
    pub fn default_model(&self) -> &'static str {
        match self {
            Provider::Groq => "llama-3.3-70b-versatile",
            Provider::Anthropic => "claude-fable-5",
            Provider::Openai => "gpt-4o",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AiConfig {
    /// Active la review IA du diff PKGBUILD.
    pub enabled: bool,
    /// Fournisseur sélectionné.
    pub provider: Provider,
    /// Modèle à utiliser (vide => default_model du provider).
    #[serde(default)]
    pub model: String,
    /// Variable d'env contenant la clé API (vide => default_key_env du provider).
    #[serde(default)]
    pub api_key_env: String,
    /// Nombre total de votes (1er appel inclus) pour CONFIRMER un blocage.
    /// Un verdict « sûr » au 1er appel ne déclenche aucun vote supplémentaire
    /// (1 seul appel) ; seuls les blocages sont confirmés à la majorité.
    #[serde(default = "default_confirm_votes")]
    pub confirm_votes: u32,
}

fn default_confirm_votes() -> u32 {
    3
}

impl Default for AiConfig {
    fn default() -> Self {
        AiConfig {
            enabled: true,
            provider: Provider::Groq,
            model: String::new(),
            api_key_env: String::new(),
            confirm_votes: default_confirm_votes(),
        }
    }
}

impl AiConfig {
    pub fn model_or_default(&self) -> String {
        if self.model.is_empty() {
            self.provider.default_model().to_string()
        } else {
            self.model.clone()
        }
    }

    pub fn key_env_or_default(&self) -> String {
        if self.api_key_env.is_empty() {
            self.provider.default_key_env().to_string()
        } else {
            self.api_key_env.clone()
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// Nombre de jours pendant lesquels une maj fraîche est retardée.
    pub delay_days: u64,
    /// Helper AUR à utiliser pour lister et installer (yay/paru).
    pub helper: String,
    /// Active l'appel à `aur-scan` (analyse statique) s'il est installé.
    pub use_aur_scan: bool,
    /// Paquets de confiance de l'utilisateur : maj immédiate, délai ignoré.
    /// (La review IA / le scan statique s'appliquent quand même.)
    pub whitelist: Vec<String>,
    pub ai: AiConfig,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            delay_days: 14,
            helper: "yay".to_string(),
            use_aur_scan: true,
            whitelist: recommended_whitelist(),
            ai: AiConfig::default(),
        }
    }
}

/// Whitelist recommandée : paquets `-bin` qui repackagent un binaire signé d'un
/// éditeur réputé. Pour eux le délai retarderait surtout des correctifs de
/// sécurité légitimes ; on les met à jour vite MAIS on garde scan + review IA
/// (car le PKGBUILD reste le vecteur d'attaque possible).
pub fn recommended_whitelist() -> Vec<String> {
    [
        "google-chrome",
        "google-chrome-beta",
        "microsoft-edge-stable-bin",
        "brave-bin",
        "zen-browser-bin",
        "android-studio",
        "cursor-bin",
        "visual-studio-code-bin",
        "vscodium-bin",
        "spotify",
        "discord",
        "slack-desktop",
        "zoom",
        "1password",
        "dropbox",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

impl Config {
    /// Chemin du fichier de configuration.
    pub fn path() -> Result<PathBuf> {
        let base = dirs::config_dir().context("impossible de résoudre ~/.config")?;
        Ok(base.join("aur-guard").join("config.toml"))
    }

    /// Charge la config, en créant un fichier par défaut si absent.
    pub fn load_or_init() -> Result<Config> {
        let path = Self::path()?;
        if !path.exists() {
            let cfg = Config::default();
            cfg.save()?;
            eprintln!("Config par défaut créée : {}", path.display());
            return Ok(cfg);
        }
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("lecture de {}", path.display()))?;
        let cfg: Config = toml::from_str(&text)
            .with_context(|| format!("parsing TOML de {}", path.display()))?;
        Ok(cfg)
    }

    pub fn save(&self) -> Result<()> {
        let path = Self::path()?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let text = toml::to_string_pretty(self)?;
        std::fs::write(&path, text)
            .with_context(|| format!("écriture de {}", path.display()))?;
        Ok(())
    }

    pub fn is_whitelisted(&self, pkg: &str) -> bool {
        self.whitelist.iter().any(|w| w == pkg)
    }
}
