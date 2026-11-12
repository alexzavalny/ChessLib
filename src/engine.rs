use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use shakmaty::{Chess, Color, EnPassantMode, Move, Position, fen::Fen, san::SanPlus, uci::UciMove};
use std::{
    io::{BufRead, BufReader, Write},
    path::PathBuf,
    process::{Child, ChildStdin, Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};

#[derive(Clone, Debug)]
pub struct Evaluation {
    pub score: String,
    pub depth: u32,
    pub line: String,
    pub best: Option<Move>,
    pub white_chance: f64,
    /// 1-based MultiPV rank; 1 is the engine's top line.
    pub multipv: usize,
}

pub enum Event {
    Update(Evaluation),
    Finished(Result<()>),
    Progress { done: usize, total: usize },
    Reviewed(Result<Report>),
    /// The computer's reply in a game against it.
    Moved(Result<Move>),
}

pub struct Job {
    pub receive: mpsc::Receiver<Event>,
    cancel: Arc<AtomicBool>,
}
impl Drop for Job {
    // No join: the worker sees the flag within ~50 ms and reaps Stockfish itself.
    // Joining here blocked the UI thread and swallowed the piece-slide animation.
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

pub fn discover() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("CHESSLIB_STOCKFISH") {
        return Some(path.into());
    }
    [
        "/opt/homebrew/bin/stockfish",
        "/usr/local/bin/stockfish",
        "/usr/bin/stockfish",
    ]
    .into_iter()
    .map(PathBuf::from)
    .find(|p| p.is_file())
    .or_else(|| {
        std::env::var_os("PATH").and_then(|paths| {
            std::env::split_paths(&paths)
                .map(|p| p.join("stockfish"))
                .find(|p| p.is_file())
        })
    })
}

pub fn start(
    path: PathBuf,
    position: Chess,
    continuous: bool,
    lines: usize,
    ctx: eframe::egui::Context,
) -> Job {
    spawn(ctx, move |cancel, emit| {
        let result = (|| {
            let mut engine = Engine::new(path, cancel)?;
            writeln!(engine.input, "setoption name MultiPV value {lines}")?;
            engine.analyze(
                &position,
                &position_command(&position, &[]),
                if continuous { None } else { Some(3000) },
                cancel,
                |e| emit(Event::Update(e)),
            )?;
            Ok(())
        })();
        emit(Event::Finished(result));
    })
}

/// Stockfish picks a reply, weakened by UCI `Skill Level` (0–20).
pub fn start_move(path: PathBuf, position: Chess, level: u8, ctx: eframe::egui::Context) -> Job {
    spawn(ctx, move |cancel, emit| {
        let result = (|| {
            let mut engine = Engine::new(path, cancel)?;
            writeln!(engine.input, "setoption name Skill Level value {}", level.min(20))?;
            // shortcut: sends only the FEN, so Stockfish cannot see repetitions; send the move list if draws by repetition matter.
            let evaluation = engine.analyze(&position, &position_command(&position, &[]), Some(800), cancel, |_| {})?;
            evaluation.best.context("Stockfish found no move")
        })();
        emit(Event::Moved(result));
    })
}

pub fn start_review(
    path: PathBuf,
    positions: Vec<(usize, Chess)>,
    ctx: eframe::egui::Context,
) -> Job {
    spawn(ctx, move |cancel, emit| {
        let result = review(path, &positions, cancel, |done, total| {
            emit(Event::Progress { done, total })
        });
        emit(Event::Reviewed(result));
    })
}

fn spawn(
    ctx: eframe::egui::Context,
    work: impl FnOnce(&AtomicBool, &dyn Fn(Event)) + Send + 'static,
) -> Job {
    let (send, receive) = mpsc::channel();
    let cancel = Arc::new(AtomicBool::new(false));
    let flag = cancel.clone();
    std::thread::spawn(move || {
        work(&flag, &|event| {
            let _ = send.send(event);
            ctx.request_repaint();
        })
    });
    Job {
        receive,
        cancel,
    }
}

// Always reap the engine, including cancellation, protocol errors and broken pipes.
struct Process(Child);
impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

struct Engine {
    _process: Process,
    input: ChildStdin,
    receive: mpsc::Receiver<std::io::Result<String>>,
}

impl Engine {
    fn new(path: PathBuf, cancel: &AtomicBool) -> Result<Self> {
        let mut child = Process(
            Command::new(&path)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()
                .with_context(|| format!("Could not start {}", path.display()))?,
        );
        let input = child.0.stdin.take().context("No engine input")?;
        let output = child.0.stdout.take().context("No engine output")?;
        let (send, receive) = mpsc::sync_channel(64);
        std::thread::spawn(move || {
            for line in BufReader::new(output).lines() {
                if send.send(line).is_err() {
                    break;
                }
            }
        });
        let mut engine = Self {
            _process: child,
            input,
            receive,
        };
        writeln!(engine.input, "uci")?;
        engine.input.flush()?;
        wait_for(&engine.receive, "uciok", cancel)?;
        writeln!(
            engine.input,
            "setoption name Threads value 1\nsetoption name Hash value 64\nisready"
        )?;
        engine.input.flush()?;
        wait_for(&engine.receive, "readyok", cancel)?;
        Ok(engine)
    }

    fn analyze(
        &mut self,
        position: &Chess,
        command: &str,
        milliseconds: Option<u64>,
        cancel: &AtomicBool,
        mut update: impl FnMut(Evaluation),
    ) -> Result<Evaluation> {
        if let Some(outcome) = position.outcome().known() {
            let white_chance = match outcome.winner() {
                Some(Color::White) => 100.0,
                Some(Color::Black) => 0.0,
                None => 50.0,
            };
            return Ok(Evaluation {
                score: if outcome.winner().is_none() {
                    "0.00".into()
                } else {
                    "Checkmate".into()
                },
                depth: 0,
                line: String::new(),
                best: None,
                white_chance,
                multipv: 1,
            });
        }
        writeln!(self.input, "{command}")?;
        if let Some(ms) = milliseconds {
            writeln!(self.input, "go movetime {ms}")?;
        } else {
            writeln!(self.input, "go infinite")?;
        }
        self.input.flush()?;
        let deadline = milliseconds
            .map(|ms| Instant::now() + Duration::from_millis(ms) + Duration::from_secs(10));
        let mut latest = None;
        loop {
            let line = read_line(&self.receive, deadline, cancel)?;
            if let Some(evaluation) = parse_info(&line, position) {
                update(evaluation.clone());
                if evaluation.multipv == 1 {
                    latest = Some(evaluation);
                }
            }
            if let Some(rest) = line.strip_prefix("bestmove ") {
                let mut latest = latest.context("Stockfish returned no usable evaluation")?;
                // With a Skill Level set, bestmove can differ from the top line's first move.
                if let Some(movement) = rest
                    .split_whitespace()
                    .next()
                    .and_then(|uci| uci.parse::<UciMove>().ok()?.to_move(position).ok())
                {
                    latest.best = Some(movement);
                }
                return Ok(latest);
            }
        }
    }
}

fn position_command(initial: &Chess, moves: &[Move]) -> String {
    let mut command = format!(
        "position fen {}",
        Fen::from_position(initial, EnPassantMode::Legal)
    );
    if !moves.is_empty() {
        command.push_str(" moves");
        for &movement in moves {
            command.push(' ');
            command.push_str(&UciMove::from_standard(movement).to_string());
        }
    }
    command
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Quality {
    Best,
    Good,
    Inaccuracy,
    Mistake,
    Blunder,
}
impl Quality {
    pub fn label(self) -> &'static str {
        match self {
            Self::Best => "Best",
            Self::Good => "Good",
            Self::Inaccuracy => "Inaccuracy",
            Self::Mistake => "Mistake",
            Self::Blunder => "Blunder",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ReviewedMove {
    pub node: usize,
    pub white: bool,
    pub quality: Quality,
    pub accuracy: f64,
    pub loss: f64,
    pub score: String,
    pub best: String,
    pub depth: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Report {
    pub moves: Vec<ReviewedMove>,
    pub milliseconds_per_position: u64,
}
impl Report {
    pub fn accuracy(&self, white: bool) -> Option<f64> {
        let moves: Vec<_> = self.moves.iter().filter(|m| m.white == white).collect();
        (!moves.is_empty())
            .then(|| moves.iter().map(|m| m.accuracy).sum::<f64>() / moves.len() as f64)
    }
    pub fn count(&self, white: bool, quality: Quality) -> usize {
        self.moves
            .iter()
            .filter(|m| m.white == white && m.quality == quality)
            .count()
    }
}

fn classify(
    before: &Evaluation,
    after: &Evaluation,
    white: bool,
    movement: Move,
) -> (Quality, f64, f64) {
    let loss =
        ((before.white_chance - after.white_chance) * if white { 1.0 } else { -1.0 }).max(0.0);
    // Lichess's published move formula; our game score is a simple mean, not their game metric.
    let accuracy = if loss == 0.0 {
        100.0
    } else {
        (103.1668 * (-0.04354 * loss).exp() - 3.1669).clamp(0.0, 100.0)
    };
    let quality = if loss >= 20.0 {
        Quality::Blunder
    } else if loss >= 10.0 {
        Quality::Mistake
    } else if loss >= 5.0 {
        Quality::Inaccuracy
    } else if before.best == Some(movement) {
        Quality::Best
    } else {
        Quality::Good
    };
    (quality, accuracy, loss)
}

fn review(
    path: PathBuf,
    positions: &[(usize, Chess)],
    cancel: &AtomicBool,
    mut progress: impl FnMut(usize, usize),
) -> Result<Report> {
    anyhow::ensure!(positions.len() >= 2, "The game has no moves to analyze");
    let mut engine = Engine::new(path, cancel)?;
    let mut report = Report {
        moves: vec![],
        milliseconds_per_position: 500,
    };
    let initial = &positions[0].1;
    let mut played = vec![];
    let mut before = engine.analyze(
        initial,
        &position_command(initial, &played),
        Some(500),
        cancel,
        |_| {},
    )?;
    progress(1, positions.len());
    for pair in positions.windows(2) {
        if cancel.load(Ordering::Relaxed) {
            bail!("Game analysis stopped");
        }
        let previous = &pair[0].1;
        let (node, position) = &pair[1];
        let movement = previous
            .legal_moves()
            .into_iter()
            .find(|&movement| {
                let mut next = previous.clone();
                next.play_unchecked(movement);
                next == *position
            })
            .context("Game contains an invalid continuation")?;
        played.push(movement);
        let after = engine.analyze(
            position,
            &position_command(initial, &played),
            Some(500),
            cancel,
            |_| {},
        )?;
        let white = previous.turn() == Color::White;
        let (quality, accuracy, loss) = classify(&before, &after, white, movement);
        report.moves.push(ReviewedMove {
            node: *node,
            white,
            quality,
            accuracy,
            loss,
            score: after.score.clone(),
            best: before.line.clone(),
            depth: after.depth,
        });
        progress(played.len() + 1, positions.len());
        before = after;
    }
    Ok(report)
}

fn read_line(
    receive: &mpsc::Receiver<std::io::Result<String>>,
    deadline: Option<Instant>,
    cancel: &AtomicBool,
) -> Result<String> {
    loop {
        if cancel.load(Ordering::Relaxed) {
            bail!("Analysis stopped");
        }
        if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            bail!("Stockfish did not respond in time. Try analyzing again.");
        }
        match receive.recv_timeout(Duration::from_millis(50)) {
            Ok(line) => return Ok(line?),
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => bail!("Stockfish exited unexpectedly"),
        }
    }
}

fn wait_for(
    receive: &mpsc::Receiver<std::io::Result<String>>,
    expected: &str,
    cancel: &AtomicBool,
) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(5);
    while read_line(receive, Some(deadline), cancel)?.trim() != expected {}
    Ok(())
}

fn parse_info(line: &str, position: &Chess) -> Option<Evaluation> {
    let words: Vec<_> = line.split_whitespace().collect();
    if words.first() != Some(&"info")
        || words.contains(&"lowerbound")
        || words.contains(&"upperbound")
    {
        return None;
    }
    let at = |key| words.iter().position(|&word| word == key);
    let score_at = at("score")?;
    let value: i32 = words.get(score_at + 2)?.parse().ok()?;
    let white_value = if position.turn() == Color::White {
        value
    } else {
        value.checked_neg()?
    };
    let kind = *words.get(score_at + 1)?;
    let score = match kind {
        "cp" => format!("{:+.2}", f64::from(white_value) / 100.0),
        "mate" => format!("Mate {white_value:+}"),
        _ => return None,
    };
    let depth = words.get(at("depth")? + 1)?.parse().ok()?;
    let mut pos = position.clone();
    let mut moves = Vec::new();
    let mut best = None;
    for word in words.get(at("pv")? + 1..)? {
        let movement = word.parse::<UciMove>().ok()?.to_move(&pos).ok()?;
        if best.is_none() {
            best = Some(movement);
        }
        moves.push(SanPlus::from_move(pos.clone(), movement).to_string());
        pos.play_unchecked(movement);
    }
    Some(Evaluation {
        multipv: match at("multipv") {
            Some(i) => words.get(i + 1)?.parse().ok()?,
            None => 1,
        },
        score,
        depth,
        line: moves.join(" "),
        best,
        white_chance: if kind == "cp" {
            100.0 / (1.0 + (-0.00368208 * f64::from(white_value)).exp())
        } else if white_value > 0 || (value == 0 && position.turn() == Color::Black) {
            100.0
        } else {
            0.0
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn analyze(
        path: PathBuf,
        position: Chess,
        cancel: &AtomicBool,
        update: impl FnMut(Evaluation),
    ) -> Result<()> {
        Engine::new(path, cancel)?.analyze(
            &position,
            &position_command(&position, &[]),
            Some(3000),
            cancel,
            update,
        )?;
        Ok(())
    }
    #[test]
    fn scores_are_white_relative_and_pv_is_legal_san() {
        let pos = Chess::default();
        let e = parse_info("info depth 12 score cp 34 pv e2e4 e7e5 g1f3", &pos).unwrap();
        assert_eq!(e.score, "+0.34");
        assert_eq!(e.line, "e4 e5 Nf3");
        let mut black = pos.clone();
        black.play_unchecked(e.best.unwrap());
        assert_eq!(
            parse_info("info depth 10 score cp 25 pv e7e5", &black)
                .unwrap()
                .score,
            "-0.25"
        );
        assert_eq!(
            parse_info("info depth 10 score mate -3 pv e7e5", &black)
                .unwrap()
                .score,
            "Mate +3"
        );
        assert!(parse_info("info depth 1 score cp 0 pv e2e5", &pos).is_none());
        assert!(parse_info("info depth 1 score cp 0 lowerbound pv e2e4", &pos).is_none());
        assert_eq!(e.multipv, 1);
        let second = parse_info("info depth 12 multipv 2 score cp 20 pv d2d4", &pos).unwrap();
        assert_eq!((second.multipv, second.line.as_str()), (2, "d4"));
    }
    #[test]
    fn engine_failure_timeout_and_cancellation_are_reported() {
        assert!(
            analyze(
                PathBuf::from("/nonexistent/chesslib-stockfish"),
                Chess::default(),
                &AtomicBool::new(false),
                |_| {}
            )
            .is_err()
        );
        let (send, receive) = mpsc::channel();
        let now = Instant::now();
        assert!(
            read_line(&receive, Some(now), &AtomicBool::new(false))
                .unwrap_err()
                .to_string()
                .contains("time")
        );
        assert!(
            read_line(
                &receive,
                Some(now + Duration::from_secs(5)),
                &AtomicBool::new(true)
            )
            .unwrap_err()
            .to_string()
            .contains("stopped")
        );
        drop(send);
        assert!(
            read_line(
                &receive,
                Some(now + Duration::from_secs(5)),
                &AtomicBool::new(false)
            )
            .unwrap_err()
            .to_string()
            .contains("exited")
        );
        let mut black = Chess::default();
        black.play_unchecked("e2e4".parse::<UciMove>().unwrap().to_move(&black).unwrap());
        assert!(parse_info("info depth 1 score cp -2147483648 pv e7e5", &black).is_none());
    }

    #[test]
    fn review_metrics_respect_side_thresholds_and_empty_sides() {
        let pos = Chess::default();
        let movement = "e2e4".parse::<UciMove>().unwrap().to_move(&pos).unwrap();
        let mut before = Evaluation {
            score: "0.00".into(),
            depth: 12,
            line: "e4".into(),
            best: Some(movement),
            white_chance: 50.0,
            multipv: 1,
        };
        for (loss, quality) in [
            (0.0, Quality::Best),
            (4.9, Quality::Best),
            (5.0, Quality::Inaccuracy),
            (10.0, Quality::Mistake),
            (20.0, Quality::Blunder),
        ] {
            let mut after = before.clone();
            after.white_chance -= loss;
            let (got, accuracy, actual_loss) = classify(&before, &after, true, movement);
            assert_eq!(got, quality);
            assert!((actual_loss - loss).abs() < 1e-9);
            assert!((0.0..=100.0).contains(&accuracy));
        }
        let mut after = before.clone();
        after.white_chance = 75.0;
        assert_eq!(
            classify(&before, &after, false, movement).0,
            Quality::Blunder
        );
        assert_eq!(classify(&before, &after, true, movement).1, 100.0);
        before.best = None;
        assert_eq!(classify(&before, &before, true, movement).0, Quality::Good);
        let report = Report {
            moves: vec![ReviewedMove {
                node: 1,
                white: false,
                quality: Quality::Blunder,
                accuracy: 20.0,
                loss: 30.0,
                score: "-2".into(),
                best: String::new(),
                depth: 12,
            }],
            milliseconds_per_position: 500,
        };
        assert_eq!(report.accuracy(true), None);
        assert_eq!(report.accuracy(false), Some(20.0));
        assert_eq!(report.count(false, Quality::Blunder), 1);
        let restored: Report =
            serde_json::from_str(&serde_json::to_string(&report).unwrap()).unwrap();
        assert_eq!(restored.accuracy(false), Some(20.0));
    }

    #[test]
    #[ignore = "requires an installed Stockfish executable"]
    fn installed_stockfish_continuous_and_whole_game_review() {
        let path = discover().unwrap();
        let cancel = AtomicBool::new(false);
        let mut engine = Engine::new(path.clone(), &cancel).unwrap();
        let position = Chess::default();
        let now = Instant::now();
        let result = engine.analyze(
            &position,
            &position_command(&position, &[]),
            None,
            &cancel,
            |_| {
                if now.elapsed() > Duration::from_millis(3200) {
                    cancel.store(true, Ordering::Relaxed);
                }
            },
        );
        assert!(result.is_err());
        assert!(now.elapsed() >= Duration::from_millis(3200));
        let game = pgn_reader::Reader::new("1. f3 e5 2. g4 Qh4# 0-1".as_bytes())
            .read_game(&mut crate::pgn::Parser)
            .unwrap()
            .unwrap()
            .unwrap();
        let positions = game.mainline_positions().unwrap();
        let mut progress = vec![];
        let report = review(path, &positions, &AtomicBool::new(false), |done, total| {
            progress.push((done, total))
        })
        .unwrap();
        assert_eq!(report.moves.len(), 4);
        assert_eq!(progress.last(), Some(&(5, 5)));
        assert!(
            report
                .moves
                .iter()
                .any(|m| m.white && m.quality == Quality::Blunder)
        );
        assert_eq!(report.moves[3].score, "Checkmate");
        assert!(report.accuracy(true).unwrap() < report.accuracy(false).unwrap());
    }

    #[test]
    #[ignore = "requires an installed Stockfish executable"]
    fn installed_stockfish_analyzes_and_cancels() {
        let path = discover().expect("Stockfish not found");
        let mut updates = vec![];
        analyze(
            path.clone(),
            Chess::default(),
            &AtomicBool::new(false),
            |e| updates.push(e),
        )
        .unwrap();
        assert!(updates.iter().any(|e| e.depth > 0 && e.best.is_some()));
        let mut weak = Engine::new(path.clone(), &AtomicBool::new(false)).unwrap();
        writeln!(weak.input, "setoption name Skill Level value 0").unwrap();
        let position = Chess::default();
        let reply = weak
            .analyze(&position, &position_command(&position, &[]), Some(300), &AtomicBool::new(false), |_| {})
            .unwrap();
        assert!(position.is_legal(reply.best.unwrap()));
        let cancel = AtomicBool::new(false);
        let now = Instant::now();
        assert!(
            analyze(path, Chess::default(), &cancel, |_| {
                cancel.store(true, Ordering::Relaxed);
            })
            .is_err()
        );
        assert!(now.elapsed() < Duration::from_secs(2));
    }
}
