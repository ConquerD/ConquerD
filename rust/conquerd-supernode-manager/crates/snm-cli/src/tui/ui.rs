use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, Borders, Cell, Paragraph, Row, Scrollbar, ScrollbarOrientation, Table, Wrap,
};
use ratatui::Frame;

use super::app::{
    config_field_count, config_field_is_toggle, config_field_label, App, ButtonId, ClickTarget,
    InstanceRow, LogsView, Panel, RowStatus, NODE_FORM_FIELDS, SETTINGS_FIELDS,
};

const FOOTER_HEIGHT: u16 = 4;
const TOOLBAR_ROWS: u16 = 5;
const SUMMARY_HEIGHT: u16 = 4;

const INVENTORY_ACTIONS: &[ButtonId] = &[
    ButtonId::AddNode,
    ButtonId::EditNode,
    ButtonId::NodeConfig,
    ButtonId::Settings,
];
const REMOTE_ACTIONS: &[ButtonId] = &[
    ButtonId::Refresh,
    ButtonId::Connect,
    ButtonId::Ping,
    ButtonId::Install,
    ButtonId::ConfigPush,
];
const LIFECYCLE_ACTIONS: &[ButtonId] = &[
    ButtonId::Start,
    ButtonId::Stop,
    ButtonId::Restart,
    ButtonId::Uninstall,
];
const INSPECT_ACTIONS: &[ButtonId] = &[ButtonId::Logs, ButtonId::Invite, ButtonId::Remove];
const CLUSTER_ACTIONS: &[ButtonId] = &[ButtonId::ClusterSync, ButtonId::BuildDeploy];

pub fn draw(frame: &mut Frame, app: &mut App) {
    app.hit_zones.clear();

    let root = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(8),
            Constraint::Length(FOOTER_HEIGHT),
        ])
        .split(frame.area());

    draw_header(frame, root[0], app);
    match app.panel {
        Panel::Fleet => draw_fleet(frame, root[1], app),
        Panel::Logs => draw_logs(frame, root[1], app),
        Panel::Help => draw_help(frame, root[1], app),
        Panel::NodeForm => draw_node_form(frame, root[1], app),
        Panel::NodeConfig => draw_node_config_form(frame, root[1], app),
        Panel::Settings => draw_settings_form(frame, root[1], app),
        Panel::ConfirmRemove => draw_confirm_remove(frame, root[1], app),
        Panel::ConfirmUninstall => draw_confirm_uninstall(frame, root[1], app),
    }
    draw_footer(frame, root[2], app);
}

fn draw_header(frame: &mut Frame, area: Rect, app: &App) {
    let busy = if app.busy {
        format!("  {} working", app.busy_spinner())
    } else {
        String::new()
    };
    let refresh = app
        .last_refresh
        .map(|t| format!("  ·  {}s ago", t.elapsed().as_secs()))
        .unwrap_or_default();

    let inv_name = app
        .inventory_path
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| app.inventory_path.display().to_string());

    let title = Line::from(vec![
        Span::styled(
            " supernode-manager ",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(format!(
            "  {inv_name}  |  {} node(s){busy}{refresh}",
            app.rows.len()
        )),
        Span::raw("  "),
        Span::styled(
            format!("[ {} ]", app.panel.title()),
            Style::default().fg(Color::DarkGray),
        ),
    ]);

    let block = Block::default()
        .borders(Borders::BOTTOM)
        .border_style(Style::default().fg(Color::DarkGray));
    frame.render_widget(Paragraph::new(title).block(block), area);
}

fn draw_fleet(frame: &mut Frame, area: Rect, app: &mut App) {
    let toolbar_h = TOOLBAR_ROWS.saturating_add(2);
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(toolbar_h),
            Constraint::Length(SUMMARY_HEIGHT),
            Constraint::Min(4),
        ])
        .split(area);

    draw_toolbar(frame, chunks[0], app);
    draw_selection_summary(frame, chunks[1], app);
    draw_fleet_table(frame, chunks[2], app);
}

