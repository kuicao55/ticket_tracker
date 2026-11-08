//! 猫眼城市 ID 列表。
//!
//! 与 `rs/web/js/app.js` 里的影院搜索城市保持一致；服务端渲染全城 watch
//! 城市下拉框时也使用这里的数据。

/// (猫眼城市 ID, 城市名)，顺序用于面板下拉框。
pub static CITIES: &[(u32, &str)] = &[
    (10, "上海"),
    (1, "北京"),
    (20, "广州"),
    (30, "深圳"),
    (50, "杭州"),
    (59, "成都"),
    (55, "南京"),
    (57, "武汉"),
    (42, "西安"),
    (45, "重庆"),
    (80, "苏州"),
    (40, "天津"),
    (70, "长沙"),
    (73, "郑州"),
    (60, "青岛"),
    (62, "厦门"),
    (44, "福州"),
    (96, "济南"),
    (114, "昆明"),
    (56, "合肥"),
    (51, "宁波"),
    (91, "东莞"),
    (92, "佛山"),
    (52, "无锡"),
];

pub fn name_for(id: u32) -> Option<&'static str> {
    CITIES.iter().find(|(cid, _)| *cid == id).map(|(_, n)| *n)
}

/// 面板/事件展示用名称；未知城市回退成 `城市 <id>`。
pub fn resolve_name(id: u32) -> String {
    name_for(id)
        .map(str::to_string)
        .unwrap_or_else(|| format!("城市 {}", id))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_city_names_resolve() {
        assert_eq!(name_for(10), Some("上海"));
        assert_eq!(resolve_name(1), "北京");
        assert_eq!(resolve_name(9999), "城市 9999");
    }
}
