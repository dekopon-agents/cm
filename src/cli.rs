use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result};
use clap::{CommandFactory, Parser, Subcommand};

use crate::attempt::coordinator_label;
use crate::commands;
use crate::epoch::{self, Status};
use crate::forge::GithubRepo;
use crate::host::{Host, System};
use crate::idea::{self, Maturity, Word};
use crate::lint;
use crate::outcome::{Outcome, Violation};
use crate::state::{Evidence, LANES, Lane, Phase, Work};
use crate::store::{self, Target, parse_target};
use crate::tick::proc::FileSizeLimit;

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
        /// Required parts, one PR each. Repeat for each part.
        #[arg(long)]
        part: Vec<String>,
        /// The folder to initialize.
        #[arg(default_value = ".")]
        dir: PathBuf,
        #[arg(long, help = format!("The step's lane. Choose from {}.", LANES.map(Lane::name).join(", ")))]
        lane: Option<Lane>,
        /// The step is a study: it ships no PR and finishes with `cm advance <step> done --evidence <path>`.
        #[arg(long)]
        study: bool,
        /// A rollout deploys directly from merged with --sha and --evidence, without a release.
        #[arg(long, value_parser = ["rollout"], conflicts_with = "study")]
        kind: Option<String>,
        /// What the step waits on: <step> or <NN-name>/<unit>/<step> (merged), pr-open:<step>, tag:<line>@<version> or deployed:<line>@<version>. Repeat for each.
        #[arg(long, value_name = "AFTER")]
        after: Vec<String>,
        /// A release step's repository, <owner>/<name>; its prep PR must be there. With neither release flag, inherits the newest declared release; otherwise defaults to any owner's repository named for the release line.
        #[arg(long, value_name = "OWNER/NAME")]
        github: Option<GithubRepo>,
        /// A release step's tag pattern, holding `{version}` once; cm records the one tag it names. With neither release flag, inherits the newest declared release; otherwise defaults to `v{version}`.
        #[arg(long, value_name = "PATTERN")]
        tag: Option<String>,
        /// A published crate, tap formula or ghcr.io image, without a version. Repeat for each.
        #[arg(long, value_name = "SPEC")]
        artifact: Vec<String>,
        /// Write kickoff.md from a non-empty file of at most 1 MiB, or `-` for stdin. Never overwrites an existing kickoff.
        #[arg(long, value_name = "F")]
        kickoff_file: Option<PathBuf>,
    },
    /// List the launchable steps (drafted, `--after` met, kickoff written, no pause or quota hold), or, given one, refuse it unless it is next.
    Next {
        /// <NN-name>/<unit>/<step>: check this step only.
        #[arg(value_parser = step_target)]
        target: Option<Target>,
        /// Only steps in this lane.
        #[arg(long)]
        lane: Option<Lane>,
        /// Print only coordinator actions, launchable steps among them. Exit 1 when any remain.
        #[arg(long)]
        act: bool,
    },
    /// Read PR state and release tags from GitHub (read-only `gh`), move steps pr-open → merged → released (→ deployed after their release's receipt), and render CARD.md.
    Sync {
        #[arg(
            long,
            value_name = "SECONDS",
            default_value_t = crate::sync::BUDGET.as_secs(),
            help = "Print each phase's wall time when the sync takes longer than this many seconds (it always does when the sync fails)"
        )]
        budget_secs: u64,
    },
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
    /// Print files at pinned commits, never the working tree: whole up to 8 KB, else an outline unless given a range or heading; every line numbered; about 64 KB per call.
    Read {
        /// A pins table (`## Pins`, as `cm idea facts` writes) that resolves `<alias>:` specs; with pins, every spec must be pinned.
        #[arg(long, value_name = "FILE")]
        pins: Option<PathBuf>,
        /// One pin, ALIAS=DIR@SHA. Repeat for each.
        #[arg(long, value_name = "ALIAS=DIR@SHA")]
        pin: Vec<crate::source::Pin>,
        /// <alias>:<path>, <dir>@<sha>:<path> or <sha>:<path>, each optionally :N, :A-B, :A- or :#Heading.
        #[arg(required = true, value_name = "SPEC")]
        specs: Vec<String>,
    },
    /// Admit an idea, move its maturity forward, or record its outcome; build review packets.
    Idea {
        #[command(subcommand)]
        act: IdeaAct,
    },
    /// Seal the campaign after a clean `cm lint` (warnings do not block). Refuses while OWNER-QUEUE.md has an open item or any unit or step is short of done or blocked; afterwards unit init, launch and advance refuse and RESUME.md's banner holds no `/loop`.
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
        /// Launch despite kickoff parts not declared in state; journal the reason.
        #[arg(long, value_name = "REASON")]
        no_parts: Option<String>,
        /// <NN-name>/<unit>/<step>
        #[arg(value_parser = step_target)]
        target: Target,
        /// The step's worktree, which must already exist; required for the local pi lane and undeclared steps.
        #[arg(long, value_name = "PATH")]
        worktree: Option<PathBuf>,
        /// A remote launch: the coordinator session that claims its attempt (generation 1).
        /// Defaults to trimmed CLAUDE_CODE_REMOTE_SESSION_ID (cse_ becomes session_), else —.
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
        /// A non-empty campaign file showing what cleared the block; optional when a step's until condition holds.
        #[arg(long, value_name = "PATH")]
        evidence: Option<PathBuf>,
    },
    /// Change a drafted or launched step's declaration. Release --github/--tag/--artifact/--drop-artifact also allow pr-open or merged. --part alone also amends a step that moved on.
    Amend {
        /// <NN-name>/<unit>/<step>, or <NN-name>/<unit> with --priority
        #[arg(value_parser = any_target)]
        target: Target,
        /// A unit's priority: `now` sorts its lines first in `cm next`, `--act` and CARD.
        #[arg(long, value_enum)]
        priority: Option<crate::state::Priority>,
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
        /// Remove a declared artifact before the release reaches released.
        #[arg(long, value_name = "SPEC")]
        drop_artifact: Vec<String>,
        /// A required part (one PR) of a multipart step; the step moves to a state only once every part has. On a step past launched, the first new part takes the step's state and evidence and the step rests at launched. Repeat for each.
        #[arg(long, value_name = "NAME")]
        part: Vec<String>,
        /// Replace kickoff.md from a non-empty file of at most 1 MiB, or `-` for stdin.
        #[arg(long, value_name = "F")]
        kickoff_file: Option<PathBuf>,
        /// A release step's new repository; its prep PR must be there.
        #[arg(long, value_name = "OWNER/NAME")]
        github: Option<GithubRepo>,
        /// A release step's new tag pattern, holding `{version}` once.
        #[arg(long, value_name = "PATTERN")]
        tag: Option<String>,
        /// A published crate, tap formula or ghcr.io image, without a version. Repeat for each.
        #[arg(long, value_name = "SPEC")]
        artifact: Vec<String>,
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
    #[command(
        about = "Register the session that drives a campaign lane (LANE-<name>.md) for its units, list the registered drivers, or remove one. A live attempt a registered driver claimed on a step under its units is not `earlier-coordinator`"
    )]
    Lane {
        #[command(subcommand)]
        act: LaneAct,
    },
    /// Move a unit or step to its next state with that state's evidence.
    Advance {
        /// <NN-name>/<unit> or <NN-name>/<unit>/<step>
        #[arg(value_parser = any_target)]
        target: Target,
        /// pr-open, merged, released, deployed, done, superseded or blocked:<reason>. A Mac-lane, non-rollout owner-lane or study step moves to done with --evidence.
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
        /// A non-empty file in the campaign folder, outside `.cm/`, that shows a step is done or superseded.
        #[arg(long, value_name = "PATH")]
        evidence: Option<PathBuf>,
        /// Declared successors of a superseded step, comma-separated; short names resolve in its unit.
        #[arg(long, value_delimiter = ',', value_name = "STEP[,STEP…]")]
        by: Vec<String>,
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
    /// Hold a step below deployed until a live read lands: `add` records the stored signal and its deadline, `resolve` closes it with evidence, `withdraw` closes it with a reason, and `amend` corrects its signal. Past the deadline it reads expired: unresolved, never a pass.
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
    /// Add, answer, strike or edit numbered items in OWNER-QUEUE.md.
    OwnerQueue {
        #[command(subcommand)]
        act: QueueAct,
    },
    /// File work only the owner's Mac can do as the next numbered MAC-LANE.md item, mark one done, or list the open ones.
    MacLane {
        #[command(subcommand)]
        act: MacLaneAct,
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
    #[command(
        about = "The coordinator's routine moves once, as heartbeat.sh made them: choose the one PLANNING or WORKING epoch, then run each `cm --json next --act` line's executor in the bound coordinator, or print the line. Prints only lines left for the model; exits 0 whenever it ran",
        long_about = "The coordinator's routine moves once, as heartbeat.sh made them: choose the one PLANNING or WORKING epoch, then, in the bound coordinator only, run each `cm --json next --act` line's executor or print the line. Prints only lines left for the model, and nothing past the epoch choice in any other session; exits 0 whenever the loop ran"
    )]
    Tick {
        #[arg(
            long,
            value_name = "DIR",
            default_value = ".",
            help = "The campaign-trail checkout holding `store/epochs/`"
        )]
        checkout: PathBuf,
        #[arg(
            long,
            value_name = "PROGRAM",
            help = "The cm that `cm next --act` and the executors' cm calls run; defaults to this executable. A bare name is looked up on PATH"
        )]
        cm: Option<PathBuf>,
        #[arg(
            long,
            help = "Only answer whether this session is the bound coordinator, for the Stop hook: print nothing and exit 0 if it is, 1 if not or when there is not exactly one live epoch. Runs git and handoff-pick.sh, never cm"
        )]
        check_bound: bool,
    },
    /// Report every violation in the campaign folder; exit 1 when there is any.
    Lint {
        /// A folder inside the campaign; defaults to --root or the current directory.
        dir: Option<PathBuf>,
    },
}

