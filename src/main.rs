use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use claude_sessions::report::SortKey;
use claude_sessions::{cp, doctor, export, mv, report, rm, scan, search, stats, style, timespec};

#[derive(Clone, Copy, ValueEnum)]
enum ColorChoice {
    Auto,
    Always,
    Never,
}

#[derive(Clone, Copy, ValueEnum)]
enum ExportFormat {
    Md,
    Json,
}

#[derive(Parser)]
#[command(version, about = "Inspect Claude Code sessions per directory, and move directories together with their sessions")]
struct Cli {
    /// Directory to inspect (default: current directory); sessions of its subdirectories are included
    path: Option<PathBuf>,
    /// Extra directory names to skip (added to the defaults)
    #[arg(long, short = 'x', value_name = "NAME")]
    exclude: Vec<String>,
    /// Don't skip the default names (.git, node_modules, target, ...)
    #[arg(long)]
    no_default_excludes: bool,
    /// Also list every session
    #[arg(long, short = 's')]
    sessions: bool,
    /// Machine-readable output
    #[arg(long)]
    json: bool,
    /// Only sessions last active since an age (7d, 12h, 2w) or a date (2026-01-31)
    #[arg(long, value_name = "AGE|DATE")]
    since: Option<String>,
    /// Order directories (and sessions with -s) by this instead of by path
    #[arg(long, value_enum, default_value = "path", value_name = "KEY")]
    sort: SortKey,
    /// Show at most N directories
    #[arg(long, value_name = "N")]
    limit: Option<usize>,
    /// When to use colors (NO_COLOR is honored in auto mode)
    #[arg(long, global = true, value_enum, default_value = "auto", value_name = "WHEN")]
    color: ColorChoice,
    /// Claude config directory (default: ~/.claude)
    #[arg(long, global = true, value_name = "DIR")]
    claude_dir: Option<PathBuf>,
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Move a directory and carry its Claude sessions (and those of its subdirectories) along
    Mv {
        src: PathBuf,
        dst: PathBuf,
        /// Show what would change without touching anything
        #[arg(long)]
        dry_run: bool,
        /// Only update the sessions; the directory was already moved by hand
        #[arg(long)]
        no_move_files: bool,
        /// Proceed even if Claude Code is running inside the directories
        #[arg(long)]
        force: bool,
    },
    /// Copy a directory and its Claude sessions (and those of its subdirectories); the originals stay untouched
    Cp {
        src: PathBuf,
        dst: PathBuf,
        /// Show what would change without touching anything
        #[arg(long)]
        dry_run: bool,
        /// Only copy the sessions; the directory was already copied by hand
        #[arg(long)]
        no_copy_files: bool,
    },
    /// Print one session as Markdown or JSON
    Export {
        /// Session id, or the first characters of it
        id: String,
        #[arg(long, value_enum, default_value = "md")]
        format: ExportFormat,
        /// Write to this file instead of stdout
        #[arg(long, short = 'o', value_name = "FILE")]
        output: Option<PathBuf>,
    },
    /// Search the conversations (your prompts and Claude's replies) for text
    Search {
        keyword: String,
        /// Only sessions of this directory and below (default: all)
        #[arg(long, value_name = "DIR")]
        path: Option<PathBuf>,
        /// Ignore case
        #[arg(long, short = 'i')]
        ignore_case: bool,
        /// Stop after this many matching messages
        #[arg(long, default_value_t = 20, value_name = "N")]
        limit: usize,
    },
    /// Browse projects and sessions interactively: tick, delete, move, copy, export
    #[cfg(feature = "tui")]
    Tui,
    /// Find session folders and history entries that point at directories which no longer exist
    Doctor {
        /// Machine-readable output
        #[arg(long)]
        json: bool,
        /// Remove the stale history.jsonl lines and .claude.json entries (each file is backed up first)
        #[arg(long)]
        fix: bool,
        /// With --fix: show what would be removed without touching anything
        #[arg(long, requires = "fix")]
        dry_run: bool,
        /// With --fix: don't ask for confirmation
        #[arg(long, short = 'y', requires = "fix")]
        yes: bool,
        /// With --fix: proceed even if Claude Code is running
        #[arg(long, requires = "fix")]
        force: bool,
        /// With --fix: also delete for good the session folders of missing directories, per-session
        /// data without a transcript and empty project folders (folders holding auto-memory are kept)
        #[arg(long, requires = "fix")]
        delete: bool,
    },
    /// Delete the Claude sessions of a directory (and its subdirectories)
    Rm {
        path: PathBuf,
        /// Only sessions last active more than this long ago (30d, 12h, 2w)
        #[arg(long, value_name = "AGE")]
        older_than: Option<String>,
        /// Show what would be deleted without touching anything
        #[arg(long)]
        dry_run: bool,
        /// Don't ask for confirmation
        #[arg(long, short = 'y')]
        yes: bool,
        /// Also remove the matching history.jsonl lines and (for whole folders) the .claude.json entry
        #[arg(long)]
        purge_config: bool,
        /// Ask about every folder
        #[arg(long, short = 'i', conflicts_with = "yes")]
        interactive: bool,
        /// Proceed even if Claude Code is running in those directories
        #[arg(long)]
        force: bool,
    },
    /// Delete session folders whose directory no longer exists (see `doctor`; `doctor --fix --delete` does this and more)
    Clean {
        /// Only sessions last active more than this long ago (30d, 12h, 2w)
        #[arg(long, value_name = "AGE")]
        older_than: Option<String>,
        /// Show what would be deleted without touching anything
        #[arg(long)]
        dry_run: bool,
        /// Don't ask for confirmation
        #[arg(long, short = 'y')]
        yes: bool,
        /// Also remove the matching history.jsonl lines and (for whole folders) the .claude.json entry
        #[arg(long)]
        purge_config: bool,
        /// Ask about every folder
        #[arg(long, short = 'i', conflicts_with = "yes")]
        interactive: bool,
        /// Proceed even if Claude Code is running in those directories
        #[arg(long)]
        force: bool,
    },
}

