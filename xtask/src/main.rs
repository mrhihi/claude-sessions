//! Maintainer tasks: `cargo xtask version` and `cargo xtask release <version>`.

use std::fs;
use std::io::{self, Write};
use std::path::PathBuf;
use std::process::Command;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use semver::Version;

const REPO: &str = "https://github.com/mrhihi/claude-sessions";

#[derive(Parser)]
#[command(about = "Maintainer tasks for claude-sessions")]
struct Cli {
    #[command(subcommand)]
    task: Task,
}

#[derive(Subcommand)]
enum Task {
    /// Show the current version, the latest tag and the suggested next version.
    Version,
    /// Bump the version, commit, tag vX.Y.Z and push; the tag triggers the release build.
    Release {
        /// New version, e.g. 0.2.0 or 0.2.0-rc1 (a leading `v` is accepted).
        version: String,
        /// Run every check, change nothing.
        #[arg(short = 'n', long)]
        dry_run: bool,
        /// Do not ask before pushing.
        #[arg(short, long)]
        yes: bool,
    },
}

fn main() -> Result<()> {
    match Cli::parse().task {
        Task::Version => show(),
        Task::Release { version, dry_run, yes } => release(&version, dry_run, yes),
    }
}

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).parent().expect("xtask lives in the workspace").to_path_buf()
}