#[derive(Subcommand)]
enum IdeaAct {
    /// Admit an idea to this epoch, completing an interrupted admission when needed.
    Admit { id: String },
    /// Move maturity forward: captured, exploring or designed; skipping is allowed.
    Move { id: String, maturity: Maturity },
    /// Record one outcome for this idea in this epoch; partial/deferred remainders become unassigned.
    Outcome {
        id: String,
        word: Word,
        /// Use unassigned for partial/deferred; a later epoch admits the idea with `cm idea admit`.
        #[arg(long, value_name = "unassigned")]
        to: Option<String>,
        /// An existing idea id; required for superseded.
        #[arg(long)]
        by: Option<String>,
        /// One line of evidence or reason; required for landed, partial and cancelled.
        #[arg(long)]
        note: Option<String>,
    },
    /// Write a review pass's FACTS.md: verify every pin, then resolve each record's file citations at the pins (ok or STALE, with the enclosing heading or item).
    Facts {
        /// ALIAS=DIR@SHA, a repository at a commit. Repeat for each.
        #[arg(long, required = true, value_name = "ALIAS=DIR@SHA")]
        pin: Vec<crate::source::Pin>,
        /// Write the packet here; prints it when omitted.
        #[arg(long, value_name = "FILE")]
        out: Option<PathBuf>,
        /// The ideas folder; defaults to the campaign's `ideas_dir`.
        #[arg(long, value_name = "DIR")]
        ideas: Option<PathBuf>,
        /// Record ids, one section each.
        #[arg(required = true)]
        ids: Vec<String>,
    },
    /// Check identifiers, env vars and `IDEA-NNNN:line` refs added since a base against the records each record links.
    Idents {
        /// The revision the session started from.
        #[arg(long, value_name = "REV")]
        base: String,
        /// The ideas folder; defaults to the campaign's `ideas_dir`.
        #[arg(long, value_name = "DIR")]
        ideas: Option<PathBuf>,
        #[arg(required = true)]
        ids: Vec<String>,
    },
    /// Move ruled-on review sections of large records to `history/<id>.md`, leaving heading stubs. Prints what would move unless --apply.
    SplitHistory {
        #[arg(long)]
        apply: bool,
        /// Only records of at least this many bytes.
        #[arg(long, value_name = "N", default_value_t = crate::history::MIN_BYTES)]
        min_bytes: usize,
        /// The date the stubs name; defaults to today (UTC).
        #[arg(long, value_name = "YYYY-MM-DD")]
        date: Option<String>,
        /// The ideas folder; defaults to the campaign's `ideas_dir`.
        #[arg(long, value_name = "DIR")]
        ideas: Option<PathBuf>,
        /// Record id prefixes; all records when omitted.
        ids: Vec<String>,
    },
}

