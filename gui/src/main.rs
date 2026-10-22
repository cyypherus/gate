use gate::{ARBITER, Comment, Decision, DiffLine, IMPLEMENTER, MessageKind, YOU, Event, FileDiff, Gate, Lifecycle, Line, Phase, Pr, Status, Thread, ThreadState, Verdict};
use haven::winit::WinitApp;
use haven::*;
use notify::Watcher;
use syntect::easy::HighlightLines;
use syntect::highlighting::Theme;
use syntect::parsing::SyntaxSet;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::{process, thread};

#[derive(Clone, Copy, PartialEq)]
enum Tab {
    Conversation,
    Files,
}

#[derive(Clone, PartialEq)]
struct Pull {
    slug: String,
    standing: Standing,
    pr: gate::Result<Pr>,
    colors: Vec<Vec<Spans>>,
}

#[derive(Clone, PartialEq)]
enum Standing {
    Open(gate::Result<Status>),
    Merged,
    Closed,
}

impl Pull {
    fn open(&self) -> bool {
        matches!(self.standing, Standing::Open(_))
    }

    fn label(&self) -> (String, Color) {
        match &self.standing {
            Standing::Open(Ok(status)) => (status.phase().label().to_string(), phase_color(status.phase())),
            Standing::Open(Err(e)) => (e.clone(), RED),
            Standing::Merged => ("merged".to_string(), MERGED),
            Standing::Closed => ("closed".to_string(), MUTED),
        }
    }
}

type Spans = Vec<(String, [u8; 3])>;

struct Highlighter {
    syntaxes: SyntaxSet,
    theme: Theme,
}

impl Highlighter {
    fn new() -> Highlighter {
        let theme = two_face::theme::extra()[two_face::theme::EmbeddedThemeName::OneHalfDark].clone();
        Highlighter { syntaxes: two_face::syntax::extra_newlines(), theme }
    }

    fn files(&self, pr: &Pr) -> Vec<Vec<Spans>> {
        pr.files
            .iter()
            .map(|file| {
                let extension = file.path.rsplit_once('.').map_or("", |(_, e)| e);
                let syntax = self.syntaxes.find_syntax_by_extension(extension).unwrap_or_else(|| self.syntaxes.find_syntax_plain_text());
                let mut lines = HighlightLines::new(syntax, &self.theme);
                file.lines
                    .iter()
                    .map(|l| {
                        let text = match &l.line {
                            Line::Hunk(_) => return Vec::new(),
                            Line::Added { text, .. } | Line::Removed { text, .. } | Line::Context { text, .. } => format!("{text}\n"),
                        };
                        lines
                            .highlight_line(&text, &self.syntaxes)
                            .unwrap_or_default()
                            .into_iter()
                            .map(|(style, piece)| (piece.trim_end_matches('\n').replace('\t', "    "), [style.foreground.r, style.foreground.g, style.foreground.b]))
                            .filter(|(piece, _)| !piece.is_empty())
                            .collect()
                    })
                    .collect()
            })
            .collect()
    }
}

struct Draft {
    id: i64,
    text: TextState,
    reply: ButtonState,
    resolve: ButtonState,
    uphold: ButtonState,
    dismiss: ButtonState,
}

#[derive(Clone, Copy, PartialEq)]
enum ThreadAction {
    Reply,
    Resolve,
    Uphold,
    Dismiss,
}

impl ThreadAction {
    fn button(self, draft: &Draft) -> &ButtonState {
        match self {
            ThreadAction::Reply => &draft.reply,
            ThreadAction::Resolve => &draft.resolve,
            ThreadAction::Uphold => &draft.uphold,
            ThreadAction::Dismiss => &draft.dismiss,
        }
    }

    fn button_mut(self, draft: &mut Draft) -> &mut ButtonState {
        match self {
            ThreadAction::Reply => &mut draft.reply,
            ThreadAction::Resolve => &mut draft.resolve,
            ThreadAction::Uphold => &mut draft.uphold,
            ThreadAction::Dismiss => &mut draft.dismiss,
        }
    }
}

struct FileView {
    slug: String,
    path: String,
    collapsed: bool,
    viewed: Option<String>,
    toggle: ButtonState,
    check: ButtonState,
}

#[derive(Clone, Copy, PartialEq)]
enum Confirm {
    Merge,
    Close,
}

#[derive(Clone)]
enum Request {
    Thread { thread: i64, action: ThreadAction, text: String },
    Review { slug: String, verdict: Verdict, comments: Vec<Comment> },
    Merge { slug: String, message: String },
    Close { slug: String },
}

enum Job {
    Start(PaneWaker),
    Changed,
    Run(Request),
}

enum Update {
    Pulls(Vec<Pull>),
    Done(gate::Result<()>),
}

struct State {
    jobs: Sender<Job>,
    updates: Receiver<Update>,
    running: Option<Request>,
    pulls: Option<Vec<Pull>>,
    rows: Vec<ButtonState>,
    selected: Option<String>,
    tab: Tab,
    conversation_tab: ButtonState,
    files_tab: ButtonState,
    drafts: Vec<Draft>,
    file_views: Vec<FileView>,
    merge_button: ButtonState,
    close_button: ButtonState,
    confirming: Option<Confirm>,
    merge_message: TextState,
    cancel_button: ButtonState,
    pending: Vec<Comment>,
    composing: Option<(String, u32, u32)>,
    composer_text: TextState,
    add_comment_button: ButtonState,
    cancel_comment_button: ButtonState,
    comment_button: ButtonState,
    changes_button: ButtonState,
    approve_button: ButtonState,
    diff_size: u32,
    expanded: Vec<(String, i64)>,
    unfolded: Vec<i64>,
    finished_hidden: bool,
    finished_toggle: ButtonState,
    notice: Option<String>,
    repo: String,
}