fn parse_age(spec: Option<String>) -> Result<Option<i64>> {
    spec.map(|s| timespec::parse_duration(&s).with_context(|| format!("cannot parse '{s}': use an age like 30d, 12h or 2w")))
        .transpose()
}

fn main() {
    let cli = Cli::parse();
    if let Err(e) = run(cli) {
        eprintln!("{} {e:#}", style::bold_red("error:"));
        std::process::exit(1);
    }
}

fn run(cli: Cli) -> Result<()> {
    style::set_enabled(match cli.color {
        ColorChoice::Auto => style::auto(),
        ColorChoice::Always => true,
        ColorChoice::Never => false,
    });
    let claude_dir = match cli.claude_dir {
        Some(d) => d,
        None => dirs::home_dir().context("cannot find home directory")?.join(".claude"),
    };
    match cli.cmd {
        Some(Cmd::Mv { src, dst, dry_run, no_move_files, force }) => {
            let dst = mv::resolve_dst(&src, dst, no_move_files)?;
            mv::run(&mv::Opts { claude_dir, src, dst, dry_run, no_move_files, force })
        }
        Some(Cmd::Cp { src, dst, dry_run, no_copy_files }) => {
            let dst = mv::resolve_dst(&src, dst, no_copy_files)?;
            cp::run(&cp::Opts { claude_dir, src, dst, dry_run, no_copy_files })
        }
        Some(Cmd::Export { id, format, output }) => {
            let (project, file) = scan::find_session(&claude_dir, &id)?;
            let turns = export::load_turns(&file);
            let stat = stats::analyze_session(&file);
            let text = match format {
                ExportFormat::Md => export::to_markdown(&stat, &project.cwd, &turns),
                ExportFormat::Json => {
                    let v = serde_json::json!({ "session": stat, "directory": project.cwd, "turns": turns });
                    format!("{}\n", serde_json::to_string_pretty(&v)?)
                }
            };
            match output {
                Some(o) => std::fs::write(&o, text).with_context(|| format!("cannot write {}", o.display())),
                None => {
                    print!("{text}");
                    Ok(())
                }
            }
        }
        Some(Cmd::Search { keyword, path, ignore_case, limit }) => {
            search::run(&search::Opts { claude_dir, keyword, path, ignore_case, limit })
        }
        #[cfg(feature = "tui")]
        Some(Cmd::Tui) => claude_sessions::tui::run(&claude_dir),
        Some(Cmd::Doctor { fix: true, dry_run, yes, force, delete, .. }) => doctor::fix(&claude_dir, dry_run, yes, force, delete),
        Some(Cmd::Doctor { json, .. }) => {
            let d = doctor::diagnose(&claude_dir)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&d)?);
            } else {
                doctor::print_text(&d);
            }
            Ok(())
        }
        Some(Cmd::Rm { path, older_than, dry_run, yes, purge_config, interactive, force }) => rm::run(&rm::Opts {
            claude_dir,
            target: Some(path),
            older_than: parse_age(older_than)?,
            dry_run,
            yes,
            force,
            purge_config,
            interactive,
        }),
        Some(Cmd::Clean { older_than, dry_run, yes, purge_config, interactive, force }) => rm::run(&rm::Opts {
            claude_dir,
            target: None,
            older_than: parse_age(older_than)?,
            dry_run,
            yes,
            force,
            purge_config,
            interactive,
        }),
        None => {
            let view = report::View {
                since: cli.since.as_deref().map(|s| timespec::parse_cutoff(s, timespec::now_secs())).transpose()?,
                sort: cli.sort,
                limit: cli.limit,
            };
            let base = scan::resolve(&cli.path.unwrap_or_else(|| PathBuf::from(".")))?;
            let mut excludes: Vec<String> = if cli.no_default_excludes {
                vec![]
            } else {
                scan::DEFAULT_EXCLUDES.iter().map(|s| s.to_string()).collect()
            };
            excludes.extend(cli.exclude);
            let mut r = report::build(&claude_dir, &base, &excludes)?;
            report::apply(&mut r, &view);
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&r)?);
            } else {
                report::print_text(&r, cli.sessions);
            }
            Ok(())
        }
    }
}
