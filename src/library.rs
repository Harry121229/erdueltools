//! 无限构筑存档库、快捷键绑定与旧三槽存档迁移。

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::i18n::{self, Lang};
use crate::paths;
use crate::snap::BuildSnapshot;

const INDEX_FILE: &str = "library.json";
const CONFIG_FILE: &str = "config.json";
const LEGACY_SLOTS: usize = 3;
const BINDING_SLOTS: usize = 4;

static STATE: OnceLock<Mutex<State>> = OnceLock::new();
static NEXT_ID: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BuildEntry {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub preview: String,
    /// 武器 6 格、护甲 4 格、护符 4 格对应的游戏知识图标 ID。
    #[serde(default)]
    pub icon_ids: Vec<Option<u16>>,
    #[serde(default)]
    pub stats: Option<StatsPreview>,
    #[serde(default)]
    pub combat: CombatRecord,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct CombatRecord {
    pub kills: u32,
    pub deaths: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StatsPreview {
    pub level: u32,
    pub vigor: u32,
    pub mind: u32,
    pub endurance: u32,
    pub strength: u32,
    pub dexterity: u32,
    pub intelligence: u32,
    pub faith: u32,
    pub arcane: u32,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct LibraryIndex {
    #[serde(default)]
    builds: Vec<BuildEntry>,
}

#[derive(Debug, Serialize, Deserialize)]
struct Config {
    #[serde(default = "default_bindings")]
    bindings: Vec<Option<String>>,
    #[serde(default)]
    care_enabled: bool,
    #[serde(default)]
    legacy_migrated: bool,
    #[serde(default = "default_language")]
    language: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            bindings: default_bindings(),
            care_enabled: false,
            legacy_migrated: false,
            language: default_language(),
        }
    }
}

fn default_language() -> String {
    Lang::En.code().to_owned()
}

#[derive(Debug, Default)]
struct State {
    initialized: bool,
    active_build: Option<String>,
    index: LibraryIndex,
    config: Config,
}

fn default_bindings() -> Vec<Option<String>> {
    vec![None; BINDING_SLOTS]
}

fn state() -> &'static Mutex<State> {
    STATE.get_or_init(|| Mutex::new(State::default()))
}

fn lock_state() -> Result<std::sync::MutexGuard<'static, State>, String> {
    state().lock().map_err(|_| "构筑存档库锁已损坏".to_owned())
}

fn builds_dir() -> PathBuf {
    paths::data_dir().join("builds")
}

fn index_path() -> PathBuf {
    builds_dir().join(INDEX_FILE)
}

fn config_path() -> PathBuf {
    builds_dir().join(CONFIG_FILE)
}

fn snapshot_path(id: &str) -> PathBuf {
    builds_dir().join(format!("{id}.json"))
}

fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<T, String> {
    let text =
        fs::read_to_string(path).map_err(|e| format!("读取 {} 失败：{e}", path.display()))?;
    serde_json::from_str(&text).map_err(|e| format!("解析 {} 失败：{e}", path.display()))
}

fn atomic_write_json<T: Serialize>(path: &Path, value: &T) -> Result<(), String> {
    let bytes = serde_json::to_vec_pretty(value)
        .map_err(|e| format!("序列化 {} 失败：{e}", path.display()))?;
    let nonce = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let temp = path.with_extension(format!("tmp-{}-{nonce}", std::process::id()));

    fs::write(&temp, bytes).map_err(|e| format!("写入 {} 失败：{e}", temp.display()))?;
    if let Err(first) = fs::rename(&temp, path) {
        // Windows 的 rename 不覆盖现有文件；先移除目标再重试。
        if path.exists() {
            fs::remove_file(path).map_err(|e| format!("替换 {} 失败：{e}", path.display()))?;
            fs::rename(&temp, path).map_err(|e| format!("替换 {} 失败：{e}", path.display()))?;
        } else {
            let _ = fs::remove_file(&temp);
            return Err(format!("写入 {} 失败：{first}", path.display()));
        }
    }
    Ok(())
}

fn persist_index(index: &LibraryIndex) -> Result<(), String> {
    atomic_write_json(&index_path(), index)
}

