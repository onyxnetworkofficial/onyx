//! Publisher pointers on the node (ADR-0066 item 7, READINESS B5): the optional routes, and
//! `onyx-node pointers`, the reviewer's read of the journal (docs/ops/publisher-review.md
//! "Pointers sent to the node", THREATS PUBON-PTR-7).
//!
//! The command works on the journal file alone: it asks the running node nothing, takes no
//! lock and writes nothing, so it is safe while the node runs. Like `onyx-node proposals` it
//! never works as root on the state directory: started as root it goes into the journal's
//! directory, looks at the journal's name without following it, and gives up root for the
//! journal's owner before it opens anything ([`crate::proposals::run_as`], review B2-R1 H1).

use axum::Router;
use onyx_api::publishers::{
    drop_pointer, is_id, read_drops, read_pointers, render_list, undrop_pointer, DropError,
    Dropped, PointerLimits, PointerStore, Undropped, PRUNE_EVERY, RECEIVED,
};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

/// The pointer routes for the explicitly configured journal. The parent directory must exist
/// and be private to the node's user. Unset is an empty router; a bad path fails startup.
/// Called inside the node's runtime: with a journal it starts the hourly removal task.
pub fn routes(path: Option<&Path>) -> std::io::Result<Router> {
    match path {
        None => Ok(Router::new()),
        Some(path) => {
            let store = PointerStore::open(path, PointerLimits::default())?;
            // One warning line at start when the journal is near its recorded cap.
            store.warn_if_near_cap();
            eprintln!(
                "onyx-node: publisher pointers on (POST /v1/publishers/pointer): an origin and a hash per pointer, kept in {}; the node contacts no origin and keeps no client address",
                path.display()
            );
            let store = Arc::new(store);
            // Review PTR-R1 L2: entries past their kept time leave every hour, also on a node
            // that takes no pointer for weeks.
            tokio::spawn(onyx_api::publishers::run_prune(store.clone(), PRUNE_EVERY));
            Ok(onyx_api::publishers::routes(store))
        }
    }
}

const HELP: &str = "usage: onyx-node pointers list [--all] [--journal FILE]
       onyx-node pointers show ID [--journal FILE]
       onyx-node pointers drop ID [--journal FILE]
       onyx-node pointers undrop ID [--journal FILE]
The publisher pointers the node recorded (docs/ops/publisher-review.md). FILE is the pointer
journal (--journal, else ONYX_PUBLISHER_POINTERS). list prints how full the journal is and the
pointers that wait, oldest first, each with its receipt id, the time it has left, the origin
and the application's hash (--all: the expired and the dropped ones too, which the node keeps
until 12 days after they came; an entry older than that is never shown).
show prints one waiting pointer as the two lines `onyx-publish verify-application --pointer
FILE` reads; save them to a file and run that command on a throwaway machine, never here: this
tool and the node contact no origin. drop marks a waiting pointer as dropped, for one reason
only: the file at its origin does not hash to the pointer's hash (verify-application said so).
The pointer then holds no place of its host, and its receipt says dropped. undrop takes a drop
back (a wrong id, or a file the applicant was still uploading): the receipt answers by its age
again and the pointer holds its place again. list and show write
nothing and take no lock; drop and undrop write only the drops journal beside FILE
(publisher-pointers.drops.journal), never to FILE. All are safe while the node runs. Run it as the user that owns the journal; started as root it becomes that user
before it opens anything, and it refuses a journal that is a link or belongs to root. Exit: 0
done, 1 a journal cannot be read or written, 2 a usage error, 3 no waiting pointer has this id
(undrop: no drop of this id is held).";

/// What `onyx-node pointers` was asked for.
#[derive(Debug, PartialEq, Eq)]
pub enum Ask {
    /// The list (`all`: the expired ones too).
    List {
        /// Also the expired pointers.
        all: bool,
    },
    /// One pointer as the two lines of a pointer file.
    Show(String),
    /// Mark one waiting pointer as dropped (its hash is not the file's).
    Drop(String),
    /// Take a drop back.
    Undrop(String),
}

