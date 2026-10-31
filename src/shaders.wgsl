// Lighting constants
// to_light points from the surface towards the light, in world space
const to_light: vec3<f32> = normalize(vec3<f32>(1.0, 1.0, 1.0));
const light_colour: vec3<f32> = vec3<f32>(1.0, 1.0, 1.0);
const albedo_colour: vec3<f32> = vec3<f32>(0.7, 0.05, 0.6);
const ambient_amount: f32 = 0.1;

// Camera uniform, mirrors CameraUniform in camera.rs
// model takes model space to world space
// view_proj takes world space to clip space
struct Camera {
    model: mat4x4<f32>,
    view_proj: mat4x4<f32>,
};

@group(0) @binding(0)
var<uniform> camera: Camera;

// Output structure for the vertex shader
// x, y, z are the position of the vertex in clip space
// w is the homogeneous coordinate used for perspective division
// normal is flat-interpolated so every vertex of a triangle carries the
// same value, giving the fragment shader a stable per-face identifier
struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) @interpolate(flat) normal: vec3<f32>,
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
    return out;
}

// Fragment shader entry point
@fragment
fn fragment_main(
    vertex_output: VertexOutput,
) -> @location(0) vec4<f32> {
    let n = normalize(vertex_output.normal);
    let diffuse = max(dot(n, to_light), 0.0);
    let colour = albedo_colour * (ambient_amount + light_colour * diffuse);
    return vec4<f32>(colour, 1.0);
}