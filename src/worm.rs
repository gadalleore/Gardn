//! The worm (the player): gravity + collision that keeps the little guy on the
//! ground (with a `G` god-mode flight toggle), the camera setup, and eating —
//! leaves up close, otherwise burrowing a worm-sized ball out of the soil on a
//! background thread and leaving a casting behind. `WormPlugin` wires the
//! movement/camera/god-mode systems; `eat_leaves` + `finish_burrow_tasks` are
//! `pub(crate)` because main keeps them in the ordered world pipeline.

use bevy::audio::SpatialListener;
use bevy::core_pipeline::bloom::Bloom;
use bevy::pbr::{DistanceFog, FogFalloff};
use bevy::prelude::*;
use bevy::tasks::futures_lite::future;
use bevy::tasks::{block_on, AsyncComputeTaskPool, Task};
use bevy_flycam::prelude::*;
use std::collections::HashMap;

use crate::audio::GameSounds;
use crate::chunk_store::{ChunkArchive, ChunkRecord};
use crate::distance_blur::DistanceBlur;
use crate::leaves::Leaf;
use crate::terrain;
use crate::terrain::{
    apply_edits, generate_chunk_blocks, BlockType, ChunkVoxels, TerrainMaterials, TerrainSurface,
};
use crate::topography::{is_cave_cell, surface_height_voxels, surface_top_world_y};
use crate::streaming::ChunkWorld;
use crate::weather::{Wind, WIND_PUSH_FROM};
use crate::world::*;

pub struct WormPlugin;

impl Plugin for WormPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, setup_castings)
            .add_systems(PostStartup, lower_worm_camera)
            .add_systems(Update, toggle_god_mode)
            // After the flycam has moved the camera, before transforms propagate.
            .add_systems(
                PostUpdate,
                worm_gravity.before(bevy::transform::TransformSystem::TransformPropagate),
            );
    }
}

/// Shared handles for the little dark casting cube the worm drops after eating.
fn setup_castings(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    commands.insert_resource(CastingAssets {
        mesh: meshes.add(Cuboid::from_length(VOXEL_SIZE)),
        material: materials.add(StandardMaterial {
            base_color: Color::srgb(0.26, 0.19, 0.13),
            perceptual_roughness: 1.0,
            ..default()
        }),
    });
}

/// Shared handles for worm castings — the little dark cube a worm leaves on
/// the ground behind it after eating anything (dirt or leaf alike).
#[derive(Resource)]
pub(crate) struct CastingAssets {
    mesh: Handle<Mesh>,
    material: Handle<StandardMaterial>,
}

/// Worm gravity. The little guy has weight: no god mode, no hovering — he
/// falls to the ground and stays on it. `G` toggles god mode (free flight,
/// the old behaviour) for inspecting the giants.
const GRAVITY_FT_S2: f32 = 32.0;
const TERMINAL_FALL_FT_S: f32 = 90.0;
/// Crawl speed, and the god-mode flight multiplier on top of it.
pub(crate) const WORM_SPEED: f32 = 1.8;
pub(crate) const GOD_SPEED_MULT: f32 = 3.0;

#[derive(Resource)]
pub(crate) struct GodMode {
    pub(crate) enabled: bool,
    fall_speed: f32,
    /// Current upward stretch from holding Space — a worm can *reach*, not
    /// jump (jumping waits for the "legs" branch of the skill tree).
    reach: f32,
    /// Camera position at the end of last frame's physics — the difference is
    /// this frame's crawl input, which the wind gets to argue with.
    prev_pos: Option<Vec3>,
}

