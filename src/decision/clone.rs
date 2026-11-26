//! The name, description and options a clone of a run gets, shared by
//! platform and local runs.

use std::collections::BTreeMap;

use crate::decision::RunInformation;

/// The longest name, in characters, that a clone gets.
const MAX_CLONE_NAME: usize = 128;

/// The longest description, in characters, that a clone gets.
const MAX_CLONE_DESCRIPTION: usize = 256;

/// The name of a clone of `cloned`: `name`, unless it is unset or empty,
/// cut to [`MAX_CLONE_NAME`] characters, or else the cloned run's name, or
/// its ID without one, followed by ` clone`.
pub(crate) fn clone_name(name: Option<&str>, cloned: &RunInformation) -> String {
    if let Some(name) = name.filter(|name| !name.is_empty()) {
        return truncate(name, MAX_CLONE_NAME);
    }
    const SUFFIX: &str = " clone";
    let base = match cloned.name.is_empty() {
        true => &cloned.id,
        false => &cloned.name,
    };
    truncate(base, MAX_CLONE_NAME - SUFFIX.len()) + SUFFIX
}

/// The description of a clone of `cloned`: `description`, unless it is
/// unset or empty, cut to [`MAX_CLONE_DESCRIPTION`] characters, or else
/// `Clone of ` and the cloned run's description, or its ID without one.
pub(crate) fn clone_description(description: Option<&str>, cloned: &RunInformation) -> String {
    if let Some(description) = description.filter(|description| !description.is_empty()) {
        return truncate(description, MAX_CLONE_DESCRIPTION);
    }
    const PREFIX: &str = "Clone of ";
    let base = match cloned.description.is_empty() {
        true => &cloned.id,
        false => &cloned.description,
    };
    PREFIX.to_owned() + &truncate(base, MAX_CLONE_DESCRIPTION - PREFIX.len())
}

/// The options of the run `cloned`, for a clone of it, from its options
/// summary.
pub(crate) fn cloned_options(cloned: &RunInformation) -> BTreeMap<String, String> {
    let options = cloned.metadata.options.as_ref();
    let summary = options.and_then(|options| options.options_summary.as_ref());
    let summary = summary.into_iter().flatten();
    summary
        .map(|option| (option.name.clone(), option.value.clone()))
        .collect()
}

/// The first `max` characters of `text`.
fn truncate(text: &str, max: usize) -> String {
    text.chars().take(max).collect()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    const RUN_ID: &str = "run-3k9x0q2a";

    /// A run, as the cloned run of a clone, with `name` and `description`.
    fn cloned(name: &str, description: &str) -> RunInformation {
        serde_json::from_value(json!({
            "id": RUN_ID,
            "user_email": "",
            "name": name,
            "description": description,
            "metadata": {
                "application_id": "routing",
                "application_instance_id": "",
                "application_version_id": "",
                "created_at": "2026-10-13T15:00:00Z",
                "duration": 0,
                "error": "",
                "format": { "input": { "type": "json" } },
                "input_size": 0,
                "output_size": 0,
                "status_v2": "succeeded",
                "options": {
                    "options_summary": [
                        { "name": "duration", "source": "run", "value": "10s" },
                        { "name": "iterations", "source": "environment", "value": "50" }
                    ]
                }
            }
        }))
        .unwrap()
    }

    #[test]
    fn names_a_clone() {
        let run = cloned("Monday", "");
        assert_eq!(clone_name(None, &run), "Monday clone");
        assert_eq!(clone_name(Some(""), &run), "Monday clone");
        assert_eq!(clone_name(Some("Tuesday"), &run), "Tuesday");
        assert_eq!(clone_name(None, &cloned("", "")), format!("{RUN_ID} clone"));
        let long = "é".repeat(200);
        assert_eq!(clone_name(Some(&long), &run), "é".repeat(128));
        let clone = clone_name(None, &cloned(&long, ""));
        assert_eq!(clone, "é".repeat(122) + " clone");
        assert_eq!(clone.chars().count(), 128);
    }

    #[test]
    fn describes_a_clone() {
        let run = cloned("", "Monday's routes");
        assert_eq!(clone_description(None, &run), "Clone of Monday's routes");
        assert_eq!(
            clone_description(Some(""), &run),
            "Clone of Monday's routes"
        );
        assert_eq!(clone_description(Some("Mine"), &run), "Mine");
        let without = cloned("", "");
        assert_eq!(
            clone_description(None, &without),
            format!("Clone of {RUN_ID}")
        );
        let long = "d".repeat(300);
        assert_eq!(clone_description(Some(&long), &run), "d".repeat(256));
        let clone = clone_description(None, &cloned("", &long));
        assert_eq!(clone, "Clone of ".to_owned() + &"d".repeat(247));
        assert_eq!(clone.chars().count(), 256);
    }

    #[test]
    fn takes_the_options_of_the_summary() {
        let options = cloned_options(&cloned("", ""));
        let expected = [("duration", "10s"), ("iterations", "50")]
            .map(|(name, value)| (name.to_owned(), value.to_owned()));
        assert_eq!(options, BTreeMap::from(expected));
    }
}
