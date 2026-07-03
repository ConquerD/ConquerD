mod app;
mod clipboard;
mod ui;
mod worker;

use std::io;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyEventKind,
    KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use crossterm::ExecutableCommand;
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use snm_core::Inventory;
use snm_supernode::{connect_host, LifecycleAction};
use snm_transport::{clear_session_password, SshBackend, SshTransport};
use tokio::sync::mpsc;
use tokio::time;

use self::app::{
    App, ButtonId, ClickTarget, ConfirmRemoveOutcome, ConfirmUninstallOutcome, FormField, LogsView,
    Panel, SettingsField, WorkerCmd, WorkerMsg,
};
use self::clipboard::{copy_target_from_logs, copy_to_clipboard};
use self::worker::spawn_worker;

pub async fn run(inventory_path: PathBuf, ssh_backend: SshBackend) -> Result<()> {
    let inventory = Inventory::load(&inventory_path)?;
    let mut app = App::new(inventory_path.clone(), inventory);

    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel::<WorkerCmd>();
    let (msg_tx, mut msg_rx) = mpsc::unbounded_channel::<WorkerMsg>();
    let (event_tx, mut event_rx) = mpsc::channel::<Event>(64);
    let input_paused = Arc::new(AtomicBool::new(false));

    spawn_worker(
        inventory_path.clone(),
        app.inventory.clone(),
        ssh_backend,
        cmd_rx,
        msg_tx.clone(),
    );

    let input_paused_thread = Arc::clone(&input_paused);
    std::thread::spawn(move || loop {
        if input_paused_thread.load(Ordering::Relaxed) {
            std::thread::sleep(Duration::from_millis(50));
            continue;
        }
        if event::poll(Duration::from_millis(120)).unwrap_or(false) {
            if let Ok(ev) = event::read() {
                if event_tx.blocking_send(ev).is_err() {
                    break;
                }
            }
        }
    });

    let mut terminal = setup_terminal()?;
    let mut tick = time::interval(Duration::from_secs(app.auto_refresh_secs));
    tick.set_missed_tick_behavior(time::MissedTickBehavior::Delay);
    let mut anim_tick = time::interval(Duration::from_millis(200));
    anim_tick.set_missed_tick_behavior(time::MissedTickBehavior::Skip);

    begin_refresh(&mut app);
    let _ = cmd_tx.send(WorkerCmd::RefreshAll);

    let result = loop {
        terminal.draw(|frame| ui::draw(frame, &mut app))?;

        tokio::select! {
            () = std::future::ready(()), if app.logs_open_pending.is_some() => {
                let text = app.logs_open_pending.take().unwrap();
                let title = match app.logs_view {
                    LogsView::Invite => "Invite",
                    LogsView::Journal => "Logs",
                    LogsView::Operation => "Operation",
                };
                input_paused.store(true, Ordering::Relaxed);
                std::thread::sleep(Duration::from_millis(150));
                while event_rx.try_recv().is_ok() {}
                restore_terminal(&mut terminal)?;
                eprintln!();
                eprintln!("{title} — select text with your mouse, then copy (Ctrl+C / right-click)");
                eprintln!("{}", "─".repeat(72));
                eprintln!("{text}");
                eprintln!("{}", "─".repeat(72));
                eprintln!();
                eprintln!("Press Enter to return to dashboard…");
                let mut line = String::new();
                let _ = std::io::stdin().read_line(&mut line);
                terminal = setup_terminal()?;
                while event_rx.try_recv().is_ok() {}
                input_paused.store(false, Ordering::Relaxed);
            }
            () = std::future::ready(()), if app.connect_pending.is_some() => {
                let (ssh, label) = app.connect_pending.take().unwrap();
                input_paused.store(true, Ordering::Relaxed);
                std::thread::sleep(Duration::from_millis(150));
                while event_rx.try_recv().is_ok() {}
                restore_terminal(&mut terminal)?;
                eprintln!();
                eprintln!("SSH connect: {label} ({ssh})");
                let transport = SshTransport::new(&ssh, ssh_backend);
                match connect_host(&transport).await {
                    Ok(()) => {
                        eprintln!("Connected. Password cached for this session.");
                        app.status_line = format!("connected to {label}");
                    }
                    Err(err) => {
                        eprintln!("Connect failed: {err:#}");
                        app.status_line = format!("connect {label} failed: {err}");
                    }
                }
                eprintln!();
                eprintln!("Press Enter to return to dashboard…");
                let mut line = String::new();
                let _ = std::io::stdin().read_line(&mut line);
                terminal = setup_terminal()?;
                while event_rx.try_recv().is_ok() {}
                input_paused.store(false, Ordering::Relaxed);
            }
            Some(ev) = event_rx.recv() => {
                match ev {
                    Event::Key(key) => {
                        if handle_key(&mut app, &cmd_tx, key) {
                            break Ok(());
                        }
                    }
                    Event::Mouse(mouse) => {
                        handle_mouse(&mut app, &cmd_tx, mouse);
                    }
                    Event::Paste(text) => {
                        handle_paste(&mut app, text);
                    }
                    Event::Resize(_, _) => {}
                    _ => {}
                }
            }
            Some(msg) = msg_rx.recv() => {
                app.apply_worker_msg(msg);
            }
            _ = tick.tick() => {
                if !app.busy && app.panel == Panel::Fleet {
                    begin_refresh(&mut app);
                    let _ = cmd_tx.send(WorkerCmd::RefreshAll);
                }
            }
            _ = anim_tick.tick() => {
                app.anim_frame = app.anim_frame.wrapping_add(1);
            }
        }
    };

    restore_terminal(&mut terminal)?;
    clear_session_password();
    result
}

