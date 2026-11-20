//! Project guidance belongs to its provider history window, not the developer prompt.
//! Store a local item so forks, retries and restarts retain the applied version and a
//! user writing the same marker text in chat cannot impersonate a previous snapshot.
use crate::history::History;
use anyhow::{Context, Result};
use serde_json::{Value, json};
use std::path::Path;

const KIND: &str = "pico_project_instructions";

pub(crate) fn read(cwd: &Path) -> Result<Option<String>> {
    match std::fs::read_to_string(cwd.join("AGENTS.md")) {
        Ok(text) => Ok(Some(text).filter(|s| !s.trim().is_empty())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).context("Read project AGENTS.md"),
    }
}

pub(crate) fn latest(input: &History<Value>) -> Option<&Value> {
    input.iter().rev().find(|item| item["type"] == KIND)
}

pub(crate) fn apply(input: &mut History<Value>, text: Option<&str>) {
    if latest(input).and_then(|item| item["text"].as_str()) != text {
        input.push(json!({"type": KIND, "text": text}));
    }
}

pub(crate) fn wire_item(item: &Value) -> Value {
    if item["type"] != KIND {
        return item.clone();
    }
    let content = match item["text"].as_str() {
        Some(text) => format!(
            "Project instructions (AGENTS.md). Use this version in place of any earlier AGENTS.md instructions:\n\n{text}"
        ),
        None => "The previously provided AGENTS.md instructions no longer apply.".into(),
    };
    json!({"type": "message", "role": "user", "content": content})
}
