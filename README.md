<p align="center">
  <img src="assets/header-icon.png" width="96" height="96" alt="">
</p>

<h1 align="center">ChessLib</h1>

<p align="center"><strong>A personal chess library for macOS</strong></p>

<p align="center">
  <a href="#quick-start">Quick start</a> ·
  <a href="#review-and-train">Review and train</a> ·
  <a href="#stockfish-setup">Stockfish setup</a> ·
  <a href="#library-and-study-guide">Study guide</a>
</p>

Import your PGNs, play through the positions that matter, and keep your notes alongside the moves. ChessLib stores your library on your Mac and runs Stockfish locally.

[![ChessLib showing Morphy’s Opera Game, with variation arrows, a saved note, and an evaluation graph](docs/screenshots/study.webp)](docs/screenshots/study.webp)

*The Opera Game at 7…Qe7. The arrows show two continuations you can play from this position.*

## Quick start

1. **Open ChessLib.** Open `dist/ChessLib.app` if you already have a local build. To create it from this checkout, run the command below and open the resulting app in Finder.

   ```sh
   sh scripts/bundle-macos.sh
   ```

   To build it and install it as `/Applications/ChessLib.app` (replacing an older copy; your library is kept), run `sh scripts/install-macos.sh` instead.

2. **Add a game.** Choose **Try a sample game** to load the Opera Game, or use **Import PGN** (⌘O) to bring in your own files.
3. **Study a position.** Select a game, step through it with the arrow keys, then play an alternative on the board or jot down a note. Variations and notes wait for **Save**: changed games show **Save** and **Discard** beside the title.

The build script targets Apple Silicon. For development, run `cargo run --release --locked`. An Intel Mac needs an Intel Rust toolchain. The Intel build remains untested.

## What you can do

| Feature | What you can do |
| --- | --- |
| **Organize a collection** | Search players, events, ECO codes and notes. Browse by event, filter by source file or favourite, and export a game or the current results as PGN. |
| **Build your own study** | Play and revisit variations, mark moves with symbols such as `!` and `??`, draw arrows and circles, and save notes at any position. |
| **Analyze or play against Stockfish** | Run a three-second analysis or keep it running as you step through the game. Play the engine from the opening or from the position on the board. |
| **Practice from your games** | Review a game's main line, jump to flagged moves on the evaluation graph, and replay the positions where a player blundered. |

## Review and train

**Review a game.** Select **Analyze whole game** to estimate accuracy for both sides and flag inaccuracies, mistakes and blunders. Completed reviews are saved with the game.

[![The Opera Game at 16.Qb8+, showing the evaluation graph and both players’ accuracy and move breakdowns](docs/screenshots/review.webp)](docs/screenshots/review.webp)

**Try the position again.** Select **Train on mistakes** to play puzzles from reviewed blunders. Filter by player, or right-click a reviewed game and choose **Train on this game**.

[![A ChessLib puzzle from Fool’s Mate asking White to find a better move than g4](docs/screenshots/training.webp)](docs/screenshots/training.webp)

## Stockfish setup

Install Stockfish separately to analyze, review, or play against the computer. ChessLib searches standard Homebrew locations, `/usr/bin`, and `PATH`. You can also open **Engine settings** and click **Choose Stockfish…** to select the executable for the current session or set `CHESSLIB_STOCKFISH=/absolute/path/stockfish` before launching.

ChessLib communicates with Stockfish over UCI and uses one engine thread and 64 MiB of hash memory. Library browsing and annotation work without an engine.

## Library and study guide

<details>
<summary>Importing, navigating and annotating games</summary>

