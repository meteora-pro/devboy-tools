//! TUI Edit-metadata view per [ADR-023] §3.4 (MVP views, Edit
//! Metadata).
//!
//! An editable form for the five user-tunable fields on an
//! ADR-020 entry — `description`, `retrieval_url`,
//! `rotate_every_days`, `expires_at`, `pattern_id` — plus an
//! "apply suggestion" subview that diffs an incoming
//! `propose_metadata` payload (typically from the agent via
//! `propose_metadata` per ADR-021) against the current values.
//!
//! Per ADR-023 §3.4:
//!
//! > Editable form. Apply-suggestion subview shows a side-by-side
//! > diff of `current` vs `proposed`. `[Save]` writes through
//! > `metadata.update`; the agent never touches the value, only
//! > metadata.
//!
//! ## Architecture
//!
//! Same shape as the inventory and the provision dialog: pure
//! view-model + thin render. Fields are held as `String` in the
//! draft so typing is trivial; type validation runs at
//! `save()` time and produces a useful per-field reason on
//! failure.
//!
//! [ADR-023]: https://github.com/meteora-pro/devboy-tools/blob/main/docs/architecture/adr/ADR-023-secret-store-ux-layer.md

use chrono::NaiveDate;

#[cfg(feature = "tui")]
use ratatui::Frame;
#[cfg(feature = "tui")]
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
#[cfg(feature = "tui")]
use ratatui::style::{Color, Modifier, Style};
#[cfg(feature = "tui")]
use ratatui::text::{Line, Span};
#[cfg(feature = "tui")]
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

// =============================================================================
// Domain types
// =============================================================================

/// The five fields ADR-020 lets a user edit on an existing
/// secret entry. Pinned to lower-case-snake names so a future
/// `--field=` flag doesn't have to translate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MetadataField {
    Description,
    RetrievalUrl,
    RotateEveryDays,
    ExpiresAt,
    PatternId,
}

impl MetadataField {
    /// Stable label rendered next to the input. Used as filter
    /// key in the diff view.
    pub fn label(self) -> &'static str {
        match self {
            Self::Description => "description",
            Self::RetrievalUrl => "retrieval_url",
            Self::RotateEveryDays => "rotate_every_days",
            Self::ExpiresAt => "expires_at",
            Self::PatternId => "pattern_id",
        }
    }
}

/// All editable fields as plain strings — typing is unbounded
/// and validation runs at `save()`. `None` = field not set;
/// empty `Some("")` is treated the same as `None` for diff /
/// validation purposes (so the user can clear a field without
/// special key combos).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MetadataDraft {
    pub description: Option<String>,
    pub retrieval_url: Option<String>,
    /// Days between rotations. Stored as text; parsed to `u32`
    /// at save time.
    pub rotate_every_days: Option<String>,
    /// ISO-8601 (`yyyy-mm-dd`). Parsed to `NaiveDate` at save
    /// time.
    pub expires_at: Option<String>,
    pub pattern_id: Option<String>,
}

impl MetadataDraft {
    fn get(&self, field: MetadataField) -> &str {
        match field {
            MetadataField::Description => self.description.as_deref().unwrap_or(""),
            MetadataField::RetrievalUrl => self.retrieval_url.as_deref().unwrap_or(""),
            MetadataField::RotateEveryDays => self.rotate_every_days.as_deref().unwrap_or(""),
            MetadataField::ExpiresAt => self.expires_at.as_deref().unwrap_or(""),
            MetadataField::PatternId => self.pattern_id.as_deref().unwrap_or(""),
        }
    }

    fn set(&mut self, field: MetadataField, value: String) {
        let target = match field {
            MetadataField::Description => &mut self.description,
            MetadataField::RetrievalUrl => &mut self.retrieval_url,
            MetadataField::RotateEveryDays => &mut self.rotate_every_days,
            MetadataField::ExpiresAt => &mut self.expires_at,
            MetadataField::PatternId => &mut self.pattern_id,
        };
        // Empty string normalizes back to None so equality with a
        // pristine baseline behaves intuitively.
        *target = if value.is_empty() { None } else { Some(value) };
    }
}

