# NoRisk Launcher ↔ Open Game Launcher (OGL)

This is how an external launcher (Open Game Launcher, OGL) starts and controls
the NoRisk Client launcher (NRC). OGL shows **one** "NoRisk Client" game. It
starts NRC only through the command line described here. It may *read* NRC's
profile database, but it never writes to NRC's database or folders.

Source of truth: `src-tauri/src/cli.rs` (dispatch) and the `plugins.cli` block of
`src-tauri/tauri.conf.json` (schema). The test `cli::tests::tauri_conf_declares_cli_schema`
keeps the two in sync.

## Single instance

NRC runs once per user (`tauri-plugin-single-instance`, registered as the first plugin):

- If NRC is **not running**, the command starts it and runs once the launcher
  state is ready (database open, profiles migrated). It waits for that for up
  to 120 s.
- If NRC is **already running**, the second process passes its argv to the
  running one and exits right away. The running instance does the work, so
  the 2nd process's exit code says nothing about the result.
- A start **without a subcommand** (or with one this NRC version doesn't
  know) just brings the existing window to the front.

NRC is a GUI app, so exit codes and stdout are **not** a result channel. Errors
go to the launcher log (`<root>/logs`).

## Commands

| Command | Effect |
|---|---|
| `nrc launch --profile <uuid\|exact name>` | Starts the game for that profile, without showing the launcher window. |
| `nrc open [--profile <uuid>] [--theme <id>] [--create]` | Shows the launcher: starts it or focuses the running one. |
| `nrc version` | Prints `nrc-launcher X.Y.Z` and exits. On Windows the release build has no console, so don't rely on stdout there. |
| `nrc` (no args) | Normal start, or focuses the running launcher. |

### `launch`

- `--profile, -p` (required): profile UUID, or its exact name if that name is
  unique. OGL should always pass the UUID.
- `--server, -s <host[:port]>` or `--world, -w <name>`: quick play (only one of them).
- `--account, -a <name|uuid>`: play with that Minecraft account instead of the active one.
- Runtime-only overrides, which are not saved: `--mc`, `--loader`,
  `--loader-version`, `--pack`, `--mods <a.jar,dir>`, `--jvm-args="..."`
  (use the `=` form).

### `open`

- `--profile, -p <uuid|exact name>`: opens that profile's page. An unknown
  profile is ignored (the launcher only opens) and a warning is logged.
- `--theme, -t <id>`: switches the launcher theme, e.g. `lu_hub`. Ids must
  match `[A-Za-z0-9_-]{1,64}`, otherwise the whole `open` is rejected.
- `--create`: opens the create-profile dialog. OGL's "New profile" button
  uses this, so profiles are only ever created by NRC itself.
- The flags can be combined. Order of effects: focus window → theme → profile page → create dialog.

Frontend contract (`src/`): the backend delivers these through the deep-link
queue (`deliver()` in `utils/deep_link_utils.rs`). If the frontend isn't
ready yet, they are queued and handed out by `take_pending_deep_links`.
Otherwise they arrive as live Tauri events.

| Event | Payload |
|---|---|
| `cli-set-theme` | `{ "theme": "lu_hub" }` |
| `cli-open-profile` | `{ "profileId": "<uuid>" }` (always a resolved UUID) |
| `cli-create-profile` | `{}` |

## Finding the executable

