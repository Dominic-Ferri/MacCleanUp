mod state;
pub mod theme;
mod treemap;

use eframe::egui::{
    self, Align, Align2, Color32, CornerRadius, Id, Key, Layout, Margin, Modifiers, Pos2, Rect,
    RichText, Sense, Stroke, StrokeKind, Ui, Vec2,
};
use egui_extras::{Column, TableBuilder};
use maccleanup::model::{format_bytes, CleanItem};
use maccleanup::scanner::is_protected_disk_path;
use maccleanup::system::StoragePart;
use maccleanup::trash::related_app_data;
use state::{age, plural, tilde_path, Section, SortKey, State};
use std::collections::HashMap;
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
    /// The current list's filtered, sorted row order, computed once per
    /// frame after anything that can change the section.
    order: Vec<usize>,
    /// The part of the startup disk shown in the Storage view's details.
    storage_focus: Option<Part>,
}

impl MacCleanUp {
    pub fn new(home: PathBuf) -> Self {
        Self {
            state: State::new(home),
            focus_cancel: false,
            initialized: false,
            order: Vec::new(),
            storage_focus: None,
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
        self.order = self.visible_order();
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
                Section::Overview => {
                    // The review rows outgrow short windows.
                    egui::ScrollArea::vertical().show(ui, |ui| self.overview(ui));
                }
                Section::Storage => self.storage_page(ui),
                Section::Disk => self.disk_page(ui),
                Section::Status => self.status_page(ui),
                Section::Activity => self.activity_page(ui),
                _ => self.list_page(ui),
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
                Key::Num8,
                Key::Num9,
                Key::Num0,
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
            section if section.has_clean_items() && state.is_loaded(section) => state
                .items(section)
                .map(|items| total(items))
                .unwrap_or_default(),
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
        let mut measure = false;
        let mut look = None;

        if let Some(disk) = state.status.as_ref().and_then(|status| status.disk) {
            let used = disk.total.saturating_sub(disk.available);
            let strata = storage_strata(state, used, disk.available);

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
            look = strata_bar(ui, &strata, disk.total);
            ui.add_space(10.0);
            for stratum in &strata {
                if legend_row(ui, stratum.color, &stratum.label, stratum.bytes, disk.total) {
                    look = Some(stratum.part);
                }
            }
            if let Some(breakdown) = &state.breakdown {
                let mut unread = Vec::new();
                if breakdown.backups.is_none() {
                    unread.push("iPhone and iPad backups");
                }
                if breakdown.mail.is_none() {
                    unread.push("Mail and Messages");
                }
                if breakdown.trash.is_none() {
                    unread.push("the Trash");
                }
                if !unread.is_empty() {
                    ui.label(
                        RichText::new(format!(
                            "macOS did not let MacCleanUp read {}, or they are not on this Mac. To include them, add MacCleanUp under Full Disk Access in System Settings, Privacy & Security.",
                            unread.join(", ")
                        ))
                        .small()
                        .color(SILT),
                    );
                }
            }
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                let label = if state.breakdown.is_some() {
                    "Measure again"
                } else {
                    "Measure your files and more"
                };
                if ui
                    .add_enabled(state.can_start(), egui::Button::new(label))
                    .on_hover_text("Measures Desktop, Documents, Downloads, Movies, Music, Pictures, developer tools, device backups, Mail and Messages, and the Trash. macOS may ask for access to some folders.")
                    .clicked()
                {
                    measure = true;
                }
                if state.is_scanning(Section::Overview) {
                    scan_bar(ui, state.progress, 320.0);
                }
            });
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
            search_row(
                state,
                Section::LargeFiles,
                "Files of 1 GB or more you have not changed in 6 months.",
                "Find large files",
                "Review large files",
            ),
            search_row(
                state,
                Section::BuildFolders,
                "Cargo and npm build output in your projects.",
                "Find build folders",
                "Review build folders",
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
            if section.has_clean_items() && !self.state.is_loaded(section) {
                self.state.scan(section);
            }
        }
        if measure {
            self.state.measure_storage();
        }
        if let Some(part) = look {
            self.state.select_section(Section::Storage);
            self.storage_focus = Some(part);
        }
    }

    // Lists: caches, installers, applications --------------------------------

