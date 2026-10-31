//! A clean from the home screen (`docs/03-functional-spec.md` §2 J3, spec M3c pinned
//! 4–9 and 16): its transitions, and its review, typed size, progress and result at
//! 80×24 and 160×48. The plan is a real one: `demo-home` is built in a temp dir, every
//! root points into it through the `test-roots` override, and the planning and apply
//! workers' bodies run here, in turn, as the loop would run them. The sizes the scan
//! measured are replaced by fixed ones, and paths show from `~`, so the screens read the
//! same on every file system and system.

#[path = "../../../../../kipple-adapters/tests/fixtures/tree.rs"]
mod tree;

#[path = "../../../../../kipple-adapters/tests/fixtures/node/bun_v1_4_2.rs"]
mod bun_v1_4_2;
#[path = "../../../../../kipple-adapters/tests/fixtures/cargo/v1_99_0.rs"]
mod cargo_v1_99_0;
#[path = "../../../../../kipple-adapters/tests/fixtures/claude-code/settings.rs"]
mod claude_settings;
#[path = "../../../../../kipple-adapters/tests/fixtures/demo_home.rs"]
mod demo_home;
#[path = "../../../../../kipple-adapters/tests/fixtures/journald/v262.rs"]
mod journald_v262;
#[path = "../../../../../kipple-adapters/tests/fixtures/node/npm_v11_19_0.rs"]
mod npm_v11_19_0;
#[path = "../../../../../kipple-adapters/tests/fixtures/pacman/v7_1_0.rs"]
mod pacman_v7_1_0;
#[path = "../../../../../kipple-adapters/tests/fixtures/node/pnpm_v12_9_1.rs"]
mod pnpm_v12_9_1;
#[path = "../../../../../kipple-adapters/tests/fixtures/trash/freedesktop_v1_0.rs"]
mod trash_v1_0;
#[path = "../../../../../kipple-adapters/tests/fixtures/python/uv_v0_12_23.rs"]
mod uv_v0_12_23;
#[path = "../../../../../kipple-adapters/tests/fixtures/python/venv_v3_14_8.rs"]
mod venv_v3_14_8;

use std::ffi::OsString;
use std::fmt::Write as _;
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, SystemTime};

use kipple_core::{
    Allocated, Apparent, ApplyEvent, CancelToken, Completeness, Effect as Removal, Ending,
    EventChannel, Finding, FindingId, Finished, Found, ItemEvent, ItemId, ItemOutcome,
    JudgementField, Left, LeftCount, LeftReason, Method, Receipt, ReceiptItem, Reconciled, Removed,
    RunId, ScanEvent, ScanReport, ScanRequest, Skip, SpaceEstimate, event_channel,
};
use kipple_platform::Environment;
use tempfile::TempDir;

use super::{Paint, look, press, screen, to, typed};
use crate::config::{self, Settings};
use crate::human::{receipt, review};
use crate::setup::Builtin;
use crate::theme::Glyphs;
use crate::tui::app::{App, Catalog, Cleaning, Confirmation, Effect, Ended, Key, Msg, Saved};
use crate::tui::work::{self, Ground};

const MIB: u64 = 1024 * 1024;
const RUST_APP: &str = "home/projects/rust-app/target";
const ACME: &str = "home/projects/clients/acme/api/target";
/// Recent at Balanced: Review, with the `recent` caution.
const WEB: &str = "home/projects/web/node_modules";

/// `demo-home` in a temp dir, a roots file that points every root into it, and a config
/// file of its own.
struct Home {
    temp: TempDir,
    env: Environment,
    config: PathBuf,
    builtin: Builtin,
}

