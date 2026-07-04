use bevy::prelude::*;
use std::collections::{HashMap, HashSet};

use crate::world::{GardenRng, VOXELS_PER_FOOT, VOXEL_INCHES};

const MAX_BRANCH_DROOP_RATIO: f32 = 0.7;

/// Chunky deterministic noise for crown shapes: buckets of voxels share one
/// value, giving solid lumps and deep notches instead of per-voxel fizz (a
/// porous canopy interior explodes the mesh and once exhausted VRAM).
fn lump_hash(x: i32, y: i32, z: i32, salt: u32) -> f32 {
    let mut h = (x as u32).wrapping_mul(0x8DA6_B343)
        ^ (y as u32).wrapping_mul(0xD816_3841)
        ^ (z as u32).wrapping_mul(0xCB1A_B31F)
        ^ salt.wrapping_mul(0x9E37_79B9);
    h ^= h >> 13;
    h = h.wrapping_mul(0x7FEB_352D);
    h ^= h >> 15;
    h as f32 / u32::MAX as f32
}

/// Native Australian species, each tied to the biomes where it really grows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TreeSpecies {
    RiverRedGum,  // widespread gum — savanna woodland, SE forests, watercourses
    GhostGum,     // white-barked gum of the red centre
    SnappyGum,    // small twisted white gum on Pilbara spinifex hills
    Karri,        // giant straight SW eucalypt
    Jarrah,       // dark rough-barked SW eucalypt
    MountainAsh,  // tallest flowering tree on Earth — SE wet forests
    SnowGum,      // low twisted gum of the high country
    Mulga,        // dominant arid-zone acacia shrub-tree
    GoldenWattle, // classic acacia of temperate woodland
    Boab,         // bottle-trunked Kimberley icon
    Paperbark,    // cream-barked melaleuca of tropical and coastal wetlands
    DesertOak,    // columnar drooping casuarina of the western deserts
    Banksia,      // gnarled coastal heath tree
    MoretonBayFig, // huge buttressed spreading fig of the east coast
    TreeFern,     // soft tree fern of wet gullies
    HuonPine,     // slow conifer of Tasmanian rivers
    MyrtleBeech,  // dense-crowned Tasmanian rainforest beech
}

pub const ALL_SPECIES: [TreeSpecies; 17] = [
    TreeSpecies::RiverRedGum,
    TreeSpecies::GhostGum,
    TreeSpecies::SnappyGum,
    TreeSpecies::Karri,
    TreeSpecies::Jarrah,
    TreeSpecies::MountainAsh,
    TreeSpecies::SnowGum,
    TreeSpecies::Mulga,
    TreeSpecies::GoldenWattle,
    TreeSpecies::Boab,
    TreeSpecies::Paperbark,
    TreeSpecies::DesertOak,
    TreeSpecies::Banksia,
    TreeSpecies::MoretonBayFig,
    TreeSpecies::TreeFern,
    TreeSpecies::HuonPine,
    TreeSpecies::MyrtleBeech,
];

/// Approximate outline of a tree for the distant-silhouette impostors, in feet.
/// `height_ft` replays the exact first RNG draw of the real generator, so a
/// silhouette is the same height as the tree that eventually replaces it.
pub struct SilhouetteSpec {
    pub height_ft: f32,
    pub trunk_width_ft: f32,
    pub crown_radius_ft: f32,
    pub cone: bool,
}

pub fn silhouette_spec(species: TreeSpecies, tree_seed: u64) -> SilhouetteSpec {
    let mut rng = GardenRng::new(tree_seed);

    if species == TreeSpecies::TreeFern {
        let height_ft = rng.range_i(20, 40) as f32;
        return SilhouetteSpec {
            height_ft,
            trunk_width_ft: 1.5,
            crown_radius_ft: 14.0,
            cone: false,
        };
    }

    let form = form_for(species);
    let height_ft = rng.range_i(form.height_ft.0, form.height_ft.1) as f32;
    let trunk_width_ft = form.radius_in as f32 * 2.0 / 12.0;
    let crown_radius_ft = if let Some((rx_in, _)) = form.dome {
        rx_in / 12.0 * 1.2
    } else if form.cone {
        115.0 / 12.0
    } else {
        // Whichever reaches farther: branch tips with their pom-poms, or the
        // apex crown capping the trunk (spread + enlarged blobs).
        let branch_reach = form.branch_len_in.1 as f32 * 0.5 + 46.0 * form.blob_scale;
        let apex_reach = 122.0 * form.blob_scale;
        branch_reach.max(apex_reach) / 12.0
    };

    SilhouetteSpec {
        height_ft,
        trunk_width_ft,
        crown_radius_ft,
        cone: form.cone,
    }
}