#[derive(Subcommand)]
enum WatchAct {
    /// Close an unresolved watch with a reason, releasing its hold.
    Withdraw {
        id: u32,
        #[arg(long)]
        reason: String,
    },
    /// Replace an unresolved watch's stored signal, keeping its deadline.
    Amend {
        id: u32,
        #[arg(long)]
        signal: String,
    },
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
    /// End an unknown attempt as relaned and dispatch its successor under the same claim and cap.
    Restart {
        #[arg(long)]
        lane: crate::state::Lane,
        #[arg(long, value_name = "PATH")]
        evidence: Option<PathBuf>,
    },
    /// Fence the generation that holds the attempt and take it over; --expect is that generation.
    Claim {
        /// Defaults to trimmed CLAUDE_CODE_REMOTE_SESSION_ID (cse_ becomes session_); otherwise required.
        #[arg(long, value_name = "LABEL", required = coordinator_label(&None).is_none())]
        coordinator: Option<String>,
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
enum LaneAct {
    #[command(
        about = "The sessions the coordinator registered to drive campaign lanes, recorded in `.cm/lane-drivers.json` and journaled"
    )]
    Driver {
        #[command(subcommand)]
        act: DriverAct,
    },
}

#[derive(Subcommand)]
enum DriverAct {
    #[command(
        about = "Record that --session drives lane <NAME> for the steps under each --unit, replacing the lane's earlier driver"
    )]
    Add {
        #[arg(help = "The lane's name, as in LANE-<name>.md")]
        name: String,
        #[arg(
            long,
            value_name = "LABEL",
            help = "The driver's session label, as its claims record it (e.g. session_01…)"
        )]
        session: String,
        #[arg(
            long,
            value_name = "PREFIX",
            help = "<NN-name>[/<unit>[/<step>]]: the driver's steps are those under it. Repeat for each"
        )]
        unit: Vec<String>,
        #[arg(
            long,
            value_name = "LABEL",
            required = coordinator_label(&None).is_none(),
            help = "The coordinator registering the driver. Defaults to trimmed CLAUDE_CODE_REMOTE_SESSION_ID (cse_ becomes session_); otherwise required"
        )]
        coordinator: Option<String>,
    },
    #[command(about = "List the registered drivers by lane")]
    List,
    #[command(
        about = "Remove lane <NAME>'s driver; its claims read as `earlier-coordinator` again"
    )]
    Remove {
        #[arg(help = "The lane's name, as in LANE-<name>.md")]
        name: String,
        #[arg(
            long,
            value_name = "LABEL",
            required = coordinator_label(&None).is_none(),
            help = "The coordinator removing the driver. Defaults to trimmed CLAUDE_CODE_REMOTE_SESSION_ID (cse_ becomes session_); otherwise required"
        )]
        coordinator: Option<String>,
    },
}

