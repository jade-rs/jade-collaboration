//! Compact ephemeral cursors. Tombstones keep their place after deletion,
//! so both neighbouring characters may disappear without losing the boundary.
use super::{DocxModel, Id, StoryId};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TextCursor {
    story: StoryId,
    left: Option<Id>,
    right: Option<Id>,
}

impl DocxModel {
    /// Capture a UTF-8 boundary in a visible paragraph. No clock/IDs change.
    pub fn text_cursor(&self, paragraph: usize, offset: usize) -> Option<TextCursor> {
        let paragraphs = self.paragraphs();
        let p = paragraphs.get(paragraph)?;
        if !p.text.is_char_boundary(offset) {
            return None;
        }
        let split = p.text[..offset].chars().count();
        Some(TextCursor {
            story: p.story,
            left: split.checked_sub(1).map(|i| p.chars[i]),
            right: p.chars.get(split).copied().or(p.mark),
        })
    }

    /// Resolve in this replica's current view, including across deletions.
    /// Unknown IDs (a presence arriving before its edit) have no position yet.
    pub fn resolve_text_cursor(&self, cursor: &TextCursor) -> Option<(usize, usize)> {
        let flow = &self.editable_story(cursor.story).ok()?.flow;
        let left = match cursor.left {
            Some(id) => Some(flow.position(id)?),
            None => None,
        };
        let right = match cursor.right {
            Some(id) => Some(flow.position(id)?),
            None => None,
        };
        if matches!((left, right), (Some(l), Some(r)) if l >= r) {
            return None;
        }
        let boundary = right.or_else(|| left.map(|n| n + 1)).unwrap_or(flow.len());
        for (ordinal, p) in self.paragraphs().iter().enumerate() {
            if p.story != cursor.story {
                continue;
            }
            let end = match p.mark {
                Some(id) => flow.position(id)?,
                None => flow.len(),
            };
            if boundary > end {
                continue;
            }
            let count = p
                .chars
                .iter()
                .take_while(|id| flow.position(**id).is_some_and(|n| n < boundary))
                .count();
            let offset = p
                .text
                .char_indices()
                .nth(count)
                .map_or(p.text.len(), |(byte, _)| byte);
            return Some((ordinal, offset));
        }
        None
    }
}

#[cfg(test)]
#[path = "cursor_tests.rs"]
mod tests;
