use std::{cmp::Reverse, collections::HashMap, fmt::Display, num::NonZero, sync::Arc};

use egui::ThemePreference;
use ironworks::excel::Language;
use serde::{Deserialize, Serialize, de::DeserializeOwned};

use crate::{
    github::GithubAuth,
    sheet::{FilterInputType, MatchOptions},
    utils::{CodeTheme, ColorTheme, GameVersion},
};

pub trait Keyable: Serialize + DeserializeOwned + Clone + Send + Sync + 'static {}

impl<K: Serialize + DeserializeOwned + Clone + Send + Sync + 'static> Keyable for K {}

#[derive(Debug, Clone, Copy)]
enum RetrievalMethod {
    Persisted,
    Temporary,
}

impl RetrievalMethod {
    pub fn try_get<K: Keyable>(self, ctx: &egui::Context, id: egui::Id) -> Option<K> {
        match self {
            RetrievalMethod::Persisted => ctx.data_mut(|d| d.get_persisted::<_>(id)),
            RetrievalMethod::Temporary => ctx.data(|d| d.get_temp::<_>(id)),
        }
    }

    pub fn get_or_insert<K: Keyable>(
        self,
        ctx: &egui::Context,
        id: egui::Id,
        func: impl FnOnce() -> K,
    ) -> K {
        match self {
            RetrievalMethod::Persisted => {
                ctx.data_mut(|d| d.get_persisted_mut_or_insert_with(id, func).clone())
            }
            RetrievalMethod::Temporary => {
                ctx.data_mut(|d| d.get_temp_mut_or_insert_with(id, func).clone())
            }
        }
    }

    pub fn remove<K: Keyable>(self, ctx: &egui::Context, id: egui::Id) {
        ctx.data_mut(|d| d.remove::<K>(id));
    }

    pub fn take<K: Keyable>(self, ctx: &egui::Context, id: egui::Id) -> Option<K> {
        self.try_get(ctx, id).inspect(|_| self.remove::<K>(ctx, id))
    }

    pub fn set<K: Keyable>(self, ctx: &egui::Context, id: egui::Id, value: K) {
        match self {
            RetrievalMethod::Persisted => ctx.data_mut(|d| d.insert_persisted(id, value)),
            RetrievalMethod::Temporary => ctx.data_mut(|d| {
                d.insert_temp(id, value);
            }),
        }
    }

