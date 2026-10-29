//! Advisory fast path on wasmtime (Cranelift, optimizer off).
//!
//! Only [`crate::Host`] (wasmi) produces verdicts. This engine exists so miners
//! and validators can pre-check quickly; any result it gives must be confirmed
//! by the canonical engine before it is committed (ADR-0014). It keeps the
//! instrumented call-depth counter and runs on a large native stack.

use crate::{
    allowed_features, canonical_accepts, check_profile, decode_output, fail, static_checks,
    HostError, ModuleCache, Outcome, RunResult, MAX_CALL_DEPTH, MAX_FRAME_SLOTS, MAX_MODULE_BYTES,
    MAX_TABLE_ELEMENTS,
};
use onyx_core::{keccak256, RecipeId, RuntimeProfile};
use onyx_recipe_sdk::{unpack_ptr_len, ABI_VERSION, EXPORT_ABI_VERSION, EXPORT_ALLOC, EXPORT_RUN};
use std::collections::HashSet;
use std::sync::Mutex;
use wasm_encoder::reencode::Reencode;
use wasm_encoder::{BlockType, ConstExpr, GlobalType, Instruction, ValType};
use wasmparser::{Operator, Parser, Validator};
use wasmtime::{Config, Engine, Instance, Module, Store, StoreLimits, StoreLimitsBuilder};

/// Conservative native bytes per frame slot (8 for the value, generous room for
/// Cranelift temporaries and spills) plus a fixed per-frame overhead. The budget
/// `MAX_CALL_DEPTH x (MAX_FRAME_SLOTS x BYTES_PER_SLOT + FRAME_OVERHEAD)` is about
/// 138 MiB, under `max_wasm_stack` (256 MiB). Cross-architecture margins are not
/// yet measured (x86_64 only); see THREATS R-HOST-STACK-1.
const BYTES_PER_SLOT: usize = 64;
const FRAME_OVERHEAD: usize = 4096;

/// Native stack for the run thread; wasmtime's `max_wasm_stack` is half of it.
const NATIVE_STACK_BYTES: usize = 512 << 20;

const _: () = assert!(
    (MAX_CALL_DEPTH as usize) * (MAX_FRAME_SLOTS as usize * BYTES_PER_SLOT + FRAME_OVERHEAD)
        <= NATIVE_STACK_BYTES / 2
);

/// Engine settings.
#[derive(Clone, Copy, Debug)]
pub struct HostOptions {
    /// Compile functions on several threads.
    pub parallel_compilation: bool,
    /// Cranelift `OptLevel::Speed` (true) or `OptLevel::None` (false).
    pub optimize: bool,
    /// Native stack wasm may use. Benchmarks shrink it to measure frame sizes.
    pub max_wasm_stack: usize,
}

impl Default for HostOptions {
    fn default() -> Self {
        HostOptions {
            parallel_compilation: true,
            // Off: Cranelift's egraph (LICM/GVN) can make native frames far larger
            // than the wasm frame (THREATS R-HOST-STACK-2); off also compiles ~100x faster.
            optimize: false,
            max_wasm_stack: NATIVE_STACK_BYTES / 2,
        }
    }
}

/// Advisory wasmtime runner (ahead-of-time compiled, fast). Its results are
/// never committed or used in disputes: only [`crate::Host`] (wasmi) gives
/// verdicts. Fuel counts differ from wasmi; outcomes and leaves of successful
/// runs must agree (tested). Compiled modules are cached (bounded LRU).
pub struct FastHost {
    engine: Engine,
    cache: Mutex<ModuleCache<Module>>,
    allowed: Mutex<HashSet<RecipeId>>,
}

impl FastHost {
    /// Build the engine (optimizer off: see `HostOptions`).
    pub fn new() -> Result<FastHost, HostError> {
        Self::with_parallel_compilation(true)
    }

    /// Same, choosing whether Cranelift compiles functions in parallel (tests
    /// use both to show output does not depend on it).
    pub fn with_parallel_compilation(parallel: bool) -> Result<FastHost, HostError> {
        Self::with_options(HostOptions {
            parallel_compilation: parallel,
            ..HostOptions::default()
        })
    }

