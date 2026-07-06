use bevy::asset::RenderAssetUsages;
use bevy::pbr::NotShadowCaster;
use bevy::prelude::*;
use bevy::render::mesh::{Indices, PrimitiveTopology};
use bevy::tasks::futures_lite::future;
use bevy::tasks::{block_on, AsyncComputeTaskPool, Task};
use std::collections::{HashMap, VecDeque};

use crate::australia::{biome_at_world, AussieBiome};
use crate::chunk_store::{ChunkArchive, ChunkRecord, SavedTree};
use crate::terrain::{build_colored_block_mesh, downsample_blocks, TerrainMaterials};
use crate::topography::surface_height_voxels;
use crate::trees::{generate_tree, species_colors};
use crate::world::{
    chunk_chebyshev_distance, chunk_world_origin, world_to_chunk, GardenRng, CHUNK_SIZE,
    CHUNK_UNLOAD_DISTANCE, MAX_CONCURRENT_SILHOUETTE_BUILDS, SILHOUETTES_PER_FRAME,
    SILHOUETTE_CHUNK_DISTANCE, TREE_VOXEL_SIZE, VOXEL_SIZE,
};

/// The coloured coarse blocks of one chunk's trees at one LOD.
type TreeBlocks = HashMap<IVec3, [f32; 4]>;

/// Generate each of a chunk's trees as real voxels ONCE and downsample to all
/// three far LOD block sizes, merged per chunk — returned finest-first. Because
/// each tree is the SAME `generate_tree(seed)` the streamer builds up close, the
/// blocks sit exactly where the real leaves and bark will, so stepping between
/// LODs (and finally handing off to the real trees) only sharpens the shape, it
/// never morphs. The chunk caches these maps, so refining as you approach is a
/// cheap re-mesh rather than a regeneration.
fn build_far_trees_lods(trees: &[SavedTree]) -> [TreeBlocks; 3] {
    let mut lods: [TreeBlocks; 3] = [HashMap::new(), HashMap::new(), HashMap::new()];

    for tree in trees {
        let mut rng = GardenRng::new(tree.tree_seed);
        let vt = generate_tree(tree.species, &mut rng);
        let (b, f) = species_colors(tree.species);
        let bark_col = [b.0, b.1, b.2, 1.0];
        let foliage_col = [f.0, f.1, f.2, 1.0];

        for (i, &factor) in FAR_FACTORS.iter().enumerate() {
            let tree_factor = (factor / 2).max(1);
            let block_size = tree_factor as f32 * TREE_VOXEL_SIZE;
            // Tree-local coords are relative to the tree base; shift onto the
            // coarse grid at the tree's spot in the chunk.
            let off = IVec3::new(
                (tree.local_base.x / block_size).round() as i32,
                (tree.local_base.y / block_size).round() as i32,
                (tree.local_base.z / block_size).round() as i32,
            );
            for cell in downsample_blocks(&vt.bark, tree_factor, 0.08) {
                lods[i].insert(cell + off, bark_col);
            }
            for cell in downsample_blocks(&vt.foliage, tree_factor, 0.12) {
                // Foliage overwrites bark where they share a coarse cell.
                lods[i].insert(cell + off, foliage_col);
            }
        }
    }

    lods
}

/// Mesh one cached LOD's blocks at the block size its ground `factor` implies.
fn lod_mesh(blocks: &TreeBlocks, factor: i32) -> Mesh {
    build_colored_block_mesh(blocks, factor as f32 * VOXEL_SIZE)
}

/// Distant world past the streamed chunks:
/// - every tree is a camera-facing paper-cutout billboard (one quad trunk + one
///   quad crown), standing exactly where the real tree will grow, and
/// - every chunk gets a coarse voxel ground mesh sampled from the real
///   topography — big blocks jutting out of the land, in bigger and bigger
///   sizes the farther the ring, that gain character as you approach and are
///   finally replaced by true 2-inch terrain.
/// Tree planning is deterministic per chunk, and a cutout only retires once the
/// chunk's real trees have actually spawned — no hole between cutout and tree.
#[derive(Resource, Default)]
pub struct SilhouetteWorld {
    spawned: HashMap<IVec2, SpawnedSil>,
    queue: VecDeque<(IVec2, i32)>,
    /// Chunks whose distance band changed: their ground gets re-meshed at the
    /// new block size *in place* (the old mesh stays up until the swap), so a
    /// band change reads as the land sharpening, never as it vanishing.
    rezone_queue: VecDeque<IVec2>,
    last_player_chunk: Option<IVec2>,
}

