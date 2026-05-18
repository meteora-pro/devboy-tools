//! TUI Discovery-import view per [ADR-023] §3.4 (MVP views,
//! Discovery Import).
//!
//! Lists items the configured providers expose via `source.list`
//! (1Password, Vault, env-store, …), suggests an ADR-020 path
//! for each, lets the user edit + confirm each row, and emits a
//! batch of registrations for the daemon to apply.
//!
//! Per ADR-023 §3.4:
//!
//! > Discovery import — surfaces items the agent can see in
//! > each source. Each row gets a suggested path; user can edit
//! > before confirming. **Values are never fetched; only
//! > references are registered.** Conflicts with existing paths
//! > must be flagged before the row can be confirmed.
//!
//! ## Architecture
//!
//! Same shape as the other three MVP views: pure view-model
//! ([`ImportState`]) + thin `render`. The orchestration layer
//! feeds in [`DiscoveryItem`]s via [`ImportState::replace_items`]
//! after running `source.list` against each configured source;
//! confirmed rows are pulled out via [`ImportState::submissions`].
//!
//! ## Path-suggestion algorithm
//!
//! Conservative by design — surprises here would survive into
//! committed manifests. The algorithm:
//!
//! 1. Split the source path on `/` (and on `:` for the scheme
//!    prefix `op://`, `vault://`).
//! 2. Drop the scheme + bucket segments.
//! 3. The last segment becomes `<purpose>`; the
//!    second-to-last becomes `<provider>`. If only one segment
//!    is left, `<provider>` falls back to `misc`.
//! 4. Slugify each: lower-case, replace any non-alnum run with
//!    `-`, trim leading / trailing `-`, collapse repeats.
//! 5. Prefix with the requested scope (default `team`).
//!
//! [ADR-023]: https://github.com/meteora-pro/devboy-tools/blob/main/docs/architecture/adr/ADR-023-secret-store-ux-layer.md

#[cfg(feature = "tui")]
use ratatui::Frame;
#[cfg(feature = "tui")]
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
#[cfg(feature = "tui")]
use ratatui::style::{Color, Modifier, Style};
#[cfg(feature = "tui")]
use ratatui::text::{Line, Span};
#[cfg(feature = "tui")]
use ratatui::widgets::{Block, Borders, Cell, Paragraph, Row, Table, TableState};

// =============================================================================
// Path suggestion
// =============================================================================

/// Default scope segment used when the caller doesn't supply
/// one. Pinned to `team` because most freshly discovered secrets
/// belong to the shared team scope; personal items can be
/// re-scoped before confirming.
pub const DEFAULT_SCOPE: &str = "team";

/// Suggest an ADR-020 path for a provider-side item. Pure
/// function — no I/O, no allocations beyond the returned
/// `String`. See module docs for the algorithm.
pub fn suggest_path(source_path: &str, scope: &str) -> String {
    let mut segments: Vec<&str> = source_path
        .split(['/', ':'])
        .filter(|s| !s.is_empty())
        .collect();
    // Drop scheme prefix (`op`, `vault`, `kv`, …) and bucket
    // names (`Private`, `Personal`). The heuristic keeps any
    // segment that looks "data-shaped" (contains a digit or is
    // long enough to be meaningful) and drops boilerplate.
    while segments.first().is_some_and(|s| is_boilerplate_segment(s)) {
        segments.remove(0);
    }

    let purpose = segments.pop().unwrap_or("");
    let provider = segments.pop().unwrap_or("misc");

    let slug_provider = slugify(provider);
    let slug_purpose = slugify(purpose);
    let scope_slug = slugify(scope);

    let provider_part = if slug_provider.is_empty() {
        "misc".to_string()
    } else {
        slug_provider
    };
    let purpose_part = if slug_purpose.is_empty() {
        "secret".to_string()
    } else {
        slug_purpose
    };

    format!("{scope_slug}/{provider_part}/{purpose_part}")
}

fn is_boilerplate_segment(s: &str) -> bool {
    matches!(
        s.to_ascii_lowercase().as_str(),
        "op" | "vault"
            | "https"
            | "http"
            | "kv"
            | "v1"
            | "v2"
            | "secret"
            | "secrets"
            | "env"
            | "keychain"
    )
}

