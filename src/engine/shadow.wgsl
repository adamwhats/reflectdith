// Same camera/model layout as camera.rs and scene.wgsl.
struct Camera {
    camera: vec4<f32>,
    to_light: vec4<f32>,
    specular: vec4<f32>,
    floor: vec4<f32>,
    model: mat4x4<f32>,
    view_proj: mat4x4<f32>,
};
struct Shadow {
    light_view_proj: mat4x4<f32>,
    settings: vec4<f32>,
};
@group(0) @binding(0) var<uniform> camera: Camera;
@group(1) @binding(0) var<uniform> shadow: Shadow;

@vertex
fn vertex_shadow(@location(0) position: vec3<f32>) -> @builtin(position) vec4<f32> {
    return shadow.light_view_proj * camera.model * vec4<f32>(position, 1.0);
}
