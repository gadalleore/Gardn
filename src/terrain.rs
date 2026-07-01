use bevy::prelude::*;
use bevy::asset::RenderAssetUsages;
use bevy::render::mesh::{Indices, PrimitiveTopology};
use std::collections::{HashMap, HashSet};

use crate::australia::{biome_profile, AussieBiome};
use crate::world::{
    GardenRng, CHUNK_DEPTH_VOXELS, CHUNK_SIZE, CHUNK_VOXELS, SURFACE_VOXEL_Y, VOXEL_SIZE,
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
    fn get(&self, x: i32, y: i32, z: i32) -> Option<BlockType> {
        if self.in_bounds(x, y, z) {
            self.cells[self.index(x, y, z)]
        } else {
            None
        }
    }

    pub fn is_empty(&self) -> bool {
        self.solid_count == 0
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

    let mut voxels = ChunkVoxels::new(
        CHUNK_VOXELS,
        CHUNK_VOXELS,
        -CHUNK_DEPTH_VOXELS,
        SURFACE_VOXEL_Y,
    );

    if profile.biome == AussieBiome::Ocean {
        fill_ocean_chunk(&mut voxels, &mut rng);
        return voxels;
    }

    let veins = plan_ore_veins(coord, terrain_seed, &profile, &mut rng);

    // Per-column surface layers, sampled every 8 voxels into a flat array
    // (was a 36k-entry HashMap). Smooth enough, 64× cheaper than per-voxel.
    const STRIDE: i32 = 8;
    let cols = CHUNK_VOXELS as usize;
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

    for lz in 0..CHUNK_VOXELS {
        for lx in 0..CHUNK_VOXELS {
            let layers = columns[lz as usize * cols + lx as usize];
            let column_bottom = SURFACE_VOXEL_Y - layers.dirt_depth - layers.soft_depth;

            for y in -CHUNK_DEPTH_VOXELS..=SURFACE_VOXEL_Y {
                let block = if y == SURFACE_VOXEL_Y {
                    layers.surface
                } else if y > SURFACE_VOXEL_Y - layers.dirt_depth {
                    layers.subsurface
                } else if y > column_bottom {
                    layers.soft_rock
                } else if y == -CHUNK_DEPTH_VOXELS {
                    BlockType::Stone
                } else {
                    continue;
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
            for y in -CHUNK_DEPTH_VOXELS..=SURFACE_VOXEL_Y {
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
) -> Vec<OreVein> {
    let mut veins = Vec::new();
    let total_weight: f32 = profile.ore_weights.iter().map(|(_, w)| w).sum();

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
                    vr.range_i(-CHUNK_DEPTH_VOXELS + 4, -2),
                    vr.range_i(4, CHUNK_VOXELS - 4),
                ),
                radius: IVec3::new(
                    vr.range_i(2, 6),
                    vr.range_i(2, 4),
                    vr.range_i(2, 6),
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

/// One combined mesh per chunk. Iterates the dense grid and emits only faces
/// whose neighbour is air or out-of-bounds — O(1) neighbour checks, no hashing.
pub fn build_colored_terrain_mesh(voxels: &ChunkVoxels) -> Mesh {
    // Upper bound: each solid cell contributes at most ~3 visible faces.
    let face_estimate = voxels.solid_count * 3;
    let mut positions: Vec<[f32; 3]> = Vec::with_capacity(face_estimate * 4);
    let mut normals: Vec<[f32; 3]> = Vec::with_capacity(face_estimate * 4);
    let mut colors: Vec<[f32; 4]> = Vec::with_capacity(face_estimate * 4);
    let mut indices: Vec<u32> = Vec::with_capacity(face_estimate * 6);

    let faces: [(IVec3, [f32; 3], [[f32; 3]; 4]); 6] = [
        (
            IVec3::X,
            [1.0, 0.0, 0.0],
            [
                [1.0, 0.0, 0.0],
                [1.0, 1.0, 0.0],
                [1.0, 1.0, 1.0],
                [1.0, 0.0, 1.0],
            ],
        ),
        (
            IVec3::NEG_X,
            [-1.0, 0.0, 0.0],
            [
                [0.0, 0.0, 1.0],
                [0.0, 1.0, 1.0],
                [0.0, 1.0, 0.0],
                [0.0, 0.0, 0.0],
            ],
        ),
        (
            IVec3::Y,
            [0.0, 1.0, 0.0],
            [
                [0.0, 1.0, 1.0],
                [1.0, 1.0, 1.0],
                [1.0, 1.0, 0.0],
                [0.0, 1.0, 0.0],
            ],
        ),
        (
            IVec3::NEG_Y,
            [0.0, -1.0, 0.0],
            [
                [0.0, 0.0, 0.0],
                [1.0, 0.0, 0.0],
                [1.0, 0.0, 1.0],
                [0.0, 0.0, 1.0],
            ],
        ),
        (
            IVec3::Z,
            [0.0, 0.0, 1.0],
            [
                [1.0, 0.0, 1.0],
                [1.0, 1.0, 1.0],
                [0.0, 1.0, 1.0],
                [0.0, 0.0, 1.0],
            ],
        ),
        (
            IVec3::NEG_Z,
            [0.0, 0.0, -1.0],
            [
                [0.0, 0.0, 0.0],
                [0.0, 1.0, 0.0],
                [1.0, 1.0, 0.0],
                [1.0, 0.0, 0.0],
            ],
        ),
    ];

    for y in voxels.y_min..=voxels.y_max {
        for z in 0..voxels.size_z {
            for x in 0..voxels.size_x {
                let Some(block) = voxels.get(x, y, z) else {
                    continue;
                };
                let color = block_color(block);
                let origin = Vec3::new(
                    x as f32 * VOXEL_SIZE,
                    y as f32 * VOXEL_SIZE,
                    z as f32 * VOXEL_SIZE,
                );

                for (neighbor, normal, corners) in &faces {
                    if voxels
                        .get(x + neighbor.x, y + neighbor.y, z + neighbor.z)
                        .is_some()
                    {
                        continue;
                    }

                    let base = positions.len() as u32;
                    for corner in corners {
                        let pos = origin
                            + Vec3::new(
                                corner[0] * VOXEL_SIZE,
                                corner[1] * VOXEL_SIZE,
                                corner[2] * VOXEL_SIZE,
                            );
                        positions.push(pos.to_array());
                        normals.push(*normal);
                        colors.push(color);
                    }

                    indices.extend_from_slice(&[
                        base,
                        base + 1,
                        base + 2,
                        base,
                        base + 2,
                        base + 3,
                    ]);
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

/// Used by voxel trees (material color comes from StandardMaterial, not vertex colors).
pub fn build_culled_voxel_mesh(blocks: &HashSet<IVec3>, block_size: f32) -> Mesh {
    let face_estimate = blocks.len() * 3;
    let mut positions: Vec<[f32; 3]> = Vec::with_capacity(face_estimate * 4);
    let mut normals: Vec<[f32; 3]> = Vec::with_capacity(face_estimate * 4);
    let mut uvs: Vec<[f32; 2]> = Vec::with_capacity(face_estimate * 4);
    let mut indices: Vec<u32> = Vec::with_capacity(face_estimate * 6);

    let faces: [(IVec3, [f32; 3], [[f32; 3]; 4]); 6] = [
        (
            IVec3::X,
            [1.0, 0.0, 0.0],
            [
                [1.0, 0.0, 0.0],
                [1.0, 1.0, 0.0],
                [1.0, 1.0, 1.0],
                [1.0, 0.0, 1.0],
            ],
        ),
        (
            IVec3::NEG_X,
            [-1.0, 0.0, 0.0],
            [
                [0.0, 0.0, 1.0],
                [0.0, 1.0, 1.0],
                [0.0, 1.0, 0.0],
                [0.0, 0.0, 0.0],
            ],
        ),
        (
            IVec3::Y,
            [0.0, 1.0, 0.0],
            [
                [0.0, 1.0, 1.0],
                [1.0, 1.0, 1.0],
                [1.0, 1.0, 0.0],
                [0.0, 1.0, 0.0],
            ],
        ),
        (
            IVec3::NEG_Y,
            [0.0, -1.0, 0.0],
            [
                [0.0, 0.0, 0.0],
                [1.0, 0.0, 0.0],
                [1.0, 0.0, 1.0],
                [0.0, 0.0, 1.0],
            ],
        ),
        (
            IVec3::Z,
            [0.0, 0.0, 1.0],
            [
                [1.0, 0.0, 1.0],
                [1.0, 1.0, 1.0],
                [0.0, 1.0, 1.0],
                [0.0, 0.0, 1.0],
            ],
        ),
        (
            IVec3::NEG_Z,
            [0.0, 0.0, -1.0],
            [
                [0.0, 0.0, 0.0],
                [0.0, 1.0, 0.0],
                [1.0, 1.0, 0.0],
                [1.0, 0.0, 0.0],
            ],
        ),
    ];

    for block in blocks {
        let origin = Vec3::new(
            block.x as f32 * block_size,
            block.y as f32 * block_size,
            block.z as f32 * block_size,
        );

        for (neighbor, normal, corners) in &faces {
            if blocks.contains(&(*block + *neighbor)) {
                continue;
            }

            let base = positions.len() as u32;
            for corner in corners {
                let pos = origin + Vec3::new(
                    corner[0] * block_size,
                    corner[1] * block_size,
                    corner[2] * block_size,
                );
                positions.push(pos.to_array());
                normals.push(*normal);
                uvs.push([corner[0], corner[1]]);
            }

            indices.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
        }
    }

    let mut mesh = Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::default());
    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, positions);
    mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, normals);
    mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, uvs);
    mesh.insert_indices(Indices::U32(indices));
    mesh
}

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
        ));
    });
}