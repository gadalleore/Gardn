use bevy::asset::RenderAssetUsages;
use bevy::pbr::{NotShadowCaster, NotShadowReceiver};
use bevy::prelude::*;
use bevy::render::mesh::{Indices, PrimitiveTopology};
use bevy::render::view::RenderLayers;
use std::collections::{HashMap, VecDeque};

use crate::australia::{biome_at_world, AussieBiome};
use crate::chunk_store::{ChunkArchive, ChunkRecord};
use crate::terrain::TerrainMaterials;
use crate::topography::surface_height_voxels;
use crate::trees::{silhouette_spec, species_colors, TreeSpecies, ALL_SPECIES};
use crate::world::{
    chunk_chebyshev_distance, chunk_world_origin, world_to_chunk, CHUNK_SIZE,
    CHUNK_UNLOAD_DISTANCE, SILHOUETTES_PER_FRAME, SILHOUETTE_CHUNK_DISTANCE, VOXEL_SIZE,
};

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
    /// terrain spawns opaque in its place (the cutout trees linger until the
    /// real trees stand).
    ground: Option<Entity>,
    /// Ground block-size factor this chunk was meshed at (fine voxels per
    /// coarse block edge); when the distance band changes it gets re-meshed.
    factor: i32,
}

/// Far-ground block size by chunk distance (in 1-inch voxels): 16-inch blocks
/// in the nearest far ring, 32-inch beyond that, 64-inch out at the horizon.
fn far_ground_factor(dist: i32) -> i32 {
    if dist <= 8 {
        16
    } else if dist <= 14 {
        32
    } else {
        64
    }
}

#[derive(Resource)]
pub struct SilhouetteAssets {
    trunk_mesh: Handle<Mesh>,
    crown_mesh: Handle<Mesh>,
    cone_mesh: Handle<Mesh>,
    /// Per species: (trunk material, crown material) — dark, lit, fog-aware.
    materials: HashMap<TreeSpecies, (Handle<StandardMaterial>, Handle<StandardMaterial>)>,
    /// Material for the shadow-only planes — never drawn by the camera.
    shadow_material: Handle<StandardMaterial>,
}

/// A silhouette tree that spins to face the camera (yaw only).
#[derive(Component)]
pub struct SilhouetteBillboard {
    world_x: f32,
    world_z: f32,
}

/// Root of a cutout tree's invisible shadow planes. Lives on render layer 1
/// (only the sun/moon look there), and faces the light instead of the camera,
/// so the shadow keeps its full tree shape no matter where the player stands.
#[derive(Component)]
pub struct SilhouetteShadow;

/// Turn all cutout shadow planes to face the current shadow-casting body.
pub fn orient_silhouette_shadows(
    sun: Res<crate::SunDirection>,
    mut shadows: Query<&mut Transform, With<SilhouetteShadow>>,
) {
    let rotation = Quat::from_rotation_y(sun.0.x.atan2(sun.0.z));
    for mut tf in &mut shadows {
        tf.rotation = rotation;
    }
}

/// Unit canopy cutout: a lumpy cloud, wider than tall, with a flattened
/// underside — the profile of a real crown (apex blobs and branch pom-poms),
/// not a lollipop ball. Spans x,y ∈ [-0.5, 0.5]; fan-triangulated around the
/// centroid so the lumpy outline stays valid.
fn canopy_mesh() -> Mesh {
    let n = 22usize;
    let mut ring: Vec<[f32; 2]> = Vec::with_capacity(n);
    let mut y_min = f32::MAX;
    let mut y_max = f32::MIN;
    for i in 0..n {
        let a = i as f32 / n as f32 * std::f32::consts::TAU;
        // Three broad lobes with a phase twist read as clumped foliage.
        let lump = 0.72 + 0.28 * ((a * 3.0 + 0.7).sin() * 0.5 + 0.5);
        let x = a.cos() * 0.5 * lump;
        let mut y = a.sin() * 0.5 * lump;
        // Tall billowing top, squashed underside.
        y *= if y > 0.0 { 1.0 } else { 0.45 };
        y_min = y_min.min(y);
        y_max = y_max.max(y);
        ring.push([x, y]);
    }
    // Normalise the squashed profile back to a unit-height span.
    let scale = 1.0 / (y_max - y_min);
    let shift = (y_max + y_min) * 0.5;
    for p in &mut ring {
        p[1] = (p[1] - shift) * scale;
    }

    let mut positions: Vec<[f32; 3]> = vec![[0.0, 0.0, 0.0]];
    positions.extend(ring.iter().map(|&[x, y]| [x, y, 0.0]));
    let normals: Vec<[f32; 3]> = vec![[0.0, 0.0, 1.0]; positions.len()];
    let mut indices: Vec<u32> = Vec::with_capacity(n * 3);
    for i in 0..n as u32 {
        indices.extend_from_slice(&[0, 1 + i, 1 + (i + 1) % n as u32]);
    }

    let mut mesh = Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::default());
    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, positions);
    mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, normals);
    mesh.insert_indices(Indices::U32(indices));
    mesh
}

