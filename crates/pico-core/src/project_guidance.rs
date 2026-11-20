//! Versioned workspace context belongs to provider history, not the stable base
//! prompt. Local item types distinguish our snapshots from conversation text.
use crate::history::History;
use anyhow::{Context, Result};
use serde_json::{Value, json};
use std::path::Path;

#[derive(Clone, Copy)]
pub(crate) enum Fragment {
    Project,
    Skills,
}
impl Fragment {
    fn kind(self) -> &'static str {
        match self {
            Self::Project => "pico_project_instructions",
            Self::Skills => "pico_skill_catalog",
        }
    }
    pub(crate) fn latest(self, input: &History<Value>) -> Option<&Value> {
        input.iter().rev().find(|item| item["type"] == self.kind())
    }
    pub(crate) fn apply(self, input: &mut History<Value>, text: Option<&str>) {
        if self.latest(input).and_then(|item| item["text"].as_str()) != text {
            input.push(json!({"type": self.kind(), "text": text}));
        }
    }
}

pub(crate) fn read(cwd: &Path) -> Result<Option<String>> {
    match std::fs::read_to_string(cwd.join("AGENTS.md")) {
        Ok(text) => Ok(Some(text).filter(|s| !s.trim().is_empty())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).context("Read project AGENTS.md"),
    }
}

pub(crate) fn wire_item(item: &Value) -> Value {
    if item["type"] == Fragment::Skills.kind() {
        return json!({"type":"message","role":"developer","content":item["text"].as_str().unwrap_or("No skills are currently advertised. This replaces any earlier skill catalog.")});
    }
    if item["type"] != Fragment::Project.kind() {
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