fn draw_toolbar(frame: &mut Frame, area: Rect, app: &mut App) {
    let inner = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::DarkGray))
        .title(" Actions ")
        .inner(area);
    frame.render_widget(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::DarkGray))
            .title(" Actions "),
        area,
    );

    let groups: [(&str, &[ButtonId]); 5] = [
        ("Inventory", INVENTORY_ACTIONS),
        ("Remote", REMOTE_ACTIONS),
        ("Lifecycle", LIFECYCLE_ACTIONS),
        ("Inspect", INSPECT_ACTIONS),
        ("Cluster", CLUSTER_ACTIONS),
    ];
    let label_width = if inner.width >= 92 { 12 } else { 0 };
    let max_x = inner.x.saturating_add(inner.width);

    for (row_idx, (group, buttons)) in groups.iter().enumerate() {
        if row_idx >= TOOLBAR_ROWS.min(inner.height) as usize {
            break;
        }
        let y = inner.y.saturating_add(row_idx as u16);
        let mut x = inner.x.saturating_add(1);
        if label_width > 0 {
            frame.render_widget(
                Paragraph::new(format!("{group}:")).style(Style::default().fg(Color::DarkGray)),
                Rect::new(x, y, label_width, 1),
            );
            x = x.saturating_add(label_width);
        }
        for id in *buttons {
            let label = button_caption(*id);
            let width = label.len() as u16 + 2;
            if x.saturating_add(width) >= max_x {
                break;
            }
            let rect = Rect::new(x, y, width, 1);
            draw_toolbar_button(frame, app, rect, *id, &label);
            x = x.saturating_add(width + 1);
        }
    }
}

fn button_caption(id: ButtonId) -> String {
    let base = id.label();
    match id.shortcut() {
        Some(key) => format!("{base} [{key}]"),
        None => base.to_string(),
    }
}

fn draw_toolbar_button(frame: &mut Frame, app: &mut App, rect: Rect, id: ButtonId, label: &str) {
    let enabled = app.button_enabled(id);
    let hovered = enabled && app.hover_target == Some(ClickTarget::Button(id));
    let style = if !enabled {
        Style::default()
            .fg(Color::DarkGray)
            .bg(Color::Rgb(22, 24, 36))
    } else if hovered {
        Style::default()
            .fg(Color::Black)
            .bg(Color::Cyan)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::Cyan).bg(Color::Rgb(28, 32, 48))
    };
    frame.render_widget(Paragraph::new(format!(" {label} ")).style(style), rect);
    if enabled {
        app.register_hit_zone(rect, ClickTarget::Button(id));
    }
}

