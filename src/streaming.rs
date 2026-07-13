//! Chunk streaming: the world-state resources (what's loaded, held, or queued),
//! the async pipeline that generates and meshes terrain off-thread, and the
//! round-radius load/unload logic with its gapless far-ground hand-off.
//! (Phase 0: tree/grass/leaf integration is stripped — this pipeline is terrain
//! only. The ecology re-layers onto it in Phase 1; see docs/project_roadmap.md.)
//! The systems here run inside main's ordered world pipeline (they're
//! `pub(crate)`, registered there alongside the silhouette + worm steps), so
//! this module owns the code but not the schedule.

use bevy::prelude::*;
use bevy::tasks::futures_lite::future;
use bevy::tasks::{block_on, AsyncComputeTaskPool, Task};
use std::collections::{HashMap, HashSet, VecDeque};

use crate::chunk_store::{archive_chunk, take_saved_chunk, ChunkArchive, ChunkRecord};
use crate::silhouettes::SilhouetteWorld;
use crate::terrain;
use crate::terrain::{apply_edits, generate_chunk_blocks, TerrainMaterials, TerrainSurface};
use crate::world::*;

#[derive(Component)]
pub(crate) struct WorldChunk {
    #[allow(dead_code)]
    coord: IVec2,
}

// PHASE 0: the two tree-lifecycle marker components below are DORMANT — nothing
// in the terrain pipeline attaches or reads them now. They're kept only so the
// paused `foliage.rs` (still compiled, unwired) keeps building; the tree grow-in
// + reveal choreography that drives them re-layers in Phase 1.

/// Real trees still building asynchronously for a live chunk. The horizon
/// cutouts watch this: a chunk's silhouette trees only retire once it hits
/// zero, so a distant tree never vanishes before its real self stands up.
#[derive(Component)]
pub struct TreesPending(pub usize);

/// Set on a chunk once every real tree is built and its (initially hidden) tree
/// roots have been flipped visible. The silhouette only despawns once its chunk
/// carries this — so the blocky stand-in holds until the detailed trees are
/// actually on screen, and the swap never opens a gap.
#[derive(Component)]
pub struct ChunkTreesRevealed;

/// Tracks which chunk columns are currently materialized in the ECS world.
#[derive(Resource, Default)]
pub(crate) struct ChunkWorld {
    // These three are read by the worm (gravity/collision/eating), a sibling
    // module, so they're pub(crate); the rest stay private to streaming.
    pub(crate) loaded: HashMap<IVec2, Entity>,
    /// Live records for chunks still in the ECS — collapsed into [`ChunkArchive`] on unload.
    pub(crate) active_records: HashMap<IVec2, ChunkRecord>,
    /// Chunks generating on the background pool right now.
    pending: HashSet<IVec2>,
    /// Actual per-column surface voxel of each live chunk (post-caves,
    /// post-burrows-at-load). The gravity floor stands on THIS, not on the
    /// height formula, which can disagree with the meshed terrain by a voxel.
    pub(crate) surface_tops: HashMap<IVec2, Vec<i32>>,
    load_queue: VecDeque<IVec2>,
    last_player_chunk: Option<IVec2>,
    /// Chunks that left the load radius but whose real meshes are kept ALIVE
    /// (still rendering) until a far silhouette stands to take over — so a
    /// receding chunk never vanishes into a gap. Held until either its stand-in
    /// is ready or it recedes past the silhouette ring (fogged out of sight);
    /// crucially NOT on a timer, so a slow-to-build stand-in can't drop a chunk
    /// that's still in view (the "turn around and the box is gone" bug).
    unloading: HashMap<IVec2, Entity>,
}

/// Everything a background thread builds for one terrain chunk.
struct BuiltChunk {
    record: ChunkRecord,
    mesh: Option<Mesh>,
    column_tops: Vec<i32>,
}

/// An in-flight background chunk build; resolved by [`finish_chunk_tasks`].
#[derive(Component)]
pub(crate) struct PendingChunk {
    task: Task<BuiltChunk>,
    coord: IVec2,
}

fn despawn_chunk_entity(entity: Entity, commands: &mut Commands) {
    // A chunk owns its terrain mesh as a child — in Bevy 0.15 a plain despawn
    // would orphan it and leave ghost geometry behind.
    commands.entity(entity).despawn_recursive();
}

fn chunk_is_queued(chunk_world: &ChunkWorld, coord: IVec2) -> bool {
    chunk_world.load_queue.iter().any(|&queued| queued == coord)
}

