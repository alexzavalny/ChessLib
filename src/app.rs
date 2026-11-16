use crate::{
    engine,
    library::{self, Event, Filter, ImportStats, Loaded, Page, Request},
    pgn::{Game, Replay},
};
use eframe::egui::{self, Color32, RichText, Vec2};
use objc2::{AnyThread, rc::Retained};
use objc2_app_kit::NSSound;
use objc2_foundation::{NSData, NSString, NSUserDefaults, ns_string};
use shakmaty::{Color, EnPassantMode, Move, Piece, Position, Role, Setup, Square, fen::Fen};
use std::{
    collections::HashSet,
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
/// What the "Play vs computer" dialog starts with, from its button or P.
const NEW_PLAY: PlaySettings = PlaySettings { human: Color::White, level: 5, show_evaluation: true };

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

/// The library's file name inside its folder.
pub const LIBRARY_FILE: &str = "library.sqlite3";

/// The folder chosen with "Library folder…", if any.
pub fn saved_library_folder() -> Option<PathBuf> {
    NSUserDefaults::standardUserDefaults()
        .stringForKey(ns_string!("LibraryFolder"))
        .map(|folder| PathBuf::from(folder.to_string()))
}

#[derive(Clone, Copy)]
enum FolderEdit {
    New(Option<i64>),
    Rename(i64),
}

struct BoardEditor {
    setup: Setup,
    piece: Option<Piece>,
}

impl BoardEditor {
    fn place(&mut self, square: Square, piece: Option<Piece>) {
        if let Some(piece) = piece {
            self.setup.board.set_piece_at(square, piece);
        } else {
            self.setup.board.discard_piece_at(square);
        }
        // A manually changed board no longer has a known last pawn move.
        self.setup.ep_square = None;
        self.setup.halfmoves = 0;
        for (king, rooks, color) in [
            (Square::E1, [Square::A1, Square::H1], Color::White),
            (Square::E8, [Square::A8, Square::H8], Color::Black),
        ] {
            for rook in rooks {
                if self.setup.board.piece_at(king) != Some(color.king())
                    || self.setup.board.piece_at(rook) != Some(color.rook())
                {
                    self.setup.castling_rights.discard(rook);
                }
            }
        }
    }
}

pub struct App {
    board_editor: Option<BoardEditor>,
    play: Option<Play>,
    play_setup: Option<PlaySettings>,
    /// The "Train on mistakes" player field while its dialog is open; the name is kept for next time.
    train_setup: bool,
    /// The game awaiting delete confirmation: id and "White — Black".
    delete_game: Option<(i64, String)>,
    /// The folder name dialog: a new folder under the parent (None for top level), or a rename, with the typed name.
    folder_edit: Option<(FolderEdit, String)>,
    /// Folders shown closed in the sidebar tree.
    collapsed: HashSet<i64>,
    /// The folder open when the running import started; its new games are filed there.
    import_folder: Option<i64>,
    /// Another library folder was chosen: start a new app on it once this window has saved and closes.
    relaunch: bool,
    train_player: String,
    /// The text typed into an open "+ Tag" menu.
    tag_draft: String,
    /// The Cmd+P "Jump to game" query, its matches and the highlighted match.
    jump: Option<(String, Vec<library::Row>, usize)>,
    /// The Events list while it replaces the games list: its name filter and every event.
    event_list: Option<(String, Vec<library::EventRow>)>,
    /// The last game id before the running import, so new games can be checked afterwards.
    import_floor: i64,
    /// Imported games still missing details, and the one being filled in (White, Black, Event, Date; true when opened by Edit…).
    missing: Vec<i64>,
    details: Option<(Loaded, [String; 5], bool)>,
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
    /// Branch points of the open game whose side lines are folded in the move list.
    folded: HashSet<usize>,
    header_icon: egui::TextureHandle,
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
    engine_settings_open: bool,
    /// Height of the move list, set by dragging the rule below it.
    moves_height: f32,
    /// Height of the sidebar folder tree, set by dragging the rule below it.
    folders_height: f32,
    /// Widths of the library and notebook panels, set by dragging their inner edges.
    library_width: f32,
    notebook_width: f32,
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
    /// Open the newest game when the running import finishes (set by Save to library).
    open_imported: bool,
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
        // Ubuntu Bold matches egui's Ubuntu Light; the rest of the proportional stack stays as fallback.
        fonts.font_data.insert("bold".into(), egui::FontData::from_static(include_bytes!("../assets/Ubuntu-Bold.ttf")).into());
        let fallback = fonts.families.get(&egui::FontFamily::Proportional).cloned().unwrap_or_default();
        fonts.families.insert(egui::FontFamily::Name("bold".into()), [vec!["bold".into()], fallback].concat());
        ctx.set_fonts(fonts);
        let icon = image::load_from_memory_with_format(
            include_bytes!("../assets/header-icon.png"),
            image::ImageFormat::Png,
        )
        .expect("embedded header icon must be a valid PNG")
        .into_rgba8();
        let header_icon = ctx.load_texture(
            "ChessLib header icon",
            egui::ColorImage::from_rgba_unmultiplied(
                [icon.width() as usize, icon.height() as usize],
                icon.as_raw(),
            ),
            egui::TextureOptions::LINEAR.with_mipmap_mode(Some(egui::TextureFilter::Linear)),
        );
        let piece_art = image::load_from_memory_with_format(
            include_bytes!("../assets/stone-pieces.png"),
            image::ImageFormat::Png,
        )
        .expect("embedded chess piece atlas must be a valid PNG")
        .into_rgba8();
        debug_assert_eq!(piece_art.dimensions(), (1536, 512));
        let piece_art = ctx.load_texture(
            "stone chess pieces",
            egui::ColorImage::from_rgba_unmultiplied([1536, 512], piece_art.as_raw()),
            egui::TextureOptions::LINEAR.with_mipmap_mode(Some(egui::TextureFilter::Linear)),
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
            board_editor: None,
            play: None,
            play_setup: None,
            train_setup: false,
            delete_game: None,
            folder_edit: None,
            collapsed: HashSet::new(),
            import_folder: None,
            relaunch: false,
            train_player: String::new(),
            tag_draft: String::new(),
            jump: None,
            event_list: None,
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
            folded: HashSet::new(),
            header_icon,
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
            library_width: saved_width(ns_string!("LibraryWidth"), 302.0),
            notebook_width: saved_width(ns_string!("NotebookWidth"), 310.0),
            folders_height: match NSUserDefaults::standardUserDefaults().doubleForKey(ns_string!("FoldersHeight")) {
                h if h < 40.0 => 170.0,
                h => h as f32,
            },
            analysis_error: None,
            continuous: false,
            engine_settings_open: false,
            review_job: None,
            review_game: None,
            review_progress: (0, 0),
            review_error: None,
            dirty: false,
            edited: Instant::now(),
            status: "Your chess, thoughtfully organised.".into(),
            error: None,
            importing: false,
            open_imported: false,
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
        // Play games and the analysis board (id -1) are not library rows.
        if self.play.is_some() || self.loaded.as_ref().is_some_and(|l| l.id < 0) {
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
    fn delete(&mut self, id: i64) {
        if let Err(e) = library::delete(&self.db, id) {
            self.error = Some(format!("Could not delete game: {e}"));
            return;
        }
        // Close the game if it is open so nothing saves edits to a deleted row.
        if self.loaded.as_ref().is_some_and(|g| g.id == id) {
            self.loaded = None;
            self.replay = None;
            self.current = 0;
            self.dirty = false;
        }
        self.missing.retain(|&m| m != id);
        self.status = "Game deleted".into();
        self.refresh(false);
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
                let fields = [known("Title"), known("White"), known("Black"), known("Event"), known("Date")];
                self.details = Some((loaded, fields, editing));
            }
            Err(e) => self.error = Some(format!("Could not open game: {e}")),
        }
    }
    /// The sidebar's folder tree: click to show a folder's games, right-click to add, rename or delete.
    fn folder_tree(&mut self, ui: &mut egui::Ui, folders: &[library::Folder]) {
        enum Action { Select(i64), New(Option<i64>), Rename(i64, String), Delete(i64, Option<i64>) }
        let mut action = None;
        ui.horizontal(|ui| {
            ui.label(RichText::new("FOLDERS").size(11.0).strong().color(GREEN));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.small_button("+ New").on_hover_text("New top-level folder").clicked() {
                    action = Some(Action::New(None));
                }
            });
        });
        if folders.is_empty() {
            ui.small("Make folders for openings, courses or your own games.");
        }
        let tree = egui::ScrollArea::vertical().id_salt("folders").max_height(self.folders_height).auto_shrink(false).show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 2.0;
            for (depth, folder) in folder_rows(folders, &self.collapsed) {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 2.0;
                    ui.add_space(depth as f32 * 14.0);
                    let height = 26.0;
                    let toggle = ui.allocate_response(Vec2::new(14.0, height), egui::Sense::click());
                    if folders.iter().any(|f| f.parent == Some(folder.id)) {
                        let open = !self.collapsed.contains(&folder.id);
                        let (c, r) = (toggle.rect.center(), 4.0);
                        let points = if open {
                            vec![c + Vec2::new(-r, -r * 0.6), c + Vec2::new(r, -r * 0.6), c + Vec2::new(0.0, r * 0.8)]
                        } else {
                            vec![c + Vec2::new(-r * 0.6, -r), c + Vec2::new(r * 0.8, 0.0), c + Vec2::new(-r * 0.6, r)]
                        };
                        let color = if toggle.hovered() { INK } else { MUTED };
                        ui.painter().add(egui::Shape::convex_polygon(points, color, egui::Stroke::NONE));
                        if toggle.clicked() && !self.collapsed.remove(&folder.id) {
                            self.collapsed.insert(folder.id);
                        }
                    }
                    // A full-width row: name on the left, game count right-aligned in quieter figures.
                    let (rect, response) = ui.allocate_exact_size(Vec2::new(ui.available_width(), height), egui::Sense::click());
                    let selected = self.filter.folder == Some(folder.id);
                    let painter = ui.painter();
                    if selected {
                        painter.rect(rect, 6, ui.visuals().selection.bg_fill, ui.visuals().selection.stroke, egui::StrokeKind::Inside);
                    } else if response.hovered() {
                        painter.rect_filled(rect, 6, ui.visuals().faint_bg_color);
                    }
                    let count = card_text(painter, &folder.games.to_string(), 12.0, if folder.games == 0 { BEVEL } else if selected { GREEN } else { MUTED }, f32::INFINITY, 1);
                    let name = card_text(painter, &folder.name, 14.0, INK, rect.width() - count.size().x - 26.0, 1);
                    painter.galley(rect.left_center() + Vec2::new(8.0, -name.size().y / 2.0), name, INK);
                    painter.galley(rect.right_center() - Vec2::new(8.0 + count.size().x, count.size().y / 2.0), count, MUTED);
                    if response.clicked() {
                        action = Some(Action::Select(folder.id));
                    }
                    response.context_menu(|ui| {
                        if ui.button("New subfolder…").clicked() {
                            action = Some(Action::New(Some(folder.id)));
                        }
                        if ui.button("Rename…").clicked() {
                            action = Some(Action::Rename(folder.id, folder.name.clone()));
                        }
                        if ui.button("Delete folder").on_hover_text("Its games and subfolders move up one level; no games are deleted.").clicked() {
                            action = Some(Action::Delete(folder.id, folder.parent));
                        }
                        if action.is_some() {
                            ui.close();
                        }
                    });
                });
            }
        });
        // The tree keeps the dragged height even past its last folder; tags and games keep at least 240pt below.
        let full = (tree.inner_rect.height() + ui.available_height() - 240.0).max(40.0);
        resize_rule(ui, &mut self.folders_height, 40.0..=full, ns_string!("FoldersHeight"));
        match action {
            Some(Action::Select(id)) => {
                // Clicking the open folder again shows every folder's games.
                self.filter.folder = (self.filter.folder != Some(id)).then_some(id);
                self.event_list = None;
                self.refresh(true);
            }
            Some(Action::New(parent)) => self.folder_edit = Some((FolderEdit::New(parent), String::new())),
            Some(Action::Rename(id, name)) => self.folder_edit = Some((FolderEdit::Rename(id), name)),
            Some(Action::Delete(id, parent)) => {
                match library::delete_folder(&self.db, id) {
                    Ok(()) if self.filter.folder == Some(id) => self.filter.folder = parent,
                    Ok(()) => {}
                    Err(e) => self.error = Some(format!("Could not delete the folder: {e}")),
                }
                self.refresh(true);
            }
            None => {}
        }
    }
    fn move_games(&mut self, id: Option<i64>, folder: Option<i64>) {
        match library::move_games(&self.db, &self.filter, id, folder) {
            Ok(1) => self.status = "Moved 1 game".into(),
            Ok(n) => self.status = format!("Moved {n} games"),
            Err(e) => self.error = Some(format!("Could not move games: {e}")),
        }
        self.refresh(false);
    }
    /// Switches to the library in a chosen folder: remembers it, then quits so main() reopens the app on it.
    fn choose_library_folder(&mut self, ctx: &egui::Context) {
        let Some(folder) = rfd::FileDialog::new()
            .set_title("Choose the folder for your library")
            .set_directory(self.path.parent().unwrap_or(&self.path))
            .pick_folder()
        else {
            return;
        };
        let path = folder.join(LIBRARY_FILE);
        if path == self.path {
            return;
        }
        // Opening creates or upgrades the library, so a folder that cannot hold one fails here, not after the restart.
        if let Err(e) = library::open(&path) {
            self.error = Some(format!("Cannot use {} as a library: {e}", path.display()));
            return;
        }
        // SAFETY: an NSString is a property-list value, which is all user defaults require.
        unsafe {
            NSUserDefaults::standardUserDefaults()
                .setObject_forKey(Some(&NSString::from_str(&folder.to_string_lossy())), ns_string!("LibraryFolder"));
        }
        self.relaunch = true;
        self.status = format!("Reopening on {}", path.display());
        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
    }
    fn import_files(&mut self, paths: Vec<PathBuf>, ctx: &egui::Context) {
        if paths.is_empty() || self.importing || !self.save() {
            return;
        }
        self.importing = true;
        self.import_floor = library::last_id(&self.db).unwrap_or(i64::MAX);
        self.import_folder = self.filter.folder;
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
                                Ok(replay) => {
                                    self.replay = Some(replay);
                                    self.folded.clear();
                                }
                                Err(e) => {
                                    self.replay = None;
                                    self.error = Some(format!(
                                        "Cannot display this game: {e}. It is still available for export."
                                    ));
                                }
                            }
                            self.flipped = loaded.game.flipped;
                            // Opened from a tag filter: start at the first position carrying that tag.
                            self.current = self.filter.tag.as_ref().and_then(|tag| {
                                let (node, _) = loaded.game.position_labels.iter().find(|(_, names)| names.contains(tag))?;
                                self.replay.as_ref()?.nodes.get(*node)?.position.as_ref()?;
                                Some(*node)
                            }).unwrap_or(0);
                            self.loaded = Some(loaded);
                            self.scrolled_node = None;
                            self.dirty = false;
                        }
                        Err(e) => self.error = Some(format!("Could not open game: {e}")),
                    }
                }
                Event::Progress(stats) => self.progress = stats,
                Event::Imported(result) => {
                    self.importing = false;
                    // Also after a stop or an error, so completed batches land in the folder too.
                    if let Some(folder) = self.import_folder
                        && let Err(e) = library::file_imported(&self.db, self.import_floor, folder)
                    {
                        self.error = Some(format!("Imported games could not be put in the folder: {e}"));
                    }
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
                    // A duplicate adds nothing, so the board stays open.
                    if std::mem::take(&mut self.open_imported)
                        && let Ok(id) = library::last_id(&self.db)
                        && id > self.import_floor
                    {
                        self.select(id);
                    }
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
        drag_panel_width(ctx, "library", true, &mut self.library_width, 240.0..=640.0, ns_string!("LibraryWidth"));
        egui::SidePanel::left("library")
            .exact_width(self.library_width)
            .resizable(true)
            .show_separator_line(false)
            .frame(egui::Frame::new().fill(IVORY).inner_margin(18))
            .show(ctx, |ui| {
                // Content laid out in a child, so anything too wide is clipped instead of widening the panel past its edge.
                let ui = &mut ui.new_child(egui::UiBuilder::new().max_rect(ui.max_rect()));
                // Carved ivory leaf, set slightly below the cover with a fold at its spine edge.
                let rect = ui.max_rect().expand(18.0);
                material(ui.painter(), &cloth, rect, Color32::from_rgb(240, 227, 206), 300.0);
                top_shadow(ui.painter(), rect, 8);
                for (i, alpha) in [70u8, 35, 15].into_iter().enumerate() {
                    ui.painter().vline(rect.right() - 0.5 - i as f32, rect.y_range(), egui::Stroke::new(1.0_f32, Color32::from_black_alpha(alpha)));
                }
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    ui.label(RichText::new("YOUR LIBRARY").size(11.0).strong().color(GREEN));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let button = ui
                            .add_enabled(!self.importing && !self.exporting, egui::Button::new(RichText::new("Library folder…").small()))
                            .on_hover_text(format!("{}\n\nOpen the library in another folder, or start a new one there. This library stays where it is.", self.path.display()));
                        if button.clicked() {
                            self.choose_library_folder(ctx);
                        }
                    });
                });
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
                ui.horizontal_wrapped(|ui| {
                    let listing = self.event_list.is_some();
                    if ui
                        .selectable_label(!self.filter.starred && !listing, "All games")
                        .clicked()
                    {
                        self.filter.starred = false;
                        self.filter.folder = None;
                        self.filter.tag = None;
                        self.event_list = None;
                        self.refresh(true);
                    }
                    if ui
                        .selectable_label(self.filter.starred && !listing, "★ Favourites")
                        .clicked()
                    {
                        self.filter.starred = true;
                        self.event_list = None;
                        self.refresh(true);
                    }
                    if ui.selectable_label(listing, "Events").clicked() {
                        self.event_list = match library::events(&self.db) {
                            Ok(events) if !listing => Some((String::new(), events)),
                            Ok(_) => None,
                            Err(e) => {
                                self.error = Some(format!("Could not list events: {e}"));
                                None
                            }
                        };
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
                if let Some(event) = self.filter.event.clone() {
                    ui.horizontal(|ui| {
                        // Button first so the label truncates into the remaining width instead of widening the panel.
                        if ui.small_button("✕").on_hover_text("Show games from every event").clicked() {
                            self.filter.event = None;
                            changed = true;
                        }
                        ui.add(egui::Label::new(RichText::new(format!("Event: {}", event_name(&event))).color(GREEN)).truncate());
                    });
                }
                if changed {
                    self.refresh(true);
                }
                let folders = self.page.as_ref().map(|p| p.folders.clone()).unwrap_or_default();
                self.folder_tree(ui, &folders);
                let all_labels = self.page.as_ref().map(|p| p.labels.clone()).unwrap_or_default();
                // Tags with no games in this folder are hidden, unless selected so they can be cleared.
                let labels: Vec<_> = all_labels.iter().filter(|(name, games)| *games > 0 || self.filter.tag.as_ref() == Some(name)).cloned().collect();
                ui.label(RichText::new("TAGS").size(11.0).strong().color(GREEN));
                if all_labels.is_empty() {
                    ui.small("Tag a game under its title, or a position under its notes.");
                } else if labels.is_empty() {
                    ui.small("No tagged games in this folder.");
                }
                let mut chosen = None;
                egui::ScrollArea::vertical().id_salt("tags").max_height(84.0).auto_shrink([false, true]).show(ui, |ui| {
                    ui.horizontal_wrapped(|ui| {
                        ui.spacing_mut().item_spacing = Vec2::new(5.0, 5.0);
                        for (name, games) in &labels {
                            let selected = self.filter.tag.as_ref() == Some(name);
                            if tag_chip(ui, name, Some(&games.to_string()), selected).clicked() {
                                chosen = Some(name.clone());
                            }
                        }
                    });
                });
                if let Some(name) = chosen {
                    // Clicking the active tag again shows every tag's games.
                    self.filter.tag = (self.filter.tag.as_ref() != Some(&name)).then_some(name);
                    self.event_list = None;
                    self.refresh(true);
                }
                if let Some((query, events)) = &mut self.event_list {
                    ui.add(
                        egui::TextEdit::singleline(query)
                            .hint_text("Filter events…")
                            .desired_width(f32::INFINITY),
                    );
                    let needle = query.to_lowercase();
                    let shown: Vec<_> = events
                        .iter()
                        .filter(|e| e.name.to_lowercase().contains(&needle))
                        .collect();
                    ui.small(match shown.len() {
                        0 if events.is_empty() => "No events yet.".into(),
                        0 => "No matching events.".into(),
                        1 => "1 event".into(),
                        count => format!("{count} events"),
                    });
                    let mut chosen = None;
                    ui.scope(|ui| {
                        ui.spacing_mut().item_spacing.y = 5.0;
                        egui::ScrollArea::vertical().id_salt("events").show_rows(
                            ui, EVENT_CARD_HEIGHT, shown.len(), |ui, range| {
                                for event in &shown[range] {
                                    let selected = self.filter.event.as_ref() == Some(&event.name);
                                    if event_card(ui, event, selected).clicked() {
                                        chosen = Some(event.name.clone());
                                    }
                                }
                            },
                        );
                    });
                    if let Some(name) = chosen {
                        self.filter.event = Some(name);
                        self.event_list = None;
                        self.refresh(true);
                    }
                    return;
                }
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
                    if total > 0 {
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.menu_button(RichText::new("Move all to folder").small(), |ui| {
                                ui.small(format!("Moves the {total} games listed here"));
                                if let Some(folder) = folder_picker(ui, &folders) {
                                    self.move_games(None, folder);
                                    ui.close();
                                }
                            });
                        });
                    }
                });
                egui::ScrollArea::vertical()
                    .id_salt("games")
                    .max_height((ui.available_height() - 55.0).max(100.0))
                    .show(ui, |ui| {
                        ui.spacing_mut().item_spacing.y = 5.0;
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
                            let response = game_card(ui, &row, selected);
                            if response.clicked() {
                                self.select(row.id);
                            }
                            response.context_menu(|ui| {
                                if ui.button("Edit…").clicked() {
                                    self.edit_details(row.id, true);
                                    ui.close();
                                }
                                if ui.button("Train on this game").clicked() {
                                    self.start_training(Some(row.id));
                                    ui.close();
                                }
                                ui.menu_button("Move to folder", |ui| {
                                    if let Some(folder) = folder_picker(ui, &folders) {
                                        self.move_games(Some(row.id), folder);
                                        ui.close();
                                    }
                                });
                                ui.separator();
                                if ui
                                    .add_enabled(self.play.is_none(), egui::Button::new("Delete…"))
                                    .on_disabled_hover_text("Finish the game or training first.")
                                    .clicked()
                                {
                                    let name = |s: &str| if s.is_empty() { "?".to_owned() } else { s.to_owned() };
                                    let label = if row.title.is_empty() { format!("{}  —  {}", name(&row.white), name(&row.black)) } else { row.title.clone() };
                                    self.delete_game = Some((row.id, label));
                                    ui.close();
                                }
                            });
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

    /// An unsaved board at the start position, shown whenever no game is open.
    /// Unknown tags stay empty so saving it asks for the game's details.
    fn show_analysis_board(&mut self, position: Option<&shakmaty::Chess>) {
        let mut game = Game::default();
        game.tags = vec![("Result".into(), "*".into())];
        self.show_scratch(game, position);
        self.flipped = position.is_some_and(|p| p.turn() == Color::Black);
    }

    /// Leaves the open game (saving its edits) for a fresh analysis board.
    fn new_board(&mut self) {
        if !self.save() {
            return;
        }
        // Drop any library game still loading so it cannot replace the new board.
        self.load_seq += 1;
        self.loading = false;
        self.show_analysis_board(None);
    }

    /// Imports the analysis board as a new library game, then opens it.
    fn save_analysis(&mut self, ctx: &egui::Context) {
        let Some(loaded) = &self.loaded else { return };
        let pgn = match loaded.game.export_as_game(&loaded.notes) {
            Ok(pgn) => pgn,
            Err(e) => {
                self.error = Some(format!("Could not save the board: {e}"));
                return;
            }
        };
        let path = std::env::temp_dir().join("chesslib-board.pgn");
        match std::fs::write(&path, pgn) {
            Ok(()) => {
                self.import_files(vec![path], ctx);
                self.open_imported = self.importing;
            }
            Err(e) => self.error = Some(e.to_string()),
        }
    }

    /// Shows an unsaved game starting from `position` (the standard start when None).
    fn show_scratch(&mut self, mut game: Game, position: Option<&shakmaty::Chess>) {
        if let Some(position) = position {
            let fen = Fen::from_position(position, EnPassantMode::Legal).to_string();
            game.tags.extend([("SetUp".into(), "1".into()), ("FEN".into(), fen)]);
        }
        self.replay = game.replay().ok();
        self.folded.clear();
        self.loaded = Some(Loaded { id: -1, game, notes: Default::default(), starred: false });
        self.current = 0;
        // Scratch boards share an id, so replacing one must invalidate its old engine result.
        self.board_position = None;
        self.analysis_position = None;
        self.analysis = None;
        self.lines.clear();
        self.review_job = None;
        self.review_error = None;
        self.review_progress = (0, 0);
        self.slide = None;
    }

    fn edit_board(&mut self, ctx: &egui::Context) {
        let Some(editor) = &mut self.board_editor else { return };
        let (mut apply, mut cancel) = (None, false);
        let modal = egui::Modal::new(egui::Id::new("board-editor")).show(ctx, |ui| {
            ui.heading("Board editor");
            ui.label("Choose a piece, then click squares to place it. Right-click to remove.");
            ui.horizontal_top(|ui| {
                let cell = 46.0;
                let (board, _) = ui.allocate_exact_size(Vec2::splat(cell * 8.0 + 28.0), egui::Sense::hover());
                material(ui.painter(), &self.bookcloth, board, CLOTH, 150.0);
                let origin = board.min + Vec2::splat(14.0);
                for row in 0..8 {
                    for col in 0..8 {
                        let (file, rank) = if self.flipped { (7 - col, row) } else { (col, 7 - row) };
                        let square = Square::new((rank * 8 + file) as u32);
                        let rect = egui::Rect::from_min_size(origin + Vec2::new(col as f32 * cell, row as f32 * cell), Vec2::splat(cell));
                        let piece = editor.setup.board.piece_at(square);
                        let label = piece.map(|p| format!("{square}: {:?} {:?}", p.color, p.role)).unwrap_or_else(|| format!("{square}: empty"));
                        let response = ui.interact(rect, ui.id().with(square), egui::Sense::click());
                        response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, &label));
                        if response.clicked() { editor.place(square, editor.piece); }
                        if response.secondary_clicked() { editor.place(square, None); }
                        if col == 0 {
                            ui.painter().text(egui::pos2(board.left() + 7.0, rect.center().y), egui::Align2::CENTER_CENTER, (rank + 1).to_string(), egui::FontId::proportional(10.0), IVORY);
                        }
                        if row == 7 {
                            ui.painter().text(egui::pos2(rect.center().x, board.bottom() - 7.0), egui::Align2::CENTER_CENTER, ((b'a' + file as u8) as char).to_string(), egui::FontId::proportional(10.0), IVORY);
                        }
                    }
                }
                let squares = egui::Rect::from_min_size(origin, Vec2::splat(cell * 8.0));
                mini_board(ui.painter(), &self.bookcloth, &self.piece_art, squares, &editor.setup.board, self.flipped);
                ui.vertical(|ui| {
                    ui.set_width(280.0);
                    for color in [Color::White, Color::Black] {
                        ui.label(if color == Color::White { "White pieces" } else { "Black pieces" });
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 3.0;
                            for role in [Role::King, Role::Queen, Role::Rook, Role::Bishop, Role::Knight, Role::Pawn] {
                                let piece = Piece { color, role };
                                let label = format!("{color:?} {role:?}");
                                let image = egui::Image::new((self.piece_art.id(), Vec2::splat(30.0))).uv(piece_uv(piece));
                                let response = ui.add(egui::Button::image(image).min_size(Vec2::splat(42.0)).selected(editor.piece == Some(piece)));
                                response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, &label));
                                if response.on_hover_text(label).clicked() { editor.piece = Some(piece); }
                            }
                        });
                    }
                    ui.selectable_value(&mut editor.piece, None, "Eraser");
                    ui.horizontal(|ui| {
                        if ui.button("Clear board").clicked() { editor.setup = Setup::empty(); }
                        if ui.button("Starting position").clicked() { editor.setup = Setup::initial(); }
                    });
                    ui.horizontal(|ui| {
                        ui.label("To move");
                        let before = editor.setup.turn;
                        ui.selectable_value(&mut editor.setup.turn, Color::White, "White");
                        ui.selectable_value(&mut editor.setup.turn, Color::Black, "Black");
                        if editor.setup.turn != before { editor.setup.ep_square = None; }
                    });
                    ui.label("Castling rights");
                    for (color, king, rooks) in [
                        (Color::White, Square::E1, [Square::H1, Square::A1]),
                        (Color::Black, Square::E8, [Square::H8, Square::A8]),
                    ] {
                        ui.horizontal(|ui| {
                            ui.label(format!("{color:?}"));
                            for (rook, label) in rooks.into_iter().zip(["O-O", "O-O-O"]) {
                                let available = editor.setup.board.piece_at(king) == Some(color.king()) && editor.setup.board.piece_at(rook) == Some(color.rook());
                                let mut enabled = editor.setup.castling_rights.contains(rook);
                                if ui.add_enabled(available, egui::Checkbox::new(&mut enabled, label)).changed() {
                                    editor.setup.castling_rights.set(rook, enabled);
                                }
                            }
                        });
                    }
                    ui.horizontal(|ui| {
                        ui.label("En passant");
                        egui::ComboBox::from_id_salt("editor-ep")
                            .selected_text(editor.setup.ep_square.map(|s| s.to_string()).unwrap_or_else(|| "None".into()))
                            .show_ui(ui, |ui| {
                                ui.selectable_value(&mut editor.setup.ep_square, None, "None");
                                let rank = if editor.setup.turn == Color::White { 5 } else { 2 };
                                for file in 0..8 {
                                    let square = Square::new(rank * 8 + file);
                                    ui.selectable_value(&mut editor.setup.ep_square, Some(square), square.to_string());
                                }
                            });
                    });
                    ui.horizontal(|ui| {
                        ui.label("Move number");
                        let mut number = editor.setup.fullmoves.get();
                        if ui.add(egui::DragValue::new(&mut number).range(1..=u32::MAX)).changed() {
                            editor.setup.fullmoves = std::num::NonZeroU32::new(number).unwrap();
                        }
                    });
                    ui.horizontal(|ui| {
                        ui.label("Halfmove clock");
                        ui.add(egui::DragValue::new(&mut editor.setup.halfmoves).range(0..=u32::MAX));
                    });
                });
            });
            ui.set_width(690.0);
            let position = editor.setup.clone().position::<shakmaty::Chess>(shakmaty::CastlingMode::Standard);
            if let Err(error) = &position {
                ui.colored_label(RIBBON, format!("Invalid position: {error}."));
            } else {
                ui.label("Position is valid.");
            }
            ui.small("Apply starts a new line from this position, replacing this unsaved board's moves and notes.");
            ui.horizontal(|ui| {
                if ui.add_enabled(position.is_ok(), egui::Button::new("Apply position")).clicked() { apply = position.ok(); }
                cancel = ui.button("Cancel").clicked();
            });
        });
        if let Some(position) = apply {
            let flipped = self.flipped;
            self.show_analysis_board(Some(&position));
            self.flipped = flipped;
            self.dirty = false;
            self.board_editor = None;
            self.position_changed();
        } else if cancel || modal.should_close() {
            self.board_editor = None;
        }
    }

    /// Puzzles from one game (both sides), or from every reviewed game for the remembered player.
    fn start_training(&mut self, game: Option<i64>) {
        let player = if game.is_some() { "" } else { self.train_player.as_str() };
        let puzzles = match library::mistakes(&self.db, game, player) {
            Ok(puzzles) => puzzles,
            Err(e) => {
                self.error = Some(format!("Could not collect mistakes: {e}"));
                return;
            }
        };
        if puzzles.is_empty() {
            self.error = Some(if game.is_some() {
                "This game has no blunders, or it hasn't been reviewed yet. Open it and run Analyze whole game first.".into()
            } else {
                "No blunders found. Run Analyze whole game on some games first, or check the player name.".into()
            });
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
        ui.label(format!("{side} to move. The game went {} here, a blunder. Find a better move.", puzzle.played));
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
            let line: Vec<_> = puzzle.best.split_whitespace().take(4).collect();
            ui.label(format!("Engine line: {} …", line.join(" ")));
        }
        let (mut retry, mut next) = (false, false);
        ui.horizontal_wrapped(|ui| {
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
            self.flipped = self.loaded.as_ref().is_some_and(|l| l.game.flipped);
            self.dirty = false;
        }
    }

    /// Flips the board and remembers it with the game.
    fn flip(&mut self) {
        self.flipped = !self.flipped;
        if let Some(loaded) = &mut self.loaded {
            loaded.game.flipped = self.flipped;
            self.dirty = true;
            self.edited = Instant::now();
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
        // Stockfish already answered here (a step forward, or the player repeated a move), so go on to its answer.
        if replay.nodes[self.current].position.as_ref().is_some_and(|p| p.turn() != human)
            && let Some(next) = replay.next(self.current)
        {
            self.current = next;
        }
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
            ui.label("Stockfish to move.");
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
        let position = self
            .replay
            .as_ref()
            .and_then(|r| r.nodes[self.current].position.clone());
        let terminal = position.as_ref().is_some_and(|p| p.outcome().is_known());
        let available =
            position.is_some() && !terminal && !self.loading && self.engine_path.is_some();
        ui.add_space(6.0);
        ui.separator();
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            ui.spacing_mut().button_padding = Vec2::new(6.0, 5.0);
            ui.label(RichText::new("Stockfish").strong());
            if ui.add_enabled(self.engine_path.is_some(), |ui: &mut egui::Ui| analysis_toggle(ui, &mut self.continuous))
                .on_hover_text("Continuous analysis (L)")
                .changed()
            {
                self.analysis = None;
                self.lines.clear();
                self.analysis_error = None;
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let settings = ui.button("⚙").on_hover_text("Stockfish settings");
                settings.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, "Stockfish settings"));
                if settings.clicked() {
                    self.engine_settings_open = true;
                }
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
            });
        });
        if !self.lines.is_empty() || self.analysis.is_some() {
            ui.horizontal(|ui| {
                if let Some(line) = self.lines.first() {
                    ui.small(format!("Depth {}", line.depth));
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
                            self.lines.clear();
                        }
                        ui.spinner();
                    });
                }
            });
        }
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
    }

    fn engine_settings(&mut self, ctx: &egui::Context) {
        if !self.engine_settings_open { return; }
        let mut close = false;
        let modal = egui::Modal::new(egui::Id::new("stockfish-settings")).show(ctx, |ui| {
            ui.set_width(360.0);
            ui.heading("Stockfish settings");
            ui.horizontal(|ui| {
                ui.label("Lines");
                let before = self.engine_lines;
                egui::ComboBox::from_id_salt("engine-line-count")
                    .selected_text(self.engine_lines.to_string())
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
            section(ui, "Engine settings", "");
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
            ui.add_space(8.0);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                close = ui.button("Done").clicked();
            });
        });
        if close || modal.should_close() {
            self.engine_settings_open = false;
        }
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
            if report.milliseconds_per_position == 0 {
                ui.small("From the evaluations in the PGN. They name no best moves; Analyze whole game for Stockfish's.");
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
        drag_panel_width(ctx, "notebook", false, &mut self.notebook_width, 260.0..=720.0, ns_string!("NotebookWidth"));
        egui::SidePanel::right("notebook")
            .exact_width(self.notebook_width)
            .resizable(true)
            .show_separator_line(false)
            .frame(egui::Frame::new().fill(PAPER).inner_margin(16))
            .show(ctx, |ui| {
                let ui = &mut ui.new_child(egui::UiBuilder::new().max_rect(ui.max_rect()));
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
                        let mut rebuild = false;
                        let editable = self.play.is_none();
                        if let Some(replay) = &self.replay {
                            let mut next = self.current;
                            egui::ScrollArea::vertical()
                                .id_salt("moves")
                                .max_height(self.moves_height)
                                .auto_shrink([false, false])
                                .show(ui, |ui| {
                                    let Some(loaded) = &mut self.loaded else { return };
                                    let mut sheet = Sheet {
                                        replay,
                                        cloth: &self.bookcloth,
                                        art: &self.piece_art,
                                        flipped: self.flipped,
                                        first_added: loaded.game.first_added(replay),
                                        loaded,
                                        folded: &mut self.folded,
                                        current: self.current,
                                        scroll: self.scrolled_node != Some(self.current),
                                        editable,
                                        next,
                                        rebuild: false,
                                        edited: false,
                                    };
                                    sheet.show(ui);
                                    next = sheet.next;
                                    rebuild = sheet.rebuild;
                                    if sheet.edited {
                                        self.dirty = true;
                                        self.edited = Instant::now();
                                    }
                                });
                            let main = replay.line(0);
                            if !main.contains(&self.current) && ui.button("Return to main line").clicked() {
                                // Back to the game move the side line branched from.
                                next = self.current;
                                while !main.contains(&next) {
                                    next = replay.nodes[next].parent.unwrap();
                                }
                            }
                            self.scrolled_node = Some(self.current);
                            self.current = next;
                            if let Some(warning) = &replay.warning {
                                ui.colored_label(Color32::from_rgb(163, 91, 36), warning);
                            }
                        }
                        // Promoting or deleting changes the tree's order and IDs, so rebuild it from the saved edit.
                        if rebuild && let Some(loaded) = &self.loaded {
                            self.replay = loaded.game.replay().ok();
                            self.scrolled_node = None;
                            self.folded.clear();
                            // Node numbers may have changed, so the board does not glide or click from the old number.
                            self.board_position = Some((loaded.id, self.current));
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
                            resize_rule(ui, &mut self.moves_height, 80.0..=900.0, ns_string!("MovesHeight"));
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
                                if self.current > 0 {
                                    let known = self.page.as_ref().map(|p| p.labels.as_slice()).unwrap_or_default();
                                    let tags = loaded.game.position_labels.entry(self.current).or_default();
                                    let changed = tag_editor(ui, tags, known, &mut self.tag_draft, "position");
                                    if tags.is_empty() {
                                        loaded.game.position_labels.remove(&self.current);
                                    }
                                    if changed {
                                        self.dirty = true;
                                        if self.save() { self.refresh(false); }
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
            let title = self.loaded.as_ref().map(|loaded| if loaded.id < 0 { "Analysis board".into() } else if !loaded.game.tag("Title").is_empty() { loaded.game.tag("Title").to_owned() } else { format!("{} — {}",loaded.game.tag("White"),loaded.game.tag("Black")) });
            // The window is named after the open game, as its heading below.
            let window = title.clone().unwrap_or_else(|| "ChessLib".into());
            if ctx.input(|i| i.viewport().title.as_deref() != Some(window.as_str())) {
                ctx.send_viewport_cmd(egui::ViewportCommand::Title(window));
            }
            let (Some(loaded), Some(title)) = (self.loaded.as_ref(), title) else { return };
            let subtitle = match &self.play {
                Some(play) if play.train.is_some() => "Train on mistakes  ·  find the better move".into(),
                Some(play) => format!("Play vs computer  ·  you play {}", if play.settings.human == Color::White { "White" } else { "Black" }),
                None if loaded.id < 0 => "Not saved to the library  ·  paste a FEN (⌘V) to set up a position".into(),
                None => workspace_subtitle(&loaded.game),
            };
            ui.label(RichText::new("GAME WORKSPACE").size(11.0).strong().color(GREEN));
            let saved = loaded.id >= 0;
            ui.horizontal_wrapped(|ui| {
                ui.heading(title);
                let starred = self.loaded.as_ref().unwrap().starred;
                if self.play.is_none() && !saved && !self.loading && ui.button("Board editor").clicked()
                    && let Some(position) = self.replay.as_ref().and_then(|r| r.nodes[self.current].position.as_ref())
                {
                    self.board_editor = Some(BoardEditor { setup: position.to_setup(EnPassantMode::Always), piece: Some(Color::White.pawn()) });
                }
                if self.play.is_none() && !saved && ui.button("Save to library").clicked() {
                    self.save_analysis(ctx);
                }
                if self.play.is_none() && saved && ui.add(egui::Button::new(RichText::new(if starred { "★" } else { "☆" }).color(if starred { RIBBON } else { MUTED }))).on_hover_text("Favourite this game").clicked() {
                    let loaded = self.loaded.as_mut().unwrap(); loaded.starred = !loaded.starred; self.dirty=true; self.edited=Instant::now();
                    if self.save() { self.refresh(false); }
                }
                if self.play.is_none() && ui.button("New board").on_hover_text("A fresh, unsaved board (⌘N)").clicked() {
                    self.new_board();
                }
            });
            if !subtitle.is_empty() {
                ui.label(RichText::new(subtitle).size(13.0).color(MUTED));
            }
            if self.play.is_none() && saved {
                let known = self.page.as_ref().map(|p| p.labels.as_slice()).unwrap_or_default();
                let loaded = self.loaded.as_mut().unwrap();
                if tag_editor(ui, &mut loaded.game.labels, known, &mut self.tag_draft, "game") {
                    self.dirty = true;
                    if self.save() { self.refresh(false); }
                }
            }
            ui.add_space(14.0);
            if self.loading { ui.spinner(); }
            let Some(replay) = &self.replay else { ui.label("Board unavailable for this game. You can still add game notes and export it."); return; };
            let pos = replay.nodes[self.current].position.as_ref().unwrap();
            // Layout: evaluation bar, a gap, then the clothbound cover with a 22px binding margin
            // that carries the coordinates around the squares.
            const BIND: f32 = 22.0;
            // Room below for the controls row, plus the review graph or its hint line when shown.
            let has_review = self.loaded.as_ref().is_some_and(|l| l.game.review.is_some());
            let reserve = if has_review { 125.0 } else if self.play.is_some() { 52.0 } else { 75.0 };
            // A game with Lichess clocks keeps room right of the board for them.
            let has_clocks = self.loaded.as_ref().is_some_and(|l| l.game.tokens.iter().any(|t| matches!(t, crate::pgn::Token::Comment(c) if c.contains("[%clk "))));
            let room = if has_clocks { 80.0 } else { 0.0 };
            let side = (ui.available_width()-64.0-BIND-room).min(ui.available_height()-reserve-2.0*BIND).max(240.0);
            let cell = side/8.0;
            // Centre the squares in the desk; the evaluation bar hangs to their left, so a narrow window
            // shifts the board right just enough to keep the bar on screen, and left just enough for the clocks.
            let (row,_) = ui.allocate_exact_size(Vec2::new(ui.available_width(),side+2.0*BIND),egui::Sense::hover());
            let left = (row.center().x - side/2.0 - 64.0).min(row.right() - side - 64.0 - BIND - room).max(row.left());
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
            if has_clocks {
                // Each side's clock after its latest move, by its edge of the board as on Lichess; the side to move's in cloth.
                let mut clocks = [None, None];
                let mut node = self.current;
                while let Some(parent) = replay.nodes[node].parent && clocks.iter().any(Option::is_none) {
                    let clock = &mut clocks[replay.half_move(node) % 2];
                    if clock.is_none() { *clock = replay.command(node, "clk"); }
                    node = parent;
                }
                for (white, y, align) in [(self.flipped, squares.top(), egui::Align2::LEFT_TOP), (!self.flipped, squares.bottom(), egui::Align2::LEFT_BOTTOM)] {
                    let Some(clock) = clocks[usize::from(!white)] else { continue };
                    let running = pos.turn().is_white() == white;
                    let text = clock.strip_prefix("0:").unwrap_or(clock);
                    let galley = painter.layout_no_wrap(text.into(), egui::FontId::proportional(16.0), if running { IVORY } else { INK });
                    let chip = align.anchor_size(egui::pos2(cover.right() + 10.0, y), galley.size() + Vec2::new(16.0, 8.0));
                    painter.rect(chip, 4, if running { CLOTH } else { PAPER }, egui::Stroke::new(1.0_f32, CLOTH_SHADOW), egui::StrokeKind::Inside);
                    painter.galley(chip.center() - galley.size() / 2.0, galley, INK);
                }
            }
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
            let square_at = |p: egui::Pos2| {
                let (c, r) = ((((p.x - origin.x) / cell) as usize).min(7), (((p.y - origin.y) / cell) as usize).min(7));
                let (f, rk) = if self.flipped { (7 - c, r) } else { (c, 7 - r) };
                Square::new((rk * 8 + f) as u32)
            };
            // Lichess colours: green, Shift red, Option blue, both yellow.
            let brush = match ctx.input(|i| (i.modifiers.shift, i.modifiers.alt || i.modifiers.ctrl)) {
                (false, false) => 'G',
                (true, false) => 'R',
                (false, true) => 'B',
                (true, true) => 'Y',
            };
            let mut drawn = None;
            let mut sketch = None;
            let center = |s: Square| {
                let (file, rank) = (s.file() as usize, s.rank() as usize);
                let (col, row) = if self.flipped { (7 - file, rank) } else { (file, 7 - rank) };
                origin + Vec2::new((col as f32 + 0.5) * cell, (row as f32 + 0.5) * cell)
            };
            let draw_piece = |piece: shakmaty::Piece, at: egui::Pos2| {
                painter.image(self.piece_art.id(), egui::Rect::from_center_size(at, Vec2::splat(cell)), piece_uv(piece), Color32::WHITE);
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
                    (_, false, false) => Color32::from_rgb(128, 157, 132),
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
                let primary = egui::PointerButton::Primary;
                if response.drag_started_by(primary) && own_piece.is_some() { self.selected_square = Some(square); }
                if self.selected_square == Some(square) && own_piece.is_some() {
                    if response.dragged_by(primary) { dragged = own_piece; }
                    // A drop is handled as a click on the square under the pointer.
                    if response.drag_stopped_by(primary) && let Some(p) = ctx.pointer_interact_pos() && squares.contains(p) {
                        clicked = Some(square_at(p));
                        drop = true;
                    }
                }
                if response.clicked() { clicked = Some(square); }
                let secondary = egui::PointerButton::Secondary;
                if response.secondary_clicked() { drawn = Some((square, square)); }
                if let Some(p) = ctx.pointer_interact_pos().filter(|p| squares.contains(*p)) {
                    if response.dragged_by(secondary) { sketch = Some((square, square_at(p))); }
                    if response.drag_stopped_by(secondary) { drawn = Some((square, square_at(p))); }
                }
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
            // Drawing the same shape again removes it; a new colour replaces the old one.
            // Saved with the game per move, so a left click leaves them alone, as in a Lichess study.
            if let Some((from, to)) = drawn && let Some(loaded) = &mut self.loaded {
                let (from, to) = (u8::from(from), u8::from(to));
                let drawings = loaded.game.drawings.entry(self.current).or_default();
                let existing = drawings.iter().position(|d| (d.0, d.1) == (from, to));
                match existing.map(|i| drawings.remove(i)) {
                    Some(old) if old.2 == brush => {}
                    _ => drawings.push((from, to, brush)),
                }
                if drawings.is_empty() { loaded.game.drawings.remove(&self.current); }
                self.dirty = true;
                self.edited = Instant::now();
            }
            // Returns the arrow's tail and the base of its head, for placing a label between them.
            let arrow_between = |from: Square, to: Square, color: Color32, head: f32, width: f32| {
                let (start, end) = (center(from), center(to));
                let dir = (end - start).normalized();
                let normal = dir.rot90();
                let base = end - dir * head;
                painter.line_segment([start + dir * cell * 0.2, base], egui::Stroke::new(width, color));
                painter.add(egui::Shape::convex_polygon(vec![end, base + normal * head * 0.6, base - normal * head * 0.6], color, egui::Stroke::NONE));
                (start, base)
            };
            let arrow = |movement: Move, color: Color32, head: f32, width: f32| {
                arrow_between(movement.from().unwrap_or(movement.to()), move_destination(movement), color, head, width)
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
            // Where the line branches, an arrow for each next move: the continuation bolder, alternatives fainter.
            let children = &replay.nodes[self.current].children;
            if children.len() > 1 && self.play.is_none() && let Some(before) = &replay.nodes[self.current].position {
                for (i, &child) in children.iter().enumerate().rev() {
                    let Some(movement) = parse(before, &replay.nodes[child].san) else { continue };
                    let (alpha, head, width) = if i == 0 { (200, cell * 0.36, cell * 0.13) } else { (130, cell * 0.3, cell * 0.1) };
                    arrow(movement, Color32::from_rgba_unmultiplied(120, 80, 40, alpha), head, width);
                }
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
            let saved = self.loaded.as_ref().and_then(|l| l.game.drawings.get(&self.current)).map(|d| d.as_slice()).unwrap_or_default();
            let sketch = sketch.map(|(from, to): (Square, Square)| (u8::from(from), u8::from(to), brush));
            for &(from, to, brush) in saved.iter().chain(sketch.iter()) {
                let (from, to) = (Square::new(from.into()), Square::new(to.into()));
                let color = match brush {
                    'R' => Color32::from_rgba_unmultiplied(136, 32, 32, 200),
                    'B' => Color32::from_rgba_unmultiplied(0, 48, 136, 200),
                    'Y' => Color32::from_rgba_unmultiplied(230, 143, 0, 200),
                    _ => Color32::from_rgba_unmultiplied(21, 120, 27, 200),
                };
                if from == to {
                    painter.circle_stroke(center(from), cell * 0.46, egui::Stroke::new(cell * 0.07, color));
                } else {
                    arrow_between(from, to, color, cell * 0.36, cell * 0.14);
                }
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
                if ui.add_enabled(parent.is_some(),egui::Button::new("←")).on_hover_text("Previous move · Left arrow").clicked() { self.current=step_back(replay, self.current, self.play.as_ref().filter(|p| p.train.is_none()).map(|p| p.settings.human)); }
                ui.label(format!("{move_number} / {line_length}"));
                if ui.add_enabled(next.is_some(),egui::Button::new("→")).on_hover_text("Next move · Right arrow").clicked() { self.current=next.unwrap_or(self.current); }
                if ui.add_enabled(self.current!=last,egui::Button::new("→|")).on_hover_text("Last position · End").clicked() { self.current=last; }
                if ui.button("Flip").on_hover_text("Flip the board (F)").clicked() {
                    // Field by field: `self.flip()` would clash with the borrowed replay.
                    self.flipped = !self.flipped;
                    if let Some(loaded) = &mut self.loaded { loaded.game.flipped = self.flipped; self.dirty = true; self.edited = Instant::now(); }
                }
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
        } else if ctx.input(|i| i.viewport().close_requested()) && self.relaunch {
            // Saved above, so the new app can open its library; CHESSLIB_DB would override the chosen folder.
            let spawned = std::env::current_exe()
                .and_then(|exe| std::process::Command::new(exe).env_remove("CHESSLIB_DB").spawn());
            if let Err(e) = spawned {
                ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
                self.relaunch = false;
                self.error = Some(format!("The library folder is saved, but ChessLib could not restart: {e}. Quit and open it again."));
            }
        }
        let jump_opened = self.board_editor.is_none() && !self.engine_settings_open && self.play.is_none() && ctx.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::P));
        if self.board_editor.is_none() && !self.engine_settings_open && ctx.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::O)) {
            self.choose_import(ctx);
        }
        if self.board_editor.is_none() && !self.engine_settings_open && self.play.is_none() && ctx.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::N)) {
            self.new_board();
        }
        if self.board_editor.is_none() && !self.engine_settings_open && ctx.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::S)) {
            self.save();
        }
        if jump_opened {
            self.jump = Some((String::new(), library::jump(&self.db, "").unwrap_or_default(), 0));
        }
        let plain = |key| ctx.input(|i| i.modifiers.is_none() && i.key_pressed(key));
        if self.board_editor.is_none() && !self.engine_settings_open && !ctx.wants_keyboard_input() {
            if plain(egui::Key::F) {
                self.flip();
            }
            if plain(egui::Key::P) && self.play.is_none() && self.play_setup.is_none() && self.engine_path.is_some() {
                self.play_setup = Some(NEW_PLAY);
            }
            if plain(egui::Key::L) && self.engine_path.is_some() {
                self.continuous = !self.continuous;
                self.analysis = None;
                self.lines.clear();
                self.analysis_error = None;
            }
            // Space plays the engine's top move, like "Play best", while analysis runs outside play/training.
            if plain(egui::Key::Space)
                && self.play.is_none()
                && self.analysis.is_some()
                && !self.loading
                && let Some(best) = self.lines.first().and_then(|e| e.best)
            {
                self.play(best);
            }
        }
        if self.board_editor.is_none() && !self.engine_settings_open && !ctx.wants_keyboard_input()
            && let Some(replay) = &self.replay
        {
            if ctx.input(|i| i.key_pressed(egui::Key::ArrowRight)) {
                self.current = replay.next(self.current).unwrap_or(self.current);
            }
            if ctx.input(|i| i.key_pressed(egui::Key::ArrowLeft)) {
                self.current = step_back(replay, self.current, self.play.as_ref().filter(|p| p.train.is_none()).map(|p| p.settings.human));
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
        if self.board_editor.is_none() && !self.engine_settings_open && !drops.is_empty() {
            self.import_files(drops, ctx);
        }
        // ⌘V outside a text field imports pasted PGN or FEN (a FEN on the analysis board sets it up instead); the importer reads files, so stage it in one.
        let pasted = if self.board_editor.is_some() || self.engine_settings_open || ctx.wants_keyboard_input() || self.importing { None } else {
            ctx.input(|i| i.events.iter().find_map(|e| match e {
                egui::Event::Paste(text) if !text.trim().is_empty() => Some(text.clone()),
                _ => None,
            }))
        };
        if let Some(text) = pasted {
            let on_board = self.play.is_none() && self.loaded.as_ref().is_some_and(|l| l.id < 0);
            match text.trim().parse::<Fen>() {
                // On the analysis board a FEN sets up the position; save it from there if it is worth keeping.
                Ok(fen) if on_board => match fen.into_position(shakmaty::CastlingMode::Standard) {
                    Ok(position) => self.show_analysis_board(Some(&position)),
                    Err(e) => self.error = Some(format!("That FEN is not a legal position: {e}")),
                },
                _ => {
                    let path = std::env::temp_dir().join("chesslib-paste.pgn");
                    match std::fs::write(&path, pasted_pgn(text)) { Ok(()) => self.import_files(vec![path], ctx), Err(e) => self.error = Some(e.to_string()) }
                }
            }
        }
        let cloth = self.bookcloth.clone();
        egui::TopBottomPanel::top("toolbar")
            .show_separator_line(false)
            .frame(
                egui::Frame::new()
                    .fill(CLOTH)
                    .inner_margin(egui::Margin::symmetric(22, 7)),
            )
            .show(ctx, |ui| {
                let rect = ui.max_rect().expand2(Vec2::new(22.0, 7.0));
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
                    ui.image((self.header_icon.id(), Vec2::splat(30.0)));
                    embossed(ui, "CHESSLIB", 19.0, IVORY);
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
                                self.play_setup = Some(NEW_PLAY);
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
        if self.loaded.is_none() && self.play.is_none() {
            self.show_analysis_board(None);
        }
        self.notebook(ctx);
        self.position_changed();
        self.board(ctx);
        self.position_changed();
        self.edit_board(ctx);
        // Esc cancels the dialogs drawn as windows; egui's modals close on it by themselves.
        let escape = ctx.input(|i| i.key_pressed(egui::Key::Escape));
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
            } else if cancel || escape {
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
                    ui.label("Puzzles from blunders in every game you have reviewed.");
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
                self.start_training(None);
            } else if cancel || escape {
                self.train_setup = false;
            }
        }
        self.engine_settings(ctx);
        if let Some((edit, mut name)) = self.folder_edit.take() {
            let (mut save, mut cancel) = (false, false);
            let modal = egui::Modal::new(egui::Id::new("folder")).show(ctx, |ui| {
                ui.set_width(340.0);
                ui.heading(match edit {
                    FolderEdit::New(None) => "New folder",
                    FolderEdit::New(Some(_)) => "New subfolder",
                    FolderEdit::Rename(_) => "Rename folder",
                });
                let field = ui.add(egui::TextEdit::singleline(&mut name).hint_text("Folder name").desired_width(f32::INFINITY));
                field.request_focus();
                save = field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    save |= ui.add_enabled(!name.trim().is_empty(), egui::Button::new("Save").fill(GREEN)).clicked();
                    cancel = ui.button("Cancel").clicked();
                });
            });
            // Trim only on save; trimming while typing would swallow each space as it is typed.
            let trimmed = name.trim();
            if save && !trimmed.is_empty() {
                let name = trimmed.to_owned();
                let result = match edit {
                    FolderEdit::New(parent) => library::create_folder(&self.db, parent, &name).map(|_| {
                        // Open the parent so the new folder shows.
                        if let Some(parent) = parent { self.collapsed.remove(&parent); }
                    }),
                    FolderEdit::Rename(id) => library::rename_folder(&self.db, id, &name),
                };
                if let Err(e) = result {
                    self.error = Some(format!("Could not save the folder: {e}"));
                }
                self.refresh(false);
            } else if !cancel && !modal.should_close() {
                self.folder_edit = Some((edit, name));
            }
        }
        if let Some((id, title)) = &self.delete_game {
            let (id, mut delete, mut cancel) = (*id, false, false);
            let modal = egui::Modal::new(egui::Id::new("delete")).show(ctx, |ui| {
                ui.set_width(380.0);
                ui.heading("Delete game?");
                ui.label(RichText::new(title).strong());
                ui.label("Its notes and analysis are deleted too. This cannot be undone.");
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    delete = ui
                        .add(egui::Button::new(RichText::new("Delete").color(Color32::WHITE)).fill(RIBBON).stroke(egui::Stroke::new(1.0_f32, RIBBON_LIGHT)))
                        .clicked();
                    cancel = ui.button("Cancel").clicked();
                });
            });
            if delete {
                self.delete(id);
            }
            if delete || cancel || modal.should_close() {
                self.delete_game = None;
            }
        }
        if let Some((query, rows, highlight)) = &mut self.jump {
            let (mut open, mut close) = (None, false);
            let modal = egui::Modal::new(egui::Id::new("jump")).show(ctx, |ui| {
                ui.set_width(460.0_f32.min(ctx.content_rect().width() - 32.0));
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
                let query_changed = field.changed();
                if query_changed {
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
                // The modal's layout starts at its previous height, which may fit only one result.
                let mut results_rect = ui.available_rect_before_wrap();
                results_rect.max.y = results_rect.min.y + (ctx.content_rect().height() - 120.0).clamp(80.0, 500.0);
                ui.scope_builder(egui::UiBuilder::new().max_rect(results_rect), |ui| {
                    let mut scroll = egui::ScrollArea::vertical().auto_shrink([false, true]);
                    if jump_opened || query_changed {
                        scroll = scroll.vertical_scroll_offset(0.0);
                    }
                    scroll.show(ui, |ui| {
                        ui.spacing_mut().item_spacing.y = 4.0;
                        for (i, row) in rows.iter().enumerate() {
                            let selected = i == *highlight;
                            let response = jump_card(ui, row, selected);
                            if selected && (up || down) {
                                response.scroll_to_me(None);
                            }
                            if response.clicked() {
                                open = Some(row.id);
                            }
                        }
                    });
                });
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
                    for (field, (label, hint)) in fields.iter_mut().zip([("Title", "Optional, e.g. Bird trap line"), ("White", "Player name"), ("Black", "Player name"), ("Event", "Tournament or match"), ("Date", "YYYY.MM.DD")]) {
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
                for (key, value) in ["Title", "White", "Black", "Event", "Date"].into_iter().zip(fields.iter()) {
                    loaded.game.set_header(key, value);
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
                    if ui.button("Dismiss").clicked() || escape {
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

/// The position before `id`; against the computer, back to the player's previous turn so they can play on from it.
fn step_back(replay: &Replay, id: usize, human: Option<Color>) -> usize {
    let mut node = replay.nodes[id].parent.unwrap_or(0);
    while let Some(parent) = replay.nodes[node].parent
        && human.is_some_and(|human| replay.nodes[node].position.as_ref().is_some_and(|p| p.turn() != human))
    {
        node = parent;
    }
    node
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

fn analysis_toggle(ui: &mut egui::Ui, on: &mut bool) -> egui::Response {
    let (rect, mut response) = ui.allocate_exact_size(Vec2::new(28.0, 24.0), egui::Sense::click());
    if response.clicked() {
        *on = !*on;
        response.mark_changed();
    }
    response.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::Checkbox, ui.is_enabled(), *on, "Continuous analysis"));
    let visuals = ui.style().interact_selectable(&response, *on);
    let track = egui::Rect::from_center_size(rect.center(), Vec2::new(28.0, 16.0));
    ui.painter().rect(track, 8.0, if *on { GREEN } else { visuals.bg_fill }, visuals.bg_stroke, egui::StrokeKind::Inside);
    let x = if *on { track.right() - 8.0 } else { track.left() + 8.0 };
    ui.painter().circle_filled(egui::pos2(x, track.center().y), 5.0, PAPER);
    response
}

#[cfg(test)]
#[test]
fn analysis_toggle_clicks_and_disabled_state() {
    let ctx = egui::Context::default();
    let mut on = false;
    let mut frame = |events, enabled| {
        let mut response = None;
        let _ = ctx.run(egui::RawInput { events, ..Default::default() }, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                response = Some(ui.add_enabled(enabled, |ui: &mut egui::Ui| analysis_toggle(ui, &mut on)));
            });
        });
        (response.unwrap(), on)
    };
    let (response, _) = frame(vec![], true);
    let pos = response.rect.center();
    let click = || vec![
        egui::Event::PointerMoved(pos),
        egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed: true, modifiers: egui::Modifiers::NONE },
        egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed: false, modifiers: egui::Modifiers::NONE },
    ];
    let (response, on) = frame(click(), true);
    assert!(response.changed() && on);
    let (response, on) = frame(click(), false);
    assert!(!response.changed() && on);
    frame(vec![], true);
    let (response, on) = frame(click(), true);
    assert!(response.changed() && !on);
}

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

/// A move mark's symbol, colour and name, for NAGs 1–6; the colours follow `quality_style`.
/// The move list: the game's moves as a two-column scoresheet, broken wherever side lines or comments branch off.
/// Side lines follow as indented blocks of clickable moves, nested lines indented again.
struct Sheet<'a> {
    replay: &'a Replay,
    /// For the board preview when hovering a move.
    cloth: &'a egui::TextureHandle,
    art: &'a egui::TextureHandle,
    flipped: bool,
    loaded: &'a mut Loaded,
    /// Branch points whose side lines are folded into a one-line summary.
    folded: &'a mut HashSet<usize>,
    first_added: usize,
    current: usize,
    /// Scroll the current move into view; only once per move, so the list can still be scrolled by hand.
    scroll: bool,
    editable: bool,
    next: usize,
    rebuild: bool,
    edited: bool,
}

