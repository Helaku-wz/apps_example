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

#![feature(cfg_boolean_literals)]
extern crate esp_radio_sys;
extern crate libm;
extern crate librs;
extern crate rsrt;

mod app_window {
    include!(env!("SLINT_UI_GENERATED"));
}
mod background;
mod math;
mod wifi;

use crate::background::PanelRgb565Pixel;
use crate::app_window::MainWindow;
use librs::{c_str::CStr, syscall::Syscall};
use slint::platform::software_renderer::{LineBufferProvider, RepaintBufferType};
use slint::platform::{PointerEventButton, WindowAdapter, WindowEvent};
use slint::ComponentHandle;
use std::cell::RefCell;
use std::io::{Error, ErrorKind, Result as IoResult};
use std::rc::Rc;
use std::thread;

const LCD_H_RES: u16 = 480;
const LCD_V_RES: u16 = 480;
const FRAME_DELAY_MS: libc::c_uint = 16;
const UI_THREAD_STACK_SIZE: usize = 64 * 1024;
// Keep the 64-row render stripe on the heap. A full RGB565 frame is emitted in
// eight writes without consuming almost all of the UI thread's 64 KiB stack.
const RENDER_BATCH_ROWS: usize = 64;
const TOUCH_REPORT_SIZE: usize = 12;
const TOUCH_REPORT_VERSION: u8 = 1;
const TOUCH_DEVICE_PATH: &[u8] = b"/dev/cst9220\0";
const TOUCH_CONTROLLER_NAME: &str = "CST9220";
// CST9220 firmware reports coordinates in the mounted panel's logical direction.
// Do not mirror them again for the LCD controller's hardware scan direction.
const TOUCH_FLIP_X: bool = false;
const TOUCH_FLIP_Y: bool = false;
const TOUCH_SWAP_XY: bool = false;

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

#[derive(Clone, Copy, Default)]
struct TouchPoint {
    status: u8,
    x: u16,
    y: u16,
}

struct TouchReport {
    touch_count: u8,
    points: [TouchPoint; 2],
}

impl TouchReport {
    fn decode(bytes: &[u8; TOUCH_REPORT_SIZE]) -> IoResult<Self> {
        if bytes[0] != TOUCH_REPORT_VERSION || bytes[1] > 2 {
            return Err(Error::new(
                ErrorKind::InvalidData,
                "invalid CST9220 touch report",
            ));
        }

        let mut points = [TouchPoint::default(); 2];
        for (index, point) in points.iter_mut().enumerate() {
            let offset = 2 + index * 5;
            *point = TouchPoint {
                status: bytes[offset],
                x: u16::from_le_bytes([bytes[offset + 1], bytes[offset + 2]]),
                y: u16::from_le_bytes([bytes[offset + 3], bytes[offset + 4]]),
            };
        }

        Ok(Self {
            touch_count: bytes[1],
            points,
        })
    }

    fn active_point(&self) -> Option<TouchPoint> {
        if self.touch_count == 0 {
            return None;
        }

        self.points
            .iter()
            .copied()
            .find(|point| point.status != 0)
            .or(Some(self.points[0]))
    }
}

struct TouchFile {
    fd: libc::c_int,
    pressed: bool,
    last_x: f32,
    last_y: f32,
}

impl TouchFile {
    fn open() -> IoResult<Self> {
        let path = CStr::from_bytes_with_nul(TOUCH_DEVICE_PATH)
            .map_err(|_| Error::from_raw_os_error(libc::EINVAL))?;
        let fd = librs::syscall::sys::Sys::open(path, libc::O_RDONLY, 0);
        if fd < 0 {
            return Err(syscall_error(fd));
        }

        Ok(Self {
            fd,
            pressed: false,
            last_x: 0.0,
            last_y: 0.0,
        })
    }

    fn read_report(&self) -> IoResult<TouchReport> {
        let mut bytes = [0u8; TOUCH_REPORT_SIZE];
        match librs::syscall::sys::Sys::read(self.fd, &mut bytes) {
            Ok(TOUCH_REPORT_SIZE) => TouchReport::decode(&bytes),
            Ok(_) => Err(Error::new(ErrorKind::UnexpectedEof, "short CST9220 report")),
            Err(librs::errno::Errno(errno)) => Err(Error::from_raw_os_error(errno)),
        }
    }

