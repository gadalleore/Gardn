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
use bevy::core_pipeline::bloom::Bloom;
use bevy::pbr::{CascadeShadowConfigBuilder, DistanceFog, FogFalloff, NotShadowCaster};
use bevy::render::mesh::{Indices, PrimitiveTopology};
use bevy::render::RenderPlugin;
use bevy::render::settings::{RenderCreation, WgpuSettings};
use bevy::image::{ImageAddressMode, ImageSamplerDescriptor};
use bevy::render::texture::ImagePlugin;
use bevy::render::view::RenderLayers;
use bevy::tasks::futures_lite::future;
use bevy::tasks::{block_on, AsyncComputeTaskPool, Task};
use bevy_flycam::prelude::*;
use std::collections::{HashMap, HashSet, VecDeque};
use std::time::{SystemTime, UNIX_EPOCH};

use australia::{biome_at_world, biome_display_name, biome_profile, pick_coastal_spawn, AussieBiome};
use chunk_store::{
    archive_chunk, take_saved_chunk, ChunkArchive, ChunkRecord, ChunkTreeJob,
};
use map_ui::{setup_map_ui, toggle_map_ui, update_map_ui, MapOverlay};
use silhouettes::{
    billboard_silhouettes, orient_silhouette_shadows, plan_tree_silhouettes,
    process_silhouette_queue, setup_silhouette_assets, SilhouetteWorld,
};
use terrain::{
    apply_edits, build_culled_voxel_mesh, downsample_blocks, generate_chunk_blocks, BlockType,
    ChunkVoxels, TerrainMaterials, TerrainSurface,
};
use topography::{is_cave_cell, surface_height_voxels, surface_top_world_y};
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
    base_x: f32,
    base_y: f32,
    base_z: f32,
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

/// Real trees still building asynchronously for a live chunk. The horizon
/// cutouts watch this: a chunk's silhouette trees only retire once it hits
/// zero, so a distant tree never vanishes before its real self stands up.
#[derive(Component)]
pub struct TreesPending(pub usize);

/// Tracks which chunk columns are currently materialized in the ECS world.
#[derive(Resource, Default)]
struct ChunkWorld {
    loaded: HashMap<IVec2, Entity>,
    /// Live records for chunks still in the ECS — collapsed into [`ChunkArchive`] on unload.
    active_records: HashMap<IVec2, ChunkRecord>,
    /// Chunks generating on the background pool right now.
    pending: HashSet<IVec2>,
    /// Actual per-column surface voxel of each live chunk (post-caves,
    /// post-burrows-at-load). The gravity floor stands on THIS, not on the
    /// height formula, which can disagree with the meshed terrain by a voxel.
    surface_tops: HashMap<IVec2, Vec<i32>>,
    load_queue: VecDeque<IVec2>,
    last_player_chunk: Option<IVec2>,
}

/// Everything a background thread builds for one terrain chunk.
struct BuiltChunk {
    record: ChunkRecord,
    mesh: Option<Mesh>,
    column_tops: Vec<i32>,
}

/// An in-flight background chunk build; resolved by [`finish_chunk_tasks`].
#[derive(Component)]
struct PendingChunk {
    task: Task<BuiltChunk>,
    coord: IVec2,
}

#[derive(Resource, Default)]
struct TreeSpawnQueue(VecDeque<ChunkTreeJob>);

/// Shared handles for the extruded-PNG leaf so every chunk can scatter copies.
#[derive(Resource)]
struct LeafAssets {
    mesh: Handle<Mesh>,
    material: Handle<StandardMaterial>,
}

/// Optional player-authored skin for tree foliage blocks (`assets/foliage.png`).
/// Painted in grayscale — each species' foliage colour multiplies it, so one
/// texture skins every tree. Alpha in the texture makes leaf blocks semi-
/// transparent. `None` (file absent) falls back to flat-colour foliage.
#[derive(Resource, Default)]
struct FoliageSkin(Option<Handle<Image>>);

#[derive(Resource)]
struct GameSounds {
    munch: Handle<AudioSource>,
}

/// Shared handles for worm castings — the little dark cube a worm leaves on
/// the ground behind it after eating anything (dirt or leaf alike).
#[derive(Resource)]
struct CastingAssets {
    mesh: Handle<Mesh>,
    material: Handle<StandardMaterial>,
}

/// In-game radio: every track in `assets/music/` is in the rotation (drop a
/// file in to add it). A song starts every [`SOUNDTRACK_INTERVAL_SECS`]; picks
/// are random but never the same song twice in a row.
#[derive(Resource)]
struct Soundtrack {
    tracks: Vec<Handle<AudioSource>>,
    last_played: Option<usize>,
    /// Time until the next song may start (a still-playing song delays it).
    timer: Timer,
    rng: GardenRng,
}

const SOUNDTRACK_INTERVAL_SECS: f32 = 5.0 * 60.0;

/// Marks the currently playing soundtrack song (despawned when it ends).
#[derive(Component)]
struct SoundtrackSong;

/// Global wind: a slowly wandering direction, gusts layered on, and a
/// 0–5 strength that re-rolls every half-minute-or-so with a strong bias
/// toward calm. 0 = nice still day; 5 = a gale that physically shoves the
/// worm downwind. Streamers in the air show where it's blowing.
#[derive(Resource)]
struct Wind {
    dir: Vec2,
    /// Compass heading of `dir`, radians. Only moves ±1° per weather shift.
    heading: f32,
    /// 0 = dead calm … 5 = worm-shoving gale.
    strength: f32,
    target: f32,
    next_shift_at: f32,
    rng: GardenRng,
}

impl Default for Wind {
    fn default() -> Self {
        let mut rng = GardenRng::new(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos() as u64)
                .unwrap_or(0xB1FF),
        );
        let heading = rng.range(0.0, std::f32::consts::TAU);
        Self {
            dir: Vec2::new(heading.cos(), heading.sin()),
            heading,
            strength: 0.0,
            target: 0.0,
            next_shift_at: 0.0,
            rng,
        }
    }
}

/// The gale threshold: above this the worm starts getting pushed.
const WIND_PUSH_FROM: f32 = 4.0;

/// A ribbon of air racing downwind past the worm — the wind made visible.
/// Spawned upwind, despawned when it outlives itself or blows out of range.
#[derive(Component)]
struct WindStreamer {
    age: f32,
    life: f32,
    speed: f32,
    bob_phase: f32,
}

#[derive(Resource)]
struct StreamerAssets {
    mesh: Handle<Mesh>,
    material: Handle<StandardMaterial>,
}

/// Grass clumps are leaf-width crossed quads wearing the player's white
/// cutout sprites, tinted per species with a base→tip vertex-colour gradient.
/// Mitchell grass is two-part: blades plus a separately tinted seed-head top.
struct GrassSpecies {
    mesh: Handle<Mesh>,
    material: Handle<StandardMaterial>,
    /// Optional stacked top piece (Mitchell's straw seed heads).
    top: Option<(Handle<Mesh>, Handle<StandardMaterial>)>,
}

#[derive(Resource)]
struct GrassAssets {
    species: Vec<GrassSpecies>,
    /// Per biome (`AussieBiome as usize`): species index + clumps per chunk.
    by_biome: [Option<(usize, (i32, i32))>; 8],
}

/// One planted clump: fixed facing plus its own sway rhythm.
#[derive(Component, Clone, Copy)]
struct GrassClump {
    yaw: f32,
    phase: f32,
    freq: f32,
}

/// Grass clumps match the collectible leaves in width (~3 worm-lengths) and
/// stand three widths tall — grass towers over a worm.
const GRASS_WIDTH: f32 = WORM_LENGTH * 3.0;
const GRASS_HEIGHT: f32 = GRASS_WIDTH * 3.0;

