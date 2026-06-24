use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    Frame,
    layout::{Alignment, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Padding},
};

use crate::action::Action;

/// Describes one provider or agent source, as known to config at startup.
/// `Provider { models: None }` means the provider's model list is not known yet
/// and must be fetched lazily when the user expands its node in the picker.
pub enum ModelSourceConfig {
    Provider { name: String, models: Option<Vec<String>> },
    Agent { name: String },
}

/// A top-level node in the provider/model tree.
struct ProviderNode {
    name: String,
    is_agent: bool,
    expanded: bool,
    /// True while an async fetch for this node's models is in flight.
    loading: bool,
    /// `None` until the model list is known (config or a completed fetch).
    models: Option<Vec<String>>,
}

/// One visible row in the picker, indexing into `ModelPicker::nodes`.
#[derive(Clone, Copy)]
enum Row {
    /// A provider/agent header row.
    Provider(usize),
    /// A placeholder row shown while a provider's models are being fetched.
    Loading,
    /// A placeholder row shown when a provider has no models.
    Empty,
    /// A selectable model row: (node index, model index within that node).
    Model(usize, usize),
}

/// A filterable provider → model tree picker overlay.
///
/// With an empty filter, shows all configured providers/agents as expandable
/// disclosure nodes; expanding a provider whose models aren't known yet triggers
/// a lazy fetch via `Action::RequestProviderModels`. With a non-empty filter,
/// flattens to a filtered list of models across all providers (matching the
/// previous search UX) — only providers whose models are already known are
/// searchable, since fetching happens only on manual expand.
///
/// Opened by the "Switch Model" command in the command palette.
pub struct ModelPicker {
    /// Whether this overlay is currently visible.
    pub visible: bool,
    /// The current filter text.
    filter: String,
    /// Provider/agent nodes, in config order.
    nodes: Vec<ProviderNode>,
    /// Name of the currently active provider/agent.
    active_provider: String,
    /// The currently active model identifier.
    active_model: String,
    /// Rows currently visible, recomputed on every structural change.
    rows: Vec<Row>,
    /// Selection state over `rows`.
    list_state: ListState,
}

impl ModelPicker {
    pub fn new() -> Self {
        Self {
            visible: false,
            filter: String::new(),
            nodes: Vec::new(),
            active_provider: String::new(),
            active_model: String::new(),
            rows: Vec::new(),
            list_state: ListState::default(),
        }
    }

    /// Initialize the provider/agent tree. Called once after config is loaded.
    /// Preserves any already-known or in-flight model data for providers that
    /// persist across calls (defensive against being called more than once).
    pub fn set_sources(&mut self, sources: Vec<ModelSourceConfig>) {
        let old = std::mem::take(&mut self.nodes);
        self.nodes = sources
            .into_iter()
            .map(|s| match s {
                ModelSourceConfig::Provider { name, models } => {
                    let preserved = old
                        .iter()
                        .find(|n| !n.is_agent && n.name == name)
                        .and_then(|n| n.models.clone());
                    ProviderNode {
                        name,
                        is_agent: false,
                        expanded: false,
                        loading: false,
                        models: models.or(preserved),
                    }
                }
                ModelSourceConfig::Agent { name } => ProviderNode {
                    name,
                    is_agent: true,
                    expanded: false,
                    loading: false,
                    models: Some(vec!["default".to_string()]),
                },
            })
            .collect();
        self.rebuild_rows();
    }

    /// Update the active provider/model indicator. Cheap — does not touch the tree.
    pub fn set_active(&mut self, provider: &str, model: &str) {
        self.active_provider = provider.to_string();
        self.active_model = model.to_string();
    }

    /// Supply a provider's model list, e.g. once a lazy fetch completes, or
    /// opportunistically after switching to a provider that just discovered its
    /// own models. Clears any in-flight loading indicator.
    pub fn set_provider_models(&mut self, name: &str, models: Vec<String>) {
        if let Some(node) = self.nodes.iter_mut().find(|n| !n.is_agent && n.name == name) {
            node.loading = false;
            node.models = Some(models);
            self.rebuild_rows();
        }
    }

