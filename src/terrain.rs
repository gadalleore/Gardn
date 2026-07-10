use bevy::prelude::*;
use bevy::asset::RenderAssetUsages;
use bevy::render::mesh::{Indices, PrimitiveTopology};
use std::collections::{HashMap, HashSet};

use crate::australia::{biome_at_world, biome_profile, AussieBiome};
use crate::topography::{is_cave_cell, surface_height_voxels, CAVE_CELL_VOXELS};
use crate::world::{
    GardenRng, CHUNK_DEPTH_VOXELS, CHUNK_SIZE, CHUNK_VOXELS, SEA_LEVEL_VOXEL_Y, VOXEL_SIZE,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BlockType {
    Grass,
    Dirt,
    RedSand,
    Sand,
    Sandstone,
    Laterite,
    Limestone,
    Stone,
    IronOre,
    BauxiteOre,
    CoalOre,
    CopperOre,
    GoldOre,
    UraniumOre,
    LeadZincOre,
    OpalOre,
    Water,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum OreType {
    Iron,
    Bauxite,
    Coal,
    Copper,
    Gold,
    Uranium,
    LeadZinc,
    Opal,
}

impl OreType {
    pub fn to_block(self) -> BlockType {
        match self {
            OreType::Iron => BlockType::IronOre,
            OreType::Bauxite => BlockType::BauxiteOre,
            OreType::Coal => BlockType::CoalOre,
            OreType::Copper => BlockType::CopperOre,
            OreType::Gold => BlockType::GoldOre,
            OreType::Uranium => BlockType::UraniumOre,
            OreType::LeadZinc => BlockType::LeadZincOre,
            OreType::Opal => BlockType::OpalOre,
        }
    }
}

#[derive(Resource)]
pub struct TerrainMaterials {
    pub vertex_color_terrain: Handle<StandardMaterial>,
}

impl TerrainMaterials {
    pub fn new(materials: &mut Assets<StandardMaterial>) -> Self {
        Self {
            vertex_color_terrain: materials.add(StandardMaterial {
                base_color: Color::WHITE,
                ..default()
            }),
        }
    }
}

/// Owns the shared terrain material. (The voxel-generation and mesh builders in
/// this module are plain functions the streamer/silhouettes call — no systems of
/// their own — so this plugin just does the one bit of terrain setup.)
pub struct TerrainPlugin;

impl Plugin for TerrainPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, setup_terrain_materials);
    }
}

fn setup_terrain_materials(mut commands: Commands, mut materials: ResMut<Assets<StandardMaterial>>) {
    commands.insert_resource(TerrainMaterials::new(&mut materials));
}

struct OreVein {
    ore: OreType,
    center: IVec3,
    radius: IVec3,
    strength: f32,
}

/// Dense voxel grid for one chunk column. Replaces the old
/// `HashMap<BlockType, HashSet<IVec3>>` representation: a flat `Vec` indexed by
/// (x, y, z) gives O(1) neighbour lookups for face culling instead of millions
/// of hash probes per chunk. `None` = air.
pub struct ChunkVoxels {
    size_x: i32,
    size_z: i32,
    y_min: i32,
    y_max: i32,
    solid_count: usize,
    cells: Vec<Option<BlockType>>,
}

impl ChunkVoxels {
    fn new(size_x: i32, size_z: i32, y_min: i32, y_max: i32) -> Self {
        let len = (size_x * size_z * (y_max - y_min + 1)) as usize;
        Self {
            size_x,
            size_z,
            y_min,
            y_max,
            solid_count: 0,
            cells: vec![None; len],
        }
    }

    #[inline(always)]
    fn index(&self, x: i32, y: i32, z: i32) -> usize {
        (((y - self.y_min) * self.size_z + z) * self.size_x + x) as usize
    }

    #[inline(always)]
    fn in_bounds(&self, x: i32, y: i32, z: i32) -> bool {
        x >= 0
            && x < self.size_x
            && z >= 0
            && z < self.size_z
            && y >= self.y_min
            && y <= self.y_max
    }

    #[inline(always)]
    fn set(&mut self, x: i32, y: i32, z: i32, block: BlockType) {
        let i = self.index(x, y, z);
        if self.cells[i].is_none() {
            self.solid_count += 1;
        }
        self.cells[i] = Some(block);
    }

    #[inline(always)]
    pub fn get(&self, x: i32, y: i32, z: i32) -> Option<BlockType> {
        if self.in_bounds(x, y, z) {
            self.cells[self.index(x, y, z)]
        } else {
            None
        }
    }

    /// Carve one voxel out (burrowing). No-op if already air or out of bounds.
    pub fn clear_cell(&mut self, x: i32, y: i32, z: i32) {
        if self.in_bounds(x, y, z) {
            let i = self.index(x, y, z);
            if self.cells[i].is_some() {
                self.cells[i] = None;
                self.solid_count -= 1;
            }
        }
    }

