mod models;
mod engine;

use std::collections::HashMap;
use std::fs;
use std::io;
use std::panic;
use std::path::PathBuf;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Gauge, List, ListItem, ListState, Paragraph, Wrap},
    Terminal,
};

use models::{BlkConfig, DiffSummary, SetManifest};
use engine::{
    engine_auto_init, engine_check_changes, engine_delete_cascade,
    engine_restore_chain, engine_save_new_delta,
    engine_update_global_path, engine_update_manifest,
    format_bytes, get_snapshot_size,
};

enum InputMode {
    Normal,
    EditingName,
    ConfirmDelete,
    Configuring,
    AddingPath,
    Initializing,
}

enum ConfigFocus {
    Scopes,
    Exclusions,
}

struct App {
    config: BlkConfig,
    app_root: PathBuf,

    items: Vec<String>,
    ids: Vec<String>,
    manifests_cache: HashMap<String, SetManifest>,

    state: ListState,

    is_processing: bool,
    progress: u16,
    status_msg: String,
    
    // Contador para animação do spinner
    spinner_tick: u64,

    input_text: String,
    input_mode: InputMode,

    delete_target_id: String,
    delete_warning_msg: String,

    config_target_id: String,
    config_temp_scopes: Vec<String>,
    config_temp_exclusions: Vec<String>,
    config_focus: ConfigFocus,
    config_state: ListState,

    tree_scroll: u16,
    pending_save_after_config: bool,

    diff_summary: DiffSummary,
    active_set_id: Option<String>,

    receiver: Option<mpsc::Receiver<(f32, String)>>,
    diff_receiver: Option<mpsc::Receiver<DiffSummary>>,
    reload_needed: bool,
    init_thread_spawned: bool,
}

impl App {
    fn new() -> Self {
        let app_root = std::env::current_dir().unwrap();
        let blk_path = app_root.join(".blk");

        if !blk_path.exists() {
            return App {
                config: BlkConfig::default(),
                app_root,
                items: vec![],
                ids: vec![],
                manifests_cache: HashMap::new(),
                state: ListState::default(),
                is_processing: true,
                progress: 0,
                status_msg: "Initializing...".into(),
                spinner_tick: 0,
                input_text: String::new(),
                input_mode: InputMode::Initializing,
                delete_target_id: String::new(),
                delete_warning_msg: String::new(),
                config_target_id: String::new(),
                config_temp_scopes: vec![],
                config_temp_exclusions: vec![],
                config_focus: ConfigFocus::Scopes,
                config_state: ListState::default(),
                tree_scroll: 0,
                pending_save_after_config: false,
                diff_summary: DiffSummary::default(),
                active_set_id: None,
                receiver: None,
                diff_receiver: None,
                reload_needed: false,
                init_thread_spawned: false,
            };
        }

        App::load_initial_state(app_root)
    }

    fn load_initial_state(app_root: PathBuf) -> Self {
        let config_path = app_root.join(".blk").join("config.json");
        let config: BlkConfig = if config_path.exists() {
            let txt = fs::read_to_string(config_path).unwrap_or_else(|_| "{}".into());
            serde_json::from_str(&txt).unwrap_or_default()
        } else {
            BlkConfig::default()
        };

        let sets_root = app_root.join(".blk").join("sets");
        let mut cache = HashMap::new();
        let mut list: Vec<SetManifest> = Vec::new();

        if let Ok(entries) = fs::read_dir(sets_root) {
            for entry in entries.flatten() {
                if entry.path().extension().map_or(false, |e| e == "json") {
                    if let Ok(txt) = fs::read_to_string(entry.path()) {
                        if let Ok(man) = serde_json::from_str::<SetManifest>(&txt) {
                            cache.insert(man.id.clone(), man.clone());
                            list.push(man);
                        }
                    }
                }
            }
        }

        list.sort_by(|a, b| b.created_at.cmp(&a.created_at));

        let mut items = Vec::new();
        let mut ids = Vec::new();

        for m in list {
            items.push(m.name);
            ids.push(m.id);
        }

        if items.is_empty() {
            items.push("BLK Zero Set".into());
            ids.push("".into());
        }

        let mut app = App {
            config,
            app_root,
            items,
            ids,
            manifests_cache: cache,
            state: ListState::default(),
            is_processing: false,
            progress: 0,
            status_msg: "Ready. [F5] Check modifications.".into(),
            spinner_tick: 0,
            input_text: String::new(),
            input_mode: InputMode::Normal,
            delete_target_id: String::new(),
            delete_warning_msg: String::new(),
            config_target_id: String::new(),
            config_temp_scopes: vec![],
            config_temp_exclusions: vec![],
            config_focus: ConfigFocus::Scopes,
            config_state: ListState::default(),
            tree_scroll: 0,
            pending_save_after_config: false,
            diff_summary: DiffSummary::default(),
            active_set_id: None,
            receiver: None,
            diff_receiver: None,
            reload_needed: false,
            init_thread_spawned: true,
        };

        app.state.select(Some(0));
        app
    }

