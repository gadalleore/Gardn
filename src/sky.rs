//! Day/night sky: a 24-hour clock that walks the sun (and the full moon
//! opposite it) across the sky, retinting the sky colour, fog, and ambient
//! light, and swinging the two directional lights + their visible discs.
//! Sundown grades blue → gold → orange → violet → moonlit night, the sun ball
//! itself blushes at the horizon, and a starfield wheels overhead after dark.
//! `SkyPlugin` owns the clock, the celestial entities, and the update. Its one
//! published item is [`SkyClock`] — the day fraction + day count, read by
//! weather's seasons and morning-fog windows so both modules share the same
//! clock (including the GARDN_HOUR / GARDN_DAY_SECS dev knobs).

use bevy::pbr::{CascadeShadowConfigBuilder, DistanceFog, NotShadowCaster};
use bevy::prelude::*;
use bevy::render::view::RenderLayers;

use crate::world::GardenRng;

pub struct SkyPlugin;

impl Plugin for SkyPlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(DayCycle::from_env())
            .init_resource::<SkyClock>()
            .insert_resource(SunDirection(Vec3::new(0.6, 0.6, 0.35).normalize()))
            .add_systems(Startup, setup_sky)
            .add_systems(Update, update_day_cycle);
    }
}

/// One full sun cycle every 24 real hours. The clock starts at
/// [`GAME_START_HOUR`] when the app launches; GARDN_HOUR=<0-24> overrides the
/// starting hour (e.g. `GARDN_HOUR=0` to see the moonlit night right away),
/// and GARDN_DAY_SECS=<secs> compresses the whole cycle (dev knob — e.g.
/// `GARDN_DAY_SECS=120` sweeps dawn-to-dawn in two minutes to eyeball the
/// colour grading).
const DAY_LENGTH_SECS: f32 = 24.0 * 3600.0;
const GAME_START_HOUR: f32 = 8.0;
/// How many stars the night dome carries. All share one mesh + one material,
/// so the count is cheap; it just needs to feel like a sky, not a scatter.
const STAR_COUNT: usize = 220;
/// How far from the camera the sun/moon discs float — past the fog's end
/// (780 ft) but inside the far clip (900 ft); unlit materials skip fog, so they
/// burn through and read as sky, not scenery.
const CELESTIAL_DISTANCE_FT: f32 = 850.0;

#[derive(Resource)]
struct DayCycle {
    start_frac: f32,
    /// Real seconds per full cycle — [`DAY_LENGTH_SECS`] unless GARDN_DAY_SECS
    /// shrinks it for testing.
    day_secs: f32,
}

impl DayCycle {
    fn from_env() -> Self {
        let hour = std::env::var("GARDN_HOUR")
            .ok()
            .and_then(|v| v.trim().parse::<f32>().ok())
            .unwrap_or(GAME_START_HOUR);
        let day_secs = std::env::var("GARDN_DAY_SECS")
            .ok()
            .and_then(|v| v.trim().parse::<f32>().ok())
            .filter(|s| *s > 1.0)
            .unwrap_or(DAY_LENGTH_SECS);
        println!("🕗 Day cycle: starting the 24-hour clock at {hour:.1}h ({day_secs:.0}s per day).");
        Self {
            start_frac: (hour / 24.0).rem_euclid(1.0),
            day_secs,
        }
    }
}

/// The game-time clock, published for the weather systems: `frac` is the
/// fraction of the 24-h day (0 = midnight, 0.25 = 6:00, 0.5 = noon) and `day`
/// counts completed days since launch — seasons divide it, fog windows read
/// it. Kept here so GARDN_HOUR / GARDN_DAY_SECS steer every consumer at once.
#[derive(Resource, Default)]
pub(crate) struct SkyClock {
    pub(crate) frac: f32,
    pub(crate) day: u32,
}

/// A directional light driven around the sky by the day cycle.
#[derive(Component)]
struct CelestialLight {
    is_sun: bool,
}

/// Current to-sun direction, updated by the day cycle (kept for shadow-facing
/// features to read).
#[derive(Resource)]
struct SunDirection(Vec3);

/// The visible unlit disc for a celestial body, re-anchored to the camera each
/// frame so it always hangs at the same sky position.
#[derive(Component)]
struct CelestialDisc {
    is_sun: bool,
}

/// Parent of all the star sprites. Re-anchored to the camera and rotated with
/// the sun's orbital angle each frame, so the whole field rises and sets as one
/// rigid dome — stars wheel across the night instead of hanging frozen.
#[derive(Component)]
struct StarDome;

