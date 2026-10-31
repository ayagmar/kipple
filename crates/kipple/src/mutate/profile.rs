//! `kipple profile` (03 J6, spec M3c pinned 10–13, D-068): the scopes
//! `clean --profile NAME --yes` runs unattended.
//!
//! `create` and `review` scan, show what they would save and what it would clean today,
//! and save exactly that once a person says yes at a terminal. `list` checks each profile
//! against the config without scanning. `remove` only takes authority away, so it asks
//! nothing. The core freezes, checks and encodes; the platform's store moves the bytes.

use std::collections::BTreeSet;
use std::io::{self, Write};
use std::process::ExitCode;

use clap::{Args, Subcommand};
use kipple_core::format::IntegrationDigests;
use kipple_core::profile::{
    self, DraftProfile, EntryScope, FreezeError, Profile, ProfileCheck, ProfileContents,
    ProfileEntry, ProfileError, ProfileName, ProfileScene, ProfileStore as _, ProfileStoreError,
};
use kipple_core::{
    CancelToken, CleanRequest, Ending, Finding, Finished, PlanMode, Policy, Receipt, RuleId,
    ScanReport, Selection, Verdict, prepare_clean,
};
use kipple_platform::OsNames;

use super::output::{Line, Shown, Told};
use super::{BLOCKED, Format, Scanned, machine, only, prompt, scan_with};
use crate::human::profile as words;
use crate::machine::Machine;
use crate::theme::{Ink, Role};
use crate::{CANCELLED, Host, Io, Person, USAGE, say};

/// `kipple profile` actions.
#[derive(Debug, Subcommand)]
pub enum ProfileAction {
    /// Save what `clean --profile NAME --yes` may clean: the Safe findings of each rule as
    /// the config and a scan judge them now. Shows it and asks first, at a terminal.
    Create(CreateArgs),
    /// Show what changed since a profile was saved, and save it again as it is now. Shows
    /// it and asks first, at a terminal.
    Review(ReviewArgs),
    /// List the profiles, each with how many of its rules are active, suspended and
    /// pending. Read-only, and without a scan.
    List,
    /// Remove a profile. Nothing else changes.
    Remove {
        /// The profile's name.
        #[arg(value_name = "NAME")]
        name: String,
    },
}

/// `kipple profile create` options.
#[derive(Debug, Args)]
pub struct CreateArgs {
    /// Its name: up to 64 lowercase letters, digits and `-`.
    #[arg(value_name = "NAME")]
    pub name: String,
    /// Take what is Safe now, rule by rule.
    #[arg(long, required = true)]
    pub from_safe: bool,
    /// Only this rule. Repeat for several. The other rules are saved as left out, which a
    /// review adds only when named with `--add`.
    #[arg(long = "only", value_name = "RULE")]
    pub only: Vec<String>,
}

/// `kipple profile review` options.
#[derive(Debug, Args)]
pub struct ReviewArgs {
    /// The profile's name.
    #[arg(value_name = "NAME")]
    pub name: String,
    /// Include this rule too: one new since the profile was saved, or one `--only` left
    /// out. Repeat for several.
    #[arg(long = "add", value_name = "RULE")]
    pub add: Vec<String>,
}

/// `kipple profile ACTION`.
pub(crate) fn profile(host: &Host<'_>, action: &ProfileAction, io: &mut Io<'_>) -> ExitCode {
    let ran = match action {
        ProfileAction::Create(args) => create(host, args, io),
        ProfileAction::Review(args) => review(host, args, io),
        ProfileAction::List => list(host, io),
        ProfileAction::Remove { name } => remove(host, name, io),
    };
    ran.unwrap_or_else(|code| code)
}

/// `text` as a profile name, checked before anything is opened; a usage error said on
/// `stderr` when it isn't one.
pub(super) fn name(text: &str, stderr: &mut dyn Write) -> Result<ProfileName, ExitCode> {
    ProfileName::parse(text).ok_or_else(|| {
        say(stderr, &words::bad_name(text));
        ExitCode::from(USAGE)
    })
}

/// The profile `name`, read once from `machine`'s store; why it can't be on `stderr`.
pub(super) fn saved(
    machine: &Machine,
    name: &ProfileName,
    stderr: &mut dyn Write,
) -> Result<Profile, ExitCode> {
    profile::read(machine.profiles(), name, &OsNames).map_err(|error| refused(name, &error, stderr))
}

/// `saved` checked against `machine`'s config and the scan `report`: what of it is
/// active now. Each entry it leaves out, and why, is said on `stderr`.
pub(super) fn checked(
    saved: &Profile,
    machine: &Machine,
    report: &ScanReport,
    stderr: &mut dyn Write,
) -> ProfileCheck {
    let integrations = machine.engine().integration_digests();
    let check = profile::check(saved, &scene(machine, report, &integrations));
    for line in left_out(&check) {
        say(stderr, &line);
    }
    check
}

