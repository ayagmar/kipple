//! No secret on a process's command line reaches what kipple shows (`docs/04-safety-model.md`
//! §8a): a scan over an injected process whose title holds a token and a broker URL, shown
//! as JSON and as the detail lines that `explain` and the home screen print.

use std::hash::{DefaultHasher, Hash as _, Hasher as _};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

use kipple_core::{
    ActivityEvidence, Apparent, CancelToken, Category, Clock, Completeness, Consequence, Cost,
    DirAccess, DirEntryMeta, Engine, EntryKind, EntryMeta, Evidence, FileIdentity, FindingSink,
    FsProbe, Integration, IntegrationDescriptor, IntegrationError, IntegrationId, Inventory, Lens,
    Measurement, Method, Observer, Operation, Platform, Policy, Ports, ProbeError, ProcessOutcome,
    ProcessOwner, ProcessProbe, ProcessRecord, ProcessSnapshot, Recovery, ResolvedRoot, RootSource,
    Roots, Rule, RuleId, RuleText, ScanContext, ScanRequest, Selector, Sizer, SpaceEstimate,
    SymbolicRoot, event_channel,
};

use crate::json::FindingOut;

const SECRETS: [&str; 2] = ["hunter2", "s3cret"];

/// Every path is a directory with an identity of its own; there is no repository.
#[derive(Debug)]
struct Disk;

impl FsProbe for Disk {
    fn metadata(&self, path: &Path) -> Result<EntryMeta, ProbeError> {
        if path.ends_with(".git") {
            return Err(ProbeError::io(path, io::ErrorKind::NotFound.into()));
        }
        let mut hasher = DefaultHasher::new();
        path.hash(&mut hasher);
        Ok(EntryMeta {
            kind: EntryKind::Dir,
            identity: Some(FileIdentity {
                volume: 1,
                file: hasher.finish(),
            }),
        })
    }

    fn read_dir(&self, _: &Path) -> Result<Vec<DirEntryMeta>, ProbeError> {
        Ok(Vec::new())
    }

    fn open_dir(&self, _: &Path) -> Result<(), ProbeError> {
        Ok(())
    }

    fn read_file(&self, path: &Path, _: u64) -> Result<Vec<u8>, ProbeError> {
        Err(ProbeError::io(path, io::ErrorKind::NotFound.into()))
    }

    fn access(&self, dir: &Path) -> Result<DirAccess, ProbeError> {
        Err(ProbeError::io(dir, io::ErrorKind::PermissionDenied.into()))
    }
}

#[derive(Debug)]
struct Sized;

impl Sizer for Sized {
    fn size(&self, _: &Path, _: &CancelToken) -> Result<Measurement, ProbeError> {
        Ok(Measurement {
            size: SpaceEstimate {
                apparent: Apparent(10),
                allocated: None,
                unique_reclaim: None,
                completeness: Completeness::Complete,
            },
            modified: Some(SystemTime::UNIX_EPOCH),
            repository: None,
        })
    }
}

#[derive(Debug)]
struct Epoch;

impl Clock for Epoch {
    fn now(&self) -> SystemTime {
        SystemTime::UNIX_EPOCH
    }
}

/// One root process that can't be inspected, whose title is `title`.
#[derive(Debug)]
struct Titled(String);

impl ProcessProbe for Titled {
    fn snapshot(&self) -> Result<ProcessSnapshot, ProbeError> {
        let denied = ProcessOutcome::Denied(Arc::new(io::ErrorKind::PermissionDenied.into()));
        let failed =
            || kipple_core::Unread::Failed(Arc::new(io::ErrorKind::PermissionDenied.into()));
        Ok(ProcessSnapshot {
            processes: vec![ProcessRecord {
                owner: ProcessOwner::Root,
                argv: Ok(vec![self.0.clone().into()]),
                cwd: Err(failed()),
                env_paths: Err(failed()),
                mapped_files: Err(failed()),
                ..ProcessRecord::new(9, denied)
            }],
            inventory: Inventory::Complete,
            observer: Observer::PLAIN,
        })
    }
}

