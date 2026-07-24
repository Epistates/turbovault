//! Working-tree state construction for the WSS write-safety matrix.
//!
//! Builds a target path into one of the nine
//! `[e]xists[t]racked[c]ommitted[s]taged[u]nstaged` states via the real `git`
//! CLI — the state recipes *are* git commands (design doc §5), so driving real
//! git is both the clearest and the most faithful construction. After building,
//! resolves the per-path version tokens (HEAD / INDEX / WORKDIR blob oids) the
//! precondition axis draws from.
//!
//! The five-character code string (`code()`) is the single source of truth for
//! a state's `(e,t,c,s,u)` flags — every predicate reads it, so the enum and
//! the matrix column headers cannot drift.

use std::path::Path;
use std::process::{Command, Output};

/// The `(e,t,c,s,u)` working-tree state of a target path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GitState {
    /// `-----` — absent from the working tree entirely.
    Absent,
    /// `etc--` — committed, working tree == index == HEAD (clean).
    CleanCommitted,
    /// `etcs-` — committed, then modified + staged (index != HEAD, wt == index).
    CommittedStaged,
    /// `etc-u` — committed, then modified in the working tree (wt != index == HEAD).
    CommittedUnstaged,
    /// `etcsu` — committed, staged a change, then modified again (wt != index != HEAD).
    CommittedStagedUnstaged,
    /// `et-s-` — new file, `git add`, never committed (index != HEAD-absent, wt == index).
    NewStaged,
    /// `et--u` — tracked via `git add --intent-to-add`: an index entry with no
    /// staged content, working-tree content present (the odd `¬c ¬s u` state).
    IntentToAdd,
    /// `et-su` — new file staged, then modified in the working tree (wt != index, uncommitted).
    NewStagedUnstaged,
    /// `e---u` — present on disk, never tracked (untracked).
    Untracked,
}

use GitState::*;

impl GitState {
    /// Every state, in matrix column order.
    pub const ALL: [GitState; 9] = [
        Absent,
        CleanCommitted,
        CommittedStaged,
        CommittedUnstaged,
        CommittedStagedUnstaged,
        NewStaged,
        IntentToAdd,
        NewStagedUnstaged,
        Untracked,
    ];

    /// The `etcsu`-notation code — the single source of truth for the flags,
    /// and the label used in self-describing test names.
    pub fn code(self) -> &'static str {
        match self {
            Absent => "-----",
            CleanCommitted => "etc--",
            CommittedStaged => "etcs-",
            CommittedUnstaged => "etc-u",
            CommittedStagedUnstaged => "etcsu",
            NewStaged => "et-s-",
            IntentToAdd => "et--u",
            NewStagedUnstaged => "et-su",
            Untracked => "e---u",
        }
    }

    fn flag(self, i: usize) -> bool {
        self.code().as_bytes()[i] != b'-'
    }

    /// `e` — present in the working tree.
    pub fn exists(self) -> bool {
        self.flag(0)
    }
    /// `t` — known to git (has an index entry or is committed).
    pub fn tracked(self) -> bool {
        self.flag(1)
    }
    /// `c` — has content in the HEAD tree.
    pub fn committed(self) -> bool {
        self.flag(2)
    }
    /// `s` — a staged change relative to HEAD exists.
    pub fn staged(self) -> bool {
        self.flag(3)
    }
    /// `u` — working-tree content differs from the index.
    pub fn unstaged(self) -> bool {
        self.flag(4)
    }
}

/// Per-path version tokens resolved after building a state. A field is `Some`
/// only when that token is *defined* for the state (the matrix's N/A rule):
/// HEAD iff committed, INDEX iff staged, WORKDIR iff exists.
#[derive(Clone, Debug)]
pub struct Oids {
    pub head: Option<String>,
    pub index: Option<String>,
    pub workdir: Option<String>,
}