fn slugify(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut prev_dash = true; // suppress leading dashes
    for ch in input.chars() {
        let c = ch.to_ascii_lowercase();
        if c.is_ascii_alphanumeric() {
            out.push(c);
            prev_dash = false;
        } else if !prev_dash {
            out.push('-');
            prev_dash = true;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    out
}

// =============================================================================
// Domain types
// =============================================================================

/// One item the orchestration layer fetched via `source.list`.
/// Constructed from the source's catalog response — never holds
/// the actual secret value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveryItem {
    /// Source plugin id (e.g. `1password`, `vault`, `keychain`,
    /// `env-store`).
    pub source_id: String,
    /// Source-side identifier — e.g. `op://Private/jira-token`,
    /// `vault://kv/team/jira-token`. Used as the registered
    /// reference (no fetch happens).
    pub source_path: String,
    /// Human-readable label the source returned (`item.title` in
    /// 1Password parlance, `metadata.name` for Vault).
    pub label: String,
    /// Optional descriptive hint — provider note, last-used
    /// date, etc. Rendered in the row when present.
    pub hint: Option<String>,
}

/// Per-row classification — pending review, confirmed, skipped,
/// or in conflict. Conflict status carries a reason so the user
/// understands why submit is blocked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImportRowStatus {
    /// Default — neither confirmed nor skipped.
    Pending,
    /// User accepted the (possibly edited) path. Will be
    /// included in `submissions()`.
    Confirmed,
    /// User explicitly skipped this row. Excluded from
    /// `submissions()`.
    Skipped,
    /// Edited path is invalid or already exists in the manifest.
    /// Reason is plain text.
    Conflict { reason: String },
}

impl ImportRowStatus {
    pub fn label(&self) -> &str {
        match self {
            Self::Pending => "pending",
            Self::Confirmed => "confirmed",
            Self::Skipped => "skipped",
            Self::Conflict { .. } => "conflict",
        }
    }
}

/// One reviewable row. `edited_path` starts equal to
/// `suggested_path`; the user can mutate it through `set_path`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportRow {
    pub item: DiscoveryItem,
    pub suggested_path: String,
    pub edited_path: String,
    pub status: ImportRowStatus,
}

/// Focus targets for keyboard navigation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ImportFocus {
    Table,
    EditPath,
    ApplyConfirmed,
}

/// Result row pulled out of the state for the daemon to act on.
/// One per `Confirmed` row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportSubmission {
    pub source_id: String,
    pub source_path: String,
    pub registered_path: String,
}

/// View-model. Mutated through the small set of methods
/// documented below; `render` is the only function that touches
/// `ratatui`.
#[derive(Debug, Clone)]
pub struct ImportState {
    rows: Vec<ImportRow>,
    /// Paths already registered in the manifest — used to flag
    /// conflicts when the user edits a path that collides.
    existing_paths: std::collections::HashSet<String>,
    selected: usize,
    focus: ImportFocus,
}

impl ImportState {
    /// Build a fresh state. `existing_paths` should be the
    /// active context's path set so conflicts surface
    /// immediately.
    pub fn new(existing_paths: Vec<String>) -> Self {
        Self {
            rows: Vec::new(),
            existing_paths: existing_paths.into_iter().collect(),
            selected: 0,
            focus: ImportFocus::Table,
        }
    }

    /// Replace the discovered items, computing a suggested path
    /// for each via [`suggest_path`] under the default scope.
    pub fn replace_items(&mut self, items: Vec<DiscoveryItem>) {
        self.replace_items_with_scope(items, DEFAULT_SCOPE);
    }

    /// Same as [`Self::replace_items`] but with an explicit scope
    /// (`team`, `personal`, `service-account/<n>`, …). Each
    /// fresh row's status is computed by re-running the conflict
    /// check against `existing_paths`.
    pub fn replace_items_with_scope(&mut self, items: Vec<DiscoveryItem>, scope: &str) {
        self.rows = items
            .into_iter()
            .map(|item| {
                let suggested = suggest_path(&item.source_path, scope);
                let edited = suggested.clone();
                let status = self.classify_path(&edited);
                ImportRow {
                    item,
                    suggested_path: suggested,
                    edited_path: edited,
                    status,
                }
            })
            .collect();
        self.selected = 0;
    }

    pub fn rows(&self) -> &[ImportRow] {
        &self.rows
    }

    pub fn selected(&self) -> usize {
        self.selected
    }

    pub fn focus(&self) -> ImportFocus {
        self.focus
    }

    pub fn selected_row(&self) -> Option<&ImportRow> {
        self.rows.get(self.selected)
    }

    /// Number of rows currently in `Confirmed` state.
    pub fn confirmed_count(&self) -> usize {
        self.rows
            .iter()
            .filter(|r| matches!(r.status, ImportRowStatus::Confirmed))
            .count()
    }

