use bevy::prelude::*;
use bevy::asset::RenderAssetUsages;
use bevy::render::mesh::{Indices, PrimitiveTopology};
use std::collections::{HashMap, HashSet};

use crate::australia::{biome_profile, AussieBiome};
use crate::topography::surface_height_voxels;
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

    if profile.biome == AussieBiome::Ocean {
        let mut voxels = ChunkVoxels::new(
            CHUNK_VOXELS,
            CHUNK_VOXELS,
            -CHUNK_DEPTH_VOXELS,
            SEA_LEVEL_VOXEL_Y,
        );
        fill_ocean_chunk(&mut voxels, &mut rng);
        return voxels;
    }

    const STRIDE: i32 = 8;
    // Height lattice is finer than the layer lattice so worm-scale micro-relief
    // (5 ft wavelengths) survives the interpolation.
    const HEIGHT_STRIDE: i32 = 4;
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
    // stays the same size).
    let chunk_floor = min_h - CHUNK_DEPTH_VOXELS;
    for lz in 0..CHUNK_VOXELS {
        for lx in 0..CHUNK_VOXELS {
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

fn fill_ocean_chunk(voxels: &mut ChunkVoxels, rng: &mut GardenRng) {
    for lz in 0..CHUNK_VOXELS {
        for lx in 0..CHUNK_VOXELS {
            for y in -CHUNK_DEPTH_VOXELS..=SEA_LEVEL_VOXEL_Y {
                let block = if y >= -2 {
                    BlockType::Water
                } else if y > -6 {
                    BlockType::Sand
                } else if y == -CHUNK_DEPTH_VOXELS {
                    BlockType::Sandstone
                } else {
                    continue;
                };
                voxels.set(lx, y, lz, block);
            }
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
                    // Ore only replaces existing solid ground, never air/caves.
                    if voxels.cells[idx].is_none() {
                        continue;
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
    indices: &mut Vec<u32>,
    normal: [f32; 3],
    corners: [[i32; 3]; 4],
    base: [i32; 3],
    len: i32,
    merge_axis: usize,
    cell_size: f32,
) {
    let idx0 = positions.len() as u32;
    for corner in corners {
        let mut p = [0f32; 3];
        for k in 0..3 {
            let c = if k == merge_axis { corner[k] * len } else { corner[k] };
            p[k] = (base[k] + c) as f32 * cell_size;
        }
        positions.push(p);
        normals.push(normal);
    }
    indices.extend_from_slice(&[idx0, idx0 + 1, idx0 + 2, idx0, idx0 + 2, idx0 + 3]);
}

/// One combined mesh per chunk. Visible faces (neighbour is air/out-of-bounds)
/// are merged into strips along one axis — at 2-inch voxels this collapses flat
/// ground into long quads and keeps vertex counts sane.
pub fn build_colored_terrain_mesh(voxels: &ChunkVoxels) -> Mesh {
    let mut positions: Vec<[f32; 3]> = Vec::new();
    let mut normals: Vec<[f32; 3]> = Vec::new();
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
    mesh.insert_attribute(Mesh::ATTRIBUTE_COLOR, colors);
    mesh.insert_indices(Indices::U32(indices));
    mesh
}

/// Used by voxel trees (material colour comes from StandardMaterial, so no
/// vertex colours or UVs). Same strip merging as the terrain mesher — trunk
/// walls collapse into long vertical quads.
pub fn build_culled_voxel_mesh(blocks: &HashSet<IVec3>, block_size: f32) -> Mesh {
    let mut positions: Vec<[f32; 3]> = Vec::new();
    let mut normals: Vec<[f32; 3]> = Vec::new();
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
    mesh.insert_indices(Indices::U32(indices));
    mesh
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