fn draw_selection_summary(frame: &mut Frame, area: Rect, app: &App) {
    let block = Block::default()
        .title(" Selected ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::DarkGray));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let Some(row) = app.selected_row() else {
        frame.render_widget(
            Paragraph::new("No node selected").style(Style::default().fg(Color::DarkGray)),
            inner,
        );
        return;
    };

    let line1 = Line::from(vec![
        Span::styled(
            truncate_end(&row.label(), 28),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
        Span::styled(
            format!("{} {}", row.status.glyph(), row.status.state_label()),
            status_style(&row.status),
        ),
        Span::raw(format!("  version {}", row.status.version_detail_label())),
    ]);
    let cluster_span = if let Some(ref cid) = row.cluster_id {
        vec![
            Span::styled("  cluster ", Style::default().fg(Color::DarkGray)),
            Span::styled(cid.clone(), Style::default().fg(Color::Yellow)),
        ]
    } else {
        vec![]
    };
    let mut line2_spans = vec![
        Span::styled("public ", Style::default().fg(Color::DarkGray)),
        Span::raw(truncate_end(&row.public_host, 32)),
        Span::styled("  ports ", Style::default().fg(Color::DarkGray)),
        Span::raw(ports_label(row)),
        Span::styled("  ssh ", Style::default().fg(Color::DarkGray)),
        Span::raw(truncate_middle(&row.ssh, 28)),
        Span::styled("  source ", Style::default().fg(Color::DarkGray)),
        Span::raw(truncate_end(&install_source(app), 34)),
    ];
    line2_spans.extend(cluster_span);
    let line2 = Line::from(line2_spans);

    if inner.height > 0 {
        frame.render_widget(
            Paragraph::new(line1),
            Rect::new(inner.x, inner.y, inner.width, 1),
        );
    }
    if inner.height > 1 {
        frame.render_widget(
            Paragraph::new(line2).style(Style::default().fg(Color::Gray)),
            Rect::new(inner.x, inner.y.saturating_add(1), inner.width, 1),
        );
    }
}

fn install_source(app: &App) -> String {
    let defaults = &app.inventory.defaults;
    if defaults.version == "local" {
        return defaults
            .binary_path
            .as_ref()
            .map(|p| format!("local {}", p.display()))
            .unwrap_or_else(|| "local binary not set".into());
    }
    format!("{}@{}", defaults.release_repo, defaults.version)
}

fn ports_label(row: &InstanceRow) -> String {
    match row.web_port {
        Some(web) => format!(
            "relay {} / ws {} / web {}",
            row.relay_port, row.ws_port, web
        ),
        None => format!("relay {} / ws {}", row.relay_port, row.ws_port),
    }
}

fn draw_fleet_table(frame: &mut Frame, area: Rect, app: &mut App) {
    let block = Block::default()
        .title(" Fleet ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::DarkGray));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if app.rows.is_empty() {
        let hint = vec![
            Line::from(""),
            Line::from(Span::styled(
                "No nodes in inventory",
                Style::default().fg(Color::Yellow).bold(),
            )),
            Line::from(""),
            Line::from(Span::styled(
                "Press n or click + Add Node to get started",
                Style::default().fg(Color::DarkGray),
            )),
            Line::from(Span::styled(
                "Press ? for the full key reference",
                Style::default().fg(Color::DarkGray),
            )),
        ];
        frame.render_widget(Paragraph::new(hint).alignment(Alignment::Center), inner);
        return;
    }

    let header = Row::new(vec![
        Cell::from(""),
        Cell::from("HOST"),
        Cell::from("ID"),
        Cell::from("STATE"),
        Cell::from("VERSION"),
        Cell::from("PORTS"),
        Cell::from("PLATFORM"),
        Cell::from("SSH"),
    ])
    .style(
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    )
    .height(1);

    let rows: Vec<Row> = app
        .rows
        .iter()
        .enumerate()
        .map(|(i, row)| {
            let state_style = status_style(&row.status);
            let state_text = match &row.status {
                RowStatus::Error(msg) => {
                    format!("{} {}", row.status.glyph(), truncate_end(msg, 18))
                }
                _ => format!("{} {}", row.status.glyph(), row.status.state_label()),
            };
            let ports = match row.web_port {
                Some(web) => format!("{}/{}/{}", row.relay_port, row.ws_port, web),
                None => format!("{}/{}", row.relay_port, row.ws_port),
            };
            let platform = row.platform.as_deref().unwrap_or("-");
            let marker = if i == app.selected { ">" } else { " " };

            let selected = Style::default().bg(Color::Rgb(30, 35, 55));
            let base = if i == app.selected {
                selected
            } else {
                Style::default()
            };

            Row::new(vec![
                Cell::from(marker).style(base.fg(Color::Cyan)),
                Cell::from(row.host_name.clone()).style(base),
                Cell::from(row.instance_id.clone()).style(base),
                Cell::from(state_text).style(base.patch(state_style)),
                Cell::from(truncate_end(&row.status.version_label(), 26)).style(base),
                Cell::from(ports).style(base),
                Cell::from(platform.to_string()).style(base),
                Cell::from(truncate_middle(&row.ssh, 22)).style(base.fg(Color::DarkGray)),
            ])
            .height(1)
        })
        .collect();

    let table = Table::new(
        rows,
        [
            Constraint::Length(2),
            Constraint::Length(14),
            Constraint::Length(6),
            Constraint::Length(14),
            Constraint::Length(26),
            Constraint::Length(16),
            Constraint::Length(12),
            Constraint::Min(14),
        ],
    )
    .header(header);

    frame.render_widget(table, inner);

    let body_top = inner.y.saturating_add(1);
    let row_count = app.rows.len().min(inner.height.saturating_sub(1) as usize);
    for index in 0..row_count {
        let row_rect = Rect::new(inner.x, body_top + index as u16, inner.width, 1);
        app.register_hit_zone(row_rect, ClickTarget::SelectRow(index));
    }
}

fn truncate_end(s: &str, max: usize) -> String {
    let count = s.chars().count();
    if count <= max {
        return s.to_string();
    }
    if max <= 3 {
        return ".".repeat(max);
    }
    let mut out: String = s.chars().take(max - 3).collect();
    out.push_str("...");
    out
}

fn truncate_middle(s: &str, max: usize) -> String {
    let count = s.chars().count();
    if count <= max {
        return s.to_string();
    }
    if max <= 3 {
        return ".".repeat(max);
    }
    let keep = (max - 3) / 2;
    let head: String = s.chars().take(keep).collect();
    let tail_len = max - 3 - keep;
    let tail: String = s
        .chars()
        .rev()
        .take(tail_len)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    format!("{head}...{tail}")
}

fn draw_confirm_remove(frame: &mut Frame, area: Rect, app: &mut App) {
    let label = app.remove_confirm.label.clone();
    let error = app.remove_confirm.error.clone();

    let block = Block::default()
        .title(" Remove From Inventory ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Red));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(4), Constraint::Length(3)])
        .split(inner);
    let content = chunks[0];
    let content_bottom = content.y.saturating_add(content.height);

    let mut y = content.y;
    let draw_line = |frame: &mut Frame, y: u16, line: Line<'_>| -> Option<u16> {
        if y >= content_bottom {
            return None;
        }
        frame.render_widget(
            Paragraph::new(line),
            Rect::new(content.x, y, content.width, 1),
        );
        Some(y.saturating_add(1))
    };

    if let Some(next) = draw_line(
        frame,
        y,
        Line::from(vec![
            Span::raw("Drop "),
            Span::styled(label.as_str(), Style::default().fg(Color::Yellow).bold()),
            Span::raw(" from this manager's inventory?"),
        ]),
    ) {
        y = next;
    }
    if let Some(next) = draw_line(
        frame,
        y,
        Line::from(Span::styled(
            "The remote host is not contacted. The supernode keeps running there.",
            Style::default().fg(Color::DarkGray),
        )),
    ) {
        y = next;
    }
    if let Some(next) = draw_line(
        frame,
        y,
        Line::from(Span::styled(
            "Uninstall on the host first if you want to stop or delete the service.",
            Style::default().fg(Color::Red),
        )),
    ) {
        y = next;
    }

    if let Some(err) = &error {
        let _ = draw_line(
            frame,
            y,
            Line::from(Span::styled(
                format!("{err}"),
                Style::default().fg(Color::Red).bold(),
            )),
        );
    }

    let button_row = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(55), Constraint::Percentage(45)])
        .split(chunks[1]);

    draw_form_button(
        frame,
        app,
        button_row[0],
        ButtonId::ConfirmRemove,
        " Confirm Remove ",
        Color::Red,
    );
    draw_form_button(
        frame,
        app,
        button_row[1],
        ButtonId::CancelRemove,
        " Cancel ",
        Color::DarkGray,
    );
}

