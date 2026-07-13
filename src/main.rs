mod audio;
mod australia;
mod chunk_store;
mod distance_blur;
mod foliage;
mod grass;
mod leaves;
mod map_ui;
mod silhouettes;
mod sky;
mod streaming;
mod terrain;
mod topography;
mod trees;
mod weather;
mod world;
mod worm;

use bevy::prelude::*;
use bevy::render::RenderPlugin;
use bevy::render::settings::{RenderCreation, WgpuSettings};
use bevy::image::{ImageAddressMode, ImageSamplerDescriptor};
use bevy::render::texture::ImagePlugin;
use bevy_flycam::prelude::*;
use std::time::{SystemTime, UNIX_EPOCH};

use audio::GameAudioPlugin;
use australia::{biome_at_world, biome_display_name, pick_coastal_spawn};
use chunk_store::ChunkArchive;
use map_ui::{setup_map_ui, toggle_map_ui, update_map_ui, MapOverlay};
use terrain::TerrainPlugin;
use distance_blur::DistanceBlurPlugin;
use worm::{finish_burrow_tasks, GodMode, WormPlugin, GOD_SPEED_MULT, WORM_SPEED};
use streaming::{
    finalize_deferred_unloads, finish_chunk_tasks, plan_chunk_streaming, process_chunk_load_queue,
    ChunkWorld,
};
use world::*;
fn main() {
    let god_mode = GodMode::from_env();
    let start_speed = if god_mode.enabled {
        WORM_SPEED * GOD_SPEED_MULT
    } else {
        WORM_SPEED
    };

    App::new()
        .add_plugins(DefaultPlugins
            // Nearest for the pixel-art look; Repeat so block-skin UVs that run
            // 0..len across merged voxel strips tile instead of smearing.
            .set(ImagePlugin {
                default_sampler: ImageSamplerDescriptor {
                    address_mode_u: ImageAddressMode::Repeat,
                    address_mode_v: ImageAddressMode::Repeat,
                    address_mode_w: ImageAddressMode::Repeat,
                    ..ImageSamplerDescriptor::nearest()
                },
            })
            .set(WindowPlugin {
                primary_window: Some(Window {
                    title: "gardn".into(),
                    resolution: (1280., 720.).into(),
                    ..default()
                }),
                ..default()
            })
            .set(RenderPlugin {
                // Force Vulkan backend — much more stable than DirectX 12 on many Windows machines
                render_creation: RenderCreation::Automatic(WgpuSettings {
                    backends: Some(wgpu::Backends::VULKAN),
                    ..default()
                }),
                ..default()
            })
        )
        .add_plugins(PlayerPlugin) // Adds WASD + mouse look camera automatically
        .add_plugins(DistanceBlurPlugin) // Foreground-sharp, distance-soft blur
        .add_plugins(TerrainPlugin) // Shared terrain material
        // --- PHASE 0: ecology stripped (trees, grass, leaves, weather, day/night
        //     sky). Preserved in git tag v0-full-ecology; re-layered in Phase 1+.
        //     GrassPlugin / LeavesPlugin / WeatherPlugin / FoliagePlugin / SkyPlugin.
        .add_plugins(GameAudioPlugin) // Sound-effect handles + music rotation
        .add_plugins(WormPlugin) // Worm gravity/collision, god mode, camera, eating
        .insert_resource(MovementSettings {
            sensitivity: 0.00012,
            speed: start_speed, // Slow crawl — we're a tiny worm (3× in god mode)
            ..default()
        })
        .insert_resource(ClearColor(Color::srgb(0.58, 0.72, 0.88))) // Soft garden sky
        .insert_resource(god_mode)
        .init_resource::<ChunkWorld>()
        .init_resource::<ChunkArchive>()
        .init_resource::<MapOverlay>()
        .add_systems(
            Startup,
            (choose_spawn_location, setup_garden, setup_map_ui).chain(),
        )
        // The world-structure systems are chained: each one then sees the
        // previous one's spawns/despawns actually applied. Unordered, a chunk
        // unload could race an eat/tree-finish touching entities in that chunk
        // and double-despawn them (Bevy's B0003 warning).
        .add_systems(
            Update,
            (
                plan_chunk_streaming,
                process_chunk_load_queue,
                finish_chunk_tasks,
                // PHASE 0: tree build + tree-silhouette LOD + leaf-eating stripped.
                finalize_deferred_unloads,
                finish_burrow_tasks,
            )
                .chain(),
        )
        .add_systems(
            Update,
            (
                toggle_map_ui,
                update_map_ui,
            ),
        )
        .add_systems(PostStartup, plan_chunk_streaming)
        .run();
}

/// PHASE 0 terrain sandbox setup: just a static sun. The full day/night sky,
/// grass, leaves, trees, and weather were stripped (tag v0-full-ecology) so the
/// terrain foundation stands alone; they re-layer in Phase 1+.
fn setup_garden(mut commands: Commands) {
    println!("🐛 Phase 0 terrain — WASD crawl · Space stretch/reach · E burrow · M map · G god mode (flight)");

    // A plain fixed sun so the bare terrain is lit and casts shadow — the real
    // celestial system lives in SkyPlugin (stripped with the ecology).
    commands.spawn((
        DirectionalLight {
            illuminance: 11_000.0,
            shadows_enabled: true,
            ..default()
        },
        Transform::from_xyz(0.4, 1.0, 0.25).looking_at(Vec3::ZERO, Vec3::Y),
    ));
    commands.insert_resource(AmbientLight {
        color: Color::srgb(0.85, 0.92, 1.0),
        brightness: 380.0,
    });
}

/// Pick a fresh spot on a green stretch of coastline for this launch and pin it
/// to world origin — every new game starts on a different beach with the ocean
/// in view. Must run before any terrain/biome sampling.
fn choose_spawn_location() {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    let seed = nanos ^ WORLD_SEED ^ 0x5DEE_CE66_A55A_1234;

    let (lat, lon) = pick_coastal_spawn(seed);
    set_spawn_geo_offset(geo_to_world_offset(lat, lon));

    // Now that the offset is set, world origin reports the spawn biome.
    let biome = biome_at_world(0.0, 0.0);
    println!(
        "🌏 New game — the little worm washes up on the {} coast ({:.2}°S {:.2}°E).",
        biome_display_name(biome),
        -lat,
        lon
    );
}