fn handle_mouse(app: &mut App, cmd_tx: &mpsc::UnboundedSender<WorkerCmd>, mouse: MouseEvent) {
    match mouse.kind {
        MouseEventKind::Moved => {
            app.hover_target = app.hit_test(mouse.column, mouse.row);
        }
        MouseEventKind::Down(MouseButton::Left) => {
            if let Some(target) = app.hit_test(mouse.column, mouse.row) {
                activate_click(app, cmd_tx, target);
            }
        }
        MouseEventKind::ScrollUp => scroll_panel(app, -3),
        MouseEventKind::ScrollDown => scroll_panel(app, 3),
        _ => {}
    }
}

fn scroll_panel(app: &mut App, delta: i16) {
    match app.panel {
        Panel::Logs => app.scroll_logs(delta),
        Panel::Help => app.scroll_help(delta),
        Panel::NodeForm => app.scroll_node_form(delta),
        Panel::NodeConfig => app.scroll_node_config_form(delta),
        Panel::Settings => app.scroll_settings_form(delta),
        _ => {}
    }
}

fn activate_click(app: &mut App, cmd_tx: &mpsc::UnboundedSender<WorkerCmd>, target: ClickTarget) {
    match target {
        ClickTarget::SelectRow(index) => {
            app.selected = index;
            app.status_line = selection_status(app, index);
        }
        ClickTarget::Button(id) => run_button(app, cmd_tx, id),
        ClickTarget::FormField(field) => {
            app.node_form.focus(field);
        }
        ClickTarget::ConfigField(index) => {
            app.node_config_form.focus(index);
        }
        ClickTarget::SettingsField(field) => {
            app.settings_form.focus(field);
        }
        ClickTarget::UninstallConfirmField => {
            app.uninstall_confirm.purge_field_focused = true;
            app.uninstall_confirm.error = None;
            app.status_line =
                "type instance id or label, then click Confirm Uninstall below".into();
        }
    }
}

fn handle_paste(app: &mut App, text: String) {
    if app.panel == Panel::ConfirmUninstall && app.uninstall_confirm.purge {
        app.uninstall_confirm.purge_confirm.push_str(text.trim());
        app.uninstall_confirm.error = None;
        app.uninstall_confirm.purge_field_focused = true;
    }
}

fn selection_status(app: &App, index: usize) -> String {
    let Some(row) = app.rows.get(index) else {
        return "no selection".into();
    };
    format!("selected {} — {}", row.label(), row.status.state_label())
}