/// One piece of the scoresheet, in reading order.
#[derive(Debug, PartialEq)]
enum Entry {
    /// A move number with White's and Black's moves; a missing White move shows "…" after a break.
    Row(usize, Option<usize>, Option<usize>),
    Comment(usize),
    /// Side lines from a node: its children except the game's next move.
    Branches(usize, Option<usize>),
}

impl Sheet<'_> {
    fn show(&mut self, ui: &mut egui::Ui) {
        let replay = self.replay;
        ui.spacing_mut().item_spacing = Vec2::new(4.0, 2.0);
        ui.spacing_mut().button_padding = Vec2::new(8.0, 3.0);
        let main = replay.line(0);
        let last = *main.last().unwrap();
        let forks: Vec<usize> = (0..replay.nodes.len())
            .filter(|&p| replay.nodes[p].children.len() > 1 || (p == last && !replay.nodes[p].children.is_empty()))
            .collect();
        ui.horizontal(|ui| {
            let start = ui.selectable_label(self.current == 0, "Starting position");
            if start.clicked() {
                self.next = 0;
            }
            if self.current == 0 && self.scroll {
                start.scroll_to_me(Some(egui::Align::Center));
            }
            if !forks.is_empty() {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let fold = self.folded.is_empty();
                    let label = RichText::new(if fold { "Fold side lines" } else { "Unfold side lines" }).size(12.0).color(MUTED);
                    if ui.add(egui::Button::new(label).frame(false)).clicked() {
                        if fold {
                            self.folded.extend(&forks);
                        } else {
                            self.folded.clear();
                        }
                    }
                });
            }
        });
        let entries = scoresheet(replay);
        let height = ui.text_style_height(&egui::TextStyle::Button) + 2.0 * ui.spacing().button_padding.y;
        let cell = Vec2::new(((ui.available_width() - 36.0) / 2.0).floor(), height);
        let mut stripe = false;
        for entry in entries {
            match entry {
                Entry::Row(number, white, black) => {
                    let background = ui.painter().add(egui::Shape::Noop);
                    let rect = ui
                        .horizontal(|ui| {
                            ui.add_sized([24.0, height], egui::Label::new(RichText::new(format!("{number}.")).color(MUTED)));
                            match white {
                                Some(id) => {
                                    self.token(ui, id, false, Some(cell));
                                }
                                None => {
                                    ui.allocate_ui_with_layout(cell, egui::Layout::left_to_right(egui::Align::Center), |ui| {
                                        ui.set_min_size(cell);
                                        ui.add_space(ui.spacing().button_padding.x);
                                        ui.label(RichText::new("…").color(MUTED));
                                    });
                                }
                            }
                            if let Some(id) = black {
                                self.token(ui, id, false, Some(cell));
                            }
                        })
                        .response
                        .rect;
                    if stripe {
                        let rect = rect.with_max_x(ui.max_rect().right());
                        ui.painter().set(background, egui::Shape::rect_filled(rect, 4, ui.visuals().faint_bg_color));
                    }
                    stripe = !stripe;
                }
                Entry::Comment(id) => {
                    egui::Frame::new().inner_margin(egui::Margin { left: 30, right: 4, top: 1, bottom: 3 }).show(ui, |ui| {
                        self.comments(ui, id);
                    });
                }
                Entry::Branches(parent, skip) => self.branches(ui, parent, skip, 1),
            }
        }
    }

    /// The side lines from `parent` as one indented block, or as a one-line summary when folded.
    fn branches(&mut self, ui: &mut egui::Ui, parent: usize, skip: Option<usize>, depth: usize) {
        let replay = self.replay;
        let lines: Vec<usize> = replay.nodes[parent].children.iter().copied().filter(|&c| Some(c) != skip).collect();
        if lines.is_empty() {
            return;
        }
        let indent = if depth == 1 { 28.0 } else { 0.0 };
        // A folded block still opens while the current move is inside it.
        let mut node = self.current;
        let mut inside = false;
        while let Some(up) = replay.nodes[node].parent {
            if up == parent && lines.contains(&node) {
                inside = true;
                break;
            }
            node = up;
        }
        let tint = Color32::from_rgba_unmultiplied(203, 185, 155, 40);
        let color = if depth == 1 { RIBBON } else { BEVEL };
        if self.folded.contains(&parent) && !inside {
            let mut summary: Vec<String> = lines.iter().take(3).map(|&c| replay.label(c)).collect();
            if lines.len() > 3 {
                summary.push(format!("+{} more", lines.len() - 3));
            }
            let pill = egui::Frame::new()
                .fill(tint)
                .corner_radius(4)
                .inner_margin(egui::Margin { left: 18, right: 8, top: 1, bottom: 1 })
                .outer_margin(egui::Margin { left: indent as i8, ..Default::default() })
                .show(ui, |ui| ui.label(RichText::new(summary.join("  ·  ")).size(12.0).italics().color(MUTED)))
                .response
                .rect;
            let pill = pill.with_min_x(pill.min.x + indent);
            let open = ui.interact(pill, ui.id().with(("unfold", parent)), egui::Sense::click()).on_hover_text("Show these side lines");
            triangle(ui.painter(), egui::pos2(pill.min.x + 9.0, pill.center().y), false, if open.hovered() { INK } else { color });
            if open.clicked() {
                self.folded.remove(&parent);
            }
            return;
        }
        let mut ticks = Vec::new();
        let block = egui::Frame::new()
            // Low alpha, so each nested block reads a shade darker than the one around it.
            .fill(tint)
            .corner_radius(4)
            .inner_margin(egui::Margin { left: 18, right: 4, top: 3, bottom: 3 })
            .outer_margin(egui::Margin { left: indent as i8, ..Default::default() })
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                for &start in &lines {
                    ticks.push(self.line(ui, start, depth));
                }
            })
            .response
            .rect;
        let block = block.with_min_x(block.min.x + indent);
        // The guide strip on the left is a tree of the lines, and folds them when clicked.
        let gutter = ui
            .interact(egui::Rect::from_min_size(block.min, Vec2::new(16.0, block.height())), ui.id().with(("fold", parent)), egui::Sense::click())
            .on_hover_text("Fold these side lines");
        let stroke = egui::Stroke::new(1.5_f32, if gutter.hovered() { INK } else { color.gamma_multiply(0.55) });
        let x = block.min.x + 8.5;
        let last = ticks.last().copied().unwrap_or(block.top());
        ui.painter().vline(x, (block.top() + 4.0)..=last, stroke);
        for y in ticks {
            ui.painter().hline(x..=x + 6.0, y, stroke);
        }
        if gutter.hovered() {
            triangle(ui.painter(), egui::pos2(x, block.top() + 6.0), true, INK);
        }
        if gutter.clicked() {
            self.folded.insert(parent);
        }
    }

    /// One side line from `start` along its first moves; its own side lines break it into nested blocks.
    fn line(&mut self, ui: &mut egui::Ui, start: usize, depth: usize) -> f32 {
        let replay = self.replay;
        let mut tick = None;
        let mut from = Some(start);
        while let Some(first) = from {
            let fork = ui
                .horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing = Vec2::new(1.0, 2.0);
                    ui.spacing_mut().button_padding = Vec2::new(3.0, 2.0);
                    let (mut id, mut numbered) = (first, true);
                    loop {
                        let rect = self.token(ui, id, numbered, None);
                        tick.get_or_insert(rect.center().y);
                        numbered = self.comments(ui, id);
                        // Moves branching off here are shown right after this one, before the line goes on.
                        if id != start && replay.nodes[replay.nodes[id].parent.unwrap()].children.len() > 1 {
                            return Some(id);
                        }
                        match replay.nodes[id].children.first() {
                            Some(&child) => id = child,
                            None => return None,
                        }
                    }
                })
                .inner;
            from = fork.and_then(|id| {
                self.branches(ui, replay.nodes[id].parent.unwrap(), Some(id), depth + 1);
                replay.nodes[id].children.first().copied()
            });
        }
        tick.unwrap_or_default()
    }

    /// The PGN's comments after a move, in the reader's grey; returns whether there were any.
    fn comments(&self, ui: &mut egui::Ui, id: usize) -> bool {
        let mut any = false;
        for text in self.replay.comments(id) {
            ui.label(RichText::new(text).size(12.5).italics().color(MUTED));
            any = true;
        }
        any
    }

    /// A clickable move with its mark, tag and note signs: in a scoresheet `cell`, or inline in a side line with its
    /// number (`numbered` also numbers a Black move). Right-click marks it, double-click bookmarks it.
    fn token(&mut self, ui: &mut egui::Ui, id: usize, numbered: bool, cell: Option<Vec2>) -> egui::Rect {
        let replay = self.replay;
        let node = &replay.nodes[id];
        let note = self.loaded.notes.get(&id).map(|n| n.trim()).filter(|n| !n.is_empty()).map(str::to_owned);
        let tag = self.loaded.game.position_labels.get(&id).and_then(|t| t.first()).cloned();
        // Your mark, else the PGN's, else the game review's verdict, as on Lichess; a removed mark hides all.
        let game = &self.loaded.game;
        let glyph = replay
            .glyph(&game.glyphs, id)
            .or_else(|| {
                let review = game.review.as_ref().filter(|_| !game.glyphs.contains_key(&id))?;
                match review.moves.iter().find(|m| m.node == id)?.quality {
                    engine::Quality::Inaccuracy => Some(6),
                    engine::Quality::Mistake => Some(2),
                    engine::Quality::Blunder => Some(4),
                    _ => None,
                }
            })
            .and_then(glyph_style);
        let side = cell.is_none();
        let size = if side { 13.0 } else { 14.0 };
        // Game moves in ink, the PGN's side lines a shade lighter, your own analysis in green.
        let ink = if replay.in_game(id) {
            INK
        } else if id >= self.first_added {
            GREEN
        } else {
            Color32::from_rgb(84, 78, 62)
        };
        let mut job = egui::text::LayoutJob::default();
        let add = |job: &mut egui::text::LayoutJob, text: RichText| {
            text.append_to(job, ui.style(), egui::FontSelection::Default, egui::Align::Center);
        };
        let half = replay.half_move(id);
        if side && (half.is_multiple_of(2) || numbered) {
            let dots = if half.is_multiple_of(2) { "." } else { "..." };
            add(&mut job, RichText::new(format!("{}{dots} ", half / 2 + 1)).size(size).color(MUTED));
        }
        add(&mut job, RichText::new(&node.san).size(size).color(ink));
        if let Some((symbol, color, _)) = glyph {
            add(&mut job, RichText::new(symbol).size(size).strong().color(color));
        }
        // A diamond in the position's tag colour, then a dot for a note.
        let mut signs = egui::text::LayoutJob::default();
        let target = if side { &mut job } else { &mut signs };
        if let Some(tag) = &tag {
            add(target, RichText::new(" ◆").size(9.0).color(tag_color(tag)));
        }
        if note.is_some() {
            add(target, RichText::new(" •").color(RIBBON));
        }
        // The clock left after the move, as Lichess exports record it; the hour goes while it is zero.
        if let Some(clock) = replay.command(id, "clk").filter(|_| !side) {
            add(&mut signs, RichText::new(format!("  {}", clock.strip_prefix("0:").unwrap_or(clock))).size(11.0).color(MUTED));
        }
        let mut button = egui::Button::selectable(self.current == id, job);
        if let Some(cell) = cell {
            button = button.min_size(cell).right_text(signs);
        }
        // Hovering shows the position after the move, and its note, without leaving the current move.
        let response = ui.add_enabled(node.position.is_some(), button).on_disabled_hover_text("This move from the PGN is illegal here");
        let response = match &node.position {
            Some(position) => response.on_hover_ui(|ui| {
                ui.set_max_width(184.0);
                ui.label(RichText::new(replay.label(id)).strong());
                let (rect, _) = ui.allocate_exact_size(Vec2::splat(184.0), egui::Sense::hover());
                mini_board(ui.painter(), self.cloth, self.art, rect, position.board(), self.flipped);
                if let Some(note) = &note {
                    let preview: String = note.chars().take(280).collect();
                    ui.label(if preview.len() < note.len() { format!("{preview}…") } else { preview });
                }
            }),
            None => response,
        };
        if self.current == id && self.scroll {
            response.scroll_to_me(Some(egui::Align::Center));
        }
        if response.clicked() {
            self.next = id;
        }
        let loaded = &mut *self.loaded;
        // Right-click a move to mark it, as Lichess's ! ? buttons.
        response.context_menu(|ui| {
            let mut chosen = None;
            for nag in [3, 1, 5, 6, 2, 4] {
                let (symbol, color, name) = glyph_style(nag).unwrap();
                let text = RichText::new(format!("{symbol}  {name}")).color(color);
                if ui.add(egui::Button::selectable(glyph.is_some_and(|g| g.0 == symbol), text)).clicked() {
                    chosen = Some(nag);
                }
            }
            if glyph.is_some() && ui.button("Remove mark").clicked() {
                chosen = Some(0);
            }
            if let Some(nag) = chosen {
                loaded.game.glyphs.insert(id, nag);
                self.edited = true;
                ui.close();
            }
            if !self.editable {
                return;
            }
            let parent = node.parent.unwrap();
            let first = replay.nodes[parent].children[0];
            let promotable = first != id && !replay.in_game(first) && node.position.is_some();
            let branches = replay.branches_to(id);
            let to_main = node.position.is_some() && !branches.is_empty();
            let added = id >= self.first_added;
            if promotable || to_main || added {
                ui.separator();
            }
            // When this branch is the only one in the way, promoting it already makes the main line.
            if promotable && branches != [id] && ui.button("Promote variation").on_hover_text("Make this move the continuation of its line").clicked() {
                loaded.game.promote(replay, id);
                self.rebuild = true;
            }
            if to_main
                && ui.button("Set as main line").on_hover_text("Make this whole line the main line. If it replaces the game's own moves, those become a side line.").clicked()
            {
                self.next = loaded.game.set_main_line(replay, &mut loaded.notes, id).unwrap_or(id);
                self.rebuild = true;
            }
            if added && ui.button("Delete from here").on_hover_text("Remove this move and everything after it, with their notes").clicked() {
                loaded.game.delete_line(replay, &mut loaded.notes, id);
                self.next = parent;
                self.rebuild = true;
            }
            if self.rebuild {
                self.edited = true;
                ui.close();
            }
        });
        if response.double_clicked() {
            let marks = &mut loaded.game.bookmarks;
            if !marks.remove(&id) {
                marks.insert(id);
            }
            self.edited = true;
        }
        // A ribbon strip inside the move's left padding marks a bookmark.
        if loaded.game.bookmarks.contains(&id) {
            let strip = egui::Rect::from_min_size(response.rect.left_top(), Vec2::new(if side { 2.0 } else { 3.0 }, response.rect.height()));
            ui.painter().rect_filled(strip, 1, RIBBON);
        }
        response.rect
    }
}

