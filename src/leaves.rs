//! Collectible ground leaves: a 3D mesh extruded from the enhanced leaf PNG's
//! silhouette, scattered per chunk (and a hand-placed welcome set at spawn),
//! bobbing and spinning in the wind. Sized to match a grass clump. Eating them
//! lives with the worm (see `eat_leaves`); this module owns their look and life.

use bevy::asset::RenderAssetUsages;
use bevy::prelude::*;
use bevy::render::mesh::{Indices, PrimitiveTopology};

use crate::australia::{biome_profile, AussieBiome};
use crate::grass::GRASS_HEIGHT;
use crate::topography::surface_top_world_y;
use crate::world::{chunk_seed, chunk_world_origin, GardenRng, CHUNK_SIZE, WORLD_SEED};
use crate::weather::Wind;

pub struct LeavesPlugin;

impl Plugin for LeavesPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Update, animate_floating_leaves);
    }
}

/// Marker for a collectible leaf (the worm eats these).
#[derive(Component)]
pub(crate) struct Leaf;

#[derive(Component)]
struct FloatingLeaf {
    base_x: f32,
    base_y: f32,
    base_z: f32,
    phase: f32,
    bob_speed: f32,
    spin_speed: f32,
    base_rotation: Quat, // artistic starting orientation
}

/// Shared handles for the extruded-PNG leaf so every chunk can scatter copies.
#[derive(Resource)]
pub(crate) struct LeafAssets {
    pub(crate) mesh: Handle<Mesh>,
    pub(crate) material: Handle<StandardMaterial>,
}

/// Build the shared leaf mesh + material, scatter the hand-placed welcome set,
/// and insert the [`LeafAssets`] resource. Called once from the world setup.
pub(crate) fn setup_leaves(
    commands: &mut Commands,
    asset_server: &AssetServer,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
) {
    let leaf_texture = asset_server.load("Enhanced Leaf.png");
    let leaf_assets = LeafAssets {
        material: materials.add(StandardMaterial {
            base_color_texture: Some(leaf_texture),
            // The mesh geometry *is* the leaf outline now — no need for alpha
            // cutout. Opaque is cleanest + cheapest.
            alpha_mode: AlphaMode::Opaque,
            double_sided: true,
            ..default()
        }),
        mesh: create_extruded_leaf_mesh(meshes),
    };
    spawn_textured_leaves(commands, &leaf_assets);
    commands.insert_resource(leaf_assets);
}


/// A collectible leaf is sized to match a grass clump — its long axis equals
/// the grass height (the mesh's longest dimension is 0.95 units). Wayyy bigger
/// than the old ~3-worm-length leaf; you crawl up to a leaf like a tuft of grass.
const LEAF_BASE_SCALE: f32 = GRASS_HEIGHT / 0.95;

/// How far to lift a leaf of this rotation + world scale so its LOWEST point
/// clears the ground rather than burying half the (grass-clump-big) leaf. The
/// mesh half-extents (long axis 0.95→0.475, narrow width, thin) are projected
/// through the rotation onto world-Y.
fn leaf_ground_clearance(rot: Quat, scale: f32) -> f32 {
    let m = Mat3::from_quat(rot);
    scale * (m.x_axis.y.abs() * 0.28 + m.y_axis.y.abs() * 0.02 + m.z_axis.y.abs() * 0.475)
}

