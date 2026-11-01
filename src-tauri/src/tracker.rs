//! Semantic script tracker.
//!
//! Stage 0 gates on new speech, stage 1 is a cheap local lexical match against the block
//! summaries, and stage 2 (DeepSeek) only runs when stage 1 is ambiguous. Calling an LLM
//! on every buffer would cost money every few seconds and let one bad response yank the
//! highlight mid-sentence.
//!
//! Two guards make the highlight stable: hysteresis (a proposal must repeat) and a
//! monotonic bias (moving backwards in the script needs more evidence than moving
//! forwards, because a sales call moves forwards).

use serde::Deserialize;
use std::collections::{HashSet, VecDeque};
use std::time::Instant;

/// Embedded so the shipped app never depends on the repo layout at runtime.
const SCRIPT_JSON: &str = include_str!("../../src/fixtures/remodeler_script.json");

/// How many trailing words the classifier sees.
const BUFFER_WORDS: usize = 18;
/// New words required before a classification cycle runs.
pub const WORDS_PER_CYCLE: usize = 6;
/// Stage 1 confidence margin: best block must beat the runner-up by this ratio.
const LOCAL_MARGIN: f64 = 1.25;
/// Minimum number of distinct shared words before stage 1 may trust itself.
const MIN_HITS: usize = 2;
/// When no other block scores at all, the ratio test is vacuously satisfied, so require a
/// higher absolute floor. Without this a single incidental word ("driving") counts as proof.
const MIN_DOMINANT_SCORE: f64 = 1.6;

#[derive(Clone, Debug, Deserialize, serde::Serialize)]
pub struct ScriptBlock {
    pub id: i32,
    pub phase: String,
    pub label: String,
    pub summary: String,
    #[serde(default)]
    pub body: String,
}

#[derive(Clone, Debug, Deserialize, serde::Serialize)]
pub struct ScriptFile {
    pub id: String,
    pub title: String,
    pub version: u32,
    pub blocks: Vec<ScriptBlock>,
}

pub fn load_script() -> Result<ScriptFile, String> {
    serde_json::from_str(SCRIPT_JSON).map_err(|e| format!("script fixture is malformed: {e}"))
}

const STOPWORDS: &[&str] = &[
    "the", "and", "you", "your", "for", "that", "this", "with", "are", "what", "how", "they",
    "them", "have", "has", "was", "were", "from", "but", "not", "all", "can", "will", "just",
    "out", "about", "into", "when", "then", "than", "there", "here", "does", "did", "doing", "get",
    "got", "one", "two", "our", "its", "we", "us", "me", "my", "so", "if", "or", "of", "to", "in",
    "on", "at", "is", "it", "be", "as", "a", "an", "do", "much", "most", "right", "now", "like",
    "really", "actually", "think", "know",
];

fn tokens(text: &str) -> HashSet<String> {
    text.to_lowercase()
        .split(|c: char| !c.is_alphanumeric() && c != '\'' && c != '-')
        .map(|w| w.trim_matches('\'').to_string())
        .filter(|w| w.len() >= 3 && !STOPWORDS.contains(&w.as_str()))
        .collect()
}

pub struct Tracker {
    blocks: Vec<ScriptBlock>,
    active: i32,
    window: VecDeque<String>,
    words_since_cycle: usize,
    pending_id: Option<i32>,
    pending_count: u8,
    last_change: Instant,
    /// Set false after repeated LLM failures so the HUD degrades instead of stalling.
    pub llm_enabled: bool,
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct TrackerSnapshot {
    pub active_id: i32,
    pub pending_id: Option<i32>,
    pub buffered_words: usize,
    pub seconds_since_change: u64,
    pub llm_enabled: bool,
}

impl Tracker {
    pub fn new(blocks: Vec<ScriptBlock>) -> Result<Self, String> {
        let active = blocks.first().ok_or("script has no blocks")?.id;
        Ok(Self {
            blocks,
            active,
            window: VecDeque::with_capacity(BUFFER_WORDS),
            words_since_cycle: 0,
            pending_id: None,
            pending_count: 0,
            last_change: Instant::now(),
            llm_enabled: true,
        })
    }

    pub fn blocks(&self) -> &[ScriptBlock] {
        &self.blocks
    }

