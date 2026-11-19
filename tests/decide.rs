//! The decider slot: any program can make Asqu's risk judgement stricter, never looser.
//! (Its own test program, so it can point Asqu's settings folder at a temporary one.)
#![cfg(unix)]

use asqu::tries::{KeepOptions, KeepOutcome};
use asqu::{Store, Why};
use std::fs;

fn keep_with_decider(home: &std::path::Path, decider: &str, risky: bool) -> KeepOutcome {
    fs::write(home.join("decider.toml"), format!("command = {decider:?}\ntimeout_secs = 2\n")).unwrap();
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("readme.md"), "hello").unwrap();
    fs::write(dir.path().join("old.txt"), "x").unwrap();
    let mut store = Store::open_at(dir.path()).unwrap();
    store.save(&Why::default()).unwrap();
    let t = store.start_try("t", "Fix a typo", "me").unwrap();
    if risky {
        fs::remove_file(t.join("old.txt")).unwrap(); // the rules call this high risk
    } else {
        fs::write(t.join("readme.md"), "hello!").unwrap(); // the rules call this low risk
    }
    store.keep_try("t", KeepOptions::default(), "me").unwrap()
}

#[test]
fn a_decider_can_only_make_things_stricter() {
    let home = tempfile::tempdir().unwrap();
    std::env::set_var("ASQU_HOME", home.path()); // the only test in this program

    // It reads the question and raises a harmless-looking change to high: the person is asked.
    let says_high = r#"grep -q '"question":"risk"' && echo '{"level":"high","reason":"the readme holds deploy steps"}'"#;
    match keep_with_decider(home.path(), says_high, false) {
        KeepOutcome::NeedsApproval { risk, .. } => {
            assert!(risk.reasons.iter().any(|r| r.contains("deploy steps")), "{:?}", risk.reasons)
        }
        _ => panic!("the decider's 'high' should mean the person is asked"),
    }

    // It can't make a change the rules call high risk look safe.
    let says_low = r#"cat >/dev/null; echo '{"level":"low","reason":"trust me"}'"#;
    assert!(matches!(keep_with_decider(home.path(), says_low, true), KeepOutcome::NeedsApproval { .. }));

    // A hanging or broken decider is ignored: the rules decide.
    assert!(matches!(keep_with_decider(home.path(), "sleep 30", false), KeepOutcome::Kept { .. }));
    assert!(matches!(keep_with_decider(home.path(), "echo not json", false), KeepOutcome::Kept { .. }));

    // Comparing Tries: the decider sees each one's changes and may suggest another winner.
    let picks_b = r#"grep -q '"question":"pick"' && echo '{"pick":"b","reason":"b handles the empty form too"}'"#;
    let c = compare_with_decider(home.path(), picks_b);
    assert_eq!(c.suggested.unwrap().0, "a", "the rules' own pick is still shown");
    assert_eq!(c.decider, Some(("b".into(), "b handles the empty form too".into())));
    // It sees the changes themselves.
    let shows_diff = r#"grep -q 'empty form' && echo '{"pick":"b","reason":"saw it"}'"#;
    assert_eq!(compare_with_decider(home.path(), shows_diff).decider.unwrap().1, "saw it");
    // A pick that isn't one of the Tries is ignored.
    let made_up = r#"cat >/dev/null; echo '{"pick":"zzz","reason":"?"}'"#;
    assert!(compare_with_decider(home.path(), made_up).decider.is_none());
}

fn compare_with_decider(home: &std::path::Path, decider: &str) -> asqu::compare::Comparison {
    fs::write(home.join("decider.toml"), format!("command = {decider:?}\ntimeout_secs = 2\n")).unwrap();
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("form.js"), "submit()\n").unwrap();
    let mut store = Store::open_at(dir.path()).unwrap();
    store.save(&Why::default()).unwrap();
    let a = store.start_try("a", "Fix the form", "me").unwrap();
    let b = store.start_try("b", "Fix the form", "me").unwrap();
    fs::write(a.join("form.js"), "check(); submit()\n").unwrap();
    fs::write(b.join("form.js"), "// handles the empty form\nif (empty) warn();\ncheck(); submit()\n").unwrap();
    store.compare_tries(&[], false, None, "me").unwrap()
}
