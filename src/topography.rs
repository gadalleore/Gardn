use crate::australia::{biome_at_world, is_land, AussieBiome};
use crate::world::{
    world_to_continental, world_to_geo, MAX_SURFACE_VOXEL_Y, VOXELS_PER_FOOT, VOXEL_SIZE,
    WORLD_SEED,
};

/// Deterministic per-biome surface heights. Sampled in continent-space feet so
/// the same square of Australia always has the same landforms regardless of
/// where a given run's spawn offset placed world origin.
///
/// Two scales stack:
/// - **Regional forms** (hundreds of feet): dunes in the red centre, mesas in
///   the Pilbara, ridge-and-valley grain in the SE and Tasmania.
/// - **Micro-relief** (5–25 ft): the terrain a worm actually lives in — mounds,
///   hummocks, and rocky ribs a foot or three tall, i.e. mountains at worm scale.
///
/// All amplitudes are authored in feet and converted to voxels at the end, so
/// changing the voxel size never flattens the world.
pub fn surface_height_voxels(world_x: f32, world_z: f32) -> i32 {
    let biome = biome_at_world(world_x, world_z);
    if biome == AussieBiome::Ocean {
        return 0;
    }

    let c = world_to_continental(world_x, world_z);
    let x = c.x;
    let z = c.y;

    // Regional landforms, in feet.
    let regional_ft = match biome {
        AussieBiome::Ocean => 0.0,
        // Top End: near-flat floodplains broken by abrupt sandstone escarpments
        // (Arnhem Land "stone country" plateaus).
        AussieBiome::TropicalSavanna => {
            let plain = 0.7 + fbm(x / 900.0, z / 900.0, seed(1), 3) * 1.0;
            let plateau_t = fbm01(x / 2600.0, z / 2600.0, seed(2), 3);
            plain + smoothstep(0.55, 0.66, plateau_t) * 3.7
        }
        // Longitudinal dune fields: parallel NNW–SSE ridges with wide swales,
        // amplitude wandering so some corridors are dune-free gibber plain.
        AussieBiome::AridOutback => {
            let across = x * 0.38 + z * 0.92;
            let phase_wiggle = fbm(x / 2000.0, z / 2000.0, seed(3), 2) * 1.5;
            let wave = (across / 1400.0 * std::f32::consts::TAU + phase_wiggle).sin();
            let crest = ((wave + 1.0) * 0.5).powf(2.0);
            let dune_amp = 1.7 * (0.5 + 0.5 * fbm01(x / 3200.0, z / 3200.0, seed(4), 2));
            0.5 + crest * dune_amp + fbm(x / 700.0, z / 700.0, seed(5), 2) * 0.5
        }
        // Iron country: rolling spinifex base with flat-topped mesas and
        // rocky ridge lines.
        AussieBiome::Pilbara => {
            let base = 0.7 + fbm(x / 800.0, z / 800.0, seed(6), 3) * 1.0;
            let mesa_t = fbm01(x / 2200.0, z / 2200.0, seed(7), 3);
            let mesa = smoothstep(0.52, 0.60, mesa_t) * 5.3;
            base + mesa + ridged(x / 500.0, z / 500.0, seed(8), 2) * 1.0
        }
        // Gently undulating jarrah/karri plateau of the SW.
        AussieBiome::Mediterranean => 0.8 + fbm(x / 850.0, z / 850.0, seed(9), 3) * 1.7,
        // Great Dividing Range foothills: long ridge-and-valley grain.
        AussieBiome::TemperateForest => {
            1.0 + ridged(x / 1800.0, z / 1800.0, seed(10), 3) * 6.0
                + fbm(x / 420.0, z / 420.0, seed(11), 2) * 1.3
        }
        // Littoral strip: rolling hills and old dune ridges.
        AussieBiome::CoastalBush => 0.7 + fbm(x / 650.0, z / 650.0, seed(12), 3) * 2.0,
        // Rugged cool-temperate highlands.
        AussieBiome::Tasmania => {
            1.0 + ridged(x / 1500.0, z / 1500.0, seed(13), 3) * 7.3
                + fbm(x / 300.0, z / 300.0, seed(14), 2) * 1.7
        }
    };

    // Worm-mountain micro-relief, in feet: (amplitude, rocky ridges?). These
    // read as serious mountain ranges from 1.5 inches off the ground — a 2 ft
    // mound is ~16 worm-lengths tall.
    let (micro_amp_ft, rocky) = match biome {
        AussieBiome::Ocean => (0.0, false),
        AussieBiome::TropicalSavanna => (2.5, false), // cracked floodplain hummocks
        AussieBiome::AridOutback => (3.5, false),     // sand ripples and gibber mounds
        AussieBiome::Pilbara => (5.0, true),          // ironstone rubble and ribs
        AussieBiome::Mediterranean => (3.0, false),   // laterite gravel rises
        AussieBiome::TemperateForest => (4.5, false), // root mounds and gully lips
        AussieBiome::CoastalBush => (3.5, false),     // hind-dune hummocks
        AussieBiome::Tasmania => (6.0, true),         // boulder and button-grass mounds
    };
    let micro_ft = if rocky {
        (ridged(x / 24.0, z / 24.0, seed(20), 3) - 0.35) * 2.0 * micro_amp_ft
    } else {
        // fbm concentrates around 0 — stretch it so full-amplitude mounds
        // actually occur in the landscape, then soft-clip the extremes.
        (fbm(x / 24.0, z / 24.0, seed(21), 3) * 1.8).clamp(-1.0, 1.0) * micro_amp_ft
    };
    // Rolling hills, in feet — the visible "up and down" of the landscape.
    // 300-ish ft wavelengths put several full rises and falls inside the far
    // horizon (~640 ft), so distant ground steps dramatically instead of
    // reading as a plain. fbm01 keeps it additive: hills rise from the plains
    // rather than digging swamps below sea level.
    let hills_amp_ft = match biome {
        AussieBiome::Ocean => 0.0,
        AussieBiome::TropicalSavanna => 5.0,
        AussieBiome::AridOutback => 6.0,
        AussieBiome::Pilbara => 9.0,
        AussieBiome::Mediterranean => 7.0,
        AussieBiome::TemperateForest => 11.0,
        AussieBiome::CoastalBush => 7.0,
        AussieBiome::Tasmania => 13.0,
    };
    let hills_ft = fbm01(x / 320.0, z / 320.0, seed(30), 3) * hills_amp_ft;

    // Mountain ranges: long-wavelength ridged peaks. The subtraction keeps the
    // plains flat while the ridge lines climb toward the 80 ft ceiling —
    // Minecraft-grade relief, worm-graded.
    let mountain_amp_ft = match biome {
        AussieBiome::Ocean => 0.0,
        AussieBiome::TropicalSavanna => 24.0,
        AussieBiome::AridOutback => 28.0,
        AussieBiome::Pilbara => 55.0,
        AussieBiome::Mediterranean => 30.0,
        AussieBiome::TemperateForest => 70.0,
        AussieBiome::CoastalBush => 24.0,
        AussieBiome::Tasmania => 80.0,
    };
    let mountains_ft =
        (ridged(x / 900.0, z / 900.0, seed(32), 3) - 0.22).max(0.0) * mountain_amp_ft * 1.28;

    // Worm-mountain mounds: ~95 ft wavelength, so the streamed neighbourhood
    // itself always holds a full rise and fall. To a 3-inch worm these 2–6 ft
    // molehills ARE mountains — a 6 ft climb is ~25 worm-lengths of ascent.
    let mounds_ft = fbm(x / 95.0, z / 95.0, seed(31), 3) * (hills_amp_ft * 0.45);

    // Inch-scale roughness everywhere — soil is never billiard-flat to a worm.
    let fine_ft = fbm(x / 3.5, z / 3.5, seed(22), 2) * 0.2;

    // Regional forms doubled: the originals were authored against a 16 ft
    // ceiling; with 80 ft of headroom the escarpments and ranges can loom.
    let h_ft = (regional_ft * 2.0 + mountains_ft + hills_ft + mounds_ft + micro_ft + fine_ft)
        * coast_openness(world_x, world_z);
    ((h_ft * VOXELS_PER_FOOT as f32).round() as i32).clamp(1, MAX_SURFACE_VOXEL_Y)
}

