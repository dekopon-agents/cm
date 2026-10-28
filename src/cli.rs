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
    /// Change a drafted or launched step's declaration: mark it a study, move its lane or add or remove gates. --part alone also amends a step that moved on.
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
        /// A gate to remove, as `cm init --after` takes it. Repeat for each.
        #[arg(long, value_name = "AFTER")]
        drop_after: Vec<String>,
        /// A required part (one PR) of a multipart step; the step moves to a state only once every part has. On a step past launched, the first new part takes the step's state and evidence and the step rests at launched. Repeat for each.
        #[arg(long, value_name = "NAME")]
        part: Vec<String>,
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
        /// A codex step's merge gate: the PR head when GitHub cannot be read (else it must match).
        #[arg(long, value_name = "SHA")]
        head: Option<String>,
        /// A codex step's merge gate: a campaign file showing green CI at the head, taken only when GitHub cannot be read.
        #[arg(long, value_name = "PATH")]
        ci_verified: Option<PathBuf>,
        /// Discouraged: waive a codex step's whole merge gate with a non-empty campaign file saying why; journaled with what it waived. For a red check, record `cm known-red` instead; lint warns on an override of a red check no exception names.
        #[arg(long = "override", value_name = "PATH")]
        waiver: Option<PathBuf>,
        /// Move this part of a multipart step; the step follows once every part has.
        #[arg(long, value_name = "NAME")]
        part: Option<String>,
    },
    /// Record a known-red exception: the merge gate accepts this check failing at one commit or, with --standing, across PR heads until the closing condition holds. Refused for a check that is cancelled, timed out or unrun, or green at the PR's base.
    KnownRed {
        /// <NN-name>/<unit>/<step>
        #[arg(value_parser = step_target)]
        target: Target,
        /// The PR head the check failed at, all 40 hex digits.
        #[arg(
            long,
            value_name = "SHA",
            required_unless_present = "standing",
            conflicts_with = "standing"
        )]
        commit: Option<String>,
        /// Cover any PR head until the closing condition holds.
        #[arg(long)]
        standing: bool,
        /// Apply a standing exception to this step or the whole campaign.
        #[arg(long, value_parser = ["step", "campaign"], default_value = "step")]
        scope: String,
        /// The check's exact name as GitHub reports it.
        #[arg(long, value_name = "NAME")]
        check: String,
        /// Why it is red by design, one line.
        #[arg(long)]
        reason: String,
        /// Who answers for the exception.
        #[arg(long)]
        owner: String,
        /// The closing condition, as `cm init --after` takes it: <step> (merged), pr-open:<step>, tag:<line>@<version> or deployed:<line>@<version>.
        #[arg(long, value_name = "AFTER")]
        until: String,
    },
    /// Hold a step below deployed until a live read lands: `add` records the stored signal and its deadline, `resolve` closes it with evidence. Past the deadline it reads expired: unresolved, never a pass.
    Watch {
        /// <NN-name>/<unit>/<step>
        #[arg(value_parser = step_target)]
        target: Target,
        #[command(subcommand)]
        act: WatchAct,
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
        /// Pin the evidence to the commit it was produced against: <step>[@<sha>], the step's open PR head when the sha is left out. `cm sync` marks it stale when that head moves.
        #[arg(long, value_name = "STEP[@SHA]")]
        candidate: Option<String>,
        /// Pin the evidence to the harness it ran under, as --candidate takes it.
        #[arg(long, value_name = "STEP[@SHA]", requires = "candidate")]
        harness: Option<String>,
    },
    /// Append one review row to REVIEW-OFF.md, rendered from `.cm/reviews.json`; --import adopts the hand-written rows once. `record` and `resolve` keep computed verdicts.
    #[command(args_conflicts_with_subcommands = true, subcommand_negates_reqs = true)]
    Review {
        #[command(subcommand)]
        act: Option<ReviewAct>,
        /// <NN-name>/<unit>/<step>
        #[arg(value_parser = step_target, required_unless_present = "import")]
        target: Option<Target>,
        #[arg(long, conflicts_with = "target")]
        import: bool,
        #[arg(long, required_unless_present = "import")]
        pr: Option<String>,
        #[arg(long, required_unless_present = "import")]
        head: Option<String>,
        #[arg(long, required_unless_present = "import")]
        model: Option<String>,
        /// The effort as the runtime reports it.
        #[arg(long, required_unless_present = "import")]
        effort: Option<String>,
        /// accept or fix-required.
        #[arg(long, required_unless_present = "import")]
        verdict: Option<String>,
        /// <confirmed>/<false-positive>
        #[arg(long, required_unless_present = "import")]
        findings: Option<String>,
        /// USD.
        #[arg(long, required_unless_present = "import")]
        cost: Option<String>,
        /// Minutes.
        #[arg(long, required_unless_present = "import")]
        wall: Option<String>,
        /// A delta review: the head it reads from.
        #[arg(long, value_name = "FROM_SHA")]
        delta: Option<String>,
    },
    /// Append a dated ruling to <step>/evidence/coordinator-scope.md, creating it if missing.
    Ruling {
        /// <NN-name>/<unit>/<step>
        #[arg(value_parser = step_target)]
        target: Target,
        /// The ruling, one line.
        text: String,
    },
    /// Print the step's kickoff, then its coordinator-scope.md under a fixed heading: a reviewer's brief.
    Brief {
        /// <NN-name>/<unit>/<step>
        #[arg(value_parser = step_target)]
        target: Target,
    },
    /// List the non-blocking findings recorded by `cm review record`, or close one.
    Followups {
        /// Only the open ones, grouped by repository as checklist lines for the epoch's cleanup PR.
        #[arg(long)]
        epoch: bool,
        #[command(subcommand)]
        act: Option<FollowupAct>,
    },
    /// Report every violation in the campaign folder; exit 1 when there is any.
    Lint {
        /// A folder inside the campaign; defaults to --root or the current directory.
        dir: Option<PathBuf>,
    },
}

