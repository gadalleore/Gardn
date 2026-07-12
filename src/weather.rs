//! Weather: the global wind — a slowly wandering direction with a gusty 0–5
//! strength that re-rolls toward calm — and the ribbon streamers that make it
//! visible and audible as they race past the worm. The rolled level is only
//! the *base*: a private gust engine layers surges, lulls, and flutter on top
//! so `strength` breathes like real air instead of holding a flat line.
//! `WeatherPlugin` owns the `Wind` resource, the streamer assets, and the wind
//! systems; grass, leaves and trees all read `crate::weather::Wind` to sway
//! with it.
//!
//! Rotation 2 also brings the season clock and *local* weather. A season clock
//! (day length never changes — seasons only tilt weather odds) feeds a
//! procession brain (owner's spec: sunny → cirrostratus herald → one main
//! cloud type → altostratus/stratus outro → dissipate, with winter / arid /
//! coastal / wind modifiers and a morning-fog roll). Crucially the brain does
//! NOT flip a global "it is cloudy" switch — it's a *director* that decides
//! what kind of front is brewing and how full the sky should be, and a
//! population of discrete, world-positioned **blocky voxel clouds** drift
//! through a field around the worm to match. Different places have different
//! sky; you can crawl out from under the weather. Clouds render as voxel
//! clumps shaped to each type's silhouette with a near/far LOD, exactly like
//! the leaves/trees (owner's rotation-2 clarification). Cloud shadows and
//! rain-under-clouds ride in follow-up PRs; morning fog is applied by sky.rs
//! (the single writer of fog) off [`WeatherSky`].

use bevy::audio::{SpatialScale, Volume};
use bevy::pbr::NotShadowCaster;
use bevy::prelude::*;
use bevy::render::mesh::{Indices, PrimitiveTopology};
use bevy::render::render_asset::RenderAssetUsages;

use crate::world::GardenRng;
use crate::audio::GameSounds;
use crate::streaming::ChunkWorld;
use crate::worm::ground_world_y;

pub struct WeatherPlugin;

impl Plugin for WeatherPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Wind>()
            .init_resource::<GustTexture>()
            .insert_resource(SeasonClock::from_env())
            .insert_resource(CloudSimRes(CloudSim::from_env()))
            .init_resource::<CloudDirector>()
            .init_resource::<WeatherSky>()
            .add_systems(Startup, (setup_streamers, setup_cloud_library))
            .add_systems(
                Update,
                (
                    update_wind,
                    update_wind_streamers,
                    update_seasons,
                    update_cloud_director,
                    tint_clouds,
                    spawn_local_clouds,
                    drive_clouds,
                    update_cloud_lod,
                )
                    .chain(),
            );
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

/// Private state of the gust engine. Kept out of `Wind` on purpose: `Wind`'s
/// shape is a cross-module contract (grass/leaves/foliage/worm read it), so
/// the texture layers live here and only their *sum* is published each frame
/// as `Wind::strength`. Real wind isn't a level that eases and holds — it
/// surges, sags, and trembles around its mean, so:
///   strength = base (the eased 0–5 roll)
///            + gust  (an event with a fast smooth rise and a slower fall;
///                     ~30% are negative — lulls where the air sags)
///            + flutter (two incommensurate sines — the air never flatlines)
#[derive(Resource, Default)]
struct GustTexture {
    /// The slow ease toward `Wind::target` — what `strength` used to be.
    base: f32,
    /// Signed peak of the active gust; negative means a lull.
    amp: f32,
    /// Envelope timeline, absolute seconds: rise `start→peak`, fall `peak→end`.
    start: f32,
    peak: f32,
    end: f32,
    /// When to roll the next gust/lull event.
    next_at: f32,
}

/// Hermite ease for the gust envelope — a linear ramp reads as mechanical.
fn smoothstep(x: f32) -> f32 {
    let x = x.clamp(0.0, 1.0);
    x * x * (3.0 - 2.0 * x)
}

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
        // Speed = the ribbon's own random pace + a live wind term, so when a
        // gust hits, every streamer already in flight surges with it (and sags
        // in a lull) instead of only newly spawned ones knowing the news.
        tf.translation += dir3 * (streamer.speed + wind.strength * 3.0) * dt;
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

        // Only the random part is baked in; the wind's share of the speed is
        // added live each frame above, so gusts reach ribbons mid-flight.
        let speed = 4.0 + wind.rng.range(0.0, 2.0);
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

fn update_wind(time: Res<Time>, mut wind: ResMut<Wind>, mut gusts: ResMut<GustTexture>) {
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

        println!("🌬️ Wind shifting toward {level}/5");
    }
    // Ease the BASE toward the target like real weather, not a light switch.
    let blend = (time.delta_secs() * 0.06).min(1.0);
    gusts.base += (wind.target - gusts.base) * blend;

    // Roll gust/lull events. Windier weather gusts harder and more often;
    // near-calm air is left alone (a gust out of a 0/5 day would be weather
    // the level system didn't order).
    if t >= gusts.next_at {
        if gusts.base > 0.3 {
            gusts.amp = if wind.rng.chance(0.3) {
                // A lull: the air sags but doesn't die — cap what it can take
                // so light breezes don't invert.
                -wind.rng.range(0.25, 0.6) * gusts.base.min(2.0)
            } else {
                // A surge, scaled to the weather: a breeze puffs (+~0.5), a
                // near-gale slams (+~1.5, briefly over WIND_PUSH_FROM — the
                // shove the worm feels in gusty weather IS these events).
                wind.rng.range(0.5, 1.0) * (0.3 + gusts.base * 0.28)
            };
            // Real gusts hit fast and drain slow: ~1 s rise, several to fall.
            let attack = wind.rng.range(0.6, 1.6);
            gusts.start = t;
            gusts.peak = t + attack;
            gusts.end = gusts.peak + wind.rng.range(2.5, 6.0);
        }
        gusts.next_at = t + wind.rng.range(4.0, 14.0) / (0.6 + gusts.base * 0.35);
    }

    // The active event's envelope: smooth rise to `amp`, slower smooth fall.
    let env = if t < gusts.peak {
        smoothstep((t - gusts.start) / (gusts.peak - gusts.start))
    } else if t < gusts.end {
        1.0 - smoothstep((t - gusts.peak) / (gusts.end - gusts.peak))
    } else {
        0.0
    };
    // Flutter: two sines at incommensurate rates so the sum never visibly
    // repeats — the fine trembling between events, scaled to the weather.
    let flutter = ((t * 0.9).sin() * 0.6 + (t * 2.37 + 1.7).sin() * 0.4) * 0.12 * gusts.base;

    wind.strength = (gusts.base + gusts.amp * env + flutter).clamp(0.0, 5.0);

    // The published direction breathes a few degrees around the slow-wander
    // heading — harder wind wobbles more. `heading` stays the canonical
    // compass value; only the derived `dir` carries the texture, so the
    // ±1°-per-shift wander contract above is untouched.
    let wobble = ((t * 0.23).sin() * 0.6 + (t * 0.71 + 3.1).sin() * 0.4)
        * 4.0f32.to_radians()
        * (wind.strength / 5.0);
    let h = wind.heading + wobble;
    wind.dir = Vec2::new(h.cos(), h.sin());
}

// ===================== Seasons =====================

/// Australian seasons. They drive weather probabilities ONLY — day length
/// never changes (owner's spec). The game starts in summer; the year cycles
/// Summer → Autumn → Winter → Spring, [`DAYS_PER_SEASON`] game days each.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Season {
    Summer,
    Autumn,
    Winter,
    Spring,
}

/// Game days per season. A game day is 24 real hours, so even one day is a
/// long visit — three keeps a season from outstaying a play session's memory
/// while still being "the weather's been like this for days".
pub(crate) const DAYS_PER_SEASON: u32 = 3;

impl Season {
    pub(crate) fn from_day(day: u32) -> Self {
        match (day / DAYS_PER_SEASON) % 4 {
            0 => Season::Summer,
            1 => Season::Autumn,
            2 => Season::Winter,
            _ => Season::Spring,
        }
    }

    /// GARDN_SEASON=summer|autumn|winter|spring pins the season (dev knob).
    fn from_env() -> Option<Self> {
        std::env::var("GARDN_SEASON")
            .ok()
            .and_then(|v| match v.trim().to_ascii_lowercase().as_str() {
                "summer" => Some(Season::Summer),
                "autumn" | "fall" => Some(Season::Autumn),
                "winter" => Some(Season::Winter),
                "spring" => Some(Season::Spring),
                _ => None,
            })
    }

    /// Owner: winter buffs cloud likelihood +30%. Other seasons are neutral.
    fn cloud_factor(self) -> f32 {
        if self == Season::Winter { 1.3 } else { 1.0 }
    }

    /// Owner: morning fog 70% in fall and winter, 20% in summer. Spring is
    /// unspecified — split the difference (noted in coordination for review).
    fn fog_chance(self) -> f32 {
        match self {
            Season::Autumn | Season::Winter => 0.70,
            Season::Summer => 0.20,
            Season::Spring => 0.40,
        }
    }
}

/// The season, published for other modules (grass browning, breeding seasons…
/// future gameplay reads this; today only the cloud machine does).
#[derive(Resource)]
pub(crate) struct SeasonClock {
    pub(crate) season: Season,
    /// GARDN_SEASON pin — when set, the day count is ignored.
    forced: Option<Season>,
}

