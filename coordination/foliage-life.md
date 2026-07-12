# Track: foliage-life

**Owns:** src/grass.rs, src/leaves.rs
**Scope:** grass + collectible leaves; wind, growth, death, decay, sprouting

## Status
Seen broadcast #9 (rebased onto main ad03e39). Rotation 2 assignment picked up
from ../_director/foliage-life.md, INCLUDING the owner's just-added persistence
requirement. Last shift's PR #10 merged; queued stale-doc-comment fix was already
handled by PR #16 — nothing left there.

**PR 1 — leaves ride the wind + persistence — DONE, opening PR.**
Two owner asks, one PR:
1. *Travel:* `animate_floating_leaves` integrates `weather::Wind` into a per-leaf
   `drift` accumulator so leaves TRAVEL downwind (faster in gusts); `wrap_drift`
   recycles each axis within ±7 ft so a patch keeps blowing about instead of
   stripping bare. Old tether (±0.3) + in-place spin — the "static and spinning"
   (Master Dev Note #4) — gone; spin is now a gentle tumble.
2. *Persistence (owner, just added):* every appearance/disappearance/recycle is
   gated OFF-SCREEN via `leaf_out_of_view` (leaf is hidden until it's clearly
   behind the camera, then it's simply there full-size; the ±14 ft wrap jump only
   fires off-screen too, held at the box edge while in view). Despawn was already
   off-screen (chunk unload beyond view distance). So nothing winks in/out/teleports
   under the worm's gaze. Replaced the earlier on-screen edge-fade with this.

Both helpers unit-tested (`wrap_drift` ×3, `leaf_out_of_view` ×4 — 26 tests pass).
Verified in-game under run-lock: PR1 forced-breeze run showed a leaf visibly
travelling across a static camera (temp hack reverted); persistence run showed
leaves rendering, no panic, no regression with the new camera query.

**Tradeoffs / pushback for the director (rule 7):**
- The view gate is ANGULAR (behind-camera), so a leaf freshly streamed *ahead*
  of the worm stays hidden until it falls behind — the path directly ahead can
  look a touch bare of newly-arrived leaves until you pass/turn. This is the
  faithful, SAFE reading of "never a seen transition." If the owner finds forward
  bareness worse than a distant hazed pop, the fix is a distance-OR in the gate
  (reveal leaves beyond clear-view/fog range even when ahead). Left as a follow-up
  rather than guessed — needs an eyeball call. Flagging for the owner's review run.
- Leaves keep a fixed hover height while drifting (no per-frame ground re-sample)
  — fine over the modest ±7 ft box; revisit if relief makes it read wrong.

## Currently touching
- **PR #26 OPEN** (https://github.com/gadalleore/Gardn/pull/26) — leaves ride the
  wind + persistence. Awaiting review/merge. files: src/leaves.rs, README.md.
- Next once merged: PR 2 arc (growth/death/decay/sprouting; "no grass on rocks"
  via topography::boulders_near_chunk — no routing needed, see Needs below).

## Notes for other tracks
-

## Needs / requests (flag the human)
- (PR 2+) "worm eats DEAD foliage" hookup is a worm.rs (core) change — will spec
  exact behaviour here when I build the death/decay arc.
- (PR 2+) "no grass on rocks" — GOOD NEWS, **no terrain routing needed**:
  `topography::boulders_near_chunk(coord)` (pub(crate)) + `Boulder::top_at(wx_vox,
  wz_vox)` (pub(crate)) let grass.rs detect a surfaced boulder itself. Plan: in
  scatter_chunk_grass, fetch boulders once per chunk; for each candidate spot,
  skip if any boulder's `top_at` >= `surface_height_voxels` there (i.e. rock pokes
  above the dirt). topography is a shared module I already import from — I can do
  this in my own file. Downgrades the director's "spec an accessor" contingency.

## Done / merged
- **PR #10** (merged, rotation 1): grass/leaf grow-in on stream, cluster
  families + drifts, per-spot coastal ocean check. Owner eyeballed, called it
  perfect.