/// The scoresheet's rows, with the comments and side lines that break them, for the game's main line.
fn scoresheet(replay: &Replay) -> Vec<Entry> {
    let mut main = replay.line(0);
    // Analysis played on from the game's last move continues the columns, so a fresh board's moves are not a side line.
    while let Some(next) = replay.next(*main.last().unwrap()) {
        main.push(next);
    }
    let last = *main.last().unwrap();
    let commented = |id: usize| replay.comments(id).next().is_some();
    let mut entries = Vec::new();
    if commented(0) {
        entries.push(Entry::Comment(0));
    }
    let mut row: Option<(usize, Option<usize>, Option<usize>)> = None;
    for &id in &main[1..] {
        let half = replay.half_move(id);
        let number = half / 2 + 1;
        if half.is_multiple_of(2) {
            entries.extend(row.take().map(|(n, w, b)| Entry::Row(n, w, b)));
            row = Some((number, Some(id), None));
        } else if let Some((n, _, black)) = &mut row
            && *n == number
        {
            *black = Some(id);
        } else {
            entries.extend(row.take().map(|(n, w, b)| Entry::Row(n, w, b)));
            row = Some((number, None, Some(id)));
        }
        let parent = replay.nodes[id].parent.unwrap();
        let forks = replay.nodes[parent].children.len() > 1;
        if half % 2 == 1 || forks || commented(id) {
            entries.extend(row.take().map(|(n, w, b)| Entry::Row(n, w, b)));
        }
        if commented(id) {
            entries.push(Entry::Comment(id));
        }
        if forks {
            entries.push(Entry::Branches(parent, Some(id)));
        }
    }
    entries.extend(row.take().map(|(n, w, b)| Entry::Row(n, w, b)));
    // A first move after the end that the board cannot play.
    if !replay.nodes[last].children.is_empty() {
        entries.push(Entry::Branches(last, None));
    }
    entries
}

