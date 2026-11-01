use wgpu::{
    Device, Extent3d, TextureDescriptor, TextureDimension, TextureFormat, TextureUsages, TextureView,
    TextureViewDescriptor,
};

pub const DEPTH_FORMAT: TextureFormat = TextureFormat::Depth32Float;

// The format the scene is rendered in. It is floating point so a colour can go
// past white, which is how specular highlights stand out from the diffuse light
// for the post pass to find
pub const SCENE_FORMAT: TextureFormat = TextureFormat::Rgba16Float;

// Create a depth texture view on the device of a given width/height
pub fn create_depth_view(device: &Device, width: u32, height: u32) -> TextureView {
    let depth_texture = device.create_texture(&TextureDescriptor {
        label: Some("Depth texture"),
        size: Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: TextureDimension::D2,
        format: DEPTH_FORMAT,
        usage: TextureUsages::RENDER_ATTACHMENT,
        view_formats: &[],
    });
    depth_texture.create_view(&TextureViewDescriptor::default())
}

// Create the offscreen colour texture view the scene is rendered into, of a
// given width/height. It is a render target for the scene pass and is
// then read by the post-process pass, so it needs both usages
pub fn create_scene_view(device: &Device, width: u32, height: u32) -> TextureView {
    let scene_texture = device.create_texture(&TextureDescriptor {
        label: Some("Scene texture"),
        size: Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: TextureDimension::D2,
        format: SCENE_FORMAT,
        usage: TextureUsages::RENDER_ATTACHMENT | TextureUsages::TEXTURE_BINDING,
        view_formats: &[],
    });
    scene_texture.create_view(&TextureViewDescriptor::default())
}
