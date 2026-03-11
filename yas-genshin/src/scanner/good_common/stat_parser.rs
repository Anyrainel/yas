use lazy_static::lazy_static;
use regex::Regex;

/// Result of parsing a stat from OCR text
#[derive(Debug, Clone)]
pub struct ParsedStat {
    pub key: String,
    pub value: f64,
    pub inactive: bool,
}

/// Stat key entry: either a simple string key or flat/percent variants
enum StatKeyEntry {
    Simple(&'static str),
    FlatPercent { flat: &'static str, percent: &'static str },
}

/// Chinese stat name → GOOD stat key mapping
/// For HP/ATK/DEF, flat vs percent is determined by presence of "%" in OCR text
const STAT_KEY_ENTRIES: &[(&str, StatKeyEntry)] = &[
    ("\u{751F}\u{547D}\u{503C}", StatKeyEntry::FlatPercent { flat: "hp", percent: "hp_" }),           // 生命值
    ("\u{653B}\u{51FB}\u{529B}", StatKeyEntry::FlatPercent { flat: "atk", percent: "atk_" }),          // 攻击力
    ("\u{9632}\u{5FA1}\u{529B}", StatKeyEntry::FlatPercent { flat: "def", percent: "def_" }),          // 防御力
    ("\u{5143}\u{7D20}\u{7CBE}\u{901A}", StatKeyEntry::Simple("eleMas")),                              // 元素精通
    ("\u{5143}\u{7D20}\u{5145}\u{80FD}\u{6548}\u{7387}", StatKeyEntry::Simple("enerRech_")),           // 元素充能效率
    ("\u{66B4}\u{51FB}\u{7387}", StatKeyEntry::Simple("critRate_")),                                    // 暴击率
    ("\u{66B4}\u{51FB}\u{4F24}\u{5BB3}", StatKeyEntry::Simple("critDMG_")),                            // 暴击伤害
    ("\u{6CBB}\u{7597}\u{52A0}\u{6210}", StatKeyEntry::Simple("heal_")),                               // 治疗加成
    ("\u{7269}\u{7406}\u{4F24}\u{5BB3}\u{52A0}\u{6210}", StatKeyEntry::Simple("physical_dmg_")),       // 物理伤害加成
    ("\u{706B}\u{5143}\u{7D20}\u{4F24}\u{5BB3}\u{52A0}\u{6210}", StatKeyEntry::Simple("pyro_dmg_")),   // 火元素伤害加成
    ("\u{96F7}\u{5143}\u{7D20}\u{4F24}\u{5BB3}\u{52A0}\u{6210}", StatKeyEntry::Simple("electro_dmg_")),// 雷元素伤害加成
    ("\u{6C34}\u{5143}\u{7D20}\u{4F24}\u{5BB3}\u{52A0}\u{6210}", StatKeyEntry::Simple("hydro_dmg_")),  // 水元素伤害加成
    ("\u{8349}\u{5143}\u{7D20}\u{4F24}\u{5BB3}\u{52A0}\u{6210}", StatKeyEntry::Simple("dendro_dmg_")), // 草元素伤害加成
    ("\u{98CE}\u{5143}\u{7D20}\u{4F24}\u{5BB3}\u{52A0}\u{6210}", StatKeyEntry::Simple("anemo_dmg_")),  // 风元素伤害加成
    ("\u{5CA9}\u{5143}\u{7D20}\u{4F24}\u{5BB3}\u{52A0}\u{6210}", StatKeyEntry::Simple("geo_dmg_")),    // 岩元素伤害加成
    ("\u{51B0}\u{5143}\u{7D20}\u{4F24}\u{5BB3}\u{52A0}\u{6210}", StatKeyEntry::Simple("cryo_dmg_")),   // 冰元素伤害加成
];

lazy_static! {
    /// Stat names ordered by length descending for greedy matching
    static ref STAT_NAMES: Vec<&'static str> = {
        let mut names: Vec<&str> = STAT_KEY_ENTRIES.iter().map(|(name, _)| *name).collect();
        names.sort_by(|a, b| b.chars().count().cmp(&a.chars().count()));
        names
    };

    /// Matches numbers like "5.8", "15", ".7" (missing integer part → 0.7), "5."
    static ref NUM_REGEX: Regex = Regex::new(r"[+\s]?([\d]+\.?\d*|\.\d+)").unwrap();

