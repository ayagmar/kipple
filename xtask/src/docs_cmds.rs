//! `cargo xtask check-docs`: every `kipple` command the README, the docs and the site show
//! in a shell block parses against the real CLI (`docs/08-engineering-standards.md` §1).
//!
//! A fenced `sh`, `shell`, `bash` or `zsh` block, and the `$ ` lines of a `console` block,
//! are commands someone can run today. Planned commands go in `text` blocks, which this
//! check skips, as does inline code in prose.

use std::fs;
use std::io::{self, Write as _};
use std::iter::Peekable;
use std::mem;
use std::path::{Path, PathBuf};
use std::str::Chars;

use anyhow::{Context, Result, bail};
use clap::CommandFactory as _;
use clap::error::ErrorKind;

/// The files and directories whose Markdown is checked, relative to the workspace root.
const SOURCES: &[&str] = &["README.md", "docs", "site/src"];

/// The program whose command lines are checked.
const PROGRAM: &str = "kipple";

/// A Markdown file, with its path relative to the workspace root.
#[derive(Debug)]
struct Doc {
    path: PathBuf,
    contents: String,
}

/// How a fenced block's lines are commands.
#[derive(Debug, Clone, Copy)]
enum Kind {
    /// Every line is a command.
    Shell,
    /// Only the lines after a `$ ` prompt are commands; the rest is output.
    Console,
}

/// A fence's opening line: its character and length, and the block's language.
#[derive(Debug)]
struct Fence {
    marker: char,
    len: usize,
    kind: Option<Kind>,
}

/// The lines of a shell or console block.
#[derive(Debug)]
struct Block<'doc> {
    kind: Kind,
    /// The 1-based line number of the block's first line.
    start: usize,
    lines: Vec<&'doc str>,
}

/// One `kipple` command line found in a doc.
#[derive(Debug)]
struct Invocation<'doc> {
    path: &'doc Path,
    line: usize,
    words: Vec<String>,
}

/// Fails if a documented `kipple` command doesn't parse, or if the docs show none.
pub(crate) fn run(root: &Path) -> Result<()> {
    let docs = markdown_files(root)?;
    let invocations: Vec<Invocation<'_>> = docs.iter().flat_map(invocations).collect();
    if invocations.is_empty() {
        bail!(
            "found no `{PROGRAM}` command in a shell block of {}, so nothing was checked",
            SOURCES.join(", ")
        );
    }
    let violations = violations(&invocations);
    if violations.is_empty() {
        return Ok(());
    }
    let mut stderr = io::stderr().lock();
    for violation in &violations {
        writeln!(stderr, "check-docs: {violation}")?;
    }
    bail!(
        "{} documented command(s) don't parse, see docs/08-engineering-standards.md §1",
        violations.len()
    )
}

/// Every invocation that the real CLI rejects, as `path:line: `command`: why`.
fn violations(invocations: &[Invocation<'_>]) -> Vec<String> {
    invocations
        .iter()
        .filter_map(|invocation| {
            let problem = problem(&invocation.words)?;
            Some(format!(
                "{}:{}: `{}`: {problem}",
                invocation.path.display(),
                invocation.line,
                invocation.words.join(" ")
            ))
        })
        .collect()
}

/// Why clap rejects `words`, or `None` when it parses them (`--help` and `--version` count).
fn problem(words: &[String]) -> Option<String> {
    let error = kipple::Cli::command().try_get_matches_from(words).err()?;
    if matches!(
        error.kind(),
        ErrorKind::DisplayHelp | ErrorKind::DisplayVersion
    ) {
        return None;
    }
    let message = error.to_string();
    let first = message.lines().next().unwrap_or_default();
    Some(first.trim_start_matches("error: ").to_owned())
}

/// The `kipple` command lines in the doc's shell and console blocks.
fn invocations(doc: &Doc) -> Vec<Invocation<'_>> {
    let mut found = Vec::new();
    for block in shell_blocks(&doc.contents) {
        for (line, text) in block_commands(&block) {
            found.extend(
                simple_commands(&text)
                    .into_iter()
                    .filter_map(kipple_words)
                    .map(|words| Invocation {
                        path: &doc.path,
                        line,
                        words,
                    }),
            );
        }
    }
    found
}

