use std::collections::{BTreeMap, VecDeque};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, Weak};

type AffValue = crate::UnknownType;
type AffResult = Result<AffValue, AffValue>;
type AffCallback = Arc<dyn Fn(AffResult) + Send + Sync>;
type AffCancel = Arc<dyn Fn(AffValue) -> AffNodeRef + Send + Sync>;
type AffNodeRef = Arc<AffCell>;

// Aff is a reusable description. Only a fiber owns a mutable execution stack.
enum AffNode {
    Pure(AffValue),
    Throw(AffValue),
    Bind(AffNodeRef, AffValue),
    Map(AffNodeRef, AffValue),
    Catch(AffNodeRef, AffValue),
    Sync(AffValue),
    Async(AffValue),
    NativeAsync {
        yield_after_registration: bool,
        build: Arc<dyn Fn(AffCallback) -> AffCancel + Send + Sync>,
    },
    Bracket(AffNodeRef, AffValue, AffValue),
    Fork(bool, AffNodeRef),
    Sequential(AffNodeRef),
    ParMap(AffValue, AffNodeRef),
    ParApply(AffNodeRef, AffNodeRef),
    ParAlt(AffNodeRef, AffNodeRef),
}

// Releasing a deep bind/applicative description must be stack-safe too.
struct AffCell {
    node: AffNode,
}

fn aff_new(node: AffNode) -> AffNodeRef {
    Arc::new(AffCell { node })
}

fn aff_drop_links(node: AffNode, pending: &mut Vec<AffNodeRef>) {
    match node {
        AffNode::Bind(child, _)
        | AffNode::Map(child, _)
        | AffNode::Catch(child, _)
        | AffNode::Bracket(child, _, _)
        | AffNode::Fork(_, child)
        | AffNode::Sequential(child)
        | AffNode::ParMap(_, child) => pending.push(child),
        AffNode::ParApply(left, right) | AffNode::ParAlt(left, right) => {
            pending.push(left);
            pending.push(right);
        }
        _ => {}
    }
}

impl Drop for AffCell {
    fn drop(&mut self) {
        let mut pending = Vec::new();
        aff_drop_links(
            std::mem::replace(&mut self.node, AffNode::Pure(crate::Value::Unit)),
            &mut pending,
        );
        while let Some(child) = pending.pop() {
            if let Some(mut child) = Arc::into_inner(child) {
                aff_drop_links(
                    std::mem::replace(&mut child.node, AffNode::Pure(crate::Value::Unit)),
                    &mut pending,
                );
            }
        }
    }
}

struct AffBox(AffNodeRef);

fn aff_box(node: AffNode) -> AffValue {
    crate::Value::Class(Arc::new(AffBox(aff_new(node))))
}

fn aff_node(value: &AffValue) -> AffNodeRef {
    value.unwrap_class::<AffBox>().0.clone()
}

fn aff_fn(f: impl Fn(AffValue) -> AffValue + Send + Sync + 'static) -> AffValue {
    crate::Value::Func1(purust_core::Func1::Shared(Arc::new(f)))
}

fn aff_effect(f: impl Fn() -> AffValue + Send + Sync + 'static) -> AffValue {
    aff_fn(move |_| f())
}

fn aff_call(f: &AffValue, value: AffValue) -> AffValue {
    f.unwrap_func1()(value)
}
fn aff_run_effect(effect: &AffValue) -> AffValue {
    aff_call(effect, crate::Value::Unit)
}
fn aff_try(f: impl FnOnce() -> AffValue) -> AffResult {
    Purs_Effect_Exception::purust_exception_try(f)
}
fn aff_unit() -> AffNodeRef {
    aff_new(AffNode::Pure(crate::Value::Unit))
}
fn aff_no_cancel() -> AffCancel {
    Arc::new(|_| aff_unit())
}

#[derive(Clone)]
struct AffUtil {
    is_left: AffValue,
    from_left: AffValue,
    from_right: AffValue,
    left: AffValue,
    right: AffValue,
}

impl AffUtil {
    fn new(value: AffValue) -> Self {
        Self {
            is_left: value.get_isLeft(),
            from_left: value.get_fromLeft(),
            from_right: value.get_fromRight(),
            left: value.get_left(),
            right: value.get_right(),
        }
    }
    fn encode(&self, result: AffResult) -> AffValue {
        match result {
            Ok(value) => aff_call(&self.right, value),
            Err(error) => aff_call(&self.left, error),
        }
    }
    fn decode(&self, value: AffValue) -> AffResult {
        if aff_call(&self.is_left, value.clone()).unwrap_bool() {
            Err(aff_call(&self.from_left, value))
        } else {
            Ok(aff_call(&self.from_right, value))
        }
    }
}

struct AffRuntime {
    pool: Arc<AffPool>,
    microtasks: Arc<purust_core::microtasks::Queue>,
    handle: tokio::runtime::Handle,
    next_id: AtomicUsize,
    active: AtomicUsize,
    fibers: Mutex<BTreeMap<usize, Arc<AffFiber>>>,
    changed: Arc<tokio::sync::Notify>,
    errors: Mutex<BTreeMap<usize, AffValue>>,
    timers: Mutex<BTreeMap<(tokio::time::Instant, usize), AffCallback>>,
    timer_changed: tokio::sync::Notify,
    panic: Mutex<Option<Box<dyn std::any::Any + Send>>>,
}

// Fiber startup no longer runs on the submitting fiber's stack. This bounded
// pool executes each fiber until its first suspension; Tokio keeps handling
// waits, timers and resumptions. A dedicated pool is deliberate: CPU work must
// be limited to a measurable number of workers, unlike the (up to 512-thread)
// Tokio blocking pool that is also used for resumptions.
struct AffPoolQueue {
    jobs: VecDeque<Box<dyn FnOnce() + Send + 'static>>,
    stop: bool,
}

struct AffPoolShared {
    queue: Mutex<AffPoolQueue>,
    ready: Condvar,
    handle: tokio::runtime::Handle,
}

struct AffPool {
    shared: Arc<AffPoolShared>,
    workers: Mutex<Vec<std::thread::JoinHandle<()>>>,
}

