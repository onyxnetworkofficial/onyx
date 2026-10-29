//! Host determinism and rejection tests (ADR-0004, THREATS T-DET-*, R-DET-4, R-DUPKEY-1).

use onyx_core::{keccak256, RuntimeProfile};
use onyx_recipe_host::{failure_leaf, Host, HostError, Outcome, HOST_ID};

fn profile() -> RuntimeProfile {
    RuntimeProfile {
        abi_version: 1,
        host: HOST_ID.into(),
        fuel_limit: 10_000_000,
        max_memory_bytes: 16 << 20,
        max_output_bytes: 1 << 20,
        max_call_depth: 64,
    }
}

/// ABI exports around a custom `onyx_run` body and data.
fn module(run_body: &str, extra: &str) -> Vec<u8> {
    let text = format!(
        r#"(module
          (memory (export "memory") 1)
          ;; RunReturn::Ok with rows [(key [1], value [2])]: 15 bytes at 2048
          (data (i32.const 2048) "\00\01\00\00\00\01\00\00\00\01\01\00\00\00\02")
          ;; Same, two rows with keys out of order: [2] then [1]
          (data (i32.const 4096) "\00\02\00\00\00\01\00\00\00\02\01\00\00\00\02\01\00\00\00\01\01\00\00\00\02")
          ;; RunReturn::Ok with zero rows: 5 bytes at 6144
          (data (i32.const 6144) "\00\00\00\00\00")
          (func (export "onyx_abi_version") (result i32) i32.const 1)
          (func (export "onyx_alloc") (param i32) (result i32) i32.const 1024)
          (func (export "onyx_run") (param i32 i32) (result i64) {run_body})
          {extra})"#
    );
    wat::parse_str(text).expect("wat")
}

const OK_RUN: &str = "i64.const 8796093022223"; // (2048 << 32) | 15

fn run(wasm: &[u8]) -> Result<onyx_recipe_host::RunResult, HostError> {
    Host::new()
        .expect("host")
        .run(&keccak256(wasm), wasm, b"input", &profile())
}

#[test]
fn ok_run_is_byte_identical_across_runs_compilation_modes_and_threads() {
    let wasm = module(OK_RUN, "");
    let first = run(&wasm).expect("run");
    assert_eq!(first.outcome, Outcome::Ok);
    assert_eq!(first.leaves.len(), 1);
    for _ in 0..3 {
        assert_eq!(run(&wasm).expect("run"), first);
    }
    // The advisory fast path (wasmtime, serial and parallel compilation) agrees
    // on outcome and leaves; fuel is engine-specific and not compared.
    for parallel in [false, true] {
        let fast = onyx_recipe_host::fast::FastHost::with_parallel_compilation(parallel)
            .expect("fast host")
            .allowing(keccak256(&wasm))
            .run(&keccak256(&wasm), &wasm, b"input", &profile())
            .expect("run");
        assert_eq!((fast.outcome, &fast.leaves), (first.outcome, &first.leaves));
    }
    let handles: Vec<_> = (0..8)
        .map(|_| {
            let w = wasm.clone();
            std::thread::spawn(move || run(&w).expect("run"))
        })
        .collect();
    for h in handles {
        assert_eq!(h.join().expect("join"), first);
    }
}

#[test]
fn floats_rejected() {
    let wasm = module(
        OK_RUN,
        "(func (param f64) (result f64) local.get 0 local.get 0 f64.add)",
    );
    assert!(matches!(run(&wasm), Err(HostError::Invalid(_))));
    // Float conversion hidden in an integer-looking function.
    let wasm = module(
        OK_RUN,
        "(func (param i32) (result i64) local.get 0 f32.convert_i32_s i64.trunc_sat_f32_s)",
    );
    assert!(matches!(run(&wasm), Err(HostError::Invalid(_))));
}

#[test]
fn simd_rejected() {
    let wasm = module(OK_RUN, "(func (result v128) v128.const i32x4 0 0 0 0)");
    assert!(matches!(run(&wasm), Err(HostError::Invalid(_))));
}

#[test]
fn any_import_rejected() {
    let text = r#"(module
        (import "env" "clock" (func (result i64)))
        (memory (export "memory") 1)
        (func (export "onyx_abi_version") (result i32) i32.const 1)
        (func (export "onyx_alloc") (param i32) (result i32) i32.const 1024)
        (func (export "onyx_run") (param i32 i32) (result i64) call 0))"#;
    let wasm = wat::parse_str(text).expect("wat");
    assert!(matches!(run(&wasm), Err(HostError::ForbiddenImport(_))));
}

