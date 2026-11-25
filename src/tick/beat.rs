use std::ffi::{OsStr, OsString};
use std::fmt;
use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::Deserializer as _;
use serde::de::{self, DeserializeSeed, MapAccess, SeqAccess, Visitor};
use serde_json::Value;

use crate::tick::act::{ActLine, Ctx, Env, Outcome, Registry, Report, clip};
use crate::tick::proc::{Cmd, Exit, KILL_AFTER, Keep, Proc, Ran, Spilled, StdoutKeep, SystemProc};
use crate::tick::{TickError, claude_md, collect, directive, exec, gitsync, pin};

pub const DEADLINE_SECS: i64 = 230;

pub const EXECUTOR_CAP_SECS: i64 = 120;

pub const FRAME_CAP: Duration = Duration::from_secs(30);

pub const QUEUE_HELD: [&str; 2] = ["merge-ready", "review-owed"];

pub const HANDOFF_PICK: &str = "tools/cloud-env/handoff-pick.sh";

pub type Phase<'a> = &'a dyn Fn(&Ctx) -> Report;

pub struct Phases<'a> {
    pub gitsync: &'a dyn Fn(&Ctx, gitsync::Mode) -> Report,
    pub pin: Phase<'a>,
    pub directive: Phase<'a>,
    pub collect: Phase<'a>,
    pub claude_md: Phase<'a>,
}

impl Phases<'static> {
    pub const REAL: Phases<'static> = Phases {
        gitsync: &gitsync::sync,
        pin: &pin::run,
        directive: &directive::run,
        collect: &collect::run,
        claude_md: &claude_md::run,
    };
}

pub struct Beat<'a> {
    pub checkout: &'a Path,
    pub proc: &'a dyn Proc,
    pub env: &'a Env,
    pub registry: &'a Registry,
    pub phases: &'a Phases<'a>,
    pub cm: &'a OsStr,
    pub started: Instant,
}

impl Beat<'_> {
    fn ctx<'b>(&'b self, epoch: &'b Path, budget: Duration) -> Ctx<'b> {
        Ctx::new(self.checkout, epoch, self.proc, self.env, self.cm, budget)
    }

    fn elapsed_secs(&self) -> i64 {
        i64::try_from(self.started.elapsed().as_secs()).unwrap_or(i64::MAX)
    }
}

pub fn cli(checkout: &Path, cm: Option<&OsStr>, out: &mut dyn FnMut(String)) -> OsString {
    let started = Instant::now();
    let env = Env::system();
    let registry = exec::registry();
    let cm = cm.map_or_else(this_cm, resolve);
    let checkout = std::path::absolute(checkout).unwrap_or_else(|_| checkout.to_path_buf());
    let beat = Beat {
        checkout: &checkout,
        proc: &SystemProc,
        env: &env,
        registry: &registry,
        phases: &Phases::REAL,
        cm: &cm,
        started,
    };
    run(&beat, out);
    cm
}

pub fn this_cm() -> OsString {
    std::env::current_exe().map_or_else(|_| "cm".into(), PathBuf::into_os_string)
}

// Children run in the checkout, so `--cm` is resolved where it was given: a path made absolute,
// a bare name looked up on PATH as a shell would, an empty or relative entry taken from the
// directory cm tick started in. A name PATH does not hold from there stays bare, so the spawn
// searches PATH again from the checkout, where an empty or relative entry can still find one.
pub fn resolve(cm: &OsStr) -> OsString {
    let named = Path::new(cm);
    if cm.as_encoded_bytes().contains(&b'/') {
        return std::path::absolute(named).map_or_else(|_| cm.to_owned(), PathBuf::into_os_string);
    }
    // The effective user's own execute permission, not the mode's bits: a file whose only
    // execute bit is another class's is skipped, as execvp would fail on it and move on.
    let runnable = |path: &Path| {
        path.metadata().is_ok_and(|meta| meta.is_file())
            && rustix::fs::accessat(
                rustix::fs::CWD,
                path,
                rustix::fs::Access::EXEC_OK,
                rustix::fs::AtFlags::EACCESS,
            )
            .is_ok()
    };
    std::env::var_os("PATH")
        .and_then(|paths| {
            std::env::split_paths(&paths)
                .map(|dir| dir.join(named))
                .find(|path| !cm.is_empty() && runnable(path))
        })
        .and_then(|path| std::path::absolute(path).ok())
        .map_or_else(|| cm.to_owned(), PathBuf::into_os_string)
}

