//! Game audio: the shared sound-effect handles (munch, the looping wind bed the
//! streamers play) and the music rotation that pulls every file in
//! `assets/music/`. `GameAudioPlugin` loads it all at startup and spaces out the
//! songs. (Distinct from Bevy's own `AudioPlugin` in DefaultPlugins.)

use bevy::prelude::*;

use crate::world::GardenRng;

pub struct GameAudioPlugin;

impl Plugin for GameAudioPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, setup_audio)
            .add_systems(Update, run_soundtrack);
    }
}

#[derive(Resource)]
pub(crate) struct GameSounds {
    pub(crate) munch: Handle<AudioSource>,
    /// Looping wind bed, played spatially from the wind streamers so the gusts
    /// you see flowing past the worm are the same ones you hear.
    pub(crate) wind: Handle<AudioSource>,
}

/// In-game radio: every track in `assets/music/` is in the rotation (drop a
/// file in to add it). A song starts every [`SOUNDTRACK_INTERVAL_SECS`]; picks
/// are random but never the same song twice in a row.
#[derive(Resource)]
struct Soundtrack {
    tracks: Vec<Handle<AudioSource>>,
    last_played: Option<usize>,
    /// Time until the next song may start (a still-playing song delays it).
    timer: Timer,
    rng: GardenRng,
}

const SOUNDTRACK_INTERVAL_SECS: f32 = 5.0 * 60.0;

/// Marks the currently playing soundtrack song (despawned when it ends).
#[derive(Component)]
struct SoundtrackSong;

/// Load the sound-effect handles and the music rotation, and kick off the first
/// song. A Startup system — nothing in the world setup depends on these.
fn setup_audio(mut commands: Commands, asset_server: Res<AssetServer>) {
    commands.insert_resource(GameSounds {
        munch: asset_server.load("sounds/munch.wav"),
        wind: asset_server.load("sounds/wind.wav"),
    });

    // Soundtrack rotation: every audio file in assets/music/ is a track.
    // The first song starts right away; run_soundtrack spaces out the rest.
    let mut tracks = Vec::new();
    if let Ok(entries) = std::fs::read_dir("assets/music") {
        let mut names: Vec<String> = entries
            .flatten()
            .filter_map(|e| e.file_name().into_string().ok())
            .filter(|n| {
                let lower = n.to_lowercase();
                // Only formats compiled into the build (Cargo features).
                lower.ends_with(".mp3") || lower.ends_with(".wav")
            })
            .collect();
        names.sort();
        for name in names {
            tracks.push(asset_server.load(format!("music/{name}")));
        }
    }
    let mut soundtrack = Soundtrack {
        tracks,
        last_played: None,
        timer: Timer::from_seconds(SOUNDTRACK_INTERVAL_SECS, TimerMode::Once),
        rng: GardenRng::new(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos() as u64)
                .unwrap_or(0x50D4),
        ),
    };
    if !soundtrack.tracks.is_empty() {
        let first = (soundtrack.rng.next_f32() * soundtrack.tracks.len() as f32) as usize
            % soundtrack.tracks.len();
        commands.spawn((
            SoundtrackSong,
            AudioPlayer::new(soundtrack.tracks[first].clone()),
            PlaybackSettings::DESPAWN,
        ));
        soundtrack.last_played = Some(first);
    }
    commands.insert_resource(soundtrack);
}

/// Start the next soundtrack song once the interval has elapsed — random pick,
/// never the same song twice in a row. A song that outlasts the interval is
/// never cut off; the next one starts when it ends.
fn run_soundtrack(
    time: Res<Time>,
    mut commands: Commands,
    mut soundtrack: ResMut<Soundtrack>,
    playing: Query<(), With<SoundtrackSong>>,
) {
    soundtrack.timer.tick(time.delta());
    if !soundtrack.timer.finished() || !playing.is_empty() || soundtrack.tracks.is_empty() {
        return;
    }

    let n = soundtrack.tracks.len();
    let mut pick = (soundtrack.rng.next_f32() * n as f32) as usize % n;
    if n > 1 && Some(pick) == soundtrack.last_played {
        pick = (pick + 1) % n;
    }

    commands.spawn((
        SoundtrackSong,
        AudioPlayer::new(soundtrack.tracks[pick].clone()),
        PlaybackSettings::DESPAWN,
    ));
    soundtrack.last_played = Some(pick);
    soundtrack.timer.reset();
}