fn draw_confirm_uninstall(frame: &mut Frame, area: Rect, app: &mut App) {
    let label = app.uninstall_confirm.label.clone();
    let data_dir = app.uninstall_confirm.data_dir.clone();
    let instance_id = app.uninstall_confirm.instance_id.clone();
    let purge = app.uninstall_confirm.purge;
    let purge_confirm = app.uninstall_confirm.purge_confirm.clone();
    let field_focused = app.uninstall_confirm.purge_field_focused;
    let error = app.uninstall_confirm.error.clone();

    let block = Block::default()
        .title(" Uninstall Supernode ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Magenta));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(4), Constraint::Length(3)])
        .split(inner);
    let content = chunks[0];
    let content_bottom = content.y.saturating_add(content.height);

    let mut y = content.y;
    let draw_line = |frame: &mut Frame, y: u16, line: Line<'_>| -> Option<u16> {
        if y >= content_bottom {
            return None;
        }
        frame.render_widget(
            Paragraph::new(line),
            Rect::new(content.x, y, content.width, 1),
        );
        Some(y.saturating_add(1))
    };

    if let Some(next) = draw_line(
        frame,
        y,
        Line::from(vec![
            Span::raw("Uninstall "),
            Span::styled(label.as_str(), Style::default().fg(Color::Yellow).bold()),
            Span::raw(" on the remote host (stop/disable service)?"),
        ]),
    ) {
        y = next;
    }
    if let Some(next) = draw_line(
        frame,
        y,
        Line::from(vec![
            Span::raw("Data directory: "),
            Span::styled(data_dir.as_str(), Style::default().fg(Color::Gray)),
        ]),
    ) {
        y = next;
    }
    if let Some(next) = draw_line(
        frame,
        y,
        Line::from(Span::styled(
            if purge {
                "Purge checked — type the id below, then click Confirm Uninstall."
            } else {
                "Click Confirm Uninstall below (no typing needed)."
            },
            Style::default().fg(Color::DarkGray),
        )),
    ) {
        y = next;
    }

    if let Some(err) = &error {
        if let Some(next) = draw_line(
            frame,
            y,
            Line::from(Span::styled(
                format!("{err}"),
                Style::default().fg(Color::Red).bold(),
            )),
        ) {
            y = next;
        }
    }

    if y < content_bottom {
        let purge_label = if purge {
            "[x] Delete remote data directory (purge)"
        } else {
            "[ ] Delete remote data directory (purge)"
        };
        let purge_rect = Rect::new(content.x, y, content.width, 1);
        let purge_style =
            if app.hover_target == Some(ClickTarget::Button(ButtonId::UninstallPurgeToggle)) {
                Style::default().fg(Color::Black).bg(Color::Magenta).bold()
            } else if purge {
                Style::default().fg(Color::Magenta).bold()
            } else {
                Style::default().fg(Color::DarkGray)
            };
        frame.render_widget(Paragraph::new(purge_label).style(purge_style), purge_rect);
        app.register_hit_zone(
            purge_rect,
            ClickTarget::Button(ButtonId::UninstallPurgeToggle),
        );
        y = y.saturating_add(1);
    }

    if purge && y.saturating_add(1) < content_bottom {
        if let Some(next) = draw_line(
            frame,
            y,
            Line::from(format!("Type '{instance_id}' or '{label}':"))
                .style(Style::default().fg(Color::Magenta)),
        ) {
            y = next;
        }
        if y < content_bottom {
            let value_rect = Rect::new(
                content.x.saturating_add(2),
                y,
                content.width.saturating_sub(2),
                1,
            );
            let display = format!("{purge_confirm}_");
            let field_style = if field_focused {
                Style::default()
                    .fg(Color::Yellow)
                    .bg(Color::Rgb(45, 48, 68))
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default()
                    .fg(Color::Yellow)
                    .bg(Color::Rgb(35, 38, 58))
            };
            frame.render_widget(Paragraph::new(display).style(field_style), value_rect);
            app.register_hit_zone(value_rect, ClickTarget::UninstallConfirmField);
        }
    }

    let button_row = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(55), Constraint::Percentage(45)])
        .split(chunks[1]);

    draw_form_button(
        frame,
        app,
        button_row[0],
        ButtonId::ConfirmUninstall,
        " Confirm Uninstall ",
        Color::Magenta,
    );
    draw_form_button(
        frame,
        app,
        button_row[1],
        ButtonId::CancelUninstall,
        " Cancel ",
        Color::DarkGray,
    );
}

