//! Fuzzy subsequence matcher for the command palette.
//!
//! An fzf-style dynamic program rather than a greedy scan: greedy matching
//! locks onto the first occurrence of each pattern character and then scores
//! poorly on the cases launchers hit constantly (`vsc` → "Visual Studio Code",
//! `ps` → "PowerShell"). The DP considers every alignment and keeps the best.
//!
//! Cost is `O(|pattern| × |text|)` per candidate with a fast rejection pass in
//! front, which measures well under a millisecond for a few thousand entries —
//! see `benches` note in TODO.md.

/// Score awarded for a matched character.
const SCORE_MATCH: i32 = 16;
/// Penalty for opening a gap between matched characters.
const SCORE_GAP_START: i32 = -3;
/// Penalty for each additional character in an existing gap.
const SCORE_GAP_EXTENSION: i32 = -1;
/// Match immediately after a separator (space, `-`, `_`, `.`, `/`, `\`).
const BONUS_BOUNDARY: i32 = SCORE_MATCH / 2;
/// Match at a camelCase hump (`lower` → `Upper`).
const BONUS_CAMEL: i32 = BONUS_BOUNDARY + SCORE_GAP_EXTENSION;
/// Match directly after another match.
const BONUS_CONSECUTIVE: i32 = -(SCORE_GAP_START + SCORE_GAP_EXTENSION);
/// Multiplier applied to the bonus of the very first matched character.
const BONUS_FIRST_CHAR_MULTIPLIER: i32 = 2;
/// Extra credit, applied per matched character that lands on a word start.
///
/// Gap penalties otherwise let a dense substring outrank an acronym spread
/// across word initials — `vsc` would prefer "Advanced **vsc**onfig backup" to
/// "**V**isual **S**tudio **C**ode", which is the wrong answer for a launcher.
/// This is scored after the alignment is chosen, so it rewards the alignment
/// the DP picked rather than steering it; that is sufficient in practice
/// because acronym and substring alignments rarely compete inside one string.
const BONUS_WORD_START: i32 = 8;

/// Longest haystack the matcher will consider, in characters.
///
/// Bounds the `O(|pattern| x |text|)` dynamic program against titles supplied
/// by other processes. See `no_panic_on_pathological_input`.
pub const MAX_HAYSTACK: usize = 512;

/// A successful match: its score and the indices (in `char` units) that matched.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Match {
    pub score: i32,
    pub positions: Vec<usize>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum CharClass {
    White,
    NonWord,
    Digit,
    Lower,
    Upper,
}

fn class_of(c: char) -> CharClass {
    if c.is_whitespace() {
        CharClass::White
    } else if c.is_ascii_digit() || c.is_numeric() {
        CharClass::Digit
    } else if c.is_lowercase() {
        CharClass::Lower
    } else if c.is_uppercase() {
        CharClass::Upper
    } else if c.is_alphabetic() {
        // Scripts without case (CJK, Arabic, …) behave like lowercase letters:
        // they are word characters, but never a camelCase hump.
        CharClass::Lower
    } else {
        CharClass::NonWord
    }
}

/// Positional bonus for matching at `cur`, given the preceding character.
fn bonus_for(prev: Option<CharClass>, cur: CharClass) -> i32 {
    match prev {
        // Start of string counts as the strongest boundary.
        None => BONUS_BOUNDARY,
        Some(CharClass::White) | Some(CharClass::NonWord) => BONUS_BOUNDARY,
        Some(CharClass::Lower) if cur == CharClass::Upper => BONUS_CAMEL,
        Some(CharClass::Digit) if cur != CharClass::Digit => BONUS_CAMEL,
        _ => 0,
    }
}

/// Case-insensitive equality without allocating.
fn chars_eq(a: char, b: char) -> bool {
    a == b || a.to_lowercase().eq(b.to_lowercase())
}