fn run_button(app: &mut App, cmd_tx: &mpsc::UnboundedSender<WorkerCmd>, id: ButtonId) {
    if !app.button_enabled(id) {
        app.status_line = "wait for the current operation to finish".into();
        return;
    }
    match id {
        ButtonId::AddNode => app.open_add_node(),
        ButtonId::EditNode if !app.busy && app.panel == Panel::Fleet => app.open_edit_node(),
        ButtonId::NodeConfig if app.panel == Panel::Fleet => app.open_node_config(),
        ButtonId::SaveNodeConfig => match app.commit_node_config_form() {
            Ok(()) => {}
            Err(err) => app.node_config_form.error = Some(err.to_string()),
        },
        ButtonId::CancelNodeConfig => {
            app.panel = Panel::Fleet;
            app.status_line = "cancelled supernode config".into();
        }
        ButtonId::SaveNode => match app.commit_node_form() {
            Ok(()) => {}
            Err(err) => app.node_form.error = Some(err.to_string()),
        },
        ButtonId::CancelNode => {
            app.panel = Panel::Fleet;
            app.status_line = if app.node_form.is_edit() {
                "cancelled edit node".into()
            } else {
                "cancelled add node".into()
            };
        }
        ButtonId::Refresh if !app.busy && app.panel == Panel::Fleet => {
            begin_refresh(app);
            app.status_line = "refreshing fleet…".into();
            let _ = cmd_tx.send(WorkerCmd::RefreshAll);
        }
        ButtonId::Connect if !app.busy && app.panel == Panel::Fleet => {
            app.queue_connect();
        }
        ButtonId::Ping if !app.busy && app.panel == Panel::Fleet => {
            app.busy = true;
            app.status_line = "pinging host…".into();
            let _ = cmd_tx.send(WorkerCmd::Ping(app.selected));
        }
        ButtonId::Start if !app.busy && app.panel == Panel::Fleet => {
            app.busy = true;
            app.status_line = "starting instance…".into();
            let _ = cmd_tx.send(WorkerCmd::Lifecycle(app.selected, LifecycleAction::Start));
        }
        ButtonId::Stop if !app.busy && app.panel == Panel::Fleet => {
            app.busy = true;
            app.status_line = "stopping instance…".into();
            let _ = cmd_tx.send(WorkerCmd::Lifecycle(app.selected, LifecycleAction::Stop));
        }
        ButtonId::Restart if !app.busy && app.panel == Panel::Fleet => {
            app.busy = true;
            app.status_line = "restarting instance…".into();
            let _ = cmd_tx.send(WorkerCmd::Lifecycle(app.selected, LifecycleAction::Restart));
        }
        ButtonId::Install if !app.busy && app.panel == Panel::Fleet => {
            app.busy = true;
            app.status_line = "installing instance…".into();
            let _ = cmd_tx.send(WorkerCmd::Install(app.selected));
        }
        ButtonId::Settings if app.panel == Panel::Fleet => app.open_settings(),
        ButtonId::ConfigPush if !app.busy && app.panel == Panel::Fleet => {
            app.busy = true;
            app.status_line = "pushing config…".into();
            let _ = cmd_tx.send(WorkerCmd::ConfigPush(app.selected));
        }
        ButtonId::SaveSettings => match app.commit_settings_form() {
            Ok(()) => {}
            Err(err) => app.settings_form.error = Some(err.to_string()),
        },
        ButtonId::CancelSettings => {
            app.panel = Panel::Fleet;
            app.status_line = "cancelled settings".into();
        }
        ButtonId::Uninstall if !app.busy && app.panel == Panel::Fleet => {
            app.open_confirm_uninstall();
        }
        ButtonId::Logs if !app.busy && app.panel == Panel::Fleet => {
            app.busy = true;
            app.status_line = "fetching logs…".into();
            let _ = cmd_tx.send(WorkerCmd::FetchLogs(app.selected));
        }
        ButtonId::Invite if !app.busy && app.panel == Panel::Fleet => {
            app.busy = true;
            app.status_line = "fetching invite…".into();
            let _ = cmd_tx.send(WorkerCmd::FetchInvite(app.selected));
        }
        ButtonId::ClusterSync if !app.busy && app.panel == Panel::Fleet => {
            app.busy = true;
            app.status_line = "syncing cluster roster…".into();
            let _ = cmd_tx.send(WorkerCmd::ClusterSync);
        }
        ButtonId::BuildDeploy if !app.busy && app.panel == Panel::Fleet => {
            app.busy = true;
            app.status_line = "building and deploying…".into();
            let _ = cmd_tx.send(WorkerCmd::BuildDeploy(app.selected));
        }
        ButtonId::Remove if !app.busy && app.panel == Panel::Fleet => {
            app.open_confirm_remove();
        }
        ButtonId::UninstallPurgeToggle if app.panel == Panel::ConfirmUninstall => {
            app.toggle_uninstall_purge();
        }
        ButtonId::ConfirmUninstall if app.panel == Panel::ConfirmUninstall => {
            submit_confirm_uninstall(app, cmd_tx);
        }
        ButtonId::CancelUninstall => {
            app.panel = Panel::Fleet;
            app.status_line = "cancelled uninstall".into();
        }
        ButtonId::ConfirmRemove if app.panel == Panel::ConfirmRemove => {
            submit_confirm_remove(app);
        }
        ButtonId::CancelRemove => {
            app.panel = Panel::Fleet;
            app.status_line = "cancelled remove".into();
        }
        _ => {}
    }
}

