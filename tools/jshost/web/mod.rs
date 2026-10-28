//! The part of `src/web/` the JavaScript engine needs, for the host
//! build: the engine and its DOM, the document, selectors, addresses.

#[path = "../../../src/web/text.rs"]
pub mod text;
#[path = "../../../src/web/html.rs"]
pub mod html;
#[path = "../../../src/web/tree.rs"]
pub mod tree;
#[path = "../../../src/web/css.rs"]
pub mod css;
#[path = "../../../src/web/url.rs"]
pub mod url;
#[path = "../../../src/web/js/mod.rs"]
pub mod js;
