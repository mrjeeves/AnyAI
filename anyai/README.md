# AnyAI uninstaller (temporary)

One-shot scripts to wipe every artifact the **old** `AnyAI` install dropped on
a test machine, before installing the renamed `myownllm` build. This folder
will be deleted from the repo once all test machines are clean — it is not
shipped to end users.

## What it removes

- Binary: `/usr/local/bin/anyai`, `~/.local/bin/anyai`, `%LOCALAPPDATA%\Programs\AnyAI\anyai.exe`
- App data: `~/.anyai/` (config, conversations, whisper models, transcribe
  buffers, staged self-updates, `watcher.lock`)
- Tauri app data under the old bundle ID `run.anyai.app`:
  - macOS: `~/Library/{Application Support,Caches,Logs,WebKit,HTTPStorages,Saved Application State}/run.anyai.app*`, `~/Library/Preferences/run.anyai.app.plist`
  - Linux: `~/.config/run.anyai.app`, `~/.local/share/run.anyai.app`, `~/.cache/run.anyai.app`
  - Windows: `%APPDATA%\run.anyai.app`, `%LOCALAPPDATA%\run.anyai.app`
- The `# added by anyai installer` PATH line in `~/.bashrc` / `~/.zshrc` /
  `~/.profile` / `~/.bash_profile` / `~/.config/fish/config.fish`
- The `%LOCALAPPDATA%\Programs\AnyAI` entry on the Windows user PATH
- Stray installer temp dirs (`/tmp/anyai-install-*`, `%TEMP%\anyai-install-*`,
  `%TEMP%\AnyAI-*`)

The scripts also stop any running `anyai` / `anyai.exe` process first so file
removal succeeds.

Ollama and any models managed by Ollama are left untouched — those are not
AnyAI artifacts.

## Run it

### macOS / Linux

```sh
./anyai/uninstall.sh --dry-run   # preview
./anyai/uninstall.sh             # actually remove
```

May prompt for `sudo` if the binary lives in `/usr/local/bin`.

### Windows (PowerShell)

```powershell
.\anyai\uninstall.ps1 -DryRun
.\anyai\uninstall.ps1
```

No admin needed — everything lives under the user profile.
