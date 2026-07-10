//! Grass: the player's sprites pixel-extruded into solid 3D "lego" clumps,
//! scattered per chunk by biome, sized and faded by distance, and swayed by the
//! wind. Owns its own assets, scatter, and sway — `GrassPlugin` wires the sway
//! system; `build_grass_assets` is called once from the world setup, and the
//! chunk streamer calls `scatter_chunk_grass` as each chunk lands.

use bevy::pbr::NotShadowCaster;
use bevy::prelude::*;
use std::collections::HashSet;

use crate::australia::{biome_profile, AussieBiome};
use crate::terrain::build_culled_voxel_mesh;
use crate::world::{
    chunk_seed, chunk_world_origin, GardenRng, CHUNK_SIZE, CHUNK_VOXELS, VOXEL_SIZE, WORLD_SEED,
    WORM_LENGTH,
};
use crate::weather::Wind;

pub struct GrassPlugin;

impl Plugin for GrassPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Update, sway_grass);
    }
}

/// Grass clumps are leaf-width crossed quads wearing the player's white
/// cutout sprites, tinted per species with a base→tip vertex-colour gradient.
/// Mitchell grass is two-part: blades plus a separately tinted seed-head top.
struct GrassSpecies {
    mesh: Handle<Mesh>,
    material: Handle<StandardMaterial>,
    /// Optional stacked top piece (Mitchell's straw seed heads).
    top: Option<(Handle<Mesh>, Handle<StandardMaterial>)>,
}

#[derive(Resource)]
pub(crate) struct GrassAssets {
    species: Vec<GrassSpecies>,
    /// Per biome (`AussieBiome as usize`): species index + clumps per chunk.
    by_biome: [Option<(usize, (i32, i32))>; 8],
}

/// One planted clump: fixed facing, its own sway rhythm, and the full-size
/// scale it wears up close (sway_grass shrinks it toward this × a distance fade
/// so the carpet dissolves into the distance instead of cropping at a hard ring).
#[derive(Component, Clone, Copy)]
struct GrassClump {
    yaw: f32,
    phase: f32,
    freq: f32,
    base_scale: f32,
    /// Sprout clock, in seconds: starts negative (a per-clump stagger), counts
    /// up every frame; the clump is full-size once it passes SPROUT_SECS. Kept
    /// on the clump (not a separate component) so finishing doesn't need a
    /// Commands archetype move for hundreds of entities per chunk.
    grow: f32,
}

/// Freshly streamed grass sprouts out of the soil instead of popping in: each
/// clump waits its random stagger (up to SPROUT_STAGGER) then grows to full
/// size over SPROUT_SECS, so a new chunk fills in as a ~2s wave, not a flash.
const SPROUT_SECS: f32 = 0.9;
const SPROUT_STAGGER: f32 = 1.2;

/// Grass thins toward the render edge rather than hard-cropping: full size
/// within `GRASS_FULL_FT`, shrinking to nothing by `GRASS_GONE_FT` (just inside
/// where the clump entities actually end), so the field melts into the haze and
/// a freshly streamed edge clump grows in smoothly instead of popping.
const GRASS_FULL_FT: f32 = 60.0;
const GRASS_GONE_FT: f32 = 95.0;

/// Grass clumps match the collectible leaves in width (~3 worm-lengths) and
/// stand five widths tall — a worm crawls through a canopy of it, and the
/// biggest clumps (see the size spread in scatter_chunk_grass) become a jungle
/// arching overhead, dialling up the felt scale of everything beyond.
pub(crate) const GRASS_WIDTH: f32 = WORM_LENGTH * 3.0;
pub(crate) const GRASS_HEIGHT: f32 = GRASS_WIDTH * 5.0;

