use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

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
    pub position: [f32; 3],
    pub normal: [f32; 3],
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
    index_buffer: Option<wgpu::Buffer>,
    shadow_indices: Option<wgpu::Buffer>,
    // The lowest point of the mesh, which is where the floor goes
    pub min_y: f32,
}

// Load an STL file into a vertex buffer
pub fn create_mesh(device: &Device, file_path: &Path) -> Mesh {
    let vertices = from_stl(file_path);
    upload(device, &vertices)
}

// Half the width of the floor, which is a square centred on the origin. The
// shader fades it to black well inside this, so its edge is never seen
pub(crate) const FLOOR_HALF_EXTENT: f32 = 32.0;

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
pub(crate) fn from_stl(file_path: &Path) -> Vec<Vertex> {
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

// The scene shader interpolates normals flat, which takes each triangle's
// normal from its first vertex only. So the first corner keeps a vertex with
// its exact normal, while the other two may share any vertex at the same
// position. With hard normals this runs the vertex shader about once per
// triangle instead of three times, and renders identically: positions and
// triangle order are unchanged. Indexing is used only if it saves bytes.
fn indexed_geometry(vertices: &[Vertex]) -> Option<(Vec<Vertex>, Vec<u32>)> {
    let mut exact = HashMap::with_capacity(vertices.len());
    // The most recently used vertex at each position, for vertex cache hits
    let mut at_position = HashMap::with_capacity(vertices.len());
    let mut unique = Vec::new();
    let mut indices = Vec::with_capacity(vertices.len());
    for (corner, vertex) in vertices.iter().enumerate() {
        let position = vertex.position.map(f32::to_bits);
        let shared = (corner % 3 != 0).then(|| at_position.get(&position).copied()).flatten();
        let index = shared.unwrap_or_else(|| {
            *exact
                .entry((position, vertex.normal.map(f32::to_bits)))
                .or_insert_with(|| {
                    let index = u32::try_from(unique.len()).expect("Mesh exceeds u32 vertex indices");
                    unique.push(*vertex);
                    index
                })
        });
        at_position.insert(position, index);
        indices.push(index);
    }
    (unique.len() * std::mem::size_of::<Vertex>() + indices.len() * 4 < std::mem::size_of_val(vertices))
        .then_some((unique, indices))
}

// Depth rendering ignores normals. Reuse a representative vertex at each
// identical position so hard-normal seams do not defeat the vertex cache.
fn shadow_indices(stored: &[Vertex], indices: Option<&[u32]>, count: usize) -> Option<Vec<u32>> {
    let mut seen = HashMap::new();
    let mut out = Vec::with_capacity(count);
    for i in 0..count {
        let index = indices.map_or(i as u32, |indices| indices[i]);
        let key = stored[index as usize].position.map(f32::to_bits);
        out.push(*seen.entry(key).or_insert(index));
    }
    (seen.len() * 2 < count).then_some(out)
}

// Upload geometry in its authored coordinates.
pub fn upload(device: &Device, vertices: &[Vertex]) -> Mesh {
    let indexed = indexed_geometry(vertices);
    let stored = indexed.as_ref().map_or(vertices, |(unique, _)| unique.as_slice());
    Mesh {
        vertex_buffer: device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("Mesh vertices"),
            contents: bytemuck::cast_slice(stored),
            usage: wgpu::BufferUsages::VERTEX,
        }),
        index_buffer: indexed.as_ref().map(|(_, indices)| {
            device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("Mesh indices"),
                contents: bytemuck::cast_slice(indices),
                usage: wgpu::BufferUsages::INDEX,
            })
        }),
        shadow_indices: shadow_indices(
            stored,
            indexed.as_ref().map(|(_, indices)| indices.as_slice()),
            vertices.len(),
        )
        .map(|indices| {
            device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("Shadow position indices"),
                contents: bytemuck::cast_slice(&indices),
                usage: wgpu::BufferUsages::INDEX,
            })
        }),
        num_vertices: vertices.len(),
        min_y: vertices.iter().map(|v| v.position[1]).fold(f32::INFINITY, f32::min),
    }
}

impl Mesh {
    pub fn draw<'a>(&'a self, pass: &mut wgpu::RenderPass<'a>) {
        pass.set_vertex_buffer(0, self.vertex_buffer.slice(..));
        if let Some(indices) = &self.index_buffer {
            pass.set_index_buffer(indices.slice(..), wgpu::IndexFormat::Uint32);
            pass.draw_indexed(0..self.num_vertices as u32, 0, 0..1);
        } else {
            pass.draw(0..self.num_vertices as u32, 0..1);
        }
    }

    pub fn draw_shadow<'a>(&'a self, pass: &mut wgpu::RenderPass<'a>) {
        if let Some(indices) = &self.shadow_indices {
            pass.set_vertex_buffer(0, self.vertex_buffer.slice(..));
            pass.set_index_buffer(indices.slice(..), wgpu::IndexFormat::Uint32);
            pass.draw_indexed(0..self.num_vertices as u32, 0, 0..1);
        } else {
            self.draw(pass);
        }
    }

    pub fn gpu_bytes(&self) -> u64 {
        self.vertex_buffer.size()
            + self.index_buffer.as_ref().map_or(0, |b| b.size())
            + self.shadow_indices.as_ref().map_or(0, |b| b.size())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn indexing_preserves_positions_and_first_corner_normals_exactly() {
        let a = Vertex {
            position: [0., 0., 0.],
            normal: [0., 0., 1.],
        };
        let b = Vertex {
            position: [1., 0., 0.],
            ..a
        };
        let c = Vertex {
            position: [0., 1., 0.],
            ..a
        };
        let hard_edge = Vertex {
            normal: [0., 1., 0.],
            ..a
        };
        // The third triangle's other corners share the first two triangles'
        // vertices even though their normals differ
        let vertices = [a, b, c, a, b, c, hard_edge, b, c, b, hard_edge, c];
        let (unique, indices) = indexed_geometry(&vertices).unwrap();
        assert_eq!(unique.len(), 4);
        for (corner, (source, index)) in vertices.iter().zip(&indices).enumerate() {
            let stored = unique[*index as usize];
            assert_eq!(source.position.map(f32::to_bits), stored.position.map(f32::to_bits));
            if corner % 3 == 0 {
                assert_eq!(source.normal.map(f32::to_bits), stored.normal.map(f32::to_bits));
            }
        }
        assert!(indexed_geometry(&[a, b, c]).is_none());
        let shadow = shadow_indices(&unique, Some(&indices), vertices.len()).unwrap();
        for (source, index) in vertices.iter().zip(shadow) {
            assert_eq!(
                source.position.map(f32::to_bits),
                unique[index as usize].position.map(f32::to_bits)
            );
        }
        assert_eq!(shadow_indices(&[a, b, c], None, 3), None);
    }
}
