// Corners of a single triangle large enough to cover the whole screen. The
// screen is the square (-1,-1) to (1,1) in clip space, and the long edge of
// this triangle passes through (1,1), so the square is fully inside it. The
// GPU clips away the part that falls outside the screen
const clip_vertices = array<vec2<f32>, 3>(
    vec2<f32>(-1.0, -1.0),
    vec2<f32>(3.0, -1.0),
    vec2<f32>(-1.0, 3.0),
);

// Rank from 0 to 63 of a cell in the 8x8 Bayer matrix, so dividing by 64
// gives a threshold spread evenly over the pattern. Built from the bits of x
// and y rather than indexing a constant array, which would need a per-pixel
// copy into a local variable. The first rows are:
//   0 32  8 40  2 34 10 42
//  48 16 56 24 50 18 58 26
//  12 44  4 36 14 46  6 38
fn bayer_rank(x: u32, y: u32) -> u32 {
    let z = x ^ y;
    return ((z & 1u) << 5u) | ((y & 1u) << 4u) | ((z & 2u) << 2u) | ((y & 2u) << 1u) | ((z & 4u) >> 1u)
        | ((y & 4u) >> 2u);
}

// Tones for the dithering and the dither cell size, mirrors PostUniform in
// post.rs
struct Post {
    tones: array<vec4<f32>, 16>,
    // x is the capture scale, y the number of tones, z the Bayer density, w shared grid
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
    // One scene sample for the entire output cell. The silhouette, lighting,
    // shadow and threshold all share this screen-space grid.
    let capture_scale = max(post.block.x, 1.0);
    let density = clamp(post.block.z, 0.125, 1.0);
    let bayer_cell = vec2<i32>(floor(frag_pos.xy * density / capture_scale));
    let max_texel = vec2<i32>(textureDimensions(scene)) - vec2<i32>(1);
    let scene_density = select(1.0, density, post.block.w > 0.5);
    let scene_cell = floor(frag_pos.xy * scene_density / capture_scale);
    let centre = (scene_cell + vec2<f32>(0.5)) * capture_scale / scene_density;
    let coords = clamp(vec2<i32>(floor(centre)), vec2<i32>(0), max_texel);
    let colour = textureLoad(scene, coords, 0);

    // Calculate luminance. The scene texture holds linear values, which can go
    // past 1 where specular light is added to a surface which is already
    // bright, so clamp it. Gamma-encode it so the threshold compares perceived
    // brightness, otherwise mid-greys would come out far too dark
    let linear_luminance = dot(colour.rgb, vec3<f32>(0.299, 0.587, 0.114));
    let luminance = pow(min(linear_luminance, 1.0), 1.0 / 2.2);

    // Look up this pixel's threshold from the Bayer matrix, which tiles every
    // 8 pixels in x and y
    let x = u32(bayer_cell.x) % 8u;
    let y = u32(bayer_cell.y) % 8u;
    let bayer_threshold = (f32(bayer_rank(x, y)) + 0.5) / 64.0;

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
    // The finite palette is gamma-interpolated on the CPU when edited.
    return vec4<f32>(post.tones[u32(step)].rgb, 1.0);
}
