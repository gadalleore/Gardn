mod australia;
mod chunk_store;
mod map_ui;
mod terrain;
mod world;

use bevy::prelude::*;
use bevy::asset::RenderAssetUsages;
use bevy::render::mesh::{Indices, PrimitiveTopology};
use bevy::render::RenderPlugin;
use bevy::render::settings::{RenderCreation, WgpuSettings};
use bevy::render::texture::ImagePlugin;
use bevy_flycam::prelude::*;
use std::collections::{HashMap, HashSet, VecDeque};

use chunk_store::{
    archive_chunk, take_saved_chunk, ChunkArchive, ChunkRecord, ChunkTreeJob, SavedTreeSpecies,
};
use map_ui::{setup_map_ui, toggle_map_ui, update_map_ui, MapOverlay};
use terrain::{
    build_culled_voxel_mesh, generate_chunk_blocks, spawn_terrain_meshes, TerrainMaterials,
};
use world::*;

/// ~2 ft trunk diameter — enormous next to a 3-inch worm.
const TRUNK_RADIUS_INCHES: i32 = 12;
const TRUNK_RADIUS_VOXELS: i32 = TRUNK_RADIUS_INCHES / VOXEL_INCHES;
const FOLIAGE_BLOB_INCHES: i32 = 8;
const FOLIAGE_BLOB_VOXELS: i32 = FOLIAGE_BLOB_INCHES / VOXEL_INCHES;
const FOLIAGE_BLOB_WORLD_SIZE: f32 = FOLIAGE_BLOB_VOXELS as f32 * VOXEL_SIZE;
const MAX_BRANCH_DROOP_RATIO: f32 = 0.7;

#[derive(Component)]
struct Leaf;

#[derive(Component)]
struct EucalyptusTree;

#[derive(Component)]
struct AcaciaTree;

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

