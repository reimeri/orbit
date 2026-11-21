//! The app's fixed OpenAI catalog. User preferences live in Settings, not here.
//! Capability sources and refresh procedure: .agents/skills/update-openai-models/SKILL.md.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Model {
    pub id: &'static str,
    pub name: &'static str,
    pub efforts: &'static [&'static str],
    pub audio_input: bool,
}

impl std::fmt::Display for Model {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name)
    }
}

const STANDARD_EFFORTS: &[&str] = &["low", "medium", "high", "xhigh", "max"];

pub const ASTRA: Model = Model {
    id: "gpt-6.2-astra",
    name: "Astra",
    audio_input: false,
    efforts: STANDARD_EFFORTS,
};
pub const SOL: Model = Model {
    id: "gpt-6.2-sol",
    name: "Sol",
    audio_input: false,
    efforts: STANDARD_EFFORTS,
};
pub const TERRA: Model = Model {
    id: "gpt-6.1-terra",
    name: "Terra",
    audio_input: false,
    efforts: STANDARD_EFFORTS,
};
pub const LUNA: Model = Model {
    id: "gpt-6-luna",
    name: "Luna",
    audio_input: false,
    efforts: &["none", "low", "medium", "high", "xhigh", "max"],
};

pub const DEFAULT_MODEL: &str = SOL.id;
pub const MODELS: &[Model] = &[ASTRA, SOL, TERRA, LUNA];

pub fn model(id: &str) -> Option<&'static Model> {
    MODELS.iter().find(|model| model.id == id)
}
