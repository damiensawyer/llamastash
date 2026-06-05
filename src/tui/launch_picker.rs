//! Launch picker form state — the typed-knob editor.
//!
//! The Settings tab renders a vertical list of rows: every
//! `TypedKnobs` field (ctx, reasoning, n_gpu_layers, … in
//! `knob_specs()` order) with a per-row source label (`(user)`,
//! `(last used)`, `(arch default)`, `(model default)`,
//! `(server default)`), plus an `extras` free-text row at the
//! bottom. Up/Down moves between rows; Left/Right cycles the focused
//! row's value; `e` enters inline edit; Enter launches (or commits
//! an open edit); Backspace resets the focused row.

use std::cell::Cell;
use std::collections::BTreeMap;
use std::sync::LazyLock;

use crate::config::TypedKnobs;
use crate::gpu::Card;
use crate::launch::flag_aliases::{KnobField, KV_CACHE_TYPES};
use crate::launch::params::LayerLabel;

/// Pre-canned context-length presets surfaced as quick picks. Custom
/// values flow through the same field when the user types digits.
pub const CTX_PRESETS: &[u32] = &[2048, 4096, 8192, 16384, 32768, 65536, 131072];

/// Default device selector when no GPUs are detected — lets llama-server
/// auto-select (which may split across all GPUs on Vulkan). A single-item
/// list so cycle_through wraps back to `None` (reset) reliably.
pub const DEVICE_PRESETS: &[&str] = &[""];

/// Encode a card-first device selector as "card_index:driver_offset".
/// When `driver_offset` is -1 the card has no drivers (unified GPU
/// or single-driver card) so the user just sees the card name.
pub fn encode_device_selector(card_index: usize, driver_offset: i32) -> String {
  format!("{}:{}", card_index, driver_offset)
}

/// Parse a card-first device selector back to (card_index, driver_offset).
pub fn parse_device_selector(input: &str) -> (usize, i32) {
  if input.is_empty() {
    return (0, 0);
  }
  let parts = input.splitn(2, ':').collect::<Vec<_>>();
  match parts.as_slice() {
    [card_str, driver_str] => {
      let card = card_str.parse::<usize>().unwrap_or(0);
      let driver = driver_str.parse::<i32>().unwrap_or(-1);
      (card, driver)
    }
    [card_str] => (card_str.parse::<usize>().unwrap_or(0), -1),
    _ => (0, -1),
  }
}

/// Which row the cursor is on. The editor renders top-to-bottom in
/// [`PickerField::all`] order so it doubles as the vertical-navigation
/// order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PickerField {
  Knob(KnobField),
  Extras,
}

/// Lazily-built navigation order — every knob in `knob_specs()`
/// order (ctx, reasoning, n_gpu_layers, …), followed by `Extras`.
/// Built once on first access so per-keypress navigation does no
/// allocation.
static ALL_FIELDS: LazyLock<Box<[PickerField]>> = LazyLock::new(|| {
  let mut v: Vec<PickerField> = Vec::new();
  for spec in crate::launch::flag_aliases::knob_specs() {
    v.push(PickerField::Knob(spec.field));
  }
  v.push(PickerField::Extras);
  v.into_boxed_slice()
});

impl PickerField {
  /// All rows in render / navigation order. Returns a static slice so
  /// `next_field` / `prev_field` don't allocate on each keypress.
  pub fn all() -> &'static [PickerField] {
    &ALL_FIELDS
  }

  /// Whether `e:edit` opens an inline buffer on this row.
  ///
  /// - Numeric / float / enum knobs and the free-text `Extras` row
  ///   open an [`crate::tui::input_field::InputField`] for typing.
  /// - Boolean knobs (reasoning, flash_attn, mlock, no_mmap) don't —
  ///   they're cycled with ←/→. Surfacing `e:edit` on a boolean row
  ///   would be a no-op chip and a misleading affordance.
  ///
  /// Shared between the Settings-row edit handler (which early-returns
  /// on booleans) and the right-pane hint strip (which hides the chip
  /// on those rows) so the chip and the handler stay in lockstep.
  pub fn is_editable(self) -> bool {
    match self {
      PickerField::Extras => true,
      PickerField::Knob(k) => match k {
        KnobField::Reasoning | KnobField::FlashAttn | KnobField::Mlock | KnobField::NoMmap => false,
        KnobField::Ctx
        | KnobField::NGpuLayers
        | KnobField::Threads
        | KnobField::Parallel
        | KnobField::BatchSize
        | KnobField::UbatchSize
        | KnobField::Keep
        | KnobField::RopeFreqScale
        | KnobField::CacheTypeK
        | KnobField::CacheTypeV
        | KnobField::Device => true,
      },
    }
  }
}

/// Inline-edit state owned by [`LaunchPickerState`].
///
/// The buffer and modal `editing` flag live in `inline_edit`
/// ([`crate::tui::input_field::InputField`]) so the typed-knob editor shares the
/// `e:edit / Esc:walk-back / Enter:Submit` contract with every
/// other text input in the TUI. The wrapper carries the two extra
/// pieces of state `crate::tui::input_field::InputField` doesn't model:
///
/// - `field` — which `PickerField` the open edit is editing (numeric
///   / enum knob or the extras row), so `commit_inline_edit` knows
///   where to write the parsed value.
/// - `error` — the inline parse / validation error rendered under
///   the row when commit fails.
///
/// Both reset when the edit closes (either via successful commit
/// or `Esc` walk-back).
#[derive(Debug, Clone, Default)]
pub struct InlineEdit {
  pub field: Option<PickerField>,
  pub input: crate::tui::input_field::InputField,
  pub error: Option<String>,
}

