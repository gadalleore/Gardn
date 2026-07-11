# Track: sprites

**Owns:** assets/ (+ tiny loader tweaks)
**Scope:** PNG/audio assets, cutouts

Seen broadcast #4. Session wound down 2026-07-10 per thermal rotation;
branch confirmed content-identical to main (b27f61f merged).

## Status
Idle — awaiting next rotation (likely owner's creative direction on new art
after the combined art+sky eyeball).

## Currently touching
- files: (none)

## Notes for other tracks
- foliage-life: doc comment on `create_extruded_leaf_mesh` (leaves.rs) still
  says the contour comes from `assets/leaf.png`; the code embeds
  `assets/Enhanced Leaf.png`. Stale comment, your file — flagging, not fixing.

## Needs / requests (flag the human)
-

## Done / merged
- PR #12 (merged b27f61f): grass cutouts + foliage skin redrawn at 32×32,
  silhouette-first; dead leaf intermediates removed; audio audited, no changes.
  Durable knowledge for future sprite work:
  - Grass loader (`grass_lego_mesh`) reads ONLY alpha (>128 → voxel), tints via
    vertex colors; RGB in the PNGs is cosmetic. Sprites >48px get nearest-
    downsampled to 32×32 — author grass cutouts at native 32×32, keep every
    blade connected (disconnected pixels extrude to floating boxes), center
    content horizontally (canvas center = yaw/sway pivot), touch the bottom row.
  - `foliage.png`: tiles once per foliage voxel (nearest+repeat sampler),
    multiplied by species tint, alpha is a hard Mask(0.5) cutout — so: small
    seamless grayscale tile, binary alpha, ~65% coverage reads well.
  - `mitchell_top.png` stacks at 0.55×GRASS_HEIGHT (0.6 height) over the
    blades — keep its stems bottom-anchored so they read as continuations.
  - Audio: wind.wav loop endpoints are zero-crossing (keep it that way if
    replaced); peaks across munch/wind are ~14–18%, mutually consistent —
    match that level for any new SFX. music/ rotation is drop-in (.mp3/.wav).
  - Sprite regen script (bezier-blade generator + alpha-map viewer) lives in
    this session's scratchpad only — trivial to rewrite; PS 5.1 gotcha: comma
    binds tighter than arithmetic inside @(), parenthesize every element.
