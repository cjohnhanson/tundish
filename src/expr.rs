//! jaq filters and templates, compiled once per pull.

use jaq_core::load::{Arena, File, Loader};
use jaq_core::{data, unwrap_valr, Compiler, Ctx, Vars};
use jaq_json::Val;

use crate::error::{Error, Result};

/// A compiled jaq program.
pub struct Expr {
    filter: jaq_core::Filter<data::JustLut<Val>>,
}

impl Expr {
    /// Compile `code`. A syntax or name error names the expression.
    pub fn compile(code: &str) -> Result<Self> {
        let defs = jaq_core::defs()
            .chain(jaq_std::defs())
            .chain(jaq_json::defs());
        let funs = jaq_core::funs()
            .chain(jaq_std::funs())
            .chain(jaq_json::funs());
        let arena = Arena::default();
        let modules = Loader::new(defs)
            .load(&arena, File { code, path: () })
            .map_err(|e| Error::Expr(format!("{code}: {e:?}")))?;
        let filter = Compiler::default()
            .with_funs(funs)
            .compile(modules)
            .map_err(|e| Error::Expr(format!("{code}: {e:?}")))?;
        Ok(Self { filter })
    }

    fn run(&self, input: Val) -> Result<Vec<Val>> {
        let ctx = Ctx::<data::JustLut<Val>>::new(&self.filter.lut, Vars::new([]));
        self.filter
            .id
            .run((ctx, input))
            .map(unwrap_valr)
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|e| Error::Expr(e.to_string()))
    }

    /// jq `select` semantics: true when any output is truthy.
    pub fn matches(&self, input: &Val) -> Result<bool> {
        Ok(self.run(input.clone())?.iter().any(jaq_core::ValT::as_bool))
    }

    /// The outputs joined by newlines. A string prints raw; anything
    /// else prints as JSON.
    pub fn render(&self, input: &Val) -> Result<String> {
        let parts: Vec<String> = self.run(input.clone())?.iter().map(text).collect();
        Ok(parts.join("\n"))
    }
}

/// A value as text: raw for a string, compact JSON otherwise.
pub fn text(v: &Val) -> String {
    match v {
        Val::TStr(b) | Val::BStr(b) => String::from_utf8_lossy(b).into_owned(),
        other => other.to_string(),
    }
}

/// Parse JSON text into a jaq value.
pub fn value(json: &str) -> Result<Val> {
    jaq_json::read::parse_single(json.as_bytes()).map_err(|e| Error::Expr(format!("{e:?}")))
}
