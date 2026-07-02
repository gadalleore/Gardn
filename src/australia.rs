use crate::terrain::OreType;
use crate::trees::TreeSpecies;
use crate::world::{
    world_to_geo, GardenRng, AUSTRALIA_CENTER_LAT, AUSTRALIA_CENTER_LON, AUSTRALIA_HEIGHT_FT,
    AUSTRALIA_WIDTH_FT,
};

/// Simplified climate / ecology zones aligned with real Australian regions.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AussieBiome {
    Ocean,
    TropicalSavanna,  // Top End, Cape York — wet-dry tropics
    AridOutback,      // Red centre, SA outback
    Pilbara,          // WA iron province
    Mediterranean,    // SW WA — jarrah/karri country
    TemperateForest,  // SE coast — Great Dividing Range
    CoastalBush,      // East coast littoral
    Tasmania,         // Cool temperate island
}

pub struct BiomeProfile {
    pub biome: AussieBiome,
    /// Native species and their rough share of the region's tree cover.
    pub tree_mix: &'static [(TreeSpecies, f32)],
    pub tree_density: f32,
    pub ore_weights: [(OreType, f32); 8],
}

/// Species mixes per biome — proportions loosely track what actually dominates
/// each region's woodland (e.g. mulga over most of the arid zone, jarrah/karri
/// in the SW, mountain ash and tree ferns in the wet SE forests).
const SAVANNA_MIX: &[(TreeSpecies, f32)] = &[
    (TreeSpecies::RiverRedGum, 0.30),
    (TreeSpecies::Paperbark, 0.28),
    (TreeSpecies::Boab, 0.14),
    (TreeSpecies::GoldenWattle, 0.28),
];
const OUTBACK_MIX: &[(TreeSpecies, f32)] = &[
    (TreeSpecies::Mulga, 0.42),
    (TreeSpecies::DesertOak, 0.22),
    (TreeSpecies::GhostGum, 0.28),
    (TreeSpecies::GoldenWattle, 0.08),
];
const PILBARA_MIX: &[(TreeSpecies, f32)] = &[
    (TreeSpecies::SnappyGum, 0.42),
    (TreeSpecies::GhostGum, 0.25),
    (TreeSpecies::Mulga, 0.33),
];
const MEDITERRANEAN_MIX: &[(TreeSpecies, f32)] = &[
    (TreeSpecies::Jarrah, 0.40),
    (TreeSpecies::Karri, 0.28),
    (TreeSpecies::Banksia, 0.16),
    (TreeSpecies::GoldenWattle, 0.16),
];
const TEMPERATE_MIX: &[(TreeSpecies, f32)] = &[
    (TreeSpecies::MountainAsh, 0.28),
    (TreeSpecies::RiverRedGum, 0.26),
    (TreeSpecies::SnowGum, 0.14),
    (TreeSpecies::TreeFern, 0.16),
    (TreeSpecies::GoldenWattle, 0.16),
];
const COASTAL_MIX: &[(TreeSpecies, f32)] = &[
    (TreeSpecies::RiverRedGum, 0.28),
    (TreeSpecies::Banksia, 0.24),
    (TreeSpecies::Paperbark, 0.18),
    (TreeSpecies::MoretonBayFig, 0.10),
    (TreeSpecies::GoldenWattle, 0.20),
];
const TASMANIA_MIX: &[(TreeSpecies, f32)] = &[
    (TreeSpecies::MyrtleBeech, 0.24),
    (TreeSpecies::HuonPine, 0.18),
    (TreeSpecies::MountainAsh, 0.22),
    (TreeSpecies::TreeFern, 0.22),
    (TreeSpecies::SnowGum, 0.14),
];