/// The command's words from `kipple` on, after any `NAME=value` assignments, if it runs
/// `kipple`.
fn kipple_words(mut words: Vec<String>) -> Option<Vec<String>> {
    let start = words.iter().position(|word| !is_assignment(word))?;
    words.drain(..start);
    (words.first()? == PROGRAM).then_some(words)
}

fn is_assignment(word: &str) -> bool {
    word.split_once('=').is_some_and(|(name, _)| {
        name.chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
            && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
    })
}

/// The fenced blocks whose language makes their lines commands. A fence closes on a line of
/// at least as many of the same character; one left open runs to the end of the doc.
fn shell_blocks(contents: &str) -> Vec<Block<'_>> {
    let mut blocks = Vec::new();
    let mut open: Option<(Fence, Option<Block<'_>>)> = None;
    for (index, line) in contents.lines().enumerate() {
        let Some((fence, block)) = &mut open else {
            open = opening(line).map(|fence| {
                let block = fence.kind.map(|kind| Block {
                    kind,
                    start: index + 2,
                    lines: Vec::new(),
                });
                (fence, block)
            });
            continue;
        };
        if closes(fence, line) {
            blocks.extend(open.take().and_then(|(_, closed)| closed));
        } else if let Some(block) = block {
            block.lines.push(line);
        }
    }
    blocks.extend(open.and_then(|(_, block)| block));
    blocks
}

/// The fence this line opens, if it opens one: three or more backticks or tildes, then the
/// language.
fn opening(line: &str) -> Option<Fence> {
    let trimmed = line.trim_start();
    let marker = trimmed.chars().next().filter(|c| matches!(c, '`' | '~'))?;
    let len = trimmed.chars().take_while(|&c| c == marker).count();
    if len < 3 {
        return None;
    }
    let info = trimmed.trim_start_matches(marker);
    // Inline code such as ```` ```x``` ````, not a fence.
    if marker == '`' && info.contains('`') {
        return None;
    }
    let kind = match info.split_whitespace().next().unwrap_or_default() {
        "sh" | "shell" | "bash" | "zsh" => Some(Kind::Shell),
        "console" => Some(Kind::Console),
        _ => None,
    };
    Some(Fence { marker, len, kind })
}

fn closes(fence: &Fence, line: &str) -> bool {
    let trimmed = line.trim();
    trimmed.chars().count() >= fence.len && trimmed.chars().all(|c| c == fence.marker)
}

/// The block's command lines, each with the line it starts on. A line ending in `\`
/// continues on the next.
fn block_commands(block: &Block<'_>) -> Vec<(usize, String)> {
    let mut commands = Vec::new();
    let mut current: Option<(usize, String)> = None;
    for (offset, raw) in block.lines.iter().enumerate() {
        let raw = raw.trim_start();
        let text = match (&current, block.kind) {
            (Some(_), _) | (None, Kind::Shell) => raw,
            (None, Kind::Console) => match raw.strip_prefix("$ ") {
                Some(command) => command,
                None => continue,
            },
        };
        let (text, continues) = text
            .strip_suffix('\\')
            .map_or((text, false), |text| (text, true));
        let (_, command) = current.get_or_insert_with(|| (block.start + offset, String::new()));
        command.push_str(text);
        command.push(' ');
        if !continues {
            commands.extend(current.take());
        }
    }
    commands.extend(current);
    commands
}

