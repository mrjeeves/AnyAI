use anyhow::{anyhow, Result};

/// Entry point for CLI mode. `args` is everything after the binary name.
pub async fn run(args: Vec<String>) -> Result<()> {
    match args.first().map(|s| s.as_str()) {
        Some("run") => cmd_run(&args[1..]).await,
        Some("status") => cmd_status(&args[1..]).await,
        Some("stop") => cmd_stop().await,
        Some("models") => cmd_models(&args[1..]).await,
        Some("sources") => cmd_sources(&args[1..]).await,
        Some("providers") => cmd_providers(&args[1..]).await,
        Some("import") => cmd_import(&args[1..]).await,
        Some("export") => cmd_export(&args[1..]).await,
        Some("help") | Some("--help") | Some("-h") => { print_help(); Ok(()) }
        Some(unknown) => Err(anyhow!("unknown command: {unknown}\nRun `anyai help` for usage.")),
        None => { print_help(); Ok(()) }
    }
}

fn print_help() {
    println!(
r#"anyai — local AI, zero configuration

USAGE:
  anyai [command] [flags]

COMMANDS:
  run           Start chat (terminal)
  status        Show current state
  stop          Stop ollama serve
  models        Manage pulled models
  sources       Manage provider sources
  providers     Manage providers
  import <url>  Import config from URL or file
  export        Export config

FLAGS (run):
  --mode <text|vision|code|transcribe>
  --model <name>        Override model
  --profile <url>       One-off manifest URL

FLAGS (global):
  --json                Machine-readable JSON output
  --quiet               Suppress progress output
"#
    );
}

async fn cmd_run(args: &[String]) -> Result<()> {
    let mut mode = "text".to_string();
    let mut model_override: Option<String> = None;
    let mut profile_url: Option<String> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--mode" => { mode = args.get(i + 1).cloned().unwrap_or_default(); i += 2; }
            "--model" => { model_override = args.get(i + 1).cloned(); i += 2; }
            "--profile" => { profile_url = args.get(i + 1).cloned(); i += 2; }
            _ => { i += 1; }
        }
    }

    // Install Ollama if missing
    if !crate::ollama::is_installed() {
        eprintln!("Ollama not found. Installing…");
        crate::ollama::install().await?;
    }

    // Ensure Ollama is running
    eprint!("Starting ollama… ");
    crate::ollama::ensure_running().await?;
    eprintln!("ok");

    // Resolve model
    let hw = crate::hardware::detect()?;
    let model = if let Some(m) = model_override {
        m
    } else {
        resolve_model_for_cli(&hw, &mode, profile_url.as_deref()).await?
    };

    eprintln!("Model: {model}  Mode: {mode}");

    // Pull model if not present
    pull_if_needed(&model).await?;

    // Chat loop
    chat_loop(&model, &mode).await
}

async fn resolve_model_for_cli(
    hw: &crate::hardware::HardwareProfile,
    mode: &str,
    profile_url: Option<&str>,
) -> Result<String> {
    // Delegate to the TS layer by reading the config and resolving locally (simplified Rust impl)
    // In production this would call into the same manifest resolution logic.
    // For the CLI we use a direct HTTP fetch + local tier resolution.
    let anyai_dir = crate::anyai_dir()?;
    let config_path = anyai_dir.join("config.json");

    let manifest_url = if let Some(url) = profile_url {
        url.to_string()
    } else if config_path.exists() {
        let config: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&config_path)?)?;
        let active = config["active_provider"].as_str().unwrap_or("");
        config["providers"]
            .as_array()
            .and_then(|ps| ps.iter().find(|p| p["name"].as_str() == Some(active)))
            .and_then(|p| p["url"].as_str())
            .unwrap_or("https://anyai.run/manifest/default.json")
            .to_string()
    } else {
        "https://anyai.run/manifest/default.json".to_string()
    };

    // Check mode_overrides
    if config_path.exists() {
        let config: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&config_path)?)?;
        if let Some(m) = config["mode_overrides"][mode].as_str() {
            return Ok(m.to_string());
        }
    }

    // Fetch or load cached manifest
    let cache_dir = anyai_dir.join("cache/manifests");
    let cache_key = format!("{:x}", md5_hash(&manifest_url));
    let cache_path = cache_dir.join(format!("{cache_key}.json"));

    let manifest_json = if cache_path.exists() {
        std::fs::read_to_string(&cache_path)?
    } else {
        let out = tokio::process::Command::new("curl")
            .args(["-sf", "--max-time", "10", &manifest_url])
            .output()
            .await?;
        if out.status.success() {
            let s = String::from_utf8_lossy(&out.stdout).into_owned();
            let _ = std::fs::create_dir_all(&cache_dir);
            let _ = std::fs::write(&cache_path, &s);
            s
        } else {
            // Try bundled fallback
            let fallback = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../manifests/default.json");
            std::fs::read_to_string(fallback).unwrap_or_default()
        }
    };

    resolve_from_manifest_json(&manifest_json, hw, mode)
}

