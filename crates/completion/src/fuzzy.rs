//! Fuzzy subsequence matcher for schema search (Phase D).
//!
//! Command-palette style: `usml` matches `user_email`. A candidate matches
//! only if every query char appears in order in the haystack (cheap
//! left-to-right scan that rejects most candidates before scoring). Survivors
//! are scored by contiguity, word-boundary starts, and how early the match
//! begins — so tighter, earlier, boundary-aligned matches rank higher.
//!
//! Pure and allocation-light on the reject path (no allocation until a match
//! is confirmed). Callers pass a pre-lowercased haystack; the query is
//! lowercased here.

/// Scoring weights. Higher score = better match.
const BONUS_MATCH: i32 = 16; // base per matched char
const BONUS_CONTIGUOUS: i32 = 18; // matched char adjacent to the previous match
const BONUS_BOUNDARY: i32 = 24; // match at a word boundary (start / after _ . / camelCase)
const BONUS_FIRST: i32 = 12; // match at the very start of the haystack
const PENALTY_GAP: i32 = 3; // per skipped char between matches
const PENALTY_LEADING: i32 = 2; // per char skipped before the first match
const PENALTY_LENGTH: i32 = 1; // per haystack char beyond the query length

/// A successful fuzzy match: a score and the byte ranges in the haystack that
/// matched (merged into contiguous runs), for client-side highlighting.
#[derive(Debug, Clone, PartialEq)]
pub struct FuzzyMatch {
    pub score: i32,
    pub ranges: Vec<(u32, u32)>,
}

/// Match `query` against `haystack_lower` (already lowercased by the caller).
/// Returns `None` when `query` is not an in-order subsequence. An empty query
/// matches everything with score 0 (no ranges).
pub fn fuzzy_match(query: &str, haystack_lower: &str) -> Option<FuzzyMatch> {
    if query.is_empty() {
        return Some(FuzzyMatch {
            score: 0,
            ranges: Vec::new(),
        });
    }

    // Lowercase the query; iterate its chars in order.
    let q: Vec<char> = query.chars().flat_map(char::to_lowercase).collect();
    let mut qi = 0usize;

    let mut score = 0i32;
    let mut ranges: Vec<(u32, u32)> = Vec::new();
    let mut haystack_chars = 0usize;

    let mut prev_matched_char_idx: Option<usize> = None;
    let mut prev_haystack_char: Option<char> = None;

    for (char_idx, (byte_idx, ch)) in haystack_lower.char_indices().enumerate() {
        haystack_chars += 1;
        if qi >= q.len() {
            continue;
        }
        if ch == q[qi] {
            // A match. Score it.
            score += BONUS_MATCH;

            // Word boundary: start of string, or right after a separator such
            // as `_` / `.` (the haystack is already lowercased, so camelCase
            // boundaries aren't visible here).
            let at_boundary = prev_haystack_char.map_or(true, |prev| !prev.is_alphanumeric());
            if at_boundary {
                score += BONUS_BOUNDARY;
            }
            if char_idx == 0 {
                score += BONUS_FIRST;
            }
            match prev_matched_char_idx {
                Some(prev) if prev + 1 == char_idx => {
                    score += BONUS_CONTIGUOUS;
                    // extend the last range
                    if let Some(last) = ranges.last_mut() {
                        last.1 = (byte_idx + ch.len_utf8()) as u32;
                    }
                }
                Some(prev) => {
                    score -= PENALTY_GAP * (char_idx - prev - 1) as i32;
                    ranges.push((byte_idx as u32, (byte_idx + ch.len_utf8()) as u32));
                }
                None => {
                    score -= PENALTY_LEADING * char_idx as i32;
                    ranges.push((byte_idx as u32, (byte_idx + ch.len_utf8()) as u32));
                }
            }

            prev_matched_char_idx = Some(char_idx);
            qi += 1;
        }
        prev_haystack_char = Some(ch);
    }

    if qi < q.len() {
        return None; // query was not a full subsequence
    }

    // Prefer shorter haystacks: penalize length beyond the matched query.
    let extra = haystack_chars.saturating_sub(q.len());
    score -= PENALTY_LENGTH * extra as i32;

    Some(FuzzyMatch { score, ranges })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subsequence_matches() {
        assert!(fuzzy_match("usml", "user_email").is_some());
        assert!(fuzzy_match("ue", "user_email").is_some());
        assert!(fuzzy_match("xyz", "user_email").is_none());
        // out of order fails
        assert!(fuzzy_match("lu", "user_email").is_none());
    }

    #[test]
    fn empty_query_matches_with_zero() {
        let m = fuzzy_match("", "anything").unwrap();
        assert_eq!(m.score, 0);
        assert!(m.ranges.is_empty());
    }

    #[test]
    fn prefix_beats_scattered() {
        let prefix = fuzzy_match("user", "user_email").unwrap();
        let scattered = fuzzy_match("user", "quaint_stellar_erosion").unwrap();
        assert!(
            prefix.score > scattered.score,
            "prefix {} should beat scattered {}",
            prefix.score,
            scattered.score
        );
    }

    #[test]
    fn boundary_aligned_match_scores_higher() {
        // Greedy match lands `id` on the `_id` boundary in "user_id" (bonus),
        // but mid-word in "void" (no bonus). Both are contiguous.
        let boundary = fuzzy_match("id", "user_id").unwrap();
        let midword = fuzzy_match("id", "void").unwrap();
        assert!(
            boundary.score > midword.score,
            "boundary {} should beat midword {}",
            boundary.score,
            midword.score
        );
    }

    #[test]
    fn ranges_cover_matched_bytes() {
        let m = fuzzy_match("use", "user_email").unwrap();
        // contiguous "use" -> single range 0..3
        assert_eq!(m.ranges, vec![(0, 3)]);
    }

    #[test]
    fn shorter_haystack_preferred() {
        let short = fuzzy_match("id", "id").unwrap();
        let long = fuzzy_match("id", "identifier_column_name").unwrap();
        assert!(short.score > long.score);
    }
}
