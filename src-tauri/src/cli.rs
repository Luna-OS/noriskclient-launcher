//! CLI dispatch for the launcher.
//!
//! Two entry points:
//!   * [`dispatch_cold_start`] — called once from `main()`'s `setup()` hook.
//!     Sees the args this process itself was started with.
//!   * [`dispatch_hot_start`] — called from the `tauri-plugin-single-instance`
//!     callback with argv forwarded from a *second* invocation of the binary.
//!
//! Both go through `tauri-plugin-cli`'s `matches()` / `matches_from(argv)` —
//! the subcommand schema lives once in `tauri.conf.json`.
//!
//! The contract external launchers (Open Game Launcher) rely on is documented
//! in `docs/OGL_INTEROP.md` — keep it in sync when changing subcommands here.

use crate::commands::modpack_command::{run_modpack, ModpackArgs, PackSource};
use crate::commands::profile_command::{
    launch_profile_with_overrides, launch_temp_profile, LaunchOverrides, TempLaunchArgs,
};
use crate::state::state_manager::State;
use crate::utils::deep_link_utils::{deliver, focus_main_window};
use log::{error, info, warn};
use serde::Serialize;
use tauri::AppHandle;
use uuid::Uuid;
use tauri_plugin_cli::{CliExt, SubcommandMatches};

/// Quick-play target parsed from `--world` / `--server`. Mutually exclusive —
/// Minecraft can only do one per launch.
#[derive(Default, Clone)]
struct QuickPlay {
    /// `--world`: join this singleplayer world. → quick_play_singleplayer
    world: Option<String>,
    /// `--server`: connect to this server address. → quick_play_multiplayer
    server: Option<String>,
}

impl QuickPlay {
    fn from_matches(sub: &SubcommandMatches) -> Result<Self, String> {
        let world = arg_str(sub, "world");
        let server = arg_str(sub, "server");
        if world.is_some() && server.is_some() {
            return Err("--world and --server are mutually exclusive".into());
        }
        Ok(Self { world, server })
    }
}

/// Parsed `launch` subcommand, lifted out of tauri-plugin-cli's `Matches` map.
struct LaunchArgs {
    profile: String,
    overrides: LaunchOverrides,
    quick_play: QuickPlay,
    local_mods: Vec<String>,
    account: Option<String>,
    jvm_args: Vec<String>,
}

impl LaunchArgs {
    fn from_matches(sub: &SubcommandMatches) -> Result<Self, String> {
        let profile = arg_str(sub, "profile")
            .ok_or_else(|| "--profile is required".to_string())?;
        Ok(Self {
            profile,
            overrides: LaunchOverrides {
                game_version: arg_str(sub, "mc"),
                loader: arg_str(sub, "loader"),
                loader_version: arg_str(sub, "loader-version"),
                pack: arg_str(sub, "pack"),
            },
            quick_play: QuickPlay::from_matches(sub)?,
            local_mods: arg_list(sub, "mods"),
            account: arg_str(sub, "account"),
            jvm_args: arg_words(sub, "jvm-args"),
        })
    }
}

impl ModpackArgs {
    fn from_matches(sub: &SubcommandMatches) -> Result<Self, String> {
        Ok(Self {
            id: arg_str(sub, "id").ok_or_else(|| "--id is required".to_string())?,
            source: match arg_str(sub, "source") {
                Some(value) => PackSource::parse(&value)?,
                None => PackSource::default(),
            },
            // Not "version": clap reserves that name for its own --version flag
            // and panics on the collision while building the subcommand.
            version: arg_str(sub, "pack-version"),
            game_version: arg_str(sub, "mc"),
            loader: arg_str(sub, "loader"),
            no_launch: arg_flag(sub, "no-launch"),
        })
    }
}

/// Frontend events emitted by `open`. Delivered through the deep-link queue so
/// they survive a cold start (frontend drains them via `take_pending_deep_links`).
pub const OPEN_PROFILE_EVENT: &str = "cli-open-profile";
pub const SET_THEME_EVENT: &str = "cli-set-theme";
pub const CREATE_PROFILE_EVENT: &str = "cli-create-profile";

/// How long a CLI action waits for `State::init` before giving up.
const STATE_READY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);

/// Parsed `open` subcommand: show the launcher, optionally on a profile page,
/// with a theme, or with the create-profile dialog open.
#[derive(Debug, Default, PartialEq)]
struct OpenArgs {
    profile: Option<String>,
    theme: Option<String>,
    create: bool,
}

