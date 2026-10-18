//! The title line: the level, the scan, the roots, the sort order, and what narrows the
//! tree.

use kipple_core::{Level, ScanOutcome};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line as Text, Span};
use ratatui::widgets::Paragraph;

use super::{Look, cols, lens_title, title_case};
use crate::human::{escaped, homed};
use crate::tui::app::{App, Mode, Progress};

pub(super) fn header(app: &App, look: Look, width: u16) -> Paragraph<'static> {
    let state = match app.progress {
        Progress::Scanning => look.ellipsis("scanning"),
        Progress::Done { outcome, .. } if outcome != ScanOutcome::Completed => {
            "scan stopped early".to_owned()
        }
        Progress::Done { elapsed, .. } => {
            format!(
                "scanned in {}.{} s",
                elapsed.as_secs(),
                elapsed.subsec_millis() / 100
            )
        }
    };
    let mut spans = vec![Span::styled(
        "kipple",
        Style::new().add_modifier(Modifier::BOLD),
    )];
    for (index, &level) in Level::ALL.iter().enumerate() {
        spans.push(Span::raw(if index == 0 { "  " } else { look.dot() }));
        let name = title_case(level.name());
        // The brackets mark the level without colour.
        spans.push(if level == app.policy.level {
            Span::styled(
                format!("[{name}]"),
                look.fg(Color::Cyan).add_modifier(Modifier::BOLD),
            )
        } else {
            Span::styled(name, look.dim())
        });
    }
    spans.push(Span::raw(look.dot()));
    spans.push(Span::raw(state));
    spans.push(Span::raw(look.dot()));
    // A sort order other than size changes how the list reads, and the header is the only
    // lasting sign of it, so it comes first whenever it fits whole. The roots, fixed
    // config also listed in settings, get what is left.
    let used: usize = spans.iter().map(Span::width).sum();
    let room = usize::from(width).saturating_sub(used);
    let order = app.order.title().filter(|order| cols(order) <= room);
    let reserved = order.map_or(0, |order| cols(order) + cols(look.dot()));
    let roots = roots(app, look, room.saturating_sub(reserved));
    if let Some(roots) = &roots {
        spans.push(Span::raw(roots.clone()));
    }
    if let Some(order) = order {
        if roots.is_some() {
            spans.push(Span::raw(look.dot()));
        }
        spans.push(Span::raw(order));
    }
    if !app.gaps.is_empty() {
        spans.push(Span::raw(look.dot()));
        spans.push(Span::styled(
            format!("partial: {} not covered", app.gaps.len()),
            look.fg(Color::Yellow),
        ));
    }
    if let Some(lens) = app.lens {
        spans.push(Span::raw(look.dot()));
        spans.push(Span::raw(format!("lens: {}", lens_title(lens))));
    }
    if !app.filter.is_empty() && app.mode != Mode::Filter {
        spans.push(Span::raw(look.dot()));
        spans.push(Span::raw(format!("filter: {}", escaped(&app.filter))));
    }
    Paragraph::new(Text::from(spans))
}

/// The project roots in at most `room` columns: the first one and how many more,
/// the first one shortened from the left, or only how many. `None` when not even that
/// fits.
fn roots(app: &App, look: Look, room: usize) -> Option<String> {
    let fits = |text: String| (cols(&text) <= room).then_some(text);
    let Some((first, rest)) = app.roots.split_first() else {
        return fits("no project roots".to_owned());
    };
    let first = escaped(&homed(first, app.home.as_deref()));
    let more = if rest.is_empty() {
        String::new()
    } else {
        format!(" +{}", rest.len())
    };
    if let Some(full) = fits(format!("{first}{more}")) {
        return Some(full);
    }
    let ellipsis = look.ellipsis("");
    let keep = room.saturating_sub(cols(&more) + cols(&ellipsis));
    // Fewer than a few characters of a path say nothing; the count says more.
    if keep >= 4 {
        let mut taken = 0;
        let start = first
            .char_indices()
            .rev()
            .find_map(|(index, c)| {
                taken += cols(c.encode_utf8(&mut [0; 4]));
                (taken > keep).then_some(index + c.len_utf8())
            })
            .unwrap_or(0);
        let end = first.get(start..).unwrap_or_default();
        return fits(format!("{ellipsis}{end}{more}"));
    }
    fits(match app.roots.len() {
        1 => "1 root".to_owned(),
        count => format!("{count} roots"),
    })
}
