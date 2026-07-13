Volume One: Terrain
Worm Game — Australia-Scale Procedural World Engine

Document Type: Technical Specification (Terrain Foundation)
Version: 1.2 (Progressive Hierarchical Unfolding + Destructive State Mutation)
Date: July 12, 2026
Scope: Terrain generation, materials, hierarchical determinism, progressive detail unfolding, and permanent state alteration.
Future Volumes: Volume 2 – Life | Volume 3 – Weather | Volume 4 – Gameplay & AI


1. Vision & Goals for Volume One
The goal is to create a beautiful, coherent, and vast world that feels worth inhabiting. Volume One defines the terrain foundation that enables epic scale, permanent player-driven change, and rich emergent systems.
Core Principles
Everything is deterministic from world_seed + game_time + current state.
The hierarchical seed tree is the single source of truth. It can be evaluated to different depths depending on need (distance, performance, or detail required).
Player actions are destructive and permanent. Significant changes replace the previous procedural state at that leaf. The old version no longer exists.
Progressive unfolding enables enormous vistas and massive structures: far away the tree is only partially evaluated (simple/blocky representation); closer it unfolds to full detail, including time-sensitive elements.
Water and Fire serve as primary interfaces between Terrain, Weather, and Life.
All future systems will follow the same deterministic model.

This volume focuses on terrain. Vegetation dynamics, weather, and gameplay are covered in later volumes.


2. Scale, Units & Worm-Scale Aesthetic + Vast Vistas
World units: Feet.
Player reference: ~1 inch worm. A 12-foot mound must feel massive (achieved via vertical exaggeration at render time).
Epic scale support: The system must support enormous structures and vast vistas visible from great distances while remaining fully deterministic and performant.
Far away: Partial evaluation of the determinism tree produces simplified representations (e.g., Minecraft-style block clusters of wood/leaf color for massive trees or rock formations).
Medium distance: Deeper unfolding produces proper trunks, leaf clusters, and mid-detail geometry.
Close range: Full leaf-level detail, including time-based and seasonal elements (e.g., spring flowers on a tree).

This progressive unfolding is a core feature of the hierarchical design rather than a bolted-on LOD system.


3. Determinism, Progressive Unfolding & State Alteration Philosophy
3.1 Single Source of Truth
The hierarchical seed tree (defined in Section 4) is the single source of truth for the world. The same tree can be evaluated to different depths:

Shallow evaluation (far away or low-detail needs): Stop at macro or chunk level. Produces aggregated or simplified data (e.g., "this large area is mostly leafy vegetation of type X").
Deep evaluation (close range): Unfold all the way to leaf level, including time-sensitive branches (seasonal state, growth stage, flowers, etc.).

This enables both performance at distance and rich detail up close without maintaining separate world representations.
3.2 Destructive State Mutation
When a player performs a significant terrain-altering action:

The original procedural leaf state at that hierarchical address is destroyed.
A new canonical state is computed and becomes the permanent baseline for that address.
Future queries at that address (at any depth) respect the mutated state.

There is no way to recover the previous procedural version of a mutated leaf. The change is now part of the world's deterministic reality.
3.3 New Game vs Saved Game Behavior
New game: All locations are generated purely from the hierarchical seed tree (evaluated to the required depth).
Saved/played game: Mutated leaves use their new canonical state. Untouched leaves generate normally from the seed tree.


4. Hierarchical Deterministic Topography System + Progressive Unfolding
4.1 Macro Level — Real Australian Topography
Average elevation ≈ 330 m (1,083 ft).
World divided into ~300-mile macro boxes.
Each box derives a macro_seed from real topography characteristics using a low-resolution baked DEM.
4.2 Hierarchical Seed Derivation
macro_seed   = f(real_topo_deviation, roughness, terrain_class)

level_1_seed = hash(macro_seed, sub_box_x, sub_box_y, 1)

level_2_seed = hash(level_1_seed, chunk_x, chunk_y, 2)

leaf_seed    = hash(level_2_seed, local_x, local_y, layer, 3)
4.3 Progressive / Distance-Based Unfolding
Generation functions accept a max_depth or detail_level parameter:

