//! TUI Provision / Rotation dialog per [ADR-023] §3.4 (MVP views,
//! Provision/Rotation).
//!
//! A modal opened in response to `request_provision` /
//! `request_rotation`. Per the ADR:
//!
//! > Modal dialog: shows path metadata (provider, format,
//! > rotation method), an `[Open URL]` button that launches the
//! > provider UI, a hidden value input, and `[Validate & save]`.
//! > For `mode = rotation` the title changes and a destructive
//! > confirmation must be acknowledged before save. The agent
//! > never receives the value: the dialog hands the value to the
//! > daemon directly via the local socket.
//!
//! ## Architecture
//!
//! Same shape as [`crate::inventory`]: pure view-model
//! ([`DialogState`]) plus a thin `render` function. Key handlers
//! mutate the state through `cycle_focus_*`, `type_char`,
//! `backspace`, `clear_value`, `toggle_confirm`, `submit`. The
//! orchestration (P12+ daemon glue) wires `submit()` to the
//! actual RPC and feeds the daemon's reply back via
//! [`DialogState::apply_status`].
//!
//! ## Value handling
//!
//! The hidden input is held in a [`SecretString`] (zeroize on
//! drop). The render function only ever sees the **length** and
//! emits bullet glyphs — the actual chars never reach the
//! buffer, so the snapshot tests can prove "no values are
//! rendered" with a simple substring check.
//!
//! [ADR-023]: https://github.com/meteora-pro/devboy-tools/blob/main/docs/architecture/adr/ADR-023-secret-store-ux-layer.md

use secrecy::{ExposeSecret, SecretString};

#[cfg(feature = "tui")]
use ratatui::Frame;
#[cfg(feature = "tui")]
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
#[cfg(feature = "tui")]
use ratatui::style::{Color, Modifier, Style};
#[cfg(feature = "tui")]
use ratatui::text::{Line, Span};
#[cfg(feature = "tui")]
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};

// =============================================================================
// Domain types
// =============================================================================

/// Distinguishes a first-time provisioning from a rotation. Per
/// ADR-023 §3.4, only the title and the "destructive confirm"
/// gate change between the two — the rest of the modal is
/// shared.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DialogMode {
    /// First-time write — no prior value to overwrite.
    Provision,
    /// Replacement of an existing value — requires explicit
    /// confirmation per ADR-023's "destructive operations are
    /// gated" rule.
    Rotation,
}

impl DialogMode {
    /// Title rendered at the top of the modal. Pinned to the
    /// ADR copy so a future i18n pass has a single source.
    pub fn title(self) -> &'static str {
        match self {
            Self::Provision => "Provision secret",
            Self::Rotation => "Rotate secret",
        }
    }

    /// Whether this mode requires the destructive-confirm
    /// checkbox to be ticked before `submit()` will accept.
    pub fn requires_confirmation(self) -> bool {
        matches!(self, Self::Rotation)
    }
}

/// Read-only metadata shown in the upper half of the modal.
/// Constructed by the orchestration layer from the manifest +
/// router resolution + the active token catalog. None of the
/// fields contain secret values.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DialogMetadata {
    /// ADR-020 path, e.g. `team/jira/api-key`.
    pub path: String,
    /// Provider label rendered as `via <provider>` — e.g.
    /// `1password`, `keychain`, `local-vault`.
    pub provider: String,
    /// Rotation method declared in the manifest / pattern.
    /// Free text rather than an enum to keep this crate decoupled
    /// from `devboy-storage::RotationMethod`.
    pub rotation_method: String,
    /// Provider URL the `[Open URL]` button hands off to the OS
    /// browser. None disables the button.
    pub provisioning_url: Option<String>,
    /// Human-readable hint about the expected format ("Bearer
    /// token, 36 chars" or "PEM block"). Rendered next to the
    /// input.
    pub format_hint: Option<String>,
    /// Free-prose description of the token variant — surfaced
    /// above the steps so the user knows what they are
    /// requesting. Sourced from
    /// `ProviderCatalog.variants[i].description` (or the
    /// catalog-level description when there's no variant
    /// override). Empty when no catalog matched the path.
    pub description: Option<String>,
    /// Numbered procedure walking the user through obtaining
    /// the value at the provider's console. Each entry renders
    /// as one numbered line; an empty Vec hides the block.
    /// Sourced from
    /// `ProviderCatalog.variants[i].retrieval.steps`.
    pub retrieval_steps: Vec<String>,
    /// Caveat / pro-tip rendered as a small note after the
    /// steps. Common content: "project-scoped keys inherit
    /// billing from the parent" / "rotate every 90 days" /
    /// "use a service-account email instead of a user token".
    /// Sourced from
    /// `ProviderCatalog.variants[i].retrieval.notes`.
    pub retrieval_notes: Option<String>,
    /// Provider's official documentation for the credential —
    /// the page explaining scopes / best practices, distinct
    /// from the creation page (`provisioning_url`). Rendered
    /// as a "Provider docs" link. Sourced from
    /// `ProviderCatalog.variants[i].retrieval.docs_url`.
    pub docs_url: Option<String>,
    /// Provider's rotation / key-hygiene guide. Rendered as a
    /// "Rotation guide" link in the dialog's rotation section.
    /// Sourced from
    /// `ProviderCatalog.variants[i].rotation.guide_url`.
    pub rotation_guide_url: Option<String>,
    /// Concrete rotation procedure — the "how" of rotating
    /// this credential (atomic pair rotation, reinstall after
    /// scope change, overlap window). Rendered as a block in
    /// the rotation section. Sourced from
    /// `ProviderCatalog.variants[i].rotation.notes`.
    pub rotation_notes: Option<String>,
}

