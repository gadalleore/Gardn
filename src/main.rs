mod australia;
mod chunk_store;
mod map_ui;
mod silhouettes;
mod terrain;
mod topography;
mod trees;
mod world;

use bevy::prelude::*;
use bevy::asset::RenderAssetUsages;
use bevy::pbr::{CascadeShadowConfigBuilder, DistanceFog, FogFalloff};
use bevy::render::mesh::{Indices, PrimitiveTopology};
use bevy::render::RenderPlugin;
use bevy::render::settings::{RenderCreation, WgpuSettings};
use bevy::render::texture::ImagePlugin;
use bevy::tasks::futures_lite::future;
use bevy::tasks::{block_on, AsyncComputeTaskPool, Task};
use bevy_flycam::prelude::*;
use std::collections::{HashMap, VecDeque};
use std::time::{SystemTime, UNIX_EPOCH};

use australia::{biome_at_world, biome_display_name, biome_profile, pick_coastal_spawn, AussieBiome};
use chunk_store::{
    archive_chunk, take_saved_chunk, ChunkArchive, ChunkRecord, ChunkTreeJob,
};
use map_ui::{setup_map_ui, toggle_map_ui, update_map_ui, MapOverlay};
use silhouettes::{
    billboard_silhouettes, plan_tree_silhouettes, process_silhouette_queue,
    setup_silhouette_assets, SilhouetteWorld,
};
use terrain::{
    apply_edits, build_culled_voxel_mesh, generate_chunk_blocks, spawn_terrain_meshes, BlockType,
    ChunkVoxels, TerrainMaterials, TerrainSurface,
};
use topography::surface_top_world_y;
use trees::{generate_tree, TreeSpecies, VoxelTreeData};
use world::*;

#[derive(Component)]
struct Leaf;

/// Any procedurally generated native tree; species recorded for future
/// fauna/food interactions.
#[derive(Component)]
struct WildTree {
    #[allow(dead_code)]
    species: TreeSpecies,
}

#[derive(Component)]
struct FloatingLeaf {
    base_y: f32,
    phase: f32,
    bob_speed: f32,
    spin_speed: f32,
    base_rotation: Quat,   // artistic starting orientation
}

/// Root entity for one streamed chunk column — despawning this removes terrain + trees.
#[derive(Component)]
struct WorldChunk {
    coord: IVec2,
}

/// Tracks which chunk columns are currently materialized in the ECS world.
#[derive(Resource, Default)]
struct ChunkWorld {
    loaded: HashMap<IVec2, Entity>,
    /// Live records for chunks still in the ECS — collapsed into [`ChunkArchive`] on unload.
    active_records: HashMap<IVec2, ChunkRecord>,
    load_queue: VecDeque<IVec2>,
    last_player_chunk: Option<IVec2>,
}

#[derive(Resource, Default)]
struct TreeSpawnQueue(VecDeque<ChunkTreeJob>);

/// Shared handles for the extruded-PNG leaf so every chunk can scatter copies.
#[derive(Resource)]
struct LeafAssets {
    mesh: Handle<Mesh>,
    material: Handle<StandardMaterial>,
}

#[derive(Resource)]
struct GameSounds {
    munch: Handle<AudioSource>,
}

/// Everything the render world needs for one finished tree, produced on a
/// background compute thread so 650-ft giants never hitch the frame.
struct BuiltTree {
    bark_mesh: Mesh,
    foliage_mesh: Mesh,
    bark_color: Color,
    foliage_color: Color,
}

/// An in-flight background tree build; resolved by [`finish_tree_build_tasks`].
#[derive(Component)]
struct PendingTree {
    task: Task<BuiltTree>,
    chunk_entity: Entity,
    local_base: Vec3,
    species: TreeSpecies,
}

/// A completed bite: which voxel came out of which chunk, plus the freshly
/// rebuilt terrain mesh, all computed off the main thread.
struct BurrowResult {
    coord: IVec2,
    local: IVec3,
    block: BlockType,
    mesh: Mesh,
}

/// An in-flight burrow (probe + carve + remesh) on the async pool. Only one at
/// a time — a second `E` while chewing is ignored.
#[derive(Component)]
struct PendingBurrow {
    task: Task<Option<BurrowResult>>,
}

fn main() {
    App::new()
        .add_plugins(DefaultPlugins
            .set(ImagePlugin::default_nearest())
            .set(WindowPlugin {
                primary_window: Some(Window {
                    title: "gardn".into(),
                    resolution: (1280., 720.).into(),
                    ..default()
                }),
                ..default()
            })
            .set(RenderPlugin {
                // Force Vulkan backend — much more stable than DirectX 12 on many Windows machines
                render_creation: RenderCreation::Automatic(WgpuSettings {
                    backends: Some(wgpu::Backends::VULKAN),
                    ..default()
                }),
                ..default()
            })
        )
        .add_plugins(PlayerPlugin) // Adds WASD + mouse look camera automatically
        .insert_resource(MovementSettings {
            sensitivity: 0.00012,
            speed: 1.8, // Slow crawl — we're a tiny worm
            ..default()
        })
        .insert_resource(ClearColor(Color::srgb(0.58, 0.72, 0.88))) // Soft garden sky
        .init_resource::<ChunkWorld>()
        .init_resource::<ChunkArchive>()
        .init_resource::<TreeSpawnQueue>()
        .init_resource::<SilhouetteWorld>()
        .init_resource::<MapOverlay>()
        .add_systems(
            Startup,
            (
                choose_spawn_location,
                setup_garden,
                setup_silhouette_assets,
                setup_map_ui,
            )
                .chain(),
        )
        // The world-structure systems are chained: each one then sees the
        // previous one's spawns/despawns actually applied. Unordered, a chunk
        // unload could race an eat/tree-finish touching entities in that chunk
        // and double-despawn them (Bevy's B0003 warning).
        .add_systems(
            Update,
            (
                plan_chunk_streaming,
                process_chunk_load_queue,
                start_tree_build_tasks,
                finish_tree_build_tasks,
                plan_tree_silhouettes,
                process_silhouette_queue,
                eat_leaves,
                finish_burrow_tasks,
            )
                .chain(),
        )
        .add_systems(
            Update,
            (
                billboard_silhouettes,
                toggle_map_ui,
                update_map_ui,
                animate_floating_leaves,
            ),
        )
        .add_systems(PostStartup, (lower_worm_camera, plan_chunk_streaming))
        .run();
}

