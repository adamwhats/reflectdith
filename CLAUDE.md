# reflecdith

A Rust + wgpu project to render 3D objects with reflective surfaces, then apply a
dithering post-process pass. This is a learning project — see "Teaching style" below,
it governs how Claude should behave here above almost anything else.

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

## Teaching style — read this before touching code

**Claude must not write implementation code in this project unless explicitly asked
to for a specific one-off reason.** The user is treating this as a guided learning
project, not a delivery task. Default workflow per concept:

1. Claude explains the concept (what it is, why it exists, how it fits the pipeline).
2. Claude describes what the code needs to do in plain English / structural terms
   (function names, struct shape, control flow) — not Rust syntax to copy-paste.
3. The user writes the actual Rust.
4. Claude reviews what they wrote, explains bugs or improvements, and only then moves
   on to the next concept.

Keep lessons scoped to one concept/milestone step at a time rather than dumping the
whole remaining roadmap at once.

## User background

- Understands GPU/rendering concepts already — has built a raytracing simulation with
  NVIDIA OptiX, and has watched a lot of shader-focused content. Conceptual grounding
  (pipelines, shaders, buffers) does not need re-explaining from scratch.
- Has **not** used a graphics API hands-on before (OpenGL/Vulkan/DirectX/wgpu) — the
  wgpu-specific object model (Instance/Adapter/Device/Queue/Surface) and WGSL are new.
- Rust knowledge is cursory (did the Google Rust course roughly a year ago, hasn't used
  it much since) — flag Rust-specific idioms/wrinkles (ownership, `Option`, lifetimes
  as they come up) rather than assuming familiarity.
