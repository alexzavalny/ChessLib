mod app;
mod cli;
mod engine;
mod library;
mod pgn;

/// ~/Library/Application Support/ChessLib: the default library, and the library `chesslib library use` remembers.
fn support_dir() -> std::path::PathBuf {
    std::path::PathBuf::from(std::env::var_os("HOME").expect("HOME must be set")).join("Library/Application Support/ChessLib")
}

/// CHESSLIB_DB wins, then the folder chosen with "Library folder…", then Application Support.
fn library_path() -> std::path::PathBuf {
    std::env::var_os("CHESSLIB_DB")
        .map(std::path::PathBuf::from)
        .or_else(|| app::saved_library_folder().map(|folder| folder.join(app::LIBRARY_FILE)))
        .unwrap_or_else(|| support_dir().join(app::LIBRARY_FILE))
}

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    // Any argument means the command line, except the -psn_… that older Finders pass to apps they launch.
    if args.first().is_some_and(|a| !a.starts_with("-psn_")) {
        return cli::run(args);
    }
    let path = library_path();
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
