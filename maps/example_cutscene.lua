-- Sample cutscene script.
--
-- The point (AGENTS.md 4.5): Lua **never touches World directly**.
-- Every call below only queues a command on the CutsceneBridge, which the
-- simulation consumes on the next frame. A script can therefore pause, replay
-- and fast-forward freely without corrupting physics state.

-- Push the camera in towards the summit while the dummy walks to the cliff.
camera.zoom(120, 40, 1.75, 1.5)

world.walk_to(1, 140, 40, 2.0)
world.wait(30)

-- Play a sound effect, then switch the player's state.
audio.play("event:/sfx/event/rumble", 0.8)
set_player_state("frozen")
world.wait(45)

-- Spawn an NPC to talk.
spawn("granny", 150, 40)
audio.play("event:/sfx/dialog/text", 1.0)
world.wait(180)

-- Hand control back.
set_player_state("normal")
camera.zoom(160, 90, 1.0, 1.0)
