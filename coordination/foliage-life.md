# Track: foliage-life

**Owns:** src/grass.rs, src/leaves.rs
**Scope:** grass + collectible leaves; growth/death

## Status
Seen broadcast #4 (rebased onto 95f090a; run-lock honoured for the verify
run). **PR #10 open, awaiting review/merge**: grow-in on stream (staggered scale-in — shared
materials make per-entity alpha fade expensive, scale-in is cheaper anyway)
+ natural scatter (grass cluster families, leaf drifts) + the director-asked
per-spot ocean check from terrain's coastal find. I applied the same ocean
check to leaves too: they only skipped whole-Ocean chunks, so mixed coastal
chunks could hover leaves over the shallows — same bug, same one-liner.

## Currently touching
- files: src/grass.rs, src/leaves.rs, README.md (one bullet)

## Notes for other tracks
-

## Needs / requests (flag the human)
-

## Done / merged
-
