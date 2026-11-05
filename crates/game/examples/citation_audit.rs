//! Citation audit: turn every `pureCeleste.cs:LINE[-LINE]` citation in the
//! source into a machine-checkable list.
//!
//! Why it exists: the rule in this repository is that every behavioural line
//! points back to a vanilla line number
//! (`.agents/skills/releste-vanilla-audit`). When a citation is wrong or has
//! drifted, reading the code by eye will not show it -- this tool lays every
//! citation out and supports one **verifiable citation style**:
//!
//! ```text
//! // the 25 px/s threshold: pureCeleste.cs:120402 `Math.Abs(Speed.X) <= 25f`
//! ```
//!
//! A backtick-quoted snippet **immediately** after a citation (only spaces
//! between) is treated as "that is what the cited lines should contain" and is
//! verified by whitespace-normalised substring match. A citation without a
//! snippet is recorded as `unchecked` -- not a failure, but it cannot catch drift.
//!
//! Usage (from the repository root):
//!
//! ```text
//! cargo run -p releste-game --example citation_audit                # problems only
//! cargo run -p releste-game --example citation_audit -- --json      # machine-readable
//! cargo run -p releste-game --example citation_audit -- --all       # every entry + excerpt
//! cargo run -p releste-game --example citation_audit -- --coverage  # per-file stats
//! cargo run -p releste-game --example citation_audit -- \
//!     --ref references/pureCeleste.cs crates/game/src/player.rs
//! ```
//!
//! Exit code: 0 = no `out_of_range` / `snippet_mismatch`; 1 = there are some.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

/// Output schema of the citation audit.
const SCHEMA: &str = "releste.citation-audit.v1";

/// One citation.
#[derive(Debug)]
struct Entry {
    file: String,
    /// Source line the citation sits on (1-based).
    src_line: usize,
    /// Cited vanilla line range (1-based, inclusive).
    first: usize,
    last: usize,
    /// Backtick-quoted snippet directly after the citation, if any.
    snippet: Option<String>,
    /// Excerpt of the cited vanilla lines (whitespace-normalised, cut at 120
    /// characters).
    excerpt: String,
    /// `ok` / `unchecked` / `out_of_range` / `snippet_mismatch`.
    status: &'static str,
}

fn main() -> std::process::ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("citation_audit: {e}");
            std::process::ExitCode::FAILURE
        }
    }
}

fn run(args: &[String]) -> Result<std::process::ExitCode, String> {
    let mut json = false;
    let mut all = false;
    let mut coverage = false;
    let mut ref_path: Option<PathBuf> = None;
    let mut files: Vec<PathBuf> = Vec::new();

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--json" => json = true,
            "--all" => all = true,
            "--coverage" => coverage = true,
            "--ref" => {
                i += 1;
                ref_path = Some(PathBuf::from(
                    args.get(i).ok_or("--ref needs a path")?.clone(),
                ));
            }
            "-h" | "--help" => {
                println!("{HELP}");
                return Ok(std::process::ExitCode::SUCCESS);
            }
            "--find" => {
                i += 1;
                let needle = args.get(i).ok_or("--find needs a substring")?;
                find_in_reference(needle)?;
                return Ok(std::process::ExitCode::SUCCESS);
            }
            "--methods" => {
                list_methods()?;
                return Ok(std::process::ExitCode::SUCCESS);
            }
            other if other.starts_with("--") => return Err(format!("unknown flag `{other}`")),
            other => files.push(PathBuf::from(other)),
        }
        i += 1;
    }

    let reference = ref_path.unwrap_or_else(default_reference);
    let vanilla = std::fs::read_to_string(&reference)
        .map_err(|e| format!("cannot read {}: {e}", reference.display()))?;
    let vanilla: Vec<&str> = vanilla.lines().collect();

    if files.is_empty() {
        files = default_sources();
    }
    if files.is_empty() {
        return Err("no source files to scan (pass paths or run from the repo root)".into());
    }

    let mut entries = Vec::new();
    for file in &files {
        let text = std::fs::read_to_string(file)
            .map_err(|e| format!("cannot read {}: {e}", file.display()))?;
        scan_file(file, &text, &vanilla, &mut entries);
    }
    entries.sort_by(|a, b| (&a.file, a.src_line).cmp(&(&b.file, b.src_line)));

    let problems = entries
        .iter()
        .filter(|e| e.status == "out_of_range" || e.status == "snippet_mismatch")
        .count();

    if json {
        print_json(&reference, &entries, &files);
    } else {
        print_text(&reference, &entries, &files, all, coverage);
    }
    Ok(if problems == 0 {
        std::process::ExitCode::SUCCESS
    } else {
        std::process::ExitCode::FAILURE
    })
}

const HELP: &str = "\
citation_audit - verify pureCeleste.cs citations (line ranges + optional snippets)

USAGE:
    cargo run -p releste-game --example citation_audit -- [FLAGS] [FILES...]

FLAGS:
    --json        machine-readable JSON (with the vanilla excerpt per citation)
    --all         in text mode, list every entry (default: problems only)
    --coverage    per-file citation and verified counts
    --ref PATH    path to the vanilla source (default references/pureCeleste.cs)
    -h, --help    this help