impl Home {
    fn new(config: &str) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let now = demo_home::looked_at();
        demo_home::tree(now).build_in(temp.path()).unwrap();
        let mut file = String::new();
        for (name, relative) in demo_home::ROOTS {
            writeln!(file, "{name} = {}", temp.path().join(relative).display()).unwrap();
        }
        let seconds = now
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        writeln!(file, "now = {seconds}").unwrap();
        let roots = temp.path().join("roots.txt");
        fs::write(&roots, file).unwrap();
        let config_file = temp.path().join("config.toml");
        // The roots file, the config named by `KIPPLE_CONFIG`, and `PATH`, so git can be
        // asked.
        let mut vars = vec![
            (OsString::from("KIPPLE_TEST_ROOTS"), roots.into_os_string()),
            (
                OsString::from(config::VAR),
                config_file.clone().into_os_string(),
            ),
        ];
        vars.extend(std::env::var_os("PATH").map(|path| (OsString::from("PATH"), path)));
        let home = Self {
            temp,
            env: Environment::from_vars(vars),
            config: config_file,
            builtin: Builtin::load().unwrap(),
        };
        home.write_config(config);
        home
    }

    /// Adds `count` more Rust projects, each with its `target`, under `projects/many`.
    fn more_projects(&self, count: usize) {
        let mut more = tree::FixtureTree::new();
        for at in 0..count {
            cargo_v1_99_0::project(&mut more, &format!("home/projects/many/p{at:02}"));
        }
        more.build_in(self.temp.path()).unwrap();
    }

    fn write_config(&self, text: &str) {
        fs::write(&self.config, text).unwrap();
    }

    fn at(&self, relative: &str) -> PathBuf {
        self.temp.path().join(relative)
    }

    /// Keeps the finding at `relative` besides what the config says, as `p` would.
    fn keep(&self, relative: &str) {
        let kept = self.at(relative);
        let mut text = fs::read_to_string(&self.config).unwrap();
        writeln!(text, "[keep]\npaths = [{:?}]", kept.to_str().unwrap()).unwrap();
        self.write_config(&text);
    }

    fn settings(&self) -> Settings {
        let catalog = config::catalog(&self.builtin);
        Settings::load(&self.env, None, &[], &catalog).unwrap()
    }

    /// A scan of its projects with the sizes each finding is given here: a size is the
    /// only fact the file system decides alone.
    fn scan(&self) -> ScanReport {
        let settings = self.settings();
        let engine = self.builtin.engine(&self.env).unwrap();
        let request = ScanRequest {
            roots: settings.roots,
            lens: None,
            exclude: settings.policy.exclude,
        };
        let cancel = CancelToken::new();
        let EventChannel {
            tx: events,
            rx: received,
        } = event_channel(256, &cancel);
        let mut report = thread::scope(|scope| {
            let scan = scope.spawn(|| {
                let events = events;
                engine.scan(&request, &events, &cancel)
            });
            received.for_each(drop);
            scan.join().unwrap()
        });
        // The projects' findings only, in path order: the system's differ by system.
        report.findings.retain(|finding| {
            finding
                .path
                .ancestors()
                .any(|above| above.ends_with("home/projects"))
        });
        report.findings.sort_by(|a, b| a.path.cmp(&b.path));
        for (finding, mib) in report.findings.iter_mut().zip((1..).map(|n: u64| 8 * n)) {
            finding.size = Some(SpaceEstimate {
                apparent: Apparent(mib * MIB),
                allocated: Some(Allocated(mib * MIB)),
                unique_reclaim: Some(Allocated(mib * MIB)),
                completeness: Completeness::Complete,
            });
        }
        report
    }

    /// The home screen while `report` streams in, on a `width` by `height` terminal.
    fn scanning(&self, report: &ScanReport, width: u16, height: u16) -> App {
        let mut app = App::new(Catalog::new(&self.builtin), width, height);
        app.configure(Saved::of(&self.settings()));
        for finding in &report.findings {
            let added = Finding {
                size: None,
                facts: None,
                ..finding.clone()
            };
            app.update(Msg::Scan(Box::new(ScanEvent::FindingAdded {
                finding: added,
            })));
            app.update(Msg::Scan(Box::new(ScanEvent::FindingUpdated {
                id: finding.id,
                size: finding.size,
                facts: finding.facts.clone().unwrap(),
            })));
        }
        app
    }

    /// The home screen once a scan has ended.
    fn app(&self, width: u16, height: u16) -> App {
        let report = self.scan();
        let mut app = self.scanning(&report, width, height);
        app.update(Msg::Finished {
            report: Arc::new(report),
            elapsed: Duration::from_millis(1_240),
        });
        app
    }

    fn ground(&self) -> Ground<'_> {
        Ground {
            builtin: &self.builtin,
            env: &self.env,
            settings: self.settings(),
        }
    }

    /// Answers the plan `app` asked for, as the planning worker would.
    fn plan(&self, app: &mut App) {
        let Some(Effect::Plan {
            generation,
            selection,
        }) = app.take_effect()
        else {
            panic!("no plan was asked for");
        };
        let report = Arc::clone(app.report().unwrap());
        let threshold = self.settings().loaded.config.confirm_by_typing_above;
        let prepared = work::plan(self.ground(), &report, &selection, &CancelToken::new()).unwrap();
        app.update(Msg::Planned {
            generation,
            prepared: Box::new(prepared),
            threshold,
        });
    }

    /// Runs the plan `app` confirmed, as the apply worker would, passing each of its
    /// events on as they arrive.
    fn apply(&self, app: &mut App) {
        assert!(
            matches!(app.take_effect(), Some(Effect::Apply(_))),
            "nothing was confirmed"
        );
        let prepared = Arc::clone(app.confirmed().unwrap());
        let (sender, events) = mpsc::channel();
        let finished = work::apply(self.ground(), &prepared, sender, &CancelToken::new()).unwrap();
        for event in events.try_iter() {
            app.update(Msg::Applied(Box::new(event)));
        }
        app.update(Msg::Ran(Box::new(finished)));
    }
}

