//! `onyx-node proposals`: the reviewer's tool for the proposal queue (ROADMAP S5, READINESS
//! B2; docs/ops/proposal-review.md, THREATS REV-Q-1..REV-Q-7).
//!
//! It works on the two journal files alone ([`onyx_api::submit::decisions`]): it asks the
//! running node nothing, never takes the intake journal's lock and never writes that file. A
//! decision is one appended line in the decisions journal beside it, which the node re-reads.
//! The right to write the node's state directory is the only thing that makes someone a
//! reviewer.
//!
//! It never works as root on that directory (review B2-R1 H1). The node's own user owns it
//! and could plant a symbolic link under any name the tool opens; a root process following
//! one would hand that user a file anywhere on the host. Started as root, the tool goes into
//! the journal's directory, looks at the journal's name without following it, and gives up
//! root for the journal's owner (groups, group, user, in that order, for good) before it
//! opens anything; a journal owned by root, a link or anything but a regular file is refused.
//! The files it creates then belong to that user with no change of owner.

use onyx_api::submit::decisions::{
    decide, read_queue, render_list, render_show, Action, Checklist, DecideError, Printer,
    CHECKLIST_LINES, CLEARED, DROPPED, ON_HOLD,
};
use onyx_api::submit::SubmitLimits;
use std::path::Path;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::{SystemTime, UNIX_EPOCH};

const HELP: &str = "usage: onyx-node proposals list [--all] [--journal FILE]
       onyx-node proposals show ID [--journal FILE]
       onyx-node proposals clear ID --terms-reread yes --no-personal-data yes --entities-only yes
                                    --source-public yes --rights-level allowed|restricted [--journal FILE]
       onyx-node proposals hold ID --reason CODE [--journal FILE]
       onyx-node proposals drop ID --reason CODE [--journal FILE]
The review queue of the proposals the node recorded (docs/ops/proposal-review.md). FILE is the
intake journal (--journal, else ONYX_SUBMISSIONS). list prints how full the journal is and the
proposals waiting for a decision, oldest first (--all: the decided ones too); show prints one
proposal whole, for the reviewer only. clear accepts a proposal for the next step and needs every
checklist line answered yes and a rights level; it verifies and publishes nothing. hold and drop
need a reason code: terms_forbid, personal_data, not_entities, duplicate, unreachable_source,
out_of_scope, spam, or other:CODE (CODE: 1 to 32 of a-z, 0-9 and _). A proposer sees the state
and the reason code on the receipt, never a reviewer's text. Decisions are appended to
FILE's decisions journal (submissions.decisions.journal). The intake journal is never written,
and no decision is changed or removed; only a damaged decisions journal is copied aside
(.corrupt-TIME) and written afresh from its good lines, when the next decision is taken. Safe
while the node runs. Run it as the user that owns the journal; started as root it becomes that user
before it opens anything, and it refuses a journal that is a link or belongs to root. Words of a
proposal shaped like an e-mail address or a URL with a secret print as [withheld], and
invisible characters as \\u{..}. Exit: 0 done, 1 a journal cannot be read or written, 2 a usage
error, 3 the decision is refused (unknown id, already decided, checklist, reason).";

/// What `onyx-node proposals` was asked for.
#[derive(Debug, PartialEq, Eq)]
pub enum Ask {
    /// The queue (`all`: the decided ones too).
    List {
        /// Also the decided proposals.
        all: bool,
    },
    /// One proposal whole.
    Show(String),
    /// A decision on one proposal.
    Decide(String, Action),
}