struct SpawnedSil {
    root: Entity,
    /// The coarse ground mesh child — despawned the same frame the real chunk
    /// terrain spawns opaque in its place (the coarse trees linger until the
    /// real trees stand).
    ground: Option<Entity>,
    /// The merged coarse-voxel-trees mesh child (the whole chunk's distant trees
    /// in one mesh at the current LOD), or None until its build lands.
    trees: Option<Entity>,
    /// Cached coarse-tree blocks at all three LOD levels (finest first), so a
    /// band change is a cheap re-mesh, never a regeneration.
    lods: Option<[TreeBlocks; 3]>,
    /// Ground block-size factor this chunk is meshed at; a band change re-meshes
    /// the ground and swaps the trees to the matching cached LOD.
    factor: i32,
}

/// The three far LOD block sizes, in 3-inch ground voxels: ×3 inch gives 24-inch
/// (2 ft) just outside the streamed ring, 36-inch (3 ft), then 48-inch (4 ft) at
/// the horizon. A distant chunk steps down through these — finer as you
/// approach — then hands off to the real 6-inch trees. (These are ÷3 of the old
/// values so the coarser voxel grid doesn't inflate them into mega-blocks.)
const FAR_FACTORS: [i32; 3] = [8, 12, 16];

/// Far-ground / far-tree block size by chunk distance, spread across the ~16
/// chunk silhouette ring so all three LOD steps are visible as you approach.
fn far_ground_factor(dist: i32) -> i32 {
    if dist <= 7 {
        FAR_FACTORS[0]
    } else if dist <= 11 {
        FAR_FACTORS[1]
    } else {
        FAR_FACTORS[2]
    }
}

/// Which cached LOD level a ground factor maps to (finest = 0).
fn far_lod_index(factor: i32) -> usize {
    FAR_FACTORS.iter().position(|&f| f == factor).unwrap_or(FAR_FACTORS.len() - 1)
}

/// An in-flight background build of one chunk's coarse-tree LOD block maps.
#[derive(Component)]
pub struct PendingSilTrees {
    task: Task<Option<[TreeBlocks; 3]>>,
    /// The silhouette chunk root these trees belong under.
    coord: IVec2,
}

/// Matches the dominant surface block of each biome so the coarse far ground
/// blends with real terrain at the streaming boundary.
fn ground_color(biome: AussieBiome) -> (f32, f32, f32) {
    match biome {
        AussieBiome::Ocean => (0.22, 0.42, 0.68),
        AussieBiome::TropicalSavanna => (0.32, 0.52, 0.22),
        AussieBiome::AridOutback => (0.72, 0.38, 0.22),
        AussieBiome::Pilbara => (0.66, 0.34, 0.22),
        AussieBiome::Mediterranean => (0.38, 0.50, 0.24),
        AussieBiome::TemperateForest => (0.32, 0.52, 0.22),
        AussieBiome::CoastalBush => (0.36, 0.52, 0.24),
        AussieBiome::Tasmania => (0.28, 0.46, 0.24),
    }
}

const WATER_COLOR: [f32; 4] = [0.22, 0.42, 0.68, 1.0];
const SAND_COLOR: [f32; 4] = [0.82, 0.74, 0.48, 1.0];