/// The cloth squares and piece art of `board` in `rect`, as the board editor and move previews draw it.
fn mini_board(painter: &egui::Painter, cloth: &egui::TextureHandle, art: &egui::TextureHandle, rect: egui::Rect, board: &shakmaty::Board, flipped: bool) {
    let cell = rect.width() / 8.0;
    for row in 0..8 {
        for col in 0..8 {
            let (file, rank) = if flipped { (7 - col, row) } else { (col, 7 - row) };
            let square = egui::Rect::from_min_size(rect.min + Vec2::new(col as f32 * cell, row as f32 * cell), Vec2::splat(cell));
            let tint = if (row + col) % 2 == 0 { Color32::from_rgb(255, 250, 231) } else { Color32::from_rgb(128, 157, 132) };
            let uv = egui::Rect::from_min_size(egui::pos2(col as f32 * 0.5, row as f32 * 0.5), Vec2::splat(0.5));
            painter.image(cloth.id(), square, uv, tint);
            if let Some(piece) = board.piece_at(Square::new((rank * 8 + file) as u32)) {
                painter.image(art.id(), square, piece_uv(piece), Color32::WHITE);
            }
        }
    }
}

/// A small filled triangle pointing down (open) or right (folded); the fonts lack ▾ and ▸.
fn triangle(painter: &egui::Painter, center: egui::Pos2, down: bool, color: Color32) {
    let (a, b) = (3.5, 2.2);
    let points = if down {
        vec![center + Vec2::new(-a, -b), center + Vec2::new(a, -b), center + Vec2::new(0.0, b + 1.0)]
    } else {
        vec![center + Vec2::new(-b, -a), center + Vec2::new(b + 1.0, 0.0), center + Vec2::new(-b, a)]
    };
    painter.add(egui::Shape::convex_polygon(points, color, egui::Stroke::NONE));
}

