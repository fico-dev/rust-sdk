//! IDs that the SDK generates.
//!
//! Randomness comes from `crate::rand`, so no crate is needed.

use crate::rand::random_fraction;

/// Longest ID the platform accepts.
const MAX_ID_LEN: usize = 40;
/// Room left at the end of an ID for an index tag, such as `-001`.
const INDEX_TAG_LEN: usize = 3;
/// Length of the random suffix of an ID.
const SUFFIX_LEN: usize = 8;
/// Characters of the random suffix of an ID.
const SUFFIX_CHARS: &[u8; 36] = b"abcdefghijklmnopqrstuvwxyz0123456789";

/// A new ID: `prefix`, a dash and 8 random lowercase letters and digits,
/// e.g. `local-3k9x0q2a`. A long `prefix` is cut, so the ID always fits the
/// platform's ID length limit.
pub(crate) fn safe_id(prefix: &str) -> String {
    let max_prefix_len = MAX_ID_LEN - INDEX_TAG_LEN - (SUFFIX_LEN + 1);
    // A prefix of `max_prefix_len` fits, and a longer one is cut one
    // character shorter.
    let prefix: String = if prefix.chars().count() > max_prefix_len {
        prefix.chars().take(max_prefix_len - 1).collect()
    } else {
        prefix.to_owned()
    };
    format!("{prefix}-{}", random_suffix())
}

/// [`SUFFIX_LEN`] random characters from [`SUFFIX_CHARS`].
fn random_suffix() -> String {
    (0..SUFFIX_LEN)
        .map(|_| {
            // `random_fraction` is below 1, so the index is below 36.
            let index = (random_fraction() * SUFFIX_CHARS.len() as f64) as usize;
            char::from(SUFFIX_CHARS[index])
        })
        .collect()
}

/// A display name and an ID derived from a user-supplied `entity_id`: the ID
/// is `prefix`, `entity_id` in kebab-case, and 8 random characters, cut to
/// fit the platform's ID length limit; the name is the ID in Start Case.
///
/// For example, `("inpset", "My Scenario 12!")` gives
/// `("Inpset My Scenario 12 3k9x0q2a", "inpset-my-scenario-12-3k9x0q2a")`.
/// Returns two empty strings if `prefix` or `entity_id` is empty.
pub(crate) fn safe_name_and_id(prefix: &str, entity_id: &str) -> (String, String) {
    if prefix.is_empty() || entity_id.is_empty() {
        return (String::new(), String::new());
    }
    // The room for the words leaves out the prefix, and the check below
    // counts the prefix again, so IDs stay well under the limit.
    let room = MAX_ID_LEN - INDEX_TAG_LEN - (SUFFIX_LEN + 1);
    let max = room.saturating_sub(prefix.chars().count());
    let mut parts = vec![prefix.to_owned()];
    for word in kebab_case(entity_id).split('-') {
        let word: String = match word.chars().count() > max {
            true => word.chars().take(max.saturating_sub(1)).collect(),
            false => word.to_owned(),
        };
        let joined_len =
            parts.iter().map(|p| p.chars().count() + 1).sum::<usize>() + word.chars().count();
        if joined_len >= max {
            break;
        }
        parts.push(word);
    }
    parts.retain(|part| !part.is_empty());
    let id = format!("{}-{}", parts.join("-"), random_suffix());
    (start_case(&id), id)
}

/// `value` in kebab-case: its runs of ASCII letters and digits, lowercase,
/// joined by dashes. Everything else separates words.
fn kebab_case(value: &str) -> String {
    value
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|word| !word.is_empty())
        .map(str::to_ascii_lowercase)
        .collect::<Vec<_>>()
        .join("-")
}

/// `value` in Start Case: its words, split at dashes, underscores and
/// whitespace, each with an uppercase first letter and the rest lowercase,
/// joined by spaces.
fn start_case(value: &str) -> String {
    value
        .split(|c: char| c == '-' || c == '_' || c.is_whitespace())
        .filter(|word| !word.is_empty())
        .map(|word| {
            let mut chars = word.chars();
            match chars.next() {
                Some(first) => first
                    .to_uppercase()
                    .chain(chars.flat_map(char::to_lowercase))
                    .collect(),
                None => String::new(),
            }
        })
        .collect::<Vec<String>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    /// Whether `suffix` is 8 lowercase letters and digits.
    fn is_suffix(suffix: &str) -> bool {
        suffix.len() == SUFFIX_LEN
            && suffix
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
    }

    #[test]
    fn joins_the_prefix_and_a_random_suffix() {
        let id = safe_id("build");
        let (prefix, suffix) = id.split_once('-').unwrap();
        assert_eq!(prefix, "build");
        assert!(is_suffix(suffix), "{id}");
    }

    #[test]
    fn generates_unique_ids() {
        let ids: HashSet<String> = (0..1000).map(|_| safe_id("workspace")).collect();
        assert_eq!(ids.len(), 1000);
    }

    #[test]
    fn cuts_long_prefixes() {
        let fits = "a".repeat(28);
        let id = safe_id(&fits);
        assert_eq!(id.rsplit_once('-').unwrap().0, fits);

        let id = safe_id(&"b".repeat(29));
        let (prefix, suffix) = id.rsplit_once('-').unwrap();
        assert_eq!(prefix, "b".repeat(27));
        assert!(is_suffix(suffix), "{id}");
        assert!(id.len() <= MAX_ID_LEN - INDEX_TAG_LEN, "{id}");
    }

    #[test]
    fn derives_the_name_and_id_from_the_entity_id() {
        let (name, id) = safe_name_and_id("inpset", "My Scenario 12!");
        let suffix = id.strip_prefix("inpset-my-scenario-12-").unwrap();
        assert!(is_suffix(suffix), "{id}");
        assert_eq!(name, start_case(&id));
        assert!(name.starts_with("Inpset My Scenario 12 "), "{name}");
    }

    #[test]
    fn drops_words_that_do_not_fit() {
        let (_, id) = safe_name_and_id("inpset", "a very long scenario name that goes on");
        assert!(id.starts_with("inpset-a-very-long-"), "{id}");
        assert_eq!(id.len(), "inpset-a-very-long-".len() + SUFFIX_LEN, "{id}");
        // A single word longer than the room left is dropped.
        let (_, id) = safe_name_and_id("inpset", "supercalifragilisticexpialidocious");
        assert!(is_suffix(id.strip_prefix("inpset-").unwrap()), "{id}");
    }

    #[test]
    fn handles_empty_and_symbol_only_entity_ids() {
        assert_eq!(safe_name_and_id("", "x"), (String::new(), String::new()));
        assert_eq!(
            safe_name_and_id("inpset", ""),
            (String::new(), String::new())
        );
        let (name, id) = safe_name_and_id("inpset", "!!!");
        assert!(is_suffix(id.strip_prefix("inpset-").unwrap()), "{id}");
        assert_eq!(name, start_case(&id));
    }

    #[test]
    fn converts_case() {
        assert_eq!(kebab_case("  My_Workspace: v2.0 (é) "), "my-workspace-v2-0");
        assert_eq!(
            start_case("man-input_my--scenario 3k9x"),
            "Man Input My Scenario 3k9x"
        );
    }
}
