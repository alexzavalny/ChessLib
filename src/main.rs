mod app;
mod engine;
mod library;
mod pgn;

fn main() -> anyhow::Result<()> {
    // CHESSLIB_DB wins, then the folder chosen with "Library folder…", then Application Support.
    let path = std::env::var_os("CHESSLIB_DB")
        .map(std::path::PathBuf::from)
        .or_else(|| app::saved_library_folder().map(|folder| folder.join(app::LIBRARY_FILE)))
        .unwrap_or_else(|| {
            std::path::PathBuf::from(std::env::var_os("HOME").expect("HOME must be set"))
                .join("Library/Application Support/ChessLib")
                .join(app::LIBRARY_FILE)
        });
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let db = library::open(&path)?;
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title("ChessLib")
            .with_inner_size([1380.0, 880.0])
            .with_min_inner_size([1080.0, 720.0]),
        // Exit with the window instead of returning: keeping AppKit's loop alive past the closed window
        // crashed in its Touch Bar observer ("Cannot remove an observer … nextResponder").
        run_and_return: false,
        ..Default::default()
    };
    eframe::run_native(
        "ChessLib",
        options,
        Box::new(move |cc| Ok(Box::new(app::App::new(cc, path, db)))),
    )
    .map_err(|e| anyhow::anyhow!("{e}"))
}
