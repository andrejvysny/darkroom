//! Generate the committed synthetic PQ HEIF test fixture (`tests/fixtures/synthetic_pq.hif`).
//!
//! Writes a 96×64 16-bit PNG with three neutral vertical bands at exact 10-bit PQ code values
//! (1 nit, BT.2408 diffuse white = 203 nits, 10 000 nits), then prints the `heif-enc` command that
//! converts it losslessly to a 10-bit BT.2020 PQ `.hif`:
//!
//! ```sh
//! cargo run -p core-raw --example gen_pq_fixture
//! heif-enc /tmp/synthetic_pq16.png -L -b 10 \
//!   --colour_primaries 9 --transfer_characteristic 16 --matrix_coefficients 9 --full_range_flag 1 \
//!   -o crates/core-raw/tests/fixtures/synthetic_pq.hif
//! ```
//!
//! (`heif-enc` maps 16-bit PNG → 10-bit codes by scaling; PNG values are chosen so the scale
//! round-trips to the exact intended 10-bit code.)

fn pq_oetf(y: f64) -> f64 {
    const M1: f64 = 2610.0 / 16384.0;
    const M2: f64 = 2523.0 / 4096.0 * 128.0;
    const C1: f64 = 3424.0 / 4096.0;
    const C2: f64 = 2413.0 / 4096.0 * 32.0;
    const C3: f64 = 2392.0 / 4096.0 * 32.0;
    let p = y.clamp(0.0, 1.0).powf(M1);
    ((C1 + C2 * p) / (1.0 + C3 * p)).powf(M2)
}

fn main() {
    const W: u32 = 96;
    const H: u32 = 64;
    let codes: Vec<u16> = [1.0 / 10000.0, 203.0 / 10000.0, 1.0]
        .iter()
        .map(|&y| (pq_oetf(y) * 1023.0).round() as u16)
        .collect();
    println!("10-bit PQ band codes: {codes:?}");

    let mut img = image::ImageBuffer::<image::Rgb<u16>, Vec<u16>>::new(W, H);
    for (x, _y, px) in img.enumerate_pixels_mut() {
        let code = codes[(x as usize * 3 / W as usize).min(2)];
        // 16-bit PNG value that scales back to exactly `code` at 10 bits.
        let v16 = ((code as u32 * 65535 + 511) / 1023) as u16;
        *px = image::Rgb([v16, v16, v16]);
    }
    let out = std::env::temp_dir().join("synthetic_pq16.png");
    img.save(&out).expect("write png");
    println!("wrote {}", out.display());
}
