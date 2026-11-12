# ChessLib — macOS chess library

A native Rust desktop app for keeping, browsing, and annotating a personal chess library. No account, server, or network connection is required.

## Open the app

Open `dist/ChessLib.app` in Finder. The packaged build targets Apple Silicon.

To build from source:

```sh
cargo run --release --locked
sh scripts/bundle-macos.sh
```

The toolchain file selects native Apple Silicon Rust. On Intel, use an installed Intel toolchain with `cargo +stable-x86_64-apple-darwin run --release --locked` (the Intel build is not tested).

## Use it

- **Import PGN** or **⌘O** opens one or more files; dropping PGNs into the window also imports them. The sample button adds Morphy's Opera Game.
- Search players, event names, dates, ECO codes, imported comments, and your notes. Search is case-insensitive, matches word prefixes, and requires all entered words. It is not an arbitrary substring search.
- Narrow the library by source PGN filename or favourites. Results are paged, newest imports first.
- Select a game, click a move, or use **← / →**, **Home / End**. Navigation follows the current line. Flip the board or copy the current FEN.
- **Play a move:** click a piece, then a highlighted destination. Legal moves include castling (click the king's destination), en passant and a choice of all four promotion pieces. Escape clears the selection. Existing moves are reused; alternatives become saved variations.
- Playing or stepping forward one move uses recordings of pieces on a wooden board. Captures have a firmer clack. Turn off **Sounds** below the board to mute them across launches. The CC0 recordings are by el_boss. [Piece Placement](https://freesound.org/people/el_boss/sounds/546119/) · [Piece Capture](https://freesound.org/people/el_boss/sounds/546120/)
- **Explore variations:** choose **Continue with** before a branch, or **Alternatives at this move** after it. **Return to main line** returns to the corresponding move in the main line. Imported variations are playable too.
- **Analyze once · 3 seconds** runs local Stockfish for the selected position, showing a White-relative evaluation, depth, best move and continuation. **Play best move** explores its suggestion.
- The vertical evaluation bar beside the board shows White/Black advantage and the score. It follows board orientation, uses live analysis first and saved review scores otherwise, and shows a neutral dash when the position has no evaluation. Checkmate and drawn positions display their outcome.
- **Continuous analysis** keeps Stockfish calculating until disabled. Changing positions cancels the previous calculation and starts a new one. The once button switches to a bounded calculation; Stop disables continuous mode. Closing the app stops and reaps the engine.
- **Analyze whole game** reviews the original main line at 0.5 seconds per position, with progress and cancellation. The report shows approximate accuracy for each side, best/good moves, inaccuracies, mistakes and blunders. Expand **Moves & mistakes** to jump to a move; hover for its suggested continuation. Completed reports autosave with the game and survive reopening. Cancelled runs retain any previous completed report. Continuous analysis pauses during a review and resumes afterward.
- Write notes at the starting position or after any move, including variations. Notes autosave after a short pause, before changing games, and before closing. **⌘S** saves immediately. If saving fails, edits remain open and closing is blocked.
- **Export game** writes the selected game. **Export results** writes every game matching the current filters, across all pages. Both include your notes, original annotations, NAGs, imported variations and newly added lines.
- Imports run in the background. **Stop** retains completed work. Reimporting the same parsed game skips it without overwriting notes.

## Local analysis setup

Stockfish is discovered in the standard Homebrew locations, `/usr/bin`, and `PATH`. This Mac already has `/opt/homebrew/bin/stockfish`. On another Mac, install Stockfish separately or use **Engine → Choose Stockfish…** to select its executable for the current session. `CHESSLIB_STOCKFISH=/absolute/path/stockfish` overrides discovery. No engine download or network access happens automatically.

Analysis uses one CPU thread and 64 MiB of engine hash memory. The app communicates through UCI, checks suggested moves for legality, and reports startup, timeout and process errors. Scores are engine estimates; longer analysis can change them. The packaged app does not bundle Stockfish.

### Review scoring

Reviews use the [published Lichess move-accuracy equations](https://lichess.org/page/accuracy): convert White's centipawn score to `100 / (1 + exp(-0.00368208 * cp))`, then measure the mover's loss in that value. Mate and terminal outcomes map to 0/100, or 50 for a draw. Move accuracy is `103.1668 * exp(-0.04354 * loss) - 3.1669`, clamped to 0–100 (no loss = 100).

ChessLib's **approximate accuracy** is the arithmetic mean of that side's move accuracies; it is not Lichess's weighted game accuracy or Chess.com's score. Losses of at least 5, 10 and 20 percentage points are classified as inaccuracy, mistake and blunder. Other moves are Best if they match the engine's suggestion, otherwise Good. Scores are sensitive to search time; this is a quick local review, not a measure of player strength or evidence of cheating. The review sends the entire move history to Stockfish so repetition context is available. Resignation results are not treated as a forced mate in the final position.

## Storage and large collections

The default database is `~/Library/Application Support/ChessLib/library.sqlite3`. `CHESSLIB_DB=/absolute/path/library.sqlite3` overrides it for development or a separate library.

PGN input is streamed one game at a time. SQLite commits batches of 200 games, uses WAL, and indexes full-text search with FTS5. The GUI holds 80 result rows and the selected game; it does not load the entire library. Pagination uses an ID cursor instead of increasingly expensive offsets. Search, imports, loading, and export run off the UI thread. Note writes are synchronous and may briefly wait for an import transaction.

Original PGN files are never changed during import. Imported tags and move tokens also remain unchanged in storage when studying: added moves are saved separately inside the game's record. Existing main-line note indices remain valid, and added/imported branch notes use stable node IDs. Reimporting the original PGN still skips it without removing study work. Use this version or newer for libraries with added variations; older builds do not understand that study data. Games are stored as parsed tags and tokens; export normalizes formatting rather than reproducing original bytes. A duplicate means identical parsed content, not merely the same moves. Files with the same basename share a source filter; duplicate games remain under their first source.

For a complete backup, quit the app and copy the database. While the app is running, SQLite may also use `-wal` and `-shm` sidecars; copying only the main file at that time is not a reliable backup. PGN export is portable but does not preserve favourites or source grouping.

## MVP boundaries

- Standard chess and legal FEN starting positions. Unsupported variants remain stored and exportable, with a display error. An illegal imported move disables that continuation with an explanation; other legal branches remain playable and the original tokens survive export.
- Variations can be added and explored, with notes on each position. Deleting/reordering lines, replacing the original main line, and editing imported annotations are not included.
- No opening tree, online sync, position search, collection deletion, or game metadata editor. Position analysis supports one-shot and continuous modes. Full-game review covers the original main line; it does not classify study variations.
- Notes are separate from original annotations. On export, braces in comments are replaced with parentheses because PGN brace comments cannot nest.
- UTF-8 PGNs are supported; invalid byte sequences are replaced by the Unicode replacement character. Very long comments may be normalized into multiple comments. A single game is capped at 16 MiB of parsed content to bound accidental memory growth. Malformed PGNs should be checked against their originals; the PGN reader is permissive.
- The app is locally ad-hoc signed, not Developer ID signed or notarized for distribution. Built with eframe/egui, pgn-reader, shakmaty, and bundled SQLite. The project is GPL-3.0-or-later, consistent with the chess library dependency.

## Checks

```sh
cargo test --locked
cargo clippy --all-targets --locked -- -D warnings
cargo test --release --locked -- --ignored --nocapture
```

The ignored library test generates 100,000 distinct 33-ply games, imports them into a temporary database, checks first/deep pages and indexed search, saves a note, exports a game, verifies SQLite integrity, and reports timings. Its temporary files are removed afterward.

Measured on the development Mac (Apple Silicon, release build, 12 November 2026): 100,000 games imported in **7.61 s**, first page **254 ms**, indexed player search **11 ms**, deep page **42 ms**. This is one synthetic local run, not a guarantee for arbitrary PGNs or multi-million-game databases.

Live macOS checks covered sample import, main-line navigation to checkmate, board flipping, selected-move scrolling, note entry/autosave, persistence after restarting, and native PGN export. The exported file contained the saved note and the original variation.

The focused tests also cover nested imported and new variations, branch-note persistence and PGN export, unchanged duplicate detection after study edits, corrupted-study export protection, castling, en passant, underpromotion, Black-to-move FENs, engine score interpretation, protocol failure handling and cancellation. The ignored engine tests require a local Stockfish executable and check one-shot analysis, continuous search beyond three seconds, cancellation during a search, and a full review ending in checkmate.

Live checks of this update used an isolated test library: imported and newly played variations, legal-square selection, choosing knight promotion, branch-note saving, Stockfish evaluation and playing its best move, continuous analysis across position changes, whole-game progress and summaries, jumping to a flagged blunder, automatic resumption of continuous analysis, and report persistence after restarting. The release app was rebuilt; Intel remains untested.