/// Lego grass: every opaque pixel of the player's sprite becomes a solid box
/// one pixel deep — genuine 3D pixel art with joined faces on every side,
/// meshed with the same culled-voxel builder the trees use, then rescaled to
/// world size and tinted with a bottom→tip gradient. Oversized sprites are
/// downsampled to lego resolution first.
fn grass_lego_mesh(
    path: &str,
    width: f32,
    height: f32,
    y_offset: f32,
    base: (f32, f32, f32),
    tip: (f32, f32, f32),
) -> Mesh {
    let img = image::open(path)
        .unwrap_or_else(|e| panic!("failed to load grass sprite {path}: {e}"))
        .to_rgba8();
    let img = if img.width() > 48 || img.height() > 48 {
        image::imageops::resize(&img, 32, 32, image::imageops::FilterType::Nearest)
    } else {
        img
    };
    let (w, h) = img.dimensions();

    let mut cells: HashSet<IVec3> = HashSet::new();
    for py in 0..h {
        for px in 0..w {
            if img.get_pixel(px, py)[3] > 128 {
                cells.insert(IVec3::new(px as i32, (h - 1 - py) as i32, 0));
            }
        }
    }

    let mut mesh = build_culled_voxel_mesh(&cells, 1.0);

    // Rescale pixel units to world feet (centred on x, one pixel of depth
    // centred on z) and paint the height gradient into vertex colours.
    let sx = width / w as f32;
    let sy = height / h as f32;
    let cx = w as f32 * 0.5;
    let mut colors: Vec<[f32; 4]> = Vec::new();
    if let Some(bevy::render::mesh::VertexAttributeValues::Float32x3(positions)) =
        mesh.attribute_mut(Mesh::ATTRIBUTE_POSITION)
    {
        for p in positions.iter_mut() {
            let t = (p[1] / h as f32).clamp(0.0, 1.0);
            colors.push([
                base.0 + (tip.0 - base.0) * t,
                base.1 + (tip.1 - base.1) * t,
                base.2 + (tip.2 - base.2) * t,
                1.0,
            ]);
            p[0] = (p[0] - cx) * sx;
            p[1] = p[1] * sy + y_offset;
            p[2] = (p[2] - 0.5) * sx;
        }
    }
    mesh.insert_attribute(Mesh::ATTRIBUTE_COLOR, colors);
    mesh
}

/// Grass shivers fast and light — same wind and gusts as the trees, higher
/// frequency, flattening progressively in a gale. And it makes way for the
/// worm: clumps near the little guy bend away, springing back as he passes.
fn sway_grass(
    time: Res<Time>,
    wind: Res<Wind>,
    cam_q: Query<&Transform, (With<Camera>, Without<GrassClump>)>,
    mut clumps: Query<(&GrassClump, &GlobalTransform, &mut Transform)>,
) {
    let t = time.elapsed_secs();
    let gust = 0.5 + 0.5 * ((t * 0.11).sin() * 0.6 + (t * 0.043).sin() * 0.4);
    let wind_axis = Vec3::new(-wind.dir.y, 0.0, wind.dir.x);
    let force = wind.strength / 5.0;
    let cam_pos = cam_q.get_single().map(|c| c.translation).ok();

    // How close the worm must be before grass yields, and how hard it bends.
    const PUSH_RADIUS: f32 = 0.9;
    const PUSH_MAX_RAD: f32 = 1.25;

    for (clump, global, mut tf) in &mut clumps {
        let wind_angle = force
            * ((0.06 + 0.11 * gust) * (t * clump.freq + clump.phase).sin() + 0.35 * gust);
        let mut rotation = Quat::from_axis_angle(wind_axis, wind_angle);

        if let Some(cam) = cam_pos {
            let world = global.translation();
            let away = Vec3::new(world.x - cam.x, 0.0, world.z - cam.z);
            let dist = away.length();
            if dist < PUSH_RADIUS && dist > 0.001 {
                let strength = 1.0 - dist / PUSH_RADIUS;
                let bend_axis = Vec3::Y.cross(away / dist);
                rotation =
                    Quat::from_axis_angle(bend_axis, PUSH_MAX_RAD * strength * strength)
                        * rotation;
            }
        }

        tf.rotation = rotation * Quat::from_rotation_y(clump.yaw);
    }
}

/// Blanket a chunk in grass clumps — species and density by biome, planted on
/// the chunk's REAL column tops so every clump roots exactly on the dirt.
fn scatter_chunk_grass(
    commands: &mut Commands,
    chunk_entity: Entity,
    coord: IVec2,
    tops: &[i32],
    grass: &GrassAssets,
) {
    let center = chunk_world_origin(coord) + Vec3::new(CHUNK_SIZE * 0.5, 0.0, CHUNK_SIZE * 0.5);
    let profile = biome_profile(center.x, center.z);
    let Some((species_idx, (lo, hi))) = grass.by_biome[profile.biome as usize] else {
        return;
    };
    let species = &grass.species[species_idx];

    let mut rng = GardenRng::new(chunk_seed(WORLD_SEED, coord) ^ 0x6A55_6A55);
    let count = rng.range_i(lo, hi);

    commands.entity(chunk_entity).with_children(|chunk| {
        for _ in 0..count {
            let lx = rng.range(0.2, CHUNK_SIZE - 0.2);
            let lz = rng.range(0.2, CHUNK_SIZE - 0.2);
            let cx = ((lx / VOXEL_SIZE) as i32).clamp(0, CHUNK_VOXELS - 1);
            let cz = ((lz / VOXEL_SIZE) as i32).clamp(0, CHUNK_VOXELS - 1);
            let y = (tops[(cz * CHUNK_VOXELS + cx) as usize] + 1) as f32 * VOXEL_SIZE;

            let clump = GrassClump {
                yaw: rng.range(0.0, std::f32::consts::TAU),
                phase: rng.range(0.0, std::f32::consts::TAU),
                freq: rng.range(2.2, 3.6),
            };
            // Wild size spread: anything from a 5% sprout barely clearing the
            // soil to a 200% monster clump twice standard height.
            let transform = Transform {
                translation: Vec3::new(lx, y, lz),
                rotation: Quat::from_rotation_y(clump.yaw),
                scale: Vec3::splat(rng.range(0.05, 2.0)),
            };

            if let Some((top_mesh, top_mat)) = &species.top {
                chunk.spawn((
                    GrassClump { ..clump },
                    Mesh3d(top_mesh.clone()),
                    MeshMaterial3d(top_mat.clone()),
                    NotShadowCaster,
                    transform,
                ));
            }
            chunk.spawn((
                clump,
                Mesh3d(species.mesh.clone()),
                MeshMaterial3d(species.material.clone()),
                NotShadowCaster,
                transform,
            ));
        }
    });
}

/// Keep a population of streamers proportional to wind strength flowing past
/// the camera, all pointing (and moving) downwind.
fn update_wind_streamers(
    time: Res<Time>,
    mut commands: Commands,
    mut wind: ResMut<Wind>,
    assets: Res<StreamerAssets>,
    chunk_world: Res<ChunkWorld>,
    cam_q: Query<&Transform, (With<Camera>, Without<WindStreamer>)>,
    mut streamers: Query<(Entity, &mut WindStreamer, &mut Transform), Without<Camera>>,
) {
    let Ok(cam) = cam_q.get_single() else {
        return;
    };
    let cam_pos = cam.translation;
    let dir3 = Vec3::new(wind.dir.x, 0.0, wind.dir.y);
    let yaw = Quat::from_rotation_y((-wind.dir.y).atan2(wind.dir.x));
    let dt = time.delta_secs();
    let t = time.elapsed_secs();

    // Streamer population: none on a calm day, a handful in light air, a
    // blizzard of them in a gale.
    let desired = if wind.strength < 0.4 {
        0
    } else {
        ((wind.strength * 10.0).round() as usize).max(4)
    };

    let mut alive = 0usize;
    for (entity, mut streamer, mut tf) in &mut streamers {
        streamer.age += dt;
        tf.translation += dir3 * streamer.speed * dt;
        tf.translation.y += (t * 2.3 + streamer.bob_phase).sin() * 0.3 * dt;
        tf.rotation = yaw;

        let gone_far = Vec2::new(tf.translation.x - cam_pos.x, tf.translation.z - cam_pos.z)
            .length()
            > 45.0;
        if streamer.age > streamer.life || gone_far {
            commands.entity(entity).despawn();
        } else {
            alive += 1;
        }
    }

    // Top up toward the target population, a few per frame, seeded upwind so
    // they stream past the worm.
    let mut to_spawn = desired.saturating_sub(alive).min(3);
    while to_spawn > 0 {
        to_spawn -= 1;
        let side = Vec3::new(-dir3.z, 0.0, dir3.x);
        // Low worm-level airspace, close by — streamers a worm actually sees,
        // hugging the ground it crawls on.
        let mut pos =
            cam_pos - dir3 * wind.rng.range(3.0, 18.0) + side * wind.rng.range(-10.0, 10.0);
        pos.y = ground_world_y(&chunk_world, pos.x, pos.z) + wind.rng.range(0.15, 2.2);

        let speed = 4.0 + wind.strength * 3.0 + wind.rng.range(0.0, 2.0);
        // The streamer IS the wind gauge: a level-1 breeze draws short wisps,
        // a level-5 gale drags long fat banners. (Direction is the streak's
        // long axis + its motion — both point downwind.)
        let power = wind.strength / 5.0;
        let length = (0.4 + power * 2.6) * wind.rng.range(0.85, 1.15);
        let girth = 0.5 + power * 2.0;
        commands.spawn((
            WindStreamer {
                age: 0.0,
                life: wind.rng.range(3.0, 6.0),
                speed,
                bob_phase: wind.rng.range(0.0, std::f32::consts::TAU),
            },
            Mesh3d(assets.mesh.clone()),
            MeshMaterial3d(assets.material.clone()),
            NotShadowCaster,
            Transform {
                translation: pos,
                rotation: yaw,
                scale: Vec3::new(length, girth, girth),
            },
        ));
    }
}

