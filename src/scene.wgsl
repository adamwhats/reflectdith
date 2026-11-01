// Lighting constants
const light_colour: vec3<f32> = vec3<f32>(1.0, 1.0, 1.0);
// Grey, as the colour comes from the palette in the post pass
const albedo_colour: vec3<f32> = vec3<f32>(0.8, 0.8, 0.8);
const ambient_amount: f32 = 0.1;

// Camera uniform, mirrors CameraUniform in camera.rs
// model takes model space to world space
// view_proj takes world space to clip space
struct Camera {
    camera: vec4<f32>,
    // Points from the surface towards the light, in world space
    to_light: vec4<f32>,
    // x is the specular shininess, y the specular strength
    specular: vec4<f32>,
    // x is the floor brightness, y and z the distances from the centre where it
    // starts and finishes fading to black. w is world-noise cells per unit
    floor: vec4<f32>,
    model: mat4x4<f32>,
    view_proj: mat4x4<f32>,
};

@group(0) @binding(0)
var<uniform> camera: Camera;

@group(1) @binding(0)
var world_noise: texture_2d<f32>;

struct Shadow {
    light_view_proj: mat4x4<f32>,
    settings: vec4<f32>,
};
@group(2) @binding(0) var<uniform> shadow: Shadow;
@group(2) @binding(1) var shadow_map: texture_depth_2d;
@group(2) @binding(2) var shadow_sampler: sampler_comparison;

fn surface_visibility(world: vec3<f32>, normal: vec3<f32>) -> f32 {
    // settings.x is the shadow strength; 0 turns shadows off
    if shadow.settings.x <= 0.0 { return 1.0; }
    // Offset the receiver slightly to avoid a surface shadowing itself.
    let clip = shadow.light_view_proj * vec4<f32>(world + normal * 0.002, 1.0);
    let ndc = clip.xyz / clip.w;
    if any(abs(ndc.xy) > vec2<f32>(1.0)) || ndc.z < 0.0 || ndc.z > 1.0 { return 1.0; }
    let uv = vec2<f32>(ndc.x * 0.5 + 0.5, 0.5 - ndc.y * 0.5);
    let texel = 1.0 / vec2<f32>(textureDimensions(shadow_map));
    var visibility = 0.0;
    for (var y = -1; y <= 1; y += 1) {
        for (var x = -1; x <= 1; x += 1) {
            visibility += textureSampleCompareLevel(shadow_map, shadow_sampler, uv + vec2<f32>(f32(x), f32(y)) * texel, ndc.z - 0.0002);
        }
    }
    return mix(1.0, visibility / 9.0, shadow.settings.x);
}

// Output structure for the vertex shader
// x, y, z are the position of the vertex in clip space
// w is the homogeneous coordinate used for perspective division
// normal is flat-interpolated so every vertex of a triangle carries the
// same value, giving the fragment shader a stable per-face identifier
// world_position is needed for specular highlights in the fragment shader
struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) @interpolate(flat) normal: vec3<f32>,
    @location(1) world_position: vec3<f32>,
};

// Vertex shader entry point
@vertex
fn vertex_main(
    @builtin(vertex_index) in_vertex_index: u32,
    @location(0) position: vec3<f32>,
    @location(1) normal: vec3<f32>,
) -> VertexOutput {
    var out: VertexOutput;
    let world_position = camera.model * vec4<f32>(position, 1.0);
    out.clip_position = camera.view_proj * world_position;
    // Transform normals from model-space into world-space using the upper-left 3x3 of the model transform
    let ul = mat3x3<f32>(camera.model[0].xyz, camera.model[1].xyz, camera.model[2].xyz);
    out.normal = ul * normal;
    out.world_position = world_position.xyz;
    return out;
}

// Fragment shader entry point
@fragment
fn fragment_main(
    vertex_output: VertexOutput,
) -> @location(0) vec4<f32> {
    // Vectors
    let to_light = normalize(camera.to_light.xyz);
    let n = normalize(vertex_output.normal);
    let view = normalize(camera.camera.xyz - vertex_output.world_position);
    let half = normalize(to_light + view);
    // Diffuse lighting
    let diffuse_term = max(dot(n, to_light), 0.0);
    let visibility = surface_visibility(vertex_output.world_position, n);
    let diffuse = albedo_colour * (ambient_amount + light_colour * diffuse_term * visibility);
    // Specular lighting
    let specular_term = pow(max(dot(n, half), 0.0), camera.specular.x);
    let specular = light_colour * camera.specular.y * specular_term * visibility;
    // Sum for final colour
    let colour = diffuse + specular;
    return vec4<f32>(colour, 1.0);
}

// Fragment shader entry point for the floor, which is lit like the mesh, then
// fades to black between two distances from the centre, leaving a pool of light
// under the mesh
// Negative alpha carries a world-locked blue-noise threshold; mesh alpha is 1.
@fragment
fn fragment_floor(
    vertex_output: VertexOutput,
) -> @location(0) vec4<f32> {
    let to_light = normalize(camera.to_light.xyz);
    let n = normalize(vertex_output.normal);
    let diffuse_term = max(dot(n, to_light), 0.0);
    // Shade at the same world-space cell centre used by the blue-noise
    // threshold, so neither shadow boundaries nor the fade cut a floor cell.
    let density = max(camera.floor.w, 0.001);
    let cell = vec2<i32>(floor(vertex_output.world_position.xz * density));
    let centre_xz = (vec2<f32>(cell) + vec2<f32>(0.5)) / density;
    let sample_world = vec3<f32>(centre_xz.x, camera.model[3].y, centre_xz.y);
    let centre_distance = length(centre_xz - camera.model[3].xz);
    let fade = 1.0 - smoothstep(camera.floor.y, camera.floor.z, centre_distance);
    if fade <= 0.0 { return vec4<f32>(0.0, 0.0, 0.0, 1.0); }
    let colour = vec3<f32>(camera.floor.x) * (ambient_amount + light_colour * diffuse_term * surface_visibility(sample_world, n)) * fade;
    // Wrap signed cells explicitly so the pattern is continuous across the origin.
    let size = vec2<i32>(textureDimensions(world_noise));
    let coords = ((cell % size) + size) % size;
    let threshold = textureLoad(world_noise, coords, 0).r;
    return vec4<f32>(colour, -threshold);
}