    pub fn use_with<K: Keyable, T>(
        self,
        ctx: &egui::Context,
        id: egui::Id,
        insert_with: impl FnOnce() -> K,
        func: impl FnOnce(&mut K) -> T,
    ) -> T {
        match self {
            RetrievalMethod::Persisted => {
                ctx.data_mut(|d| func(d.get_persisted_mut_or_insert_with(id, insert_with)))
            }
            RetrievalMethod::Temporary => {
                ctx.data_mut(|d| func(d.get_temp_mut_or_insert_with(id, insert_with)))
            }
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct BaseKey<K: Keyable, const TEMP: bool> {
    id: &'static str,
    _marker: std::marker::PhantomData<K>,
}

impl<K: Keyable, const TEMP: bool> BaseKey<K, TEMP> {
    const fn new(name: &'static str) -> Self {
        Self {
            id: name,
            _marker: std::marker::PhantomData,
        }
    }

    pub fn try_get(&self, ctx: &egui::Context) -> Option<K> {
        Self::method().try_get(ctx, self.id.into())
    }

    pub fn get_or_insert(&self, ctx: &egui::Context, func: impl FnOnce() -> K) -> K {
        Self::method().get_or_insert(ctx, self.id.into(), func)
    }

    pub fn set(&self, ctx: &egui::Context, value: K) {
        Self::method().set(ctx, self.id.into(), value);
    }

    pub fn use_with<T>(
        &self,
        ctx: &egui::Context,
        insert_with: impl FnOnce() -> K,
        func: impl FnOnce(&mut K) -> T,
    ) -> T {
        Self::method().use_with(ctx, self.id.into(), insert_with, func)
    }

    pub fn take(&self, ctx: &egui::Context) -> Option<K> {
        Self::method().take(ctx, self.id.into())
    }

    pub fn remove(&self, ctx: &egui::Context) {
        Self::method().remove::<K>(ctx, self.id.into());
    }

    fn method() -> RetrievalMethod {
        if TEMP {
            RetrievalMethod::Temporary
        } else {
            RetrievalMethod::Persisted
        }
    }
}

pub struct FuncKey<K: Keyable, const TEMP: bool, P> {
    imp: BaseKey<K, TEMP>,
    preflight: fn(&egui::Context) -> P,
    insert_with: fn(&egui::Context, P) -> K,
}

impl<K: Keyable, const TEMP: bool> FuncKey<K, TEMP, ()> {
    pub const fn new(name: &'static str, insert_with: fn(&egui::Context, ()) -> K) -> Self {
        Self {
            imp: BaseKey::new(name),
            preflight: |_| (),
            insert_with,
        }
    }
}

impl<K: Keyable, const TEMP: bool, P> FuncKey<K, TEMP, P> {
    // Required to help prevent deadlocking when calling ctx.data() and similar methods.
    const fn new_with_preflight(
        name: &'static str,
        preflight: fn(&egui::Context) -> P,
        insert_with: fn(&egui::Context, P) -> K,
    ) -> Self {
        Self {
            imp: BaseKey::new(name),
            preflight,
            insert_with,
        }
    }

    pub fn try_get(&self, ctx: &egui::Context) -> Option<K> {
        self.imp.try_get(ctx)
    }

    pub fn get(&self, ctx: &egui::Context) -> K {
        let r = (self.preflight)(ctx);
        self.imp.get_or_insert(ctx, || (self.insert_with)(ctx, r))
    }

    pub fn set(&self, ctx: &egui::Context, value: K) {
        self.imp.set(ctx, value);
    }

    pub fn use_with<T>(&self, ctx: &egui::Context, func: impl FnOnce(&mut K) -> T) -> T {
        let r = (self.preflight)(ctx);
        self.imp.use_with(ctx, || (self.insert_with)(ctx, r), func)
    }
}

pub struct DefaultedKey<K: Keyable, const TEMP: bool> {
    imp: BaseKey<K, TEMP>,
    default: K,
}

impl<K: Keyable, const TEMP: bool> DefaultedKey<K, TEMP> {
    const fn new(name: &'static str, default: K) -> Self {
        Self {
            imp: BaseKey::new(name),
            default,
        }
    }

    pub fn try_get(&self, ctx: &egui::Context) -> Option<K> {
        self.imp.try_get(ctx)
    }

    pub fn get(&self, ctx: &egui::Context) -> K {
        self.imp.get_or_insert(ctx, || self.default.clone())
    }

    pub fn set(&self, ctx: &egui::Context, value: K) {
        self.imp.set(ctx, value);
    }

    pub fn use_with<T>(&self, ctx: &egui::Context, func: impl FnOnce(&mut K) -> T) -> T {
        self.imp.use_with(ctx, || self.default.clone(), func)
    }
}

/// Persist through a `String` so changes to `K` cannot change egui's persistence key.
pub struct StableDKey<K: Keyable> {
    id: &'static str,
    default: K,
}

impl<K: Keyable> StableDKey<K> {
    const fn new(name: &'static str, default: K) -> Self {
        Self { id: name, default }
    }

    pub fn try_get(&self, ctx: &egui::Context) -> Option<K> {
        ctx.data_mut(|data| {
            data.get_persisted::<String>(self.id.into())
                .and_then(|value| serde_json::from_str(&value).ok())
        })
    }

    pub fn get(&self, ctx: &egui::Context) -> K {
        self.try_get(ctx).unwrap_or_else(|| {
            let value = self.default.clone();
            self.set(ctx, value.clone());
            value
        })
    }

    pub fn set(&self, ctx: &egui::Context, value: K) {
        match serde_json::to_string(&value) {
            Ok(serialized) => ctx.data_mut(|data| {
                data.insert_persisted(self.id.into(), serialized);
            }),
            Err(error) => log::error!("无法保存设置 {}: {error}", self.id),
        }
    }

    pub fn use_with<T>(&self, ctx: &egui::Context, func: impl FnOnce(&mut K) -> T) -> T {
        let mut value = self.get(ctx);
        let result = func(&mut value);
        self.set(ctx, value);
        result
    }

    pub fn remove(&self, ctx: &egui::Context) {
        ctx.data_mut(|data| data.remove::<String>(self.id.into()));
    }
}

pub type Key<K> = BaseKey<K, false>;
pub type FKey<K, P = ()> = FuncKey<K, false, P>;
pub type DKey<K> = DefaultedKey<K, false>;

pub type TempKey<K> = BaseKey<K, true>;
pub type TempFKey<K, P = ()> = FuncKey<K, true, P>;
pub type TempDKey<K> = DefaultedKey<K, true>;

pub const LOGGER_SHOWN: DKey<bool> = DKey::new("logger-shown", false);
pub const SORTED_BY_OFFSET: DKey<bool> = DKey::new("sorted-by-offset", false);
pub const SOLID_SCROLLBAR: DKey<bool> = DKey::new("solid-scrollbar", true);
pub const ALWAYS_HIRES: DKey<bool> = DKey::new("always-hires", false);
pub const DISPLAY_FIELD_SHOWN: DKey<bool> = DKey::new("display-field-shown", true);
pub const EVALUATE_STRINGS: DKey<bool> = DKey::new("evaluate-strings", false);
pub const TEXT_WRAP_WIDTH: DKey<Option<NonZero<u16>>> =
    DKey::new("text-wrap-width", NonZero::new(600));
pub const TEXT_MAX_LINES: DKey<Option<NonZero<u8>>> = DKey::new("text-max-lines", NonZero::new(5));
pub const TEXT_USE_SCROLL: DKey<bool> = DKey::new("text-use-scroll", false);
pub const BACKEND_CONFIG: StableDKey<Option<BackendConfig>> =
    StableDKey::new("backend-config", None);
/// The signed-in GitHub account. Kept so a session survives a reload; a revoked token simply fails
/// its next call and is cleared by signing out.
pub const GITHUB_AUTH: DKey<Option<GithubAuth>> = DKey::new("github-auth", None);
pub const LANGUAGE: DKey<Language> = DKey::new("language", Language::ChineseSimplified);
pub const SHEETS_FILTER: DKey<String> = DKey::new("sheets-filter", String::new());
pub const SHEET_FILTERS: FKey<HashMap<String, (FilterInputType, String)>> =
    FKey::new("sheet-filters", |_, ()| HashMap::new());
pub const SHEET_FILTER_OPTIONS: DKey<MatchOptions> = DKey::new(
    "sheet-filter-options",
    MatchOptions {
        case_insensitive: true,
        use_display_field: true,
    },
);
/// Whether file names this install carries that the community path list does not know may be sent
/// on. `None` until the user has been asked; declining is kept so the ask happens once.
pub const REPORT_PATHS: DKey<Option<bool>> = DKey::new("report-paths", None);
pub const REPORT_WINDOW_SHOWN: DKey<bool> = DKey::new("report-window-shown", false);
pub const FILTER_GUIDE_VISIBLE: DKey<bool> = DKey::new("filter-guide-visible", false);
pub const SELECTED_SHEET: DKey<Option<String>> = DKey::new("selected-sheet", None);
pub const MISC_SHEETS_SHOWN: DKey<bool> = DKey::new("misc-sheets-shown", false);
pub const PR_CHANGED_ONLY: DKey<bool> = DKey::new("pr-changed-only", true);
pub const SCHEMA_EDITOR_VISIBLE: DKey<bool> = DKey::new("schema-editor-visible", false);
pub const SCHEMA_EDITOR_WORD_WRAP: DKey<bool> = DKey::new("schema-editor-word-wrap", false);
pub const SCHEMA_EDITOR_ERRORS_SHOWN: DKey<bool> = DKey::new("schema-editor-errors-shown", false);
/// An effect counts in frames without saying how fast they run, so the rate reading them as time is
/// the viewer's to pick. 30 matches the tick rate the same authoring pipeline's `.pap`/`.tmb`
/// timelines carry.
pub const AVFX_FRAME_RATE: DKey<f32> = DKey::new("avfx-frame-rate", 30.0);

pub const COLOR_THEME: FKey<ColorTheme, ThemePreference> = FKey::new_with_preflight(
    "color-theme",
    |ctx| ctx.options(|opt| opt.theme_preference),
    |_, preference| preference.into(),
);
pub const CODE_SYNTAX_THEME: FKey<CodeTheme, Arc<egui::Style>> = FKey::new_with_preflight(
    "syntax-theme",
    |ctx| ctx.global_style(),
    |_, style| CodeTheme {
        theme: if style.visuals.dark_mode {
            "base16-mocha.dark"
        } else {
            "Solarized (light)"
        }
        .to_owned(),
        font_id: egui::FontId::monospace(egui::TextStyle::Monospace.resolve(&style).size),
    },
);

pub const CURRENT_SHEET_LANGUAGES: TempKey<(String, Vec<Language>)> =
    TempKey::new("current-sheet-languages");
pub const TEMP_SCROLL_TO: TempKey<((u32, Option<u16>), u16)> = TempKey::new("temp-scroll-to");
pub const TEMP_HIGHLIGHTED_ROW: TempKey<(u32, Option<u16>)> = TempKey::new("temp-highlighted-row");

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum Region {
    Global,
    Korea,
    China,
    Taiwan,
}

impl Region {
    /// The API's key for this region. Replaces the old hardcoded repository slugs: the server
    /// resolves a region to its repositories, so the client no longer has to know their ids.
    pub fn api_name(&self) -> &'static str {
        match self {
            Region::Global => "global",
            Region::Korea => "korea",
            Region::China => "china",
            Region::Taiwan => "taiwan",
        }
    }

    pub fn name(&self) -> &'static str {
        match self {
            Region::Global => "国际服 / 日服",
            Region::Korea => "韩服",
            Region::China => "国服",
            Region::Taiwan => "台服",
        }
    }

    /// Whether the backend actually serves this region. Answered by `/api/regions/` rather than by
    /// a table baked into the binary, so a newly supported region needs no client release.
    pub fn is_available(&self, served: Option<&[String]>) -> bool {
        served.is_none_or(|regions| regions.iter().any(|r| r == self.api_name()))
    }
}

impl Display for Region {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.name())
    }
}

