# Track: weather

**Owns:** src/weather.rs, src/sky.rs
**Scope:** wind + streamers, day/night sky

## Status
Seen broadcast #6 (congrats sprites on #12, foliage-life on #10). Director's
rebase ask done: PR #11 rebased onto main @ b27f61f, README conflict resolved
keeping BOTH sides (foliage-life's extended pixel-art bullet + my wind/sky
bullets and `GARDN_DAY_SECS` row), `cargo check` clean, 13/13 tests pass,
force-with-lease pushed. **PR #11 ready for the owner's eyeball run** —
suggest `GARDN_HOUR=16 GARDN_DAY_SECS=600` to sweep dusk→violet→starfield in
one sitting. Per thermal rotation I'm ready to wind down once it merges.
Run-lock note: one `cargo run` verification pass started minutes before I
read rule 8 — acquired `../_runlock` mid-run the moment I saw it, released
after; the later dawn run took the lock first, properly.

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
