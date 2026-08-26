//! Taste/Style Profile Learning — schema and confidence-scoring model.
//!
//! Design reference: `research/product-research-t_2be9e216.md` ide #9
//! (inspiration: CommandCode taste registry, byNara "remembers your style").
//!
//! A [`TasteProfile`] is a portable, versioned collection of coding-style
//! preferences (e.g. `"named exports"` at confidence `0.85`) learned from
//! trajectory history. Profiles are the shared currency of the feature:
//!
//! - **Extraction** (see [`PreferenceExtractor`]) parses trajectory records
//!   and emits [`PreferenceObservation`]s, which are folded into a profile
//!   via [`TasteProfile::apply_observation`].
//! - **Injection** renders high-confidence preferences into a system-prompt
//!   block via [`TasteProfile::render_prompt_block`].
//! - **Push/pull** moves profiles between projects through a [`TasteStore`].
//!   The default [`FileTasteStore`] persists one JSON document per profile
//!   name under `<data_root>/taste/<name>.json` (see [`crate::persist`]).
//!   A project-local copy conventionally lives at `<project>/.kerux/taste.json`
//!   (see [`project_taste_path`]).
//!
//! ## Confidence model
//!
//! Every preference counts supporting (`positive`) and contradicting
//! (`negative`) observations. Confidence combines two factors:
//!
//! - **Consistency** — `positive / (positive + negative)`. A preference that
//!   is contradicted half the time can never exceed `0.5`.
//! - **Saturation** — `n / (n + HALF_SATURATION)` with `HALF_SATURATION = 5`.
//!   One observation yields `~0.17`, five yield `0.5`, twenty yield `0.8`;
//!   confidence grows slowly and never quite reaches `1.0`.
//!
//! `confidence = consistency * saturation`, clamped to `[0.0, 1.0]`
//! ([`compute_confidence`]). The score is stored denormalized on the
//! preference and recomputed whenever evidence changes.
//!
//! ## Portable storage format
//!
//! The JSON document *is* the [`TasteProfile`] (`version` field guards
//! forward compatibility; unknown future versions can be detected via
//! [`TASTE_SCHEMA_VERSION`]). Push/pull therefore needs no conversion step:
//! push = load project profile, save to store under a name; pull = load from
//! store, [`TasteProfile::merge`] into the project profile.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::persist;
use crate::trajectory::Trajectory;

/// Current schema version of the portable taste profile format.
pub const TASTE_SCHEMA_VERSION: u32 = 1;

/// Observation count at which the saturation factor reaches `0.5`.
///
/// Higher values make confidence harder to earn; lower values make it
/// twitchier. Five means a handful of consistent observations yields a
/// mid-range score and dozens are needed to approach certainty.
pub const HALF_SATURATION: f32 = 5.0;

/// Compute a confidence score from evidence counts.
///
/// `confidence = (positive / total) * (total / (total + HALF_SATURATION))`,
/// clamped to `[0.0, 1.0]`. Returns `0.0` when there is no evidence.
pub fn compute_confidence(positive: u32, negative: u32) -> f32 {
    let total = positive.saturating_add(negative);
    if total == 0 {
        return 0.0;
    }
    let total_f = total as f32;
    let consistency = positive as f32 / total_f;
    let saturation = total_f / (total_f + HALF_SATURATION);
    (consistency * saturation).clamp(0.0, 1.0)
}

/// Broad grouping for preferences, used for filtering and display.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum PreferenceCategory {
    /// Identifier naming conventions (casing, prefixes, suffixes).
    Naming,
    /// Whitespace, line length, import ordering, formatter choices.
    Formatting,
    /// Structural preferences (composition over inheritance, error style).
    Architecture,
    /// Preferred tools, linters, build systems, test runners.
    Tooling,
    /// Language or framework idioms (e.g. "named exports" in TypeScript).
    #[default]
    Language,
    /// Comment and doc-comment conventions.
    Documentation,
    /// Testing style (TDD, coverage expectations, fixture layout).
    Testing,
    /// Process preferences (commit granularity, branch naming, review flow).
    Workflow,
    /// Anything that does not fit the other categories.
    Other,
}