/// Spawns the hand-placed welcome leaves around the starting beach.
/// (Chunk streaming scatters many more everywhere — see [`scatter_chunk_leaves`].)
fn spawn_textured_leaves(commands: &mut Commands, leaf_assets: &LeafAssets) {
    let leaf_scale = LEAF_BASE_SCALE;

    // Leaf data: (position, base rotation, scale multiplier)
    let leaf_spawns: [(Vec3, Quat, f32); 7] = [
        (Vec3::new(-3.5, 0.8, -4.2), Quat::from_rotation_x(-0.2), 0.9),
        (Vec3::new(6.2, 0.7, 6.8), Quat::from_rotation_x(-0.15) * Quat::from_rotation_z(-0.4), 1.0),
        (Vec3::new(-11.5, 1.0, 13.0), Quat::from_rotation_x(-0.25) * Quat::from_rotation_z(0.3), 0.85),
        (Vec3::new(20.5, 0.9, -4.5), Quat::from_rotation_x(-0.18), 0.95),
        (Vec3::new(-6.1, 1.6, -8.6), Quat::from_euler(EulerRot::XYZ, -0.7, 0.5, 0.2), 0.8),
        (Vec3::new(9.1, 1.4, 13.8), Quat::from_euler(EulerRot::XYZ, -0.5, -1.0, -0.15), 1.05),
        (Vec3::new(-5.8, 2.2, -10.2), Quat::from_euler(EulerRot::XYZ, -1.0, 0.8, 0.25), 0.75),
    ];

    for (i, (pos, base_rot, scale)) in leaf_spawns.iter().enumerate() {
        let phase = i as f32 * 1.7;
        let bob_speed = 1.8 + (i as f32 * 0.07);
        let spin_speed = 0.85 + (i as f32 * 0.1);

        // Hover heights were authored against flat ground for small leaves —
        // ride the local terrain, and lift by the (now grass-clump-big) leaf's
        // own rotated half-height so it doesn't clip.
        let full_scale = *scale * leaf_scale;
        let pos = Vec3::new(
            pos.x,
            surface_top_world_y(pos.x, pos.z) + leaf_ground_clearance(*base_rot, full_scale) + pos.y,
            pos.z,
        );

        commands.spawn((
            Mesh3d(leaf_assets.mesh.clone()),
            MeshMaterial3d(leaf_assets.material.clone()),
            Transform {
                translation: pos,
                rotation: *base_rot,
                scale: Vec3::splat(full_scale),
            },
            Leaf,
            FloatingLeaf {
                base_x: pos.x,
                base_y: pos.y,
                base_z: pos.z,
                phase,
                bob_speed,
                spin_speed,
                base_rotation: *base_rot,
            },
        ));
    }
}

/// Litter every land chunk with bobbing leaves — deterministic per chunk, count
/// scaled by how wooded the biome is, each riding the local terrain surface.
pub(crate) fn scatter_chunk_leaves(
    commands: &mut Commands,
    chunk_entity: Entity,
    coord: IVec2,
    leaf_assets: &LeafAssets,
) {
    let origin = chunk_world_origin(coord);
    let center = origin + Vec3::new(CHUNK_SIZE * 0.5, 0.0, CHUNK_SIZE * 0.5);
    let profile = biome_profile(center.x, center.z);
    if profile.biome == AussieBiome::Ocean {
        return;
    }

    let mut rng = GardenRng::new(chunk_seed(WORLD_SEED, coord) ^ 0x1EAF_1EAF);
    let count = (rng.range(3.0, 7.0) * (0.4 + profile.tree_density * 0.5)).round() as i32;

    commands.entity(chunk_entity).with_children(|chunk| {
        for _ in 0..count {
            let lx = rng.range(1.5, CHUNK_SIZE - 1.5);
            let lz = rng.range(1.5, CHUNK_SIZE - 1.5);
            let ground = surface_top_world_y(origin.x + lx, origin.z + lz);

            let base_rot = Quat::from_euler(
                EulerRot::XYZ,
                rng.range(-1.0, 0.2),
                rng.range(0.0, std::f32::consts::TAU),
                rng.range(-0.4, 0.4),
            );
            let leaf_scale = LEAF_BASE_SCALE * rng.range(0.65, 1.15);

            // The leaves are grass-clump big now, so seating them by their centre
            // buried the lower half. Lift each so its LOWEST point clears the
            // ground, then add a small hover.
            let y = ground + leaf_ground_clearance(base_rot, leaf_scale) + rng.range(0.3, 1.5);

            chunk.spawn((
                Mesh3d(leaf_assets.mesh.clone()),
                MeshMaterial3d(leaf_assets.material.clone()),
                Transform {
                    translation: Vec3::new(lx, y, lz),
                    rotation: base_rot,
                    scale: Vec3::splat(leaf_scale),
                },
                Leaf,
                FloatingLeaf {
                    base_x: lx,
                    base_y: y,
                    base_z: lz,
                    phase: rng.range(0.0, std::f32::consts::TAU),
                    bob_speed: rng.range(1.2, 2.4),
                    spin_speed: rng.range(0.45, 1.3),
                    base_rotation: base_rot,
                },
            ));
        }
    });
}

