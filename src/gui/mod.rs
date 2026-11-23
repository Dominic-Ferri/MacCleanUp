mod state;
pub mod theme;
mod treemap;

use eframe::egui::{
    self, Align, Align2, Color32, CornerRadius, Id, Key, Layout, Margin, Modifiers, Pos2, Rect,
    RichText, Sense, Stroke, StrokeKind, Ui, Vec2,
};
use egui_extras::{Column, TableBuilder};
use maccleanup::model::{format_bytes, AppItem, CleanItem};
use maccleanup::scanner::is_protected_disk_path;
use maccleanup::trash::related_app_data;
use state::{age, plural, tilde_path, Section, SortKey, State};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;
use theme::{
    condensed, medium, BEDROCK, CHALK, FLARE, HEADLAMP, ORE, RIDGE, SEAM, SHALE, SILT, STRATA,
};

pub struct MacCleanUp {
    state: State,
    /// Set when a dialog opens so Cancel receives keyboard focus once.
    focus_cancel: bool,
    initialized: bool,
}

impl MacCleanUp {
    pub fn new(home: PathBuf) -> Self {
        Self {
            state: State::new(home),
            focus_cancel: false,
            initialized: false,
        }
    }

    /// Draws one frame. Separate from `eframe::App` so tests can drive it.
    pub fn show(&mut self, ui: &mut Ui) {
        let ctx = ui.ctx().clone();
        if !self.initialized {
            theme::install(&ctx);
            let waker = ctx.clone();
            self.state
                .set_waker(Arc::new(move || waker.request_repaint()));
            self.initialized = true;
        }
        if let Some(delay) = self.state.update() {
            ctx.request_repaint_after(delay);
        }
        self.shortcuts(&ctx);

        egui::Panel::left("navigation")
            .exact_size(214.0)
            .resizable(false)
            .frame(
                egui::Frame::new()
                    .fill(SEAM)
                    .inner_margin(Margin::symmetric(14, 18)),
            )
            .show(ui, |ui| self.navigation(ui));
        egui::Panel::bottom("status_bar")
            .exact_size(34.0)
            .frame(
                egui::Frame::new()
                    .fill(BEDROCK)
                    .stroke(Stroke::new(1.0, SHALE))
                    .inner_margin(Margin::symmetric(18, 6)),
            )
            .show(ui, |ui| self.status_bar(ui));
        if has_inspector(self.state.section) {
            egui::Panel::right("inspector")
                .exact_size(316.0)
                .resizable(false)
                .frame(
                    egui::Frame::new()
                        .fill(SEAM)
                        .inner_margin(Margin::symmetric(18, 22)),
                )
                .show(ui, |ui| self.inspector(ui));
        }
        egui::CentralPanel::no_frame()
            .frame(
                egui::Frame::new()
                    .fill(BEDROCK)
                    .inner_margin(Margin::symmetric(28, 22)),
            )
            .show(ui, |ui| match self.state.section {
                Section::Overview => self.overview(ui),
                Section::Clean | Section::Installers | Section::Applications => self.list_page(ui),
                Section::Disk => self.disk_page(ui),
                Section::Status => self.status_page(ui),
                Section::Activity => self.activity_page(ui),
            });

        self.dialogs(&ctx);
    }

    fn shortcuts(&mut self, ctx: &egui::Context) {
        if self.state.confirmation.is_some() || self.state.notice.is_some() {
            return;
        }
        let typing = ctx.memory(|memory| memory.focused().is_some());
        ctx.input_mut(|input| {
            for (index, key) in [
                Key::Num1,
                Key::Num2,
                Key::Num3,
                Key::Num4,
                Key::Num5,
                Key::Num6,
                Key::Num7,
            ]
            .into_iter()
            .enumerate()
            {
                if input.consume_key(Modifiers::COMMAND, key) {
                    self.state.select_section(Section::ALL[index]);
                }
            }
            if input.consume_key(Modifiers::COMMAND, Key::R) {
                self.state.scan(self.state.section);
            }
            if input.consume_key(Modifiers::NONE, Key::Escape) {
                self.state.cancel_scan();
            }
        });
        if typing || !is_list(self.state.section) {
            return;
        }
        let order = self.visible_order();
        if order.is_empty() {
            return;
        }
        let position = self
            .state
            .focus_index()
            .and_then(|focus| order.iter().position(|&index| index == focus));
        let (down, up, space) = ctx.input_mut(|input| {
            (
                input.consume_key(Modifiers::NONE, Key::ArrowDown),
                input.consume_key(Modifiers::NONE, Key::ArrowUp),
                input.consume_key(Modifiers::NONE, Key::Space),
            )
        });
        if down {
            let next = position.map_or(0, |position| (position + 1).min(order.len() - 1));
            self.state.set_focus(order[next]);
        }
        if up {
            let next = position.map_or(0, |position| position.saturating_sub(1));
            self.state.set_focus(order[next]);
        }
        if space {
            if let Some(focus) = self.state.focus_index() {
                self.state.toggle(focus);
            }
        }
    }

    // Navigation ------------------------------------------------------------

    fn navigation(&mut self, ui: &mut Ui) {
        ui.label(
            RichText::new("MacCleanUp")
                .font(condensed(24.0))
                .color(CHALK),
        );
        ui.label(
            RichText::new(concat!("Version ", env!("CARGO_PKG_VERSION")))
                .small()
                .color(SILT),
        );
        ui.add_space(22.0);
        for section in Section::ALL {
            if section == Section::Status {
                ui.add_space(10.0);
                ui.separator();
                ui.add_space(6.0);
            }
            let hint = self.nav_hint(section);
            if nav_row(ui, section.title(), &hint, self.state.section == section).clicked() {
                self.state.select_section(section);
            }
        }
        ui.with_layout(Layout::bottom_up(Align::Min), |ui| {
            ui.label(
                RichText::new("Items move to Trash only after you review them. Nothing is erased.")
                    .small()
                    .color(SILT),
            );
        });
    }

    fn nav_hint(&self, section: Section) -> String {
        let state = &self.state;
        let total = |items: &[CleanItem]| format_bytes(items.iter().map(|item| item.bytes).sum());
        match section {
            Section::Clean if state.is_loaded(section) => total(&state.clean_items),
            Section::Installers if state.is_loaded(section) => total(&state.installer_items),
            Section::Applications if state.is_loaded(section) => state.app_items.len().to_string(),
            Section::Activity if !state.activity.is_empty() => state.activity.len().to_string(),
            _ => String::new(),
        }
    }

    // Overview --------------------------------------------------------------