/// Editor mode. `Edit` is the default; `ApplySuggestion` is
/// entered by handing in a [`MetadataDraft`] suggestion via
/// [`EditorState::apply_suggestion`] — typically from a
/// `propose_metadata` agent call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditorMode {
    Edit,
    ApplySuggestion,
}

/// Lifecycle status of the editor. Updated by orchestration via
/// [`EditorState::apply_status`] as the daemon `metadata.update`
/// RPC progresses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EditorStatus {
    Idle,
    Saving,
    Saved,
    ValidationFailed {
        field: MetadataField,
        reason: String,
    },
    Cancelled,
}

impl EditorStatus {
    pub fn label(&self) -> &str {
        match self {
            Self::Idle => "ready",
            Self::Saving => "saving…",
            Self::Saved => "saved",
            Self::ValidationFailed { .. } => "validation failed",
            Self::Cancelled => "cancelled",
        }
    }
}

/// Focus targets. Suggestion-related buttons are only visible /
/// reachable while `mode = ApplySuggestion`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EditorFocus {
    Field(MetadataField),
    Save,
    Cancel,
    AcceptSuggestion,
    RejectSuggestion,
}

/// Result of a successful `save()`. The orchestration layer
/// turns this into a `metadata.update` RPC.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditorSubmission {
    pub path: String,
    pub description: Option<String>,
    pub retrieval_url: Option<String>,
    pub rotate_every_days: Option<u32>,
    pub expires_at: Option<NaiveDate>,
    pub pattern_id: Option<String>,
}

/// Per-field diff entry rendered in the ApplySuggestion subview.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldDiff {
    pub field: MetadataField,
    pub current: String,
    pub proposed: String,
    pub changed: bool,
}

/// View-model for the metadata editor. Mutated through the
/// methods documented below; `render` is the only function that
/// touches `ratatui`.
#[derive(Debug, Clone)]
pub struct EditorState {
    path: String,
    original: MetadataDraft,
    draft: MetadataDraft,
    suggestion: Option<MetadataDraft>,
    mode: EditorMode,
    focus: EditorFocus,
    status: EditorStatus,
}

const FIELDS: [MetadataField; 5] = [
    MetadataField::Description,
    MetadataField::RetrievalUrl,
    MetadataField::RotateEveryDays,
    MetadataField::ExpiresAt,
    MetadataField::PatternId,
];

impl EditorState {
    /// Build a fresh editor seeded with the current values.
    /// Initial focus = first field (description) so Tab-cycle
    /// progresses through the form in render order.
    pub fn new(path: impl Into<String>, original: MetadataDraft) -> Self {
        let path = path.into();
        let draft = original.clone();
        Self {
            path,
            original,
            draft,
            suggestion: None,
            mode: EditorMode::Edit,
            focus: EditorFocus::Field(MetadataField::Description),
            status: EditorStatus::Idle,
        }
    }

    pub fn path(&self) -> &str {
        &self.path
    }

    pub fn mode(&self) -> EditorMode {
        self.mode
    }

    pub fn focus(&self) -> EditorFocus {
        self.focus
    }

    pub fn status(&self) -> &EditorStatus {
        &self.status
    }

    pub fn draft(&self) -> &MetadataDraft {
        &self.draft
    }

    pub fn original(&self) -> &MetadataDraft {
        &self.original
    }

    pub fn suggestion(&self) -> Option<&MetadataDraft> {
        self.suggestion.as_ref()
    }

    /// Whether the draft differs from the baseline. Used to gate
    /// `save()` — saving an unchanged form is a no-op.
    pub fn is_dirty(&self) -> bool {
        self.draft != self.original
    }

    /// Hand in an agent suggestion. Switches to ApplySuggestion
    /// mode and parks focus on the Accept button.
    pub fn apply_suggestion(&mut self, suggestion: MetadataDraft) {
        self.suggestion = Some(suggestion);
        self.mode = EditorMode::ApplySuggestion;
        self.focus = EditorFocus::AcceptSuggestion;
    }

