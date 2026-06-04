# Gardn

A whimsical Bevy garden game with 3D leaves extruded directly from 2D pixel art.

## Current Features

- **PNG-Driven 3D Leaves** (the star of the show):
  - `assets/leaf.png` is the single source of truth.
  - At runtime it generates:
    - The `StandardMaterial` base color texture.
    - A true low-poly 3D mesh whose silhouette exactly follows the PNG's green contours (jagged 8-bit steps).
  - Thin "coffee-coaster" extrusion with chunky, axis-aligned retro side walls.
  - Clean "just the green" edges — no black borders, sampling lines, or visible rims (thanks to green-dominant pixel filtering, aggressive UV insetting on the perimeter, `ImagePlugin::default_nearest()`, and slight geometry insets on the end bars).
  - Internal 8-bit art renders undistorted on the faces.
- 7 independently animated floating leaves (random bob + spin + base tilt speeds).
- Worm-cam fly controls (WASD + mouse look via `bevy_flycam`).
- Eat leaves with `E` when close enough.
- Directional sun light + real-time shadows.
- Simple grass + rock placeholder scene.

## Running the Game

```bash
cargo run
```

Built with:
- Bevy 0.15
- `bevy_flycam`
- `image` crate (for PNG contour extraction at mesh creation time)

## Controls

- **Fly**: WASD + Mouse (look around, move)
- **Eat**: `E` (when near a leaf)
- Close the window to exit.

## Development Notes

Everything leaf-related lives in `src/main.rs` in `create_extruded_leaf_mesh` and `spawn_textured_leaves`.

Key techniques used:
- Per-row min/max green pixels (ignoring black outline/details) → closed outline polygon.
- Rectification to pure H/V segments for retro jagged sides.
- Local strip triangulation between left/right chains (with index remapping) for good texture fidelity.
- UV insetting toward content center (extra on top/bottom bars) + nearest sampling so the geometric edge always samples interior green.
- Full uniform thickness with extra geom pull + zero? No — full thickness on wide ends after iteration.
- One shared mesh handle + cloned material for efficiency.

Changing `assets/leaf.png` and rebuilding updates both the look *and* the 3D shape.

## Future Plans (as discussed)

- Procedural generated environment
- Enhanced eating mechanics (animations, effects, progression?)

## Assets

Primary art asset: `assets/leaf.png` (high-res 8-bit style leaf with stem)

Other images in `assets/` are earlier experiments/cleanups and can be ignored.

---

Made with lots of iteration and a healthy dose of "let's see if we can do this entirely in code." 🌿

Thanks for the fun project!