/// Animates the floating 3D leaves (now with real thickness).
/// They bob and spin — and they answer the wind: drifting downwind, leaning
/// with it, and spinning faster the harder it blows.
fn animate_floating_leaves(
    time: Res<Time>,
    wind: Res<Wind>,
    mut query: Query<(&mut Transform, &FloatingLeaf)>,
) {
    let t = time.elapsed_secs();
    let force = wind.strength / 5.0;
    let lean_axis = Vec3::new(-wind.dir.y, 0.0, wind.dir.x);

    for (mut transform, floating) in &mut query {
        // Gentle vertical bob, a little choppier when the wind is up.
        let bob = (t * floating.bob_speed * (1.0 + force * 0.8) + floating.phase).sin() * 0.20;
        transform.translation.y = floating.base_y + bob;

        // Downwind drift: tethered to the spawn point, straining with gusts.
        let drift = force * (0.22 + 0.12 * (t * 1.3 + floating.phase).sin());
        transform.translation.x = floating.base_x + wind.dir.x * drift;
        transform.translation.z = floating.base_z + wind.dir.y * drift;

        // Spin around Y — a gale whips leaves into a proper twirl.
        let spin = Quat::from_rotation_y(
            t * floating.spin_speed * (1.0 + force * 1.6) + floating.phase * 0.5,
        );

        // Combine:
        // - A downwind lean that grows with the wind
        // - The artistic base rotation the leaf was given at spawn
        // - Y spin for rotation
        // - Strong vertical orientation so the plane stands up instead of lying flat
        let lean = Quat::from_axis_angle(lean_axis, force * 0.4);
        let vertical_stand = Quat::from_rotation_x(-1.4);
        transform.rotation = lean * spin * vertical_stand * floating.base_rotation;
    }
}