    pub fn active_id(&self) -> i32 {
        self.active
    }

    pub fn spoken_buffer(&self) -> String {
        self.window.iter().cloned().collect::<Vec<_>>().join(" ")
    }

    /// Adds spoken words to the rolling window; returns how many were new.
    pub fn push_text(&mut self, text: &str) -> usize {
        let mut added = 0;
        for word in text.split_whitespace() {
            if self.window.len() == BUFFER_WORDS {
                self.window.pop_front();
            }
            self.window.push_back(word.to_string());
            added += 1;
        }
        self.words_since_cycle += added;
        added
    }

    /// Stage 0: is there enough new speech to justify a cycle?
    pub fn should_run_cycle(&self) -> bool {
        self.words_since_cycle >= WORDS_PER_CYCLE
    }

    pub fn mark_cycle_ran(&mut self) {
        self.words_since_cycle = 0;
    }

    /// Stage 1: lexical match against block summaries, with rare words weighted higher.
    /// Returns `(best_id, confident, best_score)`.
    pub fn local_match(&self) -> Option<(i32, bool, f64)> {
        let buffer = tokens(&self.spoken_buffer());
        if buffer.is_empty() {
            return None;
        }

        let block_tokens: Vec<HashSet<String>> = self
            .blocks
            .iter()
            .map(|b| tokens(&format!("{} {} {}", b.label, b.summary, b.body)))
            .collect();

        let mut scored: Vec<(i32, f64, usize)> = self
            .blocks
            .iter()
            .zip(block_tokens.iter())
            .map(|(block, words)| {
                let mut score = 0.0;
                let mut hits = 0usize;
                for token in &buffer {
                    if !words.contains(token) {
                        continue;
                    }
                    hits += 1;
                    let df = block_tokens
                        .iter()
                        .filter(|set| set.contains(token))
                        .count() as f64;
                    // Distinctive words count more than words every block shares.
                    score += (block_tokens.len() as f64 / df.max(1.0)).ln().max(0.05);
                }
                (block.id, score, hits)
            })
            .collect();

        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        let (best_id, best, best_hits) = scored[0];
        if best <= 0.0 {
            return None;
        }
        let second = scored.get(1).map(|(_, score, _)| *score).unwrap_or(0.0);

        // A lone shared word must not count as evidence. When every other block scores zero
        // the ratio test passes trivially, which is how off-script speech ("driving down from
        // Ohio") used to jump to a block that merely contains the word "driving".
        let margin_ok = if second <= f64::EPSILON {
            best >= MIN_DOMINANT_SCORE
        } else {
            best >= second * LOCAL_MARGIN
        };
        let confident = best_hits >= MIN_HITS && margin_ok;
        Some((best_id, confident, best))
    }

    /// Applies hysteresis. Returns the new active id when a change is committed.
    pub fn consider(&mut self, proposed: i32, confident: bool) -> Option<i32> {
        if !self.blocks.iter().any(|b| b.id == proposed) {
            return None;
        }
        if proposed == self.active {
            self.pending_id = None;
            self.pending_count = 0;
            return None;
        }

        // Forward moves need less proof than backward moves.
        let required: u8 = if confident {
            1
        } else if proposed > self.active {
            2
        } else {
            3
        };

        if self.pending_id == Some(proposed) {
            self.pending_count = self.pending_count.saturating_add(1);
        } else {
            self.pending_id = Some(proposed);
            self.pending_count = 1;
        }

        if self.pending_count >= required {
            self.active = proposed;
            self.pending_id = None;
            self.pending_count = 0;
            self.window.clear();
            self.words_since_cycle = 0;
            self.last_change = Instant::now();
            Some(proposed)
        } else {
            None
        }
    }

    /// Manual override always wins immediately.
    pub fn manual(&mut self, delta: i32) -> i32 {
        let ids: Vec<i32> = self.blocks.iter().map(|b| b.id).collect();
        let index = ids.iter().position(|&id| id == self.active).unwrap_or(0) as i32;
        let next = (index + delta).clamp(0, ids.len() as i32 - 1) as usize;
        self.active = ids[next];
        self.pending_id = None;
        self.pending_count = 0;
        self.window.clear();
        self.words_since_cycle = 0;
        self.last_change = Instant::now();
        self.active
    }