/// Coarse blocky ground for one far chunk (chunk-local coords): the real
/// topography sampled per coarse cell at its *exact* surface height, meshed as
/// flat cell tops with walls dropping to lower neighbours. X/Z stays coarse
/// (bigger cells the farther the ring) for LOD density, but the tops trace the
/// true hills — so a far chunk lines up with the real 1-inch terrain at the
/// streaming boundary, and with the neighbouring ring across a factor change,
/// instead of stepping off a height-quantisation cliff. A downward perimeter
/// skirt covers any residual sub-cell mismatch at those boundaries.
/// Underwater cells become a flat sea sheet; near-sea land reads as beach.
pub fn build_far_ground_mesh(coord: IVec2, factor: i32) -> Mesh {
    let cell = VOXEL_SIZE * factor as f32;
    let n = (CHUNK_SIZE / cell).round().max(1.0) as i32;
    let origin = chunk_world_origin(coord);
    let water_top = VOXEL_SIZE;

    // Heights + colours on an (n+2)² grid — one border cell all round so edge
    // walls line up with the identically sampled neighbouring chunk.
    let idx = |i: i32, j: i32| ((i + 1) * (n + 2) + (j + 1)) as usize;
    let mut tops = vec![0.0f32; ((n + 2) * (n + 2)) as usize];
    let mut cols = vec![[0.0f32; 4]; tops.len()];
    for i in -1..=n {
        for j in -1..=n {
            let wx = origin.x + (i as f32 + 0.5) * cell;
            let wz = origin.z + (j as f32 + 0.5) * cell;
            let h = surface_height_voxels(wx, wz);
            let biome = biome_at_world(wx, wz);
            // Ocean is keyed off the biome, not `h`: surface_height_voxels
            // never goes negative (0 at sea, clamped ≥1 on land), so the old
            // `h < 0` test never fired and sea cells rendered as raised sand.
            let (top, col) = if biome == AussieBiome::Ocean {
                (water_top, WATER_COLOR)
            } else {
                let col = if h < 6 {
                    SAND_COLOR
                } else {
                    let (r, g, b) = ground_color(biome);
                    [r, g, b, 1.0]
                };
                // Exact top face, matching the real terrain's (h+1)*VOXEL_SIZE.
                ((h + 1) as f32 * VOXEL_SIZE, col)
            };
            tops[idx(i, j)] = top;
            cols[idx(i, j)] = col;
        }
    }

    let mut positions: Vec<[f32; 3]> = Vec::new();
    let mut normals: Vec<[f32; 3]> = Vec::new();
    let mut colors: Vec<[f32; 4]> = Vec::new();
    let mut indices: Vec<u32> = Vec::new();
    let mut quad = |verts: [[f32; 3]; 4], normal: [f32; 3], color: [f32; 4]| {
        let i0 = positions.len() as u32;
        positions.extend_from_slice(&verts);
        normals.extend([normal; 4]);
        colors.extend([color; 4]);
        indices.extend_from_slice(&[i0, i0 + 1, i0 + 2, i0, i0 + 2, i0 + 3]);
    };

    for i in 0..n {
        for j in 0..n {
            let t = tops[idx(i, j)];
            let c = cols[idx(i, j)];
            let (x0, z0) = (i as f32 * cell, j as f32 * cell);
            let (x1, z1) = (x0 + cell, z0 + cell);

            quad(
                [[x0, t, z0], [x0, t, z1], [x1, t, z1], [x1, t, z0]],
                [0.0, 1.0, 0.0],
                c,
            );

            // Walls down to every lower neighbour, in a shaded side colour so
            // the steps read as blocks, not contour lines.
            let side = [c[0] * 0.78, c[1] * 0.78, c[2] * 0.78, 1.0];
            let nx0 = tops[idx(i - 1, j)];
            if nx0 < t {
                quad(
                    [[x0, nx0, z1], [x0, t, z1], [x0, t, z0], [x0, nx0, z0]],
                    [-1.0, 0.0, 0.0],
                    side,
                );
            }
            let nx1 = tops[idx(i + 1, j)];
            if nx1 < t {
                quad(
                    [[x1, nx1, z0], [x1, t, z0], [x1, t, z1], [x1, nx1, z1]],
                    [1.0, 0.0, 0.0],
                    side,
                );
            }
            let nz0 = tops[idx(i, j - 1)];
            if nz0 < t {
                quad(
                    [[x0, nz0, z0], [x0, t, z0], [x1, t, z0], [x1, nz0, z0]],
                    [0.0, 0.0, -1.0],
                    side,
                );
            }
            let nz1 = tops[idx(i, j + 1)];
            if nz1 < t {
                quad(
                    [[x1, nz1, z1], [x1, t, z1], [x0, t, z1], [x0, nz1, z1]],
                    [0.0, 0.0, 1.0],
                    side,
                );
            }
        }
    }

    // Perimeter skirt: a downward apron just outside each chunk edge. A
    // neighbouring chunk in a different LOD ring — or the real 1-inch terrain
    // at the streaming boundary — samples the surface at a different spacing,
    // so their shared edge can disagree by a fraction of a cell. This apron
    // hangs below the rim to cover any such crack without the mesher needing to
    // know the neighbour's geometry. Depth scales with cell size (coarser rings
    // risk bigger mismatches); nudged a hair outward so it never z-fights the
    // in-chunk step walls.
    let skirt = (cell * 2.5).max(2.0);
    let eps = VOXEL_SIZE * 0.5;
    let span = n as f32 * cell;
    let edge_col = |i: i32, j: i32| {
        let c = cols[idx(i, j)];
        [c[0] * 0.7, c[1] * 0.7, c[2] * 0.7, 1.0]
    };
    for k in 0..n {
        let a = k as f32 * cell;
        let b = a + cell;

        let t = tops[idx(0, k)];
        quad(
            [[-eps, t - skirt, b], [-eps, t, b], [-eps, t, a], [-eps, t - skirt, a]],
            [-1.0, 0.0, 0.0],
            edge_col(0, k),
        );

        let t = tops[idx(n - 1, k)];
        quad(
            [
                [span + eps, t - skirt, a],
                [span + eps, t, a],
                [span + eps, t, b],
                [span + eps, t - skirt, b],
            ],
            [1.0, 0.0, 0.0],
            edge_col(n - 1, k),
        );

        let t = tops[idx(k, 0)];
        quad(
            [[a, t - skirt, -eps], [a, t, -eps], [b, t, -eps], [b, t - skirt, -eps]],
            [0.0, 0.0, -1.0],
            edge_col(k, 0),
        );

        let t = tops[idx(k, n - 1)];
        quad(
            [
                [b, t - skirt, span + eps],
                [b, t, span + eps],
                [a, t, span + eps],
                [a, t - skirt, span + eps],
            ],
            [0.0, 0.0, 1.0],
            edge_col(k, n - 1),
        );
    }

    let mut mesh = Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::default());
    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, positions);
    mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, normals);
    mesh.insert_attribute(Mesh::ATTRIBUTE_COLOR, colors);
    mesh.insert_indices(Indices::U32(indices));
    mesh
}

