use crate::{
    engine,
    library::{self, Event, Filter, ImportStats, Loaded, Page, Request},
    pgn::{Game, Replay},
};
use eframe::egui::{self, Color32, RichText, Vec2};
use objc2::{AnyThread, rc::Retained};
use objc2_app_kit::NSSound;
use objc2_foundation::{NSData, NSUserDefaults, ns_string};
use shakmaty::{Color, EnPassantMode, Move, Position, Role, Square, fen::Fen};
use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};
// Palette sampled from the app icon; see docs/design.md.
const CLOTH: Color32 = Color32::from_rgb(23, 62, 33);
const GREEN: Color32 = Color32::from_rgb(37, 72, 48);
const CLOTH_SHADOW: Color32 = Color32::from_rgb(5, 35, 14);
const PAPER: Color32 = Color32::from_rgb(247, 236, 221);
const IVORY: Color32 = Color32::from_rgb(245, 233, 214);
const FOLD: Color32 = Color32::from_rgb(203, 185, 155);
const BEVEL: Color32 = Color32::from_rgb(189, 165, 129);
const RIBBON: Color32 = Color32::from_rgb(205, 76, 5);
const RIBBON_LIGHT: Color32 = Color32::from_rgb(239, 116, 33);
const INK: Color32 = Color32::from_rgb(38, 40, 32);
const MUTED: Color32 = Color32::from_rgb(112, 100, 80);

#[derive(Clone, Copy)]
struct PlaySettings {
    human: Color,
    level: u8,
    show_evaluation: bool,
}

/// A game against Stockfish, kept in memory and never saved to the library.
struct Play {
    settings: PlaySettings,
    /// The computer's search and the node it is replying to.
    job: Option<(usize, engine::Job)>,
    error: Option<String>,
    /// The library game that was open, restored when the player leaves.
    stash: (Option<Loaded>, Option<Replay>, usize),
    /// Set when replaying reviewed mistakes instead of playing Stockfish.
    train: Option<Train>,
}

struct Train {
    puzzles: Vec<library::Puzzle>,
    index: usize,
    revealed: bool,
}