fn resolve_from_manifest_json(
    json: &str,
    hw: &crate::hardware::HardwareProfile,
    mode: &str,
) -> Result<String> {
    let manifest: serde_json::Value = serde_json::from_str(json)
        .map_err(|_| anyhow!("invalid manifest JSON"))?;

    let default_mode = manifest["default_mode"].as_str().unwrap_or("text");
    let mode_spec = manifest["modes"][mode]
        .as_object()
        .or_else(|| manifest["modes"][default_mode].as_object())
        .ok_or_else(|| anyhow!("mode '{mode}' not found in manifest"))?;

    let tiers = mode_spec["tiers"].as_array()
        .ok_or_else(|| anyhow!("no tiers in manifest"))?;

    let vram = hw.vram_gb.unwrap_or(0.0);
    let ram = hw.ram_gb;

    for tier in tiers {
        let min_vram = tier["min_vram_gb"].as_f64().unwrap_or(0.0);
        let min_ram = tier["min_ram_gb"].as_f64().unwrap_or(0.0);
        if vram >= min_vram || ram >= min_ram {
            if let Some(model) = tier["model"].as_str() {
                return Ok(model.to_string());
            }
        }
    }

    // Bottom-tier fallback
    tiers.last()
        .and_then(|t| t["model"].as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| anyhow!("no model found in manifest tiers"))
}

fn md5_hash(s: &str) -> u64 {
    // Simple djb2 hash as a cache key — not cryptographic.
    s.bytes().fold(5381u64, |h, b| h.wrapping_mul(33).wrapping_add(b as u64))
}

async fn pull_if_needed(model: &str) -> Result<()> {
    let out = tokio::process::Command::new("ollama")
        .args(["show", "--modelfile", model])
        .output()
        .await?;
    if out.status.success() { return Ok(()); }

    eprint!("Pulling {model}… ");
    let status = tokio::process::Command::new("ollama")
        .args(["pull", model])
        .status()
        .await?;
    if !status.success() {
        return Err(anyhow!("failed to pull {model}"));
    }
    eprintln!("done");
    Ok(())
}

async fn chat_loop(model: &str, _mode: &str) -> Result<()> {
    use std::io::{self, BufRead, Write};
    let stdin = io::stdin();
    let mut history: Vec<serde_json::Value> = Vec::new();

    println!("AnyAI — {model}  (Ctrl+C or type 'exit' to quit)\n");

    loop {
        print!("> ");
        io::stdout().flush()?;
        let mut line = String::new();
        if stdin.lock().read_line(&mut line)? == 0 { break; } // EOF
        let line = line.trim();
        if line.is_empty() { continue; }
        if line == "exit" || line == "quit" { break; }

        history.push(serde_json::json!({ "role": "user", "content": line }));

        let body = serde_json::json!({
            "model": model,
            "messages": history,
            "stream": false
        });

        let response = tokio::process::Command::new("curl")
            .args([
                "-sf", "-X", "POST",
                "http://127.0.0.1:11434/api/chat",
                "-H", "Content-Type: application/json",
                "-d", &body.to_string(),
            ])
            .output()
            .await?;

        if !response.status.success() {
            eprintln!("(ollama request failed)");
            continue;
        }

        let resp: serde_json::Value = serde_json::from_slice(&response.stdout)
            .unwrap_or_default();
        let content = resp["message"]["content"].as_str().unwrap_or("(no response)");
        println!("{content}\n");
        history.push(serde_json::json!({ "role": "assistant", "content": content }));
    }
    Ok(())
}