    /// Accept all proposed values. Suggestion fields that are
    /// `None` leave the existing draft entry untouched (per
    /// ADR-023: "a `propose_metadata` payload is additive — fields
    /// it omits are not cleared").
    pub fn accept_suggestion(&mut self) {
        let Some(s) = self.suggestion.take() else {
            return;
        };
        if s.description.is_some() {
            self.draft.description = s.description;
        }
        if s.retrieval_url.is_some() {
            self.draft.retrieval_url = s.retrieval_url;
        }
        if s.rotate_every_days.is_some() {
            self.draft.rotate_every_days = s.rotate_every_days;
        }
        if s.expires_at.is_some() {
            self.draft.expires_at = s.expires_at;
        }
        if s.pattern_id.is_some() {
            self.draft.pattern_id = s.pattern_id;
        }
        self.mode = EditorMode::Edit;
        self.focus = EditorFocus::Save;
    }

    /// Discard the suggestion and return to plain Edit mode.
    pub fn reject_suggestion(&mut self) {
        self.suggestion = None;
        self.mode = EditorMode::Edit;
        self.focus = EditorFocus::Field(MetadataField::Description);
    }

    /// Diff the current draft against the suggestion. Returns
    /// an empty vector when no suggestion is present.
    pub fn diff(&self) -> Vec<FieldDiff> {
        let Some(suggestion) = self.suggestion.as_ref() else {
            return Vec::new();
        };
        FIELDS
            .iter()
            .map(|f| {
                let current = self.draft.get(*f).to_string();
                let proposed = suggestion.get(*f).to_string();
                let changed = !proposed.is_empty() && proposed != current;
                FieldDiff {
                    field: *f,
                    current,
                    proposed,
                    changed,
                }
            })
            .collect()
    }

    /// Snapshot the current draft value for a field as a fresh
    /// `String`. Used by the GUI text-input binding which needs
    /// an owned `&mut String` to bind into.
    pub fn draft_field_clone(&self, field: MetadataField) -> String {
        self.draft.get(field).to_string()
    }

    /// Replace a field's value wholesale. Useful from a paste
    /// handler or a programmatic test fixture; key handlers go
    /// through `type_char` / `backspace` instead.
    pub fn set_field(&mut self, field: MetadataField, value: impl Into<String>) {
        self.draft.set(field, value.into());
    }

    /// Append a character to whatever field is focused. No-op
    /// if focus is on a button.
    pub fn type_char(&mut self, c: char) {
        let EditorFocus::Field(f) = self.focus else {
            return;
        };
        let mut s = self.draft.get(f).to_string();
        s.push(c);
        self.draft.set(f, s);
    }

    /// Drop the last character from the focused field.
    pub fn backspace(&mut self) {
        let EditorFocus::Field(f) = self.focus else {
            return;
        };
        let mut s = self.draft.get(f).to_string();
        s.pop();
        self.draft.set(f, s);
    }

    pub fn cycle_focus_next(&mut self) {
        self.focus = next_focus(self.focus, self.mode);
    }

    pub fn cycle_focus_prev(&mut self) {
        self.focus = prev_focus(self.focus, self.mode);
    }

    pub fn apply_status(&mut self, status: EditorStatus) {
        self.status = status;
    }

    /// Save guard. Validates each non-empty field. On success
    /// returns an [`EditorSubmission`] with parsed types and
    /// flips status to `Saving`. On failure stamps the status
    /// with the offending field + reason and returns `None`.
    pub fn save(&mut self) -> Option<EditorSubmission> {
        if !self.is_dirty() {
            self.status = EditorStatus::ValidationFailed {
                field: MetadataField::Description,
                reason: "no changes to save".into(),
            };
            return None;
        }

        let rotate = match self.draft.rotate_every_days.as_deref() {
            Some(raw) => match raw.parse::<u32>() {
                Ok(n) if (1..=3650).contains(&n) => Some(n),
                _ => {
                    self.status = EditorStatus::ValidationFailed {
                        field: MetadataField::RotateEveryDays,
                        reason: "must be an integer between 1 and 3650".into(),
                    };
                    return None;
                }
            },
            None => None,
        };

        let expires = match self.draft.expires_at.as_deref() {
            Some(raw) => match NaiveDate::parse_from_str(raw, "%Y-%m-%d") {
                Ok(d) => Some(d),
                Err(_) => {
                    self.status = EditorStatus::ValidationFailed {
                        field: MetadataField::ExpiresAt,
                        reason: "must be ISO-8601 (yyyy-mm-dd)".into(),
                    };
                    return None;
                }
            },
            None => None,
        };

        // pattern_id is free-form here; the daemon validates it
        // against the catalogue server-side. URL is also
        // free-form (a strict URL parse would reject `gopher://`
        // and the like, which is overkill for a hint field).

        self.status = EditorStatus::Saving;
        Some(EditorSubmission {
            path: self.path.clone(),
            description: self.draft.description.clone(),
            retrieval_url: self.draft.retrieval_url.clone(),
            rotate_every_days: rotate,
            expires_at: expires,
            pattern_id: self.draft.pattern_id.clone(),
        })
    }