impl SeasonClock {
    fn from_env() -> Self {
        let forced = Season::from_env();
        match forced {
            Some(s) => println!("🗓️ Season pinned to {s:?} via GARDN_SEASON."),
            None => println!(
                "🗓️ Seasons: starting in Summer, {DAYS_PER_SEASON} game days each."
            ),
        }
        Self { season: forced.unwrap_or(Season::Summer), forced }
    }
}

/// Turn the season with the day count from the sky's clock.
fn update_seasons(clock: Res<crate::sky::SkyClock>, mut seasons: ResMut<SeasonClock>) {
    let now = seasons.forced.unwrap_or_else(|| Season::from_day(clock.day));
    if now != seasons.season {
        seasons.season = now;
        println!("🗓️ The season turns: {now:?} (day {}).", clock.day);
    }
}

// ===================== The procession brain =====================
//
// Owner's spec, verbatim spirit: cirrostratus is the herald — it never appears
// alone, it precedes the other clouds (a layer above them) and recedes after
// they're gone. One main cloud type at a time. Sunny → cirrostratus → main
// event → altostratus/stratus (50/50) → dissipate. Event odds: cirrus /
// altocumulus / stratocumulus / cirrocumulus at 25% each; a cirrus event then
// resolves into cumulonimbus (10) / nimbostratus (45) / cumulus (50 — false
// alarm). Those three sum to 105, so they're treated as weights (≈9.5% /
// 42.9% / 47.6%). Modifiers: winter ×1.3, arid ×0.6, coastal ×1.1 on
// formation; active wind lifts the formation chance itself to 70%.
//
// ROTATION-2 RE-SCOPE: this brain is unchanged in what it decides, but it is
// no longer a global switch. Its per-frame output — herald fullness, the main
// cloud type, and how full the main layer should be — is read as *population
// targets* for a field of discrete local clouds, not as a scene-wide "cloudy"
// flag. See [`CloudDirector`] and the local-cloud section below.

/// Every cloud form that can take the main stage. The cirrostratus herald is
/// deliberately NOT here — it's a permanent layer of its own (the herald
/// density in [`CloudDirector`]), never the main event.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum CloudType {
    Cirrus,
    Altocumulus,
    Stratocumulus,
    Cirrocumulus,
    Cumulonimbus,
    Nimbostratus,
    Cumulus,
    Altostratus,
    Stratus,
}

impl CloudType {
    /// GARDN_CLOUDS=<name> forces this type as an immediate first event —
    /// the eyeballing knob (a short herald, then the cloud).
    fn from_env() -> Option<Self> {
        std::env::var("GARDN_CLOUDS")
            .ok()
            .and_then(|v| match v.trim().to_ascii_lowercase().as_str() {
                "cirrus" => Some(CloudType::Cirrus),
                "altocumulus" => Some(CloudType::Altocumulus),
                "stratocumulus" => Some(CloudType::Stratocumulus),
                "cirrocumulus" => Some(CloudType::Cirrocumulus),
                "cumulonimbus" => Some(CloudType::Cumulonimbus),
                "nimbostratus" => Some(CloudType::Nimbostratus),
                "cumulus" => Some(CloudType::Cumulus),
                "altostratus" => Some(CloudType::Altostratus),
                "stratus" => Some(CloudType::Stratus),
                _ => None,
            })
    }
}

/// Where the machine is in one weather system's life. The timeline is linear
/// by design — the owner's spec is a procession, not a free graph.
#[derive(Clone, Copy, PartialEq, Debug)]
enum CloudPhase {
    /// Sunny. `phase_end` is the next formation check.
    Clear,
    /// Cirrostratus builds; the coming main event is already rolled so the
    /// herald genuinely *heralds* something.
    HeraldOnset { main: CloudType },
    /// The main event (under the full cirrostratus veil). A `Cirrus` main
    /// resolves into a second `Main` — cumulonimbus / nimbostratus / cumulus.
    Main { cloud: CloudType },
    /// The 50/50 altostratus-or-stratus tail every event drains through.
    Outro { cloud: CloudType },
    /// Clouds gone; the cirrostratus veil thins away last, as it came first.
    HeraldReceding,
}

// Pacing constants (real seconds — deliberately NOT scaled by GARDN_DAY_SECS,
// so a compressed test day still gives events long enough to look at). Ranges
// are (min, max) fed to the rng.
const FORM_CHECK_SECS: f32 = 180.0;
/// Base chance a front forms per check on a calm day (~one system per
/// 10–15 min of sunny sky before modifiers).
const FORM_BASE_CHANCE: f32 = 0.22;
/// Owner: with wind blowing, 70% chance the weather turns cloudy. This
/// *replaces* the base — the regional/seasonal multipliers still apply on top.
const WINDY_FORM_CHANCE: f32 = 0.70;
/// "Wind is blowing" threshold on the 0–5 scale — level 2 is the first level
/// that reads as real wind rather than an idle breeze.
const WIND_ACTIVE_FROM: f32 = 2.0;
const HERALD_ONSET_SECS: (f32, f32) = (90.0, 150.0);
const MAIN_SECS: (f32, f32) = (240.0, 480.0);
/// The resolved half of a cirrus event (storm / rain / fluffy false alarm).
const ESCALATED_SECS: (f32, f32) = (180.0, 420.0);
const OUTRO_SECS: (f32, f32) = (120.0, 240.0);
const HERALD_RECEDE_SECS: (f32, f32) = (60.0, 120.0);
/// Main-layer target fade at each phase edge, so a front fills in and drains
/// out (the local cloud population ramps with it) instead of snapping.
const COVER_RAMP_SECS: f32 = 45.0;

/// Morning fog window as day fractions (0 = midnight): rolls in ~5:31,
/// burn-off starts ~7:26, gone by ~8:38.
const FOG_WINDOW: (f32, f32) = (0.23, 0.36);
const FOG_RAMP_IN_FRAC: f32 = 0.03;
const FOG_FADE_FROM: f32 = 0.31;

/// What the sim needs to know about the world this step. Plain data so the
/// unit tests build one by hand — no Bevy in the loop.
struct CloudCtx {
    now: f32,
    day: u32,
    /// Fraction of the 24-h day, 0 = midnight.
    day_frac: f32,
    season: Season,
    wind_strength: f32,
    arid: bool,
    coastal: bool,
}

/// Chance a front forms at one formation check. Pure — the modifier math the
/// tests pin exactly. Wind swaps the base (owner's 70% rule); winter / arid /
/// coastal multiply it; capped so weather is never a certainty.
fn formation_chance(season: Season, wind_strength: f32, arid: bool, coastal: bool) -> f32 {
    let base = if wind_strength >= WIND_ACTIVE_FROM {
        WINDY_FORM_CHANCE
    } else {
        FORM_BASE_CHANCE
    };
    let mut chance = base * season.cloud_factor();
    if arid {
        chance *= 0.6;
    }
    if coastal {
        chance *= 1.1;
    }
    chance.min(0.95)
}

/// The four equally likely main events (owner: 25% each).
fn roll_main(rng: &mut GardenRng) -> CloudType {
    match (rng.next_f32() * 4.0) as u32 {
        0 => CloudType::Cirrus,
        1 => CloudType::Altocumulus,
        2 => CloudType::Stratocumulus,
        _ => CloudType::Cirrocumulus,
    }
}

/// What a cirrus event resolves into. Owner's odds 10 / 45 / 50 total 105, so
/// they're weights: cumulonimbus ≈9.5%, nimbostratus ≈42.9%, cumulus ≈47.6%.
fn roll_escalation(rng: &mut GardenRng) -> CloudType {
    let r = rng.next_f32() * 105.0;
    if r < 10.0 {
        CloudType::Cumulonimbus
    } else if r < 55.0 {
        CloudType::Nimbostratus
    } else {
        CloudType::Cumulus
    }
}

/// The whole machine, Bevy-free so tests can run it for simulated days.
struct CloudSim {
    phase: CloudPhase,
    phase_start: f32,
    phase_end: f32,
    rng: GardenRng,
    /// Day the morning-fog roll last happened, so it happens once per morning.
    fog_rolled_day: Option<u32>,
    fog_today: bool,
    /// GARDN_FOG=1 — every morning's roll comes up fog (dev knob).
    force_fog: bool,
}

impl CloudSim {
    fn new(seed: u64, forced_event: Option<CloudType>, force_fog: bool) -> Self {
        let mut sim = Self {
            phase: CloudPhase::Clear,
            phase_start: 0.0,
            phase_end: FORM_CHECK_SECS,
            rng: GardenRng::new(seed),
            fog_rolled_day: None,
            fog_today: false,
            force_fog,
        };
        if let Some(main) = forced_event {
            println!("☁️ GARDN_CLOUDS: forcing an immediate {main:?} event.");
            sim.phase = CloudPhase::HeraldOnset { main };
            sim.phase_end = 20.0;
        }
        sim
    }

