# Player parity with vanilla Celeste

Status: in progress. The movement core is aligned line by line; the
world-interaction states (boost, bounce, pickup, dream dash, wind, room
transitions) are not ported yet.

Authority: `references/Celeste.exe.cs.d` (decompiled vanilla), which is the only
behavioural reference. Port: `crates/game/src/player.rs` for the simulation,
`crates/game/src/session.rs` for hair and sprite drawing.

Vanilla citations use full split-source `path.cs:LINE` references that the audit tool
checks: `cargo run -p releste-game --example citation_audit -- --coverage`.

---

## 1. Ported and aligned

| Mechanic | Vanilla anchor | Notes |
|---|---|---|
| Run / air control | references/Celeste.exe.cs.d/Celeste/Player.cs:135-141 | `MaxRun` 90, `RunAccel` 1000, `RunReduce` 400, `AirMult` 0.65 |
| Gravity and fall caps | references/Celeste.exe.cs.d/Celeste/Player.cs:125-131 | 900, `MaxFall` 160, `FastMaxFall` 240, `HalfGravThreshold` 40 |
| Jump | references/Celeste.exe.cs.d/Celeste/Player.cs:2439-2479 | `Speed.Y = -105` + `Speed.X += 40 * moveX` (references/Celeste.exe.cs.d/Celeste/Player.cs:2453-2454); declared references/Celeste.exe.cs.d/Celeste/Player.cs:163-167 |
| varJump clamp | references/Celeste.exe.cs.d/Celeste/Player.cs:3791-3797 | hold-to-rise, released -> timer cleared |
| Jump buffer / coyote | references/Celeste.exe.cs.d/Celeste/Input.cs:153, references/Celeste.exe.cs.d/Celeste/Player.cs:1581-1588 | 0.08s buffered press, 0.1s grace |
| Super jump | references/Celeste.exe.cs.d/Celeste/Player.cs:2487-2526 | |
| Wall jump / super wall jump | references/Celeste.exe.cs.d/Celeste/Player.cs:2528-2626 | includes the anti-re-grab `forceMoveX` window |
| Climb | references/Celeste.exe.cs.d/Celeste/Player.cs:2651-4153 | `ClimbJump` references/Celeste.exe.cs.d/Celeste/Player.cs:2651, `ClimbCheck` references/Celeste.exe.cs.d/Celeste/Player.cs:3564, `ClimbBegin` references/Celeste.exe.cs.d/Celeste/Player.cs:3889, `ClimbUpdate` references/Celeste.exe.cs.d/Celeste/Player.cs:3933, `ClimbHop` references/Celeste.exe.cs.d/Celeste/Player.cs:4129, `SlipCheck` references/Celeste.exe.cs.d/Celeste/Player.cs:4153 |
| Duck / unduck | references/Celeste.exe.cs.d/Celeste/Player.cs:3753-3757 | duck hitbox 8x6 at +5 (references/Celeste.exe.cs.d/Celeste/Player.cs:607) |
| Pose probes | references/Celeste.exe.cs.d/Celeste/Player.cs:1030-1044, references/Celeste.exe.cs.d/Celeste/Player.cs:2917-2936 | `CanUnDuck`, `CanUnDuckAt`, `DuckFreeAt` (normal vs duck hitbox) |
| Dash | references/Celeste.exe.cs.d/Celeste/Player.cs:4480-4580 | eight-way, 0.15s at 240 px/s, attack window 0.3s, cooldown 0.2s |
| Dash refill | references/Celeste.exe.cs.d/Celeste/Player.cs:1604-1612 | on the ground, then the 0.2s cooldown |
| Dash end speed | references/Celeste.exe.cs.d/Celeste/Player.cs:4573 | `DashDir * 160` |
| Hyper / wavedash | references/Celeste.exe.cs.d/Celeste/Player.cs:3326-3331 | a landed down-diagonal dash converts to a horizontal one |
| Corner correction | references/Celeste.exe.cs.d/Celeste/Player.cs:3184-3208, references/Celeste.exe.cs.d/Celeste/Player.cs:3285-3305, references/Celeste.exe.cs.d/Celeste/Player.cs:3374-3414 | dash corner, air-dash landing, ceiling corner, plus the varJump cut |
| Animation ids | references/Celeste.exe.cs.d/Monocle/Sprite.cs:366-367, references/Celeste.exe.cs.d/Celeste/PlayerSprite.cs:169-182 | `Sprite.Play` writes `LastAnimationID` from the new id |
| Atlas frame lookup | references/Celeste.exe.cs.d/Monocle/Atlas.cs:353-371 | index 0 may be the bare texture name, then zero padding |
| Hair metadata and colours | references/Celeste.exe.cs.d/Celeste/PlayerSprite.cs:246-274, references/Celeste.exe.cs.d/Celeste/Player.cs:2007-2020 | per-frame `hair="dx,dy[:frame]"`, `AC3232` normal |
| Hair rendering | references/Celeste.exe.cs.d/Celeste/PlayerHair.cs:101-151 | outline pass then fill, bangs at node 0 |