fn draw_node_form(frame: &mut Frame, area: Rect, app: &mut App) {
    let hint = if app.node_form.is_edit() {
        "Topology only — use Configs (C) for supernode.toml settings."
    } else {
        "Empty port fields auto-allocate. Use Configs (C) after adding for features."
    };
    let fields: Vec<(ClickTarget, &str, String)> = NODE_FORM_FIELDS
        .iter()
        .map(|f| {
            (
                ClickTarget::FormField(*f),
                f.label(),
                app.node_form.field_mut(*f).clone(),
            )
        })
        .collect();
    let title = app.node_form.title().to_string();
    let focused = app.node_form.focused_field;
    let scroll = app.node_form.scroll;
    let error = app.node_form.error.clone();
    draw_scroll_form(
        frame,
        area,
        app,
        &title,
        hint,
        &fields,
        focused,
        scroll,
        error.as_deref(),
        ButtonId::SaveNode,
        ButtonId::CancelNode,
    );
}

fn draw_node_config_form(frame: &mut Frame, area: Rect, app: &mut App) {
    let form = app.node_config_form.clone();
    let block = Block::default()
        .title(format!(" Supernode Config — {} ", form.label))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let mut y = inner.y.saturating_add(1);
    frame.render_widget(
        Paragraph::new(Span::styled(
            form.effective_hint,
            Style::default().fg(Color::DarkGray),
        )),
        Rect::new(inner.x, y, inner.width, 1),
    );
    y = y.saturating_add(2);

    if let Some(err) = &form.error {
        frame.render_widget(
            Paragraph::new(Span::styled(
                format!("Error: {err}"),
                Style::default().fg(Color::Red),
            )),
            Rect::new(inner.x, y, inner.width, 1),
        );
        y = y.saturating_add(2);
    }

    let buttons_y = inner.y.saturating_add(inner.height.saturating_sub(3));
    let visible_rows = buttons_y.saturating_sub(y) as usize;
    let total = config_field_count();
    let scroll = form.scroll as usize;

    for index in scroll..total {
        if index - scroll >= visible_rows {
            break;
        }
        let label = config_field_label(index);
        let focused = form.focused_field == index;
        let display = if config_field_is_toggle(index) {
            let on = if super::app::config_field_is_room_policy(index) {
                match index {
                    4 => form.allow_public_rooms,
                    5 => form.allow_private_rooms,
                    _ => false,
                }
            } else {
                let toggle_idx = index - super::app::CONFIG_FEATURE_TOGGLES_START;
                form.feature_toggles
                    .get(toggle_idx)
                    .copied()
                    .unwrap_or(false)
            };
            format!("[{}] {label}", if on { "x" } else { " " })
        } else {
            let v = match index {
                0 => form.listen_bind.clone(),
                1 => form.access_mode.clone(),
                2 => form.identity_file.clone(),
                3 => form.extra_features.clone(),
                _ => String::new(),
            };
            if focused {
                format!("{v}_")
            } else if v.is_empty() {
                "— (inherit from defaults)".into()
            } else {
                v
            }
        };

        let label_rect = Rect::new(inner.x, y, 28, 1);
        let value_rect = Rect::new(
            inner.x.saturating_add(28),
            y,
            inner.width.saturating_sub(28),
            1,
        );
        if config_field_is_toggle(index) {
            frame.render_widget(
                Paragraph::new(display).style(if focused {
                    Style::default()
                        .fg(Color::Yellow)
                        .bg(Color::Rgb(35, 38, 58))
                } else {
                    Style::default().fg(Color::Gray)
                }),
                Rect::new(inner.x, y, inner.width, 1),
            );
        } else {
            frame.render_widget(
                Paragraph::new(format!("{label}:")).style(Style::default().fg(Color::Cyan)),
                label_rect,
            );
            frame.render_widget(
                Paragraph::new(display).style(if focused {
                    Style::default()
                        .fg(Color::Yellow)
                        .bg(Color::Rgb(35, 38, 58))
                } else {
                    Style::default().fg(Color::Gray)
                }),
                value_rect,
            );
        }
        app.register_hit_zone(
            Rect::new(inner.x, y, inner.width, 1),
            ClickTarget::ConfigField(index),
        );
        y = y.saturating_add(1);
    }

    draw_form_button(
        frame,
        app,
        Rect::new(inner.x, buttons_y, 14, 1),
        ButtonId::SaveNodeConfig,
        " Save ",
        Color::Green,
    );
    draw_form_button(
        frame,
        app,
        Rect::new(inner.x.saturating_add(16), buttons_y, 16, 1),
        ButtonId::CancelNodeConfig,
        " Cancel ",
        Color::Red,
    );

    let hint_y = buttons_y.saturating_add(2);
    if hint_y < inner.y.saturating_add(inner.height) {
        frame.render_widget(
            Paragraph::new(
                "Enter save  ·  Esc cancel  ·  Space toggle feature  ·  Tab / scroll fields",
            )
            .style(Style::default().fg(Color::DarkGray)),
            Rect::new(inner.x, hint_y, inner.width, 1),
        );
    }
}