    fn logical_position(point: TouchPoint) -> slint::LogicalPosition {
        let (raw_x, raw_y) = if TOUCH_SWAP_XY {
            (point.y, point.x)
        } else {
            (point.x, point.y)
        };
        let mut x = raw_x.min(LCD_H_RES - 1);
        let mut y = raw_y.min(LCD_V_RES - 1);
        if TOUCH_FLIP_X {
            x = LCD_H_RES - 1 - x;
        }
        if TOUCH_FLIP_Y {
            y = LCD_V_RES - 1 - y;
        }
        slint::LogicalPosition::new(x as f32, y as f32)
    }

    fn dispatch(
        &mut self,
        window: &slint::platform::software_renderer::MinimalSoftwareWindow,
    ) -> IoResult<()> {
        let report = self.read_report()?;
        match report.active_point() {
            Some(point) => {
                let position = Self::logical_position(point);
                if self.pressed {
                    if position.x != self.last_x || position.y != self.last_y {
                        window.dispatch_event(WindowEvent::PointerMoved { position });
                    }
                } else {
                    println!(
                        "{} press: raw=({}, {}), slint=({}, {})",
                        TOUCH_CONTROLLER_NAME, point.x, point.y, position.x, position.y
                    );
                    window.dispatch_event(WindowEvent::PointerPressed {
                        position,
                        button: PointerEventButton::Left,
                    });
                    self.pressed = true;
                }
                self.last_x = position.x;
                self.last_y = position.y;
            }
            None if self.pressed => {
                println!(
                    "{} release: slint=({}, {})",
                    TOUCH_CONTROLLER_NAME, self.last_x, self.last_y
                );
                window.dispatch_event(WindowEvent::PointerReleased {
                    position: slint::LogicalPosition::new(self.last_x, self.last_y),
                    button: PointerEventButton::Left,
                });
                self.pressed = false;
            }
            None => {}
        }
        Ok(())
    }
}

impl Drop for TouchFile {
    fn drop(&mut self) {
        let _ = librs::syscall::sys::Sys::close(self.fd);
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

    fn draw_line(&mut self, pixels: &[u8], origin_x: usize, origin_y: usize) -> IoResult<()> {
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

        write_all(self.fd, pixels)
    }
}

/// Renders into one reusable scanline and batches complete rows without allocating
/// a full-frame pixel buffer.
///
/// This keeps the software renderer's SRAM usage low. Slint 1.17 does not support `Path`
/// items in `render_by_line`, so this backend intentionally does not accept `Path` items.
struct FbLineBuffer<'a> {
    fb: &'a mut FbFile,
    output: [PanelRgb565Pixel; LCD_H_RES as usize],
    row_cache: &'a mut Vec<u8>,
    cached_row_start: usize,
    cached_row_count: usize,
    result: &'a mut IoResult<()>,
}

impl<'a> FbLineBuffer<'a> {
    fn new(fb: &'a mut FbFile, row_cache: &'a mut Vec<u8>, result: &'a mut IoResult<()>) -> Self {
        row_cache.clear();
        Self {
            fb,
            output: [PanelRgb565Pixel(0); LCD_H_RES as usize],
            row_cache,
            cached_row_start: 0,
            cached_row_count: 0,
            result,
        }
    }

    fn flush_rows(&mut self) {
        if self.cached_row_count == 0 {
            return;
        }

        if self.result.is_ok() {
            *self.result = self.fb.draw_line(self.row_cache, 0, self.cached_row_start);
        }
        self.row_cache.clear();
        self.cached_row_count = 0;
    }

    fn cache_full_line(&mut self, line: usize) {
        if self.cached_row_count == 0 {
            self.cached_row_start = line;
        } else if line != self.cached_row_start + self.cached_row_count {
            self.flush_rows();
            if self.result.is_err() {
                return;
            }
            self.cached_row_start = line;
        }

        match self.fb.pixel_format {
            PixelFormat::Rgb565 => {
                let pixels = &self.output[..LCD_H_RES as usize];
                // PanelRgb565Pixel is transparent over u16 and every element
                // was initialized by Slint before this copy.
                let bytes = unsafe {
                    core::slice::from_raw_parts(
                        pixels.as_ptr().cast::<u8>(),
                        core::mem::size_of_val(pixels),
                    )
                };
                self.row_cache.extend_from_slice(bytes);
            }
            PixelFormat::Bgra8888 => {
                append_bgra8888(self.row_cache, &self.output[..LCD_H_RES as usize]);
            }
        }
        self.cached_row_count += 1;
        if self.cached_row_count == RENDER_BATCH_ROWS {
            self.flush_rows();
        }
    }
}

