//! Throwaway: compute the ProPhoto-linear working-space matrices using rawler's own helpers, so the
//! develop shader's `PP_TO_SRGB` constant is derived (not hand-typed). Verifies neutral → neutral.

use rawler::imgop::matrix::{multiply, normalize, pseudo_inverse};
use rawler::imgop::xyz::{XYZ_TO_PROFOTORGB_D50, XYZ_TO_SRGB_D65};

fn apply(m: &[[f32; 3]; 3], v: [f32; 3]) -> [f32; 3] {
    [
        m[0][0] * v[0] + m[0][1] * v[1] + m[0][2] * v[2],
        m[1][0] * v[0] + m[1][1] * v[1] + m[1][2] * v[2],
        m[2][0] * v[0] + m[2][1] * v[1] + m[2][2] * v[2],
    ]
}

fn main() {
    // ProPhoto-linear → XYZ (D50), via rawler's XYZ→ProPhoto inverse.
    let pp_to_xyz = pseudo_inverse(XYZ_TO_PROFOTORGB_D50);
    // ProPhoto-linear → sRGB-linear, row-normalized so ProPhoto white maps to sRGB white (neutral
    // preserved) — mirrors rawler's own normalize-then-use convention on the encode side.
    let pp_to_srgb = normalize(multiply(&XYZ_TO_SRGB_D65, &pp_to_xyz));

    println!("PP_TO_XYZ_D50 = {pp_to_xyz:?}");
    println!("PP_TO_SRGB =");
    for row in &pp_to_srgb {
        println!("  [{:.7}, {:.7}, {:.7}],", row[0], row[1], row[2]);
    }
    // Sanity: neutral must stay neutral; a saturated ProPhoto green must land in-range-ish sRGB.
    println!(
        "white  [1,1,1]      -> {:?}",
        apply(&pp_to_srgb, [1.0, 1.0, 1.0])
    );
    println!(
        "gray   [0.2,0.2,0.2]-> {:?}",
        apply(&pp_to_srgb, [0.2, 0.2, 0.2])
    );
    println!(
        "pp red [1,0,0]      -> {:?}",
        apply(&pp_to_srgb, [1.0, 0.0, 0.0])
    );
    println!(
        "pp grn [0,1,0]      -> {:?}",
        apply(&pp_to_srgb, [0.0, 1.0, 0.0])
    );
    println!(
        "pp blu [0,0,1]      -> {:?}",
        apply(&pp_to_srgb, [0.0, 0.0, 1.0])
    );
}