/// Decide which chunks should exist and queue generation — never builds meshes
/// here. Leaving chunks are parked in `ChunkWorld.unloading` (still rendering);
/// the actual despawn + archive happens in `finalize_deferred_unloads` once a
/// silhouette stands, which is why this system no longer needs Commands.
pub(crate) fn plan_chunk_streaming(
    mut chunk_world: ResMut<ChunkWorld>,
    cam_q: Query<&Transform, With<Camera>>,
) {
    let Ok(cam) = cam_q.get_single() else {
        return;
    };

    let player_chunk = world_to_chunk(cam.translation.x, cam.translation.z);
    if chunk_world.last_player_chunk == Some(player_chunk) {
        return;
    }
    chunk_world.last_player_chunk = Some(player_chunk);

    chunk_world.load_queue.retain(|coord| {
        chunk_radial_distance(*coord, player_chunk) <= CHUNK_VIEW_DISTANCE
    });

    let mut needed = Vec::new();
    for dx in -CHUNK_VIEW_DISTANCE..=CHUNK_VIEW_DISTANCE {
        for dz in -CHUNK_VIEW_DISTANCE..=CHUNK_VIEW_DISTANCE {
            let coord = player_chunk + IVec2::new(dx, dz);
            // Carve the square sweep down to a disc.
            if chunk_radial_distance(coord, player_chunk) > CHUNK_VIEW_DISTANCE
                || chunk_world.loaded.contains_key(&coord)
                || chunk_world.pending.contains(&coord)
                || chunk_is_queued(&chunk_world, coord)
            {
                continue;
            }
            needed.push(coord);
        }
    }

    needed.sort_by_key(|coord| chunk_radial_distance(*coord, player_chunk));
    for coord in needed {
        chunk_world.load_queue.push_back(coord);
    }

    // A holding chunk that wandered back inside the radius is revived outright —
    // its real meshes never went away, so it just rejoins the live set (and any
    // silhouette that began building for it gets retired by the normal path).
    let revived: Vec<IVec2> = chunk_world
        .unloading
        .keys()
        .copied()
        .filter(|coord| chunk_radial_distance(*coord, player_chunk) <= CHUNK_UNLOAD_DISTANCE)
        .collect();
    for coord in revived {
        if let Some(entity) = chunk_world.unloading.remove(&coord) {
            chunk_world.loaded.insert(coord, entity);
        }
    }

    // Leaving chunks don't despawn here — they move to the holding set, still
    // rendering, and finalize_deferred_unloads drops them once their far ground
    // stands.
    let to_unload: Vec<IVec2> = chunk_world
        .loaded
        .keys()
        .copied()
        .filter(|coord| chunk_radial_distance(*coord, player_chunk) > CHUNK_UNLOAD_DISTANCE)
        .collect();

    for coord in to_unload {
        let Some(entity) = chunk_world.loaded.remove(&coord) else {
            continue;
        };
        chunk_world.unloading.insert(coord, entity);
    }
}

/// Drop each held-over chunk the frame its far ground stands. Until then its
/// real meshes keep rendering, so the swap reads as the detailed world coarsening
/// into blocks — never a gap where the box blinks out before popping back in.
/// The ONLY other way a held chunk is released is once it has receded past the
/// silhouette ring, where distance fog already hides it — so a chunk whose
/// stand-in is slow to build (queue backed up during a fast god-mode flight) is
/// held, still visible, instead of being dropped on a timer and leaving the hole
/// you'd find by turning around. Runs every frame so a landing stand-in is
/// claimed promptly.
pub(crate) fn finalize_deferred_unloads(
    mut commands: Commands,
    mut chunk_world: ResMut<ChunkWorld>,
    mut archive: ResMut<ChunkArchive>,
    sil: Res<SilhouetteWorld>,
    cam_q: Query<&Transform, With<Camera>>,
) {
    if chunk_world.unloading.is_empty() {
        return;
    }
    let player_chunk = cam_q
        .get_single()
        .ok()
        .map(|c| world_to_chunk(c.translation.x, c.translation.z));

    let done: Vec<IVec2> = chunk_world
        .unloading
        .keys()
        .copied()
        .filter(|coord| {
            sil.stand_in_ready(*coord)
                || player_chunk.is_some_and(|pc| {
                    chunk_radial_distance(*coord, pc) > SILHOUETTE_CHUNK_DISTANCE
                })
        })
        .collect();

    for coord in done {
        let Some(entity) = chunk_world.unloading.remove(&coord) else {
            continue;
        };
        chunk_world.surface_tops.remove(&coord);
        if let Some(record) = chunk_world.active_records.remove(&coord) {
            archive_chunk(&mut archive, record);
        }
        despawn_chunk_entity(entity, &mut commands);
    }
}