    /// Show the model picker. Auto-expands the active provider's node so the
    /// current model is immediately visible, requesting a fetch if needed.
    pub fn show(&mut self) -> Option<Action> {
        self.visible = true;
        self.filter.clear();

        let active_provider = self.active_provider.clone();
        let mut fetch_request = None;
        if let Some(idx) = self.nodes.iter().position(|n| n.name == active_provider) {
            let node = &mut self.nodes[idx];
            if !node.expanded {
                node.expanded = true;
                if !node.is_agent && node.models.is_none() && !node.loading {
                    node.loading = true;
                    fetch_request = Some(node.name.clone());
                }
            }
        }

        self.rebuild_rows();
        self.select_active_row();
        fetch_request.map(Action::RequestProviderModels)
    }

    /// Hide the model picker.
    pub fn hide(&mut self) {
        self.visible = false;
        self.filter.clear();
    }

    /// Recompute `rows` from `nodes` and the current filter, resetting selection to 0.
    fn rebuild_rows(&mut self) {
        self.rows.clear();
        let query = self.filter.to_lowercase();

        if query.is_empty() {
            // Tree mode: provider headers, with children shown only when expanded.
            for (ni, node) in self.nodes.iter().enumerate() {
                self.rows.push(Row::Provider(ni));
                if !node.expanded {
                    continue;
                }
                match &node.models {
                    None => self.rows.push(Row::Loading),
                    Some(models) if models.is_empty() => self.rows.push(Row::Empty),
                    Some(models) => {
                        for mi in 0..models.len() {
                            self.rows.push(Row::Model(ni, mi));
                        }
                    }
                }
            }
        } else {
            // Flattened filtered mode: only models already known are searchable.
            for (ni, node) in self.nodes.iter().enumerate() {
                let Some(models) = &node.models else { continue };
                for (mi, m) in models.iter().enumerate() {
                    if m.to_lowercase().contains(&query) || node.name.to_lowercase().contains(&query) {
                        self.rows.push(Row::Model(ni, mi));
                    }
                }
            }
        }

        if self.rows.is_empty() {
            self.list_state.select(None);
        } else {
            self.list_state.select(Some(0));
        }
    }

    /// Select the row for the active provider or, if already expanded with a
    /// known model list, the active model row within it.
    fn select_active_row(&mut self) {
        let active_provider = self.active_provider.clone();
        let active_model = self.active_model.clone();

        let model_row = self.rows.iter().position(|r| match r {
            Row::Model(ni, mi) => {
                let node = &self.nodes[*ni];
                node.name == active_provider
                    && node.models.as_ref().and_then(|m| m.get(*mi)) == Some(&active_model)
            }
            _ => false,
        });
        if let Some(idx) = model_row {
            self.list_state.select(Some(idx));
            return;
        }

        self.select_provider_row(&active_provider);
    }

    /// Select the header row for the named provider, if present among the current rows.
    fn select_provider_row(&mut self, name: &str) {
        if let Some(idx) = self
            .rows
            .iter()
            .position(|r| matches!(r, Row::Provider(ni) if self.nodes[*ni].name == name))
        {
            self.list_state.select(Some(idx));
        }
    }

    /// The row currently under the selection cursor, if any.
    fn current_row(&self) -> Option<Row> {
        self.list_state.selected().and_then(|i| self.rows.get(i)).copied()
    }

    /// Expand or collapse a provider/agent node. Returns an action to fetch its
    /// models if expanding reveals that they aren't known yet.
    fn toggle_node(&mut self, ni: usize) -> Action {
        let name = self.nodes[ni].name.clone();
        let node = &mut self.nodes[ni];

        if node.expanded {
            node.expanded = false;
            self.rebuild_rows();
            self.select_provider_row(&name);
            return Action::Noop;
        }

        node.expanded = true;
        let needs_fetch = !node.is_agent && node.models.is_none() && !node.loading;
        if needs_fetch {
            node.loading = true;
        }
        self.rebuild_rows();
        self.select_provider_row(&name);

        if needs_fetch {
            Action::RequestProviderModels(name)
        } else {
            Action::Noop
        }
    }

    /// Move selection up by one.
    fn select_previous(&mut self) {
        if self.rows.is_empty() {
            return;
        }
        let i = match self.list_state.selected() {
            Some(0) => self.rows.len() - 1,
            Some(i) => i - 1,
            None => 0,
        };
        self.list_state.select(Some(i));
    }