/// Lego grass: every opaque pixel of the player's sprite becomes a solid box
/// one pixel deep — genuine 3D pixel art with joined faces on every side,
/// meshed with the same culled-voxel builder the trees use, then rescaled to
/// world size and tinted with a bottom→tip gradient. Oversized sprites are
/// downsampled to lego resolution first.
fn grass_lego_mesh(
    path: &str,
    width: f32,
    height: f32,
    y_offset: f32,
    base: (f32, f32, f32),
    tip: (f32, f32, f32),
) -> Mesh {
    let img = image::open(path)
        .unwrap_or_else(|e| panic!("failed to load grass sprite {path}: {e}"))
        .to_rgba8();
    let img = if img.width() > 48 || img.height() > 48 {
        image::imageops::resize(&img, 32, 32, image::imageops::FilterType::Nearest)
    } else {
        img
    };
    let (w, h) = img.dimensions();

    let mut cells: HashSet<IVec3> = HashSet::new();
    for py in 0..h {
        for px in 0..w {
            if img.get_pixel(px, py)[3] > 128 {
                cells.insert(IVec3::new(px as i32, (h - 1 - py) as i32, 0));
            }
        }
    }

    let mut mesh = build_culled_voxel_mesh(&cells, 1.0);

    // Rescale pixel units to world feet (centred on x, one pixel of depth
    // centred on z) and paint the height gradient into vertex colours.
    let sx = width / w as f32;
    let sy = height / h as f32;
    let cx = w as f32 * 0.5;
    let mut colors: Vec<[f32; 4]> = Vec::new();
    if let Some(bevy::render::mesh::VertexAttributeValues::Float32x3(positions)) =
        mesh.attribute_mut(Mesh::ATTRIBUTE_POSITION)
    {
        for p in positions.iter_mut() {
            let t = (p[1] / h as f32).clamp(0.0, 1.0);
            colors.push([
                base.0 + (tip.0 - base.0) * t,
                base.1 + (tip.1 - base.1) * t,
                base.2 + (tip.2 - base.2) * t,
                1.0,
            ]);
            p[0] = (p[0] - cx) * sx;
            p[1] = p[1] * sy + y_offset;
            p[2] = (p[2] - 0.5) * sx;
        }
    }
    mesh.insert_attribute(Mesh::ATTRIBUTE_COLOR, colors);
    mesh
}

/// Build every grass species mesh/material and the per-biome density table.
/// Called once from the world setup.
pub(crate) fn build_grass_assets(
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
) -> GrassAssets {
    // Lego grass: the player's sprites pixel-extruded into solid 3D — one
    // shared opaque vertex-colour material for everything (no cutout, no
    // sorting). Mitchell stacks straw seed heads over blue-green blades.
    let grass_material = materials.add(StandardMaterial {
        base_color: Color::WHITE,
        perceptual_roughness: 1.0,
        ..default()
    });
    let mitchell = GrassSpecies {
        mesh: meshes.add(grass_lego_mesh(
            "assets/grass/mitchell.png",
            GRASS_WIDTH,
            GRASS_HEIGHT,
            0.0,
            (0.34, 0.44, 0.26),
            (0.55, 0.60, 0.34),
        )),
        material: grass_material.clone(),
        top: Some((
            meshes.add(grass_lego_mesh(
                "assets/grass/mitchell_top.png",
                GRASS_WIDTH,
                GRASS_HEIGHT * 0.6,
                GRASS_HEIGHT * 0.55,
                (0.72, 0.62, 0.38),
                (0.82, 0.74, 0.46),
            )),
            grass_material.clone(),
        )),
    };
    let kangaroo = GrassSpecies {
        mesh: meshes.add(grass_lego_mesh(
            "assets/grass/kangaroo.png",
            GRASS_WIDTH,
            GRASS_HEIGHT,
            0.0,
            (0.24, 0.40, 0.18),
            (0.60, 0.36, 0.22),
        )),
        material: grass_material.clone(),
        top: None,
    };
    let button = GrassSpecies {
        mesh: meshes.add(grass_lego_mesh(
            "assets/grass/button.png",
            GRASS_WIDTH,
            GRASS_HEIGHT,
            0.0,
            (0.28, 0.38, 0.20),
            (0.52, 0.50, 0.30),
        )),
        material: grass_material,
        top: None,
    };

    let mut by_biome: [Option<(usize, (i32, i32))>; 8] = [None; 8];
    by_biome[AussieBiome::TropicalSavanna as usize] = Some((0, (40, 80)));
    by_biome[AussieBiome::AridOutback as usize] = Some((0, (8, 20)));
    by_biome[AussieBiome::Pilbara as usize] = Some((0, (10, 24)));
    by_biome[AussieBiome::Mediterranean as usize] = Some((1, (60, 110)));
    // Forests get a LOT of grass.
    by_biome[AussieBiome::TemperateForest as usize] = Some((1, (110, 180)));
    by_biome[AussieBiome::CoastalBush as usize] = Some((1, (60, 100)));
    by_biome[AussieBiome::Tasmania as usize] = Some((2, (70, 120)));

    GrassAssets {
        species: vec![mitchell, kangaroo, button],
        by_biome,
    }
}

