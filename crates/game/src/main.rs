//! `releste` - the game binary.
//!
//! CLI follows ROADMAP Appendix B where it is already unambiguous
//! (`--help` suffices, `--verbose`/`--log`/`--no-color`, user error vs bug).
//! The sub-command tree from Appendix B lands with Phase 4; right now the
//! default action *is* `play`.

#![deny(warnings)]

mod app;
mod devtools;
mod filmstrip;
use releste_kernel::log as logging;
mod paths;
mod player;
mod service;
mod session;
mod settings;
mod sprites;

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Instant;

use clap::{ArgAction, Parser};
use releste_kernel::Kernel;

use crate::app::GameApp;
use crate::paths::Paths;
use crate::service::{boot_plugins, watch_maps, GameService};
use crate::session::Session;

/// The `releste` command line.
#[derive(Parser, Debug)]
#[command(
    name = "releste",
    version,
    about = "Releste - a 2D precision platformer engine",
    long_about = "Releste - a 2D precision platformer engine.\n\n\
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

    /// Config file (key bindings). Defaults to
    /// `$XDG_CONFIG_HOME/releste/config.toml` (`RELESTE_CONFIG_FILE` overrides
    /// it, this flag overrides both).
    #[arg(long, value_name = "PATH")]
    config: Option<PathBuf>,

    /// Write the resolved config file (current bindings, or the vanilla defaults
    /// when it does not exist yet) and exit.
    #[arg(long)]
    write_config: bool,

    /// Do not scan `plugins/`.
    #[arg(long)]
    no_plugins: bool,

    /// Simulate without a window, print a frame report, and exit.
    #[arg(long)]
    headless: bool,

    /// How many ticks `--headless` simulates; with `--screenshot`, how
    /// many rendered frames to wait before capturing.
    #[arg(long, value_name = "N")]
    frames: Option<u64>,

    /// Render a real frame and save it to this PNG, then exit.
    ///
    /// Needs a window (the headless loop never draws). Useful to inspect
    /// the actual renderer output from scripts and CI.
    #[arg(long, value_name = "PATH")]
    screenshot: Option<PathBuf>,

    /// Write a per-tick player trace (JSONL) to this path. Needs `--headless`.
    ///
    /// First line is a self-describing schema banner (`releste.trace.v1`),
    /// then one object per tick. Deterministic for a given map + no input.
    #[arg(long, value_name = "PATH")]
    trace: Option<PathBuf>,

    /// Replay a TOML scenario (inputs + assertions) and exit.
    ///
    /// Deterministic: no plugins, no hot reload. Assertion failures print
    /// `tick=... field: expected ... got ...` and exit 1. See `tests/scenarios/`.
    #[arg(long, value_name = "PATH")]
    script: Option<PathBuf>,

    /// Compose a PNG strip of the player without a GPU (needs `--script`).
    ///
    /// Frames come from the engine's own draw list, so this shows exactly what the
    /// renderer would draw: handy on headless machines for animation/hair checks.
    #[arg(long, value_name = "PATH")]
    filmstrip: Option<PathBuf>,

    /// How many frames `--filmstrip` composes (default 8).
    #[arg(long, value_name = "N")]
    filmstrip_frames: Option<usize>,

    /// Compose a `--filmstrip` frame every N ticks (default 6).
    #[arg(long, value_name = "N")]
    filmstrip_every: Option<u64>,

    /// Integer upscale for `--filmstrip` (default 1; 4+ makes 1px offsets visible).
    #[arg(long, value_name = "N")]
    filmstrip_zoom: Option<u32>,

    /// Run this many ticks before the first `--filmstrip` sample (default 0).
    #[arg(long, value_name = "N")]
    filmstrip_start: Option<u64>,

    /// `--filmstrip` canvas size, `WxH` (default 160x120; centre = hitbox centre).
    #[arg(long, value_name = "WxH")]
    filmstrip_size: Option<String>,

    /// Dump the map as the runtime parsed it (JSON) and exit.
    ///
    /// Complements `releste-map --example dump_rooms` (raw file view):
    /// this is what the engine actually loaded - spawn, atlas, tile rows.
    #[arg(long, value_name = "PATH")]
    dump_map: Option<PathBuf>,

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
    /// Effective log filter: `--log` > `RUST_LOG` > `--verbose` > the default tier.
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

    /// Effective tick count: headless defaults to 600; `--screenshot` defaults to 30
    /// (the camera chases 1/4 per tick and has converged after 30 ticks, and plugins have started).
    fn frame_count(&self) -> u64 {
        self.frames
            .unwrap_or(if self.screenshot.is_some() { 30 } else { 600 })
    }

    /// Default tier: INFO for this project, WARN for everything else.
    ///
    /// Without this, wgpu-hal would spew every line of the Vulkan loader to the screen
    /// and the log would be unreadable (ROADMAP Appendix A: logs must be human-friendly).
    /// EnvFilter targets match with `starts_with`, so a bare prefix suffices and no `*` is needed.
    fn default_filter() -> &'static str {
        "warn,releste=info,releste_=info,plugins=info"
    }
}

