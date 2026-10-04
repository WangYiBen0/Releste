-- 示例过场脚本。
--
-- 要点（AGENTS.md §4.5）：Lua **永远不直接操作 World**。
-- 下面每个调用都只是把命令排进 CutsceneBridge 的队列，
-- 由仿真在下一帧消费。因此脚本可以随意暂停、重播、快进，
-- 而不会破坏物理状态。

-- 让摄像机推近到山顶，同时假玩家走向悬崖。
camera.zoom(120, 40, 1.75, 1.5)

world.walk_to(1, 140, 40, 2.0)
world.wait(30)

-- 播放音效，然后切换玩家状态。
audio.play("event:/sfx/event/rumble", 0.8)
set_player_state("frozen")
world.wait(45)

-- 生成一个 NPC 说话。
spawn("granny", 150, 40)
audio.play("event:/sfx/dialog/text", 1.0)
world.wait(180)

-- 恢复控制。
set_player_state("normal")
camera.zoom(160, 90, 1.0, 1.0)