    /// User dismissed the editor. Resets the draft back to the
    /// baseline so a re-open has a clean slate.
    pub fn cancel(&mut self) {
        self.draft = self.original.clone();
        self.suggestion = None;
        self.mode = EditorMode::Edit;
        self.status = EditorStatus::Cancelled;
    }
}

fn next_focus(focus: EditorFocus, mode: EditorMode) -> EditorFocus {
    match (focus, mode) {
        (EditorFocus::Field(MetadataField::Description), _) => {
            EditorFocus::Field(MetadataField::RetrievalUrl)
        }
        (EditorFocus::Field(MetadataField::RetrievalUrl), _) => {
            EditorFocus::Field(MetadataField::RotateEveryDays)
        }
        (EditorFocus::Field(MetadataField::RotateEveryDays), _) => {
            EditorFocus::Field(MetadataField::ExpiresAt)
        }
        (EditorFocus::Field(MetadataField::ExpiresAt), _) => {
            EditorFocus::Field(MetadataField::PatternId)
        }
        (EditorFocus::Field(MetadataField::PatternId), EditorMode::ApplySuggestion) => {
            EditorFocus::AcceptSuggestion
        }
        (EditorFocus::Field(MetadataField::PatternId), EditorMode::Edit) => EditorFocus::Save,
        (EditorFocus::AcceptSuggestion, _) => EditorFocus::RejectSuggestion,
        (EditorFocus::RejectSuggestion, _) => EditorFocus::Save,
        (EditorFocus::Save, _) => EditorFocus::Cancel,
        (EditorFocus::Cancel, _) => EditorFocus::Field(MetadataField::Description),
    }
}

fn prev_focus(focus: EditorFocus, mode: EditorMode) -> EditorFocus {
    match (focus, mode) {
        (EditorFocus::Field(MetadataField::Description), _) => EditorFocus::Cancel,
        (EditorFocus::Field(MetadataField::RetrievalUrl), _) => {
            EditorFocus::Field(MetadataField::Description)
        }
        (EditorFocus::Field(MetadataField::RotateEveryDays), _) => {
            EditorFocus::Field(MetadataField::RetrievalUrl)
        }
        (EditorFocus::Field(MetadataField::ExpiresAt), _) => {
            EditorFocus::Field(MetadataField::RotateEveryDays)
        }
        (EditorFocus::Field(MetadataField::PatternId), _) => {
            EditorFocus::Field(MetadataField::ExpiresAt)
        }
        (EditorFocus::AcceptSuggestion, _) => EditorFocus::Field(MetadataField::PatternId),
        (EditorFocus::RejectSuggestion, _) => EditorFocus::AcceptSuggestion,
        (EditorFocus::Save, EditorMode::ApplySuggestion) => EditorFocus::RejectSuggestion,
        (EditorFocus::Save, EditorMode::Edit) => EditorFocus::Field(MetadataField::PatternId),
        (EditorFocus::Cancel, _) => EditorFocus::Save,
    }
}

// =============================================================================
// Render
// =============================================================================

