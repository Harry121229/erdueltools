@echo off
chcp 65001 >nul
setlocal
cd /d "%~dp0"

if not exist "mod\erdueltools.dll" (
  echo [error] missing mod\erdueltools.dll
  echo Build first: cargo build --release --target x86_64-pc-windows-msvc
  pause
  exit /b 1
)

if not exist "modengine2_launcher.exe" (
  echo [error] put this folder next to Mod Engine 2, or copy launcher here.
  pause
  exit /b 1
)

".\modengine2_launcher.exe" -t er -c ".\config_erdueltools.toml"
endlocal
