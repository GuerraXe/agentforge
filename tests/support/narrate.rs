//! Shared terminal-narration formatting for the runnable demos (`examples/demo.rs`,
//! `examples/quickstart.rs`) — one place for section headers, `[n/total]` progress, plain-
//! language term callouts, and color-if-supported-else-plain, so formatting logic isn't
//! duplicated between scenarios. `print_section` reproduces `demo_scenario.rs`'s original
//! `section()` output exactly (`"\n=== {title} ==="`); `Narrator` is the richer, beginner-
//! oriented API used by `quickstart_scenario.rs`.
//!
//! This file is `#[path]`-included separately into four compilation units (`examples/demo.rs`,
//! `tests/demo_e2e.rs`, `examples/quickstart.rs`, `tests/quickstart_e2e.rs`), and `demo_scenario`
//! only exercises `print_section` while `quickstart_scenario` only exercises `Narrator` — so any
//! single compilation unit only ever uses half of this module's public surface. `dead_code` is
//! allowed at the module level for that reason, not because anything here is actually unused.

#![allow(dead_code)]

use std::cell::RefCell;
use std::io::IsTerminal;

/// Pure, directly testable — no env/tty access here, so it's unit-testable without mutating
/// process-global state (`std::env::set_var` in tests is racy across parallel test binaries).
pub fn should_color(no_color_set: bool, is_tty: bool) -> bool {
    !no_color_set && is_tty
}

/// Byte-identical to `demo_scenario.rs`'s original inline `section()` body.
pub fn print_section(enabled: bool, title: &str) {
    if enabled {
        println!("\n=== {title} ===");
    }
}

/// Beginner-oriented narration: numbered steps, plain-language explanations, first-use glossary
/// callouts, and a final recap — with color used only when it's actually going to help (a real
/// terminal, `NO_COLOR` unset), and every emitted line kept for order/content test assertions.
pub struct Narrator {
    enabled: bool,
    color: bool,
    transcript: RefCell<Vec<String>>,
}

impl Narrator {
    pub fn new(enabled: bool) -> Self {
        let color = should_color(
            std::env::var_os("NO_COLOR").is_some(),
            std::io::stdout().is_terminal(),
        );
        Narrator {
            enabled,
            color,
            transcript: RefCell::new(Vec::new()),
        }
    }

    fn emit(&self, line: String) {
        self.transcript.borrow_mut().push(line.clone());
        if self.enabled {
            println!("{line}");
        }
    }

    /// "\n[2/6] Registering the task and evaluator"
    pub fn step(&self, n: u32, total: u32, title: &str) {
        let line = if self.color {
            format!("\n\x1b[1m[{n}/{total}] {title}\x1b[0m")
        } else {
            format!("\n[{n}/{total}] {title}")
        };
        self.emit(line);
    }

    /// A short plain-language paragraph, printed before (or in place of) a command.
    pub fn explain(&self, text: &str) {
        self.emit(text.to_string());
    }

    /// A one-sentence glossary callout at a term's first use, e.g.
    /// `n.term("Task", "a registered unit of work: a prompt, a repo/commit, and an Evaluator.")`.
    pub fn term(&self, name: &str, definition: &str) {
        let line = if self.color {
            format!("  \x1b[36m{name}\x1b[0m \u{2014} {definition}")
        } else {
            format!("  {name} \u{2014} {definition}")
        };
        self.emit(line);
    }

    /// "$ agentforge run --task ..." — the command about to run.
    pub fn command(&self, cmd_line: &str) {
        let line = if self.color {
            format!("\x1b[2m$ {cmd_line}\x1b[0m")
        } else {
            format!("$ {cmd_line}")
        };
        self.emit(line);
    }

    /// A concise result summary line.
    pub fn result(&self, text: &str) {
        self.emit(format!("  {text}"));
    }

    /// An un-narrated section header, for a bonus stage that isn't part of the numbered core
    /// walkthrough (e.g. "Optional: compare two agents in parallel").
    pub fn section(&self, title: &str) {
        self.emit(format!("\n=== {title} ==="));
    }

    pub fn recap(&self, lines: &[&str]) {
        self.emit(String::new());
        self.emit("=== Recap ===".to_string());
        for l in lines {
            self.emit(format!("  {l}"));
        }
    }

    pub fn transcript(&self) -> Vec<String> {
        self.transcript.borrow().clone()
    }
}

#[cfg(test)]
mod tests {
    use super::should_color;

    #[test]
    fn color_needs_both_no_color_unset_and_a_tty() {
        assert!(should_color(false, true));
        assert!(!should_color(true, true));
        assert!(!should_color(false, false));
        assert!(!should_color(true, false));
    }
}
