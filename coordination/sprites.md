# Track: sprites

**Owns:** assets/ (+ tiny loader tweaks)
**Scope:** PNG/audio assets, cutouts

Seen broadcast #4 (rebased onto 95f090a). Note: my 30s verify-run of the game
happened just before I read #4's run-lock rule; future runs will take
`../_runlock` first.

## Status
First assignment done — PR #12 open (grass cutouts + foliage skin redrawn,
dead leaf intermediates removed, audio audited-no-changes). Awaiting merge;
per thermal rotation this session winds down after that.

Audit findings driving the work:
- `grass/mitchell.png` (32×32): noisy — disconnected specks extrude to floating
  boxes; top 5 rows empty.
- `grass/button.png` (32×32): only ~2 blades, all in the left half of the
  canvas → clump is half-width and off-centre of its yaw/sway pivot.
- `grass/mitchell_top.png` is 1032×1032; the loader nearest-downsamples to
  32×32 which shreds the thin strands into noise. Redrawing at native 32×32.
- `foliage.png` (1032×1032) is binary white/transparent maze, but foliage-block
  UVs tile once per voxel (nearest+repeat) and the material multiplies a
  species tint — so a small grayscale leaf-cluster tile is the intended use of
  that pipeline and currently wasted. Repainting at 32×32.
- Audio: munch.wav (0.9s) + wind.wav (17s loop, clean zero endpoints) are fine;
  peaks are low (~14–18%) but mutually consistent. music/ has one track
  (gardnr.mp3); rotation code supports more — drop-in additions welcome.

## Currently touching
- files: assets/grass/*.png, assets/foliage.png, coordination/sprites.md

## Notes for other tracks
- foliage-life: doc comment on `create_extruded_leaf_mesh` (leaves.rs) still
  says the contour comes from `assets/leaf.png`; the code embeds
  `assets/Enhanced Leaf.png`. Stale comment, your file — flagging, not fixing.

## Needs / requests (flag the human)
-

## Done / merged
-