    fn from_env() -> Self {
        let seed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0xC10D);
        let force_fog = std::env::var("GARDN_FOG").map(|v| v.trim() == "1").unwrap_or(false);
        Self::new(seed, CloudType::from_env(), force_fog)
    }

    fn enter(&mut self, phase: CloudPhase, at: f32, duration: f32) {
        self.phase = phase;
        self.phase_start = at;
        self.phase_end = at + duration;
    }

    fn range(&mut self, r: (f32, f32)) -> f32 {
        self.rng.range(r.0, r.1)
    }

    /// Advance the machine to `ctx.now`. Transitions fire off the *scheduled*
    /// boundary (`phase_end`), not the frame time, and the loop drains any
    /// backlog — so a test can jump hours and every check still happens.
    fn step(&mut self, ctx: &CloudCtx) {
        // One fog roll per morning, at the window's edge (or mid-window if the
        // game launched inside it).
        if ctx.day_frac >= FOG_WINDOW.0
            && ctx.day_frac < FOG_WINDOW.1
            && self.fog_rolled_day != Some(ctx.day)
        {
            self.fog_rolled_day = Some(ctx.day);
            self.fog_today = self.force_fog || self.rng.chance(ctx.season.fog_chance());
            if self.fog_today {
                println!("🌫️ A {:?} morning fog settles in.", ctx.season);
            }
        }

        while ctx.now >= self.phase_end {
            let at = self.phase_end;
            match self.phase {
                CloudPhase::Clear => {
                    let chance = formation_chance(
                        ctx.season,
                        ctx.wind_strength,
                        ctx.arid,
                        ctx.coastal,
                    );
                    if self.rng.chance(chance) {
                        let main = roll_main(&mut self.rng);
                        let dur = self.range(HERALD_ONSET_SECS);
                        self.enter(CloudPhase::HeraldOnset { main }, at, dur);
                        println!("☁️ A cirrostratus veil creeps in — a {main:?} front is coming.");
                    } else {
                        self.phase_end = at + FORM_CHECK_SECS;
                    }
                }
                CloudPhase::HeraldOnset { main } => {
                    let dur = self.range(MAIN_SECS);
                    self.enter(CloudPhase::Main { cloud: main }, at, dur);
                    println!("☁️ Main event: {main:?}.");
                }
                CloudPhase::Main { cloud } => {
                    if cloud == CloudType::Cirrus {
                        // The cirrus wisps resolve into what they warned of.
                        let next = roll_escalation(&mut self.rng);
                        let dur = self.range(ESCALATED_SECS);
                        self.enter(CloudPhase::Main { cloud: next }, at, dur);
                        match next {
                            CloudType::Cumulonimbus => println!(
                                "⛈️ The cirrus darkens into CUMULONIMBUS — find shelter!"
                            ),
                            CloudType::Nimbostratus => println!(
                                "🌧️ The cirrus thickens into nimbostratus — rain coming."
                            ),
                            _ => println!("⛅ False alarm — just fluffy cumulus."),
                        }
                    } else {
                        let outro = if self.rng.chance(0.5) {
                            CloudType::Altostratus
                        } else {
                            CloudType::Stratus
                        };
                        let dur = self.range(OUTRO_SECS);
                        self.enter(CloudPhase::Outro { cloud: outro }, at, dur);
                        println!("☁️ The system drains through {outro:?}.");
                    }
                }
                CloudPhase::Outro { .. } => {
                    let dur = self.range(HERALD_RECEDE_SECS);
                    self.enter(CloudPhase::HeraldReceding, at, dur);
                    println!("☁️ Clouds gone — the cirrostratus veil recedes.");
                }
                CloudPhase::HeraldReceding => {
                    self.enter(CloudPhase::Clear, at, FORM_CHECK_SECS);
                    println!("🌞 Clear skies.");
                }
            }
        }
    }

    /// The front order at `now`: (herald fullness, main type, main fullness),
    /// each 0..1. Cirrostratus ramps over its onset/recede phases and holds
    /// full through the event — so the main layer never fills without the
    /// herald already above it (owner's invariant). This is the same shape the
    /// pre-rescope global `covers()` produced; now it's read as *population
    /// targets* for local clouds rather than a scene-wide cover.
    fn front_order(&self, now: f32) -> (f32, Option<CloudType>, f32) {
        let progress = ((now - self.phase_start)
            / (self.phase_end - self.phase_start).max(f32::EPSILON))
        .clamp(0.0, 1.0);
        match self.phase {
            CloudPhase::Clear => (0.0, None, 0.0),
            CloudPhase::HeraldOnset { .. } => (smoothstep(progress), None, 0.0),
            CloudPhase::Main { cloud } | CloudPhase::Outro { cloud } => {
                let edge = ((now - self.phase_start) / COVER_RAMP_SECS)
                    .min((self.phase_end - now) / COVER_RAMP_SECS)
                    .clamp(0.0, 1.0);
                (1.0, Some(cloud), smoothstep(edge))
            }
            CloudPhase::HeraldReceding => (1.0 - smoothstep(progress), None, 0.0),
        }
    }

    /// Morning fog thickness 0..1 — rises fast off the roll, burns off slow.
    fn fog_level(&self, day: u32, day_frac: f32) -> f32 {
        if self.fog_rolled_day != Some(day) || !self.fog_today {
            return 0.0;
        }
        if !(FOG_WINDOW.0..FOG_WINDOW.1).contains(&day_frac) {
            return 0.0;
        }
        let rise = smoothstep((day_frac - FOG_WINDOW.0) / FOG_RAMP_IN_FRAC);
        let fall = 1.0 - smoothstep((day_frac - FOG_FADE_FROM) / (FOG_WINDOW.1 - FOG_FADE_FROM));
        rise.min(fall)
    }
}

/// The sim as a resource. Private — the spawner reads [`CloudDirector`].
#[derive(Resource)]
struct CloudSimRes(CloudSim);

/// The brain's per-frame *order* to the local cloud field: how full the herald
/// and main layers should be, and which main type. NOT a global "it is cloudy"
/// switch — [`spawn_local_clouds`] turns these fullness targets into a
/// population of discrete drifting clouds. Retires the pre-rescope global
/// cover scalar that sky.rs used to dim the whole scene by.
#[derive(Resource, Default)]
pub(crate) struct CloudDirector {
    /// 0..1 target fullness of the high cirrostratus herald layer.
    herald: f32,
    /// The single main cloud type currently on stage, if any.
    main_type: Option<CloudType>,
    /// 0..1 target fullness of the main cloud layer.
    main: f32,
    /// 0..1 estimate of how much of the sky the main layer covers (its
    /// fullness weighted by how sky-filling that cloud type is). Drives the
    /// owner's white-until-50%-then-grey rule in [`tint_clouds`].
    cover: f32,
}

/// The one weather value sky.rs reads: morning-fog thickness. Kept as its own
/// tiny resource so the fog contract between the two files is a single field,
/// and sky.rs stays the sole writer of the actual `DistanceFog`.
#[derive(Resource, Default)]
pub(crate) struct WeatherSky {
    pub(crate) fog: f32,
}

/// Step the procession brain with the live world and publish its order. The
/// sky's clock gives day/frac; the wind gives the 70% rule; the biome under
/// the worm gives the regional modifiers (arid outback starves clouds, the
/// coasts feed them).
fn update_cloud_director(
    time: Res<Time>,
    clock: Res<crate::sky::SkyClock>,
    seasons: Res<SeasonClock>,
    wind: Res<Wind>,
    mut sim: ResMut<CloudSimRes>,
    mut director: ResMut<CloudDirector>,
    mut sky: ResMut<WeatherSky>,
    cam_q: Query<&Transform, With<Camera>>,
) {
    let (arid, coastal) = cam_q
        .get_single()
        .map(|tf| region_class(tf.translation.x, tf.translation.z))
        .unwrap_or((false, false));
    let ctx = CloudCtx {
        now: time.elapsed_secs(),
        day: clock.day,
        day_frac: clock.frac,
        season: seasons.season,
        wind_strength: wind.strength,
        arid,
        coastal,
    };
    sim.0.step(&ctx);
    let (herald, main_type, main) = sim.0.front_order(ctx.now);
    director.herald = herald;
    director.main_type = main_type;
    director.main = main;
    // Sky-cover estimate: the main layer's fullness scaled by how sky-filling
    // its type is (a full stratus deck ≈ overcast; a few cumulus barely count).
    director.cover = match main_type {
        Some(kind) => main * form_params(FormKind::Main(kind)).sky_weight,
        None => 0.0,
    };
    sky.fog = sim.0.fog_level(ctx.day, ctx.day_frac);
}

/// Owner: clouds are WHITE by default and only grey once MORE than half the
/// sky is covered, then they darken toward overcast. One global tint on the
/// shared cloud material — a lone cumulus drifting a clear sky stays white; a
/// filling stratus deck greys the whole sky together.
fn tint_clouds(
    director: Res<CloudDirector>,
    library: Res<CloudLibrary>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    let Some(mat) = materials.get_mut(&library.material) else {
        return;
    };
    // 0 below half cover, ramping to full grey as cover → 1.
    let grey_t = smoothstep(((director.cover - 0.5) / 0.5).clamp(0.0, 1.0));
    let white = Vec3::splat(1.0);
    let overcast = Vec3::new(0.42, 0.44, 0.5);
    let c = white.lerp(overcast, grey_t);
    mat.base_color = Color::srgb(c.x, c.y, c.z);
}

/// Owner's regional modifiers, mapped onto the biome under the worm: the arid
/// interior (outback + Pilbara) starves clouds; the littoral biomes (coastal
/// bush, the Mediterranean south-west, island Tasmania) feed them. The big
/// savanna and temperate forest count as neither — they're vast regions whose
/// character isn't the shoreline. (True distance-to-coast would be nicer;
/// biome class is the cheap 90% of it.)
fn region_class(world_x: f32, world_z: f32) -> (bool, bool) {
    use crate::australia::AussieBiome::*;
    match crate::australia::biome_at_world(world_x, world_z) {
        AridOutback | Pilbara => (true, false),
        CoastalBush | Mediterranean | Tasmania => (false, true),
        _ => (false, false),
    }
}