    fn refresh_list(&mut self) {
        let new = App::load_initial_state(self.app_root.clone());
        self.items = new.items;
        self.ids = new.ids;
        self.manifests_cache = new.manifests_cache;
        self.config = new.config;
        self.state.select(Some(0));
        self.tree_scroll = 0;
        self.reload_needed = false;
        self.input_mode = InputMode::Normal;
    }

    fn trigger_auto_init(&mut self) {
        self.init_thread_spawned = true;
        let root = self.app_root.clone();
        let (tx, rx) = mpsc::channel();
        self.receiver = Some(rx);

        thread::spawn(move || {
            engine_auto_init(&root, tx);
        });
    }

    fn next(&mut self) {
        let i = match self.state.selected() {
            Some(i) => {
                if i >= self.items.len() - 1 {
                    0
                } else {
                    i + 1
                }
            }
            None => 0,
        };
        self.state.select(Some(i));
        self.tree_scroll = 0;
    }

    fn previous(&mut self) {
        let i = match self.state.selected() {
            Some(i) => {
                if i == 0 {
                    self.items.len() - 1
                } else {
                    i - 1
                }
            }
            None => 0,
        };
        self.state.select(Some(i));
        self.tree_scroll = 0;
    }

    fn resolve_dependencies(&self, target_id: &str) -> Vec<String> {
        let mut stack = Vec::new();
        let mut cursor = Some(target_id.to_string());

        while let Some(id) = cursor {
            if let Some(man) = self.manifests_cache.get(&id) {
                stack.push(id.clone());
                cursor = man.parent_id.clone();
            } else {
                break;
            }
        }

        stack.reverse();
        stack
    }

    fn get_children(&self, parent_id: &str) -> Vec<String> {
        self.manifests_cache
            .values()
            .filter(|m| m.parent_id.as_deref() == Some(parent_id))
            .map(|m| m.id.clone())
            .collect()
    }

    // --------------------- config view / editing ---------------------------

    fn start_config_viewer(&mut self) {
        let idx = self.state.selected().unwrap_or(0);
        if idx >= self.ids.len() {
            return;
        }
        let id = self.ids[idx].clone();
        if id.is_empty() {
            return;
        }

        if let Some(man) = self.manifests_cache.get(&id) {
            self.config_target_id = id;
            self.config_temp_scopes = man.scopes.clone();
            self.config_temp_exclusions = man.exclusions.clone();
            self.config_focus = ConfigFocus::Scopes;
            self.config_state.select(if self.config_temp_scopes.is_empty() {
                None
            } else {
                Some(0)
            });
            self.input_mode = InputMode::Configuring;
        }
    }

    fn config_toggle_focus(&mut self) {
        match self.config_focus {
            ConfigFocus::Scopes => {
                self.config_focus = ConfigFocus::Exclusions;
                self.config_state.select(if self.config_temp_exclusions.is_empty() {
                    None
                } else {
                    Some(0)
                });
            }
            ConfigFocus::Exclusions => {
                self.config_focus = ConfigFocus::Scopes;
                self.config_state.select(if self.config_temp_scopes.is_empty() {
                    None
                } else {
                    Some(0)
                });
            }
        }
    }

    fn config_next_item(&mut self) {
        let len = match self.config_focus {
            ConfigFocus::Scopes => self.config_temp_scopes.len(),
            ConfigFocus::Exclusions => self.config_temp_exclusions.len(),
        };
        if len == 0 {
            return;
        }
        let i = match self.config_state.selected() {
            Some(i) => {
                if i >= len - 1 {
                    0
                } else {
                    i + 1
                }
            }
            None => 0,
        };
        self.config_state.select(Some(i));
    }

    fn config_start_add(&mut self) {
        self.input_text.clear();
        self.input_mode = InputMode::AddingPath;
    }

