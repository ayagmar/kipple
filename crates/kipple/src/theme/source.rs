//! Where themes come from: the built-ins, and the user's files in the `themes` directory
//! beside the default config file, whatever `--config` names. A theme file is read the
//! way the config is, and the directory is listed without following links; nothing here
//! writes.

use std::borrow::Cow;
use std::fmt;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use kipple_core::{EntryKind, FsProbe as _};
use kipple_platform::{Environment, NoFollowFs, kipple_dirs, read_config, resolve_roots};

use super::{BUILTIN, DEFAULT, Theme, ThemeError, builtin_text, parse, valid_name};
use crate::human::escaped;

/// Where a theme comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Origin {
    /// Compiled into kipple.
    Builtin,
    /// A file in the user theme directory.
    File(PathBuf),
}

impl Origin {
    const fn path(&self) -> Option<&PathBuf> {
        match self {
            Self::Builtin => None,
            Self::File(path) => Some(path),
        }
    }
}

impl fmt::Display for Origin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Builtin => f.write_str("built-in"),
            Self::File(path) => write!(f, "{}", path.display()),
        }
    }
}

/// One theme of the [`Catalog`], resolved, or why it can't be used.
#[derive(Debug, Clone)]
pub struct Entry {
    /// Its name.
    pub name: String,
    /// Where it comes from.
    pub origin: Origin,
    /// The theme with what it extends, or why it can't be used.
    pub theme: Result<Theme, ThemeError>,
}

/// Every theme there is: the built-ins in their order, then the user's by name.
#[derive(Debug, Clone)]
pub struct Catalog {
    /// The themes.
    pub entries: Vec<Entry>,
    /// Why the user theme directory couldn't be listed, when it couldn't.
    pub problem: Option<String>,
}

impl Catalog {
    /// The built-in themes alone.
    #[must_use]
    pub fn builtin() -> Self {
        Themes {
            dir: Err(String::new()),
        }
        .catalog()
    }

    /// The theme called `name`.
    #[must_use]
    pub fn find(&self, name: &str) -> Option<&Entry> {
        self.entries.iter().find(|entry| entry.name == name)
    }
}

/// `kipple theme list`: each theme and where it comes from, or why it can't be used.
///
/// # Errors
/// When writing to `out` fails.
pub(crate) fn write_list(out: &mut dyn Write, catalog: &Catalog) -> io::Result<()> {
    for entry in &catalog.entries {
        let about = match (&entry.theme, &entry.origin) {
            (Err(error), _) => format!("can't be used: {error}"),
            (Ok(_), Origin::Builtin) if entry.name == DEFAULT => "built-in, the default".to_owned(),
            (Ok(_), origin) => origin.to_string(),
        };
        writeln!(out, "{:<16}{}", escaped(&entry.name), escaped(&about))?;
    }
    out.flush()
}

/// The themes on this machine: the built-ins and the user theme directory.
#[derive(Debug, Clone)]
pub struct Themes {
    /// The user theme directory, or why it is unknown.
    dir: Result<PathBuf, String>,
}

impl Themes {
    /// The themes of the machine `env` describes.
    #[must_use]
    pub fn new(env: &Environment) -> Self {
        let roots = resolve_roots(env, &[]);
        let dir = kipple_dirs(&roots)
            .config
            .path
            .map(|config| config.join("themes"))
            .map_err(|e| format!("kipple's config directory is unknown: {e}"));
        Self { dir }
    }

    /// The user theme directory, or why it is unknown.
    ///
    /// # Errors
    /// When kipple's config directory is unknown.
    pub fn dir(&self) -> Result<&Path, &str> {
        self.dir.as_deref().map_err(String::as_str)
    }

