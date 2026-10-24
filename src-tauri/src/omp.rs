//! oh-my-pi（omp / omp-zh）联动。
//!
//! omp 从自己的 agent 目录加载扩展：`<agent>/extensions/<名字>/index.ts`。开启时把
//! `omp/clawd-pet.ts`（编进本程序）写成 `<agent>/extensions/clawd-pet/index.ts`，再在同目录写
//! `config.json`（桥接端口和令牌），并放一个标记文件，关闭时只删带标记的目录，不碰用户自己的扩展。
//!
//! agent 目录按 omp 自己的规则解析（和 omp 一致，不然写进一个 omp 永远不读的目录还显示“已开启”）：
//! - 配置根是 `~/<PI_CONFIG_DIR 或 .omp>`；
//! - 有命名 profile（`OMP_PROFILE`，没设才看 `PI_PROFILE`）时是 `<根>/profiles/<名字>/agent`，
//!   此时 `PI_CODING_AGENT_DIR` 被 omp 忽略；
//! - 否则 `PI_CODING_AGENT_DIR` 能整体挪走默认的 `<根>/agent`。
//!
//! omp-zh（汉化版）是同一个程序换了壳，目录和扩展接口都一样。

use std::path::{Path, PathBuf};

use crate::claude::HookStatus;

pub const EXTENSION: &str = include_str!("../omp/clawd-pet.ts");
const EXT_NAME: &str = "clawd-pet";
const MARKER_FILE: &str = ".clawd-pet-managed.json";

/// omp 把 profile 名当 slug：小写字母数字开头，只含 `a-z0-9._-`，最长 64
fn profile_name(raw: Option<std::ffi::OsString>) -> Option<String> {
    let name = raw?.to_str()?.trim().to_string();
    let valid = !name.is_empty()
        && name != "default"
        && !name.ends_with('.')
        && name.len() <= 64
        && name.chars().next().is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        && name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '_' | '-'));
    let reserved = {
        let stem = name.split('.').next().unwrap_or("").to_ascii_uppercase();
        matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
            || ((stem.starts_with("COM") || stem.starts_with("LPT")) && stem.len() == 4 && stem.as_bytes()[3].is_ascii_digit())
    };
    (valid && !reserved).then_some(name)
}

/// 环境里和 omp 相关的几个变量，单独取出来方便测试
#[derive(Default, Clone)]
pub struct Env {
    pub home: Option<PathBuf>,
    pub config_dir: Option<String>,
    pub omp_profile: Option<std::ffi::OsString>,
    pub pi_profile: Option<std::ffi::OsString>,
    pub agent_dir: Option<PathBuf>,
}

impl Env {
    pub fn from_process() -> Env {
        let non_empty = |k: &str| std::env::var_os(k).filter(|v| !v.is_empty());
        Env {
            home: non_empty("USERPROFILE").or_else(|| non_empty("HOME")).map(PathBuf::from),
            config_dir: non_empty("PI_CONFIG_DIR").and_then(|v| v.into_string().ok()),
            // OMP_PROFILE 只要“设了”（哪怕是空）就盖过 PI_PROFILE
            omp_profile: std::env::var_os("OMP_PROFILE"),
            pi_profile: std::env::var_os("PI_PROFILE"),
            agent_dir: non_empty("PI_CODING_AGENT_DIR").map(PathBuf::from),
        }
    }
}