    fn overview(&mut self, ui: &mut Ui) {
        page_heading(ui, Section::Overview);
        let state = &self.state;
        let clean: u64 = state.clean_items.iter().map(|item| item.bytes).sum();
        let installers: u64 = state.installer_items.iter().map(|item| item.bytes).sum();
        let apps: u64 = state.app_items.iter().map(|item| item.bytes).sum();

        if let Some(disk) = state.status.as_ref().and_then(|status| status.disk) {
            let used = disk.total.saturating_sub(disk.available);
            let measured = [
                ("Applications", apps, state.is_loaded(Section::Applications)),
                ("Caches and logs", clean, state.is_loaded(Section::Clean)),
                (
                    "Installer files",
                    installers,
                    state.is_loaded(Section::Installers),
                ),
            ];
            let known: u64 = measured.iter().map(|(_, bytes, _)| *bytes).sum();
            let mut strata: Vec<(String, u64, Color32)> = measured
                .iter()
                .enumerate()
                .filter(|(_, (_, bytes, loaded))| *loaded && *bytes > 0)
                .map(|(index, (label, bytes, _))| (label.to_string(), *bytes, STRATA[index]))
                .collect();
            strata.push((
                "Everything else in use (not measured)".to_string(),
                used.saturating_sub(known),
                RIDGE,
            ));
            strata.push((
                "Free".to_string(),
                disk.available,
                SHALE.gamma_multiply(0.6),
            ));

            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(format_bytes(disk.available))
                        .font(condensed(44.0))
                        .color(CHALK),
                );
                ui.add_space(6.0);
                ui.vertical(|ui| {
                    ui.add_space(12.0);
                    ui.label(RichText::new("free on your startup disk").color(SILT));
                    ui.label(
                        RichText::new(format!(
                            "{} used of {}",
                            format_bytes(used),
                            format_bytes(disk.total)
                        ))
                        .small()
                        .color(SILT),
                    );
                });
            });
            ui.add_space(10.0);
            strata_bar(ui, &strata, disk.total);
            ui.add_space(10.0);
            for (label, bytes, color) in &strata {
                legend_row(ui, *color, label, *bytes, disk.total);
            }
        }

        ui.add_space(26.0);
        ui.label(
            RichText::new("Ready to review")
                .font(condensed(20.0))
                .color(CHALK),
        );
        ui.add_space(4.0);
        let rows = [
            (
                Section::Clean,
                if state.is_loaded(Section::Clean) {
                    format!(
                        "{} using {}. Copies and records apps keep for themselves; safe to remove.",
                        plural(state.clean_items.len(), "item", "items"),
                        format_bytes(clean)
                    )
                } else {
                    "Copies and records apps keep for themselves. Not scanned yet.".to_string()
                },
                "Review caches",
            ),
            (
                Section::Installers,
                if state.is_loaded(Section::Installers) {
                    format!(
                        "{} found, {}",
                        plural(state.installer_items.len(), "file", "files"),
                        format_bytes(installers)
                    )
                } else {
                    "Reads Downloads, Desktop, and Documents. macOS may ask for access.".to_string()
                },
                if state.is_loaded(Section::Installers) {
                    "Review installers"
                } else {
                    "Find installers"
                },
            ),
            (
                Section::Applications,
                if state.is_loaded(Section::Applications) {
                    format!(
                        "{} installed, {}",
                        plural(state.app_items.len(), "app", "apps"),
                        format_bytes(apps)
                    )
                } else {
                    "Not measured yet".to_string()
                },
                "Review apps",
            ),
            (
                Section::Disk,
                "See which folders in your home folder are largest.".to_string(),
                "Open disk map",
            ),
        ];
        let mut go = None;
        for (section, detail, action) in rows {
            ui.separator();
            ui.horizontal(|ui| {
                ui.set_min_height(44.0);
                ui.vertical(|ui| {
                    ui.add_space(4.0);
                    ui.label(RichText::new(section.title()).font(medium(15.0)));
                    ui.label(RichText::new(detail).small().color(SILT));
                });
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui.button(action).clicked() {
                        go = Some(section);
                    }
                });
            });
        }
        if let Some(section) = go {
            self.state.select_section(section);
            if section == Section::Installers && !self.state.is_loaded(section) {
                self.state.scan(section);
            }
        }
    }

    // Lists: caches, installers, applications --------------------------------

    fn visible_order(&self) -> Vec<usize> {
        let state = &self.state;
        let section = state.section;
        let filter = state.filter[section.index()].to_lowercase();
        let sort = state.sort[section.index()];
        let mut rows: Vec<(usize, String, u64, Option<SystemTime>, String)> = match section {
            Section::Clean => clean_rows(&state.clean_items, &state.app_items, &state.home),
            Section::Installers => {
                clean_rows(&state.installer_items, &state.app_items, &state.home)
            }
            Section::Applications => state
                .app_items
                .iter()
                .enumerate()
                .map(|(index, app)| {
                    (
                        index,
                        app.name.clone(),
                        app.bytes,
                        app.modified,
                        app.bundle_id.clone().unwrap_or_default(),
                    )
                })
                .collect(),
            _ => Vec::new(),
        };
        rows.retain(|(_, name, _, _, extra)| {
            filter.is_empty()
                || name.to_lowercase().contains(&filter)
                || extra.to_lowercase().contains(&filter)
        });
        match sort {
            SortKey::Size => rows.sort_by(|a, b| b.2.cmp(&a.2).then_with(|| a.1.cmp(&b.1))),
            SortKey::Name => rows.sort_by_key(|row| row.1.to_lowercase()),
            SortKey::Modified => rows.sort_by_key(|row| std::cmp::Reverse(row.3)),
        }
        rows.into_iter().map(|row| row.0).collect()
    }

    fn list_page(&mut self, ui: &mut Ui) {
        let section = self.state.section;
        page_heading(ui, section);
        if section == Section::Clean {
            cache_explainer(ui);
        }
        let loaded = self.state.is_loaded(section);
        let busy = !self.state.can_start();

        // Toolbar: filter on the left, scan and the Trash action on the right.
        ui.horizontal(|ui| {
            let filter = &mut self.state.filter[section.index()];
            ui.add(
                egui::TextEdit::singleline(filter)
                    .hint_text("Filter by name")
                    .desired_width(220.0),
            )
            .on_hover_text("Filter by name");
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let (count, bytes) = self.state.selected_total(section);
                let action = if section == Section::Applications {
                    format!("Uninstall {}…", plural(count, "app", "apps"))
                } else {
                    format!("Move {} to Trash…", plural(count, "item", "items"))
                };
                let trash = egui::Button::new(RichText::new(action).color(if count > 0 {
                    BEDROCK
                } else {
                    SILT
                }))
                .fill(if count > 0 { ORE } else { SHALE });
                if ui.add_enabled(count > 0 && !busy, trash).clicked() {
                    self.state.review();
                    self.focus_cancel = true;
                }
                if count > 0 {
                    ui.label(RichText::new(format_bytes(bytes)).color(SILT));
                }
                let scan_label = if loaded { "Scan again" } else { "Scan" };
                if ui
                    .add_enabled(!busy, egui::Button::new(scan_label))
                    .clicked()
                {
                    self.state.scan(section);
                }
            });
        });
        ui.add_space(10.0);

        if !loaded {
            empty_state(
                ui,
                "Not scanned yet",
                match section {
                    Section::Installers => "Scan looks in Downloads, Desktop, Documents, Public, and iCloud Downloads. macOS may ask MacCleanUp for access to those folders the first time.",
                    _ => "Scan to see what is here.",
                },
            );
            return;
        }
        let order = self.visible_order();
        if order.is_empty() {
            let empty = if self.state.filter[section.index()].is_empty() {
                match section {
                    Section::Installers => {
                        "No installer files were found in the usual download folders."
                    }
                    Section::Applications => {
                        "No apps were found in /Applications or ~/Applications."
                    }
                    _ => "Nothing to clear here right now.",
                }
            } else {
                "No items match the filter. Clear it to see everything."
            };
            empty_state(ui, "Nothing to show", empty);
            return;
        }

        let mut toggle = None;
        let mut focus = None;
        let mut toggle_all = false;
        let mut sort = None;
        let focused = self.state.focus_index();
        let all_selected = order.iter().all(|&index| self.is_selected(index));
        let third = match section {
            Section::Applications => "Version",
            Section::Installers => "Folder",
            _ => "Type",
        };
        let row_height = 30.0;
        TableBuilder::new(ui)
            .id_salt(section.index())
            .striped(false)
            .sense(Sense::click())
            .cell_layout(Layout::left_to_right(Align::Center))
            .column(Column::exact(28.0))
            .column(Column::remainder().at_least(160.0).clip(true))
            .column(Column::exact(150.0).clip(true))
            .column(Column::exact(86.0))
            .column(Column::exact(104.0))
            .header(28.0, |mut header| {
                header.col(|ui| {
                    let mut all = all_selected;
                    if ui
                        .checkbox(&mut all, "")
                        .on_hover_text("Select or clear all shown")
                        .clicked()
                    {
                        toggle_all = true;
                    }
                });
                for (label, key) in [
                    ("Name", Some(SortKey::Name)),
                    (third, None),
                    ("Size", Some(SortKey::Size)),
                    ("Changed", Some(SortKey::Modified)),
                ] {
                    header.col(|ui| match key {
                        Some(key) => {
                            let active = self.state.sort[section.index()] == key;
                            let text = RichText::new(label).font(medium(12.5)).color(if active {
                                HEADLAMP
                            } else {
                                SILT
                            });
                            if ui
                                .add(egui::Button::new(text).frame(false))
                                .on_hover_text(format!("Sort by {}", label.to_lowercase()))
                                .clicked()
                            {
                                sort = Some(key);
                            }
                        }
                        None => {
                            ui.label(RichText::new(label).font(medium(12.5)).color(SILT));
                        }
                    });
                }
            })
            .body(|body| {
                body.rows(row_height, order.len(), |mut row| {
                    let index = order[row.index()];
                    row.set_selected(focused == Some(index));
                    let (name, third_text, bytes, modified, complete) = self.row_text(index);
                    let mut selected = self.is_selected(index);
                    row.col(|ui| {
                        if ui
                            .checkbox(&mut selected, "")
                            .on_hover_text(format!("Select {name}"))
                            .clicked()
                        {
                            toggle = Some(index);
                        }
                    });
                    row.col(|ui| {
                        ui.label(RichText::new(&name).color(CHALK));
                    });
                    row.col(|ui| {
                        ui.label(RichText::new(third_text).color(SILT));
                    });
                    row.col(|ui| {
                        let text = RichText::new(format_bytes(bytes))
                            .font(medium(13.0))
                            .color(if complete { CHALK } else { ORE });
                        ui.label(text).on_hover_text(if complete {
                            "Measured in full"
                        } else {
                            "Some files could not be read, so the size may be larger"
                        });
                    });
                    row.col(|ui| {
                        ui.label(RichText::new(age(modified)).color(SILT));
                    });
                    if row.response().clicked() {
                        focus = Some(index);
                    }
                });
            });

        if let Some(index) = toggle {
            self.state.toggle(index);
            self.state.set_focus(index);
        }
        if let Some(index) = focus {
            self.state.set_focus(index);
        }
        if toggle_all {
            self.state.toggle_all(&order);
        }
        if let Some(key) = sort {
            self.state.sort[section.index()] = key;
        }
    }

    fn is_selected(&self, index: usize) -> bool {
        match self.state.section {
            Section::Clean => self
                .state
                .clean_items
                .get(index)
                .is_some_and(|item| item.selected),
            Section::Installers => self
                .state
                .installer_items
                .get(index)
                .is_some_and(|item| item.selected),
            Section::Applications => self
                .state
                .app_items
                .get(index)
                .is_some_and(|app| app.selected),
            _ => false,
        }
    }

    fn row_text(&self, index: usize) -> (String, String, u64, Option<SystemTime>, bool) {
        let state = &self.state;
        match state.section {
            Section::Applications => {
                let app = &state.app_items[index];
                (
                    app.name.clone(),
                    app.version.clone().unwrap_or_else(|| "Unknown".to_string()),
                    app.bytes,
                    app.modified,
                    true,
                )
            }
            Section::Installers => {
                let item = &state.installer_items[index];
                let folder = item
                    .path
                    .parent()
                    .map(|parent| tilde_path(parent, &state.home))
                    .unwrap_or_default();
                (
                    item.name.clone(),
                    folder,
                    item.bytes,
                    item.modified,
                    item.complete,
                )
            }
            _ => {
                let item = &state.clean_items[index];
                (
                    friendly_name(&item.name, &state.app_items),
                    item.category.label().to_string(),
                    item.bytes,
                    item.modified,
                    item.complete,
                )
            }
        }
    }

    // Disk map --------------------------------------------------------------

    fn disk_page(&mut self, ui: &mut Ui) {
        page_heading(ui, Section::Disk);
        let busy = !self.state.can_start();
        ui.horizontal(|ui| {
            if ui
                .add_enabled(
                    !busy && self.state.disk_root.parent().is_some(),
                    egui::Button::new("Up"),
                )
                .on_hover_text("Open the enclosing folder")
                .clicked()
            {
                self.state.open_parent();
            }
            let field = ui.add(
                egui::TextEdit::singleline(&mut self.state.path_draft)
                    .hint_text("Folder path, for example ~/Downloads")
                    .desired_width(ui.available_width() - 190.0),
            );
            let field = field.on_hover_text("Folder to measure");
            let submitted = field.lost_focus() && ui.input(|input| input.key_pressed(Key::Enter));
            if ui.add_enabled(!busy, egui::Button::new("Open")).clicked() || (submitted && !busy) {
                self.state.open_typed_folder();
            }
            let label = if self.state.is_loaded(Section::Disk) {
                "Measure again"
            } else {
                "Measure"
            };
            if ui.add_enabled(!busy, egui::Button::new(label)).clicked() {
                self.state.scan(Section::Disk);
            }
        });
        ui.add_space(6.0);
        self.breadcrumbs(ui);
        ui.add_space(10.0);

        if !self.state.is_loaded(Section::Disk) {
            empty_state(
                ui,
                "Not measured yet",
                "Measure sizes every item in this folder. Large folders take a while, and macOS may ask for access to protected folders.",
            );
            return;
        }
        if self.state.disk_items.is_empty() {
            let text = if busy {
                "Measuring. Large folders take a while; press Esc to stop."
            } else {
                "This folder is empty, or it could not be read."
            };
            empty_state(ui, "No items to show", text);
            return;
        }

        let map_height = (ui.available_height() * 0.58).max(180.0);
        let (rect, response) =
            ui.allocate_exact_size(Vec2::new(ui.available_width(), map_height), Sense::click());
        let items = &self.state.disk_items;
        let weights: Vec<u64> = items.iter().map(|item| item.bytes).collect();
        let tiles = treemap::layout(&weights, rect.shrink(1.0));
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, CornerRadius::same(6), SEAM);
        let focused = self.state.focus_index();
        let hovered = response
            .hover_pos()
            .and_then(|pos| tiles.iter().position(|tile| tile.contains(pos)));
        for (index, (tile, item)) in tiles.iter().zip(items).enumerate() {
            if !tile.is_positive() || tile.width() < 2.0 || tile.height() < 2.0 {
                continue;
            }
            let base = STRATA[index % STRATA.len()];
            let fill = if hovered == Some(index) {
                base.gamma_multiply(1.25)
            } else {
                base
            };
            let inner = tile.shrink(1.0);
            painter.rect_filled(inner, CornerRadius::same(3), fill);
            if tile.width() > 64.0 && tile.height() > 34.0 {
                let label_rect = inner.shrink2(Vec2::new(8.0, 6.0));
                let name_galley = painter.layout_no_wrap(
                    elide(&item.name, label_rect.width(), 8.0),
                    medium(13.0),
                    CHALK,
                );
                painter.galley(label_rect.min, name_galley, CHALK);
                painter.text(
                    label_rect.min + Vec2::new(0.0, 17.0),
                    Align2::LEFT_TOP,
                    format_bytes(item.bytes),
                    egui::FontId::proportional(12.0),
                    CHALK.gamma_multiply(0.75),
                );
            }
        }
        if let Some(index) = focused {
            if let Some(tile) = tiles.get(index).filter(|tile| tile.is_positive()) {
                painter.rect_stroke(
                    tile.shrink(1.0),
                    CornerRadius::same(3),
                    Stroke::new(2.0, HEADLAMP),
                    StrokeKind::Inside,
                );
            }
        }
        if let Some(index) = hovered {
            let item = &items[index];
            response.clone().on_hover_text(format!(
                "{}\n{}{}",
                item.name,
                format_bytes(item.bytes),
                if item.is_directory {
                    "\nDouble-click to open"
                } else {
                    ""
                }
            ));
        }
        let mut open = None;
        let mut clicked_tile = None;
        if let Some(index) = hovered {
            if response.clicked() {
                clicked_tile = Some(index);
            }
            if response.double_clicked() && items[index].is_directory && !busy {
                open = Some(items[index].path.clone());
            }
        }
        if let Some(index) = clicked_tile {
            self.state.set_focus(index);
        }
        if let Some(path) = open {
            self.state.open_folder(path);
            return;
        }

        ui.add_space(12.0);
        let total: u64 = self.state.disk_items.iter().map(|item| item.bytes).sum();
        let focused = self.state.focus_index();
        let mut clicked = None;
        let mut opened = None;
        TableBuilder::new(ui)
            .id_salt("disk")
            .sense(Sense::click())
            .cell_layout(Layout::left_to_right(Align::Center))
            .column(Column::exact(14.0))
            .column(Column::remainder().at_least(160.0).clip(true))
            .column(Column::exact(86.0))
            .column(Column::exact(64.0))
            .column(Column::exact(104.0))
            .header(26.0, |mut header| {
                for label in ["", "Name", "Size", "Share", "Changed"] {
                    header.col(|ui| {
                        ui.label(RichText::new(label).font(medium(12.5)).color(SILT));
                    });
                }
            })
            .body(|body| {
                body.rows(28.0, self.state.disk_items.len(), |mut row| {
                    let index = row.index();
                    let item = &self.state.disk_items[index];
                    row.set_selected(focused == Some(index));
                    row.col(|ui| {
                        let (rect, _) = ui.allocate_exact_size(Vec2::splat(10.0), Sense::hover());
                        ui.painter().rect_filled(
                            rect,
                            CornerRadius::same(2),
                            STRATA[index % STRATA.len()],
                        );
                    });
                    row.col(|ui| {
                        let name = if item.is_directory {
                            format!("{}/", item.name)
                        } else {
                            item.name.clone()
                        };
                        ui.label(RichText::new(name).color(CHALK));
                    });
                    row.col(|ui| {
                        ui.label(
                            RichText::new(format_bytes(item.bytes))
                                .font(medium(13.0))
                                .color(if item.complete { CHALK } else { ORE }),
                        );
                    });
                    row.col(|ui| {
                        ui.label(RichText::new(percent(item.bytes, total)).color(SILT));
                    });
                    row.col(|ui| {
                        ui.label(RichText::new(age(item.modified)).color(SILT));
                    });
                    let response = row.response();
                    if response.clicked() {
                        clicked = Some(index);
                    }
                    if response.double_clicked() && item.is_directory {
                        opened = Some(item.path.clone());
                    }
                });
            });
        if let Some(index) = clicked {
            self.state.set_focus(index);
        }
        if let Some(path) = opened.filter(|_| !busy) {
            self.state.open_folder(path);
        }
    }

    fn breadcrumbs(&mut self, ui: &mut Ui) {
        let root = self.state.disk_root.clone();
        let mut target = None;
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            let mut path = PathBuf::new();
            let components: Vec<_> = root.components().collect();
            for (index, component) in components.iter().enumerate() {
                path.push(component);
                let label = if path == self.state.home {
                    "Home".to_string()
                } else {
                    match component {
                        std::path::Component::RootDir => "Startup disk".to_string(),
                        other => other.as_os_str().to_string_lossy().into_owned(),
                    }
                };
                let last = index + 1 == components.len();
                let text = RichText::new(label)
                    .small()
                    .color(if last { CHALK } else { SILT });
                if ui
                    .add(egui::Button::new(text).frame(false))
                    .on_hover_text(format!("Open {}", path.display()))
                    .clicked()
                    && !last
                {
                    target = Some(path.clone());
                }
                if !last {
                    ui.label(RichText::new("/").small().color(SHALE));
                }
            }
        });
        if let Some(path) = target.filter(|_| self.state.can_start()) {
            self.state.open_folder(path);
        }
    }

    // System status ---------------------------------------------------------

    fn status_page(&mut self, ui: &mut Ui) {
        page_heading(ui, Section::Status);
        let Some(status) = self.state.status.clone() else {
            empty_state(ui, "Reading metrics", "System metrics appear in a moment.");
            return;
        };
        ui.label(RichText::new(&status.processor).font(medium(16.0)));
        ui.label(
            RichText::new(format!(
                "{}, {} logical CPUs",
                status.os_version, status.logical_cpus
            ))
            .color(SILT),
        );
        ui.add_space(20.0);
        if let Some(memory) = status.memory {
            metric(
                ui,
                "Memory in use",
                &format_bytes(memory.used),
                &format!("of {}", format_bytes(memory.total)),
                ratio(memory.used, memory.total),
                STRATA[0],
            );
        }
        if let Some(disk) = status.disk {
            let used = disk.total.saturating_sub(disk.available);
            metric(
                ui,
                "Startup disk in use",
                &format_bytes(used),
                &format!(
                    "{} free of {}",
                    format_bytes(disk.available),
                    format_bytes(disk.total)
                ),
                ratio(used, disk.total),
                STRATA[3],
            );
        }
        ui.add_space(8.0);
        ui.columns(2, |columns| {
            let load = status
                .load_average
                .map(|load| format!("{:.2}", load[0]))
                .unwrap_or_else(|| "Unavailable".to_string());
            let detail = status
                .load_average
                .map(|load| {
                    format!(
                        "Over the last minute. 5-minute average {:.2}, 15-minute {:.2}.",
                        load[1], load[2]
                    )
                })
                .unwrap_or_default();
            figure(&mut columns[0], "Load average", &load, &detail);
            let uptime = status
                .uptime_seconds
                .map(duration)
                .unwrap_or_else(|| "Unavailable".to_string());
            figure(
                &mut columns[1],
                "Up for",
                &uptime,
                "Since the last restart.",
            );
        });
    }

    // Activity --------------------------------------------------------------

    fn activity_page(&mut self, ui: &mut Ui) {
        page_heading(ui, Section::Activity);
        if self.state.activity.is_empty() {
            empty_state(
                ui,
                "Nothing moved yet",
                "When you move items to Trash, each step is listed here with every path, so you can find them in the Trash later.",
            );
            return;
        }
        egui::ScrollArea::vertical().show(ui, |ui| {
            for entry in &self.state.activity {
                ui.separator();
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new(clock(entry.when))
                            .font(medium(13.0))
                            .color(SILT),
                    );
                    ui.label(
                        RichText::new(&entry.summary)
                            .font(medium(15.0))
                            .color(if entry.failed { FLARE } else { CHALK }),
                    );
                });
                for line in &entry.details {
                    ui.label(RichText::new(line).small().color(SILT));
                }
                ui.add_space(6.0);
            }
        });
    }

    // Inspector -------------------------------------------------------------

    fn inspector(&mut self, ui: &mut Ui) {
        // A focused item hidden by the filter is not shown.
        let visible = !is_list(self.state.section)
            || self
                .state
                .focus_index()
                .is_some_and(|focus| self.visible_order().contains(&focus));
        let state = &self.state;
        let Some(index) = state.focus_index().filter(|_| visible) else {
            ui.label(RichText::new("Details").font(condensed(20.0)));
            ui.add_space(6.0);
            ui.label(
                RichText::new("Click an item to see where it lives, how large it is, and what happens when it moves to Trash.")
                    .color(SILT),
            );
            return;
        };
        let home = state.home.clone();
        let mut reveal: Option<PathBuf> = None;
        egui::ScrollArea::vertical().show(ui, |ui| match state.section {
            Section::Clean | Section::Installers => {
                let item = if state.section == Section::Clean {
                    &state.clean_items[index]
                } else {
                    &state.installer_items[index]
                };
                let shown = friendly_name(&item.name, &state.app_items);
                inspector_title(ui, &shown, item.category.label());
                ui.label(RichText::new("What it is").font(medium(13.0)).color(SILT));
                ui.label(item.category.explanation());
                ui.add_space(12.0);
                fact(ui, "Size", &size_text(item.bytes, item.complete));
                fact(ui, "Changed", &age(item.modified));
                if shown != item.name {
                    fact(ui, "Folder name", &item.name);
                }
                fact(ui, "Location", &tilde_path(&item.path, &home));
                ui.add_space(10.0);
                if ui.button("Show in Finder").clicked() {
                    reveal = Some(item.path.clone());
                }
            }
            Section::Applications => {
                let app = &state.app_items[index];
                inspector_title(
                    ui,
                    &app.name,
                    &app
                        .version
                        .as_ref()
                        .map(|version| format!("Version {version}"))
                        .unwrap_or_else(|| "Version unknown".to_string()),
                );
                fact(ui, "Size", &format_bytes(app.bytes));
                fact(ui, "Changed", &age(app.modified));
                fact(
                    ui,
                    "Bundle ID",
                    app.bundle_id.as_deref().unwrap_or("None found"),
                );
                fact(ui, "Location", &tilde_path(&app.path, &home));
                ui.add_space(12.0);
                ui.label(RichText::new("Uninstall moves").font(medium(13.0)).color(SILT));
                ui.label(format!("{}  {}", app.name, format_bytes(app.bytes)));
                let related = related_app_data(&state.app_items, app, &home);
                for (path, bytes) in &related.paths {
                    ui.label(format!("{}  {}", tilde_path(path, &home), format_bytes(*bytes)));
                }
                if related.kept_shared {
                    ui.label(
                        RichText::new("Its caches stay, because another installed copy uses them.")
                            .color(SILT),
                    );
                } else if related.paths.is_empty() {
                    ui.label(RichText::new("No caches or logs were found for this app.").color(SILT));
                }
                ui.add_space(10.0);
                ui.label(RichText::new("Stays on your Mac").font(medium(13.0)).color(SILT));
                ui.label("Settings, documents, app containers, and browser profiles.");
                ui.add_space(10.0);
                if ui.button("Show in Finder").clicked() {
                    reveal = Some(app.path.clone());
                }
            }
            Section::Disk => {
                let item = &state.disk_items[index];
                let total: u64 = state.disk_items.iter().map(|item| item.bytes).sum();
                inspector_title(
                    ui,
                    &item.name,
                    if item.is_directory { "Folder" } else { "File" },
                );
                fact(ui, "Size", &size_text(item.bytes, item.complete));
                fact(ui, "Share of folder", &percent(item.bytes, total));
                fact(ui, "Changed", &age(item.modified));
                fact(ui, "Location", &tilde_path(&item.path, &home));
                ui.add_space(12.0);
                let protected = is_protected_disk_path(&item.path, &home);
                if protected {
                    ui.label(
                        RichText::new("Protected. MacCleanUp does not move this item, because macOS or your own files depend on it.")
                            .color(SILT),
                    );
                }
                if ui.button("Show in Finder").clicked() {
                    reveal = Some(item.path.clone());
                }
            }
            _ => {}
        });
        // Buttons that change state are handled after the immutable borrow.
        if self.state.section == Section::Disk {
            if let Some(index) = self.state.focus_index() {
                let item = self.state.disk_items[index].clone();
                let protected = is_protected_disk_path(&item.path, &self.state.home);
                ui.add_space(8.0);
                if item.is_directory
                    && ui
                        .add_enabled(self.state.can_start(), egui::Button::new("Open folder"))
                        .clicked()
                {
                    self.state.open_folder(item.path.clone());
                }
                let trash =
                    egui::Button::new(RichText::new("Move to Trash…").color(BEDROCK)).fill(ORE);
                if ui
                    .add_enabled(!protected && self.state.can_start(), trash)
                    .clicked()
                {
                    self.state.review();
                    self.focus_cancel = true;
                }
            }
        }
        if let Some(path) = reveal {
            reveal_in_finder(&path);
        }
    }

    // Footer and dialogs ----------------------------------------------------

    fn status_bar(&mut self, ui: &mut Ui) {
        ui.horizontal_centered(|ui| {
            let message = &self.state.message;
            let failed = message.contains("failed") || message.starts_with("Cannot");
            ui.label(RichText::new(message).color(if failed { FLARE } else { SILT }));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let errors = &self.state.scan_errors[self.state.section.index()];
                if !errors.is_empty()
                    && ui
                        .add(
                            egui::Button::new(
                                RichText::new(plural(
                                    errors.len(),
                                    "unreadable location",
                                    "unreadable locations",
                                ))
                                .small(),
                            )
                            .frame(false),
                        )
                        .on_hover_text("Show the locations the last scan could not read")
                        .clicked()
                {
                    self.state.notice = Some(state::Notice {
                        title: "Locations that could not be read".to_string(),
                        lines: errors.clone(),
                    });
                }
                if let Some(busy) = self.state.busy.clone() {
                    if !busy.mutating
                        && ui
                            .small_button("Stop")
                            .on_hover_text("Stop this scan (Esc)")
                            .clicked()
                    {
                        self.state.cancel_scan();
                    }
                    ui.label(RichText::new(&busy.label).color(if busy.mutating {
                        ORE
                    } else {
                        HEADLAMP
                    }));
                    ui.add(egui::Spinner::new().size(14.0).color(HEADLAMP));
                }
            });
        });
    }

    fn dialogs(&mut self, ctx: &egui::Context) {
        if let Some(confirmation) = self.state.confirmation.clone() {
            let mut cancel = false;
            let mut accept = false;
            let modal = egui::Modal::new(Id::unique("confirm"))
                .backdrop_color(Color32::from_black_alpha(150))
                .frame(
                    egui::Frame::new()
                        .fill(SEAM)
                        .stroke(Stroke::new(1.0, ORE.gamma_multiply(0.6)))
                        .corner_radius(CornerRadius::same(12))
                        .inner_margin(Margin::same(24)),
                )
                .show(ctx, |ui| {
                    ui.set_width(520.0);
                    ui.label(RichText::new(&confirmation.title).font(condensed(26.0)));
                    let total: u64 = confirmation.moves.iter().map(|(_, bytes)| bytes).sum();
                    ui.label(
                        RichText::new(format!(
                            "{} in {}. You can put them back from the Trash until you empty it.",
                            format_bytes(total),
                            plural(confirmation.moves.len(), "location", "locations")
                        ))
                        .color(SILT),
                    );
                    ui.add_space(12.0);
                    egui::Frame::new()
                        .fill(BEDROCK)
                        .corner_radius(CornerRadius::same(6))
                        .inner_margin(Margin::same(10))
                        .show(ui, |ui| {
                            egui::ScrollArea::vertical()
                                .max_height(220.0)
                                .show(ui, |ui| {
                                    ui.set_width(ui.available_width());
                                    for (path, bytes) in &confirmation.moves {
                                        ui.horizontal(|ui| {
                                            ui.add(
                                                egui::Label::new(RichText::new(path).small())
                                                    .truncate(),
                                            );
                                            ui.with_layout(
                                                Layout::right_to_left(Align::Center),
                                                |ui| {
                                                    ui.label(
                                                        RichText::new(format_bytes(*bytes))
                                                            .small()
                                                            .color(SILT),
                                                    );
                                                },
                                            );
                                        });
                                    }
                                });
                        });
                    ui.add_space(10.0);
                    for note in &confirmation.notes {
                        ui.label(RichText::new(note).small().color(SILT));
                    }
                    ui.add_space(16.0);
                    ui.horizontal(|ui| {
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            let trash = egui::Button::new(
                                RichText::new(&confirmation.button).color(BEDROCK),
                            )
                            .fill(ORE);
                            if ui.add(trash).clicked() {
                                accept = true;
                            }
                            let cancel_button = ui.button("Cancel");
                            if self.focus_cancel {
                                cancel_button.request_focus();
                                self.focus_cancel = false;
                            }
                            if cancel_button.clicked() {
                                cancel = true;
                            }
                        });
                    });
                });
            // Return and Esc both cancel; only a click on the Trash button proceeds.
            let keyed =
                ctx.input(|input| input.key_pressed(Key::Escape) || input.key_pressed(Key::Enter));
            if cancel || keyed || modal.should_close() {
                self.state.cancel_confirmation();
            } else if accept {
                self.state.accept_confirmation();
            }
        }

        if let Some(notice) = self.state.notice.clone() {
            let modal = egui::Modal::new(Id::unique("notice"))
                .backdrop_color(Color32::from_black_alpha(150))
                .frame(
                    egui::Frame::new()
                        .fill(SEAM)
                        .corner_radius(CornerRadius::same(12))
                        .inner_margin(Margin::same(24)),
                )
                .show(ctx, |ui| {
                    ui.set_width(520.0);
                    ui.label(RichText::new(&notice.title).font(condensed(24.0)));
                    ui.add_space(8.0);
                    egui::ScrollArea::vertical()
                        .max_height(300.0)
                        .show(ui, |ui| {
                            for line in &notice.lines {
                                ui.label(RichText::new(line).small());
                            }
                        });
                    ui.add_space(14.0);
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.button("Close").clicked()
                    })
                    .inner
                });
            if modal.inner || modal.should_close() {
                self.state.notice = None;
            }
        }
    }
}