fn persist_config(config: &Config) -> Result<(), String> {
    atomic_write_json(&config_path(), config)
}

fn legacy_id(slot: usize) -> String {
    format!("legacy-bd{}", slot + 1)
}

fn migrate_legacy(state: &mut State) -> Result<(), String> {
    let mut imported = Vec::new();
    for slot in 0..LEGACY_SLOTS {
        let old_path = paths::data_dir().join(format!("bd{}.json", slot + 1));
        if !old_path.exists() {
            continue;
        }

        let id = legacy_id(slot);
        let destination = snapshot_path(&id);
        if !destination.exists() {
            let snapshot: BuildSnapshot = read_json(&old_path)?;
            atomic_write_json(&destination, &snapshot)?;
        }

        if !state.index.builds.iter().any(|entry| entry.id == id) {
            let snapshot: BuildSnapshot = read_json(&destination)?;
            state.index.builds.push(BuildEntry {
                id: id.clone(),
                name: i18n::fmt(i18n::t().legacy_build, [slot + 1]),
                preview: snapshot_preview(&snapshot),
                icon_ids: Vec::new(),
                stats: stats_preview(&snapshot),
                combat: CombatRecord::default(),
            });
        }
        imported.push((slot, id));
    }

    // 先持久化索引，再标记迁移完成；任一步中断后都可安全重跑。
    persist_index(&state.index)?;
    for (slot, id) in imported {
        if state.config.bindings.len() <= slot {
            state.config.bindings.resize(slot + 1, None);
        }
        if state.config.bindings[slot].is_none() {
            state.config.bindings[slot] = Some(id);
        }
    }
    state.config.legacy_migrated = true;
    persist_config(&state.config)
}

/// 初始化存档库，并幂等迁移 `bd1.json`、`bd2.json`、`bd3.json`。
pub fn init() -> Result<(), String> {
    let mut state = lock_state()?;
    if state.initialized {
        return Ok(());
    }

    fs::create_dir_all(builds_dir()).map_err(|e| format!("创建构筑存档目录失败：{e}"))?;

    state.index = if index_path().exists() {
        read_json(&index_path())?
    } else {
        LibraryIndex::default()
    };
    state.config = if config_path().exists() {
        read_json(&config_path())?
    } else {
        Config::default()
    };
    if state.config.bindings.len() < BINDING_SLOTS {
        state.config.bindings.resize(BINDING_SLOTS, None);
    }
    let lang = Lang::from_code(&state.config.language).unwrap_or(Lang::En);
    state.config.language = lang.code().to_owned();
    i18n::set_language(lang);

    migrate_legacy(&mut state)?;
    let mut preview_changed = false;
    for entry in &mut state.index.builds {
        if let Ok(snapshot) = read_json::<BuildSnapshot>(&snapshot_path(&entry.id)) {
            let preview = snapshot_preview(&snapshot);
            if entry.preview != preview {
                entry.preview = preview;
                preview_changed = true;
            }
            let stats = stats_preview(&snapshot);
            if entry.stats != stats {
                entry.stats = stats;
                preview_changed = true;
            }
        }
    }
    if preview_changed {
        persist_index(&state.index)?;
    }
    state.initialized = true;
    Ok(())
}

fn ready_state() -> Result<std::sync::MutexGuard<'static, State>, String> {
    init()?;
    lock_state()
}

/// 按索引顺序列出全部构筑。
pub fn list() -> Result<Vec<BuildEntry>, String> {
    Ok(ready_state()?.index.builds.clone())
}

/// 保存一个新构筑，名称为创建时间，ID 保证在当前存档库中唯一。
pub fn create(snapshot: &BuildSnapshot) -> Result<BuildEntry, String> {
    let mut state = ready_state()?;
    let entry = loop {
        let (id, name) = generated_identity();
        if !state.index.builds.iter().any(|entry| entry.id == id) && !snapshot_path(&id).exists() {
            break BuildEntry {
                id,
                name,
                preview: snapshot_preview(snapshot),
                icon_ids: Vec::new(),
                stats: stats_preview(snapshot),
                combat: CombatRecord::default(),
            };
        }
    };

    let path = snapshot_path(&entry.id);
    atomic_write_json(&path, snapshot)?;
    state.index.builds.push(entry.clone());
    if let Err(error) = persist_index(&state.index) {
        state.index.builds.pop();
        let _ = fs::remove_file(path);
        return Err(error);
    }
    state.active_build = Some(entry.id.clone());
    Ok(entry)
}

