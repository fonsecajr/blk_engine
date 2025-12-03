use std::collections::HashMap;
use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use chrono::{Datelike, Local, Timelike};
use glob::Pattern;
use sha2::{Digest, Sha256};
use walkdir::WalkDir;
use zip::write::FileOptions;
use zip::DateTime as ZipDateTime;

use crate::models::{BlkConfig, DiffSummary, FileEntry, SetManifest};

// -----------------------------------------------------------------------------
// Time helpers
// -----------------------------------------------------------------------------

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

// -----------------------------------------------------------------------------
// Public helpers
// -----------------------------------------------------------------------------

pub fn get_snapshot_size(app_root: &Path, id: &str) -> u64 {
    let path = app_root
        .join(".blk")
        .join("snapshots")
        .join(format!("{id}.zip"));
    if let Ok(meta) = fs::metadata(path) {
        meta.len()
    } else {
        0
    }
}

pub fn format_bytes(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    const GB: u64 = MB * 1024;

    if bytes >= GB {
        format!("{:.2} GB", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.2} MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.2} KB", bytes as f64 / KB as f64)
    } else {
        format!("{bytes} B")
    }
}

// -----------------------------------------------------------------------------
// Safety & Filters
// -----------------------------------------------------------------------------

fn should_ignore(path: &Path) -> bool {
    let path_str = path.to_string_lossy().to_lowercase();
    let name = path
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_lowercase();

    // Proteção vital para não apagar o próprio sistema
    if path_str.contains(".blk")
        || name == "cargo.toml"
        || name == "cargo.lock"
        || name == "src"
        || path_str.contains("/src/")
        || path_str.contains("\\src\\")
        || name == "target"
        || path_str.contains("/target/")
        || path_str.contains("\\target\\")
        || path_str.contains(".git")
        || path_str.contains(".vscode")
    {
        return true;
    }

    if let Ok(exe_path) = std::env::current_exe() {
        if path == exe_path {
            return true;
        }
    }

    false
}

fn matches_exclusion(path: &Path, exclusions: &[String]) -> bool {
    if exclusions.is_empty() {
        return false;
    }
    let s = path.to_string_lossy().replace("\\", "/");

    for exc in exclusions {
        if let Ok(pat) = Pattern::new(exc) {
            if pat.matches(&s) {
                return true;
            }
        }
        if s.contains(exc) {
            return true;
        }
    }
    false
}

// -----------------------------------------------------------------------------
// ZIP helpers
// -----------------------------------------------------------------------------

fn create_zip_archive(archive_path: &Path, source_dir: &Path) -> Result<(), String> {
    let file = File::create(archive_path)
        .map_err(|e| format!("Failed to create zip file: {}", e))?;

    let mut zip = zip::ZipWriter::new(file);
    let walk_dir = WalkDir::new(source_dir);

    for entry in walk_dir.into_iter().filter_map(|e| e.ok()) {
        let path = entry.path();
        let name = path
            .strip_prefix(source_dir)
            .map_err(|e| format!("Path prefix error: {}", e))?
            .to_string_lossy()
            .replace("\\", "/");

        let meta = fs::metadata(path).ok();

        let zip_time = if let Some(ref m) = meta {
            if let Ok(mtime) = m.modified() {
                let dt: chrono::DateTime<Local> = mtime.into();
                match ZipDateTime::from_date_and_time(
                    dt.year() as u16,
                    dt.month() as u8,
                    dt.day() as u8,
                    dt.hour() as u8,
                    dt.minute() as u8,
                    dt.second() as u8,
                ) {
                    Ok(zt) => zt,
                    Err(_) => ZipDateTime::default_for_write(),
                }
            } else {
                ZipDateTime::default_for_write()
            }
        } else {
            ZipDateTime::default_for_write()
        };

        if path.is_dir() {
            if !name.is_empty() {
                let options = FileOptions::<()>::default()
                    .compression_method(zip::CompressionMethod::Stored)
                    .unix_permissions(0o755)
                    .last_modified_time(zip_time);
                zip.add_directory(&name, options)
                    .map_err(|e| format!("Zip dir error: {}", e))?;
            }
        } else {
            let len = meta.as_ref().map(|m| m.len()).unwrap_or(0);
            let options = FileOptions::<()>::default()
                .compression_method(zip::CompressionMethod::Zstd)
                .unix_permissions(0o755)
                .large_file(len > 0xffffffff)
                .last_modified_time(zip_time);

            zip.start_file(&name, options)
                .map_err(|e| format!("Zip start file error: {}", e))?;

            let mut f = File::open(path)
                .map_err(|e| format!("Failed to open file {:?}: {}", path, e))?;

            io::copy(&mut f, &mut zip)
                .map_err(|e| format!("Write zip error (streaming): {}", e))?;
        }
    }

    zip.finish()
        .map_err(|e| format!("Failed to finalize zip: {}", e))?;

    Ok(())
}

