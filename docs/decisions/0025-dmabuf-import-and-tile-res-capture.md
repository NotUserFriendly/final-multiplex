# 0025. Zero-copy dmabuf import (deferred): Vulkan-HAL interop with CPU-copy fallback

- **Status:** Accepted
- **Date:** 2026-06-29

## Context

ADR-0024 set full zero-copy (hardware decode → dmabuf → wgpu import) as the GPU presentation
path's target. Implementation found the import path blocked at the library level: wgpu-hal 27
enables `VK_EXT_external_memory_dma_buf` on capable adapters but wires only a Win32
external-memory import (D3D11 handle). There is no high-level wgpu API to import a Linux dmabuf fd
as a texture.

## Decision

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
- **Native resolution is ultimately per-rect (res-appropriate-to-rect, a Phase-5 focus-mode
  concern):** native for large/focused rects, tile-res for thumbnails. This is a quality-and-
  bandwidth principle; its performance interaction with the render path is part of the render-rate
  investigation (below), not decided here.

## Consequences

- dmabuf, when built, is unsafe Vulkan-only code with a hard coupling to the Vulkan backend; the
  CPU-copy fallback bounds the blast radius.
- **Cost model (context):** capture-copy, texture-upload, and GPU-composite scale with displayed
  pixels; decode scales with source count; fullscreen 4K multiplies displayed pixels ~4×. The
  per-source decode wall is touched only by hardware decode (dmabuf's bundled half) — the primary
  use case (one viewer, many streams, fullscreen, discrete GPU) is exactly what the full
  hardware-decode + zero-copy path targets, which is why 0024 makes it the target.

## Scope — what this ADR deliberately does NOT decide

The GPU-path stutter root cause and the capture/upload-resolution performance tradeoff are
**unsettled empirical questions** tracked in `troubleshooting.md`. They are intentionally kept out
of this ADR: an ADR's rationale must be stable, and those findings have already revised more than
once (native-res copy cost → present-timing beat → render-rate bottleneck). The settled outcome —
e.g. a dedicated render surface, or an upload-resolution policy — will be captured in its own ADR
once measured. The interim capture resolution is whatever the render-rate investigation lands on;
this ADR makes no claim about it.

## Relationship

Implements and defers the zero-copy target of ADR-0024. ADR-0024 gets a one-line forward-pointer
("dmabuf import path: see ADR-0025").