/// Parse the arguments of `onyx-node proposals`. `journal` is `ONYX_SUBMISSIONS`.
/// `Ok(None)`: `--help`.
pub fn parse(args: &[String], journal: Option<String>) -> Result<Option<(PathBuf, Ask)>, String> {
    let mut it = args.iter();
    let verb = match it.next().map(String::as_str) {
        None | Some("-h" | "--help") => return Ok(None),
        Some(v @ ("list" | "show" | "clear" | "hold" | "drop")) => v,
        Some(other) => return Err(format!("unknown command {other:?}")),
    };
    let (mut journal, mut all, mut id, mut reason) = (journal, false, None, None);
    let mut answers: [Option<String>; 5] = Default::default();
    const FLAGS: [&str; 5] = [
        CHECKLIST_LINES[0].0,
        CHECKLIST_LINES[1].0,
        CHECKLIST_LINES[2].0,
        CHECKLIST_LINES[3].0,
        "--rights-level",
    ];
    while let Some(a) = it.next() {
        let mut value = || {
            it.next()
                .cloned()
                .ok_or_else(|| format!("{a} needs a value"))
        };
        match a.as_str() {
            "-h" | "--help" => return Ok(None),
            "--journal" => journal = Some(value()?),
            "--all" if verb == "list" => all = true,
            "--reason" if matches!(verb, "hold" | "drop") => reason = Some(value()?),
            flag if verb == "clear" && FLAGS.contains(&flag) => {
                let at = FLAGS.iter().position(|f| *f == flag).unwrap_or(0);
                answers[at] = Some(value()?);
            }
            flag if flag.starts_with('-') => {
                return Err(format!("unknown argument {flag:?} for {verb}"))
            }
            _ if id.is_none() && verb != "list" => id = Some(a.clone()),
            other => return Err(format!("unexpected argument {other:?}")),
        }
    }
    let journal = journal
        .map(|j| j.trim().to_string())
        .filter(|j| !j.is_empty())
        .ok_or("no intake journal: give --journal FILE or set ONYX_SUBMISSIONS")?;
    let need_id = || {
        id.clone()
            .ok_or(format!("{verb} needs a proposal id (sub_...)"))
    };
    let ask = match verb {
        "list" => Ask::List { all },
        "show" => Ask::Show(need_id()?),
        "clear" => {
            // A missing line stays empty: the decision is then refused with the line's name.
            let [terms, personal, entities, public, rights] =
                answers.map(Option::unwrap_or_default);
            Ask::Decide(
                need_id()?,
                Action::Clear(Checklist {
                    terms_reread_today: terms,
                    no_personal_data: personal,
                    entities_only: entities,
                    source_public_reachable: public,
                    rights_level: rights,
                }),
            )
        }
        _ => {
            let reason = reason.ok_or(format!("{verb} needs --reason CODE"))?;
            let id = need_id()?;
            Ask::Decide(
                id,
                if verb == "hold" {
                    Action::Hold(reason)
                } else {
                    Action::Drop(reason)
                },
            )
        }
    };
    Ok(Some((PathBuf::from(journal), ask)))
}

fn print(p: &Printer) -> ExitCode {
    print!("{}", p.text());
    if p.withheld > 0 {
        eprintln!(
            "onyx-node proposals: {} word(s) of the proposer's text print as [withheld]: each is shaped like an e-mail address or a URL that carries a secret (docs/ops/proposal-review.md)",
            p.withheld
        );
    }
    ExitCode::SUCCESS
}