pub fn run(beat: &Beat, out: &mut dyn FnMut(String)) {
    let out: &mut dyn FnMut(String) = &mut |line| out(one_line(line));
    let epochs = match live_epochs(beat.checkout) {
        Ok(epochs) => epochs,
        Err(error) => {
            out(format!(
                "heartbeat: could not read {error}; stay read-only until cm, PR and journal state are reconciled"
            ));
            return;
        }
    };
    let [epoch] = epochs.as_slice() else {
        out(format!(
            "heartbeat: {} PLANNING or WORKING epochs, not one; stay read-only until cm, PR and journal state are reconciled",
            epochs.len()
        ));
        return;
    };
    if !bound(beat.checkout, epoch, beat.proc, beat.env) {
        return;
    }
    let deadline = deadline(beat.env).unwrap_or_else(|line| {
        out(line);
        DEADLINE_SECS
    });
    let phases = beat.phases;
    if !sync(beat, epoch, out, gitsync::Mode::Always) {
        return;
    }
    phase(beat, epoch, out, "pin", pin::BUDGET, phases.pin);
    phase(
        beat,
        epoch,
        out,
        "directive",
        directive::BUDGET,
        phases.directive,
    );
    phase(beat, epoch, out, "collect", collect::BUDGET, phases.collect);
    let started = Instant::now();
    let left = u64::try_from(deadline.saturating_sub(beat.elapsed_secs())).unwrap_or(0);
    let budget = Duration::from_secs(left).saturating_add(KILL_AFTER);
    // A failed `cm next` prints only its own line, never the act phase's wall time.
    let act = act(beat, epoch, deadline, out);
    if !act.failed
        && let Some(line) = wall_line("act", started.elapsed(), budget, false)
    {
        out(line);
    }
    if act.ran && !sync(beat, epoch, out, gitsync::Mode::IfDirty) {
        return;
    }
    phase(
        beat,
        epoch,
        out,
        "claude-md",
        claude_md::BUDGET,
        phases.claude_md,
    );
}

pub fn check(checkout: &Path, proc: &dyn Proc, env: &Env) -> bool {
    matches!(live_epochs(checkout).as_deref(), Ok([epoch]) if bound(checkout, epoch, proc, env))
}

pub fn cli_check(checkout: &Path) -> bool {
    let checkout = std::path::absolute(checkout).unwrap_or_else(|_| checkout.to_path_buf());
    check(&checkout, &SystemProc, &Env::system())
}

