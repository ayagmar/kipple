//! The summary band: each bucket's total and count over the whole scan, as one card
//! each with a bar of its share of what was found, or as one line when narrow.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::Modifier;
use ratatui::text::{Line as Text, Span};
use ratatui::widgets::Paragraph;

use super::frame::vertical;
use super::layout::Areas;
use super::{Look, bar_tint, tint};
use crate::human::bytes;
use crate::theme::Role;
use crate::tui::app::{App, Progress, Shelf};

/// The totals card's width: `eligible >= 1023.9 GiB` and a space either side.
const TOTALS: u16 = 22;

/// One bucket's findings over the whole scan.
#[derive(Debug, Clone, Copy)]
struct Bucket {
    shelf: Shelf,
    count: usize,
    bytes: u64,
}

/// The three buckets, then Measuring while anything is.
fn buckets(app: &App) -> Vec<Bucket> {
    let mut buckets: Vec<Bucket> = [Shelf::Safe, Shelf::Review, Shelf::Protected]
        .into_iter()
        .map(|shelf| Bucket {
            shelf,
            count: 0,
            bytes: 0,
        })
        .collect();
    let mut measuring = Bucket {
        shelf: Shelf::Measuring,
        count: 0,
        bytes: 0,
    };
    for row in &app.rows {
        let shelf = row.shelf();
        let bucket = buckets
            .iter_mut()
            .find(|bucket| bucket.shelf == shelf)
            .unwrap_or(&mut measuring);
        bucket.count += 1;
        bucket.bytes += row.bytes();
    }
    if measuring.count > 0 {
        buckets.push(measuring);
    }
    buckets
}

/// What a bucket says after its name. An empty bucket of a partial scan says the scan
/// is partial when `whole`; the one-line band says it once at its end instead.
fn amount(app: &App, look: Look<'_>, bucket: Bucket, whole: bool) -> String {
    if bucket.shelf == Shelf::Measuring {
        return bucket.count.to_string();
    }
    if bucket.count > 0 {
        return format!(
            "{}{}{}",
            bytes(bucket.bytes),
            look.glyphs.dot(),
            bucket.count
        );
    }
    // An empty bucket of a scan that couldn't read everything says so: it doesn't mean
    // there is nothing there.
    if !app.gaps.is_empty() && whole {
        "none found, scan partial".to_owned()
    } else if !app.gaps.is_empty() {
        "none found".to_owned()
    } else if matches!(app.progress, Progress::Scanning) {
        "none yet".to_owned()
    } else {
        "none found".to_owned()
    }
}

fn name(look: Look<'_>, bucket: Bucket, upper: bool) -> Span<'static> {
    let name = match bucket.shelf {
        Shelf::Safe => "Safe",
        Shelf::Review => "Review",
        Shelf::Protected => "Protected",
        Shelf::Measuring => "Measuring",
    };
    let name = if upper {
        name.to_uppercase()
    } else {
        name.to_owned()
    };
    let style = look.style(tint(bucket.shelf));
    // Measuring is a muted card, not a bucket's heading.
    if bucket.shelf == Shelf::Measuring {
        Span::styled(name, style)
    } else {
        Span::styled(name, style.add_modifier(Modifier::BOLD))
    }
}

/// Draws the band where `areas` puts it, if anywhere: one card per bucket and one for
/// the totals, each with a bar unless bars are off, or one line.
pub(super) fn render(app: &App, look: Look<'_>, areas: &Areas, frame: &mut Frame<'_>) {
    let Some((area, cards)) = areas.band else {
        return;
    };
    let buckets = buckets(app);
    if !cards {
        frame.render_widget(Paragraph::new(line(app, look, &buckets)), area);
        return;
    }
    let found: u64 = buckets.iter().map(|bucket| bucket.bytes).sum();
    let mut widths: Vec<Constraint> = Vec::new();
    for _ in &buckets {
        widths.push(Constraint::Fill(1));
        widths.push(Constraint::Length(1));
    }
    widths.push(Constraint::Length(TOTALS));
    // The band's text area keeps a column clear on each side; the cards' lines start
    // in that column, so they reach the frame.
    let band = Rect::new(
        area.x.saturating_sub(1),
        area.y,
        area.width + 2,
        area.height,
    );
    let parts = Layout::horizontal(widths).split(band);
    for (index, bucket) in buckets.iter().enumerate() {
        let (Some(&card), Some(&line)) = (parts.get(index * 2), parts.get(index * 2 + 1)) else {
            continue;
        };
        let card = card.inner(ratatui::layout::Margin::new(1, 0));
        frame.render_widget(
            Paragraph::new(card_lines(
                app,
                look,
                *bucket,
                found,
                areas.bars.then_some(card.width),
            )),
            card,
        );
        vertical(look, line.x, line.y, line.bottom(), frame.buffer_mut());
    }
    if let Some(&last) = parts.last() {
        let totals = app.totals();
        let text = vec![
            Text::raw(format!("found {}", bytes(totals.discovered))),
            Text::raw(format!("eligible {}", bytes(totals.eligible))),
        ];
        let last = last.inner(ratatui::layout::Margin::new(1, 0));
        frame.render_widget(Paragraph::new(text), last);
    }
}

/// A card: the bucket's name and amount, then a bar `bar` columns wide of its share of
/// `found`, unless bars are off. An empty bucket has no bar, so what it says goes
/// there, where it has the card's width.
fn card_lines(
    app: &App,
    look: Look<'_>,
    bucket: Bucket,
    found: u64,
    bar: Option<u16>,
) -> Vec<Text<'static>> {
    let name = name(look, bucket, true);
    let amount = amount(app, look, bucket, true);
    if bucket.shelf == Shelf::Measuring {
        let amount = Span::styled(format!(" {amount}"), look.style(Role::Muted));
        return vec![Text::from(vec![name, amount])];
    }
    let head = Text::from(vec![name.clone(), Span::raw(format!(" {amount}"))]);
    if bucket.count == 0 {
        return vec![
            Text::from(name),
            Text::styled(amount, look.style(Role::Muted)),
        ];
    }
    let Some(cells) = bar.map(usize::from) else {
        return vec![head];
    };
    let eighths = u128::from(bucket.bytes) * (cells as u128 * 8) / u128::from(found.max(1));
    let drawn = look
        .glyphs
        .bar(u64::try_from(eighths).unwrap_or(u64::MAX), cells);
    vec![
        head,
        Text::styled(drawn, look.style(bar_tint(bucket.shelf))),
    ]
}

/// The band as one line: each bucket's name and amount.
fn line(app: &App, look: Look<'_>, buckets: &[Bucket]) -> Text<'static> {
    let mut spans = Vec::new();
    for (index, &bucket) in buckets.iter().enumerate() {
        if index > 0 {
            spans.push(Span::raw("   "));
        }
        spans.push(name(look, bucket, false));
        let amount = Span::raw(format!(" {}", amount(app, look, bucket, false)));
        spans.push(if bucket.shelf == Shelf::Measuring {
            amount.style(look.style(Role::Muted))
        } else {
            amount
        });
    }
    if !app.gaps.is_empty() && buckets.iter().any(|bucket| bucket.count == 0) {
        spans.push(Span::raw(look.glyphs.dot()));
        spans.push(Span::styled("scan partial", look.style(Role::Warning)));
    }
    Text::from(spans)
}
