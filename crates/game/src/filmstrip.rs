//! GPU-free sprite compositor.
//!
//! `--filmstrip` renders the engine's own draw list (`Session::player_draws`, the same
//! numbers the wgpu path consumes) with the `image` crate, so a headless machine with no
//! display and no GPU can still produce a picture of Madeline. That is what makes the
//! animation/hair work reviewable: the engine's placement math is the thing under test, so
//! the compositor must not re-derive it -- it only turns [`SpriteDraw`]s into pixels.

use std::collections::HashMap;
use std::path::Path;

use anyhow::{Context as _, Result};
use image::{Rgba, RgbaImage};
use releste_render::{SpriteAtlas, SpriteDraw};

/// The atlas pages, decoded once, keyed by texture-group id.
pub struct CpuTextures {
    pages: HashMap<u32, RgbaImage>,
}

impl CpuTextures {
    /// Decode every texture group of `atlas` (the same payloads the GPU would upload).
    pub fn load(atlas: &SpriteAtlas) -> Result<Self> {
        let mut pages = HashMap::new();
        for (id, group) in atlas.descriptor().textures.iter().enumerate() {
            let path = atlas.group_payload_path(group);
            let image = image::open(&path)
                .with_context(|| format!("failed to decode {}", path.display()))?
                .to_rgba8();
            pages.insert(id as u32, image);
        }
        Ok(CpuTextures { pages })
    }

    /// Texture size, the CPU equivalent of `GpuRenderer::texture_size`.
    pub fn size(&self, id: u32) -> Option<(u32, u32)> {
        self.pages.get(&id).map(|page| page.dimensions())
    }

    /// The page an atlas id lives on.
    pub fn page(&self, id: u32) -> Option<&RgbaImage> {
        self.pages.get(&id)
    }
}

/// Compose `draws` onto a `w`x`h` canvas whose top-left is `origin` in world space.
///
/// Painter's algorithm by `layer` (ascending), nearest-neighbour sampling, source-over
/// blending with the draw's vertex colour -- the same order and colours the batch uses.
pub fn compose(
    textures: &CpuTextures,
    draws: &[SpriteDraw],
    origin: (f32, f32),
    w: u32,
    h: u32,
) -> RgbaImage {
    let mut canvas = RgbaImage::from_pixel(w, h, Rgba([28, 28, 36, 255]));
    let mut ordered: Vec<&SpriteDraw> = draws.iter().collect();
    ordered.sort_by_key(|d| d.layer);

    for draw in ordered {
        let Some(page) = textures.page(draw.atlas) else {
            continue;
        };
        let (pw, ph) = page.dimensions();
        let src_x = (draw.uv_min.0 * pw as f32).round() as i64;
        let src_y = (draw.uv_min.1 * ph as f32).round() as i64;
        let src_w = ((draw.uv_max.0 - draw.uv_min.0) * pw as f32).round() as i64;
        let src_h = ((draw.uv_max.1 - draw.uv_min.1) * ph as f32).round() as i64;
        if src_w <= 0 || src_h <= 0 {
            continue;
        }

        // A **negative** size is XNA's negative-scale mirroring: the geometry and the UVs
        // both reverse (which is how `PlayerHair` mirrors the bangs, `GetHairScale` returns
        // `Facing` for node 0). Normalise it into (min corner, positive size, flip) so the
        // CPU result matches `SpriteBatch::vertices` instead of collapsing to one pixel.
        let (sx, sy) = (draw.size.x.to_f32(), draw.size.y.to_f32());
        let dst_w = sx.abs().round().max(1.0) as i64;
        let dst_h = sy.abs().round().max(1.0) as i64;
        let left = if sx < 0.0 {
            draw.position.x.to_f32() + sx
        } else {
            draw.position.x.to_f32()
        };
        let top = if sy < 0.0 {
            draw.position.y.to_f32() + sy
        } else {
            draw.position.y.to_f32()
        };
        let dst_x = (left - origin.0).round() as i64;
        let dst_y = (top - origin.1).round() as i64;
        let flip = draw.flip_x ^ (sx < 0.0);
        let color = draw.color;

        for dy in 0..dst_h {
            let ty = dst_y + dy;
            if ty < 0 || ty >= h as i64 {
                continue;
            }
            // Nearest-neighbour vertical sample.
            let sy = src_y + (dy * src_h) / dst_h;
            if sy < 0 || sy >= ph as i64 {
                continue;
            }
            for dx in 0..dst_w {
                let tx = dst_x + dx;
                if tx < 0 || tx >= w as i64 {
                    continue;
                }
                let sample_x = if flip {
                    src_x + src_w - 1 - (dx * src_w) / dst_w
                } else {
                    src_x + (dx * src_w) / dst_w
                };
                if sample_x < 0 || sample_x >= pw as i64 {
                    continue;
                }
                let texel = page.get_pixel(sample_x as u32, sy as u32).0;
                let alpha = (f32::from(texel[3]) / 255.0 * color[3]).clamp(0.0, 1.0);
                if alpha <= 0.0 {
                    continue;
                }
                let src = [
                    f32::from(texel[0]) / 255.0 * color[0],
                    f32::from(texel[1]) / 255.0 * color[1],
                    f32::from(texel[2]) / 255.0 * color[2],
                ];
                let dst = canvas.get_pixel(tx as u32, ty as u32).0;
                let dst_a = f32::from(dst[3]) / 255.0;
                let out_a = alpha + dst_a * (1.0 - alpha);
                let blend = |s: f32, d: u8| -> u8 {
                    let d = f32::from(d) / 255.0;
                    let v = (s * alpha + d * dst_a * (1.0 - alpha)) / out_a.max(f32::EPSILON);
                    (v * 255.0).round().clamp(0.0, 255.0) as u8
                };
                canvas.put_pixel(
                    tx as u32,
                    ty as u32,
                    Rgba([
                        blend(src[0], dst[0]),
                        blend(src[1], dst[1]),
                        blend(src[2], dst[2]),
                        (out_a * 255.0).round().clamp(0.0, 255.0) as u8,
                    ]),
                );
            }
        }
    }
    canvas
}

