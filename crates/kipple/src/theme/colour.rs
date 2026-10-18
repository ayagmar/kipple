//! Whether to use colour, and how many colours the terminal has: decided once from the
//! command line and the environment snapshot, never read again.

use ratatui::style::Color;

use kipple_platform::Environment;

/// `--color`: whether to use colour.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ColorChoice {
    /// When the output is a terminal and `NO_COLOR` isn't set.
    #[default]
    Auto,
    /// Always, even when piped or under `NO_COLOR`.
    Always,
    /// Never.
    Never,
}

/// How many colours the terminal shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Depth {
    /// The 16 ANSI colours.
    Ansi16,
    /// The 256-colour palette.
    Ansi256,
    /// Any 24-bit colour.
    TrueColor,
}

/// Whether colour is drawn, and in how many colours.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Colours {
    /// No colour: the `mono` theme.
    Off,
    /// Colour, brought down to this depth.
    On(Depth),
}

impl Colours {
    /// What `choice` says, then `NO_COLOR` (set and not empty, per no-color.org), then
    /// whether the output is a `terminal`. The depth comes from `COLORTERM`
    /// (`truecolor` or `24bit`), else `WT_SESSION` (Windows Terminal, which always has
    /// 24-bit colour), else `TERM` (`…256color…`), else 16 colours.
    #[must_use]
    pub fn detect(choice: ColorChoice, env: &Environment, terminal: bool) -> Self {
        let on = match choice {
            ColorChoice::Always => true,
            ColorChoice::Never => false,
            ColorChoice::Auto => terminal && env.get("NO_COLOR").is_none(),
        };
        if on {
            Self::On(Depth::detect(env))
        } else {
            Self::Off
        }
    }
}

impl Depth {
    fn detect(env: &Environment) -> Self {
        let var = |name| {
            env.get(name)
                .map(|value| value.to_string_lossy().to_lowercase())
        };
        let truecolor =
            var("COLORTERM").is_some_and(|value| value == "truecolor" || value == "24bit");
        if truecolor || var("WT_SESSION").is_some_and(|value| !value.is_empty()) {
            Self::TrueColor
        } else if var("TERM").is_some_and(|value| value.contains("256color")) {
            Self::Ansi256
        } else {
            Self::Ansi16
        }
    }

    /// `colour` as this depth can show it: the nearest of the 256 or 16 colours.
    /// Names and `reset` stay as they are; so do indexes in 256 colours.
    #[must_use]
    pub fn fit(self, colour: Color) -> Color {
        match (self, colour) {
            (Self::Ansi256, Color::Rgb(r, g, b)) => Color::Indexed(nearest_256([r, g, b])),
            (Self::Ansi16, Color::Rgb(r, g, b)) => nearest_16([r, g, b]),
            (Self::Ansi16, Color::Indexed(index)) => match ANSI16.get(usize::from(index)) {
                Some(&(named, _)) => named,
                None => nearest_16(rgb(index)),
            },
            (Self::TrueColor | Self::Ansi256 | Self::Ansi16, colour) => colour,
        }
    }
}

/// The 16 ANSI colours with xterm's values for them, in palette order.
const ANSI16: [(Color, [u8; 3]); 16] = [
    (Color::Black, [0, 0, 0]),
    (Color::Red, [205, 0, 0]),
    (Color::Green, [0, 205, 0]),
    (Color::Yellow, [205, 205, 0]),
    (Color::Blue, [0, 0, 238]),
    (Color::Magenta, [205, 0, 205]),
    (Color::Cyan, [0, 205, 205]),
    (Color::Gray, [229, 229, 229]),
    (Color::DarkGray, [127, 127, 127]),
    (Color::LightRed, [255, 0, 0]),
    (Color::LightGreen, [0, 255, 0]),
    (Color::LightYellow, [255, 255, 0]),
    (Color::LightBlue, [92, 92, 255]),
    (Color::LightMagenta, [255, 0, 255]),
    (Color::LightCyan, [0, 255, 255]),
    (Color::White, [255, 255, 255]),
];

/// The value of a 256-colour index: the 16 ANSI colours, the 6×6×6 cube, then 24 greys.
fn rgb(index: u8) -> [u8; 3] {
    const LEVELS: [u8; 6] = [0, 95, 135, 175, 215, 255];
    let level = |at: u8| LEVELS.get(usize::from(at)).copied().unwrap_or(0);
    match index {
        0..=15 => ANSI16
            .get(usize::from(index))
            .map_or([0; 3], |&(_, rgb)| rgb),
        16..=231 => {
            let cube = index - 16;
            [level(cube / 36), level(cube / 6 % 6), level(cube % 6)]
        }
        232..=255 => [8 + 10 * (index - 232); 3],
    }
}

