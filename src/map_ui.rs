use bevy::prelude::*;

use crate::australia::{biome_at_geo, biome_at_world, biome_display_name, is_land, AussieBiome};
use crate::world::{geo_to_normalized, world_to_geo};

/// Grid resolution for the little pixel-art continent — fine enough that the
/// real coastline polygon (gulfs, capes, Tasmania) actually reads.
const MAP_COLS: usize = 88;
const MAP_ROWS: usize = 72;

/// UI colour for each biome on the overlay silhouette.
fn biome_map_color(biome: AussieBiome) -> Color {
    match biome {
        AussieBiome::Ocean => Color::srgb(0.15, 0.45, 0.72),
        AussieBiome::TropicalSavanna => Color::srgb(0.62, 0.66, 0.28),
        AussieBiome::AridOutback => Color::srgb(0.78, 0.44, 0.22),
        AussieBiome::Pilbara => Color::srgb(0.70, 0.34, 0.24),
        AussieBiome::Mediterranean => Color::srgb(0.56, 0.60, 0.30),
        AussieBiome::TemperateForest => Color::srgb(0.26, 0.46, 0.26),
        AussieBiome::CoastalBush => Color::srgb(0.40, 0.56, 0.30),
        AussieBiome::Tasmania => Color::srgb(0.32, 0.52, 0.42),
    }
}

/// Invert [`geo_to_normalized`] so a grid cell maps back to lat/lon.
fn cell_to_geo(u: f32, v: f32) -> (f32, f32) {
    let lon = 113.0 + u * 40.5;
    let lat = -10.0 - v * 34.5;
    (lat, lon)
}

#[derive(Resource)]
pub struct MapOverlay {
    pub visible: bool,
    pub root: Option<Entity>,
}

impl Default for MapOverlay {
    fn default() -> Self {
        Self {
            visible: false,
            root: None,
        }
    }
}

#[derive(Component)]
pub(crate) struct MapHudRoot;

#[derive(Component)]
pub(crate) struct MapPlayerDot;

#[derive(Component)]
pub(crate) struct MapBiomeLabel;

pub fn setup_map_ui(mut commands: Commands, mut overlay: ResMut<MapOverlay>) {
    let root = commands
        .spawn((
            MapHudRoot,
            Node {
                width: Val::Percent(100.0),
                height: Val::Percent(100.0),
                justify_content: JustifyContent::Center,
                align_items: AlignItems::Center,
                ..default()
            },
            Visibility::Hidden,
        ))
        .with_children(|panel| {
            panel
                .spawn((
                    Node {
                        width: Val::Px(420.0),
                        height: Val::Px(340.0),
                        flex_direction: FlexDirection::Column,
                        padding: UiRect::all(Val::Px(12.0)),
                        border: UiRect::all(Val::Px(2.0)),
                        ..default()
                    },
                    BackgroundColor(Color::srgba(0.08, 0.10, 0.14, 0.92)),
                    BorderColor(Color::srgb(0.35, 0.42, 0.50)),
                ))
                .with_children(|card| {
                    card.spawn((
                        Text::new("Australia — you are here (probably)"),
                        TextFont {
                            font_size: 15.0,
                            ..default()
                        },
                        TextColor(Color::srgb(0.85, 0.90, 0.95)),
                    ));

                    card.spawn((
                        Text::new("Press M to close — useless at worm scale"),
                        TextFont {
                            font_size: 11.0,
                            ..default()
                        },
                        TextColor(Color::srgb(0.55, 0.62, 0.70)),
                        Node {
                            margin: UiRect::bottom(Val::Px(8.0)),
                            ..default()
                        },
                    ));

                    card.spawn((
                        Node {
                            width: Val::Px(380.0),
                            height: Val::Px(240.0),
                            border: UiRect::all(Val::Px(1.0)),
                            ..default()
                        },
                        BackgroundColor(Color::srgb(0.15, 0.45, 0.72)),
                        BorderColor(Color::srgb(0.30, 0.55, 0.78)),
                    ))
                    .with_children(|map_box| {
                        // Pixel-art continent: one small cell per land sample,
                        // coloured by biome. Ocean cells are skipped so the blue
                        // box shows through as the sea.
                        let cell_w = 100.0 / MAP_COLS as f32;
                        let cell_h = 100.0 / MAP_ROWS as f32;
                        for row in 0..MAP_ROWS {
                            for col in 0..MAP_COLS {
                                let u = (col as f32 + 0.5) / MAP_COLS as f32;
                                let v = (row as f32 + 0.5) / MAP_ROWS as f32;
                                let (lat, lon) = cell_to_geo(u, v);
                                if !is_land(lat, lon) {
                                    continue;
                                }
                                let biome = biome_at_geo(lat, lon);
                                map_box.spawn((
                                    Node {
                                        width: Val::Percent(cell_w + 0.4),
                                        height: Val::Percent(cell_h + 0.4),
                                        position_type: PositionType::Absolute,
                                        left: Val::Percent(col as f32 * cell_w),
                                        top: Val::Percent(row as f32 * cell_h),
                                        ..default()
                                    },
                                    BackgroundColor(biome_map_color(biome)),
                                ));
                            }
                        }

                        map_box.spawn((
                            MapPlayerDot,
                            Node {
                                width: Val::Px(4.0),
                                height: Val::Px(4.0),
                                position_type: PositionType::Absolute,
                                left: Val::Percent(50.0),
                                top: Val::Percent(50.0),
                                ..default()
                            },
                            BackgroundColor(Color::srgb(1.0, 0.15, 0.15)),
                        ));
                    });

                    card.spawn((
                        MapBiomeLabel,
                        Text::new("Biome: —"),
                        TextFont {
                            font_size: 12.0,
                            ..default()
                        },
                        TextColor(Color::srgb(0.75, 0.82, 0.72)),
                        Node {
                            margin: UiRect::top(Val::Px(8.0)),
                            ..default()
                        },
                    ));
                });
        })
        .id();

    overlay.root = Some(root);
}

pub fn toggle_map_ui(
    keyboard: Res<ButtonInput<KeyCode>>,
    mut overlay: ResMut<MapOverlay>,
    mut visibility: Query<&mut Visibility, With<MapHudRoot>>,
) {
    if !keyboard.just_pressed(KeyCode::KeyM) {
        return;
    }
    overlay.visible = !overlay.visible;
    let Ok(mut vis) = visibility.get_single_mut() else {
        return;
    };
    *vis = if overlay.visible {
        Visibility::Visible
    } else {
        Visibility::Hidden
    };
}

pub fn update_map_ui(
    cam_q: Query<&Transform, With<Camera>>,
    mut dot_q: Query<&mut Node, With<MapPlayerDot>>,
    mut label_q: Query<&mut Text, With<MapBiomeLabel>>,
) {
    let Ok(cam) = cam_q.get_single() else {
        return;
    };

    let (lat, lon) = world_to_geo(cam.translation.x, cam.translation.z);
    let norm = geo_to_normalized(lat, lon);

    if let Ok(mut dot) = dot_q.get_single_mut() {
        dot.left = Val::Percent((norm.x * 100.0).clamp(1.0, 99.0));
        dot.top = Val::Percent((norm.y * 100.0).clamp(1.0, 99.0));
    }

    if let Ok(mut label) = label_q.get_single_mut() {
        let biome = biome_at_world(cam.translation.x, cam.translation.z);
        let land = if is_land(lat, lon) { "land" } else { "ocean" };
        **label = format!(
            "Biome: {} ({land})  |  {:.2}°S {:.2}°E",
            biome_display_name(biome),
            -lat,
            lon
        );
    }
}