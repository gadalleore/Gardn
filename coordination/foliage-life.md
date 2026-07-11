# Track: foliage-life

**Owns:** src/grass.rs, src/leaves.rs
**Scope:** grass + collectible leaves; growth/death

## Status
Seen broadcast #6. Session wound down 2026-07-10 per thermal rotation —
PR #10 merged (6681f23), branch confirmed content-identical to main
(b27f61f). Nothing in progress.

**Next shift, start here:**
1. Sprites flagged (their coordination file, "Notes for other tracks"): the
   doc comment on `create_extruded_leaf_mesh` in leaves.rs still says the
   contour comes from `assets/leaf.png`, but the code embeds
   `assets/Enhanced Leaf.png`. One-line comment fix, our file — queued
   rather than opening a PR during wind-down. Fold into the next real PR.
2. Sprites' PR #12 redrew all four grass PNGs (mitchell, mitchell_top,
   kangaroo, button) at native 32×32 — grass_lego_mesh's >48px downsample
   branch is now dead in practice (harmless, but worth an eyeball in-game:
   clump silhouettes changed under our extrusion).
3. Scope reminder: "growth/death" lifecycle is still untouched — no design
   yet; candidate for a future assignment.

## Currently touching
- files: (none)

## Notes for other tracks
- sprites: your leaves.rs doc-comment flag is queued for our next shift —
  thanks for routing instead of touching.

## Needs / requests (flag the human)
-

## Done / merged
- **PR #10** (merged 6681f23, 2026-07-10): grass/leaf pop-in fixed with
  staggered scale-in grow animations (scale-in over alpha fade — shared
  materials would need per-entity clones); grass scatters as cluster
  families + loners, leaves settle in drifts; per-spot ocean check in both
  scatters (director ask from terrain's coastal find; extended to leaves on
  own judgment — same bug). Verified: cargo check/test clean, 35s coastal
  run under run-lock, no panics.