impl InlineEdit {
  /// Open the edit on `field`, seed the buffer with `initial`, and
  /// enter edit mode so subsequent keystrokes append to the buffer.
  pub fn open(&mut self, field: PickerField, initial: String) {
    self.field = Some(field);
    self.input.set_text(initial);
    self.input.enter_edit();
    self.error = None;
  }

  /// Close the edit — clear the field marker, drop the buffer, exit
  /// edit mode, and clear any stale error.
  pub fn close(&mut self) {
    self.field = None;
    self.input.clear();
    self.input.exit_edit();
    self.error = None;
  }

  /// True while the user is actively typing into the buffer (the
  /// edit is open *and* `crate::tui::input_field::InputField` reports
  /// edit mode). Used by
  /// the event router to send keys to the input instead of the
  /// outer keymap.
  pub fn is_open(&self) -> bool {
    self.field.is_some() && self.input.is_editing()
  }
}

/// State of the launch picker.
#[derive(Debug, Clone)]
pub struct LaunchPickerState {
  /// Display name of the focused model (rendered in the title).
  pub model_name: String,
  /// User-supplied typed knobs (only fields the user explicitly set;
  /// every other field stays `None` and inherits from the resolved
  /// chain on render). Includes `ctx` and `reasoning`.
  pub user_knobs: TypedKnobs,
  /// Resolved knobs after applying the layered resolver — what the
  /// editor shows for each row.
  pub resolved: TypedKnobs,
  /// Per-knob source labels for the right-aligned origin chip.
  pub sources: BTreeMap<KnobField, LayerLabel>,
  /// Free-form argv tail forwarded to llama-server.
  pub extras: Vec<std::ffi::OsString>,
  /// Modal text-input for the extras row (`is_editing()` replaces
  /// the bespoke `extras_editing` bool; `buffer()` replaces the raw
  /// string + cursor pair). Shares the `e:edit / Esc:walk-back /
  /// Enter:Submit` contract with every other text input in the TUI.
  pub extras_input: crate::tui::input_field::InputField,
  /// Inline edit state for numeric / enum rows. Wraps an
  /// [`crate::tui::input_field::InputField`] plus the `PickerField`
  /// marker so the commit path
  /// knows which row to write back to, and an optional parse-error
  /// string rendered under the row.
  pub inline_edit: InlineEdit,
  pub field: PickerField,
  pub active_instances: usize,
  pub prefer_port: Option<u16>,
  /// Available GPU devices (backend-prefixed names), from host metrics.
  /// Used by the device picker to cycle through cards.
  pub devices: Vec<String>,
  /// Card-first device list from host metrics. Each card carries its
  /// available drivers. When a card has a single driver the picker
  /// shows just the card; when multiple drivers are available the user
  /// first selects a card then cycles through its drivers.
  pub cards: Vec<Card>,
  /// When the focused card has multiple drivers, this holds the
  /// selected driver offset within that card (0 = first driver,
  /// -1 = not yet selected). Only used when cards.len() > 0
  /// and the focused card has drivers.len() > 1.
  pub selected_driver_offset: i32,
  /// Row offset clipped from the top of the rendered line list so the
  /// focused row stays visible on small viewports. Recomputed on each
  /// render using the actual area height — the `Cell` lets the
  /// read-only render path (which only has `&App`) update the cached
  /// offset without taking a mutable borrow.
  pub scroll_offset: Cell<u16>,
}

impl LaunchPickerState {
  pub fn for_model(model_name: impl Into<String>) -> Self {
    Self {
      model_name: model_name.into(),
      user_knobs: TypedKnobs::default(),
      resolved: TypedKnobs::default(),
      sources: BTreeMap::new(),
      extras: Vec::new(),
      extras_input: crate::tui::input_field::InputField::default(),
      inline_edit: InlineEdit::default(),
      field: PickerField::Knob(KnobField::Ctx),
      active_instances: 0,
      prefer_port: None,
      devices: Vec::new(),
      cards: Vec::new(),
      selected_driver_offset: -1,
      scroll_offset: Cell::new(0),
    }
  }

  /// Seed the resolved knobs + source map from the layered resolver
  /// output. The user-knobs layer is empty on a freshly-opened
  /// editor — the rows show inherited values.
  pub fn set_resolved(&mut self, resolved: TypedKnobs, sources: BTreeMap<KnobField, LayerLabel>) {
    self.resolved = resolved;
    self.sources = sources;
  }

  /// Cycle the focused field's value forward (Right arrow).
  pub fn cycle_focused_value_next(&mut self) {
    match self.field {
      PickerField::Knob(k) => self.cycle_knob(k, true),
      PickerField::Extras => {}
    }
  }

  /// Cycle the focused field's value backward (Left arrow).
  pub fn cycle_focused_value_prev(&mut self) {
    match self.field {
      PickerField::Knob(k) => self.cycle_knob(k, false),
      PickerField::Extras => {}
    }
  }

