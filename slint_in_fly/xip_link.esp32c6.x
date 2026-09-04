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
  RWDATA (rw) : ORIGIN = 0x4085E610, LENGTH = 0x10000
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
  .text : ALIGN(4)
  {
    KEEP(*(.text._start))
    *(.text .text.*)
  } > IROM :text

  .rodata : ALIGN(4)
  {
    *(.rodata .rodata.*)
    *(.srodata .srodata.*)
  } > IROM :rodata
  
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


  ASSERT(SIZEOF(.text) > 0, "XIP payload has no text")
  ASSERT((ADDR(.text) & 0xffff) == 0,
         "XIP .text must start at a 64KB boundary (Ibus cache MMU entry)")
  ASSERT(ADDR(.text) + SIZEOF(.text) <= ORIGIN(IROM) + LENGTH(IROM),
         "XIP text exceeds its flash page")
  ASSERT(ADDR(.bss) + SIZEOF(.bss) <= ORIGIN(HEAP),
         "XIP writable data exceeds its 16KB SRAM region")
  ASSERT(SIZEOF(.heap) == LENGTH(HEAP),
         "XIP heap is not the expected 48KB")
}