pub fn api_base(ctx: &egui::Context) -> String {
    BACKEND_CONFIG
        .get(ctx)
        .map(|config| config.api_url.trim_end_matches('/').to_string())
        .unwrap_or_default()
}

#[derive(Clone, Serialize, Deserialize, PartialEq)]
pub enum InstallLocation {
    #[cfg(not(target_arch = "wasm32"))]
    Sqpack(String),
    #[cfg(target_arch = "wasm32")]
    Worker(String),
    Web(Region, Option<GameVersion>),
}

#[derive(Clone, Serialize, Deserialize, PartialEq)]
pub struct GithubSchemaLocation {
    pub owner: String,
    pub repo: String,
    pub branch: GithubSchemaBranch,
}

impl GithubSchemaLocation {
    /// Which (owner, repo, branch) actually carries the schema files. A pull request's are on its
    /// head fork, which is a different repository from the one being merged into.
    pub fn source(&self) -> (&str, &str, String) {
        if let GithubSchemaBranch::PullRequest {
            full_name, branch, ..
        } = &self.branch
        {
            let (owner, repo) = full_name.split_once('/').unwrap_or((full_name, ""));
            return (owner, repo, branch.clone());
        }
        (&self.owner, &self.repo, self.base_branch())
    }

    pub fn base_url(&self) -> String {
        let (owner, repo, branch) = self.source();
        format!("https://raw.githubusercontent.com/{owner}/{repo}/refs/heads/{branch}")
    }

