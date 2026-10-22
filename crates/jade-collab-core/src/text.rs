//! Scalar validity for strings materialized as XML 1.0. This does not escape
//! XML or enforce field-specific rules. SpreadsheetML cell text has a separate
//! `_xHHHH_` encoding and must not be restricted by this predicate.
use std::fmt;

pub fn is_xml_10_character(ch: char) -> bool {
    matches!(ch, '\t' | '\n' | '\r' | ' '..='\u{d7ff}' | '\u{e000}'..='\u{fffd}' | '\u{10000}'..='\u{10ffff}')
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InvalidXmlCharacter {
    pub byte_offset: usize,
    pub codepoint: u32,
}
impl fmt::Display for InvalidXmlCharacter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "U+{:04X} at UTF-8 byte {} is not an XML 1.0 character",
            self.codepoint, self.byte_offset
        )
    }
}
impl std::error::Error for InvalidXmlCharacter {}

pub fn check_xml_10(text: &str) -> Result<(), InvalidXmlCharacter> {
    match text
        .char_indices()
        .find(|(_, ch)| !is_xml_10_character(*ch))
    {
        Some((byte_offset, ch)) => Err(InvalidXmlCharacter {
            byte_offset,
            codepoint: ch as u32,
        }),
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    proptest! {
        #[test]
        fn scalar_and_first_error_match_numeric_xml_intervals(chars in prop::collection::vec(any::<char>(),0..90)) {
            let reference=|c:char| {
                let n=c as u32;
                [9,10,13].contains(&n)||[(32,0xd7ff),(0xe000,0xfffd),(0x10000,0x10ffff)].iter().any(|&(lo,hi)|lo<=n&&n<=hi)
            };
            let text: String=chars.iter().collect();
            for &ch in &chars {prop_assert_eq!(is_xml_10_character(ch),reference(ch));}
            let expected=text.char_indices().find(|&(_,c)|!reference(c)).map(|(byte_offset,c)| InvalidXmlCharacter {byte_offset,codepoint:c as u32});
            prop_assert_eq!(check_xml_10(&text).err(),expected);
        }
    }
    #[test]
    fn xml_boundaries_include_tabs_newlines_carriage_returns_and_supplementary_planes() {
        for n in [0, 1, 8, 11, 12, 14, 31, 0xfffe, 0xffff] {
            assert!(!is_xml_10_character(char::from_u32(n).unwrap()));
        }
        for n in [
            9, 10, 13, 32, 0x7f, 0x85, 0xd7ff, 0xe000, 0xfdd0, 0xfffd, 0x10000, 0x1fffe, 0x10ffff,
        ] {
            assert!(is_xml_10_character(char::from_u32(n).unwrap()));
        }
        assert_eq!(check_xml_10("日本🙂\0").unwrap_err().byte_offset, 10);
    }
}
