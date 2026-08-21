// Copyright (c) 2025 vivo Mobile Communication Co., Ltd.
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//       http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

#![allow(internal_features)]
#![feature(cfg_boolean_literals)]
#![feature(core_intrinsics)]
#![no_main]

extern crate libm;
extern crate librs;

mod app_window {
    include!(env!("SLINT_UI_GENERATED"));
}
mod math;

use crate::app_window::MainWindow;
use core::alloc::{GlobalAlloc, Layout};
use core::ptr::NonNull;
use core::sync::atomic::{compiler_fence, Ordering};
use librs::{c_str::CStr, syscall::Syscall};
use linked_list_allocator::Heap;
use slint::platform::software_renderer::{LineBufferProvider, Rgb565Pixel};
use spin::Mutex;
use std::cell::RefCell;
use std::io::{Error, ErrorKind, Result as IoResult};
use std::rc::Rc;
use std::thread;

const LCD_H_RES: u16 = 320;
const LCD_V_RES: u16 = 480;
const FRAME_DELAY_MS: libc::c_uint = 16;
const UI_THREAD_STACK_SIZE: usize = 64 * 1024;

const HEAP_SIZE: usize = 48 * 1024;

extern "C" {
    static __heap_start: u8;
    static __heap_end: u8;
}

struct HeapAllocator {
    heap: Mutex<Heap>,
}

impl HeapAllocator {
    const fn new() -> Self {
        Self {
            heap: Mutex::new(Heap::empty()),
        }
    }

    fn initialize(&self, heap: &mut Heap) -> bool {
        if !heap.bottom().is_null() {
            return true;
        }

        let start = core::ptr::addr_of!(__heap_start) as *mut u8;
        let end = core::ptr::addr_of!(__heap_end) as usize;
        let size = end.saturating_sub(start as usize);
        if size != HEAP_SIZE {
            return false;
        }

        // The linker script reserves this range exclusively for the allocator.
        unsafe { heap.init(start, size) };
        true
    }
}

unsafe impl GlobalAlloc for HeapAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let mut heap = self.heap.lock();
        if !self.initialize(&mut heap) {
            return core::ptr::null_mut();
        }
        heap.allocate_first_fit(layout)
            .map_or(core::ptr::null_mut(), |ptr| ptr.as_ptr())
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        if ptr.is_null() {
            return;
        }

        let mut heap = self.heap.lock();
        if heap.bottom().is_null() {
            return;
        }

        // Every non-null pointer passed here came from `allocate_first_fit`.
        heap.deallocate(NonNull::new_unchecked(ptr), layout);
    }
}

#[global_allocator]
static GLOBAL_ALLOCATOR: HeapAllocator = HeapAllocator::new();

// Reached when a panic unwinds under `-Cpanic=abort` (rustc lowers every panic
// to a call of this C-ABI symbol), or when C glue (libm/libatomic/slint) calls
// abort() directly. Must trap immediately without going through the panic
// machinery — otherwise `unreachable!()`/`panic!` here would re-enter abort()
// and overflow the stack. `core::intrinsics::abort` lowers to a trap
// instruction (EBREAK/`unimp` on RISC-V, UDF on ARM), matching the kernel's
// authoritative definition in kernel/infra/src/string.rs.
#[no_mangle]
pub extern "C" fn abort() -> ! {
    core::intrinsics::abort();
}

// RV32IMC has no A extension. The linked libatomic implementation serializes
// its fallback operations by calling these IRQ save/restore hooks.
#[no_mangle]
pub extern "C" fn disable_local_irq_save() -> usize {
    const MSTATUS_MIE: usize = 1 << 3;
    compiler_fence(Ordering::SeqCst);
    let old: usize;
    unsafe {
        core::arch::asm!(
            "csrrci {old}, mstatus, {bit}",
            bit = const MSTATUS_MIE,
            old = out(reg) old,
            options(nostack),
        );
    }
    old
}

