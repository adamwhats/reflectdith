use std::path::{Path, PathBuf};

use bytemuck::{Pod, Zeroable};
use glam::{Quat, Vec3};
use wgpu::{Device, util::DeviceExt};

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

// Paths of every STL file in a directory, sorted by name
pub fn list_models(dir: &str) -> Vec<PathBuf> {
    let mut models: Vec<PathBuf> = std::fs::read_dir(dir)
        .expect(&format!("Failed to read model directory: {}", dir))
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.extension().is_some_and(|extension| extension == "stl"))
        .collect();
    models.sort();
    models
}

// A mesh loaded onto the GPU
pub struct Mesh {
    pub vertex_buffer: wgpu::Buffer,
    pub num_vertices: usize,
    // The lowest point of the mesh, which is where the floor goes
    pub min_y: f32,
}

// Load an STL file into a vertex buffer
pub fn create_mesh(device: &Device, file_path: &Path) -> Mesh {
    let vertices = from_stl(file_path);
    let vertex_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("Mesh vertex buffer"),
        contents: bytemuck::cast_slice(&vertices),
        usage: wgpu::BufferUsages::VERTEX,
    });
    let min_y = vertices
        .iter()
        .map(|vertex| vertex.position[1])
        .fold(f32::MAX, f32::min);
    Mesh {
        vertex_buffer,
        num_vertices: vertices.len(),
        min_y,
    }
}

// Half the width of the floor, which is a square centred on the origin. The
// shader fades it to black well inside this, so its edge is never seen
const FLOOR_HALF_EXTENT: f32 = 8.0;

// The number of vertices in the floor, which is two triangles
pub const FLOOR_VERTEX_COUNT: u32 = 6;

// Make a flat floor at height 0, facing up. It is moved to the right height
// by the model matrix it is drawn with
pub fn create_floor_buffer(device: &Device) -> wgpu::Buffer {
    let corner = |x: f32, z: f32| Vertex {
        position: [x, 0.0, z],
        normal: [0.0, 1.0, 0.0],
    };
    let e = FLOOR_HALF_EXTENT;
    let vertices = [
        corner(-e, -e),
        corner(-e, e),
        corner(e, e),
        corner(-e, -e),
        corner(e, e),
        corner(e, -e),
    ];
    device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("Floor vertex buffer"),
        contents: bytemuck::cast_slice(&vertices),
        usage: wgpu::BufferUsages::VERTEX,
    })
}

// Load the vertex data from an STL file
fn from_stl(file_path: &Path) -> Vec<Vertex> {
    // Read the STL file into a mesh object
    let mut file = std::fs::File::open(file_path).expect(&format!("Failed to open STL file: {}", file_path.display()));
    let mesh = stl_io::read_stl(&mut file).expect(&format!("Failed to read STL file: {}", file_path.display()));

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