/// Render the metadata editor into `area`. In `Edit` mode the
/// layout is a stacked form; in `ApplySuggestion` mode the
/// upper half hosts the form and the lower half hosts a
/// three-column diff (FIELD | CURRENT | PROPOSED).
#[cfg(feature = "tui")]
pub fn render(frame: &mut Frame<'_>, state: &EditorState, area: Rect) {
    let modal = centered_rect(80, 80, area);
    frame.render_widget(Clear, modal);

    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" Edit metadata — {} ", state.path()))
        .title_alignment(Alignment::Left);
    let inner = block.inner(modal);
    frame.render_widget(block, modal);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(15), // form
            Constraint::Length(if state.mode() == EditorMode::ApplySuggestion {
                8
            } else {
                0
            }), // diff
            Constraint::Length(2),  // buttons
            Constraint::Min(1),     // status
        ])
        .split(inner);

    render_form(frame, state, chunks[0]);
    if state.mode() == EditorMode::ApplySuggestion {
        render_diff(frame, state, chunks[1]);
    }
    render_buttons(frame, state, chunks[2]);
    render_status(frame, state, chunks[3]);
}

#[cfg(feature = "tui")]
fn render_form(frame: &mut Frame<'_>, state: &EditorState, area: Rect) {
    let lines: Vec<Line<'_>> = FIELDS
        .iter()
        .map(|f| {
            let focused = state.focus() == EditorFocus::Field(*f);
            let arrow = if focused { "▶ " } else { "  " };
            let label = format!("{:<18}", f.label());
            let value = state.draft().get(*f);
            let cursor = if focused { "_" } else { "" };
            let dirty = if state.draft().get(*f) != state.original().get(*f) {
                "  *"
            } else {
                ""
            };
            let style = if focused {
                Style::default().add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };
            Line::from(vec![
                Span::raw(arrow),
                Span::styled(
                    label,
                    Style::default().fg(Color::Cyan).add_modifier(Modifier::DIM),
                ),
                Span::styled(format!("{value}{cursor}"), style),
                Span::styled(
                    dirty,
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                ),
            ])
        })
        .collect();

    let body = Paragraph::new(lines).block(
        Block::default()
            .borders(Borders::ALL)
            .title(" Fields (Tab=next, Shift-Tab=prev) "),
    );
    frame.render_widget(body, area);
}

#[cfg(feature = "tui")]
fn render_diff(frame: &mut Frame<'_>, state: &EditorState, area: Rect) {
    let diff = state.diff();
    let mut lines = vec![Line::from(vec![
        Span::styled(
            format!("{:<18}", "FIELD"),
            Style::default().add_modifier(Modifier::DIM),
        ),
        Span::styled(
            format!("{:<28}", "CURRENT"),
            Style::default().add_modifier(Modifier::DIM),
        ),
        Span::styled("PROPOSED", Style::default().add_modifier(Modifier::DIM)),
    ])];
    for d in diff {
        let style = if d.changed {
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().add_modifier(Modifier::DIM)
        };
        let current_disp = if d.current.is_empty() {
            "—"
        } else {
            &d.current
        };
        let proposed_disp = if d.proposed.is_empty() {
            "—"
        } else {
            &d.proposed
        };
        lines.push(Line::from(vec![
            Span::styled(format!("{:<18}", d.field.label()), style),
            Span::styled(format!("{current_disp:<28}"), style),
            Span::styled(proposed_disp.to_string(), style),
        ]));
    }
    let body = Paragraph::new(lines).block(
        Block::default()
            .borders(Borders::ALL)
            .title(" Suggestion diff "),
    );
    frame.render_widget(body, area);
}

#[cfg(feature = "tui")]
fn render_buttons(frame: &mut Frame<'_>, state: &EditorState, area: Rect) {
    if state.mode() == EditorMode::ApplySuggestion {
        let cols = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Percentage(25),
                Constraint::Percentage(25),
                Constraint::Percentage(25),
                Constraint::Percentage(25),
            ])
            .split(area);
        button(
            frame,
            "[ Accept suggestion ]",
            state.focus() == EditorFocus::AcceptSuggestion,
            cols[0],
        );
        button(
            frame,
            "[ Reject suggestion ]",
            state.focus() == EditorFocus::RejectSuggestion,
            cols[1],
        );
        button(
            frame,
            "[ Save ]",
            state.focus() == EditorFocus::Save,
            cols[2],
        );
        button(
            frame,
            "[ Cancel ]",
            state.focus() == EditorFocus::Cancel,
            cols[3],
        );
    } else {
        let cols = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
            .split(area);
        button(
            frame,
            "[ Save ]",
            state.focus() == EditorFocus::Save,
            cols[0],
        );
        button(
            frame,
            "[ Cancel ]",
            state.focus() == EditorFocus::Cancel,
            cols[1],
        );
    }
}

