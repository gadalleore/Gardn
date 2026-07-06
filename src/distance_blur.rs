//! Background-only distance blur post-process.
//!
//! Bevy's built-in depth-of-field is a *physical* lens model: it blurs both
//! sides of a focal plane, and for a camera sitting on the ground (a worm) that
//! means the near grass is blurred at least as hard as the far trees — the near
//! circle of confusion grows without bound. No amount of tuning fixes that; it's
//! optics.
//!
//! This effect instead blurs purely by distance: everything nearer than `start`
//! is razor sharp, then a separable Gaussian ramps up to `max_blur` pixels by
//! `end`. The foreground never blurs. Structure mirrors Bevy's own `dof` module
//! (extract → configure depth usage → specialize pipelines → view node), with
//! the physical circle-of-confusion swapped for a `smoothstep(start, end, dist)`
//! ramp in `distance_blur.wgsl`.

use bevy::asset::{load_internal_asset, Handle};
use bevy::core_pipeline::core_3d::graph::{Core3d, Node3d};
use bevy::core_pipeline::fullscreen_vertex_shader::fullscreen_shader_vertex_state;
use bevy::ecs::query::QueryItem;
use bevy::ecs::system::lifetimeless::Read;
use bevy::prelude::*;
use bevy::render::extract_component::{ComponentUniforms, DynamicUniformIndex, UniformComponentPlugin};
use bevy::render::render_graph::{
    NodeRunError, RenderGraphApp, RenderGraphContext, RenderLabel, ViewNode, ViewNodeRunner,
};
use bevy::render::render_resource::{
    binding_types::{sampler, texture_2d, texture_depth_2d, uniform_buffer},
    BindGroupEntries, BindGroupLayout, BindGroupLayoutEntries, CachedRenderPipelineId,
    ColorTargetState, ColorWrites, FilterMode, FragmentState, LoadOp, Operations, PipelineCache,
    RenderPassColorAttachment, RenderPassDescriptor, RenderPipelineDescriptor, Sampler,
    SamplerBindingType, SamplerDescriptor, Shader, ShaderStages, ShaderType,
    SpecializedRenderPipeline, SpecializedRenderPipelines, StoreOp, TextureFormat,
    TextureSampleType, TextureUsages,
};
use bevy::render::renderer::{RenderContext, RenderDevice};
use bevy::render::sync_component::SyncComponentPlugin;
use bevy::render::sync_world::RenderEntity;
use bevy::render::view::{prepare_view_targets, ExtractedView, ViewDepthTexture, ViewTarget};
use bevy::render::camera::Projection;
use bevy::render::{Extract, ExtractSchedule, Render, RenderApp, RenderSet};

const SHADER_HANDLE: Handle<Shader> = Handle::weak_from_u128(0x7f3a9c2b5e814d06a1c4f2b8e7d09355);

/// Attach to a `Camera3d` to blur the world by distance, foreground kept sharp.
#[derive(Component, Clone, Copy)]
pub struct DistanceBlur {
    /// Closer than this (feet) stays perfectly sharp.
    pub start: f32,
    /// Blur reaches full strength at this distance (feet).
    pub end: f32,
    /// Maximum blur diameter, in pixels.
    pub max_blur: f32,
}

/// GPU-side parameters (see `Params` in `distance_blur.wgsl`).
#[derive(Component, Clone, Copy, ShaderType)]
struct DistanceBlurUniform {
    near: f32,
    start: f32,
    end: f32,
    max_blur: f32,
}

#[derive(Component)]
struct DistanceBlurPipelines {
    horizontal: CachedRenderPipelineId,
    vertical: CachedRenderPipelineId,
}

pub struct DistanceBlurPlugin;

