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

pub const KILL_AFTER: Duration = Duration::from_secs(5);

pub const OUTPUT_CAP: usize = 4 << 20;

pub const WHOLE_CAP: usize = 64 << 20;

const GROUP_POLL: Duration = Duration::from_millis(50);

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
    pub keep_stdout: Keep,
    pub keep_stderr: Keep,
    pub stdout_file: Option<PathBuf>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Keep {
    Whole,
    Head,
    Tail,
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
            keep_stdout: Keep::Whole,
            keep_stderr: Keep::Tail,
            stdout_file: None,
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

    pub fn stdout_file(mut self, path: impl Into<PathBuf>) -> Self {
        self.stdout_file = Some(path.into());
        self
    }

    pub fn keep(mut self, stdout: Keep, stderr: Keep) -> Self {
        self.keep_stdout = stdout;
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ran {
    pub exit: Exit,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub stdout_truncated: bool,
    pub stderr_truncated: bool,
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
        let merged = if let Some(path) = &cmd.stdout_file {
            // Straight to disk: output of any size, never held in memory.
            let file = std::fs::File::create(path).map_err(fail)?;
            if cmd.merge_stderr {
                command.stderr(file.try_clone().map_err(fail)?);
            } else {
                command.stderr(Stdio::piped());
            }
            command.stdout(file);
            None
        } else if cmd.merge_stderr {
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
        let (stdout, stdout_truncated) = out.finish();
        let (stderr, stderr_truncated) = err.finish();
        Ok(Ran {
            exit,
            stdout,
            stderr,
            stdout_truncated,
            stderr_truncated,
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
    keep: Keep,
    bytes: Vec<u8>,
    overflowed: bool,
    truncated: bool,
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
            truncated: false,
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

    fn take(&mut self, chunk: &[u8]) {
        let cap = match self.keep {
            Keep::Whole => WHOLE_CAP,
            Keep::Head => OUTPUT_CAP,
            Keep::Tail => {
                self.bytes.extend_from_slice(chunk);
                if self.bytes.len() > 2 * OUTPUT_CAP {
                    self.bytes.drain(..self.bytes.len() - OUTPUT_CAP);
                    self.truncated = true;
                }
                return;
            }
        };
        let room = cap.saturating_sub(self.bytes.len());
        self.bytes
            .extend_from_slice(&chunk[..chunk.len().min(room)]);
        if chunk.len() > room {
            self.truncated = true;
            self.overflowed |= self.keep == Keep::Whole;
        }
    }

    fn finish(mut self) -> (Vec<u8>, bool) {
        if self.keep == Keep::Tail && self.bytes.len() > OUTPUT_CAP {
            self.bytes.drain(..self.bytes.len() - OUTPUT_CAP);
            self.truncated = true;
        }
        (self.bytes, self.truncated)
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
        let mut ran = (self.answer)(cmd)?;
        if let Some(path) = &cmd.stdout_file {
            std::fs::write(path, std::mem::take(&mut ran.stdout)).map_err(|source| {
                TickError::Io {
                    path: path.clone(),
                    source,
                }
            })?;
        }
        Ok(ran)
    }

    fn pause(&self, pause: Duration) {
        self.pauses.borrow_mut().push(pause);
    }
}
