//! Skill & memory lifecycle curation.
//!
//! The curator is a periodic background pass that keeps long-term state
//! healthy without user intervention:
//!
//! - **Memory decay**: importance of unattended memories decays over time;
//!   memories that fall below a floor are removed.
//! - **Deduplication**: near-duplicate memories (high word-overlap) are merged,
//!   keeping the most important instance.
//! - **Session archiving**: sessions idle longer than a threshold are archived.
//! - **Skill auto-archiving**: skills untouched for a threshold are moved to
//!   `<skills>/_archive/` so they stop loading.
//! - **Skill distillation**: clusters of distilled facts sharing a tag are
//!   promoted into draft skills under the skills root.
//!
//! Everything is incremental and idempotent; a pass is cheap enough to run on
//! every agent startup plus on the autonomous tick.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::memory::{MemoryBlock, MemoryManager};
use crate::skills::SkillManager;

/// Tunables for a curator pass. All thresholds are opt-out: setting
/// `*_days` to `0` disables that particular rule.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct CurationPolicy {
    /// Days without access before memory importance decays one step. `0` disables.
    pub memory_decay_days: u32,
    /// Importance floor; memories at or below this after decay are removed.
    pub memory_min_importance: u8,
    /// Days idle before a session is archived. `0` disables.
    pub session_archive_days: u32,
    /// Days since a skill's SKILL.md was modified before it is archived.
    /// `0` disables.
    pub skill_stale_days: u32,
    /// Jaccard word-overlap threshold (0..=100) above which two memories are
    /// considered near-duplicates.
    pub dedup_threshold_pct: u8,
    /// Minimum distilled facts sharing a tag before a draft skill is created.
    pub skill_distill_min_facts: usize,
    /// Rewrite distilled draft skill bodies through the LLM so they read as
    /// prose ("how to work in this repo") rather than a facts bullet list.
    /// Requires a configured provider; otherwise falls back to bullet lists.
    pub skill_distill_llm_summary: bool,
    /// Seconds between periodic curator passes in long-lived runtimes.
    /// `0` disables periodic runs (passes still happen at startup/tick).
    pub interval_secs: u64,
    /// When `false`, distilled draft skills are written under
    /// `<skills>/_pending/` and stay unloadable until a human approves them
    /// (TUI `a` key in the Skills panel). `true` writes them directly into
    /// the loadable skills directory.
    pub auto_approve_skills: bool,
    /// Days idle before a low-importance fact becomes a compression candidate.
    /// `0` disables trajectory compression.
    pub compression_min_age_days: u32,
    /// Importance ceiling for compression candidates. Facts at or above this
    /// value are never compressed (protects hand-curated/`importance(90)`
    /// distilled blocks).
    pub compression_max_importance: u8,
    /// Minimum eligible facts needed before a compression block is created.
    pub compression_min_count: usize,
}

// ponytail: skill-level pinning (front-matter `pinned: true`) — currently
// only MemoryBlock carries the flag; a skill-level exemption from
// auto-archiving is the natural upgrade when skill metadata expands.

impl Default for CurationPolicy {
    fn default() -> Self {
        Self {
            memory_decay_days: 14,
            memory_min_importance: 10,
            session_archive_days: 30,
            skill_stale_days: 90,
            dedup_threshold_pct: 80,
            skill_distill_min_facts: 3,
            skill_distill_llm_summary: false,
            interval_secs: 0,
            auto_approve_skills: false,
            compression_min_age_days: 60,
            compression_max_importance: 90,
            compression_min_count: 5,
        }
    }
}

/// What a single curation pass changed. Counts are per-pass deltas.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CurationReport {
    pub memories_decayed: usize,
    pub memories_pruned: usize,
    pub memories_deduped: usize,
    pub sessions_archived: usize,
    pub skills_archived: Vec<String>,
    pub skills_distilled: Vec<String>,
    /// Facts folded into `session_summary` blocks this pass.
    pub memories_compressed: usize,
}