impl eframe::App for MacCleanUp {
    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        if ctx.input(|input| input.viewport().close_requested()) && !self.state.can_quit() {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.state.message =
                "MacCleanUp closes after the Trash operation finishes.".to_string();
        }
        self.show(ui);
    }
}

// Small drawing helpers ------------------------------------------------------

fn has_inspector(section: Section) -> bool {
    matches!(
        section,
        Section::Clean | Section::Applications | Section::Installers | Section::Disk
    )
}

fn is_list(section: Section) -> bool {
    matches!(
        section,
        Section::Clean | Section::Applications | Section::Installers
    )
}

fn clean_rows(
    items: &[CleanItem],
    apps: &[AppItem],
    home: &Path,
) -> Vec<(usize, String, u64, Option<SystemTime>, String)> {
    items
        .iter()
        .enumerate()
        .map(|(index, item)| {
            (
                index,
                friendly_name(&item.name, apps),
                item.bytes,
                item.modified,
                tilde_path(&item.path, home),
            )
        })
        .collect()
}

/// Turns a folder named after a bundle ID into a name people recognise:
/// the installed app's name when one matches, otherwise a tidied form of
/// the ID ("com.google.Chrome" becomes "Google Chrome"). Other names pass
/// through unchanged.
fn friendly_name(folder: &str, apps: &[AppItem]) -> String {
    if let Some(app) = apps.iter().find(|app| {
        app.bundle_id
            .as_deref()
            .is_some_and(|id| id.eq_ignore_ascii_case(folder))
    }) {
        return app.name.clone();
    }
    let parts: Vec<&str> = folder.split('.').collect();
    let reverse_dns = parts.len() >= 3
        && ["com", "org", "io", "net", "app", "dev", "co", "us"]
            .contains(&parts[0].to_ascii_lowercase().as_str())
        && parts
            .iter()
            .all(|part| !part.is_empty() && !part.contains(' '));
    if !reverse_dns {
        return folder.to_string();
    }
    let capital = |word: &str| {
        let mut chars = word.chars();
        chars
            .next()
            .map(|first| first.to_uppercase().chain(chars).collect::<String>())
            .unwrap_or_default()
    };
    let vendor = parts[1];
    let product = parts[parts.len() - 1];
    if vendor.eq_ignore_ascii_case("apple") || vendor.eq_ignore_ascii_case(product) {
        capital(product)
    } else {
        format!("{} {}", capital(vendor), capital(product))
    }
}