    /// Full control over engine settings. Benchmarks and tests use this to
    /// compare settings; production uses `Host::new()`.
    pub fn with_options(o: HostOptions) -> Result<FastHost, HostError> {
        let mut c = Config::new();
        c.cranelift_opt_level(if o.optimize {
            wasmtime::OptLevel::Speed
        } else {
            wasmtime::OptLevel::None
        });
        c.consume_fuel(true)
            .cranelift_nan_canonicalization(true)
            .wasm_simd(false)
            .wasm_relaxed_simd(false)
            // Threads: the `threads` cargo feature is not compiled in, and the
            // validator allowlist rejects shared memory and atomics.
            .wasm_multi_memory(false)
            .wasm_memory64(false)
            .wasm_tail_call(false)
            .max_wasm_stack(o.max_wasm_stack)
            .async_stack_size(NATIVE_STACK_BYTES / 2)
            .parallel_compilation(o.parallel_compilation);
        let engine = Engine::new(&c).map_err(|e| HostError::Internal(e.to_string()))?;
        Ok(FastHost {
            engine,
            cache: Mutex::new(ModuleCache::default()),
            allowed: Mutex::new(HashSet::new()),
        })
    }

    /// Allow this recipe id on the fast path. Only reviewed, registered recipes
    /// belong here: Cranelift's compile cost has super-linear shapes that load
    /// caps alone have not closed (L10-F1, L10-F2), so unreviewed code never
    /// reaches it. Everything runs on the canonical engine regardless.
    pub fn allow(&self, id: RecipeId) {
        if let Ok(mut a) = self.allowed.lock() {
            a.insert(id);
        }
    }

    /// Builder form of [`FastHost::allow`].
    pub fn allowing(self, id: RecipeId) -> Self {
        self.allow(id);
        self
    }

    fn check_allowed(&self, id: &RecipeId) -> Result<(), HostError> {
        let ok = self
            .allowed
            .lock()
            .map_err(|_| HostError::Internal("allowlist poisoned".into()))?
            .contains(id);
        if ok {
            Ok(())
        } else {
            Err(HostError::NotAllowlisted(*id))
        }
    }

    /// Run load-time checks and return the instrumented module bytes.
    pub fn prepare(
        &self,
        id: &RecipeId,
        wasm: &[u8],
        profile: &RuntimeProfile,
    ) -> Result<Vec<u8>, HostError> {
        self.check_allowed(id)?;
        if keccak256(wasm) != *id {
            return Err(HostError::IdMismatch(*id));
        }
        check_profile(profile)?;
        if wasm.len() > MAX_MODULE_BYTES {
            return Err(HostError::TooLarge(format!("module {} bytes", wasm.len())));
        }
        let globals = static_checks(wasm)?;
        // Advisory engine accepts exactly what the canonical engine accepts (L8-VALID-4).
        canonical_accepts(wasm)?;
        instrument(wasm, globals, profile.max_call_depth)
    }

    /// Number of compiled modules currently cached.
    pub fn cached_modules(&self) -> usize {
        self.cache.lock().map(|c| c.len()).unwrap_or(0)
    }

    /// Validate, instrument and run `wasm` on `input` (Borsh of `RecipeInput`).
    pub fn run(
        &self,
        id: &RecipeId,
        wasm: &[u8],
        input: &[u8],
        profile: &RuntimeProfile,
    ) -> Result<RunResult, HostError> {
        self.check_allowed(id)?;
        let key = (*id, profile.max_call_depth);
        let cached = self
            .cache
            .lock()
            .map_err(|_| HostError::Internal("cache poisoned".into()))?
            .get(&key);
        let module = match cached {
            Some(m) => {
                // Bytes must still hash to the id; the profile gets the same check as cold.
                if keccak256(wasm) != *id {
                    return Err(HostError::IdMismatch(*id));
                }
                check_profile(profile)?;
                m
            }
            None => {
                let instrumented = self.prepare(id, wasm, profile)?;
                let m = Module::new(&self.engine, &instrumented)
                    .map_err(|e| HostError::Internal(e.to_string()))?;
                self.cache
                    .lock()
                    .map_err(|_| HostError::Internal("cache poisoned".into()))?
                    .insert(key, m.clone(), wasm.len());
                m
            }
        };
        if crate::initial_memory_bytes(wasm) > profile.max_memory_bytes {
            return fail(Outcome::Trap, 0);
        }
        let engine = self.engine.clone();
        let input = input.to_vec();
        let profile = profile.clone();
        std::thread::Builder::new()
            .stack_size(NATIVE_STACK_BYTES)
            .spawn(move || execute(&engine, &module, &input, &profile))
            .map_err(|e| HostError::Abstain(format!("cannot start run thread: {e}")))?
            .join()
            .map_err(|_| HostError::Internal("run thread panicked".into()))?
    }
}

