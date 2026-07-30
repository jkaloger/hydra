//! The stored document: SPEC §3's file shape, one type per JSON object.

use std::collections::BTreeMap;

use jiff::Timestamp;
use serde::{Deserialize, Serialize};
use ulid::Ulid;

pub const VERSION: u32 = 1;

/// Second precision. §3's shape shows whole seconds, and sub-second digits only
/// make the git diff of a hand-edited tree louder.
pub fn now() -> Timestamp {
    let now = Timestamp::now();
    Timestamp::from_second(now.as_second()).unwrap_or(now)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tree {
    pub version: u32,
    pub slug: String,
    /// What the interview is for, in prose (§3). Required at `init`, so no path
    /// in the lib makes a tree without one.
    ///
    /// `default` rather than `Option`: a tree written before this field existed
    /// has to keep loading, and bumping `VERSION` for an additive field would
    /// make every one of them a §3 version rejection instead. Empty therefore
    /// means "written by an older hydra", which a consumer can tell apart from
    /// prose.
    #[serde(default)]
    pub intent: String,
    pub created_at: Timestamp,
    /// Keyed by slug, unique within the tree (§2).
    pub heads: BTreeMap<String, Head>,
}

impl Tree {
    pub fn new(slug: String, intent: String) -> Self {
        Tree {
            version: VERSION,
            slug,
            intent,
            created_at: now(),
            heads: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Head {
    pub id: Ulid,
    pub slug: String,
    pub question: String,
    pub parent: Option<String>,
    pub seq: u32,
    pub blocked_by: Vec<String>,
    pub status: Status,
    pub rev: u32,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
    pub answer: Option<Answer>,
    /// The single most recent superseded answer (§3); deeper history is git's.
    pub prior: Option<Answer>,
}

impl Head {
    pub fn touch(&mut self) {
        self.updated_at = now();
    }

    /// Every answer goes through here. `with_tree_mut` cannot know which heads a
    /// closure touched, so `rev` and `prior` have to be owned next to the field
    /// they track rather than remembered at each of §5's eight mutations.
    pub fn set_answer(&mut self, answer: Answer) {
        // Only a superseded answer displaces `prior`. Cutting a head that
        // `reopen` already emptied must not overwrite the answer `reopen` put
        // there: §3 says `prior` is the most recent superseded answer, and after
        // reopen-then-cut that is still the one from before the reopen.
        if let Some(superseded) = self.answer.replace(answer) {
            self.prior = Some(superseded);
        }
        self.status = Status::Answered;
        self.rev += 1;
        self.touch();
    }

    /// The `answered → open` half of §2's lifecycle, for both an explicit
    /// `reopen` and a cascade. The answer moves to `prior` rather than being
    /// dropped — that retained context is what makes re-answering one word.
    ///
    /// Hydra's convention, not something §2 settles: `rev` counts answers given,
    /// so a withdrawal does not move it. §2's "bumped when its answer changes"
    /// reads either way — emptying the field is a change — but a head reopened
    /// five times by upstream churn and never re-answered has not been revised
    /// five times, and the withdrawal is already legible from `status` + `prior`.
    ///
    /// Returns whether anything changed, so a cascade over already-open heads
    /// reports only the ones it actually reopened. Either marker of answeredness
    /// is enough to act on: gating on `answer` alone would let a hand-edited
    /// `{status: "answered", answer: null}` head pass `graph::reopen`'s status
    /// check and then stay answered.
    pub fn reopen(&mut self) -> bool {
        if self.status == Status::Open && self.answer.is_none() {
            return false;
        }
        if let Some(superseded) = self.answer.take() {
            self.prior = Some(superseded);
        }
        self.status = Status::Open;
        self.touch();
        true
    }

    pub fn set_question(&mut self, question: String) {
        self.question = question;
        self.touch();
    }

    /// `seq` is meaningless apart from the parent it orders under (§3), so the
    /// two move together and a reparent cannot forget to re-seat the head in its
    /// new sibling set.
    pub fn set_parent(&mut self, parent: Option<String>, seq: u32) {
        self.parent = parent;
        self.seq = seq;
        self.touch();
    }

    /// `blocked_by` is a set — §2 derives `blocked` from *any* member being open
    /// — so it is stored sorted and deduplicated. Two trees with the same edges
    /// then have the same bytes, which is §3's stable-diff property.
    pub fn set_blocked_by(&mut self, mut blocked_by: Vec<String>) {
        blocked_by.sort();
        blocked_by.dedup();
        self.blocked_by = blocked_by;
        self.touch();
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Open,
    Answered,
}

impl std::fmt::Display for Status {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Status::Open => "open",
            Status::Answered => "answered",
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Answer {
    pub text: String,
    pub rationale: Option<String>,
    pub rejected: Vec<Rejected>,
    pub cauterised_by: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rejected {
    pub option: String,
    pub why_not: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store;
    use std::str::FromStr;

    /// Copied from SPEC §3's file shape, with keys in the sorted order §3
    /// mandates and a second head to exercise the `heads` map. A renamed field
    /// fails `fixture_round_trips` in both directions.
    const FIXTURE: &str = r#"{
  "created_at": "2026-07-28T04:11:02Z",
  "heads": {
    "consumption-surface": {
      "answer": null,
      "blocked_by": [],
      "created_at": "2026-07-28T04:11:30Z",
      "id": "01J8XQ2K7T4V9WZ3N5M6P8R0AA",
      "parent": null,
      "prior": null,
      "question": "What does hydra look like from outside?",
      "rev": 0,
      "seq": 1,
      "slug": "consumption-surface",
      "status": "open",
      "updated_at": "2026-07-28T04:11:30Z"
    },
    "graph-shape": {
      "answer": {
        "cauterised_by": null,
        "rationale": "nesting keeps the resume dump legible; cross edges cost one cycle check",
        "rejected": [
          {
            "option": "strict tree",
            "why_not": "can't express cross-branch gating"
          },
          {
            "option": "pure DAG",
            "why_not": "render nondeterministic, loses 'where am I'"
          }
        ],
        "text": "spanning tree + blocked_by cross edges"
      },
      "blocked_by": [
        "consumption-surface"
      ],
      "created_at": "2026-07-28T04:12:00Z",
      "id": "01J8XQ2K7T4V9WZ3N5M6P8R0AB",
      "parent": "consumption-surface",
      "prior": null,
      "question": "Strict tree, tree + dep edges, or pure DAG?",
      "rev": 1,
      "seq": 2,
      "slug": "graph-shape",
      "status": "answered",
      "updated_at": "2026-07-28T04:19:31Z"
    }
  },
  "intent": "Design hydra itself: what it stores, what it refuses, what it looks like from outside.",
  "slug": "hydra-design",
  "version": 1
}
"#;

    fn ts(s: &str) -> Timestamp {
        Timestamp::from_str(s).unwrap()
    }

    fn fixture_tree() -> Tree {
        let root = Head {
            id: Ulid::from_string("01J8XQ2K7T4V9WZ3N5M6P8R0AA").unwrap(),
            slug: "consumption-surface".to_string(),
            question: "What does hydra look like from outside?".to_string(),
            parent: None,
            seq: 1,
            blocked_by: vec![],
            status: Status::Open,
            rev: 0,
            created_at: ts("2026-07-28T04:11:30Z"),
            updated_at: ts("2026-07-28T04:11:30Z"),
            answer: None,
            prior: None,
        };
        let shape = Head {
            id: Ulid::from_string("01J8XQ2K7T4V9WZ3N5M6P8R0AB").unwrap(),
            slug: "graph-shape".to_string(),
            question: "Strict tree, tree + dep edges, or pure DAG?".to_string(),
            parent: Some("consumption-surface".to_string()),
            seq: 2,
            blocked_by: vec!["consumption-surface".to_string()],
            status: Status::Answered,
            rev: 1,
            created_at: ts("2026-07-28T04:12:00Z"),
            updated_at: ts("2026-07-28T04:19:31Z"),
            answer: Some(Answer {
                text: "spanning tree + blocked_by cross edges".to_string(),
                rationale: Some(
                    "nesting keeps the resume dump legible; cross edges cost one cycle check"
                        .to_string(),
                ),
                rejected: vec![
                    Rejected {
                        option: "strict tree".to_string(),
                        why_not: "can't express cross-branch gating".to_string(),
                    },
                    Rejected {
                        option: "pure DAG".to_string(),
                        why_not: "render nondeterministic, loses 'where am I'".to_string(),
                    },
                ],
                cauterised_by: None,
            }),
            prior: None,
        };

        let mut heads = BTreeMap::new();
        heads.insert(shape.slug.clone(), shape);
        heads.insert(root.slug.clone(), root);
        Tree {
            version: VERSION,
            slug: "hydra-design".to_string(),
            intent: "Design hydra itself: what it stores, what it refuses, what it looks like from outside.".to_string(),
            created_at: ts("2026-07-28T04:11:02Z"),
            heads,
        }
    }

    #[test]
    fn fixture_round_trips() {
        let parsed: Tree = serde_json::from_str(FIXTURE).unwrap();
        assert_eq!(parsed, fixture_tree());
        assert_eq!(store::to_json(&fixture_tree()).unwrap(), FIXTURE);
    }

    #[test]
    fn absent_fields_are_null() {
        let json = store::to_json(&fixture_tree()).unwrap();
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        let root = &value["heads"]["consumption-surface"];
        for key in ["parent", "answer", "prior"] {
            assert!(root[key].is_null(), "{key} should be null");
        }
        assert!(value["heads"]["graph-shape"]["answer"]["cauterised_by"].is_null());
    }

    /// The whole reason `intent` is `#[serde(default)]` rather than a `VERSION`
    /// bump (§3): a tree committed before the field existed still loads, and
    /// reads as empty rather than as prose.
    #[test]
    fn a_tree_without_intent_loads_empty() {
        let mut value: serde_json::Value = serde_json::from_str(FIXTURE).unwrap();
        value.as_object_mut().unwrap().remove("intent");
        let parsed: Tree = serde_json::from_value(value).unwrap();
        assert_eq!(parsed.intent, "");
        assert_eq!(parsed.version, VERSION);
    }

    #[test]
    fn status_is_lowercase() {
        assert_eq!(serde_json::to_string(&Status::Open).unwrap(), r#""open""#);
        assert_eq!(
            serde_json::to_string(&Status::Answered).unwrap(),
            r#""answered""#
        );
        assert_eq!(
            serde_json::from_str::<Status>(r#""answered""#).unwrap(),
            Status::Answered
        );
        assert!(serde_json::from_str::<Status>(r#""cauterised""#).is_err());
    }

    #[test]
    fn keys_are_sorted_and_stable() {
        let tree = fixture_tree();
        let first = store::to_json(&tree).unwrap();
        assert_eq!(first, store::to_json(&tree).unwrap());

        // Asserted against the raw text, not a parsed Value: parsing back into
        // serde_json's BTreeMap-backed Map re-sorts the keys, so a Value-based
        // check passes whatever the file actually says.
        assert_keys_ascending(
            &first,
            2,
            &["created_at", "heads", "intent", "slug", "version"],
        );
        assert_keys_ascending(
            &first,
            6,
            &[
                "answer",
                "blocked_by",
                "created_at",
                "id",
                "parent",
                "prior",
                "question",
                "rev",
                "seq",
                "slug",
                "status",
                "updated_at",
            ],
        );
        assert_keys_ascending(
            &first,
            8,
            &["cauterised_by", "rationale", "rejected", "text"],
        );
    }

    fn assert_keys_ascending(json: &str, indent: usize, keys: &[&str]) {
        let pad = " ".repeat(indent);
        let mut previous = 0;
        for key in keys {
            let needle = format!("\n{pad}\"{key}\":");
            let at = json
                .find(&needle)
                .unwrap_or_else(|| panic!("no key {key:?} at indent {indent}:\n{json}"));
            assert!(at > previous, "key {key:?} is out of order:\n{json}");
            previous = at;
        }
    }

    #[test]
    fn timestamps_are_rfc3339_zulu() {
        let json = store::to_json(&Tree {
            created_at: ts("2026-07-28T04:11:02Z"),
            ..Tree::new("t".to_string(), "test intent".to_string())
        })
        .unwrap();
        assert!(
            json.contains(r#""created_at": "2026-07-28T04:11:02Z""#),
            "{json}"
        );
    }

    #[test]
    fn now_has_second_precision() {
        assert_eq!(now().subsec_nanosecond(), 0);
    }

    #[test]
    fn touch_advances_updated_at() {
        let mut head = fixture_tree().heads.remove("consumption-surface").unwrap();
        let created = head.created_at;
        head.touch();
        assert!(head.updated_at > created);
        assert_eq!(head.created_at, created, "created_at is immutable");
    }

    #[test]
    fn set_answer_captures_prior_and_bumps_rev() {
        let mut head = fixture_tree().heads.remove("consumption-surface").unwrap();
        let first = Answer {
            text: "CLI unix tool".to_string(),
            rationale: None,
            rejected: vec![],
            cauterised_by: None,
        };
        head.set_answer(first.clone());
        assert_eq!(head.status, Status::Answered);
        assert_eq!(head.rev, 1);
        assert_eq!(head.answer.as_ref(), Some(&first));
        assert_eq!(head.prior, None, "nothing was superseded yet");

        let second = Answer {
            text: "CLI unix tool plus an MCP shim".to_string(),
            ..first.clone()
        };
        head.set_answer(second.clone());
        assert_eq!(head.rev, 2);
        assert_eq!(head.answer.as_ref(), Some(&second));
        assert_eq!(
            head.prior.as_ref(),
            Some(&first),
            "prior holds the single most recent superseded answer"
        );
    }
}
