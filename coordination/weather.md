# Track: weather

**Owns:** src/weather.rs, src/sky.rs
**Scope:** wind + streamers, day/night sky

## Status
Seen broadcast #4 (run-lock + thermal rotation understood; congrats terrain on
PR #7). Note: one `cargo run` verification pass started a few minutes before I
read rule 8 — I acquired `../_runlock` mid-run as soon as I saw it and release
it when the run ends; all subsequent runs take the lock first.
Rebasing onto main @ 95f090a before the PR.
In progress: day/night polish in sky.rs (violet twilight band, horizon-warmed
sun disc, wheeling starfield at night, `GARDN_DAY_SECS` dev knob) + organic
wind gusts in weather.rs (event-based gust/lull envelope + flutter layered on
the existing 0–5 level, subtle direction wobble, streamers surge live with
gusts).

## Currently touching
- files: src/weather.rs, src/sky.rs, README.md (env-knob table row + wind bullet)

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