/// Sets up the very first basic garden space
fn setup_garden(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    asset_server: Res<AssetServer>,
) {
    commands.insert_resource(TerrainMaterials::new(&mut materials));

    // 3D leaves: extruded from the higher-res pixel art leaf.png with jagged 8-bit outline following the sprite pixels exactly
    // (coffee-coaster scale). The mesh itself is the leaf silhouette; spins/bobs
    // use the same logic as before so placements still look good.
    // (Press E when close to one to eat)
    let leaf_texture = asset_server.load("leaf.png");
    let leaf_assets = LeafAssets {
        material: materials.add(StandardMaterial {
            base_color_texture: Some(leaf_texture),
            // The mesh geometry *is* the leaf outline now — no need for alpha
            // cutout. Opaque is cleanest + cheapest.
            alpha_mode: AlphaMode::Opaque,
            double_sided: true,
            ..default()
        }),
        mesh: create_extruded_leaf_mesh(&mut meshes),
    };
    spawn_textured_leaves(&mut commands, &leaf_assets);
    commands.insert_resource(leaf_assets);

    commands.insert_resource(GameSounds {
        munch: asset_server.load("sounds/munch.wav"),
    });

    // Sun light — shadows on, so open canopies throw dappled light shafts onto
    // the forest floor. Tight first cascade keeps shadow detail crisp at worm
    // eye level; the far cascades cover the giants overhead.
    commands.spawn((
        DirectionalLight {
            illuminance: 11_000.0,
            shadows_enabled: true,
            ..default()
        },
        CascadeShadowConfigBuilder {
            num_cascades: 4,
            first_cascade_far_bound: 12.0,
            maximum_distance: 350.0,
            ..default()
        }
        .build(),
        Transform::from_rotation(Quat::from_euler(EulerRot::XYZ, -0.9, 0.7, 0.0)),
    ));

    commands.insert_resource(AmbientLight {
        color: Color::srgb(0.82, 0.88, 0.95),
        brightness: 70.0,
    });

    // Background music, looping for the whole session.
    commands.spawn((
        AudioPlayer::new(asset_server.load("music/gardnr.mp3")),
        PlaybackSettings::LOOP,
    ));
}

/// Pick a fresh spot on a green stretch of coastline for this launch and pin it
/// to world origin — every new game starts on a different beach with the ocean
/// in view. Must run before any terrain/biome sampling.
fn choose_spawn_location() {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    let seed = nanos ^ WORLD_SEED ^ 0x5DEE_CE66_A55A_1234;

    let (lat, lon) = pick_coastal_spawn(seed);
    set_spawn_geo_offset(geo_to_world_offset(lat, lon));

    // Now that the offset is set, world origin reports the spawn biome.
    let biome = biome_at_world(0.0, 0.0);
    println!(
        "🌏 New game — the little worm washes up on the {} coast ({:.2}°S {:.2}°E).",
        biome_display_name(biome),
        -lat,
        lon
    );
}

/// Kick queued tree jobs onto the async compute pool. Voxel generation and mesh
/// building for a giant can take a good chunk of a frame, so it all happens off
/// the main thread; only asset registration and entity spawning stay on it.
fn start_tree_build_tasks(
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
            let bark_mesh = build_culled_voxel_mesh(&tree.bark, VOXEL_SIZE);
            let foliage_mesh = build_culled_voxel_mesh(&tree.foliage, VOXEL_SIZE);
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
                foliage_mesh,
                bark_color,
                foliage_color,
            }
        });

        commands.spawn(PendingTree {
            task,
            chunk_entity: job.chunk_entity,
            local_base: job.local_base,
            species,
        });
        in_flight += 1;
    }
}

/// Collect finished background builds and stand the trees up in the world.
fn finish_tree_build_tasks(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut pending_q: Query<(Entity, &mut PendingTree)>,
    live_chunks: Query<(), With<WorldChunk>>,
) {
    for (holder, mut pending) in &mut pending_q {
        let Some(built) = block_on(future::poll_once(&mut pending.task)) else {
            continue;
        };

        // The chunk may have streamed out while the tree was building.
        if live_chunks.get(pending.chunk_entity).is_ok() {
            let bark_mesh = meshes.add(built.bark_mesh);
            let foliage_mesh = meshes.add(built.foliage_mesh);
            let bark_material = materials.add(StandardMaterial {
                base_color: built.bark_color,
                ..default()
            });
            let foliage_material = materials.add(StandardMaterial {
                base_color: built.foliage_color,
                ..default()
            });

            let species = pending.species;
            let local_base = pending.local_base;
            commands.entity(pending.chunk_entity).with_children(|trees| {
                trees
                    .spawn((
                        WildTree { species },
                        Visibility::default(),
                        Transform::from_translation(local_base),
                    ))
                    .with_children(|tree_root| {
                        tree_root.spawn((
                            Mesh3d(bark_mesh),
                            MeshMaterial3d(bark_material),
                            Transform::IDENTITY,
                        ));
                        tree_root.spawn((
                            Mesh3d(foliage_mesh),
                            MeshMaterial3d(foliage_material),
                            Transform::IDENTITY,
                        ));
                    });
            });
        }

        commands.entity(holder).despawn();
    }
}

fn load_chunk(
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    terrain_materials: &TerrainMaterials,
    record: &ChunkRecord,
) -> Entity {
    let coord = record.coord;
    let origin = chunk_world_origin(coord);

    let chunk_entity = commands
        .spawn((
            WorldChunk { coord },
            Transform::from_translation(origin),
            Visibility::default(),
        ))
        .id();

    let mut blocks = generate_chunk_blocks(coord, origin, record.terrain_seed);
    // Re-open any burrows the worm has eaten here on previous visits.
    apply_edits(&mut blocks, &record.edits);
    spawn_terrain_meshes(commands, chunk_entity, meshes, terrain_materials, blocks);

    chunk_entity
}

fn despawn_entity_tree(entity: Entity, commands: &mut Commands) {
    // Chunks own terrain meshes and trees as children — in Bevy 0.15 a plain
    // despawn would orphan them and leave ghost geometry behind.
    commands.entity(entity).despawn_recursive();
}