    /// Match digit-space-dot-digit or digit-dot-space-digit patterns.
    /// Used to collapse "4 .7" or "4. 7" into "4.7".
    static ref SPACE_DOT_REGEX: Regex = Regex::new(r"(?P<d1>\d)\s*\.\s*(?P<d2>\d)").unwrap();

    /// Match OCR-corrupted digits after decimal point: "6.e%" or "10.a%" → fix the letter to digit.
    /// Also handles missing digit: "6.%" → keep "6." so it can be captured as "6" (we can't recover the lost digit).
    static ref DOT_LETTER_REGEX: Regex = Regex::new(r"(\d+\.)([a-zA-Z])(\d*)").unwrap();
}

/// Strip spaces between CJK characters.
/// OCR models often insert spaces within Chinese text (e.g., "暴击 伤害" instead of "暴击伤害").
fn strip_cjk_spaces(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    if chars.len() < 3 {
        return text.to_string();
    }
    let mut result = String::with_capacity(text.len());
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == ' ' {
            // Check if surrounded by CJK characters
            let prev_cjk = i > 0 && is_cjk(chars[i - 1]);
            let next_cjk = i + 1 < chars.len() && is_cjk(chars[i + 1]);
            if prev_cjk && next_cjk {
                // Skip the space
                i += 1;
                continue;
            }
        }
        result.push(chars[i]);
        i += 1;
    }
    result
}

fn is_cjk(c: char) -> bool {
    c >= '\u{4E00}' && c <= '\u{9FFF}'
}

/// Pre-clean OCR text: normalize decimal separators, strip bullets, fix common errors.
fn clean_ocr_text(text: &str) -> String {
    let text = text
        .replace(',', "")
        .replace('\u{FF0E}', ".")
        .replace('\u{3002}', ".")
        .replace('\u{00B7}', ".")
        .replace('\u{2022}', "")
        .replace('\u{2027}', "");
    // Strip spaces between CJK characters — OCR often inserts spurious spaces
    // that break stat name matching (e.g., "暴击 伤害" → "暴击伤害")
    let text = strip_cjk_spaces(&text);
    let text = SPACE_DOT_REGEX.replace_all(&text, "${d1}.${d2}");
    DOT_LETTER_REGEX.replace_all(&text, |caps: &regex::Captures| {
        let prefix = &caps[1];
        let letter = &caps[2];
        let suffix = &caps[3];
        let fixed = match letter {
            "e" | "E" => "6",
            "a" | "A" => "4",
            "o" | "O" => "0",
            "n" => "0",
            "l" | "I" | "i" => "1",
            "s" | "S" => "5",
            "b" | "B" => "8",
            "t" | "T" => "7",
            "g" | "q" => "9",
            "z" | "Z" => "2",
            _ => letter,
        };
        format!("{}{}{}", prefix, fixed, suffix)
    }).into_owned()
}

/// Try to extract a numeric value from text, with OCR digit fixup fallback.
fn try_extract_value(text: &str) -> Option<f64> {
    if let Some(caps) = NUM_REGEX.captures(text) {
        if let Ok(v) = caps[1].parse::<f64>() {
            return Some(v);
        }
    }
    let fixed = fix_ocr_digits(text);
    NUM_REGEX.captures(&fixed).and_then(|c| c[1].parse::<f64>().ok())
}

/// Artifact slot mapping: Chinese slot name → GOOD slot key
pub const SLOT_KEY_MAP: &[(&str, &str)] = &[
    ("\u{751F}\u{4E4B}\u{82B1}", "flower"),  // 生之花
    ("\u{6B7B}\u{4E4B}\u{7FBD}", "plume"),   // 死之羽
    ("\u{65F6}\u{4E4B}\u{6C99}", "sands"),   // 时之沙
    ("\u{7A7A}\u{4E4B}\u{676F}", "goblet"),  // 空之杯
    ("\u{7406}\u{4E4B}\u{51A0}", "circlet"), // 理之冠
];

/// Parse a stat from OCR text.
///
/// Returns the GOOD stat key, numeric value, and whether the stat is inactive (待激活).
///
/// Port of `parseStatFromText()` from GOODScanner/lib/constants.js
pub fn parse_stat_from_text(text: &str) -> Option<ParsedStat> {
    if text.is_empty() {
        return None;
    }

    let text = clean_ocr_text(text);
    let text = text.trim();

    // Try direct match first
    if let Some(result) = parse_stat_inner(text) {
        return Some(result);
    }

    // Fuzzy retry: apply common OCR character substitutions
    let fuzzy_text = apply_fuzzy_substitutions(text);
    if fuzzy_text != text {
        if let Some(result) = parse_stat_inner(&fuzzy_text) {
            return Some(result);
        }
    }

    // Last resort: match stat name with first character dropped.
    // The stat icon at the left edge of substat lines sometimes causes OCR
    // to replace the first character (e.g., "晨击伤害" instead of "暴击伤害").
    // Try matching on suffix (all but first char of stat name).
    if let Some(result) = parse_stat_suffix(text) {
        return Some(result);
    }

    None
}

