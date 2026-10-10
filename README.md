# claude-sessions

**English** | [繁體中文](README.zh-TW.md)

Show Claude Code session stats for a directory (including its subdirectories), and move a directory together with its sessions.

![claude-sessions output with -s](docs/screenshot.png)

## Installation

Requires the Rust toolchain (`rustc` + `cargo`), installed once via rustup:

```sh
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

`cargo install` compiles from source on your machine, so the first install takes a minute or two.

```sh
cargo install --git https://github.com/mrhihi/claude-sessions                # install
cargo install --git https://github.com/mrhihi/claude-sessions --tag v0.1.0   # pin a version
cargo install --git https://github.com/mrhihi/claude-sessions --force        # upgrade
```

- The binary is placed in `~/.cargo/bin/claude-sessions` (rustup adds that directory to your PATH).
- Supported on macOS and Linux.

### Prebuilt binaries

No Rust needed: download the archive for your platform from the [Releases](https://github.com/mrhihi/claude-sessions/releases) page, extract it and put `claude-sessions` on your `PATH`. Each archive unpacks to a folder holding the binary, the license and the READMEs. Replace `v0.2.0` below with the version you downloaded.

**macOS Apple Silicon** (`claude-sessions-<tag>-aarch64-apple-darwin.tar.gz`)

```sh
tar xzf claude-sessions-v0.2.0-aarch64-apple-darwin.tar.gz
mkdir -p ~/.local/bin
cp claude-sessions-v0.2.0-aarch64-apple-darwin/claude-sessions ~/.local/bin/
xattr -d com.apple.quarantine ~/.local/bin/claude-sessions   # the binary is unsigned; clears the Gatekeeper block
claude-sessions --version
```

If `claude-sessions` is not found, add `export PATH="$HOME/.local/bin:$PATH"` to `~/.zshrc` and open a new terminal.

**Windows x64** (`claude-sessions-<tag>-x86_64-pc-windows-msvc.zip`), in PowerShell:

```powershell
Expand-Archive claude-sessions-v0.2.0-x86_64-pc-windows-msvc.zip .
New-Item -ItemType Directory -Force "$HOME\bin" | Out-Null
Copy-Item claude-sessions-v0.2.0-x86_64-pc-windows-msvc\claude-sessions.exe "$HOME\bin\"
# one-time: add that folder to your user PATH, then open a new terminal
[Environment]::SetEnvironmentVariable("Path", [Environment]::GetEnvironmentVariable("Path", "User") + ";$HOME\bin", "User")
claude-sessions --version
```

The binary is unsigned, so Windows SmartScreen may warn on first run: choose "More info" → "Run anyway".

To verify a download, compare it with `SHA256SUMS` from the same release (`shasum -a 256 <file>` on macOS, `Get-FileHash <file>` on Windows).

## Quick start

```sh
claude-sessions                  # stats for the current directory (including subdirectories)
claude-sessions ~/projects       # stats for a given directory
claude-sessions --all            # every directory Claude Code has run in on this machine
claude-sessions -s               # also list every session
claude-sessions --since 7d       # only sessions active in the last 7 days
claude-sessions mv <src> <dst>   # move a directory together with its sessions
claude-sessions memory           # show this project's auto-memory
claude-sessions tui              # browse and manage sessions interactively
```

## Command reference

| Command | Purpose |
|---|---|
| `claude-sessions [PATH]` | Stats for a directory and its subdirectories (default mode); `--all` covers every directory on this machine instead |
| [`mv`](#mv) | Move a directory together with its sessions |
| [`cp`](#cp) | Copy a directory together with its sessions |
| [`rm`](#rm--clean) | Delete the sessions of a directory |
| [`clean`](#rm--clean) | Delete the sessions of directories that no longer exist |
| [`doctor`](#doctor) | Find (and optionally fix) leftovers of missing directories |
| [`search`](#search) | Search prompts and replies |
| [`export`](#export) | Print one session as Markdown or JSON |
| [`memory`](#memory) | Show, edit, delete, copy and export a project's auto-memory |
| [`tui`](#tui) | Interactive browser |

### Global options

These work with every command.

| Option | Meaning | Default / values |
|---|---|---|
| `--color <WHEN>` | Colored output | `auto` (default), `always`, `never` |
| `--claude-dir <DIR>` | Claude config directory to read and modify | `~/.claude` |
| `-h`, `--help` | Show help | |
| `-V`, `--version` | Show the version | |

- `--color auto` colors only when stdout is a terminal, `NO_COLOR` is unset and `TERM` is not `dumb`.
- Errors print `error: …` to stderr and exit with status 1.

### Time specs

`--since` and `--older-than` share one grammar.

| Form | Example | Meaning | Accepted by |
|---|---|---|---|
| `Nh` | `12h` | N hours ago | `--since`, `--older-than` |
| `Nd` | `7d` | N days ago | `--since`, `--older-than` |
| `Nw` | `2w` | N weeks ago | `--since`, `--older-than` |
| `YYYY-MM-DD` | `2026-01-31` | That date at 00:00 UTC | `--since` only |

Any other value is an error.

### Stats (default mode)

```sh
claude-sessions [PATH] [options]
```

| Option | Meaning | Default / values |
|---|---|---|
| `PATH` | Directory to report on, subdirectories included. It does not have to exist. | `.` |
| `-s`, `--sessions` | Also list every session | off |
| `--json` | Print the whole report as JSON | off |
| `--since <AGE\|DATE>` | Keep only sessions last active at or after the cutoff; totals are recomputed and empty directories dropped | all |
| `--sort <KEY>` | Sort directories (and the sessions inside them) | `path`, `size`, `tokens`, `messages`, `last-used`; default `path` |
| `--limit <N>` | Show only the first N directories (sessions are not truncated) | all |
| `-x`, `--exclude <NAME>` | Also exclude directories with this name. Repeatable. | none |
| `--no-default-excludes` | Drop the default excludes (`-x` names still apply) | off |

- Every `--sort` key except `path` sorts descending.
- The `MEM` column counts the project's auto-memory files (`memory_files` in JSON). Directories that have memory but no sessions are listed too (`--since` drops them).
- Default excludes: `.git node_modules target .venv venv __pycache__ .idea .vscode dist build .next .cache`

```sh
claude-sessions --sort tokens --limit 5 --since 30d
claude-sessions -x vendor -x third_party
claude-sessions --json -s > report.json
```

### mv

Move a directory and carry its sessions along.

```sh
claude-sessions mv <SRC> <DST> [options]
```

| Option | Meaning |
|---|---|
| `--dry-run` | Preview only; nothing is changed |
| `--no-move-files` | The directory was already moved by hand; only move the sessions. `DST` must already exist. |
| `--force` | Run even if Claude Code has its working directory inside `SRC` or `DST` |

What it changes:

- Moves the directory itself.
- Renames the matching folders under `~/.claude/projects/`; the auto-memory inside them (`memory/`) goes along, including folders that hold only memory and no sessions.
- Rewrites the directory fields in the session jsonl files: `cwd`, `relocatedCwd`, `projectPath`, `live_cwd`, `workingDirectory`, `realParentDir` (tool inputs/outputs and message text are history and stay as they were).
- Rewrites `history.jsonl` and `~/.claude.json` (both backed up first, see [Backups](#backups)).

Refuses when:

| Situation | Fix |
|---|---|
| `SRC` is not a directory | Check the path (or use `--no-move-files` if it is gone) |
| `DST` already exists | Pick another path (`--no-move-files` expects it to exist) |
| `DST` is inside `SRC`, or both are the same | Pick another path |
| A Claude Code process runs inside `SRC` or `DST` (they are listed) | Exit those sessions, or pass `--force` |

- Like `/bin/mv`: if `DST` is an existing directory, `SRC` is moved *into* it (`mv proj ..` → `../proj`). Otherwise `DST` is the final path, i.e. a rename; a note is printed when the name changes. A `\` in `DST` is refused (an unquoted `\` is dropped by the shell, so `GSSCLI\GSSDRIVE` becomes `GSSCLIGSSDRIVE`).
- After the update it re-checks the result the way Claude's `/resume` looks sessions up (folder name from the new path, recorded directory) and fails loudly if they disagree. If a step fails after the directory was moved, the error prints the `--no-move-files` command that finishes the job.
- Sessions left behind by a rename done outside the tool show up in `doctor` as orphans, with the likely new directory when there is a single similar sibling.
- `--dry-run` also warns when a real run would be blocked by a running Claude Code.
- It ends with an `undo:` line. To undo, run `mv` the other way round (`claude-sessions mv <DST> <SRC>`): folders, `cwd` records and history are restored exactly.

### cp

Copy a directory and its sessions; the originals are untouched.

```sh
claude-sessions cp <SRC> <DST> [options]
```

| Option | Meaning |
|---|---|
| `--dry-run` | Preview only |
| `--no-copy-files` | The directory was already copied by hand; only copy the sessions |

- The same path errors as `mv` apply, plus copying onto itself.
- The directory is copied with the system `cp -a`.
- Only the copies get the new `cwd`; originals keep pointing at `SRC`.
- Auto-memory is copied too (including folders that hold only memory).
- `history.jsonl` and `.claude.json` are left alone, so Claude asks to trust the new directory.
- There is no `--force` and no running-Claude check.

### rm / clean

Delete sessions permanently. They differ only in what they target.

```sh
claude-sessions rm <PATH> [options]   # projects at or below PATH
claude-sessions clean [options]       # projects whose directory no longer exists
```

| Option | Meaning |
|---|---|
| `--older-than <AGE>` | Delete only sessions last active before the cutoff (`12h`, `30d`, `2w`). Without it, whole project folders go. |
| `--dry-run` | Print the plan and stop |
| `-y`, `--yes` | Skip the `[y/N]` confirmation |
| `-i`, `--interactive` | Ask per folder: `y` / `n` / `a` (all) / `q` (quit). Cannot be combined with `-y`. |
| `--purge-config` | Also clean `history.jsonl` and `.claude.json` (see below) |
| `--keep-memory` | When a whole folder goes, keep its `memory/` (and the project's `.claude.json` entry and history lines, so the memory can still be tied to its directory) |
| `--force` | Override the running-Claude guards |

Safety rules:

- Asks for confirmation unless `-y` is given.
- Refuses while Claude Code runs in an affected directory (`--force` overrides).
- Plans show `(+N memory file(s))` when a folder holds auto-memory, or `(N memory file(s) kept)` with `--keep-memory`.
- Folders that hold only memory and no sessions are never deleted by `rm` / `clean`; use [`memory rm`](#memory) for memory.

What gets deleted besides transcripts:

| Deleted | Never touched |
|---|---|
| `file-history/<id>`, `session-env/<id>`, `tasks/<id>`, `debug/<id>` (where present) | `plans/`, `shell-snapshots/` (nothing ties them to a session) |

`--purge-config`:

- By default `history.jsonl` and `.claude.json` are left alone, because that is your prompt history.
- With the flag, matching `history.jsonl` lines are dropped.
- `.claude.json` project entries are dropped only when the whole folder is deleted (not with a partial `--older-than`).
- Both files are backed up first (see [Backups](#backups)).
- Claude Code must be closed (`--force` overrides).

```sh
claude-sessions rm ~/old-project --dry-run
claude-sessions rm ~/projects --older-than 30d -i
claude-sessions clean -y --purge-config
```

### doctor

Find leftovers of directories and sessions that are gone.

```sh
claude-sessions doctor [--json]
claude-sessions doctor --fix [--delete] [--dry-run] [-y] [--force]
```

| Option | Meaning | Requires `--fix` |
|---|---|---|
| `--json` | Machine-readable diagnosis (ignored together with `--fix`) | no |
| `--fix` | Remove stale `history.jsonl` lines and `.claude.json` entries (backed up first) | |
| `--delete` | Additionally delete leftovers on disk, **for good** | yes |
| `--dry-run` | Print the plan and stop | yes |
| `-y`, `--yes` | Skip the `[y/N]` confirmation | yes |
| `--force` | Run although Claude Code is running (checked only when records would be edited) | yes |

What `doctor` reports:

- Session folders whose directory is gone (hint: `mv <old> <new> --no-move-files` or `doctor --fix --delete`).
- Stale `history.jsonl` entries and stale `.claude.json` entries.
- Per-session data (`file-history`, `session-env`, `tasks`, `debug`) whose session has no transcript.
- `projects/` folders with no transcript at all (their working directory cannot be told).
- Folders that only hold memory files (as a note).

The two levels:

| Command | Removes | Undo |
|---|---|---|
| `doctor --fix` | Only stale records. Files on disk stay, and the rest is listed at the end. | Backup restores it completely |
| `doctor --fix --delete` | Also **permanently** deletes session folders of missing directories, per-session data without a transcript, and empty project folders | Not possible from a backup |

- `clean` is the narrower tool for session folders (it has `--older-than`, `-i`, `--purge-config`).
- Never deleted:
  - A project folder holding auto-memory (`memory/` with files; use `claude purge <path>` for those).
  - `plans/` and `shell-snapshots/`.
  - Entries that don't look like a session id.
  - Anything touched in the last 24 hours.
  - Data of sessions recorded in `sessions/*.json`.

### search

Search the text of prompts and replies.

```sh
claude-sessions search <KEYWORD> [options]
```

| Option | Meaning | Default |
|---|---|---|
| `<KEYWORD>` | Literal substring (not a regex); must not be empty | |
| `-i`, `--ignore-case` | Case-insensitive match | case-sensitive |
| `--path <DIR>` | Only sessions of this directory and below | all projects |
| `--limit <N>` | Stop after N matching messages | `20` |
| `--no-memory` | Don't search the auto-memory files | searched |

- A `Memory` block comes first: one hit per matching memory file (file, directory, first matching line). These count toward `--limit`.
- It searches the same text as `export`, including `[tool: …]` lines.
- Each session prints its 8-character id, title and directory.
- Each hit prints role, `YYYY-MM-DD HH:MM` and a snippet (first matching line of the message, with the match highlighted).
- The footer shows `N match(es) in M session(s)` (plus `and K memory file(s)` when memory matched), a hint to raise `--limit` if it stopped early, or `(no matches)`.

### export

Print one session.

```sh
claude-sessions export <ID> [options]
```

| Option | Meaning | Default / values |
|---|---|---|
| `<ID>` | Full session id or a unique prefix. An ambiguous prefix lists up to 5 candidates. | |
| `--format <FMT>` | Output format | `md` (default), `json` |
| `-o`, `--output <FILE>` | Write to a file | stdout |

Included and skipped:

| Included | Skipped |
|---|---|
| User and assistant text; `[tool: Name] <first argument>` lines; `[image]` markers | Thinking blocks, tool results, subagent (sidechain) messages, meta messages |

- Consecutive assistant lines sharing one message id become one turn.
- Markdown starts with a title, then Session, Directory, Time (UTC) and Models lines; each turn is `## User|Assistant · timestamp`.
- JSON is `{session, directory, turns[]}`; each turn has `role`, `timestamp`, `model`, `text`.

### memory

Manage Claude Code's auto-memory: the `MEMORY.md` index under `~/.claude/projects/<project>/memory/` (one line per memory, loaded at the start of every conversation) and one Markdown file per memory.

```sh
claude-sessions memory [PATH] [--all] [--json]          # list memory
claude-sessions memory show [NAME] [--path DIR]         # print one file (default MEMORY.md)
claude-sessions memory edit [NAME] [--path DIR]         # open it in your editor (default MEMORY.md)
claude-sessions memory rm <NAME>... [--path DIR] [--dry-run] [-y] [--force]
claude-sessions memory cp <SRC> <DST> [NAME...] [--dry-run] [--force]
claude-sessions memory export [PATH] [--format md|json] [-o FILE]
```

| Command | What it does |
|---|---|
| `memory` | Lists file, type (frontmatter `type`) and description. `--all` covers every project, `--json` prints JSON. |
| `show` / `edit` | `NAME` is the file name, the name without `.md`, the frontmatter `name`, or a unique prefix of these. The editor is `$VISUAL`, then `$EDITOR`, then `vi` (`notepad` on Windows). |
| `rm` | Deletes the files and the `MEMORY.md` lines that link to them (`MEMORY.md` is backed up first, see [Backups](#backups)). Asks first; refuses while Claude Code runs in the project (`--force` overrides). |
| `cp` | Copies memory from `SRC`'s project to `DST`'s and adds the index lines to the destination `MEMORY.md`. Without `NAME` everything is copied; refuses to overwrite a file of the same name (`--force` does). |
| `export` | Prints all memory of a project as one Markdown or JSON document. |

- Which project: Claude picks the memory folder by **git repository root**, so subdirectories and worktrees share one. `PATH` / `--path` (default: the current directory) uses its git root, or else the nearest directory above it that has memory.
- If `memory cp`'s destination has no folder yet, one is created for its git root (or `DST` itself outside a repository).
- If `autoMemoryDirectory` is set in `~/.claude/settings.json`, memory lives elsewhere; this tool only manages `projects/*/memory` and prints a note.

```sh
claude-sessions memory --all
claude-sessions memory show feedback-history
claude-sessions memory rm old-note --dry-run
claude-sessions memory cp ~/proj-a ~/proj-b coding-style
claude-sessions memory export -o memory.md
```

### tui

```sh
claude-sessions tui
```

- Needs a terminal (stdin and stdout).
- Lists every project (orphans in red; `MEM` counts memory files, and projects with only memory are listed too). `Enter` opens a menu for the directory: browse its sessions, browse its memory, open a shell there, or quit and `cd` there. `→` goes straight to the sessions; a session opens for reading.
- `--cd-file FILE`: where "quit and cd here" writes the chosen directory (default: print it to stdout after leaving the TUI).
- `--claude-dir` and `--color` are honored.
- Build with `--no-default-features` to leave the TUI (and its `ratatui` dependency) out.

| Key | Action |
|---|---|
| `↑` `↓` / `j` `k` | Move |
| `PgUp` `PgDn` | Move 10 rows (a page when reading) |
| `g` `G` / `Home` `End` | First / last |
| `Space` | Tick and move on (page down when reading) |
| `a` | Tick all / none |
| `Enter` | Project: menu (`s` sessions, `m` memory, `h` shell here — `exit` returns to the list, `x` quit and cd here); session / memory list: read it |
| `→` | Open a project's sessions, or read a session |
| `M` | Open the auto-memory of the project under the cursor (project list) or of the open project (Sessions view) |
| `S` | Memory view: switch to the project's sessions |
| `Esc` / `←` | Back (`Esc` in the top view clears the filter, then quits; `←` never quits) |
| `/` | Filter (`Enter` confirms, `Esc` clears) |
| `o` | Orphans only |
| `s` | Cycle sort: path, size, last used |
| `d` | Delete ticked rows (or the row under the cursor); confirm with `y`, cancel with `n` / `Esc`, `p` toggles `--purge-config` |
| `m` / `c` | Move / copy the project directory (asks for the destination, runs `mv` / `cp`, then waits for `Enter`) |
| `e` | Sessions view: export the session as Markdown (default file `<id>.md`). Memory view or reading a memory file: edit it in `$EDITOR` |
| `x` | Memory view: export all of the project's memory as Markdown (default file `<dir name>-memory.md`) |
| `r` | Reload |
| `?` | Show the keys of the current page (on the project list including the `Enter` menu) |
| `q` / `Ctrl-C` | Quit |

- The title bar shows where the cursor is: `n/total` and the full path of the project (or the session / memory file) under it, cut from the left when it doesn't fit.
- Going back from a project's sessions or memory puts the cursor on that project again.
- In the Memory view, `d` deletes the ticked memory files and their index lines (like `memory rm`); `p` does not apply there.
- A program cannot change its parent shell's directory, so `x` hands the path back instead. Add this wrapper to `~/.zshrc` / `~/.bashrc` to really `cd`:

  ```sh
  cs() { local f; f=$(mktemp) || return; claude-sessions tui --cd-file "$f"; [ -s "$f" ] && cd "$(cat "$f")"; rm -f "$f"; }
  ```

- The shell opened with `h` has `CLAUDE_SESSIONS_TUI=1` set, so a prompt can show that you are inside the TUI.
- Delete refuses while Claude Code runs in an affected directory; with `--purge-config` on, while any Claude Code runs.

## Relation to `claude purge`

`claude purge` removes a whole project. This tool covers what it doesn't.

| | `claude purge [path]` | `claude-sessions` |
|---|---|---|
| Scope | Everything for one project (transcripts, file history, its `history.jsonl` lines and `.claude.json` entry); `--all` wipes every project | Only what you select |
| Orphans | Not found | `doctor` finds them |
| Old sessions only | No | `--older-than` |
| Prompt history | Removed | Kept unless `--purge-config` |
| While Claude Code runs | | Refuses |

To remove one project completely, `claude purge <path>` is the right tool.

## Backups

Before `mv`, `rm`/`clean --purge-config` or `doctor --fix` rewrite `history.jsonl` or `~/.claude.json`, and before `memory rm`/`cp` rewrite a `MEMORY.md`, the file is copied next to itself.

- **Name**: `<name>.claude-sessions-<UTC time>.bak`, for example `history.jsonl.claude-sessions-20261008T083342Z.bak`.
- **Never overwritten**: the name is specific to this tool.
- **Retention**: only the newest 3 per file are kept.
- **Plain `*.bak` files** (from other tools or earlier versions) are never touched or counted.

What a backup can bring back:

| Command | Copying the backup over the original… |
|---|---|
| `doctor --fix` | …fully undoes it: only stale records were removed, no session was deleted. |
| `rm` / `clean --purge-config` | …only restores a *record* of what was removed. The sessions themselves are deleted for good, so the restored history lines point at conversations that no longer exist. |
| `memory rm` / `cp` | …restores the `MEMORY.md` index lines. A file deleted by `memory rm` is gone for good. |
| `mv` | …is not enough on its own: the rewritten `cwd` in the session files and the renamed folders are not backed up. Undo it with `claude-sessions mv <dst> <src>` instead. |

## Releasing (maintainers)

```sh
cargo xtask version              # current version, latest tag, suggested next version
cargo xtask release 0.2.0 -n     # dry run: run every check, change nothing
cargo xtask release 0.2.0        # bump Cargo.toml, commit, tag v0.2.0 and push
```

Pushing the tag triggers `.github/workflows/release.yml`, which builds the macOS Apple Silicon and Windows binaries and attaches them to the GitHub Release. Add `-y` to skip the confirmation prompt.