/// One flat polygon in the XY plane, fan-triangulated. Cutouts get billboarded
/// toward the camera, so a single plane is all a tree needs.
fn plane_mesh(points: &[[f32; 2]]) -> Mesh {
    let positions: Vec<[f32; 3]> = points.iter().map(|&[u, v]| [u, v, 0.0]).collect();
    let normals: Vec<[f32; 3]> = vec![[0.0, 0.0, 1.0]; points.len()];
    let mut indices: Vec<u32> = Vec::new();
    for i in 1..points.len() as u32 - 1 {
        indices.extend_from_slice(&[0, i, i + 1]);
    }

    let mut mesh = Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::default());
    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, positions);
    mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, normals);
    mesh.insert_indices(Indices::U32(indices));
    mesh
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

pub fn setup_silhouette_assets(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    // Unit trunk: 1×1 rectangle rising from y=0.
    let trunk_mesh = meshes.add(plane_mesh(&[
        [-0.5, 0.0],
        [0.5, 0.0],
        [0.5, 1.0],
        [-0.5, 1.0],
    ]));

    // Unit crown: lumpy canopy cloud capping the trunk.
    let crown_mesh = meshes.add(canopy_mesh());

    // Unit conifer: triangle rising from y=0.
    let cone_mesh = meshes.add(plane_mesh(&[[-0.5, 0.0], [0.5, 0.0], [0.0, 1.0]]));

    let mut mats = HashMap::new();
    for species in ALL_SPECIES {
        let (bark, foliage) = species_colors(species);
        // Lit (not unlit) so the camera's distance fog applies — faraway
        // cutouts haze into the sky like real distant trees. Kept dark and
        // fully rough so they still read as silhouettes.
        let dark = |c: (f32, f32, f32), k: f32| StandardMaterial {
            base_color: Color::srgb(c.0 * k, c.1 * k, c.2 * k),
            perceptual_roughness: 1.0,
            reflectance: 0.0,
            cull_mode: None,
            ..default()
        };
        mats.insert(
            species,
            (materials.add(dark(bark, 0.30)), materials.add(dark(foliage, 0.32))),
        );
    }

    commands.insert_resource(SilhouetteAssets {
        trunk_mesh,
        crown_mesh,
        cone_mesh,
        materials: mats,
        shadow_material: materials.add(StandardMaterial {
            base_color: Color::BLACK,
            unlit: true,
            ..default()
        }),
    });
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

/// Matches TREE_FADE_SECS closely enough that the cutout dissolves while the
/// last real trees are still ramping up — the eye always has a tree to rest on.
const CUTOUT_FADE_SECS: f32 = 1.2;

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

/// Drain a few queued chunks per frame; each spawns one cheap parent entity
/// holding the chunk's ground tile plus a billboard cutout for every tree the
/// chunk will eventually grow.
pub fn process_silhouette_queue(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    assets: Res<SilhouetteAssets>,
    terrain_materials: Res<TerrainMaterials>,
    archive: Res<ChunkArchive>,
    chunk_world: Res<crate::ChunkWorld>,
    mut sil: ResMut<SilhouetteWorld>,
) {
    // Band-change re-meshes first: each swaps a new ground mesh onto the live
    // entity in one frame — the old blocks stand until the finer ones replace
    // them, so a rezone can never open a hole in the world.
    let player_chunk = sil.last_player_chunk;
    for _ in 0..SILHOUETTES_PER_FRAME {
        let Some(coord) = sil.rezone_queue.pop_front() else {
            break;
        };
        let Some(player_chunk) = player_chunk else {
            break;
        };
        // Re-derive the band at drain time — the queue may be a few frames
        // stale — and skip chunks that retired or already match.
        let factor = far_ground_factor(chunk_chebyshev_distance(coord, player_chunk));
        let Some(spawned) = sil.spawned.get_mut(&coord) else {
            continue;
        };
        if spawned.factor == factor {
            continue;
        }
        let Some(ground) = spawned.ground else {
            continue;
        };
        commands
            .entity(ground)
            .insert(Mesh3d(meshes.add(build_far_ground_mesh(coord, factor))));
        spawned.factor = factor;
    }

    for _ in 0..SILHOUETTES_PER_FRAME {
        let Some((coord, factor)) = sil.queue.pop_front() else {
            break;
        };
        if sil.spawned.contains_key(&coord) || chunk_world.loaded.contains_key(&coord) {
            continue;
        }

        let origin = chunk_world_origin(coord);
        let center_x = origin.x + CHUNK_SIZE * 0.5;
        let center_z = origin.z + CHUNK_SIZE * 0.5;
        let biome = biome_at_world(center_x, center_z);

        // Archived chunks keep their exact trees; unvisited ones get the same
        // deterministic plan the streamer will generate later.
        let trees = if biome == AussieBiome::Ocean {
            Vec::new()
        } else {
            match archive.saved.get(&coord) {
                Some(record) => record.trees.clone(),
                None => ChunkRecord::generate(coord).trees,
            }
        };

        let parent = commands
            .spawn((
                Transform::from_translation(origin),
                Visibility::default(),
            ))
            .id();

        let mut ground_entity = None;
        commands.entity(parent).with_children(|chunk| {
            // Distant ground: the real topography in coarse blocky steps —
            // dramatic rises and falls that sharpen as their ring approaches.
            // It fades in through a throwaway material, then swaps back to the
            // shared one; it receives shadows so cutout trees darken it.
            let fade_material = materials.add(StandardMaterial {
                base_color: Color::WHITE.with_alpha(0.0),
                alpha_mode: AlphaMode::Blend,
                ..default()
            });
            ground_entity = Some(
                chunk
                    .spawn((
                        Mesh3d(meshes.add(build_far_ground_mesh(coord, factor))),
                        MeshMaterial3d(fade_material.clone()),
                        NotShadowCaster,
                        Transform::IDENTITY,
                        crate::FadeIn {
                            material: fade_material,
                            timer: Timer::from_seconds(
                                crate::GROUND_FADE_SECS,
                                TimerMode::Once,
                            ),
                            final_alpha_mode: AlphaMode::Opaque,
                            swap_to: Some(terrain_materials.vertex_color_terrain.clone()),
                        },
                    ))
                    .id(),
            );

            for tree in &trees {
                let spec = silhouette_spec(tree.species, tree.tree_seed);
                let Some((trunk_mat, crown_mat)) = assets.materials.get(&tree.species) else {
                    continue;
                };
                let h = spec.height_ft;
                let r = spec.crown_radius_ft;
                // Canopy centre: hangs off the trunk top so its billowing rim
                // always overtops the trunk — no bare pole above the crown.
                let crown_y = h - 0.52 * r;
                let crown_scale = Vec3::new(r * 2.2, r * 1.5, r * 2.2);

                chunk
                    .spawn((
                        SilhouetteBillboard {
                            world_x: origin.x + tree.local_base.x,
                            world_z: origin.z + tree.local_base.z,
                        },
                        Transform::from_translation(tree.local_base),
                        Visibility::default(),
                    ))
                    .with_children(|tree_root| {
                        tree_root.spawn((
                            Mesh3d(assets.trunk_mesh.clone()),
                            MeshMaterial3d(trunk_mat.clone()),
                            NotShadowCaster,
                            NotShadowReceiver,
                            Transform::from_scale(Vec3::new(
                                spec.trunk_width_ft,
                                h,
                                spec.trunk_width_ft,
                            )),
                        ));
                        if spec.cone {
                            // Conifer: triangle from ~28% height past the tip.
                            // Nudged toward the camera so the coplanar trunk
                            // can never z-fight through it.
                            tree_root.spawn((
                                Mesh3d(assets.cone_mesh.clone()),
                                MeshMaterial3d(crown_mat.clone()),
                                NotShadowCaster,
                                NotShadowReceiver,
                                Transform {
                                    translation: Vec3::new(0.0, h * 0.28, 1.0),
                                    scale: Vec3::new(r * 2.0, h * 0.8, r * 2.0),
                                    ..default()
                                },
                            ));
                        } else {
                            tree_root.spawn((
                                Mesh3d(assets.crown_mesh.clone()),
                                MeshMaterial3d(crown_mat.clone()),
                                NotShadowCaster,
                                NotShadowReceiver,
                                Transform {
                                    translation: Vec3::new(0.0, crown_y, 1.0),
                                    scale: crown_scale,
                                    ..default()
                                },
                            ));
                        }
                    });

                // Invisible sun-facing twin on layer 1: only the lights see
                // it, so the cutout throws a real tree-shaped shadow that the
                // arriving voxel tree's shadow smoothly takes over from.
                chunk
                    .spawn((
                        SilhouetteShadow,
                        Transform::from_translation(tree.local_base),
                        Visibility::default(),
                    ))
                    .with_children(|shadow_root| {
                        shadow_root.spawn((
                            Mesh3d(assets.trunk_mesh.clone()),
                            MeshMaterial3d(assets.shadow_material.clone()),
                            NotShadowReceiver,
                            RenderLayers::layer(1),
                            Transform::from_scale(Vec3::new(
                                spec.trunk_width_ft,
                                h,
                                spec.trunk_width_ft,
                            )),
                        ));
                        let (mesh, transform) = if spec.cone {
                            (
                                assets.cone_mesh.clone(),
                                Transform {
                                    translation: Vec3::Y * (h * 0.28),
                                    scale: Vec3::new(r * 2.0, h * 0.8, r * 2.0),
                                    ..default()
                                },
                            )
                        } else {
                            (
                                assets.crown_mesh.clone(),
                                Transform {
                                    translation: Vec3::Y * crown_y,
                                    scale: crown_scale,
                                    ..default()
                                },
                            )
                        };
                        shadow_root.spawn((
                            Mesh3d(mesh),
                            MeshMaterial3d(assets.shadow_material.clone()),
                            NotShadowReceiver,
                            RenderLayers::layer(1),
                            transform,
                        ));
                    });
            }
        });

        sil.spawned.insert(
            coord,
            SpawnedSil {
                root: parent,
                ground: ground_entity,
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

/// Start the cross-fade on freshly retiring cutout roots: every visible plane
/// gets a private blend-material clone (the originals are shared per species
/// and must not dim other chunks), shadow twins go instantly (the real trees
/// cast true shadows already), and any in-flight FadeIn is stripped so it
/// can't snap a material back to opaque mid-dissolve.
pub fn begin_silhouette_fadeout(
    mut commands: Commands,
    mut materials: ResMut<Assets<StandardMaterial>>,
    roots: Query<Entity, Added<SilhouetteRetiring>>,
    children_q: Query<&Children>,
    shadows: Query<(), With<SilhouetteShadow>>,
    mesh_mats: Query<&MeshMaterial3d<StandardMaterial>>,
) {
    for root in &roots {
        let mut clones = Vec::new();
        let mut stack: Vec<Entity> = vec![root];
        while let Some(entity) = stack.pop() {
            if shadows.get(entity).is_ok() {
                commands.entity(entity).despawn_recursive();
                continue;
            }
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

/// Ramp retiring cutouts down to nothing, then despawn the whole subtree.
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

/// Turn every cutout tree to face the camera (yaw only) — one quad each, no
/// crossed planes, no edge-on slivers.
pub fn billboard_silhouettes(
    cam_q: Query<&Transform, (With<Camera>, Without<SilhouetteBillboard>)>,
    mut boards: Query<(&SilhouetteBillboard, &mut Transform)>,
) {
    let Ok(cam) = cam_q.get_single() else {
        return;
    };
    let cam_pos = cam.translation;

    for (board, mut transform) in &mut boards {
        let yaw = (cam_pos.x - board.world_x).atan2(cam_pos.z - board.world_z);
        transform.rotation = Quat::from_rotation_y(yaw);
    }
}
