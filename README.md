# Gardn

**A worm's-eye survival game set on a procedurally generated Australia — at a scale where a gum tree is a skyscraper.**

You begin as a worm on a vast, real-geography continent rendered in voxels. The
world is not scaled down for you: it is kept genuinely, absurdly enormous, and
*you* are three inches long. A tree looms like the Burj Khalifa. Grass arches
overhead like a jungle canopy. The ocean waits miles away down a sheer coast.
The scale is the point.

Built from scratch in **Rust + [Bevy](https://bevyengine.org/)**.

---

## The vision

Gardn is growing into a **worm-to-apex evolution survival game**:

1. **Start as a worm.** Survive the ground and the things that hunt it.
2. **Find a mate**, and protect them until they lay an **egg**.
3. **Choose an evolution** — a skill-tree branch that decides *what you become*.
4. After the larva grows, you *become* that creature, with new abilities and new
   predators to match.
5. Repeat, climbing the food chain — a bird the size of a dragon today is your
   body tomorrow.

The survival/evolution loop is the roadmap. What's playable today is the world it
happens in — the hard part: a believable, streamable, gargantuan planet.

## What's in the world right now

- **A whole procedural Australia.** Real (if stylised) coastline and geography,
  eight biomes — tropical savanna, the arid Red Centre, the Pilbara, the SW
  Mediterranean, temperate forest, coastal bush, Tasmania — each with its own
  landforms, tree species, and grasses. Every new game washes you up on a random
  green stretch of coast.
- **Voxel terrain** with regional ranges, worm-scale micro-relief, and a
  Minecraft-style cave web underground.
- **Titan trees.** Procedurally grown, species-accurate giants — a mountain ash
  tops **1,000 ft** (~4,000 worm-lengths) — planted sparsely so each stands as a
  lone landmark you crawl between. Distant ones render as coarse voxel LODs that
  cross-fade seamlessly into the real thing as you approach.
- **Extruded pixel-art life.** Grass clumps and collectible leaves are painted as
  small sprites and pixel-extruded into solid 3D — a file-drop art pipeline where
  editing a PNG changes the model.
- **Wind & gravity.** Trees and grass sway on a shared gust system; grass bends
  away from the passing worm.
- **A custom background-only distance blur** (`src/distance_blur.rs`): a
  from-scratch render pass that keeps the foreground razor sharp while the distant
  titans go dreamy — something a physical depth-of-field physically cannot do for
  a camera sitting on the ground. Depth-aware, so near objects cut cleanly through
  the soft background.
- **God mode** for flying out and taking in the scale.
- The **M-key map** is, at worm scale, gloriously useless. This is on purpose.

## Controls

| Key | Action |
| --- | --- |
| **WASD** | Crawl |
| **Space** | Stretch / reach (never a jump — legs are a future evolution) |
| **E** | Eat / burrow |
| **M** | Map (a joke at this scale) |
| **G** | God mode (flight) |
| Mouse | Look |

Close the window to exit.

## Running

```bash
cargo run
```

Release build (much smoother for the streaming world):

```bash
cargo run --release
```

## Built with

- **Rust** + **Bevy 0.15**
- `bevy_flycam` for the worm/fly camera
- `image` for the PNG → voxel art pipeline
- A hand-rolled voxel mesher, chunk streamer, LOD/silhouette system, and the
  custom distance-blur render node

## Art & audio pipeline

Cosmetics are file-drop driven. Drop a grayscale sprite into `assets/` (grass,
foliage skin, leaf) and the engine extrudes/tints it; drop tracks into
`assets/music/` for an auto-shuffled soundtrack. Missing files fall back cleanly.

---

*A deliberately over-scaled little world, made mostly in code, one worm at a time.* 🐛