/// Match `pattern` against `text`, returning `None` if `pattern` is not a
/// subsequence of `text` (case-insensitively).
///
/// An empty pattern matches everything with score `0` and no highlights, which
/// lets the palette show its full list before the user types.
pub fn match_score(pattern: &str, text: &str) -> Option<Match> {
    if pattern.is_empty() {
        return Some(Match {
            score: 0,
            positions: Vec::new(),
        });
    }

    let p: Vec<char> = pattern.chars().collect();
    let mut t: Vec<char> = text.chars().collect();

    // Window titles come from other processes and are not length-limited. The
    // dynamic program allocates two `m * n` matrices, so a hostile 2 MB title
    // would allocate hundreds of megabytes on every keystroke. No launcher
    // needs to match beyond a few hundred characters.
    if t.len() > MAX_HAYSTACK {
        t.truncate(MAX_HAYSTACK);
    }

    if p.len() > t.len() {
        return None;
    }

    // Fast rejection: a cheap greedy subsequence test. Most candidates fail
    // here, so the quadratic DP only runs for plausible matches.
    {
        let mut pi = 0usize;
        for &tc in &t {
            if pi < p.len() && chars_eq(p[pi], tc) {
                pi += 1;
            }
        }
        if pi != p.len() {
            return None;
        }
    }

    let n = t.len();
    let m = p.len();

    // Precompute the positional bonus for every text index once.
    let mut bonus = vec![0i32; n];
    let mut prev_class: Option<CharClass> = None;
    for (j, &tc) in t.iter().enumerate() {
        let cls = class_of(tc);
        bonus[j] = bonus_for(prev_class, cls);
        prev_class = Some(cls);
    }

    const NEG: i32 = i32::MIN / 2;

    // `mat[i][j]`: best score for pattern[..=i] where p[i] is matched at t[j].
    // `best[i][j]`: best score for pattern[..=i] using only t[..=j].
    let mut mat = vec![NEG; m * n];
    let mut best = vec![NEG; m * n];

    for i in 0..m {
        let mut prev_best = NEG;
        for j in 0..n {
            let mut match_here = NEG;
            if chars_eq(p[i], t[j]) {
                let base = if i == 0 {
                    // The first pattern character may start anywhere; leading
                    // text is skipped for free.
                    0
                } else if j == 0 {
                    NEG // later pattern chars need something before them
                } else {
                    best[(i - 1) * n + (j - 1)]
                };

                if base > NEG {
                    let mut b = bonus[j];
                    if i == 0 {
                        b *= BONUS_FIRST_CHAR_MULTIPLIER;
                    } else {
                        // Reward a run: p[i-1] matched exactly at t[j-1].
                        let consecutive = mat[(i - 1) * n + (j - 1)];
                        if consecutive > NEG && consecutive == base {
                            b = b.max(BONUS_CONSECUTIVE);
                        }
                    }
                    match_here = base + SCORE_MATCH + b;
                }
            }

            // Skipping t[j] for this pattern position.
            let skip = if j == 0 {
                NEG
            } else {
                let prev = best[i * n + (j - 1)];
                if prev <= NEG {
                    NEG
                } else {
                    let was_match = mat[i * n + (j - 1)] == prev;
                    prev + if was_match {
                        SCORE_GAP_START
                    } else {
                        SCORE_GAP_EXTENSION
                    }
                }
            };

            mat[i * n + j] = match_here;
            let b = match_here.max(skip);
            best[i * n + j] = b;
            prev_best = prev_best.max(b);
        }
        if prev_best <= NEG {
            return None; // pattern[..=i] cannot be placed at all
        }
    }

    // Best final score over all end positions of the last pattern char.
    let last = m - 1;
    let mut end = 0usize;
    let mut score = NEG;
    for j in 0..n {
        if mat[last * n + j] > score {
            score = mat[last * n + j];
            end = j;
        }
    }
    if score <= NEG {
        return None;
    }

    // Backtrack through `mat` to recover the chosen alignment.
    let mut positions = Vec::with_capacity(m);
    let mut i = last as isize;
    let mut j = end as isize;
    while i >= 0 {
        // Walk left to the cell that produced this match.
        while j >= 0 && mat[i as usize * n + j as usize] <= NEG {
            j -= 1;
        }
        if j < 0 {
            break;
        }
        // Among equal-scoring cells prefer the right-most (already at `j`).
        positions.push(j as usize);
        i -= 1;
        j -= 1;
        if i >= 0 {
            // Find the best predecessor cell at or before j.
            let mut bj = j;
            let mut bscore = NEG;
            let mut k = j;
            while k >= 0 {
                let v = mat[i as usize * n + k as usize];
                if v > bscore {
                    bscore = v;
                    bj = k;
                }
                k -= 1;
            }
            j = bj;
        }
    }
    positions.reverse();

    // Reward characters that landed on word starts (see BONUS_WORD_START).
    let word_starts = positions
        .iter()
        .filter(|&&pos| bonus[pos] >= BONUS_BOUNDARY)
        .count() as i32;
    let score = score + word_starts * BONUS_WORD_START;

    // Slight preference for shorter haystacks so exact-ish names win.
    let score = score - (n as i32) / 8;

    Some(Match { score, positions })
}