/// Mainland coastline as (lon, lat) vertices, walked clockwise from Cape York:
/// down the east coast, around Victoria and the SA gulf country, across the
/// Bight, up the west coast, through the Kimberley and Top End, and around the
/// Gulf of Carpentaria back to the Cape.
const MAINLAND_COAST: &[(f32, f32)] = &[
    (142.6, -10.7), // Cape York
    (143.1, -11.9),
    (143.6, -14.2),
    (144.6, -14.3),
    (145.3, -15.0),
    (145.5, -16.1), // Cairns
    (146.1, -17.7),
    (147.4, -19.4), // Townsville
    (148.8, -20.3),
    (149.7, -22.4),
    (150.8, -23.6), // Rockhampton
    (152.5, -25.3), // Fraser coast
    (153.2, -27.0), // Brisbane
    (153.6, -28.7), // Cape Byron (easternmost)
    (152.9, -31.2),
    (151.8, -32.9), // Newcastle
    (151.3, -33.9), // Sydney
    (150.1, -35.9),
    (150.0, -37.5), // Cape Howe
    (147.9, -37.9), // Gippsland
    (146.4, -39.1), // Wilsons Promontory
    (145.0, -38.3), // Port Phillip
    (143.5, -38.9), // Cape Otway
    (141.4, -38.4), // Portland
    (139.8, -37.3),
    (139.7, -36.0), // The Coorong
    (138.6, -35.7), // Fleurieu Peninsula
    (138.5, -34.6), // Gulf St Vincent
    (138.1, -34.3),
    (137.8, -35.1), // Yorke Peninsula toe
    (137.4, -34.0), // Spencer Gulf
    (137.8, -33.0), // head of Spencer Gulf
    (136.9, -33.7),
    (136.1, -34.9), // Eyre Peninsula tip
    (135.4, -34.6),
    (134.8, -33.3), // Streaky Bay
    (133.6, -32.2),
    (131.4, -31.6), // Head of Bight
    (129.0, -31.7), // Nullarbor cliffs
    (127.0, -32.1),
    (124.3, -33.0),
    (123.2, -34.0), // Esperance
    (121.5, -33.9),
    (119.4, -34.5),
    (118.0, -35.1), // Albany
    (116.0, -34.9),
    (115.1, -34.4), // Cape Leeuwin
    (115.5, -33.3),
    (115.7, -32.1), // Perth
    (115.1, -30.5),
    (114.6, -28.8), // Geraldton
    (113.4, -26.2), // Shark Bay
    (113.5, -24.9), // Carnarvon
    (114.1, -21.8), // North West Cape
    (115.9, -21.1),
    (117.2, -20.7), // Karratha
    (119.0, -20.0), // Port Hedland
    (121.0, -19.6), // Eighty Mile Beach
    (122.2, -18.1), // Broome
    (123.0, -16.5),
    (123.5, -16.1), // King Sound
    (124.4, -16.3),
    (125.0, -15.4),
    (126.0, -14.5), // Kimberley coast
    (127.2, -13.8),
    (128.1, -15.2), // Joseph Bonaparte Gulf
    (129.1, -14.9),
    (129.6, -13.6),
    (130.2, -12.9),
    (130.8, -12.4), // Darwin
    (131.8, -12.2),
    (132.5, -11.5), // Cobourg Peninsula
    (133.7, -11.9),
    (135.2, -12.1), // Arnhem Land
    (136.6, -11.9),
    (136.9, -12.8), // Gove
    (135.9, -13.8),
    (135.7, -15.0), // west Gulf of Carpentaria
    (136.5, -15.9),
    (137.6, -16.4),
    (139.1, -17.6),
    (140.4, -17.7), // bottom of the Gulf
    (141.3, -16.5),
    (141.5, -14.5), // east Gulf coast
    (141.9, -12.5),
    (142.2, -11.3),
];

/// Tasmania, clockwise from the northwest tip.
const TASMANIA_COAST: &[(f32, f32)] = &[
    (144.7, -40.8),
    (145.8, -40.9),
    (146.8, -41.1),
    (147.9, -40.8),
    (148.3, -41.6),
    (148.3, -42.3),
    (147.9, -43.0),
    (147.0, -43.6),
    (146.0, -43.5),
    (145.2, -42.6),
    (144.7, -41.6),
];

/// Even-odd ray-cast point-in-polygon; poly points are (lon, lat).
fn point_in_poly(lat: f32, lon: f32, poly: &[(f32, f32)]) -> bool {
    let mut inside = false;
    let mut j = poly.len() - 1;
    for i in 0..poly.len() {
        let (xi, yi) = poly[i];
        let (xj, yj) = poly[j];
        if (yi > lat) != (yj > lat) && lon < (xj - xi) * (lat - yi) / (yj - yi) + xi {
            inside = !inside;
        }
        j = i;
    }
    inside
}

/// Land mask from a real (if simplified) coastline polygon — Cape York, the
/// Gulf of Carpentaria, the Kimberley, the Bight, the SA gulfs, the east-coast
/// bulge, and Tasmania are all where they should be.
pub fn is_land(lat: f32, lon: f32) -> bool {
    // Cheap bounding box first.
    if !(112.5..=154.0).contains(&lon) || !(-44.0..=-10.5).contains(&lat) {
        return false;
    }
    point_in_poly(lat, lon, MAINLAND_COAST) || point_in_poly(lat, lon, TASMANIA_COAST)
}

pub fn biome_at_world(world_x: f32, world_z: f32) -> AussieBiome {
    let (lat, lon) = world_to_geo(world_x, world_z);
    biome_at_geo(lat, lon)
}

