//! The command line: every library operation except engine analysis and play, printed as JSON for
//! scripts and agents. `chesslib help` lists the commands.
use crate::{
    library::{self, Filter, Folder, Loaded},
    pgn::{Game, Replay},
};
use anyhow::{Context, Result, bail, ensure};
use rusqlite::Connection;
use serde_json::{Map, Value, json};
use shakmaty::{EnPassantMode, Square, fen::Fen, san::SanPlus};
use std::{
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::atomic::AtomicBool,
};

const USAGE: &str = "\
ChessLib command line (run with no arguments for the app). Every command prints JSON, except
`games export` without --out, which prints PGN. Games and folders are named by numeric id.

  chesslib library                          the library file, its game count and source files
  chesslib library use PATH|none            open this library (a file, or a folder holding library.sqlite3)
                                            in later commands; none goes back to the app's library
  chesslib games list [FILTERS] [--before ID]
                                            80 games, newest first; pass the returned next as --before for more
  chesslib games show ID                    headers, tags, folder, PGN, and every position (node) with its
                                            move, FEN, note, mark, tags, bookmark and drawings
  chesslib games import FILE|-... [--to FOLDER]
                                            PGN files, or - for PGN or a FEN on standard input
  chesslib games export [ID] [FILTERS] [--out PATH]
  chesslib games delete ID...
  chesslib games star|unstar ID...
  chesslib games set ID Key=Value...        PGN headers: Title, White, Black, Event, Date, Result, ECO, ...;
                                            an empty value resets it to unknown, or removes it
  chesslib games move ID... --to FOLDER|none
  chesslib games move --to FOLDER|none FILTERS|--all
                                            put games in a folder; none takes them out of folders
  chesslib games note ID NODE TEXT...       \"\" as TEXT removes the note
  chesslib games mark ID NODE !!|!|!?|?!|?|??|none
  chesslib games bookmark|unbookmark ID NODE
  chesslib games draw|undraw ID NODE SHAPE...
                                            SHAPE: arrow e2e4 or circle e4, optionally led by a colour
                                            G, R, B or Y (green by default), as games show lists them
  chesslib games play ID NODE SAN...        add moves after NODE; moves already there are reused
  chesslib games promote ID NODE            make a variation the continuation of its line
  chesslib games delete-line ID NODE        remove an added line from NODE on; later added nodes renumber
  chesslib folders list                     game counts include subfolders
  chesslib folders create NAME [--parent FOLDER]
  chesslib folders rename FOLDER NAME
  chesslib folders delete FOLDER            its games and subfolders move up one level
  chesslib tags list [--folder FOLDER]
  chesslib tags add|remove ID NAME... [--node NODE]
                                            tag the game, or with --node one of its positions
  chesslib events list

FILTERS: --query WORDS --source FILE --starred --event NAME --folder FOLDER (and its subfolders) --tag NAME
NODE 0 is the starting position; games show numbers the rest. Put -- before text that starts with --.
--db PATH picks another library; otherwise CHESSLIB_DB, then the one set with `library use`, then the
app's library folder, then ~/Library/Application Support/ChessLib/library.sqlite3. A game open in the app keeps the app's copy:
its next save there replaces command-line edits to that game.";

/// Move marks by NAG, as the app shows them; "none" (0) hides an imported mark.
const MARKS: [&str; 7] = ["none", "!", "?", "!!", "??", "!?", "?!"];
/// Flags that take no value.
const SWITCHES: [&str; 3] = ["starred", "all", "help"];

#[derive(Default)]
struct Args {
    words: Vec<String>,
    flags: Vec<(String, String)>,
}

fn parse(raw: Vec<String>) -> Result<Args> {
    let mut args = Args::default();
    let mut raw = raw.into_iter();
    while let Some(arg) = raw.next() {
        match arg.strip_prefix("--") {
            Some("") => {
                args.words.extend(raw);
                break;
            }
            Some(name) if SWITCHES.contains(&name) => args.flags.push((name.into(), String::new())),
            Some(name) => {
                let value = raw.next().with_context(|| format!("--{name} needs a value"))?;
                args.flags.push((name.into(), value));
            }
            None => args.words.push(arg),
        }
    }
    Ok(args)
}

