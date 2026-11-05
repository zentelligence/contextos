//! `$math$` and `$$math$$` as `MathML` Core, which browsers draw with no script.

use math_core::{LatexToMathML, MathCoreConfig, MathDisplay};
use pulldown_cmark::{CowStr, Event};

use crate::escape::{escape_attribute, escape_text};
use crate::prepare::Spanned;

/// The converter for one page.
pub(super) struct Math(Option<LatexToMathML>);

impl Default for Math {
    fn default() -> Self {
        // No macros are defined, so the converter cannot fail to build; were it to, every expression is shown as
        // flagged source rather than the page failing.
        Self(LatexToMathML::new(MathCoreConfig::default(), Vec::new()).ok())
    }
}

impl Math {
    fn typeset(&self, latex: &str, display: MathDisplay) -> String {
        let converted = self
            .0
            .as_ref()
            .ok_or_else(|| "math typesetting is unavailable".to_owned())
            .and_then(|converter| {
                converter
                    .convert_with_local_state(latex, display)
                    .map_err(|error| error.to_string())
            });
        match converted {
            Ok(result) => result.mathml,
            Err(reason) => format!(
                "<span class=\"render-flag math-source\" title=\"This expression could not be typeset: {}\"><code>{}</code></span>",
                escape_attribute(&reason),
                escape_text(latex)
            ),
        }
    }

    /// Each math event typeset in place.
    pub(super) fn apply(&self, events: Vec<Spanned>) -> Vec<Spanned> {
        events
            .into_iter()
            .map(|(event, range)| match event {
                Event::InlineMath(latex) => (
                    Event::Html(CowStr::from(self.typeset(&latex, MathDisplay::Inline))),
                    range,
                ),
                Event::DisplayMath(latex) => (
                    Event::Html(CowStr::from(self.typeset(&latex, MathDisplay::Block))),
                    range,
                ),
                other => (other, range),
            })
            .collect()
    }
}
