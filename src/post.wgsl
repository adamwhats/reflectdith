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

// Tones for the dithering and the dither cell size, mirrors PostUniform in
// post.rs
struct Post {
    dark: vec4<f32>,
    light: vec4<f32>,
    // x is the size in pixels of each dither cell, y the number of tones
    block: vec4<f32>,
};

// The offscreen image the scene was rendered into
@group(0) @binding(0)
var scene: texture_2d<f32>;

@group(0) @binding(1)
var<uniform> post: Post;


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
    // part to get the integer pixel
    let pixel = vec2<i32>(frag_pos.xy);

    // The dither works in square cells of block pixels, which is 1 on screen.
    // A screenshot at a higher resolution uses bigger cells so the dither
    // looks the same, just sharper. Each cell reads the scene once, at its
    // centre, and gets one threshold
    let block = max(i32(post.block.x), 1);
    let cell = pixel / vec2<i32>(block);
    let max_texel = vec2<i32>(textureDimensions(scene)) - vec2<i32>(1);
    let coords = min(cell * vec2<i32>(block) + vec2<i32>(block / 2), max_texel);

    // Exact texel read at mip level 0, no sampler or filtering needed
    let colour = textureLoad(scene, coords, 0);

    // Calculate luminance. The scene texture holds linear values, which can go
    // past 1 where specular light is added to a surface which is already
    // bright, so clamp it. Gamma-encode it so the threshold compares perceived
    // brightness, otherwise mid-greys would come out far too dark
    let linear_luminance = dot(colour.rgb, vec3<f32>(0.299, 0.587, 0.114));
    let luminance = pow(min(linear_luminance, 1.0), 1.0 / 2.2);

    // Look up this pixel's threshold from the Bayer matrix, which tiles every
    // 8 pixels in x and y. Indexing with a runtime value needs a variable, so
    // copy the constant array into one
    var bayer_local = bayer;
    let x = u32(cell.x) % 8u;
    let y = u32(cell.y) % 8u;
    let bayer_threshold = (f32(bayer_local[y * 8u + x]) + 0.5) / 64.0;

    // The floor supplies a negative world-space blue-noise threshold in alpha.
    // Mesh alpha is 1 and retains the original screen-space Bayer pattern.
    let threshold = select(bayer_threshold, -colour.a, colour.a < 0.5);

    // Quantise the brightness to evenly spaced tones from dark to light. Scaled
    // to the tone steps, the whole part picks the band between two neighbouring
    // tones and the fraction is how far through the band the pixel is. The
    // threshold decides whether it takes the upper or the lower tone. With 2
    // levels there is one band, so it is just dark or light
    let levels = max(post.block.y, 2.0);
    let scaled = luminance * (levels - 1.0);
    let band = min(floor(scaled), levels - 2.0);
    let step = band + select(0.0, 1.0, scaled - band > threshold);
    let tone = step / (levels - 1.0);

    // Mix the two colours in gamma space, so the steps between tones look even
    // rather than bunching up at the bright end
    let encoded = mix(
        pow(post.dark.rgb, vec3<f32>(1.0 / 2.2)),
        pow(post.light.rgb, vec3<f32>(1.0 / 2.2)),
        tone,
    );
    let output_colour = pow(encoded, vec3<f32>(2.2));
    return vec4<f32>(output_colour, 1.0);
}