/// Lifecycle status of a dialog. The orchestration layer
/// updates this through [`DialogState::apply_status`] as the
/// daemon RPC progresses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DialogStatus {
    /// Awaiting user input — the default after `new`.
    Idle,
    /// Submission in flight — `submit()` has produced a
    /// [`DialogSubmission`] and the orchestration is waiting for
    /// the daemon's reply.
    Submitting,
    /// Daemon validated and persisted the value. The dialog
    /// should be closed by the caller after a brief "saved"
    /// affordance.
    Saved,
    /// Validation failed — rendered next to the input. Reason
    /// is plain text (e.g. `"value does not match format_regex"`).
    ValidationFailed { reason: String },
    /// User dismissed the modal without submitting.
    Cancelled,
}

impl DialogStatus {
    /// Lower-cased label rendered in the dialog footer. Stable
    /// strings for snapshot tests and a future log scrape.
    pub fn label(&self) -> &str {
        match self {
            Self::Idle => "ready",
            Self::Submitting => "submitting…",
            Self::Saved => "saved",
            Self::ValidationFailed { .. } => "validation failed",
            Self::Cancelled => "cancelled",
        }
    }
}

/// Focus targets within the modal. `cycle_focus_next/prev`
/// rotates through them in render order; the destructive-confirm
/// checkbox is only present when `mode = Rotation`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DialogFocus {
    /// `[Open URL]` button — Enter launches the OS browser via
    /// the orchestration callback.
    OpenUrl,
    /// Hidden input field receiving keystrokes.
    ValueInput,
    /// Destructive-confirm checkbox (rotation only).
    ConfirmRotation,
    /// `[Validate & save]` button — Enter triggers `submit()`.
    ValidateAndSave,
    /// `[Cancel]` button — Enter sets status to `Cancelled`.
    Cancel,
}

impl DialogFocus {
    fn next_in_mode(self, mode: DialogMode) -> Self {
        match (self, mode) {
            (Self::OpenUrl, _) => Self::ValueInput,
            (Self::ValueInput, DialogMode::Rotation) => Self::ConfirmRotation,
            (Self::ValueInput, DialogMode::Provision) => Self::ValidateAndSave,
            (Self::ConfirmRotation, _) => Self::ValidateAndSave,
            (Self::ValidateAndSave, _) => Self::Cancel,
            (Self::Cancel, _) => Self::OpenUrl,
        }
    }

    fn prev_in_mode(self, mode: DialogMode) -> Self {
        match (self, mode) {
            (Self::OpenUrl, _) => Self::Cancel,
            (Self::ValueInput, _) => Self::OpenUrl,
            (Self::ConfirmRotation, _) => Self::ValueInput,
            (Self::ValidateAndSave, DialogMode::Rotation) => Self::ConfirmRotation,
            (Self::ValidateAndSave, DialogMode::Provision) => Self::ValueInput,
            (Self::Cancel, _) => Self::ValidateAndSave,
        }
    }
}

/// Result of a successful `submit()`. The caller hands this to
/// the daemon over the local socket and feeds the daemon's reply
/// back via `apply_status`.
#[derive(Debug)]
pub struct DialogSubmission {
    /// What kind of write the daemon should perform.
    pub mode: DialogMode,
    /// ADR-020 path the value targets.
    pub path: String,
    /// The value, still wrapped in `SecretString` so the
    /// orchestration layer is forced to call `expose_secret()`
    /// explicitly when handing it off.
    pub value: SecretString,
    /// `true` only when `mode = Rotation` and the user ticked
    /// the destructive-confirm box. For `Provision` this field
    /// is always `true`.
    pub confirmed: bool,
}

/// View-model for the provision/rotation modal. Mutated through
/// the small set of methods documented below; `render` is the
/// only function that touches `ratatui`.
#[derive(Debug)]
pub struct DialogState {
    mode: DialogMode,
    metadata: DialogMetadata,
    value: SecretString,
    value_len: usize,
    confirm_checked: bool,
    focus: DialogFocus,
    status: DialogStatus,
}

impl DialogState {
    /// Build a fresh dialog. Initial focus is the value input —
    /// it's what users hit first in 99% of flows; jumping to the
    /// `[Open URL]` button is one Tab-back away. For rotation
    /// the confirmation checkbox starts unchecked.
    pub fn new(mode: DialogMode, metadata: DialogMetadata) -> Self {
        Self {
            mode,
            metadata,
            value: SecretString::new("".to_string().into()),
            value_len: 0,
            confirm_checked: false,
            focus: DialogFocus::ValueInput,
            status: DialogStatus::Idle,
        }
    }

    pub fn mode(&self) -> DialogMode {
        self.mode
    }