    /// Lowest voxel layer — kept as unbreakable bedrock so burrows never open
    /// into the void below the world.
    pub fn floor_y(&self) -> i32 {
        self.y_min
    }

    pub fn is_empty(&self) -> bool {
        self.solid_count == 0
    }

    /// Highest solid voxel per column, indexed `[z * size_x + x]` — the
    /// authoritative ground the physics floor stands on (the height *formula*
    /// can disagree with the meshed terrain by a voxel or two, which is
    /// exactly enough to fall through a freshly eaten floor).
    pub fn column_tops(&self) -> Vec<i32> {
        let mut tops = vec![self.y_min - 1; (self.size_x * self.size_z) as usize];
        for lz in 0..self.size_z {
            for lx in 0..self.size_x {
                for y in (self.y_min..=self.y_max).rev() {
                    if self.get(lx, y, lz).is_some() {
                        tops[(lz * self.size_x + lx) as usize] = y;
                        break;
                    }
                }
            }
        }
        tops
    }
}

/// Re-carve every recorded bite into freshly generated chunk voxels.
pub fn apply_edits(voxels: &mut ChunkVoxels, edits: &HashSet<IVec3>) {
    for e in edits {
        voxels.clear_cell(e.x, e.y, e.z);
    }
}

pub fn generate_chunk_blocks(
    coord: IVec2,
    chunk_origin: Vec3,
    terrain_seed: u64,
) -> ChunkVoxels {
    let mut rng = GardenRng::new(terrain_seed);
    let center_world = chunk_origin + Vec3::new(CHUNK_SIZE * 0.5, 0.0, CHUNK_SIZE * 0.5);
    let profile = biome_profile(center_world.x, center_world.z);

    // Strides are in (1-inch) voxels — layer/height sampling stays at the same
    // physical spacing it had on the old 2-inch grid, so noise cost per chunk
    // is unchanged even though the voxel grid is 4× denser.
    const STRIDE: i32 = 16;
    // Height lattice is finer than the layer lattice so worm-scale micro-relief
    // (5 ft wavelengths) survives the interpolation.
    const HEIGHT_STRIDE: i32 = 8;
    let cols = CHUNK_VOXELS as usize;

    // Biome topography: sample the height function on a sparse lattice and
    // bilinearly interpolate per column — smooth slopes at a fraction of the
    // noise cost.
    let gcols = (CHUNK_VOXELS / HEIGHT_STRIDE) as usize + 1;
    let mut height_grid = vec![0f32; gcols * gcols];
    for gz in 0..gcols {
        for gx in 0..gcols {
            let wx = chunk_origin.x + (gx as i32 * HEIGHT_STRIDE) as f32 * VOXEL_SIZE;
            let wz = chunk_origin.z + (gz as i32 * HEIGHT_STRIDE) as f32 * VOXEL_SIZE;
            height_grid[gz * gcols + gx] = surface_height_voxels(wx, wz) as f32;
        }
    }

    // Coastline: land vs sea is decided per COLUMN, from the same polygon the
    // height formula and the far ground sample. The old code asked once at the
    // chunk centre and made the WHOLE chunk ocean or land, which quantised a
    // diagonal shore into a 32-ft checkerboard — flat water squares biting
    // into beaches and dry squares jutting into the sea, each disagreeing with
    // its neighbours (and the per-cell far ground) at every shared chunk edge.
    // The lattice tells us for free which chunks straddle the shore:
    // surface_height_voxels returns 0 ONLY on ocean (land clamps to >= 1), so
    // pure-land chunks skip the mask entirely and pay nothing. (Coast wiggles
    // thinner than the 2-ft lattice can slip past the straddle test — a
    // sub-lattice sliver, invisible at worm scale.)
    let lattice_ocean = height_grid.iter().filter(|h| **h == 0.0).count();
    let center_ocean = profile.biome == AussieBiome::Ocean;
    if lattice_ocean == height_grid.len() && center_ocean {
        // Open sea in every sample — the flat fast path.
        let mut voxels = ChunkVoxels::new(
            CHUNK_VOXELS,
            CHUNK_VOXELS,
            -CHUNK_DEPTH_VOXELS,
            SEA_LEVEL_VOXEL_Y,
        );
        fill_ocean_chunk(&mut voxels, &mut rng);
        return voxels;
    }
    let straddles_coast = lattice_ocean > 0 || center_ocean;
    let sea_mask: Vec<bool> = if straddles_coast {
        let mut mask = vec![false; cols * cols];
        for lz in 0..CHUNK_VOXELS {
            for lx in 0..CHUNK_VOXELS {
                let wx = chunk_origin.x + (lx as f32 + 0.5) * VOXEL_SIZE;
                let wz = chunk_origin.z + (lz as f32 + 0.5) * VOXEL_SIZE;
                mask[lz as usize * cols + lx as usize] =
                    biome_at_world(wx, wz) == AussieBiome::Ocean;
            }
        }
        mask
    } else {
        vec![false; cols * cols]
    };

    let mut heights = vec![0i32; cols * cols];
    let mut min_h = i32::MAX;
    let mut max_h = i32::MIN;
    for lz in 0..CHUNK_VOXELS {
        for lx in 0..CHUNK_VOXELS {
            let gx = (lx / HEIGHT_STRIDE) as usize;
            let gz = (lz / HEIGHT_STRIDE) as usize;
            let tx = (lx % HEIGHT_STRIDE) as f32 / HEIGHT_STRIDE as f32;
            let tz = (lz % HEIGHT_STRIDE) as f32 / HEIGHT_STRIDE as f32;
            let a = height_grid[gz * gcols + gx];
            let b = height_grid[gz * gcols + gx + 1];
            let c = height_grid[(gz + 1) * gcols + gx];
            let d = height_grid[(gz + 1) * gcols + gx + 1];
            let h = (a + (b - a) * tx + (c - a) * tz + (a - b - c + d) * tx * tz).round() as i32;
            heights[lz as usize * cols + lx as usize] = h;
            min_h = min_h.min(h);
            max_h = max_h.max(h);
        }
    }

    let mut voxels = ChunkVoxels::new(
        CHUNK_VOXELS,
        CHUNK_VOXELS,
        min_h - CHUNK_DEPTH_VOXELS,
        max_h,
    );

    let veins = plan_ore_veins(coord, terrain_seed, &profile, &mut rng, min_h);

    // Per-column surface layers, sampled every 8 voxels into a flat array
    // (was a 36k-entry HashMap). Smooth enough, 64× cheaper than per-voxel.
    let mut columns: Vec<ColumnLayers> =
        vec![column_layers(&mut rng, &profile); cols * cols];
    for lz in (0..CHUNK_VOXELS).step_by(STRIDE as usize) {
        for lx in (0..CHUNK_VOXELS).step_by(STRIDE as usize) {
            let wx = chunk_origin.x + (lx as f32 + 0.5) * VOXEL_SIZE;
            let wz = chunk_origin.z + (lz as f32 + 0.5) * VOXEL_SIZE;
            let col_profile = biome_profile(wx, wz);
            let layers = column_layers(&mut rng, &col_profile);
            for dz in 0..STRIDE {
                for dx in 0..STRIDE {
                    let px = (lx + dx).min(CHUNK_VOXELS - 1) as usize;
                    let pz = (lz + dz).min(CHUNK_VOXELS - 1) as usize;
                    columns[pz * cols + px] = layers;
                }
            }
        }
    }

    // Each column's soil stack rides its own surface height. Filled solid down
    // to the chunk-wide stone floor so even the steepest worm-mountain walls
    // never expose a hollow core (interior faces are culled away, so the mesh
    // stays the same size). Sea columns get the standard seabed profile
    // instead, so a mixed coastal chunk meets a full-ocean neighbour with the
    // exact same water/sand stack at the shared edge.
    let chunk_floor = min_h - CHUNK_DEPTH_VOXELS;
    for lz in 0..CHUNK_VOXELS {
        for lx in 0..CHUNK_VOXELS {
            if sea_mask[lz as usize * cols + lx as usize] {
                fill_seabed_column(&mut voxels, lx, lz, chunk_floor);
                continue;
            }
            let h = heights[lz as usize * cols + lx as usize];
            let layers = columns[lz as usize * cols + lx as usize];
            let soft_bottom = h - layers.dirt_depth - layers.soft_depth;

            for y in chunk_floor..=h {
                let block = if y == h {
                    layers.surface
                } else if y > h - layers.dirt_depth {
                    layers.subsurface
                } else if y > soft_bottom {
                    layers.soft_rock
                } else {
                    BlockType::Stone
                };
                voxels.set(lx, y, lz, block);
            }
        }
    }

    // Stamp ore veins directly over their bounding boxes instead of testing
    // every voxel against every vein (was ~25M checks/chunk, all underground).
    stamp_ore_veins(&mut voxels, &veins);

    // Carve the cave web — after ore stamping, so veins gleam in cave walls.
    // Decided per 8-inch lattice cell in world space (seamless across chunk
    // borders); the bottom two layers stay bedrock so caves never open into
    // the void.
    let base_wx = coord.x * CHUNK_VOXELS;
    let base_wz = coord.y * CHUNK_VOXELS;
    let carve_floor = chunk_floor + 2;
    let y_start = carve_floor.div_euclid(CAVE_CELL_VOXELS) * CAVE_CELL_VOXELS;
    for lz in (0..CHUNK_VOXELS).step_by(CAVE_CELL_VOXELS as usize) {
        for lx in (0..CHUNK_VOXELS).step_by(CAVE_CELL_VOXELS as usize) {
            let cx = (lx + CAVE_CELL_VOXELS / 2).min(CHUNK_VOXELS - 1);
            let cz = (lz + CAVE_CELL_VOXELS / 2).min(CHUNK_VOXELS - 1);
            let h = heights[cz as usize * cols + cx as usize];

            let mut y = y_start;
            while y <= h {
                if is_cave_cell(base_wx + lx, y, base_wz + lz, h) {
                    for dy in 0..CAVE_CELL_VOXELS {
                        let cy = y + dy;
                        if cy < carve_floor || cy > h {
                            continue;
                        }
                        for dz in 0..CAVE_CELL_VOXELS {
                            for dx in 0..CAVE_CELL_VOXELS {
                                // Never carve a sea column — a cave cell that
                                // straddles the waterline must not punch holes
                                // in the water sheet or the seabed.
                                let (cx, cz) = (lx + dx, lz + dz);
                                if sea_mask[cz as usize * cols + cx as usize] {
                                    continue;
                                }
                                voxels.clear_cell(cx, cy, cz);
                            }
                        }
                    }
                }
                y += CAVE_CELL_VOXELS;
            }
        }
    }

    voxels
}

