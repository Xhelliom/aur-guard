//! Configuration de aur-guard : délai, whitelist et paramètres de review IA.
//! Le fichier vit dans ~/.config/aur-guard/config.toml et est créé avec des
//! valeurs par défaut au premier lancement.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Permissions du fichier de secrets (lecture/écriture propriétaire seul).
const SECRETS_MODE: u32 = 0o600;

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

/// Sémantique du délai de sécurité.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DelayMode {
    /// Bloque toute maj dont la dernière version a moins de `delay_days`.
    /// Reste sur la version installée (risque : paquets à maj fréquente jamais
    /// mis à jour).
    Hold,
    /// Installe la révision qui était la HEAD du git AUR il y a `delay_days`
    /// jours. Les maj arrivent toujours, avec un retard constant.
    Lag,
}

fn default_delay_mode() -> DelayMode {
    DelayMode::Lag
}

/// Intervalle de notification par défaut (heures) : compromis entre fraîcheur
/// de l'info et discrétion.
fn default_notify_interval() -> u64 {
    6
}

/// Réglages des notifications de bureau (timer systemd `--user`).
///
/// Le timer exécute `aur-guard notify`, qui compte les mises à jour officielles
/// et AUR disponibles (sans review IA, donc sans coût) et appelle `notify-send`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NotifyConfig {
    /// Le timer de notification doit-il être actif.
    pub enabled: bool,
    /// Périodicité de la vérification, en heures.
    #[serde(default = "default_notify_interval")]
    pub interval_hours: u64,
    /// Ne rien notifier quand le système est à jour (sinon notification discrète).
    #[serde(default)]
    pub silent_when_up_to_date: bool,
}

impl Default for NotifyConfig {
    fn default() -> Self {
        NotifyConfig {
            enabled: false,
            interval_hours: default_notify_interval(),
            silent_when_up_to_date: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// Nombre de jours pendant lesquels une maj fraîche est retardée.
    pub delay_days: u64,
    /// Sémantique du délai (hold ou lag).
    #[serde(default = "default_delay_mode")]
    pub delay_mode: DelayMode,
    /// Helper AUR à utiliser pour lister et installer (yay/paru).
    pub helper: String,
    /// Active l'appel à `aur-scan` (analyse statique) s'il est installé.
    pub use_aur_scan: bool,
    /// Paquets de confiance de l'utilisateur : maj immédiate, délai ignoré.
    /// (La review IA / le scan statique s'appliquent quand même.)
    pub whitelist: Vec<String>,
    pub ai: AiConfig,
    /// Réglages des notifications de bureau.
    #[serde(default)]
    pub notify: NotifyConfig,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            delay_days: 14,
            delay_mode: DelayMode::Lag,
            helper: "yay".to_string(),
            use_aur_scan: true,
            whitelist: recommended_whitelist(),
            ai: AiConfig::default(),
            notify: NotifyConfig::default(),
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
        let cfg: Config =
            toml::from_str(&text).with_context(|| format!("parsing TOML de {}", path.display()))?;
        Ok(cfg)
    }

    pub fn save(&self) -> Result<()> {
        let path = Self::path()?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let text = toml::to_string_pretty(self)?;
        std::fs::write(&path, text).with_context(|| format!("écriture de {}", path.display()))?;
        Ok(())
    }

    pub fn is_whitelisted(&self, pkg: &str) -> bool {
        self.whitelist.iter().any(|w| w == pkg)
    }
}

/// Clés API des fournisseurs, stockées hors de `config.toml` dans un fichier à
/// permissions restreintes. **Jamais** versionné ni journalisé.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Secrets {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub groq: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anthropic: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub openai: Option<String>,
}

impl Secrets {
    pub fn path() -> Result<PathBuf> {
        let base = dirs::config_dir().context("impossible de résoudre ~/.config")?;
        Ok(base.join("aur-guard").join("secrets.toml"))
    }

    /// Charge les secrets (structure vide si le fichier est absent).
    pub fn load() -> Secrets {
        let Ok(path) = Self::path() else {
            return Secrets::default();
        };
        std::fs::read_to_string(&path)
            .ok()
            .and_then(|t| toml::from_str(&t).ok())
            .unwrap_or_default()
    }

    /// Écrit les secrets avec des permissions `0600`.
    pub fn save(&self) -> Result<()> {
        let path = Self::path()?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let text = toml::to_string_pretty(self)?;
        std::fs::write(&path, text).with_context(|| format!("écriture de {}", path.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(SECRETS_MODE))?;
        }
        Ok(())
    }

    pub fn get(&self, provider: Provider) -> Option<&str> {
        match provider {
            Provider::Groq => self.groq.as_deref(),
            Provider::Anthropic => self.anthropic.as_deref(),
            Provider::Openai => self.openai.as_deref(),
        }
    }

    pub fn set(&mut self, provider: Provider, key: Option<String>) {
        // Une chaîne vide efface la clé.
        let key = key.filter(|k| !k.trim().is_empty());
        match provider {
            Provider::Groq => self.groq = key,
            Provider::Anthropic => self.anthropic = key,
            Provider::Openai => self.openai = key,
        }
    }
}

/// Résout la clé API à utiliser : variable d'environnement en priorité, sinon
/// le fichier de secrets. Renvoie None si aucune n'est disponible.
pub fn resolve_api_key(ai: &AiConfig) -> Option<String> {
    if let Ok(key) = std::env::var(ai.key_env_or_default()) {
        if !key.is_empty() {
            return Some(key);
        }
    }
    Secrets::load().get(ai.provider).map(|s| s.to_string())
}
