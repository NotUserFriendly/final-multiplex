● Found the cause. Here's what happened:

  Your program final-multiplex triggered a kernel panic.

  At [86332.668922] the kernel hit a NULL pointer dereference inside free_pages_and_swap_cache while your process called munmap(). The full
  call chain:

  munmap() syscall
    → do_vmi_munmap
      → unmap_region / unmap_vmas / zap_pte_range
        → tlb_flush_mmu
          → free_pages_and_swap_cache  ← NULL ptr dereference here

  Key details:
  - Process: PID 253683, final-multiplex (your program in this working directory)
  - Crash type: BUG: kernel NULL pointer dereference, address: 0x0 — kernel mode write to NULL
  - Taint flag: [O]=OOT_MODULE — out-of-tree (NVIDIA) drivers were loaded
  - Earlier: There were also two wgpu-subsurface process segfaults before the panic — those are separate GPU renderer crashes that didn't
  bring the kernel down

  This is most likely a kernel bug, not a bug in final-multiplex itself. The process simply called munmap() to release memory and the
  kernel's page table teardown code dereferenced a NULL pointer — that's not something user-space code normally causes. The NVIDIA
  out-of-tree modules (nvidia, nvidia_uvm, nvidia_drm) being loaded is a potential contributing factor, as driver memory management can
  corrupt page tables.

  I notice final-multiplex is also a directory in this working dir — is this something you're actively developing? If so, it might be worth
  looking at what kind of memory mapping it does (large mmap/munmap of GPU/shared memory regions tends to be where these surface).
