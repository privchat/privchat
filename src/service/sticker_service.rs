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

//! 表情包服务
//!
//! 提供表情包库管理功能，包括：
//! - 内置表情包库
//! - 表情包查询
//! - 表情包包详情

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// 表情包
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Sticker {
    /// 表情包 ID
    pub sticker_id: String,
    /// 所属表情包库 ID
    pub package_id: String,
    /// 图片 URL
    pub image_url: String,
    /// 替代文字（纯文本，客户端显示时添加方括号）
    pub alt_text: String,
    /// Emoji 表情（可选）
    pub emoji: Option<String>,
    /// 图片宽度（像素）
    pub width: u32,
    /// 图片高度（像素）
    pub height: u32,
    /// MIME 类型
    pub mime_type: String,
}

/// 表情包库
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StickerPackage {
    /// 表情包库 ID
    pub package_id: String,
    /// 表情包库名称
    pub name: String,
    /// 缩略图 URL
    pub thumbnail_url: String,
    /// 作者/来源
    pub author: String,
    /// 描述
    pub description: String,
    /// 表情包数量
    pub sticker_count: usize,
    /// 所有表情包（仅在获取详情时返回）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stickers: Option<Vec<Sticker>>,
}

/// 表情包服务
pub struct StickerService {
    /// 表情包库（package_id -> StickerPackage）
    packages: HashMap<String, StickerPackage>,
    /// 表情包（sticker_id -> Sticker）
    stickers: HashMap<String, Sticker>,
}

impl StickerService {
    /// 创建新的表情包服务（带内置表情包）
    pub fn new() -> Self {
        let mut service = Self {
            packages: HashMap::new(),
            stickers: HashMap::new(),
        };

        // 初始化内置表情包
        service.init_builtin_stickers();

        service
    }

