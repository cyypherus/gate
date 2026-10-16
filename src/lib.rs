use clap::ValueEnum;
use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub type Result<T> = std::result::Result<T, String>;

pub const YOU: &str = "you";
pub const IMPLEMENTER: &str = "implementer";
pub const ARBITER: &str = "arbiter";

const SCHEMA: &str = "
PRAGMA foreign_keys = ON;
PRAGMA journal_mode = WAL;
CREATE TABLE IF NOT EXISTS prs (slug TEXT PRIMARY KEY, base TEXT NOT NULL, prompt TEXT NOT NULL, state TEXT NOT NULL DEFAULT 'open');
CREATE TABLE IF NOT EXISTS reviews (
    id INTEGER PRIMARY KEY,
    slug TEXT NOT NULL REFERENCES prs(slug) ON DELETE CASCADE,
    sha TEXT NOT NULL,
    reviewer TEXT NOT NULL,
    verdict TEXT NOT NULL,
    at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS threads (
    id INTEGER PRIMARY KEY,
    review INTEGER NOT NULL REFERENCES reviews(id) ON DELETE CASCADE,
    file TEXT NOT NULL,
    start_line INTEGER NOT NULL,
    end_line INTEGER NOT NULL CHECK (end_line >= start_line),
    resolved_by TEXT
);
CREATE TABLE IF NOT EXISTS messages (
    id INTEGER PRIMARY KEY,
    thread INTEGER NOT NULL REFERENCES threads(id) ON DELETE CASCADE,
    author TEXT NOT NULL,
    kind TEXT NOT NULL,
    body TEXT NOT NULL,
    at INTEGER NOT NULL
);
DROP TABLE IF EXISTS waiters;
CREATE TABLE IF NOT EXISTS agents (
    slug TEXT NOT NULL,
    role TEXT NOT NULL,
    pid INTEGER,
    seen INTEGER NOT NULL,
    PRIMARY KEY (slug, role)
);
";

const IDLE_STALE: i64 = 1800;

#[derive(Clone, Copy, PartialEq, ValueEnum)]
pub enum Verdict {
    Approve,
    Changes,
    Comment,
}

impl Verdict {
    pub fn label(self) -> &'static str {
        match self {
            Verdict::Approve => "approved",
            Verdict::Changes => "requested changes",
            Verdict::Comment => "commented",
        }
    }
}

#[derive(Clone, Copy, PartialEq, ValueEnum)]
pub enum Lifecycle {
    Open,
    Merged,
    Closed,
}

#[derive(Clone, Copy, PartialEq, ValueEnum)]
pub enum Decision {
    Uphold,
    Dismiss,
    Escalate,
}

#[derive(Clone, Copy, PartialEq)]
pub enum MessageKind {
    Comment,
    Reply,
    Dispute,
    Ruling(Decision),
}

impl MessageKind {
    fn key(self) -> String {
        match self {
            MessageKind::Comment => "comment".to_string(),
            MessageKind::Reply => "reply".to_string(),
            MessageKind::Dispute => "dispute".to_string(),
            MessageKind::Ruling(d) => key(d),
        }
    }

    fn parse(text: &str) -> Result<MessageKind> {
        match text {
            "comment" => Ok(MessageKind::Comment),
            "reply" => Ok(MessageKind::Reply),
            "dispute" => Ok(MessageKind::Dispute),
            _ => parse(text).map(MessageKind::Ruling),
        }
    }
}

#[derive(Clone, PartialEq)]
pub struct Message {
    pub author: String,
    pub kind: MessageKind,
    pub body: String,
    pub at: i64,
}

#[derive(Clone)]
pub struct Comment {
    pub file: String,
    pub start: u32,
    pub end: u32,
    pub body: String,
}

#[derive(Clone, Copy, PartialEq)]
pub enum ThreadState {
    Open,
    Waiting,
    Disputed,
    Escalated,
    Resolved,
    Dismissed,
    Note,
}

impl ThreadState {
    pub fn unsettled(self) -> bool {
        matches!(self, ThreadState::Open | ThreadState::Waiting | ThreadState::Disputed | ThreadState::Escalated)
    }

