//! The `validator` masquerade — `#[derive(Validate)]` + `#[validate(...)]` field attributes
//! (`email`, `length`, `range`, `regex`, `custom`, `nested`). No dependency: replaces the crate,
//! doesn't wrap it. The derive macro itself lives in `wirefold_derive` (this module supplies
//! the runtime types + trait it generates code against).
//!
//! Scope is the real crate's usage across the codebase this was built for (surveyed, not
//! guessed): `email`/`length(min, max)`/`range(min, max)`/`regex(path = "*STATIC", code = "...")`/
//! `custom(function = "...")`/`nested` are the only attribute shapes in use. Features like
//! `must_match`/`contains`/`credit_card`/`phone`/`url`/`does_not_contain` are real `validator`
//! capabilities this masquerade doesn't implement, because nothing calling it uses them — add
//! them if a caller ever does, don't build ahead of demand.

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::fmt;

/// One field-level validation failure — the `validator::ValidationError` masquerade.
#[derive(Clone, Debug)]
pub struct ValidationError {
    /// A short machine-readable code (`"length"`, `"email"`, or a custom code the caller chose).
    pub code: Cow<'static, str>,
    /// An optional human-readable message overriding the bare code in `Debug`/`Display` output.
    pub message: Option<Cow<'static, str>>,
    /// Named template parameters (`{min}`/`{value}`-style message interpolation in the real
    /// crate) — write-only here (surveyed: nothing calling this reads a param back out), stored
    /// as its `Debug` rendering, which is all any caller needs for logging/error display.
    pub params: BTreeMap<Cow<'static, str>, String>,
}

impl ValidationError {
    /// The `validator::ValidationError::new(code)` constructor.
    #[must_use]
    pub fn new(code: &'static str) -> Self {
        Self {
            code: Cow::Borrowed(code),
            message: None,
            params: BTreeMap::new(),
        }
    }

    /// Attach a human-readable message (`validator::ValidationError::with_message`).
    #[must_use]
    pub fn with_message(mut self, message: Cow<'static, str>) -> Self {
        self.message = Some(message);
        self
    }

    /// Record a named parameter for message templating (`validator::ValidationError::add_param`).
    pub fn add_param<T: fmt::Debug>(&mut self, name: Cow<'static, str>, val: &T) {
        self.params.insert(name, format!("{val:?}"));
    }
}

impl fmt::Display for ValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.message {
            Some(msg) => write!(f, "{msg}"),
            None => write!(f, "{}", self.code),
        }
    }
}

/// The collected validation failures for one value — the `validator::ValidationErrors`
/// masquerade. Keyed by field name; each field can carry more than one failure (matching the
/// real crate's `HashMap<&'static str, Vec<ValidationError>>` shape via a `BTreeMap` instead, for
/// deterministic `Debug` output — nothing here relies on hash order). A field name is borrowed when it
/// is a literal and owned when it was built by [`merge_nested`](Self::merge_nested), so nested names
/// are freed with the error set rather than leaked.
#[derive(Clone, Debug, Default)]
pub struct ValidationErrors {
    errors: BTreeMap<Cow<'static, str>, Vec<ValidationError>>,
}

impl ValidationErrors {
    /// A fresh, empty error set.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Record one field's failure.
    pub fn add(&mut self, field: &'static str, error: ValidationError) {
        self.errors
            .entry(Cow::Borrowed(field))
            .or_default()
            .push(error);
    }

    /// Merge in a nested value's own `ValidationErrors`, prefixing each field name with
    /// `parent_field.` (matching `validator`'s own nested-error field naming).
    pub fn merge_nested(&mut self, parent_field: &'static str, nested: ValidationErrors) {
        for (field, errs) in nested.errors {
            let combined = Cow::Owned(format!("{parent_field}.{field}"));
            self.errors.entry(combined).or_default().extend(errs);
        }
    }

    /// Merge a child's own `validate()` RESULT under `field` (`validator::ValidationErrors::
    /// merge_self`) — the `Result`-taking sibling of `merge_nested` (which takes the unwrapped
    /// `ValidationErrors` directly), for the common `errors.merge_self("field", value.validate())`
    /// call shape. A no-op on `Ok(())`.
    pub fn merge_self(&mut self, field: &'static str, result: Result<(), ValidationErrors>) {
        if let Err(nested) = result {
            self.merge_nested(field, nested);
        }
    }

    /// True if no field recorded a failure.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.errors.is_empty()
    }

    /// Turn a populated error set into `Err`, or `Ok(())` if nothing failed — the shape every
    /// generated `validate()` body reduces to.
    pub fn into_result(self) -> Result<(), Self> {
        if self.is_empty() {
            Ok(())
        } else {
            Err(self)
        }
    }
}

impl fmt::Display for ValidationErrors {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (field, errs) in &self.errors {
            for e in errs {
                writeln!(f, "{field}: {e}")?;
            }
        }
        Ok(())
    }
}

impl std::error::Error for ValidationErrors {}

/// The `validator::Validate` trait masquerade — implemented by `#[derive(Validate)]`.
pub trait Validate {
    /// Run every `#[validate(...)]` check declared on this type's fields, collecting ALL
    /// failures (not just the first) into one `ValidationErrors`, matching the real crate's
    /// multi-error behavior.
    fn validate(&self) -> Result<(), ValidationErrors>;
}

/// The `validator` crate's blanket `impl<T: Validate> Validate for Vec<T>` — a `Vec<T>` field is
/// itself `Validate` (each element checked, failures combined into one `ValidationErrors`), the
/// shape `errors.merge_self("field", self.some_vec_field.validate())` needs.
impl<T: Validate> Validate for Vec<T> {
    fn validate(&self) -> Result<(), ValidationErrors> {
        let mut errors = ValidationErrors::new();
        for item in self {
            if let Err(e) = item.validate() {
                for (field, errs) in e.errors {
                    errors.errors.entry(field).or_default().extend(errs);
                }
            }
        }
        errors.into_result()
    }
}