fn main() -> ExitCode {
    let args = Args::parse();

    // The log format is shared by every binary (releste-kernel::log), so the
    // editor, the server and the pipeline look exactly like the game.
    logging::init_with(args.no_color, Some(args.filter().as_str()));

    install_panic_hook();

    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            // The full user-visible error chain; exit code 1 = usage/resource problem (Appendix B rule 5).
            tracing::error!("releste failed: {e:#}");
            ExitCode::FAILURE
        }
    }
}

/// The single `run`: after loading the map it branches to headless / window / dev tools.
fn run(args: &Args) -> anyhow::Result<()> {
    if args.headless && args.screenshot.is_some() {
        anyhow::bail!(
            "--screenshot needs a rendered window; the headless loop never draws. \
             Drop --headless (and optionally set --frames N to choose the capture moment)."
        );
    }
    if args.trace.is_some() && !args.headless {
        anyhow::bail!(
            "--trace needs --headless: the windowed loop ticks in real time, so its \
             per-tick trace is not reproducible. Use `--headless --frames N --trace out.jsonl`."
        );
    }
    if args.script.is_some() && (args.headless || args.screenshot.is_some() || args.trace.is_some())
    {
        anyhow::bail!(
            "--script drives its own deterministic tick loop: drop --headless/--screenshot/\
             --trace and put the trace path in the scenario (`[scenario] trace = \"out.jsonl\"`)."
        );
    }
    if args.dump_map.is_some()
        && (args.script.is_some() || args.headless || args.screenshot.is_some())
    {
        anyhow::bail!("--dump-map only loads and dumps the map; drop the simulation flags.");
    }

    let paths = Paths::resolve(args.root.as_deref());
    info_paths(&paths);
    let config = settings::config_location(args.config.as_deref(), paths.root());
    if args.write_config {
        let bindings = settings::KeyBindings::load_resolved(&config, paths.root());
        bindings.save(&config.path)?;
        println!("ok: config written to {}", config.path.display());
        return Ok(());
    }
    tracing::info!(
        config = %config.path.display(),
        read = %config.read_path(paths.root()).display(),
        "config file"
    );

    let session = Session::load(&args.map, None, &paths)?;

    if let Some(out) = &args.dump_map {
        devtools::dump_session_json(&session, out)?;
        println!("ok: map dump written to {}", out.display());
        return Ok(());
    }

    if let Some(path) = &args.script {
        let script = devtools::load_script(path)?;
        if let Some(note) = &script.scenario.note {
            tracing::info!(note = %note, "scenario");
        }
        let filmstrip = args.filmstrip.as_ref().map(|out| {
            filmstrip::FilmstripOptions::build(
                out.clone(),
                args.filmstrip_frames,
                args.filmstrip_every,
                args.filmstrip_zoom,
                args.filmstrip_start,
                args.filmstrip_size.as_deref().and_then(parse_size),
            )
        });
        let outcome = devtools::run_script(&script, session, &paths, &args.map, filmstrip)?;
        for failure in &outcome.failures {
            println!("FAIL {failure}");
        }
        println!("{}", outcome.summary());
        if !outcome.failures.is_empty() {
            anyhow::bail!(
                "scenario {}: {} failed check(s) - see the FAIL lines above",
                path.display(),
                outcome.failures.len()
            );
        }
        return Ok(());
    }

    if args.headless {
        return run_headless(args, &paths, session);
    }

    let plugins = !args.no_plugins;
    let app = GameApp::new(
        paths,
        session,
        plugins,
        args.screenshot.clone(),
        args.frame_count(),
        config,
    );
    app.run()
}