// heartbeat.sh's is_bound. Every child runs under FRAME_CAP, never through `Ctx::run`, so the
// check spends no phase budget and prints no wall time.
pub(crate) fn bound(checkout: &Path, epoch: &Path, proc: &dyn Proc, env: &Env) -> bool {
    let run = |cmd: Cmd| {
        proc.run(
            &cmd.cwd(checkout)
                .timeout(FRAME_CAP)
                .keep(Keep::Head, Keep::Tail),
        )
        .ok()
    };
    let answered = |cmd: Cmd| run(cmd).filter(|ran| ran.exit != Exit::TimedOut);
    let succeeded = |cmd: Cmd| run(cmd).filter(|ran| ran.exit.success());
    let branch = answered(Cmd::new("git").args(["rev-parse", "--abbrev-ref", "HEAD"]));
    if branch
        .is_none_or(|ran| String::from_utf8_lossy(&ran.stdout).trim_end_matches('\n') != "main")
    {
        return false;
    }
    let Some(id) = env.non_empty("CLAUDE_CODE_REMOTE_SESSION_ID") else {
        return false;
    };
    let lineage = format!("session_{}", id.strip_prefix("cse_").unwrap_or(id));
    // Age never unbinds: an unsuperseded head of any age (`stale`, exit 6) is still this
    // coordinator's. Any other status is a pick that failed, whatever it printed.
    let picked = |dir: &Path| {
        let pick = Cmd::new("sh").arg(HANDOFF_PICK).arg(dir).args([
            "coordinator",
            "--lineage",
            &lineage,
            "--max-age-hours",
            "24",
        ]);
        run(pick).is_some_and(|ran| {
            let line = first_line(&ran.stdout, ran.stdout.len());
            matches!(ran.exit, Exit::Code(0 | 6))
                && (line.starts_with("ok ") || line.starts_with("stale "))
        })
    };
    if !picked(epoch) {
        return false;
    }
    if succeeded(Cmd::new("git").args(["fetch", "-q", "origin", "main"])).is_none() {
        return true;
    }
    let handoff = epoch.join("handoff");
    let mut under = handoff.clone().into_os_string();
    under.push("/");
    let added = succeeded(
        Cmd::new("git")
            .args([
                "diff",
                "--name-only",
                "-z",
                "--no-renames",
                "--diff-filter=A",
                "HEAD",
                "origin/main",
                "--",
            ])
            .arg(under),
    );
    let Some(added) = added else {
        return true;
    };
    let coordinator = |name: &[u8]| {
        let file = name.rsplit(|byte| *byte == b'/').next().unwrap_or(name);
        file.starts_with(b"coordinator-") && file.ends_with(b".md")
    };
    // A list cut at its head may hide a name past the cut, so it goes on to origin's own view.
    if !added.stdout_truncated && !added.stdout.split(|byte| *byte == 0).any(coordinator) {
        return true;
    }
    let Some(root) = extract_root(env.get("TMPDIR")) else {
        return false;
    };
    let Ok(tmp) = tempfile::Builder::new().prefix("cm-tick-").tempdir_in(root) else {
        return false;
    };
    let dir = tmp.path();
    let tar = dir.join("h.tar");
    // Only origin's own attributes may change the copy, never this checkout's eol settings.
    let archived = succeeded(
        Cmd::new("git")
            .args([
                "-c",
                "core.autocrlf=false",
                "-c",
                "core.eol=lf",
                "archive",
                "-o",
            ])
            .arg(&tar)
            .arg("origin/main")
            .arg(&handoff),
    )
    .is_some()
        && succeeded(
            Cmd::new("tar")
                .arg("-x")
                .arg("-C")
                .arg(dir)
                .arg("-f")
                .arg(&tar),
        )
        .is_some();
    // git archive applies origin's export-ignore, export-subst and eol attributes, and a changed
    // copy is not origin's folder: every file its tree lists must be in the extract, byte for byte.
    let listed = archived
        .then(|| {
            succeeded(
                Cmd::new("git")
                    .args(["ls-tree", "-r", "-z", "origin/main", "--"])
                    .arg(&handoff),
            )
        })
        .flatten();
    let whole = listed.is_some_and(|ran| {
        !ran.stdout_truncated && extract_is_origins(&succeeded, dir, &ran.stdout)
    });
    whole && picked(&dir.join(epoch))
}

// mktemp's folder for TMPDIR, /tmp when it is unset or empty, except that a relative TMPDIR,
// which heartbeat.sh resolved in the checkout, is refused: the extract is never made there.
fn extract_root(tmpdir: Option<&str>) -> Option<&Path> {
    let root = tmpdir
        .filter(|dir| !dir.is_empty())
        .map_or(Path::new("/tmp"), Path::new);
    root.is_absolute().then_some(root)
}

const HASH_ARGV_BYTES: usize = 128 << 10;

// `git ls-tree -r -z` entries are `<mode> <type> <object>\t<path>`. A file's bytes, or a
// symlink's target, must hash to its object; a submodule has nothing in an archive to compare,
// and the executable bit is no part of a file's content.
fn extract_is_origins(succeeded: &dyn Fn(Cmd) -> Option<Ran>, dir: &Path, listing: &[u8]) -> bool {
    use std::os::unix::ffi::OsStrExt;
    let links = dir.join("links");
    let mut files: Vec<(PathBuf, &[u8])> = Vec::new();
    for entry in listing
        .split(|byte| *byte == 0)
        .filter(|entry| !entry.is_empty())
    {
        let Some(tab) = entry.iter().position(|byte| *byte == b'\t') else {
            return false;
        };
        let mut head = entry[..tab].split(|byte| *byte == b' ');
        let (Some(mode), Some(_), Some(object)) = (head.next(), head.next(), head.next()) else {
            return false;
        };
        let path = dir.join(OsStr::from_bytes(&entry[tab + 1..]));
        let kind = path.symlink_metadata().map(|meta| meta.file_type());
        match mode {
            b"160000" => continue,
            b"100644" | b"100755" if kind.as_ref().is_ok_and(|kind| kind.is_file()) => {
                files.push((path, object));
            }
            b"120000" if kind.as_ref().is_ok_and(|kind| kind.is_symlink()) => {
                let copy = links.join(files.len().to_string());
                let Ok(target) = std::fs::read_link(&path) else {
                    return false;
                };
                if std::fs::create_dir_all(&links)
                    .and_then(|()| std::fs::write(&copy, target.as_os_str().as_bytes()))
                    .is_err()
                {
                    return false;
                }
                files.push((copy, object));
            }
            _ => return false,
        }
    }
    let mut rest = files.as_slice();
    while !rest.is_empty() {
        let mut bytes = 0;
        let count = rest
            .iter()
            .take_while(|(path, _)| {
                bytes += path.as_os_str().len() + 1;
                bytes <= HASH_ARGV_BYTES
            })
            .count()
            .max(1);
        let (chunk, later) = rest.split_at(count);
        rest = later;
        let hashed = succeeded(
            Cmd::new("git")
                .args(["hash-object", "--no-filters", "--"])
                .args(chunk.iter().map(|(path, _)| path)),
        );
        let Some(hashed) = hashed.filter(|ran| !ran.stdout_truncated) else {
            return false;
        };
        let objects: Vec<&[u8]> = hashed
            .stdout
            .split(|byte| *byte == b'\n')
            .filter(|line| !line.is_empty())
            .collect();
        if !objects
            .iter()
            .copied()
            .eq(chunk.iter().map(|(_, object)| *object))
        {
            return false;
        }
    }
    true
}

