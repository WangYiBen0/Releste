# Player parity with vanilla Celeste

Status: in progress. The movement core is aligned line by line; the
world-interaction states (boost, bounce, pickup, dream dash, wind, room
transitions) are not ported yet.

Authority: `references/pureCeleste.cs` (decompiled vanilla), which is the only
behavioural reference. Port: `crates/game/src/player.rs` for the simulation,
`crates/game/src/session.rs` for hair and sprite drawing.

Vanilla citations use the `// pureCeleste.cs:LINE` form that the audit tool
checks: `cargo run -p releste-game --example citation_audit -- --coverage`.

---

## 1. Ported and aligned

| Mechanic | Vanilla anchor | Notes |
|---|---|---|
| Run / air control | 118386-118392 | `MaxRun` 90, `RunAccel` 1000, `RunReduce` 400, `AirMult` 0.65 |
| Gravity and fall caps | 118376-118382 | 900, `MaxFall` 160, `FastMaxFall` 240, `HalfGravThreshold` 40 |
| Jump | 120690-120730 | `Speed.Y = -105` + `Speed.X += 40 * moveX` (120704-120705); declared 118414-118418 |
| varJump clamp | 122042-122048 | hold-to-rise, released -> timer cleared |
| Jump buffer / coyote | 110376, 119832-119839 | 0.08s buffered press, 0.1s grace |
| Super jump | 120738-120777 | |
| Wall jump / super wall jump | 120779-120877 | includes the anti-re-grab `forceMoveX` window |
| Climb | 120902-122404 | `ClimbJump` 120902, `ClimbCheck` 121815, `ClimbBegin` 122140, `ClimbUpdate` 122184, `ClimbHop` 122380, `SlipCheck` 122404 |
| Duck / unduck | 122004-122008 | duck hitbox 8x6 at +5 (118858) |
| Pose probes | 119281-119295, 121168-121187 | `CanUnDuck`, `CanUnDuckAt`, `DuckFreeAt` (normal vs duck hitbox) |
| Dash | 122731-122831 | eight-way, 0.15s at 240 px/s, attack window 0.3s, cooldown 0.2s |
| Dash refill | 119855-119863 | on the ground, then the 0.2s cooldown |
| Dash end speed | 122824 | `DashDir * 160` |
| Hyper / wavedash | 121577-121582 | a landed down-diagonal dash converts to a horizontal one |
| Corner correction | 121435-121459, 121536-121556, 121625-121665 | dash corner, air-dash landing, ceiling corner, plus the varJump cut |
| Animation ids | 10491-10492, 111375-111388 | `Sprite.Play` writes `LastAnimationID` from the new id |
| Atlas frame lookup | 14311-14329 | index 0 may be the bare texture name, then zero padding |
| Hair metadata and colours | 111452-111480, 120258-120271 | per-frame `hair="dx,dy[:frame]"`, `AC3232` normal |
| Hair rendering | 111155-111205 | outline pass then fill, bangs at node 0 |

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
   branch of `OnGround`, 116950-116969): the tile format has no JumpThru kind.
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
11. **Whole states** (vanilla `StateMachine.SetCallbacks`, 119397-119419):
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
  bottom centre (118856). The collision box is the same 8x11.
- **Movement step**: a fixed 1/2px advance per iteration (`STEP`) replaces
  vanilla's `MoveH`/`MoveV` integer remainder (`movementCounter`).
- **`grounded_at`** has no JumpThru branch (section 2.5).
- **`DashCorrectCheck`** always returns false (no `LedgeBlocker` registered).
- **Hair**: `session::hair_nodes` is a static chain; vanilla's `PlayerHair`
  spring (111114-111139, `StepApproach` 64) lags behind and is not simulated.
- **No death or chapter state**: the session never restarts, so `Die` is
  absent.
- **`var_jump < 9`** approximates `varJumpTimer < 0.15f` (121662-121665) within
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
