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

//! Sequential write benchmark for the File IO demo page.
//!
//! Writes a 4 KiB block to `/tmp/io_bench.bin` once per timer tick until the
//! target (512 KiB = 128 blocks) is reached. The live rate is computed from
//! the *measured* latency of each block write, and the average from the
//! elapsed time since the run started. This mirrors the Wi-Fi scanner module
//! (`wifi.rs`): a state machine driven by a repeated `slint::Timer`, which
//! keeps the benchmark off the UI render path while still updating the UI.

use crate::app_window::MainWindow;
use slint::ComponentHandle;
use std::cell::RefCell;
use std::fs::{create_dir_all, OpenOptions};
use std::io::{Error, ErrorKind, Result as IoResult, Seek, SeekFrom, Write};
use std::rc::Rc;

/// Block size written in each tick. 4 KiB matches typical page/cluster size.
const BLOCK_BYTES: usize = 4 * 1024;

// The only writable filesystem on this board is tmpfs (RAM-backed; vfs_init
// mounts no block storage on esp32c6_devkitc_1). The whole 0x6E610-byte HP RAM
// must hold code, data, the UI thread stack (64 KiB), the frame render stripe
// cache, the Slint tree, AND the benchmark file. To keep the file's tmpfs
// footprint a constant 4 KiB regardless of run length, each tick seeks back to
// offset 0 and overwrites the same block instead of appending. This measures
// sustained overwrite latency without growing the in-memory store.
const TARGET_BLOCKS: usize = 128;
const TOTAL_BYTES: usize = TARGET_BLOCKS * BLOCK_BYTES;
/// Tick cadence. Tmpfs is fast, so a block write is near-instant; this delay
/// paces the progress bar so it is actually visible rather than a single flash.
const TICK_INTERVAL_MS: u64 = 40;

const BENCH_DIR: &str = "/tmp";
const BENCH_PATH: &str = "/tmp/io_bench.bin";

/// Monotonic time in microseconds since boot. Reuses the kernel's
/// `CLOCK_MONOTONIC`, the same clock `uptime_millis` in `main.rs` uses, but
/// keeps microsecond resolution so a single 4 KiB write can be timed.
fn uptime_micros() -> u128 {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };

    let ret = unsafe { librs::time::clock_gettime(librs::time::CLOCK_MONOTONIC, &mut ts) };
    if ret != 0 {
        return 0;
    }

    (ts.tv_sec as u128) * 1_000_000 + (ts.tv_nsec as u128) / 1_000
}

#[derive(Clone, Copy)]
enum IoState {
    Idle,
    Running {
        /// File handle index into `file` (only one, kept simple) — retained
        /// so each tick appends one block instead of reopening the file.
        start_micros: u128,
        blocks_written: usize,
    },
    Done,
}

struct IoBench {
    state: IoState,
    /// Reused 4 KiB write buffer; filled once, never reallocated per tick.
    block: Vec<u8>,
    file: Option<std::fs::File>,
    start_requested: bool,
}

impl IoBench {
    fn new() -> Self {
        Self {
            state: IoState::Idle,
            block: vec![0u8; BLOCK_BYTES],
            file: None,
            start_requested: false,
        }
    }

    fn request_start(&mut self, ui: &MainWindow) {
        if matches!(self.state, IoState::Running { .. }) || self.start_requested {
            return;
        }
        self.start_requested = true;
        ui.set_io_running(true);
        ui.set_io_status_text("Opening file".into());
    }

    fn finish(&mut self, ui: &MainWindow, result: IoResult<()>) {
        self.state = IoState::Done;
        match result {
            Ok(()) => {
                // Show the final (last-block) rate as the live figure and keep
                // the running average as the headline number.
                ui.set_io_running(false);
                ui.set_io_status_text(format!("done — {} KiB written", TOTAL_BYTES / 1024).into());
            }
            Err(error) => {
                ui.set_io_running(false);
                ui.set_io_speed_kbps(0);
                // The full error is printed to the serial console for diagnosis;
                // the on-screen line is kept short because it is elided.
                let kind = error.kind();
                let raw = error.raw_os_error();
                println!(
                    "io_bench failed: kind={:?} errno={:?} msg={}",
                    kind, raw, error
                );
                let label = match raw {
                    Some(errno) => format!("write failed: errno {}", errno),
                    None => format!("write failed: {:?}", kind),
                };
                ui.set_io_status_text(label.into());
            }
        }
    }

