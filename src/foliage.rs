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
        // CHAINED, and the order is load-bearing: `update_foliage_lod` must run
        // before `reveal_built_chunks`. Every foliage rung spawns Hidden while
        // bark spawns visible-by-default, so if a reveal beat rung selection to
        // a freshly built tree, the root flipped visible as a BARE TRUNK until
        // the next frame — a real flash at the streaming edge, where frames
        // hitch. Chaining guarantees a root is only revealed after its correct
        // rung is live, whichever side of main's streaming chain we run on.
        app.add_systems(
            Update,
            (sway_trees, attach_lod_fade_state, update_foliage_lod, reveal_built_chunks)
                .chain(),
        );
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

/// Foliage LOD ladder: block-size multipliers per level (6″ → 18″ → 42″ → 8 ft
/// leaf blocks on the 6-inch tree voxel grid) and the switch-over distances in
/// feet. Distance is measured from the camera to the canopy's bounding
/// *sphere*, so the crown of a giant goes coarse even while you stand at its
/// trunk — 6-inch blocks 400 ft overhead are wasted triangles either way.
// Four rungs, not three, so each LOD step is a gentle ~2.3–3× block-size
// change instead of a jarring 4× snap. The whole pipeline is generic over this
// array's length (build, spawn, and update_foliage_lod all size themselves
// from it), so adding a rung needs no other change — just keep DISTANCES one
// shorter than FACTORS.
pub(crate) const FOLIAGE_LOD_FACTORS: [i32; 4] = [1, 3, 7, 16];
// Push the resolution steps well out so the finest leaves are already present
// long before you reach a tree — no refining "pop" in your face. The finest
// blocks hold out to 60 ft; the coarser (and so more visible) swaps are placed
// progressively deeper, where the distance blur (full by 260 ft) has already
// smeared them past noticing. The one lightly-blurred swap at 60 ft is also
// the smallest block-size jump, so it's doubly hard to catch.
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

/// Seconds for one LOD cross-fade (two phases of half this each). Short: it
/// only has to break the single-frame snap into a blend, and both rungs render
/// (and cast shadows) while it runs.
const FOLIAGE_LOD_FADE_SECS: f32 = 0.3;

/// Per-tree LOD cross-fade state. Attached lazily by `attach_lod_fade_state`
/// — NOT by the streamer's spawn code — so `FoliageLodGroup`'s construction,
/// a cross-module contract, stays untouched. A tree missing this component for
/// its first frame or two simply swaps rungs instantly (it's hidden then
/// anyway, pre-reveal).
#[derive(Component, Default)]
struct FoliageLodFade(Option<LodFade>);

/// One running cross-fade, in two phases so canopy pixels NEVER stop writing
/// depth (blended materials don't write it, and the distance-blur pass reads
/// it — see the foliage material comments in streaming.rs):
///   phase A (t < 0.5): incoming rung alpha-BLENDS in over the outgoing rung,
///     which stays fully opaque/masked underneath and carries the depth;
///   phase B (t ≥ 0.5): incoming is restored to its depth-writing mode and
///     carries the canopy; the outgoing rung blends out on top.
/// Only silhouette-fringe pixels (where one rung sticks out past the other)
/// briefly lack depth — exactly the far, blur-softened pixels where it can't
/// be seen. At the end the outgoing rung hides and both materials are back to
/// their real mode, ready to fade either way next time.
struct LodFade {
    /// Rung being faded out; hidden when the fade completes.
    from: usize,
    /// The rungs' real alpha mode (Opaque, or Mask for skinned foliage),
    /// reinstated on each rung as its blend phase ends.
    restore: AlphaMode,
    /// 0..1 progress across both phases.
    t: f32,
}

fn attach_lod_fade_state(
    mut commands: Commands,
    trees: Query<Entity, (With<FoliageLodGroup>, Without<FoliageLodFade>)>,
) {
    for tree in &trees {
        commands.entity(tree).insert(FoliageLodFade::default());
    }
}