/// World-space Y of the top face of the surface voxel at (x, z).
pub fn surface_top_world_y(world_x: f32, world_z: f32) -> f32 {
    (surface_height_voxels(world_x, world_z) + 1) as f32 * VOXEL_SIZE
}

/// Fade relief toward sea level near the coastline so beaches slope into the
/// water instead of ending in a cliff. Samples the land mask ~1500 ft out in
/// four directions; fully inland columns are unaffected.
fn coast_openness(world_x: f32, world_z: f32) -> f32 {
    const R: f32 = 1500.0;
    let mut land = 0;
    for (dx, dz) in [(R, 0.0), (-R, 0.0), (0.0, R), (0.0, -R)] {
        let (lat, lon) = world_to_geo(world_x + dx, world_z + dz);
        if is_land(lat, lon) {
            land += 1;
        }
    }
    0.2 + 0.8 * (land as f32 / 4.0)
}

/// The cave web, decided on a world-aligned 4-voxel (8-inch) lattice so
/// tunnels run seamlessly across chunk borders and the gravity floor can ask
/// the exact same question the chunk generator answered. Takes WORLD voxel
/// coords plus the column's surface voxel; quantisation happens inside.
///
/// Shape is Minecraft-style: two 3D noises pinching near zero make spaghetti
/// tunnels (wider with depth); a third opens caverns down deep. A thin surface
/// skin keeps the ground solid except where a strong tunnel core punches a
/// natural entrance.
pub const CAVE_CELL_VOXELS: i32 = 4;