  fn cycle_knob(&mut self, field: KnobField, forward: bool) {
    match field {
      KnobField::Ctx => self.cycle_u32(field, CTX_PRESETS, forward),
      KnobField::Reasoning => self.cycle_bool(field, forward),
      KnobField::NGpuLayers => self.cycle_u32(field, &[0, 16, 32, 64, 99], forward),
      KnobField::Threads => self.cycle_u32(field, &[1, 2, 4, 6, 8, 12, 16, 24], forward),
      KnobField::Parallel => self.cycle_u32(field, &[1, 2, 4, 8, 16], forward),
      KnobField::BatchSize => self.cycle_u32(field, &[256, 512, 1024, 2048, 4096], forward),
      KnobField::UbatchSize => self.cycle_u32(field, &[128, 256, 512, 1024], forward),
      KnobField::Keep => self.cycle_u32(field, &[0, 64, 128, 256, 512, 1024], forward),
      KnobField::RopeFreqScale => self.cycle_f32(field, &[0.5, 1.0, 2.0, 4.0], forward),
      KnobField::CacheTypeK | KnobField::CacheTypeV => self.cycle_enum(field, forward),
      KnobField::FlashAttn | KnobField::Mlock | KnobField::NoMmap => {
        self.cycle_bool(field, forward)
      }
      KnobField::Device => self.cycle_device(field, forward),
    }
  }

  fn cycle_u32(&mut self, field: KnobField, presets: &[u32], forward: bool) {
    let current = self.user_value_u32(field);
    let next = cycle_through(current, presets, forward);
    self.set_user_u32(field, next);
  }

  fn cycle_f32(&mut self, field: KnobField, presets: &[f32], forward: bool) {
    let current = self.user_value_f32(field);
    let next = cycle_through(current, presets, forward);
    self.set_user_f32(field, next);
  }

  fn cycle_enum(&mut self, field: KnobField, forward: bool) {
    // Find the current user-set value inside the `&'static [&'static str]`
    // catalog so cycle_through's `T = &'static str` lifetime detaches
    // from `&self` — that lets us call `set_user_str(&mut self, ...)`
    // immediately after without dragging a borrow. Avoids the prior
    // `Vec<String>` + `Vec<&str>` allocation pair on every keypress.
    let current: Option<&'static str> = self
      .user_value_str(field)
      .and_then(|s| KV_CACHE_TYPES.iter().copied().find(|t| *t == s));
    let next = cycle_through(current, KV_CACHE_TYPES, forward);
    self.set_user_str(field, next.map(|s| s.to_string()));
  }

  fn cycle_bool(&mut self, field: KnobField, forward: bool) {
    // Tri-state: default ↔ on ↔ off (wrap).
    let current = self.user_value_bool(field);
    let next = if forward {
      match current {
        None => Some(true),
        Some(true) => Some(false),
        Some(false) => None,
      }
    } else {
      match current {
        None => Some(false),
        Some(false) => Some(true),
        Some(true) => None,
      }
    };
    self.set_user_bool(field, next);
  }

  fn cycle_device(&mut self, field: KnobField, forward: bool) {
    if self.cards.is_empty() {
      // No GPUs — fall back to default preset.
      let current: Option<&str> = self.user_value_str(field);
      let next = cycle_through(current, DEVICE_PRESETS, forward);
      self.set_user_str(field, next.map(str::to_string));
      self.selected_driver_offset = -1;
      return;
    }
    // Card-first: the current device value encodes "card_index:driver_offset"
    // or is empty (default / unselected).
    let current: Option<&str> = self.user_value_str(field);
    let (card_idx, driver_offset) = current.map(parse_device_selector).unwrap_or((0, 0));
    if forward {
      // Try cycling the driver first (within the current card).
      let card = self.cards.get(card_idx);
      if let Some(c) = card {
        let max_driver = (c.drivers.len() - 1) as i32;
        if max_driver > 0 {
          let next_driver = if driver_offset < max_driver {
            driver_offset + 1
          } else {
            -1
          };
          if next_driver >= 0 {
            // Advance driver within card.
            self.set_user_str(field, Some(encode_device_selector(card_idx, next_driver)));
            self.selected_driver_offset = next_driver;
            return;
          }
        }
      }
      // Driver exhausted — advance to next card.
      let next_card = card_idx + 1;
      if next_card < self.cards.len() {
        let next_card_ref = &self.cards[next_card];
        let driver = if !next_card_ref.drivers.is_empty() {
          0i32
        } else {
          -1
        };
        self.set_user_str(field, Some(encode_device_selector(next_card, driver)));
        self.selected_driver_offset = driver;
        return;
      }
      // All cards exhausted — wrap to first card, first driver.
      let first_card = &self.cards[0];
      let driver = if !first_card.drivers.is_empty() {
        0i32
      } else {
        -1
      };
      self.set_user_str(field, Some(encode_device_selector(0, driver)));
      self.selected_driver_offset = driver;
    } else {
      // Backward: try cycling driver first.
      let cur_card = self.cards.get(card_idx);
      if let Some(c) = cur_card {
        if !c.drivers.is_empty() && driver_offset > 0 {
          let next_driver = driver_offset - 1;
          self.set_user_str(field, Some(encode_device_selector(card_idx, next_driver)));
          self.selected_driver_offset = next_driver;
          return;
        }
      }
      // Driver at minimum — go to previous card, last driver.
      if card_idx > 0 {
        let prev_card = &self.cards[card_idx - 1];
        let max_driver = (prev_card.drivers.len().saturating_sub(1)) as i32;
        self.set_user_str(
          field,
          Some(encode_device_selector(card_idx - 1, max_driver)),
        );
        self.selected_driver_offset = max_driver;
        return;
      }
      // Wrapped to last card.
      let last_card = &self.cards[self.cards.len() - 1];
      let max_driver = (last_card.drivers.len().saturating_sub(1)) as i32;
      self.set_user_str(
        field,
        Some(encode_device_selector(self.cards.len() - 1, max_driver)),
      );
      self.selected_driver_offset = max_driver;
    }
  }

