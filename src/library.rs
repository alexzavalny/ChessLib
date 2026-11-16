use crate::pgn::{Game, Notes, Parser};
use anyhow::{Context, Result};
use rusqlite::{Connection, params};
use std::{
    io::{BufReader, BufWriter, Write},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::Duration,
};

pub const PAGE_SIZE: usize = 80;

pub fn open(path: &Path) -> Result<Connection> {
    let db = Connection::open(path)?;
    db.busy_timeout(Duration::from_secs(5))?;
    db.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;
        CREATE TABLE IF NOT EXISTS games (
            id INTEGER PRIMARY KEY, hash TEXT NOT NULL UNIQUE,
            white TEXT NOT NULL, black TEXT NOT NULL, event TEXT NOT NULL,
            date TEXT NOT NULL, result TEXT NOT NULL, eco TEXT NOT NULL,
            source TEXT NOT NULL, body TEXT NOT NULL, notes TEXT NOT NULL DEFAULT '{}',
            starred INTEGER NOT NULL DEFAULT 0, search_text TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS games_source_id ON games(source, id);
        CREATE INDEX IF NOT EXISTS games_starred_id ON games(starred, id);
        CREATE INDEX IF NOT EXISTS games_event_date ON games(event, date);
        CREATE VIRTUAL TABLE IF NOT EXISTS games_fts USING fts5(search_text, content='games', content_rowid='id', tokenize='unicode61 remove_diacritics 2');
        CREATE TRIGGER IF NOT EXISTS games_ai AFTER INSERT ON games BEGIN
          INSERT INTO games_fts(rowid,search_text) VALUES(new.id,new.search_text); END;
        CREATE TRIGGER IF NOT EXISTS games_au AFTER UPDATE OF search_text ON games BEGIN
          INSERT INTO games_fts(games_fts,rowid,search_text) VALUES('delete',old.id,old.search_text);
          INSERT INTO games_fts(rowid,search_text) VALUES(new.id,new.search_text); END;
        CREATE TRIGGER IF NOT EXISTS games_ad AFTER DELETE ON games BEGIN
          INSERT INTO games_fts(games_fts,rowid,search_text) VALUES('delete',old.id,old.search_text); END;
        -- Tags on a game (node NULL) or one of its positions; an index of the game JSON, rewritten by save().
        CREATE TABLE IF NOT EXISTS labels (game INTEGER NOT NULL REFERENCES games(id) ON DELETE CASCADE, node INTEGER, name TEXT NOT NULL);
        CREATE INDEX IF NOT EXISTS labels_name ON labels(name, game);
        CREATE INDEX IF NOT EXISTS labels_game ON labels(game);")?;
    // Nested folders; each game sits in at most one. Checks the column, not user_version, which older builds reset to 1.
    if db.query_row("SELECT count(*) FROM pragma_table_info('games') WHERE name='folder'", [], |r| r.get::<_, i64>(0))? == 0 {
        db.execute_batch("BEGIN;
            CREATE TABLE IF NOT EXISTS folders (id INTEGER PRIMARY KEY, parent INTEGER REFERENCES folders(id), name TEXT NOT NULL);
            ALTER TABLE games ADD COLUMN folder INTEGER REFERENCES folders(id);
            CREATE INDEX games_folder_id ON games(folder, id);
            COMMIT;")?;
    }
    // A title for lines and positions that are not games; the game body keeps it as a PGN "Title" tag.
    if db.query_row("SELECT count(*) FROM pragma_table_info('games') WHERE name='title'", [], |r| r.get::<_, i64>(0))? == 0 {
        db.execute_batch("ALTER TABLE games ADD COLUMN title TEXT NOT NULL DEFAULT '';")?;
    }
    db.execute_batch("PRAGMA user_version=2;")?;
    Ok(db)
}

#[derive(Clone, Default)]
pub struct Filter {
    pub query: String,
    pub source: String,
    pub starred: bool,
    /// Only games of this event (exact name); None for any.
    pub event: Option<String>,
    /// Only games in this folder or its subfolders; None for any.
    pub folder: Option<i64>,
    /// Only games with this tag on the game or any of its positions; None for any.
    pub tag: Option<String>,
}
impl Filter {
    fn fts(&self) -> String {
        self.query
            .split_whitespace()
            .take(16)
            .map(|word| format!("\"{}\"*", word.replace('"', "\"\"")))
            .collect::<Vec<_>>()
            .join(" AND ")
    }
    fn clause(&self) -> String {
        format!(
            "WHERE (?1 = '' OR source = ?1) AND (?2 = 0 OR starred = 1) AND (?4 IS NULL OR event = ?4)
            AND (?5 IS NULL OR folder IN (WITH RECURSIVE t(id) AS (SELECT ?5 UNION ALL SELECT f.id FROM folders f JOIN t ON f.parent = t.id) SELECT id FROM t))
            AND (?6 IS NULL OR id IN (SELECT game FROM labels WHERE name = ?6)) {}",
            if self.fts().is_empty() {
                "AND ?3 = ''"
            } else {
                "AND id IN (SELECT rowid FROM games_fts WHERE games_fts MATCH ?3)"
            }
        )
    }
}
#[derive(Clone)]
pub struct Row {
    pub id: i64,
    pub title: String,
    pub white: String,
    pub black: String,
    pub event: String,
    pub date: String,
    pub result: String,
    pub eco: String,
    pub starred: bool,
    /// Every tag on the game and its positions, sorted; filled by search() only.
    pub labels: Vec<String>,
    /// The breadcrumb for a jump result, if the game is in a folder.
    pub folder_path: Option<String>,
}
pub struct Page {
    pub rows: Vec<Row>,
    pub total: i64,
    pub all: i64,
    pub sources: Vec<(String, i64)>,
    pub folders: Vec<Folder>,
    /// Every tag with the number of games carrying it, sorted by name.
    pub labels: Vec<(String, i64)>,
    pub more: bool,
}
/// A folder with the number of games in it and all its subfolders; folders come sorted by name.
#[derive(Clone)]
pub struct Folder {
    pub id: i64,
    pub parent: Option<i64>,
    pub name: String,
    pub games: i64,
}
pub fn folders(db: &Connection) -> Result<Vec<Folder>> {
    let mut folders: Vec<Folder> = db
        .prepare("SELECT id, parent, name, (SELECT count(*) FROM games WHERE folder = folders.id) FROM folders ORDER BY name COLLATE NOCASE")?
        .query_map([], |r| Ok(Folder { id: r.get(0)?, parent: r.get(1)?, name: r.get(2)?, games: r.get(3)? }))?
        .collect::<rusqlite::Result<_>>()?;
    // Add each folder's own count to every ancestor.
    let parents: std::collections::HashMap<i64, Option<i64>> = folders.iter().map(|f| (f.id, f.parent)).collect();
    let own: Vec<(Option<i64>, i64)> = folders.iter().map(|f| (f.parent, f.games)).collect();
    for (mut parent, games) in own {
        while let Some(id) = parent {
            folders.iter_mut().find(|f| f.id == id).unwrap().games += games;
            parent = parents[&id];
        }
    }
    Ok(folders)
}
pub fn create_folder(db: &Connection, parent: Option<i64>, name: &str) -> Result<i64> {
    db.execute("INSERT INTO folders(parent, name) VALUES(?, ?)", params![parent, name])?;
    Ok(db.last_insert_rowid())
}
pub fn rename_folder(db: &Connection, id: i64, name: &str) -> Result<()> {
    db.execute("UPDATE folders SET name=? WHERE id=?", params![name, id])?;
    Ok(())
}
/// Deletes only the folder: its games and subfolders move up into its parent.
pub fn delete_folder(db: &Connection, id: i64) -> Result<()> {
    let tx = db.unchecked_transaction()?;
    let parent: Option<i64> = tx.query_row("SELECT parent FROM folders WHERE id=?", [id], |r| r.get(0))?;
    tx.execute("UPDATE games SET folder=? WHERE folder=?", params![parent, id])?;
    tx.execute("UPDATE folders SET parent=? WHERE parent=?", params![parent, id])?;
    tx.execute("DELETE FROM folders WHERE id=?", [id])?;
    tx.commit()?;
    Ok(())
}
/// Files games imported after `after` that have no folder yet into `folder`.
pub fn file_imported(db: &Connection, after: i64, folder: i64) -> Result<usize> {
    Ok(db.execute("UPDATE games SET folder=?1 WHERE id > ?2 AND folder IS NULL", params![folder, after])?)
}
/// Moves one game, or with `id` None every game matching `filter`, into `folder` (None takes them out of folders).
pub fn move_games(db: &Connection, filter: &Filter, id: Option<i64>, folder: Option<i64>) -> Result<usize> {
    Ok(db.execute(
        &format!("UPDATE games SET folder=?8 {} AND (?7 IS NULL OR id=?7)", filter.clause()),
        params![filter.source, filter.starred, filter.fts(), filter.event, filter.folder, filter.tag, id, folder],
    )?)
}
pub struct Loaded {
    pub id: i64,
    pub game: Game,
    pub notes: Notes,
    pub starred: bool,
}

fn row(r: &rusqlite::Row) -> rusqlite::Result<Row> {
    Ok(Row {
        id: r.get(0)?,
        white: r.get(1)?,
        black: r.get(2)?,
        event: r.get(3)?,
        date: r.get(4)?,
        result: r.get(5)?,
        eco: r.get(6)?,
        starred: r.get(7)?,
        title: r.get(8)?,
        labels: Vec::new(),
        folder_path: None,
    })
}

/// Games whose players contain every word of `query` anywhere, newest first: "Gri Pla" finds
/// "Grigorijs … vs Platons …". Case-insensitive for ASCII only (SQLite LIKE).
pub fn jump(db: &Connection, query: &str) -> Result<Vec<Row>> {
    let words: Vec<String> = query
        .split_whitespace()
        .take(16)
        .map(|w| format!("%{}%", w.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_")))
        .collect();
    let sql = format!(
        "WITH RECURSIVE matches AS MATERIALIZED (
            SELECT id, white, black, event, date, result, eco, starred, title, folder
            FROM games WHERE 1 {} ORDER BY id DESC LIMIT 10
        ), breadcrumbs(game, parent, path) AS (
            SELECT m.id, f.parent, f.name FROM matches m JOIN folders f ON f.id = m.folder
            UNION ALL
            SELECT b.game, f.parent, f.name || ' › ' || b.path
            FROM breadcrumbs b JOIN folders f ON f.id = b.parent
        )
        SELECT m.id, m.white, m.black, m.event, m.date, m.result, m.eco, m.starred, m.title, b.path
        FROM matches m LEFT JOIN breadcrumbs b ON b.game = m.id AND b.parent IS NULL
        ORDER BY m.id DESC",
        " AND (title || ' ' || white || ' ' || black) LIKE ? ESCAPE '\\'".repeat(words.len())
    );
    Ok(db
        .prepare(&sql)?
        .query_map(rusqlite::params_from_iter(words), |r| {
            let mut game = row(r)?;
            game.folder_path = r.get(9)?;
            Ok(game)
        })?
        .collect::<rusqlite::Result<_>>()?)
}

pub fn search(db: &Connection, filter: &Filter, before: i64) -> Result<Page> {
    let clause = filter.clause();
    let fts = filter.fts();
    let total = db.query_row(
        &format!("SELECT count(*) FROM games {clause}"),
        params![filter.source, filter.starred, fts, filter.event, filter.folder, filter.tag],
        |r| r.get(0),
    )?;
    let all = db.query_row("SELECT count(*) FROM games", [], |r| r.get(0))?;
    let mut stmt = db.prepare(&format!("SELECT id, white, black, event, date, result, eco, starred, title FROM games {clause} AND id < ?7 ORDER BY id DESC LIMIT ?8"))?;
    let mut rows = stmt
        .query_map(
            params![
                filter.source,
                filter.starred,
                fts,
                filter.event,
                filter.folder,
                filter.tag,
                before,
                PAGE_SIZE as i64 + 1
            ],
            row,
        )?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let more = rows.len() > PAGE_SIZE;
    rows.truncate(PAGE_SIZE);
    let mut names = db.prepare("SELECT DISTINCT name FROM labels WHERE game = ? ORDER BY name COLLATE NOCASE")?;
    for row in &mut rows {
        row.labels = names.query_map([row.id], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?;
    }
    // Every tag, counting only games in the selected folder and its subfolders; the tag menu offers them all.
    let labels = db
        .prepare(
            "WITH RECURSIVE t(id) AS (SELECT ?1 UNION ALL SELECT f.id FROM folders f JOIN t ON f.parent = t.id)
            SELECT l.name, count(DISTINCT CASE WHEN ?1 IS NULL OR g.folder IN (SELECT id FROM t) THEN l.game END)
            FROM labels l JOIN games g ON g.id = l.game GROUP BY l.name ORDER BY l.name COLLATE NOCASE",
        )?
        .query_map([filter.folder], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let sources = db
        .prepare("SELECT source, count(*) FROM games GROUP BY source ORDER BY source")?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(Page {
        rows,
        total,
        all,
        sources,
        folders: folders(db)?,
        labels,
        more,
    })
}

pub fn load(db: &Connection, id: i64) -> Result<Loaded> {
    let (body, notes, starred): (String, String, bool) = db.query_row(
        "SELECT body,notes,starred FROM games WHERE id=?",
        [id],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?;
    let mut game: Game = serde_json::from_str(&body)?;
    // Evaluations the PGN brought, as Lichess exports carry, stand in until Stockfish reviews the game.
    if game.review.is_none() {
        game.review = game.pgn_review();
    }
    Ok(Loaded {
        id,
        game,
        notes: serde_json::from_str(&notes)?,
        starred,
    })
}
/// Removes a game with its notes and analysis; the search index follows via the games_ad trigger.
pub fn delete(db: &Connection, id: i64) -> Result<()> {
    db.execute("DELETE FROM games WHERE id=?", [id])?;
    Ok(())
}
/// An event with its game count and first/last known dates (None when every date is unknown).
#[derive(Clone)]
pub struct EventRow {
    pub name: String,
    pub games: i64,
    pub first: Option<String>,
    pub last: Option<String>,
}
/// Every event, most recent first; events without a known date go last.
pub fn events(db: &Connection) -> Result<Vec<EventRow>> {
    Ok(db
        .prepare("SELECT event, count(*), min(CASE WHEN date GLOB '[0-9]*' THEN date END) AS first,
            max(CASE WHEN date GLOB '[0-9]*' THEN date END) AS last
            FROM games GROUP BY event ORDER BY last IS NULL, last DESC, event")?
        .query_map([], |r| Ok(EventRow { name: r.get(0)?, games: r.get(1)?, first: r.get(2)?, last: r.get(3)? }))?
        .collect::<rusqlite::Result<_>>()?)
}
/// Games added after `after` with no White, Black, Event or year ("?", "????.??.??" or blank).
pub fn missing_details(db: &Connection, after: i64) -> Result<Vec<i64>> {
    Ok(db
        .prepare("SELECT id FROM games WHERE id > ?1 AND (trim(white, ' ?') = '' OR trim(black, ' ?') = '' OR trim(event, ' ?') = '' OR date = '' OR date LIKE '?%') ORDER BY id")?
        .query_map([after], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?)
}
/// A position before a reviewed blunder, to replay as a puzzle.
pub struct Puzzle {
    pub white: String,
    pub black: String,
    pub date: String,
    pub position: shakmaty::Chess,
    pub played: String,
    pub best: String,
}
/// Blunders (a loss of 20+ win-chance points) from reviewed games, newest games first; milder
/// mistakes are too subtle to make clear puzzles. `game` limits it to one game. A non-blank `player` keeps only
/// moves by a side whose name contains every word of it (case-insensitive).
pub fn mistakes(db: &Connection, game: Option<i64>, player: &str) -> Result<Vec<Puzzle>> {
    let words: Vec<String> = player.split_whitespace().map(str::to_lowercase).collect();
    let mut puzzles = Vec::new();
    // shortcut: parses every reviewed game on the UI thread; move to the worker if reviews number in the thousands.
    let mut stmt = db.prepare("SELECT body FROM games WHERE instr(body, '\"review\":{') > 0 AND (?1 IS NULL OR id = ?1) ORDER BY id DESC")?;
    for body in stmt.query_map([game], |r| r.get::<_, String>(0))? {
        let body = body?;
        let game: Game = serde_json::from_str(&body)?;
        let (Some(review), Ok(positions), Ok(replay)) = (&game.review, game.mainline_positions(), game.replay()) else { continue };
        for m in review.moves.iter().filter(|m| m.quality == crate::engine::Quality::Blunder && !m.best.is_empty()) {
            let name = game.tag(if m.white { "White" } else { "Black" }).to_lowercase();
            let Some(i) = positions.iter().position(|(node, _)| *node == m.node) else { continue };
            if i == 0 || !words.iter().all(|w| name.contains(w.as_str())) {
                continue;
            }
            puzzles.push(Puzzle {
                white: game.tag("White").into(),
                black: game.tag("Black").into(),
                date: game.tag("Date").into(),
                position: positions[i - 1].1.clone(),
                played: replay.nodes[m.node].san.clone(),
                best: m.best.clone(),
            });
        }
    }
    Ok(puzzles)
}
pub fn last_id(db: &Connection) -> Result<i64> {
    Ok(db.query_row("SELECT coalesce(max(id), 0) FROM games", [], |r| r.get(0))?)
}
fn search_text(game: &Game, notes: &Notes) -> String {
    let mut text = game
        .tags
        .iter()
        .map(|(_, v)| v.as_str())
        .collect::<Vec<_>>()
        .join(" ");
    for token in &game.tokens {
        if let crate::pgn::Token::Comment(s) = token {
            text.push(' ');
            text.push_str(s);
        }
    }
    for note in notes.values() {
        text.push(' ');
        text.push_str(note);
    }
    text
}
pub fn save(db: &Connection, loaded: &Loaded) -> Result<()> {
    let tx = db.unchecked_transaction()?;
    anyhow::ensure!(
        tx.execute(
            "UPDATE games SET notes=?1,starred=?2,search_text=?3,body=?5,white=?6,black=?7,date=?8,event=?9,title=?10,result=?11,eco=?12 WHERE id=?4",
            params![
                serde_json::to_string(&loaded.notes)?,
                loaded.starred,
                search_text(&loaded.game, &loaded.notes),
                loaded.id,
                serde_json::to_string(&loaded.game)?,
                loaded.game.tag("White"),
                loaded.game.tag("Black"),
                loaded.game.tag("Date"),
                loaded.game.tag("Event"),
                loaded.game.tag("Title"),
                loaded.game.tag("Result"),
                loaded.game.tag("ECO")
            ]
        )? == 1,
        "Game no longer exists"
    );
    tx.execute("DELETE FROM labels WHERE game = ?", [loaded.id])?;
    let mut insert = tx.prepare("INSERT INTO labels(game, node, name) VALUES(?, ?, ?)")?;
    for name in &loaded.game.labels {
        insert.execute(params![loaded.id, None::<i64>, name])?;
    }
    for (node, names) in &loaded.game.position_labels {
        for name in names {
            insert.execute(params![loaded.id, *node as i64, name])?;
        }
    }
    drop(insert);
    tx.commit()?;
    Ok(())
}

#[derive(Default, Clone, Debug)]
pub struct ImportStats {
    pub imported: u64,
    pub duplicates: u64,
    pub read: u64,
    pub cancelled: bool,
}
pub fn import(
    db: &mut Connection,
    paths: &[PathBuf],
    cancel: &AtomicBool,
    mut progress: impl FnMut(ImportStats),
) -> Result<ImportStats> {
    let mut stats = ImportStats::default();
    for path in paths {
        let file =
            std::fs::File::open(path).with_context(|| format!("Opening {}", path.display()))?;
        let mut reader = pgn_reader::Reader::new(BufReader::new(file));
        let source = path.file_name().unwrap_or_default().to_string_lossy();
        let mut eof = false;
        while !eof && !cancel.load(Ordering::Relaxed) {
            let tx = db.transaction()?;
            let mut batch = stats.clone();
            for _ in 0..200 {
                if cancel.load(Ordering::Relaxed) {
                    break;
                }
                let Some(game) = reader.read_game(&mut Parser).with_context(|| {
                    format!(
                        "Reading {} after {} games; earlier batches are saved",
                        path.display(),
                        stats.read
                    )
                })?
                else {
                    eof = true;
                    break;
                };
                let game = game?;
                let body = serde_json::to_string(&game)?;
                let hash = blake3::hash(body.as_bytes()).to_hex().to_string();
                let inserted = tx.execute("INSERT OR IGNORE INTO games(hash,white,black,event,date,result,eco,source,body,search_text,title) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)", params![hash,game.tag("White"),game.tag("Black"),game.tag("Event"),game.tag("Date"),game.tag("Result"),game.tag("ECO"),source,body,search_text(&game,&Notes::new()),game.tag("Title")])?;
                batch.read += 1;
                if inserted == 1 {
                    batch.imported += 1;
                } else {
                    batch.duplicates += 1;
                }
            }
            tx.commit()?;
            stats = batch;
            progress(stats.clone());
        }
        if cancel.load(Ordering::Relaxed) {
            stats.cancelled = true;
            break;
        }
    }
    Ok(stats)
}

/// Writes one game, or with `id` None every game matching `filter`, as PGN with the reader's notes.
pub fn write_pgn(db: &Connection, filter: &Filter, id: Option<i64>, out: &mut impl Write) -> Result<u64> {
    let mut stmt = db.prepare(&format!(
        "SELECT body,notes FROM games {} AND (?7 IS NULL OR id=?7) ORDER BY id",
        filter.clause()
    ))?;
    let mut rows = stmt.query(params![filter.source, filter.starred, filter.fts(), filter.event, filter.folder, filter.tag, id])?;
    let mut count = 0;
    while let Some(row) = rows.next()? {
        let game: Game = serde_json::from_str(&row.get::<_, String>(0)?)?;
        let notes: Notes = serde_json::from_str(&row.get::<_, String>(1)?)?;
        out.write_all(game.export(&notes)?.as_bytes())?;
        count += 1;
    }
    Ok(count)
}
pub fn export(db: &Connection, filter: &Filter, id: Option<i64>, path: &Path) -> Result<u64> {
    // Write beside the destination, then rename: a failed export cannot truncate an existing file.
    let parent = path.parent().unwrap_or(Path::new("."));
    let temp = parent.join(format!(".chesslib-export-{}.tmp", std::process::id()));
    let file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp)?;
    let result = (|| -> Result<u64> {
        let mut out = BufWriter::new(file);
        let count = write_pgn(db, filter, id, &mut out)?;
        out.flush()?;
        out.get_ref().sync_all()?;
        std::fs::rename(&temp, path)?;
        Ok(count)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(temp);
    }
    result
}

pub enum Request {
    Search(u64, Filter, i64),
    Load(u64, i64),
    Export(Filter, Option<i64>, PathBuf),
}
pub enum Event {
    Page(u64, Result<Page>),
    Loaded(u64, Result<Loaded>),
    Exported(Result<u64>),
    Progress(ImportStats),
    Imported(Result<ImportStats>),
}
pub fn worker(
    path: PathBuf,
    ctx: eframe::egui::Context,
) -> (
    mpsc::Sender<Request>,
    mpsc::Sender<Event>,
    mpsc::Receiver<Event>,
) {
    let (send, requests) = mpsc::channel();
    let (events, receive) = mpsc::channel();
    let responses = events.clone();
    std::thread::spawn(move || {
        let db = match open(&path) {
            Ok(db) => db,
            Err(e) => {
                let _ = responses.send(Event::Page(0, Err(e)));
                ctx.request_repaint();
                return;
            }
        };
        for request in requests {
            let event = match request {
                Request::Search(seq, filter, before) => {
                    Event::Page(seq, search(&db, &filter, before))
                }
                Request::Load(seq, id) => Event::Loaded(seq, load(&db, id)),
                Request::Export(filter, id, path) => {
                    Event::Exported(export(&db, &filter, id, &path))
                }
            };
            if responses.send(event).is_err() {
                break;
            }
            ctx.request_repaint();
        }
    });
    (send, events, receive)
}
pub fn start_import(
    path: PathBuf,
    paths: Vec<PathBuf>,
    events: mpsc::Sender<Event>,
    ctx: eframe::egui::Context,
    cancel: Arc<AtomicBool>,
) {
    std::thread::spawn(move || {
        let result = open(&path).and_then(|mut db| {
            import(&mut db, &paths, &cancel, |s| {
                let _ = events.send(Event::Progress(s));
                ctx.request_repaint();
            })
        });
        let _ = events.send(Event::Imported(result));
        ctx.request_repaint();
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn delete_removes_game_from_search() {
        let dir = tempfile::tempdir().unwrap();
        let mut db = open(&dir.path().join("db")).unwrap();
        let pgn = dir.path().join("input.pgn");
        std::fs::write(&pgn, "[White \"Tal\"]\n[Black \"Fischer\"]\n\n1. e4 *\n\n[White \"Carlsen\"]\n[Black \"Anand\"]\n\n1. d4 *").unwrap();
        import(&mut db, &[pgn], &AtomicBool::new(false), |_| {}).unwrap();
        let tal = Filter { query: "Tal".into(), ..Default::default() };
        delete(&db, search(&db, &tal, i64::MAX).unwrap().rows[0].id).unwrap();
        assert_eq!(search(&db, &tal, i64::MAX).unwrap().total, 0);
        assert_eq!(search(&db, &Filter::default(), i64::MAX).unwrap().all, 1);
        db.execute("INSERT INTO games_fts(games_fts) VALUES('integrity-check')", []).unwrap();
    }
    #[test]
    fn nested_folders_filter_move_and_delete() {
        let dir = tempfile::tempdir().unwrap();
        let mut db = open(&dir.path().join("db")).unwrap();
        let pgn = dir.path().join("input.pgn");
        std::fs::write(&pgn, "[White \"Tal\"]\n\n1. e4 *\n\n[White \"Carlsen\"]\n\n1. d4 *\n\n[White \"Anand\"]\n\n1. c4 *").unwrap();
        import(&mut db, &[pgn], &AtomicBool::new(false), |_| {}).unwrap();
        let openings = create_folder(&db, None, "Openings").unwrap();
        let sicilian = create_folder(&db, Some(openings), "Sicilian").unwrap();
        let tal = Filter { query: "Tal".into(), ..Default::default() };
        assert_eq!(move_games(&db, &tal, None, Some(sicilian)).unwrap(), 1);
        let carlsen = search(&db, &Filter { query: "Carlsen".into(), ..Default::default() }, i64::MAX).unwrap().rows[0].id;
        move_games(&db, &Filter::default(), Some(carlsen), Some(openings)).unwrap();
        let count = |folder| search(&db, &Filter { folder: Some(folder), ..Default::default() }, i64::MAX).unwrap().total;
        assert_eq!((count(openings), count(sicilian)), (2, 1), "a folder includes its subfolders");
        let page = search(&db, &Filter::default(), i64::MAX).unwrap();
        let games = |id| page.folders.iter().find(|f| f.id == id).unwrap().games;
        assert_eq!((games(openings), games(sicilian)), (2, 1));
        let recent = jump(&db, "").unwrap();
        let paths: Vec<_> = recent.iter().map(|r| r.folder_path.as_deref()).collect();
        assert_eq!(paths, [None, Some("Openings"), Some("Openings › Sicilian")]);
        rename_folder(&db, sicilian, "Najdorf").unwrap();
        assert_eq!(jump(&db, "Tal").unwrap()[0].folder_path.as_deref(), Some("Openings › Najdorf"));
        let inner = create_folder(&db, Some(sicilian), "6.Bg5").unwrap();
        delete_folder(&db, sicilian).unwrap();
        assert_eq!(jump(&db, "Tal").unwrap()[0].folder_path.as_deref(), Some("Openings"));
        assert_eq!(count(openings), 2, "deleting a folder keeps its games");
        assert_eq!(folders(&db).unwrap().iter().find(|f| f.id == inner).unwrap().parent, Some(openings));
        assert_eq!(search(&db, &Filter::default(), i64::MAX).unwrap().all, 3);
        drop(db);
        assert_eq!(open(&dir.path().join("db")).unwrap().query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0)).unwrap(), 2);
    }
    #[test]
    fn tags_on_games_and_positions_filter_and_follow_saves() {
        let dir = tempfile::tempdir().unwrap();
        let mut db = open(&dir.path().join("db")).unwrap();
        let pgn = dir.path().join("input.pgn");
        std::fs::write(&pgn, "[White \"Tal\"]\n\n1. e4 e5 *\n\n[White \"Carlsen\"]\n\n1. d4 *").unwrap();
        import(&mut db, &[pgn], &AtomicBool::new(false), |_| {}).unwrap();
        let id = |name: &str| search(&db, &Filter { query: name.into(), ..Default::default() }, i64::MAX).unwrap().rows[0].id;
        let (tal, carlsen) = (id("Tal"), id("Carlsen"));
        let mut game = load(&db, tal).unwrap();
        game.game.labels.insert("Attack".into());
        game.game.position_labels.insert(2, ["Open game".into(), "Attack".into()].into());
        save(&db, &game).unwrap();
        let mut other = load(&db, carlsen).unwrap();
        other.game.position_labels.insert(1, ["Open game".into()].into());
        save(&db, &other).unwrap();
        let tagged = |tag: &str| search(&db, &Filter { tag: Some(tag.into()), ..Default::default() }, i64::MAX).unwrap();
        assert_eq!(tagged("Attack").total, 1, "a game counts once however many of its positions carry the tag");
        assert_eq!(tagged("Open game").total, 2);
        let page = tagged("Attack");
        assert_eq!(page.labels, vec![("Attack".into(), 1), ("Open game".into(), 2)]);
        assert_eq!(page.rows[0].labels, vec!["Attack", "Open game"]);
        let parent = create_folder(&db, None, "Openings").unwrap();
        let child = create_folder(&db, Some(parent), "e4").unwrap();
        move_games(&db, &Filter::default(), Some(tal), Some(child)).unwrap();
        let in_folder = search(&db, &Filter { folder: Some(parent), ..Default::default() }, i64::MAX).unwrap();
        assert_eq!(in_folder.labels, vec![("Attack".into(), 1), ("Open game".into(), 1)], "counts cover the folder and its subfolders");
        let empty = create_folder(&db, None, "Empty").unwrap();
        let none = search(&db, &Filter { folder: Some(empty), ..Default::default() }, i64::MAX).unwrap();
        assert_eq!(none.labels, vec![("Attack".into(), 0), ("Open game".into(), 0)], "every tag stays listed");
        assert_eq!(load(&db, tal).unwrap().game.position_labels[&2].len(), 2, "tags round-trip through the game JSON");
        game.game.labels.clear();
        game.game.position_labels.clear();
        save(&db, &game).unwrap();
        assert_eq!(tagged("Attack").total, 0, "removed tags leave the index");
        delete(&db, carlsen).unwrap();
        assert!(tagged("Open game").labels.is_empty(), "deleting a game drops its tags");
    }
    #[test]
    fn failed_export_preserves_existing_destination() {
        let dir = tempfile::tempdir().unwrap();
        let mut db = open(&dir.path().join("db")).unwrap();
        let pgn = dir.path().join("input.pgn");
        std::fs::write(&pgn, include_str!("../fixtures/sample.pgn")).unwrap();
        import(&mut db, &[pgn], &AtomicBool::new(false), |_| {}).unwrap();
        db.execute("UPDATE games SET body='invalid json'", [])
            .unwrap();
        let output = dir.path().join("keep.pgn");
        std::fs::write(&output, "existing data").unwrap();
        assert!(export(&db, &Filter::default(), None, &output).is_err());
        assert_eq!(std::fs::read_to_string(output).unwrap(), "existing data");
        assert!(
            !dir.path()
                .join(format!(".chesslib-export-{}.tmp", std::process::id()))
                .exists()
        );
    }
    #[test]
    #[ignore = "100,000-game storage and search benchmark; run with --release --ignored --nocapture"]
    fn large_library_benchmark() {
        use std::time::Instant;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("large.db");
        let pgn = dir.path().join("large.pgn");
        let mut file = BufWriter::new(std::fs::File::create(&pgn).unwrap());
        let moves = include_str!("../fixtures/sample.pgn")
            .split_once("\n\n")
            .unwrap()
            .1;
        for n in 0..100_000 {
            writeln!(file, "[White \"Player {n}\"]\n[Black \"Opponent {}\"]\n[Event \"Tournament {}\"]\n[Result \"1-0\"]\n\n{moves}", n%500,n%50).unwrap();
        }
        file.flush().unwrap();
        let mut db = open(&path).unwrap();
        let now = Instant::now();
        let stats = import(&mut db, &[pgn], &AtomicBool::new(false), |_| {}).unwrap();
        let import_time = now.elapsed();
        assert_eq!(stats.imported, 100_000);
        let now = Instant::now();
        let first = search(&db, &Filter::default(), i64::MAX).unwrap();
        let page_time = now.elapsed();
        assert_eq!(first.total, 100_000);
        assert_eq!(first.rows.len(), PAGE_SIZE);
        let now = Instant::now();
        let found = search(
            &db,
            &Filter {
                query: "Player 99999".into(),
                ..Default::default()
            },
            i64::MAX,
        )
        .unwrap();
        let search_time = now.elapsed();
        assert_eq!(found.total, 1);
        let now = Instant::now();
        assert!(jump(&db, "nobody here").unwrap().is_empty());
        let jump_time = now.elapsed();
        let now = Instant::now();
        let deep = search(&db, &Filter::default(), 1000).unwrap();
        let deep_time = now.elapsed();
        assert_eq!(deep.rows.len(), PAGE_SIZE);
        let mut game = load(&db, found.rows[0].id).unwrap();
        game.notes.insert(12, "Benchmark annotation".into());
        save(&db, &game).unwrap();
        assert_eq!(game.game.replay().unwrap().line(0).len(), 34);
        let out = dir.path().join("export.pgn");
        assert_eq!(
            export(&db, &Filter::default(), Some(game.id), &out).unwrap(),
            1
        );
        assert!(
            std::fs::read_to_string(out)
                .unwrap()
                .contains("Benchmark annotation")
        );
        let integrity: String = db
            .query_row("PRAGMA integrity_check", [], |r| r.get(0))
            .unwrap();
        assert_eq!(integrity, "ok");
        eprintln!(
            "100,000 games: import={import_time:?}; first page={page_time:?}; indexed search={search_time:?}; deep page={deep_time:?}; jump miss={jump_time:?}"
        );
    }
    #[test]
    fn import_deduplicate_search_save_reopen_export() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("library.db");
        let pgn = dir.path().join("test.pgn");
        std::fs::write(&pgn,"[White \"Tal\"]\n[Black \"Fischer\"]\n[Event \"Candidates\"]\n\n1. e4 c5 *\n\n[White \"Carlsen\"]\n[Black \"Anand\"]\n\n1. d4 d5 *").unwrap();
        let mut db = open(&path).unwrap();
        let cancel = AtomicBool::new(false);
        assert_eq!(
            import(&mut db, std::slice::from_ref(&pgn), &cancel, |_| {})
                .unwrap()
                .imported,
            2
        );
        assert_eq!(
            import(&mut db, &[pgn], &cancel, |_| {}).unwrap().duplicates,
            2
        );
        let filter = Filter {
            query: "Fis".into(),
            ..Default::default()
        };
        let page = search(&db, &filter, i64::MAX).unwrap();
        assert_eq!(page.total, 1);
        let events = events(&db).unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!((events[1].name.as_str(), events[1].games, events[1].last.as_deref()), ("Candidates", 1, None));
        let candidates = Filter { event: Some("Candidates".into()), ..Default::default() };
        assert_eq!(search(&db, &candidates, i64::MAX).unwrap().rows[0].white, "Tal");
        let missing = missing_details(&db, 0).unwrap();
        assert_eq!(missing.len(), 2, "neither fixture game has a date");
        assert!(missing_details(&db, last_id(&db).unwrap()).unwrap().is_empty());
        let mut dated = load(&db, missing[1]).unwrap();
        dated.game.set_tag("Date", "2024.03.01".into());
        dated.game.set_tag("White", "Magnus Carlsen".into());
        dated.game.set_tag("Event", "Norway Chess".into());
        save(&db, &dated).unwrap();
        assert_eq!(missing_details(&db, 0).unwrap(), [missing[0]]);
        assert_eq!(jump(&db, "Magnus").unwrap()[0].date, "2024.03.01");
        assert_eq!(jump(&db, "Magnus").unwrap()[0].event, "Norway Chess");
        dated.game.set_tag("Title", "Bird trap".into());
        save(&db, &dated).unwrap();
        assert_eq!(jump(&db, "trap").unwrap()[0].title, "Bird trap");
        dated.game.set_tag("White", "Carlsen".into());
        save(&db, &dated).unwrap();
        let names = |q| jump(&db, q).unwrap().into_iter().map(|r| r.white).collect::<Vec<_>>();
        assert_eq!(names("arl nan"), ["Carlsen"]);
        assert_eq!(names("ISCH"), ["Tal"]);
        assert_eq!(names(""), ["Carlsen", "Tal"]);
        assert!(names("Tal Anand").is_empty());
        assert!(names("%").is_empty());
        let mut game = load(&db, page.rows[0].id).unwrap();
        game.notes.insert(1, "Excellent preparation".into());
        game.starred = true;
        save(&db, &game).unwrap();
        drop(db);
        let db = open(&path).unwrap();
        assert_eq!(
            load(&db, game.id).unwrap().notes[&1],
            "Excellent preparation"
        );
        assert_eq!(
            search(
                &db,
                &Filter {
                    query: "preparation".into(),
                    starred: true,
                    ..Default::default()
                },
                i64::MAX
            )
            .unwrap()
            .total,
            1
        );
        assert_eq!(
            search(
                &db,
                &Filter {
                    query: "\" OR *".into(),
                    ..Default::default()
                },
                i64::MAX
            )
            .unwrap()
            .total,
            0
        );
        let out = dir.path().join("out.pgn");
        assert_eq!(export(&db, &Filter::default(), None, &out).unwrap(), 2);
        assert!(
            std::fs::read_to_string(out)
                .unwrap()
                .contains("Excellent preparation")
        );
    }
    #[test]
    fn variations_survive_reopen_and_original_reimport_keeps_notes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("library.db");
        let pgn = dir.path().join("input.pgn");
        std::fs::write(&pgn, "1. e4 e5 (1... c5) 2. Nf3 *").unwrap();
        let mut db = open(&path).unwrap();
        import(
            &mut db,
            std::slice::from_ref(&pgn),
            &AtomicBool::new(false),
            |_| {},
        )
        .unwrap();
        let mut loaded = load(&db, 1).unwrap();
        loaded.notes.insert(1, "Existing note".into());
        let mut replay = loaded.game.replay().unwrap();
        let c5 = replay.nodes[1].children[1];
        let pos = replay.nodes[c5].position.as_ref().unwrap();
        let movement = "Nc3"
            .parse::<shakmaty::san::San>()
            .unwrap()
            .to_move(pos)
            .unwrap();
        let branch = loaded.game.play(&mut replay, c5, movement).unwrap();
        loaded.notes.insert(branch, "Branch preparation".into());
        let e5 = replay.nodes[1].children[0];
        loaded.game.review = Some(crate::engine::Report {
            moves: vec![crate::engine::ReviewedMove {
                node: 1,
                white: true,
                quality: crate::engine::Quality::Good,
                accuracy: 98.0,
                loss: 0.4,
                score: "+0.2".into(),
                best: "e4".into(),
                depth: 16,
            }, crate::engine::ReviewedMove {
                node: e5,
                white: false,
                quality: crate::engine::Quality::Blunder,
                accuracy: 10.0,
                loss: 30.0,
                score: "+3.0".into(),
                best: "c5 Nf3".into(),
                depth: 16,
            }],
            milliseconds_per_position: 500,
        });
        loaded.game.drawings.insert(branch, vec![(12, 28, 'G'), (27, 27, 'R')]);
        loaded.game.flipped = true;
        save(&db, &loaded).unwrap();
        drop(db);
        let mut db = open(&path).unwrap();
        let stats = import(&mut db, &[pgn], &AtomicBool::new(false), |_| {}).unwrap();
        assert_eq!(stats.duplicates, 1);
        let reopened = load(&db, 1).unwrap();
        assert_eq!(reopened.notes[&1], "Existing note");
        assert_eq!(reopened.notes[&branch], "Branch preparation");
        assert_eq!(reopened.game.drawings[&branch], [(12, 28, 'G'), (27, 27, 'R')]);
        assert!(reopened.game.flipped);
        assert_eq!(
            reopened.game.review.as_ref().unwrap().accuracy(true),
            Some(98.0)
        );
        assert_eq!(reopened.game.replay().unwrap().nodes[branch].san, "Nc3");
        let puzzles = mistakes(&db, None, "").unwrap();
        assert_eq!(puzzles.len(), 1, "only the blunder becomes a puzzle");
        assert_eq!((puzzles[0].played.as_str(), puzzles[0].best.as_str()), ("e5", "c5 Nf3"));
        assert_eq!(shakmaty::Position::turn(&puzzles[0].position), shakmaty::Color::Black);
        assert!(mistakes(&db, None, "Tal").unwrap().is_empty());
        assert_eq!(mistakes(&db, Some(1), "").unwrap().len(), 1);
        assert!(mistakes(&db, Some(2), "").unwrap().is_empty());
        assert_eq!(
            search(
                &db,
                &Filter {
                    query: "preparation".into(),
                    ..Default::default()
                },
                i64::MAX
            )
            .unwrap()
            .total,
            1
        );
        let out = dir.path().join("out.pgn");
        export(&db, &Filter::default(), Some(1), &out).unwrap();
        let text = std::fs::read_to_string(&out).unwrap();
        assert!(text.contains("Nc3") && text.contains("Branch preparation"));
        db.execute(
            "UPDATE games SET body=?1",
            ["{\"tags\":[],\"tokens\":[],\"study\":[{\"parent\":99,\"san\":\"e4\"}]}"],
        )
        .unwrap();
        assert!(export(&db, &Filter::default(), Some(1), &out).is_err());
        assert_eq!(std::fs::read_to_string(out).unwrap(), text);
    }

    #[test]
    fn paging_and_cancel_are_bounded() {
        let dir = tempfile::tempdir().unwrap();
        let mut db = open(&dir.path().join("db")).unwrap();
        let pgn = dir.path().join("games.pgn");
        let text = (0..205)
            .map(|n| format!("[White \"Player {n}\"]\n\n1. e4 *\n\n"))
            .collect::<String>();
        std::fs::write(&pgn, text).unwrap();
        let cancel = AtomicBool::new(false);
        let stats = import(&mut db, &[pgn], &cancel, |s| {
            if s.read >= 200 {
                cancel.store(true, Ordering::Relaxed);
            }
        })
        .unwrap();
        assert!(stats.cancelled);
        assert_eq!(stats.imported, 200);
        let first = search(&db, &Filter::default(), i64::MAX).unwrap();
        assert_eq!(first.rows.len(), PAGE_SIZE);
        assert!(first.more);
        let second = search(&db, &Filter::default(), first.rows.last().unwrap().id).unwrap();
        assert!(
            second
                .rows
                .iter()
                .all(|r| r.id < first.rows.last().unwrap().id)
        );
    }
}
