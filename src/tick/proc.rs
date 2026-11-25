//! Child processes for the tick: always an argv, never a shell string.
//!
//! Every child runs in a process group of its own. At its timeout the whole group gets TERM. The
//! group then has [`KILL_AFTER`] to finish (GNU `timeout --kill-after=5`, which heartbeat.sh
//! used), whether or not the leader has already exited, before the whole group gets KILL, so a
//! child's own children go with it and a descendant that cleans up on TERM gets the full grace.
//! The group leader is not reaped until the KILL has been sent, so its id cannot be reused by an
//! unrelated group in between. A child that exits but leaves a descendant holding its stdout or
//! stderr open counts as running until the timeout.
//!
//! Each output stream is kept by its [`Keep`]: whole (a producer's JSON; past [`WHOLE_CAP`] the
//! child is killed and exits [`Exit::TooLarge`], never cut silently), its head (a first line) or
//! its tail (a last line).
//!
//! [`SystemProc`] is the real runner; [`FakeProc`] answers from a closure and records every
//! call, for tests.

use std::cell::RefCell;
use std::ffi::{OsStr, OsString};
use std::io::{ErrorKind, Read, Write};
use std::os::fd::AsFd;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use rustix::process::{Pid, Signal, WaitId, WaitIdOptions};

use crate::tick::{Result, TickError};

/// How long a timed-out child's process group has between TERM and KILL.
pub const KILL_AFTER: Duration = Duration::from_secs(5);

/// Bytes a [`Keep::Head`] or [`Keep::Tail`] stream keeps.
pub const OUTPUT_CAP: usize = 4 << 20;

/// The most bytes a [`Keep::Whole`] stream holds; one byte more kills the child with its process
/// group, and it exits [`Exit::TooLarge`].
pub const WHOLE_CAP: usize = 64 << 20;

/// How often the runner looks for live members of a TERMed group during its grace.
const GROUP_POLL: Duration = Duration::from_millis(50);

/// How long the runner sleeps when a child has neither exited nor written.
const POLL: Duration = Duration::from_millis(10);

/// The most buffers one pump of a pipe reads before the runner checks the clock again.
const PUMP_READS: usize = 64;

/// One child to run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cmd {
    /// `argv[0]` is the program, looked up on `PATH` when it has no `/`.
    pub argv: Vec<OsString>,
    /// The working directory; `None` keeps the tick's own.
    pub cwd: Option<PathBuf>,
    /// Variables added to the inherited environment.
    pub env: Vec<(OsString, OsString)>,
    /// Written to the child's stdin, which is then closed; `None` gives it `/dev/null`.
    pub stdin: Option<Vec<u8>>,
    /// After this the whole process group gets TERM, then KILL [`KILL_AFTER`] later.
    pub timeout: Duration,
    /// stderr shares stdout's pipe, interleaved as written (`2>&1`); [`Ran::stderr`] stays empty.
    pub merge_stderr: bool,
    /// What is kept of stdout (with `merge_stderr`, of both); [`Keep::Whole`] by default.
    pub keep_stdout: Keep,
    /// What is kept of stderr; [`Keep::Tail`] by default.
    pub keep_stderr: Keep,
}

/// What a runner keeps of one output stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Keep {
    /// Every byte, for output that is parsed, up to [`WHOLE_CAP`]; past it the child is killed
    /// with its process group and exits [`Exit::TooLarge`].
    Whole,
    /// The first [`OUTPUT_CAP`] bytes, for a first line.
    Head,
    /// The last [`OUTPUT_CAP`] bytes, for a last line.
    Tail,
}

impl Cmd {
    /// The timeout a new `Cmd` has; a [`Ctx`](crate::tick::act::Ctx) lowers it to its budget.
    pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(120);

    pub fn new(program: impl AsRef<OsStr>) -> Self {
        Cmd {
            argv: vec![program.as_ref().to_owned()],
            cwd: None,
            env: Vec::new(),
            stdin: None,
            timeout: Self::DEFAULT_TIMEOUT,
            merge_stderr: false,
            keep_stdout: Keep::Whole,
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

    pub fn keep(mut self, stdout: Keep, stderr: Keep) -> Self {
        self.keep_stdout = stdout;
        self.keep_stderr = stderr;
        self
    }

    /// The program's file name: `/usr/bin/git` is `git`.
    pub fn program_name(&self) -> String {
        self.argv.first().map_or_else(String::new, |program| {
            Path::new(program)
                .file_name()
                .unwrap_or(program)
                .to_string_lossy()
                .into_owned()
        })
    }

    /// The call as a fake script logs it: the program's file name, then its arguments, spaced.
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

/// How a child ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Exit {
    Code(i32),
    Signal(i32),
    /// It ran past its timeout and was killed with its process group.
    TimedOut,
    /// It printed more than [`WHOLE_CAP`] to a stream kept whole and was killed with its process
    /// group; that stream holds only its first [`WHOLE_CAP`] bytes.
    TooLarge,
}

impl Exit {
    pub fn success(self) -> bool {
        self == Exit::Code(0)
    }

    /// The status a shell reports: the code, 128 plus the signal, GNU timeout's 124, or 137 (a
    /// kill) for [`Exit::TooLarge`].
    pub fn shell_status(self) -> i32 {
        match self {
            Exit::Code(code) => code,
            Exit::Signal(signal) => 128 + signal,
            Exit::TimedOut => 124,
            Exit::TooLarge => 137,
        }
    }
}

/// What a child left.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ran {
    pub exit: Exit,
    pub stdout: Vec<u8>,
    /// Empty when the command merged it into stdout.
    pub stderr: Vec<u8>,
    pub wall: Duration,
}

impl Ran {
    /// A child that exited with `code` after printing `stdout`; for fakes.
    pub fn code(code: i32, stdout: impl Into<Vec<u8>>) -> Self {
        Ran {
            exit: Exit::Code(code),
            stdout: stdout.into(),
            stderr: Vec::new(),
            wall: Duration::ZERO,
        }
    }

