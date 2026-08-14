// Copyright (c) 2026 vivo Mobile Communication Co., Ltd.
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

#[no_mangle]
pub extern "C" fn floorf(x: f32) -> f32 {
    libm::floorf(x)
}

#[no_mangle]
pub extern "C" fn ceilf(x: f32) -> f32 {
    libm::ceilf(x)
}

#[no_mangle]
pub extern "C" fn roundf(x: f32) -> f32 {
    libm::roundf(x)
}

#[no_mangle]
pub extern "C" fn round(x: f64) -> f64 {
    libm::round(x)
}

#[no_mangle]
pub extern "C" fn sqrtf(x: f32) -> f32 {
    libm::sqrtf(x)
}

#[no_mangle]
pub extern "C" fn atan2f(y: f32, x: f32) -> f32 {
    libm::atan2f(y, x)
}

#[no_mangle]
pub extern "C" fn fmaxf(x: f32, y: f32) -> f32 {
    libm::fmaxf(x, y)
}

#[no_mangle]
pub extern "C" fn fminf(x: f32, y: f32) -> f32 {
    libm::fminf(x, y)
}

#[no_mangle]
pub extern "C" fn fmodf(x: f32, y: f32) -> f32 {
    libm::fmodf(x, y)
}

#[no_mangle]
pub extern "C" fn tanf(x: f32) -> f32 {
    libm::tanf(x)
}

#[no_mangle]
pub extern "C" fn cosf(x: f32) -> f32 {
    libm::cosf(x)
}

#[no_mangle]
pub extern "C" fn sinf(x: f32) -> f32 {
    libm::sinf(x)
}

#[no_mangle]
pub extern "C" fn exp2f(x: f32) -> f32 {
    libm::exp2f(x)
}

#[no_mangle]
pub extern "C" fn powf(x: f32, y: f32) -> f32 {
    libm::powf(x, y)
}

#[no_mangle]
pub extern "C" fn cbrtf(x: f32) -> f32 {
    libm::cbrtf(x)
}