#[test]
fn infinite_loop_runs_out_of_fuel_deterministically() {
    let wasm = module("(loop br 0) i64.const 0", "");
    let a = run(&wasm).expect("run");
    assert_eq!(a.outcome, Outcome::OutOfFuel);
    assert_eq!(a.leaves, vec![failure_leaf(Outcome::OutOfFuel)]);
    assert_eq!(a.fuel_used, profile().fuel_limit);
    assert_eq!(run(&wasm).expect("run"), a);
}

#[test]
fn unbounded_recursion_hits_instrumented_depth_limit() {
    let wasm = module("call $rec i64.const 0", "(func $rec call $rec)");
    let a = run(&wasm).expect("run");
    assert_eq!(a.outcome, Outcome::Trap);
    // Trapped by depth, long before fuel ran out.
    assert!(a.fuel_used < profile().fuel_limit / 10, "{}", a.fuel_used);
    assert_eq!(run(&wasm).expect("run"), a);
    // A deeper pinned limit traps later, with more fuel used: depth is what stopped it.
    let mut deep = profile();
    deep.max_call_depth = 1024;
    let b = Host::new()
        .expect("host")
        .run(&keccak256(&wasm), &wasm, b"", &deep)
        .expect("run");
    assert_eq!(b.outcome, Outcome::Trap);
    assert!(b.fuel_used > a.fuel_used);
}

#[test]
fn depth_limit_allows_calls_within_limit() {
    // 10 nested calls with limit 64 run fine.
    let mut chain = String::new();
    for i in 0..10 {
        chain.push_str(&format!("(func $f{i} call $f{})", i + 1));
    }
    chain.push_str("(func $f10)");
    let wasm = module(&format!("call $f0 {OK_RUN}"), &chain);
    assert_eq!(run(&wasm).expect("run").outcome, Outcome::Ok);
}

#[test]
fn unsorted_or_empty_output_is_bad_output() {
    let wasm = module("i64.const 17592186044441", ""); // (4096 << 32) | 25
    assert_eq!(run(&wasm).expect("run").outcome, Outcome::BadOutput);
    let wasm = module(
        "i64.const 26388279066629", // (6144 << 32) | 5: RunReturn::Ok with zero rows
        "",
    );
    assert_eq!(run(&wasm).expect("run").outcome, Outcome::BadOutput);
}

#[test]
fn oversized_output_rejected() {
    let wasm = module(OK_RUN, "");
    let mut p = profile();
    p.max_output_bytes = 4;
    let r = Host::new()
        .expect("host")
        .run(&keccak256(&wasm), &wasm, b"", &p)
        .expect("run");
    assert_eq!(r.outcome, Outcome::OutputTooLarge);
}

#[test]
fn memory_growth_beyond_pinned_limit_fails_deterministically() {
    // Try to grow by 1000 pages (64 MiB) with a 16 MiB limit: memory.grow returns -1, recipe traps.
    let wasm = module(
        "(if (i32.eq (memory.grow (i32.const 1000)) (i32.const -1)) (then unreachable)) i64.const 0",
        "",
    );
    let a = run(&wasm).expect("run");
    assert_eq!(a.outcome, Outcome::Trap);
    assert_eq!(run(&wasm).expect("run"), a);
}

#[test]
fn load_time_checks() {
    let wasm = module(OK_RUN, "");
    let host = Host::new().expect("host");
    assert!(matches!(
        host.run(&keccak256(b"other"), &wasm, b"", &profile()),
        Err(HostError::IdMismatch(_))
    ));
    let mut p = profile();
    p.host = "onyx-host.9.9.9".into();
    assert!(matches!(
        host.run(&keccak256(&wasm), &wasm, b"", &p),
        Err(HostError::WrongProfile(_))
    ));
    let few_locals = module(OK_RUN, "(func (local i64 i64) (local i32) nop)");
    assert!(run(&few_locals).is_ok());
    let text = format!(
        "(func {})",
        "(local i32) ".repeat(onyx_recipe_host::MAX_FRAME_SLOTS as usize + 1)
    );
    let too_many = module(OK_RUN, &text);
    assert!(matches!(run(&too_many), Err(HostError::FrameTooLarge(_))));
    let mut big_mem = profile();
    big_mem.max_memory_bytes = onyx_recipe_host::MAX_MEMORY_BYTES + 1;
    assert!(matches!(
        host.run(&keccak256(&wasm), &wasm, b"", &big_mem),
        Err(HostError::WrongProfile(_))
    ));
    let no_exports = wat::parse_str("(module (memory (export \"memory\") 1))").expect("wat");
    assert!(matches!(run(&no_exports), Err(HostError::BadAbi(_))));
}