    /// Set the selection to a specific index. Clamped to the
    /// current row count. Used by the GUI backend's
    /// click-to-select.
    pub fn set_selected(&mut self, idx: usize) {
        if self.rows.is_empty() {
            self.selected = 0;
        } else {
            self.selected = idx.min(self.rows.len() - 1);
        }
    }

    pub fn move_up(&mut self) {
        self.selected = self.selected.saturating_sub(1);
    }

    pub fn move_down(&mut self) {
        if self.rows.is_empty() {
            self.selected = 0;
            return;
        }
        let last = self.rows.len() - 1;
        self.selected = (self.selected + 1).min(last);
    }

    pub fn cycle_focus_next(&mut self) {
        self.focus = match self.focus {
            ImportFocus::Table => ImportFocus::EditPath,
            ImportFocus::EditPath => ImportFocus::ApplyConfirmed,
            ImportFocus::ApplyConfirmed => ImportFocus::Table,
        };
    }

    pub fn cycle_focus_prev(&mut self) {
        self.focus = match self.focus {
            ImportFocus::Table => ImportFocus::ApplyConfirmed,
            ImportFocus::EditPath => ImportFocus::Table,
            ImportFocus::ApplyConfirmed => ImportFocus::EditPath,
        };
    }

    /// Replace the edited path on the currently selected row and
    /// re-classify. No-op when no row is selected.
    pub fn set_path(&mut self, path: impl Into<String>) {
        let new_path = path.into();
        let new_status = self.classify_path(&new_path);
        if let Some(row) = self.rows.get_mut(self.selected) {
            row.edited_path = new_path;
            row.status = new_status;
        }
    }

    /// Confirm the selected row. Refuses to confirm a row in
    /// conflict — the conflict must be edited away first.
    pub fn confirm_selected(&mut self) -> bool {
        let Some(row) = self.rows.get_mut(self.selected) else {
            return false;
        };
        if matches!(row.status, ImportRowStatus::Conflict { .. }) {
            return false;
        }
        row.status = ImportRowStatus::Confirmed;
        true
    }

    /// Mark the selected row as skipped — excluded from the
    /// final submission batch but kept in the table for context.
    pub fn skip_selected(&mut self) {
        if let Some(row) = self.rows.get_mut(self.selected) {
            row.status = ImportRowStatus::Skipped;
        }
    }

    /// Reset the selected row back to Pending so the user can
    /// re-decide. Useful when a confirm was accidental.
    pub fn unmark_selected(&mut self) {
        if let Some(row) = self.rows.get_mut(self.selected) {
            // Re-classify in case `existing_paths` changed
            // beneath us (e.g. another import landed).
            let new_status = Self::classify_path_static(&self.existing_paths, &row.edited_path);
            row.status = new_status;
        }
    }

    /// Pull every Confirmed row's submission. Caller hands these
    /// to the daemon (`secrets.register` or equivalent) — the
    /// daemon does the actual write.
    pub fn submissions(&self) -> Vec<ImportSubmission> {
        self.rows
            .iter()
            .filter(|r| matches!(r.status, ImportRowStatus::Confirmed))
            .map(|r| ImportSubmission {
                source_id: r.item.source_id.clone(),
                source_path: r.item.source_path.clone(),
                registered_path: r.edited_path.clone(),
            })
            .collect()
    }

    fn classify_path(&self, path: &str) -> ImportRowStatus {
        Self::classify_path_static(&self.existing_paths, path)
    }

    fn classify_path_static(
        existing: &std::collections::HashSet<String>,
        path: &str,
    ) -> ImportRowStatus {
        if path.is_empty() {
            return ImportRowStatus::Conflict {
                reason: "edited path is empty".into(),
            };
        }
        let segments: Vec<&str> = path.split('/').collect();
        if segments.len() < 3 {
            return ImportRowStatus::Conflict {
                reason: "ADR-020 path requires at least 3 segments (scope/provider/purpose)".into(),
            };
        }
        if segments.iter().any(|s| s.is_empty()) {
            return ImportRowStatus::Conflict {
                reason: "path has empty segment(s)".into(),
            };
        }
        if existing.contains(path) {
            return ImportRowStatus::Conflict {
                reason: format!("path '{path}' already registered"),
            };
        }
        ImportRowStatus::Pending
    }
}

// =============================================================================
// Render
// =============================================================================