    pub fn base_branch(&self) -> String {
        match &self.branch {
            GithubSchemaBranch::Latest => "latest".to_string(),
            GithubSchemaBranch::Other(name) => name.clone(),
            GithubSchemaBranch::Version(v) => format!("ver/{}", v.0),
            GithubSchemaBranch::PullRequest { .. } => "latest".to_string(),
        }
    }
}

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub enum GithubSchemaBranch {
    Latest,
    PullRequest {
        number: u32,
        title: String,
        label: String,
        username: String,
        full_name: String,
        branch: String,
    },
    Other(String),
    Version(Reverse<GameVersion>),
}

impl Display for GithubSchemaBranch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GithubSchemaBranch::Latest => write!(f, "最新"),
            GithubSchemaBranch::Version(v) => v.0.fmt(f),
            GithubSchemaBranch::Other(name) => name.fmt(f),
            GithubSchemaBranch::PullRequest {
                number,
                title,
                label,
                ..
            } => write!(f, "PR #{number} - {title} ({label})"),
        }
    }
}

#[derive(Clone, Serialize, Deserialize, PartialEq)]
pub enum SchemaLocation {
    #[cfg(not(target_arch = "wasm32"))]
    Local(String),
    #[cfg(target_arch = "wasm32")]
    Worker(String),
    Github(GithubSchemaLocation),
    Web(String),
}