- **Import PGN** or **⌘O** opens one or more files; dropping PGNs into the window also imports them, and **⌘V** outside a text field imports PGN text from the clipboard (listed under the source `chesslib-paste.pgn`). The sample button adds Morphy's Opera Game. After an import, each new game missing players, event or date asks for them; right-click any game and choose **Edit…** to change those fields later.
- Search players, event names, dates, ECO codes, imported comments, and your notes. Search is case-insensitive, matches word prefixes, and requires all entered words. It is not an arbitrary substring search.
- Browse by event or narrow the library by source PGN filename or favourites. Results are paged, newest imports first.
- **Folders** organise openings, courses and your own games. **+ New** under *Folders* makes a top-level folder; right-click a folder to add a subfolder, rename it or delete it. Deleting a folder never deletes games: its games and subfolders move up one level. Click a folder to list its games, including those in its subfolders; click it again, or **All games**, to show everything. Right-click a game and choose **Move to folder**, or use **Move all to folder** above the list to move every game the current search and filters list. Each game is in at most one folder. Imports go into the folder that is open when they start, which suits importing a course PGN straight into its folder; skipped duplicates stay where they were.
- **Tags** cut across folders: a game can have many, and so can any position in it. Use **+ Tag** under the game title to tag the game, or under a move's notes to tag that position; type a new tag and press Return, or pick one you already use. Click a tag's × to remove it. A tagged move shows a coloured ◆ in the move list. The *Tags* section of the sidebar lists every tag with its number of games: click one to list those games, and click it again, or **All games**, to clear it. A game opened from a tag starts at the first position carrying that tag. Tags are not written to exported PGN.
- Select a game, click a move, or use **← / →**, **↑ / ↓** or **Home / End**. Navigation follows the current line. Flip the board (**F**) or copy the current FEN. **L** toggles continuous analysis. Drag the rule under the move list to resize it.
- **New board** (⌘N) opens an unsaved analysis board. Choose **Board editor** to set up the current position: select a piece and click a square to place it, or use **Eraser** / right-click to remove pieces. Set the side to move, castling rights, en passant square and move counters; **Clear board** and **Starting position** reset the setup. **Apply position** requires a valid position and replaces the unsaved board's moves and notes; **Cancel** keeps them. Play or analyze from there, then use **Save to library** to keep it.
- **Play a move:** click a piece, then a highlighted destination. Legal moves include castling (click the king's destination), en passant and a choice of all four promotion pieces. Escape clears the selection. Existing moves are reused; alternatives become saved variations.
- Playing or stepping forward one move uses recordings of pieces on a wooden board. Captures have a firmer clack. Turn off **Sounds** in the toolbar to mute them across launches. The CC0 recordings are by el_boss. [Piece Placement](https://freesound.org/people/el_boss/sounds/546119/) · [Piece Capture](https://freesound.org/people/el_boss/sounds/546120/)
- **Explore variations:** the move list shows the whole game tree. The game's moves, and the moves you play on from its end, stay in the White and Black columns; each side line follows right after the move it replaces, as an indented block, with nested lines indented again and the PGN's comments in grey where they belong. Your own analysis is green. Click any move to go there, or hover it to see the position (and its note) without leaving the current move. Click a block's left edge to fold it into a one-line summary, or use **Fold side lines** for the whole game. **Return to main line** returns to the corresponding move in the main line. Where the line branches, brown arrows on the board show every next move, the continuation bolder. Right-click a side-line move and choose **Promote variation** to make it the line's continuation, or **Set as main line** to make its whole line the main line; if that replaces moves played in the game, the game is rewritten with your line as its moves and the old ones as a side line, notes and marks staying on their moves (a whole-game review is dropped), or right-click any move and choose **Delete from here** to remove it and everything after it, with their notes, marks and drawings, once you confirm. This works on imported lines and the game's own moves too: deleting a game move lets the next side line there continue the game, and drops a whole-game review.
- **Analyze 3 s** runs local Stockfish for the selected position, showing a White-relative evaluation, depth, best move and continuation. **Play best** explores its suggestion.
- **Play vs computer** (**P**) starts a practice game from the current position, or from the starting position when no game is open. Choose a side and a Stockfish level from 0 to 20. **←** steps back to your previous move; play a different one there to take a move back, or the same one to carry on.
- The vertical evaluation bar beside the board shows White/Black advantage and the score. It follows board orientation, uses live analysis first and saved review scores otherwise, and shows a neutral dash when the position has no evaluation. Checkmate and drawn positions display their outcome.
- The **toggle beside Stockfish** keeps continuous analysis running until disabled. The **gear** opens Stockfish settings, where you choose the number of candidate lines (1–5) and the local engine executable. Changing positions cancels the previous calculation and starts a new one. The once button switches to a bounded calculation; Stop disables continuous mode. Closing the app stops and reaps the engine.
- **Analyze whole game** reviews the original main line at 0.5 seconds per position, with progress and cancellation. The report shows approximate accuracy for each side, best/good moves, inaccuracies, mistakes and blunders. Expand **Moves & mistakes** to jump to a move; hover for its suggested continuation. Completed reports save with the game at once, or with its unsaved edits when there are some, and survive reopening. Cancelled runs retain any previous completed report. Continuous analysis pauses during a review and resumes afterward. A game whose PGN has an evaluation on every main-line move (Lichess's `[%eval]`; the mating move may lack one) opens with a report built from them, until you run the review yourself; it has no best moves, so none count as Best and its blunders are not puzzles.
- **Clocks** from Lichess PGNs (`[%clk]`) show beside each move in the move list, and beside the board each side's clock after its latest move, the side to move's in green. These and other `[%…]` commands are hidden from the comments but kept in the PGN and on export.
- **Train on mistakes** turns every blunder (a loss of at least 20 win-chance points) from all reviewed games into a puzzle: the board shows the position before the move, and you look for a better one. Enter a player name to train only on that player's moves, or right-click a game and choose **Train on this game** for both sides' blunders in that game alone. A move counts as correct when it matches the review's suggested move, so another strong move may be marked as different. **Show answer** reveals the engine line. Training never changes the saved games.
- **Mark a move:** right-click it in the move list and choose !!, !, !?, ?!, ? or ??. Marks from imported PGNs show too, and after **Analyze whole game** its inaccuracies, mistakes and blunders appear as ?!, ? and ??. Your own mark replaces either; review marks are not exported.
- **Draw on the board:** right-drag for an arrow, right-click a square for a circle; hold Shift, Option or both for red, blue or yellow. Drawing a shape again removes it. Drawings are saved with each move, and each game remembers whether you flipped its board.
- Write notes at the starting position or after any move, including variations. Moves, variations, notes, arrows and position tags are not saved until you choose **Save** (⌘S); **Discard** goes back to the last saved version. Opening another game, a new board, play, training, an export, another library folder, closing the window or ⌘Q first asks to **Save**, **Discard** or **Cancel**. Favourites, game tags and the board's orientation save at once. If saving fails, edits remain open and closing is blocked.
- **Export game** writes the selected game. **Export results** writes every game matching the current filters, across all pages. Both include your notes, original annotations, NAGs, imported variations and newly added lines.
- Imports run in the background. **Stop** retains completed work. Reimporting the same parsed game skips it without overwriting notes.

</details>

<details>
<summary>How review scoring works</summary>

Reviews use the [published Lichess move-accuracy equations](https://lichess.org/page/accuracy): convert White's centipawn score to `100 / (1 + exp(-0.00368208 * cp))`, then measure the mover's loss in that value. Mate and terminal outcomes map to 0/100, or 50 for a draw. Move accuracy is `103.1668 * exp(-0.04354 * loss) - 3.1669`, clamped to 0–100 (no loss = 100).

ChessLib reports **approximate accuracy** as the arithmetic mean of that side's move accuracies. Losses of at least 5, 10 and 20 percentage points are classified as inaccuracy, mistake and blunder. Other moves are Best if they match the engine's suggestion, otherwise Good. Scores vary with search time. The review sends the entire move history to Stockfish so repetition context is available. Resignation results are not treated as a forced mate in the final position.

</details>

<details>
<summary>Local storage, backups and large collections</summary>

The default database is `~/Library/Application Support/ChessLib/library.sqlite3`. **Library folder…** at the top of the sidebar (hover it to see the current file) chooses another folder: ChessLib opens the `library.sqlite3` in it, or starts a new library there, remembers the choice, and restarts on it. It does not move or copy the current library; to move one, quit the app and move `library.sqlite3` together with any `-wal` and `-shm` files beside it, then choose the new folder. `CHESSLIB_DB=/absolute/path/library.sqlite3` overrides both for development or a separate library. Avoid folders that sync while the app is open (iCloud Drive, Dropbox): syncing a live SQLite database can corrupt it.

PGN input is streamed one game at a time. SQLite commits batches of 200 games, uses WAL, and indexes full-text search with FTS5. The GUI holds 80 result rows and the selected game; it does not load the entire library. Pagination uses an ID cursor instead of increasingly expensive offsets. Search, imports, loading, and export run off the UI thread. Note writes are synchronous and may briefly wait for an import transaction.

Original PGN files are never changed during import. Imported tags and move tokens also remain unchanged in storage when studying: added moves are saved separately inside the game's record. Existing main-line note indices remain valid, and added/imported branch notes use stable node IDs. Reimporting the original PGN still skips it without removing study work. Use this version or newer for libraries with added variations; older builds do not understand that study data. Games are stored as parsed tags and tokens; export normalizes formatting rather than reproducing original bytes. A duplicate means identical parsed content, not merely the same moves. Files with the same basename share a source filter; duplicate games remain under their first source.

For a complete backup, quit the app and copy the database. While the app is running, SQLite may also use `-wal` and `-shm` sidecars; copying only the main file at that time is not a reliable backup. PGN export is portable but does not preserve favourites or source grouping.

</details>

<details>
<summary>Supported PGNs and current limits</summary>

- Standard chess and legal FEN starting positions. Unsupported variants remain stored and exportable, with a display error. An illegal imported move disables that continuation with an explanation; other legal branches remain playable and the original tokens survive export.
- Variations can be added, explored, promoted and (if you added them) deleted, with notes on each position. Deleting imported variations and editing imported annotations are not included.
- No opening tree, online sync, position search, or collection deletion. The app's game editor covers Title, White, Black, Event and Date only; `chesslib games set` edits any header except the starting position (FEN, SetUp, Variant). Position analysis supports one-shot and continuous modes. Full-game review covers the original main line; it does not classify study variations.
- Notes are separate from original annotations. On export, braces in comments are replaced with parentheses because PGN brace comments cannot nest.
- UTF-8 PGNs are supported; invalid byte sequences are replaced by the Unicode replacement character. Very long comments may be normalized into multiple comments. A single game is capped at 16 MiB of parsed content to bound accidental memory growth. Malformed PGNs should be checked against their originals; the PGN reader is permissive.
- The build script signs the app ad hoc for local use.

</details>

## Command line for scripts and agents

The app binary is also a command-line tool. With arguments, it works on the library instead of opening a window: importing and exporting PGN, listing and searching games, editing headers, favourites, folders, tags, notes, move marks, bookmarks, drawings and added variations. Analysis and play against Stockfish are app-only. Every command prints one line of JSON, except `games export` without `--out`, which prints PGN. Errors go to standard error with exit status 1. Flags a command does not take are rejected before anything changes.

```sh
CHESSLIB=/Applications/ChessLib.app/Contents/MacOS/chesslib
$CHESSLIB help                                   # every command
$CHESSLIB folders create "Najdorf" --parent 3    # {"id":7}
$CHESSLIB games import course.pgn --to 7         # {"ids":[41,42],"imported":2,...}
pbpaste | $CHESSLIB games import -               # PGN or a FEN from standard input
$CHESSLIB games list --tag Endgame --folder 3    # 80 games, newest first; page with --before
$CHESSLIB games show 41                          # headers, PGN and every position (node) with its FEN
$CHESSLIB games play 41 12 Nf3 Nc6               # add a variation after node 12
$CHESSLIB games note 41 13 "The point of the line"
$CHESSLIB tags add 41 Sicilian
$CHESSLIB games export --folder 3 --out najdorf.pgn
```

`$CHESSLIB library use ~/Chess/Openings` makes later commands open that library (a `library.sqlite3` file, or a folder holding one); `$CHESSLIB library use none` goes back to the app's library. It is remembered in `~/Library/Application Support/ChessLib/cli-library` and does not change the library the app opens. In order, a command opens `--db PATH`, then `CHESSLIB_DB`, then the library set with `library use`, then the app's library folder, then the default database. The app's library folder is read from the app's own settings, which a symlink to the binary cannot see: it opens the default database instead. Run the binary inside the app, use an alias or a script containing `exec /Applications/ChessLib.app/Contents/MacOS/chesslib "$@"`, or set the library once with `library use`. `chesslib library` shows which file a command uses.

The app and the command line can run together. The app shows command-line changes when its list next refreshes. A game open in the app keeps the app's copy: if you edit that game in the app, its next save replaces the command-line edits.

## Development

ChessLib is built with Rust, eframe/egui, pgn-reader, shakmaty, and bundled SQLite. See the [design notes](docs/design.md) for the visual system.

<details>
<summary>Tests and the 100,000-game benchmark</summary>

```sh
cargo test --locked
cargo clippy --all-targets --locked -- -D warnings
cargo test --release --locked -- --ignored --nocapture
```

The ignored library test generates 100,000 distinct 33-ply games, imports them into a temporary database, checks first/deep pages and indexed search, saves a note, exports a game, verifies SQLite integrity, and reports timings. Its temporary files are removed afterward.

Measured on the development Mac (Apple Silicon, release build, 12 November 2026): 100,000 games imported in **7.61 s**, first page **254 ms**, indexed player search **11 ms**, deep page **42 ms**. This is one synthetic local run, not a guarantee for arbitrary PGNs or multi-million-game databases.

</details>

## License

[GPL-3.0-or-later](LICENSE).