/// Kept for upcoming UI/fauna features (e.g. naming what the worm is under).
#[allow(dead_code)]
pub fn species_display_name(species: TreeSpecies) -> &'static str {
    match species {
        TreeSpecies::RiverRedGum => "River Red Gum",
        TreeSpecies::GhostGum => "Ghost Gum",
        TreeSpecies::SnappyGum => "Snappy Gum",
        TreeSpecies::Karri => "Karri",
        TreeSpecies::Jarrah => "Jarrah",
        TreeSpecies::MountainAsh => "Mountain Ash",
        TreeSpecies::SnowGum => "Snow Gum",
        TreeSpecies::Mulga => "Mulga",
        TreeSpecies::GoldenWattle => "Golden Wattle",
        TreeSpecies::Boab => "Boab",
        TreeSpecies::Paperbark => "Paperbark",
        TreeSpecies::DesertOak => "Desert Oak",
        TreeSpecies::Banksia => "Banksia",
        TreeSpecies::MoretonBayFig => "Moreton Bay Fig",
        TreeSpecies::TreeFern => "Soft Tree Fern",
        TreeSpecies::HuonPine => "Huon Pine",
        TreeSpecies::MyrtleBeech => "Myrtle Beech",
    }
}

/// (bark, foliage) base colours — jittered slightly per tree at spawn time.
pub fn species_colors(species: TreeSpecies) -> ((f32, f32, f32), (f32, f32, f32)) {
    match species {
        TreeSpecies::RiverRedGum => ((0.67, 0.54, 0.40), (0.45, 0.63, 0.52)),
        TreeSpecies::GhostGum => ((0.90, 0.88, 0.82), (0.52, 0.64, 0.48)),
        TreeSpecies::SnappyGum => ((0.86, 0.84, 0.78), (0.50, 0.60, 0.44)),
        TreeSpecies::Karri => ((0.78, 0.70, 0.58), (0.38, 0.56, 0.40)),
        TreeSpecies::Jarrah => ((0.42, 0.30, 0.24), (0.36, 0.52, 0.38)),
        TreeSpecies::MountainAsh => ((0.75, 0.72, 0.62), (0.40, 0.58, 0.44)),
        TreeSpecies::SnowGum => ((0.84, 0.79, 0.70), (0.50, 0.62, 0.46)),
        TreeSpecies::Mulga => ((0.38, 0.31, 0.24), (0.55, 0.58, 0.36)),
        TreeSpecies::GoldenWattle => ((0.40, 0.33, 0.26), (0.62, 0.70, 0.32)),
        TreeSpecies::Boab => ((0.72, 0.62, 0.52), (0.42, 0.60, 0.34)),
        TreeSpecies::Paperbark => ((0.90, 0.87, 0.78), (0.48, 0.62, 0.44)),
        TreeSpecies::DesertOak => ((0.30, 0.26, 0.22), (0.38, 0.46, 0.34)),
        TreeSpecies::Banksia => ((0.45, 0.38, 0.30), (0.34, 0.48, 0.30)),
        TreeSpecies::MoretonBayFig => ((0.55, 0.50, 0.44), (0.24, 0.42, 0.24)),
        TreeSpecies::TreeFern => ((0.35, 0.28, 0.22), (0.30, 0.55, 0.26)),
        TreeSpecies::HuonPine => ((0.44, 0.36, 0.28), (0.28, 0.46, 0.30)),
        TreeSpecies::MyrtleBeech => ((0.40, 0.34, 0.28), (0.26, 0.44, 0.28)),
    }
}

/// Bark and foliage both live on the same world voxel grid — one block size for
/// everything the worm sees.
pub struct VoxelTreeData {
    pub bark: HashSet<IVec3>,
    pub foliage: HashSet<IVec3>,
}

pub fn generate_tree(species: TreeSpecies, rng: &mut GardenRng) -> VoxelTreeData {
    match species {
        TreeSpecies::TreeFern => generate_tree_fern(rng),
        _ => generate_form_tree(rng, &form_for(species)),
    }
}

/// One knob-set per species: trunk profile, branching habit, and crown style.
/// Physical measurements are in feet/inches so they survive voxel-size changes;
/// every species except the tree fern is an instance of the same generator.
struct TreeForm {
    height_ft: (i32, i32),
    radius_in: i32,
    /// Extra trunk radius (inches) at ground level, fading out over the bottom 10%.
    base_flare_in: i32,
    /// Fraction of radius shed above 72% height (0.0 = no taper).
    top_taper: f32,
    /// Boab bottle profile: fat belly, sharp shoulder taper.
    bottle: bool,
    wobble: f32,
    /// Max lateral trunk drift, in inches.
    wobble_clamp_in: i32,
    /// (min, max) extra stems forked near the crown — wattle silhouette.
    forks: (i32, i32),
    /// Fraction of trunk height where branches start.
    branch_zone: f32,
    branch_count: (i32, i32),
    branch_len_in: (i32, i32),
    branch_elev_deg: (f32, f32),
    low_elev_deg: (f32, f32),
    outward_pull: (f32, f32),
    /// Chance per branch voxel of sprouting foliage along the branch, not just tips.
    canopy_along_branch: f32,
    /// Scales the pom-pom foliage radii.
    blob_scale: f32,
    /// Dense ellipsoid crown over the whole top: (rx, ry) in inches.
    dome: Option<(f32, f32)>,
    /// Conifer: solid cone of foliage from ~30% height to the tip.
    cone: bool,
}