    pub fn label(self) -> &'static str {
        match self {
            ThreadState::Open => "open",
            ThreadState::Waiting => "awaiting reviewer",
            ThreadState::Disputed => "disputed",
            ThreadState::Escalated => "escalated",
            ThreadState::Resolved => "resolved",
            ThreadState::Dismissed => "resolved: implementer was right",
            ThreadState::Note => "comment",
        }
    }
}

#[derive(Clone, PartialEq)]
pub struct Thread {
    pub id: i64,
    pub reviewer: String,
    pub sha: String,
    pub file: String,
    pub start: u32,
    pub end: u32,
    pub messages: Vec<Message>,
    pub resolved_by: Option<String>,
    pub state: ThreadState,
}

#[derive(Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Action {
    Implement,
    Reattach,
    Sync,
    Fix,
    Review,
    Respond,
    Rule,
    Answer,
}

#[derive(PartialEq, Serialize)]
pub struct Work {
    pub commit: String,
    pub base: String,
    pub role: String,
    pub action: Action,
    pub threads: Vec<i64>,
}

#[derive(Clone, PartialEq, Serialize)]
pub struct Waiting {
    pub thread: i64,
    pub reviewer: String,
}

#[derive(Clone, PartialEq, Serialize)]
pub struct Status {
    pub worktree: PathBuf,
    pub implemented: bool,
    pub awaiting: Vec<String>,
    pub open: Vec<i64>,
    pub waiting: Vec<Waiting>,
    pub disputed: Vec<i64>,
    pub escalated: Vec<i64>,
    pub awaiting_you: bool,
    pub conflicting: bool,
    pub detached: bool,
    pub comments: Vec<i64>,
}

#[derive(Clone, Copy, PartialEq)]
pub enum Phase {
    Unimplemented,
    Detached,
    Conflicting,
    Escalated,
    Disputed,
    AwaitingReview,
    Discussing,
    ChangesRequested,
    AwaitingYou,
    Approved,
}

impl Status {
    pub fn phase(&self) -> Phase {
        match () {
            _ if !self.implemented => Phase::Unimplemented,
            _ if self.detached => Phase::Detached,
            _ if self.conflicting => Phase::Conflicting,
            _ if !self.escalated.is_empty() => Phase::Escalated,
            _ if !self.disputed.is_empty() => Phase::Disputed,
            _ if !self.open.is_empty() => Phase::ChangesRequested,
            _ if !self.waiting.is_empty() => Phase::Discussing,
            _ if !self.awaiting.is_empty() => Phase::AwaitingReview,
            _ if self.awaiting_you => Phase::AwaitingYou,
            _ => Phase::Approved,
        }
    }
}

impl Phase {
    pub fn label(self) -> &'static str {
        match self {
            Phase::Unimplemented => "unimplemented",
            Phase::Detached => "off branch",
            Phase::Conflicting => "conflicting",
            Phase::Escalated => "escalated",
            Phase::Disputed => "disputed",
            Phase::AwaitingReview => "awaiting review",
            Phase::Discussing => "discussing",
            Phase::ChangesRequested => "changes requested",
            Phase::AwaitingYou => "awaiting your review",
            Phase::Approved => "approved",
        }
    }
}

#[derive(Clone, PartialEq)]
pub enum Event {
    Commit { sha: String, subject: String, at: i64 },
    Review { sha: String, reviewer: String, verdict: Verdict, threads: Vec<usize>, at: i64 },
}

impl Event {
    fn at(&self) -> i64 {
        match self {
            Event::Commit { at, .. } | Event::Review { at, .. } => *at,
        }
    }
}

#[derive(Clone, PartialEq)]
pub enum Line {
    Hunk(String),
    Added { new: u32, text: String },
    Removed { old: u32, text: String },
    Context { old: u32, new: u32, text: String },
}

#[derive(Clone, PartialEq)]
pub struct DiffLine {
    pub line: Line,
    pub threads: Vec<usize>,
}

#[derive(Clone, PartialEq)]
pub struct FileDiff {
    pub path: String,
    pub added: usize,
    pub removed: usize,
    pub lines: Vec<DiffLine>,
}