// heartbeat.sh re-checks after git_sync: a successor's handoff pulled just now retires this
// session.
fn sync(beat: &Beat, epoch: &Path, out: &mut dyn FnMut(String), mode: gitsync::Mode) -> bool {
    let wall = run_phase(beat, epoch, out, "gitsync", gitsync::BUDGET, |ctx| {
        (beat.phases.gitsync)(ctx, mode)
    });
    if !bound(beat.checkout, epoch, beat.proc, beat.env) {
        return false;
    }
    if let Some(line) = wall {
        out(line);
    }
    true
}

// A path, a child's text, the act list or the environment can hold a line break or a terminal
// control; printed raw, it would read as a line of its own, an act line a model obeys.
fn one_line(line: String) -> String {
    let control = |c: char| c.is_control() || matches!(c, '\u{2028}' | '\u{2029}');
    if !line.contains(control) {
        return line;
    }
    line.chars()
        .fold(String::with_capacity(line.len()), |mut text, c| {
            if control(c) {
                text.extend(c.escape_default());
            } else {
                text.push(c);
            }
            text
        })
}

// A store or EPOCH.md that is missing or not the kind of file it should be is no epoch, as
// heartbeat.sh's glob matched none; a read that fails (EMFILE, EACCES, ELOOP) is returned, not
// counted as no epoch.
pub fn live_epochs(checkout: &Path) -> std::result::Result<Vec<PathBuf>, TickError> {
    use std::io::ErrorKind::{IsADirectory, NotADirectory, NotFound};
    let epochs = Path::new("store/epochs");
    let missing =
        |error: &std::io::Error| matches!(error.kind(), NotFound | NotADirectory | IsADirectory);
    let failed = |path: &Path| {
        let path = path.to_path_buf();
        move |source| TickError::Io { path, source }
    };
    let entries = match std::fs::read_dir(checkout.join(epochs)) {
        Err(error) if missing(&error) => return Ok(Vec::new()),
        entries => entries.map_err(failed(epochs))?,
    };
    let live = |bytes: &[u8]| {
        bytes.split(|byte| *byte == b'\n').any(|line| {
            line.starts_with(b"status: PLANNING") || line.starts_with(b"status: WORKING")
        })
    };
    let mut found = Vec::new();
    for entry in entries {
        let name = entry.map_err(failed(epochs))?.file_name();
        if name.as_encoded_bytes().starts_with(b".") {
            continue;
        }
        let epoch = epochs.join(name);
        let file = epoch.join("EPOCH.md");
        match std::fs::read(checkout.join(&file)) {
            Ok(bytes) if live(&bytes) => found.push(epoch),
            Ok(_) => {}
            Err(error) if missing(&error) => {}
            Err(error) => return Err(failed(&file)(error)),
        }
    }
    found.sort();
    Ok(found)
}

fn deadline(env: &Env) -> Result<i64, String> {
    match env.non_empty("HEARTBEAT_DEADLINE") {
        None => Ok(DEADLINE_SECS),
        Some(text) => text.trim().parse().map_err(|_| {
            format!(
                "heartbeat: HEARTBEAT_DEADLINE={text} is not a whole number of seconds; using {DEADLINE_SECS}"
            )
        }),
    }
}

