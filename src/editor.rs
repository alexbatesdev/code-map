use std::{borrow::Cow, collections::VecDeque, ops::Range};

pub const MAX_INPUT_BYTES: usize = 16 * 1024;
const UNDO_LIMIT: usize = 32;

#[derive(Clone)]
struct Snapshot {
    text: String,
    cursor: usize,
    anchor: usize,
}

struct Composition {
    text: String,
    replace: Range<usize>,
    selection: Range<usize>,
}

#[derive(Default)]
pub struct Editor {
    pub text: String,
    pub scroll_x: f64,
    cursor: usize,
    anchor: usize,
    composition: Option<Composition>,
    undo: VecDeque<Snapshot>,
}

impl Editor {
    pub fn new(text: impl Into<String>) -> Result<Self, String> {
        let text = single_line(&text.into());
        check_size(text.len())?;
        let cursor = text.len();
        Ok(Self {
            text,
            scroll_x: 0.0,
            cursor,
            anchor: cursor,
            composition: None,
            undo: VecDeque::new(),
        })
    }

    pub fn set_text(&mut self, text: &str) -> Result<bool, String> {
        let text = single_line(text);
        check_size(text.len())?;
        let changed = self.text != text;
        self.text = text;
        self.cursor = self.text.len();
        self.anchor = self.cursor;
        self.scroll_x = 0.0;
        self.composition = None;
        self.undo.clear();
        Ok(changed)
    }

    pub fn select_all(&mut self) {
        self.cancel_composition();
        self.anchor = 0;
        self.cursor = self.text.len();
    }

    pub fn selection_bytes(&self) -> Range<usize> {
        let cursor = floor_boundary(&self.text, self.cursor);
        let anchor = floor_boundary(&self.text, self.anchor);
        anchor.min(cursor)..anchor.max(cursor)
    }

    pub fn selected_text(&self) -> &str {
        &self.text[self.selection_bytes()]
    }

    pub fn native_selection(&self) -> (usize, usize) {
        let selection = self.selection_bytes();
        (
            self.text[..selection.start].encode_utf16().count(),
            self.text[selection].encode_utf16().count(),
        )
    }

    pub fn click_byte(&mut self, byte: usize, extend: bool) {
        self.cancel_composition();
        self.cursor = floor_boundary(&self.text, byte);
        if !extend {
            self.anchor = self.cursor;
        }
    }

    pub fn move_left(&mut self, extend: bool) {
        let selection = self.selection_bytes();
        let next = if !extend && !selection.is_empty() {
            selection.start
        } else {
            let cursor = floor_boundary(&self.text, self.cursor);
            self.text[..cursor].char_indices().next_back().map_or(0, |(i, _)| i)
        };
        self.click_byte(next, extend);
    }

    pub fn move_right(&mut self, extend: bool) {
        let selection = self.selection_bytes();
        let next = if !extend && !selection.is_empty() {
            selection.end
        } else {
            let cursor = floor_boundary(&self.text, self.cursor);
            cursor + self.text[cursor..].chars().next().map_or(0, char::len_utf8)
        };
        self.click_byte(next, extend);
    }

    pub fn home(&mut self, extend: bool) {
        self.click_byte(0, extend);
    }

    pub fn end(&mut self, extend: bool) {
        self.click_byte(self.text.len(), extend);
    }

    pub fn move_home(&mut self, extend: bool) {
        self.home(extend);
    }

    pub fn move_end(&mut self, extend: bool) {
        self.end(extend);
    }

    pub fn backspace(&mut self) -> bool {
        self.cancel_composition();
        let mut range = self.selection_bytes();
        if range.is_empty() {
            range.start = self.text[..range.start]
                .char_indices().next_back().map_or(0, |(i, _)| i);
        }
        self.replace("", range).unwrap_or(false)
    }