#[derive(Clone, Copy)]
struct ColumnLayers {
    surface: BlockType,
    subsurface: BlockType,
    soft_rock: BlockType,
    dirt_depth: i32,
    soft_depth: i32,
}

fn column_layers(rng: &mut GardenRng, profile: &crate::australia::BiomeProfile) -> ColumnLayers {
    let (surface, subsurface, soft_rock) = match profile.biome {
        AussieBiome::AridOutback | AussieBiome::Pilbara => {
            if rng.chance(0.55) {
                (BlockType::RedSand, BlockType::Laterite, BlockType::Sandstone)
            } else {
                (BlockType::RedSand, BlockType::Dirt, BlockType::Sandstone)
            }
        }
        AussieBiome::TropicalSavanna => {
            (BlockType::Grass, BlockType::Dirt, BlockType::Limestone)
        }
        AussieBiome::Mediterranean | AussieBiome::TemperateForest | AussieBiome::Tasmania => {
            (BlockType::Grass, BlockType::Dirt, BlockType::Limestone)
        }
        AussieBiome::CoastalBush => {
            if rng.chance(0.15) {
                (BlockType::Sand, BlockType::Sand, BlockType::Sandstone)
            } else {
                (BlockType::Grass, BlockType::Dirt, BlockType::Sandstone)
            }
        }
        AussieBiome::Ocean => (BlockType::Water, BlockType::Sand, BlockType::Sandstone),
    };

    ColumnLayers {
        surface,
        subsurface,
        soft_rock,
        dirt_depth: rng.range_i(2, 4),
        soft_depth: rng.range_i(3, 6),
    }
}