/// Per-tree sway character, seeded from the tree itself: giants heave slowly,
/// saplings flick about.
#[derive(Component)]
struct WindSway {
    phase: f32,
    /// Peak lean, radians.
    amplitude: f32,
    /// Oscillation rate, rad/s.
    frequency: f32,
}

fn update_wind(time: Res<Time>, mut wind: ResMut<Wind>) {
    let t = time.elapsed_secs();

    // Weather re-roll: six discrete levels on a quadratic likelihood curve,
    // pinned at 50% for dead calm and 1% for a full 5/5 gale, the middle
    // falling off as (1 - n/5)²:
    //   0: 50.0%  1: 26.1%  2: 14.7%  3: 6.5%  4: 1.6%  5: 1.0%
    //
    // How long a level holds depends on how windy it is — calm days linger,
    // gales blow themselves out: 0→5 min, 1→4, 2→3, 3→2.5, 4→2, 5→1.
    if t >= wind.next_shift_at {
        const WIND_LEVEL_CDF: [f32; 6] = [0.500, 0.7613, 0.9083, 0.9737, 0.9900, 1.0];
        const HOLD_MINUTES: [f32; 6] = [5.0, 4.0, 3.0, 2.5, 2.0, 1.0];

        let roll = wind.rng.next_f32();
        let level = WIND_LEVEL_CDF
            .iter()
            .position(|&cum| roll < cum)
            .unwrap_or(5);
        wind.target = level as f32;
        wind.next_shift_at = t + HOLD_MINUTES[level] * 60.0;

        // The direction creeps: exactly one degree per shift, coin-flip
        // left or right — over hours the wind slowly wanders the compass.
        let step = 1.0f32.to_radians();
        wind.heading += if wind.rng.chance(0.5) { step } else { -step };
        wind.dir = Vec2::new(wind.heading.cos(), wind.heading.sin());

        println!("🌬️ Wind shifting toward {level}/5");
    }
    // Ease toward the target like real weather, not a light switch.
    let blend = (time.delta_secs() * 0.06).min(1.0);
    wind.strength += (wind.target - wind.strength) * blend;
}

/// Rock every tree around its base. Rotation pivots at the trunk root (tree
/// meshes grow up from y=0), so the roots stay planted while the crown rides
/// the gusts — the wood carries the leaves, so the whole canopy moves with it.
fn sway_trees(
    time: Res<Time>,
    wind: Res<Wind>,
    mut trees: Query<(&WindSway, &mut Transform), With<WildTree>>,
    mut canopies: Query<
        (&Parent, &mut Transform),
        (With<FoliageLod>, Without<WildTree>),
    >,
    sway_of: Query<&WindSway>,
) {
    let t = time.elapsed_secs();
    // Gusts: two slow sines beating against each other, 0..1.
    let gust = 0.5 + 0.5 * ((t * 0.11).sin() * 0.6 + (t * 0.043).sin() * 0.4);
    let lean_axis = Vec3::new(-wind.dir.y, 0.0, wind.dir.x);
    // 0/5 = trees stand dead still; 5/5 = everything heaving.
    let force = wind.strength / 5.0;

    for (sway, mut tf) in &mut trees {
        let wave = (t * sway.frequency + sway.phase).sin() * 0.7
            + (t * sway.frequency * 2.3 + sway.phase * 1.7).sin() * 0.3;
        let angle = force
            * (sway.amplitude * (0.35 + 0.65 * gust) * wave
                + sway.amplitude * 0.5 * gust); // steady downwind lean under the oscillation
        tf.rotation = Quat::from_axis_angle(lean_axis, angle);
    }

    // Leaf flutter: the canopy stirs a touch faster than the trunk under it,
    // and keeps a whisper of life even in light air.
    let flutter_force = (0.08 + 0.92 * force).min(1.0);
    for (parent, mut tf) in &mut canopies {
        let Ok(sway) = sway_of.get(parent.get()) else {
            continue;
        };
        let flutter = flutter_force
            * sway.amplitude
            * 0.35
            * (0.3 + 0.7 * gust)
            * (t * sway.frequency * 2.9 + sway.phase * 2.3).sin();
        tf.rotation = Quat::from_axis_angle(lean_axis, flutter);
    }
}

/// Foliage LOD ladder: block-size multipliers per level (2″ → 8″ → 32″ leaf
/// blocks) and the switch-over distances in feet. Distance is measured from
/// the camera to the canopy's bounding *sphere*, so the crown of a giant goes
/// coarse even while you stand at its trunk — 2-inch blocks 400 ft overhead
/// are wasted triangles either way.
const FOLIAGE_LOD_FACTORS: [i32; 3] = [1, 4, 16];
const FOLIAGE_LOD_DISTANCES_FT: [f32; 2] = [25.0, 100.0];
/// Minimum fraction of fine voxels a coarse cell needs to survive downsampling.
const FOLIAGE_LOD_FILL: f32 = 0.2;

/// On the tree root: canopy bounding sphere (tree-local) for LOD selection.
#[derive(Component)]
struct FoliageLodGroup {
    center: Vec3,
    radius: f32,
}

/// On each foliage mesh child: which rung of the LOD ladder it is.
#[derive(Component)]
struct FoliageLod {
    level: usize,
}

/// Worm gravity. The little guy has weight: no god mode, no hovering — he
/// falls to the ground and stays on it. `G` toggles god mode (free flight,
/// the old behaviour) for inspecting the giants.
const GRAVITY_FT_S2: f32 = 32.0;
const TERMINAL_FALL_FT_S: f32 = 90.0;

#[derive(Resource)]
struct GodMode {
    enabled: bool,
    fall_speed: f32,
    /// Current upward stretch from holding Space — a worm can *reach*, not
    /// jump (jumping waits for the "legs" branch of the skill tree).
    reach: f32,
    /// Camera position at the end of last frame's physics — the difference is
    /// this frame's crawl input, which the wind gets to argue with.
    prev_pos: Option<Vec3>,
}

impl GodMode {
    fn from_env() -> Self {
        // A debug start high in the air implies you want to fly around it.
        let enabled = std::env::var("GARDN_HIGH").is_ok_and(|v| v.trim().parse::<f32>().is_ok());
        Self {
            enabled,
            fall_speed: 0.0,
            reach: 0.0,
            prev_pos: None,
        }
    }
}

fn toggle_god_mode(keys: Res<ButtonInput<KeyCode>>, mut god: ResMut<GodMode>) {
    if keys.just_pressed(KeyCode::KeyG) {
        god.enabled = !god.enabled;
        god.fall_speed = 0.0;
        if god.enabled {
            println!("👼 God mode ON — free flight.");
        } else {
            println!("🪱 God mode OFF — gravity has you.");
        }
    }
}

/// The ground under (x, z): the terrain surface, sunk through natural caves
/// and any voxels the worm has eaten — digging (or a cave mouth) really
/// lowers your floor.
fn ground_world_y(chunk_world: &ChunkWorld, x: f32, z: f32) -> f32 {
    let surface = surface_height_voxels(x, z);
    let vx = (x / VOXEL_SIZE).floor() as i32;
    let vz = (z / VOXEL_SIZE).floor() as i32;
    let coord = IVec2::new(vx.div_euclid(CHUNK_VOXELS), vz.div_euclid(CHUNK_VOXELS));
    let local_x = vx.rem_euclid(CHUNK_VOXELS);
    let local_z = vz.rem_euclid(CHUNK_VOXELS);

    // Live chunks know their REAL per-column surface (post-caves, post-load
    // burrows) — the formula can be a voxel or two off the meshed terrain,
    // which used to leave the worm hovering over (or clipping into) a freshly
    // eaten floor. Fall back to the formula only for unloaded ground.
    let cached = chunk_world
        .surface_tops
        .get(&coord)
        .map(|tops| tops[(local_z * CHUNK_VOXELS + local_x) as usize]);
    let mut top = cached.unwrap_or(surface);

    let record = chunk_world.active_records.get(&coord);
    let bedrock = surface - CHUNK_DEPTH_VOXELS + 2;
    while top > bedrock {
        let eaten = record.is_some_and(|r| r.edits.contains(&IVec3::new(local_x, top, local_z)));
        // Cached tops already account for caves; only this session's fresh
        // bites need the descent. Formula fallback also sinks through caves.
        if eaten || (cached.is_none() && is_cave_cell(vx, top, vz, surface)) {
            top -= 1;
        } else {
            break;
        }
    }
    (top + 1) as f32 * VOXEL_SIZE
}

