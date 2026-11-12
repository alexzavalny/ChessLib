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
        CREATE VIRTUAL TABLE IF NOT EXISTS games_fts USING fts5(search_text, content='games', content_rowid='id', tokenize='unicode61 remove_diacritics 2');
        CREATE TRIGGER IF NOT EXISTS games_ai AFTER INSERT ON games BEGIN
          INSERT INTO games_fts(rowid,search_text) VALUES(new.id,new.search_text); END;
        CREATE TRIGGER IF NOT EXISTS games_au AFTER UPDATE OF search_text ON games BEGIN
          INSERT INTO games_fts(games_fts,rowid,search_text) VALUES('delete',old.id,old.search_text);
          INSERT INTO games_fts(rowid,search_text) VALUES(new.id,new.search_text); END;
        PRAGMA user_version=1;")?;
    Ok(db)
}

#[derive(Clone, Default)]
pub struct Filter {
    pub query: String,
    pub source: String,
    pub starred: bool,
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
            "WHERE (?1 = '' OR source = ?1) AND (?2 = 0 OR starred = 1) {}",
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
    pub white: String,
    pub black: String,
    pub event: String,
    pub date: String,
    pub result: String,
    pub eco: String,
    pub starred: bool,
}
pub struct Page {
    pub rows: Vec<Row>,
    pub total: i64,
    pub all: i64,
    pub sources: Vec<(String, i64)>,
    pub more: bool,
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
        "SELECT id, white, black, event, date, result, eco, starred FROM games WHERE 1 {} ORDER BY id DESC LIMIT 10",
        " AND (white || ' ' || black) LIKE ? ESCAPE '\\'".repeat(words.len())
    );
    Ok(db
        .prepare(&sql)?
        .query_map(rusqlite::params_from_iter(words), row)?
        .collect::<rusqlite::Result<_>>()?)
}

pub fn search(db: &Connection, filter: &Filter, before: i64) -> Result<Page> {
    let clause = filter.clause();
    let fts = filter.fts();
    let total = db.query_row(
        &format!("SELECT count(*) FROM games {clause}"),
        params![filter.source, filter.starred, fts],
        |r| r.get(0),
    )?;
    let all = db.query_row("SELECT count(*) FROM games", [], |r| r.get(0))?;
    let mut stmt = db.prepare(&format!("SELECT id, white, black, event, date, result, eco, starred FROM games {clause} AND id < ?4 ORDER BY id DESC LIMIT ?5"))?;
    let mut rows = stmt
        .query_map(
            params![
                filter.source,
                filter.starred,
                fts,
                before,
                PAGE_SIZE as i64 + 1
            ],
            row,
        )?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let more = rows.len() > PAGE_SIZE;
    rows.truncate(PAGE_SIZE);
    let sources = db
        .prepare("SELECT source, count(*) FROM games GROUP BY source ORDER BY source")?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(Page {
        rows,
        total,
        all,
        sources,
        more,
    })
}

pub fn load(db: &Connection, id: i64) -> Result<Loaded> {
    let (body, notes, starred): (String, String, bool) = db.query_row(
        "SELECT body,notes,starred FROM games WHERE id=?",
        [id],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?;
    Ok(Loaded {
        id,
        game: serde_json::from_str(&body)?,
        notes: serde_json::from_str(&notes)?,
        starred,
    })
}
/// Games added after `after` with no White, Black, Event or year ("?", "????.??.??" or blank).
pub fn missing_details(db: &Connection, after: i64) -> Result<Vec<i64>> {
    Ok(db
        .prepare("SELECT id FROM games WHERE id > ?1 AND (trim(white, ' ?') = '' OR trim(black, ' ?') = '' OR trim(event, ' ?') = '' OR date = '' OR date LIKE '?%') ORDER BY id")?
        .query_map([after], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?)
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
    anyhow::ensure!(
        db.execute(
            "UPDATE games SET notes=?1,starred=?2,search_text=?3,body=?5,white=?6,black=?7,date=?8,event=?9 WHERE id=?4",
            params![
                serde_json::to_string(&loaded.notes)?,
                loaded.starred,
                search_text(&loaded.game, &loaded.notes),
                loaded.id,
                serde_json::to_string(&loaded.game)?,
                loaded.game.tag("White"),
                loaded.game.tag("Black"),
                loaded.game.tag("Date"),
                loaded.game.tag("Event")
            ]
        )? == 1,
        "Game no longer exists"
    );
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
                let inserted = tx.execute("INSERT OR IGNORE INTO games(hash,white,black,event,date,result,eco,source,body,search_text) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)", params![hash,game.tag("White"),game.tag("Black"),game.tag("Event"),game.tag("Date"),game.tag("Result"),game.tag("ECO"),source,body,search_text(&game,&Notes::new())])?;
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
        let mut stmt = db.prepare(&format!(
            "SELECT body,notes FROM games {} AND (?4 IS NULL OR id=?4) ORDER BY id",
            filter.clause()
        ))?;
        let mut rows = stmt.query(params![filter.source, filter.starred, filter.fts(), id])?;
        let mut count = 0;
        while let Some(row) = rows.next()? {
            let game: Game = serde_json::from_str(&row.get::<_, String>(0)?)?;
            let notes: Notes = serde_json::from_str(&row.get::<_, String>(1)?)?;
            out.write_all(game.export(&notes)?.as_bytes())?;
            count += 1;
        }
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
            }],
            milliseconds_per_position: 500,
        });
        save(&db, &loaded).unwrap();
        drop(db);
        let mut db = open(&path).unwrap();
        let stats = import(&mut db, &[pgn], &AtomicBool::new(false), |_| {}).unwrap();
        assert_eq!(stats.duplicates, 1);
        let reopened = load(&db, 1).unwrap();
        assert_eq!(reopened.notes[&1], "Existing note");
        assert_eq!(reopened.notes[&branch], "Branch preparation");
        assert_eq!(
            reopened.game.review.as_ref().unwrap().accuracy(true),
            Some(98.0)
        );
        assert_eq!(reopened.game.replay().unwrap().nodes[branch].san, "Nc3");
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