/// omp 此刻会加载扩展的 agent 目录
pub fn agent_dir(env: &Env) -> Option<PathBuf> {
    let home = env.home.clone()?;
    // omp 总是把名字接在 home 后面，所以看着像绝对路径的值也跑不出 home
    let configured = env.config_dir.as_deref().unwrap_or("").trim_start_matches(['/', '\\']);
    let root = home.join(if configured.is_empty() { ".omp" } else { configured });
    let raw_profile = if env.omp_profile.is_some() { env.omp_profile.clone() } else { env.pi_profile.clone() };
    let profile = profile_name(raw_profile);
    if let Some(p) = &profile {
        return Some(root.join("profiles").join(p).join("agent"));
    }
    // 切换 profile 时 omp 会同时导出派生的 PI_CODING_AGENT_DIR；显式回到默认 profile 的子进程
    // 可能还带着它，omp 认得出来并丢掉，这里同样处理
    let derived = profile_name(env.pi_profile.clone()).map(|p| root.join("profiles").join(p).join("agent"));
    match &env.agent_dir {
        Some(dir) if Some(dir) != derived.as_ref() => Some(dir.clone()),
        _ => Some(root.join("agent")),
    }
}

fn ext_dir(agent: &Path) -> PathBuf {
    agent.join("extensions").join(EXT_NAME)
}

fn config_text(port: u16, token: &str) -> String {
    format!("{}\n", serde_json::json!({ "port": port, "token": token }))
}

fn marker_text() -> String {
    format!("{}\n", serde_json::json!({ "managed": true, "by": "clawd-pet" }))
}

fn write_atomic(path: &Path, text: &str) -> Result<(), String> {
    let tmp = path.with_extension("clawd-tmp");
    std::fs::write(&tmp, text).map_err(|e| format!("{}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, path).map_err(|e| format!("{}: {e}", path.display()))
}

/// 是我们装的：目录里有标记文件
fn is_managed(dir: &Path) -> bool {
    std::fs::read_to_string(dir.join(MARKER_FILE))
        .ok()
        .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
        .is_some_and(|v| v["managed"] == true)
}

fn install_into(agent: &Path, port: u16, token: &str) -> Result<(), String> {
    let dir = ext_dir(agent);
    if dir.exists() && !is_managed(&dir) {
        return Err(format!("{} already exists and was not created by Clawd Pet; left untouched", dir.display()));
    }
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    // 标记最先写：中途失败也能被识别成“我们的目录”，卸载不会留残骸
    write_atomic(&dir.join(MARKER_FILE), &marker_text())?;
    write_atomic(&dir.join("config.json"), &config_text(port, token))?;
    write_atomic(&dir.join("index.ts"), EXTENSION)
}

fn uninstall_from(agent: &Path) -> Result<(), String> {
    let dir = ext_dir(agent);
    if !dir.exists() {
        return Ok(());
    }
    if !is_managed(&dir) {
        return Err(format!("{} was not created by Clawd Pet; left untouched", dir.display()));
    }
    std::fs::remove_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))
}

fn status_at(agent: &Path, port: u16, token: &str) -> HookStatus {
    let dir = ext_dir(agent);
    let mut st = HookStatus {
        installed: false,
        permission: false,
        current: true,
        settings_path: dir.display().to_string(),
        error: None,
    };
    if !dir.exists() {
        return st;
    }
    if !is_managed(&dir) {
        st.error = Some(format!("{} exists but was not created by Clawd Pet", dir.display()));
        return st;
    }
    st.installed = true;
    st.current = std::fs::read_to_string(dir.join("index.ts")).is_ok_and(|t| t == EXTENSION)
        && std::fs::read_to_string(dir.join("config.json")).is_ok_and(|t| t == config_text(port, token));
    st
}

pub fn install(port: u16, token: &str) -> Result<(), String> {
    let agent = agent_dir(&Env::from_process()).ok_or("Could not find the oh-my-pi folder")?;
    install_into(&agent, port, token)
}

pub fn uninstall() -> Result<(), String> {
    match agent_dir(&Env::from_process()) {
        Some(a) => uninstall_from(&a),
        None => Ok(()),
    }
}

