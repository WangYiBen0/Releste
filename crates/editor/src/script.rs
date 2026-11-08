//! Lua script console: user automation against the editor model.
//!
//! The console is deliberately small: one Lua state per run, a handful of
//! model-level functions, and every run collapses into a single undo step
//! (see Editor::begin_stroke). It exists because map edits quickly become
//! repetitive in ways a rectangle tool cannot express ("stamp a checkerboard",
//! "clear the border ring of every room"), and Lua is the scripting layer the
//! engine already locks in.
//!
//! SAFETY: the closures below hold the editor pointer as a raw usize.
//! The Lua state is created and dropped inside one call that exclusively
//! borrows the editor, and nothing spawns threads, so every dereference
//! happens while the pointer is valid.

use std::sync::{Arc, Mutex};

use crate::model::Editor;

/// Persistent state of the script console window.
#[derive(Debug, Default)]
pub struct ScriptConsole {
    /// Whether the window is open.
    pub open: bool,
    /// The Lua source being edited.
    pub code: String,
    /// Lines printed by the last run (echo output and error reports).
    pub output: Vec<String>,
}

/// Renders one Lua value the way a REPL would.
fn display_value(value: mlua::Value) -> String {
    match value {
        mlua::Value::String(s) => s.to_str().map(|text| text.to_owned()).unwrap_or_default(),
        mlua::Value::Integer(i) => i.to_string(),
        mlua::Value::Number(n) => n.to_string(),
        mlua::Value::Boolean(b) => b.to_string(),
        mlua::Value::Nil => "nil".to_string(),
        other => format!("{other:?}"),
    }
}