/// Headless smoke test: run `--frames` ticks, print a report, exit.
fn run_headless(args: &Args, paths: &Paths, session: Session) -> anyhow::Result<()> {
    let host = if args.no_plugins {
        None
    } else {
        boot_plugins(paths)
    };
    let watcher = watch_maps(paths);
    let service = GameService::new(session, host, watcher, None);

    let mut kernel = Kernel::new().with(service);
    let mut trace = match &args.trace {
        Some(path) => Some(devtools::TraceWriter::create(
            path,
            &args.map,
            paths.root(),
        )?),
        None => None,
    };
    let started = Instant::now();
    let frames = args.frame_count();
    for tick in 1..=frames {
        kernel.tick();
        if let Some(trace) = trace.as_mut() {
            if let Some(svc) = kernel.resolve::<GameService>() {
                let room = svc.room_name();
                let snap = svc.player.snapshot(tick, room);
                // At `-v` also print a human timeline; JSONL is always the primary data.
                tracing::debug!("{}", snap.to_line());
                trace.write(&snap)?;
            }
        }
    }
    let elapsed = started.elapsed();
    let traced = trace
        .as_ref()
        .map(devtools::TraceWriter::ticks)
        .unwrap_or(0);
    if let Some(trace) = trace {
        trace.finish()?;
    }
    // Report data is taken after the run: hot reload may happen while running (the document is swapped and
    // reloads increases), and a snapshot before the ticks would report them all as 0.
    let (rooms, entities, room, plugins, reloads) = kernel
        .resolve::<GameService>()
        .map(|s| {
            let (rooms, entities, room) = s.session.summary();
            (rooms, entities, room, s.plugin_count(), s.reloads())
        })
        .unwrap_or((0, 0, "<gone>".into(), 0, 0));
    kernel.shutdown();

    let us_per_tick = if frames == 0 {
        0
    } else {
        elapsed.as_micros() / frames as u128
    };
    println!(
        "ok: {} ticks in {:.2?} ({us_per_tick} us/tick); {rooms} rooms, {entities} entities; \
         spawn room {room}; {plugins} plugins; {reloads} hot reloads; {traced} traced",
        frames, elapsed,
    );
    Ok(())
}

/// Startup path overview: when assets are not found this log line saves a lot of guessing.
fn info_paths(paths: &Paths) {
    tracing::info!(
        root = %paths.root().display(),
        maps = %paths.maps().display(),
        atlas = %paths.atlas_dir().display(),
        plugins = %paths.plugins().display(),
        "resolving assets relative to"
    );
}

/// panic -> a `[FATAL]` line + the original hook output.
///
/// The hook relies only on closure-parameter type inference and never names `PanicHookInfo` -- that type was renamed in 1.81
/// while `rust-version` is locked to 1.80 (AGENTS.md 15.2).
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

/// `WxH` (or `W,H`) for `--filmstrip-size`.
fn parse_size(text: &str) -> Option<(u32, u32)> {
    let (w, h) = text.split_once(['x', 'X', ','])?;
    Some((w.trim().parse().ok()?, h.trim().parse().ok()?))
}