fn draw_settings_form(frame: &mut Frame, area: Rect, app: &mut App) {
    let fields: Vec<(ClickTarget, &str, String)> = SETTINGS_FIELDS
        .iter()
        .map(|f| {
            (
                ClickTarget::SettingsField(*f),
                f.label(),
                app.settings_form.field_mut(*f).clone(),
            )
        })
        .collect();
    let focused = app.settings_form.focused_field;
    let scroll = app.settings_form.scroll;
    let error = app.settings_form.error.clone();
    draw_scroll_form(
        frame,
        area,
        app,
        " Fleet Settings ",
        "[defaults] install source, SSH/systemd settings, and supernode manifest defaults",
        &fields,
        focused,
        scroll,
        error.as_deref(),
        ButtonId::SaveSettings,
        ButtonId::CancelSettings,
    );
}

fn draw_scroll_form(
    frame: &mut Frame,
    area: Rect,
    app: &mut App,
    title: &str,
    hint: &str,
    fields: &[(ClickTarget, &str, String)],
    focused_field: usize,
    scroll: u16,
    error: Option<&str>,
    save_id: ButtonId,
    cancel_id: ButtonId,
) {
    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let mut y = inner.y.saturating_add(1);
    frame.render_widget(
        Paragraph::new(Span::styled(hint, Style::default().fg(Color::DarkGray))),
        Rect::new(inner.x, y, inner.width, 1),
    );
    y = y.saturating_add(2);

    if let Some(err) = error {
        frame.render_widget(
            Paragraph::new(Span::styled(
                format!("Error: {err}"),
                Style::default().fg(Color::Red),
            )),
            Rect::new(inner.x, y, inner.width, 1),
        );
        y = y.saturating_add(2);
    }

    let buttons_y = inner.y.saturating_add(inner.height.saturating_sub(3));
    let visible_rows = buttons_y.saturating_sub(y) as usize;
    let total = fields.len();
    let scroll = scroll.min(total.saturating_sub(1) as u16) as usize;

    for (row, (target, label, value)) in fields.iter().enumerate().skip(scroll) {
        if row - scroll >= visible_rows {
            break;
        }
        let label_rect = Rect::new(inner.x, y, 28, 1);
        let value_rect = Rect::new(
            inner.x.saturating_add(28),
            y,
            inner.width.saturating_sub(28),
            1,
        );
        let focused = focused_field == row;

        frame.render_widget(
            Paragraph::new(format!("{label}:")).style(Style::default().fg(Color::Cyan)),
            label_rect,
        );

        let display = if focused {
            format!("{value}_")
        } else if value.is_empty() {
            "—".into()
        } else {
            value.clone()
        };
        let value_style = if focused {
            Style::default()
                .fg(Color::Yellow)
                .bg(Color::Rgb(35, 38, 58))
        } else {
            Style::default().fg(Color::Gray)
        };
        frame.render_widget(Paragraph::new(display).style(value_style), value_rect);
        app.register_hit_zone(Rect::new(inner.x, y, inner.width, 1), target.clone());
        y = y.saturating_add(1);
    }

    draw_form_button(
        frame,
        app,
        Rect::new(inner.x, buttons_y, 14, 1),
        save_id,
        " Save ",
        Color::Green,
    );
    draw_form_button(
        frame,
        app,
        Rect::new(inner.x.saturating_add(16), buttons_y, 16, 1),
        cancel_id,
        " Cancel ",
        Color::Red,
    );

    let hint_y = buttons_y.saturating_add(2);
    if hint_y < inner.y.saturating_add(inner.height) {
        frame.render_widget(
            Paragraph::new("Enter save  ·  Esc cancel  ·  Tab fields  ·  scroll for more")
                .style(Style::default().fg(Color::DarkGray)),
            Rect::new(inner.x, hint_y, inner.width, 1),
        );
    }
}