    fn config_confirm_add(&mut self) {
        let entry = self.input_text.trim().to_string();
        if !entry.is_empty() {
            match self.config_focus {
                ConfigFocus::Scopes => {
                    // CORREÇÃO: Lógica para aceitar "Key=Path" OU apenas "Path"
                    let (key_clean, path_clean) = if let Some((key, path)) = entry.split_once('=') {
                        (key.trim().to_string(), path.trim().to_string())
                    } else {
                        // Trata caminho puro
                        let raw = entry.trim_matches('"').to_string();
                        if raw.contains(':') || raw.contains('\\') || raw.contains('/') {
                            // É um caminho. Extrair nome da pasta para ser a chave.
                            let pb = PathBuf::from(&raw);
                            let folder_name = pb.file_name()
                                .and_then(|n| n.to_str())
                                .map(|s| s.replace(" ", "_")) // Sanitiza espaços
                                .unwrap_or_else(|| "Extra".to_string());
                            
                            (folder_name, raw)
                        } else {
                            // É apenas uma chave existente ou nome lógico
                            (raw.clone(), String::new()) 
                        }
                    };

                    // Se temos um caminho, atualiza o mapa global
                    if !path_clean.is_empty() {
                         engine_update_global_path(
                            &self.app_root,
                            key_clean.clone(),
                            path_clean.clone(),
                        );
                        self.config
                            .path_map
                            .insert(key_clean.clone(), PathBuf::from(path_clean));
                    }
                    
                    // IMPORTANTE: Adiciona apenas ao escopo local do manifesto sendo editado
                    self.config_temp_scopes.push(key_clean);
                }
                ConfigFocus::Exclusions => self.config_temp_exclusions.push(entry),
            }
        }

        self.input_text.clear();
        self.input_mode = InputMode::Configuring;
        let new_len = match self.config_focus {
            ConfigFocus::Scopes => self.config_temp_scopes.len(),
            ConfigFocus::Exclusions => self.config_temp_exclusions.len(),
        };
        self.config_state
            .select(Some(if new_len > 0 { new_len - 1 } else { 0 }));
    }

    fn config_delete_selected(&mut self) {
        match self.config_focus {
            ConfigFocus::Scopes => {
                if let Some(i) = self.config_state.selected() {
                    if i < self.config_temp_scopes.len() {
                        self.config_temp_scopes.remove(i);
                        self.config_state.select(if self.config_temp_scopes.is_empty() {
                            None
                        } else {
                            Some(if i == 0 { 0 } else { i - 1 })
                        });
                    }
                }
            }
            ConfigFocus::Exclusions => {
                if let Some(i) = self.config_state.selected() {
                    if i < self.config_temp_exclusions.len() {
                        self.config_temp_exclusions.remove(i);
                        self.config_state.select(if self.config_temp_exclusions.is_empty()
                        {
                            None
                        } else {
                            Some(if i == 0 { 0 } else { i - 1 })
                        });
                    }
                }
            }
        }
    }

    fn config_save_and_exit(&mut self) {
        let triggered_by_save = self.pending_save_after_config;
        self.pending_save_after_config = false;

        self.input_mode = if triggered_by_save {
            InputMode::EditingName
        } else {
            InputMode::Normal
        };

        self.is_processing = true;
        self.status_msg = "Saving configuration...".into();
        self.reload_needed = !triggered_by_save;

        let id = self.config_target_id.clone();
        let scopes = self.config_temp_scopes.clone();
        let exc = self.config_temp_exclusions.clone();

        if let Some(m) = self.manifests_cache.get_mut(&id) {
            m.scopes = scopes.clone();
            m.exclusions = exc.clone();
        }

        let (tx, rx) = mpsc::channel();
        self.receiver = Some(rx);
        let root = self.app_root.clone();

        thread::spawn(move || {
            engine_update_manifest(&root, id, scopes, exc, tx);
        });
    }

    // ------------------------ diff checking --------------------------------