/// Smoothstep for the alpha ramps.
fn ease(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

type RungQuery<'w, 's> = Query<
    'w,
    's,
    (
        &'static FoliageLod,
        &'static mut Visibility,
        &'static mut MeshMaterial3d<StandardMaterial>,
    ),
    Without<FoliageLodGroup>,
>;

/// Give `level`'s rung its own material copy if it still shares the tree's
/// single spawned handle with a sibling rung — alpha animation must never leak
/// onto the rung it's crossing WITH. One clone per rung, made the first time
/// that rung takes part in a fade, then reused for life.
fn ensure_private_rung_material(
    children: &Children,
    rungs: &mut RungQuery,
    materials: &mut Assets<StandardMaterial>,
    level: usize,
) -> Option<AssetId<StandardMaterial>> {
    let mut target = None;
    let mut sibling_ids = Vec::new();
    for &child in children {
        let Ok((lod, _, mat)) = rungs.get(child) else {
            continue;
        };
        if lod.level == level {
            target = Some(child);
        } else {
            sibling_ids.push(mat.0.id());
        }
    }
    let Ok((_, _, mut mat)) = rungs.get_mut(target?) else {
        return None;
    };
    if sibling_ids.contains(&mat.0.id()) {
        let clone = materials.get(&mat.0)?.clone();
        mat.0 = materials.add(clone);
    }
    Some(mat.0.id())
}

/// Apply `edit` to `level`'s rung material.
fn edit_rung_material(
    children: &Children,
    rungs: &mut RungQuery,
    materials: &mut Assets<StandardMaterial>,
    level: usize,
    edit: impl FnOnce(&mut StandardMaterial),
) {
    for &child in children {
        let Ok((lod, _, mat)) = rungs.get(child) else {
            continue;
        };
        if lod.level == level {
            if let Some(m) = materials.get_mut(&mat.0) {
                edit(m);
            }
            return;
        }
    }
}

/// Prime a cross-fade from `outgoing` to `incoming`: both rungs get private
/// materials, and the incoming one starts blended at alpha 0 (so it can't
/// flash fully opaque on its first visible frame). Returns the alpha mode to
/// restore — read from the incoming rung, whose mode is at rest here (any
/// interrupted fade was restored before this is called). Ordered so a failure
/// part-way leaves no rung in a blended state.
fn begin_lod_fade(
    children: &Children,
    rungs: &mut RungQuery,
    materials: &mut Assets<StandardMaterial>,
    incoming: usize,
    outgoing: usize,
) -> Option<AlphaMode> {
    ensure_private_rung_material(children, rungs, materials, outgoing)?;
    let in_id = ensure_private_rung_material(children, rungs, materials, incoming)?;
    let m = materials.get_mut(in_id)?;
    let restore = m.alpha_mode;
    m.alpha_mode = AlphaMode::Blend;
    m.base_color = m.base_color.with_alpha(0.0);
    Some(restore)
}

/// Swap each tree's foliage mesh by distance: full 6-inch leaf blocks up
/// close, bigger averaged blocks farther out. Distance is to the canopy's
/// bounding sphere, so height counts — a giant's crown coarsens even from
/// directly below, and sharpens again if you ever get up there. Once a tree
/// is revealed, rung swaps CROSS-FADE (see [`LodFade`]) instead of snapping;
/// pre-reveal (root still hidden) they stay instant, so the correct rung is
/// live the frame `reveal_built_chunks` shows the tree.
fn update_foliage_lod(
    time: Res<Time>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    cam_q: Query<&Transform, With<Camera>>,
    mut trees: Query<(
        &GlobalTransform,
        &Visibility,
        &mut FoliageLodGroup,
        Option<&mut FoliageLodFade>,
        &Children,
    )>,
    mut rungs: RungQuery,
) {
    let Ok(cam) = cam_q.get_single() else {
        return;
    };
    let cam_pos = cam.translation;
    let dt = time.delta_secs();

    for (tree_tf, root_vis, mut group, fade_state, children) in &mut trees {
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

        let fade = fade_state.map(Mut::into_inner);
        let mut keep = None;
        if let Some(fade) = fade {
            if let Some(run) = fade.0.as_mut() {
                run.t += dt / FOLIAGE_LOD_FADE_SECS;
            }
            // A finished fade — or one made obsolete by the level moving again
            // mid-blend — restores BOTH its rungs to full alpha in their real
            // mode. (Both, not just one: a hitchy frame can jump t straight
            // past phase B's per-frame writes.)
            let done = fade.0.as_ref().is_some_and(|run| run.t >= 1.0);
            if done || level != group.level {
                if let Some(run) = fade.0.take() {
                    for rung_level in [group.level, run.from] {
                        edit_rung_material(children, &mut rungs, &mut materials, rung_level, |m| {
                            m.alpha_mode = run.restore;
                            m.base_color = m.base_color.with_alpha(1.0);
                        });
                    }
                }
            }

            if level != group.level {
                let prev = group.level;
                group.level = level;
                // Fade only watchable swaps. While the root is hidden
                // (pre-reveal) the switch stays instant — and must: the reveal
                // depends on the new rung being fully live, not mid-blend.
                if !matches!(root_vis, Visibility::Hidden) {
                    if let Some(restore) =
                        begin_lod_fade(children, &mut rungs, &mut materials, level, prev)
                    {
                        fade.0 = Some(LodFade { from: prev, restore, t: 0.0 });
                    }
                }
            } else if let Some(run) = fade.0.as_ref() {
                // Drive both rungs by phase, idempotently, every frame.
                let (restore, from, t) = (run.restore, run.from, run.t);
                if t < 0.5 {
                    let a = ease(t * 2.0);
                    edit_rung_material(children, &mut rungs, &mut materials, level, |m| {
                        m.alpha_mode = AlphaMode::Blend;
                        m.base_color = m.base_color.with_alpha(a);
                    });
                    edit_rung_material(children, &mut rungs, &mut materials, from, |m| {
                        m.alpha_mode = restore;
                        m.base_color = m.base_color.with_alpha(1.0);
                    });
                } else {
                    let a = 1.0 - ease(t * 2.0 - 1.0);
                    edit_rung_material(children, &mut rungs, &mut materials, level, |m| {
                        m.alpha_mode = restore;
                        m.base_color = m.base_color.with_alpha(1.0);
                    });
                    edit_rung_material(children, &mut rungs, &mut materials, from, |m| {
                        m.alpha_mode = AlphaMode::Blend;
                        m.base_color = m.base_color.with_alpha(a);
                    });
                }
            }
            keep = fade.0.as_ref().map(|run| run.from);
        } else if group.level != level {
            // Fade state not attached yet (tree spawned this frame): instant.
            group.level = level;
        }

        for &child in children {
            let Ok((lod, mut vis, _)) = rungs.get_mut(child) else {
                continue;
            };
            let want = if lod.level == level || Some(lod.level) == keep {
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
