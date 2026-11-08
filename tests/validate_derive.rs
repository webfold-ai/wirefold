//! `#[derive(Validate)]` — the `validator` masquerade's derive macro, exercised directly (not a
//! hand-simulated codegen shape) against real structs, including the `Vec<T>` `length` case a
//! real caller (`sauthy`'s bootstrap module) found broken: the codegen unconditionally called
//! `.chars().count()`, which doesn't compile on a `Vec` field at all.

use wirefold::validate::{Validate, ValidationError, ValidationErrors};

#[derive(Debug)]
struct Access {
    kind: String,
}

impl Validate for Access {
    fn validate(&self) -> Result<(), ValidationErrors> {
        let mut errors = ValidationErrors::new();
        if self.kind.is_empty() {
            errors.add("kind", ValidationError::new("required"));
        }
        errors.into_result()
    }
}

#[derive(Debug, wirefold::Validate)]
struct ApiKey {
    #[validate(length(min = 2, max = 24))]
    name: String,
    #[validate(length(min = 1))]
    access: Vec<String>,
}

#[test]
fn string_length_still_uses_char_count() {
    let ok = ApiKey {
        name: "svc".into(),
        access: vec!["read".into()],
    };
    assert!(ok.validate().is_ok());

    let too_short = ApiKey {
        name: "a".into(),
        access: vec!["read".into()],
    };
    assert!(
        too_short.validate().is_err(),
        "name below min length must fail"
    );
}

#[test]
fn vec_length_uses_element_count_not_chars_the_real_bug_this_fixes() {
    let empty_access = ApiKey {
        name: "svc".into(),
        access: vec![],
    };
    let err = empty_access.validate().unwrap_err();
    assert!(
        err.to_string().contains("access"),
        "empty Vec<T> below min length must fail: {err}"
    );

    let nonempty_access = ApiKey {
        name: "svc".into(),
        access: vec!["a".into(), "b".into(), "c".into()],
    };
    // A 3-element Vec would fail if the codegen were still (wrongly) treating it as a string and
    // somehow counting bytes/chars of a Vec's Debug output — this must pass on real element count.
    assert!(nonempty_access.validate().is_ok());
}

#[derive(Debug, wirefold::Validate)]
struct Holder {
    #[validate(nested)]
    items: Vec<Access>,
}

#[test]
fn nested_validate_on_vec_field_merges_each_elements_errors() {
    let holder = Holder {
        items: vec![
            Access { kind: "ok".into() },
            Access {
                kind: String::new(),
            },
        ],
    };
    let err = holder.validate().unwrap_err();
    assert!(
        err.to_string().contains("items.kind"),
        "nested Vec<T> field errors are field-prefixed: {err}"
    );
}

#[test]
fn add_param_and_merge_self_compose_the_way_a_hand_written_validate_impl_uses_them() {
    struct Wrapper {
        exp: i64,
        access: Vec<Access>,
    }
    impl Validate for Wrapper {
        fn validate(&self) -> Result<(), ValidationErrors> {
            let mut errors = ValidationErrors::new();
            if self.exp < 1719784800 {
                let mut err = ValidationError::new("range");
                err.add_param(std::borrow::Cow::from("min"), &1719784800i64);
                err.add_param(std::borrow::Cow::from("value"), &self.exp);
                errors.add("exp", err);
            }
            errors.merge_self("access", self.access.validate());
            errors.into_result()
        }
    }

    let good = Wrapper {
        exp: 2000000000,
        access: vec![Access { kind: "ok".into() }],
    };
    assert!(good.validate().is_ok());

    let bad = Wrapper {
        exp: 0,
        access: vec![Access {
            kind: String::new(),
        }],
    };
    let err = bad.validate().unwrap_err();
    assert!(err.to_string().contains("exp"), "range check failed: {err}");
    assert!(
        err.to_string().contains("access.kind"),
        "merge_self applied the field prefix: {err}"
    );
}