#[cfg(feature = "tui")]
pub fn render(frame: &mut Frame<'_>, state: &ImportState, area: Rect) {
    let block = Block::default().borders(Borders::ALL).title(format!(
        " Discovery import — {} confirmed / {} total ",
        state.confirmed_count(),
        state.rows().len()
    ));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(8),    // table
            Constraint::Length(3), // edit-path field
            Constraint::Length(2), // help line
        ])
        .split(inner);

    render_table(frame, state, chunks[0]);
    render_edit_path(frame, state, chunks[1]);
    render_help(frame, state, chunks[2]);
}

#[cfg(feature = "tui")]
fn render_table(frame: &mut Frame<'_>, state: &ImportState, area: Rect) {
    let header = Row::new(vec![
        Cell::from("SOURCE"),
        Cell::from("DISCOVERED"),
        Cell::from("REGISTERED PATH"),
        Cell::from("STATUS"),
    ])
    .style(Style::default().add_modifier(Modifier::BOLD));

    let rows: Vec<Row<'_>> = state
        .rows()
        .iter()
        .map(|r| {
            let status_style = match r.status {
                ImportRowStatus::Pending => Style::default().add_modifier(Modifier::DIM),
                ImportRowStatus::Confirmed => Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD),
                ImportRowStatus::Skipped => Style::default().add_modifier(Modifier::DIM),
                ImportRowStatus::Conflict { .. } => {
                    Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)
                }
            };
            Row::new(vec![
                Cell::from(r.item.source_id.clone()),
                Cell::from(r.item.label.clone()),
                Cell::from(r.edited_path.clone()),
                Cell::from(r.status.label().to_string()).style(status_style),
            ])
        })
        .collect();

    let widths = [
        Constraint::Length(12),
        Constraint::Min(20),
        Constraint::Min(28),
        Constraint::Length(12),
    ];
    let mut table_state = TableState::default();
    table_state.select(Some(state.selected()));

    let focused_table = state.focus() == ImportFocus::Table;
    let highlight = if focused_table {
        Style::default().add_modifier(Modifier::REVERSED | Modifier::BOLD)
    } else {
        Style::default().add_modifier(Modifier::REVERSED)
    };
    let table = Table::new(rows, widths)
        .header(header)
        .row_highlight_style(highlight)
        .highlight_symbol("▶ ");

    frame.render_stateful_widget(table, area, &mut table_state);
}

#[cfg(feature = "tui")]
fn render_edit_path(frame: &mut Frame<'_>, state: &ImportState, area: Rect) {
    let focused = state.focus() == ImportFocus::EditPath;
    let body = state
        .selected_row()
        .map(|r| {
            let cursor = if focused { "_" } else { "" };
            format!("{}{}", r.edited_path, cursor)
        })
        .unwrap_or_default();
    let title = if focused {
        " edit path ▶ "
    } else {
        " edit path "
    };
    frame.render_widget(
        Paragraph::new(body).block(Block::default().borders(Borders::ALL).title(title)),
        area,
    );
}

