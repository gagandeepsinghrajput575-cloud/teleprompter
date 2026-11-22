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
const COLD_RULES: &[Rule] = &[
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
            " what does the audit require ",
            " what does the audit involve ",
            " what do you need from me ",
            " audit requirement ",
            " what is required for the audit ",
            " what is required ",
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
            " what is this regarding ",
            " who is this regarding ",
            " what is this about ",
            " what s this about ",
            " is he expecting ",
            " expecting the call ",
            " one of his guys ",
            " one of the guys ",
            " in a meeting ",
            " later today ",
            " do it today ",
            " call me today ",
            " talk today ",
            " free today ",
            " lock it in for monday ",
            " monday ",
        ],
    },
];

/// Sales call closing objection rules (Call 1 Audit, Call 2 Close, Custom sales scripts).
const SALES_RULES: &[Rule] = &[
    Rule {
        index: 3,
        phrases: &[
            " partner ",
            " business partner ",
            " co founder ",
            " talk to my partner ",
            " discuss with my partner ",
            " speak with my partner ",
            " run this by my partner ",
            " talk it over ",
            " my wife ",
        ],
    },
    Rule {
        index: 2,
        phrases: &[
            " burned before ",
            " past agency ",
            " agencies in the past ",
            " lost money ",
            " burned by ",
            " bad experience ",
            " wasted money ",
            " didn t get results ",
        ],
    },
    Rule {
        index: 1,
        phrases: &[
            " cash flow ",
            " tight right now ",
            " too expensive ",
            " can t afford ",
            " out of our budget ",
            " lot of money ",
            " too much money ",
            " cost is high ",
            " how much does it cost ",
            " what does it cost ",
            " what s the price ",
            " price ",
            " cost ",
        ],
    },
    Rule {
        index: 4,
        phrases: &[
            " next quarter ",
            " revisit this ",
            " bad time ",
            " busy season ",
            " after the summer ",
            " next month ",
            " call me back in a few months ",
        ],
    },
    Rule {
        index: 5,
        phrases: &[
            " send a proposal ",
            " send the proposal ",
            " send the contract ",
            " send over the agreement ",
            " put it in writing ",
            " send an agreement ",
            " review the proposal ",
            " send me an email ",
            " email me ",
        ],
    },
    Rule {
        index: 6,
        phrases: &[
            " in house ",
            " do it ourselves ",
            " do this ourselves ",
            " our own team ",
            " my own team ",
            " do it on our own ",
            " build it ourselves ",
        ],
    },
    Rule {
        index: 7,
        phrases: &[
            " how much time ",
            " how long will it take ",
            " how much work ",
            " time commitment ",
            " how many hours ",
            " take up my time ",
        ],
    },
    Rule {
        index: 8,
        phrases: &[
            " will this work for us ",
            " our specific niche ",
            " we are different ",
            " our business is unique ",
            " in our industry ",
        ],
    },
    Rule {
        index: 9,
        phrases: &[
            " guarantee ",
            " what if it doesn t work ",
            " what if it fails ",
            " case studies ",
            " references ",
            " track record ",
            " results guarantee ",
        ],
    },
    Rule {
        index: 0,
        phrases: &[
            " long term contract ",
            " lock in ",
            " locked in ",
            " cancel anytime ",
            " month to month ",
            " commitment ",
            " annual contract ",
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

/// Returns the objection index the prospect most likely just raised for the specific call type.
pub fn classify_for_call(text: &str, call_type: &str) -> Option<i32> {
    let rules = match call_type {
        "call1" | "call2" | "sales" | "close" => SALES_RULES,
        _ => COLD_RULES,
    };
    let haystack = normalize(text);
    let mut best: Option<(i32, usize)> = None;
    for rule in rules {
        let mut score = 0usize;
        for phrase in rule.phrases {
            if haystack.contains(phrase) {
                score += phrase.split_whitespace().count();
            }
        }
        if score == 0 {
            continue;
        }
        if best.map_or(true, |(_, best_score)| score > best_score) {
            best = Some((rule.index, score));
        }
    }
    best.map(|(index, _)| index)
}

/// Returns the objection index the prospect most likely just raised, if any.
#[allow(dead_code)]
pub fn classify(text: &str) -> Option<i32> {
    classify_for_call(text, "coldcall")
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
        assert_eq!(classify("What does the audit require from us?"), Some(9));
    }

    #[test]
    fn employee_maps_to_zero() {
        assert_eq!(classify("I'm just the employee here"), Some(0));
        assert_eq!(classify("What is this regarding?"), Some(0));
        assert_eq!(classify("Is he expecting the call?"), Some(0));
        assert_eq!(classify("What is this about?"), Some(0));
        assert_eq!(classify("Can you call me back later today?"), Some(0));
    }

    #[test]
    fn ordinary_conversation_is_not_an_objection() {
        assert_eq!(classify("Yes that sounds fine, go ahead"), None);
        assert_eq!(classify(""), None);
    }

    #[test]
    fn sales_call_objections_match_correct_slots() {
        use super::classify_for_call;
        assert_eq!(classify_for_call("I need to speak with my partner first", "call2"), Some(3));
        assert_eq!(classify_for_call("We got burned before by a marketing agency", "sales"), Some(2));
        assert_eq!(classify_for_call("Our cash flow is very tight right now", "call1"), Some(1));
        assert_eq!(classify_for_call("Can you send a proposal over to review?", "sales"), Some(5));
        assert_eq!(classify_for_call("What kind of guarantee or case studies do you have?", "call2"), Some(9));
    }

    #[test]
    fn cooldown_suppresses_repeats() {
        let mut recent = Recent::new();
        assert!(recent.should_emit(4));
        assert!(!recent.should_emit(4));
        assert!(recent.should_emit(5));
    }
}