/// Handles to the sky materials retinted per frame: the sun disc blushes at the
/// horizon and the shared star material fades with twilight.
#[derive(Resource)]
struct SkyMaterials {
    sun_disc: Handle<StandardMaterial>,
    stars: Handle<StandardMaterial>,
}

/// Spawn the sun/moon directional lights and their glowing discs, plus the base
/// ambient light. The day cycle steers everything from here each frame.
fn setup_sky(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    // Sun light — shadows on, so open canopies throw dappled light shafts onto
    // the forest floor. Tight first cascade keeps shadow detail crisp at worm
    // eye level; the far cascades cover the giants overhead. The day cycle
    // steers its direction, colour, and strength every frame.
    // Layer 1 holds the invisible sun-facing shadow planes of the horizon
    // cutouts: the camera (layer 0) never draws them, but the lights see both
    // layers, so distant cutout trees still throw tree-shaped shadows.
    commands.spawn((
        CelestialLight { is_sun: true },
        DirectionalLight {
            illuminance: 11_000.0,
            shadows_enabled: true,
            ..default()
        },
        CascadeShadowConfigBuilder {
            // Bevy caps a light at 4 cascades. Keeping the first bound tight
            // (12 ft) holds the crisp up-close shadows; the cascades split
            // logarithmically out to `maximum_distance`, which we push from 350
            // to 650 ft so distant trees land inside the shadow pass. 650 also
            // meets the fog (starts 650 ft), so trees are shadowed right up to
            // where the haze swallows them — the far cascade is coarse, but so
            // are the blocky trees it shadows.
            num_cascades: 4,
            first_cascade_far_bound: 12.0,
            maximum_distance: 650.0,
            ..default()
        }
        .build(),
        RenderLayers::from_layers(&[0, 1]),
        Transform::from_rotation(Quat::from_euler(EulerRot::XYZ, -0.9, 0.7, 0.0)),
    ));

    // Full moon, always opposite the sun — the night is never pitch black.
    commands.spawn((
        CelestialLight { is_sun: false },
        DirectionalLight {
            illuminance: 0.0,
            color: Color::srgb(0.72, 0.80, 1.0),
            shadows_enabled: false,
            ..default()
        },
        CascadeShadowConfigBuilder {
            num_cascades: 2,
            first_cascade_far_bound: 12.0,
            maximum_distance: 160.0,
            ..default()
        }
        .build(),
        RenderLayers::from_layers(&[0, 1]),
        Transform::default(),
    ));

    // The bodies themselves: unlit spheres that ignore fog, hung well past the
    // fog wall so they read as sky, not scenery.
    let sun_disc_mat = materials.add(StandardMaterial {
        // Pure HDR emitter (unlit would discard emissive): with bloom on
        // the camera the disc blazes a brilliant white halo instead of
        // reading as a flat dot. fog_enabled: false so the fog wall at
        // 680 ft can't swallow it. Retinted per frame — white-hot overhead,
        // a deep ember at the horizon.
        base_color: Color::BLACK,
        emissive: LinearRgba::rgb(40.0, 39.0, 36.0),
        fog_enabled: false,
        ..default()
    });
    commands.spawn((
        CelestialDisc { is_sun: true },
        Mesh3d(meshes.add(Sphere::new(34.0))),
        MeshMaterial3d(sun_disc_mat.clone()),
        NotShadowCaster,
        Transform::default(),
    ));
    commands.spawn((
        CelestialDisc { is_sun: false },
        Mesh3d(meshes.add(Sphere::new(24.0))),
        MeshMaterial3d(materials.add(StandardMaterial {
            // A gentle glow — moonlight, not a second sun.
            base_color: Color::BLACK,
            emissive: LinearRgba::rgb(1.6, 1.8, 2.4),
            fog_enabled: false,
            ..default()
        })),
        NotShadowCaster,
        Transform::default(),
    ));

    // The star dome: one shared unit sphere + one shared emissive material,
    // scattered over the whole celestial sphere (not just the visible half —
    // the dome rotates with the sun angle, so stars below the horizon now are
    // the ones rising later tonight). Sizes vary, and a few outliers get
    // doubled so bloom picks out "bright stars" from the dust. Fixed seed:
    // the constellations are the same every session, like a real sky.
    let star_mesh = meshes.add(Sphere::new(1.0));
    let star_mat = materials.add(StandardMaterial {
        // Emissive-only like the sun/moon discs; the emissive is scaled by the
        // twilight fade each frame (and the dome hidden by day, so the fully
        // faded black spheres never draw).
        base_color: Color::BLACK,
        emissive: LinearRgba::BLACK,
        fog_enabled: false,
        ..default()
    });
    let mut rng = GardenRng::new(0x57A2_F1E1D);
    commands
        .spawn((StarDome, Transform::default(), Visibility::Hidden))
        .with_children(|dome| {
            for _ in 0..STAR_COUNT {
                // Uniform point on the sphere: y uniform in [-1, 1], then a
                // random longitude at that latitude's radius.
                let y = rng.range(-1.0, 1.0);
                let theta = rng.range(0.0, std::f32::consts::TAU);
                let r = (1.0 - y * y).max(0.0).sqrt();
                let dir = Vec3::new(r * theta.cos(), y, r * theta.sin());
                let mut size = rng.range(0.5, 1.4);
                if rng.chance(0.08) {
                    size *= 2.0;
                }
                dome.spawn((
                    Mesh3d(star_mesh.clone()),
                    MeshMaterial3d(star_mat.clone()),
                    NotShadowCaster,
                    Transform {
                        translation: dir * CELESTIAL_DISTANCE_FT,
                        scale: Vec3::splat(size),
                        ..default()
                    },
                ));
            }
        });

    commands.insert_resource(SkyMaterials {
        sun_disc: sun_disc_mat,
        stars: star_mat,
    });

    commands.insert_resource(AmbientLight {
        color: Color::srgb(0.82, 0.88, 0.95),
        brightness: 70.0,
    });
}