/// 按 ID 读取构筑快照。
pub fn load(id: &str) -> Result<BuildSnapshot, String> {
    let state = ready_state()?;
    if !state.index.builds.iter().any(|entry| entry.id == id) {
        return Err(i18n::fmt(i18n::t().build_not_found, [id]));
    }
    read_json(&snapshot_path(id))
}

/// 覆盖指定存档内容，保留 ID、名称和快捷键绑定。
pub fn overwrite(id: &str, snapshot: &BuildSnapshot) -> Result<BuildEntry, String> {
    let mut state = ready_state()?;
    let Some(position) = state.index.builds.iter().position(|entry| entry.id == id) else {
        return Err(i18n::fmt(i18n::t().build_not_found, [id]));
    };
    let old_snapshot = read_json::<BuildSnapshot>(&snapshot_path(id))?;
    let old_entry = state.index.builds[position].clone();
    let path = snapshot_path(id);

    atomic_write_json(&path, snapshot)?;
    state.index.builds[position].preview = snapshot_preview(snapshot);
    state.index.builds[position].icon_ids.clear();
    state.index.builds[position].stats = stats_preview(snapshot);
    if let Err(error) = persist_index(&state.index) {
        state.index.builds[position] = old_entry;
        let _ = atomic_write_json(&path, &old_snapshot);
        return Err(error);
    }
    state.active_build = Some(id.to_owned());
    Ok(state.index.builds[position].clone())
}

/// 当前存档是最后创建、加载或覆盖的存档。
pub fn active() -> Option<String> {
    init().ok()?;
    let state = lock_state().ok()?;
    let id = state.active_build.as_ref()?;
    state
        .index
        .builds
        .iter()
        .any(|entry| &entry.id == id)
        .then(|| id.clone())
}

pub fn active_combat() -> Option<(String, CombatRecord)> {
    init().ok()?;
    let state = lock_state().ok()?;
    let id = state.active_build.as_ref()?;
    let entry = state.index.builds.iter().find(|entry| &entry.id == id)?;
    Some((entry.name.clone(), entry.combat))
}

/// 将面板选中项设为 F7 的覆盖目标。
pub fn set_active(id: &str) -> Result<(), String> {
    let mut state = ready_state()?;
    if !state.index.builds.iter().any(|entry| entry.id == id) {
        return Err(i18n::fmt(i18n::t().build_not_found, [id]));
    }
    if state.active_build.as_deref() == Some(id) {
        return Ok(());
    }
    state.active_build = Some(id.to_owned());
    Ok(())
}

/// 给当前选中存档累计战绩；未选中时按规则忽略。
pub fn add_active_combat(kills: u32, deaths: u32) -> Result<Option<CombatRecord>, String> {
    if kills == 0 && deaths == 0 {
        return Ok(None);
    }
    let mut state = ready_state()?;
    let Some(id) = state.active_build.clone() else {
        return Ok(None);
    };
    let Some(position) = state.index.builds.iter().position(|entry| entry.id == id) else {
        state.active_build = None;
        return Ok(None);
    };
    let old = state.index.builds[position].combat;
    state.index.builds[position].combat.kills = state.index.builds[position]
        .combat
        .kills
        .saturating_add(kills);
    state.index.builds[position].combat.deaths = state.index.builds[position]
        .combat
        .deaths
        .saturating_add(deaths);
    if let Err(error) = persist_index(&state.index) {
        state.index.builds[position].combat = old;
        return Err(error);
    }
    Ok(Some(state.index.builds[position].combat))
}