mod input_assembly {
    use onyx_core::{
        keccak256, AddressCanon, AddressProof, ChainId, Confidence, DataRights, EntityKind,
        LabelSetRef, ProofOutcome, ProofVector, Registry, ShardSpec, Snapshot, SourceId, SourceRef,
        StaticRegistry,
    };
    use onyx_labels::{DropReason, Label, LabelSet};
    use onyx_recipe_host::{assemble_input, HostError};
    use onyx_recipe_sdk::{LabelSetView, RecipeInput};

    struct Lower;
    impl AddressCanon for Lower {
        fn namespace(&self) -> &str {
            "t"
        }
        fn canon_id(&self) -> &str {
            "onyx.canon.test.v1"
        }
        fn vectors(&self) -> &'static [(&'static str, bool)] {
            &[("a", true), ("A", false)]
        }
        fn is_canonical(&self, body: &str) -> bool {
            !body.bytes().any(|c| c.is_ascii_uppercase())
        }
    }

    /// Proof rule that never validates (attestation paths are tested in source-btc).
    struct NoProof;
    impl AddressProof for NoProof {
        fn namespace(&self) -> &str {
            "t"
        }
        fn chain(&self) -> &str {
            "t.main"
        }
        fn proof_id(&self) -> &str {
            "onyx.proof.test.v1"
        }
        fn max_window(&self) -> u64 {
            10
        }
        fn vectors(&self) -> &'static [ProofVector] {
            &[]
        }
        fn verify(&self, _: &str, _: &[u8], _: &str) -> ProofOutcome {
            ProofOutcome::Invalid
        }
    }

    fn reg() -> StaticRegistry {
        let mut r = StaticRegistry::new();
        r.set_domain("onyx.devnet")
            .and_then(|r| r.add_entity("entity:x", EntityKind::Exchange))
            .and_then(|r| r.add_canon(Box::new(Lower)))
            .and_then(|r| r.add_proof(Box::new(NoProof)))
            .and_then(|r| r.add_source(DataRights::public_chain(SourceId("chain.t.main".into()))))
            .expect("reg");
        r
    }

    fn set(addrs: &[&str]) -> LabelSet {
        LabelSet {
            name: "onyx.test".into(),
            version: 1,
            labels: addrs
                .iter()
                .map(|a| Label {
                    address: (*a).into(),
                    entity: "entity:x".into(),
                    kind: EntityKind::Exchange,
                    confidence: Confidence(900_000),
                    evidence: vec![],
                })
                .collect(),
        }
    }

    fn pin(s: &LabelSet) -> LabelSetRef {
        LabelSetRef {
            name: s.name.clone(),
            version: s.version,
            content_hash: s.content_hash(),
        }
    }

    fn spec(r: &StaticRegistry, label_set: Option<LabelSetRef>) -> ShardSpec {
        let source = SourceId("chain.t.main".into());
        ShardSpec {
            recipe: None,
            runtime: None,
            label_set,
            rights_hash: r.source_rights(&source).expect("rights").hash(),
            source: SourceRef {
                source,
                snapshot: Snapshot::BlockRange {
                    chain: ChainId("t.main".into()),
                    from_height: 1,
                    to_height: 2,
                    tip_hash: keccak256(b"tip"),
                    content_hash: keccak256(&[]),
                },
            },
            schema: keccak256(b"schema"),
            params: vec![2],
            registry_root: r.root(),
        }
    }

    /// R-LBLVIEW-1, L13-F3: only the pinned, valid label set reaches a recipe,
    /// and everything comes from the shard spec and its registry.
    #[test]
    fn only_pinned_valid_labels_reach_the_recipe() {
        let r = reg();
        let good = set(&["t:a", "t:b"]);
        let out = assemble_input(&spec(&r, Some(pin(&good))), vec![], Some(&good), &[], &r)
            .expect("assemble");
        // L14-F10: source bytes must match the snapshot's content hash.
        assert!(matches!(
            assemble_input(&spec(&r, Some(pin(&good))), vec![1], Some(&good), &[], &r),
            Err(HostError::BadSource)
        ));
        assert!(out.dropped.is_empty());
        let input: RecipeInput = borsh::from_slice(&out.bytes).expect("decode");
        assert_eq!(input.params, vec![2], "params come from the spec");
        let view: LabelSetView = borsh::from_slice(&input.labels).expect("view");
        assert!(view.is_canonical());
        assert_eq!(view.labels.len(), 2);

        let bad = |labels: Option<&LabelSet>, s: &ShardSpec| {
            matches!(
                assemble_input(s, vec![], labels, &[], &r),
                Err(HostError::BadLabels(_))
            )
        };
        let unsorted = set(&["t:b", "t:a"]);
        assert!(bad(Some(&unsorted), &spec(&r, Some(pin(&unsorted)))));
        let other = set(&["t:a"]);
        assert!(bad(Some(&other), &spec(&r, Some(pin(&good)))));
        assert!(bad(None, &spec(&r, Some(pin(&good)))));
        assert!(bad(Some(&good), &spec(&r, None)));
        let mut forged = pin(&good);
        forged.content_hash = keccak256(b"forged");
        assert!(bad(Some(&good), &spec(&r, Some(forged))));
        // A spec pinned to another registry root is refused.
        let mut stale = spec(&r, Some(pin(&good)));
        stale.registry_root = keccak256(b"other registry");
        assert!(bad(Some(&good), &stale));
    }

    /// L15-F2: source bytes must hash to a document snapshot's content hash;
    /// a single-block snapshot pins no bytes and cannot feed a recipe.
    #[test]
    fn document_and_block_snapshots() {
        let r = reg();
        let mut doc = spec(&r, None);
        doc.source.snapshot = Snapshot::Document {
            published_unix: 1,
            content_hash: keccak256(b"the filing"),
        };
        assert!(assemble_input(&doc, b"the filing".to_vec(), None, &[], &r).is_ok());
        assert!(matches!(
            assemble_input(&doc, b"an edited filing".to_vec(), None, &[], &r),
            Err(HostError::BadSource)
        ));
        let mut block = spec(&r, None);
        block.source.snapshot = Snapshot::Block {
            chain: ChainId("t.main".into()),
            height: 1,
            block_hash: keccak256(b"block"),
        };
        assert!(matches!(
            assemble_input(&block, vec![], None, &[], &r),
            Err(HostError::BadSource)
        ));
    }

    /// L12-F1, L13-F4: a full-confidence label without an attestation never
    /// reaches a recipe; the rest of the set does.
    #[test]
    fn unattested_full_confidence_label_is_dropped() {
        let r = reg();
        let mut s = set(&["t:a", "t:b"]);
        s.labels[0].confidence = Confidence(Confidence::ONE);
        let out =
            assemble_input(&spec(&r, Some(pin(&s))), vec![], Some(&s), &[], &r).expect("assemble");
        assert_eq!(
            out.dropped,
            vec![("t:a".to_string(), DropReason::UnattestedFullConfidence)]
        );
        let input: RecipeInput = borsh::from_slice(&out.bytes).expect("decode");
        let view: LabelSetView = borsh::from_slice(&input.labels).expect("view");
        assert_eq!(view.labels.len(), 1);
        assert_eq!(view.labels[0].address, "t:b");
    }
}

