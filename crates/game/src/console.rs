//! Interactive debug console: input editing, command parsing and history.
//! This is a small developer tool, not a gameplay scripting layer.

use std::collections::VecDeque;

const MAX_HISTORY: usize = 200;

/// Result of one submitted command.
#[derive(Debug, Clone)]
pub(crate) struct CommandResult {
    pub command: &'static str,
    pub output: String,
    pub close: bool,
    pub overlay: Option<bool>,
}

/// Text state, independent of rendering and winit.
#[derive(Debug, Clone, Default)]
pub(crate) struct DebugConsole {
    open: bool,
    input: String,
    history: VecDeque<String>,
    commands: Vec<String>,
    history_pos: Option<usize>,
    draft: String,
}

impl DebugConsole {
    /// Toggle visibility and clear the unfinished command on close.
    pub fn toggle(&mut self) {
        self.open = !self.open;
        self.history_pos = None;
        if !self.open {
            self.input.clear();
        }
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    #[cfg(test)]
    pub fn input(&self) -> &str {
        &self.input
    }

    pub fn display_lines(&self) -> Vec<String> {
        let mut lines: Vec<String> = self.history.iter().cloned().collect();
        lines.push(format!("> {}_", self.input));
        lines
    }

    /// Append printable input; the pico-8 sheet has lowercase glyphs only.
    pub fn text(&mut self, text: &str) {
        for ch in text.chars() {
            if ch.is_ascii_graphic() || ch == ' ' {
                self.input.push(ch.to_ascii_lowercase());
            }
        }
    }

    pub fn backspace(&mut self) {
        self.input.pop();
    }

    pub fn previous(&mut self) {
        if self.commands.is_empty() {
            return;
        }
        let next = match self.history_pos {
            None => {
                self.draft = self.input.clone();
                self.commands.len() - 1
            }
            Some(pos) => pos.saturating_sub(1),
        };
        self.history_pos = Some(next);
        self.input = self.commands[next].clone();
    }

    pub fn next(&mut self) {
        let Some(pos) = self.history_pos else {
            return;
        };
        if pos + 1 >= self.commands.len() {
            self.history_pos = None;
            self.input = std::mem::take(&mut self.draft);
        } else {
            self.history_pos = Some(pos + 1);
            self.input = self.commands[pos + 1].clone();
        }
    }

    fn print(&mut self, text: impl Into<String>) {
        if self.history.len() == MAX_HISTORY {
            self.history.pop_front();
        }
        self.history.push_back(text.into());
    }

    /// Submit the current line; whitespace is normalized.
    pub fn submit(&mut self) -> CommandResult {
        let line = self.input.trim().trim_start_matches('>').trim().to_string();
        self.input.clear();
        self.history_pos = None;
        if line.is_empty() {
            return CommandResult {
                command: "empty",
                output: String::new(),
                close: false,
                overlay: None,
            };
        }
        // Recall only the submitted command, not the output it prints; otherwise
        // Up/Down would walk through status messages instead of commands.
        // Recall only the submitted command, not the output it prints; otherwise
        // Up/Down would walk through status messages instead of commands.
        self.commands.push(line.clone());
        self.print(format!("> {line}"));
        let mut words = line.split_whitespace();
        let name = words.next().unwrap_or_default().to_ascii_lowercase();
        let args: Vec<&str> = words.collect();
        let known = ["help", "clear", "close", "overlay"];
        if !known.contains(&name.as_str()) {
            let output = format!("unknown command: {name} (try help)");
            self.print(&output);
            return CommandResult {
                command: "unknown",
                output,
                close: false,
                overlay: None,
            };
        }
        let result = match name.as_str() {
            "help" => CommandResult {
                command: "help",
                output: "help clear close overlay [on|off|toggle]".into(),
                close: false,
                overlay: None,
            },
            "clear" => {
                self.history.clear();
                CommandResult {
                    command: "clear",
                    output: String::new(),
                    close: false,
                    overlay: None,
                }
            }
            "close" => CommandResult {
                command: "close",
                output: String::new(),
                close: true,
                overlay: None,
            },
            "overlay" => {
                let value = parse_on_off(args.first().copied());
                let output = match value {
                    Some(on) => format!("debug overlay {}", if on { "on" } else { "off" }),
                    None => "usage: overlay [on|off|toggle]".into(),
                };
                self.print(&output);
                CommandResult {
                    command: "overlay",
                    output,
                    close: false,
                    overlay: value,
                }
            }
            _ => unreachable!("the known command list is exhaustive"),
        };
        if result.command != "clear" && result.command != "overlay" {
            self.print(&result.output);
        }
        result
    }
}

fn parse_on_off(arg: Option<&str>) -> Option<bool> {
    match arg {
        None | Some("toggle") => Some(true),
        Some("on" | "true" | "1") => Some(true),
        Some("off" | "false" | "0") => Some(false),
        Some(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typing_folds_case_and_ignores_unsupported_text() {
        let mut c = DebugConsole::default();
        c.text(
            "HeLLo, 123!
",
        );
        assert_eq!(c.input(), "hello, 123!");
        c.backspace();
        assert_eq!(c.input(), "hello, 123");
    }

    #[test]
    fn history_preserves_the_draft() {
        let mut c = DebugConsole::default();
        c.text("help");
        c.submit();
        c.text("overlay on");
        c.submit();
        c.text("draft");
        c.previous();
        assert_eq!(c.input(), "overlay on");
        c.previous();
        assert_eq!(c.input(), "help");
        c.next();
        assert_eq!(c.input(), "overlay on");
        c.next();
        assert_eq!(c.input(), "draft");
    }

    #[test]
    fn commands_have_stable_results() {
        let mut c = DebugConsole::default();
        c.text("overlay off");
        let result = c.submit();
        assert_eq!(result.command, "overlay");
        assert_eq!(result.overlay, Some(false));
        c.text("not-a-command");
        assert_eq!(c.submit().command, "unknown");
        c.text("close");
        assert!(c.submit().close);
    }
}