    pub fn metadata(&self) -> &DialogMetadata {
        &self.metadata
    }

    pub fn focus(&self) -> DialogFocus {
        self.focus
    }

    pub fn status(&self) -> &DialogStatus {
        &self.status
    }

    pub fn confirm_checked(&self) -> bool {
        self.confirm_checked
    }

    /// Length of the held value in code points. Render uses this
    /// to decide how many bullet glyphs to draw; tests use it to
    /// assert on input progress without ever seeing the chars.
    pub fn value_len(&self) -> usize {
        self.value_len
    }

    /// Apply a daemon-side status update — typically a transition
    /// from `Submitting` to `Saved` or `ValidationFailed`.
    pub fn apply_status(&mut self, status: DialogStatus) {
        self.status = status;
    }

    pub fn cycle_focus_next(&mut self) {
        self.focus = self.focus.next_in_mode(self.mode);
    }

    pub fn cycle_focus_prev(&mut self) {
        self.focus = self.focus.prev_in_mode(self.mode);
    }

    /// Append a single character to the hidden input. Only does
    /// anything when the input is focused — protects against
    /// stray keystrokes leaking into the buffer when a button is
    /// focused. Rebuilds the `SecretString` (the crate doesn't
    /// expose interior mutability for `SecretString`, so an
    /// allocation per keystroke is the right cost-vs-safety
    /// trade-off).
    pub fn type_char(&mut self, c: char) {
        if self.focus != DialogFocus::ValueInput {
            return;
        }
        let mut s = self.value.expose_secret().to_string();
        s.push(c);
        self.value_len = s.chars().count();
        self.value = SecretString::new(s.into());
    }

    /// Drop the last character from the hidden input.
    pub fn backspace(&mut self) {
        if self.focus != DialogFocus::ValueInput {
            return;
        }
        let mut s = self.value.expose_secret().to_string();
        s.pop();
        self.value_len = s.chars().count();
        self.value = SecretString::new(s.into());
    }

    /// Clone the current value into a fresh `String` for use by
    /// a GUI text-input binding. The returned buffer is the
    /// caller's responsibility to drop or feed back through
    /// [`Self::replace_value_str`]. Documented as
    /// `for_edit` so the intent (transient editing) is explicit
    /// at the call site.
    pub fn value_clone_for_edit(&self) -> String {
        self.value.expose_secret().to_string()
    }

    /// Wholesale replace the secret value from a GUI buffer.
    /// The previous `SecretString` is dropped here, which
    /// zeroizes its memory. Updates the cached length.
    pub fn replace_value_str(&mut self, s: String) {
        self.value_len = s.chars().count();
        self.value = SecretString::new(s.into());
    }

    /// Wipe the entered value. Intended for "user typed garbage,
    /// hit Ctrl-U" — the previous `SecretString` is dropped
    /// here, which zeroizes its memory.
    pub fn clear_value(&mut self) {
        self.value = SecretString::new("".to_string().into());
        self.value_len = 0;
    }

    /// Toggle the destructive-confirm checkbox. No-op outside
    /// rotation mode — provisioning has nothing to overwrite.
    pub fn toggle_confirm(&mut self) {
        if self.mode == DialogMode::Rotation {
            self.confirm_checked = !self.confirm_checked;
        }
    }

    /// Submission guard. Returns `Some(DialogSubmission)` when:
    ///
    /// 1. The value input is non-empty.
    /// 2. For rotation, the destructive-confirm checkbox is
    ///    ticked.
    ///
    /// On success the dialog flips to `Submitting`. On guard
    /// failure the status is updated with a useful reason and
    /// `None` is returned — the caller renders the new status
    /// without dismissing the modal.
    pub fn submit(&mut self) -> Option<DialogSubmission> {
        if self.value_len == 0 {
            self.status = DialogStatus::ValidationFailed {
                reason: "value is empty".into(),
            };
            return None;
        }
        if self.mode.requires_confirmation() && !self.confirm_checked {
            self.status = DialogStatus::ValidationFailed {
                reason: "rotation must be confirmed before save".into(),
            };
            return None;
        }
        self.status = DialogStatus::Submitting;
        Some(DialogSubmission {
            mode: self.mode,
            path: self.metadata.path.clone(),
            value: SecretString::new(self.value.expose_secret().to_string().into()),
            confirmed: self.confirm_checked || !self.mode.requires_confirmation(),
        })
    }

    /// User dismissed the modal. The orchestration layer should
    /// drop the state right after this — `clear_value` is called
    /// here defensively so even a delayed drop doesn't keep the
    /// value alive.
    pub fn cancel(&mut self) {
        self.clear_value();
        self.status = DialogStatus::Cancelled;
    }
}

// =============================================================================
// Render
// =============================================================================