/// Grass shivers fast and light — same wind and gusts as the trees, higher
/// frequency, flattening progressively in a gale. And it makes way for the
/// worm: clumps near the little guy bend away, springing back as he passes.
fn sway_grass(
    time: Res<Time>,
    wind: Res<Wind>,
    cam_q: Query<&Transform, (With<Camera>, Without<GrassClump>)>,
    mut clumps: Query<(&mut GrassClump, &GlobalTransform, &mut Transform)>,
) {
    let t = time.elapsed_secs();
    let dt = time.delta_secs();
    let gust = 0.5 + 0.5 * ((t * 0.11).sin() * 0.6 + (t * 0.043).sin() * 0.4);
    let wind_axis = Vec3::new(-wind.dir.y, 0.0, wind.dir.x);
    let force = wind.strength / 5.0;
    let cam_pos = cam_q.get_single().map(|c| c.translation).ok();

    // How close the worm must be before grass yields, and how hard it bends.
    const PUSH_RADIUS: f32 = 0.9;
    const PUSH_MAX_RAD: f32 = 1.25;

    for (mut clump, global, mut tf) in &mut clumps {
        // Advance the sprout clock unconditionally — an add + min is cheaper
        // than branching per clump, and full-grown clumps just saturate.
        clump.grow = (clump.grow + dt).min(SPROUT_SECS);
        let g = (clump.grow / SPROUT_SECS).clamp(0.0, 1.0);
        let sprout = g * g * (3.0 - 2.0 * g); // smoothstep: eases in AND out

        let wind_angle = force
            * ((0.06 + 0.11 * gust) * (t * clump.freq + clump.phase).sin() + 0.35 * gust);
        let mut rotation = Quat::from_axis_angle(wind_axis, wind_angle);

        // Full size up close, melting to nothing by the render edge so grass
        // dissolves into the haze rather than cropping at a hard ring.
        let mut fade = 1.0;
        if let Some(cam) = cam_pos {
            let world = global.translation();
            let away = Vec3::new(world.x - cam.x, 0.0, world.z - cam.z);
            let dist = away.length();
            if dist < PUSH_RADIUS && dist > 0.001 {
                let strength = 1.0 - dist / PUSH_RADIUS;
                let bend_axis = Vec3::Y.cross(away / dist);
                rotation =
                    Quat::from_axis_angle(bend_axis, PUSH_MAX_RAD * strength * strength) * rotation;
            }
            let f = ((dist - GRASS_FULL_FT) / (GRASS_GONE_FT - GRASS_FULL_FT)).clamp(0.0, 1.0);
            fade = 1.0 - f * f * (3.0 - 2.0 * f); // smoothstep, sharp near → 0 far
        }

        tf.rotation = rotation * Quat::from_rotation_y(clump.yaw);
        tf.scale = Vec3::splat(clump.base_scale * fade * sprout);
    }
}