/// How many chunk builds may run on the pool at once.
const MAX_CONCURRENT_CHUNK_BUILDS: usize = 3;

/// Drain the load queue onto the background compute pool — voxel generation,
/// cave carving, and meshing all happen off the main thread now, so streaming
/// into new territory never hitches the frame.
pub(crate) fn process_chunk_load_queue(
    mut commands: Commands,
    mut chunk_world: ResMut<ChunkWorld>,
    mut archive: ResMut<ChunkArchive>,
) {
    let pool = AsyncComputeTaskPool::get();

    for _ in 0..CHUNKS_PER_FRAME {
        if chunk_world.pending.len() >= MAX_CONCURRENT_CHUNK_BUILDS {
            break;
        }
        let Some(coord) = chunk_world.load_queue.pop_front() else {
            break;
        };
        if chunk_world.loaded.contains_key(&coord) || chunk_world.pending.contains(&coord) {
            continue;
        }

        let saved = take_saved_chunk(&mut archive, coord);
        let task = pool.spawn(async move {
            let record = saved.unwrap_or_else(|| ChunkRecord::generate(coord));
            let origin = chunk_world_origin(coord);
            let mut blocks = generate_chunk_blocks(coord, origin, record.terrain_seed);
            // Re-open any burrows the worm has eaten here on previous visits.
            apply_edits(&mut blocks, &record.edits);
            let column_tops = blocks.column_tops();
            let mesh = if blocks.is_empty() {
                None
            } else {
                Some(terrain::build_colored_terrain_mesh(&blocks))
            };
            BuiltChunk {
                record,
                mesh,
                column_tops,
            }
        });

        chunk_world.pending.insert(coord);
        commands.spawn(PendingChunk { task, coord });
    }
}

/// Land finished chunk builds: spawn the chunk entity and its terrain mesh.
/// (Phase 0: no leaf/grass scatter or tree queueing — terrain only.)
pub(crate) fn finish_chunk_tasks(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut chunk_world: ResMut<ChunkWorld>,
    mut archive: ResMut<ChunkArchive>,
    terrain_materials: Res<TerrainMaterials>,
    mut pending_q: Query<(Entity, &mut PendingChunk)>,
) {
    for (holder, mut pending) in &mut pending_q {
        let Some(built) = block_on(future::poll_once(&mut pending.task)) else {
            continue;
        };
        let coord = pending.coord;
        commands.entity(holder).despawn();
        chunk_world.pending.remove(&coord);

        // The player may have wandered off while this chunk was building.
        let still_wanted = chunk_world
            .last_player_chunk
            .is_none_or(|pc| chunk_radial_distance(coord, pc) <= CHUNK_UNLOAD_DISTANCE);
        if !still_wanted || chunk_world.loaded.contains_key(&coord) {
            archive_chunk(&mut archive, built.record);
            continue;
        }

        let origin = chunk_world_origin(coord);
        let chunk_entity = commands
            .spawn((
                WorldChunk { coord },
                Transform::from_translation(origin),
                Visibility::default(),
            ))
            .id();

        if let Some(mesh) = built.mesh {
            let mesh = meshes.add(mesh);
            // Real terrain spawns opaque. It must NOT fade in from transparent:
            // chunks that load without a coarse silhouette behind them (every
            // chunk at spawn, and anything inside the streamed neighbourhood)
            // would show straight through the world as a gaping hole for the
            // duration of the fade. The coarse far ground is despawned the same
            // frame this appears (see plan_ground_silhouettes), so the hand-off is
            // gapless, and the seam fix now lands both at the same height.
            let material = terrain_materials.vertex_color_terrain.clone();
            commands.entity(chunk_entity).with_children(|parent| {
                parent.spawn((
                    Mesh3d(mesh),
                    MeshMaterial3d(material),
                    Transform::IDENTITY,
                    TerrainSurface,
                ));
            });
        }

        chunk_world.surface_tops.insert(coord, built.column_tops);
        chunk_world.active_records.insert(coord, built.record);
        chunk_world.loaded.insert(coord, chunk_entity);
    }
}

