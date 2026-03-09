use std::collections::HashMap;

/// OCR confusion pairs: (wrong char, correct char)
/// When OCR produces one character, try substituting the other.
const OCR_CONFUSIONS: &[(&str, &str)] = &[
    ("\u{8332}", "\u{5179}"), // 茲 → 兹
    ("\u{5179}", "\u{8332}"), // 兹 → 茲
];

/// Fuzzy match OCR text against a name→key map.
///
/// Matching strategy (in order):
/// 1. OCR confusion substitution → exact match
/// 2. Exact match on cleaned text
/// 3. Substring match (longest match wins, both directions)
/// 4. Levenshtein distance fallback (threshold: 30% of name length)
///
/// Port of `fuzzyMatchMap()` from GOODScanner/lib/constants.js
pub fn fuzzy_match_map(text: &str, map: &HashMap<String, String>) -> Option<String> {
    if text.is_empty() {
        return None;
    }

    // Clean OCR text: keep CJK, alphanumeric, and 「」
    let cleaned: String = text
        .chars()
        .filter(|c| {
            matches!(*c, '\u{4E00}'..='\u{9FFF}' | '\u{300C}' | '\u{300D}' | 'a'..='z' | 'A'..='Z' | '0'..='9')
        })
        .collect();
    let cleaned = cleaned.trim().to_string();
    if cleaned.is_empty() {
        return None;
    }

    // Try OCR confusion substitutions
    for &(from, to) in OCR_CONFUSIONS {
        if cleaned.contains(from) {
            let alt = cleaned.replace(from, to);
            if let Some(val) = map.get(&alt) {
                return Some(val.clone());
            }
        }
    }

    // Exact match
    if let Some(val) = map.get(&cleaned) {
        return Some(val.clone());
    }

    // Substring match (longest match wins)
    let mut best_match: Option<String> = None;
    let mut best_len: usize = 0;

    for (cn, val) in map.iter() {
        // cleaned contains the map key
        if cleaned.contains(cn.as_str()) && cn.len() > best_len {
            best_match = Some(val.clone());
            best_len = cn.len();
        }
        // map key contains cleaned
        if cn.contains(cleaned.as_str()) && cleaned.len() > best_len {
            best_match = Some(val.clone());
            best_len = cleaned.len();
        }
    }
    if best_match.is_some() {
        return best_match;
    }

    // Levenshtein distance fallback — character-level comparison for CJK
    let cleaned_chars: Vec<char> = cleaned.chars().collect();
    let mut min_dist = usize::MAX;
    for (cn, val) in map.iter() {
        let cn_chars: Vec<char> = cn.chars().collect();
        let dist = edit_distance_chars(&cleaned_chars, &cn_chars);
        // 30% threshold, min 1 for short strings
        let threshold = std::cmp::max(1, cn_chars.len() * 3 / 10);
        if dist < min_dist && dist <= threshold {
            min_dist = dist;
            best_match = Some(val.clone());
        }
    }

    best_match
}

/// Character-level Levenshtein distance (important for CJK where
/// byte-level distance gives misleading results).
fn edit_distance_chars(a: &[char], b: &[char]) -> usize {
    let m = a.len();
    let n = b.len();
    let mut dp = vec![vec![0usize; n + 1]; m + 1];
    for i in 0..=m { dp[i][0] = i; }
    for j in 0..=n { dp[0][j] = j; }
    for i in 1..=m {
        for j in 1..=n {
            let cost = if a[i - 1] == b[j - 1] { 0 } else { 1 };
            dp[i][j] = (dp[i - 1][j] + 1)
                .min(dp[i][j - 1] + 1)
                .min(dp[i - 1][j - 1] + cost);
        }
    }
    dp[m][n]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_exact_match() {
        let mut map = HashMap::new();
        map.insert("神里绫华".to_string(), "KamisatoAyaka".to_string());
        assert_eq!(
            fuzzy_match_map("神里绫华", &map),
            Some("KamisatoAyaka".to_string())
        );
    }

    #[test]
    fn test_substring_match() {
        let mut map = HashMap::new();
        map.insert("神里绫华".to_string(), "KamisatoAyaka".to_string());
        // OCR might produce extra noise characters around the name
        assert_eq!(
            fuzzy_match_map("·神里绫华", &map),
            Some("KamisatoAyaka".to_string())
        );
    }

    #[test]
    fn test_no_match() {
        let map = HashMap::new();
        assert_eq!(fuzzy_match_map("随机文本", &map), None);
    }

    #[test]
    fn test_empty_input() {
        let map = HashMap::new();
        assert_eq!(fuzzy_match_map("", &map), None);
    }
}