/// Kick off a background build of one chunk's merged coarse-trees mesh at the
/// ground `factor` (trees downsample to their own 2-inch grid, so the tree
/// factor is half). Off-thread because generating each tree's full voxels is
/// the same work the streamer does up close.
fn spawn_tree_mesh_task(commands: &mut Commands, coord: IVec2, trees: Vec<SavedTree>) -> bool {
    if trees.is_empty() {
        return false;
    }
    let pool = AsyncComputeTaskPool::get();
    let task = pool.spawn(async move { Some(build_far_trees_lods(&trees)) });
    commands.spawn(PendingSilTrees { task, coord });
    true
}

/// (Re)mesh a chunk's trees at the LOD its `factor` implies, from the cached
/// blocks — replacing any existing (coarser or finer) trees mesh. The old mesh
/// stands until the new one is built here, so a band change never opens a gap.
fn attach_tree_lod(
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    material: &Handle<StandardMaterial>,
    spawned: &mut SpawnedSil,
    factor: i32,
) {
    let Some(lods) = &spawned.lods else {
        return;
    };
    let blocks = &lods[far_lod_index(factor)];
    if blocks.is_empty() {
        return;
    }
    let mesh = meshes.add(lod_mesh(blocks, factor));
    if let Some(old) = spawned.trees.take() {
        commands.entity(old).despawn_recursive();
    }
    let ent = commands
        .spawn((
            Mesh3d(mesh),
            MeshMaterial3d(material.clone()),
            NotShadowCaster,
            Transform::IDENTITY,
        ))
        .set_parent(spawned.root)
        .id();
    spawned.trees = Some(ent);
}