enum Item<'a> {
    Decision(&'a Thread),
    Commit { sha: &'a str, subject: &'a str, at: i64 },
    Review { reviewer: &'a str, verdict: Verdict, sha: &'a str, at: i64, stale: bool },
    Thread(&'a Thread),
    File(&'a FileDiff, usize),
    Line(&'a str, &'a DiffLine, &'a [(String, [u8; 3])]),
    Pending(&'a Comment),
    Composer,
    Merge(&'a Pull),
    Fold { reviewer: &'a str, at: i64, count: usize, open: bool },
}

const BACKGROUND: Color = Color::from_rgb8(8, 9, 11);
const PANEL: Color = Color::from_rgb8(11, 12, 15);
const CARD: Color = Color::from_rgb8(14, 16, 19);
const HOVER: Color = Color::from_rgb8(21, 23, 29);
const BORDER: Color = Color::from_rgb8(26, 29, 34);
const EDGE: Color = Color::from_rgb8(38, 42, 50);
const TEXT: Color = Color::from_rgb8(232, 234, 237);
const BODY: Color = Color::from_rgb8(200, 205, 212);
const MUTED: Color = Color::from_rgb8(154, 161, 172);
const FAINT: Color = Color::from_rgb8(124, 131, 142);
const ACCENT: Color = Color::from_rgb8(139, 156, 255);
const RED: Color = Color::from_rgb8(248, 113, 113);
const GREEN: Color = Color::from_rgb8(52, 211, 153);
const YELLOW: Color = Color::from_rgb8(245, 165, 36);
const MAGENTA: Color = Color::from_rgb8(232, 121, 249);
const PURPLE: Color = Color::from_rgb8(169, 181, 255);
const MERGED: Color = Color::from_rgb8(109, 91, 208);
const CYAN: Color = Color::from_rgb8(94, 200, 230);
const SANS: &str = "Geist";
const MONO: &str = "Geist Mono";
const OPEN_CHEVRON: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24"><path d="m6 9 6 6 6-6" fill="none" stroke="#7c838e" stroke-width="2.4" stroke-linecap="round" stroke-linejoin="round"/></svg>"##;
const CLOSED_CHEVRON: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24"><path d="m9 6 6 6-6 6" fill="none" stroke="#7c838e" stroke-width="2.4" stroke-linecap="round" stroke-linejoin="round"/></svg>"##;

fn chevron<'a>(id: u64, open: bool, app: &mut PaneState) -> View<'a, State> {
    svg(id!(id), if open { OPEN_CHEVRON } else { CLOSED_CHEVRON }).finish(app).width(12.).height(12.)
}
const FONTS: [(&[u8], &str); 6] = [
    (include_bytes!("../fonts/Geist-Regular.ttf"), SANS),
    (include_bytes!("../fonts/Geist-Medium.ttf"), SANS),
    (include_bytes!("../fonts/Geist-SemiBold.ttf"), SANS),
    (include_bytes!("../fonts/Geist-Bold.ttf"), SANS),
    (include_bytes!("../fonts/GeistMono-Regular.ttf"), MONO),
    (include_bytes!("../fonts/GeistMono-Medium.ttf"), MONO),
];

fn text(id: u64, body: impl AsRef<str>) -> Text {
    haven::text(id, body).font_family(SANS)
}

fn rich_text(id: u64, spans: impl IntoIterator<Item = Span>) -> Text {
    haven::rich_text(id, spans).font_family(SANS)
}

fn text_field<'a>(id: u64, state: (&'a TextState, Binding<State, TextState>)) -> TextField<'a, State> {
    haven::text_field(id, state).font_family(SANS)
}
const STALE_ICON: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 40 40"><circle cx="20" cy="20" r="19" fill="#0f2a21" stroke="#3a3f49" stroke-width="2"/><path d="M12 20.5l5.5 5.5 11-12" fill="none" stroke="#7c838e" stroke-width="3.5" stroke-linecap="round" stroke-linejoin="round"/></svg>"##;
const APPROVED_ICON: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 40 40"><circle cx="20" cy="20" r="19" fill="#0f2a21" stroke="#1f5a43" stroke-width="2"/><path d="M12 20.5l5.5 5.5 11-12" fill="none" stroke="#34d399" stroke-width="3.5" stroke-linecap="round" stroke-linejoin="round"/></svg>"##;

fn phase_color(phase: Phase) -> Color {
    match phase {
        Phase::Implementing => MUTED,
        Phase::Detached | Phase::Conflicting => RED,
        Phase::Discussing => CYAN,
        Phase::Escalated => RED,
        Phase::Disputed => MAGENTA,
        Phase::AwaitingReview => CYAN,
        Phase::ChangesRequested => YELLOW,
        Phase::AwaitingYou => PURPLE,
        Phase::Approved => GREEN,
    }
}

fn thread_color(state: ThreadState) -> Color {
    match state {
        ThreadState::Open => YELLOW,
        ThreadState::Waiting => PURPLE,
        ThreadState::Disputed => MAGENTA,
        ThreadState::Escalated => RED,
        ThreadState::Resolved | ThreadState::Dismissed => GREEN,
        ThreadState::Note => CYAN,
    }
}

fn ago(at: i64) -> String {
    let now = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(at);
    match now.saturating_sub(at) {
        s if s < 60 => format!("{s}s ago"),
        s if s < 3600 => format!("{}m ago", s / 60),
        s if s < 86400 => format!("{}h ago", s / 3600),
        s => format!("{}d ago", s / 86400),
    }
}

impl State {
    fn new(jobs: Sender<Job>, updates: Receiver<Update>) -> State {
        State {
            jobs,
            updates,
            running: None,
            pulls: None,
            rows: Vec::new(),
            selected: None,
            tab: Tab::Conversation,
            conversation_tab: ButtonState::default(),
            files_tab: ButtonState::default(),
            drafts: Vec::new(),
            file_views: Vec::new(),
            merge_button: ButtonState::default(),
            close_button: ButtonState::default(),
            confirming: None,
            merge_message: TextState::new(""),
            cancel_button: ButtonState::default(),
            pending: Vec::new(),
            composing: None,
            composer_text: TextState::new(""),
            add_comment_button: ButtonState::default(),
            cancel_comment_button: ButtonState::default(),
            comment_button: ButtonState::default(),
            changes_button: ButtonState::default(),
            approve_button: ButtonState::default(),
            diff_size: DEFAULT_FONT_SIZE,
            expanded: Vec::new(),
            unfolded: Vec::new(),
            finished_hidden: false,
            finished_toggle: ButtonState::default(),
            notice: None,
            repo: std::env::current_dir().ok().and_then(|d| d.file_name().map(|n| n.to_string_lossy().into_owned())).unwrap_or_default(),
        }
    }

    fn selected(&self) -> Option<&Pull> {
        self.pulls.iter().flatten().find(|p| Some(&p.slug) == self.selected.as_ref())
    }
}

fn sync(state: &mut State) {
    state.rows.resize(state.rows.len().max(state.pulls.as_ref().map_or(0, Vec::len)), ButtonState::default());
    let Some(Ok(pr)) = state.selected().map(|p| &p.pr) else { return };
    let active: Vec<i64> = pr
        .threads
        .iter()
        .filter(|t| !matches!(t.state, ThreadState::Resolved | ThreadState::Dismissed))
        .map(|t| t.id)
        .collect();
    let files: Vec<(String, String)> = pr.files.iter().map(|f| (f.path.clone(), f.blob.clone())).collect();
    let slug = state.selected.clone().unwrap_or_default();
    for id in active {
        if !state.drafts.iter().any(|d| d.id == id) {
            let blank = ButtonState::default();
            state.drafts.push(Draft { id, text: TextState::new(""), reply: blank, resolve: blank, uphold: blank, dismiss: blank });
        }
    }
    for (path, blob) in files {
        match state.file_views.iter_mut().find(|v| v.slug == slug && v.path == path) {
            Some(view) if view.viewed.as_ref().is_some_and(|v| *v != blob) => (view.viewed, view.collapsed) = (None, false),
            Some(_) => {}
            None => state.file_views.push(FileView { slug: slug.clone(), path, collapsed: false, viewed: None, toggle: ButtonState::default(), check: ButtonState::default() }),
        }
    }
}

fn apply(state: &mut State, app: &mut PaneState) {
    let updates: Vec<Update> = state.updates.try_iter().collect();
    for update in updates {
        match update {
            Update::Pulls(pulls) => {
                state.pulls = Some(pulls);
                sync(state);
            }
            Update::Done(result) => {
                if let (Some(request), true) = (state.running.take(), result.is_ok()) {
                    match request {
                        Request::Thread { thread, .. } => {
                            if let Some(draft) = state.drafts.iter_mut().find(|d| d.id == thread) {
                                draft.text = TextState::new("");
                            }
                        }
                        Request::Review { .. } => state.pending.clear(),
                        Request::Merge { slug, .. } | Request::Close { slug } => {
                            if state.selected.as_ref() == Some(&slug) {
                                state.selected = None;
                                state.confirming = None;
                            }
                        }
                    }
                }
                state.notice = result.err();
            }
        }
    }
    app.redraw();
}

fn run(state: &mut State, request: Request) {
    if state.running.is_some() {
        return;
    }
    match state.jobs.send(Job::Run(request.clone())) {
        Ok(()) => state.running = Some(request),
        Err(_) => state.notice = Some("the gate worker stopped".to_string()),
    }
}

fn perform(gate: &Gate, request: &Request) -> gate::Result<()> {
    match request {
        Request::Thread { thread, action, text } => match action {
            ThreadAction::Reply => gate.reply(*thread, YOU, text),
            ThreadAction::Resolve if text.trim().is_empty() => gate.resolve(*thread, YOU),
            ThreadAction::Resolve => gate.reply(*thread, YOU, text).and_then(|_| gate.resolve(*thread, YOU)),
            ThreadAction::Uphold => gate.decide(*thread, YOU, Decision::Uphold, text),
            ThreadAction::Dismiss => gate.decide(*thread, YOU, Decision::Dismiss, text),
        },
        Request::Review { slug, verdict, comments } => gate.submit_review(slug, *verdict, comments.clone()),
        Request::Merge { slug, message } => gate.merge(slug, message),
        Request::Close { slug } => gate.close(slug),
    }
}

fn load(gate: &Gate, highlighter: &Highlighter, finished: &mut Vec<Pull>) -> Vec<Pull> {
    gate.slugs()
        .unwrap_or_default()
        .into_iter()
        .map(|(slug, lifecycle)| {
            if let Some(pull) = finished.iter().find(|p| p.slug == slug).filter(|_| !matches!(lifecycle, Lifecycle::Open(_))) {
                return pull.clone();
            }
            let pr = gate.pr(&slug);
            let colors = pr.as_ref().map(|pr| highlighter.files(pr)).unwrap_or_default();
            let standing = match lifecycle {
                Lifecycle::Open(_) => Standing::Open(gate.status(&slug)),
                Lifecycle::Merged => Standing::Merged,
                Lifecycle::Closed => Standing::Closed,
            };
            let pull = Pull { standing, pr, colors, slug };
            if !matches!(lifecycle, Lifecycle::Open(_)) {
                finished.push(pull.clone());
            }
            pull
        })
        .collect()
}

fn work(gate: Gate, jobs: Sender<Job>, inbox: Receiver<Job>, updates: Sender<Update>) {
    let Ok(Job::Start(waker)) = inbox.recv() else { return };
    let highlighter = Highlighter::new();
    let Ok(mut watcher) = notify::recommended_watcher(move |_| {
        jobs.send(Job::Changed).ok();
    }) else {
        return;
    };
    for (path, mode) in gate.watched() {
        watcher.watch(&path, mode).ok();
    }
    let mut last = None;
    let mut done = Vec::new();
    let mut finished = Vec::new();
    loop {
        let pulls = load(&gate, &highlighter, &mut finished);
        let mut sent = done.drain(..).map(Update::Done).collect::<Vec<_>>();
        if last.as_ref() != Some(&pulls) {
            sent.insert(0, Update::Pulls(pulls.clone()));
            last = Some(pulls);
        }
        if !sent.is_empty() {
            if sent.into_iter().any(|u| updates.send(u).is_err()) {
                return;
            }
            waker.wake();
        }
        let Ok(job) = inbox.recv() else { return };
        let mut batch = vec![job];
        if matches!(batch[0], Job::Changed) {
            thread::sleep(Duration::from_millis(100));
            batch.extend(inbox.try_iter());
        }
        for job in batch {
            if let Job::Run(request) = job {
                done.push(perform(&gate, &request));
            }
        }
    }
}

const WORKING: &str = "Working…";

#[derive(Clone, Copy, PartialEq)]
enum Activity {
    Idle,
    Working,
    Blocked,
}

fn activity(running: &Option<Request>, mine: impl Fn(&Request) -> bool) -> Activity {
    match running {
        None => Activity::Idle,
        Some(request) if mine(request) => Activity::Working,
        Some(_) => Activity::Blocked,
    }
}

fn action_button<'a>(
    id: u64,
    label: &str,
    color: Color,
    activity: Activity,
    binding: (&ButtonState, Binding<State, ButtonState>),
    on_click: impl Fn(&mut State, &mut PaneState) + 'static,
    app: &mut PaneState,
) -> View<'a, State> {
    let label_width = label.chars().count().max(WORKING.chars().count()) as f32 * 8. + 48.;
    let (label, color) = match activity {
        Activity::Idle => (label.to_string(), color),
        Activity::Working => (WORKING.to_string(), MUTED),
        Activity::Blocked => (label.to_string(), EDGE),
    };
    button(id, binding)
        .surface(move |button, app| {
            rect(id!(id))
                .fill(if button.hovered && activity == Activity::Idle { color.with_alpha(0.1) } else { TRANSPARENT })
                .stroke(color.with_alpha(0.4), Stroke::new(1.))
                .corner_rounding(8.)
                .build(app)
        })
        .label(move |_, app| text(id!(id), &label).font_size(13).font_weight(FontWeight::MEDIUM).fill(color).build(app))
        .on_click(on_click)
        .build(app)
        .width(label_width)
        .height(34.)
}

fn thread_button<'a>(index: usize, draft: &'a Draft, action: ThreadAction, state: &State, app: &mut PaneState) -> View<'a, State> {
    let (label, color) = match action {
        ThreadAction::Reply => ("Reply", CYAN),
        ThreadAction::Resolve => ("Resolve", GREEN),
        ThreadAction::Uphold => ("Rule for reviewer", YELLOW),
        ThreadAction::Dismiss => ("Rule for implementer", PURPLE),
    };
    let binding = Binding::new(move |s: &State| action.button(&s.drafts[index]), move |s: &mut State| action.button_mut(&mut s.drafts[index]));
    let thread = draft.id;
    let activity = activity(&state.running, |r| matches!(r, Request::Thread { thread: t, action: a, .. } if *t == thread && *a == action));
    action_button(
        id!(thread as u64, action as u64),
        label,
        color,
        activity,
        (action.button(draft), binding),
        move |state, app| {
            let text = state.drafts[index].text.text.clone();
            run(state, Request::Thread { thread, action, text });
            app.redraw();
        },
        app,
    )
}