impl AffPool {
    fn new(workers: usize, handle: tokio::runtime::Handle) -> Arc<Self> {
        let shared = Arc::new(AffPoolShared {
            queue: Mutex::new(AffPoolQueue {
                jobs: VecDeque::new(),
                stop: false,
            }),
            ready: Condvar::new(),
            handle,
        });
        let mut handles = Vec::with_capacity(workers);
        for index in 0..workers {
            let worker = shared.clone();
            let handle = std::thread::Builder::new()
                .name(format!("purust-aff-{index}"))
                .spawn(move || AffPool::work(worker))
                .expect("failed to spawn an Aff worker thread");
            handles.push(handle);
        }
        Arc::new(Self {
            shared,
            workers: Mutex::new(handles),
        })
    }
    fn work(shared: Arc<AffPoolShared>) {
        loop {
            let job = {
                let mut queue = shared.queue.lock().unwrap();
                loop {
                    if queue.stop {
                        break None;
                    }
                    if let Some(job) = queue.jobs.pop_front() {
                        break Some(job);
                    }
                    queue = shared.ready.wait(queue).unwrap();
                }
            };
            match job {
                Some(job) => {
                    // Entering the handle keeps `tokio::spawn` working from FFI.
                    // `block_in_place` still treats this thread as outside the
                    // runtime and simply blocks it.
                    let _entered = shared.handle.enter();
                    job();
                }
                None => return,
            }
        }
    }
    fn submit(&self, job: impl FnOnce() + Send + 'static) {
        self.shared.queue.lock().unwrap().jobs.push_back(Box::new(job));
        self.shared.ready.notify_one();
    }
}

impl Drop for AffPool {
    fn drop(&mut self) {
        {
            // An orderly shutdown leaves the queue empty: `active` keeps the
            // entry point alive until every submitted fiber finished. Anything
            // left (a Rust panic aborted the wait) must not run after the
            // runtime is gone.
            let mut queue = self.shared.queue.lock().unwrap();
            queue.stop = true;
            queue.jobs.clear();
        }
        self.shared.ready.notify_all();
        if let Ok(mut workers) = self.workers.lock() {
            for worker in workers.drain(..) {
                let _ = worker.join();
            }
        }
    }
}

fn aff_default_workers() -> usize {
    std::env::var("PURUST_AFF_WORKERS")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|workers| *workers > 0)
        .unwrap_or_else(|| {
            std::thread::available_parallelism()
                .map(|workers| workers.get().max(2))
                .unwrap_or(4)
        })
}

thread_local! {
    static AFF_RUNTIME: std::cell::RefCell<Option<Arc<AffRuntime>>> = const { std::cell::RefCell::new(None) };
}

struct AffRuntimeScope(Option<Arc<AffRuntime>>);
impl AffRuntimeScope {
    fn enter(runtime: Arc<AffRuntime>) -> Self {
        Self(AFF_RUNTIME.with(|slot| slot.replace(Some(runtime))))
    }
}
impl Drop for AffRuntimeScope {
    fn drop(&mut self) {
        AFF_RUNTIME.with(|slot| {
            slot.replace(self.0.take());
        });
    }
}
fn aff_runtime() -> Arc<AffRuntime> {
    AFF_RUNTIME
        .with(|slot| slot.borrow().clone())
        .expect("Aff requires purust_aff_run_main")
}

fn aff_record_panic(runtime: &AffRuntime, panic: Box<dyn std::any::Any + Send>) {
    let mut stored = runtime.panic.lock().unwrap();
    if stored.is_none() {
        *stored = Some(panic);
    }
    drop(stored);
    runtime.changed.notify_one();
}

// Register native IO before spawning it. Even an abandoned Promise must finish
// its operation and cleanup before the Aff entry point can exit.
pub fn purust_aff_spawn_native<F, C>(future: F, complete: C)
where
    F: std::future::Future<Output = Result<AffValue, AffValue>> + Send,
    F: 'static,
    C: FnOnce(Result<AffValue, AffValue>) + Send + Sync + 'static,
{
    let runtime = aff_runtime();
    runtime.active.fetch_add(1, Ordering::AcqRel);
    let task = runtime.handle.spawn(future);
    let executor = runtime.handle.clone();
    executor.spawn(async move {
        let result = match task.await {
            Ok(result) => result,
            Err(error) => {
                if error.is_panic() { aff_record_panic(&runtime, error.into_panic()); }
                Err(Purs_Effect_Exception::Effect_Exception_error("A native IO task terminated unexpectedly".to_owned()))
            }
        };
        let queued = runtime.clone();
        runtime.microtasks.enqueue(move || {
            struct Completed(Arc<AffRuntime>);
            impl Drop for Completed {
                fn drop(&mut self) {
                    self.0.active.fetch_sub(1, Ordering::AcqRel);
                    self.0.changed.notify_one();
                }
            }
            let _completed = Completed(queued.clone());
            let _scope = AffRuntimeScope::enter(queued);
            complete(result);
        });
    });
}

pub fn purust_aff_run_main(main: impl FnOnce() -> AffValue) {
    aff_run_main_with_workers(aff_default_workers(), main)
}

fn aff_run_main_with_workers(workers: usize, main: impl FnOnce() -> AffValue) {
    let executor = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    let changed = Arc::new(tokio::sync::Notify::new());
    let wake = changed.clone();
    let runtime = Arc::new(AffRuntime {
        pool: AffPool::new(workers.max(1), executor.handle().clone()),
        microtasks: purust_core::microtasks::Queue::new(move || wake.notify_one()),
        handle: executor.handle().clone(),
        next_id: AtomicUsize::new(1),
        active: AtomicUsize::new(0),
        fibers: Mutex::new(BTreeMap::new()),
        changed,
        errors: Mutex::new(BTreeMap::new()),
        timers: Mutex::new(BTreeMap::new()),
        timer_changed: tokio::sync::Notify::new(),
        panic: Mutex::new(None),
    });
    runtime.handle.spawn(aff_timer_loop(runtime.clone()));
    let result = executor.block_on(async {
        let _scope = AffRuntimeScope::enter(runtime.clone());
        let main_result =
            match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| runtime.microtasks.turn(|| aff_try(main)))) {
                Ok(result) => result,
                Err(panic) => {
                    aff_record_panic(&runtime, panic);
                    Ok(crate::Value::Unit)
                }
            };
        // A Rust panic is fatal even while unrelated fibers or native IO remain
        // active. Ordinary Aff errors still wait for their normal cleanup below.
        while runtime.panic.lock().unwrap().is_none() {
            let changed = runtime.changed.notified();
            match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| aff_try(|| {
                runtime.microtasks.drain();
                crate::Value::Unit
            }))) {
                Ok(Err(error)) => { runtime.errors.lock().unwrap().insert(0, error); }
                Err(panic) => aff_record_panic(&runtime, panic),
                _ => {},
            }
            if runtime.panic.lock().unwrap().is_some()
                || (runtime.active.load(Ordering::Acquire) == 0 && !runtime.microtasks.has_jobs())
            {
                break;
            }
            changed.await;
        }
        main_result.and_then(
            |_| match runtime.errors.lock().unwrap().values().next().cloned() {
                Some(error) => Err(error),
                None => Ok(crate::Value::Unit),
            },
        )
    });
    if let Some(panic) = runtime.panic.lock().unwrap().take() {
        std::panic::resume_unwind(panic);
    }
    if let Err(error) = result {
        // Report only at the outer boundary, after all fibers have finished.
        // Handled exceptions stay silent; a failed write must preserve the error.
        use std::io::Write;
        let message = Purs_Effect_Exception::Effect_Exception_showErrorImpl(error.clone());
        let _ = writeln!(
            std::io::stderr().lock(),
            "{}",
            purust_core::purust_string_to_utf8_lossy(&message)
        );
        Purs_Effect_Exception::purust_exception_raise(error);
    }
}

