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
extern crate libm;
extern crate librs;
extern crate rsrt;

mod app_window {
    include!(env!("SLINT_UI_GENERATED"));
}
mod math;

use crate::app_window::MainWindow;
use librs::{c_str::CStr, syscall::Syscall};
use slint::platform::software_renderer::{LineBufferProvider, Rgb565Pixel};
use slint::platform::{PointerEventButton, WindowEvent};
use slint::ComponentHandle;
use std::cell::RefCell;
use std::io::{Error, ErrorKind, Result as IoResult};
use std::rc::Rc;
use std::thread;

const LCD_H_RES: u16 = 320;
const LCD_V_RES: u16 = 480;
const FRAME_DELAY_MS: libc::c_uint = 16;
const UI_THREAD_STACK_SIZE: usize = 64 * 1024;
const TOUCH_REPORT_SIZE: usize = 12;
const TOUCH_REPORT_VERSION: u8 = 1;
// FT6336U firmware reports coordinates in the mounted panel's logical direction.
// Do not mirror them again for the LCD controller's hardware scan direction.
const TOUCH_FLIP_X: bool = false;
const TOUCH_FLIP_Y: bool = false;
const TOUCH_SWAP_XY: bool = false;
const CPU_LOAD_MIN: i32 = 1200;
const CPU_LOAD_MAX: i32 = 9200;
const TEMPERATURE_MIN_TENTHS: i32 = 360;
const TEMPERATURE_MAX_TENTHS: i32 = 580;
const TEMPERATURE_STEP_TENTHS: i32 = 1;
const TEMPERATURE_FRAME_DIVIDER: u32 = 2;
const RAM_LOAD_MIN: i32 = 3000;
const RAM_LOAD_MAX: i32 = 8200;
const RAM_LOAD_STEP: i32 = 15;
const RAM_CAPACITY_KB: i32 = 512;
const CURRENT_MIN_MA: i32 = 110;
const CURRENT_MAX_MA: i32 = 260;
const CURRENT_STEP_MA: i32 = 1;
const CURRENT_FRAME_DIVIDER: u32 = 2;
const VOLTAGE_MIN_MV: i32 = 3240;
const VOLTAGE_MAX_MV: i32 = 3330;
const VOLTAGE_STEP_MV: i32 = 1;
const VOLTAGE_FRAME_DIVIDER: u32 = 4;
const MERCURY_ORBIT_FRAMES: u32 = 72;
const VENUS_ORBIT_FRAMES: u32 = 96;
const EARTH_ORBIT_FRAMES: u32 = 128;
const MARS_ORBIT_FRAMES: u32 = 168;
const JUPITER_ORBIT_FRAMES: u32 = 240;
const SATURN_ORBIT_FRAMES: u32 = 300;
const URANUS_ORBIT_FRAMES: u32 = 360;
const NEPTUNE_ORBIT_FRAMES: u32 = 420;
const PLUTO_ORBIT_FRAMES: u32 = 480;

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
            return Err(Error::new(ErrorKind::InvalidData, "invalid FT6336U report"));
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
        let path = CStr::from_bytes_with_nul(b"/dev/ft6336u0\0")
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
            Ok(_) => Err(Error::new(ErrorKind::UnexpectedEof, "short FT6336U report")),
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
                        "FT6336U press: raw=({}, {}), slint=({}, {})",
                        point.x, point.y, position.x, position.y
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
                println!("FT6336U release: slint=({}, {})", self.last_x, self.last_y);
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

fn advance_fake_value(value: &mut i32, step: &mut i32, min: i32, max: i32) {
    *value += *step;
    if *value >= max {
        *value = max;
        *step = -*step;
    } else if *value <= min {
        *value = min;
        *step = -*step;
    }
}

fn orbit_phase(frame_count: u32, orbit_frames: u32) -> f32 {
    (frame_count % orbit_frames) as f32 / orbit_frames as f32
}

struct FakeDataState {
    ui: Option<slint::Weak<MainWindow>>,
    frame_count: u32,
    cpu_load: i32,
    cpu_target: i32,
    cpu_target_frames: u32,
    cpu_display: i32,
    random_state: u32,
    temperature_tenths: i32,
    temperature_step: i32,
    ram_load: i32,
    ram_step: i32,
    ram_display: i32,
    current_ma: i32,
    current_step: i32,
    voltage_mv: i32,
    voltage_step: i32,
    voltage_display: i32,
}