pub fn is_cave_cell(world_vx: i32, world_vy: i32, world_vz: i32, surface_vy: i32) -> bool {
    let noise = cave_cell_noise(world_vx, world_vy, world_vz);
    let cy = world_vy.div_euclid(CAVE_CELL_VOXELS) * CAVE_CELL_VOXELS + CAVE_CELL_VOXELS / 2;
    cave_from_noise(&noise, surface_vy - cy)
}

// ---- Geology: how deep the dirt goes ----------------------------------------
//
// Owner spec: "Dirt should descend a tree length down, but should vary in
// depth. A distribution. Most likely ... an average tree length's of dirt
// downward, but there is a 1% chance that it is bedrock right below you and
// 1% chance that the dirt descends two treelengths."
//
// The yardstick is the SMALLEST tree class in trees.rs (mallee / desert oak,
// 30–55 ft): 30 ft. Measuring by a river red gum (550–820 ft) would demand a
// 300-ft-deep diggable world; 30 ft already deepens chunks ~4× (measured:
// chunk mesh ~100 ms → ~400 ms, all off the main thread).

/// One tree length, in ground voxels — the unit the dirt-depth field speaks.
pub(crate) const TREE_LENGTH_VOXELS: i32 = 30 * VOXELS_PER_FOOT;

/// How far below its own surface a land column stays diggable: two tree
/// lengths of possible dirt, a 12-ft rock band (so the deepest dirt still has
/// worm-highway rock underneath), then two voxels of bedrock floor.
/// worm.rs collision treats everything deeper as solid bedrock — the two
/// MUST stay in lockstep (worm.rs references this constant).
pub(crate) const DIGGABLE_DEPTH_VOXELS: i32 =
    2 * TREE_LENGTH_VOXELS + 12 * VOXELS_PER_FOOT + 2;

/// Smooth "how deep is the dirt here" field, in voxels. Continental-space so
/// it's seamless across chunks and pinned to the real map like the heights.
///
/// 260-ft base wavelength (finest octave ~65 ft): neighbouring dig sites feel
/// geologically related, not per-column dice rolls. Measured percentiles of
/// the underlying noise (1M samples): P1 = 0.196, P50 = 0.500, P99 = 0.803.
/// The linear map sends P1→0 (bedrock right below you), P50→one tree length,
/// P99→two tree lengths; the clamp pins both 1% tails exactly as spec'd.
pub(crate) fn dirt_depth_voxels(world_x: f32, world_z: f32) -> i32 {
    let c = world_to_continental(world_x, world_z);
    let n = fbm01(c.x / 260.0, c.y / 260.0, seed(50), 3);
    let frac = ((n - 0.196) / (0.803 - 0.196) * 2.0).clamp(0.0, 2.0);
    (frac * TREE_LENGTH_VOXELS as f32).round() as i32
}

