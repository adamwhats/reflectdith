struct Mapping {
    crop: vec4<f32>,
    destination: vec4<f32>,
    dimensions: vec4<f32>,
    dark: vec4<f32>,
};
@group(0) @binding(0) var canvas: texture_2d<f32>;
@group(0) @binding(1) var<uniform> mapping: Mapping;

@vertex
fn vertex_main(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {
    var positions = array<vec2<f32>, 3>(vec2(-1., -1.), vec2(3., -1.), vec2(-1., 3.));
    return vec4(positions[i], 0., 1.);
}
@fragment
fn fragment_main(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    let local = (position.xy / mapping.dimensions.xy - mapping.destination.xy) / mapping.destination.zw;
    if any(local < vec2(0.)) || any(local >= vec2(1.)) { return vec4(mapping.dark.rgb, 1.); }
    let uv = mapping.crop.xy + local * mapping.crop.zw;
    let size = textureDimensions(canvas);
    let coords = clamp(vec2<i32>(floor(uv * vec2<f32>(size))), vec2(0), vec2<i32>(size) - vec2(1));
    return textureLoad(canvas, coords, 0);
}
