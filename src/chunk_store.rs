use bevy::prelude::*;
use std::collections::HashMap;

use crate::australia::{biome_profile, AussieBiome};
use crate::world::{
    chunk_seed, chunk_world_origin, GardenRng, CHUNK_SIZE, WORLD_SEED,
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
}

#[derive(Clone, Copy, Debug)]
pub enum SavedTreeSpecies {
    Eucalyptus,
    Acacia,
}

#[derive(Clone, Debug)]
pub struct SavedTree {
    pub local_base: Vec3,
    pub species: SavedTreeSpecies,
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
    pub species: SavedTreeSpecies,
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
    let target_trees = ((rng.range_i(2, 4) as f32) * profile.tree_density)
        .round()
        .clamp(0.0, 6.0) as i32;
    let margin = 4.0;

    let mut placed: Vec<Vec3> = Vec::new();
    let mut trees = Vec::new();

    for _ in 0..300 {
        if trees.len() >= target_trees as usize {
            break;
        }

        let local_x = rng.range(margin, CHUNK_SIZE - margin);
        let local_z = rng.range(margin, CHUNK_SIZE - margin);
        let local_base = Vec3::new(local_x, 0.0, local_z);
        let world_base = chunk_world_origin(coord) + local_base;
        let spot_profile = biome_profile(world_base.x, world_base.z);

        if spot_profile.biome == AussieBiome::Ocean {
            continue;
        }
        if world_base.length() < spawn_clear_radius {
            continue;
        }
        if placed.iter().any(|p| p.distance(local_base) < min_spacing) {
            continue;
        }

        let species_roll =
            rng.next_f32() * (spot_profile.eucalyptus_weight + spot_profile.acacia_weight);
        let species = if species_roll < spot_profile.eucalyptus_weight {
            SavedTreeSpecies::Eucalyptus
        } else {
            SavedTreeSpecies::Acacia
        };
        let (bark_color, foliage_color) = match species {
            SavedTreeSpecies::Eucalyptus => ((0.67, 0.54, 0.40), (0.45, 0.63, 0.52)),
            SavedTreeSpecies::Acacia => ((0.40, 0.33, 0.26), (0.62, 0.70, 0.32)),
        };

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

/// Collapse a live chunk back into archive storage when the player leaves.
pub fn archive_chunk(archive: &mut ChunkArchive, record: ChunkRecord) {
    archive.saved.insert(record.coord, record);
}

/// Restore a previously collapsed chunk, or None on first visit.
pub fn take_saved_chunk(archive: &mut ChunkArchive, coord: IVec2) -> Option<ChunkRecord> {
    archive.saved.remove(&coord)
}

