//! Publication of the lambda-free public schema. Analysis and merge always
//! finish on the complete graph before this final presentation step.
use anyhow::{bail, Context, Result};
use regex::Regex;
use rusqlite::{params, Connection};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;

// Only changed storage is copied. Unchanged raw relations are aliases, so the
// large flow-edge and provenance tables do not need a second physical copy.
const CHANGED: &[&str] = &[
    "functions",
    "strings",
    "variables_data",
    "call_sites_data",
    "call_edges_data",
    "flow_nodes_data",
    "flow_parameters_data",
    "flow_return_calls",
    "execution_contexts",
    "link_method_facts",
    "link_method_origins",
    "link_method_guesses",
    "link_view_methods",
    "link_dispatch_queries",
    "link_member_calls",
    "link_ipc_senders",
    "arg_flow_edges",
    "flow_calls",
    "flow_call_origins_data",
    "flow_call_expressions_data",
    "cfg_functions",
    "cfg_blocks",
    "cfg_edges",
    "cfg_events",
    "cfg_event_sites",
    "cfg_event_order",
    "cfg_gaps",
    "merge_function_identities",
    "merge_call_site_identities",
];

// Export-only views over the complete graph. They are temporary and never
// become a public schema or a compatibility obligation.
const PROJECTION_VIEWS: &str = r#"
CREATE TEMP VIEW _projection_functions AS
SELECT id, name, file_id, line_start, line_end, linkage, signature,
       is_defined, is_dep, is_weak, target_id
FROM raw_functions WHERE name NOT GLOB '*::$lambda[0-9]*:[0-9]*';

-- Compute reachability once per distinct entering lambda, independently of
-- how many visible callers enter it. UNION bounds cycles by lambda pairs.
CREATE TEMP VIEW _projection_lambda_reachability AS
WITH RECURSIVE paths(start_fn_id, lambda_fn_id) AS (
    SELECT ce.callee_fn_id, ce.callee_fn_id
    FROM raw_call_edges ce
    JOIN _projection_functions caller ON caller.id = ce.caller_fn_id
    LEFT JOIN _projection_functions callee ON callee.id = ce.callee_fn_id
    WHERE callee.id IS NULL
    UNION
    SELECT p.start_fn_id, ce.callee_fn_id
    FROM paths p JOIN raw_call_edges ce ON ce.caller_fn_id = p.lambda_fn_id
    LEFT JOIN _projection_functions callee ON callee.id = ce.callee_fn_id
    WHERE callee.id IS NULL
)
SELECT start_fn_id, lambda_fn_id FROM paths;

-- Attach original entering-edge provenance only after the shared closure.
CREATE TEMP VIEW _projection_lambda_callers AS
SELECT ce.id AS entry_edge_id, ce.call_site_id, ce.caller_fn_id,
       p.lambda_fn_id, ce.resolution
FROM raw_call_edges ce
JOIN _projection_functions caller ON caller.id = ce.caller_fn_id
JOIN _projection_lambda_reachability p ON p.start_fn_id = ce.callee_fn_id;

CREATE TEMP VIEW _projection_call_edges AS
SELECT ce.id AS entry_edge_id, ce.call_site_id, ce.caller_fn_id, ce.callee_fn_id, ce.resolution
FROM raw_call_edges ce
JOIN _projection_functions caller ON caller.id = ce.caller_fn_id
JOIN _projection_functions callee ON callee.id = ce.callee_fn_id
UNION
SELECT p.entry_edge_id, p.call_site_id, p.caller_fn_id, ce.callee_fn_id, p.resolution
FROM _projection_lambda_callers p JOIN raw_call_edges ce ON ce.caller_fn_id = p.lambda_fn_id
JOIN _projection_functions callee ON callee.id = ce.callee_fn_id;

CREATE TEMP VIEW _projection_unresolved AS
SELECT cs.id AS call_site_id, cs.caller_fn_id, cs.callee_text, cs.file_id, cs.line, cs.col
FROM raw_call_sites cs JOIN _projection_functions caller ON caller.id = cs.caller_fn_id
WHERE cs.is_direct = 0
  AND NOT EXISTS (SELECT 1 FROM raw_call_edges ce WHERE ce.call_site_id = cs.id)
UNION
SELECT cs.id, p.caller_fn_id, cs.callee_text, cs.file_id, cs.line, cs.col
FROM raw_call_sites cs JOIN _projection_lambda_callers p ON p.lambda_fn_id = cs.caller_fn_id
WHERE cs.is_direct = 0
  AND NOT EXISTS (SELECT 1 FROM raw_call_edges ce WHERE ce.call_site_id = cs.id);
"#;

fn drop_projection_views(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "DROP VIEW _projection_unresolved;
        DROP VIEW _projection_call_edges;
        DROP VIEW _projection_lambda_callers;
        DROP VIEW _projection_lambda_reachability;
        DROP VIEW _projection_functions;",
    )?;
    Ok(())
}

fn quote(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

// Rewrite identifiers in our own schema DDL, leaving literals and comments
// intact. No user SQL is accepted here.
fn raw_sql(sql: &str, names: &BTreeSet<String>) -> String {
    static TOKENS: std::sync::LazyLock<Regex> = std::sync::LazyLock::new(|| {
        Regex::new(r"'(?:''|[^'])*'|--[^\n]*|[A-Za-z_][A-Za-z_0-9]*").unwrap()
    });
    let tokens = &*TOKENS;
    tokens
        .replace_all(sql, |caps: &regex::Captures<'_>| {
            let token = &caps[0];
            if names.contains(token) {
                format!("raw_{token}")
            } else {
                token.to_owned()
            }
        })
        .into_owned()
}