impl PreferenceCategory {
    /// Stable lowercase name used in serialization and prompts.
    pub fn as_str(&self) -> &'static str {
        match self {
            PreferenceCategory::Naming => "naming",
            PreferenceCategory::Formatting => "formatting",
            PreferenceCategory::Architecture => "architecture",
            PreferenceCategory::Tooling => "tooling",
            PreferenceCategory::Language => "language",
            PreferenceCategory::Documentation => "documentation",
            PreferenceCategory::Testing => "testing",
            PreferenceCategory::Workflow => "workflow",
            PreferenceCategory::Other => "other",
        }
    }
}

impl std::fmt::Display for PreferenceCategory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Where a preference came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PreferenceSource {
    /// Mined automatically from trajectory history.
    Extracted,
    /// Derived by the agent during a session (not from raw history).
    Inferred,
    /// Explicitly stated by the user; wins conflicts on merge.
    Manual,
}

/// A single learned style preference with its evidence counters.
///
/// Preferences are identified by `key` within a profile. `value` is the
/// concrete preference text shown in prompts (e.g. key `"export style"`,
/// value `"named exports"`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TastePreference {
    /// Short stable identifier for the preference dimension.
    pub key: String,
    /// Broad grouping.
    pub category: PreferenceCategory,
    /// Human-readable preference value injected into prompts.
    pub value: String,
    /// Count of observations supporting this value.
    pub positive: u32,
    /// Count of observations contradicting this value.
    pub negative: u32,
    /// Denormalized confidence in `[0.0, 1.0]`; recomputed on evidence change.
    pub confidence: f32,
    /// How this preference was learned.
    pub source: PreferenceSource,
    /// Unix seconds of the first observation.
    pub first_observed_at: i64,
    /// Unix seconds of the most recent observation.
    pub last_observed_at: i64,
}

impl TastePreference {
    /// Total number of observations (supporting and contradicting).
    pub fn observations(&self) -> u32 {
        self.positive.saturating_add(self.negative)
    }

    /// Fraction of observations that support this value (`0.5` with none).
    pub fn consistency(&self) -> f32 {
        let total = self.observations();
        if total == 0 {
            return 0.5;
        }
        self.positive as f32 / total as f32
    }

    /// Recompute [`Self::confidence`] from the current evidence counters.
    pub fn recompute_confidence(&mut self) {
        self.confidence = compute_confidence(self.positive, self.negative);
    }
}

/// One observed data point emitted by an extractor.
///
/// Folding observations into a profile ([`TasteProfile::apply_observation`])
/// is the only way evidence accumulates, so confidence scores always reflect
/// what was actually seen in history.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PreferenceObservation {
    /// Preference dimension this observation belongs to.
    pub key: String,
    /// Broad grouping.
    pub category: PreferenceCategory,
    /// Concrete value observed (e.g. `"named exports"`).
    pub value: String,
    /// `true` = evidence supporting the value, `false` = counter-evidence.
    pub supports: bool,
    /// Evidence weight; normally `1`, higher for strong signals.
    pub weight: u32,
    /// How the observation was obtained.
    pub source: PreferenceSource,
    /// Unix seconds when the behavior was observed.
    pub observed_at: i64,
}

impl PreferenceObservation {
    /// Convenience constructor: weight `1`, source [`PreferenceSource::Extracted`],
    /// timestamp now.
    pub fn new(
        key: impl Into<String>,
        category: PreferenceCategory,
        value: impl Into<String>,
        supports: bool,
    ) -> Self {
        Self {
            key: key.into(),
            category,
            value: value.into(),
            supports,
            weight: 1,
            source: PreferenceSource::Extracted,
            observed_at: now_secs(),
        }
    }
}

/// A portable, versioned collection of learned style preferences.
///
/// Serializes to the portable JSON document used for push/pull between
/// projects. Field defaults keep old documents readable as the schema grows.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TasteProfile {
    /// Schema version; always [`TASTE_SCHEMA_VERSION`] for new profiles.
    #[serde(default = "schema_version_default")]
    pub version: u32,
    /// Profile name (typically the project slug or a user-chosen label).
    pub name: String,
    /// Unix seconds when the profile was created.
    #[serde(default)]
    pub created_at: i64,
    /// Unix seconds of the last modification.
    #[serde(default)]
    pub updated_at: i64,
    /// The learned preferences.
    #[serde(default)]
    pub preferences: Vec<TastePreference>,
    /// Free-form metadata (source project, extractor version, ...).
    #[serde(default)]
    pub metadata: HashMap<String, String>,
}