/// As whom the tool works on a journal, by its own effective user and the journal's owner.
#[derive(Debug, PartialEq, Eq)]
pub enum RunAs {
    /// Not root: as it was started (the files' own modes decide what it may do).
    Unchanged,
    /// Root: give up root for the journal's owner first.
    DropTo {
        /// The journal's owner.
        uid: u32,
        /// The journal's group.
        gid: u32,
    },
    /// Root, and the journal's name is not plainly the node's own file. Refused, with why.
    Refuse(&'static str),
}

/// The rule of reviews B2-R1 H1 and B2-R2 L2: root never works on the state directory, and it
/// gives up root only for the user the directory itself belongs to. `owner_uid`, `owner_gid`
/// and `names` are the journal's (its number of hard links); `dir_uid` owns the directory it is
/// in. A journal of root has no user to drop to; one with a second name, or one whose owner is
/// not the directory's, could be another user's file linked into the directory, and the tool
/// would then run as that user.
pub fn run_as(euid: u32, owner_uid: u32, owner_gid: u32, names: u64, dir_uid: u32) -> RunAs {
    if euid != 0 {
        return RunAs::Unchanged;
    }
    if owner_uid == 0 {
        RunAs::Refuse("belongs to root: a node's journal belongs to the node's own user")
    } else if names != 1 {
        RunAs::Refuse("has more than one name (a hard link): it may be another user's file")
    } else if dir_uid != owner_uid {
        RunAs::Refuse("does not belong to the owner of its directory")
    } else {
        RunAs::DropTo {
            uid: owner_uid,
            gid: owner_gid,
        }
    }
}

/// Make this process one that others of its user cannot attach to or read (review B2-R2
/// L3): the node's user, which the tool runs as, could otherwise trace it and write through it
/// to the reviewer's terminal. Before anything of a proposal is read.
fn seal() -> Result<(), String> {
    #[cfg(target_os = "linux")]
    {
        use rustix::process::{set_dumpable_behavior, DumpableBehavior};
        set_dumpable_behavior(DumpableBehavior::NotDumpable)
            .map_err(|e| format!("cannot seal the process against tracing: {e}"))?;
    }
    Ok(())
}

/// The journal's path as the tool will use it, after giving up root when it was started as
/// root. Root first goes into the journal's directory (the service's private directory cannot
/// be entered by name by anyone else) and then works on `./<name>` as the journal's owner.
fn enter(journal: &Path) -> Result<PathBuf, String> {
    use std::os::unix::fs::MetadataExt;
    if rustix::process::geteuid().as_raw() != 0 {
        return Ok(journal.to_path_buf());
    }
    let name = journal
        .file_name()
        .ok_or_else(|| format!("{}: not a file name", journal.display()))?;
    let dir = match journal.parent() {
        Some(d) if !d.as_os_str().is_empty() => d,
        _ => Path::new("."),
    };
    std::env::set_current_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let here = Path::new(".").join(name);
    let meta = std::fs::symlink_metadata(&here).map_err(|e| {
        format!(
            "{}: {e} (no intake journal here: the node writes it when ONYX_SUBMISSIONS is set)",
            journal.display()
        )
    })?;
    if !meta.file_type().is_file() {
        return Err(format!(
            "{} is a link or not a regular file: refused, nothing was opened",
            journal.display()
        ));
    }
    let dir_uid = std::fs::symlink_metadata(".")
        .map_err(|e| format!("{}: {e}", dir.display()))?
        .uid();
    match run_as(0, meta.uid(), meta.gid(), meta.nlink(), dir_uid) {
        RunAs::DropTo { uid, gid } => {
            drop_root(uid, gid)?;
            // The uid change made the process traceable by its new user again.
            seal()?;
            Ok(here)
        }
        RunAs::Refuse(why) => Err(format!(
            "{} {why}: refused, nothing was opened (this tool never works as root on a state directory)",
            journal.display()
        )),
        RunAs::Unchanged => Ok(here),
    }
}

/// Give up root for `uid`/`gid` for good, and prove it: afterwards neither the user nor the
/// group is root's and root cannot be taken back. The tool has one thread at this point.
#[cfg(not(target_os = "linux"))]
fn drop_root(_uid: u32, _gid: u32) -> Result<(), String> {
    Err("this tool gives up root only on Linux: run it as the user that owns the journal".into())
}

/// Give up root for `uid`/`gid` for good, and prove it: afterwards neither the user nor the
/// group is root's and root cannot be taken back. The tool has one thread at this point.
#[cfg(target_os = "linux")]
fn drop_root(uid: u32, gid: u32) -> Result<(), String> {
    use rustix::process::{getegid, geteuid, getgid, getuid};
    use rustix::thread::{set_thread_gid, set_thread_groups, set_thread_uid};
    use rustix::{fs::Gid, fs::Uid};
    let fail = |what: &str, e: rustix::io::Errno| {
        format!("cannot give up root for the journal's owner ({what}: {e}); nothing was opened")
    };
    set_thread_groups(&[]).map_err(|e| fail("groups", e))?;
    set_thread_gid(Gid::from_raw(gid)).map_err(|e| fail("group", e))?;
    set_thread_uid(Uid::from_raw(uid)).map_err(|e| fail("user", e))?;
    let now = (
        getuid().as_raw(),
        geteuid().as_raw(),
        getgid().as_raw(),
        getegid().as_raw(),
    );
    if now != (uid, uid, gid, gid) || set_thread_uid(Uid::from_raw(0)).is_ok() {
        return Err("root was not given up for the journal's owner; nothing was opened".into());
    }
    Ok(())
}

/// What the reviewer is told after a decision: the state, and honestly what follows it.
fn after(state: &str) -> &'static str {
    match state {
        CLEARED => "The receipt now shows: cleared for the next step. Nothing is verified or published by this. Next, by hand: docs/ops/proposal-review.md \"After a clear\".",
        ON_HOLD => "The receipt now shows: on hold, with the reason code. Decide it later with clear or drop.",
        DROPPED => "The receipt now shows: will not proceed, with the reason code. This is final; the proposal's text stays in the private journal (docs/ops/data-retention.md).",
        _ => "",
    }
}