impl Plugin for DistanceBlurPlugin {
    fn build(&self, app: &mut App) {
        load_internal_asset!(app, SHADER_HANDLE, "distance_blur.wgsl", Shader::from_wgsl);

        app.add_plugins((
            SyncComponentPlugin::<DistanceBlur>::default(),
            UniformComponentPlugin::<DistanceBlurUniform>::default(),
        ));

        let Some(render_app) = app.get_sub_app_mut(RenderApp) else {
            return;
        };

        render_app
            .init_resource::<SpecializedRenderPipelines<DistanceBlurPipeline>>()
            .add_systems(ExtractSchedule, extract_distance_blur)
            .add_systems(
                Render,
                configure_view_targets
                    .after(prepare_view_targets)
                    .in_set(RenderSet::ManageViews),
            )
            .add_systems(Render, prepare_pipelines.in_set(RenderSet::Prepare))
            .add_render_graph_node::<ViewNodeRunner<DistanceBlurNode>>(Core3d, DistanceBlurLabel)
            .add_render_graph_edges(
                Core3d,
                (Node3d::Bloom, DistanceBlurLabel, Node3d::Tonemapping),
            );
    }

    fn finish(&self, app: &mut App) {
        let Some(render_app) = app.get_sub_app_mut(RenderApp) else {
            return;
        };
        render_app.init_resource::<DistanceBlurGlobal>();
    }
}

/// Copies `DistanceBlur` into the render world and derives the GPU uniform. The
/// near-plane distance comes from the camera's perspective projection so depth
/// can be linearised in the shader.
fn extract_distance_blur(
    mut commands: Commands,
    query: Extract<Query<(RenderEntity, &DistanceBlur, &Projection)>>,
) {
    for (entity, blur, projection) in &query {
        let Projection::Perspective(perspective) = projection else {
            continue;
        };
        if let Some(mut entity_commands) = commands.get_entity(entity) {
            entity_commands.insert((
                *blur,
                DistanceBlurUniform {
                    near: perspective.near,
                    start: blur.start,
                    end: blur.end,
                    max_blur: blur.max_blur,
                },
            ));
        }
    }
}

/// The depth buffer must be bindable as a texture for the shader to read it.
fn configure_view_targets(mut view_targets: Query<&mut Camera3d, With<DistanceBlur>>) {
    for mut camera_3d in &mut view_targets {
        let mut usages = TextureUsages::from(camera_3d.depth_texture_usages);
        usages |= TextureUsages::TEXTURE_BINDING;
        camera_3d.depth_texture_usages = usages.into();
    }
}

fn prepare_pipelines(
    mut commands: Commands,
    pipeline_cache: Res<PipelineCache>,
    mut pipelines: ResMut<SpecializedRenderPipelines<DistanceBlurPipeline>>,
    global: Res<DistanceBlurGlobal>,
    views: Query<(Entity, &ExtractedView), With<DistanceBlur>>,
) {
    for (entity, view) in &views {
        let pipeline = DistanceBlurPipeline {
            layout: global.layout.clone(),
        };
        let horizontal = pipelines.specialize(
            &pipeline_cache,
            &pipeline,
            DistanceBlurKey { hdr: view.hdr, vertical: false },
        );
        let vertical = pipelines.specialize(
            &pipeline_cache,
            &pipeline,
            DistanceBlurKey { hdr: view.hdr, vertical: true },
        );
        commands
            .entity(entity)
            .insert(DistanceBlurPipelines { horizontal, vertical });
    }
}

#[derive(RenderLabel, Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct DistanceBlurLabel;

#[derive(Default)]
struct DistanceBlurNode;

impl ViewNode for DistanceBlurNode {
    type ViewQuery = (
        Read<ViewTarget>,
        Read<ViewDepthTexture>,
        Read<DistanceBlurPipelines>,
        Read<DynamicUniformIndex<DistanceBlurUniform>>,
    );