#[derive(Subcommand)]
enum WatchAct {
    /// Record a pending read of a stored signal: span:<query>, log:<query> or metric:<query>.
    Add {
        #[arg(long)]
        signal: String,
        /// An RFC 3339 instant, e.g. 2026-10-27T06:00Z.
        #[arg(long)]
        deadline: String,
    },
    /// Close a watch with the evidence that shows its signal.
    Resolve {
        id: u32,
        #[arg(long, value_name = "PATH")]
        evidence: PathBuf,
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
enum ReviewAct {
    /// Recover an interrupted review save from persisted rows and reconcile the journal.
    Render,
    /// Parse a report's `cm-review` block, compute its verdict and record it under the step's evidence.
    Record {
        /// <NN-name>/<unit>/<step>
        #[arg(value_parser = step_target)]
        target: Target,
        /// The review report, ending with its `cm-review` block.
        report: PathBuf,
        /// Who reviewed: astra, claude, …
        #[arg(long, value_name = "KIND")]
        reviewer: Option<String>,
    },
    /// Record what resolved a blocking finding: a commit, or a ruling file in the campaign.
    Resolve {
        /// <NN-name>/<unit>/<step>
        #[arg(value_parser = step_target)]
        target: Target,
        /// The finding: R<n>.<id>, or <id> in the step's latest review holding it.
        finding: String,
        #[arg(long, value_name = "COMMIT|RULING-FILE")]
        by: String,
    },
}

#[derive(Subcommand)]
enum FollowupAct {
    /// Close a follow-up with the commit that fixed it or the reason it stays.
    Close {
        /// R<n>.<id>, as `cm followups` lists it.
        id: String,
        #[arg(long, value_name = "COMMIT|REASON")]
        by: String,
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
        Commands::Next { target, lane } => {
            crate::plan::next_at(&root()?, target.as_ref(), *lane, host.now())
        }
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
            drop_after,
            part,
        } => commands::amend(
            host,
            &root()?,
            target,
            &commands::Amendment {
                study: *study,
                lane: *lane,
                after: after.clone(),
                drop_after: drop_after.clone(),
                parts: part.clone(),
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
            head,
            ci_verified,
            waiver,
            part,
        } => {
            let evidence = Evidence {
                sha: sha.clone(),
                pr: pr.clone(),
                tag: tag.clone(),
                digest: digest.clone(),
                file: None,
            };
            let file = file.as_deref().map(|file| under_root(cli, file));
            let forge = crate::forge::Gh::default();
            let gate = commands::MergeGate {
                forge: Some(&forge),
                head: head.clone(),
                ci_verified: ci_verified.as_deref().map(|file| under_root(cli, file)),
                waiver: waiver.as_deref().map(|file| under_root(cli, file)),
            };
            commands::advance_gated(
                host,
                &root()?,
                target,
                commands::Advance {
                    to: state,
                    given: &evidence,
                    file: file.as_deref(),
                    repo: repo.as_deref(),
                    part: part.as_deref(),
                },
                &gate,
            )
        }
        Commands::KnownRed {
            target,
            commit,
            standing,
            scope,
            check,
            reason,
            owner,
            until,
        } => crate::holds::known_red(
            host,
            &crate::forge::Gh::default(),
            &root()?,
            target,
            &crate::holds::KnownRedArgs {
                commit: commit.as_deref().unwrap_or_default(),
                standing: *standing,
                campaign: scope == "campaign",
                check,
                reason,
                owner,
                until,
            },
        ),
        Commands::Watch {
            target,
            act: WatchAct::Add { signal, deadline },
        } => crate::holds::watch_add(host, &root()?, target, signal, deadline),
        Commands::Watch {
            target,
            act: WatchAct::Resolve { id, evidence },
        } => crate::holds::watch_resolve(host, &root()?, target, *id, &under_root(cli, evidence)),
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
            candidate,
            harness,
        } => {
            let body = read_body(body_file)?;
            let pin = crate::ledger::PinArgs {
                candidate: candidate.as_deref(),
                harness: harness.as_deref(),
            };
            crate::ledger::evidence_pinned(host, &root()?, target, name, &body, &pin)
        }
        Commands::Review {
            act:
                Some(ReviewAct::Record {
                    target,
                    report,
                    reviewer,
                }),
            ..
        } => crate::verdict::record(
            host,
            &root()?,
            target,
            &under_root(cli, report),
            reviewer.as_deref(),
        ),
        Commands::Review {
            act:
                Some(ReviewAct::Resolve {
                    target,
                    finding,
                    by,
                }),
            ..
        } => crate::verdict::resolve(
            host,
            &root()?,
            target,
            finding,
            by,
            &under_root(cli, Path::new(by)),
        ),
        Commands::Review {
            act: Some(ReviewAct::Render),
            ..
        } => crate::review::recover_reviews(&root()?),
        Commands::Review { import: true, .. } => crate::review::import_reviews(host, &root()?),
        Commands::Review {
            target,
            pr,
            head,
            model,
            effort,
            verdict,
            findings,
            cost,
            wall,
            delta,
            ..
        } => {
            let text = |value: &Option<String>| value.clone().unwrap_or_default();
            let (pr, head, model, effort) = (text(pr), text(head), text(model), text(effort));
            let (verdict, findings, cost, wall) =
                (text(verdict), text(findings), text(cost), text(wall));
            let args = crate::review::ReviewArgs {
                pr: &pr,
                head: &head,
                model: &model,
                effort: &effort,
                verdict: &verdict,
                findings: &findings,
                cost: &cost,
                wall: &wall,
                delta: delta.as_deref(),
            };
            let target = target
                .clone()
                .expect("clap requires a target without --import");
            crate::review::review(host, &root()?, &target, &args)
        }
        Commands::Ruling { target, text } => crate::verdict::ruling(host, &root()?, target, text),
        Commands::Brief { target } => crate::verdict::brief(&root()?, target),
        Commands::Followups { epoch, act: None } => crate::verdict::followups(&root()?, *epoch),
        Commands::Followups {
            act: Some(FollowupAct::Close { id, by }),
            ..
        } => crate::verdict::close_followup(host, &root()?, id, by),
        Commands::Lint { dir } => {
            let start = match (dir, &cli.root) {
                (Some(dir), _) => dir.clone(),
                (None, Some(root)) => root.clone(),
                (None, None) => std::env::current_dir()?,
            };
            lint::lint_at(&start, host.now())
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
        Commands::KnownRed { .. } => "known-red",
        Commands::Watch { .. } => "watch",
        Commands::Feedback { .. } => "feedback",
        Commands::OwnerQueue { .. } => "owner-queue",
        Commands::Evidence { .. } => "evidence",
        Commands::Review { .. } => "review",
        Commands::Ruling { .. } => "ruling",
        Commands::Brief { .. } => "brief",
        Commands::Followups { .. } => "followups",
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
