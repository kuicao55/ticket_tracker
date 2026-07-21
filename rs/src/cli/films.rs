//! `tt films [1|2|3]`：拉猫眼 films 列表。

use anyhow::Result;

use crate::maoyan;

pub fn run(show_type: u8) -> Result<()> {
    let label = match show_type {
        1 => "热映",
        2 => "即将上映",
        3 => "经典",
        _ => "未知",
    };
    println!("→ 拉取「{}」列表…", label);
    let rt = tokio::runtime::Runtime::new()?;
    let pairs = rt.block_on(maoyan::fetch_films_list_async(show_type))?;
    println!("{:<10} {}", "ID", "NAME");
    for (id, name) in pairs {
        println!("{:<10} {}", id, name);
    }
    Ok(())
}
