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
mod math;

use crate::app_window::{MainWindow, WifiNetwork};
use librs::{c_str::CStr, syscall::Syscall};
use slint::platform::software_renderer::{LineBufferProvider, Rgb565Pixel};
use slint::platform::{PointerEventButton, WindowEvent};
use slint::{ComponentHandle, Model};
use std::cell::RefCell;
use std::io::{Error, ErrorKind, Result as IoResult};
use std::rc::Rc;
use std::thread;

const LCD_H_RES: u16 = 480;
const LCD_V_RES: u16 = 480;
const FRAME_DELAY_MS: libc::c_uint = 16;
const UI_THREAD_STACK_SIZE: usize = 64 * 1024;
const SCAN_POLL_ATTEMPTS: usize = 25;
const SCAN_POLL_INTERVAL_MS: u128 = 200;
const WIFI_STARTUP_DELAY_MS: u128 = 1000;
const AUTO_SCAN_INTERVAL_MS: u128 = 15_000;
const SCAN_BUFFER_SIZE: usize = 2048;
const MAX_VISIBLE_NETWORKS: usize = 6;
const TOUCH_REPORT_SIZE: usize = 12;
const TOUCH_REPORT_VERSION: u8 = 1;
const TOUCH_DEVICE_PATH: &[u8] = b"/dev/cst9220\0";
const TOUCH_CONTROLLER_NAME: &str = "CST9220";
// CST9220 firmware reports coordinates in the mounted panel's logical direction.
// Do not mirror them again for the LCD controller's hardware scan direction.
const TOUCH_FLIP_X: bool = false;
const TOUCH_FLIP_Y: bool = false;
const TOUCH_SWAP_XY: bool = false;

#[derive(Debug)]
struct WifiNetworkInfo {
    ssid: String,
    signal_dbm: i8,
    channel: u16,
    security: u8,
}

struct WifiScanResults {
    total_count: usize,
    networks: Vec<WifiNetworkInfo>,
}

struct SocketFd(libc::c_int);

#[derive(Clone, Copy)]
enum WifiScanState {
    Idle,
    Waiting {
        poll_count: usize,
        next_poll_at: u128,
    },
}

struct WifiScanner {
    socket: Option<SocketFd>,
    scan_buffer: Vec<u8>,
    state: WifiScanState,
    scan_requested: bool,
    first_scan_at: u128,
    next_auto_scan_at: u128,
}

impl SocketFd {
    fn open_for_wifi_scan() -> IoResult<Self> {
        let fd =
            librs::net::socket::socket(libc::AF_INET, libc::SOCK_DGRAM | libc::SOCK_CLOEXEC, 0);
        if fd < 0 {
            Err(syscall_error(fd))
        } else {
            Ok(Self(fd))
        }
    }
}

impl Drop for SocketFd {
    fn drop(&mut self) {
        let _ = librs::syscall::sys::Sys::close(self.0);
    }
}

fn security_name(security: u8) -> &'static str {
    match security {
        0 => "Open",
        1 => "WEP",
        2 => "WPA",
        3 => "WPA2",
        4 => "WPA3",
        _ => "Unknown",
    }
}

fn signal_strength(signal_dbm: i8) -> i32 {
    match signal_dbm {
        -50..=i8::MAX => 4,
        -65..=-51 => 3,
        -75..=-66 => 2,
        _ => 1,
    }
}

fn wlan0_name() -> [libc::c_char; 16] {
    let mut name = [0 as libc::c_char; 16];
    for (dst, src) in name.iter_mut().zip(b"wlan0\0") {
        *dst = *src as libc::c_char;
    }
    name
}