/// Try matching stat name by suffix (drop first character of each stat name).
/// This handles cases where the OCR corrupts the first character due to a
/// stat icon or bullet point interfering.
fn parse_stat_suffix(text: &str) -> Option<ParsedStat> {
    for &(stat_name, ref entry) in STAT_KEY_ENTRIES {
        let suffix: String = stat_name.chars().skip(1).collect();
        if suffix.len() < 2 || !text.contains(&suffix) {
            continue;
        }

        let is_inactive = text.contains("\u{5F85}\u{6FC0}\u{6D3B}"); // 待激活
        let has_percent = text.contains('%');
        let value = try_extract_value(text).unwrap_or(0.0);

        return Some(ParsedStat {
            key: resolve_stat_key(entry, has_percent),
            value,
            inactive: is_inactive,
        });
    }

    None
}

/// Fix common OCR digit errors in the value portion of a stat string.
/// e.g., "n" → "0", "a" → "4", "l" → "1", "o"/"O" → "0"
fn fix_ocr_digits(text: &str) -> String {
    text.chars()
        .map(|c| match c {
            'n' => '0',
            'a' => '4',
            'l' | 'I' => '1',
            'o' | 'O' => '0',
            's' | 'S' => '5',
            _ => c,
        })
        .collect()
}

/// Find a stat name in text and return its GOOD key, or None.
fn find_stat_key_in_text(text: &str, has_percent: bool) -> Option<String> {
    for &stat_name in STAT_NAMES.iter() {
        if text.contains(stat_name) {
            return STAT_KEY_ENTRIES
                .iter()
                .find(|(name, _)| *name == stat_name)
                .map(|(_, entry)| resolve_stat_key(entry, has_percent));
        }
    }
    None
}

/// Resolve a StatKeyEntry to a GOOD key string based on whether "%" was present.
fn resolve_stat_key(entry: &StatKeyEntry, has_percent: bool) -> String {
    match entry {
        StatKeyEntry::Simple(k) => k.to_string(),
        StatKeyEntry::FlatPercent { flat, percent } => {
            if has_percent { percent.to_string() } else { flat.to_string() }
        }
    }
}

/// Apply common OCR character substitutions for Chinese stat names.
///   "力" misread as "b"/"B" (only after non-ASCII)
///   "生" misread as "E"/"三"/"t" (at position 0)
///   "元" misread as "亡"
fn apply_fuzzy_substitutions(text: &str) -> String {
    let mut result = String::with_capacity(text.len());
    let chars: Vec<char> = text.chars().collect();
    for (i, &ch) in chars.iter().enumerate() {
        if (ch == 'b' || ch == 'B') && i > 0 && !chars[i - 1].is_ascii() {
            result.push('\u{529B}'); // 力
        } else if (ch == 'E' || ch == '\u{4E09}' || ch == 't') && i == 0 {
            result.push('\u{751F}'); // 生
        } else if ch == '\u{4EA1}' {
            result.push('\u{5143}'); // 元
        } else {
            result.push(ch);
        }
    }
    result
}

fn parse_stat_inner(text: &str) -> Option<ParsedStat> {
    let has_percent = text.contains('%');
    let key = find_stat_key_in_text(text, has_percent)?;

    Some(ParsedStat {
        key,
        value: try_extract_value(text).unwrap_or(0.0),
        inactive: text.contains("\u{5F85}\u{6FC0}\u{6D3B}"), // 待激活
    })
}