/// `n` live i64 values on the operand stack around a recursive call.
fn big_frame_recursion(n: usize) -> Vec<u8> {
    let loads = "(i64.load (i32.const 0)) ".repeat(n);
    let drops = "drop ".repeat(n);
    module(
        "call $deep i64.const 0",
        &format!("(func $deep {loads} call $deep {drops})"),
    )
}

/// R-HOST-STACK-1: frames whose operand stack could overflow the native stack
/// before the depth counter are rejected at load.
#[test]
fn oversized_frames_rejected() {
    let wasm = big_frame_recursion(16_000);
    assert!(matches!(run(&wasm), Err(HostError::FrameTooLarge(_))));
}

/// R-HOST-STACK-1: the largest allowed frame still traps by the depth counter,
/// never by native stack overflow: the trap point moves with the pinned depth.
#[test]
fn large_allowed_frames_trap_by_depth_counter() {
    let wasm = big_frame_recursion(2000);
    let host = Host::new().expect("host");
    let mut p = profile();
    p.max_call_depth = onyx_recipe_host::MAX_CALL_DEPTH;
    let deep = host.run(&keccak256(&wasm), &wasm, b"", &p).expect("run");
    assert_eq!(deep.outcome, Outcome::Trap);
    p.max_call_depth = onyx_recipe_host::MAX_CALL_DEPTH / 2;
    let half = host.run(&keccak256(&wasm), &wasm, b"", &p).expect("run");
    assert_eq!(half.outcome, Outcome::Trap);
    // Each frame costs the same fuel, so trapping at the counter means fuel scales with depth.
    let per_frame =
        (deep.fuel_used - half.fuel_used) / u64::from(onyx_recipe_host::MAX_CALL_DEPTH / 2);
    assert!(per_frame > 2000, "per-frame fuel {per_frame}");
    assert_eq!(
        host.run(&keccak256(&wasm), &wasm, b"", &p).expect("run"),
        half
    );
}

