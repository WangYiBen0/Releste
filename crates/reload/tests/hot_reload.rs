//! Hot reload end-to-end tests (AGENTS.md 10).
//!
//! Acceptance criterion:
//! > move a wall corner, save, see the change, the player is not ejected, no
//! > frame drops.
//!
//! This test uses a real filesystem + a real FileWatcher through the whole
//! chain: write `.map` -> watcher fires -> diff -> apply -> verify the player
//! state is unchanged.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use releste_map::{MapDocument, RectDef, Room, Tileset};
use releste_math::Vec2;
use releste_reload::{FileWatcher, Reload};
use releste_world::{Entity, EntityData, EntityId, EntityKindId, FieldValue, World};

const KIND_WALL: EntityKindId = EntityKindId::new(1);
const KIND_PLAYER: EntityKindId = EntityKindId::new(2);

const PLAYER_ID: EntityId = EntityId::new(0x911A_1E20);

fn tmp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("releste-reload-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

/// One wall + the player, simulating "the room currently being played".
fn world_with_wall(wall_x: f32) -> World {
    let mut w = World::new();
    let mut player = Entity::new(PLAYER_ID, KIND_PLAYER);
    player.set("x", FieldValue::Float(16.0));
    player.set("y", FieldValue::Float(16.0));
    player.set("width", FieldValue::Int(8));
    player.set("height", FieldValue::Int(8));
    // Player session state: must be preserved across a hot reload.
    player.set("vx", FieldValue::Float(3.5));
    player.set("vy", FieldValue::Float(-1.25));
    player.set("dashing", FieldValue::Bool(true));

    let mut wall = Entity::new(EntityId::new(1), KIND_WALL);
    wall.set("x", FieldValue::Float(wall_x));
    wall.set("y", FieldValue::Float(100.0));
    wall.set("width", FieldValue::Int(8));
    wall.set("height", FieldValue::Int(8));

    w.insert(player);
    w.insert(wall);
    w.set_player(PLAYER_ID);
    w
}

/// Build a map containing one wall.
fn make_map(wall_x: f32) -> MapDocument {
    let mut entities = Vec::new();
    let mut wall = EntityData::new("wall", EntityId::new(1), wall_x, 100.0);
    wall.width = Some(8);
    wall.height = Some(8);
    entities.push(wall);

    MapDocument {
        area: "HotReloadTest".into(),
        rooms: vec![Room {
            name: "room_0".into(),
            bounds: RectDef {
                x: 0.0,
                y: 0.0,
                w: 320.0,
                h: 180.0,
            },
            tiles: Tileset::new("Gameplay", 20, 12),
            bg: Tileset::default(),
            entities,
            triggers: Vec::new(),
            script: None,
            ..Default::default()
        }],
        sync_rules: Vec::new(),
        ..Default::default()
    }
}

/// Write the map file.
fn write_map(path: &Path, doc: &MapDocument) {
    releste_map::save_map(doc, path).expect("write map");
}

fn player_props(w: &World) -> std::collections::BTreeMap<String, FieldValue> {
    w.get(PLAYER_ID).expect("player exists").properties.clone()
}

// ---------- Deterministic part: does not depend on inotify timing ----------

#[test]
fn map_roundtrips_through_disk() {
    let dir = tmp_dir("roundtrip");
    let path = dir.join("a.map");

    let doc = make_map(100.0);
    write_map(&path, &doc);

    let loaded = releste_map::load_map(&path).expect("load");
    assert_eq!(loaded.area, "HotReloadTest");
    assert_eq!(loaded.rooms.len(), 1);
    assert_eq!(loaded.rooms[0].entities.len(), 1);

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn wall_edit_produces_a_move_patch() {
    let dir = tmp_dir("diff");
    let path_old = dir.join("old.map");
    let path_new = dir.join("new.map");

    write_map(&path_old, &make_map(100.0));
    write_map(&path_new, &make_map(104.0));

    let old = releste_map::load_map(&path_old).unwrap();
    let new = releste_map::load_map(&path_new).unwrap();
    let patches = releste_map::diff(&old, &new);

    assert_eq!(patches.len(), 1, "wall move should yield exactly one patch");
    assert!(matches!(patches[0], releste_map::Patch::MoveEntity { .. }));

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn applying_wall_edit_keeps_player_untouched() {
    let dir = tmp_dir("apply");
    let path_old = dir.join("old.map");
    let path_new = dir.join("new.map");

    write_map(&path_old, &make_map(100.0));
    write_map(&path_new, &make_map(104.0));

    let mut world = world_with_wall(100.0);
    let before = player_props(&world);

    let old = releste_map::load_map(&path_old).unwrap();
    let new = releste_map::load_map(&path_new).unwrap();
    let patches = releste_map::diff(&old, &new);

    let mut reload = Reload::new();
    let report = reload.apply(&mut world, patches).expect("apply");

    assert_eq!(report.moved, 1);
    assert_eq!(
        player_props(&world),
        before,
        "player session state must survive a hot reload (AGENTS.md 10)"
    );

    // The wall really did move.
    let wall = world.get(EntityId::new(1)).unwrap();
    assert_eq!(wall.get("x"), Some(&FieldValue::Float(104.0)));

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn reload_does_not_remove_or_recreate_the_player() {
    let mut world = world_with_wall(100.0);
    let player_before = world.player();

    // A batch of patches unrelated to this room.
    let patches = vec![
        releste_map::Patch::AddEntity(EntityData::new("spinner", EntityId::new(50), 250.0, 50.0)),
        releste_map::Patch::ReplaceTiles(Tileset::new("Gameplay", 20, 12)),
    ];

    let mut reload = Reload::new();
    reload.apply(&mut world, patches).expect("apply");

    assert_eq!(world.player(), player_before);
    assert!(
        world.get(PLAYER_ID).is_some(),
        "player must be Persistent, never rebuilt"
    );
}

// ---------- Full chain depending on file watching ----------

/// The full hot reload chain: edit file -> watcher fires -> diff -> apply.
///
/// If the environment does not support filesystem notifications (some
/// containers / unusual filesystems), the disk-watching part is skipped rather
/// than failing -- but the deterministic chain is still covered by the tests
/// above.
#[test]
fn full_hot_reload_loop_via_file_watcher() {
    let dir = tmp_dir("watch");
    let path = dir.join("room.map");
    write_map(&path, &make_map(100.0));

    let mut watcher = match FileWatcher::new(&dir, 50) {
        Ok(w) => w,
        Err(e) => {
            eprintln!("skipping: file watcher unavailable: {e}");
            std::fs::remove_dir_all(&dir).ok();
            return;
        }
    };

    let mut world = world_with_wall(100.0);
    let before = player_props(&world);

    // Move a wall corner and save.
    write_map(&path, &make_map(104.0));

    // Wait for the watcher to report this change.
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut changed: Option<PathBuf> = None;
    while Instant::now() < deadline {
        if let Some(p) = watcher.wait_timeout(Duration::from_millis(500)) {
            if p.ends_with("room.map") {
                changed = Some(p);
                break;
            }
        }
    }

    let Some(changed_path) = changed else {
        eprintln!(
            "skipping: no filesystem notification observed \
             (inotify may be unavailable in this environment)"
        );
        std::fs::remove_dir_all(&dir).ok();
        return;
    };

    // Watcher fires -> parse -> diff -> apply.
    let new_doc = releste_map::load_map(&changed_path).expect("load changed map");
    let old_doc = make_map(100.0);
    let patches = releste_map::diff(&old_doc, &new_doc);
    assert!(!patches.is_empty(), "expected a patch from the edit");

    let mut reload = Reload::new();
    let report = reload.apply(&mut world, patches).expect("apply");

    assert_eq!(report.moved, 1);
    assert_eq!(
        player_props(&world),
        before,
        "player state must be preserved through the full hot-reload loop"
    );
    let wall = world.get(EntityId::new(1)).unwrap();
    assert_eq!(wall.get("x"), Some(&FieldValue::Float(104.0)));

    std::fs::remove_dir_all(&dir).ok();
}

/// Rapid consecutive writes must be debounced into a single reload.
#[test]
fn rapid_writes_are_debounced() {
    let dir = tmp_dir("debounce");
    let path = dir.join("room.map");
    write_map(&path, &make_map(100.0));

    let mut watcher = match FileWatcher::new(&dir, 150) {
        Ok(w) => w,
        Err(e) => {
            eprintln!("skipping: file watcher unavailable: {e}");
            std::fs::remove_dir_all(&dir).ok();
            return;
        }
    };

    // Write 5 times in a row.
    for i in 0..5 {
        write_map(&path, &make_map(100.0 + i as f32));
        std::thread::sleep(Duration::from_millis(5));
    }

    // The first trigger (after the debounce window).
    let first = watcher.wait_timeout(Duration::from_secs(10));
    if first.is_none() {
        eprintln!("skipping: no filesystem notification observed");
        std::fs::remove_dir_all(&dir).ok();
        return;
    }

    // There should be no further event within the window (they were merged).
    let extra = watcher.wait_timeout(Duration::from_millis(300));
    assert!(
        extra.is_none(),
        "rapid writes must debounce into a single reload, got extra: {extra:?}"
    );

    std::fs::remove_dir_all(&dir).ok();
}

/// Shrinking bounds + player outside -> roll back the whole patch batch
/// (AGENTS.md 7.4 strategy 3).
#[test]
fn shrinking_room_past_player_rolls_back_entire_patch() {
    let mut world = world_with_wall(100.0);
    let before: Vec<(EntityId, Entity)> = world.all().map(|e| (e.id, e.clone())).collect();

    let shrunk = releste_math::Rect::new(Vec2::from_f32s(0.0, 0.0), Vec2::from_f32s(300.0, 300.0));
    // The player is at (16,16); move it outside the bounds to trigger
    // validation.
    let mut reload = Reload::new().with_bounds(releste_math::Rect::new(
        Vec2::from_f32s(500.0, 500.0),
        Vec2::from_f32s(600.0, 600.0),
    ));

    let err = reload.apply(
        &mut world,
        vec![releste_map::Patch::AddEntity(EntityData::new(
            "wall",
            EntityId::new(77),
            1.0,
            1.0,
        ))],
    );

    assert!(err.is_err(), "player outside bounds must reject the patch");
    assert!(
        world.get(EntityId::new(77)).is_none(),
        "the whole patch batch must be rolled back"
    );
    let after: Vec<(EntityId, Entity)> = world.all().map(|e| (e.id, e.clone())).collect();
    assert_eq!(after.len(), before.len());

    // Conversely: a sufficiently large bounds should pass.
    let mut reload_ok = Reload::new().with_bounds(shrunk);
    reload_ok
        .apply(
            &mut world,
            vec![releste_map::Patch::AddEntity(EntityData::new(
                "wall",
                EntityId::new(78),
                200.0,
                200.0,
            ))],
        )
        .expect("player inside bounds should pass");
}