/// Dirt tunnels — the worm highways threading the rock band. Same
/// pinched-noise machinery as the caves but on its own seeds, wider cores,
/// and flatter (stretched y) so they read as wandering horizontal passages.
/// Crucially these are SOLID DIRT, not air: they cost zero mesh faces, the
/// collision map doesn't change, and the worm discovers them by chewing —
/// hit one while digging through rock and you can follow it for dozens of
/// feet. Decided per cave-lattice cell in world space, seamless across
/// chunks. Width 0.16 measured ≈ 8–12% of rock-band volume.
pub(crate) fn is_dirt_tunnel_cell(world_vx: i32, world_vy: i32, world_vz: i32) -> bool {
    let q = |v: i32| v.div_euclid(CAVE_CELL_VOXELS) * CAVE_CELL_VOXELS + CAVE_CELL_VOXELS / 2;
    let (cx, cy, cz) = (q(world_vx), q(world_vy), q(world_vz));
    let c = world_to_continental(cx as f32 * VOXEL_SIZE, cz as f32 * VOXEL_SIZE);
    let y_ft = cy as f32 * VOXEL_SIZE;
    let n1 = fbm3(c.x / 14.0, y_ft / 6.0, c.y / 14.0, seed(45), 2);
    let n2 = fbm3(c.x / 18.0, y_ft / 7.5, c.y / 18.0, seed(46), 2);
    n1.abs().max(n2.abs()) < 0.16
}

/// One procedural rock clump. Coordinates are WORLD voxels; the ellipsoid is
/// stamped into any chunk it overlaps, so clumps cross chunk borders
/// seamlessly (everything derives from a world-space lattice hash).
pub(crate) struct Boulder {
    pub(crate) center: bevy::math::IVec3,
    pub(crate) radius: bevy::math::IVec3,
}

/// Boulders whose ellipsoids could overlap the given chunk. Two tiers on
/// jittered world lattices: sparse GIANTS (2–8 ft, ~1 per couple of chunks,
/// a third of them surfacing as visible rocks in the landscape) and common
/// small rocks souped invisibly through the dirt (worm-scale obstacles —
/// they refuse the bite, so digs detour around them). Ocean cells spawn
/// nothing.
pub(crate) fn boulders_near_chunk(chunk_coord: bevy::math::IVec2) -> Vec<Boulder> {
    use crate::world::{chunk_world_origin, GardenRng, CHUNK_VOXELS, WORLD_SEED};
    use bevy::math::{IVec2, IVec3};

    let origin = chunk_world_origin(chunk_coord);
    // (lattice cell size in voxels, max radius, presence, surfacing share,
    //  radius range ft, tier salt)
    const TIERS: [(i32, f32, f32, (f32, f32), u64); 2] = [
        (96, 0.22, 0.33, (2.0, 8.0), 0xB0_71DE55),
        (32, 0.18, 0.08, (0.8, 2.2), 0x570_5E5),
    ];

    let base_vx = chunk_coord.x * CHUNK_VOXELS;
    let base_vz = chunk_coord.y * CHUNK_VOXELS;
    let mut out = Vec::new();

    for (cell, presence, surfacing, (r_lo, r_hi), salt) in TIERS {
        let max_r = (r_hi * VOXELS_PER_FOOT as f32).ceil() as i32;
        let c0 = IVec2::new(
            (base_vx - max_r).div_euclid(cell),
            (base_vz - max_r).div_euclid(cell),
        );
        let c1 = IVec2::new(
            (base_vx + CHUNK_VOXELS + max_r).div_euclid(cell),
            (base_vz + CHUNK_VOXELS + max_r).div_euclid(cell),
        );
        for cz in c0.y..=c1.y {
            for cx in c0.x..=c1.x {
                let mut rng = GardenRng::new(
                    WORLD_SEED
                        ^ salt
                        ^ (cx as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15)
                        ^ (cz as u64).wrapping_mul(0xC2B2_AE3D_27D4_EB4F),
                );
                if !rng.chance(presence) {
                    continue;
                }
                let vx = cx * cell + (rng.next_f32() * cell as f32) as i32;
                let vz = cz * cell + (rng.next_f32() * cell as f32) as i32;
                let wx = vx as f32 * VOXEL_SIZE;
                let wz = vz as f32 * VOXEL_SIZE;
                if biome_at_world(wx, wz) == AussieBiome::Ocean {
                    continue;
                }

                let rx = (rng.range(r_lo, r_hi) * VOXELS_PER_FOOT as f32) as i32;
                let rz = (rng.range(r_lo, r_hi) * VOXELS_PER_FOOT as f32) as i32;
                let ry = ((rx + rz) as f32 * rng.range(0.30, 0.45)) as i32;
                let surf = surface_height_voxels(wx, wz);
                let vy = if rng.chance(surfacing) {
                    // Poking out of the ground: about two-thirds of the dome
                    // shows — a visible giant rock in the landscape.
                    surf - ry / 3
                } else {
                    // Buried in the dirt band (or the rock below, where the
                    // dirt runs shallow — invisible there, harmless).
                    let dirt = dirt_depth_voxels(wx, wz).max(8);
                    surf - ((0.25 + 0.6 * rng.next_f32()) * dirt as f32) as i32 - ry / 4
                };
                out.push(Boulder {
                    center: IVec3::new(vx, vy, vz),
                    radius: IVec3::new(rx.max(2), ry.max(2), rz.max(2)),
                });
            }
        }
    }
    out
}

