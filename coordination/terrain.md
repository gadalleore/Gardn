# Track: terrain

**Owns:** src/terrain.rs, src/topography.rs
**Scope:** voxel generation, meshing, heightfield, caves

Seen broadcast #4.

## Status
2026-07-10: Session winding down per thermal rotation (broadcast #4).
Branch reset to merged main (95f090a) — no local diffs. No open terrain
work; ready for a fresh assignment next rotation.

## Currently touching
- files: (none)

## Notes for other tracks
- Terrain seams investigation results (context for future shifts):
  - Real↔real chunk boundaries are provably sealed (full boundary walls,
    bit-exact f32 alignment of mesh edges and height-lattice samples) —
    don't re-litigate that if seams get reported again.
  - Fixed in PR #7 (merged, a0e6854): `generate_chunk_blocks` decided
    ocean-vs-land once per chunk (biome at chunk centre), quantising the
    shoreline into a 32-ft water/land checkerboard. Now per-column from the
    coastline polygon; mixed coastal chunks share the seabed profile with
    full-ocean chunks (`fill_seabed_column`); caves/ore skip sea columns.
    Regression test: `coastal_chunks_split_land_and_water_per_column`.
  - The chunk-sized holes showing the sea through missing ground was the
    core moat bug — fixed in PR #4 (silhouettes.rs).
- Verification technique that worked well: run `target\debug\gardn.exe`
  with GARDN_HIGH=250 and BEVY_ASSET_ROOT set, drive the flycam with
  synthetic mouse input, screenshot a 4-direction sweep; plus deterministic
  ASCII coast maps from an `#[ignore]` test using `pick_coastal_spawn` +
  `set_spawn_geo_offset` (run it alone — the offset is a global OnceLock).
- **foliage-life:** `scatter_chunk_grass` places clumps on `column_tops`
  with no per-spot water check. Coastal chunks now have real water columns,
  so grass can stand in the shallows at the shoreline. Cosmetically minor
  ("reeds"), but a per-spot `biome_at_world != Ocean` check in grass.rs
  would clean it up. Trees already do this check (chunk_store.rs).

## Needs / requests (flag the human)
- (none open) The dist-3 ring gap was fixed in core (PR #4).

## Done / merged
- 2026-07-09: per-column coastline in `generate_chunk_blocks` — PR #7,
  merged as a0e6854.
- 2026-07-09: flagged the silhouette-ring moat (fixed in core as PR #4).