impl OpenArgs {
    fn from_matches(sub: &SubcommandMatches) -> Result<Self, String> {
        let theme = arg_str(sub, "theme");
        if let Some(t) = &theme {
            if !valid_theme_id(t) {
                return Err(format!("--theme '{}' is not a valid theme id", t));
            }
        }
        Ok(Self {
            profile: arg_str(sub, "profile").filter(|p| !p.trim().is_empty()),
            theme,
            create: arg_flag(sub, "create"),
        })
    }
}

/// Theme ids are plain identifiers (`lu_hub`, `christmas`, ...). Anything else
/// is rejected before it reaches the frontend.
fn valid_theme_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct OpenProfilePayload {
    profile_id: String,
}

#[derive(Clone, Serialize)]
struct SetThemePayload {
    theme: String,
}

impl TempLaunchArgs {
    fn from_matches(sub: &SubcommandMatches) -> Result<Self, String> {
        let game_version = arg_str(sub, "mc").ok_or_else(|| "--mc is required".to_string())?;
        let loader = arg_str(sub, "loader").ok_or_else(|| "--loader is required".to_string())?;
        let qp = QuickPlay::from_matches(sub)?;
        Ok(Self {
            game_version,
            loader,
            loader_version: arg_str(sub, "loader-version"),
            pack: arg_str(sub, "pack"),
            name: arg_str(sub, "name"),
            quick_play_singleplayer: qp.world,
            quick_play_multiplayer: qp.server,
            local_mods: arg_list(sub, "mods"),
            account: arg_str(sub, "account"),
            jvm_args: arg_words(sub, "jvm-args"),
        })
    }
}

fn arg_str(sub: &SubcommandMatches, name: &str) -> Option<String> {
    sub.matches
        .args
        .get(name)
        .and_then(|a| a.value.as_str())
        .map(String::from)
}

/// Boolean flag — present (and not literally `false`) means on.
fn arg_flag(sub: &SubcommandMatches, name: &str) -> bool {
    match sub.matches.args.get(name) {
        Some(a) => a.value.as_bool().unwrap_or(true),
        None => false,
    }
}

fn arg_words(sub: &SubcommandMatches, name: &str) -> Vec<String> {
    arg_str(sub, name)
        .map(|s| s.split_whitespace().map(String::from).collect())
        .unwrap_or_default()
}