/// Three content generations used across the staged/unstaged states.
const V1: &str = "v1 committed\n";
const V2: &str = "v2 staged\n";
const V3: &str = "v3 workdir\n";

fn git(repo: &Path, args: &[&str]) -> Output {
    // Hermetic git: neutralize the user's global/system config so the suite is
    // portable and identical on every machine + CI (the upstream-shareability
    // requirement). Without this, e.g. `status.showUntrackedFiles=no` or a
    // global `core.excludesfile` silently changes what states are observable.
    // ponytail: /dev/null is unix-only; if this suite ever runs on Windows,
    // swap for an empty temp file.
    Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .expect("failed to spawn git")
}

fn git_ok(repo: &Path, args: &[&str]) {
    let out = git(repo, args);
    assert!(
        out.status.success(),
        "git {:?} failed: {}",
        args,
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Trimmed stdout of a git command; `None` if it exited non-zero.
fn git_capture(repo: &Path, args: &[&str]) -> Option<String> {
    let out = git(repo, args);
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// A fresh git repo with an initial seed commit (so HEAD always exists, even
/// for the absent/untracked states) and a committer identity configured. The
/// matrix target file is NOT created here.
pub fn new_seeded_repo() -> tempfile::TempDir {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let p = tmp.path();
    git_ok(p, &["init", "-b", "main"]);
    git_ok(p, &["config", "user.name", "wss-test"]);
    git_ok(p, &["config", "user.email", "wss@test.local"]);
    std::fs::write(p.join(".seed"), "seed\n").unwrap();
    git_ok(p, &["add", ".seed"]);
    git_ok(p, &["commit", "-m", "seed"]);
    tmp
}

/// Write the target at `rel` and build `state` around it. `repo` must already
/// be a seeded repo ([`new_seeded_repo`]). Returns the resolved version tokens.
pub fn build_state(repo: &Path, rel: &str, state: GitState) -> Oids {
    let path = repo.join(rel);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }

    let commit_v1 = |repo: &Path| {
        std::fs::write(&path, V1).unwrap();
        git_ok(repo, &["add", rel]);
        // Path-scoped commit: commit ONLY `rel`, leaving any other path's staged
        // content untouched. This makes building several independent paths
        // order-independent — a dual-path op (move) can build its destination
        // (staged) via the runner, then its source (committed) inside `invoke`,
        // without the source's commit sweeping up the destination's staged state.
        git_ok(repo, &["commit", "-m", "add target", "--", rel]);
    };

    match state {
        Absent => {
            let _ = std::fs::remove_file(&path);
        }
        CleanCommitted => commit_v1(repo),
        CommittedStaged => {
            commit_v1(repo);
            std::fs::write(&path, V2).unwrap();
            git_ok(repo, &["add", rel]);
        }
        CommittedUnstaged => {
            commit_v1(repo);
            std::fs::write(&path, V2).unwrap(); // no add
        }
        CommittedStagedUnstaged => {
            commit_v1(repo);
            std::fs::write(&path, V2).unwrap();
            git_ok(repo, &["add", rel]);
            std::fs::write(&path, V3).unwrap(); // no add
        }
        NewStaged => {
            std::fs::write(&path, V1).unwrap();
            git_ok(repo, &["add", rel]);
        }
        IntentToAdd => {
            std::fs::write(&path, V1).unwrap();
            git_ok(repo, &["add", "--intent-to-add", rel]);
        }
        NewStagedUnstaged => {
            std::fs::write(&path, V1).unwrap();
            git_ok(repo, &["add", rel]);
            std::fs::write(&path, V2).unwrap(); // no add
        }
        Untracked => {
            std::fs::write(&path, V1).unwrap();
        }
    }

    Oids {
        head: state
            .committed()
            .then(|| git_capture(repo, &["rev-parse", &format!("HEAD:{rel}")]))
            .flatten(),
        index: state
            .staged()
            .then(|| git_capture(repo, &["rev-parse", &format!(":{rel}")]))
            .flatten(),
        workdir: state
            .exists()
            .then(|| git_capture(repo, &["hash-object", rel]))
            .flatten(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Measure the actual `(e,t,c,s,u)` flags from git *independently* of how
    /// `build_state` gated them, so the assertion validates that construction
    /// reached the intended state rather than just re-reading its own inputs.
    fn measured_flags(repo: &Path, rel: &str) -> (bool, bool, bool, bool, bool) {
        let e = repo.join(rel).exists();
        let t = git_capture(repo, &["ls-files", "--", rel]).is_some_and(|s| !s.is_empty());
        let c = git(
            repo,
            &["rev-parse", "--verify", "--quiet", &format!("HEAD:{rel}")],
        )
        .status
        .success();
        // `--quiet` implies `--exit-code`: non-zero == a diff exists.
        let s = !git(repo, &["diff", "--cached", "--quiet", "--", rel])
            .status
            .success();
        // Untracked files are invisible to `git diff`; detect their `u` via the
        // porcelain `??` marker instead.
        let porcelain =
            git_capture(repo, &["status", "--porcelain", "--", rel]).unwrap_or_default();
        let u = if porcelain.starts_with("??") {
            true
        } else {
            !git(repo, &["diff", "--quiet", "--", rel]).status.success()
        };
        (e, t, c, s, u)
    }

    #[test]
    fn builds_all_nine_states_correctly() {
        for state in GitState::ALL {
            let repo = new_seeded_repo();
            let oids = build_state(repo.path(), "note.md", state);

            let expected = (
                state.exists(),
                state.tracked(),
                state.committed(),
                state.staged(),
                state.unstaged(),
            );
            let actual = measured_flags(repo.path(), "note.md");
            assert_eq!(
                actual,
                expected,
                "state {} ({}): measured (e,t,c,s,u) mismatch",
                state.code(),
                format_args!("{state:?}")
            );

            // Token defined-ness must match the flags (the matrix's N/A rule).
            assert_eq!(
                oids.head.is_some(),
                state.committed(),
                "{}: HEAD token defined iff committed",
                state.code()
            );
            assert_eq!(
                oids.index.is_some(),
                state.staged(),
                "{}: INDEX token defined iff staged",
                state.code()
            );
            assert_eq!(
                oids.workdir.is_some(),
                state.exists(),
                "{}: WORKDIR token defined iff exists",
                state.code()
            );
        }
    }

    /// The token *values* must reflect the content generations: WORKDIR tracks
    /// the last-written bytes; HEAD tracks the committed bytes; and where a
    /// state has diverging generations, the tokens must differ.
    #[test]
    fn token_values_track_content_generations() {
        let repo = new_seeded_repo();

        // etcsu: three distinct generations => three distinct tokens.
        let o = build_state(repo.path(), "a.md", GitState::CommittedStagedUnstaged);
        assert!(o.head.is_some() && o.index.is_some() && o.workdir.is_some());
        assert_ne!(o.head, o.index, "etcsu: HEAD (v1) != INDEX (v2)");
        assert_ne!(o.index, o.workdir, "etcsu: INDEX (v2) != WORKDIR (v3)");
        assert_ne!(o.head, o.workdir, "etcsu: HEAD (v1) != WORKDIR (v3)");

        // etc--: clean => WORKDIR == HEAD (the matrix's SKIP:E-row basis).
        let o = build_state(repo.path(), "b.md", GitState::CleanCommitted);
        assert_eq!(o.workdir, o.head, "etc--: WORKDIR == HEAD when clean");

        // et-s-: staged new, no unstaged => WORKDIR == INDEX (the SKIP:I basis).
        let o = build_state(repo.path(), "c.md", GitState::NewStaged);
        assert_eq!(o.workdir, o.index, "et-s-: WORKDIR == INDEX (no unstaged)");
        assert!(o.head.is_none(), "et-s-: uncommitted => no HEAD token");
    }
}
