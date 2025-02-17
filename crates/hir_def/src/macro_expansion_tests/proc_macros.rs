//! Tests for user-defined procedural macros.
//!
//! Note `//- proc_macros: identity` fixture metas in tests -- we don't use real
//! proc-macros here, as that would be slow. Instead, we use several hard-coded
//! in-memory macros.
use expect_test::expect;

use crate::macro_expansion_tests::check;

#[test]
fn attribute_macro_attr_censoring() {
    cov_mark::check!(attribute_macro_attr_censoring);
    check(
        r#"
//- proc_macros: identity
#[attr1] #[proc_macros::identity] #[attr2]
struct S;
"#,
        expect![[r##"
#[attr1] #[proc_macros::identity] #[attr2]
struct S;

#[attr1]
#[attr2] struct S;"##]],
    );
}

#[test]
fn derive_censoring() {
    cov_mark::check!(derive_censoring);
    check(
        r#"
//- proc_macros: derive_identity
#[attr1]
#[derive(Foo)]
#[derive(proc_macros::derive_identity)]
#[derive(Bar)]
#[attr2]
struct S;
"#,
        expect![[r##"
#[attr1]
#[derive(Foo)]
#[derive(proc_macros::derive_identity)]
#[derive(Bar)]
#[attr2]
struct S;

#[attr1]
#[derive(Bar)]
#[attr2] struct S;"##]],
    );
}