impl Args {
    fn flag(&mut self, name: &str) -> Option<String> {
        let i = self.flags.iter().position(|(k, _)| k == name)?;
        Some(self.flags.remove(i).1)
    }
    fn switch(&mut self, name: &str) -> bool {
        self.flag(name).is_some()
    }
    fn filter(&mut self) -> Result<Filter> {
        Ok(Filter {
            query: self.flag("query").unwrap_or_default(),
            source: self.flag("source").unwrap_or_default(),
            starred: self.switch("starred"),
            event: self.flag("event"),
            folder: self.flag("folder").map(|f| number(&f)).transpose()?,
            tag: self.flag("tag"),
        })
    }
    /// Refuses flags the command did not read; each command calls it before changing anything.
    fn done(&self) -> Result<()> {
        match self.flags.first() {
            Some((name, _)) => bail!("--{name} is not an option of this command, or was given twice; see `chesslib help`"),
            None => Ok(()),
        }
    }
}

pub fn run(raw: Vec<String>) -> Result<()> {
    let mut args = parse(raw)?;
    if args.switch("help") || matches!(args.words.first().map(String::as_str), None | Some("help" | "-h")) {
        println!("{USAGE}");
        return Ok(());
    }
    // `library use PATH|none` remembers or forgets the library later commands open, then reports it as `library` does.
    let using = match &args.words[..] {
        [l, u, target] if l == "library" && u == "use" => Some(target.clone()),
        _ => None,
    };
    if using.is_some() {
        args.done()?;
        ensure!(std::env::var_os("CHESSLIB_DB").is_none(), "CHESSLIB_DB is set and would override `library use`; unset it first");
        args.words.truncate(1);
    }
    let memory = crate::support_dir().join("cli-library");
    let path = match using.as_deref() {
        Some("none") => crate::library_path(),
        Some(target) => std::path::absolute(target)?,
        None => args
            .flag("db")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("CHESSLIB_DB").map(PathBuf::from))
            .or_else(|| std::fs::read_to_string(&memory).ok().map(|p| PathBuf::from(p.trim_end_matches('\n'))))
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or_else(crate::library_path),
    };
    let path = if path.is_dir() { path.join(crate::app::LIBRARY_FILE) } else { path };
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut db = library::open(&path).with_context(|| format!("Opening {}", path.display()))?;
    if let Some(target) = using {
        // Written once the library opened, so a path that cannot hold one is never remembered.
        std::fs::create_dir_all(crate::support_dir())?;
        std::fs::write(&memory, if target == "none" { String::new() } else { format!("{}\n", path.display()) })?;
    }
    let out = command(&mut db, &path, &mut args)?;
    if !out.is_null() {
        println!("{out}");
    }
    Ok(())
}