/// How a profile run whose `check` left no entry active ends: nothing ran, said on
/// `stderr` with how to make it run again, and the receipt names what was left out.
pub(super) fn none_active(check: &ProfileCheck, stderr: &mut dyn Write) -> Finished {
    say(stderr, &words::none_active(check.name().as_str()));
    Finished {
        ending: Ending::NoneRan,
        run: None,
        receipt: Receipt::default(),
        dropped: Vec::new(),
        refused: Vec::new(),
        profile: Some(check.summary()),
    }
}

/// What a profile is frozen and checked against: `machine`'s config and roots, the scan
/// `report`, and each integration's digest.
fn scene<'now>(
    machine: &'now Machine,
    report: &'now ScanReport,
    integrations: &'now IntegrationDigests,
) -> ProfileScene<'now> {
    ProfileScene {
        judge: machine.judge(),
        scan: report,
        roots: &machine.settings().roots,
        integrations,
    }
}

/// What a check leaves out: each suspended entry with what changed, each rule new since
/// the profile was saved, and each rule kipple no longer has.
fn left_out(check: &ProfileCheck) -> Vec<String> {
    let suspended = check
        .suspended()
        .iter()
        .map(|one| words::suspended(&one.rule.0, &one.changes));
    let pending = check.pending().iter().map(|rule| words::pending(&rule.0));
    let gone = check.gone().iter().map(|rule| words::gone(&rule.0));
    suspended.chain(pending).chain(gone).collect()
}

/// Says why the profile `name` can't be used on `stderr`, and exits as that says: 2 for a
/// name or a file at fault, 1 for kipple's store.
fn refused(name: &ProfileName, error: &ProfileError, stderr: &mut dyn Write) -> ExitCode {
    say(
        stderr,
        &words::cant_use(name.as_str(), &words::error(error)),
    );
    match error {
        ProfileError::Store(
            ProfileStoreError::Missing | ProfileStoreError::Exists | ProfileStoreError::Read(_),
        )
        | ProfileError::Format(_)
        | ProfileError::Misnamed => ExitCode::from(USAGE),
        ProfileError::Store(
            ProfileStoreError::NoDataDir
            | ProfileStoreError::FileSystem(_)
            | ProfileStoreError::Write(_)
            | ProfileStoreError::Unsupported,
        )
        | ProfileError::ReadBack => ExitCode::FAILURE,
    }
}

/// Says why the profile `name` can't be frozen on `stderr`, and exits as that says: 2
/// for a rule named that can't be in it or nothing to include, 1 otherwise.
fn not_frozen(name: &ProfileName, error: &FreezeError, stderr: &mut dyn Write) -> ExitCode {
    say(
        stderr,
        &words::cant_use(name.as_str(), &words::freeze_error(error)),
    );
    match error {
        FreezeError::NotRemovable(_) | FreezeError::Empty => ExitCode::from(USAGE),
        FreezeError::ScanIncomplete | FreezeError::UnknownIntegration(_) => ExitCode::FAILURE,
    }
}

/// `kipple profile create NAME --from-safe [--only RULE]...`.
fn create(host: &Host<'_>, args: &CreateArgs, io: &mut Io<'_>) -> Result<ExitCode, ExitCode> {
    let name = name(&args.name, io.stderr)?;
    if io.person == Person::Absent {
        say(io.stderr, words::NEEDS_TERMINAL);
        return Err(ExitCode::from(BLOCKED));
    }
    let only = only(host, &args.only, io.stderr)?;
    let machine = machine(host, &[], None, io.stderr)?;
    // An existing name is refused before a scan that would only be thrown away.
    match machine.profiles().read(&name) {
        Ok(None) => {}
        Ok(Some(_)) => {
            let exists = ProfileError::Store(ProfileStoreError::Exists);
            return Err(refused(&name, &exists, io.stderr));
        }
        Err(why) => return Err(refused(&name, &ProfileError::Store(why), io.stderr)),
    }
    let scanned = scan_with(host, machine, Format::Human, io)?;
    let integrations = scanned.machine.engine().integration_digests();
    let scene = scene(&scanned.machine, &scanned.report, &integrations);
    let created = kipple_platform::clock(host.env).now();
    let draft = profile::freeze(name.clone(), &scene, created, only.as_ref())
        .map_err(|error| not_frozen(&name, &error, io.stderr))?;
    let ink = host.ink(io.stderr);
    if !confirmed(&draft, &scanned, &ink, io)? {
        return Ok(ExitCode::from(BLOCKED));
    }
    profile::create(scanned.machine.profiles(), &draft, &OsNames)
        .map_err(|error| refused(&name, &error, io.stderr))?;
    Ok(told(io.stderr, &words::saved(name.as_str())))
}