  /// Backspace on a focused row: clear the user override and re-
  /// inherit from the resolver chain.
  pub fn reset_focused_row(&mut self) {
    match self.field {
      PickerField::Knob(k) => self.clear_user(k),
      PickerField::Extras => {
        self.extras.clear();
      }
    }
  }

  /// Move cursor to the next row.
  pub fn next_field(&mut self) {
    let all = PickerField::all();
    if let Some(i) = all.iter().position(|f| *f == self.field) {
      self.field = all[(i + 1) % all.len()];
    }
  }

  pub fn prev_field(&mut self) {
    let all = PickerField::all();
    if let Some(i) = all.iter().position(|f| *f == self.field) {
      let n = all.len();
      self.field = all[(i + n - 1) % n];
    }
  }

  /// True when the focused row is cyclable (Up/Down would change
  /// the value). `Extras` is non-cyclable; the rest are.
  pub fn focused_field_is_cyclable(&self) -> bool {
    !matches!(self.field, PickerField::Extras)
  }

  /// Read the value the editor row should display, taking the user
  /// override first and the resolver-chain value otherwise.
  pub fn effective_u32(&self, field: KnobField) -> Option<u32> {
    self.user_value_u32(field).or(self.resolved_u32(field))
  }

  pub fn effective_f32(&self, field: KnobField) -> Option<f32> {
    self.user_value_f32(field).or(self.resolved_f32(field))
  }

  pub fn effective_str(&self, field: KnobField) -> Option<String> {
    self
      .user_value_str(field)
      .map(str::to_string)
      .or_else(|| self.resolved_str(field).map(str::to_string))
  }

  pub fn effective_bool(&self, field: KnobField) -> Option<bool> {
    self.user_value_bool(field).or(self.resolved_bool(field))
  }

  /// Source label for `field`. Returns `LayerLabel::User` when the
  /// user has an explicit override; falls back to the resolver's
  /// source map otherwise, then to the spec's `fallback_label` when
  /// the resolver hasn't populated the map yet (freshly-opened
  /// editor before the first resolve).
  pub fn source_for(&self, field: KnobField) -> LayerLabel {
    if self.user_has(field) {
      LayerLabel::User
    } else {
      self
        .sources
        .get(&field)
        .copied()
        .unwrap_or_else(|| crate::launch::flag_aliases::spec_for(field).fallback_label)
    }
  }

  fn user_has(&self, field: KnobField) -> bool {
    match field {
      KnobField::Ctx => self.user_knobs.ctx.is_some(),
      KnobField::Reasoning => self.user_knobs.reasoning.is_some(),
      KnobField::NGpuLayers => self.user_knobs.n_gpu_layers.is_some(),
      KnobField::Threads => self.user_knobs.threads.is_some(),
      KnobField::CacheTypeK => self.user_knobs.cache_type_k.is_some(),
      KnobField::CacheTypeV => self.user_knobs.cache_type_v.is_some(),
      KnobField::FlashAttn => self.user_knobs.flash_attn.is_some(),
      KnobField::Mlock => self.user_knobs.mlock.is_some(),
      KnobField::NoMmap => self.user_knobs.no_mmap.is_some(),
      KnobField::Parallel => self.user_knobs.parallel.is_some(),
      KnobField::BatchSize => self.user_knobs.batch_size.is_some(),
      KnobField::UbatchSize => self.user_knobs.ubatch_size.is_some(),
      KnobField::RopeFreqScale => self.user_knobs.rope_freq_scale.is_some(),
      KnobField::Keep => self.user_knobs.keep.is_some(),
      KnobField::Device => self.user_knobs.device.is_some(),
    }
  }

  fn user_value_u32(&self, field: KnobField) -> Option<u32> {
    match field {
      KnobField::Ctx => self.user_knobs.ctx,
      KnobField::NGpuLayers => self.user_knobs.n_gpu_layers,
      KnobField::Threads => self.user_knobs.threads,
      KnobField::Parallel => self.user_knobs.parallel,
      KnobField::BatchSize => self.user_knobs.batch_size,
      KnobField::UbatchSize => self.user_knobs.ubatch_size,
      KnobField::Keep => self.user_knobs.keep,
      _ => None,
    }
  }

  fn user_value_f32(&self, field: KnobField) -> Option<f32> {
    match field {
      KnobField::RopeFreqScale => self.user_knobs.rope_freq_scale,
      _ => None,
    }
  }

  fn user_value_str(&self, field: KnobField) -> Option<&str> {
    match field {
      KnobField::CacheTypeK => self.user_knobs.cache_type_k.as_deref(),
      KnobField::CacheTypeV => self.user_knobs.cache_type_v.as_deref(),
      KnobField::Device => self.user_knobs.device.as_deref(),
      _ => None,
    }
  }

