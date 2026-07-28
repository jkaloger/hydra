//! Slug handles per SPEC §2: `^[a-z0-9][a-z0-9-]*$`.

use crate::{Error, Result};

pub fn is_valid(slug: &str) -> bool {
    let mut chars = slug.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !first.is_ascii_lowercase() && !first.is_ascii_digit() {
        return false;
    }
    chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

pub fn validate(slug: &str) -> Result<()> {
    if is_valid(slug) {
        Ok(())
    } else {
        Err(Error::MalformedSlug {
            slug: slug.to_string(),
        })
    }
}

/// Mechanical transliteration of arbitrary text into slug shape. Hydra has no
/// opinion about question text (SPEC §1 non-goals), so this does not shorten,
/// stem or drop stop words — it lowercases, replaces every non-`[a-z0-9]` run
/// with a single `-`, and trims. The result can be empty, which `validate`
/// rejects; callers pick their own fallback.
pub fn slugify(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.ends_with('-') {
            out.push('-');
        }
    }
    out.trim_matches('-').to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts() {
        for slug in [
            "a",
            "0",
            "graph-shape",
            "0abc",
            "9",
            "a-b-c",
            "a--b",
            // Trailing and doubled `-` are legal per the regex; only the first
            // character is constrained.
            "a-",
            "a---",
            "consumption-surface-2",
        ] {
            assert!(is_valid(slug), "should accept {slug:?}");
            assert!(validate(slug).is_ok());
        }
    }

    #[test]
    fn rejects() {
        for slug in [
            "",
            "-",
            "-a",
            "--a",
            "A",
            "aB",
            "GRAPH-SHAPE",
            "a_b",
            "a b",
            "a.b",
            "a/b",
            "a:b",
            "graph shape",
            "héllo",
            "café",
            "日本語",
            "🐍",
            "a\n",
            " a",
            "a ",
        ] {
            assert!(!is_valid(slug), "should reject {slug:?}");
            assert!(matches!(validate(slug), Err(Error::MalformedSlug { .. })));
        }
    }

    #[test]
    fn slugify_is_mechanical() {
        let cases = [
            (
                "Strict tree, tree + dep edges, or pure DAG?",
                "strict-tree-tree-dep-edges-or-pure-dag",
            ),
            ("  Hello   World  ", "hello-world"),
            ("ABC", "abc"),
            ("already-a-slug", "already-a-slug"),
            ("snake_case_text", "snake-case-text"),
            ("Ünicode ünder pressure", "nicode-nder-pressure"),
            ("2 heads", "2-heads"),
            ("!!!", ""),
            ("", ""),
        ];
        for (input, expected) in cases {
            assert_eq!(slugify(input), expected, "slugify({input:?})");
        }
    }

    #[test]
    fn slugify_output_validates_when_nonempty() {
        for text in [
            "Strict tree, tree + dep edges, or pure DAG?",
            "-- leading punctuation --",
            "9 lives",
            "日本語 and text",
        ] {
            let slug = slugify(text);
            if !slug.is_empty() {
                assert!(is_valid(&slug), "slugify({text:?}) = {slug:?}");
            }
        }
    }
}
