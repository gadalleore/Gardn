# Track: trees

**Owns:** src/foliage.rs, src/trees.rs
**Scope:** tree generation, canopy LOD + sway

## Status
- seen broadcast #7 — rotation one complete; this session is wound down.
- Branch `trees` reset to main @ cb259ba and pushed; merged work branch
  `trees-lod-fade` deleted local + remote. Working tree clean, run-lock free.

## Currently touching
- files: none

## Notes for other tracks
- The LOD cross-fade lives entirely inside foliage.rs: per-rung materials are
  cloned lazily at first fade, so `streaming.rs`'s spawn code and the
  `FoliageLodGroup { center, radius, level }` construction are untouched — no
  contract change.

## Needs / requests (flag the human)
-

## Done / merged
- PR #13 — canopy quality: per-limb continuous taper, arcing limbs, weeping
  gum tips with hanging streamers (merged as 2d98b17).
- PR #14 — LOD cross-fade (two-phase, depth-writing throughout) + bare-trunk
  flash fix at tree reveal (chained rung selection before reveal) (merged as
  cb259ba).

## Durable notes for next rotation (thermal rule)
- trees.rs: `trace_arc` is the shared limb-path tracer (fixed per-limb curl +
  weeping droop); `grow_limb` tapers r0→r1 and returns the tip HEADING so
  children continue curves. Termination: depth ≥ 3 or radius ≤ 1.4.
- foliage.rs: fade state lives in `FoliageLodFade` (attached lazily, NOT part
  of the streaming spawn contract). Two-phase fade keeps depth written (the
  distance-blur pass reads depth — hard constraint); the FoliagePlugin system
  order (update_foliage_lod → reveal_built_chunks) is load-bearing — see the
  plugin comment.
- Run-lock history: two pre-rule-8 unlocked runs (disclosed, no collision);
  everything after took `../_runlock` properly.
- Follow-up ideas: skinned-foliage Mask-cutoff dissolve as a richer fade for
  textured leaves; arc/droop for silhouette stand-in trees (silhouettes.rs —
  core, needs routing) so far crowns match the new near shapes; consider
  making species tables' `branch_len_in` honest (paths span ~55% of nominal
  inches — pre-existing step-length quirk, kept to avoid retuning all species).