/// Try to extract just the stat key from OCR text, ignoring the value.
///
/// Returns `(key, has_percent, is_inactive)` if a stat name is found.
/// This is used as a rescue mechanism when `parse_stat_from_text` fails
/// because the value portion is garbled — we can still identify the stat type.
pub fn try_extract_stat_key(text: &str) -> Option<(String, bool, bool)> {
    let text = clean_ocr_text(text);
    let text = text.trim();
    if text.is_empty() {
        return None;
    }

    let is_inactive = text.contains("\u{5F85}\u{6FC0}\u{6D3B}"); // 待激活
    let has_percent = text.contains('%');

    // Try finding a stat name in text (direct, then fuzzy, then suffix)
    if let Some(key) = find_stat_key_in_text(text, has_percent) {
        return Some((key, has_percent, is_inactive));
    }

    let fuzzy_text = apply_fuzzy_substitutions(text);
    if fuzzy_text != text {
        if let Some(key) = find_stat_key_in_text(&fuzzy_text, has_percent) {
            return Some((key, has_percent, is_inactive));
        }
    }

    // Suffix match (drop first char of each stat name)
    for &(stat_name, ref entry) in STAT_KEY_ENTRIES {
        let suffix: String = stat_name.chars().skip(1).collect();
        if suffix.len() < 2 || !text.contains(&suffix) {
            continue;
        }
        return Some((resolve_stat_key(entry, has_percent), has_percent, is_inactive));
    }

    None
}

/// Extract a numeric value from text, applying OCR digit fixups.
/// Used for retry OCR on just the number portion of a substat line.
pub fn extract_number(text: &str) -> Option<f64> {
    let cleaned = clean_ocr_text(text);
    try_extract_value(cleaned.trim())
}

/// Convert a flat stat key to its percent variant for main stat context.
///
/// On sands/goblet/circlet, HP/ATK/DEF main stats are always percent.
/// The OCR region for main stat only captures the stat name (not the value
/// line with "%"), so parse_stat_from_text defaults to flat. This function
/// corrects that.
pub fn main_stat_key_fixup(key: &str) -> String {
    match key {
        "hp" => "hp_".to_string(),
        "atk" => "atk_".to_string(),
        "def" => "def_".to_string(),
        other => other.to_string(),
    }
}

/// Match OCR text against the slot key map.
///
/// Returns the GOOD slot key (e.g., "flower", "plume") or None.
pub fn match_slot_key(text: &str) -> Option<&'static str> {
    for &(cn_name, key) in SLOT_KEY_MAP {
        if text.contains(cn_name) {
            return Some(key);
        }
    }
    None
}

