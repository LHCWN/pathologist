use anyhow::Result;
use clap::Parser;
use std::io::Write;
use std::path::PathBuf;
use trace_merge::{merge_databases, MergeOptions, MergeReport, WarningKind};

#[derive(Parser, Debug)]
#[command(
    name = "trace-merge",
    version = env!("CARGO_PKG_VERSION"),
    about = "Merge multiple trace analysis databases and reconstruct cross-repository callgraph"
)]
struct Cli {
    /// Input SQLite databases from `trace analyze`.
    #[arg(required = true)]
    inputs: Vec<PathBuf>,

    /// Output merged SQLite database path [default: unified.db].
    #[arg(short, long, default_value = "unified.db")]
    output: PathBuf,

    /// Print detailed information about all cross-repo diagnostics.
    #[arg(short, long)]
    verbose: bool,
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    eprintln!(
        "trace-merge: merging {} database(s) into {}...",
        cli.inputs.len(),
        cli.output.display()
    );

    let report = merge_databases(
        &cli.inputs,
        &MergeOptions {
            output: cli.output.clone(),
            verbose: cli.verbose,
        },
    )?;

    let mut stderr = std::io::stderr().lock();
    render_warnings(&report, cli.verbose, &mut stderr)?;
    drop(stderr);

    eprintln!(
        "\nmerge complete: \
         {} databases -> {} \
         ({} files, {} functions [{} defined, {} decls], {} call edges \
         [{} cross-repo resolved, {} unresolved external, {} ambiguous, {} IPC bridges and {} member calls reconstructed])",
        report.input_dbs.len(),
        cli.output.display(),
        report.files_total,
        report.functions_total,
        report.functions_defined,
        report.functions_declarations,
        report.call_edges_total,
        report.cross_repo_calls_resolved,
        report.external_calls_unresolved,
        report.ambiguous_calls,
        report.ipc_bridges_added,
        report.member_calls_added,
    );

    Ok(())
}

/// Print merge diagnostics to `out`, grouped by kind. Collision details are
/// always shown; unresolved/weak details need `--verbose`.
fn render_warnings(
    report: &MergeReport,
    verbose: bool,
    out: &mut impl Write,
) -> std::io::Result<()> {
    if report.warnings.is_empty() {
        return Ok(());
    }

    let mut collisions = Vec::new();
    let mut unresolved = Vec::new();
    let mut weak_overrides = Vec::new();
    let mut duplicates = Vec::new();
    let mut without_contexts = Vec::new();
    let mut without_sequencing = Vec::new();
    let mut without_linking = Vec::new();

    for w in &report.warnings {
        match w.kind {
            WarningKind::MultipleDefinitions => collisions.push(w),
            WarningKind::UnresolvedExternal => unresolved.push(w),
            WarningKind::WeakOverride => weak_overrides.push(w),
            WarningKind::DuplicateInput => duplicates.push(w),
            WarningKind::MissingExecutionContexts => without_contexts.push(w),
            WarningKind::MissingSequencing => without_sequencing.push(w),
            WarningKind::MissingLinkingFacts => without_linking.push(w),
        }
    }

    writeln!(
        out,
        "\nwarning: merge diagnostics: {} collision(s), {} unresolved external symbol(s), {} weak override(s)",
        collisions.len(),
        unresolved.len(),
        weak_overrides.len()
    )?;

    // Always display multiple definition collisions in full: they are critical ODR / linking conflicts.
    if !collisions.is_empty() {
        writeln!(out, "  Collisions (multiple strong definitions):")?;
        for (i, w) in collisions.iter().enumerate() {
            writeln!(out, "    {}. [collision] {}", i + 1, w.message)?;
        }
    }

    for w in &duplicates {
        writeln!(out, "    [duplicate-input] {}", w.message)?;
    }
    for w in &without_linking {
        writeln!(out, "    [no-linking-facts] {}", w.message)?;
    }
    for w in &without_contexts {
        writeln!(out, "    [no-execution-contexts] {}", w.message)?;
    }
    for w in &without_sequencing {
        writeln!(out, "    [no-sequencing] {}", w.message)?;
    }

    if verbose {
        if !unresolved.is_empty() {
            writeln!(out, "  Unresolved external functions:")?;
            for (i, w) in unresolved.iter().enumerate() {
                writeln!(out, "    {}. [unresolved] {}", i + 1, w.message)?;
            }
        }
        if !weak_overrides.is_empty() {
            writeln!(out, "  Weak symbol overrides:")?;
            for (i, w) in weak_overrides.iter().enumerate() {
                writeln!(out, "    {}. [weak-override] {}", i + 1, w.message)?;
            }
        }
    } else if !unresolved.is_empty() || !weak_overrides.is_empty() {
        writeln!(
            out,
            "  (pass --verbose to see detailed locations for all {} diagnostics)",
            report.warnings.len()
        )?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use trace_merge::{MergeReport, MergeWarning};

    fn warning(kind: WarningKind, message: &str) -> MergeWarning {
        MergeWarning {
            kind,
            message: message.to_string(),
            symbol: None,
            file_id: None,
            line: 0,
        }
    }

    fn mixed_report() -> MergeReport {
        MergeReport {
            warnings: vec![
                warning(WarningKind::MultipleDefinitions, "collision one"),
                warning(WarningKind::MultipleDefinitions, "collision two"),
                warning(WarningKind::UnresolvedExternal, "missing callee"),
                warning(WarningKind::WeakOverride, "weak wins"),
                warning(WarningKind::DuplicateInput, "same input twice"),
                warning(WarningKind::MissingExecutionContexts, "old export"),
                warning(WarningKind::MissingSequencing, "no sequencing"),
                warning(WarningKind::MissingLinkingFacts, "no linking facts"),
            ],
            ..MergeReport::default()
        }
    }

    fn rendered(report: &MergeReport, verbose: bool) -> String {
        let mut out = Vec::new();
        render_warnings(report, verbose, &mut out).unwrap();
        String::from_utf8(out).unwrap()
    }

    #[test]
    fn warnings_report_each_kind_and_hide_details_without_verbose() {
        let report = mixed_report();
        let text = rendered(&report, false);

        assert!(text.starts_with(
            "\nwarning: merge diagnostics: 2 collision(s), 1 unresolved external symbol(s), 1 weak override(s)"
        ));
        assert!(text.contains("  Collisions (multiple strong definitions):"));
        assert!(text.contains("    1. [collision] collision one"));
        assert!(text.contains("    2. [collision] collision two"));
        assert!(text.contains("    [duplicate-input] same input twice"));
        assert!(text.contains("    [no-linking-facts] no linking facts"));
        assert!(text.contains("    [no-execution-contexts] old export"));
        assert!(text.contains("    [no-sequencing] no sequencing"));
        assert!(text.contains("  (pass --verbose to see detailed locations for all 8 diagnostics)"));
        assert!(!text.contains("[unresolved]"));
        assert!(!text.contains("[weak-override]"));
    }

    #[test]
    fn verbose_warnings_include_unresolved_and_weak_details() {
        let text = rendered(&mixed_report(), true);

        assert!(text.contains("  Unresolved external functions:"));
        assert!(text.contains("    1. [unresolved] missing callee"));
        assert!(text.contains("  Weak symbol overrides:"));
        assert!(text.contains("    1. [weak-override] weak wins"));
        assert!(!text.contains("pass --verbose"));
    }

    #[test]
    fn no_warnings_render_nothing() {
        assert_eq!(rendered(&MergeReport::default(), false), "");
    }
}
