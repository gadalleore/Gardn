//! Weather: the global wind — a slowly wandering direction with a gusty 0–5
//! strength that re-rolls toward calm — and the ribbon streamers that make it
//! visible and audible as they race past the worm. The rolled level is only
//! the *base*: a private gust engine layers surges, lulls, and flutter on top
//! so `strength` breathes like real air instead of holding a flat line.
//! `WeatherPlugin` owns the `Wind` resource, the streamer assets, and the wind
//! systems; grass, leaves and trees all read `crate::weather::Wind` to sway
//! with it.
//!
//! Rotation 2 adds the season clock and the cloud state machine (owner's
//! design doc, Weather section): sunny → cirrostratus herald → one main cloud
//! event → altostratus/stratus outro → the herald recedes → sunny again, with
//! seasonal / regional / wind modifiers on how often fronts form, plus a
//! morning-fog roll. This file holds the pure logic and publishes it as the
//! [`CloudState`] resource; the procedural cloud rendering reads that and
//! ships separately.

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
            .init_resource::<GustTexture>()
            .insert_resource(SeasonClock::from_env())
            .insert_resource(CloudSimRes(CloudSim::from_env()))
            .init_resource::<CloudState>()
            .add_systems(Startup, setup_streamers)
            .add_systems(
                Update,
                (update_wind, update_wind_streamers, update_seasons, update_clouds),
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

// ===================== Cloud state machine =====================
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

/// Every cloud form that can take the main stage. The cirrostratus herald is
/// deliberately NOT here — it's a permanent layer of its own
/// ([`CloudState::cirrostratus`]), never the main event.
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
    /// the eyeballing knob for the renderer (a 20 s herald, then the cloud).
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
/// Main-layer cover fade at each phase edge, so clouds roll in and drain out
/// instead of popping.
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
    /// GARDN_FOG=1 — every morning's roll comes up fog (renderer dev knob).
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

    /// The herald + main-layer covers at `now`. Cirrostratus ramps over its
    /// onset/recede phases and holds full through the event — so any time the
    /// main layer shows, the veil above it is already up (owner's invariant:
    /// cirrostratus never appears alone, and nothing appears without it).
    fn covers(&self, now: f32) -> (f32, Option<CloudType>, f32) {
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

/// The sim as a resource. Private — everyone else reads [`CloudState`].
#[derive(Resource)]
struct CloudSimRes(CloudSim);

/// What the sky is doing, published for the renderer (and, later, gameplay:
/// cumulonimbus lightning, nimbostratus rain). Covers are 0..1.
#[derive(Resource, Default)]
pub(crate) struct CloudState {
    /// The high thin herald veil — up whenever anything else is.
    pub(crate) cirrostratus: f32,
    /// The one main cloud type on stage right now, if any.
    pub(crate) main_type: Option<CloudType>,
    pub(crate) main_cover: f32,
    /// Morning fog thickness.
    pub(crate) fog: f32,
}

/// Step the cloud machine with the live world: the sky's clock for day/frac,
/// the wind for the 70% rule, and the biome under the worm for the regional
/// modifiers (arid outback starves clouds, the coasts feed them).
fn update_clouds(
    time: Res<Time>,
    clock: Res<crate::sky::SkyClock>,
    seasons: Res<SeasonClock>,
    wind: Res<Wind>,
    mut sim: ResMut<CloudSimRes>,
    mut state: ResMut<CloudState>,
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
    let (cirrostratus, main_type, main_cover) = sim.0.covers(ctx.now);
    state.cirrostratus = cirrostratus;
    state.main_type = main_type;
    state.main_cover = main_cover;
    state.fog = sim.0.fog_level(ctx.day, ctx.day_frac);
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
                    // Keep `now` before the first formation check so the phase
                    // machine stays out of the rng stream — this test is only
                    // about the fog roll.
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
    /// the phase order is the spec's procession, the main layer never shows
    /// without the cirrostratus veil, and only legal types appear per phase.
    #[test]
    fn full_cycle_keeps_herald_invariant_and_order() {
        let mut sim = CloudSim::new(0xA57_0C1, None, false);
        let mut seen_phases = Vec::new();
        let mut cycles = 0;
        let mut t = 0.0f32;
        // Windy winter coast — fronts form often, so a few sim-hours covers
        // many full systems.
        while cycles < 12 && t < 8.0 * 3600.0 {
            t += 20.0;
            sim.step(&ctx(t, Season::Winter, 4.0, false, true));

            let (cirro, main_type, main_cover) = sim.covers(t);
            // Owner: nothing appears without the herald above it.
            assert!(
                main_cover <= cirro + 1e-4,
                "main layer ({main_cover}) outgrew the veil ({cirro}) in {:?}",
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

            let disc = std::mem::discriminant(&sim.phase);
            if seen_phases.last() != Some(&disc) {
                seen_phases.push(disc);
                if matches!(sim.phase, CloudPhase::Clear) {
                    cycles += 1;
                }
            }
        }
        assert!(cycles >= 6, "only {cycles} full weather systems in 8 sim-hours");

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
        // Re-drive a fresh copy recording full phases (discriminants can't be
        // pattern-matched) to validate each adjacent transition.
        let mut sim = CloudSim::new(0xA57_0C1, None, false);
        let mut prev = sim.phase;
        let mut t = 0.0f32;
        while t < 8.0 * 3600.0 {
            t += 20.0;
            sim.step(&ctx(t, Season::Winter, 4.0, false, true));
            if std::mem::discriminant(&sim.phase) != std::mem::discriminant(&prev)
                || sim.phase != prev
            {
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
        // The event-riding time dilutes the raw 0.6 check-ratio, so just pin
        // a sane band around it.
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
                // Count each outro once, on entry.
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
}