/// Walk the sun (and the full moon opposite it) across the sky on a real
/// 24-hour clock, retinting sky, fog, and ambient light to match. The moon
/// takes over lighting at night so the world is always readable.
fn update_day_cycle(
    time: Res<Time>,
    day: Res<DayCycle>,
    sky_mats: Res<SkyMaterials>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut clear: ResMut<ClearColor>,
    mut ambient: ResMut<AmbientLight>,
    mut sun_direction: ResMut<SunDirection>,
    mut sky_clock: ResMut<SkyClock>,
    mut cam_q: Query<(&Transform, Option<&mut DistanceFog>), With<Camera>>,
    mut lights: Query<(&CelestialLight, &mut DirectionalLight, &mut Transform), Without<Camera>>,
    mut discs: Query<
        (&CelestialDisc, &mut Transform, &mut Visibility),
        (Without<Camera>, Without<CelestialLight>),
    >,
    mut dome_q: Query<
        (&mut Transform, &mut Visibility),
        (
            With<StarDome>,
            Without<Camera>,
            Without<CelestialLight>,
            Without<CelestialDisc>,
        ),
    >,
) {
    let total = day.start_frac + time.elapsed_secs() / day.day_secs;
    let frac = total.rem_euclid(1.0);
    sky_clock.frac = frac;
    sky_clock.day = total as u32;
    // 0.25 of the cycle = 6:00 — sunrise on the eastern horizon.
    let angle = (frac - 0.25) * std::f32::consts::TAU;
    // Slight southward tilt keeps noon shadows from collapsing to nothing.
    let sun_dir = Vec3::new(angle.cos(), angle.sin(), 0.35).normalize();
    let moon_dir = -sun_dir;
    let elev = sun_dir.y;
    // The cutout shadow planes face whichever body is casting shadows.
    sun_direction.0 = if elev >= 0.0 { sun_dir } else { moon_dir };

    let day_t = (elev * 3.0).clamp(0.0, 1.0);
    let dusk_t = ((elev + 0.15) / 0.15).clamp(0.0, 1.0);
    let moon_t = (-elev * 3.0).clamp(0.0, 1.0);

    // Six-stop sky: proper BLUE all day, and sundown walks blue → yellow →
    // orange → violet → dark. The violet band is the civil-twilight afterglow
    // — the sun is below the horizon but still colouring the air, and skipping
    // straight from orange to near-black read as a light switch. Night is
    // never black — the full moon on the other side of the sky lifts it with
    // a cool white sheen.
    const DAY_SKY: Vec3 = Vec3::new(0.34, 0.58, 0.96);
    const GOLD_SKY: Vec3 = Vec3::new(0.92, 0.80, 0.48);
    const ORANGE_SKY: Vec3 = Vec3::new(0.96, 0.52, 0.26);
    const VIOLET_SKY: Vec3 = Vec3::new(0.34, 0.20, 0.40);
    const NIGHT_SKY: Vec3 = Vec3::new(0.07, 0.09, 0.17);
    const MOON_SHEEN: Vec3 = Vec3::new(0.18, 0.21, 0.30);

    let sky = if elev >= 0.35 {
        DAY_SKY
    } else if elev >= 0.15 {
        GOLD_SKY.lerp(DAY_SKY, (elev - 0.15) / 0.20)
    } else if elev >= 0.0 {
        ORANGE_SKY.lerp(GOLD_SKY, elev / 0.15)
    } else if dusk_t >= 0.5 {
        // Upper half of twilight: the horizon orange cools into violet.
        VIOLET_SKY.lerp(ORANGE_SKY, (dusk_t - 0.5) * 2.0)
    } else {
        // Lower half: violet sinks into the moonlit night.
        NIGHT_SKY
            .lerp(MOON_SHEEN, moon_t)
            .lerp(VIOLET_SKY, dusk_t * 2.0)
    };
    let sky_color = Color::srgb(sky.x, sky.y, sky.z);
    clear.0 = sky_color;

    ambient.brightness = 16.0 + 54.0 * day_t;
    // Ambient follows the same walk: blue daylight, golden dusk, moon-white night.
    let amb = Vec3::new(0.55, 0.62, 0.85)
        .lerp(Vec3::new(0.78, 0.86, 1.0), day_t)
        .lerp(Vec3::new(0.95, 0.85, 0.62), (1.0 - day_t) * (dusk_t * dusk_t));
    ambient.color = Color::srgb(amb.x, amb.y, amb.z);

    // The sun ball itself blushes with altitude: white-hot overhead, a deep
    // ember on the horizon. Bloom smears that tint into the halo, so sunrise
    // and sunset get their glow for free. Stars ride the same fade in reverse,
    // easing in once the sun dips below ~1° so they surface through the violet
    // rather than popping on. (Two material writes per frame — negligible.)
    let star_t = ((-elev - 0.02) / 0.10).clamp(0.0, 1.0);
    if let Some(mat) = materials.get_mut(&sky_mats.sun_disc) {
        let glow = Vec3::new(32.0, 9.0, 2.5).lerp(Vec3::new(40.0, 39.0, 36.0), day_t);
        mat.emissive = LinearRgba::rgb(glow.x, glow.y, glow.z);
    }
    if let Some(mat) = materials.get_mut(&sky_mats.stars) {
        let e = Vec3::new(3.2, 3.5, 4.2) * star_t;
        mat.emissive = LinearRgba::rgb(e.x, e.y, e.z);
    }

    for (light, mut dl, mut tf) in &mut lights {
        if light.is_sun {
            dl.illuminance = 24_000.0 * elev.max(0.0).powf(0.6);
            let warm = Vec3::new(1.0, 0.60, 0.35).lerp(Vec3::new(1.0, 0.98, 0.94), day_t);
            dl.color = Color::srgb(warm.x, warm.y, warm.z);
            // Hand the (expensive) shadow pass to whichever body is up.
            dl.shadows_enabled = elev > 0.02;
            tf.look_to(-sun_dir, Vec3::Y);
        } else {
            dl.illuminance = 420.0 * moon_t;
            dl.shadows_enabled = elev < -0.02;
            tf.look_to(-moon_dir, Vec3::Y);
        }
    }

    let Ok((cam_tf, fog)) = cam_q.get_single_mut() else {
        return;
    };
    let cam_pos = cam_tf.translation;
    if let Some(mut fog) = fog {
        fog.color = sky_color;
        let glow = Vec3::new(1.0, 0.95, 0.85).lerp(Vec3::new(0.75, 0.82, 1.0), moon_t);
        fog.directional_light_color = Color::srgba(glow.x, glow.y, glow.z, 0.6);
    }

    for (disc, mut tf, mut vis) in &mut discs {
        let dir = if disc.is_sun { sun_dir } else { moon_dir };
        tf.translation = cam_pos + dir * CELESTIAL_DISTANCE_FT;
        let want = if dir.y > -0.05 {
            Visibility::Visible
        } else {
            Visibility::Hidden
        };
        if *vis != want {
            *vis = want;
        }
    }

    // The star dome rides the camera like the discs, but also rotates with the
    // sun's orbital angle (same Z axis the sun circles on), so the whole field
    // wheels westward through the night. Hidden by day: no point drawing 220
    // black spheres behind a blue sky.
    for (mut tf, mut vis) in &mut dome_q {
        tf.translation = cam_pos;
        tf.rotation = Quat::from_rotation_z(angle);
        let want = if star_t > 0.001 {
            Visibility::Visible
        } else {
            Visibility::Hidden
        };
        if *vis != want {
            *vis = want;
        }
    }
}