Far away / low detail → Stop at high level (macro or chunk). Use simplified rules or aggregated data for rendering (blocky Minecraft-style clusters for massive features).
Medium distance → Unfold one or two more levels.
Close range → Full evaluation to leaf level, including time-sensitive branches (e.g., seasonal state such as spring flowers).

This reuses the existing seed hierarchy elegantly and naturally supports:

Performance-friendly distant rendering of enormous structures.
Rich, time-based detail only when the player is close enough to observe it.
Consistent behavior with player-mutated states (the mutated state is respected at whatever depth is queried).


5. Elevation Generation
Multi-octave FBM/Simplex noise with parameters driven by the hierarchical seed + macro topographic bias.

Vertical exaggeration (V) is applied only as a final display transform for worm-scale drama.


6. Material System (Rock, Soil, Clumping)
6.1 Rock Density & Deterministic Clumping
depth_to_surface = max(0, surface_h - current_h)

rock_cluster_noise = fbm(wx, wy, depth_to_surface \times frequency, leaf_seed)

rock_density = base_rock(hierarchical_seed, arid_factor)

  + smoothstep(0, clump_depth, depth_to_surface)

    \times cluster_strength

    \times saturate(rock_cluster_noise - threshold)

  + arid_surface_rock_bonus
6.2 Soil Depth
soil_depth = clamp(

  base_soil 

  + elevation_factor \times static_foliage_bias 

  - arid_factor \times rock_exposure,

  0, max_soil

)
6.3 Surface Expression
Derived values create rich visual ground (exposed rock, soil coverage, surface rock clutter, arid vs soil-rich character).


7. State Exposed for Higher Layers (Water, Fire, Life, Coast)
Terrain must expose clean, queryable state so future systems can interact with it at appropriate depths:

GetHeight(x, y)
GetSoilDepth(x, y)
GetRockDensity(x, y)
GetMoisture(x, y)
GetPoolingDuration(x, y) — duration water has been sitting (for moss)
GetFlammability(x, y) — for lightning/fire
IsCoastalZone(x, y)
MutateLeafState(address, newState) — destructive replacement

These queries can be answered at partial tree depths when full detail is not required.


8. Permanent State Alteration (Destructive Mutation Model)
See Section 3.2. Significant changes permanently replace the procedural leaf state. The new state becomes the canonical baseline for that address going forward.


9. Region Blending
Smooth crossover blending uses distance-based falloff curves. Blending applies to untouched areas. Mutated leaves use their new canonical state.

Crossover of Regions Diagram


10. Implementation Notes
Generation functions should support a max_depth / detail_level parameter for progressive unfolding.
State storage for mutated leaves must work with partial-depth queries.
Chunk system should support on-demand deeper evaluation as the player approaches.
Aesthetic parameters should be tunable for beautiful results at all distances.


11. Relationship to Future Volumes & Progressive Detail
Volume 2: Life — Vegetation, growth, moss (from persistent water), and seasonal effects will evaluate the determinism tree to the required depth. Time-sensitive details (flowers, autumn colors) only need deep evaluation when close.
Volume 3: Weather — Rain and fire events will read/write moisture and flammability state. Fire can use flammability at the appropriate detail level.
Rendering / LOD — The progressive unfolding model enables efficient distant rendering of massive structures while delivering rich detail up close.


Summary
Volume One defines a terrain engine where:

The hierarchical seed tree serves as the single source of truth and can be evaluated progressively based on distance.
Player actions permanently mutate leaf states.
Enormous vistas and massive structures are supported through partial tree evaluation far away and full detail up close.
Clean state interfaces are provided for Water, Fire, Life, and Coast systems.
The foundation supports epic scale while remaining fully deterministic and performant.

Files

This specification (v1.2): /home/workdir/artifacts/Terrain_Volume_1_Specification.md
Crossover Diagram: /home/workdir/artifacts/imagine_images/aI8fx.jpg



This version now fully incorporates the progressive hierarchical unfolding model for vast vistas and distance-based detail while preserving the destructive state mutation philosophy. Ready for implementation.