impl Default for TreeForm {
    // Defaults are a mature river red gum — a genuine skyscraper of a tree:
    // 160–260 ft tall, 7 ft through the trunk, crown blobs the size of houses.
    fn default() -> Self {
        Self {
            height_ft: (160, 260),
            radius_in: 44,
            base_flare_in: 0,
            top_taper: 0.0,
            bottle: false,
            wobble: 0.05,
            wobble_clamp_in: 8,
            forks: (0, 0),
            branch_zone: 0.58,
            branch_count: (6, 10),
            branch_len_in: (180, 480),
            branch_elev_deg: (12.0, 72.0),
            low_elev_deg: (-28.0, 18.0),
            outward_pull: (0.25, 0.55),
            canopy_along_branch: 0.0,
            blob_scale: 1.6,
            dome: None,
            cone: false,
        }
    }
}

/// Erdtree rule: the player is a 3-inch worm, so trees are BIG-ASS TALL — sky-
/// piercing towers. A mountain ash tops 650 ft here (~2,500 worm-lengths); even
/// the "shrubs" would dwarf a house. Girth and crown reach scale with height so
/// nothing looks like a flagpole.
fn form_for(species: TreeSpecies) -> TreeForm {
    match species {
        TreeSpecies::RiverRedGum => TreeForm::default(),
        TreeSpecies::GhostGum => TreeForm {
            height_ft: (90, 150),
            radius_in: 26,
            branch_zone: 0.5,
            branch_count: (4, 7),
            branch_len_in: (140, 320),
            blob_scale: 1.2,
            ..default_form()
        },
        TreeSpecies::SnappyGum => TreeForm {
            height_ft: (35, 60),
            radius_in: 16,
            wobble: 0.25,
            wobble_clamp_in: 16,
            branch_zone: 0.35,
            branch_count: (4, 7),
            branch_len_in: (90, 200),
            ..default_form()
        },
        TreeSpecies::Karri => TreeForm {
            height_ft: (350, 550),
            radius_in: 60,
            wobble: 0.02,
            branch_zone: 0.78,
            branch_count: (5, 8),
            branch_len_in: (220, 560),
            blob_scale: 1.8,
            ..default_form()
        },
        TreeSpecies::Jarrah => TreeForm {
            height_ft: (220, 350),
            radius_in: 48,
            branch_zone: 0.6,
            branch_count: (6, 10),
            branch_len_in: (160, 420),
            blob_scale: 1.5,
            ..default_form()
        },
        TreeSpecies::MountainAsh => TreeForm {
            height_ft: (450, 650),
            radius_in: 72,
            wobble: 0.02,
            branch_zone: 0.8,
            branch_count: (5, 8),
            branch_len_in: (240, 640),
            blob_scale: 1.9,
            ..default_form()
        },
        TreeSpecies::SnowGum => TreeForm {
            height_ft: (35, 60),
            radius_in: 16,
            wobble: 0.3,
            wobble_clamp_in: 20,
            forks: (1, 2),
            branch_zone: 0.3,
            branch_count: (5, 9),
            branch_len_in: (90, 240),
            branch_elev_deg: (0.0, 45.0),
            low_elev_deg: (-30.0, 5.0),
            blob_scale: 1.0,
            ..default_form()
        },
        TreeSpecies::Mulga => TreeForm {
            height_ft: (30, 55),
            radius_in: 12,
            wobble: 0.15,
            wobble_clamp_in: 12,
            forks: (2, 3),
            branch_zone: 0.25,
            branch_count: (6, 10),
            branch_len_in: (80, 190),
            canopy_along_branch: 0.05,
            blob_scale: 0.8,
            ..default_form()
        },
        TreeSpecies::GoldenWattle => TreeForm {
            height_ft: (70, 120),
            radius_in: 18,
            wobble: 0.12,
            wobble_clamp_in: 16,
            top_taper: 0.45,
            forks: (2, 3),
            branch_zone: 0.22,
            branch_count: (8, 14),
            branch_len_in: (100, 300),
            branch_elev_deg: (4.0, 48.0),
            low_elev_deg: (-18.0, 22.0),
            outward_pull: (0.45, 0.75),
            canopy_along_branch: 0.05,
            blob_scale: 1.2,
            ..default_form()
        },
        TreeSpecies::Boab => TreeForm {
            height_ft: (50, 90),
            radius_in: 90,
            bottle: true,
            wobble: 0.03,
            branch_zone: 0.85,
            branch_count: (5, 8),
            branch_len_in: (160, 380),
            branch_elev_deg: (18.0, 70.0),
            blob_scale: 1.0,
            ..default_form()
        },
        TreeSpecies::Paperbark => TreeForm {
            height_ft: (90, 160),
            radius_in: 20,
            branch_zone: 0.4,
            branch_count: (7, 12),
            branch_len_in: (130, 330),
            canopy_along_branch: 0.06,
            blob_scale: 1.2,
            ..default_form()
        },
        TreeSpecies::DesertOak => TreeForm {
            height_ft: (70, 120),
            radius_in: 18,
            branch_zone: 0.25,
            branch_count: (12, 20),
            branch_len_in: (50, 130),
            branch_elev_deg: (-24.0, 12.0),
            low_elev_deg: (-32.0, -6.0),
            outward_pull: (0.15, 0.35),
            canopy_along_branch: 0.03,
            blob_scale: 0.7,
            ..default_form()
        },
        TreeSpecies::Banksia => TreeForm {
            height_ft: (40, 75),
            radius_in: 16,
            wobble: 0.25,
            wobble_clamp_in: 16,
            branch_zone: 0.35,
            branch_count: (6, 10),
            branch_len_in: (80, 200),
            blob_scale: 0.9,
            dome: Some((80.0, 45.0)),
            ..default_form()
        },
        TreeSpecies::MoretonBayFig => TreeForm {
            height_ft: (130, 220),
            radius_in: 66,
            base_flare_in: 40,
            branch_zone: 0.45,
            branch_count: (6, 9),
            branch_len_in: (260, 700),
            branch_elev_deg: (-6.0, 26.0),
            low_elev_deg: (-20.0, 8.0),
            outward_pull: (0.5, 0.85),
            canopy_along_branch: 0.015,
            blob_scale: 1.25,
            dome: Some((160.0, 58.0)),
            ..default_form()
        },
        TreeSpecies::HuonPine => TreeForm {
            height_ft: (90, 160),
            radius_in: 20,
            branch_zone: 0.6,
            branch_count: (3, 6),
            branch_len_in: (60, 140),
            blob_scale: 0.8,
            cone: true,
            ..default_form()
        },
        TreeSpecies::MyrtleBeech => TreeForm {
            height_ft: (110, 180),
            radius_in: 32,
            branch_zone: 0.5,
            branch_count: (6, 10),
            branch_len_in: (100, 280),
            canopy_along_branch: 0.04,
            dome: Some((130.0, 70.0)),
            ..default_form()
        },
        TreeSpecies::TreeFern => TreeForm::default(), // handled by its own generator
    }
}

