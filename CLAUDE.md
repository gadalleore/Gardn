# Gardn — agent guide (read this first)

A worm-to-apex evolution survival game on a Bevy voxel Australia. You (a Claude
agent) are one of several working **in parallel on separate branches**. This
file is loaded every session so you land on the same page as the others.

## Parallel-work rules (important)

1. **Work on your track's branch**, not `main`: `git checkout -b <track>` (or
   pull your existing branch). Never commit straight to `main`.
2. **Only edit the files your track owns** (see map below). If you need a change
   in someone else's file or a shared contract, DON'T just do it — note it in
   your coordination file and flag it for the human to route.
3. **At session start:** `git pull` on `main` into your branch if you can, then
   read `docs/module-contracts.md` and skim `coordination/*.md` to see what the
   other tracks are touching.
4. **Keep your status current:** edit **only** `coordination/<your-track>.md` —
   what you're doing, which files you're touching, anything others should know.
   Never edit another track's coordination file. (Different files = no merge
   conflicts. One shared file would recreate the very monolith we split up.)
5. **Reality check:** every agent is a separate clone. You see other tracks'
   work only as of your last pull / once it's merged — this is not live chat.
   Small PRs + frequent `main` pulls + the human as merge hub is the real
   coordination channel.

## Track → files map

| Track | Owns |
|---|---|
| **terrain** | `src/terrain.rs` (voxel gen, meshing, `TerrainPlugin`), `src/topography.rs` (heightfield, caves) |
| **weather** | `src/weather.rs` (wind + streamers), `src/sky.rs` (day/night) |
| **trees** | `src/foliage.rs` (tree ECS/LOD/sway), `src/trees.rs` (tree generation) |
| **foliage-life** | `src/grass.rs`, `src/leaves.rs` |
| **sprites** | `assets/` (PNGs, audio), + tiny loader tweaks |

Shared/core (change only via the human): `src/main.rs` (plugin list),
`src/streaming.rs` (chunk pipeline + `ChunkWorld`), `src/world.rs` (constants),
`src/chunk_store.rs`, `src/silhouettes.rs`, `src/distance_blur.rs`, `src/audio.rs`,
`src/worm.rs`, `src/australia.rs`, `src/map_ui.rs`.

## Architecture in one breath

`main.rs` is thin (~190 lines): it builds the app and `.add_plugins(...)` one
per subsystem. Each subsystem is a file exposing a Bevy `Plugin` that wires its
own systems and owns its asset setup, so tracks don't collide in the schedule.
The exception is `streaming.rs`: its systems run in main's ordered `.chain()`
world pipeline (mixed with silhouette + worm-eating steps that must stay
ordered), so they're `pub(crate)` and registered in `main.rs`, not a plugin.

Cross-module references go through `crate::<module>::Item` (e.g.
`crate::weather::Wind`, `crate::streaming::ChunkWorld`). The exact boundaries
are in `docs/module-contracts.md` — **don't break those signatures** without
routing it through the human.

## Build & test

- `cargo check` — fast type-check; run constantly. First thing after any change.
- `cargo test` — unit tests for pure logic (mesh/gen/math). `terrain.rs` and
  `silhouettes.rs` have examples; add tests for your pure functions.
- `cargo run` — the real proof: launch the full game and eyeball your subsystem.
  Env knobs: `GARDN_HOUR=0` (night), `GARDN_HIGH=300` (start airborne).

A pure code-move or self-contained change that `cargo check`s clean is almost
certainly behaviour-identical — but anything with runtime effect, run it.

## Documentation — every PR updates the docs it touches

Docs ship **in the same PR** as the change, never "later":

1. **`README.md`** — if your change alters gameplay, controls, world features,
   build/run steps, or architecture, update the matching README section.
   README is the *one* shared file every track may edit, under strict
   discipline: touch **only the lines describing your own change** (usually one
   bullet or table row), keep the diff minimal, and expect to rebase it. Don't
   restructure the README from a track branch — route that through the human.
2. **`coordination/<your-track>.md`** — always current (rule 4 above).
3. **`docs/module-contracts.md`** — only when a cross-module signature genuinely
   changed, and only via the human (rule 2 above).

If a PR needs no README change (pure refactor, internal fix), say so in the PR
description — "No player-visible change; README untouched" — so the human
doesn't have to check.

## House style

Match the surrounding code: dense, specific comments that explain *why*
(there's a lot of hard-won rendering/streaming rationale in the comments — read
them before changing that logic). Keep new cross-module items `pub(crate)`, not
`pub`, unless truly needed.
