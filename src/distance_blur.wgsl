// Background-only distance blur.
//
// Unlike a physical depth-of-field (which blurs BOTH sides of a focal plane and,
// for a near-ground camera, always blurs the foreground grass), this blurs only
// with distance: everything nearer than `start` is perfectly sharp, then the
// blur ramps up to `max_blur` by `end`. The grass at the worm's nose stays crisp
// while the distant titans go soft.
//
// Separable Gaussian: one horizontal pass then one vertical pass. Each pass
// recomputes the blur radius per pixel from the (unchanged) depth texture.

#import bevy_core_pipeline::fullscreen_vertex_shader::FullscreenVertexOutput

struct Params {
    // Camera near-plane distance, for turning reverse-Z depth into feet.
    near: f32,
    // World distance (feet) at which softening begins. Closer than this = sharp.
    start: f32,
    // World distance (feet) at which the blur reaches full strength.
    end: f32,
    // Maximum blur diameter, in pixels.
    max_blur: f32,
}

@group(0) @binding(0) var<uniform> params: Params;
@group(0) @binding(1) var depth_texture: texture_depth_2d;
@group(0) @binding(2) var color_texture: texture_2d<f32>;
@group(0) @binding(3) var color_sampler: sampler;

// Camera view distance (feet) at a texel. Bevy uses an infinite reverse-Z
// projection, so linear distance is simply `near / raw_depth`. raw == 0 is the
// cleared far value (open sky / nothing drawn) — treat it as very far.
fn dist_at(coord: vec2<i32>) -> f32 {
    let raw = textureLoad(depth_texture, coord, 0);
    if raw > 0.0 {
        return params.near / raw;
    }
    return 1.0e9;
}

// Blur diameter (pixels) for a given camera distance.
fn blur_diameter(dist: f32) -> f32 {
    let denom = max(params.end - params.start, 0.0001);
    let t = clamp((dist - params.start) / denom, 0.0, 1.0);
    let s = t * t * (3.0 - 2.0 * t); // smoothstep
    return params.max_blur * s;
}

// One direction of a separable Gaussian, sized by the circle of confusion `coc`.
//
// Depth-aware gather: a blurred (background) pixel must NOT pull in samples that
// belong to a nearer object — otherwise the sharp foreground smears into the
// background halo and the near object stops cleanly occluding the blur. So each
// tap is weighted down as it gets closer than the center pixel: taps at the same
// depth or farther count fully, taps well in front of the center are rejected.
// The result is that closer, sharper objects cut crisply through the soft
// background instead of the blur creeping over them.
fn gaussian_blur(frag_coord: vec4<f32>, coc: f32, center_dist: f32, frag_offset: vec2<f32>) -> vec4<f32> {
    let dims = vec2<f32>(textureDimensions(color_texture));
    let uv = frag_coord.xy / dims;

    let sigma = coc * 0.25;
    // Anything under half a pixel of blur is just the sharp center texel.
    if sigma < 0.5 {
        return vec4(textureSampleLevel(color_texture, color_sampler, uv, 0.0).rgb, 1.0);
    }

    let support = i32(ceil(sigma * 1.5));
    let px = frag_offset; // one texel step along the blur axis
    let texel = px / dims;
    let exp_factor = -1.0 / (2.0 * sigma * sigma);
    // Taps nearer than this fraction of the center distance are fully rejected.
    let near_edge = center_dist * 0.7;

    var sum = textureSampleLevel(color_texture, color_sampler, uv, 0.0).rgb;
    var weight_sum = 1.0;
    for (var i = 1; i <= support; i += 1) {
        let g = exp(exp_factor * f32(i) * f32(i));
        let fi = f32(i);
        for (var s = -1; s <= 1; s += 2) {
            let step = fi * f32(s);
            let suv = uv + texel * step;
            let scoord = vec2<i32>(clamp(frag_coord.xy + px * step, vec2(0.0), dims - 1.0));
            // 1.0 when the tap is at/behind the center, ramping to 0.0 as it
            // comes forward of it — a closer object shouldn't bleed backward.
            let occl = smoothstep(near_edge, center_dist, dist_at(scoord));
            let weight = g * occl;
            sum += textureSampleLevel(color_texture, color_sampler, suv, 0.0).rgb * weight;
            weight_sum += weight;
        }
    }

    return vec4(sum / weight_sum, 1.0);
}

@fragment
fn horizontal(in: FullscreenVertexOutput) -> @location(0) vec4<f32> {
    let dist = dist_at(vec2<i32>(floor(in.position.xy)));
    let coc = blur_diameter(dist);
    return gaussian_blur(in.position, coc, dist, vec2(1.0, 0.0));
}

@fragment
fn vertical(in: FullscreenVertexOutput) -> @location(0) vec4<f32> {
    let dist = dist_at(vec2<i32>(floor(in.position.xy)));
    let coc = blur_diameter(dist);
    return gaussian_blur(in.position, coc, dist, vec2(0.0, 1.0));
}