fn start(state: &mut State, app: &mut PaneState) {
    state.jobs.send(Job::Start(app.waker())).ok();
}

fn card<'a>(id: u64, content: View<'a, State>, app: &mut PaneState) -> View<'a, State> {
    stack(vec![rect(id).fill(CARD).stroke(BORDER, Stroke::new(1.)).corner_rounding(10.).build(app), content.pad(14.)])
}

fn pill<'a>(id: u64, label: &str, color: Color, app: &mut PaneState) -> View<'a, State> {
    let width = label.chars().count() as f32 * 7. + 32.;
    stack(vec![
        rect(id!(id)).fill(color.with_alpha(0.12)).stroke(color.with_alpha(0.3), Stroke::new(1.)).corner_rounding(12.).build(app),
        row_spaced(6., vec![
            circle(id!(id)).fill(color).finish(app).width(6.).height(6.),
            text(id!(id), label).font_size(12).font_weight(FontWeight::MEDIUM).fill(color).build(app),
        ]),
    ])
    .width(width)
    .height(24.)
}

fn badge<'a>(id: u64, name: &str, color: Color, app: &mut PaneState) -> View<'a, State> {
    let initials: String = name.split(['-', ' ']).filter_map(|w| w.chars().next()).take(2).collect::<String>().to_uppercase();
    stack(vec![
        rect(id!(id)).fill(color.with_alpha(0.14)).corner_rounding(6.).build(app),
        text(id!(id), initials).font_family(MONO).font_size(10).fill(color).build(app),
    ])
    .width(24.)
    .height(24.)
}

fn heading<'a>(id: u64, label: &str, app: &mut PaneState) -> View<'a, State> {
    row(vec![text(id!(id), label.to_uppercase()).font_family(MONO).font_size(11).fill(FAINT).build(app), space()]).height(18.)
}

fn line<'a>(id: u64, body: &str, color: Color, app: &mut PaneState) -> View<'a, State> {
    text(id, body).fill(color).align(Alignment::Start).wrap().build(app).expand_x()
}

fn pr_row<'a>(index: usize, pull: &Pull, state: &'a State, app: &mut PaneState) -> View<'a, State> {
    let selected = state.selected.as_ref() == Some(&pull.slug);
    let (label, color) = pull.label();
    let slug = pull.slug.clone();
    button(
        id!(index as u64),
        (&state.rows[index], Binding::new(move |s: &State| &s.rows[index], move |s: &mut State| &mut s.rows[index])),
    )
    .surface(move |button, app| {
        stack(vec![
            rect(id!(index as u64))
                .fill(if selected || button.hovered { HOVER } else { TRANSPARENT })
                .stroke(if selected { EDGE } else { TRANSPARENT }, Stroke::new(1.))
                .corner_rounding(8.)
                .build(app),
            if selected { rect(id!(index as u64)).fill(color).build(app).width(2.).pad_y(8.).align(Align::Leading) } else { empty() },
        ])
    })
    .label({
        let slug = slug.clone();
        let open = pull.open();
        move |_, app| {
            let dot = circle(id!(index as u64)).fill(color).finish(app).width(7.).height(7.);
            if open {
                column_spaced(4., vec![
                    row_spaced(8., vec![dot, text(id!(index as u64), &slug).font_size(13).font_weight(FontWeight::MEDIUM).fill(TEXT).build(app), space()]),
                    row(vec![text(id!(index as u64), &label).font_size(12).fill(color).build(app), space()]).pad_leading(15.),
                ])
                .pad_x(10.)
            } else {
                row_spaced(8., vec![
                    dot,
                    text(id!(index as u64), &slug).font_size(13).fill(MUTED).build(app),
                    space(),
                    text(id!(index as u64), &label).font_family(MONO).font_size(11).fill(FAINT).build(app),
                ])
                .pad_x(10.)
            }
        }
    })
    .on_click(move |state, app| {
        state.selected = Some(slug.clone());
        state.confirming = None;
        state.notice = None;
        state.pending.clear();
        state.composing = None;
        sync(state);
        app.redraw();
    })
    .build(app)
    .height(if pull.open() { 56. } else { 32. })
}

fn tab_button<'a>(tab: Tab, title: &'static str, count: usize, binding: (&ButtonState, Binding<State, ButtonState>), current: Tab, app: &mut PaneState) -> View<'a, State> {
    let id = title.len() as u64;
    let width = title.len() as f32 * 8.5 + 56.;
    button(id!(id), binding)
        .surface(move |button, app| {
            stack(vec![
                rect(id!(id)).fill(if button.hovered && current != tab { HOVER } else { TRANSPARENT }).corner_rounding(6.).build(app),
                if current == tab { rect(id!(id)).fill(ACCENT).build(app).height(2.).align(Align::Bottom) } else { empty() },
            ])
        })
        .label(move |_, app| {
            row_spaced(8., vec![
                text(id!(id), title).font_weight(if current == tab { FontWeight::MEDIUM } else { FontWeight::NORMAL }).fill(if current == tab { TEXT } else { MUTED }).build(app),
                stack(vec![
                    rect(id!(id)).fill(Color::from_rgb8(26, 29, 36)).corner_rounding(4.).build(app),
                    text(id!(id), count.to_string()).font_family(MONO).font_size(11).fill(MUTED).build(app),
                ]).width(count.to_string().len() as f32 * 7. + 12.).height(20.),
            ])
        })
        .on_click(move |state, app| {
            state.tab = tab;
            app.redraw();
        })
        .build(app)
        .width(width)
        .height(40.)
}

fn side<'a>(id: u64, who: &str, stance: &str, color: Color, body: &str, button: View<'a, State>, app: &mut PaneState) -> View<'a, State> {
    stack(vec![
        rect(id!(id)).fill(CARD).stroke(Color::from_rgb8(35, 38, 45), Stroke::new(1.)).corner_rounding(10.).build(app),
        column_spaced(10., vec![
            row_spaced(8., vec![
                badge(id!(id), who, color, app),
                text(id!(id), who).font_weight(FontWeight::MEDIUM).fill(TEXT).build(app),
                text(id!(id), stance).font_size(12).fill(color).build(app),
                space(),
            ])
            .height(24.),
            line(id!(id), body, BODY, app),
            row(vec![button, space()]),
        ])
        .pad(16.),
    ])
}

fn decision<'a>(id: u64, index: usize, thread: &'a Thread, draft: &'a Draft, state: &'a State, app: &mut PaneState) -> View<'a, State> {
    let last = |kind: MessageKind| thread.messages.iter().rev().find(|m| m.kind == kind).map_or("", |m| m.body.as_str());
    let subtitle = match thread.state {
        ThreadState::Escalated => format!("The arbiter could not decide: {}", last(MessageKind::Ruling(Decision::Escalate))),
        _ => "The arbiter is deciding. You can decide now instead.".to_string(),
    };
    let binding = Binding::new(move |s: &State| &s.drafts[index].text, move |s: &mut State| &mut s.drafts[index].text);
    let reviewer = thread.reviewer.as_str();
    stack(vec![
        rect(id!(id)).fill(Color::from_rgb8(18, 14, 22)).stroke(MAGENTA.with_alpha(0.35), Stroke::new(1.)).corner_rounding(14.).build(app),
        column_spaced(14., vec![
            row_spaced(10., vec![
                text(id!(id), "Decision needed").font_weight(FontWeight::BOLD).fill(Color::from_rgb8(244, 198, 251)).build(app),
                text(id!(id), "Your ruling unblocks merge").font_size(13).fill(MUTED).build(app),
                space(),
            ])
            .height(24.),
            line(id!(id), &subtitle, MUTED, app),
            column_spaced(12., vec![
                side(id!(id, 1u64), reviewer, "wants a change", YELLOW, last(MessageKind::Comment), thread_button(index, draft, ThreadAction::Uphold, state, app), app),
                side(id!(id, 2u64), IMPLEMENTER, "keep as is", PURPLE, last(MessageKind::Dispute), thread_button(index, draft, ThreadAction::Dismiss, state, app), app),
            ]),
            text(id!(id), "Ruling note — sent to both agents").font_size(12).fill(MUTED).build(app).align(Align::Leading),
            text_field(id!(id), (&draft.text, binding)).hint_text("Your reason").align(Alignment::Start).build(app).height(34.),
        ])
        .pad(18.),
    ])
}