fn glyph_style(nag: u8) -> Option<(&'static str, Color32, &'static str)> {
    Some(match nag {
        1 => ("!", Color32::from_rgb(110, 122, 92), "Good move"),
        2 => ("?", RIBBON, "Mistake"),
        3 => ("!!", CLOTH, "Brilliant move"),
        4 => ("??", Color32::from_rgb(128, 28, 24), "Blunder"),
        5 => ("!?", Color32::from_rgb(70, 110, 150), "Interesting move"),
        6 => ("?!", Color32::from_rgb(184, 138, 40), "Dubious move"),
        _ => return None,
    })
}

fn piece_uv(piece: Piece) -> egui::Rect {
    let column = match piece.role {
        Role::King => 0,
        Role::Queen => 1,
        Role::Bishop => 2,
        Role::Knight => 3,
        Role::Rook => 4,
        Role::Pawn => 5,
    };
    let top = if piece.color == Color::White { 0.0 } else { 0.5 };
    egui::Rect::from_min_max(egui::pos2(column as f32 / 6.0, top), egui::pos2((column + 1) as f32 / 6.0, top + 0.5))
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
    use crate::pgn::Token;

    #[test]
    fn scoresheet_breaks_rows_for_side_lines_and_comments_and_continues_into_analysis() {
        // 1. e4 (1. d4) e5 (1... c5 2. Nf3) 2. Nf3 {Develops.} Nc6, then the reader's 3. Bb5.
        let m = |san: &str| Token::Move(san.into());
        let mut game = Game::default();
        game.tokens = vec![m("e4"), Token::Open, m("d4"), Token::Close, m("e5"), Token::Open, m("c5"), m("Nf3"), Token::Close, m("Nf3"), Token::Comment("Develops.".into()), m("Nc6")];
        game.study.push(crate::pgn::StudyMove { parent: 4, san: "Bb5".into() });
        let replay = game.replay().unwrap();
        assert_eq!(
            scoresheet(&replay),
            [
                Entry::Row(1, Some(1), None),
                Entry::Branches(0, Some(1)),
                Entry::Row(1, None, Some(2)),
                Entry::Branches(1, Some(2)),
                Entry::Row(2, Some(3), None),
                Entry::Comment(3),
                Entry::Row(2, None, Some(4)),
                Entry::Row(3, Some(8), None),
            ]
        );
    }

    #[test]
    fn board_editor_placement_clears_stale_rights() {
        let mut editor = BoardEditor { setup: Setup::initial(), piece: None };
        editor.setup.ep_square = Some(Square::E3);
        editor.setup.halfmoves = 12;
        editor.place(Square::H1, None);
        assert_eq!(editor.setup.board.piece_at(Square::H1), None);
        assert!(!editor.setup.castling_rights.contains(Square::H1));
        assert!(editor.setup.castling_rights.contains(Square::A1));
        assert_eq!(editor.setup.ep_square, None);
        assert_eq!(editor.setup.halfmoves, 0);
        editor.place(Square::H1, Some(Color::White.rook()));
        assert!(!editor.setup.castling_rights.contains(Square::H1));
        editor.place(Square::E1, Some(Color::White.queen()));
        assert!(!editor.setup.castling_rights.contains(Square::A1));
        assert!(editor.setup.castling_rights.contains(Square::A8));
        assert!(editor.setup.castling_rights.contains(Square::H8));
    }

    #[test]
    fn board_editor_rejects_invalid_positions_and_exports_a_playable_setup() {
        let mut editor = BoardEditor { setup: Setup::empty(), piece: None };
        let validate = |setup: &Setup| setup.clone().position::<shakmaty::Chess>(shakmaty::CastlingMode::Standard).map_err(|e| e.to_string());
        assert!(validate(&editor.setup).is_err());
        editor.place(Square::E1, Some(Color::White.king()));
        editor.place(Square::E8, Some(Color::Black.king()));
        editor.place(Square::A8, Some(Color::White.pawn()));
        assert!(validate(&editor.setup).is_err());
        editor.place(Square::A8, None);
        editor.place(Square::E2, Some(Color::Black.king()));
        assert!(validate(&editor.setup).is_err());
        editor.place(Square::E2, None);
        editor.place(Square::D4, Some(Color::Black.pawn()));
        editor.place(Square::E4, Some(Color::White.pawn()));
        editor.setup.turn = Color::Black;
        editor.setup.ep_square = Some(Square::E3);
        editor.setup.fullmoves = std::num::NonZeroU32::new(20).unwrap();
        let position = validate(&editor.setup).unwrap();
        let fen = Fen::from_position(&position, EnPassantMode::Legal).to_string();
        assert_eq!(fen, "4k3/8/8/8/3pP3/8/8/4K3 b - e3 0 20");
        let mut game = Game::default();
        game.set_tag("SetUp", "1".into());
        game.set_tag("FEN", fen.clone());
        let mut replay = game.replay().unwrap();
        let movement = "dxe3".parse::<shakmaty::san::San>().unwrap().to_move(&position).unwrap();
        game.play(&mut replay, 0, movement).unwrap();
        let exported = game.export_as_game(&Default::default()).unwrap();
        let imported = pgn_reader::Reader::new(exported.as_bytes()).read_game(&mut crate::pgn::Parser).unwrap().unwrap().unwrap();
        assert_eq!(imported.tag("FEN"), fen);
        assert_eq!(imported.mainline_positions().unwrap().len(), 2);
    }

    #[test]
    fn stepping_back_against_the_computer_lands_on_the_players_turn() {
        let mut game = Game::default();
        let mut replay = game.replay().unwrap();
        let mut id = 0;
        for san in ["e4", "e5", "Nf3", "Nc6"] {
            let position = replay.nodes[id].position.clone().unwrap();
            id = game.play(&mut replay, id, san.parse::<shakmaty::san::San>().unwrap().to_move(&position).unwrap()).unwrap();
        }
        assert_eq!(step_back(&replay, 4, None), 3);
        assert_eq!(step_back(&replay, 4, Some(Color::White)), 2);
        assert_eq!(step_back(&replay, 3, Some(Color::White)), 2);
        // Black has no earlier turn than after 1.e4, so stepping back reaches the start.
        assert_eq!(step_back(&replay, 1, Some(Color::Black)), 0);
    }

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

const EVENT_CARD_HEIGHT: f32 = 64.0;
const CARD_SAGE: Color32 = Color32::from_rgb(106, 135, 108);

fn event_card(ui: &mut egui::Ui, event: &library::EventRow, selected: bool) -> egui::Response {
    let name = event_name(&event.name);
    let first = event.first.as_deref().and_then(pretty_date);
    let last = event.last.as_deref().and_then(pretty_date);
    let dates = match (first, last) {
        (Some(first), Some(last)) if first != last => format!("{first} – {last}"),
        (Some(date), _) | (_, Some(date)) => date,
        _ => "Date unknown".into(),
    };
    let unit = if event.games == 1 { "game" } else { "games" };
    let count = format!("{} {unit}", event.games);
    let details = || format!("{name}\n{dates} · {count}");
    let width = ui.available_width();
    let (_, rect) = ui.allocate_space(Vec2::new(width, EVENT_CARD_HEIGHT));
    let response = ui.interact(
        rect,
        ui.make_persistent_id(("event", &event.name)),
        egui::Sense::click(),
    );
    response.widget_info(|| {
        egui::WidgetInfo::selected(
            egui::WidgetType::SelectableLabel,
            ui.is_enabled(),
            selected,
            details(),
        )
    });
    let painter = ui.painter();
    paint_list_card(painter, &response, selected);
    let left = rect.left() + 13.0;
    let title = card_text(painter, name, 13.5, INK, width - 26.0, 2);
    let title_elided = title.elided;
    painter.galley(egui::pos2(left, rect.top() + 8.0), title, INK);

    let count = card_text(painter, &count, 11.0, GREEN, 100.0, 1);
    let badge = paint_card_badge(
        painter,
        egui::pos2(rect.right() - 13.0, rect.bottom() - 15.0),
        count,
        selected,
    );
    let date = card_text(painter, &dates, 11.0, GREEN, badge.left() - left - 8.0, 1);
    let date_elided = date.elided;
    painter.galley(egui::pos2(left, badge.center().y - date.size().y / 2.0), date, GREEN);
    if title_elided || date_elided {
        response.on_hover_text(details())
    } else {
        response
    }
}

fn card_text(
    painter: &egui::Painter,
    text: &str,
    size: f32,
    color: Color32,
    max_width: f32,
    max_rows: usize,
) -> Arc<egui::Galley> {
    card_text_in(painter, text, egui::FontId::proportional(size), color, max_width, max_rows)
}

fn card_text_in(
    painter: &egui::Painter,
    text: &str,
    font: egui::FontId,
    color: Color32,
    max_width: f32,
    max_rows: usize,
) -> Arc<egui::Galley> {
    let mut job = egui::text::LayoutJob::single_section(text.to_owned(), egui::TextFormat::simple(font, color));
    job.wrap.max_width = max_width;
    job.wrap.max_rows = max_rows;
    job.break_on_newline = false;
    painter.layout_job(job)
}

fn paint_list_card(painter: &egui::Painter, response: &egui::Response, selected: bool) {
    let rect = response.rect;
    let (fill, border) = if selected {
        (Color32::from_rgb(221, 231, 210), CARD_SAGE)
    } else if response.hovered() {
        (Color32::from_rgb(255, 249, 238), BEVEL)
    } else {
        (PAPER, FOLD)
    };
    let border = if response.has_focus() { GREEN } else { border };
    painter.rect(
        rect,
        7,
        fill,
        egui::Stroke::new(1.0_f32, border),
        egui::StrokeKind::Inside,
    );
    if selected {
        let accent = egui::Rect::from_center_size(
            egui::pos2(rect.left() + 2.0, rect.center().y),
            Vec2::new(3.0, rect.height() - 16.0),
        );
        painter.rect_filled(accent, 2, GREEN);
    }
}

fn paint_card_badge(
    painter: &egui::Painter,
    right_center: egui::Pos2,
    text: Arc<egui::Galley>,
    selected: bool,
) -> egui::Rect {
    let size = Vec2::new(text.size().x + 14.0, 20.0);
    let badge = egui::Rect::from_min_size(right_center - Vec2::new(size.x, size.y / 2.0), size);
    let fill = if selected {
        Color32::from_rgb(199, 215, 187)
    } else {
        Color32::from_rgb(234, 223, 203)
    };
    painter.rect_filled(badge, 5, fill);
    painter.galley(badge.center() - text.size() / 2.0, text, GREEN);
    badge
}

fn jump_card(ui: &mut egui::Ui, row: &library::Row, selected: bool) -> egui::Response {
    let name = if row.title.is_empty() {
        format!("{}  —  {}", row.white, row.black)
    } else {
        row.title.clone()
    };
    let date = pretty_date(&row.date);
    let path = row.folder_path.as_deref();
    let details = || {
        [Some(name.as_str()), date.as_deref(), path, Some(row.event.as_str())]
            .into_iter()
            .flatten()
            .filter(|text| !text.is_empty())
            .collect::<Vec<_>>()
            .join("\n")
    };
    let width = ui.available_width();
    let (_, rect) = ui.allocate_space(Vec2::new(width, if path.is_some() { 46.0 } else { 32.0 }));
    let response = ui.interact(rect, ui.make_persistent_id(("jump", row.id)), egui::Sense::click());
    response.widget_info(|| {
        egui::WidgetInfo::selected(egui::WidgetType::SelectableLabel, ui.is_enabled(), selected, details())
    });
    let painter = ui.painter();
    paint_list_card(painter, &response, selected);
    let left = rect.left() + 12.0;
    let right = rect.right() - 12.0;
    let title_y = rect.top() + if path.is_some() { 15.0 } else { rect.height() / 2.0 };
    let mut title_right = right;
    if let Some(date) = date.as_deref() {
        let date = card_text(painter, date, 11.0, MUTED, (width * 0.3).min(100.0), 1);
        let size = date.size();
        title_right -= size.x + 12.0;
        painter.galley(egui::pos2(right - size.x, title_y - size.y / 2.0), date, MUTED);
    }
    let title = card_text(painter, &name, 14.0, INK, title_right - left, 1);
    painter.galley(egui::pos2(left, title_y - title.size().y / 2.0), title, INK);
    if let Some(path) = path {
        let breadcrumb = card_text(painter, path, 11.5, GREEN, right - left, 1);
        painter.galley(egui::pos2(left, rect.top() + 25.0), breadcrumb, GREEN);
    }
    response.on_hover_text(details())
}

fn game_card(ui: &mut egui::Ui, row: &library::Row, selected: bool) -> egui::Response {
    let white = if row.white.is_empty() {
        "?"
    } else {
        &row.white
    };
    let black = if row.black.is_empty() {
        "?"
    } else {
        &row.black
    };
    // A titled line or position leads with its title; players and event follow only when known.
    let known = |s: &str| !s.trim_matches([' ', '?']).is_empty();
    let titled = !row.title.is_empty();
    let show_names = !titled || known(&row.white) || known(&row.black);
    let event = event_name(&row.event);
    let show_result = !row.result.is_empty() && row.result != "*";
    let date = pretty_date(&row.date);
    let metadata = [date.as_deref(), Some(row.eco.as_str())]
        .into_iter()
        .flatten()
        .filter(|text| !text.is_empty())
        .collect::<Vec<_>>()
        .join("  ·  ");
    let width = ui.available_width();
    let painter = ui.painter().clone();
    let layout = |text, size, color, max_width, max_rows| {
        card_text(&painter, text, size, color, max_width, max_rows)
    };
    let reserve = if row.starred { 20.0 } else { 0.0 } + if show_result { 63.0 } else { 0.0 };
    let title_line = titled.then(|| layout(&row.title, 15.0, INK, width - 26.0 - reserve, 2));
    let event_line = (!titled || known(&row.event)).then(|| layout(event, 11.5, MUTED, width - 26.0, 2));
    let title_height = title_line.as_ref().map_or(0.0, |g| g.size().y + 4.0);
    let names_height = if show_names { 36.0 } else { 0.0 };
    let event_height = event_line.as_ref().map_or(0.0, |g| g.size().y);
    let metadata_height = if metadata.is_empty() { 0.0 } else { 16.0 };
    let tags_height = if row.labels.is_empty() { 0.0 } else { 26.0 };
    let (_, rect) = ui.allocate_space(Vec2::new(
        width,
        14.0 + title_height + names_height + event_height + metadata_height + tags_height,
    ));
    let response = ui.interact(rect, ui.make_persistent_id(row.id), egui::Sense::click());
    let details = || {
        format!(
            "{}{}White: {white}\nBlack: {black}\n{event}\n{metadata}\n{}{}",
            if titled { format!("{}\n", row.title) } else { String::new() },
            if row.starred { "Favourite\n" } else { "" },
            row.result,
            if row.labels.is_empty() { String::new() } else { format!("\nTags: {}", row.labels.join(", ")) },
        )
    };
    response.widget_info(|| {
        egui::WidgetInfo::selected(
            egui::WidgetType::SelectableLabel,
            ui.is_enabled(),
            selected,
            details(),
        )
    });
    if !ui.is_rect_visible(rect) {
        return response;
    }

    paint_list_card(&painter, &response, selected);

    let left = rect.left() + 13.0;
    let mut right = rect.right() - 13.0;
    // Star and result sit level with the title, or between the two names.
    let matchup_y = rect.top() + if titled { 16.0 } else { 24.0 };
    if row.starred {
        painter.text(
            egui::pos2(right, matchup_y),
            egui::Align2::RIGHT_CENTER,
            "★",
            egui::FontId::proportional(13.0),
            RIBBON,
        );
        right -= 20.0;
    }
    let mut truncated = event_line.as_ref().is_some_and(|g| g.elided) || title_line.as_ref().is_some_and(|g| g.elided);
    if show_result {
        let score = layout(&row.result, 12.0, GREEN, 54.0, 1);
        truncated |= score.elided;
        let badge = paint_card_badge(&painter, egui::pos2(right, matchup_y), score, selected);
        right = badge.left() - 9.0;
    }
    let mut y = rect.top() + 6.0;
    if let Some(title) = title_line {
        let height = title.size().y;
        painter.galley(egui::pos2(left, y), title, INK);
        y += height + 4.0;
    }
    let names_top = y;
    if show_names {
        y += 36.0;
    }
    for (name, y, marker) in [
        (white, names_top + 9.0, PAPER),
        (black, names_top + 27.0, GREEN),
    ]
    .into_iter()
    .filter(|_| show_names)
    {
        let center = egui::pos2(left + 4.0, y);
        painter.circle(center, 3.4, marker, egui::Stroke::new(1.0_f32, CARD_SAGE));
        let name = layout(name, 13.5, INK, right - left - 16.0, 1);
        truncated |= name.elided;
        painter.galley(egui::pos2(left + 16.0, y - name.size().y / 2.0), name, INK);
    }
    let metadata_y = y + event_line.as_ref().map_or(0.0, |g| g.size().y + 3.0);
    if let Some(event_line) = event_line {
        painter.galley(egui::pos2(left, y), event_line, MUTED);
    }
    if !metadata.is_empty() {
        let line = layout(&metadata, 11.0, GREEN, width - 26.0, 1);
        truncated |= line.elided;
        painter.galley(egui::pos2(left, metadata_y), line, GREEN);
    }
    // Tags in one row along the bottom; those that do not fit become "+N".
    let mut x = left;
    for (i, name) in row.labels.iter().enumerate() {
        let pill = Pill::new(&painter, name, None, 120.0);
        let room = rect.right() - 13.0 - x - if i + 1 < row.labels.len() { 30.0 } else { 0.0 };
        if pill.size().x > room {
            let more = layout(&format!("+{}", row.labels.len() - i), 11.0, MUTED, 40.0, 1);
            painter.galley(egui::pos2(x, rect.bottom() - 17.0 - more.size().y / 2.0), more, MUTED);
            truncated = true;
            break;
        }
        let size = pill.size();
        x += size.x + 5.0;
        pill.paint(&painter, egui::Rect::from_min_size(egui::pos2(x - size.x - 5.0, rect.bottom() - 17.0 - size.y / 2.0), size), 0);
    }
    if truncated {
        response.on_hover_text(details())
    } else {
        response
    }
}

/// Event, date and result under the game's heading, leaving out what PGN marks unknown; a titled game adds its players.
fn workspace_subtitle(game: &Game) -> String {
    let known = |s: &str| !s.trim_matches([' ', '?']).is_empty();
    let players = (!game.tag("Title").is_empty() && (known(game.tag("White")) || known(game.tag("Black"))))
        .then(|| format!("{} — {}", game.tag("White"), game.tag("Black")));
    let event = known(game.tag("Event")).then(|| game.tag("Event").to_owned());
    let result = (!matches!(game.tag("Result"), "" | "*")).then(|| game.tag("Result").to_owned());
    [players, event, pretty_date(game.tag("Date")), result].into_iter().flatten().collect::<Vec<_>>().join("  ·  ")
}

#[cfg(test)]
#[test]
fn subtitle_leaves_out_unknowns() {
    let mut game = Game::default();
    game.tags = vec![("Title".into(), "Bird trap".into()), ("White".into(), "?".into()), ("Black".into(), "?".into()), ("Event".into(), "?".into()), ("Date".into(), "????.??.??".into()), ("Result".into(), "*".into())];
    assert_eq!(workspace_subtitle(&game), "");
    game.set_tag("Event", "Blitz".into());
    game.set_tag("Date", "2026.11.14".into());
    game.set_tag("Result", "1-0".into());
    assert_eq!(workspace_subtitle(&game), "Blitz  ·  14 Nov 2026  ·  1-0");
}

/// An event's display name; PGN leaves unknown events blank or "?".
fn event_name(name: &str) -> &str {
    if name.trim_matches([' ', '?']).is_empty() { "(No event)" } else { name }
}

/// A PGN date ("2026.11.07", "2026.11.??", "2026.??.??") as "7 Nov 2026", "Nov 2026" or "2026"; None when the year is unknown.
fn pretty_date(date: &str) -> Option<String> {
    const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
    let mut parts = date.split('.').map(|p| p.parse::<usize>().ok());
    let year = parts.next()??;
    let month = parts.next().flatten().filter(|m| (1..=12).contains(m));
    let day = parts.next().flatten().filter(|_| month.is_some());
    Some(match (day, month) {
        (Some(day), Some(month)) => format!("{day} {} {year}", MONTHS[month - 1]),
        (_, Some(month)) => format!("{} {year}", MONTHS[month - 1]),
        _ => year.to_string(),
    })
}

/// Pasted text as PGN: a bare FEN (a puzzle, say) becomes a game starting from that position.
pub fn pasted_pgn(text: String) -> String {
    match text.trim().parse::<Fen>() {
        Ok(_) => format!("[SetUp \"1\"]\n[FEN \"{}\"]\n\n*\n", text.trim()),
        Err(_) => text,
    }
}

#[cfg(test)]
#[test]
fn pasted_fen_becomes_a_game() {
    let pgn = pasted_pgn(" 8/8/8/8/8/6k1/8/4K3 b - - 0 42\n".into());
    let game = pgn_reader::Reader::new(pgn.as_bytes()).read_game(&mut crate::pgn::Parser).unwrap().unwrap().unwrap();
    assert_eq!(game.tag("FEN"), "8/8/8/8/8/6k1/8/4K3 b - - 0 42");
    assert_eq!(pasted_pgn("1. e4 e5 *".into()), "1. e4 e5 *");
}

#[cfg(test)]
#[test]
fn pretty_dates() {
    assert_eq!(pretty_date("2026.11.07").as_deref(), Some("7 Nov 2026"));
    assert_eq!(pretty_date("2026.11.??").as_deref(), Some("Nov 2026"));
    assert_eq!(pretty_date("1858.??.??").as_deref(), Some("1858"));
    assert_eq!(pretty_date("????.??.??"), None);
    assert_eq!(pretty_date(""), None);
}

/// Folders depth-first with their depth, leaving out the insides of collapsed ones.
fn folder_rows<'a>(folders: &'a [library::Folder], collapsed: &HashSet<i64>) -> Vec<(usize, &'a library::Folder)> {
    fn walk<'a>(folders: &'a [library::Folder], parent: Option<i64>, depth: usize, collapsed: &HashSet<i64>, out: &mut Vec<(usize, &'a library::Folder)>) {
        for folder in folders.iter().filter(|f| f.parent == parent) {
            out.push((depth, folder));
            if !collapsed.contains(&folder.id) {
                walk(folders, Some(folder.id), depth + 1, collapsed, out);
            }
        }
    }
    let mut out = Vec::new();
    walk(folders, None, 0, collapsed, &mut out);
    out
}