/// R-HOST-TABLE-1: table growth is stopped by the limiter before allocating.
#[test]
fn table_growth_capped() {
    let wasm = module(
        "(if (i32.eq (table.grow 0 (ref.null func) (i32.const 1073741824)) (i32.const -1)) (then unreachable)) i64.const 0",
        "(table 1 funcref)",
    );
    let r = run(&wasm).expect("run");
    assert_eq!(r.outcome, Outcome::Trap);
    assert!(r.fuel_used < 1_000, "fuel {}", r.fuel_used);
    let declared = module(OK_RUN, "(table 1073741824 funcref)");
    assert!(matches!(run(&declared), Err(HostError::TooLarge(_))));
}

/// R-HOST-STACK-2 (found by the loop-6 review): with Cranelift's optimizer,
/// loop invariants are hoisted, so a function with 7 wasm frame slots got a
/// ~250 KiB native frame and recursion to depth 1000 (< the pinned 1024)
/// overflowed the native stack. The canonical engine (wasmi) keeps its stacks
/// on the heap, so this must now be `Ok` (ADR-0014).
#[test]
fn compiler_hoisted_values_must_not_overflow_native_stack() {
    let mut body = String::new();
    let mut cnt = 0;
    'o: for base in ["$a", "$b", "$c", "$d", "$e"] {
        for k in (-8191i64..=8191).step_by(2) {
            if k == 1 || k == -1 {
                continue;
            }
            body.push_str(&format!("local.get {base} i64.const {k} i64.mul i64.xor\n"));
            cnt += 1;
            if cnt == 36_900 {
                break 'o;
            }
        }
    }
    let f = format!(
        r#"(func $f (param $n i32) (result i64) (local $acc i64) (local $i i32)
        (local $a i64) (local $b i64) (local $c i64) (local $d i64) (local $e i64)
        (local.set $a (i64.load (i32.const 0))) (local.set $b (i64.load (i32.const 8)))
        (local.set $c (i64.load (i32.const 16))) (local.set $d (i64.load (i32.const 24)))
        (local.set $e (i64.load (i32.const 32)))
        (loop $L
          local.get $acc
          {body}
          local.set $acc
          local.get $i i32.const 1 i32.add local.tee $i i32.const 2 i32.lt_u br_if $L)
        (if (i32.gt_u (local.get $n) (i32.const 0)) (then
          (local.set $acc (i64.add (local.get $acc) (call $f (i32.sub (local.get $n) (i32.const 1)))))))
        local.get $acc)"#
    );
    let wasm = module(&format!("(drop (call $f (i32.const 1000))) {OK_RUN}"), &f);
    let mut p = profile();
    p.max_call_depth = onyx_recipe_host::MAX_CALL_DEPTH;
    p.fuel_limit = 40_000_000_000;
    let r = Host::new()
        .expect("host")
        .run(&keccak256(&wasm), &wasm, b"", &p)
        .expect("run");
    assert_eq!(
        r.outcome,
        Outcome::Ok,
        "native stack overflow before the depth limit"
    );
}

/// R-PROFILE-1: cold and cached paths give the same profile verdict.
#[test]
fn profile_checked_identically_cold_and_cached() {
    let wasm = module(OK_RUN, "");
    let host = Host::new().expect("host");
    let mut bad = profile();
    bad.abi_version = 2;
    let cold = host.run(&keccak256(&wasm), &wasm, b"", &bad);
    host.run(&keccak256(&wasm), &wasm, b"", &profile())
        .expect("warm the cache");
    let warm = host.run(&keccak256(&wasm), &wasm, b"", &bad);
    assert_eq!(cold, warm);
    assert!(matches!(warm, Err(HostError::WrongProfile(_))));
}

/// R-CACHE-2: the compile cache is bounded.
#[test]
fn compile_cache_is_bounded() {
    let host = Host::new().expect("host");
    let wasm = module(OK_RUN, "");
    for depth in 1..=(onyx_recipe_host::HOST_CACHE_MAX_ENTRIES as u32 + 8) {
        let mut p = profile();
        p.max_call_depth = depth;
        host.run(&keccak256(&wasm), &wasm, b"", &p).expect("run");
    }
    assert!(host.cached_modules() <= onyx_recipe_host::HOST_CACHE_MAX_ENTRIES);
}

