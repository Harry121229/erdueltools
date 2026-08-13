# erdueltools

Elden Ring duel build toolkit. Save and restore armor, weapons, talismans, spells, and stats. Licensed under [MIT](LICENSE).

艾尔登法环决斗 BD 工具：保存 / 还原护甲、武器、护符、法术与加点。MIT 开源。

## Features

- F5 in-game build manager panel (ImGui)
- F1–F4 load bound builds
- Hold Ctrl (panel toggle) + mouse L/R to pick a build by dominant stat color, release to load
- F7 overwrite current build / Shift+F7 create a new one
- F6 toggle top-right duel score / win-rate HUD
- F9 purge weapons / armor / talismans only
- Language: English / 中文 / 日本語 / Français
- After sync, send a native equipment snapshot so peers refresh appearance

## Build

Requires Rust stable (`x86_64-pc-windows-msvc`) and Windows.

```bat
cargo build --release --target x86_64-pc-windows-msvc
copy /y target\x86_64-pc-windows-msvc\release\erdueltools.dll mod\erdueltools.dll
```

Load with Mod Engine 2. Example config:

```toml
[modengine]
debug = false
external_dlls = [ "SeamlessCoop\\ersc.dll", "mod/erdueltools.dll" ]

[extension.mod_loader]
enabled = true
mods = [{ enabled = true, name = "default", path = "mod" }]
```

Do not run together with er3v3.

## Credits

Game bindings are vendored from [eldenring-rs](https://github.com/vswarte/eldenring-rs) (MIT OR Apache-2.0).