fn chunk_is_queued(chunk_world: &ChunkWorld, coord: IVec2) -> bool {
    chunk_world.load_queue.iter().any(|&queued| queued == coord)
}

/// Decide which chunks should exist and queue generation — never builds meshes here.
fn plan_chunk_streaming(
    mut commands: Commands,
    mut chunk_world: ResMut<ChunkWorld>,
    mut archive: ResMut<ChunkArchive>,
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
        chunk_chebyshev_distance(*coord, player_chunk) <= CHUNK_VIEW_DISTANCE
    });

    let mut needed = Vec::new();
    for dx in -CHUNK_VIEW_DISTANCE..=CHUNK_VIEW_DISTANCE {
        for dz in -CHUNK_VIEW_DISTANCE..=CHUNK_VIEW_DISTANCE {
            let coord = player_chunk + IVec2::new(dx, dz);
            if chunk_world.loaded.contains_key(&coord) || chunk_is_queued(&chunk_world, coord) {
                continue;
            }
            needed.push(coord);
        }
    }

    needed.sort_by_key(|coord| chunk_chebyshev_distance(*coord, player_chunk));
    for coord in needed {
        chunk_world.load_queue.push_back(coord);
    }

    let to_unload: Vec<IVec2> = chunk_world
        .loaded
        .keys()
        .copied()
        .filter(|coord| chunk_chebyshev_distance(*coord, player_chunk) > CHUNK_UNLOAD_DISTANCE)
        .collect();

    for coord in to_unload {
        let Some(entity) = chunk_world.loaded.remove(&coord) else {
            continue;
        };
        tree_queue.0.retain(|job| job.chunk_entity != entity);
        if let Some(record) = chunk_world.active_records.remove(&coord) {
            archive_chunk(&mut archive, record);
        }
        despawn_entity_tree(entity, &mut commands);
    }
}

/// Drain the load queue a little each frame so the main thread stays responsive.
fn process_chunk_load_queue(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut chunk_world: ResMut<ChunkWorld>,
    mut archive: ResMut<ChunkArchive>,
    mut tree_queue: ResMut<TreeSpawnQueue>,
    terrain_materials: Res<TerrainMaterials>,
    leaf_assets: Res<LeafAssets>,
) {
    for _ in 0..CHUNKS_PER_FRAME {
        let Some(coord) = chunk_world.load_queue.pop_front() else {
            break;
        };

        if chunk_world.loaded.contains_key(&coord) {
            continue;
        }
        if tree_queue.0.len() >= MAX_TREE_QUEUE {
            chunk_world.load_queue.push_front(coord);
            break;
        }

        let record =
            take_saved_chunk(&mut archive, coord).unwrap_or_else(|| ChunkRecord::generate(coord));

        let entity = load_chunk(&mut commands, &mut meshes, &terrain_materials, &record);
        scatter_chunk_leaves(&mut commands, entity, coord, &leaf_assets);
        chunk_world.active_records.insert(coord, record.clone());
        tree_queue.0.extend(record.tree_jobs(entity));
        chunk_world.loaded.insert(coord, entity);
    }
}

/// A collectible leaf is ~3 worm-lengths across.
const LEAF_BASE_SCALE: f32 = (WORM_LENGTH * 3.0) / 0.95;

/// Spawns the hand-placed welcome leaves around the starting beach.
/// (Chunk streaming scatters many more everywhere — see [`scatter_chunk_leaves`].)
fn spawn_textured_leaves(commands: &mut Commands, leaf_assets: &LeafAssets) {
    let leaf_scale = LEAF_BASE_SCALE;

    // Leaf data: (position, base rotation, scale multiplier)
    let leaf_spawns: [(Vec3, Quat, f32); 7] = [
        (Vec3::new(-3.5, 0.8, -4.2), Quat::from_rotation_x(-0.2), 0.9),
        (Vec3::new(6.2, 0.7, 6.8), Quat::from_rotation_x(-0.15) * Quat::from_rotation_z(-0.4), 1.0),
        (Vec3::new(-11.5, 1.0, 13.0), Quat::from_rotation_x(-0.25) * Quat::from_rotation_z(0.3), 0.85),
        (Vec3::new(20.5, 0.9, -4.5), Quat::from_rotation_x(-0.18), 0.95),
        (Vec3::new(-6.1, 1.6, -8.6), Quat::from_euler(EulerRot::XYZ, -0.7, 0.5, 0.2), 0.8),
        (Vec3::new(9.1, 1.4, 13.8), Quat::from_euler(EulerRot::XYZ, -0.5, -1.0, -0.15), 1.05),
        (Vec3::new(-5.8, 2.2, -10.2), Quat::from_euler(EulerRot::XYZ, -1.0, 0.8, 0.25), 0.75),
    ];

    for (i, (pos, base_rot, scale)) in leaf_spawns.iter().enumerate() {
        let phase = i as f32 * 1.7;
        let bob_speed = 1.8 + (i as f32 * 0.07);
        let spin_speed = 0.85 + (i as f32 * 0.1);

        // Original hover heights were authored against flat ground — ride the
        // local terrain surface instead.
        let pos = Vec3::new(pos.x, surface_top_world_y(pos.x, pos.z) + pos.y, pos.z);

        commands.spawn((
            Mesh3d(leaf_assets.mesh.clone()),
            MeshMaterial3d(leaf_assets.material.clone()),
            Transform {
                translation: pos,
                rotation: *base_rot,
                scale: Vec3::splat(*scale * leaf_scale),
            },
            Leaf,
            FloatingLeaf {
                base_y: pos.y,
                phase,
                bob_speed,
                spin_speed,
                base_rotation: *base_rot,
            },
        ));
    }
}

