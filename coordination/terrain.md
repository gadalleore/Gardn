# Track: terrain

**Owns (Phase 0):** all terrain-foundation files, including core ones —
`main.rs`, `streaming.rs`, `silhouettes.rs`, `worm.rs`, `world.rs`,
`chunk_store.rs`, `australia.rs`, `terrain.rs`, `topography.rs`. (Authorized by
the director: I'm the sole active agent for Phase 0; the fleet is paused.)
**Branch:** `phase-0-terrain` (NOT `terrain`).

Seen broadcast #10 (the PROJECT PIVOT to a phased build — terrain first, solo).

## Status
2026-07-12: **Phase 0, Stage 1 COMPLETE — the ecology strip now RUNS.**
The director's WIP (2a1d58c) stripped ecology from `main.rs` but left runtime
panics — `finish_chunk_tasks` and `finalize_deferred_unloads` still required
resources no longer inserted (`LeafAssets`/`GrassAssets`/`TreeSpawnQueue`,
`SilhouetteWorld`), and `worm_gravity` required `Wind`. Fixed:

- **streaming.rs** — stripped all grass/leaf/tree integration from the chunk
  pipeline: removed `scatter_chunk_*`, the `TreeSpawnQueue`/`FoliageSkin` types,
  and the orphaned `start_tree_build_tasks`/`finish_tree_build_tasks`. The
  pipeline is now terrain-only (generate → mesh → spawn → unload). Kept the
  `TreesPending`/`ChunkTreesRevealed` marker types (DORMANT) because the paused
  `foliage.rs` still imports them and is still compiled.
- **silhouettes.rs** — **kept far-GROUND, dropped far-TREE LOD.** Rationale: the
  distant terrain vista IS a terrain feature (samples real topography, has two
  regression tests, and is the embryo of the spec §4.3 progressive-unfolding),
  AND `finalize_deferred_unloads` depends on `stand_in_ready` for the gapless
  chunk hand-off — disabling silhouettes entirely would strand it. Removed all
  tree-mesh machinery; `plan_tree_silhouettes` → `plan_ground_silhouettes`,
  retire-on-`loaded`, `stand_in_ready` = ground up.
- **worm.rs** — left BYTE-IDENTICAL. main.rs now seeds a default **calm** `Wind`
  (`.init_resource::<Wind>()`; strength 0, never animated without WeatherPlugin),
  so the crawl model reads a dead-still day. (Director's suggested path.)
- **main.rs** — re-added `SilhouetteWorld` + `plan_ground_silhouettes` /
  `process_silhouette_queue` to the Update chain; added the calm `Wind`.

**Verified:** `cargo check` clean (only expected Phase-0 dead-code warnings);
`cargo test` 19/19 pass incl. both far-ground tests and
`generation_and_collision_agree_on_every_land_voxel`; `cargo run` (GARDN_HIGH=250,
run-lock held+released) ran the full ~60 s with **no panic** — green voxel
Australia lit by the static sun, terrain reaching a clean horizon (far-ground
vista intact, no moat/void), distance blur working.

Also did **Stage 0**: extracted the owner's two `.docx` specs to versioned
markdown — `docs/project_roadmap.md`, `docs/terrain_volume_1_spec.md` (the
Phase 0 source of truth).

## Currently touching
- files: (none — Stage 1 committed; PR pending)

## Open question for the director/owner
**PR base + branch model.** phase-0-terrain is the designated Phase 0 branch and
already carries the director's WIP commit as its tip. For "the first PR" (the
running sandbox) I'm opening **phase-0-terrain → main** so the owner can eyeball
it. Flag if you'd rather Phase 0 stay a separate line off `main` (main keeps the
full ecology game) until Phase 0 matures, or want stages as sub-PRs into
phase-0-terrain. Easy to retarget.

## Stage 2 (next, per director inbox) — subsequent small PRs
1. **State-hook facade** (spec §7): `GetHeight`/`GetSoilDepth`/`GetRockDensity`
   wrapping existing fields; `GetMoisture`/`GetPoolingDuration`/`GetFlammability`/
   `IsCoastalZone` as stubs until water/fire arrive.
2. **Durable mutation** (spec §3.2/§8): serialize `ChunkArchive` to disk so edits
   persist across sessions; new-game vs saved-game per §3.3.
3. **Hierarchical seed tree + progressive unfolding** (spec §4): formalize the
   flat `chunk_seed` into macro→L1→L2→leaf; unify LOD as depth-limited eval
   (can subsume the far-ground I kept).
4. **Worm-scale calibration** (spec §2): worm is ~1 inch; code has
   `WORM_LENGTH = 3 inches`. Flag the exact change for an owner eyeball before
   changing (eye-height/reach/collision feel).

## Design notes (carried forward)
- **Depth authority:** `DIGGABLE_DEPTH_VOXELS` (topography.rs). Gen/collision
  agreement is load-bearing — keep new underground features column-based and
  overhang-free (the `generation_and_collision_agree...` test pins it at zero
  mismatch).
- **Distance language: feet/inches; the worm is 1 inch** (owner's yardstick).

## Done / merged
- 2026-07-12: Phase 0 Stage 0 (spec extraction, 4829a59) + Stage 1 (ecology-strip
  → running sandbox) — PR pending.
- Earlier rotation-1/2 terrain work (coastline, geology, boulders, tunnels,
  cave-alignment) shipped in PRs #7/#18/#21 — see git history.