/// Recursion `n` deep below `onyx_run`: `n + 1` nested calls.
fn recursion(n: u32) -> Vec<u8> {
    module(
        &format!("(call $f (i32.const {n})) {OK_RUN}"),
        "(func $f (param $n i32)
           (if (i32.gt_u (local.get $n) (i32.const 0))
             (then (call $f (i32.sub (local.get $n) (i32.const 1))))))",
    )
}

/// L8-DEPTH-1: both engines count depth the same way: `max_call_depth` nested
/// calls below the entry export are allowed, one more traps.
#[test]
fn depth_boundary_identical_on_both_engines() {
    let canonical = Host::new().expect("host");
    let fast = onyx_recipe_host::fast::FastHost::new().expect("fast");
    for d in [64u32, 1024] {
        let mut p = profile();
        p.max_call_depth = d;
        for (arg, want) in [
            (d - 2, Outcome::Ok),
            (d - 1, Outcome::Ok),
            (d, Outcome::Trap),
        ] {
            let w = recursion(arg);
            let a = canonical
                .run(&keccak256(&w), &w, b"", &p)
                .expect("canonical");
            fast.allow(keccak256(&w));
            fast.allow(keccak256(&w));
            let b = fast.run(&keccak256(&w), &w, b"", &p).expect("fast");
            assert_eq!(a.outcome, want, "canonical depth {d} arg {arg}");
            assert_eq!(b.outcome, want, "fast depth {d} arg {arg}");
            assert_eq!(a.leaves, b.leaves);
        }
    }
}

/// Grows memory by ~1000 MiB and returns a different Ok row depending on
/// whether the growth succeeded (rows (5,6)) or returned -1 (rows (1,2)).
fn grow_probe() -> Vec<u8> {
    module(
        "(if (result i64) (i32.eq (memory.grow (i32.const 16000)) (i32.const -1))
           (then i64.const 8796093022223)
           (else i64.const 35184372088847))",
        r#"(data (i32.const 8192) "\00\01\00\00\00\01\00\00\00\05\01\00\00\00\06")"#,
    )
}

fn grow_profile() -> RuntimeProfile {
    let mut p = profile();
    p.max_memory_bytes = onyx_recipe_host::MAX_MEMORY_BYTES;
    // memory.grow is charged fuel per byte in wasmi; growing ~1000 MiB needs a large budget.
    p.fuel_limit = 1 << 40;
    p
}

/// Child half of `machine_oom_abstains_instead_of_changing_the_verdict`; runs
/// only when spawned under a low `ulimit -v`.
#[test]
#[ignore = "spawned by machine_oom_abstains_instead_of_changing_the_verdict"]
fn oom_child_probe() {
    if std::env::var_os("ONYX_OOM_CHILD").is_none() {
        return;
    }
    let w = grow_probe();
    let r = Host::new()
        .expect("host")
        .run(&keccak256(&w), &w, b"", &grow_profile());
    println!("OOM_CHILD_RESULT: {r:?}");
    // The fast path must abstain too, never turn a machine failure into a verdict (L9-F3).
    let f = onyx_recipe_host::fast::FastHost::new().and_then(|h| {
        h.allowing(keccak256(&w))
            .run(&keccak256(&w), &w, b"", &grow_profile())
    });
    println!("OOM_CHILD_FAST: {f:?}");
}