impl ScriptConsole {
    /// Runs the Lua source against the editor.
    ///
    /// The caller wraps this in Editor::begin_stroke and Editor::end_stroke
    /// so the whole run undoes as one step. A Lua error aborts the run at
    /// the first failing statement; edits made before the error stay in the
    /// document (they are undone together with the stroke).
    pub fn run(editor: &mut Editor, code: &str) -> Result<Vec<String>, String> {
        let lua = mlua::Lua::new();
        let out: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let editor_ptr: Arc<Mutex<usize>> = Arc::new(Mutex::new(editor as *mut Editor as usize));
        let table = lua.create_table().map_err(|e| e.to_string())?;

        // SAFETY: see the module docs. The pointer is valid for the whole
        // call and nothing re-enters Rust while the borrow is held.
        macro_rules! with_editor {
            ($ptr:expr, $body:expr) => {{
                let p = *$ptr.lock().unwrap_or_else(|err| err.into_inner());
                // SAFETY: see above; exclusive borrow held by this call.
                let editor = unsafe { &mut *(p as *mut Editor) };
                $body(editor)
            }};
        }

        let set_tile = {
            let ptr = editor_ptr.clone();
            lua.create_function(move |_, (x, y, v): (i64, i64, i64)| {
                let (x, y) = (x.max(0) as u32, y.max(0) as u32);
                let v = v.clamp(0, u16::MAX as i64) as u16;
                with_editor!(ptr, |e: &mut Editor| e
                    .paint_tile(x, y, v)
                    .map_err(|err| mlua::Error::runtime(err.to_string())))
            })
            .map_err(|e| e.to_string())?
        };
        table.set("set_tile", set_tile).map_err(|e| e.to_string())?;

        let get_tile = {
            let ptr = editor_ptr.clone();
            lua.create_function(move |_, (x, y): (i64, i64)| {
                let (x, y) = (x.max(0) as u32, y.max(0) as u32);
                with_editor!(ptr, |e: &mut Editor| Ok(e
                    .room()
                    .and_then(|r| r.tiles.get(x, y))))
            })
            .map_err(|e| e.to_string())?
        };
        table.set("get_tile", get_tile).map_err(|e| e.to_string())?;

        let fill = {
            let ptr = editor_ptr.clone();
            lua.create_function(move |_, (x0, y0, x1, y1, v): (i64, i64, i64, i64, i64)| {
                let (x0, y0, x1, y1) = (
                    x0.max(0) as u32,
                    y0.max(0) as u32,
                    x1.max(0) as u32,
                    y1.max(0) as u32,
                );
                let v = v.clamp(0, u16::MAX as i64) as u16;
                with_editor!(ptr, |e: &mut Editor| e
                    .fill_tiles(x0, y0, x1, y1, v)
                    .map_err(|err| mlua::Error::runtime(err.to_string())))
            })
            .map_err(|e| e.to_string())?
        };
        table.set("fill", fill).map_err(|e| e.to_string())?;

        let room_width = {
            let ptr = editor_ptr.clone();
            lua.create_function(move |_, ()| {
                with_editor!(ptr, |e: &mut Editor| Ok(e.room().map(|r| r.tiles.width)))
            })
            .map_err(|e| e.to_string())?
        };
        table
            .set("room_width", room_width)
            .map_err(|e| e.to_string())?;

        let room_height = {
            let ptr = editor_ptr.clone();
            lua.create_function(move |_, ()| {
                with_editor!(ptr, |e: &mut Editor| Ok(e.room().map(|r| r.tiles.height)))
            })
            .map_err(|e| e.to_string())?
        };
        table
            .set("room_height", room_height)
            .map_err(|e| e.to_string())?;

        let room_count = {
            let ptr = editor_ptr.clone();
            lua.create_function(move |_, ()| {
                with_editor!(ptr, |e: &mut Editor| Ok(e.document().rooms.len() as i64))
            })
            .map_err(|e| e.to_string())?
        };
        table
            .set("room_count", room_count)
            .map_err(|e| e.to_string())?;

        let current_room = {
            let ptr = editor_ptr.clone();
            lua.create_function(move |_, ()| {
                with_editor!(ptr, |e: &mut Editor| Ok(e.current_room() as i64))
            })
            .map_err(|e| e.to_string())?
        };
        table
            .set("current_room", current_room)
            .map_err(|e| e.to_string())?;

        let set_current_room = {
            let ptr = editor_ptr.clone();
            lua.create_function(move |_, index: i64| {
                let index = index.max(0) as usize;
                with_editor!(ptr, |e: &mut Editor| e
                    .set_current_room(index)
                    .map_err(|err| mlua::Error::runtime(err.to_string())))
            })
            .map_err(|e| e.to_string())?
        };
        table
            .set("set_current_room", set_current_room)
            .map_err(|e| e.to_string())?;

        let room_names = {
            let ptr = editor_ptr.clone();
            lua.create_function(move |_, ()| {
                with_editor!(ptr, |e: &mut Editor| Ok(e
                    .document()
                    .rooms
                    .iter()
                    .map(|r| r.name.clone())
                    .collect::<Vec<String>>()))
            })
            .map_err(|e| e.to_string())?
        };
        table
            .set("room_names", room_names)
            .map_err(|e| e.to_string())?;

        let echo = {
            let out = out.clone();
            lua.create_function(move |_, values: mlua::MultiValue| {
                let parts: Vec<String> = values.into_iter().map(display_value).collect();
                out.lock()
                    .unwrap_or_else(|err| err.into_inner())
                    .push(parts.join("  "));
                Ok(())
            })
            .map_err(|e| e.to_string())?
        };
        table.set("echo", echo).map_err(|e| e.to_string())?;

        lua.globals()
            .set("editor", table)
            .map_err(|e| e.to_string())?;
        lua.load(code).exec().map_err(|e| e.to_string())?;
        drop(lua);
        match Arc::try_unwrap(out) {
            Ok(cell) => Ok(cell.into_inner().unwrap_or_default()),
            // The closures died with the Lua state; this cannot happen, but
            // a failed unwrap would panic and the console must not.
            Err(shared) => Ok(shared.lock().unwrap_or_else(|err| err.into_inner()).clone()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Editor;
    use releste_map::{RectDef, Room, Tileset};

    fn editor_with_room() -> Editor {
        let mut e = Editor::new();
        e.add_room(Room {
            name: "a".into(),
            bounds: RectDef {
                x: 0.0,
                y: 0.0,
                w: 320.0,
                h: 184.0,
            },
            tiles: Tileset::new("Gameplay", 8, 8),
            ..Default::default()
        })
        .unwrap();
        e
    }

    #[test]
    fn script_paints_and_collapses_into_one_undo_step() {
        let mut e = editor_with_room();
        e.begin_stroke("Run script");
        let result = ScriptConsole::run(
            &mut e,
            "editor.set_tile(0, 0, 5)\neditor.set_tile(1, 1, 5)\n",
        );
        e.end_stroke();
        result.expect("script runs");
        assert_eq!(e.room().expect("room").tiles.get(1, 1), Some(5));
        // add_room plus the whole script as one entry.
        assert_eq!(e.undo_len(), 2);
        e.undo().unwrap();
        assert_eq!(e.room().expect("room").tiles.get(1, 1), Some(0));
    }

    #[test]
    fn script_echo_collects_output() {
        let mut e = editor_with_room();
        let out = ScriptConsole::run(&mut e, "editor.echo('hi', 42)\n").unwrap();
        assert_eq!(out, vec!["hi  42".to_string()]);
    }

    #[test]
    fn script_can_read_tiles_and_room_geometry() {
        let mut e = editor_with_room();
        e.fill_tiles(2, 2, 2, 2, 9).unwrap();
        let out = ScriptConsole::run(
            &mut e,
            "editor.echo(editor.get_tile(2, 2))\neditor.echo(editor.room_width())\n",
        )
        .unwrap();
        assert_eq!(out, vec!["9".to_string(), "8".to_string()]);
    }

    #[test]
    fn script_errors_are_reported_not_panics() {
        let mut e = editor_with_room();
        let err = ScriptConsole::run(&mut e, "editor.no_such_fn()").unwrap_err();
        assert!(err.contains("no_such_fn"));
    }

    #[test]
    fn script_sees_room_names_and_switches_rooms() {
        let mut e = editor_with_room();
        e.add_room(Room {
            name: "b".into(),
            bounds: RectDef {
                x: 0.0,
                y: 0.0,
                w: 160.0,
                h: 96.0,
            },
            tiles: Tileset::new("Gameplay", 4, 4),
            ..Default::default()
        })
        .unwrap();
        let out = ScriptConsole::run(
            &mut e,
            "local names = editor.room_names()\neditor.echo(names[1], names[2])\neditor.set_current_room(1)\neditor.echo(editor.room_width())\n",
        )
        .unwrap();
        assert_eq!(out, vec!["a  b".to_string(), "4".to_string()]);
    }
}