#[derive(Clone, Serialize, Deserialize)]
pub struct BackendConfig {
    #[serde(default = "default_api_url")]
    pub api_url: String,
    pub location: InstallLocation,
    pub schema: SchemaLocation,
}

fn default_api_url() -> String {
    crate::DEFAULT_API_URL.to_owned()
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename = "Memory")]
struct LegacyMemory {
    #[serde(default)]
    data: LegacyData,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct LegacyData(Vec<(u64, LegacyElement)>);

#[derive(Debug, Serialize, Deserialize)]
struct LegacyElement {
    type_id: LegacyTypeId,
    ron: String,
    generation: usize,
}

#[derive(Debug, Serialize, Deserialize)]
struct LegacyTypeId(u64);

#[derive(Serialize, Deserialize)]
#[serde(rename = "Memory")]
struct PersistedMemory {
    options: Box<ron::value::RawValue>,
    data: LegacyData,
    to_global: Box<ron::value::RawValue>,
    areas: Box<ron::value::RawValue>,
}

#[derive(Deserialize)]
struct LegacyBackendConfig {
    location: LegacyInstallLocation,
    schema: SchemaLocation,
}

#[derive(Deserialize)]
enum LegacyInstallLocation {
    #[cfg(not(target_arch = "wasm32"))]
    Sqpack(String),
    #[cfg(target_arch = "wasm32")]
    Worker(String),
    Web(String, Region, Option<GameVersion>),
}

impl LegacyBackendConfig {
    fn into_current(self) -> BackendConfig {
        let (api_url, location) = match self.location {
            #[cfg(not(target_arch = "wasm32"))]
            LegacyInstallLocation::Sqpack(path) => {
                (default_api_url(), InstallLocation::Sqpack(path))
            }
            #[cfg(target_arch = "wasm32")]
            LegacyInstallLocation::Worker(name) => {
                (default_api_url(), InstallLocation::Worker(name))
            }
            LegacyInstallLocation::Web(api_url, region, version) => {
                (api_url, InstallLocation::Web(region, version))
            }
        };
        BackendConfig {
            api_url,
            location,
            schema: self.schema,
        }
    }
}

/// Move configurations written with egui's type-dependent key to `StableDKey`.
pub fn migrate_backend_config(ctx: &egui::Context) {
    if BACKEND_CONFIG
        .try_get(ctx)
        .is_some_and(|config| config.is_some())
    {
        return;
    }

    let Some(memory) = ctx.memory(|memory| ron::to_string(memory).ok()) else {
        return;
    };
    let config = read_legacy_backend_config(&memory);

    if let Some(config) = config {
        if let Some(memory) = clean_legacy_backend_config(&memory)
            && let Ok(memory) = ron::from_str::<egui::Memory>(&memory)
        {
            ctx.memory_mut(|current| *current = memory);
        }
        BACKEND_CONFIG.set(ctx, Some(config));
    }
}

fn read_legacy_backend_config(memory: &str) -> Option<BackendConfig> {
    let memory = ron::from_str::<LegacyMemory>(memory).ok()?;
    let id = egui::Id::new("backend-config").value();
    memory.data.0.into_iter().find_map(|(raw, element)| {
        if raw ^ id != element.type_id.0 {
            return None;
        }
        if let Ok(Some(config)) = ron::from_str::<Option<BackendConfig>>(&element.ron) {
            return Some(config);
        }
        ron::from_str::<Option<LegacyBackendConfig>>(&element.ron)
            .ok()
            .flatten()
            .map(LegacyBackendConfig::into_current)
    })
}

fn clean_legacy_backend_config(memory: &str) -> Option<String> {
    let mut memory = ron::from_str::<PersistedMemory>(memory).ok()?;
    let id = egui::Id::new("backend-config").value();
    let before = memory.data.0.len();
    memory.data.0.retain(|(raw, element)| {
        let is_backend_key = raw ^ id == element.type_id.0;
        let is_legacy_value = ron::from_str::<Option<BackendConfig>>(&element.ron).is_ok()
            || ron::from_str::<Option<LegacyBackendConfig>>(&element.ron).is_ok();
        !(is_backend_key && is_legacy_value)
    });
    (memory.data.0.len() != before)
        .then(|| ron::to_string(&memory).ok())
        .flatten()
}
