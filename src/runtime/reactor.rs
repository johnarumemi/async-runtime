#[allow(unused)]
use std::{
    collections::HashMap,
    future::Future,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex, OnceLock,
    },
    // `Wake` trait is not used, but kept here to highlight it's importance
    task::{Context, Wake, Waker},
    thread,
};

use mio::{net::TcpStream, Events, Interest, Poll, Registry, Token};

// ===================== END OF DEPENDENCIES =====================

// NOTE: Reactor is dependent on `std::task::Waker`
// rather than our own custom `MyWaker`.
type Wakers = Arc<Mutex<HashMap<usize, Waker>>>;

/// WARNING: This can be accessed from multiple threads.
/// However, we use the OnceLock to ensure that we only initialise the Reactor once.
/// Hence, there will only be a single instance of this reactor running, even if
/// multiple threads are accessing it.
/// It is however private to this module.
static REACTOR: OnceLock<Reactor> = OnceLock::new();

pub fn reactor() -> &'static Reactor {
    REACTOR
        .get()
        .expect("Reactor called outside a runtime context")
}

pub struct Reactor {
    wakers: Wakers,
    // used for interacting with event queue in mio
    registry: Registry,
    /// tracks next available ID / Token, so that we can track which event occurred and
    /// which Waker to use.
    /// NOTE: We are not using the task id's as tokens to mio. In fact, whenever we register
    /// interest in an event on a source, we do no reuse token ID's. This means we do not
    /// accidently get the same token ID twice for a given source.
    next_id: AtomicUsize,
}

impl Reactor {
    /// Register interest in notifications for an event source
    pub fn register(&self, stream: &mut TcpStream, interest: Interest, id: usize) {
        self.registry
            .register(stream, Token(id), interest)
            .expect("Failed to register stream with reactor");
    }

    pub fn set_waker(&self, cx: &Context, id: usize) {
        let _ = self
            .wakers
            .lock()
            .as_deref_mut()
            // if waker was updated, old waker is returned. We discard it via using `is_none()`
            // IMPORTANT: we always store the most recent waker for a given task.
            .map(|w| w.insert(id, cx.waker().clone()).is_none())
            .unwrap();
    }

    pub fn deregister(&self, stream: &mut TcpStream, id: usize) {
        // 1. remove waker
        self.wakers
            .lock()
            .as_deref_mut()
            .map(|w| w.remove(&id))
            .unwrap();

        // 2. syscall to deregister `id`
        self.registry.deregister(stream).unwrap();
    }

    pub fn next_id(&self) -> usize {
        // only care about ensuring that we don't hand out the same value twice, so Relaxed
        // ordering suffices.
        self.next_id.fetch_add(1, Ordering::Relaxed)
    }
}

/// Holds logic for event loop that waits and reacts to new events
fn event_loop(mut poll: Poll, wakers: Wakers) {
    let mut events = Events::with_capacity(100);

    loop {
        // 1. Block on event queue until OS notifies us of ready events.
        //    This yields exection of current thread to OS scheduler.
        poll.poll(&mut events, None).unwrap();

        // 2. Iterate through events and match tokens with wakers.
        //    Then call waker's `wake` method.
        for event in events.iter() {
            let Token(id) = event.token();

            let wakers = wakers.lock().unwrap();

            if let Some(waker) = wakers.get(&id) {
                // Waker for token ID found

                // NOTE: we use `wake_by_ref`, since `wake` consumes the waker due
                // to having a receiver of `self` vs `&self` with wake_by_ref.
                waker.wake_by_ref()
            }
        }

        // Finished processing all events. Repeat and go back to blocking on event queue.
    }
}

/// Initialise the reactor and start the event loop.
pub fn start() {
    let wakers: Wakers = Arc::new(Mutex::new(HashMap::new()));

    // OS event queue abstraction
    // NOTE: The reactor does not "Own" the poll instance, the event_loop does.
    // The reactor does have access to the registry though, to enable communicating
    // with the event queue. It's only the Poll instance though that can block on the event queue.
    let poll = Poll::new().unwrap();
    let registry = poll.registry().try_clone().unwrap();
    let next_id = AtomicUsize::new(1);
    let reactor = Reactor {
        wakers: wakers.clone(),
        registry,
        next_id,
    };

    // Set global reactor instance
    // From this point, the reactor is alive and running
    REACTOR.set(reactor).ok().expect("Reactor already running");

    // spawn a new OS thread that runs the main event_loop. The event loop
    // makes use of the Reactor helper methods to modify state.
    // NOTE: could have just allowed it to access reactor wakers directly without
    // passing them in as arguments.
    thread::spawn(move || event_loop(poll, wakers));
}
