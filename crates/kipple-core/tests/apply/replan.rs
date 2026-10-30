//! Plan files planned again: nothing a file says widens what a fresh plan removes (spec
//! M3b pinned 2; D-067).

use kipple_core::format::{decode_plan_file, encode_plan_file};
use kipple_core::{
    CancelToken, Caution, Dropped, ImportedPlan, Mark, Method, Modified, PlanMode, ReplanRequest,
    Replanned, ScanReport, Skip, replan,
};
use serde_json::Value;

use super::codec::TestCodec;
use super::world::World;
use super::{Config, plan_all, ports, project, rel, scan_of, snapshot, world_of};

/// Two projects' `target/`, scanned and planned under `config`, with the file `plan -o`
/// writes for that plan.
fn planned(config: &Config) -> (World, ScanReport, Value) {
    let projects = [project(0, 100, &["a", "b"]), project(1, 200, &["c"])];
    let world = world_of(&projects);
    let scan = scan_of(&[projects[0].0.clone(), projects[1].0.clone()]);
    let planned = plan_all(&scan, config, &world, PlanMode::Safe);
    assert_eq!(planned.plan.entries().len(), 2);
    let file =
        serde_json::from_slice(&encode_plan_file(&planned.plan, &TestCodec).unwrap()).unwrap();
    (world, scan, file)
}

fn read(file: &Value) -> ImportedPlan {
    decode_plan_file(&serde_json::to_vec(file).unwrap(), &TestCodec).unwrap()
}

fn again(file: &Value, scan: &ScanReport, config: &Config, world: &World) -> Replanned {
    let imported = read(file);
    let request = ReplanRequest {
        imported: &imported,
        scan,
        mode: PlanMode::Safe,
    };
    replan(
        &request,
        &config.judge(),
        &ports(world),
        &CancelToken::new(),
    )
}

fn why(replanned: &Replanned) -> Vec<(usize, Dropped)> {
    replanned
        .dropped
        .iter()
        .map(|dropped| (dropped.index, dropped.why.clone()))
        .collect()
}

/// How many entries each item of `replanned` may remove.
fn authorized(replanned: &Replanned) -> Vec<usize> {
    replanned
        .plan
        .entries()
        .iter()
        .map(|entry| {
            entry
                .tree()
                .entries()
                .filter(|one| one.mark() == Mark::Remove)
                .count()
        })
        .collect()
}

/// Catches a file planned again differently from the plan it was written from when
/// nothing changed: every item, every entry, nothing dropped.
#[test]
fn an_unchanged_file_plans_again_as_written() {
    let config = Config::new();
    let (world, scan, file) = planned(&config);
    let replanned = again(&file, &scan, &config, &world);
    assert_eq!(why(&replanned), []);
    assert_eq!(authorized(&replanned), [3, 2]);
}

/// Catches a file that names a root the current config doesn't scan reaching it: the
/// item is found nowhere and dropped.
#[test]
fn a_file_with_an_extra_root_reaches_nothing_there() {
    let config = Config::new();
    let (world, scan, mut file) = planned(&config);
    let mut extra = file["items"][0].clone();
    extra["root_path"] = Value::from("/elsewhere");
    file["items"].as_array_mut().unwrap().push(extra);
    let replanned = again(&file, &scan, &config, &world);
    assert_eq!(why(&replanned), [(2, Dropped::NotFound)]);
    assert_eq!(authorized(&replanned), [3, 2]);
}

/// Catches a file's delete overriding a config that says trash, which would make a
/// recoverable removal permanent; trash over a configured delete only narrows.
#[test]
fn a_file_never_deletes_where_the_config_trashes() {
    let config = Config::new();
    let (world, scan, file) = planned(&config);
    let mut trashing = Config::new();
    trashing.methods.config = Some(Method::Trash);
    let replanned = again(&file, &scan, &trashing, &world);
    assert_eq!(
        why(&replanned),
        [
            (
                0,
                Dropped::MethodWidened {
                    file: Method::Delete,
                    now: Method::Trash
                }
            ),
            (
                1,
                Dropped::MethodWidened {
                    file: Method::Delete,
                    now: Method::Trash
                }
            )
        ]
    );
    assert_eq!(replanned.plan.entries().len(), 0);
}

/// Catches a file's trash dropped or turned into a delete where the config deletes: a
/// trash move only narrows a delete.
#[test]
fn a_files_trash_narrows_a_configured_delete() {
    let mut trashing = Config::new();
    trashing.methods.config = Some(Method::Trash);
    let (world, scan, file) = planned(&trashing);
    let replanned = again(&file, &scan, &Config::new(), &world);
    assert_eq!(why(&replanned), []);
    assert!(
        replanned
            .plan
            .entries()
            .iter()
            .all(|entry| entry.method() == Method::Trash),
        "the file's trash narrows the configured delete"
    );
}

/// Catches a file whose trees were swapped between its items authorizing one item's
/// entries by the other's tree: nothing matches, so both drop.
#[test]
fn swapped_trees_authorize_nothing() {
    let config = Config::new();
    let (world, scan, mut file) = planned(&config);
    let first = file["items"][0]["tree"].clone();
    file["items"][0]["tree"] = file["items"][1]["tree"].clone();
    file["items"][1]["tree"] = first;
    let replanned = again(&file, &scan, &config, &world);
    assert_eq!(
        why(&replanned),
        [(0, Dropped::NothingInCommon), (1, Dropped::NothingInCommon)]
    );
}

/// Catches an entry that appeared after the file was written being removed with it:
/// only entries both the file and the fresh freeze hold are authorized.
#[test]
fn an_entry_added_since_the_file_is_left_out() {
    let config = Config::new();
    let (world, scan, file) = planned(&config);
    world
        .state()
        .targets
        .get_mut(std::path::Path::new("/home/u/p0/target"))
        .unwrap()
        .entries
        .insert(
            rel("late"),
            snapshot(kipple_core::EntryKind::File, 150, 100, 1),
        );
    let replanned = again(&file, &scan, &config, &world);
    assert_eq!(why(&replanned), []);
    let tree = replanned.plan.entries()[0].tree();
    let late = tree.find(&rel("late")).unwrap();
    assert_ne!(
        tree.mark(late),
        Mark::Remove,
        "the new entry isn't authorized"
    );
    assert_eq!(authorized(&replanned), [3, 2]);
}

/// Catches a file planning an item that is Review now, which needs the TUI's per-item
/// review (pinned 1).
#[test]
fn an_item_that_needs_review_now_drops() {
    let config = Config::new();
    let (world, mut scan, file) = planned(&config);
    scan.findings[1].facts.as_mut().unwrap().modified =
        Modified::Ago(std::time::Duration::from_hours(1));
    let replanned = again(&file, &scan, &config, &world);
    assert_eq!(
        why(&replanned),
        [(1, Dropped::Refused(Skip::Riskier(vec![Caution::Recent])))]
    );
    assert_eq!(authorized(&replanned), [3]);
}