fn phase(
    beat: &Beat,
    epoch: &Path,
    out: &mut dyn FnMut(String),
    name: &str,
    budget: Duration,
    body: impl FnOnce(&Ctx) -> Report,
) {
    if let Some(line) = run_phase(beat, epoch, out, name, budget, body) {
        out(line);
    }
}

fn run_phase(
    beat: &Beat,
    epoch: &Path,
    out: &mut dyn FnMut(String),
    name: &str,
    budget: Duration,
    body: impl FnOnce(&Ctx) -> Report,
) -> Option<String> {
    phase_in(&beat.ctx(epoch, budget), out, name, budget, body)
}

fn phase_in(
    ctx: &Ctx,
    out: &mut dyn FnMut(String),
    name: &str,
    budget: Duration,
    body: impl FnOnce(&Ctx) -> Report,
) -> Option<String> {
    let started = ctx.now();
    let report = contained(|| body(ctx)).unwrap_or_else(|panic| Report {
        lines: vec![format!("tick: {name} phase panicked: {panic}")],
        failed: true,
    });
    for line in report.lines {
        out(line);
    }
    let wall = ctx.now().saturating_duration_since(started);
    wall_line(
        name,
        wall.saturating_sub(ctx.checking()),
        budget,
        report.failed || ctx.timed_out(),
    )
}

fn wall_line(name: &str, wall: Duration, budget: Duration, failed: bool) -> Option<String> {
    (failed || wall.as_secs() > budget.as_secs()).then(|| {
        format!(
            "tick: {name} phase took {:.1} s (budget {} s)",
            wall.as_secs_f64(),
            budget.as_secs()
        )
    })
}

struct Act {
    ran: bool,
    failed: bool,
}

const READ_FAILED: Act = Act {
    ran: false,
    failed: true,
};

fn act(beat: &Beat, epoch: &Path, deadline: i64, out: &mut dyn FnMut(String)) -> Act {
    // Up to 64 MiB the list stays in memory, as heartbeat.sh held it, so it needs no disk;
    // past that it spills to a file. Either way it is read twice, whole and then line by line.
    let next = Cmd::new(beat.cm)
        .args(["--json", "next", "-C"])
        .arg(epoch)
        .arg("--act")
        .cwd(beat.checkout)
        .timeout(FRAME_CAP)
        .keep(StdoutKeep::Spill, Keep::Head);
    let next = match beat.proc.run(&next) {
        Ok(ran) if ran.exit == Exit::TimedOut => {
            out(format!(
                "heartbeat: cm --json next --act ran past {} s and was killed",
                FRAME_CAP.as_secs()
            ));
            return READ_FAILED;
        }
        Ok(Ran {
            stdout_spill: Some(Spilled::Failed(error)),
            ..
        }) => {
            out(format!(
                "heartbeat: cm --json next --act's list was too large to keep in memory and could not be stored in {}: {error}; no act line was read",
                std::env::temp_dir().display()
            ));
            return READ_FAILED;
        }
        Ok(ran) => ran,
        Err(error) => {
            out(format!(
                "heartbeat: cm --json next --act could not run: {error}"
            ));
            return READ_FAILED;
        }
    };
    let status = next.exit.shell_status();
    let list = List::of(&next);
    let shape = (status <= 1).then(|| list.read(None).ok()).flatten();
    let Some(shape) = shape else {
        if status == 0 && list.only_newlines() {
            return Act {
                ran: false,
                failed: false,
            };
        }
        out(format!(
            "heartbeat: cm --json next --act exited {status} without an actionable list: {}",
            first_line(&next.stderr, 300)
        ));
        return READ_FAILED;
    };
    if shape.unread != 0 {
        out(format!(
            "owner-queue: {} unread item(s); read OWNER-QUEUE.md and run cm owner-queue read before any merge or review",
            shape.unread
        ));
    }
    if shape.lines == 0 {
        return Act {
            ran: false,
            failed: false,
        };
    }
    let lines = Lines {
        beat,
        epoch,
        epoch_dir: beat.checkout.join(epoch),
        unread: shape.unread,
        deadline,
    };
    let mut ran = false;
    let read = list.read(Some(&mut |json| ran |= lines.act(json, out)));
    if let Err(error) = read {
        out(format!(
            "heartbeat: cm --json next --act's list could not be read again: {error}"
        ));
        return Act { ran, failed: true };
    }
    Act { ran, failed: false }
}

