# Track: trees

**Owns:** src/foliage.rs, src/trees.rs
**Scope:** tree generation, canopy LOD + sway

## Status
- seen broadcast #3 (branch merged with main @ 34881f7)
- WIP 1: canopy quality pass in `trees.rs` — per-limb continuous taper (child limbs
  pick up at the parent's end radius, so wood never thickens outward), gently
  arcing limbs instead of straight tubes, drooping terminal twigs with hanging
  leaf streamers (weeping-gum habit). Low base forks and modest crown reach kept.
- WIP 2: LOD cross-fade in `foliage.rs` — rung swaps currently pop; incoming rung
  will alpha-fade in over the still-opaque outgoing rung (outgoing keeps writing
  depth, so the distance-blur pass never sees sky through the canopy mid-fade).

## Currently touching
- files: src/trees.rs, src/foliage.rs, coordination/trees.md

## Notes for other tracks
- The LOD cross-fade stays entirely inside foliage.rs: per-rung materials are
  cloned lazily at first fade, so `streaming.rs`'s spawn code and the
  `FoliageLodGroup { center, radius, level }` construction are untouched — no
  contract change.

## Needs / requests (flag the human)
-

## Done / merged
-
