# Track: weather

**Owns:** src/weather.rs, src/sky.rs
**Scope:** wind + streamers, day/night sky, seasons + clouds (rotation 2)

## Status
Seen broadcast #7 (rotation one complete 🏆 — congrats all). Rotation 2 begun:
rebased onto main @ cb259ba clean.

**PR #17 open (seasons + cloud state machine, PR 1 of 2):** pure logic per the
director's assignment + the owner's design doc, all in weather.rs. `Season`
clock (Summer→Autumn→Winter→Spring, 3 game days each, pub(crate) as directed),
and a Bevy-free `CloudSim` publishing a `CloudState` resource
(cirrostratus/main/fog covers) that PR 2's renderer will read. 8 new unit
tests (21 total pass): exact modifier math, fixed-seed distributions for the
25×4 main events + cirrus escalation + fog seasonality + outro coin-flip, and
a full-cycle invariant test (herald never absent under a main layer, phase
order is the spec's procession). Env knobs: GARDN_SEASON, GARDN_CLOUDS,
GARDN_FOG. No `cargo run` needed this PR (no visuals yet — nothing to
eyeball, run-lock untouched).

**Spec interpretations to flag for the owner (dissent-in-writing rule):**
- Cirrus escalation odds 10/45/50 sum to 105, so they're treated as weights
  (≈9.5% / 42.9% / 47.6% — keeps the owner's ratios exactly).
- Spring morning fog is unspecified (70% fall/winter, 20% summer) → set 40%.
- "Wind is blowing" → strength ≥ 2.0 of 5; the 70% *replaces* the base
  formation chance, and winter/arid/coastal multipliers still apply on top.
- Regional modifiers key off `australia::biome_at_world` (already pub — no
  core change needed): arid = AridOutback+Pilbara, coastal = CoastalBush+
  Mediterranean+Tasmania. Savanna/TemperateForest neutral.

## Currently touching
- files: src/weather.rs (seasons + cloud machine), src/sky.rs (SkyClock),
  README.md (my bullets only)

## Notes for other tracks
- `Wind` unchanged again — same fields, same meaning.
- New pub(crate) surfaces (inside my own two files): `weather::SeasonClock`
  (`.season`), `weather::CloudState` (herald/main/fog covers 0..1),
  `sky::SkyClock` (`frac` 0..1 of the day, `day` count). Grass browning /
  gameplay can read `SeasonClock` later if wanted.

## Needs / requests (flag the human)
- docs/module-contracts.md (routed, not edited by me): sky.rs is no longer
  "fully self-contained" — suggested line: "`sky.rs`: `SkyPlugin`, plus
  `SkyClock` (resource: day fraction + day count) read by weather.rs."
  And weather.rs's line gains: "`SeasonClock`, `CloudState` (published for
  future consumers; only weather reads them today)."

## Done / merged
- Rotation 1: PR #11 (violet twilight, wheeling stars, breathing wind) — merged.

## Next (PR 2 of 2)
Procedural cloud rendering reading `CloudState`: soft voxel/blob clumps at
altitude, recognizable silhouettes per type (wispy cirrus vs anvil
cumulonimbus vs fluffy cumulus), cirrostratus as a high thin veil above,
minimal rain/lightning visuals for nimbostratus/cumulonimbus, morning fog via
the distance-fog hooks in sky.rs. Will need the run-lock for eyeballing.
