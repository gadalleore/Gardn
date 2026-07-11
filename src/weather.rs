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
            .init_resource::<ActiveFormation>()
            .init_resource::<StormLight>()
            .add_systems(Startup, (setup_streamers, setup_clouds))
            // Chained so the visual systems read THIS frame's CloudState and
            // the sky (which runs unordered) is at most one frame behind.
            .add_systems(
                Update,
                (
                    update_wind,
                    update_wind_streamers,
                    update_seasons,
                    update_clouds,
                    sync_cloud_formations,
                    animate_clouds,
                    update_rain,
                    update_lightning,
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

// ===================== Procedural cloud rendering =====================
//
// Owner ruled: procedural, no sprites. Each cloud is a clump of low-poly
// blob puffs (one shared icosphere mesh, a handful of shared gray materials)
// hung at type-specific altitude — worm-scale, they're distant titans'
// ceilings. The whole field is camera-anchored like the star dome (sky decor,
// not streamed world) and drifts with the live wind; per-type layouts give
// each form its silhouette: streaks for cirrus, a mackerel scatter for
// cirrocumulus, fluffy flat-bottomed heaps for cumulus, an anvil tower for
// cumulonimbus, broad gray decks for the stratus family. Cover from the state
// machine swells clouds in and out; the cirrostratus veil is a single huge
// translucent disc above everything. Rain is streamer-style falling streaks;
// lightning is a light flash published as [`StormLight`] for the sky to apply
// (single-writer: sky.rs owns every light/fog write).

/// How far the cloud field spreads around the worm, and how high the herald
/// veil hangs (just under the celestial discs at 850 ft).
const CLOUD_FIELD_RADIUS: f32 = 750.0;
const VEIL_ALTITUDE: f32 = 700.0;
/// Veil disc radius, sized so its farthest fragment (√(500² + 700²) ≈ 860 ft)
/// stays inside the camera's 1000 ft far plane — a bigger disc gets the far
/// clip carving a hard arc across the sky (seen in the first eyeball run).
/// The edge that remains sits past the fog start, so the haze soft-fades it.
const VEIL_RADIUS: f32 = 500.0;
const VEIL_MAX_ALPHA: f32 = 0.32;
/// Cloud drift in ft/s per wind-strength unit — a gale visibly marches the
/// ceiling along, a breeze barely.
const CLOUD_DRIFT_FTPS: f32 = 4.0;

/// How much a cloud type grays and dims the daylight at full cover — the sky
/// reads this (with [`CloudState`]) to sell overcast without touching clouds.
pub(crate) fn sky_grayness(kind: CloudType) -> f32 {
    match kind {
        CloudType::Cumulonimbus => 0.85,
        CloudType::Nimbostratus => 0.75,
        CloudType::Stratus => 0.55,
        CloudType::Altostratus => 0.45,
        CloudType::Stratocumulus => 0.35,
        CloudType::Cumulus => 0.12,
        CloudType::Cirrus | CloudType::Altocumulus | CloudType::Cirrocumulus => 0.08,
    }
}

/// Lightning flash level, 0..1 decaying — written here when a cumulonimbus
/// bolt fires, applied by sky.rs (the one writer of ambient/sky colour).
#[derive(Resource)]
pub(crate) struct StormLight {
    pub(crate) flash: f32,
    next_bolt_at: f32,
    rng: GardenRng,
}

impl Default for StormLight {
    fn default() -> Self {
        Self { flash: 0.0, next_bolt_at: 0.0, rng: GardenRng::new(0x0B_017) }
    }
}

/// Root of the current main-event formation; its children are cloud masses.
#[derive(Component)]
struct CloudFormation;

/// One cloud: a puff clump. `offset` is its slot in the camera-anchored field
/// (y = altitude), `base_scale` its full-cover size, `bob` a phase for the
/// slow breathing pulse.
#[derive(Component)]
struct CloudMass {
    offset: Vec3,
    base_scale: Vec3,
    bob: f32,
}

/// The single cirrostratus disc above everything.
#[derive(Component)]
struct HeraldVeil;

/// One falling rain streak (world-space, near the worm).
#[derive(Component)]
struct RainStreak {
    fall_speed: f32,
}

/// Which main type the spawned formation was built for — rebuilt when the
/// state machine moves on.
#[derive(Resource, Default)]
struct ActiveFormation(Option<CloudType>);

/// Shared cloud geometry + the fixed shade ladder. One icosphere, six
/// materials — a formation of 150 puffs costs one mesh and a texture-less
/// material each.
#[derive(Resource)]
struct CloudAssets {
    puff: Handle<Mesh>,
    veil_mat: Handle<StandardMaterial>,
    shades: [Handle<StandardMaterial>; 6],
    rain_mesh: Handle<Mesh>,
    rain_mat: Handle<StandardMaterial>,
}

/// The shade ladder's gray levels, brightest first (index into `shades`).
const CLOUD_GRAYS: [f32; 6] = [1.0, 0.94, 0.80, 0.62, 0.45, 0.34];

impl CloudAssets {
    /// Nearest shade material for a wanted gray level.
    fn shade(&self, gray: f32) -> Handle<StandardMaterial> {
        let mut best = 0;
        for (i, g) in CLOUD_GRAYS.iter().enumerate() {
            if (g - gray).abs() < (CLOUD_GRAYS[best] - gray).abs() {
                best = i;
            }
        }
        self.shades[best].clone()
    }
}

fn setup_clouds(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    let shades = CLOUD_GRAYS.map(|g| {
        materials.add(StandardMaterial {
            // Faintly blue-shadowed white, fully rough so the sun models the
            // clumps; fog stays ON so distant clouds melt into the haze.
            base_color: Color::srgb(g, g, (g * 1.03).min(1.0)),
            perceptual_roughness: 1.0,
            reflectance: 0.06,
            ..default()
        })
    });
    let veil_mat = materials.add(StandardMaterial {
        // The herald: a milky sheet whose alpha IS the machine's cover value.
        // Unlit and retinted per frame by `animate_clouds` — a LIT sheet seen
        // from below shows its shadowed underside and reads as a dark UFO
        // (first eyeball run). Fog stays on so the disc edge melts into haze.
        base_color: Color::srgba(1.0, 1.0, 1.0, 0.0),
        unlit: true,
        alpha_mode: AlphaMode::Blend,
        double_sided: true,
        cull_mode: None,
        ..default()
    });
    commands.spawn((
        HeraldVeil,
        Mesh3d(meshes.add(Cylinder::new(VEIL_RADIUS, 2.0))),
        MeshMaterial3d(veil_mat.clone()),
        NotShadowCaster,
        Transform::from_xyz(0.0, VEIL_ALTITUDE, 0.0),
        Visibility::Hidden,
    ));

    commands.insert_resource(CloudAssets {
        puff: meshes.add(Sphere::new(1.0).mesh().ico(2).unwrap_or_else(|_| Sphere::new(1.0).into())),
        veil_mat,
        shades,
        rain_mesh: meshes.add(Cuboid::new(0.04, 1.8, 0.04)),
        rain_mat: materials.add(StandardMaterial {
            base_color: Color::srgba(0.66, 0.73, 0.86, 0.6),
            alpha_mode: AlphaMode::Blend,
            unlit: true,
            ..default()
        }),
    });
}

/// Per-type formation recipe: where it lives and how its clumps are built.
struct CloudLook {
    altitude: f32,
    /// How many cloud masses across the field.
    count: (i32, i32),
    /// Puffs per mass.
    puffs: (i32, i32),
    /// Puff radius range, feet.
    puff_ft: (f32, f32),
    /// Local scatter of puffs inside a mass (half-extents, feet).
    spread: Vec3,
    /// Whole-mass anisotropic stretch — streaks vs decks vs heaps.
    stretch: Vec3,
    gray: f32,
    /// Cumulus family: puffs pile upward off a flat base.
    flat_base: bool,
    /// Streak types align their long axis downwind.
    wind_aligned: bool,
}

fn cloud_look(kind: CloudType) -> CloudLook {
    use CloudType::*;
    match kind {
        // High wisps combed into long thin downwind streaks.
        Cirrus => CloudLook {
            altitude: 580.0,
            count: (5, 9),
            puffs: (5, 9),
            puff_ft: (7.0, 12.0),
            spread: Vec3::new(60.0, 4.0, 10.0),
            stretch: Vec3::new(2.6, 0.3, 0.7),
            gray: 1.0,
            flat_base: false,
            wind_aligned: true,
        },
        // A mackerel sky: many small high rippled patches.
        Cirrocumulus => CloudLook {
            altitude: 560.0,
            count: (14, 22),
            puffs: (3, 6),
            puff_ft: (4.5, 7.5),
            spread: Vec3::new(26.0, 3.0, 20.0),
            stretch: Vec3::ONE,
            gray: 1.0,
            flat_base: false,
            wind_aligned: false,
        },
        // Mid-level puff patches in loose rows.
        Altocumulus => CloudLook {
            altitude: 400.0,
            count: (10, 16),
            puffs: (4, 7),
            puff_ft: (9.0, 14.0),
            spread: Vec3::new(30.0, 8.0, 24.0),
            stretch: Vec3::new(1.0, 0.6, 1.0),
            gray: 0.94,
            flat_base: false,
            wind_aligned: false,
        },
        // Low lumpy patchwork — bigger, grayer, closer.
        Stratocumulus => CloudLook {
            altitude: 260.0,
            count: (7, 11),
            puffs: (7, 11),
            puff_ft: (16.0, 24.0),
            spread: Vec3::new(55.0, 10.0, 45.0),
            stretch: Vec3::new(1.0, 0.5, 1.0),
            gray: 0.80,
            flat_base: false,
            wind_aligned: false,
        },
        // The classic fluffy heap: flat bottom, cauliflower top.
        Cumulus => CloudLook {
            altitude: 230.0,
            count: (4, 7),
            puffs: (6, 10),
            puff_ft: (14.0, 22.0),
            spread: Vec3::new(34.0, 22.0, 30.0),
            stretch: Vec3::new(1.0, 0.9, 1.0),
            gray: 1.0,
            flat_base: true,
            wind_aligned: false,
        },
        // Handled by `spawn_cumulonimbus` (tower + anvil), but keep sane
        // numbers here for the shared fields it does use.
        Cumulonimbus => CloudLook {
            altitude: 200.0,
            count: (1, 2),
            puffs: (0, 0),
            puff_ft: (0.0, 0.0),
            spread: Vec3::ZERO,
            stretch: Vec3::ONE,
            gray: 0.34,
            flat_base: true,
            wind_aligned: false,
        },
        // The rain deck: broad, dark, low, featureless.
        Nimbostratus => CloudLook {
            altitude: 190.0,
            count: (9, 13),
            puffs: (8, 12),
            puff_ft: (22.0, 32.0),
            spread: Vec3::new(70.0, 8.0, 60.0),
            stretch: Vec3::new(1.2, 0.4, 1.2),
            gray: 0.45,
            flat_base: false,
            wind_aligned: false,
        },
        // The two outro sheets: gray mid-level, paler low.
        Altostratus => CloudLook {
            altitude: 380.0,
            count: (8, 12),
            puffs: (7, 10),
            puff_ft: (20.0, 28.0),
            spread: Vec3::new(65.0, 6.0, 55.0),
            stretch: Vec3::new(1.3, 0.35, 1.3),
            gray: 0.62,
            flat_base: false,
            wind_aligned: false,
        },
        Stratus => CloudLook {
            altitude: 140.0,
            count: (7, 10),
            puffs: (8, 12),
            puff_ft: (24.0, 34.0),
            spread: Vec3::new(80.0, 6.0, 66.0),
            stretch: Vec3::new(1.4, 0.3, 1.4),
            gray: 0.72,
            flat_base: false,
            wind_aligned: false,
        },
    }
}

/// Rebuild the formation whenever the state machine changes the main event.
/// The swap happens at zero cover (the machine ramps every phase edge), so
/// nothing pops on screen.
fn sync_cloud_formations(
    mut commands: Commands,
    state: Res<CloudState>,
    assets: Res<CloudAssets>,
    wind: Res<Wind>,
    mut active: ResMut<ActiveFormation>,
    formations: Query<Entity, With<CloudFormation>>,
) {
    if active.0 == state.main_type {
        return;
    }
    active.0 = state.main_type;
    for e in &formations {
        commands.entity(e).despawn_recursive();
    }
    let Some(kind) = state.main_type else {
        return;
    };

    // Fresh seed per formation — cloudscapes never repeat.
    let mut rng = GardenRng::new(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0xC10D_5EED),
    );
    let look = cloud_look(kind);
    let count = rng.range_i(look.count.0, look.count.1);

    let root = commands
        .spawn((CloudFormation, Transform::default(), Visibility::default()))
        .id();
    for _ in 0..count {
        // Uniform slot in the field disc (sqrt for area-uniform).
        let ang = rng.range(0.0, std::f32::consts::TAU);
        let r = CLOUD_FIELD_RADIUS * rng.next_f32().sqrt();
        let offset = Vec3::new(
            ang.cos() * r,
            look.altitude * rng.range(0.92, 1.12),
            ang.sin() * r,
        );
        let yaw = if look.wind_aligned {
            // Streaks comb along the wind (long axis = local X).
            Quat::from_rotation_y((-wind.dir.y).atan2(wind.dir.x))
        } else {
            Quat::from_rotation_y(rng.range(0.0, std::f32::consts::TAU))
        };
        let base_scale = look.stretch * rng.range(0.8, 1.25);
        let mass = commands
            .spawn((
                CloudMass {
                    offset,
                    base_scale,
                    bob: rng.range(0.0, std::f32::consts::TAU),
                },
                Transform {
                    translation: offset,
                    rotation: yaw,
                    // Swelled to real size by `animate_clouds`.
                    scale: Vec3::splat(0.001),
                },
                Visibility::Hidden,
            ))
            .id();
        commands.entity(root).add_child(mass);
        if kind == CloudType::Cumulonimbus {
            spawn_cumulonimbus(&mut commands, mass, &assets, &mut rng);
        } else {
            spawn_puff_clump(&mut commands, mass, &assets, &look, &mut rng);
        }
    }
    println!("☁️ Cloudscape rebuilt: {count} {kind:?} masses.");
}

/// Fill a mass with its puff clump per the recipe.
fn spawn_puff_clump(
    commands: &mut Commands,
    mass: Entity,
    assets: &CloudAssets,
    look: &CloudLook,
    rng: &mut GardenRng,
) {
    let n = rng.range_i(look.puffs.0, look.puffs.1);
    for _ in 0..n {
        let mut pos = Vec3::new(
            rng.range(-look.spread.x, look.spread.x),
            rng.range(-look.spread.y, look.spread.y),
            rng.range(-look.spread.z, look.spread.z),
        );
        if look.flat_base {
            // Cumulus family: pile upward off a common flat bottom.
            pos.y = pos.y.abs();
        }
        let r = rng.range(look.puff_ft.0, look.puff_ft.1);
        // Real cloud bases sit in shadow: puffs low in the clump run a shade
        // darker than the sunlit crown.
        let frac_up = (pos.y + look.spread.y) / (2.0 * look.spread.y).max(1.0);
        let gray = look.gray * (0.82 + 0.18 * frac_up.clamp(0.0, 1.0));
        let puff = commands
            .spawn((
                Mesh3d(assets.puff.clone()),
                MeshMaterial3d(assets.shade(gray)),
                NotShadowCaster,
                Transform {
                    translation: pos,
                    rotation: Quat::from_rotation_y(rng.range(0.0, std::f32::consts::TAU)),
                    // Slightly squashed blobs read as vapor, not marbles.
                    scale: Vec3::new(r, r * rng.range(0.55, 0.8), r * rng.range(0.8, 1.1)),
                },
            ))
            .id();
        commands.entity(mass).add_child(puff);
    }
}

/// The one bespoke build: a cumulonimbus tower — dark shelf base, boiling
/// column, and the white anvil smeared flat at the top.
fn spawn_cumulonimbus(
    commands: &mut Commands,
    mass: Entity,
    assets: &CloudAssets,
    rng: &mut GardenRng,
) {
    let mut spawn = |pos: Vec3, scale: Vec3, gray: f32, rng: &mut GardenRng| {
        let p = commands
            .spawn((
                Mesh3d(assets.puff.clone()),
                MeshMaterial3d(assets.shade(gray)),
                NotShadowCaster,
                Transform {
                    translation: pos,
                    rotation: Quat::from_rotation_y(rng.range(0.0, std::f32::consts::TAU)),
                    scale,
                },
            ))
            .id();
        commands.entity(mass).add_child(p);
    };
    // The dark shelf the rain falls from.
    for _ in 0..rng.range_i(5, 7) {
        let r = rng.range(26.0, 36.0);
        let pos = Vec3::new(rng.range(-45.0, 45.0), rng.range(-6.0, 6.0), rng.range(-45.0, 45.0));
        spawn(pos, Vec3::new(r, r * 0.45, r * rng.range(0.8, 1.1)), 0.34, rng);
    }
    // The boiling column, brightening as it climbs into sunlight.
    for _ in 0..rng.range_i(9, 12) {
        let h = rng.range(25.0, 300.0);
        let taper = 1.0 - h / 420.0;
        let r = rng.range(18.0, 28.0) * (0.6 + 0.6 * taper);
        let pos = Vec3::new(
            rng.range(-30.0, 30.0) * taper,
            h,
            rng.range(-30.0, 30.0) * taper,
        );
        let gray = 0.45 + 0.5 * (h / 300.0);
        spawn(pos, Vec3::new(r, r * rng.range(0.7, 0.95), r), gray.min(0.95), rng);
    }
    // The anvil: bright, wide, smeared flat where the tower hits the ceiling.
    for _ in 0..rng.range_i(5, 8) {
        let r = rng.range(20.0, 26.0);
        let pos = Vec3::new(rng.range(-70.0, 70.0), rng.range(295.0, 330.0), rng.range(-70.0, 70.0));
        spawn(pos, Vec3::new(r * 3.2, r * 0.35, r * 3.2), 1.0, rng);
    }
}

/// Per-frame cloud life: the field rides the camera (sky decor, like the star
/// dome), masses drift downwind and wrap, swell with the machine's cover, and
/// breathe a slow pulse; the herald veil's alpha follows its own cover.
fn animate_clouds(
    time: Res<Time>,
    wind: Res<Wind>,
    state: Res<CloudState>,
    clock: Res<crate::sky::SkyClock>,
    assets: Res<CloudAssets>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    cam_q: Query<&Transform, (With<Camera>, Without<CloudFormation>, Without<CloudMass>, Without<HeraldVeil>)>,
    mut formation_q: Query<&mut Transform, (With<CloudFormation>, Without<CloudMass>, Without<HeraldVeil>)>,
    mut masses: Query<(&mut CloudMass, &mut Transform, &mut Visibility), Without<CloudFormation>>,
    mut veil_q: Query<
        (&mut Transform, &mut Visibility),
        (With<HeraldVeil>, Without<CloudFormation>, Without<CloudMass>, Without<Camera>),
    >,
) {
    let Ok(cam) = cam_q.get_single() else {
        return;
    };
    let dt = time.delta_secs();
    let t = time.elapsed_secs();
    let drift = Vec3::new(wind.dir.x, 0.0, wind.dir.y) * wind.strength * CLOUD_DRIFT_FTPS * dt;

    for mut tf in &mut formation_q {
        tf.translation = Vec3::new(cam.translation.x, 0.0, cam.translation.z);
    }

    let cover = state.main_cover;
    for (mut mass, mut tf, mut vis) in &mut masses {
        mass.offset += drift;
        let flat = Vec2::new(mass.offset.x, mass.offset.z);
        if flat.length() > CLOUD_FIELD_RADIUS {
            // Blew off the field's edge — re-enter from the upwind side.
            mass.offset.x = -mass.offset.x * 0.96;
            mass.offset.z = -mass.offset.z * 0.96;
        }
        tf.translation = mass.offset;
        // Grow in from 35% size rather than zero — a swelling cloud, not an
        // inflating balloon — with a slow breathing pulse on top.
        let pulse = 1.0 + ((t * 0.05) + mass.bob).sin() * 0.03;
        let swell = (0.35 + 0.65 * cover) * pulse;
        tf.scale = (mass.base_scale * swell).max(Vec3::splat(0.001));
        let want = if cover > 0.02 {
            Visibility::Inherited
        } else {
            Visibility::Hidden
        };
        if *vis != want {
            *vis = want;
        }
    }

    for (mut tf, mut vis) in &mut veil_q {
        tf.translation = Vec3::new(cam.translation.x, VEIL_ALTITUDE, cam.translation.z);
        let want = if state.cirrostratus > 0.01 {
            Visibility::Visible
        } else {
            Visibility::Hidden
        };
        if *vis != want {
            *vis = want;
        }
    }
    if let Some(mat) = materials.get_mut(&assets.veil_mat) {
        // Unlit, so day/night is painted in by hand: same sun-elevation curve
        // the sky uses (0.25 of the day = sunrise), dimming the milk to a
        // faint moonlit gray at night.
        let elev = ((clock.frac - 0.25) * std::f32::consts::TAU).sin();
        let day_l = (elev * 3.0).clamp(0.0, 1.0);
        let v = 0.22 + 0.78 * day_l;
        mat.base_color =
            Color::srgba(v, v, (v * 1.02).min(1.0), VEIL_MAX_ALPHA * state.cirrostratus);
    }
}

/// Rain under the rain-bearers: streamer-style streaks falling around the
/// worm, heavier under cumulonimbus than nimbostratus, leaning with the wind.
/// Minimal by design — real water is a future epic; this is the weather being
/// legible from the ground.
fn update_rain(
    time: Res<Time>,
    state: Res<CloudState>,
    mut wind: ResMut<Wind>,
    assets: Res<CloudAssets>,
    chunk_world: Res<ChunkWorld>,
    mut commands: Commands,
    cam_q: Query<&Transform, (With<Camera>, Without<RainStreak>)>,
    mut streaks: Query<(Entity, &RainStreak, &mut Transform), Without<Camera>>,
) {
    let Ok(cam) = cam_q.get_single() else {
        return;
    };
    let cam_pos = cam.translation;
    let dt = time.delta_secs();

    let intensity = match state.main_type {
        Some(CloudType::Nimbostratus) => state.main_cover,
        // The deadly rain: harder, denser.
        Some(CloudType::Cumulonimbus) => state.main_cover * 1.6,
        _ => 0.0,
    };
    let lean = Vec3::new(wind.dir.x, 0.0, wind.dir.y) * wind.strength * 1.5;

    let mut alive = 0usize;
    for (entity, streak, mut tf) in &mut streaks {
        tf.translation.y -= streak.fall_speed * dt;
        tf.translation += lean * dt;
        let floor = ground_world_y(&chunk_world, tf.translation.x, tf.translation.z);
        if tf.translation.y < floor {
            commands.entity(entity).despawn();
        } else {
            alive += 1;
        }
    }

    let desired = (intensity.clamp(0.0, 1.6) * 130.0) as usize;
    let mut to_spawn = desired.saturating_sub(alive).min(8);
    while to_spawn > 0 {
        to_spawn -= 1;
        let pos = cam_pos
            + Vec3::new(
                wind.rng.range(-35.0, 35.0),
                wind.rng.range(18.0, 42.0),
                wind.rng.range(-35.0, 35.0),
            );
        commands.spawn((
            RainStreak { fall_speed: wind.rng.range(26.0, 34.0) },
            Mesh3d(assets.rain_mesh.clone()),
            MeshMaterial3d(assets.rain_mat.clone()),
            NotShadowCaster,
            Transform::from_translation(pos),
        ));
    }
}

/// Cumulonimbus bolts: random flashes published as [`StormLight`]; sky.rs
/// turns the flash into the actual light/colour kick (single writer). No
/// thunder yet — there's no thunder sample in assets/ (flagged to sprites).
fn update_lightning(time: Res<Time>, state: Res<CloudState>, mut storm: ResMut<StormLight>) {
    let t = time.elapsed_secs();
    // Fast exponential decay — a flash, not a floodlight.
    storm.flash *= (-time.delta_secs() * 7.0).exp();
    if storm.flash < 0.001 {
        storm.flash = 0.0;
    }
    let stormy =
        state.main_type == Some(CloudType::Cumulonimbus) && state.main_cover > 0.5;
    if stormy && t >= storm.next_bolt_at {
        storm.flash = 1.0;
        let gap = storm.rng.range(6.0, 18.0);
        storm.next_bolt_at = t + gap;
        println!("⚡ Lightning!");
    } else if !stormy {
        // Keep the timer ahead so the first bolt of a storm isn't instant.
        storm.next_bolt_at = t + 8.0;
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