pub fn clear_combat(id: &str) -> Result<(), String> {
    let mut state = ready_state()?;
    let Some(position) = state.index.builds.iter().position(|entry| entry.id == id) else {
        return Err(i18n::fmt(i18n::t().build_not_found, [id]));
    };
    let old = state.index.builds[position].combat;
    state.index.builds[position].combat = CombatRecord::default();
    if let Err(error) = persist_index(&state.index) {
        state.index.builds[position].combat = old;
        return Err(error);
    }
    Ok(())
}

/// 删除构筑，并清除所有指向它的快捷键绑定。
pub fn delete(id: &str) -> Result<(), String> {
    let mut state = ready_state()?;
    let Some(position) = state.index.builds.iter().position(|entry| entry.id == id) else {
        return Err(i18n::fmt(i18n::t().build_not_found, [id]));
    };

    let old_entry = state.index.builds.remove(position);
    let old_bindings = state.config.bindings.clone();
    let old_active = state.active_build.clone();
    for binding in &mut state.config.bindings {
        if binding.as_deref() == Some(id) {
            *binding = None;
        }
    }
    if state.active_build.as_deref() == Some(id) {
        state.active_build = None;
    }

    if let Err(error) = persist_index(&state.index).and_then(|_| persist_config(&state.config)) {
        state.index.builds.insert(position, old_entry);
        state.config.bindings = old_bindings;
        state.active_build = old_active;
        return Err(error);
    }

    let path = snapshot_path(id);
    if path.exists() {
        fs::remove_file(&path).map_err(|e| format!("删除 {} 失败：{e}", path.display()))?;
    }
    if let Some(slot) = id
        .strip_prefix("legacy-bd")
        .and_then(|value| value.parse::<usize>().ok())
    {
        let legacy = paths::data_dir().join(format!("bd{slot}.json"));
        if legacy.exists() {
            fs::remove_file(&legacy)
                .map_err(|e| format!("删除旧存档 {} 失败：{e}", legacy.display()))?;
        }
    }
    Ok(())
}

/// 修改构筑显示名称。
pub fn rename(id: &str, name: &str) -> Result<(), String> {
    let name = name.trim();
    if name.is_empty() {
        return Err(i18n::t().build_name_empty.to_owned());
    }

    let mut state = ready_state()?;
    let Some(position) = state.index.builds.iter().position(|entry| entry.id == id) else {
        return Err(i18n::fmt(i18n::t().build_not_found, [id]));
    };
    let old_name = std::mem::replace(&mut state.index.builds[position].name, name.to_owned());
    if let Err(error) = persist_index(&state.index) {
        state.index.builds[position].name = old_name;
        return Err(error);
    }
    Ok(())
}

/// 返回快捷键索引当前绑定的构筑 ID；未初始化或未绑定时返回 `None`。
pub fn binding(index: usize) -> Option<String> {
    init().ok()?;
    lock_state()
        .ok()?
        .config
        .bindings
        .get(index)
        .cloned()
        .flatten()
}

/// 设置或清除快捷键绑定；绑定目标必须是已存在的构筑。
pub fn bind(index: usize, id: Option<&str>) -> Result<(), String> {
    let mut state = ready_state()?;
    if let Some(id) = id {
        if !state.index.builds.iter().any(|entry| entry.id == id) {
            return Err(i18n::fmt(i18n::t().build_not_found, [id]));
        }
    }
    if state.config.bindings.len() <= index {
        state.config.bindings.resize(index + 1, None);
    }

    let old = state.config.bindings[index].clone();
    state.config.bindings[index] = id.map(str::to_owned);
    if let Err(error) = persist_config(&state.config) {
        state.config.bindings[index] = old;
        return Err(error);
    }
    Ok(())
}

/// 返回护理开关；未初始化或配置读取失败时安全地返回默认值 `false`。
pub fn care_enabled() -> bool {
    if init().is_err() {
        return false;
    }
    lock_state()
        .map(|state| state.config.care_enabled)
        .unwrap_or(false)
}

/// 持久化护理开关。
pub fn set_care_enabled(enabled: bool) -> Result<(), String> {
    let mut state = ready_state()?;
    let old = state.config.care_enabled;
    state.config.care_enabled = enabled;
    if let Err(error) = persist_config(&state.config) {
        state.config.care_enabled = old;
        return Err(error);
    }
    Ok(())
}