/// One column's collision data, computed once per (x, z) query: real top (post
/// caves/burrows where loaded), bedrock band, and per-voxel solidity.
struct ColumnProbe<'a> {
    surface: i32,
    top: i32,
    bedrock: i32,
    vx: i32,
    vz: i32,
    local: IVec2,
    record: Option<&'a ChunkRecord>,
}

impl ColumnProbe<'_> {
    fn at(chunk_world: &ChunkWorld, x: f32, z: f32) -> ColumnProbe<'_> {
        let surface = surface_height_voxels(x, z);
        let vx = (x / VOXEL_SIZE).floor() as i32;
        let vz = (z / VOXEL_SIZE).floor() as i32;
        let coord = IVec2::new(vx.div_euclid(CHUNK_VOXELS), vz.div_euclid(CHUNK_VOXELS));
        let local = IVec2::new(vx.rem_euclid(CHUNK_VOXELS), vz.rem_euclid(CHUNK_VOXELS));
        let top = chunk_world
            .surface_tops
            .get(&coord)
            .map(|tops| tops[(local.y * CHUNK_VOXELS + local.x) as usize])
            .unwrap_or(surface);
        ColumnProbe {
            surface,
            top,
            bedrock: surface - CHUNK_DEPTH_VOXELS + 2,
            vx,
            vz,
            local,
            record: chunk_world.active_records.get(&coord),
        }
    }

    /// Exact voxel solidity — the rule blocks live by: nothing above the top,
    /// bedrock always solid, otherwise solid unless eaten or a cave cell.
    fn solid(&self, vy: i32) -> bool {
        if vy > self.top {
            return false;
        }
        if vy <= self.bedrock {
            return true;
        }
        if self
            .record
            .is_some_and(|r| r.edits.contains(&IVec3::new(self.local.x, vy, self.local.y)))
        {
            return false;
        }
        !is_cave_cell(self.vx, vy, self.vz, self.surface)
    }

    fn solid_at_ft(&self, y_ft: f32) -> bool {
        self.solid((y_ft / VOXEL_SIZE).floor() as i32)
    }

    /// Top face of the first solid voxel at or below `from_ft` — the local
    /// floor, correct inside tunnels and under roofs (a heightfield is not).
    fn floor_below(&self, from_ft: f32) -> f32 {
        let mut vy = ((from_ft / VOXEL_SIZE).floor() as i32).min(self.top);
        while vy > self.bedrock && !self.solid(vy) {
            vy -= 1;
        }
        (vy + 1) as f32 * VOXEL_SIZE
    }

    /// Bottom face of the first solid voxel above `from_ft` — the roof, or
    /// effectively-infinity under open sky.
    fn ceiling_above(&self, from_ft: f32) -> f32 {
        let mut vy = (from_ft / VOXEL_SIZE).floor() as i32 + 1;
        while vy <= self.top {
            if self.solid(vy) {
                return vy as f32 * VOXEL_SIZE;
            }
            vy += 1;
        }
        f32::MAX
    }
}

/// Pull the worm down to the ground unless god mode is on. Runs after the
/// flycam has moved the camera, just before transforms propagate.
fn worm_gravity(
    time: Res<Time>,
    keys: Res<ButtonInput<KeyCode>>,
    wind: Res<Wind>,
    mut god: ResMut<GodMode>,
    chunk_world: Res<ChunkWorld>,
    mut cam_q: Query<&mut Transform, With<Camera>>,
) {
    if god.enabled {
        god.prev_pos = None;
        return;
    }
    let Ok(mut tf) = cam_q.get_single_mut() else {
        return;
    };

    // Wind vs. crawl: split this frame's movement into downwind, upwind, and
    // crosswind parts. Tailwinds help a little; headwinds drag harder with
    // every level; at gale strength control collapses — upwind is impossible,
    // sideways barely works, and even going with it is a slow scramble.
    if let Some(prev) = god.prev_pos {
        let delta = Vec2::new(tf.translation.x - prev.x, tf.translation.z - prev.z);
        // Ignore teleport-sized jumps (spawns, debug moves).
        if delta.length_squared() < 25.0 && delta.length_squared() > 0.0 {
            let s = wind.strength;
            let along = delta.dot(wind.dir);
            let cross = delta - wind.dir * along;

            // 4/5 → gale: whatever control is left fades out fast.
            let control = 1.0 - ((s - WIND_PUSH_FROM).clamp(0.0, 1.0) * 0.65);
            let along_scaled = if along >= 0.0 {
                along * (1.0 + 0.08 * s) * control
            } else {
                along * (1.0 - 0.2 * s).max(0.0)
            };
            let cross_scaled = cross * (1.0 - 0.06 * s).max(0.2) * control;

            let adjusted = wind.dir * along_scaled + cross_scaled;
            tf.translation.x = prev.x + adjusted.x;
            tf.translation.z = prev.z + adjusted.y;
        }
    }

    // A gale shoves the worm downwind — above 4/5 the push outpaces crawling
    // into it, so find shelter or get blown across the paddock.
    if wind.strength > WIND_PUSH_FROM {
        let shove = (wind.strength - WIND_PUSH_FROM) * 2.2 * time.delta_secs();
        tf.translation.x += wind.dir.x * shove;
        tf.translation.z += wind.dir.y * shove;
    }

    // Solid blocks BLOCK — the worm's body is tested against the actual
    // voxels, so cave walls, roofs, and cliff faces all stop movement dead.
    // A column is passable if the worm fits at its current height, or one
    // climbable step higher (that's how small ledges stay mountable). Blocked
    // movement slides along whichever axis stays open.
    const CLIMB_LIMIT_FT: f32 = 0.26;
    if let Some(prev) = god.prev_pos {
        let fits = |x: f32, z: f32, y: f32| -> bool {
            let col = ColumnProbe::at(&chunk_world, x, z);
            !col.solid_at_ft(y) && !col.solid_at_ft(y - WORM_EYE_HEIGHT * 0.6)
        };
        let passable = |x: f32, z: f32| -> bool {
            fits(x, z, tf.translation.y) || fits(x, z, tf.translation.y + CLIMB_LIMIT_FT)
        };
        if !passable(tf.translation.x, tf.translation.z) {
            if passable(tf.translation.x, prev.z) {
                tf.translation.z = prev.z;
            } else if passable(prev.x, tf.translation.z) {
                tf.translation.x = prev.x;
            } else {
                tf.translation.x = prev.x;
                tf.translation.z = prev.z;
            }
        }
    }

    // The floor is wherever solid ground actually is BELOW the worm — inside
    // a tunnel that's the tunnel floor, never some surface high overhead.
    let here_col = ColumnProbe::at(&chunk_world, tf.translation.x, tf.translation.z);
    let here = here_col.floor_below(tf.translation.y + 0.02);

    // Glide path: a 3-inch worm spans several 1-inch columns, so its ride
    // height is the FOOTPRINT AVERAGE of the ground around it — block steps
    // become gradients, and approaching a ledge starts the rise early.
    // Samples more than a climbable step away (cliff lips, pit edges) are
    // ignored so the average never floats the worm off a wall.
    const CLIMB_LOOKAHEAD_FT: f32 = 0.22;
    let mut floor_sum = here;
    let mut floor_n = 1.0;
    for (dx, dz) in [(1.0, 0.0), (-1.0, 0.0), (0.0, 1.0), (0.0, -1.0)] {
        let col = ColumnProbe::at(
            &chunk_world,
            tf.translation.x + dx * CLIMB_LOOKAHEAD_FT,
            tf.translation.z + dz * CLIMB_LOOKAHEAD_FT,
        );
        let sample = col.floor_below(tf.translation.y + CLIMB_LIMIT_FT);
        if (sample - here).abs() <= CLIMB_LIMIT_FT {
            floor_sum += sample;
            floor_n += 1.0;
        }
    }
    let floor = floor_sum / floor_n;

    // Space = reach: the worm stretches up to a full extra worm-length to get
    // its mouth at higher blocks. No jumping — that's a legs feature.
    let reach_target = if keys.pressed(KeyCode::Space) {
        WORM_REACH
    } else {
        0.0
    };
    let blend = (time.delta_secs() * 10.0).min(1.0);
    god.reach += (reach_target - god.reach) * blend;

    // Roofs are real: standing height (and stretching) stops under the first
    // solid voxel overhead instead of poking through it.
    let ceiling = here_col.ceiling_above(floor + 0.01);
    let stand = (floor + WORM_EYE_HEIGHT + god.reach).min(ceiling - 0.03).max(floor + 0.02);
    let dy = stand - tf.translation.y;

    // Eased vertical follow: the camera glides toward ride height on an
    // exponential curve — no fixed-rate starts and stops, no per-block
    // snapping. Rate caps keep cliffs from yanking it; only real drops
    // (beyond a body length) turn ballistic.
    const CLIMB_SPEED_FT_S: f32 = 2.6;
    const SETTLE_SPEED_FT_S: f32 = 4.0;
    const GLIDE_STIFFNESS: f32 = 9.0;
    let dt = time.delta_secs();
    if dy > -0.35 {
        let ease = 1.0 - (-dt * GLIDE_STIFFNESS).exp();
        let step = (dy * ease).clamp(-SETTLE_SPEED_FT_S * dt, CLIMB_SPEED_FT_S * dt);
        tf.translation.y += step;
        god.fall_speed = 0.0;
    } else {
        god.fall_speed = (god.fall_speed + GRAVITY_FT_S2 * dt).min(TERMINAL_FALL_FT_S);
        tf.translation.y = (tf.translation.y - god.fall_speed * dt).max(stand);
        if tf.translation.y <= stand {
            god.fall_speed = 0.0;
        }
    }

    god.prev_pos = Some(tf.translation);
}