/// Splits a shell command line into its simple commands, with quotes removed: the words
/// between `|`, `&`, `;`, parentheses, backticks and `$(`. A redirection and its target are
/// left out, and a `#` that starts a word starts a comment.
fn simple_commands(line: &str) -> Vec<Vec<String>> {
    let mut lexer = Lexer::default();
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\'' => lexer.single_quoted(&mut chars),
            '"' => lexer.double_quoted(&mut chars),
            '\\' => lexer.escaped(chars.next()),
            '#' if lexer.word.is_none() => break,
            '<' | '>' => {
                lexer.redirect();
                while chars
                    .next_if(|next| matches!(next, '<' | '>' | '&' | '|'))
                    .is_some()
                {}
            }
            '$' if chars.next_if_eq(&'(').is_some() => lexer.end_command(),
            '|' | '&' | ';' | '(' | ')' | '`' => lexer.end_command(),
            c if c.is_whitespace() => lexer.end_word(),
            c => lexer.push(c),
        }
    }
    lexer.end_command();
    lexer.commands
}

/// The state of [`simple_commands`].
#[derive(Debug, Default)]
struct Lexer {
    commands: Vec<Vec<String>>,
    words: Vec<String>,
    /// The word being read. `Some("")` after `''` or `""`, an empty word.
    word: Option<String>,
    /// Whether the next word is a redirection's target.
    redirect: bool,
}

impl Lexer {
    fn push(&mut self, c: char) {
        self.word.get_or_insert_default().push(c);
    }

    fn escaped(&mut self, next: Option<char>) {
        if let Some(c) = next {
            self.push(c);
        }
    }

    fn single_quoted(&mut self, chars: &mut Peekable<Chars<'_>>) {
        let word = self.word.get_or_insert_default();
        word.extend(chars.by_ref().take_while(|&c| c != '\''));
    }

    fn double_quoted(&mut self, chars: &mut Peekable<Chars<'_>>) {
        let word = self.word.get_or_insert_default();
        while let Some(c) = chars.next() {
            match c {
                '"' => return,
                '\\' => match chars.next_if(|next| matches!(next, '"' | '\\' | '$' | '`')) {
                    Some(escaped) => word.push(escaped),
                    None => word.push('\\'),
                },
                c => word.push(c),
            }
        }
    }

    fn end_word(&mut self) {
        if let Some(word) = self.word.take()
            && !mem::take(&mut self.redirect)
        {
            self.words.push(word);
        }
    }

    fn end_command(&mut self) {
        self.end_word();
        let words = mem::take(&mut self.words);
        if !words.is_empty() {
            self.commands.push(words);
        }
    }

    /// Starts a redirection. A file descriptor number right before it, as in `2>`, is
    /// part of it.
    fn redirect(&mut self) {
        if self
            .word
            .as_deref()
            .is_some_and(|word| !word.is_empty() && word.bytes().all(|b| b.is_ascii_digit()))
        {
            self.word = None;
        }
        self.end_word();
        self.redirect = true;
    }
}

/// The Markdown files under [`SOURCES`], sorted by path.
fn markdown_files(root: &Path) -> Result<Vec<Doc>> {
    let mut docs = Vec::new();
    let mut pending: Vec<PathBuf> = SOURCES.iter().map(|source| root.join(source)).collect();
    while let Some(path) = pending.pop() {
        if path.is_dir() {
            pending.extend(markdown_children(&path)?);
            continue;
        }
        let contents = fs::read_to_string(&path)
            .with_context(|| format!("could not read {}", path.display()))?;
        let path = path.strip_prefix(root).unwrap_or(&path).to_path_buf();
        docs.push(Doc { path, contents });
    }
    docs.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(docs)
}

