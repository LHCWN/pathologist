use crate::common::*;

#[test]
fn opaque_parameters_retain_late_callback_slot_addresses() {
    for extension in ["c", "cpp"] {
        let root = scratch(&[(
            &format!("main.{extension}"),
            r#"
typedef void (*F)(void);
void alpha(void) {}
void put(void *p) { *(F *)p = alpha; }
void forward(void *p) { put(p); }
void initialized(void) { F cb; void *p = (void *)&cb; forward(p); cb(); }
void assigned(void) { F cb; void *p; p = (void *)&cb; forward(p); cb(); }
void control(void) { F cb; put((void *)&cb); cb(); }
"#,
        )]);
        let program = trace_parse::build_program(root.path(), &default_opts(root.path())).unwrap();
        assert!(program.diagnostics.is_empty(), "{:?}", program.diagnostics);
        let (_, analysis) = trace_analysis::analyze(&program);
        for caller in ["initialized", "assigned", "control"] {
            assert!(
                has_any_edge(&program, &analysis, caller, "alpha"),
                "{extension}: {caller}"
            );
        }
    }
}