/// Parse the arguments of `onyx-node pointers`. `journal` is `ONYX_PUBLISHER_POINTERS`.
/// `Ok(None)`: `--help`.
pub fn parse(args: &[String], journal: Option<String>) -> Result<Option<(PathBuf, Ask)>, String> {
    let mut it = args.iter();
    let verb = match it.next().map(String::as_str) {
        None | Some("-h" | "--help") => return Ok(None),
        Some(v @ ("list" | "show" | "drop" | "undrop")) => v,
        Some(other) => return Err(format!("unknown command {other:?}")),
    };
    let (mut journal, mut all, mut id) = (journal, false, None);
    while let Some(a) = it.next() {
        match a.as_str() {
            "-h" | "--help" => return Ok(None),
            "--journal" => {
                journal = Some(
                    it.next()
                        .cloned()
                        .ok_or_else(|| format!("{a} needs a value"))?,
                )
            }
            "--all" if verb == "list" => all = true,
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
        .ok_or("no pointer journal: give --journal FILE or set ONYX_PUBLISHER_POINTERS")?;
    let ask = match verb {
        "list" => Ask::List { all },
        _ => {
            let id = id.ok_or(format!(
                "{verb} needs a receipt id (ptr_ and 32 hex digits)"
            ))?;
            if !is_id(&id) {
                return Err(format!(
                    "{id:?} is not a receipt id (ptr_ and 32 hex digits)"
                ));
            }
            match verb {
                "show" => Ask::Show(id),
                "drop" => Ask::Drop(id),
                _ => Ask::Undrop(id),
            }
        }
    };
    Ok(Some((PathBuf::from(journal), ask)))
}

/// `onyx-node pointers ...` (exit codes in the help text).
pub fn main(args: &[String]) -> ExitCode {
    let (journal, ask) = match parse(args, std::env::var("ONYX_PUBLISHER_POINTERS").ok()) {
        Ok(Some(x)) => x,
        Ok(None) => {
            println!("{HELP}");
            return ExitCode::SUCCESS;
        }
        Err(e) => {
            eprintln!("onyx-node pointers: {e}\n{HELP}");
            return ExitCode::from(2);
        }
    };
    let entered = crate::proposals::seal().and_then(|()| {
        crate::proposals::enter(
            &journal,
            "no pointer journal here: the node writes it when ONYX_PUBLISHER_POINTERS is set",
        )
    });
    let journal = match entered {
        Ok(j) => j,
        Err(e) => {
            eprintln!("onyx-node pointers: {e}");
            return ExitCode::FAILURE;
        }
    };
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let capacity = PointerLimits::default().max_recorded;
    let storage = |e: std::io::Error| {
        eprintln!(
            "onyx-node pointers: {e} (run it as the user that owns the node's state directory)"
        );
        ExitCode::FAILURE
    };
    if let Ask::Drop(id) = &ask {
        return match drop_pointer(&journal, capacity, id, now) {
            Ok(Dropped::Now) => {
                println!("{id}  dropped\nIts receipt now says: dropped, the file at the origin is not the one this pointer names. Its place is free for that host.");
                ExitCode::SUCCESS
            }
            Ok(Dropped::Already) => {
                println!("{id}  dropped\nIt was dropped before; nothing was written.");
                ExitCode::SUCCESS
            }
            Err(DropError::NotWaiting) => {
                eprintln!("onyx-node pointers: no waiting pointer has the id {id}: nothing written (list --all shows the expired and the dropped ones)");
                ExitCode::from(3)
            }
            Err(DropError::Storage(e)) => storage(e),
        };
    }
    if let Ask::Undrop(id) = &ask {
        return match undrop_pointer(&journal, id) {
            Ok(Undropped::Now) => {
                println!("{id}  not dropped\nThe drop was taken back. Its receipt answers by its age again, and a waiting pointer holds its place again.");
                ExitCode::SUCCESS
            }
            Ok(Undropped::NotDropped) => {
                eprintln!("onyx-node pointers: no drop of the id {id} is held: nothing written (list --all shows the dropped ones)");
                ExitCode::from(3)
            }
            Err(DropError::NotWaiting) => ExitCode::from(3),
            Err(DropError::Storage(e)) => storage(e),
        };
    }
    let read = read_pointers(&journal, capacity).and_then(|p| read_drops(&journal).map(|d| (p, d)));
    let (pointers, drops) = match read {
        Ok(x) => x,
        Err(e) => return storage(e),
    };
    match ask {
        Ask::List { all } => {
            print!("{}", render_list(&pointers, &drops, capacity, now, all));
            ExitCode::SUCCESS
        }
        Ask::Show(id) | Ask::Drop(id) | Ask::Undrop(id) => match pointers
            .iter()
            .find(|p| p.id == id && p.state_with(&drops, now) == RECEIVED)
        {
            Some(p) => {
                print!("{}", p.text());
                ExitCode::SUCCESS
            }
            None => {
                eprintln!("onyx-node pointers: no waiting pointer has the id {id} (list --all shows the expired and the dropped ones)");
                ExitCode::from(3)
            }
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ask(s: &str) -> Result<Option<(PathBuf, Ask)>, String> {
        let args: Vec<String> = s.split_whitespace().map(str::to_owned).collect();
        parse(&args, Some("/state/publisher-pointers.journal".into()))
    }

    #[test]
    fn the_arguments_are_read_and_a_wrong_one_is_refused() {
        let j = PathBuf::from("/state/publisher-pointers.journal");
        let id = format!("ptr_{}", "0a".repeat(16));
        assert_eq!(ask("list"), Ok(Some((j.clone(), Ask::List { all: false }))));
        assert_eq!(
            ask("list --all --journal /x/j"),
            Ok(Some((PathBuf::from("/x/j"), Ask::List { all: true })))
        );
        assert_eq!(
            ask(&format!("show {id}")),
            Ok(Some((j, Ask::Show(id.clone()))))
        );
        assert_eq!(ask(""), Ok(None));
        assert_eq!(ask("list --help"), Ok(None));
        assert_eq!(
            ask(&format!("drop {id}")),
            Ok(Some((
                PathBuf::from("/state/publisher-pointers.journal"),
                Ask::Drop(id.clone())
            )))
        );
        assert_eq!(
            ask(&format!("undrop {id}")).map(|x| x.map(|(_, a)| a)),
            Ok(Some(Ask::Undrop(id.clone())))
        );
        for bad in [
            "undrop".to_string(),
            format!("undrop {id} --all"),
            "drop".to_string(),
            "drop ptr_1".to_string(),
            format!("drop {id} --all"),
            format!("drop {id} --reason spam"),
            "decide x".to_string(),
            "clear".to_string(),
            format!("list {id}"),
            "list --everything".to_string(),
            "show".to_string(),
            "show sub_0123456789abcdef0123456789abcdef".to_string(),
            "show ptr_1".to_string(),
            format!("show {id} {id}"),
            format!("show {id} --all"),
            format!("show {id} --journal"),
            format!("show {}", id.to_uppercase()),
        ] {
            assert!(ask(&bad).is_err(), "{bad}");
        }
        let list = ["list".to_string()];
        assert!(parse(&list, None).is_err(), "no journal named");
        assert!(parse(&list, Some("  ".into())).is_err());
    }

    /// THREATS PUBON-PTR-7: the list tool lives by the proposal tool's privilege rule. Root
    /// never reads the state directory as root: it becomes the journal's owner or refuses.
    #[test]
    fn root_drops_to_the_pointer_journals_owner_or_refuses() {
        use crate::proposals::{run_as, RunAs};
        let node = (64_123, 64_124);
        assert_eq!(
            run_as(0, node.0, node.1, 1, node.0),
            RunAs::DropTo {
                uid: node.0,
                gid: node.1
            }
        );
        for (owner, names, dir, why) in [
            (0, 1, 0, "a journal of root"),
            (node.0, 2, node.0, "a second name"),
            (1_000, 1, node.0, "not the directory's owner"),
        ] {
            assert!(
                matches!(run_as(0, owner, node.1, names, dir), RunAs::Refuse(_)),
                "{why}"
            );
        }
        assert_eq!(run_as(1_000, node.0, node.1, 1, node.0), RunAs::Unchanged);
        // The command itself goes through that rule before it opens the journal.
        let src = include_str!("pointers.rs");
        let (enter, read, write) = (
            src.find("crate::proposals::enter(").expect("enter"),
            src.find("read_pointers(&journal").expect("read"),
            src.find("drop_pointer(&journal").expect("drop"),
        );
        assert!(enter < src.find("undrop_pointer(&journal").expect("undrop"));
        assert!(enter < read, "the journal is read only after the rule");
        assert!(enter < write, "a drop is written only after the rule");
        assert!(src.find("crate::proposals::seal()").expect("seal") < enter);
    }
}
