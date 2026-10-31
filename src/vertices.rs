use bytemuck::{Pod, Zeroable};
use glam::{Quat, Vec3};

// Holds data for a single vertex. For GPU compatibility, we need:
//  - repr(C) to ensure the struct is laid out in memory in the same way as a C
//    struct
// - The Pod and Zeroable traits from bytemuck to allow for safe casting between
//   this struct and raw byte slices
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub struct Vertex {
    position: [f32; 3],
    normal: [f32; 3],
}

// Centre the positions on the origin and scale them to fit a unit sphere, so
// the camera and lights don't depend on the size or position of the model
fn normalise(positions: &[Vec3]) -> Vec<Vec3> {
    let min = positions.iter().copied().fold(Vec3::splat(f32::MAX), Vec3::min);
    let max = positions.iter().copied().fold(Vec3::splat(f32::MIN), Vec3::max);
    let centre = (min + max) * 0.5;
    let radius = positions.iter().map(|p| p.distance(centre)).fold(0.0, f32::max);
    positions.iter().map(|p| (*p - centre) / radius).collect()
}

// Load the vertex data from an STL file
pub fn from_stl(file_path: &str) -> Vec<Vertex> {
    // Read the STL file into a mesh object
    let mut file = std::fs::File::open(file_path).expect(&format!("Failed to open STL file: {}", file_path));
    let mesh = stl_io::read_stl(&mut file).expect(&format!("Failed to read STL file: {}", file_path));

    // The mesh is authored Z-up, so rotate it -90 degrees around X to bring it
    // into our Y-up world. Normals take the same rotation
    let rotation = Quat::from_rotation_x(-std::f32::consts::FRAC_PI_2);
    let rotated: Vec<Vec3> = mesh
        .vertices
        .iter()
        .map(|p| rotation * Vec3::new(p[0], p[1], p[2]))
        .collect();

    let positions = normalise(&rotated);

    // Read the mesh data into a vector of Vertex structs
    let mut vertices: Vec<Vertex> = Vec::new();
    for triangle in mesh.faces.iter() {
        for vertex_index in triangle.vertices.iter() {
            // Positions are deduplicated and shared across triangles, so
            // access by index from IndexedMesh
            let position = positions[*vertex_index as usize];

            // Normals are stored per-triangle, not shared, so use this
            // triangle's normal directly
            let normal = rotation * Vec3::new(triangle.normal[0], triangle.normal[1], triangle.normal[2]);

            // Push the vertex data into the list
            vertices.push(Vertex {
                position: position.to_array(),
                normal: normal.to_array(),
            });
        }
    }

    // Return the vector
    vertices
}

// Maps the Vertex struct to a wgpu::VertexBufferLayout
pub fn buffer_layout<'a>() -> wgpu::VertexBufferLayout<'a> {
    wgpu::VertexBufferLayout {
        array_stride: std::mem::size_of::<Vertex>() as wgpu::BufferAddress,
        step_mode: wgpu::VertexStepMode::Vertex,
        attributes: &[
            // Position attribute
            wgpu::VertexAttribute {
                offset: 0,
                shader_location: 0,
                format: wgpu::VertexFormat::Float32x3,
            },
            // Normal attribute
            wgpu::VertexAttribute {
                offset: std::mem::offset_of!(Vertex, normal) as wgpu::BufferAddress,
                shader_location: 1,
                format: wgpu::VertexFormat::Float32x3,
            },
        ],
    }
}
