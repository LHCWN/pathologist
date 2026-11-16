/*
 * Function models: dataflow through bodyless memory functions
 * (memcpy_s / memset_s) and through project wrappers described in a TOML
 * model configuration. See docs/MODELS.md ("Function models").
 *
 * OpsA / OpsB are structurally identical but DISTINCT types: instance-
 * insensitive field summaries are keyed per type, so copying an initialized
 * OpsA table into an OpsB object is invisible to the baseline analysis.
 * Function models bridge exactly this gap.
 */

struct OpsA {
    int (*run)(int);
    int value;
};

struct OpsB {
    int (*run)(int);
    int value;
};

static int impl_run(int x) { return x + 1; }

struct OpsA g_src;
struct OpsB g_dst;
struct OpsB g_fourth;
struct OpsA g_cleared;

void fill_source(void) {
    g_src.run = impl_run;
    g_src.value = 41;
}

/* Prototype-only secure-libc copy: builtin model mem_copy dst=0 src=2. */
void copy_via_memcpy_s(void) {
    memcpy_s(&g_dst, sizeof(g_dst), &g_src, sizeof(g_src));
    (void)g_dst.run(7);
}

/* Project wrapper: modeled only via models.toml (mem_copy dst=0 src=1). */
void copy_via_wrapper(struct OpsB *dst, struct OpsA *src);

void call_through_wrapper_copy(void) {
    copy_via_wrapper(&g_fourth, &g_src);
    (void)g_fourth.run(9);
}

/* Terminator: builtin model clears param=0; introduces no values. */
void clear_only(void) {
    memset_s(&g_cleared, sizeof(g_cleared), 0, sizeof(g_cleared));
}

/* Heap-return summary: builtin return_heap for malloc. */
struct OpsA *alloc_ops(void) {
    return (struct OpsA *)malloc(sizeof(struct OpsA));
}

/* Heap-return summary chained with realloc growth. */
struct OpsA *grow_ops(struct OpsA *old) {
    return (struct OpsA *)realloc(old, 2 * sizeof(struct OpsA));
}

/* Callers make the wrapper return flows reachable from the entry set. */
struct OpsA *use_alloc(void) {
    struct OpsA *p = alloc_ops();
    struct OpsA *g = grow_ops(p);
    return g;
}
