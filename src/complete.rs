//! Completion candidates (SPEC §5). Bin-side, so the lib stays clap-free.
//!
//! Every entry point swallows its errors into an empty list: this code runs
//! inside the user's command line, where a diagnostic is written into the line
//! being edited rather than onto a terminal anyone is reading.

use clap_complete::CompletionCandidate;

use hydra::model::{Head, Status, Tree};
use hydra::{Store, query, render};

/// Trees in the store, described by the first line of the intent.
pub fn trees() -> Vec<CompletionCandidate> {
    let Ok(store) = Store::discover() else {
        return vec![];
    };
    let Ok(slugs) = store.trees() else {
        return vec![];
    };
    slugs
        .into_iter()
        .enumerate()
        .map(|(order, slug)| {
            let intent = store
                .load(&slug)
                .map(|tree| query::first_line(&tree.intent).to_string())
                .unwrap_or_default();
            candidate(slug, intent, order)
        })
        .collect()
}

/// Every head of the HEAD tree.
pub fn heads() -> Vec<CompletionCandidate> {
    head_candidates(|_| true)
}

/// The heads `reopen` and `cauterise --by` accept (§4.6, §4.7).
pub fn answered_heads() -> Vec<CompletionCandidate> {
    head_candidates(|head| head.status == Status::Answered)
}

fn head_candidates(keep: impl Fn(&Head) -> bool) -> Vec<CompletionCandidate> {
    let Some(tree) = head_tree() else {
        return vec![];
    };
    query::preorder(&tree)
        .into_iter()
        .filter(|visit| keep(visit.head))
        .enumerate()
        .map(|(order, visit)| {
            let state = render::glyph(query::state(&tree, visit.head));
            let question = query::first_line(&visit.head.question);
            candidate(visit.slug, format!("{state} {question}"), order)
        })
        .collect()
}

/// The pre-order index rides along as the display order: it is the order `tree`,
/// `resume` and `next` walk the tree in (§5), so a slug's neighbours in the list
/// are its neighbours in the interview. Shells that sort candidates themselves
/// ignore it.
fn candidate(value: impl Into<String>, help: String, order: usize) -> CompletionCandidate {
    CompletionCandidate::new(value.into())
        .help((!help.is_empty()).then(|| help.into()))
        .display_order(Some(order))
}

fn head_tree() -> Option<Tree> {
    let store = Store::discover().ok()?;
    store.load(&store.head().ok()?).ok()
}