/// "No folder" and every folder, indented; returns the choice, Some(None) for no folder.
fn folder_picker(ui: &mut egui::Ui, folders: &[library::Folder]) -> Option<Option<i64>> {
    let mut chosen = None;
    if ui.button("No folder").clicked() {
        chosen = Some(None);
    }
    for (depth, folder) in folder_rows(folders, &HashSet::new()) {
        ui.horizontal(|ui| {
            ui.add_space(depth as f32 * 14.0);
            if ui.button(&folder.name).clicked() {
                chosen = Some(Some(folder.id));
            }
        });
    }
    chosen
}

/// A horizontal rule that resizes the area above it when dragged; the height is kept in user defaults under `key`.
fn resize_rule(ui: &mut egui::Ui, height: &mut f32, range: std::ops::RangeInclusive<f32>, key: &NSString) {
    let (rule, drag) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 12.0), egui::Sense::drag());
    let drag = drag.on_hover_cursor(egui::CursorIcon::ResizeVertical);
    if drag.dragged() {
        // Start from what is shown, so a height above the range's end has no dead zone.
        *height = (height.min(*range.end()) + drag.drag_delta().y).clamp(*range.start(), *range.end());
    }
    if drag.drag_stopped() {
        NSUserDefaults::standardUserDefaults().setDouble_forKey(*height as _, key);
    }
    let color = if drag.hovered() || drag.dragged() { BEVEL } else { FOLD };
    ui.painter().hline(rule.x_range(), rule.center().y, egui::Stroke::new(1.0_f32, color));
    ui.painter().rect_filled(egui::Rect::from_center_size(rule.center(), Vec2::new(32.0, 5.0)), 3, color);
}