/// The noise trio for one cave lattice cell, computed once and reused by every
/// column the cell covers. Generation carves from this + each column's own
/// surface, so the chunk builder and the worm's collision probe answer the
/// SAME question from the SAME inputs — any drift between them puts phantom
/// air inside visibly solid ground (the worm clips in) or invisible floors
/// over visible caves. A zero-mismatch test in terrain.rs pins the agreement.
pub(crate) struct CaveCellNoise {
    tunnel: f32,
    cavern: f32,
}

pub(crate) fn cave_cell_noise(world_vx: i32, world_vy: i32, world_vz: i32) -> CaveCellNoise {
    let q = |v: i32| v.div_euclid(CAVE_CELL_VOXELS) * CAVE_CELL_VOXELS + CAVE_CELL_VOXELS / 2;
    let (cx, cy, cz) = (q(world_vx), q(world_vy), q(world_vz));

    let c = world_to_continental(cx as f32 * VOXEL_SIZE, cz as f32 * VOXEL_SIZE);
    let y_ft = cy as f32 * VOXEL_SIZE;

    // Tunnels: two noises pinching near zero; 1–2 ft wide near the skin.
    let n1 = fbm3(c.x / 7.0, y_ft / 4.5, c.y / 7.0, seed(40), 2);
    let n2 = fbm3(c.x / 9.0, y_ft / 5.5, c.y / 9.0, seed(41), 2);
    CaveCellNoise {
        tunnel: n1.abs().max(n2.abs()),
        cavern: fbm3(c.x / 16.0, y_ft / 8.0, c.y / 16.0, seed(42), 2),
    }
}

/// Depth-dependent half of the cave decision (see [`cave_cell_noise`]).
/// `depth_vox` is the column's surface voxel minus the CELL-CENTRE y.
pub(crate) fn cave_from_noise(noise: &CaveCellNoise, depth_vox: i32) -> bool {
    if depth_vox < 0 {
        return false;
    }
    // Tunnels open up with depth; a thin surface skin keeps the ground solid
    // except where a strong tunnel core punches a natural entrance.
    let mut width = 0.085 + 0.05 * (depth_vox as f32 / 120.0).min(1.0);
    if depth_vox < 8 {
        width *= 0.35;
    }
    if noise.tunnel < width {
        return true;
    }
    // Caverns in the deep dark.
    depth_vox > 40 && noise.cavern > 0.62
}