/// L8-OOM-1: on a machine with enough memory the growth succeeds (Ok, row 5);
/// on a machine that cannot allocate it, the validator abstains instead of
/// committing the other Ok root.
#[test]
fn machine_oom_abstains_instead_of_changing_the_verdict() {
    let w = grow_probe();
    let normal = Host::new()
        .expect("host")
        .run(&keccak256(&w), &w, b"", &grow_profile())
        .expect("normal run");
    assert_eq!(normal.outcome, Outcome::Ok);
    assert!(
        normal.leaves[0].ends_with(&[1, 0, 0, 0, 6]),
        "growth succeeded"
    );
    if !cfg!(target_os = "linux") {
        return;
    }
    let exe = std::env::current_exe().expect("exe");
    let out = std::process::Command::new("sh")
        .arg("-c")
        .arg("ulimit -v 700000 && exec \"$0\" oom_child_probe --exact --ignored --nocapture --test-threads=1")
        .arg(exe)
        .env("ONYX_OOM_CHILD", "1")
        .output()
        .expect("spawn child");
    let stdout = String::from_utf8_lossy(&out.stdout);
    // Abstain specifically because of memory, not for some other reason (L9-F6).
    assert!(
        stdout.contains("OOM_CHILD_RESULT: Err(Abstain(\"out of system memory"),
        "canonical child must abstain for memory, got: {stdout} {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        stdout.contains("OOM_CHILD_FAST: Err(Abstain("),
        "fast child must abstain, got: {stdout}"
    );
}

/// L8-COMPILE-3 and caps: oversized br_tables, wide function types and huge
/// initial memories are rejected before validation cost is paid.
#[test]
fn compile_cost_caps() {
    let targets = "0 ".repeat(onyx_recipe_host::MAX_BR_TABLE_TARGETS_PER_FUNCTION + 1);
    let big_table = module(
        OK_RUN,
        &format!("(func (param i32) (block (local.get 0) (br_table {targets} 0)))"),
    );
    assert!(matches!(run(&big_table), Err(HostError::TooLarge(_))));
    let results = "i32 ".repeat(onyx_recipe_host::MAX_TYPE_RESULTS + 1);
    let consts = "i32.const 0 ".repeat(onyx_recipe_host::MAX_TYPE_RESULTS + 1);
    let wide = module(OK_RUN, &format!("(func (result {results}) {consts})"));
    assert!(matches!(run(&wide), Err(HostError::TooLarge(_))));
    let text = r#"(module (memory (export "memory") 2000)
        (func (export "onyx_abi_version") (result i32) i32.const 1)
        (func (export "onyx_alloc") (param i32) (result i32) i32.const 1024)
        (func (export "onyx_run") (param i32 i32) (result i64) i64.const 0))"#;
    let big_mem = wat::parse_str(text).expect("wat");
    assert!(matches!(run(&big_mem), Err(HostError::TooLarge(_))));
}

/// L8-VALID-4: `prepare` includes the canonical engine's own validation, and the
/// fast path refuses whatever the canonical engine refuses.
#[test]
fn prepare_means_canonical_engine_accepts() {
    let w = module(OK_RUN, "");
    let host = Host::new().expect("host");
    assert_eq!(host.prepare(&keccak256(&w), &w, &profile()), Ok(()));
    assert!(onyx_recipe_host::canonical_accepts(&w).is_ok());
    assert!(matches!(
        onyx_recipe_host::canonical_accepts(b"\0asm\x01\0\0\0\x0bgarbage"),
        Err(HostError::EngineRejected(_))
    ));
}

/// L9-F1: the canonical value stack fits `max_call_depth` nested calls of the
/// largest allowed frames; both engines agree at the boundary.
#[test]
fn max_frames_at_max_depth_identical_on_both_engines() {
    let locals = format!("(local {})", "i64 ".repeat(2045));
    let f = format!(
        "(func $f (param $n i32) {locals}
           (if (i32.gt_u (local.get $n) (i32.const 0))
             (then (call $f (i32.sub (local.get $n) (i32.const 1))))))"
    );
    let canonical = Host::new().expect("host");
    let fast = onyx_recipe_host::fast::FastHost::new().expect("fast");
    let mut p = profile();
    p.max_call_depth = onyx_recipe_host::MAX_CALL_DEPTH;
    p.fuel_limit = 1 << 36;
    for (arg, want) in [
        (p.max_call_depth - 1, Outcome::Ok),
        (p.max_call_depth, Outcome::Trap),
    ] {
        let w = module(&format!("(call $f (i32.const {arg})) {OK_RUN}"), &f);
        let a = canonical
            .run(&keccak256(&w), &w, b"", &p)
            .expect("canonical");
        fast.allow(keccak256(&w));
        let b = fast.run(&keccak256(&w), &w, b"", &p).expect("fast");
        assert_eq!(a.outcome, want, "canonical arg {arg}");
        assert_eq!(b.outcome, want, "fast arg {arg}");
    }
}

/// L9-F2: deep block nesting and huge function counts are rejected at load.
#[test]
fn nesting_and_function_count_caps() {
    let n = onyx_recipe_host::MAX_BLOCK_NESTING as usize + 1;
    let nested = format!("(func {}{})", "(block ".repeat(n), ")".repeat(n));
    assert!(matches!(
        run(&module(OK_RUN, &nested)),
        Err(HostError::TooLarge(_))
    ));
    let ok_nested = format!("(func {}{})", "(block ".repeat(n - 1), ")".repeat(n - 1));
    assert!(run(&module(OK_RUN, &ok_nested)).is_ok());
    let many = "(func)".repeat(onyx_recipe_host::MAX_FUNCTIONS as usize);
    assert!(matches!(
        run(&module(OK_RUN, &many)),
        Err(HostError::TooLarge(_))
    ));
}

