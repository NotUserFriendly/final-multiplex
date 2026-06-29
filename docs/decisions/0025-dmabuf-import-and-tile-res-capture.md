# 0025. dmabuf→wgpu import via Vulkan-HAL interop; capture reverts to tile-res, native-res deferred to focus mode

- **Status:** Accepted
- **Date:** 2026-06-29

## Context

ADR-0024 set full zero-copy (hardware decode → dmabuf → wgpu import) as the GPU presentation
path's target, with CPU-copy as the minimal milestone. Implementation surfaced two things:

- **The import path is blocked at the library level.** wgpu-hal 27 enables
  `VK_EXT_external_memory_dma_buf` on capable adapters but only wires a Win32 external-memory
  import (D3D11 handle). There is no high-level wgpu API to import a Linux dmabuf fd as a texture.
- **Native-res CPU capture (B2) regressed smoothness.** Moving the probe to the pre-scale tap so
  the ring captures full source-resolution frames costs proportionally more on the capture thread
  than tile-res — and on a uniform small grid the extra resolution is downscaled away at draw
  time, i.e. the path pays to copy pixels it then discards.

Cost model (reasoned this session): capture-copy and GPU-composite scale with **displayed
pixels** (≈ equal across grid density for a given output); **decode scales with source count** (a
4×4 of cameras is ~4× the decode of a 2×2 regardless of tile size). Fullscreen 4K multiplies
displayed pixels ~4× over the small-window test. The original 4K CPU-*composite* stutter is
already resolved (composite is on the GPU); the residual CPU cost at fullscreen is the per-frame
**capture copy** (~1 GB/s at 4K/30 fps), and the per-source **decode wall** is untouched by any
capture-side change.

## Decision

- **Revert GPU capture to tile-res** (post-scale tap, `vcaps_{id}:src`) as the default — capture
  what is displayed, not source-native. Restores the Block-3 smooth state and floors the
  fullscreen copy at output resolution. Optionally re-tighten the adapter scale + `vshmcaps` so
  native-res frames don't cross the transport unread.
- **Native-res capture is per-rect, not blanket, and belongs to focus mode (Phase 5).** Native
  resolution earns its cost only when a rect is large enough to show the extra pixels (the focused
  large tile). This is res-appropriate-to-rect (native for the focused tile, tile-res for
  thumbnails). B2's blanket native-res was a way station, superseded by per-rect selection.
- **When zero-copy dmabuf is built, do it via wgpu-HAL Vulkan interop:**
  `create_texture_from_hal::<wgpu::hal::vulkan::Api>` over a `vk::Image`/`vk::DeviceMemory`
  constructed from the dmabuf fd (`vkImportMemoryFdKHR`, `DMA_BUF_BIT_EXT`), adding `wgpu-hal` as a
  direct dependency with its `vulkan` feature. The standard technique until wgpu exposes a
  first-class import; it is unsafe, Vulkan-backend-only, and has no wgpu-level fallback.
- **Fallback is the tile-res CPU-copy path.** Where hardware decode / dmabuf import is unavailable
  (driver, format/modifier, non-Vulkan adapter), the CPU-copy path runs. dmabuf is the fast path,
  not the only path — same additive/fallback philosophy as the compositor tier (0024).
- **Sequencing: dmabuf is deferred to its own deliberate block, coupled to the focus-mode native
  tile and the watch-many-streams fullscreen case.** It is not on the critical path for
  uniform-grid smoothness (tile-res capture handles that), so it sequences **after Phase 4 (3rd
  source)**, not before.

## Consequences

- Uniform-grid smoothness is restored now without dmabuf; Phase 4 proceeds on a smooth tile-res
  GPU path.
- The fullscreen-4K residual is the tile-res capture copy at **output** resolution — lighter than
  native-res capture and bounded by output pixels, not source pixels. A fullscreen 4K 2×2
  measurement calibrates whether this floor is acceptable (dmabuf "eventually") or pinching
  (dmabuf "soon").
- dmabuf, when built, is unsafe Vulkan-only code with a maintenance cost and a hard coupling to
  the Vulkan backend; the CPU-copy fallback bounds the blast radius.
- **The decode wall is unaddressed by any capture-side work** — only hardware decode (the dmabuf
  path's bundled half) touches it. Many-source fullscreen walls are gated on that, not on
  capture resolution. This is the primary use case (one viewer, many streams, fullscreen, discrete
  GPU), which is why the full hardware-decode + zero-copy path is ADR-0024's *target* rather than a
  nice-to-have.
- res-appropriate-to-rect and native-res capture are now explicitly Phase-5 (focus mode) concerns;
  the dmabuf native tile is where they converge.

## Relationship

Implements and defers the zero-copy target of ADR-0024, and narrows its native-res capture from
blanket to per-rect. ADR-0024 gets a one-line forward-pointer ("dmabuf import path + capture-res
decision: see ADR-0025"), consistent with the existing inter-ADR pointers.