/// Litter every land chunk with bobbing leaves — deterministic per chunk, count
/// scaled by how wooded the biome is, each riding the local terrain surface.
fn scatter_chunk_leaves(
    commands: &mut Commands,
    chunk_entity: Entity,
    coord: IVec2,
    leaf_assets: &LeafAssets,
) {
    let origin = chunk_world_origin(coord);
    let center = origin + Vec3::new(CHUNK_SIZE * 0.5, 0.0, CHUNK_SIZE * 0.5);
    let profile = biome_profile(center.x, center.z);
    if profile.biome == AussieBiome::Ocean {
        return;
    }

    let mut rng = GardenRng::new(chunk_seed(WORLD_SEED, coord) ^ 0x1EAF_1EAF);
    let count = (rng.range(3.0, 7.0) * (0.4 + profile.tree_density * 0.5)).round() as i32;

    commands.entity(chunk_entity).with_children(|chunk| {
        for _ in 0..count {
            let lx = rng.range(1.5, CHUNK_SIZE - 1.5);
            let lz = rng.range(1.5, CHUNK_SIZE - 1.5);
            let ground = surface_top_world_y(origin.x + lx, origin.z + lz);
            let y = ground + rng.range(0.4, 2.2);

            let base_rot = Quat::from_euler(
                EulerRot::XYZ,
                rng.range(-1.0, 0.2),
                rng.range(0.0, std::f32::consts::TAU),
                rng.range(-0.4, 0.4),
            );

            chunk.spawn((
                Mesh3d(leaf_assets.mesh.clone()),
                MeshMaterial3d(leaf_assets.material.clone()),
                Transform {
                    translation: Vec3::new(lx, y, lz),
                    rotation: base_rot,
                    scale: Vec3::splat(LEAF_BASE_SCALE * rng.range(0.65, 1.15)),
                },
                Leaf,
                FloatingLeaf {
                    base_y: y,
                    phase: rng.range(0.0, std::f32::consts::TAU),
                    bob_speed: rng.range(1.2, 2.4),
                    spin_speed: rng.range(0.45, 1.3),
                    base_rotation: base_rot,
                },
            ));
        }
    });
}

/// Little guy (the flycam) eats with `E`. Leaves take priority when one is in
/// range; otherwise the worm BURROWS — the voxel it's facing (or the ground
/// under it) is eaten out of the world for real, the chunk remeshes on a
/// background thread, and the bite is remembered so tunnels survive streaming.
fn eat_leaves(
    mut commands: Commands,
    keyboard: Res<ButtonInput<KeyCode>>,
    sounds: Res<GameSounds>,
    chunk_world: Res<ChunkWorld>,
    cam_q: Query<&Transform, With<Camera>>,
    leaf_q: Query<(Entity, &GlobalTransform), With<Leaf>>,
    pending_q: Query<(), With<PendingBurrow>>,
) {
    if !keyboard.just_pressed(KeyCode::KeyE) {
        return;
    }

    let Ok(cam) = cam_q.get_single() else { return; };
    let cam_pos = cam.translation;

    let mut closest: Option<(Entity, f32)> = None;

    // Leaves may be chunk children, so measure in world space.
    for (ent, tf) in &leaf_q {
        let d = cam_pos.distance(tf.translation());
        if d < 2.8 && closest.map_or(true, |(_, cd)| d < cd) {
            closest = Some((ent, d));
        }
    }

    fn munch(commands: &mut Commands, sounds: &GameSounds) {
        commands.spawn((
            AudioPlayer::new(sounds.munch.clone()),
            PlaybackSettings::DESPAWN,
        ));
    }

    if let Some((ent, d)) = closest {
        commands.entity(ent).despawn();
        munch(&mut commands, &sounds);
        println!("🍃 Yum! Little guy devoured a leaf (dist: {:.1})", d);
        return;
    }

    // No leaf — burrow, off the main thread. Snapshot the nearby chunk records
    // and hand the probe + carve + remesh to the compute pool; the frame never
    // waits. One bite in flight at a time.
    if !pending_q.is_empty() {
        return;
    }

    let player_chunk = world_to_chunk(cam_pos.x, cam_pos.z);
    let mut records: Vec<(IVec2, u64, std::collections::HashSet<IVec3>)> = Vec::new();
    for dx in -1..=1 {
        for dz in -1..=1 {
            let coord = player_chunk + IVec2::new(dx, dz);
            if !chunk_world.loaded.contains_key(&coord) {
                continue;
            }
            if let Some(record) = chunk_world.active_records.get(&coord) {
                records.push((coord, record.terrain_seed, record.edits.clone()));
            }
        }
    }

    let forward = *cam.forward();
    let pool = AsyncComputeTaskPool::get();
    let task = pool.spawn(async move { probe_and_carve(cam_pos, forward, &records) });
    commands.spawn(PendingBurrow { task });
}

/// Probe along the look direction, then straight down, against the same
/// deterministic generation (plus recorded bites) that built the chunk meshes.
/// On a hit, carve the voxel and rebuild that chunk's terrain mesh — all pure
/// CPU work, safe to run on a background thread.
fn probe_and_carve(
    cam_pos: Vec3,
    forward: Vec3,
    records: &[(IVec2, u64, std::collections::HashSet<IVec3>)],
) -> Option<BurrowResult> {
    let mut voxel_cache: HashMap<IVec2, ChunkVoxels> = HashMap::new();
    let mut hit: Option<(IVec2, IVec3, BlockType)> = None;

    'rays: for dir in [forward, Vec3::NEG_Y] {
        for step in 0..16 {
            let p = cam_pos + dir * (step as f32 * VOXEL_SIZE * 0.5);
            let vx = (p.x / VOXEL_SIZE).floor() as i32;
            let vy = (p.y / VOXEL_SIZE).floor() as i32;
            let vz = (p.z / VOXEL_SIZE).floor() as i32;
            let coord = IVec2::new(vx.div_euclid(CHUNK_VOXELS), vz.div_euclid(CHUNK_VOXELS));
            let Some((_, seed, edits)) = records.iter().find(|(c, _, _)| *c == coord) else {
                continue;
            };

            let voxels = voxel_cache.entry(coord).or_insert_with(|| {
                let mut v = generate_chunk_blocks(coord, chunk_world_origin(coord), *seed);
                apply_edits(&mut v, edits);
                v
            });

            let local = IVec3::new(vx.rem_euclid(CHUNK_VOXELS), vy, vz.rem_euclid(CHUNK_VOXELS));
            if let Some(block) = voxels.get(local.x, local.y, local.z) {
                // Water isn't food, and the bottom layer is bedrock.
                if block != BlockType::Water && local.y > voxels.floor_y() {
                    hit = Some((coord, local, block));
                }
                break 'rays;
            }
        }
    }

    let (coord, local, block) = hit?;
    let mut voxels = voxel_cache.remove(&coord).expect("probed chunk is cached");
    voxels.clear_cell(local.x, local.y, local.z);

    Some(BurrowResult {
        coord,
        local,
        block,
        mesh: terrain::build_colored_terrain_mesh(&voxels),
    })
}