/// L9-F5: output size is capped by profile and bounds-checked before allocating.
#[test]
fn output_bounds_checked_before_allocation() {
    let mut p = profile();
    p.max_output_bytes = onyx_recipe_host::MAX_OUTPUT_BYTES + 1;
    let w = module(OK_RUN, "");
    assert!(matches!(
        Host::new().expect("host").run(&keccak256(&w), &w, b"", &p),
        Err(HostError::WrongProfile(_))
    ));
    // Claims 16 MiB of output at offset 0 in a 1-page memory: BadOutput, no huge allocation.
    p.max_output_bytes = onyx_recipe_host::MAX_OUTPUT_BYTES;
    let w = module("i64.const 16777216", "");
    let a = Host::new()
        .expect("host")
        .run(&keccak256(&w), &w, b"", &p)
        .expect("run");
    assert_eq!(a.outcome, Outcome::BadOutput);
    let b = onyx_recipe_host::fast::FastHost::new()
        .expect("fast")
        .allowing(keccak256(&w))
        .run(&keccak256(&w), &w, b"", &p)
        .expect("run");
    assert_eq!(b.outcome, Outcome::BadOutput);
}

/// Initial memory above the pinned limit is the same deterministic trap on both engines.
#[test]
fn initial_memory_above_profile_traps_on_both_engines() {
    let text = r#"(module (memory (export "memory") 300)
        (func (export "onyx_abi_version") (result i32) i32.const 1)
        (func (export "onyx_alloc") (param i32) (result i32) i32.const 1024)
        (func (export "onyx_run") (param i32 i32) (result i64) i64.const 0))"#;
    let w = wat::parse_str(text).expect("wat");
    let a = run(&w).expect("canonical");
    let b = onyx_recipe_host::fast::FastHost::new()
        .expect("fast")
        .allowing(keccak256(&w))
        .run(&keccak256(&w), &w, b"input", &profile())
        .expect("fast");
    assert_eq!(a.outcome, Outcome::Trap);
    assert_eq!(b.outcome, Outcome::Trap);
}

/// L9-F2: blocks with parameters are capped per function (fast-path compiler
/// memory grows about quadratically in them).
#[test]
fn typed_block_cap() {
    let make = |n: usize| {
        let body = "(i32.const 1) (block (type $t) drop)".repeat(n);
        let text = format!(
            r#"(module (type $t (func (param i32)))
              (memory (export "memory") 1)
              (data (i32.const 2048) "\00\01\00\00\00\01\00\00\00\01\01\00\00\00\02")
              (func (export "onyx_abi_version") (result i32) i32.const 1)
              (func (export "onyx_alloc") (param i32) (result i32) i32.const 1024)
              (func (export "onyx_run") (param i32 i32) (result i64) {OK_RUN})
              (func {body}))"#
        );
        wat::parse_str(text).expect("wat")
    };
    let cap = onyx_recipe_host::MAX_TYPED_BLOCKS_PER_FUNCTION as usize;
    assert!(run(&make(cap)).is_ok());
    assert!(matches!(run(&make(cap + 1)), Err(HostError::TooLarge(_))));
}

/// L10-F1/F2: unreviewed recipes never reach the fast path's compiler.
#[test]
fn fast_path_runs_only_allowlisted_recipes() {
    let w = module(OK_RUN, "");
    let fast = onyx_recipe_host::fast::FastHost::new().expect("fast");
    assert!(matches!(
        fast.run(&keccak256(&w), &w, b"", &profile()),
        Err(HostError::NotAllowlisted(_))
    ));
    fast.allow(keccak256(&w));
    assert_eq!(
        fast.run(&keccak256(&w), &w, b"", &profile())
            .expect("run")
            .outcome,
        Outcome::Ok
    );
}

/// L10-F1: total blocks per function are capped, whatever their type.
#[test]
fn total_block_cap() {
    let n = onyx_recipe_host::MAX_BLOCKS_PER_FUNCTION as usize;
    let ok = module(
        OK_RUN,
        &format!(
            "(func {})",
            "(block (result i32) i32.const 7) drop ".repeat(n)
        ),
    );
    assert!(run(&ok).is_ok());
    let over = module(
        OK_RUN,
        &format!(
            "(func {})",
            "(block (result i32) i32.const 7) drop ".repeat(n + 1)
        ),
    );
    assert!(matches!(run(&over), Err(HostError::TooLarge(_))));
}
