//! Semantic script tracker.
//!
//! Stage 0 gates on new speech, stage 1 is a cheap local lexical match against the block
//! summaries, and stage 2 (Jev) only runs when stage 1 is ambiguous. Calling an LLM
//! on every buffer would cost money every few seconds and let one bad response yank the
//! highlight mid-sentence.
//!
//! Two guards make the highlight stable: hysteresis (a proposal must repeat) and a
//! monotonic bias (moving backwards in the script needs more evidence than moving
//! forwards, because a sales call moves forwards).

use std::collections::{HashSet, VecDeque};
use std::time::Instant;

/// Embedded so the shipped app never depends on the repo layout at runtime.
const CALL1_SCRIPT_JSON: &str = include_str!("../../src/fixtures/call1_audit_script.json");
const CALL2_SCRIPT_JSON: &str = include_str!("../../src/fixtures/call2_sales_script.json");
const COLD_CALL_SCRIPT_JSON: &str = include_str!("../../src/fixtures/coldcall_scalix.json");
const LEGACY_SCRIPT_JSON: &str = include_str!("../../src/fixtures/remodeler_script.json");

/// How many trailing words of finalized speech the classifier sees.
const BUFFER_WORDS: usize = 18;
/// New finalized words required before a classification cycle runs.
pub const WORDS_PER_CYCLE: usize = 1;
/// Interim words required before an in-progress sentence can trigger a cycle.
const PARTIAL_WORDS_TRIGGER: usize = 1;
/// Stage 1 confidence margin: best block must beat the runner-up by this ratio.
const LOCAL_MARGIN: f64 = 1.25;
/// Minimum number of distinct shared words before stage 1 may trust itself.
const MIN_HITS: usize = 2;
/// When no other block scores at all, the ratio test is vacuously satisfied, so require a
/// higher absolute floor.
const MIN_DOMINANT_SCORE: f64 = 1.6;
/// Fraction of a line's own content words that must actually have been spoken.
const MIN_COVERAGE: f64 = 0.25;
/// Fraction of the active line the speaker must have delivered before the *next* line may pull
/// the highlight forward early. This is the guard against jumping on the first couple of words.
const EAGER_MIN_ACTIVE_PROGRESS: f64 = 0.5;
/// Coverage of the active line that counts an utterance final as a *completed* line rather than
/// a breath/emphasis pause. High on purpose: a pause mid-sentence must not advance the script.
const COMPLETE_MIN_COVERAGE: f64 = 0.6;
/// When the line's closing words were actually spoken, a lower coverage still counts as complete.
const COMPLETE_TAIL_COVERAGE: f64 = 0.4;
/// Coverage of the active line required when the speaker has plainly begun the next line.
const COMPLETE_NEXT_START_COVERAGE: f64 = 0.3;
/// Coverage + closing word required to complete a line from the *interim* transcript, before the
/// endpointing pause. High enough that an unfinished sentence cannot skip a line, but low enough
/// to fire within an interim of hearing the line's final word (so the highlight leads, not lags).
const LIVE_COMPLETE_COVERAGE: f64 = 0.70;

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

    if normalised.contains("DOWN")
        || normalised.contains("UP")
        || normalised.contains("SLOW & LOW")
        || normalised.contains("SLOW AND LOW")
        || normalised.contains("PACE UP")
        || normalised.contains("CURIOUS")
        || normalised.contains("PAUSE")
    {
        return true;
    }
    // Pause marker such as "⏸️ 1.5s": digits plus a trailing unit 's'
    token.chars().any(|c| c.is_ascii_digit()) && token.to_lowercase().contains('s')
}