/// Blanket a chunk in grass clumps — species and density by biome, planted on
/// the chunk's REAL column tops so every clump roots exactly on the dirt.
pub(crate) fn scatter_chunk_grass(
    commands: &mut Commands,
    chunk_entity: Entity,
    coord: IVec2,
    tops: &[i32],
    grass: &GrassAssets,
) {
    let center = chunk_world_origin(coord) + Vec3::new(CHUNK_SIZE * 0.5, 0.0, CHUNK_SIZE * 0.5);
    let profile = biome_profile(center.x, center.z);
    let Some((species_idx, (lo, hi))) = grass.by_biome[profile.biome as usize] else {
        return;
    };
    let species = &grass.species[species_idx];

    let mut rng = GardenRng::new(chunk_seed(WORLD_SEED, coord) ^ 0x6A55_6A55);
    // 3× the biome's base density — a properly lush carpet.
    let count = rng.range_i(lo, hi) * 3;

    commands.entity(chunk_entity).with_children(|chunk| {
        // Grass grows in families, not white noise: most clumps belong to a
        // cluster — a shared centre, footprint and size bias, so a patch reads
        // as siblings of one bush — and the rest scatter as loners so the gaps
        // between patches aren't sterile.
        let mut remaining = count;
        while remaining > 0 {
            let clustered = rng.chance(0.75);
            let group = if clustered {
                rng.range_i(4, 12).min(remaining)
            } else {
                1
            };
            remaining -= group;
            let gx = rng.range(0.2, CHUNK_SIZE - 0.2);
            let gz = rng.range(0.2, CHUNK_SIZE - 0.2);
            let radius = if clustered { rng.range(1.5, 4.5) } else { 0.0 };
            // Siblings share a rough family height; the per-clump roll below
            // still lets a sprout hide under a monster within one patch.
            let size_bias = if clustered { rng.range(0.5, 1.6) } else { 1.0 };

            for _ in 0..group {
                // Linear radial falloff: dense core, thinning fringe.
                let ang = rng.range(0.0, std::f32::consts::TAU);
                let r = radius * rng.range(0.0, 1.0);
                let lx = gx + ang.cos() * r;
                let lz = gz + ang.sin() * r;
                // Drop fringe members that fall off the chunk instead of
                // clamping them — clamping piles clumps into a visible line
                // along the border every 32 ft.
                if !(0.2..=CHUNK_SIZE - 0.2).contains(&lx)
                    || !(0.2..=CHUNK_SIZE - 0.2).contains(&lz)
                {
                    continue;
                }
                let cx = ((lx / VOXEL_SIZE) as i32).clamp(0, CHUNK_VOXELS - 1);
                let cz = ((lz / VOXEL_SIZE) as i32).clamp(0, CHUNK_VOXELS - 1);
                let y = (tops[(cz * CHUNK_VOXELS + cx) as usize] + 1) as f32 * VOXEL_SIZE;

                // Wild size spread: anything from a 5% sprout barely clearing
                // the soil to a 300% monster clump towering three times standard
                // height — the tallest become the worm's overhead jungle canopy.
                // Kept on the clump so the distance fade scales relative to it.
                let base_scale = if clustered {
                    (size_bias * rng.range(0.3, 2.0)).clamp(0.05, 3.0)
                } else {
                    rng.range(0.05, 3.0)
                };
                let clump = GrassClump {
                    yaw: rng.range(0.0, std::f32::consts::TAU),
                    phase: rng.range(0.0, std::f32::consts::TAU),
                    freq: rng.range(2.2, 3.6),
                    base_scale,
                    grow: -rng.range(0.0, SPROUT_STAGGER),
                };
                // Spawn near-zero scale: commands flush after this stage, so a
                // frame could render before sway_grass takes over the scale —
                // starting tiny guarantees no single full-size pop frame.
                let transform = Transform {
                    translation: Vec3::new(lx, y, lz),
                    rotation: Quat::from_rotation_y(clump.yaw),
                    scale: Vec3::splat(0.001),
                };

                if let Some((top_mesh, top_mat)) = &species.top {
                    chunk.spawn((
                        GrassClump { ..clump },
                        Mesh3d(top_mesh.clone()),
                        MeshMaterial3d(top_mat.clone()),
                        NotShadowCaster,
                        transform,
                    ));
                }
                chunk.spawn((
                    clump,
                    Mesh3d(species.mesh.clone()),
                    MeshMaterial3d(species.material.clone()),
                    NotShadowCaster,
                    transform,
                ));
            }
        }
    });
}