fn thread_card<'a>(id: u64, thread: &'a Thread, state: &'a State, app: &mut PaneState) -> View<'a, State> {
    let lines = if thread.start == thread.end { format!("{}", thread.start) } else { format!("{}–{}", thread.start, thread.end) };
    let tone = thread_color(thread.state);
    let mut header = vec![
        pill(id!(id), thread.state.label(), tone, app),
        text(id!(id), format!("{}:{lines}", thread.file)).font_family(MONO).font_size(12).fill(MUTED).align(Alignment::Start).wrap().build(app).expand_x(),
    ];
    if state.tab == Tab::Conversation && state.unfolded.contains(&thread.id) {
        let thread_id = thread.id;
        header.push(stack(vec![
            rect(id!(id))
                .fill(TRANSPARENT)
                .view()
                .gesture(gesture::click(id!(id)).button(MouseButton::Left).run(move |state: &mut State, app, event| {
                    if event.state == ClickPhase::Completed {
                        state.unfolded.retain(|t| *t != thread_id);
                        app.redraw();
                    }
                }))
                .build(app),
            text(id!(id), "Collapse").font_size(12).fill(MUTED).build(app),
        ])
        .width(64.)
        .height(24.));
    }
    let mut body = vec![row_spaced(12., header)];
    for (i, m) in thread.messages.iter().enumerate() {
        let k = (id << 8) + i as u64;
        let (verb, color) = match m.kind {
            MessageKind::Comment | MessageKind::Reply => ("", MUTED),
            MessageKind::Dispute => (" · disputed", MAGENTA),
            MessageKind::Ruling(Decision::Uphold) => (" · reviewer is right", YELLOW),
            MessageKind::Ruling(Decision::Dismiss) => (" · implementer is right", GREEN),
            MessageKind::Ruling(Decision::Escalate) => (" · asks you to decide", RED),
        };
        let who = if m.author == IMPLEMENTER { PURPLE } else if m.author == YOU { ACCENT } else { YELLOW };
        body.push(row_spaced(12., vec![
            badge(id!(k), &m.author, who, app).align(Align::Top),
            column_spaced(4., vec![
                rich_text(id!(k), [
                    span(m.author.clone()).color(TEXT).weight(FontWeight::MEDIUM).size(12),
                    span(format!("{verb} · {}", ago(m.at))).color(if verb.is_empty() { MUTED } else { color }).size(12),
                ]).align(Alignment::Start).build(app).expand_x(),
                line(id!(k), &m.body, BODY, app),
            ])
            .expand_x(),
        ]));
    }
    if let Some(by) = &thread.resolved_by {
        body.push(text(id!(id), format!("Resolved by {by}")).font_size(12).fill(GREEN).build(app).align(Align::Leading));
    }
    if let Some((index, draft)) = state.drafts.iter().enumerate().find(|(_, d)| d.id == thread.id).filter(|_| state.selected().is_some_and(Pull::open)) {
        if matches!(thread.state, ThreadState::Disputed | ThreadState::Escalated) {
            if state.tab == Tab::Files {
                body.push(decision(id, index, thread, draft, state, app));
            } else {
                body.push(text(id!(id), "Decision pinned at the top ↑").font_size(12).fill(PURPLE).build(app).align(Align::Leading));
            }
        } else {
            let binding = Binding::new(move |s: &State| &s.drafts[index].text, move |s: &mut State| &mut s.drafts[index].text);
            body.push(text_field(id!(id), (&draft.text, binding)).hint_text("Reply").align(Alignment::Start).build(app).height(34.));
            body.push(row_spaced(8., vec![thread_button(index, draft, ThreadAction::Reply, state, app), thread_button(index, draft, ThreadAction::Resolve, state, app), space()]));
        }
    }
    let edge = if thread.state.unsettled() { tone.with_alpha(0.35) } else { BORDER };
    stack(vec![rect(id!(id)).fill(CARD).stroke(edge, Stroke::new(1.)).corner_rounding(10.).build(app), column_spaced(12., body).pad(14.)]).pad_y(4.)
}

const RAIL: f32 = 40.;

fn rail<'a>(id: u64, marker: Option<View<'a, State>>, content: View<'a, State>, app: &mut PaneState) -> View<'a, State> {
    row(vec![
        stack(vec![
            rect(id!(id)).fill(BORDER).build(app).width(2.),
            marker.map_or(empty(), |m| m.align(Align::Top).pad_top(6.)),
        ])
        .width(RAIL),
        content.expand_x(),
    ])
}

fn item_view<'a>(id: u64, item: &Item<'a>, state: &'a State, app: &mut PaneState) -> View<'a, State> {
    match item {
        Item::Decision(thread) => match state.drafts.iter().enumerate().find(|(_, d)| d.id == thread.id) {
            Some((index, draft)) => decision(id, index, thread, draft, state, app).pad_bottom(16.),
            None => empty(),
        },
        Item::Commit { sha, subject, at } => rail(
            id,
            Some(rect(id!(id)).fill(BACKGROUND).stroke(Color::from_rgb8(58, 63, 73), Stroke::new(2.)).corner_rounding(6.).build(app).width(11.).height(11.)),
            row_spaced(10., vec![
                text(id!(id), sha.get(..12).unwrap_or(sha)).font_family(MONO).font_size(12).fill(PURPLE).build(app),
                text(id!(id), *subject).font_size(13).fill(TEXT).build(app),
                text(id!(id), ago(*at)).font_size(12).fill(FAINT).build(app),
                space(),
            ])
            .height(36.),
            app,
        ),
        Item::Review { reviewer, verdict, sha, at, stale } => {
            let (label, color) = match verdict {
                Verdict::Approve if *stale => ("approved", MUTED),
                Verdict::Approve => ("approved", GREEN),
                Verdict::Changes => ("requested changes", YELLOW),
                Verdict::Comment => ("commented", CYAN),
            };
            let marker = match verdict {
                Verdict::Approve => svg(id!(id), if *stale { STALE_ICON } else { APPROVED_ICON }).finish(app).width(16.).height(16.),
                Verdict::Changes | Verdict::Comment => stack(vec![
                    circle(id!(id)).fill(color.with_alpha(0.15)).finish(app),
                    circle(id!(id)).fill(color).finish(app).width(5.).height(5.),
                ])
                .width(16.)
                .height(16.),
            };
            let short = sha.get(..7).unwrap_or(sha);
            let mut content = vec![
                text(id!(id), *reviewer).font_size(13).font_weight(FontWeight::MEDIUM).fill(TEXT).build(app),
                text(id!(id), label).font_size(13).fill(color).build(app),
            ];
            if *stale && *verdict == Verdict::Approve {
                let chip = format!("stale · on {short}");
                let width = chip.chars().count() as f32 * 6. + 12.;
                content.push(stack(vec![
                    rect(id!(id)).fill(TRANSPARENT).stroke(Color::from_rgb8(58, 48, 34), Stroke::new(1.)).corner_rounding(4.).build(app),
                    text(id!(id), chip).font_size(11).fill(Color::from_rgb8(245, 196, 107)).build(app),
                ]).width(width).height(18.));
            }
            content.push(text(id!(id), ago(*at)).font_size(12).fill(FAINT).build(app));
            content.push(space());
            rail(id, Some(marker), row_spaced(10., content).height(36.), app)
        }
        Item::Thread(thread) if state.tab == Tab::Conversation && !thread.state.unsettled() && !state.unfolded.contains(&thread.id) => {
            let lines = if thread.start == thread.end { format!("{}", thread.start) } else { format!("{}–{}", thread.start, thread.end) };
            let file = thread.file.rsplit('/').next().unwrap_or(&thread.file);
            let last = thread.messages.last().map_or("", |m| m.body.as_str());
            let summary: String = if last.chars().count() > 56 { format!("{}…", last.chars().take(56).collect::<String>()) } else { last.to_string() };
            let thread_id = thread.id;
            let row = stack(vec![
                rect(id!(id))
                    .fill(CARD)
                    .stroke(BORDER, Stroke::new(1.))
                    .corner_rounding(10.)
                    .view()
                    .gesture(gesture::click(id!(id)).button(MouseButton::Left).run(move |state: &mut State, app, event| {
                        if event.state == ClickPhase::Completed {
                            state.unfolded.push(thread_id);
                            app.redraw();
                        }
                    }))
                    .build(app),
                row_spaced(10., vec![
                    circle(id!(id)).fill(thread_color(thread.state)).finish(app).width(6.).height(6.),
                    text(id!(id), format!("{file}:{lines}")).font_family(MONO).font_size(12).fill(MUTED).build(app),
                    text(id!(id), summary.replace('\n', " ")).font_size(12).fill(BODY).align(Alignment::Start).wrap().build(app).expand_x(),
                    text(id!(id), "Expand").font_size(12).fill(MUTED).build(app),
                ])
                .pad_x(14.),
            ])
            .height(52.)
            .pad_y(4.);
            rail(id, None, row, app)
        }
        Item::Thread(thread) if state.tab == Tab::Conversation => {
            let card = thread_card(id, thread, state, app);
            rail(id, None, card, app)
        }
        Item::Thread(thread) => thread_card(id, thread, state, app).pad_leading(24.),
        Item::File(file, index) => file_header(*index, file, state, app).pad_top(12.),
        Item::Line(path, l, spans) => {
            let selected = match (&state.composing, &l.line) {
                (Some((file, start, end)), Line::Added { new, .. } | Line::Context { new, .. }) => file == path && (start..=end).contains(&new),
                _ => false,
            };
            diff_line(id, path, l, spans, selected, state.selected().is_some_and(Pull::open), state.diff_size, app)
        }
        Item::Pending(comment) => card(id!(id), column_spaced(6., vec![
            text(id!(id), "you · pending").font_weight(FontWeight::BOLD).fill(PURPLE).build(app).align(Align::Leading),
            line(id!(id), &comment.body, TEXT, app),
        ]), app).pad_y(4.).pad_leading(24.),
        Item::Composer => composer(state, app).pad_y(4.).pad_leading(24.),
        Item::Merge(pull) => merge_box(pull, state, app).pad_top(16.).pad_bottom(16.).pad_leading(RAIL),
        Item::Fold { reviewer, at, count, open } => {
            let key = (reviewer.to_string(), *at);
            let label = match (open, count) {
                (true, _) => "Hide comments".to_string(),
                (false, 1) => "1 comment, approved later".to_string(),
                (false, n) => format!("{n} comments, approved later"),
            };
            let fold = stack(vec![
                rect(id!(id))
                    .fill(TRANSPARENT)
                    .view()
                    .gesture(gesture::click(id!(id)).button(MouseButton::Left).run(move |state: &mut State, app, event| {
                        if event.state != ClickPhase::Completed {
                            return;
                        }
                        match state.expanded.iter().position(|k| *k == key) {
                            Some(i) => {
                                state.expanded.remove(i);
                            }
                            None => state.expanded.push(key.clone()),
                        }
                        app.redraw();
                    }))
                    .build(app),
                row_spaced(
                    6.,
                    vec![
                        chevron(id, *open, app),
                        text(id!(id), label).font_size(13).fill(MUTED).build(app),
                        space(),
                    ],
                ),
            ])
            .pad_y(4.);
            rail(id, None, fold, app)
        }
    }
}