impl Default for FakeDataState {
    fn default() -> Self {
        Self {
            ui: None,
            frame_count: 0,
            cpu_load: 6400,
            cpu_target: 4200,
            cpu_target_frames: 50,
            cpu_display: 64,
            random_state: 0x6d2b_79f5,
            temperature_tenths: 426,
            temperature_step: TEMPERATURE_STEP_TENTHS,
            ram_load: 4800,
            ram_step: -RAM_LOAD_STEP,
            ram_display: 48,
            current_ma: 186,
            current_step: CURRENT_STEP_MA,
            voltage_mv: 3290,
            voltage_step: -VOLTAGE_STEP_MV,
            voltage_display: 329,
        }
    }
}

impl FakeDataState {
    fn attach_ui(&mut self, ui: slint::Weak<MainWindow>) {
        self.ui = Some(ui);
    }

    fn next_random(&mut self) -> u32 {
        let mut value = self.random_state;
        value ^= value << 13;
        value ^= value >> 17;
        value ^= value << 5;
        self.random_state = value;
        value
    }

    fn select_cpu_target(&mut self) {
        let roll = self.next_random() % 100;
        let (min, max, hold_min, hold_span) = if roll < 55 {
            (1500, 4200, 45, 100)
        } else if roll < 88 {
            (4200, 7200, 35, 80)
        } else {
            (7200, CPU_LOAD_MAX, 18, 42)
        };
        let range = (max - min + 1) as u32;
        self.cpu_target = min + (self.next_random() % range) as i32;
        self.cpu_target_frames = hold_min + self.next_random() % hold_span;
    }

    fn update_cpu_load(&mut self) {
        let difference = self.cpu_target - self.cpu_load;
        if self.cpu_target_frames == 0 || difference.abs() < 20 {
            self.select_cpu_target();
        } else {
            self.cpu_target_frames -= 1;
        }

        let difference = self.cpu_target - self.cpu_load;
        let distance = difference.abs();
        let max_step = if distance > 2500 {
            55
        } else if distance > 1200 {
            35
        } else if distance > 400 {
            22
        } else {
            12
        };
        let step = difference.clamp(-max_step, max_step);
        let jitter = (self.next_random() % 9) as i32 - 4;
        self.cpu_load = (self.cpu_load + step + jitter).clamp(CPU_LOAD_MIN, CPU_LOAD_MAX);
    }