/// One column of open sea: water sheet at sea level, a thin sand bed, hollow
/// dark below, bedrock at the floor. Shared by full-ocean chunks and the sea
/// columns of mixed coastal chunks so the two meet seamlessly at chunk edges.
fn fill_seabed_column(voxels: &mut ChunkVoxels, lx: i32, lz: i32, floor_y: i32) {
    for y in floor_y..=SEA_LEVEL_VOXEL_Y {
        let block = if y >= -2 {
            BlockType::Water
        } else if y > -6 {
            BlockType::Sand
        } else if y == floor_y {
            BlockType::Sandstone
        } else {
            continue;
        };
        voxels.set(lx, y, lz, block);
    }
}

fn fill_ocean_chunk(voxels: &mut ChunkVoxels, rng: &mut GardenRng) {
    for lz in 0..CHUNK_VOXELS {
        for lx in 0..CHUNK_VOXELS {
            fill_seabed_column(voxels, lx, lz, -CHUNK_DEPTH_VOXELS);
            if rng.chance(0.02) {
                voxels.set(lx, -8, lz, BlockType::Limestone);
            }
        }
    }
}

fn plan_ore_veins(
    _coord: IVec2,
    terrain_seed: u64,
    profile: &crate::australia::BiomeProfile,
    rng: &mut GardenRng,
    min_surface_h: i32,
) -> Vec<OreVein> {
    let mut veins = Vec::new();
    let total_weight: f32 = profile.ore_weights.iter().map(|(_, w)| w).sum();

    // Keep veins in the rock band below the lowest surface in the chunk so they
    // stay underground even where the terrain dips.
    let vein_floor = min_surface_h - CHUNK_DEPTH_VOXELS + 4;
    let vein_ceiling = (min_surface_h - 2).max(vein_floor);

    for (ore, weight) in profile.ore_weights {
        let share = weight / total_weight;
        let vein_count = (share * rng.range(4.0, 10.0)).round() as i32;
        for v in 0..vein_count {
            let vein_seed = terrain_seed
                ^ ((ore as u64) << 32)
                ^ (v as u64 * 0x9E37_79B9);
            let mut vr = GardenRng::new(vein_seed);
            veins.push(OreVein {
                ore,
                center: IVec3::new(
                    vr.range_i(4, CHUNK_VOXELS - 4),
                    vr.range_i(vein_floor, vein_ceiling),
                    vr.range_i(4, CHUNK_VOXELS - 4),
                ),
                radius: IVec3::new(
                    vr.range_i(4, 12),
                    vr.range_i(4, 8),
                    vr.range_i(4, 12),
                ),
                strength: vr.range(0.55, 0.92),
            });
        }
    }

    veins
}

