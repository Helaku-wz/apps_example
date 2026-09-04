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

// The ESP32-C6 Wi-Fi archive calls the C ABI `floor` symbol. BlueOS does not
// link a native libm, so provide the symbol using the vendored Rust libm crate.
#[no_mangle]
pub extern "C" fn floor(x: f64) -> f64 {
    libm::floor(x)
}