fn preserve_raw(conn: &Connection, has_lambdas: bool) -> Result<()> {
    let objects = conn
        .prepare(
            "SELECT type,name,tbl_name,sql FROM sqlite_schema
         WHERE sql IS NOT NULL AND name NOT GLOB 'sqlite_*'
           AND name <> 'database_identity'
         ORDER BY CASE type WHEN 'table' THEN 0 WHEN 'view' THEN 1 ELSE 2 END,name",
        )?
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let relations: BTreeSet<_> = objects
        .iter()
        .filter(|o| o.0 != "index")
        .map(|o| o.1.clone())
        .collect();
    let changed: BTreeSet<_> = CHANGED
        .iter()
        .filter(|_| has_lambdas)
        .map(|s| (*s).to_owned())
        .collect();
    for (kind, name, table, sql) in &objects {
        match kind.as_str() {
            "table" if changed.contains(name) => {
                conn.execute_batch(&raw_sql(sql, &changed))?;
                conn.execute_batch(&format!(
                    "INSERT INTO {} SELECT * FROM {}",
                    quote(&format!("raw_{name}")),
                    quote(name)
                ))?;
            }
            "table" => conn.execute_batch(&format!(
                "CREATE VIEW {} AS SELECT * FROM {}",
                quote(&format!("raw_{name}")),
                quote(name)
            ))?,
            "view" => conn.execute_batch(&raw_sql(sql, &relations))?,
            "index" if changed.contains(table) => {
                let mut names = changed.clone();
                names.insert(name.clone());
                conn.execute_batch(&raw_sql(sql, &names))?;
            }
            _ => {}
        }
    }
    Ok(())
}

/// Select the complete stored graph on a read connection, including linking
/// metadata. Temporary aliases leave the immutable snapshot unchanged. Legacy
/// snapshots already expose their complete graph under the public names.
pub fn use_raw_relations(conn: &Connection) -> Result<()> {
    if crate::table_exists(conn, "lambda_owners")? {
        for relation in ["raw_functions", "raw_call_sites", "raw_call_edges"] {
            conn.prepare(&format!("SELECT * FROM {relation} LIMIT 0"))
                .and_then(|mut s| s.exists([]))
                .with_context(|| format!("incomplete raw graph: {relation} cannot be queried; re-export or re-merge this snapshot"))?;
        }
    }
    let names = conn.prepare(
        "SELECT name FROM main.sqlite_schema WHERE name GLOB 'raw_*' AND type IN ('table','view') ORDER BY name",
    )?.query_map([], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for raw in names {
        let name = raw.strip_prefix("raw_").unwrap();
        conn.execute_batch(&format!(
            "CREATE TEMP VIEW IF NOT EXISTS {} AS SELECT * FROM main.{}",
            quote(name),
            quote(&raw)
        ))?;
    }
    conn.flush_prepared_statement_cache();
    Ok(())
}

pub(crate) fn public_projection_active(conn: &Connection) -> Result<bool> {
    Ok(conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM main.sqlite_schema WHERE name='lambda_owners')
        AND NOT EXISTS(SELECT 1 FROM temp.sqlite_schema WHERE name='functions')",
        [],
        |r| r.get(0),
    )?)
}

/// Source slicing needs complete return bindings and execution boundaries.
/// Scope the temporary aliases so subsequent consumer queries still see the
/// public projection, on both success and failure.
pub(crate) fn with_raw_relations<T>(
    conn: &Connection,
    read: impl FnOnce() -> Result<T>,
) -> Result<T> {
    if !public_projection_active(conn)?
        || !conn.query_row("SELECT EXISTS(SELECT 1 FROM lambda_owners)", [], |r| {
            r.get::<_, bool>(0)
        })?
    {
        return read();
    }
    let query_only: bool = conn
        .query_row("PRAGMA query_only", [], |r| r.get(0))
        .unwrap_or(false);
    if query_only {
        conn.execute_batch("PRAGMA query_only=OFF;")?;
    }
    let res = (|| -> Result<T> {
        conn.execute_batch("SAVEPOINT trace_raw_inspection")?;
        let result = use_raw_relations(conn).and_then(|()| read());
        let cleanup =
            conn.execute_batch("ROLLBACK TO trace_raw_inspection; RELEASE trace_raw_inspection");
        conn.flush_prepared_statement_cache();
        let value = result?;
        cleanup?;
        Ok(value)
    })();
    if query_only {
        let _ = conn.execute_batch("PRAGMA query_only=ON;");
    }
    res
}

fn project_call_ids(conn: &Connection) -> Result<()> {
    let inherited = crate::table_exists(conn, "raw_merge_function_identities")?;
    let target_key = if inherited {
        "(SELECT identity FROM raw_merge_function_identities WHERE function_id=p.callee_fn_id)"
    } else {
        "NULL"
    };
    let mut rows = conn.prepare(&format!(
        "SELECT p.rowid,p.entry_edge_id,p.callee_fn_id,r.callee_fn_id,{target_key}
         FROM projected_calls p JOIN raw_call_edges_data r ON r.id=p.entry_edge_id
         ORDER BY p.entry_edge_id,p.callee_fn_id"
    ))?;
    let mut update = conn.prepare("UPDATE projected_calls SET projected_id=?2 WHERE rowid=?1")?;
    let mut rows = rows.query([])?;
    while let Some(row) = rows.next()? {
        let row_id: i64 = row.get(0)?;
        let entry: i64 = row.get(1)?;
        let callee: i64 = row.get(2)?;
        let original_callee: i64 = row.get(3)?;
        let id = if callee == original_callee {
            entry
        } else {
            let mut hash = Sha256::new();
            hash.update(b"trace-lambda-projected-edge-v1");
            hash.update(entry.to_be_bytes());
            let identity: Option<Vec<u8>> = row.get(4)?;
            if inherited && identity.is_none() {
                bail!("missing merged function identity for projected callee {callee}");
            }
            hash.update(identity.as_deref().unwrap_or(&callee.to_be_bytes()));
            let digest = hash.finalize();
            i64::from_be_bytes(digest[..8].try_into().unwrap()) & i64::MAX
        };
        update.execute(params![row_id, id])?;
    }
    // Treat even the extremely unlikely digest collision as an export error;
    // do not silently assign an order-dependent identity.
    conn.execute_batch(
        "CREATE UNIQUE INDEX temp.projected_call_ids ON projected_calls(projected_id)",
    )
    .context("hash collision detected while assigning deterministic projected call edge IDs")?;
    Ok(())
}