    /// A child killed at its timeout before printing anything.
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

/// Runs children. The tick reaches every program through this, so tests can fake them all.
pub trait Proc {
    /// Runs `cmd` until it exits, or until its timeout and the kill of its process group.
    /// `Err` only when it could not be started or waited for; an exit status is data.
    fn run(&self, cmd: &Cmd) -> Result<Ran>;
}

/// The real runner.
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
        command
            .args(&cmd.argv[1..])
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
                Drain::new(err, cmd.keep_stderr)?,
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
            if !moved {
                let left = deadline.map_or(POLL, |deadline| deadline - now);
                std::thread::sleep(left.min(POLL));
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
        Ok(Ran {
            exit,
            stdout: out.finish(),
            stderr: err.finish(),
            wall: started.elapsed(),
        })
    }
}

/// Signals the child's process group, and the child itself in case it left the group. The
/// child is not reaped yet, so neither id can belong to anyone else.
fn kill(pid: Pid, signal: Signal) {
    let _ = rustix::process::kill_process_group(pid, signal);
    let _ = rustix::process::kill_process(pid, signal);
}

/// Whether any member of the process group `pgid` is still running; a zombie is not. Linux reads
/// `/proc`; elsewhere a group cannot be told apart from its unreaped leader, so it reads as
/// running and the whole grace is waited.
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

/// Whether the group leader has exited, leaving it unreaped so its pid stays reserved.
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

/// One output pipe, read without blocking, kept by its [`Keep`].
struct Drain {
    reader: Option<Box<dyn Stream>>,
    keep: Keep,
    bytes: Vec<u8>,
    /// A [`Keep::Whole`] stream passed [`WHOLE_CAP`].
    overflowed: bool,
}

impl Drain {
    fn new(reader: Option<Box<dyn Stream>>, keep: Keep) -> std::io::Result<Self> {
        if let Some(reader) = &reader {
            nonblocking(reader.as_fd())?;
        }
        Ok(Drain {
            reader,
            keep,
            bytes: Vec::new(),
            overflowed: false,
        })
    }

    fn done(&self) -> bool {
        self.reader.is_none()
    }

    /// Reads what is there, at most [`PUMP_READS`] buffers so a flood cannot hold off the
    /// deadline; true when it read anything or reached the end.
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

    fn take(&mut self, chunk: &[u8]) {
        match self.keep {
            Keep::Whole => {
                let room = WHOLE_CAP.saturating_sub(self.bytes.len());
                self.bytes
                    .extend_from_slice(&chunk[..chunk.len().min(room)]);
                self.overflowed |= chunk.len() > room;
            }
            Keep::Head => {
                let room = OUTPUT_CAP.saturating_sub(self.bytes.len());
                self.bytes
                    .extend_from_slice(&chunk[..chunk.len().min(room)]);
            }
            Keep::Tail => {
                self.bytes.extend_from_slice(chunk);
                if self.bytes.len() > 2 * OUTPUT_CAP {
                    self.bytes.drain(..self.bytes.len() - OUTPUT_CAP);
                }
            }
        }
    }

    fn finish(mut self) -> Vec<u8> {
        if self.keep == Keep::Tail && self.bytes.len() > OUTPUT_CAP {
            self.bytes.drain(..self.bytes.len() - OUTPUT_CAP);
        }
        self.bytes
    }
}

/// The child's stdin, written without blocking, then closed.
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

    /// Writes what the pipe takes; true when it wrote anything or closed the pipe.
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

/// A runner for tests: answers every call from a closure and records it.
pub struct FakeProc {
    calls: RefCell<Vec<Cmd>>,
    answer: Box<Answer>,
}

impl FakeProc {
    pub fn new(answer: impl Fn(&Cmd) -> Result<Ran> + 'static) -> Self {
        FakeProc {
            calls: RefCell::new(Vec::new()),
            answer: Box::new(answer),
        }
    }

    /// Every call so far, in order.
    pub fn calls(&self) -> Vec<Cmd> {
        self.calls.borrow().clone()
    }

    /// Every call so far as [`Cmd::line`] prints it.
    pub fn log(&self) -> Vec<String> {
        self.calls.borrow().iter().map(Cmd::line).collect()
    }
}

impl Proc for FakeProc {
    fn run(&self, cmd: &Cmd) -> Result<Ran> {
        self.calls.borrow_mut().push(cmd.clone());
        (self.answer)(cmd)
    }
}