impl GodMode {
    pub(crate) fn from_env() -> Self {
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

fn toggle_god_mode(
    keys: Res<ButtonInput<KeyCode>>,
    mut god: ResMut<GodMode>,
    mut movement: ResMut<MovementSettings>,
) {
    if keys.just_pressed(KeyCode::KeyG) {
        god.enabled = !god.enabled;
        god.fall_speed = 0.0;
        if god.enabled {
            movement.speed = WORM_SPEED * GOD_SPEED_MULT;
            println!("👼 God mode ON — 3× flight (the world stays solid).");
        } else {
            movement.speed = WORM_SPEED;
            println!("🪱 God mode OFF — gravity has you.");
        }
    }
}

/// The ground under (x, z): the terrain surface, sunk through natural caves
/// and any voxels the worm has eaten — digging (or a cave mouth) really
/// lowers your floor.
pub(crate) fn ground_world_y(chunk_world: &ChunkWorld, x: f32, z: f32) -> f32 {
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
    let bedrock = surface - crate::topography::DIGGABLE_DEPTH_VOXELS + 2;
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
            bedrock: surface - crate::topography::DIGGABLE_DEPTH_VOXELS + 2,
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
/// The camera eye must clear solid voxels not just in its own 3-inch column
/// but a near-plane's width to either side — otherwise it can rest flush on a
/// wall face and Bevy's ~0.1 ft near plane renders *through* the block. Fresh
/// dug holes wall the worm in on every side, so that flush-poke was the "clip
/// through terrain when you dig" bug. The eye gets a thin horizontal body
/// radius, just over the near plane, used by collision and the roof clamps.
const NEAR_PLANE_MARGIN_FT: f32 = 0.12;

/// True if the eye at (x, z, y) clears solid ground at its own column and a
/// near-plane margin out in all 8 surrounding directions — a thin square body
/// radius. The diagonals matter: a single voxel jutting out of a dug-hole wall
/// sits at the corner *between* the cardinal probes, so a plus-shaped test let
/// the eye walk right up to its edge and the near plane poked through. The full
/// ring catches that corner.
fn eye_clears(chunk_world: &ChunkWorld, x: f32, z: f32, y: f32) -> bool {
    const M: f32 = NEAR_PLANE_MARGIN_FT;
    for (dx, dz) in [
        (0.0, 0.0),
        (M, 0.0),
        (-M, 0.0),
        (0.0, M),
        (0.0, -M),
        (M, M),
        (M, -M),
        (-M, M),
        (-M, -M),
    ] {
        let col = ColumnProbe::at(chunk_world, x + dx, z + dz);
        if col.solid_at_ft(y) || col.solid_at_ft(y - WORM_EYE_HEIGHT * 0.6) {
            return false;
        }
    }
    true
}

fn worm_gravity(
    time: Res<Time>,
    keys: Res<ButtonInput<KeyCode>>,
    wind: Res<Wind>,
    mut god: ResMut<GodMode>,
    chunk_world: Res<ChunkWorld>,
    mut cam_q: Query<&mut Transform, With<Camera>>,
) {
    let Ok(mut tf) = cam_q.get_single_mut() else {
        return;
    };

    if god.enabled {
        // God mode: no gravity, 3× flight — but the world stays solid. The
        // same body test blocks flying into walls (with axis sliding), a
        // vertical revert stops phasing through roofs and floors, and a
        // final clamp keeps the camera out of the ground.
        if let Some(prev) = god.prev_pos {
            let fits = |x: f32, z: f32, y: f32| -> bool { eye_clears(&chunk_world, x, z, y) };
            let y = tf.translation.y;
            if !fits(tf.translation.x, tf.translation.z, y) {
                if fits(tf.translation.x, prev.z, y) {
                    tf.translation.z = prev.z;
                } else if fits(prev.x, tf.translation.z, y) {
                    tf.translation.x = prev.x;
                } else {
                    tf.translation.x = prev.x;
                    tf.translation.z = prev.z;
                }
            }
            // Still inside solid after the horizontal slide (dived or rose
            // into it): undo the vertical part too.
            if !fits(tf.translation.x, tf.translation.z, tf.translation.y) {
                tf.translation.y = prev.y;
            }
        }
        let col = ColumnProbe::at(&chunk_world, tf.translation.x, tf.translation.z);
        let floor = col.floor_below(tf.translation.y + 0.02);
        if tf.translation.y < floor + WORM_EYE_HEIGHT * 0.5 {
            tf.translation.y = floor + WORM_EYE_HEIGHT * 0.5;
        }
        god.prev_pos = Some(tf.translation);
        return;
    }

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
        let fits = |x: f32, z: f32, y: f32| -> bool { eye_clears(&chunk_world, x, z, y) };
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
    let stand = (floor + WORM_EYE_HEIGHT + god.reach).min(ceiling - NEAR_PLANE_MARGIN_FT).max(floor + 0.02);
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

    // Hard anti-clip: however far the eased glide is lagging, never let the
    // camera sit inside the ground. The coarse 3-inch steps outrun the glide,
    // so snap up to a clear ride height above the floor directly below (capped
    // under any ceiling so it can't shove through a low roof).
    let min_ride = (here + WORM_EYE_HEIGHT * 0.5).min(ceiling - NEAR_PLANE_MARGIN_FT);
    if tf.translation.y < min_ride {
        tf.translation.y = min_ride;
    }

    god.prev_pos = Some(tf.translation);
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
pub(crate) struct PendingBurrow {
    task: Task<Option<BurrowResult>>,
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

pub(crate) fn eat_leaves(
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
                // Only soils are food — rock, ore, bedrock and water refuse
                // the bite (worm_edible); the bottom layer is bedrock too.
                if block.worm_edible() && local.y > voxels.floor_y() {
                    hit = Some((coord, local, block));
                }
                break 'rays;
            }
        }
    }

    let (coord, local, block) = hit?;

    // A worm doesn't nibble single cubes — every bite scoops a worm-sized
    // BALL out of the ground (~4 inches across at 1-inch voxels), spilling
    // into neighbouring chunks when the bite straddles a border. Only soils
    // are carved — rock, ore, bedrock and water are never eaten.
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
                if !b.worm_edible() || clocal.y <= voxels.floor_y() {
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
pub(crate) fn finish_burrow_tasks(
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

        // The worm's ears: spatial audio (the wind streamers) is heard from
        // here, panned and attenuated by where each sound sits relative to the
        // camera.
        commands.entity(entity).insert(SpatialListener::default());

        // Far clip a little past the silhouette ring (~768 ft) so distant trees
        // render before being clipped; the fog hazes them into the sky short of
        // that wall. Pushed out for the titan-landmark scale — a 1000 ft crown a
        // few hundred feet away needs real vertical room before the clip plane.
        commands.entity(entity).insert(Projection::Perspective(PerspectiveProjection {
            far: 1600.0,
            // Tiny near plane so the eye can sit right up against a wall — fresh
            // dug holes box the worm in on every side — without the near plane
            // (a rectangle whose corners sit deeper than its centre) slicing
            // through the block and showing daylight beyond. The default 0.1 ft
            // corner reached ~0.13 ft, past the collision margin, so glancing
            // angles clipped. Reverse-z depth keeps precision fine to 1600 ft.
            near: 0.02,
            ..default()
        }));

        // Background-only distance BLUR (see src/distance_blur.rs): the world
        // stays razor sharp out to `start`, then softens with distance so the
        // far titans go dreamy while the grass at the worm's nose is crisp. A
        // physical depth-of-field can't do this — focused past the grass it
        // blurs the near foreground at least as hard as the far trees (optics).
        // Msaa must be off so the depth buffer is a plain (non-MSAA) texture the
        // blur shader can sample.
        commands.entity(entity).insert(Msaa::Off);
        commands.entity(entity).insert(DistanceBlur {
            // Razor-sharp only in the worm's immediate 18 ft, then softening
            // across the grass field so distant clumps read fuzzy while the
            // ones underfoot stay crisp — full blur by 260 ft. Tune `start` for
            // where softening begins, `end` for how far the sharp-to-soft
            // gradient stretches, `max_blur` for how dreamy the horizon gets.
            start: 18.0,
            end: 260.0,
            max_blur: 26.0,
            // The blur is a worm's-eye effect; climbing to inspect the giants
            // from the air fades it out (200 ft above ground → gone by 500 ft),
            // so the sky view reads crisp instead of muddy.
            sky_fade_start: 200.0,
            sky_fade_end: 500.0,
        });

        // A light fog stays, only to melt the very far edge into the sky (so the
        // ring's horizon has no hard cut) — thin enough that it barely dims and
        // the sun still cuts through.
        commands.entity(entity).insert(DistanceFog {
            color: Color::srgb(0.58, 0.72, 0.88),
            directional_light_color: Color::srgba(1.0, 0.95, 0.85, 0.6),
            directional_light_exponent: 40.0,
            falloff: FogFalloff::Linear {
                start: 650.0,
                end: 1350.0,
            },
        });
    }
}