- **Windows:** `%LOCALAPPDATA%\NoRisk Launcher\` or `%LOCALAPPDATA%\Programs\NoRisk Launcher\`
  (the uninstall registry entry's `DisplayName` contains "NoRisk"). The exe is
  `NoRisk Launcher.exe` or `noriskclient-launcher-v3.exe`.
- **macOS:** `/Applications/NoRisk Launcher.app`.
- **Linux:** `noriskclient-launcher-v3` in `/usr/bin`, or Flatpak `gg.norisk.NoRiskClientLauncherV3`.

## Reading profiles (read-only)

Root dir (`<root>`):

| OS | Path |
|---|---|
| Windows | `%APPDATA%\norisk\NoRiskClientV3` |
| macOS | `~/Library/Application Support/gg.norisk.NoRiskClientV3` |
| Linux | `$XDG_DATA_HOME/noriskclientv3` (Flatpak: `~/.var/app/gg.norisk.NoRiskClientLauncherV3/data/noriskclientv3`) |

- **Data dir:** Windows `<root>\data`. On macOS and Linux it is `<root>` itself.
- **Database:** always `<standard meta>/app.db`, even when a custom game
  directory is set. The standard meta dir is `<root>\meta` on Windows and
  `<root>/meta` elsewhere.
  - It runs in WAL mode. Read a **copy** of `app.db`, `app.db-wal` and
    `app.db-shm`, or open it read-only. Never write to it.
- **Watching for changes:** poll the mtime of `app.db` and `app.db-wal`.
  Nothing else changes when profiles change. `profiles.json` is a legacy file
  that was migrated once.
- **Table `profiles`:** columns `id` (UUID text), `name`, `path`,
  `game_version`, `loader`, `last_played`.
  - `last_played` is Unix time in **nanoseconds** (UTC). `NULL` means never played.
- **Config:** `launcher_config.json` sits in `<root>` (the root dir, not the data dir).

### Profiles dir

1. If `custom_game_directory` is set in `launcher_config.json`, use `<custom>/profiles`.
2. Otherwise use `<data>/profiles`.

### Instance dir of a profile

The checks below are case-insensitive. They mirror
`ProfileManager::calculate_instance_path_for_profile`.

1. Shared folder applies when `use_shared_minecraft_folder` is true and the
   group is not `server` or `modpacks`. Then:
   - group `nrc`, `noriskclient` or `norisk client`:
     `profiles/noriskclient/legacy` (MC < 1.13) or `profiles/noriskclient/new`
   - any other group: `profiles/groups/<sanitize_filename(lowercase(group))>`
   - no group: rule 2
2. Otherwise: `profiles/<path>`. To resolve `path`:
   - split it on `/` **and** `\`
   - drop segments that are empty or consist only of dots and spaces (`.`, `..`)
   - drop segments containing `:`

   This is `join_profile_path` in `profile_state.rs`. A `path` from the
   database can never point outside the profiles dir.

Profiles with a shared folder share one instance dir per group. Anything
placed there affects every profile in that group.

### Adding content to an instance

- **Mods:** put local jars flat into `<instance>/mods/`. NRC's own "add local
  mod" does the same.
  - `custom_mods/` is **deprecated**. NRC still reads it at launch for old
    profiles, but don't write new files there.
- **Resource packs, shader packs, worlds:** `resourcepacks/`, `shaderpacks/`
  and `saves/`, as in vanilla.
- **New profiles:** don't create them yourself. Use `open --create`.

## What NRC reuses from the PC

| What | How |
|---|---|
| Data dir and profiles | Fixed per-user location (above). A reinstall finds the same database. A database left in an old custom game dir is adopted into the standard meta dir once. |
| Java | NRC's own Zulu runtime (`<meta>/java/zulu_<N>`) if present. If not, the official launcher's runtime for the exact component is used when its major version matches. Only then does NRC download a new runtime. |
| Assets | Objects from the official `.minecraft/assets` are hard-linked (or copied across drives) instead of downloaded. A broken `.minecraft` falls back to downloading. |
| Imports | Prism, MultiMC, Modrinth App, CurseForge and others. Imported mod jars seed the shared mod cache. |

## Housekeeping (no loose instances)

- **Pack imports** (`.mrpack`, CurseForge zip, `.noriskpack`), **create** and
  **copy profile**: if one fails before the profile is saved, the instance
  folder it created is removed again (`utils/dir_guard.rs`).
- **Launcher imports:** they stage in `profiles/.import-<uuid>`. Stale staging
  dirs left by a crash are deleted at startup.
- **Java downloads:** they stage in `<meta>/java/.<name>.partial-<id>` and are
  renamed into place only when complete.
- **Temp profiles** (`temp` subcommand): they live in `profiles/noriskclient/temp/`
  and are moved to the trash at the next start. A folder still in use (the
  game outlived the launcher) is skipped.
- **Orphaned instance folders:** these are folders in the profiles dir that no
  profile points to. They are logged at startup and listed by the Tauri command
  `list_orphaned_instance_dirs`. They are **never deleted automatically**.