/// Land finished bites: record the edit, swap the chunk's terrain mesh, munch.
fn finish_burrow_tasks(
    mut commands: Commands,
    sounds: Res<GameSounds>,
    mut chunk_world: ResMut<ChunkWorld>,
    mut archive: ResMut<ChunkArchive>,
    mut meshes: ResMut<Assets<Mesh>>,
    terrain_materials: Res<TerrainMaterials>,
    mut pending_q: Query<(Entity, &mut PendingBurrow)>,
    children_q: Query<&Children>,
    surface_q: Query<(), With<TerrainSurface>>,
) {
    for (holder, mut pending) in &mut pending_q {
        let Some(result) = block_on(future::poll_once(&mut pending.task)) else {
            continue;
        };
        commands.entity(holder).despawn();

        let Some(result) = result else {
            println!("Nothing edible in range (get near a leaf or up against soil + press E)");
            continue;
        };

        // Persist the bite wherever the chunk lives now (it may have streamed
        // out mid-chew — the tunnel still has to be there on revisit).
        if let Some(record) = chunk_world.active_records.get_mut(&result.coord) {
            record.edits.insert(result.local);
        } else if let Some(record) = archive.saved.get_mut(&result.coord) {
            record.edits.insert(result.local);
            continue; // No live mesh to swap.
        } else {
            continue;
        }

        let Some(&chunk_entity) = chunk_world.loaded.get(&result.coord) else {
            continue;
        };
        if let Ok(children) = children_q.get(chunk_entity) {
            for child in children {
                if surface_q.get(*child).is_ok() {
                    commands.entity(*child).despawn();
                }
            }
        }
        let mesh = meshes.add(result.mesh);
        commands.entity(chunk_entity).with_children(|chunk| {
            chunk.spawn((
                Mesh3d(mesh),
                MeshMaterial3d(terrain_materials.vertex_color_terrain.clone()),
                Transform::IDENTITY,
                TerrainSurface,
            ));
        });

        commands.spawn((
            AudioPlayer::new(sounds.munch.clone()),
            PlaybackSettings::DESPAWN,
        ));
        println!("🪱 Burrowed through a mouthful of {:?}.", result.block);
    }
}

/// Animates the floating 3D leaves (now with real thickness).
/// They bob and spin; the small extrusion gives them volume and edge highlights.
fn animate_floating_leaves(
    time: Res<Time>,
    mut query: Query<(&mut Transform, &FloatingLeaf)>,
) {
    let t = time.elapsed_secs();

    for (mut transform, floating) in &mut query {
        // Gentle vertical bob
        let bob = (t * floating.bob_speed + floating.phase).sin() * 0.20;
        transform.translation.y = floating.base_y + bob;

        // Spin around Y
        let spin = Quat::from_rotation_y(t * floating.spin_speed + floating.phase * 0.5);

        // Combine:
        // - The artistic base rotation the leaf was given at spawn
        // - Y spin for rotation
        // - Strong vertical orientation so the plane stands up instead of lying flat
        let vertical_stand = Quat::from_rotation_x(-1.4);
        transform.rotation = spin * vertical_stand * floating.base_rotation;
    }
}

/// Places the flycam at world origin (the chosen spawn biome) at worm eye level,
/// just above the surface voxel so you start crawling on the ground rather than
/// clipped inside it. Also dresses the camera with distance fog so faraway
/// giants haze out into the sky instead of rendering pin-sharp.
fn lower_worm_camera(
    mut commands: Commands,
    mut query: Query<(Entity, &mut Transform), With<Camera>>,
) {
    // Top of the local surface voxel + a worm's eye height above it.
    let surface_top = surface_top_world_y(0.0, 0.0);
    let mut eye_y = surface_top + WORM_EYE_HEIGHT;

    // Debug only: GARDN_HIGH=<feet> starts the camera up in the air, for
    // inspecting distant terrain/silhouette rendering without a slow climb.
    // Requires an explicit number — a stray/empty value changes nothing, so a
    // normal launch always starts on the ground.
    if let Ok(alt) = std::env::var("GARDN_HIGH") {
        if let Ok(feet) = alt.trim().parse::<f32>() {
            println!("🔧 GARDN_HIGH debug: starting the camera {feet} ft up.");
            eye_y += feet;
        }
    }

    for (entity, mut transform) in &mut query {
        transform.translation.x = 0.0;
        transform.translation.z = 0.0;
        transform.translation.y = eye_y;

        commands.entity(entity).insert(DistanceFog {
            // Matches the clear colour so fogged geometry melts into the sky.
            color: Color::srgb(0.58, 0.72, 0.88),
            directional_light_color: Color::srgba(1.0, 0.95, 0.85, 0.6),
            directional_light_exponent: 40.0,
            falloff: FogFalloff::Linear {
                start: 120.0,
                end: 680.0,
            },
        });
    }
}