/// `kipple profile review NAME [--add RULE]...`.
fn review(host: &Host<'_>, args: &ReviewArgs, io: &mut Io<'_>) -> Result<ExitCode, ExitCode> {
    let name = name(&args.name, io.stderr)?;
    if io.person == Person::Absent {
        say(io.stderr, words::NEEDS_TERMINAL);
        return Err(ExitCode::from(BLOCKED));
    }
    let add: BTreeSet<RuleId> = only(host, &args.add, io.stderr)?.unwrap_or_default();
    let machine = machine(host, &[], None, io.stderr)?;
    let stored = saved(&machine, &name, io.stderr)?;
    let scanned = scan_with(host, machine, Format::Human, io)?;
    let integrations = scanned.machine.engine().integration_digests();
    let scene = scene(&scanned.machine, &scanned.report, &integrations);
    let reviewed = profile::review(&stored, &scene, &add)
        .map_err(|error| not_frozen(&name, &error, io.stderr))?;
    let ink = host.ink(io.stderr);
    let refrozen = reviewed
        .refrozen
        .iter()
        .map(|one| words::suspended(&one.rule.0, &one.changes));
    let added = reviewed.added.iter().map(|rule| words::added(&rule.0));
    let dropped = reviewed.dropped.iter().map(|rule| words::gone(&rule.0));
    let not_added = reviewed
        .not_added
        .iter()
        .map(|rule| words::pending(&rule.0));
    let changes: Vec<String> = refrozen.chain(added).chain(dropped).collect();
    let mut lines = Vec::new();
    if !changes.is_empty() {
        lines.push(Line::new(Role::Title, words::review_heading(name.as_str())));
    }
    lines.extend(
        changes
            .into_iter()
            .map(|text| Line::new(Role::Text, format!("  {text}"))),
    );
    lines.extend(not_added.map(|text| Line::new(Role::Muted, text)));
    let Some(draft) = reviewed.draft else {
        lines.push(Line::new(Role::Text, words::NOTHING_TO_REVIEW.to_owned()));
        write_lines(io.stderr, &lines, &ink).map_err(|_| ExitCode::from(CANCELLED))?;
        return Ok(ExitCode::SUCCESS);
    };
    write_lines(io.stderr, &lines, &ink).map_err(|_| ExitCode::from(CANCELLED))?;
    if !confirmed(&draft, &scanned, &ink, io)? {
        return Ok(ExitCode::from(BLOCKED));
    }
    profile::replace(scanned.machine.profiles(), &draft, &OsNames)
        .map_err(|error| refused(&name, &error, io.stderr))?;
    Ok(told(io.stderr, &words::saved(name.as_str())))
}

/// Shows `draft` and what it would clean today on stderr, asks whether to save it, and
/// reads the answer. A cancel at the question exits 130; anything but yes is a no.
fn confirmed(
    draft: &DraftProfile,
    scanned: &Scanned,
    ink: &Ink,
    io: &mut Io<'_>,
) -> Result<bool, ExitCode> {
    let contents = draft.contents();
    let mut lines = entry_lines(contents);
    lines.extend(today(contents, scanned, &io.cancel));
    if io.cancel.is_cancelled() {
        return Err(ExitCode::from(CANCELLED));
    }
    let methods = contents
        .entries()
        .iter()
        .filter(|entry| entry.scope() == EntryScope::Included)
        .map(ProfileEntry::method);
    lines.extend(prompt::method_lines(methods));
    let question = words::save_question(contents.name().as_str());
    let answered = write_lines(io.stderr, &lines, ink)
        .and_then(|()| prompt::answer(&mut *io.stdin, io.stderr, &question, &io.cancel));
    match answered {
        Ok(None) => Err(ExitCode::from(CANCELLED)),
        Ok(Some(answer)) if crate::human::receipt::is_yes(&answer) => Ok(true),
        // A question that can't be shown or answered is a no.
        Ok(Some(_)) | Err(_) => {
            told(io.stderr, words::NOT_SAVED);
            Ok(false)
        }
    }
}

/// Each entry of `contents` with the folders it looks in, then the kept and excluded
/// paths it holds.
fn entry_lines(contents: &ProfileContents) -> Vec<Line> {
    let mut lines = vec![Line::new(
        Role::Title,
        words::entries_heading(contents.name().as_str()),
    )];
    for entry in contents.entries() {
        lines.push(Line::new(Role::Text, words::entry_line(entry)));
        lines.extend(
            entry
                .roots()
                .iter()
                .map(|root| Line::new(Role::Muted, words::root_line(root))),
        );
    }
    lines.extend(
        contents
            .held()
            .iter()
            .map(|held| Line::new(Role::Text, words::held_line(held.kind(), held.path()))),
    );
    lines
}