/// `onyx-node proposals ...` (exit codes in the help text).
pub fn main(args: &[String]) -> ExitCode {
    let (journal, ask) = match parse(args, std::env::var("ONYX_SUBMISSIONS").ok()) {
        Ok(Some(x)) => x,
        Ok(None) => {
            println!("{HELP}");
            return ExitCode::SUCCESS;
        }
        Err(e) => {
            eprintln!("onyx-node proposals: {e}\n{HELP}");
            return ExitCode::from(2);
        }
    };
    let journal = match seal().and_then(|()| enter(&journal)) {
        Ok(j) => j,
        Err(e) => {
            eprintln!("onyx-node proposals: {e}");
            return ExitCode::FAILURE;
        }
    };
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let capacity = SubmitLimits::default().max_recorded;
    let storage = |e: std::io::Error| {
        eprintln!(
            "onyx-node proposals: {e} (run it as the user that owns the node's state directory)"
        );
        ExitCode::FAILURE
    };
    match ask {
        Ask::List { all } => match read_queue(&journal, capacity) {
            Ok(q) => print(&render_list(&q, now, all)),
            Err(e) => storage(e),
        },
        Ask::Show(id) => match read_queue(&journal, capacity) {
            Ok(q) => match q.proposals.iter().find(|p| p.item.id == id) {
                Some(p) => print(&render_show(p, now)),
                None => {
                    eprintln!("onyx-node proposals: {}", DecideError::NotFound);
                    ExitCode::from(3)
                }
            },
            Err(e) => storage(e),
        },
        Ask::Decide(id, action) => match decide(&journal, capacity, &id, action, now) {
            Ok(done) => {
                let p = &done.proposal;
                if let Some(copy) = &done.set_aside {
                    eprintln!(
                        "onyx-node proposals: the decisions journal was damaged: it was copied to {} and written afresh from its good lines before this decision (docs/ops/proposal-review.md \"Recovery\")",
                        copy.display()
                    );
                }
                println!("{}  {}", p.item.id, p.state());
                println!("{}", after(p.state()));
                ExitCode::SUCCESS
            }
            Err(DecideError::Storage(e)) => storage(e),
            Err(e) => {
                eprintln!("onyx-node proposals: refused, nothing written: {e}");
                ExitCode::from(3)
            }
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(s: &str) -> Vec<String> {
        s.split_whitespace().map(str::to_owned).collect()
    }

    fn ask(s: &str) -> Result<Option<(PathBuf, Ask)>, String> {
        parse(&args(s), Some("/state/submissions.journal".into()))
    }

    #[test]
    fn the_arguments_are_read_and_a_wrong_one_is_refused() {
        let j = PathBuf::from("/state/submissions.journal");
        assert_eq!(ask("list"), Ok(Some((j.clone(), Ask::List { all: false }))));
        assert_eq!(
            ask("list --all --journal /x/j"),
            Ok(Some((PathBuf::from("/x/j"), Ask::List { all: true })))
        );
        assert_eq!(
            ask("show sub_1"),
            Ok(Some((j.clone(), Ask::Show("sub_1".into()))))
        );
        assert_eq!(
            ask("hold sub_1 --reason duplicate"),
            Ok(Some((
                j.clone(),
                Ask::Decide("sub_1".into(), Action::Hold("duplicate".into()))
            )))
        );
        assert_eq!(
            ask("drop --reason other:late sub_1"),
            Ok(Some((
                j.clone(),
                Ask::Decide("sub_1".into(), Action::Drop("other:late".into()))
            )))
        );
        assert_eq!(ask(""), Ok(None));
        assert_eq!(ask("clear sub_1 --help"), Ok(None));
        for bad in [
            "decide sub_1",
            "list sub_1",
            "list --reason spam",
            "show",
            "show sub_1 sub_2",
            "hold sub_1",
            "hold --reason spam",
            "drop sub_1 --reason",
            "drop sub_1 --terms-reread yes --reason spam",
            "clear sub_1 --reason spam",
            "clear sub_1 --all",
            "show sub_1 --journal",
        ] {
            assert!(ask(bad).is_err(), "{bad}");
        }
        assert!(parse(&args("list"), None).is_err(), "no journal named");
        assert!(parse(&args("list"), Some("  ".into())).is_err());
    }

    /// Review B2-R1 H1: root never works on the state directory. Root drops to the journal's
    /// owner, a journal owned by root is refused, and anyone else stays who they are.
    #[test]
    fn root_drops_to_the_journals_owner_or_refuses() {
        let node = (64_123, 64_124);
        assert_eq!(
            run_as(0, node.0, node.1, 1, node.0),
            RunAs::DropTo {
                uid: 64_123,
                gid: 64_124
            }
        );
        assert!(matches!(run_as(0, 0, 0, 1, 0), RunAs::Refuse(_)));
        assert!(
            matches!(run_as(0, 0, 64_124, 1, node.0), RunAs::Refuse(_)),
            "a group does not help"
        );
        // Review B2-R2 L2: another user's file linked into the node's directory.
        assert!(
            matches!(run_as(0, 1_000, 1_000, 2, node.0), RunAs::Refuse(_)),
            "a second name"
        );
        assert!(
            matches!(run_as(0, 1_000, 1_000, 1, node.0), RunAs::Refuse(_)),
            "not the directory's owner"
        );
        assert!(matches!(
            run_as(0, node.0, node.1, 2, node.0),
            RunAs::Refuse(_)
        ));
        assert!(matches!(run_as(0, node.0, node.1, 1, 0), RunAs::Refuse(_)));
        assert!(matches!(
            run_as(0, node.0, node.1, 0, node.0),
            RunAs::Refuse(_)
        ));
        assert_eq!(run_as(1_000, 1_000, 1_000, 1, 1_000), RunAs::Unchanged);
        assert_eq!(run_as(1_000, 64_123, 64_123, 3, 0), RunAs::Unchanged);
        assert_eq!(
            run_as(1_000, 0, 0, 1, 0),
            RunAs::Unchanged,
            "only root can drop"
        );
        for owner in [1u32, 999, 64_123, u32::MAX - 1] {
            for names in [0u64, 1, 2] {
                for dir in [0u32, owner, owner + 1] {
                    assert_ne!(
                        run_as(0, owner, 0, names, dir),
                        RunAs::Unchanged,
                        "root never stays root"
                    );
                }
            }
        }
    }

    /// Review B2-R2 L3: the tool cannot be traced by another process of its user.
    #[cfg(target_os = "linux")]
    #[test]
    fn the_process_is_sealed_against_tracing() {
        use rustix::process::{dumpable_behavior, DumpableBehavior};
        seal().expect("seal");
        assert_eq!(
            dumpable_behavior().expect("read"),
            DumpableBehavior::NotDumpable
        );
    }

    /// `clear` carries exactly the answers given: a line left out stays empty, so the decision
    /// is refused with that line's name instead of being guessed.
    #[test]
    fn clear_carries_each_checklist_answer_and_guesses_none() {
        let full = "clear sub_1 --terms-reread yes --no-personal-data yes --entities-only yes --source-public yes --rights-level allowed";
        let Ok(Some((_, Ask::Decide(_, Action::Clear(list))))) = ask(full) else {
            panic!("clear");
        };
        assert_eq!(list.complete(), Ok(()));
        for (n, flag) in [
            "--terms-reread",
            "--no-personal-data",
            "--entities-only",
            "--source-public",
            "--rights-level",
        ]
        .iter()
        .enumerate()
        {
            let words: Vec<&str> = full.split_whitespace().collect();
            let at = 2 + 2 * n;
            assert_eq!(words[at], *flag);
            let without = [&words[..at], &words[at + 2..]].concat().join(" ");
            let Ok(Some((_, Ask::Decide(_, Action::Clear(list))))) = ask(&without) else {
                panic!("{without}");
            };
            assert_eq!(list.complete(), Err(*flag), "{without}");
            let mut no = words.clone();
            no[at + 1] = "no";
            let Ok(Some((_, Ask::Decide(_, Action::Clear(list))))) = ask(&no.join(" ")) else {
                panic!("no");
            };
            assert_eq!(list.complete(), Err(*flag));
        }
    }
}
