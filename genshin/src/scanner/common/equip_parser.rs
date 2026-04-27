use std::collections::HashMap;
use super::fuzzy_match::fuzzy_match_map;

/// Parse equipped character from equip text.
///
/// The OCR region captures text like "CharName已装备" with possible noise
/// prefix chars from card decorations (c, Y, ca, emojis, etc).
/// Also handles truncated "已装" when the region clips the right side.
pub fn parse_equip_location(text: &str, char_map: &HashMap<String, String>) -> String {
    // Check for "已装备" or truncated "已装"
    let equip_marker = if text.contains("\u{5DF2}\u{88C5}\u{5907}") {
        Some("\u{5DF2}\u{88C5}\u{5907}") // 已装备
    } else if text.contains("\u{5DF2}\u{88C5}") {
        Some("\u{5DF2}\u{88C5}") // 已装 (truncated)
    } else {
        None
    };

    if let Some(marker) = equip_marker {
        return parse_equip_owner_name(text.replace(marker, ""), char_map);
    }
    String::new()
}

/// Parse an equipped owner name from a selection-view owner label.
///
/// Unlike `parse_equip_location`, this accepts OCR text without the explicit
/// "已装备" marker because the manager only needs to know whether the selected
/// artifact is owned by the currently processed character.
pub fn parse_equip_owner(text: &str, char_map: &HashMap<String, String>) -> String {
    parse_equip_owner_name(strip_equip_markers(text), char_map)
}

fn strip_equip_markers(text: &str) -> String {
    text.replace("\u{5DF2}\u{88C5}\u{5907}", "") // 已装备
        .replace("\u{5DF2}\u{88C5}", "") // 已装
        .replace("\u{88C5}\u{5907}", "") // 装备
        .replace(['\u{5907}', '\u{5DF2}'], "")
}

fn parse_equip_owner_name(text: String, char_map: &HashMap<String, String>) -> String {
    let char_name = text
        .replace([':', '\u{FF1A}', ' ', '\n', '\r', '\t'], "")
        .trim()
        .to_string();

    // Strip leading ASCII noise (c, Y, n, etc.) and emojis from OCR.
    let cleaned: String = char_name
        .trim_start_matches(|c: char| c.is_ascii() || !c.is_alphanumeric())
        .to_string();

    for name in [&cleaned, &char_name] {
        if !name.is_empty() {
            if let Some(key) = fuzzy_match_map(name, char_map) {
                return key;
            }
        }
    }

    String::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn char_map() -> HashMap<String, String> {
        HashMap::from([
            ("叶洛亚".to_string(), "Aloy".to_string()),
            ("菈乌玛".to_string(), "Lauma".to_string()),
            ("胡桃".to_string(), "HuTao".to_string()),
        ])
    }

    #[test]
    fn parses_owner_with_equip_marker() {
        assert_eq!(parse_equip_owner("叶洛亚已装备", &char_map()), "Aloy");
    }

    #[test]
    fn parses_owner_without_equip_marker() {
        assert_eq!(parse_equip_owner("叶洛亚", &char_map()), "Aloy");
    }

    #[test]
    fn parses_owner_through_fuzzy_normalization() {
        assert_eq!(parse_equip_owner("拉鸟玛已装备", &char_map()), "Lauma");
    }

    #[test]
    fn strict_location_parser_still_requires_marker() {
        assert_eq!(parse_equip_location("胡桃", &char_map()), "");
        assert_eq!(parse_equip_location("胡桃已装备", &char_map()), "HuTao");
    }
}