fn schema_version_default() -> u32 {
    TASTE_SCHEMA_VERSION
}

impl TasteProfile {
    /// Create an empty profile with the given name.
    pub fn new(name: impl Into<String>) -> Self {
        let now = now_secs();
        Self {
            version: TASTE_SCHEMA_VERSION,
            name: name.into(),
            created_at: now,
            updated_at: now,
            preferences: Vec::new(),
            metadata: HashMap::new(),
        }
    }

    /// Parse a profile from its portable JSON representation.
    pub fn from_json(raw: &str) -> serde_json::Result<TasteProfile> {
        serde_json::from_str(raw)
    }

    /// Serialize to pretty JSON (the portable push/pull format).
    pub fn to_json_pretty(&self) -> serde_json::Result<String> {
        serde_json::to_string_pretty(self)
    }

    /// Whether this profile's schema version is supported by this build.
    pub fn is_supported(&self) -> bool {
        self.version <= TASTE_SCHEMA_VERSION
    }

    /// Find a preference by key.
    pub fn find(&self, key: &str) -> Option<&TastePreference> {
        self.preferences.iter().find(|p| p.key == key)
    }

    /// Fold one observation into the profile.
    ///
    /// Matching preferences (same `key` **and** `value`) accumulate evidence;
    /// a new `(key, value)` pair creates a new preference. Competing values
    /// for the same key may coexist here — [`Self::merge`] and
    /// [`Self::render_prompt_block`] resolve conflicts by evidence strength.
    pub fn apply_observation(&mut self, obs: &PreferenceObservation) {
        match self
            .preferences
            .iter_mut()
            .find(|p| p.key == obs.key && p.value == obs.value)
        {
            Some(pref) => {
                if obs.supports {
                    pref.positive = pref.positive.saturating_add(obs.weight);
                } else {
                    pref.negative = pref.negative.saturating_add(obs.weight);
                }
                pref.last_observed_at = pref.last_observed_at.max(obs.observed_at);
                if obs.source == PreferenceSource::Manual {
                    pref.source = PreferenceSource::Manual;
                }
                pref.recompute_confidence();
            }
            None => {
                let (positive, negative) = if obs.supports {
                    (obs.weight, 0)
                } else {
                    (0, obs.weight)
                };
                let mut pref = TastePreference {
                    key: obs.key.clone(),
                    category: obs.category,
                    value: obs.value.clone(),
                    positive,
                    negative,
                    confidence: 0.0,
                    source: obs.source,
                    first_observed_at: obs.observed_at,
                    last_observed_at: obs.observed_at,
                };
                pref.recompute_confidence();
                self.preferences.push(pref);
            }
        }
        self.updated_at = self.updated_at.max(obs.observed_at);
    }

    /// Fold many observations into the profile.
    pub fn apply_observations(&mut self, observations: &[PreferenceObservation]) {
        for obs in observations {
            self.apply_observation(obs);
        }
    }

    /// Merge `other` into `self` (used by pull).
    ///
    /// - Preferences with matching `key` **and** `value` sum their evidence
    ///   counters, widen the observation window, and recompute confidence.
    /// - Same `key` with conflicting `value`: the side with more total
    ///   evidence wins (ties go to the more recently observed one).
    /// - Keys only present in `other` are appended.
    /// - [`PreferenceSource::Manual`] propagates over learned sources.
    /// - Metadata keys missing from `self` are copied over.
    pub fn merge(&mut self, other: &TasteProfile) {
        for other_pref in &other.preferences {
            match self
                .preferences
                .iter_mut()
                .find(|p| p.key == other_pref.key)
            {
                Some(existing) if existing.value == other_pref.value => {
                    existing.positive = existing.positive.saturating_add(other_pref.positive);
                    existing.negative = existing.negative.saturating_add(other_pref.negative);
                    existing.first_observed_at =
                        existing.first_observed_at.min(other_pref.first_observed_at);
                    existing.last_observed_at =
                        existing.last_observed_at.max(other_pref.last_observed_at);
                    if other_pref.source == PreferenceSource::Manual {
                        existing.source = PreferenceSource::Manual;
                    }
                    existing.recompute_confidence();
                }
                Some(existing) => {
                    let other_total = other_pref.observations();
                    let existing_total = existing.observations();
                    if other_total > existing_total
                        || (other_total == existing_total
                            && other_pref.last_observed_at > existing.last_observed_at)
                    {
                        *existing = other_pref.clone();
                    }
                }
                None => self.preferences.push(other_pref.clone()),
            }
        }
        for (key, value) in &other.metadata {
            self.metadata
                .entry(key.clone())
                .or_insert_with(|| value.clone());
        }
        self.updated_at = self.updated_at.max(other.updated_at);
        self.sort_preferences();
    }