/// Plain definitions for people who have never heard the words "cache" or
/// "log", shown above the list they describe.
fn cache_explainer(ui: &mut Ui) {
    egui::Frame::new()
        .fill(SEAM)
        .corner_radius(CornerRadius::same(8))
        .inner_margin(Margin::symmetric(18, 14))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.columns(2, |columns| {
                for (ui, (question, answer)) in columns.iter_mut().zip([
                    (
                        "What is a cache?",
                        "Copies an app keeps so it can open things faster, like pictures from websites you have visited. Apps make new ones when they need them.",
                    ),
                    (
                        "What is a log?",
                        "A record an app keeps of what it did, mostly to help fix problems. Old logs are not needed by anything.",
                    ),
                ]) {
                    // Columns justify text by default; keep it ragged-right.
                    ui.with_layout(Layout::top_down(Align::Min), |ui| {
                        ui.label(RichText::new(question).font(medium(14.5)).color(CHALK));
                        ui.label(RichText::new(answer).color(SILT));
                    });
                }
            });
            ui.add_space(8.0);
            ui.label(
                RichText::new(
                    "Your documents, photos, passwords, and settings are never listed here. Removing these files is safe; at worst an app opens a little slower the first time.",
                )
                .color(HEADLAMP),
            );
        });
    ui.add_space(14.0);
}

