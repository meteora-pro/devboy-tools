//! TUI Inventory view per [ADR-023] §3.4 (MVP views, Inventory).
//!
//! A sortable, filterable, value-free table of every secret the
//! active context can see. Per the ADR:
//!
//! > Columns: path, status (`provisioned` / `expiring` /
//! > `missing` / `format-invalid`), routed source, `expires_at`,
//! > provider, scope. Filters by scope, provider, status. **No
//! > values are ever shown.**
//!
//! ## Architecture
//!
//! Pure view-model + thin render. The data lives in
//! [`InventoryState`]; key handlers mutate the state via
//! `move_*` / `cycle_focus_*` / `set_filter_*`. The render
//! function is the only thing that touches `ratatui`. Tests
//! exercise the state machine without spinning up a terminal,
//! and a single snapshot test renders into `TestBackend` to
//! pin the layout shape.
//!
//! ## Daemon subscription
//!
//! The view subscribes to daemon status updates by accepting
//! [`DaemonStatus`] through [`InventoryState::apply_daemon_status`].
//! The actual transport (agent socket, polling, push) belongs in
//! P12+ orchestration; the inventory just renders whatever it's
//! told and provides a clean hook the orchestration can call.
//!
//! [ADR-023]: https://github.com/meteora-pro/devboy-tools/blob/main/docs/architecture/adr/ADR-023-secret-store-ux-layer.md

#[cfg(feature = "tui")]
use ratatui::Frame;
#[cfg(feature = "tui")]
use ratatui::layout::{Constraint, Direction, Layout, Rect};
#[cfg(feature = "tui")]
use ratatui::style::{Color, Modifier, Style};
#[cfg(feature = "tui")]
use ratatui::text::{Line, Span};
#[cfg(feature = "tui")]
use ratatui::widgets::{Block, Borders, Cell, Paragraph, Row, Table, TableState};

// =============================================================================
// Domain types
// =============================================================================

/// Per-row classification surfaced in the Inventory's status
/// column. Mirrors ADR-023 §3.4's enumerated states.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RowStatus {
    /// Path resolves and the value is currently usable.
    Provisioned,
    /// Path resolves; `expires_at` is within the doctor's
    /// 14-day informational window (P7.3 inherits the same
    /// threshold).
    Expiring,
    /// Path declared in the manifest but not provisioned at any
    /// configured source.
    Missing,
    /// Path resolves, value present, but doesn't match the
    /// declared `format_regex` / `pattern_id`.
    FormatInvalid,
    /// Doctor / liveness probe hasn't run yet, or returned
    /// something we can't classify cleanly.
    Unknown,
}

impl RowStatus {
    /// Lower-case, kebab-shaped label rendered in the table and
    /// used as the filter key. Pinned to the ADR strings so a
    /// downstream `--status=` flag, when it lands, doesn't have
    /// to translate the names.
    pub fn label(self) -> &'static str {
        match self {
            Self::Provisioned => "provisioned",
            Self::Expiring => "expiring",
            Self::Missing => "missing",
            Self::FormatInvalid => "format-invalid",
            Self::Unknown => "unknown",
        }
    }

    /// Sort weight — `Provisioned` last (least urgent),
    /// `Missing` first (most urgent). Used as a secondary sort
    /// key when two rows have the same `expires_at`.
    pub fn urgency(self) -> u8 {
        match self {
            Self::Missing => 0,
            Self::FormatInvalid => 1,
            Self::Expiring => 2,
            Self::Unknown => 3,
            Self::Provisioned => 4,
        }
    }
}

