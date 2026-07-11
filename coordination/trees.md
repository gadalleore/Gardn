# Track: trees

**Owns:** src/foliage.rs, src/trees.rs
**Scope:** tree generation, canopy LOD + sway

## Status
- seen broadcast #5 (rebased onto main @ 6681f23)
- **PR #13 open** — canopy quality: per-limb continuous taper, arcing limbs,
  weeping gum tips with hanging streamers. Shape rules honoured (low forks,
  monotone taper, modest crown). Tests + budget guards green; verified in-game.
- **Run-lock mea culpa:** my two verification runs for PR #13 happened before I
  pulled/read broadcast #4's run-lock rule — they ran unlocked. No collision
  occurred (checked: no other game was up), but flagging it for the record.
  All future `cargo run`s will take `../_runlock` first.
- WIP 2 (next): LOD cross-fade in foliage.rs, on stacked branch `trees-lod-fade`.

## Director ask (2026-07-10, trunk-before-canopy at streaming edge) — diagnosis
Root cause found, and it IS in my files: `FoliagePlugin` registers its systems
as an **unordered** tuple, so on the frame a chunk's last tree finishes,
`reveal_built_chunks` can flip tree roots visible before `update_foliage_lod`
has ever selected a foliage rung for that just-spawned tree — bark is visible
by default, every leaf rung still spawns `Visibility::Hidden` → bare trunk.
Nominally one frame, but streaming-edge frames hitch (mesh uploads), so it can
hold for a perceptible beat. Fix ships in the cross-fade PR: chain
`update_foliage_lod` before `reveal_built_chunks` so a root is only ever
revealed after its correct rung is live (covers both possible orderings vs the
main chain). Not a separate/out-of-scope code path — same PR.

## Currently touching
- files: src/foliage.rs (branch `trees-lod-fade`), coordination/trees.md

## Notes for other tracks
- The LOD cross-fade stays entirely inside foliage.rs: per-rung materials are
  cloned lazily at first fade, so `streaming.rs`'s spawn code and the
  `FoliageLodGroup { center, radius, level }` construction are untouched — no
  contract change.

## Needs / requests (flag the human)
- PR #13 ready for review (canopy quality, trees.rs only + this file).

## Done / merged
-