/// Paint ore into solid voxels by walking each vein's bounding box. Where veins
/// overlap, the strongest score wins (same result as the old per-voxel scan, but
/// proportional to vein volume rather than chunk volume × vein count).
fn stamp_ore_veins(voxels: &mut ChunkVoxels, veins: &[OreVein]) {
    if veins.is_empty() {
        return;
    }
    // Best score per contested cell index — keeps "strongest vein wins".
    let mut best: HashMap<usize, f32> = HashMap::new();

    for (i, vein) in veins.iter().enumerate() {
        let rx = vein.radius.x.max(1);
        let ry = vein.radius.y.max(1);
        let rz = vein.radius.z.max(1);

        let y0 = (vein.center.y - vein.radius.y).max(voxels.y_min);
        let y1 = (vein.center.y + vein.radius.y).min(voxels.y_max);
        let z0 = (vein.center.z - vein.radius.z).max(0);
        let z1 = (vein.center.z + vein.radius.z).min(voxels.size_z - 1);
        let x0 = (vein.center.x - vein.radius.x).max(0);
        let x1 = (vein.center.x + vein.radius.x).min(voxels.size_x - 1);

        for y in y0..=y1 {
            for z in z0..=z1 {
                for x in x0..=x1 {
                    let idx = voxels.index(x, y, z);
                    // Ore only replaces existing solid ground — never air/caves,
                    // and never the sea (mixed coastal chunks have water above
                    // the vein ceiling; ore must not gleam in the waves).
                    match voxels.cells[idx] {
                        None | Some(BlockType::Water) => continue,
                        Some(_) => {}
                    }
                    let dx = (x - vein.center.x) as f32 / rx as f32;
                    let dy = (y - vein.center.y) as f32 / ry as f32;
                    let dz = (z - vein.center.z) as f32 / rz as f32;
                    let dist_sq = dx * dx + dy * dy + dz * dz;
                    if dist_sq > 1.0 {
                        continue;
                    }
                    let score = (1.0 - dist_sq) * vein.strength;
                    if block_hash(IVec3::new(x, y, z), i as u32) >= score {
                        continue;
                    }
                    if score > *best.get(&idx).unwrap_or(&0.0) {
                        best.insert(idx, score);
                        voxels.cells[idx] = Some(vein.ore.to_block());
                    }
                }
            }
        }
    }
}

fn block_hash(pos: IVec3, salt: u32) -> f32 {
    let mut h = pos.x as u32
        ^ pos.y.wrapping_mul(374761) as u32
        ^ pos.z.wrapping_mul(668265) as u32
        ^ salt.wrapping_mul(2_147_483_647);
    h = h.wrapping_mul(2_246_822_519);
    h as f32 / u32::MAX as f32
}

fn block_color(block: BlockType) -> [f32; 4] {
    let rgb = match block {
        BlockType::Grass => [0.32, 0.52, 0.22],
        BlockType::Dirt => [0.42, 0.30, 0.18],
        BlockType::RedSand => [0.72, 0.38, 0.22],
        BlockType::Sand => [0.82, 0.74, 0.48],
        BlockType::Sandstone => [0.74, 0.62, 0.42],
        BlockType::Laterite => [0.58, 0.28, 0.20],
        BlockType::Limestone => [0.78, 0.76, 0.68],
        BlockType::Stone => [0.48, 0.48, 0.50],
        BlockType::IronOre => [0.55, 0.36, 0.28],
        BlockType::BauxiteOre => [0.62, 0.42, 0.38],
        BlockType::CoalOre => [0.18, 0.18, 0.20],
        BlockType::CopperOre => [0.48, 0.58, 0.38],
        BlockType::GoldOre => [0.72, 0.62, 0.22],
        BlockType::UraniumOre => [0.42, 0.62, 0.32],
        BlockType::LeadZincOre => [0.52, 0.52, 0.58],
        BlockType::OpalOre => [0.62, 0.78, 0.88],
        BlockType::Water => [0.22, 0.42, 0.68],
    };
    [rgb[0], rgb[1], rgb[2], 1.0]
}