/// Biome purely from geographic coordinates — used both by world generation
/// (via [`biome_at_world`]) and by the map overlay, which wants the absolute
/// continent regardless of where this game's origin was placed.
pub fn biome_at_geo(lat: f32, lon: f32) -> AussieBiome {
    if !is_land(lat, lon) {
        return AussieBiome::Ocean;
    }

    // Tasmania
    if lon > 144.5 && lon < 148.5 && lat < -40.5 && lat > -43.8 {
        return AussieBiome::Tasmania;
    }

    // Tropical north
    if lat > -20.0 {
        return AussieBiome::TropicalSavanna;
    }

    // Pilbara / WA iron belt
    if lon < 122.0 && lat > -28.0 && lat < -18.0 {
        return AussieBiome::Pilbara;
    }

    // Mediterranean southwest
    if lon < 125.0 && lat < -28.0 && lat > -35.0 {
        return AussieBiome::Mediterranean;
    }

    // Temperate southeast
    if lon > 140.0 && lat < -28.0 {
        return AussieBiome::TemperateForest;
    }

    // East coast strip
    if lon > 148.0 && lat > -32.0 {
        return AussieBiome::CoastalBush;
    }

    // Arid interior (most of the continent by area)
    if lat < -22.0 && lon > 118.0 && lon < 145.0 {
        return AussieBiome::AridOutback;
    }

    AussieBiome::CoastalBush
}

/// Real-world-ish proportions: iron & bauxite dominate Australian mining output.
fn base_ore_weights() -> [(OreType, f32); 8] {
    [
        (OreType::Iron, 0.38),
        (OreType::Bauxite, 0.22),
        (OreType::Coal, 0.14),
        (OreType::Copper, 0.10),
        (OreType::Gold, 0.05),
        (OreType::Uranium, 0.04),
        (OreType::LeadZinc, 0.04),
        (OreType::Opal, 0.03),
    ]
}

pub fn biome_profile(world_x: f32, world_z: f32) -> BiomeProfile {
    let biome = biome_at_world(world_x, world_z);
    let mut ores = base_ore_weights();

    match biome {
        AussieBiome::Ocean => {
            return BiomeProfile {
                biome,
                tree_mix: &[],
                tree_density: 0.0,
                ore_weights: ores,
            };
        }
        AussieBiome::Pilbara => {
            ores[0].1 *= 3.5; // iron heartland
            ores[1].1 *= 0.5;
        }
        AussieBiome::TropicalSavanna => {
            ores[1].1 *= 2.8; // Weipa bauxite
            ores[0].1 *= 0.6;
        }
        AussieBiome::AridOutback => {
            ores[7].1 *= 4.0; // Coober Pedy opal
            ores[4].1 *= 1.8; // gold
            ores[0].1 *= 1.2;
        }
        AussieBiome::TemperateForest | AussieBiome::CoastalBush => {
            ores[2].1 *= 2.2; // NSW/Vic coal
            ores[4].1 *= 1.5;
        }
        AussieBiome::Mediterranean => {
            ores[0].1 *= 1.4;
            ores[3].1 *= 1.3; // nickel/copper belts
        }
        AussieBiome::Tasmania => {
            ores[2].1 *= 1.5;
            ores[3].1 *= 1.2;
        }
    }

    // Forest biomes are dense but not closed-canopy — enough gap between
    // crowns for sunlight to break through. Deserts stay sparse for contrast.
    let (tree_mix, density): (&'static [(TreeSpecies, f32)], f32) = match biome {
        AussieBiome::TropicalSavanna => (SAVANNA_MIX, 1.2),
        AussieBiome::AridOutback => (OUTBACK_MIX, 0.35),
        AussieBiome::Pilbara => (PILBARA_MIX, 0.25),
        AussieBiome::Mediterranean => (MEDITERRANEAN_MIX, 1.8),
        AussieBiome::TemperateForest => (TEMPERATE_MIX, 2.2),
        AussieBiome::CoastalBush => (COASTAL_MIX, 1.6),
        AussieBiome::Tasmania => (TASMANIA_MIX, 2.4),
        AussieBiome::Ocean => (&[], 0.0),
    };

    BiomeProfile {
        biome,
        tree_mix,
        tree_density: density,
        ore_weights: ores,
    }
}