fn trigger_wifi_scan(fd: libc::c_int) -> IoResult<()> {
    let scan_req = libc::iw_scan_req {
        scan_type: libc::IW_SCAN_TYPE_ACTIVE as u8,
        essid_len: 0,
        num_channels: 0,
        flags: 0,
        bssid: unsafe { core::mem::zeroed() },
        essid: [0u8; libc::IW_ESSID_MAX_SIZE],
        min_channel_time: 0,
        max_channel_time: 0,
        channel_list: [libc::iw_freq {
            m: 0,
            e: 0,
            i: 0,
            flags: 0,
        }; libc::IW_MAX_FREQUENCIES],
    };
    let mut iwreq = libc::iwreq {
        ifr_ifrn: libc::__c_anonymous_iwreq {
            ifrn_name: wlan0_name(),
        },
        u: libc::iwreq_data {
            essid: libc::iw_point {
                pointer: &scan_req as *const libc::iw_scan_req as *mut libc::c_void,
                length: core::mem::size_of::<libc::iw_scan_req>() as u16,
                flags: 0,
            },
        },
    };

    unsafe {
        librs::syscall::sys::Sys::ioctl(
            fd,
            libc::SIOCSIWSCAN,
            &mut iwreq as *mut _ as *mut libc::c_void,
        )
        .map(|_| ())
        .map_err(|librs::errno::Errno(errno)| Error::from_raw_os_error(errno))
    }
}

fn poll_wifi_scan(fd: libc::c_int, buffer: &mut Vec<u8>) -> IoResult<Option<usize>> {
    buffer.resize(SCAN_BUFFER_SIZE, 0);
    let data = libc::iw_point {
        pointer: buffer.as_mut_ptr() as *mut libc::c_void,
        length: buffer.len() as u16,
        flags: 0,
    };
    let mut iwreq = libc::iwreq {
        ifr_ifrn: libc::__c_anonymous_iwreq {
            ifrn_name: wlan0_name(),
        },
        u: libc::iwreq_data { data },
    };

    let result = unsafe {
        librs::syscall::sys::Sys::ioctl(
            fd,
            libc::SIOCGIWSCAN,
            &mut iwreq as *mut _ as *mut libc::c_void,
        )
    };
    match result {
        Ok(size) if size >= 0 => {
            let size = size as usize;
            if size > buffer.len() {
                return Err(Error::new(
                    ErrorKind::InvalidData,
                    "wireless scan result exceeds buffer",
                ));
            }
            Ok(Some(size))
        }
        Err(librs::errno::Errno(errno)) if errno == libc::EAGAIN => Ok(None),
        Err(librs::errno::Errno(errno)) => Err(Error::from_raw_os_error(errno)),
        _ => Err(Error::new(
            ErrorKind::InvalidData,
            "wireless scan returned an invalid size",
        )),
    }
}

fn take_bytes<'a>(buffer: &'a [u8], offset: &mut usize, len: usize) -> IoResult<&'a [u8]> {
    let end = offset
        .checked_add(len)
        .ok_or_else(|| Error::new(ErrorKind::InvalidData, "wireless scan result overflow"))?;
    let bytes = buffer
        .get(*offset..end)
        .ok_or_else(|| Error::new(ErrorKind::InvalidData, "truncated wireless scan result"))?;
    *offset = end;
    Ok(bytes)
}

fn decode_wifi_scan(buffer: &[u8]) -> IoResult<WifiScanResults> {
    let mut offset = 0;
    let count = take_bytes(buffer, &mut offset, 4)?;
    let count = u32::from_le_bytes(count.try_into().unwrap()) as usize;
    let mut networks: Vec<WifiNetworkInfo> = Vec::with_capacity(count.min(MAX_VISIBLE_NETWORKS));

    for _ in 0..count {
        let ssid_len = take_bytes(buffer, &mut offset, 4)?;
        let ssid_len = u32::from_le_bytes(ssid_len.try_into().unwrap()) as usize;
        if ssid_len > libc::IW_ESSID_MAX_SIZE {
            return Err(Error::new(
                ErrorKind::InvalidData,
                "wireless scan returned an invalid SSID length",
            ));
        }
        let ssid = take_bytes(buffer, &mut offset, ssid_len)?;
        let _bssid = take_bytes(buffer, &mut offset, 6)?;
        let signal_dbm = take_bytes(buffer, &mut offset, 1)?[0] as i8;
        let channel = take_bytes(buffer, &mut offset, 2)?;
        let channel = u16::from_le_bytes(channel.try_into().unwrap());
        let security = take_bytes(buffer, &mut offset, 1)?[0];

        let insert_at = if networks.len() < MAX_VISIBLE_NETWORKS {
            Some(networks.len())
        } else {
            networks
                .iter()
                .enumerate()
                .min_by_key(|(_, network)| network.signal_dbm)
                .and_then(|(index, weakest)| (signal_dbm > weakest.signal_dbm).then_some(index))
        };

        if let Some(index) = insert_at {
            let network = WifiNetworkInfo {
                ssid: if ssid.is_empty() {
                    String::from("<hidden network>")
                } else {
                    String::from_utf8_lossy(ssid).into_owned()
                },
                signal_dbm,
                channel,
                security,
            };
            if index == networks.len() {
                networks.push(network);
            } else {
                networks[index] = network;
            }
        }
    }

    networks.sort_by(|left, right| right.signal_dbm.cmp(&left.signal_dbm));
    Ok(WifiScanResults {
        total_count: count,
        networks,
    })
}