#[no_mangle]
pub extern "C" fn enable_local_irq_restore(old: usize) {
    unsafe {
        core::arch::asm!("csrw mstatus, {old}", old = in(reg) old, options(nostack));
    }
    compiler_fence(Ordering::SeqCst);
}

#[derive(Clone, Copy)]
enum PixelFormat {
    Rgb565,
    Bgra8888,
}

impl PixelFormat {
    fn bytes_per_pixel(self) -> u32 {
        match self {
            Self::Rgb565 => 2,
            Self::Bgra8888 => 4,
        }
    }
}

struct FbFile {
    fd: libc::c_int,
    fixed_info: libc::fb_fix_screeninfo,
    variable_info: libc::fb_var_screeninfo,
    pixel_format: PixelFormat,
}

/// Renders into one reusable scanline instead of allocating a full-frame pixel buffer.
///
/// This keeps the software renderer's SRAM usage low. Slint 1.17 does not support `Path`
/// items in `render_by_line`, so this backend intentionally does not accept `Path` items.
struct FbLineBuffer<'a> {
    fb: &'a mut FbFile,
    pixels: [Rgb565Pixel; LCD_H_RES as usize],
    result: &'a mut IoResult<()>,
}

impl<'a> FbLineBuffer<'a> {
    fn new(fb: &'a mut FbFile, result: &'a mut IoResult<()>) -> Self {
        Self {
            fb,
            pixels: [Rgb565Pixel(0); LCD_H_RES as usize],
            result,
        }
    }
}

impl LineBufferProvider for FbLineBuffer<'_> {
    type TargetPixel = Rgb565Pixel;

    fn process_line(
        &mut self,
        line: usize,
        range: core::ops::Range<usize>,
        render_fn: impl FnOnce(&mut [Self::TargetPixel]),
    ) {
        let pixel_count = range.len();
        debug_assert!(pixel_count <= self.pixels.len());

        let pixels = &mut self.pixels[..pixel_count];
        render_fn(pixels);

        if self.result.is_ok() {
            *self.result = self.fb.draw_line(pixels, range.start, line);
        }
    }
}

impl Drop for FbFile {
    fn drop(&mut self) {
        let _ = librs::syscall::sys::Sys::close(self.fd);
    }
}

unsafe fn ioctl(fd: libc::c_int, request: libc::c_ulong, arg: *mut libc::c_void) -> IoResult<()> {
    match librs::syscall::sys::Sys::ioctl(fd, request, arg) {
        Ok(ret) if ret < 0 => Err(syscall_error(ret)),
        Ok(_) => Ok(()),
        Err(librs::errno::Errno(errno)) => Err(Error::from_raw_os_error(errno)),
    }
}

impl FbFile {
    fn open() -> IoResult<Self> {
        let path = CStr::from_bytes_with_nul(b"/dev/fb0\0")
            .map_err(|_| Error::from_raw_os_error(libc::EINVAL))?;
        let fd = librs::syscall::sys::Sys::open(path, libc::O_RDWR, 0);
        if fd < 0 {
            return Err(syscall_error(fd));
        }

        let mut fb = Self {
            fd,
            fixed_info: unsafe { core::mem::zeroed() },
            variable_info: unsafe { core::mem::zeroed() },
            pixel_format: PixelFormat::Rgb565,
        };

        if let Err(err) = fb.load_info().and_then(|_| fb.validate_format()) {
            return Err(err);
        }

        Ok(fb)
    }

    fn load_info(&mut self) -> IoResult<()> {
        unsafe {
            ioctl(
                self.fd,
                libc::FBIOGET_FSCREENINFO,
                &mut self.fixed_info as *mut libc::fb_fix_screeninfo as *mut libc::c_void,
            )?;
            ioctl(
                self.fd,
                libc::FBIOGET_VSCREENINFO,
                &mut self.variable_info as *mut libc::fb_var_screeninfo as *mut libc::c_void,
            )?;
        }
        Ok(())
    }