/// One full sun cycle every 24 real hours. The clock starts at
/// [`GAME_START_HOUR`] when the app launches; GARDN_HOUR=<0-24> overrides the
/// starting hour (e.g. `GARDN_HOUR=0` to see the moonlit night right away).
const DAY_LENGTH_SECS: f32 = 24.0 * 3600.0;
const GAME_START_HOUR: f32 = 8.0;
/// How far from the camera the sun/moon discs float — past the fog's end
/// (680 ft) but inside the far clip (1000 ft); unlit materials skip fog, so
/// they burn through the haze.
const CELESTIAL_DISTANCE_FT: f32 = 880.0;

#[derive(Resource)]
struct DayCycle {
    start_frac: f32,
}

impl DayCycle {
    fn from_env() -> Self {
        let hour = std::env::var("GARDN_HOUR")
            .ok()
            .and_then(|v| v.trim().parse::<f32>().ok())
            .unwrap_or(GAME_START_HOUR);
        println!("🕗 Day cycle: starting the 24-hour clock at {hour:.1}h.");
        Self {
            start_frac: (hour / 24.0).rem_euclid(1.0),
        }
    }
}

/// A directional light driven around the sky by the day cycle.
#[derive(Component)]
struct CelestialLight {
    is_sun: bool,
}

/// Current to-sun direction, updated by the day cycle — the silhouette shadow
/// planes orient themselves by it so cutout shadows track the real sun.
#[derive(Resource)]
pub struct SunDirection(pub Vec3);

/// Fades a freshly spawned thing in by ramping its material's base-colour
/// alpha from 0, then restores the steady-state alpha mode (and optionally
/// swaps back to a shared material) so nothing pays transparency costs
/// forever. Attach to the entity holding the material.
#[derive(Component)]
pub struct FadeIn {
    pub material: Handle<StandardMaterial>,
    pub timer: Timer,
    pub final_alpha_mode: AlphaMode,
    /// Swap to this (shared) material once done, releasing the fade clone.
    pub swap_to: Option<Handle<StandardMaterial>>,
}

const TREE_FADE_SECS: f32 = 1.6;
pub const GROUND_FADE_SECS: f32 = 1.2;

fn fade_in_materials(
    time: Res<Time>,
    mut commands: Commands,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut fades: Query<(Entity, &mut FadeIn)>,
) {
    for (entity, mut fade) in &mut fades {
        fade.timer.tick(time.delta());
        let t = fade.timer.fraction();
        let eased = t * t * (3.0 - 2.0 * t);

        if let Some(mat) = materials.get_mut(&fade.material) {
            mat.base_color = mat.base_color.with_alpha(eased);
            if fade.timer.finished() {
                mat.base_color = mat.base_color.with_alpha(1.0);
                mat.alpha_mode = fade.final_alpha_mode;
            }
        }

        if fade.timer.finished() {
            if let Some(shared) = fade.swap_to.take() {
                commands.entity(entity).insert(MeshMaterial3d(shared));
            }
            commands.entity(entity).remove::<FadeIn>();
        }
    }
}

/// The visible unlit disc for a celestial body, re-anchored to the camera each
/// frame so it always hangs at the same sky position.
#[derive(Component)]
struct CelestialDisc {
    is_sun: bool,
}

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
struct PendingTree {
    task: Task<BuiltTree>,
    chunk_entity: Entity,
    local_base: Vec3,
    species: TreeSpecies,
    tree_seed: u64,
}