/// The finding of `app`'s scan at `relative`.
fn finding(app: &App, relative: &str) -> FindingId {
    app.report()
        .unwrap()
        .findings
        .iter()
        .find(|finding| finding.path.ends_with(relative))
        .unwrap_or_else(|| panic!("{relative} wasn't found"))
        .id
}

/// `app` with the findings at `relative` selected.
fn select(app: &mut App, relative: &[&str]) {
    for wanted in relative {
        let id = finding(app, wanted);
        press(app, &[Key::Home]);
        to(app, id.0);
        press(app, &[Key::Char('s')]);
    }
}

/// The plan's paths, from the review, each as its last two components.
fn reviewed(app: &App) -> Vec<String> {
    let Some(Cleaning::Reviewing(review)) = &app.cleaning else {
        panic!("no review is open: {:?}", app.cleaning);
    };
    review
        .prepared
        .plan()
        .entries()
        .iter()
        .map(|entry| {
            let mut parts: Vec<_> = entry.path().iter().rev().take(2).collect();
            parts.reverse();
            parts
                .iter()
                .map(|part| part.to_string_lossy())
                .collect::<Vec<_>>()
                .join("/")
        })
        .collect()
}

/// A home with a Safe and a Review finding selected, and its review open.
fn reviewing(home: &Home, width: u16, height: u16) -> App {
    let mut app = home.app(width, height);
    select(&mut app, &[RUST_APP, WEB]);
    press(&mut app, &[Key::Enter]);
    home.plan(&mut app);
    app
}

fn text(app: &App, width: u16, height: u16) -> String {
    screen(app, look(Paint::Mono, Glyphs::Unicode), width, height)
}

// Catches a Review item never reaching the plan from the home screen (planned as Safe
// only, like the command line), or the review showing more or fewer items than the
// selection.
#[test]
fn enter_plans_the_selection_review_items_included_and_the_review_shows_that_plan() {
    let home = Home::new("");
    let app = reviewing(&home, 160, 48);

    assert_eq!(reviewed(&app), ["rust-app/target", "web/node_modules"]);
    let shown = text(&app, 160, 48);
    assert!(shown.contains("Review: clean 2 items"), "{shown}");
    assert!(shown.contains("needs review: recent"), "{shown}");
    assert!(shown.contains(receipt::DELETE_PERMANENT), "{shown}");
}