/// True when a cave cell's noise can't open a cave at ANY depth — lets the
/// chunk builder skip a whole 4×4-column block without per-column tests.
pub(crate) fn cave_never_opens(noise: &CaveCellNoise) -> bool {
    noise.tunnel >= 0.135 && noise.cavern <= 0.62
}

fn seed(n: u32) -> u32 {
    (WORLD_SEED as u32).wrapping_mul(0x9E37_79B9) ^ n.wrapping_mul(0x85EB_CA6B)
}

fn lattice_hash3(ix: i32, iy: i32, iz: i32, seed: u32) -> f32 {
    let mut h = (ix as u32).wrapping_mul(0x8DA6_B343)
        ^ (iy as u32).wrapping_mul(0xD816_3841)
        ^ (iz as u32).wrapping_mul(0xCB1A_B31F)
        ^ seed.wrapping_mul(0x9E37_79B9);
    h ^= h >> 13;
    h = h.wrapping_mul(0x7FEB_352D);
    h ^= h >> 15;
    h as f32 / u32::MAX as f32
}

/// Smoothly interpolated 3D lattice value noise in [0, 1].
fn value_noise3(x: f32, y: f32, z: f32, seed: u32) -> f32 {
    let (x0, y0, z0) = (x.floor(), y.floor(), z.floor());
    let (tx, ty, tz) = (smooth(x - x0), smooth(y - y0), smooth(z - z0));
    let (ix, iy, iz) = (x0 as i32, y0 as i32, z0 as i32);

    let mut corners = [0f32; 8];
    for (k, corner) in corners.iter_mut().enumerate() {
        let (dx, dy, dz) = ((k & 1) as i32, ((k >> 1) & 1) as i32, ((k >> 2) & 1) as i32);
        *corner = lattice_hash3(ix + dx, iy + dy, iz + dz, seed);
    }
    let lerp = |a: f32, b: f32, t: f32| a + (b - a) * t;
    let xy0 = lerp(lerp(corners[0], corners[1], tx), lerp(corners[2], corners[3], tx), ty);
    let xy1 = lerp(lerp(corners[4], corners[5], tx), lerp(corners[6], corners[7], tx), ty);
    lerp(xy0, xy1, tz)
}

/// Fractal 3D value noise in [-1, 1].
fn fbm3(x: f32, y: f32, z: f32, seed: u32, octaves: u32) -> f32 {
    let mut sum = 0.0;
    let mut amp = 1.0;
    let mut norm = 0.0;
    let (mut fx, mut fy, mut fz) = (x, y, z);
    for o in 0..octaves {
        sum += value_noise3(fx, fy, fz, seed.wrapping_add(o * 97)) * amp;
        norm += amp;
        amp *= 0.5;
        fx *= 2.0;
        fy *= 2.0;
        fz *= 2.0;
    }
    (sum / norm) * 2.0 - 1.0
}

fn lattice_hash(ix: i32, iz: i32, seed: u32) -> f32 {
    let mut h = (ix as u32).wrapping_mul(0x8DA6_B343)
        ^ (iz as u32).wrapping_mul(0xD816_3841)
        ^ seed.wrapping_mul(0xCB1A_B31F);
    h ^= h >> 13;
    h = h.wrapping_mul(0x7FEB_352D);
    h ^= h >> 15;
    h as f32 / u32::MAX as f32
}

fn smooth(t: f32) -> f32 {
    t * t * (3.0 - 2.0 * t)
}

/// Smoothly interpolated lattice value noise in [0, 1].
fn value_noise(x: f32, z: f32, seed: u32) -> f32 {
    let x0 = x.floor();
    let z0 = z.floor();
    let tx = smooth(x - x0);
    let tz = smooth(z - z0);
    let ix = x0 as i32;
    let iz = z0 as i32;
    let a = lattice_hash(ix, iz, seed);
    let b = lattice_hash(ix + 1, iz, seed);
    let c = lattice_hash(ix, iz + 1, seed);
    let d = lattice_hash(ix + 1, iz + 1, seed);
    a + (b - a) * tx + (c - a) * tz + (a - b - c + d) * tx * tz
}

