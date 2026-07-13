Worm Game – Project Roadmap
Version: 1.0
Date: July 12, 2026
Goal: Build a beautiful, fully deterministic world where player actions permanently alter reality, and emergent ecological systems (water, fire, growth, coasts) create rich, repeatable stories.


Core Philosophy
Everything is deterministic from world_seed + game_time + current state.
Terrain is the foundation. All other systems read from and write to terrain state.
Player actions are destructive — when you change something significant, the old procedural state is replaced. You cannot go back.
Water and Fire are the primary interfaces between Terrain, Weather, and Life.
Build in layers with open, clean hooks so higher systems can be added without rewriting lower ones.


High-Level Phases
Phase
Focus
Parallel Work?
Goal
0
Terrain Foundation
No
Solid, queryable, modifiable world base
1
Nature, Weather & Coast
Yes
Ecological flavor + visual richness
2
Water Simulation + Advanced Life
Partial
Flowing water, moss, deeper growth systems
3
AI + Gameplay
—
Creatures, player systems, interactions


Phase 0: Terrain Foundation (Volume 1)
Goal: Create the base world that everything else builds upon.

Key Deliverables

Hierarchical seed system rooted in real Australian topography
Elevation + multi-octave noise
Rock density + deterministic clumping near surface
Soil depth and material variation
Destructive leaf state mutation (player changes permanently replace procedural state)
Basic hooks for future systems:
Moisture / water content per location
Pooling duration (how long water has sat there)
Flammability / dryness level
Coastal zone detection (proximity to sea level + shoreline rules)
Height + topology (for water flow)

Dependencies: None
What it exposes upward: Clean state queries and mutation APIs for moisture, pooling, flammability, coast status, height, rock/soil.

Status: Spec complete (see Terrain_Volume_1_Specification.md)


Phase 1: Nature, Weather & Coast (Build in Parallel)
Once core Terrain is stable, these three tracks can be developed concurrently because they mostly consume terrain state and add visual/ecological flavor.
1A. Foliage / Natural Behavior (Early Life)
Basic grass, bushes, and simple vegetation
Growth rules driven by soil + moisture
Moss formation when water has coalesced for > 1 in-game day
Simple death/decay
Hooks needed from Terrain: Moisture level, pooling duration, soil depth, rock exposure
1B. Weather
Rain (adds moisture to terrain)
Lightning (can trigger fire when conditions are dry)
Basic fire propagation (affected by dryness + vegetation)
Deterministic weather patterns (same every run from seed + time)
Hooks needed from Terrain: Current moisture, flammability/dryness, height (for some effects)
1C. Beach / Rocky Coast System
Special shoreline generation rules
Sandy beaches vs rocky coast differentiation
Possible simple wave/erosion effects near shore
Different material and vegetation rules in coastal zones
Hooks needed from Terrain: Height relative to sea level, distance to coast, coastal zone flag

Recommended Approach for Phase 1:

Start with lightweight versions of all three.
Focus on visual beauty and ecological feel first.
Make sure each system writes back to Terrain state where appropriate (e.g., rain increases moisture, fire reduces vegetation and increases dryness).


Phase 2: Water Simulation + Advanced Life
Water System
Realistic water flow: water moves to lower blocks and coalesces
Absorption into soil
Persistent pooling state (feeds moss in Phase 1 and future systems)
Possible flooding and drying cycles
Depends on: Terrain height + moisture state from Phase 0/1
Advanced Life
More sophisticated vegetation growth, tree systems, and ecological succession
Reaction to fire and water events
Possible simple wildlife or bio interactions later
Depends on: Stable moisture, pooling, fire effects, and coastal rules from Phase 1

Note: Water and Advanced Life have some natural overlap and can inform each other.


Phase 3: AI + Gameplay
Creatures / AI that interact with the living world
Player mechanics and progression
Full gameplay loops built on top of the established systems


Interface & Hook Strategy (Critical)
To keep the project maintainable, every layer should define clear interfaces:
Terrain (Base Layer) Should Expose:
GetHeight(x, y)
GetSoilDepth(x, y)
GetRockDensity(x, y)
GetMoisture(x, y)
GetPoolingDuration(x, y) — how long water has been sitting
GetFlammability(x, y)
IsCoastalZone(x, y)
MutateState(address, newState) — destructive replacement of leaf state
Weather Should:
Read terrain moisture + flammability
Write moisture changes (rain) and trigger fire events
Not directly modify core terrain shape
Life / Foliage Should:
Read moisture, soil, pooling duration, rock exposure, coastal status
Modify vegetation state and potentially soil over long periods
React to fire and water events
Water System Should:
Read height and current moisture
Modify moisture and pooling duration
Potentially slowly modify terrain (erosion) in later stages


Recommended Build Order Summary
Order
What to Build
Can Run Parallel With
Notes
1
Terrain Foundation (Vol 1)
—
Must be solid first
2
Basic Foliage + Moss
Weather (light) + Coast
Early ecological feel
3
Weather (Rain, Lightning, Fire)
Foliage + Coast
Creates dynamic events
4
Beach / Rocky Coast
Foliage + Weather
Special terrain rules
5
Water Flow + Pooling
Advanced Life
Needs stable terrain + moisture
6
Advanced Life & Ecology
—
Builds on everything above
7
AI + Gameplay
—
Final layer


Next Immediate Action
Would you like me to:

Create the detailed task breakdown for Phase 0 (Terrain) right now, with each task including suggested interfaces/hooks for the layers above?
Or first draft high-level interface definitions between Terrain ↔ Weather and Terrain ↔ Life?

Just tell me which direction you want to go and I’ll produce it.

This roadmap gives us a clear path while keeping the systems loosely coupled through well-defined state and hooks. Ready when you are.

