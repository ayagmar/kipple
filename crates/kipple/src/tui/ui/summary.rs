//! The summary band: each bucket's total and count over the whole scan, as one card
//! each with a bar of its share of what was found, or as one line when narrow.

use ratatui::Frame;
use ratatui::layout::{Margin, Rect};
use ratatui::style::Modifier;
use ratatui::text::{Line as Text, Span};
use ratatui::widgets::Paragraph;

use super::frame::vertical;
use super::layout::Areas;
use super::{Look, drawn_bar, tint};
use crate::human::{self, bytes};
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
    let mut buckets: Vec<Bucket> = Shelf::SORTED
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

/// How the one-line band is shortened to fit: the gaps between buckets, then an empty
/// bucket's words, then the counts, then the gaps again. What follows the buckets
/// always shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Squeeze {
    None,
    Gaps,
    Empty,
    Counts,
    Tight,
}

/// What a bucket says after its name. An empty bucket of a partial scan says the scan
/// is partial when `whole`; the one-line band says it once at its end instead.
fn amount(app: &App, look: Look<'_>, bucket: Bucket, whole: bool, squeeze: Squeeze) -> String {
    if bucket.shelf == Shelf::Measuring {
        return bucket.count.to_string();
    }
    if bucket.count > 0 && squeeze >= Squeeze::Counts {
        return bytes(bucket.bytes);
    }
    if bucket.count > 0 {
        return format!(
            "{}{}{}",
            bytes(bucket.bytes),
            look.glyphs.dot(),
            bucket.count
        );
    }
    if squeeze >= Squeeze::Empty {
        return "none".to_owned();
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
    let name = human::standing(bucket.shelf.bucket()).name;
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
        frame.render_widget(
            Paragraph::new(line(app, look, &buckets, usize::from(area.width))),
            area,
        );
        return;
    }
    let found: u64 = buckets.iter().map(|bucket| bucket.bytes).sum();
    // The band's text area keeps a column clear on each side; the cards' lines start
    // in that column, so they reach the frame.
    let band = Rect::new(
        area.x.saturating_sub(1),
        area.y,
        area.width + 2,
        area.height,
    );
    // Beside the detail pane, the totals card is as wide as the pane, so the line
    // before it meets the line between the tree and the pane.
    let totals = areas
        .divider
        .filter(|&x| x > band.x && x < band.right())
        .map_or(TOTALS, |x| band.right() - x - 1);
    let lines = u16::try_from(buckets.len()).unwrap_or(u16::MAX);
    let room = band.width.saturating_sub(totals).saturating_sub(lines);
    // Each card needs its widest line; when they don't all fit, their narrowest forms.
    let needs_of = |least: bool| -> Vec<u16> {
        let width = if least { 0 } else { usize::MAX };
        buckets
            .iter()
            .map(|&bucket| {
                let widest = card_lines(app, look, bucket, None, width)
                    .iter()
                    .map(Text::width)
                    .max()
                    .unwrap_or(0);
                u16::try_from(widest).unwrap_or(u16::MAX).saturating_add(2)
            })
            .collect()
    };
    let mut needs = needs_of(false);
    if needs.iter().sum::<u16>() > room {
        needs = needs_of(true);
    }
    let widths = share(room, &needs);
    // One bar length for every card, so the bars read as one split of what was found.
    let bar = widths.iter().min().map(|width| width.saturating_sub(2));
    let mut x = band.x;
    for (&bucket, &width) in buckets.iter().zip(&widths) {
        let card = Rect::new(x, band.y, width, band.height).inner(Margin::new(1, 0));
        let bar = bar.filter(|_| areas.bars).map(|bar| (bar, found));
        let text = card_lines(app, look, bucket, bar, usize::from(card.width));
        frame.render_widget(Paragraph::new(text), card);
        x += width;
        vertical(look, x, band.y, band.bottom(), frame.buffer_mut());
        x += 1;
    }
    let sums = app.totals();
    let text = vec![
        Text::raw(format!("found {}", bytes(sums.discovered))),
        Text::raw(format!("eligible {}", bytes(sums.eligible))),
    ];
    let last = Rect::new(x, band.y, band.right().saturating_sub(x), band.height);
    frame.render_widget(Paragraph::new(text), last.inner(Margin::new(1, 0)));
}

