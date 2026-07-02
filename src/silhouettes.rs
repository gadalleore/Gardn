use bevy::asset::RenderAssetUsages;
use bevy::pbr::{NotShadowCaster, NotShadowReceiver};
use bevy::prelude::*;
use bevy::render::mesh::{Indices, PrimitiveTopology};
use std::collections::{HashMap, VecDeque};

use crate::australia::{biome_at_world, AussieBiome};
use crate::chunk_store::{ChunkArchive, ChunkRecord};
use crate::topography::surface_top_world_y;
use crate::trees::{silhouette_spec, species_colors, TreeSpecies, ALL_SPECIES};
use crate::world::{
    chunk_chebyshev_distance, chunk_world_origin, world_to_chunk, CHUNK_SIZE,
    CHUNK_UNLOAD_DISTANCE, SILHOUETTES_PER_FRAME, SILHOUETTE_CHUNK_DISTANCE, VOXEL_SIZE,
};

/// Distant world past the streamed chunks, drawn as paper cutouts:
/// - every tree is a camera-facing billboard (one quad trunk + one quad crown),
///   so there are no crossed planes or edge-on slivers, and
/// - every chunk lays down one flat biome-coloured ground quad at its terrain
///   height (water-blue at sea level for ocean), so from a treetop the land
///   stretches to the horizon instead of floating over sky.
/// Tree planning is deterministic per chunk, so every silhouette stands exactly
/// where the real tree will when its chunk streams in.
#[derive(Resource, Default)]
pub struct SilhouetteWorld {
    spawned: HashMap<IVec2, Entity>,
    queue: VecDeque<IVec2>,
    last_player_chunk: Option<IVec2>,
}

#[derive(Resource)]
pub struct SilhouetteAssets {
    trunk_mesh: Handle<Mesh>,
    crown_mesh: Handle<Mesh>,
    cone_mesh: Handle<Mesh>,
    ground_mesh: Handle<Mesh>,
    /// Per species: (trunk material, crown material) — dark, lit, fog-aware.
    materials: HashMap<TreeSpecies, (Handle<StandardMaterial>, Handle<StandardMaterial>)>,
    /// Ground colour per biome, indexed by `AussieBiome as usize`.
    ground_materials: [Handle<StandardMaterial>; 8],
}

/// A silhouette tree that spins to face the camera (yaw only).
#[derive(Component)]
pub struct SilhouetteBillboard {
    world_x: f32,
    world_z: f32,
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

/// Unit ground tile: 1×1 in the XZ plane, centred on the origin, facing up.
fn ground_tile_mesh() -> Mesh {
    let mut mesh = Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::default());
    mesh.insert_attribute(
        Mesh::ATTRIBUTE_POSITION,
        vec![
            [-0.5, 0.0, -0.5],
            [-0.5, 0.0, 0.5],
            [0.5, 0.0, 0.5],
            [0.5, 0.0, -0.5],
        ],
    );
    mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, vec![[0.0, 1.0, 0.0]; 4]);
    mesh.insert_indices(Indices::U32(vec![0, 1, 2, 0, 2, 3]));
    mesh
}

fn ground_color(biome: AussieBiome) -> Color {
    // Matches the dominant surface block of each biome so the cutout ground
    // blends with real terrain at the streaming boundary.
    let (r, g, b) = match biome {
        AussieBiome::Ocean => (0.22, 0.42, 0.68),
        AussieBiome::TropicalSavanna => (0.32, 0.52, 0.22),
        AussieBiome::AridOutback => (0.72, 0.38, 0.22),
        AussieBiome::Pilbara => (0.66, 0.34, 0.22),
        AussieBiome::Mediterranean => (0.38, 0.50, 0.24),
        AussieBiome::TemperateForest => (0.32, 0.52, 0.22),
        AussieBiome::CoastalBush => (0.36, 0.52, 0.24),
        AussieBiome::Tasmania => (0.28, 0.46, 0.24),
    };
    Color::srgb(r, g, b)
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

    // Unit crown: 12-gon disc of radius 0.5 centred on the origin.
    let circle: Vec<[f32; 2]> = (0..12)
        .map(|i| {
            let a = i as f32 / 12.0 * std::f32::consts::TAU;
            [a.cos() * 0.5, a.sin() * 0.5]
        })
        .collect();
    let crown_mesh = meshes.add(plane_mesh(&circle));

    // Unit conifer: triangle rising from y=0.
    let cone_mesh = meshes.add(plane_mesh(&[[-0.5, 0.0], [0.5, 0.0], [0.0, 1.0]]));

    let ground_mesh = meshes.add(ground_tile_mesh());

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

    let ground_materials = [
        AussieBiome::Ocean,
        AussieBiome::TropicalSavanna,
        AussieBiome::AridOutback,
        AussieBiome::Pilbara,
        AussieBiome::Mediterranean,
        AussieBiome::TemperateForest,
        AussieBiome::CoastalBush,
        AussieBiome::Tasmania,
    ]
    .map(|biome| {
        materials.add(StandardMaterial {
            base_color: ground_color(biome),
            perceptual_roughness: 1.0,
            reflectance: 0.0,
            ..default()
        })
    });

    commands.insert_resource(SilhouetteAssets {
        trunk_mesh,
        crown_mesh,
        cone_mesh,
        ground_mesh,
        materials: mats,
        ground_materials,
    });
}