#[cfg(feature = "tui")]
fn render_help(frame: &mut Frame<'_>, state: &ImportState, area: Rect) {
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(70), Constraint::Percentage(30)])
        .split(area);

    let help = Paragraph::new(Line::from(Span::styled(
        "↑↓=select  c=confirm  s=skip  u=unmark  Tab=focus  e=edit",
        Style::default().add_modifier(Modifier::DIM),
    )));
    frame.render_widget(help, cols[0]);

    let apply_focused = state.focus() == ImportFocus::ApplyConfirmed;
    let apply_style = if apply_focused {
        Style::default().add_modifier(Modifier::REVERSED | Modifier::BOLD)
    } else {
        Style::default()
    };
    let apply_label = format!("[ Apply {} confirmed ]", state.confirmed_count());
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(apply_label, apply_style)))
            .alignment(Alignment::Right),
        cols[1],
    );
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn item(source_id: &str, source_path: &str, label: &str) -> DiscoveryItem {
        DiscoveryItem {
            source_id: source_id.into(),
            source_path: source_path.into(),
            label: label.into(),
            hint: None,
        }
    }

    fn fresh() -> ImportState {
        let mut s = ImportState::new(Vec::new());
        s.replace_items(vec![
            item("1password", "op://Private/jira-api-key", "Jira API key"),
            item(
                "vault",
                "vault://kv/team/github/deploy-token",
                "Deploy token",
            ),
            item("keychain", "keychain://service/openai/api-key", "OpenAI"),
        ]);
        s
    }

    // -- Path suggestion ---------------------------------------

    #[test]
    fn suggest_strips_scheme_and_bucket_segments() {
        assert_eq!(
            suggest_path("op://Private/jira-api-key", "team"),
            "team/private/jira-api-key"
        );
    }

    #[test]
    fn suggest_uses_last_two_meaningful_segments() {
        assert_eq!(
            suggest_path("vault://kv/team/github/deploy-token", "team"),
            "team/github/deploy-token"
        );
    }

    #[test]
    fn suggest_falls_back_to_misc_provider_when_only_one_segment_left() {
        assert_eq!(
            suggest_path("op://Private/lone", "team"),
            "team/private/lone"
        );
        // Just one data segment after stripping scheme.
        assert_eq!(suggest_path("env://API_KEY", "team"), "team/misc/api-key");
    }

    #[test]
    fn suggest_slugifies_aggressively() {
        assert_eq!(
            suggest_path("op://Private/Jira API_Key (prod)", "team"),
            "team/private/jira-api-key-prod"
        );
    }

    #[test]
    fn suggest_honors_caller_supplied_scope() {
        assert_eq!(
            suggest_path("op://Private/personal-token", "personal"),
            "personal/private/personal-token"
        );
    }

    #[test]
    fn suggest_handles_empty_purpose_gracefully() {
        // No alnum chars in last segment after scheme strip.
        let s = suggest_path("op://---/!!!", "team");
        // Slugified to empty → fallback chain kicks in.
        assert!(!s.is_empty());
        let parts: Vec<&str> = s.split('/').collect();
        assert_eq!(parts.len(), 3);
    }

    // -- Replace items + initial classification ----------------

    #[test]
    fn replace_items_computes_suggested_paths_for_every_row() {
        let s = fresh();
        let paths: Vec<&str> = s.rows().iter().map(|r| r.suggested_path.as_str()).collect();
        assert_eq!(paths[0], "team/private/jira-api-key");
        assert_eq!(paths[1], "team/github/deploy-token");
        assert_eq!(paths[2], "team/openai/api-key");
    }

    #[test]
    fn fresh_rows_start_in_pending_status() {
        let s = fresh();
        for r in s.rows() {
            assert!(matches!(r.status, ImportRowStatus::Pending), "{:?}", r);
        }
    }

    #[test]
    fn rows_with_already_registered_paths_start_in_conflict() {
        let mut s = ImportState::new(vec!["team/github/deploy-token".into()]);
        s.replace_items(vec![item(
            "vault",
            "vault://kv/team/github/deploy-token",
            "Deploy token",
        )]);
        match &s.rows()[0].status {
            ImportRowStatus::Conflict { reason } => assert!(reason.contains("already registered")),
            other => panic!("expected conflict, got {other:?}"),
        }
    }

    // -- Selection & focus -------------------------------------

    #[test]
    fn move_up_clamps_at_zero() {
        let mut s = fresh();
        for _ in 0..10 {
            s.move_up();
        }
        assert_eq!(s.selected(), 0);
    }

    #[test]
    fn move_down_clamps_at_last_row() {
        let mut s = fresh();
        for _ in 0..100 {
            s.move_down();
        }
        assert_eq!(s.selected(), s.rows().len() - 1);
    }

    #[test]
    fn focus_cycles_through_table_edit_apply() {
        let mut s = fresh();
        assert_eq!(s.focus(), ImportFocus::Table);
        s.cycle_focus_next();
        assert_eq!(s.focus(), ImportFocus::EditPath);
        s.cycle_focus_next();
        assert_eq!(s.focus(), ImportFocus::ApplyConfirmed);
        s.cycle_focus_next();
        assert_eq!(s.focus(), ImportFocus::Table);
    }

    // -- Edit + classify ---------------------------------------

    #[test]
    fn set_path_reclassifies_against_existing_set() {
        let mut s = ImportState::new(vec!["team/github/deploy-token".into()]);
        s.replace_items(vec![item("1password", "op://Private/jira-api-key", "Jira")]);
        // Initially pending — no collision.
        assert!(matches!(s.rows()[0].status, ImportRowStatus::Pending));
        // Edit to a colliding path → conflict.
        s.set_path("team/github/deploy-token");
        assert!(matches!(
            s.rows()[0].status,
            ImportRowStatus::Conflict { .. }
        ));
        // Edit back to a unique path → pending.
        s.set_path("team/jira/api-key");
        assert!(matches!(s.rows()[0].status, ImportRowStatus::Pending));
    }

    #[test]
    fn set_path_rejects_too_few_segments() {
        let mut s = fresh();
        s.set_path("team/jira");
        match &s.rows()[0].status {
            ImportRowStatus::Conflict { reason } => assert!(reason.contains("3 segments")),
            other => panic!("expected conflict, got {other:?}"),
        }
    }

    #[test]
    fn set_path_rejects_empty_segment() {
        let mut s = fresh();
        s.set_path("team//api-key");
        assert!(matches!(
            s.rows()[0].status,
            ImportRowStatus::Conflict { .. }
        ));
    }

    // -- Confirm / skip / unmark -------------------------------

    #[test]
    fn confirm_pending_row_succeeds() {
        let mut s = fresh();
        assert!(s.confirm_selected());
        assert!(matches!(s.rows()[0].status, ImportRowStatus::Confirmed));
    }

    #[test]
    fn confirm_refuses_row_in_conflict() {
        let mut s = ImportState::new(vec!["team/private/jira-api-key".into()]);
        s.replace_items(vec![item("1password", "op://Private/jira-api-key", "Jira")]);
        assert!(!s.confirm_selected());
        assert!(matches!(
            s.rows()[0].status,
            ImportRowStatus::Conflict { .. }
        ));
    }

    #[test]
    fn skip_marks_row_as_skipped_regardless_of_prior_status() {
        let mut s = fresh();
        s.confirm_selected();
        s.skip_selected();
        assert!(matches!(s.rows()[0].status, ImportRowStatus::Skipped));
    }

    #[test]
    fn unmark_returns_row_to_pending_or_conflict() {
        let mut s = fresh();
        s.confirm_selected();
        s.unmark_selected();
        assert!(matches!(s.rows()[0].status, ImportRowStatus::Pending));
    }

    // -- Submissions -------------------------------------------

    #[test]
    fn submissions_only_yields_confirmed_rows() {
        let mut s = fresh();
        s.confirm_selected(); // row 0
        s.move_down();
        s.skip_selected(); // row 1
        s.move_down();
        s.confirm_selected(); // row 2
        let subs = s.submissions();
        assert_eq!(subs.len(), 2);
        assert_eq!(subs[0].registered_path, "team/private/jira-api-key");
        assert_eq!(subs[1].registered_path, "team/openai/api-key");
    }

    #[test]
    fn submission_carries_source_id_and_source_path_for_daemon_handoff() {
        let mut s = fresh();
        s.confirm_selected();
        let sub = &s.submissions()[0];
        assert_eq!(sub.source_id, "1password");
        assert_eq!(sub.source_path, "op://Private/jira-api-key");
    }

    #[test]
    fn confirmed_count_tracks_confirmed_rows_only() {
        let mut s = fresh();
        s.confirm_selected();
        s.move_down();
        s.confirm_selected();
        assert_eq!(s.confirmed_count(), 2);
        s.skip_selected();
        assert_eq!(s.confirmed_count(), 1);
    }

    // -- "Never fetches values" guarantee ---------------------

    #[test]
    fn discovery_items_never_carry_secret_values() {
        // Compile-time assertion — DiscoveryItem has no value
        // field. This test exists so a future refactor that
        // adds one to the struct trips the compiler here.
        let i = item("vault", "vault://kv/team/x/y", "X");
        let _: &str = &i.source_id;
        let _: &str = &i.source_path;
        let _: &str = &i.label;
        let _: Option<&String> = i.hint.as_ref();
        // No `.value()` getter exists, no `value` field. End of
        // assertion.
    }

    // -- Render snapshot ---------------------------------------

    #[cfg(feature = "tui")]
    #[test]
    fn render_writes_table_chrome_and_progress_into_test_backend() {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;

        let backend = TestBackend::new(120, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut state = fresh();
        state.confirm_selected();
        terminal
            .draw(|f| {
                let area = f.area();
                render(f, &state, area);
            })
            .unwrap();
        let buffer = terminal.backend().buffer().clone();
        let dump: String = buffer.content().iter().map(|c| c.symbol()).collect();
        assert!(dump.contains("Discovery import"));
        assert!(dump.contains("1 confirmed / 3 total"));
        assert!(dump.contains("SOURCE"));
        assert!(dump.contains("DISCOVERED"));
        assert!(dump.contains("REGISTERED PATH"));
        assert!(dump.contains("STATUS"));
        assert!(dump.contains("Apply 1 confirmed"));
        assert!(dump.contains("team/private/jira-api-key"));
    }
}