impl LineBufferProvider for FbLineBuffer<'_> {
    type TargetPixel = PanelRgb565Pixel;

    fn provides_background(&self) -> bool {
        true
    }

    fn process_line(
        &mut self,
        line: usize,
        range: core::ops::Range<usize>,
        render_fn: impl FnOnce(&mut [Self::TargetPixel]),
    ) {
        let pixel_count = range.len();
        debug_assert!(pixel_count <= self.output.len());
        let is_full_width = range.start == 0 && pixel_count == LCD_H_RES as usize;

        // Partial rows have a framebuffer stride between them and cannot be
        // represented by the same compact write as consecutive full rows.
        if !is_full_width {
            self.flush_rows();
        }

        let pixels = &mut self.output[..pixel_count];
        background::copy_background_line(pixels, line, range.start);
        render_fn(pixels);

        if self.result.is_ok() {
            if is_full_width {
                self.cache_full_line(line);
            } else {
                match self.fb.pixel_format {
                    PixelFormat::Rgb565 => {
                        let bytes = unsafe {
                            core::slice::from_raw_parts(
                                pixels.as_ptr().cast::<u8>(),
                                core::mem::size_of_val(pixels),
                            )
                        };
                        *self.result = self.fb.draw_line(bytes, range.start, line);
                    }
                    PixelFormat::Bgra8888 => {
                        self.row_cache.clear();
                        append_bgra8888(self.row_cache, pixels);
                        *self.result = self.fb.draw_line(self.row_cache, range.start, line);
                        self.row_cache.clear();
                    }
                }
            }
        }
    }
}

impl Drop for FbLineBuffer<'_> {
    fn drop(&mut self) {
        self.flush_rows();
    }
}

fn append_bgra8888(bytes: &mut Vec<u8>, pixels: &[PanelRgb565Pixel]) {
    for pixel in pixels {
        let rgb = u16::from_be(pixel.0);
        bytes.push(((rgb & 0x001f) << 3) as u8);
        bytes.push(((rgb & 0x07e0) >> 3) as u8);
        bytes.push(((rgb & 0xf800) >> 8) as u8);
        bytes.push(0xff);
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
        let cache_capacity =
            LCD_H_RES as usize * RENDER_BATCH_ROWS * fb.pixel_format.bytes_per_pixel() as usize;
        let mut render_row_cache = Vec::with_capacity(cache_capacity);
        let mut touch = match TouchFile::open() {
            Ok(touch) => Some(touch),
            Err(error) => {
                println!("Failed to open /dev/cst9220: {error}");
                None
            }
        };
        let mut touch_error_reported = false;

        loop {
            slint::platform::update_timers_and_animations();

            if let Some(window) = self.window.borrow().clone() {
                // Dispatch input before drawing so its visual state is visible
                // in this iteration instead of one event-loop cycle later.
                if let Some(touch) = touch.as_mut() {
                    match touch.dispatch(&window) {
                        Ok(()) => touch_error_reported = false,
                        Err(error) if !touch_error_reported => {
                            println!("Failed to read CST9220 touch data: {error}");
                            touch_error_reported = true;
                        }
                        Err(_) => {}
                    }
                }

                let has_animations = window.window().has_active_animations();
                let mut draw_result = Ok(());
                window.draw_if_needed(|renderer| {
                    // Render line-by-line to avoid a full-frame RGB565 allocation. This saves
                    // substantial SRAM, at the cost of not supporting Slint `Path` items.
                    renderer.render_by_line(FbLineBuffer::new(
                        &mut fb,
                        &mut render_row_cache,
                        &mut draw_result,
                    ));
                });
                draw_result.map_err(|err| slint::PlatformError::Other(err.to_string()))?;

                let delay = if has_animations { FRAME_DELAY_MS } else { 30 };
                let _ = librs::time::msleep(delay);
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
    let _wifi_scan_timer = wifi::install(&ui);
    ui.show()
        .map_err(|err| Error::new(ErrorKind::Other, err.to_string()))?;

    slint::run_event_loop().map_err(|err| Error::new(ErrorKind::Other, err.to_string()))
}

fn main() -> IoResult<()> {
    let ui_thread = thread::Builder::new()
        .name("slint-ui".to_string())
        .stack_size(UI_THREAD_STACK_SIZE)
        .spawn(run_slint_ui)?;

    ui_thread
        .join()
        .map_err(|_| Error::new(ErrorKind::Other, "slint ui thread panicked"))?
}
