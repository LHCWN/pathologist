use crate::common::*;
use rusqlite::Connection;

const SOURCE: &str = include_str!("../../../../tests/fixtures/cpp_review_257_invocations/main.cpp");

fn source_line(spelling: &str) -> i64 {
    SOURCE
        .lines()
        .position(|line| line.contains(spelling))
        .unwrap() as i64
        + 1
}

#[test]
fn cli_retains_reference_returning_member_pointer_targets() {
    for mode in [vec![], vec!["--full-export"]] {
        let db = cli_analyze(&fixture("cpp_review_257_invocations"), &mode);
        let conn = Connection::open(db.path()).unwrap();
        for (caller, target) in [
            ("reference_member", "C::ref"),
            ("rvalue_reference_member", "C::rref"),
        ] {
            let count: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM call_edges e JOIN functions c ON c.id=e.caller_fn_id
                 JOIN functions t ON t.id=e.callee_fn_id
                 WHERE c.name=?1 AND t.name=?2 AND e.resolution='indirect'",
                    (caller, target),
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(count, 1, "{mode:?}: {caller} -> {target}");
        }
    }
}

#[test]
fn member_pointer_signature_selects_static_callback_argument() {
    let db = cli_analyze(&fixture("cpp_review_257_invocations"), &[]);
    let conn = Connection::open(db.path()).unwrap();
    let mut stmt = conn
        .prepare(
            "SELECT t.line_start FROM call_edges e JOIN functions c ON c.id=e.caller_fn_id
         JOIN functions t ON t.id=e.callee_fn_id
         WHERE c.name='C::go' AND t.name='Callbacks::target' AND e.resolution='indirect'
         ORDER BY t.line_start",
        )
        .unwrap();
    let targets: Vec<i64> = stmt
        .query_map([], |row| row.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(targets, [source_line("static void target(int)")]);
}

#[test]
fn returned_functor_invocation_uses_expression_cv_and_value_category() {
    let db = cli_analyze(&fixture("cpp_review_257_invocations"), &[]);
    let conn = Connection::open(db.path()).unwrap();
    let mut stmt = conn
        .prepare(
            "SELECT t.line_start FROM call_edges e JOIN functions c ON c.id=e.caller_fn_id
         JOIN functions t ON t.id=e.callee_fn_id
         WHERE c.name=?1 AND t.name='Functor::operator()' ORDER BY t.line_start",
        )
        .unwrap();
    for (caller, overload) in [
        ("value_functor", "operator()() &&"),
        ("reference_functor", "operator()() & {}"),
        ("const_value_functor", "operator()() const &&"),
        ("const_reference_functor", "operator()() const & {}"),
        ("rvalue_reference_functor", "operator()() &&"),
    ] {
        let targets: Vec<i64> = stmt
            .query_map([caller], |row| row.get(0))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        assert_eq!(targets, [source_line(overload)], "{caller}");
    }
}

#[test]
fn cli_sequence_finds_original_arrow_star_callback_label() {
    let db = cli_analyze(&fixture("cpp_review_257_invocations"), &["--sequencing"]);
    let conn = Connection::open(db.path()).unwrap();
    let label: String = conn
        .query_row(
            "SELECT s.callee_text FROM call_sites s JOIN functions c ON c.id=s.caller_fn_id
         JOIN call_edges e ON e.call_site_id=s.id JOIN functions t ON t.id=e.callee_fn_id
         WHERE c.name='callback_label' AND t.name='callback_target'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(label, "p->*1");
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_trace"))
        .arg("inspect")
        .arg(db.path())
        .arg("sequence")
        .args([
            "--file",
            "main.cpp",
            "--line",
            &source_line("void callback_label").to_string(),
            "--callee",
            "p->*1",
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = String::from_utf8(out.stdout).unwrap();
    assert!(text.contains("p->*1()"), "{text}");
    assert!(!text.contains("p+"), "{text}");
}
