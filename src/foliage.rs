//! Tree foliage in the ECS: the per-tree components (species tag, wind-sway
//! character, canopy LOD group + rungs), the foliage-LOD ladder, and the systems
//! that sway the trees, swap canopy detail by distance, and reveal a chunk's
//! trees the frame they're all built. The tree *geometry* is generated in
//! `trees.rs`; the streamer spawns the entities (see `finish_tree_build_tasks`)
//! using the `pub(crate)` types here. `FoliagePlugin` wires the frame systems.

use bevy::prelude::*;

use crate::trees::TreeSpecies;
use crate::weather::Wind;
use crate::streaming::{ChunkTreesRevealed, TreesPending, WorldChunk};

pub struct FoliagePlugin;

impl Plugin for FoliagePlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Update, (sway_trees, update_foliage_lod, reveal_built_chunks));
    }
}

/// Any procedurally generated native tree; species recorded for future
/// fauna/food interactions.
#[derive(Component)]
pub(crate) struct WildTree {
    #[allow(dead_code)]
    pub(crate) species: TreeSpecies,
}

/// Per-tree sway character, seeded from the tree itself: giants heave slowly,
/// saplings flick about.
#[derive(Component)]
pub(crate) struct WindSway {
    pub(crate) phase: f32,
    /// Peak lean, radians.
    pub(crate) amplitude: f32,
    /// Oscillation rate, rad/s.
    pub(crate) frequency: f32,
}

/// Rock every tree around its base. Rotation pivots at the trunk root (tree
/// meshes grow up from y=0), so the roots stay planted while the crown rides
/// the gusts — the wood carries the leaves, so the whole canopy moves with it.
fn sway_trees(
    time: Res<Time>,
    wind: Res<Wind>,
    mut trees: Query<(&WindSway, &mut Transform), With<WildTree>>,
    mut canopies: Query<
        (&Parent, &mut Transform),
        (With<FoliageLod>, Without<WildTree>),
    >,
    sway_of: Query<(&WindSway, &FoliageLodGroup)>,
) {
    let t = time.elapsed_secs();
    // Gusts: two slow sines beating against each other, 0..1.
    let gust = 0.5 + 0.5 * ((t * 0.11).sin() * 0.6 + (t * 0.043).sin() * 0.4);
    let lean_axis = Vec3::new(-wind.dir.y, 0.0, wind.dir.x);
    // Horizontal downwind direction — the way the leaves stream.
    let downwind = Vec3::new(wind.dir.x, 0.0, wind.dir.y);
    // 0/5 = trees stand dead still; 5/5 = everything heaving.
    let force = wind.strength / 5.0;

    for (sway, mut tf) in &mut trees {
        let wave = (t * sway.frequency + sway.phase).sin() * 0.7
            + (t * sway.frequency * 2.3 + sway.phase * 1.7).sin() * 0.3;
        let angle = force
            * (sway.amplitude * (0.35 + 0.65 * gust) * wave
                + sway.amplitude * 0.5 * gust); // steady downwind lean under the oscillation
        tf.rotation = Quat::from_axis_angle(lean_axis, angle);
    }

    // Leaf flutter: the canopy stirs a touch faster than the trunk under it,
    // and keeps a whisper of life even in light air. On top of the flutter the
    // whole crown SLIDES downwind — a horizontal drift proportional to canopy
    // size, gust-driven with a steady downwind bias — so the leaves visibly
    // stream in the wind instead of only leaning, selling the gusts.
    let flutter_force = (0.08 + 0.92 * force).min(1.0);
    for (parent, mut tf) in &mut canopies {
        let Ok((sway, group)) = sway_of.get(parent.get()) else {
            continue;
        };
        let osc = (t * sway.frequency * 2.9 + sway.phase * 2.3).sin();
        let flutter = flutter_force * sway.amplitude * 0.35 * (0.3 + 0.7 * gust) * osc;
        tf.rotation = Quat::from_axis_angle(lean_axis, flutter);

        // Slide up to ~5% of the canopy radius: a steady downwind offset (grows
        // with the wind) with the flutter oscillation riding on it.
        let slide = flutter_force * group.radius * 0.05 * ((0.4 + 0.6 * gust) + 0.6 * osc);
        tf.translation = downwind * slide;
    }
}