    pub fn delete(&mut self) -> bool {
        self.cancel_composition();
        let mut range = self.selection_bytes();
        if range.is_empty() {
            range.end += self.text[range.end..].chars().next().map_or(0, char::len_utf8);
        }
        self.replace("", range).unwrap_or(false)
    }

    pub fn undo(&mut self) -> bool {
        self.cancel_composition();
        let Some(previous) = self.undo.pop_back() else { return false };
        self.text = previous.text;
        self.cursor = previous.cursor;
        self.anchor = previous.anchor;
        true
    }

    pub fn insert(&mut self, text: &str) -> Result<bool, String> {
        let range = self.composition.as_ref()
            .map_or_else(|| self.selection_bytes(), |c| c.replace.clone());
        self.replace(text, range)
    }

    pub fn replace_native(
        &mut self, text: &str, start_utf16: usize, len_utf16: usize,
    ) -> Result<bool, String> {
        let range = self.native_range(start_utf16, len_utf16)?;
        self.replace(text, range)
    }

    pub fn set_composition(
        &mut self, text: &str, selection_start: usize, selection_len: usize,
        replace_start: usize, replace_len: usize,
    ) -> Result<bool, String> {
        if text.is_empty() {
            let changed = self.is_composing();
            self.cancel_composition();
            return Ok(changed);
        }
        let text = single_line(text);
        let selection = utf16_range(&text, selection_start, selection_len)?;
        let replace = self.native_range(replace_start, replace_len)?;
        check_size(self.text.len() - replace.len() + text.len())?;
        self.composition = Some(Composition { text, replace, selection });
        Ok(true)
    }

    pub fn display_text(&self) -> Cow<'_, str> {
        let Some(c) = &self.composition else { return Cow::Borrowed(&self.text) };
        let mut text = String::with_capacity(self.text.len() - c.replace.len() + c.text.len());
        text.push_str(&self.text[..c.replace.start]);
        text.push_str(&c.text);
        text.push_str(&self.text[c.replace.end..]);
        Cow::Owned(text)
    }

    pub fn display_caret_byte(&self) -> usize {
        self.composition.as_ref().map_or_else(
            || floor_boundary(&self.text, self.cursor),
            |c| c.replace.start + c.selection.end,
        )
    }

    pub fn composition_bytes(&self) -> Option<Range<usize>> {
        self.composition.as_ref().map(|c| c.replace.start..c.replace.start + c.text.len())
    }

    pub fn commit_composition(&mut self) -> Result<bool, String> {
        let Some(c) = &self.composition else { return Ok(false) };
        let text = c.text.clone();
        let range = c.replace.clone();
        self.replace(&text, range)
    }

    pub fn cancel_composition(&mut self) {
        self.composition = None;
    }

    pub fn is_composing(&self) -> bool {
        self.composition.is_some()
    }

    fn native_range(&self, start: usize, len: usize) -> Result<Range<usize>, String> {
        if start == usize::MAX {
            Ok(self.composition.as_ref()
                .map_or_else(|| self.selection_bytes(), |c| c.replace.clone()))
        } else {
            utf16_range(&self.text, start, len)
        }
    }

    fn replace(&mut self, text: &str, range: Range<usize>) -> Result<bool, String> {
        let text = single_line(text);
        check_size(self.text.len() - range.len() + text.len())?;
        let changed = self.text[range.clone()] != text;
        if changed {
            if self.undo.len() == UNDO_LIMIT {
                self.undo.pop_front();
            }
            self.undo.push_back(Snapshot {
                text: self.text.clone(),
                cursor: floor_boundary(&self.text, self.cursor),
                anchor: floor_boundary(&self.text, self.anchor),
            });
            self.text.replace_range(range.clone(), &text);
        }
        self.cursor = range.start + text.len();
        self.anchor = self.cursor;
        self.composition = None;
        Ok(changed)
    }
}

fn single_line(text: &str) -> String {
    text.chars().map(|c| match c { '\n' | '\r' | '\t' => ' ', _ => c }).collect()
}