fn extract_zip_archive(archive_path: &Path, dest_dir: &Path) -> Result<(), String> {
    let file = File::open(archive_path).map_err(|e| format!("Failed to open zip: {}", e))?;

    let mut archive = zip::ZipArchive::new(file)
        .map_err(|e| format!("Failed to read zip archive: {}", e))?;

    for i in 0..archive.len() {
        let mut file = archive
            .by_index(i)
            .map_err(|e| format!("Zip index error: {}", e))?;

        let outpath = match file.enclosed_name() {
            Some(path) => dest_dir.join(path),
            None => continue,
        };

        if file.name().ends_with('/') {
            fs::create_dir_all(&outpath)
                .map_err(|e| format!("Failed to create dir {:?}: {}", outpath, e))?;
        } else {
            if let Some(p) = outpath.parent() {
                if !p.exists() {
                    fs::create_dir_all(p)
                        .map_err(|e| format!("Failed to create parent dir: {}", e))?;
                }
            }
            
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                if let Some(mode) = file.unix_mode() {
                    let _ = fs::set_permissions(&outpath, fs::Permissions::from_mode(mode));
                }
            }

            let mut outfile = File::create(&outpath)
                .map_err(|e| format!("Failed to create file {:?}: {}", outpath, e))?;

            io::copy(&mut file, &mut outfile)
                .map_err(|e| format!("Failed to extract file: {}", e))?;
        }
    }

    Ok(())
}

// -----------------------------------------------------------------------------
// NUCLEAR WIPE HELPER
// -----------------------------------------------------------------------------

fn nuke_scopes(config: &BlkConfig, tx: &mpsc::Sender<(f32, String)>) -> usize {
    let mut deleted_count = 0;
    
    // Itera por todos os caminhos configurados (Scopes)
    for (scope_name, root) in &config.path_map {
        if !root.exists() { continue; }

        tx.send((0.0, format!("Nuking scope: {}...", scope_name))).ok();
        
        // Pega itens de nível superior para não apagar a pasta raiz em si, apenas conteúdo
        if let Ok(read_dir) = fs::read_dir(root) {
            for entry in read_dir.flatten() {
                let path = entry.path();
                
                // CRÍTICO: Não apagar .blk, .git, etc.
                if should_ignore(&path) { 
                    continue; 
                }

                if path.is_dir() {
                    if fs::remove_dir_all(&path).is_ok() {
                        deleted_count += 1;
                    }
                } else {
                    if fs::remove_file(&path).is_ok() {
                        deleted_count += 1;
                    }
                }
            }
        }
    }
    deleted_count
}

fn prune_empty_dirs(config: &BlkConfig) {
    for _ in 0..3 {
        let mut changes = false;
        
        for (_scope_name, root) in &config.path_map {
            if !root.exists() { continue; }

            for entry in WalkDir::new(root).contents_first(true).into_iter().filter_map(|e| e.ok()) {
                let path = entry.path();
                
                if path.is_dir() {
                    if path == root { continue; }
                    if should_ignore(path) { continue; }

                    if fs::remove_dir(path).is_ok() {
                        changes = true;
                    } else {
                        // Tenta remover se tiver so lixo
                        if let Ok(iter) = fs::read_dir(path) {
                            let items: Vec<_> = iter.filter_map(|e| e.ok()).collect();
                            if !items.is_empty() && items.len() <= 2 {
                                let all_junk = items.iter().all(|i| {
                                    let n = i.file_name().to_string_lossy().to_lowercase();
                                    n == "thumbs.db" || n == ".ds_store" || n == "desktop.ini"
                                });

                                if all_junk {
                                    let _ = fs::remove_dir_all(path);
                                    changes = true;
                                }
                            }
                        }
                    }
                }
            }
        }
        if !changes { break; }
    }
}