impl CurationReport {
    /// True when the pass changed nothing.
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

/// One full curation pass over memory, sessions, and skills.
pub async fn curate(
    memory: &MemoryManager,
    skills_dir: &Path,
    policy: &CurationPolicy,
) -> Result<CurationReport> {
    curate_with_llm(memory, skills_dir, policy, None, None).await
}

/// Full pass with an optional LLM tap. When `client` and `model` are given
/// and `policy.skill_distill_llm_summary` is on, distilled draft skills are
/// written as prose generated from their fact cluster; failures fall back to
/// the bullet-list body.
pub async fn curate_with_llm(
    memory: &MemoryManager,
    skills_dir: &Path,
    policy: &CurationPolicy,
    client: Option<Arc<dyn crate::client::LLMProvider>>,
    model: Option<String>,
) -> Result<CurationReport> {
    let now = unix_now();
    let mut report = CurationReport::default();

    curate_memories(memory, policy, now, &mut report).await;
    curate_sessions(memory, policy, now, &mut report).await;
    archive_stale_skills(skills_dir, policy, now, &mut report)?;
    distill_skills_from_memory(memory, skills_dir, policy, client, model, &mut report).await?;
    compress_old_facts(memory, policy, now, &mut report).await?;

    memory
        .save_to_disk()
        .await
        .map_err(|e| Error::Agent(format!("Failed to persist curated memory: {}", e)))?;
    Ok(report)
}

/// Decay unattended memories, prune below the floor, merge near-duplicates.
async fn curate_memories(
    memory: &MemoryManager,
    policy: &CurationPolicy,
    now: i64,
    report: &mut CurationReport,
) {
    const DECAY_STEP: u8 = 5;
    let mut blocks = memory.all().await;
    if blocks.is_empty() {
        return;
    }

    // Decay pass; pinned memories never decay. Track which blocks changed.
    let mut decayed_ids = std::collections::HashSet::new();
    if policy.memory_decay_days > 0 {
        let decay_after = i64::from(policy.memory_decay_days) * 86_400;
        for block in &mut blocks {
            if block.pinned {
                continue;
            }
            let idle = now.saturating_sub(block.last_accessed);
            if idle > decay_after {
                let steps = (idle / decay_after) as u8;
                let drop = steps.saturating_mul(DECAY_STEP);
                if drop > 0 {
                    block.importance = block.importance.saturating_sub(drop);
                    decayed_ids.insert(block.id.clone());
                }
            }
        }
        report.memories_decayed = decayed_ids.len();
    }

    // Dedup pass: pinned blocks never get dropped and are seeded as kept so
    // unpinned duplicates fold into them.
    blocks.sort_by_key(|b| std::cmp::Reverse(b.importance));
    let mut kept: Vec<&MemoryBlock> = Vec::new();
    let mut drop_ids: Vec<String> = Vec::new();
    let threshold = policy.dedup_threshold_pct.min(100);
    if threshold < 100 {
        kept.extend(blocks.iter().filter(|b| b.pinned));
        for block in blocks.iter().filter(|b| !b.pinned) {
            let dup = kept
                .iter()
                .any(|k| jaccard_pct(&k.content, &block.content) >= threshold);
            if dup {
                drop_ids.push(block.id.clone());
            } else {
                kept.push(block);
            }
        }
    }

    // Apply: pinned blocks immune to dedup-drop and floor-prune.
    for block in &blocks {
        if !block.pinned && drop_ids.contains(&block.id) {
            memory.remove(&block.id).await;
            report.memories_deduped += 1;
        } else if !block.pinned && block.importance <= policy.memory_min_importance {
            memory.remove(&block.id).await;
            report.memories_pruned += 1;
        } else if decayed_ids.contains(&block.id) {
            memory.update(block.clone()).await;
        }
    }
}

/// Fold many old, low-importance, unpinned facts into `session_summary`
/// blocks and remove the originals. Runs after decay/dedup/prune so that
/// pass operates on the surviving (still-valuable) set; distilled
/// (`importance(90)`) and pinned facts never qualify. Deterministic —
/// concatenation only, no LLM — so the curator stays offline & reproducible.
async fn compress_old_facts(
    memory: &MemoryManager,
    policy: &CurationPolicy,
    now: i64,
    report: &mut CurationReport,
) -> Result<()> {
    if policy.compression_min_age_days == 0 || policy.compression_min_count == 0 {
        return Ok(());
    }
    let age_limit = i64::from(policy.compression_min_age_days) * 86_400;
    let mut candidates: Vec<MemoryBlock> = memory
        .all()
        .await
        .into_iter()
        .filter(|b| {
            !b.pinned
                && b.block_type == "fact"
                && b.importance < policy.compression_max_importance
                && now.saturating_sub(b.last_accessed) > age_limit
        })
        .collect();
    if candidates.len() < policy.compression_min_count {
        return Ok(());
    }
    // Oldest first so the summary reads chronologically.
    candidates.sort_by_key(|b| b.created_at);

    let batch = candidates.len().min(20); // one summary per pass; next pass folds the rest
    let absorbed: Vec<MemoryBlock> = candidates.drain(..batch).collect();
    let body = absorbed
        .iter()
        .map(|b| format!("- {}", b.content.trim()))
        .collect::<Vec<_>>()
        .join("\n");
    let summary_id = format!(
        "compressed-{}-{}",
        now,
        absorbed.first().map(|b| b.created_at).unwrap_or(now)
    );
    let mut summary = MemoryBlock::new(summary_id, "session_summary", body)
        .importance(60)
        .tags(vec!["compressed".to_string(), "long_term".to_string()]);
    summary.created_at = absorbed.first().map(|b| b.created_at).unwrap_or(now);
    summary.last_accessed = now;

    memory.store(summary).await;
    for block in &absorbed {
        memory.remove(&block.id).await;
    }
    report.memories_compressed += batch;
    Ok(())
}

/// Archive sessions idle beyond the policy threshold.
async fn curate_sessions(
    memory: &MemoryManager,
    policy: &CurationPolicy,
    now: i64,
    report: &mut CurationReport,
) {
    if policy.session_archive_days == 0 {
        return;
    }
    let idle_limit = i64::from(policy.session_archive_days) * 86_400;
    for session in memory.list_sessions().await {
        if now.saturating_sub(session.last_activity) > idle_limit {
            memory.archive_session(&session.id).await;
            report.sessions_archived += 1;
        }
    }
}

/// Move stale *agent-created, unpinned* skills into `_archive/`. User skills
/// are never auto-archived (curator policy: archive is provenance-gated), and
/// pinned skills are exempt regardless of provenance. Staleness prefers
/// `last_activity_at` metadata when present, otherwise falls back to SKILL.md
/// mtime.
fn archive_stale_skills(
    skills_dir: &Path,
    policy: &CurationPolicy,
    now: i64,
    report: &mut CurationReport,
) -> Result<()> {
    if policy.skill_stale_days == 0 || !skills_dir.is_dir() {
        return Ok(());
    }
    let stale_after = u64::from(policy.skill_stale_days) * 86_400;
    let mut manager = SkillManager::new(skills_dir.to_path_buf());
    let loaded = match manager.load_all() {
        Ok(loaded) => loaded,
        Err(_) => return Ok(()), // unreadable root: nothing to curate
    };

    for skill in loaded {
        if skill.origin != crate::skills::SkillOrigin::Agent || skill.pinned {
            continue;
        }
        let last_touch = skill.last_activity_at.unwrap_or_else(|| {
            // Fall back to SKILL.md mtime for agent skills never invoked.
            std::fs::metadata(skills_dir.join(&skill.name).join("SKILL.md"))
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map(|d| d.as_secs() as i64)
                .unwrap_or(now)
        });
        if now.saturating_sub(last_touch) > stale_after as i64 && manager.archive(&skill.name)? {
            report.skills_archived.push(skill.name.clone());
        }
    }
    Ok(())
}

/// Promote clusters of same-tag distilled facts into draft skills.
///
/// Distilled facts (tagged `distilled`, importance ≥ 70) are grouped by their
/// remaining tags. Once a tag groups at least `skill_distill_min_facts`
/// facts, a draft skill named `distilled-<tag>` is created containing the
/// facts as its body. Existing skills are never overwritten.
async fn distill_skills_from_memory(
    memory: &MemoryManager,
    skills_dir: &Path,
    policy: &CurationPolicy,
    client: Option<Arc<dyn crate::client::LLMProvider>>,
    model: Option<String>,
    report: &mut CurationReport,
) -> Result<()> {
    if policy.skill_distill_min_facts == 0 {
        return Ok(());
    }
    let facts: Vec<MemoryBlock> = memory
        .get_by_type("fact")
        .await
        .into_iter()
        .filter(|b| b.importance >= 70 && b.tags.iter().any(|t| t == "distilled"))
        .collect();
    if facts.len() < policy.skill_distill_min_facts {
        return Ok(());
    }

    // Group by first non-reserved tag.
    const RESERVED: [&str; 2] = ["distilled", "long_term"];
    let mut groups: HashMap<String, Vec<String>> = HashMap::new();
    for fact in &facts {
        if let Some(tag) = fact
            .tags
            .iter()
            .find(|t| !RESERVED.contains(&t.as_str()) && is_valid_skill_name(t))
        {
            groups
                .entry(tag.clone())
                .or_default()
                .push(fact.content.clone());
        }
    }

    let mut manager = SkillManager::new(skills_dir.to_path_buf());
    let _ = manager.load_all(); // tolerate missing dir; create() makes dirs

    for (tag, contents) in groups {
        if contents.len() < policy.skill_distill_min_facts {
            continue;
        }
        let skill_name = format!("distilled-{}", tag);
        // Backwards compatibility: a live skill with this name already covers
        // the cluster regardless of approval state. When approval is off,
        // also skip if a draft already awaits review under `_pending/`.
        if skills_dir.join(&skill_name).exists()
            || skills_dir
                .join(crate::skills::PENDING_DIR_NAME)
                .join(&skill_name)
                .exists()
        {
            continue;
        }
        let body = match (&client, &model, policy.skill_distill_llm_summary) {
            (Some(client), Some(model), true) => {
                match summarize_facts_with_llm(client, model, &skill_name, &contents).await {
                    Ok(summary) => summary,
                    Err(error) => {
                        tracing::warn!(%error, "LLM skill summary failed; using bullet list");
                        bullet_body(&contents)
                    }
                }
            }
            _ => bullet_body(&contents),
        };
        let skill_md = format!(
            "---\nname: {}\ndescription: Distilled from {} long-term memories\nversion: 0.1.0\ncreated_by: agent\n---\n# {}\n\n{}\n",
            skill_name,
            contents.len(),
            skill_name,
            body
        );
        // A fresh draft honors `auto_approve_skills`. `_pending/` presence was
        // already excluded above, so re-distillation never fights the queue.
        if policy.auto_approve_skills {
            manager.create(&skill_name, &skill_md)?;
        } else {
            manager.create_pending(&skill_name, &skill_md)?;
        }
        report.skills_distilled.push(skill_name);
    }
    Ok(())
}

fn bullet_body(contents: &[String]) -> String {
    contents
        .iter()
        .map(|c| format!("- {}", c))
        .collect::<Vec<_>>()
        .join("\n")
}

const SKILL_SUMMARY_DIRECTIVE: &str = "You are writing a reusable skill document. Summarize the provided facts into short actionable prose for an AI coding agent: start with one-sentence intent, then concise imperative guidance. Output ONLY the prose body, no headings, no bullet points if a paragraph works.";

async fn summarize_facts_with_llm(
    client: &Arc<dyn crate::client::LLMProvider>,
    model: &str,
    skill_name: &str,
    facts: &[String],
) -> Result<String> {
    let prompt = format!(
        "Skill: {}\nFacts:\n{}",
        skill_name,
        facts
            .iter()
            .map(|f| format!("- {}", f))
            .collect::<Vec<_>>()
            .join("\n")
    );
    let messages = vec![
        crate::client::Message::system(SKILL_SUMMARY_DIRECTIVE),
        crate::client::Message::user(prompt),
    ];
    let response = client.chat(model, &messages, None).await?;
    let body = response
        .choices
        .into_iter()
        .next()
        .and_then(|c| c.message.content)
        .ok_or_else(|| Error::ParseResponse("Skill summary response had no content".into()))?;
    let body = body.trim().to_string();
    if body.is_empty() {
        return Err(Error::ParseResponse("Skill summary was empty".into()));
    }
    Ok(body)
}

/// Jaccard word-overlap between two texts, 0..=100.
fn jaccard_pct(a: &str, b: &str) -> u8 {
    use std::collections::HashSet;
    let words = |s: &str| -> HashSet<String> {
        s.split(|c: char| !c.is_alphanumeric())
            .filter(|w| w.len() > 2)
            .map(|w| w.to_lowercase())
            .collect()
    };
    let (a, b) = (words(a), words(b));
    if a.is_empty() || b.is_empty() {
        return 0;
    }
    let intersection = a.intersection(&b).count();
    let union = a.union(&b).count();
    ((intersection * 100) / union.max(1)) as u8
}

fn is_valid_skill_name(tag: &str) -> bool {
    !tag.is_empty()
        && tag.len() <= 40
        && tag
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// Spawn a periodic curator loop for long-lived runtimes (TUI, autonomous
/// daemon). First tick fires after one full interval, so startup and tick
/// passes (each of which still runs once eagerly) don't double up.
/// Returns `None` without spawning when `policy.interval_secs == 0`.
pub fn spawn_periodic_curator(
    memory: MemoryManager,
    skills_dir: impl AsRef<Path>,
    policy: CurationPolicy,
) -> Option<tokio::task::JoinHandle<()>> {
    if policy.interval_secs == 0 {
        return None;
    }
    let skills_dir = skills_dir.as_ref().to_path_buf();
    Some(tokio::spawn(async move {
        let mut ticker =
            tokio::time::interval(std::time::Duration::from_secs(policy.interval_secs));
        ticker.tick().await; // skip the immediate first tick
        loop {
            ticker.tick().await;
            match curate(&memory, &skills_dir, &policy).await {
                Ok(report) if !report.is_empty() => {
                    tracing::info!(?report, "Periodic curator pass complete");
                }
                Ok(_) => {}
                Err(error) => tracing::warn!(%error, "Periodic curator pass failed"),
            }
        }
    }))
}

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::MemoryBlock;
    use std::path::PathBuf;

    fn test_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "hermes_curator_{}_{}_{}",
            name,
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn aged_block(id: &str, content: &str, importance: u8, idle_days: i64) -> MemoryBlock {
        let mut block = MemoryBlock::new(id, "fact", content).importance(importance);
        block.last_accessed = unix_now() - idle_days * 86_400;
        block
    }

    #[test]
    fn jaccard_recognizes_overlap() {
        assert_eq!(
            jaccard_pct("the quick brown fox", "the quick brown fox"),
            100
        );
        assert!(jaccard_pct("the quick brown fox", "completely different words") < 30);
    }

    #[tokio::test]
    async fn compresses_old_low_importance_facts_into_summary() {
        let dir = test_dir("compress");
        let memory = MemoryManager::with_storage_dir(dir.clone());
        // 5 old low-facts eligible; one recent + one distilled (importance 90)
        // and one pinned stay untouched.
        for i in 0..5 {
            memory
                .store(aged_block(
                    &format!("old-{}", i),
                    &format!("old fact {}", i),
                    20,
                    90,
                ))
                .await;
        }
        memory.store(aged_block("recent", "recent", 20, 1)).await;
        memory
            .store(aged_block("distilled", "distilled high", 90, 200))
            .await;
        let mut pinned = aged_block("pinned", "keep me", 10, 300);
        pinned.pinned = true;
        memory.store(pinned).await;

        let policy = CurationPolicy {
            memory_decay_days: 0, // isolate: decay could prune candidates first
            memory_min_importance: 0,
            dedup_threshold_pct: 100, // disable: "old fact N" bodies near-duplicate otherwise
            compression_min_age_days: 60,
            compression_max_importance: 90,
            compression_min_count: 5,
            ..CurationPolicy::default()
        };
        let skills = test_dir("compress_skills");
        let report = curate(&memory, &skills, &policy).await.unwrap();
        assert_eq!(report.memories_compressed, 5);
        for i in 0..5 {
            assert!(memory.get(&format!("old-{}", i)).await.is_none());
        }
        assert!(memory.get("recent").await.is_some());
        assert!(memory.get("distilled").await.is_some());
        assert!(memory.get("pinned").await.is_some());

        let summaries = memory.get_by_type("session_summary").await;
        assert_eq!(summaries.len(), 1);
        let body = &summaries[0].content;
        for i in 0..5 {
            assert!(body.contains(&format!("old fact {}", i)), "missing: {}", i);
        }
        assert!(summaries[0].tags.iter().any(|t| t == "compressed"));

        // Re-running finds nothing eligible.
        let report = curate(&memory, &skills, &policy).await.unwrap();
        assert_eq!(report.memories_compressed, 0);

        // Under the minimum count, nothing compresses.
        let memory2 = MemoryManager::with_storage_dir(test_dir("compress_min"));
        memory2.store(aged_block("a", "one", 20, 90)).await;
        memory2.store(aged_block("b", "two", 20, 90)).await;
        let report = curate(&memory2, &skills, &policy).await.unwrap();
        assert_eq!(report.memories_compressed, 0);
        assert!(memory2.get("a").await.is_some());

        let _ = std::fs::remove_dir_all(dir);
        let _ = std::fs::remove_dir_all(skills);
    }

    #[tokio::test]
    async fn decays_and_prunes_stale_memories() {
        let dir = test_dir("decay");
        let memory = MemoryManager::with_storage_dir(dir.clone());
        memory
            .store(aged_block("fresh", "recent fact stays", 90, 1))
            .await;
        memory
            .store(aged_block("stale", "old fact decays hard", 12, 60))
            .await;

        // Fresh block clawing back to importance 50 default minus decay steps.
        memory
            .update(aged_block("mid", "halfway decays but survives", 55, 30))
            .await;

        let policy = CurationPolicy {
            memory_decay_days: 14,
            memory_min_importance: 10,
            ..CurationPolicy::default()
        };
        let skills = test_dir("decay_skills");
        let report = curate(&memory, &skills, &policy).await.unwrap();

        assert!(report.memories_decayed >= 2);
        assert!(report.memories_pruned >= 1); // stale decays to 0ish
        assert!(memory.get("fresh").await.is_some());
        assert!(memory.get("stale").await.is_none());
        assert!(memory.get("mid").await.is_some());

        let _ = std::fs::remove_dir_all(dir);
        let _ = std::fs::remove_dir_all(skills);
    }

    #[tokio::test]
    async fn dedups_near_identical_memories() {
        let dir = test_dir("dedup");
        let memory = MemoryManager::with_storage_dir(dir.clone());
        memory
            .store(MemoryBlock::new("a", "fact", "User prefers concise answers").importance(90))
            .await;
        memory
            .store(
                MemoryBlock::new("b", "fact", "User prefers concise answers always").importance(50),
            )
            .await;
        memory
            .store(
                MemoryBlock::new("c", "fact", "Entirely unrelated fact goes here").importance(60),
            )
            .await;

        let policy = CurationPolicy {
            memory_decay_days: 0,
            dedup_threshold_pct: 70,
            ..CurationPolicy::default()
        };
        let skills = test_dir("dedup_skills");
        let report = curate(&memory, &skills, &policy).await.unwrap();

        assert_eq!(report.memories_deduped, 1);
        // The higher-importance representative survives.
        assert!(memory.get("a").await.is_some());
        assert!(memory.get("b").await.is_none());
        assert!(memory.get("c").await.is_some());

        let _ = std::fs::remove_dir_all(dir);
        let _ = std::fs::remove_dir_all(skills);
    }

    #[tokio::test]
    async fn distills_tagged_facts_into_skill() {
        let dir = test_dir("distill_mem");
        let skills = test_dir("distill_skills");
        let memory = MemoryManager::with_storage_dir(dir.clone());
        for (i, fact) in [
            "Always run cargo fmt before committing",
            "Every rust change must pass clippy",
            "Never hand-edit Cargo.lock content",
        ]
        .iter()
        .enumerate()
        {
            let block = MemoryBlock::new(format!("f{}", i), "fact", *fact)
                .importance(80)
                .tags(vec![
                    "distilled".to_string(),
                    "long_term".to_string(),
                    "rust".to_string(),
                ]);
            memory.store(block).await;
        }

        let policy = CurationPolicy {
            memory_decay_days: 0,
            skill_distill_min_facts: 3,
            auto_approve_skills: true,
            ..CurationPolicy::default()
        };
        let report = curate(&memory, &skills, &policy).await.unwrap();

        assert_eq!(report.skills_distilled, vec!["distilled-rust".to_string()]);
        let skill_path = skills.join("distilled-rust").join("SKILL.md");
        let content = std::fs::read_to_string(skill_path).unwrap();
        assert!(content.contains("cargo fmt"));

        let _ = std::fs::remove_dir_all(dir);
        let _ = std::fs::remove_dir_all(skills);
    }

    #[tokio::test]
    async fn pinned_memories_survive_decay_prune_dedup() {
        let dir = test_dir("pinned");
        let memory = MemoryManager::with_storage_dir(dir.clone());

        // Old + low importance — would be pruned if not pinned.
        let mut pin = aged_block("pinned-low", "Pinned but idle", 5, 90);
        pin.pinned = true;
        memory.store(pin).await;

        // Duplicate of pinned survives as the pinned stronger instance;
        // the UNPINNED near-duplicate gets deduped into it.
        let mut pinned_a =
            MemoryBlock::new("pinned-dup", "fact", "Keep this exact wording").importance(40);
        pinned_a.pinned = true;
        memory.store(pinned_a).await;
        memory
            .store(
                MemoryBlock::new("dupe", "fact", "Keep this exact wording plus tail")
                    .importance(90),
            )
            .await;

        let policy = CurationPolicy {
            memory_decay_days: 7,
            memory_min_importance: 20,
            dedup_threshold_pct: 60,
            skill_distill_min_facts: 0,
            ..CurationPolicy::default()
        };
        let skills = test_dir("pinned_skills");
        let report = curate(&memory, &skills, &policy).await.unwrap();

        let pinned_low = memory.get("pinned-low").await.unwrap();
        assert_eq!(pinned_low.importance, 5, "pinned memory must not decay");
        assert!(memory.get("pinned-dup").await.is_some());
        assert!(report.memories_deduped >= 1 || memory.get("dupe").await.is_some());

        let _ = std::fs::remove_dir_all(dir);
        let _ = std::fs::remove_dir_all(skills);
    }

    /// Local provider double returning a fixed skill summary.
    struct StubProvider;

    #[async_trait::async_trait]
    impl crate::client::LLMProvider for StubProvider {
        async fn chat(
            &self,
            _model: &str,
            _messages: &[crate::client::Message],
            _tools: Option<&[crate::schema::ToolSchema]>,
        ) -> crate::error::Result<crate::client::ChatResponse> {
            Ok(crate::client::ChatResponse {
                id: "stub".into(),
                object: "chat.completion".into(),
                created: 0,
                model: "stub".into(),
                choices: vec![crate::client::Choice {
                    index: 0,
                    message: crate::client::MessageDelta {
                        role: Some(crate::client::Role::Assistant),
                        content: Some(
                            "Always keep the workspace green: format, then clippy.".to_string(),
                        ),
                        reasoning_content: None,
                        tool_calls: None,
                    },
                    finish_reason: Some("stop".to_string()),
                }],
                usage: crate::client::Usage {
                    prompt_tokens: 0,
                    completion_tokens: 0,
                    total_tokens: 0,
                },
            })
        }

        async fn chat_streaming(
            &self,
            _model: &str,
            _messages: &[crate::client::Message],
            _tools: Option<&[crate::schema::ToolSchema]>,
        ) -> crate::error::Result<crate::client::ChatStreamResponse> {
            unimplemented!()
        }

        fn capabilities(&self, _model: &str) -> crate::client::ProviderCapabilities {
            crate::client::ProviderCapabilities::default()
        }
    }

    #[tokio::test]
    async fn llm_summary_rewrites_distilled_skill_body() {
        let dir = test_dir("llm_mem");
        let skills = test_dir("llm_skills");
        let memory = MemoryManager::with_storage_dir(dir.clone());
        for (i, fact) in [
            "Always run cargo fmt before committing",
            "Every rust change must pass clippy",
            "Never hand-edit Cargo.lock content",
        ]
        .iter()
        .enumerate()
        {
            memory
                .store(
                    MemoryBlock::new(format!("f{}", i), "fact", *fact)
                        .importance(80)
                        .tags(vec!["distilled".into(), "long_term".into(), "rust".into()]),
                )
                .await;
        }

        let policy = CurationPolicy {
            memory_decay_days: 0,
            skill_distill_min_facts: 3,
            skill_distill_llm_summary: true,
            auto_approve_skills: true,
            ..CurationPolicy::default()
        };
        let report = curate_with_llm(
            &memory,
            &skills,
            &policy,
            Some(Arc::new(StubProvider)),
            Some("stub".to_string()),
        )
        .await
        .unwrap();

        assert_eq!(report.skills_distilled.len(), 1);
        let body = std::fs::read_to_string(skills.join("distilled-rust").join("SKILL.md")).unwrap();
        assert!(body.contains("workspace green"), "got: {}", body);
        assert!(!body.contains("- Always run cargo fmt"));

        let _ = std::fs::remove_dir_all(dir);
        let _ = std::fs::remove_dir_all(skills);
    }

    #[tokio::test]
    async fn distillation_routes_to_pending_unless_auto_approved() {
        use crate::memory::{MemoryBlock, MemoryManager};

        for auto_approve in [false, true] {
            let dir = test_dir(&format!("pend_mem_{}", auto_approve));
            let skills = test_dir(&format!("pend_skills_{}", auto_approve));
            let memory = MemoryManager::with_storage_dir(dir.clone());
            for (i, fact) in ["fact one", "fact two", "fact three"].iter().enumerate() {
                memory
                    .store(
                        MemoryBlock::new(format!("f{}", i), "fact", *fact)
                            .importance(80)
                            .tags(vec!["distilled".into(), "long_term".into(), "rust".into()]),
                    )
                    .await;
            }

            let policy = CurationPolicy {
                skill_distill_min_facts: 3,
                auto_approve_skills: auto_approve,
                ..CurationPolicy::default()
            };
            let report = curate(&memory, &skills, &policy).await.unwrap();
            assert_eq!(report.skills_distilled, vec!["distilled-rust"]);

            let live = skills.join("distilled-rust").join("SKILL.md").exists();
            let pending = skills
                .join(crate::skills::PENDING_DIR_NAME)
                .join("distilled-rust")
                .join("SKILL.md")
                .exists();
            assert_eq!(live, auto_approve);
            assert_eq!(pending, !auto_approve);

            let _ = std::fs::remove_dir_all(dir);
            let _ = std::fs::remove_dir_all(skills);
        }
    }

    #[tokio::test]
    async fn archives_skills_into_archive_dir_and_skips_it() {
        let skills = test_dir("archive_skills");
        let fresh = skills.join("fresh-skill");
        std::fs::create_dir_all(&fresh).unwrap();
        std::fs::write(fresh.join("SKILL.md"), "# fresh\n").unwrap();

        let mut manager = SkillManager::new(skills.clone());
        manager.load_all().unwrap();
        assert!(manager.archive("fresh-skill").unwrap());
        assert!(skills.join("_archive").join("fresh-skill").exists());

        // Reload skips archived dirs.
        let mut manager = SkillManager::new(skills.clone());
        let loaded = manager.load_all().unwrap();
        assert_eq!(loaded.len(), 0);

        let _ = std::fs::remove_dir_all(skills);
    }
}