    /// 初始化内置表情包库
    fn init_builtin_stickers(&mut self) {
        // 表情包库 1: 经典表情
        let classic_stickers = vec![
            Sticker {
                sticker_id: "classic_001".to_string(),
                package_id: "classic".to_string(),
                image_url: "https://cdn.example.com/stickers/classic/smile.png".to_string(),
                alt_text: "微笑".to_string(),
                emoji: Some("😊".to_string()),
                width: 128,
                height: 128,
                mime_type: "image/png".to_string(),
            },
            Sticker {
                sticker_id: "classic_002".to_string(),
                package_id: "classic".to_string(),
                image_url: "https://cdn.example.com/stickers/classic/laugh.png".to_string(),
                alt_text: "大笑".to_string(),
                emoji: Some("😆".to_string()),
                width: 128,
                height: 128,
                mime_type: "image/png".to_string(),
            },
            Sticker {
                sticker_id: "classic_003".to_string(),
                package_id: "classic".to_string(),
                image_url: "https://cdn.example.com/stickers/classic/cry.png".to_string(),
                alt_text: "哭泣".to_string(),
                emoji: Some("😢".to_string()),
                width: 128,
                height: 128,
                mime_type: "image/png".to_string(),
            },
            Sticker {
                sticker_id: "classic_004".to_string(),
                package_id: "classic".to_string(),
                image_url: "https://cdn.example.com/stickers/classic/love.png".to_string(),
                alt_text: "爱心".to_string(),
                emoji: Some("😍".to_string()),
                width: 128,
                height: 128,
                mime_type: "image/png".to_string(),
            },
            Sticker {
                sticker_id: "classic_005".to_string(),
                package_id: "classic".to_string(),
                image_url: "https://cdn.example.com/stickers/classic/angry.png".to_string(),
                alt_text: "生气".to_string(),
                emoji: Some("😠".to_string()),
                width: 128,
                height: 128,
                mime_type: "image/png".to_string(),
            },
            Sticker {
                sticker_id: "classic_006".to_string(),
                package_id: "classic".to_string(),
                image_url: "https://cdn.example.com/stickers/classic/cool.png".to_string(),
                alt_text: "酷".to_string(),
                emoji: Some("😎".to_string()),
                width: 128,
                height: 128,
                mime_type: "image/png".to_string(),
            },
            Sticker {
                sticker_id: "classic_007".to_string(),
                package_id: "classic".to_string(),
                image_url: "https://cdn.example.com/stickers/classic/sweat.png".to_string(),
                alt_text: "汗".to_string(),
                emoji: Some("😅".to_string()),
                width: 128,
                height: 128,
                mime_type: "image/png".to_string(),
            },
            Sticker {
                sticker_id: "classic_008".to_string(),
                package_id: "classic".to_string(),
                image_url: "https://cdn.example.com/stickers/classic/thinking.png".to_string(),
                alt_text: "思考".to_string(),
                emoji: Some("🤔".to_string()),
                width: 128,
                height: 128,
                mime_type: "image/png".to_string(),
            },
            Sticker {
                sticker_id: "classic_009".to_string(),
                package_id: "classic".to_string(),
                image_url: "https://cdn.example.com/stickers/classic/thumbsup.png".to_string(),
                alt_text: "点赞".to_string(),
                emoji: Some("👍".to_string()),
                width: 128,
                height: 128,
                mime_type: "image/png".to_string(),
            },
            Sticker {
                sticker_id: "classic_010".to_string(),
                package_id: "classic".to_string(),
                image_url: "https://cdn.example.com/stickers/classic/ok.png".to_string(),
                alt_text: "OK".to_string(),
                emoji: Some("👌".to_string()),
                width: 128,
                height: 128,
                mime_type: "image/png".to_string(),
            },
        ];

        let classic_package = StickerPackage {
            package_id: "classic".to_string(),
            name: "经典表情".to_string(),
            thumbnail_url: "https://cdn.example.com/stickers/classic/cover.png".to_string(),
            author: "PrivChat".to_string(),
            description: "最常用的经典表情包，适合日常聊天使用".to_string(),
            sticker_count: classic_stickers.len(),
            stickers: Some(classic_stickers.clone()),
        };

        // 存储表情包库
        self.packages.insert("classic".to_string(), classic_package);

        // 存储所有表情包
        for sticker in classic_stickers {
            self.stickers.insert(sticker.sticker_id.clone(), sticker);
        }

        // 表情包库 2: 动物表情
        let animal_stickers = vec![
            Sticker {
                sticker_id: "animal_001".to_string(),
                package_id: "animals".to_string(),
                image_url: "https://cdn.example.com/stickers/animals/cat.png".to_string(),
                alt_text: "猫咪".to_string(),
                emoji: Some("🐱".to_string()),
                width: 128,
                height: 128,
                mime_type: "image/png".to_string(),
            },
            Sticker {
                sticker_id: "animal_002".to_string(),
                package_id: "animals".to_string(),
                image_url: "https://cdn.example.com/stickers/animals/dog.png".to_string(),
                alt_text: "狗狗".to_string(),
                emoji: Some("🐶".to_string()),
                width: 128,
                height: 128,
                mime_type: "image/png".to_string(),
            },
            Sticker {
                sticker_id: "animal_003".to_string(),
                package_id: "animals".to_string(),
                image_url: "https://cdn.example.com/stickers/animals/rabbit.png".to_string(),
                alt_text: "兔子".to_string(),
                emoji: Some("🐰".to_string()),
                width: 128,
                height: 128,
                mime_type: "image/png".to_string(),
            },
            Sticker {
                sticker_id: "animal_004".to_string(),
                package_id: "animals".to_string(),
                image_url: "https://cdn.example.com/stickers/animals/panda.png".to_string(),
                alt_text: "熊猫".to_string(),
                emoji: Some("🐼".to_string()),
                width: 128,
                height: 128,
                mime_type: "image/png".to_string(),
            },
            Sticker {
                sticker_id: "animal_005".to_string(),
                package_id: "animals".to_string(),
                image_url: "https://cdn.example.com/stickers/animals/bear.png".to_string(),
                alt_text: "小熊".to_string(),
                emoji: Some("🐻".to_string()),
                width: 128,
                height: 128,
                mime_type: "image/png".to_string(),
            },
            Sticker {
                sticker_id: "animal_006".to_string(),
                package_id: "animals".to_string(),
                image_url: "https://cdn.example.com/stickers/animals/monkey.png".to_string(),
                alt_text: "猴子".to_string(),
                emoji: Some("🐵".to_string()),
                width: 128,
                height: 128,
                mime_type: "image/png".to_string(),
            },
            Sticker {
                sticker_id: "animal_007".to_string(),
                package_id: "animals".to_string(),
                image_url: "https://cdn.example.com/stickers/animals/pig.png".to_string(),
                alt_text: "小猪".to_string(),
                emoji: Some("🐷".to_string()),
                width: 128,
                height: 128,
                mime_type: "image/png".to_string(),
            },
            Sticker {
                sticker_id: "animal_008".to_string(),
                package_id: "animals".to_string(),
                image_url: "https://cdn.example.com/stickers/animals/fox.png".to_string(),
                alt_text: "狐狸".to_string(),
                emoji: Some("🦊".to_string()),
                width: 128,
                height: 128,
                mime_type: "image/png".to_string(),
            },
        ];

        let animal_package = StickerPackage {
            package_id: "animals".to_string(),
            name: "可爱动物".to_string(),
            thumbnail_url: "https://cdn.example.com/stickers/animals/cover.png".to_string(),
            author: "PrivChat".to_string(),
            description: "萌萌的动物表情包，让聊天更有趣".to_string(),
            sticker_count: animal_stickers.len(),
            stickers: Some(animal_stickers.clone()),
        };

        self.packages.insert("animals".to_string(), animal_package);

        for sticker in animal_stickers {
            self.stickers.insert(sticker.sticker_id.clone(), sticker);
        }

        // 表情包库 3: 食物表情
        let food_stickers = vec![
            Sticker {
                sticker_id: "food_001".to_string(),
                package_id: "food".to_string(),
                image_url: "https://cdn.example.com/stickers/food/pizza.png".to_string(),
                alt_text: "披萨".to_string(),
                emoji: Some("🍕".to_string()),
                width: 128,
                height: 128,
                mime_type: "image/png".to_string(),
            },
            Sticker {
                sticker_id: "food_002".to_string(),
                package_id: "food".to_string(),
                image_url: "https://cdn.example.com/stickers/food/burger.png".to_string(),
                alt_text: "汉堡".to_string(),
                emoji: Some("🍔".to_string()),
                width: 128,
                height: 128,
                mime_type: "image/png".to_string(),
            },
            Sticker {
                sticker_id: "food_003".to_string(),
                package_id: "food".to_string(),
                image_url: "https://cdn.example.com/stickers/food/sushi.png".to_string(),
                alt_text: "寿司".to_string(),
                emoji: Some("🍣".to_string()),
                width: 128,
                height: 128,
                mime_type: "image/png".to_string(),
            },
            Sticker {
                sticker_id: "food_004".to_string(),
                package_id: "food".to_string(),
                image_url: "https://cdn.example.com/stickers/food/icecream.png".to_string(),
                alt_text: "冰淇淋".to_string(),
                emoji: Some("🍦".to_string()),
                width: 128,
                height: 128,
                mime_type: "image/png".to_string(),
            },
            Sticker {
                sticker_id: "food_005".to_string(),
                package_id: "food".to_string(),
                image_url: "https://cdn.example.com/stickers/food/cake.png".to_string(),
                alt_text: "蛋糕".to_string(),
                emoji: Some("🍰".to_string()),
                width: 128,
                height: 128,
                mime_type: "image/png".to_string(),
            },
            Sticker {
                sticker_id: "food_006".to_string(),
                package_id: "food".to_string(),
                image_url: "https://cdn.example.com/stickers/food/coffee.png".to_string(),
                alt_text: "咖啡".to_string(),
                emoji: Some("☕".to_string()),
                width: 128,
                height: 128,
                mime_type: "image/png".to_string(),
            },
        ];

        let food_package = StickerPackage {
            package_id: "food".to_string(),
            name: "美食世界".to_string(),
            thumbnail_url: "https://cdn.example.com/stickers/food/cover.png".to_string(),
            author: "PrivChat".to_string(),
            description: "各种美食表情，吃货必备".to_string(),
            sticker_count: food_stickers.len(),
            stickers: Some(food_stickers.clone()),
        };

        self.packages.insert("food".to_string(), food_package);

        for sticker in food_stickers {
            self.stickers.insert(sticker.sticker_id.clone(), sticker);
        }
    }

    /// 获取所有表情包库列表（不包含表情包详情）
    pub async fn list_packages(&self) -> Vec<StickerPackage> {
        self.packages
            .values()
            .map(|pkg| {
                let mut pkg_copy = pkg.clone();
                pkg_copy.stickers = None; // 列表中不返回具体表情包
                pkg_copy
            })
            .collect()
    }

    /// 获取表情包库详情（包含所有表情包）
    pub async fn get_package_detail(&self, package_id: &str) -> Option<StickerPackage> {
        self.packages.get(package_id).cloned()
    }

    /// 获取单个表情包详情
    pub async fn get_sticker(&self, sticker_id: &str) -> Option<Sticker> {
        self.stickers.get(sticker_id).cloned()
    }

    /// 获取表情包库中的所有表情包
    pub async fn list_stickers_by_package(&self, package_id: &str) -> Vec<Sticker> {
        self.stickers
            .values()
            .filter(|s| s.package_id == package_id)
            .cloned()
            .collect()
    }
}

impl Default for StickerService {
    fn default() -> Self {
        Self::new()
    }
}