/// Re-encoder that appends a private depth-counter global and wraps every call.
struct DepthInstrumenter {
    depth_global: u32,
    max_depth: i32,
    global_emitted: bool,
}

impl DepthInstrumenter {
    fn depth_global_type() -> GlobalType {
        GlobalType {
            val_type: ValType::I32,
            mutable: true,
            shared: false,
        }
    }
}

impl Reencode for DepthInstrumenter {
    type Error = std::convert::Infallible;

    fn parse_global_section(
        &mut self,
        globals: &mut wasm_encoder::GlobalSection,
        section: wasmparser::GlobalSectionReader<'_>,
    ) -> Result<(), wasm_encoder::reencode::Error<Self::Error>> {
        wasm_encoder::reencode::utils::parse_global_section(self, globals, section)?;
        globals.global(Self::depth_global_type(), &ConstExpr::i32_const(0));
        self.global_emitted = true;
        Ok(())
    }

    fn intersperse_section_hook(
        &mut self,
        module: &mut wasm_encoder::Module,
        _after: Option<wasm_encoder::SectionId>,
        before: Option<wasm_encoder::SectionId>,
    ) -> Result<(), wasm_encoder::reencode::Error<Self::Error>> {
        use wasm_encoder::SectionId as S;
        let past_global_slot = matches!(
            before,
            None | Some(S::Export | S::Start | S::Element | S::DataCount | S::Code | S::Data)
        );
        if !self.global_emitted && past_global_slot {
            let mut g = wasm_encoder::GlobalSection::new();
            g.global(Self::depth_global_type(), &ConstExpr::i32_const(0));
            module.section(&g);
            self.global_emitted = true;
        }
        Ok(())
    }

    fn parse_function_body(
        &mut self,
        code: &mut wasm_encoder::CodeSection,
        func: wasmparser::FunctionBody<'_>,
    ) -> Result<(), wasm_encoder::reencode::Error<Self::Error>> {
        let mut f = self.new_function_with_parsed_locals(&func)?;
        let mut reader = func.get_operators_reader()?;
        let g = self.depth_global;
        while !reader.eof() {
            let op = reader.read()?;
            let is_call = matches!(op, Operator::Call { .. } | Operator::CallIndirect { .. });
            if is_call {
                f.instruction(&Instruction::GlobalGet(g))
                    .instruction(&Instruction::I32Const(1))
                    .instruction(&Instruction::I32Add)
                    .instruction(&Instruction::GlobalSet(g))
                    .instruction(&Instruction::GlobalGet(g))
                    .instruction(&Instruction::I32Const(self.max_depth))
                    .instruction(&Instruction::I32GtU)
                    .instruction(&Instruction::If(BlockType::Empty))
                    .instruction(&Instruction::Unreachable)
                    .instruction(&Instruction::End);
            }
            f.instruction(&self.instruction(op)?);
            if is_call {
                f.instruction(&Instruction::GlobalGet(g))
                    .instruction(&Instruction::I32Const(1))
                    .instruction(&Instruction::I32Sub)
                    .instruction(&Instruction::GlobalSet(g));
            }
        }
        code.function(&f);
        Ok(())
    }
}

fn instrument(wasm: &[u8], globals: u32, max_depth: u32) -> Result<Vec<u8>, HostError> {
    let mut r = DepthInstrumenter {
        depth_global: globals,
        max_depth: i32::try_from(max_depth).map_err(|e| HostError::Internal(e.to_string()))?,
        global_emitted: false,
    };
    let mut module = wasm_encoder::Module::new();
    r.parse_core_module(&mut module, Parser::new(0), wasm)
        .map_err(|e| HostError::Internal(format!("instrument: {e:?}")))?;
    let out = module.finish();
    // The instrumented module must still pass the same validator.
    Validator::new_with_features(allowed_features())
        .validate_all(&out)
        .map_err(|e| HostError::Internal(format!("instrumented module invalid: {e}")))?;
    Ok(out)
}

/// Store limits plus a flag: wasmtime calls `*_grow_failed` only after the
/// limits allowed a growth that the machine then could not satisfy (L8-OOM-1).
struct AbstainingLimits {
    limits: StoreLimits,
    machine_oom: bool,
}

impl wasmtime::ResourceLimiter for AbstainingLimits {
    fn memory_growing(
        &mut self,
        current: usize,
        desired: usize,
        maximum: Option<usize>,
    ) -> wasmtime::Result<bool> {
        self.limits.memory_growing(current, desired, maximum)
    }

    fn memory_grow_failed(&mut self, _error: wasmtime::Error) -> wasmtime::Result<()> {
        self.machine_oom = true;
        Ok(())
    }

