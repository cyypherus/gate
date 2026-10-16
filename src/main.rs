use clap::Parser;
use gate::{ARBITER, IMPLEMENTER, Lifecycle, Comment, Decision, Event, Gate, Line, MessageKind, Result, Verdict, YOU};
use notify::Watcher;
use std::io::IsTerminal;
use std::process::exit;

#[derive(Parser)]
enum Cli {
    Open { slug: String, prompt: String },
    Status { slug: String },
    Show { slug: String },
    List,
    Reviewers,
    Wait {
        slug: String,
        #[arg(long = "as")]
        role: String,
    },
    Review {
        slug: String,
        reviewer: String,
        verdict: Verdict,
        #[arg(short, long = "comment", value_parser = parse_comment)]
        comments: Vec<Comment>,
    },
    Reply {
        thread: i64,
        #[arg(long = "as")]
        author: String,
        body: String,
    },
    Dispute { thread: i64, argument: String },
    Decide { thread: i64, decision: Decision, reason: String },
    Resolve {
        thread: i64,
        #[arg(long = "as")]
        by: String,
    },
}

fn main() {
    if let Err(e) = run(Cli::parse()) {
        eprintln!("gate: {e}");
        exit(1);
    }
}

fn run(cli: Cli) -> Result<()> {
    let gate = Gate::new()?;
    let not_you = |name: &str| if name == YOU { Err("only the GUI acts as you".to_string()) } else { Ok(()) };
    match cli {
        Cli::Open { slug, prompt } => println!("{}", gate.open(&slug, &prompt)?.display()),
        Cli::Status { slug } => println!("{}", serde_json::to_string_pretty(&gate.status(&slug)?).unwrap()),
        Cli::List => {
            for (slug, _) in gate.slugs()?.into_iter().filter(|(_, l)| *l == Lifecycle::Open) {
                println!("{slug}\t{}", gate.status(&slug)?.phase().label());
            }
        }
        Cli::Show { slug } => show(&gate, &slug)?,
        Cli::Reviewers => println!("{}", gate.rules()?.join("\n")),
        Cli::Wait { slug, role } => {
            let (tx, changes) = std::sync::mpsc::channel();
            let mut watcher = notify::recommended_watcher(move |_| {
                tx.send(()).ok();
            })
            .map_err(|e| e.to_string())?;
            for (path, mode) in gate.watched() {
                watcher.watch(&path, mode).ok();
            }
            gate.register(&slug, &role, Some(std::process::id()))?;
            let result = loop {
                match gate.lifecycle(&slug)? {
                    None => return Err(format!("no PR {slug}")),
                    Some(Lifecycle::Open) => {}
                    Some(_) => break serde_json::json!({ "closed": true }),
                }
                if role == IMPLEMENTER {
                    let missing = gate.missing(&slug)?;
                    if !missing.is_empty() {
                        for absent in &missing {
                            gate.register(&slug, absent, None)?;
                        }
                        break serde_json::json!({ "start": missing });
                    }
                }
                if let Some(work) = gate.pending(&slug)?.into_iter().find(|w| w.role == role) {
                    break serde_json::json!({ "action": work.action, "commit": work.commit, "base": work.base, "threads": work.threads });
                }
                changes.recv_timeout(std::time::Duration::from_secs(60)).ok();
                while changes.try_recv().is_ok() {}
            };
            gate.register(&slug, &role, None)?;
            println!("{result}");
        }
        Cli::Review { slug, reviewer, verdict, comments } => gate.review(&slug, &reviewer, verdict, comments)?,
        Cli::Reply { thread, author, body } => {
            not_you(&author)?;
            gate.reply(thread, &author, &body)?
        }
        Cli::Dispute { thread, argument } => gate.dispute(thread, &argument)?,
        Cli::Decide { thread, decision, reason } => gate.decide(thread, ARBITER, decision, &reason)?,
        Cli::Resolve { thread, by } => {
            not_you(&by)?;
            gate.resolve(thread, &by)?
        }
    }
    Ok(())
}

fn show(gate: &Gate, slug: &str) -> Result<()> {
    let tty = std::io::stdout().is_terminal();
    let paint = |code: &str, text: &str| if tty { format!("\x1b[{code}m{text}\x1b[0m") } else { text.to_string() };
    let pr = gate.pr(slug)?;
    println!("{}\n{}\n", paint("1", slug), pr.prompt);
    for (reviewer, verdict) in &pr.reviewers {
        let (code, label) = match verdict {
            Some(Verdict::Approve) => ("32", "approved"),
            Some(v) => ("33", v.label()),
            None => ("2", "pending"),
        };
        println!("{}", paint(code, &format!("{reviewer}: {label}")));
    }
    for event in &pr.timeline {
        match event {
            Event::Commit { sha, subject, .. } => println!("\n{}", paint("2", &format!("commit {sha} {subject}"))),
            Event::Review { sha, reviewer, verdict, threads, .. } => {
                println!("\n{}", paint("1", &format!("{reviewer} {} on {sha}", verdict.label())));
                for t in threads.iter().map(|&i| &pr.threads[i]) {
                    println!("  thread {} [{}] {}:{}-{}", t.id, t.state.label(), t.file, t.start, t.end);
                    for m in &t.messages {
                        let kind = match m.kind {
                            MessageKind::Comment => "",
                            MessageKind::Reply => "reply ",
                            MessageKind::Dispute => "dispute ",
                            MessageKind::Ruling(Decision::Uphold) => "ruling uphold ",
                            MessageKind::Ruling(Decision::Dismiss) => "ruling dismiss ",
                            MessageKind::Ruling(Decision::Escalate) => "ruling escalate ",
                        };
                        println!("    {kind}{}: {}", m.author, m.body);
                    }
                }
            }
        }
    }
    for file in &pr.files {
        println!("\n{} {} {}", paint("1", &file.path), paint("32", &format!("+{}", file.added)), paint("31", &format!("-{}", file.removed)));
        for l in &file.lines {
            match &l.line {
                Line::Hunk(text) => println!("{}", paint("36", text)),
                Line::Added { new, text } => println!("{}", paint("32", &format!("     {new:>4} +{text}"))),
                Line::Removed { old, text } => println!("{}", paint("31", &format!("{old:>4}      -{text}"))),
                Line::Context { old, new, text } => println!("{old:>4} {new:>4}  {text}"),
            }
            for t in l.threads.iter().map(|&i| &pr.threads[i]) {
                let first = t.messages.first().map_or("", |m| m.body.as_str());
                println!("{}", paint("33", &format!("  ▎ thread {} [{}] {}: {first}", t.id, t.state.label(), t.reviewer)));
            }
        }
    }
    Ok(())
}

fn parse_comment(arg: &str) -> std::result::Result<Comment, String> {
    let malformed = || format!("expected FILE:LINE:BODY or FILE:START-END:BODY, got {arg}");
    let (file, rest) = arg.split_once(':').ok_or_else(malformed)?;
    let (lines, body) = rest.split_once(':').ok_or_else(malformed)?;
    let (start, end) = lines.split_once('-').unwrap_or((lines, lines));
    let number = |n: &str| n.parse().map_err(|_| malformed());
    Ok(Comment { file: file.to_string(), start: number(start)?, end: number(end)?, body: body.to_string() })
}
