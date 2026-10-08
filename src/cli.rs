use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};

use crate::commands;
use crate::host::{Host, System};
use crate::lint;
use crate::outcome::Outcome;
use crate::state::{Evidence, Phase};
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
    /// Write a campaign folder, or a unit folder (<NN-name>/<unit>) inside one, from the embedded templates. Never overwrites.
    Init {
        /// The folder to initialize.
        #[arg(default_value = ".")]
        dir: PathBuf,
    },
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
        /// The step's worktree, which must already exist.
        #[arg(long, value_name = "PATH")]
        worktree: PathBuf,
    },
    /// Move a unit or step to its next state with that state's evidence.
    Advance {
        /// <NN-name>/<unit> or <NN-name>/<unit>/<step>
        #[arg(value_parser = any_target)]
        target: Target,
        /// pr-open, merged, released, deployed, done or blocked:<reason>.
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
        /// The repository --sha and --tag resolve in; defaults to the step worktrees.
        #[arg(long, value_name = "PATH")]
        repo: Option<PathBuf>,
    },
    /// Report every violation in the campaign folder; exit 1 when there is any.
    Lint {
        /// A folder inside the campaign; defaults to --root or the current directory.
        dir: Option<PathBuf>,
    },
}

fn step_target(text: &str) -> Result<Target, String> {
    parse_target(text, true)
}

fn any_target(text: &str) -> Result<Target, String> {
    parse_target(text, false)
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
        Commands::Init { dir } => commands::init(host, dir),
        Commands::Journal { title, body_file } => {
            let body = body_file.as_deref().map(read_body).transpose()?;
            commands::journal(host, &root()?, title, body.as_deref())
        }
        Commands::Launch { target, worktree } => commands::launch(host, &root()?, target, worktree),
        Commands::Advance {
            target,
            state,
            sha,
            pr,
            tag,
            digest,
            repo,
        } => {
            let evidence = Evidence {
                sha: sha.clone(),
                pr: pr.clone(),
                tag: tag.clone(),
                digest: digest.clone(),
            };
            commands::advance(host, &root()?, target, state, &evidence, repo.as_deref())
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
        Commands::Init { .. } => "init",
        Commands::Journal { .. } => "journal",
        Commands::Launch { .. } => "launch",
        Commands::Advance { .. } => "advance",
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