#[derive(Subcommand)]
enum ReviewAct {
    /// Block until this review round is recorded or the absolute deadline passes.
    Wait {
        #[arg(value_parser = step_target)]
        target: Target,
        #[arg(long, value_parser = commit_sha)]
        head: String,
        #[arg(long, value_parser = reviewer_name)]
        reviewer: String,
        #[arg(long)]
        deadline: jiff::Timestamp,
        #[arg(long)]
        part: Option<String>,
    },
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
        /// The declared part reviewed by this report.
        #[arg(long, value_name = "NAME")]
        part: Option<String>,
        #[arg(
            long,
            value_parser = ["resolution"],
            help = "The report reviewed only the conflict resolutions of the merge from the base branch at its head; with both families' ACCEPT the merge gate's walk passes that merge"
        )]
        kind: Option<String>,
        #[arg(
            long,
            value_name = "PATH",
            requires = "kind",
            help = "The checkout --kind resolution reads the merge in, with origin/HEAD as the base branch's tip; defaults to the step's worktree"
        )]
        repo: Option<PathBuf>,
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
    /// Read the valid queue and journal an explicit acknowledgement of its current items.
    Read,
    /// Append `## N. <title>` with its Context, question, Recommendation and an empty Answer.
    Add {
        title: String,
        #[arg(long)]
        context: String,
        #[arg(long)]
        question: String,
        #[arg(long)]
        recommendation: String,
        /// Also block this step on the new item (`blocked:until:owner-queue:N`), in the same write.
        #[arg(long, value_name = "STEP", value_parser = step_target)]
        block: Option<Target>,
    },
    /// Journal the answer and remove the numbered item.
    Answer {
        number: u32,
        /// A non-empty answer body; `-` reads stdin.
        #[arg(long, value_name = "FILE")]
        body_file: PathBuf,
    },
    /// Remove the numbered item and journal why.
    Strike {
        number: u32,
        #[arg(long)]
        reason: String,
    },
    /// Replace the numbered item's body, keeping its heading and required fields.
    Edit {
        number: u32,
        /// A non-empty item body; `-` reads stdin.
        #[arg(long, value_name = "FILE")]
        body_file: PathBuf,
    },
}