  fn user_value_bool(&self, field: KnobField) -> Option<bool> {
    match field {
      KnobField::Reasoning => self.user_knobs.reasoning,
      KnobField::FlashAttn => self.user_knobs.flash_attn,
      KnobField::Mlock => self.user_knobs.mlock,
      KnobField::NoMmap => self.user_knobs.no_mmap,
      _ => None,
    }
  }

  fn resolved_u32(&self, field: KnobField) -> Option<u32> {
    match field {
      KnobField::Ctx => self.resolved.ctx,
      KnobField::NGpuLayers => self.resolved.n_gpu_layers,
      KnobField::Threads => self.resolved.threads,
      KnobField::Parallel => self.resolved.parallel,
      KnobField::BatchSize => self.resolved.batch_size,
      KnobField::UbatchSize => self.resolved.ubatch_size,
      KnobField::Keep => self.resolved.keep,
      _ => None,
    }
  }

  fn resolved_f32(&self, field: KnobField) -> Option<f32> {
    match field {
      KnobField::RopeFreqScale => self.resolved.rope_freq_scale,
      _ => None,
    }
  }

  fn resolved_str(&self, field: KnobField) -> Option<&str> {
    match field {
      KnobField::CacheTypeK => self.resolved.cache_type_k.as_deref(),
      KnobField::CacheTypeV => self.resolved.cache_type_v.as_deref(),
      KnobField::Device => self.resolved.device.as_deref(),
      _ => None,
    }
  }

  fn resolved_bool(&self, field: KnobField) -> Option<bool> {
    match field {
      KnobField::Reasoning => self.resolved.reasoning,
      KnobField::FlashAttn => self.resolved.flash_attn,
      KnobField::Mlock => self.resolved.mlock,
      KnobField::NoMmap => self.resolved.no_mmap,
      _ => None,
    }
  }

  pub fn set_user_u32(&mut self, field: KnobField, value: Option<u32>) {
    match field {
      KnobField::Ctx => self.user_knobs.ctx = value,
      KnobField::NGpuLayers => self.user_knobs.n_gpu_layers = value,
      KnobField::Threads => self.user_knobs.threads = value,
      KnobField::Parallel => self.user_knobs.parallel = value,
      KnobField::BatchSize => self.user_knobs.batch_size = value,
      KnobField::UbatchSize => self.user_knobs.ubatch_size = value,
      KnobField::Keep => self.user_knobs.keep = value,
      _ => {}
    }
  }

  pub fn set_user_f32(&mut self, field: KnobField, value: Option<f32>) {
    if matches!(field, KnobField::RopeFreqScale) {
      self.user_knobs.rope_freq_scale = value;
    }
  }

  pub fn set_user_str(&mut self, field: KnobField, value: Option<String>) {
    match field {
      KnobField::CacheTypeK => self.user_knobs.cache_type_k = value,
      KnobField::CacheTypeV => self.user_knobs.cache_type_v = value,
      KnobField::Device => self.user_knobs.device = value,
      _ => {}
    }
  }

  pub fn set_user_bool(&mut self, field: KnobField, value: Option<bool>) {
    match field {
      KnobField::Reasoning => self.user_knobs.reasoning = value,
      KnobField::FlashAttn => self.user_knobs.flash_attn = value,
      KnobField::Mlock => self.user_knobs.mlock = value,
      KnobField::NoMmap => self.user_knobs.no_mmap = value,
      _ => {}
    }
  }

  fn clear_user(&mut self, field: KnobField) {
    self.set_user_u32(field, None);
    self.set_user_f32(field, None);
    self.set_user_str(field, None);
    self.set_user_bool(field, None);
  }

  /// Display label for the device knob, including backend context
  /// (e.g. `"Nvidia0 (CUDA)"`). Returns `"default"` when no device
  /// is selected.
  pub fn device_value_display(&self) -> String {
    let sel = self
      .effective_str(KnobField::Device)
      .filter(|v| !v.is_empty());
    sel
      .map(|s| {
        let (card_idx, driver_offset) = parse_device_selector(&s);
        let card = self.cards.get(card_idx);
        match (card, driver_offset) {
          (Some(c), drv) if drv >= 0 && drv < (c.drivers.len() as i32) => {
            let driver = &c.drivers[drv as usize];
            format!("{} ({})", c.name, driver.label)
          }
          (Some(c), _) => c.name.to_string(),
          _ => s.to_string(),
        }
      })
      .unwrap_or_else(|| "default".into())
  }
}

