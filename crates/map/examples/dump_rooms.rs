//! 打印一张 `.map` 的房间几何与**边界通道**，用于排查「玩家到底站在
//! 哪个房间 / 会不会在接缝处卡住或坠落」。
//!
//! 用法：`cargo run -p releste-map --example dump_rooms -- assets/maps/0-Intro.map`
//!
//! 输出三部分：
//! 1. 每个房间一行：世界矩形、瓦片尺寸、实体数；`player` 实体另起一行。
//! 2. 每个房间四条边的**开孔**（连续的空格段，世界坐标区间）。
//! 3. 接缝风险：某侧开孔在所有共享该缝的邻室对面都是实心 → 跨缝卡墙；
//!    开孔段对面没有任何房间 → 走出去就是掉出世界。
//!
//! 注意：原版数据按「切屏转场」设计，不保证两侧瓦片对齐；本引擎是
//! 连续世界，所以这类风险点要靠房间转场（ROADMAP §4.3）或碰撞兜底消化。

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
            "{:<10} x[{:.0},{:.0}) y[{:.0},{:.0})  {}x{} tiles  {} entities",
            room.name,
            b.x,
            b.x + b.w,
            b.y,
            b.y + b.h,
            room.tiles.width,
            room.tiles.height,
            room.entities.len(),
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
        }
    }

    println!("-- openings per edge (world px, empty spans only) --");
    for room in &doc.rooms {
        for (edge, spans) in openings(room) {
            if spans.is_empty() {
                continue; // 整条边都是实心，没有通道
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

/// 一条边的开孔：`cells` 是沿边方向的瓦片列/行是否有固体。
/// 返回的区间是**世界坐标**（像素），只含空段。
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

/// 四条边的开孔（世界坐标）。
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

/// `a.edge` 对面的接缝：返回邻室 `b` 的对边与共享线段（沿缝方向）。
fn opposite_edge(a: &Room, edge: &str, b: &Room) -> Option<(&'static str, (f32, f32))> {
    let (ax0, ay0, ax1, ay1) = rect(a);
    let (bx0, by0, bx1, by1) = rect(b);
    match edge {
        // 垂直接缝：共享线段沿 y。
        "right" if f32::abs(ax1 - bx0) < 0.5 => Some(("left", (ay0.max(by0), ay1.min(by1)))),
        "left" if f32::abs(ax0 - bx1) < 0.5 => Some(("right", (ay0.max(by0), ay1.min(by1)))),
        // 水平接缝：共享线段沿 x。
        "bottom" if f32::abs(ay1 - by0) < 0.5 => Some(("top", (ax0.max(bx0), ax1.min(bx1)))),
        "top" if f32::abs(ay0 - by1) < 0.5 => Some(("bottom", (ax0.max(bx0), ax1.min(bx1)))),
        _ => None,
    }
}

/// `[o0,o1)` 减去 `covered` 里的所有区间，剩下的未覆盖空档。
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

/// 对每条边的每个开孔：找共享该缝的所有邻室，把开孔减去所有邻室的
/// 开孔后，剩下的未覆盖子段就是风险点 —— 对面是实心（跨缝卡墙）
/// 或虚空（走出去掉出世界）。
fn seam_risks(doc: &MapDocument) -> Vec<String> {
    let mut out = Vec::new();
    for a in &doc.rooms {
        for (edge, spans) in openings(a) {
            for (o0, o1) in spans {
                // 邻室元数据 + 它们的开孔与 [o0,o1) 的交集。
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
                        continue; // 邻室与这段开孔不共线
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
                    // 空档还没区分「对面是实心」还是「对面是虚空」：
                    // 按邻室矩形的段边界把空档切碎，逐段分类。
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
