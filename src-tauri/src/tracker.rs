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

#[derive(Clone, Debug, serde::Serialize)]
pub struct ScriptLine {
    pub id: i32,
    pub phase: String,
    pub label: String,
    /// Original text with delivery markers intact.
    pub raw: String,
    /// Spoken text, markers stripped. Derived so the fixture cannot drift.
    pub body: String,
}

#[derive(serde::Deserialize)]
struct RawLine {
    id: i32,
    phase: String,
    label: String,
    raw: String,
}

#[derive(serde::Deserialize)]
struct RawScript {
    id: String,
    title: String,
    version: u32,
    lines: Vec<RawLine>,
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct ScriptFile {
    pub id: String,
    pub title: String,
    pub version: u32,
    pub lines: Vec<ScriptLine>,
}

/// Removes delivery markers (`[⬇️ DOWN]`, `[⏸️ 1.5s]`, …) but keeps placeholders such as
/// `[Name]`, mirroring the TypeScript cue parser.
fn strip_annotations(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut rest = raw;
    while let Some(start) = rest.find('[') {
        out.push_str(&rest[..start]);
        match rest[start..].find(']') {
            Some(offset) => {
                let end = start + offset;
                if is_delivery_marker(&rest[start + 1..end]) {
                    out.push(' ');
                } else {
                    out.push_str(&rest[start..=end]);
                }
                rest = &rest[end + 1..];
            }
            None => {
                out.push_str(&rest[start..]);
                rest = "";
                break;
            }
        }
    }
    out.push_str(rest);
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn is_delivery_marker(token: &str) -> bool {
    let letters: String = token
        .chars()
        .filter(|c| c.is_ascii_alphabetic() || *c == '&' || c.is_whitespace())
        .collect();
    let normalised = letters
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_uppercase();

    if matches!(
        normalised.as_str(),
        "DOWN" | "UP" | "SLOW & LOW" | "SLOW AND LOW" | "PACE UP" | "CURIOUS"
    ) {
        return true;
    }
    // Pause marker such as "⏸️ 1.5s": digits plus a trailing unit, no other words.
    token.chars().any(|c| c.is_ascii_digit()) && (normalised.is_empty() || normalised == "S")
}

pub fn load_script() -> Result<ScriptFile, String> {
    let raw: RawScript = serde_json::from_str(SCRIPT_JSON)
        .map_err(|e| format!("script fixture is malformed: {e}"))?;
    Ok(ScriptFile {
        id: raw.id,
        title: raw.title,
        version: raw.version,
        lines: raw
            .lines
            .into_iter()
            .map(|line| ScriptLine {
                body: strip_annotations(&line.raw),
                id: line.id,
                phase: line.phase,
                label: line.label,
                raw: line.raw,
            })
            .collect(),
    })
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
    lines: Vec<ScriptLine>,
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
    pub fn new(lines: Vec<ScriptLine>) -> Result<Self, String> {
        let active = lines.first().ok_or("script has no lines")?.id;
        Ok(Self {
            lines,
            active,
            window: VecDeque::with_capacity(BUFFER_WORDS),
            words_since_cycle: 0,
            pending_id: None,
            pending_count: 0,
            last_change: Instant::now(),
            llm_enabled: true,
        })
    }

    pub fn lines(&self) -> &[ScriptLine] {
        &self.lines
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

        let line_tokens: Vec<HashSet<String>> = self
            .lines
            .iter()
            .map(|line| tokens(&format!("{} {}", line.label, line.body)))
            .collect();

        let mut scored: Vec<(i32, f64, usize)> = self
            .lines
            .iter()
            .zip(line_tokens.iter())
            .map(|(line, words)| {
                let mut score = 0.0;
                let mut hits = 0usize;
                for token in &buffer {
                    if !words.contains(token) {
                        continue;
                    }
                    hits += 1;
                    let df = line_tokens.iter().filter(|set| set.contains(token)).count() as f64;
                    // Distinctive words count more than words every line shares.
                    score += (line_tokens.len() as f64 / df.max(1.0)).ln().max(0.05);
                }
                (line.id, score, hits)
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
        if !self.lines.iter().any(|line| line.id == proposed) {
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
        let ids: Vec<i32> = self.lines.iter().map(|line| line.id).collect();
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

    /// Lines within reach of the current position, used to shrink the LLM prompt. With
    /// line-level tracking this is wider than before, because one sentence is a much smaller
    /// step than one paragraph.
    pub fn candidate_window(&self) -> Vec<ScriptLine> {
        let ids: Vec<i32> = self.lines.iter().map(|line| line.id).collect();
        let index = ids.iter().position(|&id| id == self.active).unwrap_or(0);
        let start = index.saturating_sub(3);
        let end = (index + 4).min(self.lines.len());
        self.lines[start..end].to_vec()
    }
}

const DEEPSEEK_URL: &str = "https://api.deepseek.com/chat/completions";
/// Kept tight so a slow model cannot stall the HUD behind a stale highlight.
const LLM_TIMEOUT_MS: u64 = 900;

const CLASSIFIER_SYSTEM: &str = "You are an invisible real-time sales co-pilot. Your only task \
is to track which script LINE the salesperson is currently delivering. Each line is a single \
spoken sentence with a numeric id. Match semantic intent, not exact wording: if they paraphrase, \
map to the corresponding line. If they are off-script, handling an interruption, or the intent is \
ambiguous, return the current active id. Candidates are limited to the lines supplied. Reply with \
JSON only, exactly {\"active_id\": <int>} where the int is a line id.";

/// Stage 2: asks DeepSeek which line is being delivered. Thinking is disabled and the prompt is
/// capped so the call stays in the few-hundred-millisecond range.
pub async fn classify_with_llm(
    client: &reqwest::Client,
    api_key: &str,
    candidates: &[ScriptLine],
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
        Tracker::new(script.lines).expect("tracker builds")
    }

    #[test]
    fn fixture_loads_as_granular_lines() {
        let script = load_script().unwrap();
        assert_eq!(script.lines.len(), 26);
        assert_eq!(script.lines[0].id, 1);
        assert_eq!(script.lines[25].id, 26);
    }

    #[test]
    fn annotation_stripping_keeps_placeholders_and_drops_markers() {
        let script = load_script().unwrap();
        let first = script.lines.iter().find(|l| l.id == 1).unwrap();
        assert!(
            first.body.contains("[Name]"),
            "placeholder should survive: {}",
            first.body
        );
        for line in &script.lines {
            for marker in ["DOWN", "CURIOUS", "PACE UP", "SLOW & LOW"] {
                assert!(
                    !line.body.contains(marker),
                    "line {} still contains {marker}: {}",
                    line.id,
                    line.body
                );
            }
        }
    }

    #[test]
    fn local_match_finds_the_missed_leads_line_from_a_paraphrase() {
        let mut t = tracker();
        t.push_text("what happens when a lead rings while you are busy on a job site");
        let (id, confident, _) = t.local_match().expect("should match");
        assert_eq!(id, 11);
        assert!(confident, "a distinctive paraphrase should be confident");
    }

    #[test]
    fn local_match_finds_the_project_photos_line() {
        let mut t = tracker();
        t.push_text("all those photos of your past projects do they just sit there");
        let (id, _, _) = t.local_match().expect("should match");
        assert_eq!(id, 10);
    }

    #[test]
    fn local_match_finds_the_money_line() {
        let mut t = tracker();
        t.push_text("you are easily leaving fifteen k on the table every month");
        let (id, _, _) = t.local_match().expect("should match");
        assert_eq!(id, 20);
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
        for _ in 0..40 {
            t.manual(1);
        }
        assert_eq!(t.active_id(), 26);
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
        // Only "driving" overlaps any line (line 12's "driving out to tire-kickers").
        // Everything else here is off-script. This must not be treated as a match.
        t.push_text("my cousin is driving down from Ohio next weekend");
        let (id, confident, _) = t.local_match().expect("there is some overlap");
        assert_eq!(id, 12);
        assert!(
            !confident,
            "one shared word must not count as a confident line match"
        );
    }

    #[test]
    fn candidate_window_stays_near_the_active_line() {
        let mut t = tracker();
        t.consider(5, true);
        let ids: Vec<i32> = t.candidate_window().iter().map(|l| l.id).collect();
        // Three lines back, four forward, clamped at the start of the script.
        assert_eq!(ids, vec![2, 3, 4, 5, 6, 7, 8]);
    }
}