/// Land finished coarse-trees meshes: attach each under its chunk root, fading
/// in like the ground, and replace any older (coarser) trees mesh. Builds whose
/// chunk retired or rezoned meanwhile are dropped.
pub fn finish_sil_tree_tasks(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    terrain_materials: Res<TerrainMaterials>,
    mut sil: ResMut<SilhouetteWorld>,
    mut pending_q: Query<(Entity, &mut PendingSilTrees)>,
) {
    for (holder, mut pending) in &mut pending_q {
        let Some(result) = block_on(future::poll_once(&mut pending.task)) else {
            continue;
        };
        commands.entity(holder).despawn();

        let coord = pending.coord;
        let Some(spawned) = sil.spawned.get_mut(&coord) else {
            continue;
        };
        let Some(lods) = result else {
            continue;
        };
        // Cache all LODs, then pop the current one straight in on the opaque
        // shared material — no fade, the blocky trees appear discretely the way
        // voxel leaves do.
        spawned.lods = Some(lods);
        let factor = spawned.factor;
        attach_tree_lod(
            &mut commands,
            &mut meshes,
            &terrain_materials.vertex_color_terrain,
            spawned,
            factor,
        );
    }
}

/// Marks a cutout-chunk root whose real trees are all standing: instead of
/// blinking out under the still-fading real trees, its planes get throwaway
/// blend materials and fade away — a true cross-fade between the two worlds.
#[derive(Component)]
pub struct SilhouetteRetiring;

/// A retiring cutout root mid-fade: alpha ramps down on the cloned materials,
/// then the whole subtree despawns.
#[derive(Component)]
pub struct SilhouetteFadeOut {
    timer: Timer,
    materials: Vec<Handle<StandardMaterial>>,
}

/// Matched to TREE_FADE_SECS so the coarse chunk dissolves out over exactly the
/// window the real trees fade in — a clean cross-fade, the blocky version
/// sharpening into the detailed one rather than one blinking out.
const CUTOUT_FADE_SECS: f32 = 1.6;