/// Fractal value noise in [0, 1].
fn fbm01(x: f32, z: f32, seed: u32, octaves: u32) -> f32 {
    let mut sum = 0.0;
    let mut amp = 1.0;
    let mut norm = 0.0;
    let mut fx = x;
    let mut fz = z;
    for o in 0..octaves {
        sum += value_noise(fx, fz, seed.wrapping_add(o * 101)) * amp;
        norm += amp;
        amp *= 0.5;
        fx *= 2.0;
        fz *= 2.0;
    }
    sum / norm
}

/// Fractal value noise in [-1, 1].
fn fbm(x: f32, z: f32, seed: u32, octaves: u32) -> f32 {
    fbm01(x, z, seed, octaves) * 2.0 - 1.0
}

/// Ridged fractal noise in [0, 1] — sharp crests, broad valleys.
fn ridged(x: f32, z: f32, seed: u32, octaves: u32) -> f32 {
    let mut sum = 0.0;
    let mut amp = 1.0;
    let mut norm = 0.0;
    let mut fx = x;
    let mut fz = z;
    for o in 0..octaves {
        let n = value_noise(fx, fz, seed.wrapping_add(o * 131));
        sum += (1.0 - (n * 2.0 - 1.0).abs()) * amp;
        norm += amp;
        amp *= 0.5;
        fx *= 2.0;
        fz *= 2.0;
    }
    let r = sum / norm;
    r * r
}