enum AffFrame {
    Bind(AffValue, usize),
    Map(AffValue, usize),
    Catch(AffValue, usize),
    Acquire {
        options: AffValue,
        use_resource: AffValue,
        interrupt: usize,
    },
    Release {
        options: AffValue,
        resource: AffValue,
        interrupt: usize,
    },
    Finalized(AffResult),
}

enum AffStep {
    Node(AffNodeRef),
    Result(AffResult),
    Pending { tick: usize, cancel: AffCancel },
}
struct AffMachine {
    step: AffStep,
    frames: Vec<AffFrame>,
    mask: usize,
    interrupt: Option<AffValue>,
    tick: usize,
}
enum AffEvent {
    Resolve(usize, AffResult),
    Kill(AffValue),
}
struct AffJoin {
    rethrow: bool,
    callback: AffCallback,
}
struct AffFiberState {
    machine: Option<AffMachine>,
    running: bool,
    started: bool,
    events: VecDeque<AffEvent>,
    result: Option<AffResult>,
    joins: BTreeMap<usize, AffJoin>,
    next_join: usize,
    rethrow: bool,
}
struct AffFiber {
    id: usize,
    runtime: Arc<AffRuntime>,
    util: AffUtil,
    supervisor: Option<Arc<AffSupervisor>>,
    state: Mutex<AffFiberState>,
}
struct AffSupervisor {
    fibers: Mutex<BTreeMap<usize, Weak<AffFiber>>>,
}

