//! DEPRECATED: This module is no longer required as we now
//! implement the standard library Future trait.
#![allow(unused)]
use std::pin::Pin;

use crate::runtime::MyWaker;

/// Represents some operation that will complete in the future
/// and return a value of type `Future::Output`.
pub trait Future {
    type Output;
    // When we poll a future, we must now supply a Waker
    fn poll(self: Pin<&mut Self>, waker: &MyWaker) -> PollState<Self::Output>;
}

/// PollState is an enum that represents the state of a future.
/// It is either Ready or NotReady. The value returned when ready is of type T
pub enum PollState<T> {
    Ready(T),
    NotReady,
}