// ===================== Local blocky voxel clouds =====================
//
// Owner's rotation-2 clarification: clouds are BLOCKY voxel clumps with LOD
// (coarse from afar, detailed up close, blocks tracing the type's silhouette),
// and weather is LOCAL — real clouds at real places, drifting on the wind, not
// a global switch. So the director's herald/main fullness becomes a *target
// population* of discrete cloud entities that spawn into a field around the
// worm, drift downwind, and age out the far side. Each cloud is a voxel clump
// (metaball union rasterised to a grid) built once into a shared mesh library
// at startup — three shape variants per type, each at three LOD block sizes —
// so spawning a cloud is just picking handles, never meshing on the hot path.

/// The forms the library carries: the cirrostratus herald plus every main
/// [`CloudType`]. Keyed with `PartialEq` so the spawner can look up the entry
/// for "the current main type" or "the herald".
#[derive(Clone, Copy, PartialEq, Eq)]
enum FormKind {
    Cirrostratus,
    Main(CloudType),
}

const ALL_FORMS: [FormKind; 10] = [
    FormKind::Cirrostratus,
    FormKind::Main(CloudType::Cirrus),
    FormKind::Main(CloudType::Cirrocumulus),
    FormKind::Main(CloudType::Altocumulus),
    FormKind::Main(CloudType::Stratocumulus),
    FormKind::Main(CloudType::Cumulus),
    FormKind::Main(CloudType::Cumulonimbus),
    FormKind::Main(CloudType::Nimbostratus),
    FormKind::Main(CloudType::Altostratus),
    FormKind::Main(CloudType::Stratus),
];

/// Static shape/placement recipe for one form. No per-type colour: the owner's
/// refinement is that clouds are WHITE by default and only grey once more than
/// half the sky is covered (applied globally in [`tint_clouds`]), and every
/// cloud shares ONE grid orientation aligned to the wind — so neither a tint
/// nor a per-cloud rotation lives here.
struct FormParams {
    /// Fine voxel grid size (cells) — the silhouette's bounding box. The local
    /// X axis is the cloud grid's "along-wind" axis (cirrus streaks run down
    /// it), so every cloud yaws to the wind and their lines stay parallel.
    dims: IVec3,
    /// Fine cell edge, feet (bigger clouds use bigger blocks).
    cell_ft: f32,
    /// Y of the cloud's centre, feet — clouds are titans' ceilings.
    altitude: f32,
    alt_jitter: f32,
    /// Population at full fullness for this layer.
    max_count: usize,
    scale_jitter: (f32, f32),
    /// How much of the sky one cloud of this form covers at full population —
    /// a broad stratus deck greys the sky (near 1), a few fair-weather cumulus
    /// barely dent it (~0.4). Feeds the >50%-cover greying.
    sky_weight: f32,
}

fn form_params(form: FormKind) -> FormParams {
    // Owner's altitude layering (2026-07-12, corrected) — and the review fix:
    // the trees are TITANS (a mountain ash tops ~1000 ft), so the whole sky had
    // to move up to clear the treetops. Flat wide "loaves" skim the canopy
    // (~320–440 ft), the puffy/mid types ride higher, the thin cirrus family
    // sits high, the cumulonimbus giants are the BIGGEST and tower highest of
    // the *main* clouds (anvil ~1150 ft), and the cirrostratus herald veil caps
    // ABOVE them all (~1250 ft). Everything stays inside the camera's 1600 ft
    // far plane even at the field's far edge — the guardrail the reviewer set.
    // (Cloud material is `fog_enabled: false`, so the 650–1350 ft distance fog
    // no longer swallows the high layers; the distance-blur pass still softens
    // the far ones.)
    use CloudType::*;
    match form {
        // The herald: a wide, very thin, broken pale veil — the TOP layer of
        // the sky, above even the cumulonimbus anvils.
        FormKind::Cirrostratus => FormParams {
            dims: IVec3::new(24, 2, 24),
            cell_ft: 15.0,
            altitude: 1250.0,
            alt_jitter: 30.0,
            max_count: 9,
            scale_jitter: (0.9, 1.25),
            sky_weight: 0.7,
        },
        FormKind::Main(kind) => match kind {
            // High wispy streaks combed along the wind.
            Cirrus => FormParams {
                dims: IVec3::new(20, 2, 6),
                cell_ft: 11.0,
                altitude: 940.0,
                alt_jitter: 40.0,
                max_count: 7,
                scale_jitter: (0.8, 1.3),
                sky_weight: 0.4,
            },
            // A mackerel sky: many small high rippled patches.
            Cirrocumulus => FormParams {
                dims: IVec3::new(14, 2, 12),
                cell_ft: 8.0,
                altitude: 880.0,
                alt_jitter: 30.0,
                max_count: 9,
                scale_jitter: (0.85, 1.2),
                sky_weight: 0.5,
            },
            // Mid-level puff patches.
            Altocumulus => FormParams {
                dims: IVec3::new(12, 3, 11),
                cell_ft: 9.0,
                altitude: 740.0,
                alt_jitter: 30.0,
                max_count: 9,
                scale_jitter: (0.85, 1.2),
                sky_weight: 0.6,
            },
            // A flat loaf skimming the canopy — wide, squat, rounded.
            Stratocumulus => FormParams {
                dims: IVec3::new(16, 6, 14),
                cell_ft: 10.0,
                altitude: 440.0,
                alt_jitter: 24.0,
                max_count: 8,
                scale_jitter: (0.85, 1.2),
                sky_weight: 0.85,
            },
            // The classic fluffy heap: flat bottom, cauliflower dome, riding
            // above the treetop loaves.
            Cumulus => FormParams {
                dims: IVec3::new(11, 8, 11),
                cell_ft: 8.0,
                altitude: 600.0,
                alt_jitter: 40.0,
                max_count: 6,
                scale_jitter: (0.8, 1.35),
                sky_weight: 0.4,
            },
            // The giant: biggest footprint, tallest column (~500 ft), anvil
            // topping the whole main layer well clear of the 1000 ft trees.
            Cumulonimbus => FormParams {
                dims: IVec3::new(14, 46, 14),
                cell_ft: 11.0,
                altitude: 900.0,
                alt_jitter: 20.0,
                max_count: 2,
                scale_jitter: (0.9, 1.15),
                sky_weight: 0.55,
            },
            // The rain loaf: the broadest, thickest low deck.
            Nimbostratus => FormParams {
                dims: IVec3::new(20, 6, 18),
                cell_ft: 11.0,
                altitude: 380.0,
                alt_jitter: 20.0,
                max_count: 8,
                scale_jitter: (0.9, 1.2),
                sky_weight: 1.0,
            },
            // The outro sheets: altostratus a wide mid loaf, stratus the lowest
            // loaf of all, right over the canopy.
            Altostratus => FormParams {
                dims: IVec3::new(18, 4, 16),
                cell_ft: 11.0,
                altitude: 660.0,
                alt_jitter: 24.0,
                max_count: 8,
                scale_jitter: (0.9, 1.2),
                sky_weight: 0.9,
            },
            Stratus => FormParams {
                dims: IVec3::new(18, 5, 16),
                cell_ft: 10.0,
                altitude: 320.0,
                alt_jitter: 18.0,
                max_count: 8,
                scale_jitter: (0.9, 1.2),
                sky_weight: 0.95,
            },
        },
    }
}

/// A metaball: cells within `radius` of `center` (both in fine-cell units)
/// fill. A union of these, rasterised, is the cloud — soft where they overlap,
/// blocky once voxelised, and cheap to trace any silhouette with.
struct Blob {
    center: Vec3,
    radius: f32,
}