fn default_form() -> TreeForm {
    TreeForm::default()
}

fn generate_form_tree(rng: &mut GardenRng, form: &TreeForm) -> VoxelTreeData {
    let trunk = build_trunk(rng, form);
    let (branches, mut foliage) = build_branches(rng, form, &trunk);

    if let Some((rx_in, ry_in)) = form.dome {
        foliage.extend(dome_canopy(rng, &trunk, rx_in, ry_in));
    }
    if form.cone {
        foliage.extend(cone_canopy(rng, &trunk));
    }
    let mut bark = trunk;
    bark.extend(branches);

    if form.dome.is_none() && !form.cone {
        let (crown_wood, crown_leaves) = apex_crown(rng, &bark, form);
        bark.extend(crown_wood);
        foliage.extend(crown_leaves);
    }

    VoxelTreeData { bark, foliage }
}

fn build_trunk(rng: &mut GardenRng, form: &TreeForm) -> HashSet<IVec3> {
    let height_voxels = (rng.range_i(form.height_ft.0, form.height_ft.1) * VOXELS_PER_FOOT).max(4);
    let radius_voxels = (form.radius_in / VOXEL_INCHES).max(1) as f32;
    let flare_voxels = form.base_flare_in as f32 / VOXEL_INCHES as f32;
    let wobble_clamp = (form.wobble_clamp_in / VOXEL_INCHES).max(1);
    let bottle_radius = radius_voxels + rng.range_i(-4, 8) as f32 / VOXEL_INCHES as f32;

    let mut center_x = 0i32;
    let mut center_z = 0i32;
    let mut trunk = HashSet::new();

    for y in 0..height_voxels {
        if y > 0 {
            if rng.chance(form.wobble) {
                center_x += rng.choice_i(&[-1, 0, 1]);
            }
            if rng.chance(form.wobble) {
                center_z += rng.choice_i(&[-1, 0, 1]);
            }
            center_x = center_x.clamp(-wobble_clamp, wobble_clamp);
            center_z = center_z.clamp(-wobble_clamp, wobble_clamp);
        }

        let t = y as f32 / height_voxels as f32;
        let mut r = radius_voxels;
        if form.bottle {
            r = if t < 0.55 {
                bottle_radius * (1.06 - 0.18 * t)
            } else {
                let k = ((t - 0.55) / 0.45).powf(0.8);
                let shoulder = bottle_radius * 0.96;
                shoulder + (2.5 - shoulder) * k
            };
        } else {
            if flare_voxels > 0.0 && t < 0.1 {
                r += flare_voxels * (1.0 - t / 0.1);
            }
            if form.top_taper > 0.0 && t > 0.72 {
                r *= 1.0 - form.top_taper * ((t - 0.72) / 0.28);
            }
        }
        let ri = r.round().max(1.0) as i32;
        let ri_sq = ri * ri;
        // Fat trunks are hollow shells (a 2-voxel-thick ring): the culled mesh
        // only ever shows the surface, so interior voxels are pure waste. Thin
        // trunks and the capping top layer stay solid.
        let inner_sq = if ri >= 4 && y + 1 < height_voxels {
            let inner = ri - 2;
            inner * inner
        } else {
            -1
        };

        for dx in -ri..=ri {
            for dz in -ri..=ri {
                let d_sq = dx * dx + dz * dz;
                if d_sq <= ri_sq && d_sq > inner_sq {
                    trunk.insert(IVec3::new(center_x + dx, y, center_z + dz));
                }
            }
        }
    }

    // Optional crown forks — classic wattle / snow gum multi-stem silhouette.
    let fork_count = if form.forks.1 > 0 {
        rng.range_i(form.forks.0, form.forks.1)
    } else {
        0
    };
    let fork_start = height_voxels * 7 / 10;
    for _ in 0..fork_count {
        let mut fx = center_x;
        let mut fz = center_z;
        let fork_dx = rng.choice_i(&[-1, 1]);
        let fork_dz = rng.choice_i(&[-1, 1]);
        let fork_len = rng.range_i(18, 42) / VOXEL_INCHES;

        for i in 0..fork_len {
            let y = fork_start + i;
            fx += if i > 0 && rng.chance(0.35) { fork_dx.signum() } else { 0 };
            fz += if i > 0 && rng.chance(0.35) { fork_dz.signum() } else { 0 };

            let stem_r = rng.range_i(2, 4);
            let stem_r_sq = stem_r * stem_r;
            for dx in -stem_r..=stem_r {
                for dz in -stem_r..=stem_r {
                    if dx * dx + dz * dz <= stem_r_sq {
                        trunk.insert(IVec3::new(fx + dx, y, fz + dz));
                    }
                }
            }
        }
    }

    trunk
}