/// Render the provision/rotation modal into `area`.
///
/// Draws into the buffer in five vertical stripes:
///
/// 1. Title (varies by `mode`).
/// 2. Metadata block — path / provider / rotation method /
///    format hint.
/// 3. `[Open URL]` action row.
/// 4. Hidden input (`••••` × `value_len`) with focus marker.
/// 5. Destructive-confirm row (rotation only) and the
///    `[Validate & save]` / `[Cancel]` buttons.
///
/// The render function is intentionally allocation-light — it
/// borrows from `state` instead of cloning. Tests render into a
/// `TestBackend` and assert on the visible chrome.
#[cfg(feature = "tui")]
pub fn render(frame: &mut Frame<'_>, state: &DialogState, area: Rect) {
    // Carve the modal out of the available area — 70% width,
    // 70% height, centered. We `Clear` the underlying buffer
    // first so the modal sits on top of whatever was beneath
    // (typically the inventory table).
    let modal = centered_rect(70, 70, area);
    frame.render_widget(Clear, modal);

    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" {} ", state.mode.title()))
        .title_alignment(Alignment::Left)
        .border_style(border_style_for_mode(state.mode));
    let inner = block.inner(modal);
    frame.render_widget(block, modal);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(metadata_height(state)),
            Constraint::Length(2), // [Open URL] row
            Constraint::Length(3), // hidden input
            Constraint::Length(if state.mode == DialogMode::Rotation {
                2
            } else {
                0
            }),
            Constraint::Length(2), // action buttons
            Constraint::Min(1),    // status footer
        ])
        .split(inner);

    render_metadata(frame, state, chunks[0]);
    render_open_url(frame, state, chunks[1]);
    render_input(frame, state, chunks[2]);
    if state.mode == DialogMode::Rotation {
        render_confirm(frame, state, chunks[3]);
    }
    render_buttons(frame, state, chunks[4]);
    render_status(frame, state, chunks[5]);
}

#[cfg(feature = "tui")]
fn border_style_for_mode(mode: DialogMode) -> Style {
    match mode {
        DialogMode::Provision => Style::default().fg(Color::Cyan),
        // Rotation = destructive — paint the border red so the
        // affordance matches the gate.
        DialogMode::Rotation => Style::default().fg(Color::Red),
    }
}

#[cfg(feature = "tui")]
fn metadata_height(state: &DialogState) -> u16 {
    // path + provider + rotation method + (optional) format hint
    let mut h: u16 = 3;
    if state.metadata.format_hint.is_some() {
        h += 1;
    }
    // Catalog-derived fields (S2 / U-series): description,
    // numbered procedure, notes. Each occupies a separate
    // visual block so the dialog matches what egui renders.
    if state.metadata.description.is_some() {
        // Approximate one line; long descriptions wrap and
        // get truncated until we add scroll (future epic).
        h += 2;
    }
    if !state.metadata.retrieval_steps.is_empty() {
        // 1 line header + 1 line per step + blank separator
        h += 1 + state.metadata.retrieval_steps.len() as u16 + 1;
    }
    if state.metadata.retrieval_notes.is_some() {
        h += 2;
    }
    // Guide fields (G-series): a DOCS line, plus a rotation
    // block (header + notes + guide URL).
    if state.metadata.docs_url.is_some() {
        h += 1;
    }
    if state.metadata.rotation_notes.is_some() || state.metadata.rotation_guide_url.is_some() {
        // blank separator + header + (optional) notes line +
        // (optional) guide-url line
        h += 2;
        if state.metadata.rotation_notes.is_some() {
            h += 1;
        }
        if state.metadata.rotation_guide_url.is_some() {
            h += 1;
        }
    }
    h
}

#[cfg(feature = "tui")]
fn render_metadata(frame: &mut Frame<'_>, state: &DialogState, area: Rect) {
    let mut lines = vec![
        Line::from(vec![
            Span::styled("PATH      ", Style::default().add_modifier(Modifier::DIM)),
            Span::styled(
                &state.metadata.path,
                Style::default().add_modifier(Modifier::BOLD),
            ),
        ]),
        Line::from(vec![
            Span::styled("VIA       ", Style::default().add_modifier(Modifier::DIM)),
            Span::raw(&state.metadata.provider),
        ]),
        Line::from(vec![
            Span::styled("ROTATION  ", Style::default().add_modifier(Modifier::DIM)),
            Span::raw(&state.metadata.rotation_method),
        ]),
    ];
    if let Some(hint) = state.metadata.format_hint.as_deref() {
        lines.push(Line::from(vec![
            Span::styled("FORMAT    ", Style::default().add_modifier(Modifier::DIM)),
            Span::raw(hint),
        ]));
    }
    // Provider docs URL (G-series) — the page that explains
    // scopes / best practices, distinct from the creation
    // page on the [Open URL] button.
    if let Some(docs) = state.metadata.docs_url.as_deref() {
        lines.push(Line::from(vec![
            Span::styled("DOCS      ", Style::default().add_modifier(Modifier::DIM)),
            Span::raw(docs),
        ]));
    }
    // Variant description — italic-style block when the
    // catalog matched a variant (S2 / U-series).
    if let Some(desc) = state.metadata.description.as_deref() {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            desc,
            Style::default().add_modifier(Modifier::ITALIC),
        )));
    }
    // Numbered procedure from `retrieval.steps`. Each entry on
    // its own line; mirrors the egui rendering.
    if !state.metadata.retrieval_steps.is_empty() {
        lines.push(Line::from(Span::styled(
            "How to obtain:",
            Style::default().add_modifier(Modifier::BOLD),
        )));
        for (i, step) in state.metadata.retrieval_steps.iter().enumerate() {
            lines.push(Line::from(format!("  {}. {step}", i + 1)));
        }
    }
    // Caveat / pro-tip after the steps. Dimmed so it visually
    // de-prioritises against the actionable list above.
    if let Some(notes) = state.metadata.retrieval_notes.as_deref() {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            format!("Note: {notes}"),
            Style::default().add_modifier(Modifier::DIM),
        )));
    }
    // Rotation section (G-series) — the "how to rotate" half
    // of the catalog contract. Only rendered when the catalog
    // carries rotation guidance.
    if state.metadata.rotation_notes.is_some() || state.metadata.rotation_guide_url.is_some() {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "Rotating this secret:",
            Style::default().add_modifier(Modifier::BOLD),
        )));
        if let Some(notes) = state.metadata.rotation_notes.as_deref() {
            lines.push(Line::from(format!("  {notes}")));
        }
        if let Some(url) = state.metadata.rotation_guide_url.as_deref() {
            lines.push(Line::from(vec![
                Span::styled("  guide: ", Style::default().add_modifier(Modifier::DIM)),
                Span::raw(url),
            ]));
        }
    }
    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), area);
}

