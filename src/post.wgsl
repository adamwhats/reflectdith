// Corners of a single triangle large enough to cover the whole screen. The
// screen is the square (-1,-1) to (1,1) in clip space, and the long edge of
// this triangle passes through (1,1), so the square is fully inside it. The
// GPU clips away the part that falls outside the screen
const clip_vertices = array<vec2<f32>, 3>(
    vec2<f32>(-1.0, -1.0),
    vec2<f32>(3.0, -1.0),
    vec2<f32>(-1.0, 3.0),
);

// 8x8 Bayer matrix, stored row by row. Each value is a rank from 0 to 63, so
// dividing by 64 gives a threshold which is spread evenly over the pattern
const bayer = array<u32, 64>(
    0u, 32u, 8u, 40u, 2u, 34u, 10u, 42u,
    48u, 16u, 56u, 24u, 50u, 18u, 58u, 26u,
    12u, 44u, 4u, 36u, 14u, 46u, 6u, 38u,
    60u, 28u, 52u, 20u, 62u, 30u, 54u, 22u,
    3u, 35u, 11u, 43u, 1u, 33u, 9u, 41u,
    51u, 19u, 59u, 27u, 49u, 17u, 57u, 25u,
    15u, 47u, 7u, 39u, 13u, 45u, 5u, 37u,
    63u, 31u, 55u, 23u, 61u, 29u, 53u, 21u,
);

// Tones for the dithering
const dark_colour: vec3<f32> = vec3<f32>(0.002, 0.0, 0.01);
const light_colour: vec3<f32> = vec3<f32>(1.0, 0.15, 0.6);

// The offscreen image the scene was rendered into
@group(0) @binding(0)
var scene: texture_2d<f32>;

// Vertex shader entry point
// There are no vertex buffers, the position comes from the vertex index alone
@vertex
fn vertex_main(
    @builtin(vertex_index) i: u32,
) -> @builtin(position) vec4<f32> {
    // Indexing an array with a runtime value needs a variable, so copy the
    // constant array into one
    var clip_vertices_local = clip_vertices;
    let x = clip_vertices_local[i].x;
    let y = clip_vertices_local[i].y;

    // z is 0 as there is no depth in this pass, w is 1 for no perspective
    return vec4<f32>(x, y, 0.0, 1.0);
}

// Fragment shader entry point
// Reads the pixel at the same screen position from the offscreen image
@fragment
fn fragment_main(@builtin(position) frag_pos: vec4<f32>) -> @location(0) vec4<f32> {
    // In the fragment stage the position is in pixels, so drop the fractional
    // part to get the integer texel to read
    let coords = vec2<i32>(frag_pos.xy);

    // Exact texel read at mip level 0, no sampler or filtering needed
    let colour = textureLoad(scene, coords, 0);

    // Calculate luminance. The scene texture is sRGB, so the texel read above
    // is linear. Gamma-encode it so the threshold compares perceived
    // brightness, otherwise mid-greys would come out far too dark
    let linear_luminance = dot(colour.rgb, vec3<f32>(0.299, 0.587, 0.114));
    let luminance = pow(linear_luminance, 1.0 / 2.2);

    // Look up this pixel's threshold from the Bayer matrix, which tiles every
    // 8 pixels in x and y. Indexing with a runtime value needs a variable, so
    // copy the constant array into one
    var bayer_local = bayer;
    let x = u32(coords.x) % 8u;
    let y = u32(coords.y) % 8u;
    let threshold = (f32(bayer_local[y * 8u + x]) + 0.5) / 64.0;

    // Pixels brighter than their threshold take the light colour, the rest
    // take the dark colour
    let dithered = select(0.0, 1.0, luminance > threshold);
    let output_colour = mix(dark_colour, light_colour, dithered);
    return vec4<f32>(output_colour, 1.0);
}