// Catches `y` running a plan other than the one shown, or apply never starting:
// confirming runs exactly the reviewed items and nothing else, and the result shows
// each of them done with the run's totals.
#[test]
fn y_runs_the_plan_shown_and_the_result_shows_its_receipt() {
    let home = Home::new("");
    let mut app = reviewing(&home, 160, 48);

    press(&mut app, &[Key::Char('y')]);
    home.apply(&mut app);

    assert!(!home.at(RUST_APP).exists(), "the Safe item stays");
    assert!(!home.at(WEB).exists(), "the Review item stays");
    assert!(home.at(ACME).exists(), "an item that wasn't selected went");
    let Some(Cleaning::Ended(Ended::Ran { finished, .. })) = &app.cleaning else {
        panic!("no result: {:?}", app.cleaning);
    };
    assert_eq!(finished.ending, Ending::Done);
    let shown = text(&app, 160, 48);
    assert!(shown.contains("Removed an estimated"), "{shown}");
    assert!(shown.contains("Done."), "{shown}");
}

// Catches `enter` planning a selection from a scan that hasn't ended, which has no
// report to plan from.
#[test]
fn enter_waits_for_the_scan_to_end() {
    let home = Home::new("");
    let report = home.scan();
    let id = report
        .findings
        .iter()
        .find(|finding| finding.path.ends_with(RUST_APP))
        .unwrap()
        .id;
    let mut app = home.scanning(&report, 160, 48);
    to(&mut app, id.0);
    press(&mut app, &[Key::Char('s')]);
    press(&mut app, &[Key::Enter]);
    assert_eq!(app.take_effect(), None);
    assert_eq!(app.notice.as_deref(), Some(review::REVIEW_AFTER_SCAN));
}

// Catches a plan asked for before `esc` opening a review after a new `enter`, which
// would show, and let `y` run, a plan of a selection that is no longer the one asked
// about.
#[test]
fn a_plan_answering_an_earlier_request_is_dropped() {
    let home = Home::new("");
    let mut app = home.app(160, 48);
    select(&mut app, &[RUST_APP]);
    press(&mut app, &[Key::Enter]);
    let Some(Effect::Plan {
        generation: first,
        selection,
    }) = app.take_effect()
    else {
        panic!("no plan was asked for");
    };
    press(&mut app, &[Key::Esc]);
    assert_eq!(app.take_effect(), Some(Effect::StopPlanning));
    assert!(app.cleaning.is_none(), "esc stayed in planning");
    assert_eq!(app.selected.len(), 1, "esc lost the selection");

    select(&mut app, &[ACME]);
    press(&mut app, &[Key::Enter]);
    let report = Arc::clone(app.report().unwrap());
    let stale = work::plan(home.ground(), &report, &selection, &CancelToken::new()).unwrap();
    app.update(Msg::Planned {
        generation: first,
        prepared: Box::new(stale),
        threshold: None,
    });
    assert!(
        matches!(app.cleaning, Some(Cleaning::Planning(latest)) if latest != first),
        "an earlier request's plan opened the review: {:?}",
        app.cleaning
    );

    home.plan(&mut app);
    assert_eq!(reviewed(&app), ["api/target", "rust-app/target"]);
}

// Catches quitting while planning waiting for the worker, or esc there leaving the
// person stuck on a screen with nothing to confirm.
#[test]
fn quitting_while_planning_quits_and_esc_goes_back_to_the_tree() {
    let home = Home::new("");
    for key in [Key::Char('q'), Key::Interrupt] {
        let mut app = home.app(160, 48);
        select(&mut app, &[RUST_APP]);
        press(&mut app, &[Key::Enter, key]);
        assert!(app.quit, "{key:?} while planning didn't quit");
    }
}

// Catches a typed size that isn't the one shown confirming, `y` alone confirming above
// the threshold, or the size shown failing to confirm when typed in another case.
#[test]
fn above_the_threshold_only_the_size_shown_typed_confirms() {
    let home = Home::new("[review]\nconfirm_by_typing_above = \"1 MB\"\n");
    let mut app = reviewing(&home, 160, 48);
    press(&mut app, &[Key::Char('y')]);
    let Some(Cleaning::Typing(review, _)) = &app.cleaning else {
        panic!("y didn't ask for the size: {:?}", app.cleaning);
    };
    let size = receipt::typed_size(&review.total);

    typed(&mut app, "1 MB");
    press(&mut app, &[Key::Enter]);
    assert_eq!(app.take_effect(), None);
    assert_eq!(app.notice.as_deref(), Some(receipt::NOT_THE_SIZE));
    assert!(
        matches!(app.cleaning, Some(Cleaning::Reviewing(_))),
        "the review didn't stay open: {:?}",
        app.cleaning
    );

    press(&mut app, &[Key::Char('y')]);
    typed(&mut app, &format!(" {} ", size.to_uppercase()));
    press(&mut app, &[Key::Enter]);
    assert_eq!(app.take_effect(), Some(Effect::Apply(Confirmation::Size)));
}