pub fn load_script_by_type(call_type: &str) -> Result<ScriptFile, String> {
    let json = match call_type {
        "call2" | "sales" | "close" | "2" => CALL2_SCRIPT_JSON,
        "coldcall" | "cold" | "scalix" => COLD_CALL_SCRIPT_JSON,
        "legacy" | "remodeler" => LEGACY_SCRIPT_JSON,
        _ => CALL1_SCRIPT_JSON,
    };
    let raw: RawScript =
        serde_json::from_str(json).map_err(|e| format!("script fixture is malformed: {e}"))?;
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

pub fn load_script() -> Result<ScriptFile, String> {
    load_script_by_type("call1")
}

const STOPWORDS: &[&str] = &[
    "the", "and", "you", "your", "for", "that", "this", "with", "are", "what", "how", "they",
    "them", "have", "has", "was", "were", "from", "but", "not", "all", "can", "will", "just",
    "out", "about", "into", "when", "then", "than", "there", "here", "does", "did", "doing", "get",
    "got", "one", "two", "our", "its", "we", "us", "me", "my", "so", "if", "or", "of", "to", "in",
    "on", "at", "is", "it", "be", "as", "a", "an", "do", "much", "most", "right", "now", "like",
    "really", "actually", "think", "know",
];

/// Content words in spoken order (stopwords and very short words removed), deduplicated only by
/// the caller. Order matters for detecting a line's *closing* words at a pause.
fn ordered_tokens(text: &str) -> Vec<String> {
    text.to_lowercase()
        .split(|c: char| !c.is_alphanumeric() && c != '\'' && c != '-')
        .map(|w| w.trim_matches('\'').to_string())
        .filter(|w| w.len() >= 3 && !STOPWORDS.contains(&w.as_str()))
        .collect()
}

fn tokens(text: &str) -> HashSet<String> {
    ordered_tokens(text).into_iter().collect()
}

/// The matching vocabulary for a line is the spoken **body only**. `label` and `phase` are
/// navigation chrome the rep never says; folding them in let a single incidental word jump the
/// highlight, because a common word could match a label rather than anything spoken.
fn body_tokens(line: &ScriptLine) -> HashSet<String> {
    tokens(&line.body)
}

/// The words the speaker is actually expected to say, in order. Unlike `body`, this also removes
/// bracketed **placeholders** such as `[Name]` or `[mention 1 specific thing from their website]`,
/// which the rep replaces with a real value — keeping them would make "the whole line" impossible
/// to cover (e.g. line 1's `[Name]` alone would cap coverage at 5/6). Used for completion checks.
fn spoken_content(raw: &str) -> Vec<String> {
    let mut literal = String::with_capacity(raw.len());
    let mut rest = raw;
    while let Some(start) = rest.find('[') {
        literal.push_str(&rest[..start]);
        match rest[start..].find(']') {
            Some(offset) => rest = &rest[start + offset + 1..],
            None => {
                rest = "";
                break;
            }
        }
    }
    literal.push_str(rest);
    ordered_tokens(&literal)
}

pub struct Tracker {
    lines: Vec<ScriptLine>,
    /// Per-line matching vocabulary (spoken body only), precomputed so `local_match` does not
    /// rebuild every line's token set on every interim word.
    body_sets: Vec<HashSet<String>>,
    active: i32,
    window: VecDeque<String>,
    words_since_cycle: usize,
    pending_id: Option<i32>,
    pending_count: u8,
    last_change: Instant,
    /// Latest line that was completed and advanced from, with timestamp, to prevent final STT
    /// echoes from pulling the highlight backward to the sentence just finished.
    last_completed: Option<(i32, Instant)>,
    /// Latest interim transcript for the sentence being spoken right now.
    partial: String,
    /// Partial word count at the last cycle, so the same interim cannot re-trigger.
    partial_marked: usize,
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
        let body_sets: Vec<HashSet<String>> = lines.iter().map(body_tokens).collect();
        Ok(Self {
            lines,
            body_sets,
            active,
            window: VecDeque::with_capacity(BUFFER_WORDS),
            words_since_cycle: 0,
            pending_id: None,
            pending_count: 0,
            last_change: Instant::now(),
            last_completed: None,
            partial: String::new(),
            partial_marked: 0,
            llm_enabled: true,
        })
    }

    pub fn lines(&self) -> &[ScriptLine] {
        &self.lines
    }

    pub fn switch_script(&mut self, lines: Vec<ScriptLine>) -> i32 {
        let first_id = lines.first().map(|l| l.id).unwrap_or(1);
        self.lines = lines;
        self.body_sets = self.lines.iter().map(body_tokens).collect();
        self.active = first_id;
        self.pending_id = None;
        self.pending_count = 0;
        self.window.clear();
        self.partial.clear();
        self.partial_marked = 0;
        self.words_since_cycle = 0;
        self.last_change = Instant::now();
        self.last_completed = None;
        self.active
    }

    pub fn active_id(&self) -> i32 {
        self.active
    }

    pub fn spoken_buffer(&self) -> String {
        let mut parts: Vec<String> = self.window.iter().cloned().collect();
        let partial = self.partial.trim();
        if !partial.is_empty() {
            parts.push(partial.to_string());
        }
        parts.join(" ")
    }

    pub fn is_completed_echo(&self, text: &str) -> bool {
        if let Some((completed_id, completed_at)) = self.last_completed {
            if completed_at.elapsed() < std::time::Duration::from_secs(4) {
                if let Some(comp_line) = self.lines.iter().find(|l| l.id == completed_id) {
                    let comp_toks = tokens(&comp_line.body);
                    let input_toks = tokens(text);
                    let hits = input_toks.iter().filter(|t| comp_toks.contains(*t)).count();
                    if hits >= 1 {
                        let curr_toks = self
                            .lines
                            .iter()
                            .find(|l| l.id == self.active)
                            .map(|l| tokens(&l.body))
                            .unwrap_or_default();
                        let curr_hits =
                            input_toks.iter().filter(|t| curr_toks.contains(*t)).count();
                        if hits > curr_hits {
                            return true;
                        }
                    }
                }
            }
        }
        false
    }

    /// Adds spoken words to the rolling window; returns how many were new.
    pub fn push_text(&mut self, text: &str) -> usize {
        if self.is_completed_echo(text) {
            // Drop finalized echo of the line that was already completed to keep the active line fresh
            return 0;
        }
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

    /// Records the interim transcript of the sentence in progress. Interims are rewritten by
    /// the recogniser, so this replaces rather than appends.
    pub fn set_partial(&mut self, text: &str) {
        self.partial = text.trim().to_string();
    }

    /// A finalized segment supersedes the interim that predicted it, so the tail is dropped
    /// rather than duplicated into the buffer.
    pub fn clear_partial(&mut self) {
        self.partial.clear();
        self.partial_marked = 0;
    }

    /// Stage 0: is there enough new speech to justify a cycle? Interims count, which is what
    /// makes the highlight move while a sentence is still being spoken rather than after it
    /// is finished.
    pub fn should_run_cycle(&self) -> bool {
        if self.words_since_cycle >= WORDS_PER_CYCLE {
            return true;
        }
        let partial_words = self.partial.split_whitespace().count();
        partial_words >= PARTIAL_WORDS_TRIGGER && partial_words != self.partial_marked
    }

    pub fn mark_cycle_ran(&mut self) {
        self.partial_marked = self.partial.split_whitespace().count();
        self.words_since_cycle = 0;
    }

    /// When an utterance is finalized (the speaker paused), decide whether the active line was
    /// actually finished or whether this is only a breath / emphasis pause mid-sentence.
    ///
    /// The old test (`2 hits OR 35%`) completed a line after its opening clause, so a natural
    /// pause — or a Hormozi `[⏸️ PAUSE 1s]` cue — advanced the script while the rep was still
    /// mid-sentence. Completion now needs strong evidence: most of the line, or its closing
    /// words, or the speaker having plainly started the next line.
    pub fn on_utterance_final(&mut self, text: &str) -> Option<i32> {
        let final_tokens = tokens(text);
        if final_tokens.is_empty() {
            return None;
        }

        let complete = {
            let curr_line = self.lines.iter().find(|l| l.id == self.active)?;
            let ordered = spoken_content(&curr_line.raw);
            if ordered.is_empty() {
                // A pause-only line has no words to say; it completes on the first pause.
                true
            } else {
                let body: HashSet<String> = ordered.iter().cloned().collect();
                // Coverage is measured over everything heard for this line so far (recent window
                // plus this final), so a line split by a mid-sentence pause still accumulates.
                let mut heard = tokens(&self.spoken_buffer());
                heard.extend(final_tokens.iter().cloned());
                let hits = heard.iter().filter(|t| body.contains(*t)).count();
                let coverage = hits as f64 / body.len() as f64;
                // Closing words: a breath pause rarely utters the end of the sentence.
                let tail_present = ordered.iter().rev().take(2).any(|t| heard.contains(t));
                // Did the speaker already begin the next line? Then this one is over.
                let next_started = self
                    .lines
                    .iter()
                    .find(|l| l.id > self.active)
                    .map(|next| {
                        let open = spoken_content(&next.raw);
                        open.iter()
                            .take(2)
                            .filter(|t| final_tokens.contains(*t))
                            .count()
                            >= 2
                    })
                    .unwrap_or(false);

                coverage >= COMPLETE_MIN_COVERAGE
                    || (tail_present && coverage >= COMPLETE_TAIL_COVERAGE)
                    || (next_started && coverage >= COMPLETE_NEXT_START_COVERAGE)
            }
        };

        if complete {
            self.complete_active()
        } else {
            None
        }
    }

    /// Advances from the active line to the one after it, if there is one.
    pub fn complete_active(&mut self) -> Option<i32> {
        let next = self.lines.iter().find(|l| l.id > self.active)?.id;
        self.consider(next, true)
    }

    /// True when the running interim has already covered essentially the whole active line — the
    /// speaker reached the end of the sentence — so the highlight can advance the instant the line
    /// is read, without waiting for the ~700ms endpointing pause. Interims are guesses, so this is
    /// deliberately strict (nearly the whole line AND its closing word): a false positive would
    /// skip a line, which is worse than advancing a beat late.
    pub fn active_line_spoken_out(&self) -> bool {
        let Some(line) = self.lines.iter().find(|l| l.id == self.active) else {
            return false;
        };
        let ordered = spoken_content(&line.raw);
        if ordered.is_empty() {
            // Pause-only lines have no words to detect; the final path completes them.
            return false;
        }
        let spoken = tokens(&self.spoken_buffer());
        let body: HashSet<String> = ordered.iter().cloned().collect();
        let hits = spoken.iter().filter(|t| body.contains(*t)).count();
        let coverage = hits as f64 / body.len() as f64;
        let tail_present = ordered.last().map(|t| spoken.contains(t)).unwrap_or(false);
        coverage >= LIVE_COMPLETE_COVERAGE && tail_present
    }

    /// Fraction of the active line's content words present in recent speech (rolling window +
    /// interim). Used to keep the highlight on the current line until it is largely delivered.
    fn active_progress(&self) -> f64 {
        let Some(line) = self.lines.iter().find(|l| l.id == self.active) else {
            return 0.0;
        };
        let body = body_tokens(line);
        if body.is_empty() {
            return 1.0;
        }
        let spoken = tokens(&self.spoken_buffer());
        let hits = body.iter().filter(|t| spoken.contains(*t)).count();
        hits as f64 / body.len() as f64
    }

    /// Stage 1: lexical match against block summaries, with rare words weighted higher.
    /// Returns `(best_id, confident, best_score)`.
    pub fn local_match(&self) -> Option<(i32, bool, f64)> {
        let buffer = tokens(&self.spoken_buffer());
        if buffer.is_empty() {
            return None;
        }

        // Eager forward tracking: the opening of the NEXT line may pull the highlight forward
        // before the utterance is finalized, but ONLY once the active line is substantially
        // delivered. Without this gate the highlight jumped to N+1 on the first two words of N.
        let active_progress = self.active_progress();
        if active_progress >= EAGER_MIN_ACTIVE_PROGRESS {
            let next_line_opt = self
                .lines
                .iter()
                .skip_while(|l| l.id <= self.active)
                .find(|l| !body_tokens(l).is_empty());

            if let Some(next_line) = next_line_opt {
                let next_words = body_tokens(next_line);
                let recent_text = if !self.partial.trim().is_empty() {
                    self.partial.clone()
                } else {
                    self.window
                        .iter()
                        .rev()
                        .take(6)
                        .cloned()
                        .collect::<Vec<_>>()
                        .join(" ")
                };
                let recent_tokens = tokens(&recent_text);
                if !recent_tokens.is_empty() {
                    let next_hits = recent_tokens
                        .iter()
                        .filter(|t| next_words.contains(*t))
                        .count();
                    let curr_words = self
                        .lines
                        .iter()
                        .find(|l| l.id == self.active)
                        .map(body_tokens)
                        .unwrap_or_default();
                    let curr_hits = recent_tokens
                        .iter()
                        .filter(|t| curr_words.contains(*t))
                        .count();
                    let next_coverage = next_hits as f64 / next_words.len() as f64;

                    // The speaker must actually be into the next line, and more into it than
                    // into the current one, before the highlight leads them there.
                    if next_hits >= 2 && next_hits > curr_hits && next_coverage >= 0.2 {
                        return Some((next_line.id, true, 2.0 + next_hits as f64));
                    }
                }
            }
        }

        let line_tokens = &self.body_sets;

        let mut scored: Vec<(i32, f64, usize, usize)> = self
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
                (line.id, score, hits, words.len())
            })
            .collect();

        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        let (best_id, best, best_hits, best_tokens) = scored[0];
        if best <= 0.0 {
            return None;
        }

        // Never propose moving backwards to a line that was completed within the last 4 seconds:
        if let Some((completed_id, completed_at)) = self.last_completed {
            if best_id == completed_id && completed_at.elapsed() < std::time::Duration::from_secs(4)
            {
                return None;
            }
        }

        let second = scored.get(1).map(|(_, score, _, _)| *score).unwrap_or(0.0);

        // The speaker delivers lines in forward sequence: active -> active + 1. The next line is
        // allowed a slightly easier margin so the highlight can lead into it, but it still needs
        // real evidence (two distinct content words), never one incidental word.
        let is_next_line = best_id == self.active + 1;
        let (min_hits, min_dominant, ratio_margin, min_coverage) = if is_next_line {
            (2usize, 1.0, 1.1, 0.22)
        } else {
            (MIN_HITS, MIN_DOMINANT_SCORE, LOCAL_MARGIN, MIN_COVERAGE)
        };

        let margin_ok = if second <= f64::EPSILON {
            best >= min_dominant
        } else {
            best >= second * ratio_margin
        };
        let coverage = if best_tokens == 0 {
            0.0
        } else {
            best_hits as f64 / best_tokens as f64
        };
        let mut confident = best_hits >= min_hits && margin_ok && coverage >= min_coverage;

        // Forward gate: while the active line is still mostly unspoken, a proposal for the next
        // line must be unmistakable (the speaker has clearly moved on) or the highlight holds.
        if is_next_line && active_progress < EAGER_MIN_ACTIVE_PROGRESS {
            confident = best_hits >= 3 && coverage >= 0.35;
        }

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

        // Forward moves need little proof; the highlight should lead the speaker. Backward moves
        // are exceptional and require three confirmations, which is what stops the highlight from
        // oscillating between two adjacent lines as the interim transcript churns at a boundary.
        let required: u8 = if proposed > self.active {
            if confident {
                1
            } else {
                2
            }
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
            let old_active = self.active;
            if proposed > old_active {
                self.last_completed = Some((old_active, Instant::now()));
            }
            self.active = proposed;
            self.pending_id = None;
            self.pending_count = 0;
            self.window.clear();
            self.partial.clear();
            self.partial_marked = 0;
            self.words_since_cycle = 0;
            self.last_change = Instant::now();
            Some(proposed)
        } else {
            None
        }
    }

    /// Manual override always wins immediately and re-anchors at the target line.
    pub fn manual(&mut self, delta: i32) -> i32 {
        let ids: Vec<i32> = self.lines.iter().map(|line| line.id).collect();
        let index = ids.iter().position(|&id| id == self.active).unwrap_or(0) as i32;
        let next = (index + delta).clamp(0, ids.len() as i32 - 1) as usize;
        self.move_to_index(next)
    }

    /// Snaps to a line by index and starts listening fresh from there (manual re-anchor). The
    /// stale audio buffer is dropped so the tracker cannot immediately jump back to a stale match.
    fn move_to_index(&mut self, index: usize) -> i32 {
        if let Some(line) = self.lines.get(index) {
            self.active = line.id;
        }
        self.pending_id = None;
        self.pending_count = 0;
        self.window.clear();
        self.partial.clear();
        self.partial_marked = 0;
        self.words_since_cycle = 0;
        self.last_change = Instant::now();
        self.last_completed = None;
        self.active
    }

    /// Jumps to the previous/next phase (section), so a big correction is one press, not many.
    pub fn jump_phase(&mut self, delta: i32) -> i32 {
        let Some(current_index) = self.lines.iter().position(|l| l.id == self.active) else {
            return self.active;
        };
        let current_phase = self.lines[current_index].phase.clone();
        let target = if delta > 0 {
            self.lines
                .iter()
                .enumerate()
                .skip(current_index + 1)
                .find(|(_, l)| l.phase != current_phase)
                .map(|(i, _)| i)
        } else {
            let first_of_current = self
                .lines
                .iter()
                .position(|l| l.phase == current_phase)
                .unwrap_or(0);
            if current_index > first_of_current {
                Some(first_of_current)
            } else {
                self.lines
                    .iter()
                    .take(first_of_current)
                    .rposition(|l| l.phase != current_phase)
                    .map(|last_of_prev| {
                        let prev_phase = self.lines[last_of_prev].phase.clone();
                        self.lines
                            .iter()
                            .position(|l| l.phase == prev_phase)
                            .unwrap_or(0)
                    })
            }
        };
        match target {
            Some(index) => self.move_to_index(index),
            None => self.active,
        }
    }

    /// Re-anchor at the current line without moving it: drop the stale audio so the tracker starts
    /// listening fresh from where the speaker actually is.
    pub fn reanchor(&mut self) -> i32 {
        let index = self
            .lines
            .iter()
            .position(|l| l.id == self.active)
            .unwrap_or(0);
        self.move_to_index(index)
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

/// Jev (TypeSafe "System One") decision endpoint, reached through OpenRouter so a single
/// OpenRouter key authenticates it. Jev is **not** a chat model: it takes a state plus typed
/// questions and returns a typed answer with probabilities, which is exactly the "which line am
/// I on?" decision and removes the JSON-in-a-string fragility of the old chat completion.
const JEV_URL: &str = "https://openrouter.ai/api/v1/systemone";
/// Pinned so a threshold tuned here keeps meaning the same thing. `~typesafe/jev-latest` tracks
/// the newest release; pin when a confidence floor must stay stable across upgrades.
const JEV_MODEL: &str = "typesafe/jev-1.14";
/// Kept tight so a slow model cannot stall the HUD behind a stale highlight.
const LLM_TIMEOUT_MS: u64 = 900;
/// Minimum confidence for a Jev choice to move the highlight. Below this the answer is treated
/// as "no decision" and the position holds, matching the old model's ambiguity handling.
const JEV_MIN_CONFIDENCE: f64 = 0.55;

/// Instructions for the single `choice` question. Each option's description carries the line
/// text, so the model separates lines by meaning rather than by remembering bare ids.
const JEV_INSTRUCTIONS: &str = "Which script line is the salesperson currently delivering? \
Match semantic intent, not exact wording. If the speech is off-script, an interruption, or \
ambiguous, choose the line whose id equals active_line_id in the state.";

/// Stage 2: asks Jev which line is being delivered. Returns `Ok(Some(id))` for a confident
/// in-window answer, `Ok(None)` when Jev answered but not confidently enough to act (hold
/// position), and `Err` on transport/parse failure so the caller can degrade stage 2.
pub async fn classify_with_llm(
    client: &reqwest::Client,
    api_key: &str,
    candidates: &[ScriptLine],
    active: i32,
    spoken: &str,
) -> Result<Option<i32>, String> {
    // A Choice's option set lives in `criteria`: each key is an option name (here, the line id)
    // and each value describes it. Jev returns the chosen key back in `answers.<id>.choice`.
    let criteria: serde_json::Map<String, serde_json::Value> = candidates
        .iter()
        .map(|line| {
            (
                line.id.to_string(),
                serde_json::Value::String(format!("{}: {}", line.label, line.body)),
            )
        })
        .collect();

    let body = serde_json::json!({
        "model": JEV_MODEL,
        "state": {
            "active_line_id": active,
            "spoken_buffer": spoken,
        },
        "questions": {
            "active_line": {
                "type": "choice",
                "instructions": JEV_INSTRUCTIONS,
                "criteria": criteria,
            }
        }
    });

    let response = client
        .post(JEV_URL)
        .bearer_auth(api_key)
        .json(&body)
        .timeout(std::time::Duration::from_millis(LLM_TIMEOUT_MS))
        .send()
        .await
        .map_err(|e| format!("jev request failed: {e}"))?;

    let status = response.status();
    if !status.is_success() {
        let text = response.text().await.unwrap_or_default();
        let short: String = text.chars().take(180).collect();
        return Err(format!("jev http {status}: {short}"));
    }

    let payload: serde_json::Value = response
        .json()
        .await
        .map_err(|e| format!("jev response not json: {e}"))?;
    parse_jev_choice(&payload, candidates)
}

/// Extracts the chosen line id and confidence from a Jev decision response body.
///
/// Kept pure and separate so the guard rails (out-of-window rejection, low-confidence hold) are
/// unit-testable without a network call — the same discipline the tracker's other guards follow.
fn parse_jev_choice(
    payload: &serde_json::Value,
    candidates: &[ScriptLine],
) -> Result<Option<i32>, String> {
    let answer = payload
        .get("answers")
        .and_then(|a| a.get("active_line"))
        .ok_or_else(|| format!("jev response missing answers.active_line: {payload}"))?;
    let choice = answer
        .get("choice")
        .and_then(|c| c.as_str())
        .ok_or_else(|| format!("jev answer missing choice: {answer}"))?;

    let chosen = choice.trim();
    let id = chosen
        .parse::<i32>()
        .ok()
        .or_else(|| {
            candidates
                .iter()
                .find(|c| c.id.to_string() == chosen)
                .map(|c| c.id)
        })
        .ok_or_else(|| format!("jev choice was not a known line id: {choice}"))?;

    // A hallucinated id outside the candidate window must never move the highlight.
    if !candidates.iter().any(|b| b.id == id) {
        return Err(format!("jev returned out-of-window id {id}"));
    }

    let confidence = answer
        .get("confidence")
        .and_then(|c| c.as_f64())
        .unwrap_or(0.0);
    if confidence < JEV_MIN_CONFIDENCE {
        println!("JEV_LOW_CONFIDENCE id={id} confidence={confidence:.3}");
        return Ok(None);
    }
    Ok(Some(id))
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
mod jev_parse_tests {
    use super::*;

    fn candidates() -> Vec<ScriptLine> {
        vec![
            ScriptLine {
                id: 7,
                phase: "discovery".into(),
                label: "Typical day".into(),
                raw: "raw".into(),
                body: "walk me through a typical day".into(),
            },
            ScriptLine {
                id: 8,
                phase: "discovery".into(),
                label: "Personal time".into(),
                raw: "raw".into(),
                body: "where does your personal time go".into(),
            },
        ]
    }

    #[test]
    fn confident_choice_maps_option_key_to_line_id() {
        let payload = serde_json::json!({
            "model": "typesafe/jev-1.14",
            "answers": {
                "active_line": {
                    "type": "choice",
                    "choice": "8",
                    "probabilities": { "7": 0.1, "8": 0.9 },
                    "confidence": 0.9
                }
            }
        });
        assert_eq!(parse_jev_choice(&payload, &candidates()).unwrap(), Some(8));
    }

    #[test]
    fn low_confidence_choice_holds_position() {
        let payload = serde_json::json!({
            "answers": { "active_line": { "type": "choice", "choice": "7", "confidence": 0.2 } }
        });
        assert_eq!(parse_jev_choice(&payload, &candidates()).unwrap(), None);
    }

    #[test]
    fn out_of_window_choice_is_rejected() {
        let payload = serde_json::json!({
            "answers": { "active_line": { "type": "choice", "choice": "99", "confidence": 0.99 } }
        });
        assert!(parse_jev_choice(&payload, &candidates()).is_err());
    }

    #[test]
    fn missing_answer_is_an_error() {
        let payload = serde_json::json!({ "answers": {} });
        assert!(parse_jev_choice(&payload, &candidates()).is_err());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tracker() -> Tracker {
        let script = load_script_by_type("call1").expect("fixture loads");
        Tracker::new(script.lines).expect("tracker builds")
    }

    #[test]
    fn fixture_loads_both_calls() {
        let call1 = load_script_by_type("call1").unwrap();
        assert_eq!(call1.lines.len(), 31);
        assert_eq!(call1.lines[0].id, 1);
        assert_eq!(call1.lines[30].id, 31);

        let call2 = load_script_by_type("call2").unwrap();
        assert_eq!(call2.lines.len(), 24);
        assert_eq!(call2.lines[0].id, 1);
        assert_eq!(call2.lines[23].id, 24);
    }

    #[test]
    fn coldcall_fixture_loads_with_name_and_niche_placeholders() {
        let cold = load_script_by_type("coldcall").unwrap();
        assert_eq!(cold.lines.len(), 12);
        assert_eq!(cold.lines[0].id, 1);
        assert!(cold.lines.iter().any(|l| l.body.contains("[Name]")));
        assert!(cold.lines.iter().any(|l| l.body.contains("[Niche]")));
    }

    #[test]
    fn coldcall_placeholders_are_not_counted_as_spoken_words() {
        let cold = load_script_by_type("coldcall").unwrap();
        let hook = cold.lines.iter().find(|l| l.id == 5).unwrap();
        let words = spoken_content(&hook.raw);
        assert!(
            !words.iter().any(|w| w == "name" || w == "niche"),
            "placeholders must not enter the matching vocabulary: {words:?}"
        );
    }

    #[test]
    fn annotation_stripping_keeps_placeholders_and_drops_markers() {
        let script = load_script_by_type("call1").unwrap();
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
    fn local_match_finds_call1_speed_to_lead_line() {
        let mut t = tracker();
        t.push_text("high value client calls while on site at eight pm who catches that");
        let (id, confident, _) = t.local_match().expect("should match");
        assert_eq!(id, 16);
        assert!(confident, "a distinctive paraphrase should be confident");
    }

    #[test]
    fn local_match_finds_call1_project_photos_line() {
        let mut t = tracker();
        // The spoken body of line 14: "What happens to those right now—do they mostly just sit there?"
        // (The old input blended in line 13's "photos of past projects" and only matched line 14
        // through its *label*, which is exactly the contamination this matcher no longer allows.)
        t.push_text("what happens to those right now do they mostly just sit there");
        let (id, _, _) = t.local_match().expect("should match");
        assert_eq!(id, 14);
    }

    #[test]
    fn local_match_finds_call1_money_line() {
        let mut t = tracker();
        t.push_text("you are easily leaving fifteen k on the table every month");
        let (id, _, _) = t.local_match().expect("should match");
        assert_eq!(id, 25);
    }

    #[test]
    fn local_match_finds_call2_price_pitch() {
        let call2 = load_script_by_type("call2").unwrap();
        let mut t = Tracker::new(call2.lines).unwrap();
        t.push_text("under our scale accelerate plan flat fifteen hundred a month");
        let (id, _, _) = t.local_match().expect("should match");
        assert!(id == 20 || id == 21, "should match pricing section");
    }

    #[test]
    fn script_switching_resets_tracker() {
        let mut t = tracker();
        t.consider(5, true);
        assert_eq!(t.active_id(), 5);

        let call2 = load_script_by_type("call2").unwrap();
        let new_id = t.switch_script(call2.lines);
        assert_eq!(new_id, 1);
        assert_eq!(t.active_id(), 1);
        assert_eq!(t.lines().len(), 24);
    }

    #[test]
    fn hysteresis_requires_two_confirmations_for_an_unconfident_forward_move() {
        let mut t = tracker();
        assert_eq!(t.active_id(), 1);
        // Non-sequential skip (1 -> 3) requires two confirmations:
        assert_eq!(t.consider(3, false), None);
        assert_eq!(t.consider(3, false), Some(3));
    }

    #[test]
    fn a_confident_match_commits_immediately() {
        let mut t = tracker();
        assert_eq!(t.consider(3, true), Some(3));
    }

    #[test]
    fn a_strong_backward_match_can_recover_a_desynced_tracker() {
        let mut t = tracker();
        // A bad jump left the highlight ahead of the speaker.
        t.consider(5, true);
        assert_eq!(t.active_id(), 5);
        // Backward correction is deliberately conservative: three confident votes for the line
        // the speaker is actually on are required, which prevents boundary oscillation.
        assert_eq!(t.consider(3, true), None);
        assert_eq!(t.consider(3, true), None);
        assert_eq!(t.consider(3, true), Some(3));
        assert_eq!(t.active_id(), 3);
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
        for _ in 0..50 {
            t.manual(1);
        }
        assert_eq!(t.active_id(), 31);
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
        // Only "driving" overlaps line 17's "driving out to tire-kickers".
        t.push_text("my cousin is driving down from Ohio next weekend");
        let (id, confident, _) = t.local_match().expect("there is some overlap");
        assert_eq!(id, 17);
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

    #[test]
    fn two_incidental_words_are_not_enough_coverage() {
        let mut t = tracker();
        // "driving" and "back" both appear in line 17, but together they cover about a fifth.
        t.push_text("my cousin is driving down from Ohio and back");
        let (id, confident, _) = t.local_match().expect("some overlap");
        assert_eq!(id, 17);
        assert!(
            !confident,
            "low coverage must not be treated as a confident match"
        );
    }

    #[test]
    fn an_interim_can_trigger_a_cycle_before_the_sentence_is_final() {
        let mut t = tracker();
        assert!(!t.should_run_cycle());
        t.set_partial("what happens");
        assert!(
            t.should_run_cycle(),
            "an interim must be able to trigger a cycle"
        );
        t.mark_cycle_ran();
        assert!(
            !t.should_run_cycle(),
            "an unchanged interim must not re-trigger"
        );
        t.set_partial("what happens when a lead");
        assert!(t.should_run_cycle(), "a longer interim must trigger again");
    }

    #[test]
    fn the_buffer_includes_the_interim_tail() {
        let mut t = tracker();
        t.push_text("what happens when");
        t.set_partial("a lead calls");
        assert!(t.spoken_buffer().ends_with("a lead calls"));
    }

    #[test]
    fn final_transcript_of_completed_line_does_not_bounce_tracker_back() {
        let mut t = tracker();
        assert_eq!(t.active_id(), 1);

        // Line 1 finishes:
        let next = t.on_utterance_final("good to meet you appreciate you carving out 10 minutes");
        assert_eq!(next, Some(2));
        assert_eq!(t.active_id(), 2, "tracker must be on line 2");

        // Delayed echo of Line 1:
        t.push_text("good to meet you appreciate you carving out 10 minutes");
        if let Some((bounce_id, _, _)) = t.local_match() {
            assert_ne!(bounce_id, 1, "final echo of line 1 must not propose line 1");
        }
        assert_eq!(t.active_id(), 2, "tracker must remain on line 2");
    }

    #[test]
    fn eager_forward_tracking_advances_on_sentence_opening() {
        let mut t = tracker();
        assert_eq!(t.active_id(), 1);

        // User finishes line 1:
        let next = t.on_utterance_final("good to meet you appreciate you carving out 10 minutes");
        assert_eq!(next, Some(2));
        assert_eq!(t.active_id(), 2);

        // While user is speaking the middle of line 2:
        t.set_partial("so we use our time well");
        if let Some((next_id, _, _)) = t.local_match() {
            assert_ne!(next_id, 3, "middle of line 2 must not advance to line 3");
        }

        // When line 2 finishes (utterance final arrives):
        let next3 = t.on_utterance_final("just to set the stage so we use our time well I am not here to pitch you anything today");
        assert_eq!(
            next3,
            Some(3),
            "on_utterance_final must advance to line 3 when line 2 finishes"
        );
        assert_eq!(t.active_id(), 3);
    }

    #[test]
    fn a_final_does_not_duplicate_the_interim_tail() {
        let mut t = tracker();
        t.set_partial("what happens when a lead");
        t.push_text("what happens when a lead");
        t.clear_partial();
        let buffer = t.spoken_buffer();
        assert_eq!(
            buffer.split_whitespace().filter(|w| *w == "lead").count(),
            1,
            "the interim tail must be superseded, not appended: {buffer}"
        );
    }

    // --- regression suite: the "jumps ahead on the first words" / "gets stuck" bugs ---

    #[test]
    fn first_two_words_of_a_line_do_not_jump_to_the_next() {
        let mut t = tracker();
        t.set_partial("name good");
        if let Some((id, confident, _)) = t.local_match() {
            if let Some(jumped) = t.consider(id, confident) {
                panic!("jumped to line {jumped} after only two words of line 1");
            }
        }
        assert_eq!(t.active_id(), 1, "must stay on the line being spoken");
    }

    #[test]
    fn next_line_words_while_still_on_active_do_not_steal_the_highlight() {
        let mut t = tracker();
        // "time" and "pitch" are content words of line 2, uttered before line 1 is delivered.
        t.set_partial("time pitch");
        if let Some((id, confident, _)) = t.local_match() {
            if let Some(jumped) = t.consider(id, confident) {
                panic!("line 2 stole the highlight (now line {jumped}) while line 1 was active");
            }
        }
        assert_eq!(
            t.active_id(),
            1,
            "active line must not move on incidental matches"
        );
    }

    #[test]
    fn a_breath_pause_mid_line_does_not_complete_the_line() {
        let mut t = tracker();
        // Only the opening clause of line 1 has been spoken when the endpoint final arrives.
        assert_eq!(
            t.on_utterance_final("good to meet you"),
            None,
            "a mid-sentence pause must not be treated as a completed line"
        );
        assert_eq!(
            t.active_id(),
            1,
            "a breath pause must not advance the script"
        );
    }

    #[test]
    fn finishing_the_line_then_pausing_advances_cleanly() {
        let mut t = tracker();
        let next =
            t.on_utterance_final("name good to meet you appreciate you carving out ten minutes");
        assert_eq!(
            next,
            Some(2),
            "a fully delivered line must advance on the pause"
        );
        assert_eq!(t.active_id(), 2);
    }

    #[test]
    fn tracker_follows_line_two_after_line_one_without_sticking() {
        let mut t = tracker();
        assert_eq!(
            t.on_utterance_final("name good to meet you appreciate you carving out ten minutes"),
            Some(2)
        );

        // Speaking the middle of line 2 must not jump ahead.
        t.set_partial("just to set the stage so we use our time well");
        if let Some((id, confident, _)) = t.local_match() {
            if let Some(jumped) = t.consider(id, confident) {
                panic!("jumped to line {jumped} mid-line-2");
            }
        }
        assert_eq!(t.active_id(), 2, "must stay on line 2 while delivering it");

        // Finishing line 2 advances to 3.
        let next = t.on_utterance_final(
            "just to set the stage so we use our time well i am not here to pitch you anything today",
        );
        assert_eq!(next, Some(3));
        assert_eq!(t.active_id(), 3);
    }

    #[test]
    fn a_line_split_by_a_pause_still_completes_at_its_real_end() {
        let mut t = tracker();
        // The speaker pauses after the opening clause; the recogniser finalises there.
        assert_eq!(t.on_utterance_final("good to meet you"), None);
        // lib.rs then pushes that final into the rolling window and clears the interim.
        t.push_text("good to meet you");
        t.clear_partial();
        // The speaker resumes and finishes the line; this second final must complete it.
        let next = t.on_utterance_final("appreciate you carving out ten minutes");
        assert_eq!(
            next,
            Some(2),
            "a line split by a pause must complete at its real end, not stay stuck"
        );
        assert_eq!(t.active_id(), 2);
    }

    #[test]
    fn a_full_sentence_final_sequence_advances_without_stalling_or_oscillating() {
        // With the endpointing fix, the recogniser now emits whole-sentence finals instead of
        // one-word fragments. This models reading lines 1-3 end to end and asserts the highlight
        // marches forward 1→2→3→4 with no stall and no bounce-back.
        let mut t = tracker();
        assert_eq!(
            t.on_utterance_final("good to meet you appreciate you carving out ten minutes"),
            Some(2)
        );
        assert_eq!(t.active_id(), 2);
        assert_eq!(
            t.on_utterance_final(
                "just to set the stage so we use our time well i am not here to pitch you anything today"
            ),
            Some(3)
        );
        assert_eq!(t.active_id(), 3);
        assert_eq!(
            t.on_utterance_final(
                "every firm runs their day to day differently so i dont even know where ai makes the most sense for you yet"
            ),
            Some(4)
        );
        assert_eq!(t.active_id(), 4);
    }

    #[test]
    fn an_interim_that_speaks_the_whole_line_is_complete_instantly() {
        let mut t = tracker();
        t.set_partial("good to meet you appreciate you carving out ten minutes");
        assert!(
            t.active_line_spoken_out(),
            "a fully spoken line must complete from the interim, before any pause"
        );
    }

    #[test]
    fn a_mid_line_interim_is_not_complete() {
        let mut t = tracker();
        t.set_partial("good to meet you appreciate");
        assert!(
            !t.active_line_spoken_out(),
            "a partial line must not complete"
        );
    }

    #[test]
    fn the_closing_word_is_required_to_complete_instantly() {
        let mut t = tracker();
        // Covers most of line 1 but stops just before "...10 minutes".
        t.set_partial("name good to meet you appreciate you carving out");
        assert!(
            !t.active_line_spoken_out(),
            "the closing word 'minutes' is missing"
        );
    }

    #[test]
    fn completing_from_speech_advances_immediately_and_does_not_retrigger() {
        let mut t = tracker();
        t.set_partial("good to meet you appreciate you carving out ten minutes");
        assert!(t.active_line_spoken_out());
        assert_eq!(t.complete_active(), Some(2));
        assert_eq!(t.active_id(), 2);
        // The stale interim still names line 1; after advancing it must not complete line 2.
        t.set_partial("good to meet you appreciate you carving out ten minutes");
        assert!(!t.active_line_spoken_out());
    }

    #[test]
    fn manual_line_jumps_reanchor_and_go_both_ways() {
        let mut t = tracker();
        assert_eq!(t.active_id(), 1);
        assert_eq!(t.manual(1), 2);
        assert_eq!(t.active_id(), 2);
        // Re-anchoring keeps the line but drops the stale audio buffer.
        t.push_text("good to meet you appreciate you carving out ten minutes");
        assert!(!t.spoken_buffer().is_empty());
        assert_eq!(t.reanchor(), 2);
        assert_eq!(t.spoken_buffer(), "", "re-anchor must flush stale audio");
        assert_eq!(t.manual(-1), 1);
        assert_eq!(t.active_id(), 1);
    }

    #[test]
    fn section_jump_moves_by_phase() {
        let mut t = tracker();
        assert_eq!(t.active_id(), 1);
        let next = t.jump_phase(1); // first line of the next phase
        assert!(next > 1, "forward section jump should advance");
        let back = t.jump_phase(-1); // back to the previous phase start
        assert!(back < next, "backward section jump should go back");
    }

    #[test]
    fn matching_vocabulary_excludes_label_and_phase() {
        let script = load_script_by_type("call1").unwrap();
        let line = script.lines.iter().find(|l| l.id == 1).unwrap();
        let words = body_tokens(line);
        // Label is "Good to meet & carve time"; "carve" is never spoken in the body.
        assert!(
            !words.contains("carve"),
            "label word leaked into the match vocabulary: {words:?}"
        );
        // Phase is "PHASE 1: SETTING THE FRAME".
        assert!(
            !words.contains("phase") && !words.contains("frame") && !words.contains("setting"),
            "phase word leaked into the match vocabulary: {words:?}"
        );
    }

    #[test]
    fn cue_markers_never_become_spoken_words() {
        // Line 6 carries `[⏸️ PAUSE 1s]` and `[🐢 SLOW & LOW]`; none may survive into matching.
        let script = load_script_by_type("call1").unwrap();
        let line = script.lines.iter().find(|l| l.id == 6).unwrap();
        let words = body_tokens(line);
        for marker in ["pause", "slow", "down", "curious"] {
            assert!(
                !words.contains(marker),
                "cue marker '{marker}' leaked into the vocabulary: {words:?}"
            );
        }
    }
}
