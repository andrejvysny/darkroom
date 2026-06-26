//! Hardening tests for the preset importers: malformed / hostile / alternate-encoding inputs must be
//! rejected or parsed safely (never panic, never stack-overflow, never exceed the size cap).

use core_preset::{Registry, MAX_IMPORT_BYTES};
use std::path::Path;

/// Deeply-nested Lua tables must error (depth guard) rather than overflow the stack.
#[test]
fn deeply_nested_lrtemplate_is_rejected_not_overflow() {
    let mut src = String::from("s = ");
    src.push_str(&"{".repeat(500)); // far past MAX_DEPTH
    let reg = Registry::with_defaults();
    let res = reg.import(src.as_bytes(), Some(Path::new("deep.lrtemplate")));
    assert!(res.is_err(), "deep nesting must be rejected");
}

/// Lua long-bracket strings `[[ … ]]` — as a value AND as array elements — must parse without
/// derailing the surrounding table (the real crs settings still map).
#[test]
fn lrtemplate_handles_long_bracket_strings() {
    let src = r#"
s = {
    value = {
        settings = {
            Exposure2012 = 0.75,
            Caption = [[multi
line note]],
            Tags = { [[one]], [[two]] },
        },
    },
}
"#;
    let reg = Registry::with_defaults();
    let parsed = reg
        .import(src.as_bytes(), Some(Path::new("longstr.lrtemplate")))
        .expect("long-bracket strings must not break parsing");
    assert!((parsed.params["exposure"].as_f64().unwrap() - 0.75).abs() < 1e-6);
}

/// An import larger than the cap is rejected before parsing (bounds DOM/parse cost).
#[test]
fn oversized_import_is_rejected() {
    let big = vec![b' '; MAX_IMPORT_BYTES + 1];
    let reg = Registry::with_defaults();
    assert!(reg.import(&big, Some(Path::new("huge.xmp"))).is_err());
}

/// crs settings written as child ELEMENTS (not attributes) must be read.
#[test]
fn xmp_reads_element_form_crs_settings() {
    let xmp = r#"<x:xmpmeta xmlns:x="adobe:ns:meta/">
 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description xmlns:crs="http://ns.adobe.com/camera-raw-settings/1.0/">
   <crs:Exposure2012>0.75</crs:Exposure2012>
   <crs:Contrast2012>20</crs:Contrast2012>
  </rdf:Description>
 </rdf:RDF>
</x:xmpmeta>"#;
    let reg = Registry::with_defaults();
    let parsed = reg
        .import(xmp.as_bytes(), Some(Path::new("element.xmp")))
        .expect("element-form crs must import");
    assert!((parsed.params["exposure"].as_f64().unwrap() - 0.75).abs() < 1e-6);
    assert!((parsed.params["contrast"].as_f64().unwrap() - 20.0).abs() < 1e-6);
}
