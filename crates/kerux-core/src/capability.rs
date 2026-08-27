//! Model capability classification.
//!
//! Turns a [`crate::client::ModelInfo`] into a [`CapabilityReport`] describing
//! what a model can do: vision, tool calling, streaming, reasoning, audio.
//!
//! Each capability carries a [`CapabilityStatus`] and a [`CapabilitySource`].
//! Provider-declared data (OpenRouter `architecture.modalities` and
//! `supported_parameters`, Gemini `inputModalities`/`supportedGenerationMethods`)
//! yields `Declared` entries; providers without metadata (Ollama) fall back to
//! id heuristics (`Inferred`). The runtime probe overwrites entries with
//! `Probe`-sourced results via [`CapabilityReport::merge_probe`].

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::client::ModelInfo;

/// A model capability the runtime can classify.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Capability {
    /// Accepts image (or video-frame) input.
    Vision,
    /// Accepts tool / function-call definitions.
    Tools,
    /// Streams response chunks (SSE / streamGenerateContent).
    Streaming,
    /// Reasoning / thinking model (chain-of-thought output).
    Reasoning,
    /// Audio input or output modality.
    Audio,
}

impl Capability {
    /// All capabilities, in canonical display order.
    pub const ALL: [Capability; 5] = [
        Capability::Vision,
        Capability::Tools,
        Capability::Streaming,
        Capability::Reasoning,
        Capability::Audio,
    ];

    /// Short lowercase label used in badges and reports.
    pub fn label(self) -> &'static str {
        match self {
            Capability::Vision => "vision",
            Capability::Tools => "tools",
            Capability::Streaming => "streaming",
            Capability::Reasoning => "reasoning",
            Capability::Audio => "audio",
        }
    }
}

impl std::fmt::Display for Capability {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.label())
    }
}

/// Whether a capability is available on a model.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityStatus {
    Supported,
    Unsupported,
    /// No signal either way; consumers should assume supported for
    /// non-vision capabilities when deciding whether to attempt a feature.
    Unknown,
}

/// Where a capability verdict came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CapabilitySource {
    /// Declared by the provider catalog (modalities, supported_parameters,
    /// supportedGenerationMethods).
    Declared,
    /// Guessed from the model id because the provider exposes no metadata.
    Inferred,
    /// Confirmed by a runtime probe request.
    Probe,
}

/// One classified capability.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapabilityEntry {
    pub status: CapabilityStatus,
    pub source: CapabilitySource,
}

/// Full capability profile for one model.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapabilityReport {
    pub entries: BTreeMap<Capability, CapabilityEntry>,
}

impl CapabilityReport {
    pub fn new() -> Self {
        Self::default()
    }

    /// Status for a capability; missing entries read as `Unknown`.
    pub fn status(&self, cap: Capability) -> CapabilityStatus {
        self.entries
            .get(&cap)
            .map(|e| e.status)
            .unwrap_or(CapabilityStatus::Unknown)
    }

    /// Source of the current verdict, if any.
    pub fn source(&self, cap: Capability) -> Option<CapabilitySource> {
        self.entries.get(&cap).map(|e| e.source)
    }

    pub fn supports(&self, cap: Capability) -> bool {
        self.status(cap) == CapabilityStatus::Supported
    }

    /// Overwrite one entry.
    pub fn set(&mut self, cap: Capability, status: CapabilityStatus, source: CapabilitySource) {
        self.entries.insert(cap, CapabilityEntry { status, source });
    }

    /// Apply verified probe results, overwriting catalog/heuristic verdicts.
    /// Capabilities the probe did not test keep their previous entry.
    pub fn merge_probe(&mut self, results: &[(Capability, CapabilityStatus)]) {
        for &(cap, status) in results {
            self.set(cap, status, CapabilitySource::Probe);
        }
    }

    /// Short labels for supported capabilities, in canonical order.
    /// Intended for model-picker badges.
    pub fn badges(&self) -> Vec<String> {
        Capability::ALL
            .iter()
            .filter(|cap| self.supports(**cap))
            .map(|cap| cap.label().to_string())
            .collect()
    }
}

