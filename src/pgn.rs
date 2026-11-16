use anyhow::{Context, Result};
use pgn_reader::{Nag, Outcome, RawComment, RawTag, SanPlus, Skip, Visitor};
use serde::{Deserialize, Serialize};
use shakmaty::{CastlingMode, Chess, Move, Position, fen::Fen, san::SanPlus as ChessSan};
use std::{collections::BTreeMap, ops::ControlFlow};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Token {
    Move(String),
    Comment(String),
    Nag(u8),
    Open,
    Close,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Game {
    pub tags: Vec<(String, String)>,
    pub tokens: Vec<Token>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub study: Vec<StudyMove>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review: Option<crate::engine::Report>,
    /// Move nodes the reader bookmarked by double-clicking them in the scoresheet.
    #[serde(default, skip_serializing_if = "std::collections::BTreeSet::is_empty")]
    pub bookmarks: std::collections::BTreeSet<usize>,
    /// Arrows and circles drawn on each move's board: from and to square indices (equal for a circle) and a brush, G, R, B or Y.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub drawings: BTreeMap<usize, Vec<(u8, u8, char)>>,
    /// Move marks the reader set, as NAGs 1–6 (! ? !! ?? !? ?!); 0 hides an imported mark.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub glyphs: BTreeMap<usize, u8>,
    /// The reader flipped the board to see this game from Black's side.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub flipped: bool,
    /// Side lines the reader promoted: parent node to the child that now continues it.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub promoted: BTreeMap<usize, usize>,
    /// The reader's tags on the whole game, and on single positions by move node.
    #[serde(default, skip_serializing_if = "std::collections::BTreeSet::is_empty")]
    pub labels: std::collections::BTreeSet<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub position_labels: BTreeMap<usize, std::collections::BTreeSet<String>>,
    #[serde(skip)]
    bytes: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StudyMove {
    pub parent: usize,
    pub san: String,
}

pub type Notes = BTreeMap<usize, String>;

impl Game {
    pub fn tag(&self, key: &str) -> &str {
        self.tags
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
            .unwrap_or("")
    }

    pub fn set_tag(&mut self, key: &str, value: String) {
        match self.tags.iter_mut().find(|(k, _)| k == key) {
            Some((_, v)) => *v = value,
            None => self.tags.push((key.into(), value)),
        }
    }

    /// Sets a header as the game editor does: dates take dots, and a blank value leaves PGN's
    /// unknown spelling for White, Black, Event, Date and Result, or removes any other header.
    pub fn set_header(&mut self, key: &str, value: &str) {
        let value = value.trim();
        let value = if key == "Date" { value.replace(['-', '/'], ".") } else { value.to_owned() };
        let unknown = match key {
            "White" | "Black" | "Event" => "?",
            "Date" => "????.??.??",
            "Result" => "*",
            _ => "",
        };
        match (value.is_empty(), unknown.is_empty()) {
            (true, true) => self.tags.retain(|(k, _)| k != key),
            (true, false) => self.set_tag(key, unknown.into()),
            _ => self.set_tag(key, value),
        }
    }

    pub fn initial(&self) -> Result<Chess> {
        anyhow::ensure!(
            matches!(self.tag("Variant"), "" | "Standard" | "Chess" | "Normal"),
            "Unsupported variant: {}",
            self.tag("Variant")
        );
        if self.tag("FEN").is_empty() {
            Ok(Chess::default())
        } else {
            Ok(self
                .tag("FEN")
                .parse::<Fen>()?
                .into_position(CastlingMode::Standard)?)
        }
    }

    pub fn replay(&self) -> Result<Replay> {
        let initial = self.initial()?;
        let mut depth = 0usize;
        let mut main_count = 0;
        for token in &self.tokens {
            match token {
                Token::Open => depth += 1,
                Token::Close => depth = depth.saturating_sub(1),
                Token::Move(_) if depth == 0 => main_count += 1,
                _ => {}
            }
        }
        // Main-line IDs come first, so they remain the old note indices.
        let root = Node {
            position: Some(initial),
            parent: None,
            children: vec![],
            san: String::new(),
            annotations: vec![],
            ply: 0,
        };
        let mut replay = Replay {
            nodes: vec![root; main_count + 1],
            warning: None,
            game_len: main_count + 1,
        };
        let mut current = 0;
        let mut main_id = 0;
        let mut stack = Vec::new();
        for token in &self.tokens {
            match token {
                Token::Move(san) => {
                    let node = replay.node_after(current, san);
                    if node.position.is_none() && replay.warning.is_none() {
                        replay.warning = Some(format!(
                            "Cannot play {san} in an imported line. Original moves remain available in the PGN export."
                        ));
                    }
                    let id = if stack.is_empty() {
                        main_id += 1;
                        replay.nodes[main_id] = node;
                        main_id
                    } else {
                        replay.nodes.push(node);
                        replay.nodes.len() - 1
                    };
                    replay.nodes[current].children.push(id);
                    current = id;
                }
                Token::Open => {
                    stack.push(current);
                    current = replay.nodes[current].parent.unwrap_or(0);
                }
                Token::Close => {
                    if let Some(parent) = stack.pop() {
                        current = parent;
                    }
                }
                annotation => replay.nodes[current].annotations.push(annotation.clone()),
            }
        }
        for movement in &self.study {
            anyhow::ensure!(
                movement.parent < replay.nodes.len(),
                "Invalid saved variation parent"
            );
            let node = replay.node_after(movement.parent, &movement.san);
            anyhow::ensure!(
                node.position.is_some(),
                "Illegal saved variation move: {}",
                movement.san
            );
            let id = replay.nodes.len();
            replay.nodes[movement.parent].children.push(id);
            replay.nodes.push(node);
        }
        for (&parent, &child) in &self.promoted {
            anyhow::ensure!(
                replay.nodes.get(child).is_some_and(|n| n.parent == Some(parent)),
                "Invalid promoted variation"
            );
            // The move played in the game always stays first.
            if !replay.in_game(replay.nodes[parent].children[0]) {
                let children = &mut replay.nodes[parent].children;
                children.retain(|&c| c != child);
                children.insert(0, child);
            }
        }
        Ok(replay)
    }

    /// The first node ID that belongs to a line the reader added; lower IDs come from the PGN.
    pub fn first_added(&self, replay: &Replay) -> usize {
        replay.nodes.len() - self.study.len()
    }

    /// Makes `id` the continuation of its parent's line, unless the parent's next move was played in the game.
    pub fn promote(&mut self, replay: &Replay, id: usize) -> bool {
        let Some(parent) = replay.nodes[id].parent else { return false };
        let first = replay.nodes[parent].children[0];
        if first == id || replay.in_game(first) || replay.nodes[id].position.is_none() {
            return false;
        }
        self.promoted.insert(parent, id);
        true
    }

    /// Makes `id`'s line the main line and returns `id`'s node afterwards; None when it is already on it or is illegal.
    /// Where a move played in the game is in the way, the game is rewritten: the line becomes the game's moves and the old
    /// ones a side line, nodes renumber with notes, marks, drawings, bookmarks and tags following their moves, and a
    /// whole-game review, which no longer fits, is dropped.
    pub fn set_main_line(&mut self, replay: &Replay, notes: &mut Notes, id: usize) -> Option<usize> {
        let branches = replay.branches_to(id);
        if replay.nodes[id].position.is_none() || branches.is_empty() {
            return None;
        }
        let first_of = |n: usize| replay.nodes[replay.nodes[n].parent.unwrap()].children[0];
        if !branches.iter().any(|&n| replay.in_game(first_of(n))) {
            for n in branches {
                self.promote(replay, n);
            }
            return Some(id);
        }
        // The new main line: the way to `id`, then on through each first move.
        let mut main = vec![id];
        while let Some(parent) = replay.nodes[*main.last().unwrap()].parent {
            main.push(parent);
        }
        main.reverse();
        while let Some(&child) = replay.nodes[*main.last().unwrap()].children.first() {
            main.push(child);
        }
        let base = self.first_added(replay);
        let mut on_main = vec![false; replay.nodes.len()];
        for &n in &main {
            on_main[n] = true;
        }
        // The game's moves and the new main line become the PGN moves, the main line first at every branch.
        let order = |n: usize| {
            let mut kids: Vec<usize> = replay.nodes[n].children.iter().copied().filter(|&c| c < base || on_main[c]).collect();
            kids.sort_by_key(|&c| !on_main[c]);
            kids
        };
        let ids = self.rewrite(replay, notes, &order, |_| true);
        debug_assert_eq!(ids.iter().flatten().count(), replay.nodes.len());
        self.review = None;
        ids[id]
    }

    /// Writes the PGN moves anew, `order` giving each node's children with the continuation first, and keeps the added
    /// moves `order` leaves out where `keep` allows; nodes renumber with what the reader keeps per node. Returns each
    /// old node's new number, None for a dropped one.
    fn rewrite(&mut self, replay: &Replay, notes: &mut Notes, order: &dyn Fn(usize) -> Vec<usize>, keep: impl Fn(usize) -> bool) -> Vec<Option<usize>> {
        let base = self.first_added(replay);
        let mut ids = vec![None; replay.nodes.len()];
        // The parser numbers the main line first.
        let (mut node, mut next) = (0, 1);
        ids[0] = Some(0);
        while let Some(&child) = order(node).first() {
            ids[child] = Some(next);
            next += 1;
            node = child;
        }
        let mut tokens = replay.nodes[0].annotations.clone();
        write_tokens(replay, order, 0, &mut ids, &mut next, &mut tokens);
        let moves_end = next;
        let mut study = Vec::new();
        for (i, movement) in self.study.iter().enumerate() {
            if ids[base + i].is_none() && keep(base + i) {
                ids[base + i] = Some(next);
                next += 1;
                study.push(StudyMove { parent: ids[movement.parent].unwrap(), san: movement.san.clone() });
            }
        }
        self.tokens = tokens;
        self.study = study;
        self.renumber(notes, |old| ids[old]);
        // Orders among the game's moves are now written into them.
        self.promoted.retain(|_, child| *child >= moves_end);
        ids
    }

    /// Moves what the reader keeps per node to the node's new number; `map` gives None for a removed node.
    fn renumber(&mut self, notes: &mut Notes, map: impl Fn(usize) -> Option<usize>) {
        *notes = std::mem::take(notes).into_iter().filter_map(|(k, v)| Some((map(k)?, v))).collect();
        self.drawings = std::mem::take(&mut self.drawings).into_iter().filter_map(|(k, v)| Some((map(k)?, v))).collect();
        self.glyphs = std::mem::take(&mut self.glyphs).into_iter().filter_map(|(k, v)| Some((map(k)?, v))).collect();
        self.bookmarks = std::mem::take(&mut self.bookmarks).into_iter().filter_map(&map).collect();
        self.position_labels = std::mem::take(&mut self.position_labels).into_iter().filter_map(|(k, v)| Some((map(k)?, v))).collect();
        self.promoted = std::mem::take(&mut self.promoted).into_iter().filter_map(|(p, c)| Some((map(p)?, map(c)?))).collect();
    }

    /// Removes `id` and every move after it, with their notes, marks and drawings, and returns its parent's new node;
    /// None for the starting position. Deleting a game move rewrites the game: the next side line there, if any, continues
    /// it, nodes renumber, and a whole-game review, which no longer fits, is dropped. Deleting an added line only shifts
    /// later added moves down.
    pub fn delete_line(&mut self, replay: &Replay, notes: &mut Notes, id: usize) -> Option<usize> {
        let parent = replay.nodes[id].parent?;
        let base = self.first_added(replay);
        if id < base {
            let mut removed = vec![false; replay.nodes.len()];
            let mut stack = vec![id];
            while let Some(n) = stack.pop() {
                removed[n] = true;
                stack.extend(&replay.nodes[n].children);
            }
            if replay.in_game(id) {
                self.review = None;
            }
            let order = |n: usize| replay.nodes[n].children.iter().copied().filter(|&c| c < base && !removed[c]).collect();
            return self.rewrite(replay, notes, &order, |n| !removed[n])[parent];
        }
        let mut removed = std::collections::BTreeSet::from([id]);
        // Study moves come after their parents, so one forward pass collects the whole subtree.
        for (i, movement) in self.study.iter().enumerate() {
            if removed.contains(&movement.parent) {
                removed.insert(base + i);
            }
        }
        let remap = |old: usize| -> Option<usize> {
            if removed.contains(&old) {
                None
            } else if old < base {
                Some(old)
            } else {
                Some(old - removed.range(..old).count())
            }
        };
        let mut i = base;
        self.study.retain(|_| {
            i += 1;
            !removed.contains(&(i - 1))
        });
        for movement in &mut self.study {
            movement.parent = remap(movement.parent).unwrap();
        }
        self.renumber(notes, remap);
        Some(parent)
    }

    pub fn mainline_positions(&self) -> Result<Vec<(usize, Chess)>> {
        let mut original = self.clone();
        original.study.clear();
        original.promoted.clear();
        let replay = original.replay()?;
        let mut positions = Vec::new();
        let mut id = 0;
        loop {
            let node = &replay.nodes[id];
            positions.push((
                id,
                node.position.clone().context(
                    "The main line contains an illegal move; a complete review is unavailable",
                )?,
            ));
            if let Some(&next) = node.children.first() {
                id = next;
            } else {
                break;
            }
        }
        Ok(positions)
    }

    /// A review from the `[%eval]` values the PGN's main line carries, as Lichess exports have; see `engine::pgn_report`.
    pub fn pgn_review(&self) -> Option<crate::engine::Report> {
        if !self.tokens.iter().any(|t| matches!(t, Token::Comment(c) if c.contains("[%eval "))) {
            return None;
        }
        let replay = self.replay().ok()?;
        crate::engine::pgn_report(&self.mainline_positions().ok()?, |n| replay.command(n, "eval").and_then(crate::engine::pgn_eval))
    }

    pub fn play(&mut self, replay: &mut Replay, parent: usize, movement: Move) -> Result<usize> {
        let position = replay
            .nodes
            .get(parent)
            .and_then(|n| n.position.as_ref())
            .context("This position cannot be played")?;
        anyhow::ensure!(position.is_legal(movement), "Illegal move");
        let san = ChessSan::from_move(position.clone(), movement).to_string();
        if let Some(&id) = replay.nodes[parent].children.iter().find(|&&id| {
            replay.nodes[id]
                .san
                .parse::<ChessSan>()
                .ok()
                .and_then(|s| s.san.to_move(position).ok())
                == Some(movement)
        }) {
            return Ok(id);
        }
        let node = replay.node_after(parent, &san);
        let id = replay.nodes.len();
        self.study.push(StudyMove { parent, san });
        replay.nodes[parent].children.push(id);
        replay.nodes.push(node);
        Ok(id)
    }

    pub fn export(&self, notes: &Notes) -> Result<String> {
        self.write(notes, false)
    }

    /// PGN with the reader's own lines written as the game's moves, for a board that has no game of its own.
    pub fn export_as_game(&self, notes: &Notes) -> Result<String> {
        self.write(notes, true)
    }

    fn write(&self, notes: &Notes, as_game: bool) -> Result<String> {
        match self.replay() {
            Ok(mut replay) => {
                if as_game {
                    replay.game_len = replay.nodes.len();
                }
                let mut out = String::new();
                for (name, value) in &self.tags {
                    let value = value
                        .replace('\\', "\\\\")
                        .replace('"', "\\\"")
                        .replace(['\n', '\r'], " ");
                    out.push_str(&format!("[{name} \"{value}\"]\n"));
                }
                out.push('\n');
                replay.write_pgn(notes, &self.glyphs, &mut out);
                out.push_str(if self.tag("Result").is_empty() {
                    "*"
                } else {
                    self.tag("Result")
                });
                out.push_str("\n\n");
                Ok(out)
            }
            Err(error) if !self.study.is_empty() => {
                Err(error.context("Cannot export saved variations"))
            }
            Err(_) => Ok(self.export_original(notes)),
        }
    }

    fn export_original(&self, notes: &Notes) -> String {
        let escape = |s: &str| {
            s.replace('\\', "\\\\")
                .replace('"', "\\\"")
                .replace(['\n', '\r'], " ")
        };
        let comment = |s: &str| format!("{{ {} }} ", s.replace('{', "(").replace('}', ")"));
        let mut out = String::new();
        for (name, value) in &self.tags {
            out.push_str(&format!("[{name} \"{}\"]\n", escape(value)));
        }
        out.push('\n');
        if let Some(note) = notes.get(&0).filter(|n| !n.is_empty()) {
            out.push_str(&comment(note));
        }
        let initial = self.initial().unwrap_or_default();
        let mut ply =
            (initial.fullmoves().get() as usize - 1) * 2 + usize::from(initial.turn().is_black());
        let mut stack = Vec::new();
        let mut main_ply = 0;
        for token in &self.tokens {
            match token {
                Token::Move(san) => {
                    out.push_str(&format!(
                        "{}{} {san} ",
                        ply / 2 + 1,
                        if ply.is_multiple_of(2) { "." } else { "..." }
                    ));
                    ply += 1;
                    if stack.is_empty() {
                        main_ply += 1;
                        if let Some(note) = notes.get(&main_ply).filter(|n| !n.is_empty()) {
                            out.push_str(&comment(note));
                        }
                    }
                }
                Token::Comment(s) => out.push_str(&comment(s)),
                Token::Nag(n) => out.push_str(&format!("${n} ")),
                Token::Open => {
                    stack.push(ply);
                    ply = ply.saturating_sub(1);
                    out.push_str("( ");
                }
                Token::Close => {
                    if let Some(saved) = stack.pop() {
                        ply = saved;
                        out.push_str(") ");
                    }
                }
            }
        }
        out.push_str(if self.tag("Result").is_empty() {
            "*"
        } else {
            self.tag("Result")
        });
        out.push_str("\n\n");
        out
    }
}

#[derive(Clone)]
pub struct Node {
    pub position: Option<Chess>,
    pub parent: Option<usize>,
    pub children: Vec<usize>,
    pub san: String,
    pub annotations: Vec<Token>,
    pub ply: usize,
}

pub struct Replay {
    pub nodes: Vec<Node>,
    pub warning: Option<String>,
    /// Nodes 0..game_len are the game's own main line; every other node is a variation or analysis.
    game_len: usize,
}

/// A comment without its `[%…]` commands (clocks, evaluations, drawings), trimmed; an unclosed one stays.
fn without_commands(comment: &str) -> String {
    let (mut text, mut rest) = (String::new(), comment);
    while let Some((before, after)) = rest.split_once("[%").and_then(|(b, a)| Some((b, a.split_once(']')?.1))) {
        text.push_str(before);
        rest = after;
    }
    text.push_str(rest);
    text.trim().to_owned()
}

/// Writes the moves after `node` as PGN tokens, `order`'s first child continuing and the rest as variations, and numbers
/// each new node in the order the parser will; nodes that already have a number keep it.
fn write_tokens(replay: &Replay, order: &dyn Fn(usize) -> Vec<usize>, mut node: usize, ids: &mut [Option<usize>], next: &mut usize, tokens: &mut Vec<Token>) {
    fn put(replay: &Replay, n: usize, ids: &mut [Option<usize>], next: &mut usize, tokens: &mut Vec<Token>) {
        ids[n].get_or_insert_with(|| {
            *next += 1;
            *next - 1
        });
        tokens.push(Token::Move(replay.nodes[n].san.clone()));
        tokens.extend(replay.nodes[n].annotations.iter().cloned());
    }
    loop {
        let kids = order(node);
        let Some((&first, others)) = kids.split_first() else { return };
        put(replay, first, ids, next, tokens);
        for &other in others {
            tokens.push(Token::Open);
            put(replay, other, ids, next, tokens);
            write_tokens(replay, order, other, ids, next, tokens);
            tokens.push(Token::Close);
        }
        node = first;
    }
}

impl Replay {
    fn node_after(&self, parent: usize, san: &str) -> Node {
        let position = self.nodes[parent].position.as_ref().and_then(|pos| {
            let movement = san.parse::<ChessSan>().ok()?.san.to_move(pos).ok()?;
            let mut next = pos.clone();
            next.play_unchecked(movement);
            Some(next)
        });
        Node {
            position,
            parent: Some(parent),
            children: vec![],
            san: san.into(),
            annotations: vec![],
            ply: self.nodes[parent].ply + 1,
        }
    }

    /// The moves on the way to `id` that are not their parent's first choice, deepest first; empty on the main line.
    pub fn branches_to(&self, id: usize) -> Vec<usize> {
        let mut branches = Vec::new();
        let mut node = id;
        while let Some(parent) = self.nodes[node].parent {
            if self.nodes[parent].children[0] != node {
                branches.push(node);
            }
            node = parent;
        }
        branches
    }

    pub fn next(&self, id: usize) -> Option<usize> {
        self.nodes[id]
            .children
            .first()
            .copied()
            .filter(|&child| self.nodes[child].position.is_some())
    }

    /// Whether `id` is the start position or a move actually played in the game.
    pub fn in_game(&self, id: usize) -> bool {
        id < self.game_len
    }

    /// The line through `id`; from a game move it runs to the game's end, never into analysis past it.
    pub fn line(&self, id: usize) -> Vec<usize> {
        let mut line = vec![id];
        let mut current = id;
        while let Some(parent) = self.nodes[current].parent {
            line.push(parent);
            current = parent;
        }
        line.reverse();
        current = id;
        while let Some(child) = self.next(current).filter(|&c| !self.in_game(id) || self.in_game(c)) {
            line.push(child);
            current = child;
        }
        line
    }

    /// Half-moves since move 1, so even is a White move and odd a Black move.
    pub fn half_move(&self, id: usize) -> usize {
        let initial = self.nodes[0].position.as_ref().unwrap();
        (initial.fullmoves().get() as usize - 1) * 2
            + usize::from(initial.turn().is_black())
            + self.nodes[id].ply
            - 1
    }

    /// The move's PGN comments as the reader sees them: without `[%…]` commands, and none that held only commands.
    pub fn comments(&self, id: usize) -> impl Iterator<Item = String> + '_ {
        self.nodes[id].annotations.iter().filter_map(|t| match t {
            Token::Comment(c) => Some(without_commands(c)).filter(|c| !c.is_empty()),
            _ => None,
        })
    }

    /// The value of a `[%name value]` command in the move's comments, such as Lichess's `clk` and `eval`.
    pub fn command(&self, id: usize, name: &str) -> Option<&str> {
        let tag = format!("[%{name} ");
        self.nodes[id].annotations.iter().find_map(|t| match t {
            Token::Comment(c) => Some(c[c.find(&tag)? + tag.len()..].split(']').next()?.trim()),
            _ => None,
        })
    }

    /// The move's mark as a NAG 1–6: the reader's own, else the first imported one.
    pub fn glyph(&self, glyphs: &BTreeMap<usize, u8>, id: usize) -> Option<u8> {
        glyphs
            .get(&id)
            .copied()
            .or_else(|| {
                self.nodes[id].annotations.iter().find_map(|t| match t {
                    Token::Nag(n @ 1..=6) => Some(*n),
                    _ => None,
                })
            })
            .filter(|&n| n != 0)
    }

    pub fn label(&self, id: usize) -> String {
        let absolute = self.half_move(id);
        format!(
            "{}{} {}",
            absolute / 2 + 1,
            if absolute.is_multiple_of(2) {
                "."
            } else {
                "…"
            },
            self.nodes[id].san
        )
    }

    fn write_pgn(&self, notes: &Notes, glyphs: &BTreeMap<usize, u8>, out: &mut String) {
        enum Action {
            Node(usize),
            Children(usize),
            /// Repeats a game move's SAN to open a variation continuing past it.
            Restate(usize),
            Open,
            Close,
        }
        let mut pending = vec![Action::Children(0), Action::Node(0)];
        while let Some(action) = pending.pop() {
            match action {
                Action::Open => out.push_str("( "),
                Action::Close => out.push_str(") "),
                Action::Restate(id) => out.push_str(&format!("{} ", self.label(id).replace('…', "..."))),
                Action::Node(id) => {
                    if id != 0 {
                        out.push_str(&self.label(id).replace('…', "..."));
                        out.push(' ');
                    }
                    // The reader's own mark replaces any imported move mark.
                    let own = glyphs.get(&id);
                    if let Some(&n) = own.filter(|&&n| n != 0) {
                        out.push_str(&format!("${n} "));
                    }
                    for token in &self.nodes[id].annotations {
                        match token {
                            Token::Comment(s) => write_comment(out, s),
                            Token::Nag(1..=6) if own.is_some() => {}
                            Token::Nag(n) => out.push_str(&format!("${n} ")),
                            _ => {}
                        }
                    }
                    if let Some(note) = notes.get(&id).filter(|s| !s.is_empty()) {
                        write_comment(out, note);
                    }
                }
                Action::Children(id) => {
                    let children = &self.nodes[id].children;
                    // Analysis played past the game's last move: write "( 44. Kf2 Rb3 )", not as game moves.
                    if self.in_game(id) && children.first().is_some_and(|&c| !self.in_game(c)) {
                        for &child in children.iter().rev() {
                            pending.push(Action::Close);
                            pending.push(Action::Children(child));
                            pending.push(Action::Node(child));
                            if id != 0 {
                                pending.push(Action::Restate(id));
                            }
                            pending.push(Action::Open);
                        }
                    } else if let Some(&first) = children.first() {
                        pending.push(Action::Children(first));
                        for &alternative in children[1..].iter().rev() {
                            pending.push(Action::Close);
                            pending.push(Action::Children(alternative));
                            pending.push(Action::Node(alternative));
                            pending.push(Action::Open);
                        }
                        pending.push(Action::Node(first));
                    }
                }
            }
        }
    }
}

fn write_comment(out: &mut String, text: &str) {
    out.push_str(&format!(
        "{{ {} }} ",
        text.replace('{', "(").replace('}', ")")
    ));
}

pub struct Parser;
impl Parser {
    fn push(game: &mut Game, token: Token, size: usize) -> ControlFlow<Result<Game>> {
        game.bytes += size + 32;
        if game.bytes > 16 * 1024 * 1024 {
            return ControlFlow::Break(Err(anyhow::anyhow!(
                "A single game exceeds the 16 MiB safety limit"
            )));
        }
        game.tokens.push(token);
        ControlFlow::Continue(())
    }
}
impl Visitor for Parser {
    type Tags = Game;
    type Movetext = Game;
    type Output = Result<Game>;
    fn begin_tags(&mut self) -> ControlFlow<Self::Output, Game> {
        ControlFlow::Continue(Game::default())
    }
    fn tag(
        &mut self,
        game: &mut Game,
        name: &[u8],
        value: RawTag<'_>,
    ) -> ControlFlow<Self::Output> {
        let name = String::from_utf8_lossy(name).into_owned();
        let value = value.decode_utf8_lossy().into_owned();
        game.bytes += name.len() + value.len() + 32;
        if game.bytes > 16 * 1024 * 1024 {
            return ControlFlow::Break(Err(anyhow::anyhow!(
                "Game headers exceed the safety limit"
            )));
        }
        game.tags.push((name, value));
        ControlFlow::Continue(())
    }
    fn begin_movetext(&mut self, game: Game) -> ControlFlow<Self::Output, Game> {
        ControlFlow::Continue(game)
    }
    fn san(&mut self, game: &mut Game, san: SanPlus) -> ControlFlow<Self::Output> {
        let san = san.to_string();
        let len = san.len();
        Self::push(game, Token::Move(san), len)
    }
    fn comment(&mut self, game: &mut Game, comment: RawComment<'_>) -> ControlFlow<Self::Output> {
        let text = String::from_utf8_lossy(comment.as_bytes()).into_owned();
        let len = text.len();
        Self::push(game, Token::Comment(text), len)
    }
    fn nag(&mut self, game: &mut Game, nag: Nag) -> ControlFlow<Self::Output> {
        Self::push(game, Token::Nag(nag.0), 1)
    }
    fn begin_variation(&mut self, game: &mut Game) -> ControlFlow<Self::Output, Skip> {
        match Self::push(game, Token::Open, 1) {
            ControlFlow::Break(e) => ControlFlow::Break(e),
            _ => ControlFlow::Continue(Skip(false)),
        }
    }
    fn end_variation(&mut self, game: &mut Game) -> ControlFlow<Self::Output> {
        Self::push(game, Token::Close, 1)
    }
    fn outcome(&mut self, game: &mut Game, outcome: Outcome) -> ControlFlow<Self::Output> {
        if game.tag("Result").is_empty() {
            game.tags.push(("Result".into(), outcome.to_string()));
        }
        ControlFlow::Continue(())
    }
    fn end_game(&mut self, game: Game) -> Self::Output {
        Ok(game)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pgn_reader::Reader;
    pub fn parse(text: &str) -> Game {
        Reader::new(text.as_bytes())
            .read_game(&mut Parser)
            .unwrap()
            .unwrap()
            .unwrap()
    }
    #[test]
    fn variations_notes_and_unicode_round_trip() {
        let game = parse(
            "[White \"Таль\"]\n[Result \"1-0\"]\n\n1. e4 {original} e5 (1... c5 $1 (1... e6)) 2. Nf3 Nc6 1-0",
        );
        let replay = game.replay().unwrap();
        assert_eq!(replay.line(0).len(), 5);
        assert!(matches!(&replay.nodes[1].annotations[0], Token::Comment(s) if s == "original"));
        let notes = BTreeMap::from([(1, "My {idea} ♞".into())]);
        let exported = game.export(&notes).unwrap();
        let again = parse(&exported);
        assert_eq!(again.tag("White"), "Таль");
        assert_eq!(again.replay().unwrap().line(0).len(), replay.line(0).len());
        assert!(exported.contains("c5 $1"));
        assert!(exported.contains("My (idea)"));
    }
    #[test]
    fn own_move_marks_replace_imported_ones() {
        let mut game = parse("1. e4 $1 e5 $2 2. Nf3 $14 *");
        let replay = game.replay().unwrap();
        assert_eq!(replay.glyph(&game.glyphs, 1), Some(1));
        assert_eq!(replay.glyph(&game.glyphs, 3), None, "$14 is not a move mark");
        game.glyphs.extend([(1, 3), (2, 0), (3, 6)]);
        assert_eq!(replay.glyph(&game.glyphs, 2), None);
        let exported = game.export(&Notes::new()).unwrap();
        assert!(exported.contains("1. e4 $3 1... e5 2. Nf3 $6 $14 *"), "{exported}");
    }

    #[test]
    fn fen_black_to_move_and_illegal_move() {
        let game = parse("[FEN \"8/8/8/8/8/6k1/8/4K3 b - - 0 42\"]\n\n42... Kf3 *");
        assert_eq!(game.replay().unwrap().line(0).len(), 2);
        assert!(game.export(&Notes::new()).unwrap().contains("42... Kf3"));
        let bad = parse("1. e4 e5 2. Bh6 *");
        assert_eq!(bad.replay().unwrap().line(0).len(), 3);
        assert!(bad.replay().unwrap().warning.is_some());
    }
    fn play_san(game: &mut Game, replay: &mut Replay, parent: usize, san: &str) -> usize {
        let pos = replay.nodes[parent].position.as_ref().unwrap();
        let movement = san.parse::<ChessSan>().unwrap().san.to_move(pos).unwrap();
        game.play(replay, parent, movement).unwrap()
    }

    #[test]
    fn imported_and_added_branches_keep_stable_notes_and_export() {
        let mut game = parse("1. e4 {main note} e5 (1... c5 $1 (1... e6 {French})) 2. Nf3 *");
        let original = serde_json::to_string(&game.tokens).unwrap();
        let mut replay = game.replay().unwrap();
        assert_eq!(replay.nodes[1].children.len(), 3);
        let c5 = replay.nodes[1].children[1];
        let e6 = replay.nodes[1].children[2];
        assert_eq!(replay.nodes[c5].san, "c5");
        assert_eq!(replay.nodes[e6].san, "e6");
        let nc3 = play_san(&mut game, &mut replay, c5, "Nc3");
        let nf6 = play_san(&mut game, &mut replay, nc3, "Nf6");
        let d4 = play_san(&mut game, &mut replay, 0, "d4");
        assert_eq!(play_san(&mut game, &mut replay, 0, "d4"), d4);
        assert_eq!(game.study.len(), 3);
        assert_eq!(play_san(&mut game, &mut replay, 1, "c5"), c5);
        let notes = Notes::from([
            (1, "old main-line note".into()),
            (c5, "Sicilian note".into()),
            (nc3, "New branch note".into()),
            (nf6, "Nested continuation".into()),
        ]);
        let restored: Game = serde_json::from_str(&serde_json::to_string(&game).unwrap()).unwrap();
        let restored_replay = restored.replay().unwrap();
        assert_eq!(restored_replay.nodes[nc3].san, "Nc3");
        assert_eq!(restored_replay.nodes[nf6].parent, Some(nc3));
        assert_eq!(serde_json::to_string(&restored.tokens).unwrap(), original);
        let exported = restored.export(&notes).unwrap();
        let again = parse(&exported).replay().unwrap();
        assert!(again.warning.is_none(), "{exported}");
        assert_eq!(again.nodes[1].san, "e4");
        assert_eq!(again.nodes[2].san, "e5");
        assert_eq!(again.nodes[3].san, "Nf3");
        let branch = again.nodes.iter().find(|n| n.san == "Nc3").unwrap();
        assert_eq!(again.nodes[branch.parent.unwrap()].san, "c5");
        assert!(
            branch
                .annotations
                .iter()
                .any(|t| matches!(t, Token::Comment(s) if s.trim() == "New branch note"))
        );
        assert!(
            exported.contains("$1")
                && exported.contains("French")
                && exported.contains("old main-line note")
        );
    }

    #[test]
    fn promote_and_delete_added_lines() {
        let mut game = parse("1. e4 e5 (1... c5) 2. Nf3 *");
        let mut replay = game.replay().unwrap();
        let d5 = play_san(&mut game, &mut replay, 1, "d5");
        let exd5 = play_san(&mut game, &mut replay, d5, "exd5");
        let nc3 = play_san(&mut game, &mut replay, d5, "Nc3");
        let d4 = play_san(&mut game, &mut replay, 0, "d4");
        let mut notes = Notes::from([(exd5, "gone".into()), (d4, "kept".into()), (2, "game".into())]);
        game.glyphs.insert(d4, 1);
        game.position_labels.insert(d4, ["Gambit".to_owned()].into());
        game.position_labels.insert(nc3, ["Gone".to_owned()].into());
        assert!(game.promote(&replay, nc3));
        assert!(!game.promote(&replay, d5), "the game's own move stays first");
        let replay = game.replay().unwrap();
        assert_eq!(replay.nodes[d5].children, vec![nc3, exd5]);
        assert_eq!(game.delete_line(&replay, &mut notes, 0), None, "the starting position stays");
        assert_eq!(game.delete_line(&replay, &mut notes, d5), Some(1));
        let replay = game.replay().unwrap();
        assert_eq!(game.study.len(), 1);
        assert_eq!(replay.nodes[d5].san, "d4", "later added moves shift into the freed IDs");
        assert_eq!(notes, Notes::from([(2, "game".into()), (d5, "kept".into())]));
        assert_eq!(game.glyphs, BTreeMap::from([(d5, 1)]));
        assert_eq!(game.position_labels.keys().copied().collect::<Vec<_>>(), vec![d5], "position tags follow their move");
        assert!(game.promoted.is_empty());
        assert!(!game.export(&notes).unwrap().contains("d5"));
    }

    #[test]
    fn delete_game_moves_and_imported_lines() {
        // 0 start, 1 e4, 2 e5, 3 Nf3, 4 Nc6, then the side line 5 c5, 6 Nf3.
        let fresh = || {
            let mut game = parse("1. e4 e5 (1... c5 2. Nf3) 2. Nf3 Nc6 *");
            let mut replay = game.replay().unwrap();
            let d6 = play_san(&mut game, &mut replay, 6, "d6");
            game.review = parse("1. e4 { [%eval 0.18] } *").pgn_review();
            let notes = Notes::from([(2, "e5".into()), (4, "Nc6".into()), (5, "c5".into()), (d6, "d6".into())]);
            (game.replay().unwrap(), game, notes)
        };
        let (replay, mut game, mut notes) = fresh();
        assert_eq!(game.delete_line(&replay, &mut notes, 5), Some(1), "an imported side line, with what was added to it");
        let replay = game.replay().unwrap();
        assert_eq!(replay.nodes.len(), 5);
        assert!(game.study.is_empty());
        assert_eq!(notes, Notes::from([(2, "e5".into()), (4, "Nc6".into())]));
        assert!(game.review.is_some(), "the game's moves are untouched");
        assert!(!game.export(&notes).unwrap().contains("c5"));

        let (replay, mut game, mut notes) = fresh();
        assert_eq!(game.delete_line(&replay, &mut notes, 2), Some(1), "the game's own 1... e5");
        let replay = game.replay().unwrap();
        assert_eq!(replay.line(0).iter().map(|&n| replay.nodes[n].san.as_str()).collect::<Vec<_>>(), ["", "e4", "c5", "Nf3"], "the side line continues the game");
        assert_eq!((replay.nodes[4].san.as_str(), replay.nodes[4].parent), ("d6", Some(3)), "the added move stays added, after it");
        assert_eq!(notes, Notes::from([(2, "c5".into()), (4, "d6".into())]), "notes follow their moves");
        assert!(game.review.is_none());
    }

    #[test]
    fn lichess_evaluations_and_clocks() {
        // Lichess writes the evaluation and clock in one comment or several, and leaves the mating move unevaluated.
        let game = parse("1. e4 { [%eval 0.18] [%clk 0:29:54] } e5 { [%eval 0.2] } { Book. } { [%clk 0:30:11] } 2. Qh5 { [%eval #-3] } Ke7 { [%eval #1] } 3. Qxe5# { [%clk 1:02:03.5] } 1-0");
        let replay = game.replay().unwrap();
        assert_eq!((replay.command(1, "clk"), replay.command(2, "clk"), replay.command(3, "clk")), (Some("0:29:54"), Some("0:30:11"), None));
        assert_eq!(replay.comments(1).count(), 0);
        assert_eq!(replay.comments(2).collect::<Vec<_>>(), ["Book."]);
        assert_eq!(without_commands("a [%cal Ge2e4] b [%unclosed"), "a  b [%unclosed");
        let report = game.pgn_review().unwrap();
        let summary: Vec<_> = report.moves.iter().map(|m| (m.node, m.score.as_str(), m.quality)).collect();
        use crate::engine::Quality::*;
        assert_eq!(summary, [(1, "+0.18", Good), (2, "+0.20", Good), (3, "Mate -3", Blunder), (4, "Mate +1", Blunder), (5, "Checkmate", Good)]);
        assert!(game.export(&Notes::new()).unwrap().contains("[%clk 0:29:54]"), "export keeps them");
        assert!(parse("1. e4 { [%eval 0.18] } e5 2. Nf3 { [%eval 0.2] } *").pgn_review().is_none(), "a move without one");
        assert!(parse("1. e4 e5 *").pgn_review().is_none());
    }

    #[test]
    fn set_main_line_promotes_added_branches() {
        let mut game = parse("1. e4 e5 *");
        let mut replay = game.replay().unwrap();
        let nf3 = play_san(&mut game, &mut replay, 2, "Nf3");
        play_san(&mut game, &mut replay, nf3, "Nc6");
        let bc4 = play_san(&mut game, &mut replay, 2, "Bc4");
        play_san(&mut game, &mut replay, bc4, "Nf6");
        let bc5 = play_san(&mut game, &mut replay, bc4, "Bc5");
        let tokens = format!("{:?}", game.tokens);
        assert_eq!(game.set_main_line(&replay, &mut Notes::new(), nf3), None, "already the main line");
        assert_eq!(game.set_main_line(&replay, &mut Notes::new(), bc5), Some(bc5));
        let replay = game.replay().unwrap();
        assert_eq!(replay.nodes[2].children, vec![bc4, nf3]);
        assert_eq!(replay.nodes[bc4].children[0], bc5);
        assert_eq!(format!("{:?}", game.tokens), tokens, "past the game's end only the order changes");
    }

    #[test]
    fn set_main_line_over_the_game_rewrites_it_and_keeps_notes_on_their_moves() {
        // Nodes: 1 e4, 2 e5, 3 Nf3, 4 Nc6, 5 c5, 6 Nf3; added 7 e6, 8 d4, 9 Nf3, 10 Bb5.
        let mut game = parse("1. e4 e5 {Open} (1... c5 2. Nf3) 2. Nf3 Nc6 *");
        let mut replay = game.replay().unwrap();
        let e6 = play_san(&mut game, &mut replay, 1, "e6");
        let d4 = play_san(&mut game, &mut replay, e6, "d4");
        play_san(&mut game, &mut replay, e6, "Nf3");
        let bb5 = play_san(&mut game, &mut replay, 4, "Bb5");
        let mut notes = Notes::from([(2, "e5".into()), (d4, "French".into()), (bb5, "later".into())]);
        game.glyphs.insert(e6, 1);
        game.bookmarks.insert(6);
        game.position_labels.insert(d4, ["Tag".to_owned()].into());
        assert_eq!(game.set_main_line(&replay, &mut notes, d4), Some(3));
        let replay = game.replay().unwrap();
        let sans = |r: &Replay, line: Vec<usize>| line.iter().skip(1).map(|&n| r.nodes[n].san.clone()).collect::<Vec<_>>();
        assert_eq!(sans(&replay, replay.line(0)), ["e4", "e6", "d4"]);
        assert!(replay.in_game(3) && replay.nodes[4].san == "e5");
        assert!(replay.nodes[4].annotations.iter().any(|t| matches!(t, Token::Comment(c) if c == "Open")));
        assert_eq!(notes, Notes::from([(3, "French".into()), (4, "e5".into()), (10, "later".into())]));
        assert_eq!((game.glyphs.get(&2), game.bookmarks.iter().next(), game.position_labels.contains_key(&3)), (Some(&1), Some(&8), true));
        assert_eq!((replay.nodes[9].san.as_str(), replay.nodes[9].parent), ("Nf3", Some(2)), "the other added move stays added");
        assert_eq!((replay.nodes[10].san.as_str(), replay.nodes[10].parent), ("Bb5", Some(6)));
        assert_eq!(game.first_added(&replay), 9);
        // And back: the old game's line is the main line again, now with 2... Bb5 played on.
        assert_eq!(game.set_main_line(&replay, &mut notes, 6), Some(4));
        let replay = game.replay().unwrap();
        assert_eq!(sans(&replay, replay.line(0)), ["e4", "e5", "Nf3", "Nc6", "Bb5"]);
        assert_eq!(notes.get(&2).map(String::as_str), Some("e5"));
    }

    #[test]
    fn analysis_past_the_last_move_stays_out_of_the_game() {
        let mut game = parse("1. e4 e5 2. Nf3 *");
        let mut replay = game.replay().unwrap();
        let nc6 = play_san(&mut game, &mut replay, 3, "Nc6");
        let bb5 = play_san(&mut game, &mut replay, nc6, "Bb5");
        assert_eq!(replay.line(0), vec![0, 1, 2, 3]);
        assert_eq!(replay.line(nc6), vec![0, 1, 2, 3, nc6, bb5]);
        assert!(replay.in_game(3) && !replay.in_game(nc6));
        let exported = game.export(&Notes::new()).unwrap();
        assert!(exported.contains("2. Nf3 ( 2. Nf3 2... Nc6 3. Bb5 ) *"), "{exported}");
        let again = parse(&exported).replay().unwrap();
        assert_eq!(again.line(0).len(), 4, "re-imported game keeps its own length");
    }

    #[test]
    fn illegal_imported_branch_does_not_block_main_line_or_lose_tokens() {
        let game = parse("1. e4 e5 (1... Bh6 2. a3) 2. Nf3 *");
        let replay = game.replay().unwrap();
        assert_eq!(replay.line(0).len(), 4);
        assert!(replay.warning.is_some());
        let exported = game.export(&Notes::new()).unwrap();
        assert!(exported.contains("Bh6") && exported.contains("a3"));
    }

    #[test]
    fn special_moves_and_invalid_saved_variations() {
        for (fen, san) in [
            ("r3k2r/8/8/8/8/8/8/R3K2R w KQkq - 0 1", "O-O"),
            ("4k3/8/8/3pP3/8/8/8/4K3 w - d6 0 1", "exd6"),
            ("4k3/P7/8/8/8/8/8/4K3 w - - 0 1", "a8=N"),
            ("8/8/8/8/8/6k1/8/4K3 b - - 0 42", "Kf3"),
        ] {
            let mut game = parse(&format!("[FEN \"{fen}\"]\n\n*"));
            let mut replay = game.replay().unwrap();
            play_san(&mut game, &mut replay, 0, san);
            let exported = game.export(&Notes::new()).unwrap();
            assert!(
                parse(&exported).replay().unwrap().warning.is_none(),
                "{exported}"
            );
        }
        let mut game = parse("1. e4 *");
        game.study.push(StudyMove {
            parent: 99,
            san: "d4".into(),
        });
        assert!(game.replay().is_err());
        assert!(game.export(&Notes::new()).is_err());
    }
    #[test]
    fn board_lines_export_as_game_moves() {
        let mut game = parse("*");
        let mut replay = game.replay().unwrap();
        let mut at = 0;
        for san in ["f4", "e5"] {
            let position = replay.nodes[at].position.clone().unwrap();
            let movement = san.parse::<ChessSan>().unwrap().san.to_move(&position).unwrap();
            at = game.play(&mut replay, at, movement).unwrap();
        }
        assert!(game.export(&Notes::new()).unwrap().contains("( 1. f4"), "plain export keeps them as analysis");
        let saved = parse(&game.export_as_game(&Notes::new()).unwrap());
        assert_eq!(saved.mainline_positions().unwrap().len(), 3);
    }

    #[test]
    fn whole_game_positions_exclude_study_and_reject_truncated_main_lines() {
        let mut game = parse("1. e4 e5 *");
        let mut replay = game.replay().unwrap();
        play_san(&mut game, &mut replay, 2, "Nf3");
        play_san(&mut game, &mut replay, 0, "d4");
        assert_eq!(game.mainline_positions().unwrap().len(), 3);
        assert!(parse("1. e4 e5 2. Bh6 *").mainline_positions().is_err());
        let black = parse("[FEN \"8/8/8/8/8/6k1/8/4K3 b - - 0 42\"]\n\n42... Kf3 *");
        assert_eq!(
            black.mainline_positions().unwrap()[0].1.turn(),
            shakmaty::Color::Black
        );
    }
}