    pub fn snapshot(&self) -> TrackerSnapshot {
        TrackerSnapshot {
            active_id: self.active,
            pending_id: self.pending_id,
            buffered_words: self.window.len(),
            seconds_since_change: self.last_change.elapsed().as_secs(),
            llm_enabled: self.llm_enabled,
        }
    }

    /// Blocks within reach of the current position, used to shrink the LLM prompt.
    pub fn candidate_window(&self) -> Vec<ScriptBlock> {
        let ids: Vec<i32> = self.blocks.iter().map(|b| b.id).collect();
        let index = ids.iter().position(|&id| id == self.active).unwrap_or(0);
        let start = index.saturating_sub(2);
        let end = (index + 3).min(self.blocks.len());
        self.blocks[start..end].to_vec()
    }
}

const DEEPSEEK_URL: &str = "https://api.deepseek.com/chat/completions";
/// Kept tight so a slow model cannot stall the HUD behind a stale highlight.
const LLM_TIMEOUT_MS: u64 = 900;

const CLASSIFIER_SYSTEM: &str = "You are an invisible real-time sales co-pilot. Your only task \
is to track which script block the salesperson is currently delivering. Match semantic intent, \
not exact wording: if they paraphrase, map to the corresponding concept. If they are off-script, \
handling an interruption, or the intent is ambiguous, return the current active id. Candidates \
are limited to the blocks supplied. Reply with JSON only, exactly {\"active_id\": <int>}.";

/// Stage 2: asks DeepSeek to pick the block. Thinking is disabled and the prompt is capped
/// so the call stays in the few-hundred-millisecond range.
pub async fn classify_with_llm(
    client: &reqwest::Client,
    api_key: &str,
    candidates: &[ScriptBlock],
    active: i32,
    spoken: &str,
) -> Result<i32, String> {
    let blocks_json = serde_json::to_string(candidates).map_err(|e| e.to_string())?;
    let user = format!(
        "candidate_blocks: {blocks_json}\ncurrent_active_id: {active}\nspoken_buffer: \"{spoken}\""
    );

    let body = serde_json::json!({
        "model": "deepseek-flash",
        "messages": [
            { "role": "system", "content": CLASSIFIER_SYSTEM },
            { "role": "user", "content": user }
        ],
        // Thinking defaults to enabled in DeepSeek V4 and would blow the latency budget.
        "thinking": { "type": "disabled" },
        "response_format": { "type": "json_object" },
        "max_tokens": 16,
        "temperature": 0.0,
        "stream": false
    });

    let response = client
        .post(DEEPSEEK_URL)
        .bearer_auth(api_key)
        .json(&body)
        .timeout(std::time::Duration::from_millis(LLM_TIMEOUT_MS))
        .send()
        .await
        .map_err(|e| format!("llm request failed: {e}"))?;

    let status = response.status();
    if !status.is_success() {
        let text = response.text().await.unwrap_or_default();
        let short: String = text.chars().take(180).collect();
        return Err(format!("llm http {status}: {short}"));
    }

    let payload: serde_json::Value = response
        .json()
        .await
        .map_err(|e| format!("llm response not json: {e}"))?;
    let content = payload["choices"][0]["message"]["content"]
        .as_str()
        .ok_or("llm response had no content")?;
    let parsed: serde_json::Value = serde_json::from_str(content)
        .map_err(|e| format!("llm content was not json ({e}): {content}"))?;
    let id = parsed["active_id"]
        .as_i64()
        .ok_or_else(|| format!("llm content missing active_id: {content}"))? as i32;

    // A hallucinated id outside the candidate window must never move the highlight.
    if !candidates.iter().any(|b| b.id == id) {
        return Err(format!("llm returned out-of-window id {id}"));
    }
    Ok(id)
}

/// True when stage 1 cannot settle the block on its own, so the model must be consulted.
/// Confident local matches short-circuit stage 2 and cost nothing.
pub fn needs_llm(local: Option<(i32, bool, f64)>) -> bool {
    !matches!(local, Some((_, true, _)))
}

#[cfg(test)]
mod stage_routing_tests {
    use super::needs_llm;

    #[test]
    fn a_confident_local_match_skips_the_model() {
        assert!(!needs_llm(Some((4, true, 3.2))));
    }

