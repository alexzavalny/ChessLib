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
        // Main-line IDs remain the old note indices; imported tokens never change.
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
        Ok(replay)
    }

    pub fn mainline_positions(&self) -> Result<Vec<(usize, Chess)>> {
        let mut original = self.clone();
        original.study.clear();
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
        match self.replay() {
            Ok(replay) => {
                let mut out = String::new();
                for (name, value) in &self.tags {
                    let value = value
                        .replace('\\', "\\\\")
                        .replace('"', "\\\"")
                        .replace(['\n', '\r'], " ");
                    out.push_str(&format!("[{name} \"{value}\"]\n"));
                }
                out.push('\n');
                replay.write_pgn(notes, &mut out);
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

    pub fn next(&self, id: usize) -> Option<usize> {
        self.nodes[id]
            .children
            .first()
            .copied()
            .filter(|&child| self.nodes[child].position.is_some())
    }

    pub fn line(&self, id: usize) -> Vec<usize> {
        let mut line = vec![id];
        let mut current = id;
        while let Some(parent) = self.nodes[current].parent {
            line.push(parent);
            current = parent;
        }
        line.reverse();
        current = id;
        while let Some(child) = self.next(current) {
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

    fn write_pgn(&self, notes: &Notes, out: &mut String) {
        enum Action {
            Node(usize),
            Children(usize),
            Open,
            Close,
        }
        let mut pending = vec![Action::Children(0), Action::Node(0)];
        while let Some(action) = pending.pop() {
            match action {
                Action::Open => out.push_str("( "),
                Action::Close => out.push_str(") "),
                Action::Node(id) => {
                    if id != 0 {
                        out.push_str(&self.label(id).replace('…', "..."));
                        out.push(' ');
                    }
                    for token in &self.nodes[id].annotations {
                        match token {
                            Token::Comment(s) => write_comment(out, s),
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
                    if let Some(&first) = children.first() {
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