/// One inventory row. Constructed by the orchestration layer
/// from `merge_manifest` + the router's resolution + (eventually)
/// liveness probe results.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InventoryRow {
    /// ADR-020 path (e.g. `team/gitlab/token-deploy`).
    pub path: String,
    /// Classification — see [`RowStatus`].
    pub status: RowStatus,
    /// Source name that serves this path, or `None` when no
    /// route resolves.
    pub routed_source: Option<String>,
    /// `expires_at` from the global index (ISO 8601 date), or
    /// `None` when not declared.
    pub expires_at: Option<String>,
    /// Middle segment of the path, or whatever the orchestration
    /// layer derived as the provider hint.
    pub provider: Option<String>,
    /// First segment of the path (`team` / `personal` /
    /// `client-acme` / …).
    pub scope: String,
    /// Catalog source override badge for the GUI (P22.2). `None`
    /// when the path resolves to a bundled catalog (or to no
    /// catalog at all). When `Some`, the GUI renders a tiny chip
    /// next to the path so a team-pinned override is visible at
    /// a glance. Free-form text — typical values: `"user"`,
    /// `"project"`, `"url:host.example"`. The renderer maps the
    /// prefix (`user` / `project` / `url`) to a colour.
    pub catalog_override: Option<String>,
}

/// Daemon's current state — feeds the inventory header chip.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DaemonStatus {
    Unlocked,
    Locked,
    NotInstalled,
    Unknown,
}

impl DaemonStatus {
    pub fn label(self) -> &'static str {
        match self {
            Self::Unlocked => "daemon: unlocked",
            Self::Locked => "daemon: locked",
            Self::NotInstalled => "daemon: not installed",
            Self::Unknown => "daemon: ?",
        }
    }
}

/// Filter chips shown along the bottom of the view.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InventoryFilters {
    pub scope: Option<String>,
    pub provider: Option<String>,
    pub status: Option<RowStatus>,
}

impl InventoryFilters {
    /// `true` when no filter is active.
    pub fn is_empty(&self) -> bool {
        self.scope.is_none() && self.provider.is_none() && self.status.is_none()
    }
}

/// Sort key for the table's primary order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortKey {
    Path,
    /// Default per ADR-023 §3.4. Rows without an `expires_at`
    /// sink to the bottom; ties fall back to status urgency.
    ExpiresAt,
    Status,
}

impl SortKey {
    pub fn label(self) -> &'static str {
        match self {
            Self::Path => "path",
            Self::ExpiresAt => "expires_at",
            Self::Status => "status",
        }
    }
}

/// Which UI element currently has keyboard focus. Tab cycles
/// forward; Shift+Tab cycles backward.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Table,
    ScopeFilter,
    ProviderFilter,
    StatusFilter,
}

impl Focus {
    pub fn next(self) -> Self {
        match self {
            Self::Table => Self::ScopeFilter,
            Self::ScopeFilter => Self::ProviderFilter,
            Self::ProviderFilter => Self::StatusFilter,
            Self::StatusFilter => Self::Table,
        }
    }
    pub fn prev(self) -> Self {
        match self {
            Self::Table => Self::StatusFilter,
            Self::ScopeFilter => Self::Table,
            Self::ProviderFilter => Self::ScopeFilter,
            Self::StatusFilter => Self::ProviderFilter,
        }
    }
}

// =============================================================================
// View-model
// =============================================================================

/// Self-contained Inventory state. Built by the orchestration
/// layer, mutated by key handlers, consumed by [`render`].
#[derive(Debug, Clone)]
pub struct InventoryState {
    /// Every row the orchestration layer produced, before
    /// filtering.
    rows: Vec<InventoryRow>,
    /// Current filters.
    filters: InventoryFilters,
    /// Primary sort key. ADR-023 §3.4 defaults to `ExpiresAt`.
    sort_key: SortKey,
    /// Free-text query that further narrows `visible_rows`.
    /// Empty string == no query (every row matches). Matches
    /// case-insensitively across `path | scope | provider |
    /// catalog_override | routed_source`. The render layer
    /// owns the input widget; this is just the canonical
    /// holder so the filter logic can read it.
    query: String,
    /// Index into the *visible* rows list; clamped to
    /// `[0, visible_rows().len())` whenever filters or rows
    /// change.
    selected: usize,
    /// Which UI element has keyboard focus.
    focus: Focus,
    /// Daemon status — rendered in the header.
    daemon_status: DaemonStatus,
}