/// Creates a slightly extruded 3D leaf mesh whose silhouette *exactly* follows
/// the opaque contours of the higher-res 8-bit leaf (assets/leaf.png).
/// We trace per-row min/max opaque pixels, then rectify the polyline to pure
/// horizontal+vertical segments so the outline (and extruded side walls) are
/// chunky 8-bit jagged, exactly following the pixel steps of the art. Then
/// extrude a tiny bit for thickness. The result is a cool retro low-poly 3D leaf.
///
/// The leaf face lies in the X/Z plane (matching old Plane3d) + thickness on Y
/// so all the existing bob/spin/base rotations continue to work unchanged.
fn create_extruded_leaf_mesh(meshes: &mut Assets<Mesh>) -> Handle<Mesh> {
    // Embed the source PNG so the mesh shape is derived directly from it at
    // compile time (change the PNG and rebuild to update the 3D outline).
    const LEAF_PNG: &[u8] = include_bytes!("../assets/Enhanced Leaf.png");
    let img = image::load_from_memory_with_format(LEAF_PNG, image::ImageFormat::Png)
        .expect("Failed to decode embedded assets/Enhanced Leaf.png for 3D leaf contour")
        .to_rgba8();

    let (w, h) = img.dimensions();
    let alpha_threshold: u8 = 128;

    // Build per-row spans of opaque pixels (only rows that have any leaf).
    // This captures the exact left/right silhouette at every scanline.
    let mut row_spans: Vec<(u32, u32, u32)> = Vec::new(); // (y, left, right)
    let mut good_u = 0.5f32;
    let mut good_v = 0.5f32;
    let mut found_green = false;
    for y in 0..h {
        let mut left = None::<u32>;
        let mut right = None::<u32>;
        for x in 0..w {
            let p = img.get_pixel(x, y);
            if p[3] > alpha_threshold && p[1] > p[0] && p[1] > p[2] && p[1] > 100 {
                // only bright green pixels for the silhouette (ignore black outline/detail)
                if !found_green {
                    good_u = x as f32 / w as f32;
                    good_v = y as f32 / h as f32;
                    found_green = true;
                }
                if left.is_none() {
                    left = Some(x);
                }
                right = Some(x);
            }
        }
        if let (Some(l), Some(r)) = (left, right) {
            row_spans.push((y, l, r));
        }
    }

    // For top and bottom, compute mid u to make them pointed (remove flat horizontal lines at top/base).
    // Use pixel centers for accurate mapping to green texels.
    let top_l_u = if !row_spans.is_empty() { (row_spans[0].1 as f32 + 0.5) / w as f32 } else { 0.5 };
    let top_r_u = if !row_spans.is_empty() { (row_spans[0].2 as f32 + 0.5) / w as f32 } else { 0.5 };
    let top_mid_u = (top_l_u + top_r_u) / 2.0;
    let bot_l_u = if !row_spans.is_empty() { (row_spans.last().unwrap().1 as f32 + 0.5) / w as f32 } else { 0.5 };
    let bot_r_u = if !row_spans.is_empty() { (row_spans.last().unwrap().2 as f32 + 0.5) / w as f32 } else { 0.5 };
    let bot_mid_u = (bot_l_u + bot_r_u) / 2.0;

    // Decide whether to force pointed tips: only for narrow end rows (true tips in raster).
    // Wide ends (like this PNG top~65px, bot~98px) keep their natural flat-ish contour width.
    let top_pix_width = if !row_spans.is_empty() { row_spans[0].2 as i32 - row_spans[0].1 as i32 + 1 } else { 0 };
    let bot_pix_width = if !row_spans.is_empty() { row_spans.last().unwrap().2 as i32 - row_spans.last().unwrap().1 as i32 + 1 } else { 0 };
    let point_top = top_pix_width < 12;
    let point_bot = bot_pix_width < 12;

    // Compute the actual content bounding box in UV (so we can map *just* the leaf
    // pixels to a nice world size without the PNG's transparent margins). Use centers.
    let min_u = row_spans.iter().map(|&(_, l, _)| (l as f32 + 0.5) / w as f32).fold(f32::INFINITY, f32::min);
    let max_u = row_spans.iter().map(|&(_, _, r)| (r as f32 + 0.5) / w as f32).fold(f32::NEG_INFINITY, f32::max);
    let min_v = row_spans.first().map(|(y, _, _)| *y as f32 / h as f32).unwrap_or(0.0);
    let max_v = row_spans.last().map(|(y, _, _)| *y as f32 / h as f32).unwrap_or(1.0);
    let span_u = (max_u - min_u).max(0.0001);
    let span_v = (max_v - min_v).max(0.0001);
    let center_u = (min_u + max_u) * 0.5;
    let center_v = (min_v + max_v) * 0.5;

    // Map the content bbox with aspect preservation so the elongated
    // high-res leaf keeps its natural proportions.
    let max_dim = 0.95;
    let (desired_w, desired_h) = if span_u >= span_v {
        (max_dim, max_dim * (span_v / span_u))
    } else {
        (max_dim * (span_u / span_v), max_dim)
    };

    // Build the boundary polygon in texture UV space (0..1).
    // Left chain top-to-bottom, only adding a point when the column actually changes
    // (keeps key silhouette corners/steps, drops redundant points on vertical runs).
    // Use pixel centers so nearest sampling + small inset lands on green not border.
    let mut left_chain: Vec<[f32; 2]> = Vec::new(); // [u, v] tex
    for &(y, l, _) in &row_spans {
        let u = (l as f32 + 0.5) / w as f32;
        let v = y as f32 / h as f32;
        left_chain.push([u, v]);
    }

    let mut right_chain: Vec<[f32; 2]> = Vec::new();
    for &(y, _, r) in &row_spans {
        let u = (r as f32 + 0.5) / w as f32; // center of the rightmost green pixel
        let v = y as f32 / h as f32;
        right_chain.push([u, v]);
    }

    // Make the chains "jagged 8-bit" by turning any diagonal connections into
    // explicit horizontal + vertical segments. This makes the mesh outline
    // and extruded side walls follow the pixel steps exactly, for a cool retro
    // chunky look instead of smoothed diagonals.
    fn rectify_to_axis_aligned(chain: Vec<[f32; 2]>) -> Vec<[f32; 2]> {
        if chain.len() < 2 {
            return chain;
        }
        let mut result: Vec<[f32; 2]> = vec![chain[0]];
        for pt in chain.into_iter().skip(1) {
            let prev = *result.last().unwrap();
            let du = pt[0] - prev[0];
            let dv = pt[1] - prev[1];
            if du.abs() > 1e-5 && dv.abs() > 1e-5 {
                // Insert an axis-aligned corner to keep pure H/V edges.
                // For left chain (going down image): 
                //   - if stepping right (du>0, narrowing), vertical first then horiz
                //   - if stepping left (du<0, widening), horiz first then vertical
                // For right chain we apply similar logic (direction is same top->bottom).
                if du > 0.0 {
                    result.push([prev[0], pt[1]]);
                } else {
                    result.push([pt[0], prev[1]]);
                }
                result.push(pt);
            } else {
                result.push(pt);
            }
        }
        result
    }

    let mut left_chain = rectify_to_axis_aligned(left_chain);
    let mut right_chain = rectify_to_axis_aligned(right_chain);

    // Collapse top/bottom to mid only for narrow tips (point_top/point_bot).
    // Wide ends keep full left/right at the end row so the silhouette follows the PNG's actual end contours.
    // For wide ends we apply strong UV inset (v + lateral) + geom pull on the end bar verts to ensure
    // the perimeter edge samples deep inner green (no black line or rim). Full thickness everywhere.
    if !left_chain.is_empty() && !right_chain.is_empty() {
        if point_top {
            let top_v = left_chain[0][1];
            left_chain[0] = [top_mid_u, top_v];
            right_chain[0] = [top_mid_u, top_v];
        }
        if point_bot {
            let last = left_chain.len() - 1;
            let bot_v = left_chain[last][1];
            left_chain[last] = [bot_mid_u, bot_v];
            right_chain[last] = [bot_mid_u, bot_v];
        }
    }
    let left_chain = rectify_to_axis_aligned(left_chain);
    let right_chain = rectify_to_axis_aligned(right_chain);

    // Build unique boundary points from left and right chains (sharing tip points if pointed).
    // Also record indices into this boundary list for left and right (top to bottom).
    let mut boundary: Vec<[f32; 2]> = vec![];
    let mut left_idx: Vec<usize> = vec![];
    let mut right_idx: Vec<usize> = vec![];

    fn get_or_add(b: &mut Vec<[f32; 2]>, p: [f32; 2]) -> usize {
        if let Some(i) = b.iter().position(|&q| (q[0] - p[0]).abs() < 1e-5 && (q[1] - p[1]).abs() < 1e-5) {
            i
        } else {
            let i = b.len();
            b.push(p);
            i
        }
    }

    for &p in &left_chain {
        left_idx.push(get_or_add(&mut boundary, p));
    }
    for &p in &right_chain {
        right_idx.push(get_or_add(&mut boundary, p));
    }

    // Build the closed perimeter order (for side walls): left (top->bottom) + rev(right) (bottom->top)
    let mut perim_order: Vec<usize> = left_idx.clone();
    let mut rev_r: Vec<usize> = right_idx.clone();
    rev_r.reverse();
    perim_order.extend(rev_r);

    // Clean duplicates at closure and consecutive (pointed tips)
    if perim_order.len() > 1 && perim_order[0] == perim_order[perim_order.len() - 1] {
        perim_order.pop();
    }
    let mut i = 0usize;
    while i + 1 < perim_order.len() {
        if perim_order[i] == perim_order[i + 1] {
            perim_order.remove(i + 1);
        } else {
            i += 1;
        }
    }

    // Map from boundary index (in left_idx/right_idx) to position in perim_order (so cap_tris use correct 0..n-1 mesh indices)
    let mut boundary_to_perim: Vec<usize> = vec![0; boundary.len()];
    for (pos, &bidx) in perim_order.iter().enumerate() {
        boundary_to_perim[bidx] = pos;
    }

    // Build cap triangulation as a strip between left and right chains (local triangles, better texture fidelity than one big fan from center).
    // This avoids large spanning triangles at top/bottom that cause visible lines or warping.
    let mut cap_tris: Vec<usize> = vec![];
    let mut li = 0usize;
    let mut ri = 0usize;
    while li < left_idx.len() - 1 || ri < right_idx.len() - 1 {
        let left_next_v = if li + 1 < left_idx.len() { left_chain[li + 1][1] } else { f32::INFINITY };
        let right_next_v = if ri + 1 < right_idx.len() { right_chain[ri + 1][1] } else { f32::INFINITY };
        if li < left_idx.len() - 1 && left_next_v <= right_next_v {
            let a = boundary_to_perim[ left_idx[li] ];
            let b = boundary_to_perim[ right_idx[ri] ];
            let c = boundary_to_perim[ left_idx[li + 1] ];
            if a != b && a != c && b != c {
                cap_tris.push(a);
                cap_tris.push(b);
                cap_tris.push(c);
            }
            li += 1;
        } else if ri < right_idx.len() - 1 {
            let a = boundary_to_perim[ left_idx[li] ];
            let b = boundary_to_perim[ right_idx[ri] ];
            let c = boundary_to_perim[ right_idx[ri + 1] ];
            if a != b && a != c && b != c {
                cap_tris.push(a);
                cap_tris.push(b);
                cap_tris.push(c);
            }
            ri += 1;
        }
    }

    // Now build ordered perimeter geometry + UVs from perim_order (boundary indices map 1:1 to 0..n-1)
    let mut outline_2d: Vec<[f32; 2]> = vec![];
    let mut perim_uv: Vec<[f32; 2]> = vec![];
    let mut y_fronts: Vec<f32> = vec![];
    let mut y_backs: Vec<f32> = vec![];
    let mut orig_vs: Vec<f32> = vec![];
    for &bidx in &perim_order {
        let [u, v] = boundary[bidx];
        let orig_v = v;
        let is_top = (orig_v - min_v).abs() < 1e-4;
        let is_bot = (orig_v - max_v).abs() < 1e-4;
        // Full thickness everywhere (including wide end bars) for uniform "coffee coaster" look.
        // "No rim/line on silhouette" is achieved via UV inset (edge samples inner green) + nearest filter
        // + geom pull on end bars. Side walls are flat green (side_uv), which is "just the green".
        let yf = 0.011;
        let yb = -0.011;
        // For position: slightly inset top and bottom points inward in x (narrower) and z (shorter)
        // to cut the flat top and base lines.
        let mut calc_u = u;
        let mut calc_v = v;
        if is_top && point_top {
            calc_u = top_mid_u;
            calc_v = orig_v + 0.02;
        }
        if is_bot && point_bot {
            calc_u = bot_mid_u;
            calc_v = orig_v - 0.01;
        }
        // For wide ends (not pointing), still slightly pull the end bar's L/R points inward
        // in model space (narrows the extreme top/bot bar a tad) to help eliminate flat line look.
        if is_top && !point_top {
            let du = center_u - u;
            calc_u = u + du * 0.025;
            calc_v = orig_v + 0.005;
        }
        if is_bot && !point_bot {
            let du = center_u - u;
            calc_u = u + du * 0.025;
            calc_v = orig_v - 0.005;
        }
        let x = (calc_u - center_u) / span_u * desired_w;
        // Negated (was `center_v - calc_v`) so the leaf isn't upside down: the
        // sprite's top (v small) maps to −Z instead of +Z. UVs ride with each
        // vertex and the side normals are derived from this flipped outline, so
        // it stays consistent; double_sided covers the reflected winding.
        let z = (calc_v - center_v) / span_v * desired_h;
        outline_2d.push([x, z]);
        // For UV: inset toward center. With nearest sampling (ImagePlugin::default_nearest) a small inset
        // (5px sides) ensures the silhouette edge samples solid green (the art uses (21,255,0) right to the
        // green-filtered boundary). Wide end bars get large v inset (20px top /12px bot) + 10px lateral u inset
        // so the top/bottom perimeter edges are deep inner green. Geom pull on end bars + full thickness helps
        // avoid flat/rim artifacts. Keeps art faithful overall.
        let pu;
        let pv;
        if is_top {
            pv = orig_v + (if point_top { 6.0 } else { 20.0 }) / h as f32;
            let mut puu = u;
            if !point_top {
                // additionally inset u toward center for the wide top bar verts, to clear black details near the upper sides/corners
                // Use fixed pixel shift (not scaled by distance to center)
                let du = center_u - u;
                let len = du.abs().max(1e-6);
                puu = (u + (du / len) * (10.0 / w as f32)).clamp(0.0, 1.0);
            }
            pu = if point_top { top_mid_u } else { puu };
        } else if is_bot {
            pv = orig_v - (if point_bot { 5.0 } else { 12.0 }) / h as f32;
            let mut puu = u;
            if !point_bot {
                let du = center_u - u;
                let len = du.abs().max(1e-6);
                puu = (u + (du / len) * (10.0 / w as f32)).clamp(0.0, 1.0);
            }
            pu = if point_bot { bot_mid_u } else { puu };
        } else {
            let du = center_u - u;
            let dv = center_v - v;
            let len = (du * du + dv * dv).sqrt().max(1e-6);
            let inset = 5.0 / h as f32;
            pu = (u + du / len * inset).clamp(0.0, 1.0);
            pv = (v + dv / len * inset).clamp(0.0, 1.0);
        }
        perim_uv.push([pu, pv]);
        y_fronts.push(yf);
        y_backs.push(yb);
        orig_vs.push(orig_v);
    }
    let n = perim_order.len();

    // Precompute outward side normals (X/Z plane). We negate because the trace
    // order from the row walk ends up CW when viewed from +Y.
    let mut side_normals: Vec<[f32; 3]> = Vec::with_capacity(n);
    for i in 0..n {
        let [x0, z0] = outline_2d[i];
        let [x1, z1] = outline_2d[(i + 1) % n];
        let dx = x1 - x0;
        let dz = z1 - z0;
        let len = (dx * dx + dz * dz).sqrt().max(0.0001);
        let nx = dz / len;
        let nz = -dx / len;
        side_normals.push([-nx, 0.0, -nz]);
    }

    let mut positions: Vec<[f32; 3]> = Vec::new();
    let mut normals: Vec<[f32; 3]> = Vec::new();
    let mut uvs: Vec<[f32; 2]> = Vec::new();

    // Front perimeter (cap) — use the *real* perim_uv from the PNG so texturing
    // matches the original sprite exactly on the 3D surface.
    for (i, &[x, z]) in outline_2d.iter().enumerate() {
        let orig_v = orig_vs[i];
        let mut zz = z;
        let is_top = (orig_v - min_v).abs() < 1e-4;
        let is_bot = (orig_v - max_v).abs() < 1e-4;
        if is_top {
            zz -= if point_top { 0.04 } else { 0.04 };
        }
        if is_bot {
            zz += if point_bot { 0.04 } else { 0.04 };
        }
        positions.push([x, y_fronts[i], zz]);
        normals.push([0.0, 1.0, 0.0]);
        uvs.push(perim_uv[i]);
    }

    // Back perimeter
    let back_perim_start = positions.len() as u32;
    for (i, &[x, z]) in outline_2d.iter().enumerate() {
        let orig_v = orig_vs[i];
        let mut zz = z;
        let is_top = (orig_v - min_v).abs() < 1e-4;
        let is_bot = (orig_v - max_v).abs() < 1e-4;
        if is_top {
            zz -= if point_top { 0.015 } else { 0.015 };
        }
        if is_bot {
            zz += if point_bot { 0.015 } else { 0.015 };
        }
        positions.push([x, y_backs[i], zz]);
        normals.push([0.0, -1.0, 0.0]);
        uvs.push(perim_uv[i]);
    }

    // Side wall verts (duplicated for hard 90° edges + correct normals)
    let side_top_start = positions.len() as u32;
    // Use a guaranteed green pixel UV for the rim (from the first green pixel found).
    // This ensures the extruded sides are "just the green", not black border.
    let side_uv = [good_u, good_v];
    for (i, &[x, z]) in outline_2d.iter().enumerate() {
        let y_top = y_fronts[i];
        positions.push([x, y_top, z]);
        normals.push(side_normals[i]);
        uvs.push(side_uv);
    }
    let side_bot_start = positions.len() as u32;
    for (i, &[x, z]) in outline_2d.iter().enumerate() {
        let y_bot = y_backs[i];
        positions.push([x, y_bot, z]);
        normals.push(side_normals[i]);
        uvs.push(side_uv);
    }

    let mut indices: Vec<u32> = Vec::new();

    // Front cap: strip triangulation between left and right chains (local tris for faithful texture).
    for t in 0..cap_tris.len() / 3 {
        let a = cap_tris[t * 3] as u32;
        let b = cap_tris[t * 3 + 1] as u32;
        let c = cap_tris[t * 3 + 2] as u32;
        indices.push(a);
        indices.push(b);
        indices.push(c);
    }

    // Back cap: same strip but reversed winding, offset to back perim verts.
    for t in 0..cap_tris.len() / 3 {
        let a = back_perim_start as u32 + cap_tris[t * 3] as u32;
        let b = back_perim_start as u32 + cap_tris[t * 3 + 1] as u32;
        let c = back_perim_start as u32 + cap_tris[t * 3 + 2] as u32;
        indices.push(a);
        indices.push(c);
        indices.push(b);
    }

    // Side wall quads
    for i in 0..n {
        let f0 = side_top_start + (i as u32);
        let f1 = side_top_start + (((i + 1) % n) as u32);
        let b0 = side_bot_start + (i as u32);
        let b1 = side_bot_start + (((i + 1) % n) as u32);

        indices.push(f0);
        indices.push(f1);
        indices.push(b1);
        indices.push(f0);
        indices.push(b1);
        indices.push(b0);
    }

    let mut mesh = Mesh::new(
        PrimitiveTopology::TriangleList,
        RenderAssetUsages::default(),
    );
    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, positions);
    mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, normals);
    mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, uvs);
    mesh.insert_indices(Indices::U32(indices));

    meshes.add(mesh)
}