// -----------------------------------------------------------------------------
// Hashing & Lazy Scan
// -----------------------------------------------------------------------------

fn hash_file(path: &Path) -> String {
    let mut file = match File::open(path) {
        Ok(f) => f,
        Err(_) => return String::new(),
    };
    let mut hasher = Sha256::new();
    if io::copy(&mut file, &mut hasher).is_err() {
        return String::new();
    }
    hex::encode(hasher.finalize())
}

fn scan_state(
    config: &BlkConfig,
    scopes: &[String],
    exclusions: &[String],
    previous_baseline: Option<&HashMap<String, FileEntry>>,
) -> HashMap<String, FileEntry> {
    let mut map = HashMap::new();

    for scope_name in scopes {
        if let Some(root) = config.path_map.get(scope_name) {
            if !root.exists() {
                continue;
            }
            for entry in WalkDir::new(root) {
                let entry = match entry {
                    Ok(e) => e,
                    Err(_) => continue,
                };
                let path = entry.path();

                if should_ignore(path) {
                    continue;
                }
                if matches_exclusion(path, exclusions) {
                    continue;
                }

                if path.is_file() {
                    let rel = path
                        .strip_prefix(root)
                        .unwrap_or(path)
                        .to_string_lossy()
                        .replace("\\", "/");
                    let key = format!("{}::{}", scope_name, rel);

                    let (size, modified) = if let Ok(meta) = fs::metadata(path) {
                        let m = meta
                            .modified()
                            .unwrap_or(UNIX_EPOCH)
                            .duration_since(UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_secs();
                        (meta.len(), m)
                    } else {
                        (0, 0)
                    };

                    let hash = if let Some(prev_map) = previous_baseline {
                        if let Some(old_entry) = prev_map.get(&key) {
                            if old_entry.size == size && old_entry.modified == modified {
                                old_entry.hash.clone()
                            } else {
                                hash_file(path)
                            }
                        } else {
                            hash_file(path)
                        }
                    } else {
                        hash_file(path)
                    };

                    map.insert(
                        key,
                        FileEntry {
                            hash,
                            size,
                            modified,
                        },
                    );
                }
            }
        }
    }
    map
}

fn save_baseline(app_root: &Path, map: &HashMap<String, FileEntry>) {
    let path = app_root.join(".blk").join("baseline.json");
    let json = serde_json::to_string(map).unwrap_or("{}".into());
    let _ = fs::write(path, json);
}

fn load_baseline(app_root: &Path) -> HashMap<String, FileEntry> {
    let path = app_root.join(".blk").join("baseline.json");
    if !path.exists() {
        return HashMap::new();
    }
    let txt = fs::read_to_string(path).unwrap_or("{}".into());
    serde_json::from_str(&txt).unwrap_or_default()
}

// -----------------------------------------------------------------------------
// Auto-init
// -----------------------------------------------------------------------------

pub fn engine_auto_init(app_root: &Path, tx: mpsc::Sender<(f32, String)>) {
    tx.send((10.0, "Creating .blk structure (v3.0 Nuke)...".into()))
        .ok();

    let blk = app_root.join(".blk");
    if blk.exists() {
        tx.send((100.0, "Already initialized.".into())).ok();
        return;
    }

    fs::create_dir_all(blk.join("sets")).ok();
    fs::create_dir_all(blk.join("snapshots")).ok();

    let mut path_map = HashMap::new();
    path_map.insert("Root".to_string(), app_root.to_path_buf());
    let config = BlkConfig { path_map };
    let cfg_json = serde_json::to_string_pretty(&config).unwrap();
    fs::write(blk.join("config.json"), cfg_json).ok();

    let vanilla = SetManifest {
        id: "vanilla".into(),
        name: "Vanilla".into(),
        parent_id: None,
        scopes: vec!["Root".into()],
        exclusions: vec![],
        created_at: now_unix(),
        deleted_paths: vec![],
    };
    let van_json = serde_json::to_string_pretty(&vanilla).unwrap();
    fs::write(blk.join("sets").join("vanilla.json"), van_json).ok();

    tx.send((40.0, "Creating vanilla snapshot...".into())).ok();
    let staging = blk.join("staging_vanilla");
    if staging.exists() {
        let _ = fs::remove_dir_all(&staging);
    }
    fs::create_dir_all(&staging).ok();

    for entry in WalkDir::new(app_root) {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };
        let path = entry.path();
        if should_ignore(path) {
            continue;
        }
        if path.is_file() {
            if let Ok(rel) = path.strip_prefix(app_root) {
                let dest = staging.join(rel);
                if let Some(parent) = dest.parent() {
                    fs::create_dir_all(parent).ok();
                }
                let _ = fs::copy(path, dest);
            }
        }
    }

    let vanilla_zip = blk.join("snapshots").join("vanilla.zip");
    if let Err(e) = create_zip_archive(&vanilla_zip, &staging) {
        tx.send((100.0, format!("❌ Zip error: {e}"))).ok();
        return;
    }
    let _ = fs::remove_dir_all(&staging);

    tx.send((80.0, "Building baseline...".into())).ok();
    let state = scan_state(&config, &vec!["Root".into()], &vec![], None);
    save_baseline(app_root, &state);

    tx.send((100.0, "✅ Initialization complete.".into())).ok();
}