#[cfg(feature = "tui")]
fn render_open_url(frame: &mut Frame<'_>, state: &DialogState, area: Rect) {
    let focused = state.focus == DialogFocus::OpenUrl;
    let label = match state.metadata.provisioning_url.as_deref() {
        Some(_) => "[ Open URL ]",
        None => "[ Open URL ]  (no URL configured)",
    };
    let style = button_style(focused, state.metadata.provisioning_url.is_some());
    frame.render_widget(Paragraph::new(Line::from(Span::styled(label, style))), area);
}

#[cfg(feature = "tui")]
fn render_input(frame: &mut Frame<'_>, state: &DialogState, area: Rect) {
    let focused = state.focus == DialogFocus::ValueInput;
    let bullets: String = "•".repeat(state.value_len);
    let cursor = if focused { "_" } else { "" };
    let body = format!("{bullets}{cursor}");
    let title = if focused {
        " value (hidden) ▶ "
    } else {
        " value (hidden) "
    };
    frame.render_widget(
        Paragraph::new(body).block(Block::default().borders(Borders::ALL).title(title)),
        area,
    );
}

#[cfg(feature = "tui")]
fn render_confirm(frame: &mut Frame<'_>, state: &DialogState, area: Rect) {
    let focused = state.focus == DialogFocus::ConfirmRotation;
    let mark = if state.confirm_checked { "[x]" } else { "[ ]" };
    let label = format!(
        "{mark} I understand this overwrites the current secret at {}",
        state.metadata.path
    );
    let style = if focused {
        Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::Red)
    };
    frame.render_widget(Paragraph::new(Line::from(Span::styled(label, style))), area);
}

#[cfg(feature = "tui")]
fn render_buttons(frame: &mut Frame<'_>, state: &DialogState, area: Rect) {
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(area);

    let save_focused = state.focus == DialogFocus::ValidateAndSave;
    let cancel_focused = state.focus == DialogFocus::Cancel;

    let save_label = "[ Validate & save ]";
    let cancel_label = "[ Cancel ]";

    let save_style = button_style(save_focused, true);
    let cancel_style = button_style(cancel_focused, true);

    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(save_label, save_style))),
        cols[0],
    );
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(cancel_label, cancel_style)))
            .alignment(Alignment::Right),
        cols[1],
    );
}

#[cfg(feature = "tui")]
fn render_status(frame: &mut Frame<'_>, state: &DialogState, area: Rect) {
    let line = match state.status() {
        DialogStatus::Idle => Line::from(Span::styled(
            "Tab=cycle focus  Enter=activate  Esc=cancel",
            Style::default().add_modifier(Modifier::DIM),
        )),
        DialogStatus::Submitting => Line::from("submitting…"),
        DialogStatus::Saved => Line::from(Span::styled(
            "saved",
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        )),
        DialogStatus::ValidationFailed { reason } => Line::from(Span::styled(
            format!("validation failed: {reason}"),
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        )),
        DialogStatus::Cancelled => Line::from(Span::styled(
            "cancelled",
            Style::default().add_modifier(Modifier::DIM),
        )),
    };
    frame.render_widget(Paragraph::new(line), area);
}