/// Runs a command in the repo root and returns trimmed stdout; fails on a non-zero exit.
fn run(program: &str, args: &[&str]) -> Result<String> {
    let out = Command::new(program).args(args).current_dir(root()).output().with_context(|| format!("cannot run {program}"))?;
    if !out.status.success() {
        bail!("`{program} {}` failed: {}", args.join(" "), String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Like `run`, but lets the command print to the terminal.
fn run_inherit(program: &str, args: &[&str]) -> Result<()> {
    let status = Command::new(program).args(args).current_dir(root()).status().with_context(|| format!("cannot run {program}"))?;
    if !status.success() {
        bail!("`{program} {}` failed", args.join(" "));
    }
    Ok(())
}

fn cargo_toml() -> PathBuf {
    root().join("Cargo.toml")
}

/// The `version` of the `[package]` section; other `version = ` lines (dependencies) are ignored.
fn package_version(toml: &str) -> Option<Version> {
    let mut in_package = false;
    for line in toml.lines() {
        let t = line.trim();
        if t.starts_with('[') {
            in_package = t == "[package]";
        } else if in_package && let Some(v) = t.strip_prefix("version") {
            let v = v.trim_start().strip_prefix('=')?.trim().trim_matches('"');
            return Version::parse(v).ok();
        }
    }
    None
}

/// Rewrites the `[package]` version in place, leaving every other byte as it was.
fn set_version(toml: &str, new: &Version) -> Result<String> {
    let mut in_package = false;
    let mut done = false;
    let lines: Vec<String> = toml
        .split_inclusive('\n')
        .map(|line| {
            let t = line.trim();
            if t.starts_with('[') {
                in_package = t == "[package]";
            } else if in_package && !done && t.starts_with("version") && t.contains('=') {
                done = true;
                let eol = &line[line.trim_end().len()..];
                return format!("version = \"{new}\"{eol}");
            }
            line.to_string()
        })
        .collect();
    if !done {
        bail!("no version found in the [package] section of Cargo.toml");
    }
    Ok(lines.concat())
}

fn latest_tag() -> Result<Option<(String, Version)>> {
    let tags = run("git", &["tag", "--list", "v[0-9]*"])?;
    Ok(tags
        .lines()
        .filter_map(|t| Some((t.to_string(), Version::parse(t.strip_prefix('v')?).ok()?)))
        .max_by(|a, b| a.1.cmp(&b.1)))
}

fn release_of(v: &Version) -> Version {
    Version::new(v.major, v.minor, v.patch)
}

/// (patch, minor, major) candidates after `v`. A pre-release `X.Y.Z-rcN` is finished by `X.Y.Z`.
fn candidates(v: &Version) -> [Version; 3] {
    let base = release_of(v);
    [Version::new(base.major, base.minor, base.patch + 1), Version::new(base.major, base.minor + 1, 0), Version::new(base.major + 1, 0, 0)]
}

/// Heuristic: a commit titled "Add…"/"feat…" brings new functionality (minor), anything else is a patch.
fn suggest(v: &Version, subjects: &[String]) -> (Version, &'static str) {
    if !v.pre.is_empty() {
        return (release_of(v), "finish the pre-release");
    }
    let [patch, minor, _] = candidates(v);
    let feature = subjects.iter().any(|s| {
        let s = s.to_lowercase();
        s.starts_with("add") || s.starts_with("feat")
    });
    if feature { (minor, "new functionality in the commits") } else { (patch, "fixes only") }
}

fn commits_since(tag: Option<&str>) -> Result<Vec<String>> {
    let range = tag.map(|t| format!("{t}..HEAD"));
    let mut args = vec!["log", "--format=%s"];
    args.extend(range.as_deref());
    Ok(run("git", &args)?.lines().map(String::from).collect())
}

fn show() -> Result<()> {
    let cur = package_version(&fs::read_to_string(cargo_toml())?).context("no version in Cargo.toml")?;
    let tag = latest_tag()?;
    let base = tag.as_ref().map(|t| t.1.clone()).unwrap_or_else(|| cur.clone());
    let subjects = commits_since(tag.as_ref().map(|t| t.0.as_str()))?;
    println!("Cargo.toml version : {cur}");
    println!("Latest tag         : {}", tag.as_ref().map_or("<none>", |t| t.0.as_str()));
    println!("Commits since tag  : {}", subjects.len());
    for s in &subjects {
        println!("  - {s}");
    }
    println!("\nCandidates (from {base}):");
    for (name, v) in ["patch", "minor", "major"].iter().zip(candidates(&base)) {
        println!("  {name}  {v}");
    }
    let (next, why) = suggest(&base, &subjects);
    println!("\nSuggested next     : {next}  ({why})");
    println!("Release it with    : cargo xtask release {next}");
    Ok(())
}

fn release(arg: &str, dry_run: bool, yes: bool) -> Result<()> {
    let new = Version::parse(arg.strip_prefix('v').unwrap_or(arg)).with_context(|| format!("bad version '{arg}' (want X.Y.Z or X.Y.Z-rc1)"))?;
    if !new.build.is_empty() {
        bail!("build metadata is not supported in tags");
    }
    let tag_name = format!("v{new}");
    let text = fs::read_to_string(cargo_toml())?;
    let cur = package_version(&text).context("no version in Cargo.toml")?;
    let last = latest_tag()?;

    if run("git", &["branch", "--show-current"])? != "main" {
        bail!("not on main");
    }
    if !run("git", &["status", "--porcelain"])?.is_empty() {
        bail!("working tree is not clean");
    }
    run("git", &["fetch", "-q", "origin", "main", "--tags"])?;
    if run("git", &["rev-parse", "HEAD"])? != run("git", &["rev-parse", "origin/main"])? {
        bail!("main is not in sync with origin/main (pull or push first)");
    }
    if run("git", &["tag", "--list", &tag_name])? == tag_name {
        bail!("tag {tag_name} already exists");
    }
    if let Some((t, v)) = &last
        && &new <= v
    {
        bail!("{new} is not newer than {t}");
    }

    println!("Release {tag_name}  (Cargo.toml {cur} -> {new}, previous tag {})", last.as_ref().map_or("none", |t| t.0.as_str()));
    if dry_run {
        println!("dry run: checks passed, nothing changed");
        return Ok(());
    }
    if !yes {
        print!("Bump, commit, tag and push to origin? [y/N] ");
        io::stdout().flush()?;
        let mut a = String::new();
        io::stdin().read_line(&mut a)?;
        if !a.trim().eq_ignore_ascii_case("y") {
            bail!("aborted");
        }
    }
    if cur != new {
        fs::write(cargo_toml(), set_version(&text, &new)?)?;
        run("cargo", &["check", "--quiet"])?; // refreshes Cargo.lock
        run("git", &["add", "Cargo.toml", "Cargo.lock"])?;
        run("git", &["commit", "-q", "-m", &format!("Release {tag_name}")])?;
    }
    run("git", &["tag", "-a", &tag_name, "-m", &tag_name])?;
    run_inherit("git", &["push", "origin", "main", &tag_name])?;
    println!("Pushed {tag_name}. Watch the build: gh run watch  |  {REPO}/actions");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &str) -> Version {
        Version::parse(s).unwrap()
    }

    const TOML: &str = "[package]\nname = \"x\"\nversion = \"0.1.0\"\nedition = \"2024\"\n\n[dependencies]\nclap = { version = \"4\" }\nfoo = { version = \"1\" }\n\n[dependencies.bar]\nversion = \"2\"\n";

    #[test]
    fn reads_only_the_package_version() {
        assert_eq!(package_version(TOML), Some(v("0.1.0")));
        assert_eq!(package_version("[dependencies.bar]\nversion = \"2\"\n"), None);
    }

    #[test]
    fn rewrites_only_the_package_version() {
        let out = set_version(TOML, &v("0.2.0-rc1")).unwrap();
        assert_eq!(out, TOML.replace("version = \"0.1.0\"", "version = \"0.2.0-rc1\""));
        assert!(set_version("[dependencies.bar]\nversion = \"2\"\n", &v("1.0.0")).is_err());
    }

    #[test]
    fn rewrite_keeps_crlf_line_endings() {
        let crlf = TOML.replace('\n', "\r\n");
        assert_eq!(set_version(&crlf, &v("0.3.0")).unwrap(), crlf.replace("0.1.0", "0.3.0"));
    }

    #[test]
    fn candidates_bump_each_level() {
        assert_eq!(candidates(&v("0.1.9")), [v("0.1.10"), v("0.2.0"), v("1.0.0")]);
        assert_eq!(candidates(&v("1.2.3-rc1")), [v("1.2.4"), v("1.3.0"), v("2.0.0")]);
    }

    #[test]
    fn suggests_minor_for_features_patch_for_fixes_and_final_for_rc() {
        let feat = vec!["Fix a bug".to_string(), "Add export command".to_string()];
        let fix = vec!["Fix a bug".to_string()];
        assert_eq!(suggest(&v("0.1.0"), &feat).0, v("0.2.0"));
        assert_eq!(suggest(&v("0.1.0"), &fix).0, v("0.1.1"));
        assert_eq!(suggest(&v("0.2.0-rc1"), &feat).0, v("0.2.0"));
    }

    #[test]
    fn prerelease_sorts_before_release() {
        assert!(v("0.2.0-rc1") < v("0.2.0"));
        assert!(v("0.2.0-rc2") > v("0.2.0-rc1"));
    }
}