impl AffFiber {
    fn new(
        runtime: Arc<AffRuntime>,
        util: AffUtil,
        supervisor: Option<Arc<AffSupervisor>>,
        node: AffNodeRef,
    ) -> Arc<Self> {
        Arc::new(Self {
            id: runtime.next_id.fetch_add(1, Ordering::Relaxed),
            runtime,
            util,
            supervisor,
            state: Mutex::new(AffFiberState {
                machine: Some(AffMachine {
                    step: AffStep::Node(node),
                    frames: Vec::new(),
                    mask: 0,
                    interrupt: None,
                    tick: 0,
                }),
                running: false,
                started: false,
                events: VecDeque::new(),
                result: None,
                joins: BTreeMap::new(),
                next_join: 0,
                rethrow: true,
            }),
        })
    }
    fn register(self: &Arc<Self>) {
        if let Some(supervisor) = &self.supervisor {
            supervisor
                .fibers
                .lock()
                .unwrap()
                .insert(self.id, Arc::downgrade(self));
        }
    }
    // Claim the fiber exactly once. Pool submission and inline `event` drives
    // must agree on one transition, and `active` counts the fiber from the moment
    // it becomes startable so the entry point cannot exit early.
    fn begin(self: &Arc<Self>) -> bool {
        let mut state = self.state.lock().unwrap();
        if state.started || state.result.is_some() {
            return false;
        }
        state.started = true;
        self.runtime.active.fetch_add(1, Ordering::AcqRel);
        self.runtime
            .fibers
            .lock()
            .unwrap()
            .insert(self.id, self.clone());
        true
    }
    // Starting is submitting. Every explicit start site (`forkAff`, `joinFiber`,
    // `launchAff` and parallel leaves) runs its first instructions on the pool,
    // so the caller and the new fiber can interleave freely.
    fn start(self: &Arc<Self>) {
        if !self.begin() {
            return;
        }
        let runtime = self.runtime.clone();
        let panic_runtime = runtime.clone();
        let fiber = self.clone();
        runtime.pool.submit(move || {
            if let Err(panic) =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| fiber.drive()))
            {
                aff_record_panic(&panic_runtime, panic);
            }
        });
    }
    fn event(self: &Arc<Self>, event: AffEvent) {
        {
            let mut state = self.state.lock().unwrap();
            if state.result.is_some() {
                return;
            }
            // Starting a suspended cancellation and a concurrent join must agree
            // on one transition. The queued Kill runs before its first instruction.
            if !state.started && matches!(event, AffEvent::Kill(_)) {
                state.started = true;
                self.runtime.active.fetch_add(1, Ordering::AcqRel);
                self.runtime
                    .fibers
                    .lock()
                    .unwrap()
                    .insert(self.id, self.clone());
            }
            state.events.push_back(event);
        }
        self.drive();
    }
    fn observe(self: &Arc<Self>, rethrow: bool, callback: AffCallback) -> AffValue {
        let _scope = AffRuntimeScope::enter(self.runtime.clone());
        let immediate;
        let id;
        {
            let mut state = self.state.lock().unwrap();
            id = state.next_join;
            state.next_join += 1;
            immediate = state.result.clone();
            if immediate.is_some() {
                state.rethrow &= rethrow;
            } else {
                state.joins.insert(
                    id,
                    AffJoin {
                        rethrow,
                        callback: callback.clone(),
                    },
                );
            }
        }
        if let Some(result) = immediate {
            if !rethrow {
                self.runtime.errors.lock().unwrap().remove(&self.id);
            }
            callback(result);
        }
        let weak = Arc::downgrade(self);
        aff_effect(move || {
            if let Some(fiber) = weak.upgrade() {
                fiber.state.lock().unwrap().joins.remove(&id);
            }
            crate::Value::Unit
        })
    }
    fn kill(self: &Arc<Self>, error: AffValue, callback: AffCallback) -> AffValue {
        let remover = self.observe(false, Arc::new(move |_| callback(Ok(crate::Value::Unit))));
        self.event(AffEvent::Kill(error));
        remover
    }
    fn complete(self: &Arc<Self>, result: AffResult) {
        let (joins, active);
        {
            let mut state = self.state.lock().unwrap();
            if state.result.is_some() {
                return;
            }
            state.rethrow &= state.joins.values().all(|join| join.rethrow);
            if state.rethrow {
                if let Err(error) = &result {
                    self.runtime
                        .errors
                        .lock()
                        .unwrap()
                        .insert(self.id, error.clone());
                }
            }
            state.result = Some(result.clone());
            state.machine = None;
            state.running = false;
            state.events.clear();
            joins = std::mem::take(&mut state.joins);
            active = state.started;
        }
        if let Some(supervisor) = &self.supervisor {
            supervisor.fibers.lock().unwrap().remove(&self.id);
        }
        for (_, join) in joins {
            match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                aff_try(|| {
                    (join.callback)(result.clone());
                    crate::Value::Unit
                })
            })) {
                Ok(Ok(_)) => {}
                Ok(Err(error)) => {
                    self.runtime
                        .errors
                        .lock()
                        .unwrap()
                        .insert(self.runtime.next_id.fetch_add(1, Ordering::Relaxed), error);
                }
                Err(panic) => aff_record_panic(&self.runtime, panic),
            }
        }
        if active {
            self.runtime.fibers.lock().unwrap().remove(&self.id);
            self.runtime.active.fetch_sub(1, Ordering::AcqRel);
            self.runtime.changed.notify_one();
        }
    }
    fn drive(self: &Arc<Self>) {
        if let Err(panic) =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.runtime.microtasks.turn(|| self.drive_inner())))
        {
            aff_record_panic(&self.runtime, panic);
            let _scope = AffRuntimeScope::enter(self.runtime.clone());
            // Joiners must finish too. The original Rust panic remains fatal at
            // the entry point even if a joiner catches this terminal notification.
            self.complete(Err(Purs_Effect_Exception::Effect_Exception_error(
                "[Aff] A Rust panic terminated this fiber".to_owned(),
            )));
        }
    }
    fn drive_inner(self: &Arc<Self>) {
        let mut machine = {
            let mut state = self.state.lock().unwrap();
            if state.running || !state.started || state.result.is_some() {
                return;
            }
            state.running = true;
            state.machine.take().unwrap()
        };
        let _scope = AffRuntimeScope::enter(self.runtime.clone());
        loop {
            let events = { std::mem::take(&mut self.state.lock().unwrap().events) };
            for event in events {
                match event {
                    AffEvent::Resolve(tick, result) => {
                        if matches!(machine.step, AffStep::Pending { tick: current, .. } if current == tick)
                        {
                            machine.tick += 1;
                            machine.step = AffStep::Result(result);
                        }
                    }
                    AffEvent::Kill(error) => {
                        if machine.interrupt.is_none() {
                            machine.interrupt = Some(error.clone());
                        }
                        if machine.mask == 0 {
                            let previous = std::mem::replace(
                                &mut machine.step,
                                AffStep::Result(Ok(crate::Value::Unit)),
                            );
                            machine.tick += 1;
                            if let AffStep::Pending { cancel, .. } = previous {
                                machine
                                    .frames
                                    .push(AffFrame::Finalized(Ok(crate::Value::Unit)));
                                machine.mask += 1;
                                machine.step = match aff_try(|| {
                                    crate::Value::Class(Arc::new(AffBox(cancel(error))))
                                }) {
                                    Ok(value) => AffStep::Node(aff_node(&value)),
                                    Err(error) => AffStep::Result(Err(error)),
                                };
                            }
                        }
                    }
                }
            }
            if matches!(machine.step, AffStep::Pending { .. }) {
                let mut state = self.state.lock().unwrap();
                if !state.events.is_empty() {
                    continue;
                }
                state.machine = Some(machine);
                state.running = false;
                return;
            }
            let step =
                std::mem::replace(&mut machine.step, AffStep::Result(Ok(crate::Value::Unit)));
            match step {
                AffStep::Node(node) => {
                    if self.instruction(&mut machine, node) {
                        // Even a zero-delay callback can arrive during registration.
                        // Publish the suspension before consuming that event, so
                        // the continuation cannot run on the launching fiber's stack.
                        let ready = {
                            let mut state = self.state.lock().unwrap();
                            state.machine = Some(machine);
                            state.running = false;
                            !state.events.is_empty()
                        };
                        if ready {
                            let fiber = self.clone();
                            self.runtime.handle.spawn_blocking(move || fiber.drive());
                        }
                        return;
                    }
                }
                AffStep::Result(result) => {
                    if let Some(frame) = machine.frames.pop() {
                        self.unwind(&mut machine, frame, result);
                    } else {
                        self.complete(match machine.interrupt {
                            Some(error) => Err(error),
                            None => result,
                        });
                        return;
                    }
                }
                AffStep::Pending { .. } => unreachable!(),
            }
        }
    }
    fn instruction(self: &Arc<Self>, machine: &mut AffMachine, node: AffNodeRef) -> bool {
        let interrupt = usize::from(machine.interrupt.is_some());
        match &node.node {
            AffNode::Pure(value) => machine.step = AffStep::Result(Ok(value.clone())),
            AffNode::Throw(error) => machine.step = AffStep::Result(Err(error.clone())),
            AffNode::Bind(next, f) => {
                machine.frames.push(AffFrame::Bind(f.clone(), interrupt));
                machine.step = AffStep::Node(next.clone());
            }
            AffNode::Map(next, f) => {
                machine.frames.push(AffFrame::Map(f.clone(), interrupt));
                machine.step = AffStep::Node(next.clone());
            }
            AffNode::Catch(next, f) => {
                machine.frames.push(AffFrame::Catch(f.clone(), interrupt));
                machine.step = AffStep::Node(next.clone());
            }
            AffNode::Sync(effect) => {
                machine.step = AffStep::Result(aff_try(|| aff_run_effect(effect)))
            }
            AffNode::Bracket(acquire, options, use_resource) => {
                machine.frames.push(AffFrame::Acquire {
                    options: options.clone(),
                    use_resource: use_resource.clone(),
                    interrupt,
                });
                machine.mask += 1;
                machine.step = AffStep::Node(acquire.clone());
            }
            AffNode::Fork(immediate, next) => {
                let child = AffFiber::new(
                    self.runtime.clone(),
                    self.util.clone(),
                    self.supervisor.clone(),
                    next.clone(),
                );
                child.register();
                if *immediate {
                    child.start();
                }
                machine.step = AffStep::Result(Ok(aff_fiber_record(child)));
            }
            AffNode::Async(build) => {
                let tick = machine.tick;
                let fiber = self.clone();
                let util = self.util.clone();
                let callback = aff_fn(move |value| {
                    let fiber = fiber.clone();
                    let util = util.clone();
                    aff_effect(move || {
                        fiber.event(AffEvent::Resolve(tick, util.decode(value.clone())));
                        crate::Value::Unit
                    })
                });
                let built = aff_try(|| aff_run_effect(&aff_call(build, callback)));
                match built {
                    Ok(cancel) => {
                        machine.step = AffStep::Pending {
                            tick,
                            cancel: Arc::new(move |error| aff_node(&aff_call(&cancel, error))),
                        }
                    }
                    Err(error) => {
                        machine.step = AffStep::Pending {
                            tick,
                            cancel: aff_no_cancel(),
                        };
                        self.event(AffEvent::Resolve(tick, Err(error)));
                    }
                }
            }
            AffNode::NativeAsync { yield_after_registration, build } => {
                let tick = machine.tick;
                let fiber = self.clone();
                let callback: AffCallback =
                    Arc::new(move |result| fiber.event(AffEvent::Resolve(tick, result)));
                machine.step = AffStep::Pending {
                    tick,
                    cancel: build(callback),
                };
                return *yield_after_registration;
            }
            AffNode::Sequential(par) => {
                let tick = machine.tick;
                let fiber = self.clone();
                let callback: AffCallback =
                    Arc::new(move |result| fiber.event(AffEvent::Resolve(tick, result)));
                machine.step = AffStep::Pending {
                    tick,
                    cancel: aff_run_parallel(self, par.clone(), callback),
                };
            }
            AffNode::ParMap(..) | AffNode::ParApply(..) | AffNode::ParAlt(..) => {
                panic!("ParAff must be consumed through sequential")
            }
        }
        false
    }
    fn unwind(&self, machine: &mut AffMachine, frame: AffFrame, result: AffResult) {
        let now = usize::from(machine.interrupt.is_some());
        let apply = |function: &AffValue, value| match aff_try(|| aff_call(function, value)) {
            Ok(value) => AffStep::Node(aff_node(&value)),
            Err(error) => AffStep::Result(Err(error)),
        };
        match frame {
            AffFrame::Bind(f, before) => {
                machine.step = match result {
                    Ok(value) if machine.mask > 0 || before == now => apply(&f, value),
                    other => AffStep::Result(other),
                }
            }
            AffFrame::Map(f, before) => {
                machine.step = match result {
                    Ok(value) if machine.mask > 0 || before == now => {
                        AffStep::Result(aff_try(|| aff_call(&f, value)))
                    }
                    other => AffStep::Result(other),
                }
            }
            AffFrame::Catch(f, before) => {
                machine.step = match result {
                    Err(error) if machine.mask > 0 || before == now => apply(&f, error),
                    other => AffStep::Result(other),
                }
            }
            AffFrame::Acquire {
                options,
                use_resource,
                interrupt,
            } => {
                machine.mask -= 1;
                match result {
                    Ok(resource) => {
                        machine.frames.push(AffFrame::Release {
                            options,
                            resource: resource.clone(),
                            interrupt,
                        });
                        machine.step = if machine.mask > 0 || interrupt == now {
                            apply(&use_resource, resource)
                        } else {
                            AffStep::Result(Ok(resource))
                        };
                    }
                    Err(error) => machine.step = AffStep::Result(Err(error)),
                }
            }
            AffFrame::Release {
                options,
                resource,
                interrupt,
            } => {
                let cleanup = if machine.mask == 0 && interrupt != now {
                    aff_try(|| {
                        aff_call(
                            &aff_call(&options.get_killed(), machine.interrupt.clone().unwrap()),
                            resource,
                        )
                    })
                } else {
                    match &result {
                        Ok(value) => aff_try(|| {
                            aff_call(&aff_call(&options.get_completed(), value.clone()), resource)
                        }),
                        Err(error) => aff_try(|| {
                            aff_call(&aff_call(&options.get_failed(), error.clone()), resource)
                        }),
                    }
                };
                machine.frames.push(AffFrame::Finalized(result));
                machine.mask += 1;
                machine.step = match cleanup {
                    Ok(value) => AffStep::Node(aff_node(&value)),
                    Err(error) => AffStep::Result(Err(error)),
                };
            }
            AffFrame::Finalized(previous) => {
                machine.mask -= 1;
                machine.step = AffStep::Result(previous);
            }
        }
    }
}