    /// Move selection down by one.
    fn select_next(&mut self) {
        if self.rows.is_empty() {
            return;
        }
        let i = match self.list_state.selected() {
            Some(i) if i >= self.rows.len() - 1 => 0,
            Some(i) => i + 1,
            None => 0,
        };
        self.list_state.select(Some(i));
    }

    /// Handle Enter: toggle a provider row, or select a model row.
    fn handle_enter(&mut self) -> Action {
        match self.current_row() {
            Some(Row::Provider(ni)) => self.toggle_node(ni),
            Some(Row::Model(ni, mi)) => {
                let node = &self.nodes[ni];
                let model = node.models.as_ref().and_then(|m| m.get(mi)).cloned();
                let Some(model) = model else { return Action::Noop };
                let provider = node.name.clone();
                self.hide();
                Action::SwitchModel(model, provider)
            }
            Some(Row::Loading) | Some(Row::Empty) | None => Action::Noop,
        }
    }

    /// Handle a key event while the picker is visible.
    pub fn handle_key_event(&mut self, key: KeyEvent) -> Action {
        if !self.visible {
            return Action::Noop;
        }

        match (key.code, key.modifiers) {
            // Close
            (KeyCode::Esc, _) => Action::CloseModelPicker,

            // Quit still works
            (KeyCode::Char('q'), m) if m.contains(KeyModifiers::CONTROL) => Action::Quit,

            // Navigation
            (KeyCode::Up, _) => {
                self.select_previous();
                Action::Noop
            }
            (KeyCode::Down, _) => {
                self.select_next();
                Action::Noop
            }

            // Explicit expand/collapse (only meaningful in tree mode)
            (KeyCode::Right, _) => {
                if let Some(Row::Provider(ni)) = self.current_row() {
                    if !self.nodes[ni].expanded {
                        return self.toggle_node(ni);
                    }
                }
                Action::Noop
            }
            (KeyCode::Left, _) => {
                if let Some(Row::Provider(ni)) = self.current_row() {
                    if self.nodes[ni].expanded {
                        return self.toggle_node(ni);
                    }
                }
                Action::Noop
            }

            // Select model / toggle provider disclosure
            (KeyCode::Enter, _) => self.handle_enter(),

            // Backspace: delete filter char
            (KeyCode::Backspace, _) => {
                self.filter.pop();
                self.rebuild_rows();
                Action::Noop
            }

            // Printable characters: append to filter
            (KeyCode::Char(c), m) if !m.contains(KeyModifiers::CONTROL) => {
                self.filter.push(c);
                self.rebuild_rows();
                Action::Noop
            }

            _ => Action::Noop,
        }
    }