fn page_heading(ui: &mut Ui, section: Section) {
    ui.label(
        RichText::new(section.title())
            .font(condensed(32.0))
            .color(CHALK),
    );
    ui.label(RichText::new(section.description()).color(SILT));
    ui.add_space(14.0);
}

fn nav_row(ui: &mut Ui, title: &str, hint: &str, active: bool) -> egui::Response {
    let (rect, response) =
        ui.allocate_exact_size(Vec2::new(ui.available_width(), 32.0), Sense::click());
    let painter = ui.painter();
    if active {
        painter.rect_filled(rect, CornerRadius::same(5), BEDROCK);
        painter.rect_filled(
            Rect::from_min_size(rect.min + Vec2::new(0.0, 7.0), Vec2::new(3.0, 18.0)),
            CornerRadius::same(2),
            HEADLAMP,
        );
    } else if response.hovered() {
        painter.rect_filled(rect, CornerRadius::same(5), SHALE.gamma_multiply(0.6));
    }
    painter.text(
        Pos2::new(rect.left() + 14.0, rect.center().y),
        Align2::LEFT_CENTER,
        title,
        if active {
            medium(14.5)
        } else {
            egui::FontId::proportional(14.5)
        },
        if active { CHALK } else { SILT },
    );
    if !hint.is_empty() {
        painter.text(
            Pos2::new(rect.right() - 8.0, rect.center().y),
            Align2::RIGHT_CENTER,
            hint,
            egui::FontId::proportional(12.0),
            SILT,
        );
    }
    response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, title));
    response
}