fn aff_fiber_record(fiber: Arc<AffFiber>) -> AffValue {
    let run_fiber = fiber.clone();
    let join_fiber = fiber.clone();
    let kill_fiber = fiber.clone();
    let complete_fiber = fiber.clone();
    let suspended_fiber = fiber;
    let mut record = purust_core::Record_a::default();
    record.run = Some(aff_effect(move || {
        run_fiber.start();
        crate::Value::Unit
    }));
    record.join = Some(aff_fn(move |callback| {
        let fiber = join_fiber.clone();
        aff_effect(move || {
            let callback = callback.clone();
            let util = fiber.util.clone();
            let remover = fiber.observe(
                false,
                Arc::new(move |result| {
                    aff_run_effect(&aff_call(&callback, util.encode(result)));
                }),
            );
            fiber.start();
            remover
        })
    }));
    record.kill = Some(crate::Value::Func2(purust_core::Func2::Shared(Arc::new(
        move |error, callback| {
            let fiber = kill_fiber.clone();
            aff_effect(move || {
                let callback = callback.clone();
                let util = fiber.util.clone();
                fiber.kill(
                    error.clone(),
                    Arc::new(move |result| {
                        aff_run_effect(&aff_call(&callback, util.encode(result)));
                    }),
                )
            })
        },
    ))));
    record.onComplete = Some(aff_fn(move |options| {
        let fiber = complete_fiber.clone();
        aff_effect(move || {
            let callback = options.get_handler();
            let util = fiber.util.clone();
            fiber.observe(
                options.get_rethrow().unwrap_bool(),
                Arc::new(move |result| {
                    aff_run_effect(&aff_call(&callback, util.encode(result)));
                }),
            )
        })
    }));
    record.isSuspended = Some(aff_effect(move || {
        let state = suspended_fiber.state.lock().unwrap();
        crate::Value::Bool(!state.started && state.result.is_none())
    }));
    crate::Value::Record_a(perceus_ptr::PerceusPtr::new(record))
}

