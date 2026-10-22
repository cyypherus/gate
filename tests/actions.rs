use gate::{ARBITER, Comment, Decision, Gate, IMPLEMENTER, Lifecycle, Phase, Readiness, Verdict, YOU};
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
    std::fs::create_dir_all(tmp.join("reviewers")).unwrap();
    std::fs::write(tmp.join("reviewers/arch-test.md"), "").unwrap();
    std::fs::create_dir_all(&repo).unwrap();
    sh(&repo, "git init -q && git config user.email t@t && git config user.name t && printf '.gate/worktrees/\\n.gate/gate.db*\\n' > .gitignore && echo a > f && git add . && git commit -qm init");
    std::env::set_current_dir(&repo).unwrap();
    let gate = Gate::new(tmp.join("reviewers")).unwrap();

    assert!(gate.open("drop", "add b").is_ok_and(|_| gate.mark("drop", IMPLEMENTER, Readiness::Ready).is_err()));
    for slug in ["keep", "drop"] {
        let tree = gate.open(slug, "add b").unwrap();
        sh(&tree, &format!("git config user.email t@t && git config user.name t && printf '{slug}\\n2\\n' > g && git add . && git commit -qm {slug}"));
        assert!(gate.status(slug).unwrap().phase() == Phase::Implementing);
        assert!(gate.pending(slug).unwrap().iter().all(|w| w.role == IMPLEMENTER && w.action == gate::Action::Implement));
        gate.mark(slug, IMPLEMENTER, Readiness::Ready).unwrap();
    }
    assert!(gate.mark("drop", "arch-test", Readiness::Implementing).is_err());
    gate.mark("drop", IMPLEMENTER, Readiness::Implementing).unwrap();
    assert!(gate.review("drop", "arch-test", Verdict::Approve, vec![]).is_err());
    assert!(gate.submit_review("drop", Verdict::Approve, vec![]).is_err());
    assert!(gate.status("drop").unwrap().phase() == Phase::Implementing);
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
    assert!(!repo.join(".gate/worktrees/keep").exists());
    assert!(gate.slugs().unwrap() == vec![("drop".to_string(), gate::Lifecycle::Implementing), ("keep".to_string(), gate::Lifecycle::Merged)]);
    assert!(gate.lifecycle("keep").unwrap() == Some(gate::Lifecycle::Merged));
    assert!(gate.open("keep", "again").is_err());
    assert!(gate.lifecycle("never").unwrap().is_none());

    gate.close("drop").unwrap();
    assert!(gate.slugs().unwrap().iter().all(|(_, l)| !l.open()));
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
    std::fs::create_dir_all(tmp.join("reviewers")).unwrap();
    std::fs::write(tmp.join("reviewers/arch-test.md"), "").unwrap();
    std::fs::create_dir_all(&repo).unwrap();
    sh(&repo, "git init -q -b trunk && git config user.email t@t && git config user.name t && printf '.gate/worktrees/\\n.gate/gate.db*\\n' > .gitignore && echo a > f && git add . && git commit -qm init");
    std::env::set_current_dir(&repo).unwrap();
    let gate = Gate::new(tmp.join("reviewers")).unwrap();
    for slug in ["one", "two"] {
        let tree = gate.open(slug, "edit f").unwrap();
        sh(&tree, &format!("git config user.email t@t && git config user.name t && echo {slug} > f && git commit -qam {slug}"));
        gate.mark(slug, IMPLEMENTER, Readiness::Ready).unwrap();
        gate.review(slug, "arch-test", Verdict::Approve, vec![]).unwrap();
        gate.submit_review(slug, Verdict::Approve, vec![]).unwrap();
    }
    gate.merge("one", "one").unwrap();

    assert!(gate.status("two").unwrap().phase() == Phase::Conflicting);
    let sync = gate.pending("two").unwrap().into_iter().find(|w| w.role == IMPLEMENTER).unwrap();
    assert!(sync.action == gate::Action::Sync && sync.base == "trunk");
    assert!(gate.merge("two", "two").is_err());

    let tree = repo.join(".gate/worktrees/two");
    sh(&tree, "git merge trunk >/dev/null; echo resolved > f && git commit -qam merge-trunk");
    gate.mark("two", IMPLEMENTER, Readiness::Ready).unwrap();
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
    std::fs::create_dir_all(tmp.join("reviewers")).unwrap();
    std::fs::write(tmp.join("reviewers/arch-test.md"), "").unwrap();
    std::fs::create_dir_all(&repo).unwrap();
    sh(&repo, "git init -q && git config user.email t@t && git config user.name t && printf '.gate/worktrees/\\n.gate/gate.db*\\n' > .gitignore && echo a > f && git add . && git commit -qm init");
    std::env::set_current_dir(&repo).unwrap();
    let gate = Gate::new(tmp.join("reviewers")).unwrap();
    let tree = gate.open("pr", "task").unwrap();
    sh(&tree, "git config user.email t@t && git config user.name t && echo b > f && git commit -qam b");
    gate.mark("pr", IMPLEMENTER, Readiness::Ready).unwrap();
    gate.review("pr", "arch-test", Verdict::Changes, vec![comment(1, 1, "no")]).unwrap();
    let thread = gate.status("pr").unwrap().open[0];
    gate.dispute(thread, "yes").unwrap();
    gate.decide(thread, ARBITER, Decision::Escalate, "?").unwrap();
    assert!(gate.status("pr").unwrap().phase() == Phase::Escalated);

    sh(&tree, "echo c > f && git commit -qam c");
    gate.mark("pr", IMPLEMENTER, Readiness::Ready).unwrap();
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
    assert!(gate.status("pr").unwrap().phase() == Phase::Implementing);
    gate.mark("pr", IMPLEMENTER, Readiness::Ready).unwrap();
    assert!(gate.status("pr").unwrap().phase() == Phase::AwaitingReview);
    std::fs::remove_dir_all(&tmp).unwrap();
}