/// The storage "core sample": one bar, each stratum proportional to its bytes.
fn strata_bar(ui: &mut Ui, strata: &[(String, u64, Color32)], total: u64) {
    let (rect, response) =
        ui.allocate_exact_size(Vec2::new(ui.available_width(), 46.0), Sense::hover());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, CornerRadius::same(6), SEAM);
    let mut x = rect.left();
    let mut hovered = None;
    for (index, (label, bytes, color)) in strata.iter().enumerate() {
        let width = rect.width() * ratio(*bytes, total);
        let segment = Rect::from_min_max(
            Pos2::new(x, rect.top()),
            Pos2::new(x + width, rect.bottom()),
        );
        painter.rect_filled(segment, CornerRadius::ZERO, *color);
        if response
            .hover_pos()
            .is_some_and(|pos| segment.contains(pos))
        {
            hovered = Some((label.clone(), *bytes));
        }
        if index + 1 < strata.len() && width > 1.0 {
            painter.line_segment(
                [
                    Pos2::new(x + width, rect.top()),
                    Pos2::new(x + width, rect.bottom()),
                ],
                Stroke::new(1.0, BEDROCK),
            );
        }
        x += width;
    }
    painter.rect_stroke(
        rect,
        CornerRadius::same(6),
        Stroke::new(1.0, SHALE),
        StrokeKind::Inside,
    );
    if let Some((label, bytes)) = hovered {
        response.on_hover_text(format!("{label}: {}", format_bytes(bytes)));
    }
}