enum List<'a> {
    Kept(&'a [u8]),
    Spilled(&'a File),
}

impl<'a> List<'a> {
    fn of(ran: &'a Ran) -> Self {
        match &ran.stdout_spill {
            Some(Spilled::Stored(file)) => List::Spilled(file),
            _ => List::Kept(&ran.stdout),
        }
    }

    fn read(&self, each: Option<&mut dyn FnMut(Value)>) -> serde_json::Result<Shape> {
        match self {
            List::Kept(bytes) => read_list(*bytes, each),
            List::Spilled(file) => {
                rewind(file).map_err(serde_json::Error::io)?;
                read_list(BufReader::new(*file), each)
            }
        }
    }

    // heartbeat.sh's `[ -z "$act" ]`: nothing but newlines.
    fn only_newlines(&self) -> bool {
        match self {
            List::Kept(bytes) => bytes.iter().all(|byte| *byte == b'\n'),
            List::Spilled(file) => {
                rewind(file).is_ok()
                    && BufReader::new(*file)
                        .bytes()
                        .all(|byte| matches!(byte, Ok(b'\n')))
            }
        }
    }
}

fn rewind(mut file: &File) -> std::io::Result<()> {
    file.seek(SeekFrom::Start(0)).map(drop)
}

struct Shape {
    lines: usize,
    unread: usize,
}

// Without `each` this only checks the whole document, keeping none of its lines. Both passes
// parse every value alike, so a document the check accepts is one the acting pass reads.
fn read_list(source: impl Read, each: Option<&mut dyn FnMut(Value)>) -> serde_json::Result<Shape> {
    let mut parser = serde_json::Deserializer::from_reader(source);
    let shape = parser.deserialize_map(Document { each })?;
    parser.end()?;
    Ok(shape)
}

struct Document<'a> {
    each: Option<&'a mut dyn FnMut(Value)>,
}

impl<'de> Visitor<'de> for Document<'_> {
    type Value = Shape;

    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("an object with an `actionable` array")
    }

    fn visit_map<M: MapAccess<'de>>(mut self, mut map: M) -> Result<Shape, M::Error> {
        let mut lines = None;
        let mut unread = 0;
        while let Some(key) = map.next_key::<String>()? {
            match key.as_str() {
                "actionable" if lines.is_some() => {
                    return Err(de::Error::duplicate_field("actionable"));
                }
                "actionable" => {
                    lines = Some(map.next_value_seed(Elements {
                        each: self.each.take(),
                    })?);
                }
                "owner_queue_unread" => {
                    unread = map.next_value::<Value>()?.as_array().map_or(0, Vec::len);
                }
                _ => {
                    map.next_value::<Value>()?;
                }
            }
        }
        let lines = lines.ok_or_else(|| de::Error::missing_field("actionable"))?;
        Ok(Shape { lines, unread })
    }
}

struct Elements<'a> {
    each: Option<&'a mut dyn FnMut(Value)>,
}

impl<'de> DeserializeSeed<'de> for Elements<'_> {
    type Value = usize;

    fn deserialize<D: serde::Deserializer<'de>>(self, deserializer: D) -> Result<usize, D::Error> {
        deserializer.deserialize_seq(self)
    }
}

impl<'de> Visitor<'de> for Elements<'_> {
    type Value = usize;

    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("an array of act lines")
    }

    fn visit_seq<S: SeqAccess<'de>>(mut self, mut seq: S) -> Result<usize, S::Error> {
        let mut count = 0;
        while let Some(line) = seq.next_element::<Value>()? {
            if let Some(each) = &mut self.each {
                each(line);
            }
            count += 1;
        }
        Ok(count)
    }
}

struct Lines<'a> {
    beat: &'a Beat<'a>,
    epoch: &'a Path,
    epoch_dir: PathBuf,
    unread: usize,
    deadline: i64,
}