/// Cycle through `presets` from `current`. Behaviour by case:
///
/// - **`current == None`** (row is on the inherited default): wrap to
///   the first preset (forward) or the last preset (backward).
/// - **`current` matches a preset exactly**: advance / reverse one
///   slot. Falling off either end wraps back to `None` so the row
///   re-inherits.
/// - **`current` sits between presets** (e.g. user typed a custom
///   value via `e`): snap to the nearest preset *in the chosen
///   direction* — pressing `→` jumps to the smallest preset strictly
///   greater than `current`; pressing `←` jumps to the largest one
///   strictly less. This keeps cycling consistent with the visible
///   direction of travel; the previous behaviour of jumping to
///   `presets[0]` was a footgun on custom values mid-list.
///
/// `presets` is assumed to be sorted in ascending order — every
/// caller in [`LaunchPickerState::cycle_knob`] passes a hand-curated
/// ascending list.
fn cycle_through<T: PartialEq + PartialOrd + Copy>(
  current: Option<T>,
  presets: &[T],
  forward: bool,
) -> Option<T> {
  if presets.is_empty() {
    return None;
  }
  match current {
    None => Some(if forward {
      presets[0]
    } else {
      presets[presets.len() - 1]
    }),
    Some(v) => {
      if let Some(i) = presets.iter().position(|p| *p == v) {
        return if forward {
          if i + 1 >= presets.len() {
            None
          } else {
            Some(presets[i + 1])
          }
        } else if i == 0 {
          None
        } else {
          Some(presets[i - 1])
        };
      }
      // Off-preset custom value: snap to the nearest preset in the
      // direction the user pressed. Falls back to first/last when
      // every preset sits on the other side of `current` (e.g. user
      // typed something smaller than `presets[0]` then pressed ←).
      if forward {
        presets
          .iter()
          .find(|p| **p > v)
          .copied()
          .or(Some(presets[presets.len() - 1]))
      } else {
        presets
          .iter()
          .rev()
          .find(|p| **p < v)
          .copied()
          .or(Some(presets[0]))
      }
    }
  }
}

#[cfg(test)]
mod tests {
  #![allow(clippy::useless_conversion)]
  use super::*;
  use crate::gpu::Driver;

  #[test]
  fn cycle_ctx_walks_through_presets_then_returns_to_native() {
    let mut s = LaunchPickerState::for_model("qwen");
    s.field = PickerField::Knob(KnobField::Ctx);
    assert_eq!(s.user_knobs.ctx, None);
    s.cycle_focused_value_next();
    assert_eq!(s.user_knobs.ctx, Some(CTX_PRESETS[0]));
    for preset in CTX_PRESETS.iter().skip(1) {
      s.cycle_focused_value_next();
      assert_eq!(s.user_knobs.ctx, Some(*preset));
    }
    s.cycle_focused_value_next();
    assert_eq!(s.user_knobs.ctx, None, "wraps back to native");
  }

  #[test]
  fn reasoning_cycle_walks_tri_state_in_both_directions() {
    let mut s = LaunchPickerState::for_model("qwen");
    s.field = PickerField::Knob(KnobField::Reasoning);
    s.cycle_focused_value_next();
    assert_eq!(s.user_knobs.reasoning, Some(true));
    s.cycle_focused_value_next();
    assert_eq!(s.user_knobs.reasoning, Some(false));
    s.cycle_focused_value_next();
    assert_eq!(s.user_knobs.reasoning, None);
  }

  #[test]
  fn next_field_iterates_every_picker_row() {
    let mut s = LaunchPickerState::for_model("qwen");
    let all = PickerField::all();
    assert!(
      all.len() > 14,
      "should cover ctx + reasoning + 12 knobs + extras"
    );
    for expected in all.iter().skip(1).chain(std::iter::once(&all[0])) {
      s.next_field();
      assert_eq!(s.field, *expected);
    }
  }

  #[test]
  fn cycle_knob_n_gpu_layers_walks_presets() {
    let mut s = LaunchPickerState::for_model("qwen");
    s.field = PickerField::Knob(KnobField::NGpuLayers);
    s.cycle_focused_value_next();
    assert_eq!(s.user_knobs.n_gpu_layers, Some(0));
    s.cycle_focused_value_next();
    assert_eq!(s.user_knobs.n_gpu_layers, Some(16));
  }

  #[test]
  fn cycle_knob_flash_attn_walks_tristate() {
    let mut s = LaunchPickerState::for_model("qwen");
    s.field = PickerField::Knob(KnobField::FlashAttn);
    s.cycle_focused_value_next();
    assert_eq!(s.user_knobs.flash_attn, Some(true));
    s.cycle_focused_value_next();
    assert_eq!(s.user_knobs.flash_attn, Some(false));
    s.cycle_focused_value_next();
    assert_eq!(s.user_knobs.flash_attn, None);
  }

  #[test]
  fn reset_focused_row_clears_user_override() {
    let mut s = LaunchPickerState::for_model("qwen");
    s.field = PickerField::Knob(KnobField::Threads);
    s.cycle_focused_value_next();
    assert!(s.user_knobs.threads.is_some());
    s.reset_focused_row();
    assert!(s.user_knobs.threads.is_none());
  }

  #[test]
  fn source_for_falls_through_to_resolver_when_no_user_override() {
    let mut s = LaunchPickerState::for_model("qwen");
    let mut sources = BTreeMap::new();
    sources.insert(KnobField::NGpuLayers, LayerLabel::ArchDefault);
    s.set_resolved(
      TypedKnobs {
        n_gpu_layers: Some(99),
        ..TypedKnobs::default()
      },
      sources,
    );
    assert_eq!(s.source_for(KnobField::NGpuLayers), LayerLabel::ArchDefault);
    // User override flips the source to User.
    s.user_knobs.n_gpu_layers = Some(32);
    assert_eq!(s.source_for(KnobField::NGpuLayers), LayerLabel::User);
  }

  #[test]
  fn cycle_through_starts_at_first_preset_when_current_is_none() {
    assert_eq!(cycle_through::<u32>(None, &[1, 2, 3], true), Some(1));
    assert_eq!(cycle_through::<u32>(None, &[1, 2, 3], false), Some(3));
  }