fn file_header<'a>(index: usize, file: &'a FileDiff, state: &'a State, app: &mut PaneState) -> View<'a, State> {
    let view = &state.file_views[index];
    let (collapsed, viewed, blob) = (view.collapsed, view.viewed.is_some(), file.blob.clone());
    let id = index as u64;
    let toggle = button(id!(id), (&view.toggle, Binding::new(move |s: &State| &s.file_views[index].toggle, move |s: &mut State| &mut s.file_views[index].toggle)))
        .surface(|_, _| empty())
        .label(move |_, app| {
            row_spaced(
                10.,
                vec![
                    chevron(id, !collapsed, app),
                    match file.path.rsplit_once('/') {
                        Some((dir, name)) => rich_text(id!(id), [
                            span(format!("{dir}/")).color(FAINT).size(13),
                            span(name).color(TEXT).weight(FontWeight::MEDIUM).size(13),
                        ]),
                        None => rich_text(id!(id), [span(file.path.as_str()).color(TEXT).weight(FontWeight::MEDIUM).size(13)]),
                    }
                    .font_family(MONO)
                    .build(app),
                    text(id!(id), format!("+{}", file.added)).font_family(MONO).font_size(12).fill(GREEN).build(app),
                    text(id!(id), format!("−{}", file.removed)).font_family(MONO).font_size(12).fill(RED).build(app),
                    space(),
                ],
            )
        })
        .on_click(move |state, app| {
            state.file_views[index].collapsed = !state.file_views[index].collapsed;
            app.redraw();
        })
        .build(app)
        .expand_x();
    let check = button(id!(id), (&view.check, Binding::new(move |s: &State| &s.file_views[index].check, move |s: &mut State| &mut s.file_views[index].check)))
        .surface(|_, _| empty())
        .label(move |_, app| {
            row_spaced(
                8.,
                vec![
                    rect(id!(id)).fill(if viewed { GREEN } else { TRANSPARENT }).stroke(if viewed { GREEN } else { MUTED }, Stroke::new(1.5)).corner_rounding(3.).build(app).width(14.).height(14.),
                    text(id!(id), "Viewed").fill(if viewed { TEXT } else { MUTED }).build(app),
                ],
            )
        })
        .on_click(move |state, app| {
            let view = &mut state.file_views[index];
            view.viewed = if view.viewed.is_some() { None } else { Some(blob.clone()) };
            view.collapsed = view.viewed.is_some();
            app.redraw();
        })
        .build(app)
        .width(90.);
    stack(vec![
        rect(id!(id)).fill(Color::from_rgb8(16, 18, 22)).stroke(Color::from_rgb8(30, 33, 39), Stroke::new(1.)).corner_rounding(10.).build(app),
        row_spaced(12., vec![toggle, check]).pad_x(14.),
    ])
    .height(44.)
}

fn merge_box<'a>(pull: &'a Pull, state: &'a State, app: &mut PaneState) -> View<'a, State> {
    let status = match &pull.standing {
        Standing::Open(Ok(status)) => Some(status),
        _ => None,
    };
    let phase = status.map(Status::phase);
    let approved = phase == Some(Phase::Approved);
    let title = match phase {
        Some(Phase::Approved) => "All reviewers approved",
        Some(Phase::Escalated) => "Escalated",
        Some(Phase::Disputed) => "Disputed",
        Some(Phase::AwaitingReview) => "Awaiting review",
        Some(Phase::ChangesRequested) => "Changes requested",
        Some(Phase::Detached) => "Off branch",
        Some(Phase::Conflicting) => "Conflicting",
        Some(Phase::Implementing) => "Implementing",
        Some(Phase::AwaitingYou) => "Awaiting your review",
        Some(Phase::Discussing) => "Discussing",
        None => "Unknown",
    };
    let detail: Vec<String> = match (phase, status) {
        (Some(Phase::Approved), _) => vec!["This branch can be squashed and merged.".to_string()],
        (Some(Phase::Detached), _) => vec!["The PR's worktree is not on its branch, so new commits are not reaching the PR. The implementer is moving it back.".to_string()],
        (Some(Phase::Conflicting), _) => vec!["The branch conflicts with the base branch. The implementer is merging it in and resolving it.".to_string()],
        (Some(Phase::Implementing), _) => vec!["The implementer hasn't marked it ready for review.".to_string()],
        (_, None) => vec!["The PR's state couldn't be read.".to_string()],
        (_, Some(status)) => {
            let threads = |n: usize| if n == 1 { "1 thread".to_string() } else { format!("{n} threads") };
            let mut lines = Vec::new();
            if !status.escalated.is_empty() {
                lines.push(format!("{YOU}: decide {} escalated to you", threads(status.escalated.len())));
            }
            if !status.disputed.is_empty() {
                lines.push(format!("{ARBITER}: rule on {} the implementer disputed", threads(status.disputed.len())));
            }
            if !status.open.is_empty() {
                lines.push(format!("{IMPLEMENTER}: address {} with changes requested", threads(status.open.len())));
            }
            if !status.comments.is_empty() {
                lines.push(format!("{IMPLEMENTER}: answer {} with comments", threads(status.comments.len())));
            }
            let mut repliers: Vec<&str> = status.waiting.iter().map(|w| w.reviewer.as_str()).collect();
            repliers.sort();
            repliers.dedup();
            for reviewer in repliers {
                let n = status.waiting.iter().filter(|w| w.reviewer == reviewer).count();
                lines.push(format!("{reviewer}: reply on {}", threads(n)));
            }
            lines.extend(status.awaiting.iter().map(|reviewer| format!("{reviewer}: review the latest commit")));
            if status.awaiting_you {
                lines.push(format!("{YOU}: review the latest commit in Files changed"));
            }
            lines
        }
    };
    let cancel = |app: &mut PaneState| {
        action_button(id!(), "Cancel", MUTED, Activity::Idle, binding!(state.cancel_button), |state, app| {
            state.confirming = None;
            app.redraw();
        }, app)
    };
    let slug = pull.slug.clone();
    let subjects: String = pull
        .pr
        .as_ref()
        .map(|pr| {
            pr.timeline
                .iter()
                .filter_map(|e| match e {
                    Event::Commit { subject, .. } => Some(format!("\n* {subject}")),
                    Event::Review { .. } => None,
                })
                .collect()
        })
        .unwrap_or_default();
    let controls = match state.confirming {
        Some(Confirm::Merge) => vec![
            text_field(id!(), binding!(state.merge_message)).multiline().wrap().align(Alignment::Start).vertical_align(TextFieldVerticalAlignment::Top).build(app).height(120.),
            row_spaced(8., vec![
                action_button(id!(), "Confirm squash and merge", GREEN, activity(&state.running, |r| matches!(r, Request::Merge { .. })), binding!(state.merge_button), move |state, app| {
                    let message = state.merge_message.text.clone();
                    run(state, Request::Merge { slug: slug.clone(), message });
                    app.redraw();
                }, app),
                cancel(app),
                space(),
            ]),
        ],
        Some(Confirm::Close) => vec![row_spaced(8., vec![
            action_button(id!(), "Confirm close", RED, activity(&state.running, |r| matches!(r, Request::Close { .. })), binding!(state.close_button), move |state, app| {
                run(state, Request::Close { slug: slug.clone() });
                app.redraw();
            }, app),
            cancel(app),
            space(),
        ])],
        None => vec![row_spaced(8., vec![
            if approved {
                action_button(id!(), "Squash & merge", GREEN, Activity::Idle, binding!(state.merge_button), move |state, app| {
                    state.merge_message = TextState::new(format!("{slug}\n{subjects}"));
                    state.confirming = Some(Confirm::Merge);
                    app.redraw();
                }, app)
            } else {
                empty()
            },
            action_button(id!(), "Close", RED, Activity::Idle, binding!(state.close_button), |state, app| {
                state.confirming = Some(Confirm::Close);
                app.redraw();
            }, app),
            space(),
        ])],
    };
    let accent = if approved { GREEN } else { BORDER };
    let mut body = vec![
        text(id!(), title).font_size(16).font_weight(FontWeight::BOLD).fill(if approved { GREEN } else { phase.map_or(RED, phase_color) }).build(app).align(Align::Leading),
    ];
    body.extend(detail.into_iter().enumerate().map(|(i, line)| text(id!(i as u64), line).fill(MUTED).build(app).align(Align::Leading)));
    if let Some(notice) = &state.notice {
        body.push(text(id!(), notice).fill(RED).build(app).align(Align::Leading));
    }
    body.extend(controls);
    stack(vec![
        rect(id!()).fill(if approved { Color::from_rgb8(22, 42, 30) } else { CARD }).stroke(accent, Stroke::new(if approved { 2. } else { 1. })).corner_rounding(8.).build(app),
        column_spaced(10., body).pad(16.),
    ])
}

