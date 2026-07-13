# Track: weather

**Owns:** src/weather.rs, src/sky.rs
**Scope:** wind + streamers, day/night sky, seasons + LOCAL blocky clouds

## Status
Seen broadcast #8 (main @ ad03e39; terrain #24 wind-down merged). Re-read the
weather inbox in full including all rotation-2 re-scope + refinement + altitude
notes (2026-07-12 corrected). **Rebuilt the cloud system local + blocky per the
owner's sharpened direction.**

**PR #25 open (local blocky clouds v1) — branch `weather-local-clouds` off
main.** Retires #17/#19's global-scalar + icosphere model (leave those open as
reference per director).

**Review fix applied (2026-07-12):** reviewer flagged the high clouds (esp.
cumulonimbus) sitting in the treetops. Root cause: trees here are TITANS (a
mountain ash tops ~1000 ft) and my altitudes (140–740 ft) put clouds among the
trunks. Fix: raised the whole layering above the canopy within the 1600 ft far
plane — treetop loaves ~320–440, mid ~600–740, cirrus family ~880–940,
cumulonimbus anvil ~1150, cirrostratus veil ~1250. Also set the cloud material
`fog_enabled: false` so the high layers (now inside the 650–1350 ft distance
fog) aren't washed out to sky; the distance-blur pass still softens far ones.
30 tests pass; eyeballed airborne — clouds now sit up in the sky above the
treetop line, not among the trunks.

What's here:
- **Seasons** — kept verbatim from #17 (owner: pure win). `SeasonClock`.
- **The procession brain** — kept verbatim (phases, exact odds, winter/arid/
  coastal/wind modifiers, morning fog). Repurposed: its `front_order()`
  (herald fullness, main type, main fullness) is now read as *population
  targets* for local clouds, not a global cover scalar. `CloudDirector`.
- **Local blocky voxel clouds** — discrete world-positioned entities in a field
  around the worm. A shared mesh library baked at startup: 10 forms × 3 shape
  variants × 3 LOD block sizes (culled-cube meshes, downsampled coarser with
  distance — the leaves' near=detail/far=coarse ladder). Clouds drift on the
  live wind, clump onto approaching neighbours, grow-in/retire by fade, and
  age off the far side.
- **Owner refinements folded in:** one shared grid orientation aligned to wind
  (no per-cloud random yaw; cirrus lines run parallel across the sky); drift IN
  from the upwind edge (nothing pops overhead); white by default, greying only
  once cover > 50% (`tint_clouds`, one global tint — replaces per-type gray);
  altitude layering (flat treetop LOAVES ~140–210 ft → cumulus/mid → high
  cirrus family → cumulonimbus giants tower highest of the mains ~690 ft →
  cirrostratus veil caps ABOVE all at ~740 ft); cumulonimbus is the biggest
  form (tall tower + anvil).
- **Morning fog** — still applied by sky.rs (single fog writer), now off the
  tiny `WeatherSky { fog }` resource instead of the retired global CloudState.
- 27 unit tests pass (8 brain/season + 3 voxel-gen: bounded non-solid clumps,
  coarser-LOD-sheds-geometry, culled-mesher-skips-interior). `cargo check`
  clean (only other tracks' pre-existing warnings).
- Eyeballed under the run-lock across several runs (blocky white clouds render
  and DRIFT IN over time; bigger CB grid runs with no hitch). Note: one run
  stole a 2h-stale `director` lock (no live process) per rule 8.

## Deferred to follow-up PRs (flagging, not dropping)
- **Real cloud shadows** (owner #4 of rework): a soft dark ground projection
  under each cloud tracking it. Cloud `radius`/`height` are already baked for
  it. Next PR.
- **Local rain + lightning** under rain-bearing clouds (emitters parented to
  the cloud, moving with it). Next PR. (No thunder.wav in assets/ — will flag
  sprites when we get there.)
- **True merge-on-contact** (owner: two clumps drifting close merge into one
  larger cloud). Clumping (spawn-bias) is in; entity-level merge is a meaty
  feature — deferred, noted.
- **Wind blows inland ~75%** (owner): bias the heading `update_wind` wanders
  toward, using a coast direction sampled from `australia::biome_at_world`.
  Self-contained wind feature, cleanly separable → its own small PR.

## Spec interpretations (flag the human)
- Cirrus escalation odds 10/45/50 sum to 105 → treated as weights (exact
  ratios preserved). Spring fog unspecified → 40%. "Wind blowing" ≥ 2/5.
- Regional arid = AridOutback/Pilbara; coastal = CoastalBush/Mediterranean/
  Tasmania (from `biome_at_world`, already pub — no core change needed).
- Cover→grey metric: main fullness × per-type `sky_weight` (a stratus deck ≈
  overcast, a few cumulus barely count), grey ramps once that passes 0.5.

## Needs / requests (route via human)
- **CORE CHANGE NEEDED — raise the camera far plane (blocker for round-2
  clouds).** Owner's round-2 direction wants clouds ENORMOUS, HIGH (every high
  type's base clearly above the 1000 ft treeline), and MANY across the sky.
  That blows past the 1600 ft far plane in `worm.rs`
  (`Projection { far: 1600.0 }`, core — I did NOT touch it). With enormous high
  clouds spread over a ~1600 ft spawn radius up to ~2750 ft altitude, the
  farthest cloud sits ~3500 ft out, so **requesting `far: 5000.0`**.
  - Tradeoffs for the director to weigh: (1) depth precision — far/near grows
    (near is ~0.1), so distant terrain z-fighting is possible; bumping the near
    plane a touch (e.g. 0.3) would help if it doesn't clip the worm's nose.
    (2) Fog/silhouettes are unaffected in practice — terrain silhouettes only
    reach ~770 ft and still fog out ~1350 ft; the extra range is sky-only.
    (3) Clouds are already `fog_enabled: false`, so they won't fog at range;
    the distance-blur pass still softens them.
  - Alternative if a 5000 ft world far plane is undesirable: a separate sky/
    cloud render pass not bound by the world far plane — heavier to build; I'd
    prefer the far-plane bump. Your call.
  - **Until this lands, the high/enormous clouds clip at 1600 ft in-game**, so
    the round-2 push is sized for the target and eyeball-verified only for the
    overhead band; full-sky verification waits on the far-plane change.
- docs/module-contracts.md update (I did NOT edit it — routing the wording):
  - `sky.rs`: no longer "fully self-contained" — now `SkyPlugin` + `SkyClock`
    (resource: day fraction + day count, read by weather). Reads
    `crate::weather::WeatherSky` (morning-fog level) and applies it.
  - `weather.rs`: adds `SeasonClock`, `CloudDirector`, `WeatherSky`
    (pub(crate); only weather/sky use them today). `Wind` shape UNCHANGED.

## Currently touching
- files: src/weather.rs (seasons + brain + local blocky clouds), src/sky.rs
  (SkyClock + morning-fog apply), README.md (my bullets/rows only).

## Done / merged
- Rotation 1: PR #11 (violet twilight, stars, breathing wind) — merged.
- Rotation 2 held/superseded: #17 (seasons+brain logic), #19 (icosphere
  renderer) — left open as reference; this rework supersedes them.
