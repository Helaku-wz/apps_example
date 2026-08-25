/*
 * Copyright (c) 2026 vivo Mobile Communication Co., Ltd.
 *
 * Licensed under the Apache License, Version 2.0 (the "License");
 * you may not use this file except in compliance with the License.
 * You may obtain a copy of the License at
 *
 *       http://www.apache.org/licenses/LICENSE-2.0
 *
 * Unless required by applicable law or agreed to in writing, software
 * distributed under the License is distributed on an "AS IS" BASIS,
 * WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
 * See the License for the specific language governing permissions and
 * limitations under the License.
 */

ENTRY(_start)

MEMORY
{
  IROM (rx) : ORIGIN = 0x42200000, LENGTH = 0x100000
  RODATA (r) : ORIGIN = 0x3c140000, LENGTH = 0x100000
  RWDATA (rw) : ORIGIN = 0x3FCCE800, LENGTH = 0x10000
  /* Keep the first 16KB for initialized and zero-initialized data, and reserve
   * the remaining 48KB for the application's global allocator. */
  HEAP (rw) : ORIGIN = ORIGIN(RWDATA) + 0x4000, LENGTH = 0xC000
}

PHDRS
{
  text PT_LOAD FLAGS(5);
  rodata PT_LOAD FLAGS(4);
  data PT_LOAD FLAGS(6);
}

SECTIONS
{
  /* .rodata is laid out first so SIZEOF(.rodata) is known when .rotext_dummy
   * reserves the matching gap below. Reordering does not change its VMA — it
   * still lands at ORIGIN(RODATA). */
  .rodata : ALIGN(4)
  {
    *(.rodata .rodata.*)
    *(.srodata .srodata.*)
  } > RODATA :rodata

  /* The same physical flash is cache-mapped into both IROM (Ibus, 0x42xx_xxxx)
   * and RODATA (Dbus, 0x3cxx_xxxx) via a shared 64KB-entry MMU indexed by
   * (vaddr & 0x7fffff) >> 16. With .rodata at 0x3c120000 (entry 0x12) and .text
   * nominally at ORIGIN(IROM) = 0x42110000 (entry 0x11), a large .text spills
   * into entries 0x12.. and collides with .rodata — each entry names only one
   * physical page, so the cache hands back wrong bytes and XIP fetches crash.
   *
   * Mirror kernel/src/boards/seeed_xiao_esp32c3/link.x .rotext_dummy: reserve
   * .rodata's footprint inside IROM so .text starts past .rodata's last entry.
   * Two adjustments vs. the kernel: (1) ORIGIN(IROM) is one entry below
   * ORIGIN(RODATA) (0x11 vs 0x12), and (2) neither region carries the kernel's
   * 0x20 mapping offset, so ALIGN(0x10000) alone would land .text's first entry
   * exactly on .rodata's last entry. Skipping one extra 64KB page closes that
   * one-entry gap; the ASSERT below pins the invariant. */
  .rotext_dummy (NOLOAD) :
  {
    . = ALIGN(ALIGNOF(.rodata));
    . = . + SIZEOF(.rodata);
    . = ALIGN(0x10000);
    . = . + 0x10000;
    _rotext_reserved_start = .;
  } > IROM :NONE

  .text : ALIGN(4)
  {
    KEEP(*(.text._start))
    *(.text .text.*)
  } > IROM :text

  .data : ALIGN(4)
  {
    *(.sdata .sdata.*)
    *(.data .data.*)
    *(.got .got.*)
    *(.tdata .tdata.*)
  } > RWDATA :data

  .bss (NOLOAD) : ALIGN(4)
  {
    *(.sbss .sbss.*)
    *(.bss .bss.*)
    *(.tbss .tbss.*)
    *(COMMON)
  } > RWDATA :data

  .heap (NOLOAD) : ALIGN(8)
  {
    __heap_start = .;
    . = . + LENGTH(HEAP);
    __heap_end = .;
  } > HEAP :data

  /DISCARD/ :
  {
    *(.eh_frame*)
    *(.comment*)
  }

  /* First 64KB boundary at or after .rodata ends, in Dbus coordinates. */
  _rodata_mmu_end = (ORIGIN(RODATA) + SIZEOF(.rodata) + 0xffff) & ~0xffff;

  ASSERT(SIZEOF(.text) > 0, "XIP payload has no text")
  ASSERT((ADDR(.text) & 0xffff) == 0,
         "XIP .text must start at a 64KB boundary (Ibus cache MMU entry)")
  ASSERT((ADDR(.text) & 0x7fffff) >= (_rodata_mmu_end & 0x7fffff),
         "XIP .text overlaps .rodata Ibus/Dbus cache MMU entries")
  ASSERT(ADDR(.text) + SIZEOF(.text) <= ORIGIN(IROM) + LENGTH(IROM),
         "XIP text exceeds its flash page")
  ASSERT(SIZEOF(.rodata) <= LENGTH(RODATA), "XIP rodata exceeds its flash page")
  ASSERT(ADDR(.bss) + SIZEOF(.bss) <= ORIGIN(HEAP),
         "XIP writable data exceeds its 16KB SRAM region")
  ASSERT(SIZEOF(.heap) == LENGTH(HEAP),
         "XIP heap is not the expected 48KB")
}