    fn run<'w>(
        &self,
        _: &mut RenderGraphContext,
        render_context: &mut RenderContext<'w>,
        (view_target, view_depth, pipelines, uniform_index): QueryItem<'w, Self::ViewQuery>,
        world: &'w World,
    ) -> Result<(), NodeRunError> {
        let pipeline_cache = world.resource::<PipelineCache>();
        let global = world.resource::<DistanceBlurGlobal>();
        let uniforms = world.resource::<ComponentUniforms<DistanceBlurUniform>>();

        for pipeline_id in [pipelines.horizontal, pipelines.vertical] {
            let (Some(pipeline), Some(uniform_binding)) = (
                pipeline_cache.get_render_pipeline(pipeline_id),
                uniforms.binding(),
            ) else {
                return Ok(());
            };

            // Ping-pong: read the current source, write the other texture.
            let postprocess = view_target.post_process_write();
            let bind_group = render_context.render_device().create_bind_group(
                Some("distance blur bind group"),
                &global.layout,
                &BindGroupEntries::sequential((
                    uniform_binding,
                    view_depth.view(),
                    postprocess.source,
                    &global.sampler,
                )),
            );

            let attachments = [Some(RenderPassColorAttachment {
                view: postprocess.destination,
                resolve_target: None,
                ops: Operations {
                    load: LoadOp::Clear(default()),
                    store: StoreOp::Store,
                },
            })];
            let mut render_pass =
                render_context
                    .command_encoder()
                    .begin_render_pass(&RenderPassDescriptor {
                        label: Some("distance blur pass"),
                        color_attachments: &attachments,
                        ..default()
                    });
            render_pass.set_pipeline(pipeline);
            render_pass.set_bind_group(0, &bind_group, &[uniform_index.index()]);
            render_pass.draw(0..3, 0..1);
        }

        Ok(())
    }
}

/// The bind group layout and sampler, shared across passes and views.
#[derive(Resource)]
struct DistanceBlurGlobal {
    layout: BindGroupLayout,
    sampler: Sampler,
}

impl FromWorld for DistanceBlurGlobal {
    fn from_world(world: &mut World) -> Self {
        let render_device = world.resource::<RenderDevice>();
        let layout = render_device.create_bind_group_layout(
            Some("distance blur bind group layout"),
            &BindGroupLayoutEntries::sequential(
                ShaderStages::FRAGMENT,
                (
                    uniform_buffer::<DistanceBlurUniform>(true),
                    texture_depth_2d(),
                    texture_2d(TextureSampleType::Float { filterable: true }),
                    sampler(SamplerBindingType::Filtering),
                ),
            ),
        );
        let sampler = render_device.create_sampler(&SamplerDescriptor {
            label: Some("distance blur sampler"),
            mag_filter: FilterMode::Linear,
            min_filter: FilterMode::Linear,
            mipmap_filter: FilterMode::Linear,
            ..default()
        });
        DistanceBlurGlobal { layout, sampler }
    }
}

struct DistanceBlurPipeline {
    layout: BindGroupLayout,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct DistanceBlurKey {
    hdr: bool,
    vertical: bool,
}

impl SpecializedRenderPipeline for DistanceBlurPipeline {
    type Key = DistanceBlurKey;

    fn specialize(&self, key: Self::Key) -> RenderPipelineDescriptor {
        let format = if key.hdr {
            ViewTarget::TEXTURE_FORMAT_HDR
        } else {
            TextureFormat::bevy_default()
        };
        RenderPipelineDescriptor {
            label: Some("distance blur pipeline".into()),
            layout: vec![self.layout.clone()],
            push_constant_ranges: vec![],
            vertex: fullscreen_shader_vertex_state(),
            primitive: default(),
            depth_stencil: None,
            multisample: default(),
            fragment: Some(FragmentState {
                shader: SHADER_HANDLE,
                shader_defs: vec![],
                entry_point: if key.vertical {
                    "vertical".into()
                } else {
                    "horizontal".into()
                },
                targets: vec![Some(ColorTargetState {
                    format,
                    blend: None,
                    write_mask: ColorWrites::ALL,
                })],
            }),
            zero_initialize_workgroup_memory: false,
        }
    }
}
