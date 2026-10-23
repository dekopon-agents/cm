use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};

use crate::commands;
use crate::epoch::{self, Status};
use crate::forge::GithubRepo;
use crate::host::{Host, System};
use crate::lint;
use crate::outcome::Outcome;
use crate::state::{Evidence, Lane, Phase, Work};
use crate::store::{self, Target, parse_target};

#[derive(Parser)]
#[command(
    name = "cm",
    version,
    about = "Campaign manager: the campaign lifecycle as one tool"
)]
struct Cli {
    /// Print one JSON object on stdout instead of text.
    #[arg(long, global = true)]
    json: bool,

    /// The campaign folder; found by walking up from the current directory when omitted.
    #[arg(short = 'C', long, global = true, value_name = "DIR")]
    root: Option<PathBuf>,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    #[command(about = "Inspect explicit PRs once and route next actions; never advances state")]
    Attention {
        #[arg(long, required = true)]
        pr: Vec<String>,
        #[arg(long)]
        worker: String,
        #[arg(long)]
        coordinator: String,
        #[arg(long, help = "Record changed observations in the campaign journal")]
        record: bool,
    },
    /// Write a campaign folder, a unit folder (<NN-name>/<unit>) inside one, or declare a step (<NN-name>/<unit>/<step> --lane …) in a unit. Never overwrites.
    Init {
        /// The folder to initialize.
        #[arg(default_value = ".")]
        dir: PathBuf,
        /// The step's lane: pi, codex, claude-cloud, fable, owner or coordinator.
        #[arg(long)]
        lane: Option<Lane>,
        /// The step is a study: it ships no PR and finishes with `cm advance <step> done --evidence <path>`.
        #[arg(long)]
        study: bool,
        /// What the step waits on: <step> or <NN-name>/<unit>/<step> (merged), pr-open:<step>, tag:<line>@<version> or deployed:<line>@<version>. Repeat for each.
        #[arg(long, value_name = "AFTER")]
        after: Vec<String>,
        /// A release step's repository, <owner>/<name>; its prep PR must be there. Defaults to any owner's repository named for the release line.
        #[arg(long, value_name = "OWNER/NAME")]
        github: Option<GithubRepo>,
        /// A release step's tag pattern, holding `{version}` once; cm records the one tag it names. Defaults to `v{version}`.
        #[arg(long, value_name = "PATTERN")]
        tag: Option<String>,
    },
    /// List the declared steps whose `--after` is met, or, given one, refuse it unless it is next.
    Next {
        /// <NN-name>/<unit>/<step>: check this step only.
        #[arg(value_parser = step_target)]
        target: Option<Target>,
        /// Only steps in this lane.
        #[arg(long)]
        lane: Option<Lane>,
    },
    /// Read PR state and release tags from GitHub (read-only `gh`), move steps pr-open → merged → released (→ deployed after their release's receipt), and render CARD.md.
    Sync,
    /// Move EPOCH.md along OPEN → PLANNING → WORKING → CLOSED, stamping the date. Refuses a null pin.
    Epoch {
        /// planning, working or closed.
        status: Status,
        /// The skill revision to pin (`skill_revision`).
        #[arg(long, value_name = "REV")]
        skill_revision: Option<String>,
        /// The approved plan revision to pin (`approved_plan_revision`).
        #[arg(long, value_name = "SHA")]
        plan_revision: Option<String>,
    },
    /// Seal the campaign. Refuses while OWNER-QUEUE.md has an open item or any unit or step is short of done or blocked; afterwards unit init, launch and advance refuse and RESUME.md's banner holds no `/loop`.
    Close,
    /// Append a stamped entry to JOURNAL.md.
    Journal {
        /// The entry's title, one line.
        title: String,
        /// A file holding the entry's body; `-` reads stdin.
        #[arg(long, value_name = "FILE")]
        body_file: Option<PathBuf>,
    },
    /// Run the launch checks for a step, mark it launched and print the launch command. Spawns nothing.
    Launch {
        /// <NN-name>/<unit>/<step>
        #[arg(value_parser = step_target)]
        target: Target,
        /// The step's worktree, which must already exist; required for the local pi lane and undeclared steps.
        #[arg(long, value_name = "PATH")]
        worktree: Option<PathBuf>,
        /// A remote launch: the coordinator session that claims its attempt (generation 1).
        #[arg(long, value_name = "LABEL")]
        coordinator: Option<String>,
        /// A remote launch: its cap in minutes, stored as an absolute deadline a replacement inherits.
        #[arg(long, value_name = "MINUTES")]
        cap_min: Option<u32>,
    },
    /// Release a blocked unit or step to the state it was blocked from, with the evidence that cleared it.
    Unblock {
        /// <NN-name>/<unit> or <NN-name>/<unit>/<step>
        #[arg(value_parser = any_target)]
        target: Target,
        /// A non-empty file in the campaign folder, outside `.cm/`, showing what cleared the block.
        #[arg(long, value_name = "PATH")]
        evidence: PathBuf,
    },
    /// Change a drafted or launched step's declaration: mark it a study, move its lane or add a gate.
    Amend {
        /// <NN-name>/<unit>/<step>
        #[arg(value_parser = step_target)]
        target: Target,
        /// The step ships no PR and finishes with `cm advance <step> done --evidence <path>`.
        #[arg(long)]
        study: bool,
        /// The step's new lane.
        #[arg(long)]
        lane: Option<Lane>,
        /// A gate to add, as `cm init --after` takes it. Repeat for each.
        #[arg(long, value_name = "AFTER")]
        after: Vec<String>,
    },
    /// Show, claim or extend a remote launch's attempt record; every external action is recorded before the next.
    Attempt {
        /// <NN-name>/<unit>/<step>
        #[arg(value_parser = step_target)]
        target: Target,
        /// The claim token `cm launch` or `claim` printed; begin, record and end need the current one.
        #[arg(long, global = true, value_name = "TOKEN")]
        claim: Option<String>,
        #[command(subcommand)]
        act: Option<AttemptAct>,
    },
    /// Move a unit or step to its next state with that state's evidence.
    Advance {
        /// <NN-name>/<unit> or <NN-name>/<unit>/<step>
        #[arg(value_parser = any_target)]
        target: Target,
        /// pr-open, merged, released, deployed, done or blocked:<reason>. An owner-lane or study step moves to done, with --evidence, from drafted or launched.
        state: Phase,
        /// A commit: the PR head for pr-open, the merge commit for merged.
        #[arg(long)]
        sha: Option<String>,
        /// The pull request URL (pr-open).
        #[arg(long)]
        pr: Option<String>,
        /// The release tag (released).
        #[arg(long)]
        tag: Option<String>,
        /// sha256:<hex> of the released or deployed artifact.
        #[arg(long)]
        digest: Option<String>,
        /// A non-empty file in the campaign folder, outside `.cm/`, that shows an owner-lane or study step is done (done).
        #[arg(long, value_name = "PATH")]
        evidence: Option<PathBuf>,
        /// The repository --sha and --tag resolve in; defaults to the step worktrees.
        #[arg(long, value_name = "PATH")]
        repo: Option<PathBuf>,
    },
    /// Append an entry, or a follow-up to one, to FEEDBACK.md under the next FB-<sequence>-NNN id.
    Feedback {
        /// The entry's title, one line.
        title: String,
        /// urgent or queued.
        #[arg(long)]
        urgency: Option<String>,
        /// The id a follow-up answers, e.g. FB-d-011.
        #[arg(long, value_name = "ID")]
        follow_up: Option<String>,
        /// A follow-up's disposition: proposed, accepted, applied, deferred or rejected.
        #[arg(long)]
        disposition: Option<String>,
        /// A file holding the body, which may not be empty; `-` reads stdin.
        #[arg(long, value_name = "FILE")]
        body_file: PathBuf,
    },
    /// Add the next numbered item to OWNER-QUEUE.md and withhold RESUME.md's `/loop`.
    OwnerQueue {
        #[command(subcommand)]
        act: QueueAct,
    },
    /// Write <step>/evidence/<NAME>.md once from a non-empty body.
    Evidence {
        /// <NN-name>/<unit>/<step>
        #[arg(value_parser = step_target)]
        target: Target,
        #[arg(long, value_name = "NAME")]
        name: String,
        /// A file holding the body; `-` reads stdin.
        #[arg(long, value_name = "FILE")]
        body_file: PathBuf,
    },
    /// Report every violation in the campaign folder; exit 1 when there is any.
    Lint {
        /// A folder inside the campaign; defaults to --root or the current directory.
        dir: Option<PathBuf>,
    },
}