pub struct App {
    play: Option<Play>,
    play_setup: Option<PlaySettings>,
    /// The "Train on mistakes" player field while its dialog is open; the name is kept for next time.
    train_setup: bool,
    train_player: String,
    /// The Cmd+P "Jump to game" query, its matches and the highlighted match.
    jump: Option<(String, Vec<library::Row>, usize)>,
    /// The last game id before the running import, so new games can be checked afterwards.
    import_floor: i64,
    /// Imported games still missing details, and the one being filled in (White, Black, Event, Date; true when opened by Edit…).
    missing: Vec<i64>,
    details: Option<(Loaded, [String; 4], bool)>,
    db: rusqlite::Connection,
    path: PathBuf,
    requests: mpsc::Sender<Request>,
    events: mpsc::Sender<Event>,
    receive: mpsc::Receiver<Event>,
    filter: Filter,
    page: Option<Page>,
    cursors: Vec<i64>,
    search_seq: u64,
    load_seq: u64,
    searching: bool,
    loading: bool,
    search_due: Option<Instant>,
    loaded: Option<Loaded>,
    replay: Option<Replay>,
    current: usize,
    scrolled_node: Option<usize>,
    piece_art: egui::TextureHandle,
    bookcloth: egui::TextureHandle,
    flipped: bool,
    sounds_enabled: bool,
    placement_sound: Option<Retained<NSSound>>,
    capture_sound: Option<Retained<NSSound>>,
    selected_square: Option<Square>,
    /// The node the board last showed and when it changed, so pieces glide from there.
    slide: Option<(usize, Instant)>,
    /// The next position change came from a drag, which already put the piece in place.
    dropped: bool,
    promotion: Vec<Move>,
    board_position: Option<(i64, usize)>,
    engine_path: Option<PathBuf>,
    analysis: Option<engine::Job>,
    analysis_position: Option<(i64, usize)>,
    /// Engine lines for the current position, sorted by MultiPV rank.
    lines: Vec<engine::Evaluation>,
    engine_lines: usize,
    /// Height of the move list, set by dragging the rule below it.
    moves_height: f32,
    analysis_error: Option<String>,
    continuous: bool,
    review_job: Option<engine::Job>,
    review_game: Option<i64>,
    review_progress: (usize, usize),
    review_error: Option<String>,
    dirty: bool,
    edited: Instant,
    status: String,
    error: Option<String>,
    importing: bool,
    exporting: bool,
    progress: ImportStats,
    cancel: Arc<AtomicBool>,
}
impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, path: PathBuf, db: rusqlite::Connection) -> Self {
        let ctx = &cc.egui_ctx;
        // Pin light: style() and set_style() act on the active theme, so a dark-mode Mac
        // would otherwise receive this styling in the unused slot.
        ctx.set_theme(egui::ThemePreference::Light);
        let mut style = (*ctx.style()).clone();
        let v = &mut style.visuals;
        v.panel_fill = PAPER;
        v.override_text_color = Some(INK);
        v.selection.bg_fill = Color32::from_rgb(208, 220, 188);
        v.selection.stroke = egui::Stroke::new(1.0_f32, GREEN);
        v.hyperlink_color = RIBBON;
        v.faint_bg_color = Color32::from_rgb(240, 228, 208);
        v.extreme_bg_color = Color32::from_rgb(253, 248, 238);
        v.window_fill = PAPER;
        v.window_stroke = egui::Stroke::new(1.0_f32, BEVEL);
        v.window_corner_radius = egui::CornerRadius::same(8);
        v.window_shadow = egui::Shadow { offset: [0, 10], blur: 28, spread: 0, color: Color32::from_black_alpha(70) };
        v.popup_shadow = egui::Shadow { offset: [0, 4], blur: 12, spread: 0, color: Color32::from_black_alpha(50) };
        // Ivory keys with a carved bevel edge; pressed keys sink into the fold shadow.
        let w = &mut v.widgets;
        w.noninteractive.bg_stroke = egui::Stroke::new(1.0_f32, FOLD);
        w.inactive.weak_bg_fill = Color32::from_rgb(250, 243, 231);
        w.inactive.bg_fill = Color32::from_rgb(236, 224, 204);
        w.inactive.bg_stroke = egui::Stroke::new(1.0_f32, BEVEL);
        w.hovered.weak_bg_fill = Color32::from_rgb(255, 250, 240);
        w.hovered.bg_fill = Color32::from_rgb(240, 229, 210);
        w.hovered.bg_stroke = egui::Stroke::new(1.0_f32, Color32::from_rgb(160, 136, 100));
        w.active.weak_bg_fill = Color32::from_rgb(228, 213, 188);
        w.active.bg_fill = FOLD;
        w.active.bg_stroke = egui::Stroke::new(1.0_f32, Color32::from_rgb(140, 116, 82));
        w.open.weak_bg_fill = Color32::from_rgb(240, 229, 210);
        w.open.bg_stroke = egui::Stroke::new(1.0_f32, BEVEL);
        for state in [&mut w.inactive, &mut w.hovered, &mut w.active, &mut w.open] {
            state.corner_radius = egui::CornerRadius::same(6);
        }
        style.spacing.item_spacing = Vec2::new(10.0, 10.0);
        style.spacing.button_padding = Vec2::new(12.0, 7.0);
        style
            .text_styles
            .insert(egui::TextStyle::Body, egui::FontId::proportional(14.0));
        style
            .text_styles
            .insert(egui::TextStyle::Button, egui::FontId::proportional(14.0));
        ctx.set_style(style);
        let mut fonts = egui::FontDefinitions::default();
        if let Ok(bytes) = std::fs::read("/System/Library/Fonts/Apple Symbols.ttf") {
            fonts
                .font_data
                .insert("symbols".into(), egui::FontData::from_owned(bytes).into());
            fonts
                .families
                .entry(egui::FontFamily::Proportional)
                .or_default()
                .push("symbols".into());
        }
        ctx.set_fonts(fonts);
        let piece_art = image::load_from_memory_with_format(
            include_bytes!("../assets/stone-pieces.png"),
            image::ImageFormat::Png,
        )
        .expect("embedded chess piece atlas must be a valid PNG")
        .into_rgba8();
        debug_assert_eq!(piece_art.dimensions(), (768, 256));
        let piece_art = ctx.load_texture(
            "stone chess pieces",
            egui::ColorImage::from_rgba_unmultiplied([768, 256], piece_art.as_raw()),
            egui::TextureOptions::LINEAR,
        );
        let bookcloth = image::load_from_memory_with_format(
            include_bytes!("../assets/bookcloth.png"),
            image::ImageFormat::Png,
        )
        .expect("embedded bookcloth texture must be a valid PNG")
        .into_luma8();
        let bookcloth = ctx.load_texture(
            "board bookcloth",
            egui::ColorImage::from_gray(
                [bookcloth.width() as usize, bookcloth.height() as usize],
                bookcloth.as_raw(),
            ),
            egui::TextureOptions::LINEAR_MIRRORED_REPEAT
                .with_mipmap_mode(Some(egui::TextureFilter::Linear)),
        );
        let (requests, events, receive) = library::worker(path.clone(), ctx.clone());
        let mut app = Self {
            play: None,
            play_setup: None,
            train_setup: false,
            train_player: String::new(),
            jump: None,
            import_floor: 0,
            missing: Vec::new(),
            details: None,
            db,
            path,
            requests,
            events,
            receive,
            filter: Filter::default(),
            page: None,
            cursors: vec![i64::MAX],
            search_seq: 0,
            load_seq: 0,
            searching: false,
            loading: false,
            search_due: None,
            loaded: None,
            replay: None,
            current: 0,
            scrolled_node: None,
            piece_art,
            bookcloth,
            flipped: false,
            sounds_enabled: !NSUserDefaults::standardUserDefaults()
                .boolForKey(ns_string!("MuteBoardSounds")),
            placement_sound: BoardSound::Placement.load(),
            capture_sound: BoardSound::Capture.load(),
            selected_square: None,
            slide: None,
            dropped: false,
            promotion: vec![],
            board_position: None,
            engine_path: engine::discover(),
            analysis: None,
            analysis_position: None,
            lines: vec![],
            engine_lines: match NSUserDefaults::standardUserDefaults().integerForKey(ns_string!("EngineLines")) {
                n if n < 1 => 3,
                n => (n as usize).min(5),
            },
            moves_height: match NSUserDefaults::standardUserDefaults().doubleForKey(ns_string!("MovesHeight")) {
                h if h < 80.0 => 260.0,
                h => h as f32,
            },
            analysis_error: None,
            continuous: false,
            review_job: None,
            review_game: None,
            review_progress: (0, 0),
            review_error: None,
            dirty: false,
            edited: Instant::now(),
            status: "Your chess, thoughtfully organised.".into(),
            error: None,
            importing: false,
            exporting: false,
            progress: ImportStats::default(),
            cancel: Arc::new(AtomicBool::new(false)),
        };
        app.refresh(false);
        app
    }
    fn refresh(&mut self, reset: bool) {
        if reset {
            self.cursors = vec![i64::MAX];
        }
        self.search_seq += 1;
        self.searching = true;
        if self
            .requests
            .send(Request::Search(
                self.search_seq,
                self.filter.clone(),
                *self.cursors.last().unwrap(),
            ))
            .is_err()
        {
            self.error = Some("The library worker stopped. Restart the app.".into());
            self.searching = false;
        }
        self.search_due = None;
    }
    fn save(&mut self) -> bool {
        if self.play.is_some() {
            self.dirty = false;
            return true;
        }
        if !self.dirty {
            return true;
        }
        if let Some(loaded) = &self.loaded {
            match library::save(&self.db, loaded) {
                Ok(()) => {
                    self.dirty = false;
                    self.status = "Study work saved on this Mac".into();
                }
                Err(e) => {
                    self.error = Some(format!(
                        "Changes have not been saved: {e}. Your edits remain open; retry Save."
                    ));
                    return false;
                }
            }
        }
        true
    }
    fn select(&mut self, id: i64) {
        if !self.save() {
            return;
        }
        if self.loaded.as_ref().is_some_and(|g| g.id == id) {
            self.load_seq += 1;
            self.loading = false;
            return;
        }
        self.load_seq += 1;
        self.loading = true;
        if self
            .requests
            .send(Request::Load(self.load_seq, id))
            .is_err()
        {
            self.loading = false;
            self.error = Some("The library worker stopped.".into());
        }
    }
    fn edit_details(&mut self, id: i64, editing: bool) {
        // Flush pending notes first: the dialog saves its own copy of the game from the database.
        if editing && !self.save() {
            return;
        }
        match library::load(&self.db, id) {
            Ok(loaded) => {
                let known = |key: &str| {
                    let v = loaded.game.tag(key);
                    if v.is_empty() || v.starts_with('?') { String::new() } else { v.to_owned() }
                };
                let fields = [known("White"), known("Black"), known("Event"), known("Date")];
                self.details = Some((loaded, fields, editing));
            }
            Err(e) => self.error = Some(format!("Could not open game: {e}")),
        }
    }
    fn import_files(&mut self, paths: Vec<PathBuf>, ctx: &egui::Context) {
        if paths.is_empty() || self.importing || !self.save() {
            return;
        }
        self.importing = true;
        self.import_floor = library::last_id(&self.db).unwrap_or(i64::MAX);
        self.progress = ImportStats::default();
        self.cancel = Arc::new(AtomicBool::new(false));
        self.status = "Importing games… You can keep browsing.".into();
        library::start_import(
            self.path.clone(),
            paths,
            self.events.clone(),
            ctx.clone(),
            self.cancel.clone(),
        );
    }
    fn choose_import(&mut self, ctx: &egui::Context) {
        if self.importing {
            return;
        }
        if let Some(paths) = rfd::FileDialog::new()
            .add_filter("Chess games", &["pgn"])
            .set_title("Import PGN files")
            .pick_files()
        {
            self.import_files(paths, ctx);
        }
    }
    fn export(&mut self, current: bool) {
        if self.exporting || !self.save() {
            return;
        }
        if let Some(path) = rfd::FileDialog::new()
            .add_filter("PGN", &["pgn"])
            .set_file_name(if current { "game.pgn" } else { "library.pgn" })
            .save_file()
        {
            let db = std::fs::canonicalize(&self.path).unwrap_or(self.path.clone());
            let target = std::fs::canonicalize(&path).unwrap_or(path.clone());
            if target == db
                || target == format!("{}-wal", db.display())
                || target == format!("{}-shm", db.display())
            {
                self.error = Some("Choose a PGN file, not the library database.".into());
                return;
            }
            let (filter, id) = if current {
                (Filter::default(), self.loaded.as_ref().map(|g| g.id))
            } else {
                (self.filter.clone(), None)
            };
            self.exporting = true;
            if self
                .requests
                .send(Request::Export(filter, id, path))
                .is_err()
            {
                self.exporting = false;
                self.error = Some("The library worker stopped.".into());
            }
        }
    }
    fn events(&mut self) {
        while let Ok(event) = self.receive.try_recv() {
            match event {
                Event::Page(seq, result) if seq == self.search_seq || seq == 0 => {
                    self.searching = false;
                    match result {
                        Ok(page) => self.page = Some(page),
                        Err(e) => self.error = Some(format!("Could not search library: {e}")),
                    }
                }
                Event::Loaded(seq, result) if seq == self.load_seq => {
                    self.loading = false;
                    // Save edits made while the next game was loading before replacing the editor.
                    if !self.save() {
                        continue;
                    }
                    match result {
                        Ok(loaded) => {
                            match loaded.game.replay() {
                                Ok(replay) => self.replay = Some(replay),
                                Err(e) => {
                                    self.replay = None;
                                    self.error = Some(format!(
                                        "Cannot display this game: {e}. It is still available for export."
                                    ));
                                }
                            }
                            self.loaded = Some(loaded);
                            self.current = 0;
                            self.scrolled_node = None;
                            self.dirty = false;
                        }
                        Err(e) => self.error = Some(format!("Could not open game: {e}")),
                    }
                }
                Event::Progress(stats) => self.progress = stats,
                Event::Imported(result) => {
                    self.importing = false;
                    match result {
                        Ok(stats) => {
                            self.status = format!(
                                "{} · {} added · {} duplicates skipped",
                                if stats.cancelled {
                                    "Import stopped"
                                } else {
                                    "Import complete"
                                },
                                stats.imported,
                                stats.duplicates
                            )
                        }
                        Err(e) => {
                            self.error = Some(format!(
                                "Import stopped: {e:#}. Completed batches are retained; importing again skips duplicates."
                            ))
                        }
                    }
                    self.missing = library::missing_details(&self.db, self.import_floor).unwrap_or_default();
                    self.refresh(true);
                }
                Event::Exported(result) => {
                    self.exporting = false;
                    match result {
                        Ok(n) => self.status = format!("Exported {n} games with notes"),
                        Err(e) => self.error = Some(format!("Export failed: {e}")),
                    }
                }
                _ => {}
            }
        }
    }
    fn sidebar(&mut self, ctx: &egui::Context) {
        let cloth = self.bookcloth.clone();
        egui::SidePanel::left("library")
            .exact_width(302.0)
            .resizable(false)
            .show_separator_line(false)
            .frame(egui::Frame::new().fill(IVORY).inner_margin(18))
            .show(ctx, |ui| {
                // Carved ivory leaf, set slightly below the cover with a fold at its spine edge.
                let rect = ui.max_rect().expand(18.0);
                material(ui.painter(), &cloth, rect, Color32::from_rgb(240, 227, 206), 300.0);
                top_shadow(ui.painter(), rect, 8);
                for (i, alpha) in [70u8, 35, 15].into_iter().enumerate() {
                    ui.painter().vline(rect.right() - 0.5 - i as f32, rect.y_range(), egui::Stroke::new(1.0_f32, Color32::from_black_alpha(alpha)));
                }
                ui.add_space(8.0);
                ui.label(
                    RichText::new("YOUR LIBRARY")
                        .size(11.0)
                        .strong()
                        .color(GREEN),
                );
                ui.horizontal(|ui| {
                    ui.heading("Games");
                    if let Some(page) = &self.page {
                        ui.label(RichText::new(format!("{}", page.all)).color(GREEN));
                    }
                });
                let response = ui.add(
                    egui::TextEdit::singleline(&mut self.filter.query)
                        .hint_text("Search players, events, notes…")
                        .desired_width(f32::INFINITY),
                );
                if response.changed() {
                    self.search_due = Some(Instant::now());
                }
                ui.horizontal(|ui| {
                    if ui
                        .selectable_label(!self.filter.starred, "All games")
                        .clicked()
                    {
                        self.filter.starred = false;
                        self.refresh(true);
                    }
                    if ui
                        .selectable_label(self.filter.starred, "★ Favourites")
                        .clicked()
                    {
                        self.filter.starred = true;
                        self.refresh(true);
                    }
                });
                let sources = self
                    .page
                    .as_ref()
                    .map(|p| p.sources.clone())
                    .unwrap_or_default();
                let mut changed = false;
                egui::ComboBox::from_id_salt("sources")
                    .width(ui.available_width())
                    .selected_text(if self.filter.source.is_empty() {
                        "All PGN files"
                    } else {
                        &self.filter.source
                    })
                    .show_ui(ui, |ui| {
                        changed |= ui
                            .selectable_value(
                                &mut self.filter.source,
                                String::new(),
                                "All PGN files",
                            )
                            .changed();
                        for (source, count) in sources {
                            changed |= ui
                                .selectable_value(
                                    &mut self.filter.source,
                                    source.clone(),
                                    format!("{source} · {count}"),
                                )
                                .changed();
                        }
                    });
                if changed {
                    self.refresh(true);
                }
                ui.separator();
                let (rows, total, more) = self
                    .page
                    .as_ref()
                    .map(|p| (p.rows.clone(), p.total, p.more))
                    .unwrap_or_default();
                ui.horizontal(|ui| {
                    ui.small(format!("{total} games"));
                    if self.searching {
                        ui.spinner();
                    }
                });
                egui::ScrollArea::vertical()
                    .id_salt("games")
                    .max_height((ui.available_height() - 55.0).max(100.0))
                    .show(ui, |ui| {
                        if rows.is_empty() && !self.searching {
                            ui.add_space(24.0);
                            ui.label(if self.page.as_ref().is_some_and(|p| p.all > 0) {
                                "No matching games.\nTry another search or file."
                            } else {
                                "A home for your chess.\nImport a PGN to get started."
                            });
                        }
                        for row in rows {
                            let selected = self.loaded.as_ref().is_some_and(|g| g.id == row.id);
                            let name = |s: &str| {
                                if s.is_empty() {
                                    "?".to_owned()
                                } else {
                                    s.to_owned()
                                }
                            };
                            let title = format!(
                                "{}{}  —  {}",
                                if row.starred { "★ " } else { "" },
                                name(&row.white),
                                name(&row.black)
                            );
                            let text = format!(
                                "{title}\n{} · {}\n{}   {}",
                                row.event, row.date, row.result, row.eco
                            );
                            let response = ui.add_sized(
                                [ui.available_width(), 76.0],
                                egui::Button::new(RichText::new(text).size(13.0))
                                    .wrap()
                                    .selected(selected)
                                    .frame(selected),
                            );
                            if selected {
                                // A ribbon marks the open game, like a bookmark between pages.
                                let r = response.rect;
                                ribbon(ui.painter(), &cloth, r.left() + 12.0, r.top(), Vec2::new(11.0, 22.0));
                            }
                            if response.clicked() {
                                self.select(row.id);
                            }
                            response.context_menu(|ui| {
                                if ui.button("Edit…").clicked() {
                                    self.edit_details(row.id, true);
                                    ui.close();
                                }
                            });
                            ui.separator();
                        }
                    });
                ui.with_layout(egui::Layout::bottom_up(egui::Align::LEFT), |ui| {
                    ui.horizontal(|ui| {
                        if ui
                            .add_enabled(
                                self.cursors.len() > 1 && !self.searching,
                                egui::Button::new("← Back"),
                            )
                            .clicked()
                        {
                            self.cursors.pop();
                            self.refresh(false);
                        }
                        ui.small(format!("Page {}", self.cursors.len()));
                        if ui
                            .add_enabled(more && !self.searching, egui::Button::new("Next →"))
                            .clicked()
                            && let Some(last) = self.page.as_ref().and_then(|p| p.rows.last())
                        {
                            self.cursors.push(last.id);
                            self.refresh(false);
                        }
                    });
                });
            });
    }
    fn play(&mut self, movement: Move) {
        if self.loading {
            return;
        }
        if let (Some(loaded), Some(replay)) = (&mut self.loaded, &mut self.replay) {
            let before = loaded.game.study.len();
            match loaded.game.play(replay, self.current, movement) {
                Ok(id) => {
                    self.current = id;
                    self.selected_square = None;
                    self.promotion.clear();
                    if loaded.game.study.len() != before {
                        self.dirty = true;
                        self.edited = Instant::now();
                    }
                }
                Err(e) => self.error = Some(e.to_string()),
            }
        }
    }

    fn start_play(&mut self, settings: PlaySettings) {
        if !self.save() {
            return;
        }
        // Drop any library game still loading so it cannot replace the new game.
        self.load_seq += 1;
        self.loading = false;
        let computer = format!("Stockfish level {}", settings.level);
        let mut game = Game::default();
        game.tags = vec![
            ("Event".into(), "Play vs computer".into()),
            ("White".into(), if settings.human == Color::White { "You".into() } else { computer.clone() }),
            ("Black".into(), if settings.human == Color::Black { "You".into() } else { computer }),
            ("Result".into(), "*".into()),
        ];
        // Play on from the position on the board; with no game open, from the start.
        let position = self.replay.as_ref().and_then(|r| r.nodes.get(self.current)?.position.clone());
        let stash = (self.loaded.take(), self.replay.take(), self.current);
        self.show_scratch(game, position.as_ref());
        self.flipped = settings.human == Color::Black;
        self.play = Some(Play { settings, job: None, error: None, stash, train: None });
    }

    /// Shows an unsaved game starting from `position` (the standard start when None).
    fn show_scratch(&mut self, mut game: Game, position: Option<&shakmaty::Chess>) {
        if let Some(position) = position {
            let fen = Fen::from_position(position, EnPassantMode::Legal).to_string();
            game.tags.extend([("SetUp".into(), "1".into()), ("FEN".into(), fen)]);
        }
        self.replay = game.replay().ok();
        self.loaded = Some(Loaded { id: -1, game, notes: Default::default(), starred: false });
        self.current = 0;
    }

    fn start_training(&mut self) {
        let puzzles = match library::mistakes(&self.db, &self.train_player) {
            Ok(puzzles) => puzzles,
            Err(e) => {
                self.error = Some(format!("Could not collect mistakes: {e}"));
                return;
            }
        };
        if puzzles.is_empty() {
            self.error = Some("No mistakes or blunders found. Run Analyze whole game on some games first, or check the player name.".into());
            return;
        }
        if !self.save() {
            return;
        }
        self.load_seq += 1;
        self.loading = false;
        let stash = (self.loaded.take(), self.replay.take(), self.current);
        let settings = PlaySettings { human: Color::White, level: 0, show_evaluation: false };
        self.play = Some(Play { settings, job: None, error: None, stash, train: Some(Train { puzzles, index: 0, revealed: false }) });
        self.show_puzzle(0);
    }

    fn show_puzzle(&mut self, index: usize) {
        let Some(play) = &mut self.play else { return };
        let Some(train) = &mut play.train else { return };
        train.index = index;
        train.revealed = false;
        let puzzle = &train.puzzles[index];
        let mut game = Game::default();
        game.tags = vec![
            ("Event".into(), "Train on mistakes".into()),
            ("White".into(), puzzle.white.clone()),
            ("Black".into(), puzzle.black.clone()),
            ("Result".into(), "*".into()),
        ];
        let position = puzzle.position.clone();
        play.settings.human = position.turn();
        self.flipped = position.turn() == Color::Black;
        self.show_scratch(game, Some(&position));
    }

    fn train_panel(&mut self, ui: &mut egui::Ui) {
        let (Some(play), Some(replay)) = (&mut self.play, &self.replay) else { return };
        let Some(train) = &mut play.train else { return };
        let (index, total) = (train.index, train.puzzles.len());
        let puzzle = &train.puzzles[index];
        section(ui, "Train on mistakes", &format!("Puzzle {} of {total}", index + 1));
        ui.label(format!("{} — {} · {}", puzzle.white, puzzle.black, puzzle.date));
        let side = if puzzle.position.turn() == Color::White { "White" } else { "Black" };
        ui.label(format!("{side} to move. The game went {} here, a {}. Find a better move.", puzzle.played, puzzle.quality.label().to_lowercase()));
        ui.add_space(6.0);
        let parse = |san: &str| san.parse::<shakmaty::san::SanPlus>().ok()?.san.to_move(&puzzle.position).ok();
        let best = puzzle.best.split_whitespace().next().and_then(parse);
        // The board locks after one move, so the board shows either the puzzle or one reply to it.
        if self.current == 0 {
            ui.label(RichText::new("Your move").strong());
        } else {
            let san = &replay.nodes[self.current].san;
            let tried = parse(san);
            if tried.is_some() && tried == best {
                ui.label(RichText::new(format!("Correct — {san} is the engine's move.")).strong());
            } else if tried.is_some() && tried == parse(&puzzle.played) {
                ui.label("That's the move from the game. Try again.");
            } else {
                ui.label(format!("{san} isn't the engine's choice. Try again or show the answer."));
            }
        }
        let mut revealed = train.revealed;
        if revealed {
            ui.label(format!("Engine line: {}", puzzle.best));
        }
        let (mut retry, mut next) = (false, false);
        ui.horizontal(|ui| {
            retry = ui.add_enabled(self.current != 0, egui::Button::new("Try again")).clicked();
            revealed |= ui.add_enabled(!revealed, egui::Button::new("Show answer")).clicked();
            next = ui.button(if index + 1 < total { "Next puzzle" } else { "Finish" }).clicked();
        });
        train.revealed = revealed;
        ui.small("Moves are checked against the review's best move; another strong move may also hold.");
        if retry {
            self.current = 0;
        }
        if next {
            if index + 1 < total {
                self.show_puzzle(index + 1);
            } else {
                self.leave_play();
            }
        }
    }

    fn leave_play(&mut self) {
        if let Some(play) = self.play.take() {
            (self.loaded, self.replay, self.current) = play.stash;
            self.dirty = false;
        }
    }

    fn play_panel(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        self.position_changed();
        self.poll_analysis();
        if self.play.as_ref().is_some_and(|p| p.train.is_some()) {
            return self.train_panel(ui);
        }
        let (Some(play), Some(loaded), Some(replay)) = (&mut self.play, &mut self.loaded, &mut self.replay) else { return };
        let mut moved = None;
        if let Some((node, job)) = &play.job {
            while let Ok(event) = job.receive.try_recv() {
                if let engine::Event::Moved(result) = event {
                    moved = Some((*node, result));
                }
            }
        }
        if let Some((node, result)) = moved {
            play.job = None;
            match result.and_then(|movement| loaded.game.play(replay, node, movement)) {
                // The reply lands even if the player stepped back; the board follows only from the tip.
                Ok(id) if self.current == node => self.current = id,
                Ok(_) => {}
                Err(e) => play.error = Some(e.to_string()),
            }
        }
        let human = play.settings.human;
        section(ui, "Play vs Stockfish", &format!("Level {}", play.settings.level));
        let Some(position) = replay.nodes[self.current].position.clone() else { return };
        let over = position.outcome().known();
        if let Some(path) = &self.engine_path {
            if over.is_none()
                && position.turn() != human
                && replay.nodes[self.current].children.is_empty()
                && play.job.is_none()
                && play.error.is_none()
            {
                play.job = Some((self.current, engine::start_move(path.clone(), position.clone(), play.settings.level, ctx.clone())));
            }
            if play.settings.show_evaluation && over.is_none() && self.analysis.is_none() && self.analysis_error.is_none() {
                self.analysis = Some(engine::start(path.clone(), position.clone(), true, 1, ctx.clone()));
            }
        }
        if let Some(outcome) = over {
            ui.label(RichText::new(match outcome.winner() {
                Some(winner) if winner == human => "Checkmate — you win",
                Some(_) => "Checkmate — Stockfish wins",
                None => "Draw",
            }).strong());
        } else if play.job.is_some() {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label("Stockfish is thinking…");
            });
        } else if position.turn() == human {
            ui.label(RichText::new("Your move").strong());
        } else if play.error.is_none() {
            ui.label("Stockfish to move. Return to the last move to continue.");
        }
        if let Some(error) = play.error.clone() {
            ui.colored_label(Color32::DARK_RED, error);
            if ui.button("Try again").clicked() {
                play.error = None;
            }
        }
        if let Some(error) = &self.analysis_error {
            ui.colored_label(Color32::DARK_RED, error);
        }
        ui.small("Step back and play a different move to take one back.");
    }

    fn position_changed(&mut self) {
        let key = self.loaded.as_ref().map(|g| (g.id, self.current));
        let game = self.loaded.as_ref().map(|g| g.id);
        if self.review_game != game {
            self.review_job = None;
            self.review_error = None;
            self.review_progress = (0, 0);
            self.review_game = game;
        }
        if key != self.board_position {
            if self.sounds_enabled
                && let (Some((game, current)), Some((previous_game, previous)), Some(replay)) =
                    (key, self.board_position, &self.replay)
                && game == previous_game
                && let Some(cue) = move_sound(replay, previous, current)
                && let Some(sound) = match cue {
                    BoardSound::Placement => &self.placement_sound,
                    BoardSound::Capture => &self.capture_sound,
                }
            {
                if sound.isPlaying() {
                    sound.stop();
                }
                sound.play();
            }
            self.selected_square = None;
            self.promotion.clear();
            let dropped = std::mem::take(&mut self.dropped);
            self.slide = match (key, self.board_position) {
                (Some((game, _)), Some((previous_game, previous))) if game == previous_game && !dropped => Some((previous, Instant::now())),
                _ => None,
            };
            self.board_position = key;
        }
        if key != self.analysis_position {
            self.analysis = None;
            self.lines.clear();
            self.analysis_error = None;
            self.analysis_position = key;
        }
    }

    fn poll_analysis(&mut self) {
        let mut finished = None;
        if let Some(job) = &self.analysis {
            while let Ok(event) = job.receive.try_recv() {
                match event {
                    engine::Event::Update(e) => {
                        self.lines.retain(|l| l.multipv != e.multipv);
                        self.lines.push(e);
                        self.lines.sort_by_key(|l| l.multipv);
                    }
                    engine::Event::Finished(result) => finished = Some(result),
                    _ => {}
                }
            }
        }
        if let Some(result) = finished {
            self.analysis = None;
            if let Err(e) = result {
                self.analysis_error = Some(e.to_string());
                self.continuous = false;
            }
        }
    }

    fn engine_panel(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        self.position_changed();
        self.poll_analysis();
        let mut reviewed = None;
        if let Some(job) = &self.review_job {
            while let Ok(event) = job.receive.try_recv() {
                match event {
                    engine::Event::Progress { done, total } => self.review_progress = (done, total),
                    engine::Event::Reviewed(result) => reviewed = Some(result),
                    _ => {}
                }
            }
        }
        if let Some(result) = reviewed {
            self.review_job = None;
            match result {
                Ok(report) => {
                    if let Some(loaded) = &mut self.loaded {
                        loaded.game.review = Some(report);
                        self.dirty = true;
                        self.edited = Instant::now();
                        self.save();
                    }
                }
                Err(e) => self.review_error = Some(e.to_string()),
            }
        }
        section(ui, "Stockfish", &self.lines.first().map(|e| format!("Depth {}", e.depth)).unwrap_or_default());
        let position = self
            .replay
            .as_ref()
            .and_then(|r| r.nodes[self.current].position.clone());
        let terminal = position.as_ref().is_some_and(|p| p.outcome().is_known());
        let available =
            position.is_some() && !terminal && !self.loading && self.engine_path.is_some();
        // Row as tall as a button, so the checkbox centres against the dropdown.
        let row = Vec2::new(ui.available_width(), ui.text_style_height(&egui::TextStyle::Button) + 2.0 * ui.spacing().button_padding.y);
        ui.allocate_ui_with_layout(row, egui::Layout::left_to_right(egui::Align::Center), |ui| {
            if ui
                .add_enabled(
                    self.engine_path.is_some(),
                    egui::Checkbox::new(&mut self.continuous, "Continuous"),
                )
                .on_hover_text("Continuous analysis (L)")
                .changed()
            {
                self.analysis = None;
                self.analysis_error = None;
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let before = self.engine_lines;
                egui::ComboBox::from_id_salt("engine-lines")
                    .width(76.0)
                    .selected_text(format!("{} {}", self.engine_lines, if self.engine_lines == 1 { "line" } else { "lines" }))
                    .show_ui(ui, |ui| {
                        for n in 1..=5 {
                            ui.selectable_value(&mut self.engine_lines, n, n.to_string());
                        }
                    })
                    .response
                    .on_hover_text("How many candidate moves Stockfish calculates and shows as arrows");
                if self.engine_lines != before {
                    NSUserDefaults::standardUserDefaults()
                        .setInteger_forKey(self.engine_lines as _, ns_string!("EngineLines"));
                    self.analysis = None;
                    self.lines.clear();
                }
            });
        });
        ui.horizontal(|ui| {
            if ui
                .add_enabled(
                    available && self.review_job.is_none(),
                    egui::Button::new("Analyze 3 s"),
                )
                .on_hover_text("Analyze this position once for three seconds")
                .clicked()
            {
                self.continuous = false;
                drop(self.analysis.take());
                self.lines.clear();
                self.analysis_error = None;
                self.analysis = Some(engine::start(
                    self.engine_path.clone().unwrap(),
                    position.clone().unwrap(),
                    false,
                    self.engine_lines,
                    ctx.clone(),
                ));
            }
            if let Some(best) = self.lines.first().and_then(|e| e.best)
                && ui
                    .add_enabled(!self.loading, egui::Button::new("Play best"))
                    .clicked()
            {
                self.play(best);
            }
            if self.analysis.is_some() {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("Stop").clicked() {
                        self.continuous = false;
                        self.analysis = None;
                    }
                    ui.spinner();
                });
            }
        });
        if self.continuous && available && self.analysis.is_none() && self.review_job.is_none() {
            self.analysis = Some(engine::start(
                self.engine_path.clone().unwrap(),
                position.unwrap(),
                true,
                self.engine_lines,
                ctx.clone(),
            ));
        }
        if self.continuous && self.review_job.is_some() {
            ui.small("Continuous analysis pauses during game review.");
        }
        if terminal {
            ui.small("Game over in this position.");
        }
        if !self.lines.is_empty() {
            egui::Grid::new("engine-lines").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
                for line in &self.lines {
                    ui.label(RichText::new(&line.score).strong())
                        .on_hover_text("Positive favours White; negative favours Black.");
                    ui.add(egui::Label::new(&line.line).truncate());
                    ui.end_row();
                }
            });
        }
        if let Some(error) = &self.analysis_error {
            ui.colored_label(Color32::DARK_RED, error);
        }
        if self.engine_path.is_none() {
            ui.label("Choose a local Stockfish executable to enable offline analysis.");
            ui.hyperlink_to("Get Stockfish", "https://stockfishchess.org/download/");
        }
        ui.collapsing("Engine settings", |ui| {
            if let Some(path) = &self.engine_path {
                ui.small(path.display().to_string());
            }
            if ui.button("Choose Stockfish…").clicked()
                && let Some(path) = rfd::FileDialog::new()
                    .set_title("Choose the Stockfish executable")
                    .pick_file()
            {
                self.analysis = None;
                self.review_job = None;
                self.lines.clear();
                self.analysis_error = None;
                self.review_error = None;
                self.engine_path = Some(path);
            }
            ui.small("Runs locally on one CPU thread. No game data is uploaded.");
        });
    }

    fn review_panel(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        section(ui, "Game review", "Main line · 0.5 s per move");
        if self.review_job.is_some() {
            let (done, total) = self.review_progress;
            ui.add(
                egui::ProgressBar::new(done as f32 / total.max(1) as f32)
                    .text(format!("{done} / {total} positions")),
            );
            if ui.button("Stop game review").clicked() {
                self.review_job = None;
                self.review_error =
                    Some("Review stopped. Any previous completed report is kept.".into());
            }
        } else if ui
            .add_enabled(
                self.loaded.is_some() && !self.loading && self.engine_path.is_some(),
                egui::Button::new("Analyze whole game"),
            )
            .clicked()
        {
            self.analysis = None;
            self.review_error = None;
            match self.loaded.as_ref().unwrap().game.mainline_positions() {
                Ok(positions) if positions.len() > 1 => {
                    self.review_progress = (0, positions.len());
                    self.review_job = Some(engine::start_review(
                        self.engine_path.clone().unwrap(),
                        positions,
                        ctx.clone(),
                    ));
                }
                Ok(_) => {
                    self.review_error = Some("The original game has no moves to analyze.".into())
                }
                Err(e) => self.review_error = Some(e.to_string()),
            }
        }
        if let Some(error) = &self.review_error {
            ui.colored_label(Color32::DARK_RED, error);
        }
        if let Some(report) = self.loaded.as_ref().and_then(|g| g.game.review.as_ref()) {
            if self.review_job.is_some() {
                ui.small("Previous completed report");
            }
            let right = |ui: &mut egui::Ui, text: RichText| {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| ui.label(text));
            };
            // Three equal columns fill the panel width.
            let column = ((ui.available_width() - 16.0) / 3.0).floor();
            egui::Grid::new("review-summary").striped(true).min_col_width(column).spacing([8.0, 6.0]).show(ui, |ui| {
                ui.label("");
                right(ui, RichText::new("White").strong());
                right(ui, RichText::new("Black").strong());
                ui.end_row();
                ui.label("Accuracy").on_hover_text("Approximate average move accuracy, estimated from loss of winning chances. Analysis time affects the result and deeper analysis may change the verdict; this is not a Chess.com or Lichess game score.");
                for white in [true, false] { right(ui, RichText::new(report.accuracy(white).map(|v| format!("{v:.1}%")).unwrap_or_else(|| "—".into())).strong()); }
                ui.end_row();
                for quality in [engine::Quality::Best, engine::Quality::Good, engine::Quality::Inaccuracy, engine::Quality::Mistake, engine::Quality::Blunder] {
                    ui.label(quality.label());
                    for white in [true, false] { right(ui, RichText::new(report.count(white, quality).to_string())); }
                    ui.end_row();
                }
            });
            ui.collapsing("Moves & mistakes", |ui| {
                if let Some(replay) = &self.replay {
                    for movement in &report.moves {
                        if movement.node >= replay.nodes.len() { continue; }
                        // Darker than the board badges so the text stays readable on paper.
                        let color = match movement.quality {
                            engine::Quality::Blunder => Color32::DARK_RED,
                            engine::Quality::Mistake => Color32::from_rgb(180, 90, 15),
                            engine::Quality::Inaccuracy => Color32::from_rgb(145, 115, 20),
                            _ => GREEN,
                        };
                        let text = format!("{} · {} · {}", replay.label(movement.node), movement.quality.label(), movement.score);
                        if ui.selectable_label(self.current == movement.node, RichText::new(text).color(color))
                            .on_hover_text(format!("Move accuracy {:.1}% · Win-chance loss {:.1} points\nSuggested line: {}", movement.accuracy, movement.loss, movement.best)).clicked() {
                            self.current = movement.node;
                        }
                    }
                }
            });
        }
    }

    fn notebook(&mut self, ctx: &egui::Context) {
        let cloth = self.bookcloth.clone();
        egui::SidePanel::right("notebook")
            .exact_width(310.0)
            .resizable(false)
            .show_separator_line(false)
            .frame(egui::Frame::new().fill(PAPER).inner_margin(16))
            .show(ctx, |ui| {
                // The study notebook: a lighter paper page with a gutter shadow and a bookmark ribbon.
                let rect = ui.max_rect().expand(16.0);
                material(ui.painter(), &cloth, rect, Color32::from_rgb(252, 244, 232), 300.0);
                top_shadow(ui.painter(), rect, 8);
                for (i, alpha) in [80u8, 40, 20, 8].into_iter().enumerate() {
                    ui.painter().vline(rect.left() + 0.5 + i as f32, rect.y_range(), egui::Stroke::new(1.0_f32, Color32::from_black_alpha(alpha)));
                }
                ribbon(ui.painter(), &cloth, rect.right() - 46.0, rect.top(), Vec2::new(18.0, 52.0));
                egui::ScrollArea::vertical()
                    .id_salt("study-desk")
                    .show(ui, |ui| {
                        ui.label(RichText::new("STUDY DESK").size(11.0).strong().color(GREEN));
                        ui.heading("Moves & notes");
                        if self.loaded.is_none() {
                            ui.label(
                                "Select a game to explore its moves and write your own notes.",
                            );
                            return;
                        }
                        if let Some(replay) = &self.replay {
                            let mut next = self.current;
                            let line = replay.line(self.current);
                            egui::ScrollArea::vertical()
                                .id_salt("moves")
                                .max_height(self.moves_height)
                                .auto_shrink([false, false])
                                .show(ui, |ui| {
                                    let start =
                                        ui.selectable_label(self.current == 0, "Starting position");
                                    if start.clicked() {
                                        next = 0;
                                    }
                                    if self.current == 0 && self.scrolled_node != Some(0) {
                                        start.scroll_to_me(Some(egui::Align::Center));
                                    }
                                    // Scoresheet: move number, then White and Black in fixed columns.
                                    let cell = Vec2::new(((ui.available_width() - 44.0) / 2.0).floor(), 0.0);
                                    egui::Grid::new("scoresheet")
                                        .num_columns(3)
                                        .striped(true)
                                        .spacing([4.0, 2.0])
                                        .show(ui, |ui| {
                                            let mut open = false;
                                            for &id in line.iter().skip(1) {
                                                let half = replay.half_move(id);
                                                if !open {
                                                    ui.label(RichText::new(format!("{}.", half / 2 + 1)).color(MUTED));
                                                    if half % 2 == 1 {
                                                        ui.label("");
                                                    }
                                                }
                                                let noted = self.loaded.as_ref().is_some_and(|l| {
                                                    l.notes.get(&id).is_some_and(|n| !n.is_empty())
                                                });
                                                let response = ui.add(
                                                    egui::Button::selectable(self.current == id, replay.nodes[id].san.as_str())
                                                        .min_size(cell)
                                                        .right_text(RichText::new(if noted { "•" } else { "" }).color(RIBBON)),
                                                );
                                                if self.current == id && self.scrolled_node != Some(id) {
                                                    response.scroll_to_me(Some(egui::Align::Center));
                                                }
                                                if response.clicked() {
                                                    next = id;
                                                }
                                                if let Some(loaded) = &mut self.loaded {
                                                    if response.double_clicked() {
                                                        let marks = &mut loaded.game.bookmarks;
                                                        if !marks.remove(&id) { marks.insert(id); }
                                                        self.dirty = true;
                                                        self.edited = Instant::now();
                                                    }
                                                    // A ribbon strip inside the move's left padding marks a bookmark.
                                                    if loaded.game.bookmarks.contains(&id) {
                                                        let strip = egui::Rect::from_min_size(response.rect.left_top(), Vec2::new(3.0, response.rect.height()));
                                                        ui.painter().rect_filled(strip, 1, RIBBON);
                                                    }
                                                }
                                                open = half % 2 == 0;
                                                if !open {
                                                    ui.end_row();
                                                }
                                            }
                                            if open {
                                                ui.end_row();
                                            }
                                        });
                                });
                            let node = &replay.nodes[self.current];
                            if let Some(parent) = node.parent
                                && replay.nodes[parent].children.len() > 1
                            {
                                ui.small("Alternatives at this move");
                                ui.horizontal_wrapped(|ui| {
                                    for &id in &replay.nodes[parent].children {
                                        if ui
                                            .add_enabled(
                                                replay.nodes[id].position.is_some(),
                                                egui::Button::new(&replay.nodes[id].san)
                                                    .selected(id == self.current),
                                            )
                                            .clicked()
                                        {
                                            next = id;
                                        }
                                    }
                                });
                            }
                            if node.children.len() > 1 {
                                ui.small("Continue with");
                                ui.horizontal_wrapped(|ui| {
                                    for &id in &node.children {
                                        if ui
                                            .add_enabled(
                                                replay.nodes[id].position.is_some(),
                                                egui::Button::new(&replay.nodes[id].san),
                                            )
                                            .clicked()
                                        {
                                            next = id;
                                        }
                                    }
                                });
                            }
                            let main = replay.line(0);
                            if !main.contains(&self.current) && ui.button("Return to main line").clicked() {
                                next = main[node.ply.min(main.len() - 1)];
                            }
                            self.scrolled_node = Some(self.current);
                            self.current = next;
                            if let Some(warning) = &replay.warning {
                                ui.colored_label(Color32::from_rgb(163, 91, 36), warning);
                            }
                        }
                        if self.play.is_some() {
                            self.play_panel(ui, ctx);
                        } else {
                            let title = self
                                .replay
                                .as_ref()
                                .filter(|_| self.current > 0)
                                .map(|r| format!("Notes after {}", r.label(self.current)))
                                .unwrap_or_else(|| "Game notes".into());
                            // The rule between moves and notes is a drag handle for the move list height.
                            ui.add_space(6.0);
                            let (rule, drag) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 12.0), egui::Sense::drag());
                            let drag = drag.on_hover_cursor(egui::CursorIcon::ResizeVertical);
                            if drag.dragged() {
                                self.moves_height = (self.moves_height + drag.drag_delta().y).clamp(80.0, 900.0);
                            }
                            if drag.drag_stopped() {
                                NSUserDefaults::standardUserDefaults()
                                    .setDouble_forKey(self.moves_height as _, ns_string!("MovesHeight"));
                            }
                            let color = if drag.hovered() || drag.dragged() { BEVEL } else { FOLD };
                            ui.painter().hline(rule.x_range(), rule.center().y, egui::Stroke::new(1.0_f32, color));
                            ui.painter().rect_filled(egui::Rect::from_center_size(rule.center(), Vec2::new(32.0, 5.0)), 3, color);
                            // Notes autosave shortly after typing; ⌘S saves immediately.
                            header(ui, title, if self.dirty { "Unsaved" } else { "Saved" });
                            if let Some(loaded) = &mut self.loaded {
                                let note = loaded.notes.entry(self.current).or_default();
                                let sheet = ui.painter().add(egui::Shape::Noop);
                                let response = ui.add(
                                    egui::TextEdit::multiline(note)
                                        .id_salt((loaded.id, self.current))
                                        .hint_text("Plans, ideas, things to remember…")
                                        .desired_width(f32::INFINITY)
                                        .desired_rows(5)
                                        .frame(false)
                                        .margin(egui::Margin { left: 14, right: 6, top: 4, bottom: 4 }),
                                );
                                // Ruled notepaper behind the text: rules sit on each text baseline.
                                let rect = response.rect;
                                let row = ui.fonts_mut(|f| f.row_height(&egui::TextStyle::Body.resolve(ui.style())));
                                let focused = response.has_focus();
                                let mut shapes = vec![
                                    egui::Shape::rect_filled(rect, 4, ui.visuals().extreme_bg_color),
                                    egui::Shape::vline(rect.left() + 9.5, rect.y_range(), egui::Stroke::new(1.0_f32, RIBBON.gamma_multiply(0.45))),
                                ];
                                let mut y = rect.top() + 4.0 + row;
                                while y < rect.bottom() - 2.0 {
                                    shapes.push(egui::Shape::hline(rect.x_range().shrink(2.0), y.round() + 0.5, egui::Stroke::new(1.0_f32, FOLD.gamma_multiply(0.7))));
                                    y += row;
                                }
                                shapes.push(egui::Shape::rect_stroke(rect, 4, egui::Stroke::new(1.0_f32, if focused { GREEN } else { BEVEL }), egui::StrokeKind::Inside));
                                ui.painter().set(sheet, egui::Shape::Vec(shapes));
                                top_shadow(ui.painter(), rect.shrink(1.0), 3);
                                if response.changed() {
                                    self.dirty = true;
                                    self.edited = Instant::now();
                                }
                            }
                            if let Some(replay) = &self.replay {
                                for annotation in &replay.nodes[self.current].annotations {
                                    if let crate::pgn::Token::Comment(text) = annotation {
                                        ui.label(text);
                                    }
                                }
                            }
                            self.engine_panel(ui, ctx);
                            self.review_panel(ui, ctx);
                        }
                        ui.add_space(6.0);
                        ui.separator();
                        ui.collapsing("Game details & PGN", |ui| {
                            if let Some(loaded) = &self.loaded {
                                for (key, value) in &loaded.game.tags {
                                    ui.label(format!("{key}: {value}"));
                                }
                                let mut pgn = loaded
                                    .game
                                    .export(&loaded.notes)
                                    .unwrap_or_else(|e| format!("Cannot export this game: {e}"));
                                ui.add(
                                    egui::TextEdit::multiline(&mut pgn)
                                        .font(egui::TextStyle::Monospace)
                                        .desired_width(f32::INFINITY)
                                        .interactive(false),
                                );
                            }
                        });
                    });
            });
    }
    fn board(&mut self, ctx: &egui::Context) {
        let cloth = self.bookcloth.clone();
        egui::CentralPanel::default().frame(egui::Frame::new().fill(PAPER).inner_margin(24)).show(ctx,|ui| {
            let desk = ui.max_rect().expand(24.0);
            material(ui.painter(), &cloth, desk, PAPER, 300.0);
            top_shadow(ui.painter(), desk, 10);
            if self.loaded.is_none() {
                ui.add_space(60.0);
                ui.label(RichText::new("Every game has\nsomething to teach.").size(34.0).strong());
                ui.add_space(16.0); ui.label("Bring your games together. Revisit the critical\npositions. Keep your thinking alongside the moves.");
                ui.add_space(24.0);
                if ui.add(egui::Button::new(RichText::new("Import PGN files").color(Color32::WHITE)).fill(RIBBON).stroke(egui::Stroke::new(1.0_f32, RIBBON_LIGHT))).clicked() { self.choose_import(ctx); }
                if ui.button("Try a sample game").clicked() {
                    let path = std::env::temp_dir().join("chesslib-sample.pgn");
                    match std::fs::write(&path,include_str!("../fixtures/sample.pgn")) { Ok(()) => self.import_files(vec![path],ctx), Err(e) => self.error=Some(e.to_string()) }
                }
                ui.add_space(24.0); ui.small("Drag PGN files into this window · ⌘O to import\nYour library stays on this Mac.");
                return;
            }
            let loaded = self.loaded.as_ref().unwrap();
            let title = format!("{} — {}",loaded.game.tag("White"),loaded.game.tag("Black"));
            let subtitle = match &self.play {
                Some(play) if play.train.is_some() => "Train on mistakes  ·  find the better move".into(),
                Some(play) => format!("Play vs computer  ·  you play {}", if play.settings.human == Color::White { "White" } else { "Black" }),
                None => format!("{}  ·  {}  ·  {}",loaded.game.tag("Event"),loaded.game.tag("Date"),loaded.game.tag("Result")),
            };
            ui.label(RichText::new("GAME WORKSPACE").size(11.0).strong().color(GREEN));
            ui.horizontal(|ui| {
                ui.heading(title);
                let starred = self.loaded.as_ref().unwrap().starred;
                if self.play.is_none() && ui.add(egui::Button::new(RichText::new(if starred { "★" } else { "☆" }).color(if starred { RIBBON } else { MUTED }))).on_hover_text("Favourite this game").clicked() {
                    let loaded = self.loaded.as_mut().unwrap(); loaded.starred = !loaded.starred; self.dirty=true; self.edited=Instant::now();
                    if self.save() { self.refresh(false); }
                }
            });
            ui.label(RichText::new(subtitle).size(13.0).color(MUTED));
            ui.add_space(14.0);
            if self.loading { ui.spinner(); }
            let Some(replay) = &self.replay else { ui.label("Board unavailable for this game. You can still add game notes and export it."); return; };
            let pos = replay.nodes[self.current].position.as_ref().unwrap();
            // Layout: evaluation bar, a gap, then the clothbound cover with a 22px binding margin
            // that carries the coordinates around the squares.
            const BIND: f32 = 22.0;
            let side = (ui.available_width()-64.0-BIND).min(ui.available_height()-125.0-2.0*BIND).max(240.0);
            let cell = side/8.0;
            // Centre the squares in the desk; the evaluation bar hangs to their left, so a narrow window
            // shifts the board right just enough to keep the bar on screen.
            let (row,_) = ui.allocate_exact_size(Vec2::new(ui.available_width(),side+2.0*BIND),egui::Sense::hover());
            let left = (row.center().x - side/2.0 - 64.0).max(row.left());
            let rect = egui::Rect::from_min_size(egui::pos2(left, row.top()), Vec2::new(side+64.0+BIND,side+2.0*BIND));
            let origin = rect.min+Vec2::new(64.0,BIND);
            let squares = egui::Rect::from_min_size(origin, Vec2::splat(side));
            let cover = squares.expand(BIND);
            let painter = ui.painter();
            painter.add(egui::Shadow { offset: [0, 10], blur: 26, spread: 0, color: Color32::from_black_alpha(80) }.as_shape(cover, 6));
            painter.add(egui::Shadow { offset: [0, 2], blur: 3, spread: 0, color: Color32::from_black_alpha(70) }.as_shape(cover, 6));
            material(painter, &cloth, cover, CLOTH, 150.0);
            painter.rect_stroke(cover, 2, egui::Stroke::new(1.0_f32, CLOTH_SHADOW), egui::StrokeKind::Inside);
            painter.hline(cover.x_range().shrink(2.0), cover.top() + 1.5, egui::Stroke::new(1.0_f32, Color32::from_white_alpha(28)));
            // Squares sit in a shallow recess pressed into the cover.
            painter.rect_stroke(squares, 0, egui::Stroke::new(3.0_f32, CLOTH_SHADOW), egui::StrokeKind::Outside);
            painter.hline(squares.x_range().expand(3.0), squares.bottom() + 3.5, egui::Stroke::new(1.0_f32, Color32::from_white_alpha(30)));
            if self.play.as_ref().is_none_or(|p| p.settings.show_evaluation) {
            let saved_score = self.loaded.as_ref().and_then(|g| g.game.review.as_ref())
                .and_then(|report| report.moves.iter().find(|m| m.node == self.current)).map(|m| m.score.as_str());
            let score = self.lines.first().map(|e| e.score.as_str()).or(saved_score);
            let evaluation = evaluation_bar_value(pos, score);
            let bar = egui::Rect::from_min_size(egui::pos2(rect.min.x + 4.0, origin.y), Vec2::new(28.0, side));
            let bar_label = evaluation.as_ref().map(|(_, label)| format!("Evaluation: {label}. Positive favours White; negative favours Black."))
                .unwrap_or_else(|| "No evaluation for this position. Run analysis to see the advantage.".into());
            let response = ui.interact(bar, ui.id().with("evaluation-bar"), egui::Sense::hover());
            response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Label, true, &bar_label));
            response.on_hover_text(bar_label);
            let painter = ui.painter();
            // A groove cut into the desk: lit lower edge outside, shadow inside the top.
            painter.rect_stroke(bar.expand(1.0), 4, egui::Stroke::new(1.0_f32, Color32::from_white_alpha(140)), egui::StrokeKind::Outside);
            if let Some((white_share, label)) = evaluation {
                painter.rect_filled(bar, 0.0, Color32::from_rgb(44, 44, 40));
                let white_height = bar.height() * ctx.animate_value_with_time(ui.id().with("evaluation-bar-fill"), white_share, 0.4);
                let white_top = if self.flipped { bar.top() } else { bar.bottom() - white_height };
                painter.rect_filled(egui::Rect::from_min_size(egui::pos2(bar.left(), white_top), Vec2::new(bar.width(), white_height)), 0.0, IVORY);
                let white_advantage = white_share >= 0.5;
                let at_top = white_advantage == self.flipped;
                let label_pos = egui::pos2(bar.center().x, if at_top { bar.top()+12.0 } else { bar.bottom()-12.0 });
                let font_size = if label.len() > 5 { 9.0 } else { 11.0 };
                painter.text(label_pos, egui::Align2::CENTER_CENTER, label, egui::FontId::proportional(font_size), if white_advantage { INK } else { Color32::WHITE });
                painter.line_segment([egui::pos2(bar.left(),bar.center().y), egui::pos2(bar.right(),bar.center().y)], egui::Stroke::new(1.0_f32,Color32::from_rgb(150,150,145)));
            } else {
                painter.rect_filled(bar, 0.0, Color32::from_rgb(232, 218, 194));
                painter.text(bar.center(), egui::Align2::CENTER_CENTER, "—", egui::FontId::proportional(12.0), INK);
            }
            top_shadow(painter, bar, 4);
            painter.vline(bar.left() + 0.5, bar.y_range(), egui::Stroke::new(1.0_f32, Color32::from_black_alpha(40)));
            painter.rect_stroke(bar, 0.0, egui::Stroke::new(1.0_f32, BEVEL), egui::StrokeKind::Outside);
            }
            let painter = ui.painter();
            let legal = pos.legal_moves();
            // Against the computer, only the player's own pieces move.
            let can_move = !self.loading && self.play.as_ref().is_none_or(|p| p.settings.human == pos.turn());
            let mut clicked = None;
            let mut dragged = None;
            let mut drop = false;
            // Atlas cell and lift; shorter pieces sit low in the atlas because the bases share a baseline.
            let piece_uv = |piece: shakmaty::Piece| {
                let (column, lift) = match piece.role {
                    Role::King => (0, 0.0),
                    Role::Queen => (1, 0.04),
                    Role::Bishop => (2, 0.05),
                    Role::Knight => (3, 0.05),
                    Role::Rook => (4, 0.07),
                    Role::Pawn => (5, 0.09),
                };
                let top = if piece.color == Color::White { 0.0 } else { 0.5 };
                (egui::Rect::from_min_max(egui::pos2(column as f32 / 6.0, top), egui::pos2((column + 1) as f32 / 6.0, top + 0.5)), lift)
            };
            let center = |s: Square| {
                let (file, rank) = (s.file() as usize, s.rank() as usize);
                let (col, row) = if self.flipped { (7 - file, rank) } else { (file, 7 - rank) };
                origin + Vec2::new((col as f32 + 0.5) * cell, (row as f32 + 0.5) * cell)
            };
            let draw_piece = |piece: shakmaty::Piece, at: egui::Pos2| {
                let (uv, lift) = piece_uv(piece);
                painter.image(self.piece_art.id(), egui::Rect::from_center_size(at, Vec2::splat(cell)).translate(Vec2::new(0.0, -cell * lift)), uv, Color32::WHITE);
            };
            // Each piece that arrived on a square glides from the nearest square it left.
            const SLIDE: f32 = 0.2;
            let mut sliding = Vec::new();
            let mut progress = 1.0;
            if let Some((node, started)) = self.slide
                && let Some(before) = replay.nodes.get(node).and_then(|n| n.position.as_ref())
                && started.elapsed().as_secs_f32() < SLIDE
            {
                let t = started.elapsed().as_secs_f32() / SLIDE;
                progress = 1.0 - (1.0 - t).powi(3);
                let (old, new) = (before.board(), pos.board());
                for (to, piece) in new.clone() {
                    if old.piece_at(to) == Some(piece) { continue; }
                    let from = old.clone().into_iter().filter(|&(s, p)| p == piece && new.piece_at(s) != Some(piece)).map(|(s, _)| s).min_by_key(|s| s.distance(to));
                    if let Some(from) = from { sliding.push((piece, from, to, old.piece_at(to))); }
                }
                ctx.request_repaint();
            }
            for row in 0..8 { for col in 0..8 {
                let file = if self.flipped { 7-col } else { col };
                let rank = if self.flipped { row } else { 7-row };
                let square = Square::new((rank*8+file) as u32);
                let min = origin+Vec2::new(col as f32*cell,row as f32*cell);
                let square_rect = egui::Rect::from_min_size(min,Vec2::splat(cell));
                let changed = replay.nodes[self.current].parent.and_then(|id| replay.nodes[id].position.as_ref())
                    .is_some_and(|previous| previous.board().piece_at(square) != pos.board().piece_at(square));
                let tint = match (self.selected_square == Some(square), changed, (row + col) % 2 == 0) {
                    (true, _, _) => Color32::from_rgb(250, 213, 100),
                    (_, true, true) => Color32::from_rgb(241, 239, 169),
                    (_, true, false) => Color32::from_rgb(181, 193, 119),
                    (_, false, true) => Color32::from_rgb(255, 250, 231),
                    (_, false, false) => Color32::from_rgb(113, 156, 122),
                };
                let cloth_uv = egui::Rect::from_min_size(
                    egui::pos2(col as f32 * 0.5, row as f32 * 0.5),
                    Vec2::splat(0.5),
                );
                painter.image(self.bookcloth.id(), square_rect, cloth_uv, tint);
                let destination = self.selected_square.is_some_and(|from| legal.iter().any(|m| m.from() == Some(from) && move_destination(*m) == square));
                if destination { painter.circle_filled(square_rect.center(), cell * 0.12, Color32::from_black_alpha(75)); }
                let piece = pos.board().piece_at(square);
                let square_label = piece.map(|piece| format!("{square}: {:?} {:?}", piece.color, piece.role)).unwrap_or_else(|| format!("{square}: empty"));
                let response = ui.interact(square_rect, ui.id().with(("square", square)), egui::Sense::click_and_drag());
                response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, !self.loading, &square_label));
                let own_piece = piece.filter(|p| p.color == pos.turn() && can_move);
                if response.drag_started() && own_piece.is_some() { self.selected_square = Some(square); }
                if self.selected_square == Some(square) && own_piece.is_some() {
                    if response.dragged() { dragged = own_piece; }
                    // A drop is handled as a click on the square under the pointer.
                    if response.drag_stopped() && let Some(p) = ctx.pointer_interact_pos() && squares.contains(p) {
                        let (c, r) = ((((p.x - origin.x) / cell) as usize).min(7), (((p.y - origin.y) / cell) as usize).min(7));
                        let (f, rk) = if self.flipped { (7 - c, r) } else { (c, 7 - r) };
                        clicked = Some(Square::new((rk * 8 + f) as u32));
                        drop = true;
                    }
                }
                if response.clicked() { clicked = Some(square); }
                // Coordinates stamped into the cover cloth.
                let coordinate = Color32::from_rgb(214, 202, 176);
                if col==0 { painter.text(min-Vec2::new(BIND/2.0+1.5,-cell/2.0),egui::Align2::CENTER_CENTER,(rank+1).to_string(),egui::FontId::proportional(11.0),coordinate); }
                if row==7 { painter.text(min+Vec2::new(cell/2.0,cell+BIND/2.0+1.5),egui::Align2::CENTER_CENTER,((b'a'+file as u8) as char).to_string(),egui::FontId::proportional(11.0),coordinate); }
                // A captured piece stays put until the capturer lands on it.
                let arriving = sliding.iter().find(|s| s.2 == square);
                let shown = match arriving { Some(s) => s.3, None => piece };
                if let Some(piece) = shown && !(dragged.is_some() && self.selected_square == Some(square)) {
                    draw_piece(piece, square_rect.center());
                }
            }}
            // Drawn after the squares so a gliding piece passes over the others.
            for &(piece, from, to, _) in &sliding {
                draw_piece(piece, center(from) + (center(to) - center(from)) * progress);
            }
            // Returns the arrow's tail and the base of its head, for placing a label between them.
            let arrow = |movement: Move, color: Color32, head: f32, width: f32| {
                let (start, end) = (center(movement.from().unwrap_or(movement.to())), center(move_destination(movement)));
                let dir = (end - start).normalized();
                let normal = dir.rot90();
                let base = end - dir * head;
                painter.line_segment([start + dir * cell * 0.2, base], egui::Stroke::new(width, color));
                painter.add(egui::Shape::convex_polygon(vec![end, base + normal * head * 0.6, base - normal * head * 0.6], color, egui::Stroke::NONE));
                (start, base)
            };
            // Game review verdict on the move just played; for a poor move, also the move the engine preferred.
            let parse = |before: &shakmaty::Chess, san: &str| san.parse::<shakmaty::san::SanPlus>().ok()?.san.to_move(before).ok();
            let verdict = self.loaded.as_ref()
                .and_then(|l| l.game.review.as_ref())
                .and_then(|r| r.moves.iter().find(|m| m.node == self.current))
                .zip(replay.nodes[self.current].parent.and_then(|p| replay.nodes[p].position.as_ref()))
                .and_then(|(review, before)| {
                    let played = parse(before, &replay.nodes[self.current].san)?;
                    let suggested = review.best.split_whitespace().next().and_then(|san| parse(before, san)).filter(|&m| m != played);
                    Some((review.quality, move_destination(played), suggested))
                });
            if let Some((quality, _, Some(suggested))) = verdict
                && matches!(quality, engine::Quality::Inaccuracy | engine::Quality::Mistake | engine::Quality::Blunder)
            {
                // Deep bookcloth green reads on both the ivory and the green squares.
                arrow(suggested, Color32::from_rgba_unmultiplied(CLOTH.r(), CLOTH.g(), CLOTH.b(), 190), cell * 0.32, cell * 0.11);
            }
            // Engine lines as arrows over the pieces, weakest first so the best sits on top.
            // The legality check guards a stale evaluation.
            for line in self.lines.iter().rev().filter(|_| self.play.is_none()) {
                let Some(best) = line.best.filter(|m| legal.contains(m)) else { continue };
                let top = line.multipv == 1;
                let (head, width) = if top { (cell * 0.4, cell * 0.16) } else { (cell * 0.32, cell * 0.11) };
                let color = if top { Color32::from_rgba_unmultiplied(40, 110, 200, 180) } else { Color32::from_rgba_unmultiplied(40, 110, 200, 100) };
                let (start, base) = arrow(best, color, head, width);
                let galley = painter.layout_no_wrap(line.score.clone(), egui::FontId::proportional((cell * 0.2).clamp(10.0, 14.0)), Color32::WHITE);
                let label = egui::Rect::from_center_size(start + (base - start) * 0.5, galley.size() + Vec2::new(8.0, 2.0));
                painter.rect_filled(label, 4, Color32::from_rgba_unmultiplied(25, 70, 140, if top { 230 } else { 190 }));
                painter.galley(label.min + Vec2::new(4.0, 1.0), galley, Color32::WHITE);
            }
            if let Some((quality, square, _)) = verdict {
                let (color, symbol) = quality_style(quality);
                let radius = cell * 0.13;
                let at = center(square) + Vec2::new(cell * 0.5 - radius * 0.9, -cell * 0.5 + radius * 0.9);
                painter.circle_filled(at + Vec2::new(0.0, 1.5), radius, Color32::from_black_alpha(50));
                painter.circle(at, radius, color, egui::Stroke::new(1.5_f32, IVORY));
                // Star and tick are drawn as shapes so they sit exactly in the centre.
                match quality {
                    engine::Quality::Best => {
                        let point = |i: usize, r: f32| {
                            let angle = std::f32::consts::PI * (i as f32 / 5.0 - 0.5);
                            at + Vec2::angled(angle) * r + Vec2::new(0.0, radius * 0.06)
                        };
                        let (outer, inner) = (radius * 0.62, radius * 0.25);
                        // A concave star as a convex pentagon plus five point triangles.
                        painter.add(egui::Shape::convex_polygon((0..5).map(|i| point(2 * i + 1, inner)).collect(), IVORY, egui::Stroke::NONE));
                        for i in 0..5 {
                            painter.add(egui::Shape::convex_polygon(vec![point((2 * i + 9) % 10, inner), point(2 * i, outer), point(2 * i + 1, inner)], IVORY, egui::Stroke::NONE));
                        }
                    }
                    engine::Quality::Good => {
                        let tick = [Vec2::new(-0.42, 0.02), Vec2::new(-0.12, 0.32), Vec2::new(0.42, -0.28)].map(|v| at + v * radius);
                        painter.add(egui::Shape::line(tick.to_vec(), egui::Stroke::new(radius * 0.2, IVORY)));
                    }
                    _ => {
                        painter.text(at, egui::Align2::CENTER_CENTER, symbol, egui::FontId::proportional(radius * 1.1), IVORY);
                    }
                }
            }
            if let Some(piece) = dragged && let Some(p) = ctx.pointer_interact_pos() {
                draw_piece(piece, p);
                ctx.set_cursor_icon(egui::CursorIcon::Grabbing);
            }
            ui.add_space(8.0);
            // Controls line up under the board's left edge.
            let below = egui::Rect::from_min_max(egui::pos2(cover.left(), ui.cursor().top()), ui.max_rect().max);
            ui.scope_builder(egui::UiBuilder::new().max_rect(below), |ui| {
            let line = replay.line(self.current);
            let last = *line.last().unwrap();
            let parent = replay.nodes[self.current].parent;
            let next = replay.next(self.current);
            let move_number = replay.nodes[self.current].ply;
            let line_length = replay.nodes[last].ply;
            let fen = Fen::from_position(pos,EnPassantMode::Legal).to_string();
            ui.horizontal(|ui| {
                if ui.add_enabled(self.current>0,egui::Button::new("|←")).on_hover_text("Starting position · Home").clicked() { self.current=0; }
                if ui.add_enabled(parent.is_some(),egui::Button::new("←")).on_hover_text("Previous move · Left arrow").clicked() { self.current=parent.unwrap_or(0); }
                ui.label(format!("{move_number} / {line_length}"));
                if ui.add_enabled(next.is_some(),egui::Button::new("→")).on_hover_text("Next move · Right arrow").clicked() { self.current=next.unwrap_or(self.current); }
                if ui.add_enabled(self.current!=last,egui::Button::new("→|")).on_hover_text("Last position · End").clicked() { self.current=last; }
                if ui.button("Flip").on_hover_text("Flip the board (F)").clicked() { self.flipped = !self.flipped; }
                if ui.button("Copy FEN").on_hover_text(&fen).clicked() {
                    ctx.copy_text(fen.clone());
                    self.status = "Position copied".into();
                }
            });
            // Evaluation graph from the game review: ivory rises with White's advantage, like the bar beside the board.
            let points: Vec<(usize, f32, String, engine::Quality)> = self.loaded.as_ref().and_then(|l| l.game.review.as_ref())
                .map(|review| review.moves.iter().filter_map(|m| {
                    let (share, label) = evaluation_bar_value(replay.nodes.get(m.node)?.position.as_ref()?, Some(&m.score))?;
                    Some((m.node, share, label, m.quality))
                }).collect())
                .unwrap_or_default();
            if points.is_empty() {
                if self.play.is_none() { ui.small("Run a game review to see the evaluation graph."); }
            } else {
                ui.add_space(6.0);
                let (graph, response) = ui.allocate_exact_size(Vec2::new(cover.width(), 80.0), egui::Sense::click());
                response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Other, true, "Evaluation graph. Click to jump to a move."));
                let painter = ui.painter_at(graph);
                let span = (points.len() as f32 - 1.0).max(1.0);
                let at = |i: usize| egui::pos2(graph.left() + graph.width() * i as f32 / span, graph.bottom() - graph.height() * points[i].1);
                painter.rect_filled(graph, 0, Color32::from_rgb(44, 44, 40));
                for i in 1..points.len() {
                    let (a, b) = (at(i - 1), at(i));
                    painter.add(egui::Shape::convex_polygon(vec![egui::pos2(a.x, graph.bottom()), a, b, egui::pos2(b.x, graph.bottom())], IVORY, egui::Stroke::NONE));
                }
                painter.hline(graph.x_range(), graph.center().y, egui::Stroke::new(1.0_f32, Color32::from_rgb(150, 150, 145)));
                painter.add(egui::Shape::line((0..points.len()).map(at).collect(), egui::Stroke::new(1.5_f32, RIBBON)));
                for (i, point) in points.iter().enumerate() {
                    if matches!(point.3, engine::Quality::Inaccuracy | engine::Quality::Mistake | engine::Quality::Blunder) {
                        painter.circle(at(i), 3.0, quality_style(point.3).0, egui::Stroke::new(1.0_f32, IVORY));
                    }
                }
                if let Some(i) = points.iter().position(|p| p.0 == self.current) {
                    painter.vline(at(i).x, graph.y_range(), egui::Stroke::new(1.0_f32, RIBBON));
                }
                let hovered = response.hover_pos().map(|p| (((p.x - graph.left()) / graph.width() * span).round().max(0.0) as usize).min(points.len() - 1));
                if let Some(i) = hovered {
                    painter.circle_filled(at(i), 4.5, RIBBON);
                    let (id, _, label, quality) = &points[i];
                    let text = format!("{} [{}]\nAdvantage: {label}", replay.label(*id), quality.label());
                    if response.on_hover_text_at_pointer(text).clicked() { self.current = *id; }
                }
                ui.painter().rect_stroke(graph, 0, egui::Stroke::new(1.0_f32, BEVEL), egui::StrokeKind::Outside);
            }
            });
            if let Some(square) = clicked && can_move {
                if let Some(from) = self.selected_square {
                    let candidates: Vec<_> = legal.iter().copied().filter(|m| m.from() == Some(from) && move_destination(*m) == square).collect();
                    if candidates.len() == 1 { self.dropped = drop; self.play(candidates[0]); }
                    else if candidates.len() > 1 { self.promotion = candidates; }
                    else { self.selected_square = (square != from && pos.board().piece_at(square).is_some_and(|p| p.color == pos.turn())).then_some(square); }
                } else {
                    self.selected_square = pos.board().piece_at(square).filter(|p| p.color == pos.turn()).map(|_| square);
                }
            }
        });
    }
}
impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.events();
        if self
            .search_due
            .is_some_and(|t| t.elapsed() > Duration::from_millis(250))
        {
            self.refresh(true);
        }
        if self.dirty && self.edited.elapsed() > Duration::from_millis(700) {
            self.save();
            self.edited = Instant::now();
        }
        if self.dirty || self.search_due.is_some() {
            ctx.request_repaint_after(Duration::from_millis(100));
        }
        if ctx.input(|i| i.viewport().close_requested())
            && (!self.save() || self.importing || self.exporting)
        {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            if self.importing {
                self.cancel.store(true, Ordering::Relaxed);
                self.status = "Finishing current import batch. Close again when complete.".into();
            }
            if self.exporting {
                self.status = "Finishing export. Close again when complete.".into();
            }
        }
        if ctx.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::O)) {
            self.choose_import(ctx);
        }
        if ctx.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::S)) {
            self.save();
        }
        if self.play.is_none() && ctx.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::P)) {
            self.jump = Some((String::new(), library::jump(&self.db, "").unwrap_or_default(), 0));
        }
        let plain = |key| ctx.input(|i| i.modifiers.is_none() && i.key_pressed(key));
        if !ctx.wants_keyboard_input() {
            if plain(egui::Key::F) {
                self.flipped = !self.flipped;
            }
            if plain(egui::Key::L) && self.engine_path.is_some() {
                self.continuous = !self.continuous;
                self.analysis = None;
                self.analysis_error = None;
            }
        }
        if !ctx.wants_keyboard_input()
            && let Some(replay) = &self.replay
        {
            if ctx.input(|i| i.key_pressed(egui::Key::ArrowRight)) {
                self.current = replay.next(self.current).unwrap_or(self.current);
            }
            if ctx.input(|i| i.key_pressed(egui::Key::ArrowLeft)) {
                self.current = replay.nodes[self.current].parent.unwrap_or(0);
            }
            if ctx.input(|i| i.key_pressed(egui::Key::Home) || i.key_pressed(egui::Key::ArrowUp)) {
                self.current = 0;
            }
            if ctx.input(|i| i.key_pressed(egui::Key::End) || i.key_pressed(egui::Key::ArrowDown)) {
                self.current = *replay.line(self.current).last().unwrap();
            }
            if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
                self.selected_square = None;
                self.promotion.clear();
            }
        }
        self.position_changed();
        let drops = ctx.input(|i| {
            i.raw
                .dropped_files
                .iter()
                .filter_map(|f| f.path.clone())
                .collect::<Vec<_>>()
        });
        if !drops.is_empty() {
            self.import_files(drops, ctx);
        }
        // ⌘V outside a text field imports pasted PGN; the importer reads files, so stage it in one.
        let pasted = if ctx.wants_keyboard_input() || self.importing { None } else {
            ctx.input(|i| i.events.iter().find_map(|e| match e {
                egui::Event::Paste(text) if !text.trim().is_empty() => Some(text.clone()),
                _ => None,
            }))
        };
        if let Some(text) = pasted {
            let path = std::env::temp_dir().join("chesslib-paste.pgn");
            match std::fs::write(&path, text) { Ok(()) => self.import_files(vec![path], ctx), Err(e) => self.error = Some(e.to_string()) }
        }
        let cloth = self.bookcloth.clone();
        egui::TopBottomPanel::top("toolbar")
            .show_separator_line(false)
            .frame(
                egui::Frame::new()
                    .fill(CLOTH)
                    .inner_margin(egui::Margin::symmetric(22, 14)),
            )
            .show(ctx, |ui| {
                let rect = ui.max_rect().expand2(Vec2::new(22.0, 14.0));
                material(ui.painter(), &cloth, rect, CLOTH, 150.0);
                // Rounded spine edge: a lit top thread and a deep shadow along the bottom.
                ui.painter().hline(rect.x_range(), rect.top() + 0.5, egui::Stroke::new(1.0_f32, GREEN));
                ui.painter().hline(rect.x_range(), rect.bottom() - 1.0, egui::Stroke::new(2.0_f32, CLOTH_SHADOW));
                ui.visuals_mut().override_text_color = Some(IVORY);
                let w = &mut ui.visuals_mut().widgets;
                w.inactive.bg_stroke = egui::Stroke::new(1.0_f32, CLOTH_SHADOW);
                w.hovered.bg_stroke = egui::Stroke::new(1.0_f32, Color32::from_rgb(90, 120, 96));
                w.noninteractive.fg_stroke.color = Color32::from_rgb(140, 160, 140);
                ui.horizontal(|ui| {
                    embossed(ui, "♞  CHESSLIB", 19.0, IVORY);
                    ui.label(
                        RichText::new(" /  A personal chess library")
                            .size(12.0)
                            .color(Color32::from_rgb(170, 187, 168)),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui
                            .add_enabled(
                                !self.exporting,
                                egui::Button::new("Export results").fill(GREEN),
                            )
                            .clicked()
                        {
                            self.export(false);
                        }
                        if ui
                            .add_enabled(
                                self.loaded.is_some() && self.play.is_none() && !self.exporting,
                                egui::Button::new("Export game").fill(GREEN),
                            )
                            .clicked()
                        {
                            self.export(true);
                        }
                        if ui
                            .add_enabled(
                                !self.importing,
                                egui::Button::new(RichText::new("+ Import PGN").color(Color32::WHITE))
                                    .fill(RIBBON)
                                    .stroke(egui::Stroke::new(1.0_f32, RIBBON_LIGHT)),
                            )
                            .clicked()
                        {
                            self.choose_import(ctx);
                        }
                        if let Some(play) = &self.play {
                            let label = if play.train.is_some() { "Leave training" } else { "Leave game" };
                            if ui.add(egui::Button::new(label).fill(GREEN)).clicked() {
                                self.leave_play();
                            }
                        } else {
                            if ui
                                .add_enabled(self.engine_path.is_some(), egui::Button::new("Play vs computer").fill(GREEN))
                                .on_disabled_hover_text("Choose a Stockfish executable to play")
                                .clicked()
                            {
                                self.play_setup = Some(PlaySettings { human: Color::White, level: 5, show_evaluation: true });
                            }
                            if ui
                                .add(egui::Button::new("Train on mistakes").fill(GREEN))
                                .on_hover_text("Replay positions where reviewed games went wrong")
                                .clicked()
                            {
                                self.train_setup = true;
                            }
                        }
                        ui.add_space(8.0);
                        if ui.checkbox(&mut self.sounds_enabled, RichText::new("Sounds").color(IVORY)).changed() {
                            NSUserDefaults::standardUserDefaults()
                                .setBool_forKey(!self.sounds_enabled, ns_string!("MuteBoardSounds"));
                        }
                    });
                });
            });
        egui::TopBottomPanel::bottom("status")
            .show_separator_line(false)
            .frame(egui::Frame::new().fill(FOLD).inner_margin(10))
            .show(ctx, |ui| {
                let rect = ui.max_rect().expand(10.0);
                material(ui.painter(), &cloth, rect, Color32::from_rgb(232, 216, 190), 300.0);
                ui.painter().hline(rect.x_range(), rect.top() + 0.5, egui::Stroke::new(1.0_f32, BEVEL));
                ui.painter().hline(rect.x_range(), rect.top() + 1.5, egui::Stroke::new(1.0_f32, Color32::from_white_alpha(90)));
                ui.horizontal(|ui| {
                    if self.importing {
                        ui.spinner();
                        ui.label(format!(
                            "Importing · {} added · {} duplicates",
                            self.progress.imported, self.progress.duplicates
                        ));
                        if ui.button("Stop").clicked() {
                            self.cancel.store(true, Ordering::Relaxed);
                        }
                    } else if self.exporting {
                        ui.spinner();
                        ui.label("Exporting PGN…");
                    } else {
                        ui.small(&self.status);
                    }
                });
            });
        if self.play.is_none() {
            self.sidebar(ctx);
        }
        self.notebook(ctx);
        self.position_changed();
        self.board(ctx);
        self.position_changed();
        if let Some(settings) = &mut self.play_setup {
            let (mut start, mut cancel) = (false, false);
            egui::Window::new("Play vs computer")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
                .show(ctx, |ui| {
                    ui.horizontal(|ui| {
                        ui.selectable_value(&mut settings.human, Color::White, "As White");
                        ui.selectable_value(&mut settings.human, Color::Black, "As Black");
                    });
                    ui.add(egui::Slider::new(&mut settings.level, 0..=20).text("Stockfish level"));
                    ui.checkbox(&mut settings.show_evaluation, "Show evaluation");
                    if self.replay.is_some() {
                        ui.small("Starts from the position on the board.");
                    }
                    ui.horizontal(|ui| {
                        start = ui
                            .add(egui::Button::new(RichText::new("Start").color(Color32::WHITE)).fill(RIBBON).stroke(egui::Stroke::new(1.0_f32, RIBBON_LIGHT)))
                            .clicked();
                        cancel = ui.button("Cancel").clicked();
                    });
                });
            if start {
                let settings = *settings;
                self.play_setup = None;
                self.start_play(settings);
            } else if cancel {
                self.play_setup = None;
            }
        }
        if self.train_setup {
            let (mut start, mut cancel) = (false, false);
            egui::Window::new("Train on mistakes")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
                .show(ctx, |ui| {
                    ui.label("Puzzles from mistakes and blunders in games you have reviewed.");
                    ui.horizontal(|ui| {
                        ui.label("Player");
                        let field = ui.add(egui::TextEdit::singleline(&mut self.train_player).hint_text("Both sides").desired_width(220.0));
                        start = field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                    });
                    ui.small("Only moves by a player whose name contains these words.");
                    ui.horizontal(|ui| {
                        start |= ui
                            .add(egui::Button::new(RichText::new("Start").color(Color32::WHITE)).fill(RIBBON).stroke(egui::Stroke::new(1.0_f32, RIBBON_LIGHT)))
                            .clicked();
                        cancel = ui.button("Cancel").clicked();
                    });
                });
            if start {
                self.train_setup = false;
                self.start_training();
            } else if cancel {
                self.train_setup = false;
            }
        }
        if let Some((query, rows, highlight)) = &mut self.jump {
            let (mut open, mut close) = (None, false);
            let modal = egui::Modal::new(egui::Id::new("jump")).show(ctx, |ui| {
                ui.set_width(460.0);
                ui.heading("Jump to game");
                // Take the arrows and Return before the text field moves its cursor on them.
                let (up, down, enter) = ui.input_mut(|i| {
                    (
                        i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp),
                        i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown),
                        i.consume_key(egui::Modifiers::NONE, egui::Key::Enter),
                    )
                });
                let field = ui.add(
                    egui::TextEdit::singleline(query)
                        .hint_text("Players, e.g. Gri Pla")
                        .desired_width(f32::INFINITY),
                );
                field.request_focus();
                if field.changed() {
                    // shortcut: queries on the UI thread (~40 ms worst case at 100k games); move to the worker if libraries grow far past that.
                    match library::jump(&self.db, query) {
                        Ok(found) => *rows = found,
                        Err(e) => self.error = Some(format!("Could not search library: {e}")),
                    }
                    *highlight = 0;
                }
                if down {
                    *highlight = (*highlight + 1).min(rows.len().saturating_sub(1));
                }
                if up {
                    *highlight = highlight.saturating_sub(1);
                }
                if enter {
                    open = rows.get(*highlight).map(|r| r.id);
                }
                ui.add_space(6.0);
                if rows.is_empty() {
                    ui.label("No matching games.");
                }
                for (i, row) in rows.iter().enumerate() {
                    let response = ui
                        .add_sized(
                            [ui.available_width(), 30.0],
                            egui::Button::new(format!("{}  —  {}   · {}", row.white, row.black, row.date))
                                .truncate()
                                .selected(i == *highlight)
                                .frame(i == *highlight),
                        )
                        .on_hover_text(&row.event);
                    if response.clicked() {
                        open = Some(row.id);
                    }
                }
                close = ui.input(|i| i.key_pressed(egui::Key::Escape));
            });
            if let Some(id) = open {
                self.jump = None;
                self.select(id);
            } else if close || modal.should_close() {
                self.jump = None;
            }
        }
        if self.details.is_none() && !self.missing.is_empty() {
            let id = self.missing.remove(0);
            self.edit_details(id, false);
        }
        if let Some((loaded, fields, editing)) = &mut self.details {
            let editing = *editing;
            let (mut save, mut skip, mut skip_all) = (false, false, false);
            let modal = egui::Modal::new(egui::Id::new("details")).show(ctx, |ui| {
                ui.set_width(420.0);
                ui.heading(if editing { "Edit game" } else { "Missing game details" });
                ui.label(loaded.game.tag("Result"));
                if !editing && !self.missing.is_empty() {
                    ui.small(format!("{} more imported games need details after this one.", self.missing.len()));
                }
                ui.add_space(8.0);
                egui::Grid::new("details_grid").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
                    for (field, (label, hint)) in fields.iter_mut().zip([("White", "Player name"), ("Black", "Player name"), ("Event", "Tournament or match"), ("Date", "YYYY.MM.DD")]) {
                        ui.label(label);
                        ui.add(egui::TextEdit::singleline(field).hint_text(hint).desired_width(280.0));
                        ui.end_row();
                    }
                });
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    save = ui.button("Save").clicked() || ui.input(|i| i.key_pressed(egui::Key::Enter));
                    skip = ui.button(if editing { "Cancel" } else { "Skip" }).clicked();
                    skip_all = !editing && !self.missing.is_empty() && ui.button("Skip all").clicked();
                });
            });
            if save {
                // Blank stays unknown, in PGN's own spelling.
                for ((key, unknown), value) in [("White", "?"), ("Black", "?"), ("Event", "?"), ("Date", "????.??.??")].into_iter().zip(fields.iter()) {
                    let value = value.trim();
                    let value = if key == "Date" { value.replace(['-', '/'], ".") } else { value.to_owned() };
                    loaded.game.set_tag(key, if value.is_empty() { unknown.into() } else { value });
                }
                match library::save(&self.db, loaded) {
                    Ok(()) => {
                        // The editor may hold this game too; keep its next save from restoring the old tags.
                        if let Some(open) = &mut self.loaded
                            && open.id == loaded.id
                        {
                            open.game.tags = loaded.game.tags.clone();
                        }
                        self.refresh(false);
                    }
                    Err(e) => self.error = Some(format!("Could not save game details: {e}")),
                }
            }
            if skip_all {
                self.missing.clear();
            }
            if save || skip || skip_all || modal.should_close() {
                self.details = None;
            }
        }
        if !self.promotion.is_empty() {
            egui::Window::new("Promote pawn")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
                .show(ctx, |ui| {
                    ui.label("Choose a promotion piece");
                    ui.horizontal(|ui| {
                        for movement in self.promotion.clone() {
                            let label = match movement.promotion() {
                                Some(Role::Queen) => "Queen",
                                Some(Role::Rook) => "Rook",
                                Some(Role::Bishop) => "Bishop",
                                Some(Role::Knight) => "Knight",
                                _ => continue,
                            };
                            if ui.button(label).clicked() {
                                self.play(movement);
                            }
                        }
                        if ui.button("Cancel").clicked() {
                            self.promotion.clear();
                        }
                    });
                });
        }
        if let Some(error) = self.error.clone() {
            egui::Window::new("Needs attention")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
                .show(ctx, |ui| {
                    ui.set_max_width(460.0);
                    ui.label(error);
                    if ui.button("Dismiss").clicked() {
                        self.error = None;
                    }
                });
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
enum BoardSound {
    Placement,
    Capture,
}

impl BoardSound {
    fn load(self) -> Option<Retained<NSSound>> {
        let (bytes, volume): (&[u8], _) = match self {
            Self::Placement => (include_bytes!("../assets/piece-placement.mp3"), 1.0),
            Self::Capture => (include_bytes!("../assets/piece-capture.mp3"), 0.5),
        };
        let sound = NSSound::initWithData(NSSound::alloc(), &NSData::with_bytes(bytes))?;
        sound.setVolume(volume);
        Some(sound)
    }
}

fn move_sound(replay: &Replay, previous: usize, current: usize) -> Option<BoardSound> {
    let node = &replay.nodes[current];
    node.position.as_ref()?;
    if node.parent != Some(previous) {
        return None;
    }
    Some(if node.san.contains('x') {
        BoardSound::Capture
    } else {
        BoardSound::Placement
    })
}

fn evaluation_bar_value(position: &shakmaty::Chess, score: Option<&str>) -> Option<(f32, String)> {
    if let Some(outcome) = position.outcome().known() {
        return Some(match outcome.winner() {
            Some(Color::White) => (1.0, "1-0".into()),
            Some(Color::Black) => (0.0, "0-1".into()),
            None => (0.5, "½".into()),
        });
    }
    let score = score?;
    if let Some(mate) = score.strip_prefix("Mate ") {
        let moves: i32 = mate.parse().ok()?;
        return Some(if moves > 0 {
            (1.0, format!("M{moves}"))
        } else {
            (0.0, format!("-M{}", moves.unsigned_abs()))
        });
    }
    let pawns: f64 = score.parse().ok()?;
    if !pawns.is_finite() {
        return None;
    }
    let share = (1.0 / (1.0 + (-0.368208 * pawns).exp())) as f32;
    Some((
        share,
        if pawns.abs() < 0.05 {
            "0.0".into()
        } else {
            format!("{pawns:+.1}")
        },
    ))
}

/// Paints `rect` as a woven material: the near-white bookcloth grain multiplied by `color`.
/// UVs follow screen position so the grain stays continuous across neighbouring surfaces.
fn material(painter: &egui::Painter, cloth: &egui::TextureHandle, rect: egui::Rect, color: Color32, weave: f32) {
    let uv = egui::Rect::from_min_size((rect.min.to_vec2() / weave).to_pos2(), rect.size() / weave);
    painter.image(cloth.id(), rect, uv, color);
}

/// Soft shadow falling inward from the top edge, as under a book cover or a lifted page.
/// A study-desk section: rule, bold title, and quiet status on the right.
fn section(ui: &mut egui::Ui, title: impl Into<String>, meta: &str) {
    ui.add_space(6.0);
    ui.separator();
    header(ui, title, meta);
}

fn header(ui: &mut egui::Ui, title: impl Into<String>, meta: &str) {
    ui.horizontal(|ui| {
        ui.label(RichText::new(title).strong());
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(RichText::new(meta).size(12.0).color(MUTED));
        });
    });
}