async fn cmd_status(args: &[String]) -> Result<()> {
    let json = args.contains(&"--json".to_string());
    let hw = crate::hardware::detect()?;

    let anyai_dir = crate::anyai_dir()?;
    let config_path = anyai_dir.join("config.json");
    let (active_provider, active_mode) = if config_path.exists() {
        let config: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&config_path)?)?;
        (
            config["active_provider"].as_str().unwrap_or("(none)").to_string(),
            config["active_mode"].as_str().unwrap_or("text").to_string(),
        )
    } else {
        ("(none)".to_string(), "text".to_string())
    };

    let running = {
        let out = tokio::process::Command::new("curl")
            .args(["-sf", "--max-time", "1", "http://127.0.0.1:11434/"])
            .output()
            .await;
        out.map(|o| o.status.success()).unwrap_or(false)
    };

    if json {
        println!("{}", serde_json::json!({
            "active_provider": active_provider,
            "active_mode": active_mode,
            "ollama_running": running,
            "hardware": hw,
        }));
    } else {
        println!("Provider : {active_provider}");
        println!("Mode     : {active_mode}");
        println!("Ollama   : {}", if running { "running" } else { "stopped" });
        println!("VRAM     : {}",
            hw.vram_gb.map(|v| format!("{:.1} GB ({:?})", v, hw.gpu_type))
                      .unwrap_or_else(|| "none (CPU)".into()));
        println!("RAM      : {:.1} GB", hw.ram_gb);
        println!("Disk free: {:.1} GB", hw.disk_free_gb);
    }
    Ok(())
}

async fn cmd_stop() -> Result<()> {
    crate::ollama::stop().await?;
    println!("ollama stopped");
    Ok(())
}