/// Face table shared by both meshers: (neighbour offset, normal, unit corners,
/// merge axis). Corners are voxel-unit offsets; when a run of `len` faces is
/// merged into one quad, the corner component on the merge axis is scaled by
/// `len`. Top/bottom faces merge along x; side faces merge along y (long
/// vertical strips on trunk and cliff walls).
const FACE_DIRS: [(IVec3, [f32; 3], [[i32; 3]; 4], usize); 6] = [
    (
        IVec3::X,
        [1.0, 0.0, 0.0],
        [[1, 0, 0], [1, 1, 0], [1, 1, 1], [1, 0, 1]],
        1,
    ),
    (
        IVec3::NEG_X,
        [-1.0, 0.0, 0.0],
        [[0, 0, 1], [0, 1, 1], [0, 1, 0], [0, 0, 0]],
        1,
    ),
    (
        IVec3::Y,
        [0.0, 1.0, 0.0],
        [[0, 1, 1], [1, 1, 1], [1, 1, 0], [0, 1, 0]],
        0,
    ),
    (
        IVec3::NEG_Y,
        [0.0, -1.0, 0.0],
        [[0, 0, 0], [1, 0, 0], [1, 0, 1], [0, 0, 1]],
        0,
    ),
    (
        IVec3::Z,
        [0.0, 0.0, 1.0],
        [[1, 0, 1], [1, 1, 1], [0, 1, 1], [0, 0, 1]],
        1,
    ),
    (
        IVec3::NEG_Z,
        [0.0, 0.0, -1.0],
        [[0, 0, 0], [0, 1, 0], [1, 1, 0], [1, 0, 0]],
        1,
    ),
];

fn push_merged_quad(
    positions: &mut Vec<[f32; 3]>,
    normals: &mut Vec<[f32; 3]>,
    uvs: &mut Vec<[f32; 2]>,
    indices: &mut Vec<u32>,
    normal: [f32; 3],
    corners: [[i32; 3]; 4],
    base: [i32; 3],
    len: i32,
    merge_axis: usize,
    cell_size: f32,
) {
    let idx0 = positions.len() as u32;
    // UVs tile once per voxel (a merged strip spans 0..len), so a block skin
    // repeats along the strip instead of stretching. Needs a repeat sampler.
    let n_axis = if normal[0] != 0.0 {
        0
    } else if normal[1] != 0.0 {
        1
    } else {
        2
    };
    let (u_axis, v_axis) = match n_axis {
        0 => (2, 1),
        1 => (0, 2),
        _ => (0, 1),
    };

    for corner in corners {
        let mut p = [0f32; 3];
        let mut c_scaled = [0i32; 3];
        for k in 0..3 {
            c_scaled[k] = if k == merge_axis { corner[k] * len } else { corner[k] };
            p[k] = (base[k] + c_scaled[k]) as f32 * cell_size;
        }
        positions.push(p);
        normals.push(normal);
        uvs.push([c_scaled[u_axis] as f32, c_scaled[v_axis] as f32]);
    }
    indices.extend_from_slice(&[idx0, idx0 + 1, idx0 + 2, idx0, idx0 + 2, idx0 + 3]);
}

/// One combined mesh per chunk. Visible faces (neighbour is air/out-of-bounds)
/// are merged into strips along one axis — at 2-inch voxels this collapses flat
/// ground into long quads and keeps vertex counts sane.
pub fn build_colored_terrain_mesh(voxels: &ChunkVoxels) -> Mesh {
    let mut positions: Vec<[f32; 3]> = Vec::new();
    let mut normals: Vec<[f32; 3]> = Vec::new();
    let mut uvs: Vec<[f32; 2]> = Vec::new();
    let mut colors: Vec<[f32; 4]> = Vec::new();
    let mut indices: Vec<u32> = Vec::new();

    let lens = [
        voxels.size_x,
        voxels.y_max - voxels.y_min + 1,
        voxels.size_z,
    ];
    let offs = [0, voxels.y_min, 0];

    for (neighbor, normal, corners, merge_axis) in FACE_DIRS {
        let (o1_axis, o2_axis) = match merge_axis {
            0 => (1, 2),
            1 => (0, 2),
            _ => (0, 1),
        };

        let face_of = |cell: [i32; 3]| -> Option<BlockType> {
            let b = voxels.get(cell[0], cell[1], cell[2])?;
            if voxels
                .get(
                    cell[0] + neighbor.x,
                    cell[1] + neighbor.y,
                    cell[2] + neighbor.z,
                )
                .is_some()
            {
                None
            } else {
                Some(b)
            }
        };

        for i in 0..lens[o1_axis] {
            for j in 0..lens[o2_axis] {
                let mut w = 0;
                while w < lens[merge_axis] {
                    let mut cell = [0i32; 3];
                    cell[o1_axis] = i + offs[o1_axis];
                    cell[o2_axis] = j + offs[o2_axis];
                    cell[merge_axis] = w + offs[merge_axis];

                    let Some(block) = face_of(cell) else {
                        w += 1;
                        continue;
                    };

                    let mut len = 1;
                    while w + len < lens[merge_axis] {
                        let mut next = cell;
                        next[merge_axis] = w + len + offs[merge_axis];
                        match face_of(next) {
                            Some(b) if b == block => len += 1,
                            _ => break,
                        }
                    }

                    push_merged_quad(
                        &mut positions,
                        &mut normals,
                        &mut uvs,
                        &mut indices,
                        normal,
                        corners,
                        cell,
                        len,
                        merge_axis,
                        VOXEL_SIZE,
                    );
                    colors.extend([block_color(block); 4]);

                    w += len;
                }
            }
        }
    }

    let mut mesh = Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::default());
    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, positions);
    mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, normals);
    mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, uvs);
    mesh.insert_attribute(Mesh::ATTRIBUTE_COLOR, colors);
    mesh.insert_indices(Indices::U32(indices));
    mesh
}