fn top_shadow(painter: &egui::Painter, rect: egui::Rect, depth: u8) {
    for i in 0..depth {
        let y = rect.top() + i as f32 + 0.5;
        let alpha = (36.0 * (1.0 - i as f32 / depth as f32).powi(2)) as u8;
        painter.hline(rect.x_range(), y, egui::Stroke::new(1.0_f32, Color32::from_black_alpha(alpha)));
    }
}

/// Burnt-orange bookmark ribbon with a notched tail, hanging from `top` at `x`.
fn ribbon(painter: &egui::Painter, cloth: &egui::TextureHandle, x: f32, top: f32, size: Vec2) {
    let rect = egui::Rect::from_min_size(egui::pos2(x, top), size);
    let notch = size.x * 0.4;
    painter.add(
        egui::Shadow { offset: [1, 3], blur: 6, spread: 0, color: Color32::from_black_alpha(60) }
            .as_shape(rect.with_max_y(rect.bottom() - notch), 0),
    );
    let body = rect.with_max_y(rect.bottom() - notch);
    material(painter, cloth, body, RIBBON, 60.0);
    let (l, r, c, b, m) = (rect.left(), rect.right(), rect.center().x, rect.bottom(), body.bottom());
    // Two tails left by the V-shaped notch (epaint only fills convex polygons).
    for tail in [[egui::pos2(l, m), egui::pos2(c, m), egui::pos2(l, b)], [egui::pos2(c, m), egui::pos2(r, m), egui::pos2(r, b)]] {
        painter.add(egui::Shape::convex_polygon(tail.to_vec(), RIBBON, egui::Stroke::NONE));
    }
    painter.vline(l + 1.5, top..=b - 2.0, egui::Stroke::new(1.5_f32, RIBBON_LIGHT));
    painter.vline(r - 1.0, top..=b - 1.0, egui::Stroke::new(1.0_f32, Color32::from_rgb(150, 50, 0)));
}

