//! A fake `System` and fixtures shared by the Inspect & Ask tests. No test
//! spawns `ocs`, `lsof` or `git`.
use super::{Inspect, Ocs, Probe, Relation, Run, Session, System};
use serde_json::Value;
use std::{
    cell::RefCell,
    fs,
    path::{Path, PathBuf},
    time::Duration,
};
use tempfile::TempDir;

#[derive(Default)]
pub(super) struct Fake {
    pub installed: bool,
    pub sessions: Vec<Session>,
    /// What `ocs whoami` answers.
    pub me: RefCell<Option<String>>,
    pub port: Option<PathBuf>,
    /// Git worktree toplevels: a directory belongs to the longest one that
    /// contains it.
    pub tops: Vec<PathBuf>,
    /// `git ls-files` answers per toplevel.
    pub configs: Vec<(PathBuf, Vec<String>)>,
    pub relations: Vec<(PathBuf, Relation)>,
    /// Directories whose worktree probe times out.
    pub slow: Vec<PathBuf>,
    pub calls: RefCell<Vec<String>>,
    pub dm_result: RefCell<Option<Run>>,
    pub sent: RefCell<Vec<(String, String)>>,
    /// The `--as` of each dm, in order.
    pub senders: RefCell<Vec<String>>,
    /// Answers to other ocs commands by their argv joined with spaces; a
    /// command with no answer fails. Every call is logged in `ocs_calls`.
    pub ocs_answers: RefCell<Vec<(String, String)>>,
    pub ocs_calls: RefCell<Vec<String>>,
}

impl System for Fake {
    fn ocs(&self) -> Option<Ocs> {
        self.installed.then(|| Ocs {
            path: "/fake/ocs".into(),
            version: Some("0.8.3".into()),
        })
    }
    fn roster(&self, _: &Path) -> Result<Vec<Session>, String> {
        Ok(self.sessions.clone())
    }
    fn whoami(&self, _: &Path) -> Result<String, String> {
        self.me
            .borrow()
            .clone()
            .ok_or_else(|| "ocs whoami did not name this session".into())
    }
    fn port_cwd(&self, port: u16, _: Duration) -> Probe<PathBuf> {
        self.calls.borrow_mut().push(format!("lsof {port}"));
        self.port.clone().map_or(Probe::Absent, Probe::Found)
    }
    fn git_toplevel(&self, dir: &Path, _: Duration) -> Probe<PathBuf> {
        self.calls
            .borrow_mut()
            .push(format!("toplevel {}", dir.display()));
        if self.slow.iter().any(|s| s == dir) {
            return Probe::TimedOut;
        }
        self.tops
            .iter()
            .filter(|t| dir.starts_with(t))
            .max_by_key(|t| t.as_os_str().len())
            .cloned()
            .map_or(Probe::Absent, Probe::Found)
    }
    fn git_configs(&self, top: &Path, _: Duration) -> Probe<String> {
        self.calls
            .borrow_mut()
            .push(format!("ls-files {}", top.display()));
        self.configs
            .iter()
            .find(|(t, _)| t == top)
            .map_or(Probe::Found(String::new()), |(_, files)| {
                Probe::Found(files.iter().map(|f| format!("{f}\0")).collect())
            })
    }
    fn git_revision(&self, top: &Path, _: &str, _: Duration) -> Probe<Relation> {
        Probe::Found(
            self.relations
                .iter()
                .find(|(t, _)| t == top)
                .map_or(Relation::Unknown, |(_, r)| *r),
        )
    }
    fn ocs_json(&self, _: &Path, args: &[&str]) -> Result<Run, String> {
        let argv = args.join(" ");
        self.ocs_calls.borrow_mut().push(argv.clone());
        let answer = self
            .ocs_answers
            .borrow()
            .iter()
            .find(|(a, _)| *a == argv)
            .map(|(_, out)| out.clone())
            .ok_or_else(|| format!("no fake answer for ocs {argv}"))?;
        Ok(Run {
            code: Some(0),
            stdout: answer,
            ..Run::default()
        })
    }
    fn dm(&self, _: &Path, id: &str, text: &str, sender: &str) -> Result<Run, String> {
        self.senders.borrow_mut().push(sender.into());
        self.sent.borrow_mut().push((id.into(), text.into()));
        Ok(self.dm_result.borrow_mut().take().unwrap_or(Run {
            code: Some(0),
            stdout: r#"{"v":1,"stored":true,"channel":"dm-x","seq":7,"wake":"delivered","exit":0,"lines":[]}"#.into(),
            ..Run::default()
        }))
    }
}

pub(super) fn session(id: &str, cwd: &Path) -> Session {
    Session {
        id: id.into(),
        kind: "claude".into(),
        name: Some("fixture".into()),
        cwd: cwd.into(),
        status: Some("idle".into()),
        addresses: Vec::new(),
    }
}

pub(super) struct World {
    pub _temp: TempDir,
    pub data: PathBuf,
    pub project: PathBuf,
}

pub(super) fn world() -> World {
    let temp = TempDir::new().unwrap();
    let base = fs::canonicalize(temp.path()).unwrap();
    let project = base.join("my project");
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(project.join("src/App.tsx"), "x").unwrap();
    World {
        data: base.join("home/.chrome-use/inspect"),
        project,
        _temp: temp,
    }
}

pub(super) fn call(w: &World, sys: &Fake, op: &str, params: Value) -> Result<Value, String> {
    Inspect {
        root: w.data.clone(),
        sys,
        profile: None,
    }
    .handle(op, &params)
}

pub(super) const ORIGIN: &str = "http://localhost:5173";
