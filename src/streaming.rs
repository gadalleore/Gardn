//! Chunk streaming: the world-state resources (what's loaded, held, or queued),
//! the async pipeline that generates/meshes terrain and grows trees off-thread,
//! and the round-radius load/unload logic with its gapless silhouette hand-off.
//! The systems here run inside main's ordered world pipeline (they're
//! `pub(crate)`, registered there alongside the silhouette + worm steps), so
//! this module owns the code but not the schedule.

use bevy::prelude::*;
use bevy::render::view::NoFrustumCulling;
use bevy::tasks::futures_lite::future;
use bevy::tasks::{block_on, AsyncComputeTaskPool, Task};
use std::collections::{HashMap, HashSet, VecDeque};

use crate::chunk_store::{
    archive_chunk, take_saved_chunk, ChunkArchive, ChunkRecord, ChunkTreeJob,
};
use crate::foliage::{
    FoliageLod, FoliageLodGroup, WildTree, WindSway, FOLIAGE_LOD_FACTORS, FOLIAGE_LOD_FILL,
};
use crate::grass::{scatter_chunk_grass, GrassAssets};
use crate::leaves::{scatter_chunk_leaves, LeafAssets};
use crate::silhouettes::SilhouetteWorld;
use crate::terrain;
use crate::terrain::{
    apply_edits, build_culled_voxel_mesh, downsample_blocks, generate_chunk_blocks,
    TerrainMaterials, TerrainSurface,
};
use crate::trees::{generate_tree, TreeSpecies, VoxelTreeData};
use crate::world::*;

#[derive(Component)]
pub(crate) struct WorldChunk {
    #[allow(dead_code)]
    coord: IVec2,
}

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

#[derive(Resource, Default)]
pub(crate) struct TreeSpawnQueue(VecDeque<ChunkTreeJob>);

/// Optional player-authored skin for tree foliage blocks (`assets/foliage.png`).
/// Painted in grayscale — each species' foliage colour multiplies it, so one
/// texture skins every tree. Alpha in the texture makes leaf blocks semi-
/// transparent. `None` (file absent) falls back to flat-colour foliage.
#[derive(Resource, Default)]
pub(crate) struct FoliageSkin(pub(crate) Option<Handle<Image>>);

/// Everything the render world needs for one finished tree, produced on a
/// background compute thread so 650-ft giants never hitch the frame.
struct BuiltTree {
    bark_mesh: Mesh,
    /// One foliage mesh per LOD level, finest first.
    foliage_meshes: [Mesh; FOLIAGE_LOD_FACTORS.len()],
    foliage_center: Vec3,
    foliage_radius: f32,
    bark_color: Color,
    foliage_color: Color,
}

/// An in-flight background tree build; resolved by [`finish_tree_build_tasks`].
#[derive(Component)]
pub(crate) struct PendingTree {
    task: Task<BuiltTree>,
    chunk_entity: Entity,
    local_base: Vec3,
    species: TreeSpecies,
    tree_seed: u64,
}