#[test]
fn threads_outrank_reviews() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let tmp = std::env::temp_dir().join(format!("gate-test-rank-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    let repo = tmp.join("repo");
    std::fs::create_dir_all(tmp.join("reviewers")).unwrap();
    std::fs::write(tmp.join("reviewers/arch-a.md"), "").unwrap();
    std::fs::write(tmp.join("reviewers/arch-b.md"), "").unwrap();
    std::fs::create_dir_all(&repo).unwrap();
    sh(&repo, "git init -q && git config user.email t@t && git config user.name t && printf '.gate/worktrees/\\n.gate/gate.db*\\n' > .gitignore && echo a > f && git add . && git commit -qm init");
    std::env::set_current_dir(&repo).unwrap();
    let gate = Gate::new(tmp.join("reviewers")).unwrap();
    let tree = gate.open("pr", "task").unwrap();
    sh(&tree, "git config user.email t@t && git config user.name t && printf '1\\n2\\n' > g && git add . && git commit -qm g");
    gate.mark("pr", IMPLEMENTER, Readiness::Ready).unwrap();
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

#[test]
fn invalid_lifecycles_are_refused() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let tmp = std::env::temp_dir().join(format!("gate-lifecycle-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    let repo = tmp.join("repo");
    std::fs::create_dir_all(tmp.join("reviewers")).unwrap();
    std::fs::write(tmp.join("reviewers/arch-test.md"), "").unwrap();
    std::fs::create_dir_all(&repo).unwrap();
    sh(&repo, "git init -q && git config user.email t@t && git config user.name t && printf '.gate/worktrees/\\n.gate/gate.db*\\n' > .gitignore && echo a > f && git add . && git commit -qm init");
    std::env::set_current_dir(&repo).unwrap();
    let base = sh(&repo, "git rev-parse HEAD");
    let gate = Gate::new(tmp.join("reviewers")).unwrap();

    let db = rusqlite::Connection::open(repo.join(".gate/gate.db")).unwrap();
    let insert = |lifecycle: &str, ready_at: Option<&str>| db.execute("INSERT INTO prs (slug, base, prompt, lifecycle, ready_at) VALUES (?1, 'b', 'p', ?1, ?2)", rusqlite::params![lifecycle, ready_at]);
    for (lifecycle, ready_at) in [("open", None), ("Ready", Some("x")), ("merged-ready", None), ("", None), ("ready", None), ("implementing", Some("x")), ("merged", Some("x")), ("closed", Some("x"))] {
        assert!(insert(lifecycle, ready_at).is_err(), "{lifecycle} {ready_at:?}");
    }
    for gone in ["state", "readiness"] {
        assert!(db.prepare(&format!("SELECT {gone} FROM prs")).is_err(), "{gone}");
    }

    let tree = gate.open("pr", "task").unwrap();
    assert!(gate.mark("pr", IMPLEMENTER, Readiness::Ready).is_err(), "ready without commits");
    sh(&tree, "git config user.email t@t && git config user.name t && echo b > f && git commit -qam b");
    assert!(gate.mark("pr", "arch-test", Readiness::Ready).is_err(), "reviewer marks");
    assert!(gate.mark("pr", YOU, Readiness::Ready).is_err(), "you mark");
    assert!(gate.review("pr", "arch-test", Verdict::Approve, vec![]).is_err(), "review while implementing");
    gate.mark("pr", IMPLEMENTER, Readiness::Ready).unwrap();
    gate.review("pr", "arch-test", Verdict::Changes, vec![comment(1, 1, "no")]).unwrap();
    let thread = gate.status("pr").unwrap().open[0];

    let marked = gate.status("pr").unwrap();
    assert!(marked.phase() == Phase::ChangesRequested);
    sh(&tree, "echo fix > f && git commit -qam fix");
    assert!(gate.status("pr").unwrap().phase() == Phase::Implementing, "commit after ready");
    assert!(gate.pending("pr").unwrap().iter().all(|w| w.role == IMPLEMENTER && w.action == gate::Action::Implement));
    assert!(gate.reply(thread, IMPLEMENTER, "fixed").is_err());
    gate.mark("pr", IMPLEMENTER, Readiness::Ready).unwrap();
    gate.reply(thread, IMPLEMENTER, "fixed").unwrap();

    sh(&tree, &format!("git reset -q --hard {base}"));
    assert!(matches!(gate.lifecycle("pr").unwrap(), Some(Lifecycle::Ready(_))));
    assert!(gate.status("pr").unwrap().phase() == Phase::Implementing, "ready with no commits");
    assert!(gate.pending("pr").unwrap().iter().all(|w| w.role == IMPLEMENTER && w.action == gate::Action::Implement));
    assert!(gate.review("pr", "arch-test", Verdict::Approve, vec![]).is_err());
    assert!(gate.submit_review("pr", Verdict::Approve, vec![]).is_err());
    assert!(gate.reply(thread, IMPLEMENTER, "x").is_err());
    assert!(gate.dispute(thread, "x").is_err());
    assert!(gate.resolve(thread, "arch-test").is_err());

    sh(&tree, "echo c > f && git commit -qam c");
    gate.mark("pr", IMPLEMENTER, Readiness::Implementing).unwrap();
    assert!(gate.reply(thread, IMPLEMENTER, "x").is_err());
    assert!(gate.merge("pr", "m").is_err());
    gate.close("pr").unwrap();
    assert!(gate.mark("pr", IMPLEMENTER, Readiness::Ready).is_err(), "ready after close");
    assert!(gate.lifecycle("pr").unwrap() == Some(Lifecycle::Closed));

    let tree = gate.open("gone", "task").unwrap();
    sh(&tree, "git config user.email t@t && git config user.name t && echo d > f && git commit -qam d");
    gate.mark("gone", IMPLEMENTER, Readiness::Ready).unwrap();
    sh(&repo, &format!("git worktree remove --force {} && git branch -qD gate/gone", tree.display()));
    assert!(gate.status("gone").is_err(), "open PR without a branch");
    std::fs::remove_dir_all(&tmp).unwrap();
}