/// A small plus in `color`, used to make placements judgeable by eye.
pub fn marker(canvas: &mut RgbaImage, x: i64, y: i64, color: Rgba<u8>) {
    for (dx, dy) in [(0i64, 0i64), (-2, 0), (2, 0), (0, -2), (0, 2)] {
        let (px, py) = (x + dx, y + dy);
        if px >= 0 && py >= 0 && (px as u32) < canvas.width() && (py as u32) < canvas.height() {
            canvas.put_pixel(px as u32, py as u32, color);
        }
    }
}

/// Nearest-neighbour integer upscale.
pub fn zoom(image: &RgbaImage, factor: u32) -> RgbaImage {
    let factor = factor.max(1);
    if factor == 1 {
        return image.clone();
    }
    let (w, h) = image.dimensions();
    let mut out = RgbaImage::new(w * factor, h * factor);
    for (x, y, texel) in image.enumerate_pixels() {
        for dy in 0..factor {
            for dx in 0..factor {
                out.put_pixel(x * factor + dx, y * factor + dy, *texel);
            }
        }
    }
    out
}

/// Stitch frames left-to-right into one strip with a 1px separator.
pub fn strip(frames: &[RgbaImage]) -> RgbaImage {
    let (fw, fh) = frames.first().map(|f| f.dimensions()).unwrap_or((1, 1));
    let total_w = fw.saturating_mul(frames.len() as u32) + frames.len().saturating_sub(1) as u32;
    let mut out = RgbaImage::from_pixel(total_w.max(1), fh.max(1), Rgba([60, 60, 70, 255]));
    for (i, frame) in frames.iter().enumerate() {
        let x = i as u32 * (fw + 1);
        for (px, py, texel) in frame.enumerate_pixels() {
            if px < fw && py < fh {
                out.put_pixel(x + px, py, *texel);
            }
        }
    }
    out
}

/// Where `--filmstrip` writes and how many frames to sample.
#[derive(Debug, Clone)]
pub struct FilmstripOptions {
    /// Output PNG path.
    pub path: std::path::PathBuf,
    /// Frames to compose (default 8).
    pub frames: usize,
    /// Compose one frame every N ticks (default 6).
    pub every: u64,
    /// Canvas size (default 160x120, centred on the player each frame).
    pub size: (u32, u32),
    /// Integer upscale applied before writing (nearest neighbour), so a 1px offset is
    /// visible in the PNG (default 1).
    pub zoom: u32,
    /// Ticks to run before the first sample, so a specific moment can be inspected
    /// (default 0).
    pub start: u64,
}

impl FilmstripOptions {
    /// Build from the CLI: `path`, counts and zoom.
    pub fn build(
        path: impl Into<std::path::PathBuf>,
        frames: Option<usize>,
        every: Option<u64>,
        zoom: Option<u32>,
        start: Option<u64>,
        size: Option<(u32, u32)>,
    ) -> Self {
        let (w, h) = size.unwrap_or((160, 120));
        FilmstripOptions {
            path: path.into(),
            frames: frames.unwrap_or(8),
            every: every.unwrap_or(6),
            size: (w.clamp(16, 4096), h.clamp(16, 4096)),
            zoom: zoom.unwrap_or(1).clamp(1, 32),
            start: start.unwrap_or(0),
        }
    }
}