    fn check_dir_status(&mut self) {
        let (scopes, exclusions) = match &self.active_set_id {
            Some(id) => {
                if let Some(m) = self.manifests_cache.get(id) {
                    (m.scopes.clone(), m.exclusions.clone())
                } else {
                    // Fallback apenas se não achar o manifesto
                    (self.config.path_map.keys().cloned().collect(), vec![])
                }
            }
            // Se nenhum set está ativo (inicialização), pega tudo.
            None => (self.config.path_map.keys().cloned().collect(), vec![]),
        };

        let (tx, rx) = mpsc::channel();
        self.diff_receiver = Some(rx);

        let cfg = self.config.clone();
        let root = self.app_root.clone();
        self.status_msg = "Checking modifications...".into();

        thread::spawn(move || {
            engine_check_changes(&root, cfg, scopes, exclusions, tx);
        });
    }

    // ---------------------------- actions ----------------------------------

    fn action_restore(&mut self) {
        let idx = self.state.selected().unwrap_or(0);
        if idx >= self.ids.len() {
            return;
        }
        let id = self.ids[idx].clone();
        if id.is_empty() {
            return;
        }

        self.active_set_id = Some(id.clone());
        let man = self.manifests_cache.get(&id).unwrap();
        let scopes = man.scopes.clone();
        let exclusions = man.exclusions.clone();

        let chain = self.resolve_dependencies(&id);
        let ids_clone = chain.clone();

        self.is_processing = true;
        self.progress = 0;
        self.status_msg = format!("Restoring {} layers...", chain.len());

        let (tx, rx) = mpsc::channel();
        self.receiver = Some(rx);
        let cfg = self.config.clone();
        let root = self.app_root.clone();

        thread::spawn(move || {
            engine_restore_chain(&root, cfg, ids_clone, scopes, exclusions, tx);
        });
    }

    fn action_save(&mut self) {
        let name = self.input_text.clone();
        if name.trim().is_empty() {
            return;
        }

        let idx = self.state.selected().unwrap_or(0);
        let current_id = if idx < self.ids.len() {
            self.ids[idx].clone()
        } else {
            String::new()
        };
        let parent_id = if current_id.is_empty() {
            None
        } else {
            Some(current_id.clone())
        };

        let (scopes, exclusions) = if let Some(man) = self.manifests_cache.get(&current_id) {
            (man.scopes.clone(), man.exclusions.clone())
        } else {
            (self.config.path_map.keys().cloned().collect(), vec![])
        };

        self.input_mode = InputMode::Normal;
        self.input_text.clear();
        self.is_processing = true;
        self.status_msg = format!("Saving '{name}'...");
        self.reload_needed = true;

        let (tx, rx) = mpsc::channel();
        self.receiver = Some(rx);
        let cfg = self.config.clone();
        let root = self.app_root.clone();

        thread::spawn(move || {
            engine_save_new_delta(&root, cfg, name, parent_id, scopes, exclusions, tx);
        });
    }

    fn start_delete_process(&mut self) {
        let idx = self.state.selected().unwrap_or(0);
        if idx >= self.ids.len() {
            return;
        }
        let id = self.ids[idx].clone();
        if id.is_empty() {
            return;
        }

        self.delete_target_id = id.clone();
        let children = self.get_children(&id);
        self.input_mode = InputMode::ConfirmDelete;
        self.input_text.clear();

        if children.is_empty() {
            self.delete_warning_msg = format!("Delete '{}'? (type 'y')", self.items[idx]);
        } else {
            self.delete_warning_msg = format!(
                "DANGER: Delete '{}' and {} children? Type 'DELETE'",
                self.items[idx],
                children.len()
            );
        }
    }

    fn action_delete_confirm(&mut self) {
        let children = self.get_children(&self.delete_target_id);
        let input_clean = self.input_text.trim();
        
        // CORREÇÃO: Lógica de delete flexível
        let confirmed = if children.is_empty() {
            // Se for folha, aceita y ou Y
            input_clean.eq_ignore_ascii_case("y")
        } else {
            // Se tiver filhos, exige DELETE exato
            input_clean == "DELETE"
        };

        if confirmed {
            self.input_mode = InputMode::Normal;
            self.input_text.clear();
            self.is_processing = true;
            self.status_msg = "Deleting...".into();
            self.reload_needed = true;

            let target = self.delete_target_id.clone();
            let all: Vec<SetManifest> = self.manifests_cache.values().cloned().collect();
            let (tx, rx) = mpsc::channel();
            self.receiver = Some(rx);
            let root = self.app_root.clone();

            thread::spawn(move || {
                engine_delete_cascade(&root, target, &all, tx);
            });
        }
    }