#[cfg(feature = "tui")]
fn button_style(focused: bool, enabled: bool) -> Style {
    match (focused, enabled) {
        (true, true) => Style::default().add_modifier(Modifier::REVERSED | Modifier::BOLD),
        (false, true) => Style::default(),
        (_, false) => Style::default().add_modifier(Modifier::DIM),
    }
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

    fn meta() -> DialogMetadata {
        DialogMetadata {
            path: "team/jira/api-key".into(),
            provider: "1password".into(),
            rotation_method: "provider-ui".into(),
            provisioning_url: Some("https://example.invalid/jira/tokens".into()),
            format_hint: Some("Bearer token, 36 chars".into()),
            ..DialogMetadata::default()
        }
    }

    // -- Mode ---------------------------------------------------

    #[test]
    fn provision_title_is_pinned_to_adr_copy() {
        assert_eq!(DialogMode::Provision.title(), "Provision secret");
    }

    #[test]
    fn rotation_title_is_pinned_to_adr_copy() {
        assert_eq!(DialogMode::Rotation.title(), "Rotate secret");
    }

    #[test]
    fn rotation_requires_confirmation_provision_does_not() {
        assert!(DialogMode::Rotation.requires_confirmation());
        assert!(!DialogMode::Provision.requires_confirmation());
    }

    // -- Focus cycling -----------------------------------------

    #[test]
    fn provision_focus_cycle_skips_confirm_checkbox() {
        let mut s = DialogState::new(DialogMode::Provision, meta());
        // Start at ValueInput per `new`.
        assert_eq!(s.focus(), DialogFocus::ValueInput);
        s.cycle_focus_next();
        assert_eq!(s.focus(), DialogFocus::ValidateAndSave);
        s.cycle_focus_next();
        assert_eq!(s.focus(), DialogFocus::Cancel);
        s.cycle_focus_next();
        assert_eq!(s.focus(), DialogFocus::OpenUrl);
        s.cycle_focus_next();
        assert_eq!(s.focus(), DialogFocus::ValueInput);
    }

    #[test]
    fn rotation_focus_cycle_includes_confirm_checkbox() {
        let mut s = DialogState::new(DialogMode::Rotation, meta());
        assert_eq!(s.focus(), DialogFocus::ValueInput);
        s.cycle_focus_next();
        assert_eq!(s.focus(), DialogFocus::ConfirmRotation);
        s.cycle_focus_next();
        assert_eq!(s.focus(), DialogFocus::ValidateAndSave);
    }

    #[test]
    fn focus_cycle_prev_walks_backwards() {
        let mut s = DialogState::new(DialogMode::Rotation, meta());
        s.cycle_focus_prev();
        assert_eq!(s.focus(), DialogFocus::OpenUrl);
        s.cycle_focus_prev();
        assert_eq!(s.focus(), DialogFocus::Cancel);
    }

    // -- Value input -------------------------------------------

    #[test]
    fn type_char_only_accepted_when_input_is_focused() {
        let mut s = DialogState::new(DialogMode::Provision, meta());
        s.cycle_focus_next(); // → ValidateAndSave
        s.type_char('x');
        assert_eq!(s.value_len(), 0, "keystroke leaked into a button");
    }

    #[test]
    fn type_char_extends_value_and_backspace_shrinks_it() {
        let mut s = DialogState::new(DialogMode::Provision, meta());
        for c in "hunter2".chars() {
            s.type_char(c);
        }
        assert_eq!(s.value_len(), 7);
        s.backspace();
        assert_eq!(s.value_len(), 6);
    }

    #[test]
    fn clear_value_resets_length_to_zero() {
        let mut s = DialogState::new(DialogMode::Provision, meta());
        s.type_char('x');
        s.clear_value();
        assert_eq!(s.value_len(), 0);
    }

    // -- Confirm checkbox --------------------------------------

    #[test]
    fn toggle_confirm_no_ops_in_provision_mode() {
        let mut s = DialogState::new(DialogMode::Provision, meta());
        s.toggle_confirm();
        assert!(!s.confirm_checked());
    }

    #[test]
    fn toggle_confirm_flips_in_rotation_mode() {
        let mut s = DialogState::new(DialogMode::Rotation, meta());
        s.toggle_confirm();
        assert!(s.confirm_checked());
        s.toggle_confirm();
        assert!(!s.confirm_checked());
    }

    // -- Submit guards -----------------------------------------

    #[test]
    fn submit_with_empty_value_fails_with_useful_reason() {
        let mut s = DialogState::new(DialogMode::Provision, meta());
        let out = s.submit();
        assert!(out.is_none());
        match s.status() {
            DialogStatus::ValidationFailed { reason } => assert!(reason.contains("empty")),
            other => panic!("unexpected status {other:?}"),
        }
    }

    #[test]
    fn rotation_submit_blocks_until_destructive_confirm_is_acknowledged() {
        let mut s = DialogState::new(DialogMode::Rotation, meta());
        for c in "newsecret".chars() {
            s.type_char(c);
        }
        assert!(s.submit().is_none());
        match s.status() {
            DialogStatus::ValidationFailed { reason } => assert!(reason.contains("confirm")),
            other => panic!("unexpected status {other:?}"),
        }
        s.toggle_confirm();
        let out = s.submit().expect("rotation now allowed");
        assert!(matches!(out.mode, DialogMode::Rotation));
        assert!(out.confirmed);
        assert_eq!(out.path, "team/jira/api-key");
        assert_eq!(out.value.expose_secret(), "newsecret");
    }

    #[test]
    fn provision_submit_marks_as_confirmed_without_a_checkbox() {
        let mut s = DialogState::new(DialogMode::Provision, meta());
        for c in "newsecret".chars() {
            s.type_char(c);
        }
        let out = s
            .submit()
            .expect("provision should pass once value present");
        assert!(
            out.confirmed,
            "provision needs no checkbox but should still be confirmed"
        );
    }

    #[test]
    fn submit_transitions_status_to_submitting_on_success() {
        let mut s = DialogState::new(DialogMode::Provision, meta());
        s.type_char('x');
        let _ = s.submit().unwrap();
        assert!(matches!(s.status(), DialogStatus::Submitting));
    }

    // -- Lifecycle ---------------------------------------------

    #[test]
    fn cancel_wipes_value_and_marks_cancelled() {
        let mut s = DialogState::new(DialogMode::Provision, meta());
        s.type_char('x');
        s.type_char('y');
        s.cancel();
        assert_eq!(s.value_len(), 0);
        assert!(matches!(s.status(), DialogStatus::Cancelled));
    }

    #[test]
    fn apply_status_overrides_lifecycle_label() {
        let mut s = DialogState::new(DialogMode::Provision, meta());
        s.apply_status(DialogStatus::Saved);
        assert_eq!(s.status().label(), "saved");
    }

    // -- Render snapshot ---------------------------------------

    #[cfg(feature = "tui")]
    #[test]
    fn render_provision_does_not_panic_and_writes_chrome_into_test_backend() {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;

        let backend = TestBackend::new(120, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        let state = DialogState::new(DialogMode::Provision, meta());
        terminal
            .draw(|f| {
                let area = f.area();
                render(f, &state, area);
            })
            .unwrap();
        let buffer = terminal.backend().buffer().clone();
        let dump: String = buffer.content().iter().map(|c| c.symbol()).collect();
        assert!(dump.contains("Provision secret"));
        assert!(dump.contains("PATH"));
        assert!(dump.contains("team/jira/api-key"));
        assert!(dump.contains("Open URL"));
        assert!(dump.contains("value (hidden)"));
        assert!(dump.contains("Validate & save"));
        assert!(dump.contains("Cancel"));
        // Provision mode must not render the confirm checkbox.
        assert!(!dump.contains("[x]"));
        assert!(!dump.contains("[ ] I understand"));
    }

    #[cfg(feature = "tui")]
    #[test]
    fn render_rotation_shows_destructive_confirm_row() {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;

        let backend = TestBackend::new(120, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        let state = DialogState::new(DialogMode::Rotation, meta());
        terminal
            .draw(|f| {
                let area = f.area();
                render(f, &state, area);
            })
            .unwrap();
        let buffer = terminal.backend().buffer().clone();
        let dump: String = buffer.content().iter().map(|c| c.symbol()).collect();
        assert!(dump.contains("Rotate secret"));
        assert!(dump.contains("I understand this overwrites"));
    }

    #[cfg(feature = "tui")]
    #[test]
    fn render_never_writes_the_value_into_the_buffer() {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;

        let backend = TestBackend::new(120, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut state = DialogState::new(DialogMode::Provision, meta());
        // A value the test can search for unambiguously.
        for c in "supersecretvaluexyz".chars() {
            state.type_char(c);
        }
        terminal
            .draw(|f| {
                let area = f.area();
                render(f, &state, area);
            })
            .unwrap();
        let buffer = terminal.backend().buffer().clone();
        let dump: String = buffer.content().iter().map(|c| c.symbol()).collect();
        assert!(!dump.contains("supersecretvaluexyz"));
        // Bullets stand in for the chars.
        assert!(dump.contains("•"));
    }

    // -- Catalog procedure rendering (U-series) -----------------

    /// Build a metadata bundle that mirrors what
    /// `metadata_from_catalog_and_entry` produces for
    /// `team/openai/api-key` once a catalog match is found.
    /// Pins what TUI / GUI snapshots must show.
    fn meta_with_catalog_procedure() -> DialogMetadata {
        DialogMetadata {
            path: "team/openai/api-key".into(),
            provider: "openai".into(),
            rotation_method: "manual".into(),
            provisioning_url: Some("https://platform.openai.com/api-keys".into()),
            format_hint: Some("starts with sk-, 20+ chars".into()),
            description: Some("Personal or workspace key issued at platform.openai.com.".into()),
            retrieval_steps: vec![
                "Open platform.openai.com/api-keys".into(),
                "Click Create new secret key".into(),
                "Copy the value — it is shown once".into(),
            ],
            retrieval_notes: Some(
                "Prefer sk-proj-... over user-scoped keys for server-to-server".into(),
            ),
            docs_url: Some("https://platform.openai.com/docs/api-reference/authentication".into()),
            rotation_guide_url: Some(
                "https://platform.openai.com/docs/guides/production-best-practices".into(),
            ),
            rotation_notes: Some(
                "Create the new key first, deploy it, then revoke the old one.".into(),
            ),
        }
    }

    #[cfg(feature = "tui")]
    #[test]
    fn render_includes_description_steps_and_notes_when_catalog_match_present() {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;

        // Bigger buffer so the 5 extra lines fit before the
        // Open-URL row.
        let backend = TestBackend::new(120, 36);
        let mut terminal = Terminal::new(backend).unwrap();
        let state = DialogState::new(DialogMode::Provision, meta_with_catalog_procedure());
        terminal
            .draw(|f| {
                let area = f.area();
                render(f, &state, area);
            })
            .unwrap();
        let buffer = terminal.backend().buffer().clone();
        let dump: String = buffer.content().iter().map(|c| c.symbol()).collect();

        // Catalog-derived blocks all render in the metadata
        // section above the Open URL row.
        assert!(
            dump.contains("Personal or workspace key"),
            "description must render above the steps:\n{dump}"
        );
        assert!(
            dump.contains("How to obtain:"),
            "step header must render:\n{dump}"
        );
        assert!(
            dump.contains("1. Open platform.openai.com/api-keys"),
            "step 1 must be numbered:\n{dump}"
        );
        assert!(
            dump.contains("Note: Prefer sk-proj-..."),
            "notes must render with the `Note:` prefix:\n{dump}"
        );
    }

    #[cfg(feature = "tui")]
    #[test]
    fn render_includes_docs_url_and_rotation_block_when_catalog_match_present() {
        // G-series: the DOCS line + the "Rotating this secret:"
        // block (notes + guide URL) must reach the terminal
        // modal alongside the U-series procedure.
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;

        // Tall buffer — the rotation block sits below the
        // 5-line procedure, so the metadata section needs
        // room.
        let backend = TestBackend::new(120, 44);
        let mut terminal = Terminal::new(backend).unwrap();
        let state = DialogState::new(DialogMode::Provision, meta_with_catalog_procedure());
        terminal
            .draw(|f| {
                let area = f.area();
                render(f, &state, area);
            })
            .unwrap();
        let buffer = terminal.backend().buffer().clone();
        let dump: String = buffer.content().iter().map(|c| c.symbol()).collect();

        assert!(
            dump.contains("DOCS"),
            "DOCS line must render the provider docs URL:\n{dump}"
        );
        assert!(
            dump.contains("docs/api-reference/authentication"),
            "DOCS line must carry the actual docs_url:\n{dump}"
        );
        assert!(
            dump.contains("Rotating this secret:"),
            "rotation section header must render:\n{dump}"
        );
        assert!(
            dump.contains("Create the new key first"),
            "rotation notes must render:\n{dump}"
        );
        assert!(
            dump.contains("guide:") && dump.contains("production-best-practices"),
            "rotation guide URL must render:\n{dump}"
        );
    }

    #[cfg(feature = "tui")]
    #[test]
    fn render_omits_docs_and_rotation_block_when_guide_fields_empty() {
        // The plain `meta()` fixture has no docs_url / rotation
        // guidance — neither block may appear.
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;

        let backend = TestBackend::new(120, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        let state = DialogState::new(DialogMode::Provision, meta());
        terminal
            .draw(|f| {
                let area = f.area();
                render(f, &state, area);
            })
            .unwrap();
        let buffer = terminal.backend().buffer().clone();
        let dump: String = buffer.content().iter().map(|c| c.symbol()).collect();
        assert!(
            !dump.contains("DOCS"),
            "DOCS line must NOT render when docs_url is None:\n{dump}"
        );
        assert!(
            !dump.contains("Rotating this secret:"),
            "rotation header must NOT render when guidance absent:\n{dump}"
        );
    }

    #[cfg(feature = "tui")]
    #[test]
    fn render_omits_catalog_blocks_when_metadata_is_empty() {
        // The pre-catalog fallback path: only the manifest
        // populated DialogMetadata. The catalog blocks must
        // NOT appear — otherwise the dialog confuses the user
        // with empty `How to obtain:` headers.
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;

        let backend = TestBackend::new(120, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        let state = DialogState::new(DialogMode::Provision, meta());
        terminal
            .draw(|f| {
                let area = f.area();
                render(f, &state, area);
            })
            .unwrap();
        let buffer = terminal.backend().buffer().clone();
        let dump: String = buffer.content().iter().map(|c| c.symbol()).collect();
        assert!(
            !dump.contains("How to obtain:"),
            "step header must NOT render when steps empty:\n{dump}"
        );
        assert!(
            !dump.contains("Note:"),
            "notes header must NOT render when notes None:\n{dump}"
        );
    }

    #[test]
    fn metadata_height_grows_with_each_catalog_field() {
        // The TUI layout has a fixed-height metadata block —
        // when the catalog populates description / steps /
        // notes, the block must grow to accommodate them.
        // Pin the contract so a future refactor of
        // metadata_height doesn't silently truncate the
        // bundle.
        let plain_state = DialogState::new(DialogMode::Provision, meta());
        let rich_state = DialogState::new(DialogMode::Provision, meta_with_catalog_procedure());
        let plain = metadata_height(&plain_state);
        let rich = metadata_height(&rich_state);
        assert!(
            rich > plain,
            "catalog-rich metadata block ({rich}) must be taller than the plain one ({plain})"
        );
        // Sanity bound for the full catalog-rich bundle:
        //   3 base
        // + 1 FORMAT
        // + 1 DOCS
        // + 2 description
        // + (1 hdr + 3 steps + 1 sep) retrieval steps
        // + 2 retrieval notes
        // + (2 + 1 notes + 1 guide) rotation block
        // = 18. Allow ±2 for any future tweak.
        assert!(
            (16..=20).contains(&rich),
            "rich metadata height {rich} drifted out of the expected range"
        );
    }
}
