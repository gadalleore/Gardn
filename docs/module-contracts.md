# Module contracts

The cross-module surface. **Read before touching anything another track uses;
changing a signature here breaks other tracks — route it through the human, and
note it in your `coordination/*.md`.** Generated 2026-07-08; keep it in sync when
the boundaries genuinely change.

Everything is reached as `crate::<module>::Item`. Prefer `pub(crate)` over `pub`
for anything new that only the crate needs.

## Shared foundation (change with great care — everyone depends on these)

### `world.rs` — constants + helpers, no logic to break
Scale/grid: `VOXEL_SIZE`, `VOXEL_INCHES`, `VOXELS_PER_FOOT`, `INCH`,
`TREE_VOXEL_SIZE`, `TREE_VOXEL_INCHES`, `TREE_VOXELS_PER_FOOT`, `CHUNK_SIZE`,
`CHUNK_VOXELS`, `CHUNK_DEPTH_VOXELS`, `SEA_LEVEL_VOXEL_Y`, `MAX_SURFACE_VOXEL_Y`,
`WORM_LENGTH`, `WORM_EYE_HEIGHT`.
Streaming knobs: `CHUNK_VIEW_DISTANCE`, `CHUNK_UNLOAD_DISTANCE`,
`SILHOUETTE_CHUNK_DISTANCE`, `CHUNKS_PER_FRAME`, `SILHOUETTES_PER_FRAME`,
`MAX_CONCURRENT_*`, `WORLD_SEED`.
Geo: `AUSTRALIA_*`, `KM_TO_FEET`. Helpers: `world_to_chunk`, `chunk_world_origin`,
`chunk_radial_distance`, `chunk_seed`, `GardenRng`, spawn-offset fns.
> Changing a scale constant reshapes the whole world — coordinate loudly.

### `streaming.rs` — the chunk core (owned by human/core, not a track)
Resources: `ChunkWorld` (fields `loaded`, `active_records`, `surface_tops` are
`pub(crate)` for the worm; the rest private), `TreeSpawnQueue`, `FoliageSkin`.
Components: `WorldChunk`, `TreesPending(pub usize)`, `ChunkTreesRevealed`,
`PendingChunk`, `PendingTree`.
Systems (registered in `main.rs`'s ordered `.chain()`): `plan_chunk_streaming`,
`process_chunk_load_queue`, `finish_chunk_tasks`, `start_tree_build_tasks`,
`finish_tree_build_tasks`, `finalize_deferred_unloads`.
> `finish_chunk_tasks` calls into terrain/grass/leaves; `*_tree_build_tasks` into
> foliage + trees. If you change those callees' signatures, this breaks.

### `chunk_store.rs` — persistence
`ChunkArchive`, `ChunkRecord`, `ChunkTreeJob`, `SavedTree`, `archive_chunk`,
`take_saved_chunk`.

## Track surfaces

### terrain (`terrain.rs`, `topography.rs`)
Provides to streaming/silhouettes/worm:
`generate_chunk_blocks`, `apply_edits`, `build_colored_terrain_mesh`,
`build_culled_voxel_mesh`, `build_colored_block_mesh`, `downsample_blocks`,
`ChunkVoxels` (`get`/`clear_cell`/`floor_y`/`column_tops`/`is_empty`),
`BlockType`, `OreType`, `TerrainMaterials`, `TerrainSurface`, `TerrainPlugin`.
`topography`: `surface_height_voxels`, `surface_top_world_y`, `is_cave_cell`,
`CAVE_CELL_VOXELS`.
> These are consumed by streaming, worm (digging), silhouettes. Keep signatures
> stable; add new fns rather than changing existing ones.

### trees (`trees.rs`, `foliage.rs`)
`trees.rs`: `generate_tree`, `species_colors`, `species_display_name`,
`TreeSpecies`, `VoxelTreeData`. Consumed by streaming (spawn) + silhouettes (LOD).
`foliage.rs` (`pub(crate)`, used by streaming's tree spawn): `WildTree`,
`WindSway`, `FoliageLodGroup`, `FoliageLod`, `FOLIAGE_LOD_FACTORS`,
`FOLIAGE_LOD_FILL`. Plugin: `FoliagePlugin`. Reads `crate::weather::Wind`.

### weather (`weather.rs`, `sky.rs`)
`weather.rs`: `Wind` (resource — read by grass/leaves/foliage sway),
`WIND_PUSH_FROM` (read by worm). `WeatherPlugin`. Reads `crate::streaming::ChunkWorld`,
`crate::audio::GameSounds`, `crate::worm::ground_world_y`.
`sky.rs`: fully self-contained; `SkyPlugin` only.

### foliage-life (`grass.rs`, `leaves.rs`)
`grass.rs`: `GrassAssets`, `build_grass_assets`, `scatter_chunk_grass` (called by
streaming), `GRASS_WIDTH`/`GRASS_HEIGHT` (leaves reads HEIGHT), `GrassPlugin`.
`leaves.rs`: `Leaf`, `LeafAssets`, `setup_leaves`, `scatter_chunk_leaves` (called
by streaming), `LeavesPlugin`. Both read `crate::weather::Wind`.

### worm (`worm.rs`) — core, not a track
`GodMode`, `WORM_SPEED`, `GOD_SPEED_MULT`, `CastingAssets`, `PendingBurrow`,
`ground_world_y` (read by weather), `eat_leaves`/`finish_burrow_tasks` (in main's
chain), `WormPlugin`.

### other core
`silhouettes.rs`: `SilhouetteWorld` (`stand_in_ready`), `plan_tree_silhouettes`,
`process_silhouette_queue`, `finish_sil_tree_tasks`, `build_far_ground_mesh`.
`distance_blur.rs`: `DistanceBlur` (component with the tuning knobs),
`DistanceBlurPlugin`. `audio.rs`: `GameSounds`, `GameAudioPlugin`.

## The one shared choke point

Adding a subsystem plugin touches `main.rs`'s `.add_plugins(...)` list — a
one-line add. That's the only place tracks overlap; trivial to merge, but it IS
`main.rs`, so mention it in your coordination file so the human expects the diff.