/// Sculpt the blob field for a form — this is where each type gets its
/// silhouette. Seeded, so each (form, variant) is a stable shape.
fn form_blobs(form: FormKind, dims: IVec3, rng: &mut GardenRng) -> Vec<Blob> {
    let (dx, dy, dz) = (dims.x as f32, dims.y as f32, dims.z as f32);
    let mut blobs = Vec::new();
    let mid_y = dy * 0.5;
    match form {
        // Wide broken veil: a scatter of flat lumps spanning the thin slab.
        FormKind::Cirrostratus => {
            let n = ((dx * dz) / 22.0) as i32;
            for _ in 0..n {
                if !rng.chance(0.7) {
                    continue; // broken, not solid — gaps let sky through
                }
                blobs.push(Blob {
                    center: Vec3::new(rng.range(0.0, dx), mid_y, rng.range(0.0, dz)),
                    radius: rng.range(dy * 0.9, dy * 1.6).max(1.6),
                });
            }
        }
        FormKind::Main(kind) => match kind {
            // A streak: blobs strung along X, radius tapering to feathered ends.
            CloudType::Cirrus => {
                let n = 7;
                for i in 0..n {
                    let f = i as f32 / (n - 1) as f32;
                    let taper = (f * std::f32::consts::PI).sin(); // 0 at ends, 1 mid
                    blobs.push(Blob {
                        center: Vec3::new(
                            f * dx,
                            mid_y + rng.range(-0.4, 0.4),
                            dz * 0.5 + rng.range(-1.0, 1.0),
                        ),
                        radius: (dy * (0.7 + 1.4 * taper)).max(1.2),
                    });
                }
            }
            // Mackerel / puff patches: many small lumps scattered thin.
            CloudType::Cirrocumulus | CloudType::Altocumulus => {
                let n = ((dx * dz) / 10.0) as i32;
                for _ in 0..n {
                    blobs.push(Blob {
                        center: Vec3::new(
                            rng.range(0.0, dx),
                            mid_y + rng.range(-0.5, 0.5),
                            rng.range(0.0, dz),
                        ),
                        radius: rng.range(1.2, 2.2),
                    });
                }
            }
            // A lumpy heap piling up off a flat base: dome cut flat at y=0.
            CloudType::Cumulus => {
                // Broad base lumps.
                for _ in 0..4 {
                    blobs.push(Blob {
                        center: Vec3::new(
                            dx * 0.5 + rng.range(-dx * 0.22, dx * 0.22),
                            dy * 0.32,
                            dz * 0.5 + rng.range(-dz * 0.22, dz * 0.22),
                        ),
                        radius: rng.range(dy * 0.32, dy * 0.46),
                    });
                }
                // Cauliflower crown — smaller, higher, pulled toward centre.
                for _ in 0..6 {
                    let up = rng.range(0.5, 0.95);
                    blobs.push(Blob {
                        center: Vec3::new(
                            dx * 0.5 + rng.range(-dx * 0.28, dx * 0.28) * (1.0 - up),
                            dy * up,
                            dz * 0.5 + rng.range(-dz * 0.28, dz * 0.28) * (1.0 - up),
                        ),
                        radius: rng.range(dy * 0.18, dy * 0.30),
                    });
                }
            }
            // Tower + anvil: a boiling column, then a wide flat flare on top.
            // Step count scales with the (tall) grid so the column stays
            // continuous rather than beading up the height.
            CloudType::Cumulonimbus => {
                let steps = (dy as i32 / 3).max(8);
                for i in 0..steps {
                    let h = i as f32 / (steps - 1) as f32; // 0 base → 1 top
                    // Column narrows a touch with height until the anvil.
                    let jitter = dx * 0.12 * (1.0 - h);
                    blobs.push(Blob {
                        center: Vec3::new(
                            dx * 0.5 + rng.range(-jitter, jitter),
                            dy * (0.05 + 0.78 * h),
                            dz * 0.5 + rng.range(-jitter, jitter),
                        ),
                        radius: rng.range(dx * 0.24, dx * 0.32),
                    });
                }
                // The anvil: wide flat lumps smeared across the top.
                for _ in 0..6 {
                    blobs.push(Blob {
                        center: Vec3::new(
                            dx * 0.5 + rng.range(-dx * 0.5, dx * 0.5),
                            dy * rng.range(0.82, 0.96),
                            dz * 0.5 + rng.range(-dz * 0.5, dz * 0.5),
                        ),
                        radius: rng.range(dx * 0.22, dx * 0.34),
                    });
                }
            }
            // The loaf (owner: treetop-level clouds are wide, flat, roundish
            // loaves — wider than tall, low and squat). Stratocumulus,
            // Nimbostratus, Altostratus, Stratus route here. Blobs fill a
            // rounded (elliptical footprint) low dome: bigger toward the
            // centre, lower toward the rim, the flat bottom coming for free
            // from the grid floor clipping the low-centred blobs.
            _ => {
                let (cx, cz) = (dx * 0.5, dz * 0.5);
                let n = ((dx * dz) / 5.0) as i32;
                for _ in 0..n {
                    let (ex, ez) = (rng.range(-1.0, 1.0), rng.range(-1.0, 1.0));
                    let rr = ex * ex + ez * ez;
                    if rr > 1.0 {
                        continue; // round off the corners → an oval loaf
                    }
                    let edge = 1.0 - rr; // 1 at centre, 0 at the rim
                    blobs.push(Blob {
                        center: Vec3::new(
                            cx + ex * (dx * 0.46),
                            dy * 0.3 + edge * dy * 0.28 * rng.range(0.6, 1.0),
                            cz + ez * (dz * 0.46),
                        ),
                        radius: rng.range(1.7, 2.6) * (0.7 + 0.5 * edge),
                    });
                }
            }
        },
    }
    blobs
}

/// A boolean occupancy grid — the rasterised cloud.
struct VoxelGrid {
    dims: IVec3,
    cells: Vec<bool>,
}

impl VoxelGrid {
    fn empty(dims: IVec3) -> Self {
        Self {
            dims,
            cells: vec![false; (dims.x * dims.y * dims.z).max(0) as usize],
        }
    }

    fn idx(&self, x: i32, y: i32, z: i32) -> usize {
        ((x * self.dims.y + y) * self.dims.z + z) as usize
    }

    fn get(&self, x: i32, y: i32, z: i32) -> bool {
        if x < 0 || y < 0 || z < 0 || x >= self.dims.x || y >= self.dims.y || z >= self.dims.z {
            return false;
        }
        self.cells[self.idx(x, y, z)]
    }

    #[cfg_attr(not(test), allow(dead_code))]
    fn count(&self) -> usize {
        self.cells.iter().filter(|c| **c).count()
    }
}

/// Rasterise the blob union into a fine grid.
fn rasterise(dims: IVec3, blobs: &[Blob]) -> VoxelGrid {
    let mut grid = VoxelGrid::empty(dims);
    for x in 0..dims.x {
        for y in 0..dims.y {
            for z in 0..dims.z {
                let p = Vec3::new(x as f32 + 0.5, y as f32 + 0.5, z as f32 + 0.5);
                let filled = blobs
                    .iter()
                    .any(|b| p.distance_squared(b.center) < b.radius * b.radius);
                if filled {
                    let i = grid.idx(x, y, z);
                    grid.cells[i] = true;
                }
            }
        }
    }
    grid
}

/// Minimum fraction of fine sub-cells a coarse cell needs to survive an LOD
/// step — low, so a coarse block appears wherever the cloud has any real mass
/// there and the silhouette doesn't shrink away with distance.
const CLOUD_FILL: f32 = 0.18;

/// Collapse a grid by `factor` for a coarser LOD rung (majority-ish vote).
fn downsample(grid: &VoxelGrid, factor: i32) -> VoxelGrid {
    if factor <= 1 {
        return VoxelGrid { dims: grid.dims, cells: grid.cells.clone() };
    }
    let nd = IVec3::new(
        (grid.dims.x + factor - 1) / factor,
        (grid.dims.y + factor - 1) / factor,
        (grid.dims.z + factor - 1) / factor,
    );
    let mut out = VoxelGrid::empty(nd);
    for cx in 0..nd.x {
        for cy in 0..nd.y {
            for cz in 0..nd.z {
                let (mut filled, mut total) = (0i32, 0i32);
                for dx in 0..factor {
                    for dy in 0..factor {
                        for dz in 0..factor {
                            let (x, y, z) =
                                (cx * factor + dx, cy * factor + dy, cz * factor + dz);
                            if x >= grid.dims.x || y >= grid.dims.y || z >= grid.dims.z {
                                continue;
                            }
                            total += 1;
                            if grid.get(x, y, z) {
                                filled += 1;
                            }
                        }
                    }
                }
                if total > 0 && filled as f32 / total as f32 >= CLOUD_FILL {
                    let i = out.idx(cx, cy, cz);
                    out.cells[i] = true;
                }
            }
        }
    }
    out
}

// The six cube faces: (neighbour offset, the four corner offsets of the quad
// wound CCW when viewed from outside, and the outward normal).
const FACES: [([i32; 3], [[f32; 3]; 4], [f32; 3]); 6] = [
    // +X
    ([1, 0, 0], [[1., 0., 0.], [1., 1., 0.], [1., 1., 1.], [1., 0., 1.]], [1., 0., 0.]),
    // -X
    ([-1, 0, 0], [[0., 0., 1.], [0., 1., 1.], [0., 1., 0.], [0., 0., 0.]], [-1., 0., 0.]),
    // +Y (top — brightest)
    ([0, 1, 0], [[0., 1., 0.], [0., 1., 1.], [1., 1., 1.], [1., 1., 0.]], [0., 1., 0.]),
    // -Y (bottom — darkest)
    ([0, -1, 0], [[0., 0., 1.], [0., 0., 0.], [1., 0., 0.], [1., 0., 1.]], [0., -1., 0.]),
    // +Z
    ([0, 0, 1], [[1., 0., 1.], [1., 1., 1.], [0., 1., 1.], [0., 0., 1.]], [0., 0., 1.]),
    // -Z
    ([0, 0, -1], [[0., 0., 0.], [0., 1., 0.], [1., 1., 0.], [1., 0., 0.]], [0., 0., -1.]),
];

/// Per-face baked shade (top bright → bottom dark) so the block form reads
/// with volume even under flat ambient; the sun's lighting layers on top.
const FACE_SHADE: [f32; 6] = [0.82, 0.82, 1.0, 0.55, 0.82, 0.82];

/// Build a culled-cube mesh from a grid: only faces between a filled cell and
/// empty space are emitted, centred so the mesh's local origin is the cloud's
/// centre. Vertex colours carry the baked top-bright/bottom-dark shading.
fn build_cloud_mesh(grid: &VoxelGrid, cell_ft: f32) -> Mesh {
    let dims = grid.dims;
    let origin = -Vec3::new(dims.x as f32, dims.y as f32, dims.z as f32) * 0.5 * cell_ft;
    let mut positions: Vec<[f32; 3]> = Vec::new();
    let mut normals: Vec<[f32; 3]> = Vec::new();
    let mut colors: Vec<[f32; 4]> = Vec::new();
    let mut uvs: Vec<[f32; 2]> = Vec::new();
    let mut indices: Vec<u32> = Vec::new();
    let y_span = (dims.y as f32 - 1.0).max(1.0);

    for x in 0..dims.x {
        for y in 0..dims.y {
            for z in 0..dims.z {
                if !grid.get(x, y, z) {
                    continue;
                }
                let cell = Vec3::new(x as f32, y as f32, z as f32);
                // Undersides in shadow: cells low in the cloud run darker.
                let height_shade = 0.62 + 0.38 * (y as f32 / y_span);
                for (fi, (noff, corners, normal)) in FACES.iter().enumerate() {
                    if grid.get(x + noff[0], y + noff[1], z + noff[2]) {
                        continue; // interior face — skip
                    }
                    let base = positions.len() as u32;
                    let shade = FACE_SHADE[fi] * (0.72 + 0.28 * height_shade);
                    for corner in corners {
                        let p = origin
                            + (cell + Vec3::from_array(*corner)) * cell_ft;
                        positions.push([p.x, p.y, p.z]);
                        normals.push(*normal);
                        colors.push([shade, shade, shade, 1.0]);
                    }
                    uvs.extend_from_slice(&[[0., 0.], [0., 1.], [1., 1.], [1., 0.]]);
                    indices.extend_from_slice(&[
                        base, base + 1, base + 2, base, base + 2, base + 3,
                    ]);
                }
            }
        }
    }

    let mut mesh = Mesh::new(
        PrimitiveTopology::TriangleList,
        RenderAssetUsages::RENDER_WORLD,
    );
    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, positions);
    mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, normals);
    mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, uvs);
    mesh.insert_attribute(Mesh::ATTRIBUTE_COLOR, colors);
    mesh.insert_indices(Indices::U32(indices));
    mesh
}

