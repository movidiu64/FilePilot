//! Nucleul programului: modelul regulilor, condițiile, variabilele și șabloanele.
//!
//! Crate-ul nu face nicio operație pe disc și nu are dependențe externe, deci se
//! testează și se măsoară izolat. Toate comparațiile sunt fără diferență între
//! majuscule și minuscule.

pub mod condition;
pub mod extract;
pub mod meta;
pub mod model;
pub mod regex;
pub mod secret;
pub mod template;
pub mod timefmt;

pub use condition::{
    content_key, Condition, ContentLoader, DateField, DateOp, EvalCtx, Prepared, RuleError, SizeOp, TextField,
    TextOp, MAX_DEPTH,
};
pub use extract::{Extract, ExtractKind, Extractor, Found, Occurrence};
pub use meta::FileMeta;
pub use model::{visible_rules, Action, Conflict, Rule, WatchedFolder};
pub use template::{relative_tree, safe_name, Dictionary, DateSrc, Mode, Part, RenderCtx, Template, TemplateError, Tz, Var};
pub use timefmt::Lang;