    /// The theme `name`, with every theme it extends under it.
    ///
    /// # Errors
    /// When there is no such theme, or it or a theme it extends can't be read, has a
    /// mistake, or extends itself through others.
    pub fn resolve(&self, name: &str) -> Result<Theme, ThemeError> {
        let Some((text, origin)) = self.source(name)? else {
            let message = self.dir.as_ref().map_or_else(
                |_| format!("there is no theme {name}"),
                |dir| {
                    let file = dir.join(format!("{name}.toml"));
                    format!(
                        "there is no theme {name}; a theme of your own is {}",
                        file.display()
                    )
                },
            );
            return Err(ThemeError {
                file: None,
                position: None,
                message,
            });
        };
        self.build(name, &text, &origin, &mut Vec::new())
    }

    /// Every theme, each resolved. A user file named like a built-in theme, or with a
    /// name no theme can have, is listed as an error.
    #[must_use]
    pub fn catalog(&self) -> Catalog {
        let mut entries: Vec<Entry> = BUILTIN
            .iter()
            .map(|&(name, _)| Entry {
                name: name.to_owned(),
                origin: Origin::Builtin,
                theme: self.resolve(name),
            })
            .collect();
        let (mut own, problem) = self.own();
        own.sort_by(|a, b| a.name.cmp(&b.name));
        entries.extend(own);
        Catalog { entries, problem }
    }

    /// The user's theme files, each resolved, and why the directory couldn't be listed.
    fn own(&self) -> (Vec<Entry>, Option<String>) {
        let Ok(dir) = &self.dir else {
            return (Vec::new(), None);
        };
        let listed = match NoFollowFs.read_dir(dir) {
            Ok(listed) => listed,
            Err(e) if e.io_kind() == Some(io::ErrorKind::NotFound) => return (Vec::new(), None),
            Err(e) => return (Vec::new(), Some(format!("can't be listed: {e}"))),
        };
        let entries = listed
            .into_iter()
            .filter(|entry| matches!(entry.kind, EntryKind::File | EntryKind::Symlink))
            .filter_map(|entry| {
                let file = dir.join(&entry.name);
                let name = Path::new(&entry.name)
                    .to_string_lossy()
                    .strip_suffix(".toml")?
                    .to_owned();
                let theme = self.own_theme(&name, &file);
                Some(Entry {
                    name,
                    origin: Origin::File(file),
                    theme,
                })
            })
            .collect();
        (entries, None)
    }

    /// The user theme `name` in `file`, refused when its name is a built-in's or no
    /// theme's.
    fn own_theme(&self, name: &str, file: &Path) -> Result<Theme, ThemeError> {
        let refuse = |message: String| ThemeError {
            file: Some(file.to_owned()),
            position: None,
            message,
        };
        if !valid_name(name) {
            return Err(refuse(
                "a theme's file name is its name: lowercase letters, digits and -".to_owned(),
            ));
        }
        if builtin_text(name).is_some() {
            return Err(refuse(format!(
                "{name} is a built-in theme's name; give this file another name"
            )));
        }
        self.resolve(name)
    }

    /// The text of the theme `name` and where it comes from; `None` when there is none.
    fn source(&self, name: &str) -> Result<Option<(Cow<'static, str>, Origin)>, ThemeError> {
        if let Some(text) = builtin_text(name) {
            return Ok(Some((Cow::Borrowed(text), Origin::Builtin)));
        }
        let Ok(dir) = &self.dir else {
            return Ok(None);
        };
        if !valid_name(name) {
            return Ok(None);
        }
        let file = dir.join(format!("{name}.toml"));
        match read_config(&file) {
            Ok(Some(text)) => Ok(Some((Cow::Owned(text), Origin::File(file)))),
            Ok(None) => Ok(None),
            Err(e) => Err(ThemeError {
                file: Some(file),
                position: None,
                message: format!("it can't be read: {e}"),
            }),
        }
    }