#[derive(Subcommand)]
enum AttemptAct {
    /// Fence the generation that holds the attempt and take it over; --expect is that generation.
    Claim {
        #[arg(long, value_name = "LABEL")]
        coordinator: String,
        #[arg(long, value_name = "GENERATION")]
        expect: u32,
    },
    /// Record that an external action (thread, turn, session, cancel, …) is about to start.
    Begin { kind: String },
    /// Record the id the begun action returned.
    Record { kind: String, id: String },
    /// End the attempt: complete, partial (with --evidence) or unknown.
    End {
        outcome: crate::state::AttemptOutcome,
        #[arg(long, value_name = "PATH")]
        evidence: Option<PathBuf>,
    },
}

#[derive(Subcommand)]
enum QueueAct {
    /// Append `## N. <title>` with its Context, question, Recommendation and an empty Answer.
    Add {
        title: String,
        #[arg(long)]
        context: String,
        #[arg(long)]
        question: String,
        #[arg(long)]
        recommendation: String,
    },
}

fn step_target(text: &str) -> Result<Target, String> {
    parse_target(text, true)
}

fn any_target(text: &str) -> Result<Target, String> {
    parse_target(text, false)
}

fn under_root(cli: &Cli, path: &Path) -> PathBuf {
    match &cli.root {
        Some(root) if path.is_relative() => root.join(path),
        _ => path.to_path_buf(),
    }
}

