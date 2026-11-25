use std::cell::RefCell;
use std::collections::TryReserveError;
use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::io::{ErrorKind, Read, Write};
use std::os::fd::AsFd;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use rustix::fs::{FileType, OFlags, SeekFrom};
use rustix::process::{Pid, Resource, Signal, WaitId, WaitIdOptions};

use crate::tick::{Result, TickError};

pub const KILL_AFTER: Duration = Duration::from_secs(5);

pub const OUTPUT_CAP: usize = 4 << 20;

pub const WHOLE_CAP: usize = 64 << 20;

const GROUP_POLL: Duration = Duration::from_millis(50);

const POLL_FIRST: Duration = Duration::from_millis(1);

const POLL: Duration = Duration::from_millis(10);

const PUMP_READS: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cmd {
    pub argv: Vec<OsString>,
    pub cwd: Option<PathBuf>,
    pub env: Vec<(OsString, OsString)>,
    pub env_remove: Vec<OsString>,
    pub stdin: Option<Vec<u8>>,
    pub timeout: Duration,
    pub merge_stderr: bool,
    pub keep_stdout: StdoutKeep,
    pub keep_stderr: Keep,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Keep {
    Whole,
    Head,
    Tail,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StdoutKeep {
    Keep(Keep),
    Spill,
}

impl From<Keep> for StdoutKeep {
    fn from(keep: Keep) -> Self {
        StdoutKeep::Keep(keep)
    }
}

impl PartialEq<Keep> for StdoutKeep {
    fn eq(&self, keep: &Keep) -> bool {
        *self == StdoutKeep::Keep(*keep)
    }
}

impl Cmd {
    pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(120);

    pub fn new(program: impl AsRef<OsStr>) -> Self {
        Cmd {
            argv: vec![program.as_ref().to_owned()],
            cwd: None,
            env: Vec::new(),
            env_remove: Vec::new(),
            stdin: None,
            timeout: Self::DEFAULT_TIMEOUT,
            merge_stderr: false,
            keep_stdout: StdoutKeep::Keep(Keep::Whole),
            keep_stderr: Keep::Tail,
        }
    }

    pub fn arg(mut self, arg: impl AsRef<OsStr>) -> Self {
        self.argv.push(arg.as_ref().to_owned());
        self
    }

    pub fn args<I: IntoIterator<Item = S>, S: AsRef<OsStr>>(mut self, args: I) -> Self {
        self.argv
            .extend(args.into_iter().map(|arg| arg.as_ref().to_owned()));
        self
    }

    pub fn cwd(mut self, dir: impl Into<PathBuf>) -> Self {
        self.cwd = Some(dir.into());
        self
    }

    pub fn env(mut self, key: impl AsRef<OsStr>, value: impl AsRef<OsStr>) -> Self {
        self.env
            .push((key.as_ref().to_owned(), value.as_ref().to_owned()));
        self
    }

    pub fn env_remove(mut self, key: impl AsRef<OsStr>) -> Self {
        self.env_remove.push(key.as_ref().to_owned());
        self
    }

    pub fn stdin(mut self, bytes: impl Into<Vec<u8>>) -> Self {
        self.stdin = Some(bytes.into());
        self
    }

    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    pub fn merge_stderr(mut self) -> Self {
        self.merge_stderr = true;
        self
    }

    pub fn keep(mut self, stdout: impl Into<StdoutKeep>, stderr: Keep) -> Self {
        self.keep_stdout = stdout.into();
        self.keep_stderr = stderr;
        self
    }

    pub fn program_name(&self) -> String {
        self.argv.first().map_or_else(String::new, |program| {
            Path::new(program)
                .file_name()
                .unwrap_or(program)
                .to_string_lossy()
                .into_owned()
        })
    }

    pub fn line(&self) -> String {
        std::iter::once(self.program_name())
            .chain(
                self.argv
                    .iter()
                    .skip(1)
                    .map(|arg| arg.to_string_lossy().into_owned()),
            )
            .collect::<Vec<_>>()
            .join(" ")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Exit {
    Code(i32),
    Signal(i32),
    TimedOut,
    TooLarge,
}

impl Exit {
    pub fn success(self) -> bool {
        self == Exit::Code(0)
    }

    pub fn shell_status(self) -> i32 {
        match self {
            Exit::Code(code) => code,
            Exit::Signal(signal) => 128 + signal,
            Exit::TimedOut => 124,
            Exit::TooLarge => 137,
        }
    }
}

#[derive(Debug, Clone)]
pub enum Spilled {
    Stored(Arc<File>),
    Failed(Arc<std::io::Error>),
}

impl PartialEq for Spilled {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Spilled::Stored(one), Spilled::Stored(other)) => Arc::ptr_eq(one, other),
            (Spilled::Failed(one), Spilled::Failed(other)) => Arc::ptr_eq(one, other),
            _ => false,
        }
    }
}

impl Eq for Spilled {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ran {
    pub exit: Exit,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub stdout_truncated: bool,
    pub stderr_truncated: bool,
    pub stdout_spill: Option<Spilled>,
    pub wall: Duration,
}

impl Ran {
    pub fn code(code: i32, stdout: impl Into<Vec<u8>>) -> Self {
        Ran {
            exit: Exit::Code(code),
            stdout: stdout.into(),
            stderr: Vec::new(),
            stdout_truncated: false,
            stderr_truncated: false,
            stdout_spill: None,
            wall: Duration::ZERO,
        }
    }

    pub fn timed_out() -> Self {
        Ran {
            exit: Exit::TimedOut,
            ..Ran::code(0, Vec::new())
        }
    }

    pub fn with_stderr(mut self, stderr: impl Into<Vec<u8>>) -> Self {
        self.stderr = stderr.into();
        self
    }

    pub fn stdout_text(&self) -> String {
        String::from_utf8_lossy(&self.stdout).into_owned()
    }

    pub fn stderr_text(&self) -> String {
        String::from_utf8_lossy(&self.stderr).into_owned()
    }
}

pub trait Proc {
    fn run(&self, cmd: &Cmd) -> Result<Ran>;

    fn pause(&self, pause: Duration) {
        std::thread::sleep(pause);
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct SystemProc;

impl Proc for SystemProc {
    fn run(&self, cmd: &Cmd) -> Result<Ran> {
        let started = Instant::now();
        let program = cmd.argv.first().cloned().unwrap_or_default();
        let fail = |source: std::io::Error| TickError::Spawn {
            program: program.to_string_lossy().into_owned(),
            source,
        };
        if program.is_empty() {
            return Err(fail(ErrorKind::InvalidInput.into()));
        }
        let mut command = Command::new(&program);
        command.args(&cmd.argv[1..]);
        for key in &cmd.env_remove {
            command.env_remove(key);
        }
        command
            .envs(cmd.env.iter().map(|(key, value)| (key, value)))
            .process_group(0)
            .stdin(if cmd.stdin.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            });
        if let Some(dir) = &cmd.cwd {
            command.current_dir(dir);
        }
        let merged = if cmd.merge_stderr {
            let (reader, writer) = std::io::pipe().map_err(fail)?;
            command
                .stdout(writer.try_clone().map_err(fail)?)
                .stderr(writer);
            Some(reader)
        } else {
            command.stdout(Stdio::piped()).stderr(Stdio::piped());
            None
        };
        let mut child = command.spawn().map_err(fail)?;
        // The command holds the write end of a merged pipe; drop it or the pipe never ends.
        drop(command);
        let pid = Pid::from_child(&child);
        let pipes = (|| {
            let out: Option<Box<dyn Stream>> = match merged {
                Some(reader) => Some(Box::new(reader)),
                None => child.stdout.take().map(|s| Box::new(s) as Box<dyn Stream>),
            };
            let err = child.stderr.take().map(|s| Box::new(s) as Box<dyn Stream>);
            Ok((
                Drain::new(out, cmd.keep_stdout)?,
                Drain::new(err, StdoutKeep::Keep(cmd.keep_stderr))?,
                Feed::new(child.stdin.take(), cmd.stdin.as_deref())?,
            ))
        })();
        let (mut out, mut err, mut feed) = match pipes {
            Ok(pipes) => pipes,
            Err(error) => {
                kill(pid, Signal::KILL);
                let _ = child.wait();
                return Err(fail(error));
            }
        };
        let deadline = started.checked_add(cmd.timeout);
        let mut exited = false;
        let mut idle = POLL_FIRST;
        let end = loop {
            let moved = feed.push() | out.pump() | err.pump();
            if out.overflowed || err.overflowed {
                break Some(Exit::TooLarge);
            }
            exited = exited || leader_exited(pid);
            if exited && out.done() && err.done() {
                break None;
            }
            let now = Instant::now();
            if deadline.is_some_and(|deadline| now >= deadline) {
                break Some(Exit::TimedOut);
            }
            // A full pipe is drained within a millisecond, so on one CPU the capture is not held to
            // a pipe's worth per POLL; an idle child backs off to POLL.
            if moved {
                idle = POLL_FIRST;
            } else {
                let left = deadline.map_or(idle, |deadline| deadline - now);
                std::thread::sleep(left.min(idle));
                idle = (idle * 2).min(POLL);
            }
        };
        drop(feed);
        if end.is_some() {
            kill(pid, Signal::TERM);
            // The grace runs out on the clock or when no member is left running, never merely
            // because the leader exited: a descendant cleaning up on TERM gets its time.
            let grace = Instant::now() + KILL_AFTER;
            while group_running(pid) && Instant::now() < grace {
                if !(out.pump() | err.pump()) {
                    std::thread::sleep(GROUP_POLL);
                }
            }
            // Also reaches what ignored TERM. The leader is still unreaped, so the group id
            // cannot have been reused.
            kill(pid, Signal::KILL);
            out.pump();
            err.pump();
        }
        let status = child.wait().map_err(fail)?;
        let exit = if let Some(end) = end {
            end
        } else if let Some(code) = status.code() {
            Exit::Code(code)
        } else {
            Exit::Signal(status.signal().unwrap_or(0))
        };
        let (stdout, stdout_truncated, stdout_spill) = out.finish();
        let (stderr, stderr_truncated, _) = err.finish();
        Ok(Ran {
            exit,
            stdout,
            stderr,
            stdout_truncated,
            stderr_truncated,
            stdout_spill,
            wall: started.elapsed(),
        })
    }
}

fn kill(pid: Pid, signal: Signal) {
    let _ = rustix::process::kill_process_group(pid, signal);
    let _ = rustix::process::kill_process(pid, signal);
}

fn group_running(pgid: Pid) -> bool {
    #[cfg(target_os = "linux")]
    {
        let Ok(entries) = std::fs::read_dir("/proc") else {
            return true;
        };
        let pgid = pgid.as_raw_nonzero().get().to_string();
        entries.flatten().any(|entry| {
            let is_pid = entry
                .file_name()
                .to_str()
                .is_some_and(|name| name.bytes().all(|byte| byte.is_ascii_digit()));
            // `<pid> (<comm>) <state> <ppid> <pgrp> …`; the command may hold spaces or `)`.
            is_pid
                && std::fs::read_to_string(entry.path().join("stat")).is_ok_and(|stat| {
                    let mut fields = stat
                        .rsplit_once(") ")
                        .map_or("", |(_, rest)| rest)
                        .split_whitespace();
                    let state = fields.next();
                    let pgrp = fields.nth(1);
                    pgrp == Some(pgid.as_str()) && !matches!(state, Some("Z" | "X") | None)
                })
        })
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = pgid;
        true
    }
}

fn leader_exited(pid: Pid) -> bool {
    let options = WaitIdOptions::EXITED | WaitIdOptions::NOHANG | WaitIdOptions::NOWAIT;
    match rustix::process::waitid(WaitId::Pid(pid), options) {
        Ok(None) | Err(rustix::io::Errno::INTR) => false,
        // Any other error means there is nothing left to wait for; `Child::wait` reports it.
        Ok(Some(_)) | Err(_) => true,
    }
}

trait Stream: Read + AsFd {}
impl<T: Read + AsFd> Stream for T {}

fn nonblocking(fd: impl AsFd) -> std::io::Result<()> {
    use rustix::fs::{OFlags, fcntl_getfl, fcntl_setfl};
    let flags = fcntl_getfl(&fd)?;
    fcntl_setfl(&fd, flags | OFlags::NONBLOCK)?;
    Ok(())
}

struct Drain {
    reader: Option<Box<dyn Stream>>,
    keep: StdoutKeep,
    bytes: Vec<u8>,
    overflowed: bool,
    truncated: bool,
    spill: Option<std::io::Result<SpillFile>>,
    open_spill: fn() -> std::io::Result<File>,
    reserve: fn(&mut Vec<u8>, usize) -> std::result::Result<(), TryReserveError>,
}

impl Drain {
    fn new(reader: Option<Box<dyn Stream>>, keep: StdoutKeep) -> std::io::Result<Self> {
        if let Some(reader) = &reader {
            nonblocking(reader.as_fd())?;
        }
        Ok(Drain {
            reader,
            keep,
            bytes: Vec::new(),
            overflowed: false,
            truncated: false,
            spill: None,
            // Unlinked from the start, so a killed tick leaves nothing behind.
            open_spill: tempfile::tempfile,
            reserve: Vec::try_reserve,
        })
    }

    fn done(&self) -> bool {
        self.reader.is_none()
    }

    fn pump(&mut self) -> bool {
        let mut buffer = [0; 8192];
        let mut moved = false;
        for _ in 0..PUMP_READS {
            let Some(reader) = &mut self.reader else {
                return moved;
            };
            match reader.read(&mut buffer) {
                Ok(0) => {
                    self.reader = None;
                    return true;
                }
                Ok(n) => {
                    self.take(&buffer[..n]);
                    moved = true;
                }
                Err(error) if error.kind() == ErrorKind::Interrupted => {}
                Err(error) if error.kind() == ErrorKind::WouldBlock => return moved,
                Err(_) => {
                    self.reader = None;
                    return true;
                }
            }
        }
        moved
    }

    // Memory that cannot be had is output not kept, never an abort.
    fn hold(&mut self, bytes: &[u8]) -> bool {
        let held = (self.reserve)(&mut self.bytes, bytes.len()).is_ok();
        if held {
            self.bytes.extend_from_slice(bytes);
        }
        held
    }

    fn take(&mut self, chunk: &[u8]) {
        let keep = match self.keep {
            StdoutKeep::Spill => return self.spill(chunk),
            StdoutKeep::Keep(keep) => keep,
        };
        let cap = match keep {
            Keep::Whole => WHOLE_CAP,
            Keep::Head => OUTPUT_CAP,
            Keep::Tail => {
                if !self.hold(chunk) {
                    self.truncated = true;
                } else if self.bytes.len() > 2 * OUTPUT_CAP {
                    self.bytes.drain(..self.bytes.len() - OUTPUT_CAP);
                    self.truncated = true;
                }
                return;
            }
        };
        let room = cap.saturating_sub(self.bytes.len());
        let held = self.hold(&chunk[..chunk.len().min(room)]);
        if chunk.len() > room || !held {
            self.truncated = true;
            self.overflowed |= keep == Keep::Whole;
        }
    }

    fn spill(&mut self, chunk: &[u8]) {
        if self.spill.is_none() && self.bytes.len() + chunk.len() <= WHOLE_CAP && self.hold(chunk) {
            return;
        }
        let written = match &mut self.spill {
            Some(Err(_)) => return,
            Some(Ok(file)) => file.write(chunk),
            None => {
                let held = std::mem::take(&mut self.bytes);
                let stored = (self.open_spill)().and_then(|file| {
                    let mut file = SpillFile::new(file);
                    file.write(&held)?;
                    file.write(chunk)?;
                    Ok(file)
                });
                match stored {
                    Ok(file) => {
                        self.spill = Some(Ok(file));
                        return;
                    }
                    Err(error) => Err(error),
                }
            }
        };
        if let Err(error) = written {
            self.spill = Some(Err(error));
            self.overflowed = true;
        }
    }

    fn finish(mut self) -> (Vec<u8>, bool, Option<Spilled>) {
        if self.keep == Keep::Tail && self.bytes.len() > OUTPUT_CAP {
            self.bytes.drain(..self.bytes.len() - OUTPUT_CAP);
            self.truncated = true;
        }
        let spill = self.spill.map(|spill| match spill {
            Ok(file) => Spilled::Stored(Arc::new(file.file)),
            Err(error) => Spilled::Failed(Arc::new(error)),
        });
        (self.bytes, self.truncated, spill)
    }
}

struct SpillFile {
    file: File,
    len: u64,
    limit: Option<u64>,
}

impl SpillFile {
    fn new(file: File) -> Self {
        SpillFile {
            file,
            len: 0,
            limit: rustix::process::getrlimit(Resource::Fsize).current,
        }
    }

    // A write past RLIMIT_FSIZE would raise SIGXFSZ, which kills the tick before the write can
    // fail, so it fails here instead.
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<()> {
        let len = self.len + bytes.len() as u64;
        if self.limit.is_some_and(|limit| len > limit) {
            return Err(rustix::io::Errno::FBIG.into());
        }
        self.file.write_all(bytes)?;
        self.len = len;
        Ok(())
    }
}

// RLIMIT_FSIZE for a write to a regular file the tick did not open, such as its stdout.
pub struct FileSizeLimit(u64);

impl FileSizeLimit {
    pub fn of(fd: impl AsFd) -> Option<Self> {
        let limit = rustix::process::getrlimit(Resource::Fsize).current?;
        let stat = rustix::fs::fstat(fd).ok()?;
        (FileType::from_raw_mode(stat.st_mode) == FileType::RegularFile)
            .then_some(FileSizeLimit(limit))
    }

    pub fn fits(&self, fd: impl AsFd, len: usize) -> bool {
        let fd = fd.as_fd();
        let at = if rustix::fs::fcntl_getfl(fd).is_ok_and(|flags| flags.contains(OFlags::APPEND)) {
            rustix::fs::fstat(fd).map(|stat| stat.st_size.unsigned_abs())
        } else {
            rustix::fs::seek(fd, SeekFrom::Current(0))
        };
        at.is_ok_and(|at| at.saturating_add(len as u64) <= self.0)
    }
}

struct Feed<'a> {
    writer: Option<std::process::ChildStdin>,
    rest: &'a [u8],
}

impl<'a> Feed<'a> {
    fn new(
        writer: Option<std::process::ChildStdin>,
        bytes: Option<&'a [u8]>,
    ) -> std::io::Result<Self> {
        if let Some(writer) = &writer {
            nonblocking(writer)?;
        }
        Ok(Feed {
            writer,
            rest: bytes.unwrap_or_default(),
        })
    }

    fn push(&mut self) -> bool {
        let Some(writer) = &mut self.writer else {
            return false;
        };
        let mut moved = false;
        while !self.rest.is_empty() {
            match writer.write(self.rest) {
                Ok(n) => {
                    self.rest = &self.rest[n..];
                    moved = true;
                }
                Err(error) if error.kind() == ErrorKind::Interrupted => {}
                Err(error) if error.kind() == ErrorKind::WouldBlock => return moved,
                // The child closed its end (EPIPE) or the pipe broke: stop feeding.
                Err(_) => break,
            }
        }
        self.writer = None;
        true
    }
}

type Answer = dyn Fn(&Cmd) -> Result<Ran>;

pub struct FakeProc {
    calls: RefCell<Vec<Cmd>>,
    pauses: RefCell<Vec<Duration>>,
    answer: Box<Answer>,
}

impl FakeProc {
    pub fn new(answer: impl Fn(&Cmd) -> Result<Ran> + 'static) -> Self {
        FakeProc {
            calls: RefCell::new(Vec::new()),
            pauses: RefCell::new(Vec::new()),
            answer: Box::new(answer),
        }
    }

    pub fn calls(&self) -> Vec<Cmd> {
        self.calls.borrow().clone()
    }

    pub fn pauses(&self) -> Vec<Duration> {
        self.pauses.borrow().clone()
    }

    pub fn log(&self) -> Vec<String> {
        self.calls.borrow().iter().map(Cmd::line).collect()
    }
}

impl Proc for FakeProc {
    fn run(&self, cmd: &Cmd) -> Result<Ran> {
        self.calls.borrow_mut().push(cmd.clone());
        (self.answer)(cmd)
    }

    fn pause(&self, pause: Duration) {
        self.pauses.borrow_mut().push(pause);
    }
}

#[cfg(test)]
mod tests {
    use std::fs::write as fs_write;
    use std::io::{Seek, SeekFrom};

    use super::*;

    fn spilling(open_spill: fn() -> std::io::Result<File>) -> Drain {
        Drain {
            open_spill,
            ..Drain::new(None, StdoutKeep::Spill).unwrap()
        }
    }

    fn unreachable_store() -> std::io::Result<File> {
        panic!("nothing up to the cap is stored")
    }

    #[test]
    fn a_spill_keeps_64_mib_in_memory_and_moves_the_rest_to_an_unlinked_file() {
        let chunk = vec![b'x'; 1 << 20];
        let mut kept = spilling(unreachable_store);
        for _ in 0..WHOLE_CAP >> 20 {
            kept.take(&chunk);
        }
        let (bytes, truncated, spill) = kept.finish();
        assert_eq!((bytes.len(), truncated, spill), (WHOLE_CAP, false, None));

        let mut spilled = spilling(tempfile::tempfile);
        for _ in 0..WHOLE_CAP >> 20 {
            spilled.take(&chunk);
        }
        spilled.take(b"yz");
        assert!(!spilled.overflowed);
        let (bytes, truncated, spill) = spilled.finish();
        assert_eq!((bytes.len(), truncated), (0, false));
        let Some(Spilled::Stored(file)) = spill else {
            panic!("{spill:?}");
        };
        let mut file = &*file;
        assert_eq!(file.seek(SeekFrom::End(0)).unwrap(), WHOLE_CAP as u64 + 2);
        file.seek(SeekFrom::End(-3)).unwrap();
        let mut tail = Vec::new();
        file.read_to_end(&mut tail).unwrap();
        assert_eq!(tail, b"xyz");
    }

    #[test]
    fn a_hold_that_cannot_get_memory_spills_or_stops_keeping_instead_of_aborting() {
        fn no_memory(_: &mut Vec<u8>, _: usize) -> std::result::Result<(), TryReserveError> {
            Vec::<u8>::new().try_reserve(usize::MAX)
        }
        let starved = |keep: StdoutKeep| Drain {
            reserve: no_memory,
            ..Drain::new(None, keep).unwrap()
        };
        let mut spill = starved(StdoutKeep::Spill);
        spill.take(b"abc");
        assert!(!spill.overflowed);
        let (bytes, _, stored) = spill.finish();
        assert!(bytes.is_empty());
        let Some(Spilled::Stored(file)) = stored else {
            panic!("{stored:?}");
        };
        let mut file = &*file;
        file.seek(SeekFrom::Start(0)).unwrap();
        let mut kept = Vec::new();
        file.read_to_end(&mut kept).unwrap();
        assert_eq!(kept, b"abc");
        for (keep, overflowed) in [
            (Keep::Whole, true),
            (Keep::Head, false),
            (Keep::Tail, false),
        ] {
            let mut drain = starved(keep.into());
            drain.take(b"abc");
            assert_eq!(
                (drain.overflowed, drain.truncated),
                (overflowed, true),
                "{keep:?}"
            );
            assert!(drain.finish().0.is_empty(), "{keep:?}");
        }
    }

    #[test]
    fn a_write_past_the_file_size_limit_fails_before_it_is_made() {
        let mut spill = SpillFile {
            limit: Some(10),
            ..SpillFile::new(tempfile::tempfile().unwrap())
        };
        spill.write(b"123456").unwrap();
        let error = spill.write(b"78901").unwrap_err();
        assert_eq!(error.kind(), ErrorKind::FileTooLarge);
        assert_eq!(spill.len, 6);
        assert_eq!(spill.file.metadata().unwrap().len(), 6);
        spill.write(b"7890").unwrap();

        let mut file = tempfile::tempfile().unwrap();
        file.write_all(b"12345").unwrap();
        let limit = FileSizeLimit(10);
        assert!(limit.fits(&file, 5));
        assert!(!limit.fits(&file, 6));
        file.seek(SeekFrom::Start(0)).unwrap();
        assert!(limit.fits(&file, 10));
        let appending = tempfile::NamedTempFile::new().unwrap();
        fs_write(appending.path(), b"12345").unwrap();
        let appending = File::options().append(true).open(appending.path()).unwrap();
        assert!(!limit.fits(&appending, 6), "an append lands at the end");
    }

    #[test]
    fn a_spill_that_cannot_be_stored_stops_the_child_and_keeps_the_error() {
        fn full() -> std::io::Result<File> {
            File::options().write(true).open("/dev/full")
        }
        fn missing() -> std::io::Result<File> {
            Err(ErrorKind::NotFound.into())
        }
        for (store, kind) in [
            (
                full as fn() -> std::io::Result<File>,
                ErrorKind::StorageFull,
            ),
            (missing, ErrorKind::NotFound),
        ] {
            let mut drain = spilling(store);
            drain.take(&vec![b'x'; WHOLE_CAP]);
            assert!(!drain.overflowed);
            drain.take(b"y");
            assert!(drain.overflowed, "the run loop kills the child on this");
            drain.take(b"discarded");
            let (bytes, _, spill) = drain.finish();
            assert!(bytes.is_empty());
            let Some(Spilled::Failed(error)) = spill else {
                panic!("{spill:?}");
            };
            assert_eq!(error.kind(), kind);
        }
    }
}