fn handle_key(app: &mut App, cmd_tx: &mpsc::UnboundedSender<WorkerCmd>, key: KeyEvent) -> bool {
    // Windows terminals often emit Repeat (and sometimes Release) alongside Press
    // for a single physical keypress — ignore those to avoid doubled input.
    if key.kind != KeyEventKind::Press {
        return false;
    }

    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
        return true;
    }

    match app.panel {
        Panel::ConfirmRemove => match key.code {
            KeyCode::Esc => {
                app.panel = Panel::Fleet;
                app.status_line = "cancelled remove".into();
            }
            KeyCode::Enter => submit_confirm_remove(app),
            _ => {}
        },
        Panel::ConfirmUninstall => match key.code {
            KeyCode::Esc => {
                app.panel = Panel::Fleet;
                app.status_line = "cancelled uninstall".into();
            }
            KeyCode::Enter => submit_confirm_uninstall(app, cmd_tx),
            KeyCode::Char('p') => app.toggle_uninstall_purge(),
            KeyCode::Backspace if app.uninstall_confirm.purge => {
                app.uninstall_confirm.purge_confirm.pop();
            }
            KeyCode::Char(c) if app.uninstall_confirm.purge => {
                app.uninstall_confirm.purge_confirm.push(c);
                app.uninstall_confirm.error = None;
            }
            _ => {}
        },
        Panel::NodeConfig => match key.code {
            KeyCode::Esc => {
                app.panel = Panel::Fleet;
                app.status_line = "cancelled supernode config".into();
            }
            KeyCode::Enter => match app.commit_node_config_form() {
                Ok(()) => {}
                Err(err) => app.node_config_form.error = Some(err.to_string()),
            },
            KeyCode::Tab => {
                if key.modifiers.contains(KeyModifiers::SHIFT) {
                    app.node_config_form.prev_field();
                } else {
                    app.node_config_form.next_field();
                }
            }
            KeyCode::BackTab => app.node_config_form.prev_field(),
            KeyCode::Char(' ') => app.node_config_form.toggle_focused_feature(),
            KeyCode::Backspace => app.node_config_form.backspace(),
            KeyCode::Up | KeyCode::Char('k') => app.scroll_node_config_form(-1),
            KeyCode::Down | KeyCode::Char('j') => app.scroll_node_config_form(1),
            KeyCode::Char('q') if key.modifiers.contains(KeyModifiers::CONTROL) => return true,
            KeyCode::Char(c) => app.node_config_form.insert_char(c),
            _ => {}
        },
        Panel::Settings => match key.code {
            KeyCode::Esc => {
                app.panel = Panel::Fleet;
                app.status_line = "cancelled settings".into();
            }
            KeyCode::Enter => match app.commit_settings_form() {
                Ok(()) => {}
                Err(err) => app.settings_form.error = Some(err.to_string()),
            },
            KeyCode::Tab => {
                let next = if key.modifiers.contains(KeyModifiers::SHIFT) {
                    SettingsField::from_index(app.settings_form.focused_field).prev()
                } else {
                    SettingsField::from_index(app.settings_form.focused_field).next()
                };
                app.settings_form.focus(next);
            }
            KeyCode::BackTab => {
                let prev = SettingsField::from_index(app.settings_form.focused_field).prev();
                app.settings_form.focus(prev);
            }
            KeyCode::Backspace => app.settings_form.backspace(),
            KeyCode::Up | KeyCode::Char('k') => app.scroll_settings_form(-1),
            KeyCode::Down | KeyCode::Char('j') => app.scroll_settings_form(1),
            KeyCode::Char('q') if key.modifiers.contains(KeyModifiers::CONTROL) => return true,
            KeyCode::Char(c) => app.settings_form.insert_char(c),
            _ => {}
        },
        Panel::NodeForm => match key.code {
            KeyCode::Esc => {
                app.panel = Panel::Fleet;
                app.status_line = if app.node_form.is_edit() {
                    "cancelled edit node".into()
                } else {
                    "cancelled add node".into()
                };
            }
            KeyCode::Enter => match app.commit_node_form() {
                Ok(()) => {}
                Err(err) => app.node_form.error = Some(err.to_string()),
            },
            KeyCode::Tab => {
                let next = if key.modifiers.contains(KeyModifiers::SHIFT) {
                    FormField::from_index(app.node_form.focused_field).prev()
                } else {
                    FormField::from_index(app.node_form.focused_field).next()
                };
                app.node_form.focus(next);
            }
            KeyCode::BackTab => {
                let prev = FormField::from_index(app.node_form.focused_field).prev();
                app.node_form.focus(prev);
            }
            KeyCode::Backspace => app.node_form.backspace(),
            KeyCode::Up | KeyCode::Char('k') => app.scroll_node_form(-1),
            KeyCode::Down | KeyCode::Char('j') => app.scroll_node_form(1),
            KeyCode::Char('q') if key.modifiers.contains(KeyModifiers::CONTROL) => return true,
            KeyCode::Char(c) => app.node_form.insert_char(c),
            _ => {}
        },
        Panel::Help => match key.code {
            KeyCode::Esc | KeyCode::Char('?') => app.panel = Panel::Fleet,
            KeyCode::Char('q') => return true,
            KeyCode::Up | KeyCode::Char('k') => app.scroll_help(-1),
            KeyCode::Down | KeyCode::Char('j') => app.scroll_help(1),
            KeyCode::PageUp => app.scroll_help(-8),
            KeyCode::PageDown => app.scroll_help(8),
            KeyCode::Home => app.help_scroll = 0,
            _ => {}
        },
        Panel::Logs => match key.code {
            KeyCode::Esc => app.panel = Panel::Fleet,
            KeyCode::Char('?') => {
                app.help_scroll = 0;
                app.panel = Panel::Help;
            }
            KeyCode::Char('q') => return true,
            KeyCode::Char('y') => {
                let target = copy_target_from_logs(&app.logs_text);
                match copy_to_clipboard(&target) {
                    Ok(()) => {
                        let preview = if target.len() > 48 {
                            format!("{}…", &target[..48])
                        } else {
                            target.clone()
                        };
                        app.status_line = format!("copied to clipboard: {preview}");
                    }
                    Err(err) => app.status_line = err,
                }
            }
            KeyCode::Char('o') => {
                app.logs_open_pending = Some(app.logs_text.clone());
            }
            KeyCode::Up | KeyCode::Char('k') => app.scroll_logs(-1),
            KeyCode::Down | KeyCode::Char('j') => app.scroll_logs(1),
            KeyCode::PageUp => app.scroll_logs(-12),
            KeyCode::PageDown => app.scroll_logs(12),
            KeyCode::Home => app.scroll_logs_home(),
            KeyCode::End => app.scroll_logs_end(24),
            KeyCode::Char('g') => app.scroll_logs_home(),
            KeyCode::Char('G') => app.scroll_logs_end(24),
            _ => {}
        },
        Panel::Fleet => match key.code {
            KeyCode::Char('q') => return true,
            KeyCode::Char('?') => {
                app.help_scroll = 0;
                app.panel = Panel::Help;
            }
            KeyCode::Char('n') => app.open_add_node(),
            KeyCode::Char('e') if !app.busy => app.open_edit_node(),
            KeyCode::Char('C') => app.open_node_config(),
            KeyCode::Char('G') => app.open_settings(),
            KeyCode::Char('P') if !app.busy => {
                app.busy = true;
                app.status_line = "pushing config…".into();
                let _ = cmd_tx.send(WorkerCmd::ConfigPush(app.selected));
            }
            KeyCode::Char('d') if !app.busy => app.open_confirm_remove(),
            KeyCode::Char('u') if !app.busy => app.open_confirm_uninstall(),
            KeyCode::Up | KeyCode::Char('k') => {
                app.move_selection(-1);
                app.status_line = selection_status(app, app.selected);
            }
            KeyCode::Down | KeyCode::Char('j') => {
                app.move_selection(1);
                app.status_line = selection_status(app, app.selected);
            }
            KeyCode::Char('r') if !app.busy => {
                begin_refresh(app);
                app.status_line = "refreshing fleet…".into();
                let _ = cmd_tx.send(WorkerCmd::RefreshAll);
            }
            KeyCode::Char('c') if !app.busy => {
                app.queue_connect();
            }
            KeyCode::Char('p') if !app.busy => {
                app.busy = true;
                app.status_line = "pinging host…".into();
                let _ = cmd_tx.send(WorkerCmd::Ping(app.selected));
            }
            KeyCode::Char('s') if !app.busy => {
                app.busy = true;
                app.status_line = "starting instance…".into();
                let _ = cmd_tx.send(WorkerCmd::Lifecycle(app.selected, LifecycleAction::Start));
            }
            KeyCode::Char('x') if !app.busy => {
                app.busy = true;
                app.status_line = "stopping instance…".into();
                let _ = cmd_tx.send(WorkerCmd::Lifecycle(app.selected, LifecycleAction::Stop));
            }
            KeyCode::Char('R') if !app.busy => {
                app.busy = true;
                app.status_line = "restarting instance…".into();
                let _ = cmd_tx.send(WorkerCmd::Lifecycle(app.selected, LifecycleAction::Restart));
            }
            KeyCode::Char('i') if !app.busy => {
                app.busy = true;
                app.status_line = "installing instance…".into();
                let _ = cmd_tx.send(WorkerCmd::Install(app.selected));
            }
            KeyCode::Char('l') if !app.busy => {
                app.busy = true;
                app.status_line = "fetching logs…".into();
                let _ = cmd_tx.send(WorkerCmd::FetchLogs(app.selected));
            }
            KeyCode::Char('v') if !app.busy => {
                app.busy = true;
                app.status_line = "fetching invite…".into();
                let _ = cmd_tx.send(WorkerCmd::FetchInvite(app.selected));
            }
            KeyCode::Char('Z') if !app.busy => {
                app.busy = true;
                app.status_line = "syncing cluster roster…".into();
                let _ = cmd_tx.send(WorkerCmd::ClusterSync);
            }
            KeyCode::Char('B') if !app.busy => {
                app.busy = true;
                app.status_line = "building and deploying…".into();
                let _ = cmd_tx.send(WorkerCmd::BuildDeploy(app.selected));
            }
            _ => {}
        },
    }
    false
}