/// Classify a model from catalog metadata, falling back to id heuristics
/// when the provider declares nothing.
pub fn classify(info: &ModelInfo) -> CapabilityReport {
    let mut report = CapabilityReport::new();
    let id = info.id.to_ascii_lowercase();
    let name = info.display_name.to_ascii_lowercase();

    let in_mods = |needle: &str| has_modality(&info.input_modalities, needle);
    let out_mods = |needle: &str| has_modality(&info.output_modalities, needle);
    let params = supported_parameters(&info.raw);

    // Vision: declared image/video input wins; otherwise id heuristics.
    let vision = if !info.input_modalities.is_empty() {
        (
            in_mods("image") || in_mods("video"),
            CapabilitySource::Declared,
        )
    } else {
        (id_hints_vision(&id), CapabilitySource::Inferred)
    };
    report.set(Capability::Vision, status_of(vision.0), vision.1);

    // Tools: OpenRouter supported_parameters, Gemini tool support, else guess.
    let tools = if let Some(params) = &params {
        (
            params.iter().any(|p| {
                let p = p.to_ascii_lowercase();
                p == "tools" || p == "function_call" || p == "function_calling"
            }),
            CapabilitySource::Declared,
        )
    } else if gemini_declares(&info.raw, "toolConfig") {
        (true, CapabilitySource::Declared)
    } else {
        (id_hints_tools(&id), CapabilitySource::Inferred)
    };
    report.set(Capability::Tools, status_of(tools.0), tools.1);

    // Streaming: every wired provider supports SSE; only explicit negative
    // catalog signals would flip this, and none are known so far.
    report.set(
        Capability::Streaming,
        CapabilityStatus::Supported,
        CapabilitySource::Inferred,
    );

    // Reasoning: never declared in catalogs today; heuristic on id/name and
    // Gemini thinkingConfig support.
    let reasoning = if gemini_declares(&info.raw, "thinkingConfig") {
        (true, CapabilitySource::Declared)
    } else {
        (id_hints_reasoning(&id, &name), CapabilitySource::Inferred)
    };
    report.set(Capability::Reasoning, status_of(reasoning.0), reasoning.1);

    // Audio: declared modalities win, else id hints.
    let audio = if !info.input_modalities.is_empty() || !info.output_modalities.is_empty() {
        (
            in_mods("audio") || out_mods("audio"),
            CapabilitySource::Declared,
        )
    } else {
        (id_hints_audio(&id), CapabilitySource::Inferred)
    };
    report.set(Capability::Audio, status_of(audio.0), audio.1);

    report
}

fn status_of(supported: bool) -> CapabilityStatus {
    if supported {
        CapabilityStatus::Supported
    } else {
        CapabilityStatus::Unsupported
    }
}

fn has_modality(modalities: &[String], needle: &str) -> bool {
    modalities
        .iter()
        .any(|m| m.to_ascii_lowercase().contains(needle))
}

/// OpenRouter `supported_parameters`, lowercased. None when absent.
fn supported_parameters(raw: &serde_json::Value) -> Option<Vec<String>> {
    let arr = raw.get("supported_parameters")?.as_array()?;
    let mut params: Vec<String> = arr
        .iter()
        .filter_map(|v| v.as_str().map(str::to_string))
        .map(|s| s.to_ascii_lowercase())
        .collect();
    params.sort();
    params.dedup();
    Some(params)
}

/// Gemini `supportedGenerationMethods` membership check.
fn gemini_declares(raw: &serde_json::Value, method: &str) -> bool {
    raw.get("supportedGenerationMethods")
        .and_then(|v| v.as_array())
        .is_some_and(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str())
                .any(|m| m.eq_ignore_ascii_case(method))
        })
}

fn id_hints_vision(id: &str) -> bool {
    [
        "gpt-4o",
        "gpt-4-turbo",
        "gpt-4.1",
        "claude-3",
        "claude-sonnet",
        "claude-opus",
        "claude-haiku",
        "gemini",
        "llava",
        "qwen-vl",
        "qwen2-vl",
        "qwen2.5-vl",
        "pixtral",
        "minicpm-v",
        "moondream",
        "bakllava",
        "granite-vision",
    ]
    .iter()
    .any(|hint| id.contains(hint))
}

fn id_hints_tools(id: &str) -> bool {
    [
        "gpt-4",
        "gpt-3.5-turbo",
        "gpt-4o",
        "gpt-4.1",
        "o1",
        "o3",
        "o4-mini",
        "claude-3",
        "claude-sonnet",
        "claude-opus",
        "claude-haiku",
        "gemini-1.5",
        "gemini-2",
        "qwen2.5",
        "qwen3",
        "llama3.1",
        "llama3.2",
        "llama3.3",
        "llama4",
        "mistral-large",
        "mistral-small",
        "mixtral",
        "command-r",
        "deepseek-chat",
        "deepseek-v3",
    ]
    .iter()
    .any(|hint| id.contains(hint))
}

fn id_hints_reasoning(id: &str, name: &str) -> bool {
    if name.contains("reasoning") || name.contains("thinking") {
        return true;
    }
    if id.contains("deepseek-r1")
        || id.contains("qwq")
        || id.contains("-r1")
        || id.contains("reasoning")
        || id.contains("thinking")
    {
        return true;
    }
    // OpenAI o-series: o1 / o3 / o4 prefixes (but never o2, which does not
    // exist, so a plain prefix scan is safe).
    let stripped = id
        .rsplit('/')
        .next()
        .unwrap_or(id)
        .trim_start_matches("openai/")
        .trim_start_matches("openai.");
    stripped.starts_with("o1")
        || stripped.starts_with("o3")
        || stripped.starts_with("o4")
        || stripped == "o1"
}