impl InventoryState {
    /// Build a new state from the orchestration layer's row list.
    /// Defaults: sort by `ExpiresAt`, focus on the table, daemon
    /// status `Unknown`, no query.
    pub fn new(rows: Vec<InventoryRow>) -> Self {
        Self {
            rows,
            filters: InventoryFilters::default(),
            sort_key: SortKey::ExpiresAt,
            query: String::new(),
            selected: 0,
            focus: Focus::Table,
            daemon_status: DaemonStatus::Unknown,
        }
    }

    pub fn rows(&self) -> &[InventoryRow] {
        &self.rows
    }
    pub fn filters(&self) -> &InventoryFilters {
        &self.filters
    }
    pub fn sort_key(&self) -> SortKey {
        self.sort_key
    }
    pub fn selected(&self) -> usize {
        self.selected
    }
    pub fn focus(&self) -> Focus {
        self.focus
    }
    pub fn daemon_status(&self) -> DaemonStatus {
        self.daemon_status
    }

    /// Free-text query currently applied to `visible_rows`.
    /// Empty string means no query — every row matches.
    pub fn query(&self) -> &str {
        &self.query
    }

    /// Replace the free-text query, clamp the selection. Render
    /// layer calls this whenever the search-bar text changes.
    pub fn set_query(&mut self, query: impl Into<String>) {
        self.query = query.into();
        self.clamp_selection();
    }

    /// Replace the row list and clamp the cursor. Called when
    /// the orchestration layer rebuilds the inventory after a
    /// `secrets.list` / liveness probe.
    pub fn replace_rows(&mut self, rows: Vec<InventoryRow>) {
        self.rows = rows;
        self.clamp_selection();
    }

    /// Apply a fresh daemon status (e.g. after a JSON-RPC
    /// `vault.status` reply).
    pub fn apply_daemon_status(&mut self, status: DaemonStatus) {
        self.daemon_status = status;
    }

    pub fn set_scope_filter(&mut self, value: Option<String>) {
        self.filters.scope = value;
        self.clamp_selection();
    }
    pub fn set_provider_filter(&mut self, value: Option<String>) {
        self.filters.provider = value;
        self.clamp_selection();
    }
    pub fn set_status_filter(&mut self, value: Option<RowStatus>) {
        self.filters.status = value;
        self.clamp_selection();
    }
    pub fn clear_filters(&mut self) {
        self.filters = InventoryFilters::default();
        self.clamp_selection();
    }

    pub fn set_sort_key(&mut self, key: SortKey) {
        self.sort_key = key;
        self.clamp_selection();
    }

    pub fn cycle_focus_forward(&mut self) {
        self.focus = self.focus.next();
    }
    pub fn cycle_focus_backward(&mut self) {
        self.focus = self.focus.prev();
    }

    /// Set the selection to a specific index. Clamped to the
    /// current visible row count. Used by the GUI backend's
    /// click-to-select; TUI key handlers prefer `move_up` /
    /// `move_down`.
    pub fn set_selected(&mut self, idx: usize) {
        let len = self.visible_rows().len();
        if len == 0 {
            self.selected = 0;
        } else {
            self.selected = idx.min(len - 1);
        }
    }

    pub fn move_up(&mut self) {
        if self.selected > 0 {
            self.selected -= 1;
        }
    }
    pub fn move_down(&mut self) {
        let visible = self.visible_rows().len();
        if visible > 0 && self.selected + 1 < visible {
            self.selected += 1;
        }
    }
    pub fn move_to_top(&mut self) {
        self.selected = 0;
    }
    pub fn move_to_bottom(&mut self) {
        let visible = self.visible_rows().len();
        self.selected = visible.saturating_sub(1);
    }