    fn check_progress(&mut self) {
        let mut done = false;

        if let Some(rx) = &self.receiver {
            for (p, msg) in rx.try_iter() {
                self.progress = p as u16;
                self.status_msg = msg;
                if self.progress >= 100 {
                    done = true;
                }
            }
        }

        if done {
            if let InputMode::Initializing = self.input_mode {
                *self = App::load_initial_state(self.app_root.clone());
                self.input_mode = InputMode::Normal;
                return;
            }

            self.is_processing = false;
            self.progress = 0;
            self.receiver = None;

            if self.reload_needed {
                self.refresh_list();
            }

            self.check_dir_status();
        }

        if let Some(rx) = &self.diff_receiver {
            for diff in rx.try_iter() {
                self.diff_summary = diff;
                self.status_msg = "Check completed.".into();
            }
        }
    }
}

// ------------------------- UI helpers / main ------------------------------

fn centered_rect(percent_x: u16, percent_y: u16, r: Rect) -> Rect {
    let popup_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints(
            [
                Constraint::Percentage((100 - percent_y) / 2),
                Constraint::Percentage(percent_y),
                Constraint::Percentage((100 - percent_y) / 2),
            ]
            .as_ref(),
        )
        .split(r);

    Layout::default()
        .direction(Direction::Horizontal)
        .constraints(
            [
                Constraint::Percentage((100 - percent_x) / 2),
                Constraint::Percentage(percent_x),
                Constraint::Percentage((100 - percent_x) / 2),
            ]
            .as_ref(),
        )
        .split(popup_layout[1])[1]
}