pub fn Effect_Aff__pure(value: AffValue) -> AffValue {
    aff_box(AffNode::Pure(value))
}
pub fn Effect_Aff__throwError(error: AffValue) -> AffValue {
    aff_box(AffNode::Throw(error))
}
pub fn Effect_Aff__bind(aff: AffValue, next: purust_core::Func1<AffValue, AffValue>) -> AffValue {
    aff_box(AffNode::Bind(aff_node(&aff), crate::Value::Func1(next)))
}
pub fn Effect_Aff__map(map: purust_core::Func1<AffValue, AffValue>, aff: AffValue) -> AffValue {
    let node = aff_node(&aff);
    if let AffNode::Pure(value) = &node.node {
        aff_box(AffNode::Pure(map(value.clone())))
    } else {
        aff_box(AffNode::Map(node, crate::Value::Func1(map)))
    }
}
pub fn Effect_Aff__catchError(
    aff: AffValue,
    handler: purust_core::Func1<AffValue, AffValue>,
) -> AffValue {
    aff_box(AffNode::Catch(aff_node(&aff), crate::Value::Func1(handler)))
}
pub fn Effect_Aff__liftEffect(effect: AffValue) -> AffValue {
    aff_box(AffNode::Sync(effect))
}
pub fn Effect_Aff__fork(immediate: bool, aff: AffValue) -> AffValue {
    aff_box(AffNode::Fork(immediate, aff_node(&aff)))
}
pub fn Effect_Aff_makeAff(
    build: purust_core::Func1<
        purust_core::Func1<Arc<Purs_Data_Either::Either>, AffValue>,
        AffValue,
    >,
) -> AffValue {
    aff_box(AffNode::Async(aff_fn(move |callback| {
        build(purust_core::Func1::Shared(Arc::new(move |either| {
            aff_call(&callback, crate::Value::Class(Arc::new(either)))
        })))
    })))
}
pub fn Effect_Aff_generalBracket(
    acquire: AffValue,
    conditions: AffValue,
    use_resource: purust_core::Func1<AffValue, AffValue>,
) -> AffValue {
    aff_box(AffNode::Bracket(
        aff_node(&acquire),
        conditions,
        crate::Value::Func1(use_resource),
    ))
}
pub fn Effect_Aff__makeFiber() -> AffValue {
    crate::Value::Func2(purust_core::Func2::Shared(Arc::new(|util, aff| {
        aff_effect(move || {
            aff_fiber_record(AffFiber::new(
                aff_runtime(),
                AffUtil::new(util.clone()),
                None,
                aff_node(&aff),
            ))
        })
    })))
}
pub fn Effect_Aff__makeSupervisedFiber() -> AffValue {
    crate::Value::Func2(purust_core::Func2::Shared(Arc::new(|util, aff| {
        aff_effect(move || {
            let supervisor = Arc::new(AffSupervisor {
                fibers: Mutex::new(BTreeMap::new()),
            });
            let fiber = AffFiber::new(
                aff_runtime(),
                AffUtil::new(util.clone()),
                Some(supervisor.clone()),
                aff_node(&aff),
            );
            let mut record = purust_core::Record_a::default();
            record.fiber = Some(aff_fiber_record(fiber));
            record.supervisor = Some(crate::Value::Class(supervisor));
            crate::Value::Record_a(perceus_ptr::PerceusPtr::new(record))
        })
    })))
}
// Dispatch expired timers in deadline/registration order. A continuation can
// perform arbitrary synchronous CPU work, so it must not run on the timer
// loop or occupy an async worker needed to wake other fibers.
async fn aff_timer_loop(runtime: Arc<AffRuntime>) {
    loop {
        let changed = runtime.timer_changed.notified();
        let (ready, deadline) = {
            let mut timers = runtime.timers.lock().unwrap();
            match timers.first_key_value() {
                Some((key, _)) if key.0 <= tokio::time::Instant::now() => {
                    (timers.pop_first().map(|(_, callback)| callback), None)
                }
                Some((key, _)) => (None, Some(key.0)),
                None => (None, None),
            }
        };
        if let Some(callback) = ready {
            runtime.handle.spawn_blocking(move || callback(Ok(crate::Value::Unit)));
        } else if let Some(deadline) = deadline {
            tokio::select! { _ = tokio::time::sleep_until(deadline) => {}, _ = changed => {} }
        } else {
            changed.await;
        }
    }
}

pub fn Effect_Aff__delay() -> AffValue {
    crate::Value::Func2(purust_core::Func2::Shared(Arc::new(
        |_right, milliseconds| {
            let milliseconds = milliseconds.unwrap_number();
            aff_box(AffNode::NativeAsync {
                yield_after_registration: true,
                build: Arc::new(move |callback| {
                    let runtime = aff_runtime();
                    let duration = std::time::Duration::from_secs_f64(if milliseconds.is_finite() {
                        milliseconds.max(0.0) / 1000.0
                    } else {
                        0.0
                    });
                    let key = (
                        tokio::time::Instant::now() + duration,
                        runtime.next_id.fetch_add(1, Ordering::Relaxed),
                    );
                    runtime.timers.lock().unwrap().insert(key, callback);
                    runtime.timer_changed.notify_one();
                    Arc::new(move |_| {
                        runtime.timers.lock().unwrap().remove(&key);
                        runtime.timer_changed.notify_one();
                        aff_unit()
                    })
                }),
            })
        },
    )))
}

pub fn Effect_Aff__parAffMap(
    map: purust_core::Func1<AffValue, AffValue>,
    aff: AffValue,
) -> AffValue {
    aff_box(AffNode::ParMap(crate::Value::Func1(map), aff_node(&aff)))
}
pub fn Effect_Aff__parAffApply(left: AffValue, right: AffValue) -> AffValue {
    aff_box(AffNode::ParApply(aff_node(&left), aff_node(&right)))
}
pub fn Effect_Aff__parAffAlt(left: AffValue, right: AffValue) -> AffValue {
    aff_box(AffNode::ParAlt(aff_node(&left), aff_node(&right)))
}
pub fn Effect_Aff__sequential() -> AffValue {
    aff_fn(|aff| aff_box(AffNode::Sequential(aff_node(&aff))))
}

// Revoking a cancellation wait removes its observers; it never revives a fiber.
fn aff_cancel_wait(removers: Arc<Mutex<Vec<AffValue>>>) -> AffCancel {
    Arc::new(move |_| {
        let removers = removers.clone();
        aff_new(AffNode::Sync(aff_effect(move || {
            let callbacks = std::mem::take(&mut *removers.lock().unwrap());
            for callback in callbacks {
                aff_run_effect(&callback);
            }
            crate::Value::Unit
        })))
    })
}

fn aff_kill_many(
    fibers: Vec<Arc<AffFiber>>,
    error: AffValue,
    callback: AffCallback,
) -> Arc<Mutex<Vec<AffValue>>> {
    let removers = Arc::new(Mutex::new(Vec::new()));
    if fibers.is_empty() {
        callback(Ok(crate::Value::Unit));
        return removers;
    }
    let remaining = Arc::new(AtomicUsize::new(fibers.len()));
    for fiber in fibers {
        let remaining = remaining.clone();
        let callback = callback.clone();
        let remover = fiber.kill(
            error.clone(),
            Arc::new(move |_| {
                if remaining.fetch_sub(1, Ordering::AcqRel) == 1 {
                    callback(Ok(crate::Value::Unit));
                }
            }),
        );
        removers.lock().unwrap().push(remover);
    }
    removers
}

