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
const EAGER_MIN_ACTIVE_PROGRESS: f64 = 0.65;
/// Coverage of the active line that counts an utterance final as a *completed* line rather than
/// a breath/emphasis pause. High on purpose: a pause mid-sentence must not advance the script.
const COMPLETE_MIN_COVERAGE: f64 = 0.75;
/// When the line's closing words were actually spoken, a lower coverage still counts as complete.
const COMPLETE_TAIL_COVERAGE: f64 = 0.50;
/// Coverage of the active line required when the speaker has plainly begun the next line.
const COMPLETE_NEXT_START_COVERAGE: f64 = 0.35;
/// Coverage + closing word required to complete a line from the *interim* transcript, before the
/// endpointing pause. High enough that an unfinished sentence cannot skip a line, but low enough
/// to fire within an interim of hearing the line's final word (so the highlight leads, not lags).
const LIVE_COMPLETE_COVERAGE: f64 = 0.70;

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct ScriptBranch {
    pub condition: String,
    pub response: String,
    #[serde(default)]
    pub side: Option<String>,
    #[serde(default)]
    pub head: Option<String>,
    #[serde(default)]
    pub prospect: Option<String>,
    #[serde(default)]
    pub note: Option<String>,
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct ScriptLine {
    pub id: i32,
    pub phase: String,
    pub label: String,
    /// Original text with delivery markers intact.
    pub raw: String,
    /// Spoken text, markers stripped. Derived so the fixture cannot drift.
    pub body: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub branches: Vec<ScriptBranch>,
}

#[derive(serde::Deserialize)]
struct RawLine {
    id: i32,
    phase: String,
    label: String,
    raw: String,
    #[serde(default)]
    branches: Vec<ScriptBranch>,
}

#[derive(serde::Deserialize)]
struct RawFlowCard {
    id: i32,
    #[serde(default)]
    turn: Option<String>,
    #[serde(default)]
    label: String,
    raw: String,
}

#[derive(serde::Deserialize)]
struct RawScript {
    id: String,
    title: String,
    version: u32,
    #[serde(default)]
    lines: Vec<RawLine>,
    #[serde(default)]
    center: Vec<RawFlowCard>,
    #[serde(default)]
    branches: Vec<RawFlowCard>,
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
    // Strip any loose cue emojis that may exist outside brackets
    let cleaned: String = out
        .chars()
        .filter(|&c| c != '⬇' && c != '🤔' && c != '🐢' && c != '⚡' && c != '⏸' && c != '\u{fe0f}')
        .collect();
    cleaned.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn is_delivery_marker(token: &str) -> bool {
    // Pure emoji delivery markers: ⬇, ⬇️, 🤔, 🐢, ⚡, ⏸, ⏸️
    if token.contains('⬇')
        || token.contains('🤔')
        || token.contains('🐢')
        || token.contains('⚡')
        || token.contains('⏸')
    {
        return true;
    }

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
    let lines = if !raw.lines.is_empty() {
        raw.lines
            .into_iter()
            .map(|line| ScriptLine {
                body: strip_annotations(&line.raw),
                id: line.id,
                phase: line.phase,
                label: line.label,
                raw: line.raw,
                branches: line.branches,
            })
            .collect()
    } else {
        raw.center
            .into_iter()
            .chain(raw.branches.into_iter())
            .map(|card| ScriptLine {
                body: strip_annotations(&card.raw),
                id: card.id,
                phase: card.turn.unwrap_or_else(|| card.label.clone()),
                label: card.label,
                raw: card.raw,
                branches: Vec::new(),
            })
            .collect()
    };
    Ok(ScriptFile {
        id: raw.id,
        title: raw.title,
        version: raw.version,
        lines,
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

/// Simple English stemmer for common suffixes so plurals ("calls" vs "call") and verb forms
/// ("booking" vs "booked") match seamlessly.
pub fn stem(word: &str) -> String {
    let mut w = word.to_lowercase();
    if w.len() > 5 && w.ends_with("ing") {
        w.truncate(w.len() - 3);
        return w;
    }
    if w.len() > 4 && w.ends_with("ed") {
        w.truncate(w.len() - 2);
        return w;
    }
    if w.len() > 4 && w.ends_with("es") {
        w.truncate(w.len() - 2);
        return w;
    }
    if w.len() > 3 && w.ends_with('s') && !w.ends_with("ss") {
        w.truncate(w.len() - 1);
        return w;
    }
    w
}

/// Levenshtein edit distance between two strings.
pub fn edit_distance(s1: &str, s2: &str) -> usize {
    let v1: Vec<char> = s1.chars().collect();
    let v2: Vec<char> = s2.chars().collect();
    let len1 = v1.len();
    let len2 = v2.len();
    if len1 == 0 {
        return len2;
    }
    if len2 == 0 {
        return len1;
    }
    if len1.abs_diff(len2) > 2 {
        return 999;
    }
    let mut prev: Vec<usize> = (0..=len2).collect();
    let mut curr = vec![0; len2 + 1];
    for i in 1..=len1 {
        curr[0] = i;
        for j in 1..=len2 {
            let cost = if v1[i - 1] == v2[j - 1] { 0 } else { 1 };
            curr[j] = (prev[j] + 1).min(curr[j - 1] + 1).min(prev[j - 1] + cost);
        }
        prev.copy_from_slice(&curr);
    }
    prev[len2]
}

/// Matches words with stem invariance and fuzzy tolerance for phonetic/ASR near-misses.
pub fn words_match(a: &str, b: &str) -> bool {
    if a == b {
        return true;
    }
    let sa = stem(a);
    let sb = stem(b);
    if sa == sb || sa == b || a == sb {
        return true;
    }
    let la = a.chars().count();
    let lb = b.chars().count();
    if la.abs_diff(lb) <= 2 {
        let max_edits = if la >= 8 && lb >= 8 {
            2
        } else if la >= 5 && lb >= 5 {
            1
        } else {
            0
        };
        if max_edits > 0 && edit_distance(a, b) <= max_edits {
            return true;
        }
    }
    false
}

/// Counts how many target words in `line_body` are satisfied by `heard` tokens, using exact,
/// stem, and fuzzy matching.
pub fn count_hits(heard: &HashSet<String>, line_body: &HashSet<String>) -> usize {
    line_body
        .iter()
        .filter(|&bw| {
            heard.contains(bw)
                || heard.contains(&stem(bw))
                || heard.iter().any(|h| words_match(h, bw))
        })
        .count()
}

/// Normalizes numeric tokens and times into spoken words so "10" matches "ten", "4" matches "four", etc.
fn expand_numbers_and_contractions(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 32);
    for raw_word in text.split_whitespace() {
        let lower = raw_word.to_lowercase();
        let cleaned =
            lower.trim_matches(|c: char| !c.is_alphanumeric() && c != ':' && c != '$' && c != ',');
        match cleaned {
            "1" => out.push_str("one "),
            "2" => out.push_str("two "),
            "3" => out.push_str("three "),
            "4" => out.push_str("four "),
            "5" => out.push_str("five "),
            "6" => out.push_str("six "),
            "7" => out.push_str("seven "),
            "8" => out.push_str("eight "),
            "9" => out.push_str("nine "),
            "10" => out.push_str("ten "),
            "11" => out.push_str("eleven "),
            "12" => out.push_str("twelve "),
            "13" => out.push_str("thirteen "),
            "14" => out.push_str("fourteen "),
            "15" => out.push_str("fifteen "),
            "16" => out.push_str("sixteen "),
            "17" => out.push_str("seventeen "),
            "18" => out.push_str("eighteen "),
            "19" => out.push_str("nineteen "),
            "20" => out.push_str("twenty "),
            "24" => out.push_str("twenty four "),
            "30" => out.push_str("thirty "),
            "40" => out.push_str("forty "),
            "50" => out.push_str("fifty "),
            "60" => out.push_str("sixty "),
            "80" => out.push_str("eighty "),
            "12:30" => out.push_str("twelve thirty "),
            "15:30" => out.push_str("fifteen thirty "),
            "14:30" => out.push_str("fourteen thirty "),
            "15:00" => out.push_str("fifteen "),
            "8:00" => out.push_str("eight "),
            "$15k" | "15k" => out.push_str("fifteen thousand "),
            "40,000" | "40000" => out.push_str("forty thousand "),
            "80,000" | "80000" => out.push_str("eighty thousand "),
            _ => {
                out.push_str(raw_word);
                out.push(' ');
            }
        }
    }
    out
}

/// Content words in spoken order (stopwords and very short words removed), deduplicated only by
/// the caller. Order matters for detecting a line's *closing* words at a pause.
fn ordered_tokens(text: &str) -> Vec<String> {
    let expanded = expand_numbers_and_contractions(text);
    expanded
        .to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .map(|w| w.replace('\'', "").trim().to_string())
        .filter(|w| (w.len() >= 3 || w == "hi") && !STOPWORDS.contains(&w.as_str()))
        .collect()
}

fn tokens(text: &str) -> HashSet<String> {
    ordered_tokens(text).into_iter().collect()
}

/// The matching vocabulary for a line is the spoken **body only**. `label` and `phase` are
/// navigation chrome the rep never says; folding them in let a single incidental word jump the
/// highlight, because a common word could match a label rather than anything spoken.
#[allow(dead_code)]
pub fn body_tokens(line: &ScriptLine) -> HashSet<String> {
    let mut set: HashSet<String> = spoken_content(&line.raw).into_iter().collect();
    if set.is_empty() {
        set = tokens(&line.body);
    }
    for b in &line.branches {
        set.extend(spoken_content(&b.response));
    }
    set
}

/// Per-line matching variants: the primary spoken line and each branch response are evaluated
/// as separate alternative paths, so a speaker delivering a branch or a brief primary line is
/// evaluated against that variant's token set rather than an artificially inflated union.
fn line_variants(line: &ScriptLine) -> Vec<HashSet<String>> {
    let mut variants = Vec::with_capacity(1 + line.branches.len());
    let mut primary: HashSet<String> = spoken_content(&line.raw).into_iter().collect();
    if primary.is_empty() {
        primary = tokens(&line.body);
    }
    variants.push(primary);
    for b in &line.branches {
        let branch_set: HashSet<String> = spoken_content(&b.response).into_iter().collect();
        if !branch_set.is_empty() {
            variants.push(branch_set);
        }
    }
    variants
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
    /// Per-line matching variants: each line has a list of token sets (the primary line, plus any
    /// branch responses) so branch paths don't inflate the primary line's coverage denominator.
    line_variants: Vec<Vec<HashSet<String>>>,
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
        let line_variants: Vec<Vec<HashSet<String>>> = lines.iter().map(line_variants).collect();
        Ok(Self {
            lines,
            line_variants,
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
        self.line_variants = self.lines.iter().map(line_variants).collect();
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

    pub fn last_completed(&self) -> Option<(i32, Instant)> {
        self.last_completed
    }

    /// Checks whether the given text strongly matches variants of the specified line.
    pub fn matches_line_tokens(&self, line_id: i32, text: &str) -> bool {
        let t = tokens(text);
        if t.is_empty() {
            return false;
        }
        let Some((idx, _)) = self.lines.iter().enumerate().find(|(_, l)| l.id == line_id) else {
            return false;
        };
        for var in &self.line_variants[idx] {
            if var.is_empty() {
                continue;
            }
            let hits = count_hits(&t, var);
            let cov = hits as f64 / var.len() as f64;
            if hits >= 2 && cov >= 0.25 {
                return true;
            }
        }
        false
    }

    pub fn spoken_buffer(&self) -> String {
        let mut parts: Vec<String> = self.window.iter().cloned().collect();
        let partial = self.partial.trim();
        if !partial.is_empty() {
            parts.push(partial.to_string());
        }
        parts.join(" ")
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
            let mut ordered_variants = vec![spoken_content(&curr_line.raw)];
            for b in &curr_line.branches {
                ordered_variants.push(spoken_content(&b.response));
            }

            let mut heard = tokens(&self.spoken_buffer());
            heard.extend(final_tokens.iter().cloned());

            let mut any_variant_complete = false;
            for ordered in ordered_variants {
                if ordered.is_empty() {
                    any_variant_complete = true;
                    break;
                }
                let body: HashSet<String> = ordered.iter().cloned().collect();
                let hits = count_hits(&heard, &body);
                let coverage = hits as f64 / body.len() as f64;
                // Closing words: check the last 3 words using stem and fuzzy matching
                let tail_present = ordered.iter().rev().take(3).any(|t| {
                    heard.contains(t)
                        || heard.contains(&stem(t))
                        || heard.iter().any(|h| words_match(h, t))
                });
                // Did the speaker already begin the next line? Then this one is over.
                let next_started = self
                    .lines
                    .iter()
                    .find(|l| l.id > self.active)
                    .map(|next| {
                        let open = spoken_content(&next.raw);
                        open.iter()
                            .take(2)
                            .filter(|t| {
                                final_tokens.contains(*t)
                                    || final_tokens.contains(&stem(t))
                                    || final_tokens.iter().any(|f| words_match(f, t))
                            })
                            .count()
                            >= 2
                    })
                    .unwrap_or(false);

                if coverage >= COMPLETE_MIN_COVERAGE
                    || (tail_present && coverage >= COMPLETE_TAIL_COVERAGE)
                    || (next_started && coverage >= COMPLETE_NEXT_START_COVERAGE)
                {
                    any_variant_complete = true;
                    break;
                }
            }
            any_variant_complete
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
        let spoken = tokens(&self.spoken_buffer());
        let mut ordered_variants = vec![spoken_content(&line.raw)];
        for b in &line.branches {
            ordered_variants.push(spoken_content(&b.response));
        }

        for ordered in ordered_variants {
            if ordered.is_empty() {
                continue;
            }
            let body: HashSet<String> = ordered.iter().cloned().collect();
            let hits = count_hits(&spoken, &body);
            let coverage = hits as f64 / body.len() as f64;
            let tail_present = ordered.iter().rev().take(2).any(|t| {
                spoken.contains(t)
                    || spoken.contains(&stem(t))
                    || spoken.iter().any(|s| words_match(s, t))
            });
            // Auto-advance as soon as line reaches 80% completion (or 70% with closing tail words)
            if (coverage >= LIVE_COMPLETE_COVERAGE && tail_present) || coverage >= 0.80 {
                return true;
            }
        }
        false
    }

    /// True when closing tail tokens of the active line are present in recent speech.
    fn active_tail_present(&self) -> bool {
        let Some(line) = self.lines.iter().find(|l| l.id == self.active) else {
            return false;
        };
        let spoken = tokens(&self.spoken_buffer());
        let mut ordered_variants = vec![spoken_content(&line.raw)];
        for b in &line.branches {
            ordered_variants.push(spoken_content(&b.response));
        }

        for ordered in ordered_variants {
            if ordered.is_empty() {
                continue;
            }
            let tail_present = ordered.iter().rev().take(3).any(|t| {
                spoken.contains(t)
                    || spoken.contains(&stem(t))
                    || spoken.iter().any(|s| words_match(s, t))
            });
            if tail_present {
                return true;
            }
        }
        false
    }

    /// Fraction of the active line's content words present in recent speech (rolling window +
    /// interim). Used to keep the highlight on the current line until it is largely delivered.
    fn active_progress(&self) -> f64 {
        let Some((idx, _)) = self
            .lines
            .iter()
            .enumerate()
            .find(|(_, l)| l.id == self.active)
        else {
            return 0.0;
        };
        let variants = &self.line_variants[idx];
        let spoken = tokens(&self.spoken_buffer());
        let mut max_progress = 0.0f64;
        for var in variants {
            if var.is_empty() {
                return 1.0;
            }
            let hits = count_hits(&spoken, var);
            let progress = hits as f64 / var.len() as f64;
            if progress > max_progress {
                max_progress = progress;
            }
        }
        max_progress
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
        // delivered and its closing cadence is present, OR if the speaker has clearly moved on
        // and is reading the opening words of the next line.
        let active_progress = self.active_progress();
        let tail_ready = self.active_tail_present();
        let next_line_opt = self
            .lines
            .iter()
            .enumerate()
            .skip_while(|(_, l)| l.id <= self.active)
            .find(|(i, _)| self.line_variants[*i].iter().any(|v| !v.is_empty()));

        if let Some((next_idx, next_line)) = next_line_opt {
            let next_vars = &self.line_variants[next_idx];
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
                let next_best_hits = next_vars
                    .iter()
                    .map(|v| count_hits(&recent_tokens, v))
                    .max()
                    .unwrap_or(0);
                let curr_idx = self
                    .lines
                    .iter()
                    .position(|l| l.id == self.active)
                    .unwrap_or(0);
                let curr_hits = self.line_variants[curr_idx]
                    .iter()
                    .map(|v| count_hits(&recent_tokens, v))
                    .max()
                    .unwrap_or(0);
                let next_best_len = next_vars
                    .iter()
                    .find(|v| count_hits(&recent_tokens, v) == next_best_hits)
                    .map(|v| v.len())
                    .unwrap_or(1)
                    .max(1);
                let next_coverage = next_best_hits as f64 / next_best_len as f64;

                // The speaker must actually be into the next line, and more into it than
                // into the current one, before the highlight leads them there.
                let min_lead_hits = if next_best_len <= 2 { next_best_len } else { 3 };
                let speaker_clearly_on_next = next_best_hits >= min_lead_hits
                    && next_best_hits > curr_hits
                    && next_coverage >= 0.20;

                let active_mostly_done = active_progress >= 0.60 && (tail_ready || active_progress >= 0.75);

                if speaker_clearly_on_next || (active_mostly_done && next_best_hits >= 2) {
                    let confident = speaker_clearly_on_next && (active_progress >= 0.50 || next_best_hits >= 4);
                    return Some((next_line.id, confident, 2.0 + next_best_hits as f64));
                }
            }
        }

        let total_lines = self.lines.len() as f64;

        let mut scored: Vec<(i32, f64, usize, usize)> = self
            .lines
            .iter()
            .zip(self.line_variants.iter())
            .map(|(line, variants)| {
                let mut best_score = 0.0;
                let mut best_hits = 0usize;
                let mut best_len = variants.first().map(|v| v.len()).unwrap_or(0);

                for var in variants {
                    // Delayed echo guard: never score the immediate line just completed
                    // within the last 2.5 seconds (prevents the delayed final transcript from pulling tracker back
                    // and prevents it from masking upcoming lines).
                    if let Some((completed_id, completed_at)) = self.last_completed {
                        if line.id == completed_id
                            && completed_at.elapsed() < std::time::Duration::from_millis(2500)
                        {
                            continue;
                        }
                    }

                    let mut score = 0.0;
                    let mut hits = 0usize;
                    for token in &buffer {
                        let matched = var.contains(token)
                            || var.contains(&stem(token))
                            || var.iter().any(|w| words_match(w, token));
                        if !matched {
                            continue;
                        }
                        hits += 1;
                        let df = self
                            .line_variants
                            .iter()
                            .filter(|line_vars| {
                                line_vars.iter().any(|v| {
                                    v.contains(token)
                                        || v.contains(&stem(token))
                                        || v.iter().any(|w| words_match(w, token))
                                })
                            })
                            .count() as f64;
                        // Distinctive words count more than words every line shares.
                        score += (total_lines / df.max(1.0)).ln().max(0.05);
                    }
                    if score > best_score || (score == best_score && hits > best_hits) {
                        best_score = score;
                        best_hits = hits;
                        best_len = var.len();
                    }
                }
                (line.id, best_score, best_hits, best_len)
            })
            .collect();

        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        let (best_id, best, best_hits, best_tokens) = scored[0];
        if best <= 0.0 {
            return None;
        }

        let is_next_line = best_id == self.active + 1;
        let is_near_forward = best_id == self.active + 2;
        let is_distant_forward = best_id > self.active + 2;
        let is_backward = best_id < self.active;
        let coverage = if best_tokens == 0 {
            0.0
        } else {
            best_hits as f64 / best_tokens as f64
        };

        let second = scored.get(1).map(|(_, score, _, _)| *score).unwrap_or(0.0);

        // Forward sequence: active -> active + 1 leads slightly easier.
        // Near forward (active + 2): allows skipping a short beat.
        // Distant forward (> active + 2): requires distinctive proof (>=3 hits, >=30% coverage)
        // so accidental 1-2 keyword collisions do not jump across the script.
        // Backward sequence: jumping to an already passed line requires at least 60% coverage
        // so deliberate re-reading tracks smoothly without random shuffling mid-call.
        let (min_hits, min_dominant, ratio_margin, min_coverage) = if is_next_line {
            (2usize, 1.0, 1.1, 0.22)
        } else if is_near_forward {
            (2usize.min(best_tokens.max(1)), 1.2, 1.15, 0.25)
        } else if is_distant_forward {
            (3usize.min(best_tokens.max(1)), 1.6, 1.20, 0.30)
        } else if is_backward {
            (2usize.min(best_tokens.max(1)), 1.3, 1.15, 0.60)
        } else {
            (MIN_HITS, MIN_DOMINANT_SCORE, LOCAL_MARGIN, MIN_COVERAGE)
        };

        let effective_min_hits = if best_tokens <= 1 { 1usize } else { min_hits };

        let margin_ok = if second <= f64::EPSILON {
            best >= min_dominant
        } else {
            best >= second * ratio_margin
        };

        let mut confident =
            best_hits >= effective_min_hits && margin_ok && coverage >= min_coverage;

        // Forward gate: while the active line is still mostly unspoken, a proposal for the next
        // line must be unmistakable (the speaker has clearly moved on) or the highlight holds.
        if is_next_line && active_progress < EAGER_MIN_ACTIVE_PROGRESS {
            confident = (best_hits >= 3 && coverage >= 0.20) || best_hits >= 4;
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
        // require 60% coverage to be confident; when confident (intentional re-read), they commit
        // immediately (1 confirmation). Unconfident backward proposals require three confirmations
        // to prevent jitter and random shuffling mid-call.
        let required: u8 = if proposed > self.active {
            if confident {
                1
            } else {
                2
            }
        } else if confident {
            1
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

    /// Jumps directly to a specific line ID and re-anchors there.
    pub fn jump_to(&mut self, target_id: i32) -> i32 {
        if let Some(index) = self.lines.iter().position(|l| l.id == target_id) {
            self.move_to_index(index)
        } else {
            self.active
        }
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

    /// Lines within reach of the current position.
    #[allow(dead_code)]
    pub fn candidate_window(&self) -> Vec<ScriptLine> {
        let ids: Vec<i32> = self.lines.iter().map(|line| line.id).collect();
        let index = ids.iter().position(|&id| id == self.active).unwrap_or(0);
        let start = index.saturating_sub(3);
        let end = (index + 4).min(self.lines.len());
        self.lines[start..end].to_vec()
    }

    /// Complete script flow from start to finish, providing the LLM complete contextual awareness.
    pub fn all_lines(&self) -> Vec<ScriptLine> {
        self.lines.clone()
    }

    /// Prepares domain words and key phrases from the loaded script for Speech Recognizer priming.
    pub fn vocabulary(&self) -> Vec<String> {
        let mut set = HashSet::new();
        for line in &self.lines {
            for tok in ordered_tokens(&line.raw) {
                if tok.len() >= 3 {
                    set.insert(tok);
                }
            }
            for b in &line.branches {
                for tok in ordered_tokens(&b.response) {
                    if tok.len() >= 3 {
                        set.insert(tok);
                    }
                }
            }
            let stripped = strip_annotations(&line.raw);
            for chunk in stripped.split(['.', ',', '?', '—']) {
                let trimmed = chunk.trim();
                let count = trimmed.split_whitespace().count();
                if (2..=4).contains(&count) && trimmed.len() <= 32 {
                    set.insert(trimmed.to_string());
                }
            }
        }
        let domain_terms = [
            "Scalix",
            "Scalix Elite",
            "Gagan",
            "Denmark",
            "Danish",
            "DKK",
            "residential",
            "construction",
            "remodeler",
            "remodeling",
            "audit",
            "operations",
            "shortlist",
            "voicemail",
            "gross profit",
            "profit margin",
            "high-ticket",
            "contractor",
            "job site",
            "lead generation",
        ];
        for term in domain_terms {
            set.insert(term.to_string());
        }
        let mut list: Vec<String> = set.into_iter().collect();
        list.sort();
        list
    }
}

#[derive(Clone, Debug)]
pub enum LlmProvider {
    Groq(String),
    Cerebras(String),
    Jev(String),
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct LlmDecision {
    pub choice: Option<i32>,
    pub objection: Option<i32>,
}

/// The 10 canonical objection battlecards and exact authored rebuttals for Scalix Elite.
pub const OBJECTION_PROMPT_BLOCK: &str = "\
=== 10 OBJECTION BATTLECARDS & REBUTTALS ===
[Objection 1] 'Price / Why is it free?'
  Rebuttal: 'Because we know 80% of established firms will adopt AI automation in the next 8 to 12 months. So we are building relationships and giving upfront value now, so if or when the time comes that you do want to plug those leaks, you\\'ve already got us as a trusted contact. Sounds fair?'
[Objection 2] 'Not interested / We\\'re good'
  Rebuttal: 'I\\'m not surprised, [Name]—I haven\\'t got to the good bit yet. Give me 60 seconds to explain why I called, and if you still think it\\'s useless for your business after that, tell me no and I\\'ll hang up myself. Fair enough?'
[Objection 3] 'Don\\'t take cold calls'
  Rebuttal: 'Neither do I, [Name], if I can possibly avoid it. But the problem is: your existing software or web guys aren\\'t telling you about this, and you aren\\'t going to go looking for it on your own because you don\\'t know it exists yet. So looking past the phone call for 60 seconds—how about I quickly state the reason I called?'
[Objection 4] 'Where did you get my number?'
  Rebuttal: 'Our research team pulls numbers from public Danish registries. But we\\'re very exclusive in the criteria we look for. We only reach out to established firms with roughly 4 to 8 staff and a solid local reputation—which I\\'m assuming applies to you guys?'
[Objection 5] 'Send me an email'
  Rebuttal: 'For sure, I can email you. But the reason we do the 10-minute look first is we only implement AI in a business if it can actually generate meaningful results—and the missed-call setup I told you about is only one of the things. So I\\'m not sure what to send you on email without it being irrelevant to you, and that\\'s why we have that 10-minute call first—so we can see all the possible gaps in the business. So what do you say—should we grab 10 minutes tomorrow afternoon, or is Thursday morning better?'
[Objection 6] 'We already have a web/tech guy'
  Rebuttal: 'Good to hear. What are they handling right now—just the website, or site call intake too? Got it. We don\\'t replace your web guy. And since the 10-minute look is free, it costs you nothing—after 10 minutes, you\\'ll have a crystal-clear picture of whether there\\'s a leak or if your current setup is airtight. Is tomorrow afternoon or Thursday morning better?'
[Objection 7] 'We don\\'t use AI / old-school'
  Rebuttal: 'Well, using AI sounds unnecessary and fancy until you actually see how much evening time it saves you per week, or how it directly stops high-ticket calls from slipping to competitors. See, the thing is, there could be a lot to go over—and that\\'s the exact reason we do the 10-minute review, so we can see all the possible gaps in your business without you committing to anything. Does tomorrow afternoon or Thursday morning work better to at least see what\\'s possible in your case?'
[Objection 8] 'Booked out / plenty of work'
  Rebuttal: 'The issue when you\\'re slammed isn\\'t getting more work—it\\'s missing out on cherry-picking the highest-margin 200,000 DKK jobs while wasting your evenings quoting tire-kickers. That\\'s why we should connect for 10 minutes so you can get your evenings back—is tomorrow or Thursday better?'
[Objection 9] 'Busy / on site / driving'
  Rebuttal: 'Totally get it, [Name]—I can hear you\\'re busy on site. I won\\'t waste your site time. I can give you a quick ring back at 15:30 when you\\'re off tools, or is tomorrow morning around 08:30 better?'
[Objection 0] 'Employee on site / not the owner'
  Rebuttal: 'Alright, I\\'m trying to reach [Name]\\'s direct mobile regarding an operations review, and I think I dialed the site line by mistake. What\\'s the best phone number to catch him on directly?'";

/// Jev (TypeSafe \"System One\") decision endpoint, reached through OpenRouter so a single
/// OpenRouter key authenticates it. Jev is **not** a chat model: it takes a state plus typed
/// questions and returns a typed answer with probabilities, which is exactly the \"which line am
/// I on?\" decision and removes the JSON-in-a-string fragility of the old chat completion.
const JEV_URL: &str = "https://openrouter.ai/api/v1/systemone";
/// Pinned so a threshold tuned here keeps meaning the same thing. `~typesafe/jev-latest` tracks
/// the newest release; pin when a confidence floor must stay stable across upgrades.
const JEV_MODEL: &str = "typesafe/jev-1.14";
/// Kept tight so a slow model cannot stall the HUD behind a stale highlight.
const LLM_TIMEOUT_MS: u64 = 1200;
/// Minimum confidence for a Jev choice to move the highlight. Below this the answer is treated
/// as \"no decision\" and the position holds, matching the old model's ambiguity handling.
const JEV_MIN_CONFIDENCE: f64 = 0.55;

/// Instructions for the single `choice` question. Each option's description carries the line
/// text, so the model separates lines by meaning rather than by remembering bare ids.
const JEV_INSTRUCTIONS: &str = "Which script line is the salesperson currently delivering? \
Match semantic intent, not exact wording. If the speech is off-script, an interruption, or \
ambiguous, choose the line whose id equals active_line_id in the state.";

/// Stage 2: asks an LLM (Groq LPU, Cerebras, or Jev System One) which line is being delivered.
/// Returns `Ok(LlmDecision)` for a confident in-window answer and any detected objection.
pub async fn classify_with_llm(
    client: &reqwest::Client,
    provider: &LlmProvider,
    candidates: &[ScriptLine],
    active: i32,
    spoken: &str,
) -> Result<LlmDecision, String> {
    match provider {
        LlmProvider::Jev(api_key) => {
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
            let choice = parse_jev_choice(&payload, candidates)?;
            Ok(LlmDecision {
                choice,
                objection: None,
            })
        }
        LlmProvider::Groq(api_key) | LlmProvider::Cerebras(api_key) => {
            let (url, model) = match provider {
                LlmProvider::Groq(_) => (
                    "https://api.groq.com/openai/v1/chat/completions",
                    "qwen/qwen3.8-27b",
                ),
                LlmProvider::Cerebras(_) => (
                    "https://api.cerebras.ai/v1/chat/completions",
                    "llama3.1-8b",
                ),
                _ => unreachable!(),
            };

            let script_flow = candidates
                .iter()
                .map(|line| {
                    let mut s = if line.id == active {
                        format!(">>> [CURRENT ACTIVE LINE] ID {}: [{}] (Phase: {})\n    Primary Script: \"{}\"", line.id, line.label, line.phase, line.body)
                    } else {
                        format!("    ID {}: [{}] (Phase: {})\n    Primary Script: \"{}\"", line.id, line.label, line.phase, line.body)
                    };
                    if !line.branches.is_empty() {
                        s.push_str("\n    Branches / Alternatives:");
                        for b in &line.branches {
                            s.push_str(&format!("\n      * If {}: \"{}\"", b.condition, b.response));
                        }
                    }
                    s
                })
                .collect::<Vec<_>>()
                .join("\n\n");

            let system_prompt = format!("\
You are the real-time AI sales copilot tracking a live, high-stakes sales conversation for Scalix Elite. \
The salesperson (Gagan from Scalix) is pitching high-ticket inbound growth infrastructure, speed-to-lead automation systems, and qualified booked appointments to business owners (residential construction, remodeling, roofing, and home services contractors). \
\
Call Methodologies: \
- Cold Call: Permission opener ('Gagan from Scalix... did I catch you in the middle of a job?'), establishing the core problem (slow response time, losing high-margin jobs, shared lead waste), pitching the 10-minute diagnostic audit, and securing the time. \
- Call 1 (Diagnostic Audit): Clinical diagnosis across 4 revenue buckets (Lead Volume, Speed to Lead, Booking Rate, Average Job Size), uncovering pipeline leaks, creating contrast, and scheduling Call 2 (Prototype Demo). \
- Call 2 (Closing Call / Prototype Demo): Revealing the custom-built live prototype, demonstrating speed-to-lead and closed-loop ROI, handling price/terms/guarantee, and closing the partnership. \
\
{}\
\
DECISION GUIDELINES: \
1. SCRIPT FLOW & LINE SELECTION (`choice`): \
   - Match semantic intent and meaning, not literal words. Gagan speaks naturally, paraphrases, uses casual slang, and adapts on the fly. \
   - Look at the full script flow: notice where Gagan is currently positioned (CURRENT ACTIVE LINE) and determine where he is heading. \
   - If Gagan is speaking the active line or any of its branches, return the active line's ID. \
   - If Gagan has transitioned into a subsequent line (or jumped to another line), return that line's ID. \
   - If Gagan is off-script, making casual small talk, handling an interruption, or the speech is ambiguous, return the current active_line_id so the teleprompter remains solidly FROZEN on the active line. \
\
2. OBJECTIONS & BATTLECARDS (`objection`): \
   - If the recent conversation indicates an objection was raised by the prospect OR Gagan is addressing/rebutting an objection (from the 10 battlecards above), set `objection` to that integer (0 to 9). \
   - If no objection is active, set `objection` to null. \
\
3. RESPONSE FORMAT: \
   Return strictly valid JSON: \
   {{\"choice\": \"<ID>\", \"objection\": <0..9 or null>, \"confidence\": <0.0 to 1.0>}}", OBJECTION_PROMPT_BLOCK);

            let user_prompt = format!(
                "Current Active Line ID: {}\n\nFull Script Flow (>>> marks where the salesperson currently is):\n{}\n\nRecently spoken in conversation:\n\"{}\"\n\nIdentify the active script line ID and any active objection:",
                active, script_flow, spoken
            );

            let body = serde_json::json!({
                "model": model,
                "messages": [
                    { "role": "system", "content": system_prompt },
                    { "role": "user", "content": user_prompt }
                ],
                "response_format": { "type": "json_object" },
                "temperature": 0.0,
                "max_tokens": 60
            });

            let response = client
                .post(url)
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

            let content_str = payload
                .get("choices")
                .and_then(|c| c.get(0))
                .and_then(|c0| c0.get("message"))
                .and_then(|m| m.get("content"))
                .and_then(|c| c.as_str())
                .ok_or_else(|| format!("missing content in llm response: {payload}"))?;

            let parsed: serde_json::Value = serde_json::from_str(content_str)
                .map_err(|e| format!("llm content not valid json: {e}: {content_str}"))?;

            parse_llm_json_decision(&parsed, candidates)
        }
    }
}

pub fn parse_llm_json_choice(
    payload: &serde_json::Value,
    candidates: &[ScriptLine],
) -> Result<Option<i32>, String> {
    let choice_val = payload
        .get("choice")
        .or_else(|| payload.get("active_line_id"))
        .or_else(|| payload.get("id"));

    let choice_str = choice_val
        .and_then(|c| {
            c.as_str()
                .map(|s| s.to_string())
                .or_else(|| c.as_i64().map(|n| n.to_string()))
        })
        .ok_or_else(|| format!("llm response missing choice: {payload}"))?;

    let chosen = choice_str.trim();
    let cleaned = chosen
        .trim_start_matches(|c: char| c == 'I' || c == 'D' || c == 'i' || c == 'd' || c == ':' || c == ' ' || c == '#');

    let id = cleaned
        .parse::<i32>()
        .ok()
        .or_else(|| chosen.parse::<i32>().ok())
        .or_else(|| {
            candidates
                .iter()
                .find(|c| c.id.to_string() == chosen || c.id.to_string() == cleaned)
                .map(|c| c.id)
        })
        .ok_or_else(|| format!("llm choice was not a known line id: {choice_str}"))?;

    if !candidates.iter().any(|b| b.id == id) {
        return Err(format!("llm returned out-of-window id {id}"));
    }

    let confidence = payload
        .get("confidence")
        .and_then(|c| c.as_f64())
        .unwrap_or(0.9);

    if confidence < JEV_MIN_CONFIDENCE {
        println!("LLM_LOW_CONFIDENCE id={id} confidence={confidence:.3}");
        return Ok(None);
    }
    Ok(Some(id))
}

pub fn parse_llm_json_decision(
    payload: &serde_json::Value,
    candidates: &[ScriptLine],
) -> Result<LlmDecision, String> {
    let choice = parse_llm_json_choice(payload, candidates)?;

    let objection = payload.get("objection").and_then(|obj| {
        if obj.is_null() {
            None
        } else if let Some(n) = obj.as_i64() {
            if (0..=9).contains(&n) {
                Some(n as i32)
            } else {
                None
            }
        } else if let Some(s) = obj.as_str() {
            s.trim().parse::<i32>().ok().filter(|&n| (0..=9).contains(&n))
        } else {
            None
        }
    });

    Ok(LlmDecision { choice, objection })
}

struct RepPrefixRule {
    index: i32,
    prefixes: &'static [&'static str],
}

const REP_PREFIX_RULES: &[RepPrefixRule] = &[
    RepPrefixRule {
        index: 3, // Don't take cold calls
        prefixes: &[
            "neither do i",
            "neither do we",
            "neither would i",
        ],
    },
    RepPrefixRule {
        index: 2, // Not interested / We're good
        prefixes: &[
            "i m not surprised",
            "im not surprised",
            "not surprised",
            "haven t got to the good bit",
            "havent got to the good bit",
        ],
    },
    RepPrefixRule {
        index: 4, // Where did you get my number?
        prefixes: &[
            "our research team",
            "research team pulls",
            "public danish registries",
            "danish registries",
        ],
    },
    RepPrefixRule {
        index: 5, // Send me an email
        prefixes: &[
            "for sure i can email",
            "sure i can email",
            "i can email you",
            "the reason we do the 10 minute",
            "the reason we do the ten minute",
        ],
    },
    RepPrefixRule {
        index: 6, // We already have a web/tech guy
        prefixes: &[
            "good to hear",
            "what are they handling right now",
            "we don t replace your web guy",
            "we dont replace your web guy",
            "we don t replace your web",
            "we dont replace your web",
        ],
    },
    RepPrefixRule {
        index: 7, // We don't use AI / old-school
        prefixes: &[
            "well using ai",
            "using ai sounds",
            "using ai sounds unnecessary",
            "ai sounds unnecessary",
        ],
    },
    RepPrefixRule {
        index: 8, // Booked out / plenty of work
        prefixes: &[
            "the issue when you re slammed",
            "the issue when youre slammed",
            "issue when you re slammed",
            "issue when youre slammed",
            "cherry picking the highest",
            "cherrypicking the highest",
        ],
    },
    RepPrefixRule {
        index: 9, // Busy / on site / driving
        prefixes: &[
            "totally get it",
            "i can hear you re busy",
            "i can hear youre busy",
            "busy on site",
            "won t waste your site",
            "wont waste your site",
            "understood when do you",
        ],
    },
    RepPrefixRule {
        index: 0, // Employee on site / not the owner
        prefixes: &[
            "alright i m trying to reach",
            "alright im trying to reach",
            "i m trying to reach",
            "im trying to reach",
            "dialed the site line",
            "dialled the site line",
        ],
    },
    RepPrefixRule {
        index: 1, // Price / Why is it free?
        prefixes: &[
            "because we know 80",
            "because we know eighty",
            "building relationships and giving upfront",
            "we are building relationships",
        ],
    },
];

fn normalize_prefix_text(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push(' ');
    for ch in text.chars().flat_map(char::to_lowercase) {
        if ch.is_alphanumeric() {
            out.push(ch);
        } else if !out.ends_with(' ') {
            out.push(' ');
        }
    }
    if !out.ends_with(' ') {
        out.push(' ');
    }
    out
}

/// Detects whether the salesperson just started uttering the opening 2-3 words of an objection rebuttal.
/// Runs with 0ms latency and 100% priority.
pub fn detect_rep_objection_prefix(text: &str) -> Option<i32> {
    let norm = normalize_prefix_text(text);
    for rule in REP_PREFIX_RULES {
        for prefix in rule.prefixes {
            let p_norm = format!(" {} ", prefix.trim());
            if norm.contains(&p_norm) || norm.trim_start().starts_with(prefix.trim()) {
                return Some(rule.index);
            }
        }
    }
    None
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
                branches: Vec::new(),
            },
            ScriptLine {
                id: 8,
                phase: "discovery".into(),
                label: "Personal time".into(),
                raw: "raw".into(),
                body: "where does your personal time go".into(),
                branches: Vec::new(),
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

    #[test]
    fn parses_groq_json_choice_correctly() {
        use super::parse_llm_json_choice;
        let payload = serde_json::json!({
            "choice": "8",
            "confidence": 0.95
        });
        assert_eq!(parse_llm_json_choice(&payload, &candidates()).unwrap(), Some(8));
    }

    #[test]
    fn parses_groq_prefixed_choice_and_objection() {
        use super::parse_llm_json_decision;
        let payload = serde_json::json!({
            "choice": "ID 8",
            "objection": 4,
            "confidence": 0.95
        });
        let decision = parse_llm_json_decision(&payload, &candidates()).unwrap();
        assert_eq!(decision.choice, Some(8));
        assert_eq!(decision.objection, Some(4));
    }

    #[test]
    fn parses_groq_null_objection() {
        use super::parse_llm_json_decision;
        let payload = serde_json::json!({
            "choice": "7",
            "objection": null,
            "confidence": 0.92
        });
        let decision = parse_llm_json_decision(&payload, &candidates()).unwrap();
        assert_eq!(decision.choice, Some(7));
        assert_eq!(decision.objection, None);
    }

    #[test]
    fn rep_prefix_matcher_triggers_instantly() {
        use super::detect_rep_objection_prefix;
        assert_eq!(detect_rep_objection_prefix("Neither do I John if I can avoid it"), Some(3));
        assert_eq!(detect_rep_objection_prefix("I'm not surprised I haven't got to"), Some(2));
        assert_eq!(detect_rep_objection_prefix("Our research team pulls numbers from public"), Some(4));
        assert_eq!(detect_rep_objection_prefix("For sure I can email you"), Some(5));
        assert_eq!(detect_rep_objection_prefix("Good to hear what are they handling"), Some(6));
        assert_eq!(detect_rep_objection_prefix("Well using AI sounds unnecessary"), Some(7));
        assert_eq!(detect_rep_objection_prefix("The issue when you're slammed isn't more work"), Some(8));
        assert_eq!(detect_rep_objection_prefix("Totally get it I can hear you're busy"), Some(9));
        assert_eq!(detect_rep_objection_prefix("Alright I'm trying to reach his direct mobile"), Some(0));
        assert_eq!(detect_rep_objection_prefix("Because we know 80% of established firms"), Some(1));
        assert_eq!(detect_rep_objection_prefix("Good morning how are you"), None);
    }

    #[test]
    fn groq_low_confidence_holds_position() {
        use super::parse_llm_json_choice;
        let payload = serde_json::json!({
            "choice": "8",
            "confidence": 0.3
        });
        assert_eq!(parse_llm_json_choice(&payload, &candidates()).unwrap(), None);
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
        assert_eq!(call1.lines.len(), 33);
        assert_eq!(call1.lines[0].id, 1);
        assert_eq!(call1.lines[32].id, 33);

        let call2 = load_script_by_type("call2").unwrap();
        assert_eq!(call2.lines.len(), 24);
        assert_eq!(call2.lines[0].id, 1);
        assert_eq!(call2.lines[23].id, 24);
    }

    #[test]
    fn coldcall_fixture_loads_with_name_and_niche_placeholders() {
        let cold = load_script_by_type("coldcall").unwrap();
        assert_eq!(cold.lines.len(), 24);
        assert_eq!(cold.lines[0].id, 1);
        assert!(cold.lines.iter().any(|l| l.body.contains("[Prospect Name]")));
    }

    #[test]
    fn coldcall_placeholders_are_not_counted_as_spoken_words() {
        let cold = load_script_by_type("coldcall").unwrap();
        let hook = cold.lines.iter().find(|l| l.id == 3).unwrap();
        let words = spoken_content(&hook.raw);
        assert!(
            !words.iter().any(|w| w == "prospect" || w == "name"),
            "placeholders must not enter the matching vocabulary: {words:?}"
        );
    }

    #[test]
    fn coldcall_new_opener_and_greeting_match_cleanly() {
        let cold = load_script_by_type("coldcall").unwrap();
        let mut t = Tracker::new(cold.lines).unwrap();

        // Line 1: Main Opener
        t.push_text("hi it's Gagan from Scalix Elite you're not in the middle of a job site right now");
        let (id1, confident1, _) = t.local_match().expect("should match line 1");
        assert_eq!(id1, 1);
        assert!(confident1);

        // Advance to Turn 2 (Line 3: The Hook & Assumption)
        t.consider(3, true);
        assert_eq!(t.active_id(), 3);

        // Line 3: The Hook & Assumption
        t.reanchor();
        t.push_text("reaching out because we work with residential construction firms across Denmark not calling to sell leads or pitch generic marketing roughly people");
        let (id3, confident3, _) = t.local_match().expect("should match line 3");
        assert_eq!(id3, 3);
        assert!(confident3);
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
        assert_eq!(id, 15);
        assert!(confident, "a distinctive paraphrase should be confident");
    }

    #[test]
    fn local_match_finds_call1_project_photos_line() {
        let mut t = tracker();
        // The spoken body of line 13: "What happens to those right now—do they mostly just sit there?"
        // (The old input blended in line 12's "photos of past projects" and only matched line 13
        // through its *label*, which is exactly the contamination this matcher no longer allows.)
        t.push_text("what happens to those right now do they mostly just sit there");
        let (id, _, _) = t.local_match().expect("should match");
        assert_eq!(id, 13);
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
        // Confident backward moves (verified with >= 60% match) commit immediately
        assert_eq!(t.consider(3, true), Some(3));
        assert_eq!(t.active_id(), 3);
    }

    #[test]
    fn backward_jump_to_passed_line_requires_sixty_percent_match() {
        let script = load_script_by_type("call1").unwrap();
        let mut t = Tracker::new(script.lines).unwrap();

        // Advance through lines to line 4 (so last_completed is line 3, not line 1)
        t.consider(2, true);
        t.consider(3, true);
        t.consider(4, true);
        assert_eq!(t.active_id(), 4);

        // Say only 1 word of line 1: "appreciate" -> low coverage (< 60%)
        t.push_text("appreciate");
        let (id_weak, confident_weak, _) = t.local_match().expect("matches line 1");
        assert_eq!(id_weak, 1);
        assert!(
            !confident_weak,
            "backward match below 60% must not be confident to prevent random shuffling"
        );

        // Now speak full line 1: "good to meet you appreciate you carving out ten minutes" -> 100% coverage
        t.push_text("good to meet you appreciate you carving out ten minutes");
        let (id_strong, confident_strong, _) = t.local_match().expect("matches line 1");
        assert_eq!(id_strong, 1);
        assert!(confident_strong, "backward match >= 60% must be confident");
        assert_eq!(t.consider(id_strong, confident_strong), Some(1));
        assert_eq!(t.active_id(), 1);
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
    fn jumping_forward_multiple_lines_commits_instantly_on_confident_match() {
        let script = load_script_by_type("call1").unwrap();
        let mut t = Tracker::new(script.lines).unwrap();
        assert_eq!(t.active_id(), 1);

        // Speaker jumps directly from line 1 to line 15 (high-value client calls)
        t.push_text("high value client calls while on site at eight pm who catches that");
        let (id, confident, _) = t.local_match().expect("should match line 15");
        assert_eq!(id, 15);
        assert!(confident, "strong multi-word jump must be confident");
        assert_eq!(t.consider(id, confident), Some(15));
        assert_eq!(t.active_id(), 15);
    }

    #[test]
    fn jumping_backward_from_later_line_commits_cleanly() {
        let script = load_script_by_type("call1").unwrap();
        let mut t = Tracker::new(script.lines).unwrap();
        t.consider(5, true);
        assert_eq!(t.active_id(), 5);

        // Speaker jumps back to line 2 (Not here to pitch)
        t.push_text("just to set the stage so we use our time well I'm not here to pitch you anything today");
        let (id, confident, _) = t.local_match().expect("should match line 2");
        assert_eq!(id, 2);
        assert!(confident, "strong backward jump must be confident");
        assert_eq!(t.consider(id, confident), Some(2));
        assert_eq!(t.active_id(), 2);
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
        assert_eq!(t.active_id(), 33);
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
        // Only "wasted" overlaps line 16's "already wasted time calling them back".
        t.push_text("my cousin wasted the entire weekend in Ohio");
        let (id, confident, _) = t.local_match().expect("there is some overlap");
        assert_eq!(id, 16);
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
        // "driving" and "back" both appear in line 16, but together they cover about a fifth.
        t.push_text("my cousin is driving from Ohio and back");
        let (id, confident, _) = t.local_match().expect("some overlap");
        assert_eq!(id, 16);
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
                "so for the next eight minutes the agenda is simple i am going to ask you a couple of specific questions to address the operational problems we see firms at your size face"
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
        // Line 5 carries `[⏸️]` and `[🐢]`; none may survive into matching.
        let script = load_script_by_type("call1").unwrap();
        let line = script.lines.iter().find(|l| l.id == 5).unwrap();
        let words = body_tokens(line);
        for marker in ["pause", "slow", "down", "curious"] {
            assert!(
                !words.contains(marker),
                "cue marker '{marker}' leaked into the vocabulary: {words:?}"
            );
        }
    }

    #[test]
    fn words_match_handles_stems_and_fuzzy_typos() {
        assert!(super::words_match("calls", "call"));
        assert!(super::words_match("leaking", "leak"));
        assert!(super::words_match("projects", "project"));
        assert!(super::words_match("booked", "booking"));
        assert!(super::words_match("scalex", "scalix"));
        assert!(super::words_match("customised", "customized"));
        assert!(!super::words_match("sell", "tell"));
        assert!(!super::words_match("call", "fall"));
    }

    #[test]
    fn numbers_normalize_and_match_spoken_words() {
        let mut t = tracker();
        assert_eq!(t.active_id(), 1);

        // Line 1 raw text is "...Appreciate you carving out 10 minutes".
        // The speaker says "ten minutes" with numbers spoken as words.
        t.push_text("good to meet you appreciate you carving out ten minutes");
        let (id, confident, _) = t
            .local_match()
            .expect("should match line 1 with spoken numbers");
        assert_eq!(id, 1);
        assert!(confident);
    }

    #[test]
    fn fuzzy_asr_phonetics_advance_line_two_cleanly() {
        let script = load_script_by_type("call1").unwrap();
        let mut t = Tracker::new(script.lines).unwrap();
        t.move_to_index(1);
        assert_eq!(t.active_id(), 2);

        // ASR hears slight typo on line 2 ("pich" instead of "pitch")
        t.set_partial("just to set the stage so we use our time well i'm not here to pich you anything today");
        assert!(
            t.active_line_spoken_out(),
            "line 2 should complete live despite minor ASR typo"
        );
        let next = t.complete_active();
        assert_eq!(next, Some(3));
        assert_eq!(t.active_id(), 3);
    }

    #[test]
    fn tracker_vocabulary_includes_domain_and_script_terms() {
        let cold = load_script_by_type("coldcall").unwrap();
        let t = Tracker::new(cold.lines).unwrap();
        let vocab = t.vocabulary();
        assert!(vocab.iter().any(|v| v.eq_ignore_ascii_case("scalix")));
        assert!(vocab.iter().any(|v| v.eq_ignore_ascii_case("gagan")));
        assert!(vocab.iter().any(|v| v.eq_ignore_ascii_case("denmark")));
        assert!(vocab.iter().any(|v| v.eq_ignore_ascii_case("operations")));
    }

    #[test]
    fn audit_call_upcoming_line_leads_smoothly_without_backward_jump() {
        let script = load_script_by_type("call1").unwrap();
        let mut t = Tracker::new(script.lines).unwrap();
        assert_eq!(t.active_id(), 1);

        // 1. User delivers Line 1
        t.set_partial("good to meet you appreciate you carving out ten minutes");
        assert!(t.active_line_spoken_out());
        assert_eq!(t.complete_active(), Some(2));
        assert_eq!(t.active_id(), 2);

        // Delayed final of Line 1 arrives:
        let stale_echo = "good to meet you appreciate you carving out ten minutes";
        if let Some((comp_id, _)) = t.last_completed() {
            if !t.matches_line_tokens(comp_id, stale_echo) {
                t.push_text(stale_echo);
            }
        }
        t.clear_partial();

        // 2. User starts delivering upcoming Line 2
        t.set_partial("just to set the stage so we use our time well");
        let (match_id, _, _) = t.local_match().expect("should match line 2");
        assert_eq!(match_id, 2, "must match line 2, NEVER bounce back to line 1");
        assert_eq!(t.active_id(), 2);

        // Line 2 completes
        t.set_partial("just to set the stage so we use our time well i'm not here to pitch you anything today");
        assert!(t.active_line_spoken_out());
        assert_eq!(t.complete_active(), Some(3));
        assert_eq!(t.active_id(), 3);

        // Delayed final of Line 2 arrives:
        let stale_echo_2 = "just to set the stage so we use our time well i'm not here to pitch you anything today";
        if let Some((comp_id, _)) = t.last_completed() {
            if !t.matches_line_tokens(comp_id, stale_echo_2) {
                t.push_text(stale_echo_2);
            }
        }
        t.clear_partial();

        // 3. User begins upcoming Line 3: "So for the next 8 minutes, the agenda is simple..."
        t.set_partial("so for the next eight minutes the agenda is simple");
        let (match_id_3, _, _) = t.local_match().expect("should match line 3");
        assert_eq!(match_id_3, 3, "must match line 3, NEVER bounce back to line 2 or 1");

        // Line 3 completes
        t.set_partial("so for the next eight minutes the agenda is simple i am going to ask you a couple of specific questions to address the operational problems we see firms at your size face");
        assert!(t.active_line_spoken_out());
        assert_eq!(t.complete_active(), Some(4));
        assert_eq!(t.active_id(), 4);

        // 4. User starts upcoming Line 4: "From how high-ticket inquiries slip to voicemail on site..."
        t.set_partial("from how high ticket inquiries slip to voicemail on site");
        let (match_id_4, _, _) = t.local_match().expect("should match line 4");
        assert_eq!(match_id_4, 4, "must match line 4, NEVER bounce back to earlier lines");
    }

    #[test]
    fn deliberate_backward_reread_commits_on_confident_match() {
        let script = load_script_by_type("call1").unwrap();
        let mut t = Tracker::new(script.lines).unwrap();
        t.move_to_index(2);
        assert_eq!(t.active_id(), 3);

        // Speaker intentionally re-reads Line 1
        t.set_partial("good to meet you appreciate you carving out ten minutes");
        let (proposed, confident, _) = t.local_match().expect("must match line 1 on intentional re-read");
        assert_eq!(proposed, 1);
        assert!(confident, "intentional re-read with full sentence is confident");
        assert_eq!(t.consider(proposed, confident), Some(1));
        assert_eq!(t.active_id(), 1);
    }

    #[test]
    fn eighty_percent_line_spoken_advances_to_next_line() {
        let script = load_script_by_type("call1").unwrap();
        let mut t = Tracker::new(script.lines).unwrap();
        assert_eq!(t.active_id(), 1);

        // Line 1 tokens: "good", "meet", "appreciate", "carving", "ten", "minutes" (6 content words)
        // 83.3% delivery (5 of 6 content words): "good to meet you appreciate you carving out ten"
        t.set_partial("good to meet you appreciate you carving out ten");
        // With 83% (>= 80%) coverage, active line is completed live
        assert!(t.active_line_spoken_out(), "80% coverage should satisfy line completion");
        let next = t.complete_active();
        assert_eq!(next, Some(2), "must strictly advance to sequential next line (2)");
        assert_eq!(t.active_id(), 2);
    }

    #[test]
    fn ambiguous_or_mumbled_speech_holds_position_on_active_line() {
        let script = load_script_by_type("call1").unwrap();
        let mut t = Tracker::new(script.lines).unwrap();
        assert_eq!(t.active_id(), 1);

        // Mumbled or unrecognized speech that has coincidental keyword collisions
        t.push_text("yeah uh huh calls and business whatever");
        // Must NOT complete line 1
        assert!(!t.active_line_spoken_out());
        // Must not commit any jump
        assert_eq!(t.active_id(), 1);
    }
}
