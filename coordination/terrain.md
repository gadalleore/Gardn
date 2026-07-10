# Track: terrain

**Owns:** src/terrain.rs, src/topography.rs
**Scope:** voxel generation, meshing, heightfield, caves

Seen broadcast #3.

## Status
2026-07-09: Re-verified on rebased main (34881f7, includes the core moat
fix). One real seam bug remained beyond the moat and is fixed in this
track's files: the whole-chunk ocean decision in `generate_chunk_blocks`
(details below). Visual check post-fix: coastline renders as a clean
diagonal in all four compass directions, no chunk-square artifacts, no
holes. Ground-truth ASCII dumps confirm real chunks and far ground both
follow the coastline polygon. PR #7 open, awaiting review.

## Currently touching
- files: src/terrain.rs (coastline per-column fix + regression test)

## Notes for other tracks
- Terrain seams investigation results:
  - Real↔real chunk boundaries were provably sealed all along (full boundary
    walls, bit-exact f32 alignment of mesh edges/lattice samples).
  - REAL bug found + fixed in terrain.rs: `generate_chunk_blocks` decided
    ocean-vs-land ONCE per chunk (biome at chunk centre), so a diagonal
    shoreline became a 32-ft checkerboard of all-water / all-dry chunks that
    disagreed with neighbours and the per-cell far ground at every shared
    edge. Now decided per column from the same coastline polygon; mixed
    coastal chunks build both beach and seabed, seabed profile matches
    full-ocean chunks exactly at shared edges. Regression test added
    (`coastal_chunks_split_land_and_water_per_column`).
  - The other visible artifact (chunk-sized squares showing the sea through
    missing ground) was the core moat bug — confirmed fixed in PR #4.
- **foliage-life:** `scatter_chunk_grass` places clumps on `column_tops`
  with no per-spot water check. Mixed coastal chunks now have real water
  columns, so grass can stand in the shallows at the shoreline. Cosmetically
  minor ("reeds"), but a per-spot `biome_at_world != Ocean` check in
  grass.rs would clean it up. Trees already do this check (chunk_store.rs).

## Needs / requests (flag the human)
- (resolved) The dist-3 ring gap flagged earlier was fixed in core (PR #4).

## Done / merged
- 2026-07-09: per-column coastline in `generate_chunk_blocks` (PR pending).