fn smoothstep(edge0: f32, edge1: f32, x: f32) -> f32 {
    let t = ((x - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::world::geo_to_world_offset;

    /// Every biome's height program stays inside the legal band, and ocean is
    /// flat sea level. Uses the unshifted geo mapping (no spawn offset in tests)
    /// so a lat/lon converts straight to world coordinates.
    #[test]
    fn heights_stay_in_band_across_all_biomes() {
        let spots = [
            ("savanna", -13.5, 132.0),
            ("outback", -24.5, 133.5),
            ("pilbara", -22.5, 118.5),
            ("mediterranean", -30.0, 119.0),
            ("temperate", -35.0, 146.0),
            ("coastal", -26.0, 152.5),
            ("tasmania", -42.0, 146.8),
        ];
        for (name, lat, lon) in spots {
            let w = geo_to_world_offset(lat, lon);
            for step in 0..25 {
                let dx = (step % 5) as f32 * 130.0;
                let dz = (step / 5) as f32 * 130.0;
                let h = surface_height_voxels(w.x + dx, w.y + dz);
                assert!(
                    (1..=MAX_SURFACE_VOXEL_Y).contains(&h),
                    "{name}: height {h} out of band"
                );
            }
        }

        // Indian Ocean west of the continent — unambiguously open water.
        let sea = geo_to_world_offset(-30.0, 110.0);
        assert_eq!(surface_height_voxels(sea.x, sea.y), 0);
    }

    /// Different regions actually get different relief — the ranges must not be
    /// statistically flat, the plains must not be mountainous.
    #[test]
    fn relief_varies_by_region() {
        let relief = |lat: f32, lon: f32| -> i32 {
            let w = geo_to_world_offset(lat, lon);
            let mut lo = i32::MAX;
            let mut hi = i32::MIN;
            for i in 0..40 {
                let h = surface_height_voxels(w.x + i as f32 * 220.0, w.y + i as f32 * 170.0);
                lo = lo.min(h);
                hi = hi.max(h);
            }
            hi - lo
        };

        let savanna_plain = relief(-14.5, 133.5);
        let tasmania_ranges = relief(-42.0, 146.8);
        assert!(
            tasmania_ranges > savanna_plain,
            "Tasmania ({tasmania_ranges}) should be rougher than the savanna plains ({savanna_plain})"
        );
        assert!(tasmania_ranges >= 24, "Tasmanian highlands too flat: {tasmania_ranges}");
    }

    /// The owner's dirt-depth spec, pinned: "Most likely below you there will
    /// be an average tree length's of dirt downward, but there is a 1% chance
    /// that it is bedrock right below you and 1% chance that the dirt descends
    /// two treelengths." Percentiles measured over a wide sample, plus a
    /// smoothness check so digging feels geological, not per-column random.
    #[test]
    fn dirt_depth_distribution_matches_owner_spec() {
        let n = 350;
        let mut total = 0f64;
        let mut at_zero = 0u32;
        let mut at_max = 0u32;
        let count = (n * n) as f64;
        for i in 0..n {
            for j in 0..n {
                // ~37 ft spacing over ~13 km — thousands of independent
                // geology cells at the 260-ft base wavelength.
                let x = i as f32 * 37.3 - 6500.0;
                let z = j as f32 * 41.7 - 7300.0;
                let d = dirt_depth_voxels(x, z);
                total += d as f64;
                if d == 0 {
                    at_zero += 1;
                }
                if d == 2 * TREE_LENGTH_VOXELS {
                    at_max += 1;
                }
            }
        }

        let mean = total / count;
        let target = TREE_LENGTH_VOXELS as f64;
        assert!(
            (mean - target).abs() < target * 0.15,
            "mean dirt depth {mean:.1} voxels should be ≈ one tree length ({target})"
        );
        let frac_zero = at_zero as f64 / count;
        let frac_max = at_max as f64 / count;
        assert!(
            (0.004..0.03).contains(&frac_zero),
            "bedrock-right-below fraction {frac_zero:.4} should be ≈ 1%"
        );
        assert!(
            (0.004..0.03).contains(&frac_max),
            "two-tree-lengths fraction {frac_max:.4} should be ≈ 1%"
        );

        // Adjacent columns (one 3-inch voxel apart) must be near-identical —
        // the depth field varies geologically, not as per-column noise.
        let mut max_step = 0i32;
        let mut step_sum = 0f64;
        let pairs = 4000;
        for k in 0..pairs {
            let x = (k as f32 * 53.71).sin() * 6000.0;
            let z = (k as f32 * 31.17).cos() * 6000.0;
            let step =
                (dirt_depth_voxels(x + VOXEL_SIZE, z) - dirt_depth_voxels(x, z)).abs();
            max_step = max_step.max(step);
            step_sum += step as f64;
        }
        assert!(
            max_step <= 4,
            "dirt depth jumped {max_step} voxels between adjacent columns"
        );
        assert!(
            step_sum / pairs as f64 <= 1.0,
            "dirt depth too jittery between adjacent columns: mean step {:.2}",
            step_sum / pairs as f64
        );
    }

    /// Caves exist underground at a sane density: enough to stumble into,
    /// nowhere near enough to swiss-cheese the world hollow.
    #[test]
    fn caves_exist_at_sane_density() {
        let w = geo_to_world_offset(-35.0, 146.0);
        let mut cave = 0u32;
        let mut total = 0u32;
        for i in 0..60 {
            for j in 0..60 {
                let x = w.x + i as f32 * 21.0;
                let z = w.y + j as f32 * 17.0;
                let surface = surface_height_voxels(x, z);
                let vx = (x / VOXEL_SIZE) as i32;
                let vz = (z / VOXEL_SIZE) as i32;
                // Probe the tunnel band under the skin.
                for depth in [12, 28, 44, 60] {
                    total += 1;
                    if is_cave_cell(vx, surface - depth, vz, surface) {
                        cave += 1;
                    }
                }
            }
        }
        let density = cave as f32 / total as f32;
        assert!(
            (0.005..0.35).contains(&density),
            "cave density {density:.4} out of the sane band"
        );
    }

    /// A worm crawling ~50 ft in any biome must cross real ups and downs — the
    /// micro-relief layer, not just the regional forms.
    #[test]
    fn ground_is_never_flat_at_worm_scale() {
        let spots = [(-24.5, 133.5), (-14.5, 133.5), (-35.0, 146.0)];
        for (lat, lon) in spots {
            let w = geo_to_world_offset(lat, lon);
            let mut lo = i32::MAX;
            let mut hi = i32::MIN;
            for i in 0..50 {
                let h = surface_height_voxels(w.x + i as f32, w.y + i as f32 * 0.7);
                lo = lo.min(h);
                hi = hi.max(h);
            }
            assert!(
                hi - lo >= 3,
                "terrain near ({lat}, {lon}) is billiard-flat: relief {} voxels",
                hi - lo
            );
        }
    }
}