/// A completed bite: a worm-sized ball of voxels carved around the bite
/// point — possibly straddling a chunk border — plus each affected chunk's
/// freshly rebuilt terrain mesh, all computed off the main thread.
struct BurrowResult {
    block: BlockType,
    /// Per affected chunk: carved voxels (chunk-local) and the rebuilt mesh.
    chunks: Vec<(IVec2, Vec<IVec3>, Mesh)>,
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
            // Nearest for the pixel-art look; Repeat so block-skin UVs that run
            // 0..len across merged voxel strips tile instead of smearing.
            .set(ImagePlugin {
                default_sampler: ImageSamplerDescriptor {
                    address_mode_u: ImageAddressMode::Repeat,
                    address_mode_v: ImageAddressMode::Repeat,
                    address_mode_w: ImageAddressMode::Repeat,
                    ..ImageSamplerDescriptor::nearest()
                },
            })
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
        .insert_resource(DayCycle::from_env())
        .insert_resource(GodMode::from_env())
        .insert_resource(SunDirection(Vec3::new(0.6, 0.6, 0.35).normalize()))
        .init_resource::<Wind>()
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
                finish_chunk_tasks,
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
                orient_silhouette_shadows,
                update_foliage_lod,
                update_wind,
                sway_trees,
                sway_grass,
                update_wind_streamers,
                update_day_cycle,
                fade_in_materials,
                run_soundtrack,
                toggle_god_mode,
                toggle_map_ui,
                update_map_ui,
                animate_floating_leaves,
            ),
        )
        // After the flycam has moved the camera, before transforms propagate.
        .add_systems(
            PostUpdate,
            worm_gravity.before(bevy::transform::TransformSystem::TransformPropagate),
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
    println!("🐛 Controls: WASD crawl · Space stretch/reach · E eat/burrow · M map · G god mode (flight)");

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

    commands.insert_resource(CastingAssets {
        mesh: meshes.add(Cuboid::from_length(VOXEL_SIZE)),
        material: materials.add(StandardMaterial {
            base_color: Color::srgb(0.26, 0.19, 0.13),
            perceptual_roughness: 1.0,
            ..default()
        }),
    });

    // Lego grass: the player's sprites pixel-extruded into solid 3D — one
    // shared opaque vertex-colour material for everything (no cutout, no
    // sorting). Mitchell stacks straw seed heads over blue-green blades.
    let grass_material = materials.add(StandardMaterial {
        base_color: Color::WHITE,
        perceptual_roughness: 1.0,
        ..default()
    });
    let mitchell = GrassSpecies {
        mesh: meshes.add(grass_lego_mesh(
            "assets/grass/mitchell.png",
            GRASS_WIDTH,
            GRASS_HEIGHT,
            0.0,
            (0.34, 0.44, 0.26),
            (0.55, 0.60, 0.34),
        )),
        material: grass_material.clone(),
        top: Some((
            meshes.add(grass_lego_mesh(
                "assets/grass/mitchell_top.png",
                GRASS_WIDTH,
                GRASS_HEIGHT * 0.6,
                GRASS_HEIGHT * 0.55,
                (0.72, 0.62, 0.38),
                (0.82, 0.74, 0.46),
            )),
            grass_material.clone(),
        )),
    };
    let kangaroo = GrassSpecies {
        mesh: meshes.add(grass_lego_mesh(
            "assets/grass/kangaroo.png",
            GRASS_WIDTH,
            GRASS_HEIGHT,
            0.0,
            (0.24, 0.40, 0.18),
            (0.60, 0.36, 0.22),
        )),
        material: grass_material.clone(),
        top: None,
    };
    let button = GrassSpecies {
        mesh: meshes.add(grass_lego_mesh(
            "assets/grass/button.png",
            GRASS_WIDTH,
            GRASS_HEIGHT,
            0.0,
            (0.28, 0.38, 0.20),
            (0.52, 0.50, 0.30),
        )),
        material: grass_material,
        top: None,
    };

    let mut by_biome: [Option<(usize, (i32, i32))>; 8] = [None; 8];
    by_biome[AussieBiome::TropicalSavanna as usize] = Some((0, (40, 80)));
    by_biome[AussieBiome::AridOutback as usize] = Some((0, (8, 20)));
    by_biome[AussieBiome::Pilbara as usize] = Some((0, (10, 24)));
    by_biome[AussieBiome::Mediterranean as usize] = Some((1, (60, 110)));
    // Forests get a LOT of grass.
    by_biome[AussieBiome::TemperateForest as usize] = Some((1, (110, 180)));
    by_biome[AussieBiome::CoastalBush as usize] = Some((1, (60, 100)));
    by_biome[AussieBiome::Tasmania as usize] = Some((2, (70, 120)));
    commands.insert_resource(GrassAssets {
        species: vec![mitchell, kangaroo, button],
        by_biome,
    });

    // Wind streamers: pale ribbons ~1.4 ft long, translucent, unlit so they
    // read as moving air rather than solid geometry. Kept low over the ground
    // where a worm's eye actually looks.
    commands.insert_resource(StreamerAssets {
        mesh: meshes.add(Cuboid::new(1.4, 0.025, 0.025)),
        material: materials.add(StandardMaterial {
            base_color: Color::srgba(0.96, 0.97, 1.0, 0.6),
            alpha_mode: AlphaMode::Blend,
            unlit: true,
            ..default()
        }),
    });

    // Foliage block skin is optional — probe the disk so a missing file means
    // clean flat-colour fallback instead of a pink error texture.
    let foliage_skin = std::path::Path::new("assets/foliage.png")
        .exists()
        .then(|| asset_server.load("foliage.png"));
    commands.insert_resource(FoliageSkin(foliage_skin));

    // Sun light — shadows on, so open canopies throw dappled light shafts onto
    // the forest floor. Tight first cascade keeps shadow detail crisp at worm
    // eye level; the far cascades cover the giants overhead. The day cycle
    // steers its direction, colour, and strength every frame.
    // Layer 1 holds the invisible sun-facing shadow planes of the horizon
    // cutouts: the camera (layer 0) never draws them, but the lights see both
    // layers, so distant cutout trees still throw tree-shaped shadows.
    commands.spawn((
        CelestialLight { is_sun: true },
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
        RenderLayers::from_layers(&[0, 1]),
        Transform::from_rotation(Quat::from_euler(EulerRot::XYZ, -0.9, 0.7, 0.0)),
    ));

    // Full moon, always opposite the sun — the night is never pitch black.
    commands.spawn((
        CelestialLight { is_sun: false },
        DirectionalLight {
            illuminance: 0.0,
            color: Color::srgb(0.72, 0.80, 1.0),
            shadows_enabled: false,
            ..default()
        },
        CascadeShadowConfigBuilder {
            num_cascades: 2,
            first_cascade_far_bound: 12.0,
            maximum_distance: 160.0,
            ..default()
        }
        .build(),
        RenderLayers::from_layers(&[0, 1]),
        Transform::default(),
    ));

    // The bodies themselves: unlit spheres that ignore fog, hung well past the
    // fog wall so they read as sky, not scenery.
    commands.spawn((
        CelestialDisc { is_sun: true },
        Mesh3d(meshes.add(Sphere::new(34.0))),
        MeshMaterial3d(materials.add(StandardMaterial {
            // Pure HDR emitter (unlit would discard emissive): with bloom on
            // the camera the disc blazes a brilliant white halo instead of
            // reading as a flat dot. fog_enabled: false so the fog wall at
            // 680 ft can't swallow it.
            base_color: Color::BLACK,
            emissive: LinearRgba::rgb(40.0, 39.0, 36.0),
            fog_enabled: false,
            ..default()
        })),
        NotShadowCaster,
        Transform::default(),
    ));
    commands.spawn((
        CelestialDisc { is_sun: false },
        Mesh3d(meshes.add(Sphere::new(24.0))),
        MeshMaterial3d(materials.add(StandardMaterial {
            // A gentle glow — moonlight, not a second sun.
            base_color: Color::BLACK,
            emissive: LinearRgba::rgb(1.6, 1.8, 2.4),
            fog_enabled: false,
            ..default()
        })),
        NotShadowCaster,
        Transform::default(),
    ));

    commands.insert_resource(AmbientLight {
        color: Color::srgb(0.82, 0.88, 0.95),
        brightness: 70.0,
    });

    // Soundtrack rotation: every audio file in assets/music/ is a track.
    // The first song starts right away; run_soundtrack spaces out the rest.
    let mut tracks = Vec::new();
    if let Ok(entries) = std::fs::read_dir("assets/music") {
        let mut names: Vec<String> = entries
            .flatten()
            .filter_map(|e| e.file_name().into_string().ok())
            .filter(|n| {
                let lower = n.to_lowercase();
                // Only formats compiled into the build (Cargo features).
                lower.ends_with(".mp3") || lower.ends_with(".wav")
            })
            .collect();
        names.sort();
        for name in names {
            tracks.push(asset_server.load(format!("music/{name}")));
        }
    }
    let mut soundtrack = Soundtrack {
        tracks,
        last_played: None,
        timer: Timer::from_seconds(SOUNDTRACK_INTERVAL_SECS, TimerMode::Once),
        rng: GardenRng::new(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos() as u64)
                .unwrap_or(0x50D4),
        ),
    };
    if !soundtrack.tracks.is_empty() {
        let first = (soundtrack.rng.next_f32() * soundtrack.tracks.len() as f32) as usize
            % soundtrack.tracks.len();
        commands.spawn((
            SoundtrackSong,
            AudioPlayer::new(soundtrack.tracks[first].clone()),
            PlaybackSettings::DESPAWN,
        ));
        soundtrack.last_played = Some(first);
    }
    commands.insert_resource(soundtrack);
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
fn finish_tree_build_tasks(
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
            // Trees fade in over a second and a half instead of popping:
            // materials start fully transparent and FadeIn ramps them up,
            // restoring the cheap steady-state alpha mode afterwards.
            let bark_material = materials.add(StandardMaterial {
                base_color: built.bark_color.with_alpha(0.0),
                alpha_mode: AlphaMode::Blend,
                ..default()
            });
            // The species tint multiplies the (grayscale) skin texture, so one
            // player-drawn foliage.png colours itself per tree. Texture alpha
            // needs blending; flat colour stays opaque (cheaper, no sorting).
            // Strand-like foliage (fern fronds, casuarina needles) is drawn as
            // thin ribbons of blocks — the cutout skin shreds those, so they
            // stay solid.
            let strand_foliage = matches!(
                pending.species,
                TreeSpecies::TreeFern | TreeSpecies::DesertOak
            );
            let skin = if strand_foliage {
                None
            } else {
                foliage_skin.0.clone()
            };
            let foliage_final_mode = if skin.is_some() {
                AlphaMode::Blend
            } else {
                AlphaMode::Opaque
            };
            let foliage_material = materials.add(StandardMaterial {
                base_color: built.foliage_color.with_alpha(0.0),
                alpha_mode: AlphaMode::Blend,
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
                        },
                        Visibility::default(),
                        Transform::from_translation(local_base),
                    ))
                    .with_children(|tree_root| {
                        tree_root.spawn((
                            Mesh3d(bark_mesh),
                            MeshMaterial3d(bark_material.clone()),
                            Transform::IDENTITY,
                            FadeIn {
                                material: bark_material,
                                timer: Timer::from_seconds(TREE_FADE_SECS, TimerMode::Once),
                                final_alpha_mode: AlphaMode::Opaque,
                                swap_to: None,
                            },
                        ));
                        // All LOD rungs spawn hidden; update_foliage_lod shows
                        // the right one on the next frame. The foliage material
                        // is shared across rungs, so one FadeIn (on rung 0)
                        // fades whichever rung is showing.
                        for (level, mesh) in foliage_meshes.into_iter().enumerate() {
                            let mut rung = tree_root.spawn((
                                FoliageLod { level },
                                Mesh3d(mesh),
                                MeshMaterial3d(foliage_material.clone()),
                                Transform::IDENTITY,
                                Visibility::Hidden,
                            ));
                            if level == 0 {
                                rung.insert(FadeIn {
                                    material: foliage_material.clone(),
                                    timer: Timer::from_seconds(
                                        TREE_FADE_SECS,
                                        TimerMode::Once,
                                    ),
                                    final_alpha_mode: foliage_final_mode,
                                    swap_to: None,
                                });
                            }
                        }
                    });
            });
        }

        commands.entity(holder).despawn();
    }
}

