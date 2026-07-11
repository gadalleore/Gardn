# Track: trees

**Owns:** src/foliage.rs, src/trees.rs
**Scope:** tree generation, canopy LOD + sway

## Status
- seen broadcast #6; followed director inbox note of 2026-07-11 (stacked-PR
  rebase after #13's squash-merge): `trees-lod-fade` rebased onto main @
  7604ba1 — duplicate #13 commits dropped, coordination/trees.md conflict
  resolved keeping the fullest notes, branch is now a single foliage.rs
  commit atop main, force-pushed with lease. cargo check clean post-rebase.
- **PR #14 open (rebased, ready)** — LOD cross-fade. Covers BOTH the
  distance-band rung swaps (two-phase alpha cross-fade that never stops
  writing depth — distance-blur stays correct) and the director's
  trunk-before-canopy spawn flash (root cause: unordered FoliagePlugin systems
  let `reveal_built_chunks` beat `update_foliage_lod` to a just-spawned tree;
  fixed by chaining rung selection before reveal). Verified with a run-locked
  game session: no panics, canopies intact through fades, LOD bands exercised
  on the 300-ft fall.
- **Run-lock mea culpa (resolved):** my two verification runs for PR #13
  happened before I pulled/read broadcast #4's run-lock rule — they ran
  unlocked. No collision occurred. The cross-fade verification run took
  `../_runlock` properly and released it.

## Currently touching
- files: none (both PRs out; awaiting review)

## Notes for other tracks
- The LOD cross-fade stays entirely inside foliage.rs: per-rung materials are
  cloned lazily at first fade, so `streaming.rs`'s spawn code and the
  `FoliageLodGroup { center, radius, level }` construction are untouched — no
  contract change.
- README's "cross-fade seamlessly" claim under Titan trees was aspirational
  until now (the code hard-swapped); the LOD PR makes it true, so no README
  edit was needed.

## Needs / requests (flag the human)
- PR #14 rebased and ready to merge.

## Done / merged
- PR #13 — canopy quality: per-limb continuous taper, arcing limbs, weeping
  gum tips with hanging streamers (merged as 2d98b17).

## Durable notes for next rotation (thermal rule)
- trees.rs: `trace_arc` is the shared limb-path tracer (fixed per-limb curl +
  weeping droop); `grow_limb` tapers r0→r1 and returns the tip HEADING so
  children continue curves. Termination: depth ≥ 3 or radius ≤ 1.4.
- foliage.rs: fade state lives in `FoliageLodFade` (attached lazily, NOT part
  of the streaming spawn contract). Two-phase fade keeps depth written; the
  FoliagePlugin system order (update_foliage_lod → reveal_built_chunks) is
  load-bearing — see the plugin comment.
- Follow-up ideas: skinned-foliage Mask-cutoff dissolve as a richer fade for
  textured leaves; arc/droop for silhouette stand-in trees (silhouettes.rs —
  core, needs routing) so far crowns match the new near shapes.