async fn cmd_models(args: &[String]) -> Result<()> {
    let json = args.contains(&"--json".to_string());

    match args.first().map(|s| s.as_str()) {
        None | Some("--json") => {
            // List models
            let pulled = crate::ollama::list_models().await?;
            let config = load_config()?;
            let kept: Vec<&str> = config["kept_models"].as_array()
                .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
                .unwrap_or_default();
            let overrides = config["mode_overrides"].as_object();
            let override_models: Vec<&str> = overrides
                .map(|o| o.values().filter_map(|v| v.as_str()).collect())
                .unwrap_or_default();

            if json {
                let items: Vec<_> = pulled.iter().map(|m| {
                    serde_json::json!({
                        "name": m.name,
                        "size_bytes": m.size,
                        "kept": kept.contains(&m.name.as_str()),
                        "override_for": overrides.map(|o| {
                            o.iter().filter(|(_, v)| v.as_str() == Some(&m.name))
                                    .map(|(k, _)| k.clone()).collect::<Vec<_>>()
                        }).unwrap_or_default(),
                    })
                }).collect();
                println!("{}", serde_json::to_string_pretty(&items)?);
            } else {
                println!("{:<35} {:>10}  {:}", "NAME", "SIZE", "FLAGS");
                for m in &pulled {
                    let size_gb = m.size as f64 / 1024.0 / 1024.0 / 1024.0;
                    let mut flags = vec![];
                    if kept.contains(&m.name.as_str()) { flags.push("kept"); }
                    if override_models.contains(&m.name.as_str()) { flags.push("override"); }
                    println!("{:<35} {:>9.1}G  {}", m.name, size_gb, flags.join(" "));
                }
            }
        }
        Some("keep") => {
            let name = args.get(1).ok_or_else(|| anyhow!("usage: anyai models keep <model>"))?;
            let mut config = load_config()?;
            let kept = config["kept_models"].as_array_mut().map(|a| {
                if !a.iter().any(|v| v.as_str() == Some(name)) {
                    a.push(serde_json::json!(name));
                }
            });
            if kept.is_none() {
                config["kept_models"] = serde_json::json!([name]);
            }
            save_config(&config)?;
            println!("Kept: {name}");
        }
        Some("unkeep") => {
            let name = args.get(1).ok_or_else(|| anyhow!("usage: anyai models unkeep <model>"))?;
            let mut config = load_config()?;
            if let Some(arr) = config["kept_models"].as_array_mut() {
                arr.retain(|v| v.as_str() != Some(name));
            }
            save_config(&config)?;
            println!("Unpinned: {name}");
        }
        Some("override") => {
            let mode = args.get(1).ok_or_else(|| anyhow!("usage: anyai models override <mode> <model|--clear>"))?;
            let model_or_clear = args.get(2).ok_or_else(|| anyhow!("usage: anyai models override <mode> <model|--clear>"))?;
            let mut config = load_config()?;
            if config["mode_overrides"].is_null() {
                config["mode_overrides"] = serde_json::json!({});
            }
            if model_or_clear == "--clear" {
                config["mode_overrides"][mode] = serde_json::Value::Null;
                println!("Override for {mode} cleared");
            } else {
                config["mode_overrides"][mode] = serde_json::json!(model_or_clear);
                println!("Override for {mode}: {model_or_clear}");
            }
            save_config(&config)?;
        }
        Some("prune") => {
            let config = load_config()?;
            let kept: Vec<&str> = config["kept_models"].as_array()
                .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
                .unwrap_or_default();
            let override_models: Vec<&str> = config["mode_overrides"].as_object()
                .map(|o| o.values().filter_map(|v| v.as_str()).collect())
                .unwrap_or_default();

            let status_path = crate::anyai_dir()?.join("cache/model-status.json");
            let unrecommended: Vec<String> = if status_path.exists() {
                let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&status_path)?)?;
                v.as_object().map(|o| {
                    o.iter()
                     .filter(|(_, info)| info["recommended_by"].as_array().map(|a| a.is_empty()).unwrap_or(true))
                     .map(|(k, _)| k.clone())
                     .collect()
                }).unwrap_or_default()
            } else {
                vec![]
            };

            for model in &unrecommended {
                if kept.contains(&model.as_str()) || override_models.contains(&model.as_str()) {
                    continue;
                }
                println!("Removing {model}…");
                let _ = crate::ollama::delete_model(model).await;
            }
            println!("Prune complete");
        }
        Some("rm") => {
            let name = args.get(1).ok_or_else(|| anyhow!("usage: anyai models rm <model>"))?;
            // Remove keep + override entries for this model too
            let mut config = load_config()?;
            if let Some(arr) = config["kept_models"].as_array_mut() {
                arr.retain(|v| v.as_str() != Some(name));
            }
            if let Some(overrides) = config["mode_overrides"].as_object_mut() {
                for v in overrides.values_mut() {
                    if v.as_str() == Some(name) {
                        *v = serde_json::Value::Null;
                    }
                }
            }
            save_config(&config)?;
            crate::ollama::delete_model(name).await?;
            println!("Removed: {name}");
        }
        Some(unknown) => return Err(anyhow!("unknown models subcommand: {unknown}")),
    }
    Ok(())
}

