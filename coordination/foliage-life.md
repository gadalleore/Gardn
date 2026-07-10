# Track: foliage-life

**Owns:** src/grass.rs, src/leaves.rs
**Scope:** grass + collectible leaves; growth/death

## Status
Seen broadcast #3 (rebased onto 34881f7). Working on: reduce grass/leaf pop-in
as chunks stream (scale-in grow animation — shared materials make alpha fade
per-entity expensive, and terrain's no-alpha-fade rule doesn't bind us but
scale-in is cheaper anyway) + natural scatter distribution (grass cluster
families, leaf drifts).

## Currently touching
- files: src/grass.rs, src/leaves.rs

## Notes for other tracks
-

## Needs / requests (flag the human)
-

## Done / merged
-
