//! DOM bindings: what a page's scripts see of its document.
//!
//! Step 16.1 brought the engine into the build with this state empty, so
//! that `Realm` could keep its field; the bindings themselves (`document`,
//! elements, events) are step 16.2.

#[derive(Default)]
pub struct DomState {}

impl DomState {
    /// Forgets every node wrapper (page teardown).
    pub fn clear(&mut self) {}
}
