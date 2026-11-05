//! Print a `.map` room's geometry and **boundary openings**, to investigate "which room the
//! player is actually standing in / whether they get stuck or fall at a seam".
//!
//! Usage: `cargo run -p releste-map --example dump_rooms -- assets/maps/0-Intro.map`
//!
//! Prints three parts:
//! 1. One line per room: world rect, tile size, entity count; `player` entities get their own line.
//! 2. The **openings** on each of the room's four edges (contiguous empty runs, in world-coordinate intervals).
//! 3. Seam risks: an opening on one side that is solid across the seam in every neighboring room sharing that seam -> wall stuck across the seam;
//!    an opening with no room on the other side -> walking through means falling out of the world.
//!
//! Note: the vanilla data is designed around "screen-cut transitions" and does not guarantee the tiles on both sides line up; this engine is
//! a continuous world, so these risk points must be absorbed by room transitions (ROADMAP 4.3) or collision fallbacks.

use std::error::Error;
use std::path::PathBuf;

use releste_map::{MapDocument, Room};

fn main() -> Result<(), Box<dyn Error>> {
    let path = PathBuf::from(
        std::env::args()
            .nth(1)
            .expect("usage: dump_rooms <map-file>"),
    );
    let doc = releste_map::load_map(&path)?;
    println!("area = {} ({} rooms)", doc.area, doc.rooms.len());

    for room in &doc.rooms {
        let b = &room.bounds;
        println!(
            "{:<10} x[{:.0},{:.0}) y[{:.0},{:.0})  {}x{} tiles  {} entities  decals bg/fg {}/{}  scenery fg {}x{} bg {}x{}",
            room.name,
            b.x,
            b.x + b.w,
            b.y,
            b.y + b.h,
            room.tiles.width,
            room.tiles.height,
            room.entities.len(),
            room.bg_decals.len(),
            room.fg_decals.len(),
            room.fg_scenery.width,
            room.fg_scenery.height,
            room.bg_scenery.width,
            room.bg_scenery.height,
        );
        for e in &room.entities {
            if e.kind == "player" {
                println!(
                    "           player local ({:.0},{:.0}) -> world ({:.0},{:.0})",
                    e.x,
                    e.y,
                    b.x + e.x,
                    b.y + e.y
                );
            }
            if !e.nodes.is_empty() {
                let nodes: Vec<String> = e
                    .nodes
                    .iter()
                    .map(|(nx, ny)| format!("({nx:.0},{ny:.0})"))
                    .collect();
                println!(
                    "           {} at ({:.0},{:.0}) size {:?}x{:?} -> {}",
                    e.kind,
                    e.x,
                    e.y,
                    e.width,
                    e.height,
                    nodes.join(" ")
                );
            }
        }
    }

    println!("-- area style --");
    println!(
        "clear = #{:02x}{:02x}{:02x}  backgrounds = {}  foregrounds = {}",
        doc.style.background_color[0],
        doc.style.background_color[1],
        doc.style.background_color[2],
        doc.style.backgrounds.len(),
        doc.style.foregrounds.len(),
    );
    for (group, list) in [
        ("bg", &doc.style.backgrounds),
        ("fg", &doc.style.foregrounds),
    ] {
        for bd in list {
            println!(
                "  {group} {:<12} tex={:<16} atlas={:<5} pos=({:.0},{:.0}) scroll=({:.2},{:.2}) speed=({:.0},{:.0}) loop=({},{}) color={:?} blend={}",
                bd.kind,
                bd.texture,
                bd.atlas,
                bd.x,
                bd.y,
                bd.scroll_x,
                bd.scroll_y,
                bd.speed_x,
                bd.speed_y,
                bd.loop_x,
                bd.loop_y,
                bd.effective_color(),
                bd.blend,
            );
        }
    }

    println!("-- openings per edge (world px, empty spans only) --");
    for room in &doc.rooms {
        for (edge, spans) in openings(room) {
            if spans.is_empty() {
                continue; // the whole edge is solid, no opening
            }
            let spans = spans
                .iter()
                .map(|(a, b)| format!("[{a:.0},{b:.0})"))
                .collect::<Vec<_>>()
                .join(" ");
            println!("{:<10} {:>6} {}", room.name, edge, spans);
        }
    }

    println!("-- seam risks --");
    let risks = seam_risks(&doc);
    if risks.is_empty() {
        println!("(none)");
    }
    for risk in risks {
        println!("{risk}");
    }
    Ok(())
}

/// An opening on one edge: `cells` is whether the tile column/row along the edge direction is solid.
/// The returned intervals are in **world coordinates** (pixels) and contain only empty runs.
fn spans_of(cells: Vec<bool>, origin: f32) -> Vec<(f32, f32)> {
    let mut spans = Vec::new();
    let mut start: Option<usize> = None;
    for (i, solid) in cells.into_iter().chain(std::iter::once(true)).enumerate() {
        match (start, solid) {
            (Some(s), true) => {
                spans.push((origin + s as f32 * 8.0, origin + i as f32 * 8.0));
                start = None;
            }
            (None, false) => start = Some(i),
            _ => {}
        }
    }
    spans
}