/// Publish the master-compatible column layouts with lambda-free meanings.
/// Must run once, inside the export transaction, after all graph/link facts
/// and indexes have been written and before recording the database identity.
/// Requires current compact storage and the producer's bulk-load setting
/// `PRAGMA foreign_keys=OFF`; publication can replace referenced rows.
pub fn publish_lambda_free(conn: &Connection) -> Result<()> {
    if crate::table_exists(conn, "raw_functions")? {
        bail!("lambda-free projection has already been published");
    }
    let has_lambdas: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM functions WHERE name GLOB '*::$lambda[0-9]*:[0-9]*')",
        [],
        |r| r.get(0),
    )?;
    preserve_raw(conn, has_lambdas)?;
    conn.execute_batch(PROJECTION_VIEWS)?;
    conn.execute_batch(
        r#"
CREATE TABLE lambda_owners (
    lambda_fn_id INTEGER PRIMARY KEY,
    owner_fn_id INTEGER REFERENCES functions(id)
);
CREATE TABLE lambda_call_provenance (
    edge_id INTEGER NOT NULL REFERENCES call_edges_data(id),
    entry_edge_id INTEGER NOT NULL,
    PRIMARY KEY (edge_id, entry_edge_id)
) WITHOUT ROWID;
-- Preserve unresolved-path semantics before lifting lexical ownership. In
-- particular, an uninvoked lambda must not add unresolved calls to its parent.
CREATE TABLE lambda_unresolved_calls (
    call_site_id INTEGER, caller_fn_id INTEGER, callee_text TEXT,
    file_id INTEGER, line INTEGER, col INTEGER
);
INSERT INTO lambda_unresolved_calls SELECT * FROM _projection_unresolved;
"#,
    )?;
    if !has_lambdas {
        // No changed storage needs a second physical copy or a rewrite. This
        // also preserves ordinary edge IDs at the full SQLite integer range.
        conn.execute_batch("INSERT INTO lambda_call_provenance SELECT id,id FROM call_edges_data")?;
        drop_projection_views(conn)?;
        return Ok(());
    }
    // Ownership is lexical, scoped to file and image, with the containing
    // definition preferred over another TU's same-named definition. A lambda
    // in a file-scope initializer has no enclosing function (NULL owner).
    conn.execute_batch(
        r#"
INSERT INTO lambda_owners
SELECT id,owner FROM (
 SELECT l.id,f.id AS owner,row_number() OVER (PARTITION BY l.id ORDER BY
        (f.line_start <= l.line_start AND f.line_end >= l.line_end) DESC,
        f.is_defined DESC,f.id) AS rank
 FROM raw_functions l LEFT JOIN raw_functions f
   ON f.name=substr(l.name,1,instr(l.name,'::$lambda')-1)
  AND f.file_id=l.file_id AND f.target_id IS l.target_id
 WHERE l.name GLOB '*::$lambda[0-9]*:[0-9]*'
) WHERE rank=1;

-- Materialize before changing either endpoint relation. An entering edge can
-- reach multiple leaves, and multiple entering edges can reach the same
-- visible callee from the same site. Deduplicate public edges on
-- (caller, callee, call_site, resolution) while preserving every entering edge
-- in provenance.
CREATE TEMP TABLE projected_call_provenance AS
SELECT * FROM _projection_call_edges;

CREATE TEMP TABLE projected_calls AS
SELECT
    CASE WHEN min(CASE WHEN r.callee_fn_id = p.callee_fn_id THEN p.entry_edge_id END) IS NOT NULL
         THEN min(CASE WHEN r.callee_fn_id = p.callee_fn_id THEN p.entry_edge_id END)
         ELSE min(p.entry_edge_id)
    END AS entry_edge_id,
    p.call_site_id, p.caller_fn_id, p.callee_fn_id, p.resolution,
    CAST(NULL AS INTEGER) AS projected_id
FROM projected_call_provenance p
JOIN raw_call_edges_data r ON r.id = p.entry_edge_id
GROUP BY p.caller_fn_id, p.callee_fn_id, p.call_site_id, p.resolution;
"#,
    )?;
    project_call_ids(conn)?;
    conn.execute_batch(r#"
DELETE FROM call_edges_data;
INSERT INTO call_edges_data
SELECT p.projected_id,p.call_site_id,p.caller_fn_id,p.callee_fn_id,r.resolution_id
FROM projected_calls p JOIN raw_call_edges_data r ON r.id=p.entry_edge_id;

INSERT INTO lambda_call_provenance
SELECT DISTINCT p.projected_id, prov.entry_edge_id
FROM projected_calls p
JOIN projected_call_provenance prov
  ON prov.caller_fn_id = p.caller_fn_id
 AND prov.callee_fn_id = p.callee_fn_id
 AND prov.call_site_id IS p.call_site_id
 AND prov.resolution = p.resolution;
DROP TABLE projected_calls;
DROP TABLE projected_call_provenance;

UPDATE variables_data SET kind='local' WHERE kind='param' AND fn_id IN (SELECT lambda_fn_id FROM lambda_owners);
UPDATE variables_data SET fn_id=(SELECT owner_fn_id FROM lambda_owners WHERE lambda_fn_id=fn_id)
WHERE fn_id IN (SELECT lambda_fn_id FROM lambda_owners);
-- A function location is a callable value, not an ownership fact. Preserve
-- its node and flow without claiming that it points to the lexical parent.
-- Scope the relabeling and unlinking to actual fn: location nodes; other nodes
-- in the lambda (variables, call_target nodes) are remapped to the owner.
INSERT INTO strings(id, text)
SELECT (SELECT coalesce(max(id), 0) + 1 FROM strings), 'anonymous callable'
WHERE NOT EXISTS (SELECT 1 FROM strings WHERE text = 'anonymous callable')
  AND EXISTS (
    SELECT 1 FROM flow_nodes_data n
    WHERE n.fn_id IN (SELECT lambda_fn_id FROM lambda_owners)
      AND n.kind_id = (SELECT id FROM strings WHERE text = 'loc')
      AND n.label_id IN (SELECT id FROM strings WHERE text LIKE 'fn:%')
);
UPDATE flow_nodes_data
SET label_id = (SELECT max(id) FROM strings WHERE text = 'anonymous callable'),
    fn_id = NULL
WHERE fn_id IN (SELECT lambda_fn_id FROM lambda_owners)
  AND kind_id = (SELECT id FROM strings WHERE text = 'loc')
  AND label_id IN (SELECT id FROM strings WHERE text LIKE 'fn:%');

UPDATE flow_nodes_data
SET fn_id = (SELECT owner_fn_id FROM lambda_owners WHERE lambda_fn_id = fn_id)
WHERE fn_id IN (SELECT lambda_fn_id FROM lambda_owners);
UPDATE call_sites_data SET caller_fn_id=(SELECT owner_fn_id FROM lambda_owners WHERE lambda_fn_id=caller_fn_id)
WHERE caller_fn_id IN (SELECT lambda_fn_id FROM lambda_owners WHERE owner_fn_id IS NOT NULL);
-- A site that names a generated lambda invokes it where it is written. Stripping
-- that name below would leave the owner's, reading as a call of the owner.
INSERT INTO strings(id, text)
SELECT (SELECT coalesce(max(id), 0) + 1 FROM strings), '<lambda>'
WHERE NOT EXISTS (SELECT 1 FROM strings WHERE text = '<lambda>')
  AND EXISTS (
    SELECT 1 FROM call_sites_data cs JOIN strings t ON t.id = cs.callee_text_id
    WHERE t.text GLOB '*::$lambda[0-9]*:[0-9]*'
);
UPDATE call_sites_data SET callee_text_id = (SELECT max(id) FROM strings WHERE text = '<lambda>')
WHERE callee_text_id IN (SELECT id FROM strings WHERE text GLOB '*::$lambda[0-9]*:[0-9]*');
-- File-scope lambdas have no lexical function. Use a recorded visible caller
-- when available; otherwise their body sites exist only in the raw interface.
UPDATE call_sites_data SET caller_fn_id=(SELECT min(caller_fn_id) FROM _projection_lambda_callers p
                                        WHERE p.lambda_fn_id=call_sites_data.caller_fn_id)
WHERE caller_fn_id IN (SELECT lambda_fn_id FROM lambda_owners WHERE owner_fn_id IS NULL)
  AND EXISTS (SELECT 1 FROM _projection_lambda_callers p WHERE p.lambda_fn_id=call_sites_data.caller_fn_id);
DELETE FROM call_sites_data WHERE caller_fn_id IN (SELECT lambda_fn_id FROM lambda_owners);
UPDATE flow_nodes_data SET call_site_id=NULL WHERE call_site_id IS NOT NULL
  AND call_site_id NOT IN (SELECT id FROM call_sites_data);
DELETE FROM flow_calls WHERE call_site_id NOT IN (SELECT id FROM call_sites_data);
DELETE FROM flow_return_calls WHERE call_site_id NOT IN (SELECT id FROM call_sites_data);
DELETE FROM flow_call_origins_data WHERE call_site_id NOT IN (SELECT id FROM call_sites_data);
DELETE FROM flow_call_expressions_data WHERE call_site_id NOT IN (SELECT id FROM call_sites_data);
DELETE FROM arg_flow_edges WHERE call_site_id NOT IN (SELECT id FROM call_sites_data);
UPDATE arg_flow_edges SET actual_fn_id=NULL WHERE actual_fn_id IN (SELECT lambda_fn_id FROM lambda_owners);
-- These are signature bindings, not ownership facts: lifting lambda parameters
-- into the enclosing function's signature would overwrite its real parameters.
DELETE FROM flow_parameters_data WHERE fn_id IN (SELECT lambda_fn_id FROM lambda_owners);
DELETE FROM flow_return_calls WHERE callee_fn_id IN (SELECT lambda_fn_id FROM lambda_owners);

-- Entry points describe execution, so use reachable visible callees, never
-- the lexical parent. Cycles terminate through UNION on function pairs.
CREATE TEMP TABLE projected_contexts AS
WITH RECURSIVE reach(start,fn_id) AS (
 SELECT entry_fn_id,entry_fn_id FROM raw_execution_contexts
 WHERE entry_fn_id IN (SELECT lambda_fn_id FROM lambda_owners)
 UNION
 SELECT r.start,e.callee_fn_id FROM reach r JOIN raw_call_edges e ON e.caller_fn_id=r.fn_id
 WHERE r.fn_id IN (SELECT lambda_fn_id FROM lambda_owners)
)
SELECT c.id AS original_id,r.fn_id AS entry_fn_id
FROM raw_execution_contexts c JOIN reach r ON r.start=c.entry_fn_id
WHERE r.fn_id NOT IN (SELECT lambda_fn_id FROM lambda_owners);
DELETE FROM execution_contexts WHERE entry_fn_id IN (SELECT lambda_fn_id FROM lambda_owners);
INSERT INTO execution_contexts
SELECT CASE WHEN row_number() OVER (PARTITION BY p.original_id ORDER BY p.entry_fn_id)=1 THEN p.original_id
       ELSE (SELECT coalesce(max(id),0) FROM raw_execution_contexts)
            + row_number() OVER (ORDER BY p.original_id,p.entry_fn_id) END,
       c.kind,p.entry_fn_id,c.call_site_id,c.api_fn_id,c.param_index,c.receiver_var_id,
       c.model,c.multi_instance,c.self_concurrent
FROM projected_contexts p JOIN raw_execution_contexts c ON c.id=p.original_id;
DROP TABLE projected_contexts;
UPDATE execution_contexts SET api_fn_id=NULL WHERE api_fn_id IN (SELECT lambda_fn_id FROM lambda_owners);
UPDATE execution_contexts SET call_site_id=NULL WHERE call_site_id IS NOT NULL
  AND call_site_id NOT IN (SELECT id FROM call_sites_data);

DELETE FROM link_method_origins WHERE fn_id IN (SELECT lambda_fn_id FROM lambda_owners);
DELETE FROM link_method_guesses WHERE fn_id IN (SELECT lambda_fn_id FROM lambda_owners);
DELETE FROM link_view_methods WHERE fn_id IN (SELECT lambda_fn_id FROM lambda_owners);
DELETE FROM link_method_facts WHERE fn_id IN (SELECT lambda_fn_id FROM lambda_owners);
DELETE FROM link_ipc_senders WHERE fn_id IN (SELECT lambda_fn_id FROM lambda_owners);
UPDATE link_dispatch_queries SET callee_fn_id=NULL WHERE callee_fn_id IN (SELECT lambda_fn_id FROM lambda_owners);
DELETE FROM link_member_calls WHERE call_site_id NOT IN (SELECT id FROM call_sites_data);
UPDATE link_member_calls SET source_site_id=NULL WHERE source_site_id IS NOT NULL
  AND source_site_id NOT IN (SELECT id FROM call_sites_data);

-- A lambda CFG cannot be spliced into its lexical parent's control flow:
-- execution might happen asynchronously or never. Keep it in raw_cfg_*.
DELETE FROM cfg_event_order WHERE event_id IN (
 SELECT e.id FROM cfg_events e JOIN cfg_functions f ON f.id=e.cfg_id
 WHERE f.fn_id IN (SELECT lambda_fn_id FROM lambda_owners));
DELETE FROM cfg_event_sites WHERE event_id IN (
 SELECT e.id FROM cfg_events e JOIN cfg_functions f ON f.id=e.cfg_id
 WHERE f.fn_id IN (SELECT lambda_fn_id FROM lambda_owners))
 OR call_site_id NOT IN (SELECT id FROM call_sites_data);
DELETE FROM cfg_gaps WHERE cfg_id IN (SELECT id FROM cfg_functions WHERE fn_id IN (SELECT lambda_fn_id FROM lambda_owners));
DELETE FROM cfg_events WHERE cfg_id IN (SELECT id FROM cfg_functions WHERE fn_id IN (SELECT lambda_fn_id FROM lambda_owners));
DELETE FROM cfg_edges WHERE src_block IN (
 SELECT b.id FROM cfg_blocks b JOIN cfg_functions f ON f.id=b.cfg_id
 WHERE f.fn_id IN (SELECT lambda_fn_id FROM lambda_owners));
DELETE FROM cfg_blocks WHERE cfg_id IN (SELECT id FROM cfg_functions WHERE fn_id IN (SELECT lambda_fn_id FROM lambda_owners));
DELETE FROM cfg_functions WHERE fn_id IN (SELECT lambda_fn_id FROM lambda_owners);

DELETE FROM functions WHERE id IN (SELECT lambda_fn_id FROM lambda_owners);
"#)?;
    for (table, key, parent) in [
        ("merge_function_identities", "function_id", "functions"),
        (
            "merge_call_site_identities",
            "call_site_id",
            "call_sites_data",
        ),
    ] {
        if crate::table_exists(conn, table)? {
            conn.execute_batch(&format!(
                "DELETE FROM {table} WHERE {key} NOT IN (SELECT id FROM {parent})"
            ))?;
        }
    }
    // Text IDs remain stable. Strip only reserved generated name segments;
    // source-level lambda expressions and ordinary strings are retained.
    let segment = Regex::new(r"::\$lambda[0-9]+:[0-9]+")?;
    let strings = conn
        .prepare("SELECT id,text FROM strings WHERE text GLOB '*::$lambda[0-9]*:[0-9]*'")?
        .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut update = conn.prepare("UPDATE strings SET text=?2 WHERE id=?1")?;
    for (id, text) in strings {
        update.execute(params![id, segment.replace_all(&text, "").as_ref()])?;
    }
    drop_projection_views(conn)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn complete_graph() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys=OFF").unwrap();
        conn.execute_batch(crate::SCHEMA_V8).unwrap();
        conn.execute_batch(
            "INSERT INTO files VALUES(0,'/src/main.cpp','',0);
            INSERT INTO types VALUES(0,'int','int',4,'{}');
            INSERT INTO functions VALUES
            (0,'Root',0,1,20,'external','void(int)',1,0,0,NULL),
            (1,'Root::$lambda3:1',0,3,9,'internal','void(int)',1,0,0,NULL),
            (2,'Root::$lambda3:1::$lambda4:1',0,4,8,'internal','void()',1,0,0,NULL),
            (3,'Leaf',0,21,22,'external','void()',1,0,0,NULL),
            (4,'Other',0,23,24,'external','void()',1,0,0,NULL),
            (5,'Root::$lambda10:1',0,10,12,'internal','void()',1,0,0,NULL),
            (6,'<tu>::$lambda1:1',0,1,1,'internal','void()',1,0,0,NULL),
            (7,'AltEntry',0,25,26,'external','void()',1,0,0,NULL);",
        )
        .unwrap();
        let mut texts = crate::TextPool::new(&conn).unwrap();
        let direct = texts.intern("direct").unwrap();
        let indirect = texts.intern("indirect").unwrap();
        let name = texts.intern("p").unwrap();
        let unresolved = texts.intern("unknown()").unwrap();
        let label = texts.intern("fn:Root::$lambda3:1::$lambda4:1").unwrap();
        for (id, caller, callee, is_direct) in [
            (0, 0, Some(1), false),
            (1, 1, Some(2), true),
            (2, 2, Some(3), true),
            (3, 2, Some(4), true),
            (4, 2, Some(1), true),
            (5, 5, Some(3), true),
            (6, 1, None, false),
            (7, 5, None, false),
            (8, 6, Some(3), true),
            (9, 0, Some(6), false),
            (10, 7, Some(1), false),
        ] {
            conn.execute("INSERT INTO call_sites_data(id,caller_fn_id,file_id,line,col,callee_text_id,is_direct)
                VALUES(?1,?2,0,?1+1,1,?3,?4)",params![id,caller,unresolved,is_direct]).unwrap();
            if let Some(callee) = callee {
                conn.execute(
                    "INSERT INTO call_edges_data VALUES(?1,?1,?2,?3,?4)",
                    params![
                        id,
                        caller,
                        callee,
                        if is_direct { direct } else { indirect }
                    ],
                )
                .unwrap();
            }
        }
        for (id, owner, kind) in [
            (0, 0, "param"),
            (1, 1, "param"),
            (2, 2, "local"),
            (3, 6, "local"),
        ] {
            conn.execute(
                "INSERT INTO variables_data(id,name_id,kind,fn_id,type_id,file_id,line)
                VALUES(?1,?2,?3,?4,0,0,4)",
                params![id, name, kind, owner],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO flow_nodes_data(id,kind_id,var_id,fn_id) VALUES(?1,?2,?1,?3)",
                params![id, texts.intern("var").unwrap(), owner],
            )
            .unwrap();
        }
        conn.execute(
            "INSERT INTO flow_nodes_data(id,kind_id,label_id,fn_id) VALUES(4,?1,?2,1)",
            params![texts.intern("loc").unwrap(), label],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO flow_edges_data VALUES(0,0,2,?1)",
            [texts.intern("copy").unwrap()],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO flow_parameters_data VALUES(0,0,0,?1,?1),(1,0,1,?1,?1)",
            [name],
        )
        .unwrap();
        conn.execute_batch(
            "INSERT INTO flow_field_access VALUES(0,2,'field');
            INSERT INTO arg_flow_edges VALUES(0,0,0,NULL,1,1);
            INSERT INTO execution_contexts VALUES(0,'task',1,0,NULL,0,NULL,'test','single',0);
            INSERT INTO flow_return_calls VALUES(0,1,0,1);
            INSERT INTO link_method_facts VALUES(1,0,NULL,0);
            INSERT INTO link_method_origins VALUES(1,0,1);
            INSERT INTO link_ipc_senders VALUES(1);",
        )
        .unwrap();
        drop(texts);
        conn
    }

    fn count(conn: &Connection, sql: &str) -> i64 {
        conn.query_row(sql, [], |r| r.get(0)).unwrap()
    }

    #[test]
    fn scoped_complete_reads_restore_public_relations_on_success_and_error() {
        let conn = complete_graph();
        publish_lambda_free(&conn).unwrap();
        assert_eq!(
            with_raw_relations(&conn, || Ok(count(&conn, "SELECT count(*) FROM functions")))
                .unwrap(),
            8
        );
        assert_eq!(count(&conn, "SELECT count(*) FROM functions"), 4);
        let result: Result<()> = with_raw_relations(&conn, || bail!("failed inspection"));
        assert!(result.is_err());
        assert!(public_projection_active(&conn).unwrap());
        assert_eq!(count(&conn, "SELECT count(*) FROM functions"), 4);
    }

    #[test]
    fn a_damaged_raw_graph_is_not_silently_replaced_with_the_public_graph() {
        let conn = complete_graph();
        publish_lambda_free(&conn).unwrap();
        conn.execute_batch("DROP TABLE raw_functions").unwrap();
        assert!(use_raw_relations(&conn)
            .unwrap_err()
            .to_string()
            .contains("incomplete raw graph"));
    }

    #[test]
    fn complete_relations_can_be_selected_on_a_read_only_connection() {
        let conn = complete_graph();
        publish_lambda_free(&conn).unwrap();
        let scratch = tempfile::tempdir().unwrap();
        let path = scratch.path().join("snapshot.db");
        conn.execute("VACUUM INTO ?1", [path.to_str().unwrap()])
            .unwrap();
        let read =
            Connection::open_with_flags(&path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
        assert!(public_projection_active(&read).unwrap());
        use_raw_relations(&read).unwrap();
        use_raw_relations(&read).unwrap();
        assert!(!public_projection_active(&read).unwrap());
        assert_eq!(count(&read, "SELECT count(*) FROM functions"), 8);
        assert!(read.execute("DELETE FROM main.functions", []).is_err());
        assert_eq!(count(&read, "SELECT count(*) FROM main.functions"), 4);
    }

    #[test]
    fn ordinary_edges_keep_their_ids_including_the_integer_boundary() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys=OFF").unwrap();
        conn.execute_batch(crate::SCHEMA_V8).unwrap();
        conn.execute_batch(
            "INSERT INTO files VALUES(0,'main.c','',0);
            INSERT INTO functions VALUES
            (0,'Root',0,1,2,'external','void()',1,0,0,NULL),
            (1,'Leaf',0,3,4,'external','void()',1,0,0,NULL);",
        )
        .unwrap();
        let mut texts = crate::TextPool::new(&conn).unwrap();
        let direct = texts.intern("direct").unwrap();
        conn.execute(
            "INSERT INTO call_edges_data VALUES(?1,NULL,0,1,?2)",
            params![i64::MAX, direct],
        )
        .unwrap();
        publish_lambda_free(&conn).unwrap();
        assert_eq!(count(&conn, "SELECT id FROM call_edges"), i64::MAX);
        assert_eq!(count(&conn, "SELECT count(*) FROM lambda_owners"), 0);
    }

    #[test]
    fn public_contract_retains_columns_and_raw_rows_and_preserves_reachability() {
        let conn = complete_graph();
        let relations = conn
            .prepare(
                "SELECT type,name FROM sqlite_schema WHERE type IN ('table','view')
            AND name NOT GLOB 'sqlite_*' ORDER BY name",
            )
            .unwrap()
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        let columns = |name: &str| -> Vec<(String, String, bool, i64)> {
            conn.prepare("SELECT name,type,\"notnull\",pk FROM pragma_table_info(?1)")
                .unwrap()
                .query_map([name], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
                .unwrap()
                .collect::<rusqlite::Result<_>>()
                .unwrap()
        };
        let before: Vec<_> = relations.iter().map(|(_, n)| columns(n)).collect();
        let rows = |name: &str| -> Vec<Vec<rusqlite::types::Value>> {
            let width = columns(name).len();
            let order = (1..=width)
                .map(|i| i.to_string())
                .collect::<Vec<_>>()
                .join(",");
            conn.prepare(&format!("SELECT * FROM {} ORDER BY {order}", quote(name)))
                .unwrap()
                .query_map([], |r| (0..width).map(|i| r.get(i)).collect())
                .unwrap()
                .collect::<rusqlite::Result<_>>()
                .unwrap()
        };
        let raw_rows: Vec<_> = relations.iter().map(|(_, n)| rows(n)).collect();
        publish_lambda_free(&conn).unwrap();
        for (i, (kind, name)) in relations.iter().enumerate() {
            assert_eq!(columns(name), before[i], "column contract for {name}");
            assert_eq!(
                conn.query_row(
                    "SELECT type FROM sqlite_schema WHERE name=?1",
                    [name],
                    |r| r.get::<_, String>(0)
                )
                .unwrap(),
                *kind
            );
            assert_eq!(rows(&format!("raw_{name}")), raw_rows[i], "complete {name}");
        }
        assert_eq!(count(&conn,"SELECT count(*) FROM sqlite_schema WHERE name GLOB '*no_lambdas*' OR name GLOB '_projection_*'"),0);
        assert_eq!(
            count(
                &conn,
                "SELECT count(*) FROM sqlite_temp_schema WHERE name GLOB '_projection_*'"
            ),
            0
        );
        assert_eq!(
            count(
                &conn,
                "SELECT count(*) FROM functions WHERE name GLOB '*::$lambda[0-9]*:[0-9]*'"
            ),
            0
        );
        assert_eq!(
            count(
                &conn,
                "SELECT count(*) FROM strings WHERE text GLOB '*::$lambda[0-9]*:[0-9]*'"
            ),
            0
        );
        assert_eq!(
            count(
                &conn,
                "SELECT count(*) FROM call_edges WHERE caller_fn_id=0 AND callee_fn_id IN (3,4)"
            ),
            3
        );
        assert_eq!(
            count(
                &conn,
                "SELECT count(*) FROM call_edges WHERE caller_fn_id=7 AND callee_fn_id IN (3,4)"
            ),
            2
        );
        assert_eq!(
            count(
                &conn,
                "SELECT count(*) FROM call_edges WHERE call_site_id=5"
            ),
            0,
            "uninvoked lambda"
        );
        assert_eq!(count(&conn, "SELECT fn_id FROM variables WHERE id=2"), 0);
        assert_eq!(
            count(&conn, "SELECT fn_id IS NULL FROM variables WHERE id=3"),
            1
        );
        assert_eq!(
            count(&conn, "SELECT count(*) FROM flow_parameters WHERE fn_id=0"),
            1,
            "parent signature"
        );
        assert_eq!(count(&conn, "SELECT count(*) FROM flow_field_access"), 1);
        assert_eq!(
            count(
                &conn,
                "SELECT count(*) FROM execution_contexts WHERE entry_fn_id IN (3,4)"
            ),
            2
        );
        assert_eq!(count(&conn,"SELECT count(*) FROM lambda_unresolved_calls WHERE caller_fn_id=0 AND call_site_id=6"),1);
        assert_eq!(
            count(
                &conn,
                "SELECT count(*) FROM lambda_unresolved_calls WHERE call_site_id=7"
            ),
            0
        );
        assert_eq!(
            count(&conn, "SELECT count(*) FROM pragma_foreign_key_check"),
            0
        );
        let graph = crate::call_graph(&conn, 0, crate::Direction::Down, 1).unwrap();
        assert!(!graph.nodes.values().any(|n| n.label.contains("$lambda")));
        assert!(graph.nodes.values().any(|n| n.label == "Leaf"));
        use_raw_relations(&conn).unwrap();
        assert_eq!(count(&conn, "SELECT count(*) FROM functions"), 8);
        assert_eq!(count(&conn, "SELECT fn_id FROM variables WHERE id=2"), 2);
        assert_eq!(
            count(
                &conn,
                "SELECT count(*) FROM execution_contexts WHERE entry_fn_id=1"
            ),
            1
        );
    }

    #[test]
    fn multiple_entering_edges_to_same_callee_deduplicate_in_public_edges() {
        let conn = complete_graph();
        // Add another entering edge from Root (fn 0) to lambda 5 at call site 0,
        // which also reaches Leaf (fn 3).
        let indirect: i64 = conn
            .query_row("SELECT id FROM strings WHERE text = 'indirect'", [], |r| {
                r.get(0)
            })
            .unwrap();
        conn.execute(
            "INSERT INTO call_edges_data VALUES(100, 0, 0, 5, ?1)",
            [indirect],
        )
        .unwrap();
        publish_lambda_free(&conn).unwrap();
        // Root has call site 0: originally edge 0 went to lambda 1 (reaching Leaf 3 and Other 4).
        // Edge 100 also at call site 0 went to lambda 5 (reaching Leaf 3).
        // In public call_edges, Root -> Leaf at site 0 must appear only ONCE!
        assert_eq!(
            count(
                &conn,
                "SELECT count(*) FROM call_edges WHERE caller_fn_id=0 AND callee_fn_id=3 AND call_site_id=0"
            ),
            1
        );
        // But lambda_call_provenance must record both entering edges (0 and 100).
        let edge_id: i64 = conn
            .query_row(
                "SELECT id FROM call_edges WHERE caller_fn_id=0 AND callee_fn_id=3 AND call_site_id=0",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let prov_count: i64 = conn
            .query_row(
                "SELECT count(*) FROM lambda_call_provenance WHERE edge_id=?1",
                [edge_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(prov_count, 2);
    }

    #[test]
    fn hash_collision_error_is_annotated() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TEMP TABLE projected_calls (
                entry_edge_id INTEGER,
                callee_fn_id INTEGER,
                projected_id INTEGER
            );
            CREATE TABLE raw_call_edges_data (
                id INTEGER PRIMARY KEY,
                callee_fn_id INTEGER
            );",
        )
        .unwrap();
        // Insert two rows that will produce the same hash: identical entry_edge_id and callee_fn_id
        conn.execute_batch(
            "INSERT INTO raw_call_edges_data VALUES(1, 10);
             INSERT INTO projected_calls VALUES(1, 20, NULL), (1, 20, NULL);",
        )
        .unwrap();
        let err = project_call_ids(&conn).unwrap_err();
        assert!(
            err.to_string().contains(
                "hash collision detected while assigning deterministic projected call edge IDs"
            ),
            "{err:#}"
        );
    }

    #[test]
    fn synthetic_edges_preserve_provenance_when_lambdas_present() {
        let conn = complete_graph();
        // Insert a synthetic IPC edge with call_site_id = NULL
        let ipc: i64 = conn
            .query_row("SELECT id FROM strings WHERE text = 'ipc'", [], |r| {
                r.get(0)
            })
            .unwrap();
        conn.execute(
            "INSERT INTO call_edges_data VALUES(200, NULL, 0, 3, ?1)",
            [ipc],
        )
        .unwrap();
        publish_lambda_free(&conn).unwrap();
        assert_eq!(
            count(
                &conn,
                "SELECT count(*) FROM call_edges WHERE id = 200 AND call_site_id IS NULL"
            ),
            1
        );
        assert_eq!(
            count(
                &conn,
                "SELECT count(*) FROM lambda_call_provenance WHERE edge_id = 200 AND entry_edge_id = 200"
            ),
            1
        );
    }

    #[test]
    fn lambda_owned_call_targets_preserve_labels_and_remap_owner() {
        let conn = complete_graph();
        // Node 4 in complete_graph() is a loc node for Lambda 1.
        // Add a call_target node inside Lambda 1 (fn_id = 1, call_site_id = 6).
        let ct_kind: i64 = conn
            .query_row(
                "SELECT id FROM strings WHERE text = 'call_target'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let target_label: i64 = conn
            .query_row("SELECT id FROM strings WHERE text = 'unknown()'", [], |r| {
                r.get(0)
            })
            .unwrap();
        conn.execute(
            "INSERT INTO flow_nodes_data(id, kind_id, label_id, detail_id, var_id, fn_id, call_site_id)
             VALUES(50, ?1, ?2, 1, NULL, 1, 6)",
            params![ct_kind, target_label],
        )
        .unwrap();
        publish_lambda_free(&conn).unwrap();

        // 1. loc node for lambda is relabeled "anonymous callable" and fn_id becomes NULL
        let loc_label: String = conn
            .query_row("SELECT label FROM flow_nodes WHERE id = 4", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(loc_label, "anonymous callable");
        assert_eq!(
            count(&conn, "SELECT fn_id IS NULL FROM flow_nodes WHERE id = 4"),
            1
        );

        // 2. call_target node keeps its label ("unknown()") and its fn_id is remapped to owner (0)
        let ct_label: String = conn
            .query_row("SELECT label FROM flow_nodes WHERE id = 50", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(ct_label, "unknown()");
        let ct_fn: i64 = conn
            .query_row("SELECT fn_id FROM flow_nodes WHERE id = 50", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(ct_fn, 0, "call_target fn_id remapped to owner_fn_id");
    }

    #[test]
    fn with_raw_relations_succeeds_under_query_only() {
        let conn = complete_graph();
        publish_lambda_free(&conn).unwrap();
        conn.execute_batch("PRAGMA query_only = ON;").unwrap();
        let count = with_raw_relations(&conn, || {
            conn.query_row("SELECT count(*) FROM functions", [], |r| r.get::<_, i64>(0))
                .map_err(Into::into)
        })
        .unwrap();
        assert_eq!(count, 8);
        assert!(conn
            .query_row("PRAGMA query_only", [], |r| r.get::<_, bool>(0))
            .unwrap());
    }

    #[test]
    fn functions_named_lambda_without_numeric_suffix_are_not_classified_as_lambdas() {
        let conn = complete_graph();
        // Add a function whose name contains ::$lambda but without digits:digits suffix.
        conn.execute(
            "INSERT INTO functions VALUES(20, 'MyClass::$lambda_helper', 0, 100, 110, 'external', 'void()', 1, 0, 0, NULL)",
            [],
        )
        .unwrap();
        publish_lambda_free(&conn).unwrap();

        // 1. Function must remain in public functions table
        assert_eq!(
            count(
                &conn,
                "SELECT count(*) FROM functions WHERE name = 'MyClass::$lambda_helper'"
            ),
            1
        );
        // 2. Must not be in lambda_owners
        assert_eq!(
            count(
                &conn,
                "SELECT count(*) FROM lambda_owners WHERE lambda_fn_id = 20"
            ),
            0
        );
    }
}