fn rule() -> Rule {
    Rule {
        id: RuleId("builtin.test.venv".to_owned()),
        integration: IntegrationId("test"),
        text: RuleText {
            title: "Test".to_owned(),
            description: String::new(),
            why: "why".to_owned(),
            loss: "loss".to_owned(),
            references: Vec::new(),
        },
        category: Category::Cache,
        platforms: Platform::ALL.to_vec(),
        root: SymbolicRoot::ProjectRoots,
        selector: Selector::Integration("test"),
        requirements: Vec::new(),
        eligibility: Vec::new(),
        operation: Operation::RemoveFiles(vec![Method::Delete]),
        recent_days: None,
    }
}

/// Proposes one virtual environment.
#[derive(Debug)]
struct Venv(PathBuf);

impl Integration for Venv {
    fn descriptor(&self) -> IntegrationDescriptor {
        IntegrationDescriptor {
            id: IntegrationId("test"),
            lens: Lens::Dev,
            platforms: Platform::ALL,
            roots: &[],
            selectors: &[],
            artifacts: &[],
            owner_files: &[],
            verified: &[],
        }
    }

    fn activity_evidence(&self) -> ActivityEvidence {
        ActivityEvidence::Interpreted {
            runtimes: &["python*"],
        }
    }

    fn discover(
        &self,
        _: &ScanContext<'_>,
        sink: &mut FindingSink<'_>,
    ) -> Result<(), IntegrationError> {
        let consequence = Consequence {
            recovery: Recovery::Reinstall,
            loss: &[],
            cost: Cost::Low,
        };
        let evidence = vec![Evidence::Marker("pyproject.toml".to_owned())];
        sink.add(&rule(), &self.0, evidence, Some(consequence))
    }
}

/// `path` made absolute on this OS.
fn abs(path: &str) -> PathBuf {
    if Path::new("/").is_absolute() {
        PathBuf::from(path)
    } else {
        PathBuf::from(format!("C:{path}"))
    }
}

#[test]
fn a_secret_in_a_process_title_never_reaches_json_or_the_detail_lines() {
    for venv in ["/home/u/code/app/.venv", "/home/u/code/my app/.venv"] {
        let venv = abs(venv);
        let title = format!(
            "{}/bin/python manage.py --token=hunter2 --broker=redis://u:s3cret@h",
            venv.display()
        );
        let ports = Ports {
            fs: Box::new(Disk),
            sizer: Box::new(Sized),
            processes: Box::new(Titled(title)),
            clock: Box::new(Epoch),
        };
        let engine = Engine::new(ports, vec![Box::new(Venv(venv.clone()))], vec![rule()]).unwrap();
        let mut roots = Roots::default();
        let code = ResolvedRoot::new(
            SymbolicRoot::ProjectRoots,
            abs("/home/u/code"),
            RootSource::CommandLine,
        );
        roots.add(SymbolicRoot::ProjectRoots, Ok(code));
        let request = ScanRequest {
            roots,
            lens: None,
            exclude: Vec::new(),
        };
        let cancel = CancelToken::new();
        let (tx, rx) = event_channel(1024, &cancel);
        let drain = std::thread::spawn(move || rx.into_iter().count());
        let report = engine.scan(&request, &tx, &cancel);
        drop(tx);
        drain.join().unwrap();
        let finding = report.findings.first().unwrap();
        let policy = Policy::default();

        let json = serde_json::to_string(&FindingOut::new(finding, Lens::Dev, &policy)).unwrap();
        let lines = super::facts(finding, &policy);
        let (_, in_use) = lines.iter().find(|(label, _)| *label == "In use").unwrap();

        assert!(json.contains("\"in-use\""), "{json}");
        assert!(in_use.starts_with("process 9 was started with"), "{in_use}");
        for secret in SECRETS {
            assert!(!json.contains(secret), "{json}");
            assert!(!in_use.contains(secret), "{in_use}");
        }
    }
}