/// Comma-separated multi-value arg → trimmed, non-empty parts.
fn arg_list(sub: &SubcommandMatches, name: &str) -> Vec<String> {
    arg_str(sub, name)
        .map(|s| {
            s.split(',')
                .map(|p| p.trim().to_string())
                .filter(|p| !p.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

/// Maps a parsed subcommand to its action. `Ok(None)` = not a dispatchable
/// subcommand (unknown, or handled elsewhere like `version`).
fn parse_action(sub: &SubcommandMatches) -> Result<Option<DispatchAction>, String> {
    Ok(Some(match sub.name.as_str() {
        "launch" => DispatchAction::Launch(LaunchArgs::from_matches(sub)?),
        "temp" => DispatchAction::Temp(TempLaunchArgs::from_matches(sub)?),
        "modpack" => DispatchAction::Modpack(ModpackArgs::from_matches(sub)?),
        "open" => DispatchAction::Open(OpenArgs::from_matches(sub)?),
        _ => return Ok(None),
    }))
}

/// Cold-start dispatch. Returns `true` if the app should short-circuit GUI
/// startup (e.g. after printing `version`).
pub fn dispatch_cold_start(app: &AppHandle) -> bool {
    let matches = match app.cli().matches() {
        Ok(m) => m,
        Err(e) => {
            error!("[CLI cold] could not parse args: {}", e);
            return false;
        }
    };
    let Some(sub) = matches.subcommand else {
        return false;
    };

    if sub.name == "version" {
        println!("nrc-launcher {}", env!("CARGO_PKG_VERSION"));
        app.exit(0);
        return true;
    }

    match parse_action(&sub) {
        Ok(Some(action)) => spawn_action(app.clone(), action, "cold"),
        Ok(None) => info!(
            "[CLI cold] unknown subcommand '{}', falling through to GUI",
            sub.name
        ),
        Err(e) => error!("[CLI cold] {}: {}", sub.name, e),
    }
    false
}

/// Hot-start dispatch (called from the single-instance callback). Returns
/// `true` if argv was consumed by a CLI subcommand — caller should then skip
/// the default window-focus behavior.
pub fn dispatch_hot_start(app: &AppHandle, argv: Vec<String>) -> bool {
    if crate::utils::deep_link_utils::is_deep_link_launch(&argv) {
        return false;
    }
    // Log the parse failure instead of swallowing it: an unknown flag or a
    // subcommand this build predates is otherwise indistinguishable from "no
    // subcommand given", and the caller just silently focuses the window.
    let matches = match app.cli().matches_from(argv) {
        Ok(m) => m,
        Err(e) => {
            error!("[CLI hot] could not parse forwarded args: {}", e);
            return false;
        }
    };
    let Some(sub) = matches.subcommand else {
        info!("[CLI hot] forwarded args carried no subcommand");
        return false;
    };
    info!("[CLI hot] subcommand '{}'", sub.name);

    match parse_action(&sub) {
        Ok(Some(action)) => {
            spawn_action(app.clone(), action, "hot");
            true
        }
        Ok(None) => false,
        Err(e) => {
            error!("[CLI hot] {}: {}", sub.name, e);
            false
        }
    }
}

/// Action picked up by either dispatch path.
enum DispatchAction {
    Launch(LaunchArgs),
    Temp(TempLaunchArgs),
    Modpack(ModpackArgs),
    Open(OpenArgs),
}

async fn run_action(app: &AppHandle, action: DispatchAction, tag: &'static str) {
    match action {
        DispatchAction::Launch(a) => {
            match launch_profile_with_overrides(
                a.profile,
                a.overrides,
                a.quick_play.world,
                a.quick_play.server,
                a.local_mods,
                a.account,
                Some(a.jvm_args),
            )
            .await
            {
                Ok(_) => info!("[CLI {}] Launch dispatched.", tag),
                Err(e) => error!("[CLI {}] Launch failed: {:?}", tag, e),
            }
        }
        DispatchAction::Temp(a) => match launch_temp_profile(a).await {
            Ok(_) => info!("[CLI {}] Temp launch dispatched.", tag),
            Err(e) => error!("[CLI {}] Temp launch failed: {:?}", tag, e),
        },
        // Downloads before it launches, so this one runs for a while rather than
        // just handing off like the other two.
        DispatchAction::Modpack(a) => {
            info!("[CLI {}] Modpack install starting.", tag);
            run_modpack(a).await;
            info!("[CLI {}] Modpack install finished.", tag);
        }
        DispatchAction::Open(a) => run_open(app, a, tag).await,
    }
}

async fn run_open(app: &AppHandle, args: OpenArgs, tag: &'static str) {
    focus_main_window(app);
    if let Some(theme) = args.theme {
        info!("[CLI {}] open: theme '{}'", tag, theme);
        deliver(app, SET_THEME_EVENT, SetThemePayload { theme });
    }
    if let Some(profile_ref) = args.profile {
        match resolve_profile_id(&profile_ref).await {
            Some(id) => {
                info!("[CLI {}] open: profile {}", tag, id);
                deliver(
                    app,
                    OPEN_PROFILE_EVENT,
                    OpenProfilePayload {
                        profile_id: id.to_string(),
                    },
                );
            }
            None => warn!(
                "[CLI {}] open: no profile matches '{}', showing the launcher only",
                tag, profile_ref
            ),
        }
    }
    if args.create {
        deliver(app, CREATE_PROFILE_EVENT, serde_json::json!({}));
    }
}

/// UUID or exact (unique) profile name → profile id. Standard profiles are
/// accepted by id too.
async fn resolve_profile_id(profile_ref: &str) -> Option<Uuid> {
    let state = State::get().await.ok()?;
    let profiles = state.profile_manager.list_profiles().await.ok()?;
    if let Ok(id) = Uuid::parse_str(profile_ref.trim()) {
        let known = profiles.iter().any(|p| p.id == id)
            || state
                .norisk_version_manager
                .get_profile_by_id(id)
                .await
                .is_some();
        return known.then_some(id);
    }
    let mut named = profiles.iter().filter(|p| p.name == profile_ref);
    match (named.next(), named.next()) {
        (Some(p), None) => Some(p.id),
        _ => None,
    }
}

/// Runs an action once `State::init` has fully finished. Both the cold path
/// (state still initializing) and the hot path (a second start can arrive
/// while the first instance is still booting) go through here, so a CLI
/// action never reads profiles before the database is open and migrated.
fn spawn_action(app: AppHandle, action: DispatchAction, tag: &'static str) {
    tauri::async_runtime::spawn(async move {
        if !State::wait_ready(STATE_READY_TIMEOUT).await {
            error!(
                "[CLI {}] launcher state not ready after {}s, dropping the CLI action",
                tag,
                STATE_READY_TIMEOUT.as_secs()
            );
            return;
        }
        run_action(&app, action, tag).await;
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use tauri_plugin_cli::ArgData;

    fn sub(name: &str, args: &[(&str, serde_json::Value)]) -> SubcommandMatches {
        let mut s = SubcommandMatches::default();
        s.name = name.to_string();
        for (k, v) in args {
            let mut a = ArgData::default();
            a.value = v.clone();
            a.occurrences = 1;
            s.matches.args.insert(k.to_string(), a);
        }
        s
    }

    #[test]
    fn open_parses_all_flags() {
        let s = sub(
            "open",
            &[
                ("profile", "3f1c2b8e-0d5a-4c1e-9f7a-2b6d8e9c0a11".into()),
                ("theme", "lu_hub".into()),
                ("create", true.into()),
            ],
        );
        let a = OpenArgs::from_matches(&s).unwrap();
        assert_eq!(
            a.profile.as_deref(),
            Some("3f1c2b8e-0d5a-4c1e-9f7a-2b6d8e9c0a11")
        );
        assert_eq!(a.theme.as_deref(), Some("lu_hub"));
        assert!(a.create);
    }

    #[test]
    fn open_without_args_is_plain_focus() {
        let a = OpenArgs::from_matches(&sub("open", &[])).unwrap();
        assert_eq!(a, OpenArgs::default());
        // A blank profile is ignored, not an error.
        let a = OpenArgs::from_matches(&sub("open", &[("profile", "  ".into())])).unwrap();
        assert_eq!(a.profile, None);
    }

    #[test]
    fn open_rejects_bad_theme() {
        let long = "x".repeat(65);
        for bad in ["", "../x", "lu hub", "a;b", long.as_str()] {
            let s = sub("open", &[("theme", bad.into())]);
            assert!(OpenArgs::from_matches(&s).is_err(), "accepted {:?}", bad);
        }
        assert!(valid_theme_id("lu_hub"));
        assert!(valid_theme_id("christmas-2025"));
    }

    #[test]
    fn launch_requires_profile_and_rejects_world_plus_server() {
        assert!(LaunchArgs::from_matches(&sub("launch", &[])).is_err());
        let s = sub(
            "launch",
            &[
                ("profile", "x".into()),
                ("world", "w".into()),
                ("server", "s".into()),
            ],
        );
        assert!(LaunchArgs::from_matches(&s).is_err());
        let s = sub(
            "launch",
            &[
                ("profile", "x".into()),
                ("mods", " a.jar, ,b ".into()),
                ("jvm-args", "-Xmx2G  -Dfoo=1".into()),
            ],
        );
        let a = LaunchArgs::from_matches(&s).unwrap();
        assert_eq!(a.local_mods, vec!["a.jar", "b"]);
        assert_eq!(a.jvm_args, vec!["-Xmx2G", "-Dfoo=1"]);
    }

    #[test]
    fn flag_semantics() {
        assert!(!arg_flag(&sub("open", &[]), "create"));
        assert!(arg_flag(&sub("open", &[("create", true.into())]), "create"));
        assert!(!arg_flag(&sub("open", &[("create", false.into())]), "create"));
    }

    #[test]
    fn parse_action_maps_subcommands() {
        assert!(matches!(
            parse_action(&sub("open", &[])),
            Ok(Some(DispatchAction::Open(_)))
        ));
        assert!(matches!(parse_action(&sub("version", &[])), Ok(None)));
        assert!(matches!(parse_action(&sub("nope", &[])), Ok(None)));
        assert!(parse_action(&sub("launch", &[])).is_err());
    }

    /// The schema lives in tauri.conf.json; every subcommand and arg this module
    /// reads must be declared there, otherwise clap rejects it at runtime.
    #[test]
    fn tauri_conf_declares_cli_schema() {
        let conf: serde_json::Value =
            serde_json::from_str(include_str!("../tauri.conf.json")).unwrap();
        let subs = &conf["plugins"]["cli"]["subcommands"];
        let arg_names = |name: &str| -> Vec<String> {
            subs[name]["args"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|x| x["name"].as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default()
        };
        for a in ["profile", "theme", "create"] {
            assert!(arg_names("open").contains(&a.to_string()), "open --{}", a);
        }
        for a in [
            "profile", "mc", "loader", "world", "server", "mods", "jvm-args", "account",
        ] {
            assert!(arg_names("launch").contains(&a.to_string()), "launch --{}", a);
        }
        assert!(subs["version"].is_object());
    }
}
