//! What the lambda-free public relations of an export hold
//! (docs/SQLITE_SCHEMA.md, "Lambda-free call graph views").
use crate::common::{cli_analyze, scratch};
use trace_db::open_db;

const GENERATED: &str = "*::$lambda[0-9]*:[0-9]*";

/// Every public relation and text column holding a generated lambda name. A
/// string literal location spelling one is the program's own text.
fn generated_names(conn: &rusqlite::Connection) -> Vec<String> {
    let columns: Vec<(String, String)> = conn
        .prepare(
            "SELECT m.name, c.name FROM sqlite_schema m, pragma_table_info(m.name) c \
             WHERE m.type IN ('table','view') \
               AND m.name NOT GLOB 'raw_*' AND m.name NOT GLOB 'sqlite_*' ORDER BY 1, 2",
        )
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    columns
        .into_iter()
        .filter(|(relation, column)| {
            let own_text = match relation.as_str() {
                "locations" => "AND kind <> 'StringLit'",
                "strings" => "AND text NOT GLOB 'string:*'",
                "flow_nodes" | "flow_nodes_text" => "AND label NOT GLOB 'string:*'",
                _ => "",
            };
            let sql = format!(
                "SELECT EXISTS(SELECT 1 FROM \"{relation}\" WHERE \"{column}\" GLOB ?1 {own_text})"
            );
            conn.query_row(&sql, [GENERATED], |r| r.get(0)).unwrap()
        })
        .map(|(relation, column)| format!("{relation}.{column}"))
        .collect()
}

#[test]
fn no_public_relation_of_a_full_export_names_a_generated_lambda() {
    let src = "int Used(int v) { return v; }\n\
               void Spawn(int (*f)()) { f(); }\n\
               auto file_scope = [] { return Used(3); };\n\
               int Invoked() {\n    int local = 2;\n    \
               auto run = [&local](int p) {\n        static int count;\n        \
               struct Local { int f; } s;\n        s.f = p + count;\n        \
               int *h = new int(s.f);\n        return Used(local + *h);\n    };\n    \
               return run(1);\n}\n\
               void Passed() { Spawn([] { return Used(4); }); }\n\
               void NeverInvoked() { auto l = [] { return Used(5); }; (void)l; }\n\
               const char *g_tag = \"Root::$lambda3:4\";\n\
               void (*g_callback)();\n\
               void Unresolved() { auto l = [] { g_callback(); }; l(); }\n";
    let dir = scratch(&[("main.cpp", src)]);
    let db = cli_analyze(
        dir.path(),
        &["--full-export", "--debug-points-to", "--sequencing"],
    );
    let conn = open_db(&db).unwrap();
    let raw: i64 = conn
        .query_row(
            "SELECT count(*) FROM raw_locations WHERE kind = 'Function' AND desc GLOB ?1 \
             AND rtrim(desc, '0123456789:') GLOB '*::$lambda'",
            [GENERATED],
            |r| r.get(0),
        )
        .unwrap();
    assert!(raw > 0, "the fixture must produce generated location names");
    let relabeled: i64 = conn
        .query_row(
            "SELECT count(*) FROM locations WHERE desc = 'anonymous callable'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(relabeled, raw, "each lambda location reads as a callable");
    assert_eq!(generated_names(&conn), Vec::<String>::new());
    let literal: i64 = conn
        .query_row(
            "SELECT count(*) FROM flow_nodes WHERE label = 'string:Root::$lambda3:4'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        literal, 1,
        "a string literal keeps its text in the flow graph"
    );
    let dangling = conn
        .prepare("PRAGMA foreign_key_check")
        .unwrap()
        .exists([])
        .unwrap();
    assert!(!dangling, "publication must leave no dangling reference");
}

/// `points_to` references `locations`, so debug output exports both, whether or
/// not the rest of a full export was asked for.
#[test]
fn debug_points_to_alone_exports_the_locations_it_references() {
    let src = "void f() { auto l = [] { static int g; return &g; }; l(); }\n";
    let dir = scratch(&[("main.cpp", src)]);
    let db = cli_analyze(dir.path(), &["--debug-points-to"]);
    let conn = open_db(&db).unwrap();
    let located: i64 = conn
        .query_row("SELECT count(*) FROM locations", [], |r| r.get(0))
        .unwrap();
    assert!(located > 0);
    let points_to: i64 = conn
        .query_row("SELECT count(*) FROM points_to", [], |r| r.get(0))
        .unwrap();
    assert!(points_to > 0, "the fixture must produce points-to rows");
}

/// A site in a lambda nothing invokes is published under the function it is
/// written in. Each reader either keeps that lexical meaning or reads edges
/// (docs/SQLITE_SCHEMA.md, "Lambda-free call graph views").
#[test]
fn a_site_in_an_uninvoked_lambda_is_lexical_for_every_reader() {
    let src = "int Unused(int v) { return v; }\nvoid NeverInvoked() {\n    int x = 1;\n    \
               auto l = [x] { return Unused(x); };\n    (void)l;\n}\n";
    let dir = scratch(&[("main.cpp", src)]);
    let db = cli_analyze(dir.path(), &["--sequencing"]);
    let conn = open_db(&db).unwrap();
    let (site, owner, function): (i64, String, u32) = conn
        .query_row(
            "SELECT cs.id, f.name, f.id FROM call_sites cs \
             JOIN functions f ON f.id = cs.caller_fn_id WHERE cs.callee_text = 'Unused'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!(owner, "NeverInvoked");

    // The complete graph keeps the call in the lambda; no visible function makes it.
    let rows = |sql: &str| -> i64 { conn.query_row(sql, [site], |r| r.get(0)).unwrap() };
    let at_site = |table: &str| {
        rows(&format!(
            "SELECT count(*) FROM {table} WHERE call_site_id = ?1"
        ))
    };
    assert_eq!(at_site("call_edges"), 0);
    let raw =
        "SELECT count(*) FROM raw_call_edges e JOIN raw_functions f ON f.id = e.caller_fn_id \
               WHERE e.call_site_id = ?1 AND f.name GLOB 'NeverInvoked::$lambda*'";
    assert_eq!(rows(raw), 1);

    // Value flow: the step's scope says where the call is written.
    assert_eq!(
        crate::flow_origin_sites::inspect_scope(&db, ("main.cpp", 3, 9), "down", (4, 27))
            .as_deref(),
        Some("NeverInvoked")
    );

    // Call graph and editor: edges only, so no call is claimed.
    let callgraph = crate::inspect_tests::inspect_output(
        &db,
        &[
            "callgraph",
            "--file",
            "main.cpp",
            "--line",
            "2",
            "--direction",
            "down",
        ],
    );
    assert!(!callgraph.contains("Unused"), "{callgraph}");
    let calls = crate::inspect_tests::inspect_output(&db, &["calls", "--from", "NeverInvoked"]);
    assert!(!calls.contains("Unused"), "{calls}");
    let chain = crate::inspect_tests::inspect_output(
        &db,
        &["callchain", "--from", "NeverInvoked", "--to", "Unused"],
    );
    assert!(chain.contains("no call chain"), "{chain}");
    let hierarchy =
        trace_db::editor_calls(&conn, trace_ir::FnId(function), trace_db::Direction::Down);
    assert!(hierarchy.unwrap().is_empty());

    // Sequencing: a lambda's graph cannot join its parent's, so it stays raw.
    assert_eq!(at_site("cfg_event_sites"), 0);
    assert_eq!(at_site("raw_cfg_event_sites"), 1);
}