Verifiable citation style:
    // pureCeleste.cs:120402 `Math.Abs(Speed.X) <= 25f`
    A snippet directly after the citation must occur inside the cited range
    (whitespace-normalised).
    Citations without a snippet are recorded as unchecked: listed, but unable
    to catch drift.

EXIT: 0 = clean; 1 = out_of_range / snippet_mismatch present.";

/// `--find`: locate a substring in the vanilla source and print
/// `LINE| content` (at most 40 hits).
///
/// Use it to locate line numbers when writing a citation instead of trusting
/// memory -- remembered line numbers are exactly where drift comes from.
fn find_in_reference(needle: &str) -> Result<(), String> {
    let reference = default_reference();
    let text = std::fs::read_to_string(&reference)
        .map_err(|e| format!("cannot read {}: {e}", reference.display()))?;
    let mut hits = 0;
    for (idx, line) in text.lines().enumerate() {
        if line.contains(needle) {
            hits += 1;
            if hits <= 40 {
                println!("{}| {}", idx + 1, line.trim_end());
            }
        }
    }
    if hits > 40 {
        println!("... {} more hit(s); narrow the pattern", hits - 40);
    }
    if hits == 0 {
        println!("no match for {needle:?}");
    }
    Ok(())
}

/// `--methods`: the vanilla method declaration list (`LINE| declaration`).
///
/// For checking the endpoints of function-level citations (such as
/// `UpdateSprite (pureCeleste.cs:...)`).
fn list_methods() -> Result<(), String> {
    let reference = default_reference();
    let text = std::fs::read_to_string(&reference)
        .map_err(|e| format!("cannot read {}: {e}", reference.display()))?;
    for (idx, line) in text.lines().enumerate() {
        let t = line.trim();
        let looks_like_decl = (t.starts_with("private ")
            || t.starts_with("public ")
            || t.starts_with("protected ")
            || t.starts_with("internal "))
            && t.ends_with(')')
            && t.contains('(')
            && !t.contains('=')
            && !t.contains(';');
        if looks_like_decl {
            println!("{}| {}", idx + 1, t);
        }
    }
    Ok(())
}

/// Default vanilla path: prefer `references/pureCeleste.cs` at the repo root.
fn default_reference() -> PathBuf {
    let from_manifest =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../references/pureCeleste.cs");
    if from_manifest.exists() {
        from_manifest
    } else {
        PathBuf::from("references/pureCeleste.cs")
    }
}

/// Default scan area: every `.rs` under `crates/` and `tools/`.
fn default_sources() -> Vec<PathBuf> {
    let mut out = Vec::new();
    for root in ["crates", "tools"] {
        collect_rs(Path::new(root), &mut out);
    }
    out.sort();
    out
}