  #[test]
  fn cycle_through_wraps_to_none_at_the_end() {
    assert_eq!(cycle_through::<u32>(Some(3), &[1, 2, 3], true), None);
    assert_eq!(cycle_through::<u32>(Some(1), &[1, 2, 3], false), None);
  }

  #[test]
  fn cycle_through_off_preset_snaps_to_nearest_in_direction() {
    // User typed `n_gpu_layers=42` via `e`, then presses →.
    let presets = &[0, 16, 32, 64, 99];
    assert_eq!(cycle_through(Some(42_u32), presets, true), Some(64));
    assert_eq!(cycle_through(Some(42_u32), presets, false), Some(32));
  }

  #[test]
  fn cycle_through_off_preset_below_first_snaps_to_first_going_forward() {
    let presets = &[10, 20, 30];
    // Forward from a value below presets[0] → first preset > current = 10.
    assert_eq!(cycle_through(Some(5_u32), presets, true), Some(10));
    // Backward from below presets[0] has nothing smaller → fall back
    // to first preset.
    assert_eq!(cycle_through(Some(5_u32), presets, false), Some(10));
  }

  #[test]
  fn cycle_through_off_preset_above_last_snaps_to_last_going_backward() {
    let presets = &[10, 20, 30];
    assert_eq!(cycle_through(Some(99_u32), presets, false), Some(30));
    // Forward from above presets[last] has nothing greater → fall back
    // to last preset.
    assert_eq!(cycle_through(Some(99_u32), presets, true), Some(30));
  }

  // ---- Card-first device picker tests ----

  #[test]
  fn encode_decode_roundtrips_card_and_driver() {
    assert_eq!(encode_device_selector(0, 0), "0:0");
    assert_eq!(encode_device_selector(3, -1), "3:-1");
    assert_eq!(encode_device_selector(10, 5), "10:5");
    assert_eq!(parse_device_selector("0:0"), (0, 0));
    assert_eq!(parse_device_selector("3:-1"), (3, -1));
    assert_eq!(parse_device_selector("10:5"), (10, 5));
  }

  #[test]
  fn parse_device_selector_handles_empty_and_edge_cases() {
    assert_eq!(parse_device_selector(""), (0, 0));
    assert_eq!(parse_device_selector("5"), (5, -1));
    assert_eq!(parse_device_selector("abc"), (0, -1));
  }

  #[test]
  fn device_value_display_single_card_no_driver() {
    let mut s = LaunchPickerState::for_model("test");
    s.cards = vec![Card {
      id: "PCI-0000:01:00.0".into(),
      name: "NVIDIA GeForce RTX 3080".into(),
      total_memory_bytes: 10_737_418_240,
      drivers: vec![Driver {
        backend: "nvidia".into(),
        label: "CUDA".into(),
        index: 0,
        selector: "Nvidia0".into(),
        utilization_pct: None,
        temperature_c: None,
        used_memory_bytes: None,
      }],
    }];
    // No selection → default.
    assert_eq!(s.device_value_display(), "default");
    // Select card 0, driver -1 (no driver needed).
    s.set_user_str(KnobField::Device, Some("0:-1".into()));
    assert_eq!(s.device_value_display(), "NVIDIA GeForce RTX 3080");
  }

  #[test]
  fn device_value_display_card_with_multiple_drivers() {
    let mut s = LaunchPickerState::for_model("test");
    s.cards = vec![
      Card {
        id: "PCI-0000:01:00.0".into(),
        name: "AMD Radeon RX 6800".into(),
        total_memory_bytes: 16_106_127_360,
        drivers: vec![
          Driver {
            backend: "amd".into(),
            label: "ROCm".into(),
            index: 0,
            selector: "Amd0".into(),
            utilization_pct: None,
            temperature_c: None,
            used_memory_bytes: None,
          },
          Driver {
            backend: "unknown".into(),
            label: "Vulkan".into(),
            index: 1,
            selector: "Vulkan0".into(),
            utilization_pct: None,
            temperature_c: None,
            used_memory_bytes: None,
          },
        ],
      },
      Card {
        id: "PCI-0000:02:00.0".into(),
        name: "NVIDIA GeForce RTX 3090".into(),
        total_memory_bytes: 24_696_061_500,
        drivers: vec![
          Driver {
            backend: "nvidia".into(),
            label: "CUDA".into(),
            index: 0,
            selector: "Nvidia0".into(),
            utilization_pct: None,
            temperature_c: None,
            used_memory_bytes: None,
          },
          Driver {
            backend: "unknown".into(),
            label: "Vulkan".into(),
            index: 1,
            selector: "Vulkan0".into(),
            utilization_pct: None,
            temperature_c: None,
            used_memory_bytes: None,
          },
        ],
      },
    ];
    // Select card 0, driver 0 → card name + driver label.
    s.set_user_str(KnobField::Device, Some("0:0".into()));
    assert_eq!(s.device_value_display(), "AMD Radeon RX 6800 (ROCm)");
    // Select card 0, driver 1.
    s.set_user_str(KnobField::Device, Some("0:1".into()));
    assert_eq!(s.device_value_display(), "AMD Radeon RX 6800 (Vulkan)");
    // Select card 1, driver 0.
    s.set_user_str(KnobField::Device, Some("1:0".into()));
    assert_eq!(s.device_value_display(), "NVIDIA GeForce RTX 3090 (CUDA)");
  }