/// Derive ascension phase from level and ascended status.
///
/// Ascension boundaries: 20→0, 40→1, 50→2, 60→3, 70→4, 80→5, 90→6
/// When level equals a boundary, `ascended` determines if the character/weapon
/// has been ascended past that boundary.
///
/// Port of `levelToAscension()` from GOODScanner/lib/constants.js
pub fn level_to_ascension(level: i32, ascended: bool) -> i32 {
    let thresholds = [20, 40, 50, 60, 70, 80];
    for (i, &threshold) in thresholds.iter().enumerate() {
        if level < threshold {
            return i as i32;
        }
        if level == threshold {
            return if ascended { i as i32 + 1 } else { i as i32 };
        }
    }
    6
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_level_to_ascension() {
        assert_eq!(level_to_ascension(1, false), 0);
        assert_eq!(level_to_ascension(20, false), 0);
        assert_eq!(level_to_ascension(20, true), 1);
        assert_eq!(level_to_ascension(40, true), 2);
        assert_eq!(level_to_ascension(50, false), 2);
        assert_eq!(level_to_ascension(80, true), 6);
        assert_eq!(level_to_ascension(90, false), 6);
    }

    #[test]
    fn test_parse_stat_percent() {
        let result = parse_stat_from_text("\u{653B}\u{51FB}\u{529B}+46.6%"); // 攻击力+46.6%
        assert!(result.is_some());
        let stat = result.unwrap();
        assert_eq!(stat.key, "atk_");
        assert!((stat.value - 46.6).abs() < 0.01);
        assert!(!stat.inactive);
    }

    #[test]
    fn test_parse_stat_flat() {
        let result = parse_stat_from_text("\u{751F}\u{547D}\u{503C}+4780"); // 生命值+4780
        assert!(result.is_some());
        let stat = result.unwrap();
        assert_eq!(stat.key, "hp");
        assert!((stat.value - 4780.0).abs() < 0.01);
    }

    #[test]
    fn test_parse_ocr_fuzzy() {
        // "E命值+209" → 生命值 (HP flat) via first-char substitution
        let r = parse_stat_from_text("E\u{547D}\u{503C}+209");
        assert!(r.is_some());
        let s = r.unwrap();
        assert_eq!(s.key, "hp");
        assert!((s.value - 209.0).abs() < 0.01);

        // "三命值+269" → 生命值 via suffix matching
        let r = parse_stat_from_text("\u{4E09}\u{547D}\u{503C}+269");
        assert!(r.is_some());
        assert_eq!(r.unwrap().key, "hp");

        // "攻击b+4n%" → 攻击力+40% via b→力 and n→0
        let r = parse_stat_from_text("\u{653B}\u{51FB}b+4n%");
        assert!(r.is_some());
        let s = r.unwrap();
        assert_eq!(s.key, "atk_");

        // "方御力+35" → 防御力 (DEF flat) via suffix matching
        let r = parse_stat_from_text("\u{65B9}\u{5FA1}\u{529B}+35");
        assert!(r.is_some());
        let s = r.unwrap();
        assert_eq!(s.key, "def");
        assert!((s.value - 35.0).abs() < 0.01);

        // "亡素精通+68" → 元素精通 via 亡→元
        let r = parse_stat_from_text("\u{4EA1}\u{7D20}\u{7CBE}\u{901A}+68");
        assert!(r.is_some());
        let s = r.unwrap();
        assert_eq!(s.key, "eleMas");
        assert!((s.value - 68.0).abs() < 0.01);
    }

    #[test]
    fn test_parse_inactive() {
        // "生命值+4.1%（待激活）"
        let r = parse_stat_from_text("\u{751F}\u{547D}\u{503C}+4.1%\u{FF08}\u{5F85}\u{6FC0}\u{6D3B}\u{FF09}");
        assert!(r.is_some());
        let s = r.unwrap();
        assert_eq!(s.key, "hp_");
        assert!(s.inactive);
        assert!((s.value - 4.1).abs() < 0.01); // inactive keeps real value
    }

    #[test]
    fn test_match_slot_key() {
        assert_eq!(match_slot_key("\u{751F}\u{4E4B}\u{82B1}"), Some("flower")); // 生之花
        assert_eq!(match_slot_key("\u{7406}\u{4E4B}\u{51A0}"), Some("circlet")); // 理之冠
        assert_eq!(match_slot_key("random"), None);
    }

    #[test]
    fn test_cjk_space_stripping() {
        // "暴击 伤害+7.8%" → "暴击伤害+7.8%" (space between CJK removed)
        let r = parse_stat_from_text("\u{66B4}\u{51FB} \u{4F24}\u{5BB3}+7.8%");
        assert!(r.is_some());
        let s = r.unwrap();
        assert_eq!(s.key, "critDMG_");
        assert!((s.value - 7.8).abs() < 0.01);

        // "元素 充能 效率+6.5%" → "元素充能效率+6.5%"
        let r = parse_stat_from_text("\u{5143}\u{7D20} \u{5145}\u{80FD} \u{6548}\u{7387}+6.5%");
        assert!(r.is_some());
        let s = r.unwrap();
        assert_eq!(s.key, "enerRech_");
        assert!((s.value - 6.5).abs() < 0.01);

        // Space between CJK and ASCII should NOT be stripped: "攻击力 +19"
        let r = parse_stat_from_text("\u{653B}\u{51FB}\u{529B} +19");
        assert!(r.is_some());
        assert_eq!(r.unwrap().key, "atk");
    }

    #[test]
    fn test_try_extract_stat_key() {
        // Normal text
        let r = try_extract_stat_key("\u{653B}\u{51FB}\u{529B}+19"); // 攻击力+19
        assert!(r.is_some());
        let (key, has_pct, inactive) = r.unwrap();
        assert_eq!(key, "atk");
        assert!(!has_pct);
        assert!(!inactive);

        // Percent
        let r = try_extract_stat_key("\u{653B}\u{51FB}\u{529B}+4.7%"); // 攻击力+4.7%
        assert!(r.is_some());
        assert_eq!(r.unwrap().0, "atk_");

        // Garbled number but key still identifiable
        let r = try_extract_stat_key("\u{66B4}\u{51FB}\u{4F24}\u{5BB3}#@!"); // 暴击伤害#@!
        assert!(r.is_some());
        assert_eq!(r.unwrap().0, "critDMG_");

        // Inactive
        let r = try_extract_stat_key("\u{5143}\u{7D20}\u{5145}\u{80FD}\u{6548}\u{7387}+6.5%\u{FF08}\u{5F85}\u{6FC0}\u{6D3B}\u{FF09}");
        assert!(r.is_some());
        let (key, _, inactive) = r.unwrap();
        assert_eq!(key, "enerRech_");
        assert!(inactive);
    }
}