    fn table_growing(
        &mut self,
        current: usize,
        desired: usize,
        maximum: Option<usize>,
    ) -> wasmtime::Result<bool> {
        self.limits.table_growing(current, desired, maximum)
    }

    fn table_grow_failed(&mut self, _error: wasmtime::Error) -> wasmtime::Result<()> {
        self.machine_oom = true;
        Ok(())
    }

    fn instances(&self) -> usize {
        self.limits.instances()
    }

    fn tables(&self) -> usize {
        self.limits.tables()
    }

    fn memories(&self) -> usize {
        self.limits.memories()
    }
}

fn execute(
    engine: &Engine,
    module: &Module,
    input: &[u8],
    profile: &RuntimeProfile,
) -> Result<RunResult, HostError> {
    let limits = StoreLimitsBuilder::new()
        .memory_size(usize::try_from(profile.max_memory_bytes).unwrap_or(usize::MAX))
        .table_elements(MAX_TABLE_ELEMENTS as usize)
        .instances(1)
        .tables(1)
        .memories(1)
        .build();
    let mut store: Store<AbstainingLimits> = Store::new(
        engine,
        AbstainingLimits {
            limits,
            machine_oom: false,
        },
    );
    store.limiter(|l| l);
    store
        .set_fuel(profile.fuel_limit)
        .map_err(|e| HostError::Internal(e.to_string()))?;
    let used = |s: &Store<AbstainingLimits>| profile.fuel_limit - s.get_fuel().unwrap_or(0);
    // Only real wasm traps are verdicts. Any other error (for example failing to
    // reserve virtual memory at instantiation) depends on the machine: abstain (L9-F3).
    let verdict = |s: &Store<AbstainingLimits>, e: &wasmtime::Error| {
        if s.data().machine_oom {
            return Err(HostError::Abstain(format!("out of system memory: {e}")));
        }
        match e.downcast_ref::<wasmtime::Trap>() {
            Some(wasmtime::Trap::OutOfFuel) => fail(Outcome::OutOfFuel, used(s)),
            Some(_) => fail(Outcome::Trap, used(s)),
            None => Err(HostError::Abstain(format!("engine error, not a trap: {e}"))),
        }
    };

    let instance = match Instance::new(&mut store, module, &[]) {
        Ok(i) => i,
        Err(e) => return verdict(&store, &e),
    };
    let abi = instance
        .get_typed_func::<(), u32>(&mut store, EXPORT_ABI_VERSION)
        .map_err(|e| HostError::BadAbi(e.to_string()))?;
    let alloc = instance
        .get_typed_func::<u32, u32>(&mut store, EXPORT_ALLOC)
        .map_err(|e| HostError::BadAbi(e.to_string()))?;
    let run = instance
        .get_typed_func::<(u32, u32), u64>(&mut store, EXPORT_RUN)
        .map_err(|e| HostError::BadAbi(e.to_string()))?;
    let memory = instance
        .get_memory(&mut store, "memory")
        .ok_or_else(|| HostError::BadAbi("no memory export".into()))?;

    match abi.call(&mut store, ()) {
        Ok(v) if v == ABI_VERSION => {}
        Ok(v) => return Err(HostError::BadAbi(format!("abi version {v}"))),
        Err(e) => return verdict(&store, &e),
    }
    let len =
        u32::try_from(input.len()).map_err(|_| HostError::Internal("input too big".into()))?;
    let ptr = match alloc.call(&mut store, len) {
        Ok(p) => p,
        Err(e) => return verdict(&store, &e),
    };
    if memory.write(&mut store, ptr as usize, input).is_err() {
        return fail(Outcome::Trap, used(&store));
    }
    let packed = match run.call(&mut store, (ptr, len)) {
        Ok(v) => v,
        Err(e) => return verdict(&store, &e),
    };
    if store.data().machine_oom {
        return Err(HostError::Abstain("out of system memory during run".into()));
    }
    let fuel_used = used(&store);
    let (optr, olen) = unpack_ptr_len(packed);
    if olen > profile.max_output_bytes {
        return fail(Outcome::OutputTooLarge, fuel_used);
    }
    if u64::from(optr) + u64::from(olen) > memory.data_size(&store) as u64 {
        return fail(Outcome::BadOutput, fuel_used);
    }
    let mut out = vec![0u8; olen as usize];
    if memory.read(&store, optr as usize, &mut out).is_err() {
        return fail(Outcome::BadOutput, fuel_used);
    }
    decode_output(&out, profile, fuel_used)
}