#[derive(Clone, PartialEq)]
pub struct Pr {
    pub prompt: String,
    pub head: String,
    pub reviewers: Vec<(String, Option<Verdict>)>,
    pub threads: Vec<Thread>,
    pub timeline: Vec<Event>,
    pub files: Vec<FileDiff>,
}

struct ReviewRow {
    id: i64,
    sha: String,
    reviewer: String,
    verdict: Verdict,
    at: i64,
}

pub struct Gate {
    root: PathBuf,
    store: PathBuf,
    db: Connection,
}

fn now() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

fn key<T: ValueEnum>(value: T) -> String {
    value.to_possible_value().map(|v| v.get_name().to_string()).unwrap_or_default()
}

fn parse<T: ValueEnum>(text: &str) -> Result<T> {
    T::from_str(text, false).map_err(|_| format!("unknown value {text}"))
}

fn sql(e: rusqlite::Error) -> String {
    e.to_string()
}

impl Gate {
    pub fn new() -> Result<Gate> {
        let common = PathBuf::from(git(Path::new("."), &["rev-parse", "--path-format=absolute", "--git-common-dir"])?);
        let root = common.parent().ok_or("bare repositories are unsupported")?.to_path_buf();
        let store = common.join("gate");
        fs::create_dir_all(&store).map_err(|e| e.to_string())?;
        let db = Connection::open(store.join("gate.db")).map_err(sql)?;
        db.busy_timeout(Duration::from_secs(10)).map_err(sql)?;
        db.execute_batch(SCHEMA).map_err(sql)?;
        let has_state: bool = db.prepare("SELECT 1 FROM pragma_table_info('prs') WHERE name = 'state'").and_then(|mut q| q.exists([])).map_err(sql)?;
        if !has_state {
            db.execute("ALTER TABLE prs ADD COLUMN state TEXT NOT NULL DEFAULT 'open'", []).map_err(sql)?;
        }
        Ok(Gate { root, store, db })
    }

    fn worktree(&self, slug: &str) -> PathBuf {
        self.store.join(slug)
    }

    fn base(&self, slug: &str) -> Result<(String, String)> {
        match self.lifecycle(slug)? {
            None => return Err(format!("no PR {slug}")),
            Some(Lifecycle::Open) => {}
            Some(state) => return Err(format!("{slug} is {}", key(state))),
        }
        self.db.query_row("SELECT base, prompt FROM prs WHERE slug = ?1", [slug], |r| Ok((r.get(0)?, r.get(1)?))).map_err(sql)
    }

    pub fn lifecycle(&self, slug: &str) -> Result<Option<Lifecycle>> {
        let state: Option<String> = self.db.query_row("SELECT state FROM prs WHERE slug = ?1", [slug], |r| r.get(0)).optional().map_err(sql)?;
        state.map(|s| parse(&s)).transpose()
    }

    fn head(&self, slug: &str) -> Result<String> {
        git(&self.root, &["rev-parse", "--short=12", &format!("gate/{slug}")])
    }

