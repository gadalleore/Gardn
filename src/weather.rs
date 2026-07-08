//! Weather: the global wind — a slowly wandering direction with a gusty 0–5
//! strength that re-rolls toward calm — and the ribbon streamers that make it
//! visible and audible as they race past the worm. `WeatherPlugin` owns the
//! `Wind` resource, the streamer assets, and the wind systems; grass, leaves and
//! trees all read `crate::weather::Wind` to sway with it.

use bevy::audio::{SpatialScale, Volume};
use bevy::pbr::NotShadowCaster;
use bevy::prelude::*;

use crate::world::GardenRng;
use crate::audio::GameSounds;
use crate::streaming::ChunkWorld;
use crate::worm::ground_world_y;

pub struct WeatherPlugin;

impl Plugin for WeatherPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Wind>()
            .add_systems(Startup, setup_streamers)
            .add_systems(Update, (update_wind, update_wind_streamers));
    }
}

/// Wind streamers: pale ribbons ~1.4 ft long, translucent, unlit so they read
/// as moving air rather than solid geometry.
fn setup_streamers(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    commands.insert_resource(StreamerAssets {
        mesh: meshes.add(Cuboid::new(1.4, 0.025, 0.025)),
        material: materials.add(StandardMaterial {
            base_color: Color::srgba(0.96, 0.97, 1.0, 0.6),
            alpha_mode: AlphaMode::Blend,
            unlit: true,
            ..default()
        }),
    });
}

/// Global wind: a slowly wandering direction, gusts layered on, and a
/// 0–5 strength that re-rolls every half-minute-or-so with a strong bias
/// toward calm. 0 = nice still day; 5 = a gale that physically shoves the
/// worm downwind. Streamers in the air show where it's blowing.
#[derive(Resource)]
pub(crate) struct Wind {
    pub(crate) dir: Vec2,
    /// Compass heading of `dir`, radians. Only moves ±1° per weather shift.
    pub(crate) heading: f32,
    /// 0 = dead calm … 5 = worm-shoving gale.
    pub(crate) strength: f32,
    pub(crate) target: f32,
    pub(crate) next_shift_at: f32,
    pub(crate) rng: GardenRng,
}

impl Default for Wind {
    fn default() -> Self {
        let mut rng = GardenRng::new(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos() as u64)
                .unwrap_or(0xB1FF),
        );
        let heading = rng.range(0.0, std::f32::consts::TAU);
        Self {
            dir: Vec2::new(heading.cos(), heading.sin()),
            heading,
            strength: 0.0,
            target: 0.0,
            next_shift_at: 0.0,
            rng,
        }
    }
}

/// The gale threshold: above this the worm starts getting pushed.
pub(crate) const WIND_PUSH_FROM: f32 = 4.0;

/// A ribbon of air racing downwind past the worm — the wind made visible.
/// Spawned upwind, despawned when it outlives itself or blows out of range.
#[derive(Component)]
struct WindStreamer {
    age: f32,
    life: f32,
    speed: f32,
    bob_phase: f32,
}

/// Marks the handful of streamers currently carrying a spatial wind voice, so
/// the population is capped — dozens of overlapping loops would be a mush and a
/// CPU drain. The rest are silent ribbons; these few are the audible gusts.
#[derive(Component)]
struct WindVoice;

/// How many streamers may sound at once. Kept high on purpose: many copies of
/// the wind loop, each begun at a different moment as its streamer spawned and
/// each placed at a different point around the worm, overlap into a shifting
/// wash that surrounds you — that phase soup is exactly what sells "wind all
/// around." Per-voice volume stays modest so the sum is rich, not clipping, and
/// scales with strength (calm → faint, gale → full).
const MAX_WIND_VOICES: usize = 14;
const WIND_VOICE_MIN_VOL: f32 = 0.06;
const WIND_VOICE_MAX_VOL: f32 = 0.45;
/// Shrinks world distances for the wind voices' attenuation — the world is in
/// feet and streamers hug the worm within ~45 ft, so without this they'd fall
/// silent almost immediately. Smaller = the gusts carry from farther.
const WIND_SPATIAL_SCALE: f32 = 0.1;

#[derive(Resource)]
struct StreamerAssets {
    mesh: Handle<Mesh>,
    material: Handle<StandardMaterial>,
}