fn diff_line<'a>(id: u64, path: &'a str, l: &'a DiffLine, spans: &[(String, [u8; 3])], selected: bool, open: bool, size: u32, app: &mut PaneState) -> View<'a, State> {
    let number = |n: Option<u32>, app: &mut PaneState| {
        text(id!(id), n.map_or(String::new(), |n| n.to_string())).font_family(MONO).font_size(size).fill(MUTED).build(app).align(Align::Trailing).width(size as f32 * 3.)
    };
    let (old, new, marker, body, color, tint) = match &l.line {
        Line::Hunk(text) => (None, None, "", text, FAINT, Color::from_rgb8(15, 18, 25)),
        Line::Added { new, text } => (None, Some(*new), "+", text, GREEN, GREEN.with_alpha(0.08)),
        Line::Removed { old, text } => (Some(*old), None, "−", text, RED, RED.with_alpha(0.08)),
        Line::Context { old, new, text } => (Some(*old), Some(*new), " ", text, TEXT, TRANSPARENT),
    };
    let background = match new.filter(|_| open && !matches!(l.line, Line::Removed { .. })) {
        Some(new) => {
            let (path, extend_path) = (path.to_string(), path.to_string());
            rect(id!(id))
                .fill(if selected { ACCENT.with_alpha(0.18) } else { tint })
                .view()
                .gesture(gesture::click(id!(id, 1u64)).button(MouseButton::Left).modifiers(!Modifier::Shift).run(move |state: &mut State, app, event| {
                    if event.state == ClickPhase::Completed {
                        state.composing = Some((path.clone(), new, new));
                        state.composer_text = TextState::new("");
                        app.redraw();
                    }
                }))
                .gesture(gesture::click(id!(id, 2u64)).button(MouseButton::Left).modifiers(Modifier::Shift).run(move |state: &mut State, app, event| {
                    if event.state != ClickPhase::Completed {
                        return;
                    }
                    if let Some((file, start, end)) = &mut state.composing {
                        if *file == extend_path {
                            (*start, *end) = ((*start).min(new), (*end).max(new));
                        }
                    }
                    app.redraw();
                }))
                .build(app)
        }
        None => rect(id!(id)).fill(tint).build(app),
    };
    stack(vec![
        background,
        row_spaced(
            8.,
            vec![
                number(old, app),
                number(new, app),
                text(id!(id), marker).font_family(MONO).font_size(size).fill(color).build(app).width(size as f32 * 0.7),
                if spans.is_empty() {
                    text(id!(id), body.replace('\t', "    ")).font_family(MONO).font_size(size).fill(color).align(Alignment::Start).wrap().build(app).expand_x()
                } else {
                    rich_text(id!(id), spans.iter().map(|(piece, [r, g, b])| span(piece.clone()).color(Color::from_rgb8(*r, *g, *b)).size(size))).font_family(MONO).font_size(size).align(Alignment::Start).wrap().build(app).expand_x()
                },
            ],
        )
        .pad_y(1.),
    ])
}

fn composer<'a>(state: &'a State, app: &mut PaneState) -> View<'a, State> {
    card(
        id!(),
        column_spaced(
            8.,
            vec![
                text_field(id!(), binding!(state.composer_text)).hint_text("Leave a comment").align(Alignment::Start).build(app).height(34.),
                row_spaced(
                    8.,
                    vec![
                        action_button(id!(), "Add comment", GREEN, Activity::Idle, binding!(state.add_comment_button), |state, app| {
                            if let Some((file, start, end)) = state.composing.take() {
                                let body = state.composer_text.text.trim().to_string();
                                if !body.is_empty() {
                                    state.pending.push(Comment { file, start, end, body });
                                }
                            }
                            app.redraw();
                        }, app),
                        action_button(id!(), "Cancel", MUTED, Activity::Idle, binding!(state.cancel_comment_button), |state, app| {
                            state.composing = None;
                            app.redraw();
                        }, app),
                        space(),
                    ],
                ),
            ],
        ),
        app,
    )
}

fn your_review<'a>(pull: &'a Pull, state: &'a State, app: &mut PaneState) -> View<'a, State> {
    let submit = |verdict: Verdict| {
        let slug = pull.slug.clone();
        move |state: &mut State, app: &mut PaneState| {
            let comments = state.pending.clone();
            run(state, Request::Review { slug: slug.clone(), verdict, comments });
            app.redraw();
        }
    };
    let reviewing = |v: Verdict| activity(&state.running, |r| matches!(r, Request::Review { verdict, .. } if *verdict == v));
    let count = state.pending.len();
    let approved = pull.pr.as_ref().is_ok_and(|pr| pr.reviewers.iter().any(|(r, v)| r == YOU && *v == Some(Verdict::Approve)));
    let buttons = if approved && count == 0 {
        vec![text(id!(), "You approved this commit").fill(GREEN).font_weight(FontWeight::MEDIUM).build(app)]
    } else if count == 0 {
        vec![action_button(id!(), "Approve", ACCENT, reviewing(Verdict::Approve), binding!(state.approve_button), submit(Verdict::Approve), app)]
    } else {
        vec![
            action_button(id!(), "Comment", MUTED, reviewing(Verdict::Comment), binding!(state.comment_button), submit(Verdict::Comment), app),
            action_button(id!(), "Request changes", YELLOW, reviewing(Verdict::Changes), binding!(state.changes_button), submit(Verdict::Changes), app),
        ]
    };
    let files = pull.pr.as_ref().map_or(0, |pr| pr.files.len());
    let viewed = state.file_views.iter().filter(|v| v.slug == pull.slug && v.viewed.is_some()).count();
    let drafts = match count {
        0 => "no draft comments — click a line to comment".to_string(),
        1 => "1 draft comment".to_string(),
        n => format!("{n} draft comments"),
    };
    let summary = match &state.notice {
        Some(notice) => text(id!(), notice).font_size(12).fill(RED).build(app),
        None => text(id!(), format!("{viewed} of {files} files viewed · {drafts}")).font_size(12).fill(MUTED).build(app),
    };
    let mut body = vec![
        column_spaced(2., vec![
            row(vec![text(id!(), "Your review").font_size(13).font_weight(FontWeight::MEDIUM).fill(TEXT).build(app), space()]),
            row(vec![summary, space()]),
        ])
        .expand_x(),
    ];
    body.extend(buttons);
    stack(vec![
        rect(id!()).fill(Color::from_rgb8(15, 17, 21)).stroke(Color::from_rgb8(35, 38, 45), Stroke::new(1.)).corner_rounding(12.).build(app),
        row_spaced(12., body).pad_leading(16.).pad_trailing(10.),
    ])
    .height(58.)
}

fn items<'a>(pull: &'a Pull, pr: &'a Pr, state: &'a State) -> Vec<Item<'a>> {
    match state.tab {
        Tab::Conversation => {
            let mut items: Vec<Item> = if pull.open() {
                pr.threads.iter().filter(|t| matches!(t.state, ThreadState::Disputed | ThreadState::Escalated)).map(Item::Decision).collect()
            } else {
                Vec::new()
            };
            for (n, event) in pr.timeline.iter().enumerate() {
                match event {
                    Event::Commit { sha, subject, at } => items.push(Item::Commit { sha, subject, at: *at }),
                    Event::Review { sha, reviewer, verdict, threads, at } => {
                        items.push(Item::Review { reviewer, verdict: *verdict, sha, at: *at, stale: *sha != pr.head });
                        let approved_later = pr.timeline[n + 1..]
                            .iter()
                            .any(|e| matches!(e, Event::Review { reviewer: r, verdict: Verdict::Approve, .. } if r == reviewer));
                        let unsettled = threads.iter().any(|&i| pr.threads[i].state.unsettled());
                        let open = !approved_later || unsettled || state.expanded.iter().any(|(r, a)| r == reviewer && a == at);
                        if approved_later && !threads.is_empty() {
                            items.push(Item::Fold { reviewer, at: *at, count: threads.len(), open });
                        }
                        if open {
                            items.extend(threads.iter().map(|&i| Item::Thread(&pr.threads[i])));
                        }
                    }
                }
            }
            if pull.open() {
                items.push(Item::Merge(pull));
            }
            items
        }
        Tab::Files => {
            let mut items = Vec::new();
            for (f, file) in pr.files.iter().enumerate() {
                let Some(index) = state.file_views.iter().position(|v| v.slug == pull.slug && v.path == file.path) else { continue };
                items.push(Item::File(file, index));
                if !state.file_views[index].collapsed {
                    for (n, l) in file.lines.iter().enumerate() {
                        let spans = pull.colors.get(f).and_then(|lines| lines.get(n)).map_or(&[][..], |s| s.as_slice());
                        items.push(Item::Line(&file.path, l, spans));
                        items.extend(l.threads.iter().map(|&i| Item::Thread(&pr.threads[i])));
                        if let Line::Added { new, .. } | Line::Context { new, .. } = l.line {
                            items.extend(state.pending.iter().filter(|c| c.file == file.path && c.end == new).map(Item::Pending));
                            if state.composing.as_ref().is_some_and(|(f, _, end)| *f == file.path && *end == new) {
                                items.push(Item::Composer);
                            }
                        }
                    }
                }
            }
            items
        }
    }
}