fn draw_form_button(
    frame: &mut Frame,
    app: &mut App,
    rect: Rect,
    id: ButtonId,
    label: &str,
    accent: Color,
) {
    let hovered = app.hover_target == Some(ClickTarget::Button(id));
    let style = if hovered {
        Style::default().fg(Color::Black).bg(accent).bold()
    } else {
        Style::default().fg(accent).bg(Color::Rgb(28, 32, 48))
    };
    frame.render_widget(
        Paragraph::new(label)
            .style(style)
            .alignment(Alignment::Center),
        rect,
    );
    app.register_hit_zone(rect, ClickTarget::Button(id));
}

fn draw_logs(frame: &mut Frame, area: Rect, app: &App) {
    let label = app
        .selected_row()
        .map(|r| r.label())
        .unwrap_or_else(|| "—".into());

    let view_label = match app.logs_view {
        LogsView::Invite => "Invite",
        LogsView::Journal => "Logs",
        LogsView::Operation => "Operation",
    };

    let visible = area.height.saturating_sub(2).max(1);
    let total = app.logs_line_count();
    let scroll_hint = if total > visible {
        format!(
            "  ·  {}–{}/{}",
            app.logs_scroll + 1,
            (app.logs_scroll + visible).min(total),
            total
        )
    } else {
        String::new()
    };

    let block = Block::default()
        .title(format!(" {view_label} · {label}{scroll_hint} "))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::DarkGray));

    let inner = block.inner(area);
    frame.render_widget(block, area);

    let lines: Vec<Line> = app.logs_text.lines().map(styled_log_line).collect();

    let paragraph = Paragraph::new(lines)
        .wrap(Wrap { trim: false })
        .scroll((app.logs_scroll, 0))
        .style(Style::default().fg(Color::Gray));

    frame.render_widget(paragraph, inner);

    if total > visible {
        let mut scroll_state = ratatui::widgets::ScrollbarState::new(total as usize)
            .position(app.logs_scroll as usize);
        frame.render_stateful_widget(
            Scrollbar::new(ScrollbarOrientation::VerticalRight)
                .thumb_symbol("█")
                .track_symbol(Some("│")),
            inner,
            &mut scroll_state,
        );
    }
}

fn styled_log_line(line: &str) -> Line<'static> {
    let Some(start) = line.find("conquerd://") else {
        return Line::from(line.to_string());
    };
    let rest = &line[start..];
    let end = rest.find(|c: char| c.is_whitespace()).unwrap_or(rest.len());
    let mut spans = Vec::new();
    if start > 0 {
        spans.push(Span::raw(line[..start].to_string()));
    }
    spans.push(Span::styled(
        rest[..end].to_string(),
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    ));
    if end < rest.len() {
        spans.push(Span::raw(rest[end..].to_string()));
    }
    Line::from(spans)
}

