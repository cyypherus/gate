use gate::{ARBITER, Comment, Decision, Gate, IMPLEMENTER, Phase, Verdict, YOU};
use std::path::Path;
use std::process::Command;
use std::sync::Mutex;

static SERIAL: Mutex<()> = Mutex::new(());

fn sh(dir: &Path, cmd: &str) -> String {
    let out = Command::new("sh").arg("-c").arg(cmd).current_dir(dir).output().unwrap();
    assert!(out.status.success(), "{cmd}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn comment(start: u32, end: u32, body: &str) -> Comment {
    Comment { file: "g".into(), start, end, body: body.into() }
}

#[test]
fn review_threads_merge_close() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let tmp = std::env::temp_dir().join(format!("gate-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    let repo = tmp.join("repo");
    std::fs::create_dir_all(tmp.join("home/.claude/skills/arch-test")).unwrap();
    std::fs::create_dir_all(&repo).unwrap();
    unsafe { std::env::set_var("HOME", tmp.join("home")) };
    sh(&repo, "git init -q && git config user.email t@t && git config user.name t && echo a > f && git add . && git commit -qm init");
    std::env::set_current_dir(&repo).unwrap();
    let gate = Gate::new().unwrap();

    for slug in ["keep", "drop"] {
        let tree = gate.open(slug, "add b").unwrap();
        sh(&tree, &format!("git config user.email t@t && git config user.name t && printf '{slug}\\n2\\n' > g && git add . && git commit -qm {slug}"));
    }
    let phase = |slug: &str| gate.status(slug).unwrap().phase();

    assert!(gate.review("keep", "arch-test", Verdict::Changes, vec![comment(2, 1, "bad range")]).is_err());
    gate.review("keep", "arch-test", Verdict::Changes, vec![comment(1, 2, "no"), comment(1, 1, "why")]).unwrap();
    let [first, second] = gate.status("keep").unwrap().open[..] else { panic!() };

    gate.reply(second, IMPLEMENTER, "because").unwrap();
    assert!(phase("keep") == Phase::ChangesRequested);
    assert!(gate.status("keep").unwrap().waiting.iter().map(|w| w.thread).eq([second]));
    assert!(gate.resolve(second, "arch-other").is_err());
    gate.resolve(second, "arch-test").unwrap();
    assert!(gate.status("keep").unwrap().awaiting.is_empty(), "thread one is still open");
    assert!(gate.review("keep", "arch-test", Verdict::Approve, vec![]).is_err());
    assert!(gate.reply(second, IMPLEMENTER, "late").is_err());

    gate.dispute(first, "yes").unwrap();
    assert!(phase("keep") == Phase::Disputed);
    assert!(gate.resolve(first, "arch-test").is_err());
    gate.decide(first, ARBITER, Decision::Escalate, "?").unwrap();
    assert!(gate.decide(first, ARBITER, Decision::Dismiss, "no").is_err());
    assert!(gate.merge("keep", "keep: add b").is_err());
    gate.decide(first, YOU, Decision::Dismiss, "fine").unwrap();
    assert!(phase("keep") == Phase::AwaitingReview);
    gate.review("keep", "arch-test", Verdict::Approve, vec![]).unwrap();
    assert!(phase("keep") == Phase::AwaitingYou);

    gate.submit_review("keep", Verdict::Changes, vec![comment(1, 2, "rename")]).unwrap();
    let mine = gate.status("keep").unwrap().open[0];
    gate.dispute(mine, "no").unwrap();
    assert!(gate.decide(mine, ARBITER, Decision::Dismiss, "x").is_err());
    gate.decide(mine, ARBITER, Decision::Uphold, "fix it").unwrap();
    gate.resolve(mine, YOU).unwrap();
    gate.submit_review("keep", Verdict::Approve, vec![]).unwrap();
    assert!(phase("keep") == Phase::Approved);

    assert!(gate.merge("keep", " ").is_err());
    gate.merge("keep", "keep: add b").unwrap();
    assert_eq!(sh(&repo, "head -1 g"), "keep");
    assert_eq!(sh(&repo, "git log -1 --format=%s"), "keep: add b");
    assert!(!repo.join(".git/gate/keep").exists());
    assert!(gate.slugs().unwrap() == vec![("drop".to_string(), gate::Lifecycle::Open), ("keep".to_string(), gate::Lifecycle::Merged)]);
    assert!(gate.lifecycle("keep").unwrap() == Some(gate::Lifecycle::Merged));
    assert!(gate.open("keep", "again").is_err());
    assert!(gate.lifecycle("never").unwrap().is_none());

    gate.close("drop").unwrap();
    assert!(gate.slugs().unwrap().iter().all(|(_, l)| *l != gate::Lifecycle::Open));
    assert!(gate.pr("keep").unwrap().files.iter().any(|f| f.path == "g"));
    assert_eq!(sh(&repo, "git branch --list 'gate/*' --format='%(refname:short)'"), "gate/drop\ngate/keep");
    std::fs::remove_dir_all(&tmp).unwrap();
}

#[test]
fn conflicts_need_a_sync() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let tmp = std::env::temp_dir().join(format!("gate-conflict-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    let repo = tmp.join("repo");
    std::fs::create_dir_all(tmp.join("home/.claude/skills/arch-test")).unwrap();
    std::fs::create_dir_all(&repo).unwrap();
    unsafe { std::env::set_var("HOME", tmp.join("home")) };
    sh(&repo, "git init -q -b trunk && git config user.email t@t && git config user.name t && echo a > f && git add . && git commit -qm init");
    std::env::set_current_dir(&repo).unwrap();
    let gate = Gate::new().unwrap();
    for slug in ["one", "two"] {
        let tree = gate.open(slug, "edit f").unwrap();
        sh(&tree, &format!("git config user.email t@t && git config user.name t && echo {slug} > f && git commit -qam {slug}"));
        gate.review(slug, "arch-test", Verdict::Approve, vec![]).unwrap();
        gate.submit_review(slug, Verdict::Approve, vec![]).unwrap();
    }
    gate.merge("one", "one").unwrap();

    assert!(gate.status("two").unwrap().phase() == Phase::Conflicting);
    let sync = gate.pending("two").unwrap().into_iter().find(|w| w.role == IMPLEMENTER).unwrap();
    assert!(sync.action == gate::Action::Sync && sync.base == "trunk");
    assert!(gate.merge("two", "two").is_err());

    let tree = repo.join(".git/gate/two");
    sh(&tree, "git merge trunk >/dev/null; echo resolved > f && git commit -qam merge-trunk");
    assert!(gate.status("two").unwrap().phase() == Phase::AwaitingReview);
    assert!(gate.pr("two").unwrap().files.iter().all(|f| f.path == "f"));
    std::fs::remove_dir_all(&tmp).unwrap();
}

#[test]
fn threads_outlive_commits() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let tmp = std::env::temp_dir().join(format!("gate-settle-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    let repo = tmp.join("repo");
    std::fs::create_dir_all(tmp.join("home/.claude/skills/arch-test")).unwrap();
    std::fs::create_dir_all(&repo).unwrap();
    unsafe { std::env::set_var("HOME", tmp.join("home")) };
    sh(&repo, "git init -q && git config user.email t@t && git config user.name t && echo a > f && git add . && git commit -qm init");
    std::env::set_current_dir(&repo).unwrap();
    let gate = Gate::new().unwrap();
    let tree = gate.open("pr", "task").unwrap();
    sh(&tree, "git config user.email t@t && git config user.name t && echo b > f && git commit -qam b");
    gate.review("pr", "arch-test", Verdict::Changes, vec![comment(1, 1, "no")]).unwrap();
    let thread = gate.status("pr").unwrap().open[0];
    gate.dispute(thread, "yes").unwrap();
    gate.decide(thread, ARBITER, Decision::Escalate, "?").unwrap();
    assert!(gate.status("pr").unwrap().phase() == Phase::Escalated);

    sh(&tree, "echo c > f && git commit -qam c");
    assert!(gate.status("pr").unwrap().phase() == Phase::Escalated, "a commit must not outdate the thread");
    assert!(gate.review("pr", "arch-test", Verdict::Approve, vec![]).is_err());
    gate.decide(thread, YOU, Decision::Dismiss, "fine").unwrap();
    assert!(gate.status("pr").unwrap().phase() == Phase::AwaitingReview);
    gate.review("pr", "arch-test", Verdict::Approve, vec![]).unwrap();
    assert!(gate.status("pr").unwrap().phase() == Phase::AwaitingYou);

    sh(&tree, "git checkout -q --detach && echo d > f && git commit -qam d");
    assert!(gate.status("pr").unwrap().phase() == Phase::Detached);
    assert!(gate.pending("pr").unwrap()[0].action == gate::Action::Reattach);
    sh(&tree, "git checkout -q -B gate/pr");
    assert!(gate.status("pr").unwrap().phase() == Phase::AwaitingReview);
    std::fs::remove_dir_all(&tmp).unwrap();
}

#[test]
fn threads_outrank_reviews() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let tmp = std::env::temp_dir().join(format!("gate-test-rank-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    let repo = tmp.join("repo");
    std::fs::create_dir_all(tmp.join("home/.claude/skills/arch-a")).unwrap();
    std::fs::create_dir_all(tmp.join("home/.claude/skills/arch-b")).unwrap();
    std::fs::create_dir_all(&repo).unwrap();
    unsafe { std::env::set_var("HOME", tmp.join("home")) };
    sh(&repo, "git init -q && git config user.email t@t && git config user.name t && echo a > f && git add . && git commit -qm init");
    std::env::set_current_dir(&repo).unwrap();
    let gate = Gate::new().unwrap();
    let tree = gate.open("pr", "task").unwrap();
    sh(&tree, "git config user.email t@t && git config user.name t && printf '1\\n2\\n' > g && git add . && git commit -qm g");
    let phase = || gate.status("pr").unwrap().phase();

    gate.review("pr", "arch-a", Verdict::Changes, vec![comment(1, 1, "fix")]).unwrap();
    assert!(gate.status("pr").unwrap().awaiting == vec!["arch-b".to_string()]);
    assert!(phase() == Phase::ChangesRequested, "an open thread must outrank arch-b's missing review");

    let thread = gate.status("pr").unwrap().open[0];
    gate.reply(thread, IMPLEMENTER, "why").unwrap();
    assert!(phase() == Phase::Discussing, "a reply owed must outrank arch-b's missing review");
    gate.resolve(thread, "arch-a").unwrap();
    assert!(phase() == Phase::AwaitingReview);

    gate.submit_review("pr", Verdict::Changes, vec![comment(2, 2, "mine")]).unwrap();
    let mine = gate.status("pr").unwrap().open[0];
    gate.reply(mine, IMPLEMENTER, "done").unwrap();
    let status = gate.status("pr").unwrap();
    assert!(status.waiting.iter().any(|w| w.thread == mine && w.reviewer == YOU));
    assert!(status.phase() == Phase::Discussing, "a reply owed by you is not 'awaiting your review'");
    assert!(gate.pending("pr").unwrap().iter().all(|w| w.role != YOU), "agents must not be asked to act as you");
    std::fs::remove_dir_all(&tmp).ok();
}
