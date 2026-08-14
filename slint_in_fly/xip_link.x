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
  IROM (rx) : ORIGIN = 0x42110000, LENGTH = 0x100000
  RODATA (r) : ORIGIN = 0x3c120000, LENGTH = 0x100000
  RWDATA (rw) : ORIGIN = 0x3fcc7400, LENGTH = 0x1000
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
  } > RODATA :rodata

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

  /DISCARD/ :
  {
    *(.eh_frame*)
    *(.comment*)
  }

  ASSERT(SIZEOF(.text) > 0, "XIP payload has no text")
  ASSERT(SIZEOF(.text) <= LENGTH(IROM), "XIP text exceeds its flash page")
  ASSERT(SIZEOF(.rodata) <= LENGTH(RODATA), "XIP rodata exceeds its flash page")
  ASSERT(SIZEOF(.data) + SIZEOF(.bss) <= LENGTH(RWDATA),
         "XIP writable data exceeds its SRAM region")
}