/// `room` columns shared out among cards that need `needs`: as evenly as can be, with
/// a card that needs more than its share given what it needs, unless the needs don't
/// fit at all.
fn share(room: u16, needs: &[u16]) -> Vec<u16> {
    let mut given = vec![false; needs.len()];
    loop {
        let taken: u16 = needs
            .iter()
            .zip(&given)
            .filter_map(|(&need, &given)| given.then_some(need))
            .sum();
        let shared = given.iter().filter(|&&given| !given).count();
        let Some(mut evenly) = even(room.saturating_sub(taken), shared) else {
            // Not even their needs fit: an even share each.
            let mut evenly = even(room, needs.len()).unwrap_or_default();
            return needs.iter().map(|_| evenly.next()).collect();
        };
        let each = evenly.each;
        let mut grew = false;
        for (&need, flag) in needs.iter().zip(given.iter_mut()) {
            if !*flag && need > each {
                *flag = true;
                grew = true;
            }
        }
        if !grew {
            return needs
                .iter()
                .zip(&given)
                .map(|(&need, &given)| if given { need } else { evenly.next() })
                .collect();
        }
    }
}

/// `room` split into `count` shares as even as can be, the first ones a column wider.
#[derive(Debug, Default)]
struct Even {
    each: u16,
    extra: u16,
}

impl Even {
    fn next(&mut self) -> u16 {
        let one = u16::from(self.extra > 0);
        self.extra -= one;
        self.each + one
    }
}

fn even(room: u16, count: usize) -> Option<Even> {
    let count = u16::try_from(count).ok().filter(|&count| count > 0)?;
    Some(Even {
        each: room / count,
        extra: room % count,
    })
}

/// A card `width` columns wide: the bucket's name and amount, then a bar of its share
/// of what was found, `bar` giving its length and what a full bar stands for, unless
/// bars are off. An empty bucket has no bar, so
/// what it says goes there, where it has the card's width, and so does the amount of a
/// card too narrow for both on one line. Narrower still, an empty bucket of a partial
/// scan says `none found` after its name and `scan partial` below.
fn card_lines(
    app: &App,
    look: Look<'_>,
    bucket: Bucket,
    bar: Option<(u16, u64)>,
    width: usize,
) -> Vec<Text<'static>> {
    let name = name(look, bucket, true);
    let amount = amount(app, look, bucket, true, Squeeze::None);
    let fits = |lines: &[Text<'static>]| lines.iter().all(|line| line.width() <= width);
    if bucket.shelf == Shelf::Measuring {
        let amount = Span::styled(format!(" {amount}"), look.style(Role::Muted));
        return vec![Text::from(vec![name, amount])];
    }
    if bucket.count == 0 {
        let stacked = vec![
            Text::from(name.clone()),
            Text::styled(amount, look.style(Role::Muted)),
        ];
        if fits(&stacked) || app.gaps.is_empty() {
            return stacked;
        }
        let none = Span::styled(" none found", look.style(Role::Muted));
        return vec![
            Text::from(vec![name, none]),
            Text::styled("scan partial", look.style(Role::Muted)),
        ];
    }
    let head = Text::from(vec![name.clone(), Span::raw(format!(" {amount}"))]);
    if !fits(std::slice::from_ref(&head)) {
        return vec![Text::from(name), Text::raw(amount)];
    }
    let Some((cells, found)) = bar else {
        return vec![head];
    };
    let cells = usize::from(cells);
    let eighths = u128::from(bucket.bytes) * (cells as u128 * 8) / u128::from(found.max(1));
    vec![
        head,
        Text::from(drawn_bar(look, bucket.shelf, eighths, cells)),
    ]
}

/// The band as one line in `width` columns: each bucket's name and amount, shortened
/// until what follows them fits.
fn line(app: &App, look: Look<'_>, buckets: &[Bucket], width: usize) -> Text<'static> {
    let mut text = line_in(app, look, buckets, Squeeze::None);
    for squeeze in [
        Squeeze::Gaps,
        Squeeze::Empty,
        Squeeze::Counts,
        Squeeze::Tight,
    ] {
        if text.width() <= width {
            break;
        }
        text = line_in(app, look, buckets, squeeze);
    }
    text
}

fn line_in(app: &App, look: Look<'_>, buckets: &[Bucket], squeeze: Squeeze) -> Text<'static> {
    let gap = match squeeze {
        Squeeze::None => "   ",
        Squeeze::Gaps | Squeeze::Empty | Squeeze::Counts => "  ",
        Squeeze::Tight => " ",
    };
    let mut spans = Vec::new();
    for (index, &bucket) in buckets.iter().enumerate() {
        if index > 0 {
            spans.push(Span::raw(gap));
        }
        spans.push(name(look, bucket, false));
        let amount = Span::raw(format!(" {}", amount(app, look, bucket, false, squeeze)));
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