fn build_branches(
    rng: &mut GardenRng,
    form: &TreeForm,
    trunk: &HashSet<IVec3>,
) -> (HashSet<IVec3>, HashSet<IVec3>) {
    let min_y = trunk.iter().map(|p| p.y).min().unwrap_or(0);
    let max_y = trunk.iter().map(|p| p.y).max().unwrap_or(0);
    let trunk_height = max_y - min_y;
    let branch_zone_start = min_y + (trunk_height as f32 * form.branch_zone) as i32;

    let mut branches = HashSet::new();
    let mut foliage = HashSet::new();
    let branch_count = rng.range_i(form.branch_count.0, form.branch_count.1);

    for _ in 0..branch_count {
        let attach_top = max_y.saturating_sub(2).max(branch_zone_start);
        let attach_y = rng.range_i(branch_zone_start, attach_top);
        let ring: Vec<IVec3> = trunk
            .iter()
            .copied()
            .filter(|p| p.y == attach_y)
            .collect();
        if ring.is_empty() {
            continue;
        }

        let start = ring[(rng.next_f32() * ring.len() as f32).floor() as usize];
        let center = trunk_centroid_at_y(trunk, attach_y);
        let outward = Vec3::new(
            (start.x - center.x) as f32,
            0.0,
            (start.z - center.z) as f32,
        )
        .normalize_or_zero();

        let dir = sample_branch_direction(rng, outward, form);
        let length_voxels =
            (rng.range_i(form.branch_len_in.0, form.branch_len_in.1) / VOXEL_INCHES).max(2);
        let path = rasterize_branch(start, dir, length_voxels);

        let mut tip = None;
        for (i, block) in path.iter().enumerate() {
            if !trunk.contains(block) {
                // 2×2×2 voxel cross-section: a 4-inch limb, sturdy enough to
                // read as a branch next to 2-inch ground blocks.
                for d in [
                    IVec3::ZERO,
                    IVec3::X,
                    IVec3::Y,
                    IVec3::Z,
                    IVec3::new(1, 1, 0),
                    IVec3::new(1, 0, 1),
                    IVec3::new(0, 1, 1),
                    IVec3::new(1, 1, 1),
                ] {
                    branches.insert(*block + d);
                }
                tip = Some(*block);

                if i > 2 && rng.chance(form.canopy_along_branch) {
                    foliage.extend(pom_foliage(rng, *block, form.blob_scale));
                }
            }
        }

        if let Some(tip_pos) = tip {
            foliage.extend(pom_foliage(rng, tip_pos, form.blob_scale));
        }
    }

    (branches, foliage)
}

/// Upward-biased branch direction with limited droop below horizontal, tuned by
/// the species form (fig branches run near-flat; desert oak droops).
fn sample_branch_direction(rng: &mut GardenRng, outward: Vec3, form: &TreeForm) -> Vec3 {
    let azimuth = rng.range(0.0, std::f32::consts::TAU);
    let elev_deg = if rng.chance(0.8) {
        rng.range(form.branch_elev_deg.0, form.branch_elev_deg.1)
    } else {
        rng.range(form.low_elev_deg.0, form.low_elev_deg.1)
    };
    let elev = elev_deg.to_radians();

    let mut dir = Vec3::new(
        elev.cos() * azimuth.cos(),
        elev.sin(),
        elev.cos() * azimuth.sin(),
    );

    if dir.y < 0.0 {
        let horizontal = Vec2::new(dir.x, dir.z).length().max(0.05);
        if dir.y.abs() > MAX_BRANCH_DROOP_RATIO * horizontal {
            dir.y = -MAX_BRANCH_DROOP_RATIO * horizontal;
        }
    }

    if outward.length_squared() > 0.01 {
        dir = (dir + outward * rng.range(form.outward_pull.0, form.outward_pull.1)).normalize();
    } else {
        dir = dir.normalize();
    }

    dir
}