fn help_lines(app: &App) -> Vec<Line<'static>> {
    vec![
        Line::from(Span::styled(
            "Navigation",
            Style::default().fg(Color::Cyan).bold(),
        )),
        Line::from("  ↑/k, ↓/j     move selection"),
        Line::from("  click row     select instance"),
        Line::from("  Esc           back to fleet view"),
        Line::from(""),
        Line::from(Span::styled(
            "Fleet actions",
            Style::default().fg(Color::Cyan).bold(),
        )),
        Line::from("  n  add node       e  edit node (topology)"),
        Line::from("  C  supernode configs   G  fleet defaults"),
        Line::from("  P  push config to host"),
        Line::from("  r  refresh         c  SSH connect"),
        Line::from("  p  ping host       i  install"),
        Line::from("  s  start           x  stop"),
        Line::from("  R  restart         u  uninstall"),
        Line::from("  l  logs            v  invite link"),
        Line::from("  d  remove (inventory only)"),
        Line::from(""),
        Line::from(Span::styled(
            "Logs / Invite view",
            Style::default().fg(Color::Cyan).bold(),
        )),
        Line::from("  y  copy to clipboard"),
        Line::from("  o  open plain text for mouse selection"),
        Line::from("  ↑↓/jk  scroll   Home/End  top/bottom"),
        Line::from(""),
        Line::from(Span::styled(
            "Forms & confirms",
            Style::default().fg(Color::Cyan).bold(),
        )),
        Line::from("  click fields, type, Enter to save"),
        Line::from("  uninstall: click Confirm (type only if purge checked)"),
        Line::from("  remove: drops inventory only — uninstall host first"),
        Line::from(""),
        Line::from(Span::styled(
            "General",
            Style::default().fg(Color::Cyan).bold(),
        )),
        Line::from("  ?  toggle help     q  quit"),
        Line::from(""),
        Line::from(format!(
            "Auto-refresh every {}s when idle on the fleet view.",
            app.auto_refresh_secs
        )),
    ]
}

fn draw_help(frame: &mut Frame, area: Rect, app: &App) {
    let lines = help_lines(app);
    let block = Block::default()
        .title(" Help ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::DarkGray));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    frame.render_widget(
        Paragraph::new(lines)
            .scroll((app.help_scroll, 0))
            .alignment(Alignment::Left),
        inner,
    );

    let total = help_lines(app).len() as u16;
    let visible = inner.height;
    if total > visible {
        let mut scroll_state = ratatui::widgets::ScrollbarState::new(total as usize)
            .position(app.help_scroll as usize);
        frame.render_stateful_widget(
            Scrollbar::new(ScrollbarOrientation::VerticalRight),
            inner,
            &mut scroll_state,
        );
    }
}

fn draw_footer(frame: &mut Frame, area: Rect, app: &App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(2), Constraint::Length(1)])
        .split(area);

    let detail = app.selected_row().map(|row| {
        format!(
            "{}  |  {}  |  {}/{}{}",
            row.label(),
            row.public_host,
            row.relay_port,
            row.ws_port,
            row.web_port.map(|p| format!("/{p}")).unwrap_or_default()
        )
    });

    let status_max = chunks[0].width.saturating_sub(2).max(12) as usize;
    let status_text = truncate_end(&app.status_line, status_max.min(88));

    let mut status_spans = vec![Span::styled(
        format!(" {status_text} "),
        Style::default().fg(Color::Yellow),
    )];
    if app.panel == Panel::Fleet {
        if let Some(detail) = detail {
            status_spans.push(Span::styled(
                format!("  {}", truncate_end(&detail, 72)),
                Style::default().fg(Color::DarkGray),
            ));
        }
    }

    frame.render_widget(
        Paragraph::new(Line::from(status_spans)).block(
            Block::default()
                .borders(Borders::TOP)
                .border_style(Style::default().fg(Color::DarkGray)),
        ),
        chunks[0],
    );

    let keys = footer_keys(app.panel);
    frame.render_widget(
        Paragraph::new(Span::styled(
            format!(" {keys}"),
            Style::default().fg(Color::DarkGray),
        )),
        chunks[1],
    );
}

fn footer_keys(panel: Panel) -> &'static str {
    match panel {
        Panel::Fleet => "? help | n add | i install | s/x/R lifecycle | l logs | v invite | q quit",
        Panel::NodeForm => "Enter save | Esc cancel | Tab fields",
        Panel::NodeConfig => "Enter save | Space toggle | Esc cancel | P push after save",
        Panel::Settings => "Enter save | Esc cancel | Tab / scroll fields",
        Panel::ConfirmRemove => "Enter confirm | Esc cancel",
        Panel::ConfirmUninstall => "Enter / click Confirm | p toggle purge | Esc cancel",
        Panel::Logs => "y copy | o select | scroll | Esc back | ? help",
        Panel::Help => "Up/Down scroll | Esc/? close | q quit",
    }
}

fn status_style(status: &RowStatus) -> Style {
    match status {
        RowStatus::Active(_) => Style::default().fg(Color::Green),
        RowStatus::Inactive(_) => Style::default().fg(Color::Red),
        RowStatus::Loading => Style::default().fg(Color::Yellow),
        RowStatus::Error(_) => Style::default().fg(Color::Red).bold(),
        RowStatus::Unknown => Style::default().fg(Color::DarkGray),
    }
}