## 2. Not ported yet

Ordered by how quickly a player would notice, not by difficulty.

1. **Room transitions** (`TransitionTo`, `BeforeSideTransition`, ...): the engine
   is a continuous multi-room world by design (player.rs:19-24), so a seam is
   crossed by walking instead of by a camera transition.
2. **Wind zones** (`WindMove`).
3. **Dash assist** (`DashAssistInit`).
4. **Dream dash** (`DreamDashBegin/Update/End`, `DreamDashCheck`,
   `DreamDashedIntoSolid`).
5. **JumpThru** (`JumpThruBoostBlockedCheck`; the `CollideCheckOutside<JumpThru>`
   branch of `OnGround`, references/Celeste.exe.cs.d/Celeste/Actor.cs:136-155): the tile format has no JumpThru kind.
6. **Entity interactions**: `ExplodeLaunch`, `Boost`/`RedBoost`,
   `Bounce`/`BounceCheck`/`SuperBounce`/`PointBounce`/`SideBounce`,
   `Rebound`/`ReflectBounce`, `HiccupJump`, `WallBoosterCheck`,
   `BadelineBoostLaunch`.
7. **Holdable / carry**: `Pickup`, `Throw`, `Drop`, `UpdateCarry` (`Holding`).
8. **Colliders and blocks**: `OnDashCollide` (`DashBlock`, `DreamBlock`),
   `LedgeBlocker` (`DashCorrectCheck` is a stub that returns false),
   `ClimbBlocker.EdgeCheck`.
9. **Triggers and lifecycle**: `CleanUpTriggers`, `ClimbTrigger`, `OnSquish`,
   `OnBoundsH/V`, `OnTransition`, `SceneEnd`, `Die` and the death/respawn flow.
10. **Effects**: `CreateTrail`, `CreateSplitParticles`,
    `CreateWallSlideParticles`, `PlaySweatEffectDangerOverride`.
11. **Whole states** (vanilla `StateMachine.SetCallbacks`, references/Celeste.exe.cs.d/Celeste/Player.cs:1146-1168):
    3 Swim, 4 Boost, 5 RedDash, 6 HitSquash, 7 Launch, 8 Pickup, 9 DreamDash,
    10 SummitLaunch, 11 Dummy, 12-15 intro coroutines, 16 BirdDashTutorial,
    18 ReflectionFall, 19 StarFly, 20 TempleFall. `Player::step` falls back to
    Normal for all of them.
12. **Misc**: `CorrectDashPrecision`, `UpdateChaserStates`, `IsRiding` (moving
    solids), `UnderwaterMusicCheck`, `FrozenUpdate`, `Swat`, `Throw`.

## 3. Declared deviations

- **Continuous world**: collision covers every room the hitbox touches
  (`hit_in_box`), and landings snap to world cell edges (player.rs:10-24).
- **Anchor**: `pos` is the hitbox **top-left**; vanilla's `Position` is the
  bottom centre (references/Celeste.exe.cs.d/Celeste/Player.cs:605). The collision box is the same 8x11.
- **Movement step**: a fixed 1/2px advance per iteration (`STEP`) replaces
  vanilla's `MoveH`/`MoveV` integer remainder (`movementCounter`).
- **`grounded_at`** has no JumpThru branch (section 2.5).
- **`DashCorrectCheck`** always returns false (no `LedgeBlocker` registered).
- **Hair**: `session::hair_nodes` is a static chain; vanilla's `PlayerHair`
  spring (references/Celeste.exe.cs.d/Celeste/PlayerHair.cs:60-85, `StepApproach` 64) lags behind and is not simulated.
- **No death or chapter state**: the session never restarts, so `Die` is
  absent.
- **`var_jump < 9`** approximates `varJumpTimer < 0.15f` (references/Celeste.exe.cs.d/Celeste/Player.cs:3411-3414) within
  one tick, because the port decrements the timer earlier in the tick.

## 4. How to re-audit

```bash
cargo run -p releste-game --example citation_audit -- --coverage   # citation health
cargo run -p releste-game --example citation_audit -- --find Jump   # where a symbol is cited
```

The `releste-vanilla-audit` skill (`.agents/skills/releste-vanilla-audit/SKILL.md`)
documents the full recipe, including the deterministic tooling this document
relies on: `--trace` (JSONL snapshots), `--script` (TOML replay with
assertions), `--dump-map`, and the GPU-free filmstrip screenshots.