/// Spawn on the coastline, always: walk a random ray from the continent centre
/// out to the land/ocean boundary (binary search on the land mask), then step
/// ~80 ft back inland so the worm wakes up on the beach with the sea in view.
/// Directions that hit arid shoreline (Pilbara coast, the Nullarbor) are
/// re-rolled — the interesting green coasts only.
pub fn pick_coastal_spawn(seed: u64) -> (f32, f32) {
    let mut rng = GardenRng::new(seed);
    let ft_per_deg_lon = AUSTRALIA_WIDTH_FT / 40.0;
    let ft_per_deg_lat = AUSTRALIA_HEIGHT_FT / 34.0;

    for _ in 0..64 {
        let theta = rng.range(0.0, std::f32::consts::TAU);
        let dl = theta.cos();
        let da = theta.sin();

        // The centre is land and 25° out is always open ocean; bisect the
        // crossing. (Rays through the Gulf of Carpentaria just find the gulf's
        // own shore — still a coast.)
        let mut lo = 0.0f32;
        let mut hi = 25.0f32;
        if is_land(
            AUSTRALIA_CENTER_LAT + da * hi,
            AUSTRALIA_CENTER_LON + dl * hi,
        ) {
            continue;
        }
        for _ in 0..40 {
            let mid = (lo + hi) * 0.5;
            if is_land(
                AUSTRALIA_CENTER_LAT + da * mid,
                AUSTRALIA_CENTER_LON + dl * mid,
            ) {
                lo = mid;
            } else {
                hi = mid;
            }
        }

        // Step back inland ~80 ft along the ray (converted to degrees).
        let ray_ft_per_deg =
            ((dl * ft_per_deg_lon).powi(2) + (da * ft_per_deg_lat).powi(2)).sqrt();
        let t = lo - 80.0 / ray_ft_per_deg;
        let lat = AUSTRALIA_CENTER_LAT + da * t;
        let lon = AUSTRALIA_CENTER_LON + dl * t;

        if !is_land(lat, lon) {
            continue;
        }
        match biome_at_geo(lat, lon) {
            AussieBiome::TropicalSavanna
            | AussieBiome::Mediterranean
            | AussieBiome::TemperateForest
            | AussieBiome::CoastalBush => return (lat, lon),
            _ => continue,
        }
    }

    // Practically unreachable fallback: a lush spot on the east coast.
    (-27.0, 152.8)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every spawn must be on land, in a lush biome, with open ocean a short
    /// worm-trek away — the coast should be visible from the starting beach.
    #[test]
    fn coastal_spawn_is_green_land_near_water() {
        let deg_per_ft_lon = 40.0 / AUSTRALIA_WIDTH_FT;
        let deg_per_ft_lat = 34.0 / AUSTRALIA_HEIGHT_FT;

        for seed in 0..40u64 {
            let (lat, lon) = pick_coastal_spawn(seed.wrapping_mul(0x9E37_79B9) ^ 0xC0A57);
            assert!(is_land(lat, lon), "seed {seed}: spawn not on land");

            let biome = biome_at_geo(lat, lon);
            assert!(
                matches!(
                    biome,
                    AussieBiome::TropicalSavanna
                        | AussieBiome::Mediterranean
                        | AussieBiome::TemperateForest
                        | AussieBiome::CoastalBush
                ),
                "seed {seed}: spawned in {biome:?}"
            );

            // Ocean within ~250 ft in at least one compass direction.
            let r_ft = 250.0;
            let near_water = [
                (0.0, r_ft),
                (0.0, -r_ft),
                (r_ft, 0.0),
                (-r_ft, 0.0),
            ]
            .iter()
            .any(|(de, dn)| {
                !is_land(lat + dn * deg_per_ft_lat, lon + de * deg_per_ft_lon)
            });
            assert!(near_water, "seed {seed}: no ocean near ({lat}, {lon})");
        }
    }
}

pub fn biome_display_name(biome: AussieBiome) -> &'static str {
    match biome {
        AussieBiome::Ocean => "Ocean",
        AussieBiome::TropicalSavanna => "Tropical Savanna",
        AussieBiome::AridOutback => "Arid Outback",
        AussieBiome::Pilbara => "Pilbara",
        AussieBiome::Mediterranean => "Mediterranean SW",
        AussieBiome::TemperateForest => "Temperate SE",
        AussieBiome::CoastalBush => "Coastal Bush",
        AussieBiome::Tasmania => "Tasmania",
    }
}
#[cfg(test)]
mod shape_preview {
    use super::*;
    #[test]
    #[ignore]
    fn ascii_australia() {
        for row in 0..36 {
            let mut line = String::new();
            for col in 0..80 {
                let lon = 112.0 + col as f32 / 80.0 * 43.0;
                let lat = -9.5 - row as f32 / 36.0 * 35.5;
                line.push(if is_land(lat, lon) { '#' } else { '.' });
            }
            println!("{line}");
        }
    }
}