  #[test]
  fn cycle_device_forward_with_single_card_single_driver() {
    let mut s = LaunchPickerState::for_model("test");
    s.cards = vec![Card {
      id: "apple-metal".into(),
      name: "Apple Silicon (unified)".into(),
      total_memory_bytes: 16_106_127_360,
      drivers: vec![Driver {
        backend: "apple_metal".into(),
        label: "Metal".into(),
        index: 0,
        selector: "Metal0".into(),
        utilization_pct: None,
        temperature_c: None,
        used_memory_bytes: None,
      }],
    }];
    // Start: no selection.
    assert_eq!(s.user_value_str(KnobField::Device), None);
    // Forward: selects card 0, driver 0 (first forward picks card 0, driver 0).
    s.cycle_device(KnobField::Device, true);
    assert_eq!(s.user_value_str(KnobField::Device), Some("0:0".into()));
    assert_eq!(s.selected_driver_offset, 0);
    // Forward again: wraps to card 0, driver 0 (single card, single driver).
    s.cycle_device(KnobField::Device, true);
    assert_eq!(s.user_value_str(KnobField::Device), Some("0:0".into()));
    assert_eq!(s.selected_driver_offset, 0);
  }

  #[test]
  fn cycle_device_forward_with_multi_card_multi_driver() {
    let mut s = LaunchPickerState::for_model("test");
    s.cards = vec![
      Card {
        id: "PCI-0000:01:00.0".into(),
        name: "NVIDIA GeForce RTX 3080".into(),
        total_memory_bytes: 10_737_418_240,
        drivers: vec![
          Driver {
            backend: "nvidia".into(),
            label: "CUDA".into(),
            index: 0,
            selector: "Nvidia0".into(),
            utilization_pct: None,
            temperature_c: None,
            used_memory_bytes: None,
          },
          Driver {
            backend: "unknown".into(),
            label: "Vulkan".into(),
            index: 1,
            selector: "Vulkan0".into(),
            utilization_pct: None,
            temperature_c: None,
            used_memory_bytes: None,
          },
        ],
      },
      Card {
        id: "PCI-0000:02:00.0".into(),
        name: "AMD Radeon RX 6800".into(),
        total_memory_bytes: 16_106_127_360,
        drivers: vec![Driver {
          backend: "amd".into(),
          label: "ROCm".into(),
          index: 0,
          selector: "Amd0".into(),
          utilization_pct: None,
          temperature_c: None,
          used_memory_bytes: None,
        }],
      },
    ];
    // First forward: cycles to card 0, driver 1 (first forward advances driver).
    s.cycle_device(KnobField::Device, true);
    assert_eq!(s.user_value_str(KnobField::Device), Some("0:1".into()));
    // Second forward: card 1, driver 0 (driver exhausted, advance card).
    s.cycle_device(KnobField::Device, true);
    assert_eq!(s.user_value_str(KnobField::Device), Some("1:0".into()));
    // Third forward: wraps to card 0, driver 0.
    s.cycle_device(KnobField::Device, true);
    assert_eq!(s.user_value_str(KnobField::Device), Some("0:0".into()));
  }

  #[test]
  fn cycle_device_backward_with_multi_card() {
    let mut s = LaunchPickerState::for_model("test");
    s.cards = vec![
      Card {
        id: "PCI-0000:01:00.0".into(),
        name: "NVIDIA RTX 3080".into(),
        total_memory_bytes: 10_737_418_240,
        drivers: vec![Driver {
          backend: "nvidia".into(),
          label: "CUDA".into(),
          index: 0,
          selector: "Nvidia0".into(),
          utilization_pct: None,
          temperature_c: None,
          used_memory_bytes: None,
        }],
      },
      Card {
        id: "PCI-0000:02:00.0".into(),
        name: "AMD RX 6800".into(),
        total_memory_bytes: 16_106_127_360,
        drivers: vec![Driver {
          backend: "amd".into(),
          label: "ROCm".into(),
          index: 0,
          selector: "Amd0".into(),
          utilization_pct: None,
          temperature_c: None,
          used_memory_bytes: None,
        }],
      },
    ];
    // First forward: card 0 → card 1 (card 0 wraps to card 1).
    s.cycle_device(KnobField::Device, true);
    assert_eq!(s.user_value_str(KnobField::Device), Some("1:0".into()));
    // Second forward: card 1 wraps to card 0 (no more cards).
    s.cycle_device(KnobField::Device, true);
    assert_eq!(s.user_value_str(KnobField::Device), Some("0:0".into()));
    // Backward: wraps to card 1 (last card — no prev card from card 0).
    s.cycle_device(KnobField::Device, false);
    assert_eq!(s.user_value_str(KnobField::Device), Some("1:0".into()));
    // Backward: goes to card 0 (prev card, last driver).
    s.cycle_device(KnobField::Device, false);
    assert_eq!(s.user_value_str(KnobField::Device), Some("0:0".into()));
  }

  #[test]
  fn cycle_device_with_no_cards_falls_back_to_default() {
    let mut s = LaunchPickerState::for_model("test");
    // cards is empty — should fall through to DEVICE_PRESETS.
    s.cycle_device(KnobField::Device, true);
    assert_eq!(s.selected_driver_offset, -1);
    s.cycle_device(KnobField::Device, false);
    assert_eq!(s.selected_driver_offset, -1);
  }
}
