---
name: gate
description: Implement a task as a gate PR and see it through review. You are the implementer; gate tells you when to start reviewer and arbiter agents, and you wait on it for feedback until the PR is merged or closed. Use for "/gate <slug> <prompt> [--model <model>] [--arbiter-model <model>]".
---

# Gate

`gate` is a local PR store. Every comment is a thread with an integer id. Run `gate show <slug>` to read the PR's diff, threads and messages. The human works in `gate-gui`; never act as `you`.

## 1. Open

Run `gate open <slug> "<prompt>"`. It prints the PR's worktree. Do every edit and commit inside that worktree, never in the current checkout. If it fails, stop and report the error.

## 2. Implement

Implement the prompt in the worktree and commit. When the change is complete, run `gate mark <slug> ready --as implementer`; reviewers start only then. To pull it back from review for more work, run `gate mark <slug> implementing --as implementer`.

## 3. Wait

Repeat until the PR closes:

1. Run `gate wait <slug> --as implementer` with the Bash tool and `timeout: 86400000`. It returns at once while you owe work, and blocks only when you owe nothing.
2. `{"closed":true}`: the PR was merged or closed. Stop and give the user one line.
3. `{"start":[...]}`: these roles have work and no live agent. Start each as a background agent with the Agent tool, using the prompts in section 4: `arbiter` is the arbiter (subagent type `gate-arbiter`, model `--arbiter-model`, else `--model`); any other name is a reviewer (subagent type `general-purpose`, model `--model`). Then wait again.
4. `action` and `threads`:
   - `implement`: the PR is marked implementing. Do section 2, ending with `gate mark <slug> ready --as implementer`.
   - `reattach`: the worktree is not on its branch `gate/<slug>`, so your commits are not reaching the PR. If the worktree's HEAD already contains the branch's commits, run `git checkout -B gate/<slug>` in it; otherwise check out `gate/<slug>` and cherry-pick the missing commits. Never leave the worktree detached.
   - `sync`: your branch conflicts with the base branch. In the worktree run `git merge <base>`, resolve every conflict, and commit the merge.
   - `answer`: the human left comments on these threads. Read them with `gate show <slug>`, change the code if a comment calls for it (commit), and reply to each with `gate reply <thread> --as implementer "<message>"`.
   - `fix`: address every listed thread before anything else. For each, either fix it in code and reply with `gate reply <thread> --as implementer "<what you changed>"`, reply to explain or ask, or dispute with `gate dispute <thread> "<argument>"` when you are confident it is wrong. A fix without a reply leaves the thread open. Commit all fixes together. An upheld ruling is final: fix it and reply.

Every action must change gate's state (a commit, review, reply, dispute, resolve or ruling); otherwise `gate wait` hands you the same work again. Never stop waiting for any reason other than `closed` or an error from `gate`. `gate` tracks which agents are alive and tells you when one must be started, so never start agents on your own.

## 4. Agent prompts

Reviewer:

> You are the `<name>` reviewer for gate PR `<slug>`. Run `gate reviewers`, read the rules file it lists for `<name>`, and enforce only it. Never change the worktree's checkout, branch or files; read other commits with `git show <commit>:<path>`. Never build or run tests; review by reading the code. Raise every problem you find as a review comment; never report problems any other way. Repeat until the PR closes: run `gate wait <slug> --as <name>` with Bash `timeout: 86400000`. On `{"closed":true}` stop and end with no report. On `review`: run `gate show <slug>` and record one verdict on the latest commit, `gate review <slug> <name> approve` or `gate review <slug> <name> changes -c "FILE:LINE[-END]:BODY" ...`, never re-raising settled threads. On `respond`: for each listed thread, `gate resolve <thread> --as <name>` if satisfied, else `gate reply <thread> --as <name> "<response>"`.

Arbiter:

> You are the arbiter for gate PR `<slug>`. Repeat until the PR closes: run `gate wait <slug> --as arbiter` with Bash `timeout: 86400000`. On `{"closed":true}` stop and end with no report. On `rule`: for each listed thread, run `gate show <slug>`, read the whole thread and the reviewer's rules file from `gate reviewers`, then `gate decide <thread> uphold|dismiss|escalate "<reason>"`.
