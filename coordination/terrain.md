# Track: terrain

**Owns:** src/terrain.rs, src/topography.rs
**Scope:** voxel generation, meshing, heightfield, caves

Seen broadcast #9.

## Status
2026-07-12: **winding down (thermal rotation).** Rotation-2 geology arc is
COMPLETE and merged — depth distribution, rock + bedrock, inedible rock,
blocky craggy LOD boulders, and dirt worm-highways. Owner flew the boulders
and loved them. Branch synced to main (ebbb87e). No open terrain work.

While tidying I also dropped the now-obsolete `#[allow(dead_code)]` on
`BlockType::worm_edible` (it's called from worm.rs now via core PR #22) —
the director flagged it as a trivial next-time cleanup; done here in this
wind-down. Bundled into the wind-down PR.

## Currently touching
- files: (none)

## Open question for the owner (director invited it)
Is 30 ft ("one tree length") the right dirt-depth yardstick now that geology
is the headline feature? I sized `TREE_LENGTH_VOXELS = 30 * VOXELS_PER_FOOT`
to the smallest tree class (mallee/desert oak, 30–55 ft); a bigger yardstick
(e.g. a river red gum) means a proportionally deeper diggable world. Cost
scales ~linearly (see numbers below). It's one constant in topography.rs if
the owner wants it bumped.

## Design notes (for whoever picks up terrain next)
- **Depth authority:** `DIGGABLE_DEPTH_VOXELS` (topography.rs) = 290 voxels
  (2 tree lengths + 12 ft rock + 2 bedrock). worm.rs collision consumes it,
  so the diggable band and the bedrock floor stay in lockstep. `dirt_depth_voxels`
  is the smooth per-column field (P1→0, P50→1 tree length, P99→2).
- **Gen/collision agreement is load-bearing.** Caves, bedrock, boulders and
  highways are all built per-column from the exact inputs worm.rs's
  `ColumnProbe` uses; `generation_and_collision_agree_on_every_land_voxel`
  pins it at ZERO mismatches. Keep any new underground feature column-based
  and overhang-free, or the worm clips through. (That test caught the
  original 130–190 phantom-voxels-per-chunk clip-through.)
- **Measured cost (release):** the 4× deeper world runs chunk gen ~50 ms,
  mesh ~460 ms, both off-thread; ~290k verts/chunk (budget 2.5M).
- **Residual clip:** a diagonal clip against protruding dug-hole blocks
  remains — core worm.rs collision, the director is fixing it. Not terrain's.

## Notes for other tracks
- (rotation 1) Real↔real chunk boundaries are provably sealed; coastal
  per-column shoreline fixed in PR #7 with a regression test.
- **foliage-life:** grass can still stand in coastal shallows
  (`scatter_chunk_grass` has no per-spot ocean check). Also, surfacing
  boulders now appear in `column_tops`, so grass may sprout on top of big
  rocks. Both cosmetic; a block-type-aware grass check isn't possible today
  (grass only receives tops) — happy to expose more if wanted.
- **trees:** tree placement (chunk_store.rs) samples the height formula, so a
  surfacing boulder may intersect the odd trunk base. Cosmetic, flagging for
  awareness.

## Done / merged
- 2026-07-12: blocky craggy boulders + dirt worm-highways — PR #21 (ebbb87e).
  (Superseded auto-closed #20 after its base branch was deleted on #18's merge.)
- 2026-07-11: dirt-depth distribution, rock + bedrock, inedible rock, and the
  gen/collision cave-alignment fix (root-caused the dig clip-through) — PR #18
  (028b132). Core worm.rs hookup (rock refusal, deep dig, near-plane fix) was
  the director's PR #22.
- 2026-07-09: per-column coastline in `generate_chunk_blocks` — PR #7 (a0e6854).
- 2026-07-09: flagged the silhouette-ring moat (fixed in core as PR #4).