/// `#[validate(email)]`'s check — the `validator::validate_email` masquerade. A pragmatic
/// format check (one `@`, non-empty local/domain parts, domain has an internal `.`, no
/// whitespace), not full RFC 5321 grammar: the real crate's own check is a permissive regex,
/// not a compliant parser either, and nothing calling this needs stricter than "looks like an
/// email" - actual deliverability is proven by sending mail, not by the field's shape.
#[must_use]
pub fn is_email(value: &str) -> bool {
    if value.is_empty() || value.chars().any(char::is_whitespace) {
        return false;
    }
    let Some((local, domain)) = value.split_once('@') else {
        return false;
    };
    if local.is_empty() || domain.is_empty() || domain.contains('@') {
        return false;
    }
    let Some(dot) = domain.find('.') else {
        return false;
    };
    dot > 0 && dot < domain.len() - 1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_email_accepts_a_plain_address() {
        assert!(is_email("user@example.com"));
    }

    #[test]
    fn is_email_rejects_missing_at_or_dot() {
        assert!(!is_email("no-at-sign.example.com"));
        assert!(!is_email("user@localhost"));
        assert!(!is_email("@example.com"));
        assert!(!is_email("user@"));
        assert!(!is_email("user@.com"));
        assert!(!is_email("user@example."));
        assert!(!is_email("has space@example.com"));
        assert!(!is_email(""));
    }

    #[test]
    fn validation_error_displays_message_over_code() {
        let e = ValidationError::new("too_long").with_message(Cow::Borrowed("must be shorter"));
        assert_eq!(e.to_string(), "must be shorter");
    }

    #[test]
    fn validation_error_displays_code_with_no_message() {
        let e = ValidationError::new("too_long");
        assert_eq!(e.to_string(), "too_long");
    }

    #[test]
    fn empty_errors_convert_to_ok() {
        assert!(ValidationErrors::new().into_result().is_ok());
    }

    #[test]
    fn populated_errors_convert_to_err() {
        let mut errs = ValidationErrors::new();
        errs.add("email", ValidationError::new("email"));
        assert!(errs.into_result().is_err());
    }

    #[test]
    fn merge_nested_prefixes_field_names() {
        let mut inner = ValidationErrors::new();
        inner.add("street", ValidationError::new("length"));

        let mut outer = ValidationErrors::new();
        outer.merge_nested("address", inner);

        let rendered = outer.to_string();
        assert!(rendered.contains("address.street"));
    }

    #[test]
    fn add_param_records_named_params_by_debug_rendering() {
        let mut err = ValidationError::new("range");
        err.add_param(Cow::from("min"), &1719784800i64);
        err.add_param(Cow::from("value"), &"exp");
        assert_eq!(
            err.params.get("min").map(String::as_str),
            Some("1719784800")
        );
        assert_eq!(err.params.get("value").map(String::as_str), Some("\"exp\""));
    }

    #[test]
    fn merge_self_merges_on_err_and_is_a_noop_on_ok() {
        let mut outer = ValidationErrors::new();
        outer.merge_self("access", Ok(()));
        assert!(outer.is_empty(), "Ok(()) merges nothing");

        let mut child = ValidationErrors::new();
        child.add("kind", ValidationError::new("required"));
        outer.merge_self("access", Err(child));
        assert!(
            outer.to_string().contains("access.kind"),
            "Err merges under the given field, same as merge_nested"
        );
    }

    struct Item(bool); // true = valid
    impl Validate for Item {
        fn validate(&self) -> Result<(), ValidationErrors> {
            let mut errors = ValidationErrors::new();
            if !self.0 {
                errors.add("ok", ValidationError::new("must_be_true"));
            }
            errors.into_result()
        }
    }

    #[test]
    fn vec_blanket_impl_passes_when_every_element_is_valid() {
        let items = vec![Item(true), Item(true)];
        assert!(items.validate().is_ok());
    }

    #[test]
    fn vec_blanket_impl_collects_failures_from_invalid_elements() {
        let items = vec![Item(true), Item(false), Item(false)];
        let err = items.validate().unwrap_err();
        // both failing elements' "ok" errors land under the same field — real behavior for the
        // one real caller (`errors.merge_self("access", self.access.validate())`), which only
        // needs "did any element fail", not which index.
        assert_eq!(err.errors.get("ok").map(Vec::len), Some(2));
    }

    #[test]
    fn vec_field_length_check_uses_element_count_not_chars() {
        #[derive(Debug)]
        struct HasVec {
            tags: Vec<i32>,
        }
        // Mirrors what `#[derive(Validate)]` + `#[validate(length(min = 1))]` generates on a
        // `Vec<T>` field — the real bug this fix closes: it used to call `.chars().count()`
        // unconditionally, which doesn't even compile on a `Vec` (no `.chars()` method), found by
        // a real caller's `Vec<ApiKeyAccess>` field in `sauthy`'s bootstrap module.
        fn check_len_ge_1(v: &HasVec) -> Result<(), ValidationErrors> {
            let mut errors = ValidationErrors::new();
            let __v = &v.tags;
            let __len = __v.len() as i64; // the fixed codegen shape for a Vec field
            if __len < 1 {
                errors.add("tags", ValidationError::new("length"));
            }
            errors.into_result()
        }
        assert!(check_len_ge_1(&HasVec { tags: vec![1] }).is_ok());
        assert!(check_len_ge_1(&HasVec { tags: vec![] }).is_err());
    }
}