// Catches a config made stricter while the review was open being ignored: the loop says
// the size must now be typed, and `y` alone runs nothing.
#[test]
fn a_threshold_set_while_the_review_was_open_asks_for_the_size() {
    let home = Home::new("");
    let mut app = reviewing(&home, 160, 48);
    press(&mut app, &[Key::Char('y')]);
    assert_eq!(app.take_effect(), Some(Effect::Apply(Confirmation::Key)));

    app.update(Msg::MustType);

    assert!(
        matches!(app.cleaning, Some(Cleaning::Typing(..))),
        "y alone ran: {:?}",
        app.cleaning
    );
    assert_eq!(app.notice.as_deref(), Some(review::TYPE_THE_SIZE));
}

// Catches a refusal at confirm (the config no longer loads, the data folder unknown)
// losing the review, or showing another plan when the person comes back to it.
#[test]
fn a_refusal_at_confirm_goes_back_to_the_same_review_with_its_reason() {
    let home = Home::new("");
    let mut app = reviewing(&home, 160, 48);
    let Some(Cleaning::Reviewing(before)) = &app.cleaning else {
        panic!("no review");
    };
    let before = Arc::clone(&before.prepared);
    press(&mut app, &[Key::Char('y')]);

    app.update(Msg::NotApplied(review::not_run(
        "kipple's data folder isn't known",
    )));

    let Some(Cleaning::Reviewing(after)) = &app.cleaning else {
        panic!("the review closed: {:?}", app.cleaning);
    };
    assert!(
        Arc::ptr_eq(&before, &after.prepared),
        "another plan is shown"
    );
    let shown = text(&app, 160, 48);
    assert!(shown.contains("data folder isn't known"), "{shown}");
}

// Catches an empty plan (every selected finding refused) inviting a `y` that runs
// nothing, or hiding why nothing can be cleaned.
#[test]
fn an_empty_plan_says_why_and_y_does_nothing() {
    let home = Home::new("");
    let mut app = home.app(160, 48);
    select(&mut app, &[RUST_APP]);
    // Kept after it was selected: Protected when planned.
    home.keep(RUST_APP);
    press(&mut app, &[Key::Enter]);
    home.plan(&mut app);

    let shown = text(&app, 160, 48);
    assert!(shown.contains(review::NOTHING_TO_CLEAN), "{shown}");
    assert!(shown.contains(receipt::NOT_PLANNED), "{shown}");
    press(&mut app, &[Key::Char('y')]);
    assert_eq!(app.take_effect(), None);
    assert!(
        matches!(app.cleaning, Some(Cleaning::Reviewing(_))),
        "the review didn't stay open: {:?}",
        app.cleaning
    );
}

// Catches `y` running a plan whose permanence warning, cautions or refusals were never
// drawn: with more items than the review has rows, they still show on its first page,
// above the list.
#[test]
fn what_deleting_means_and_what_to_read_below_show_above_a_long_review() {
    let home = Home::new("");
    home.more_projects(30);
    let mut app = home.app(80, 24);
    select(&mut app, &[WEB]);
    // Kept after it was selected: Protected when planned.
    home.keep(ACME);
    press(&mut app, &[Key::Char('c')]);
    home.plan(&mut app);

    let shown = text(&app, 80, 24);
    let Some(Cleaning::Reviewing(review)) = &app.cleaning else {
        panic!("no review: {:?}", app.cleaning);
    };
    let last = review.prepared.plan().entries().last().unwrap().path();
    let last = last
        .parent()
        .unwrap()
        .file_name()
        .unwrap()
        .to_string_lossy();
    assert!(
        !shown.contains(&format!("{last}/target")),
        "the whole plan fits, so nothing is below the fold: {shown}"
    );
    assert!(shown.contains(receipt::DELETE_PERMANENT), "{shown}");
    assert!(shown.contains(&review::below(1, 1).unwrap()), "{shown}");
}