/// Collapse fine voxels into `factor`-sized coarse cells for distant LOD
/// meshes: a coarse cell survives when enough of its fine voxels are filled
/// (`fill_ratio` of factor³), so the big blocks trace the average shape of the
/// cloud instead of swelling to its loosest outline. Output coords are on the
/// coarse grid — mesh them with `block_size * factor`.
pub fn downsample_blocks(blocks: &HashSet<IVec3>, factor: i32, fill_ratio: f32) -> HashSet<IVec3> {
    let mut counts: HashMap<IVec3, u32> = HashMap::new();
    for b in blocks {
        let cell = IVec3::new(
            b.x.div_euclid(factor),
            b.y.div_euclid(factor),
            b.z.div_euclid(factor),
        );
        *counts.entry(cell).or_insert(0) += 1;
    }

    let needed = (((factor * factor * factor) as f32 * fill_ratio) as u32).max(1);
    counts
        .into_iter()
        .filter(|(_, n)| *n >= needed)
        .map(|(cell, _)| cell)
        .collect()
}

/// Used by voxel trees (colour comes from StandardMaterial, so no vertex
/// colours; UVs let an optional block skin texture tile over the faces). Same
/// strip merging as the terrain mesher — trunk walls collapse into long
/// vertical quads.
pub fn build_culled_voxel_mesh(blocks: &HashSet<IVec3>, block_size: f32) -> Mesh {
    let mut positions: Vec<[f32; 3]> = Vec::new();
    let mut normals: Vec<[f32; 3]> = Vec::new();
    let mut uvs: Vec<[f32; 2]> = Vec::new();
    let mut indices: Vec<u32> = Vec::new();

    for (neighbor, normal, corners, merge_axis) in FACE_DIRS {
        let mut faces: Vec<IVec3> = blocks
            .iter()
            .copied()
            .filter(|b| !blocks.contains(&(*b + neighbor)))
            .collect();

        // Sort so cells that can merge (same coords on the other two axes,
        // consecutive on the merge axis) end up adjacent.
        faces.sort_unstable_by_key(|p| match merge_axis {
            0 => (p.y, p.z, p.x),
            1 => (p.x, p.z, p.y),
            _ => (p.x, p.y, p.z),
        });
        let step = match merge_axis {
            0 => IVec3::X,
            1 => IVec3::Y,
            _ => IVec3::Z,
        };

        let mut i = 0;
        while i < faces.len() {
            let start = faces[i];
            let mut len = 1i32;
            while i + (len as usize) < faces.len() && faces[i + len as usize] == start + step * len {
                len += 1;
            }

            push_merged_quad(
                &mut positions,
                &mut normals,
                &mut uvs,
                &mut indices,
                normal,
                corners,
                [start.x, start.y, start.z],
                len,
                merge_axis,
                block_size,
            );

            i += len as usize;
        }
    }

    let mut mesh = Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::default());
    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, positions);
    mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, normals);
    mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, uvs);
    mesh.insert_indices(Indices::U32(indices));
    mesh
}