    /// Drop preferences below `min_confidence`; returns how many were removed.
    pub fn retain_confident(&mut self, min_confidence: f32) -> usize {
        let before = self.preferences.len();
        self.preferences.retain(|p| p.confidence >= min_confidence);
        before - self.preferences.len()
    }

    /// Deterministic ordering: confidence descending, then key, then value.
    pub fn sort_preferences(&mut self) {
        self.preferences.sort_by(|a, b| {
            b.confidence
                .partial_cmp(&a.confidence)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.key.cmp(&b.key))
                .then_with(|| a.value.cmp(&b.value))
        });
    }

    /// Render the system-prompt injection block.
    ///
    /// Keeps at most `max_items` preferences with `confidence >= min_confidence`,
    /// highest confidence first, at most one entry per key (the strongest
    /// value wins). Returns `None` when nothing qualifies or `max_items == 0`.
    pub fn render_prompt_block(&self, min_confidence: f32, max_items: usize) -> Option<String> {
        if max_items == 0 {
            return None;
        }
        let mut candidates: Vec<&TastePreference> = self
            .preferences
            .iter()
            .filter(|p| p.confidence >= min_confidence)
            .collect();
        candidates.sort_by(|a, b| {
            b.confidence
                .partial_cmp(&a.confidence)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.key.cmp(&b.key))
                .then_with(|| a.value.cmp(&b.value))
        });

        let mut seen = HashSet::new();
        let mut lines = Vec::new();
        for pref in candidates {
            if !seen.insert(pref.key.as_str()) {
                continue;
            }
            lines.push(format!(
                "- {}: {} (confidence {:.2})",
                pref.key, pref.value, pref.confidence
            ));
            if lines.len() == max_items {
                break;
            }
        }
        if lines.is_empty() {
            return None;
        }
        Some(format!(
            "## Learned Coding Style Preferences\n\n\
             Learned from past sessions. Follow these unless the user instructs otherwise.\n\n{}",
            lines.join("\n")
        ))
    }
}

/// Contract for the extraction engine (task `t_ba87b0b7`): inspect
/// trajectories and emit observations for [`TasteProfile::apply_observations`].
pub trait PreferenceExtractor: Send + Sync {
    /// Mine style-preference observations from trajectory history.
    fn extract(&self, trajectories: &[Trajectory]) -> Vec<PreferenceObservation>;
}

/// Contract for portable profile storage (push/pull between projects).
///
/// `name` is a free-form profile label; implementations map it to a
/// storage location (files for [`FileTasteStore`]).
pub trait TasteStore: Send + Sync {
    /// Load a profile by name; `None` when absent or unreadable.
    fn load(&self, name: &str) -> Option<TasteProfile>;
    /// Persist a profile under `name` (overwrite).
    fn save(&self, name: &str, profile: &TasteProfile) -> std::io::Result<()>;
    /// List all stored profile names, sorted.
    fn list(&self) -> std::io::Result<Vec<String>>;
    /// Delete a stored profile; returns whether it existed.
    fn delete(&self, name: &str) -> std::io::Result<bool>;
}

/// File-backed [`TasteStore`]: one pretty-JSON document per profile under a
/// root directory. The default root is `<data_root>/taste/` (see
/// [`crate::persist::data_root`], honoring `KERUX_HOME`).
#[derive(Debug, Clone)]
pub struct FileTasteStore {
    root: PathBuf,
}

impl FileTasteStore {
    /// Store rooted at an explicit directory (tests, custom installs).
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// Default root: `<data_root>/taste`.
    pub fn default_root() -> PathBuf {
        persist::data_dir("taste")
    }

    /// Store using the default root.
    pub fn at_default_root() -> Self {
        Self::new(Self::default_root())
    }

    fn path_for(&self, name: &str) -> PathBuf {
        self.root
            .join(format!("{}.json", persist::sanitize_key(name)))
    }
}

