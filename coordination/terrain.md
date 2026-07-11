# Track: terrain

**Owns:** src/terrain.rs, src/topography.rs
**Scope:** voxel generation, meshing, heightfield, caves

Seen broadcast #7.

## Status
2026-07-11 (rotation 2): **PR A (#18) is up** — depth distribution, rock +
bedrock, worm_edible(), and the gen/collision cave alignment fix (zero
mismatches, was ~150 phantom voxels/chunk). **PR B (boulders + dirt
worm-highways) implemented and tested locally**, doing the run-lock visual
pass before opening it. Routed worm.rs one-liners below still pending.

## Currently touching
- files: src/terrain.rs, src/topography.rs, README.md (one geology bullet)

## Plan (director's suggested split)
- **PR A — depth + rock + bedrock:** noise-driven dirt depth (P1 = bedrock
  right below, mean = one tree length, P99 = two tree lengths, smooth at dig
  scale), rock band + `Bedrock` block below, `BlockType::worm_edible()`,
  deepened diggable band, and the gen-vs-collision cave alignment fix (see
  findings). Regression tests pin the distribution percentiles and pin
  gen/collision agreement to zero mismatches.
- **PR B — boulders + dirt tunnels:** procedural rock clumps in the dirt
  (some surfacing as giant rocks), dirt tunnels threading the rock band
  (worm highways — they're solid dirt, so they cost zero mesh faces and are
  discovered by digging).

## Key design decisions (dissent welcome)
- **"One tree length" = 30 ft** (the smallest tree class in trees.rs is
  30–55 ft; a river red gum yardstick would demand a 300-ft-deep world).
  So: dirt mean 120 voxels, max 240; diggable band
  `DIGGABLE_DEPTH_VOXELS = 290` (2 tree lengths + 12 ft rock + bedrock),
  vs today's 48. Measured cost (release): chunk gen 8 ms → est ~70 ms, mesh
  100 ms → est ~400 ms, all off-thread; bite latency rises similarly (async,
  no frame hitch). I think that's a fair trade for the owner's spec; if the
  fleet feels streaming slow down, the constant is one knob.
- `CHUNK_DEPTH_VOXELS` in world.rs stays untouched (shared); the new depth
  constant lives in topography.rs (my file) and worm.rs consumes it via the
  routed lines below.

## Findings — dig clip-through bug (owner repro: dig a hole → clip through)
Two independent mechanisms, with measurements:

1. **Phantom cave cells (terrain-side, I'm fixing it in PR A).** Collision
   (`ColumnProbe::solid`) asks `is_cave_cell(vx, vy, vz, formula_surface)`
   per column; generation carves caves per 4×4-column lattice cell using the
   *cell-centre column's bilinearly-interpolated* height. Where they disagree,
   collision believes air inside visibly solid ground. Audit over 4 chunks:
   **130–190 phantom voxels per chunk, most within 12 voxels of the surface**
   — the worm sinks/clips into ground there, and digging (which drops you
   into that band) makes it far more likely. Also ~20k "ghost" voxels per
   chunk (gen air, collision solid → invisible cave floors) from the
   bedrock-band mismatch (`surface - 46` per column vs `min_h - 46` chunk
   floor). Fix: generation now decides caves per column from the exact same
   inputs collision uses (shared per-cell noise + per-column formula surface
   + per-column bedrock), pinned by a zero-mismatch regression test.
2. **Near-plane wall poke (core, worm.rs — routing, not touching).**
   `fits()` in `worm_gravity` (worm.rs:258/330) tests only the camera's own
   column — zero horizontal body radius — so the camera can legally rest
   0 mm from a wall face. The camera near plane (Bevy default 0.1 ft = 1.2 in)
   then renders inside the wall. Freshly dug holes surround you with walls,
   so digging makes it constant. Same family: the roof clamp
   `stand.min(ceiling - 0.03)` (worm.rs:389) parks the eye 0.36 in under a
   roof, inside near-plane range. **Suggested core fix:** give `fits` a
   horizontal margin ≥ the near plane (test the neighbouring column when the
   camera is within ~0.12 ft of a column edge) and widen the roof clamp to
   `ceiling - 0.12`. Happy to spec this in more detail if useful.

## Needs / requests (flag the human) — exact routed lines for worm.rs
Same protocol as the moat. Once PR A merges, worm.rs needs four one-liners
(my side compiles and is safe without them, but digging stops at the old
12-ft floor and rock is still edible until they land):

1. worm.rs `ground_world_y` (line ~143):
   `let bedrock = surface - CHUNK_DEPTH_VOXELS + 2;`
   → `let bedrock = surface - crate::topography::DIGGABLE_DEPTH_VOXELS + 2;`
2. worm.rs `ColumnProbe::at` (line ~184): same replacement.
3. worm.rs `probe_and_carve` hit test (line ~583):
   `if block != BlockType::Water && local.y > voxels.floor_y() {`
   → `if block.worm_edible() && local.y > voxels.floor_y() {`
4. worm.rs `probe_and_carve` ball carve (line ~633):
   `if b == BlockType::Water || clocal.y <= voxels.floor_y() {`
   → `if !b.worm_edible() || clocal.y <= voxels.floor_y() {`

Optional/nice: docs/module-contracts.md terrain section gains
`DIGGABLE_DEPTH_VOXELS`, `dirt_depth_voxels`, `BlockType::worm_edible`
(human-routed doc). And the near-plane fix above is core's call.

## Notes for other tracks
- (carried from rotation 1) Real↔real chunk boundaries are provably sealed;
  coastal per-column shoreline fixed in PR #7 with a regression test.
- **foliage-life:** unchanged from last note — grass can still stand in
  coastal shallows (`scatter_chunk_grass` has no per-spot ocean check).
  After my PR B, giant surfacing boulders will appear in `column_tops`, so
  grass may sprout on top of big rocks. Cosmetic; a block-type-aware check
  isn't possible today (grass only gets tops), happy to expose more if wanted.
- **trees:** tree placement (chunk_store.rs) samples the height formula;
  surfacing boulders (PR B) may intersect the odd trunk base. Cosmetic,
  flagging for awareness.

## Done / merged
- 2026-07-09: per-column coastline in `generate_chunk_blocks` — PR #7 (a0e6854).
- 2026-07-09: flagged the silhouette-ring moat (fixed in core as PR #4).