fn command(db: &mut Connection, path: &Path, args: &mut Args) -> Result<Value> {
    let words = std::mem::take(&mut args.words);
    let words: Vec<&str> = words.iter().map(String::as_str).collect();
    Ok(match words.as_slice() {
        ["library"] => {
            args.done()?;
            let page = library::search(db, &Filter::default(), 0)?;
            let sources: Vec<_> = page.sources.iter().map(|(name, games)| json!({"name": name, "games": games})).collect();
            json!({"path": path, "games": page.all, "sources": sources})
        }
        ["games"] | ["games", "list"] => {
            let filter = args.filter()?;
            let before = args.flag("before").map(|b| number(&b)).transpose()?.unwrap_or(i64::MAX);
            args.done()?;
            let page = library::search(db, &filter, before)?;
            let games: Vec<_> = page
                .rows
                .iter()
                .map(|r| json!({"id": r.id, "title": r.title, "white": r.white, "black": r.black, "event": r.event, "date": r.date, "result": r.result, "eco": r.eco, "starred": r.starred, "tags": r.labels}))
                .collect();
            let next = page.rows.last().filter(|_| page.more).map(|r| r.id);
            json!({"total": page.total, "games": games, "next": next})
        }
        ["games", "show", g] => {
            let id = game(db, g)?;
            args.done()?;
            show(db, id)?
        }
        ["games", "import", files @ ..] => {
            let to = args.flag("to").map(|f| folder(db, &f)).transpose()?.flatten();
            args.done()?;
            ensure!(!files.is_empty(), "Name PGN files to import, or - for standard input");
            ensure!(files.iter().filter(|&&f| f == "-").count() <= 1, "Standard input can be read only once");
            let staged = std::env::temp_dir().join(format!("chesslib-cli-{}", std::process::id()));
            let mut paths = Vec::new();
            for file in files {
                if *file == "-" {
                    let mut text = String::new();
                    std::io::stdin().read_to_string(&mut text)?;
                    // The importer reads files, so stage standard input in one, as the app does for pastes.
                    std::fs::create_dir_all(&staged)?;
                    paths.push(staged.join("stdin.pgn"));
                    std::fs::write(paths.last().unwrap(), crate::app::pasted_pgn(text))?;
                } else {
                    paths.push(PathBuf::from(*file));
                }
            }
            let floor = library::last_id(db)?;
            let result = library::import(db, &paths, &AtomicBool::new(false), |_| {});
            let _ = std::fs::remove_dir_all(&staged);
            // A failed file keeps the batches saved before it, so file those too.
            if let Some(to) = to {
                library::file_imported(db, floor, to)?;
            }
            let stats = result?;
            let ids: Vec<i64> = db
                .prepare("SELECT id FROM games WHERE id > ? ORDER BY id")?
                .query_map([floor], |r| r.get(0))?
                .collect::<rusqlite::Result<_>>()?;
            json!({"read": stats.read, "imported": stats.imported, "duplicates": stats.duplicates, "ids": ids})
        }
        ["games", "export", rest @ ..] => {
            let one = match rest {
                [] => None,
                [g] => Some(game(db, g)?),
                _ => bail!("Export one game by id, or every game matching the filters"),
            };
            let filter = args.filter()?;
            let out = args.flag("out");
            args.done()?;
            match out {
                Some(out) => json!({"exported": library::export(db, &filter, one, Path::new(&out))?, "path": out}),
                None => {
                    let mut stdout = std::io::stdout().lock();
                    library::write_pgn(db, &filter, one, &mut stdout)?;
                    stdout.flush()?;
                    Value::Null
                }
            }
        }
        ["games", "delete", list @ ..] => {
            let ids = games(db, list)?;
            args.done()?;
            for &id in &ids {
                library::delete(db, id)?;
            }
            json!({"deleted": ids})
        }
        ["games", verb @ ("star" | "unstar"), list @ ..] => {
            let ids = games(db, list)?;
            args.done()?;
            for &id in &ids {
                edit(db, id, |l| {
                    l.starred = *verb == "star";
                    Ok(())
                })?;
            }
            json!({"ok": true})
        }
        ["games", "set", g, pairs @ ..] => {
            let id = game(db, g)?;
            args.done()?;
            ensure!(!pairs.is_empty(), "Give headers as Key=Value");
            edit(db, id, |l| {
                for pair in pairs {
                    let (key, value) = pair.split_once('=').with_context(|| format!("{pair:?} is not Key=Value"))?;
                    ensure!(!key.is_empty() && key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'), "{key:?} is not a PGN header name");
                    ensure!(!matches!(key, "FEN" | "SetUp" | "Variant"), "{key} sets the starting position, which would break the game's moves");
                    l.game.set_header(key, value);
                }
                Ok(())
            })?;
            json!({"headers": headers(&load(db, id)?.game)})
        }
        ["games", "move", list @ ..] => {
            let to = folder(db, &args.flag("to").context("Say where with --to FOLDER, or --to none")?)?;
            let moved = if list.is_empty() {
                let all = args.switch("all");
                let filter = args.filter()?;
                args.done()?;
                let narrowed = !filter.query.is_empty() || !filter.source.is_empty() || filter.starred || filter.event.is_some() || filter.folder.is_some() || filter.tag.is_some();
                ensure!(narrowed || all, "Name games by id, filter them, or pass --all to move every game");
                library::move_games(db, &filter, None, to)?
            } else {
                let ids = games(db, list)?;
                args.done()?;
                ids.iter().map(|&id| library::move_games(db, &Filter::default(), Some(id), to)).sum::<Result<usize>>()?
            };
            json!({"moved": moved})
        }
        ["games", "note", g, n, text @ ..] => {
            let id = game(db, g)?;
            args.done()?;
            ensure!(!text.is_empty(), "Give the note's text, or \"\" to remove the note");
            let text = text.join(" ");
            edit(db, id, |l| {
                let (_, n) = node(&l.game, n)?;
                if text.trim().is_empty() {
                    l.notes.remove(&n);
                } else {
                    l.notes.insert(n, text);
                }
                Ok(())
            })?;
            json!({"ok": true})
        }
        ["games", "mark", g, n, mark] => {
            let id = game(db, g)?;
            args.done()?;
            let nag = MARKS.iter().position(|m| m == mark).with_context(|| format!("A mark is one of {}", MARKS.join(" ")))?;
            edit(db, id, |l| {
                let (_, n) = node(&l.game, n)?;
                l.game.glyphs.insert(n, nag as u8);
                Ok(())
            })?;
            json!({"ok": true})
        }
        ["games", verb @ ("bookmark" | "unbookmark"), g, n] => {
            let id = game(db, g)?;
            args.done()?;
            edit(db, id, |l| {
                let (_, n) = node(&l.game, n)?;
                if *verb == "bookmark" {
                    l.game.bookmarks.insert(n);
                } else {
                    l.game.bookmarks.remove(&n);
                }
                Ok(())
            })?;
            json!({"ok": true})
        }
        ["games", verb @ ("draw" | "undraw"), g, n, list @ ..] => {
            let id = game(db, g)?;
            args.done()?;
            ensure!(!list.is_empty(), "Give shapes such as e2e4 (an arrow) or Re4 (a red circle)");
            let shapes = list.iter().map(|s| shape(s)).collect::<Result<Vec<_>>>()?;
            edit(db, id, |l| {
                let (_, n) = node(&l.game, n)?;
                let drawings = l.game.drawings.entry(n).or_default();
                // As on the board, a square pair holds one shape; drawing it again in a new colour replaces it.
                for (from, to, brush) in shapes {
                    drawings.retain(|d| (d.0, d.1) != (from, to));
                    if *verb == "draw" {
                        drawings.push((from, to, brush));
                    }
                }
                if drawings.is_empty() {
                    l.game.drawings.remove(&n);
                }
                Ok(())
            })?;
            json!({"ok": true})
        }
        ["games", "play", g, n, moves @ ..] => {
            let id = game(db, g)?;
            args.done()?;
            ensure!(!moves.is_empty(), "Give the moves to add in SAN, such as e4 Nf3");
            let added = edit(db, id, |l| {
                let (mut replay, mut at) = node(&l.game, n)?;
                let mut added = Vec::new();
                for san in moves {
                    let position = replay.nodes[at].position.as_ref().with_context(|| format!("Node {at} follows an illegal move and cannot be played from"))?;
                    let movement = san
                        .parse::<SanPlus>()
                        .ok()
                        .and_then(|s| s.san.to_move(position).ok())
                        .with_context(|| format!("{san} is not a legal move at node {at}"))?;
                    at = l.game.play(&mut replay, at, movement)?;
                    added.push(json!({"node": at, "move": replay.label(at).replace('…', "...")}));
                }
                Ok(added)
            })?;
            json!({"moves": added})
        }
        ["games", "promote", g, n] => {
            let id = game(db, g)?;
            args.done()?;
            edit(db, id, |l| {
                let (replay, n) = node(&l.game, n)?;
                ensure!(l.game.promote(&replay, n), "Node {n} cannot be promoted: it already continues its line, the game's own move comes first there, or it is illegal");
                Ok(())
            })?;
            json!({"ok": true})
        }
        ["games", "delete-line", g, n] => {
            let id = game(db, g)?;
            args.done()?;
            edit(db, id, |l| {
                let (replay, n) = node(&l.game, n)?;
                ensure!(l.game.delete_line(&replay, &mut l.notes, n), "Node {n} came with the imported game; only lines you added can be deleted");
                Ok(())
            })?;
            json!({"ok": true})
        }
        ["folders"] | ["folders", "list"] => {
            args.done()?;
            let folders = library::folders(db)?;
            folders
                .iter()
                .map(|f| json!({"id": f.id, "parent": f.parent, "name": f.name, "games": f.games, "path": folder_path(&folders, f.id)}))
                .collect()
        }
        ["folders", "create", name] => {
            let parent = args.flag("parent").map(|p| folder(db, &p)).transpose()?.flatten();
            args.done()?;
            ensure!(!name.trim().is_empty(), "A folder needs a name");
            json!({"id": library::create_folder(db, parent, name.trim())?})
        }
        ["folders", "rename", f, name] => {
            let id = folder(db, f)?.context("Name the folder by id")?;
            args.done()?;
            ensure!(!name.trim().is_empty(), "A folder needs a name");
            library::rename_folder(db, id, name.trim())?;
            json!({"ok": true})
        }
        ["folders", "delete", f] => {
            let id = folder(db, f)?.context("Name the folder by id")?;
            args.done()?;
            library::delete_folder(db, id)?;
            json!({"ok": true})
        }
        ["tags"] | ["tags", "list"] => {
            let folder = args.flag("folder").map(|f| folder(db, &f)).transpose()?.flatten();
            args.done()?;
            let page = library::search(db, &Filter { folder, ..Default::default() }, 0)?;
            page.labels.iter().map(|(name, games)| json!({"name": name, "games": games})).collect()
        }
        ["tags", verb @ ("add" | "remove"), g, names @ ..] => {
            let id = game(db, g)?;
            let at = args.flag("node");
            args.done()?;
            ensure!(!names.is_empty(), "Name the tags");
            ensure!(names.iter().all(|n| !n.trim().is_empty()), "A tag needs a name");
            let known: Vec<String> = db
                .prepare("SELECT DISTINCT name FROM labels")?
                .query_map([], |r| r.get(0))?
                .collect::<rusqlite::Result<_>>()?;
            edit(db, id, |l| {
                let tags = match &at {
                    None => &mut l.game.labels,
                    Some(n) => {
                        let (_, n) = node(&l.game, n)?;
                        ensure!(n > 0, "Node 0 is the starting position; tag the game itself without --node");
                        l.game.position_labels.entry(n).or_default()
                    }
                };
                for name in names {
                    // Reuse an existing tag's spelling so "endgame" joins "Endgame", as the app does.
                    let name = name.trim();
                    let name = known.iter().find(|k| k.to_lowercase() == name.to_lowercase()).map_or(name, String::as_str);
                    if *verb == "add" {
                        tags.insert(name.into());
                    } else {
                        tags.remove(name);
                    }
                }
                l.game.position_labels.retain(|_, tags| !tags.is_empty());
                Ok(())
            })?;
            json!({"ok": true})
        }
        ["events"] | ["events", "list"] => {
            args.done()?;
            library::events(db)?
                .iter()
                .map(|e| json!({"name": e.name, "games": e.games, "first": e.first, "last": e.last}))
                .collect()
        }
        _ => bail!("Unknown command `{}`; run `chesslib help`", words.join(" ")),
    })
}

fn number(s: &str) -> Result<i64> {
    s.parse().with_context(|| format!("Expected a numeric id, not {s:?}"))
}
/// A game id that exists.
fn game(db: &Connection, s: &str) -> Result<i64> {
    let id = number(s)?;
    ensure!(db.query_row("SELECT count(*) FROM games WHERE id=?", [id], |r| r.get::<_, i64>(0))? == 1, "No game {id}");
    Ok(id)
}
fn games(db: &Connection, list: &[&str]) -> Result<Vec<i64>> {
    ensure!(!list.is_empty(), "Name the games by id");
    list.iter().map(|g| game(db, g)).collect()
}
/// A folder id that exists, or None for "none".
fn folder(db: &Connection, s: &str) -> Result<Option<i64>> {
    if s == "none" {
        return Ok(None);
    }
    let id = number(s)?;
    ensure!(db.query_row("SELECT count(*) FROM folders WHERE id=?", [id], |r| r.get::<_, i64>(0))? == 1, "No folder {id}");
    Ok(Some(id))
}
fn folder_path(folders: &[Folder], mut id: i64) -> Vec<String> {
    let mut path = Vec::new();
    while let Some(folder) = folders.iter().find(|f| f.id == id) {
        path.insert(0, folder.name.clone());
        match folder.parent {
            Some(parent) => id = parent,
            None => break,
        }
    }
    path
}
fn load(db: &Connection, id: i64) -> Result<Loaded> {
    library::load(db, id).with_context(|| format!("Reading game {id}"))
}
/// Loads a game, changes it, and saves it; a failed change saves nothing.
fn edit<T>(db: &Connection, id: i64, change: impl FnOnce(&mut Loaded) -> Result<T>) -> Result<T> {
    let mut loaded = load(db, id)?;
    let out = change(&mut loaded)?;
    library::save(db, &loaded)?;
    Ok(out)
}
/// The game's moves and a node in them.
fn node(game: &Game, s: &str) -> Result<(Replay, usize)> {
    let replay = game.replay()?;
    let n: usize = s.parse().with_context(|| format!("Expected a node number, not {s:?}"))?;
    ensure!(n < replay.nodes.len(), "The game has no node {n}; games show lists them");
    Ok((replay, n))
}
fn headers(game: &Game) -> Map<String, Value> {
    game.tags.iter().map(|(k, v)| (k.clone(), v.as_str().into())).collect()
}
/// Lichess's shape notation: brush, then the arrow's two squares or the circle's one.
fn shape_name(&(from, to, brush): &(u8, u8, char)) -> String {
    let (from, to) = (Square::new(from.into()), Square::new(to.into()));
    if from == to { format!("{brush}{from}") } else { format!("{brush}{from}{to}") }
}
fn shape(s: &str) -> Result<(u8, u8, char)> {
    let (brush, squares) = match s.chars().next() {
        Some(c @ ('G' | 'R' | 'B' | 'Y')) => (c, &s[1..]),
        _ => ('G', s),
    };
    let square = |i: std::ops::Range<usize>| squares.get(i).and_then(|t: &str| t.parse::<Square>().ok()).map(u8::from);
    let parsed = match squares.len() {
        2 => square(0..2).map(|sq| (sq, sq)),
        4 => square(0..2).zip(square(2..4)),
        _ => None,
    };
    let (from, to) = parsed.with_context(|| format!("{s:?} is not a shape such as e2e4, e4 or Re2e4"))?;
    Ok((from, to, brush))
}

fn show(db: &Connection, id: i64) -> Result<Value> {
    let loaded = load(db, id)?;
    let (folder, source): (Option<i64>, String) =
        db.query_row("SELECT folder, source FROM games WHERE id=?", [id], |r| Ok((r.get(0)?, r.get(1)?)))?;
    let folders = library::folders(db)?;
    let game = &loaded.game;
    let mut out = json!({
        "id": id,
        "starred": loaded.starred,
        "folder": folder.map(|f| json!({"id": f, "path": folder_path(&folders, f)})),
        "source": source,
        "headers": headers(game),
        "tags": game.labels,
        "pgn": game.export(&loaded.notes).ok(),
    });
    match game.replay() {
        Ok(replay) => {
            let added = game.first_added(&replay);
            let moves: Vec<Value> = replay
                .nodes
                .iter()
                .enumerate()
                .map(|(i, n)| {
                    let mut m = Map::new();
                    m.insert("node".into(), i.into());
                    if let Some(parent) = n.parent {
                        m.insert("parent".into(), parent.into());
                        m.insert("move".into(), replay.label(i).replace('…', "...").into());
                        let line = if replay.in_game(i) { "game" } else if i >= added { "added" } else { "variation" };
                        m.insert("line".into(), line.into());
                    }
                    // Where the line branches, the first child continues it.
                    if n.children.len() > 1 {
                        m.insert("children".into(), json!(n.children));
                    }
                    if let Some(position) = &n.position {
                        m.insert("fen".into(), Fen::from_position(position, EnPassantMode::Legal).to_string().into());
                    }
                    if let Some(note) = loaded.notes.get(&i).filter(|n| !n.is_empty()) {
                        m.insert("note".into(), note.as_str().into());
                    }
                    if let Some(mark) = replay.glyph(&game.glyphs, i).and_then(|nag| MARKS.get(nag as usize)) {
                        m.insert("mark".into(), (*mark).into());
                    }
                    if let Some(tags) = game.position_labels.get(&i) {
                        m.insert("tags".into(), json!(tags));
                    }
                    if game.bookmarks.contains(&i) {
                        m.insert("bookmarked".into(), true.into());
                    }
                    if let Some(drawings) = game.drawings.get(&i) {
                        m.insert("drawings".into(), drawings.iter().map(shape_name).collect());
                    }
                    Value::Object(m)
                })
                .collect();
            out["moves"] = moves.into();
            if let Some(warning) = replay.warning {
                out["warning"] = warning.into();
            }
        }
        Err(e) => out["warning"] = e.to_string().into(),
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn cli(db: &mut Connection, line: &str) -> Result<Value> {
        command(db, Path::new("test"), &mut parse(line.split_whitespace().map(String::from).collect())?)
    }
    #[test]
    fn commands_cover_games_folders_tags_and_positions() {
        let dir = tempfile::tempdir().unwrap();
        let mut db = library::open(&dir.path().join("db")).unwrap();
        let db = &mut db;
        let pgn = dir.path().join("course.pgn");
        std::fs::write(&pgn, "[White \"Tal\"]\n[Black \"Fischer\"]\n\n1. e4 e5 2. Nf3 *\n\n[White \"Carlsen\"]\n\n1. d4 *").unwrap();
        let openings = cli(db, "folders create Openings").unwrap()["id"].as_i64().unwrap();
        let sub = cli(db, &format!("folders create Sicilian --parent {openings}")).unwrap()["id"].as_i64().unwrap();
        let imported = cli(db, &format!("games import {} --to {sub}", pgn.display())).unwrap();
        assert_eq!(imported["ids"], json!([1, 2]));
        assert_eq!(cli(db, &format!("games import {}", pgn.display())).unwrap()["duplicates"], 2);
        assert_eq!(cli(db, &format!("games list --folder {openings}")).unwrap()["total"], 2, "a folder lists its subfolders' games");
        cli(db, &format!("folders rename {sub} Najdorf")).unwrap();
        assert_eq!(cli(db, "games show 1").unwrap()["folder"]["path"], json!(["Openings", "Najdorf"]));

        // Unknown or misplaced flags fail before anything changes.
        assert!(cli(db, "games delete 1 --folder 2").is_err());
        assert!(cli(db, "games list --folders 2").is_err());
        assert!(cli(db, "games move --to none").is_err(), "moving every game needs --all");
        assert!(cli(db, "games show 99").is_err());
        assert_eq!(cli(db, "games list").unwrap()["total"], 2);

        cli(db, "games set 1 Result=1-0 Date=2024-01-02 Title=Bird_trap Event=").unwrap();
        let row = &cli(db, "games list --query Tal").unwrap()["games"][0];
        assert_eq!((&row["result"], &row["date"], &row["event"], &row["title"]), (&json!("1-0"), &json!("2024.01.02"), &json!("?"), &json!("Bird_trap")));
        cli(db, "games set 1 Title=").unwrap();
        assert!(cli(db, "games show 1").unwrap()["headers"].get("Title").is_none());
        assert!(cli(db, "games set 1 FEN=8/8/8/8/8/8/8/K6k").is_err());

        cli(db, "tags add 1 Attack").unwrap();
        cli(db, "tags add 2 attack Endgame --node 1").unwrap();
        assert_eq!(cli(db, "tags list").unwrap(), json!([{"name": "Attack", "games": 2}, {"name": "Endgame", "games": 1}]));
        cli(db, "tags remove 2 ATTACK Endgame --node 1").unwrap();
        assert_eq!(cli(db, "games list --tag Attack").unwrap()["total"], 1);
        assert!(cli(db, "tags add 1 X --node 0").is_err());

        // Node 1 is 1. e4; c5 becomes a variation beside the game's 1... e5.
        let played = cli(db, "games play 1 1 c5 Nf3").unwrap();
        assert_eq!(played["moves"], json!([{"node": 4, "move": "1... c5"}, {"node": 5, "move": "2. Nf3"}]));
        assert_eq!(cli(db, "games play 1 0 e4 e5").unwrap()["moves"][1]["node"], 2, "moves already there are reused");
        assert!(cli(db, "games play 1 1 Ke3").is_err());
        cli(db, "games note 1 4 The Sicilian").unwrap();
        cli(db, "games mark 1 4 !?").unwrap();
        cli(db, "games bookmark 1 4").unwrap();
        cli(db, "games draw 1 4 Rc7c5 e4").unwrap();
        cli(db, "games draw 1 4 Bc7c5").unwrap();
        let shown = cli(db, "games show 1").unwrap();
        let c5 = &shown["moves"][4];
        assert_eq!((&c5["line"], &c5["note"], &c5["mark"], &c5["bookmarked"]), (&json!("added"), &json!("The Sicilian"), &json!("!?"), &json!(true)));
        assert_eq!(c5["drawings"], json!(["Ge4", "Bc7c5"]));
        assert_eq!(shown["moves"][1]["children"], json!([2, 4]));
        assert!(shown["pgn"].as_str().unwrap().contains("The Sicilian"));
        cli(db, "games undraw 1 4 c7c5 e4").unwrap();
        assert!(cli(db, "games show 1").unwrap()["moves"][4].get("drawings").is_none());
        assert!(cli(db, "games promote 1 4").is_err(), "the game's own 1... e5 stays first");
        assert!(cli(db, "games delete-line 1 2").is_err(), "imported moves stay");
        cli(db, "games delete-line 1 4").unwrap();
        assert_eq!(cli(db, "games show 1").unwrap()["moves"].as_array().unwrap().len(), 4);

        cli(db, "games star 1 2").unwrap();
        cli(db, "games unstar 2").unwrap();
        assert_eq!(cli(db, "games list --starred").unwrap()["total"], 1);
        assert_eq!(cli(db, "games move --to none --starred").unwrap()["moved"], 1);
        assert_eq!(cli(db, &format!("games move 1 --to {openings}")).unwrap()["moved"], 1);
        let out = dir.path().join("out.pgn");
        assert_eq!(cli(db, &format!("games export --folder {openings} --out {}", out.display())).unwrap()["exported"], 2, "both games are under Openings");
        assert!(std::fs::read_to_string(&out).unwrap().contains("[Result \"1-0\"]"));
        cli(db, &format!("folders delete {openings}")).unwrap();
        assert_eq!(cli(db, "folders").unwrap(), json!([{"id": sub, "parent": null, "name": "Najdorf", "games": 1, "path": ["Najdorf"]}]));
        assert_eq!(cli(db, "events").unwrap().as_array().unwrap().len(), 2);
        assert_eq!(cli(db, "games delete 1 2").unwrap()["deleted"], json!([1, 2]));
        assert_eq!(cli(db, "library").unwrap()["games"], 0);
    }
}