pub fn Effect_Aff__killAll() -> AffValue {
    crate::Value::Func3(purust_core::Func3::Shared(Arc::new(
        |error, supervisor, callback| {
            aff_effect(move || {
                let fibers = supervisor
                    .unwrap_class::<AffSupervisor>()
                    .fibers
                    .lock()
                    .unwrap()
                    .values()
                    .filter_map(Weak::upgrade)
                    .collect();
                let callback = callback.clone();
                let removers = aff_kill_many(
                    fibers,
                    error.clone(),
                    Arc::new(move |_| {
                        aff_run_effect(&callback);
                    }),
                );
                let cancel = aff_cancel_wait(removers);
                aff_fn(move |error| crate::Value::Class(Arc::new(AffBox(cancel(error)))))
            })
        },
    )))
}

enum AffParKind {
    Leaf,
    Map(AffValue),
    Apply,
    Alt,
}
struct AffParEntry {
    kind: AffParKind,
    parent: Option<usize>,
    children: Vec<usize>,
    fiber: Option<Arc<AffFiber>>,
    result: Option<AffResult>,
    first_error: Option<AffValue>,
    settling: bool,
}
struct AffParState {
    entries: Vec<AffParEntry>,
    stopped: bool,
}
struct AffParRun {
    state: Mutex<AffParState>,
    callback: AffCallback,
    early: AffValue,
}
enum AffParAction {
    Done(AffResult),
    Wait,
    Set(usize, AffResult),
    Map(usize, AffValue, AffValue),
    Apply(usize, AffValue, AffValue),
    Kill(usize, Vec<Arc<AffFiber>>, AffResult),
}

impl AffParRun {
    fn pending(entries: &[AffParEntry], root: usize) -> Vec<Arc<AffFiber>> {
        let mut stack = vec![root];
        let mut fibers = Vec::new();
        while let Some(index) = stack.pop() {
            let entry = &entries[index];
            if let Some(fiber) = &entry.fiber {
                if entry.result.is_none() {
                    fibers.push(fiber.clone());
                }
            } else {
                stack.extend(entry.children.iter().rev().copied());
            }
        }
        fibers
    }
    fn settle(self: &Arc<Self>, mut index: usize, mut result: AffResult) {
        loop {
            let action = {
                let mut state = self.state.lock().unwrap();
                if state.stopped || state.entries[index].result.is_some() {
                    return;
                }
                state.entries[index].result = Some(result.clone());
                match state.entries[index].parent {
                    None => {
                        state.stopped = true;
                        AffParAction::Done(result.clone())
                    }
                    Some(parent) => {
                        if state.entries[parent].settling || state.entries[parent].result.is_some()
                        {
                            return;
                        }
                        let children = state.entries[parent].children.clone();
                        match &state.entries[parent].kind {
                            AffParKind::Map(map) => match &result {
                                Ok(value) => {
                                    let action =
                                        AffParAction::Map(parent, map.clone(), value.clone());
                                    state.entries[parent].settling = true;
                                    action
                                }
                                Err(error) => AffParAction::Set(parent, Err(error.clone())),
                            },
                            AffParKind::Apply => match &result {
                                Err(error) => {
                                    let sibling = if children[0] == index {
                                        children[1]
                                    } else {
                                        children[0]
                                    };
                                    let fibers = Self::pending(&state.entries, sibling);
                                    state.entries[parent].settling = true;
                                    AffParAction::Kill(parent, fibers, Err(error.clone()))
                                }
                                Ok(_) => match (
                                    &state.entries[children[0]].result,
                                    &state.entries[children[1]].result,
                                ) {
                                    (Some(Ok(function)), Some(Ok(value))) => {
                                        let action = AffParAction::Apply(
                                            parent,
                                            function.clone(),
                                            value.clone(),
                                        );
                                        state.entries[parent].settling = true;
                                        action
                                    }
                                    _ => AffParAction::Wait,
                                },
                            },
                            AffParKind::Alt => match &result {
                                Ok(value) => {
                                    let sibling = if children[0] == index {
                                        children[1]
                                    } else {
                                        children[0]
                                    };
                                    let fibers = Self::pending(&state.entries, sibling);
                                    state.entries[parent].settling = true;
                                    AffParAction::Kill(parent, fibers, Ok(value.clone()))
                                }
                                Err(error) => {
                                    if state.entries[parent].first_error.is_none() {
                                        state.entries[parent].first_error = Some(error.clone());
                                    }
                                    if children
                                        .iter()
                                        .all(|child| state.entries[*child].result.is_some())
                                    {
                                        AffParAction::Set(
                                            parent,
                                            Err(state.entries[parent].first_error.clone().unwrap()),
                                        )
                                    } else {
                                        AffParAction::Wait
                                    }
                                }
                            },
                            AffParKind::Leaf => unreachable!(),
                        }
                    }
                }
            };
            match action {
                AffParAction::Done(result) => {
                    (self.callback)(result);
                    return;
                }
                AffParAction::Wait => return,
                AffParAction::Set(parent, next) => {
                    index = parent;
                    result = next;
                }
                AffParAction::Map(parent, function, value)
                | AffParAction::Apply(parent, function, value) => {
                    index = parent;
                    result = aff_try(|| aff_call(&function, value));
                }
                AffParAction::Kill(parent, fibers, outcome) => {
                    if fibers.is_empty() {
                        index = parent;
                        result = outcome;
                    } else {
                        let run = self.clone();
                        aff_kill_many(
                            fibers,
                            self.early.clone(),
                            Arc::new(move |_| run.settle(parent, outcome.clone())),
                        );
                        return;
                    }
                }
            }
        }
    }
    fn cancel(self: &Arc<Self>, error: AffValue) -> AffNodeRef {
        let run = self.clone();
        aff_new(AffNode::NativeAsync {
            yield_after_registration: false,
            build: Arc::new(move |callback| {
                let fibers = {
                    let mut state = run.state.lock().unwrap();
                    state.stopped = true;
                    Self::pending(&state.entries, 0)
                };
                aff_cancel_wait(aff_kill_many(fibers, error.clone(), callback))
            }),
        })
    }
}

