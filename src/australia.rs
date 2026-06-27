use crate::terrain::OreType;
use crate::world::{world_to_geo, AUSTRALIA_CENTER_LAT, AUSTRALIA_CENTER_LON};

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
    pub eucalyptus_weight: f32,
    pub acacia_weight: f32,
    pub tree_density: f32,
    pub ore_weights: [(OreType, f32); 8],
}

/// Rough land mask — mainland + Tasmania. Ocean returns false.
pub fn is_land(lat: f32, lon: f32) -> bool {
    if lon < 112.5 || lon > 153.8 || lat > -9.0 || lat < -44.5 {
        return false;
    }

    // Tasmania
    let tdl = (lon - 146.5) / 2.2;
    let tda = (lat + 41.5) / 1.6;
    if tdl * tdl + tda * tda < 1.0 {
        return true;
    }

    // Gulf of Carpentaria indentation
    if lat > -17.5 && lat < -12.0 && lon > 136.0 && lon < 142.5 {
        return false;
    }
    // Great Australian Bight curve
    if lat < -32.0 && lon < 125.0 {
        let bdl = (lon - 122.0) / 6.0;
        let bda = (lat + 35.0) / 5.0;
        if bdl * bdl + bda * bda > 1.0 {
            return false;
        }
    }

    // Mainland ellipsoid with east-coast bulge
    let lon_c = AUSTRALIA_CENTER_LON;
    let lat_c = AUSTRALIA_CENTER_LAT;
    let east_boost = if lon > lon_c { 1.12 } else { 0.92 };
    let dl = (lon - lon_c) / (19.0 * east_boost);
    let da = (lat - lat_c) / 13.5;
    dl * dl + da * da < 1.0
}

pub fn biome_at_world(world_x: f32, world_z: f32) -> AussieBiome {
    let (lat, lon) = world_to_geo(world_x, world_z);
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
                eucalyptus_weight: 0.0,
                acacia_weight: 0.0,
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

    let (euc, wattle, density) = match biome {
        AussieBiome::TropicalSavanna => (0.45, 0.55, 1.1),
        AussieBiome::AridOutback => (0.35, 0.65, 0.35),
        AussieBiome::Pilbara => (0.70, 0.30, 0.25),
        AussieBiome::Mediterranean => (0.85, 0.15, 0.9),
        AussieBiome::TemperateForest => (0.80, 0.20, 1.2),
        AussieBiome::CoastalBush => (0.65, 0.35, 1.0),
        AussieBiome::Tasmania => (0.75, 0.25, 1.3),
        AussieBiome::Ocean => (0.0, 0.0, 0.0),
    };

    BiomeProfile {
        biome,
        eucalyptus_weight: euc,
        acacia_weight: wattle,
        tree_density: density,
        ore_weights: ores,
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