/// LOD block-size multipliers on the fine grid — 1× up close, 4× blocks far
/// off, same near=detail/far=coarse ladder the foliage uses.
const CLOUD_LOD_FACTORS: [i32; 3] = [1, 2, 4];
/// Camera-distance cutoffs (ft) to step coarser. Pushed well out: clouds are
/// huge and the distance blur + haze hide the swaps, so even the coarse rung
/// only shows once it's tiny on screen.
const CLOUD_LOD_DISTANCES_FT: [f32; 2] = [240.0, 520.0];
/// Shape variants baked per form, so a sky of one type isn't all one cloud.
const CLOUD_VARIANTS: usize = 3;

/// One baked cloud shape: its LOD meshes plus the world-space extents the
/// spawner/LOD/despawn logic needs (identical across rungs — only block size
/// changes, not the cloud's size).
struct CloudVariant {
    lods: [Handle<Mesh>; CLOUD_LOD_FACTORS.len()],
    /// Horizontal bounding radius, feet.
    radius: f32,
    /// Vertical extent, feet. Read by the shadow/rain follow-up PRs (footprint
    /// + rain-column base), baked now so the geometry is on hand.
    #[allow(dead_code)]
    height: f32,
}

/// Everything baked for one form.
struct FormAssets {
    form: FormKind,
    params: FormParams,
    variants: Vec<CloudVariant>,
}

/// The shared cloud mesh + material library, built once at startup. One
/// material for ALL clouds (owner: white by default), greyed globally by
/// [`tint_clouds`] once the sky is more than half covered.
#[derive(Resource)]
struct CloudLibrary {
    forms: Vec<FormAssets>,
    material: Handle<StandardMaterial>,
}

impl CloudLibrary {
    fn entry(&self, form: FormKind) -> &FormAssets {
        // ALL_FORMS order is fixed, so a linear scan over ≤10 is trivial.
        self.forms
            .iter()
            .find(|f| f.form == form)
            .unwrap_or(&self.forms[0])
    }
}

fn setup_cloud_library(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    // One shared white material. Baked vertex colours give each block its
    // top-bright/bottom-dark form; this base is retinted white→grey globally by
    // tint_clouds once cover passes 50%.
    let material = materials.add(StandardMaterial {
        base_color: Color::WHITE,
        perceptual_roughness: 1.0,
        reflectance: 0.08,
        // Lit (the sun models the blocks), but fog OFF: the high layers now sit
        // at 600–1250 ft — inside the 650–1350 ft distance fog that melts the
        // terrain horizon — so with fog on they'd wash out to sky. Punching
        // through it (like the sun/moon discs do) keeps them legible; the
        // depth-aware distance-blur pass still softens the far ones.
        fog_enabled: false,
        ..default()
    });
    let mut forms = Vec::new();
    for form in ALL_FORMS {
        let params = form_params(form);
        let mut variants = Vec::new();
        for v in 0..CLOUD_VARIANTS {
            // Stable per (form, variant): the sky's cloud shapes repeat across
            // sessions, like the star field.
            let seed = 0xC10D_5EED ^ ((form_seed(form) << 8) + v as u64);
            let mut rng = GardenRng::new(seed);
            let blobs = form_blobs(form, params.dims, &mut rng);
            let fine = rasterise(params.dims, &blobs);
            let lods = std::array::from_fn(|i| {
                let g = downsample(&fine, CLOUD_LOD_FACTORS[i]);
                meshes.add(build_cloud_mesh(&g, params.cell_ft * CLOUD_LOD_FACTORS[i] as f32))
            });
            let radius =
                0.5 * params.dims.x.max(params.dims.z) as f32 * params.cell_ft;
            let height = params.dims.y as f32 * params.cell_ft;
            variants.push(CloudVariant { lods, radius, height });
        }
        forms.push(FormAssets { form, params, variants });
    }
    println!("☁️ Cloud library baked: {} forms × {CLOUD_VARIANTS} variants × {} LODs.",
        forms.len(), CLOUD_LOD_FACTORS.len());
    commands.insert_resource(CloudLibrary { forms, material });
}

/// A small stable per-form salt for the variant seeds.
fn form_seed(form: FormKind) -> u64 {
    match form {
        FormKind::Cirrostratus => 1,
        FormKind::Main(k) => 2 + k as u64,
    }
}

/// A live cloud drifting through the field around the worm.
#[derive(Component)]
struct Cloud {
    form: FormKind,
    /// Bounding radius (ft) of the chosen variant. Read by the shadow/rain
    /// follow-up PRs (a cloud's ground shadow + rain footprint scale with it).
    #[allow(dead_code)]
    radius: f32,
    /// Presence 0..1, eased toward `target`; scales the cloud in/out so nothing
    /// pops. Despawned once it has faded away with nothing wanting it back.
    present: f32,
    /// 1 while the front still wants this cloud, 0 once it's retiring.
    target: f32,
    base_scale: f32,
}

/// On the cloud root: which LOD rung is showing (hysteresis lives here).
#[derive(Component)]
struct CloudLodGroup {
    level: usize,
}

/// On each rung child: which rung it is.
#[derive(Component)]
struct CloudLod {
    level: usize,
}

/// Where new clouds enter (a disc around the worm), and where a drifting cloud
/// is far enough off to retire. Clouds live in world space, so the worm can
/// crawl clear of the weather — the field just follows where it currently is.
const CLOUD_SPAWN_RADIUS: f32 = 560.0;
const CLOUD_DESPAWN_RADIUS: f32 = 800.0;
/// Drift speed (ft/s) per unit of wind strength — a gale marches the ceiling
/// visibly, a calm day barely stirs it.
const CLOUD_DRIFT_FTPS: f32 = 3.2;
/// Seconds for a cloud to swell in or fade out.
const CLOUD_GROW_SECS: f32 = 7.0;
/// Cap new clouds per frame so a front filling in doesn't spawn a wall at once.
const CLOUD_SPAWNS_PER_FRAME: usize = 2;

