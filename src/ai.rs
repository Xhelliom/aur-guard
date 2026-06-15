//! Review IA du diff PKGBUILD via un fournisseur configurable
//! (Groq / OpenAI : format chat-completions ; Anthropic : format messages).
//! On demande au modèle un verdict JSON structuré.

use crate::config::{AiConfig, Provider};
use anyhow::{anyhow, Context, Result};
use serde::Deserialize;

/// Température nulle : on veut le verdict le plus déterministe possible.
const TEMPERATURE: f32 = 0.0;
/// Plafond de tokens pour la réponse (le verdict JSON est court).
const MAX_TOKENS: u32 = 512;
/// Version de l'API Messages d'Anthropic.
const ANTHROPIC_VERSION: &str = "2023-06-01";

#[derive(Debug, Clone, Deserialize)]
pub struct Verdict {
    /// true si le diff paraît sûr.
    pub safe: bool,
    /// low / medium / high / critical
    pub severity: String,
    /// Explication courte en français.
    pub summary: String,
}

const SYSTEM_PROMPT: &str =
    "Tu es un auditeur de sécurité spécialisé dans les PKGBUILD Arch Linux et l'AUR. \
On te donne le diff (ou le contenu) d'un PKGBUILD et de ses scripts. Ton rôle est de \
détecter une COMPROMISSION de la supply chain, pas de critiquer le style de packaging. \
\
NORMAL et NON suspect en soi (ne PAS signaler) : changement de numéro de version (pkgver, \
pkgrel), mise à jour des sommes de contrôle (sha256sums/sha512sums/b2sums) qui accompagne \
une nouvelle version, extraction de .deb/.tar, usage de sed/ln/install/desktop-file pour \
poser des fichiers, liens symboliques vers /usr/bin, téléchargement depuis le domaine \
officiel habituel de l'éditeur déjà présent dans la version précédente. \
\
VRAIMENT suspect (signaler, safe=false) : nouvelle source pointant vers un domaine \
inhabituel/différent de l'éditeur, ajout d'un curl|bash ou wget|sh, exécution d'un binaire/ELF \
téléchargé, nouveau hook pre/post install exécutant du code distant, code obfusqué ou encodé \
(base64/eval/xxd), exfiltration (envoi de fichiers, variables d'env, clés) vers le réseau, \
ajout inattendu de dépendances npm/pip installées au build avec lifecycle hooks. \
\
Ne te base que sur ce que montre le diff. Réponds UNIQUEMENT par un objet JSON, sans texte \
autour : {\"safe\": bool, \"severity\": \"low|medium|high|critical\", \"summary\": \"...\"}. \
Mets safe=false uniquement s'il y a un indicateur réel de la liste « vraiment suspect ».";

/// Review IA d'un diff avec confirmation par multi-vote.
///
/// Stratégie d'économie : un seul appel si le 1er verdict est « sûr ». Si le
/// 1er verdict est un blocage, on lance des votes supplémentaires (jusqu'à
/// `confirm_votes` au total) et on ne confirme le blocage qu'à la majorité —
/// ce qui élimine les faux positifs dus à la non-déterminisme du modèle.
pub fn review_diff(cfg: &AiConfig, pkg: &str, diff: &str) -> Result<Verdict> {
    let first = review_once(cfg, pkg, diff)?;
    let votes = cfg.confirm_votes.max(1);

    // Cas courant : sûr dès le 1er appel, ou multi-vote désactivé -> on s'arrête.
    if first.safe || votes <= 1 {
        return Ok(first);
    }

    // Le 1er verdict est un blocage : on le met aux voix pour le confirmer.
    let mut unsafe_count = 1u32;
    let mut total = 1u32;
    let mut last_unsafe = first;
    for _ in 1..votes {
        match review_once(cfg, pkg, diff) {
            Ok(v) => {
                total += 1;
                if !v.safe {
                    unsafe_count += 1;
                    last_unsafe = v;
                }
            }
            // Un vote raté ne compte pas mais ne bloque pas la procédure.
            Err(e) => eprintln!("  (vote IA échoué pour {pkg}: {e})"),
        }
    }

    // Blocage confirmé si une majorité stricte des votes le maintient.
    if unsafe_count * 2 > total {
        let mut v = last_unsafe;
        v.summary = format!(
            "{} — blocage confirmé par {}/{} votes",
            v.summary, unsafe_count, total
        );
        Ok(v)
    } else {
        Ok(Verdict {
            safe: true,
            severity: "low".to_string(),
            summary: format!(
                "blocage initial NON confirmé ({}/{} votes suspects) — autorisé",
                unsafe_count, total
            ),
        })
    }
}