fn id_hints_audio(id: &str) -> bool {
    ["audio", "tts", "whisper", "speech", "voice", "realtime"]
        .iter()
        .any(|hint| id.contains(hint))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(id: &str, raw: serde_json::Value) -> ModelInfo {
        ModelInfo {
            id: id.to_string(),
            display_name: id.to_string(),
            context_window: None,
            input_modalities: Vec::new(),
            output_modalities: Vec::new(),
            pricing: None,
            raw,
        }
    }

    fn openrouter(id: &str, in_mods: &[&str], params: &[&str]) -> ModelInfo {
        let mut mi = info(id, serde_json::json!({ "supported_parameters": params }));
        mi.input_modalities = in_mods.iter().map(|s| s.to_string()).collect();
        mi.output_modalities = vec!["text".to_string()];
        mi
    }

    #[test]
    fn openrouter_vision_model_is_declared() {
        let report = classify(&openrouter(
            "openai/gpt-4o",
            &["text", "image"],
            &["tools", "temperature"],
        ));
        assert!(report.supports(Capability::Vision));
        assert_eq!(
            report.source(Capability::Vision),
            Some(CapabilitySource::Declared)
        );
        assert!(report.supports(Capability::Tools));
        assert!(!report.supports(Capability::Audio));
    }

    #[test]
    fn openrouter_text_only_model_has_no_vision() {
        let report = classify(&openrouter(
            "openai/gpt-3.5-turbo",
            &["text"],
            &["temperature"],
        ));
        assert_eq!(
            report.status(Capability::Vision),
            CapabilityStatus::Unsupported
        );
        // Declared supported_parameters without "tools" -> declared unsupported.
        assert_eq!(
            report.status(Capability::Tools),
            CapabilityStatus::Unsupported
        );
        assert_eq!(
            report.source(Capability::Tools),
            Some(CapabilitySource::Declared)
        );
    }

    #[test]
    fn gemini_thinking_model_is_declared_reasoning() {
        let mi = info(
            "gemini-2.5-flash",
            serde_json::json!({
                "inputTokenLimit": "1048576",
                "supportedGenerationMethods": ["generateContent", "toolConfig", "thinkingConfig"],
            }),
        );
        let report = classify(&mi);
        assert!(report.supports(Capability::Reasoning));
        assert_eq!(
            report.source(Capability::Reasoning),
            Some(CapabilitySource::Declared)
        );
        assert!(report.supports(Capability::Tools));
        // Gemini catalog exposes no modalities -> vision falls to heuristics.
        assert!(report.supports(Capability::Vision));
        assert_eq!(
            report.source(Capability::Vision),
            Some(CapabilitySource::Inferred)
        );
    }

    #[test]
    fn ollama_llava_is_inferred_vision() {
        let report = classify(&info("llava:13b", serde_json::json!({})));
        assert!(report.supports(Capability::Vision));
        assert_eq!(
            report.source(Capability::Vision),
            Some(CapabilitySource::Inferred)
        );
        assert!(!report.supports(Capability::Tools));
    }

    #[test]
    fn ollama_deepseek_r1_is_inferred_reasoning() {
        let report = classify(&info("deepseek-r1:14b", serde_json::json!({})));
        assert!(report.supports(Capability::Reasoning));
        assert_eq!(
            report.source(Capability::Reasoning),
            Some(CapabilitySource::Inferred)
        );
    }

    #[test]
    fn openai_o_series_is_reasoning_without_false_positives() {
        assert!(classify(&info("o3-mini", serde_json::json!({}))).supports(Capability::Reasoning));
        assert!(classify(&info("openai/o4-mini", serde_json::json!({})))
            .supports(Capability::Reasoning));
        // "gpt-4o" contains "o" but is not an o-series model.
        assert!(!classify(&info("gpt-4o", serde_json::json!({}))).supports(Capability::Reasoning));
    }

    #[test]
    fn streaming_defaults_supported() {
        let report = classify(&info("anything", serde_json::json!({})));
        assert!(report.supports(Capability::Streaming));
    }

    #[test]
    fn audio_model_detected() {
        let mut mi = info("gpt-4o-audio-preview", serde_json::json!({}));
        mi.input_modalities = vec!["text".to_string(), "audio".to_string()];
        let report = classify(&mi);
        assert!(report.supports(Capability::Audio));
        assert_eq!(
            report.source(Capability::Audio),
            Some(CapabilitySource::Declared)
        );
    }

    #[test]
    fn merge_probe_overwrites_catalog_verdicts() {
        let mut report = classify(&openrouter("openai/gpt-4o", &["text", "image"], &["tools"]));
        report.merge_probe(&[
            (Capability::Tools, CapabilityStatus::Unsupported),
            (Capability::Streaming, CapabilityStatus::Supported),
        ]);
        assert_eq!(
            report.status(Capability::Tools),
            CapabilityStatus::Unsupported
        );
        assert_eq!(
            report.source(Capability::Tools),
            Some(CapabilitySource::Probe)
        );
        // Untested capability keeps its catalog verdict.
        assert!(report.supports(Capability::Vision));
        assert_eq!(
            report.source(Capability::Vision),
            Some(CapabilitySource::Declared)
        );
    }

    #[test]
    fn badges_list_supported_capabilities_in_order() {
        let report = classify(&openrouter("openai/gpt-4o", &["text", "image"], &["tools"]));
        assert_eq!(report.badges(), vec!["vision", "tools", "streaming"]);
    }
}
