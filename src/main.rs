mod app;
mod engine;
mod library;
mod pgn;

fn main() -> anyhow::Result<()> {
    let path = std::env::var_os("CHESSLIB_DB")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| {
            std::path::PathBuf::from(std::env::var_os("HOME").expect("HOME must be set"))
                .join("Library/Application Support/ChessLib/library.sqlite3")
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
        ..Default::default()
    };
    eframe::run_native(
        "ChessLib",
        options,
        Box::new(move |cc| Ok(Box::new(app::App::new(cc, path, db)))),
    )
    .map_err(|e| anyhow::anyhow!("{e}"))
}
