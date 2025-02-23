//! # Requirements
//! - delayserver must be running on port 8080
//!
//! # Usage
//! Run with following
//! ```bash
//! cargo run --example basic
//! ```
mod http;

use async_runtime::runtime;
use http::Http;

pub fn main() {
    // initialise the runtime
    let mut executor = runtime::init();

    // The main top-level future we start executor with
    let future = async_main();

    executor.block_on(future);
}

async fn async_main() {
    println!("Program starting");

    let txt = Http::get("/600/HelloAsyncAwait").await;
    println!("{txt}");

    let txt = Http::get("/400/HelloAsyncAwait").await;
    println!("{txt}");
}
