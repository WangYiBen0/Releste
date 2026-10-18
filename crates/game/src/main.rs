//! `releste` — the game binary.
//!
//! CLI follows ROADMAP Appendix B where it is already unambiguous
//! (`--help` suffices, `--verbose`/`--log`/`--no-color`, user error vs bug).
//! The sub-command tree from Appendix B lands with Phase 4; right now the
//! default action *is* `play`.

#![deny(warnings)]

mod app;
mod logging;
mod paths;
mod player;
mod service;
mod session;
mod sprites;

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Instant;

use clap::{ArgAction, Parser};
use releste_kernel::Kernel;
use tracing_subscriber::EnvFilter;

use crate::app::GameApp;
use crate::paths::Paths;
use crate::service::{boot_plugins, watch_maps, GameService};
use crate::session::Session;

/// `releste` 命令行。
#[derive(Parser, Debug)]
#[command(
    name = "releste",
    version,
    about = "Releste — a 2D precision platformer engine",
    long_about = "Releste — a 2D precision platformer engine.\n\n\
                  Run with no flags to open `0-Intro.map` in a window.\n\
                  Use `--headless --frames 600` for a smoke test that needs no display."
)]
struct Args {
    /// Map to load, relative to `maps/` (extension optional).
    #[arg(default_value = "0-Intro.map", value_name = "MAP")]
    map: String,

    /// Asset root; defaults to the first of exe dir / repo / cwd that
    /// actually contains maps.
    #[arg(long, value_name = "DIR")]
    root: Option<PathBuf>,

    /// Do not scan `plugins/`.
    #[arg(long)]
    no_plugins: bool,

    /// Simulate without a window, print a frame report, and exit.
    #[arg(long)]
    headless: bool,

    /// How many ticks `--headless` simulates.
    #[arg(long, default_value_t = 600, value_name = "N")]
    frames: u64,

    /// Raise verbosity: `-v` = debug, `-vv` = trace. Ignored when `--log`/`RUST_LOG` is set.
    #[arg(short, long, action = ArgAction::Count, global = true)]
    verbose: u8,

    /// Explicit log level; overrides `RUST_LOG` and `--verbose`.
    #[arg(long, value_name = "LEVEL", global = true)]
    log: Option<String>,

    /// Disable ANSI colours (`NO_COLOR` is honoured too).
    #[arg(long, global = true)]
    no_color: bool,
}

impl Args {
    /// 生效的日志过滤器：`--log` > `RUST_LOG` > `--verbose` > 默认档。
    fn filter(&self) -> String {
        if let Some(level) = &self.log {
            return level.clone();
        }
        if let Ok(env) = std::env::var("RUST_LOG") {
            if !env.trim().is_empty() {
                return env;
            }
        }
        match self.verbose {
            0 => Self::default_filter().to_string(),
            1 => "debug,releste=debug,releste_=debug".to_string(),
            _ => "trace,releste=trace,releste_=trace".to_string(),
        }
    }

    /// 默认档：本项目 INFO、其余一律 WARN。
    ///
    /// 不这么干的话 wgpu-hal 会把 Vulkan loader 的每一行都刷到屏幕上，
    /// 日志就没法读了（ROADMAP Appendix A：日志必须对人友好）。
    /// EnvFilter 的 target 是 `starts_with` 匹配，裸前缀即可，不用 `*`。
    fn default_filter() -> &'static str {
        "warn,releste=info,releste_=info,plugins=info"
    }
}

fn main() -> ExitCode {
    let args = Args::parse();

    let filter = args.filter();
    let ansi = logging::ansi_enabled(args.no_color);
    let subscriber = tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_new(&filter).unwrap_or_else(|e| {
            eprintln!("warning: bad --log/RUST_LOG value ({e}); falling back to `info`");
            EnvFilter::new("info")
        }))
        .event_format(logging::AppendixA::new(ansi));
    subscriber.init();

    install_panic_hook();

    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            // 用户可见的完整错误链；退出码 1 = 用法/资源问题（Appendix B 规则 5）。
            tracing::error!("releste failed: {e:#}");
            ExitCode::FAILURE
        }
    }
}

/// 唯一的 `run`：加载地图后分流到 headless 或窗口。
fn run(args: &Args) -> anyhow::Result<()> {
    let paths = Paths::resolve(args.root.as_deref());
    info_paths(&paths);

    let session = Session::load(&args.map, None, &paths)?;

    if args.headless {
        return run_headless(args, &paths, session);
    }

    let plugins = !args.no_plugins;
    let app = GameApp::new(paths, session, plugins);
    app.run()
}

/// headless 冒烟：跑 `--frames` 个 tick，打印报表，退出。
fn run_headless(args: &Args, paths: &Paths, session: Session) -> anyhow::Result<()> {
    let host = if args.no_plugins {
        None
    } else {
        boot_plugins(paths)
    };
    let watcher = watch_maps(paths);
    let service = GameService::new(session, host, watcher, None);

    let mut kernel = Kernel::new().with(service);
    let started = Instant::now();
    for _ in 0..args.frames {
        kernel.tick();
    }
    let elapsed = started.elapsed();
    // 报表数据在跑完之后取：运行中可能发生热重载（文档被换掉、
    // reloads 增加），tick 前的快照会把它们全部报成 0。
    let (rooms, entities, room, plugins, reloads) = kernel
        .resolve::<GameService>()
        .map(|s| {
            let (rooms, entities, room) = s.session.summary();
            (rooms, entities, room, s.plugin_count(), s.reloads())
        })
        .unwrap_or((0, 0, "<gone>".into(), 0, 0));
    kernel.shutdown();

    let us_per_tick = if args.frames == 0 {
        0
    } else {
        elapsed.as_micros() / args.frames as u128
    };
    println!(
        "ok: {} ticks in {:.2?} ({us_per_tick} us/tick); {rooms} rooms, {entities} entities; \
         spawn room {room}; {plugins} plugins; {reloads} hot reloads",
        args.frames, elapsed,
    );
    Ok(())
}

/// 启动路径一览：找不到资源时这行日志能省掉大量猜测。
fn info_paths(paths: &Paths) {
    tracing::info!(
        root = %paths.root().display(),
        maps = %paths.maps().display(),
        atlas = %paths.atlas_dir().display(),
        plugins = %paths.plugins().display(),
        "resolving assets relative to"
    );
}

/// panic → `[FATAL]` 行 + 原始 hook 输出。
///
/// 钩子只用闭包参数的类型推断，不点名 `PanicHookInfo` —— 该类型在 1.81
/// 才改名，而 `rust-version` 锁在 1.80（AGENTS.md §15.2）。
fn install_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let payload = info.payload();
        let message = if let Some(s) = payload.downcast_ref::<&str>() {
            (*s).to_string()
        } else if let Some(s) = payload.downcast_ref::<String>() {
            s.clone()
        } else {
            "panic with a non-string payload".to_string()
        };
        let message = match info.location() {
            Some(loc) => format!("{message} (at {loc})"),
            None => message,
        };
        logging::print_fatal("releste", &message);
        previous(info);
    }));
}