#[cfg(feature = "tui")]
fn button(frame: &mut Frame<'_>, label: &str, focused: bool, area: Rect) {
    let style = if focused {
        Style::default().add_modifier(Modifier::REVERSED | Modifier::BOLD)
    } else {
        Style::default()
    };
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(label, style))).alignment(Alignment::Center),
        area,
    );
}

#[cfg(feature = "tui")]
fn render_status(frame: &mut Frame<'_>, state: &EditorState, area: Rect) {
    let line = match state.status() {
        EditorStatus::Idle => Line::from(Span::styled(
            "Tab=cycle  Enter=activate  Esc=cancel",
            Style::default().add_modifier(Modifier::DIM),
        )),
        EditorStatus::Saving => Line::from("saving…"),
        EditorStatus::Saved => Line::from(Span::styled(
            "saved",
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        )),
        EditorStatus::ValidationFailed { field, reason } => Line::from(Span::styled(
            format!("validation failed [{}]: {reason}", field.label()),
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        )),
        EditorStatus::Cancelled => Line::from(Span::styled(
            "cancelled",
            Style::default().add_modifier(Modifier::DIM),
        )),
    };
    frame.render_widget(Paragraph::new(line), area);
}

#[cfg(feature = "tui")]
fn centered_rect(percent_x: u16, percent_y: u16, r: Rect) -> Rect {
    let popup_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(r);

    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(popup_layout[1])[1]
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn baseline() -> MetadataDraft {
        MetadataDraft {
            description: Some("Jira API key".into()),
            retrieval_url: Some("https://example.invalid/jira/tokens".into()),
            rotate_every_days: Some("90".into()),
            expires_at: Some("2026-12-01".into()),
            pattern_id: Some("jira-api-token".into()),
        }
    }

    fn fresh() -> EditorState {
        EditorState::new("team/jira/api-key", baseline())
    }

    // -- Field labels are pinned ------------------------------

    #[test]
    fn field_labels_match_adr_020_keys() {
        assert_eq!(MetadataField::Description.label(), "description");
        assert_eq!(MetadataField::RetrievalUrl.label(), "retrieval_url");
        assert_eq!(MetadataField::RotateEveryDays.label(), "rotate_every_days");
        assert_eq!(MetadataField::ExpiresAt.label(), "expires_at");
        assert_eq!(MetadataField::PatternId.label(), "pattern_id");
    }

    // -- Dirtiness --------------------------------------------

    #[test]
    fn fresh_state_is_not_dirty() {
        let s = fresh();
        assert!(!s.is_dirty());
    }

    #[test]
    fn typing_into_a_focused_field_makes_state_dirty() {
        let mut s = fresh();
        s.type_char('!');
        assert!(s.is_dirty());
    }

    #[test]
    fn typing_into_a_button_does_not_alter_draft() {
        let mut s = fresh();
        s.cycle_focus_next();
        s.cycle_focus_next();
        s.cycle_focus_next();
        s.cycle_focus_next();
        s.cycle_focus_next(); // → Save
        assert_eq!(s.focus(), EditorFocus::Save);
        s.type_char('x');
        assert!(!s.is_dirty());
    }

    #[test]
    fn backspace_removes_the_last_char_of_focused_field() {
        let mut s = fresh();
        let before = s.draft().description.clone().unwrap();
        s.backspace();
        let after = s.draft().description.clone().unwrap();
        assert_eq!(after.len() + 1, before.len());
    }

    #[test]
    fn clearing_a_field_to_empty_normalizes_to_none() {
        let mut s = fresh();
        s.set_field(MetadataField::Description, "");
        assert!(s.draft().description.is_none());
    }

    // -- Focus cycling ----------------------------------------

    #[test]
    fn focus_cycle_walks_form_then_buttons_in_edit_mode() {
        let mut s = fresh();
        let order = [
            EditorFocus::Field(MetadataField::Description),
            EditorFocus::Field(MetadataField::RetrievalUrl),
            EditorFocus::Field(MetadataField::RotateEveryDays),
            EditorFocus::Field(MetadataField::ExpiresAt),
            EditorFocus::Field(MetadataField::PatternId),
            EditorFocus::Save,
            EditorFocus::Cancel,
            EditorFocus::Field(MetadataField::Description),
        ];
        for (i, expected) in order.iter().enumerate() {
            assert_eq!(&s.focus(), expected, "step {i}");
            s.cycle_focus_next();
        }
    }

    #[test]
    fn focus_cycle_includes_suggestion_buttons_when_in_apply_mode() {
        let mut s = fresh();
        s.apply_suggestion(MetadataDraft {
            description: Some("Better description".into()),
            ..MetadataDraft::default()
        });
        // After apply_suggestion, focus = AcceptSuggestion.
        assert_eq!(s.focus(), EditorFocus::AcceptSuggestion);
        s.cycle_focus_next();
        assert_eq!(s.focus(), EditorFocus::RejectSuggestion);
        s.cycle_focus_next();
        assert_eq!(s.focus(), EditorFocus::Save);
    }

    #[test]
    fn focus_cycle_prev_walks_backwards() {
        let mut s = fresh();
        s.cycle_focus_prev();
        assert_eq!(s.focus(), EditorFocus::Cancel);
    }

    // -- Suggestion handling ----------------------------------

    #[test]
    fn apply_suggestion_switches_mode_and_parks_focus_on_accept() {
        let mut s = fresh();
        s.apply_suggestion(MetadataDraft::default());
        assert_eq!(s.mode(), EditorMode::ApplySuggestion);
        assert_eq!(s.focus(), EditorFocus::AcceptSuggestion);
    }

    #[test]
    fn accept_suggestion_copies_only_present_fields_into_draft() {
        let mut s = fresh();
        s.apply_suggestion(MetadataDraft {
            description: Some("Better description".into()),
            // retrieval_url not present → must NOT clear
            // existing.
            ..MetadataDraft::default()
        });
        s.accept_suggestion();
        assert_eq!(s.draft().description.as_deref(), Some("Better description"));
        assert_eq!(
            s.draft().retrieval_url.as_deref(),
            Some("https://example.invalid/jira/tokens"),
            "additive: omitted suggestion field must not blank existing draft entry"
        );
    }

    #[test]
    fn reject_suggestion_drops_it_and_returns_to_edit() {
        let mut s = fresh();
        s.apply_suggestion(MetadataDraft {
            description: Some("Better description".into()),
            ..MetadataDraft::default()
        });
        s.reject_suggestion();
        assert_eq!(s.mode(), EditorMode::Edit);
        assert!(s.suggestion().is_none());
        // Draft must be untouched.
        assert_eq!(s.draft().description.as_deref(), Some("Jira API key"));
    }

    #[test]
    fn diff_marks_changed_fields_only() {
        let mut s = fresh();
        s.apply_suggestion(MetadataDraft {
            description: Some("Better description".into()),
            retrieval_url: Some("https://example.invalid/jira/tokens".into()), // unchanged
            ..MetadataDraft::default()
        });
        let d = s.diff();
        let by_field: std::collections::HashMap<MetadataField, &FieldDiff> =
            d.iter().map(|d| (d.field, d)).collect();
        assert!(by_field[&MetadataField::Description].changed);
        assert!(!by_field[&MetadataField::RetrievalUrl].changed);
        assert!(!by_field[&MetadataField::RotateEveryDays].changed);
    }

    #[test]
    fn diff_returns_empty_when_no_suggestion_present() {
        let s = fresh();
        assert!(s.diff().is_empty());
    }

    // -- Save guards ------------------------------------------

    #[test]
    fn save_blocks_when_no_changes_have_been_made() {
        let mut s = fresh();
        let out = s.save();
        assert!(out.is_none());
        assert!(matches!(
            s.status(),
            EditorStatus::ValidationFailed { reason, .. } if reason.contains("no changes")
        ));
    }

    #[test]
    fn save_rejects_non_integer_rotate_every_days() {
        let mut s = fresh();
        s.set_field(MetadataField::RotateEveryDays, "ninety");
        let out = s.save();
        assert!(out.is_none());
        match s.status() {
            EditorStatus::ValidationFailed { field, reason } => {
                assert_eq!(*field, MetadataField::RotateEveryDays);
                assert!(reason.contains("integer"));
            }
            other => panic!("unexpected status {other:?}"),
        }
    }

    #[test]
    fn save_rejects_out_of_range_rotate_every_days() {
        let mut s = fresh();
        s.set_field(MetadataField::RotateEveryDays, "0");
        assert!(s.save().is_none());
    }

    #[test]
    fn save_rejects_malformed_expires_at() {
        let mut s = fresh();
        s.set_field(MetadataField::ExpiresAt, "tomorrow");
        let out = s.save();
        assert!(out.is_none());
        match s.status() {
            EditorStatus::ValidationFailed { field, reason } => {
                assert_eq!(*field, MetadataField::ExpiresAt);
                assert!(reason.contains("ISO-8601"));
            }
            other => panic!("unexpected status {other:?}"),
        }
    }

    #[test]
    fn successful_save_produces_parsed_submission() {
        let mut s = fresh();
        s.set_field(MetadataField::Description, "Jira API key (rotated)");
        let out = s.save().expect("valid save");
        assert_eq!(out.path, "team/jira/api-key");
        assert_eq!(out.description.as_deref(), Some("Jira API key (rotated)"));
        assert_eq!(out.rotate_every_days, Some(90));
        assert_eq!(out.expires_at.unwrap().to_string(), "2026-12-01");
        assert!(matches!(s.status(), EditorStatus::Saving));
    }

    // -- Lifecycle --------------------------------------------

    #[test]
    fn cancel_resets_draft_to_baseline_and_drops_suggestion() {
        let mut s = fresh();
        s.set_field(MetadataField::Description, "junk");
        s.apply_suggestion(MetadataDraft::default());
        s.cancel();
        assert_eq!(s.draft(), s.original());
        assert!(s.suggestion().is_none());
        assert!(matches!(s.status(), EditorStatus::Cancelled));
    }

    #[test]
    fn apply_status_overrides_lifecycle_label() {
        let mut s = fresh();
        s.apply_status(EditorStatus::Saved);
        assert_eq!(s.status().label(), "saved");
    }

    // -- Render snapshot --------------------------------------

    #[cfg(feature = "tui")]
    #[test]
    fn render_edit_mode_writes_form_chrome_into_test_backend() {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;

        let backend = TestBackend::new(120, 30);
        let mut terminal = Terminal::new(backend).unwrap();
        let state = fresh();
        terminal
            .draw(|f| {
                let area = f.area();
                render(f, &state, area);
            })
            .unwrap();
        let buffer = terminal.backend().buffer().clone();
        let dump: String = buffer.content().iter().map(|c| c.symbol()).collect();
        assert!(dump.contains("Edit metadata"));
        assert!(dump.contains("description"));
        assert!(dump.contains("retrieval_url"));
        assert!(dump.contains("rotate_every_days"));
        assert!(dump.contains("expires_at"));
        assert!(dump.contains("pattern_id"));
        assert!(dump.contains("Save"));
        assert!(dump.contains("Cancel"));
        // Edit mode → no diff column header.
        assert!(!dump.contains("PROPOSED"));
    }

    #[cfg(feature = "tui")]
    #[test]
    fn render_apply_suggestion_mode_shows_diff_columns() {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;

        let backend = TestBackend::new(120, 30);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut state = fresh();
        state.apply_suggestion(MetadataDraft {
            description: Some("Better description".into()),
            ..MetadataDraft::default()
        });
        terminal
            .draw(|f| {
                let area = f.area();
                render(f, &state, area);
            })
            .unwrap();
        let buffer = terminal.backend().buffer().clone();
        let dump: String = buffer.content().iter().map(|c| c.symbol()).collect();
        assert!(dump.contains("Suggestion diff"));
        assert!(dump.contains("FIELD"));
        assert!(dump.contains("CURRENT"));
        assert!(dump.contains("PROPOSED"));
        assert!(dump.contains("Better description"));
        assert!(dump.contains("Accept suggestion"));
        assert!(dump.contains("Reject suggestion"));
    }
}