    /// Filtered + sorted view over `rows`. Borrows; no copy.
    pub fn visible_rows(&self) -> Vec<&InventoryRow> {
        let query_lc = self.query.trim().to_lowercase();
        let mut out: Vec<&InventoryRow> = self
            .rows
            .iter()
            .filter(|r| {
                self.filters.scope.as_deref().is_none_or(|s| r.scope == s)
                    && self
                        .filters
                        .provider
                        .as_deref()
                        .is_none_or(|p| r.provider.as_deref() == Some(p))
                    && self.filters.status.is_none_or(|s| r.status == s)
                    && (query_lc.is_empty() || row_matches_query(r, &query_lc))
            })
            .collect();
        match self.sort_key {
            SortKey::Path => out.sort_by(|a, b| a.path.cmp(&b.path)),
            SortKey::ExpiresAt => out.sort_by(|a, b| {
                // None (no expires_at) sinks to the bottom.
                let key_a = (a.expires_at.is_none(), &a.expires_at);
                let key_b = (b.expires_at.is_none(), &b.expires_at);
                key_a.cmp(&key_b).then_with(|| {
                    // Tie-break by status urgency so two
                    // identical-expiry rows order Missing before
                    // Provisioned.
                    a.status.urgency().cmp(&b.status.urgency())
                })
            }),
            SortKey::Status => out.sort_by_key(|r| r.status.urgency()),
        }
        out
    }

    /// Currently-selected row, after filters + sort.
    pub fn selected_row(&self) -> Option<InventoryRow> {
        self.visible_rows().get(self.selected).map(|r| (*r).clone())
    }

    fn clamp_selection(&mut self) {
        let visible = self.visible_rows().len();
        if visible == 0 {
            self.selected = 0;
        } else if self.selected >= visible {
            self.selected = visible - 1;
        }
    }
}

/// Case-insensitive substring match against the fields the
/// inventory's search bar searches over. `query_lc` is assumed
/// to be lower-cased + trimmed already (the caller does it
/// once per render).
///
/// Searched fields: path, scope, provider (if Some),
/// catalog_override (if Some), routed_source (if Some).
/// Deliberately does NOT search inside per-row metadata
/// (KdbxEntry custom strings, IndexEntry description) — those
/// can hold values, and we don't want a stray query like "sk_"
/// to highlight rows whose secret value starts with it.
pub fn row_matches_query(row: &InventoryRow, query_lc: &str) -> bool {
    if row.path.to_lowercase().contains(query_lc) || row.scope.to_lowercase().contains(query_lc) {
        return true;
    }
    if let Some(p) = row.provider.as_deref()
        && p.to_lowercase().contains(query_lc)
    {
        return true;
    }
    if let Some(c) = row.catalog_override.as_deref()
        && c.to_lowercase().contains(query_lc)
    {
        return true;
    }
    if let Some(s) = row.routed_source.as_deref()
        && s.to_lowercase().contains(query_lc)
    {
        return true;
    }
    false
}

// =============================================================================
// Rendering (TUI)
// =============================================================================

/// Render the Inventory view into a [`ratatui::Frame`]. Pure
/// function over [`InventoryState`]; no side effects, no I/O.
///
/// Layout:
///
/// ```text
/// ┌── Inventory ──────────────────── daemon: unlocked ──┐
/// │ PATH  STATUS  SOURCE  EXPIRES  PROVIDER  SCOPE       │
/// │ a/b/c missing keychain ―       gitlab    team        │
/// │ ...                                                  │
/// │                                                      │
/// │ scope: team    provider: gitlab    status: missing   │
/// │ Tab=focus  ↑/↓=move  s=sort  /=filter  q=quit        │
/// └──────────────────────────────────────────────────────┘
/// ```
#[cfg(feature = "tui")]
pub fn render(frame: &mut Frame<'_>, state: &InventoryState, area: Rect) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // header
            Constraint::Min(1),    // table
            Constraint::Length(3), // filter chips
            Constraint::Length(2), // help line
        ])
        .split(area);

    render_header(frame, state, chunks[0]);
    render_table(frame, state, chunks[1]);
    render_filters(frame, state, chunks[2]);
    render_help(frame, chunks[3]);
}