/// How far apart two colours are: squared distance in RGB.
fn distance(a: [u8; 3], b: [u8; 3]) -> u32 {
    a.iter()
        .zip(b)
        .map(|(&x, y)| u32::from(x.abs_diff(y)).pow(2))
        .sum()
}

/// The nearest index of the cube and the greys. The first 16 are left out: terminals
/// change them, so they can't stand for an exact colour.
fn nearest_256(colour: [u8; 3]) -> u8 {
    (16..=255)
        .min_by_key(|&index| distance(rgb(index), colour))
        .unwrap_or(16)
}

/// The nearest of the 16 ANSI colours.
fn nearest_16(colour: [u8; 3]) -> Color {
    ANSI16
        .iter()
        .min_by_key(|&&(_, rgb)| distance(rgb, colour))
        .map_or(Color::Reset, |&(named, _)| named)
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;

    use super::*;

    fn env(vars: &[(&str, &str)]) -> Environment {
        Environment::from_vars(
            vars.iter()
                .map(|&(k, v)| (OsString::from(k), OsString::from(v))),
        )
    }

    // Catches the precedence going wrong: `--color` beats NO_COLOR, which beats the
    // terminal, and an empty NO_COLOR doesn't count (no-color.org).
    #[test]
    fn color_flag_beats_no_color_which_beats_the_terminal() {
        let plain = env(&[]);
        let no_color = env(&[("NO_COLOR", "1")]);
        let empty = env(&[("NO_COLOR", "")]);
        let on = Colours::On(Depth::Ansi16);
        let cases = [
            (ColorChoice::Auto, &plain, true, on),
            (ColorChoice::Auto, &plain, false, Colours::Off),
            (ColorChoice::Auto, &no_color, true, Colours::Off),
            (ColorChoice::Auto, &empty, true, on),
            (ColorChoice::Always, &no_color, false, on),
            (ColorChoice::Never, &plain, true, Colours::Off),
        ];
        for (choice, env, terminal, want) in cases {
            assert_eq!(
                Colours::detect(choice, env, terminal),
                want,
                "{choice:?} {terminal}"
            );
        }
    }

    // Catches a depth read from the wrong variable, 24-bit colour sent to a terminal
    // that only said it has 256, or Windows Terminal brought down to 256 colours.
    #[test]
    fn depth_comes_from_colorterm_then_windows_terminal_then_term() {
        let cases = [
            (vec![("COLORTERM", "truecolor")], Depth::TrueColor),
            (
                vec![("COLORTERM", "24bit"), ("TERM", "xterm")],
                Depth::TrueColor,
            ),
            (vec![("TERM", "xterm-256color")], Depth::Ansi256),
            (
                vec![("COLORTERM", "yes"), ("TERM", "screen-256color")],
                Depth::Ansi256,
            ),
            (vec![("TERM", "xterm")], Depth::Ansi16),
            (vec![], Depth::Ansi16),
            // Windows Terminal sets no COLORTERM but always has 24-bit colour.
            (
                vec![("WT_SESSION", "0f1e2d3c"), ("TERM", "xterm-256color")],
                Depth::TrueColor,
            ),
            (vec![("WT_SESSION", "")], Depth::Ansi16),
        ];
        for (vars, want) in cases {
            let detected = Colours::detect(ColorChoice::Always, &env(&vars), true);
            assert_eq!(detected, Colours::On(want), "{vars:?}");
        }
    }

    // Catches a colour brought down to a far one: brand amber stays orange in 256
    // colours and yellow in 16, exact cube and grey values map to themselves, and names
    // never move.
    #[test]
    fn colours_come_down_to_the_nearest_the_terminal_has() {
        let amber = Color::Rgb(0xF2, 0xA2, 0x3A);
        assert_eq!(Depth::TrueColor.fit(amber), amber);
        assert_eq!(Depth::Ansi256.fit(amber), Color::Indexed(215));
        assert_eq!(Depth::Ansi16.fit(amber), Color::Yellow);
        assert_eq!(
            Depth::Ansi256.fit(Color::Rgb(95, 135, 175)),
            Color::Indexed(67)
        );
        assert_eq!(
            Depth::Ansi256.fit(Color::Rgb(128, 128, 128)),
            Color::Indexed(244)
        );
        assert_eq!(Depth::Ansi16.fit(Color::Indexed(9)), Color::LightRed);
        assert_eq!(Depth::Ansi16.fit(Color::Indexed(196)), Color::LightRed);
        assert_eq!(Depth::Ansi16.fit(Color::Indexed(232)), Color::Black);
        for named in [Color::Green, Color::DarkGray, Color::Reset] {
            assert_eq!(Depth::Ansi16.fit(named), named);
            assert_eq!(Depth::Ansi256.fit(named), named);
        }
        assert_eq!(Depth::Ansi256.fit(Color::Indexed(9)), Color::Indexed(9));
    }
}
