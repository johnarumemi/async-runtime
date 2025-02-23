use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex},
    task::{Context, Poll, Wake, Waker},
    thread::{self, Thread},
};

// NOTE: Task's must now be pinned on the heap. Our top level futures
// are expected to resolve to `()`, the unit type (aka void)
type Task = Pin<Box<dyn Future<Output = ()>>>;

// thread local static variable.
// Each OS thread will have only 1 executor running on it.
// This makes it impossible for one thread to access another thread's executor.
//
// NOTE: lazy initialisation occurs if static variable not set on first
// use with `CURRENT_EXEC.with(|executor| {...})`
thread_local! {
    static CURRENT_EXEC: ExecutorCore = ExecutorCore::default();
}

/// NOTE: fields are wrapped in types that allow the static variable
/// to be mutated via interior mutability.
#[derive(Default)]
struct ExecutorCore {
    /// We can't simply mutate a static variable, so we use a RefCell to grant us
    /// interior mutability.
    ///
    /// RefCell:: Mutable memory location with dynamically checked borrow rules.
    ///
    /// HashMap where:
    /// key = id of Task
    /// value = Task / Top-Level Future
    tasks: RefCell<HashMap<usize, Task>>,

    /// id of Tasks that are ready to be polled.
    ///
    /// This Arc will be cloned and given to each Waker
    /// that the executor creates and passes to a Task when polling it.
    /// The Waker will be sent to a different thread, to to keep Waker
    /// as Send + Sync, we need the ready_queue to be wrapped in an Arc.
    ready_queue: Arc<Mutex<Vec<usize>>>,

    /// Counter that gives out next available task ID.
    ///
    /// It should never hand out the same ID twice for a given ExecutorCore.
    /// A Cell will suffice for giving us interior mutability needed on the ExecutorCore.
    next_id: Cell<usize>,
}

/// Alternative is to place this in `future` crate, since it's part of the `Future` trait.
#[derive(Clone)]
pub struct MyWaker {
    /// Handle to executor thread
    ///
    /// This enables us to park and unpark the executor's thread using the Waker.
    /// WARNING: any other library may also be making use of getting the current thread, parking it
    /// and unparking it. This may cause us to miss wake ups or get trapped in deadlocks. This is
    /// only used for this simple implementation: see other asynchronous libraries for how they
    /// implement their Wakers.
    /// e.g. crossbeam: https://docs.rs/crossbeam/latest/crossbeam/sync/struct.Parker.html
    thread: Thread,
    /// Identifies which Task this waker is associated with. Returned from event_queue ready list as
    /// part user data.
    id: usize,
    /// Reference to the ready_queue of the executor
    ///
    /// usize: represents the id of a Task in the ready queue.
    ///
    /// NOTE: Waker could also have been supplied a function via executor that would
    /// add associated Task back to it's ready queue, without the Waker itself keeping
    /// a reference to the queue directly like below.
    /// TODO: implement above method instead.
    ready_queue: Arc<Mutex<Vec<usize>>>,
}

// NOTE: Implement the `Wake` trait from standard library on our Waker.
// Since `wake` consumes self, ensure that waker is actually called in
// the reactor via `wake_by_ref`, which has a receiver parameter of
// `&self` instead: i.e takes a references to the waker rather than
// ownership.
impl Wake for MyWaker {
    /// The function signature of `wake`, means that `MyWaker`
    /// can only be called when wrapped within an `Arc`, i.e. heap allocated.
    fn wake(self: Arc<Self>) {
        // 1. Add wakers associated task to ready queue
        // (let executor know it's ready to be polled)
        //
        // Be careful of calling unpark before
        // MutexGuard is dropped.
        self.ready_queue
            .lock()
            .as_deref_mut()
            .map(|queue| {
                queue.push(self.id);
            })
            .unwrap();

        // 2.  Unpark executor if it's yielded control back to the OS scheduler / is parked.
        self.thread.unpark();
        println!("Waker {0} woke up executor.", self.id)
    }
}

/// Allows spawning of new top-level futures (aka Tasks) from anywhere in the thread.
pub fn spawn<F>(future: F)
where
    F: Future<Output = ()> + 'static,
{
    CURRENT_EXEC.with(|executor| {
        let next_id = executor.next_id.get();

        // The future must be pinned before it can be polled
        let task: Task = Box::pin(future);

        executor.tasks.borrow_mut().insert(next_id, task);

        // Add task to queue to ensure it is polled at least once to start progressing it.
        // Remember that futures are inert / lazy in Rust.
        if let Ok(queue) = executor.ready_queue.lock().as_deref_mut() {
            queue.push(next_id)
        }

        executor.next_id.set(next_id + 1);
    });
}