impl TasteStore for FileTasteStore {
    fn load(&self, name: &str) -> Option<TasteProfile> {
        let path = self.path_for(name);
        match persist::read_json::<TasteProfile>(&path) {
            Some(profile) => Some(profile),
            None => {
                if path.exists() {
                    tracing::warn!(%name, "taste profile exists but is unreadable; starting fresh");
                }
                None
            }
        }
    }

    fn save(&self, name: &str, profile: &TasteProfile) -> std::io::Result<()> {
        persist::write_json(&self.path_for(name), profile)
    }

    fn list(&self) -> std::io::Result<Vec<String>> {
        let entries = match std::fs::read_dir(&self.root) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e),
        };
        let mut names = Vec::new();
        for entry in entries {
            let entry = entry?;
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some("json") {
                if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                    names.push(stem.to_string());
                }
            }
        }
        names.sort();
        Ok(names)
    }

    fn delete(&self, name: &str) -> std::io::Result<bool> {
        match std::fs::remove_file(self.path_for(name)) {
            Ok(()) => Ok(true),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(e),
        }
    }
}

/// Conventional project-local profile path: `<project_root>/.kerux/taste.json`.
///
/// Injection reads from here; `pull` writes here from a [`TasteStore`],
/// `push` reads from here into a store.
pub fn project_taste_path(project_root: &Path) -> PathBuf {
    project_root.join(".kerux").join("taste.json")
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx_eq(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-4
    }

    fn obs(key: &str, value: &str, supports: bool, at: i64) -> PreferenceObservation {
        PreferenceObservation {
            key: key.to_string(),
            category: PreferenceCategory::Language,
            value: value.to_string(),
            supports,
            weight: 1,
            source: PreferenceSource::Extracted,
            observed_at: at,
        }
    }

    #[test]
    fn confidence_without_evidence_is_zero() {
        assert_eq!(compute_confidence(0, 0), 0.0);
    }

    #[test]
    fn confidence_saturation_curve() {
        // Half-saturation at 5 consistent observations.
        assert!(approx_eq(compute_confidence(5, 0), 0.5));
        // 1 observation: 1/6 ≈ 0.167.
        assert!(approx_eq(compute_confidence(1, 0), 1.0 / 6.0));
        // 20 observations: 20/25 = 0.8.
        assert!(approx_eq(compute_confidence(20, 0), 0.8));
        // Monotonic in supporting evidence.
        assert!(compute_confidence(10, 0) > compute_confidence(5, 0));
        // Bounded below 1.0.
        assert!(compute_confidence(10_000, 0) < 1.0);
    }

    #[test]
    fn confidence_consistency_penalty() {
        // 8 for, 2 against: consistency 0.8, saturation 10/15.
        let expected = 0.8 * (10.0 / 15.0);
        assert!(approx_eq(compute_confidence(8, 2), expected));
        // Evenly contested evidence caps at half the saturation factor.
        assert!(compute_confidence(5, 5) <= 0.5);
    }

    #[test]
    fn apply_observation_creates_and_accumulates() {
        let base = now_secs();
        let mut profile = TasteProfile::new("demo");
        profile.apply_observation(&obs("export style", "named exports", true, base));
        profile.apply_observation(&obs("export style", "named exports", true, base + 100));
        profile.apply_observation(&obs("export style", "named exports", false, base + 200));

        assert_eq!(profile.preferences.len(), 1);
        let pref = &profile.preferences[0];
        assert_eq!(pref.positive, 2);
        assert_eq!(pref.negative, 1);
        assert_eq!(pref.first_observed_at, base);
        assert_eq!(pref.last_observed_at, base + 200);
        assert!(approx_eq(pref.confidence, (2.0 / 3.0) * (3.0 / 8.0)));
        assert_eq!(profile.updated_at, base + 200);
    }

    #[test]
    fn competing_values_coexist_until_resolved() {
        let mut profile = TasteProfile::new("demo");
        for i in 0..6 {
            profile.apply_observation(&obs("export style", "named exports", true, 100 + i));
        }
        profile.apply_observation(&obs("export style", "default exports", true, 500));
        assert_eq!(profile.preferences.len(), 2);

        // Prompt injection keeps only the strongest value per key.
        let block = profile.render_prompt_block(0.0, 10).unwrap();
        assert!(block.contains("named exports"));
        assert!(!block.contains("default exports"));
    }

    #[test]
    fn merge_sums_evidence_and_widens_window() {
        let mut a = TasteProfile::new("a");
        a.apply_observation(&obs("naming", "snake_case files", true, 100));
        a.apply_observation(&obs("naming", "snake_case files", true, 200));

        let mut b = TasteProfile::new("b");
        b.apply_observation(&obs("naming", "snake_case files", true, 50));
        b.apply_observation(&obs("testing", "pytest over unittest", true, 300));

        a.merge(&b);
        let naming = a.find("naming").unwrap();
        assert_eq!(naming.positive, 3);
        assert_eq!(naming.first_observed_at, 50);
        assert_eq!(naming.last_observed_at, 200);
        assert!(a.find("testing").is_some());
    }

    #[test]
    fn merge_conflicting_values_keeps_stronger_evidence() {
        let mut a = TasteProfile::new("a");
        a.apply_observation(&obs("quotes", "single quotes", true, 100));

        let mut b = TasteProfile::new("b");
        for i in 0..4 {
            b.apply_observation(&obs("quotes", "double quotes", true, 200 + i));
        }

        a.merge(&b);
        let quotes = a.find("quotes").unwrap();
        assert_eq!(quotes.value, "double quotes");
        assert_eq!(quotes.positive, 4);
    }

    #[test]
    fn manual_source_propagates_on_merge() {
        let mut a = TasteProfile::new("a");
        a.apply_observation(&obs("indent", "4 spaces", true, 100));

        let mut b = TasteProfile::new("b");
        let mut manual = obs("indent", "4 spaces", true, 200);
        manual.source = PreferenceSource::Manual;
        b.apply_observation(&manual);

        a.merge(&b);
        assert_eq!(a.find("indent").unwrap().source, PreferenceSource::Manual);
    }

    #[test]
    fn json_roundtrip_preserves_profile() {
        let mut profile = TasteProfile::new("roundtrip");
        profile
            .metadata
            .insert("source_project".to_string(), "kerux".to_string());
        profile.apply_observation(&obs("export style", "named exports", true, 100));
        profile.apply_observation(&obs("export style", "named exports", true, 200));

        let raw = profile.to_json_pretty().unwrap();
        let loaded = TasteProfile::from_json(&raw).unwrap();
        assert_eq!(loaded, profile);
        assert_eq!(loaded.version, TASTE_SCHEMA_VERSION);
        assert!(loaded.is_supported());
    }

    #[test]
    fn json_defaults_fill_missing_fields() {
        let loaded = TasteProfile::from_json(r#"{"name": "legacy"}"#).unwrap();
        assert_eq!(loaded.version, TASTE_SCHEMA_VERSION);
        assert!(loaded.preferences.is_empty());
        assert!(loaded.metadata.is_empty());
    }

    #[test]
    fn retain_confident_prunes_weak_preferences() {
        let mut profile = TasteProfile::new("demo");
        profile.apply_observation(&obs("weak", "maybe", true, 100)); // 1/6
        for i in 0..20 {
            profile.apply_observation(&obs("strong", "yes", true, 100 + i)); // 0.8
        }
        let removed = profile.retain_confident(0.5);
        assert_eq!(removed, 1);
        assert!(profile.find("weak").is_none());
        assert!(profile.find("strong").is_some());
    }

    #[test]
    fn render_prompt_block_filters_and_caps() {
        let mut profile = TasteProfile::new("demo");
        for i in 0..20 {
            profile.apply_observation(&obs("a-strong", "yes", true, 100 + i));
        }
        profile.apply_observation(&obs("b-weak", "maybe", true, 100));

        // Weak preference filtered out.
        let block = profile.render_prompt_block(0.5, 10).unwrap();
        assert!(block.contains("a-strong"));
        assert!(!block.contains("b-weak"));
        assert!(block.starts_with("## Learned Coding Style Preferences"));

        // Cap respected.
        let capped = profile.render_prompt_block(0.0, 1).unwrap();
        assert_eq!(capped.matches("\n- ").count(), 1);

        // Nothing qualifies -> None.
        assert!(profile.render_prompt_block(0.99, 10).is_none());
        assert!(profile.render_prompt_block(0.0, 0).is_none());
        assert!(TasteProfile::new("empty")
            .render_prompt_block(0.0, 10)
            .is_none());
    }

    #[test]
    fn file_store_roundtrip_list_delete() {
        let dir = tempfile::tempdir().unwrap();
        let store = FileTasteStore::new(dir.path());

        assert_eq!(store.list().unwrap(), Vec::<String>::new());
        assert!(store.load("kerux").is_none());

        let mut profile = TasteProfile::new("kerux");
        profile.apply_observation(&obs("export style", "named exports", true, 100));
        store.save("kerux", &profile).unwrap();

        let loaded = store.load("kerux").unwrap();
        assert_eq!(loaded, profile);
        assert_eq!(store.list().unwrap(), vec!["kerux".to_string()]);

        // Names are sanitized for the filesystem.
        store.save("my/project", &profile).unwrap();
        assert!(dir.path().join("my_project.json").exists());

        assert!(store.delete("kerux").unwrap());
        assert!(!store.delete("kerux").unwrap());
        assert!(store.load("kerux").is_none());
    }

    #[test]
    fn file_store_load_corrupt_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        let store = FileTasteStore::new(dir.path());
        std::fs::write(dir.path().join("bad.json"), "{not json").unwrap();
        assert!(store.load("bad").is_none());
    }

    #[test]
    fn project_taste_path_is_dot_kerux() {
        let path = project_taste_path(Path::new("/repo"));
        assert!(path.ends_with(Path::new(".kerux/taste.json")));
    }

    #[test]
    fn preference_consistency_helpers() {
        let mut pref = TastePreference {
            key: "k".to_string(),
            category: PreferenceCategory::Other,
            value: "v".to_string(),
            positive: 0,
            negative: 0,
            confidence: 0.0,
            source: PreferenceSource::Extracted,
            first_observed_at: 0,
            last_observed_at: 0,
        };
        assert_eq!(pref.consistency(), 0.5);
        pref.positive = 3;
        pref.negative = 1;
        assert!(approx_eq(pref.consistency(), 0.75));
        pref.recompute_confidence();
        assert!(approx_eq(pref.confidence, 0.75 * (4.0 / 9.0)));
    }

    #[test]
    fn category_display_matches_serialization() {
        assert_eq!(PreferenceCategory::Language.to_string(), "language");
        let raw = serde_json::to_string(&PreferenceCategory::Naming).unwrap();
        assert_eq!(raw, "\"naming\"");
    }

    #[test]
    fn sorted_order_is_deterministic() {
        let mut profile = TasteProfile::new("demo");
        for i in 0..10 {
            profile.apply_observation(&obs("zeta", "z", true, 100 + i));
        }
        for i in 0..10 {
            profile.apply_observation(&obs("alpha", "a", true, 100 + i));
        }
        profile.apply_observation(&obs("mid", "m", true, 100));
        profile.sort_preferences();
        let keys: Vec<&str> = profile.preferences.iter().map(|p| p.key.as_str()).collect();
        // Equal-confidence ties break alphabetically; weak one last.
        assert_eq!(keys, vec!["alpha", "zeta", "mid"]);
    }

    #[test]
    fn extractor_trait_is_object_safe() {
        struct Noop;
        impl PreferenceExtractor for Noop {
            fn extract(&self, _trajectories: &[Trajectory]) -> Vec<PreferenceObservation> {
                Vec::new()
            }
        }
        let boxed: Box<dyn PreferenceExtractor> = Box::new(Noop);
        assert!(boxed.extract(&[]).is_empty());
    }

    #[test]
    fn metadata_merged_without_overwriting() {
        let mut a = TasteProfile::new("a");
        a.metadata
            .insert("shared".to_string(), "from-a".to_string());
        let mut b = TasteProfile::new("b");
        b.metadata
            .insert("shared".to_string(), "from-b".to_string());
        b.metadata.insert("only-b".to_string(), "b".to_string());
        a.merge(&b);
        assert_eq!(a.metadata.get("shared").unwrap(), "from-a");
        assert_eq!(a.metadata.get("only-b").unwrap(), "b");
    }

    #[test]
    fn store_trait_is_object_safe() {
        let dir = tempfile::tempdir().unwrap();
        let boxed: Box<dyn TasteStore> = Box::new(FileTasteStore::new(dir.path()));
        assert!(boxed.load("x").is_none());
    }
}