// Catches the key bar's selection total (which counts what planning refused) showing
// beside the plan's own: the person can't tell which one the run frees.
#[test]
fn a_clean_shows_what_its_plan_frees_and_not_the_selections_total() {
    let home = Home::new("");
    let app = reviewing_with_refused(&home, 80, 24);

    let shown = text(&app, 80, 24);
    assert!(shown.contains("delete 96.0 MiB in 2 items"), "{shown}");
    assert!(!shown.contains("frees"), "{shown}");
}

/// Whether `k` once, after `j` held far past the end, moves `app`'s view at `width` by
/// `height`.
fn k_after_many_j_moves(app: &mut App, width: u16, height: u16) -> bool {
    press(app, &[Key::Char('j'); 100]);
    let bottom = text(app, width, height);
    press(app, &[Key::Char('k')]);
    text(app, width, height) != bottom
}

// Catches the review's and the result's scroll growing past their last page: the view
// stops at the bottom, but `k` then does nothing visible for as many presses as `j` went
// too far, and a person scrolling back up to recheck what goes thinks the screen stuck.
#[test]
fn k_moves_the_review_and_the_result_back_at_once_after_scrolling_past_the_end() {
    let home = Home::new("");
    home.more_projects(30);
    let mut review = home.app(80, 24);
    press(&mut review, &[Key::Char('c')]);
    home.plan(&mut review);
    assert!(k_after_many_j_moves(&mut review, 80, 24), "the review");

    let mut result = reviewing(&home, 80, 12);
    let finished = finished(&result);
    press(&mut result, &[Key::Char('y')]);
    result.take_effect();
    result.update(Msg::Ran(Box::new(finished)));
    assert!(k_after_many_j_moves(&mut result, 80, 12), "the result");
}

/// A run's events as the loop passes them on: the run stored, its first item started.
fn started(app: &mut App, run: &RunId) {
    app.update(Msg::Applied(Box::new(ApplyEvent::RunStarted {
        run: run.clone(),
        items: 2,
    })));
    app.update(Msg::Applied(Box::new(ApplyEvent::Item {
        run: run.clone(),
        event: ItemEvent::Started { item: ItemId(0) },
    })));
}

fn run_id() -> RunId {
    RunId::at(
        SystemTime::UNIX_EPOCH + Duration::from_secs(1_793_000_000),
        0,
    )
}

// Catches `q` quitting mid-run, the first Ctrl+C not stopping new items, or the
// second not ending kipple with the run named for the next command to reconcile.
#[test]
fn during_apply_q_says_how_to_stop_and_ctrl_c_stops_then_ends_kipple() {
    let home = Home::new("");
    let mut app = reviewing(&home, 160, 48);
    press(&mut app, &[Key::Char('y')]);
    assert_eq!(app.take_effect(), Some(Effect::Apply(Confirmation::Key)));

    press(&mut app, &[Key::Interrupt]);
    press(&mut app, &[Key::Interrupt]);
    assert_eq!(app.take_effect(), Some(Effect::Abort));
    assert_eq!(
        app.run(),
        None,
        "before the run is stored there is no run to name"
    );

    let mut stored = reviewing(&home, 160, 48);
    press(&mut stored, &[Key::Char('y')]);
    stored.take_effect();
    started(&mut stored, &run_id());
    press(&mut stored, &[Key::Char('q'), Key::Esc]);
    assert!(!stored.quit, "q quit while the run went on");
    assert_eq!(stored.notice.as_deref(), Some(review::STOP_KEY));

    press(&mut stored, &[Key::Interrupt]);
    assert_eq!(stored.take_effect(), Some(Effect::StopApply));
    assert_eq!(stored.notice.as_deref(), Some(review::STOPPING));
    press(&mut stored, &[Key::Interrupt]);
    assert_eq!(stored.take_effect(), Some(Effect::Abort));
    assert_eq!(stored.run(), Some(&run_id()));
}