/// Keep the local cloud population matching the director's targets: spawn new
/// clouds of the wanted forms on the upwind side of the field, and retire any
/// that are excess or of a stale type (they fade + drift off). This is what
/// makes the weather LOCAL — discrete clouds, not a global flag.
fn spawn_local_clouds(
    mut commands: Commands,
    director: Res<CloudDirector>,
    library: Res<CloudLibrary>,
    mut wind: ResMut<Wind>,
    cam_q: Query<&Transform, With<Camera>>,
    mut clouds: Query<(Entity, &mut Cloud, &Transform), Without<Camera>>,
) {
    let Ok(cam) = cam_q.get_single() else {
        return;
    };
    let center = Vec2::new(cam.translation.x, cam.translation.z);

    // Targets from the director's fullness.
    let herald_target =
        (director.herald * library.entry(FormKind::Cirrostratus).params.max_count as f32)
            .round() as i32;
    let (main_form, main_target) = match director.main_type {
        Some(kind) => {
            let f = FormKind::Main(kind);
            (Some(f), (director.main * library.entry(f).params.max_count as f32).round() as i32)
        }
        None => (None, 0),
    };

    // Where clouds enter: the upwind edge (owner: weather drifts in from a
    // distance, nothing pops overhead). `dir` is always a unit heading, so this
    // is well-defined even in dead calm — the front just sits far off then.
    let upwind = -wind.dir;
    // One shared grid orientation for the WHOLE sky, aligned to the wind: every
    // cloud's local +X (its "along-wind" axis, down which cirrus streaks run)
    // points along `wind.dir`, so their blocks and lines stay parallel.
    let yaw_shared = (-wind.dir.y).atan2(wind.dir.x);

    // Tally keepers, retire the unwanted, and gather clump anchors: keepers
    // still out in the upwind approach that a new cloud can bunch onto (owner:
    // clouds clump together rather than scatter evenly).
    let mut herald_have = 0i32;
    let mut main_have = 0i32;
    let mut herald_anchors: Vec<Vec3> = Vec::new();
    let mut main_anchors: Vec<Vec3> = Vec::new();
    for (_entity, mut cloud, tf) in &mut clouds {
        let is_herald = matches!(cloud.form, FormKind::Cirrostratus);
        let wanted = if is_herald {
            cloud.target > 0.5 && herald_target > 0
        } else {
            cloud.target > 0.5 && Some(cloud.form) == main_form
        };
        if !wanted {
            cloud.target = 0.0; // stale type or phase surplus → drift off
            continue;
        }
        if is_herald {
            herald_have += 1;
        } else {
            main_have += 1;
        }
        // Anchor if it's still out in the upwind half (so clumps form on the
        // approach and drift across together, never mid-sky overhead).
        let off = Vec2::new(tf.translation.x - center.x, tf.translation.z - center.y);
        if off.dot(upwind) > 0.0 && off.length() > CLOUD_SPAWN_RADIUS * 0.4 {
            let anchors = if is_herald { &mut herald_anchors } else { &mut main_anchors };
            anchors.push(tf.translation);
        }
    }

    // Retire any surplus above target.
    let trim = |clouds: &mut Query<(Entity, &mut Cloud, &Transform), Without<Camera>>,
                want_herald: bool,
                mut over: i32| {
        if over <= 0 {
            return;
        }
        for (_, mut cloud, _) in clouds.iter_mut() {
            if over <= 0 {
                break;
            }
            let is_herald = matches!(cloud.form, FormKind::Cirrostratus);
            if is_herald == want_herald && cloud.target > 0.5 {
                cloud.target = 0.0;
                over -= 1;
            }
        }
    };
    trim(&mut clouds, true, herald_have - herald_target);
    trim(&mut clouds, false, main_have - main_target);

    // Spawn toward the targets, a couple per frame.
    let mut budget = CLOUD_SPAWNS_PER_FRAME;
    let spawn_one = |commands: &mut Commands, form: FormKind, wind: &mut Wind, anchors: &[Vec3]| {
        let assets = library.entry(form);
        let vi = (wind.rng.next_f32() * CLOUD_VARIANTS as f32) as usize % CLOUD_VARIANTS;
        let variant = &assets.variants[vi];
        let p = &assets.params;

        // Clump onto an existing approaching cloud most of the time; otherwise
        // enter fresh at the upwind edge with a wide crosswind spread (a front
        // line, not a single point).
        let pos = if !anchors.is_empty() && wind.rng.chance(0.6) {
            let a = anchors[(wind.rng.next_f32() * anchors.len() as f32) as usize % anchors.len()];
            a + Vec3::new(
                wind.rng.range(-90.0, 90.0),
                wind.rng.range(-p.alt_jitter, p.alt_jitter),
                wind.rng.range(-90.0, 90.0),
            )
        } else {
            let perp = Vec2::new(-upwind.y, upwind.x);
            let edge = upwind * CLOUD_SPAWN_RADIUS
                + perp * wind.rng.range(-CLOUD_SPAWN_RADIUS * 0.6, CLOUD_SPAWN_RADIUS * 0.6);
            Vec3::new(
                center.x + edge.x,
                p.altitude + wind.rng.range(-p.alt_jitter, p.alt_jitter),
                center.y + edge.y,
            )
        };
        let base_scale = wind.rng.range(p.scale_jitter.0, p.scale_jitter.1);

        let root = commands
            .spawn((
                Cloud {
                    form,
                    radius: variant.radius * base_scale,
                    present: 0.0,
                    target: 1.0,
                    base_scale,
                },
                CloudLodGroup { level: 0 },
                Transform {
                    translation: pos,
                    rotation: Quat::from_rotation_y(yaw_shared),
                    scale: Vec3::splat(0.001),
                },
                Visibility::default(),
            ))
            .id();
        for (level, mesh) in variant.lods.iter().enumerate() {
            let child = commands
                .spawn((
                    CloudLod { level },
                    Mesh3d(mesh.clone()),
                    MeshMaterial3d(library.material.clone()),
                    NotShadowCaster,
                    // Only rung 0 starts visible; the LOD system corrects it.
                    if level == 0 { Visibility::Inherited } else { Visibility::Hidden },
                ))
                .id();
            commands.entity(root).add_child(child);
        }
    };

    while budget > 0 && herald_have < herald_target {
        spawn_one(&mut commands, FormKind::Cirrostratus, &mut wind, &herald_anchors);
        herald_have += 1;
        budget -= 1;
    }
    if let Some(f) = main_form {
        while budget > 0 && main_have < main_target {
            spawn_one(&mut commands, f, &mut wind, &main_anchors);
            main_have += 1;
            budget -= 1;
        }
    }
}

/// Drift every cloud downwind, ease its presence toward its target (grow-in /
/// retire), scale it accordingly, and despawn the ones that have faded away or
/// blown clear off the field.
fn drive_clouds(
    time: Res<Time>,
    wind: Res<Wind>,
    mut commands: Commands,
    cam_q: Query<&Transform, (With<Camera>, Without<Cloud>)>,
    mut clouds: Query<(Entity, &mut Cloud, &mut Transform), Without<Camera>>,
) {
    let dt = time.delta_secs();
    // Drift AND grid orientation both follow the wind (owner): clouds ride the
    // wind vector, and their shared grid axis stays aligned to it as it wanders.
    let drift = Vec3::new(wind.dir.x, 0.0, wind.dir.y) * wind.strength * CLOUD_DRIFT_FTPS * dt;
    let yaw_shared = Quat::from_rotation_y((-wind.dir.y).atan2(wind.dir.x));
    let center = cam_q
        .get_single()
        .map(|c| Vec2::new(c.translation.x, c.translation.z))
        .unwrap_or(Vec2::ZERO);

    for (entity, mut cloud, mut tf) in &mut clouds {
        tf.translation += drift;

        // Blown off the far edge → start retiring.
        let flat = Vec2::new(tf.translation.x - center.x, tf.translation.z - center.y);
        if flat.length() > CLOUD_DESPAWN_RADIUS {
            cloud.target = 0.0;
        }

        // Ease presence, scale by it (from a small floor so a fresh cloud isn't
        // a single degenerate point on frame one).
        let rate = dt / CLOUD_GROW_SECS;
        cloud.present += (cloud.target - cloud.present).clamp(-rate, rate);
        if cloud.target <= 0.0 && cloud.present <= 0.03 {
            commands.entity(entity).despawn_recursive();
            continue;
        }
        let grow = smoothstep(cloud.present);
        tf.scale = Vec3::splat(cloud.base_scale * (0.12 + 0.88 * grow));
        tf.rotation = yaw_shared;
    }
}

