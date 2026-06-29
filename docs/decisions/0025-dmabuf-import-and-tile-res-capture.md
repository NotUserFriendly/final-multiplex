# 0025. Zero-copy dmabuf import (deferred); interim capture stays native-res

- **Status:** Accepted
- **Date:** 2026-06-29

## Context

ADR-0024 set full zero-copy (hardware decode → dmabuf → wgpu import) as the GPU presentation
path's target. Implementation surfaced a library-level block and a misattributed stutter; this
ADR records the dmabuf decision and corrects the capture-resolution course.

- **Import blocked at the library level.** wgpu-hal 27 enables `VK_EXT_external_memory_dma_buf` on
  capable adapters but wires only a Win32 external-memory import (D3D11 handle). There is no
  high-level wgpu API to import a Linux dmabuf fd as a texture.
- **Capture resolution is not a smoothness lever (correction).** A small, consistent stutter was
  first attributed to native-res capture cost (B2), prompting a planned revert to tile-res. An A/B
  recording — tile-res vs native-res, identical stutter — falsified that: the stutter is a
  present-timing beat (frame selection driven by a 16 ms wall-clock timer, ≈62.5 Hz, beating
  against ~60 Hz vsync), fixed separately and independent of capture resolution. Capture
  resolution is therefore a **copy-cost / crispness tradeoff, not a smoothness one.**

## Decision

- **No tile-res revert.** Interim GPU capture stays at **native input resolution** (B2). The
  misattributed-stutter reason for reverting is void; native-res keeps tiles crisp at any display
  size — relevant for the fullscreen-primary use case — and the GPU scales per-rect on the fly
  without caps renegotiation on resize. Its cost is extra CPU copy and transport bandwidth for
  pixels not currently shown: real, but not stutter-causing, and addressed by zero-copy (below)
  and res-appropriate-to-rect (Phase 5).
- **Native-res is ultimately per-rect (res-appropriate-to-rect), a Phase-5 (focus mode)
  concern:** native for large/focused rects, tile-res for thumbnails. Blanket native-res (B2) is
  the interim until per-rect selection exists. This is the path by which the capture-resolution
  question is finally settled — informed by a fullscreen-4K measurement taken **after** the vsync
  fix (so it reflects copy cost, not the timer beat).
- **When zero-copy dmabuf is built, do it via wgpu-HAL Vulkan interop:**
  `create_texture_from_hal::<wgpu::hal::vulkan::Api>` over a `vk::Image`/`vk::DeviceMemory`
  constructed from the dmabuf fd (`vkImportMemoryFdKHR`, `DMA_BUF_BIT_EXT`), adding `wgpu-hal` as a
  direct dependency with its `vulkan` feature. The standard technique until wgpu exposes a
  first-class import; it is unsafe, Vulkan-backend-only, and has no wgpu-level fallback.
- **Fallback is the CPU-copy path.** Where hardware decode / dmabuf import is unavailable (driver,
  format/modifier, non-Vulkan adapter), the CPU-copy path runs. dmabuf is the fast path, not the
  only path — same additive/fallback philosophy as the compositor tier (0024).
- **dmabuf is deferred to its own block, after Phase 4 (3rd source)**, coupled to the focus-mode
  native tile and the watch-many-streams fullscreen case.

## Consequences

- The capture-resolution choice is settled later, by Phase-5 res-appropriate-to-rect, informed by
  a post-vsync-fix fullscreen measurement. Until then native-res (crisp everywhere) is the interim
  — no change to current behavior.
- **Cost model:** capture-copy and GPU-composite scale with **displayed pixels** (≈ equal across
  grid density for a given output); **decode scales with source count**; fullscreen 4K multiplies
  displayed pixels ~4×. The original 4K CPU-*composite* stutter is resolved (composite on the GPU).
  The remaining capture-copy cost at fullscreen is what zero-copy dmabuf removes; the per-source
  **decode wall** is touched only by hardware decode (dmabuf's bundled half). The primary use case
  (one viewer, many streams, fullscreen, discrete GPU) is exactly what the full hardware-decode +
  zero-copy path targets — which is why ADR-0024 makes it the *target*, not a nice-to-have.
- dmabuf, when built, is unsafe Vulkan-only code with a hard coupling to the Vulkan backend; the
  CPU-copy fallback bounds the blast radius.

## Relationship

Implements and defers the zero-copy target of ADR-0024, and narrows its native-res capture from
blanket toward per-rect (Phase 5). ADR-0024 gets a one-line forward-pointer ("dmabuf import path +
capture-resolution: see ADR-0025").