/// What a run of `contents` would clean today, planned from `scanned` as a profile run
/// plans it: the Safe findings of its included rules below their frozen roots that
/// opened. Nothing is written.
fn today(contents: &ProfileContents, scanned: &Scanned, cancel: &CancelToken) -> Vec<Line> {
    let machine = &scanned.machine;
    let selection = selected(contents, &scanned.report, &machine.settings().policy);
    let request = CleanRequest {
        scan: &scanned.report,
        selection: &selection,
    };
    let prepared =
        machine.with_session(|session| prepare_clean(&request, session, PlanMode::Safe, cancel));
    if prepared.plan().entries().is_empty() {
        return vec![Line::new(Role::Text, words::NOTHING_TODAY.to_owned())];
    }
    let mut lines = vec![Line::new(Role::Title, words::TODAY.to_owned())];
    lines.extend(prompt::item_lines(prepared.plan()));
    let shown = Shown {
        review: &[],
        policy: &machine.settings().policy,
        told: Told::NotYet,
    };
    let refused = prepared.refused_items();
    lines.extend(super::output::left_out(refused, &[], &shown));
    lines
}

/// The findings of `report` a profile of `contents` selects: each of an included rule,
/// Safe under `policy`, below one of its rule's frozen roots that opened.
fn selected(contents: &ProfileContents, report: &ScanReport, policy: &Policy) -> Selection {
    let included = |finding: &Finding| {
        contents.entries().iter().any(|entry| {
            entry.rule() == &finding.rule
                && entry.scope() == EntryScope::Included
                && entry
                    .roots()
                    .iter()
                    .any(|root| root.identity().is_some() && finding.path.starts_with(root.path()))
        })
    };
    Selection(
        report
            .findings
            .iter()
            .filter(|finding| included(finding))
            .filter(|finding| finding.verdict(policy) == Some(Verdict::Safe))
            .map(|finding| finding.id)
            .collect(),
    )
}

/// `kipple profile list`: each profile checked against the config, without a scan.
fn list(host: &Host<'_>, io: &mut Io<'_>) -> Result<ExitCode, ExitCode> {
    let machine = machine(host, &[], None, io.stderr)?;
    let names = profile::list(machine.profiles()).map_err(|error| {
        say(io.stderr, &words::error(&error));
        match error {
            ProfileError::Store(ProfileStoreError::Read(_)) => ExitCode::from(USAGE),
            ProfileError::Store(_)
            | ProfileError::Format(_)
            | ProfileError::Misnamed
            | ProfileError::ReadBack => ExitCode::FAILURE,
        }
    })?;
    let integrations = machine.engine().integration_digests();
    let mut code = ExitCode::SUCCESS;
    let mut lines = Vec::new();
    for name in &names {
        let stored = match profile::read(machine.profiles(), name, &OsNames) {
            Ok(stored) => stored,
            Err(error) => {
                code = refused(name, &error, io.stderr);
                continue;
            }
        };
        let check = profile::check_config(&stored, machine.judge(), &integrations);
        let counts = words::list_line(
            name.as_str(),
            check.active().len(),
            check.suspended().len(),
            check.pending().len(),
        );
        lines.push(Line::new(Role::Title, counts));
        lines.extend(
            left_out(&check)
                .into_iter()
                .map(|why| Line::new(Role::Text, format!("  {why}"))),
        );
    }
    let last = if names.is_empty() {
        words::NO_PROFILES
    } else {
        words::ROOTS_UNCHECKED
    };
    lines.push(Line::new(Role::Muted, last.to_owned()));
    let written = write_lines(io.stdout, &lines, &host.ink(io.stderr));
    match crate::exit_code(written, io.stderr) {
        written if written == ExitCode::SUCCESS => Ok(code),
        written => Err(written),
    }
}

/// `kipple profile remove NAME`: it only takes authority away, so it asks nothing.
fn remove(host: &Host<'_>, text: &str, io: &mut Io<'_>) -> Result<ExitCode, ExitCode> {
    let name = name(text, io.stderr)?;
    let machine = machine(host, &[], None, io.stderr)?;
    profile::remove(machine.profiles(), &name)
        .map_err(|error| refused(&name, &error, io.stderr))?;
    Ok(told(io.stderr, &words::removed(name.as_str())))
}

/// Tells the person on `stderr` how the command ended, and exits 0.
fn told(stderr: &mut dyn Write, outcome: &str) -> ExitCode {
    writeln!(stderr, "{outcome}").ok();
    ExitCode::SUCCESS
}

/// Writes `lines` on `out`, painted.
fn write_lines(out: &mut dyn Write, lines: &[Line], ink: &Ink) -> io::Result<()> {
    for line in lines {
        writeln!(out, "{}", ink.paint(line.role, &line.text))?;
    }
    out.flush()
}