#[derive(Subcommand)]
enum MacLaneAct {
    /// Stamp an open item PARTIAL, keeping its instructions and leaving it open.
    Partial {
        number: u32,
        #[arg(long)]
        receipt: String,
    },
    /// Append `N. **<title>** <body>`, numbering on from the highest item.
    Add {
        title: String,
        /// A file holding the body; `-` reads stdin.
        #[arg(long, value_name = "FILE")]
        body_file: Option<PathBuf>,
        /// Link the item to a declared step.
        #[arg(long, value_parser = step_target, value_name = "NN-name/unit/step")]
        step: Option<Target>,
    },
    /// Stamp item N `**DONE <date>** (<receipt>).`, replacing a leading PARTIAL, HELD, RUNNING or Receipt span.
    Done {
        number: u32,
        #[arg(long)]
        receipt: String,
    },
    /// List the open items: number, title and state (open, held, running or partial).
    List,
}

fn commit_sha(text: &str) -> Result<String, String> {
    crate::state::is_commit(text)
        .then(|| text.to_owned())
        .ok_or_else(|| "expected a commit SHA (7 to 40 hexadecimal digits)".into())
}

fn reviewer_name(text: &str) -> Result<String, String> {
    crate::store::is_name(text)
        .then(|| text.to_owned())
        .ok_or_else(|| "expected a reviewer name of letters, digits, `.`, `_` and `-`".into())
}

fn step_target(text: &str) -> Result<Target, String> {
    parse_target(text, true)
}

fn any_target(text: &str) -> Result<Target, String> {
    parse_target(text, false)
}

fn ideas_folder(cli: &Cli, given: &Option<PathBuf>) -> Result<PathBuf> {
    match given {
        Some(dir) => Ok(dir.clone()),
        None => idea::ideas_dir(&store::root_or_cwd(cli.root.as_deref())?),
    }
}

fn under_root(cli: &Cli, path: &Path) -> PathBuf {
    match &cli.root {
        Some(root) if path.is_relative() => root.join(path),
        _ => path.to_path_buf(),
    }
}

const KICKOFF_MAX_BYTES: usize = 1024 * 1024;

#[derive(Debug)]
struct InputTooLarge {
    path: PathBuf,
    limit: usize,
}

impl std::fmt::Display for InputTooLarge {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "kickoff input exceeds the {}-byte limit", self.limit)
    }
}

impl std::error::Error for InputTooLarge {}

fn read_body(path: &Path, limit: Option<usize>) -> Result<String> {
    let reader: Box<dyn Read> = if path == Path::new("-") {
        Box::new(std::io::stdin().lock())
    } else {
        Box::new(std::fs::File::open(path).with_context(|| format!("reading {}", path.display()))?)
    };
    let mut bytes = Vec::new();
    reader
        .take(limit.map_or(u64::MAX, |limit| limit as u64 + 1))
        .read_to_end(&mut bytes)
        .with_context(|| format!("reading {}", path.display()))?;
    if let Some(limit) = limit
        && bytes.len() > limit
    {
        return Err(InputTooLarge {
            path: path.to_owned(),
            limit,
        }
        .into());
    }
    String::from_utf8(bytes).with_context(|| format!("reading {}", path.display()))
}