    #[test]
    fn an_unconfident_local_match_routes_to_the_model() {
        assert!(needs_llm(Some((4, false, 1.1))));
    }

    #[test]
    fn no_local_match_at_all_routes_to_the_model() {
        // This is the "off-script / paraphrase" case the fallback exists for.
        assert!(needs_llm(None));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tracker() -> Tracker {
        let script = load_script().expect("fixture loads");
        Tracker::new(script.blocks).expect("tracker builds")
    }

    #[test]
    fn fixture_loads_with_eight_blocks() {
        let script = load_script().unwrap();
        assert_eq!(script.blocks.len(), 8);
        assert_eq!(script.blocks[0].id, 1);
    }

    #[test]
    fn local_match_finds_the_missed_leads_block_from_a_paraphrase() {
        let mut t = tracker();
        t.push_text("what happens when a lead rings while you are busy on a job site");
        let (id, confident, _) = t.local_match().expect("should match");
        assert_eq!(id, 4);
        assert!(confident, "a distinctive paraphrase should be confident");
    }

    #[test]
    fn local_match_finds_the_project_photos_block() {
        let mut t = tracker();
        t.push_text("all those photos of your past projects do they just sit there");
        let (id, _, _) = t.local_match().expect("should match");
        assert_eq!(id, 3);
    }

    #[test]
    fn local_match_finds_the_diagnosis_block() {
        let mut t = tracker();
        t.push_text("leaving fifteen k on the table every month with three operational leaks");
        let (id, _, _) = t.local_match().expect("should match");
        assert_eq!(id, 6);
    }

    #[test]
    fn hysteresis_requires_two_confirmations_for_an_unconfident_forward_move() {
        let mut t = tracker();
        assert_eq!(t.active_id(), 1);
        assert_eq!(t.consider(2, false), None);
        assert_eq!(t.consider(2, false), Some(2));
    }

    #[test]
    fn a_confident_match_commits_immediately() {
        let mut t = tracker();
        assert_eq!(t.consider(3, true), Some(3));
    }

    #[test]
    fn moving_backwards_needs_three_confirmations() {
        let mut t = tracker();
        t.consider(6, true);
        assert_eq!(t.active_id(), 6);
        assert_eq!(t.consider(2, false), None);
        assert_eq!(t.consider(2, false), None);
        assert_eq!(t.consider(2, false), Some(2));
    }

    #[test]
    fn a_disagreeing_proposal_resets_the_pending_count() {
        let mut t = tracker();
        assert_eq!(t.consider(2, false), None);
        assert_eq!(t.consider(3, false), None);
        // Back to 2: the earlier vote for 2 must not carry over.
        assert_eq!(t.consider(2, false), None);
        assert_eq!(t.consider(2, false), Some(2));
    }

    #[test]
    fn an_out_of_range_id_is_ignored() {
        let mut t = tracker();
        assert_eq!(t.consider(99, true), None);
        assert_eq!(t.active_id(), 1);
    }

    #[test]
    fn manual_override_clamps_at_the_ends() {
        let mut t = tracker();
        assert_eq!(t.manual(-1), 1);
        assert_eq!(t.manual(1), 2);
        for _ in 0..20 {
            t.manual(1);
        }
        assert_eq!(t.active_id(), 8);
    }

    #[test]
    fn the_buffer_is_bounded() {
        let mut t = tracker();
        t.push_text(&"word ".repeat(50));
        assert_eq!(t.snapshot().buffered_words, BUFFER_WORDS);
    }

    #[test]
    fn a_single_shared_word_is_not_confident_evidence() {
        let mut t = tracker();
        // Only "driving" overlaps any block (block 4's "driving out to tire-kickers").
        // Everything else is off-script. This must not be treated as a match.
        t.push_text("my cousin is driving down from Ohio next weekend");
        let (id, confident, _) = t.local_match().expect("there is some overlap");
        assert_eq!(id, 4);
        assert!(
            !confident,
            "one shared word must not count as a confident block match"
        );
    }

    #[test]
    fn candidate_window_stays_near_the_active_block() {
        let mut t = tracker();
        t.consider(5, true);
        let ids: Vec<i32> = t.candidate_window().iter().map(|b| b.id).collect();
        assert_eq!(ids, vec![3, 4, 5, 6, 7]);
    }
}