// -----------------------------------------------------------------------------
// Config update
// -----------------------------------------------------------------------------

pub fn engine_update_global_path(app_root: &Path, key: String, path: String) {
    let config_path = app_root.join(".blk").join("config.json");
    let mut config: BlkConfig = if config_path.exists() {
        serde_json::from_str(&fs::read_to_string(&config_path).unwrap_or("{}".into()))
            .unwrap_or_default()
    } else {
        BlkConfig::default()
    };
    config.path_map.insert(key, PathBuf::from(path));
    let json = serde_json::to_string_pretty(&config).unwrap();
    let _ = fs::write(config_path, json);
}

pub fn engine_update_manifest(
    app_root: &Path,
    id: String,
    scopes: Vec<String>,
    exclusions: Vec<String>,
    tx: mpsc::Sender<(f32, String)>,
) {
    tx.send((0.0, "Saving config...".into())).ok();
    let path = app_root.join(".blk").join("sets").join(format!("{id}.json"));

    if let Ok(txt) = fs::read_to_string(&path) {
        if let Ok(mut man) = serde_json::from_str::<SetManifest>(&txt) {
            man.scopes = scopes;
            man.exclusions = exclusions;
            if let Ok(json) = serde_json::to_string_pretty(&man) {
                fs::write(&path, json).ok();
                tx.send((100.0, "✅ Config saved!".into())).ok();
                return;
            }
        }
    }
    tx.send((100.0, "Config error.".into())).ok();
}

// -----------------------------------------------------------------------------
// Diff checking
// -----------------------------------------------------------------------------

pub fn engine_check_changes(
    app_root: &Path,
    config: BlkConfig,
    scopes: Vec<String>,
    exclusions: Vec<String>,
    tx: mpsc::Sender<DiffSummary>,
) {
    let baseline = load_baseline(app_root);
    let current = scan_state(&config, &scopes, &exclusions, Some(&baseline));

    let mut diff = DiffSummary::default();
    for (key, new_entry) in &current {
        match baseline.get(key) {
            Some(old_entry) => {
                if new_entry.hash != old_entry.hash {
                    diff.modified_files += 1;
                }
            }
            None => diff.new_files += 1,
        }
    }
    for key in baseline.keys() {
        if !current.contains_key(key) {
            diff.deleted_files += 1;
        }
    }
    diff.is_dirty =
        diff.new_files > 0 || diff.modified_files > 0 || diff.deleted_files > 0;
    tx.send(diff).ok();
}

// -----------------------------------------------------------------------------
// Restore chain (NUCLEAR WIPE + REBUILD)
// -----------------------------------------------------------------------------