/// Keep a population of streamers proportional to wind strength flowing past
/// the camera, all pointing (and moving) downwind.
fn update_wind_streamers(
    time: Res<Time>,
    mut commands: Commands,
    mut wind: ResMut<Wind>,
    assets: Res<StreamerAssets>,
    sounds: Res<GameSounds>,
    chunk_world: Res<ChunkWorld>,
    cam_q: Query<&Transform, (With<Camera>, Without<WindStreamer>)>,
    mut streamers: Query<
        (Entity, &mut WindStreamer, &mut Transform, Has<WindVoice>),
        Without<Camera>,
    >,
) {
    let Ok(cam) = cam_q.get_single() else {
        return;
    };
    let cam_pos = cam.translation;
    let dir3 = Vec3::new(wind.dir.x, 0.0, wind.dir.y);
    let yaw = Quat::from_rotation_y((-wind.dir.y).atan2(wind.dir.x));
    let dt = time.delta_secs();
    let t = time.elapsed_secs();

    // Streamer population: none on a calm day, a handful in light air, a
    // blizzard of them in a gale.
    let desired = if wind.strength < 0.4 {
        0
    } else {
        ((wind.strength * 10.0).round() as usize).max(4)
    };

    let mut alive = 0usize;
    let mut voices = 0usize;
    for (entity, mut streamer, mut tf, has_voice) in &mut streamers {
        streamer.age += dt;
        tf.translation += dir3 * streamer.speed * dt;
        tf.translation.y += (t * 2.3 + streamer.bob_phase).sin() * 0.3 * dt;
        tf.rotation = yaw;

        let gone_far = Vec2::new(tf.translation.x - cam_pos.x, tf.translation.z - cam_pos.z)
            .length()
            > 45.0;
        if streamer.age > streamer.life || gone_far {
            commands.entity(entity).despawn();
        } else {
            alive += 1;
            if has_voice {
                voices += 1;
            }
        }
    }

    // Top up toward the target population, a few per frame, seeded upwind so
    // they stream past the worm.
    let mut to_spawn = desired.saturating_sub(alive).min(3);
    while to_spawn > 0 {
        to_spawn -= 1;
        let side = Vec3::new(-dir3.z, 0.0, dir3.x);
        // Low worm-level airspace, close by — streamers a worm actually sees,
        // hugging the ground it crawls on.
        let mut pos =
            cam_pos - dir3 * wind.rng.range(3.0, 18.0) + side * wind.rng.range(-10.0, 10.0);
        pos.y = ground_world_y(&chunk_world, pos.x, pos.z) + wind.rng.range(0.15, 2.2);

        let speed = 4.0 + wind.strength * 3.0 + wind.rng.range(0.0, 2.0);
        // The streamer IS the wind gauge: a level-1 breeze draws short wisps,
        // a level-5 gale drags long fat banners. (Direction is the streak's
        // long axis + its motion — both point downwind.)
        let power = wind.strength / 5.0;
        let length = (0.4 + power * 2.6) * wind.rng.range(0.85, 1.15);
        let girth = 0.5 + power * 2.0;
        let mut streamer = commands.spawn((
            WindStreamer {
                age: 0.0,
                life: wind.rng.range(3.0, 6.0),
                speed,
                bob_phase: wind.rng.range(0.0, std::f32::consts::TAU),
            },
            Mesh3d(assets.mesh.clone()),
            MeshMaterial3d(assets.material.clone()),
            NotShadowCaster,
            Transform {
                translation: pos,
                rotation: yaw,
                scale: Vec3::new(length, girth, girth),
            },
        ));

        // Give the first few streamers a spatial wind voice, louder the harder
        // it blows — so the ribbons you watch race past the worm are the source
        // of the wind you hear, panning by as they go. Capped so a gale is a
        // rich wash, not a wall of clipping loops. When the streamer despawns,
        // its voice goes with it — gusts of sound come and go with the air.
        if voices < MAX_WIND_VOICES {
            voices += 1;
            let vol = WIND_VOICE_MIN_VOL + power * (WIND_VOICE_MAX_VOL - WIND_VOICE_MIN_VOL);
            streamer.insert((
                WindVoice,
                AudioPlayer::new(sounds.wind.clone()),
                PlaybackSettings::LOOP
                    .with_spatial(true)
                    .with_spatial_scale(SpatialScale::new(WIND_SPATIAL_SCALE))
                    .with_volume(Volume::new(vol)),
            ));
        }
    }
}

fn update_wind(time: Res<Time>, mut wind: ResMut<Wind>) {
    let t = time.elapsed_secs();

    // Weather re-roll: six discrete levels on a quadratic likelihood curve,
    // pinned at 50% for dead calm and 1% for a full 5/5 gale, the middle
    // falling off as (1 - n/5)²:
    //   0: 50.0%  1: 26.1%  2: 14.7%  3: 6.5%  4: 1.6%  5: 1.0%
    //
    // How long a level holds depends on how windy it is — calm days linger,
    // gales blow themselves out: 0→5 min, 1→4, 2→3, 3→2.5, 4→2, 5→1.
    if t >= wind.next_shift_at {
        const WIND_LEVEL_CDF: [f32; 6] = [0.500, 0.7613, 0.9083, 0.9737, 0.9900, 1.0];
        const HOLD_MINUTES: [f32; 6] = [5.0, 4.0, 3.0, 2.5, 2.0, 1.0];

        let roll = wind.rng.next_f32();
        let level = WIND_LEVEL_CDF
            .iter()
            .position(|&cum| roll < cum)
            .unwrap_or(5);
        wind.target = level as f32;
        wind.next_shift_at = t + HOLD_MINUTES[level] * 60.0;

        // The direction creeps: exactly one degree per shift, coin-flip
        // left or right — over hours the wind slowly wanders the compass.
        let step = 1.0f32.to_radians();
        wind.heading += if wind.rng.chance(0.5) { step } else { -step };
        wind.dir = Vec2::new(wind.heading.cos(), wind.heading.sin());

        println!("🌬️ Wind shifting toward {level}/5");
    }
    // Ease toward the target like real weather, not a light switch.
    let blend = (time.delta_secs() * 0.06).min(1.0);
    wind.strength += (wind.target - wind.strength) * blend;
}