/// Rank `candidates` against `pattern`, best first.
///
/// Ties break on the candidate's own index so ordering is deterministic — the
/// palette must not reshuffle equal-scoring entries between keystrokes.
pub fn rank<'a, T, F>(pattern: &str, candidates: &'a [T], key: F) -> Vec<(usize, Match)>
where
    F: Fn(&'a T) -> &'a str,
{
    let mut out: Vec<(usize, Match)> = candidates
        .iter()
        .enumerate()
        .filter_map(|(i, c)| match_score(pattern, key(c)).map(|m| (i, m)))
        .collect();
    out.sort_by(|a, b| b.1.score.cmp(&a.1.score).then_with(|| a.0.cmp(&b.0)));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn score(p: &str, t: &str) -> Option<i32> {
        match_score(p, t).map(|m| m.score)
    }

    #[test]
    fn empty_pattern_matches_everything() {
        let m = match_score("", "anything").unwrap();
        assert_eq!(m.score, 0);
        assert!(m.positions.is_empty());
    }

    #[test]
    fn non_subsequence_does_not_match() {
        assert!(match_score("xyz", "Visual Studio Code").is_none());
        assert!(match_score("zzz", "abc").is_none());
        // Right characters, wrong order.
        assert!(match_score("cba", "abc").is_none());
    }

    #[test]
    fn pattern_longer_than_text_does_not_match() {
        assert!(match_score("abcdef", "abc").is_none());
    }

    #[test]
    fn matching_is_case_insensitive() {
        assert!(match_score("VSC", "visual studio code").is_some());
        assert!(match_score("vsc", "Visual Studio Code").is_some());
    }

    #[test]
    fn positions_are_valid_and_ordered() {
        let m = match_score("vsc", "Visual Studio Code").unwrap();
        assert_eq!(m.positions.len(), 3);
        for w in m.positions.windows(2) {
            assert!(
                w[0] < w[1],
                "positions must be strictly increasing: {:?}",
                m.positions
            );
        }
        let chars: Vec<char> = "Visual Studio Code".chars().collect();
        for (k, &pos) in m.positions.iter().enumerate() {
            let pc = "vsc".chars().nth(k).unwrap();
            assert!(
                chars_eq(chars[pos], pc),
                "position {pos} does not hold {pc}"
            );
        }
    }

    #[test]
    fn word_initials_beat_a_dense_run_elsewhere() {
        // The classic launcher case: "vsc" should prefer the acronym.
        let acronym = score("vsc", "Visual Studio Code").unwrap();
        let buried = score("vsc", "Advanced vsconfig backup utility").unwrap();
        assert!(
            acronym > buried,
            "acronym {acronym} should beat buried {buried}"
        );
    }

    #[test]
    fn prefix_beats_midword() {
        let a = score("term", "Terminal").unwrap();
        let b = score("term", "Windows Terminal Preview").unwrap();
        assert!(a > b, "prefix {a} should beat later match {b}");
    }

    #[test]
    fn consecutive_beats_scattered() {
        let a = score("abc", "abcdefgh").unwrap();
        let b = score("abc", "axbxcxdx").unwrap();
        assert!(a > b, "consecutive {a} should beat scattered {b}");
    }

    #[test]
    fn shorter_text_wins_on_equal_structure() {
        let a = score("note", "Notepad").unwrap();
        let b = score("note", "Notepad Extended Edition Pro").unwrap();
        assert!(a > b);
    }

    #[test]
    fn camel_case_humps_are_rewarded() {
        let a = score("ps", "PowerShell").unwrap();
        let b = score("ps", "pothos").unwrap();
        assert!(a > b, "camel {a} should beat non-boundary {b}");
    }

    #[test]
    fn separators_create_boundaries() {
        assert!(score("gc", "git-commit").unwrap() > score("gc", "gigantic").unwrap());
        assert!(score("gc", "git_commit").is_some());
        assert!(score("gc", "git.commit").is_some());
    }

    #[test]
    fn unicode_text_is_handled_by_char_not_byte() {
        let m = match_score("üb", "Übersicht").unwrap();
        assert_eq!(m.positions[0], 0);
        // Emoji must not panic or produce byte offsets.
        assert!(match_score("ab", "a🎉b").is_some());
        let m2 = match_score("ab", "a🎉b").unwrap();
        assert_eq!(m2.positions, vec![0, 2]);
    }

    #[test]
    fn cjk_matches() {
        assert!(match_score("記", "メモ帳 記録").is_some());
    }

    #[test]
    fn single_char_pattern_prefers_start() {
        let a = score("c", "Code").unwrap();
        let b = score("c", "Visual Code").unwrap();
        assert!(a > b);
    }

    #[test]
    fn full_string_match_is_highest() {
        let m = match_score("notepad", "Notepad").unwrap();
        assert_eq!(m.positions, vec![0, 1, 2, 3, 4, 5, 6]);
    }

    #[test]
    fn rank_orders_best_first_and_is_deterministic() {
        let apps = ["Visual Studio Code", "Vim", "Volume Control", "Discord"];
        let ranked = rank("vc", &apps, |s| *s);
        assert!(!ranked.is_empty());
        // Every result must actually match.
        for (i, _) in &ranked {
            assert!(match_score("vc", apps[*i]).is_some());
        }
        // Scores must be non-increasing.
        for w in ranked.windows(2) {
            assert!(w[0].1.score >= w[1].1.score);
        }
        // Deterministic across runs.
        assert_eq!(ranked, rank("vc", &apps, |s| *s));
    }

    #[test]
    fn rank_with_empty_pattern_keeps_input_order() {
        let apps = ["b", "a", "c"];
        let ranked = rank("", &apps, |s| *s);
        assert_eq!(
            ranked.iter().map(|(i, _)| *i).collect::<Vec<_>>(),
            vec![0, 1, 2]
        );
    }

    #[test]
    fn rank_excludes_non_matches() {
        let apps = ["Notepad", "Calculator", "Paint"];
        let ranked = rank("zzz", &apps, |s| *s);
        assert!(ranked.is_empty());
    }

    #[test]
    fn a_hostile_title_cannot_drive_an_unbounded_allocation() {
        // Another process can set a window title of any length; the palette
        // matches against it on every keystroke.
        let hostile: String = "abcdefghij".repeat(200_000); // 2M chars
        let m = match_score("abcdefghij", &hostile).expect("should still match");
        // Positions must stay inside the truncated haystack.
        assert!(m.positions.iter().all(|p| *p < MAX_HAYSTACK));
    }

    #[test]
    fn no_panic_on_pathological_input() {
        let long = "a".repeat(2000);
        assert!(match_score("aaaa", &long).is_some());
        assert!(match_score(&long, "a").is_none());
        assert!(match_score("", "").is_some());
        assert!(match_score("a", "").is_none());
    }
}
