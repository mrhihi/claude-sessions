//! Minimal ANSI styling. Colors are off unless `set_enabled(true)` was called,
//! so library users and tests get plain text by default.

use std::io::IsTerminal;
use std::sync::atomic::{AtomicBool, Ordering};

static ENABLED: AtomicBool = AtomicBool::new(false);

pub fn set_enabled(on: bool) {
    ENABLED.store(on, Ordering::Relaxed);
}

/// Colors for a terminal stdout, unless `NO_COLOR` is set or `TERM=dumb`.
pub fn auto() -> bool {
    std::io::stdout().is_terminal()
        && std::env::var_os("NO_COLOR").is_none_or(|v| v.is_empty())
        && std::env::var("TERM").map_or(true, |t| t != "dumb")
}

fn paint(code: &str, s: &str) -> String {
    if ENABLED.load(Ordering::Relaxed) { format!("\x1b[{code}m{s}\x1b[0m") } else { s.to_string() }
}

pub fn bold(s: &str) -> String { paint("1", s) }
pub fn dim(s: &str) -> String { paint("2", s) }
pub fn red(s: &str) -> String { paint("31", s) }
pub fn green(s: &str) -> String { paint("32", s) }
pub fn yellow(s: &str) -> String { paint("33", s) }
pub fn cyan(s: &str) -> String { paint("36", s) }
pub fn bold_cyan(s: &str) -> String { paint("1;36", s) }
pub fn bold_green(s: &str) -> String { paint("1;32", s) }
pub fn bold_yellow(s: &str) -> String { paint("1;33", s) }
pub fn bold_red(s: &str) -> String { paint("1;31", s) }
