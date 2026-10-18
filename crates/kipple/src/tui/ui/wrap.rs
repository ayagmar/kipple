//! Breaking the detail pane's lines into rows that fit.

use ratatui::style::Style;
use ratatui::text::{Line as Text, Span};

use super::cols;

/// `lines` broken into rows of at most `width` columns: between words where it can, a
/// word longer than a row wherever it must. A break swallows the space it falls on.
pub(super) fn wrap(lines: Vec<Text<'static>>, width: u16) -> Vec<Text<'static>> {
    let width = usize::from(width);
    if width == 0 {
        return Vec::new();
    }
    let mut rows = Vec::new();
    for line in lines {
        let mut filling = Filling::new(line.style, width);
        for (word, space) in words(&line) {
            filling.word(&word);
            filling.space(&space);
        }
        rows.extend(filling.finish());
    }
    rows
}

/// A styled character.
type Cell = (char, Style);

/// `line`'s characters with their styles, as words each followed by the spaces after
/// it. Leading spaces come as a first, empty word's.
fn words(line: &Text<'_>) -> Vec<(Vec<Cell>, Vec<Cell>)> {
    let mut words: Vec<(Vec<Cell>, Vec<Cell>)> = vec![(Vec::new(), Vec::new())];
    for span in &line.spans {
        for c in span.content.chars() {
            let style = span.style;
            let Some(last) = words.last_mut() else {
                continue;
            };
            if c == ' ' {
                last.1.push((c, style));
            } else if last.1.is_empty() {
                last.0.push((c, style));
            } else {
                words.push((vec![(c, style)], Vec::new()));
            }
        }
    }
    words
}

fn char_cols(c: char) -> usize {
    cols(c.encode_utf8(&mut [0; 4]))
}

/// One line of the detail pane being broken into rows.
struct Filling {
    width: usize,
    style: Style,
    rows: Vec<Text<'static>>,
    spans: Vec<Span<'static>>,
    cols: usize,
}

impl Filling {
    const fn new(style: Style, width: usize) -> Self {
        Self {
            width,
            style,
            rows: Vec::new(),
            spans: Vec::new(),
            cols: 0,
        }
    }

    /// Adds `word` on this row, or on the next when it fits there whole.
    fn word(&mut self, word: &[Cell]) {
        let wide: usize = word.iter().map(|&(c, _)| char_cols(c)).sum();
        if self.cols + wide > self.width && wide <= self.width {
            self.next_row();
        }
        for &(c, style) in word {
            if self.cols + char_cols(c) > self.width {
                self.next_row();
            }
            self.push(c, style);
        }
    }

    /// Adds `space` up to the end of the row, where it breaks and the rest is dropped.
    fn space(&mut self, space: &[Cell]) {
        for &(c, style) in space {
            if self.cols + char_cols(c) > self.width {
                self.next_row();
                return;
            }
            self.push(c, style);
        }
    }

    fn push(&mut self, c: char, style: Style) {
        self.cols += char_cols(c);
        match self.spans.last_mut() {
            Some(span) if span.style == style => span.content.to_mut().push(c),
            _ => self.spans.push(Span::styled(c.to_string(), style)),
        }
    }

    /// Ends the row so far, unless nothing is on it yet.
    fn next_row(&mut self) {
        if self.cols == 0 {
            return;
        }
        self.cols = 0;
        let spans = std::mem::take(&mut self.spans);
        self.rows.push(Text::from(spans).style(self.style));
    }

    /// The rows, at least one: an empty line stays a row, and a break on the last
    /// space doesn't add one.
    fn finish(mut self) -> Vec<Text<'static>> {
        self.next_row();
        if self.rows.is_empty() {
            self.rows.push(Text::default().style(self.style));
        }
        self.rows
    }
}