#[cfg(feature = "tui")]
fn render_header(frame: &mut Frame<'_>, state: &InventoryState, area: Rect) {
    let title = Line::from(vec![
        Span::styled(" Inventory ", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(format!("({} rows) ", state.visible_rows().len())),
    ]);
    let daemon = Span::styled(
        format!(" {} ", state.daemon_status().label()),
        Style::default().fg(daemon_status_color(state.daemon_status())),
    );
    let header_line = Line::from(vec![
        Span::raw(""),
        Span::raw("sort: "),
        Span::styled(
            state.sort_key().label(),
            Style::default().add_modifier(Modifier::ITALIC),
        ),
        Span::raw("    "),
        daemon,
    ]);
    let block = Block::default().title(title).borders(Borders::ALL);
    frame.render_widget(Paragraph::new(header_line).block(block), area);
}

#[cfg(feature = "tui")]
fn render_table(frame: &mut Frame<'_>, state: &InventoryState, area: Rect) {
    let header = Row::new(["PATH", "STATUS", "SOURCE", "EXPIRES", "PROVIDER", "SCOPE"])
        .style(Style::default().add_modifier(Modifier::BOLD));

    let rows: Vec<Row> = state
        .visible_rows()
        .iter()
        .map(|r| {
            Row::new([
                Cell::from(r.path.clone()),
                Cell::from(Span::styled(
                    r.status.label().to_owned(),
                    Style::default().fg(status_color(r.status)),
                )),
                Cell::from(r.routed_source.clone().unwrap_or_else(|| "—".into())),
                Cell::from(r.expires_at.clone().unwrap_or_else(|| "—".into())),
                Cell::from(r.provider.clone().unwrap_or_else(|| "—".into())),
                Cell::from(r.scope.clone()),
            ])
        })
        .collect();

    let widths = [
        Constraint::Percentage(35),
        Constraint::Percentage(15),
        Constraint::Percentage(15),
        Constraint::Percentage(13),
        Constraint::Percentage(12),
        Constraint::Percentage(10),
    ];
    let block_title = if matches!(state.focus(), Focus::Table) {
        " Table [focus] "
    } else {
        " Table "
    };
    let table = Table::new(rows, widths)
        .header(header)
        .block(Block::default().borders(Borders::ALL).title(block_title))
        .row_highlight_style(Style::default().add_modifier(Modifier::REVERSED));

    let mut table_state = TableState::default();
    if !state.visible_rows().is_empty() {
        table_state.select(Some(state.selected()));
    }
    frame.render_stateful_widget(table, area, &mut table_state);
}

#[cfg(feature = "tui")]
fn render_filters(frame: &mut Frame<'_>, state: &InventoryState, area: Rect) {
    let chips: Vec<Span<'_>> = vec![
        filter_chip(
            "scope",
            state.filters().scope.as_deref(),
            matches!(state.focus(), Focus::ScopeFilter),
        ),
        Span::raw("  "),
        filter_chip(
            "provider",
            state.filters().provider.as_deref(),
            matches!(state.focus(), Focus::ProviderFilter),
        ),
        Span::raw("  "),
        filter_chip(
            "status",
            state.filters().status.map(RowStatus::label),
            matches!(state.focus(), Focus::StatusFilter),
        ),
    ];
    let line = Line::from(chips);
    frame.render_widget(
        Paragraph::new(line).block(Block::default().borders(Borders::ALL).title(" Filters ")),
        area,
    );
}