async fn cmd_sources(args: &[String]) -> Result<()> {
    match args.first().map(|s| s.as_str()) {
        None => {
            let config = load_config()?;
            let sources = config["sources"].as_array().cloned().unwrap_or_default();
            for s in &sources {
                println!("  {}  {}", s["name"].as_str().unwrap_or("?"), s["url"].as_str().unwrap_or("?"));
            }
        }
        Some("add") => {
            let url = args.get(1).ok_or_else(|| anyhow!("usage: anyai sources add <url> --name <name>"))?;
            let name = flag_value(args, "--name").unwrap_or_else(|| url.clone());
            let mut config = load_config()?;
            let sources = config["sources"].as_array_mut()
                .ok_or_else(|| anyhow!("config missing sources array"))?;
            if sources.iter().any(|s| s["name"].as_str() == Some(&name)) {
                // Update URL
                for s in sources.iter_mut() {
                    if s["name"].as_str() == Some(&name) {
                        s["url"] = serde_json::json!(url);
                    }
                }
            } else {
                sources.push(serde_json::json!({ "name": name, "url": url }));
            }
            save_config(&config)?;
            println!("Source added: {name}");
        }
        Some("rm") => {
            let name = args.get(1).ok_or_else(|| anyhow!("usage: anyai sources rm <name>"))?;
            let mut config = load_config()?;
            if let Some(arr) = config["sources"].as_array_mut() {
                arr.retain(|s| s["name"].as_str() != Some(name));
            }
            save_config(&config)?;
            println!("Source removed: {name}");
        }
        Some("list") => {
            let source_name = args.get(1).ok_or_else(|| anyhow!("usage: anyai sources list <name>"))?;
            let config = load_config()?;
            let sources = config["sources"].as_array().cloned().unwrap_or_default();
            let url = sources.iter()
                .find(|s| s["name"].as_str() == Some(source_name))
                .and_then(|s| s["url"].as_str())
                .ok_or_else(|| anyhow!("source '{source_name}' not found"))?
                .to_string();
            let out = tokio::process::Command::new("curl")
                .args(["-sf", "--max-time", "10", &url])
                .output().await?;
            let v: serde_json::Value = serde_json::from_slice(&out.stdout)?;
            for p in v["providers"].as_array().unwrap_or(&vec![]) {
                println!("  {}  —  {}", p["name"].as_str().unwrap_or("?"), p["description"].as_str().unwrap_or(""));
            }
        }
        Some("refresh") => {
            println!("Sources refreshed (TTL-expired caches cleared)");
            let cache_dir = crate::anyai_dir()?.join("cache/sources");
            let _ = std::fs::remove_dir_all(&cache_dir);
            let _ = std::fs::create_dir_all(&cache_dir);
        }
        Some("reset") => {
            merge_preset_sources()?;
            println!("Preset sources merged");
        }
        Some(unknown) => return Err(anyhow!("unknown sources subcommand: {unknown}")),
    }
    Ok(())
}