fn trunk_centroid_at_y(trunk: &HashSet<IVec3>, y: i32) -> IVec3 {
    let mut count = 0i32;
    let mut cx = 0i32;
    let mut cz = 0i32;
    for p in trunk.iter().filter(|p| p.y == y) {
        cx += p.x;
        cz += p.z;
        count += 1;
    }
    if count == 0 {
        return IVec3::ZERO;
    }
    IVec3::new(cx / count, y, cz / count)
}

/// Trunk centreline per layer in one pass — cone canopies need every layer and
/// scanning the whole trunk set per layer would be quadratic.
fn trunk_centroids(trunk: &HashSet<IVec3>) -> HashMap<i32, IVec2> {
    let mut sums: HashMap<i32, (i64, i64, i64)> = HashMap::new();
    for p in trunk {
        let e = sums.entry(p.y).or_insert((0, 0, 0));
        e.0 += p.x as i64;
        e.1 += p.z as i64;
        e.2 += 1;
    }
    sums.into_iter()
        .map(|(y, (sx, sz, n))| (y, IVec2::new((sx / n) as i32, (sz / n) as i32)))
        .collect()
}

fn rasterize_branch(start: IVec3, dir: Vec3, length_voxels: i32) -> Vec<IVec3> {
    let mut blocks = Vec::with_capacity(length_voxels as usize);
    let mut pos = Vec3::new(
        start.x as f32 + 0.5,
        start.y as f32 + 0.5,
        start.z as f32 + 0.5,
    );
    let step = dir.normalize() * 0.55;
    let mut prev = start;
    blocks.push(start);

    for _ in 0..length_voxels {
        pos += step;
        let grid = IVec3::new(
            pos.x.floor() as i32,
            pos.y.floor() as i32,
            pos.z.floor() as i32,
        );
        if grid != prev {
            blocks.push(grid);
            prev = grid;
        }
    }

    blocks
}

/// Pom-pom leaf cloud at a branch tip — rounded, slightly irregular, sized in
/// real inches and rasterised straight onto the world voxel grid.
fn pom_foliage(rng: &mut GardenRng, tip: IVec3, scale: f32) -> HashSet<IVec3> {
    let jitter = 8 / VOXEL_INCHES;
    let center = tip
        + IVec3::new(
            rng.range_i(-jitter, jitter),
            rng.range_i(0, 2 * jitter),
            rng.range_i(-jitter, jitter),
        );

    let mut cloud = HashSet::new();
    let radius_x = rng.range(26.0, 46.0) * scale / VOXEL_INCHES as f32;
    let radius_y = rng.range(19.0, 34.0) * scale / VOXEL_INCHES as f32;
    let radius_z = rng.range(26.0, 46.0) * scale / VOXEL_INCHES as f32;
    // Lumpy but SOLID: the ellipsoid's radius is eaten into by chunky noise,
    // giving a ragged, sun-pierced silhouette while the interior stays filled
    // (only the shell ever becomes mesh faces).
    let salt = rng.next_u32();

    let ex = radius_x.ceil() as i32;
    let ez = radius_z.ceil() as i32;
    let ey_up = radius_y.ceil() as i32 + 1;
    let ey_down = (radius_y * 0.55).ceil() as i32;

    for dx in -ex..=ex {
        for dy in -ey_down..=ey_up {
            for dz in -ez..=ez {
                let nx = dx as f32 / radius_x;
                let ny = dy as f32 / radius_y;
                let nz = dz as f32 / radius_z;
                let dist_sq = nx * nx + ny * ny + nz * nz;
                if dist_sq > 1.0 {
                    continue;
                }

                let lump =
                    lump_hash(dx.div_euclid(4), dy.div_euclid(4), dz.div_euclid(4), salt);
                if dist_sq <= 1.0 - 0.45 * lump {
                    cloud.insert(center + IVec3::new(dx, dy, dz));
                }
            }
        }
    }

    cloud
}

/// Leafy cap on the trunk apex: a cluster of enlarged pom blobs around the top
/// of the tree, each held up by a real 4-inch branchlet grown from the trunk
/// apex out to the tuft — leaves have weight, so wood must carry them; nothing
/// floats. Returns (supporting wood, leaves).
fn apex_crown(
    rng: &mut GardenRng,
    trunk: &HashSet<IVec3>,
    form: &TreeForm,
) -> (HashSet<IVec3>, HashSet<IVec3>) {
    let max_y = trunk.iter().map(|p| p.y).max().unwrap_or(0);
    let apex = trunk_centroid_at_y(trunk, max_y);

    let scale = form.blob_scale * 1.25;
    let spread = ((64.0 * form.blob_scale) as i32 / VOXEL_INCHES).max(1);
    let dip = (14 / VOXEL_INCHES).max(1);

    let mut wood = HashSet::new();
    let mut cloud = HashSet::new();
    let tuft_count = rng.range_i(2, 4);
    for _ in 0..tuft_count {
        let offset = IVec3::new(
            rng.range_i(-spread, spread),
            rng.range_i(-dip, dip / 2),
            rng.range_i(-spread, spread),
        );
        let target = apex + offset;

        let reach = offset.as_vec3();
        if reach.length_squared() > 1.0 {
            // rasterize_branch advances ~0.55 voxel per step, so oversample
            // the step count to actually span the offset.
            let steps = (reach.length() * 1.9).ceil() as i32;
            for block in rasterize_branch(apex, reach.normalize(), steps) {
                for d in [
                    IVec3::ZERO,
                    IVec3::X,
                    IVec3::Y,
                    IVec3::Z,
                    IVec3::new(1, 1, 0),
                    IVec3::new(1, 0, 1),
                    IVec3::new(0, 1, 1),
                    IVec3::new(1, 1, 1),
                ] {
                    wood.insert(block + d);
                }
            }
        }

        cloud.extend(pom_foliage(rng, target, scale));
    }
    (wood, cloud)
}

