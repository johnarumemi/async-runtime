mod executor;
mod reactor;

pub use executor::{spawn, Executor, MyWaker};
pub use reactor::reactor;

pub fn init() -> Executor {
    // Start reactor and event_loop

    // NOTE: event looop is spawned in different thread,
    // and reactor is initialised as a global static variable.
    reactor::start();
    // create executor and return it to caller
    Executor::new()
}
