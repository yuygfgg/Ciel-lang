use std::collections::HashSet;

const MAX_SUGGESTIONS: usize = 5;

#[derive(Clone, Debug, PartialEq, Eq)]
struct RankedSuggestion {
    distance: usize,
    compare: String,
    display: String,
}

pub fn did_you_mean_note<I, S>(needle: &str, candidates: I) -> Option<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    did_you_mean_note_with_display(
        needle,
        candidates
            .into_iter()
            .map(|candidate| {
                let name = candidate.as_ref().to_string();
                (name.clone(), name)
            })
            .collect::<Vec<_>>(),
    )
}

pub fn did_you_mean_note_with_display<I>(needle: &str, candidates: I) -> Option<String>
where
    I: IntoIterator<Item = (String, String)>,
{
    let suggestions = closest_suggestions(needle, candidates);
    match suggestions.as_slice() {
        [] => None,
        [only] => Some(format!("did you mean `{}`?", only.display)),
        _ => Some(format!(
            "did you mean one of: {}?",
            suggestions
                .iter()
                .map(|suggestion| format!("`{}`", suggestion.display))
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}

pub fn available_names_note<I, S>(label: &str, candidates: I) -> Option<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut seen = HashSet::new();
    let mut names = candidates
        .into_iter()
        .map(|candidate| candidate.as_ref().to_string())
        .filter(|candidate| !candidate.is_empty())
        .filter(|candidate| seen.insert(candidate.clone()))
        .collect::<Vec<_>>();
    names.sort();
    if names.is_empty() {
        return None;
    }
    let omitted = names.len().saturating_sub(MAX_SUGGESTIONS);
    names.truncate(MAX_SUGGESTIONS);
    let mut parts = names
        .into_iter()
        .map(|name| format!("`{name}`"))
        .collect::<Vec<_>>();
    if omitted > 0 {
        parts.push(format!("and {omitted} more"));
    }
    Some(format!("{label}: {}", parts.join(", ")))
}

fn closest_suggestions<I>(needle: &str, candidates: I) -> Vec<RankedSuggestion>
where
    I: IntoIterator<Item = (String, String)>,
{
    if needle.is_empty() {
        return Vec::new();
    }
    let needle_folded = needle.to_ascii_lowercase();
    let max_distance = max_distance_for(needle_folded.len());
    let mut seen_displays = HashSet::new();
    let mut ranked = candidates
        .into_iter()
        .filter(|(compare, display)| {
            !compare.is_empty()
                && compare != needle
                && !display.is_empty()
                && seen_displays.insert(display.clone())
        })
        .filter_map(|(compare, display)| {
            let compare_folded = compare.to_ascii_lowercase();
            let distance = levenshtein(&needle_folded, &compare_folded);
            let prefix_match = compare_folded.starts_with(&needle_folded)
                || needle_folded.starts_with(&compare_folded);
            if distance <= max_distance || prefix_match {
                Some(RankedSuggestion {
                    distance,
                    compare,
                    display,
                })
            } else {
                None
            }
        })
        .collect::<Vec<_>>();
    ranked.sort_by(|left, right| {
        left.distance
            .cmp(&right.distance)
            .then_with(|| left.compare.cmp(&right.compare))
            .then_with(|| left.display.cmp(&right.display))
    });
    ranked.truncate(MAX_SUGGESTIONS);
    ranked
}

fn max_distance_for(len: usize) -> usize {
    match len {
        0..=3 => 1,
        4..=8 => 2,
        _ => 3,
    }
}

fn levenshtein(left: &str, right: &str) -> usize {
    if left == right {
        return 0;
    }
    if left.is_empty() {
        return right.chars().count();
    }
    if right.is_empty() {
        return left.chars().count();
    }

    let right_chars = right.chars().collect::<Vec<_>>();
    let mut previous = (0..=right_chars.len()).collect::<Vec<_>>();
    let mut current = vec![0; right_chars.len() + 1];

    for (left_idx, left_ch) in left.chars().enumerate() {
        current[0] = left_idx + 1;
        for (right_idx, right_ch) in right_chars.iter().enumerate() {
            let substitution = previous[right_idx] + usize::from(left_ch != *right_ch);
            let insertion = current[right_idx] + 1;
            let deletion = previous[right_idx + 1] + 1;
            current[right_idx + 1] = substitution.min(insertion).min(deletion);
        }
        std::mem::swap(&mut previous, &mut current);
    }
    previous[right_chars.len()]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn suggests_transposed_identifier() {
        let note = did_you_mean_note("actro", ["actor"]).unwrap();
        assert_eq!(note, "did you mean `actor`?");
    }

    #[test]
    fn ignores_distant_identifier() {
        assert!(did_you_mean_note("x", ["actor", "value"]).is_none());
    }

    #[test]
    fn supports_separate_compare_and_display() {
        let note = did_you_mean_note_with_display(
            "Succes",
            [("Success".to_string(), "Status::Success".to_string())],
        )
        .unwrap();
        assert_eq!(note, "did you mean `Status::Success`?");
    }
}