/// Dense ellipsoid crown capping the whole tree — figs, banksias, beeches.
/// Radii in inches.
fn dome_canopy(rng: &mut GardenRng, trunk: &HashSet<IVec3>, rx_in: f32, ry_in: f32) -> HashSet<IVec3> {
    let max_y = trunk.iter().map(|p| p.y).max().unwrap_or(0);
    let crown = trunk_centroid_at_y(trunk, max_y);
    let lift = 8 / VOXEL_INCHES;
    let center = IVec3::new(crown.x, max_y + lift, crown.z);

    let rx = rx_in / VOXEL_INCHES as f32 * rng.range(0.85, 1.2);
    let rz = rx * rng.range(0.85, 1.15);
    let ry = ry_in / VOXEL_INCHES as f32 * rng.range(0.85, 1.2);

    let mut cloud = HashSet::new();
    let salt = rng.next_u32();
    let ex = rx.ceil() as i32;
    let ez = rz.ceil() as i32;
    let ey_up = ry.ceil() as i32;
    // Flatten the underside so the canopy sits on the crown instead of swallowing it.
    let ey_down = (ry * 0.45).ceil() as i32;

    for dx in -ex..=ex {
        for dy in -ey_down..=ey_up {
            for dz in -ez..=ez {
                let nx = dx as f32 / rx;
                let ny = dy as f32 / ry;
                let nz = dz as f32 / rz;
                let dist_sq = nx * nx + ny * ny + nz * nz;
                if dist_sq > 1.0 {
                    continue;
                }
                // Lumpy solid dome — ragged rim, filled interior.
                let lump =
                    lump_hash(dx.div_euclid(6), dy.div_euclid(4), dz.div_euclid(6), salt);
                if dist_sq <= 1.0 - 0.35 * lump {
                    cloud.insert(center + IVec3::new(dx, dy, dz));
                }
            }
        }
    }

    cloud
}

/// Solid cone of foliage from ~30% height to just past the tip — Huon pine.
fn cone_canopy(rng: &mut GardenRng, trunk: &HashSet<IVec3>) -> HashSet<IVec3> {
    let min_y = trunk.iter().map(|p| p.y).min().unwrap_or(0);
    let max_y = trunk.iter().map(|p| p.y).max().unwrap_or(0);
    let start_y = min_y + ((max_y - min_y) as f32 * 0.3) as i32;
    let top_y = max_y + 8 / VOXEL_INCHES;

    let base_radius = rng.range(90.0, 140.0) / VOXEL_INCHES as f32;
    let salt = rng.next_u32();
    let centroids = trunk_centroids(trunk);

    let mut cloud = HashSet::new();
    for y in start_y..=top_y {
        let t = (y - start_y) as f32 / (top_y - start_y).max(1) as f32;
        let r = base_radius + (1.2 - base_radius) * t;
        // Follow the trunk centreline as it wobbles (hold the last known layer
        // above the trunk top).
        let c = centroids
            .get(&y.min(max_y))
            .copied()
            .unwrap_or(IVec2::ZERO);

        let er = r.ceil() as i32;
        for dx in -er..=er {
            for dz in -er..=er {
                let nd = (dx * dx + dz * dz) as f32 / (r * r);
                if nd > 1.0 {
                    continue;
                }
                // Lumpy solid cone — ragged boughs, filled interior.
                let lump =
                    lump_hash(dx.div_euclid(4), y.div_euclid(6), dz.div_euclid(4), salt);
                if nd <= 1.0 - 0.4 * lump {
                    cloud.insert(IVec3::new(c.x + dx, y, c.y + dz));
                }
            }
        }
    }

    cloud
}