/// Requires no state of it's own. All that is in ExecutorCore, which is scoped to a thread.
#[derive(Default)]
pub struct Executor;

impl Executor {
    pub fn new() -> Self {
        Self
    }

    /// Pop a task id from ready_queue, return None if queue is empty.
    fn pop_ready(&self) -> Option<usize> {
        CURRENT_EXEC.with(|executor| {
            executor
                .ready_queue
                .lock()
                .as_deref_mut()
                .map(|queue| queue.pop())
                .unwrap()
        })
    }

    /// WARNING: also remove tasks for hash map of (id, Task)
    /// This is to prvent accidently trying retrieving the task and poll it even after
    /// it has completed. Instead, we get the task from the hash map.
    /// We then poll the Task. If it returns `NotReady`, then we add it back in to hash map.
    fn get_future(&self, id: usize) -> Option<Task> {
        CURRENT_EXEC.with(|executor| {
            let task: Option<Task> = executor.tasks.borrow_mut().remove(&id);

            task
        })
    }

    fn get_waker(&self, id: usize) -> Arc<MyWaker> {
        let ready_queue = CURRENT_EXEC.with(|executor| executor.ready_queue.clone());

        Arc::new(MyWaker {
            id,
            thread: thread::current(),
            ready_queue,
        })
    }

    /// Simply inserts the task into the hash map on ExecutorCore. It does not
    /// queue the task onto the ready_queue.
    fn insert_task(&self, id: usize, task: Task) {
        CURRENT_EXEC.with(|executor| {
            executor.tasks.borrow_mut().insert(id, task);
        })
    }

    fn task_count(&self) -> usize {
        CURRENT_EXEC.with(|executor| executor.tasks.borrow().len())
    }

    /// IMPORTANT: core logic of the executor.
    pub fn block_on<F>(&mut self, future: F)
    where
        F: Future<Output = ()> + 'static,
    {
        // NOTE: there are some futures that return Ready on first poll, so we
        // could an optimisation to poll all futures at least once. However, due
        // to below warning, this was commented out. It was left here for
        // learning purposes.
        // ---
        // WARNING: by polling the future once here, the future is thus located
        // within the stack frame of the `block_on` function. The act of polling
        // it results in self.stack.writer holding a reference to buffer, i.e. a
        // self reference. The first poll returns `NotReady`, and so we spawn
        // it, placing it within a Box, which moves the future onto the heap.
        // The next time the future is polled, the stack will be restored.
        // However, the reference held by self.stack.writer will be invalid as
        // it is pointing to the old location on the stack where the future was
        // located.
        // ---
        // let mut waker = self.get_waker(usize::MAX);
        // let mut future = future;

        // match Box::pin(future).as_mut().poll(&waker) {
        //     // future needs to be waited on
        //     PollState::NotReady => {}
        //     // future is ready, no need to block, so return
        //     PollState::Ready(_) => return,
        // }

        // spawn the future on the executor, making it a top-level task
        // note that `spawn` will also move the future to the heap and pin it.
        spawn(future);

        // Loop over all tasks in ready_queue and poll them once each
        'outer: loop {
            while let Some(id) = self.pop_ready() {
                // 1. Retrieve Task from ExecutorCore
                let mut task: Task = match self.get_future(id) {
                    Some(task) => task,
                    // Below guards agains spurious wakeups. Match arm can be reached if
                    // task has been completed already and is not in the ExecutorCore's hash map.
                    None => continue,
                };

                // 2. Creater a waker to use when polling the task
                // NOTE: we are now using a Context struct to wrap the waker.
                // But first we convert from MyWaker to `std::task::Waker`
                let waker: Waker = self.get_waker(id).into();
                let mut cx = Context::from_waker(&waker);

                // 3. Poll future / task
                match task.as_mut().poll(&mut cx) {
                    // Add future back into the hash map
                    Poll::Pending => self.insert_task(id, task),
                    // nothing to do, task already removed from hash map
                    Poll::Ready(_) => continue,
                }
            } // END OF WHILE LOOP

            // 4. Decide wether to park or not based on current uncompleted top-level Tasks
            let task_count = self.task_count();

            // Only used for debug purposes
            let thread_name = thread::current().name().unwrap().to_string();

            if task_count > 0 {
                println!("{thread_name}: {task_count} pending tasks. Sleeping until woken up.");
                thread::park()
            } else {
                println!("{thread_name}: All tasks finished.");
                break 'outer;
            }
        }
    }
}