fn run(cli: &Cli, host: &dyn Host) -> Result<(Outcome, ExitCode)> {
    let root = || store::root_or_cwd(cli.root.as_deref());
    let outcome = match &cli.command {
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
        Commands::Read { pins, pin, specs } => Ok(crate::source::read(
            specs,
            &crate::source::load_pins(pins.as_deref(), pin)?,
        )),
        Commands::Idea { act } => match act {
            IdeaAct::Admit { id } => idea::run(host, &root()?, id, idea::Action::Admit),
            IdeaAct::Move { id, maturity } => {
                idea::run(host, &root()?, id, idea::Action::Move(*maturity))
            }
            IdeaAct::Outcome {
                id,
                word,
                to,
                by,
                note,
            } => idea::run(
                host,
                &root()?,
                id,
                idea::Action::Outcome {
                    word: *word,
                    to: to.as_deref(),
                    by: by.as_deref(),
                    note: note.as_deref(),
                },
            ),
            IdeaAct::Facts {
                pin,
                out,
                ideas,
                ids,
            } => crate::facts::facts(&crate::facts::FactsArgs {
                pins: pin,
                ideas: &ideas_folder(cli, ideas)?,
                ids,
                out: out.as_deref(),
            }),
            IdeaAct::Idents { base, ideas, ids } => {
                crate::facts::idents(&ideas_folder(cli, ideas)?, base, ids)
            }
            IdeaAct::SplitHistory {
                apply,
                min_bytes,
                date,
                ideas,
                ids,
            } => crate::history::split(&crate::history::SplitArgs {
                ideas: &ideas_folder(cli, ideas)?,
                ids,
                apply: *apply,
                min_bytes: *min_bytes,
                date: &date
                    .clone()
                    .unwrap_or_else(|| host.now().strftime("%Y-%m-%d").to_string()),
            }),
        },
        Commands::Close => commands::close(host, &root()?),
        Commands::Init {
            part,
            dir,
            lane,
            study,
            kind,
            after,
            github,
            tag,
            artifact,
            kickoff_file,
        } => {
            let declaration = commands::Declaration {
                artifacts: artifact.clone(),
                parts: part.clone(),
                lane: *lane,
                after: after.clone(),
                ships: commands::Ships {
                    github: github.clone(),
                    tag: tag.clone(),
                },
                work: if *study {
                    Work::Study
                } else if kind.is_some() {
                    Work::Rollout
                } else {
                    Work::Code
                },
                kickoff: kickoff_file
                    .as_deref()
                    .map(|path| read_body(path, Some(KICKOFF_MAX_BYTES)))
                    .transpose()?,
            };
            commands::declare(host, &under_root(cli, dir), &declaration)
        }
        Commands::Next { target, lane, act } => {
            if *act {
                let forge = crate::forge::Gh::default();
                let (outcome, actionable) = crate::plan::next_act(
                    &root()?,
                    target.as_ref(),
                    *lane,
                    host.now(),
                    Some(&forge),
                )?;
                return Ok((outcome, ExitCode::from(u8::from(actionable))));
            }
            crate::plan::next_with(
                &root()?,
                target.as_ref(),
                *lane,
                host.now(),
                Some(&crate::forge::Gh::default()),
            )
        }
        Commands::Sync { budget_secs } => crate::sync::sync_with(
            host,
            crate::sync::Reader::Concurrent(&crate::forge::Gh::default()),
            &root()?,
            std::time::Duration::from_secs(*budget_secs),
        ),
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
            let body = body_file
                .as_deref()
                .map(|path| read_body(path, None))
                .transpose()?;
            commands::journal(host, &root()?, title, body.as_deref())
        }
        Commands::Launch {
            no_parts,
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
                no_parts: no_parts.clone(),
                coordinator: coordinator_label(coordinator),
                cap_min: *cap_min,
            },
        ),
        Commands::Unblock { target, evidence } => {
            let file = evidence.as_ref().map(|path| under_root(cli, path));
            commands::unblock(host, &root()?, target, file.as_deref())
        }
        Commands::Amend {
            target,
            priority,
            study,
            lane,
            after,
            drop_after,
            drop_artifact,
            artifact,
            part,
            kickoff_file,
            github,
            tag,
        } => commands::amend(
            host,
            &root()?,
            target,
            &commands::Amendment {
                study: *study,
                lane: *lane,
                after: after.clone(),
                drop_after: drop_after.clone(),
                artifacts: artifact.clone(),
                drop_artifacts: drop_artifact.clone(),
                parts: part.clone(),
                kickoff: kickoff_file
                    .as_deref()
                    .map(|path| read_body(path, Some(KICKOFF_MAX_BYTES)))
                    .transpose()?,
                ships: commands::Ships {
                    github: github.clone(),
                    tag: tag.clone(),
                },
                priority: *priority,
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
                    coordinator: coordinator_label(coordinator)
                        .expect("clap requires --coordinator without a session default"),
                    expect: *expect,
                },
                Some(AttemptAct::Restart { lane, evidence }) => Act::Restart {
                    lane: *lane,
                    file: evidence.as_deref().map(|file| under_root(cli, file)),
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
        Commands::Lane {
            act:
                LaneAct::Driver {
                    act:
                        DriverAct::Add {
                            name,
                            session,
                            unit,
                            coordinator,
                        },
                },
        } => crate::attempt::driver_add(
            host,
            &root()?,
            name,
            session,
            unit,
            &coordinator_label(coordinator)
                .expect("clap requires --coordinator without a session default"),
        ),
        Commands::Lane {
            act: LaneAct::Driver {
                act: DriverAct::List,
            },
        } => crate::attempt::driver_list(&root()?),
        Commands::Lane {
            act:
                LaneAct::Driver {
                    act: DriverAct::Remove { name, coordinator },
                },
        } => crate::attempt::driver_remove(
            host,
            &root()?,
            name,
            &coordinator_label(coordinator)
                .expect("clap requires --coordinator without a session default"),
        ),
        Commands::Advance {
            target,
            state,
            sha,
            pr,
            tag,
            digest,
            evidence: file,
            by,
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
                ..Evidence::default()
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
                    by,
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
        Commands::Watch {
            target,
            act: WatchAct::Withdraw { id, reason },
        } => crate::holds::watch_withdraw(host, &root()?, target, *id, reason),
        Commands::Watch {
            target,
            act: WatchAct::Amend { id, signal },
        } => crate::holds::watch_amend(host, &root()?, target, *id, signal),
        Commands::Feedback {
            title,
            urgency,
            follow_up,
            disposition,
            body_file,
        } => {
            let body = read_body(body_file, None)?;
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
            act: QueueAct::Read,
        } => crate::ledger::owner_queue_read(host, &root()?),
        Commands::OwnerQueue {
            act:
                QueueAct::Add {
                    title,
                    context,
                    question,
                    recommendation,
                    block,
                },
        } => crate::ledger::owner_queue_add_blocking(
            host,
            &root()?,
            &crate::ledger::QueueItem {
                title,
                context,
                question,
                recommendation,
            },
            block.as_ref(),
        ),
        Commands::OwnerQueue {
            act: QueueAct::Answer { number, body_file },
        } => {
            let body = read_body(body_file, None)?;
            crate::ledger::owner_queue_answer(host, &root()?, *number, &body)
        }
        Commands::OwnerQueue {
            act: QueueAct::Strike { number, reason },
        } => crate::ledger::owner_queue_strike(host, &root()?, *number, reason),
        Commands::OwnerQueue {
            act: QueueAct::Edit { number, body_file },
        } => {
            let body = read_body(body_file, None)?;
            crate::ledger::owner_queue_edit(host, &root()?, *number, &body)
        }
        Commands::MacLane {
            act:
                MacLaneAct::Add {
                    title,
                    body_file,
                    step,
                },
        } => {
            let body = body_file
                .as_deref()
                .map(|path| read_body(path, None))
                .transpose()?;
            crate::ledger::mac_lane_add_with_step(
                host,
                &root()?,
                title,
                body.as_deref().unwrap_or_default(),
                step.as_ref(),
            )
        }
        Commands::MacLane {
            act: MacLaneAct::Partial { number, receipt },
        } => crate::ledger::mac_lane_partial(host, &root()?, *number, receipt),
        Commands::MacLane {
            act: MacLaneAct::Done { number, receipt },
        } => crate::ledger::mac_lane_done(host, &root()?, *number, receipt),
        Commands::MacLane {
            act: MacLaneAct::List,
        } => crate::ledger::mac_lane_list(&root()?),
        Commands::Evidence {
            target,
            name,
            body_file,
            candidate,
            harness,
        } => {
            let body = read_body(body_file, None)?;
            let pin = crate::ledger::PinArgs {
                candidate: candidate.as_deref(),
                harness: harness.as_deref(),
            };
            crate::ledger::evidence_pinned(host, &root()?, target, name, &body, &pin)
        }
        Commands::Review {
            act:
                Some(ReviewAct::Wait {
                    target,
                    head,
                    reviewer,
                    deadline,
                    part,
                }),
            ..
        } => {
            let waited = match crate::review::wait_root(cli.root.as_deref()) {
                Ok(Some(root)) => {
                    crate::review::wait(&root, target, head, reviewer, part.as_deref(), *deadline)
                        .unwrap_or_else(crate::review::WaitOutcome::unreadable)
                }
                Ok(None) => crate::review::WaitOutcome::refused(
                    ".cm",
                    "no-campaign",
                    "no campaign folder at or above the starting directory; run `cm init`",
                ),
                Err(error) => crate::review::WaitOutcome::unreadable(error),
            };
            return Ok(waited.finish());
        }
        Commands::Review {
            act:
                Some(ReviewAct::Record {
                    target,
                    report,
                    reviewer,
                    part,
                    kind,
                    repo,
                }),
            ..
        } => match kind {
            Some(_) => crate::verdict::record_resolution(
                host,
                &root()?,
                target,
                &under_root(cli, report),
                reviewer.as_deref(),
                part.as_deref(),
                repo.as_deref(),
            ),
            None => crate::verdict::record(
                host,
                &root()?,
                target,
                &under_root(cli, report),
                reviewer.as_deref(),
                part.as_deref(),
            ),
        },
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
        Commands::Tick { checkout, cm, .. } => {
            let mut lines = Vec::new();
            let mut stdout = std::io::stdout().lock();
            let limit = FileSizeLimit::of(&stdout);
            let mut full = false;
            let used =
                crate::tick::beat::cli(checkout, cm.as_deref().map(Path::as_os_str), &mut |line| {
                    if cli.json {
                        lines.push(line);
                        return;
                    }
                    full = full
                        || limit
                            .as_ref()
                            .is_some_and(|limit| !limit.fits(&stdout, line.len() + 1));
                    if !full {
                        // A closed stdout (`cm tick | true`) must not end the tick: its moves still run.
                        let _ = writeln!(stdout, "{line}");
                    }
                });
            drop(stdout);
            let mut outcome = Outcome::default();
            if cli.json {
                outcome.field("cm", used.to_string_lossy());
                outcome.field("lines", lines);
            }
            return Ok((outcome, ExitCode::SUCCESS));
        }
        Commands::Lint { dir } => {
            let start = match (dir, &cli.root) {
                (Some(dir), _) => dir.clone(),
                (None, Some(root)) => root.clone(),
                (None, None) => std::env::current_dir()?,
            };
            lint::lint_at(&start, host.now())
        }
    }?;
    let exit = if outcome.ok() {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    };
    Ok((outcome, exit))
}

fn serialized_len(json: &serde_json::Value) -> usize {
    struct Count(usize);
    impl Write for Count {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0 += bytes.len();
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut count = Count(0);
    let _ = serde_json::to_writer(&mut count, json);
    count.0
}

fn name(command: &Commands) -> &'static str {
    match command {
        Commands::Attention { .. } => "attention",
        Commands::Close => "close",
        Commands::Init { .. } => "init",
        Commands::Next { .. } => "next",
        Commands::Sync { .. } => "sync",
        Commands::Epoch { .. } => "epoch",
        Commands::Read { .. } => "read",
        Commands::Idea { .. } => "idea",
        Commands::Journal { .. } => "journal",
        Commands::Launch { .. } => "launch",
        Commands::Unblock { .. } => "unblock",
        Commands::Amend { .. } => "amend",
        Commands::Attempt { .. } => "attempt",
        Commands::Lane { .. } => "lane",
        Commands::Advance { .. } => "advance",
        Commands::KnownRed { .. } => "known-red",
        Commands::Watch { .. } => "watch",
        Commands::Feedback { .. } => "feedback",
        Commands::OwnerQueue { .. } => "owner-queue",
        Commands::MacLane { .. } => "mac-lane",
        Commands::Evidence { .. } => "evidence",
        Commands::Review { .. } => "review",
        Commands::Ruling { .. } => "ruling",
        Commands::Brief { .. } => "brief",
        Commands::Followups { .. } => "followups",
        Commands::Tick { .. } => "tick",
        Commands::Lint { .. } => "lint",
    }
}

pub fn main() -> ExitCode {
    let cli = Cli::parse();
    if let Commands::Advance { state, by, .. } = &cli.command
        && !by.is_empty()
        && *state != Phase::Superseded
    {
        Cli::command()
            .error(
                clap::error::ErrorKind::ArgumentConflict,
                "--by applies only to superseded",
            )
            .exit();
    }
    if matches!(cli.command, Commands::Tick { .. }) && cli.root.is_some() {
        Cli::command()
            .error(
                clap::error::ErrorKind::ArgumentConflict,
                "cm tick chooses its own epoch: name the checkout with --checkout, not -C",
            )
            .exit();
    }
    if let Commands::Tick {
        checkout,
        check_bound: true,
        ..
    } = &cli.command
    {
        return if crate::tick::beat::cli_check(checkout) {
            ExitCode::SUCCESS
        } else {
            ExitCode::from(1)
        };
    }
    let command = name(&cli.command);
    let outcome = run(&cli, &System).or_else(|error| {
        if let Some(large) = error.downcast_ref::<InputTooLarge>() {
            return Ok((
                Outcome::refused(vec![Violation::new(
                    large.path.to_string_lossy(),
                    "kickoff-size",
                    large.to_string(),
                )]),
                ExitCode::from(1),
            ));
        }
        Err(error)
    });
    match outcome {
        Ok((outcome, exit)) => {
            if cli.json && matches!(cli.command, Commands::Tick { .. }) {
                // A closed stdout must not turn a tick whose moves ran into a panic. The object is
                // streamed, never copied into one string: only its length is needed for a limit.
                let json = outcome.to_json(command);
                let mut stdout = std::io::stdout().lock();
                if FileSizeLimit::of(&stdout)
                    .is_none_or(|limit| limit.fits(&stdout, serialized_len(&json) + 1))
                {
                    let _ = writeln!(stdout, "{json}");
                }
            } else if cli.json {
                println!("{}", outcome.to_json(command));
            } else {
                for violation in &outcome.violations {
                    println!("{violation}");
                }
                for line in &outcome.lines {
                    println!("{line}");
                }
            }
            exit
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