fn aff_run_parallel(parent: &Arc<AffFiber>, root: AffNodeRef, callback: AffCallback) -> AffCancel {
    let mut entries: Vec<AffParEntry> = Vec::new();
    let mut leaves = Vec::new();
    let mut stack = vec![(root, None)];
    // Flatten first so a synchronous winning leaf can cancel not-yet-started siblings.
    while let Some((node, parent_index)) = stack.pop() {
        let index = entries.len();
        let mut next = Vec::new();
        let (kind, fiber) = match &node.node {
            AffNode::ParMap(map, child) => {
                next.push(child.clone());
                (AffParKind::Map(map.clone()), None)
            }
            AffNode::ParApply(left, right) => {
                next.push(left.clone());
                next.push(right.clone());
                (AffParKind::Apply, None)
            }
            AffNode::ParAlt(left, right) => {
                next.push(left.clone());
                next.push(right.clone());
                (AffParKind::Alt, None)
            }
            _ => {
                let fiber = AffFiber::new(
                    parent.runtime.clone(),
                    parent.util.clone(),
                    parent.supervisor.clone(),
                    node.clone(),
                );
                fiber.register();
                leaves.push((index, fiber.clone()));
                (AffParKind::Leaf, Some(fiber))
            }
        };
        entries.push(AffParEntry {
            kind,
            parent: parent_index,
            children: Vec::new(),
            fiber,
            result: None,
            first_error: None,
            settling: false,
        });
        if let Some(parent_index) = parent_index {
            entries[parent_index].children.push(index);
        }
        for child in next.into_iter().rev() {
            stack.push((child, Some(index)));
        }
    }
    let run = Arc::new(AffParRun {
        state: Mutex::new(AffParState {
            entries,
            stopped: false,
        }),
        callback,
        early: Purs_Effect_Exception::Effect_Exception_error("[ParAff] Early exit".to_owned()),
    });
    for (index, fiber) in &leaves {
        let index = *index;
        let run = run.clone();
        fiber.observe(false, Arc::new(move |result| run.settle(index, result)));
    }
    // Every leaf is observed before any of them starts, so a synchronous winner
    // can still cancel siblings that have not begun. Start order is deliberately
    // unspecified: `start` hands each leaf to the bounded CPU pool.
    for (_, fiber) in leaves {
        fiber.start();
    }
    Arc::new(move |error| run.cancel(error))
}

#[cfg(test)]
mod parallel_tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};
    use std::time::{Duration, Instant};

    fn spin_leaf(current: &Arc<AtomicUsize>, peak: &Arc<AtomicUsize>, spin_ms: u64) -> AffNodeRef {
        let current = current.clone();
        let peak = peak.clone();
        aff_new(AffNode::Sync(aff_effect(move || {
            // Spin long enough that concurrent leaves cannot miss each other by
            // accident. The peak is the actual proof of overlap.
            let depth = current.fetch_add(1, AtomicOrdering::AcqRel) + 1;
            peak.fetch_max(depth, AtomicOrdering::AcqRel);
            let deadline = Instant::now() + Duration::from_millis(spin_ms);
            while Instant::now() < deadline {
                std::hint::spin_loop();
            }
            current.fetch_sub(1, AtomicOrdering::AcqRel);
            crate::Value::Unit
        })))
    }

    // ParApply needs a function on its left, so each added leaf maps the previous
    // result to a constant `Unit -> Unit`.
    fn par_tree(leaves: Vec<AffNodeRef>) -> AffNodeRef {
        let mut par = None;
        for leaf in leaves {
            par = Some(match par {
                None => leaf,
                Some(left) => {
                    let function =
                        aff_new(AffNode::ParMap(aff_fn(|_| aff_fn(|_| crate::Value::Unit)), left));
                    aff_new(AffNode::ParApply(function, leaf))
                }
            });
        }
        par.unwrap()
    }

    fn parallel_peak(workers: usize, outer: usize, inner: usize, spin_ms: u64) -> usize {
        let current = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        aff_run_main_with_workers(workers, || {
            let leaves = (0..outer)
                .map(|_| {
                    let nested = par_tree(
                        (0..inner)
                            .map(|_| spin_leaf(&current, &peak, spin_ms))
                            .collect(),
                    );
                    aff_new(AffNode::Sequential(nested))
                })
                .collect();
            let root = aff_box(AffNode::Sequential(par_tree(leaves)));
            aff_run_effect(&Effect_Aff_launchAff_(root))
        });
        peak.load(AtomicOrdering::Acquire)
    }

    #[test]
    fn parallel_synchronous_leaves_overlap() {
        let peak = parallel_peak(2, 1, 2, 200);
        assert_eq!(
            peak, 2,
            "two independent ParAff leaves must be able to run their synchronous \
             sections concurrently"
        );
    }

    #[test]
    fn parallel_leaves_respect_the_worker_bound() {
        let peak = parallel_peak(1, 1, 4, 20);
        assert_eq!(peak, 1, "a single worker must serialize parallel leaves");
    }

    #[test]
    fn nested_parallelism_never_deadlocks() {
        // Waiting for nested parallelism must release the worker; otherwise two
        // workers cannot drive sixteen leaves.
        let peak = parallel_peak(2, 4, 4, 50);
        assert_eq!(peak, 2);
    }
}

#[cfg(test)]
mod resumption_tests {
    use super::*;

    fn immediate_callback_threads(
        yield_after_registration: bool,
    ) -> (std::thread::ThreadId, std::thread::ThreadId) {
        let (send, receive) = std::sync::mpsc::channel();
        let registered = Arc::new(Mutex::new(None));
        purust_aff_run_main(|| {
            let registered = registered.clone();
            let wait = aff_box(AffNode::NativeAsync {
                yield_after_registration,
                // Force completion while the driver is still registering the
                // wait. This exercises the zero-delay race deterministically.
                build: Arc::new(move |callback| {
                    *registered.lock().unwrap() = Some(std::thread::current().id());
                    callback(Ok(crate::Value::Unit));
                    aff_no_cancel()
                }),
            });
            let action = Effect_Aff__bind(wait, purust_core::Func1::Shared(Arc::new(move |_| {
                send.send(std::thread::current().id()).unwrap();
                Effect_Aff__pure(crate::Value::Unit)
            })));
            aff_run_effect(&Effect_Aff_launchAff_(action))
        });
        let registered = registered.lock().unwrap().unwrap();
        (registered, receive.try_recv().unwrap())
    }

    #[test]
    fn early_timer_callback_yields_to_another_thread() {
        let (registered, resumed) = immediate_callback_threads(true);
        assert_ne!(resumed, registered);
    }

    #[test]
    fn synchronous_native_callback_stays_on_the_registration_thread() {
        let (registered, resumed) = immediate_callback_threads(false);
        assert_eq!(resumed, registered);
    }
}