/// 当前界面语言。
pub fn language() -> Lang {
    if init().is_err() {
        return i18n::language();
    }
    lock_state()
        .ok()
        .and_then(|state| Lang::from_code(&state.config.language))
        .unwrap_or_else(i18n::language)
}

/// 设置并持久化界面语言。
pub fn set_language(lang: Lang) -> Result<(), String> {
    let mut state = ready_state()?;
    let old = state.config.language.clone();
    state.config.language = lang.code().to_owned();
    if let Err(error) = persist_config(&state.config) {
        state.config.language = old;
        return Err(error);
    }
    i18n::set_language(lang);
    Ok(())
}

/// 按 zh → en → ko → ja → fr 循环切换语言并持久化。
pub fn cycle_language() -> Result<Lang, String> {
    let next = language().next();
    set_language(next)?;
    Ok(next)
}

fn generated_identity() -> (String, String) {
    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let sequence = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let id = format!(
        "build-{:x}-{:x}-{:x}",
        duration.as_nanos(),
        std::process::id(),
        sequence
    );
    let (year, month, day, hour, minute, second) = utc_parts(duration.as_secs());
    let name = format!(
        "{} {year:04}-{month:02}-{day:02} {hour:02}:{minute:02}:{second:02} UTC",
        i18n::t().build_name_prefix
    );
    (id, name)
}

fn snapshot_preview(snapshot: &BuildSnapshot) -> String {
    let ids = |slots: std::ops::RangeInclusive<u8>| {
        snapshot
            .loadout
            .iter()
            .filter(|slot| slots.contains(&slot.slot) && slot.id != u32::MAX)
            .map(|slot| (slot.id & 0x0fff_ffff).to_string())
            .collect::<Vec<_>>()
            .join("/")
    };
    let stats = snapshot.stats.as_ref().map(|stats| {
        format!(
            "Lv{} 生{} 集{} 耐{} 力{} 敏{} 智{} 信{} 感{}",
            stats.level,
            stats.vigor,
            stats.mind,
            stats.endurance,
            stats.strength,
            stats.dexterity,
            stats.intelligence,
            stats.faith,
            stats.arcane
        )
    });
    let weapons = ids(0..=5);
    let armor = ids(12..=15);
    let talismans = ids(17..=20);
    let magic = snapshot.magic.iter().filter(|id| **id >= 0).count();
    format!(
        "{}  武:{}  甲:{}  符:{}  法:{magic}",
        stats.as_deref().unwrap_or("无加点"),
        if weapons.is_empty() { "-" } else { &weapons },
        if armor.is_empty() { "-" } else { &armor },
        if talismans.is_empty() {
            "-"
        } else {
            &talismans
        },
    )
}

fn stats_preview(snapshot: &BuildSnapshot) -> Option<StatsPreview> {
    snapshot.stats.as_ref().map(|stats| StatsPreview {
        level: stats.level,
        vigor: stats.vigor,
        mind: stats.mind,
        endurance: stats.endurance,
        strength: stats.strength,
        dexterity: stats.dexterity,
        intelligence: stats.intelligence,
        faith: stats.faith,
        arcane: stats.arcane,
    })
}

fn utc_parts(seconds: u64) -> (i64, u32, u32, u32, u32, u32) {
    let days = (seconds / 86_400) as i64;
    let seconds_of_day = seconds % 86_400;
    let (year, month, day) = civil_from_days(days);
    (
        year,
        month,
        day,
        (seconds_of_day / 3_600) as u32,
        ((seconds_of_day % 3_600) / 60) as u32,
        (seconds_of_day % 60) as u32,
    )
}

// Howard Hinnant 的公历换算算法；输入为 Unix epoch 起算的天数。
fn civil_from_days(days_since_epoch: i64) -> (i64, u32, u32) {
    let z = days_since_epoch + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let day_of_era = z - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let mut year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = month_prime + if month_prime < 10 { 3 } else { -9 };
    year += i64::from(month <= 2);
    (year, month as u32, day as u32)
}