fn read_body(path: &Path) -> Result<String> {
    if path == Path::new("-") {
        let mut body = String::new();
        std::io::stdin().read_to_string(&mut body)?;
        return Ok(body);
    }
    std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))
}

fn run(cli: &Cli, host: &dyn Host) -> Result<Outcome> {
    let root = || store::root_or_cwd(cli.root.as_deref());
    match &cli.command {
        Commands::Attention {
            pr,
            worker,
            coordinator,
            record,
        } => crate::attention::run(
            host,
            &crate::attention::Gh,
            &root()?,
            pr,
            worker,
            coordinator,
            *record,
        ),
        Commands::Close => commands::close(host, &root()?),
        Commands::Init {
            dir,
            lane,
            study,
            after,
            github,
            tag,
        } => {
            let declaration = commands::Declaration {
                lane: *lane,
                after: after.clone(),
                ships: commands::Ships {
                    github: github.clone(),
                    tag: tag.clone(),
                },
                work: if *study { Work::Study } else { Work::Code },
            };
            commands::declare(host, &under_root(cli, dir), &declaration)
        }
        Commands::Next { target, lane } => crate::plan::next(&root()?, target.as_ref(), *lane),
        Commands::Sync => crate::sync::sync(host, &crate::forge::Gh::default(), &root()?),
        Commands::Epoch {
            status,
            skill_revision,
            plan_revision,
        } => epoch::epoch(
            host,
            &root()?,
            *status,
            skill_revision.as_deref(),
            plan_revision.as_deref(),
        ),
        Commands::Journal { title, body_file } => {
            let body = body_file.as_deref().map(read_body).transpose()?;
            commands::journal(host, &root()?, title, body.as_deref())
        }
        Commands::Launch {
            target,
            worktree,
            coordinator,
            cap_min,
        } => commands::launch_remote(
            host,
            &root()?,
            target,
            worktree.as_deref(),
            &commands::Remote {
                coordinator: coordinator.clone(),
                cap_min: *cap_min,
            },
        ),
        Commands::Unblock { target, evidence } => {
            commands::unblock(host, &root()?, target, &under_root(cli, evidence))
        }
        Commands::Amend {
            target,
            study,
            lane,
            after,
        } => commands::amend(
            host,
            &root()?,
            target,
            &commands::Amendment {
                study: *study,
                lane: *lane,
                after: after.clone(),
            },
        ),
        Commands::Attempt { target, claim, act } => {
            use crate::attempt::Act;
            let act = match act {
                None => Act::Status,
                Some(AttemptAct::Claim {
                    coordinator,
                    expect,
                }) => Act::Claim {
                    coordinator: coordinator.clone(),
                    expect: *expect,
                },
                Some(AttemptAct::Begin { kind }) => Act::Begin { kind: kind.clone() },
                Some(AttemptAct::Record { kind, id }) => Act::Record {
                    kind: kind.clone(),
                    id: id.clone(),
                },
                Some(AttemptAct::End { outcome, evidence }) => Act::End {
                    outcome: *outcome,
                    file: evidence.as_deref().map(|file| under_root(cli, file)),
                },
            };
            crate::attempt::attempt(host, &root()?, target, claim.as_deref(), &act)
        }
        Commands::Advance {
            target,
            state,
            sha,
            pr,
            tag,
            digest,
            evidence: file,
            repo,
        } => {
            let evidence = Evidence {
                sha: sha.clone(),
                pr: pr.clone(),
                tag: tag.clone(),
                digest: digest.clone(),
                file: None,
            };
            let file = file.as_deref().map(|file| under_root(cli, file));
            commands::advance(
                host,
                &root()?,
                target,
                state,
                &evidence,
                file.as_deref(),
                repo.as_deref(),
            )
        }
        Commands::Feedback {
            title,
            urgency,
            follow_up,
            disposition,
            body_file,
        } => {
            let body = read_body(body_file)?;
            let entry = crate::ledger::Feedback {
                title,
                urgency: urgency.as_deref(),
                follow_up: follow_up.as_deref(),
                disposition: disposition.as_deref(),
                body: &body,
            };
            crate::ledger::feedback(host, &root()?, &entry)
        }
        Commands::OwnerQueue {
            act:
                QueueAct::Add {
                    title,
                    context,
                    question,
                    recommendation,
                },
        } => crate::ledger::owner_queue_add(
            host,
            &root()?,
            &crate::ledger::QueueItem {
                title,
                context,
                question,
                recommendation,
            },
        ),
        Commands::Evidence {
            target,
            name,
            body_file,
        } => {
            let body = read_body(body_file)?;
            crate::ledger::evidence(host, &root()?, target, name, &body)
        }
        Commands::Lint { dir } => {
            let start = match (dir, &cli.root) {
                (Some(dir), _) => dir.clone(),
                (None, Some(root)) => root.clone(),
                (None, None) => std::env::current_dir()?,
            };
            lint::lint(&start)
        }
    }
}