#[cfg(feature = "tui")]
fn filter_chip<'a, V: Into<Option<&'a str>>>(name: &'a str, value: V, focused: bool) -> Span<'a> {
    let value = value.into();
    let label = match value {
        Some(v) => format!("{name}: {v}"),
        None => format!("{name}: any"),
    };
    let style = if focused {
        Style::default()
            .fg(Color::Black)
            .bg(Color::Yellow)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default()
    };
    Span::styled(label, style)
}

#[cfg(feature = "tui")]
fn render_help(frame: &mut Frame<'_>, area: Rect) {
    let help = Line::from(vec![Span::raw(
        "Tab=cycle focus  ↑/↓=move  s=sort  /=filter  q=quit",
    )]);
    frame.render_widget(Paragraph::new(help), area);
}

#[cfg(feature = "tui")]
fn status_color(s: RowStatus) -> Color {
    match s {
        RowStatus::Provisioned => Color::Green,
        RowStatus::Expiring => Color::Yellow,
        RowStatus::Missing => Color::Red,
        RowStatus::FormatInvalid => Color::Red,
        RowStatus::Unknown => Color::Gray,
    }
}

#[cfg(feature = "tui")]
fn daemon_status_color(s: DaemonStatus) -> Color {
    match s {
        DaemonStatus::Unlocked => Color::Green,
        DaemonStatus::Locked => Color::Yellow,
        DaemonStatus::NotInstalled => Color::Red,
        DaemonStatus::Unknown => Color::Gray,
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn row(
        path: &str,
        status: RowStatus,
        source: Option<&str>,
        expires_at: Option<&str>,
        provider: Option<&str>,
    ) -> InventoryRow {
        let scope = path.split('/').next().unwrap().to_owned();
        InventoryRow {
            path: path.to_owned(),
            status,
            routed_source: source.map(str::to_owned),
            expires_at: expires_at.map(str::to_owned),
            provider: provider.map(str::to_owned),
            scope,
            catalog_override: None,
        }
    }

    fn fixture_state() -> InventoryState {
        InventoryState::new(vec![
            row(
                "team/gitlab/token-deploy",
                RowStatus::Expiring,
                Some("vault-team"),
                Some("2026-05-15"),
                Some("gitlab"),
            ),
            row(
                "personal/github/pat",
                RowStatus::Provisioned,
                Some("keychain"),
                Some("2027-01-01"),
                Some("github"),
            ),
            row(
                "team/jira/api-key",
                RowStatus::Missing,
                None,
                None,
                Some("jira"),
            ),
            row(
                "team/gitlab/token-readonly",
                RowStatus::FormatInvalid,
                Some("vault-team"),
                Some("2026-05-10"),
                Some("gitlab"),
            ),
        ])
    }

    // -- Status / focus ---------------------------------------

    #[test]
    fn row_status_label_pinned_to_adr_023_strings() {
        assert_eq!(RowStatus::Provisioned.label(), "provisioned");
        assert_eq!(RowStatus::Expiring.label(), "expiring");
        assert_eq!(RowStatus::Missing.label(), "missing");
        assert_eq!(RowStatus::FormatInvalid.label(), "format-invalid");
    }

    #[test]
    fn row_status_urgency_orders_missing_before_provisioned() {
        assert!(RowStatus::Missing.urgency() < RowStatus::Provisioned.urgency());
        assert!(RowStatus::FormatInvalid.urgency() < RowStatus::Expiring.urgency());
    }

    #[test]
    fn focus_cycles_table_to_status_filter_and_back() {
        assert_eq!(Focus::Table.next(), Focus::ScopeFilter);
        assert_eq!(Focus::ScopeFilter.next(), Focus::ProviderFilter);
        assert_eq!(Focus::ProviderFilter.next(), Focus::StatusFilter);
        assert_eq!(Focus::StatusFilter.next(), Focus::Table);

        assert_eq!(Focus::Table.prev(), Focus::StatusFilter);
    }

    // -- Filtering ---------------------------------------------

    #[test]
    fn empty_filters_pass_every_row_through() {
        let s = fixture_state();
        assert_eq!(s.visible_rows().len(), s.rows().len());
    }

    #[test]
    fn scope_filter_keeps_only_matching_rows() {
        let mut s = fixture_state();
        s.set_scope_filter(Some("team".into()));
        let visible = s.visible_rows();
        assert_eq!(visible.len(), 3);
        for r in visible {
            assert_eq!(r.scope, "team");
        }
    }

    #[test]
    fn provider_filter_keeps_only_matching_rows() {
        let mut s = fixture_state();
        s.set_provider_filter(Some("gitlab".into()));
        assert_eq!(s.visible_rows().len(), 2);
    }

    #[test]
    fn status_filter_keeps_only_matching_rows() {
        let mut s = fixture_state();
        s.set_status_filter(Some(RowStatus::Missing));
        let v = s.visible_rows();
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].path, "team/jira/api-key");
    }

    #[test]
    fn multiple_filters_compose_with_and_semantics() {
        let mut s = fixture_state();
        s.set_scope_filter(Some("team".into()));
        s.set_provider_filter(Some("gitlab".into()));
        let v = s.visible_rows();
        assert_eq!(v.len(), 2);
    }

    #[test]
    fn clear_filters_restores_full_visibility() {
        let mut s = fixture_state();
        s.set_scope_filter(Some("team".into()));
        s.set_provider_filter(Some("gitlab".into()));
        s.clear_filters();
        assert_eq!(s.visible_rows().len(), s.rows().len());
        assert!(s.filters().is_empty());
    }

    // -- Sorting -----------------------------------------------

    #[test]
    fn sort_by_expires_at_pushes_none_to_bottom_and_orders_dates_ascending() {
        let s = fixture_state();
        // Default sort key = ExpiresAt.
        let v = s.visible_rows();
        // First two rows have expires_at; third is the missing-
        // expires_at row (team/jira/api-key) which sinks to last.
        assert_eq!(v.first().unwrap().expires_at.as_deref(), Some("2026-05-10"));
        assert_eq!(v.last().unwrap().expires_at, None);
    }

    #[test]
    fn sort_by_path_orders_alphabetically() {
        let mut s = fixture_state();
        s.set_sort_key(SortKey::Path);
        let v = s.visible_rows();
        let paths: Vec<&str> = v.iter().map(|r| r.path.as_str()).collect();
        let mut expected = paths.clone();
        expected.sort();
        assert_eq!(paths, expected);
    }

    #[test]
    fn sort_by_status_orders_by_urgency() {
        let mut s = fixture_state();
        s.set_sort_key(SortKey::Status);
        let v = s.visible_rows();
        // Missing < FormatInvalid < Expiring < Provisioned
        assert_eq!(v[0].status, RowStatus::Missing);
        assert_eq!(v[1].status, RowStatus::FormatInvalid);
        assert_eq!(v[2].status, RowStatus::Expiring);
        assert_eq!(v[3].status, RowStatus::Provisioned);
    }

    // -- Selection ---------------------------------------------

    #[test]
    fn move_up_clamps_at_zero() {
        let mut s = fixture_state();
        for _ in 0..5 {
            s.move_up();
        }
        assert_eq!(s.selected(), 0);
    }

    #[test]
    fn move_down_clamps_to_visible_length() {
        let mut s = fixture_state();
        for _ in 0..100 {
            s.move_down();
        }
        let last = s.visible_rows().len() - 1;
        assert_eq!(s.selected(), last);
    }

    #[test]
    fn applying_filter_clamps_selection_within_visible_range() {
        let mut s = fixture_state();
        s.move_to_bottom();
        let prev = s.selected();
        s.set_status_filter(Some(RowStatus::Missing));
        // After filtering only one row matches; selection clamps
        // to 0.
        assert!(s.selected() < s.visible_rows().len());
        assert!(s.selected() <= prev);
    }

    #[test]
    fn replace_rows_clamps_selection_when_table_shrinks() {
        let mut s = fixture_state();
        s.move_to_bottom();
        s.replace_rows(vec![row("team/x/y", RowStatus::Missing, None, None, None)]);
        assert_eq!(s.selected(), 0);
        assert_eq!(s.visible_rows().len(), 1);
    }

    // -- Daemon updates ----------------------------------------

    #[test]
    fn apply_daemon_status_updates_label() {
        let mut s = fixture_state();
        assert_eq!(s.daemon_status(), DaemonStatus::Unknown);
        s.apply_daemon_status(DaemonStatus::Unlocked);
        assert_eq!(s.daemon_status(), DaemonStatus::Unlocked);
        assert_eq!(s.daemon_status().label(), "daemon: unlocked");
    }

    // -- Selected row ------------------------------------------

    #[test]
    fn selected_row_returns_row_at_cursor() {
        let mut s = fixture_state();
        s.move_to_bottom();
        let r = s.selected_row().unwrap();
        // Default sort by ExpiresAt → last row is the one with
        // expires_at = None (team/jira/api-key).
        assert_eq!(r.expires_at, None);
    }

    #[test]
    fn selected_row_is_none_when_filter_empties_table() {
        let mut s = fixture_state();
        s.set_scope_filter(Some("does-not-exist".into()));
        assert!(s.selected_row().is_none());
    }

    // -- Render snapshot --------------------------------------

    #[cfg(feature = "tui")]
    #[test]
    fn render_does_not_panic_and_writes_table_chrome_into_test_backend() {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;

        let backend = TestBackend::new(120, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        let state = fixture_state();
        terminal
            .draw(|f| {
                let area = f.area();
                render(f, &state, area);
            })
            .unwrap();
        // Pull the buffer and confirm the visible chrome is
        // there. We don't assert on absolute layout — that's
        // what the layout tests would do — just that the render
        // function actually populated the buffer with the
        // headers and a row label.
        let buffer = terminal.backend().buffer().clone();
        let dump = buffer
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect::<String>();
        assert!(dump.contains("Inventory"));
        assert!(dump.contains("PATH"));
        assert!(dump.contains("STATUS"));
        // Filter chips show "any" when no filter set.
        assert!(dump.contains("scope: any"));
        assert!(dump.contains("Tab=cycle focus"));
    }

    // -- K9 — free-text query --------------------------------

    #[test]
    fn query_filters_by_case_insensitive_substring_across_path_and_scope() {
        let mut s = fixture_state();
        s.set_query("GITLAB");
        let visible: Vec<&str> = s.visible_rows().iter().map(|r| r.path.as_str()).collect();
        assert!(visible.iter().all(|p| p.contains("gitlab")));
        assert_eq!(
            visible.len(),
            2,
            "expected both gitlab rows, got {visible:?}"
        );
    }

    #[test]
    fn query_matches_provider_field() {
        let mut s = fixture_state();
        s.set_query("github");
        let visible: Vec<&str> = s.visible_rows().iter().map(|r| r.path.as_str()).collect();
        assert_eq!(visible, vec!["personal/github/pat"]);
    }

    #[test]
    fn query_matches_routed_source() {
        let mut s = fixture_state();
        s.set_query("vault-team");
        let visible: Vec<&str> = s.visible_rows().iter().map(|r| r.path.as_str()).collect();
        // Both gitlab rows route through vault-team.
        assert_eq!(visible.len(), 2);
    }

    #[test]
    fn empty_query_is_a_no_op() {
        let mut s = fixture_state();
        let baseline = s.visible_rows().len();
        s.set_query("");
        assert_eq!(s.visible_rows().len(), baseline);
        s.set_query("   "); // whitespace-only
        assert_eq!(s.visible_rows().len(), baseline);
    }

    #[test]
    fn query_combines_with_existing_filters() {
        let mut s = fixture_state();
        s.set_status_filter(Some(RowStatus::Missing));
        // Only the jira row is missing.
        assert_eq!(s.visible_rows().len(), 1);
        s.set_query("gitlab");
        // No gitlab row is missing → empty.
        assert_eq!(s.visible_rows().len(), 0);
        s.set_query("jira");
        // jira row is missing AND matches query.
        assert_eq!(s.visible_rows().len(), 1);
    }

    #[test]
    fn query_clamps_selection_when_match_set_shrinks() {
        let mut s = fixture_state();
        s.move_to_bottom();
        let prev = s.selected();
        s.set_query("github");
        assert!(s.selected() < s.visible_rows().len());
        assert!(s.selected() <= prev);
    }

    #[test]
    fn query_does_not_search_metadata_only_secret_shaped_strings() {
        // row_matches_query MUST NOT search inside fields that
        // could hold secret-shaped data — only the
        // already-public row scaffolding (path / scope /
        // provider / catalog_override / routed_source).
        let r = row(
            "team/x/y",
            RowStatus::Provisioned,
            Some("local-vault"),
            Some("2026-01-01"),
            Some("x"),
        );
        // These match because they live on the row scaffolding.
        assert!(row_matches_query(&r, "team"));
        assert!(row_matches_query(&r, "local-vault"));
        // The expires_at date is NOT in the searched set even
        // though it's technically on the row — this guards
        // against the user accidentally typing a date prefix
        // and revealing how many secrets expire that month.
        assert!(!row_matches_query(&r, "2026"));
    }
}