    /// The theme `name` whose file says `text`, over the theme it extends. `chain` holds
    /// the themes that extend it, to refuse a cycle.
    fn build(
        &self,
        name: &str,
        text: &str,
        origin: &Origin,
        chain: &mut Vec<String>,
    ) -> Result<Theme, ThemeError> {
        let file = origin.path();
        let parsed = parse::parse(text).map_err(|e| e.in_file(file))?;
        let Some((base, at)) = &parsed.extends else {
            return Ok(parsed.over(Theme::default()));
        };
        chain.push(name.to_owned());
        let refuse = |message: String| ThemeError {
            file: file.cloned(),
            position: Some(*at),
            message,
        };
        if chain.contains(base) {
            let cycle = format!("{} -> {base}", chain.join(" -> "));
            return Err(refuse(format!("extends {base}, a cycle: {cycle}")));
        }
        let Some((base_text, base_origin)) = self.source(base)? else {
            return Err(refuse(format!(
                "extends {base}, but there is no theme {base}"
            )));
        };
        let under = self.build(base, &base_text, &base_origin, chain)?;
        Ok(parsed.over(under))
    }
}

#[cfg(test)]
impl Themes {
    /// The built-ins and the user themes in `dir`.
    pub(crate) fn in_dir(dir: &Path) -> Self {
        Self {
            dir: Ok(dir.to_owned()),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use ratatui::style::{Color, Style};

    use super::*;
    use crate::theme::Role;

    fn themes(files: &[(&str, &str)]) -> (tempfile::TempDir, Themes) {
        let temp = tempfile::tempdir().unwrap();
        for (name, text) in files {
            fs::write(temp.path().join(name), text).unwrap();
        }
        let themes = Themes::in_dir(temp.path());
        (temp, themes)
    }

    // Catches `extends` dropping the base's roles, or a role set in the file patched
    // over the base's style instead of replacing it: exactly the two named roles change.
    #[test]
    fn a_theme_that_extends_another_changes_exactly_its_own_roles() {
        let dusk = "version = 1\nextends = \"kipple\"\n[roles]\n\
                    safe = { fg = \"#7FD47F\" }\n\"bar.safe\" = { bold = true }\n";
        let (_temp, themes) = themes(&[("dusk.toml", dusk)]);
        let kipple = Theme::builtin("kipple").unwrap();
        let theme = themes.resolve("dusk").unwrap();
        for role in Role::ALL {
            let want = match role {
                Role::Safe => Style::new().fg(Color::Rgb(0x7F, 0xD4, 0x7F)),
                Role::BarSafe => Style::new().add_modifier(ratatui::style::Modifier::BOLD),
                Role::Text
                | Role::Muted
                | Role::Accent
                | Role::Border
                | Role::Title
                | Role::Label
                | Role::Cursor
                | Role::Selected
                | Role::Review
                | Role::Protected
                | Role::Warning
                | Role::Error
                | Role::BarReview
                | Role::BarProtected
                | Role::BarTrack
                | Role::Key
                | Role::KeyLabel
                | Role::LevelCurrent
                | Role::LevelOther
                | Role::Kept => kipple.style(role),
            };
            assert_eq!(theme.style(role), want, "{}", role.name());
        }
    }

    // Catches a chain of user themes resolved in the wrong order: the nearest theme's
    // role wins.
    #[test]
    fn the_nearest_theme_in_a_chain_wins() {
        let (_temp, themes) = themes(&[
            (
                "a.toml",
                "version = 1\nextends = \"b\"\n[roles]\nsafe = { fg = 1 }\n",
            ),
            (
                "b.toml",
                "version = 1\nextends = \"mono\"\n[roles]\nsafe = { fg = 2 }\nreview = { fg = 3 }\n",
            ),
        ]);
        let theme = themes.resolve("a").unwrap();
        assert_eq!(theme.style(Role::Safe).fg, Some(Color::Indexed(1)));
        assert_eq!(theme.style(Role::Review).fg, Some(Color::Indexed(3)));
        assert_eq!(
            theme.style(Role::Title),
            Theme::builtin("mono").unwrap().style(Role::Title)
        );
    }

    // Catches a cycle that loops forever or passes, and a missing base that is taken as
    // empty: both are errors at the `extends` that causes them, in that file.
    #[test]
    fn a_cycle_or_a_missing_base_is_an_error_at_its_extends() {
        let (temp, themes) = themes(&[
            ("a.toml", "version = 1\nextends = \"b\"\n"),
            ("b.toml", "version = 1\n\nextends = \"a\"\n"),
            ("self.toml", "version = 1\nextends = \"self\"\n"),
            ("orphan.toml", "version = 1\nextends = \"nowhere\"\n"),
        ]);
        let error = themes.resolve("a").unwrap_err();
        assert_eq!(
            error.file.as_deref(),
            Some(temp.path().join("b.toml").as_path())
        );
        assert_eq!(error.position, Some((3, 11)));
        assert!(error.message.contains("a -> b -> a"), "{error}");

        let itself = themes.resolve("self").unwrap_err();
        assert!(itself.message.contains("cycle"), "{itself}");

        let orphan = themes.resolve("orphan").unwrap_err();
        assert_eq!(orphan.position, Some((2, 11)));
        assert!(orphan.message.contains("no theme nowhere"), "{orphan}");
    }

    // Catches a mistake in a user theme reported without its file, which leaves the
    // user guessing which theme is broken.
    #[test]
    fn a_bad_colour_is_reported_with_its_file_line_and_column() {
        let (temp, themes) = themes(&[(
            "dusk.toml",
            "version = 1\n[roles]\nsafe = { fg = \"grean\" }\n",
        )]);
        let error = themes.resolve("dusk").unwrap_err();
        let file = temp.path().join("dusk.toml");
        assert_eq!(error.file.as_deref(), Some(file.as_path()));
        assert_eq!(error.position, Some((3, 15)));
        let shown = error.to_string();
        assert!(
            shown.contains("line 3, column 15: safe.fg: expected"),
            "{shown}"
        );
    }

    // Catches a user file taking a built-in's name, which would leave two themes called
    // `kipple`, and a file whose name no theme can have; and a missing theme, which
    // must say where the user would put it.
    #[test]
    fn the_catalog_refuses_a_built_in_name_and_a_bad_file_name() {
        let (temp, themes) = themes(&[
            ("kipple.toml", "version = 1\n"),
            ("Dusk.toml", "version = 1\n"),
            ("dawn.toml", "version = 1\n"),
            ("notes.txt", "not a theme"),
        ]);
        fs::create_dir(temp.path().join("dir.toml")).unwrap();
        let catalog = themes.catalog();
        let names: Vec<&str> = catalog.entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "kipple",
                "kipple-dark",
                "kipple-light",
                "high-contrast",
                "mono",
                "Dusk",
                "dawn",
                "kipple"
            ]
        );
        let own = |name: &str| {
            catalog
                .entries
                .iter()
                .find(|e| e.name == name && e.origin != Origin::Builtin)
                .unwrap()
        };
        assert!(
            own("kipple")
                .theme
                .as_ref()
                .unwrap_err()
                .message
                .contains("built-in")
        );
        assert!(own("Dusk").theme.is_err());
        assert!(own("dawn").theme.is_ok());
        // The built-in keeps its name.
        assert_eq!(
            themes.resolve("kipple").unwrap(),
            Theme::builtin("kipple").unwrap()
        );

        let missing = themes.resolve("nowhere").unwrap_err();
        assert!(missing.message.contains("nowhere.toml"), "{missing}");
    }

    // Catches a missing theme directory treated as a problem: most users have none.
    #[test]
    fn no_theme_directory_lists_the_built_ins_and_no_problem() {
        let temp = tempfile::tempdir().unwrap();
        let catalog = Themes::in_dir(&temp.path().join("themes")).catalog();
        assert_eq!(catalog.entries.len(), BUILTIN.len());
        assert_eq!(catalog.problem, None);
        assert!(catalog.entries.iter().all(|e| e.theme.is_ok()));
    }
}