// Catches a worker's panic leaving the screen in progress forever, or a failed planner
// reading as a failed run when nothing was written.
#[test]
fn a_worker_that_failed_reads_as_failed_and_a_failed_planner_as_nothing_changed() {
    let home = Home::new("");
    let mut app = reviewing(&home, 160, 48);
    press(&mut app, &[Key::Char('y')]);
    app.take_effect();
    started(&mut app, &run_id());

    app.update(Msg::ApplyFailed);

    assert!(
        matches!(app.cleaning, Some(Cleaning::Ended(Ended::Failed { .. }))),
        "the failed run doesn't read as failed: {:?}",
        app.cleaning
    );
    let shown = text(&app, 160, 48);
    assert!(shown.contains(review::WORKER_FAILED), "{shown}");

    let mut planning = home.app(160, 48);
    select(&mut planning, &[RUST_APP]);
    press(&mut planning, &[Key::Enter]);
    let Some(Effect::Plan { generation, .. }) = planning.take_effect() else {
        panic!("no plan was asked for");
    };
    planning.update(Msg::PlanFailed { generation });
    assert!(planning.cleaning.is_none(), "still planning");
    assert_eq!(planning.notice.as_deref(), Some(review::PLANNING_FAILED));
}

// Catches the result keeping the old scan's findings, whose IDs no longer hold, or its
// selection, after the run removed some of them.
#[test]
fn leaving_the_result_clears_the_selection_and_scans_again() {
    let home = Home::new("");
    let mut app = reviewing(&home, 160, 48);
    press(&mut app, &[Key::Char('y')]);
    home.apply(&mut app);

    press(&mut app, &[Key::Enter]);

    assert_eq!(app.take_effect(), Some(Effect::Rescan));
    assert!(app.cleaning.is_none(), "the result stayed");
    assert!(app.selected.is_empty(), "the selection stayed");
    assert!(app.rows.is_empty(), "the old scan's findings stayed");
    assert!(app.report().is_none(), "the old scan's report stayed");
}

/// The review of [`reviewing`] with what planning left out: a selected finding kept
/// after it was selected.
fn reviewing_with_refused(home: &Home, width: u16, height: u16) -> App {
    let mut app = home.app(width, height);
    select(&mut app, &[RUST_APP, WEB, ACME]);
    home.keep(ACME);
    press(&mut app, &[Key::Enter]);
    home.plan(&mut app);
    app
}

/// A run of [`reviewing`]'s plan that removed its first item, left one locked file of
/// its second, and skipped a third as rejudged, as the receipt has them: fixed numbers,
/// so the screen doesn't depend on what the file system allocates.
fn finished(app: &App) -> Finished {
    let Some(Cleaning::Reviewing(review)) = &app.cleaning else {
        panic!("no review");
    };
    let entries = review.prepared.plan().entries();
    let removed = Removal {
        removed: Removed {
            names: 9,
            estimated: Allocated(8 * MIB),
            shared: 0,
            unmeasured: 0,
        },
        ..Removal::default()
    };
    let mut left = Left::default();
    left.0.insert(
        LeftReason::Busy,
        LeftCount {
            count: 1,
            sample: Vec::new(),
        },
    );
    let partial = Removal {
        left,
        ..removed.clone()
    };
    let item = |at: u32, outcome| {
        let entry = &entries[at as usize % entries.len()];
        ReceiptItem {
            item: ItemId(at),
            path: entry.path().clone(),
            method: Method::Delete,
            estimate: entry.estimate().copied(),
            outcome,
            restored: None,
        }
    };
    Finished {
        ending: Ending::Done,
        run: Some(run_id()),
        receipt: Receipt {
            items: vec![
                item(0, ItemOutcome::Done(removed)),
                item(1, ItemOutcome::Partial(partial)),
                item(
                    2,
                    ItemOutcome::Skipped(Skip::Rejudged(JudgementField::Level)),
                ),
            ],
        },
        dropped: Vec::new(),
        refused: Vec::new(),
        profile: None,
    }
}