    fn validate_format(&mut self) -> IoResult<()> {
        let info = &self.variable_info;
        let fixed = &self.fixed_info;
        let pixel_format = if is_rgb565(info) {
            PixelFormat::Rgb565
        } else if is_bgra8888(info) {
            PixelFormat::Bgra8888
        } else {
            return Err(Error::new(
                ErrorKind::InvalidData,
                "unsupported framebuffer format",
            ));
        };
        let min_line_length = info
            .xres
            .checked_mul(pixel_format.bytes_per_pixel())
            .ok_or_else(|| Error::from_raw_os_error(libc::EINVAL))?;
        let min_size = fixed
            .line_length
            .checked_mul(info.yres)
            .ok_or_else(|| Error::from_raw_os_error(libc::EINVAL))?;

        if info.xres < LCD_H_RES as u32
            || info.yres < LCD_V_RES as u32
            || fixed.line_length < min_line_length
            || fixed.smem_len < min_size
        {
            return Err(Error::new(
                ErrorKind::InvalidData,
                "unsupported framebuffer format",
            ));
        }

        self.pixel_format = pixel_format;
        Ok(())
    }

    fn draw_line(
        &mut self,
        pixels: &[Rgb565Pixel],
        origin_x: usize,
        origin_y: usize,
    ) -> IoResult<()> {
        let dst_offset = origin_y as u64 * self.fixed_info.line_length as u64
            + origin_x as u64 * self.pixel_format.bytes_per_pixel() as u64;
        if dst_offset > libc::off_t::MAX as u64 {
            return Err(Error::from_raw_os_error(libc::EINVAL));
        }

        let offset =
            librs::syscall::sys::Sys::lseek(self.fd, dst_offset as libc::off_t, libc::SEEK_SET);
        if offset < 0 {
            return Err(syscall_error(offset as libc::c_int));
        }

        match self.pixel_format {
            PixelFormat::Rgb565 => write_rgb565_line(self.fd, pixels),
            PixelFormat::Bgra8888 => write_bgra8888_line(self.fd, pixels),
        }
    }
}

fn write_all(fd: libc::c_int, mut buf: &[u8]) -> IoResult<()> {
    while !buf.is_empty() {
        match librs::syscall::sys::Sys::write(fd, buf) {
            Ok(0) => {
                return Err(Error::new(
                    ErrorKind::WriteZero,
                    "failed to write framebuffer",
                ))
            }
            Ok(size) => buf = &buf[size..],
            Err(librs::errno::Errno(errno)) => return Err(Error::from_raw_os_error(errno)),
        }
    }

    Ok(())
}

fn write_rgb565_line(fd: libc::c_int, pixels: &[Rgb565Pixel]) -> IoResult<()> {
    let mut bytes = [0; 128];
    let mut used = 0;

    for pixel in pixels {
        let pixel_bytes = pixel.0.to_be_bytes();
        bytes[used] = pixel_bytes[0];
        bytes[used + 1] = pixel_bytes[1];
        used += 2;

        if used == bytes.len() {
            write_all(fd, &bytes)?;
            used = 0;
        }
    }

    if used > 0 {
        write_all(fd, &bytes[..used])?;
    }

    Ok(())
}

fn write_bgra8888_line(fd: libc::c_int, pixels: &[Rgb565Pixel]) -> IoResult<()> {
    let mut bytes = [0; 128];
    let mut used = 0;

    for pixel in pixels {
        let [b0, b1] = pixel.0.to_be_bytes();
        let rgb = u16::from_be_bytes([b0, b1]);
        bytes[used] = ((rgb & 0x001f) << 3) as u8;
        bytes[used + 1] = ((rgb & 0x07e0) >> 3) as u8;
        bytes[used + 2] = ((rgb & 0xf800) >> 8) as u8;
        bytes[used + 3] = 0xff;
        used += 4;

        if used == bytes.len() {
            write_all(fd, &bytes)?;
            used = 0;
        }
    }

    if used > 0 {
        write_all(fd, &bytes[..used])?;
    }

    Ok(())
}