fn legend_row(ui: &mut Ui, color: Color32, label: &str, bytes: u64, total: u64) {
    ui.horizontal(|ui| {
        let (rect, _) = ui.allocate_exact_size(Vec2::splat(10.0), Sense::hover());
        ui.painter().rect_filled(rect, CornerRadius::same(2), color);
        ui.label(RichText::new(label).color(CHALK));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.label(RichText::new(percent(bytes, total)).color(SILT));
            ui.add_space(12.0);
            ui.label(RichText::new(format_bytes(bytes)).font(medium(13.0)));
        });
    });
}

fn metric(ui: &mut Ui, label: &str, value: &str, detail: &str, share: f32, color: Color32) {
    ui.label(RichText::new(label).color(SILT));
    ui.horizontal(|ui| {
        ui.label(RichText::new(value).font(condensed(38.0)));
        ui.label(RichText::new(detail).color(SILT));
    });
    let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 10.0), Sense::hover());
    ui.painter().rect_filled(rect, CornerRadius::same(5), SEAM);
    ui.painter().rect_filled(
        Rect::from_min_size(
            rect.min,
            Vec2::new(rect.width() * share.clamp(0.0, 1.0), rect.height()),
        ),
        CornerRadius::same(5),
        color,
    );
    ui.label(
        RichText::new(format!("{:.0}%", share * 100.0))
            .small()
            .color(SILT),
    );
    ui.add_space(16.0);
}

fn figure(ui: &mut Ui, label: &str, value: &str, detail: &str) {
    ui.label(RichText::new(label).color(SILT));
    ui.label(RichText::new(value).font(condensed(38.0)));
    ui.label(RichText::new(detail).small().color(SILT));
}

fn empty_state(ui: &mut Ui, title: &str, text: &str) {
    ui.add_space(40.0);
    ui.vertical_centered(|ui| {
        ui.set_max_width(440.0);
        ui.label(RichText::new(title).font(condensed(22.0)).color(CHALK));
        ui.add_space(4.0);
        ui.label(RichText::new(text).color(SILT));
    });
}

fn inspector_title(ui: &mut Ui, name: &str, kind: &str) {
    ui.add(egui::Label::new(RichText::new(name).font(condensed(22.0))).wrap());
    ui.label(RichText::new(kind).color(HEADLAMP));
    ui.add_space(12.0);
}

fn fact(ui: &mut Ui, label: &str, value: &str) {
    ui.label(RichText::new(label).small().color(SILT));
    ui.add(egui::Label::new(value).wrap());
    ui.add_space(6.0);
}

fn size_text(bytes: u64, complete: bool) -> String {
    if complete {
        format_bytes(bytes)
    } else {
        format!(
            "At least {} (some files could not be read)",
            format_bytes(bytes)
        )
    }
}

fn ratio(part: u64, total: u64) -> f32 {
    if total == 0 {
        0.0
    } else {
        (part as f64 / total as f64) as f32
    }
}

fn percent(part: u64, total: u64) -> String {
    let share = ratio(part, total) * 100.0;
    if part > 0 && share < 1.0 {
        "<1%".to_string()
    } else {
        format!("{share:.0}%")
    }
}

/// Shortens `text` with an ellipsis to roughly fit `width` points.
fn elide(text: &str, width: f32, char_width: f32) -> String {
    let max = (width / char_width).floor() as usize;
    if text.chars().count() <= max || max < 2 {
        return text.to_string();
    }
    let kept: String = text.chars().take(max - 1).collect();
    format!("{kept}…")
}

fn duration(seconds: u64) -> String {
    let days = seconds / 86_400;
    let hours = (seconds % 86_400) / 3_600;
    let minutes = (seconds % 3_600) / 60;
    if days > 0 {
        format!("{days}d {hours}h")
    } else {
        format!("{hours}h {minutes}m")
    }
}

/// Local time of day, for the activity log.
fn clock(when: SystemTime) -> String {
    let seconds = when
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs() as libc::time_t)
        .unwrap_or(0);
    let mut parts = std::mem::MaybeUninit::<libc::tm>::zeroed();
    // SAFETY: localtime_r writes a complete tm into the provided buffer.
    let ok = unsafe { !libc::localtime_r(&seconds, parts.as_mut_ptr()).is_null() };
    if !ok {
        return String::new();
    }
    let parts = unsafe { parts.assume_init() };
    format!("{:02}:{:02}", parts.tm_hour, parts.tm_min)
}