pub fn status(port: u16, token: &str) -> HookStatus {
    match agent_dir(&Env::from_process()) {
        Some(a) => status_at(&a, port, token),
        None => HookStatus {
            installed: false,
            permission: false,
            current: false,
            settings_path: String::new(),
            error: Some("Could not find the oh-my-pi folder".into()),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;

    fn env(home: &str) -> Env {
        Env { home: Some(PathBuf::from(home)), ..Env::default() }
    }

    fn temp_agent(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("clawd omp テスト {name} {}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn default_agent_dir_and_overrides() {
        assert_eq!(agent_dir(&env("/h")).unwrap(), PathBuf::from("/h/.omp/agent"));
        let e = Env { config_dir: Some("/.cfg".into()), ..env("/h") };
        assert_eq!(agent_dir(&e).unwrap(), PathBuf::from("/h/.cfg/agent"), "an absolute-looking value stays under home");
        let e = Env { agent_dir: Some(PathBuf::from("/x/agent")), ..env("/h") };
        assert_eq!(agent_dir(&e).unwrap(), PathBuf::from("/x/agent"));
        assert_eq!(agent_dir(&Env::default()), None);
    }

    #[test]
    fn named_profile_wins_over_agent_dir_override() {
        let e = Env {
            omp_profile: Some(OsString::from("work")),
            agent_dir: Some(PathBuf::from("/x/agent")),
            ..env("/h")
        };
        assert_eq!(agent_dir(&e).unwrap(), PathBuf::from("/h/.omp/profiles/work/agent"));
        // 设了但为空的 OMP_PROFILE 盖过 PI_PROFILE：回到默认 profile
        let e = Env { omp_profile: Some(OsString::new()), pi_profile: Some(OsString::from("work")), ..env("/h") };
        assert_eq!(agent_dir(&e).unwrap(), PathBuf::from("/h/.omp/agent"));
        // 回到默认 profile 的子进程还带着派生的 PI_CODING_AGENT_DIR：omp 丢掉它，我们也丢
        let e = Env {
            omp_profile: Some(OsString::new()),
            pi_profile: Some(OsString::from("work")),
            agent_dir: Some(PathBuf::from("/h/.omp/profiles/work/agent")),
            ..env("/h")
        };
        assert_eq!(agent_dir(&e).unwrap(), PathBuf::from("/h/.omp/agent"));
    }

    #[test]
    fn invalid_profile_names_fall_back_to_default() {
        for bad in ["..", "Work", "a b", "con", "nul.txt", "x.", "-a", "default"] {
            let e = Env { omp_profile: Some(OsString::from(bad)), ..env("/h") };
            assert_eq!(agent_dir(&e).unwrap(), PathBuf::from("/h/.omp/agent"), "{bad}");
        }
    }

    #[test]
    fn install_status_reinstall_uninstall() {
        let agent = temp_agent("cycle");
        assert!(!status_at(&agent, 47326, "tok").installed);
        install_into(&agent, 47326, "tok").unwrap();
        let st = status_at(&agent, 47326, "tok");
        assert!(st.installed && st.current && st.error.is_none());
        assert_eq!(std::fs::read_to_string(ext_dir(&agent).join("index.ts")).unwrap(), EXTENSION);
        assert!(!status_at(&agent, 47326, "other").current, "a new token needs a reinstall");
        install_into(&agent, 47326, "other").unwrap();
        assert!(status_at(&agent, 47326, "other").current);
        uninstall_from(&agent).unwrap();
        assert!(!ext_dir(&agent).exists());
        assert!(agent.join("extensions").exists(), "only our own folder goes");
        let _ = std::fs::remove_dir_all(agent);
    }

    #[test]
    fn never_touches_an_extension_it_did_not_create() {
        let agent = temp_agent("foreign");
        let dir = ext_dir(&agent);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("index.ts"), "// mine").unwrap();
        assert!(install_into(&agent, 1, "t").is_err());
        assert!(uninstall_from(&agent).is_err());
        assert!(status_at(&agent, 1, "t").error.is_some());
        assert_eq!(std::fs::read_to_string(dir.join("index.ts")).unwrap(), "// mine");
        let _ = std::fs::remove_dir_all(agent);
    }
}