fn detail<'a>(state: &'a State, pull: &'a Pull, app: &mut PaneState) -> View<'a, State> {
    let pr = match &pull.pr {
        Ok(pr) => pr,
        Err(e) => return column(vec![text(id!(), e).fill(RED).build(app).align(Align::Leading), space()]),
    };
    let (label, color) = pull.label();
    let commits = pr.timeline.iter().filter(|e| matches!(e, Event::Commit { .. })).count();
    let updated = pr.timeline.iter().map(|e| match e {
        Event::Commit { at, .. } | Event::Review { at, .. } => *at,
    }).max();
    let mut meta = vec![
        span(format!("{}   ", pr.head)).color(TEXT),
        span(format!("{commits} commits   {} files   ", pr.files.len())).color(MUTED),
        span(format!("+{}", pr.files.iter().map(|f| f.added).sum::<usize>())).color(GREEN),
        span(format!(" −{}", pr.files.iter().map(|f| f.removed).sum::<usize>())).color(RED),
    ];
    if let Some(at) = updated {
        meta.push(span(format!("   updated {}", ago(at))).color(MUTED));
    }
    let items = items(pull, pr, state);
    column_spaced(
        18.,
        vec![
            row_spaced(8., vec![
                text(id!(), &state.repo).font_family(MONO).font_size(12).fill(FAINT).build(app),
                text(id!(), "/").font_family(MONO).font_size(12).fill(FAINT).build(app),
                text(id!(), &pull.slug).font_family(MONO).font_size(12).fill(MUTED).build(app),
                space(),
            ])
            .height(16.),
            row_spaced(14., vec![
                text(id!(), &pull.slug).font_size(30).font_weight(FontWeight::SEMI_BOLD).fill(TEXT).build(app),
                pill(id!(), &label, color, app),
                space(),
            ])
            .height(40.),
            rich_text(id!(), meta.into_iter().map(|s| s.size(12))).font_family(MONO).align(Alignment::Start).build(app).expand_x(),
            row_spaced(16., vec![
                heading(id!(), "Brief", app).width(52.).align(Align::Top),
                line(id!(), &pr.prompt, BODY, app),
            ]),
            stack(vec![
                rect(id!()).fill(BORDER).build(app).height(1.).align(Align::Bottom),
                row_spaced(4., vec![
                    tab_button(Tab::Conversation, "Conversation", pr.threads.len(), binding!(state.conversation_tab), state.tab, app),
                    tab_button(Tab::Files, "Files changed", pr.files.len(), binding!(state.files_tab), state.tab, app),
                    space(),
                ]),
            ])
            .height(40.),
            scroller(id!(state.tab as u64, const_hash(&pull.slug, 0, 0)), None, move |i, _, app| items.get(i).map(|item| item_view(i as u64, item, state, app)), app).expand(),
            if state.tab == Tab::Files && pull.open() { your_review(pull, state, app) } else { empty() },
        ],
    )
    .pad_x(36.)
    .pad_top(24.)
    .pad_bottom(16.)
}

fn inspector<'a>(pr: &'a Pr, app: &mut PaneState) -> View<'a, State> {
    let approved = pr.reviewers.iter().filter(|(_, v)| *v == Some(Verdict::Approve)).count();
    let tone = |v: &Option<Verdict>| match v {
        Some(Verdict::Approve) => ("Approved", GREEN),
        Some(Verdict::Changes) => ("Changes", YELLOW),
        Some(Verdict::Comment) => ("Commented", CYAN),
        None => ("Pending", MUTED),
    };
    let commits: Vec<_> = pr.timeline.iter().rev().filter_map(|e| match e {
        Event::Commit { sha, subject, at } => Some((sha, subject, *at)),
        Event::Review { .. } => None,
    }).collect();
    scroller(id!(), None, move |i, _, app| match i {
        0 => {
            let mut body = vec![
                row(vec![
                    heading(id!(), "Consensus", app),
                    space(),
                    rich_text(id!(), [span(approved.to_string()).color(TEXT).weight(FontWeight::SEMI_BOLD).size(13), span(format!(" / {} approved", pr.reviewers.len())).color(FAINT).size(13)]).build(app),
                ]).height(20.),
                row_spaced(3., pr.reviewers.iter().enumerate().map(|(i, (_, v))| {
                    rect(id!(i as u64)).fill(if v.is_some() { tone(v).1 } else { EDGE }).corner_rounding(3.).build(app).height(6.)
                }).collect()).height(6.),
            ];
            body.extend(pr.reviewers.iter().enumerate().map(|(i, (name, v))| {
                let (state, color) = tone(v);
                row_spaced(10., vec![
                    badge(id!(i as u64), name, color, app),
                    text(id!(i as u64), name.as_str()).font_size(13).fill(TEXT).build(app),
                    space(),
                    text(id!(i as u64), state).font_size(12).fill(color).build(app),
                ])
                .height(32.)
            }));
            body.push(heading(id!(), "Commits", app).pad_top(16.).pad_bottom(4.));
            Some(column_spaced(12., body).pad_x(24.).pad_top(28.))
        }
        i => commits.get(i - 1).map(|(sha, subject, at)| {
            row_spaced(10., vec![
                text(id!(i as u64), sha.get(..7).unwrap_or(sha)).font_family(MONO).font_size(12).fill(PURPLE).build(app),
                text(id!(i as u64), subject.as_str()).font_size(13).fill(BODY).align(Alignment::Start).wrap().build(app).expand_x(),
                text(id!(i as u64), ago(*at)).font_size(12).fill(FAINT).build(app),
            ])
            .pad_x(24.)
            .pad_y(6.)
        }),
    }, app)
}

fn view<'a>(state: &'a State, app: &mut PaneState) -> View<'a, State> {
    let list = match &state.pulls {
        None => column(vec![text(id!(), "Loading PRs…").fill(MUTED).build(app).align(Align::Leading), space()]),
        Some(pulls) if pulls.is_empty() => column(vec![text(id!(), "No PRs").fill(MUTED).build(app).align(Align::Leading), space()]),
        Some(pulls) => {
            let split = pulls.iter().position(|p| !p.open()).unwrap_or(pulls.len());
            let count = match (split == pulls.len(), state.finished_hidden) {
                (true, _) => split,
                (false, true) => split + 1,
                (false, false) => pulls.len() + 1,
            };
            scroller(id!(), None, move |i, _, app| {
                let row = match i {
                    i if i >= count => return None,
                    i if i < split => pr_row(i, &pulls[i], state, app),
                    i if i == split => {
                        let hidden = state.finished_hidden;
                        let closed = pulls.len() - split;
                        button(id!(), binding!(state.finished_toggle))
                            .surface(|_, _| empty())
                            .label(move |_, app| {
                                row_spaced(6., vec![
                                    chevron(0, !hidden, app),
                                    heading(id!(), "Merged & closed", app),
                                    space(),
                                    text(id!(), closed.to_string()).font_family(MONO).font_size(11).fill(FAINT).build(app),
                                ])
                                .pad_x(8.)
                            })
                            .on_click(|state, app| {
                                state.finished_hidden = !state.finished_hidden;
                                app.redraw();
                            })
                            .build(app)
                            .height(28.)
                            .pad_top(16.)
                    }
                    i => pr_row(i - 1, &pulls[i - 1], state, app),
                };
                Some(row.pad_bottom(2.))
            }, app)
            .expand_y()
        }
    };
    let open = state.pulls.iter().flatten().filter(|p| p.open()).count();
    let selected = state.selected();
    let (detail, inspector) = match selected {
        None => (column(vec![text(id!(), "Select a PR").fill(MUTED).build(app).align(Align::Leading), space()]).pad(36.), empty()),
        Some(pull) => (detail(state, pull, app), pull.pr.as_ref().map_or(empty(), |pr| inspector(pr, app))),
    };
    let panel = |id: u64, content: View<'a, State>, app: &mut PaneState| stack(vec![rect(id!(id)).fill(PANEL).stroke(BORDER, Stroke::new(1.)).build(app), content]);
    let topbar = row_spaced(10., vec![
        circle(id!()).fill(ACCENT).finish(app).width(10.).height(10.),
        text(id!(), "gate").font_weight(FontWeight::SEMI_BOLD).fill(TEXT).build(app),
        stack(vec![
            rect(id!()).fill(TRANSPARENT).stroke(Color::from_rgb8(34, 38, 45), Stroke::new(1.)).corner_rounding(6.).build(app),
            text(id!(), format!("~/{}", state.repo)).font_family(MONO).font_size(12).fill(FAINT).build(app),
        ])
        .width(state.repo.chars().count() as f32 * 7.5 + 34.)
        .height(22.),
        space(),
        circle(id!()).fill(GREEN).finish(app).width(6.).height(6.),
        text(id!(), format!("{open} open")).font_size(12).fill(MUTED).build(app),
    ])
    .pad_leading(84.).pad_trailing(16.);
    stack(vec![
        rect(id!())
            .fill(BACKGROUND)
            .view()
            .gesture(gesture::key(id!()).modifiers(Modifier::Super).run(|state: &mut State, app, event| {
                if event.phase != KeyPhase::Pressed {
                    return;
                }
                state.diff_size = match event.key {
                    Key::Character(c) if c == "=" || c == "+" => (state.diff_size + 1).min(32),
                    Key::Character(c) if c == "-" => state.diff_size.saturating_sub(1).max(8),
                    Key::Character(c) if c == "0" => DEFAULT_FONT_SIZE,
                    _ => return,
                };
                app.redraw();
            }))
            .build(app),
        column(vec![
            panel(1, topbar, app).height(52.),
            row(vec![
                panel(2, column_spaced(4., vec![heading(id!(), "Needs you", app).pad_x(8.).pad_bottom(6.), list]).pad_x(12.).pad_y(16.), app).width(280.),
                detail.expand(),
                if selected.is_some() { panel(3, inspector, app).width(340.) } else { empty() },
            ])
            .expand(),
        ]),
    ])
}