fn name(command: &Commands) -> &'static str {
    match command {
        Commands::Attention { .. } => "attention",
        Commands::Close => "close",
        Commands::Init { .. } => "init",
        Commands::Next { .. } => "next",
        Commands::Sync => "sync",
        Commands::Epoch { .. } => "epoch",
        Commands::Journal { .. } => "journal",
        Commands::Launch { .. } => "launch",
        Commands::Unblock { .. } => "unblock",
        Commands::Amend { .. } => "amend",
        Commands::Attempt { .. } => "attempt",
        Commands::Advance { .. } => "advance",
        Commands::Feedback { .. } => "feedback",
        Commands::OwnerQueue { .. } => "owner-queue",
        Commands::Evidence { .. } => "evidence",
        Commands::Lint { .. } => "lint",
    }
}

pub fn main() -> ExitCode {
    let cli = Cli::parse();
    let command = name(&cli.command);
    match run(&cli, &System) {
        Ok(outcome) => {
            if cli.json {
                println!("{}", outcome.to_json(command));
            } else {
                for violation in &outcome.violations {
                    println!("{violation}");
                }
                for line in &outcome.lines {
                    println!("{line}");
                }
            }
            if outcome.ok() {
                ExitCode::SUCCESS
            } else {
                ExitCode::from(1)
            }
        }
        Err(error) => {
            if cli.json {
                println!(
                    "{}",
                    serde_json::json!({ "command": command, "ok": false, "violations": [], "error": format!("{error:#}") })
                );
            } else {
                eprintln!("cm {command}: {error:#}");
            }
            ExitCode::from(1)
        }
    }
}
