# Track: weather

**Owns:** src/weather.rs, src/sky.rs
**Scope:** wind + streamers, day/night sky

## Status
Seen broadcast #4 (run-lock + thermal rotation understood; congrats terrain on
PR #7). **PR #11 open** (day/night polish + organic gusts), rebased on main @
95f090a, awaiting review. Per thermal rotation I'm ready to wind down once it
merges. Run-lock note: one `cargo run` verification pass started minutes
before I read rule 8 — acquired `../_runlock` mid-run the moment I saw it,
released after; the later dawn run took the lock first, properly.

Assignment recap (durable memory for next rotation): sky.rs now grades
blue→gold→orange→violet→moonlit night, sun disc blushes at the horizon,
220-star dome wheels after dark, `GARDN_DAY_SECS` env knob compresses the
cycle for testing. weather.rs layers gust/lull events + flutter (private
`GustTexture` resource) on the rolled base level and wobbles `dir` a few
degrees in strong wind; streamers surge live with gusts. `Wind`'s shape
untouched. Possible next steps: rain/clouds, wind audio pitch tracking
strength, aurora on rare nights.

## Currently touching
- files: none (PR #11 in review)

## Notes for other tracks
- `Wind`'s shape is unchanged — same fields, same meaning. `strength` now
  breathes (gusts/lulls around the rolled level) and `dir` wobbles a few
  degrees at high wind; both were already read per-frame by grass/leaves/
  foliage/worm, so sway just inherits the texture for free.
- foliage-life: grass.rs fakes its own gust with sines (line ~216) because
  strength used to be flat — once this merges you could drop that and read the
  real thing. Your call, no action needed.

## Needs / requests (flag the human)
-

## Done / merged
-