impl Lines<'_> {
    fn act(&self, json: Value, out: &mut dyn FnMut(String)) -> bool {
        let beat = self.beat;
        let line = ActLine::read(json, &self.epoch_dir);
        let text = line.text();
        let kind = line.kind();
        let Some((name, executor)) = beat.registry.lookup(&kind, line.lane.as_deref()) else {
            out(text);
            return false;
        };
        if self.unread != 0 && QUEUE_HELD.contains(&kind.as_str()) {
            out(text);
            return false;
        }
        let cap = self
            .deadline
            .saturating_sub(beat.elapsed_secs())
            .min(EXECUTOR_CAP_SECS);
        if cap < 1 {
            out(format!("{text} [not run: heartbeat time budget spent]"));
            return false;
        }
        let ctx = beat.ctx(self.epoch, Duration::from_secs(cap.unsigned_abs()));
        let outcome = contained(|| executor.run(&line, &ctx))
            .unwrap_or_else(|panic| Outcome::Failed(format!("panicked: {panic}")));
        if ctx.timed_out() {
            out(format!(
                "{text} [tick/{name} timed out after {cap} s and was killed; check what it started]"
            ));
            return true;
        }
        match outcome {
            Outcome::Done(lines) => {
                let left = lines.join("\n");
                let left = left.trim_end_matches('\n');
                if !left.is_empty() {
                    left.split('\n').for_each(|line| out(line.to_owned()));
                }
            }
            Outcome::Left(line) => out(line),
            Outcome::Failed(reason) => out(format!(
                "{text} [tick/{name} failed: {}]",
                clip(
                    reason
                        .split('\n')
                        .rfind(|line| !line.is_empty())
                        .unwrap_or("")
                )
            )),
        }
        true
    }
}

fn contained<T>(body: impl FnOnce() -> T) -> Result<T, String> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(body)).map_err(|payload| {
        payload
            .downcast_ref::<&str>()
            .map(|text| (*text).to_owned())
            .or_else(|| payload.downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "no message".to_owned())
    })
}