pub fn engine_restore_chain(
    app_root: &Path,
    config: BlkConfig,
    ids: Vec<String>,
    scopes: Vec<String>,
    exclusions: Vec<String>,
    tx: mpsc::Sender<(f32, String)>,
) {
    // 1. NUCLEAR WIPE
    tx.send((0.0, "☢ NUCLEAR WIPE INITIATED ☢".into())).ok();
    thread::sleep(Duration::from_millis(500)); // Dramatic pause/safety
    let items_removed = nuke_scopes(&config, &tx);
    tx.send((10.0, format!("Wiped {} items. Starting rebuild...", items_removed))).ok();

    // 2. RECONSTRUCTION
    let total_layers = ids.len().max(1) as f32;

    for (idx, id) in ids.iter().enumerate() {
        let label = format!("Unpacking Layer {}/{} ({})", idx + 1, ids.len(), id);
        tx.send((10.0 + ((idx as f32 / total_layers) * 80.0), label.clone())).ok();

        let archive = app_root
            .join(".blk")
            .join("snapshots")
            .join(format!("{id}.zip"));

        if archive.exists() {
            // Extração temporária para separar scopes
            let extract_root = app_root.join(".blk").join("tmp_extract").join(id);
            if extract_root.exists() {
                let _ = fs::remove_dir_all(&extract_root);
            }
            fs::create_dir_all(&extract_root).ok();

            if let Err(e) = extract_zip_archive(&archive, &extract_root) {
                tx.send((100.0, format!("❌ Extract error for {id}: {e}")))
                    .ok();
                continue;
            }

            // Move files to destination based on SCOPE prefix
            for entry in WalkDir::new(&extract_root) {
                let entry = match entry {
                    Ok(e) => e,
                    Err(_) => continue,
                };
                let path = entry.path();
                if path.is_dir() {
                    continue;
                }

                // Arquivo no zip: "ScopeName/Path/To/File.txt"
                let rel = match path.strip_prefix(&extract_root) {
                    Ok(r) => r,
                    Err(_) => continue,
                };
                let mut it = rel.iter();
                let scope_os = match it.next() {
                    Some(c) => c,
                    None => continue,
                };
                let scope_name = scope_os.to_string_lossy().to_string();
                let rest: PathBuf = it.collect();

                // Descobre destino real
                let out_path_opt = if let Some(base) = config.path_map.get(&scope_name) {
                    Some(base.join(&rest))
                } else if let Some(root_base) = config.path_map.get("Root") {
                    // Fallback para raiz antiga
                    Some(root_base.join(&rel))
                } else {
                    None
                };

                if let Some(out_path) = out_path_opt {
                    if let Some(parent) = out_path.parent() {
                        fs::create_dir_all(parent).ok();
                    }
                    // Copy overwrites because we are layering up
                    let _ = fs::copy(path, &out_path);
                }
            }
            let _ = fs::remove_dir_all(&extract_root);
        }

        // Apply manifest specific deletions (files deleted in this delta)
        let manifest_path = app_root
            .join(".blk")
            .join("sets")
            .join(format!("{id}.json"));
        if let Ok(content) = fs::read_to_string(&manifest_path) {
            if let Ok(man) = serde_json::from_str::<SetManifest>(&content) {
                if !man.deleted_paths.is_empty() {
                    for del_key in man.deleted_paths {
                        if let Some((scope, rel)) = del_key.split_once("::") {
                            if let Some(base) = config.path_map.get(scope) {
                                let target = base.join(rel);
                                if target.exists() {
                                    let _ = fs::remove_file(target);
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    tx.send((95.0, "Pruning empty directories...".into())).ok();
    prune_empty_dirs(&config);

    tx.send((99.0, "Regenerating baseline...".into())).ok();
    let state = scan_state(&config, &scopes, &exclusions, None);
    save_baseline(app_root, &state);
    tx.send((100.0, "✅ Restore completed (Nuclear Clean)!".into())).ok();
}

// -----------------------------------------------------------------------------
// Save new delta
// -----------------------------------------------------------------------------

pub fn engine_save_new_delta(
    app_root: &Path,
    config: BlkConfig,
    name: String,
    parent_id: Option<String>,
    scopes: Vec<String>,
    exclusions: Vec<String>,
    tx: mpsc::Sender<(f32, String)>,
) {
    tx.send((0.0, "Analyzing changes...".into())).ok();

    let baseline = load_baseline(app_root);
    let id = name
        .to_lowercase()
        .replace(' ', "_")
        .replace('/', "")
        .replace('\\', "");

    // Identifica arquivos deletados em relação ao baseline anterior
    let mut deleted_paths = Vec::new();
    for (key, _) in &baseline {
        if let Some((scope, rel_path)) = key.split_once("::") {
            if scopes.iter().any(|s| s == scope) {
                if let Some(root_path) = config.path_map.get(scope) {
                    let real_path = root_path.join(rel_path);
                    if !real_path.exists() {
                        deleted_paths.push(key.clone());
                    }
                }
            }
        }
    }

    let manifest = SetManifest {
        id: id.clone(),
        name,
        parent_id,
        scopes: scopes.clone(),
        exclusions: exclusions.clone(),
        created_at: now_unix(),
        deleted_paths: deleted_paths.clone(),
    };

    let sets_dir = app_root.join(".blk").join("sets");
    if let Ok(json) = serde_json::to_string_pretty(&manifest) {
        if let Err(e) = fs::write(sets_dir.join(format!("{id}.json")), json) {
            tx.send((100.0, format!("Error writing JSON: {e}"))).ok();
            return;
        }
    }

    let staging_root = app_root.join(".blk").join("staging").join(&id);
    if staging_root.exists() {
        let _ = fs::remove_dir_all(&staging_root);
    }
    fs::create_dir_all(&staging_root).ok();

    let mut files_included = 0;

    for scope_name in &scopes {
        if let Some(root) = config.path_map.get(scope_name) {
            if !root.exists() {
                continue;
            }
            let scope_dest = staging_root.join(scope_name);

            for entry in WalkDir::new(root) {
                let entry = match entry {
                    Ok(e) => e,
                    Err(_) => continue,
                };
                let path = entry.path();

                if should_ignore(path) {
                    continue;
                }
                if matches_exclusion(path, &exclusions) {
                    continue;
                }

                if path.is_file() {
                    let rel_key = path
                        .strip_prefix(root)
                        .unwrap_or(path)
                        .to_string_lossy()
                        .replace("\\", "/");
                    let key = format!("{}::{}", scope_name, rel_key);

                    let current_hash = hash_file(path);

                    let is_modified = match baseline.get(&key) {
                        Some(old_entry) => old_entry.hash != current_hash,
                        None => true,
                    };

                    // Salva se mudou ou é novo
                    if is_modified {
                        if let Ok(rel) = path.strip_prefix(root) {
                            let dest = scope_dest.join(rel);
                            if let Some(parent) = dest.parent() {
                                fs::create_dir_all(parent).ok();
                            }
                            let _ = fs::copy(path, dest);
                            files_included += 1;
                        }
                    }
                }
            }
        }
    }

    tx.send((
        40.0,
        format!("Compressing {} files...", files_included),
    ))
    .ok();

    let archive = app_root
        .join(".blk")
        .join("snapshots")
        .join(format!("{id}.zip"));
    if let Err(e) = create_zip_archive(&archive, &staging_root) {
        tx.send((100.0, format!("Compression error: {e}"))).ok();
        return;
    }

    let _ = fs::remove_dir_all(&staging_root);

    tx.send((90.0, "Updating baseline...".into())).ok();
    let state = scan_state(&config, &scopes, &exclusions, Some(&baseline));
    save_baseline(app_root, &state);

    tx.send((
        100.0,
        format!(
            "Saved: {id} (+{}, -{})",
            files_included,
            deleted_paths.len()
        ),
    ))
    .ok();
}

pub fn engine_delete_cascade(
    app_root: &Path,
    target_id: String,
    all_manifests: &Vec<SetManifest>,
    tx: mpsc::Sender<(f32, String)>,
) {
    tx.send((0.0, "Mapping cascade delete...".into())).ok();
    let mut to_delete = Vec::new();
    let mut queue = vec![target_id.clone()];
    let mut idx = 0;
    while idx < queue.len() {
        let current = queue[idx].clone();
        to_delete.push(current.clone());
        for man in all_manifests {
            if let Some(parent) = &man.parent_id {
                if parent == &current && !queue.contains(&man.id) {
                    queue.push(man.id.clone());
                }
            }
        }
        idx += 1;
    }
    let total = to_delete.len().max(1) as f32;
    for (i, id) in to_delete.iter().enumerate() {
        let label = format!("Deleting {}/{} ({id})", i + 1, to_delete.len());
        let pct = (i as f32 / total) * 100.0;
        tx.send((pct, label)).ok();
        let json = app_root
            .join(".blk")
            .join("sets")
            .join(format!("{id}.json"));
        if json.exists() {
            let _ = fs::remove_file(json);
        }
        let archive_zip = app_root
            .join(".blk")
            .join("snapshots")
            .join(format!("{id}.zip"));
        if archive_zip.exists() {
            let _ = fs::remove_file(archive_zip);
        }
        thread::sleep(Duration::from_millis(50));
    }
    tx.send((100.0, format!("✅ {} sets deleted.", total))).ok();
}