/// [`reviewing`]'s plan running: the first item ended, the second under way.
fn applying(home: &Home, width: u16, height: u16) -> App {
    let mut app = reviewing(home, width, height);
    let done = finished(&app).receipt.items.remove(0).outcome;
    press(&mut app, &[Key::Char('y')]);
    app.take_effect();
    app.update(Msg::Applied(Box::new(ApplyEvent::Reconciled {
        run: RunId::at(
            SystemTime::UNIX_EPOCH + Duration::from_secs(1_792_000_000),
            0,
        ),
        reconciled: Reconciled::Item {
            item: ItemId(3),
            outcome: ItemOutcome::Indeterminate(Found::Gone),
        },
    })));
    started(&mut app, &run_id());
    app.update(Msg::Applied(Box::new(ApplyEvent::Item {
        run: run_id(),
        event: ItemEvent::Ended {
            item: ItemId(0),
            outcome: Box::new(done),
        },
    })));
    app.update(Msg::Applied(Box::new(ApplyEvent::Item {
        run: run_id(),
        event: ItemEvent::Started { item: ItemId(1) },
    })));
    app
}

/// The sizes every screen of a clean is drawn at: the smallest it must work at, and
/// the reference size.
const SIZES: [(u16, u16); 2] = [(80, 24), (160, 48)];

#[test]
fn the_review_at_80_by_24_and_160_by_48() {
    for (width, height) in SIZES {
        let home = Home::new("");
        let app = reviewing_with_refused(&home, width, height);
        insta::assert_snapshot!(
            format!("review_{width}x{height}"),
            text(&app, width, height)
        );
    }
}

#[test]
fn the_typed_size_at_80_by_24_and_160_by_48() {
    let home = Home::new("[review]\nconfirm_by_typing_above = \"1 MB\"\n");
    for (width, height) in SIZES {
        let mut app = reviewing(&home, width, height);
        press(&mut app, &[Key::Char('y')]);
        typed(&mut app, "16 m");
        insta::assert_snapshot!(
            format!("typed_size_{width}x{height}"),
            text(&app, width, height)
        );
    }
}

#[test]
fn the_progress_at_80_by_24_and_160_by_48() {
    let home = Home::new("");
    for (width, height) in SIZES {
        let app = applying(&home, width, height);
        insta::assert_snapshot!(
            format!("progress_{width}x{height}"),
            text(&app, width, height)
        );
    }
}

// Catches the progress numbering the item under way apart from its line and the receipt:
// "item 2" runs, but no line 2 ever appears, since that item ends as "1".
#[test]
fn the_progress_names_the_item_under_way_by_its_receipt_number() {
    let home = Home::new("");
    // Item 0 ended, item 1 under way.
    let app = applying(&home, 160, 48);

    let shown = text(&app, 160, 48);
    let title = shown.lines().nth(1).unwrap();
    assert!(title.contains("item 1:"), "{title}");
    assert!(!title.contains("item 2"), "{title}");
}

#[test]
fn the_result_at_80_by_24_and_160_by_48() {
    let home = Home::new("");
    for (width, height) in SIZES {
        let mut app = reviewing(&home, width, height);
        let finished = finished(&app);
        press(&mut app, &[Key::Char('y')]);
        app.take_effect();
        app.update(Msg::Ran(Box::new(finished)));
        insta::assert_snapshot!(
            format!("result_{width}x{height}"),
            text(&app, width, height)
        );
    }
}

// Catches a clean's screens panicking at a size the terminal can be resized to while it
// runs, down to nothing.
#[test]
fn every_screen_of_a_clean_draws_at_any_size_without_panicking() {
    let home = Home::new("[review]\nconfirm_by_typing_above = \"1 MB\"\n");
    let mut screens = vec![
        reviewing_with_refused(&home, 80, 24),
        applying(&home, 80, 24),
    ];
    let mut typing = reviewing(&home, 80, 24);
    press(&mut typing, &[Key::Char('y')]);
    screens.push(typing);
    for mut app in screens {
        for (width, height) in [(0, 0), (1, 1), (20, 5), (40, 10), (79, 23), (240, 70)] {
            app.update(Msg::Resize { width, height });
            text(&app, width, height);
        }
    }
}