/// Accumulates composed frames during a headless run and writes them at the end.
pub struct Recorder {
    opts: FilmstripOptions,
    textures: CpuTextures,
    frames: Vec<RgbaImage>,
    /// Ticks until the next sample (a countdown, so `--filmstrip-every 0` cannot divide by
    /// zero and clippy does not have to be told about `%`).
    due_in: u64,
    /// Ticks elapsed so far (`--filmstrip-start` skips the first N).
    tick: u64,
}

impl Recorder {
    /// Build a recorder for `opts`, decoding the session's atlas.
    pub fn new(opts: FilmstripOptions, session: &crate::session::Session) -> Result<Self> {
        let textures = CpuTextures::load(&session.atlas)?;
        let every = opts.every.max(1);
        Ok(Recorder {
            opts: FilmstripOptions { every, ..opts },
            textures,
            frames: Vec::new(),
            due_in: 0,
            tick: 0,
        })
    }

    /// Sample this tick if it is due (uses the live player state, `alpha = 1`).
    pub fn record(&mut self, session: &crate::session::Session, player: &crate::player::Player) {
        let elapsed = self.tick;
        self.tick += 1;
        if elapsed < self.opts.start {
            return;
        }
        let due = self.due_in == 0 && self.frames.len() < self.opts.frames;
        if self.due_in == 0 {
            // The next sample is `every` ticks after this one.
            self.due_in = self.opts.every.saturating_sub(1);
        } else {
            self.due_in -= 1;
        }
        if !due {
            return;
        }
        let body = player.anim_frame_texture(&session.anims);
        let hair = player.anim_frame_hair(&session.anims);
        let draws = session.player_draws(
            &|id| self.textures.size(id),
            player.pos,
            player.facing < 0,
            player.hair_color(),
            body.as_deref(),
            hair,
        );
        // Centre the canvas on the hitbox centre so every frame is comparable, but keep
        // the two anchors distinct: `Player.Position` is the hitbox bottom centre (the feet,
        // 118856) and that is what `hair_nodes` and `body_draw` take, while the hitbox
        // centre is only a visual reference.
        let feet = player.pos
            + releste_math::Vec2::from_f32s(crate::player::PLAYER_W * 0.5, crate::player::PLAYER_H);
        let centre = player.pos
            + releste_math::Vec2::from_f32s(
                crate::player::PLAYER_W * 0.5,
                crate::player::PLAYER_H * 0.5,
            );
        let origin = (
            centre.x.to_f32() - self.opts.size.0 as f32 / 2.0,
            centre.y.to_f32() - self.opts.size.1 as f32 / 2.0,
        );
        let mut frame = compose(
            &self.textures,
            &draws,
            origin,
            self.opts.size.0,
            self.opts.size.1,
        );
        let (cw, ch) = (self.opts.size.0 as i64, self.opts.size.1 as i64);
        // Cyan plus: the hitbox centre (canvas centre). Green plus: the feet anchor
        // (`Player.Position`); the body's bottom row must sit on it. Magenta plus: the hair
        // root (`feet + HairOffset*(Facing,1) + (0,-9)`, vanilla 111116-111117), floored as
        // `PlayerHair.Render` does (111179). A hair bug moves their relation.
        marker(&mut frame, cw / 2, ch / 2, Rgba([80, 220, 255, 255]));
        let (ax, ay) = (
            (feet.x.to_f32() - origin.0).round() as i64,
            (feet.y.to_f32() - origin.1).round() as i64,
        );
        marker(&mut frame, ax, ay, Rgba([80, 255, 120, 255]));
        let facing = if player.facing < 0 { -1.0 } else { 1.0 };
        let root = crate::session::hair_nodes(feet, facing, hair)[0];
        let (rx, ry) = (
            (root.x.to_f32() - origin.0).round() as i64,
            (root.y.to_f32() - origin.1).round() as i64,
        );
        marker(&mut frame, rx, ry, Rgba([255, 0, 255, 255]));
        self.frames.push(zoom(&frame, self.opts.zoom));
    }

    /// Frames composed so far.
    pub fn len(&self) -> usize {
        self.frames.len()
    }

    /// Write the strip and return the path.
    pub fn finish(self) -> Result<std::path::PathBuf> {
        let path = self.opts.path.clone();
        write(&path, &self.frames)?;
        Ok(path)
    }
}

/// Write `frames` to `path` as one strip.
pub fn write(path: &Path, frames: &[RgbaImage]) -> Result<()> {
    let image = strip(frames);
    image
        .save(path)
        .with_context(|| format!("failed to write {}", path.display()))?;
    Ok(())
}