    fn visible_order(&self) -> Vec<usize> {
        let state = &self.state;
        let section = state.section;
        let filter = state.filter[section.index()].to_lowercase();
        let sort = state.sort[section.index()];
        let mut rows: Vec<(usize, String, u64, Option<SystemTime>, String)> = match section {
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
            section => state
                .items(section)
                .map(|items| clean_rows(items, &state.app_names, &state.home))
                .unwrap_or_default(),
        };
        rows.retain(|(_, name, _, _, extra)| {
            filter.is_empty()
                || name.to_lowercase().contains(&filter)
                || extra.to_lowercase().contains(&filter)
        });
        match sort {
            SortKey::Size => rows.sort_by(|a, b| b.2.cmp(&a.2).then_with(|| a.1.cmp(&b.1))),
            SortKey::Name => rows.sort_by_cached_key(|row| row.1.to_lowercase()),
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
        if self.state.is_scanning(section) {
            scan_bar(ui, self.state.progress, ui.available_width());
            ui.add_space(10.0);
        }

        if !loaded {
            empty_state(
                ui,
                "Not scanned yet",
                match section {
                    Section::Installers => "Scan looks in Downloads, Desktop, Documents, Public, and iCloud Downloads. macOS may ask MacCleanUp for access to those folders the first time.",
                    Section::LargeFiles | Section::BuildFolders => "Scan looks through your whole home folder except Library and the Trash, so it takes a while. macOS may ask MacCleanUp for access to Desktop, Documents, and Downloads the first time.",
                    _ if self.state.is_stopped(section) => "The scan was stopped. Scan to see what is here.",
                    _ => "Scan to see what is here.",
                },
            );
            return;
        }
        let order = self.order.clone();
        if order.is_empty() {
            let empty = if self.state.filter[section.index()].is_empty() {
                match section {
                    Section::Installers => {
                        "No installer files were found in the usual download folders."
                    }
                    Section::Applications => {
                        "No apps were found in /Applications or ~/Applications."
                    }
                    Section::LargeFiles => {
                        "No file of 1 GB or more has gone unchanged for 6 months."
                    }
                    Section::BuildFolders => "No Cargo or npm build folders were found.",
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
            Section::Clean => "Type",
            _ => "Folder",
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
        self.state.is_selected(index)
    }

    fn row_text(&self, index: usize) -> (String, String, u64, Option<SystemTime>, bool) {
        let state = &self.state;
        if state.section == Section::Applications {
            let app = &state.app_items[index];
            return (
                app.name.clone(),
                app.version.clone().unwrap_or_else(|| "Unknown".to_string()),
                app.bytes,
                app.modified,
                app.complete,
            );
        }
        let item = &state.items(state.section).expect("list section")[index];
        let (name, third) = if state.section == Section::Clean {
            (
                friendly_name(&item.name, &state.app_names),
                item.category.label().to_string(),
            )
        } else {
            let folder = item
                .path
                .parent()
                .map(|parent| tilde_path(parent, &state.home))
                .unwrap_or_default();
            (item.name.clone(), folder)
        };
        (name, third, item.bytes, item.modified, item.complete)
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
            if self.state.is_scanning(Section::Disk) {
                ui.add_space(12.0);
                scan_bar(ui, self.state.progress, ui.available_width().min(520.0));
            }
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
        ui.add_space(8.0);
        let extras = self.state.extras.as_ref();
        let pending = if self.state.is_stopped(Section::Status) {
            "Stopped. Press Cmd-R to read it."
        } else {
            "Reading…"
        };
        ui.columns(2, |columns| {
            let (trash, detail) = match extras.map(|extras| extras.trash) {
                None => (pending.to_string(), String::new()),
                Some(None) => (
                    "Unavailable".to_string(),
                    "macOS does not let MacCleanUp read the Trash. Open the Trash in Finder to see it.".to_string(),
                ),
                Some(Some(size)) => (
                    size_text(size.bytes, size.complete),
                    "Space comes back only when you empty the Trash. MacCleanUp never empties it.".to_string(),
                ),
            };
            figure(&mut columns[0], "In the Trash", &trash, &detail);
            let snapshots = match extras.map(|extras| extras.snapshots.as_ref()) {
                None => pending.to_string(),
                Some(None) => "Unavailable".to_string(),
                Some(Some(names)) => names.len().to_string(),
            };
            figure(
                &mut columns[1],
                "Time Machine local snapshots",
                &snapshots,
                "Copies of the startup disk that macOS keeps between backups. macOS removes them by itself when it needs the space.",
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

    // Storage ---------------------------------------------------------------

    fn storage_page(&mut self, ui: &mut Ui) {
        page_heading(ui, Section::Storage);
        let mut measure = false;
        ui.horizontal(|ui| {
            let label = if self.state.breakdown.is_some() {
                "Measure again"
            } else {
                "Measure your files and more"
            };
            if ui
                .add_enabled(self.state.can_start(), egui::Button::new(label))
                .on_hover_text("Measures Desktop, Documents, Downloads, Movies, Music, Pictures, developer tools, device backups, Mail and Messages, and the Trash. macOS may ask for access to some folders.")
                .clicked()
            {
                measure = true;
            }
            if self.state.is_scanning(Section::Overview) {
                scan_bar(ui, self.state.progress, ui.available_width());
            }
        });
        ui.add_space(10.0);
        let Some(disk) = self.state.status.as_ref().and_then(|status| status.disk) else {
            empty_state(ui, "Reading the startup disk", "Sizes appear in a moment.");
            return;
        };
        let used = disk.total.saturating_sub(disk.available);
        let strata = storage_strata(&self.state, used, disk.available);
        let mut focus = strata_bar(ui, &strata, disk.total);
        ui.add_space(12.0);
        let focused = self.storage_focus;
        TableBuilder::new(ui)
            .id_salt("storage")
            .sense(Sense::click())
            .cell_layout(Layout::left_to_right(Align::Center))
            .column(Column::exact(14.0))
            .column(Column::remainder().at_least(200.0).clip(true))
            .column(Column::exact(96.0))
            .column(Column::exact(64.0))
            .header(26.0, |mut header| {
                for label in ["", "Part", "Size", "Share"] {
                    header.col(|ui| {
                        ui.label(RichText::new(label).font(medium(12.5)).color(SILT));
                    });
                }
            })
            .body(|body| {
                body.rows(30.0, strata.len(), |mut row| {
                    let stratum = &strata[row.index()];
                    row.set_selected(focused == Some(stratum.part));
                    row.col(|ui| {
                        let (rect, _) = ui.allocate_exact_size(Vec2::splat(10.0), Sense::hover());
                        ui.painter()
                            .rect_filled(rect, CornerRadius::same(2), stratum.color);
                    });
                    row.col(|ui| {
                        ui.label(RichText::new(&stratum.label).color(CHALK));
                    });
                    row.col(|ui| {
                        ui.label(RichText::new(format_bytes(stratum.bytes)).font(medium(13.0)));
                    });
                    row.col(|ui| {
                        ui.label(RichText::new(percent(stratum.bytes, disk.total)).color(SILT));
                    });
                    if row.response().clicked() {
                        focus = Some(stratum.part);
                    }
                });
            });
        if focus.is_some() {
            self.storage_focus = focus;
        }
        if measure {
            self.state.measure_storage();
        }
    }

    /// The details panel for the Storage view's selected part.
    fn storage_inspector(&mut self, ui: &mut Ui) {
        enum Act {
            Disk(PathBuf),
            Reveal(PathBuf),
            Open(PathBuf),
            Go(Section),
            Measure,
        }
        let state = &self.state;
        let home = state.home.clone();
        let Some(part) = self.storage_focus else {
            ui.label(RichText::new("Details").font(condensed(20.0)));
            ui.add_space(6.0);
            ui.label(
                RichText::new("Click a part to see what it holds and where to look at it.")
                    .color(SILT),
            );
            return;
        };
        let mut act = None;
        let entries = |wanted: StoragePart| -> Vec<(PathBuf, u64)> {
            state
                .breakdown
                .iter()
                .flat_map(|breakdown| &breakdown.entries)
                .filter(|entry| entry.part == wanted)
                .take(15)
                .map(|entry| (entry.path.clone(), entry.size.bytes))
                .collect()
        };
        // A path and its size; a click opens it.
        let path_rows = |ui: &mut Ui, rows: &[(PathBuf, u64)], reveal: bool| -> Option<Act> {
            let mut clicked = None;
            for (path, bytes) in rows {
                ui.horizontal(|ui| {
                    let link = ui
                        .add(
                            egui::Label::new(RichText::new(tilde_path(path, &home)).color(CHALK))
                                .truncate()
                                .sense(Sense::click()),
                        )
                        .on_hover_text(if reveal {
                            "Show in Finder"
                        } else {
                            "Open in the disk map"
                        });
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.label(RichText::new(format_bytes(*bytes)).small().color(SILT));
                    });
                    if link.clicked() {
                        clicked = Some(if reveal || !path.is_dir() {
                            Act::Reveal(path.clone())
                        } else {
                            Act::Disk(path.clone())
                        });
                    }
                });
            }
            clicked
        };
        let names = |items: Vec<(String, u64)>, ui: &mut Ui| {
            for (name, bytes) in items {
                fact_row(ui, &name, bytes);
            }
        };
        egui::ScrollArea::vertical().show(ui, |ui| match part {
            Part::MacOs => {
                inspector_title(ui, "macOS", "System volumes");
                ui.label("macOS keeps itself on separate volumes that are sealed and read-only. Nothing here can go to the Trash; updates replace it as a whole.");
                ui.add_space(10.0);
                if let Some(volumes) = &state.volumes {
                    names(volumes.macos_volumes.clone(), ui);
                }
            }
            Part::Applications => {
                inspector_title(ui, "Applications", "Largest first");
                let mut apps: Vec<_> = state
                    .app_items
                    .iter()
                    .map(|app| (app.name.clone(), app.bytes))
                    .collect();
                apps.sort_by_key(|app| std::cmp::Reverse(app.1));
                apps.truncate(12);
                names(apps, ui);
                ui.add_space(10.0);
                if ui.button("Review apps").clicked() {
                    act = Some(Act::Go(Section::Applications));
                }
            }
            Part::Files => {
                inspector_title(ui, "Your files", "Largest folders first");
                ui.label("Your own folders. The Photos library is left out, because reading it raises a privacy prompt; its size counts under System Data and other files.");
                ui.add_space(10.0);
                act = path_rows(ui, &entries(StoragePart::Files), false);
            }
            Part::Developer => {
                inspector_title(ui, "Developer tools", "Toolchains and Xcode data");
                if let Some(breakdown) = &state.breakdown {
                    let listed = developer_caches(state, &breakdown.developer_roots);
                    ui.label(format!(
                        "Xcode data, Rust and npm toolchains, and Homebrew. {} of caches inside these folders count under Caches and logs instead.",
                        format_bytes(listed)
                    ));
                }
                ui.add_space(10.0);
                act = path_rows(ui, &entries(StoragePart::Developer), false);
            }
            Part::Backups => {
                inspector_title(ui, "iPhone and iPad backups", "One folder per device");
                ui.label("To remove a backup, select the device in a Finder window's sidebar, then click Manage Backups.");
                ui.add_space(10.0);
                act = path_rows(ui, &entries(StoragePart::Backups), true);
            }
            Part::Mail => {
                inspector_title(ui, "Mail and Messages", "Messages and attachments");
                ui.label("Remove large attachments from inside Mail and Messages. Moving these folders breaks the apps.");
                ui.add_space(10.0);
                act = path_rows(ui, &entries(StoragePart::Mail), true);
            }
            Part::Installers => {
                inspector_title(ui, "Installer files", "Disk images and packages");
                if ui.button("Review installers").clicked() {
                    act = Some(Act::Go(Section::Installers));
                }
            }
            Part::Caches => {
                inspector_title(ui, "Caches and logs", "Largest first");
                let mut caches: Vec<_> = state
                    .clean_items
                    .iter()
                    .map(|item| (friendly_name(&item.name, &state.app_names), item.bytes))
                    .collect();
                caches.sort_by_key(|cache| std::cmp::Reverse(cache.1));
                caches.truncate(12);
                names(caches, ui);
                ui.add_space(10.0);
                if ui.button("Review caches").clicked() {
                    act = Some(Act::Go(Section::Clean));
                }
            }
            Part::Trash => {
                inspector_title(ui, "Trash", "Largest items first");
                ui.label("Items in the Trash still use space until you empty it. MacCleanUp never empties the Trash.");
                ui.add_space(10.0);
                act = path_rows(ui, &entries(StoragePart::Trash), true);
                ui.add_space(8.0);
                if ui.button("Open the Trash").clicked() {
                    act = Some(Act::Open(home.join(".Trash")));
                }
            }
            Part::Swap => {
                inspector_title(ui, "Swap and sleep image", "The VM volume");
                ui.label("macOS moves memory it is not using into swap files here, and keeps the sleep image here. It manages this space by itself.");
                if let Some(swap) = state.status.as_ref().and_then(|status| status.swap) {
                    ui.add_space(10.0);
                    fact(ui, "Swap in use", &format!("{} of {}", format_bytes(swap.used), format_bytes(swap.total)));
                }
            }
            Part::Purgeable => {
                inspector_title(ui, "Purgeable", "macOS frees this by itself");
                ui.label("Space macOS gives back when it needs room. Most of it is Time Machine local snapshots: copies of the startup disk that macOS keeps between backups. MacCleanUp does not delete them.");
                ui.add_space(10.0);
                match state.extras.as_ref().and_then(|extras| extras.snapshots.as_ref()) {
                    Some(names) if names.is_empty() => {
                        ui.label(RichText::new("No local snapshots right now.").color(SILT));
                    }
                    Some(names) => {
                        ui.label(RichText::new("Local snapshots").font(medium(13.0)).color(SILT));
                        for name in names {
                            ui.label(snapshot_label(name));
                        }
                    }
                    None => {
                        ui.label(RichText::new("The snapshot list could not be read.").color(SILT));
                    }
                }
            }
            Part::Rest => {
                if state.breakdown.is_none() {
                    inspector_title(ui, "Not measured yet", "Your files and the rest");
                    ui.label("This holds your own files and everything else MacCleanUp has not measured yet.");
                    ui.add_space(10.0);
                    if ui.add_enabled(state.can_start(), egui::Button::new("Measure your files and more")).clicked() {
                        act = Some(Act::Measure);
                    }
                } else {
                    inspector_title(ui, "System Data and other files", "What is left");
                    ui.label("Mostly app data and containers in your Library, the Photos library, iCloud files kept on this Mac, and logs and caches not in the list. MacCleanUp cannot read all of it without Full Disk Access.");
                    ui.add_space(10.0);
                    if ui.button("Open your Library in the disk map").clicked() {
                        act = Some(Act::Disk(home.join("Library")));
                    }
                }
            }
            Part::Free => {
                inspector_title(ui, "Free", "Ready to use");
                ui.label("Space free right now. When it runs low, macOS also frees the Purgeable space.");
            }
        });
        match act {
            Some(Act::Disk(path)) if self.state.can_start() => {
                self.state.select_section(Section::Disk);
                self.state.open_folder(path);
            }
            Some(Act::Reveal(path)) => reveal_in_finder(&path),
            Some(Act::Open(path)) => open_in_finder(&path),
            Some(Act::Go(section)) => self.state.select_section(section),
            Some(Act::Measure) => self.state.measure_storage(),
            _ => {}
        }
    }

    // Inspector -------------------------------------------------------------

    fn inspector(&mut self, ui: &mut Ui) {
        if self.state.section == Section::Storage {
            self.storage_inspector(ui);
            return;
        }
        // A focused item hidden by the filter is not shown.
        let visible = !is_list(self.state.section)
            || self
                .state
                .focus_index()
                .is_some_and(|focus| self.order.contains(&focus));
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
        let mut toggle_data = false;
        egui::ScrollArea::vertical().show(ui, |ui| match state.section {
            section if section.has_clean_items() => {
                let item = &state.items(section).expect("list section")[index];
                let shown = if section == Section::Clean {
                    friendly_name(&item.name, &state.app_names)
                } else {
                    item.name.clone()
                };
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
                fact(ui, "Size", &size_text(app.bytes, app.complete));
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
                let related = related_app_data(&state.app_items, app);
                for entry in &related.paths {
                    ui.label(format!(
                        "{}  {}",
                        tilde_path(&entry.path, &home),
                        format_bytes(entry.bytes)
                    ));
                }
                if related.kept_shared {
                    ui.label(
                        RichText::new("Its caches and app data stay, because another installed copy uses them.")
                            .color(SILT),
                    );
                } else if related.paths.is_empty() {
                    ui.label(RichText::new("No caches or logs were found for this app.").color(SILT));
                }
                if !app.data.is_empty() && !related.kept_shared {
                    ui.add_space(10.0);
                    let total: u64 = app.data.iter().map(|data| data.bytes).sum();
                    let mut include = app.include_data;
                    if ui
                        .checkbox(
                            &mut include,
                            format!("Also move settings and app data ({})", format_bytes(total)),
                        )
                        .clicked()
                    {
                        toggle_data = true;
                    }
                    if !app.include_data {
                        for data in &app.data {
                            ui.label(
                                RichText::new(tilde_path(&data.path, &home))
                                    .small()
                                    .color(SILT),
                            );
                        }
                    }
                }
                ui.add_space(10.0);
                ui.label(RichText::new("Stays on your Mac").font(medium(13.0)).color(SILT));
                ui.label(if app.include_data {
                    "Documents, app containers, and folders not named after this app's bundle ID."
                } else {
                    "Settings, documents, app containers, and other personal data."
                });
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
        if toggle_data {
            self.state.toggle_app_data(index);
        }
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
                    if !busy.mutating {
                        scan_bar(ui, self.state.progress, 200.0);
                    }
                    ui.label(RichText::new(&busy.label).color(if busy.mutating {
                        ORE
                    } else {
                        HEADLAMP
                    }));
                    if busy.mutating {
                        ui.add(egui::Spinner::new().size(14.0).color(HEADLAMP));
                    }
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
    is_list(section) || matches!(section, Section::Disk | Section::Storage)
}

fn is_list(section: Section) -> bool {
    section == Section::Applications || section.has_clean_items()
}

/// One part of the startup disk in the storage bar and the Storage view.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Part {
    MacOs,
    Applications,
    Files,
    Developer,
    Backups,
    Mail,
    Installers,
    Caches,
    Trash,
    Swap,
    Purgeable,
    Rest,
    Free,
}

struct Stratum {
    part: Part,
    label: String,
    bytes: u64,
    color: Color32,
}

/// Bytes of the caches list that sit inside the developer folders, such as
/// DerivedData, simulator caches and the npm cache.
fn developer_caches(state: &State, roots: &[PathBuf]) -> u64 {
    state
        .clean_items
        .iter()
        .filter(|item| roots.iter().any(|root| item.path.starts_with(root)))
        .map(|item| item.bytes)
        .sum()
}

/// The storage bar's parts, in a fixed order with a fixed color each, then
/// what is left and the free space. Parts the app has not measured are left
/// out, and the remainder's label says what it still holds.
fn storage_strata(state: &State, used: u64, available: u64) -> Vec<Stratum> {
    let total = |items: &[CleanItem]| items.iter().map(|item| item.bytes).sum::<u64>();
    let mut parts: Vec<(Part, &str, u64, usize)> = Vec::new();
    if let Some(volumes) = &state.volumes {
        parts.push((Part::MacOs, "macOS", volumes.macos, 8));
    }
    if state.is_loaded(Section::Applications) {
        let apps = state.app_items.iter().map(|app| app.bytes).sum();
        parts.push((Part::Applications, "Applications", apps, 0));
    }
    match &state.breakdown {
        Some(breakdown) => {
            parts.push((Part::Files, "Your files", breakdown.files.bytes, 2));
            // Counted once, under Caches and logs.
            let listed = developer_caches(state, &breakdown.developer_roots);
            parts.push((
                Part::Developer,
                "Developer tools",
                breakdown.developer.bytes.saturating_sub(listed),
                3,
            ));
            if let Some(backups) = breakdown.backups {
                parts.push((Part::Backups, "iPhone and iPad backups", backups.bytes, 4));
            }
            if let Some(mail) = breakdown.mail {
                parts.push((Part::Mail, "Mail and Messages", mail.bytes, 5));
            }
        }
        None if state.is_loaded(Section::Installers) => {
            let installers = total(&state.installer_items);
            parts.push((Part::Installers, "Installer files", installers, 2));
        }
        None => {}
    }
    if state.is_loaded(Section::Clean) {
        parts.push((
            Part::Caches,
            "Caches and logs",
            total(&state.clean_items),
            1,
        ));
    }
    if let Some(trash) = state
        .breakdown
        .as_ref()
        .and_then(|breakdown| breakdown.trash)
    {
        parts.push((Part::Trash, "Trash", trash.bytes, 6));
    }
    if let Some(volumes) = &state.volumes {
        parts.push((Part::Swap, "Swap and sleep image", volumes.vm, 9));
        if let Some(purgeable) = volumes.purgeable {
            parts.push((
                Part::Purgeable,
                "Purgeable: Time Machine snapshots and other space macOS frees by itself",
                purgeable,
                7,
            ));
        }
    }
    let measured: u64 = parts.iter().map(|(_, _, bytes, _)| bytes).sum();
    let mut strata: Vec<Stratum> = parts
        .into_iter()
        .filter(|(_, _, bytes, _)| *bytes > 0)
        .map(|(part, label, bytes, color)| Stratum {
            part,
            label: label.to_string(),
            bytes,
            color: STRATA[color],
        })
        .collect();
    let rest = if state.breakdown.is_some() {
        "System Data and other files"
    } else {
        "Your files, System Data and everything else (not measured yet)"
    };
    strata.push(Stratum {
        part: Part::Rest,
        label: rest.to_string(),
        bytes: used.saturating_sub(measured),
        color: RIDGE,
    });
    strata.push(Stratum {
        part: Part::Free,
        label: "Free".to_string(),
        bytes: available,
        color: SHALE.gamma_multiply(0.6),
    });
    strata
}

/// An Overview row for a search that waits for a click.
fn search_row(
    state: &State,
    section: Section,
    detail: &str,
    find: &'static str,
    review: &'static str,
) -> (Section, String, &'static str) {
    match state.items(section).filter(|_| state.is_loaded(section)) {
        Some(items) => (
            section,
            format!(
                "{} found, {}",
                plural(items.len(), "item", "items"),
                format_bytes(items.iter().map(|item| item.bytes).sum())
            ),
            review,
        ),
        None => (section, format!("{detail} Not scanned yet."), find),
    }
}

fn clean_rows(
    items: &[CleanItem],
    app_names: &HashMap<String, String>,
    home: &Path,
) -> Vec<(usize, String, u64, Option<SystemTime>, String)> {
    items
        .iter()
        .enumerate()
        .map(|(index, item)| {
            (
                index,
                friendly_name(&item.name, app_names),
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
/// through unchanged. `app_names` is keyed by lowercased bundle ID.
fn friendly_name(folder: &str, app_names: &HashMap<String, String>) -> String {
    if let Some(name) = app_names.get(&folder.to_lowercase()) {
        return name.clone();
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
/// Draws the storage bar and returns the part under a click, if any.
fn strata_bar(ui: &mut Ui, strata: &[Stratum], total: u64) -> Option<Part> {
    let (rect, response) =
        ui.allocate_exact_size(Vec2::new(ui.available_width(), 46.0), Sense::click());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, CornerRadius::same(6), SEAM);
    let mut x = rect.left();
    let mut hovered = None;
    for (index, stratum) in strata.iter().enumerate() {
        let width = rect.width() * ratio(stratum.bytes, total);
        let segment = Rect::from_min_max(
            Pos2::new(x, rect.top()),
            Pos2::new(x + width, rect.bottom()),
        );
        painter.rect_filled(segment, CornerRadius::ZERO, stratum.color);
        if response
            .hover_pos()
            .is_some_and(|pos| segment.contains(pos))
        {
            hovered = Some((stratum.label.clone(), stratum.bytes, stratum.part));
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
    let clicked = response.clicked();
    let part = hovered.as_ref().map(|(_, _, part)| *part);
    if let Some((label, bytes, _)) = hovered {
        response.on_hover_text(format!(
            "{label}: {}\nClick to see what it holds",
            format_bytes(bytes)
        ));
    }
    part.filter(|_| clicked)
}

/// One legend row; returns true when it was clicked.
fn legend_row(ui: &mut Ui, color: Color32, label: &str, bytes: u64, total: u64) -> bool {
    ui.horizontal(|ui| {
        let (rect, _) = ui.allocate_exact_size(Vec2::splat(10.0), Sense::hover());
        ui.painter().rect_filled(rect, CornerRadius::same(2), color);
        let name = ui
            .add(egui::Label::new(RichText::new(label).color(CHALK)).sense(Sense::click()))
            .on_hover_text("See what it holds");
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.label(RichText::new(percent(bytes, total)).color(SILT));
            ui.add_space(12.0);
            ui.label(RichText::new(format_bytes(bytes)).font(medium(13.0)));
        });
        name.clicked()
    })
    .inner
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

/// A scan's progress: the share of items finished, and how many files and
/// folders it has read. Before the scan knows its total, the bar animates.
fn scan_bar(ui: &mut Ui, progress: state::ScanProgress, width: f32) {
    let checked = format!("{} checked", grouped(progress.checked));
    let text = match progress.fraction {
        Some(fraction) => format!("{:.0}%   {checked}", fraction * 100.0),
        None if progress.checked == 0 => "Starting…".to_string(),
        None => checked,
    };
    ui.add(
        egui::ProgressBar::new(progress.fraction.unwrap_or(0.0))
            .desired_width(width)
            .fill(HEADLAMP.gamma_multiply(0.55))
            .animate(progress.fraction.is_none())
            .text(RichText::new(text).small().color(CHALK)),
    );
}

/// 1234567 becomes "1,234,567".
fn grouped(count: u64) -> String {
    let digits = count.to_string();
    let mut text = String::new();
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index) % 3 == 0 {
            text.push(',');
        }
        text.push(digit);
    }
    text
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
/// "com.apple.TimeMachine.2026-11-23-092637.local" becomes
/// "2026-11-23 09:26:37". Other names pass through unchanged.
fn snapshot_label(name: &str) -> String {
    let stamp = name
        .strip_prefix("com.apple.TimeMachine.")
        .and_then(|rest| rest.split(".local").next())
        .filter(|stamp| stamp.len() == 17 && stamp.as_bytes()[10] == b'-');
    match stamp {
        Some(stamp) => format!(
            "{} {}:{}:{}",
            &stamp[..10],
            &stamp[11..13],
            &stamp[13..15],
            &stamp[15..17]
        ),
        None => name.to_string(),
    }
}

/// A name and its size on one line.
fn fact_row(ui: &mut Ui, name: &str, bytes: u64) {
    ui.horizontal(|ui| {
        ui.add(egui::Label::new(RichText::new(name).color(CHALK)).truncate());
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.label(RichText::new(format_bytes(bytes)).small().color(SILT));
        });
    });
}

/// Opens a folder in Finder. Runs only on an explicit button press.
fn open_in_finder(path: &Path) {
    if let Ok(mut child) = std::process::Command::new("/usr/bin/open")
        .arg(path)
        .spawn()
    {
        std::thread::spawn(move || child.wait());
    }
}

fn reveal_in_finder(path: &Path) {
    if let Ok(mut child) = std::process::Command::new("/usr/bin/open")
        .arg("-R")
        .arg(path)
        .spawn()
    {
        // Collect the exit status so no finished process lingers.
        std::thread::spawn(move || child.wait());
    }
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
        // Without this the last click leaves a pointer and a hover highlight.
        harness.remove_cursor();
        harness.run_steps(2);
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
            let related = |path: &str, bytes| maccleanup::model::RelatedPath {
                path: home.join(path),
                bytes,
                identity: None,
            };
            app.app_items = vec![maccleanup::model::AppItem {
                name: "Example".to_string(),
                bundle_id: Some("com.example.alpha".to_string()),
                version: Some("2.4".to_string()),
                path: home.join("Applications/Example.app"),
                bytes: 2_000_000,
                complete: true,
                related: vec![related("Library/Caches/com.example.alpha", 4_096)],
                data: vec![related(
                    "Library/Preferences/com.example.alpha.plist",
                    4_096,
                )],
                ..Default::default()
            }];
            app.set_focus(0);
        }
        harness.run_steps(2);
        harness.get_by_label("Version 2.4");
        harness.get_by_label("Uninstall moves");
        harness.get_by_label_contains("Library/Caches/com.example.alpha");
        harness.get_by_label_contains("Preferences/com.example.alpha.plist");
        shot(&mut harness, "applications");

        // App data starts unticked; the checkbox ticks it.
        assert!(!harness.state().state.app_items[0].include_data);
        harness
            .get_by_label_contains("Also move settings and app data")
            .click();
        harness.run_steps(2);
        assert!(harness.state().state.app_items[0].include_data);
    }

    #[test]
    fn storage_bar_names_each_part_and_counts_developer_caches_once() {
        use maccleanup::model::CleanCategory;
        use maccleanup::scanner::Size;
        use maccleanup::system::{Breakdown, VolumeUsage};
        let home = fixture_home();
        let mut state = State::new(home.clone());
        let before = storage_strata(&state, 1_000, 500);
        assert_eq!(before.len(), 2, "nothing measured yet: remainder and free");
        assert!(before[0].label.contains("not measured yet"));
        assert_eq!(before[0].part, Part::Rest);

        state.volumes = Some(VolumeUsage {
            macos: 100,
            data: 800,
            vm: 50,
            purgeable: Some(25),
            ..Default::default()
        });
        let developer = home.join("Library/Developer");
        state.scan(Section::Clean);
        while state.busy.is_some() {
            state.poll_worker();
            std::thread::sleep(Duration::from_millis(5));
        }
        state.clean_items = vec![CleanItem {
            id: "derived".to_string(),
            category: CleanCategory::Developer,
            name: "DerivedData".to_string(),
            path: developer.join("Xcode/DerivedData/App"),
            bytes: 40,
            selected: false,
            complete: true,
            identity: None,
            modified: None,
        }];
        state.breakdown = Some(Breakdown {
            files: Size {
                bytes: 300,
                complete: true,
            },
            developer: Size {
                bytes: 140,
                complete: true,
            },
            developer_roots: vec![developer],
            backups: Some(Size {
                bytes: 60,
                complete: true,
            }),
            mail: None,
            trash: Some(Size {
                bytes: 10,
                complete: true,
            }),
            ..Default::default()
        });
        let strata = storage_strata(&state, 1_000, 500);
        let part = |label: &str| {
            strata
                .iter()
                .find(|stratum| stratum.label.starts_with(label))
                .map(|stratum| stratum.bytes)
        };
        assert_eq!(part("macOS"), Some(100));
        assert_eq!(part("Your files"), Some(300));
        assert_eq!(
            part("Developer tools"),
            Some(100),
            "DerivedData counted under caches"
        );
        assert_eq!(part("Caches and logs"), Some(40));
        assert_eq!(part("iPhone and iPad backups"), Some(60));
        assert_eq!(
            part("Mail and Messages"),
            None,
            "unreadable parts are left out"
        );
        assert_eq!(part("Trash"), Some(10));
        assert_eq!(part("Swap and sleep image"), Some(50));
        assert_eq!(part("Purgeable"), Some(25));
        // 1000 used - (100 + 300 + 100 + 60 + 40 + 10 + 50 + 25) = 315.
        assert_eq!(part("System Data and other files"), Some(315));
        assert_eq!(part("Free"), Some(500));
    }

    #[test]
    fn overview_shows_the_full_storage_breakdown() {
        use maccleanup::scanner::Size;
        use maccleanup::system::{Breakdown, VolumeUsage};
        let mut harness = harness(fixture_home());
        settle(&mut harness);
        settle(&mut harness);
        {
            let state = &mut harness.state_mut().state;
            let total = state.status.as_ref().unwrap().disk.unwrap().total;
            let share = |percent: u64| total / 100 * percent;
            state.volumes = Some(VolumeUsage {
                macos: share(5),
                macos_volumes: vec![
                    ("Macintosh HD".to_string(), share(3)),
                    ("Preboot".to_string(), share(2)),
                ],
                data: share(85),
                vm: share(2),
                purgeable: Some(share(1)),
            });
            let documents = state.home.join("Documents");
            state.breakdown = Some(Breakdown {
                files: Size {
                    bytes: share(40),
                    complete: true,
                },
                developer: Size {
                    bytes: share(20),
                    complete: true,
                },
                developer_roots: Vec::new(),
                backups: Some(Size {
                    bytes: share(6),
                    complete: true,
                }),
                mail: None,
                trash: Some(Size {
                    bytes: share(1),
                    complete: true,
                }),
                entries: vec![maccleanup::system::StorageEntry {
                    part: StoragePart::Files,
                    path: documents,
                    size: Size {
                        bytes: share(40),
                        complete: true,
                    },
                }],
            });
            state.extras = Some(maccleanup::system::StorageExtras {
                trash: None,
                snapshots: Some(vec![
                    "com.apple.TimeMachine.2026-11-23-092637.local".to_string()
                ]),
            });
        }
        harness.run_steps(3);
        harness.get_by_label("Your files");
        harness.get_by_label("Developer tools");
        harness.get_by_label("Swap and sleep image");
        harness.get_by_label("System Data and other files");
        harness.get_by_label_contains("Full Disk Access");
        harness.get_by_label("Measure again");
        shot(&mut harness, "overview-breakdown");

        // A legend row opens that part in the Storage tab.
        harness.get_by_label("Your files").click();
        harness.run_steps(3);
        assert_eq!(harness.state().state.section, Section::Storage);
        assert_eq!(harness.state().storage_focus, Some(Part::Files));
        harness.get_by_label("Largest folders first");
        harness.get_by_label("~/Documents");
        shot(&mut harness, "storage-files");
        harness.state_mut().storage_focus = Some(Part::Purgeable);
        harness.run_steps(2);
        harness.get_by_label("2026-11-23 09:26:37");
        shot(&mut harness, "storage-purgeable");
        harness.state_mut().storage_focus = Some(Part::MacOs);
        harness.run_steps(2);
        harness.get_by_label("Macintosh HD");
    }

    #[test]
    fn a_running_scan_shows_a_progress_bar_with_its_share_and_count() {
        let mut harness = harness(fixture_home());
        settle(&mut harness);
        nav(&mut harness, "Installer files");
        {
            let state = &mut harness.state_mut().state;
            state.busy = Some(state::Busy {
                label: "Looking for installer files".to_string(),
                mutating: false,
                sections: vec![Section::Installers],
            });
            state.progress = state::ScanProgress {
                fraction: Some(0.42),
                checked: 123_456,
            };
        }
        harness.run_steps(3);
        shot(&mut harness, "progress");
        // One bar above the list, one in the status bar.
        assert_eq!(
            harness.query_all_by_label("42%   123,456 checked").count(),
            2
        );
        shot(&mut harness, "progress");
        harness.state_mut().state.progress = state::ScanProgress::default();
        harness.run_steps(2);
        assert!(harness.query_all_by_label("Starting…").count() >= 1);
        harness.state_mut().state.busy = None;
    }

    #[test]
    fn search_sections_wait_for_a_scan_then_list_results() {
        let mut harness = harness(fixture_home());
        settle(&mut harness);
        nav(&mut harness, "Build folders");
        harness.run_steps(2);
        harness.get_by_label("Not scanned yet");
        harness.get_by_label("Scan").click();
        settle(&mut harness);
        // The focused row's name also titles the details panel.
        assert!(harness.query_all_by_label("target").count() >= 1);
        assert!(harness.query_all_by_label("node_modules").count() >= 1);
        harness.get_by_label("~/Documents/proj");
        shot(&mut harness, "build-folders");
        nav(&mut harness, "Large old files");
        harness.run_steps(2);
        harness.get_by_label("Not scanned yet");
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
        let apps = state::app_names(&[maccleanup::model::AppItem {
            name: "Brave Browser".to_string(),
            bundle_id: Some("com.brave.Browser".to_string()),
            path: PathBuf::from("/Applications/Brave Browser.app"),
            ..Default::default()
        }]);
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
        assert_eq!(grouped(0), "0");
        assert_eq!(grouped(999), "999");
        assert_eq!(grouped(1_000), "1,000");
        assert_eq!(grouped(1_234_567), "1,234,567");
        assert_eq!(
            snapshot_label("com.apple.TimeMachine.2026-11-23-092637.local (dataless)"),
            "2026-11-23 09:26:37"
        );
        assert_eq!(snapshot_label("something else"), "something else");
    }
}