async fn cmd_providers(args: &[String]) -> Result<()> {
    match args.first().map(|s| s.as_str()) {
        None => {
            let config = load_config()?;
            let providers = config["providers"].as_array().cloned().unwrap_or_default();
            let active = config["active_provider"].as_str().unwrap_or("");
            for p in &providers {
                let name = p["name"].as_str().unwrap_or("?");
                let marker = if name == active { "*" } else { " " };
                println!(" {marker} {}  {}", name, p["url"].as_str().unwrap_or("?"));
            }
        }
        Some("add") => {
            let url = args.get(1).ok_or_else(|| anyhow!("usage: anyai providers add <url> --name <name>"))?;
            let name = flag_value(args, "--name").unwrap_or_else(|| url.clone());
            let source = flag_value(args, "--source");
            let mut config = load_config()?;
            let providers = config["providers"].as_array_mut()
                .ok_or_else(|| anyhow!("config missing providers"))?;
            if providers.iter().any(|p| p["name"].as_str() == Some(&name)) {
                for p in providers.iter_mut() {
                    if p["name"].as_str() == Some(&name) { p["url"] = serde_json::json!(url); }
                }
            } else {
                providers.push(serde_json::json!({ "name": name, "url": url, "source": source }));
            }
            save_config(&config)?;
            println!("Provider added: {name}");
        }
        Some("use") => {
            let name = args.get(1).ok_or_else(|| anyhow!("usage: anyai providers use <name>"))?;
            let mut config = load_config()?;
            let providers = config["providers"].as_array().cloned().unwrap_or_default();
            if !providers.iter().any(|p| p["name"].as_str() == Some(name)) {
                return Err(anyhow!("provider '{name}' not found"));
            }
            config["active_provider"] = serde_json::json!(name);
            save_config(&config)?;
            println!("Active provider: {name}");
        }
        Some("rm") => {
            let name = args.get(1).ok_or_else(|| anyhow!("usage: anyai providers rm <name>"))?;
            let mut config = load_config()?;
            if config["active_provider"].as_str() == Some(name) {
                return Err(anyhow!("cannot remove active provider; switch first with `anyai providers use`"));
            }
            if let Some(arr) = config["providers"].as_array_mut() {
                arr.retain(|p| p["name"].as_str() != Some(name));
            }
            save_config(&config)?;
            println!("Provider removed: {name}");
        }
        Some("show") => {
            let name = args.get(1);
            let config = load_config()?;
            let active = config["active_provider"].as_str().unwrap_or("");
            let target = name.map(|s| s.as_str()).unwrap_or(active);
            let providers = config["providers"].as_array().cloned().unwrap_or_default();
            let url = providers.iter()
                .find(|p| p["name"].as_str() == Some(target))
                .and_then(|p| p["url"].as_str())
                .ok_or_else(|| anyhow!("provider '{target}' not found"))?
                .to_string();
            let out = tokio::process::Command::new("curl")
                .args(["-sf", "--max-time", "10", &url])
                .output().await?;
            println!("{}", String::from_utf8_lossy(&out.stdout));
        }
        Some("reset") => {
            merge_preset_providers()?;
            println!("Preset providers merged");
        }
        Some(unknown) => return Err(anyhow!("unknown providers subcommand: {unknown}")),
    }
    Ok(())
}

async fn cmd_import(args: &[String]) -> Result<()> {
    let url_or_path = args.first().ok_or_else(|| anyhow!("usage: anyai import <url|path>"))?;
    let json_str = if url_or_path.starts_with("http://") || url_or_path.starts_with("https://") {
        let out = tokio::process::Command::new("curl")
            .args(["-sf", "--max-time", "10", url_or_path])
            .output().await?;
        String::from_utf8_lossy(&out.stdout).into_owned()
    } else {
        std::fs::read_to_string(url_or_path)?
    };

    let imported: serde_json::Value = serde_json::from_str(&json_str)
        .map_err(|_| anyhow!("invalid config JSON at {url_or_path}"))?;
    let mut config = load_config()?;

    // Merge sources by name
    if let Some(new_sources) = imported["sources"].as_array() {
        let existing = config["sources"].as_array_mut()
            .ok_or_else(|| anyhow!("config missing sources"))?;
        for s in new_sources {
            let name = s["name"].as_str().unwrap_or("");
            if !existing.iter().any(|e| e["name"].as_str() == Some(name)) {
                existing.push(s.clone());
                println!("+ source: {name}");
            }
        }
    }

    // Merge providers by name
    if let Some(new_providers) = imported["providers"].as_array() {
        let existing = config["providers"].as_array_mut()
            .ok_or_else(|| anyhow!("config missing providers"))?;
        for p in new_providers {
            let name = p["name"].as_str().unwrap_or("");
            if !existing.iter().any(|e| e["name"].as_str() == Some(name)) {
                existing.push(p.clone());
                println!("+ provider: {name}");
            }
        }
    }

    save_config(&config)?;
    println!("Import complete");
    Ok(())
}

async fn cmd_export(args: &[String]) -> Result<()> {
    let as_url = args.contains(&"--url".to_string());
    let sources_only = args.contains(&"--sources-only".to_string());
    let providers_only = args.contains(&"--providers-only".to_string());

    let config = load_config()?;
    let mut export = serde_json::json!({});
    if !providers_only { export["sources"] = config["sources"].clone(); }
    if !sources_only { export["providers"] = config["providers"].clone(); }

    if as_url {
        let encoded = base64_encode(&export.to_string());
        println!("anyai:import:{encoded}");
    } else {
        println!("{}", serde_json::to_string_pretty(&export)?);
    }
    Ok(())
}

