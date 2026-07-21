//! 内置常用影院预设（与 py/.../presets.py 1:1）。

use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct Preset {
    pub id: &'static str,
    pub name: &'static str,
    pub city: &'static str,
    pub note: &'static str,
}

/// 与 Python `PRESETS` dict 同序（Py 3.7+ dict 保序）。
pub static PRESETS: &[(&str, Preset)] = &[
    (
        "前滩太古里",
        Preset {
            id: "37534",
            name: "MOViE MOViE 影城（前滩太古里店）",
            city: "上海",
            note: "艺术影院，挂幕类活动常有",
        },
    ),
    (
        "上海大光明",
        Preset {
            id: "2127",
            name: "大光明影院（南京西路）",
            city: "上海",
            note: "首映礼常驻地",
        },
    ),
    (
        "上海影城",
        Preset {
            id: "2120",
            name: "上海影城（新华路）",
            city: "上海",
            note: "上海电影节主场地",
        },
    ),
    (
        "北京万达CBD",
        Preset {
            id: "7579",
            name: "万达影城（北京CBD店）",
            city: "北京",
            note: "热门商圈",
        },
    ),
    (
        "深圳万象天地",
        Preset {
            id: "11823",
            name: "万象影城（深圳万象天地店）",
            city: "深圳",
            note: "热门商圈",
        },
    ),
];

pub fn list_presets() -> &'static [(&'static str, Preset)] {
    PRESETS
}

pub fn get_preset(name: &str) -> Option<&'static Preset> {
    PRESETS
        .iter()
        .find(|(k, _)| *k == name)
        .map(|(_, v)| v)
}
