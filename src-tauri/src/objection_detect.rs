//! Classifies *prospect* speech into the existing ⌃N objection indices.
//!
//! Deliberately a keyword tier-0 classifier: it runs on-device, costs nothing, and is fast enough
//! that the battlecard appears while the prospect is still finishing their sentence. The indices
//! match `src/lib/objections.ts` exactly (1–9, and 0 for "employee on site"), so the existing
//! `objection` event and overlay can be reused without touching the script.

use std::collections::HashMap;
use std::time::{Duration, Instant};

/// Do not re-suggest the same card more often than this while the prospect keeps talking.
const COOLDOWN: Duration = Duration::from_secs(12);

struct Rule {
    index: i32,
    phrases: &'static [&'static str],
}

/// Ordered most-specific first; ties are resolved in favour of the earlier rule.
const RULES: &[Rule] = &[
    Rule {
        index: 4,
        phrases: &[
            " how did you get my number ",
            " where did you get my number ",
            " where did you get this number ",
            " how did you get this number ",
            " who gave you my number ",
            " where did you get my details ",
        ],
    },
    Rule {
        index: 3,
        phrases: &[
            " don t call ",
            " stop calling ",
            " cold call ",
            " take me off ",
            " remove me ",
            " don t phone ",
            " don t ring ",
        ],
    },
    Rule {
        index: 6,
        phrases: &[
            " web guy ",
            " website guy ",
            " web developer ",
            " we have a guy ",
            " we ve got a guy ",
            " already have someone ",
            " we have someone ",
            " our web guy ",
            " our tech guy ",
        ],
    },
    Rule {
        index: 7,
        phrases: &[
            " don t use ai ",
            " no ai ",
            " not interested in ai ",
            " old school ",
            " old fashioned ",
            " don t believe in ai ",
            " ai is not for us ",
            " don t do technology ",
        ],
    },
    Rule {
        index: 5,
        phrases: &[
            " send me an email ",
            " email me ",
            " send some information ",
            " send information ",
            " send a brochure ",
            " send me something ",
            " send it over ",
            " email something ",
        ],
    },
    Rule {
        index: 8,
        phrases: &[
            " booked out ",
            " fully booked ",
            " booked solid ",
            " slammed ",
            " no capacity ",
            " too much work ",
            " flat out ",
            " plenty of work ",
        ],
    },
    Rule {
        index: 9,
        phrases: &[
            " on site ",
            " onsite ",
            " on a job ",
            " driving ",
            " in the van ",
            " can t talk ",
            " on the tools ",
            " up a ladder ",
            " busy ",
            " at work ",
        ],
    },
    Rule {
        index: 1,
        phrases: &[
            " how much ",
            " what does it cost ",
            " what s the price ",
            " price ",
            " expensive ",
            " what s the catch ",
            " why is it free ",
            " what do you charge ",
            " cost ",
        ],
    },
    Rule {
        index: 2,
        phrases: &[
            " not interested ",
            " no thanks ",
            " not for us ",
            " we re good ",
            " we are good ",
            " don t need it ",
            " no need ",
            " not right now ",
        ],
    },
    Rule {
        index: 0,
        phrases: &[
            " employee ",
            " not the owner ",
            " he s not here ",
            " she s not here ",
            " receptionist ",
            " i m just the ",
            " the office ",
        ],
    },
];

/// Lowercase, strip punctuation, collapse to single spaces, pad both ends so phrases can be
/// matched with word boundaries (`" busy "` never matches `" busybody "`).
fn normalize(text: &str) -> String {
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

/// Returns the objection index the prospect most likely just raised, if any.
pub fn classify(text: &str) -> Option<i32> {
    let haystack = normalize(text);
    let mut best: Option<(i32, usize)> = None;
    for rule in RULES {
        let mut score = 0usize;
        for phrase in rule.phrases {
            if haystack.contains(phrase) {
                score += phrase.split_whitespace().count();
            }
        }
        if score == 0 {
            continue;
        }
        // Strictly greater: on a tie the earlier (more specific) rule keeps the slot.
        if best.map_or(true, |(_, best_score)| score > best_score) {
            best = Some((rule.index, score));
        }
    }
    best.map(|(index, _)| index)
}

/// Per-index cooldown so a looping prospect does not re-fire the same card every interim.
pub struct Recent {
    last: HashMap<i32, Instant>,
}

impl Default for Recent {
    fn default() -> Self {
        Self::new()
    }
}

impl Recent {
    pub fn new() -> Self {
        Self {
            last: HashMap::new(),
        }
    }

    pub fn should_emit(&mut self, index: i32) -> bool {
        let now = Instant::now();
        match self.last.get(&index) {
            Some(at) if now.duration_since(*at) < COOLDOWN => false,
            _ => {
                self.last.insert(index, now);
                true
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{classify, Recent};

    #[test]
    fn price_question_maps_to_one() {
        assert_eq!(classify("How much does it cost?"), Some(1));
    }

    #[test]
    fn not_interested_maps_to_two() {
        assert_eq!(classify("We're not interested, thanks"), Some(2));
    }

    #[test]
    fn cold_call_pushback_maps_to_three() {
        assert_eq!(classify("Don't call me again, stop calling"), Some(3));
    }

    #[test]
    fn number_question_maps_to_four() {
        assert_eq!(classify("Where did you get my number?"), Some(4));
    }

    #[test]
    fn email_request_maps_to_five() {
        assert_eq!(classify("Just send me an email"), Some(5));
    }

    #[test]
    fn existing_web_guy_maps_to_six() {
        assert_eq!(classify("We already have a web guy"), Some(6));
    }

    #[test]
    fn no_ai_maps_to_seven() {
        assert_eq!(classify("We don't use AI, we're old school"), Some(7));
    }

    #[test]
    fn booked_out_maps_to_eight() {
        assert_eq!(classify("We're booked out for months"), Some(8));
    }

    #[test]
    fn on_site_maps_to_nine() {
        assert_eq!(classify("I'm on site right now, can't talk"), Some(9));
    }

    #[test]
    fn employee_maps_to_zero() {
        assert_eq!(classify("I'm just the employee here"), Some(0));
    }

    #[test]
    fn ordinary_conversation_is_not_an_objection() {
        assert_eq!(classify("Yes that sounds fine, go ahead"), None);
        assert_eq!(classify(""), None);
    }

    #[test]
    fn cooldown_suppresses_repeats() {
        let mut recent = Recent::new();
        assert!(recent.should_emit(4));
        assert!(!recent.should_emit(4));
        assert!(recent.should_emit(5));
    }
}