fn is_rgb565(info: &libc::fb_var_screeninfo) -> bool {
    info.bits_per_pixel == 16
        && info.red.offset == 11
        && info.red.length == 5
        && info.green.offset == 5
        && info.green.length == 6
        && info.blue.offset == 0
        && info.blue.length == 5
}

fn is_bgra8888(info: &libc::fb_var_screeninfo) -> bool {
    info.bits_per_pixel == 32
        && info.red.offset == 16
        && info.red.length == 8
        && info.green.offset == 8
        && info.green.length == 8
        && info.blue.offset == 0
        && info.blue.length == 8
}

fn syscall_error(ret: libc::c_int) -> Error {
    if ret == -1 {
        Error::last_os_error()
    } else {
        Error::from_raw_os_error(-ret)
    }
}

fn uptime_millis() -> u128 {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };

    let ret = unsafe { librs::time::clock_gettime(librs::time::CLOCK_MONOTONIC, &mut ts) };

    if ret != 0 {
        return 0;
    }

    (ts.tv_sec as u128) * 1000 + (ts.tv_nsec as u128) / 1_000_000
}

struct BluekernelBackend {
    window: RefCell<Option<Rc<slint::platform::software_renderer::MinimalSoftwareWindow>>>,
}

impl BluekernelBackend {
    fn new() -> Self {
        Self {
            window: RefCell::new(None),
        }
    }
}

impl slint::platform::Platform for BluekernelBackend {
    fn create_window_adapter(
        &self,
    ) -> Result<Rc<dyn slint::platform::WindowAdapter>, slint::PlatformError> {
        let window = slint::platform::software_renderer::MinimalSoftwareWindow::new(
            slint::platform::software_renderer::RepaintBufferType::ReusedBuffer,
        );
        window.set_size(slint::PhysicalSize::new(LCD_H_RES as u32, LCD_V_RES as u32));
        self.window.replace(Some(window.clone()));
        Ok(window)
    }

    fn duration_since_start(&self) -> std::time::Duration {
        let t = uptime_millis();
        std::time::Duration::from_millis(t as u64)
    }

    fn run_event_loop(&self) -> Result<(), slint::PlatformError> {
        let mut fb = FbFile::open().map_err(|err| slint::PlatformError::Other(err.to_string()))?;

        loop {
            println!("Running slint event loop iteration");
            slint::platform::update_timers_and_animations();

            if let Some(window) = self.window.borrow().clone() {
                window.request_redraw();
                let mut draw_result = Ok(());
                window.draw_if_needed(|renderer| {
                    // Render line-by-line to avoid a full-frame RGB565 allocation. This saves
                    // substantial SRAM, at the cost of not supporting Slint `Path` items.
                    renderer.render_by_line(FbLineBuffer::new(&mut fb, &mut draw_result));
                });
                draw_result.map_err(|err| slint::PlatformError::Other(err.to_string()))?;

                let _ = librs::time::msleep(FRAME_DELAY_MS);
            } else {
                let _ = librs::time::msleep(FRAME_DELAY_MS);
            }
        }
    }
}

fn run_slint_ui() -> IoResult<()> {
    println!("Starting slint ui example");

    slint::platform::set_platform(Box::new(BluekernelBackend::new()))
        .map_err(|err| Error::new(ErrorKind::Other, err.to_string()))?;
    let ui = MainWindow::new().map_err(|err| Error::new(ErrorKind::Other, err.to_string()))?;

    slint::run_event_loop().map_err(|err| Error::new(ErrorKind::Other, err.to_string()))
}

#[no_mangle]
pub extern "C" fn _start() -> u32 {
    let ui_thread = thread::Builder::new()
        .name("slint-ui".to_string())
        .stack_size(UI_THREAD_STACK_SIZE)
        .spawn(run_slint_ui)
        .unwrap();
    ui_thread
        .join()
        .map_err(|_| Error::new(ErrorKind::Other, "slint ui thread panicked"))
        .unwrap();
    0
}