/// Cull-and-mesh a set of coloured blocks (each cell carries its own vertex
/// colour). Used for the distant coarse voxel trees, where several species'
/// downsampled bark and foliage are merged into one chunk mesh — vertex colours
/// let the one shared vertex-colour material draw them all. Faces are emitted
/// per cell (no strip merging across colours), which is fine at the small block
/// counts a coarse tree produces.
pub fn build_colored_block_mesh(blocks: &HashMap<IVec3, [f32; 4]>, block_size: f32) -> Mesh {
    let mut positions: Vec<[f32; 3]> = Vec::new();
    let mut normals: Vec<[f32; 3]> = Vec::new();
    let mut colors: Vec<[f32; 4]> = Vec::new();
    let mut indices: Vec<u32> = Vec::new();

    for (&pos, &color) in blocks {
        for (neighbor, normal, corners, _) in FACE_DIRS {
            if blocks.contains_key(&(pos + neighbor)) {
                continue;
            }
            let i0 = positions.len() as u32;
            for corner in corners {
                positions.push([
                    (pos.x + corner[0]) as f32 * block_size,
                    (pos.y + corner[1]) as f32 * block_size,
                    (pos.z + corner[2]) as f32 * block_size,
                ]);
                normals.push(normal);
                colors.push(color);
            }
            indices.extend_from_slice(&[i0, i0 + 1, i0 + 2, i0, i0 + 2, i0 + 3]);
        }
    }

    let mut mesh = Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::default());
    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, positions);
    mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, normals);
    mesh.insert_attribute(Mesh::ATTRIBUTE_COLOR, colors);
    mesh.insert_indices(Indices::U32(indices));
    mesh
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::world::{chunk_seed, chunk_world_origin, WORLD_SEED};

    /// The 1-inch ground grid must stay affordable: one full chunk generates,
    /// meshes, and lands under a sane vertex ceiling.
    #[test]
    fn chunk_generation_stays_within_budget() {
        let coord = IVec2::new(3, -1);
        let origin = chunk_world_origin(coord);
        let blocks = generate_chunk_blocks(coord, origin, chunk_seed(WORLD_SEED, coord));
        assert!(!blocks.is_empty(), "chunk generated no voxels");
        let verts = build_colored_terrain_mesh(&blocks).count_vertices();
        assert!(
            verts > 0 && verts < 2_500_000,
            "chunk mesh has {verts} vertices — too heavy for streaming"
        );
    }

    /// A chunk straddling the coastline must carry BOTH sea and land columns,
    /// each matching the coastline polygon at that column — the old
    /// whole-chunk ocean test quantised a diagonal shore into 32-ft all-water
    /// or all-land squares that disagreed with their neighbours (and the
    /// per-cell far ground) at every shared chunk edge.
    #[test]
    fn coastal_chunks_split_land_and_water_per_column() {
        use crate::world::geo_to_world_offset;

        // Walk south across the Victorian west coast (unshifted geo mapping)
        // until the chunk-centre land/ocean answer flips, then test the chunks
        // around the crossing.
        let inland = geo_to_world_offset(-38.0, 141.0);
        let start = crate::world::world_to_chunk(inland.x, inland.y);
        let mut mixed = None;
        let mut prev_ocean = None;
        'scan: for dz in 0..4000 {
            let coord = start + IVec2::new(0, dz);
            let origin = chunk_world_origin(coord);
            // A chunk is a candidate once its 4 corners disagree about the sea.
            let mut corners_ocean = 0;
            for (cx, cz) in [(0.0, 0.0), (CHUNK_SIZE, 0.0), (0.0, CHUNK_SIZE), (CHUNK_SIZE, CHUNK_SIZE)]
            {
                if biome_at_world(origin.x + cx, origin.z + cz) == AussieBiome::Ocean {
                    corners_ocean += 1;
                }
            }
            if corners_ocean > 0 && corners_ocean < 4 {
                mixed = Some(coord);
                break 'scan;
            }
            let all_ocean = corners_ocean == 4;
            if prev_ocean == Some(false) && all_ocean {
                // Crossed the coast between two rows without a corner-mixed
                // chunk (coast ran between corners) — very unlikely, but bail
                // rather than assert on nothing.
                break 'scan;
            }
            prev_ocean = Some(all_ocean);
        }
        let coord = mixed.expect("no coast-straddling chunk found along the scan");

        let origin = chunk_world_origin(coord);
        let voxels = generate_chunk_blocks(coord, origin, chunk_seed(WORLD_SEED, coord));

        let mut sea_cols = 0;
        let mut land_cols = 0;
        for lz in 0..CHUNK_VOXELS {
            for lx in 0..CHUNK_VOXELS {
                let wx = origin.x + (lx as f32 + 0.5) * VOXEL_SIZE;
                let wz = origin.z + (lz as f32 + 0.5) * VOXEL_SIZE;
                let polygon_sea = biome_at_world(wx, wz) == AussieBiome::Ocean;
                let has_water = voxels.get(lx, SEA_LEVEL_VOXEL_Y, lz) == Some(BlockType::Water);
                assert_eq!(
                    polygon_sea, has_water,
                    "column ({lx},{lz}) of chunk {coord:?}: polygon says sea={polygon_sea} \
                     but generated water={has_water}"
                );
                if polygon_sea {
                    sea_cols += 1;
                } else {
                    land_cols += 1;
                }
            }
        }
        assert!(
            sea_cols > 0 && land_cols > 0,
            "chunk {coord:?} should straddle the coast (sea {sea_cols}, land {land_cols})"
        );
    }
}

/// Marks a chunk's terrain mesh child — burrowing despawns and rebuilds it.
#[derive(Component)]
pub struct TerrainSurface;

pub fn spawn_terrain_meshes(
    commands: &mut Commands,
    chunk_entity: Entity,
    meshes: &mut Assets<Mesh>,
    materials: &TerrainMaterials,
    voxels: ChunkVoxels,
) {
    if voxels.is_empty() {
        return;
    }

    let mesh = meshes.add(build_colored_terrain_mesh(&voxels));
    commands.entity(chunk_entity).with_children(|parent| {
        parent.spawn((
            Mesh3d(mesh),
            MeshMaterial3d(materials.vertex_color_terrain.clone()),
            Transform::IDENTITY,
            TerrainSurface,
        ));
    });
}