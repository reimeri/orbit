//! The app's fixed OpenAI catalog. User preferences live in Settings, not here.
//! Capability sources and refresh procedure: .agents/skills/update-openai-models/SKILL.md.
#[derive(Debug)]
pub struct Model {
    pub id: &'static str,
    pub name: &'static str,
    pub efforts: &'static [&'static str],
}

pub const DEFAULT_MODEL: &str = "gpt-6.1-sol";
pub const MODELS: &[Model] = &[
    Model {
        id: "gpt-6-astra",
        name: "GPT-6-Astra",
        efforts: &["low", "medium", "high", "xhigh", "max"],
    },
    Model {
        id: "gpt-6.1-sol",
        name: "GPT-6.1-Sol",
        efforts: &["low", "medium", "high", "xhigh", "max"],
    },
    Model {
        id: "gpt-6-luna",
        name: "GPT-6-Luna",
        efforts: &["none", "low", "medium", "high", "xhigh", "max"],
    },
];

pub fn model(id: &str) -> Option<&'static Model> {
    MODELS.iter().find(|model| model.id == id)
}