/// A tag's colour, the same on every run: muted inks that sit on the paper.
fn tag_color(name: &str) -> Color32 {
    const PALETTE: [Color32; 8] = [
        Color32::from_rgb(176, 84, 52),
        Color32::from_rgb(58, 110, 165),
        Color32::from_rgb(61, 125, 82),
        Color32::from_rgb(150, 92, 150),
        Color32::from_rgb(184, 134, 30),
        Color32::from_rgb(40, 128, 128),
        Color32::from_rgb(160, 70, 90),
        Color32::from_rgb(110, 112, 56),
    ];
    // FNV-1a: std's hasher is randomly seeded per run.
    let hash = name.to_lowercase().bytes().fold(0x811c9dc5u32, |h, b| (h ^ b as u32).wrapping_mul(0x01000193));
    PALETTE[hash as usize % PALETTE.len()]
}

/// A tag pill laid out for painting: colour dot, name, and an optional count or ×.
struct Pill {
    name: Arc<egui::Galley>,
    extra: Option<Arc<egui::Galley>>,
    color: Color32,
}
impl Pill {
    fn new(painter: &egui::Painter, name: &str, extra: Option<&str>, max_width: f32) -> Self {
        Self {
            name: card_text(painter, name, 12.0, INK, max_width, 1),
            extra: extra.map(|text| card_text(painter, text, 11.0, MUTED, 60.0, 1)),
            color: tag_color(name),
        }
    }
    fn size(&self) -> Vec2 {
        Vec2::new(22.0 + self.name.size().x + self.extra.as_ref().map_or(0.0, |e| e.size().x + 6.0) + 9.0, 22.0)
    }
    /// `emphasis`: 0 resting, 1 hovered, 2 selected.
    fn paint(self, painter: &egui::Painter, rect: egui::Rect, emphasis: u8) {
        let fill = self.color.gamma_multiply([0.13, 0.22, 0.34][emphasis as usize]);
        let stroke = if emphasis == 2 { egui::Stroke::new(1.0_f32, self.color) } else { egui::Stroke::new(1.0_f32, self.color.gamma_multiply(0.35)) };
        painter.rect(rect, 11, fill, stroke, egui::StrokeKind::Inside);
        painter.circle_filled(rect.left_center() + Vec2::new(11.0, 0.0), 3.5, self.color);
        let text = rect.left_center() + Vec2::new(20.0, -self.name.size().y / 2.0);
        let width = self.name.size().x;
        painter.galley(text, self.name, INK);
        if let Some(extra) = self.extra {
            painter.galley(egui::pos2(text.x + width + 6.0, rect.center().y - extra.size().y / 2.0), extra, MUTED);
        }
    }
}