/// Un seul appel au modèle, renvoyant un Verdict.
fn review_once(cfg: &AiConfig, pkg: &str, diff: &str) -> Result<Verdict> {
    let api_key = crate::config::resolve_api_key(cfg).ok_or_else(|| {
        anyhow!(
            "clé API {:?} introuvable (ni dans ${}, ni dans secrets.toml)",
            cfg.provider,
            cfg.key_env_or_default()
        )
    })?;
    let model = cfg.model_or_default();

    let user_msg = format!(
        "Paquet : {pkg}\nAnalyse ce diff de PKGBUILD et rends ton verdict JSON :\n\n{diff}"
    );

    let raw = match cfg.provider {
        Provider::Anthropic => call_anthropic(&api_key, &model, &user_msg)?,
        Provider::Groq | Provider::Openai => {
            call_openai_compatible(cfg.provider, &api_key, &model, &user_msg)?
        }
    };

    parse_verdict(&raw).with_context(|| format!("réponse IA non exploitable : {raw}"))
}

/// Format chat-completions (Groq et OpenAI partagent le même schéma).
fn call_openai_compatible(
    provider: Provider,
    api_key: &str,
    model: &str,
    user_msg: &str,
) -> Result<String> {
    let body = serde_json::json!({
        "model": model,
        "temperature": TEMPERATURE,
        "messages": [
            {"role": "system", "content": SYSTEM_PROMPT},
            {"role": "user", "content": user_msg}
        ]
    });
    let resp: serde_json::Value = ureq::post(provider.endpoint())
        .set("Authorization", &format!("Bearer {api_key}"))
        .set("Content-Type", "application/json")
        .send_json(body)
        .context("appel API chat-completions")?
        .into_json()
        .context("parsing réponse chat-completions")?;
    resp["choices"][0]["message"]["content"]
        .as_str()
        .map(|s| s.to_string())
        .ok_or_else(|| anyhow!("champ content absent dans la réponse"))
}

/// Format Anthropic Messages.
fn call_anthropic(api_key: &str, model: &str, user_msg: &str) -> Result<String> {
    let body = serde_json::json!({
        "model": model,
        "max_tokens": MAX_TOKENS,
        "temperature": TEMPERATURE,
        "system": SYSTEM_PROMPT,
        "messages": [
            {"role": "user", "content": user_msg}
        ]
    });
    let resp: serde_json::Value = ureq::post(Provider::Anthropic.endpoint())
        .set("x-api-key", api_key)
        .set("anthropic-version", ANTHROPIC_VERSION)
        .set("Content-Type", "application/json")
        .send_json(body)
        .context("appel API Anthropic")?
        .into_json()
        .context("parsing réponse Anthropic")?;
    resp["content"][0]["text"]
        .as_str()
        .map(|s| s.to_string())
        .ok_or_else(|| anyhow!("champ text absent dans la réponse Anthropic"))
}

/// Extrait le premier objet JSON valide du texte renvoyé par le modèle.
fn parse_verdict(raw: &str) -> Result<Verdict> {
    let start = raw.find('{').ok_or_else(|| anyhow!("pas de JSON"))?;
    let end = raw.rfind('}').ok_or_else(|| anyhow!("JSON non terminé"))?;
    let json = &raw[start..=end];
    let v: Verdict = serde_json::from_str(json)?;
    Ok(v)
}