fn check_size(new_size: usize) -> Result<(), String> {
    if new_size > MAX_INPUT_BYTES {
        Err(format!("Input exceeds the {} KiB limit", MAX_INPUT_BYTES / 1024))
    } else {
        Ok(())
    }
}

fn floor_boundary(text: &str, byte: usize) -> usize {
    let mut byte = byte.min(text.len());
    while !text.is_char_boundary(byte) { byte -= 1; }
    byte
}

fn utf16_range(text: &str, start: usize, len: usize) -> Result<Range<usize>, String> {
    let end = start.checked_add(len).ok_or("UTF-16 range overflow")?;
    let byte_at = |target| {
        let mut position = 0;
        for (byte, character) in text.char_indices() {
            if position == target { return Some(byte); }
            position += character.len_utf16();
            if position > target { return None; }
        }
        (position == target).then_some(text.len())
    };
    match (byte_at(start), byte_at(end)) {
        (Some(start), Some(end)) => Ok(start..end),
        _ => Err("UTF-16 range is outside the text or splits a surrogate pair".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_ranges_replace_whole_unicode_characters() {
        let mut e = Editor::new("a😀éz").unwrap();
        e.click_byte(1, false);
        e.move_right(true);
        assert_eq!(e.native_selection(), (1, 2));
        assert_eq!(e.selected_text(), "😀");
        assert_eq!(e.replace_native("λ", 1, 2), Ok(true));
        assert_eq!(e.text, "aλéz");
        assert_eq!(e.native_selection(), (2, 0));
    }

    #[test]
    fn invalid_native_ranges_preserve_text_selection_and_preedit() {
        let mut e = Editor::new("a😀éz").unwrap();
        e.click_byte(1, false);
        e.move_right(true);
        e.set_composition("候", 1, 0, usize::MAX, 0).unwrap();
        for (start, len) in [(2, 0), (1, 1), (0, 100), (100, 0), (5, usize::MAX)] {
            assert!(e.replace_native("bad", start, len).is_err());
            assert_eq!(e.text, "a😀éz");
            assert_eq!(e.native_selection(), (1, 2));
            assert_eq!(e.display_text(), "a候éz");
        }
        assert!(e.set_composition("😀", 1, 0, 1, 2).is_err());
        assert_eq!(e.display_text(), "a候éz");
    }

    #[test]
    fn preedit_updates_and_native_commit_use_original_committed_range() {
        let mut e = Editor::new("a😀z").unwrap();
        e.set_composition("に", 1, 0, 1, 2).unwrap();
        assert_eq!(e.text, "a😀z");
        assert_eq!(e.display_text(), "aにz");
        assert_eq!(e.composition_bytes(), Some(1..4));
        assert_eq!(e.display_caret_byte(), 4);
        e.set_composition("日本", 1, 1, 1, 2).unwrap();
        assert_eq!(e.display_text(), "a日本z");
        assert_eq!(e.display_caret_byte(), 7);
        assert_eq!(e.text, "a😀z");
        e.replace_native("日本語", 1, 2).unwrap();
        assert_eq!(e.text, "a日本語z");
        assert!(!e.is_composing());
        assert!(e.undo());
        assert_eq!(e.text, "a😀z");
        assert!(!e.undo());
    }

    #[test]
    fn cancel_discards_preedit_and_blur_commit_is_undoable() {
        let mut e = Editor::new("abc").unwrap();
        e.click_byte(1, false);
        e.move_right(true);
        e.set_composition("候補", 2, 0, usize::MAX, 0).unwrap();
        e.cancel_composition();
        assert_eq!(e.display_text(), "abc");
        assert_eq!(e.selected_text(), "b");
        e.set_composition("候補", 2, 0, usize::MAX, 0).unwrap();
        e.set_composition("", 0, 0, usize::MAX, 0).unwrap();
        assert!(!e.is_composing());
        e.set_composition("候補", 2, 0, usize::MAX, 0).unwrap();
        assert_eq!(e.commit_composition(), Ok(true));
        assert_eq!(e.text, "a候補c");
        assert!(e.undo());
        assert_eq!(e.text, "abc");
        assert_eq!(e.selected_text(), "b");
        assert_eq!(e.display_caret_byte(), 2);
    }

    #[test]
    fn unicode_navigation_selection_deletion_and_undo_preserve_anchor() {
        let mut e = Editor::new("a😀éz").unwrap();
        e.home(false);
        e.move_right(false);
        e.move_right(true);
        e.move_right(true);
        assert_eq!(e.selected_text(), "😀é");
        assert!(e.backspace());
        assert_eq!(e.text, "az");
        assert!(e.undo());
        assert_eq!(e.selected_text(), "😀é");
        e.move_left(true);
        assert_eq!(e.selected_text(), "😀");
        e.move_right(false);
        assert!(e.delete());
        assert_eq!(e.text, "a😀z");
        e.click_byte(3, false);
        assert_eq!(e.display_caret_byte(), 1);
        assert!(e.backspace());
        assert_eq!(e.text, "😀z");
    }

    #[test]
    fn pasted_lines_and_tabs_become_spaces() {
        let mut e = Editor::new("left").unwrap();
        e.insert("\nright\tlast\r\n").unwrap();
        assert_eq!(e.text, "left right last  ");
        e.select_all();
        e.insert("é\n😀").unwrap();
        assert_eq!(e.text, "é 😀");
        assert_eq!(e.native_selection(), (4, 0));
    }

    #[test]
    fn size_limit_rejection_is_atomic_for_paste_composition_and_set_text() {
        let mut e = Editor::new("a".repeat(MAX_INPUT_BYTES - 4)).unwrap();
        e.insert("😀").unwrap();
        assert_eq!(e.text.len(), MAX_INPUT_BYTES);
        let old_text = e.text.clone();
        let old_selection = e.native_selection();
        assert!(e.insert("x").is_err());
        assert!(e.set_composition("x", 1, 0, usize::MAX, 0).is_err());
        assert!(e.set_text(&"x".repeat(MAX_INPUT_BYTES + 1)).is_err());
        assert_eq!(e.text, old_text);
        assert_eq!(e.native_selection(), old_selection);
        assert!(!e.is_composing());
        assert!(e.undo());
        assert_eq!(e.text.len(), MAX_INPUT_BYTES - 4);
    }

    #[test]
    fn initial_input_obeys_byte_limit_and_default_is_empty() {
        assert!(Editor::new("x".repeat(MAX_INPUT_BYTES + 1)).is_err());
        assert!(Editor::new("😀".repeat(MAX_INPUT_BYTES / 4 + 1)).is_err());
        assert_eq!(Editor::new("😀".repeat(MAX_INPUT_BYTES / 4)).unwrap().text.len(), MAX_INPUT_BYTES);
        let e = Editor::default();
        assert_eq!(e.text, "");
        assert_eq!(e.native_selection(), (0, 0));
        assert_eq!(e.display_caret_byte(), 0);
    }

    #[test]
    fn rejected_commit_preserves_active_composition() {
        let mut e = Editor::new("old").unwrap();
        e.select_all();
        e.set_composition("新", 1, 0, usize::MAX, 0).unwrap();
        assert!(e.replace_native(&"x".repeat(MAX_INPUT_BYTES + 1), usize::MAX, 0).is_err());
        assert_eq!(e.text, "old");
        assert_eq!(e.display_text(), "新");
        assert_eq!(e.selected_text(), "old");
        e.insert("新規").unwrap();
        assert_eq!(e.text, "新規");
    }

    #[test]
    fn undo_keeps_only_thirty_two_committed_versions() {
        let mut e = Editor::new("").unwrap();
        for _ in 0..40 { e.insert("x").unwrap(); }
        for _ in 0..32 { assert!(e.undo()); }
        assert_eq!(e.text, "xxxxxxxx");
        assert!(!e.undo());
    }
}
