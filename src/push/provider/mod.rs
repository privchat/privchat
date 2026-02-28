// Copyright 2024 Shanghai Boyu Information Technology Co., Ltd.
// https://privchat.dev
//
// Author: zoujiaqing <zoujiaqing@gmail.com>
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

pub mod apns;
pub mod fcm; // ✨ Phase 2: FCM Provider
pub mod hms;
pub mod lenovo;
pub mod meizu;
pub mod mock;
pub mod oppo;
pub mod provider_trait; // ✨ Phase 3: APNs Provider
pub mod vivo;
pub mod xiaomi;
pub mod zte;

pub use apns::ApnsProvider;
pub use fcm::FcmProvider; // ✨ Phase 2
pub use hms::HmsProvider;
pub use lenovo::LenovoProvider;
pub use meizu::MeizuProvider;
pub use mock::MockProvider;
pub use oppo::OppoProvider;
pub use provider_trait::PushProvider; // ✨ Phase 3
pub use vivo::VivoProvider;
pub use xiaomi::XiaomiProvider;
pub use zte::ZteProvider;
