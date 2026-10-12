//! The message box: multi-line text with a cursor, wrapped to the box width.

use unicode_width::UnicodeWidthChar;

#[derive(Default)]
pub struct Editor {
    pub text: String,
    /// In chars.
    pub cursor: usize,
}

/// One visual row: where it starts in `text` (chars) and what it shows.
pub struct Row {
    pub start: usize,
    pub chars: Vec<char>,
}

impl Editor {
    fn byte(&self, c: usize) -> usize {
        self.text.char_indices().nth(c).map(|(i, _)| i).unwrap_or(self.text.len())
    }

    fn len(&self) -> usize {
        self.text.chars().count()
    }

    pub fn is_empty(&self) -> bool {
        self.text.trim().is_empty()
    }

    pub fn take(&mut self) -> String {
        self.cursor = 0;
        std::mem::take(&mut self.text)
    }

    pub fn set(&mut self, text: String) {
        self.cursor = text.chars().count();
        self.text = text;
    }

    pub fn insert(&mut self, s: &str) {
        let s = s.replace("\r\n", "\n").replace('\r', "\n");
        let i = self.byte(self.cursor);
        self.text.insert_str(i, &s);
        self.cursor += s.chars().count();
    }

    pub fn backspace(&mut self) {
        if self.cursor > 0 {
            self.cursor -= 1;
            let i = self.byte(self.cursor);
            self.text.remove(i);
        }
    }

    pub fn delete(&mut self) {
        if self.cursor < self.len() {
            let i = self.byte(self.cursor);
            self.text.remove(i);
        }
    }

    pub fn delete_word(&mut self) {
        let end = self.byte(self.cursor);
        let head = self.text[..end].trim_end_matches([' ', '\n']);
        let start = head.rfind([' ', '\n']).map(|i| i + 1).unwrap_or(0);
        self.text.replace_range(start..end, "");
        self.cursor = self.text[..start].chars().count();
    }

    pub fn left(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    pub fn right(&mut self) {
        self.cursor = (self.cursor + 1).min(self.len());
    }

    /// Start and end of the logical line (between newlines) holding the cursor.
    pub fn home(&mut self) {
        let chars: Vec<char> = self.text.chars().collect();
        while self.cursor > 0 && chars[self.cursor - 1] != '\n' {
            self.cursor -= 1;
        }
    }

    pub fn end(&mut self) {
        let chars: Vec<char> = self.text.chars().collect();
        while self.cursor < chars.len() && chars[self.cursor] != '\n' {
            self.cursor += 1;
        }
    }

    pub fn rows(&self, width: usize) -> Vec<Row> {
        let width = width.max(2);
        let mut rows = vec![Row { start: 0, chars: Vec::new() }];
        let mut w = 0;
        for (i, ch) in self.text.chars().enumerate() {
            if ch == '\n' {
                rows.push(Row { start: i + 1, chars: Vec::new() });
                w = 0;
                continue;
            }
            let cw = ch.width().unwrap_or(0);
            if w + cw > width {
                rows.push(Row { start: i, chars: Vec::new() });
                w = 0;
            }
            rows.last_mut().unwrap().chars.push(ch);
            w += cw;
        }
        rows
    }

    /// Cursor as (row, column in cells).
    pub fn position(&self, rows: &[Row]) -> (usize, usize) {
        let r = rows.iter().rposition(|r| r.start <= self.cursor).unwrap_or(0);
        let n = (self.cursor - rows[r].start).min(rows[r].chars.len());
        (r, rows[r].chars[..n].iter().map(|c| c.width().unwrap_or(0)).sum())
    }

    /// Moves a visual row up or down, keeping the column. False at the edge.
    pub fn vertical(&mut self, width: usize, delta: isize) -> bool {
        let rows = self.rows(width);
        let (r, col) = self.position(&rows);
        let target = r as isize + delta;
        if target < 0 || target >= rows.len() as isize {
            return false;
        }
        let row = &rows[target as usize];
        let mut w = 0;
        let mut k = 0;
        for ch in &row.chars {
            let cw = ch.width().unwrap_or(0);
            if w + cw > col {
                break;
            }
            w += cw;
            k += 1;
        }
        self.cursor = row.start + k;
        true
    }
}