/// Decide which unmaterialised chunks should show silhouettes. Cheap enough to
/// run every frame: the full ring scan only happens when the player crosses a
/// chunk border.
pub fn plan_tree_silhouettes(
    mut commands: Commands,
    chunk_world: Res<crate::ChunkWorld>,
    mut sil: ResMut<SilhouetteWorld>,
    cam_q: Query<&Transform, With<Camera>>,
    trees_pending: Query<&crate::TreesPending>,
) {
    let Ok(cam) = cam_q.get_single() else {
        return;
    };
    let player_chunk = world_to_chunk(cam.translation.x, cam.translation.z);

    // A chunk is only "fully live" once its real terrain AND all its async
    // trees are standing — cutout trees hold the pose until then, so a distant
    // tree never blinks out ahead of the real one popping in. Even then the
    // cutouts don't blink: they hand off through a fade-out that overlaps the
    // real trees' fade-in.
    let fully_live = |coord: &IVec2| -> bool {
        chunk_world
            .loaded
            .get(coord)
            .is_some_and(|e| trees_pending.get(*e).map(|p| p.0 == 0).unwrap_or(true))
    };

    let to_retire: Vec<IVec2> = sil
        .spawned
        .keys()
        .copied()
        .filter(fully_live)
        .collect();
    for coord in to_retire {
        if let Some(spawned) = sil.spawned.remove(&coord) {
            commands.entity(spawned.root).insert(SilhouetteRetiring);
        }
    }

    // Chunks a ring beyond the horizon: gone for real — the distance fog
    // swallowed them long before this fires.
    let to_remove: Vec<IVec2> = sil
        .spawned
        .keys()
        .copied()
        .filter(|coord| {
            chunk_chebyshev_distance(*coord, player_chunk) > SILHOUETTE_CHUNK_DISTANCE + 1
        })
        .collect();
    for coord in to_remove {
        if let Some(spawned) = sil.spawned.remove(&coord) {
            commands.entity(spawned.root).despawn_recursive();
        }
    }

    // Real terrain is up: drop the coarse ground the same frame the real
    // terrain spawned (opaque) in its place, so the hand-off is gapless — the
    // seam fix lands both at the same height, so the swap reads as the blocks
    // sharpening, not snapping. The cutout trees stay until their real trees
    // stand, then cross-fade out.
    let ground_done: Vec<IVec2> = sil
        .spawned
        .iter()
        .filter(|(coord, s)| s.ground.is_some() && chunk_world.loaded.contains_key(coord))
        .map(|(c, _)| *c)
        .collect();
    for coord in ground_done {
        if let Some(spawned) = sil.spawned.get_mut(&coord) {
            if let Some(ground) = spawned.ground.take() {
                // despawn_recursive (not plain despawn) detaches the child from
                // its parent's Children first — a stale reference there makes
                // the root's later recursive despawn warn (B0003).
                commands.entity(ground).despawn_recursive();
            }
        }
    }

    if sil.last_player_chunk == Some(player_chunk) {
        return;
    }
    sil.last_player_chunk = Some(player_chunk);

    // Chunks whose distance band changed get their ground re-meshed in place
    // (finer approaching, coarser receding) — never despawned, so the world
    // stays solid while it re-details.
    sil.rezone_queue.clear();
    let rezone: Vec<IVec2> = sil
        .spawned
        .iter()
        .filter(|(coord, s)| {
            s.ground.is_some()
                && s.factor != far_ground_factor(chunk_chebyshev_distance(**coord, player_chunk))
        })
        .map(|(c, _)| *c)
        .collect();
    sil.rezone_queue.extend(rezone);

    sil.queue.clear();
    let mut needed = Vec::new();
    for dx in -SILHOUETTE_CHUNK_DISTANCE..=SILHOUETTE_CHUNK_DISTANCE {
        for dz in -SILHOUETTE_CHUNK_DISTANCE..=SILHOUETTE_CHUNK_DISTANCE {
            let coord = player_chunk + IVec2::new(dx, dz);
            let dist = chunk_chebyshev_distance(coord, player_chunk);
            // Leave the streamed neighbourhood to the real terrain and trees.
            if dist <= CHUNK_UNLOAD_DISTANCE
                || chunk_world.loaded.contains_key(&coord)
                || sil.spawned.contains_key(&coord)
            {
                continue;
            }
            needed.push((dist, coord));
        }
    }
    needed.sort_by_key(|(dist, _)| *dist);
    sil.queue
        .extend(needed.into_iter().map(|(dist, c)| (c, far_ground_factor(dist))));
}