    /// Render the model picker as a centered overlay.
    pub fn render_overlay(&mut self, frame: &mut Frame, area: Rect) {
        if !self.visible {
            return;
        }

        // Calculate overlay dimensions: 50% width, 50% height, centered
        let overlay_width = (area.width * 50 / 100).max(30).min(area.width);
        let overlay_height = (area.height * 50 / 100).max(8).min(area.height);

        let x = (area.width.saturating_sub(overlay_width)) / 2 + area.x;
        let y = (area.height.saturating_sub(overlay_height)) / 2 + area.y;
        let overlay_area = Rect::new(x, y, overlay_width, overlay_height);

        // Clear the background
        frame.render_widget(Clear, overlay_area);

        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Magenta))
            .title(" Switch Model ")
            .title_alignment(Alignment::Center)
            .padding(Padding::horizontal(1));

        let inner = block.inner(overlay_area);

        // Build the filter input line
        let filter_line = Line::from(vec![
            Span::styled("> ", Style::default().fg(Color::Magenta).bold()),
            Span::styled(&self.filter, Style::default().fg(Color::White)),
            Span::styled("_", Style::default().fg(Color::DarkGray)),
        ]);

        let tree_mode = self.filter.is_empty();
        let available = inner.width.saturating_sub(2) as usize;

        let items: Vec<ListItem> = self
            .rows
            .iter()
            .map(|row| self.render_row(*row, tree_mode, available))
            .collect();

        // Render the block
        frame.render_widget(block.clone(), overlay_area);

        if inner.height < 2 {
            return;
        }

        // Render filter input on the first line of inner area
        let filter_area = Rect::new(inner.x, inner.y, inner.width, 1);
        frame.render_widget(ratatui::widgets::Paragraph::new(filter_line), filter_area);

        // Render list below the filter
        let list_area = Rect::new(inner.x, inner.y + 1, inner.width, inner.height - 1);

        if items.is_empty() {
            let no_match = ratatui::widgets::Paragraph::new(Line::from(Span::styled(
                "No matching models",
                Style::default().fg(Color::DarkGray),
            )))
            .alignment(Alignment::Center);
            frame.render_widget(no_match, list_area);
        } else {
            let list = List::new(items)
                .highlight_style(
                    Style::default()
                        .bg(Color::DarkGray)
                        .fg(Color::White)
                        .add_modifier(Modifier::BOLD),
                )
                .highlight_symbol("> ");

            frame.render_stateful_widget(list, list_area, &mut self.list_state);
        }

        // Help line at the bottom of overlay
        let help_area = Rect {
            x: overlay_area.x + 1,
            y: overlay_area.y + overlay_area.height - 1,
            width: overlay_area.width.saturating_sub(2),
            height: 1,
        };
        let help_text = if tree_mode {
            vec![
                Span::styled("Enter/→/←", Style::default().fg(Color::Magenta)),
                Span::raw(" expand  "),
                Span::styled("Enter", Style::default().fg(Color::Magenta)),
                Span::raw(" select  "),
                Span::styled("Esc", Style::default().fg(Color::Magenta)),
                Span::raw(" cancel"),
            ]
        } else {
            vec![
                Span::styled("Enter", Style::default().fg(Color::Magenta)),
                Span::raw(" select  "),
                Span::styled("Esc", Style::default().fg(Color::Magenta)),
                Span::raw(" cancel"),
            ]
        };
        let help = ratatui::widgets::Paragraph::new(Line::from(help_text)).alignment(Alignment::Center);
        frame.render_widget(help, help_area);
    }

    /// Build a single list item for a row, given the current rendering mode.
    fn render_row(&self, row: Row, tree_mode: bool, available: usize) -> ListItem<'static> {
        match row {
            Row::Provider(ni) => {
                let node = &self.nodes[ni];
                let arrow = if node.expanded { "▾ " } else { "▸ " };
                let is_active = node.name == self.active_provider;
                let style = if is_active {
                    Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(Color::White).add_modifier(Modifier::BOLD)
                };
                ListItem::new(Line::from(vec![
                    Span::styled(arrow, Style::default().fg(Color::DarkGray)),
                    Span::styled(node.name.clone(), style),
                ]))
            }
            Row::Loading => ListItem::new(Line::from(Span::styled(
                "    (loading…)",
                Style::default().fg(Color::DarkGray),
            ))),
            Row::Empty => ListItem::new(Line::from(Span::styled(
                "    (no models)",
                Style::default().fg(Color::DarkGray),
            ))),
            Row::Model(ni, mi) => {
                let node = &self.nodes[ni];
                let model = node
                    .models
                    .as_ref()
                    .and_then(|m| m.get(mi))
                    .cloned()
                    .unwrap_or_default();
                let active = node.name == self.active_provider && model == self.active_model;

                let prefix = if active { "* " } else { "  " };
                let prefix_style = if active {
                    Style::default().fg(Color::Green).bold()
                } else {
                    Style::default().fg(Color::DarkGray)
                };
                let model_style = if active {
                    Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(Color::White)
                };

                if tree_mode {
                    // Nested under its provider header; indent, no provider suffix.
                    ListItem::new(Line::from(vec![
                        Span::raw("  "),
                        Span::styled(prefix, prefix_style),
                        Span::styled(model, model_style),
                    ]))
                } else {
                    // Flattened search results; show provider name, right-aligned.
                    let name_len = prefix.len() + model.len();
                    let provider_len = node.name.len();
                    let padding = available.saturating_sub(name_len).saturating_sub(provider_len);
                    let pad_str = " ".repeat(padding);
                    ListItem::new(Line::from(vec![
                        Span::styled(prefix, prefix_style),
                        Span::styled(model, model_style),
                        Span::raw(pad_str),
                        Span::styled(node.name.clone(), Style::default().fg(Color::DarkGray)),
                    ]))
                }
            }
        }
    }
}