fn submit_confirm_remove(app: &mut App) {
    match app.try_confirm_remove() {
        ConfirmRemoveOutcome::Ready => match app.commit_remove_from_inventory() {
            Ok(message) => {
                app.panel = Panel::Fleet;
                app.status_line = message;
            }
            Err(err) => {
                app.remove_confirm.error = Some(err.to_string());
                app.status_line = err.to_string();
            }
        },
        ConfirmRemoveOutcome::BlockedBusy => {
            app.status_line = "wait for the current operation to finish".into();
        }
    }
}

fn submit_confirm_uninstall(app: &mut App, cmd_tx: &mpsc::UnboundedSender<WorkerCmd>) {
    match app.try_confirm_uninstall() {
        ConfirmUninstallOutcome::Proceed { row, purge, label } => {
            app.status_line = format!("uninstalling {label}…");
            let _ = cmd_tx.send(WorkerCmd::Uninstall { row, purge });
        }
        ConfirmUninstallOutcome::BlockedBusy => {
            app.status_line = "wait for the current operation to finish".into();
        }
        ConfirmUninstallOutcome::BlockedValidation { hint } => {
            app.status_line = hint;
        }
    }
}

fn begin_refresh(app: &mut App) {
    for row in &mut app.rows {
        row.status = app::RowStatus::Loading;
    }
    app.refresh_pending = app.rows.len();
    app.busy = true;
}

fn setup_terminal() -> Result<Terminal<CrosstermBackend<io::Stdout>>> {
    enable_raw_mode()?;
    io::stdout().execute(EnterAlternateScreen)?;
    io::stdout().execute(EnableMouseCapture)?;
    let backend = CrosstermBackend::new(io::stdout());
    let mut terminal = Terminal::new(backend)?;
    terminal.clear()?;
    terminal.hide_cursor()?;
    Ok(terminal)
}

fn restore_terminal(terminal: &mut Terminal<CrosstermBackend<io::Stdout>>) -> Result<()> {
    terminal.show_cursor()?;
    disable_raw_mode()?;
    io::stdout().execute(DisableMouseCapture)?;
    io::stdout().execute(LeaveAlternateScreen)?;
    Ok(())
}
