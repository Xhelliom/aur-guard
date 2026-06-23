//! AI review of the PKGBUILD diff via a configurable provider
//! (Groq / OpenAI: chat-completions format; Anthropic: messages format).
//! We ask the model for a structured JSON verdict.

use crate::config::{AiConfig, Provider};
use crate::t;
use anyhow::{anyhow, Context, Result};
use serde::Deserialize;

/// Zero temperature: we want the most deterministic verdict possible.
const TEMPERATURE: f32 = 0.0;
/// Token ceiling for the response (the JSON verdict is short).
const MAX_TOKENS: u32 = 512;
/// Anthropic Messages API version.
const ANTHROPIC_VERSION: &str = "2023-06-01";

#[derive(Debug, Clone, Deserialize)]
pub struct Verdict {
    /// true if the diff looks safe.
    pub safe: bool,
    /// low / medium / high / critical
    pub severity: String,
    /// Short explanation.
    pub summary: String,
}

const SYSTEM_PROMPT: &str =
    "You are a security auditor specialised in Arch Linux PKGBUILDs and the AUR. \
You are given the diff (or the contents) of a PKGBUILD and its scripts. Your role is to \
detect a supply-chain COMPROMISE, not to critique packaging style. \
\
NORMAL and NOT suspicious in itself (do NOT flag): version number bump (pkgver, \
pkgrel), checksum updates (sha256sums/sha512sums/b2sums) accompanying a new version, \
extracting a .deb/.tar, using sed/ln/install/desktop-file to place files, symlinks to \
/usr/bin, downloading from the vendor's usual official domain already present in the \
previous version. \
\
TRULY suspicious (flag, safe=false): a new source pointing to an unusual domain different \
from the vendor, addition of a curl|bash or wget|sh, execution of a downloaded binary/ELF, \
a new pre/post install hook running remote code, obfuscated or encoded code \
(base64/eval/xxd), exfiltration (sending files, env variables, keys) over the network, \
unexpected addition of npm/pip dependencies installed at build time with lifecycle hooks. \
\
Rely only on what the diff shows. Reply ONLY with a JSON object, with no surrounding text: \
{\"safe\": bool, \"severity\": \"low|medium|high|critical\", \"summary\": \"...\"}. \
Set safe=false only if there is a real indicator from the \"truly suspicious\" list.";

/// AI review of a diff with multi-vote confirmation.
///
/// Cost-saving strategy: a single call if the 1st verdict is "safe". If the
/// 1st verdict is a block, we run extra votes (up to `confirm_votes` total) and
/// only confirm the block by majority — which removes false positives caused by
/// the model's non-determinism.
pub fn review_diff(cfg: &AiConfig, pkg: &str, diff: &str) -> Result<Verdict> {
    let first = review_once(cfg, pkg, diff)?;
    let votes = cfg.confirm_votes.max(1);

    // Common case: safe on the 1st call, or multi-vote disabled -> stop here.
    if first.safe || votes <= 1 {
        return Ok(first);
    }

    // The 1st verdict is a block: put it to a vote to confirm it.
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
            // A failed vote does not count but does not abort the procedure.
            Err(e) => eprintln!("  (AI vote failed for {pkg}: {e})"),
        }
    }

    // Block confirmed if a strict majority of votes upholds it.
    if unsafe_count * 2 > total {
        let mut v = last_unsafe;
        v.summary = t!(
            "{} — block confirmed by {}/{} votes",
            v.summary,
            unsafe_count,
            total
        );
        Ok(v)
    } else {
        Ok(Verdict {
            safe: true,
            severity: "low".to_string(),
            summary: t!(
                "initial block NOT confirmed ({}/{} suspicious votes) — allowed",
                unsafe_count,
                total
            ),
        })
    }
}

/// A single call to the model, returning a Verdict.
fn review_once(cfg: &AiConfig, pkg: &str, diff: &str) -> Result<Verdict> {
    let api_key = crate::config::resolve_api_key(cfg).ok_or_else(|| {
        anyhow!(
            "{:?} API key not found (neither in ${} nor in secrets.toml)",
            cfg.provider,
            cfg.key_env_or_default()
        )
    })?;
    let model = cfg.model_or_default();

    let user_msg = format!(
        "Package: {pkg}\nAnalyse this PKGBUILD diff and return your JSON verdict:\n\n{diff}"
    );

    let raw = match cfg.provider {
        Provider::Anthropic => call_anthropic(&api_key, &model, &user_msg)?,
        Provider::Groq | Provider::Openai => {
            call_openai_compatible(cfg.provider, &api_key, &model, &user_msg)?
        }
    };

    parse_verdict(&raw).with_context(|| format!("unusable AI response: {raw}"))
}

/// Chat-completions format (Groq and OpenAI share the same schema).
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
        .context("chat-completions API call")?
        .into_json()
        .context("parsing chat-completions response")?;
    resp["choices"][0]["message"]["content"]
        .as_str()
        .map(|s| s.to_string())
        .ok_or_else(|| anyhow!("missing content field in the response"))
}

/// Anthropic Messages format.
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
        .context("Anthropic API call")?
        .into_json()
        .context("parsing Anthropic response")?;
    resp["content"][0]["text"]
        .as_str()
        .map(|s| s.to_string())
        .ok_or_else(|| anyhow!("missing text field in the Anthropic response"))
}

/// Extracts the first valid JSON object from the text returned by the model.
fn parse_verdict(raw: &str) -> Result<Verdict> {
    let start = raw.find('{').ok_or_else(|| anyhow!("no JSON"))?;
    let end = raw.rfind('}').ok_or_else(|| anyhow!("unterminated JSON"))?;
    let json = &raw[start..=end];
    let v: Verdict = serde_json::from_str(json)?;
    Ok(v)
}