/// Soft tree fern: skinny fibrous trunk, no branches, a crown of long drooping
/// fronds traced as drooping arcs of leaf voxels.
fn generate_tree_fern(rng: &mut GardenRng) -> VoxelTreeData {
    let height_voxels = (rng.range_i(20, 40) * VOXELS_PER_FOOT).max(4);
    let trunk_r = (8 / VOXEL_INCHES).max(1); // ~16-inch fibrous trunk
    let clamp = (4 / VOXEL_INCHES).max(1);

    let mut center_x = 0i32;
    let mut center_z = 0i32;
    let mut trunk = HashSet::new();
    for y in 0..height_voxels {
        if y > 0 && rng.chance(0.06) {
            center_x = (center_x + rng.choice_i(&[-1, 0, 1])).clamp(-clamp, clamp);
            center_z = (center_z + rng.choice_i(&[-1, 0, 1])).clamp(-clamp, clamp);
        }
        for dx in -trunk_r..=trunk_r {
            for dz in -trunk_r..=trunk_r {
                if dx * dx + dz * dz <= trunk_r * trunk_r {
                    trunk.insert(IVec3::new(center_x + dx, y, center_z + dz));
                }
            }
        }
    }

    let crown = IVec3::new(center_x, height_voxels - 1, center_z);
    let mut foliage = HashSet::new();

    // Small tuft right at the crown.
    let tuft = 12 / VOXEL_INCHES;
    for dx in -tuft..=tuft {
        for dz in -tuft..=tuft {
            if dx * dx + dz * dz <= tuft * tuft {
                foliage.insert(crown + IVec3::new(dx, 2, dz));
            }
        }
    }

    // Radiating fronds that rise, arc over, and droop toward the ground —
    // each step lays a 4-inch clump of leaf voxels.
    let frond_count = rng.range_i(6, 10);
    for k in 0..frond_count {
        let azimuth = k as f32 / frond_count as f32 * std::f32::consts::TAU
            + rng.range(-0.25, 0.25);
        let mut dir = Vec3::new(azimuth.cos(), 0.55, azimuth.sin()).normalize();
        let mut pos = Vec3::new(
            crown.x as f32 + 0.5,
            crown.y as f32 + 2.0,
            crown.z as f32 + 0.5,
        );
        let frond_len = rng.range_i(110, 200) / VOXEL_INCHES;

        for _ in 0..frond_len {
            pos += dir;
            dir.y -= 0.11;
            dir = dir.normalize();
            let grid = IVec3::new(
                pos.x.floor() as i32,
                pos.y.floor() as i32,
                pos.z.floor() as i32,
            );
            for d in [IVec3::ZERO, IVec3::X, IVec3::Z, IVec3::new(1, 0, 1)] {
                foliage.insert(grid + d);
            }
        }
    }

    VoxelTreeData {
        bark: trunk,
        foliage,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_species_generates_bark_and_foliage() {
        for (i, species) in ALL_SPECIES.iter().enumerate() {
            for seed in 0..6u64 {
                let mut rng = GardenRng::new(0xA11CE ^ (seed * 977) ^ (i as u64 * 31337));
                let tree = generate_tree(*species, &mut rng);
                assert!(!tree.bark.is_empty(), "{species:?} produced no bark");
                assert!(!tree.foliage.is_empty(), "{species:?} produced no foliage");
                // Trunks must start at ground level so trees root into terrain.
                let min_y = tree.bark.iter().map(|p| p.y).min().unwrap();
                assert_eq!(min_y, 0, "{species:?} trunk floats above its base");
            }
        }
    }

    /// Tree generation cost is bounded — giant species must not explode into
    /// tens of millions of voxels when the voxel size shrinks.
    #[test]
    fn giant_species_stay_within_voxel_budget() {
        for species in [
            TreeSpecies::MountainAsh,
            TreeSpecies::Karri,
            TreeSpecies::MoretonBayFig,
            TreeSpecies::Boab,
        ] {
            let mut rng = GardenRng::new(0xB0AB);
            let tree = generate_tree(species, &mut rng);
            let total = tree.bark.len() + tree.foliage.len();
            assert!(
                total < 3_000_000,
                "{species:?} generated {total} voxels — too heavy"
            );
        }
    }

    /// Distant-canopy LOD: each coarser rung must cut the foliage mesh hard,
    /// and never come out empty (a far tree with an invisible crown).
    #[test]
    fn foliage_lod_rungs_shrink_the_mesh() {
        use crate::terrain::{build_culled_voxel_mesh, downsample_blocks};
        let mut rng = GardenRng::new(0xB0AB);
        let tree = generate_tree(TreeSpecies::MountainAsh, &mut rng);

        let mut prev_verts = build_culled_voxel_mesh(&tree.foliage, 1.0).count_vertices();
        for factor in [4, 16] {
            let coarse = downsample_blocks(&tree.foliage, factor, 0.2);
            assert!(!coarse.is_empty(), "factor {factor} erased the canopy");
            let verts = build_culled_voxel_mesh(&coarse, factor as f32).count_vertices();
            assert!(
                verts * 4 < prev_verts,
                "factor {factor} only got {verts} verts from {prev_verts} — LOD too weak"
            );
            prev_verts = verts;
        }
    }

    /// GPU memory guard: the *meshes* of the giants must stay small too. A
    /// porous canopy interior once ballooned vertex buffers until wgpu ran out
    /// of VRAM — solid cores keep the mesh a shell.
    #[test]
    fn giant_species_meshes_stay_within_vertex_budget() {
        use crate::terrain::build_culled_voxel_mesh;
        for species in [
            TreeSpecies::MountainAsh,
            TreeSpecies::Karri,
            TreeSpecies::MoretonBayFig,
        ] {
            let mut rng = GardenRng::new(0xB0AB);
            let tree = generate_tree(species, &mut rng);
            let verts = build_culled_voxel_mesh(&tree.bark, 1.0).count_vertices()
                + build_culled_voxel_mesh(&tree.foliage, 1.0).count_vertices();
            assert!(
                verts < 900_000,
                "{species:?} meshes have {verts} vertices — VRAM risk"
            );
        }
    }
}