fn main() {
    let gate = gate::home_reviewers().and_then(Gate::new).unwrap_or_else(|e| {
        eprintln!("gate-gui: {e}");
        process::exit(1)
    });
    let (jobs, inbox) = channel();
    let (outbox, updates) = channel();
    let notify = jobs.clone();
    thread::spawn(move || work(gate, notify, inbox, outbox));
    let state = State::new(jobs, updates);
    let pane = FONTS.iter().fold(PaneBuilder::new("main", view), |pane, (bytes, family)| pane.add_font_bytes(bytes.to_vec(), Some(family)));
    WinitApp::new(state).pane(pane.on_start(start).on_wake(apply).title("gate").inner_size(1280, 860)).run();
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    fn sh(dir: &std::path::Path, cmd: &str) {
        assert!(Command::new("sh").arg("-c").arg(cmd).current_dir(dir).status().unwrap().success(), "{cmd}");
    }

    fn settle(pane: &mut Pane<State>, state: &mut State, done: impl Fn(&State) -> bool) -> bool {
        (0..1000).any(|_| {
            pane.redraw(state, 1280, 860, 1.0);
            done(state) || {
                thread::sleep(Duration::from_millis(10));
                false
            }
        })
    }

    #[test]
    fn unsettled_threads_stay_unfolded() {
        let thread = |state| Thread { id: 1, reviewer: "arch".into(), sha: "a".into(), file: "f".into(), start: 1, end: 1, messages: Vec::new(), resolved_by: None, state };
        let review = |verdict, threads: Vec<usize>, at| Event::Review { sha: "a".into(), reviewer: "arch".into(), verdict, threads, at };
        let (jobs, _) = channel();
        let (_, updates) = channel();
        let state = State::new(jobs, updates);
        for (thread_state, shown) in [(ThreadState::Open, true), (ThreadState::Waiting, true), (ThreadState::Resolved, false), (ThreadState::Dismissed, false)] {
            let pr = Pr {
                prompt: String::new(),
                head: "a".into(),
                reviewers: Vec::new(),
                threads: vec![thread(thread_state)],
                timeline: vec![review(Verdict::Changes, vec![0], 1), review(Verdict::Approve, Vec::new(), 2)],
                files: Vec::new(),
            };
            let pull = Pull { slug: "pr".into(), standing: Standing::Open(Err(String::new())), pr: Ok(pr.clone()), colors: Vec::new() };
            let visible = items(&pull, &pr, &state).iter().any(|i| matches!(i, Item::Thread(_)));
            assert!(visible == shown, "a {} thread folded wrong", thread_state.label());
        }
    }

    #[test]
    fn approve_button_submits_review() {
        let tmp = std::env::temp_dir().join(format!("gate-gui-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let repo = tmp.join("repo");
        std::fs::create_dir_all(tmp.join("reviewers")).unwrap();
        std::fs::write(tmp.join("reviewers/arch-test.md"), "").unwrap();
        std::fs::create_dir_all(&repo).unwrap();
        sh(&repo, "git init -q && git config user.email t@t && git config user.name t && echo a > f && git add . && git commit -qm init");
        std::env::set_current_dir(&repo).unwrap();
        let gate = Gate::new(tmp.join("reviewers")).unwrap();
        let tree = gate.open("pr", "task").unwrap();
        sh(&tree, "git config user.email t@t && git config user.name t && echo b > f && git commit -qam b");
        gate.mark("pr", IMPLEMENTER, gate::Readiness::Ready).unwrap();
        gate.review("pr", "arch-test", Verdict::Approve, vec![]).unwrap();
        assert!(gate.status("pr").unwrap().phase() == Phase::AwaitingYou);

        let (jobs, inbox) = channel();
        let (outbox, updates) = channel();
        let notify = jobs.clone();
        let worker = Gate::new(tmp.join("reviewers")).unwrap();
        thread::spawn(move || work(worker, notify, inbox, outbox));
        let mut state = State::new(jobs, updates);
        state.selected = Some("pr".into());
        state.tab = Tab::Files;
        let mut pane = PaneBuilder::new("main", view).on_start(start).on_frame(apply).build();
        pane.redraw(&mut state, 1280, 860, 1.0);
        assert!(settle(&mut pane, &mut state, |s| s.pulls.as_ref().is_some_and(|p| !p.is_empty())), "the worker never loaded the PRs");
        for _ in 0..3 {
            pane.redraw(&mut state, 1280, 860, 1.0);
        }
        let mut tab_hit = None;
        'tabs: for y in (60..300).step_by(4) {
            for x in (320..800).step_by(12) {
                pane.click(&mut state, Point::new(x as f64, y as f64));
                if state.tab == Tab::Conversation {
                    tab_hit = Some((x, y));
                    break 'tabs;
                }
            }
        }
        assert!(tab_hit.is_some(), "the tab button outside the scroller never responded");
        state.tab = Tab::Files;
        pane.redraw(&mut state, 1280, 860, 1.0);
        let mut hit = None;
        'scan: for y in (100..860).step_by(5) {
            for x in (320..800).step_by(12) {
                pane.click(&mut state, Point::new(x as f64, y as f64));
                if state.running.is_some() {
                    assert!(matches!(state.running, Some(Request::Review { verdict: Verdict::Approve, .. })));
                    assert!(activity(&state.running, |r| matches!(r, Request::Close { .. })) == Activity::Blocked);
                    run(&mut state, Request::Close { slug: "pr".into() });
                    assert!(matches!(state.running, Some(Request::Review { .. })), "a second action started while one was in flight");
                }
                assert!(settle(&mut pane, &mut state, |s| s.running.is_none()));
                if gate.status("pr").unwrap().phase() == Phase::Approved {
                    hit = Some((x, y));
                    break 'scan;
                }
            }
        }
        assert!(hit.is_some(), "no click approved the PR; notice: {:?}", state.notice);

        state.tab = Tab::Conversation;
        assert!(state.selected().is_some_and(|p| matches!(&p.standing, Standing::Open(Ok(s)) if s.phase() == Phase::Approved)), "the action finished before the list showed its result");
        let mut opened = None;
        'open: for y in (200..860).step_by(5) {
            for x in (320..800).step_by(12) {
                state.tab = Tab::Conversation;
                pane.redraw(&mut state, 1280, 860, 1.0);
                pane.click(&mut state, Point::new(x as f64, y as f64));
                pane.redraw(&mut state, 1280, 860, 1.0);
                if state.confirming.is_some() {
                    opened = Some((x, y, state.confirming == Some(Confirm::Merge)));
                    break 'open;
                }
            }
        }
        assert!(opened.is_some_and(|o| o.2), "Squash & merge never opened the confirmation");
        let mut merged = None;
        'merge: for y in (200..860).step_by(5) {
            for x in (320..800).step_by(12) {
                state.tab = Tab::Conversation;
                pane.redraw(&mut state, 1280, 860, 1.0);
                pane.click(&mut state, Point::new(x as f64, y as f64));
                assert!(settle(&mut pane, &mut state, |s| s.running.is_none()));
                if gate.lifecycle("pr").unwrap() == Some(gate::Lifecycle::Merged) {
                    merged = Some((x, y));
                    break 'merge;
                }
                if state.confirming != Some(Confirm::Merge) {
                    break 'merge;
                }
            }
        }
        assert!(merged.is_some(), "confirm never merged; confirming: {}, notice: {:?}", state.confirming.is_some(), state.notice);

        assert!(state.pulls.as_ref().is_some_and(|p| p.len() == 1 && p[0].standing == Standing::Merged), "the merge finished before the list showed it");
        let toggle = (0..120).step_by(4).find(|&y| {
            pane.click(&mut state, Point::new(100., y as f64));
            pane.redraw(&mut state, 1280, 860, 1.0);
            state.finished_hidden
        });
        assert!(toggle.is_some(), "the Merged & closed header never collapsed");
        pane.click(&mut state, Point::new(100., toggle.unwrap() as f64));
        pane.redraw(&mut state, 1280, 860, 1.0);
        assert!(!state.finished_hidden, "the header never expanded again");
        let opened = (toggle.unwrap() + 4..300).step_by(4).find(|&y| {
            pane.click(&mut state, Point::new(100., y as f64));
            pane.redraw(&mut state, 1280, 860, 1.0);
            state.selected.as_deref() == Some("pr")
        });
        std::fs::remove_dir_all(&tmp).ok();
        assert!(opened.is_some(), "the merged PR row never selected");
        assert!(state.selected().unwrap().pr.as_ref().is_ok_and(|pr| !pr.files.is_empty()), "the merged PR didn't load");
    }
}