/// A clickable tag pill.
fn tag_chip(ui: &mut egui::Ui, name: &str, extra: Option<&str>, selected: bool) -> egui::Response {
    let pill = Pill::new(ui.painter(), name, extra, 180.0);
    let (rect, response) = ui.allocate_exact_size(pill.size(), egui::Sense::click());
    response.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::SelectableLabel, ui.is_enabled(), selected, name));
    let emphasis = if selected { 2 } else { u8::from(response.hovered()) };
    pill.paint(ui.painter(), rect, emphasis);
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// The tags in `tags` as pills (click one to remove it) and a "+ Tag" menu to type a new tag or pick a known one; true when changed.
fn tag_editor(ui: &mut egui::Ui, tags: &mut std::collections::BTreeSet<String>, known: &[(String, i64)], draft: &mut String, salt: &str) -> bool {
    let mut changed = false;
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = Vec2::new(5.0, 5.0);
        for name in tags.clone() {
            if tag_chip(ui, &name, Some("×"), false).on_hover_text("Remove this tag").clicked() {
                tags.remove(&name);
                changed = true;
            }
        }
        // Pill height: the app-wide button padding would make it taller than the tags beside it.
        ui.spacing_mut().button_padding = Vec2::new(9.0, 2.0);
        let button = egui::Button::new(RichText::new("+ Tag").size(12.0).color(MUTED)).corner_radius(11).min_size(Vec2::new(0.0, 22.0));
        let menu = egui::containers::menu::MenuButton::from_button(button)
            // Clicking into the name field must not close the menu.
            .config(egui::containers::menu::MenuConfig::new().close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside));
        let (button, _) = menu.ui(ui, |ui| {
            ui.set_max_width(240.0);
            let field = ui.add(egui::TextEdit::singleline(draft).id_salt(("tag-draft", salt)).hint_text("New tag, then Return").desired_width(220.0));
            // Read Return before refocusing: lost_focus() reads focus as it is now, and refocusing would hide the loss.
            let submitted = field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            if ui.memory(|m| m.focused().is_none()) {
                field.request_focus();
            }
            let typed = draft.trim().to_owned();
            if submitted && !typed.is_empty() {
                // Reuse an existing tag's spelling so "endgame" joins "Endgame".
                let name = known.iter().find(|(k, _)| k.to_lowercase() == typed.to_lowercase()).map_or(typed.clone(), |(k, _)| k.clone());
                changed |= tags.insert(name);
                draft.clear();
                ui.close();
            }
            let needle = typed.to_lowercase();
            let offers: Vec<_> = known.iter().filter(|(k, _)| !tags.contains(k) && k.to_lowercase().contains(&needle)).collect();
            if !offers.is_empty() {
                ui.small(if needle.is_empty() { "Your tags" } else { "Matching tags" });
                egui::ScrollArea::vertical().max_height(200.0).show(ui, |ui| {
                    ui.horizontal_wrapped(|ui| {
                        ui.spacing_mut().item_spacing = Vec2::new(5.0, 5.0);
                        for (name, games) in offers {
                            if tag_chip(ui, name, Some(&games.to_string()), false).clicked() {
                                tags.insert(name.clone());
                                changed = true;
                                draft.clear();
                                ui.close();
                            }
                        }
                    });
                });
            }
        });
        if button.clicked() {
            // A fresh menu starts empty, not with a name half-typed into another tag menu.
            draft.clear();
        }
    });
    changed
}

fn saved_width(key: &NSString, default: f32) -> f32 {
    match NSUserDefaults::standardUserDefaults().doubleForKey(key) {
        w if w < 200.0 => default,
        w => w as f32,
    }
}

/// Sets a side panel's width from a drag on egui's edge handle, saving it when the drag ends.
/// The panel itself stays at an exact width: with a width range, egui stores the content's width,
/// so content slightly wider than the panel pushed the edge outward a little every frame.
fn drag_panel_width(ctx: &egui::Context, id: &str, left: bool, width: &mut f32, range: std::ops::RangeInclusive<f32>, key: &NSString) {
    let Some(handle) = ctx.read_response(egui::Id::new(id).with("__resize")) else { return };
    if handle.dragged() && let Some(pointer) = handle.interact_pointer_pos() {
        let screen = ctx.content_rect();
        let wanted = if left { pointer.x - screen.left() } else { screen.right() - pointer.x };
        *width = wanted.clamp(*range.start(), *range.end());
    }
    if handle.drag_stopped() {
        NSUserDefaults::standardUserDefaults().setDouble_forKey(*width as _, key);
    }
}
