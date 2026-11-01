# reflecdith

A Rust + wgpu project to render 3D objects with reflective surfaces, then apply a
dithering post-process pass.

## Current milestone

**Goal 1:** open a window and display `models/utah_teapot.stl` spinning, with plain
(non-reflective) shading. No reflections or dithering yet — those are later milestones
once the basic pipeline works.

Rough build order for this milestone:

1. A window that stays open (winit `ApplicationHandler`, no GPU yet)
2. wgpu core objects wired to that window: Instance → Adapter → Device/Queue → Surface
3. Load the STL into a vertex buffer (CPU mesh via `stl_io` → GPU buffer)
4. A render pipeline: WGSL shader pair + pipeline state
5. A depth buffer (needed before the mesh will render sensibly)
6. A uniform buffer holding an MVP matrix, updated per-frame for rotation
7. The render loop: acquire frame → encode → submit → present

## Stack

- `wgpu` 30.0.1 — graphics API
- `winit` 0.30.13 — windowing/event loop (uses the `ApplicationHandler` trait pattern,
  not the older closure-based `run()`)
- `stl_io` — STL mesh loading
- `pollster` — blocks on wgpu's async device/adapter requests outside an async runtime
- `env_logger` — for wgpu's internal logging

## User background

- Understands GPU/rendering concepts already — has built a raytracing simulation with
  NVIDIA OptiX, and has watched a lot of shader-focused content. Conceptual grounding
  (pipelines, shaders, buffers) does not need re-explaining from scratch.
- Has **not** used a graphics API hands-on before (OpenGL/Vulkan/DirectX/wgpu) — the
  wgpu-specific object model (Instance/Adapter/Device/Queue/Surface) and WGSL are new.
- Rust knowledge is cursory (did the Google Rust course roughly a year ago, hasn't used
  it much since) — flag Rust-specific idioms/wrinkles (ownership, `Option`, lifetimes
  as they come up) rather than assuming familiarity.