/// Swap each cloud's visible LOD rung by camera distance, with the same 10%
/// hysteresis foliage uses so a cloud hovering at a cutoff can't strobe.
fn update_cloud_lod(
    cam_q: Query<&Transform, With<Camera>>,
    mut groups: Query<(&mut CloudLodGroup, &Transform, &Children), Without<Camera>>,
    mut rungs: Query<(&CloudLod, &mut Visibility)>,
) {
    let Ok(cam) = cam_q.get_single() else {
        return;
    };
    let cam_pos = cam.translation;
    for (mut group, tf, children) in &mut groups {
        let dist = cam_pos.distance(tf.translation);
        let mut level = group.level;
        while level < CLOUD_LOD_DISTANCES_FT.len() && dist >= CLOUD_LOD_DISTANCES_FT[level] {
            level += 1;
        }
        while level > 0 && dist < CLOUD_LOD_DISTANCES_FT[level - 1] * 0.9 {
            level -= 1;
        }
        if level != group.level {
            group.level = level;
        }
        for &child in children {
            if let Ok((lod, mut vis)) = rungs.get_mut(child) {
                let want = if lod.level == level {
                    Visibility::Inherited
                } else {
                    Visibility::Hidden
                };
                if *vis != want {
                    *vis = want;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx(now: f32, season: Season, wind: f32, arid: bool, coastal: bool) -> CloudCtx {
        CloudCtx {
            now,
            day: 0,
            day_frac: 0.5, // midday — outside the fog window
            season,
            wind_strength: wind,
            arid,
            coastal,
        }
    }

    #[test]
    fn seasons_cycle_from_day() {
        use Season::*;
        let want = [Summer, Autumn, Winter, Spring];
        for day in 0..(DAYS_PER_SEASON * 8) {
            let expect = want[((day / DAYS_PER_SEASON) % 4) as usize];
            assert_eq!(Season::from_day(day), expect, "day {day}");
        }
    }

    /// The modifier math is exact — pin it to the owner's numbers.
    #[test]
    fn formation_chance_matches_spec() {
        let calm = FORM_BASE_CHANCE;
        assert_eq!(formation_chance(Season::Summer, 0.0, false, false), calm);
        // Winter +30%.
        assert_eq!(formation_chance(Season::Winter, 0.0, false, false), calm * 1.3);
        // Arid −40% of any type.
        assert_eq!(formation_chance(Season::Summer, 0.0, true, false), calm * 0.6);
        // Coastal +10%.
        assert_eq!(formation_chance(Season::Summer, 0.0, false, true), calm * 1.1);
        // Wind blowing → 70% the weather turns cloudy.
        assert_eq!(formation_chance(Season::Summer, 3.0, false, false), 0.70);
        // Modifiers stack on the windy base too (and stay under the cap).
        let windy_winter = formation_chance(Season::Winter, 5.0, false, false);
        assert!((windy_winter - 0.91).abs() < 1e-6, "got {windy_winter}");
        assert!(formation_chance(Season::Winter, 5.0, false, true) <= 0.95);
    }

    /// Fixed-seed distribution: the four main events land ~25% each.
    #[test]
    fn main_event_odds_are_uniform() {
        let mut rng = GardenRng::new(0xC1_0DD5);
        let mut counts = [0usize; 4];
        const N: usize = 40_000;
        for _ in 0..N {
            match roll_main(&mut rng) {
                CloudType::Cirrus => counts[0] += 1,
                CloudType::Altocumulus => counts[1] += 1,
                CloudType::Stratocumulus => counts[2] += 1,
                CloudType::Cirrocumulus => counts[3] += 1,
                other => panic!("{other:?} is not a main event"),
            }
        }
        for (i, &c) in counts.iter().enumerate() {
            let share = c as f32 / N as f32;
            assert!((share - 0.25).abs() < 0.02, "slot {i}: {share}");
        }
    }

    /// Fixed-seed distribution: cirrus resolves per the owner's 10/45/50
    /// weights (normalized over 105).
    #[test]
    fn cirrus_escalation_odds_match_weights() {
        let mut rng = GardenRng::new(0x57012);
        let (mut cb, mut ns, mut cu) = (0usize, 0usize, 0usize);
        const N: usize = 40_000;
        for _ in 0..N {
            match roll_escalation(&mut rng) {
                CloudType::Cumulonimbus => cb += 1,
                CloudType::Nimbostratus => ns += 1,
                CloudType::Cumulus => cu += 1,
                other => panic!("{other:?} is not an escalation"),
            }
        }
        let n = N as f32;
        assert!((cb as f32 / n - 10.0 / 105.0).abs() < 0.02, "cumulonimbus {cb}");
        assert!((ns as f32 / n - 45.0 / 105.0).abs() < 0.02, "nimbostratus {ns}");
        assert!((cu as f32 / n - 50.0 / 105.0).abs() < 0.02, "cumulus {cu}");
    }

    /// Fixed-seed distribution: fog mornings track the seasonal chances.
    #[test]
    fn fog_odds_track_season() {
        for (season, want) in [
            (Season::Winter, 0.70),
            (Season::Autumn, 0.70),
            (Season::Summer, 0.20),
            (Season::Spring, 0.40),
        ] {
            let mut sim = CloudSim::new(0xF06, None, false);
            let mut foggy = 0usize;
            const DAYS: u32 = 4_000;
            for day in 0..DAYS {
                let c = CloudCtx {
                    now: 0.0,
                    day,
                    day_frac: 0.25, // 6:00 — inside the fog window
                    season,
                    wind_strength: 0.0,
                    arid: false,
                    coastal: false,
                };
                sim.step(&c);
                if sim.fog_today {
                    foggy += 1;
                }
            }
            let rate = foggy as f32 / DAYS as f32;
            assert!((rate - want).abs() < 0.03, "{season:?}: rate {rate}, want {want}");
        }
    }

    /// Drive a real sim through whole systems and hold the owner's invariants:
    /// the phase order is the spec's procession, the main layer never fills
    /// without the cirrostratus herald, and only legal types appear per phase.
    #[test]
    fn full_cycle_keeps_herald_invariant_and_order() {
        let mut sim = CloudSim::new(0xA57_0C1, None, false);
        let mut cycles = 0;
        let mut t = 0.0f32;
        while cycles < 12 && t < 8.0 * 3600.0 {
            t += 20.0;
            sim.step(&ctx(t, Season::Winter, 4.0, false, true));

            let (herald, main_type, main) = sim.front_order(t);
            // Owner: the main layer never fills without the herald above it.
            assert!(
                main <= herald + 1e-4,
                "main fullness ({main}) outgrew the veil ({herald}) in {:?}",
                sim.phase
            );
            if let Some(kind) = main_type {
                match sim.phase {
                    CloudPhase::Main { .. } => assert!(
                        !matches!(kind, CloudType::Altostratus | CloudType::Stratus),
                        "{kind:?} can't be a main event"
                    ),
                    CloudPhase::Outro { .. } => assert!(
                        matches!(kind, CloudType::Altostratus | CloudType::Stratus),
                        "{kind:?} can't be an outro"
                    ),
                    _ => panic!("visible main layer in phase {:?}", sim.phase),
                }
            }
            if matches!(sim.phase, CloudPhase::Clear) {
                cycles += 1;
            }
        }

        // The procession is linear: Clear → onset → main(s) → outro → recede.
        let legal_after = |a: &CloudPhase, b: &CloudPhase| -> bool {
            matches!(
                (a, b),
                (CloudPhase::Clear, CloudPhase::HeraldOnset { .. })
                    | (CloudPhase::HeraldOnset { .. }, CloudPhase::Main { .. })
                    | (CloudPhase::Main { .. }, CloudPhase::Main { .. })
                    | (CloudPhase::Main { .. }, CloudPhase::Outro { .. })
                    | (CloudPhase::Outro { .. }, CloudPhase::HeraldReceding)
                    | (CloudPhase::HeraldReceding, CloudPhase::Clear)
            )
        };
        let mut sim = CloudSim::new(0xA57_0C1, None, false);
        let mut prev = sim.phase;
        let mut t = 0.0f32;
        while t < 8.0 * 3600.0 {
            t += 20.0;
            sim.step(&ctx(t, Season::Winter, 4.0, false, true));
            if sim.phase != prev {
                assert!(legal_after(&prev, &sim.phase), "{prev:?} → {:?}", sim.phase);
                prev = sim.phase;
            }
        }
    }

    /// Arid skies really do see ~40% fewer fronts than neutral ones — run the
    /// whole machine, not just the chance function.
    #[test]
    fn arid_region_forms_fewer_fronts() {
        let count_fronts = |arid: bool, seed: u64| -> usize {
            let mut sim = CloudSim::new(seed, None, false);
            let mut fronts = 0;
            let mut was_clear = true;
            let mut t = 0.0f32;
            while t < 40.0 * 3600.0 {
                t += 30.0;
                sim.step(&ctx(t, Season::Summer, 0.0, arid, false));
                let clear = matches!(sim.phase, CloudPhase::Clear);
                if was_clear && !clear {
                    fronts += 1;
                }
                was_clear = clear;
            }
            fronts
        };
        let (mut neutral, mut arid) = (0, 0);
        for seed in 0..6u64 {
            neutral += count_fronts(false, 0xBEEF ^ seed.wrapping_mul(0x9E37_79B9));
            arid += count_fronts(true, 0xBEEF ^ seed.wrapping_mul(0x9E37_79B9));
        }
        assert!(
            arid < neutral,
            "arid ({arid}) should see fewer fronts than neutral ({neutral})"
        );
        let ratio = arid as f32 / neutral as f32;
        assert!((0.4..0.9).contains(&ratio), "arid/neutral front ratio {ratio}");
    }

    /// The altostratus/stratus outro really is a coin flip.
    #[test]
    fn outro_split_is_even() {
        let mut sim = CloudSim::new(0x0072_0511, None, false);
        let (mut alto, mut strat) = (0usize, 0usize);
        let mut t = 0.0f32;
        while alto + strat < 60 && t < 200.0 * 3600.0 {
            t += 60.0;
            sim.step(&ctx(t, Season::Winter, 5.0, false, true));
            if let CloudPhase::Outro { cloud } = sim.phase {
                if t - 60.0 < sim.phase_start {
                    match cloud {
                        CloudType::Altostratus => alto += 1,
                        CloudType::Stratus => strat += 1,
                        other => panic!("{other:?} outro"),
                    }
                }
            }
        }
        let total = (alto + strat) as f32;
        assert!(total >= 60.0, "too few outros ({total})");
        let share = alto as f32 / total;
        assert!((share - 0.5).abs() < 0.2, "altostratus share {share}");
    }

    // ---- Blocky voxel cloud generation ----

    /// Every form rasterises to a non-empty voxel clump that stays inside its
    /// declared bounding box — the silhouette the mesh will trace.
    #[test]
    fn every_form_generates_a_bounded_clump() {
        for form in ALL_FORMS {
            let p = form_params(form);
            let mut rng = GardenRng::new(0x5EED ^ form_seed(form));
            let blobs = form_blobs(form, p.dims, &mut rng);
            assert!(!blobs.is_empty(), "{:?} had no blobs", form_seed(form));
            let grid = rasterise(p.dims, &blobs);
            let filled = grid.count();
            assert!(filled > 0, "form seed {} rasterised empty", form_seed(form));
            // Not a solid brick — a cloud has surface, i.e. some empty cells.
            let total = (p.dims.x * p.dims.y * p.dims.z) as usize;
            assert!(filled < total, "form seed {} filled its whole box", form_seed(form));
        }
    }

    /// Coarser LOD rungs shed triangles — the whole point of the ladder.
    #[test]
    fn coarser_lod_sheds_geometry() {
        for form in ALL_FORMS {
            let p = form_params(form);
            let mut rng = GardenRng::new(0x1D0 ^ form_seed(form));
            let blobs = form_blobs(form, p.dims, &mut rng);
            let fine = rasterise(p.dims, &blobs);
            let fine_mesh = build_cloud_mesh(&fine, p.cell_ft);
            let coarse = downsample(&fine, 4);
            let coarse_mesh = build_cloud_mesh(&coarse, p.cell_ft * 4.0);
            let fv = fine_mesh.count_vertices();
            let cv = coarse_mesh.count_vertices();
            assert!(fv > 0, "form seed {} fine mesh empty", form_seed(form));
            assert!(
                cv <= fv,
                "form seed {}: coarse rung ({cv}) not lighter than fine ({fv})",
                form_seed(form)
            );
        }
    }

    /// The culled mesher emits only boundary faces: a solid interior cell
    /// contributes nothing. A single lone voxel shows all six faces.
    #[test]
    fn culled_mesh_skips_interior_faces() {
        let mut grid = VoxelGrid::empty(IVec3::new(1, 1, 1));
        grid.cells[0] = true;
        let mesh = build_cloud_mesh(&grid, 10.0);
        // 6 faces × 4 verts.
        assert_eq!(mesh.count_vertices(), 24, "a lone voxel should be a full cube");
    }
}
