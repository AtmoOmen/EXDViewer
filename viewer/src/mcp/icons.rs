//! 图标集合、集合内的图标编号, 以及单个图标的图片、纹理路径与使用位置。

use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    rc::Rc,
    time::Duration,
};

use anyhow::{Result, anyhow, bail};
use ironworks::excel::Language;

use crate::{
    backend::Backend,
    data::{
        IconIndex,
        listing::{Listed, Listing},
        get_icon_path,
    },
    github::GithubApi,
    icons::refs::{self, IconRefs},
    settings::{BackendConfig, SchemaLocation},
};

/// 一次最多返回多少个图标编号。
pub const MAX_PAGE: usize = 2000;
/// 等路径列表落地的间隔与上限。
const LISTING_POLL: Duration = Duration::from_millis(20);
const LISTING_TIMEOUT: Duration = Duration::from_secs(180);
/// 一个图标最多列多少处使用位置, 免得一个被上千张表引用的图标把响应撑爆。
pub const MAX_USES: usize = 200;

/// 集合名, 用于区分数据表集合与另外两类。
pub const OTHER: &str = "other";
pub const LOCALIZED: &str = "localized";
pub const ALL: &str = "all";

/// 一个集合的编号表, 以及它是哪一类集合。
type SetIds = (Rc<Vec<u32>>, &'static str);

thread_local! {
    static ICON_REFS: RefCell<Option<Rc<IconRefs>>> = const { RefCell::new(None) };
    /// 每个集合的编号表, 按集合名缓存: 反查一次要扫过全部使用记录, 而同一个集合常被连着查。
    static SET_IDS: RefCell<HashMap<String, SetIds>> = RefCell::new(HashMap::new());
    /// 全部、语言、其他三类的数量。索引与反查都建好之后就不会再变, 而三类以外每数一次都要
    /// 走一遍全部图标编号。
    static TOTALS: RefCell<Option<(usize, usize, usize)>> = const { RefCell::new(None) };
}

/// 这份安装里有哪些图标, 与界面共用同一个索引。
async fn icon_index(backend: &Backend, api_base: &str) -> Result<()> {
    if backend.icons().is_some() {
        return Ok(());
    }
    let listing = ensure_listing(backend, api_base).await?;
    backend.set_icons(IconIndex::build(listing.paths(), listing.presence()));
    Ok(())
}

async fn ensure_listing(backend: &Backend, api_base: &str) -> Result<Rc<Listing>> {
    let deadline = tokio::time::Instant::now() + LISTING_TIMEOUT;
    let api = api_base.trim_end_matches('/').to_owned();
    loop {
        match backend.listing(&api) {
            Listed::Ready(listing) => return Ok(listing),
            Listed::Failed(why) => bail!("{why}"),
            Listed::Loading => {
                if tokio::time::Instant::now() >= deadline {
                    bail!("等待路径列表超时");
                }
                tokio::time::sleep(LISTING_POLL).await;
            }
        }
    }
}

/// 每个图标被哪些行引用。整份走查只做一次, 之后一直复用。
pub async fn refs(backend: &Backend, config: &BackendConfig) -> Result<Rc<IconRefs>> {
    if let Some(held) = ICON_REFS.with(|held| held.borrow().clone()) {
        return Ok(held);
    }
    let bundle = match &config.schema {
        SchemaLocation::Github(location) => {
            Some((GithubApi::new(&config.api_url, None), location.clone()))
        }
        _ => None,
    };
    let progress = Rc::new(Cell::new(refs::Progress::default()));
    let walked = refs::walk(backend.clone(), bundle, progress).await?;
    let walked = Rc::new(walked);
    ICON_REFS.with(|held| held.replace(Some(walked.clone())));
    SET_IDS.with(|held| held.borrow_mut().clear());
    TOTALS.with(|held| held.replace(None));
    Ok(walked)
}

/// 图标集合的一行, 也就是 `list_icon_sets` 返回的一个条目。
pub struct Set {
    pub name: String,
    pub kind: &'static str,
    pub count: usize,
}

/// 列举全部图标集合。数据表集合按表名排序, 另外三类跟在后面。
pub async fn list_sets(
    backend: &Backend,
    config: &BackendConfig,
    query: Option<&str>,
) -> Result<Vec<Set>> {
    icon_index(backend, &config.api_url).await?;
    let refs = refs(backend, config).await?;
    let index = backend
        .icons()
        .ok_or_else(|| anyhow!("图标索引尚未建立"))?;
    let query = query.map(str::to_ascii_lowercase);

    let mut sets: Vec<Set> = refs
        .sheets()
        .filter(|(_, _, count)| *count > 0)
        .map(|(_, name, count)| Set {
            name: name.to_owned(),
            kind: "sheet",
            count: count as usize,
        })
        .collect();
    sets.sort_by(|left, right| left.name.cmp(&right.name));

    let (all, localized, unreferenced) = totals(index, &refs);
    sets.push(Set {
        name: ALL.to_owned(),
        kind: "all",
        count: all,
    });
    sets.push(Set {
        name: LOCALIZED.to_owned(),
        kind: "localized",
        count: localized,
    });
    sets.push(Set {
        name: OTHER.to_owned(),
        kind: "other",
        count: unreferenced,
    });

    if let Some(query) = query {
        sets.retain(|set| set.name.to_ascii_lowercase().contains(&query));
    }
    Ok(sets)
}

/// 一个集合里的图标编号, 升序。
pub async fn set_ids(
    backend: &Backend,
    config: &BackendConfig,
    set: &str,
) -> Result<(Rc<Vec<u32>>, &'static str)> {
    if let Some(cached) = SET_IDS.with(|held| held.borrow().get(set).cloned()) {
        return Ok(cached);
    }
    icon_index(backend, &config.api_url).await?;
    let refs = refs(backend, config).await?;
    let index = backend
        .icons()
        .ok_or_else(|| anyhow!("图标索引尚未建立"))?;

    let (ids, kind): (Vec<u32>, &'static str) = if set.eq_ignore_ascii_case(LOCALIZED) {
        (
            index.ids().filter(|id| index.localized(*id)).collect(),
            "localized",
        )
    } else if set.eq_ignore_ascii_case(OTHER) {
        (
            index.ids().filter(|id| !refs.is_referenced(*id)).collect(),
            "other",
        )
    } else if set.eq_ignore_ascii_case(ALL) {
        (index.ids().collect(), "all")
    } else {
        match refs.sheets().find(|(_, name, _)| *name == set) {
            Some((sheet, _, _)) => (refs.icons_of(sheet), "sheet"),
            None => bail!("没有名为 {set} 的图标集合。先用 list_icon_sets 查看可用的集合"),
        }
    };

    let ids = Rc::new(ids);
    SET_IDS.with(|held| {
        held.borrow_mut()
            .insert(set.to_owned(), (ids.clone(), kind));
    });
    Ok((ids, kind))
}

/// 全部、语言、其他三类的数量, 建好一次就一直用。
fn totals(index: &IconIndex, refs: &IconRefs) -> (usize, usize, usize) {
    if let Some(held) = TOTALS.with(|held| *held.borrow()) {
        return held;
    }
    let mut all = 0usize;
    let mut localized = 0usize;
    let mut unreferenced = 0usize;
    for id in index.ids() {
        all += 1;
        if index.localized(id) {
            localized += 1;
        }
        if !refs.is_referenced(id) {
            unreferenced += 1;
        }
    }
    let counted = (all, localized, unreferenced);
    TOTALS.with(|held| held.replace(Some(counted)));
    counted
}

/// 单个图标的纹理路径与它是否另带语言文件。
pub fn paths(
    backend: &Backend,
    icon_id: u32,
    language: Language,
) -> (String, String, bool, bool) {
    let index = backend.icons();
    (
        get_icon_path(index, icon_id, false, language),
        get_icon_path(index, icon_id, true, language),
        index.is_some_and(|index| index.localized(icon_id)),
        index.is_some_and(|index| index.hires(icon_id)),
    )
}