/// Swap each tree's foliage mesh by distance: full 2-inch leaf blocks up
/// close, bigger averaged blocks farther out. Distance is to the canopy's
/// bounding sphere, so height counts — a giant's crown coarsens even from
/// directly below, and sharpens again if you ever get up there.
fn update_foliage_lod(
    cam_q: Query<&Transform, With<Camera>>,
    trees: Query<(&GlobalTransform, &FoliageLodGroup, &Children)>,
    mut lods: Query<(&FoliageLod, &mut Visibility)>,
) {
    let Ok(cam) = cam_q.get_single() else {
        return;
    };
    let cam_pos = cam.translation;

    for (tree_tf, group, children) in &trees {
        let center = tree_tf.translation() + group.center;
        let dist = (cam_pos - center).length() - group.radius;
        let level = FOLIAGE_LOD_DISTANCES_FT
            .iter()
            .position(|&cutoff| dist < cutoff)
            .unwrap_or(FOLIAGE_LOD_DISTANCES_FT.len());

        for child in children {
            let Ok((lod, mut vis)) = lods.get_mut(*child) else {
                continue;
            };
            let want = if lod.level == level {
                Visibility::Inherited
            } else {
                Visibility::Hidden
            };
            if *vis != want {
                *vis = want;
            }
        }
    }
}

/// Start the next soundtrack song once the interval has elapsed — random pick,
/// never the same song twice in a row. A song that outlasts the interval is
/// never cut off; the next one starts when it ends.
fn run_soundtrack(
    time: Res<Time>,
    mut commands: Commands,
    mut soundtrack: ResMut<Soundtrack>,
    playing: Query<(), With<SoundtrackSong>>,
) {
    soundtrack.timer.tick(time.delta());
    if !soundtrack.timer.finished() || !playing.is_empty() || soundtrack.tracks.is_empty() {
        return;
    }

    let n = soundtrack.tracks.len();
    let mut pick = (soundtrack.rng.next_f32() * n as f32) as usize % n;
    if n > 1 && Some(pick) == soundtrack.last_played {
        pick = (pick + 1) % n;
    }

    commands.spawn((
        SoundtrackSong,
        AudioPlayer::new(soundtrack.tracks[pick].clone()),
        PlaybackSettings::DESPAWN,
    ));
    soundtrack.last_played = Some(pick);
    soundtrack.timer.reset();
}