#[derive(Resource)]
struct TreeSpawnBudget {
    timer: Timer,
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
        .init_resource::<MapOverlay>()
        .add_systems(Startup, (setup_garden, init_tree_spawn_budget, setup_map_ui))
        .add_systems(
            Update,
            (
                plan_chunk_streaming,
                process_chunk_load_queue,
                process_tree_spawn_queue,
                toggle_map_ui,
                update_map_ui,
                eat_leaves,
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
    spawn_textured_leaves(&mut commands, &mut meshes, &mut materials, &asset_server);

    // Sun light
    commands.spawn((
        DirectionalLight {
            illuminance: 11_000.0,
            shadows_enabled: false,
            ..default()
        },
        Transform::from_rotation(Quat::from_euler(EulerRot::XYZ, -0.9, 0.7, 0.0)),
    ));

    commands.insert_resource(AmbientLight {
        color: Color::srgb(0.82, 0.88, 0.95),
        brightness: 70.0,
    });
}

fn init_tree_spawn_budget(mut commands: Commands) {
    commands.insert_resource(TreeSpawnBudget {
        timer: Timer::from_seconds(TREE_SPAWN_INTERVAL_SECS, TimerMode::Repeating),
    });
}

struct VoxelTreeData {
    bark: HashSet<IVec3>,
    foliage: HashSet<IVec3>,
}

fn trunk_centroid_at_y(trunk: &HashSet<IVec3>, y: i32) -> IVec3 {
    let mut count = 0i32;
    let mut cx = 0i32;
    let mut cz = 0i32;
    for p in trunk.iter().filter(|p| p.y == y) {
        cx += p.x;
        cz += p.z;
        count += 1;
    }
    if count == 0 {
        return IVec3::ZERO;
    }
    IVec3::new(cx / count, y, cz / count)
}

/// Tall, mostly straight trunk — bare for the lower ~60% like real eucalyptus.
fn generate_eucalyptus_trunk(rng: &mut GardenRng) -> HashSet<IVec3> {
    let height_feet = rng.range_i(50, 80);
    let height_voxels = height_feet * VOXELS_PER_FOOT;
    let radius = TRUNK_RADIUS_VOXELS;
    let radius_sq = radius * radius;

    let mut center_x = 0i32;
    let mut center_z = 0i32;
    let mut trunk = HashSet::new();

    for y in 0..height_voxels {
        if y > 0 {
            if rng.chance(0.05) {
                center_x += rng.choice_i(&[-1, 0, 1]);
            }
            if rng.chance(0.05) {
                center_z += rng.choice_i(&[-1, 0, 1]);
            }
            center_x = center_x.clamp(-2, 2);
            center_z = center_z.clamp(-2, 2);
        }

        for dx in -radius..=radius {
            for dz in -radius..=radius {
                if dx * dx + dz * dz <= radius_sq {
                    trunk.insert(IVec3::new(center_x + dx, y, center_z + dz));
                }
            }
        }
    }

    trunk
}

/// Upward-biased branch direction with limited droop below horizontal.
fn sample_branch_direction(rng: &mut GardenRng, outward: Vec3) -> Vec3 {
    let azimuth = rng.range(0.0, std::f32::consts::TAU);
    let elev_deg = if rng.chance(0.85) {
        rng.range(12.0, 72.0)
    } else {
        rng.range(-28.0, 18.0)
    };
    let elev = elev_deg.to_radians();

    let mut dir = Vec3::new(
        elev.cos() * azimuth.cos(),
        elev.sin(),
        elev.cos() * azimuth.sin(),
    );

    if dir.y < 0.0 {
        let horizontal = Vec2::new(dir.x, dir.z).length().max(0.05);
        if dir.y.abs() > MAX_BRANCH_DROOP_RATIO * horizontal {
            dir.y = -MAX_BRANCH_DROOP_RATIO * horizontal;
        }
    }

    if outward.length_squared() > 0.01 {
        dir = (dir + outward * rng.range(0.25, 0.55)).normalize();
    } else {
        dir = dir.normalize();
    }

    dir
}

fn rasterize_branch(start: IVec3, dir: Vec3, length_voxels: i32) -> Vec<IVec3> {
    let mut blocks = Vec::with_capacity(length_voxels as usize);
    let mut pos = Vec3::new(
        start.x as f32 + 0.5,
        start.y as f32 + 0.5,
        start.z as f32 + 0.5,
    );
    let step = dir.normalize() * 0.55;
    let mut prev = start;
    blocks.push(start);

    for _ in 0..length_voxels {
        pos += step;
        let grid = IVec3::new(
            pos.x.floor() as i32,
            pos.y.floor() as i32,
            pos.z.floor() as i32,
        );
        if grid != prev {
            blocks.push(grid);
            prev = grid;
        }
    }

    blocks
}

fn voxel_to_blob_coord(voxel: IVec3) -> IVec3 {
    IVec3::new(
        voxel.x.div_euclid(FOLIAGE_BLOB_VOXELS),
        voxel.y.div_euclid(FOLIAGE_BLOB_VOXELS),
        voxel.z.div_euclid(FOLIAGE_BLOB_VOXELS),
    )
}

/// Pom-pom leaf cloud at a branch tip — large, rounded, slightly irregular.
fn generate_branch_tip_foliage(rng: &mut GardenRng, tip: IVec3) -> HashSet<IVec3> {
    let center = voxel_to_blob_coord(tip)
        + IVec3::new(
            rng.range_i(-1, 1),
            rng.range_i(0, 2),
            rng.range_i(-1, 1),
        );

    let mut blobs = HashSet::new();
    let radius_x = rng.range(3.2, 5.8);
    let radius_y = rng.range(2.4, 4.2);
    let radius_z = rng.range(3.2, 5.8);
    let puffiness = rng.range(0.68, 0.92);

    for dx in -7..=7 {
        for dy in -2..=7 {
            for dz in -7..=7 {
                let p = center + IVec3::new(dx, dy, dz);
                let nx = dx as f32 / radius_x;
                let ny = dy as f32 / radius_y;
                let nz = dz as f32 / radius_z;
                let dist_sq = nx * nx + ny * ny + nz * nz;
                if dist_sq > 1.0 {
                    continue;
                }

                let edge_softness = 1.0 - dist_sq;
                if rng.chance(edge_softness * puffiness) {
                    blobs.insert(p);
                }
            }
        }
    }

    blobs
}

/// Semi-random upward branches from the upper trunk; leaves only at tips.
fn generate_eucalyptus_branches(
    rng: &mut GardenRng,
    trunk: &HashSet<IVec3>,
) -> (HashSet<IVec3>, HashSet<IVec3>) {
    let min_y = trunk.iter().map(|p| p.y).min().unwrap_or(0);
    let max_y = trunk.iter().map(|p| p.y).max().unwrap_or(0);
    let trunk_height = max_y - min_y;
    let branch_zone_start = min_y + (trunk_height as f32 * 0.58) as i32;

    let mut branches = HashSet::new();
    let mut foliage = HashSet::new();
    let branch_count = rng.range_i(5, 9);

    for _ in 0..branch_count {
        let attach_y = rng.range_i(branch_zone_start, max_y.saturating_sub(2));
        let ring: Vec<IVec3> = trunk
            .iter()
            .copied()
            .filter(|p| p.y == attach_y)
            .collect();
        if ring.is_empty() {
            continue;
        }

        let start = ring[(rng.next_f32() * ring.len() as f32).floor() as usize];
        let center = trunk_centroid_at_y(trunk, attach_y);
        let outward = Vec3::new(
            (start.x - center.x) as f32,
            0.0,
            (start.z - center.z) as f32,
        )
        .normalize_or_zero();

        let dir = sample_branch_direction(rng, outward);
        let length_voxels = rng.range_i(28, 96) / VOXEL_INCHES;
        let path = rasterize_branch(start, dir, length_voxels);

        let mut tip = None;
        for block in path {
            if !trunk.contains(&block) {
                branches.insert(block);
                tip = Some(block);
            }
        }

        if let Some(tip_pos) = tip {
            for blob in generate_branch_tip_foliage(rng, tip_pos) {
                foliage.insert(blob);
            }
        }
    }

    (branches, foliage)
}

fn generate_eucalyptus_tree(rng: &mut GardenRng) -> VoxelTreeData {
    let trunk = generate_eucalyptus_trunk(rng);
    let (branches, foliage) = generate_eucalyptus_branches(rng, &trunk);
    let mut bark = trunk;
    bark.extend(branches);
    VoxelTreeData { bark, foliage }
}

/// Spreading, multi-stemmed trunk — bushier and more gnarled than eucalyptus.
fn generate_acacia_trunk(rng: &mut GardenRng) -> HashSet<IVec3> {
    let height_feet = rng.range_i(40, 70);
    let height_voxels = height_feet * VOXELS_PER_FOOT;
    let radius = TRUNK_RADIUS_VOXELS;

    let mut center_x = 0i32;
    let mut center_z = 0i32;
    let mut trunk = HashSet::new();

    for y in 0..height_voxels {
        if y > 0 {
            if rng.chance(0.12) {
                center_x += rng.choice_i(&[-1, 0, 1]);
            }
            if rng.chance(0.12) {
                center_z += rng.choice_i(&[-1, 0, 1]);
            }
            center_x = center_x.clamp(-4, 4);
            center_z = center_z.clamp(-4, 4);
        }

        let taper = if y > height_voxels * 3 / 4 {
            let t = (y - height_voxels * 3 / 4) as f32 / (height_voxels / 4).max(1) as f32;
            (radius as f32 * (1.0 - t * 0.45)).max(1.0) as i32
        } else {
            radius
        };
        let taper_sq = taper * taper;

        for dx in -taper..=taper {
            for dz in -taper..=taper {
                if dx * dx + dz * dz <= taper_sq {
                    trunk.insert(IVec3::new(center_x + dx, y, center_z + dz));
                }
            }
        }
    }

    // Fork into 2–3 main stems near the crown — classic wattle silhouette.
    let fork_count = rng.range_i(2, 3);
    let fork_start = height_voxels * 7 / 10;
    for _ in 0..fork_count {
        let mut fx = center_x;
        let mut fz = center_z;
        let fork_dx = rng.choice_i(&[-1, 1]);
        let fork_dz = rng.choice_i(&[-1, 1]);
        let fork_len = rng.range_i(18, 42) / VOXEL_INCHES;

        for i in 0..fork_len {
            let y = fork_start + i;
            fx += if i > 0 && rng.chance(0.35) { fork_dx.signum() } else { 0 };
            fz += if i > 0 && rng.chance(0.35) { fork_dz.signum() } else { 0 };

            let stem_r = rng.range_i(1, 2);
            let stem_r_sq = stem_r * stem_r;
            for dx in -stem_r..=stem_r {
                for dz in -stem_r..=stem_r {
                    if dx * dx + dz * dz <= stem_r_sq {
                        trunk.insert(IVec3::new(fx + dx, y, fz + dz));
                    }
                }
            }
        }
    }

    trunk
}

/// Outward-spreading branches with foliage along the crown, not just at tips.
fn sample_acacia_branch_direction(rng: &mut GardenRng, outward: Vec3) -> Vec3 {
    let azimuth = rng.range(0.0, std::f32::consts::TAU);
    let elev_deg = if rng.chance(0.75) {
        rng.range(4.0, 48.0)
    } else {
        rng.range(-18.0, 22.0)
    };
    let elev = elev_deg.to_radians();

    let mut dir = Vec3::new(
        elev.cos() * azimuth.cos(),
        elev.sin(),
        elev.cos() * azimuth.sin(),
    );

    if dir.y < 0.0 {
        let horizontal = Vec2::new(dir.x, dir.z).length().max(0.05);
        if dir.y.abs() > MAX_BRANCH_DROOP_RATIO * horizontal {
            dir.y = -MAX_BRANCH_DROOP_RATIO * horizontal;
        }
    }

    if outward.length_squared() > 0.01 {
        dir = (dir + outward * rng.range(0.45, 0.75)).normalize();
    } else {
        dir = dir.normalize();
    }

    dir
}

fn generate_acacia_branches(
    rng: &mut GardenRng,
    trunk: &HashSet<IVec3>,
) -> (HashSet<IVec3>, HashSet<IVec3>) {
    let min_y = trunk.iter().map(|p| p.y).min().unwrap_or(0);
    let max_y = trunk.iter().map(|p| p.y).max().unwrap_or(0);
    let trunk_height = max_y - min_y;
    let branch_zone_start = min_y + (trunk_height as f32 * 0.22) as i32;

    let mut branches = HashSet::new();
    let mut foliage = HashSet::new();
    let branch_count = rng.range_i(8, 14);

    for _ in 0..branch_count {
        let attach_y = rng.range_i(branch_zone_start, max_y.saturating_sub(1));
        let ring: Vec<IVec3> = trunk
            .iter()
            .copied()
            .filter(|p| p.y == attach_y)
            .collect();
        if ring.is_empty() {
            continue;
        }

        let start = ring[(rng.next_f32() * ring.len() as f32).floor() as usize];
        let center = trunk_centroid_at_y(trunk, attach_y);
        let outward = Vec3::new(
            (start.x - center.x) as f32,
            0.0,
            (start.z - center.z) as f32,
        )
        .normalize_or_zero();

        let dir = sample_acacia_branch_direction(rng, outward);
        let length_voxels = rng.range_i(20, 72) / VOXEL_INCHES;
        let path = rasterize_branch(start, dir, length_voxels);

        let mut tip = None;
        for (i, block) in path.iter().enumerate() {
            if !trunk.contains(block) {
                branches.insert(*block);
                tip = Some(*block);

                if i > 2 && rng.chance(0.22) {
                    for blob in generate_branch_tip_foliage(rng, *block) {
                        foliage.insert(blob);
                    }
                }
            }
        }

        if let Some(tip_pos) = tip {
            for blob in generate_branch_tip_foliage(rng, tip_pos) {
                foliage.insert(blob);
            }
        }
    }

    (branches, foliage)
}

fn generate_acacia_tree(rng: &mut GardenRng) -> VoxelTreeData {
    let trunk = generate_acacia_trunk(rng);
    let (branches, foliage) = generate_acacia_branches(rng, &trunk);
    let mut bark = trunk;
    bark.extend(branches);
    VoxelTreeData { bark, foliage }
}

fn spawn_voxel_tree(
    commands: &mut Commands,
    parent: Entity,
    meshes: &mut ResMut<Assets<Mesh>>,
    materials: &mut ResMut<Assets<StandardMaterial>>,
    rng: &mut GardenRng,
    local_base: Vec3,
    tree: VoxelTreeData,
    bark_color: (f32, f32, f32),
    foliage_color: (f32, f32, f32),
    marker: impl Bundle,
) {
    let bark_mesh = meshes.add(build_culled_voxel_mesh(&tree.bark, VOXEL_SIZE));
    let foliage_mesh = meshes.add(build_culled_voxel_mesh(&tree.foliage, FOLIAGE_BLOB_WORLD_SIZE));

    let bark_material = materials.add(StandardMaterial {
        base_color: Color::srgb(
            rng.range(bark_color.0 - 0.05, bark_color.0 + 0.05),
            rng.range(bark_color.1 - 0.05, bark_color.1 + 0.05),
            rng.range(bark_color.2 - 0.05, bark_color.2 + 0.05),
        ),
        ..default()
    });
    let foliage_material = materials.add(StandardMaterial {
        base_color: Color::srgb(
            rng.range(foliage_color.0 - 0.05, foliage_color.0 + 0.05),
            rng.range(foliage_color.1 - 0.05, foliage_color.1 + 0.05),
            rng.range(foliage_color.2 - 0.05, foliage_color.2 + 0.05),
        ),
        ..default()
    });

    commands.entity(parent).with_children(|trees| {
        trees
            .spawn((
                marker,
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

fn load_chunk(
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    terrain_materials: &TerrainMaterials,
    coord: IVec2,
    terrain_seed: u64,
) -> Entity {
    let origin = chunk_world_origin(coord);

    let chunk_entity = commands
        .spawn((
            WorldChunk { coord },
            Transform::from_translation(origin),
            Visibility::default(),
        ))
        .id();

    let blocks = generate_chunk_blocks(coord, origin, terrain_seed);
    spawn_terrain_meshes(commands, chunk_entity, meshes, terrain_materials, blocks);

    chunk_entity
}

fn process_tree_spawn_queue(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut tree_queue: ResMut<TreeSpawnQueue>,
    time: Res<Time>,
    mut budget: ResMut<TreeSpawnBudget>,
    live_chunks: Query<(), With<WorldChunk>>,
) {
    if !budget.timer.tick(time.delta()).just_finished() {
        return;
    }

    let Some(job) = tree_queue.0.pop_front() else {
        return;
    };

    if live_chunks.get(job.chunk_entity).is_err() {
        return;
    }

    let mut rng = GardenRng::new(job.tree_seed);
    match job.species {
        SavedTreeSpecies::Eucalyptus => {
            let tree = generate_eucalyptus_tree(&mut rng);
            spawn_voxel_tree(
                &mut commands,
                job.chunk_entity,
                &mut meshes,
                &mut materials,
                &mut rng,
                job.local_base,
                tree,
                job.bark_color,
                job.foliage_color,
                EucalyptusTree,
            );
        }
        SavedTreeSpecies::Acacia => {
            let tree = generate_acacia_tree(&mut rng);
            spawn_voxel_tree(
                &mut commands,
                job.chunk_entity,
                &mut meshes,
                &mut materials,
                &mut rng,
                job.local_base,
                tree,
                job.bark_color,
                job.foliage_color,
                AcaciaTree,
            );
        }
    }
}

fn despawn_entity_tree(entity: Entity, commands: &mut Commands) {
    commands.entity(entity).despawn();
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

        let entity = load_chunk(
            &mut commands,
            &mut meshes,
            &terrain_materials,
            coord,
            record.terrain_seed,
        );
        chunk_world.active_records.insert(coord, record.clone());
        tree_queue.0.extend(record.tree_jobs(entity));
        chunk_world.loaded.insert(coord, entity);
    }
}

/// Spawns your actual 8-bit leaf sprite now as true lightly-extruded 3D leaves.
/// Each has a small constant thickness (~coffee coaster) so the silhouette is
/// the real leaf shape (no more alpha card) and edges catch light when spinning.
fn spawn_textured_leaves(
    commands: &mut Commands,
    meshes: &mut ResMut<Assets<Mesh>>,
    materials: &mut ResMut<Assets<StandardMaterial>>,
    asset_server: &Res<AssetServer>,
) {
    // Load the high-res leaf PNG. The UV inset on boundary + green-only silhouette + strip triangulation
    // ensure the 2D art appears correct (just the green on edges, no mangled black lines, exact jagged outline).
    let leaf_texture = asset_server.load("leaf.png");

    let leaf_material = materials.add(StandardMaterial {
        base_color_texture: Some(leaf_texture),
        // The mesh geometry *is* the leaf outline now — no need for alpha cutout.
        // Opaque is cleanest + cheapest (the PNG color still gives the details/veins).
        alpha_mode: AlphaMode::Opaque,
        double_sided: true,
        ..default()
    });

    // One base mesh (fixed size); we scale instances via Transform so thickness scales too.
    let leaf_mesh = create_extruded_leaf_mesh(meshes);

    // Collectible ground leaves — bigger than the worm, at the original spawn heights.
    let leaf_scale = (WORM_LENGTH * 3.0) / 0.95;

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

        commands.spawn((
            Mesh3d(leaf_mesh.clone()),
            MeshMaterial3d(leaf_material.clone()),
            Transform {
                translation: *pos,
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

/// Little guy (the flycam) can eat leaves when close enough.
/// Fly near one and tap E. Simple distance check + despawn.
fn eat_leaves(
    mut commands: Commands,
    keyboard: Res<ButtonInput<KeyCode>>,
    cam_q: Query<&Transform, With<Camera>>,
    leaf_q: Query<(Entity, &Transform), With<Leaf>>,
) {
    if !keyboard.just_pressed(KeyCode::KeyE) {
        return;
    }

    let Ok(cam) = cam_q.get_single() else { return; };
    let cam_pos = cam.translation;

    let mut closest: Option<(Entity, f32)> = None;

    for (ent, tf) in &leaf_q {
        let d = cam_pos.distance(tf.translation);
        if d < 2.8 && closest.map_or(true, |(_, cd)| d < cd) {
            closest = Some((ent, d));
        }
    }

    if let Some((ent, d)) = closest {
        commands.entity(ent).despawn();
        println!("🍃 Yum! Little guy devoured a leaf (dist: {:.1})", d);
    } else {
        println!("No tasty leaf within eating range (fly closer + press E)");
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

/// Lowers the camera so you're viewing the world as a little worm crawling on the ground.
fn lower_worm_camera(
    mut query: Query<&mut Transform, With<Camera>>,
) {
    for mut transform in &mut query {
        // Default flycam starts quite high — bring it down to worm eye level (~1.5 inches).
        if transform.translation.y > WORM_EYE_HEIGHT * 4.0 {
            transform.translation.y = WORM_EYE_HEIGHT;
        }
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