fn collect_rs(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut paths: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
    paths.sort();
    for path in paths {
        if path.is_dir() {
            collect_rs(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

/// Scan every citation in one file.
fn scan_file(file: &Path, text: &str, vanilla: &[&str], out: &mut Vec<Entry>) {
    const MARK: &str = "pureCeleste.cs:";
    for (idx, line) in text.lines().enumerate() {
        let mut rest = line;
        while let Some(pos) = rest.find(MARK) {
            let after = &rest[pos + MARK.len()..];
            let (first, last, consumed) = parse_range(after);
            if let Some(first) = first {
                let snippet = parse_snippet(&after[consumed..]);
                let entry = build_entry(file, idx + 1, first, last, snippet, vanilla);
                out.push(entry);
            }
            // Keep looking for later citations on the same line (comments often
            // carry several).
            rest = &after[consumed..];
            if rest.is_empty() {
                break;
            }
        }
    }
}

/// Parse `123` or `123-456` and return (first, last, bytes consumed).
fn parse_range(s: &str) -> (Option<usize>, usize, usize) {
    let digits: String = s.chars().take_while(char::is_ascii_digit).collect();
    if digits.is_empty() {
        return (None, 0, 0);
    }
    let first: usize = digits.parse().unwrap_or(0);
    let mut consumed = digits.len();
    let mut last = first;
    let tail = &s[consumed..];
    if let Some(rest) = tail.strip_prefix('-') {
        let d2: String = rest.chars().take_while(char::is_ascii_digit).collect();
        if !d2.is_empty() {
            last = d2.parse().unwrap_or(first);
            consumed += 1 + d2.len();
        }
    }
    (Some(first), last, consumed)
}

/// Backtick-quoted snippet **directly** after a citation (spaces only).
///
/// Only snippets that look like code are accepted: at least one ASCII letter
/// or digit. A backtick directly after a citation is often punctuation or
/// prose, and treating those as assertions produces pure noise -- they are not
/// counted as snippets (recorded as unchecked).
fn parse_snippet(s: &str) -> Option<String> {
    let s = s.trim_start();
    let body = s.strip_prefix('`')?;
    let end = body.find('`')?;
    let snippet = body[..end].trim();
    if snippet.is_empty() || !snippet.chars().any(|c| c.is_ascii_alphanumeric()) {
        None
    } else {
        Some(snippet.to_string())
    }
}

/// Normalise: collapse runs of whitespace to one space so comparisons work
/// across line breaks and indentation.
fn squash(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn build_entry(
    file: &Path,
    src_line: usize,
    first: usize,
    last: usize,
    snippet: Option<String>,
    vanilla: &[&str],
) -> Entry {
    let excerpt = if first >= 1 && last <= vanilla.len() && first <= last {
        let joined: String = vanilla[first - 1..last]
            .iter()
            .map(|l| squash(l))
            .filter(|l| !l.is_empty())
            .collect::<Vec<_>>()
            .join(" ");
        truncate(&joined, 120)
    } else {
        String::new()
    };

    // The range must lie inside the file, with first <= last.
    if first == 0 || last == 0 || first > last || last > vanilla.len() {
        return Entry {
            file: file.display().to_string(),
            src_line,
            first,
            last,
            snippet,
            excerpt,
            status: "out_of_range",
        };
    }

    let status = match &snippet {
        Some(want) => {
            let haystack = squash(&vanilla[first - 1..last].join(" "));
            if haystack.contains(&squash(want)) {
                "ok"
            } else {
                "snippet_mismatch"
            }
        }
        None => "unchecked",
    };

    Entry {
        file: file.display().to_string(),
        src_line,
        first,
        last,
        snippet,
        excerpt,
        status,
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max).collect();
    out.push_str("...");
    out
}

fn print_text(reference: &Path, entries: &[Entry], files: &[PathBuf], all: bool, coverage: bool) {
    println!(
        "reference: {} ({} citations in {} files)",
        reference.display(),
        entries.len(),
        files.len()
    );

    let mut problems = 0;
    for e in entries {
        let bad = e.status == "out_of_range" || e.status == "snippet_mismatch";
        if bad {
            problems += 1;
        }
        if !(bad || all) {
            continue;
        }
        println!(
            "{} {}:{} -> {}:{} [{}]",
            status_tag(e.status),
            e.file,
            e.src_line,
            e.first,
            e.last,
            e.status
        );
        if let Some(snippet) = &e.snippet {
            println!("    want: {snippet}");
        }
        if !e.excerpt.is_empty() {
            println!("    have: {}", e.excerpt);
        }
    }

    if coverage {
        println!("\nper-file coverage:");
        let mut by_file: Vec<(&str, usize, usize)> = Vec::new();
        for e in entries {
            let slot = by_file.iter_mut().find(|(name, _, _)| *name == e.file);
            match slot {
                Some((_, total, checked)) => {
                    *total += 1;
                    if e.status == "ok" {
                        *checked += 1;
                    }
                }
                None => by_file.push((e.file.as_str(), 1, usize::from(e.status == "ok"))),
            }
        }
        by_file.sort();
        for (name, total, checked) in by_file {
            println!("  {name}: {checked}/{total} verified");
        }
    }

    let ok = entries.iter().filter(|e| e.status == "ok").count();
    let unchecked = entries.iter().filter(|e| e.status == "unchecked").count();
    println!("\nsummary: ok {ok}, unchecked {unchecked}, problems {problems}");
    if unchecked > 0 {
        println!(
            "note: {unchecked} citation(s) carry no snippet -- adding one \
             (`// pureCeleste.cs:120402 `Math.Abs(Speed.X) <= 25f``) makes drift detectable."
        );
    }
}

fn status_tag(status: &str) -> &'static str {
    match status {
        "ok" => "ok  ",
        "unchecked" => "?   ",
        "out_of_range" => "BAD ",
        "snippet_mismatch" => "BAD ",
        _ => "?   ",
    }
}

fn print_json(reference: &Path, entries: &[Entry], files: &[PathBuf]) {
    let count = |status: &str| entries.iter().filter(|e| e.status == status).count();
    let mut out = String::new();
    out.push('{');
    let _ = write!(
        out,
        "\"schema\":\"{SCHEMA}\",\"reference\":\"{}\",\"files\":{},\"summary\":{{\"citations\":{},\"ok\":{},\"unchecked\":{},\"out_of_range\":{},\"snippet_mismatch\":{}}},\"entries\":[",
        reference.display(),
        files.len(),
        entries.len(),
        count("ok"),
        count("unchecked"),
        count("out_of_range"),
        count("snippet_mismatch"),
    );
    for (i, e) in entries.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        let _ = write!(
            out,
            "{{\"file\":\"{}\",\"src_line\":{},\"vanilla_first\":{},\"vanilla_last\":{},\"status\":\"{}\",\"snippet\":{},\"excerpt\":\"{}\"}}",
            json_escape(&e.file),
            e.src_line,
            e.first,
            e.last,
            e.status,
            match &e.snippet {
                Some(s) => format!("\"{}\"", json_escape(s)),
                None => "null".to_string(),
            },
            json_escape(&e.excerpt),
        );
    }
    out.push_str("]}");
    println!("{out}");
}

fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' | '\r' | '\t' => out.push(' '),
            c if (c as u32) < 0x20 => out.push(' '),
            c => out.push(c),
        }
    }
    out
}
