//! `crate::web::js` for the host build: soft-float numbers (`num`, over
//! `big`) and the regular expression matcher (`regex`).

#[path = "../../../../src/web/js/big.rs"]
pub mod big;
#[path = "../../../../src/web/js/num.rs"]
pub mod num;
#[path = "../../../../src/web/js/regex.rs"]
pub mod regex;
