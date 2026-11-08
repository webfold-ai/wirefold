//! The `anyhow` masquerade — a boxed dynamic error with a context chain, plus the `.context()`
//! extension trait. Pairs with the crate-root `#[derive(Error)]` macro (the `thiserror`
//! masquerade, `lib.rs`): a proc-macro derive and this module's [`Error`] type share the
//! identifier `Error` at the crate root with zero conflict — one lives in the macro namespace,
//! the other in the type namespace, so a single flat root satisfies both masquerades at once
//! (`use wirefold::Error;` picks whichever namespace the use-site needs: `#[derive(Error)]`
//! resolves the macro, `-> Result<T, Error>` resolves the type). No `anyhow`/`thiserror`
//! dependency anywhere — `std::error::Error` is the only trait either masquerade rides on.
//!
//! **Deliberately not `std::error::Error`.** The real `anyhow::Error` isn't either, for the same
//! reason: a blanket `impl<E: std::error::Error> From<E> for Error` collides with core's
//! reflexive `impl<T> From<T> for T` the moment `Error` implements `std::error::Error` itself
//! (`E = Error` becomes a valid, ambiguous substitution). [`root_cause`](Error::root_cause) is
//! the walk-the-chain escape hatch instead of `source()`.

use std::fmt;

/// A boxed dynamic error carrying a context chain — the `anyhow::Error` masquerade. Every
/// [`Context::context`] call prepends one line; [`fmt::Display`] renders them outermost-first.
pub struct Error {
    context: Vec<String>,
    inner: Box<dyn std::error::Error + Send + Sync + 'static>,
}

impl Error {
    /// Wrap any `std::error::Error` with no context yet — the `anyhow::Error::new`/`From` path.
    pub fn new<E: std::error::Error + Send + Sync + 'static>(inner: E) -> Self {
        Error {
            context: Vec::new(),
            inner: Box::new(inner),
        }
    }

    /// The original error at the bottom of the chain (`anyhow::Error::root_cause`).
    #[must_use]
    pub fn root_cause(&self) -> &(dyn std::error::Error + 'static) {
        &*self.inner
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for line in self.context.iter().rev() {
            writeln!(f, "{line}")?;
        }
        write!(f, "{}", self.inner)
    }
}

impl fmt::Debug for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

impl<E: std::error::Error + Send + Sync + 'static> From<E> for Error {
    fn from(inner: E) -> Self {
        Error::new(inner)
    }
}

/// The `anyhow::Result<T>` masquerade — defaults the error type to [`Error`], so `-> Result<T>`
/// means the same thing `anyhow`'s callers already expect.
pub type Result<T, E = Error> = core::result::Result<T, E>;

/// The `anyhow::Context` masquerade — `.context("...")?`/`.with_context(|| ...)?` on any
/// `Result`, prepending a line to the chain [`Error`] renders. The eager form takes the message
/// directly (cheap for a `&'static str`); the lazy form defers formatting until the error path
/// actually executes (matching `anyhow::Context::with_context`'s reason for existing).
pub trait Context<T> {
    /// Attach a context line unconditionally (the message is built even on the `Ok` path — fine
    /// for a cheap literal, wasteful for anything formatted; use [`with_context`](Self::with_context)
    /// for the latter).
    fn context<C: fmt::Display + Send + Sync + 'static>(self, msg: C) -> Result<T>;
    /// Attach a context line, built lazily — only runs `f` on the `Err` path.
    fn with_context<C: fmt::Display + Send + Sync + 'static, F: FnOnce() -> C>(
        self,
        f: F,
    ) -> Result<T>;
}

impl<T, E: std::error::Error + Send + Sync + 'static> Context<T> for core::result::Result<T, E> {
    fn context<C: fmt::Display + Send + Sync + 'static>(self, msg: C) -> Result<T> {
        self.map_err(|e| {
            let mut err = Error::new(e);
            err.context.push(msg.to_string());
            err
        })
    }

    fn with_context<C: fmt::Display + Send + Sync + 'static, F: FnOnce() -> C>(
        self,
        f: F,
    ) -> Result<T> {
        self.map_err(|e| {
            let mut err = Error::new(e);
            err.context.push(f().to_string());
            err
        })
    }
}

/// `.context()`/`.with_context()` on an already-[`Error`]-typed `Result` — chains a further line
/// rather than re-wrapping (so a `?` through several `context()`-annotated call sites builds one
/// chain, not a matryoshka of boxed errors).
impl<T> Context<T> for Result<T> {
    fn context<C: fmt::Display + Send + Sync + 'static>(self, msg: C) -> Result<T> {
        self.map_err(|mut e| {
            e.context.push(msg.to_string());
            e
        })
    }

    fn with_context<C: fmt::Display + Send + Sync + 'static, F: FnOnce() -> C>(
        self,
        f: F,
    ) -> Result<T> {
        self.map_err(|mut e| {
            e.context.push(f().to_string());
            e
        })
    }
}