/// Text pressed into cloth: a dark offset shadow under a light face.
fn embossed(ui: &mut egui::Ui, text: &str, size: f32, color: Color32) {
    let galley = ui.painter().layout_no_wrap(text.into(), egui::FontId::proportional(size), color);
    let (rect, response) = ui.allocate_exact_size(galley.size(), egui::Sense::hover());
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Label, true, text));
    ui.painter().galley_with_override_text_color(rect.min + Vec2::new(0.0, 1.0), galley.clone(), CLOTH_SHADOW);
    ui.painter().galley(rect.min, galley, color);
}

/// Colour and badge symbol for a game-review verdict.
fn quality_style(quality: engine::Quality) -> (Color32, &'static str) {
    match quality {
        // Bookcloth green, muted sage, ochre, ribbon orange and oxblood, after docs/design.md.
        engine::Quality::Best => (CLOTH, "★"),
        engine::Quality::Good => (Color32::from_rgb(110, 122, 92), "✔"),
        engine::Quality::Inaccuracy => (Color32::from_rgb(184, 138, 40), "?!"),
        engine::Quality::Mistake => (RIBBON, "?"),
        engine::Quality::Blunder => (Color32::from_rgb(128, 28, 24), "??"),
    }
}

fn move_destination(movement: Move) -> Square {
    match movement {
        Move::Castle { king, rook } => Square::from_coords(
            if rook.file() > king.file() {
                shakmaty::File::G
            } else {
                shakmaty::File::C
            },
            king.rank(),
        ),
        _ => movement.to(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn evaluation_bar_handles_unknown_scores_advantages_and_mates() {
        let position = shakmaty::Chess::default();
        assert!(evaluation_bar_value(&position, None).is_none());
        assert!(evaluation_bar_value(&position, Some("NaN")).is_none());
        assert!(evaluation_bar_value(&position, Some("Checkmate")).is_none());
        assert_eq!(
            evaluation_bar_value(&position, Some("0.00")),
            Some((0.5, "0.0".into()))
        );
        let white = evaluation_bar_value(&position, Some("+3.00")).unwrap();
        let black = evaluation_bar_value(&position, Some("-3.00")).unwrap();
        assert!(white.0 > 0.5 && black.0 < 0.5);
        assert!((white.0 + black.0 - 1.0).abs() < 1e-6);
        assert_eq!(white.1, "+3.0");
        assert_eq!(
            evaluation_bar_value(&position, Some("Mate +3")),
            Some((1.0, "M3".into()))
        );
        assert_eq!(
            evaluation_bar_value(&position, Some("Mate -2")),
            Some((0.0, "-M2".into()))
        );
        let game = pgn_reader::Reader::new("1. f3 e5 2. g4 Qh4# 0-1".as_bytes())
            .read_game(&mut crate::pgn::Parser)
            .unwrap()
            .unwrap()
            .unwrap();
        let positions = game.mainline_positions().unwrap();
        assert_eq!(
            evaluation_bar_value(&positions.last().unwrap().1, None),
            Some((0.0, "0-1".into()))
        );
        let draw = "4k3/8/8/8/8/8/8/4K3 w - - 0 1"
            .parse::<Fen>()
            .unwrap()
            .into_position(shakmaty::CastlingMode::Standard)
            .unwrap();
        assert_eq!(evaluation_bar_value(&draw, None), Some((0.5, "½".into())));
    }

    #[test]
    fn sounds_follow_single_forward_moves() {
        let game = pgn_reader::Reader::new(include_str!("../fixtures/sample.pgn").as_bytes())
            .read_game(&mut crate::pgn::Parser)
            .unwrap()
            .unwrap()
            .unwrap();
        let replay = game.replay().unwrap();
        let line = replay.line(0);
        assert_eq!(
            move_sound(&replay, line[0], line[1]),
            Some(BoardSound::Placement)
        );
        assert_eq!(
            move_sound(&replay, line[6], line[7]),
            Some(BoardSound::Capture)
        );
        assert_eq!(
            move_sound(&replay, line[20], line[21]),
            Some(BoardSound::Capture)
        );
        assert_eq!(
            move_sound(&replay, line[32], line[33]),
            Some(BoardSound::Placement)
        );
        assert_eq!(move_sound(&replay, line[1], line[0]), None);
        assert_eq!(move_sound(&replay, line[0], line[2]), None);
    }

    #[test]
    fn bundled_board_recordings_decode() {
        for cue in [BoardSound::Placement, BoardSound::Capture] {
            let sound = cue.load().expect("embedded chess recording should decode");
            let duration = sound.duration();
            assert!(duration > 0.0 && duration < 0.4);
        }
    }

    #[test]
    fn castling_uses_the_kings_destination_square() {
        assert_eq!(
            move_destination(Move::Castle {
                king: Square::E1,
                rook: Square::H1
            }),
            Square::G1
        );
        assert_eq!(
            move_destination(Move::Castle {
                king: Square::E8,
                rook: Square::A8
            }),
            Square::C8
        );
    }
}