/// Walk the sun (and the full moon opposite it) across the sky on a real
/// 24-hour clock, retinting sky, fog, and ambient light to match. The moon
/// takes over lighting at night so the world is always readable.
fn update_day_cycle(
    time: Res<Time>,
    day: Res<DayCycle>,
    mut clear: ResMut<ClearColor>,
    mut ambient: ResMut<AmbientLight>,
    mut sun_direction: ResMut<SunDirection>,
    mut cam_q: Query<(&Transform, Option<&mut DistanceFog>), With<Camera>>,
    mut lights: Query<(&CelestialLight, &mut DirectionalLight, &mut Transform), Without<Camera>>,
    mut discs: Query<
        (&CelestialDisc, &mut Transform, &mut Visibility),
        (Without<Camera>, Without<CelestialLight>),
    >,
) {
    let frac = (day.start_frac + time.elapsed_secs() / DAY_LENGTH_SECS).rem_euclid(1.0);
    // 0.25 of the cycle = 6:00 — sunrise on the eastern horizon.
    let angle = (frac - 0.25) * std::f32::consts::TAU;
    // Slight southward tilt keeps noon shadows from collapsing to nothing.
    let sun_dir = Vec3::new(angle.cos(), angle.sin(), 0.35).normalize();
    let moon_dir = -sun_dir;
    let elev = sun_dir.y;
    // The cutout shadow planes face whichever body is casting shadows.
    sun_direction.0 = if elev >= 0.0 { sun_dir } else { moon_dir };

    let day_t = (elev * 3.0).clamp(0.0, 1.0);
    let dusk_t = ((elev + 0.15) / 0.15).clamp(0.0, 1.0);
    let moon_t = (-elev * 3.0).clamp(0.0, 1.0);

    // Five-stop sky: proper BLUE all day, and sundown walks blue → yellow →
    // orange → dark. Night is never black — the full moon on the other side
    // of the sky lifts it with a cool white sheen.
    const DAY_SKY: Vec3 = Vec3::new(0.34, 0.58, 0.96);
    const GOLD_SKY: Vec3 = Vec3::new(0.92, 0.80, 0.48);
    const ORANGE_SKY: Vec3 = Vec3::new(0.96, 0.52, 0.26);
    const NIGHT_SKY: Vec3 = Vec3::new(0.07, 0.09, 0.17);
    const MOON_SHEEN: Vec3 = Vec3::new(0.18, 0.21, 0.30);

    let sky = if elev >= 0.35 {
        DAY_SKY
    } else if elev >= 0.15 {
        GOLD_SKY.lerp(DAY_SKY, (elev - 0.15) / 0.20)
    } else if elev >= 0.0 {
        ORANGE_SKY.lerp(GOLD_SKY, elev / 0.15)
    } else {
        NIGHT_SKY.lerp(MOON_SHEEN, moon_t).lerp(ORANGE_SKY, dusk_t)
    };
    let sky_color = Color::srgb(sky.x, sky.y, sky.z);
    clear.0 = sky_color;

    ambient.brightness = 16.0 + 54.0 * day_t;
    // Ambient follows the same walk: blue daylight, golden dusk, moon-white night.
    let amb = Vec3::new(0.55, 0.62, 0.85)
        .lerp(Vec3::new(0.78, 0.86, 1.0), day_t)
        .lerp(Vec3::new(0.95, 0.85, 0.62), (1.0 - day_t) * (dusk_t * dusk_t));
    ambient.color = Color::srgb(amb.x, amb.y, amb.z);

    for (light, mut dl, mut tf) in &mut lights {
        if light.is_sun {
            dl.illuminance = 24_000.0 * elev.max(0.0).powf(0.6);
            let warm = Vec3::new(1.0, 0.60, 0.35).lerp(Vec3::new(1.0, 0.98, 0.94), day_t);
            dl.color = Color::srgb(warm.x, warm.y, warm.z);
            // Hand the (expensive) shadow pass to whichever body is up.
            dl.shadows_enabled = elev > 0.02;
            tf.look_to(-sun_dir, Vec3::Y);
        } else {
            dl.illuminance = 420.0 * moon_t;
            dl.shadows_enabled = elev < -0.02;
            tf.look_to(-moon_dir, Vec3::Y);
        }
    }

    let Ok((cam_tf, fog)) = cam_q.get_single_mut() else {
        return;
    };
    let cam_pos = cam_tf.translation;
    if let Some(mut fog) = fog {
        fog.color = sky_color;
        let glow = Vec3::new(1.0, 0.95, 0.85).lerp(Vec3::new(0.75, 0.82, 1.0), moon_t);
        fog.directional_light_color = Color::srgba(glow.x, glow.y, glow.z, 0.6);
    }

    for (disc, mut tf, mut vis) in &mut discs {
        let dir = if disc.is_sun { sun_dir } else { moon_dir };
        tf.translation = cam_pos + dir * CELESTIAL_DISTANCE_FT;
        let want = if dir.y > -0.05 {
            Visibility::Visible
        } else {
            Visibility::Hidden
        };
        if *vis != want {
            *vis = want;
        }
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
            if chunk_world.loaded.contains_key(&coord)
                || chunk_world.pending.contains(&coord)
                || chunk_is_queued(&chunk_world, coord)
            {
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
fn process_chunk_load_queue(
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
fn finish_chunk_tasks(
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
            .is_none_or(|pc| chunk_chebyshev_distance(coord, pc) <= CHUNK_UNLOAD_DISTANCE);
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
                base_x: pos.x,
                base_y: pos.y,
                base_z: pos.z,
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
                    base_x: lx,
                    base_y: y,
                    base_z: lz,
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
/// Drop a casting on the ground just behind the worm — whatever goes in one
/// end, a tidy dark block comes out the other.
fn spawn_worm_casting(
    commands: &mut Commands,
    casting: &CastingAssets,
    chunk_world: &ChunkWorld,
    cam: &Transform,
) {
    let f = *cam.forward();
    let mut back = -Vec3::new(f.x, 0.0, f.z).normalize_or_zero();
    if back == Vec3::ZERO {
        back = Vec3::Z;
    }
    let spot = cam.translation + back * (WORM_LENGTH * 1.3);
    let coord = world_to_chunk(spot.x, spot.z);
    let Some(&chunk_entity) = chunk_world.loaded.get(&coord) else {
        return;
    };
    let origin = chunk_world_origin(coord);
    let y = ground_world_y(chunk_world, spot.x, spot.z) + VOXEL_SIZE * 0.5;
    // Position-hashed yaw so a trail of castings doesn't align with the grid.
    let yaw = (spot.x * 12.9898 + spot.z * 78.233).sin() * std::f32::consts::PI;

    let (mesh, material) = (casting.mesh.clone(), casting.material.clone());
    commands.entity(chunk_entity).with_children(|chunk| {
        chunk.spawn((
            Mesh3d(mesh),
            MeshMaterial3d(material),
            Transform {
                translation: Vec3::new(spot.x - origin.x, y, spot.z - origin.z),
                rotation: Quat::from_rotation_y(yaw),
                ..default()
            },
        ));
    });
}

fn eat_leaves(
    mut commands: Commands,
    keyboard: Res<ButtonInput<KeyCode>>,
    sounds: Res<GameSounds>,
    castings: Res<CastingAssets>,
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
        spawn_worm_casting(&mut commands, &castings, &chunk_world, cam);
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
        for step in 0..32 {
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

    // A worm doesn't nibble single cubes — every bite scoops a worm-sized
    // BALL out of the ground (~4 inches across at 1-inch voxels), spilling
    // into neighbouring chunks when the bite straddles a border. Bedrock and
    // water are never carved.
    const BITE_RADIUS_VOX: f32 = 2.2;
    let center = IVec3::new(
        coord.x * CHUNK_VOXELS + local.x,
        local.y,
        coord.y * CHUNK_VOXELS + local.z,
    );
    let reach = BITE_RADIUS_VOX.ceil() as i32;
    let mut carved: HashMap<IVec2, Vec<IVec3>> = HashMap::new();

    for dy in -reach..=reach {
        for dz in -reach..=reach {
            for dx in -reach..=reach {
                let d = IVec3::new(dx, dy, dz);
                if d.as_vec3().length_squared() > BITE_RADIUS_VOX * BITE_RADIUS_VOX {
                    continue;
                }
                let w = center + d;
                let ccoord =
                    IVec2::new(w.x.div_euclid(CHUNK_VOXELS), w.z.div_euclid(CHUNK_VOXELS));
                let Some((_, seed, edits)) = records.iter().find(|(c, _, _)| *c == ccoord)
                else {
                    continue;
                };
                if !voxel_cache.contains_key(&ccoord) {
                    let mut v =
                        generate_chunk_blocks(ccoord, chunk_world_origin(ccoord), *seed);
                    apply_edits(&mut v, edits);
                    voxel_cache.insert(ccoord, v);
                }
                let voxels = voxel_cache.get_mut(&ccoord).expect("just inserted");

                let clocal =
                    IVec3::new(w.x.rem_euclid(CHUNK_VOXELS), w.y, w.z.rem_euclid(CHUNK_VOXELS));
                let Some(b) = voxels.get(clocal.x, clocal.y, clocal.z) else {
                    continue;
                };
                if b == BlockType::Water || clocal.y <= voxels.floor_y() {
                    continue;
                }
                voxels.clear_cell(clocal.x, clocal.y, clocal.z);
                carved.entry(ccoord).or_default().push(clocal);
            }
        }
    }

    let chunks: Vec<(IVec2, Vec<IVec3>, Mesh)> = carved
        .into_iter()
        .map(|(ccoord, locals)| {
            let mesh = terrain::build_colored_terrain_mesh(&voxel_cache[&ccoord]);
            (ccoord, locals, mesh)
        })
        .collect();

    if chunks.is_empty() {
        return None;
    }
    Some(BurrowResult { block, chunks })
}

/// Land finished bites: record the edit, swap the chunk's terrain mesh, munch.
fn finish_burrow_tasks(
    mut commands: Commands,
    sounds: Res<GameSounds>,
    castings: Res<CastingAssets>,
    mut chunk_world: ResMut<ChunkWorld>,
    mut archive: ResMut<ChunkArchive>,
    mut meshes: ResMut<Assets<Mesh>>,
    terrain_materials: Res<TerrainMaterials>,
    mut pending_q: Query<(Entity, &mut PendingBurrow)>,
    cam_q: Query<&Transform, With<Camera>>,
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

        // Persist each affected chunk's carved voxels wherever that chunk
        // lives now (it may have streamed out mid-chew — the tunnel still has
        // to be there on revisit), and swap in the rebuilt meshes.
        let mut ate_something = false;
        for (coord, locals, mesh) in result.chunks {
            if let Some(record) = chunk_world.active_records.get_mut(&coord) {
                record.edits.extend(locals.iter().copied());
            } else if let Some(record) = archive.saved.get_mut(&coord) {
                record.edits.extend(locals.iter().copied());
                continue; // No live mesh to swap.
            } else {
                continue;
            }
            ate_something = true;

            let Some(&chunk_entity) = chunk_world.loaded.get(&coord) else {
                continue;
            };
            if let Ok(children) = children_q.get(chunk_entity) {
                for child in children {
                    if surface_q.get(*child).is_ok() {
                        commands.entity(*child).despawn();
                    }
                }
            }
            let mesh = meshes.add(mesh);
            commands.entity(chunk_entity).with_children(|chunk| {
                chunk.spawn((
                    Mesh3d(mesh),
                    MeshMaterial3d(terrain_materials.vertex_color_terrain.clone()),
                    Transform::IDENTITY,
                    TerrainSurface,
                ));
            });
        }

        if !ate_something {
            continue;
        }
        commands.spawn((
            AudioPlayer::new(sounds.munch.clone()),
            PlaybackSettings::DESPAWN,
        ));
        if let Ok(cam) = cam_q.get_single() {
            spawn_worm_casting(&mut commands, &castings, &chunk_world, cam);
        }
        println!("🪱 Scooped a worm-sized bite of {:?}.", result.block);
    }
}

/// Animates the floating 3D leaves (now with real thickness).
/// They bob and spin — and they answer the wind: drifting downwind, leaning
/// with it, and spinning faster the harder it blows.
fn animate_floating_leaves(
    time: Res<Time>,
    wind: Res<Wind>,
    mut query: Query<(&mut Transform, &FloatingLeaf)>,
) {
    let t = time.elapsed_secs();
    let force = wind.strength / 5.0;
    let lean_axis = Vec3::new(-wind.dir.y, 0.0, wind.dir.x);

    for (mut transform, floating) in &mut query {
        // Gentle vertical bob, a little choppier when the wind is up.
        let bob = (t * floating.bob_speed * (1.0 + force * 0.8) + floating.phase).sin() * 0.20;
        transform.translation.y = floating.base_y + bob;

        // Downwind drift: tethered to the spawn point, straining with gusts.
        let drift = force * (0.22 + 0.12 * (t * 1.3 + floating.phase).sin());
        transform.translation.x = floating.base_x + wind.dir.x * drift;
        transform.translation.z = floating.base_z + wind.dir.y * drift;

        // Spin around Y — a gale whips leaves into a proper twirl.
        let spin = Quat::from_rotation_y(
            t * floating.spin_speed * (1.0 + force * 1.6) + floating.phase * 0.5,
        );

        // Combine:
        // - A downwind lean that grows with the wind
        // - The artistic base rotation the leaf was given at spawn
        // - Y spin for rotation
        // - Strong vertical orientation so the plane stands up instead of lying flat
        let lean = Quat::from_axis_angle(lean_axis, force * 0.4);
        let vertical_stand = Quat::from_rotation_x(-1.4);
        transform.rotation = lean * spin * vertical_stand * floating.base_rotation;
    }
}

/// Places the flycam at world origin (the chosen spawn biome) at worm eye level,
/// just above the surface voxel so you start crawling on the ground rather than
/// clipped inside it. Also dresses the camera with distance fog so faraway
/// giants haze out into the sky instead of rendering pin-sharp.
fn lower_worm_camera(
    mut commands: Commands,
    mut query: Query<(Entity, &mut Transform, &mut Camera)>,
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

    for (entity, mut transform, mut camera) in &mut query {
        transform.translation.x = 0.0;
        transform.translation.z = 0.0;
        transform.translation.y = eye_y;

        // HDR + bloom: the sun's emissive disc overdrives past 1.0 and blooms
        // into a brilliant glare instead of clipping to flat white.
        camera.hdr = true;
        commands.entity(entity).insert(Bloom::NATURAL);

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