pub(crate) fn start_tree_build_tasks(
    mut commands: Commands,
    mut tree_queue: ResMut<TreeSpawnQueue>,
    pending: Query<(), With<PendingTree>>,
) {
    let mut in_flight = pending.iter().count();
    if in_flight >= MAX_CONCURRENT_TREE_BUILDS {
        return;
    }
    let pool = AsyncComputeTaskPool::get();

    while in_flight < MAX_CONCURRENT_TREE_BUILDS {
        let Some(job) = tree_queue.0.pop_front() else {
            break;
        };

        let species = job.species;
        let tree_seed = job.tree_seed;
        let bark = job.bark_color;
        let foliage = job.foliage_color;

        let task = pool.spawn(async move {
            let mut rng = GardenRng::new(tree_seed);
            let tree: VoxelTreeData = generate_tree(species, &mut rng);
            // Bark and foliage are both rasterised on the world voxel grid —
            // one block size for the whole world.
            let bark_mesh = build_culled_voxel_mesh(&tree.bark, TREE_VOXEL_SIZE);
            let foliage_meshes = FOLIAGE_LOD_FACTORS.map(|factor| {
                if factor == 1 {
                    build_culled_voxel_mesh(&tree.foliage, TREE_VOXEL_SIZE)
                } else {
                    let coarse = downsample_blocks(&tree.foliage, factor, FOLIAGE_LOD_FILL);
                    build_culled_voxel_mesh(&coarse, TREE_VOXEL_SIZE * factor as f32)
                }
            });

            // Canopy bounding sphere (tree-local feet) for distance-based LOD.
            let mut min = Vec3::splat(f32::MAX);
            let mut max = Vec3::splat(f32::MIN);
            for v in &tree.foliage {
                let p = v.as_vec3() * TREE_VOXEL_SIZE;
                min = min.min(p);
                max = max.max(p);
            }
            let foliage_center = (min + max) * 0.5;
            let foliage_radius = (max - min).length() * 0.5;
            let bark_color = Color::srgb(
                rng.range(bark.0 - 0.05, bark.0 + 0.05),
                rng.range(bark.1 - 0.05, bark.1 + 0.05),
                rng.range(bark.2 - 0.05, bark.2 + 0.05),
            );
            let foliage_color = Color::srgb(
                rng.range(foliage.0 - 0.05, foliage.0 + 0.05),
                rng.range(foliage.1 - 0.05, foliage.1 + 0.05),
                rng.range(foliage.2 - 0.05, foliage.2 + 0.05),
            );
            BuiltTree {
                bark_mesh,
                foliage_meshes,
                foliage_center,
                foliage_radius,
                bark_color,
                foliage_color,
            }
        });

        commands.spawn(PendingTree {
            task,
            chunk_entity: job.chunk_entity,
            local_base: job.local_base,
            species,
            tree_seed,
        });
        in_flight += 1;
    }
}

/// Collect finished background builds and stand the trees up in the world.
pub(crate) fn finish_tree_build_tasks(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    foliage_skin: Res<FoliageSkin>,
    mut pending_q: Query<(Entity, &mut PendingTree)>,
    mut tree_counts: Query<&mut TreesPending, With<WorldChunk>>,
) {
    for (holder, mut pending) in &mut pending_q {
        let Some(built) = block_on(future::poll_once(&mut pending.task)) else {
            continue;
        };

        // The chunk may have streamed out while the tree was building.
        if let Ok(mut count) = tree_counts.get_mut(pending.chunk_entity) {
            count.0 = count.0.saturating_sub(1);
            let bark_mesh = meshes.add(built.bark_mesh);
            let foliage_meshes = built.foliage_meshes.map(|m| meshes.add(m));
            // Trees build OPAQUE and HIDDEN, then reveal in one atomic frame once
            // the whole chunk is standing (reveal_built_chunks), as its blocky
            // silhouette despawns the same frame — so the swap reads as the coarse
            // blocks snapping into full detail, seamless like the foliage-box LODs,
            // with no cross-fade ghosting and no gap. (No fade-in from alpha 0: the
            // one case without a silhouette stand-in is the very first chunks at
            // spawn, which just appear when ready.)
            let bark_material = materials.add(StandardMaterial {
                base_color: built.bark_color,
                alpha_mode: AlphaMode::Opaque,
                ..default()
            });
            // The species tint multiplies the (grayscale) skin texture, so one
            // player-drawn foliage.png colours itself per tree. The skin's alpha
            // is a hard leaf-shaped cutout, so we alpha-MASK it rather than
            // blend: Mask writes depth (Blend does not), which the distance-blur
            // post-process needs — otherwise it reads the sky behind the canopy
            // and the leaves stay blurred even right up close. Flat colour stays
            // opaque. Strand-like foliage (fern fronds, casuarina needles) is
            // drawn as thin ribbons of blocks — the cutout skin shreds those, so
            // they stay solid.
            let strand_foliage = matches!(
                pending.species,
                TreeSpecies::TreeFern | TreeSpecies::DesertOak
            );
            let skin = if strand_foliage {
                None
            } else {
                foliage_skin.0.clone()
            };
            // Skinned foliage is alpha-MASKed (not blended): Mask writes depth,
            // which the distance-blur post-process needs — otherwise it reads the
            // sky behind the canopy and the leaves stay blurred right up close.
            // Flat colour stays opaque.
            let foliage_final_mode = if skin.is_some() {
                AlphaMode::Mask(0.5)
            } else {
                AlphaMode::Opaque
            };
            let foliage_material = materials.add(StandardMaterial {
                base_color: built.foliage_color,
                alpha_mode: foliage_final_mode,
                base_color_texture: skin,
                ..default()
            });

            let species = pending.species;
            let local_base = pending.local_base;

            // Sway character scales with stature: giants heave in slow, small
            // arcs; scrub whips about. Seeded per tree so a forest never rocks
            // in unison.
            let mut sway_rng = GardenRng::new(pending.tree_seed ^ 0x57A9_11FE);
            let stature_ft = built.foliage_center.y.max(10.0);
            let wind_sway = WindSway {
                phase: sway_rng.range(0.0, std::f32::consts::TAU),
                amplitude: (0.006 + 2.5 / stature_ft).min(0.03)
                    + sway_rng.range(0.0, 0.004),
                frequency: (60.0 / stature_ft).clamp(0.25, 2.0)
                    * sway_rng.range(0.85, 1.15),
            };

            commands.entity(pending.chunk_entity).with_children(|trees| {
                trees
                    .spawn((
                        WildTree { species },
                        wind_sway,
                        FoliageLodGroup {
                            center: built.foliage_center,
                            radius: built.foliage_radius,
                            level: 0,
                        },
                        // Hidden until the whole chunk is built, then revealed
                        // atomically as the silhouette drops — a clean snap, not a
                        // fade. reveal_built_chunks flips this to Inherited.
                        Visibility::Hidden,
                        Transform::from_translation(local_base),
                    ))
                    .with_children(|tree_root| {
                        tree_root.spawn((
                            Mesh3d(bark_mesh),
                            MeshMaterial3d(bark_material),
                            Transform::IDENTITY,
                            // A titan trunk towers far outside a low, close
                            // camera's view cone, so Bevy's per-mesh frustum
                            // test can reject the whole thing even while its base
                            // fills the screen — the tree blinks off as you fly
                            // at it. Trees are big landmark objects worth always
                            // submitting; skip the cull so they never vanish.
                            NoFrustumCulling,
                        ));
                        // All LOD rungs spawn hidden; update_foliage_lod shows the
                        // right one (even while the tree root is still hidden), so
                        // the correct rung is already live the frame we reveal.
                        for (level, mesh) in foliage_meshes.into_iter().enumerate() {
                            tree_root.spawn((
                                FoliageLod { level },
                                Mesh3d(mesh),
                                MeshMaterial3d(foliage_material.clone()),
                                Transform::IDENTITY,
                                Visibility::Hidden,
                                // Same reason as the bark: a giant's crown sits
                                // hundreds of feet up, outside a low camera's
                                // frustum, and would blink out. (LOD visibility
                                // still hides the inactive rungs.)
                                NoFrustumCulling,
                            ));
                        }
                    });
            });
        }

        commands.entity(holder).despawn();
    }
}

