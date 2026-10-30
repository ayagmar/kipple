//! Themes on the home screen (D-055, `docs/03-functional-spec.md` §5 Themes): each
//! built-in theme's styles, colour brought down to what the terminal says it has, and
//! no colour at all under `NO_COLOR`, with every state still a word.

use std::ffi::OsString;

use kipple_platform::Environment;
use ratatui::style::Color;

use super::{draw, home, screen, styled};
use crate::theme::{ColorChoice, Colours, Depth, Glyphs, Look, Stdout, Theme};

/// Whether a terminal can show a colour.
type Fits = fn(Color) -> bool;

const BUILT_IN: [&str; 5] = [
    "kipple",
    "kipple-dark",
    "kipple-light",
    "high-contrast",
    "mono",
];

fn drawn(name: &str, colours: Colours) -> Theme {
    Theme::builtin(name).unwrap().drawn(colours)
}

fn look(theme: &Theme) -> Look<'_> {
    Look {
        theme,
        glyphs: Glyphs::Unicode,
    }
}

// Catches a built-in theme's colour or modifier changing without anyone deciding it.
#[test]
fn each_built_in_theme_at_120_by_40() {
    let app = home();
    for name in BUILT_IN {
        let theme = drawn(name, Colours::On(Depth::TrueColor));
        insta::assert_snapshot!(
            format!("theme_{name}_at_120_by_40"),
            styled(&draw(&app, look(&theme), 120, 40))
        );
    }
}

// Catches 24-bit colour sent to a terminal that said it has only 256 or 16 colours,
// which such a terminal draws as garbage or not at all.
#[test]
fn a_24_bit_theme_draws_in_256_or_16_colours_when_the_terminal_says_so() {
    let app = home();
    let cases: [(&str, &str, Fits); 2] = [
        ("TERM", "xterm-256color", |c| !matches!(c, Color::Rgb(..))),
        ("TERM", "xterm", |c| {
            !matches!(c, Color::Rgb(..) | Color::Indexed(_))
        }),
    ];
    for (var, value, fits) in cases {
        let env = Environment::from_vars([(OsString::from(var), OsString::from(value))]);
        let colours = Colours::detect(ColorChoice::Auto, &env, Stdout::Terminal);
        let theme = drawn("kipple-dark", colours);
        let buffer = draw(&app, look(&theme), 120, 40);
        assert!(
            buffer
                .content
                .iter()
                .all(|cell| fits(cell.fg) && fits(cell.bg)),
            "{value}"
        );
        // The same words as in full colour.
        let full = drawn("kipple-dark", Colours::On(Depth::TrueColor));
        assert_eq!(
            screen(&app, look(&theme), 120, 40),
            screen(&app, look(&full), 120, 40)
        );
    }
}

// Catches colour leaking through NO_COLOR or `--color never` in any theme, and a state
// that is only a colour: without colour every theme draws exactly what mono draws, and
// each state is still there in words.
#[test]
fn without_colour_every_theme_draws_as_mono_and_every_state_keeps_its_word() {
    let app = home();
    let mono = drawn("mono", Colours::Off);
    let words = screen(&app, look(&mono), 120, 40);
    for state in [
        "Safe to clean",
        "Review",
        "Protected",
        "recent",
        "layout not recognised",
    ] {
        assert!(words.contains(state), "{state}:\n{words}");
    }
    for name in BUILT_IN {
        let theme = drawn(name, Colours::Off);
        let buffer = draw(&app, look(&theme), 120, 40);
        assert!(
            buffer
                .content
                .iter()
                .all(|cell| cell.fg == Color::Reset && cell.bg == Color::Reset),
            "{name}"
        );
        assert_eq!(screen(&app, look(&theme), 120, 40), words, "{name}");
    }
}
