use bevy::prelude::*;
use std::sync::OnceLock;

/// World units are feet. Ground runs on a fine 1-inch voxel so terrain reads
/// as soil rather than masonry to a 3-inch worm; trees keep their own chunkier
/// 2-inch grid (halving tree voxels would 8× their voxel volume and blow the
/// VRAM budgets for the giants).
pub const INCH: f32 = 1.0 / 12.0;
pub const VOXEL_INCHES: i32 = 1;
pub const VOXEL_SIZE: f32 = VOXEL_INCHES as f32 * INCH;
pub const VOXELS_PER_FOOT: i32 = 12 / VOXEL_INCHES;

/// Tree voxel grid — bark and foliage rasterise at this size.
pub const TREE_VOXEL_INCHES: i32 = 2;
pub const TREE_VOXEL_SIZE: f32 = TREE_VOXEL_INCHES as f32 * INCH;
pub const TREE_VOXELS_PER_FOOT: i32 = 12 / TREE_VOXEL_INCHES;

pub const WORM_LENGTH: f32 = 3.0 * INCH;
pub const WORM_EYE_HEIGHT: f32 = 2.5 * INCH;
/// Holding Space stretches the worm upward this far — reach, not flight.
pub const WORM_REACH: f32 = 3.0 * INCH;

/// Minecraft-style horizontal chunk — 32 ft wide = 192 voxels at 2-inch scale
/// (chunk shrinks with the voxel so per-chunk generation cost stays constant).
pub const CHUNK_SIZE: f32 = 32.0;
pub const CHUNK_VOXELS: i32 = (CHUNK_SIZE / VOXEL_SIZE) as i32;
/// Underground depth in voxels below the chunk's lowest surface (48 × 1″ =
/// 4 ft — 16 worm-lengths of diggable dark; caves live in this band).
pub const CHUNK_DEPTH_VOXELS: i32 = 48;
/// Sea level: ocean water tops out at voxel y = 0; land surface rises above it.
pub const SEA_LEVEL_VOXEL_Y: i32 = 0;
/// Tallest terrain in voxels above sea level (150 ft — BIG mountains: to a
/// 3-inch worm that summit is 600 worm-lengths of dirt in the sky).
pub const MAX_SURFACE_VOXEL_Y: i32 = 150 * VOXELS_PER_FOOT;

pub const WORLD_SEED: u64 = 0xE0CA1E52_2026;
pub const CHUNK_VIEW_DISTANCE: i32 = 2;
pub const CHUNK_UNLOAD_DISTANCE: i32 = 3;
/// Beyond the streamed chunks, tree silhouettes stay visible out to this many
/// chunks (~640 ft) so the giants dominate the horizon without costing meshes.
pub const SILHOUETTE_CHUNK_DISTANCE: i32 = 20;
pub const SILHOUETTES_PER_FRAME: usize = 8;
pub const CHUNKS_PER_FRAME: usize = 1;
/// Giant trees are built (voxels + mesh) on background compute threads; this
/// caps how many build in parallel so the pool isn't swamped.
pub const MAX_CONCURRENT_TREE_BUILDS: usize = 4;
pub const MAX_TREE_QUEUE: usize = 24;

/// Real Australia spans ~4000 km × 3200 km — mapped onto world feet.
pub const AUSTRALIA_WIDTH_KM: f32 = 4000.0;
pub const AUSTRALIA_HEIGHT_KM: f32 = 3200.0;
pub const KM_TO_FEET: f32 = 3280.84;
pub const AUSTRALIA_WIDTH_FT: f32 = AUSTRALIA_WIDTH_KM * KM_TO_FEET;
pub const AUSTRALIA_HEIGHT_FT: f32 = AUSTRALIA_HEIGHT_KM * KM_TO_FEET;
/// Geographic centre near Alice Springs / Simpson Desert.
pub const AUSTRALIA_CENTER_LAT: f32 = -25.5;
pub const AUSTRALIA_CENTER_LON: f32 = 134.0;

/// Tiny seeded RNG — stable procedural generation across runs.
pub struct GardenRng {
    state: u64,
}