    fn start_run(&mut self, ui: &MainWindow) {
        self.start_requested = false;
        ui.set_io_running(true);
        ui.set_io_bytes_written(0);
        ui.set_io_total_bytes((TOTAL_BYTES / 1024) as i32);
        ui.set_io_speed_kbps(0);
        ui.set_io_avg_speed_kbps(0);
        ui.set_io_block_count(0);
        ui.set_io_status_text("Writing 4 KiB blocks".into());

        // The root filesystem is tmpfs, but /tmp is not created by vfs_init,
        // so create it (idempotent) before opening the benchmark file. Without
        // this the open below fails with ENOENT because the parent dir is gone.
        // The dir persists across runs, so tolerate AlreadyExists (EEXIST):
        // BlueOS's create_dir_all surfaces EEXIST for an existing dir because
        // the is_dir() recovery path does not mask it.
        if let Err(error) = create_dir_all(BENCH_DIR) {
            if error.kind() != ErrorKind::AlreadyExists {
                self.finish(ui, Err(error));
                return;
            }
        }

        // Truncate any previous benchmark file so each run starts at offset 0.
        match OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(BENCH_PATH)
        {
            Ok(file) => {
                self.file = Some(file);
                self.state = IoState::Running {
                    start_micros: uptime_micros(),
                    blocks_written: 0,
                };
            }
            Err(error) => self.finish(ui, Err(error)),
        }
    }

    fn write_block(&mut self, ui: &MainWindow) {
        let now = uptime_micros();
        let (start_micros, blocks_written) =
            if let IoState::Running { start_micros, blocks_written } = self.state {
                (start_micros, blocks_written)
            } else {
                return;
            };

        let file = match self.file.as_mut() {
            Some(file) => file,
            None => {
                self.finish(ui, Err(Error::new(ErrorKind::Other, "no open file")));
                return;
            }
        };

        // Measure only the write itself so the rate reflects disk latency, not
        // the tick spacing. `block` is pre-filled with zeros at construction.
        //
        // Seek back to offset 0 before each write so the file stays a single
        // 4 KiB chunk: the tmpfs store never grows past one block, and the
        // benchmark exercises repeated overwrite latency rather than append.
        let write_start = uptime_micros();
        let write_result = file.seek(SeekFrom::Start(0)).and_then(|_| file.write_all(&self.block));
        let write_end = uptime_micros();

        if let Err(error) = write_result {
            self.finish(ui, Err(error));
            return;
        }

        let block_us = write_end.saturating_sub(write_start).max(1);
        let new_blocks = blocks_written + 1;
        // Live rate from this block's measured latency.
        let speed_kbps = (BLOCK_BYTES as u128 * 1_000_000 / block_us / 1024) as i32;

        // Running average across the whole run so far.
        let elapsed_us = now.saturating_sub(start_micros).max(1);
        let written_bytes = (new_blocks * BLOCK_BYTES) as u128;
        let avg_kbps = (written_bytes * 1_000_000 / elapsed_us / 1024) as i32;

        ui.set_io_bytes_written((written_bytes / 1024) as i32);
        ui.set_io_speed_kbps(speed_kbps);
        ui.set_io_avg_speed_kbps(avg_kbps);
        ui.set_io_block_count(new_blocks as i32);
        if new_blocks < TARGET_BLOCKS {
            ui.set_io_status_text(
                format!("block {}/{} · {} KB/s", new_blocks, TARGET_BLOCKS, speed_kbps).into(),
            );
        }

        if new_blocks >= TARGET_BLOCKS {
            // Ensure data reaches the store before declaring completion.
            let _ = file.flush();
            self.file.take(); // drops and closes the file
            self.finish(ui, Ok(()));
        } else {
            self.state = IoState::Running {
                start_micros,
                blocks_written: new_blocks,
            };
        }
    }

    fn tick(&mut self, ui: &MainWindow) {
        match self.state {
            IoState::Idle => {
                if self.start_requested {
                    self.start_run(ui);
                }
            }
            IoState::Running { .. } => self.write_block(ui),
            IoState::Done => {
                // Stay finished until the user requests another run.
                if self.start_requested {
                    self.start_run(ui);
                }
            }
        }
    }
}

/// Connect the IO benchmark to the shared launcher window. The returned
/// timer must remain alive for as long as the Slint event loop is running,
/// exactly like `wifi::install`.
pub(crate) fn install(ui: &MainWindow) -> slint::Timer {
    let bench = Rc::new(RefCell::new(IoBench::new()));
    let ui_weak = ui.as_weak();
    let callback_bench = bench.clone();
    ui.on_io_start_requested(move || {
        if let Some(ui) = ui_weak.upgrade() {
            callback_bench.borrow_mut().request_start(&ui);
        }
    });

    let bench_timer = slint::Timer::default();
    let timer_ui = ui.as_weak();
    bench_timer.start(
        slint::TimerMode::Repeated,
        std::time::Duration::from_millis(TICK_INTERVAL_MS),
        move || {
            if let Some(ui) = timer_ui.upgrade() {
                bench.borrow_mut().tick(&ui);
            }
        },
    );
    bench_timer
}
