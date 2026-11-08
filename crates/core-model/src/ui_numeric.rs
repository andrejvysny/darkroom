// JavaScript Math.round chooses the integer toward positive infinity at a tie.
pub(crate) fn js_round(value: f64) -> f64 {
    let lower = value.floor();
    let rounded = if value - lower < 0.5 {
        lower
    } else {
        lower + 1.0
    };
    if rounded == 0.0 && value.is_sign_negative() {
        -0.0
    } else {
        rounded
    }
}