/// The openings on all four edges (world coordinates).
fn openings(room: &Room) -> Vec<(&'static str, Vec<(f32, f32)>)> {
    let w = room.tiles.width as usize;
    let h = room.tiles.height as usize;
    let solid = |tx: usize, ty: usize| room.tiles.get(tx as u32, ty as u32).unwrap_or(0) != 0;

    let left = (0..h).map(|ty| solid(0, ty)).collect();
    let right = (0..h).map(|ty| solid(w - 1, ty)).collect();
    let top = (0..w).map(|tx| solid(tx, 0)).collect();
    let bottom = (0..w).map(|tx| solid(tx, h - 1)).collect();

    let b = &room.bounds;
    vec![
        ("left", spans_of(left, b.y)),
        ("right", spans_of(right, b.y)),
        ("top", spans_of(top, b.x)),
        ("bottom", spans_of(bottom, b.x)),
    ]
}

fn spans_of_room(room: &Room, edge: &str) -> Vec<(f32, f32)> {
    openings(room)
        .into_iter()
        .find(|(e, _)| *e == edge)
        .map(|(_, s)| s)
        .unwrap_or_default()
}

fn rect(r: &Room) -> (f32, f32, f32, f32) {
    (
        r.bounds.x,
        r.bounds.y,
        r.bounds.x + r.bounds.w,
        r.bounds.y + r.bounds.h,
    )
}

/// The seam opposite `a.edge`: returns neighbor `b`'s opposite edge and the shared segment (along the seam direction).
fn opposite_edge(a: &Room, edge: &str, b: &Room) -> Option<(&'static str, (f32, f32))> {
    let (ax0, ay0, ax1, ay1) = rect(a);
    let (bx0, by0, bx1, by1) = rect(b);
    match edge {
        // Vertical seam: the shared segment runs along y.
        "right" if f32::abs(ax1 - bx0) < 0.5 => Some(("left", (ay0.max(by0), ay1.min(by1)))),
        "left" if f32::abs(ax0 - bx1) < 0.5 => Some(("right", (ay0.max(by0), ay1.min(by1)))),
        // Horizontal seam: the shared segment runs along x.
        "bottom" if f32::abs(ay1 - by0) < 0.5 => Some(("top", (ax0.max(bx0), ax1.min(bx1)))),
        "top" if f32::abs(ay0 - by1) < 0.5 => Some(("bottom", (ax0.max(bx0), ax1.min(bx1)))),
        _ => None,
    }
}

/// `[o0,o1)` minus every interval in `covered`, the remaining uncovered gaps.
fn gaps(o0: f32, o1: f32, covered: &[(f32, f32)]) -> Vec<(f32, f32)> {
    let mut cov = covered.to_vec();
    cov.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut out = Vec::new();
    let mut cur = o0;
    for (c0, c1) in cov {
        if c1 <= cur {
            continue;
        }
        if c0 > cur {
            out.push((cur, c0.min(o1)));
        }
        cur = cur.max(c1);
        if cur >= o1 {
            break;
        }
    }
    if cur < o1 {
        out.push((cur, o1));
    }
    out
}

/// For every opening on every edge: find all neighboring rooms sharing that seam, subtract all of their
/// openings from the opening, and the remaining uncovered sub-segments are the risk points -- the other side is solid (wall stuck across the seam)
/// or void (walking through falls out of the world).
fn seam_risks(doc: &MapDocument) -> Vec<String> {
    let mut out = Vec::new();
    for a in &doc.rooms {
        for (edge, spans) in openings(a) {
            for (o0, o1) in spans {
                // Neighbor metadata + the intersection of their openings with [o0,o1).
                let mut neighbors: Vec<(&Room, &str, (f32, f32))> = Vec::new();
                let mut covered: Vec<(f32, f32)> = Vec::new();
                for b in &doc.rooms {
                    if std::ptr::eq(a, b) {
                        continue;
                    }
                    let Some((bedge, seg)) = opposite_edge(a, edge, b) else {
                        continue;
                    };
                    let (s0, s1) = (o0.max(seg.0), o1.min(seg.1));
                    if s0 >= s1 {
                        continue; // the neighbor is not collinear with this opening run
                    }
                    neighbors.push((b, bedge, (s0, s1)));
                    for (p0, p1) in spans_of_room(b, bedge) {
                        let (c0, c1) = (p0.max(o0), p1.min(o1));
                        if c0 < c1 {
                            covered.push((c0, c1));
                        }
                    }
                }
                for (g0, g1) in gaps(o0, o1, &covered) {
                    // The gap has not yet distinguished "the other side is solid" from "the other side is void":
                    // split the gap at the neighbor rectangles' segment boundaries and classify each piece.
                    let mut cuts: Vec<f32> = vec![g0, g1];
                    for (_, _, (s0, s1)) in &neighbors {
                        if *s0 > g0 && *s0 < g1 {
                            cuts.push(*s0);
                        }
                        if *s1 > g0 && *s1 < g1 {
                            cuts.push(*s1);
                        }
                    }
                    cuts.sort_by(|a, b| a.total_cmp(b));
                    for w in cuts.windows(2) {
                        let (c0, c1) = (w[0], w[1]);
                        if c0 >= c1 {
                            continue;
                        }
                        let here: Vec<String> = neighbors
                            .iter()
                            .filter(|(_, _, (s0, s1))| *s0 <= c0 && c1 <= *s1)
                            .map(|(b, bedge, _)| format!("{}.{}", b.name, bedge))
                            .collect();
                        let kind = if here.is_empty() {
                            "faces void (no room beyond)".to_string()
                        } else {
                            format!("blocked by solids of {}", here.join(", "))
                        };
                        out.push(format!(
                            "{:>10} {:>6} open [{c0:.0},{c1:.0}) -> {kind}",
                            a.name, edge
                        ));
                    }
                }
            }
        }
    }
    out
}
