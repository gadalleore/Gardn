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

pub fn generate_chunk_blocks(
    coord: IVec2,
    chunk_origin: Vec3,
    terrain_seed: u64,
) -> HashMap<BlockType, HashSet<IVec3>> {
    let mut rng = GardenRng::new(terrain_seed);
    let center_world = chunk_origin + Vec3::new(CHUNK_SIZE * 0.5, 0.0, CHUNK_SIZE * 0.5);
    let profile = biome_profile(center_world.x, center_world.z);

    if profile.biome == AussieBiome::Ocean {
        return generate_ocean_chunk(&mut rng);
    }

    let veins = plan_ore_veins(coord, terrain_seed, &profile, &mut rng);
    let mut columns: HashMap<IVec2, ColumnLayers> = HashMap::new();

    // Sample biome every 8 voxels — smooth enough, 64× cheaper than per-voxel.
    const STRIDE: i32 = 8;
    for lz in (0..CHUNK_VOXELS).step_by(STRIDE as usize) {
        for lx in (0..CHUNK_VOXELS).step_by(STRIDE as usize) {
            let wx = chunk_origin.x + (lx as f32 + 0.5) * VOXEL_SIZE;
            let wz = chunk_origin.z + (lz as f32 + 0.5) * VOXEL_SIZE;
            let col_profile = biome_profile(wx, wz);
            let layers = column_layers(&mut rng, &col_profile);
            for dz in 0..STRIDE {
                for dx in 0..STRIDE {
                    let px = (lx + dx).min(CHUNK_VOXELS - 1);
                    let pz = (lz + dz).min(CHUNK_VOXELS - 1);
                    columns.insert(IVec2::new(px, pz), layers);
                }
            }
        }
    }

    let mut grouped: HashMap<BlockType, HashSet<IVec3>> = HashMap::new();

    for lz in 0..CHUNK_VOXELS {
        for lx in 0..CHUNK_VOXELS {
            let layers = columns
                .get(&IVec2::new(lx, lz))
                .copied()
                .unwrap_or_else(|| column_layers(&mut rng, &profile));
            let column_bottom = SURFACE_VOXEL_Y - layers.dirt_depth - layers.soft_depth;

            for y in -CHUNK_DEPTH_VOXELS..=SURFACE_VOXEL_Y {
                let pos = IVec3::new(lx, y, lz);
                let mut block = if y == SURFACE_VOXEL_Y {
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

                if let Some(ore) = ore_at(pos, &veins) {
                    block = ore.to_block();
                }

                grouped.entry(block).or_default().insert(pos);
            }
        }
    }

    grouped
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

fn generate_ocean_chunk(rng: &mut GardenRng) -> HashMap<BlockType, HashSet<IVec3>> {
    let mut grouped: HashMap<BlockType, HashSet<IVec3>> = HashMap::new();
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
                grouped.entry(block).or_default().insert(IVec3::new(lx, y, lz));
            }
            if rng.chance(0.02) {
                grouped
                    .entry(BlockType::Limestone)
                    .or_default()
                    .insert(IVec3::new(lx, -8, lz));
            }
        }
    }
    grouped
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

fn ore_at(pos: IVec3, veins: &[OreVein]) -> Option<OreType> {
    let mut best: Option<(OreType, f32)> = None;
    for (i, vein) in veins.iter().enumerate() {
        let dx = (pos.x - vein.center.x) as f32 / vein.radius.x.max(1) as f32;
        let dy = (pos.y - vein.center.y) as f32 / vein.radius.y.max(1) as f32;
        let dz = (pos.z - vein.center.z) as f32 / vein.radius.z.max(1) as f32;
        let dist_sq = dx * dx + dy * dy + dz * dz;
        if dist_sq > 1.0 {
            continue;
        }
        let score = (1.0 - dist_sq) * vein.strength;
        if score > best.map(|(_, s)| s).unwrap_or(0.0) && block_hash(pos, i as u32) < score {
            best = Some((vein.ore, score));
        }
    }
    best.map(|(o, _)| o)
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

/// One combined mesh per chunk — culls across all block types (fixes duplicate faces + VRAM blow-up).
pub fn build_colored_terrain_mesh(grouped: &HashMap<BlockType, HashSet<IVec3>>) -> Mesh {
    let mut occupied = HashSet::new();
    let mut colors_at = HashMap::new();
    for (block_type, blocks) in grouped {
        let color = block_color(*block_type);
        for pos in blocks {
            occupied.insert(*pos);
            colors_at.insert(*pos, color);
        }
    }

    let face_estimate = occupied.len() * 2;
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

    for block in &occupied {
        let origin = Vec3::new(
            block.x as f32 * VOXEL_SIZE,
            block.y as f32 * VOXEL_SIZE,
            block.z as f32 * VOXEL_SIZE,
        );
        let color = colors_at[block];

        for (neighbor, normal, corners) in &faces {
            if occupied.contains(&(*block + *neighbor)) {
                continue;
            }

            let base = positions.len() as u32;
            for corner in corners {
                let pos = origin + Vec3::new(
                    corner[0] * VOXEL_SIZE,
                    corner[1] * VOXEL_SIZE,
                    corner[2] * VOXEL_SIZE,
                );
                positions.push(pos.to_array());
                normals.push(*normal);
                colors.push(color);
            }

            indices.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
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
    grouped: HashMap<BlockType, HashSet<IVec3>>,
) {
    if grouped.values().all(|set| set.is_empty()) {
        return;
    }

    let mesh = meshes.add(build_colored_terrain_mesh(&grouped));
    commands.entity(chunk_entity).with_children(|parent| {
        parent.spawn((
            Mesh3d(mesh),
            MeshMaterial3d(materials.vertex_color_terrain.clone()),
            Transform::IDENTITY,
        ));
    });
}