/// Creates a slightly extruded 3D leaf mesh whose silhouette *exactly* follows
/// the opaque contours of the higher-res 8-bit leaf (assets/leaf.png).
/// We trace per-row min/max opaque pixels, then rectify the polyline to pure
/// horizontal+vertical segments so the outline (and extruded side walls) are
/// chunky 8-bit jagged, exactly following the pixel steps of the art. Then
/// extrude a tiny bit for thickness. The result is a cool retro low-poly 3D leaf.
///
/// The leaf face lies in the X/Z plane (matching old Plane3d) + thickness on Y
/// so all the existing bob/spin/base rotations continue to work unchanged.
fn create_extruded_leaf_mesh(meshes: &mut ResMut<Assets<Mesh>>) -> Handle<Mesh> {
    // Embed the source PNG so the mesh shape is derived directly from it at
    // compile time (change the PNG and rebuild to update the 3D outline).
    const LEAF_PNG: &[u8] = include_bytes!("../assets/leaf.png");
    let img = image::load_from_memory_with_format(LEAF_PNG, image::ImageFormat::Png)
        .expect("Failed to decode embedded assets/leaf.png for 3D leaf contour")
        .to_rgba8();

    let (w, h) = img.dimensions();
    let alpha_threshold: u8 = 128;

    // Build per-row spans of opaque pixels (only rows that have any leaf).
    // This captures the exact left/right silhouette at every scanline.
    let mut row_spans: Vec<(u32, u32, u32)> = Vec::new(); // (y, left, right)
    let mut good_u = 0.5f32;
    let mut good_v = 0.5f32;
    let mut found_green = false;
    for y in 0..h {
        let mut left = None::<u32>;
        let mut right = None::<u32>;
        for x in 0..w {
            let p = img.get_pixel(x, y);
            if p[3] > alpha_threshold && p[1] > p[0] && p[1] > p[2] && p[1] > 100 {
                // only bright green pixels for the silhouette (ignore black outline/detail)
                if !found_green {
                    good_u = x as f32 / w as f32;
                    good_v = y as f32 / h as f32;
                    found_green = true;
                }
                if left.is_none() {
                    left = Some(x);
                }
                right = Some(x);
            }
        }
        if let (Some(l), Some(r)) = (left, right) {
            row_spans.push((y, l, r));
        }
    }

    // For top and bottom, compute mid u to make them pointed (remove flat horizontal lines at top/base).
    // Use pixel centers for accurate mapping to green texels.
    let top_l_u = if !row_spans.is_empty() { (row_spans[0].1 as f32 + 0.5) / w as f32 } else { 0.5 };
    let top_r_u = if !row_spans.is_empty() { (row_spans[0].2 as f32 + 0.5) / w as f32 } else { 0.5 };
    let top_mid_u = (top_l_u + top_r_u) / 2.0;
    let bot_l_u = if !row_spans.is_empty() { (row_spans.last().unwrap().1 as f32 + 0.5) / w as f32 } else { 0.5 };
    let bot_r_u = if !row_spans.is_empty() { (row_spans.last().unwrap().2 as f32 + 0.5) / w as f32 } else { 0.5 };
    let bot_mid_u = (bot_l_u + bot_r_u) / 2.0;

    // Decide whether to force pointed tips: only for narrow end rows (true tips in raster).
    // Wide ends (like this PNG top~65px, bot~98px) keep their natural flat-ish contour width.
    let top_pix_width = if !row_spans.is_empty() { row_spans[0].2 as i32 - row_spans[0].1 as i32 + 1 } else { 0 };
    let bot_pix_width = if !row_spans.is_empty() { row_spans.last().unwrap().2 as i32 - row_spans.last().unwrap().1 as i32 + 1 } else { 0 };
    let point_top = top_pix_width < 12;
    let point_bot = bot_pix_width < 12;

    // Compute the actual content bounding box in UV (so we can map *just* the leaf
    // pixels to a nice world size without the PNG's transparent margins). Use centers.
    let min_u = row_spans.iter().map(|&(_, l, _)| (l as f32 + 0.5) / w as f32).fold(f32::INFINITY, f32::min);
    let max_u = row_spans.iter().map(|&(_, _, r)| (r as f32 + 0.5) / w as f32).fold(f32::NEG_INFINITY, f32::max);
    let min_v = row_spans.first().map(|(y, _, _)| *y as f32 / h as f32).unwrap_or(0.0);
    let max_v = row_spans.last().map(|(y, _, _)| *y as f32 / h as f32).unwrap_or(1.0);
    let span_u = (max_u - min_u).max(0.0001);
    let span_v = (max_v - min_v).max(0.0001);
    let center_u = (min_u + max_u) * 0.5;
    let center_v = (min_v + max_v) * 0.5;

    // Map the content bbox with aspect preservation so the elongated
    // high-res leaf keeps its natural proportions.
    let max_dim = 0.95;
    let (desired_w, desired_h) = if span_u >= span_v {
        (max_dim, max_dim * (span_v / span_u))
    } else {
        (max_dim * (span_u / span_v), max_dim)
    };

    // Build the boundary polygon in texture UV space (0..1).
    // Left chain top-to-bottom, only adding a point when the column actually changes
    // (keeps key silhouette corners/steps, drops redundant points on vertical runs).
    // Use pixel centers so nearest sampling + small inset lands on green not border.
    let mut left_chain: Vec<[f32; 2]> = Vec::new(); // [u, v] tex
    for &(y, l, _) in &row_spans {
        let u = (l as f32 + 0.5) / w as f32;
        let v = y as f32 / h as f32;
        left_chain.push([u, v]);
    }

    let mut right_chain: Vec<[f32; 2]> = Vec::new();
    for &(y, _, r) in &row_spans {
        let u = (r as f32 + 0.5) / w as f32; // center of the rightmost green pixel
        let v = y as f32 / h as f32;
        right_chain.push([u, v]);
    }

    // Make the chains "jagged 8-bit" by turning any diagonal connections into
    // explicit horizontal + vertical segments. This makes the mesh outline
    // and extruded side walls follow the pixel steps exactly, for a cool retro
    // chunky look instead of smoothed diagonals.
    fn rectify_to_axis_aligned(chain: Vec<[f32; 2]>) -> Vec<[f32; 2]> {
        if chain.len() < 2 {
            return chain;
        }
        let mut result: Vec<[f32; 2]> = vec![chain[0]];
        for pt in chain.into_iter().skip(1) {
            let prev = *result.last().unwrap();
            let du = pt[0] - prev[0];
            let dv = pt[1] - prev[1];
            if du.abs() > 1e-5 && dv.abs() > 1e-5 {
                // Insert an axis-aligned corner to keep pure H/V edges.
                // For left chain (going down image): 
                //   - if stepping right (du>0, narrowing), vertical first then horiz
                //   - if stepping left (du<0, widening), horiz first then vertical
                // For right chain we apply similar logic (direction is same top->bottom).
                if du > 0.0 {
                    result.push([prev[0], pt[1]]);
                } else {
                    result.push([pt[0], prev[1]]);
                }
                result.push(pt);
            } else {
                result.push(pt);
            }
        }
        result
    }

    let mut left_chain = rectify_to_axis_aligned(left_chain);
    let mut right_chain = rectify_to_axis_aligned(right_chain);

    // Collapse top/bottom to mid only for narrow tips (point_top/point_bot).
    // Wide ends keep full left/right at the end row so the silhouette follows the PNG's actual end contours.
    // For wide ends we apply strong UV inset (v + lateral) + geom pull on the end bar verts to ensure
    // the perimeter edge samples deep inner green (no black line or rim). Full thickness everywhere.
    if !left_chain.is_empty() && !right_chain.is_empty() {
        if point_top {
            let top_v = left_chain[0][1];
            left_chain[0] = [top_mid_u, top_v];
            right_chain[0] = [top_mid_u, top_v];
        }
        if point_bot {
            let last = left_chain.len() - 1;
            let bot_v = left_chain[last][1];
            left_chain[last] = [bot_mid_u, bot_v];
            right_chain[last] = [bot_mid_u, bot_v];
        }
    }
    let left_chain = rectify_to_axis_aligned(left_chain);
    let right_chain = rectify_to_axis_aligned(right_chain);

    // Build unique boundary points from left and right chains (sharing tip points if pointed).
    // Also record indices into this boundary list for left and right (top to bottom).
    let mut boundary: Vec<[f32; 2]> = vec![];
    let mut left_idx: Vec<usize> = vec![];
    let mut right_idx: Vec<usize> = vec![];

    fn get_or_add(b: &mut Vec<[f32; 2]>, p: [f32; 2]) -> usize {
        if let Some(i) = b.iter().position(|&q| (q[0] - p[0]).abs() < 1e-5 && (q[1] - p[1]).abs() < 1e-5) {
            i
        } else {
            let i = b.len();
            b.push(p);
            i
        }
    }

    for &p in &left_chain {
        left_idx.push(get_or_add(&mut boundary, p));
    }
    for &p in &right_chain {
        right_idx.push(get_or_add(&mut boundary, p));
    }

    // Build the closed perimeter order (for side walls): left (top->bottom) + rev(right) (bottom->top)
    let mut perim_order: Vec<usize> = left_idx.clone();
    let mut rev_r: Vec<usize> = right_idx.clone();
    rev_r.reverse();
    perim_order.extend(rev_r);

    // Clean duplicates at closure and consecutive (pointed tips)
    if perim_order.len() > 1 && perim_order[0] == perim_order[perim_order.len() - 1] {
        perim_order.pop();
    }
    let mut i = 0usize;
    while i + 1 < perim_order.len() {
        if perim_order[i] == perim_order[i + 1] {
            perim_order.remove(i + 1);
        } else {
            i += 1;
        }
    }

    // Map from boundary index (in left_idx/right_idx) to position in perim_order (so cap_tris use correct 0..n-1 mesh indices)
    let mut boundary_to_perim: Vec<usize> = vec![0; boundary.len()];
    for (pos, &bidx) in perim_order.iter().enumerate() {
        boundary_to_perim[bidx] = pos;
    }

    // Build cap triangulation as a strip between left and right chains (local triangles, better texture fidelity than one big fan from center).
    // This avoids large spanning triangles at top/bottom that cause visible lines or warping.
    let mut cap_tris: Vec<usize> = vec![];
    let mut li = 0usize;
    let mut ri = 0usize;
    while li < left_idx.len() - 1 || ri < right_idx.len() - 1 {
        let left_next_v = if li + 1 < left_idx.len() { left_chain[li + 1][1] } else { f32::INFINITY };
        let right_next_v = if ri + 1 < right_idx.len() { right_chain[ri + 1][1] } else { f32::INFINITY };
        if li < left_idx.len() - 1 && left_next_v <= right_next_v {
            let a = boundary_to_perim[ left_idx[li] ];
            let b = boundary_to_perim[ right_idx[ri] ];
            let c = boundary_to_perim[ left_idx[li + 1] ];
            if a != b && a != c && b != c {
                cap_tris.push(a);
                cap_tris.push(b);
                cap_tris.push(c);
            }
            li += 1;
        } else if ri < right_idx.len() - 1 {
            let a = boundary_to_perim[ left_idx[li] ];
            let b = boundary_to_perim[ right_idx[ri] ];
            let c = boundary_to_perim[ right_idx[ri + 1] ];
            if a != b && a != c && b != c {
                cap_tris.push(a);
                cap_tris.push(b);
                cap_tris.push(c);
            }
            ri += 1;
        }
    }

    // Now build ordered perimeter geometry + UVs from perim_order (boundary indices map 1:1 to 0..n-1)
    let mut outline_2d: Vec<[f32; 2]> = vec![];
    let mut perim_uv: Vec<[f32; 2]> = vec![];
    let mut y_fronts: Vec<f32> = vec![];
    let mut y_backs: Vec<f32> = vec![];
    let mut orig_vs: Vec<f32> = vec![];
    for &bidx in &perim_order {
        let [u, v] = boundary[bidx];
        let orig_v = v;
        let is_top = (orig_v - min_v).abs() < 1e-4;
        let is_bot = (orig_v - max_v).abs() < 1e-4;
        // Full thickness everywhere (including wide end bars) for uniform "coffee coaster" look.
        // "No rim/line on silhouette" is achieved via UV inset (edge samples inner green) + nearest filter
        // + geom pull on end bars. Side walls are flat green (side_uv), which is "just the green".
        let yf = 0.011;
        let yb = -0.011;
        // For position: slightly inset top and bottom points inward in x (narrower) and z (shorter)
        // to cut the flat top and base lines.
        let mut calc_u = u;
        let mut calc_v = v;
        if is_top && point_top {
            calc_u = top_mid_u;
            calc_v = orig_v + 0.02;
        }
        if is_bot && point_bot {
            calc_u = bot_mid_u;
            calc_v = orig_v - 0.01;
        }
        // For wide ends (not pointing), still slightly pull the end bar's L/R points inward
        // in model space (narrows the extreme top/bot bar a tad) to help eliminate flat line look.
        if is_top && !point_top {
            let du = center_u - u;
            calc_u = u + du * 0.025;
            calc_v = orig_v + 0.005;
        }
        if is_bot && !point_bot {
            let du = center_u - u;
            calc_u = u + du * 0.025;
            calc_v = orig_v - 0.005;
        }
        let x = (calc_u - center_u) / span_u * desired_w;
        let z = (center_v - calc_v) / span_v * desired_h;
        outline_2d.push([x, z]);
        // For UV: inset toward center. With nearest sampling (ImagePlugin::default_nearest) a small inset
        // (5px sides) ensures the silhouette edge samples solid green (the art uses (21,255,0) right to the
        // green-filtered boundary). Wide end bars get large v inset (20px top /12px bot) + 10px lateral u inset
        // so the top/bottom perimeter edges are deep inner green. Geom pull on end bars + full thickness helps
        // avoid flat/rim artifacts. Keeps art faithful overall.
        let pu;
        let pv;
        if is_top {
            pv = orig_v + (if point_top { 6.0 } else { 20.0 }) / h as f32;
            let mut puu = u;
            if !point_top {
                // additionally inset u toward center for the wide top bar verts, to clear black details near the upper sides/corners
                // Use fixed pixel shift (not scaled by distance to center)
                let du = center_u - u;
                let len = du.abs().max(1e-6);
                puu = (u + (du / len) * (10.0 / w as f32)).clamp(0.0, 1.0);
            }
            pu = if point_top { top_mid_u } else { puu };
        } else if is_bot {
            pv = orig_v - (if point_bot { 5.0 } else { 12.0 }) / h as f32;
            let mut puu = u;
            if !point_bot {
                let du = center_u - u;
                let len = du.abs().max(1e-6);
                puu = (u + (du / len) * (10.0 / w as f32)).clamp(0.0, 1.0);
            }
            pu = if point_bot { bot_mid_u } else { puu };
        } else {
            let du = center_u - u;
            let dv = center_v - v;
            let len = (du * du + dv * dv).sqrt().max(1e-6);
            let inset = 5.0 / h as f32;
            pu = (u + du / len * inset).clamp(0.0, 1.0);
            pv = (v + dv / len * inset).clamp(0.0, 1.0);
        }
        perim_uv.push([pu, pv]);
        y_fronts.push(yf);
        y_backs.push(yb);
        orig_vs.push(orig_v);
    }
    let n = perim_order.len();

    // Precompute outward side normals (X/Z plane). We negate because the trace
    // order from the row walk ends up CW when viewed from +Y.
    let mut side_normals: Vec<[f32; 3]> = Vec::with_capacity(n);
    for i in 0..n {
        let [x0, z0] = outline_2d[i];
        let [x1, z1] = outline_2d[(i + 1) % n];
        let dx = x1 - x0;
        let dz = z1 - z0;
        let len = (dx * dx + dz * dz).sqrt().max(0.0001);
        let nx = dz / len;
        let nz = -dx / len;
        side_normals.push([-nx, 0.0, -nz]);
    }

    let mut positions: Vec<[f32; 3]> = Vec::new();
    let mut normals: Vec<[f32; 3]> = Vec::new();
    let mut uvs: Vec<[f32; 2]> = Vec::new();

    // Front perimeter (cap) — use the *real* perim_uv from the PNG so texturing
    // matches the original sprite exactly on the 3D surface.
    for (i, &[x, z]) in outline_2d.iter().enumerate() {
        let orig_v = orig_vs[i];
        let mut zz = z;
        let is_top = (orig_v - min_v).abs() < 1e-4;
        let is_bot = (orig_v - max_v).abs() < 1e-4;
        if is_top {
            zz -= if point_top { 0.04 } else { 0.04 };
        }
        if is_bot {
            zz += if point_bot { 0.04 } else { 0.04 };
        }
        positions.push([x, y_fronts[i], zz]);
        normals.push([0.0, 1.0, 0.0]);
        uvs.push(perim_uv[i]);
    }

    // Back perimeter
    let back_perim_start = positions.len() as u32;
    for (i, &[x, z]) in outline_2d.iter().enumerate() {
        let orig_v = orig_vs[i];
        let mut zz = z;
        let is_top = (orig_v - min_v).abs() < 1e-4;
        let is_bot = (orig_v - max_v).abs() < 1e-4;
        if is_top {
            zz -= if point_top { 0.015 } else { 0.015 };
        }
        if is_bot {
            zz += if point_bot { 0.015 } else { 0.015 };
        }
        positions.push([x, y_backs[i], zz]);
        normals.push([0.0, -1.0, 0.0]);
        uvs.push(perim_uv[i]);
    }

    // Side wall verts (duplicated for hard 90° edges + correct normals)
    let side_top_start = positions.len() as u32;
    // Use a guaranteed green pixel UV for the rim (from the first green pixel found).
    // This ensures the extruded sides are "just the green", not black border.
    let side_uv = [good_u, good_v];
    for (i, &[x, z]) in outline_2d.iter().enumerate() {
        let y_top = y_fronts[i];
        positions.push([x, y_top, z]);
        normals.push(side_normals[i]);
        uvs.push(side_uv);
    }
    let side_bot_start = positions.len() as u32;
    for (i, &[x, z]) in outline_2d.iter().enumerate() {
        let y_bot = y_backs[i];
        positions.push([x, y_bot, z]);
        normals.push(side_normals[i]);
        uvs.push(side_uv);
    }

    let mut indices: Vec<u32> = Vec::new();

    // Front cap: strip triangulation between left and right chains (local tris for faithful texture).
    for t in 0..cap_tris.len() / 3 {
        let a = cap_tris[t * 3] as u32;
        let b = cap_tris[t * 3 + 1] as u32;
        let c = cap_tris[t * 3 + 2] as u32;
        indices.push(a);
        indices.push(b);
        indices.push(c);
    }

    // Back cap: same strip but reversed winding, offset to back perim verts.
    for t in 0..cap_tris.len() / 3 {
        let a = back_perim_start as u32 + cap_tris[t * 3] as u32;
        let b = back_perim_start as u32 + cap_tris[t * 3 + 1] as u32;
        let c = back_perim_start as u32 + cap_tris[t * 3 + 2] as u32;
        indices.push(a);
        indices.push(c);
        indices.push(b);
    }

    // Side wall quads
    for i in 0..n {
        let f0 = side_top_start + (i as u32);
        let f1 = side_top_start + (((i + 1) % n) as u32);
        let b0 = side_bot_start + (i as u32);
        let b1 = side_bot_start + (((i + 1) % n) as u32);

        indices.push(f0);
        indices.push(f1);
        indices.push(b1);
        indices.push(f0);
        indices.push(b1);
        indices.push(b0);
    }

    let mut mesh = Mesh::new(
        PrimitiveTopology::TriangleList,
        RenderAssetUsages::default(),
    );
    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, positions);
    mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, normals);
    mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, uvs);
    mesh.insert_indices(Indices::U32(indices));

    meshes.add(mesh)
}