fn despawn_entity_tree(entity: Entity, commands: &mut Commands) {
    // Chunks own terrain meshes and trees as children — in Bevy 0.15 a plain
    // despawn would orphan them and leave ghost geometry behind.
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
    mut tree_queue: ResMut<TreeSpawnQueue>,
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
    // rendering, and finalize_deferred_unloads drops them once their silhouette
    // stands. Cancel any not-yet-built real trees for them so nothing new spawns
    // into a chunk that's on its way out.
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
        tree_queue.0.retain(|job| job.chunk_entity != entity);
        chunk_world.unloading.insert(coord, entity);
    }
}

/// Drop each held-over chunk the frame its far silhouette stands. Until then its
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
        despawn_entity_tree(entity, &mut commands);
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

/// Land finished chunk builds: spawn the chunk entity, its terrain mesh and
/// leaves, and queue its trees.
pub(crate) fn finish_chunk_tasks(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut chunk_world: ResMut<ChunkWorld>,
    mut archive: ResMut<ChunkArchive>,
    mut tree_queue: ResMut<TreeSpawnQueue>,
    terrain_materials: Res<TerrainMaterials>,
    leaf_assets: Res<LeafAssets>,
    grass_assets: Res<GrassAssets>,
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
                TreesPending(built.record.trees.len()),
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
            // frame this appears (see plan_tree_silhouettes), so the hand-off is
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
        scatter_chunk_leaves(&mut commands, chunk_entity, coord, &leaf_assets);
        scatter_chunk_grass(
            &mut commands,
            chunk_entity,
            coord,
            &built.column_tops,
            &grass_assets,
        );

        tree_queue.0.extend(built.record.tree_jobs(chunk_entity));
        chunk_world.surface_tops.insert(coord, built.column_tops);
        chunk_world.active_records.insert(coord, built.record);
        chunk_world.loaded.insert(coord, chunk_entity);
    }
}

