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
- **Real geology underfoot.** Dirt runs about a tree length deep on average —
  but it's a distribution: ~1% of ground has bedrock right below the surface,
  ~1% has dirt two tree lengths down, varying smoothly so digging feels
  geological. Beneath the dirt: a rock layer worms can't eat, floored by
  unbreakable bedrock. Giant procedural boulders — blocky, craggy voxel
  clumps, coarse from afar and detailed up close — surface as landmark rocks
  (smaller ones lurk buried in the dirt), and the deep rock is threaded with
  solid dirt tunnels — worm highways you discover by digging and follow by
  chewing.
- **Titan trees.** Procedurally grown, species-accurate giants — a mountain ash
  tops **1,000 ft** (~4,000 worm-lengths) — planted sparsely so each stands as a
  lone landmark you crawl between. Distant ones render as coarse voxel LODs that
  cross-fade seamlessly into the real thing as you approach.
- **Extruded pixel-art life.** Grass clumps and collectible leaves are painted as
  small sprites and pixel-extruded into solid 3D — a file-drop art pipeline where
  editing a PNG changes the model. Grass grows in natural cluster families and
  leaves settle in drifts; both sprout/swell in as chunks stream instead of
  popping, and neither spawns in coastal shallows.
- **Wind & gravity.** Trees and grass sway on a shared wind that gusts, lulls,
  and slowly wanders the compass; grass bends away from the passing worm.
- **A day/night sky.** A 24-hour clock walks the sun and full moon overhead —
  blue days grade through gold, orange, and violet twilight into a moonlit
  night under a wheeling starfield.
- **Seasons & local weather.** An Australian season clock (day length never
  changes — only the weather does) feeds a cloud "front director": sunny →
  cirrostratus herald → one main cloud type → altostratus/stratus outro →
  clear, with winter/arid/coast/wind modifiers and a morning-fog roll. The sky
  is *local*, not a global switch: discrete **blocky voxel clouds** drift in on
  the wind from a distance, clump together, and blow past — each a voxel clump
  shaped to its type (flat treetop loaves, fluffy cumulus, towering
  cumulonimbus giants, the high thin cirrus family under the topmost
  cirrostratus veil) with a near/far LOD like the leaves. White until the sky
  is more than half covered, then greying toward overcast. (Cloud shadows and
  rain-under-clouds land in follow-up PRs.)
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

## Developing

```bash
cargo check   # fast type-check — run constantly
cargo test    # unit tests for pure logic (meshing, generation, math)
cargo run     # the real proof: launch and eyeball your change
```

Handy env knobs for testing:

| Variable | Effect |
| --- | --- |
| `GARDN_HOUR=0` | Start at night (any 0–23 hour works) |
| `GARDN_DAY_SECS=120` | Compress the whole 24-h cycle into N real seconds — watch a full sunrise-to-starfield sweep in minutes |
| `GARDN_HIGH=300` | Start 300 ft airborne — great for checking LODs and streaming |
| `GARDN_SEASON=winter` | Pin the season (`summer`/`autumn`/`winter`/`spring`) |
| `GARDN_CLOUDS=cumulonimbus` | Force an immediate cloud front of that type |
| `GARDN_FOG=1` | Every morning's fog roll comes up foggy |

## How the code is organized

`src/main.rs` is deliberately thin (~190 lines): it builds the Bevy app and adds
one `Plugin` per subsystem. Each subsystem lives in its own file and wires its
own systems, so parallel work doesn't collide in the schedule:

| Module | What it owns |
| --- | --- |
| `terrain.rs` / `topography.rs` | Voxel generation, meshing, heightfield, caves |
| `streaming.rs` / `chunk_store.rs` | Chunk load/unload pipeline and persistence (core — runs in `main.rs`'s ordered chain) |
| `trees.rs` / `foliage.rs` | Tree generation, LODs, wind sway |
| `grass.rs` / `leaves.rs` | Extruded pixel-art grass and collectible leaves |
| `weather.rs` / `sky.rs` | Wind gusts, seasons + local blocky voxel clouds, day/night cycle |
| `worm.rs` | The player: crawling, eating, burrowing |
| `silhouettes.rs` / `distance_blur.rs` | Far-tree stand-ins and the custom background blur pass |
| `world.rs` / `australia.rs` | Scale constants, geography, biome map |
| `audio.rs` / `map_ui.rs` | Soundtrack shuffle, the (useless) map |

The exact cross-module API — who may call what — is written down in
[`docs/module-contracts.md`](docs/module-contracts.md). Treat it as a treaty:
don't change a signature listed there without coordinating.

## Parallel AI development

This repo is built by several Claude agents working **simultaneously on
separate tracks** (terrain, weather, trees, foliage-life, sprites), each in its
own clone and branch, with a human as the merge hub. The machinery:

- [`CLAUDE.md`](CLAUDE.md) — the rules every agent loads at session start.
- [`docs/module-contracts.md`](docs/module-contracts.md) — the API boundaries between tracks.
- [`coordination/`](coordination/) — one status file per track; how agents leave notes for each other.

**This README is a living document.** Every PR that changes gameplay, controls,
world features, build steps, or architecture must update the matching section
here, in the same PR — see the documentation rule in `CLAUDE.md`.

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