fn to_slint_network(network: &WifiNetworkInfo) -> WifiNetwork {
    WifiNetwork {
        ssid: network.ssid.as_str().into(),
        detail: format!(
            "{}  /  CHANNEL {}",
            security_name(network.security),
            network.channel
        )
        .into(),
        signal_text: format!("{} dBm", network.signal_dbm).into(),
        strength: signal_strength(network.signal_dbm),
        secure: network.security != 0,
    }
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
            // Wi-Fi row text changes after every scan. ReusedBuffer retains partial-rendering
            // caches for old text and eventually fragments the small ESP32-C6 heap. NewBuffer
            // clears that cache before each requested frame and redraws the full framebuffer.
            slint::platform::software_renderer::RepaintBufferType::NewBuffer,
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
                println!("Failed to open /dev/cst9220: {error}");
                None
            }
        };
        let mut touch_error_reported = false;

        loop {
            slint::platform::update_timers_and_animations();

            if let Some(window) = self.window.borrow().clone() {
                let mut draw_result = Ok(());
                window.draw_if_needed(|renderer| {
                    // Render line-by-line to avoid a full-frame RGB565 allocation. This saves
                    // substantial SRAM, at the cost of not supporting Slint `Path` items.
                    renderer.render_by_line(FbLineBuffer::new(&mut fb, &mut draw_result));
                });
                draw_result.map_err(|err| slint::PlatformError::Other(err.to_string()))?;

                // Poll after drawing so a stalled I2C bus cannot prevent the
                // initial UI frame from reaching the panel.
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

                let _ = librs::time::msleep(FRAME_DELAY_MS);
            } else {
                let _ = librs::time::msleep(FRAME_DELAY_MS);
            }
        }
    }
}

fn replace_network_rows(ui: &MainWindow, rows: Vec<WifiNetwork>) {
    let model = ui.get_networks();
    if let Some(model) = model
        .as_any()
        .downcast_ref::<slint::VecModel<WifiNetwork>>()
    {
        // Update rows individually so Slint keeps the existing repeater instances
        // and their rendering state. A model reset destroys and recreates all row
        // item trees, which needs a large contiguous allocation on every scan.
        let old_count = model.row_count();
        let new_count = rows.len();
        for (index, row) in rows.into_iter().enumerate() {
            if index < old_count {
                if model.row_data(index).as_ref() != Some(&row) {
                    model.set_row_data(index, row);
                }
            } else {
                model.push(row);
            }
        }
        for index in (new_count..old_count).rev() {
            model.remove(index);
        }
    } else {
        ui.set_networks(slint::ModelRc::new(slint::VecModel::from(rows)));
    }
}

fn show_scan_result(ui: &MainWindow, result: IoResult<WifiScanResults>) {
    ui.set_scanning(false);
    match result {
        Ok(results) => {
            let visible: Vec<WifiNetwork> = results.networks.iter().map(to_slint_network).collect();
            replace_network_rows(ui, visible);
            ui.set_result_count(results.total_count as i32);
            if results.total_count == 0 {
                ui.set_status_text("Scan completed; no access points found".into());
            } else if results.total_count > MAX_VISIBLE_NETWORKS {
                ui.set_status_text(
                    format!(
                        "Found {} networks; showing the strongest {}",
                        results.total_count, MAX_VISIBLE_NETWORKS
                    )
                    .into(),
                );
            } else {
                ui.set_status_text(format!("Found {} nearby networks", results.total_count).into());
            }
        }
        Err(error) => {
            replace_network_rows(ui, Vec::new());
            ui.set_result_count(0);
            ui.set_status_text(format!("Scan failed: {error}").into());
        }
    }
}