/// Selects the item in Finder. Runs only on an explicit button press.
fn reveal_in_finder(path: &Path) {
    let _ = std::process::Command::new("/usr/bin/open")
        .arg("-R")
        .arg(path)
        .spawn();
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui_kittest::kittest::Queryable;
    use egui_kittest::Harness;
    use std::time::Duration;

    fn fixture_home() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/home")
    }

    fn harness(home: PathBuf) -> Harness<'static, MacCleanUp> {
        Harness::builder()
            .with_size(Vec2::new(1240.0, 800.0))
            .with_accessibility_check(false)
            .build_ui_state(
                |ui, app: &mut MacCleanUp| app.show(ui),
                MacCleanUp::new(home),
            )
    }

    /// Steps frames until background jobs finish.
    fn settle(harness: &mut Harness<'static, MacCleanUp>) {
        for _ in 0..4_000 {
            harness.step();
            if harness.state().state.busy.is_none() {
                harness.run_steps(3);
                return;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        panic!("background job did not finish");
    }

    /// Clicks a sidebar entry. Overview rows reuse the titles as plain labels,
    /// so the query names the button role.
    fn nav(harness: &mut Harness<'static, MacCleanUp>, title: &str) {
        harness
            .get_by_role_and_label(egui::Role::Button, title)
            .click();
        harness.run_steps(2);
    }

    /// Saves a PNG of the current frame when MACCLEANUP_SHOTS names a folder.
    fn shot(harness: &mut Harness<'static, MacCleanUp>, name: &str) {
        let Some(folder) = std::env::var_os("MACCLEANUP_SHOTS") else {
            return;
        };
        let image = harness.render().expect("render frame");
        image
            .save(PathBuf::from(folder).join(format!("{name}.png")))
            .expect("save png");
    }

    #[test]
    fn overview_shows_storage_strata_and_review_rows() {
        let mut harness = harness(fixture_home());
        settle(&mut harness);
        settle(&mut harness);
        harness.get_by_label("Ready to review");
        harness.get_by_label_contains("free on your startup disk");
        harness.get_by_label("Find installers");
        harness.get_by_label("Review caches");
        assert!(!harness.state().state.is_loaded(Section::Installers));
        shot(&mut harness, "overview");
    }

    #[test]
    fn caches_list_inspector_and_cancel_first_dialog() {
        let mut harness = harness(fixture_home());
        settle(&mut harness);
        nav(&mut harness, "Caches and logs");
        settle(&mut harness);
        let beta = harness
            .state()
            .state
            .clean_items
            .iter()
            .position(|item| item.name == "com.example.beta")
            .unwrap();
        harness.state_mut().state.set_focus(beta);
        harness.run_steps(2);
        harness.get_by_label("What is a cache?");
        harness.get_by_label("What is a log?");
        harness.get_by_label_contains("never listed here");
        harness.get_by_label_contains("A cache is a set of copies");
        harness.get_by_label("Show in Finder");
        shot(&mut harness, "caches");

        // The row shows the tidied name, not the raw folder name.
        harness.get_by_label("Select Example Beta").click();
        harness.run_steps(2);
        harness.get_by_label("Move 1 item to Trash…").click();
        harness.run_steps(2);
        harness.get_by_label("Move 1 item to Trash?");
        harness.get_by_label("Cancel");
        shot(&mut harness, "confirm");

        // Return cancels.
        harness.key_press(Key::Enter);
        harness.run_steps(2);
        assert!(harness.state().state.confirmation.is_none());
        assert!(harness.state().state.busy.is_none());

        // Esc cancels.
        harness.get_by_label("Move 1 item to Trash…").click();
        harness.run_steps(2);
        harness.key_press(Key::Escape);
        harness.run_steps(2);
        assert!(harness.state().state.confirmation.is_none());

        // The Cancel button cancels.
        harness.get_by_label("Move 1 item to Trash…").click();
        harness.run_steps(2);
        harness.get_by_label("Cancel").click();
        harness.run_steps(2);
        assert!(harness.state().state.confirmation.is_none());
        assert!(harness.state().state.busy.is_none());
        assert!(fixture_home()
            .join("Library/Caches/com.example.beta")
            .exists());
    }

    #[test]
    fn filter_and_sort_change_the_visible_rows() {
        let mut harness = harness(fixture_home());
        settle(&mut harness);
        nav(&mut harness, "Caches and logs");
        settle(&mut harness);
        harness.state_mut().state.filter[Section::Clean.index()] = "alpha".to_string();
        harness.run_steps(2);
        assert_eq!(harness.state().visible_order().len(), 1);
        assert!(harness.query_by_label("com.example.beta").is_none());
        harness.state_mut().state.filter[Section::Clean.index()] = "nothing-matches".to_string();
        harness.run_steps(2);
        harness.get_by_label("Nothing to show");
        harness.state_mut().state.filter[Section::Clean.index()].clear();
        harness.run_steps(2);
        harness.get_by_label("Name").click();
        harness.run_steps(2);
        assert_eq!(
            harness.state().state.sort[Section::Clean.index()],
            SortKey::Name
        );
    }

    #[test]
    fn installers_wait_for_a_scan_then_list_folders() {
        let mut harness = harness(fixture_home());
        settle(&mut harness);
        nav(&mut harness, "Installer files");
        harness.run_steps(2);
        harness.get_by_label("Not scanned yet");
        harness.get_by_label("Scan").click();
        settle(&mut harness);
        assert!(harness.query_all_by_label("Setup.dmg").count() >= 1);
        harness.get_by_label("~/Downloads/sub");
        shot(&mut harness, "installers");
    }

    #[test]
    fn applications_inspector_previews_what_uninstall_moves() {
        let home = fixture_home();
        let mut harness = harness(home.clone());
        settle(&mut harness);
        settle(&mut harness);
        nav(&mut harness, "Applications");
        harness.run_steps(2);
        {
            let app = &mut harness.state_mut().state;
            app.app_items = vec![maccleanup::model::AppItem {
                name: "Example".to_string(),
                bundle_id: Some("com.example.alpha".to_string()),
                version: Some("2.4".to_string()),
                path: home.join("Applications/Example.app"),
                bytes: 2_000_000,
                selected: false,
                identity: None,
                modified: None,
            }];
            app.set_focus(0);
        }
        harness.run_steps(2);
        harness.get_by_label("Version 2.4");
        harness.get_by_label("Uninstall moves");
        harness.get_by_label_contains("Library/Caches/com.example.alpha");
        shot(&mut harness, "applications");
    }

    #[test]
    fn disk_map_measures_on_request_and_protects_home_folders() {
        let home = fixture_home();
        let mut harness = harness(home.clone());
        settle(&mut harness);
        nav(&mut harness, "Disk map");
        harness.run_steps(2);
        harness.get_by_label("Not measured yet");
        harness.get_by_label("Measure").click();
        settle(&mut harness);
        let library = harness
            .state()
            .state
            .disk_items
            .iter()
            .position(|item| item.name == "Library")
            .unwrap();
        harness.state_mut().state.set_focus(library);
        harness.run_steps(2);
        harness.get_by_label_contains("Protected.");
        shot(&mut harness, "disk");
        harness.state_mut().state.review();
        assert!(harness.state().state.confirmation.is_none());
    }

    #[test]
    fn status_and_activity_pages_render() {
        let mut harness = harness(fixture_home());
        settle(&mut harness);
        nav(&mut harness, "System");
        harness.run_steps(2);
        harness.get_by_label("Memory in use");
        harness.get_by_label("Load average");
        shot(&mut harness, "system");
        nav(&mut harness, "Activity");
        harness.run_steps(2);
        harness.get_by_label("Nothing moved yet");
        harness
            .state_mut()
            .state
            .activity
            .push(state::ActivityEntry {
                when: SystemTime::now(),
                summary: "Moved 2 items (4.00 KB) to Trash.".to_string(),
                details: vec!["~/Library/Caches/example".to_string()],
                failed: false,
            });
        harness.run_steps(2);
        harness.get_by_label("Moved 2 items (4.00 KB) to Trash.");
        shot(&mut harness, "activity");
    }

    #[test]
    fn repaints_are_requested_while_jobs_run() {
        let mut state = State::new(fixture_home());
        assert!(state.update().is_some(), "overview refreshes status");
        state.select_section(Section::Disk);
        state.open_folder(fixture_home());
        assert_eq!(state.update(), Some(Duration::from_millis(120)));
    }

    #[test]
    fn cache_folders_show_names_people_recognise() {
        let apps = vec![maccleanup::model::AppItem {
            name: "Brave Browser".to_string(),
            bundle_id: Some("com.brave.Browser".to_string()),
            version: None,
            path: PathBuf::from("/Applications/Brave Browser.app"),
            bytes: 1,
            selected: false,
            identity: None,
            modified: None,
        }];
        assert_eq!(friendly_name("com.brave.Browser", &apps), "Brave Browser");
        assert_eq!(friendly_name("COM.BRAVE.BROWSER", &apps), "Brave Browser");
        assert_eq!(friendly_name("com.google.Chrome", &apps), "Google Chrome");
        assert_eq!(friendly_name("com.apple.Safari", &apps), "Safari");
        assert_eq!(
            friendly_name("org.mozilla.firefox", &apps),
            "Mozilla Firefox"
        );
        assert_eq!(friendly_name("com.spotify.Spotify", &apps), "Spotify");
        for unchanged in ["Homebrew", "Setup.dmg", "example.log", "pip", "a.b"] {
            assert_eq!(friendly_name(unchanged, &apps), unchanged);
        }
    }

    #[test]
    fn helpers_format_shares_and_ellipses() {
        assert_eq!(percent(1, 1_000), "<1%");
        assert_eq!(percent(500, 1_000), "50%");
        assert_eq!(percent(0, 0), "0%");
        assert_eq!(elide("abcdefghij", 40.0, 8.0), "abcd…");
        assert_eq!(duration(90_061), "1d 1h");
    }
}