/// Drain a few queued chunks per frame; each spawns one parent entity holding
/// the chunk's coarse ground tile and kicks off a background build of its
/// merged coarse-voxel-trees mesh.
pub fn process_silhouette_queue(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    terrain_materials: Res<TerrainMaterials>,
    archive: Res<ChunkArchive>,
    chunk_world: Res<crate::ChunkWorld>,
    mut sil: ResMut<SilhouetteWorld>,
    pending_trees: Query<(), With<PendingSilTrees>>,
) {
    // The trees a far chunk will grow: archived chunks keep their exact ones;
    // unvisited chunks get the same deterministic plan the streamer will build.
    let trees_for = |coord: IVec2| -> Vec<SavedTree> {
        let origin = chunk_world_origin(coord);
        if biome_at_world(origin.x + CHUNK_SIZE * 0.5, origin.z + CHUNK_SIZE * 0.5)
            == AussieBiome::Ocean
        {
            return Vec::new();
        }
        match archive.saved.get(&coord) {
            Some(record) => record.trees.clone(),
            None => ChunkRecord::generate(coord).trees,
        }
    };

    // Band-change re-bakes: the ground mesh swaps in place; the trees re-bake
    // off-thread at the finer/coarser resolution (the old mesh stays up until
    // the new one lands, so a rezone never opens a hole).
    let player_chunk = sil.last_player_chunk;
    for _ in 0..SILHOUETTES_PER_FRAME {
        let Some(coord) = sil.rezone_queue.pop_front() else {
            break;
        };
        let Some(player_chunk) = player_chunk else {
            break;
        };
        let factor = far_ground_factor(chunk_chebyshev_distance(coord, player_chunk));
        let ground = match sil.spawned.get(&coord) {
            Some(s) if s.factor != factor => s.ground,
            _ => continue,
        };
        let Some(ground) = ground else {
            continue;
        };
        commands
            .entity(ground)
            .insert(Mesh3d(meshes.add(build_far_ground_mesh(coord, factor))));
        // Ground re-details AND the trees step to the matching cached LOD — a
        // cheap re-mesh from the blocks we already have, no regeneration.
        if let Some(s) = sil.spawned.get_mut(&coord) {
            s.factor = factor;
            attach_tree_lod(
                &mut commands,
                &mut meshes,
                &terrain_materials.vertex_color_terrain,
                s,
                factor,
            );
        }
    }

    // Throttle new tree builds so generation never saturates the cores the
    // renderer needs.
    let mut in_flight = pending_trees.iter().count();

    for _ in 0..SILHOUETTES_PER_FRAME {
        if in_flight >= MAX_CONCURRENT_SILHOUETTE_BUILDS {
            break;
        }
        let Some((coord, factor)) = sil.queue.pop_front() else {
            break;
        };
        if sil.spawned.contains_key(&coord) || chunk_world.loaded.contains_key(&coord) {
            continue;
        }

        let origin = chunk_world_origin(coord);
        let parent = commands
            .spawn((Transform::from_translation(origin), Visibility::default()))
            .id();

        // Distant ground: real topography in coarse blocky steps, fading in
        // through a throwaway material then swapping to the shared one.
        let fade_material = materials.add(StandardMaterial {
            base_color: Color::WHITE.with_alpha(0.0),
            alpha_mode: AlphaMode::Blend,
            ..default()
        });
        let ground_entity = commands
            .spawn((
                Mesh3d(meshes.add(build_far_ground_mesh(coord, factor))),
                MeshMaterial3d(fade_material.clone()),
                NotShadowCaster,
                Transform::IDENTITY,
                crate::FadeIn {
                    material: fade_material,
                    timer: Timer::from_seconds(crate::GROUND_FADE_SECS, TimerMode::Once),
                    final_alpha_mode: AlphaMode::Opaque,
                    swap_to: Some(terrain_materials.vertex_color_terrain.clone()),
                },
            ))
            .set_parent(parent)
            .id();

        if spawn_tree_mesh_task(&mut commands, coord, trees_for(coord)) {
            in_flight += 1;
        }

        sil.spawned.insert(
            coord,
            SpawnedSil {
                root: parent,
                ground: Some(ground_entity),
                trees: None,
                lods: None,
                factor,
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every far ring must produce a ground mesh that exists and stays cheap —
    /// the coarser the ring, the smaller the mesh.
    #[test]
    fn far_ground_meshes_shrink_with_distance() {
        let coord = IVec2::new(3, -2);
        let mut prev = usize::MAX;
        for factor in [16, 32, 64] {
            let mesh = build_far_ground_mesh(coord, factor);
            let verts = mesh.count_vertices();
            assert!(verts > 0, "factor {factor} made an empty ground mesh");
            assert!(
                verts < prev,
                "factor {factor} mesh ({verts} verts) not smaller than finer ring ({prev})"
            );
            assert!(verts < 20_000, "factor {factor} ground too heavy: {verts}");
            prev = verts;
        }
    }

    /// The far ground must present the same top face as the real 1-inch
    /// terrain — no quantisation cliff at the streaming boundary, and the same
    /// height at every LOD factor so neighbouring rings line up too. Reads the
    /// generated mesh back and checks every land top-face quad sits exactly on
    /// `surface_top_world_y` at its cell centre.
    #[test]
    fn far_ground_tops_sit_on_real_terrain() {
        use crate::topography::surface_top_world_y;
        use bevy::render::mesh::VertexAttributeValues::Float32x3;

        let coord = IVec2::new(5, 3);
        let origin = chunk_world_origin(coord);
        for factor in [16, 32, 64] {
            let mesh = build_far_ground_mesh(coord, factor);
            let pos = match mesh.attribute(Mesh::ATTRIBUTE_POSITION).unwrap() {
                Float32x3(v) => v,
                _ => panic!("positions not Float32x3"),
            };
            let nrm = match mesh.attribute(Mesh::ATTRIBUTE_NORMAL).unwrap() {
                Float32x3(v) => v,
                _ => panic!("normals not Float32x3"),
            };
            let mut checked = 0;
            for q in 0..pos.len() / 4 {
                let vs = &pos[q * 4..q * 4 + 4];
                let ns = &nrm[q * 4..q * 4 + 4];
                // Top-face quads only (skip the side/skirt walls).
                if !ns.iter().all(|n| *n == [0.0, 1.0, 0.0]) {
                    continue;
                }
                // Opposite corners → the cell centre the height was sampled at.
                let cx = origin.x + (vs[0][0] + vs[2][0]) * 0.5;
                let cz = origin.z + (vs[0][2] + vs[2][2]) * 0.5;
                if biome_at_world(cx, cz) == AussieBiome::Ocean {
                    continue;
                }
                let expected = surface_top_world_y(cx, cz);
                assert!(
                    (vs[0][1] - expected).abs() < 1e-3,
                    "factor {factor}: far top {} != real terrain {} at ({cx}, {cz})",
                    vs[0][1],
                    expected
                );
                checked += 1;
            }
            assert!(checked > 0, "factor {factor}: no land top faces to verify");
        }
    }
}

/// Start the cross-fade on a freshly retiring silhouette chunk: every mesh
/// under it (the coarse trees) gets a private blend-material clone — the shared
/// terrain material must not dim other chunks — and any in-flight FadeIn is
/// stripped so it can't snap the material back to opaque mid-dissolve. The real
/// voxel trees fade in over the same window, so nothing blinks.
pub fn begin_silhouette_fadeout(
    mut commands: Commands,
    mut materials: ResMut<Assets<StandardMaterial>>,
    roots: Query<Entity, Added<SilhouetteRetiring>>,
    children_q: Query<&Children>,
    mesh_mats: Query<&MeshMaterial3d<StandardMaterial>>,
) {
    for root in &roots {
        let mut clones = Vec::new();
        let mut stack: Vec<Entity> = vec![root];
        while let Some(entity) = stack.pop() {
            if let Ok(children) = children_q.get(entity) {
                stack.extend(children.iter().copied());
            }
            let Ok(mat_handle) = mesh_mats.get(entity) else {
                continue;
            };
            if let Some(src) = materials.get(&mat_handle.0) {
                let mut clone = src.clone();
                clone.alpha_mode = AlphaMode::Blend;
                let handle = materials.add(clone);
                commands
                    .entity(entity)
                    .insert(MeshMaterial3d(handle.clone()))
                    .remove::<crate::FadeIn>();
                clones.push(handle);
            }
        }
        commands
            .entity(root)
            .insert(SilhouetteFadeOut {
                timer: Timer::from_seconds(CUTOUT_FADE_SECS, TimerMode::Once),
                materials: clones,
            })
            .remove::<SilhouetteRetiring>();
    }
}

/// Ramp a retiring silhouette chunk down to nothing, then despawn the subtree.
pub fn tick_silhouette_fadeout(
    time: Res<Time>,
    mut commands: Commands,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut fades: Query<(Entity, &mut SilhouetteFadeOut)>,
) {
    for (root, mut fade) in &mut fades {
        fade.timer.tick(time.delta());
        let t = fade.timer.fraction();
        let alpha = 1.0 - t * t * (3.0 - 2.0 * t);
        for handle in &fade.materials {
            if let Some(mat) = materials.get_mut(handle) {
                mat.base_color = mat.base_color.with_alpha(alpha);
            }
        }
        if fade.timer.finished() {
            commands.entity(root).despawn_recursive();
        }
    }
}
