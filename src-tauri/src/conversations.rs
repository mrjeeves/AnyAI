//! Filesystem-backed conversation store. One JSON file per conversation
//! under `~/.anyai/conversations/<id>.json` (or the user-overridden
//! `conversation_dir` from config).
//!
//! Shared between the Tauri commands the local desktop UI calls and the
//! axum handlers the remote browser shell talks to — both surfaces hit
//! this module so a conversation written by either is immediately visible
//! to the other. The directory is the source of truth; we don't keep an
//! in-memory index.
//!
//! Conversation IDs are restricted to `[a-zA-Z0-9_-]{1,64}` so a remote
//! caller can't path-traverse out of the conversations directory via a
//! crafted `:id`.

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredMessage {
    pub role: String,
    pub content: String,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub thinking: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Conversation {
    pub id: String,
    pub title: String,
    pub mode: String,
    pub model: String,
    pub family: String,
    pub created_at: String,
    pub updated_at: String,
    pub messages: Vec<StoredMessage>,
}

/// Lightweight projection used by the sidebar list — avoids the cost of
/// shipping every full message body just to render N rows of titles.
#[derive(Debug, Clone, Serialize)]
pub struct ConversationMeta {
    pub id: String,
    pub title: String,
    pub mode: String,
    pub updated_at: String,
}

impl From<&Conversation> for ConversationMeta {
    fn from(c: &Conversation) -> Self {
        Self {
            id: c.id.clone(),
            title: c.title.clone(),
            mode: c.mode.clone(),
            updated_at: c.updated_at.clone(),
        }
    }
}

fn dir() -> Result<PathBuf> {
    // Honour the user-overridden conversation_dir if present and absolute;
    // otherwise default to ~/.anyai/conversations. The TS frontend uses
    // the same precedence so the two surfaces never disagree on where to
    // look.
    if let Ok(cfg) = crate::resolver::load_config_value() {
        if let Some(p) = cfg.get("conversation_dir").and_then(|v| v.as_str()) {
            if !p.is_empty() {
                return Ok(PathBuf::from(p));
            }
        }
    }
    Ok(crate::anyai_dir()?.join("conversations"))
}

/// Reject any id that could escape the conversations dir or carry
/// shell-unfriendly characters. The frontend constructor (time-prefixed
/// base36 id) always satisfies this; only adversarial inputs from the LAN
/// HTTP surface would fail here.
fn validate_id(id: &str) -> Result<()> {
    if id.is_empty() || id.len() > 64 {
        return Err(anyhow!("invalid conversation id length"));
    }
    if !id
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(anyhow!("invalid conversation id characters"));
    }
    Ok(())
}

fn path_for(id: &str) -> Result<PathBuf> {
    validate_id(id)?;
    Ok(dir()?.join(format!("{id}.json")))
}

/// Most-recent first. Bad / partial files are skipped silently — a single
/// corrupt conversation shouldn't take down the sidebar.
pub fn list() -> Result<Vec<ConversationMeta>> {
    let d = dir()?;
    if !d.exists() {
        return Ok(vec![]);
    }
    let mut out = Vec::new();
    let entries = std::fs::read_dir(&d).context("read conversations dir")?;
    for entry in entries.flatten() {
        let name = entry.file_name();
        let s = name.to_string_lossy();
        if !s.ends_with(".json") {
            continue;
        }
        let path = entry.path();
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(_) => continue,
        };
        let conv: Conversation = match serde_json::from_str(&text) {
            Ok(c) => c,
            Err(_) => continue,
        };
        out.push(ConversationMeta::from(&conv));
    }
    out.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
    Ok(out)
}

pub fn load(id: &str) -> Result<Option<Conversation>> {
    let path = path_for(id)?;
    if !path.exists() {
        return Ok(None);
    }
    let text = std::fs::read_to_string(&path).context("read conversation file")?;
    let conv: Conversation =
        serde_json::from_str(&text).with_context(|| format!("parse conversation {id}"))?;
    Ok(Some(conv))
}

pub fn save(conv: &Conversation) -> Result<()> {
    let path = path_for(&conv.id)?;
    let d = dir()?;
    std::fs::create_dir_all(&d).context("mkdir conversations dir")?;
    let body = serde_json::to_string_pretty(conv).context("serialize conversation")?;
    std::fs::write(&path, body).context("write conversation file")?;
    Ok(())
}

pub fn delete(id: &str) -> Result<()> {
    let path = path_for(id)?;
    if path.exists() {
        std::fs::remove_file(&path).context("delete conversation file")?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn id_validation_rejects_traversal() {
        assert!(validate_id("../etc/passwd").is_err());
        assert!(validate_id("a/b").is_err());
        assert!(validate_id("a.b").is_err());
        assert!(validate_id("").is_err());
        assert!(validate_id(&"a".repeat(65)).is_err());
    }

    #[test]
    fn id_validation_accepts_normal_ids() {
        assert!(validate_id("abc123").is_ok());
        assert!(validate_id("lq3kx9-ab12cd").is_ok());
        assert!(validate_id("Some_Id-42").is_ok());
    }
}
