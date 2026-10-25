//! 引用审计：把源码里的 `pureCeleste.cs:LINE[-LINE]` 引用变成可机器校验的清单。
//!
//! 为什么需要它：本仓库的规则是「每一行行为代码都要能指回原版行号」
//! （`.agents/skills/releste-vanilla-audit`）。引用写错/漂移时，
//! 纯靠人眼读代码看不出来 —— 这个工具把每条引用摊开，并支持一种
//! **可校验的引用写法**：
//!
//! ```text
//! // 120403 的阈值是 25 px/s：pureCeleste.cs:120402 `Math.Abs(Speed.X) <= 25f`
//! ```
//!
//! 引用后面**紧跟**（只隔空格）的反引号片段会被当作"被引用行里应当
//! 出现的内容"，工具用空白归一化后的子串匹配来验证。没有片段的引用
//! 记为 `unchecked`（不是失败，但也没有防漂移能力）。
//!
//! 用法（仓库根目录）：
//!
//! ```text
//! cargo run -p releste-game --example citation_audit                # 只报问题
//! cargo run -p releste-game --example citation_audit -- --json      # 机器可读
//! cargo run -p releste-game --example citation_audit -- --all       # 全部条目 + 原版摘录
//! cargo run -p releste-game --example citation_audit -- --coverage  # 每个文件的统计
//! cargo run -p releste-game --example citation_audit -- \
//!     --ref references/pureCeleste.cs crates/game/src/player.rs
//! ```
//!
//! 退出码：0 = 没有 `out_of_range` / `snippet_mismatch`；1 = 有。

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

/// 引用审计的输出 schema。
const SCHEMA: &str = "releste.citation-audit.v1";

/// 一条引用。
#[derive(Debug)]
struct Entry {
    file: String,
    /// 引用所在的源码行（1-based）。
    src_line: usize,
    /// 被引用的原版行区间（1-based，含两端）。
    first: usize,
    last: usize,
    /// 引用后面紧跟的反引号片段（若有）。
    snippet: Option<String>,
    /// 被引用原版行的摘录（空白归一化，截断到 120 字符）。
    excerpt: String,
    /// `ok` / `unchecked` / `out_of_range` / `snippet_mismatch`。
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
citation_audit — 校验 pureCeleste.cs 引用（行号范围 + 可选片段）

USAGE:
    cargo run -p releste-game --example citation_audit -- [FLAGS] [FILES...]

FLAGS:
    --json        机器可读的 JSON（含每条引用的原版摘录）
    --all         文本模式下列出全部条目（默认只列问题）
    --coverage    每个文件的引用数 / 已校验数统计
    --ref PATH    原版源码路径（默认 references/pureCeleste.cs）
    -h, --help    这份帮助

引用写法（可校验）:
    // pureCeleste.cs:120402 `Math.Abs(Speed.X) <= 25f`
    引用后紧跟的反引号片段必须出现在被引用的行区间里（空白归一化）。
    没有片段的引用记 unchecked —— 能列出来，但没有防漂移能力。

EXIT: 0 = 无问题；1 = 有 out_of_range / snippet_mismatch。";

/// `--find`：在原版里按子串找行，打印 `行号| 内容`（最多 40 条）。
///
/// 写引用时用它定位行号，不要凭记忆 —— 记忆里的行号正是漂移的来源。
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
        println!("… {} more hit(s); narrow the pattern", hits - 40);
    }
    if hits == 0 {
        println!("no match for {needle:?}");
    }
    Ok(())
}

/// `--methods`：原版的方法声明清单（`行号| 声明`）。
///
/// 用于给"函数级引用"（如 `UpdateSprite（pureCeleste.cs:…）`）核对区间端点。
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

/// 默认原版路径：优先仓库根的 `references/pureCeleste.cs`。
fn default_reference() -> PathBuf {
    let from_manifest =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../references/pureCeleste.cs");
    if from_manifest.exists() {
        from_manifest
    } else {
        PathBuf::from("references/pureCeleste.cs")
    }
}

/// 默认扫描面：`crates/` 与 `tools/` 下的全部 `.rs`。
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

/// 扫一个文件里的全部引用。
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
            // 继续找同一行后面的引用（注释里常有一句多个引用）。
            rest = &after[consumed..];
            if rest.is_empty() {
                break;
            }
        }
    }
}

/// 解析 `123` 或 `123-456`，返回 (first, last, 吃掉的字节数)。
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

/// 引用后**紧跟**（只隔空格）的反引号片段。
///
/// 只认"像代码"的片段：至少含一个 ASCII 字母或数字。注释里紧跟引用的
/// 反引号常常是中文标点或散文（``` `；` ```、``` `（` ```），把它们当成
/// 断言会全是误报 —— 这类一律不算片段（记 unchecked）。
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

/// 归一化：把连续空白压成一个空格，便于跨换行/缩进比对。
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

    // 行号区间必须落在文件内，且 first <= last。
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
    out.push('…');
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
            "note: {unchecked} citation(s) carry no snippet — adding one \
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
