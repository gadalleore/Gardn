use bevy::prelude::*;

/// World units are feet. Shared 4-inch voxels match tree trunk blocks.
pub const INCH: f32 = 1.0 / 12.0;
pub const VOXEL_INCHES: i32 = 4;
pub const VOXEL_SIZE: f32 = VOXEL_INCHES as f32 * INCH;
pub const VOXELS_PER_FOOT: i32 = 12 / VOXEL_INCHES;

pub const WORM_LENGTH: f32 = 3.0 * INCH;
pub const WORM_EYE_HEIGHT: f32 = 1.5 * INCH;

/// Minecraft-style horizontal chunk — 64 ft wide = 192 voxels at 4-inch scale.
pub const CHUNK_SIZE: f32 = 64.0;
pub const CHUNK_VOXELS: i32 = (CHUNK_SIZE / VOXEL_SIZE) as i32;
/// Underground depth in voxels (16 × 4″ ≈ 5.3 ft below surface).
pub const CHUNK_DEPTH_VOXELS: i32 = 16;
pub const SURFACE_VOXEL_Y: i32 = 0;

pub const WORLD_SEED: u64 = 0xE0CA1E52_2026;
pub const CHUNK_VIEW_DISTANCE: i32 = 1;
pub const CHUNK_UNLOAD_DISTANCE: i32 = 2;
pub const CHUNKS_PER_FRAME: usize = 1;
pub const TREE_SPAWN_INTERVAL_SECS: f32 = 0.35;
pub const MAX_TREE_QUEUE: usize = 10;

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

pub fn world_to_geo(world_x: f32, world_z: f32) -> (f32, f32) {
    let lon = AUSTRALIA_CENTER_LON + (world_x / AUSTRALIA_WIDTH_FT) * 40.0;
    let lat = AUSTRALIA_CENTER_LAT - (world_z / AUSTRALIA_HEIGHT_FT) * 34.0;
    (lat, lon)
}

pub fn geo_to_normalized(lat: f32, lon: f32) -> Vec2 {
    let u = (lon - 113.0) / 40.5;
    let v = (-10.0 - lat) / 34.5;
    Vec2::new(u.clamp(0.0, 1.0), v.clamp(0.0, 1.0))
}