/// Decide which unmaterialised chunks should show silhouettes. Cheap enough to
/// run every frame: the full ring scan only happens when the player crosses a
/// chunk border.
pub fn plan_tree_silhouettes(
    mut commands: Commands,
    chunk_world: Res<crate::ChunkWorld>,
    mut sil: ResMut<SilhouetteWorld>,
    cam_q: Query<&Transform, With<Camera>>,
) {
    let Ok(cam) = cam_q.get_single() else {
        return;
    };
    let player_chunk = world_to_chunk(cam.translation.x, cam.translation.z);

    // Retire silhouettes whose chunk went live (real trees incoming) or fell
    // out of range.
    let to_remove: Vec<IVec2> = sil
        .spawned
        .keys()
        .copied()
        .filter(|coord| {
            chunk_world.loaded.contains_key(coord)
                || chunk_chebyshev_distance(*coord, player_chunk) > SILHOUETTE_CHUNK_DISTANCE + 1
        })
        .collect();
    for coord in to_remove {
        if let Some(entity) = sil.spawned.remove(&coord) {
            commands.entity(entity).despawn_recursive();
        }
    }

    if sil.last_player_chunk == Some(player_chunk) {
        return;
    }
    sil.last_player_chunk = Some(player_chunk);

    sil.queue.clear();
    let mut needed = Vec::new();
    for dx in -SILHOUETTE_CHUNK_DISTANCE..=SILHOUETTE_CHUNK_DISTANCE {
        for dz in -SILHOUETTE_CHUNK_DISTANCE..=SILHOUETTE_CHUNK_DISTANCE {
            let coord = player_chunk + IVec2::new(dx, dz);
            let dist = chunk_chebyshev_distance(coord, player_chunk);
            // Leave the streamed neighbourhood to the real trees.
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
    sil.queue.extend(needed.into_iter().map(|(_, c)| c));
}

/// Drain a few queued chunks per frame; each spawns one cheap parent entity
/// holding the chunk's ground tile plus a billboard cutout for every tree the
/// chunk will eventually grow.
pub fn process_silhouette_queue(
    mut commands: Commands,
    assets: Res<SilhouetteAssets>,
    archive: Res<ChunkArchive>,
    chunk_world: Res<crate::ChunkWorld>,
    mut sil: ResMut<SilhouetteWorld>,
) {
    for _ in 0..SILHOUETTES_PER_FRAME {
        let Some(coord) = sil.queue.pop_front() else {
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

        commands.entity(parent).with_children(|chunk| {
            // Distant ground: one flat tile at the local terrain height (sea
            // level for ocean) so high vantage points see land, not sky.
            let ground_y = if biome == AussieBiome::Ocean {
                VOXEL_SIZE
            } else {
                surface_top_world_y(center_x, center_z)
            };
            chunk.spawn((
                Mesh3d(assets.ground_mesh.clone()),
                MeshMaterial3d(assets.ground_materials[biome as usize].clone()),
                NotShadowCaster,
                NotShadowReceiver,
                Transform {
                    translation: Vec3::new(CHUNK_SIZE * 0.5, ground_y, CHUNK_SIZE * 0.5),
                    scale: Vec3::new(CHUNK_SIZE, 1.0, CHUNK_SIZE),
                    ..default()
                },
            ));

            for tree in &trees {
                let spec = silhouette_spec(tree.species, tree.tree_seed);
                let Some((trunk_mat, crown_mat)) = assets.materials.get(&tree.species) else {
                    continue;
                };
                let h = spec.height_ft;
                let r = spec.crown_radius_ft;

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
                            tree_root.spawn((
                                Mesh3d(assets.cone_mesh.clone()),
                                MeshMaterial3d(crown_mat.clone()),
                                NotShadowCaster,
                                NotShadowReceiver,
                                Transform {
                                    translation: Vec3::Y * (h * 0.28),
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
                                    translation: Vec3::Y * (h * spec.crown_center_frac),
                                    scale: Vec3::new(r * 2.0, r * 1.5, r * 2.0),
                                    ..default()
                                },
                            ));
                        }
                    });
            }
        });

        sil.spawned.insert(coord, parent);
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
