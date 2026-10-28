//! The part of `src/web/` the JavaScript engine needs, for the host
//! build: the engine itself, and the document its contexts carry.

#[path = "../../../src/web/text.rs"]
pub mod text;
#[path = "../../../src/web/html.rs"]
pub mod html;
#[path = "../../../src/web/js/mod.rs"]
pub mod js;