fn first_line(bytes: &[u8], limit: usize) -> String {
    let head = &bytes[..bytes.len().min(limit)];
    let line = head.split(|byte| *byte == b'\n').next().unwrap_or_default();
    String::from_utf8_lossy(line).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn printed(wall: Duration, budget: Duration, failed: bool) -> Vec<String> {
        wall_line("pin", wall, budget, failed).into_iter().collect()
    }

    #[test]
    fn the_extract_goes_where_mktemp_would_make_it_but_never_in_the_checkout() {
        let tmp = Some(Path::new("/tmp"));
        assert_eq!(extract_root(None), tmp);
        assert_eq!(extract_root(Some("")), tmp);
        for dir in ["/var/tmp/x", "/no/such/folder", "/etc/passwd"] {
            assert_eq!(extract_root(Some(dir)), Some(Path::new(dir)), "{dir}");
        }
        for relative in ["tmp", ".", "../tmp", " /tmp"] {
            assert_eq!(extract_root(Some(relative)), None, "{relative:?}");
        }
    }

    #[test]
    fn a_phase_wall_leaves_out_the_bound_checks_it_ran() {
        use crate::tick::act::{Ticking, bound_children};
        let clock = std::rc::Rc::new(std::cell::Cell::new(Instant::now()));
        let proc = bound_children(&clock, |word, clock| {
            (word == "fetch").then(|| {
                clock.set(clock.get() + Duration::from_secs(62));
                Ran::code(0, "")
            })
        });
        let env: Env = [("CLAUDE_CODE_REMOTE_SESSION_ID", "cse_1")]
            .into_iter()
            .collect();
        let ticking = Ticking(clock.clone());
        let ctx = || {
            Ctx::new(
                Path::new("."),
                Path::new("store/epochs/e"),
                &proc,
                &env,
                OsStr::new("cm"),
                Duration::ZERO,
            )
            .clocked(&ticking, gitsync::BUDGET)
        };
        let mut lines = Vec::new();
        let wall = phase_in(
            &ctx(),
            &mut |line| lines.push(line),
            "gitsync",
            gitsync::BUDGET,
            |ctx| {
                assert!(ctx.still_bound());
                Report::default()
            },
        );
        assert_eq!(wall, None, "a 62 s check is not the 60 s phase's");
        let wall = phase_in(
            &ctx(),
            &mut |line| lines.push(line),
            "gitsync",
            gitsync::BUDGET,
            |ctx| {
                assert!(ctx.still_bound());
                clock.set(clock.get() + Duration::from_secs(2));
                Report {
                    lines: vec!["synced".into()],
                    failed: true,
                }
            },
        );
        assert_eq!(
            wall.as_deref(),
            Some("tick: gitsync phase took 2.0 s (budget 60 s)")
        );
        assert_eq!(lines, ["synced"]);
    }

    #[test]
    fn wall_time_prints_only_on_failure_or_a_whole_second_past_the_budget() {
        let secs = Duration::from_secs_f64;
        assert!(printed(secs(60.9), secs(60.0), false).is_empty());
        assert_eq!(
            printed(secs(61.0), secs(60.0), false),
            ["tick: pin phase took 61.0 s (budget 60 s)"]
        );
        assert_eq!(
            printed(secs(0.04), secs(60.0), true),
            ["tick: pin phase took 0.0 s (budget 60 s)"]
        );
    }

    #[test]
    fn the_deadline_is_230_unless_heartbeat_deadline_is_a_whole_number() {
        let env = |value: &str| -> Env { [("HEARTBEAT_DEADLINE", value)].into_iter().collect() };
        assert_eq!(deadline(&Env::default()), Ok(230));
        assert_eq!(deadline(&env("")), Ok(230));
        assert_eq!(deadline(&env("6")), Ok(6));
        assert_eq!(deadline(&env(" -5 ")), Ok(-5));
        assert_eq!(
            deadline(&env("soon")),
            Err(
                "heartbeat: HEARTBEAT_DEADLINE=soon is not a whole number of seconds; using 230"
                    .to_owned()
            )
        );
    }

    #[test]
    fn read_list_checks_the_whole_document_then_hands_lines_one_at_a_time() {
        let shape = |text: &str| {
            read_list(text.as_bytes(), None)
                .map(|shape| (shape.lines, shape.unread))
                .ok()
        };
        assert_eq!(
            shape(r#"{"actionable": [1, {"a": 2}], "owner_queue_unread": ["x", "y"]}"#),
            Some((2, 2))
        );
        assert_eq!(
            shape(r#"{"owner_queue_unread": "x", "ok": true, "actionable": []}"#),
            Some((0, 0))
        );
        for bad in [
            "",
            "{}",
            r#"{"actionable": {}}"#,
            r#"{"actionable": null}"#,
            "[[]]",
            r#"{"actionable": []} x"#,
            r#"{"actionable": [], "actionable": []}"#,
            r#"{"actionable": [1,"#,
        ] {
            assert_eq!(shape(bad), None, "{bad}");
        }
        let deep = format!("{}{}", "[".repeat(130), "]".repeat(130));
        for middle in [
            br#"1e999"#.to_vec(),
            deep.into_bytes(),
            b"\"\xff\"".to_vec(),
            br#""\ud800""#.to_vec(),
        ] {
            for at in [r#"{"actionable": [1, "#, r#"{"actionable": [], "other": ["#] {
                let mut text = at.as_bytes().to_vec();
                text.extend_from_slice(&middle);
                text.extend_from_slice(b"]}");
                let mut seen = 0;
                assert!(
                    read_list(&text[..], None).is_err(),
                    "{}",
                    String::from_utf8_lossy(&text)
                );
                assert!(read_list(&text[..], Some(&mut |_| seen += 1)).is_err());
            }
        }
        let mut seen = Vec::new();
        let shape = read_list(
            r#"{"actionable": [{"k": 1}, {"k": 2}], "ok": true}"#.as_bytes(),
            Some(&mut |line| seen.push(line)),
        )
        .unwrap();
        assert_eq!(shape.lines, 2);
        assert_eq!(
            seen,
            [serde_json::json!({"k": 1}), serde_json::json!({"k": 2})]
        );
    }

    #[test]
    fn only_newlines_is_an_empty_capture_kept_or_spilled() {
        let check = |bytes: &[u8]| {
            let file = tempfile::tempfile().unwrap();
            std::io::Write::write_all(&mut &file, bytes).unwrap();
            let kept = List::Kept(bytes).only_newlines();
            assert_eq!(List::Spilled(&file).only_newlines(), kept);
            kept
        };
        assert!(check(b""));
        assert!(check(b"\n\n"));
        assert!(!check(b" \n"));
        assert!(!check(b"{}"));
    }

    #[test]
    fn first_line_cuts_bytes_then_takes_the_first_line() {
        assert_eq!(first_line(b"boom\nmore\n", 300), "boom");
        assert_eq!(first_line(b"\nlater", 300), "");
        assert_eq!(first_line(&[b'x'; 400], 300).len(), 300);
        assert_eq!(first_line(b"", 300), "");
    }
}