impl GardenRng {
    pub fn new(seed: u64) -> Self {
        Self { state: seed }
    }

    pub fn next_u32(&mut self) -> u32 {
        self.state = self
            .state
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        (self.state >> 32) as u32
    }

    pub fn next_f32(&mut self) -> f32 {
        self.next_u32() as f32 / u32::MAX as f32
    }

    pub fn range(&mut self, min: f32, max: f32) -> f32 {
        min + self.next_f32() * (max - min)
    }

    pub fn range_i(&mut self, min: i32, max: i32) -> i32 {
        min + (self.next_f32() * (max - min + 1) as f32).floor() as i32
    }

    pub fn chance(&mut self, probability: f32) -> bool {
        self.next_f32() < probability
    }

    pub fn choice_i(&mut self, options: &[i32]) -> i32 {
        options[(self.next_f32() * options.len() as f32).floor() as usize % options.len()]
    }
}

pub fn chunk_seed(world_seed: u64, coord: IVec2) -> u64 {
    let mut h = world_seed;
    h = h
        .wrapping_mul(6364136223846793005)
        .wrapping_add(coord.x as u64);
    h = h
        .wrapping_mul(6364136223846793005)
        .wrapping_add(coord.y as u64);
    h
}

pub fn world_to_chunk(world_x: f32, world_z: f32) -> IVec2 {
    IVec2::new(
        (world_x / CHUNK_SIZE).floor() as i32,
        (world_z / CHUNK_SIZE).floor() as i32,
    )
}

pub fn chunk_world_origin(coord: IVec2) -> Vec3 {
    Vec3::new(coord.x as f32 * CHUNK_SIZE, 0.0, coord.y as f32 * CHUNK_SIZE)
}

pub fn chunk_chebyshev_distance(a: IVec2, b: IVec2) -> i32 {
    let d = a - b;
    d.x.abs().max(d.y.abs())
}

/// Where world-origin sits on the continent. Set once at startup so each new
/// game begins in a different, randomly chosen biome — without moving the player
/// to a far-from-origin world coordinate (which would wreck f32 precision).
static SPAWN_GEO_OFFSET: OnceLock<Vec2> = OnceLock::new();

pub fn set_spawn_geo_offset(offset: Vec2) {
    let _ = SPAWN_GEO_OFFSET.set(offset);
}

fn spawn_geo_offset() -> Vec2 {
    SPAWN_GEO_OFFSET.get().copied().unwrap_or(Vec2::ZERO)
}

/// Absolute continent-space feet (relative to the continent centre) for a world
/// position — the spawn offset folded in. Topography noise samples in this space
/// so the terrain shape is pinned to the real region of Australia, not to
/// wherever this game's origin happens to sit.
pub fn world_to_continental(world_x: f32, world_z: f32) -> Vec2 {
    let off = spawn_geo_offset();
    Vec2::new(world_x + off.x, world_z + off.y)
}

pub fn world_to_geo(world_x: f32, world_z: f32) -> (f32, f32) {
    let off = spawn_geo_offset();
    let lon = AUSTRALIA_CENTER_LON + ((world_x + off.x) / AUSTRALIA_WIDTH_FT) * 40.0;
    let lat = AUSTRALIA_CENTER_LAT - ((world_z + off.y) / AUSTRALIA_HEIGHT_FT) * 34.0;
    (lat, lon)
}

/// Inverse of the *unshifted* mapping: the world offset that places a given
/// lat/lon at world origin. Used to seed [`set_spawn_geo_offset`].
pub fn geo_to_world_offset(lat: f32, lon: f32) -> Vec2 {
    let x = (lon - AUSTRALIA_CENTER_LON) / 40.0 * AUSTRALIA_WIDTH_FT;
    let z = (AUSTRALIA_CENTER_LAT - lat) / 34.0 * AUSTRALIA_HEIGHT_FT;
    Vec2::new(x, z)
}

pub fn geo_to_normalized(lat: f32, lon: f32) -> Vec2 {
    let u = (lon - 113.0) / 40.5;
    let v = (-10.0 - lat) / 34.5;
    Vec2::new(u.clamp(0.0, 1.0), v.clamp(0.0, 1.0))
}