impl WifiScanner {
    fn new() -> Self {
        let first_scan_at = uptime_millis().saturating_add(WIFI_STARTUP_DELAY_MS);
        Self {
            socket: None,
            scan_buffer: vec![0u8; SCAN_BUFFER_SIZE],
            state: WifiScanState::Idle,
            scan_requested: true,
            first_scan_at,
            next_auto_scan_at: first_scan_at,
        }
    }

    fn request_scan(&mut self, ui: &MainWindow) {
        if matches!(self.state, WifiScanState::Waiting { .. }) || self.scan_requested {
            return;
        }

        self.scan_requested = true;
        ui.set_scanning(true);
        ui.set_status_text("Scan requested".into());
    }

    fn finish_scan(&mut self, ui: &MainWindow, result: IoResult<WifiScanResults>) {
        self.state = WifiScanState::Idle;
        self.next_auto_scan_at = uptime_millis().saturating_add(AUTO_SCAN_INTERVAL_MS);
        show_scan_result(ui, result);
    }

    fn start_scan(&mut self, ui: &MainWindow, now: u128) {
        self.scan_requested = false;
        ui.set_scanning(true);
        ui.set_status_text("Scanning channels 1 through 13".into());

        if self.socket.is_none() {
            match SocketFd::open_for_wifi_scan() {
                Ok(socket) => self.socket = Some(socket),
                Err(error) => {
                    self.finish_scan(ui, Err(error));
                    return;
                }
            }
        }

        match trigger_wifi_scan(self.socket.as_ref().unwrap().0) {
            Ok(()) => {
                self.state = WifiScanState::Waiting {
                    poll_count: 0,
                    next_poll_at: now.saturating_add(SCAN_POLL_INTERVAL_MS),
                };
            }
            Err(error) => self.finish_scan(ui, Err(error)),
        }
    }

    fn tick(&mut self, ui: &MainWindow) {
        let now = uptime_millis();
        match self.state {
            WifiScanState::Idle => {
                if now < self.first_scan_at
                    || (!self.scan_requested && now < self.next_auto_scan_at)
                {
                    return;
                }
                self.start_scan(ui, now);
            }
            WifiScanState::Waiting {
                poll_count,
                next_poll_at,
            } => {
                if now < next_poll_at {
                    return;
                }

                let fd = self.socket.as_ref().unwrap().0;
                match poll_wifi_scan(fd, &mut self.scan_buffer) {
                    Ok(Some(result_size)) => {
                        let result = decode_wifi_scan(&self.scan_buffer[..result_size]);
                        self.finish_scan(ui, result);
                    }
                    Ok(None) if poll_count + 1 < SCAN_POLL_ATTEMPTS => {
                        self.state = WifiScanState::Waiting {
                            poll_count: poll_count + 1,
                            next_poll_at: now.saturating_add(SCAN_POLL_INTERVAL_MS),
                        };
                    }
                    Ok(None) => self.finish_scan(
                        ui,
                        Err(Error::new(ErrorKind::TimedOut, "wireless scan timed out")),
                    ),
                    Err(error) => self.finish_scan(ui, Err(error)),
                }
            }
        }
    }
}

fn run_slint_ui() -> IoResult<()> {
    println!("Starting Slint wireless scan UI");

    slint::platform::set_platform(Box::new(BluekernelBackend::new()))
        .map_err(|err| Error::new(ErrorKind::Other, err.to_string()))?;
    let ui = MainWindow::new().map_err(|err| Error::new(ErrorKind::Other, err.to_string()))?;
    ui.set_networks(slint::ModelRc::new(slint::VecModel::default()));

    let scanner = Rc::new(RefCell::new(WifiScanner::new()));
    let ui_weak = ui.as_weak();
    let callback_scanner = scanner.clone();
    ui.on_scan_requested(move || {
        if let Some(ui) = ui_weak.upgrade() {
            callback_scanner.borrow_mut().request_scan(&ui);
        }
    });
    ui.show()
        .map_err(|err| Error::new(ErrorKind::Other, err.to_string()))?;

    let scan_timer = slint::Timer::default();
    let timer_ui = ui.as_weak();
    scan_timer.start(
        slint::TimerMode::Repeated,
        std::time::Duration::from_millis(SCAN_POLL_INTERVAL_MS as u64),
        move || {
            if let Some(ui) = timer_ui.upgrade() {
                scanner.borrow_mut().tick(&ui);
            }
        },
    );

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