/// The directory's subdirectories and Markdown files.
fn markdown_children(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut children = Vec::new();
    for entry in fs::read_dir(dir).with_context(|| format!("could not read {}", dir.display()))? {
        let child = entry
            .with_context(|| format!("could not read {}", dir.display()))?
            .path();
        if child.is_dir() || child.extension().is_some_and(|e| e == "md" || e == "mdx") {
            children.push(child);
        }
    }
    Ok(children)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(contents: &str) -> Doc {
        Doc {
            path: PathBuf::from("README.md"),
            contents: contents.to_owned(),
        }
    }

    fn found(contents: &str) -> Vec<String> {
        invocations(&doc(contents))
            .iter()
            .map(|invocation| invocation.words.join(" "))
            .collect()
    }

    fn problems(contents: &str) -> Vec<String> {
        violations(&invocations(&doc(contents)))
    }

    // Catches a doc that names a command the CLI doesn't have, such as a planned one
    // written as if it existed, including in a block after a line that only looks like a
    // fence.
    #[test]
    fn an_unknown_subcommand_fails() {
        let problems =
            problems("```x``` is inline\n\n```sh\nkipple doctor\nkipple frobnicate --yes\n```\n");
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert!(
            problems[0].starts_with("README.md:5: `kipple frobnicate --yes`: "),
            "{problems:?}"
        );
        assert!(problems[0].contains("frobnicate"), "{problems:?}");
    }

    // Catches a flag renamed or removed from the CLI while a doc still shows it, including
    // in a console block and after other shell syntax on the line.
    #[test]
    fn an_unknown_flag_fails() {
        let problems = problems(
            "```console\n$ kipple scan --json\n$ cd ~ && kipple scan --frobnicate | jq .\nkipple scan --output-is-not-checked\n```\n",
        );
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert!(
            problems[0].starts_with("README.md:3: `kipple scan --frobnicate`: "),
            "{problems:?}"
        );
    }

    // Catches the check rejecting a correct line because shell syntax around the command
    // (assignments, redirections, pipes, comments, continuations, quotes) reached clap.
    #[test]
    fn a_valid_line_passes() {
        let contents = "~~~bash\n\
            kipple\n\
            KIPPLE_CONFIG=demo.toml kipple scan --json 2>/dev/null > out.json # read-only\n\
            kipple scan --root ~/code \\\n  --root \"$HOME/My Projects\" --events jsonl\n\
            kipple explain 'builtin.cargo.target' && kipple doctor --json >&2\n\
            file=\"$(dirname x)\"; kipple config show\n\
            kipple theme show dracula\n\
            kipple scan --help\n\
            ~~~\n";
        assert_eq!(problems(contents), Vec::<String>::new());
        assert_eq!(
            found(contents),
            [
                "kipple",
                "kipple scan --json",
                "kipple scan --root ~/code --root $HOME/My Projects --events jsonl",
                "kipple explain builtin.cargo.target",
                "kipple doctor --json",
                "kipple config show",
                "kipple theme show dracula",
                "kipple scan --help",
            ]
        );
    }

    // Catches the spec docs' planned commands (`text` blocks) and prose failing the gate,
    // and a command that is only an argument of another (`echo kipple ...`) being checked.
    #[test]
    fn planned_commands_and_prose_are_not_checked() {
        let contents = "`kipple clean --yes` is planned.\n\n```text\nkipple clean --yes\n```\n\n\
            ````markdown\n```sh\nkipple restore RECEIPT\n```\n````\n\n\
            ```sh\necho kipple clean\n```\n";
        assert_eq!(found(contents), Vec::<String>::new());
    }

    // Catches the docs being missed (README, `docs/`, the site's `.mdx`), and the gate
    // passing because it found nothing to check.
    #[test]
    fn run_reads_every_source_and_fails_on_none() {
        let root = tempfile::tempdir().unwrap();
        let write = |path: &str, contents: &str| {
            let path = root.path().join(path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, contents).unwrap();
        };
        write("README.md", "No commands.\n");
        write("docs/notes.txt", "```sh\nkipple frobnicate\n```\n");
        write(
            "site/src/generated/cli.md",
            "```text\nkipple frobnicate\n```\n",
        );
        let nothing = run(root.path()).unwrap_err().to_string();
        assert!(nothing.contains("found no `kipple` command"), "{nothing}");

        write("README.md", "```sh\nkipple doctor\n```\n");
        write("docs/03-spec.md", "```sh\nkipple frobnicate\n```\n");
        write(
            "site/src/content/docs/start.mdx",
            "```sh\nkipple scan --nope\n```\n",
        );
        let broken = run(root.path()).unwrap_err().to_string();
        assert!(broken.starts_with("2 documented command(s)"), "{broken}");
    }
}