// Config helpers

fn config_path() -> Result<std::path::PathBuf> {
    Ok(crate::anyai_dir()?.join("config.json"))
}

pub fn load_config() -> Result<serde_json::Value> {
    let path = config_path()?;
    if !path.exists() {
        return Ok(default_config());
    }
    let s = std::fs::read_to_string(&path)?;
    serde_json::from_str(&s).map_err(|e| anyhow!("invalid config.json: {e}"))
}

fn save_config(config: &serde_json::Value) -> Result<()> {
    let path = config_path()?;
    let _ = std::fs::create_dir_all(path.parent().unwrap());
    std::fs::write(&path, serde_json::to_string_pretty(config)?)?;
    Ok(())
}

fn default_config() -> serde_json::Value {
    serde_json::json!({
        "active_provider": "AnyAI Default",
        "active_mode": "text",
        "model_cleanup_days": 1,
        "kept_models": [],
        "mode_overrides": {},
        "sources": [
            { "name": "AnyAI", "url": "https://anyai.run/sources/index.json" }
        ],
        "providers": [
            { "name": "AnyAI Default", "url": "https://anyai.run/manifest/default.json", "source": "AnyAI" }
        ]
    })
}

fn merge_preset_sources() -> Result<()> {
    let preset_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../providers/preset-sources.json");
    if !preset_path.exists() { return Ok(()); }
    let preset: Vec<serde_json::Value> = serde_json::from_str(&std::fs::read_to_string(preset_path)?)?;
    let mut config = load_config()?;
    let sources = config["sources"].as_array_mut()
        .ok_or_else(|| anyhow!("config missing sources"))?;
    for s in preset {
        let name = s["name"].as_str().unwrap_or("").to_string();
        if !sources.iter().any(|e| e["name"].as_str() == Some(&name)) {
            sources.push(s);
        }
    }
    save_config(&config)
}

fn merge_preset_providers() -> Result<()> {
    let preset_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../providers/preset.json");
    if !preset_path.exists() { return Ok(()); }
    let preset: Vec<serde_json::Value> = serde_json::from_str(&std::fs::read_to_string(preset_path)?)?;
    let mut config = load_config()?;
    let providers = config["providers"].as_array_mut()
        .ok_or_else(|| anyhow!("config missing providers"))?;
    for p in preset {
        let name = p["name"].as_str().unwrap_or("").to_string();
        if !providers.iter().any(|e| e["name"].as_str() == Some(&name)) {
            providers.push(p);
        }
    }
    save_config(&config)
}

fn flag_value(args: &[String], flag: &str) -> Option<String> {
    args.windows(2)
        .find(|w| w[0] == flag)
        .map(|w| w[1].clone())
}

fn base64_encode(s: &str) -> String {
    // Simple URL-safe base64 using standard library.
    // Using a manual impl to avoid adding a dep.
    const CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let bytes = s.as_bytes();
    let mut out = String::new();
    let mut i = 0;
    while i < bytes.len() {
        let b0 = bytes[i] as u32;
        let b1 = bytes.get(i + 1).copied().unwrap_or(0) as u32;
        let b2 = bytes.get(i + 2).copied().unwrap_or(0) as u32;
        out.push(CHARS[((b0 >> 2) & 0x3f) as usize] as char);
        out.push(CHARS[(((b0 << 4) | (b1 >> 4)) & 0x3f) as usize] as char);
        if i + 1 < bytes.len() { out.push(CHARS[(((b1 << 2) | (b2 >> 6)) & 0x3f) as usize] as char); }
        if i + 2 < bytes.len() { out.push(CHARS[(b2 & 0x3f) as usize] as char); }
        i += 3;
    }
    out
}