    fn update(&mut self) {
        self.frame_count = self.frame_count.wrapping_add(1);
        self.update_cpu_load();
        advance_fake_value(
            &mut self.ram_load,
            &mut self.ram_step,
            RAM_LOAD_MIN,
            RAM_LOAD_MAX,
        );

        if self.frame_count % TEMPERATURE_FRAME_DIVIDER == 0 {
            advance_fake_value(
                &mut self.temperature_tenths,
                &mut self.temperature_step,
                TEMPERATURE_MIN_TENTHS,
                TEMPERATURE_MAX_TENTHS,
            );
        }
        if self.frame_count % CURRENT_FRAME_DIVIDER == 0 {
            advance_fake_value(
                &mut self.current_ma,
                &mut self.current_step,
                CURRENT_MIN_MA,
                CURRENT_MAX_MA,
            );
        }
        if self.frame_count % VOLTAGE_FRAME_DIVIDER == 0 {
            advance_fake_value(
                &mut self.voltage_mv,
                &mut self.voltage_step,
                VOLTAGE_MIN_MV,
                VOLTAGE_MAX_MV,
            );
        }

        let Some(ui) = self.ui.as_ref().and_then(|ui| ui.upgrade()) else {
            return;
        };

        ui.set_cpu_level(self.cpu_load as f32 / 10_000.0);
        ui.set_ram_level(self.ram_load as f32 / 10_000.0);
        ui.set_mercury_phase(orbit_phase(self.frame_count, MERCURY_ORBIT_FRAMES));
        ui.set_venus_phase(orbit_phase(self.frame_count, VENUS_ORBIT_FRAMES));
        ui.set_earth_phase(orbit_phase(self.frame_count, EARTH_ORBIT_FRAMES));
        ui.set_mars_phase(orbit_phase(self.frame_count, MARS_ORBIT_FRAMES));
        ui.set_jupiter_phase(orbit_phase(self.frame_count, JUPITER_ORBIT_FRAMES));
        ui.set_saturn_phase(orbit_phase(self.frame_count, SATURN_ORBIT_FRAMES));
        ui.set_uranus_phase(orbit_phase(self.frame_count, URANUS_ORBIT_FRAMES));
        ui.set_neptune_phase(orbit_phase(self.frame_count, NEPTUNE_ORBIT_FRAMES));
        ui.set_pluto_phase(orbit_phase(self.frame_count, PLUTO_ORBIT_FRAMES));

        let cpu_display = self.cpu_load / 100;
        if cpu_display != self.cpu_display {
            self.cpu_display = cpu_display;
            ui.set_cpu_value(format!("{cpu_display}%").into());
        }

        let ram_display = self.ram_load / 100;
        if ram_display != self.ram_display {
            self.ram_display = ram_display;
            let ram_used_kb = ram_display * RAM_CAPACITY_KB / 100;
            ui.set_ram_value(format!("{ram_display}%").into());
            ui.set_ram_detail(format!("{ram_used_kb} / {RAM_CAPACITY_KB} KB").into());
        }

        if self.frame_count % TEMPERATURE_FRAME_DIVIDER == 0 {
            let temperature_whole = self.temperature_tenths / 10;
            let temperature_fraction = self.temperature_tenths % 10;
            let temperature_range = TEMPERATURE_MAX_TENTHS - TEMPERATURE_MIN_TENTHS;
            let temperature_level = 0.18
                + 0.70 * (self.temperature_tenths - TEMPERATURE_MIN_TENTHS) as f32
                    / temperature_range as f32;
            ui.set_temperature_level(temperature_level);
            ui.set_temperature_value(format!("{temperature_whole}.{temperature_fraction}").into());
        }

        if self.frame_count % CURRENT_FRAME_DIVIDER == 0 {
            ui.set_current_value(self.current_ma.to_string().into());
        }

        let voltage_display = self.voltage_mv / 10;
        if voltage_display != self.voltage_display {
            self.voltage_display = voltage_display;
            let voltage_volts = self.voltage_mv / 1000;
            let voltage_hundredths = self.voltage_mv % 1000 / 10;
            ui.set_voltage_value(format!("{voltage_volts}.{voltage_hundredths:02}").into());
        }
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
    fake_data: Rc<RefCell<FakeDataState>>,
}

impl BluekernelBackend {
    fn new(fake_data: Rc<RefCell<FakeDataState>>) -> Self {
        Self {
            window: RefCell::new(None),
            fake_data,
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
        let mut touch = match TouchFile::open() {
            Ok(touch) => Some(touch),
            Err(error) => {
                println!("Failed to open /dev/ft6336u0: {error}");
                None
            }
        };
        let mut touch_error_reported = false;

        loop {
            self.fake_data.borrow_mut().update();
            slint::platform::update_timers_and_animations();

            if let Some(window) = self.window.borrow().clone() {
                if let Some(touch) = touch.as_mut() {
                    match touch.dispatch(&window) {
                        Ok(()) => touch_error_reported = false,
                        Err(error) if !touch_error_reported => {
                            println!("Failed to read FT6336U touch data: {error}");
                            touch_error_reported = true;
                        }
                        Err(_) => {}
                    }
                }
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

    let fake_data = Rc::new(RefCell::new(FakeDataState::default()));
    slint::platform::set_platform(Box::new(BluekernelBackend::new(fake_data.clone())))
        .map_err(|err| Error::new(ErrorKind::Other, err.to_string()))?;
    let ui = MainWindow::new().map_err(|err| Error::new(ErrorKind::Other, err.to_string()))?;
    ui.on_touch_button_clicked(|| println!("Slint touch button clicked"));
    fake_data.borrow_mut().attach_ui(ui.as_weak());

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