/// Foliage LOD ladder: block-size multipliers per level (2″ → 8″ → 32″ leaf
/// blocks) and the switch-over distances in feet. Distance is measured from
/// the camera to the canopy's bounding *sphere*, so the crown of a giant goes
/// coarse even while you stand at its trunk — 2-inch blocks 400 ft overhead
/// are wasted triangles either way.
// Four rungs, not three, so each LOD step is a gentle ~2.3–3× block-size
// change instead of a jarring 4× snap: 2″ → 6″ → 14″ → 32″ leaf blocks. The
// whole pipeline is generic over this array's length (build, spawn, and
// update_foliage_lod all size themselves from it), so adding a rung needs no
// other change — just keep DISTANCES one shorter than FACTORS.
pub(crate) const FOLIAGE_LOD_FACTORS: [i32; 4] = [1, 3, 7, 16];
// Push the resolution steps well out so the finest leaves are already present
// long before you reach a tree — no refining "pop" in your face. The 2″ blocks
// hold out to 60 ft; the coarser (and so more visible) swaps are placed
// progressively deeper, where the distance blur (full by 260 ft) has already
// smeared them past noticing. The one lightly-blurred swap at 60 ft is also the
// smallest block-size jump (2″→6″), so it's doubly hard to catch.
const FOLIAGE_LOD_DISTANCES_FT: [f32; 3] = [60.0, 120.0, 220.0];
/// Minimum fraction of fine voxels a coarse cell needs to survive downsampling.
pub(crate) const FOLIAGE_LOD_FILL: f32 = 0.2;

/// On the tree root: canopy bounding sphere (tree-local) for LOD selection.
#[derive(Component)]
pub(crate) struct FoliageLodGroup {
    pub(crate) center: Vec3,
    pub(crate) radius: f32,
    /// Currently shown rung — remembered so the boundary has hysteresis:
    /// stepping finer needs ~10% of slack, so grazing a cutoff can't strobe
    /// the crown between two meshes.
    pub(crate) level: usize,
}

/// On each foliage mesh child: which rung of the LOD ladder it is.
#[derive(Component)]
pub(crate) struct FoliageLod {
    pub(crate) level: usize,
}

/// Swap each tree's foliage mesh by distance: full 2-inch leaf blocks up
/// close, bigger averaged blocks farther out. Distance is to the canopy's
/// bounding sphere, so height counts — a giant's crown coarsens even from
/// directly below, and sharpens again if you ever get up there.
fn update_foliage_lod(
    cam_q: Query<&Transform, With<Camera>>,
    mut trees: Query<(&GlobalTransform, &mut FoliageLodGroup, &Children)>,
    mut lods: Query<(&FoliageLod, &mut Visibility)>,
) {
    let Ok(cam) = cam_q.get_single() else {
        return;
    };
    let cam_pos = cam.translation;

    for (tree_tf, mut group, children) in &mut trees {
        let center = tree_tf.translation() + group.center;
        let dist = (cam_pos - center).length() - group.radius;
        // Step coarser the moment a cutoff is crossed, but only step finer
        // again once ~10% inside it — hovering on the line can't flicker.
        let mut level = group.level;
        while level < FOLIAGE_LOD_DISTANCES_FT.len() && dist >= FOLIAGE_LOD_DISTANCES_FT[level] {
            level += 1;
        }
        while level > 0 && dist < FOLIAGE_LOD_DISTANCES_FT[level - 1] * 0.9 {
            level -= 1;
        }
        if group.level != level {
            group.level = level;
        }

        for child in children {
            let Ok((lod, mut vis)) = lods.get_mut(*child) else {
                continue;
            };
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

/// The atomic reveal: once a chunk's last real tree has built (`TreesPending`
/// hit 0), flip all its hidden tree roots visible in one frame and mark it
/// revealed. The silhouette watches for that mark and despawns the same frame,
/// so the coarse blocks snap straight into detailed trees — no fade, no gap.
fn reveal_built_chunks(
    mut commands: Commands,
    chunks: Query<
        (Entity, &TreesPending, &Children),
        (With<WorldChunk>, Without<ChunkTreesRevealed>),
    >,
    mut tree_vis: Query<&mut Visibility, With<FoliageLodGroup>>,
) {
    for (entity, pending, children) in &chunks {
        if pending.0 != 0 {
            continue;
        }
        for &child in children.iter() {
            if let Ok(mut vis) = tree_vis.get_mut(child) {
                *vis = Visibility::Inherited;
            }
        }
        commands.entity(entity).insert(ChunkTreesRevealed);
    }
}