    pub fn rules(&self) -> Result<Vec<String>> {
        let home = std::env::var("HOME").map_err(|_| "HOME is unset")?;
        let dirs = [PathBuf::from(home).join(".claude/skills"), self.root.join(".claude/skills")];
        let mut rules: Vec<String> = dirs
            .iter()
            .flat_map(fs::read_dir)
            .flatten()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with("arch-"))
            .collect();
        if rules.is_empty() {
            return Err(format!("no arch-* reviewers in {} or {}", dirs[0].display(), dirs[1].display()));
        }
        rules.sort();
        rules.dedup();
        Ok(rules)
    }

    fn reviews(&self, slug: &str) -> Result<Vec<ReviewRow>> {
        let mut statement = self.db.prepare("SELECT id, sha, reviewer, verdict, at FROM reviews WHERE slug = ?1 ORDER BY id").map_err(sql)?;
        let rows = statement
            .query_map([slug], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get::<_, String>(3)?, r.get(4)?)))
            .map_err(sql)?;
        rows.map(|row| {
            let (id, sha, reviewer, verdict, at) = row.map_err(sql)?;
            Ok(ReviewRow { id, sha, reviewer, verdict: parse(&verdict)?, at })
        })
        .collect()
    }

    fn threads(&self, slug: &str) -> Result<Vec<Thread>> {
        let mut statement = self
            .db
            .prepare(
                "SELECT t.id, r.reviewer, r.sha, r.verdict, t.file, t.start_line, t.end_line, t.resolved_by
                 FROM threads t JOIN reviews r ON r.id = t.review WHERE r.slug = ?1 ORDER BY t.id",
            )
            .map_err(sql)?;
        let rows: Vec<(i64, String, String, String, String, u32, u32, Option<String>)> = statement
            .query_map([slug], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?, r.get(7)?)))
            .map_err(sql)?
            .collect::<std::result::Result<_, _>>()
            .map_err(sql)?;
        let mut messages = self.db.prepare("SELECT author, kind, body, at FROM messages WHERE thread = ?1 ORDER BY id").map_err(sql)?;
        let mut threads = Vec::new();
        for (id, reviewer, sha, verdict, file, start, end, resolved_by) in rows {
            let messages: Vec<Message> = messages
                .query_map([id], |r| Ok((r.get(0)?, r.get::<_, String>(1)?, r.get(2)?, r.get(3)?)))
                .map_err(sql)?
                .map(|row| {
                    let (author, kind, body, at) = row.map_err(sql)?;
                    Ok(Message { author, kind: MessageKind::parse(&kind)?, body, at })
                })
                .collect::<Result<_>>()?;
            let state = thread_state(&reviewer, parse(&verdict)?, &messages, resolved_by.is_some());
            threads.push(Thread { id, reviewer, sha, file, start, end, messages, resolved_by, state });
        }
        Ok(threads)
    }

    fn thread(&self, id: i64) -> Result<(String, Thread)> {
        let slug: String = self
            .db
            .query_row("SELECT r.slug FROM threads t JOIN reviews r ON r.id = t.review WHERE t.id = ?1", [id], |r| r.get(0))
            .optional()
            .map_err(sql)?
            .ok_or_else(|| format!("no thread {id}"))?;
        let thread = self.threads(&slug)?.into_iter().find(|t| t.id == id).ok_or_else(|| format!("no thread {id}"))?;
        Ok((slug, thread))
    }

    fn post(&self, thread: i64, author: &str, kind: MessageKind, body: &str) -> Result<()> {
        if body.trim().is_empty() {
            return Err("a message needs a body".to_string());
        }
        self.db
            .execute("INSERT INTO messages (thread, author, kind, body, at) VALUES (?1, ?2, ?3, ?4, ?5)", params![thread, author, kind.key(), body, now()])
            .map(|_| ())
            .map_err(sql)
    }

    pub fn slugs(&self) -> Result<Vec<(String, Lifecycle)>> {
        let mut statement = self.db.prepare("SELECT slug, state FROM prs ORDER BY state != 'open', slug").map_err(sql)?;
        let rows: Vec<(String, String)> = statement.query_map([], |r| Ok((r.get(0)?, r.get(1)?))).map_err(sql)?.collect::<std::result::Result<_, _>>().map_err(sql)?;
        rows.into_iter().map(|(slug, state)| Ok((slug, parse(&state)?))).collect()
    }

    pub fn open(&self, slug: &str, prompt: &str) -> Result<PathBuf> {
        let tree = self.worktree(slug);
        match self.lifecycle(slug)? {
            Some(Lifecycle::Open) => {}
            Some(state) => return Err(format!("{slug} was {}; open the task under a new slug", key(state))),
            None => {
                let base = git(&self.root, &["rev-parse", "--verify", "HEAD"]).map_err(|_| "the repository has no commits yet; make an initial commit first".to_string())?;
                git(&self.root, &["worktree", "add", "-b", &format!("gate/{slug}"), &tree.to_string_lossy(), &base])?;
                self.db.execute("INSERT INTO prs (slug, base, prompt) VALUES (?1, ?2, ?3)", params![slug, base, prompt]).map_err(sql)?;
            }
        }
        Ok(tree)
    }

    pub fn status(&self, slug: &str) -> Result<Status> {
        let (base, _) = self.base(slug)?;
        let head = self.head(slug)?;
        let reviews = self.reviews(slug)?;
        let latest = |reviewer: &str| reviews.iter().rev().find(|r| r.sha == head && r.reviewer == reviewer).map(|r| r.verdict);
        let threads = self.threads(slug)?;
        let unsettled = |reviewer: &str| {
            threads.iter().any(|t| {
                t.reviewer == reviewer && t.state.unsettled()
            })
        };
        let awaiting = self
            .rules()?
            .into_iter()
            .filter(|rule| !unsettled(rule) && latest(rule) != Some(Verdict::Approve))
            .collect();
        let mut status = Status {
            worktree: self.worktree(slug),
            implemented: !base.starts_with(&head),
            awaiting,
            open: Vec::new(),
            waiting: Vec::new(),
            disputed: Vec::new(),
            escalated: Vec::new(),
            awaiting_you: latest(YOU) != Some(Verdict::Approve),
            conflicting: !base.starts_with(&head) && self.conflicts(slug)?,
            detached: git(&self.worktree(slug), &["symbolic-ref", "-q", "--short", "HEAD"]).ok().as_deref() != Some(&format!("gate/{slug}")),
            comments: Vec::new(),
        };
        for thread in threads {
            match thread.state {
                ThreadState::Open => status.open.push(thread.id),
                ThreadState::Waiting => status.waiting.push(Waiting { thread: thread.id, reviewer: thread.reviewer }),
                ThreadState::Disputed => status.disputed.push(thread.id),
                ThreadState::Escalated => status.escalated.push(thread.id),
                ThreadState::Note if thread.messages.last().is_some_and(|m| m.author != IMPLEMENTER) => status.comments.push(thread.id),
                ThreadState::Resolved | ThreadState::Dismissed | ThreadState::Note => {}
            }
        }
        Ok(status)
    }

    pub fn pending(&self, slug: &str) -> Result<Vec<Work>> {
        let status = self.status(slug)?;
        let commit = self.head(slug)?;
        let base = git(&self.root, &["rev-parse", "--abbrev-ref", "HEAD"])?;
        let mut work = Vec::new();
        let mut push = |role: &str, action, threads| work.push(Work { commit: commit.clone(), base: base.clone(), role: role.to_string(), action, threads });
        if status.detached {
            push(IMPLEMENTER, Action::Reattach, Vec::new());
            return Ok(work);
        }
        if !status.implemented {
            push(IMPLEMENTER, Action::Implement, Vec::new());
            return Ok(work);
        }
        if status.conflicting {
            push(IMPLEMENTER, Action::Sync, Vec::new());
        }
        if !status.open.is_empty() {
            push(IMPLEMENTER, Action::Fix, status.open.clone());
        }
        if !status.comments.is_empty() {
            push(IMPLEMENTER, Action::Answer, status.comments.clone());
        }
        for reviewer in &status.awaiting {
            push(reviewer, Action::Review, Vec::new());
        }
        let mut reviewers: Vec<&str> = status.waiting.iter().map(|w| w.reviewer.as_str()).filter(|r| *r != YOU).collect();
        reviewers.dedup();
        for reviewer in reviewers {
            push(reviewer, Action::Respond, status.waiting.iter().filter(|w| w.reviewer == reviewer).map(|w| w.thread).collect());
        }
        if !status.disputed.is_empty() {
            push(ARBITER, Action::Rule, status.disputed.clone());
        }
        Ok(work)
    }

    fn conflicts(&self, slug: &str) -> Result<bool> {
        let out = Command::new("git")
            .current_dir(&self.root)
            .args(["merge-tree", "--write-tree", "--name-only", "HEAD", &format!("gate/{slug}")])
            .output()
            .map_err(|e| e.to_string())?;
        match out.status.code() {
            Some(0) => Ok(false),
            Some(1) => Ok(true),
            _ => Err(String::from_utf8_lossy(&out.stderr).trim_end().to_string()),
        }
    }

    pub fn register(&self, slug: &str, role: &str, pid: Option<u32>) -> Result<()> {
        self.db
            .execute("INSERT OR REPLACE INTO agents (slug, role, pid, seen) VALUES (?1, ?2, ?3, ?4)", params![slug, role, pid, now()])
            .map(|_| ())
            .map_err(sql)
    }

    pub fn watched(&self) -> Vec<(PathBuf, notify::RecursiveMode)> {
        let common = self.store.parent().unwrap_or(&self.store);
        vec![
            (self.store.clone(), notify::RecursiveMode::NonRecursive),
            (common.join("refs"), notify::RecursiveMode::Recursive),
            (common.join("HEAD"), notify::RecursiveMode::NonRecursive),
            (common.join("packed-refs"), notify::RecursiveMode::NonRecursive),
        ]
    }

    pub fn missing(&self, slug: &str) -> Result<Vec<String>> {
        let mut missing = Vec::new();
        for work in self.pending(slug)?.into_iter().filter(|w| w.role != IMPLEMENTER) {
            let agent: Option<(Option<u32>, i64)> = self
                .db
                .query_row("SELECT pid, seen FROM agents WHERE slug = ?1 AND role = ?2", params![slug, work.role], |r| Ok((r.get(0)?, r.get(1)?)))
                .optional()
                .map_err(sql)?;
            let alive = match agent {
                None => false,
                Some((Some(pid), _)) => Command::new("kill").args(["-0", &pid.to_string()]).status().is_ok_and(|s| s.success()),
                Some((None, seen)) => now() - seen <= IDLE_STALE,
            };
            if !alive {
                missing.push(work.role);
            }
        }
        Ok(missing)
    }

    pub fn pr(&self, slug: &str) -> Result<Pr> {
        let prompt = self.db.query_row("SELECT prompt FROM prs WHERE slug = ?1", [slug], |r| r.get(0)).optional().map_err(sql)?.ok_or_else(|| format!("no PR {slug}"))?;
        let head = self.head(slug)?;
        let reviews = self.reviews(slug)?;
        let threads = self.threads(slug)?;
        let reviewers = self
            .rules()?
            .into_iter()
            .chain(std::iter::once(YOU.to_string()))
            .map(|reviewer| {
                let verdict = reviews.iter().rev().find(|r| r.sha == head && r.reviewer == reviewer).map(|r| r.verdict);
                (reviewer, verdict)
            })
            .collect();

        let log = git(&self.root, &["log", "--reverse", "--abbrev=12", "--format=%h%x09%ct%x09%s", &format!("HEAD..gate/{slug}")])?;
        let mut timeline: Vec<Event> = log
            .lines()
            .filter_map(|l| {
                let mut parts = l.splitn(3, '\t');
                let (sha, at, subject) = (parts.next()?, parts.next()?.parse().ok()?, parts.next()?);
                Some(Event::Commit { sha: sha.to_string(), subject: subject.to_string(), at })
            })
            .collect();
        let review_threads = self.db.prepare("SELECT id FROM threads WHERE review = ?1 ORDER BY id");
        let mut review_threads = review_threads.map_err(sql)?;
        for review in &reviews {
            let ids: Vec<i64> = review_threads.query_map([review.id], |r| r.get(0)).map_err(sql)?.collect::<std::result::Result<_, _>>().map_err(sql)?;
            let indices = ids.iter().filter_map(|id| threads.iter().position(|t| t.id == *id)).collect();
            timeline.push(Event::Review { sha: review.sha.clone(), reviewer: review.reviewer.clone(), verdict: review.verdict, threads: indices, at: review.at });
        }
        timeline.sort_by_key(Event::at);

        let mut files: Vec<FileDiff> = Vec::new();
        let (mut old, mut new, mut in_hunk) = (0, 0, false);
        for l in git(&self.root, &["diff", &format!("HEAD...gate/{slug}")])?.lines() {
            if let Some(header) = l.strip_prefix("diff --git ") {
                let path = header.rsplit_once(" b/").map_or(header, |(_, p)| p).to_string();
                files.push(FileDiff { path, added: 0, removed: 0, lines: Vec::new() });
                in_hunk = false;
                continue;
            }
            let Some(file) = files.last_mut() else { continue };
            let line = if let Some(h) = l.strip_prefix("@@ -") {
                let start = |s: &str| s.split([',', ' ']).next().and_then(|n| n.parse().ok()).unwrap_or(0);
                old = start(h);
                new = start(h.split(" +").nth(1).unwrap_or(""));
                in_hunk = true;
                Line::Hunk(l.to_string())
            } else if !in_hunk {
                continue;
            } else if let Some(text) = l.strip_prefix('+') {
                new += 1;
                file.added += 1;
                Line::Added { new: new - 1, text: text.to_string() }
            } else if let Some(text) = l.strip_prefix('-') {
                old += 1;
                file.removed += 1;
                Line::Removed { old: old - 1, text: text.to_string() }
            } else if let Some(text) = l.strip_prefix(' ') {
                (old, new) = (old + 1, new + 1);
                Line::Context { old: old - 1, new: new - 1, text: text.to_string() }
            } else {
                continue;
            };
            let on_line = match line {
                Line::Added { new, .. } | Line::Context { new, .. } => (0..threads.len())
                    .filter(|&i| threads[i].file == file.path && threads[i].end == new && (threads[i].sha == head || !matches!(threads[i].state, ThreadState::Resolved | ThreadState::Dismissed)))
                    .collect(),
                _ => Vec::new(),
            };
            file.lines.push(DiffLine { line, threads: on_line });
        }
        Ok(Pr { prompt, head, reviewers, threads, timeline, files })
    }

    pub fn review(&self, slug: &str, reviewer: &str, verdict: Verdict, comments: Vec<Comment>) -> Result<()> {
        if !self.rules()?.iter().any(|r| r == reviewer) {
            return Err(format!("no reviewer {reviewer}"));
        }
        if verdict == Verdict::Comment {
            return Err("reviewers approve or request changes".to_string());
        }
        let unsettled: Vec<String> = self
            .threads(slug)?
            .into_iter()
            .filter(|t| t.reviewer == reviewer && t.state.unsettled())
            .map(|t| t.id.to_string())
            .collect();
        if !unsettled.is_empty() {
            return Err(format!("{reviewer} still has unsettled threads {}; settle them before reviewing again", unsettled.join(", ")));
        }
        self.write_review(slug, reviewer, verdict, comments)
    }

    pub fn submit_review(&self, slug: &str, verdict: Verdict, comments: Vec<Comment>) -> Result<()> {
        self.write_review(slug, YOU, verdict, comments)
    }

    fn write_review(&self, slug: &str, reviewer: &str, verdict: Verdict, comments: Vec<Comment>) -> Result<()> {
        match verdict {
            Verdict::Approve if !comments.is_empty() => return Err("an approval can't carry comments".to_string()),
            Verdict::Changes | Verdict::Comment if comments.is_empty() => return Err("this review needs at least one comment".to_string()),
            _ => {}
        }
        if let Some(c) = comments.iter().find(|c| c.start == 0 || c.end < c.start) {
            return Err(format!("{}: invalid line range {}-{}", c.file, c.start, c.end));
        }
        let (base, _) = self.base(slug)?;
        let head = self.head(slug)?;
        if base.starts_with(&head) {
            return Err(format!("{slug} has no commits to review"));
        }
        let at = now();
        self.db
            .execute("INSERT INTO reviews (slug, sha, reviewer, verdict, at) VALUES (?1, ?2, ?3, ?4, ?5)", params![slug, head, reviewer, key(verdict), at])
            .map_err(sql)?;
        let review = self.db.last_insert_rowid();
        for c in comments {
            self.db.execute("INSERT INTO threads (review, file, start_line, end_line) VALUES (?1, ?2, ?3, ?4)", params![review, c.file, c.start, c.end]).map_err(sql)?;
            self.post(self.db.last_insert_rowid(), reviewer, MessageKind::Comment, &c.body)?;
        }
        Ok(())
    }

    pub fn reply(&self, thread: i64, author: &str, body: &str) -> Result<()> {
        let (_, t) = self.thread(thread)?;
        if author != YOU && author != IMPLEMENTER && !self.rules()?.iter().any(|r| r == author) {
            return Err(format!("unknown author {author}"));
        }
        if t.resolved_by.is_some() {
            return Err(format!("thread {thread} is resolved"));
        }
        self.post(thread, author, MessageKind::Reply, body)
    }

    pub fn dispute(&self, thread: i64, argument: &str) -> Result<()> {
        let (_, t) = self.thread(thread)?;
        if t.state != ThreadState::Open {
            return Err(format!("thread {thread} is {}, not open", t.state.label()));
        }
        self.post(thread, IMPLEMENTER, MessageKind::Dispute, argument)
    }

    pub fn decide(&self, thread: i64, by: &str, decision: Decision, reason: &str) -> Result<()> {
        let (_, t) = self.thread(thread)?;
        match (by, t.state) {
            (ARBITER, ThreadState::Disputed) if decision == Decision::Dismiss && t.reviewer == YOU => {
                Err("the arbiter can't dismiss your comments; uphold or escalate".to_string())
            }
            (ARBITER, ThreadState::Disputed) | (YOU, ThreadState::Disputed | ThreadState::Escalated) => {
                self.post(thread, by, MessageKind::Ruling(decision), reason)
            }
            _ => Err(format!("{by} can't decide thread {thread} while it is {}", t.state.label())),
        }
    }

    pub fn resolve(&self, thread: i64, by: &str) -> Result<()> {
        let (_, t) = self.thread(thread)?;
        if by != YOU && by != t.reviewer {
            return Err(format!("only {} or you can resolve thread {thread}", t.reviewer));
        }
        if matches!(t.state, ThreadState::Disputed | ThreadState::Escalated) {
            return Err(format!("thread {thread} is {}; it needs a ruling before it can close", t.state.label()));
        }
        self.db.execute("UPDATE threads SET resolved_by = ?1 WHERE id = ?2", params![by, thread]).map(|_| ()).map_err(sql)
    }

    pub fn merge(&self, slug: &str, message: &str) -> Result<()> {
        if message.trim().is_empty() {
            return Err("a merge needs a commit message".to_string());
        }
        if self.status(slug)?.phase() != Phase::Approved {
            return Err(format!("{slug} is not approved"));
        }
        if !git(&self.root, &["status", "--porcelain"])?.is_empty() {
            return Err("the current checkout has uncommitted changes".to_string());
        }
        let tree = self.worktree(slug);
        if !git(&tree, &["status", "--porcelain"])?.is_empty() {
            return Err(format!("{slug}'s worktree has uncommitted changes"));
        }
        if let Err(e) = git(&self.root, &["merge", "--squash", &format!("gate/{slug}")]) {
            git(&self.root, &["reset", "--merge"])?;
            return Err(e);
        }
        git(&self.root, &["commit", "-m", message])?;
        self.finish(slug, Lifecycle::Merged)?;
        git(&self.root, &["worktree", "remove", "--force", &tree.to_string_lossy()]).map(|_| ())
    }

    pub fn close(&self, slug: &str) -> Result<()> {
        self.finish(slug, Lifecycle::Closed)
    }

    fn finish(&self, slug: &str, end: Lifecycle) -> Result<()> {
        self.db.execute("UPDATE prs SET state = ?1 WHERE slug = ?2", params![key(end), slug]).map(|_| ()).map_err(sql)
    }
}

fn thread_state(reviewer: &str, verdict: Verdict, messages: &[Message], resolved: bool) -> ThreadState {
    let ruling = messages.iter().rev().find_map(|m| match m.kind {
        MessageKind::Dispute => Some(None),
        MessageKind::Ruling(d) => Some(Some(d)),
        _ => None,
    });
    match ruling {
        _ if resolved => ThreadState::Resolved,
        Some(None) => ThreadState::Disputed,
        Some(Some(Decision::Escalate)) => ThreadState::Escalated,
        Some(Some(Decision::Dismiss)) => ThreadState::Dismissed,
        _ if verdict == Verdict::Comment => ThreadState::Note,
        _ => match messages.last() {
            Some(m) if m.kind == MessageKind::Reply && m.author != reviewer => ThreadState::Waiting,
            _ => ThreadState::Open,
        },
    }
}

fn git(dir: &Path, args: &[&str]) -> Result<String> {
    let out = Command::new("git").current_dir(dir).args(args).output().map_err(|e| e.to_string())?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        let message = if stderr.trim().is_empty() { String::from_utf8_lossy(&out.stdout) } else { stderr };
        return Err(message.trim_end().to_string());
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim_end().to_string())
}
