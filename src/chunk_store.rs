use bevy::prelude::*;
use std::collections::{HashMap, HashSet};

use crate::australia::{biome_profile, AussieBiome};
use crate::topography::surface_height_voxels;
use crate::trees::{species_colors, TreeSpecies};
use crate::world::{
    chunk_seed, chunk_world_origin, GardenRng, CHUNK_SIZE, VOXEL_SIZE, WORLD_SEED,
};

/// Compact on-disk-style snapshot of a chunk column — not every voxel, just what
/// must stay stable between visits. Terrain blocks are re-derived from `terrain_seed`
/// (deterministic ore veins + biome layers); trees are stored explicitly because
/// they spawn asynchronously and must match exactly on return.
#[derive(Clone, Debug)]
pub struct ChunkRecord {
    pub coord: IVec2,
    pub terrain_seed: u64,
    pub trees: Vec<SavedTree>,
    /// Voxels the worm has eaten out of this chunk (chunk-local coords) —
    /// burrows must survive streaming out and back in.
    pub edits: HashSet<IVec3>,
}

#[derive(Clone, Debug)]
pub struct SavedTree {
    pub local_base: Vec3,
    pub species: TreeSpecies,
    pub tree_seed: u64,
    pub bark_color: (f32, f32, f32),
    pub foliage_color: (f32, f32, f32),
}

/// Session-persistent archive: collapsed chunks keyed by column coord.
#[derive(Resource, Default)]
pub struct ChunkArchive {
    pub saved: HashMap<IVec2, ChunkRecord>,
}

impl ChunkRecord {
    /// First visit — procedurally plan trees for this Australian region.
    pub fn generate(coord: IVec2) -> Self {
        Self {
            coord,
            terrain_seed: chunk_seed(WORLD_SEED, coord),
            trees: plan_trees(coord),
            edits: HashSet::new(),
        }
    }

    pub fn tree_jobs(&self, chunk_entity: Entity) -> Vec<ChunkTreeJob> {
        self.trees
            .iter()
            .map(|t| ChunkTreeJob {
                chunk_entity,
                local_base: t.local_base,
                species: t.species,
                tree_seed: t.tree_seed,
                bark_color: t.bark_color,
                foliage_color: t.foliage_color,
            })
            .collect()
    }
}

/// Tree spawn work derived from a saved or freshly generated record.
pub struct ChunkTreeJob {
    pub chunk_entity: Entity,
    pub local_base: Vec3,
    pub species: TreeSpecies,
    pub tree_seed: u64,
    pub bark_color: (f32, f32, f32),
    pub foliage_color: (f32, f32, f32),
}

fn plan_trees(coord: IVec2) -> Vec<SavedTree> {
    let mut rng = GardenRng::new(chunk_seed(WORLD_SEED, coord));
    let chunk_center =
        chunk_world_origin(coord) + Vec3::new(CHUNK_SIZE * 0.5, 0.0, CHUNK_SIZE * 0.5);
    let profile = biome_profile(chunk_center.x, chunk_center.z);

    if profile.biome == AussieBiome::Ocean || profile.tree_density <= 0.0 {
        return Vec::new();
    }

    let min_spacing = 18.0;
    let spawn_clear_radius = 14.0;
    // Density is authored as trees per 64×64 ft of ground; scale to whatever
    // the chunk footprint is and roll the fractional remainder so a chunk with
    // an expected 0.4 trees gets one 40% of the time.
    let area_scale = (CHUNK_SIZE * CHUNK_SIZE) / 4096.0;
    let expected = rng.range(2.0, 4.0) * profile.tree_density * area_scale;
    let target_trees =
        expected.floor() as i32 + if rng.chance(expected.fract()) { 1 } else { 0 };
    let margin = 4.0;

    let mut placed: Vec<Vec3> = Vec::new();
    let mut trees = Vec::new();

    for _ in 0..300 {
        if trees.len() >= target_trees as usize {
            break;
        }

        let local_x = rng.range(margin, CHUNK_SIZE - margin);
        let local_z = rng.range(margin, CHUNK_SIZE - margin);
        let world_base = chunk_world_origin(coord) + Vec3::new(local_x, 0.0, local_z);
        let spot_profile = biome_profile(world_base.x, world_base.z);

        if spot_profile.biome == AussieBiome::Ocean || spot_profile.tree_mix.is_empty() {
            continue;
        }
        if world_base.length() < spawn_clear_radius {
            continue;
        }

        // Root the trunk ~16 inches into the local surface so trees hug the
        // terrain even on steep worm-mountains, where the chunk mesh's
        // interpolated height can differ by a few voxels from this point sample.
        let surface_y = surface_height_voxels(world_base.x, world_base.z);
        let local_base = Vec3::new(local_x, (surface_y - 8) as f32 * VOXEL_SIZE, local_z);

        if placed
            .iter()
            .any(|p: &Vec3| Vec2::new(p.x, p.z).distance(Vec2::new(local_x, local_z)) < min_spacing)
        {
            continue;
        }

        let species = pick_species(&mut rng, spot_profile.tree_mix);
        let (bark_color, foliage_color) = species_colors(species);

        trees.push(SavedTree {
            local_base,
            species,
            tree_seed: rng.next_u32() as u64 | ((rng.next_u32() as u64) << 32),
            bark_color,
            foliage_color,
        });

        placed.push(local_base);
    }

    trees
}

/// Weighted draw from a biome's native species mix.
fn pick_species(rng: &mut GardenRng, mix: &[(TreeSpecies, f32)]) -> TreeSpecies {
    let total: f32 = mix.iter().map(|(_, w)| w).sum();
    let mut roll = rng.next_f32() * total;
    for &(species, weight) in mix {
        if roll < weight {
            return species;
        }
        roll -= weight;
    }
    mix.last().map(|(s, _)| *s).unwrap_or(TreeSpecies::RiverRedGum)
}

/// Collapse a live chunk back into archive storage when the player leaves.
pub fn archive_chunk(archive: &mut ChunkArchive, record: ChunkRecord) {
    archive.saved.insert(record.coord, record);
}

/// Restore a previously collapsed chunk, or None on first visit.
pub fn take_saved_chunk(archive: &mut ChunkArchive, coord: IVec2) -> Option<ChunkRecord> {
    archive.saved.remove(&coord)
}