fn main() -> Result<(), io::Error> {
    let original_hook = panic::take_hook();
    panic::set_hook(Box::new(move |panic_info| {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen);
        original_hook(panic_info);
    }));

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    let mut app = App::new();

    loop {
        terminal.draw(|f| {
            if let InputMode::Initializing = app.input_mode {
                let rect = centered_rect(60, 20, f.size());
                f.render_widget(Clear, rect);
                let b = Block::default()
                    .borders(Borders::ALL)
                    .title(" Preparing environment ")
                    .style(Style::default().fg(Color::Yellow));
                let inner = b.inner(rect);
                f.render_widget(b, rect);

                let chunks = Layout::default()
                    .constraints([Constraint::Length(3), Constraint::Length(3)])
                    .split(inner);
                f.render_widget(
                    Paragraph::new(app.status_msg.clone()).wrap(Wrap { trim: true }),
                    chunks[0],
                );
                let gauge = Gauge::default()
                    .gauge_style(Style::default().fg(Color::Green))
                    .percent(app.progress);
                f.render_widget(gauge, chunks[1]);
                return;
            }

            let main_chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints(
                    [
                        Constraint::Length(1),
                        Constraint::Min(10),
                        Constraint::Length(3),
                        Constraint::Length(3),
                    ]
                    .as_ref(),
                )
                .split(f.size());

            let root_display = app
                .config
                .path_map
                .get("Root")
                .map(|p| p.to_string_lossy())
                .unwrap_or("?".into());

            let active_name = app
                .active_set_id
                .as_ref()
                .and_then(|id| app.manifests_cache.get(id))
                .map(|m| m.name.clone())
                .unwrap_or("None".into());

            let header = Paragraph::new(format!(
                " -- BLK -- {} | Active: {}",
                root_display, active_name
            ))
            .style(Style::default().fg(Color::Black).bg(Color::White));
            f.render_widget(header, main_chunks[0]);

            let split_chunks = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
                .split(main_chunks[1]);

            let left_panel_chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Length(7), Constraint::Min(5)])
                .split(split_chunks[0]);

            let right_panel_chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
                .split(split_chunks[1]);

            let (status_color, status_title) = if app.diff_summary.is_dirty {
                (Color::Red, " ⚠ Changes detected ")
            } else {
                (Color::Green, " ✔ Synchronized ")
            };

            let status_text = vec![
                Line::from(vec![
                    Span::raw("Active Set ID: "),
                    Span::styled(
                        app.active_set_id.clone().unwrap_or("None".into()),
                        Style::default().add_modifier(Modifier::BOLD),
                    ),
                ]),
                Line::from(""),
                Line::from(vec![
                    Span::raw("New: "),
                    Span::styled(
                        format!("{}", app.diff_summary.new_files),
                        Style::default().fg(if app.diff_summary.new_files > 0 {
                            Color::Yellow
                        } else {
                            Color::White
                        }),
                    ),
                ]),
                Line::from(vec![
                    Span::raw("Modified: "),
                    Span::styled(
                        format!("{}", app.diff_summary.modified_files),
                        Style::default().fg(if app.diff_summary.modified_files > 0 {
                            Color::Yellow
                        } else {
                            Color::White
                        }),
                    ),
                ]),
                Line::from(vec![
                    Span::raw("Deleted: "),
                    Span::styled(
                        format!("{}", app.diff_summary.deleted_files),
                        Style::default().fg(if app.diff_summary.deleted_files > 0 {
                            Color::Red
                        } else {
                            Color::White
                        }),
                    ),
                ]),
            ];
            f.render_widget(
                Paragraph::new(status_text).block(
                    Block::default()
                        .borders(Borders::ALL)
                        .title(status_title)
                        .border_style(Style::default().fg(status_color)),
                ),
                left_panel_chunks[0],
            );

            let list = List::new(
                app.items
                    .iter()
                    .map(|i| ListItem::new(i.as_str()))
                    .collect::<Vec<_>>(),
            )
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(" Available Sets "),
            )
            .highlight_style(
                Style::default()
                    .bg(Color::Blue)
                    .add_modifier(Modifier::BOLD),
            );
            f.render_stateful_widget(list, left_panel_chunks[1], &mut app.state);

            let idx = app.state.selected().unwrap_or(0);
            let current_id = if idx < app.ids.len() {
                app.ids[idx].clone()
            } else {
                String::new()
            };

            let mut tree_text = vec![];
            let mut config_text = vec![];

            if !current_id.is_empty() {
                let chain = app.resolve_dependencies(&current_id);
                if let Some(man) = app.manifests_cache.get(&current_id) {
                    tree_text.push(Line::from(Span::styled(
                        "Lineage:",
                        Style::default().add_modifier(Modifier::BOLD),
                    )));
                    for (i, node_id) in chain.iter().enumerate() {
                        let node_name = app
                            .manifests_cache
                            .get(node_id)
                            .map(|m| m.name.clone())
                            .unwrap_or("?".into());
                        let size = get_snapshot_size(&app.app_root, node_id);
                        let prefix = if i == 0 {
                            "".to_string()
                        } else {
                            format!("{}└─ ", "  ".repeat(i))
                        };
                        let style = if node_id == &current_id {
                            Style::default().fg(Color::Green)
                        } else {
                            Style::default()
                        };
                        tree_text.push(Line::from(vec![
                            Span::raw(prefix),
                            Span::styled(node_name, style),
                            Span::styled(
                                format!(" ({})", format_bytes(size)),
                                Style::default().fg(Color::DarkGray),
                            ),
                        ]));
                    }

                    config_text.push(Line::from(Span::styled(
                        "Inclusions (Scopes):",
                        Style::default().fg(Color::Cyan),
                    )));
                    for scope in &man.scopes {
                        let path_display = if let Some(p) = app.config.path_map.get(scope) {
                            format!("{scope}: {}", p.to_string_lossy())
                        } else {
                            scope.clone()
                        };
                        config_text.push(Line::from(vec![
                            Span::raw("+ "),
                            Span::styled(
                                path_display,
                                Style::default().fg(Color::Cyan),
                            ),
                        ]));
                    }
                    config_text.push(Line::from(""));
                    config_text.push(Line::from(Span::styled(
                        "Exclusions:",
                        Style::default().fg(Color::Red),
                    )));
                    for exc in &man.exclusions {
                        config_text.push(Line::from(vec![
                            Span::raw("- "),
                            Span::styled(exc, Style::default().fg(Color::Red)),
                        ]));
                    }
                }
            }

            f.render_widget(
                Paragraph::new(tree_text)
                    .block(Block::default().borders(Borders::ALL).title(" Structure "))
                    .wrap(Wrap { trim: true })
                    .scroll((app.tree_scroll, 0)),
                right_panel_chunks[0],
            );

            f.render_widget(
                Paragraph::new(config_text)
                    .block(Block::default().borders(Borders::ALL).title(" List "))
                    .wrap(Wrap { trim: true }),
                right_panel_chunks[1],
            );

            if app.is_processing {
                // CORREÇÃO: Feedback visual "ƒ" piscante
                let blink_char = if (app.spinner_tick / 5) % 2 == 0 { "ƒ" } else { " " };
                let title = format!(" Working... {} ", blink_char);

                let gauge = Gauge::default()
                    .block(
                        Block::default()
                            .borders(Borders::ALL)
                            .title(title),
                    )
                    .gauge_style(Style::default().fg(Color::Green))
                    .percent(app.progress);
                f.render_widget(gauge, main_chunks[2]);
            }

            let footer_content = match app.input_mode {
                InputMode::Normal => format!(
                    "{} | [Enter] Restore | [S] Save Delta | [D] Delete | [F5] Check | [Q] Quit",
                    app.status_msg
                ),
                InputMode::EditingName => {
                    "NEW DELTA NAME: type and [Enter], [Esc] cancels".into()
                }
                InputMode::ConfirmDelete => {
                    "DELETE: Type 'y' (simple) or 'DELETE' (cascade) and press [Enter]".into()
                }
                InputMode::Configuring => "CONFIG: [Tab] Switch | [A] Add (Name=Path or Path) | [D] Delete | [Enter] Save | [Esc] Close".into(),
                InputMode::AddingPath => "PATH: Type and [Enter]. Ex: AC=C:\\Games\\Assetto  or  C:\\Users\\...".into(),
                InputMode::Initializing => "STARTUP...".into(),
            };
            f.render_widget(
                Paragraph::new(footer_content)
                    .block(Block::default().borders(Borders::ALL)),
                main_chunks[3],
            );

            if let InputMode::EditingName = app.input_mode {
                let r = centered_rect(60, 20, f.size());
                f.render_widget(Clear, r);
                
                let block = Block::default().borders(Borders::ALL).title("New Set");
                let inner = block.inner(r);
                f.render_widget(
                    Paragraph::new(app.input_text.clone()).block(block),
                    r,
                );
                f.set_cursor(
                    inner.x + app.input_text.len() as u16,
                    inner.y,
                );
            }

            if let InputMode::ConfirmDelete = app.input_mode {
                let r = centered_rect(60, 40, f.size());
                f.render_widget(Clear, r);
                let b = Block::default()
                    .borders(Borders::ALL)
                    .title("DELETE")
                    .style(Style::default().fg(Color::Red));
                let i = b.inner(r);
                f.render_widget(b, r);

                let c = Layout::default()
                    .constraints([Constraint::Min(4), Constraint::Length(3)])
                    .split(i);
                f.render_widget(
                    Paragraph::new(app.delete_warning_msg.clone()).wrap(Wrap { trim: true }),
                    c[0],
                );
                f.render_widget(
                    Paragraph::new(app.input_text.clone())
                        .block(Block::default().borders(Borders::ALL)),
                    c[1],
                );
                f.set_cursor(
                     c[1].x + 1 + app.input_text.len() as u16,
                     c[1].y + 1
                );
            }

            if let InputMode::Configuring | InputMode::AddingPath = app.input_mode {
                let r = centered_rect(80, 80, f.size());
                f.render_widget(Clear, r);
                let mb = Block::default()
                    .borders(Borders::ALL)
                    .title(format!("Editing: {}", app.config_target_id));
                let ir = mb.inner(r);
                f.render_widget(mb, r);

                let c = Layout::default()
                    .direction(Direction::Vertical)
                    .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
                    .split(ir);

                let scopes_items: Vec<ListItem> = app
                    .config_temp_scopes
                    .iter()
                    .map(|s| ListItem::new(format!("+ {s}")))
                    .collect();
                let scopes_border = if let ConfigFocus::Scopes = app.config_focus {
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default()
                };
                let scopes_list = List::new(scopes_items)
                    .block(
                        Block::default()
                            .borders(Borders::ALL)
                            .title("Inclusion (Scope)")
                            .border_style(scopes_border),
                    )
                    .highlight_style(Style::default().bg(Color::DarkGray));

                let exc_items: Vec<ListItem> = app
                    .config_temp_exclusions
                    .iter()
                    .map(|s| ListItem::new(format!("- {s}")))
                    .collect();
                let exc_border = if let ConfigFocus::Exclusions = app.config_focus {
                    Style::default()
                        .fg(Color::Red)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default()
                };
                let exc_list = List::new(exc_items)
                    .block(
                        Block::default()
                            .borders(Borders::ALL)
                            .title("Exclusion (simple substring)")
                            .border_style(exc_border),
                    )
                    .highlight_style(Style::default().bg(Color::DarkGray));

                if let ConfigFocus::Scopes = app.config_focus {
                    f.render_stateful_widget(scopes_list, c[0], &mut app.config_state);
                    f.render_widget(exc_list, c[1]);
                } else {
                    f.render_widget(scopes_list, c[0]);
                    f.render_stateful_widget(exc_list, c[1], &mut app.config_state);
                }

                if let InputMode::AddingPath = app.input_mode {
                    let ir2 = centered_rect(60, 15, r);
                    f.render_widget(Clear, ir2);
                    
                    let block = Block::default()
                        .borders(Borders::ALL)
                        .title("Add Path (Name=Path) or raw Path")
                        .style(Style::default().fg(Color::Yellow));
                    let inner = block.inner(ir2);

                    f.render_widget(
                        Paragraph::new(app.input_text.clone()).block(block),
                        ir2,
                    );

                    // Cursor seguro (não estoura borda)
                    let max_width = inner.width.saturating_sub(1);
                    let len = app.input_text.len() as u16;
                    let cursor_x = inner.x + len.min(max_width);
                    let cursor_y = inner.y;
                    f.set_cursor(cursor_x, cursor_y);
                }
            }
        })?;

        app.check_progress();

        // Incrementa o contador de tick para o spinner piscar
        app.spinner_tick = app.spinner_tick.wrapping_add(1);

        if let InputMode::Initializing = app.input_mode {
            if !app.init_thread_spawned {
                app.trigger_auto_init();
            }
        }

        if event::poll(Duration::from_millis(50))? {
            if let Event::Key(key) = event::read()? {
                if key.kind == KeyEventKind::Press {
                    match app.input_mode {
                        InputMode::Normal => match key.code {
                            KeyCode::Char('q') => break,
                            KeyCode::Down => {
                                if !app.is_processing {
                                    app.next();
                                }
                            }
                            KeyCode::Up => {
                                if !app.is_processing {
                                    app.previous();
                                }
                            }
                            KeyCode::PageDown => {
                                app.tree_scroll = app.tree_scroll.saturating_add(3);
                            }
                            KeyCode::PageUp => {
                                if app.tree_scroll >= 3 {
                                    app.tree_scroll -= 3;
                                } else {
                                    app.tree_scroll = 0;
                                }
                            }
                            KeyCode::Enter => {
                                if !app.is_processing {
                                    app.action_restore();
                                }
                            }
                            KeyCode::Char('s') => {
                                if !app.is_processing {
                                    app.pending_save_after_config = true;
                                    app.start_config_viewer();
                                }
                            }
                            KeyCode::Char('d') => {
                                if !app.is_processing {
                                    app.start_delete_process();
                                }
                            }
                            KeyCode::F(5) => {
                                if !app.is_processing {
                                    app.check_dir_status();
                                }
                            }
                            _ => {}
                        },
                        InputMode::EditingName => match key.code {
                            KeyCode::Esc => app.input_mode = InputMode::Normal,
                            KeyCode::Enter => app.action_save(),
                            KeyCode::Backspace => {
                                app.input_text.pop();
                            }
                            KeyCode::Char(c) => {
                                app.input_text.push(c);
                            }
                            _ => {}
                        },
                        InputMode::ConfirmDelete => match key.code {
                            KeyCode::Esc => app.input_mode = InputMode::Normal,
                            KeyCode::Enter => app.action_delete_confirm(),
                            KeyCode::Backspace => {
                                app.input_text.pop();
                            }
                            KeyCode::Char(c) => {
                                app.input_text.push(c);
                            }
                            _ => {}
                        },
                        InputMode::Configuring => match key.code {
                            KeyCode::Esc => app.input_mode = InputMode::Normal,
                            KeyCode::Tab => app.config_toggle_focus(),
                            KeyCode::Down => app.config_next_item(),
                            KeyCode::Char('a') => app.config_start_add(),
                            KeyCode::Char('d') => app.config_delete_selected(),
                            KeyCode::Enter => app.config_save_and_exit(),
                            _ => {}
                        },
                        InputMode::AddingPath => match key.code {
                            KeyCode::Esc => app.input_mode = InputMode::Configuring,
                            KeyCode::Enter => app.config_confirm_add(),
                            KeyCode::Backspace => {
                                app.input_text.pop();
                            }
                            KeyCode::Char(c) => {
                                app.input_text.push(c);
                            }
                            _ => {}
                        },
                        InputMode::Initializing => {}
                    }
                }
            }
        }
    }

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    Ok(())
}